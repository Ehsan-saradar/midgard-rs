//! Pool depth tracking.
//!
//! `block_pool_depths` is sparse: a row exists only where a pool's depth actually changed. Pools
//! change on a small fraction of blocks, and a row per pool per block would be a hundred times
//! the data for the same information — reading a depth means "latest row at or before T" either
//! way.
//!
//! Keeping it sparse means holding every pool's current depth in memory and diffing after each
//! block. That state is derived purely from the events already applied, so a restart rebuilds it
//! by reading the last row per pool, and a re-sync reproduces it exactly.
//!
//! Depths move on swaps (one side in, other side out, minus fees), deposits and withdrawals,
//! donations, errata corrections, rewards, slashes and explicit balance-change events. Getting
//! any one of those wrong shows up immediately as a divergence from THORNode's own pool figures,
//! which is what `cmd/statechecks` compares upstream.

use std::collections::HashMap;

use midgard_core::asset::is_rune;

use crate::events::{Recorded, Swap};

/// One pool's balances.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Depth {
    pub asset_e8: i64,
    pub rune_e8: i64,
    pub synth_e8: i64,
    pub units: i64,
}

/// Current depth of every pool, plus the diff since the last block.
#[derive(Debug, Default)]
pub struct DepthTracker {
    current: HashMap<String, Depth>,
    /// Pools touched since the last [`Self::take_changed`].
    dirty: Vec<String>,
}

impl DepthTracker {
    pub fn new() -> DepthTracker {
        DepthTracker::default()
    }

    /// Seed from the database after a restart.
    pub fn load(&mut self, depths: impl IntoIterator<Item = (String, Depth)>) {
        self.current = depths.into_iter().collect();
        self.dirty.clear();
    }

    pub fn get(&self, pool: &str) -> Depth {
        self.current.get(pool).copied().unwrap_or_default()
    }

    pub fn pools(&self) -> impl Iterator<Item = (&String, &Depth)> {
        self.current.iter()
    }

    fn touch(&mut self, pool: &str) -> &mut Depth {
        if !self.current.contains_key(pool) {
            self.current.insert(pool.to_string(), Depth::default());
        }
        if !self.dirty.iter().any(|p| p == pool) {
            self.dirty.push(pool.to_string());
        }
        self.current.get_mut(pool).expect("just inserted")
    }

    /// Apply one decoded event.
    pub fn apply(&mut self, event: &Recorded) {
        match event {
            Recorded::Swap(s) => self.apply_swap(s),

            Recorded::Stake(s) => {
                let d = self.touch(&s.pool);
                d.asset_e8 += s.asset_e8;
                d.rune_e8 += s.rune_e8;
                d.units += s.stake_units;
            }

            Recorded::Withdraw(w) => {
                let d = self.touch(&w.pool);
                d.asset_e8 -= w.emit_asset_e8;
                d.rune_e8 -= w.emit_rune_e8;
                d.units -= w.stake_units;
                // Impermanent loss protection is paid into the pool from the reserve before the
                // withdrawal is settled, so it is an addition, not a deduction.
                d.rune_e8 += w.imp_loss_protection_e8;
            }

            Recorded::Add(a) => {
                let d = self.touch(&a.pool);
                d.rune_e8 += a.rune_e8;
                d.asset_e8 += a.asset_e8;
            }

            Recorded::Errata(e) => {
                let d = self.touch(&e.asset);
                d.asset_e8 += e.asset_e8;
                d.rune_e8 += e.rune_e8;
            }

            Recorded::PoolBalanceChange(c) => {
                let d = self.touch(&c.asset);
                d.rune_e8 += if c.rune_add { c.rune_amt } else { -c.rune_amt };
                d.asset_e8 += if c.asset_add {
                    c.asset_amt
                } else {
                    -c.asset_amt
                };
            }

            Recorded::Rewards(r) => {
                for (pool, amount) in &r.per_pool {
                    self.touch(pool).rune_e8 += *amount;
                }
            }

            Recorded::Slash(s) => {
                let d = self.touch(&s.pool);
                if is_rune(&s.asset) {
                    d.rune_e8 += s.asset_e8;
                } else {
                    d.asset_e8 += s.asset_e8;
                }
            }

            // Fees on a swap are already reflected in the emitted amount; what `pool_deduct`
            // records is the RUNE the network took out of the pool to pay the outbound gas.
            Recorded::Fee(f) => {
                if f.pool_deduct != 0 {
                    let pool = pool_of_asset(&f.asset);
                    self.touch(&pool).rune_e8 -= f.pool_deduct;
                }
            }

            // Gas spent by the network is reimbursed to the pool in RUNE and taken in asset.
            Recorded::Gas(g) => {
                let pool = pool_of_asset(&g.asset);
                let d = self.touch(&pool);
                d.asset_e8 -= g.asset_e8;
                d.rune_e8 += g.rune_e8;
            }

            // Everything else leaves depths alone.
            _ => {}
        }
    }

