//! `GET /v2/history/earnings`.
//!
//! The network's income statement. Income is liquidity fees plus block rewards; it is split
//! between nodes (bonding) and pools (liquidity), and the per-pool breakdown says where the
//! pool half went.
//!
//! `rewards_event_entries.rune_e8` can be negative: a pool holding more than its target share of
//! system income has RUNE taken out and given to the others. Clamping those to zero would
//! overstate total earnings, so they are summed as-is.

use std::collections::HashMap;

use axum::extract::{Query, State};
use axum::Json;
use midgard_core::units::{float_str, int_str};
use midgard_db::buckets::{self, Buckets};

use crate::error::ApiResult;
use crate::models::{EarningsHistoryItem, EarningsHistoryItemPool, History};
use crate::query::Params;
use crate::{usd, AppState};

pub async fn earnings_history(
    State(state): State<AppState>,
    Query(raw): Query<HashMap<String, String>>,
) -> ApiResult<Json<History<EarningsHistoryItem>>> {
    let mut params = Params::new(raw);
    let bucket_params = params.buckets()?;
    params.reject_unknown()?;

    let buckets = buckets::generate(bucket_params, &state.cursor)?;
    let rune_usd = usd::rune_price_now(&state.db, &state.config.usd_pools, buckets.end()).await?;

    let fees = fees_by_bucket_and_pool(&state, &buckets).await?;
    let rewards = rewards_by_bucket_and_pool(&state, &buckets).await?;
    let bond = bond_rewards_by_bucket(&state, &buckets).await?;

    let mut intervals = Vec::with_capacity(buckets.count());
    let mut meta_fees: HashMap<String, i64> = HashMap::new();
    let mut meta_rewards: HashMap<String, i64> = HashMap::new();
    let mut meta_bond = 0i64;

    for i in 0..buckets.count() {
        let window = buckets.bucket(i);
        let key = window.from.to_i64();

        let bucket_fees = fees.get(&key).cloned().unwrap_or_default();
        let bucket_rewards = rewards.get(&key).cloned().unwrap_or_default();
        let bucket_bond = bond.get(&key).copied().unwrap_or(0);

        for (pool, v) in &bucket_fees {
            *meta_fees.entry(pool.clone()).or_default() += v;
        }
        for (pool, v) in &bucket_rewards {
            *meta_rewards.entry(pool.clone()).or_default() += v;
        }
        meta_bond += bucket_bond;

        intervals.push(build_item(
            window.from.to_i64(),
            window.until.to_i64(),
            &bucket_fees,
            &bucket_rewards,
            bucket_bond,
            rune_usd,
        ));
    }

    let meta = build_item(
        buckets.start().to_i64(),
        buckets.end().to_i64(),
        &meta_fees,
        &meta_rewards,
        meta_bond,
        rune_usd,
    );

    Ok(Json(History { intervals, meta }))
}

fn build_item(
    start: i64,
    end: i64,
    fees: &HashMap<String, i64>,
    rewards: &HashMap<String, i64>,
    bond_e8: i64,
    rune_usd: f64,
) -> EarningsHistoryItem {
    // Every pool that shows up on either side gets an entry, so a pool that earned only rewards
    // and no fees is not silently missing.
    let mut names: Vec<&String> = fees.keys().chain(rewards.keys()).collect();
    names.sort_unstable();
    names.dedup();

    let mut pools = Vec::with_capacity(names.len());
    let mut total_fees = 0i64;
    let mut total_rewards = 0i64;

    for name in names {
        let fee = fees.get(name).copied().unwrap_or(0);
        let reward = rewards.get(name).copied().unwrap_or(0);
        total_fees += fee;
        total_rewards += reward;

        pools.push(EarningsHistoryItemPool {
            pool: name.clone(),
            // Fees are converted to RUNE when the swap is recorded, so the asset-denominated
            // split is not recoverable here and the RUNE figure carries the whole amount.
            asset_liquidity_fees: int_str(0),
            rune_liquidity_fees: int_str(fee),
            total_liquidity_fees_rune: int_str(fee),
            rewards: int_str(reward),
            earnings: int_str(fee + reward),
            saver_earning: int_str(0),
        });
    }

    let liquidity_earnings = total_fees + total_rewards;

    EarningsHistoryItem {
        start_time: start.to_string(),
        end_time: end.to_string(),
        liquidity_fees: int_str(total_fees),
        block_rewards: int_str(bond_e8 + total_rewards),
        earnings: int_str(total_fees + bond_e8 + total_rewards),
        bonding_earnings: int_str(bond_e8),
        liquidity_earnings: int_str(liquidity_earnings),
        // Node counts come from THORNode state rather than the event stream, so there is no
        // historical series to average over. Reported as zero rather than guessed.
        avg_node_count: float_str(0.0),
        rune_price_usd: float_str(rune_usd),
        pools,
    }
}

type ByBucketAndPool = HashMap<i64, HashMap<String, i64>>;

