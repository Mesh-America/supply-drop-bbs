//! Backup bundles: the database and the `config.toml` it runs with, in one zip.
//!
//! Only the web UI's "create backup" button used to produce the bundle. The
//! automatic backups and `supply-drop-bbs backup` wrote the bare database, so
//! most backups held none of the settings kept in the config file (the BBS
//! name among them). Every way of taking a backup goes through
//! [`crate::host::BbsHost`]'s `admin_trigger_backup_bundle` now, which uses
//! this.

use std::io::Write as _;
use std::path::Path;

/// The name of the config inside a bundle. Restore looks for exactly this.
pub const CONFIG_ENTRY: &str = "config.toml";

/// What went into a bundle.
#[derive(Debug, Clone, Copy)]
pub struct BundleInfo {
    /// Size of the finished zip in bytes.
    pub zip_bytes: u64,
    /// Size of the config that was bundled, if one was.
    pub config_bytes: Option<u64>,
}

/// Write `<zip_path>` holding the database at `db_path` (as `db_entry`) and,
/// if it can be read, the config at `config_path`. The zip is written under a
/// `.tmp` name and renamed only when complete, so a crash never leaves a
/// half-written bundle that looks like a backup. It is created owner-only
/// (0600): the database holds password hashes. It is then handed to the owner
/// of the backup directory where the process is allowed to do that (see
/// `hand_to_dir_owner`).
///
/// A missing or unreadable config is not an error (the bundle then holds the
/// database alone, as the old bare backups did) but is logged, since a backup
/// that silently lacks its settings is the thing this exists to prevent.
///
/// The database is streamed into the zip, never held in memory.
///
/// # Errors
/// An I/O error from reading the database or writing the zip; nothing is left
/// behind.
pub fn write_bundle(
    db_path: &Path,
    db_entry: &str,
    config_path: Option<&Path>,
    zip_path: &Path,
) -> std::io::Result<BundleInfo> {
    let tmp = crate::restore_apply::sibling_with_suffix(zip_path, ".tmp");
    let result = write_bundle_inner(db_path, db_entry, config_path, &tmp, zip_path);
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// [`write_bundle`], then remove the bare database it was made from. The bare
/// copy is only removed once the zip is complete: if bundling fails the
/// database-only backup is left in place, so a failure here never costs the
/// backup that was just taken.
///
/// # Errors
/// As [`write_bundle`]; the bare database is still there.
pub fn bundle_and_drop_bare(
    db_path: &Path,
    db_entry: &str,
    config_path: Option<&Path>,
    zip_path: &Path,
) -> std::io::Result<BundleInfo> {
    let info = write_bundle(db_path, db_entry, config_path, zip_path)?;
    let _ = std::fs::remove_file(db_path);
    Ok(info)
}

/// Give the open file to the owner (user and group) of the directory it is
/// in, best effort. `supply-drop-bbs backup` run as root would otherwise leave
/// a root-owned 0600 bundle that the service (which lists, deletes and stages
/// backups) can't read. Going by the descriptor, not the path, keeps a swapped
/// path from redirecting the chown. A process that isn't root can't give the
/// file away but can usually still set the group, so that is tried next.
///
/// This assumes the backup directory belongs to the service user, as the
/// packages set it up: a bundle is 0600 (it holds password hashes), so a
/// directory owned by someone else with only group access to the service would
/// still leave the service unable to read it.
fn hand_to_dir_owner(file: &std::fs::File, path: &Path) {
    use std::os::unix::fs::{fchown, MetadataExt as _};
    let dir = match path.parent() {
        Some(d) if !d.as_os_str().is_empty() => d,
        _ => Path::new("."),
    };
    if let Ok(meta) = std::fs::metadata(dir) {
        if fchown(file, Some(meta.uid()), Some(meta.gid())).is_err() {
            let _ = fchown(file, None, Some(meta.gid()));
        }
    }
}

fn write_bundle_inner(
    db_path: &Path,
    db_entry: &str,
    config_path: Option<&Path>,
    tmp: &Path,
    zip_path: &Path,
) -> std::io::Result<BundleInfo> {
    use zip::{write::SimpleFileOptions, CompressionMethod};

    // A leftover temp file (a backup that died mid-write) is removed and made
    // again, so the new one is created here with this mode and owner rather
    // than inheriting whatever the old one had; `create_new` (O_EXCL) also
    // refuses a symlink planted at the name, which matters when this runs as
    // root under `supply-drop-bbs backup`.
    match std::fs::remove_file(tmp) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = opts.open(tmp)?;
    hand_to_dir_owner(&file, tmp);
    let mut zip = zip::ZipWriter::new(file);
    let zopts = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .large_file(true);

    zip.start_file(db_entry, zopts)?;
    std::io::copy(&mut std::fs::File::open(db_path)?, &mut zip)?;

    let mut config_bytes = None;
    if let Some(cfg) = config_path {
        match read_config(cfg) {
            Ok(bytes) => {
                zip.start_file(CONFIG_ENTRY, zopts)?;
                zip.write_all(&bytes)?;
                config_bytes = Some(bytes.len() as u64);
            }
            Err(e) => tracing::warn!(
                "backup: could not include the config file '{}': {e}; this backup holds the \
                 database only and will not restore the settings",
                cfg.display()
            ),
        }
    } else {
        tracing::warn!(
            "backup: the location of the config file is not known, so this backup holds the \
             database only and will not restore the settings"
        );
    }

    let file = zip.finish()?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(tmp, zip_path)?;
    Ok(BundleInfo {
        zip_bytes: std::fs::metadata(zip_path)?.len(),
        config_bytes,
    })
}

fn read_config(path: &Path) -> std::io::Result<Vec<u8>> {
    use std::io::Read as _;
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(crate::restore_config::MAX_CONFIG_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > crate::restore_config::MAX_CONFIG_BYTES {
        return Err(std::io::Error::other("the config file is too large"));
    }
    Ok(bytes)
}

/// The size of the `config.toml` inside the bundle at `zip_path`, if it has
/// one. `None` for a zip without it, or one that can't be read.
#[must_use]
pub fn bundled_config_size(zip_path: &Path) -> Option<u64> {
    let file = std::fs::File::open(zip_path).ok()?;
    let mut archive = zip::ZipArchive::new(std::io::BufReader::new(file)).ok()?;
    let entry = archive.by_name(CONFIG_ENTRY).ok()?;
    Some(entry.size())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_entry(zip_path: &Path, name: &str) -> Option<Vec<u8>> {
        use std::io::Read as _;
        let mut archive = zip::ZipArchive::new(std::fs::File::open(zip_path).unwrap()).unwrap();
        let mut entry = archive.by_name(name).ok()?;
        let mut out = Vec::new();
        entry.read_to_end(&mut out).unwrap();
        Some(out)
    }

    #[test]
    fn a_bundle_holds_the_database_and_the_config() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("backup_1.db");
        let cfg = dir.path().join("config.toml");
        let zip = dir.path().join("backup_1.zip");
        std::fs::write(&db, b"SQLite format 3\0pretend database").unwrap();
        std::fs::write(&cfg, "[bbs]\nname = \"My BBS\"\n").unwrap();

        let info = write_bundle(&db, "backup_1.db", Some(&cfg), &zip).unwrap();

        assert_eq!(
            read_entry(&zip, "backup_1.db").unwrap(),
            b"SQLite format 3\0pretend database"
        );
        assert_eq!(
            read_entry(&zip, "config.toml").unwrap(),
            b"[bbs]\nname = \"My BBS\"\n"
        );
        assert_eq!(info.config_bytes, Some(22));
        assert_eq!(info.zip_bytes, std::fs::metadata(&zip).unwrap().len());
        assert_eq!(bundled_config_size(&zip), info.config_bytes);
        assert!(!dir.path().join("backup_1.zip.tmp").exists());
    }

    #[test]
    fn a_missing_config_still_makes_a_database_bundle() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("b.db");
        let zip = dir.path().join("b.zip");
        std::fs::write(&db, b"db").unwrap();

        for cfg in [None, Some(dir.path().join("absent.toml"))] {
            let info = write_bundle(&db, "b.db", cfg.as_deref(), &zip).unwrap();
            assert_eq!(info.config_bytes, None);
            assert!(read_entry(&zip, "b.db").is_some());
            assert!(read_entry(&zip, "config.toml").is_none());
            assert_eq!(bundled_config_size(&zip), None);
        }
    }

    #[test]
    fn an_oversized_config_is_left_out_not_truncated() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("b.db");
        let cfg = dir.path().join("config.toml");
        let zip = dir.path().join("b.zip");
        std::fs::write(&db, b"db").unwrap();
        std::fs::write(
            &cfg,
            vec![b'#'; crate::restore_config::MAX_CONFIG_BYTES as usize + 1],
        )
        .unwrap();
        let info = write_bundle(&db, "b.db", Some(&cfg), &zip).unwrap();
        assert_eq!(info.config_bytes, None);
        assert!(read_entry(&zip, "config.toml").is_none());
    }

    #[test]
    fn a_failure_leaves_no_partial_zip() {
        let dir = tempfile::tempdir().unwrap();
        let zip = dir.path().join("b.zip");
        // The database doesn't exist.
        assert!(write_bundle(&dir.path().join("absent.db"), "b.db", None, &zip).is_err());
        assert!(!zip.exists());
        assert!(!dir.path().join("b.zip.tmp").exists());
    }

    #[test]
    fn the_bare_database_is_removed_only_after_the_zip_is_complete() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("b.db");
        std::fs::write(&db, b"db").unwrap();

        // A zip that can't be written (its temp name is taken by a directory):
        // the database-only backup must survive.
        let zip = dir.path().join("b.zip");
        std::fs::create_dir(dir.path().join("b.zip.tmp")).unwrap();
        assert!(bundle_and_drop_bare(&db, "b.db", None, &zip).is_err());
        assert!(db.exists(), "a failed bundle must not cost the backup");
        assert!(!zip.exists());

        // A zip that is written: the bare copy is dropped.
        let zip2 = dir.path().join("c.zip");
        bundle_and_drop_bare(&db, "b.db", None, &zip2).unwrap();
        assert!(!db.exists());
        assert!(zip2.exists());
    }

    #[cfg(unix)]
    #[test]
    fn the_bundle_is_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("b.db");
        let zip = dir.path().join("b.zip");
        std::fs::write(&db, b"db").unwrap();
        write_bundle(&db, "b.db", None, &zip).unwrap();
        let mode = std::fs::metadata(&zip).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "{mode:o}");
    }
}
