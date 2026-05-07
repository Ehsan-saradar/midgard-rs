//! `GET /v2/history/depths/{pool}`.
//!
//! Depth is a *level*, not a flow, which makes this the odd one out among the history endpoints.
//! The others sum events inside a bucket; this one reports the pool's state at the moment the
//! bucket closed. `block_pool_depths` is sparse, so a bucket with no activity has no row of its
//! own and must inherit the last value before it — the query does that with a lateral lookback
//! rather than gap-filling, which would invent rows that never existed.

use std::collections::HashMap;

use axum::extract::{Path, Query, State};
use axum::Json;
use midgard_core::units::{asset_price, float_str, int_str, luvi, ratio};
use midgard_core::Second;
use midgard_db::buckets;

use crate::error::ApiResult;
use crate::models::{DepthHistory, DepthHistoryItem, DepthHistoryMeta};
use crate::query::Params;
use crate::{usd, AppState};

/// A pool's state at one bucket boundary.
#[derive(Debug, Clone, Default)]
struct Snapshot {
    asset_e8: i64,
    rune_e8: i64,
    synth_e8: i64,
    units: i64,
    members: i64,
}

pub async fn depth_history(
    State(state): State<AppState>,
    Path(pool): Path<String>,
    Query(raw): Query<HashMap<String, String>>,
) -> ApiResult<Json<DepthHistory>> {
    let mut params = Params::new(raw);
    let bucket_params = params.buckets()?;
    params.reject_unknown()?;

    let buckets = buckets::generate(bucket_params, &state.cursor)?;
    let rune_usd = usd::rune_price_now(&state.db, &state.config.usd_pools, buckets.end()).await?;

    // One snapshot per boundary, including the opening one, so `meta` can report both ends.
    let mut snapshots = Vec::with_capacity(buckets.timestamps().len());
    for ts in buckets.timestamps() {
        snapshots.push(snapshot_at(&state, &pool, *ts).await?);
    }

    let mut intervals = Vec::with_capacity(buckets.count());
    for i in 0..buckets.count() {
        let window = buckets.bucket(i);
        // The value reported for a bucket is its state at close, which is the snapshot at the
        // *next* boundary.
        let s = &snapshots[i + 1];
        intervals.push(item(window.from, window.until, s, rune_usd));
    }

    let first = snapshots.first().cloned().unwrap_or_default();
    let last = snapshots.last().cloned().unwrap_or_default();

    let start_luvi = luvi(first.asset_e8, first.rune_e8, first.units);
    let end_luvi = luvi(last.asset_e8, last.rune_e8, last.units);

    Ok(Json(DepthHistory {
        intervals,
        meta: DepthHistoryMeta {
            start_time: buckets.start().to_string(),
            end_time: buckets.end().to_string(),
            start_asset_depth: int_str(first.asset_e8),
            start_rune_depth: int_str(first.rune_e8),
            start_lp_units: int_str(first.units),
            start_synth_units: int_str(first.synth_e8),
            start_member_count: int_str(first.members),
            end_asset_depth: int_str(last.asset_e8),
            end_rune_depth: int_str(last.rune_e8),
            end_lp_units: int_str(last.units),
            end_synth_units: int_str(last.synth_e8),
            end_member_count: int_str(last.members),
            luvi_increase: float_str(ratio(end_luvi, start_luvi)),
            price_shift_loss: float_str(price_shift_loss(&first, &last)),
        },
    }))
}

fn item(from: Second, until: Second, s: &Snapshot, rune_usd: f64) -> DepthHistoryItem {
    let price = asset_price(s.asset_e8, s.rune_e8);
    DepthHistoryItem {
        start_time: from.to_string(),
        end_time: until.to_string(),
        asset_depth: int_str(s.asset_e8),
        rune_depth: int_str(s.rune_e8),
        asset_price: float_str(price),
        asset_price_usd: float_str(price * rune_usd),
        liquidity_units: int_str(s.units),
        synth_units: int_str(0),
        synth_supply: int_str(s.synth_e8),
        units: int_str(s.units),
        members_count: int_str(s.members),
        luvi: float_str(luvi(s.asset_e8, s.rune_e8, s.units)),
    }
}