    fn apply_swap(&mut self, s: &Swap) {
        let d = self.touch(&s.pool);
        let from_rune = is_rune(&s.from_asset);
        let from_synth = matches!(
            midgard_core::asset::CoinType::of(&s.from_asset),
            midgard_core::asset::CoinType::Synth
        );
        let to_synth = matches!(
            midgard_core::asset::CoinType::of(&s.to_asset),
            midgard_core::asset::CoinType::Synth
        );

        // Minting a synth adds RUNE to the pool and increases synth supply without touching the
        // asset side; redeeming does the reverse. Treating a synth swap as an ordinary one would
        // drain the asset balance of every pool with savers in it.
        if to_synth {
            d.rune_e8 += s.from_e8;
            d.synth_e8 += s.to_e8;
        } else if from_synth {
            d.synth_e8 -= s.from_e8;
            d.rune_e8 -= s.to_e8;
        } else if from_rune {
            d.rune_e8 += s.from_e8;
            d.asset_e8 -= s.to_e8;
        } else {
            d.asset_e8 += s.from_e8;
            d.rune_e8 -= s.to_e8;
        }
    }

    /// Pools whose depth changed, with their new values. Clears the dirty set.
    ///
    /// A pool touched by an event whose net effect was zero is still reported: the event
    /// happened, and suppressing the row would make the change invisible in the history even
    /// though the pool was involved.
    pub fn take_changed(&mut self) -> Vec<(String, Depth)> {
        let dirty = std::mem::take(&mut self.dirty);
        dirty
            .into_iter()
            .map(|pool| {
                let depth = self.get(&pool);
                (pool, depth)
            })
            .collect()
    }

    pub fn has_changes(&self) -> bool {
        !self.dirty.is_empty()
    }
}

