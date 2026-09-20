//! Staging a restore from a file on disk: the steps the web upload, the web
//! "restore this backup" button and the `restore stage` CLI command all
//! share, and the rules about which files may be restored.
//!
//! `Database::stage_restore` validates a file and moves it into place, and it
//! consumes (renames, and for a zip rewrites) the file it is given. So every
//! caller must first put a disposable copy of the operator's file in the data
//! directory, keep that copy private, and make sure it is gone again whatever
//! happens. Those steps used to be written out three times and had drifted
//! apart; they live here now.
//!
//! What stays with the callers: who may do this (the web checks the sysop
//! level), the web's in-process lock that keeps a stage and its confirm
//! together, and the audit log (the CLI runs without the database open).
//! There is no cross-process lock between the CLI and a running server: staging
//! twice at once is possible, and the last one to finish wins.

use std::path::{Path, PathBuf};

use crate::db::Database;
use crate::disk_space::ensure_free_space;
use crate::restore_apply::sibling_with_suffix;

/// The largest file the web UI restores, uploaded or picked from the backup
/// list. `supply-drop-bbs restore stage` has no such limit.
pub const WEB_RESTORE_MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// A backup filename is a single path component inside the backup directory:
/// non-empty, with no path separators, no `..`, no `.` and no NUL.
#[must_use]
pub fn backup_filename_is_safe(filename: &str) -> bool {
    !filename.is_empty()
        && filename != "."
        && !filename.contains('/')
        && !filename.contains('\\')
        && !filename.contains("..")
        && !filename.contains('\0')
}

/// The two kinds of file a backup can be: a `.zip` bundle or a legacy `.db`.
/// The backup list, the restore endpoint and the admin UI all use this rule.
#[must_use]
pub fn is_backup_file_name(filename: &str) -> bool {
    filename.ends_with(".db") || filename.ends_with(".zip")
}

/// Why staging a restore failed.
#[derive(Debug, thiserror::Error)]
pub enum StageError {
    /// The source file does not exist.
    #[error("backup not found")]
    NotFound,
    /// The source is a directory, a symlink (where those are refused) or some
    /// other special file.
    #[error("backup is not a regular file")]
    NotRegularFile,
    /// The source is over the caller's size limit.
    #[error("{0}")]
    TooLarge(String),
    /// The disk does not have room for the copy.
    #[error("{0}")]
    NoSpace(String),
    /// The file was copied but is not a restorable database. The message says
    /// what is wrong with it.
    #[error("{0}")]
    Rejected(String),
    /// Reading or copying the file failed.
    #[error("{0}")]
    Io(String),
}

/// A file in the data directory that is removed when this is dropped, best
/// effort, along with the `<name>.extract.tmp` a zip is extracted to beside it.
/// It covers every early return and a request future dropped mid-copy (a
/// client disconnect). After a successful stage the file has been renamed away
/// and the removal is a harmless no-op. A zip extraction already running on a
/// blocking thread when the future is dropped finishes on its own and can
/// still leave its `.extract.tmp` behind; the startup sweep collects that.
#[derive(Debug)]
pub struct TempFile(PathBuf);

impl TempFile {
    /// The path of the file.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
        let _ = std::fs::remove_file(sibling_with_suffix(&self.0, ".extract.tmp"));
    }
}

/// Create a new, empty, owner-only (0600) file `<data_dir>/<prefix><uuid>.tmp`
/// and open it for writing. It is private from the moment it exists: the
/// database inside is as sensitive as the live one (it holds password hashes).
/// The `.tmp` suffix and the `restore_upload_` / `restore_backup_` prefixes are
/// what the startup sweep looks for, so a copy orphaned by a killed process is
/// collected on the next start.
///
/// The file is handed to the data directory's owner where that can be done
/// (best effort, and a no-op for the service user). A `restore stage` run as
/// root with `--data-dir` would otherwise leave a root-owned 0600 file that the
/// service, running as its own user, cannot open.
///
/// # Errors
/// The I/O error from creating the file.
pub async fn new_private_temp(
    data_dir: &Path,
    prefix: &str,
) -> std::io::Result<(TempFile, tokio::fs::File)> {
    let path = data_dir.join(format!("{prefix}{}.tmp", uuid::Uuid::new_v4()));
    let mut opts = tokio::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    opts.mode(0o600);
    let file = opts.open(&path).await?;
    // From here on the guard owns the cleanup.
    let temp = TempFile(path);
    hand_to_dir_owner(data_dir, temp.path());
    Ok((temp, file))
}

/// Give `path` to the owner (user and group) of `dir`, best effort. For the
/// service user this changes nothing; for a `restore stage` run as root it keeps
/// the files the service will read from being root-owned and unreadable to it.
pub(crate) fn hand_to_dir_owner(dir: &Path, path: &Path) {
    use std::os::unix::fs::MetadataExt as _;
    if let Ok(meta) = std::fs::metadata(dir) {
        let _ = std::os::unix::fs::chown(path, Some(meta.uid()), Some(meta.gid()));
    }
}

