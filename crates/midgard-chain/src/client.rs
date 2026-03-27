//! The Tendermint client, and the block iterator the sync loop drives.

use std::time::Duration;

use futures::future::try_join_all;
use midgard_config::ThorChain;
use midgard_core::Nano;
use serde_json::json;

use crate::rpc::{RpcClient, RpcError};
use crate::types::{Block, BlockResponse, BlockResults, Status};

#[derive(Debug, thiserror::Error)]
pub enum ChainError {
    #[error(transparent)]
    Rpc(#[from] RpcError),

    #[error("block {requested}: node answered for height {returned}")]
    HeightMismatch { requested: i64, returned: i64 },

    #[error("block {height}: unparseable timestamp {time:?}: {reason}")]
    BadTimestamp {
        height: i64,
        time: String,
        reason: String,
    },

    #[error("configuration: {0}")]
    Config(String),
}

#[derive(Debug, Clone)]
pub struct Client {
    rpc: RpcClient,
    batch_size: usize,
    parallelism: usize,
    max_status_retries: usize,
    status_retry_backoff: Duration,
}

impl Client {
    pub fn new(cfg: &ThorChain) -> Result<Client, ChainError> {
        if cfg.parallelism == 0 || cfg.fetch_batch_size == 0 {
            return Err(ChainError::Config(
                "fetch_batch_size and parallelism must both be at least 1".to_string(),
            ));
        }
        if cfg.fetch_batch_size % cfg.parallelism != 0 {
            return Err(ChainError::Config(format!(
                "fetch_batch_size ({}) must be divisible by parallelism ({})",
                cfg.fetch_batch_size, cfg.parallelism
            )));
        }

        let (base, _ws) = cfg.split_tendermint_url();
        let rpc = RpcClient::new(&base, cfg.read_timeout.get())?;

        Ok(Client {
            rpc,
            batch_size: cfg.fetch_batch_size,
            parallelism: cfg.parallelism,
            max_status_retries: cfg.max_status_retries,
            status_retry_backoff: cfg.status_retry_backoff.get(),
        })
    }

    pub fn batch_size(&self) -> usize {
        self.batch_size
    }

    pub fn url(&self) -> &str {
        self.rpc.url()
    }

    /// The chain tip, retried a few times before giving up.
    ///
    /// This is the one call that runs before anything else, so a node that is still starting up
    /// should not be a fatal error for the daemon.
    pub async fn status(&self) -> Result<Status, ChainError> {
        let mut last: Option<RpcError> = None;
        for attempt in 0..self.max_status_retries.max(1) {
            match self.rpc.call::<Status>("status", json!({})).await {
                Ok(s) => return Ok(s),
                Err(e) => {
                    tracing::warn!(
                        attempt = attempt + 1,
                        of = self.max_status_retries,
                        error = %e,
                        "status request failed"
                    );
                    last = Some(e);
                    tokio::time::sleep(self.status_retry_backoff).await;
                }
            }
        }
        Err(ChainError::Rpc(
            last.expect("at least one attempt was made"),
        ))
    }

    /// The height of the chain tip.
    pub async fn latest_height(&self) -> Result<i64, ChainError> {
        Ok(self.status().await?.sync_info.latest_block_height.value() as i64)
    }

    /// One block, both halves of it.
    pub async fn fetch_block(&self, height: i64) -> Result<Block, ChainError> {
        let params = json!({"height": height.to_string()});
        // Concurrent rather than sequential: the two calls are independent and this halves the
        // latency of the unbatched path, which is what a caught-up node uses for every block.
        let (block, results) = futures::try_join!(
            self.rpc.call::<BlockResponse>("block", params.clone()),
            self.rpc.call::<BlockResults>("block_results", params),
        )?;
        assemble(height, block, results)
    }

    /// A contiguous range, batched and optionally fanned out.
    ///
    /// Returns exactly `count` blocks starting at `from`, in order.
    pub async fn fetch_range(&self, from: i64, count: usize) -> Result<Vec<Block>, ChainError> {
        if count == 0 {
            return Ok(Vec::new());
        }
        if count == 1 {
            return Ok(vec![self.fetch_block(from).await?]);
        }

        // Only split when each worker would get a whole number of blocks; otherwise one batch.
        let chunks = if self.parallelism > 1 && count % self.parallelism == 0 {
            self.parallelism
        } else {
            1
        };
        let per_chunk = count / chunks;

        let futures = (0..chunks).map(|i| {
            let start = from + (i * per_chunk) as i64;
            self.fetch_batch(start, per_chunk)
        });

        let parts = try_join_all(futures).await?;
        Ok(parts.into_iter().flatten().collect())
    }

    /// One batched round trip per RPC method for a contiguous range.
    async fn fetch_batch(&self, from: i64, count: usize) -> Result<Vec<Block>, ChainError> {
        let params: Vec<_> = (0..count as i64)
            .map(|i| json!({"height": (from + i).to_string()}))
            .collect();

        let (blocks, results) = futures::try_join!(
            self.rpc
                .call_batch::<BlockResponse>("block", params.clone()),
            self.rpc.call_batch::<BlockResults>("block_results", params),
        )?;

        blocks
            .into_iter()
            .zip(results)
            .enumerate()
            .map(|(i, (b, r))| assemble(from + i as i64, b, r))
            .collect()
    }

