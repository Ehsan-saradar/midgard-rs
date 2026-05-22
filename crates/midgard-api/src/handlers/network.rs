//! `GET /v2/network`.
//!
//! Node counts and bond figures come from THORNode's REST API rather than the database: they are
//! current validator state, not events, and there is no historical series to reconstruct.
//!
//! When THORNode is unreachable the node-derived fields report zero and the pooled figures —
//! which do come from the database — are still correct. Failing the whole request instead would
//! take out a widely-polled endpoint over a dependency that is not needed for most of it.

use axum::extract::State;
use axum::Json;
use midgard_core::units::{float_str, int_str};

use crate::error::ApiResult;
use crate::models::{BondMetrics, Network};
use crate::AppState;

pub async fn network(State(state): State<AppState>) -> ApiResult<Json<Network>> {
    let now = state.cursor.now_second();

    let total_pooled: Option<i64> = sqlx::query_scalar(
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

    let nodes = match state.thornode.nodes().await {
        Ok(nodes) => nodes,
        Err(e) => {
            tracing::warn!(error = %e, "THORNode unreachable, reporting node fields as zero");
            Vec::new()
        }
    };

    let mut active: Vec<i64> = nodes
        .iter()
        .filter(|n| n.is_active())
        .map(|n| n.bond_e8())
        .collect();
    active.sort_unstable();

    let standby = nodes.len().saturating_sub(active.len());
    let metrics = bond_metrics(&active);

    Ok(Json(Network {
        active_node_count: int_str(active.len() as i64),
        standby_node_count: int_str(standby as i64),
        total_reserve: int_str(0),
        total_pooled_rune: int_str(total_pooled.unwrap_or(0)),
        bonding_apy: float_str(0.0),
        liquidity_apy: float_str(0.0),
        pool_activation_countdown: int_str(0),
        next_churn_height: int_str(0),
        bond_metrics: metrics,
    }))
}

/// Summary statistics over the active nodes' bonds. `sorted` must be ascending.
fn bond_metrics(sorted: &[i64]) -> BondMetrics {
    if sorted.is_empty() {
        return BondMetrics {
            total_active_bond: int_str(0),
            average_active_bond: int_str(0),
            median_active_bond: int_str(0),
            minimum_active_bond: int_str(0),
            maximum_active_bond: int_str(0),
        };
    }

    let total: i64 = sorted.iter().sum();
    // Integer division, matching upstream: these are e8 amounts where a fractional unit is
    // meaningless.
    let average = total / sorted.len() as i64;
    let median = sorted[sorted.len() / 2];

    BondMetrics {
        total_active_bond: int_str(total),
        average_active_bond: int_str(average),
        median_active_bond: int_str(median),
        minimum_active_bond: int_str(sorted[0]),
        maximum_active_bond: int_str(sorted[sorted.len() - 1]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_active_nodes_is_all_zeros_not_a_division_by_zero() {
        let m = bond_metrics(&[]);
        assert_eq!(m.total_active_bond, "0");
        assert_eq!(m.average_active_bond, "0");
        assert_eq!(m.median_active_bond, "0");
    }

    #[test]
    fn metrics_over_a_sorted_list() {
        let m = bond_metrics(&[10, 20, 30, 40]);
        assert_eq!(m.total_active_bond, "100");
        assert_eq!(m.average_active_bond, "25");
        assert_eq!(m.minimum_active_bond, "10");
        assert_eq!(m.maximum_active_bond, "40");
    }

    #[test]
    fn a_single_node_is_its_own_everything() {
        let m = bond_metrics(&[42]);
        assert_eq!(m.total_active_bond, "42");
        assert_eq!(m.average_active_bond, "42");
        assert_eq!(m.median_active_bond, "42");
        assert_eq!(m.minimum_active_bond, "42");
        assert_eq!(m.maximum_active_bond, "42");
    }

    #[test]
    fn median_of_an_odd_count_is_the_middle_element() {
        assert_eq!(bond_metrics(&[10, 20, 30]).median_active_bond, "20");
    }

    #[test]
    fn averages_truncate_rather_than_round() {
        // 100 / 3 = 33.33; e8 amounts have no fractional part to report.
        assert_eq!(bond_metrics(&[33, 33, 34]).average_active_bond, "33");
    }
}
