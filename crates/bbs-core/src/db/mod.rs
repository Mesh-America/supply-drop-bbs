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
        // sqlx/SQLite set no mode of their own on a newly created file, so it
        // gets whatever the process's umask leaves — commonly world-readable.
        // This file holds password hashes and message content; restrict it
        // (and its WAL/SHM sidecars — `apply_pragmas` above just enabled WAL
        // mode) on every open, not just first creation, so an existing
        // install with a looser mode self-heals on next startup too.
        restrict_db_files_to_owner(path);

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

/// Best-effort: restrict the live database file and its `-wal`/`-shm`
/// sidecars to owner-only (0600). The sidecars may not exist yet (WAL only
/// creates them once something writes), so a missing candidate is silently
/// skipped; a failure restricting one that DOES exist is a `warn!`, since
/// this is the sole enforcement of the file's confidentiality (not a
/// convenience fallback) and should be visible if it doesn't work, same
/// reasoning as `dir_perms::restrict_to_owner`. Uses the same
/// open-then-`fchmod` (not chmod-by-path) pattern as that function, for the
/// same symlink-race reason.
///
/// Also hands each file to `data_dir`'s owner first, the same way
/// `restore_stage::hand_fd_to_dir_owner`'s own doc comment explains: this
/// function runs on every `open`, including from short-lived CLI
/// subcommands this project's own docs tell operators to run as root — a
/// root-created (or root-`open`'d) database file must not end up
/// root-owned *and* 0600, which would lock the actual service account out
/// of its own database entirely rather than just leaving it too permissive.
fn restrict_db_files_to_owner(path: &str) {
    use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
    let data_dir = std::path::Path::new(path).parent();
    for candidate in [
        path.to_owned(),
        format!("{path}-wal"),
        format!("{path}-shm"),
    ] {
        let opened = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&candidate);
        match opened {
            Ok(f) => {
                if let Some(dir) = data_dir {
                    crate::restore_stage::hand_fd_to_dir_owner(dir, &f);
                }
                if let Err(e) = f.set_permissions(std::fs::Permissions::from_mode(0o600)) {
                    tracing::warn!(path = %candidate, "could not restrict database file to owner-only: {e}");
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                tracing::warn!(path = %candidate, "could not open database file to restrict its permissions: {e}");
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::Database;
    use std::os::unix::fs::PermissionsExt as _;

    #[tokio::test]
    async fn opening_a_database_leaves_the_file_owner_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bbs.sqlite");
        let db = Database::open(&path.to_string_lossy()).await.unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "{mode:o}");
        drop(db);
    }

    // Simulates a database created by an older version of this project
    // (or by hand) under a looser umask: opening it must tighten the mode
    // rather than leave the existing permissions alone.
    #[tokio::test]
    async fn opening_an_existing_world_readable_database_tightens_its_mode() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bbs.sqlite");
        {
            let db = Database::open(&path.to_string_lossy()).await.unwrap();
            drop(db);
        }
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o644, "test setup must actually loosen it first");

        let db = Database::open(&path.to_string_lossy()).await.unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "{mode:o}");
        drop(db);
    }

    // The WAL/SHM sidecars are the flagship reason `restrict_db_files_to_owner`
    // exists (the main file alone isn't the whole story under WAL mode) —
    // simulate a pre-existing, loosely-permissioned pair (e.g. left by an
    // older version of this project) rather than depending on the timing of
    // when SQLite itself would create them.
    #[tokio::test]
    async fn opening_a_database_tightens_pre_existing_wal_and_shm_sidecars_too() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bbs.sqlite");
        {
            let db = Database::open(&path.to_string_lossy()).await.unwrap();
            drop(db);
        }
        let wal = format!("{}-wal", path.to_string_lossy());
        let shm = format!("{}-shm", path.to_string_lossy());
        std::fs::write(&wal, b"fake wal content").unwrap();
        std::fs::write(&shm, b"fake shm content").unwrap();
        for sidecar in [&wal, &shm] {
            std::fs::set_permissions(sidecar, std::fs::Permissions::from_mode(0o644)).unwrap();
        }

        let db = Database::open(&path.to_string_lossy()).await.unwrap();
        for sidecar in [&wal, &shm] {
            let mode = std::fs::metadata(sidecar).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "{sidecar}: {mode:o}");
        }
        drop(db);
    }
}
