//! `GET /v2/history/swaps`.
//!
//! Volume, fees and slip bucketed over time, split by direction. The split is why
//! `swap_events._direction` exists as a column: classifying on read would mean parsing asset
//! strings for every row of the scan.
//!
//! Volume is always the RUNE side of the swap. Every swap has RUNE on exactly one side — an
//! asset-to-asset trade is routed as two swaps and arrives as two events — so the RUNE leg is
//! the one figure comparable across every pool.

use std::collections::HashMap;

use axum::extract::{Query, State};
use axum::Json;
use midgard_core::units::{float_str, int_str, ratio};
use midgard_db::buckets::{self, Buckets};
use sqlx::Row;

use crate::error::ApiResult;
use crate::models::{History, SwapHistoryItem};
use crate::query::Params;
use crate::{usd, AppState};

/// One bucket's totals, straight out of the aggregate query.
#[derive(Debug, Default, Clone)]
struct Totals {
    to_asset_count: i64,
    to_rune_count: i64,
    synth_mint_count: i64,
    synth_redeem_count: i64,

    to_asset_volume: i64,
    to_rune_volume: i64,
    synth_mint_volume: i64,
    synth_redeem_volume: i64,

    to_asset_fees: i64,
    to_rune_fees: i64,
    synth_mint_fees: i64,
    synth_redeem_fees: i64,

    to_asset_slip: i64,
    to_rune_slip: i64,
    synth_mint_slip: i64,
    synth_redeem_slip: i64,
}

impl Totals {
    fn add(&mut self, other: &Totals) {
        self.to_asset_count += other.to_asset_count;
        self.to_rune_count += other.to_rune_count;
        self.synth_mint_count += other.synth_mint_count;
        self.synth_redeem_count += other.synth_redeem_count;
        self.to_asset_volume += other.to_asset_volume;
        self.to_rune_volume += other.to_rune_volume;
        self.synth_mint_volume += other.synth_mint_volume;
        self.synth_redeem_volume += other.synth_redeem_volume;
        self.to_asset_fees += other.to_asset_fees;
        self.to_rune_fees += other.to_rune_fees;
        self.synth_mint_fees += other.synth_mint_fees;
        self.synth_redeem_fees += other.synth_redeem_fees;
        self.to_asset_slip += other.to_asset_slip;
        self.to_rune_slip += other.to_rune_slip;
        self.synth_mint_slip += other.synth_mint_slip;
        self.synth_redeem_slip += other.synth_redeem_slip;
    }

    fn total_count(&self) -> i64 {
        self.to_asset_count + self.to_rune_count + self.synth_mint_count + self.synth_redeem_count
    }

    fn total_volume(&self) -> i64 {
        self.to_asset_volume
            + self.to_rune_volume
            + self.synth_mint_volume
            + self.synth_redeem_volume
    }

    fn total_fees(&self) -> i64 {
        self.to_asset_fees + self.to_rune_fees + self.synth_mint_fees + self.synth_redeem_fees
    }

    /// Slip is a per-swap basis-point figure, so the bucket's average weights each swap equally
    /// regardless of size — a big swap does not count more than a small one.
    fn average_slip(&self) -> f64 {
        let total_slip =
            self.to_asset_slip + self.to_rune_slip + self.synth_mint_slip + self.synth_redeem_slip;
        ratio(total_slip as f64, self.total_count() as f64)
    }
}

pub async fn swap_history(
    State(state): State<AppState>,
    Query(raw): Query<HashMap<String, String>>,
) -> ApiResult<Json<History<SwapHistoryItem>>> {
    let mut params = Params::new(raw);
    let bucket_params = params.buckets()?;
    let pool = params.take_string("pool");
    params.reject_unknown()?;

    let buckets = buckets::generate(bucket_params, &state.cursor)?;
    let by_bucket = query_totals(&state, &buckets, pool.as_deref()).await?;
    let rune_usd = usd::rune_price_now(&state.db, &state.config.usd_pools, buckets.end()).await?;

    let mut meta_totals = Totals::default();
    let mut intervals = Vec::with_capacity(buckets.count());

    for i in 0..buckets.count() {
        let window = buckets.bucket(i);
        let totals = by_bucket
            .get(&window.from.to_i64())
            .cloned()
            .unwrap_or_default();
        meta_totals.add(&totals);
        intervals.push(build_item(
            window.from.to_i64(),
            window.until.to_i64(),
            &totals,
            rune_usd,
        ));
    }

    // `meta` is the same shape as an interval covering the whole range, so it is the sum of the
    // buckets rather than a second query.
    let meta = build_item(
        buckets.start().to_i64(),
        buckets.end().to_i64(),
        &meta_totals,
        rune_usd,
    );

    Ok(Json(History { intervals, meta }))
}

