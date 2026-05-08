//! `Room` — a topic where messages are posted.
//!
//! Rooms are linked-list-ordered (`prev_neighbor`, `next_neighbor`)
//! so the BBS's "walk to the next room" UX has a stable order
//! defined by the sysop. Insertion / deletion / reorder is the
//! sysop's responsibility; the type system doesn't enforce list
//! consistency (that's a runtime invariant verified by the
//! persistence layer at startup).
//!
//! See ARCHITECTURE.md §3 for the domain model overview.

use crate::ids::RoomId;
use crate::timestamp::Timestamp;
use bbs_plugin_api::PermissionLevel;
use serde::{Deserialize, Serialize};

/// A named topic in the BBS.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Room {
    /// Stable internal identifier.
    pub id: RoomId,
    /// Display name. Validated per [`Room::validate_name`].
    pub name: String,
    /// Optional one-line description shown when entering the
    /// room. `None` = no description.
    pub description: Option<String>,
    /// True if only sysops/aides can post here. Users can read.
    pub read_only: bool,
    /// Minimum permission level required to *read* the room.
    /// Posting may be additionally gated by `read_only` or by
    /// per-room ACLs in a future revision.
    pub min_permission_level: PermissionLevel,
    /// Previous room in the walk order, or `None` for the head.
    pub prev_neighbor: Option<RoomId>,
    /// Next room in the walk order, or `None` for the tail.
    pub next_neighbor: Option<RoomId>,
    /// When the room was created. Useful for "rooms created in
    /// the last week" reports.
    pub created_at: Timestamp,
}

impl Room {
    /// Maximum allowed name length.
    pub const NAME_MAX_LEN: usize = 32;

    /// Maximum allowed description length.
    pub const DESCRIPTION_MAX_LEN: usize = 256;

    /// Validate a candidate room name. Rules:
    ///
    /// - 1-32 characters
    /// - ASCII letters, digits, hyphens, underscores only
    /// - Must start with a letter or digit (no leading
    ///   punctuation, which would interact poorly with command
    ///   parsing)
    pub fn validate_name(s: &str) -> Result<(), InvalidRoomName> {
        if s.is_empty() {
            return Err(InvalidRoomName::Empty);
        }
        if s.len() > Self::NAME_MAX_LEN {
            return Err(InvalidRoomName::TooLong {
                actual: s.len(),
                max: Self::NAME_MAX_LEN,
            });
        }
        let first = s.chars().next().expect("non-empty checked above");
        if !first.is_ascii_alphanumeric() {
            return Err(InvalidRoomName::BadFirstCharacter(first));
        }
        for c in s.chars() {
            if !(c.is_ascii_alphanumeric() || c == '-' || c == '_') {
                return Err(InvalidRoomName::DisallowedCharacter(c));
            }
        }
        Ok(())
    }

    /// Validate a candidate description.
    pub fn validate_description(s: &str) -> Result<(), InvalidRoomDescription> {
        if s.is_empty() {
            return Err(InvalidRoomDescription::Empty);
        }
        if s.len() > Self::DESCRIPTION_MAX_LEN {
            return Err(InvalidRoomDescription::TooLong {
                actual: s.len(),
                max: Self::DESCRIPTION_MAX_LEN,
            });
        }
        if let Some(c) = s.chars().find(|c| matches!(*c, '\0')) {
            return Err(InvalidRoomDescription::NullByte(c));
        }
        Ok(())
    }

    /// Convenience: is this room the head of the walk order?
    #[must_use]
    pub fn is_head(&self) -> bool {
        self.prev_neighbor.is_none()
    }

    /// Convenience: is this room the tail of the walk order?
    #[must_use]
    pub fn is_tail(&self) -> bool {
        self.next_neighbor.is_none()
    }
}

