//! `MIDGARD_*` environment overrides.
//!
//! Every leaf in the config tree gets an environment variable made from its path: nesting joined
//! with underscores, uppercased, behind a `MIDGARD_` prefix.
//!
//! ```text
//! listen_port                 MIDGARD_LISTEN_PORT
//! timescale.host              MIDGARD_TIMESCALE_HOST
//! thorchain.thornode_url      MIDGARD_THORCHAIN_THORNODE_URL
//! endpoints.action_params.max_limit
//!                             MIDGARD_ENDPOINTS_ACTION_PARAMS_MAX_LIMIT
//! ```
//!
//! The variable set is derived from the tree we are overlaying rather than hardcoded, so adding
//! a config field automatically gets an override and there is no second list to forget to
//! update. The flip side is that a typo'd variable is silently ignored, which is why
//! [`apply`] logs every override it does apply — it is the only way an operator can tell
//! `MIDGARD_TIMESCAL_HOST` did nothing.
//!
//! Collections have string spellings, because a JSON array in a shell variable is miserable to
//! write:
//!
//! ```text
//! MIDGARD_USD_POOLS="BTC.BTC,ETH.ETH"
//! MIDGARD_POOLS_DECIMAL="ETH.ETH:18,BTC.BTC:8"
//! ```
//!
//! Map-valued fields are set as a whole rather than key by key. Per-key variables would look
//! nicer, but there would then be no way to *remove* a default entry, and the defaults include
//! one (`case_insensitive_chains`). They also cannot be recognised by shape — `timescale` is a
//! struct whose fields happen to all be scalars, and it is indistinguishable at the JSON level
//! from a string map — so [`SCALAR_MAPS`] lists them explicitly along with their value type,
//! which doubles as the type witness an empty map cannot provide.

use serde_json::{Map, Value};

use crate::ConfigError;

const PREFIX: &str = "MIDGARD_";

/// The type of a scalar map's values, used to parse `KEY:VALUE` entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MapValue {
    Str,
    Int,
    Bool,
}

/// Config paths that are string-keyed maps rather than nested structs.
const SCALAR_MAPS: &[(&[&str], MapValue)] = &[
    (&["pools_decimal"], MapValue::Int),
    (&["filtered_addresses"], MapValue::Str),
    (&["case_insensitive_chains"], MapValue::Bool),
];

fn scalar_map_at(path: &[String]) -> Option<MapValue> {
    SCALAR_MAPS
        .iter()
        .find(|(p, _)| p.len() == path.len() && p.iter().zip(path).all(|(a, b)| *a == b))
        .map(|(_, kind)| *kind)
}

/// Snapshot the process environment in the shape [`apply`] wants.
pub fn from_process() -> Vec<(String, String)> {
    std::env::vars().collect()
}

/// Overlay environment variables onto an already-merged config tree.
pub fn apply(root: &mut Value, environment: &[(String, String)]) -> Result<(), ConfigError> {
    let mut overrides: Vec<(Vec<String>, &str, &str)> = Vec::new();
    collect(root, &mut Vec::new(), environment, &mut overrides);

    for (path, var, raw) in overrides {
        let map_kind = scalar_map_at(&path);
        let target = lookup_mut(root, &path).expect("path came from this tree");
        let parsed = coerce(target, map_kind, raw).map_err(|reason| ConfigError::Env {
            var: var.to_string(),
            reason,
        })?;
        tracing::info!(var, path = %path.join("."), "config overridden from environment");
        *target = parsed;
    }
    Ok(())
}

/// Walk the tree collecting `(path, var name, value)` for every leaf that has a variable set.
///
/// Collected first and applied afterwards because we cannot hold a mutable borrow of a node
/// while still walking the rest of the tree.
fn collect<'a>(
    node: &Value,
    path: &mut Vec<String>,
    environment: &'a [(String, String)],
    out: &mut Vec<(Vec<String>, &'a str, &'a str)>,
) {
    // Maps of scalars are configured as a whole, not per-key, so we stop at them rather than
    // descending into whatever keys happen to be present.
    if let (Value::Object(map), None) = (node, scalar_map_at(path)) {
        for (key, child) in map {
            path.push(key.clone());
            collect(child, path, environment, out);
            path.pop();
        }
        return;
    }

    let var_name = env_name(path);
    if let Some((var, value)) = environment.iter().find(|(k, _)| *k == var_name) {
        out.push((path.clone(), var.as_str(), value.as_str()));
    }
}

