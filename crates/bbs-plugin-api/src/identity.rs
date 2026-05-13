//! Identity newtypes.
//!
//! `SessionId` and `Username` are the two identifiers that cross
//! the plugin/host boundary in every interaction. Both are
//! newtypes so the type system distinguishes them from each other
//! and from raw strings/integers.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Opaque identifier for an authenticated (or pre-auth) session.
///
/// `SessionId` is **minted by the host**. Plugins never construct
/// these from scratch; they receive a `SessionId` from
/// [`Host::create_session`](crate::Host::create_session) (or via
/// other Host APIs) and pass it back unchanged in subsequent calls.
///
/// The internal representation is a 64-bit integer. The host's
/// session manager guarantees uniqueness for the lifetime of the
/// process. Sessions are short-lived (typically ≤24h) so 64 bits
/// is plenty of headroom; it also keeps the on-the-wire encoding
/// trivially small for the mesh transport.
///
/// ## Construction
///
/// Outside of the host implementation in `bbs-core` and the
/// [`crate::testing`] module, you should not need to construct
/// these. The `__internal_new` constructor is `pub` for
/// cross-crate access but is documented as off-limits to plugin
/// code; review will catch misuse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionId(u64);

impl SessionId {
    /// Construct a `SessionId` directly from its inner value.
    /// Intended for the host implementation in `bbs-core` and
    /// for test fixtures in [`crate::testing`].
    ///
    /// **Plugin authors: do not call this.** Use the `SessionId`
    /// values you receive from the [`Host`](crate::Host).
    #[doc(hidden)]
    #[must_use]
    pub const fn __internal_new(raw: u64) -> Self {
        Self(raw)
    }

    /// The inner integer. Useful for logging and DB foreign keys
    /// where a typed `SessionId` doesn't help.
    #[must_use]
    pub const fn as_u64(self) -> u64 {
        self.0
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Hex with a "session:" prefix so log lines are
        // unambiguous and grep-friendly.
        write!(f, "session:{:016x}", self.0)
    }
}

/// A validated BBS username.
///
/// Constraints (enforced by [`Username::new`]):
///
/// - Non-empty
/// - At most 32 characters
/// - ASCII only (radio bandwidth + database collation simplicity)
/// - Visible characters only (no control codes, no whitespace)
/// - Doesn't start or end with `-` or `_`
///
/// The constraints are deliberately conservative. We can relax
/// them later (allow Unicode, longer names, etc.) more easily than
/// we can tighten them after a release.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Username(String);

impl Username {
    /// Maximum allowed username length. Mirrors the field width
    /// in the eventual database schema; bumping this is a schema
    /// migration.
    pub const MAX_LEN: usize = 32;

    /// Construct a username, validating the constraints. Returns
    /// the original input as the error so the caller can render
    /// it in the user-facing error message.
    pub fn new(raw: impl Into<String>) -> Result<Self, InvalidUsername> {
        // Strip a leading '@' so mesh users can type "@alice" and get "alice".
        let raw = raw.into();
        let raw = raw.strip_prefix('@').unwrap_or(&raw).to_ascii_lowercase();
        Self::validate(&raw)?;
        Ok(Self(raw))
    }

    fn validate(raw: &str) -> Result<(), InvalidUsername> {
        Self::validate_chars(raw)?;
        // Reserved system usernames — blocked at registration but allowed in
        // storage/serde so the host can round-trip messages it sent as "bbs".
        if matches!(raw, "bbs" | "system") {
            return Err(InvalidUsername::Reserved(raw.to_owned()));
        }
        Ok(())
    }

    /// Character-level validation only (no reserved-name check).
    /// Used by `TryFrom<String>` so stored system messages can be read back.
    fn validate_chars(raw: &str) -> Result<(), InvalidUsername> {
        if raw.is_empty() {
            return Err(InvalidUsername::Empty);
        }
        if raw.len() > Self::MAX_LEN {
            return Err(InvalidUsername::TooLong {
                actual: raw.len(),
                max: Self::MAX_LEN,
            });
        }
        if !raw.is_ascii() {
            return Err(InvalidUsername::NonAscii);
        }
        for c in raw.chars() {
            if c.is_control() || c.is_whitespace() || c == '@' {
                return Err(InvalidUsername::DisallowedCharacter(c));
            }
        }
        // Bracket characters: no leading/trailing dash or underscore.
        let first = raw.chars().next().expect("non-empty checked above");
        let last = raw.chars().next_back().expect("non-empty checked above");
        if matches!(first, '-' | '_') || matches!(last, '-' | '_') {
            return Err(InvalidUsername::BracketCharacter);
        }
        Ok(())
    }

