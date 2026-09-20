//! Applying a confirmed database restore at startup.
//!
//! `pending_restore.db` in the data directory is the confirmed restore (see
//! `Database::stage_restore` and `Database::admin_apply_staged_restore`). The
//! process that finds it, before it opens the database, snapshots the live
//! database and swaps the restored file in.
//!
//! Two properties matter more than anything else here:
//!
//! * A restore that can't be applied must never stop the process from
//!   starting. The service runs under `Restart=always`, so exiting with the
//!   pending file still in place is a crash loop that also takes the web admin
//!   (the only place to fix it) down. On any failure the live database is left
//!   as it was, the pending file is set aside as `pending_restore.failed.db`
//!   and the outcome is reported to the caller to log.
//! * The live database is replaced with a rename (or a copy to a sibling temp
//!   file, then a rename), never overwritten in place, and its WAL/SHM
//!   sidecars are moved out of the way first, so a crash at any point leaves
//!   the old file with its own WAL or the new file with none, never a torn file
//!   or a stale WAL next to the wrong database.

use std::path::{Path, PathBuf};

use tracing::{info, warn};

use crate::db::Database;
use crate::disk_space::ensure_free_space;
use crate::restore_config::{apply_config, ConfigApplied, PENDING_CONFIG_NAME};

/// How many `pre-restore-safety-*.db` snapshots are kept, the newest first.
/// One is not enough: restoring A and then B would delete the snapshot of the
/// original data, the only copy of it.
pub const SAFETY_SNAPSHOTS_KEPT: usize = 3;

const PENDING_NAME: &str = "pending_restore.db";
const FAILED_NAME: &str = "pending_restore.failed.db";
const APPLIED_NAME: &str = "pending_restore.applied.db";
const SNAPSHOT_PREFIX: &str = "pre-restore-safety-";
const SNAPSHOT_SUFFIX: &str = ".db";
const ASIDE_SUFFIX: &str = ".restore-aside";
const SIDECARS: [&str; 2] = ["-wal", "-shm"];

/// What became of the settings that came with a restored backup.
#[derive(Debug)]
pub enum ConfigOutcome {
    /// The restore carried no config (a raw `.db`, an older zip, or the sysop
    /// chose not to restore settings).
    NotIncluded,
    /// The config file was replaced (its machine-specific keys kept). The
    /// caller must check that it loads and, if it does not, revert it with
    /// [`crate::restore_config::revert_config`].
    Applied(ConfigApplied),
    /// The config could not be applied; the running one is unchanged. The
    /// database restore stands.
    Failed(String),
}

/// What [`apply_pending_restore`] did.
#[derive(Debug)]
pub enum ApplyOutcome {
    /// No `pending_restore.db`: the normal case.
    NothingPending,
    /// The restored database is now in place.
    Applied {
        /// The snapshot of the previous live database, if there was one.
        snapshot: Option<PathBuf>,
        /// The previous database could not be opened to checkpoint it (it is
        /// probably damaged, which is often why it is being restored over), so
        /// a byte copy of its `-wal` sidecar sits next to the snapshot as
        /// `pre-restore-safety-<n>.db-wal` and must stay with it.
        snapshot_has_wal_copy: bool,
        /// What happened to the `config.toml` that came with the backup.
        config: ConfigOutcome,
    },
    /// The restore could not be applied. The live database was not changed.
    Rejected {
        /// Why, in a form fit for a log line.
        reason: String,
        /// Where the pending file was moved so it is not retried on every
        /// start, or `None` if even that failed.
        set_aside: Option<PathBuf>,
    },
}

/// Apply `data_dir/pending_restore.db` over the database at `db_path`, if there
/// is one, and then the config that came with it over the one at
/// `config_path` (`None` if the location of the config file is not known).
/// Never exits or panics on a failed restore; see the module docs.
pub async fn apply_pending_restore(
    data_dir: &Path,
    db_path: &Path,
    config_path: Option<&Path>,
) -> ApplyOutcome {
    let pending = data_dir.join(PENDING_NAME);
    let pending_config = data_dir.join(PENDING_CONFIG_NAME);
    if !pending.exists() {
        // A confirmed config belongs to a confirmed database; alone it must
        // never be applied.
        if std::fs::remove_file(&pending_config).is_ok() {
            warn!(
                "discarded a restored config.toml that had no database restore to go with it \
                 (the restart between confirming a restore and applying it may have been \
                 interrupted)"
            );
        }
        return ApplyOutcome::NothingPending;
    }
    info!(path = %pending.display(), "applying staged database restore");

    match apply(data_dir, db_path, &pending).await {
        Ok((snapshot, snapshot_has_wal_copy)) => {
            prune_old_snapshots(data_dir, snapshot.as_deref());
            let config = apply_pending_config(&pending_config, config_path);
            ApplyOutcome::Applied {
                snapshot,
                snapshot_has_wal_copy,
                config,
            }
        }
        Err(reason) => {
            // The settings go with the database that was not restored.
            let _ = std::fs::remove_file(&pending_config);
            ApplyOutcome::Rejected {
                reason,
                set_aside: set_aside(data_dir, &pending, FAILED_NAME),
            }
        }
    }
}

/// Apply the confirmed config, if there is one, and remove it either way so it
/// is not applied again on the next start.
fn apply_pending_config(pending_config: &Path, config_path: Option<&Path>) -> ConfigOutcome {
    if !pending_config.exists() {
        return ConfigOutcome::NotIncluded;
    }
    let outcome = match config_path {
        None => ConfigOutcome::Failed(
            "the location of the config file is not known, so it was not restored".into(),
        ),
        Some(path) => match apply_config(path, pending_config) {
            Ok(applied) => {
                info!(path = %path.display(), "config.toml restored from the backup");
                ConfigOutcome::Applied(applied)
            }
            Err(e) => ConfigOutcome::Failed(e),
        },
    };
    let _ = std::fs::remove_file(pending_config);
    outcome
}