/// How [`stage_copy`] treats its source.
#[derive(Debug, Clone, Copy)]
pub struct SourceRules {
    /// Refuse a source larger than this.
    pub max_bytes: Option<u64>,
    /// Follow a symlink at the source. The web refuses (the backup directory
    /// is not trusted to point only at backups); an operator naming a file on
    /// the command line means what they typed.
    pub follow_symlinks: bool,
}

/// Open `source` for reading. The checks that follow (regular file, size) are
/// made on the open handle, not on the path, so the file can't be swapped for a
/// symlink or a FIFO between the check and the read. `O_NONBLOCK` keeps a FIFO
/// from blocking the open (it is then refused as not a regular file), and
/// `O_NOFOLLOW` refuses a symlink where following is not allowed.
async fn open_source(source: &Path, follow_symlinks: bool) -> Result<tokio::fs::File, StageError> {
    let mut opts = tokio::fs::OpenOptions::new();
    opts.read(true);
    let mut flags = libc::O_NONBLOCK;
    if !follow_symlinks {
        flags |= libc::O_NOFOLLOW;
    }
    opts.custom_flags(flags);
    match opts.open(source).await {
        Ok(f) => Ok(f),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(StageError::NotFound),
        Err(e) if e.raw_os_error() == Some(libc::ELOOP) => Err(StageError::NotRegularFile),
        Err(e) => Err(StageError::Io(e.to_string())),
    }
}

/// Copy `source` into `data_dir` (private, under a name starting with
/// `temp_prefix`) and stage the copy with [`stage_temp`]. The source is left
/// untouched, and the temporary copy is gone again unless it became the staged
/// file.
///
/// # Errors
/// See [`StageError`].
pub async fn stage_copy(
    source: &Path,
    data_dir: &Path,
    temp_prefix: &str,
    rules: SourceRules,
) -> Result<(), StageError> {
    let mut src = open_source(source, rules.follow_symlinks).await?;
    let meta = src
        .metadata()
        .await
        .map_err(|e| StageError::Io(e.to_string()))?;
    if !meta.is_file() {
        return Err(StageError::NotRegularFile);
    }
    let len = meta.len();
    let too_large = |max: u64| {
        StageError::TooLarge(format!(
            "backup is larger than the {} GiB limit for restoring from the web UI; use \
             `supply-drop-bbs restore stage <file>` instead",
            max / (1024 * 1024 * 1024)
        ))
    };
    if let Some(max) = rules.max_bytes {
        if len > max {
            return Err(too_large(max));
        }
    }
    ensure_free_space(data_dir, len).map_err(StageError::NoSpace)?;

    let (temp, mut dest) = new_private_temp(data_dir, temp_prefix)
        .await
        .map_err(|e| StageError::Io(format!("copying backup: {e}")))?;
    // Capped at the limit whatever the file's size was when it was checked: a
    // file that grows while it is being copied can't outrun the cap.
    let cap = rules.max_bytes.map_or(u64::MAX, |m| m.saturating_add(1));
    let copied = async {
        let mut limited = tokio::io::AsyncReadExt::take(&mut src, cap);
        let n = tokio::io::copy(&mut limited, &mut dest).await?;
        tokio::io::AsyncWriteExt::flush(&mut dest).await?;
        Ok::<u64, std::io::Error>(n)
    }
    .await;
    drop(dest);
    let n = copied.map_err(|e| StageError::Io(format!("copying backup: {e}")))?;
    if let Some(max) = rules.max_bytes {
        if n > max {
            return Err(too_large(max));
        }
    }

    stage_temp(&temp, data_dir).await
}