/// Why a candidate room name failed validation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InvalidRoomName {
    /// Empty string.
    #[error("room name must not be empty")]
    Empty,

    /// Longer than [`Room::NAME_MAX_LEN`].
    #[error("room name is {actual} bytes; maximum is {max}")]
    TooLong {
        /// Actual byte length.
        actual: usize,
        /// The maximum allowed.
        max: usize,
    },

    /// First character must be ASCII alphanumeric.
    #[error("room name must start with a letter or digit; got {0:?}")]
    BadFirstCharacter(char),

    /// Contains a character outside the allowed set.
    #[error("room name contains a disallowed character: {0:?}")]
    DisallowedCharacter(char),
}

/// Why a candidate room description failed validation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InvalidRoomDescription {
    /// Empty string. Use `None` for "no description."
    #[error("description must not be empty; use None for no description")]
    Empty,

    /// Longer than [`Room::DESCRIPTION_MAX_LEN`].
    #[error("description is {actual} bytes; maximum is {max}")]
    TooLong {
        /// Actual byte length.
        actual: usize,
        /// The maximum allowed.
        max: usize,
    },

    /// Contains a NUL byte. Most other control characters are
    /// permitted (operators may use newlines for multi-line
    /// descriptions); NUL is forbidden because it confuses many
    /// downstream tools.
    #[error("description contains a NUL byte")]
    NullByte(char),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_room() -> Room {
        Room {
            id: RoomId::new(1),
            name: "Lobby".to_owned(),
            description: Some("The main hangout".to_owned()),
            read_only: false,
            min_permission_level: PermissionLevel::User,
            prev_neighbor: None,
            next_neighbor: Some(RoomId::new(2)),
            created_at: Timestamp::now(),
        }
    }

    #[test]
    fn name_validation_accepts_normal() {
        for name in ["Lobby", "tech-talk", "room_42", "X", &"a".repeat(32)] {
            Room::validate_name(name).unwrap_or_else(|e| {
                panic!("expected {name:?} to validate, got {e:?}");
            });
        }
    }

    #[test]
    fn name_validation_rejects_empty() {
        assert_eq!(Room::validate_name(""), Err(InvalidRoomName::Empty));
    }

    #[test]
    fn name_validation_rejects_too_long() {
        let too_long = "a".repeat(33);
        match Room::validate_name(&too_long) {
            Err(InvalidRoomName::TooLong {
                actual: 33,
                max: 32,
            }) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn name_validation_rejects_bad_leading_chars() {
        for bad in ["-foo", "_foo", " foo"] {
            match Room::validate_name(bad) {
                Err(InvalidRoomName::BadFirstCharacter(_)) => {}
                other => panic!("expected BadFirstCharacter for {bad:?}, got {other:?}"),
            }
        }
    }

    #[test]
    fn name_validation_rejects_spaces_and_punct() {
        match Room::validate_name("hello world") {
            Err(InvalidRoomName::DisallowedCharacter(' ')) => {}
            other => panic!("unexpected: {other:?}"),
        }
        match Room::validate_name("hello.world") {
            Err(InvalidRoomName::DisallowedCharacter('.')) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn description_validation_accepts_normal() {
        for d in [
            "A simple description",
            "Multi\nline is fine",
            &"x".repeat(256),
        ] {
            Room::validate_description(d).unwrap_or_else(|e| {
                panic!("expected {d:?} to validate, got {e:?}");
            });
        }
    }

    #[test]
    fn description_validation_rejects_empty() {
        assert_eq!(
            Room::validate_description(""),
            Err(InvalidRoomDescription::Empty)
        );
    }

    #[test]
    fn description_validation_rejects_null_byte() {
        match Room::validate_description("foo\0bar") {
            Err(InvalidRoomDescription::NullByte(_)) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn head_and_tail_helpers() {
        let r = sample_room();
        assert!(r.is_head());
        assert!(!r.is_tail());
    }

    #[test]
    fn room_serde_round_trip() {
        let r = sample_room();
        let json = serde_json::to_string(&r).unwrap();
        let back: Room = serde_json::from_str(&json).unwrap();
        assert_eq!(r, back);
    }
}