/// Move the pending file to `pending_restore.failed.db` so it isn't retried on
/// every start. `None` if that fails too.
fn set_aside(data_dir: &Path, pending: &Path, name: &str) -> Option<PathBuf> {
    let failed = data_dir.join(name);
    match std::fs::rename(pending, &failed) {
        Ok(()) => Some(failed),
        Err(e) => {
            warn!("could not set the restore file aside: {e}");
            None
        }
    }
}

async fn apply(
    data_dir: &Path,
    db_path: &Path,
    pending: &Path,
) -> Result<(Option<PathBuf>, bool), String> {
    std::fs::metadata(pending).map_err(|e| format!("reading the staged restore file: {e}"))?;

    let mut snapshot = None;
    let mut has_wal_copy = false;
    if db_path.exists() {
        let (path, wal_copy) = snapshot_live(data_dir, db_path).await?;
        info!(path = %path.display(), "pre-restore safety snapshot saved");
        snapshot = Some(path);
        has_wal_copy = wal_copy;
    }

    // The old database's WAL/SHM sidecars belong to the old file. A WAL has no
    // tie to the database it was written for, so left beside the restored file
    // SQLite would replay it onto that file. Move them away before the swap,
    // and put them back if the swap fails so the live database keeps its own.
    // Flush the file first: if that fails nothing has been moved yet.
    sync_file_blocking(pending)
        .await
        .map_err(|e| format!("flushing the staged restore file to disk: {e}"))?;
    let moved = move_sidecars_aside(db_path)?;
    if let Err(e) = install(pending, db_path) {
        restore_sidecars(&moved);
        return Err(e);
    }
    for (_, aside) in &moved {
        let _ = std::fs::remove_file(aside);
    }
    sync_dir(Some(dir_of(db_path)));
    info!("database restore applied");
    Ok((snapshot, has_wal_copy))
}

/// Rename `<db>-wal` and `<db>-shm` to `<name>.restore-aside`, returning the
/// `(original, moved)` pairs. If any rename fails the ones already done are
/// undone and the restore is refused.
fn move_sidecars_aside(db_path: &Path) -> Result<Vec<(PathBuf, PathBuf)>, String> {
    let mut moved: Vec<(PathBuf, PathBuf)> = Vec::new();
    for ext in SIDECARS {
        let original = sidecar(db_path, ext);
        if !original.exists() {
            continue;
        }
        let aside = sibling_with_suffix(&original, ASIDE_SUFFIX);
        if let Err(e) = std::fs::rename(&original, &aside) {
            restore_sidecars(&moved);
            return Err(format!(
                "could not move the live database's {ext} file out of the way: {e}"
            ));
        }
        moved.push((original, aside));
    }
    Ok(moved)
}

fn restore_sidecars(moved: &[(PathBuf, PathBuf)]) {
    for (original, aside) in moved {
        if let Err(e) = std::fs::rename(aside, original) {
            warn!(
                path = %original.display(),
                "could not put the live database's sidecar back: {e}"
            );
        }
    }
}

/// Copy the live database to a new `pre-restore-safety-<unix-seconds>.db`.
/// Returns the path and whether a copy of the WAL sidecar had to go with it.
///
/// Refuses (an `Err`) if another process holds the database open: the
/// checkpoint can't finish, the copy would race that process's writes, and the
/// swap would leave it writing to a file that is no longer the database.
async fn snapshot_live(data_dir: &Path, db_path: &Path) -> Result<(PathBuf, bool), String> {
    let wal_path = sidecar(db_path, "-wal");
    let live_len = file_len(db_path) + file_len(&wal_path);
    ensure_free_space(data_dir, live_len)?;

    // A restart through `std::process::exit` skips the checkpoint a clean
    // connection close would run, and `wal_autocheckpoint` is raised to 10000
    // pages, so recent commits sitting only in the WAL are the normal case. A
    // plain copy of the main file is only complete once they are folded in.
    let wal_copy_needed = match Database::checkpoint_wal(&db_path.to_string_lossy()).await {
        Ok(true) => false,
        Ok(false) => {
            return Err(
                "the live database is in use by another process, so it cannot be \
                        snapshotted safely; stop the other process and stage the restore again"
                    .into(),
            );
        }
        Err(e) if is_busy_or_locked(&e) => {
            return Err(format!(
                "the live database is busy or locked ({e}), so it cannot be snapshotted \
                 safely; stop whatever is using it and stage the restore again"
            ));
        }
        Err(e) => {
            warn!(
                "could not open the live database to checkpoint it (it may be damaged): {e}; \
                 snapshotting its files as they are"
            );
            wal_path.exists()
        }
    };

    let safety = next_snapshot_path(data_dir);
    let safety_wal = sidecar(&safety, "-wal");
    let copied = (|| -> Result<(), String> {
        copy_durably(db_path, &safety)
            .map_err(|e| format!("could not snapshot the live database: {e}"))?;
        if wal_copy_needed {
            copy_durably(&wal_path, &safety_wal)
                .map_err(|e| format!("could not copy the live database's WAL: {e}"))?;
        }
        Ok(())
    })();
    match copied {
        Ok(()) => {
            sync_dir(Some(data_dir));
            Ok((safety, wal_copy_needed))
        }
        Err(e) => {
            let _ = std::fs::remove_file(&safety);
            let _ = std::fs::remove_file(&safety_wal);
            Err(e)
        }
    }
}

