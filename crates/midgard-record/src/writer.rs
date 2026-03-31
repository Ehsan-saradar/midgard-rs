//! Buffering blocks and writing them.
//!
//! Blocks are accumulated in memory and flushed in one transaction per batch. Per-block
//! transactions would be correct but slow — catching up means twenty-seven million of them — and
//! the batch size is the operator's dial between commit overhead and how much work is lost when
//! the process dies.
//!
//! The invariant that makes this safe: a block's event rows and its `block_log` entry go in the
//! *same* transaction. The cursor is therefore never ahead of the data. A crash mid-batch loses
//! up to `commit_batch_size` blocks and re-fetches them on restart, which is fine because
//! decoding is deterministic — replaying a block produces exactly the rows it produced before.

use midgard_chain::Block;
use midgard_core::Nano;
use midgard_db::eventid::EventId;
use midgard_db::{Db, DbError};
use sqlx::{Postgres, QueryBuilder, Transaction};

use crate::depth::{Depth, DepthTracker};
use crate::events::{decode, Decoded, Recorded};

#[derive(Debug, thiserror::Error)]
pub enum WriteError {
    #[error(transparent)]
    Db(#[from] DbError),

    #[error("database: {0}")]
    Sql(#[from] sqlx::Error),
}

/// One row waiting to be written, with the identity every event table needs.
struct Pending {
    event_id: i64,
    block_timestamp: i64,
    record: Recorded,
}

/// A depth row waiting to be written.
struct PendingDepth {
    pool: String,
    depth: Depth,
    block_timestamp: i64,
}

/// Buffers decoded blocks and flushes them transactionally.
pub struct BlockWriter {
    db: Db,
    batch_size: usize,

    /// Blocks in the current batch, as `(height, timestamp, hash)`.
    blocks: Vec<(i64, i64, Vec<u8>)>,
    events: Vec<Pending>,
    depths: Vec<PendingDepth>,

    tracker: DepthTracker,

    /// Event types seen that we have no decoder for, so each is logged once rather than once
    /// per occurrence — a new type on a busy chain would otherwise produce millions of lines.
    unknown_seen: std::collections::HashSet<String>,
}

impl BlockWriter {
    pub fn new(db: Db, batch_size: usize) -> BlockWriter {
        BlockWriter {
            db,
            batch_size: batch_size.max(1),
            blocks: Vec::new(),
            events: Vec::new(),
            depths: Vec::new(),
            tracker: DepthTracker::new(),
            unknown_seen: std::collections::HashSet::new(),
        }
    }

    /// Restore the in-memory depth state after a restart.
    pub async fn restore(&mut self) -> Result<(), WriteError> {
        let rows: Vec<(String, i64, i64, i64, i64)> = sqlx::query_as(
            "SELECT DISTINCT ON (pool) pool, asset_e8, rune_e8, synth_e8, units
             FROM block_pool_depths
             ORDER BY pool, block_timestamp DESC",
        )
        .fetch_all(self.db.pool())
        .await?;

        let count = rows.len();
        self.tracker.load(
            rows.into_iter()
                .map(|(pool, asset_e8, rune_e8, synth_e8, units)| {
                    (
                        pool,
                        Depth {
                            asset_e8,
                            rune_e8,
                            synth_e8,
                            units,
                        },
                    )
                }),
        );

        tracing::info!(pools = count, "restored pool depths");
        Ok(())
    }

    pub fn pending_blocks(&self) -> usize {
        self.blocks.len()
    }

    pub fn depth_of(&self, pool: &str) -> Depth {
        self.tracker.get(pool)
    }

    /// Decode a block into the buffer, flushing if the batch is full.
    pub async fn add(&mut self, block: &Block) -> Result<(), WriteError> {
        let timestamp = block.timestamp.to_i64();

        // Begin-block, then transactions, then end-block: the order events actually executed in,
        // which is the order the event ids have to reflect.
        for (index, event) in block.begin_block_events() {
            let id = EventId::begin_block(block.height);
            let id = EventId {
                event_index: index as i64 + 1,
                ..id
            };
            self.push(event, id, timestamp);
        }

        for (tx_index, tx) in block.successful_txs() {
            for (event_index, event) in tx.events.iter().enumerate() {
                let id =
                    EventId::tx_event(block.height, tx_index as i64 + 1, event_index as i64 + 1);
                self.push(event, id, timestamp);
            }
        }

        for (index, event) in block.end_block_events() {
            let id = EventId::end_block(block.height, index as i64 + 1);
            self.push(event, id, timestamp);
        }

        // One depth row per pool that moved during this block, not per event.
        for (pool, depth) in self.tracker.take_changed() {
            self.depths.push(PendingDepth {
                pool,
                depth,
                block_timestamp: timestamp,
            });
        }

        let hash = hex_to_bytes(&block.hash);
        self.blocks.push((block.height, timestamp, hash));

        if self.blocks.len() >= self.batch_size {
            self.flush().await?;
        }
        Ok(())
    }

    fn push(&mut self, event: &tendermint::abci::Event, id: EventId, timestamp: i64) {
        match decode(event) {
            Ok(Decoded::Event(record)) => {
                self.tracker.apply(&record);
                self.events.push(Pending {
                    event_id: id.to_i64(),
                    block_timestamp: timestamp,
                    record: *record,
                });
            }
            // Several rows from one event; they share its id, the way the per-pool reward split
            // does.
            Ok(Decoded::Events(records)) => {
                for record in records {
                    self.tracker.apply(&record);
                    self.events.push(Pending {
                        event_id: id.to_i64(),
                        block_timestamp: timestamp,
                        record,
                    });
                }
            }
            Ok(Decoded::Ignored) => {}
            Ok(Decoded::Unknown) => {
                if self.unknown_seen.insert(event.kind.clone()) {
                    tracing::warn!(
                        event_type = %event.kind,
                        height = id.height,
                        "unknown event type, not recorded (logged once per type)"
                    );
                }
            }
            Err(e) => {
                // A single undecodable event must not stop the chain. It is logged and skipped;
                // the alternative is a daemon that halts on the first malformed attribute
                // THORNode ever emits.
                tracing::warn!(
                    event_type = %event.kind,
                    height = id.height,
                    error = %e,
                    "failed to decode event, skipped"
                );
            }
        }
    }

    /// Write everything buffered, in one transaction.
    pub async fn flush(&mut self) -> Result<(), WriteError> {
        if self.blocks.is_empty() {
            return Ok(());
        }

        let mut tx = self.db.pool().begin().await?;

        write_depths(&mut tx, &self.depths).await?;
        write_events(&mut tx, &self.events).await?;

        // Last, and in the same transaction: the cursor must never move past data that is not
        // committed alongside it.
        write_blocks(&mut tx, &self.blocks).await?;

        tx.commit().await?;

        tracing::debug!(
            blocks = self.blocks.len(),
            events = self.events.len(),
            depths = self.depths.len(),
            "committed batch"
        );

        self.blocks.clear();
        self.events.clear();
        self.depths.clear();
        Ok(())
    }
}

async fn write_blocks(
    tx: &mut Transaction<'_, Postgres>,
    blocks: &[(i64, i64, Vec<u8>)],
) -> Result<(), sqlx::Error> {
    if blocks.is_empty() {
        return Ok(());
    }
    let mut qb = QueryBuilder::new("INSERT INTO block_log (height, timestamp, hash) ");
    qb.push_values(blocks, |mut b, (height, timestamp, hash)| {
        b.push_bind(height).push_bind(timestamp).push_bind(hash);
    });
    // Re-fetching a block after a crash mid-batch is normal, so a repeat is not an error.
    qb.push(" ON CONFLICT (height) DO NOTHING");
    qb.build().execute(&mut **tx).await?;
    Ok(())
}

async fn write_depths(
    tx: &mut Transaction<'_, Postgres>,
    depths: &[PendingDepth],
) -> Result<(), sqlx::Error> {
    if depths.is_empty() {
        return Ok(());
    }
    let mut qb = QueryBuilder::new(
        "INSERT INTO block_pool_depths (pool, asset_e8, rune_e8, synth_e8, units, block_timestamp) ",
    );
    qb.push_values(depths, |mut b, d| {
        b.push_bind(&d.pool)
            .push_bind(d.depth.asset_e8)
            .push_bind(d.depth.rune_e8)
            .push_bind(d.depth.synth_e8)
            .push_bind(d.depth.units)
            .push_bind(d.block_timestamp);
    });
    qb.build().execute(&mut **tx).await?;
    Ok(())
}

/// Write the buffered events, one multi-row INSERT per table.
///
/// Grouping by table rather than inserting per event is the difference between a few dozen
/// statements per batch and a few thousand.
async fn write_events(
    tx: &mut Transaction<'_, Postgres>,
    events: &[Pending],
) -> Result<(), sqlx::Error> {
    macro_rules! insert {
        ($table:literal, ($($col:literal),+), $variant:path, |$b:ident, $v:ident| $bind:block) => {{
            let rows: Vec<_> = events
                .iter()
                .filter_map(|p| match &p.record {
                    $variant(v) => Some((p.event_id, p.block_timestamp, v)),
                    _ => None,
                })
                .collect();
            if !rows.is_empty() {
                let cols = [$($col),+].join(", ");
                let mut qb = QueryBuilder::new(format!(
                    "INSERT INTO {} ({}, event_id, block_timestamp) ", $table, cols
                ));
                qb.push_values(rows, |mut $b, (event_id, block_timestamp, $v)| {
                    $bind
                    $b.push_bind(event_id).push_bind(block_timestamp);
                });
                qb.build().execute(&mut **tx).await?;
            }
        }};
    }

    insert!(
        "swap_events",
        (
            "tx",
            "chain",
            "from_addr",
            "to_addr",
            "from_asset",
            "from_e8",
            "to_asset",
            "to_e8",
            "memo",
            "pool",
            "to_e8_min",
            "swap_slip_bp",
            "liq_fee_e8",
            "liq_fee_in_rune_e8",
            "_direction",
            "_streaming",
            "streaming_count",
            "streaming_quantity"
        ),
        Recorded::Swap,
        |b, v| {
            b.push_bind(&v.tx)
                .push_bind(&v.chain)
                .push_bind(&v.from_addr)
                .push_bind(&v.to_addr)
                .push_bind(&v.from_asset)
                .push_bind(v.from_e8)
                .push_bind(&v.to_asset)
                .push_bind(v.to_e8)
                .push_bind(&v.memo)
                .push_bind(&v.pool)
                .push_bind(v.to_e8_min)
                .push_bind(v.swap_slip_bp)
                .push_bind(v.liq_fee_e8)
                .push_bind(v.liq_fee_in_rune_e8)
                .push_bind(v.direction)
                .push_bind(v.streaming)
                .push_bind(v.streaming_count)
                .push_bind(v.streaming_quantity);
        }
    );

    insert!(
        "stake_events",
        (
            "pool",
            "asset_tx",
            "asset_chain",
            "asset_addr",
            "asset_e8",
            "stake_units",
            "rune_tx",
            "rune_addr",
            "rune_e8",
            "_asset_in_rune_e8",
            "memo"
        ),
        Recorded::Stake,
        |b, v| {
            b.push_bind(&v.pool)
                .push_bind(&v.asset_tx)
                .push_bind(&v.asset_chain)
                .push_bind(&v.asset_addr)
                .push_bind(v.asset_e8)
                .push_bind(v.stake_units)
                .push_bind(&v.rune_tx)
                .push_bind(&v.rune_addr)
                .push_bind(v.rune_e8)
                .push_bind(v.asset_in_rune_e8)
                .push_bind(&v.memo);
        }
    );

    insert!(
        "withdraw_events",
        (
            "tx",
            "chain",
            "from_addr",
            "to_addr",
            "asset",
            "asset_e8",
            "emit_asset_e8",
            "emit_rune_e8",
            "memo",
            "pool",
            "stake_units",
            "basis_points",
            "asymmetry",
            "imp_loss_protection_e8",
            "_emit_asset_in_rune_e8"
        ),
        Recorded::Withdraw,
        |b, v| {
            b.push_bind(&v.tx)
                .push_bind(&v.chain)
                .push_bind(&v.from_addr)
                .push_bind(&v.to_addr)
                .push_bind(&v.asset)
                .push_bind(v.asset_e8)
                .push_bind(v.emit_asset_e8)
                .push_bind(v.emit_rune_e8)
                .push_bind(&v.memo)
                .push_bind(&v.pool)
                .push_bind(v.stake_units)
                .push_bind(v.basis_points)
                .push_bind(v.asymmetry)
                .push_bind(v.imp_loss_protection_e8)
                .push_bind(v.emit_asset_in_rune_e8);
        }
    );

    insert!(
        "pool_events",
        ("asset", "status"),
        Recorded::Pool,
        |b, v| {
            b.push_bind(&v.asset).push_bind(&v.status);
        }
    );

    insert!(
        "fee_events",
        ("tx", "asset", "asset_e8", "pool_deduct"),
        Recorded::Fee,
        |b, v| {
            b.push_bind(&v.tx)
                .push_bind(&v.asset)
                .push_bind(v.asset_e8)
                .push_bind(v.pool_deduct);
        }
    );

    insert!(
        "gas_events",
        ("asset", "asset_e8", "rune_e8", "tx_count"),
        Recorded::Gas,
        |b, v| {
            b.push_bind(&v.asset)
                .push_bind(v.asset_e8)
                .push_bind(v.rune_e8)
                .push_bind(v.tx_count);
        }
    );

    insert!("rewards_events", ("bond_e8"), Recorded::Rewards, |b, v| {
        b.push_bind(v.bond_e8);
    });

    // The per-pool split lives in its own table, so it needs a hand-rolled pass rather than the
    // one-row-per-event shape the macro assumes.
    let reward_entries: Vec<_> = events
        .iter()
        .filter_map(|p| match &p.record {
            Recorded::Rewards(r) => Some((p.event_id, p.block_timestamp, r)),
            _ => None,
        })
        .flat_map(|(id, ts, r)| {
            r.per_pool
                .iter()
                .map(move |(pool, amount)| (id, ts, pool, *amount))
        })
        .collect();
    if !reward_entries.is_empty() {
        let mut qb = QueryBuilder::new(
            "INSERT INTO rewards_event_entries (pool, rune_e8, saver_e8, event_id, block_timestamp) ",
        );
        qb.push_values(reward_entries, |mut b, (id, ts, pool, amount)| {
            b.push_bind(pool)
                .push_bind(amount)
                .push_bind(0i64)
                .push_bind(id)
                .push_bind(ts);
        });
        qb.build().execute(&mut **tx).await?;
    }

    insert!(
        "outbound_events",
        (
            "tx",
            "chain",
            "from_addr",
            "to_addr",
            "asset",
            "asset_e8",
            "memo",
            "in_tx"
        ),
        Recorded::Outbound,
        |b, v| {
            b.push_bind(&v.tx)
                .push_bind(&v.chain)
                .push_bind(&v.from_addr)
                .push_bind(&v.to_addr)
                .push_bind(&v.asset)
                .push_bind(v.asset_e8)
                .push_bind(&v.memo)
                .push_bind(&v.in_tx);
        }
    );

    insert!(
        "refund_events",
        (
            "tx",
            "chain",
            "from_addr",
            "to_addr",
            "asset",
            "asset_e8",
            "asset_2nd",
            "asset_2nd_e8",
            "memo",
            "code",
            "reason"
        ),
        Recorded::Refund,
        |b, v| {
            b.push_bind(&v.tx)
                .push_bind(&v.chain)
                .push_bind(&v.from_addr)
                .push_bind(&v.to_addr)
                .push_bind(&v.asset)
                .push_bind(v.asset_e8)
                .push_bind(&v.asset_2nd)
                .push_bind(v.asset_2nd_e8)
                .push_bind(&v.memo)
                .push_bind(v.code)
                .push_bind(&v.reason);
        }
    );

    insert!(
        "add_events",
        (
            "tx",
            "chain",
            "from_addr",
            "to_addr",
            "asset",
            "asset_e8",
            "memo",
            "rune_e8",
            "pool"
        ),
        Recorded::Add,
        |b, v| {
            b.push_bind(&v.tx)
                .push_bind(&v.chain)
                .push_bind(&v.from_addr)
                .push_bind(&v.to_addr)
                .push_bind(&v.asset)
                .push_bind(v.asset_e8)
                .push_bind(&v.memo)
                .push_bind(v.rune_e8)
                .push_bind(&v.pool);
        }
    );

    insert!(
        "transfer_events",
        ("from_addr", "to_addr", "asset", "amount_e8"),
        Recorded::Transfer,
        |b, v| {
            b.push_bind(&v.from_addr)
                .push_bind(&v.to_addr)
                .push_bind(&v.asset)
                .push_bind(v.amount_e8);
        }
    );

    insert!(
        "bond_events",
        (
            "tx",
            "chain",
            "from_addr",
            "to_addr",
            "asset",
            "asset_e8",
            "memo",
            "bond_type",
            "e8"
        ),
        Recorded::Bond,
        |b, v| {
            b.push_bind(&v.tx)
                .push_bind(&v.chain)
                .push_bind(&v.from_addr)
                .push_bind(&v.to_addr)
                .push_bind(&v.asset)
                .push_bind(v.asset_e8)
                .push_bind(&v.memo)
                .push_bind(&v.bond_type)
                .push_bind(v.e8);
        }
    );

    insert!(
        "pending_liquidity_events",
        (
            "pool",
            "asset_tx",
            "asset_chain",
            "asset_addr",
            "asset_e8",
            "rune_tx",
            "rune_addr",
            "rune_e8",
            "pending_type"
        ),
        Recorded::PendingLiquidity,
        |b, v| {
            b.push_bind(&v.pool)
                .push_bind(&v.asset_tx)
                .push_bind(&v.asset_chain)
                .push_bind(&v.asset_addr)
                .push_bind(v.asset_e8)
                .push_bind(&v.rune_tx)
                .push_bind(&v.rune_addr)
                .push_bind(v.rune_e8)
                .push_bind(&v.pending_type);
        }
    );

    insert!(
        "errata_events",
        ("in_tx", "asset", "asset_e8", "rune_e8"),
        Recorded::Errata,
        |b, v| {
            b.push_bind(&v.in_tx)
                .push_bind(&v.asset)
                .push_bind(v.asset_e8)
                .push_bind(v.rune_e8);
        }
    );

    insert!(
        "set_mimir_events",
        ("key", "value"),
        Recorded::SetMimir,
        |b, v| {
            b.push_bind(&v.key).push_bind(&v.value);
        }
    );

    insert!(
        "mint_burn_events",
        ("asset", "asset_e8", "supply", "reason"),
        Recorded::MintBurn,
        |b, v| {
            b.push_bind(&v.asset)
                .push_bind(v.asset_e8)
                .push_bind(&v.supply)
                .push_bind(&v.reason);
        }
    );

    insert!(
        "pool_balance_change_events",
        (
            "asset",
            "rune_amt",
            "rune_add",
            "asset_amt",
            "asset_add",
            "reason"
        ),
        Recorded::PoolBalanceChange,
        |b, v| {
            b.push_bind(&v.asset)
                .push_bind(v.rune_amt)
                .push_bind(v.rune_add)
                .push_bind(v.asset_amt)
                .push_bind(v.asset_add)
                .push_bind(&v.reason);
        }
    );

    insert!(
        "slash_events",
        ("pool", "asset", "asset_e8"),
        Recorded::Slash,
        |b, v| {
            b.push_bind(&v.pool)
                .push_bind(&v.asset)
                .push_bind(v.asset_e8);
        }
    );

    insert!(
        "switch_events",
        (
            "tx",
            "from_addr",
            "to_addr",
            "burn_asset",
            "mint_asset",
            "burn_e8",
            "mint_e8"
        ),
        Recorded::Switch,
        |b, v| {
            b.push_bind(&v.tx)
                .push_bind(&v.from_addr)
                .push_bind(&v.to_addr)
                .push_bind(&v.burn_asset)
                .push_bind(&v.mint_asset)
                .push_bind(v.burn_e8)
                .push_bind(v.mint_e8);
        }
    );

    insert!(
        "active_vault_events",
        ("add_asgard_addr"),
        Recorded::ActiveVault,
        |b, v| {
            b.push_bind(&v.add_asgard_addr);
        }
    );

    insert!(
        "inactive_vault_events",
        ("add_asgard_addr"),
        Recorded::InactiveVault,
        |b, v| {
            b.push_bind(&v.add_asgard_addr);
        }
    );

    insert!(
        "update_node_account_status_events",
        ("node_addr", "former", "current"),
        Recorded::NodeStatus,
        |b, v| {
            b.push_bind(&v.node_addr)
                .push_bind(&v.former)
                .push_bind(&v.current);
        }
    );

    Ok(())
}

/// Block hashes are uppercase hex on the wire and `BYTEA` in the database.
fn hex_to_bytes(hex: &str) -> Vec<u8> {
    (0..hex.len() / 2)
        .filter_map(|i| u8::from_str_radix(hex.get(i * 2..i * 2 + 2)?, 16).ok())
        .collect()
}

/// Nanosecond timestamp of a block, for callers that only have the block.
pub fn block_timestamp(block: &Block) -> Nano {
    block.timestamp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_convert_to_bytes() {
        assert_eq!(hex_to_bytes("00FF10"), vec![0x00, 0xFF, 0x10]);
        assert_eq!(hex_to_bytes(""), Vec::<u8>::new());
    }

    #[test]
    fn a_full_block_hash_is_32_bytes() {
        let hash = "BB4D0216D44888717D4D9865D50754519AC6CCCB26BB663CB733C6D950AB777F";
        assert_eq!(hex_to_bytes(hash).len(), 32);
    }

    #[test]
    fn malformed_hex_yields_what_it_can_rather_than_panicking() {
        // Truncated or non-hex input must not take the writer down mid-batch.
        assert_eq!(hex_to_bytes("0"), Vec::<u8>::new());
        assert_eq!(hex_to_bytes("ZZ"), Vec::<u8>::new());
    }
}
