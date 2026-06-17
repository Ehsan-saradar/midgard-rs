//! API tests against a real database.
//!
//! ```sh
//! MIDGARD_TEST_DB=postgres://midgard:password@localhost:5433/midgard \
//!   cargo test -p midgard-api
//! ```
//!
//! Handlers are exercised through the router rather than called directly, so routing, extractor
//! rejection and status mapping are covered too. The data is written by the test rather than
//! synced, which keeps the assertions exact.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use midgard_api::{router, AppState};
use midgard_chain::thornode::ThorNode;
use midgard_config::{Config, TimeScale};
use midgard_db::block_log::BlockCursor;
use midgard_db::{ddl, Db};
use std::sync::Arc;
use tower::ServiceExt;

const SCHEMA_LOCK_KEY: i64 = 0x_4D49_4447; // "MIDG", shared with the other crates' tests.

/// 2021-01-01T00:00:00Z, and the basis of every block time below.
const T0: i64 = 1_609_459_200;

fn test_config() -> Option<TimeScale> {
    let dsn = std::env::var("MIDGARD_TEST_DB").ok()?;
    let rest = dsn.strip_prefix("postgres://")?;
    let (creds, hostpath) = rest.split_once('@')?;
    let (user, password) = creds.split_once(':')?;
    let (hostport, database) = hostpath.split_once('/')?;
    let (host, port) = hostport.split_once(':')?;
    Some(TimeScale {
        host: host.to_string(),
        port: port.parse().ok()?,
        user_name: user.to_string(),
        password: password.to_string(),
        database: database.split('?').next()?.to_string(),
        sslmode: "disable".to_string(),
        max_open_conns: 4,
        ..TimeScale::default()
    })
}

async fn lock_schema(cfg: &TimeScale) -> sqlx::PgConnection {
    use sqlx::Connection;
    let mut conn = sqlx::PgConnection::connect(&cfg.connection_string())
        .await
        .expect("lock connection");
    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(SCHEMA_LOCK_KEY)
        .execute(&mut conn)
        .await
        .expect("advisory lock");
    conn
}

macro_rules! app_or_skip {
    () => {
        match test_config() {
            Some(cfg) => {
                let guard = lock_schema(&cfg).await;
                let db = Db::connect(&cfg).await.expect("connect");
                sqlx::query("DROP SCHEMA IF EXISTS midgard CASCADE")
                    .execute(db.pool())
                    .await
                    .unwrap();
                ddl::ensure_schema(&db, false).await.unwrap();
                seed(&db).await;

                let cursor = BlockCursor::new();
                cursor.refresh(&db).await.unwrap();

                let config = Arc::new(Config::default());
                // Deliberately a closed port: see `network_survives_thornode_being_unreachable`.
                let thornode = Arc::new(
                    ThorNode::new("http://127.0.0.1:1", std::time::Duration::from_millis(50))
                        .unwrap(),
                );
                let state = AppState::new(db.clone(), cursor, config, thornode);
                (router(state), db, guard)
            }
            None => {
                eprintln!("MIDGARD_TEST_DB not set, skipping");
                return;
            }
        }
    };
}

