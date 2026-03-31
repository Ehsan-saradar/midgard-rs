//! End-to-end: fetch real blocks, decode them, write them, read them back.
//!
//! Needs both a node and a database:
//!
//! ```sh
//! MIDGARD_TEST_TENDERMINT=http://localhost:27147 \
//! MIDGARD_TEST_DB=postgres://midgard:password@localhost:5433/midgard \
//!   cargo test -p midgard-record --test pipeline
//! ```
//!
//! Unit tests cover decoding against fixtures, which only proves we handle the events we thought
//! to write down. This proves the pipeline survives whatever mainnet actually emits — every
//! event type in a real block range, with real attribute spellings.

use midgard_chain::Client;
use midgard_config::{ThorChain, TimeScale};
use midgard_db::{block_log, ddl, Db};
use midgard_record::BlockWriter;

fn db_config() -> Option<TimeScale> {
    let dsn = std::env::var("MIDGARD_TEST_DB").ok()?;
    let rest = dsn.strip_prefix("postgres://")?;
    let (creds, hostpath) = rest.split_once('@')?;
    let (user, password) = creds.split_once(':')?;
    let (hostport, database) = hostpath.split_once('/')?;
    let (host, port) = hostport.split_once(':')?;
    Some(TimeScale {
        host: host.to_string(),
        port: port.parse().ok()?,
        user_name: user.to_string(),
        password: password.to_string(),
        database: database.split('?').next()?.to_string(),
        sslmode: "disable".to_string(),
        max_open_conns: 4,
        ..TimeScale::default()
    })
}

fn chain_config() -> Option<ThorChain> {
    Some(ThorChain {
        tendermint_url: std::env::var("MIDGARD_TEST_TENDERMINT").ok()?,
        fetch_batch_size: 10,
        parallelism: 1,
        ..ThorChain::default()
    })
}

/// Key for the advisory lock below. Shared with `midgard-db`'s schema tests, which rebuild the
/// same schema.
const SCHEMA_LOCK_KEY: i64 = 0x_4D49_4447; // "MIDG"

/// Take an exclusive, cross-process lock on the schema.
///
/// These tests drop and rebuild the shared `midgard` schema, and so do `midgard-db`'s — which
/// `cargo test` runs as a separate process at the same time. An in-process mutex cannot see that;
/// a postgres advisory lock can.
///
/// Held by a dedicated connection, not a pooled one: advisory locks are session-scoped, and a
/// pooled connection going back to the pool keeps its session (and the lock) alive.
async fn lock_schema(cfg: &TimeScale) -> sqlx::PgConnection {
    use sqlx::Connection;
    let mut conn = sqlx::PgConnection::connect(&cfg.connection_string())
        .await
        .expect("lock connection");
    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(SCHEMA_LOCK_KEY)
        .execute(&mut conn)
        .await
        .expect("advisory lock");
    conn
}

macro_rules! setup_or_skip {
    () => {
        match (db_config(), chain_config()) {
            (Some(dbc), Some(chainc)) => {
                let guard = lock_schema(&dbc).await;
                let db = Db::connect(&dbc).await.expect("connect");
                sqlx::query("DROP SCHEMA IF EXISTS midgard CASCADE")
                    .execute(db.pool())
                    .await
                    .expect("drop");
                ddl::ensure_schema(&db, false).await.expect("schema");
                let client = Client::new(&chainc).expect("client");
                (db, client, guard)
            }
            _ => {
                eprintln!("MIDGARD_TEST_DB / MIDGARD_TEST_TENDERMINT not set, skipping");
                return;
            }
        }
    };
}

/// Index `count` blocks ending near the chain tip.
async fn index_recent(db: &Db, client: &Client, count: usize) -> i64 {
    let tip = client.latest_height().await.expect("tip");
    let from = tip - count as i64 - 10;

    let mut writer = BlockWriter::new(db.clone(), 25);
    writer.restore().await.expect("restore");

    let mut iter = client.iterator(from, from + count as i64 - 1);
    while let Some(block) = iter.next().await.expect("fetch") {
        writer.add(&block).await.expect("add");
    }
    writer.flush().await.expect("flush");
    from
}

#[tokio::test]
async fn indexes_real_blocks_end_to_end() {
    let (db, client, _guard) = setup_or_skip!();
    const COUNT: usize = 30;

    let from = index_recent(&db, &client, COUNT).await;

    let blocks: i64 = sqlx::query_scalar("SELECT count(*) FROM block_log")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(blocks, COUNT as i64, "every block should be logged");

    // Heights are contiguous, with no gap where a block failed to decode.
    let (min, max): (i64, i64) = sqlx::query_as("SELECT min(height), max(height) FROM block_log")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(min, from);
    assert_eq!(max, from + COUNT as i64 - 1);

    let cursor = block_log::BlockCursor::new();
    cursor.refresh(&db).await.unwrap();
    assert_eq!(cursor.last().height, max);
    assert!(cursor.last().timestamp.to_i64() > 0);
}

