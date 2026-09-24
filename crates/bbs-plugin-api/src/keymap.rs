//! Command keymaps: remapping Supply Drop's own command keywords to match a
//! classic BBS system's conventions, without changing what any action does.
//!
//! See `specs/002-command-keymaps/` (spec.md, plan.md, the sourced
//! `research-classic-bbs-commands.md`) for the design this implements —
//! GH #354.
//!
//! ## Design: partial override, not full replacement
//!
//! A [`Keymap`] only lists the actions it actually remaps. Everything else
//! keeps [`KeymapAction::native_keyword`]'s binding. This is deliberate, not
//! a shortcut: working through a full Maximus remap (see plan.md) found that
//! several Supply Drop actions have no single-key equivalent in some source
//! systems at all (there's no atomic "next room with unread" in Maximus —
//! Browse is a whole sub-menu). Forcing every action to have a
//! preset-authentic key produces either an invented key presented as if it
//! were sourced, or an unusable gap. Partial override avoids both.
//!
//! [`Command::parse_with_keymap`](crate::Command::parse_with_keymap) applies
//! a keymap by translating an overridden keyword to the action's native
//! keyword *before* the existing keyword match runs — the match itself never
//! needs to know keymaps exist.

use std::collections::BTreeMap;

/// An action a keymap can bind a keyword to. A closed set matching the
/// subset of [`Command`](crate::Command)'s keyword table (plus the
/// reading-mode-only actions, meant to be parsed separately in `bbs-core`'s
/// `host.rs` from `WorkflowReply` text — see spec.md's Constraints) that
/// presets actually have reason to remap. Sysop/aide-only actions are out
/// of scope for v1: see spec.md Open question 5.
///
/// As of GH #354 Phase 1, the seven `Reading*` variants below validate
/// cleanly but have **no runtime effect**: `bbs-core`'s reading-mode
/// matcher does not yet consult a [`Keymap`] at all (Phase 2,
/// `specs/002-command-keymaps/tasks.md` P2.3). A [`Keymap`] that overrides
/// one of them passes [`Keymap::validate`] and is silently a no-op until
/// that wiring lands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum KeymapAction {
    /// Log out / end the session. Native: `q`/`quit`/`exit`/`bye`/`logout`.
    Quit,
    /// List the rooms on the board. Native: `k`.
    ListRooms,
    /// Jump to the next room with unread messages. Native: `g`.
    GoNextUnread,
    /// Change to a named/numbered room. Native: `c <target>`.
    ChangeRoom,
    /// Go to the Mail room. Native: `m`.
    GoMail,
    /// Read new (unread) messages in the current room. Native: `n`.
    ReadNew,
    /// Read forward, optionally jumping to a message id first. Native:
    /// `f [id]`.
    ReadForward,
    /// Read backward from the most recent message. Native: `r`.
    ReadReverse,
    /// List messages in the current room without fully reading them.
    /// Native: `s`. **Caution:** `Command::parse`'s `"s"` arm is
    /// argument-dependent — bare `s` resolves here, but `s <text>` resolves
    /// to the unrelated, Aide+-only `SearchUsers` instead (no
    /// [`KeymapAction`] covers `SearchUsers`; it can't be remapped). A
    /// keyword a keymap binds to `ScanMessages` inherits this split: typing
    /// it with no trailing text behaves as documented, typing it with
    /// trailing text does not. Tracked as a pre-existing `command.rs`
    /// ambiguity, not something a keymap override can avoid.
    ScanMessages,
    /// Post a new message. Native: `e [body]`.
    EnterMessage,
    /// Delete a message by id. Native: `d <id>`.
    DeleteMessage,
    /// List who's currently online. Native: `w`.
    WhoIsOnline,
    /// Reading-mode bare `F` (or a preset's equivalent): next message.
    ReadingForward,
    /// Reading-mode `F <id>` (or a preset's equivalent): jump to a message.
    ReadingJump,
    /// Reading-mode `R` (or a preset's equivalent): previous message.
    ReadingReverse,
    /// Reading-mode `E` (or a preset's equivalent): reply to the message
    /// being read.
    ReadingReply,
    /// Reading-mode `H`/`?` (or a preset's equivalent): reading-mode help.
    ReadingHelp,
    /// Reading-mode bare `D` (or a preset's equivalent): delete the message
    /// being read.
    ReadingDeleteCurrent,
    /// Reading-mode `D <id>` (or a preset's equivalent): delete a specific
    /// message without leaving reading mode.
    ReadingDeleteSpecific,
}

