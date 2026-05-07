//! `GET /v2/pools`, `/v2/pool/{asset}`, `/v2/knownpools`.

use std::collections::HashMap;

use axum::extract::{Path, Query, State};
use axum::Json;
use midgard_core::units::{asset_price, float_str, int_str, luvi, ratio, UNKNOWN_DECIMALS};
use midgard_core::{Error, Second};

use crate::error::ApiResult;
use crate::models::PoolDetail;
use crate::query::Params;
use crate::{usd, AppState};

/// Window used for the yield figures when the caller does not say otherwise. Upstream's default
/// is 14 days, and the numbers are widely quoted, so it stays 14 here.
const DEFAULT_PERIOD_DAYS: i64 = 14;

/// A pool's latest depth, straight out of `block_pool_depths`.
#[derive(Debug, Clone, Default)]
struct PoolState {
    asset_e8: i64,
    rune_e8: i64,
    synth_e8: i64,
    units: i64,
    status: String,
}

pub async fn pools(
    State(state): State<AppState>,
    Query(raw): Query<HashMap<String, String>>,
) -> ApiResult<Json<Vec<PoolDetail>>> {
    let mut params = Params::new(raw);
    let status_filter = params.take_string("status").map(|s| s.to_ascii_lowercase());
    let period = period_days(&mut params)?;
    params.reject_unknown()?;

    let now = state.cursor.now_second();
    let states = load_pool_states(&state, now).await?;
    let rune_usd = usd::rune_price_now(&state.db, &state.config.usd_pools, now).await?;

    let mut out = Vec::with_capacity(states.len());
    for (asset, st) in states {
        if let Some(want) = &status_filter {
            if &st.status != want {
                continue;
            }
        }
        out.push(build_detail(&state, &asset, &st, rune_usd, period, now).await?);
    }

    // Deepest first: the order every front-end displays them in anyway.
    out.sort_by(|a, b| {
        let (x, y) = (
            b.rune_depth.parse::<i64>().unwrap_or(0),
            a.rune_depth.parse::<i64>().unwrap_or(0),
        );
        x.cmp(&y)
    });
    Ok(Json(out))
}

pub async fn pool(
    State(state): State<AppState>,
    Path(asset): Path<String>,
    Query(raw): Query<HashMap<String, String>>,
) -> ApiResult<Json<PoolDetail>> {
    let mut params = Params::new(raw);
    let period = period_days(&mut params)?;
    params.reject_unknown()?;

    let now = state.cursor.now_second();
    let states = load_pool_states(&state, now).await?;
    let st = states
        .get(&asset)
        .ok_or_else(|| Error::not_found(format!("pool {asset} not found")))?;

    let rune_usd = usd::rune_price_now(&state.db, &state.config.usd_pools, now).await?;
    Ok(Json(
        build_detail(&state, &asset, st, rune_usd, period, now).await?,
    ))
}

/// `GET /v2/knownpools` — every pool ever seen, mapped to its current status.
pub async fn known_pools(
    State(state): State<AppState>,
) -> ApiResult<Json<HashMap<String, String>>> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT DISTINCT ON (asset) asset, status
         FROM pool_events
         ORDER BY asset, block_timestamp DESC",
    )
    .fetch_all(state.db.pool())
    .await?;

    Ok(Json(rows.into_iter().collect()))
}

/// Latest depth and status for every pool.
async fn load_pool_states(state: &AppState, now: Second) -> ApiResult<HashMap<String, PoolState>> {
    let depths: Vec<(String, i64, i64, i64, i64)> = sqlx::query_as(
        "SELECT DISTINCT ON (pool) pool, asset_e8, rune_e8, synth_e8, units
         FROM block_pool_depths
         WHERE block_timestamp <= $1
         ORDER BY pool, block_timestamp DESC",
    )
    .bind(now.to_nano().to_i64())
    .fetch_all(state.db.pool())
    .await?;

    let statuses: Vec<(String, String)> = sqlx::query_as(
        "SELECT DISTINCT ON (asset) asset, status
         FROM pool_events
         ORDER BY asset, block_timestamp DESC",
    )
    .fetch_all(state.db.pool())
    .await?;
    let statuses: HashMap<String, String> = statuses.into_iter().collect();

    Ok(depths
        .into_iter()
        .map(|(pool, asset_e8, rune_e8, synth_e8, units)| {
            let status = statuses
                .get(&pool)
                .cloned()
                // A pool with depth but no status event has not been staged yet.
                .unwrap_or_else(|| "unknown".to_string());
            (
                pool,
                PoolState {
                    asset_e8,
                    rune_e8,
                    synth_e8,
                    units,
                    status,
                },
            )
        })
        .collect())
}