/// Copy `from` to `to` through a `.partial` name and flush it to disk before it
/// takes its final name, so a crash never leaves a torn file that looks like a
/// finished snapshot (and would be counted as one when pruning).
fn copy_durably(from: &Path, to: &Path) -> std::io::Result<()> {
    let partial = sibling_with_suffix(to, ".partial");
    let result = (|| {
        std::fs::copy(from, &partial)?;
        std::fs::File::open(&partial)?.sync_all()?;
        std::fs::rename(&partial, to)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&partial);
    }
    result
}

/// Flush a directory's entries (renames, new files) to disk. Best effort: not
/// every filesystem supports it.
fn sync_dir(dir: Option<&Path>) {
    if let Some(dir) = dir {
        if let Ok(d) = std::fs::File::open(dir) {
            let _ = d.sync_all();
        }
    }
}

/// A `pre-restore-safety-<unix-seconds>.db` path that doesn't exist yet. Two
/// restores in the same second would otherwise share a name.
fn next_snapshot_path(data_dir: &Path) -> PathBuf {
    let mut stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    loop {
        let path = data_dir.join(format!("{SNAPSHOT_PREFIX}{stamp}{SNAPSHOT_SUFFIX}"));
        if !path.exists() && !sidecar(&path, "-wal").exists() {
            return path;
        }
        stamp += 1;
    }
}

/// The directory a path lives in; "." for a bare file name, whose `parent()` is
/// the empty path (which `statvfs` and `open` reject).
pub(crate) fn dir_of(path: &Path) -> &Path {
    match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    }
}

/// Flush a file's contents to disk. A rename of a file whose data is not yet
/// durable can, after a power loss, leave an empty or torn file where the
/// database should be, so a failure here (an I/O error from the disk) stops the
/// rename that would have followed.
pub(crate) fn sync_file(path: &Path) -> std::io::Result<()> {
    std::fs::File::open(path)?.sync_all()
}

/// [`sync_file`] off the async runtime: a restore-sized file can have gigabytes
/// of unwritten pages, and the flush blocks for as long as the disk takes.
pub(crate) async fn sync_file_blocking(path: &Path) -> std::io::Result<()> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || sync_file(&path))
        .await
        .map_err(std::io::Error::other)?
}

/// SQLite reports "busy" and "locked" as errors when another process holds the
/// database mid-recovery: that is a live database, not a damaged one.
fn is_busy_or_locked(e: &crate::db::DbOpenError) -> bool {
    let crate::db::DbOpenError::Connect(sqlx::Error::Database(db)) = e else {
        return false;
    };
    // The primary result code is the low byte (SQLITE_BUSY 5, SQLITE_LOCKED 6),
    // whether sqlx hands back the primary or an extended code.
    db.code()
        .and_then(|c| c.parse::<i64>().ok())
        .is_some_and(|c| matches!(c & 0xff, 5 | 6))
}

/// Put `pending` at `db_path`. A rename is atomic but fails across
/// filesystems (`database.path` and the data directory are configured
/// independently), so that case copies to a temp file beside `db_path` and
/// renames that, which keeps the swap atomic.
fn install(pending: &Path, db_path: &Path) -> Result<(), String> {
    match std::fs::rename(pending, db_path) {
        Ok(()) => Ok(()),
        Err(rename_err) => install_by_copy(pending, db_path)
            .map_err(|e| format!("rename failed ({rename_err}); copy fallback failed: {e}")),
    }
}

fn install_by_copy(pending: &Path, db_path: &Path) -> Result<(), String> {
    let tmp = copy_fallback_tmp(db_path);
    let dir = dir_of(db_path);
    ensure_free_space(dir, file_len(pending))?;
    let swapped = (|| -> std::io::Result<()> {
        std::fs::copy(pending, &tmp)?;
        std::fs::File::open(&tmp)?.sync_all()?;
        std::fs::rename(&tmp, db_path)
    })();
    if let Err(e) = swapped {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.to_string());
    }
    // The database is swapped. If the pending file can't be deleted it would
    // be applied again on the next start, over whatever was written since:
    // retire it under another name instead.
    if let Err(e) = std::fs::remove_file(pending) {
        warn!("could not delete the applied restore file: {e}");
        if let Some(dir) = pending.parent() {
            if set_aside(dir, pending, APPLIED_NAME).is_none() {
                warn!(
                    path = %pending.display(),
                    "the applied restore file is still in place and would be applied again \
                     on the next start; delete it"
                );
            }
        }
    }
    Ok(())
}

/// Delete all but the newest [`SAFETY_SNAPSHOTS_KEPT`] snapshots, counting
/// `just_taken` as one of them whatever its timestamp says.
fn prune_old_snapshots(data_dir: &Path, just_taken: Option<&Path>) {
    let Ok(entries) = std::fs::read_dir(data_dir) else {
        return;
    };
    let names: Vec<String> = entries
        .flatten()
        .filter(|e| Some(e.path().as_path()) != just_taken)
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    let keep_others = SAFETY_SNAPSHOTS_KEPT.saturating_sub(usize::from(just_taken.is_some()));
    for name in snapshots_to_prune(&names, keep_others) {
        let path = data_dir.join(&name);
        for target in [path.clone(), sidecar(&path, "-wal")] {
            match std::fs::remove_file(&target) {
                Ok(()) => info!(path = %target.display(), "pruned old restore safety snapshot"),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    warn!(path = %target.display(), "could not prune old restore safety snapshot: {e}");
                }
            }
        }
    }
}

