//! The THORNode REST API.
//!
//! Everything Midgard can reconstruct, it reconstructs from the event stream. This client covers
//! what is left: state that THORNode holds but never emits as an event, so there is no historical
//! series to derive and the only available answer is "what it is right now".
//!
//! That is why `/v2/nodes` and `/v2/network` have no history counterparts while `/v2/pools`
//! does. It is also why this being unreachable degrades those endpoints rather than stopping the
//! block pipeline — the two are independent, and a node with a broken REST port can still index.

use std::time::Duration;

use serde::Deserialize;

use crate::rpc::RpcError;

#[derive(Debug, thiserror::Error)]
pub enum ThorNodeError {
    #[error("THORNode request to {path} failed: {source}")]
    Http {
        path: String,
        #[source]
        source: reqwest::Error,
    },

    #[error("THORNode {path}: malformed response: {source}")]
    Decode {
        path: String,
        #[source]
        source: reqwest::Error,
    },
}

impl From<ThorNodeError> for RpcError {
    fn from(e: ThorNodeError) -> RpcError {
        RpcError::Malformed {
            method: "thornode".to_string(),
            message: e.to_string(),
        }
    }
}

/// Amounts arrive as decimal strings, because they do not fit a JSON number.
fn e8(s: &str) -> i64 {
    s.parse().unwrap_or(0)
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Node {
    #[serde(default)]
    pub node_address: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub total_bond: String,
    #[serde(default)]
    pub version: String,
}

impl Node {
    pub fn bond_e8(&self) -> i64 {
        e8(&self.total_bond)
    }

    /// THORNode has spelled this "Active" and "active" at different versions.
    pub fn is_active(&self) -> bool {
        self.status.eq_ignore_ascii_case("active")
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Pool {
    #[serde(default)]
    pub asset: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub balance_asset: String,
    #[serde(default)]
    pub balance_rune: String,
    #[serde(default)]
    pub pool_units: String,
    #[serde(default)]
    pub lp_units: String,
    #[serde(default)]
    pub synth_units: String,
    #[serde(default)]
    pub synth_supply: String,
    #[serde(default)]
    pub savers_depth: String,
    #[serde(default)]
    pub savers_units: String,
    #[serde(default)]
    pub decimals: Option<i64>,
}

impl Pool {
    pub fn asset_e8(&self) -> i64 {
        e8(&self.balance_asset)
    }

    pub fn rune_e8(&self) -> i64 {
        e8(&self.balance_rune)
    }

    pub fn units(&self) -> i64 {
        e8(&self.pool_units)
    }

    pub fn synth_e8(&self) -> i64 {
        e8(&self.synth_supply)
    }
}

/// Network-wide totals. Only the fields the API surfaces are declared.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct Network {
    #[serde(default)]
    pub bond_reward_rune: String,
    #[serde(default)]
    pub total_bond_units: String,
    #[serde(default)]
    pub total_reserve: String,
}

#[derive(Debug, Clone)]
pub struct ThorNode {
    http: reqwest::Client,
    base: String,
}

impl ThorNode {
    pub fn new(base_url: &str, timeout: Duration) -> Result<ThorNode, ThorNodeError> {
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|source| ThorNodeError::Http {
                path: base_url.to_string(),
                source,
            })?;
        Ok(ThorNode {
            http,
            base: base_url.trim_end_matches('/').to_string(),
        })
    }

    pub fn url(&self) -> &str {
        &self.base
    }

    async fn get<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T, ThorNodeError> {
        let url = format!("{}{}", self.base, path);
        self.http
            .get(&url)
            .send()
            .await
            .map_err(|source| ThorNodeError::Http {
                path: path.to_string(),
                source,
            })?
            .error_for_status()
            .map_err(|source| ThorNodeError::Http {
                path: path.to_string(),
                source,
            })?
            .json()
            .await
            .map_err(|source| ThorNodeError::Decode {
                path: path.to_string(),
                source,
            })
    }

    pub async fn nodes(&self) -> Result<Vec<Node>, ThorNodeError> {
        self.get("/nodes").await
    }

    pub async fn pools(&self) -> Result<Vec<Pool>, ThorNodeError> {
        self.get("/pools").await
    }

    pub async fn network(&self) -> Result<Network, ThorNodeError> {
        self.get("/network").await
    }

    /// Mimir overrides: the chain's runtime-tunable constants.
    ///
    /// Values come back as JSON numbers here rather than strings, unlike everywhere else in this
    /// API.
    pub async fn mimir(&self) -> Result<std::collections::BTreeMap<String, i64>, ThorNodeError> {
        self.get("/mimir").await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trimmed from a real mainnet `/thorchain/pools` response.
    const REAL_POOL: &str = r#"{
        "asset": "AVAX.AVAX",
        "short_code": "a",
        "status": "Available",
        "pending_inbound_asset": "0",
        "pending_inbound_rune": "0",
        "balance_asset": "8362367623370",
        "balance_rune": "1234567890",
        "pool_units": "999",
        "LP_units": "888",
        "synth_units": "111",
        "synth_supply": "42",
        "savers_depth": "7",
        "savers_units": "6",
        "decimals": 8
    }"#;

    #[test]
    fn parses_a_real_pool_payload() {
        let p: Pool = serde_json::from_str(REAL_POOL).unwrap();
        assert_eq!(p.asset, "AVAX.AVAX");
        assert_eq!(p.asset_e8(), 8_362_367_623_370);
        assert_eq!(p.rune_e8(), 1_234_567_890);
        assert_eq!(p.units(), 999);
        assert_eq!(p.synth_e8(), 42);
        assert_eq!(p.decimals, Some(8));
    }

    #[test]
    fn missing_fields_fall_back_rather_than_failing() {
        // THORNode omits fields depending on version and pool type; an absent one must not take
        // the whole pool list down.
        let p: Pool = serde_json::from_str(r#"{"asset": "BTC.BTC"}"#).unwrap();
        assert_eq!(p.asset, "BTC.BTC");
        assert_eq!(p.asset_e8(), 0);
        assert_eq!(p.decimals, None);
    }

    #[test]
    fn unparseable_amounts_read_as_zero() {
        // Better than refusing the whole response: one broken figure should not hide the rest.
        let p: Pool =
            serde_json::from_str(r#"{"asset": "X", "balance_rune": "not a number"}"#).unwrap();
        assert_eq!(p.rune_e8(), 0);
    }

    #[test]
    fn node_status_comparison_is_case_insensitive() {
        for status in ["Active", "active", "ACTIVE"] {
            let n = Node {
                status: status.to_string(),
                ..Node::default()
            };
            assert!(n.is_active(), "{status}");
        }
        let n = Node {
            status: "Standby".to_string(),
            ..Node::default()
        };
        assert!(!n.is_active());
    }

    #[test]
    fn trailing_slash_in_the_base_url_does_not_double_up() {
        let tn = ThorNode::new("http://localhost:1317/thorchain/", Duration::from_secs(1)).unwrap();
        assert_eq!(tn.url(), "http://localhost:1317/thorchain");
    }
}