async fn build_detail(
    state: &AppState,
    asset: &str,
    st: &PoolState,
    rune_usd: f64,
    period_days: i64,
    now: Second,
) -> ApiResult<PoolDetail> {
    let price = asset_price(st.asset_e8, st.rune_e8);
    let window_start = now - period_days * 86_400;

    let volume_24h = swap_volume_since(state, asset, now - 86_400).await?;
    let earnings = pool_earnings_since(state, asset, window_start).await?;

    // Annualise the period's earnings against current depth. Both sides of a pool are worth the
    // same, so total pool value in RUNE is twice the RUNE depth.
    let pool_value_rune = 2.0 * st.rune_e8 as f64;
    let periods_per_year = midgard_core::time::periods_per_year(window_start, now);
    let apr = ratio(earnings as f64 * periods_per_year, pool_value_rune);

    let decimals = state
        .config
        .pools_decimal
        .get(asset)
        .copied()
        .unwrap_or(UNKNOWN_DECIMALS);

    Ok(PoolDetail {
        asset: asset.to_string(),
        status: st.status.clone(),
        asset_depth: int_str(st.asset_e8),
        rune_depth: int_str(st.rune_e8),
        asset_price: float_str(price),
        asset_price_usd: float_str(price * rune_usd),
        liquidity_units: int_str(st.units),
        // Synth units are not tracked separately from the pool's own units in this port; the
        // synth *supply* is, and that is what clients use to size the savers side.
        synth_units: int_str(0),
        synth_supply: int_str(st.synth_e8),
        units: int_str(st.units),
        native_decimal: int_str(decimals),
        savers_depth: int_str(st.synth_e8),
        savers_units: int_str(0),
        volume_24h: int_str(volume_24h),
        annual_percentage_rate: float_str(apr),
        pool_apy: float_str(apr.max(0.0)),
        earnings: int_str(earnings),
        earnings_annual_as_percent_of_depth: float_str(apr),
        liquidity_in_usd: float_str(pool_value_rune * rune_usd),
        lp_luvi: float_str(luvi(st.asset_e8, st.rune_e8, st.units)),
        savers_apr: float_str(0.0),
        total_collateral: int_str(0),
        total_debt_tor: int_str(0),
    })
}

/// Swap volume through a pool since `since`, denominated in RUNE.
///
/// Each swap has RUNE on one side, so the RUNE leg is the volume regardless of direction: it is
/// `from_e8` when swapping out of RUNE and `to_e8` when swapping into it.
async fn swap_volume_since(state: &AppState, pool: &str, since: Second) -> ApiResult<i64> {
    let volume: Option<i64> = sqlx::query_scalar(
        "SELECT COALESCE(SUM(CASE WHEN _direction IN (0, 2, 4, 6) THEN from_e8 ELSE to_e8 END), 0)::BIGINT
         FROM swap_events
         WHERE pool = $1 AND block_timestamp >= $2",
    )
    .bind(pool)
    .bind(since.to_nano().to_i64())
    .fetch_one(state.db.pool())
    .await?;
    Ok(volume.unwrap_or(0))
}

/// A pool's earnings since `since`: liquidity fees plus its share of block rewards.
async fn pool_earnings_since(state: &AppState, pool: &str, since: Second) -> ApiResult<i64> {
    let ts = since.to_nano().to_i64();

    let fees: Option<i64> = sqlx::query_scalar(
        "SELECT COALESCE(SUM(liq_fee_in_rune_e8), 0)::BIGINT
         FROM swap_events WHERE pool = $1 AND block_timestamp >= $2",
    )
    .bind(pool)
    .bind(ts)
    .fetch_one(state.db.pool())
    .await?;

    let rewards: Option<i64> = sqlx::query_scalar(
        "SELECT COALESCE(SUM(rune_e8), 0)::BIGINT
         FROM rewards_event_entries WHERE pool = $1 AND block_timestamp >= $2",
    )
    .bind(pool)
    .bind(ts)
    .fetch_one(state.db.pool())
    .await?;

    Ok(fees.unwrap_or(0) + rewards.unwrap_or(0))
}

fn period_days(params: &mut Params) -> Result<i64, Error> {
    match params.take_i64("period")? {
        None => Ok(DEFAULT_PERIOD_DAYS),
        Some(d) if d > 0 => Ok(d),
        Some(d) => Err(Error::bad_request(format!(
            "'period' must be a positive number of days, got {d}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(pairs: &[(&str, &str)]) -> Params {
        Params::new(
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        )
    }

    #[test]
    fn period_defaults_to_fourteen_days() {
        assert_eq!(period_days(&mut params(&[])).unwrap(), 14);
    }

    #[test]
    fn period_can_be_overridden() {
        assert_eq!(period_days(&mut params(&[("period", "30")])).unwrap(), 30);
    }

    #[test]
    fn a_non_positive_period_is_rejected() {
        // Zero would make the annualisation divide by a zero-length window.
        for bad in ["0", "-7"] {
            let err = period_days(&mut params(&[("period", bad)])).unwrap_err();
            assert_eq!(err.status_code(), 400, "{bad}");
        }
    }

    #[test]
    fn a_non_numeric_period_is_rejected() {
        assert!(period_days(&mut params(&[("period", "fortnight")])).is_err());
    }
}