/// The pool's state as of `at`: the newest depth row at or before that instant.
async fn snapshot_at(state: &AppState, pool: &str, at: Second) -> ApiResult<Snapshot> {
    let ts = at.to_nano().to_i64();

    let row: Option<(i64, i64, i64, i64)> = sqlx::query_as(
        "SELECT asset_e8, rune_e8, synth_e8, units
         FROM block_pool_depths
         WHERE pool = $1 AND block_timestamp <= $2
         ORDER BY block_timestamp DESC
         LIMIT 1",
    )
    .bind(pool)
    .bind(ts)
    .fetch_optional(state.db.pool())
    .await?;

    let (asset_e8, rune_e8, synth_e8, units) = row.unwrap_or((0, 0, 0, 0));

    // Members are addresses with a positive net unit balance at this point in time. Counted
    // from the events rather than kept as a running total, because a member is defined by the
    // sum of their deposits and withdrawals and there is no single row holding it.
    let members: Option<i64> = sqlx::query_scalar(
        "SELECT count(*) FROM (
             SELECT COALESCE(rune_addr, asset_addr) AS member, SUM(units) AS net FROM (
                 SELECT rune_addr, asset_addr, stake_units AS units
                 FROM stake_events WHERE pool = $1 AND block_timestamp <= $2
                 UNION ALL
                 SELECT from_addr AS rune_addr, NULL AS asset_addr, -stake_units AS units
                 FROM withdraw_events WHERE pool = $1 AND block_timestamp <= $2
             ) moves
             GROUP BY member
             HAVING SUM(units) > 0
         ) counted",
    )
    .bind(pool)
    .bind(ts)
    .fetch_one(state.db.pool())
    .await?;

    Ok(Snapshot {
        asset_e8,
        rune_e8,
        synth_e8,
        units,
        members: members.unwrap_or(0),
    })
}

/// Impermanent loss between two snapshots, as a fraction.
///
/// A pool rebalances to equal value on both sides, so a holder ends up with less of whatever
/// went up. Comparing the pool's value against simply having held the two assets gives the loss.
fn price_shift_loss(first: &Snapshot, last: &Snapshot) -> f64 {
    if first.asset_e8 <= 0 || first.rune_e8 <= 0 || last.asset_e8 <= 0 || last.rune_e8 <= 0 {
        return 0.0;
    }
    let p0 = first.rune_e8 as f64 / first.asset_e8 as f64;
    let p1 = last.rune_e8 as f64 / last.asset_e8 as f64;
    if p0 <= 0.0 {
        return 0.0;
    }
    let ratio = p1 / p0;
    // Standard constant-product result: 2*sqrt(r)/(1+r) - 1, negative for any price move.
    2.0 * ratio.sqrt() / (1.0 + ratio) - 1.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(asset: i64, rune: i64) -> Snapshot {
        Snapshot {
            asset_e8: asset,
            rune_e8: rune,
            ..Snapshot::default()
        }
    }

    #[test]
    fn no_price_move_means_no_loss() {
        assert_eq!(price_shift_loss(&snap(100, 100), &snap(100, 100)), 0.0);
    }

    #[test]
    fn any_price_move_loses_something() {
        // Doubling and halving are symmetric and both lose the same amount.
        let up = price_shift_loss(&snap(100, 100), &snap(100, 200));
        let down = price_shift_loss(&snap(100, 100), &snap(100, 50));
        assert!(up < 0.0, "up move should show a loss, got {up}");
        assert!((up - down).abs() < 1e-12, "{up} vs {down}");
        // 4x price move is the textbook ~5.7% loss.
        let four_x = price_shift_loss(&snap(100, 100), &snap(100, 400));
        assert!((four_x + 0.2).abs() < 1e-9, "expected -0.2, got {four_x}");
    }

    #[test]
    fn empty_pools_report_no_loss_rather_than_nan() {
        for (a, b) in [(snap(0, 0), snap(1, 1)), (snap(1, 1), snap(0, 0))] {
            let l = price_shift_loss(&a, &b);
            assert!(l.is_finite(), "not finite: {l}");
            assert_eq!(l, 0.0);
        }
    }

    #[test]
    fn an_interval_reports_the_state_at_close() {
        let s = Snapshot {
            asset_e8: 10,
            rune_e8: 40,
            synth_e8: 2,
            units: 5,
            members: 3,
        };
        let i = item(Second(100), Second(200), &s, 2.0);
        assert_eq!(i.start_time, "100");
        assert_eq!(i.end_time, "200");
        assert_eq!(i.asset_depth, "10");
        assert_eq!(i.asset_price, "4");
        assert_eq!(i.asset_price_usd, "8");
        assert_eq!(i.members_count, "3");
    }
}