async fn fees_by_bucket_and_pool(
    state: &AppState,
    buckets: &Buckets,
) -> ApiResult<ByBucketAndPool> {
    let truncated = buckets.truncated_timestamp("block_timestamp");
    let sql = format!(
        "SELECT {truncated} AS bucket, pool, COALESCE(SUM(liq_fee_in_rune_e8), 0)::BIGINT
         FROM swap_events
         WHERE block_timestamp >= $1 AND block_timestamp < $2
         GROUP BY bucket, pool"
    );
    collect(state, &sql, buckets).await
}

async fn rewards_by_bucket_and_pool(
    state: &AppState,
    buckets: &Buckets,
) -> ApiResult<ByBucketAndPool> {
    let truncated = buckets.truncated_timestamp("block_timestamp");
    let sql = format!(
        "SELECT {truncated} AS bucket, pool, COALESCE(SUM(rune_e8), 0)::BIGINT
         FROM rewards_event_entries
         WHERE block_timestamp >= $1 AND block_timestamp < $2
         GROUP BY bucket, pool"
    );
    collect(state, &sql, buckets).await
}

async fn collect(state: &AppState, sql: &str, buckets: &Buckets) -> ApiResult<ByBucketAndPool> {
    let rows: Vec<(i64, String, i64)> = sqlx::query_as(sql)
        .bind(buckets.start().to_nano().to_i64())
        .bind(buckets.end().to_nano().to_i64())
        .fetch_all(state.db.pool())
        .await?;

    let mut out: ByBucketAndPool = HashMap::new();
    for (bucket, pool, amount) in rows {
        *out.entry(bucket).or_default().entry(pool).or_default() += amount;
    }
    Ok(out)
}

async fn bond_rewards_by_bucket(
    state: &AppState,
    buckets: &Buckets,
) -> ApiResult<HashMap<i64, i64>> {
    let truncated = buckets.truncated_timestamp("block_timestamp");
    let sql = format!(
        "SELECT {truncated} AS bucket, COALESCE(SUM(bond_e8), 0)::BIGINT
         FROM rewards_events
         WHERE block_timestamp >= $1 AND block_timestamp < $2
         GROUP BY bucket"
    );

    let rows: Vec<(i64, i64)> = sqlx::query_as(&sql)
        .bind(buckets.start().to_nano().to_i64())
        .bind(buckets.end().to_nano().to_i64())
        .fetch_all(state.db.pool())
        .await?;
    Ok(rows.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, i64)]) -> HashMap<String, i64> {
        pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    }

    #[test]
    fn earnings_are_fees_plus_all_rewards() {
        let item = build_item(
            0,
            1,
            &map(&[("BTC.BTC", 100)]),
            &map(&[("BTC.BTC", 50)]),
            700,
            1.0,
        );
        assert_eq!(item.liquidity_fees, "100");
        assert_eq!(item.bonding_earnings, "700");
        assert_eq!(item.liquidity_earnings, "150");
        assert_eq!(item.block_rewards, "750"); // bond + pool share
        assert_eq!(item.earnings, "850"); // fees + block rewards
    }

    #[test]
    fn negative_pool_rewards_reduce_the_total() {
        // A pool above its target share has RUNE taken out. Clamping to zero would overstate
        // network earnings.
        let item = build_item(0, 1, &map(&[]), &map(&[("A", 100), ("B", -40)]), 0, 1.0);
        assert_eq!(item.liquidity_earnings, "60");
        let b = item.pools.iter().find(|p| p.pool == "B").unwrap();
        assert_eq!(b.rewards, "-40");
    }

    #[test]
    fn a_pool_with_only_rewards_still_appears() {
        let item = build_item(0, 1, &map(&[("A", 10)]), &map(&[("B", 20)]), 0, 1.0);
        let names: Vec<&str> = item.pools.iter().map(|p| p.pool.as_str()).collect();
        assert_eq!(names, vec!["A", "B"]);
    }

    #[test]
    fn pools_are_sorted_and_deduplicated() {
        let item = build_item(0, 1, &map(&[("Z", 1), ("A", 1)]), &map(&[("A", 1)]), 0, 1.0);
        let names: Vec<&str> = item.pools.iter().map(|p| p.pool.as_str()).collect();
        assert_eq!(names, vec!["A", "Z"]);
    }

    #[test]
    fn an_empty_bucket_is_all_zeros() {
        let item = build_item(5, 10, &map(&[]), &map(&[]), 0, 0.0);
        assert_eq!(item.start_time, "5");
        assert_eq!(item.earnings, "0");
        assert!(item.pools.is_empty());
    }

    #[test]
    fn per_pool_earnings_are_its_fees_plus_its_rewards() {
        let item = build_item(0, 1, &map(&[("A", 30)]), &map(&[("A", 12)]), 0, 1.0);
        let a = &item.pools[0];
        assert_eq!(a.rune_liquidity_fees, "30");
        assert_eq!(a.rewards, "12");
        assert_eq!(a.earnings, "42");
    }
}
