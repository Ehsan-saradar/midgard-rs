//! The block cursor.
//!
//! `block_log` is the only table the sync loop reads back, and it answers three questions: where
//! do we resume from, what is the oldest data we have, and what height corresponds to a given
//! wall-clock time.
//!
//! The last two are cached in memory because they are on the path of nearly every API request
//! (bucket generation clamps to the chain's first and last block) and they change either never
//! or once every five seconds.

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use midgard_core::{Nano, Second};
use sqlx::Row;

use crate::{Db, DbError};

/// A block's position in time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BlockPos {
    pub height: i64,
    pub timestamp: Nano,
}

impl BlockPos {
    pub fn is_empty(&self) -> bool {
        self.height == 0
    }
}

/// Cached first and last block, shared between the sync loop and the API handlers.
///
/// Two atomics per block rather than a mutex around a struct: readers are frequent and the two
/// fields are read independently by different callers, so a torn read of "height from before the
/// last write, timestamp from after" is at worst five seconds stale and never wrong in a way
/// that matters. A lock here would sit on the hot path of every request for no benefit.
#[derive(Debug, Default)]
struct Cached {
    height: AtomicI64,
    timestamp: AtomicI64,
}

impl Cached {
    fn load(&self) -> BlockPos {
        BlockPos {
            height: self.height.load(Ordering::Relaxed),
            timestamp: Nano(self.timestamp.load(Ordering::Relaxed)),
        }
    }

    fn store(&self, pos: BlockPos) {
        self.height.store(pos.height, Ordering::Relaxed);
        self.timestamp
            .store(pos.timestamp.to_i64(), Ordering::Relaxed);
    }
}

#[derive(Debug, Clone, Default)]
pub struct BlockCursor {
    first: Arc<Cached>,
    last: Arc<Cached>,
}

impl BlockCursor {
    pub fn new() -> BlockCursor {
        BlockCursor::default()
    }

    /// Populate from the database. Called at startup and after every commit.
    pub async fn refresh(&self, db: &Db) -> Result<(), DbError> {
        if let Some(pos) = first_block(db).await? {
            self.first.store(pos);
        }
        if let Some(pos) = last_block(db).await? {
            self.last.store(pos);
        }
        Ok(())
    }

    pub fn first(&self) -> BlockPos {
        self.first.load()
    }

    pub fn last(&self) -> BlockPos {
        self.last.load()
    }

    /// Record a block we just committed, without a round trip.
    pub fn observe(&self, pos: BlockPos) {
        if self.first.load().is_empty() {
            self.first.store(pos);
        }
        self.last.store(pos);
    }

    /// "Now" as far as the data is concerned: one second past the last block.
    ///
    /// Queries are bounded by the data we have, not by the wall clock — using the wall clock
    /// would produce empty trailing buckets whenever sync falls behind, and clients would read
    /// that as the chain having gone quiet.
    pub fn now_second(&self) -> Second {
        self.last.load().timestamp.to_second() + 1
    }

    /// The height the sync loop should ask for next.
    pub fn next_height(&self) -> i64 {
        self.last.load().height + 1
    }
}

pub async fn last_block(db: &Db) -> Result<Option<BlockPos>, DbError> {
    let row = sqlx::query("SELECT height, timestamp FROM block_log ORDER BY height DESC LIMIT 1")
        .fetch_optional(db.pool())
        .await?;
    Ok(row.map(|r| BlockPos {
        height: r.get::<i64, _>("height"),
        timestamp: Nano(r.get::<i64, _>("timestamp")),
    }))
}

pub async fn first_block(db: &Db) -> Result<Option<BlockPos>, DbError> {
    let row = sqlx::query("SELECT height, timestamp FROM block_log ORDER BY height ASC LIMIT 1")
        .fetch_optional(db.pool())
        .await?;
    Ok(row.map(|r| BlockPos {
        height: r.get::<i64, _>("height"),
        timestamp: Nano(r.get::<i64, _>("timestamp")),
    }))
}

/// The hash stored for a height, used to detect that we are following a different chain than the
/// one the database was built from.
pub async fn hash_at(db: &Db, height: i64) -> Result<Option<Vec<u8>>, DbError> {
    Ok(
        sqlx::query_scalar("SELECT hash FROM block_log WHERE height = $1")
            .bind(height)
            .fetch_optional(db.pool())
            .await?,
    )
}

/// Drop every trace of blocks at or above `height`.
///
/// Used when the chain we are following has diverged from what we recorded. Every event table
/// carries `block_timestamp`, so the cut is by timestamp rather than by height, and the
/// `block_log` row is deleted last so that a crash midway leaves the cursor pointing at data
/// that is still there rather than at data that is not.
pub async fn delete_from_height(db: &Db, height: i64) -> Result<(), DbError> {
    let timestamp: Option<i64> =
        sqlx::query_scalar("SELECT timestamp FROM block_log WHERE height = $1")
            .bind(height)
            .fetch_optional(db.pool())
            .await?;
    let Some(timestamp) = timestamp else {
        return Ok(());
    };

    let mut tx = db.pool().begin().await?;
    for table in crate::tables::rollback_tables() {
        let sql = format!("DELETE FROM {table} WHERE block_timestamp >= $1");
        sqlx::query(&sql).bind(timestamp).execute(&mut *tx).await?;
    }
    sqlx::query("DELETE FROM block_log WHERE height >= $1")
        .bind(height)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;

    tracing::warn!(height, "deleted blocks at and above height");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_starts_empty() {
        let c = BlockCursor::new();
        assert!(c.first().is_empty());
        assert_eq!(c.next_height(), 1);
    }

    #[test]
    fn first_observation_sets_both_ends() {
        let c = BlockCursor::new();
        let pos = BlockPos {
            height: 10,
            timestamp: Nano(5_000_000_000),
        };
        c.observe(pos);
        assert_eq!(c.first(), pos);
        assert_eq!(c.last(), pos);
        assert_eq!(c.next_height(), 11);
    }

    #[test]
    fn later_observations_only_move_the_far_end() {
        let c = BlockCursor::new();
        let first = BlockPos {
            height: 10,
            timestamp: Nano(5_000_000_000),
        };
        let later = BlockPos {
            height: 11,
            timestamp: Nano(10_000_000_000),
        };
        c.observe(first);
        c.observe(later);
        assert_eq!(c.first(), first);
        assert_eq!(c.last(), later);
    }

    #[test]
    fn now_is_one_second_past_the_last_block() {
        let c = BlockCursor::new();
        c.observe(BlockPos {
            height: 1,
            timestamp: Nano(1_700_000_000_500_000_000),
        });
        assert_eq!(c.now_second(), Second(1_700_000_001));
    }
}
