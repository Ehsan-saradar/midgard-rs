//! Block types.
//!
//! The wire structs come from `tendermint-rpc` rather than being hand-written here. Tracking
//! CometBFT's schema across versions is real, ongoing work and there is a maintained crate doing
//! it; the parts Midgard reads (`finalize_block_events`, `txs_results`, the header) are exactly
//! the parts that would break silently if we got them subtly wrong.
//!
//! What we do *not* take from that crate is its HTTP client, which has no JSON-RPC batching.
//! Catching up from genesis is two RPC calls times twenty-seven million blocks, and batching is
//! the difference between that being feasible and not. See [`crate::rpc`]. The response types
//! deserialize perfectly well from our own transport, so this is a hybrid rather than a fork.
//!
//! `tendermint-rpc` is pulled in with `default-features = false`: we want `endpoint::*`, not the
//! client, and skipping it takes the dependency tree from 247 crates to 164.

use midgard_core::Nano;
use tendermint::abci::{Event, EventAttribute};

pub use tendermint_rpc::endpoint::block::Response as BlockResponse;
pub use tendermint_rpc::endpoint::block_results::Response as BlockResults;
pub use tendermint_rpc::endpoint::status::Response as Status;

/// A block, assembled from the two RPC calls it takes to describe one.
#[derive(Debug, Clone)]
pub struct Block {
    pub height: i64,
    pub timestamp: Nano,
    /// Uppercase hex, matching how the RPC reports it and how `block_log.hash` stores it.
    pub hash: String,
    pub chain_id: String,
    pub results: BlockResults,
    /// Raw transaction bodies, positionally aligned with `results.txs_results`.
    pub txs: Vec<Vec<u8>>,
}

impl Block {
    /// Begin-block events: tagged `mode=BeginBlock`, or carrying no `mode` at all.
    ///
    /// CometBFT 0.38 merged the begin and end phases into one `finalize_block_events` list and
    /// distinguishes them with a `mode` attribute. The no-mode case is not hypothetical — several
    /// THORChain event types are emitted without one, and treating those as end-block would
    /// reorder them relative to the transactions in the same block.
    ///
    /// The `usize` is the position in the original list, because that is what the event id is
    /// built from; numbering within the filtered subset would collide across the two phases.
    pub fn begin_block_events(&self) -> impl Iterator<Item = (usize, &Event)> {
        self.results
            .finalize_block_events
            .iter()
            .enumerate()
            .filter(|(_, e)| !matches!(attr(e, "mode"), Some(mode) if mode != "BeginBlock"))
    }

    /// End-block events, which always carry `mode=EndBlock`.
    pub fn end_block_events(&self) -> impl Iterator<Item = (usize, &Event)> {
        self.results
            .finalize_block_events
            .iter()
            .enumerate()
            .filter(|(_, e)| attr(e, "mode") == Some("EndBlock"))
    }

    /// Transaction results paired with their index, skipping any the chain rejected.
    ///
    /// A failed transaction still emits events, but they describe an attempt that did not change
    /// state, and recording them would double-count.
    pub fn successful_txs(
        &self,
    ) -> impl Iterator<Item = (usize, &tendermint::abci::types::ExecTxResult)> {
        self.results
            .txs_results
            .iter()
            .flatten()
            .enumerate()
            .filter(|(_, tx)| !tx.code.is_err())
    }
}

/// First attribute with this key, or `None`.
///
/// Linear, which is fine: events carry a handful of attributes and building a map per event
/// costs more than the scans it saves. Attributes whose key is not valid UTF-8 are skipped
/// rather than propagated as an error — `key_str` can fail for the base64 form used by
/// CometBFT 0.34, which THORNode has not spoken for a long time.
pub fn attr<'e>(event: &'e Event, key: &str) -> Option<&'e str> {
    event
        .attributes
        .iter()
        .find_map(|a| match (a.key_str(), a.value_str()) {
            (Ok(k), Ok(v)) if k == key => Some(v),
            _ => None,
        })
}

/// Every readable `(key, value)` pair on an event, in order.
pub fn attrs(event: &Event) -> impl Iterator<Item = (&str, &str)> {
    event
        .attributes
        .iter()
        .filter_map(|a| match (a.key_str(), a.value_str()) {
            (Ok(k), Ok(v)) => Some((k, v)),
            _ => None,
        })
}

/// Build an attribute, for tests and fixtures.
///
/// `EventAttribute` is an enum over the 0.34 (base64) and 0.37+ (plain string) encodings; the
/// tuple `From` impl produces the 0.37 form, which is what THORNode speaks.
pub fn make_attr(key: &str, value: &str) -> EventAttribute {
    (key, value, true).into()
}