fn build_item(start: i64, end: i64, t: &Totals, rune_usd: f64) -> SwapHistoryItem {
    SwapHistoryItem {
        start_time: start.to_string(),
        end_time: end.to_string(),

        to_asset_count: int_str(t.to_asset_count),
        to_rune_count: int_str(t.to_rune_count),
        synth_mint_count: int_str(t.synth_mint_count),
        synth_redeem_count: int_str(t.synth_redeem_count),
        total_count: int_str(t.total_count()),

        to_asset_volume: int_str(t.to_asset_volume),
        to_rune_volume: int_str(t.to_rune_volume),
        synth_mint_volume: int_str(t.synth_mint_volume),
        synth_redeem_volume: int_str(t.synth_redeem_volume),
        total_volume: int_str(t.total_volume()),

        to_asset_fees: int_str(t.to_asset_fees),
        to_rune_fees: int_str(t.to_rune_fees),
        synth_mint_fees: int_str(t.synth_mint_fees),
        synth_redeem_fees: int_str(t.synth_redeem_fees),
        total_fees: int_str(t.total_fees()),

        to_asset_average_slip: float_str(ratio(t.to_asset_slip as f64, t.to_asset_count as f64)),
        to_rune_average_slip: float_str(ratio(t.to_rune_slip as f64, t.to_rune_count as f64)),
        synth_mint_average_slip: float_str(ratio(
            t.synth_mint_slip as f64,
            t.synth_mint_count as f64,
        )),
        synth_redeem_average_slip: float_str(ratio(
            t.synth_redeem_slip as f64,
            t.synth_redeem_count as f64,
        )),
        average_slip: float_str(t.average_slip()),

        rune_price_usd: float_str(rune_usd),
    }
}

