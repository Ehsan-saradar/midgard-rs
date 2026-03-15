//! The list of event tables.
//!
//! Anything that needs to touch "every event table" — rolling back a fork, counting rows for a
//! health check — needs this list, and deriving it from `information_schema` at runtime would
//! also pick up `block_log`, `constants` and `rune_price`, which are not per-event and must not
//! be treated as such.
//!
//! Keep in step with `sql/ddl.sql`. [`tests::every_event_table_is_listed`] fails if they drift.

/// Tables with an `event_id` and a `block_timestamp`, one row per decoded ABCI event.
pub const EVENT_TABLES: &[&str] = &[
    "active_vault_events",
    "add_events",
    "bond_events",
    "errata_events",
    "fee_events",
    "gas_events",
    "inactive_vault_events",
    "mint_burn_events",
    "outbound_events",
    "pending_liquidity_events",
    "pool_balance_change_events",
    "pool_events",
    "refund_events",
    "rewards_event_entries",
    "rewards_events",
    "set_mimir_events",
    "slash_events",
    "stake_events",
    "swap_events",
    "switch_events",
    "transfer_events",
    "update_node_account_status_events",
    "withdraw_events",
];

/// Time-series tables that Midgard derives rather than receives, and so have no `event_id`.
/// They are still keyed by `block_timestamp` and still have to be rolled back on a fork.
pub const DERIVED_TABLES: &[&str] = &["block_pool_depths", "rune_price"];

/// Everything that is per-block and therefore needs truncating when blocks are rolled back.
pub fn rollback_tables() -> impl Iterator<Item = &'static str> {
    EVENT_TABLES.iter().chain(DERIVED_TABLES.iter()).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    const DDL: &str = include_str!("../sql/ddl.sql");

    /// Every `CREATE TABLE` in the DDL, paired with whether its body declares `event_id`.
    ///
    /// Keying off the column rather than an `_events` name suffix is deliberate:
    /// `rewards_event_entries` is an event table whose name does not end in `_events`, and a
    /// name-based check silently skipped it.
    fn declared_tables() -> Vec<(&'static str, bool)> {
        let mut out = Vec::new();
        for block in DDL.split("CREATE TABLE ").skip(1) {
            let name = block
                .split_whitespace()
                .next()
                .expect("a name follows CREATE TABLE");
            let body = block.split(");").next().unwrap_or("");
            out.push((name, body.contains("event_id")));
        }
        out
    }

    #[test]
    fn every_event_table_is_listed() {
        for (name, has_event_id) in declared_tables() {
            assert_eq!(
                EVENT_TABLES.contains(&name),
                has_event_id,
                "{name}: has event_id = {has_event_id}, listed in EVENT_TABLES = {}",
                EVENT_TABLES.contains(&name)
            );
        }

        let declared: Vec<&str> = declared_tables().into_iter().map(|(n, _)| n).collect();
        for name in EVENT_TABLES {
            assert!(
                declared.contains(name),
                "{name} is in EVENT_TABLES but not created by ddl.sql"
            );
        }
    }

    #[test]
    fn derived_tables_exist_too() {
        for name in DERIVED_TABLES {
            assert!(
                DDL.contains(&format!("CREATE TABLE {name} ")),
                "{name} is not created by ddl.sql"
            );
        }
    }

    #[test]
    fn the_list_is_sorted_so_diffs_stay_readable() {
        let mut sorted = EVENT_TABLES.to_vec();
        sorted.sort_unstable();
        assert_eq!(EVENT_TABLES, sorted.as_slice());
    }

    #[test]
    fn rollback_covers_both_kinds() {
        let all: Vec<&str> = rollback_tables().collect();
        assert_eq!(all.len(), EVENT_TABLES.len() + DERIVED_TABLES.len());
        assert!(all.contains(&"swap_events"));
        assert!(all.contains(&"block_pool_depths"));
    }
}
