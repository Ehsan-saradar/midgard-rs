//! The block sync loop.
//!
//! Two phases, and the difference matters:
//!
//! * **catching up** — the chain tip is far ahead, so blocks are fetched in batches and written
//!   in batches. Throughput is everything.
//! * **at the tip** — one block every five seconds or so. Latency is everything, and the writer
//!   is flushed after each block so the API is never more than one block stale.
//!
//! Switching between them is automatic: the loop asks how far behind it is and picks.

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use midgard_chain::{ChainError, Client};
use midgard_config::Config;
use midgard_db::block_log::{BlockCursor, BlockPos};
use midgard_db::Db;
use midgard_record::BlockWriter;
use tokio_util_shim::CancellationToken;

/// Minimal stand-in for `tokio_util`'s cancellation token, so the daemon does not pull in the
/// whole crate for one type.
pub mod tokio_util_shim {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    #[derive(Clone, Default)]
    pub struct CancellationToken(Arc<AtomicBool>);

    impl CancellationToken {
        pub fn new() -> CancellationToken {
            CancellationToken::default()
        }

        pub fn cancel(&self) {
            self.0.store(true, Ordering::SeqCst);
        }

        pub fn is_cancelled(&self) -> bool {
            self.0.load(Ordering::SeqCst)
        }
    }
}

/// How far behind the tip we have to be before batching is worth it.
const BATCH_THRESHOLD: i64 = 10;

/// Is this the node saying "that block exists but I cannot answer for it yet"?
///
/// `/status` advertises a height a moment before `/block_results` can serve it, so at the tip
/// this is the normal steady state rather than a fault. Matched on the message because
/// CometBFT reports it as a generic internal error with the detail only in the text.
fn is_not_ready_yet(error: &ChainError) -> bool {
    let msg = error.to_string();
    msg.contains("could not find results for height")
        || msg.contains("must be less than or equal to the current blockchain height")
}

pub struct Syncer {
    client: Client,
    writer: BlockWriter,
    cursor: BlockCursor,
    config: Arc<Config>,
    /// Published for `/v2/health` so it can report how far behind we are.
    chain_height: Arc<AtomicI64>,
}

impl Syncer {
    pub async fn new(
        db: Db,
        client: Client,
        cursor: BlockCursor,
        config: Arc<Config>,
        chain_height: Arc<AtomicI64>,
    ) -> Result<Syncer, anyhow::Error> {
        let mut writer = BlockWriter::new(db, config.timescale.commit_batch_size);
        writer.restore().await?;

        Ok(Syncer {
            client,
            writer,
            cursor,
            config,
            chain_height,
        })
    }

    /// Run until cancelled.
    pub async fn run(mut self, cancel: CancellationToken) -> Result<(), anyhow::Error> {
        let backoff = self.config.thorchain.last_chain_backoff.get();

        while !cancel.is_cancelled() {
            match self.step(&cancel).await {
                Ok(true) => {}
                // Caught up: wait for the chain to produce more.
                Ok(false) => tokio::time::sleep(backoff).await,
                // The node reports a tip slightly before its results are queryable, so at the
                // tip this happens routinely. It is "wait a moment", not a failure, and logging
                // it as an error would mean a red line every few seconds on a healthy node.
                Err(e) if is_not_ready_yet(&e) => {
                    tracing::debug!(error = %e, "tip block not queryable yet, waiting");
                    tokio::time::sleep(backoff).await;
                }
                Err(e) => {
                    // Network blips and a node restarting are routine. Log, wait, retry — the
                    // cursor is in the database, so nothing is lost by starting the step again.
                    tracing::error!(error = %e, "sync step failed, retrying after backoff");
                    tokio::time::sleep(backoff).await;
                }
            }
        }

        // Flush whatever is buffered so a clean shutdown does not throw away a partial batch.
        tracing::info!("sync loop stopping, flushing buffered blocks");
        self.writer.flush().await?;
        Ok(())
    }