/// The pool an asset belongs to.
///
/// Synths and trade assets are claims on the underlying L1 pool, so `BTC/BTC` and `BTC~BTC` both
/// account against `BTC.BTC`.
fn pool_of_asset(asset: &str) -> String {
    midgard_core::asset::Asset::parse(asset)
        .to_string()
        .replacen(['/', '~'], ".", 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{Add, Errata, Fee, PoolBalanceChange, Rewards, Stake, Withdraw};

    fn swap(pool: &str, from: &str, from_e8: i64, to: &str, to_e8: i64) -> Recorded {
        Recorded::Swap(Swap {
            pool: pool.to_string(),
            from_asset: from.to_string(),
            from_e8,
            to_asset: to.to_string(),
            to_e8,
            ..Swap::default()
        })
    }

    #[test]
    fn a_rune_to_asset_swap_moves_both_sides() {
        let mut t = DepthTracker::new();
        t.apply(&swap("BTC.BTC", "THOR.RUNE", 1_000, "BTC.BTC", 10));

        let d = t.get("BTC.BTC");
        assert_eq!(d.rune_e8, 1_000, "rune came in");
        assert_eq!(d.asset_e8, -10, "asset went out");
    }

    #[test]
    fn an_asset_to_rune_swap_moves_both_sides() {
        let mut t = DepthTracker::new();
        t.apply(&swap("BTC.BTC", "BTC.BTC", 10, "THOR.RUNE", 1_000));

        let d = t.get("BTC.BTC");
        assert_eq!(d.asset_e8, 10);
        assert_eq!(d.rune_e8, -1_000);
    }

    #[test]
    fn minting_a_synth_leaves_the_asset_side_alone() {
        // The bug this guards: treating a synth mint as an ordinary swap drains the asset
        // balance of every pool that has savers in it.
        let mut t = DepthTracker::new();
        t.apply(&swap("BTC.BTC", "THOR.RUNE", 1_000, "BTC/BTC", 10));

        let d = t.get("BTC.BTC");
        assert_eq!(d.rune_e8, 1_000);
        assert_eq!(d.synth_e8, 10);
        assert_eq!(d.asset_e8, 0, "asset side must not move on a synth mint");
    }

    #[test]
    fn redeeming_a_synth_is_the_mirror_image() {
        let mut t = DepthTracker::new();
        t.apply(&swap("BTC.BTC", "BTC/BTC", 10, "THOR.RUNE", 1_000));

        let d = t.get("BTC.BTC");
        assert_eq!(d.synth_e8, -10);
        assert_eq!(d.rune_e8, -1_000);
        assert_eq!(d.asset_e8, 0);
    }

    #[test]
    fn deposits_and_withdrawals_move_units_too() {
        let mut t = DepthTracker::new();
        t.apply(&Recorded::Stake(Stake {
            pool: "BTC.BTC".to_string(),
            asset_e8: 100,
            rune_e8: 5_000,
            stake_units: 42,
            ..Stake::default()
        }));
        assert_eq!(
            t.get("BTC.BTC"),
            Depth {
                asset_e8: 100,
                rune_e8: 5_000,
                synth_e8: 0,
                units: 42
            }
        );

        t.apply(&Recorded::Withdraw(Withdraw {
            pool: "BTC.BTC".to_string(),
            emit_asset_e8: 50,
            emit_rune_e8: 2_500,
            stake_units: 21,
            ..Withdraw::default()
        }));
        assert_eq!(
            t.get("BTC.BTC"),
            Depth {
                asset_e8: 50,
                rune_e8: 2_500,
                synth_e8: 0,
                units: 21
            }
        );
    }

    #[test]
    fn impermanent_loss_protection_is_paid_into_the_pool() {
        let mut t = DepthTracker::new();
        t.apply(&Recorded::Withdraw(Withdraw {
            pool: "BTC.BTC".to_string(),
            emit_rune_e8: 100,
            imp_loss_protection_e8: 30,
            ..Withdraw::default()
        }));
        // 30 comes in from the reserve, 100 goes out to the member.
        assert_eq!(t.get("BTC.BTC").rune_e8, -70);
    }

    #[test]
    fn donations_add_without_issuing_units() {
        let mut t = DepthTracker::new();
        t.apply(&Recorded::Add(Add {
            pool: "BTC.BTC".to_string(),
            rune_e8: 1_000,
            asset_e8: 10,
            ..Add::default()
        }));
        let d = t.get("BTC.BTC");
        assert_eq!((d.rune_e8, d.asset_e8, d.units), (1_000, 10, 0));
    }

    #[test]
    fn negative_pool_rewards_reduce_depth() {
        let mut t = DepthTracker::new();
        t.apply(&Recorded::Rewards(Rewards {
            bond_e8: 999,
            per_pool: vec![("BTC.BTC".to_string(), 500), ("ETH.ETH".to_string(), -250)],
        }));
        assert_eq!(t.get("BTC.BTC").rune_e8, 500);
        assert_eq!(t.get("ETH.ETH").rune_e8, -250);
    }

    #[test]
    fn balance_change_events_respect_their_direction_flags() {
        let mut t = DepthTracker::new();
        t.apply(&Recorded::PoolBalanceChange(PoolBalanceChange {
            asset: "BTC.BTC".to_string(),
            rune_amt: 100,
            rune_add: false,
            asset_amt: 5,
            asset_add: true,
            reason: String::new(),
        }));
        let d = t.get("BTC.BTC");
        assert_eq!(d.rune_e8, -100);
        assert_eq!(d.asset_e8, 5);
    }

    #[test]
    fn errata_amounts_are_already_signed() {
        let mut t = DepthTracker::new();
        t.apply(&Recorded::Errata(Errata {
            in_tx: String::new(),
            asset: "BTC.BTC".to_string(),
            asset_e8: -100,
            rune_e8: 50,
        }));
        let d = t.get("BTC.BTC");
        assert_eq!(d.asset_e8, -100);
        assert_eq!(d.rune_e8, 50);
    }

    #[test]
    fn fees_are_charged_to_the_underlying_pool() {
        let mut t = DepthTracker::new();
        // A fee denominated in a synth still comes out of the L1 pool.
        t.apply(&Recorded::Fee(Fee {
            tx: String::new(),
            asset: "BTC/BTC".to_string(),
            asset_e8: 1,
            pool_deduct: 700,
        }));
        assert_eq!(t.get("BTC.BTC").rune_e8, -700);
    }

    #[test]
    fn a_zero_pool_deduct_does_not_touch_the_pool() {
        let mut t = DepthTracker::new();
        t.apply(&Recorded::Fee(Fee {
            asset: "BTC.BTC".to_string(),
            pool_deduct: 0,
            ..Fee::default()
        }));
        assert!(!t.has_changes());
    }

    #[test]
    fn synth_and_trade_assets_map_onto_the_l1_pool() {
        assert_eq!(pool_of_asset("BTC/BTC"), "BTC.BTC");
        assert_eq!(pool_of_asset("BTC~BTC"), "BTC.BTC");
        assert_eq!(pool_of_asset("BTC.BTC"), "BTC.BTC");
        assert_eq!(
            pool_of_asset("ETH/USDT-0XDAC17"),
            "ETH.USDT-0XDAC17",
            "only the first separator is rewritten"
        );
    }

    #[test]
    fn only_touched_pools_are_reported() {
        let mut t = DepthTracker::new();
        t.apply(&swap("BTC.BTC", "THOR.RUNE", 1, "BTC.BTC", 1));
        t.apply(&swap("ETH.ETH", "THOR.RUNE", 1, "ETH.ETH", 1));

        let changed = t.take_changed();
        assert_eq!(changed.len(), 2);

        // Nothing since, so nothing to write.
        assert!(t.take_changed().is_empty());
        assert!(!t.has_changes());
    }

    #[test]
    fn a_pool_touched_twice_is_reported_once_with_the_final_value() {
        let mut t = DepthTracker::new();
        t.apply(&swap("BTC.BTC", "THOR.RUNE", 100, "BTC.BTC", 1));
        t.apply(&swap("BTC.BTC", "THOR.RUNE", 200, "BTC.BTC", 2));

        let changed = t.take_changed();
        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0].1.rune_e8, 300);
    }

    #[test]
    fn state_survives_a_reload() {
        let mut t = DepthTracker::new();
        t.load([(
            "BTC.BTC".to_string(),
            Depth {
                asset_e8: 5,
                rune_e8: 6,
                synth_e8: 7,
                units: 8,
            },
        )]);
        assert_eq!(t.get("BTC.BTC").rune_e8, 6);
        // Loading is not a change to write back out.
        assert!(!t.has_changes());

        t.apply(&swap("BTC.BTC", "THOR.RUNE", 10, "BTC.BTC", 1));
        assert_eq!(t.get("BTC.BTC").rune_e8, 16);
    }
}
