//! Monthly audit log archives: a zip holding the log as plain text.
//!
//! The audit log is the record of every privileged action taken on the BBS, so
//! nothing here deletes an archive. Archiving moves entries out of the live
//! log and into a file; removing that file is a sysop's decision, made by hand.
//!
//! An archive is written under a `.tmp` name and renamed only once complete,
//! and the live log is cleared only after that rename — so a crash mid-archive
//! loses the archive, never the entries.

use std::io::Write as _;
use std::path::Path;

use bbs_plugin_api::AdminAuditEntry;

/// Filename prefix every archive shares.
pub const ARCHIVE_PREFIX: &str = "audit-";

/// Whether `filename` looks like one of this module's archives.
///
/// Used by the listing and by the download and delete endpoints, so a sysop
/// can't be handed — or asked to delete — some unrelated file that happens to
/// share the directory.
#[must_use]
pub fn is_audit_archive(filename: &str) -> bool {
    filename.starts_with(ARCHIVE_PREFIX)
        && filename.ends_with(".zip")
        && !filename.contains('/')
        && !filename.contains('\\')
        && !filename.contains("..")
        && !filename.contains('\0')
}

/// The archive file name covering `year`/`month`, e.g. `audit-2026-09.zip`.
#[must_use]
pub fn archive_name(year: i32, month: u32) -> String {
    format!("{ARCHIVE_PREFIX}{year:04}-{month:02}.zip")
}

/// The text entry inside an archive, e.g. `audit-2026-09.txt`.
#[must_use]
pub fn entry_name(year: i32, month: u32) -> String {
    format!("{ARCHIVE_PREFIX}{year:04}-{month:02}.txt")
}

/// One line of an archive's text body.
///
/// Tab-separated so it stays greppable and pastes into a spreadsheet, with
/// tabs and newlines inside a field escaped so one entry can't span lines and
/// quietly become two.
#[must_use]
pub fn format_entry(e: &AdminAuditEntry) -> String {
    fn field(s: Option<&str>) -> String {
        match s {
            None | Some("") => "-".to_owned(),
            Some(v) => v
                .replace('\\', "\\\\")
                .replace('\t', "\\t")
                .replace('\n', "\\n"),
        }
    }
    format!(
        "{}\t{}\t{}\t{}\t{}\t{}",
        e.id,
        e.created_at,
        field(Some(&e.actor)),
        field(Some(&e.action)),
        field(e.target.as_deref()),
        field(e.detail.as_deref()),
    )
}

/// Writes an archive zip incrementally, so a long log never has to fit in
/// memory to be archived.
pub struct ArchiveWriter {
    zip: zip::ZipWriter<std::fs::File>,
    tmp: std::path::PathBuf,
    target: std::path::PathBuf,
    written: u64,
}

impl ArchiveWriter {
    /// Start an archive at `target`, writing to a sibling `.tmp` until it's
    /// finished. `entry` is the name of the text file inside the zip, and
    /// `header` its first lines (what this archive covers).
    ///
    /// # Errors
    ///
    /// If the temp file can't be created or the zip header can't be written.
    pub fn create(target: &Path, entry: &str, header: &str) -> std::io::Result<Self> {
        use zip::{write::SimpleFileOptions, CompressionMethod};

        let tmp = crate::restore_apply::sibling_with_suffix(target, ".tmp");
        // A leftover temp file is from an archive that died mid-write. Remove
        // it so the new one gets this mode and owner rather than inheriting
        // the old one's, and so create_new's O_EXCL refuses a symlink planted
        // at the name.
        match std::fs::remove_file(&tmp) {
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
        let file = opts.open(&tmp)?;
        hand_to_dir_owner(&file, &tmp);

        let mut zip = zip::ZipWriter::new(file);
        let zopts = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Deflated)
            .large_file(true);
        zip.start_file(entry, zopts)?;
        zip.write_all(header.as_bytes())?;

        Ok(Self {
            zip,
            tmp,
            target: target.to_path_buf(),
            written: 0,
        })
    }

    /// Append a batch of entries.
    ///
    /// # Errors
    ///
    /// If the zip can't be written to.
    pub fn write_batch(&mut self, entries: &[AdminAuditEntry]) -> std::io::Result<()> {
        for e in entries {
            self.zip.write_all(format_entry(e).as_bytes())?;
            self.zip.write_all(b"\n")?;
            self.written += 1;
        }
        Ok(())
    }

    /// How many entries have been written so far.
    #[must_use]
    pub fn entries_written(&self) -> u64 {
        self.written
    }

