//! Restoring `config.toml` along with the database.
//!
//! A backup bundle (`backup_*.zip`) holds the database and the `config.toml`
//! it was taken with. Restoring the database alone brought back the data but
//! not the settings that live in the file (the BBS name, the welcome message,
//! the timezone, location, mesh settings and so on), so a restore never looked
//! complete.
//!
//! The restored file is not dropped in as it is. Parts of the config describe
//! *this* machine, or decide what it runs and who can reach it, and those keep
//! the running config's values (and are removed if the running config does not
//! set them), whatever the backup says:
//!
//! * where things live: the data directory, the database section (path and the
//!   tuning that goes with the disk), the log file, the backup section
//!   (directory, schedule and retention);
//! * how it is reached: the whole `[plugins.web]` and `[plugins.cli]` tables
//!   (bind address, origin, cookies, CSP, socket and its permissions);
//! * what it executes: `[[plugins.process]]`, which names commands to run;
//! * the hardware: how each radio is connected, its `[radio]` settings, whether
//!   it is `enabled`, and the security section, whose password-hashing cost is
//!   tuned to this machine's speed.
//!
//! A bundle from another host would otherwise point the admin UI or a radio
//! somewhere that doesn't exist here, or run a command that isn't installed
//! here. Everything else (the BBS name and the other `[bbs]` settings, the
//! location, the log level, the mesh transports' behaviour) comes from the
//! backup.
//!
//! The steps are split so that each fails on its own without hurting the
//! others:
//! * at staging, [`validate_staged_config`] rejects a file that is not TOML;
//! * at startup, [`apply_config`] merges and writes the file atomically after
//!   saving the running one as `<config>.pre-restore`;
//! * the caller checks that the merged file loads, and [`settle_applied`] puts
//!   the saved one back if it does not.

use std::path::{Path, PathBuf};

use toml_edit::{DocumentMut, Item, Table, TableLike};

/// Where a staged config lives until the restore is confirmed.
pub const STAGED_CONFIG_NAME: &str = "pending_restore.staged.config.toml";
/// Where a confirmed config lives until the next start applies it.
pub const PENDING_CONFIG_NAME: &str = "pending_restore.config.toml";
/// The largest `config.toml` a backup may carry. The real file is a few KiB.
pub const MAX_CONFIG_BYTES: u64 = 1024 * 1024;

/// The keys whose values stay as they are on this machine (see the module
/// docs). Each is a path into the TOML document; a table or array of tables
/// listed here is kept whole.
pub const MACHINE_SPECIFIC_KEYS: &[&[&str]] = &[
    &["bbs", "data_dir"],
    &["database"],
    &["logging", "file"],
    &["backup"],
    &["security"],
    &["plugins", "cli"],
    &["plugins", "web"],
    &["plugins", "process"],
    &["plugins", "mesh", "connection_type"],
    &["plugins", "mesh", "addr"],
    &["plugins", "mesh", "serial_port"],
    &["plugins", "mesh", "baud_rate"],
    &["plugins", "mesh", "radio"],
    &["plugins", "mesh", "enabled"],
    &["plugins", "mesh", "app_target_version"],
    &["plugins", "mesh", "protected_contact_cap"],
    &["plugins", "meshtastic", "connection_type"],
    &["plugins", "meshtastic", "addr"],
    &["plugins", "meshtastic", "serial_port"],
    &["plugins", "meshtastic", "baud_rate"],
    &["plugins", "meshtastic", "radio"],
    &["plugins", "meshtastic", "enabled"],
    &["plugins", "meshtastic", "protected_contact_cap"],
];

/// Check that `text` is a TOML document a restore can work with.
///
/// # Errors
/// A message naming the parse problem.
pub fn validate_staged_config(text: &str) -> Result<(), String> {
    text.parse::<DocumentMut>()
        .map(|_| ())
        .map_err(|e| format!("the config.toml in the backup is not valid TOML: {e}"))
}

