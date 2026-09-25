//! Loading a custom command keymap file from `data_dir` (GH #354 Phase 4).
//!
//! A custom keymap is TOML in the same shape as a built-in preset — see
//! [`bbs_plugin_api::Keymap`]'s doc comment for the exact format.
//! `#[serde(deny_unknown_fields)]` on `Keymap` and the enum-typed
//! `bindings` map mean the *shape* is already enforced by serde;
//! [`load_custom_keymap`] only needs to add the semantic check
//! ([`Keymap::validate`]) serde can't express.

use bbs_plugin_api::Keymap;
use std::path::Path;

/// Reject a filename that isn't a single, safe path component: empty,
/// containing a path separator, containing a control character, or equal
/// to `.`/`..`.
///
/// Shared by every entry point that turns a sysop-supplied string into a
/// filename under `data_dir` — [`load_custom_keymap`] (from `[bbs] keymap
/// = "custom:<filename>"`), `src/main.rs`'s `config upload-keymap
/// --as-filename`, and the web admin API's upload route (from the
/// multipart request's own `filename` field) — so none of them can
/// independently drift on which filenames are considered safe. The control-
/// character check matters specifically for the web upload path: a
/// filename containing e.g. a newline would otherwise pass through
/// untouched into `admin_write_audit`'s log entry and `config.toml`'s
/// `keymap = "custom:<filename>"` value (TOML-escaped there, but not in a
/// plain-text audit log viewer).
///
/// # Errors
///
/// A specific, human-readable error naming the problem.
pub fn is_safe_filename(filename: &str) -> Result<(), String> {
    if filename.is_empty() {
        return Err("filename is empty".to_owned());
    }
    if filename.contains('/') || filename.contains('\\') || filename == ".." || filename == "." {
        return Err(format!(
            "filename {filename:?} must be a plain filename with no path separators"
        ));
    }
    if filename.chars().any(|c| c.is_control()) {
        return Err(format!(
            "filename {filename:?} must not contain control characters"
        ));
    }
    Ok(())
}

/// Parse `text` as a [`Keymap`] and validate it.
///
/// Shared by [`load_custom_keymap`] (reading a file already in `data_dir`)
/// and [`save_custom_keymap`] (validating a keymap before it's ever written
/// to `data_dir`, from the CLI, the web admin API, or `admin_upload_keymap`),
/// so every entry point rejects a bad custom keymap the exact same way
/// instead of maintaining independently-written parse+validate steps that
/// could drift apart.
///
/// # Errors
///
/// A specific, human-readable error — never a panic — naming whether
/// parsing or validation failed. `context` is prepended (typically the
/// file path) to make the error self-contained for a CLI's `eprintln!`.
pub fn parse_and_validate(text: &str, context: &str) -> Result<Keymap, String> {
    let keymap: Keymap =
        toml::from_str(text).map_err(|e| format!("could not parse {context}: {e}"))?;
    keymap
        .validate()
        .map_err(|e| format!("{context} failed validation: {e}"))?;
    Ok(keymap)
}

/// Load and validate a custom keymap file from `data_dir/filename`.
///
/// `filename` must pass [`is_safe_filename`] — it comes from `[bbs] keymap
/// = "custom:<filename>"` in `config.toml`, which is sysop-authored but
/// treated as untrusted input anyway, matching this project's general
/// stance on config-sourced paths: a filename that tries to escape
/// `data_dir` is rejected outright rather than resolved and followed.
///
/// # Errors
///
/// Returns a specific, human-readable error — never a panic — identifying
/// which step failed (unsafe filename, missing/unreadable file, malformed
/// TOML, or a validation rule). "Reject a bad custom keymap file with a
/// clear, specific error, not a silent fallback to native" is the whole
/// point of this function; callers that want the silent-fallback behavior
/// implement it themselves at the call site (see `src/main.rs`'s keymap
/// resolution), logging this error before falling back.
pub fn load_custom_keymap(data_dir: &Path, filename: &str) -> Result<Keymap, String> {
    is_safe_filename(filename).map_err(|e| format!("custom keymap {e}"))?;

    let path = data_dir.join(filename);
    // O_NOFOLLOW, matching this crate's other privileged-path reads
    // (dir_perms::restrict_to_owner/restrict_file_to_owner,
    // restore_stage's open helpers) — data_dir is sysop-owned, but a
    // symlink planted there (by a compromised plugin process, or by hand)
    // must not cause this read to silently follow it outside data_dir.
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path)
        .map_err(|e| format!("could not open custom keymap file {}: {e}", path.display()))?;
    let mut text = String::new();
    std::io::Read::read_to_string(&mut file, &mut text)
        .map_err(|e| format!("could not read custom keymap file {}: {e}", path.display()))?;

    let keymap = parse_and_validate(&text, &format!("custom keymap file {}", path.display()))?;

    // Operator-uploaded content is the same trust tier as a backup or the
    // live database — restrict it to owner-only now that it's confirmed to
    // be a real, valid keymap file worth keeping around.
    crate::dir_perms::restrict_file_to_owner(data_dir, &path);

    Ok(keymap)
}