    /// The hash of block 1, used to tell which chain a database was built from.
    pub async fn first_block_hash(&self) -> Result<String, ChainError> {
        Ok(self.fetch_block(1).await?.hash)
    }

    pub fn iterator(&self, from: i64, through: i64) -> BlockIterator<'_> {
        BlockIterator {
            client: self,
            next: from,
            through,
            buffered: Vec::new(),
        }
    }
}

/// Combine the two responses, checking that they describe the block we asked for.
///
/// The height checks are not paranoia: a batch reassembled by the wrong id, or a load balancer
/// in front of several nodes, would otherwise attribute one block's events to another height and
/// the result would be a database that looks fine and is wrong.
fn assemble(
    requested: i64,
    block: BlockResponse,
    results: BlockResults,
) -> Result<Block, ChainError> {
    let block_height = block.block.header.height.value() as i64;
    if block_height != requested {
        return Err(ChainError::HeightMismatch {
            requested,
            returned: block_height,
        });
    }
    let results_height = results.height.value() as i64;
    if results_height != requested {
        return Err(ChainError::HeightMismatch {
            requested,
            returned: results_height,
        });
    }

    let timestamp = block_time_to_nano(requested, block.block.header.time)?;

    Ok(Block {
        height: requested,
        timestamp,
        // `Hash`'s Display is the uppercase hex the RPC reports and `block_log` stores.
        hash: block.block_id.hash.to_string(),
        chain_id: block.block.header.chain_id.to_string(),
        txs: block.block.data.clone(),
        results,
    })
}

/// `tendermint::Time` to nanoseconds since the epoch.
///
/// Their `Time` carries nanosecond precision internally, so this is a representation change and
/// not a rounding step — worth being sure of, because block timestamps are the primary key of
/// every event table and the bucket boundaries are derived from them.
fn block_time_to_nano(height: i64, time: tendermint::Time) -> Result<Nano, ChainError> {
    let odt: time::OffsetDateTime = time.into();
    i64::try_from(odt.unix_timestamp_nanos())
        .map(Nano)
        .map_err(|_| ChainError::BadTimestamp {
            height,
            time: time.to_string(),
            reason: "out of range for a 64-bit nanosecond count".to_string(),
        })
}

/// Walks a height range, refilling from the network a batch at a time.
pub struct BlockIterator<'a> {
    client: &'a Client,
    next: i64,
    through: i64,
    buffered: Vec<Block>,
}

impl BlockIterator<'_> {
    /// The next block, or `None` once `through` has been passed.
    pub async fn next(&mut self) -> Result<Option<Block>, ChainError> {
        if self.buffered.is_empty() {
            if self.next > self.through {
                return Ok(None);
            }
            let remaining = (self.through - self.next + 1) as usize;
            let count = remaining.min(self.client.batch_size());
            self.buffered = self.client.fetch_range(self.next, count).await?;
            self.next += count as i64;
            // A node that answers with fewer blocks than asked for would otherwise spin here.
            if self.buffered.is_empty() {
                return Ok(None);
            }
        }
        Ok(Some(self.buffered.remove(0)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use midgard_config::ThorChain;

    /// Block timestamps are the primary key of every event table and the basis of every bucket
    /// boundary, so the seconds-to-nanoseconds conversion is worth pinning against a real value
    /// observed on mainnet.
    #[test]
    fn timestamps_keep_nanosecond_precision() {
        let time = tendermint::Time::parse_from_rfc3339("2026-08-02T08:50:12.310239931Z").unwrap();
        let t = block_time_to_nano(1, time).unwrap();
        assert_eq!(t, Nano(1_785_660_612_310_239_931));
        // And the seconds view is a truncation of it, not a rounding.
        assert_eq!(t.to_second().to_i64(), 1_785_660_612);
    }

    #[test]
    fn timestamps_without_fractional_seconds_still_convert() {
        let time = tendermint::Time::parse_from_rfc3339("2026-08-02T08:49:53Z").unwrap();
        assert_eq!(
            block_time_to_nano(1, time).unwrap().to_i64() % 1_000_000_000,
            0
        );
    }

    #[test]
    fn batch_size_must_divide_by_parallelism() {
        let cfg = ThorChain {
            fetch_batch_size: 10,
            parallelism: 3,
            ..ThorChain::default()
        };
        let err = Client::new(&cfg).unwrap_err();
        assert!(err.to_string().contains("divisible"), "{err}");
    }

    #[test]
    fn zero_batch_size_is_rejected() {
        let cfg = ThorChain {
            fetch_batch_size: 0,
            parallelism: 1,
            ..ThorChain::default()
        };
        assert!(Client::new(&cfg).is_err());
    }

    #[test]
    fn defaults_are_accepted() {
        assert!(Client::new(&ThorChain::default()).is_ok());
    }
}