    /// Construct a reserved system username, bypassing the reserved-name check.
    ///
    /// Intended for the host implementation (`bbs-core`) only — the `"bbs"` and
    /// `"system"` senders used for host-generated messages. Plugin authors should
    /// never call this; `Username::new` rejects those names deliberately.
    #[doc(hidden)]
    #[must_use]
    pub fn __internal_system(raw: &'static str) -> Self {
        Self(raw.to_owned())
    }

    /// Borrow the validated string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Username {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for Username {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl From<Username> for String {
    fn from(value: Username) -> Self {
        value.0
    }
}

impl TryFrom<String> for Username {
    type Error = InvalidUsername;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        // Use character-level validation only so system senders ("bbs", "system")
        // stored in the database round-trip correctly. The reserved-name check is
        // enforced in Username::new(), which is the registration/input path.
        let raw = value
            .strip_prefix('@')
            .unwrap_or(&value)
            .to_ascii_lowercase();
        Self::validate_chars(&raw)?;
        Ok(Self(raw))
    }
}

/// Why a username failed validation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InvalidUsername {
    /// Empty string.
    #[error("username must not be empty")]
    Empty,

    /// Longer than [`Username::MAX_LEN`].
    #[error("username is {actual} bytes; maximum is {max}")]
    TooLong {
        /// The actual byte length.
        actual: usize,
        /// The maximum allowed.
        max: usize,
    },

    /// Contains non-ASCII bytes.
    #[error("username must be ASCII")]
    NonAscii,

    /// Contains a control or whitespace character.
    #[error("username contains a disallowed character: {0:?}")]
    DisallowedCharacter(char),

    /// Starts or ends with `-` or `_`.
    #[error("username must not start or end with `-` or `_`")]
    BracketCharacter,

    /// Reserved system username.
    #[error("'{0}' is a reserved username")]
    Reserved(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_id_display_format() {
        let s = SessionId::__internal_new(0x42);
        assert_eq!(format!("{s}"), "session:0000000000000042");
    }

    #[test]
    fn session_id_serde_roundtrip() {
        let s = SessionId::__internal_new(0xdead_beef);
        let json = serde_json::to_string(&s).unwrap();
        // Serialised as bare integer thanks to #[serde(transparent)].
        assert_eq!(json, "3735928559");
        let back: SessionId = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn username_accepts_normal_names() {
        for name in ["alice", "alice42", "a.b-c_d", "x", &"a".repeat(32)] {
            assert!(Username::new(name).is_ok(), "expected {name:?} to be valid",);
        }
    }

    #[test]
    fn username_rejects_empty() {
        assert_eq!(Username::new(""), Err(InvalidUsername::Empty));
    }

    #[test]
    fn username_rejects_too_long() {
        let too_long = "a".repeat(33);
        match Username::new(too_long) {
            Err(InvalidUsername::TooLong {
                actual: 33,
                max: 32,
            }) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn username_rejects_non_ascii() {
        assert_eq!(Username::new("álice"), Err(InvalidUsername::NonAscii));
    }

    #[test]
    fn username_rejects_whitespace() {
        match Username::new("alice bob") {
            Err(InvalidUsername::DisallowedCharacter(' ')) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn username_rejects_control_chars() {
        match Username::new("alice\nbob") {
            Err(InvalidUsername::DisallowedCharacter('\n')) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn username_rejects_leading_or_trailing_punct() {
        for bad in ["-alice", "_alice", "alice-", "alice_"] {
            assert_eq!(
                Username::new(bad),
                Err(InvalidUsername::BracketCharacter),
                "expected {bad:?} to be rejected"
            );
        }
    }

    #[test]
    fn username_rejects_reserved_names() {
        for reserved in ["bbs", "system"] {
            assert!(
                matches!(Username::new(reserved), Err(InvalidUsername::Reserved(_))),
                "expected {reserved:?} to be rejected as reserved"
            );
        }
    }
}
