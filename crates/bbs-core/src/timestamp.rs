//! UTC timestamps for the BBS.
//!
//! `Timestamp` is a newtype around [`time::OffsetDateTime`] that:
//!
//! 1. **Always normalises to UTC.** Construct from any offset; the
//!    stored value is in UTC. This eliminates the entire class of
//!    "is this naive or zoned, and which zone" bugs that plague
//!    Python projects. For display in the operator's local zone,
//!    convert at the rendering boundary, not at the storage one.
//! 2. **Always serialises as RFC 3339 with 'Z' suffix.** Audit
//!    logs, reports, JSON metrics — every `Timestamp` looks the
//!    same on disk. Human-readable, sortable as strings,
//!    universally parseable.
//! 3. **Round-trips.** Serialise + deserialise = the original
//!    instant, byte-for-byte (because we always emit UTC).
//!
//! The newtype wraps `time::OffsetDateTime` from the `time` crate.
//! If we ever need to swap underlying libraries, this type is the
//! choke point — domain code never sees `OffsetDateTime` directly.

use serde::{de::Error as _, Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use time::format_description::well_known::Rfc3339;
use time::{OffsetDateTime, UtcOffset};

/// A point in time, stored as UTC.
///
/// Construct via [`Timestamp::now`] or [`Timestamp::from_utc`].
/// Convert to local time only at the rendering boundary
/// (operator-facing display).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timestamp(OffsetDateTime);

impl Timestamp {
    /// The current instant, in UTC.
    #[must_use]
    pub fn now() -> Self {
        Self(OffsetDateTime::now_utc())
    }

    /// From a `time::OffsetDateTime`, normalising to UTC.
    /// The original offset is dropped: a 14:00+01:00 input and a
    /// 13:00+00:00 input produce the same `Timestamp`.
    #[must_use]
    pub fn from_utc(dt: OffsetDateTime) -> Self {
        Self(dt.to_offset(UtcOffset::UTC))
    }

    /// Borrow the underlying `OffsetDateTime`. Always in UTC.
    #[must_use]
    pub fn as_offset_datetime(self) -> OffsetDateTime {
        self.0
    }

    /// Render as RFC 3339 with the `Z` suffix. Identical to the
    /// serde representation; the explicit method exists for code
    /// that needs a string outside a serialiser.
    #[must_use]
    pub fn to_rfc3339(&self) -> String {
        self.0
            .format(&Rfc3339)
            .expect("UTC OffsetDateTime always formats as RFC3339")
    }

    /// Parse an RFC 3339 string. Accepts any offset; normalises
    /// to UTC.
    pub fn parse_rfc3339(s: &str) -> Result<Self, ParseTimestampError> {
        let dt = OffsetDateTime::parse(s, &Rfc3339).map_err(ParseTimestampError::Format)?;
        Ok(Self::from_utc(dt))
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_rfc3339())
    }
}

impl Serialize for Timestamp {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_rfc3339())
    }
}

impl<'de> Deserialize<'de> for Timestamp {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        // Use owned String so this works against deserializers
        // that don't lend out borrowed string slices (e.g.,
        // anything reading from an owned source). The allocation
        // is unavoidable in the general case.
        let s: String = String::deserialize(d)?;
        Self::parse_rfc3339(&s).map_err(|e| D::Error::custom(e.to_string()))
    }
}

/// What can go wrong parsing a `Timestamp` string.
#[derive(Debug, thiserror::Error)]
pub enum ParseTimestampError {
    /// The string didn't match RFC 3339 syntax.
    #[error("invalid RFC 3339 timestamp: {0}")]
    Format(#[from] time::error::Parse),
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    #[test]
    fn now_is_utc() {
        let t = Timestamp::now();
        assert_eq!(t.as_offset_datetime().offset(), UtcOffset::UTC);
    }

    #[test]
    fn from_utc_normalises_to_utc() {
        // 14:00 in +01:00 is 13:00 in UTC.
        let plus_one = datetime!(2026-05-08 14:00 +01:00);
        let t = Timestamp::from_utc(plus_one);
        let utc = datetime!(2026-05-08 13:00 +00:00);
        assert_eq!(t.as_offset_datetime(), utc);
    }

    #[test]
    fn rfc3339_round_trip() {
        let original = Timestamp::from_utc(datetime!(2026-05-08 13:14:15 UTC));
        let s = original.to_rfc3339();
        assert!(s.ends_with('Z') || s.ends_with("+00:00"));
        let back = Timestamp::parse_rfc3339(&s).unwrap();
        assert_eq!(original, back);
    }

    #[test]
    fn parse_accepts_non_utc_offset_and_normalises() {
        let t = Timestamp::parse_rfc3339("2026-05-08T14:00:00+01:00").unwrap();
        // After normalisation, it should be 13:00 UTC.
        let expected = Timestamp::from_utc(datetime!(2026-05-08 13:00 UTC));
        assert_eq!(t, expected);
    }

    #[test]
    fn serde_round_trip() {
        let original = Timestamp::from_utc(datetime!(2026-05-08 13:14:15 UTC));
        let json = serde_json::to_string(&original).unwrap();
        // Quoted RFC3339 string.
        assert!(json.starts_with('"') && json.ends_with('"'));
        let back: Timestamp = serde_json::from_str(&json).unwrap();
        assert_eq!(original, back);
    }

    #[test]
    fn parse_error_on_garbage() {
        assert!(Timestamp::parse_rfc3339("not a date").is_err());
        assert!(Timestamp::parse_rfc3339("").is_err());
    }

    #[test]
    fn ordering_matches_chronology() {
        let earlier = Timestamp::from_utc(datetime!(2026-01-01 00:00 UTC));
        let later = Timestamp::from_utc(datetime!(2026-12-31 23:59 UTC));
        assert!(earlier < later);
    }
}
