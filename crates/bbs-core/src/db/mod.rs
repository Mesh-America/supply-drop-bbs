//! Persistence layer for bbs-core.
//!
//! The entry point is [`Database`]. Obtain one by calling
//! [`Database::open`]; it runs pending migrations and verifies the
//! room walk-order invariant before returning.
//!
//! The store traits ([`UserStore`], [`RoomStore`], [`MessageStore`])
//! are implemented directly on `Database` so callers can use it as a
//! single handle for all persistence operations.

mod admin_queries;
mod audit_store;
mod block_store;
mod credential_store;
mod delivery_store;
mod error;
mod invariants;
mod message_store;
mod node_credential_store;
mod pragmas;
mod room_store;
mod user_store;

pub use error::{DbOpenError, StoreError};
pub use message_store::{MessagePage, MessageStore};
pub use room_store::RoomStore;
pub use user_store::UserStore;

use pragmas::apply_pragmas;
use sqlx::{
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
    Pool, Sqlite,
};
use std::num::NonZeroUsize;
use tracing::info;

/// Top-level database handle.
///
/// Owns a read pool (`cpu_count + 2` connections, opened read-only)
/// and a single-connection write pool. Both implement `Clone + Send +
/// Sync` because `Pool<Sqlite>` does.
///
/// Implements [`UserStore`], [`RoomStore`], and [`MessageStore`]
/// directly. The internal `CredentialStore` is not part of the
/// public store trait surface.
#[derive(Clone)]
pub struct Database {
    pub(crate) read_pool: Pool<Sqlite>,
    pub(crate) write_pool: Pool<Sqlite>,
}

impl Database {
    /// Open the database at `path`, apply PRAGMAs to every connection,
    /// run pending migrations, and verify the room walk-order invariant.
    ///
    /// Creates the file if it does not exist.
    pub async fn open(path: &str) -> Result<Self, DbOpenError> {
        let cpu_count = std::thread::available_parallelism()
            .map(NonZeroUsize::get)
            .unwrap_or(4);

        // Write pool is opened first so the file is created before the
        // read pool attempts to open it read-only (SQLite cannot create a
        // file when opened read-only, even with create_if_missing).
        let write_opts = base_opts(path);
        let write_pool = SqlitePoolOptions::new()
            .max_connections(1)
            .after_connect(|conn, _meta| Box::pin(apply_pragmas(conn)))
            .connect_with(write_opts)
            .await?;

        let read_opts = base_opts(path).read_only(true);
        let read_pool = SqlitePoolOptions::new()
            .max_connections((cpu_count + 2) as u32)
            .after_connect(|conn, _meta| Box::pin(apply_pragmas(conn)))
            .connect_with(read_opts)
            .await?;

        info!("running pending migrations");
        sqlx::migrate!("./migrations").run(&write_pool).await?;

        info!("verifying room walk-order invariant");
        invariants::verify_room_walk_order(&read_pool).await?;

        Ok(Self {
            read_pool,
            write_pool,
        })
    }

    /// Force a WAL checkpoint on the database at `path`, folding any
    /// committed-but-not-yet-checkpointed transactions from its `-wal`
    /// sidecar into the main file. A no-op (returning `true`) if `path`
    /// doesn't exist yet.
    ///
    /// Returns whether the checkpoint completed. `false` means another
    /// connection kept a read open, so the WAL could not be folded in and
    /// truncated (SQLite reports that as a "busy" row, not an error): the main
    /// file alone may be missing recent commits, and a plain copy of it must
    /// not be treated as complete. Waits at most a second for readers.
    ///
    /// Call this before taking a plain-file copy of a database that might
    /// still be running (e.g. a pre-restore safety snapshot in `main.rs`):
    /// `std::fs::copy` of the main file alone can silently miss recent
    /// commits still sitting only in the WAL. This is not a rare edge case
    /// for this project — `apply_pragmas` raises `wal_autocheckpoint` to
    /// 10000 pages, and every restart path here goes through
    /// `std::process::exit`, which (per its own contract) runs no
    /// destructors and therefore skips the checkpoint a clean connection
    /// close would otherwise perform.
    pub async fn checkpoint_wal(path: &str) -> Result<bool, DbOpenError> {
        if !std::path::Path::new(path).exists() {
            return Ok(true);
        }
        let opts = base_opts(path)
            .create_if_missing(false)
            .busy_timeout(std::time::Duration::from_secs(1));
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await?;
        // The pragma answers with one row: (busy, WAL frames, frames checkpointed).
        let busy: i64 = sqlx::query_scalar("PRAGMA wal_checkpoint(TRUNCATE)")
            .fetch_one(&pool)
            .await?;
        pool.close().await;
        Ok(busy == 0)
    }

    /// Borrow the internal credential store.
    ///
    /// Only `bbs-core`'s own auth flow should call this. Plugins have
    /// no route to credential operations.
    #[allow(dead_code)]
    pub(crate) fn credentials(&self) -> credential_store::CredentialStore<'_> {
        credential_store::CredentialStore::new(self)
    }

    /// Borrow the node-credential store (persistent mesh node → user binding).
    pub(crate) fn node_credentials(&self) -> node_credential_store::NodeCredentialStore<'_> {
        node_credential_store::NodeCredentialStore::new(self)
    }
}

fn base_opts(path: &str) -> SqliteConnectOptions {
    path.parse::<SqliteConnectOptions>()
        .expect("path must be a valid SQLite connection string")
        .create_if_missing(true)
        .foreign_keys(true)
}