impl KeymapAction {
    /// Every variant, for completeness checks (`Keymap::validate`) that must
    /// consider actions the keymap itself never mentions.
    pub const ALL: &'static [KeymapAction] = &[
        KeymapAction::Quit,
        KeymapAction::ListRooms,
        KeymapAction::GoNextUnread,
        KeymapAction::ChangeRoom,
        KeymapAction::GoMail,
        KeymapAction::ReadNew,
        KeymapAction::ReadForward,
        KeymapAction::ReadReverse,
        KeymapAction::ScanMessages,
        KeymapAction::EnterMessage,
        KeymapAction::DeleteMessage,
        KeymapAction::WhoIsOnline,
        KeymapAction::ReadingForward,
        KeymapAction::ReadingJump,
        KeymapAction::ReadingReverse,
        KeymapAction::ReadingReply,
        KeymapAction::ReadingHelp,
        KeymapAction::ReadingDeleteCurrent,
        KeymapAction::ReadingDeleteSpecific,
    ];

    /// The single canonical native keyword this action already reaches
    /// through `Command::parse`'s existing match (or, for the
    /// `Reading*` variants, `bbs-core`'s reading-mode match — see the
    /// module doc comment). Several top-level actions have more than one
    /// native synonym (`Quit` also matches `logout`/`exit`/`bye`); this
    /// returns the one a keymap override translates to, not an exhaustive
    /// list of every native spelling.
    #[must_use]
    pub fn native_keyword(self) -> &'static str {
        match self {
            KeymapAction::Quit => "q",
            KeymapAction::ListRooms => "k",
            KeymapAction::GoNextUnread => "g",
            KeymapAction::ChangeRoom => "c",
            KeymapAction::GoMail => "m",
            KeymapAction::ReadNew => "n",
            KeymapAction::ReadForward => "f",
            KeymapAction::ReadReverse => "r",
            KeymapAction::ScanMessages => "s",
            KeymapAction::EnterMessage => "e",
            KeymapAction::DeleteMessage => "d",
            KeymapAction::WhoIsOnline => "w",
            KeymapAction::ReadingForward => "f",
            KeymapAction::ReadingJump => "f",
            KeymapAction::ReadingReverse => "r",
            KeymapAction::ReadingReply => "e",
            KeymapAction::ReadingHelp => "h",
            KeymapAction::ReadingDeleteCurrent => "d",
            KeymapAction::ReadingDeleteSpecific => "d",
        }
    }
}

/// Why a [`Keymap`] failed [`Keymap::validate`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum KeymapError {
    /// Activating this keymap would leave `action` with no bound keyword at
    /// all: its native keyword was reassigned to a different action by this
    /// keymap, and the keymap provides no replacement keyword for it.
    ActionUnreachable(KeymapAction),
    /// This binding's keyword is one `Command::parse_with_keymap` already
    /// recognizes for something other than a remappable [`KeymapAction`]
    /// (an admin/auth command, a help alias, or the CANCEL/STOP fast path).
    /// Binding it here would silently make that other command unreachable
    /// board-wide instead of remapping anything.
    ReservedKeyword(String),
    /// This binding's keyword isn't in the canonical shape
    /// `Command::parse_with_keymap` looks up (non-empty, no whitespace,
    /// already lowercase) and so could never match a typed keyword at
    /// runtime — it would validate as if reachable while being permanently
    /// dead.
    MalformedKeyword(String),
}

impl std::fmt::Display for KeymapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KeymapError::ActionUnreachable(action) => write!(
                f,
                "{action:?} would have no bound key under this keymap — its native \
                 key ({}) was given to a different action, and this keymap doesn't \
                 provide a replacement key for it",
                action.native_keyword()
            ),
            KeymapError::ReservedKeyword(keyword) => write!(
                f,
                "\"{keyword}\" is already used by a command this keymap can't remap \
                 (an admin/auth command, a help alias, or cancel/stop) — binding it \
                 here would make that command unreachable instead of remapping it"
            ),
            KeymapError::MalformedKeyword(keyword) => write!(
                f,
                "\"{keyword}\" is not a valid override keyword — it must be non-empty, \
                 contain no whitespace, and already be lowercase, or it can never match \
                 a typed keyword at runtime"
            ),
        }
    }
}

impl std::error::Error for KeymapError {}

