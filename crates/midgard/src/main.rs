//! The Midgard daemon.
//!
//! Two independent jobs sharing one database: a sync loop pulling blocks from THORChain, and an
//! HTTP server answering queries about what it has written. They are deliberately not coupled —
//! the API serves whatever is committed, so a stalled sync degrades freshness rather than
//! availability, and `/v2/health` is what tells you which is happening.

mod bootstrap;
mod sync;

use std::sync::atomic::AtomicI64;
use std::sync::Arc;

use anyhow::{Context, Result};
use midgard_api::AppState;
use midgard_chain::thornode::ThorNode;
use midgard_chain::Client;
use midgard_config::Config;
use midgard_db::block_log::BlockCursor;
use midgard_db::{ddl, Db};
use sync::tokio_util_shim::CancellationToken;
use sync::Syncer;

#[tokio::main]
async fn main() -> Result<()> {
    let config = Arc::new(load_config()?);
    init_logging(&config);

    tracing::info!(
        tendermint = %config.thorchain.tendermint_url,
        thornode = %config.thorchain.thornode_url,
        listen_port = config.listen_port,
        "starting midgard"
    );

    let db = Db::connect(&config.timescale)
        .await
        .context("connecting to the database")?;

    let rebuilt = ddl::ensure_schema(&db, config.timescale.no_auto_update_ddl)
        .await
        .context("preparing the schema")?;
    if rebuilt {
        tracing::warn!("schema was (re)created; syncing from the start of the chain");
    }

    let cursor = BlockCursor::new();
    cursor
        .refresh(&db)
        .await
        .context("reading the block cursor")?;
    tracing::info!(
        first = cursor.first().height,
        last = cursor.last().height,
        "resuming from the database"
    );

    let client = Client::new(&config.thorchain).context("building the Tendermint client")?;
    let thornode = Arc::new(
        ThorNode::new(
            &config.thorchain.thornode_url,
            config.thorchain.read_timeout.get(),
        )
        .context("building the THORNode client")?,
    );

    // Depths are replayed as deltas, so an empty database that starts part-way along the chain
    // would begin every pool at zero and immediately go negative. Seed from THORNode instead.
    let start_height = config.genesis.initial_block_height.max(1);
    if cursor.last().height == 0 && start_height > 1 {
        let first = client
            .fetch_block(start_height)
            .await
            .context("fetching the first block to seed from")?;
        // One nanosecond before the first indexed block, so every later query picks it up as the
        // opening balance and nothing at that height is shadowed by it.
        let at = midgard_core::Nano(first.timestamp.to_i64() - 1);
        bootstrap::seed_pool_depths(&db, &thornode, start_height, at).await?;
    }

    let chain_height = Arc::new(AtomicI64::new(0));
    let state = AppState {
        db: db.clone(),
        cursor: cursor.clone(),
        config: config.clone(),
        thornode,
        chain_height: chain_height.clone(),
        stats_cache: Default::default(),
    };

    let cancel = CancellationToken::new();

    let syncer = Syncer::new(db.clone(), client, cursor, config.clone(), chain_height)
        .await
        .context("starting the sync loop")?;
    let sync_task = tokio::spawn({
        let cancel = cancel.clone();
        async move {
            if let Err(e) = syncer.run(cancel).await {
                tracing::error!(error = %e, "sync loop exited with an error");
            }
        }
    });

    let listener = tokio::net::TcpListener::bind(("0.0.0.0", config.listen_port))
        .await
        .with_context(|| format!("binding port {}", config.listen_port))?;
    tracing::info!(addr = %listener.local_addr()?, "serving the API");

    let shutdown = {
        let cancel = cancel.clone();
        async move {
            wait_for_signal().await;
            tracing::info!("shutdown signal received");
            cancel.cancel();
        }
    };

    axum::serve(listener, midgard_api::router(state))
        .with_graceful_shutdown(shutdown)
        .await
        .context("serving")?;

    // The server has stopped accepting; give the sync loop its chance to flush.
    cancel.cancel();
    let _ = tokio::time::timeout(config.shutdown_timeout.get(), sync_task).await;

    db.close().await;
    tracing::info!("stopped");
    Ok(())
}

/// Config path comes from a single positional argument, matching upstream's invocation.
fn load_config() -> Result<Config> {
    let args: Vec<String> = std::env::args().collect();
    let paths = match args.len() {
        1 => String::new(),
        2 => args[1].clone(),
        _ => anyhow::bail!(
            "usage: {} [config1.json:config2.json:...]",
            args.first().map(String::as_str).unwrap_or("midgard")
        ),
    };

    midgard_config::load(&paths).context("loading configuration")
}

fn init_logging(config: &Config) {
    use tracing_subscriber::EnvFilter;

    // RUST_LOG wins when set, so an operator can turn up one module without a config change.
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&config.logs.level));

    let builder = tracing_subscriber::fmt().with_env_filter(filter);
    if config.logs.console_logger {
        builder.with_ansi(!config.logs.no_color).init();
    } else {
        // Structured output for anything shipping logs to a collector.
        builder.json().init();
    }
}

async fn wait_for_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(error = %e, "cannot listen for SIGTERM, using ctrl-c only");
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };
        // SIGTERM is what a container runtime sends; ctrl-c is what a person sends.
        tokio::select! {
            _ = term.recv() => {}
            _ = tokio::signal::ctrl_c() => {}
        }
    }

    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
