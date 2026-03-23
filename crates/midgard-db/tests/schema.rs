//! Integration tests against a real TimescaleDB.
//!
//! Skipped unless `MIDGARD_TEST_DB` is set, so `cargo test` stays useful without a database:
//!
//! ```sh
//! docker compose up -d pg
//! MIDGARD_TEST_DB=postgres://midgard:password@localhost:5433/midgard cargo test -p midgard-db
//! ```
//!
//! Every test here drops and rebuilds the schema, so point it at a throwaway database.

use midgard_config::TimeScale;
use midgard_db::{block_log, ddl, tables, Db};
use sqlx::Row;

/// Parse the test DSN into a `TimeScale`, or return `None` to skip.
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

/// Every test in this file drops and rebuilds the one shared schema, so they cannot overlap.
/// Serialising here rather than telling people to pass `--test-threads=1` — a flag that will be
/// forgotten exactly once, on the run where it matters.
static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Connect and take the lock, or bail out of the test if there is no database configured.
/// Hold the returned guard for the duration of the test.
macro_rules! db_or_skip {
    () => {
        match test_config() {
            Some(cfg) => {
                let guard = DB_LOCK.lock().await;
                (Db::connect(&cfg).await.expect("connect"), guard)
            }
            None => {
                eprintln!("MIDGARD_TEST_DB not set, skipping");
                return;
            }
        }
    };
}

/// Get back to the state a first-ever start sees. The tests share one database, so a test that
/// cares about the no-schema case has to put it there itself.
async fn drop_schema(db: &Db) {
    sqlx::query("DROP SCHEMA IF EXISTS midgard CASCADE")
        .execute(db.pool())
        .await
        .unwrap();
}

/// An empty schema, guaranteed.
///
/// `ensure_schema` alone is not enough: it is a no-op when the fingerprint already matches, so a
/// test that inserts rows would inherit whatever the previously-run test left behind and collide
/// on `block_log`'s primary key. Which test that is depends on the order the harness picks, so
/// the symptom is an intermittent failure rather than a reliable one.
async fn fresh_schema(db: &Db) {
    drop_schema(db).await;
    ddl::ensure_schema(db, false).await.unwrap();
}

#[tokio::test]
async fn schema_applies_and_is_idempotent() {
    let (db, _guard) = db_or_skip!();
    drop_schema(&db).await;

    // First run creates it.
    assert!(
        ddl::ensure_schema(&db, false).await.unwrap(),
        "first run should create"
    );
    // Second run sees a matching fingerprint and leaves it alone.
    assert!(
        !ddl::ensure_schema(&db, false).await.unwrap(),
        "second run should be a no-op"
    );

    assert!(db.ping().await);
}

#[tokio::test]
async fn every_declared_table_exists_as_a_hypertable() {
    let (db, _guard) = db_or_skip!();
    fresh_schema(&db).await;

    for table in tables::rollback_tables() {
        let is_hypertable: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM timescaledb_information.hypertables
             WHERE hypertable_schema = 'midgard' AND hypertable_name = $1)",
        )
        .bind(table)
        .fetch_one(db.pool())
        .await
        .unwrap();
        assert!(is_hypertable, "{table} is not a hypertable");
    }
}

#[tokio::test]
async fn a_stale_fingerprint_triggers_a_rebuild() {
    let (db, _guard) = db_or_skip!();
    fresh_schema(&db).await;

    // Leave a marker behind so we can tell whether the rebuild really happened.
    sqlx::query("INSERT INTO block_log (height, timestamp, hash) VALUES (1, 1, '\\x00')")
        .execute(db.pool())
        .await
        .unwrap();

    sqlx::query("UPDATE constants SET value = $1 WHERE key = 'ddl_fingerprint'")
        .bind(b"not-the-right-fingerprint".as_slice())
        .execute(db.pool())
        .await
        .unwrap();

    assert!(
        ddl::ensure_schema(&db, false).await.unwrap(),
        "should rebuild"
    );

    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM block_log")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(rows, 0, "rebuild should have dropped the old data");
}

#[tokio::test]
async fn no_auto_update_refuses_instead_of_rebuilding() {
    let (db, _guard) = db_or_skip!();
    fresh_schema(&db).await;

    sqlx::query("UPDATE constants SET value = $1 WHERE key = 'ddl_fingerprint'")
        .bind(b"stale".as_slice())
        .execute(db.pool())
        .await
        .unwrap();

    let err = ddl::ensure_schema(&db, true).await.unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("no_auto_update_ddl"), "{msg}");

    // And the schema is still the old one, not silently replaced.
    let stored: Vec<u8> = sqlx::query("SELECT value FROM constants WHERE key = 'ddl_fingerprint'")
        .fetch_one(db.pool())
        .await
        .unwrap()
        .get("value");
    assert_eq!(stored, b"stale");
}

#[tokio::test]
async fn block_cursor_reads_back_what_was_written() {
    let (db, _guard) = db_or_skip!();
    fresh_schema(&db).await;

    for (height, ts) in [
        (10i64, 1_000_000_000i64),
        (11, 6_000_000_000),
        (12, 11_000_000_000),
    ] {
        sqlx::query("INSERT INTO block_log (height, timestamp, hash) VALUES ($1, $2, $3)")
            .bind(height)
            .bind(ts)
            .bind(vec![height as u8])
            .execute(db.pool())
            .await
            .unwrap();
    }

    let cursor = block_log::BlockCursor::new();
    cursor.refresh(&db).await.unwrap();

    assert_eq!(cursor.first().height, 10);
    assert_eq!(cursor.last().height, 12);
    assert_eq!(cursor.next_height(), 13);
    // 11e9 ns = 11s, and "now" is one second past the last block.
    assert_eq!(cursor.now_second().to_i64(), 12);

    assert_eq!(block_log::hash_at(&db, 11).await.unwrap(), Some(vec![11u8]));
    assert_eq!(block_log::hash_at(&db, 99).await.unwrap(), None);
}

#[tokio::test]
async fn rolling_back_a_fork_removes_blocks_and_their_events() {
    let (db, _guard) = db_or_skip!();
    fresh_schema(&db).await;

    for (height, ts) in [(10i64, 1_000_000_000i64), (11, 6_000_000_000)] {
        sqlx::query("INSERT INTO block_log (height, timestamp, hash) VALUES ($1, $2, $3)")
            .bind(height)
            .bind(ts)
            .bind(vec![height as u8])
            .execute(db.pool())
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO block_pool_depths (pool, asset_e8, rune_e8, synth_e8, units, block_timestamp)
             VALUES ('BTC.BTC', 1, 2, 0, 3, $1)",
        )
        .bind(ts)
        .execute(db.pool())
        .await
        .unwrap();
    }

    block_log::delete_from_height(&db, 11).await.unwrap();

    let blocks: i64 = sqlx::query_scalar("SELECT count(*) FROM block_log")
        .fetch_one(db.pool())
        .await
        .unwrap();
    let depths: i64 = sqlx::query_scalar("SELECT count(*) FROM block_pool_depths")
        .fetch_one(db.pool())
        .await
        .unwrap();

    assert_eq!(blocks, 1, "block 11 should be gone");
    assert_eq!(depths, 1, "the depth row for block 11 should be gone too");
}