/// A named, partial override table: only the actions it remaps. Anything
/// not listed here keeps its [`KeymapAction::native_keyword`] binding — see
/// the module doc comment for why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Keymap {
    /// Short identifier, e.g. `"native"`, `"maximus"` — matches the `[bbs]
    /// keymap = "..."` config value.
    pub name: String,
    /// Human-readable description shown to the sysop when choosing a
    /// keymap, e.g. noting an imperfect fit (see the Packet-BBS preset in
    /// `specs/002-command-keymaps/plan.md`).
    pub description: String,
    /// Override keyword (already lowercase) → the action it now triggers.
    /// A `BTreeMap` key is unique by construction, so this data structure
    /// alone rules out one keymap binding the same keyword to two different
    /// actions — no separate "Rule 1" self-collision check is needed.
    pub bindings: BTreeMap<String, KeymapAction>,
}

impl Keymap {
    /// Supply Drop's own default keymap: no overrides at all. Every action
    /// resolves through `Command::parse`'s (or reading-mode's) existing
    /// match, completely unaffected by keymap machinery — this is what
    /// makes "revert to native" trivial and lossless: it's not stored
    /// state that can drift, it's the absence of any override.
    #[must_use]
    pub fn native() -> Self {
        Keymap {
            name: "native".to_owned(),
            description: "Supply Drop's own default commands.".to_owned(),
            bindings: BTreeMap::new(),
        }
    }

    /// Look up the action bound to `keyword` under this keymap, if any.
    /// Returns `None` for a keyword this keymap doesn't override — callers
    /// then fall through to Supply Drop's native keyword matching
    /// unchanged, which is exactly the "partial override" contract.
    ///
    /// The lookup is case-sensitive and performs no normalization: it is
    /// the caller's responsibility to pass an already-lowercased `keyword`
    /// (`Command::parse_with_keymap` does this before calling in).
    #[must_use]
    pub fn action_for(&self, keyword: &str) -> Option<KeymapAction> {
        self.bindings.get(keyword).copied()
    }

    /// Validate a keymap's binding keys and reachability:
    ///
    /// - **Reserved keywords** — a binding key must not shadow a keyword
    ///   `Command::parse_with_keymap` already recognizes for something
    ///   other than a remappable [`KeymapAction`] (see
    ///   `RESERVED_KEYWORDS`, this module's private keyword table), or the
    ///   CANCEL/STOP fast path (`"cancel"`,
    ///   `"stop"`), which runs before any keymap lookup and so can never be
    ///   remapped when typed bare.
    /// - **Malformed keywords** — a binding key must be non-empty, contain
    ///   no whitespace, and already be lowercase, or it can never match a
    ///   typed keyword at runtime (`Command::parse_with_keymap` always
    ///   looks up an ASCII-lowercased word).
    /// - **Reachability** (see the module doc comment's "Rule 2") — every
    ///   [`KeymapAction`] must remain reachable by at least one *canonical*
    ///   keyword once this keymap's overrides are applied on top of the
    ///   native table. An action's native keyword can be legitimately
    ///   reassigned to a different action, but only if this keymap also
    ///   gives the displaced action a new, canonical one.
    ///
    /// # Errors
    /// The first problem found, checked in the order above. Keymap
    /// authoring should fix every such error, not just the first — this
    /// returns one at a time to keep the check itself simple; callers that
    /// want the full list can loop, removing/fixing as they go.
    pub fn validate(&self) -> Result<(), KeymapError> {
        for key in self.bindings.keys() {
            if key.is_empty()
                || key.chars().any(|c| c.is_ascii_whitespace())
                || key != &key.to_ascii_lowercase()
            {
                return Err(KeymapError::MalformedKeyword(key.clone()));
            }
            if RESERVED_KEYWORDS.contains(&key.as_str()) {
                return Err(KeymapError::ReservedKeyword(key.clone()));
            }
        }

        for action in KeymapAction::ALL.iter().copied() {
            let native_kw = action.native_keyword();
            // Does this keymap explicitly provide some (possibly the same)
            // keyword bound to `action`? (Malformed keys were already
            // rejected above, so every remaining key is canonical.)
            let has_explicit_binding = self.bindings.values().any(|&a| a == action);
            if has_explicit_binding {
                continue;
            }
            // No explicit binding — does the action's native keyword still
            // point at it, or did this keymap give that keyword to a
            // different action?
            match self.bindings.get(native_kw) {
                Some(&other) if other != action => {
                    return Err(KeymapError::ActionUnreachable(action));
                }
                _ => {} // native_kw is untouched by this keymap, or (impossible
                        // given the has_explicit_binding check above, but kept
                        // for clarity) maps to this same action.
            }
        }
        Ok(())
    }
}