/// Validate the private copy `temp` and stage it as
/// `<data_dir>/pending_restore.staged.db` (see `Database::stage_restore`). The
/// copy is removed when `temp` drops, so a rejected file leaves nothing behind.
///
/// # Errors
/// [`StageError::Rejected`] with the reason if the file is not restorable.
pub async fn stage_temp(temp: &TempFile, data_dir: &Path) -> Result<(), StageError> {
    Database::stage_restore(temp.path(), data_dir)
        .await
        .map_err(|e| StageError::Rejected(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filename_guard_rejects_empty_dot_dotdot_separators_and_nul() {
        for bad in [
            "", ".", "..", "../x.db", "a/b.db", "a\\b.db", "a\0.db", "x..db",
        ] {
            assert!(!backup_filename_is_safe(bad), "{bad:?} must be rejected");
        }
        for good in [
            "backup_20260101_000000.db",
            "backup_20260101_000000.zip",
            "my backup.db",
        ] {
            assert!(backup_filename_is_safe(good), "{good:?} must be accepted");
        }
    }

    #[test]
    fn only_db_and_zip_names_are_backup_files() {
        for yes in ["a.db", "backup_1.zip", "x.y.db"] {
            assert!(is_backup_file_name(yes), "{yes}");
        }
        for no in [
            "config.toml",
            "backup_1_config.toml",
            "notes.txt",
            "backup_20260101_000000",
            "x.db.tmp",
            "a.zip.bak",
            "",
        ] {
            assert!(!is_backup_file_name(no), "{no}");
        }
    }

    // A dropped request future (client disconnect) runs the guard's Drop,
    // which is what removes the temporary copy in that case.
    #[test]
    fn temp_file_removes_its_file_when_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("restore_backup_x.tmp");
        std::fs::write(&path, b"partial copy").unwrap();
        drop(TempFile(path.clone()));
        assert!(!path.exists());
        // Removing a file that was already renamed away is not an error.
        drop(TempFile(path));
    }

    fn rules() -> SourceRules {
        SourceRules {
            max_bytes: None,
            follow_symlinks: false,
        }
    }

    fn files(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .collect();
        v.sort();
        v
    }

    #[tokio::test]
    async fn a_missing_source_is_not_found_and_nothing_is_created() {
        let src = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let err = stage_copy(
            &src.path().join("absent.db"),
            data.path(),
            "restore_x_",
            rules(),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, StageError::NotFound), "{err:?}");
        assert!(files(data.path()).is_empty());
    }

    #[tokio::test]
    async fn a_directory_is_not_a_regular_file() {
        let src = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        std::fs::create_dir(src.path().join("looks_like.db")).unwrap();
        let err = stage_copy(
            &src.path().join("looks_like.db"),
            data.path(),
            "restore_x_",
            rules(),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, StageError::NotRegularFile), "{err:?}");
        assert!(files(data.path()).is_empty());
    }

    #[tokio::test]
    async fn a_source_over_the_limit_is_refused_before_any_copy() {
        let src = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        std::fs::write(src.path().join("big.db"), vec![0u8; 2048]).unwrap();
        let err = stage_copy(
            &src.path().join("big.db"),
            data.path(),
            "restore_x_",
            SourceRules {
                max_bytes: Some(1024),
                follow_symlinks: false,
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(err, StageError::TooLarge(_)), "{err:?}");
        assert!(files(data.path()).is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlinks_are_followed_only_when_asked() {
        let src = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        std::fs::write(src.path().join("real.db"), b"not a database").unwrap();
        std::os::unix::fs::symlink(src.path().join("real.db"), src.path().join("link.db")).unwrap();

        let refused = stage_copy(
            &src.path().join("link.db"),
            data.path(),
            "restore_x_",
            rules(),
        )
        .await
        .unwrap_err();
        assert!(matches!(refused, StageError::NotRegularFile), "{refused:?}");

        // Followed, the link reaches the file, which is then rejected as a
        // non-database, and that leaves nothing behind.
        let followed = stage_copy(
            &src.path().join("link.db"),
            data.path(),
            "restore_x_",
            SourceRules {
                max_bytes: None,
                follow_symlinks: true,
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(followed, StageError::Rejected(_)), "{followed:?}");
        assert!(files(data.path()).is_empty());
    }

    // A file that is not a database is rejected, the temp copy is removed and
    // the source is untouched.
    #[tokio::test]
    async fn a_rejected_file_leaves_no_copy_behind_and_the_source_alone() {
        let src = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        std::fs::write(
            src.path().join("bogus.db"),
            b"this is not a sqlite database",
        )
        .unwrap();
        let err = stage_copy(
            &src.path().join("bogus.db"),
            data.path(),
            "restore_x_",
            rules(),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, StageError::Rejected(_)), "{err:?}");
        assert!(files(data.path()).is_empty(), "{:?}", files(data.path()));
        assert_eq!(
            std::fs::read(src.path().join("bogus.db")).unwrap(),
            b"this is not a sqlite database"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_temp_file_is_owner_only_from_creation() {
        use std::os::unix::fs::PermissionsExt as _;
        let data = tempfile::tempdir().unwrap();
        let (temp, _file) = new_private_temp(data.path(), "restore_upload_")
            .await
            .unwrap();
        let mode = std::fs::metadata(temp.path()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "{mode:o}");
        let name = temp
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(
            name.starts_with("restore_upload_") && name.ends_with(".tmp"),
            "{name}"
        );
    }

    #[tokio::test]
    async fn two_temp_files_never_share_a_name() {
        let data = tempfile::tempdir().unwrap();
        let (a, _fa) = new_private_temp(data.path(), "restore_cli_").await.unwrap();
        let (b, _fb) = new_private_temp(data.path(), "restore_cli_").await.unwrap();
        assert_ne!(a.path(), b.path());
    }
}
