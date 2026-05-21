//! `GET /v2/actions`.
//!
//! A reverse-chronological feed of what happened, filterable by address, asset and type. This is
//! the endpoint wallets poll, so it is the one where the `event_id` encoding earns its keep:
//! ordering and pagination are `ORDER BY event_id DESC` and a `WHERE event_id < $token`, with no
//! tuple comparison and no offset scan.
//!
//! Keyset pagination rather than `OFFSET`: an offset gets slower the deeper you page, and worse,
//! shifts under you when new blocks arrive between requests, so a client walking the feed sees
//! duplicates and gaps.

use std::collections::HashMap;

use axum::extract::{Query, State};
use axum::Json;
use midgard_core::units::int_str;
use midgard_core::Error;
use serde_json::json;

use crate::error::ApiResult;
use crate::models::{Action, Actions, ActionsMeta, Coin, Transaction};
use crate::query::Params;
use crate::AppState;

const DEFAULT_LIMIT: i64 = 50;

pub async fn actions(
    State(state): State<AppState>,
    Query(raw): Query<HashMap<String, String>>,
) -> ApiResult<Json<Actions>> {
    let mut params = Params::new(raw);
    let address = params.take_string("address");
    let asset = params.take_string("asset");
    let action_type = params.take_string("type");
    let next_token = params.take_i64("nextPageToken")?;
    let limit = limit_of(&mut params, &state)?;
    params.reject_unknown()?;

    let mut rows = Vec::new();

    // Each action kind is its own table, so the feed is a merge of per-table queries rather than
    // one scan. Each is limited to `limit`, so the merge has enough rows to fill a page whichever
    // kind dominates.
    if wants(&action_type, "swap") {
        rows.extend(
            swaps(
                &state,
                address.as_deref(),
                asset.as_deref(),
                next_token,
                limit,
            )
            .await?,
        );
    }
    if wants(&action_type, "addLiquidity") {
        rows.extend(
            deposits(
                &state,
                address.as_deref(),
                asset.as_deref(),
                next_token,
                limit,
            )
            .await?,
        );
    }
    if wants(&action_type, "withdraw") {
        rows.extend(
            withdrawals(
                &state,
                address.as_deref(),
                asset.as_deref(),
                next_token,
                limit,
            )
            .await?,
        );
    }

    // Newest first, then trim to the page. Sorting after the merge is what makes the per-table
    // limits safe.
    rows.sort_by(|a, b| b.0.cmp(&a.0));
    rows.truncate(limit as usize);

    let next_page_token = rows.last().map(|(id, _)| *id).unwrap_or(0);
    let count = rows.len();

    Ok(Json(Actions {
        actions: rows.into_iter().map(|(_, a)| a).collect(),
        count: int_str(count as i64),
        meta: ActionsMeta {
            next_page_token: int_str(next_page_token),
            prev_page_token: int_str(next_token.unwrap_or(0)),
        },
    }))
}

fn wants(filter: &Option<String>, kind: &str) -> bool {
    match filter {
        None => true,
        Some(f) => f.split(',').any(|t| t.trim().eq_ignore_ascii_case(kind)),
    }
}

fn limit_of(params: &mut Params, state: &AppState) -> Result<i64, Error> {
    let max = state.config.endpoints.action_params.max_limit as i64;
    match params.take_i64("limit")? {
        None => Ok(DEFAULT_LIMIT.min(max)),
        Some(l) if l >= 1 && l <= max => Ok(l),
        Some(l) => Err(Error::bad_request(format!(
            "'limit' must be between 1 and {max}, got {l}"
        ))),
    }
}

/// Columns selected for a swap action, in order.
type SwapRow = (
    i64,
    i64,
    String,
    String,
    String,
    String,
    i64,
    String,
    i64,
    String,
);
/// Columns selected for a deposit action, in order.
type DepositRow = (
    i64,
    i64,
    String,
    Option<String>,
    Option<String>,
    i64,
    i64,
    i64,
);
/// Columns selected for a withdrawal action, in order.
type WithdrawRow = (i64, i64, String, String, String, i64, i64, i64, i64);

/// `(event_id, action)` so the merge can sort without re-parsing the response.
type Row = (i64, Action);

async fn swaps(
    state: &AppState,
    address: Option<&str>,
    asset: Option<&str>,
    before: Option<i64>,
    limit: i64,
) -> ApiResult<Vec<Row>> {
    let rows: Vec<SwapRow> = sqlx::query_as(
        "SELECT event_id, block_timestamp, pool, tx, from_addr, from_asset, from_e8,
                    to_asset, to_e8, memo
             FROM swap_events
             WHERE ($1::bigint IS NULL OR event_id < $1)
               AND ($2::text IS NULL OR from_addr = $2)
               AND ($3::text IS NULL OR pool = $3)
             ORDER BY event_id DESC
             LIMIT $4",
    )
    .bind(before)
    .bind(address)
    .bind(asset)
    .bind(limit)
    .fetch_all(state.db.pool())
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| {
            let (event_id, ts, pool, tx, from_addr, from_asset, from_e8, to_asset, to_e8, memo) = r;
            (
                event_id,
                Action {
                    date: int_str(ts),
                    height: int_str(midgard_db::eventid::height_of(event_id)),
                    action_type: "swap".to_string(),
                    status: "success".to_string(),
                    pools: vec![pool],
                    inputs: vec![Transaction {
                        address: from_addr,
                        tx_id: tx,
                        coins: vec![Coin {
                            asset: from_asset,
                            amount: int_str(from_e8),
                        }],
                    }],
                    outputs: vec![Transaction {
                        address: String::new(),
                        tx_id: String::new(),
                        coins: vec![Coin {
                            asset: to_asset,
                            amount: int_str(to_e8),
                        }],
                    }],
                    metadata: json!({ "swap": { "memo": memo } }),
                },
            )
        })
        .collect())
}

