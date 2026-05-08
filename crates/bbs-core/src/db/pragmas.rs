//! SD-card-tuned SQLite PRAGMAs applied to every new connection.
//!
//! These settings match ADR-0005. We use runtime `sqlx::query` (not
//! `query!`) because PRAGMAs don't map to schema columns and would
//! confuse the compile-time checker.

use sqlx::sqlite::SqliteConnection;

/// Apply the full PRAGMA set to a freshly opened connection.
///
/// Called via the pool's `after_connect` hook so that every
/// connection in both the read pool and the write pool gets the same
/// settings. Failures here are fatal — a connection that doesn't have
/// these settings is not safe to use.
pub async fn apply_pragmas(conn: &mut SqliteConnection) -> Result<(), sqlx::Error> {
    // journal_mode returns a result row; execute and discard.
    sqlx::query("PRAGMA journal_mode = WAL")
        .execute(&mut *conn)
        .await?;
    sqlx::query("PRAGMA synchronous = NORMAL")
        .execute(&mut *conn)
        .await?;
    sqlx::query("PRAGMA cache_size = -8000")
        .execute(&mut *conn)
        .await?;
    sqlx::query("PRAGMA mmap_size = 268435456")
        .execute(&mut *conn)
        .await?;
    sqlx::query("PRAGMA temp_store = MEMORY")
        .execute(&mut *conn)
        .await?;
    sqlx::query("PRAGMA wal_autocheckpoint = 10000")
        .execute(&mut *conn)
        .await?;
    sqlx::query("PRAGMA journal_size_limit = 67108864")
        .execute(&mut *conn)
        .await?;
    sqlx::query("PRAGMA foreign_keys = ON")
        .execute(&mut *conn)
        .await?;
    sqlx::query("PRAGMA busy_timeout = 5000")
        .execute(&mut *conn)
        .await?;
    Ok(())
}