/// The restored config with this machine's values for the
/// [`MACHINE_SPECIFIC_KEYS`] taken from `current`.
///
/// # Errors
/// A message if either text is not valid TOML.
pub fn merge_for_restore(current: &str, restored: &str) -> Result<String, String> {
    let current = current
        .parse::<DocumentMut>()
        .map_err(|e| format!("the current config.toml is not valid TOML: {e}"))?;
    let mut merged = restored
        .parse::<DocumentMut>()
        .map_err(|e| format!("the config.toml in the backup is not valid TOML: {e}"))?;

    for path in MACHINE_SPECIFIC_KEYS {
        match get_path(current.as_table(), path) {
            Some(item) => set_path(merged.as_table_mut(), path, item.clone())?,
            None => remove_path(merged.as_table_mut(), path),
        }
    }
    Ok(merged.to_string())
}

// The helpers work on `TableLike`, not `Table`: a table can be written as
// `[plugins.web]` (a `Table`) or inline as `plugins = { web = { .. } }` (an
// `InlineTable`), and a guard that only understood the first would let the
// second slip a machine-specific key past it.

fn get_path<'a>(table: &'a dyn TableLike, path: &[&str]) -> Option<&'a Item> {
    let (last, parents) = path.split_last()?;
    let mut t = table;
    for key in parents {
        t = t.get(key)?.as_table_like()?;
    }
    t.get(last)
}

fn set_path(table: &mut dyn TableLike, path: &[&str], item: Item) -> Result<(), String> {
    let Some((last, parents)) = path.split_last() else {
        return Ok(());
    };
    let mut t = table;
    for key in parents {
        if !t.contains_key(key) {
            let mut sub = Table::new();
            // Keep `[plugins]` and friends implicit, so no empty header is
            // written for a table that only exists to hold a subtable.
            sub.set_implicit(true);
            t.insert(key, Item::Table(sub));
        }
        t = t
            .get_mut(key)
            .and_then(Item::as_table_like_mut)
            .ok_or_else(|| {
                format!("the config.toml in the backup has a `{key}` that is not a table")
            })?;
    }
    t.insert(last, item);
    Ok(())
}

fn remove_path(table: &mut dyn TableLike, path: &[&str]) {
    let Some((last, parents)) = path.split_last() else {
        return;
    };
    let mut t = table;
    for key in parents {
        match t.get_mut(key).and_then(Item::as_table_like_mut) {
            Some(sub) => t = sub,
            None => return,
        }
    }
    t.remove(last);
}

/// What [`apply_config`] did.
#[derive(Debug)]
pub struct ConfigApplied {
    /// The running config, saved before it was replaced. `None` if there was
    /// no config file to save.
    pub previous: Option<PathBuf>,
}

/// The file the running config is saved to before a restore replaces it.
#[must_use]
pub fn pre_restore_path(config_path: &Path) -> PathBuf {
    crate::restore_apply::sibling_with_suffix(config_path, ".pre-restore")
}

/// Merge the staged config (`restored`) with the one at `config_path` and
/// write the result there atomically. The whole read-merge-write runs under the
/// same lock the CLI and the web settings page use, so a concurrent edit of the
/// file isn't lost. The running file is saved as `<config>.pre-restore` first,
/// and the file keeps its permissions.
///
/// # Errors
/// A message if either file can't be read, the merge fails, or the file can't
/// be written (for example when the service user can't write to it). The
/// running config is left in place on error; a `.pre-restore` copy written
/// before the failure is left too.
pub fn apply_config(config_path: &Path, restored: &Path) -> Result<ConfigApplied, String> {
    let restored_text = read_capped(restored)?;
    // Write to the file a symlinked config points at, not over the link.
    let config_path = resolve(config_path);
    let previous = pre_restore_path(&config_path);
    let target = config_path.clone();
    let previous_for_lock = previous.clone();

    let had_current = crate::config_lock::with_config_lock_sync(&config_path, move || {
        let current_text = match std::fs::read_to_string(&target) {
            Ok(t) => Some(t),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e),
        };
        let merged = merge_for_restore(current_text.as_deref().unwrap_or(""), &restored_text)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        // The new file (and the saved copy of the old one) take the old file's
        // mode and owner: the write makes a new file, which would otherwise
        // belong to whoever is running (root, for the CLI) and lose a mode the
        // operator tightened.
        let identity = match &current_text {
            Some(old) => {
                let meta = std::fs::metadata(&target)?;
                crate::config_lock::atomic_write_file_as(
                    &previous_for_lock,
                    old.as_bytes(),
                    Some(&meta),
                )?;
                Some(meta)
            }
            None => None,
        };
        crate::config_lock::atomic_write_file_as(&target, merged.as_bytes(), identity.as_ref())?;
        Ok(identity.is_some())
    })
    .map_err(|e| {
        if e.kind() == std::io::ErrorKind::InvalidData {
            e.to_string()
        } else {
            format!("could not write {}: {e}", config_path.display())
        }
    })?;

    Ok(ConfigApplied {
        previous: had_current.then_some(previous),
    })
}

