//! Permission system.
//!
//! Three concepts:
//!
//! - **`PermissionLevel`** is a tier — every authenticated user
//!   sits at exactly one tier, and operations require a minimum
//!   tier to execute.
//! - **`PermissionCtx`** is the runtime authority a `Host` call
//!   carries. It identifies the calling session and its current
//!   tier, and gates every domain-mutating method.
//! - The host's enforcement is structural: methods that touch
//!   user-visible state take a `&PermissionCtx` argument, and
//!   the host returns an error if the context isn't authorised.
//!   Transport plugins cannot synthesise a `PermissionCtx` of
//!   their own choosing — only the host mints them.

use crate::identity::{SessionId, Username};
use serde::{Deserialize, Serialize};
use std::fmt;

/// Authority tier for a BBS account.
///
/// Tiers are totally ordered: `Sysop` ≥ `Aide` ≥ `User` ≥
/// `Unvalidated`. Most operations check `level >= required` rather
/// than `level == specific`, so a sysop implicitly has all aide and
/// user privileges.
///
/// The `repr(u8)` is committed to: serialised representations
/// (audit log, reports) write the integer value, and existing rows
/// must continue to deserialise across releases.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum PermissionLevel {
    /// Registered but not yet validated by a sysop. Can interact
    /// with the BBS in very limited ways: complete the validation
    /// flow, get help, log out. Cannot post, read most rooms, or
    /// send DMs.
    Unvalidated = 0,

    /// A normal validated user. Can post in rooms permitted by the
    /// room's own permission level, send/receive DMs, manage their
    /// own session.
    User = 10,

    /// An aide — a user the sysop has delegated some moderation
    /// authority to. Can validate new users, manage rooms within
    /// constraints, see the audit log.
    Aide = 50,

    /// The system operator. Can do anything the BBS supports,
    /// including operations that cannot be undone (delete users,
    /// purge messages, change permission levels).
    Sysop = 100,
}

impl PermissionLevel {
    /// True iff `self` is at least as high a tier as `required`.
    #[must_use]
    pub fn satisfies(self, required: PermissionLevel) -> bool {
        self as u8 >= required as u8
    }
}

impl fmt::Display for PermissionLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Unvalidated => "unvalidated",
            Self::User => "user",
            Self::Aide => "aide",
            Self::Sysop => "sysop",
        };
        f.write_str(s)
    }
}

/// Runtime authority context carried into every domain-touching
/// `Host` call.
///
/// A `PermissionCtx` is **minted by the host** when it processes
/// an authenticated request. Transport plugins receive a context
/// from the host (typically as part of command processing) and
/// pass it back into subsequent host calls. They cannot construct
/// one with arbitrary authority — only the host can.
///
/// The fields are deliberately public for the host's use; the
/// `__internal_new` constructor is the documented intent. Plugin
/// code should not construct these directly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionCtx {
    /// The session this authority derives from. Logged in audit
    /// records.
    pub session: SessionId,

    /// The username bound to the session, if any. `None` for
    /// pre-authentication contexts (e.g., during the registration
    /// or login workflows).
    pub username: Option<Username>,

    /// The tier this caller is acting at. For pre-auth contexts,
    /// always `Unvalidated`.
    pub level: PermissionLevel,
}

impl PermissionCtx {
    /// Construct a permission context. Intended for the host
    /// implementation in `bbs-core` and for test fixtures in
    /// [`crate::testing`].
    ///
    /// **Plugin authors: do not call this.** Use the
    /// `PermissionCtx` values you receive from the
    /// [`Host`](crate::Host).
    #[doc(hidden)]
    #[must_use]
    pub fn __internal_new(
        session: SessionId,
        username: Option<Username>,
        level: PermissionLevel,
    ) -> Self {
        Self {
            session,
            username,
            level,
        }
    }

    /// Convenience: does this context satisfy the required tier?
    #[must_use]
    pub fn satisfies(&self, required: PermissionLevel) -> bool {
        self.level.satisfies(required)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordering_is_total_and_strict() {
        let levels = [
            PermissionLevel::Unvalidated,
            PermissionLevel::User,
            PermissionLevel::Aide,
            PermissionLevel::Sysop,
        ];
        for (i, lo) in levels.iter().enumerate() {
            for hi in &levels[i + 1..] {
                assert!(hi > lo, "{hi:?} should be > {lo:?}");
            }
        }
    }

    #[test]
    fn satisfies_is_inclusive() {
        assert!(PermissionLevel::Sysop.satisfies(PermissionLevel::Aide));
        assert!(PermissionLevel::Sysop.satisfies(PermissionLevel::Sysop));
        assert!(!PermissionLevel::User.satisfies(PermissionLevel::Aide));
    }

    #[test]
    fn display_format() {
        assert_eq!(format!("{}", PermissionLevel::Sysop), "sysop");
        assert_eq!(format!("{}", PermissionLevel::Unvalidated), "unvalidated");
    }

    #[test]
    fn discriminants_are_stable() {
        // These values are part of the wire format. Changing them
        // breaks audit logs and any reports that store the integer.
        assert_eq!(PermissionLevel::Unvalidated as u8, 0);
        assert_eq!(PermissionLevel::User as u8, 10);
        assert_eq!(PermissionLevel::Aide as u8, 50);
        assert_eq!(PermissionLevel::Sysop as u8, 100);
    }
}
