//! TimescaleDB access.
//!
//! The database is a projection of the chain and nothing else: every row in it can be derived by
//! replaying blocks from genesis. That single property is what licences the schema handling in
//! [`ddl`] — on any mismatch we drop everything and re-sync rather than migrate.

pub mod block_log;
pub mod buckets;
pub mod ddl;
pub mod eventid;
pub mod tables;

use std::time::Duration;

use midgard_config::TimeScale;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{ConnectOptions, PgPool};

pub use buckets::{Buckets, Interval, Window};
pub use eventid::{EventId, Location};

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("connecting to the database: {0}")]
    Connect(#[source] sqlx::Error),

    #[error("query failed: {0}")]
    Query(#[from] sqlx::Error),

    #[error("schema: {0}")]
    Schema(String),
}

impl From<DbError> for midgard_core::Error {
    fn from(e: DbError) -> midgard_core::Error {
        midgard_core::Error::Internal(e.to_string())
    }
}

/// A handle on the database. Cheap to clone — it is a pool handle underneath.
#[derive(Clone, Debug)]
pub struct Db {
    pool: PgPool,
}

impl Db {
    pub async fn connect(cfg: &TimeScale) -> Result<Db, DbError> {
        let options = PgConnectOptions::new()
            .host(&cfg.host)
            .port(cfg.port)
            .username(&cfg.user_name)
            .password(&cfg.password)
            .database(&cfg.database)
            // Everything Midgard owns lives in the `midgard` schema, and unqualified names in
            // the DDL and in every query rely on this. `public` stays on the path because that
            // is where the timescaledb extension's own functions live.
            .options([("search_path", "midgard,public")])
            // sqlx logs every statement at INFO by default, which at a hundred blocks per commit
            // is louder than the rest of the daemon put together.
            .disable_statement_logging();

        let pool = PgPoolOptions::new()
            .max_connections(cfg.max_open_conns)
            .acquire_timeout(Duration::from_secs(30))
            .connect_with(options)
            .await
            .map_err(DbError::Connect)?;

        Ok(Db { pool })
    }

    /// Wrap an existing pool, for tests that build their own.
    pub fn from_pool(pool: PgPool) -> Db {
        Db { pool }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Is the connection usable right now? Feeds `/v2/health`.
    pub async fn ping(&self) -> bool {
        sqlx::query("SELECT 1").execute(&self.pool).await.is_ok()
    }

    pub async fn close(&self) {
        self.pool.close().await;
    }
}
