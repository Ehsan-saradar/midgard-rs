//! `GET /v2/health`.
//!
//! What operators page on. `inSync` is the one that matters: it means the newest block we have
//! written is recent enough that answers are current, which is a different question from
//! "is the process alive".

use std::sync::atomic::Ordering;

use axum::extract::State;
use axum::Json;

use crate::error::ApiResult;
use crate::models::{Health, HeightTs};
use crate::AppState;

pub async fn health(State(state): State<AppState>) -> ApiResult<Json<Health>> {
    let database = state.db.ping().await;
    let last = state.cursor.last();

    let committed = HeightTs {
        height: last.height,
        timestamp: last.timestamp.to_second().to_i64(),
    };

    let chain_height = state.chain_height.load(Ordering::Relaxed);

    // Age is measured against the wall clock, not against the chain tip we last saw: a node that
    // has itself stopped advancing would otherwise keep us reporting "in sync" indefinitely.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default();
    let age = now - committed.timestamp;
    let in_sync = last.height > 0 && age < state.config.max_block_age.get().as_secs() as i64;

    Ok(Json(Health {
        database,
        in_sync,
        scanner_height: committed.height.to_string(),
        last_committed: HeightTs { ..committed },
        // No block store in this port, so fetched and committed are the same point.
        last_fetched: HeightTs {
            height: last.height,
            timestamp: committed.timestamp,
        },
        last_thornode: HeightTs {
            height: chain_height,
            timestamp: 0,
        },
        // Aggregates are computed on read rather than materialised, so there is no separate
        // aggregation cursor to fall behind the committed one.
        last_aggregated: HeightTs {
            height: last.height,
            timestamp: committed.timestamp,
        },
    }))
}

#[cfg(test)]
mod tests {
    /// The sync check itself, extracted so it can be tested without a database.
    fn in_sync(height: i64, block_time: i64, now: i64, max_age: i64) -> bool {
        height > 0 && (now - block_time) < max_age
    }

    #[test]
    fn a_fresh_block_is_in_sync() {
        assert!(in_sync(100, 1_000, 1_010, 60));
    }

    #[test]
    fn a_stale_block_is_not() {
        assert!(!in_sync(100, 1_000, 1_100, 60));
    }

    #[test]
    fn an_empty_database_is_never_in_sync() {
        // Height 0 with a timestamp of 0 against a large "now" is stale anyway, but an empty
        // database must not read as healthy even if the clock says otherwise.
        assert!(!in_sync(0, 0, 0, 60));
    }

    #[test]
    fn the_boundary_is_exclusive() {
        assert!(!in_sync(1, 0, 60, 60));
        assert!(in_sync(1, 0, 59, 60));
    }
}
