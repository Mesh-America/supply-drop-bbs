//! Guard: no user-facing message may hard-code a key that a keymap can move.
//!
//! Every message that names a key must take it from the active keymap
//! (`Keymap::key` / `Keymap::key_with`), so a board on another preset is never
//! told to press a key that does not work there. This test scans the
//! non-test source of every crate for text that looks like a key hint built
//! around one of the keys a keymap can rebind, and fails on a new one.
//!
//! What counts as a hint, for the native letters `Q K G C M N F R S E D W`:
//!
//! - an instruction word followed by the letter: `Type N`, `press K`,
//!   `Reply 'M'`, `or Q to log out`;
//! - the letter opening a help or prompt line: `F - Forward`, `Q — log out`,
//!   `F <id>`.
//!
//! Fixed keys (help, `register`/`login`, `.` and `C` in a draft, `V`, `X` to
//! exit reading) are not remappable, so they are allowed, and each allowed
//! line is listed below with the reason.

use std::path::{Path, PathBuf};

/// The native keys a keymap can move.
const REMAPPABLE: &[&str] = &["Q", "K", "G", "C", "M", "N", "F", "R", "S", "E", "D", "W"];

/// Words that introduce a key the user should type.
const LEAD_WORDS: &[&str] = &[
    "type", "press", "reply", "use", "or", "enter", "send", "try",
];

/// Source lines allowed to name a remappable letter, with the reason. Each
/// entry is a substring of the line.
const ALLOWED: &[(&str, &str)] = &[
    // `C` cancels a draft while composing; compose keys are not keymap actions.
    ("Type . to send, C to cancel", "compose keys are fixed"),
    // `.C` is the sysop's create-room command, a fixed word.
    (".C — create a new room", "fixed sysop command"),
];

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if path.is_dir() {
            if matches!(name, "tests" | "target" | "node_modules" | "web") {
                continue;
            }
            rust_files(&path, out);
        } else if name.ends_with(".rs") {
            out.push(path);
        }
    }
}

/// The part of a source file that is not `#[cfg(test)]` code.
fn non_test_source(text: &str) -> &str {
    match text.find("#[cfg(test)]") {
        Some(i) => &text[..i],
        None => text,
    }
}

fn strip_punct(word: &str) -> &str {
    word.trim_matches(|c: char| matches!(c, '\'' | '"' | '(' | ')' | ',' | '.' | ':' | ';' | '`'))
}

/// Why `line` looks like a hard-coded key hint, if it does.
fn hint_in(line: &str) -> Option<String> {
    let words: Vec<&str> = line.split_whitespace().collect();
    for (i, raw) in words.iter().enumerate() {
        let word = strip_punct(raw);
        if !REMAPPABLE.contains(&word) {
            continue;
        }
        if i > 0 {
            let prev = strip_punct(words[i - 1]).to_lowercase();
            if LEAD_WORDS.contains(&prev.as_str()) {
                return Some(format!("`{} {word}`", words[i - 1]));
            }
        }
        if let Some(next) = words.get(i + 1) {
            let opens_line =
                i == 0 || words[i - 1].ends_with("\\n\\") || words[i - 1].ends_with("\\n");
            let next_is_hint = matches!(*next, "-" | "—") || next.starts_with('<');
            if opens_line && next_is_hint && raw.trim_matches('"') == word {
                return Some(format!("`{word} {next}`"));
            }
        }
    }
    None
}

#[test]
fn no_message_hard_codes_a_remappable_key() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    rust_files(&root.join("crates"), &mut files);
    rust_files(&root.join("src"), &mut files);
    assert!(files.len() > 20, "scan found only {} files", files.len());

    let mut found = Vec::new();
    for file in &files {
        let text = std::fs::read_to_string(file).unwrap();
        for (n, line) in non_test_source(&text).lines().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") || trimmed.starts_with("///") || trimmed.starts_with("//!")
            {
                continue;
            }
            if ALLOWED.iter().any(|(allowed, _)| line.contains(allowed)) {
                continue;
            }
            if let Some(why) = hint_in(line) {
                found.push(format!(
                    "{}:{}: {why}: {}",
                    file.strip_prefix(&root).unwrap_or(file).display(),
                    n + 1,
                    line.trim()
                ));
            }
        }
    }
    assert!(
        found.is_empty(),
        "hard-coded key hints found. Build the text from the active keymap \
         (`Keymap::key`), or, if the key is not remappable, add the line to \
         ALLOWED in tests/keymap_hints.rs with the reason:\n{}",
        found.join("\n")
    );
}

#[test]
fn the_scanner_catches_the_hints_it_is_meant_to() {
    for bad in [
        r#"format!("Now in: {}. Type N to read.", room)"#,
        r#""You have mail. Reply 'M' to read.""#,
        r#""(more — press K again for the next page)""#,
        r#""Your account is pending. Type H for help, or Q to log out.""#,
        r#""F - Forward  R - Backward""#,
        r#""F <id> to start from a specific message""#,
        r#""Q — log out""#,
    ] {
        assert!(hint_in(bad).is_some(), "should flag: {bad}");
    }
    for ok in [
        r#""Type H for help.""#,
        r#""Type 'H' for commands.""#,
        r#""use V {} first.""#,
        r#""X - Exit""#,
        r#"format!("{} - Forward", keymap.key(KeymapAction::ReadingForward))"#,
        r#"let k = KeymapAction::ReadNew;"#,
        r#""Usage: {}""#,
    ] {
        assert!(hint_in(ok).is_none(), "should not flag: {ok}");
    }
}
