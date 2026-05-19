//! `GET /v2/history/liquidity_changes`.
//!
//! Deposits and withdrawals, bucketed. Both sides are reported in RUNE, and the asset side is
//! valued at the price *when it happened* — the value the recorder stored in
//! `_asset_in_rune_e8`, not today's price. Revaluing history every time a pool moves would make
//! yesterday's chart change overnight.

use std::collections::HashMap;

use axum::extract::{Query, State};
use axum::Json;
use midgard_core::units::{float_str, int_str};
use midgard_db::buckets::{self, Buckets};

use crate::error::ApiResult;
use crate::models::{History, LiquidityHistoryItem};
use crate::query::Params;
use crate::{usd, AppState};

#[derive(Debug, Default, Clone, Copy)]
struct Totals {
    add_count: i64,
    add_rune: i64,
    add_asset: i64,
    withdraw_count: i64,
    withdraw_rune: i64,
    withdraw_asset: i64,
}

impl Totals {
    fn add(&mut self, o: &Totals) {
        self.add_count += o.add_count;
        self.add_rune += o.add_rune;
        self.add_asset += o.add_asset;
        self.withdraw_count += o.withdraw_count;
        self.withdraw_rune += o.withdraw_rune;
        self.withdraw_asset += o.withdraw_asset;
    }

    fn add_volume(&self) -> i64 {
        self.add_rune + self.add_asset
    }

    fn withdraw_volume(&self) -> i64 {
        self.withdraw_rune + self.withdraw_asset
    }

    /// Net flow. Positive means more went in than came out.
    ///
    /// The field is documented upstream as "withdrawals - deposits" but is computed the other
    /// way round; deposits minus withdrawals is what the numbers actually show, and matching the
    /// behaviour matters more than matching the sentence.
    fn net(&self) -> i64 {
        self.add_volume() - self.withdraw_volume()
    }
}

pub async fn liquidity_history(
    State(state): State<AppState>,
    Query(raw): Query<HashMap<String, String>>,
) -> ApiResult<Json<History<LiquidityHistoryItem>>> {
    let mut params = Params::new(raw);
    let bucket_params = params.buckets()?;
    let pool = params.take_string("pool");
    params.reject_unknown()?;

    let buckets = buckets::generate(bucket_params, &state.cursor)?;
    let by_bucket = query_totals(&state, &buckets, pool.as_deref()).await?;
    let rune_usd = usd::rune_price_now(&state.db, &state.config.usd_pools, buckets.end()).await?;

    let mut meta = Totals::default();
    let mut intervals = Vec::with_capacity(buckets.count());

    for i in 0..buckets.count() {
        let w = buckets.bucket(i);
        let t = by_bucket.get(&w.from.to_i64()).copied().unwrap_or_default();
        meta.add(&t);
        intervals.push(build_item(w.from.to_i64(), w.until.to_i64(), &t, rune_usd));
    }

    Ok(Json(History {
        intervals,
        meta: build_item(
            buckets.start().to_i64(),
            buckets.end().to_i64(),
            &meta,
            rune_usd,
        ),
    }))
}

fn build_item(start: i64, end: i64, t: &Totals, rune_usd: f64) -> LiquidityHistoryItem {
    LiquidityHistoryItem {
        start_time: start.to_string(),
        end_time: end.to_string(),
        add_liquidity_count: int_str(t.add_count),
        add_liquidity_volume: int_str(t.add_volume()),
        add_asset_liquidity_volume: int_str(t.add_asset),
        add_rune_liquidity_volume: int_str(t.add_rune),
        withdraw_count: int_str(t.withdraw_count),
        withdraw_volume: int_str(t.withdraw_volume()),
        withdraw_asset_volume: int_str(t.withdraw_asset),
        withdraw_rune_volume: int_str(t.withdraw_rune),
        net: int_str(t.net()),
        rune_price_usd: float_str(rune_usd),
    }
}

async fn query_totals(
    state: &AppState,
    buckets: &Buckets,
    pool: Option<&str>,
) -> ApiResult<HashMap<i64, Totals>> {
    let truncated = buckets.truncated_timestamp("block_timestamp");
    let from = buckets.start().to_nano().to_i64();
    let until = buckets.end().to_nano().to_i64();

    // Deposits and withdrawals live in different tables, so they are unioned into one pass
    // rather than queried separately and joined in Rust.
    let sql = format!(
        "SELECT bucket,
                COALESCE(SUM(add_count), 0)::BIGINT, COALESCE(SUM(add_rune), 0)::BIGINT, COALESCE(SUM(add_asset), 0)::BIGINT,
                COALESCE(SUM(wd_count), 0)::BIGINT, COALESCE(SUM(wd_rune), 0)::BIGINT, COALESCE(SUM(wd_asset), 0)::BIGINT
         FROM (
             SELECT {truncated} AS bucket,
                    1 AS add_count, rune_e8 AS add_rune, _asset_in_rune_e8 AS add_asset,
                    0 AS wd_count, 0 AS wd_rune, 0 AS wd_asset
             FROM stake_events
             WHERE block_timestamp >= $1 AND block_timestamp < $2
               AND ($3::text IS NULL OR pool = $3)
             UNION ALL
             SELECT {truncated} AS bucket,
                    0, 0, 0,
                    1, emit_rune_e8, _emit_asset_in_rune_e8
             FROM withdraw_events
             WHERE block_timestamp >= $1 AND block_timestamp < $2
               AND ($3::text IS NULL OR pool = $3)
         ) moves
         GROUP BY bucket"
    );

    let rows: Vec<(i64, i64, i64, i64, i64, i64, i64)> = sqlx::query_as(&sql)
        .bind(from)
        .bind(until)
        .bind(pool)
        .fetch_all(state.db.pool())
        .await?;

    Ok(rows
        .into_iter()
        .map(|r| {
            (
                r.0,
                Totals {
                    add_count: r.1,
                    add_rune: r.2,
                    add_asset: r.3,
                    withdraw_count: r.4,
                    withdraw_rune: r.5,
                    withdraw_asset: r.6,
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
            add_count: 3,
            add_rune: 100,
            add_asset: 50,
            withdraw_count: 1,
            withdraw_rune: 20,
            withdraw_asset: 10,
        }
    }

    #[test]
    fn volumes_are_both_sides_summed() {
        let t = totals();
        assert_eq!(t.add_volume(), 150);
        assert_eq!(t.withdraw_volume(), 30);
    }

    #[test]
    fn net_is_deposits_minus_withdrawals() {
        assert_eq!(totals().net(), 120);
    }

    #[test]
    fn net_goes_negative_when_more_leaves_than_arrives() {
        let t = Totals {
            add_rune: 10,
            withdraw_rune: 100,
            ..Totals::default()
        };
        assert_eq!(t.net(), -90);
    }

    #[test]
    fn an_empty_bucket_is_all_zeros() {
        let item = build_item(1, 2, &Totals::default(), 0.0);
        assert_eq!(item.add_liquidity_count, "0");
        assert_eq!(item.net, "0");
    }

    #[test]
    fn accumulating_sums_every_field() {
        let mut a = Totals::default();
        a.add(&totals());
        a.add(&totals());
        assert_eq!(a.add_count, 6);
        assert_eq!(a.net(), 240);
    }
}