/// Build an event, for tests and fixtures.
pub fn make_event(kind: &str, attributes: &[(&str, &str)]) -> Event {
    Event {
        kind: kind.to_string(),
        attributes: attributes.iter().map(|(k, v)| make_attr(k, v)).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real `block_results` payload from a THORChain mainnet node, trimmed to the shape that
    /// matters. Kept verbatim so that a `tendermint-rpc` upgrade which changes how any of these
    /// fields deserialise fails here rather than in production.
    const REAL_BLOCK_RESULTS: &str = r#"{
        "height": "27260503",
        "txs_results": [
            {"code": 0, "data": null, "log": "", "info": "", "gas_wanted": "0",
             "gas_used": "0", "events": [], "codespace": ""}
        ],
        "finalize_block_events": [
            {"type": "rewards", "attributes": [
                {"key": "bond_reward", "value": "12345", "index": true},
                {"key": "mode", "value": "BeginBlock", "index": true}
            ]},
            {"type": "swap", "attributes": [
                {"key": "pool", "value": "THOR.TCY", "index": true},
                {"key": "coin", "value": "250000 THOR.TCY", "index": true},
                {"key": "mode", "value": "EndBlock", "index": true}
            ]}
        ],
        "validator_updates": null,
        "consensus_param_updates": null,
        "app_hash": ""
    }"#;

    fn parsed() -> BlockResults {
        serde_json::from_str(REAL_BLOCK_RESULTS).expect("mainnet payload should parse")
    }

    #[test]
    fn parses_a_real_block_results_payload() {
        let br = parsed();
        assert_eq!(br.height.value(), 27_260_503);
        assert_eq!(br.finalize_block_events.len(), 2);
        assert_eq!(br.txs_results.as_ref().map(Vec::len), Some(1));
    }

    #[test]
    fn unknown_fields_do_not_break_parsing() {
        // A future CometBFT adding a field must not take the daemon down.
        let raw = REAL_BLOCK_RESULTS.replace(
            "\"app_hash\": \"\"",
            "\"app_hash\": \"\", \"some_field_from_the_future\": {\"nested\": true}",
        );
        let br: BlockResults = serde_json::from_str(&raw).unwrap();
        assert_eq!(br.height.value(), 27_260_503);
    }

    #[test]
    fn attribute_lookup() {
        let br = parsed();
        let swap = &br.finalize_block_events[1];
        assert_eq!(attr(swap, "pool"), Some("THOR.TCY"));
        assert_eq!(attr(swap, "coin"), Some("250000 THOR.TCY"));
        assert_eq!(attr(swap, "absent"), None);
    }

    #[test]
    fn attribute_lookup_returns_the_first_match() {
        let e = make_event("x", &[("k", "first"), ("k", "second")]);
        assert_eq!(attr(&e, "k"), Some("first"));
    }

    #[test]
    fn attrs_yields_everything_in_order() {
        let e = make_event("x", &[("a", "1"), ("b", "2")]);
        let got: Vec<_> = attrs(&e).collect();
        assert_eq!(got, vec![("a", "1"), ("b", "2")]);
    }

    #[test]
    fn missing_mode_counts_as_begin_block() {
        let block = block_with(vec![
            make_event("no_mode", &[]),
            make_event("begin", &[("mode", "BeginBlock")]),
            make_event("end", &[("mode", "EndBlock")]),
        ]);

        let begin: Vec<&str> = block
            .begin_block_events()
            .map(|(_, e)| e.kind.as_str())
            .collect();
        let end: Vec<&str> = block
            .end_block_events()
            .map(|(_, e)| e.kind.as_str())
            .collect();

        assert_eq!(begin, vec!["no_mode", "begin"]);
        assert_eq!(end, vec!["end"]);
    }

    #[test]
    fn event_indices_are_positions_in_the_original_list() {
        // The index feeds the event id, so it has to be the position in finalize_block_events,
        // not the position within the filtered subset.
        let block = block_with(vec![
            make_event("begin", &[("mode", "BeginBlock")]),
            make_event("end0", &[("mode", "EndBlock")]),
            make_event("end1", &[("mode", "EndBlock")]),
        ]);
        assert_eq!(
            block.end_block_events().map(|(i, _)| i).collect::<Vec<_>>(),
            vec![1, 2]
        );
    }

    #[test]
    fn failed_transactions_are_skipped() {
        let raw = REAL_BLOCK_RESULTS.replace(
            r#"[
            {"code": 0"#,
            r#"[
            {"code": 6"#,
        );
        let mut block = block_with(vec![]);
        block.results = serde_json::from_str(&raw).unwrap();
        assert_eq!(block.successful_txs().count(), 0);

        block.results = parsed();
        assert_eq!(block.successful_txs().count(), 1);
    }

    fn block_with(events: Vec<Event>) -> Block {
        let mut results = parsed();
        results.finalize_block_events = events;
        Block {
            height: 1,
            timestamp: Nano(0),
            hash: String::new(),
            chain_id: "thorchain-1".to_string(),
            results,
            txs: Vec::new(),
        }
    }
}
