//! `GET /v2/stats`.
//!
//! Network-wide counters since genesis, plus a few 24-hour figures. Everything here is a full
//! aggregate over the event tables, which is why upstream caches the response — the query set is
//! identical for every caller and the numbers move once per block at most.

use axum::extract::State;
use axum::Json;
use midgard_core::units::{float_str, int_str};
use midgard_core::Second;

use crate::error::ApiResult;
use crate::models::Stats;
use crate::{usd, AppState};

pub async fn stats(State(state): State<AppState>) -> ApiResult<Json<Stats>> {
    // Keyed on the committed height: the answer is exactly right until the next block lands, so
    // a repeat request in the same block is served without touching the database at all.
    let height = state.cursor.last().height;
    let cached = state
        .stats_cache
        .get_or_compute(height, || compute(&state))
        .await?;

    Ok(Json((*cached).clone()))
}

async fn compute(state: &AppState) -> ApiResult<Stats> {
    let now = state.cursor.now_second();
    let day_ago = (now - 86_400).to_nano().to_i64();

    let rune_usd = usd::rune_price_now(&state.db, &state.config.usd_pools, now).await?;

    // One row, so the counters are consistent with each other rather than sampled across
    // several round trips while blocks land in between.
    let row: (i64, i64, i64, i64, i64, i64, i64) = sqlx::query_as(
        "SELECT
             COALESCE(COUNT(*), 0),
             COALESCE(SUM(CASE WHEN _direction IN (0,4,6) THEN from_e8 ELSE to_e8 END), 0)::BIGINT,
             COALESCE(SUM(CASE WHEN _direction IN (0,4,6) THEN 1 ELSE 0 END), 0)::BIGINT,
             COALESCE(SUM(CASE WHEN _direction IN (1,5,7) THEN 1 ELSE 0 END), 0)::BIGINT,
             COALESCE(SUM(CASE WHEN _direction = 2 THEN 1 ELSE 0 END), 0)::BIGINT,
             COALESCE(SUM(CASE WHEN _direction = 3 THEN 1 ELSE 0 END), 0)::BIGINT,
             COALESCE(SUM(CASE WHEN block_timestamp >= $1 THEN 1 ELSE 0 END), 0)::BIGINT
         FROM swap_events",
    )
    .bind(day_ago)
    .fetch_one(state.db.pool())
    .await?;

    let (swap_count, swap_volume, to_asset, to_rune, synth_mint, synth_burn, swaps_24h) = row;

    let (add_count, add_volume): (i64, i64) = sqlx::query_as(
        "SELECT COALESCE(COUNT(*), 0), COALESCE(SUM(rune_e8 + _asset_in_rune_e8), 0)::BIGINT
         FROM stake_events",
    )
    .fetch_one(state.db.pool())
    .await?;

    let (withdraw_count, withdraw_volume, ilp_paid): (i64, i64, i64) = sqlx::query_as(
        "SELECT COALESCE(COUNT(*), 0),
                COALESCE(SUM(emit_rune_e8 + _emit_asset_in_rune_e8), 0)::BIGINT,
                COALESCE(SUM(imp_loss_protection_e8), 0)::BIGINT
         FROM withdraw_events",
    )
    .fetch_one(state.db.pool())
    .await?;

    let rune_depth: Option<i64> = sqlx::query_scalar(
        "SELECT COALESCE(SUM(rune_e8), 0)::BIGINT FROM (
             SELECT DISTINCT ON (pool) rune_e8
             FROM block_pool_depths
             WHERE block_timestamp <= $1
             ORDER BY pool, block_timestamp DESC
         ) latest",
    )
    .bind(now.to_nano().to_i64())
    .fetch_one(state.db.pool())
    .await?;

    let unique_swappers = distinct_swappers(state, None).await?;
    let daily = distinct_swappers(state, Some(now - 86_400)).await?;
    let monthly = distinct_swappers(state, Some(now - 30 * 86_400)).await?;

    Ok(Stats {
        rune_price_usd: float_str(rune_usd),
        // Requires the historical BEP2/ERC20 migration events, which this port does not record.
        switched_rune: int_str(0),
        rune_depth: int_str(rune_depth.unwrap_or(0)),
        swap_volume: int_str(swap_volume),
        swap_count: int_str(swap_count),
        swap_count_24h: int_str(swaps_24h),
        to_asset_count: int_str(to_asset),
        to_rune_count: int_str(to_rune),
        synth_mint_count: int_str(synth_mint),
        synth_burn_count: int_str(synth_burn),
        daily_active_users: int_str(daily),
        monthly_active_users: int_str(monthly),
        unique_swapper_count: int_str(unique_swappers),
        add_liquidity_volume: int_str(add_volume),
        add_liquidity_count: int_str(add_count),
        withdraw_volume: int_str(withdraw_volume),
        withdraw_count: int_str(withdraw_count),
        impermanent_loss_protection_paid: int_str(ilp_paid),
    })
}

/// Distinct swap initiators, optionally since a point in time.
async fn distinct_swappers(state: &AppState, since: Option<Second>) -> ApiResult<i64> {
    let count: Option<i64> = sqlx::query_scalar(
        "SELECT COUNT(DISTINCT from_addr) FROM swap_events
         WHERE ($1::bigint IS NULL OR block_timestamp >= $1)",
    )
    .bind(since.map(|s| s.to_nano().to_i64()))
    .fetch_one(state.db.pool())
    .await?;
    Ok(count.unwrap_or(0))
}
