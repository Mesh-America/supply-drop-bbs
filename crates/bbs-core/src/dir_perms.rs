//! Restricting a data-holding directory to its owner.
//!
//! The private-file conventions elsewhere in this crate (`restore_stage`'s
//! `new_private_temp`, `backup_bundle`'s and `audit_archive`'s 0600 writes)
//! all assume the *directory* those files live in isn't itself readable by
//! other local users — a 0600 file's contents are safe, but a world-listable
//! parent directory still leaks its name, size, and timestamps, and (for the
//! live database file, which `sqlx`/SQLite create with no mode of their own
//! to control) is the only thing standing between "owner-only" and whatever
//! the process's umask happens to produce. Nothing before this module ever
//! set or checked that assumption; `mkdir`'s default mode, minus umask, is
//! what every `create_dir_all` call for `data_dir` and its siblings
//! (backups, audit archives) actually got.

use std::path::Path;
use tracing::warn;

/// Best-effort: set `path` to owner-only (0700) permissions on Unix.
///
/// Opens the directory itself first (`O_DIRECTORY | O_NOFOLLOW`) and sets
/// the mode on that open handle (`fchmod`, via `File::set_permissions`)
/// rather than calling `std::fs::set_permissions` on the path directly —
/// the same reasoning `restore_stage::hand_fd_to_dir_owner` gives for using
/// `fchown` on an already-open handle: a path-based chmod re-resolves the
/// path from scratch, so a symlink planted at `path` between this
/// directory's creation and this call would otherwise have its *target*
/// silently rechmoded instead of erroring. `O_NOFOLLOW` makes that fail
/// closed (an `Err` this function then also treats as a no-op) rather than
/// operate on the wrong target.
///
/// A no-op (with a warning — this is the sole enforcement of `data_dir`'s
/// confidentiality boundary, not a convenience fallback, so a failure here
/// should be visible rather than swallowed the way this project's other
/// best-effort `fchown` calls are) on a missing/unwritable directory.
///
/// Safe to call on a directory that already existed before this call: it
/// re-applies the mode every time, so a directory created under a looser
/// umask by an older version of this project, or by hand, self-heals on the
/// next call rather than staying loose forever.
///
/// Unix-only, unconditionally — like `restore_stage::hand_fd_to_dir_owner`,
/// no `cfg(not(unix))` fallback: this project targets Linux only (see
/// CLAUDE.md), and other code in this crate already relies on
/// `std::os::unix` without a cfg gate, so a non-Unix stub here would be
/// dead code that could never actually be reached in a build of this crate.
pub fn restrict_to_owner(path: &Path) {
    use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
    let opened = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY)
        .open(path);
    match opened {
        Ok(dir) => {
            if let Err(e) = dir.set_permissions(std::fs::Permissions::from_mode(0o700)) {
                warn!(path = %path.display(), "could not restrict directory to owner-only: {e}");
            }
        }
        Err(e) => {
            warn!(path = %path.display(), "could not open directory to restrict its permissions: {e}");
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    #[test]
    fn sets_an_existing_world_readable_directory_to_owner_only() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        restrict_to_owner(dir.path());
        let mode = std::fs::metadata(dir.path()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "{mode:o}");
    }

    #[test]
    fn a_missing_path_is_a_harmless_no_op() {
        let dir = tempfile::tempdir().unwrap();
        restrict_to_owner(&dir.path().join("does-not-exist"));
    }

    // The O_NOFOLLOW guard: a symlink planted at the target name must be
    // left alone (and, critically, its target's permissions must NOT
    // change), not silently followed and rechmoded.
    #[test]
    fn a_symlink_at_the_target_path_is_not_followed() {
        let outer = tempfile::tempdir().unwrap();
        let victim = outer.path().join("victim");
        std::fs::create_dir(&victim).unwrap();
        std::fs::set_permissions(&victim, std::fs::Permissions::from_mode(0o755)).unwrap();
        let link = outer.path().join("link");
        std::os::unix::fs::symlink(&victim, &link).unwrap();

        restrict_to_owner(&link);

        let mode = std::fs::metadata(&victim).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o755,
            "a symlink must never cause its target's permissions to change: {mode:o}"
        );
    }
}