/// Of the file names in `names`, the snapshots to delete so that only the
/// `keep` newest remain. Only `pre-restore-safety-<digits>.db` names count;
/// anything else, including a name with a non-numeric stamp, is left alone.
fn snapshots_to_prune(names: &[String], keep: usize) -> Vec<String> {
    let mut snaps: Vec<(u64, &String)> = names
        .iter()
        .filter_map(|n| snapshot_stamp(n).map(|s| (s, n)))
        .collect();
    snaps.sort_by(|a, b| b.cmp(a));
    snaps
        .into_iter()
        .skip(keep)
        .map(|(_, n)| n.clone())
        .collect()
}

fn snapshot_stamp(name: &str) -> Option<u64> {
    let digits = name
        .strip_prefix(SNAPSHOT_PREFIX)?
        .strip_suffix(SNAPSHOT_SUFFIX)?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

fn file_len(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

/// The temp file the copy fallback of a swap writes beside the database.
fn copy_fallback_tmp(db_path: &Path) -> PathBuf {
    sibling_with_suffix(db_path, ".restore.tmp")
}

/// The files a restore that died mid-swap can leave beside the database at
/// `db_path`: each live sidecar moved aside, and the copy-fallback temp file.
/// Exact names, so a sweep of these never touches anything else in a directory
/// the database shares.
#[must_use]
pub fn swap_leftover_paths(db_path: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = SIDECARS
        .iter()
        .map(|ext| sibling_with_suffix(&sidecar(db_path, ext), ASIDE_SUFFIX))
        .collect();
    paths.push(copy_fallback_tmp(db_path));
    paths
}

fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    sibling_with_suffix(path, suffix)
}

/// `path` with `suffix` appended to its file name, in the same directory.
pub(crate) fn sibling_with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};

    #[test]
    fn the_leftover_names_are_the_ones_a_swap_creates() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("bbs.sqlite");
        std::fs::write(sidecar(&db, "-wal"), b"w").unwrap();
        std::fs::write(sidecar(&db, "-shm"), b"s").unwrap();
        let moved = move_sidecars_aside(&db).unwrap();
        let mut asides: Vec<PathBuf> = moved.into_iter().map(|(_, aside)| aside).collect();
        let mut leftovers = swap_leftover_paths(&db);
        // The copy fallback's temp file is the last name.
        assert_eq!(leftovers.pop(), Some(copy_fallback_tmp(&db)));
        asides.sort();
        leftovers.sort();
        assert_eq!(asides, leftovers);
    }

    fn s(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| (*n).to_owned()).collect()
    }

    #[test]
    fn stamps_are_read_from_snapshot_names_only() {
        assert_eq!(
            snapshot_stamp("pre-restore-safety-1789869375.db"),
            Some(1789869375)
        );
        for no in [
            "pre-restore-safety-.db",
            "pre-restore-safety-abc.db",
            "pre-restore-safety-12.db-wal",
            "pre-restore-safety-12.sqlite",
            "pending_restore.db",
            "bbs.sqlite",
            "pre-restore-safety-99999999999999999999999.db",
        ] {
            assert_eq!(snapshot_stamp(no), None, "{no}");
        }
    }

    #[test]
    fn the_newest_snapshots_are_kept_by_timestamp_not_by_listing_order() {
        let names = s(&[
            "pre-restore-safety-100.db",
            "pre-restore-safety-9.db",
            "pre-restore-safety-1000.db",
            "pre-restore-safety-20.db",
        ]);
        // Numeric, not lexicographic: 9 < 20 < 100 < 1000.
        let mut pruned = snapshots_to_prune(&names, 2);
        pruned.sort();
        assert_eq!(
            pruned,
            s(&["pre-restore-safety-20.db", "pre-restore-safety-9.db"])
        );
    }

    #[test]
    fn nothing_is_pruned_at_or_under_the_limit_and_unrelated_files_are_never_pruned() {
        let names = s(&[
            "pre-restore-safety-1.db",
            "pre-restore-safety-2.db",
            "pre-restore-safety-3.db",
            "pre-restore-safety-abc.db",
            "bbs.sqlite",
            "pending_restore.failed.db",
        ]);
        assert!(snapshots_to_prune(&names, 3).is_empty());
        assert_eq!(
            snapshots_to_prune(&names, 1),
            s(&["pre-restore-safety-2.db", "pre-restore-safety-1.db"])
        );
        assert_eq!(snapshots_to_prune(&names, 0).len(), 3);
    }

    /// A database at `path` holding `rows` rows, left in WAL mode with the
    /// commits still in the `-wal` file (nothing checkpoints them).
    async fn make_db(path: &Path, rows: &[&str]) -> sqlx::SqlitePool {
        let opts = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .pragma("wal_autocheckpoint", "10000");
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await
            .unwrap();
        sqlx::query("CREATE TABLE IF NOT EXISTS t (v TEXT)")
            .execute(&pool)
            .await
            .unwrap();
        for r in rows {
            sqlx::query("INSERT INTO t (v) VALUES (?)")
                .bind(*r)
                .execute(&pool)
                .await
                .unwrap();
        }
        pool
    }

    /// The `v` values of the database file at `path`, read through a fresh
    /// connection so a sidecar next to it is replayed.
    async fn rows(path: &Path) -> Vec<String> {
        let opts = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(false);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await
            .unwrap();
        let mut v: Vec<String> = sqlx::query_scalar("SELECT v FROM t")
            .fetch_all(&pool)
            .await
            .unwrap();
        pool.close().await;
        v.sort();
        v
    }

    /// A finished (closed, checkpointed) database file containing `rows`.
    async fn plain_db(path: &Path, rows_in: &[&str]) {
        let pool = make_db(path, rows_in).await;
        pool.close().await;
    }

    fn snapshots(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| snapshot_stamp(n).is_some())
            .collect();
        v.sort();
        v
    }

    #[tokio::test]
    async fn no_pending_file_is_a_no_op() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("bbs.sqlite");
        plain_db(&db, &["live"]).await;
        assert!(matches!(
            apply_pending_restore(dir.path(), &db, None).await,
            ApplyOutcome::NothingPending
        ));
        assert_eq!(rows(&db).await, ["live"]);
        assert!(snapshots(dir.path()).is_empty());
    }

    #[tokio::test]
    async fn a_pending_restore_replaces_the_database_and_keeps_a_snapshot_of_the_old_one() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("bbs.sqlite");
        plain_db(&db, &["live"]).await;
        plain_db(&dir.path().join("pending_restore.db"), &["restored"]).await;

        let out = apply_pending_restore(dir.path(), &db, None).await;
        let ApplyOutcome::Applied {
            snapshot,
            snapshot_has_wal_copy,
            ..
        } = out
        else {
            panic!("{out:?}");
        };
        assert!(!snapshot_has_wal_copy);
        let snapshot = snapshot.expect("a snapshot of the live database");
        assert_eq!(rows(&db).await, ["restored"]);
        assert_eq!(rows(&snapshot).await, ["live"]);
        assert!(!dir.path().join("pending_restore.db").exists());
        assert!(!sidecar(&db, "-wal").exists() && !sidecar(&db, "-shm").exists());
    }

    // Recent commits sitting only in the WAL (nothing closed the connection,
    // as after `std::process::exit`) must be in the snapshot.
    #[tokio::test]
    async fn commits_still_in_the_wal_are_in_the_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("bbs.sqlite");
        let live = make_db(&db, &["a", "b", "c"]).await;
        assert!(
            file_len(&sidecar(&db, "-wal")) > 0,
            "the commits must be in the WAL"
        );
        plain_db(&dir.path().join("pending_restore.db"), &["restored"]).await;

        let out = apply_pending_restore(dir.path(), &db, None).await;
        drop(live);
        let ApplyOutcome::Applied {
            snapshot: Some(snapshot),
            ..
        } = out
        else {
            panic!("{out:?}");
        };
        assert_eq!(rows(&snapshot).await, ["a", "b", "c"]);
        assert_eq!(rows(&db).await, ["restored"]);
        assert!(!sidecar(&db, "-wal").exists());
    }

    // Another process with the database open makes the checkpoint report busy
    // (not an error): the commits still in the WAL can't be folded in, and the
    // swap would leave that process writing to a file that is no longer the
    // database. The restore is refused and nothing is touched.
    #[tokio::test]
    async fn a_database_in_use_by_another_process_refuses_the_restore_and_stays_intact() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("bbs.sqlite");
        let live = make_db(&db, &["a", "b"]).await;
        // Hold a read transaction open on a second connection.
        let reader_opts = SqliteConnectOptions::new()
            .filename(&db)
            .create_if_missing(false);
        let reader_pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(reader_opts)
            .await
            .unwrap();
        let mut reader = reader_pool.acquire().await.unwrap();
        sqlx::query("BEGIN").execute(&mut *reader).await.unwrap();
        let _: Vec<String> = sqlx::query_scalar("SELECT v FROM t")
            .fetch_all(&mut *reader)
            .await
            .unwrap();
        // A commit after the reader's snapshot: the checkpoint cannot finish.
        sqlx::query("INSERT INTO t (v) VALUES ('late')")
            .execute(&live)
            .await
            .unwrap();
        let pending = dir.path().join("pending_restore.db");
        plain_db(&pending, &["restored"]).await;

        let out = apply_pending_restore(dir.path(), &db, None).await;
        let ApplyOutcome::Rejected { reason, set_aside } = out else {
            panic!("{out:?}");
        };
        assert!(reason.contains("in use by another process"), "{reason}");
        assert!(!pending.exists());
        assert!(set_aside.is_some_and(|p| p.exists()));
        assert!(snapshots(dir.path()).is_empty());
        assert_eq!(rows(&db).await, ["a", "b", "late"]);
        assert!(
            sidecar(&db, "-wal").exists(),
            "the live WAL must stay in place"
        );
        assert!(!sibling_with_suffix(&sidecar(&db, "-wal"), ASIDE_SUFFIX).exists());
        drop(reader);
        reader_pool.close().await;
    }

    // A live database that can't even be opened (damaged) is exactly when an
    // operator restores. It must not block the restore, its files are kept
    // as they are (WAL included), and the old WAL must not survive next to
    // the restored file, where SQLite would replay it onto that file.
    #[tokio::test]
    async fn a_damaged_live_database_is_snapshotted_as_is_and_its_stale_wal_removed() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("bbs.sqlite");
        std::fs::write(&db, b"this is not a database, but it is the live file").unwrap();
        std::fs::write(sidecar(&db, "-wal"), b"stale wal bytes").unwrap();
        std::fs::write(sidecar(&db, "-shm"), b"stale shm bytes").unwrap();
        plain_db(&dir.path().join("pending_restore.db"), &["restored"]).await;

        let out = apply_pending_restore(dir.path(), &db, None).await;
        let ApplyOutcome::Applied {
            snapshot: Some(snapshot),
            snapshot_has_wal_copy,
            ..
        } = out
        else {
            panic!("{out:?}");
        };
        assert!(snapshot_has_wal_copy);
        assert_eq!(
            std::fs::read(&snapshot).unwrap(),
            b"this is not a database, but it is the live file"
        );
        assert_eq!(
            std::fs::read(sidecar(&snapshot, "-wal")).unwrap(),
            b"stale wal bytes"
        );
        assert_eq!(rows(&db).await, ["restored"]);
        for ext in ["-wal", "-shm"] {
            assert!(
                !sidecar(&db, ext).exists(),
                "{ext} must not sit beside the restored file"
            );
            assert!(!sibling_with_suffix(&sidecar(&db, ext), ASIDE_SUFFIX).exists());
        }
    }

    // When the swap fails after the snapshot was taken, the live database keeps
    // its own WAL (it was moved aside for the swap and must be put back).
    #[tokio::test]
    async fn a_failed_swap_puts_the_live_databases_wal_back() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("bbs.sqlite");
        let live = make_db(&db, &["a", "b"]).await;
        // A pending "file" that is a directory: the snapshot works, but neither
        // the rename nor the copy fallback can install it.
        std::fs::create_dir(dir.path().join("pending_restore.db")).unwrap();

        let out = apply_pending_restore(dir.path(), &db, None).await;
        let ApplyOutcome::Rejected { reason, .. } = out else {
            panic!("{out:?}");
        };
        assert!(reason.contains("rename failed"), "{reason}");
        assert_eq!(
            snapshots(dir.path()).len(),
            1,
            "the snapshot was already taken"
        );
        assert!(sidecar(&db, "-wal").exists());
        assert!(!sibling_with_suffix(&sidecar(&db, "-wal"), ASIDE_SUFFIX).exists());
        assert_eq!(rows(&db).await, ["a", "b"]);
        drop(live);
    }

    #[tokio::test]
    async fn only_the_newest_snapshots_survive_repeated_restores() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("bbs.sqlite");
        plain_db(&db, &["live"]).await;
        for old in [
            "pre-restore-safety-1.db",
            "pre-restore-safety-2.db",
            "pre-restore-safety-3.db",
        ] {
            std::fs::write(dir.path().join(old), b"old").unwrap();
        }
        // An unrelated file that only looks similar must survive.
        std::fs::write(dir.path().join("pre-restore-safety-notes.txt"), b"keep").unwrap();
        plain_db(&dir.path().join("pending_restore.db"), &["restored"]).await;

        let ApplyOutcome::Applied {
            snapshot: Some(new),
            ..
        } = apply_pending_restore(dir.path(), &db, None).await
        else {
            panic!("restore should apply");
        };
        let left = snapshots(dir.path());
        assert_eq!(left.len(), SAFETY_SNAPSHOTS_KEPT, "{left:?}");
        assert!(left.contains(&new.file_name().unwrap().to_string_lossy().into_owned()));
        assert!(left.contains(&"pre-restore-safety-3.db".to_owned()));
        assert!(left.contains(&"pre-restore-safety-2.db".to_owned()));
        assert!(!left.contains(&"pre-restore-safety-1.db".to_owned()));
        assert!(dir.path().join("pre-restore-safety-notes.txt").exists());
    }

    // SQLite says "busy" or "locked" when another process holds the database
    // mid-recovery. That is a live database, not a damaged one, and swapping the
    // file under it (or copying it mid-write) would be wrong.
    #[derive(Debug)]
    struct FakeSqliteError(&'static str);
    impl std::fmt::Display for FakeSqliteError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "sqlite error {}", self.0)
        }
    }
    impl std::error::Error for FakeSqliteError {}
    impl sqlx::error::DatabaseError for FakeSqliteError {
        fn message(&self) -> &str {
            "fake"
        }
        fn code(&self) -> Option<std::borrow::Cow<'_, str>> {
            Some(self.0.into())
        }
        fn as_error(&self) -> &(dyn std::error::Error + Send + Sync + 'static) {
            self
        }
        fn as_error_mut(&mut self) -> &mut (dyn std::error::Error + Send + Sync + 'static) {
            self
        }
        fn into_error(self: Box<Self>) -> Box<dyn std::error::Error + Send + Sync + 'static> {
            self
        }
        fn kind(&self) -> sqlx::error::ErrorKind {
            sqlx::error::ErrorKind::Other
        }
    }

    fn open_error(code: &'static str) -> crate::db::DbOpenError {
        crate::db::DbOpenError::Connect(sqlx::Error::Database(Box::new(FakeSqliteError(code))))
    }

    #[test]
    fn busy_and_locked_are_told_apart_from_damage() {
        // BUSY 5, LOCKED 6, and extended codes whose low byte is 5 or 6.
        for busy in ["5", "6", "261", "517", "262"] {
            assert!(is_busy_or_locked(&open_error(busy)), "{busy}");
        }
        // NOTADB 26, CORRUPT 11, CANTOPEN 14: the file itself is the problem.
        for damaged in ["26", "11", "14", "not-a-number"] {
            assert!(!is_busy_or_locked(&open_error(damaged)), "{damaged}");
        }
        assert!(!is_busy_or_locked(&crate::db::DbOpenError::Connect(
            sqlx::Error::PoolTimedOut
        )));
        assert!(!is_busy_or_locked(&crate::db::DbOpenError::RoomOrder(
            "x".into()
        )));
    }

    // A bare relative `database.path` has an empty parent, which `statvfs` and
    // `open` reject; it must mean the current directory.
    #[test]
    fn a_bare_file_name_lives_in_the_current_directory() {
        assert_eq!(dir_of(Path::new("bbs.sqlite")), Path::new("."));
        assert_eq!(dir_of(Path::new("data/bbs.sqlite")), Path::new("data"));
        assert_eq!(dir_of(Path::new("/var/lib/x.db")), Path::new("/var/lib"));
        assert_eq!(dir_of(Path::new("/x.db")), Path::new("/"));
    }

    // The snapshot just taken is never pruned, even if older ones carry
    // timestamps from the future (a clock that went backwards).
    #[tokio::test]
    async fn the_new_snapshot_survives_even_if_older_ones_look_newer() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("bbs.sqlite");
        plain_db(&db, &["live"]).await;
        for future in [
            "pre-restore-safety-99999999990.db",
            "pre-restore-safety-99999999991.db",
            "pre-restore-safety-99999999992.db",
        ] {
            std::fs::write(dir.path().join(future), b"x").unwrap();
        }
        plain_db(&dir.path().join("pending_restore.db"), &["restored"]).await;
        let ApplyOutcome::Applied {
            snapshot: Some(new),
            ..
        } = apply_pending_restore(dir.path(), &db, None).await
        else {
            panic!("restore should apply");
        };
        assert!(new.exists());
        assert_eq!(snapshots(dir.path()).len(), SAFETY_SNAPSHOTS_KEPT);
    }

    #[tokio::test]
    async fn restoring_with_no_live_database_just_installs_it() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("bbs.sqlite");
        plain_db(&dir.path().join("pending_restore.db"), &["restored"]).await;
        let out = apply_pending_restore(dir.path(), &db, None).await;
        assert!(
            matches!(out, ApplyOutcome::Applied { snapshot: None, .. }),
            "{out:?}"
        );
        assert_eq!(rows(&db).await, ["restored"]);
    }

    // A failure must not stop startup or touch the live database, and the
    // pending file must not be retried forever.
    #[tokio::test]
    async fn a_snapshot_failure_rejects_the_restore_without_touching_anything() {
        let dir = tempfile::tempdir().unwrap();
        // "database" is a directory: it exists, but can be neither
        // checkpointed nor copied as a file.
        let db = dir.path().join("bbs.sqlite");
        std::fs::create_dir(&db).unwrap();
        plain_db(&dir.path().join("pending_restore.db"), &["restored"]).await;

        let out = apply_pending_restore(dir.path(), &db, None).await;
        let ApplyOutcome::Rejected { reason, set_aside } = out else {
            panic!("{out:?}");
        };
        assert!(reason.contains("snapshot"), "{reason}");
        assert!(db.is_dir(), "the live path must be untouched");
        assert!(!dir.path().join("pending_restore.db").exists());
        let aside = set_aside.expect("the pending file is set aside");
        assert_eq!(aside, dir.path().join("pending_restore.failed.db"));
        assert_eq!(rows(&aside).await, ["restored"]);
        assert!(
            snapshots(dir.path()).is_empty(),
            "no half-made snapshot may remain"
        );

        // With the pending file set aside, the next start is a plain start.
        assert!(matches!(
            apply_pending_restore(dir.path(), &db, None).await,
            ApplyOutcome::NothingPending
        ));
    }

    #[tokio::test]
    async fn a_swap_failure_rejects_the_restore_and_sets_the_pending_file_aside() {
        let dir = tempfile::tempdir().unwrap();
        let pending = dir.path().join("pending_restore.db");
        plain_db(&pending, &["restored"]).await;
        // database.path in a directory that does not exist: neither the rename
        // nor the copy fallback can put the file there.
        let bad_db = dir.path().join("no-such-dir").join("bbs.sqlite");

        let out = apply_pending_restore(dir.path(), &bad_db, None).await;
        let ApplyOutcome::Rejected { reason, set_aside } = out else {
            panic!("{out:?}");
        };
        assert!(reason.contains("rename failed"), "{reason}");
        assert!(!pending.exists());
        assert_eq!(rows(&set_aside.unwrap()).await, ["restored"]);
        assert!(!sibling_with_suffix(&bad_db, ".restore.tmp").exists());
    }

    #[tokio::test]
    async fn the_copy_fallback_swaps_atomically_and_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("bbs.sqlite");
        plain_db(&db, &["live"]).await;
        let pending = dir.path().join("pending_restore.db");
        plain_db(&pending, &["restored"]).await;

        install_by_copy(&pending, &db).unwrap();

        assert_eq!(rows(&db).await, ["restored"]);
        assert!(!pending.exists());
        assert!(!sibling_with_suffix(&db, ".restore.tmp").exists());
    }

    #[test]
    fn the_copy_fallback_cleans_up_when_the_copy_fails() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("bbs.sqlite");
        std::fs::write(&db, b"live").unwrap();
        // The pending file is missing, so the copy fails part-way in.
        let err = install_by_copy(&dir.path().join("absent.db"), &db).unwrap_err();
        assert!(!err.is_empty());
        assert_eq!(std::fs::read(&db).unwrap(), b"live");
        assert!(!sibling_with_suffix(&db, ".restore.tmp").exists());
    }

    // ── settings that come with the backup ────────────────────────────────

    fn stage_config_file(dir: &Path, text: &str) {
        std::fs::write(dir.join(PENDING_CONFIG_NAME), text).unwrap();
    }

    #[tokio::test]
    async fn the_config_from_the_backup_is_applied_after_the_database() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("bbs.sqlite");
        plain_db(&db, &["live"]).await;
        plain_db(&dir.path().join("pending_restore.db"), &["restored"]).await;
        let cfg = dir.path().join("config.toml");
        std::fs::write(&cfg, "[bbs]\nname = \"Old Name\"\ndata_dir = \"/here\"\n").unwrap();
        stage_config_file(
            dir.path(),
            "[bbs]\nname = \"Restored Name\"\ndata_dir = \"/elsewhere\"\n",
        );

        let out = apply_pending_restore(dir.path(), &db, Some(&cfg)).await;
        let ApplyOutcome::Applied {
            config: ConfigOutcome::Applied(applied),
            ..
        } = out
        else {
            panic!("{out:?}");
        };
        let text = std::fs::read_to_string(&cfg).unwrap();
        assert!(text.contains("name = \"Restored Name\""), "{text}");
        assert!(
            text.contains("data_dir = \"/here\""),
            "machine-specific keys stay: {text}"
        );
        assert!(applied.previous.is_some_and(|p| p.exists()));
        assert!(
            !dir.path().join(PENDING_CONFIG_NAME).exists(),
            "the confirmed config is consumed so it can't be applied twice"
        );
        assert_eq!(rows(&db).await, ["restored"]);
    }

    #[tokio::test]
    async fn a_restore_without_a_config_leaves_the_config_alone() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("bbs.sqlite");
        plain_db(&db, &["live"]).await;
        plain_db(&dir.path().join("pending_restore.db"), &["restored"]).await;
        let cfg = dir.path().join("config.toml");
        std::fs::write(&cfg, "[bbs]\nname = \"Mine\"\n").unwrap();

        let out = apply_pending_restore(dir.path(), &db, Some(&cfg)).await;
        assert!(
            matches!(
                out,
                ApplyOutcome::Applied {
                    config: ConfigOutcome::NotIncluded,
                    ..
                }
            ),
            "{out:?}"
        );
        assert_eq!(
            std::fs::read_to_string(&cfg).unwrap(),
            "[bbs]\nname = \"Mine\"\n"
        );
    }

    // The settings are secondary: if they can't be applied the data restore
    // stands and the running config is untouched.
    #[tokio::test]
    async fn a_config_that_cannot_be_applied_does_not_undo_the_database_restore() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("bbs.sqlite");
        plain_db(&db, &["live"]).await;
        plain_db(&dir.path().join("pending_restore.db"), &["restored"]).await;
        let cfg = dir.path().join("config.toml");
        std::fs::write(&cfg, "[bbs]\nname = \"Mine\"\n").unwrap();
        stage_config_file(dir.path(), "[[[ not toml");

        let out = apply_pending_restore(dir.path(), &db, Some(&cfg)).await;
        assert!(
            matches!(
                out,
                ApplyOutcome::Applied {
                    config: ConfigOutcome::Failed(_),
                    ..
                }
            ),
            "{out:?}"
        );
        assert_eq!(rows(&db).await, ["restored"]);
        assert_eq!(
            std::fs::read_to_string(&cfg).unwrap(),
            "[bbs]\nname = \"Mine\"\n"
        );
        assert!(!dir.path().join(PENDING_CONFIG_NAME).exists());
    }

    #[tokio::test]
    async fn an_unknown_config_location_is_reported_not_guessed() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("bbs.sqlite");
        plain_db(&db, &["live"]).await;
        plain_db(&dir.path().join("pending_restore.db"), &["restored"]).await;
        stage_config_file(dir.path(), "[bbs]\nname = \"X\"\n");

        let out = apply_pending_restore(dir.path(), &db, None).await;
        assert!(
            matches!(
                out,
                ApplyOutcome::Applied {
                    config: ConfigOutcome::Failed(_),
                    ..
                }
            ),
            "{out:?}"
        );
    }

    // A failed database restore must not apply the settings that went with it,
    // and a config with no database restore behind it is never applied.
    #[tokio::test]
    async fn the_config_is_never_applied_without_its_database() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.toml");
        std::fs::write(&cfg, "[bbs]\nname = \"Mine\"\n").unwrap();

        // Database restore fails (database.path in a missing directory).
        let bad_db = dir.path().join("no-such-dir").join("bbs.sqlite");
        plain_db(&dir.path().join("pending_restore.db"), &["restored"]).await;
        stage_config_file(dir.path(), "[bbs]\nname = \"Restored\"\n");
        let out = apply_pending_restore(dir.path(), &bad_db, Some(&cfg)).await;
        assert!(matches!(out, ApplyOutcome::Rejected { .. }), "{out:?}");
        assert!(!dir.path().join(PENDING_CONFIG_NAME).exists());
        assert_eq!(
            std::fs::read_to_string(&cfg).unwrap(),
            "[bbs]\nname = \"Mine\"\n"
        );

        // An orphan config with nothing to restore is discarded, not applied.
        stage_config_file(dir.path(), "[bbs]\nname = \"Orphan\"\n");
        let out = apply_pending_restore(dir.path(), &bad_db, Some(&cfg)).await;
        assert!(matches!(out, ApplyOutcome::NothingPending), "{out:?}");
        assert!(!dir.path().join(PENDING_CONFIG_NAME).exists());
        assert_eq!(
            std::fs::read_to_string(&cfg).unwrap(),
            "[bbs]\nname = \"Mine\"\n"
        );
    }

    #[test]
    fn sibling_keeps_the_directory_and_appends_to_the_name() {
        assert_eq!(
            sibling_with_suffix(Path::new("/data/up.zip"), ".extract.tmp"),
            Path::new("/data/up.zip.extract.tmp")
        );
    }
}