#[tokio::test]
async fn real_blocks_produce_real_rows() {
    let (db, client, _guard) = setup_or_skip!();
    index_recent(&db, &client, 60).await;

    // Sixty consecutive mainnet blocks always contain rewards and pool depth movement. If these
    // come back empty, decoding silently stopped matching what the chain emits — which is the
    // failure this whole file exists to catch.
    let rewards: i64 = sqlx::query_scalar("SELECT count(*) FROM rewards_events")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert!(rewards > 0, "no rewards events decoded from 60 real blocks");

    let depths: i64 = sqlx::query_scalar("SELECT count(*) FROM block_pool_depths")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert!(depths > 0, "no pool depths recorded from 60 real blocks");

    // Transfers come from the Cosmos bank module in a different coin dialect to THORChain's own
    // events. This assertion is here because they were, in fact, all being silently dropped:
    // every mainnet block carries transfers, so an empty table means the decoder is wrong.
    let transfers: i64 = sqlx::query_scalar("SELECT count(*) FROM transfer_events")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert!(
        transfers > 0,
        "no transfer events decoded from 60 real blocks"
    );

    // And they must carry a resolved asset name, not a raw denom, and not the whole coin list
    // glued into one string — a multi-denomination transfer parsed as a single coin produces an
    // asset literally named "BTC-BTC,1736937ETH-ETH", which inserts perfectly happily.
    let bad_assets: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM transfer_events
         WHERE asset = '' OR asset = lower(asset) OR asset LIKE '%,%'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(
        bad_assets, 0,
        "transfer assets should be single upper-cased names"
    );
}

#[tokio::test]
async fn event_ids_are_unique_and_ordered_within_each_table() {
    let (db, client, _guard) = setup_or_skip!();
    index_recent(&db, &client, 40).await;

    for table in midgard_db::tables::EVENT_TABLES {
        // Two tables legitimately repeat an event id, because one ABCI event genuinely describes
        // several rows: the per-pool reward split, and a bank transfer moving more than one
        // denomination at once.
        if matches!(*table, "rewards_event_entries" | "transfer_events") {
            continue;
        }
        let sql = format!("SELECT count(*) - count(DISTINCT event_id) FROM {table}");
        let dupes: i64 = sqlx::query_scalar(&sql).fetch_one(db.pool()).await.unwrap();
        assert_eq!(dupes, 0, "{table} has duplicate event ids");
    }
}

#[tokio::test]
async fn event_ids_sort_in_the_same_order_as_block_time() {
    let (db, client, _guard) = setup_or_skip!();
    index_recent(&db, &client, 40).await;

    // The entire point of the event id encoding. If it holds on real data, the actions feed's
    // ORDER BY event_id is chain order.
    let violations: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM (
             SELECT event_id, block_timestamp,
                    lag(event_id) OVER (ORDER BY block_timestamp, event_id) AS prev_id,
                    lag(block_timestamp) OVER (ORDER BY block_timestamp, event_id) AS prev_ts
             FROM swap_events
         ) t
         WHERE prev_ts IS NOT NULL AND prev_ts < block_timestamp AND prev_id > event_id",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(violations, 0, "event ids disagree with block time ordering");
}

#[tokio::test]
async fn every_recorded_event_belongs_to_a_logged_block() {
    let (db, client, _guard) = setup_or_skip!();
    index_recent(&db, &client, 30).await;

    // The crash-safety invariant: rows and their block_log entry commit together, so an event
    // whose block is missing would mean the transaction boundary is wrong.
    for table in midgard_db::tables::rollback_tables() {
        let sql = format!(
            "SELECT count(*) FROM {table} e
             WHERE NOT EXISTS (SELECT 1 FROM block_log b WHERE b.timestamp = e.block_timestamp)"
        );
        let orphans: i64 = sqlx::query_scalar(&sql).fetch_one(db.pool()).await.unwrap();
        assert_eq!(orphans, 0, "{table} has rows with no matching block");
    }
}

#[tokio::test]
async fn indexing_is_deterministic() {
    let (db, client, _guard) = setup_or_skip!();

    let tip = client.latest_height().await.unwrap();
    let from = tip - 40;
    let through = from + 19;

    async fn index_range(db: &Db, client: &Client, from: i64, through: i64) -> Vec<(String, i64)> {
        sqlx::query("DROP SCHEMA IF EXISTS midgard CASCADE")
            .execute(db.pool())
            .await
            .unwrap();
        ddl::ensure_schema(db, false).await.unwrap();

        let mut writer = BlockWriter::new(db.clone(), 7);
        writer.restore().await.unwrap();
        let mut iter = client.iterator(from, through);
        while let Some(block) = iter.next().await.unwrap() {
            writer.add(&block).await.unwrap();
        }
        writer.flush().await.unwrap();

        sqlx::query_as("SELECT pool, rune_e8 FROM block_pool_depths ORDER BY block_timestamp, pool")
            .fetch_all(db.pool())
            .await
            .unwrap()
    }

    let first = index_range(&db, &client, from, through).await;
    let second = index_range(&db, &client, from, through).await;

    // Re-syncing has to be a valid recovery, which requires decoding to be a pure function of
    // the block. If these differ, some hidden state is leaking between runs.
    assert!(!first.is_empty(), "expected some depth rows");
    assert_eq!(
        first, second,
        "replaying the same range produced different depths"
    );
}

#[tokio::test]
async fn a_partial_batch_leaves_no_trace() {
    let (db, client, _guard) = setup_or_skip!();

    let tip = client.latest_height().await.unwrap();
    let from = tip - 30;

    // Buffer several blocks but never flush, then drop the writer.
    {
        let mut writer = BlockWriter::new(db.clone(), 1_000);
        writer.restore().await.unwrap();
        let mut iter = client.iterator(from, from + 4);
        while let Some(block) = iter.next().await.unwrap() {
            writer.add(&block).await.unwrap();
        }
        assert_eq!(
            writer.pending_blocks(),
            5,
            "nothing should have flushed yet"
        );
    }

    let blocks: i64 = sqlx::query_scalar("SELECT count(*) FROM block_log")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(blocks, 0, "an unflushed batch must not be visible");
}
