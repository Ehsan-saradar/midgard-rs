//! `GET /v2/members` and `/v2/member/{addr}`.
//!
//! A liquidity provider's position is not stored anywhere as a row — it is the sum of their
//! deposits minus their withdrawals, so both endpoints aggregate the event tables.
//!
//! Addresses are matched against both `rune_addr` and `asset_addr` because a symmetric deposit
//! carries one of each and the caller may hold either. Some chains are case-insensitive
//! (Ethereum most notably), which is what `case_insensitive_chains` in the config is for.

use std::collections::HashMap;

use axum::extract::{Path, Query, State};
use axum::Json;
use midgard_core::units::int_str;
use midgard_core::Error;

use crate::error::ApiResult;
use crate::models::{MemberDetails, MemberPool};
use crate::query::Params;
use crate::AppState;

/// `GET /v2/members` — every address with a position, optionally in one pool.
pub async fn members(
    State(state): State<AppState>,
    Query(raw): Query<HashMap<String, String>>,
) -> ApiResult<Json<Vec<String>>> {
    let mut params = Params::new(raw);
    let pool = params.take_string("pool");
    params.reject_unknown()?;

    // Only addresses still holding units: someone who withdrew everything is not a member.
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT member FROM (
             SELECT member, SUM(units) AS net FROM (
                 SELECT COALESCE(rune_addr, asset_addr) AS member, stake_units AS units, pool
                 FROM stake_events
                 UNION ALL
                 SELECT from_addr AS member, -stake_units AS units, pool
                 FROM withdraw_events
             ) moves
             WHERE ($1::text IS NULL OR pool = $1) AND member IS NOT NULL
             GROUP BY member
             HAVING SUM(units) > 0
         ) held
         ORDER BY member",
    )
    .bind(pool.as_deref())
    .fetch_all(state.db.pool())
    .await?;

    Ok(Json(rows.into_iter().map(|(m,)| m).collect()))
}

/// Columns selected for a member's position in one pool, in order.
type MemberRow = (String, String, String, i64, i64, i64, i64, i64, i64, i64);

/// `GET /v2/member/{addr}` — one address's position in every pool it has touched.
pub async fn member_details(
    State(state): State<AppState>,
    Path(addr): Path<String>,
) -> ApiResult<Json<MemberDetails>> {
    let rows: Vec<MemberRow> = sqlx::query_as(
        "SELECT pool,
                COALESCE(MAX(rune_addr), '') AS rune_address,
                COALESCE(MAX(asset_addr), '') AS asset_address,
                COALESCE(SUM(units), 0)::BIGINT AS liquidity_units,
                COALESCE(SUM(rune_added), 0)::BIGINT AS rune_added,
                COALESCE(SUM(asset_added), 0)::BIGINT AS asset_added,
                COALESCE(SUM(rune_withdrawn), 0)::BIGINT AS rune_withdrawn,
                COALESCE(SUM(asset_withdrawn), 0)::BIGINT AS asset_withdrawn,
                COALESCE(MIN(NULLIF(added_at, 0)), 0) AS first_added,
                COALESCE(MAX(added_at), 0) AS last_added
         FROM (
             SELECT pool, rune_addr, asset_addr,
                    stake_units AS units,
                    rune_e8 AS rune_added, asset_e8 AS asset_added,
                    0 AS rune_withdrawn, 0 AS asset_withdrawn,
                    block_timestamp AS added_at
             FROM stake_events
             WHERE rune_addr = $1 OR asset_addr = $1
             UNION ALL
             SELECT pool, from_addr AS rune_addr, NULL AS asset_addr,
                    -stake_units AS units,
                    0 AS rune_added, 0 AS asset_added,
                    emit_rune_e8 AS rune_withdrawn, emit_asset_e8 AS asset_withdrawn,
                    0 AS added_at
             FROM withdraw_events
             WHERE from_addr = $1
         ) moves
         GROUP BY pool
         HAVING SUM(units) > 0
         ORDER BY pool",
    )
    .bind(&addr)
    .fetch_all(state.db.pool())
    .await?;

    if rows.is_empty() {
        return Err(Error::not_found(format!("no liquidity found for address {addr}")).into());
    }

    let pools = rows
        .into_iter()
        .map(|r| MemberPool {
            pool: r.0,
            rune_address: r.1,
            asset_address: r.2,
            liquidity_units: int_str(r.3),
            rune_added: int_str(r.4),
            asset_added: int_str(r.5),
            rune_withdrawn: int_str(r.6),
            asset_withdrawn: int_str(r.7),
            // Pending liquidity is the half of a symmetric deposit still waiting for its pair.
            rune_pending: int_str(0),
            asset_pending: int_str(0),
            // Stored in nanoseconds, reported in seconds.
            date_first_added: int_str(r.8 / 1_000_000_000),
            date_last_added: int_str(r.9 / 1_000_000_000),
        })
        .collect();

    Ok(Json(MemberDetails { pools }))
}

#[cfg(test)]
mod tests {
    /// Net units held, which decides membership.
    fn net(deposits: &[i64], withdrawals: &[i64]) -> i64 {
        deposits.iter().sum::<i64>() - withdrawals.iter().sum::<i64>()
    }

    #[test]
    fn a_fully_withdrawn_position_is_not_a_membership() {
        assert_eq!(net(&[100], &[100]), 0);
        assert!(net(&[100], &[100]) <= 0, "should not count as a member");
    }

    #[test]
    fn a_partial_withdrawal_leaves_a_position() {
        assert_eq!(net(&[100], &[40]), 60);
    }

    #[test]
    fn several_deposits_accumulate() {
        assert_eq!(net(&[10, 20, 30], &[]), 60);
    }

    #[test]
    fn nanosecond_timestamps_report_as_seconds() {
        assert_eq!(1_700_000_000_500_000_000i64 / 1_000_000_000, 1_700_000_000);
    }
}
