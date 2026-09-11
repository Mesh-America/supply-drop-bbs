//! Panic-safe helpers for building up nested `[section]` tables in a
//! [`toml_edit::DocumentMut`].
//!
//! `doc["section"]["key"] = value` (and the equivalent `Table` indexing)
//! panics if `"section"` already exists but isn't a table — e.g. a
//! hand-edited or corrupted `config.toml` with `section = 1` instead of
//! `[section]`. Every bracket-assignment call site across this workspace
//! used to guard only the *absent* case (`if doc.get(section).is_none() {
//! ... }`) and then unconditionally bracket-index, which still panics on
//! the *wrong-type* case. Under the `release-min` profile
//! (`panic = "abort"`, used for shipped release builds), that panic aborts
//! the whole process — one malformed value crashes the server for every
//! connected user on the next request that touches that section
//! (supply-drop-bbs-bn3 / #276).
//!
//! [`ensure_table`] and [`ensure_subtable`] replace that pattern: they
//! create an empty table when the key is absent (same as before), but
//! return a `Result` instead of panicking when it's present with the wrong
//! type, so the caller can turn it into a normal error response (a 400, a
//! CLI `eprintln!` + exit) instead of taking the whole process down.

use toml_edit::{DocumentMut, Item, Table};

/// Get `doc[section]` as a mutable table, creating an empty one if the key
/// is absent. Returns `Err` (never panics) if the key exists but isn't a
/// table.
pub fn ensure_table<'d>(doc: &'d mut DocumentMut, section: &str) -> Result<&'d mut Table, String> {
    if doc.get(section).is_none() {
        doc[section] = Item::Table(Table::new());
    }
    doc[section]
        .as_table_mut()
        .ok_or_else(|| format!("config.toml: [{section}] exists but is not a table"))
}

/// Get `table[key]` as a mutable sub-table, creating an empty one if the
/// key is absent. Returns `Err` (never panics) if the key exists but isn't
/// a table. Composes with [`ensure_table`] for arbitrarily deep nesting,
/// e.g. `ensure_subtable(ensure_table(doc, "plugins")?, "mesh")?`.
pub fn ensure_subtable<'t>(table: &'t mut Table, key: &str) -> Result<&'t mut Table, String> {
    if table.get(key).is_none() {
        table.insert(key, Item::Table(Table::new()));
    }
    table
        .get_mut(key)
        .and_then(Item::as_table_mut)
        .ok_or_else(|| format!("config.toml: [{key}] exists but is not a table"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_table_creates_an_absent_section() {
        let mut doc: DocumentMut = "".parse().unwrap();
        let table = ensure_table(&mut doc, "bbs").unwrap();
        table.insert("name", Item::Value("test".into()));
        assert_eq!(doc["bbs"]["name"].as_str(), Some("test"));
    }

    #[test]
    fn ensure_table_reuses_an_existing_table() {
        let mut doc: DocumentMut = "[bbs]\nname = \"existing\"\n".parse().unwrap();
        let table = ensure_table(&mut doc, "bbs").unwrap();
        assert_eq!(table.get("name").and_then(|v| v.as_str()), Some("existing"));
    }

    /// The regression this module exists to prevent: `bbs = 1` (a scalar,
    /// not a table) at the top level must produce a normal `Err`, never a
    /// panic, when a caller tries to treat `[bbs]` as a table.
    #[test]
    fn ensure_table_errors_instead_of_panicking_on_a_non_table_section() {
        let mut doc: DocumentMut = "bbs = 1\n".parse().unwrap();
        let result = ensure_table(&mut doc, "bbs");
        assert!(
            result.is_err(),
            "expected an error, not a panic, for a non-table [bbs]"
        );
    }

    #[test]
    fn ensure_subtable_errors_instead_of_panicking_on_a_non_table_key() {
        let mut doc: DocumentMut = "[plugins]\nmesh = 1\n".parse().unwrap();
        let plugins = ensure_table(&mut doc, "plugins").unwrap();
        let result = ensure_subtable(plugins, "mesh");
        assert!(
            result.is_err(),
            "expected an error, not a panic, for a non-table [plugins.mesh]"
        );
    }

    #[test]
    fn ensure_subtable_composes_for_deep_nesting() {
        let mut doc: DocumentMut = "".parse().unwrap();
        let plugins = ensure_table(&mut doc, "plugins").unwrap();
        let mesh = ensure_subtable(plugins, "mesh").unwrap();
        let radio = ensure_subtable(mesh, "radio").unwrap();
        radio.insert("frequency", Item::Value(915.0.into()));
        assert_eq!(
            doc["plugins"]["mesh"]["radio"]["frequency"].as_float(),
            Some(915.0)
        );
    }
}
