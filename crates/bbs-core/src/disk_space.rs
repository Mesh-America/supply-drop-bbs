//! Free-disk-space pre-flight for the operations that write a database-sized
//! file (restore staging, zip extraction, the pre-restore snapshot).
//!
//! A restore that runs out of disk half way leaves a torn file behind, so
//! callers ask first. The answer is best-effort: where the free space can't be
//! read (a non-Linux build, a filesystem that refuses `statvfs`) the check
//! passes rather than refusing to work.

use std::path::Path;

/// Slack kept free on top of the file being written, for SQLite sidecars and
/// whatever else is writing to the same disk.
pub const HEADROOM_BYTES: u64 = 16 * 1024 * 1024;

/// The space (bytes) available to unprivileged writers on the filesystem
/// holding `path`, or `None` if it can't be read. `path` must exist.
#[must_use]
#[cfg(target_os = "linux")]
pub fn available_bytes(path: &Path) -> Option<u64> {
    let v = rustix::fs::statvfs(path).ok()?;
    // f_bavail counts blocks of f_frsize bytes (not f_bsize) available to
    // non-root, which is what a service user actually gets.
    v.f_bavail.checked_mul(v.f_frsize)
}

/// See the Linux version.
#[must_use]
#[cfg(not(target_os = "linux"))]
pub fn available_bytes(_path: &Path) -> Option<u64> {
    None
}

/// Fail with a readable message if the filesystem holding `dir` does not have
/// room for `needed` more bytes plus [`HEADROOM_BYTES`]. Passes when the free
/// space can't be determined (see the module docs).
///
/// # Errors
/// A message naming the directory and the shortfall.
pub fn ensure_free_space(dir: &Path, needed: u64) -> Result<(), String> {
    check(available_bytes(dir), dir, needed)
}

fn check(available: Option<u64>, dir: &Path, needed: u64) -> Result<(), String> {
    let Some(available) = available else {
        return Ok(());
    };
    let required = needed.saturating_add(HEADROOM_BYTES);
    if available >= required {
        return Ok(());
    }
    Err(format!(
        "not enough free disk space in {}: need about {} MiB, {} MiB available",
        dir.display(),
        required.div_ceil(1024 * 1024),
        available / (1024 * 1024)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: u64 = 1024 * 1024;

    #[test]
    fn enough_space_passes_and_a_shortfall_names_the_numbers() {
        let dir = Path::new("/data");
        assert!(check(Some(100 * MIB), dir, 50 * MIB).is_ok());
        // Exactly the file plus headroom is enough.
        assert!(check(Some(50 * MIB + HEADROOM_BYTES), dir, 50 * MIB).is_ok());
        let err = check(Some(50 * MIB), dir, 50 * MIB).unwrap_err();
        assert!(err.contains("/data"), "{err}");
        assert!(err.contains("66 MiB"), "{err}");
        assert!(err.contains("50 MiB available"), "{err}");
    }

    #[test]
    fn unknown_free_space_does_not_block() {
        assert!(check(None, Path::new("/data"), u64::MAX).is_ok());
    }

    #[test]
    fn a_huge_request_cannot_overflow() {
        assert!(check(Some(1), Path::new("/d"), u64::MAX).is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_real_directory_reports_its_free_space() {
        let dir = tempfile::tempdir().unwrap();
        let free = available_bytes(dir.path()).expect("statvfs on a temp dir");
        assert!(free > 0);
        // More than any disk can hold is refused, whatever the machine.
        assert!(ensure_free_space(dir.path(), u64::MAX / 2).is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_missing_directory_is_unknown_not_an_error() {
        assert_eq!(available_bytes(Path::new("/definitely/not/here")), None);
        assert!(ensure_free_space(Path::new("/definitely/not/here"), u64::MAX).is_ok());
    }
}