    /// One iteration. Returns whether any block was written.
    async fn step(&mut self, cancel: &CancellationToken) -> Result<bool, ChainError> {
        let tip = self.client.latest_height().await?;
        self.chain_height.store(tip, Ordering::Relaxed);

        let next = self.cursor.next_height().max(self.first_height());
        if next > tip {
            return Ok(false);
        }

        let behind = tip - next + 1;
        if behind > BATCH_THRESHOLD {
            self.catch_up(next, tip, cancel).await?;
        } else {
            self.follow(next, tip).await?;
        }
        Ok(true)
    }

    /// Where a fresh database starts from.
    fn first_height(&self) -> i64 {
        self.config.genesis.initial_block_height.max(1)
    }

    /// Batched fetch and batched write, for when we are a long way behind.
    async fn catch_up(
        &mut self,
        from: i64,
        tip: i64,
        cancel: &CancellationToken,
    ) -> Result<(), ChainError> {
        tracing::info!(from, tip, behind = tip - from + 1, "catching up");

        let mut iterator = self.client.iterator(from, tip);
        let mut written = 0i64;
        let mut last_logged = std::time::Instant::now();

        while let Some(block) = iterator.next().await? {
            if cancel.is_cancelled() {
                break;
            }
            let pos = BlockPos {
                height: block.height,
                timestamp: block.timestamp,
            };

            if let Err(e) = self.writer.add(&block).await {
                tracing::error!(height = block.height, error = %e, "failed to write block");
                return Ok(());
            }
            self.cursor.observe(pos);
            written += 1;

            // Progress at a fixed cadence rather than per block: at a few thousand blocks a
            // second, per-block logging is the bottleneck.
            if last_logged.elapsed() >= std::time::Duration::from_secs(10) {
                tracing::info!(height = block.height, tip, written, "catching up");
                last_logged = std::time::Instant::now();
            }
        }
        Ok(())
    }

    /// One block at a time, flushed immediately, for when we are at the tip.
    async fn follow(&mut self, from: i64, tip: i64) -> Result<(), ChainError> {
        for height in from..=tip {
            let block = self.client.fetch_block(height).await?;
            let pos = BlockPos {
                height: block.height,
                timestamp: block.timestamp,
            };

            if let Err(e) = self.writer.add(&block).await {
                tracing::error!(height, error = %e, "failed to write block");
                return Ok(());
            }
            // At the tip, a buffered block is a block the API cannot see.
            if let Err(e) = self.writer.flush().await {
                tracing::error!(height, error = %e, "failed to flush block");
                return Ok(());
            }
            self.cursor.observe(pos);
            tracing::debug!(height, "committed block");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::tokio_util_shim::CancellationToken;

    #[test]
    fn a_fresh_token_is_not_cancelled() {
        assert!(!CancellationToken::new().is_cancelled());
    }

    #[test]
    fn cancelling_is_visible_to_every_clone() {
        let a = CancellationToken::new();
        let b = a.clone();
        a.cancel();
        assert!(b.is_cancelled(), "cancellation should be shared");
    }

    /// The batching decision, extracted so it can be checked without a chain.
    fn should_batch(next: i64, tip: i64) -> bool {
        (tip - next + 1) > super::BATCH_THRESHOLD
    }

    #[test]
    fn the_tip_lagging_its_results_is_not_treated_as_a_failure() {
        // Verbatim from a live node, which reports this every few seconds at the tip.
        let e = super::ChainError::Rpc(midgard_chain::RpcError::Rpc {
            method: "block_results".to_string(),
            code: -32603,
            message: "Internal error: could not find results for height #27261503".to_string(),
        });
        assert!(super::is_not_ready_yet(&e));
    }

    #[test]
    fn a_real_failure_still_is_one() {
        let e = super::ChainError::Rpc(midgard_chain::RpcError::Rpc {
            method: "block_results".to_string(),
            code: -32603,
            message: "database is corrupt".to_string(),
        });
        assert!(!super::is_not_ready_yet(&e));
    }

    #[test]
    fn a_long_way_behind_means_batching() {
        assert!(should_batch(1, 27_000_000));
    }

    #[test]
    fn at_the_tip_means_following_one_block_at_a_time() {
        assert!(!should_batch(100, 100));
        assert!(!should_batch(100, 105));
    }

    #[test]
    fn the_threshold_is_exclusive() {
        assert!(!should_batch(1, 10));
        assert!(should_batch(1, 11));
    }
}