/// A small, fully known dataset: three blocks, one pool, two swaps, one deposit.
async fn seed(db: &Db) {
    let ns = |s: i64| s * 1_000_000_000;

    for (height, t) in [(1i64, T0), (2, T0 + 3_600), (3, T0 + 7_200)] {
        sqlx::query("INSERT INTO block_log (height, timestamp, hash) VALUES ($1, $2, $3)")
            .bind(height)
            .bind(ns(t))
            .bind(vec![height as u8])
            .execute(db.pool())
            .await
            .unwrap();
    }

    // Depth rises over time so the history endpoint has something to show.
    for (t, asset, rune) in [(T0, 100i64, 400i64), (T0 + 3_600, 110, 500)] {
        sqlx::query(
            "INSERT INTO block_pool_depths
                 (pool, asset_e8, rune_e8, synth_e8, units, block_timestamp)
             VALUES ('BTC.BTC', $1, $2, 0, 50, $3)",
        )
        .bind(asset)
        .bind(rune)
        .bind(ns(t))
        .execute(db.pool())
        .await
        .unwrap();
    }

    sqlx::query(
        "INSERT INTO pool_events (asset, status, event_id, block_timestamp)
         VALUES ('BTC.BTC', 'available', 10, $1)",
    )
    .bind(ns(T0))
    .execute(db.pool())
    .await
    .unwrap();

    // One swap each way, in the second block.
    for (id, dir, from_asset, from_e8, to_asset, to_e8, slip, fee) in [
        (
            20_000_000_001i64,
            0i16,
            "THOR.RUNE",
            100i64,
            "BTC.BTC",
            10i64,
            5i64,
            3i64,
        ),
        (20_000_000_002, 1, "BTC.BTC", 20, "THOR.RUNE", 200, 15, 7),
    ] {
        sqlx::query(
            "INSERT INTO swap_events
                 (tx, chain, from_addr, to_addr, from_asset, from_e8, to_asset, to_e8, memo, pool,
                  to_e8_min, swap_slip_bp, liq_fee_e8, liq_fee_in_rune_e8, _direction,
                  _streaming, streaming_count, streaming_quantity, event_id, block_timestamp)
             VALUES ('TX', 'THOR', 'thor1abc', 'thor1def', $1, $2, $3, $4, '', 'BTC.BTC',
                     0, $5, $6, $6, $7, false, 1, 1, $8, $9)",
        )
        .bind(from_asset)
        .bind(from_e8)
        .bind(to_asset)
        .bind(to_e8)
        .bind(slip)
        .bind(fee)
        .bind(dir)
        .bind(id)
        .bind(ns(T0 + 3_600))
        .execute(db.pool())
        .await
        .unwrap();
    }

    sqlx::query(
        "INSERT INTO stake_events
             (pool, asset_tx, asset_chain, asset_addr, asset_e8, stake_units, rune_tx, rune_addr,
              rune_e8, _asset_in_rune_e8, memo, event_id, block_timestamp)
         VALUES ('BTC.BTC', NULL, 'BTC', 'bc1xyz', 10, 50, NULL, 'thor1abc', 40, 40, '',
                 20000000003, $1)",
    )
    .bind(ns(T0 + 3_600))
    .execute(db.pool())
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO rewards_events (bond_e8, event_id, block_timestamp)
         VALUES (1000, 20000000004, $1)",
    )
    .bind(ns(T0 + 3_600))
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO rewards_event_entries (pool, rune_e8, saver_e8, event_id, block_timestamp)
         VALUES ('BTC.BTC', 25, 0, 20000000004, $1)",
    )
    .bind(ns(T0 + 3_600))
    .execute(db.pool())
    .await
    .unwrap();
}

async fn get(app: &axum::Router, uri: &str) -> (StatusCode, serde_json::Value) {
    let response = app
        .clone()
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

#[tokio::test]
async fn health_reports_the_committed_height() {
    let (app, _db, _g) = app_or_skip!();
    let (status, body) = get(&app, "/v2/health").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["database"], true);
    assert_eq!(body["scannerHeight"], "3");
    assert_eq!(body["lastCommitted"]["height"], 3);
}

#[tokio::test]
async fn pools_reports_depth_and_price() {
    let (app, _db, _g) = app_or_skip!();
    let (status, body) = get(&app, "/v2/pools").await;
    assert_eq!(status, StatusCode::OK);

    let pool = &body.as_array().unwrap()[0];
    assert_eq!(pool["asset"], "BTC.BTC");
    assert_eq!(pool["assetDepth"], "110");
    assert_eq!(pool["runeDepth"], "500");
    // 500 rune / 110 asset.
    assert_eq!(pool["assetPrice"], "4.545454545454546");
    assert_eq!(pool["status"], "available");
}

#[tokio::test]
async fn a_single_pool_matches_the_list_entry() {
    let (app, _db, _g) = app_or_skip!();
    let (status, body) = get(&app, "/v2/pool/BTC.BTC").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["assetDepth"], "110");
}

#[tokio::test]
async fn an_unknown_pool_is_a_404() {
    let (app, _db, _g) = app_or_skip!();
    let (status, body) = get(&app, "/v2/pool/NOPE.NOPE").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body["error"].as_str().unwrap().contains("NOPE.NOPE"));
}

#[tokio::test]
async fn known_pools_maps_asset_to_status() {
    let (app, _db, _g) = app_or_skip!();
    let (status, body) = get(&app, "/v2/knownpools").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["BTC.BTC"], "available");
}