fn env_name(path: &[String]) -> String {
    let mut s = String::from(PREFIX);
    for (i, segment) in path.iter().enumerate() {
        if i > 0 {
            s.push('_');
        }
        s.push_str(&segment.to_ascii_uppercase());
    }
    s
}

fn lookup_mut<'v>(root: &'v mut Value, path: &[String]) -> Option<&'v mut Value> {
    let mut node = root;
    for segment in path {
        node = node.as_object_mut()?.get_mut(segment)?;
    }
    Some(node)
}

/// Parse `raw` into whatever shape the existing value has.
///
/// Using the current value as the type witness is what keeps this generic: for scalars and lists
/// we never need a schema, only the tree we are about to modify. Maps are the exception — an
/// empty one says nothing about its value type — so `map_kind` supplies it.
fn coerce(current: &Value, map_kind: Option<MapValue>, raw: &str) -> Result<Value, String> {
    if let Some(kind) = map_kind {
        let mut map = Map::new();
        for entry in split_list(raw) {
            let (key, value) = entry
                .split_once(':')
                .ok_or_else(|| format!("{entry:?} is not KEY:VALUE"))?;
            let value = match kind {
                MapValue::Int => Value::Number(
                    value
                        .trim()
                        .parse::<i64>()
                        .map_err(|_| format!("{value:?} is not an integer"))?
                        .into(),
                ),
                MapValue::Bool => match value.trim() {
                    "true" | "1" | "yes" => Value::Bool(true),
                    "false" | "0" | "no" => Value::Bool(false),
                    other => return Err(format!("{other:?} is not a boolean")),
                },
                MapValue::Str => Value::String(value.trim().to_string()),
            };
            map.insert(key.trim().to_string(), value);
        }
        return Ok(Value::Object(map));
    }

    match current {
        Value::Bool(_) => match raw {
            "true" | "1" | "yes" => Ok(Value::Bool(true)),
            "false" | "0" | "no" => Ok(Value::Bool(false)),
            _ => Err(format!("{raw:?} is not a boolean")),
        },
        Value::Number(n) => {
            if n.is_f64() {
                raw.parse::<f64>()
                    .ok()
                    .and_then(serde_json::Number::from_f64)
                    .map(Value::Number)
                    .ok_or_else(|| format!("{raw:?} is not a number"))
            } else {
                raw.parse::<i64>()
                    .map(|v| Value::Number(v.into()))
                    .map_err(|_| format!("{raw:?} is not an integer"))
            }
        }
        Value::String(_) => Ok(Value::String(raw.to_string())),
        Value::Array(_) => Ok(Value::Array(
            split_list(raw)
                .map(|s| Value::String(s.to_string()))
                .collect(),
        )),
        // Reachable only for an object that is not in SCALAR_MAPS, i.e. a nested struct. There
        // is no sensible whole-struct spelling, so refuse rather than guess.
        Value::Object(_) => Err("cannot set a nested section from a single variable".to_string()),
        Value::Null => Ok(Value::String(raw.to_string())),
    }
}