/// Aggregate swaps into buckets, keyed by bucket start.
///
/// One `GROUP BY` over the whole range rather than a query per bucket: 400 buckets would be 400
/// round trips, and the grouping expression is exactly the bucket boundary function so the rows
/// land where the caller expects.
async fn query_totals(
    state: &AppState,
    buckets: &Buckets,
    pool: Option<&str>,
) -> ApiResult<HashMap<i64, Totals>> {
    let truncated = buckets.truncated_timestamp("block_timestamp");

    // Direction codes: 0 rune->asset, 1 asset->rune, 2 rune->synth, 3 synth->rune.
    // Trade and secured directions (4..7) are folded into the asset ones, which is how the
    // upstream response groups them.
    let sql = format!(
        "SELECT {truncated} AS bucket,
            COALESCE(SUM(CASE WHEN _direction IN (0,4,6) THEN 1 ELSE 0 END), 0)::BIGINT AS to_asset_count,
            COALESCE(SUM(CASE WHEN _direction IN (1,5,7) THEN 1 ELSE 0 END), 0)::BIGINT AS to_rune_count,
            COALESCE(SUM(CASE WHEN _direction = 2 THEN 1 ELSE 0 END), 0)::BIGINT AS synth_mint_count,
            COALESCE(SUM(CASE WHEN _direction = 3 THEN 1 ELSE 0 END), 0)::BIGINT AS synth_redeem_count,
            COALESCE(SUM(CASE WHEN _direction IN (0,4,6) THEN from_e8 ELSE 0 END), 0)::BIGINT AS to_asset_volume,
            COALESCE(SUM(CASE WHEN _direction IN (1,5,7) THEN to_e8 ELSE 0 END), 0)::BIGINT AS to_rune_volume,
            COALESCE(SUM(CASE WHEN _direction = 2 THEN from_e8 ELSE 0 END), 0)::BIGINT AS synth_mint_volume,
            COALESCE(SUM(CASE WHEN _direction = 3 THEN to_e8 ELSE 0 END), 0)::BIGINT AS synth_redeem_volume,
            COALESCE(SUM(CASE WHEN _direction IN (0,4,6) THEN liq_fee_in_rune_e8 ELSE 0 END), 0)::BIGINT AS to_asset_fees,
            COALESCE(SUM(CASE WHEN _direction IN (1,5,7) THEN liq_fee_in_rune_e8 ELSE 0 END), 0)::BIGINT AS to_rune_fees,
            COALESCE(SUM(CASE WHEN _direction = 2 THEN liq_fee_in_rune_e8 ELSE 0 END), 0)::BIGINT AS synth_mint_fees,
            COALESCE(SUM(CASE WHEN _direction = 3 THEN liq_fee_in_rune_e8 ELSE 0 END), 0)::BIGINT AS synth_redeem_fees,
            COALESCE(SUM(CASE WHEN _direction IN (0,4,6) THEN swap_slip_bp ELSE 0 END), 0)::BIGINT AS to_asset_slip,
            COALESCE(SUM(CASE WHEN _direction IN (1,5,7) THEN swap_slip_bp ELSE 0 END), 0)::BIGINT AS to_rune_slip,
            COALESCE(SUM(CASE WHEN _direction = 2 THEN swap_slip_bp ELSE 0 END), 0)::BIGINT AS synth_mint_slip,
            COALESCE(SUM(CASE WHEN _direction = 3 THEN swap_slip_bp ELSE 0 END), 0)::BIGINT AS synth_redeem_slip
         FROM swap_events
         WHERE block_timestamp >= $1 AND block_timestamp < $2
           AND ($3::text IS NULL OR pool = $3)
         GROUP BY bucket"
    );

    // Read by index rather than into a tuple: sqlx only implements FromRow for tuples up to 16
    // elements and this aggregate has 17 columns.
    let rows = sqlx::query(&sql)
        .bind(buckets.start().to_nano().to_i64())
        .bind(buckets.end().to_nano().to_i64())
        .bind(pool)
        .fetch_all(state.db.pool())
        .await?;

    Ok(rows
        .into_iter()
        .map(|r| {
            let n = |i: usize| r.get::<i64, _>(i);
            (
                n(0),
                Totals {
                    to_asset_count: n(1),
                    to_rune_count: n(2),
                    synth_mint_count: n(3),
                    synth_redeem_count: n(4),
                    to_asset_volume: n(5),
                    to_rune_volume: n(6),
                    synth_mint_volume: n(7),
                    synth_redeem_volume: n(8),
                    to_asset_fees: n(9),
                    to_rune_fees: n(10),
                    synth_mint_fees: n(11),
                    synth_redeem_fees: n(12),
                    to_asset_slip: n(13),
                    to_rune_slip: n(14),
                    synth_mint_slip: n(15),
                    synth_redeem_slip: n(16),
                },
            )
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn totals() -> Totals {
        Totals {
            to_asset_count: 2,
            to_rune_count: 3,
            synth_mint_count: 1,
            synth_redeem_count: 4,
            to_asset_volume: 100,
            to_rune_volume: 200,
            synth_mint_volume: 300,
            synth_redeem_volume: 400,
            to_asset_fees: 1,
            to_rune_fees: 2,
            synth_mint_fees: 3,
            synth_redeem_fees: 4,
            to_asset_slip: 20,
            to_rune_slip: 30,
            synth_mint_slip: 10,
            synth_redeem_slip: 40,
        }
    }

    #[test]
    fn totals_are_the_sum_of_the_directions() {
        let t = totals();
        assert_eq!(t.total_count(), 10);
        assert_eq!(t.total_volume(), 1_000);
        assert_eq!(t.total_fees(), 10);
    }

    #[test]
    fn average_slip_weights_every_swap_equally() {
        // (20+30+10+40) / 10 swaps = 10, regardless of how large those swaps were.
        assert_eq!(totals().average_slip(), 10.0);
    }

    #[test]
    fn an_empty_bucket_reports_zeros_not_nan() {
        let t = Totals::default();
        assert_eq!(t.average_slip(), 0.0);
        let item = build_item(1, 2, &t, 0.0);
        assert_eq!(item.total_count, "0");
        assert_eq!(item.average_slip, "0");
        assert_eq!(item.to_asset_average_slip, "0");
    }

    #[test]
    fn adding_accumulates_every_field() {
        let mut a = Totals::default();
        a.add(&totals());
        a.add(&totals());
        assert_eq!(a.total_count(), 20);
        assert_eq!(a.total_volume(), 2_000);
        // The average is unchanged, since both counts and slips doubled.
        assert_eq!(a.average_slip(), 10.0);
    }

    #[test]
    fn per_direction_averages_divide_by_their_own_count() {
        let item = build_item(0, 1, &totals(), 1.0);
        assert_eq!(item.to_asset_average_slip, "10"); // 20 / 2
        assert_eq!(item.to_rune_average_slip, "10"); // 30 / 3
        assert_eq!(item.synth_mint_average_slip, "10"); // 10 / 1
        assert_eq!(item.synth_redeem_average_slip, "10"); // 40 / 4
    }
}
