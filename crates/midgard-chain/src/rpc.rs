//! JSON-RPC 2.0 over HTTP, with batching.
//!
//! Batching is the reason this is hand-rolled. Catching up from genesis means twenty-seven
//! million blocks and two calls per block; at one HTTP round trip each that is the entire cost
//! of the sync. Tendermint accepts a JSON array of requests and answers with an array of
//! responses, which collapses a batch into one round trip.
//!
//! Responses in a batch come back in arbitrary order, so every request carries an integer id and
//! results are reassembled by it rather than by position.

use std::collections::HashMap;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde_json::{json, Value};

#[derive(Debug, thiserror::Error)]
pub enum RpcError {
    #[error("HTTP request to {url} failed: {source}")]
    Http {
        url: String,
        #[source]
        source: reqwest::Error,
    },

    #[error("{method} returned an error: {message} (code {code})")]
    Rpc {
        method: String,
        code: i64,
        message: String,
    },

    #[error("{method}: malformed response: {0}", message)]
    Malformed { method: String, message: String },
}

/// A Tendermint JSON-RPC endpoint.
#[derive(Debug, Clone)]
pub struct RpcClient {
    http: reqwest::Client,
    url: String,
}

impl RpcClient {
    pub fn new(base_url: &str, timeout: Duration) -> Result<RpcClient, RpcError> {
        let http = reqwest::Client::builder()
            .timeout(timeout)
            // Catching up reuses one connection for millions of requests; without this the
            // socket churn shows up as a measurable fraction of sync time.
            .pool_max_idle_per_host(8)
            .build()
            .map_err(|source| RpcError::Http {
                url: base_url.to_string(),
                source,
            })?;

        Ok(RpcClient {
            http,
            url: base_url.trim_end_matches('/').to_string(),
        })
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    /// One request, one response.
    pub async fn call<T: DeserializeOwned>(
        &self,
        method: &str,
        params: Value,
    ) -> Result<T, RpcError> {
        let body = json!({"jsonrpc": "2.0", "id": 0, "method": method, "params": params});

        let response: Value = self
            .http
            .post(&self.url)
            .json(&body)
            .send()
            .await
            .map_err(|source| RpcError::Http {
                url: self.url.clone(),
                source,
            })?
            .error_for_status()
            .map_err(|source| RpcError::Http {
                url: self.url.clone(),
                source,
            })?
            .json()
            .await
            .map_err(|source| RpcError::Http {
                url: self.url.clone(),
                source,
            })?;

        extract_result(method, response)
    }

    /// Many requests, one round trip.
    ///
    /// Returns results in the order the calls were given, regardless of the order the node
    /// answered in. Any single failing call fails the whole batch: the sync loop cannot use a
    /// batch with a hole in it, and retrying the range is the recovery either way.
    pub async fn call_batch<T: DeserializeOwned>(
        &self,
        method: &str,
        params: Vec<Value>,
    ) -> Result<Vec<T>, RpcError> {
        if params.is_empty() {
            return Ok(Vec::new());
        }

        let body: Vec<Value> = params
            .iter()
            .enumerate()
            .map(|(i, p)| json!({"jsonrpc": "2.0", "id": i, "method": method, "params": p}))
            .collect();

        let response: Value = self
            .http
            .post(&self.url)
            .json(&body)
            .send()
            .await
            .map_err(|source| RpcError::Http {
                url: self.url.clone(),
                source,
            })?
            .error_for_status()
            .map_err(|source| RpcError::Http {
                url: self.url.clone(),
                source,
            })?
            .json()
            .await
            .map_err(|source| RpcError::Http {
                url: self.url.clone(),
                source,
            })?;

        // A batch whose every entry failed comes back as a bare object rather than an array.
        let entries = response
            .as_array()
            .cloned()
            .ok_or_else(|| RpcError::Malformed {
                method: method.to_string(),
                message: format!("expected an array of responses, got {response}"),
            })?;

        if entries.len() != params.len() {
            return Err(RpcError::Malformed {
                method: method.to_string(),
                message: format!(
                    "asked for {} responses, got {}",
                    params.len(),
                    entries.len()
                ),
            });
        }

        let mut by_id: HashMap<i64, Value> = HashMap::with_capacity(entries.len());
        for entry in entries {
            let id =
                entry
                    .get("id")
                    .and_then(Value::as_i64)
                    .ok_or_else(|| RpcError::Malformed {
                        method: method.to_string(),
                        message: format!("response has no usable id: {entry}"),
                    })?;
            by_id.insert(id, entry);
        }

        let mut out = Vec::with_capacity(params.len());
        for i in 0..params.len() {
            let entry = by_id
                .remove(&(i as i64))
                .ok_or_else(|| RpcError::Malformed {
                    method: method.to_string(),
                    message: format!("no response with id {i}"),
                })?;
            out.push(extract_result(method, entry)?);
        }
        Ok(out)
    }
}

/// Pull `result` out of a JSON-RPC envelope, turning `error` into an `Err`.
fn extract_result<T: DeserializeOwned>(method: &str, envelope: Value) -> Result<T, RpcError> {
    if let Some(error) = envelope.get("error").filter(|e| !e.is_null()) {
        return Err(RpcError::Rpc {
            method: method.to_string(),
            code: error.get("code").and_then(Value::as_i64).unwrap_or(0),
            message: error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string()
                // `data` is where Tendermint puts the part you actually need.
                .to_string()
                + &error
                    .get("data")
                    .and_then(Value::as_str)
                    .map(|d| format!(": {d}"))
                    .unwrap_or_default(),
        });
    }

    let result = envelope
        .get("result")
        .cloned()
        .ok_or_else(|| RpcError::Malformed {
            method: method.to_string(),
            message: format!("no result field in {envelope}"),
        })?;

    serde_json::from_value(result).map_err(|e| RpcError::Malformed {
        method: method.to_string(),
        message: e.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, serde::Deserialize, PartialEq)]
    struct Thing {
        n: i64,
    }

    #[test]
    fn extracts_a_result() {
        let v = json!({"jsonrpc": "2.0", "id": 0, "result": {"n": 7}});
        assert_eq!(extract_result::<Thing>("m", v).unwrap(), Thing { n: 7 });
    }

    #[test]
    fn a_null_error_field_is_not_an_error() {
        // Tendermint sends "error": null on success, which a naive presence check misreads.
        let v = json!({"jsonrpc": "2.0", "id": 0, "error": null, "result": {"n": 1}});
        assert_eq!(extract_result::<Thing>("m", v).unwrap(), Thing { n: 1 });
    }

    #[test]
    fn rpc_errors_include_the_data_field() {
        let v = json!({
            "jsonrpc": "2.0", "id": 0,
            "error": {"code": -32603, "message": "Internal error", "data": "height 99 is not available"}
        });
        let err = extract_result::<Thing>("block", v).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("Internal error"), "{msg}");
        assert!(msg.contains("height 99 is not available"), "{msg}");
    }

    #[test]
    fn a_missing_result_is_malformed_not_a_panic() {
        let v = json!({"jsonrpc": "2.0", "id": 0});
        assert!(matches!(
            extract_result::<Thing>("m", v),
            Err(RpcError::Malformed { .. })
        ));
    }
}