async fn deposits(
    state: &AppState,
    address: Option<&str>,
    asset: Option<&str>,
    before: Option<i64>,
    limit: i64,
) -> ApiResult<Vec<Row>> {
    let rows: Vec<DepositRow> = sqlx::query_as(
        "SELECT event_id, block_timestamp, pool, rune_addr, asset_addr,
                    rune_e8, asset_e8, stake_units
             FROM stake_events
             WHERE ($1::bigint IS NULL OR event_id < $1)
               AND ($2::text IS NULL OR rune_addr = $2 OR asset_addr = $2)
               AND ($3::text IS NULL OR pool = $3)
             ORDER BY event_id DESC
             LIMIT $4",
    )
    .bind(before)
    .bind(address)
    .bind(asset)
    .bind(limit)
    .fetch_all(state.db.pool())
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| {
            let (event_id, ts, pool, rune_addr, asset_addr, rune_e8, asset_e8, units) = r;
            let mut inputs = Vec::new();
            if rune_e8 > 0 {
                inputs.push(Transaction {
                    address: rune_addr.unwrap_or_default(),
                    tx_id: String::new(),
                    coins: vec![Coin {
                        asset: midgard_core::asset::NATIVE_RUNE.to_string(),
                        amount: int_str(rune_e8),
                    }],
                });
            }
            if asset_e8 > 0 {
                inputs.push(Transaction {
                    address: asset_addr.unwrap_or_default(),
                    tx_id: String::new(),
                    coins: vec![Coin {
                        asset: pool.clone(),
                        amount: int_str(asset_e8),
                    }],
                });
            }

            (
                event_id,
                Action {
                    date: int_str(ts),
                    height: int_str(midgard_db::eventid::height_of(event_id)),
                    action_type: "addLiquidity".to_string(),
                    status: "success".to_string(),
                    pools: vec![pool],
                    inputs,
                    outputs: Vec::new(),
                    metadata: json!({
                        "addLiquidity": { "liquidityUnits": int_str(units) }
                    }),
                },
            )
        })
        .collect())
}

async fn withdrawals(
    state: &AppState,
    address: Option<&str>,
    asset: Option<&str>,
    before: Option<i64>,
    limit: i64,
) -> ApiResult<Vec<Row>> {
    let rows: Vec<WithdrawRow> = sqlx::query_as(
        "SELECT event_id, block_timestamp, pool, tx, from_addr,
                emit_rune_e8, emit_asset_e8, stake_units, basis_points
         FROM withdraw_events
         WHERE ($1::bigint IS NULL OR event_id < $1)
           AND ($2::text IS NULL OR from_addr = $2)
           AND ($3::text IS NULL OR pool = $3)
         ORDER BY event_id DESC
         LIMIT $4",
    )
    .bind(before)
    .bind(address)
    .bind(asset)
    .bind(limit)
    .fetch_all(state.db.pool())
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| {
            let (event_id, ts, pool, tx, from_addr, rune_e8, asset_e8, units, bp) = r;
            let mut outputs = Vec::new();
            if rune_e8 > 0 {
                outputs.push(Transaction {
                    address: from_addr.clone(),
                    tx_id: String::new(),
                    coins: vec![Coin {
                        asset: midgard_core::asset::NATIVE_RUNE.to_string(),
                        amount: int_str(rune_e8),
                    }],
                });
            }
            if asset_e8 > 0 {
                outputs.push(Transaction {
                    address: from_addr.clone(),
                    tx_id: String::new(),
                    coins: vec![Coin {
                        asset: pool.clone(),
                        amount: int_str(asset_e8),
                    }],
                });
            }

            (
                event_id,
                Action {
                    date: int_str(ts),
                    height: int_str(midgard_db::eventid::height_of(event_id)),
                    action_type: "withdraw".to_string(),
                    status: "success".to_string(),
                    pools: vec![pool],
                    inputs: vec![Transaction {
                        address: from_addr,
                        tx_id: tx,
                        coins: Vec::new(),
                    }],
                    outputs,
                    metadata: json!({
                        "withdraw": {
                            "liquidityUnits": int_str(-units),
                            "basisPoints": int_str(bp),
                        }
                    }),
                },
            )
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_type_filter_accepts_everything() {
        assert!(wants(&None, "swap"));
        assert!(wants(&None, "withdraw"));
    }

    #[test]
    fn a_type_filter_selects_only_that_kind() {
        let f = Some("swap".to_string());
        assert!(wants(&f, "swap"));
        assert!(!wants(&f, "withdraw"));
    }

    #[test]
    fn type_filters_accept_a_comma_separated_list() {
        let f = Some("swap,withdraw".to_string());
        assert!(wants(&f, "swap"));
        assert!(wants(&f, "withdraw"));
        assert!(!wants(&f, "addLiquidity"));
    }

    #[test]
    fn type_matching_ignores_case_and_spacing() {
        let f = Some("SWAP, addliquidity".to_string());
        assert!(wants(&f, "swap"));
        assert!(wants(&f, "addLiquidity"));
    }
}
