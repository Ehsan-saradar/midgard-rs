//! Pricing RUNE in USD.
//!
//! THORChain has no USD oracle. The price is inferred from the pools holding a dollar-pegged
//! asset: for each configured anchor pool, `runeDepth / assetDepth` is RUNE per dollar-ish unit,
//! and the deepest such pool wins because it is the hardest to move.
//!
//! This is why `usd_pools` is configuration and not a constant. The anchors have changed as
//! chains came and went — the default list still names a Binance Chain pool that has not
//! existed for years, which is harmless because a pool with no depth can never be the deepest.

use midgard_core::Second;
use midgard_db::Db;

use crate::error::ApiResult;

/// RUNE price in USD at a point in time, or `0.0` if no anchor pool had depth.
///
/// Zero rather than an error: pricing is a decoration on endpoints whose main content is
/// denominated in RUNE, and failing the whole request because the anchor pools were empty at
/// some historical instant would be worse than reporting a price we do not have.
pub async fn rune_price_at(db: &Db, pools: &[String], at: Second) -> ApiResult<f64> {
    if pools.is_empty() {
        return Ok(0.0);
    }

    // Latest depth at or before `at`, for each anchor pool. DISTINCT ON leans on the
    // (pool, block_timestamp DESC) index, so this is a handful of index seeks and not a scan.
    let rows: Vec<(String, i64, i64)> = sqlx::query_as(
        "SELECT DISTINCT ON (pool) pool, asset_e8, rune_e8
         FROM block_pool_depths
         WHERE pool = ANY($1) AND block_timestamp <= $2
         ORDER BY pool, block_timestamp DESC",
    )
    .bind(pools)
    .bind(at.to_nano().to_i64())
    .fetch_all(db.pool())
    .await?;

    Ok(price_from_depths(rows))
}

/// The deepest anchor pool's price. Split out so the choice is testable without a database.
fn price_from_depths(rows: Vec<(String, i64, i64)>) -> f64 {
    rows.into_iter()
        // A pool with no depth on either side cannot price anything.
        .filter(|(_, asset_e8, rune_e8)| *asset_e8 > 0 && *rune_e8 > 0)
        // Deepest in RUNE terms; that side is comparable across pools, the asset side is not.
        .max_by_key(|(_, _, rune_e8)| *rune_e8)
        .map(|(_, asset_e8, rune_e8)| asset_e8 as f64 / rune_e8 as f64)
        .unwrap_or(0.0)
}

/// RUNE price now.
pub async fn rune_price_now(db: &Db, pools: &[String], now: Second) -> ApiResult<f64> {
    rune_price_at(db, pools, now).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_pools_means_no_price() {
        assert_eq!(price_from_depths(vec![]), 0.0);
    }

    #[test]
    fn the_deepest_pool_sets_the_price() {
        // Shallow pool says 1 RUNE = 10 units, deep pool says 1 RUNE = 5. Deep wins.
        let rows = vec![
            ("SHALLOW".to_string(), 1_000, 100),
            ("DEEP".to_string(), 50_000, 10_000),
        ];
        assert_eq!(price_from_depths(rows), 5.0);
    }

    #[test]
    fn empty_pools_are_skipped_not_treated_as_zero_priced() {
        // A drained pool would otherwise win on some orderings and price RUNE at zero.
        let rows = vec![
            ("DRAINED".to_string(), 0, 0),
            ("REAL".to_string(), 2_000, 1_000),
        ];
        assert_eq!(price_from_depths(rows), 2.0);
    }

    #[test]
    fn a_pool_with_depth_on_only_one_side_is_skipped() {
        let rows = vec![
            ("HALF".to_string(), 0, 999_999),
            ("REAL".to_string(), 2_000, 1_000),
        ];
        assert_eq!(price_from_depths(rows), 2.0);
    }

    #[test]
    fn all_pools_empty_gives_zero_rather_than_nan() {
        let rows = vec![("A".to_string(), 0, 0), ("B".to_string(), 5, 0)];
        let p = price_from_depths(rows);
        assert!(p.is_finite(), "price should not be NaN or infinite");
        assert_eq!(p, 0.0);
    }
}