#[tokio::test]
async fn swap_history_totals_both_directions() {
    let (app, _db, _g) = app_or_skip!();
    let (status, body) = get(&app, "/v2/history/swaps?interval=hour&count=3").await;
    assert_eq!(status, StatusCode::OK);

    let meta = &body["meta"];
    assert_eq!(meta["toAssetCount"], "1");
    assert_eq!(meta["toRuneCount"], "1");
    assert_eq!(meta["totalCount"], "2");
    // Volume is the RUNE leg either way: 100 out of rune, 200 into rune.
    assert_eq!(meta["totalVolume"], "300");
    assert_eq!(meta["totalFees"], "10");
    // (5 + 15) / 2 swaps.
    assert_eq!(meta["averageSlip"], "10");
}

#[tokio::test]
async fn depth_history_reports_the_state_at_each_close() {
    let (app, _db, _g) = app_or_skip!();
    let (status, body) = get(&app, "/v2/history/depths/BTC.BTC?interval=hour&count=2").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["meta"]["endAssetDepth"], "110");
    assert_eq!(body["meta"]["endRuneDepth"], "500");
    assert!(body["intervals"].as_array().unwrap().len() <= 2);
}

#[tokio::test]
async fn earnings_splits_fees_and_rewards() {
    let (app, _db, _g) = app_or_skip!();
    let (status, body) = get(&app, "/v2/history/earnings?interval=hour&count=3").await;
    assert_eq!(status, StatusCode::OK);

    let meta = &body["meta"];
    assert_eq!(meta["liquidityFees"], "10");
    assert_eq!(meta["bondingEarnings"], "1000");
    // 10 fees + 25 pool rewards.
    assert_eq!(meta["liquidityEarnings"], "35");
    assert_eq!(meta["earnings"], "1035");
}

#[tokio::test]
async fn liquidity_history_counts_the_deposit() {
    let (app, _db, _g) = app_or_skip!();
    let (status, body) = get(&app, "/v2/history/liquidity_changes?interval=hour&count=3").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["meta"]["addLiquidityCount"], "1");
    // 40 rune + 40 asset-valued-in-rune.
    assert_eq!(body["meta"]["addLiquidityVolume"], "80");
    assert_eq!(body["meta"]["net"], "80");
}

#[tokio::test]
async fn tvl_is_twice_the_rune_depth() {
    let (app, _db, _g) = app_or_skip!();
    let (status, body) = get(&app, "/v2/history/tvl?interval=hour&count=2").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["meta"]["totalValuePooled"], "1000");
}

#[tokio::test]
async fn actions_are_newest_first() {
    let (app, _db, _g) = app_or_skip!();
    let (status, body) = get(&app, "/v2/actions?limit=10").await;
    assert_eq!(status, StatusCode::OK);

    let actions = body["actions"].as_array().unwrap();
    assert_eq!(actions.len(), 3, "two swaps and one deposit");

    let heights: Vec<i64> = actions
        .iter()
        .map(|a| a["height"].as_str().unwrap().parse().unwrap())
        .collect();
    assert!(
        heights.windows(2).all(|w| w[0] >= w[1]),
        "not descending: {heights:?}"
    );
}

#[tokio::test]
async fn actions_can_be_filtered_by_type() {
    let (app, _db, _g) = app_or_skip!();
    let (_, body) = get(&app, "/v2/actions?type=swap").await;
    let actions = body["actions"].as_array().unwrap();
    assert_eq!(actions.len(), 2);
    assert!(actions.iter().all(|a| a["type"] == "swap"));
}

#[tokio::test]
async fn members_lists_addresses_holding_units() {
    let (app, _db, _g) = app_or_skip!();
    let (status, body) = get(&app, "/v2/members").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_array().unwrap(), &[serde_json::json!("thor1abc")]);
}

#[tokio::test]
async fn member_details_reports_the_position() {
    let (app, _db, _g) = app_or_skip!();
    let (status, body) = get(&app, "/v2/member/thor1abc").await;
    assert_eq!(status, StatusCode::OK);

    let pool = &body["pools"][0];
    assert_eq!(pool["pool"], "BTC.BTC");
    assert_eq!(pool["liquidityUnits"], "50");
    assert_eq!(pool["runeAdded"], "40");
}

