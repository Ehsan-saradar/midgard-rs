//! The configuration tree and its defaults.
//!
//! Field names and nesting match the Go implementation's config files, so an existing
//! `config/ex/*.json` set can be pointed at this binary unchanged.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::duration::Duration;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub listen_port: u16,
    pub shutdown_timeout: Duration,

    /// Webserver read/write timeouts.
    pub read_timeout: Duration,
    pub write_timeout: Duration,

    /// `/v2/health` reports `inSync` while `now - last_block < max_block_age`.
    pub max_block_age: Duration,

    pub thorchain: ThorChain,
    pub timescale: TimeScale,
    pub genesis: Genesis,
    pub event_recorder: EventRecorder,
    pub endpoints: Endpoints,
    pub logs: Logs,

    /// Pools consulted, in order, to price RUNE in USD. The deepest available one wins.
    pub usd_pools: Vec<String>,

    /// Native precision per pool asset, for clients that need to render amounts. Purely
    /// informational — every amount Midgard stores has already been normalised to e8.
    pub pools_decimal: BTreeMap<String, i64>,

    /// Addresses excluded from the actions feed, mapped to an optional label.
    pub filtered_addresses: BTreeMap<String, String>,

    /// Chains whose addresses should be compared case-insensitively.
    pub case_insensitive_chains: BTreeMap<String, bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct ThorChain {
    /// Tendermint RPC. A trailing `/websocket` is stripped to form the HTTP base.
    pub tendermint_url: String,
    /// THORNode REST, for the state that is not in the event stream (nodes, mimir, ...).
    pub thornode_url: String,

    pub fetch_batch_size: usize,
    pub parallelism: usize,

    pub read_timeout: Duration,
    /// How long to wait before retrying after the chain tip request fails.
    pub last_chain_backoff: Duration,

    pub max_status_retries: usize,
    pub status_retry_backoff: Duration,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct TimeScale {
    pub host: String,
    pub port: u16,
    pub user_name: String,
    pub password: String,
    pub database: String,
    pub sslmode: String,

    pub max_open_conns: u32,
    /// Blocks buffered before a write transaction is flushed.
    pub commit_batch_size: usize,

    /// Fail on a schema version mismatch instead of dropping and rebuilding.
    pub no_auto_update_ddl: bool,
}

/// Bootstrap point when starting from a genesis file rather than block 1.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Genesis {
    pub initial_block_height: i64,
    pub initial_block_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct EventRecorder {
    /// `transfer` events are high volume and only needed for the balance endpoints.
    pub on_transfer_enabled: bool,
    pub on_message_enabled: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Endpoints {
    pub action_params: ActionParams,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct ActionParams {
    pub max_limit: u64,
    pub max_addresses: usize,
    pub max_assets: usize,
    pub max_tx_type: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Logs {
    /// Human-readable output instead of JSON lines.
    pub console_logger: bool,
    pub no_color: bool,
    pub level: String,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            listen_port: 8080,
            shutdown_timeout: Duration::from_secs(20),
            read_timeout: Duration::from_secs(20),
            write_timeout: Duration::from_secs(20),
            max_block_age: Duration::from_secs(60),

            thorchain: ThorChain::default(),
            timescale: TimeScale::default(),
            genesis: Genesis::default(),
            event_recorder: EventRecorder::default(),
            endpoints: Endpoints::default(),
            logs: Logs::default(),

            usd_pools: vec![
                "BNB.BUSD-BD1".to_string(),
                "ETH.USDT-0XDAC17F958D2EE523A2206206994597C13D831EC7".to_string(),
                "ETH.USDC-0XA0B86991C6218B36C1D19D4A2E9EB0CE3606EB48".to_string(),
            ],
            pools_decimal: BTreeMap::new(),
            filtered_addresses: BTreeMap::new(),
            case_insensitive_chains: [("ETH".to_string(), true)].into_iter().collect(),
        }
    }
}

impl Default for ThorChain {
    fn default() -> ThorChain {
        ThorChain {
            tendermint_url: "http://localhost:26657/websocket".to_string(),
            thornode_url: "http://localhost:1317/thorchain".to_string(),

            // Upstream ships with parallelism disabled: a block range containing a few very
            // large blocks will time out the whole range request, and the simplest way to get
            // past it is to not batch at all. Operators turn it up once they are caught up.
            fetch_batch_size: 1,
            parallelism: 1,

            read_timeout: Duration::from_secs(8),
            last_chain_backoff: Duration::from_secs(7),
            max_status_retries: 10,
            status_retry_backoff: Duration::from_secs(5),
        }
    }
}

impl Default for TimeScale {
    fn default() -> TimeScale {
        TimeScale {
            host: "localhost".to_string(),
            port: 5432,
            user_name: "midgard".to_string(),
            password: "password".to_string(),
            database: "midgard".to_string(),
            sslmode: "disable".to_string(),
            max_open_conns: 80,
            commit_batch_size: 100,
            no_auto_update_ddl: false,
        }
    }
}

impl Default for EventRecorder {
    fn default() -> EventRecorder {
        EventRecorder {
            on_transfer_enabled: true,
            on_message_enabled: false,
        }
    }
}

impl Default for ActionParams {
    fn default() -> ActionParams {
        ActionParams {
            max_limit: 50,
            max_addresses: 50,
            max_assets: 4,
            max_tx_type: 4,
        }
    }
}

impl Default for Logs {
    fn default() -> Logs {
        Logs {
            console_logger: true,
            no_color: false,
            level: "info".to_string(),
        }
    }
}

impl TimeScale {
    /// A libpq connection string. Kept out of `Display`/`Debug` reach on purpose so the password
    /// does not end up in a log line by accident.
    pub fn connection_string(&self) -> String {
        format!(
            "postgres://{}:{}@{}:{}/{}?sslmode={}",
            self.user_name, self.password, self.host, self.port, self.database, self.sslmode
        )
    }
}

impl ThorChain {
    /// Split the Tendermint URL into the HTTP base and the websocket path.
    ///
    /// Raw Tendermint serves JSON-RPC at `/` and websockets at `/websocket`, so a configured
    /// `http://host:26657/websocket` means base `http://host:26657`. Anything without that
    /// suffix is a proxy that upgrades on the same path, and is used as-is.
    pub fn split_tendermint_url(&self) -> (String, String) {
        match self.tendermint_url.strip_suffix("/websocket") {
            Some(base) => (base.to_string(), "/websocket".to_string()),
            None => (self.tendermint_url.clone(), String::new()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_round_trip_through_json() {
        let c = Config::default();
        let json = serde_json::to_string(&c).unwrap();
        assert_eq!(serde_json::from_str::<Config>(&json).unwrap(), c);
    }

    #[test]
    fn unknown_keys_are_rejected() {
        let err = serde_json::from_str::<Config>(r#"{"listen_prot": 8080}"#).unwrap_err();
        assert!(err.to_string().contains("unknown field"), "{err}");
    }

    #[test]
    fn partial_files_keep_defaults_for_everything_else() {
        let c: Config = serde_json::from_str(r#"{"listen_port": 9000}"#).unwrap();
        assert_eq!(c.listen_port, 9000);
        assert_eq!(c.timescale.port, 5432);
    }

    #[test]
    fn tendermint_url_split() {
        let mut t = ThorChain::default();
        assert_eq!(
            t.split_tendermint_url(),
            (
                "http://localhost:26657".to_string(),
                "/websocket".to_string()
            )
        );

        t.tendermint_url = "https://gw.example.com/chain/thorchain_rpc".to_string();
        assert_eq!(
            t.split_tendermint_url(),
            (
                "https://gw.example.com/chain/thorchain_rpc".to_string(),
                String::new()
            )
        );
    }

    #[test]
    fn connection_string_shape() {
        let t = TimeScale::default();
        assert_eq!(
            t.connection_string(),
            "postgres://midgard:password@localhost:5432/midgard?sslmode=disable"
        );
    }
}
