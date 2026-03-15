//! Schema creation and versioning.
//!
//! There are no migrations here, on purpose. The database holds nothing that is not derivable
//! from the chain, so when the schema we want stops matching the schema that exists, the correct
//! and always-available answer is to drop it and re-sync. Writing migrations for a table that is
//! regenerable would be work that can only introduce bugs.
//!
//! Re-syncing mainnet from genesis is not free, which is why `no_auto_update_ddl` exists: an
//! operator who would rather find out at deploy time than watch a node silently start over can
//! turn the reset into a hard failure.
//!
//! The stored fingerprint is a hash of the DDL text itself rather than a hand-maintained integer,
//! because a hand-maintained integer is a thing you forget to bump.

use sha2::{Digest, Sha256};
use sqlx::Row;

use crate::{Db, DbError};

/// The schema, embedded so the binary needs nothing on disk at runtime.
const DDL: &str = include_str!("../sql/ddl.sql");

const FINGERPRINT_KEY: &str = "ddl_fingerprint";

/// SHA-256 of the DDL, hex encoded.
pub fn fingerprint() -> String {
    let digest = Sha256::digest(DDL.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// Bring the schema up to date, rebuilding it if it does not match.
///
/// Returns `true` when the schema was (re)created, which the caller uses to decide whether the
/// sync cursor has to go back to the start.
pub async fn ensure_schema(db: &Db, no_auto_update: bool) -> Result<bool, DbError> {
    let want = fingerprint();
    let have = stored_fingerprint(db).await?;

    match have.as_deref() {
        Some(existing) if existing == want => {
            tracing::info!(fingerprint = %want, "schema is up to date");
            return Ok(false);
        }
        Some(existing) => {
            if no_auto_update {
                return Err(DbError::Schema(format!(
                    "schema fingerprint is {existing} but this build expects {want}; \
                     no_auto_update_ddl is set, refusing to drop and re-sync"
                )));
            }
            tracing::warn!(
                found = %existing,
                expected = %want,
                "schema is from a different build, dropping and re-syncing from the start"
            );
        }
        None => tracing::info!("no schema found, creating it"),
    }

    apply(db).await?;
    Ok(true)
}

async fn apply(db: &Db) -> Result<(), DbError> {
    // The DDL drops and recreates the schema, and postgres cannot run CREATE EXTENSION or
    // create_hypertable inside a transaction the way we would want, so this runs as a plain
    // multi-statement batch. A failure part way through leaves a half-built schema, which the
    // next start detects as a missing fingerprint and rebuilds.
    sqlx::raw_sql(DDL).execute(db.pool()).await?;

    sqlx::query("INSERT INTO constants (key, value) VALUES ($1, $2)")
        .bind(FINGERPRINT_KEY)
        .bind(fingerprint().as_bytes())
        .execute(db.pool())
        .await?;

    tracing::info!(fingerprint = %fingerprint(), "schema created");
    Ok(())
}

async fn stored_fingerprint(db: &Db) -> Result<Option<String>, DbError> {
    // A missing table is not an error here: it is the ordinary first-run state.
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS (
             SELECT 1 FROM information_schema.tables
             WHERE table_schema = 'midgard' AND table_name = 'constants'
         )",
    )
    .fetch_one(db.pool())
    .await?;
    if !exists {
        return Ok(None);
    }

    let row = sqlx::query("SELECT value FROM constants WHERE key = $1")
        .bind(FINGERPRINT_KEY)
        .fetch_optional(db.pool())
        .await?;

    Ok(row.map(|r| String::from_utf8_lossy(r.get::<Vec<u8>, _>("value").as_slice()).into_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_is_stable_and_hex() {
        let f = fingerprint();
        assert_eq!(f.len(), 64);
        assert!(f.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(f, fingerprint());
    }

    #[test]
    fn ddl_is_embedded_and_looks_like_our_schema() {
        assert!(DDL.contains("CREATE SCHEMA midgard"));
        assert!(DDL.contains("CREATE TABLE block_log"));
        assert!(DDL.contains("CREATE TABLE swap_events"));
    }
}