fn split_list(raw: &str) -> impl Iterator<Item = &str> {
    raw.split(',').map(str::trim).filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    // These exercise the overlay through the public entry point rather than calling `apply`
    // directly, because the interesting behaviour is what comes out the other end as a Config.
    use crate::{load_from, Config};

    fn env(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn scalar_overrides_at_every_depth() {
        let c = load_from(
            "",
            &env(&[
                ("MIDGARD_LISTEN_PORT", "9000"),
                ("MIDGARD_TIMESCALE_HOST", "pg"),
                (
                    "MIDGARD_THORCHAIN_THORNODE_URL",
                    "http://node:1317/thorchain",
                ),
                ("MIDGARD_ENDPOINTS_ACTION_PARAMS_MAX_LIMIT", "200"),
            ]),
        )
        .unwrap();

        assert_eq!(c.listen_port, 9000);
        assert_eq!(c.timescale.host, "pg");
        assert_eq!(c.thorchain.thornode_url, "http://node:1317/thorchain");
        assert_eq!(c.endpoints.action_params.max_limit, 200);
    }

    #[test]
    fn booleans_accept_the_usual_spellings() {
        for (raw, want) in [("true", true), ("1", true), ("false", false), ("no", false)] {
            let c = load_from("", &env(&[("MIDGARD_TIMESCALE_NO_AUTO_UPDATE_DDL", raw)])).unwrap();
            assert_eq!(c.timescale.no_auto_update_ddl, want, "{raw}");
        }
    }

    #[test]
    fn durations_go_through_the_duration_parser() {
        let c = load_from("", &env(&[("MIDGARD_MAX_BLOCK_AGE", "90s")])).unwrap();
        assert_eq!(c.max_block_age, crate::Duration::from_secs(90));

        assert!(load_from("", &env(&[("MIDGARD_MAX_BLOCK_AGE", "soon")])).is_err());
    }

    #[test]
    fn lists_are_comma_separated() {
        let c = load_from("", &env(&[("MIDGARD_USD_POOLS", "BTC.BTC, ETH.ETH")])).unwrap();
        assert_eq!(
            c.usd_pools,
            vec!["BTC.BTC".to_string(), "ETH.ETH".to_string()]
        );
    }

    #[test]
    fn maps_are_comma_separated_key_colon_value() {
        let c = load_from(
            "",
            &env(&[("MIDGARD_POOLS_DECIMAL", "ETH.ETH:18,BTC.BTC:8")]),
        )
        .unwrap();
        assert_eq!(c.pools_decimal.get("ETH.ETH"), Some(&18));
        assert_eq!(c.pools_decimal.get("BTC.BTC"), Some(&8));

        let c = load_from(
            "",
            &env(&[("MIDGARD_FILTERED_ADDRESSES", "thor1abc:treasury")]),
        )
        .unwrap();
        assert_eq!(
            c.filtered_addresses.get("thor1abc").map(String::as_str),
            Some("treasury")
        );
    }

    #[test]
    fn a_map_entry_is_not_addressable_on_its_own() {
        // MIDGARD_CASE_INSENSITIVE_CHAINS_ETH must not be interpreted as a path into the map,
        // otherwise there would be no way to remove the default ETH entry.
        let c = load_from(
            "",
            &env(&[("MIDGARD_CASE_INSENSITIVE_CHAINS_ETH", "false")]),
        )
        .unwrap();
        assert_eq!(
            c.case_insensitive_chains,
            Config::default().case_insensitive_chains
        );

        let c = load_from(
            "",
            &env(&[("MIDGARD_CASE_INSENSITIVE_CHAINS", "AVAX:true")]),
        )
        .unwrap();
        assert_eq!(c.case_insensitive_chains.get("AVAX"), Some(&true));
        assert_eq!(c.case_insensitive_chains.get("ETH"), None);
    }

    #[test]
    fn bad_value_names_the_variable() {
        let err = load_from("", &env(&[("MIDGARD_LISTEN_PORT", "eighty")])).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("MIDGARD_LISTEN_PORT"), "{msg}");
    }

    #[test]
    fn unrelated_variables_are_left_alone() {
        let c = load_from("", &env(&[("PATH", "/usr/bin"), ("MIDGARD", "x")])).unwrap();
        assert_eq!(c, Config::default());
    }

    #[test]
    fn env_beats_files() {
        let dir = std::env::temp_dir().join("midgard-config-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("port.json");
        std::fs::write(&path, r#"{"listen_port": 1234}"#).unwrap();

        let c = load_from(
            &path.display().to_string(),
            &env(&[("MIDGARD_LISTEN_PORT", "9999")]),
        )
        .unwrap();
        assert_eq!(c.listen_port, 9999);
    }
}
