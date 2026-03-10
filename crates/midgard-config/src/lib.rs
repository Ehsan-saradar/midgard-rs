//! Configuration loading.
//!
//! Three layers, each overriding the last:
//!
//! 1. compiled-in defaults ([`Config::default`])
//! 2. a colon-separated list of JSON files, merged left to right
//! 3. `MIDGARD_`-prefixed environment variables
//!
//! The merge in step 2 is a deep merge rather than a whole-object replace, which is what makes
//! the upstream idiom of composing `base.json:pg.json:net-main.json` work — each file only has
//! to carry the keys it actually changes.

pub mod duration;
pub mod env;
pub mod schema;

use std::path::{Path, PathBuf};

pub use duration::Duration;
pub use schema::{
    ActionParams, Config, Endpoints, EventRecorder, Genesis, Logs, ThorChain, TimeScale,
};

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("reading config file {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("parsing config file {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    #[error("config file {path} must contain a JSON object at the top level")]
    NotAnObject { path: PathBuf },

    #[error("invalid config: {0}")]
    Invalid(#[source] serde_json::Error),

    #[error("environment variable {var}: {reason}")]
    Env { var: String, reason: String },
}

/// Load configuration from a colon-separated file list, then apply environment overrides.
///
/// An empty list is valid and yields the defaults with the environment applied on top, which is
/// how the container image is usually driven.
pub fn load(colon_separated_paths: &str) -> Result<Config, ConfigError> {
    load_from(colon_separated_paths, &env::from_process())
}

/// The same, with the environment injected — the seam the tests use.
pub fn load_from(
    colon_separated_paths: &str,
    environment: &[(String, String)],
) -> Result<Config, ConfigError> {
    let mut merged =
        serde_json::to_value(Config::default()).expect("the default config is serializable");

    for path in split_paths(colon_separated_paths) {
        let value = read_object(&path)?;
        merge(&mut merged, value);
    }

    env::apply(&mut merged, environment)?;

    serde_json::from_value(merged).map_err(ConfigError::Invalid)
}

fn split_paths(list: &str) -> Vec<PathBuf> {
    // "null" is accepted as an explicit "no files", matching upstream's handling of the
    // argument that docker-compose passes when no config is mounted.
    if list.is_empty() || list == "null" {
        return Vec::new();
    }
    list.split(':')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .collect()
}

fn read_object(path: &Path) -> Result<serde_json::Map<String, serde_json::Value>, ConfigError> {
    let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let value: serde_json::Value =
        serde_json::from_str(&text).map_err(|source| ConfigError::Parse {
            path: path.to_path_buf(),
            source,
        })?;
    match value {
        serde_json::Value::Object(map) => Ok(map),
        _ => Err(ConfigError::NotAnObject {
            path: path.to_path_buf(),
        }),
    }
}

/// Deep-merge `overlay` into `base`.
///
/// Objects recurse; everything else replaces. In particular a list in a later file replaces the
/// earlier list rather than appending to it, so `usd_pools` can be narrowed and not just grown.
fn merge(base: &mut serde_json::Value, overlay: serde_json::Map<String, serde_json::Value>) {
    let base_map = match base {
        serde_json::Value::Object(m) => m,
        _ => {
            *base = serde_json::Value::Object(overlay);
            return;
        }
    };
    for (key, value) in overlay {
        match (base_map.get_mut(&key), value) {
            (Some(existing @ serde_json::Value::Object(_)), serde_json::Value::Object(sub)) => {
                merge(existing, sub);
            }
            (_, value) => {
                base_map.insert(key, value);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_temp(name: &str, contents: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("midgard-config-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(contents.as_bytes()).unwrap();
        path
    }

    #[test]
    fn no_files_gives_defaults() {
        let c = load_from("", &[]).unwrap();
        assert_eq!(c, Config::default());
        assert_eq!(load_from("null", &[]).unwrap(), Config::default());
    }

    #[test]
    fn later_files_win_key_by_key() {
        let a = write_temp(
            "a.json",
            r#"{"listen_port": 1111, "timescale": {"host": "a"}}"#,
        );
        let b = write_temp("b.json", r#"{"timescale": {"host": "b"}}"#);

        let list = format!("{}:{}", a.display(), b.display());
        let c = load_from(&list, &[]).unwrap();

        // b only mentioned the host, so the port from a's file and the default database survive.
        assert_eq!(c.listen_port, 1111);
        assert_eq!(c.timescale.host, "b");
        assert_eq!(c.timescale.database, "midgard");
    }

    #[test]
    fn lists_replace_rather_than_append() {
        let a = write_temp("pools.json", r#"{"usd_pools": ["ONE"]}"#);
        let c = load_from(&a.display().to_string(), &[]).unwrap();
        assert_eq!(c.usd_pools, vec!["ONE".to_string()]);
    }

    #[test]
    fn missing_file_is_an_error_not_a_silent_default() {
        let err = load_from("/nonexistent/midgard.json", &[]).unwrap_err();
        assert!(matches!(err, ConfigError::Read { .. }), "{err:?}");
    }

    #[test]
    fn malformed_file_names_itself() {
        let p = write_temp("bad.json", "{not json");
        let err = load_from(&p.display().to_string(), &[]).unwrap_err();
        match err {
            ConfigError::Parse { path, .. } => assert_eq!(path, p),
            other => panic!("expected a parse error, got {other:?}"),
        }
    }

    #[test]
    fn top_level_array_is_rejected() {
        let p = write_temp("array.json", "[1, 2]");
        let err = load_from(&p.display().to_string(), &[]).unwrap_err();
        assert!(matches!(err, ConfigError::NotAnObject { .. }), "{err:?}");
    }
}
