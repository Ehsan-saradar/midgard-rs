//! `GET /v2/history/tvl`.
//!
//! Total value locked. Pooled value is twice the RUNE depth summed over pools — a pool is
//! rebalanced to equal value on both sides, so the RUNE side is half of it and doubling avoids
//! having to price every asset separately.
//!
//! Bonded value would come from node state, which is not in the event stream, so it is reported
//! as zero here rather than guessed. That makes `totalValueLocked` equal to `totalValuePooled`
//! in this port, and the field is kept so clients do not have to special-case its absence.

use std::collections::HashMap;

use axum::extract::{Query, State};
use axum::Json;
use midgard_core::units::{float_str, int_str};
use midgard_db::buckets;

use crate::error::ApiResult;
use crate::models::{History, TvlHistoryItem, TvlPoolDepth};
use crate::query::Params;
use crate::{usd, AppState};

pub async fn tvl_history(
    State(state): State<AppState>,
    Query(raw): Query<HashMap<String, String>>,
) -> ApiResult<Json<History<TvlHistoryItem>>> {
    let mut params = Params::new(raw);
    let bucket_params = params.buckets()?;
    params.reject_unknown()?;

    let buckets = buckets::generate(bucket_params, &state.cursor)?;
    let rune_usd = usd::rune_price_now(&state.db, &state.config.usd_pools, buckets.end()).await?;

    let mut intervals = Vec::with_capacity(buckets.count());
    for i in 0..buckets.count() {
        let w = buckets.bucket(i);
        // Depth is a level, so each bucket reports the state at its close.
        let depths = pool_depths_at(&state, w.until.to_nano().to_i64()).await?;
        intervals.push(build_item(
            w.from.to_i64(),
            w.until.to_i64(),
            depths,
            rune_usd,
        ));
    }

    let final_depths = pool_depths_at(&state, buckets.end().to_nano().to_i64()).await?;
    let meta = build_item(
        buckets.start().to_i64(),
        buckets.end().to_i64(),
        final_depths,
        rune_usd,
    );

    Ok(Json(History { intervals, meta }))
}

fn build_item(start: i64, end: i64, depths: Vec<(String, i64)>, rune_usd: f64) -> TvlHistoryItem {
    let pooled: i64 = depths.iter().map(|(_, rune_e8)| rune_e8 * 2).sum();

    TvlHistoryItem {
        start_time: start.to_string(),
        end_time: end.to_string(),
        total_value_pooled: int_str(pooled),
        total_value_bonded: int_str(0),
        total_value_locked: int_str(pooled),
        rune_price_usd: float_str(rune_usd),
        pools_depth: depths
            .into_iter()
            .map(|(pool, rune_e8)| TvlPoolDepth {
                pool,
                total_depth: int_str(rune_e8 * 2),
            })
            .collect(),
    }
}

/// Every pool's RUNE depth as of `at` (nanoseconds).
async fn pool_depths_at(state: &AppState, at: i64) -> ApiResult<Vec<(String, i64)>> {
    let rows: Vec<(String, i64)> = sqlx::query_as(
        "SELECT DISTINCT ON (pool) pool, rune_e8
         FROM block_pool_depths
         WHERE block_timestamp <= $1
         ORDER BY pool, block_timestamp DESC",
    )
    .bind(at)
    .fetch_all(state.db.pool())
    .await?;

    // Drained pools contribute nothing and would just be noise in the response.
    Ok(rows
        .into_iter()
        .filter(|(_, rune_e8)| *rune_e8 > 0)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pooled_value_is_twice_the_rune_side() {
        let item = build_item(0, 1, vec![("BTC.BTC".to_string(), 100)], 1.0);
        assert_eq!(item.total_value_pooled, "200");
        assert_eq!(item.pools_depth[0].total_depth, "200");
    }

    #[test]
    fn pools_sum_into_the_total() {
        let item = build_item(
            0,
            1,
            vec![("A".to_string(), 100), ("B".to_string(), 50)],
            1.0,
        );
        assert_eq!(item.total_value_pooled, "300");
        assert_eq!(item.pools_depth.len(), 2);
    }

    #[test]
    fn locked_equals_pooled_while_bonded_is_unavailable() {
        let item = build_item(0, 1, vec![("A".to_string(), 7)], 1.0);
        assert_eq!(item.total_value_bonded, "0");
        assert_eq!(item.total_value_locked, item.total_value_pooled);
    }

    #[test]
    fn no_pools_is_zero_not_an_error() {
        let item = build_item(3, 4, vec![], 0.0);
        assert_eq!(item.total_value_pooled, "0");
        assert!(item.pools_depth.is_empty());
        assert_eq!(item.start_time, "3");
    }
}
