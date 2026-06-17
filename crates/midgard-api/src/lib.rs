//! The `/v2` HTTP API.
//!
//! Handlers read the database and nothing else, with one exception: the endpoints backed by
//! THORNode state that never reaches the event stream (`/v2/network`, node counts) call the
//! REST API directly. Those degrade rather than fail when the node is unreachable, because the
//! block pipeline and the API are independent and a broken REST port should not take out
//! `/v2/pools`.

pub mod cache;
pub mod error;
pub mod handlers;
pub mod models;
pub mod query;
pub mod usd;

use std::sync::Arc;

use axum::routing::get;
use axum::Router;
use midgard_chain::thornode::ThorNode;
use midgard_config::Config;
use midgard_db::block_log::BlockCursor;
use midgard_db::Db;
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;

pub use error::{ApiError, ApiResult};

/// Everything a handler needs. Cheap to clone — the expensive parts are behind `Arc`.
#[derive(Clone)]
pub struct AppState {
    pub db: Db,
    pub cursor: BlockCursor,
    pub config: Arc<Config>,
    pub thornode: Arc<ThorNode>,
    /// Chain tip as last seen by the sync loop, for `/v2/health`.
    pub chain_height: Arc<std::sync::atomic::AtomicI64>,
    /// `/v2/stats` scans every event table since genesis, and the answer can only change when a
    /// block lands. Keyed on committed height, so there is no staleness window to tune.
    pub stats_cache: cache::HeightCache<models::Stats>,
}

impl AppState {
    pub fn new(db: Db, cursor: BlockCursor, config: Arc<Config>, thornode: Arc<ThorNode>) -> Self {
        AppState {
            db,
            cursor,
            config,
            thornode,
            chain_height: Arc::new(std::sync::atomic::AtomicI64::new(0)),
            stats_cache: cache::HeightCache::new(),
        }
    }
}

/// Build the router.
pub fn router(state: AppState) -> Router {
    // Midgard is a public read-only API consumed from browsers, so anything may call it.
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    Router::new()
        .route("/", get(handlers::root))
        .route("/v2/health", get(handlers::health::health))
        .route("/v2/pools", get(handlers::pools::pools))
        .route("/v2/pool/{asset}", get(handlers::pools::pool))
        .route("/v2/knownpools", get(handlers::pools::known_pools))
        .route(
            "/v2/history/depths/{pool}",
            get(handlers::depths::depth_history),
        )
        .route("/v2/history/swaps", get(handlers::swaps::swap_history))
        .route(
            "/v2/history/earnings",
            get(handlers::earnings::earnings_history),
        )
        .route(
            "/v2/history/liquidity_changes",
            get(handlers::liquidity::liquidity_history),
        )
        .route("/v2/history/tvl", get(handlers::tvl::tvl_history))
        .route("/v2/actions", get(handlers::actions::actions))
        .route("/v2/members", get(handlers::members::members))
        .route("/v2/member/{addr}", get(handlers::members::member_details))
        .route("/v2/network", get(handlers::network::network))
        .route("/v2/stats", get(handlers::stats::stats))
        .layer(cors)
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}