/// Put the config saved by [`apply_config`] back. Used when the merged file
/// turns out not to load.
///
/// # Errors
/// A message if the saved copy can't be put back.
pub fn revert_config(config_path: &Path, applied: &ConfigApplied) -> Result<(), String> {
    let config_path = resolve(config_path);
    let Some(previous) = &applied.previous else {
        // There was no config before: remove the one the restore created.
        return std::fs::remove_file(&config_path)
            .map_err(|e| format!("could not remove {}: {e}", config_path.display()));
    };
    let text = std::fs::read(previous)
        .map_err(|e| format!("could not read {}: {e}", previous.display()))?;
    let target = config_path.clone();
    let previous = previous.clone();
    crate::config_lock::with_config_lock_sync(&config_path, move || {
        // The saved copy carries the original file's mode and owner.
        let meta = std::fs::metadata(&previous)?;
        crate::config_lock::atomic_write_file_as(&target, &text, Some(&meta))
    })
    .map_err(|e| format!("could not put the previous config back: {e}"))
}

/// `path` with symlinks resolved, or `path` itself if it doesn't exist yet.
fn resolve(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// Keep the config [`apply_config`] wrote only if `loads` accepts it. `loads`
/// is the caller's own check that the file is usable (in the service, loading
/// it as a config): a file that doesn't load would stop every later start, so
/// the previous one is put back and the error returned.
///
/// # Errors
/// The message from `loads`, plus a note if the previous config could not be
/// put back either.
pub fn settle_applied<T>(
    config_path: &Path,
    applied: &ConfigApplied,
    loads: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    match loads() {
        Ok(v) => Ok(v),
        Err(why) => match revert_config(config_path, applied) {
            Ok(()) => Err(why),
            Err(e) => Err(format!(
                "{why} (and the previous config could not be put back: {e})"
            )),
        },
    }
}

/// Read a staged config, refusing one over [`MAX_CONFIG_BYTES`].
fn read_capped(path: &Path) -> Result<String, String> {
    use std::io::Read as _;
    let file =
        std::fs::File::open(path).map_err(|e| format!("could not read {}: {e}", path.display()))?;
    let mut text = String::new();
    file.take(MAX_CONFIG_BYTES + 1)
        .read_to_string(&mut text)
        .map_err(|e| format!("could not read {}: {e}", path.display()))?;
    if text.len() as u64 > MAX_CONFIG_BYTES {
        return Err("the config.toml in the backup is too large".into());
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn merged(current: &str, restored: &str) -> DocumentMut {
        merge_for_restore(current, restored)
            .unwrap()
            .parse::<DocumentMut>()
            .unwrap()
    }

    #[test]
    fn settings_come_from_the_backup() {
        let current = "[bbs]\nname = \"Old Name\"\nwelcome_msg = \"old\"\n";
        let restored =
            "[bbs]\nname = \"Restored Name\"\nwelcome_msg = \"hi {name}\"\ntimezone = \"UTC\"\n\
             [location]\nlatitude = 1.5\nlongitude = 2.5\n";
        let m = merged(current, restored);
        assert_eq!(m["bbs"]["name"].as_str(), Some("Restored Name"));
        assert_eq!(m["bbs"]["welcome_msg"].as_str(), Some("hi {name}"));
        assert_eq!(m["bbs"]["timezone"].as_str(), Some("UTC"));
        assert_eq!(m["location"]["latitude"].as_float(), Some(1.5));
    }

    #[test]
    fn machine_specific_keys_keep_this_machines_values() {
        let current = r#"
[bbs]
name = "Here"
data_dir = "/var/lib/supply-drop-bbs"
[database]
path = "/var/lib/supply-drop-bbs/bbs.sqlite"
[plugins.web]
bind = "127.0.0.1:8080"
[plugins.mesh]
connection_type = "serial"
serial_port = "/dev/serial/by-id/usb-here"
baud_rate = 115200
"#;
        let restored = r#"
[bbs]
name = "There"
data_dir = "/home/other/data"
[database]
path = "/home/other/data/bbs.sqlite"
[plugins.web]
bind = "0.0.0.0:9000"
external_origin = "https://elsewhere.example"
[plugins.mesh]
connection_type = "tcp"
serial_port = "/dev/ttyACM7"
baud_rate = 9600
enabled = true
"#;
        let m = merged(current, restored);
        assert_eq!(m["bbs"]["name"].as_str(), Some("There"), "settings restore");
        assert_eq!(
            m["bbs"]["data_dir"].as_str(),
            Some("/var/lib/supply-drop-bbs")
        );
        assert_eq!(
            m["database"]["path"].as_str(),
            Some("/var/lib/supply-drop-bbs/bbs.sqlite")
        );
        assert_eq!(m["plugins"]["web"]["bind"].as_str(), Some("127.0.0.1:8080"));
        assert!(m["plugins"]["web"].get("external_origin").is_none());
        assert_eq!(
            m["plugins"]["mesh"]["connection_type"].as_str(),
            Some("serial")
        );
        assert_eq!(
            m["plugins"]["mesh"]["serial_port"].as_str(),
            Some("/dev/serial/by-id/usb-here")
        );
        assert_eq!(m["plugins"]["mesh"]["baud_rate"].as_integer(), Some(115200));
        // Whether the radio runs is this machine's too: unset here stays unset.
        assert!(m["plugins"]["mesh"].get("enabled").is_none());
    }

    // What a foreign or hostile backup must not be able to change: what the
    // service executes, who can reach it, how strong the password hashing is,
    // and what the backup task deletes.
    #[test]
    fn a_backup_cannot_change_what_runs_who_reaches_it_or_the_backup_schedule() {
        let current = r#"
[bbs]
name = "Mine"
[plugins.web]
bind = "127.0.0.1:8080"
[security]
argon2_memory_kib = 19456
[backup]
keep_daily = 7
"#;
        let hostile = r#"
[bbs]
name = "Theirs"
[[plugins.process]]
name = "evil"
command = "/bin/sh"
args = ["-c", "id > /tmp/pwned"]
[plugins.cli]
socket_mode = "0666"
[plugins.web]
enabled = false
csp = "default-src *"
[security]
argon2_memory_kib = 8
argon2_iterations = 1
[backup]
keep_daily = 0
keep_weekly = 0
interval_hours = 0
"#;
        let m = merged(current, hostile);
        assert_eq!(m["bbs"]["name"].as_str(), Some("Theirs"));
        assert!(m["plugins"].get("process").is_none(), "no process plugins");
        assert!(m["plugins"].get("cli").is_none());
        assert_eq!(m["plugins"]["web"]["bind"].as_str(), Some("127.0.0.1:8080"));
        assert!(m["plugins"]["web"].get("csp").is_none());
        assert!(m["plugins"]["web"].get("enabled").is_none());
        assert_eq!(m["security"]["argon2_memory_kib"].as_integer(), Some(19456));
        assert!(m["security"].get("argon2_iterations").is_none());
        assert_eq!(m["backup"]["keep_daily"].as_integer(), Some(7));
        assert!(m["backup"].get("keep_weekly").is_none());
    }

    // This machine's own process plugins (an array of tables) are kept as they
    // are, not replaced by the backup's.
    #[test]
    fn this_machines_process_plugins_are_kept() {
        let current = "[[plugins.process]]\nname = \"mine\"\ncommand = \"/usr/bin/mine\"\n";
        let restored = "[[plugins.process]]\nname = \"theirs\"\ncommand = \"/opt/theirs\"\n";
        let text = merge_for_restore(current, restored).unwrap();
        assert!(text.contains("/usr/bin/mine"), "{text}");
        assert!(!text.contains("theirs"), "{text}");
    }

    // The same tables written inline must be guarded the same way.
    #[test]
    fn inline_tables_cannot_smuggle_machine_specific_keys_in() {
        let current = "[plugins.web]\nbind = \"127.0.0.1:8080\"\n[bbs]\nname = \"x\"\n";
        let restored = "plugins = { web = { bind = \"0.0.0.0:80\", cookie_secure = false }, cli = { socket_mode = \"0666\" } }\n\
                        bbs = { name = \"Inline\", data_dir = \"/evil\" }\n\
                        database = { path = \"/evil/db\" }\n";
        let m = merged(current, restored);
        assert_eq!(m["bbs"]["name"].as_str(), Some("Inline"));
        assert!(m["bbs"].get("data_dir").is_none(), "{m}");
        assert!(m.get("database").is_none(), "{m}");
        assert_eq!(m["plugins"]["web"]["bind"].as_str(), Some("127.0.0.1:8080"));
        assert!(m["plugins"]["web"].get("cookie_secure").is_none(), "{m}");
        assert!(m["plugins"].get("cli").is_none(), "{m}");
    }

    #[test]
    fn a_current_config_written_inline_is_read_correctly() {
        let current =
            "plugins = { web = { bind = \"127.0.0.1:8080\" } }\nbbs = { data_dir = \"/here\" }\n";
        let restored =
            "[bbs]\nname = \"N\"\ndata_dir = \"/there\"\n[plugins.web]\nbind = \"0.0.0.0:1\"\n";
        let m = merged(current, restored);
        assert_eq!(m["bbs"]["name"].as_str(), Some("N"));
        assert_eq!(m["bbs"]["data_dir"].as_str(), Some("/here"));
        assert_eq!(m["plugins"]["web"]["bind"].as_str(), Some("127.0.0.1:8080"));
    }

    #[test]
    fn dotted_keys_are_guarded_too() {
        let current = "plugins.web.bind = \"127.0.0.1:8080\"\n";
        let restored =
            "bbs.name = \"D\"\nbbs.data_dir = \"/evil\"\nplugins.web.bind = \"0.0.0.0:1\"\n";
        let m = merged(current, restored);
        assert_eq!(m["bbs"]["name"].as_str(), Some("D"));
        assert!(m["bbs"].get("data_dir").is_none());
        assert_eq!(m["plugins"]["web"]["bind"].as_str(), Some("127.0.0.1:8080"));
    }

    #[test]
    fn a_machine_specific_table_is_kept_whole_and_created_when_missing() {
        let current = "[plugins.mesh.radio]\npreset = \"zebrahat\"\n";
        let restored = "[plugins.mesh]\nname = \"restored\"\n";
        let m = merged(current, restored);
        assert_eq!(
            m["plugins"]["mesh"]["radio"]["preset"].as_str(),
            Some("zebrahat")
        );
        assert_eq!(m["plugins"]["mesh"]["name"].as_str(), Some("restored"));

        // And a radio table the current config lacks is dropped.
        let none = merged(
            "[bbs]\nname = \"x\"\n",
            "[plugins.mesh.radio]\npreset = \"other\"\n",
        );
        assert!(none
            .get("plugins")
            .and_then(|p| p.get("mesh"))
            .and_then(|m| m.get("radio"))
            .is_none());
    }

    #[test]
    fn whether_a_radio_plugin_runs_stays_with_this_machine() {
        // A backup taken on a machine with the radio enabled must not switch
        // the radio on (or off) on a machine whose hardware differs.
        let m = merged(
            "[plugins.mesh]\nenabled = false\n[plugins.meshtastic]\nenabled = true\n",
            "[plugins.mesh]\nenabled = true\n[plugins.meshtastic]\nenabled = false\n",
        );
        assert_eq!(m["plugins"]["mesh"]["enabled"].as_bool(), Some(false));
        assert_eq!(m["plugins"]["meshtastic"]["enabled"].as_bool(), Some(true));
    }

    #[test]
    fn a_backup_with_no_plugins_section_gets_the_current_machines_keys() {
        let current =
            "[plugins.web]\nbind = \"127.0.0.1:8080\"\n[plugins.mesh]\nserial_port = \"/dev/x\"\n";
        let m = merged(current, "[bbs]\nname = \"Only Name\"\n");
        assert_eq!(m["bbs"]["name"].as_str(), Some("Only Name"));
        assert_eq!(m["plugins"]["web"]["bind"].as_str(), Some("127.0.0.1:8080"));
        assert_eq!(m["plugins"]["mesh"]["serial_port"].as_str(), Some("/dev/x"));
    }

    #[test]
    fn an_empty_current_config_is_fine() {
        let m = merged("", "[bbs]\nname = \"N\"\ndata_dir = \"/elsewhere\"\n");
        assert_eq!(m["bbs"]["name"].as_str(), Some("N"));
        assert!(m["bbs"].get("data_dir").is_none());
    }

    #[test]
    fn a_restored_key_that_is_not_a_table_is_an_error_not_a_panic() {
        let err = merge_for_restore(
            "[plugins.web]\nbind = \"127.0.0.1:8080\"\n",
            "plugins = \"oops\"\n",
        )
        .unwrap_err();
        assert!(err.contains("not a table"), "{err}");
    }

    #[test]
    fn invalid_toml_is_reported_for_either_side() {
        assert!(validate_staged_config("[bbs\nname =").is_err());
        assert!(validate_staged_config("[bbs]\nname = \"ok\"\n").is_ok());
        assert!(merge_for_restore("[[[", "[bbs]\n")
            .unwrap_err()
            .contains("current"));
        assert!(merge_for_restore("[bbs]\n", "[[[")
            .unwrap_err()
            .contains("backup"));
    }

    #[test]
    fn comments_in_the_backup_survive_the_merge() {
        let restored = "# my welcome\n[bbs]\n# the name\nname = \"N\"\n";
        let text = merge_for_restore("", restored).unwrap();
        assert!(
            text.contains("# my welcome") && text.contains("# the name"),
            "{text}"
        );
    }

    #[test]
    fn apply_writes_the_merged_file_and_keeps_the_previous_one() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.toml");
        let staged = dir.path().join(STAGED_CONFIG_NAME);
        std::fs::write(&cfg, "[bbs]\nname = \"Old\"\ndata_dir = \"/here\"\n").unwrap();
        std::fs::write(&staged, "[bbs]\nname = \"New\"\ndata_dir = \"/there\"\n").unwrap();

        let applied = apply_config(&cfg, &staged).unwrap();

        let now = std::fs::read_to_string(&cfg).unwrap();
        assert!(
            now.contains("name = \"New\"") && now.contains("data_dir = \"/here\""),
            "{now}"
        );
        let prev = applied.previous.clone().unwrap();
        assert_eq!(prev, dir.path().join("config.toml.pre-restore"));
        assert!(std::fs::read_to_string(&prev)
            .unwrap()
            .contains("name = \"Old\""));

        revert_config(&cfg, &applied).unwrap();
        assert!(std::fs::read_to_string(&cfg)
            .unwrap()
            .contains("name = \"Old\""));
    }

    #[test]
    fn a_file_that_does_not_load_is_reverted_and_one_that_does_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.toml");
        let staged = dir.path().join(STAGED_CONFIG_NAME);
        std::fs::write(&cfg, "[bbs]\nname = \"Old\"\n").unwrap();
        std::fs::write(&staged, "[bbs]\nname = \"New\"\n").unwrap();

        // Rejected by the caller's check: the old config comes back.
        let applied = apply_config(&cfg, &staged).unwrap();
        let err = settle_applied::<()>(&cfg, &applied, || Err("no good".into())).unwrap_err();
        assert_eq!(err, "no good");
        assert!(std::fs::read_to_string(&cfg)
            .unwrap()
            .contains("name = \"Old\""));

        // Accepted: the new config stays.
        let applied = apply_config(&cfg, &staged).unwrap();
        let v = settle_applied(&cfg, &applied, || Ok(7)).unwrap();
        assert_eq!(v, 7);
        assert!(std::fs::read_to_string(&cfg)
            .unwrap()
            .contains("name = \"New\""));
    }

    #[test]
    fn a_failed_revert_is_reported_with_the_original_reason() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.toml");
        let applied = ConfigApplied {
            previous: Some(dir.path().join("gone.pre-restore")),
        };
        let err = settle_applied::<()>(&cfg, &applied, || Err("bad".into())).unwrap_err();
        assert!(
            err.starts_with("bad") && err.contains("could not be put back"),
            "{err}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_config_keeps_its_permissions() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.toml");
        let staged = dir.path().join(STAGED_CONFIG_NAME);
        std::fs::write(&cfg, "[bbs]\nname = \"Old\"\n").unwrap();
        std::fs::set_permissions(&cfg, std::fs::Permissions::from_mode(0o640)).unwrap();
        std::fs::write(&staged, "[bbs]\nname = \"New\"\n").unwrap();
        let applied = apply_config(&cfg, &staged).unwrap();
        let mode_of = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode_of(&cfg), 0o640);
        assert_eq!(mode_of(&applied.previous.clone().unwrap()), 0o640);

        // Putting the old file back must not reset its mode either.
        std::fs::set_permissions(&cfg, std::fs::Permissions::from_mode(0o600)).unwrap();
        revert_config(&cfg, &applied).unwrap();
        assert_eq!(mode_of(&cfg), 0o640);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_config_stays_a_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real.toml");
        let link = dir.path().join("config.toml");
        let staged = dir.path().join(STAGED_CONFIG_NAME);
        std::fs::write(&real, "[bbs]\nname = \"Old\"\n").unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        std::fs::write(&staged, "[bbs]\nname = \"New\"\n").unwrap();

        let applied = apply_config(&link, &staged).unwrap();
        assert!(std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert!(std::fs::read_to_string(&real)
            .unwrap()
            .contains("name = \"New\""));

        revert_config(&link, &applied).unwrap();
        assert!(std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert!(std::fs::read_to_string(&real)
            .unwrap()
            .contains("name = \"Old\""));
    }

    #[test]
    fn with_no_current_config_the_restore_creates_one_and_revert_removes_it() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.toml");
        let staged = dir.path().join(STAGED_CONFIG_NAME);
        std::fs::write(&staged, "[bbs]\nname = \"New\"\n").unwrap();
        let applied = apply_config(&cfg, &staged).unwrap();
        assert!(applied.previous.is_none());
        assert!(std::fs::read_to_string(&cfg).unwrap().contains("New"));
        revert_config(&cfg, &applied).unwrap();
        assert!(!cfg.exists());
    }

    #[test]
    fn a_failed_apply_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.toml");
        std::fs::write(&cfg, "[bbs]\nname = \"Old\"\n").unwrap();
        // Missing staged file.
        assert!(apply_config(&cfg, &dir.path().join("absent.toml")).is_err());
        // Invalid staged file.
        let bad = dir.path().join("bad.toml");
        std::fs::write(&bad, "[[[").unwrap();
        assert!(apply_config(&cfg, &bad).is_err());
        assert_eq!(
            std::fs::read_to_string(&cfg).unwrap(),
            "[bbs]\nname = \"Old\"\n"
        );
        assert!(!pre_restore_path(&cfg).exists());
    }

    #[test]
    fn an_oversized_staged_config_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.toml");
        let big = dir.path().join("big.toml");
        std::fs::write(
            &big,
            format!("# {}\n", "x".repeat(MAX_CONFIG_BYTES as usize)),
        )
        .unwrap();
        let err = apply_config(&cfg, &big).unwrap_err();
        assert!(err.contains("too large"), "{err}");
    }
}