#[tokio::test]
async fn an_address_with_no_liquidity_is_a_404() {
    let (app, _db, _g) = app_or_skip!();
    let (status, _) = get(&app, "/v2/member/thor1nobody").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn stats_counts_everything_recorded() {
    let (app, _db, _g) = app_or_skip!();
    let (status, body) = get(&app, "/v2/stats").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["swapCount"], "2");
    assert_eq!(body["addLiquidityCount"], "1");
    assert_eq!(body["runeDepth"], "500");
    assert_eq!(body["uniqueSwapperCount"], "1");
}

#[tokio::test]
async fn network_survives_thornode_being_unreachable() {
    // The client points at a closed port on purpose: the pooled figures come from the database
    // and must still be served when the node is down.
    let (app, _db, _g) = app_or_skip!();
    let (status, body) = get(&app, "/v2/network").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["totalPooledRune"], "500");
    assert_eq!(body["activeNodeCount"], "0");
    assert_eq!(body["bondMetrics"]["totalActiveBond"], "0");
}

#[tokio::test]
async fn a_bad_interval_is_a_400_with_the_options() {
    let (app, _db, _g) = app_or_skip!();
    let (status, body) = get(&app, "/v2/history/swaps?interval=fortnight").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("5min"));
}

#[tokio::test]
async fn a_misspelled_parameter_is_rejected_not_ignored() {
    let (app, _db, _g) = app_or_skip!();
    let (status, body) = get(&app, "/v2/history/swaps?intervall=day").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("intervall"));
}

#[tokio::test]
async fn an_out_of_range_count_is_a_400() {
    let (app, _db, _g) = app_or_skip!();
    let (status, _) = get(&app, "/v2/history/swaps?interval=hour&count=9999").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn every_numeric_field_is_a_string() {
    // The property JavaScript clients depend on, checked across a response full of numbers.
    let (app, _db, _g) = app_or_skip!();
    let (_, body) = get(&app, "/v2/pools").await;

    let pool = &body.as_array().unwrap()[0];
    for (key, value) in pool.as_object().unwrap() {
        assert!(
            value.is_string(),
            "{key} came back as {value}, expected a string"
        );
    }
}

#[tokio::test]
async fn unknown_routes_are_404() {
    let (app, _db, _g) = app_or_skip!();
    let (status, _) = get(&app, "/v2/nonexistent").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn stats_is_cached_within_a_block_and_refreshed_across_one() {
    let (app, db, _g) = app_or_skip!();

    let (_, first) = get(&app, "/v2/stats").await;
    assert_eq!(first["swapCount"], "2");

    // Add a swap without advancing the height. The cache is keyed on committed height, so the
    // answer must not change — this is the whole point of the cache.
    sqlx::query(
        "INSERT INTO swap_events
             (tx, chain, from_addr, to_addr, from_asset, from_e8, to_asset, to_e8, memo, pool,
              to_e8_min, swap_slip_bp, liq_fee_e8, liq_fee_in_rune_e8, _direction,
              _streaming, streaming_count, streaming_quantity, event_id, block_timestamp)
         VALUES ('TX2', 'THOR', 'thor1zzz', 'thor1def', 'THOR.RUNE', 1, 'BTC.BTC', 1, '',
                 'BTC.BTC', 0, 1, 1, 1, 0, false, 1, 1, 20000000009, $1)",
    )
    .bind((T0 + 3_600) * 1_000_000_000)
    .execute(db.pool())
    .await
    .unwrap();

    let (_, again) = get(&app, "/v2/stats").await;
    assert_eq!(again["swapCount"], "2", "should still be the cached answer");

    // A new block invalidates it.
    sqlx::query("INSERT INTO block_log (height, timestamp, hash) VALUES (4, $1, '\\x04')")
        .bind((T0 + 10_800) * 1_000_000_000)
        .execute(db.pool())
        .await
        .unwrap();

    // Rebuild the app so its cursor picks up the new height, as the running daemon's would.
    let cfg = test_config().unwrap();
    let cursor = BlockCursor::new();
    cursor.refresh(&db).await.unwrap();
    let thornode = Arc::new(
        ThorNode::new("http://127.0.0.1:1", std::time::Duration::from_millis(50)).unwrap(),
    );
    let fresh = router(AppState::new(
        Db::connect(&cfg).await.unwrap(),
        cursor,
        Arc::new(Config::default()),
        thornode,
    ));

    let (_, after) = get(&fresh, "/v2/stats").await;
    assert_eq!(after["swapCount"], "3", "a new block should refresh it");
}