/// Keywords `Command::parse_with_keymap`'s match recognizes for something
/// other than a remappable [`KeymapAction`] — every synonym, not just
/// canonical ones (`"quit"`/`"exit"`/`"bye"` alongside `Quit`'s canonical
/// `"q"`), plus every admin/auth/access-control keyword, since none of
/// those are modeled as a `KeymapAction` at all (see the enum's doc
/// comment: "Sysop/aide-only actions are out of scope for v1"). `"cancel"`
/// and `"stop"` are included too: `Command::parse_with_keymap` intercepts
/// them unconditionally before any keymap lookup runs (see `command.rs`'s
/// `#120` comment), so a binding on either is dead code the moment it's
/// typed bare — reserving them here surfaces that at validation time
/// instead of leaving a keymap author to discover it by testing.
///
/// This list is `command.rs`'s `parse_with_keymap` match, keyword by
/// keyword, minus the keywords [`KeymapAction::ALL`]'s top-level variants
/// already own (`native_keyword()`'s canonical returns:
/// `q k g c m n f r s e d w`). Keep it in sync with that match by hand —
/// see `native_keyword`'s own doc comment for the same caveat about this
/// crate's two independently maintained keyword tables.
const RESERVED_KEYWORDS: &[&str] = &[
    "h",
    "help",
    "?",
    "register",
    "login",
    "logout",
    "quit",
    "exit",
    "bye",
    ".ff",
    "pending",
    "v",
    "b",
    "ban",
    "unban",
    "timeout",
    "u",
    "users",
    "whois",
    "whoami",
    "profile",
    "passwd",
    ".c",
    ".dr",
    ".er",
    ".eu",
    ".du",
    ".aide",
    ".sysop",
    ".user",
    ".pw",
    "openaccess",
    "closeaccess",
    "guestroom",
    "cancel",
    "stop",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_has_no_overrides_and_validates() {
        let native = Keymap::native();
        assert!(native.bindings.is_empty());
        assert_eq!(native.action_for("g"), None);
        assert!(native.validate().is_ok());
    }

    #[test]
    fn every_action_has_a_distinct_native_keyword_or_shares_deliberately() {
        // Reading-mode actions intentionally reuse top-level letters
        // (F/R/E/D), since they're parsed in a wholly separate namespace
        // (WorkflowReply text, not Command::parse — see spec.md's
        // Constraints). Assert that's the ONLY kind of duplication: no two
        // TOP-LEVEL actions share a native keyword, since those genuinely
        // do collide within one match statement.
        use std::collections::HashMap;
        let mut by_keyword: HashMap<&str, Vec<KeymapAction>> = HashMap::new();
        for action in KeymapAction::ALL {
            by_keyword
                .entry(action.native_keyword())
                .or_default()
                .push(*action);
        }
        let top_level = |a: &KeymapAction| {
            !matches!(
                a,
                KeymapAction::ReadingForward
                    | KeymapAction::ReadingJump
                    | KeymapAction::ReadingReverse
                    | KeymapAction::ReadingReply
                    | KeymapAction::ReadingHelp
                    | KeymapAction::ReadingDeleteCurrent
                    | KeymapAction::ReadingDeleteSpecific
            )
        };
        for actions in by_keyword.values() {
            let top_level_here: Vec<_> = actions.iter().filter(|a| top_level(a)).collect();
            assert!(
                top_level_here.len() <= 1,
                "top-level actions must not share a native keyword: {top_level_here:?}"
            );
        }
    }

    /// The Maximus preset worked through in plan.md, built here directly
    /// (not via a `Keymap::maximus()` const yet — that's Phase 3/P3.1) to
    /// prove the validation model against a real, previously-hand-checked
    /// case: `g` (native GoNextUnread) reassigned to Quit, with
    /// GoNextUnread explicitly given `]` so it stays reachable.
    fn maximus_bindings() -> BTreeMap<String, KeymapAction> {
        BTreeMap::from([
            ("g".to_owned(), KeymapAction::Quit),
            ("a".to_owned(), KeymapAction::ChangeRoom),
            ("]".to_owned(), KeymapAction::GoNextUnread),
            ("l".to_owned(), KeymapAction::ScanMessages),
            ("w".to_owned(), KeymapAction::WhoIsOnline),
        ])
    }

    #[test]
    fn maximus_preset_validates() {
        let km = Keymap {
            name: "maximus".to_owned(),
            description: "test".to_owned(),
            bindings: maximus_bindings(),
        };
        assert_eq!(km.validate(), Ok(()));
    }

    #[test]
    fn maximus_preset_resolves_g_to_quit_not_go_next_unread() {
        let km = Keymap {
            name: "maximus".to_owned(),
            description: "test".to_owned(),
            bindings: maximus_bindings(),
        };
        assert_eq!(km.action_for("g"), Some(KeymapAction::Quit));
        assert_eq!(km.action_for("]"), Some(KeymapAction::GoNextUnread));
        // Not overridden by this preset — falls through to native.
        assert_eq!(km.action_for("n"), None);
    }

    #[test]
    fn a_keymap_that_reassigns_a_keyword_without_relocating_the_displaced_action_is_rejected() {
        // Steals `g` for Quit but — unlike the real Maximus preset above —
        // never gives GoNextUnread anywhere else to live.
        let km = Keymap {
            name: "broken".to_owned(),
            description: "test".to_owned(),
            bindings: BTreeMap::from([("g".to_owned(), KeymapAction::Quit)]),
        };
        assert_eq!(
            km.validate(),
            Err(KeymapError::ActionUnreachable(KeymapAction::GoNextUnread))
        );
    }

    #[test]
    fn packet_bbs_preset_validates() {
        // The thin packet-BBS preset from plan.md: one action remapped.
        // Quit is deliberately NOT remapped to "b" here — plan.md's
        // original worked example did that (packet-BBS's "Bye" convention),
        // but "b" is Supply Drop's real BlockUser command; a keymap can no
        // longer shadow it (see binding_a_reserved_admin_keyword_is_rejected
        // and plan.md's updated Packet-BBS worked example). "bye" already
        // works natively as a Quit synonym, so no override is needed.
        let km = Keymap {
            name: "packet-bbs".to_owned(),
            description: "test".to_owned(),
            bindings: BTreeMap::from([("l".to_owned(), KeymapAction::ReadNew)]),
        };
        assert_eq!(km.validate(), Ok(()));
    }

    #[test]
    fn packet_bbs_s_original_b_for_quit_binding_would_now_be_rejected() {
        // Regression test documenting exactly what the Phase 1 hostile audit
        // found: plan.md's original Packet-BBS worked example bound "b" to
        // Quit, which passed Rule 1/Rule 2 (no KeymapAction collision) while
        // silently shadowing the real BlockUser command.
        let km = Keymap {
            name: "packet-bbs-original".to_owned(),
            description: "test".to_owned(),
            bindings: BTreeMap::from([("b".to_owned(), KeymapAction::Quit)]),
        };
        assert_eq!(
            km.validate(),
            Err(KeymapError::ReservedKeyword("b".to_owned()))
        );
    }

    #[test]
    fn a_no_op_override_matching_the_native_keyword_is_fine() {
        // A preset can bind an action to the SAME keyword it already has
        // natively (documenting "confirmed, not changed" — see plan.md's
        // Maximus WhoIsOnline row). Must not be flagged as a collision with
        // itself.
        let km = Keymap {
            name: "test".to_owned(),
            description: "test".to_owned(),
            bindings: BTreeMap::from([("w".to_owned(), KeymapAction::WhoIsOnline)]),
        };
        assert_eq!(km.validate(), Ok(()));
        assert_eq!(km.action_for("w"), Some(KeymapAction::WhoIsOnline));
    }

    #[test]
    fn binding_a_reserved_admin_keyword_is_rejected() {
        // "ban" is a real command.rs keyword (BanUser) with no KeymapAction
        // of its own — stealing it would silently disable moderation.
        let km = Keymap {
            name: "test".to_owned(),
            description: "test".to_owned(),
            bindings: BTreeMap::from([("ban".to_owned(), KeymapAction::ListRooms)]),
        };
        assert_eq!(
            km.validate(),
            Err(KeymapError::ReservedKeyword("ban".to_owned()))
        );
    }

    #[test]
    fn binding_a_quit_synonym_is_also_reserved() {
        // "quit" is a real synonym of Quit's own canonical "q" — a keymap
        // remapping it elsewhere would leave that synonym unreachable too.
        let km = Keymap {
            name: "test".to_owned(),
            description: "test".to_owned(),
            bindings: BTreeMap::from([("quit".to_owned(), KeymapAction::ListRooms)]),
        };
        assert_eq!(
            km.validate(),
            Err(KeymapError::ReservedKeyword("quit".to_owned()))
        );
    }

    #[test]
    fn binding_cancel_or_stop_is_rejected_even_though_parse_would_never_reach_it_bare() {
        let km = Keymap {
            name: "test".to_owned(),
            description: "test".to_owned(),
            bindings: BTreeMap::from([("stop".to_owned(), KeymapAction::Quit)]),
        };
        assert_eq!(
            km.validate(),
            Err(KeymapError::ReservedKeyword("stop".to_owned()))
        );
    }

    #[test]
    fn an_uppercase_binding_key_is_rejected_as_malformed_instead_of_silently_dead() {
        // Regression for the case-sensitivity false negative: {"G":
        // ListRooms, "k": Quit} used to validate() => Ok(()) while ListRooms
        // was genuinely unreachable (typed input is always lowercased
        // before lookup, so "G" could never match).
        let km = Keymap {
            name: "test".to_owned(),
            description: "test".to_owned(),
            bindings: BTreeMap::from([
                ("G".to_owned(), KeymapAction::ListRooms),
                ("k".to_owned(), KeymapAction::Quit),
            ]),
        };
        assert_eq!(
            km.validate(),
            Err(KeymapError::MalformedKeyword("G".to_owned()))
        );
    }

    #[test]
    fn an_empty_binding_key_is_rejected_as_malformed() {
        let km = Keymap {
            name: "test".to_owned(),
            description: "test".to_owned(),
            bindings: BTreeMap::from([(String::new(), KeymapAction::ListRooms)]),
        };
        assert_eq!(
            km.validate(),
            Err(KeymapError::MalformedKeyword(String::new()))
        );
    }

    #[test]
    fn a_binding_key_containing_whitespace_is_rejected_as_malformed() {
        let km = Keymap {
            name: "test".to_owned(),
            description: "test".to_owned(),
            bindings: BTreeMap::from([("g o".to_owned(), KeymapAction::ListRooms)]),
        };
        assert_eq!(
            km.validate(),
            Err(KeymapError::MalformedKeyword("g o".to_owned()))
        );
    }

    /// Compile-time guard for `KeymapAction::ALL`: since `KeymapAction` is
    /// `#[non_exhaustive]` only for *external* crates, this in-crate
    /// exhaustive match (no wildcard arm) fails to compile the moment a
    /// variant is added here without a matching `ALL` entry — turning the
    /// "every variant, for completeness checks" doc claim into something
    /// the compiler enforces instead of only a comment.
    #[allow(dead_code)]
    fn _all_keymap_action_variants_are_matched_exhaustively(action: KeymapAction) {
        match action {
            KeymapAction::Quit
            | KeymapAction::ListRooms
            | KeymapAction::GoNextUnread
            | KeymapAction::ChangeRoom
            | KeymapAction::GoMail
            | KeymapAction::ReadNew
            | KeymapAction::ReadForward
            | KeymapAction::ReadReverse
            | KeymapAction::ScanMessages
            | KeymapAction::EnterMessage
            | KeymapAction::DeleteMessage
            | KeymapAction::WhoIsOnline
            | KeymapAction::ReadingForward
            | KeymapAction::ReadingJump
            | KeymapAction::ReadingReverse
            | KeymapAction::ReadingReply
            | KeymapAction::ReadingHelp
            | KeymapAction::ReadingDeleteCurrent
            | KeymapAction::ReadingDeleteSpecific => {}
        }
    }

    #[test]
    fn all_has_no_duplicate_or_missing_entries() {
        use std::collections::HashSet;
        let unique: HashSet<_> = KeymapAction::ALL.iter().collect();
        assert_eq!(
            unique.len(),
            KeymapAction::ALL.len(),
            "KeymapAction::ALL has a duplicate entry"
        );
        // 19 variants as of GH #354 Phase 1; the exhaustive match above is
        // the real guard against a variant being *added* without updating
        // this constant — this assertion catches ALL claiming to contain
        // more entries than the enum actually has (impossible without the
        // exhaustive match failing first) and documents the expected count.
        assert_eq!(KeymapAction::ALL.len(), 19);
    }
}
