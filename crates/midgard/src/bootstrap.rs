//! Seeding pool state when starting part-way along the chain.
//!
//! Depths are reconstructed by applying deltas — a swap moves so much in and so much out — so
//! they are only absolute if the replay started at block 1. Start at block 27,000,000 with an
//! empty database and every pool begins at zero and promptly goes negative, which looks like a
//! decoding bug and is not one.
//!
//! Syncing from genesis is the correct answer and what upstream does. It is also 27 million
//! blocks. For an operator who wants a node serving current data today, this seeds the starting
//! depths from THORNode's own pool state instead.
//!
//! The trade-off is explicit and worth stating: a seeded database is **correct from the start
//! height onwards and has no history before it**. Anything asking for a window that opens before
//! the seed gets the seeded snapshot as its opening balance rather than the truth. That is
//! usually what you want from a node deployed to follow the tip, and never what you want from
//! one backing historical charts.

use anyhow::{Context, Result};
use midgard_chain::thornode::ThorNode;
use midgard_core::Nano;
use midgard_db::eventid;
use midgard_db::Db;

/// Write the starting depths for every pool THORNode currently reports.
///
/// `at` should be just before the first block we are about to index, so the rows are picked up
/// by "latest depth at or before T" for every query from that point on.
///
/// Returns the number of pools seeded.
pub async fn seed_pool_depths(
    db: &Db,
    thornode: &ThorNode,
    height: i64,
    at: Nano,
) -> Result<usize> {
    let pools = thornode
        .pools()
        .await
        .context("fetching pool state from THORNode to seed depths")?;

    if pools.is_empty() {
        anyhow::bail!("THORNode returned no pools; refusing to seed an empty state");
    }

    let mut tx = db.pool().begin().await?;
    let timestamp = at.to_i64();
    // Sorts before every real event at this height, so a genuine status change in the first
    // indexed block supersedes the seeded one.
    let event_id = eventid::first_id_at_height(height);

    let mut seeded = 0;
    for pool in &pools {
        if pool.asset.is_empty() {
            continue;
        }

        sqlx::query(
            "INSERT INTO block_pool_depths
                 (pool, asset_e8, rune_e8, synth_e8, units, block_timestamp)
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(&pool.asset)
        .bind(pool.asset_e8())
        .bind(pool.rune_e8())
        .bind(pool.synth_e8())
        .bind(pool.units())
        .bind(timestamp)
        .execute(&mut *tx)
        .await?;

        // Status too, otherwise every pool reads as "unknown" until it happens to change state.
        sqlx::query(
            "INSERT INTO pool_events (asset, status, event_id, block_timestamp)
             VALUES ($1, $2, $3, $4)",
        )
        .bind(&pool.asset)
        .bind(pool.status.to_ascii_lowercase())
        .bind(event_id)
        .bind(timestamp)
        .execute(&mut *tx)
        .await?;

        seeded += 1;
    }

    tx.commit().await?;

    tracing::warn!(
        pools = seeded,
        height,
        "seeded pool depths from THORNode: this database has NO history before this height"
    );
    Ok(seeded)
}

#[cfg(test)]
mod tests {
    use midgard_db::eventid::{self, EventId};

    #[test]
    fn the_seed_event_id_sorts_before_every_real_event_at_that_height() {
        let height = 27_260_000;
        let seed = eventid::first_id_at_height(height);

        // Whatever the first real event of that block turns out to be, the seed precedes it.
        for real in [
            EventId::begin_block(height).to_i64(),
            EventId::tx_event(height, 1, 1).to_i64(),
            EventId::end_block(height, 1).to_i64(),
        ] {
            assert!(seed < real, "seed {seed} should sort before {real}");
        }
    }

    #[test]
    fn the_seed_belongs_to_the_height_it_names() {
        let height = 27_260_000;
        assert_eq!(
            eventid::height_of(eventid::first_id_at_height(height)),
            height
        );
    }
}
