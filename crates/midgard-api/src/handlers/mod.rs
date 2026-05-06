//! Request handlers, one module per endpoint group.

pub mod actions;
pub mod depths;
pub mod earnings;
pub mod health;
pub mod liquidity;
pub mod members;
pub mod network;
pub mod pools;
pub mod stats;
pub mod swaps;
pub mod tvl;

use axum::Json;
use serde_json::json;

/// `GET /` — a pointer to the docs, matching upstream's habit of not 404ing the root.
pub async fn root() -> Json<serde_json::Value> {
    Json(json!({
        "docs": "/v2/doc",
        "health": "/v2/health",
        "swagger": "/v2/swagger.json",
    }))
}
