//! `User` — a registered BBS account.
//!
//! Users have two orthogonal axes of state:
//!
//! - [`UserStatus`] — the **lifecycle** state. Is this account
//!   usable? `Active`, `Banned`, `Deleted`.
//! - `permission_level` (from `bbs-plugin-api`) — the **authority
//!   tier**. `Unvalidated`, `User`, `Aide`, `Sysop`.
//!
//! Treat them as independent. A pending registration is
//! `status=Active, permission_level=Unvalidated`. A previously-
//! sysop user who's been banned is `status=Banned,
//! permission_level=Sysop` — the level is preserved so an
//! eventual unban restores authority correctly.

use crate::ids::UserId;
use crate::timestamp::Timestamp;
use bbs_plugin_api::{PermissionLevel, Username};
use serde::{Deserialize, Serialize};
use std::fmt;

/// Lifecycle state of a user account.
///
/// `repr(u8)` because the value goes into the DB; the discriminants
/// are part of the wire format. Adding a variant is non-breaking;
/// changing a discriminant is a schema migration.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum UserStatus {
    /// Account is in normal operation. The
    /// `permission_level` decides what the user can actually do.
    Active = 0,

    /// Account has been disabled by a sysop. Login is rejected;
    /// existing sessions terminated. Audit log records who banned
    /// and when.
    Banned = 1,

    /// Soft-deleted. The username is reserved (no-one else can
    /// register it) but the user can't log in. Their messages
    /// remain — deletion of identity is not retroactive deletion
    /// of authored content.
    Deleted = 2,
}

impl fmt::Display for UserStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Active => "active",
            Self::Banned => "banned",
            Self::Deleted => "deleted",
        })
    }
}

/// A registered account.
///
/// Constructed by the persistence layer when materialising rows.
/// Domain code generally receives `User` values from the host
/// rather than building them directly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct User {
    /// Stable internal identifier.
    pub id: UserId,
    /// Login name. Validated per `Username::new`.
    pub username: Username,
    /// Optional display name. When `None`, the username is shown.
    /// See [`User::display`] for the resolved value.
    pub display_name: Option<String>,
    /// Lifecycle state.
    pub status: UserStatus,
    /// Authority tier.
    pub permission_level: PermissionLevel,
    /// When the row was first created.
    pub created_at: Timestamp,
    /// Last successful login. `None` if never logged in (a fresh
    /// registration before its first login).
    pub last_login_at: Option<Timestamp>,
}

impl User {
    /// Maximum allowed display-name length. Mirrors the eventual
    /// schema. See [`InvalidDisplayName::TooLong`].
    pub const DISPLAY_NAME_MAX_LEN: usize = 64;

    /// Validate a candidate display name. Empty strings are
    /// disallowed at this layer — store `None` instead of
    /// `Some("")`.
    pub fn validate_display_name(s: &str) -> Result<(), InvalidDisplayName> {
        if s.is_empty() {
            return Err(InvalidDisplayName::Empty);
        }
        if s.len() > Self::DISPLAY_NAME_MAX_LEN {
            return Err(InvalidDisplayName::TooLong {
                actual: s.len(),
                max: Self::DISPLAY_NAME_MAX_LEN,
            });
        }
        if let Some(c) = s.chars().find(|c| c.is_control()) {
            return Err(InvalidDisplayName::ControlCharacter(c));
        }
        Ok(())
    }

    /// The name to show users. Falls back to the username when no
    /// display name is set.
    #[must_use]
    pub fn display(&self) -> &str {
        self.display_name
            .as_deref()
            .unwrap_or_else(|| self.username.as_str())
    }

    /// Convenience: is this user able to do anything? `false` for
    /// banned and deleted accounts.
    #[must_use]
    pub fn is_active(&self) -> bool {
        matches!(self.status, UserStatus::Active)
    }
}

/// Why a candidate display name failed validation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InvalidDisplayName {
    /// Empty string. Use `None` instead.
    #[error("display name must not be empty; use None for no display name")]
    Empty,

    /// Longer than [`User::DISPLAY_NAME_MAX_LEN`].
    #[error("display name is {actual} bytes; maximum is {max}")]
    TooLong {
        /// Actual byte length.
        actual: usize,
        /// The maximum allowed.
        max: usize,
    },

    /// Contains a control character (newline, tab, NUL, etc.).
    #[error("display name contains a control character: {0:?}")]
    ControlCharacter(char),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_user(display: Option<String>) -> User {
        User {
            id: UserId::new(1),
            username: Username::new("alice").unwrap(),
            display_name: display,
            status: UserStatus::Active,
            permission_level: PermissionLevel::User,
            created_at: Timestamp::now(),
            last_login_at: None,
        }
    }

    #[test]
    fn display_name_falls_back_to_username() {
        let u = sample_user(None);
        assert_eq!(u.display(), "alice");
    }

    #[test]
    fn display_name_uses_explicit_value() {
        let u = sample_user(Some("Alice the Great".to_owned()));
        assert_eq!(u.display(), "Alice the Great");
    }

    #[test]
    fn is_active_only_for_active_status() {
        let mut u = sample_user(None);
        assert!(u.is_active());
        u.status = UserStatus::Banned;
        assert!(!u.is_active());
        u.status = UserStatus::Deleted;
        assert!(!u.is_active());
    }

    #[test]
    fn display_name_validation_accepts_normal() {
        for name in ["A", "Alice", "Alice Q. Wonderland", &"x".repeat(64)] {
            User::validate_display_name(name)
                .unwrap_or_else(|_| panic!("expected {name:?} to validate"));
        }
    }

    #[test]
    fn display_name_validation_rejects_empty() {
        assert_eq!(
            User::validate_display_name(""),
            Err(InvalidDisplayName::Empty)
        );
    }

    #[test]
    fn display_name_validation_rejects_too_long() {
        let too_long = "x".repeat(65);
        match User::validate_display_name(&too_long) {
            Err(InvalidDisplayName::TooLong {
                actual: 65,
                max: 64,
            }) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn display_name_validation_rejects_control_chars() {
        match User::validate_display_name("Alice\nQ.") {
            Err(InvalidDisplayName::ControlCharacter('\n')) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn user_serde_round_trip() {
        let u = sample_user(Some("Alice".to_owned()));
        let json = serde_json::to_string(&u).unwrap();
        let back: User = serde_json::from_str(&json).unwrap();
        assert_eq!(u, back);
    }

    #[test]
    fn status_discriminants_are_stable() {
        assert_eq!(UserStatus::Active as u8, 0);
        assert_eq!(UserStatus::Banned as u8, 1);
        assert_eq!(UserStatus::Deleted as u8, 2);
    }
}
