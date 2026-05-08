//! Newtype wrappers around the integer IDs every domain object
//! gets. The wrapping prevents the classic class of bug where a
//! `RoomId` is passed where a `UserId` was expected — the type
//! system rejects it at compile time.
//!
//! All three wrap `i64` because that's SQLite's `INTEGER PRIMARY
//! KEY` natural type. We never expect to hit 2⁶³ users; the size
//! is just "what SQLite hands us." Negative values are allowed
//! (SQLite supports them) but conventionally we use positives.

use serde::{Deserialize, Serialize};
use std::fmt;

macro_rules! id_newtype {
    ($name:ident, $prefix:literal, $doc:literal) => {
        #[doc = $doc]
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(i64);

        impl $name {
            /// Construct from the underlying integer. Used by the
            /// persistence layer when materialising rows from the
            /// DB and by tests; domain code should normally
            /// receive these from queries rather than fabricating.
            #[must_use]
            pub const fn new(raw: i64) -> Self {
                Self(raw)
            }

            /// The inner integer. For DB foreign keys, logging,
            /// and FFI boundaries.
            #[must_use]
            pub const fn as_i64(self) -> i64 {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}{}", $prefix, self.0)
            }
        }

        impl From<i64> for $name {
            fn from(value: i64) -> Self {
                Self::new(value)
            }
        }

        impl From<$name> for i64 {
            fn from(value: $name) -> Self {
                value.as_i64()
            }
        }
    };
}

id_newtype!(UserId, "user:", "Stable identifier for a `User` row.");
id_newtype!(RoomId, "room:", "Stable identifier for a `Room` row.");
id_newtype!(MessageId, "msg:", "Stable identifier for a `Message` row.");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_format_matches_prefix() {
        assert_eq!(format!("{}", UserId::new(1)), "user:1");
        assert_eq!(format!("{}", RoomId::new(42)), "room:42");
        assert_eq!(format!("{}", MessageId::new(9999)), "msg:9999");
    }

    #[test]
    fn serde_transparent() {
        let u = UserId::new(7);
        assert_eq!(serde_json::to_string(&u).unwrap(), "7");
        let back: UserId = serde_json::from_str("7").unwrap();
        assert_eq!(u, back);
    }

    #[test]
    fn types_dont_mix() {
        // The point of newtypes: a function expecting UserId
        // refuses RoomId. We check via `mem::transmute` would be
        // cheating; the compile-time check is what we want.
        // This test mostly documents the intent.
        let _u: UserId = UserId::new(1);
        let _r: RoomId = RoomId::new(1);
        // assert_eq!(_u, _r);  // ← this would not compile, as
        // expected.
    }

    #[test]
    fn negative_ids_allowed() {
        // SQLite permits negative integer primary keys; we don't
        // forbid them at the type level because doing so would
        // require runtime validation on every construction.
        let u = UserId::new(-1);
        assert_eq!(u.as_i64(), -1);
    }

    #[test]
    fn from_into_round_trip() {
        let u: UserId = 42i64.into();
        let back: i64 = u.into();
        assert_eq!(back, 42);
    }
}