/// Validate `toml_text` and save it as a custom keymap file at
/// `data_dir/filename`, in one shot.
///
/// Shared by the CLI's `config upload-keymap`, `Host::admin_upload_keymap`,
/// and the web admin API's keymap-upload route — every caller that turns a
/// sysop-supplied blob of TOML text into a keymap file on disk. Taking the
/// text directly (rather than a source path to copy) is deliberate: the web
/// upload path never has a local file to re-read a second time, and this
/// shape closes a TOCTOU window an earlier, path-based version of the CLI's
/// upload had (GH #416) — `toml_text` is validated once, and the exact same
/// bytes that were validated are the only bytes ever written, with no
/// separate "read again to copy" step that could observe a changed file.
///
/// `data_dir` is assumed to already exist; callers that can't assume that
/// (the CLI, run standalone before any BBS has started) should
/// `create_dir_all` it first.
///
/// # Errors
///
/// Returns a specific, human-readable error — never a panic — identifying
/// which step failed (unsafe filename, malformed TOML, a validation rule, or
/// an I/O failure writing the file). Nothing is written to `data_dir` unless
/// `toml_text` fully validates first.
pub fn save_custom_keymap(
    data_dir: &Path,
    filename: &str,
    toml_text: &str,
) -> Result<Keymap, String> {
    is_safe_filename(filename).map_err(|e| format!("custom keymap {e}"))?;
    let keymap = parse_and_validate(toml_text, &format!("custom keymap {filename:?}"))?;

    let path = data_dir.join(filename);
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path)
        .map_err(|e| format!("could not write custom keymap file {}: {e}", path.display()))?;
    std::io::Write::write_all(&mut file, toml_text.as_bytes())
        .map_err(|e| format!("could not write custom keymap file {}: {e}", path.display()))?;
    file.sync_all()
        .map_err(|e| format!("could not write custom keymap file {}: {e}", path.display()))?;
    drop(file);

    // Same confidentiality tier as load_custom_keymap's own post-read step
    // (load_custom_keymap only reads an existing file; this is the write
    // counterpart, but the trust tier and the restrict-after-confirming-
    // valid rationale are identical).
    crate::dir_perms::restrict_file_to_owner(data_dir, &path);

    Ok(keymap)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, filename: &str, contents: &str) {
        std::fs::write(dir.join(filename), contents).unwrap();
    }

    #[test]
    fn is_safe_filename_rejects_empty_dotted_and_separated_names() {
        for bad in ["", ".", "..", "a/b", "a\\b", "../escape.toml"] {
            assert!(is_safe_filename(bad).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn is_safe_filename_accepts_a_plain_name() {
        assert!(is_safe_filename("my-bbs.toml").is_ok());
    }

    // Hostile-review finding, GH #354 preset-selection follow-up: a
    // newline (or other control character) in a web-uploaded filename would
    // otherwise reach config.toml and an audit-log entry verbatim.
    #[test]
    fn is_safe_filename_rejects_control_characters() {
        for bad in [
            "my\nbbs.toml",
            "my\r\nbbs.toml",
            "my\tbbs.toml",
            "my\0bbs.toml",
        ] {
            assert!(is_safe_filename(bad).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn parse_and_validate_rejects_malformed_toml_with_context() {
        let err = parse_and_validate("not valid toml {{{", "ctx.toml").unwrap_err();
        assert!(err.contains("ctx.toml"), "{err}");
    }

    #[test]
    fn parse_and_validate_rejects_a_keymap_that_fails_validate_with_context() {
        let err = parse_and_validate(
            "name = \"x\"\ndescription = \"x\"\n[bindings]\ng = \"Quit\"\n",
            "ctx.toml",
        )
        .unwrap_err();
        assert!(err.contains("ctx.toml"), "{err}");
        assert!(err.contains("failed validation"), "{err}");
    }

    #[test]
    fn a_valid_custom_keymap_loads_and_validates() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "my-bbs.toml",
            r#"
                name = "my-bbs"
                description = "test"

                [bindings]
                l = "ScanMessages"
                a = "ChangeRoom"
            "#,
        );
        let km = load_custom_keymap(dir.path(), "my-bbs.toml").unwrap();
        assert_eq!(km.name, "my-bbs");
        assert_eq!(km.validate(), Ok(()));
    }

    #[test]
    fn a_missing_file_is_a_clear_error() {
        let dir = tempfile::tempdir().unwrap();
        let err = load_custom_keymap(dir.path(), "nope.toml").unwrap_err();
        assert!(err.contains("nope.toml"), "{err}");
    }

    #[test]
    fn malformed_toml_is_a_clear_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "bad.toml", "this is not valid toml {{{");
        let err = load_custom_keymap(dir.path(), "bad.toml").unwrap_err();
        assert!(err.contains("bad.toml"), "{err}");
    }

    #[test]
    fn a_keymap_that_fails_validate_is_rejected_with_a_specific_error() {
        let dir = tempfile::tempdir().unwrap();
        // Steals native "g" (GoNextUnread) for Quit without relocating
        // GoNextUnread — Rule 2 violation.
        write(
            dir.path(),
            "broken.toml",
            r#"
                name = "broken"
                description = "test"

                [bindings]
                g = "Quit"
            "#,
        );
        let err = load_custom_keymap(dir.path(), "broken.toml").unwrap_err();
        assert!(err.contains("broken.toml"), "{err}");
        assert!(
            err.contains("GoNextUnread") || err.contains("failed validation"),
            "{err}"
        );
    }

    #[test]
    fn a_filename_with_a_path_separator_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        for bad in ["../escape.toml", "sub/dir.toml", "..", "."] {
            let err = load_custom_keymap(dir.path(), bad).unwrap_err();
            assert!(
                err.contains("path separators") || err.contains(bad),
                "{bad}: {err}"
            );
        }
    }

    #[test]
    fn an_empty_filename_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_custom_keymap(dir.path(), "").is_err());
    }

    #[test]
    fn save_custom_keymap_writes_and_returns_a_valid_keymap() {
        let dir = tempfile::tempdir().unwrap();
        let text = r#"
            name = "my-bbs"
            description = "test"

            [bindings]
            l = "ScanMessages"
            a = "ChangeRoom"
        "#;
        let km = save_custom_keymap(dir.path(), "my-bbs.toml", text).unwrap();
        assert_eq!(km.name, "my-bbs");
        assert_eq!(km.validate(), Ok(()));

        // What's on disk is exactly what was validated, and loads back the
        // same way load_custom_keymap would read any other custom file.
        let reloaded = load_custom_keymap(dir.path(), "my-bbs.toml").unwrap();
        assert_eq!(reloaded.name, "my-bbs");
        assert_eq!(reloaded.bindings, km.bindings);
    }

    #[test]
    fn save_custom_keymap_rejects_invalid_toml_without_writing_anything() {
        let dir = tempfile::tempdir().unwrap();
        let err = save_custom_keymap(dir.path(), "bad.toml", "not valid toml {{{").unwrap_err();
        assert!(err.contains("bad.toml"), "{err}");
        assert!(!dir.path().join("bad.toml").exists());
    }

    #[test]
    fn save_custom_keymap_rejects_a_keymap_that_fails_validate_without_writing_anything() {
        let dir = tempfile::tempdir().unwrap();
        // Steals native "g" (GoNextUnread) for Quit without relocating
        // GoNextUnread — Rule 2 violation, same as the load_custom_keymap
        // test above.
        let text = "name = \"broken\"\ndescription = \"test\"\n[bindings]\ng = \"Quit\"\n";
        let err = save_custom_keymap(dir.path(), "broken.toml", text).unwrap_err();
        assert!(err.contains("failed validation"), "{err}");
        assert!(!dir.path().join("broken.toml").exists());
    }

    #[test]
    fn save_custom_keymap_rejects_an_unsafe_filename_without_writing_anything() {
        let dir = tempfile::tempdir().unwrap();
        let err = save_custom_keymap(
            dir.path(),
            "../escape.toml",
            "name = \"x\"\ndescription = \"x\"\n",
        )
        .unwrap_err();
        assert!(err.contains("path separators"), "{err}");
        assert!(!dir.path().join("escape.toml").exists());
        assert!(!dir.path().parent().unwrap().join("escape.toml").exists());
    }

    #[cfg(unix)]
    #[test]
    fn save_custom_keymap_restricts_the_file_to_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let text = "name = \"my-bbs\"\ndescription = \"test\"\n[bindings]\nl = \"ScanMessages\"\n";
        save_custom_keymap(dir.path(), "my-bbs.toml", text).unwrap();
        let mode = std::fs::metadata(dir.path().join("my-bbs.toml"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }
}