    /// Finish the zip and move it into place. Returns its size in bytes.
    ///
    /// Until this returns, nothing exists at the target name — so a caller
    /// that clears the live log only after this succeeds can't lose entries.
    ///
    /// # Errors
    ///
    /// If the zip can't be finalised, synced, or renamed.
    pub fn finish(self) -> std::io::Result<u64> {
        let file = self.zip.finish()?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&self.tmp, &self.target)?;
        Ok(std::fs::metadata(&self.target)?.len())
    }

    /// Give up, removing the partial temp file.
    pub fn abandon(self) {
        let tmp = self.tmp.clone();
        drop(self.zip);
        let _ = std::fs::remove_file(tmp);
    }
}

/// The header written at the top of an archive's text entry.
#[must_use]
pub fn header(count: u64, first_id: i64, last_id: i64, taken_at: &str) -> String {
    format!(
        "# Supply Drop BBS audit log archive\n\
         # taken: {taken_at}\n\
         # entries: {count} (ids {first_id}-{last_id})\n\
         # columns: id\\tcreated_at\\tactor\\taction\\ttarget\\tdetail\n\
         # '-' means the field was empty. Tabs and newlines inside a field are\n\
         # written as \\t and \\n so one entry is always one line.\n"
    )
}

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

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: i64, actor: &str, action: &str) -> AdminAuditEntry {
        AdminAuditEntry {
            id,
            actor: actor.to_owned(),
            action: action.to_owned(),
            target: None,
            detail: None,
            created_at: "2026-09-01T00:00:00Z".to_owned(),
        }
    }

    #[test]
    fn archive_names_are_zero_padded_and_recognised() {
        assert_eq!(archive_name(2026, 9), "audit-2026-09.zip");
        assert_eq!(entry_name(2026, 12), "audit-2026-12.txt");
        assert!(is_audit_archive("audit-2026-09.zip"));
    }

    #[test]
    fn only_this_module_s_archives_are_recognised() {
        // Anything that isn't one of ours, so a download or delete can't be
        // pointed at an unrelated neighbour in the same directory.
        for name in [
            "backup_2026-09-01.zip",
            "audit-2026-09.txt",
            "audit.zip",
            "",
            "audit-2026-09.zip.tmp",
        ] {
            assert!(!is_audit_archive(name), "{name} should not be an archive");
        }
        // And no traversal or separators, however the rest looks.
        for name in [
            "../audit-2026-09.zip",
            "audit-..-09.zip",
            "sub/audit-2026-09.zip",
            "audit-2026-09.zip/x",
        ] {
            assert!(!is_audit_archive(name), "{name} should not be an archive");
        }
    }

    #[test]
    fn an_entry_is_always_one_line() {
        let mut e = entry(7, "web:sysop", "ban");
        e.detail = Some("reason\nwith a newline\tand a tab".to_owned());
        let line = format_entry(&e);
        assert!(
            !line.contains('\n'),
            "a newline would split the entry in two"
        );
        assert_eq!(
            line.matches('\t').count(),
            5,
            "six columns, five separators"
        );
        assert!(line.contains("\\n") && line.contains("\\t"));
    }

    #[test]
    fn empty_fields_read_as_a_dash() {
        let line = format_entry(&entry(1, "bbs", "open_access"));
        assert!(
            line.ends_with("-\t-"),
            "target and detail should be dashes: {line}"
        );
    }

    #[test]
    fn a_written_archive_holds_every_entry_and_appears_only_when_done() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join(archive_name(2026, 9));

        let mut w = ArchiveWriter::create(
            &target,
            &entry_name(2026, 9),
            &header(2, 1, 2, "2026-10-01T00:00:00Z"),
        )
        .unwrap();
        assert!(
            !target.exists(),
            "nothing should exist at the target name until finish()"
        );
        w.write_batch(&[entry(1, "a", "ban"), entry(2, "b", "unban")])
            .unwrap();
        assert_eq!(w.entries_written(), 2);
        let size = w.finish().unwrap();
        assert!(size > 0);
        assert!(target.exists());

        let file = std::fs::File::open(&target).unwrap();
        let mut zip = zip::ZipArchive::new(file).unwrap();
        let mut body = String::new();
        {
            use std::io::Read as _;
            zip.by_name(&entry_name(2026, 9))
                .unwrap()
                .read_to_string(&mut body)
                .unwrap();
        }
        assert!(body.contains("# entries: 2 (ids 1-2)"));
        assert!(body.contains("\tban\t") && body.contains("\tunban\t"));
        // Six header lines plus one per entry, and a trailing newline.
        assert_eq!(body.lines().filter(|l| !l.starts_with('#')).count(), 2);
    }

    #[test]
    fn abandoning_leaves_nothing_behind() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join(archive_name(2026, 9));
        let w =
            ArchiveWriter::create(&target, &entry_name(2026, 9), &header(0, 0, 0, "t")).unwrap();
        w.abandon();
        assert!(!target.exists());
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            leftovers.is_empty(),
            "the temp file should be gone too, found {leftovers:?}"
        );
    }
}
