//! Validation and truncation for the MeshCore advert node name.
//!
//! The MeshCore firmware caps an advertisement's `app_data` at
//! `MAX_ADVERT_DATA_SIZE` = **32 bytes** (firmware `MeshCore.h`). The advert
//! `app_data` is laid out as `flags(1) + [lat(4) lon(4) if location] + name`,
//! so the node name may be at most **31 bytes** with no shared location, or
//! **23 bytes** (`31 − 8`) when the advert also carries a shared GPS position.
//!
//! This limit is not advisory: **every receiver clamps `app_data` to 32
//! bytes before verifying the signature** (firmware `Mesh.cpp`
//! `onRecvPacket`) — confirmed against upstream firmware source. Whether an
//! over-budget name actually reaches that clamp depends on the *sender*:
//! reference MeshCore firmware's own advert builder
//! (`AdvertDataBuilder::encodeTo`) truncates the name locally before signing,
//! so a compliant sender never produces an oversized packet in the first
//! place — the name is silently *shortened*, not rejected. supply-drop-bbs
//! is itself always the sender here, though, and not every companion bridge
//! it talks to replicates that self-truncation: the `openhop_core`/
//! `pymc-companion` bridge used for Pi HAT setups does not, so it happily
//! signs an over-budget `app_data` in full — and *every* receiver then
//! clamps those signed bytes before verifying, so the signature no longer
//! matches and the advert is rejected as forged and silently dropped, with
//! no error anywhere in the sending stack. This is exactly what happened in
//! supply-drop-bbs#225: a name that fit without location silently stopped
//! advertising the moment location-sharing was turned on. Validating here,
//! before a name (or the location-sharing state it interacts with) is ever
//! set, means supply-drop-bbs never depends on a downstream bridge or
//! firmware to save it from that failure mode.
//!
//! `bbs.name` doubles as the MeshCore node name, so it is validated against
//! the appropriate limit wherever it — or the location-sharing setting it
//! interacts with — is set (setup wizard, CLI, web UI), and truncated
//! defensively on the advert output path.

/// Maximum node-name length in **bytes** (UTF-8) that fits in a MeshCore
/// advert with no shared location: `MAX_ADVERT_DATA_SIZE (32) − flags (1)`.
pub const MAX_MESH_NODE_NAME_BYTES: usize = 31;

/// Bytes an advert's packed lat/lon costs (`lat: i32` + `lon: i32`).
const LOCATION_ADVERT_BYTES: usize = 8;

/// Maximum node-name length in bytes when the advert also shares a GPS
/// location: [`MAX_MESH_NODE_NAME_BYTES`] minus the 8 bytes lat/lon costs.
pub const MAX_MESH_NODE_NAME_BYTES_WITH_LOCATION: usize =
    MAX_MESH_NODE_NAME_BYTES - LOCATION_ADVERT_BYTES;

/// The name-length budget for a node that does (`true`) or doesn't (`false`)
/// share its GPS location in mesh self-adverts.
#[must_use]
pub const fn max_mesh_node_name_bytes(sharing_location: bool) -> usize {
    if sharing_location {
        MAX_MESH_NODE_NAME_BYTES_WITH_LOCATION
    } else {
        MAX_MESH_NODE_NAME_BYTES
    }
}

/// Why a candidate node name was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InvalidNodeName {
    /// Empty string.
    #[error("node name must not be empty")]
    Empty,

    /// Longer than the applicable limit — see [`max_mesh_node_name_bytes`].
    #[error(
        "node name is {actual} bytes; maximum is {max} ({}MeshCore advert limit)",
        if *sharing_location { "with GPS location shared, " } else { "" }
    )]
    TooLong {
        /// Actual byte length (UTF-8).
        actual: usize,
        /// The maximum allowed for `sharing_location`.
        max: usize,
        /// Whether the limit was tightened for a shared GPS location.
        sharing_location: bool,
    },

    /// Contains a control character (newline, tab, NUL, etc.).
    #[error("node name contains a control character: {0:?}")]
    ControlCharacter(char),
}

/// Validate a candidate MeshCore node name (also used as the BBS display name).
///
/// Rejects empty names, names longer than [`max_mesh_node_name_bytes`]
/// **bytes** for the given `sharing_location` (not chars — a flag emoji is 8
/// bytes), and names containing control characters. Pass `sharing_location =
/// true` whenever the node currently has (or will have) a GPS location
/// configured *and* `[location].share_in_advert` enabled — that combination
/// is what actually ships lat/lon in the advert and tightens the budget.
pub fn validate_mesh_node_name(s: &str, sharing_location: bool) -> Result<(), InvalidNodeName> {
    if s.is_empty() {
        return Err(InvalidNodeName::Empty);
    }
    let max = max_mesh_node_name_bytes(sharing_location);
    if s.len() > max {
        return Err(InvalidNodeName::TooLong {
            actual: s.len(),
            max,
            sharing_location,
        });
    }
    if let Some(c) = s.chars().find(|c| c.is_control()) {
        return Err(InvalidNodeName::ControlCharacter(c));
    }
    Ok(())
}

/// Truncate `s` to at most [`max_mesh_node_name_bytes`] bytes for the given
/// `sharing_location` **without splitting a UTF-8 character** — a multi-byte
/// emoji at the boundary is dropped whole rather than cut mid-codepoint.
///
/// This is the defensive last line of defence on the advert output path: input
/// is validated up front, but a hand-edited config could still carry an
/// over-length name — or a valid name could be paired with location-sharing
/// turned on afterward — and an over-length advert is silently un-deliverable.
#[must_use]
pub fn truncate_mesh_node_name(s: &str, sharing_location: bool) -> &str {
    let max = max_mesh_node_name_bytes(sharing_location);
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_normal_names() {
        for name in ["Supply Drop BBS", "Mesh America BBS", "🇺🇸 Mesh America BBS"] {
            assert!(
                validate_mesh_node_name(name, false).is_ok(),
                "expected {name:?} ({} bytes) to validate",
                name.len()
            );
        }
    }

    #[test]
    fn rejects_empty() {
        assert_eq!(
            validate_mesh_node_name("", false),
            Err(InvalidNodeName::Empty)
        );
    }

    #[test]
    fn rejects_too_long() {
        // Two flag emoji (8 bytes each) + text = 34 bytes > 31.
        let name = "🇺🇸 Mesh America BBS 🇺🇸";
        assert_eq!(name.len(), 34);
        assert_eq!(
            validate_mesh_node_name(name, false),
            Err(InvalidNodeName::TooLong {
                actual: 34,
                max: 31,
                sharing_location: false,
            })
        );
    }

    #[test]
    fn rejects_control_character() {
        assert_eq!(
            validate_mesh_node_name("Mesh\nBBS", false),
            Err(InvalidNodeName::ControlCharacter('\n'))
        );
    }

    #[test]
    fn boundary_exactly_31_bytes_ok() {
        let name = "a".repeat(31);
        assert!(validate_mesh_node_name(&name, false).is_ok());
        let too_long = "a".repeat(32);
        assert!(matches!(
            validate_mesh_node_name(&too_long, false),
            Err(InvalidNodeName::TooLong {
                actual: 32,
                max: 31,
                sharing_location: false,
            })
        ));
    }

    #[test]
    fn sharing_location_tightens_budget_to_23_bytes() {
        // This is the exact case that broke supply-drop-bbs#225: a 25-byte
        // name (an 8-byte flag emoji + 17 bytes of text) fits comfortably
        // under the 31-byte no-location limit, but pushes app_data to 34
        // bytes — past the firmware's 32-byte MAX_ADVERT_DATA_SIZE — the
        // moment location-sharing is turned on, so every receiver silently
        // drops the advert as forged.
        let name = "🇺🇸 Mesh America BBS"; // 25 bytes
        assert_eq!(name.len(), 25);
        assert!(validate_mesh_node_name(name, false).is_ok());
        assert_eq!(
            validate_mesh_node_name(name, true),
            Err(InvalidNodeName::TooLong {
                actual: 25,
                max: 23,
                sharing_location: true,
            })
        );

        let short_name = "a".repeat(23);
        assert!(validate_mesh_node_name(&short_name, true).is_ok());
        let one_too_long = "a".repeat(24);
        assert!(matches!(
            validate_mesh_node_name(&one_too_long, true),
            Err(InvalidNodeName::TooLong {
                actual: 24,
                max: 23,
                sharing_location: true,
            })
        ));
    }

    #[test]
    fn truncate_leaves_short_names_unchanged() {
        assert_eq!(
            truncate_mesh_node_name("Mesh America BBS", false),
            "Mesh America BBS"
        );
    }

    #[test]
    fn truncate_never_splits_a_codepoint() {
        // 34-byte name; truncating to 31 bytes lands inside the trailing flag
        // emoji's second code point, so we back off to the nearest char
        // boundary. The result is always ≤ limit, a valid prefix, and — being
        // sliced on a char boundary — always valid UTF-8 (slicing a str off a
        // boundary cannot panic or corrupt). Display may be slightly off (a
        // lone regional indicator) but the advert is deliverable, which is the
        // whole point of the defensive trim.
        let name = "🇺🇸 Mesh America BBS 🇺🇸";
        let out = truncate_mesh_node_name(name, false);
        assert!(
            out.len() <= MAX_MESH_NODE_NAME_BYTES,
            "got {} bytes",
            out.len()
        );
        assert!(name.starts_with(out));
        assert_ne!(out, name, "an over-length name must actually be truncated");
    }

    #[test]
    fn truncate_with_location_uses_the_tighter_budget() {
        // Same 25-byte name that validates fine without location — truncating
        // it *with* location sharing must shrink it further, to ≤ 23 bytes.
        // Byte 23 of this particular string already sits on a char boundary
        // (the flag emoji occupies bytes 0-7, the rest is plain ASCII), so
        // this alone would pass even with a naive, non-boundary-safe `&s[..n]`
        // slice — see `truncate_with_location_backs_off_a_mid_codepoint_cutoff`
        // below for a fixture that actually forces the boundary backoff loop.
        let name = "🇺🇸 Mesh America BBS";
        assert_eq!(name.len(), 25);
        let out = truncate_mesh_node_name(name, true);
        assert!(
            out.len() <= MAX_MESH_NODE_NAME_BYTES_WITH_LOCATION,
            "got {} bytes",
            out.len()
        );
        assert!(name.starts_with(out));
    }

    #[test]
    fn truncate_with_location_backs_off_a_mid_codepoint_cutoff() {
        // Built so the 23-byte with-location cutoff lands INSIDE a trailing
        // 4-byte codepoint, forcing is_char_boundary's backoff loop to
        // actually execute: 🇺🇸 (8 bytes, boundary at 8) + 14 ASCII bytes
        // (boundary at 22) + 🌎 (4 bytes: 22,23,24,25 — byte 23 is the
        // *second* byte of that sequence, not a boundary).
        let name = format!("🇺🇸{}🌎", "A".repeat(14));
        assert_eq!(name.len(), 8 + 14 + 4, "fixture must be 26 bytes");
        assert!(
            !name.is_char_boundary(MAX_MESH_NODE_NAME_BYTES_WITH_LOCATION),
            "fixture must land the 23-byte cutoff mid-codepoint, or this test \
             doesn't prove anything a naive &s[..23] slice wouldn't also pass"
        );

        let out = truncate_mesh_node_name(&name, true);

        assert!(
            out.len() < MAX_MESH_NODE_NAME_BYTES_WITH_LOCATION,
            "must back off strictly below the 23-byte cutoff since 23 itself \
             is mid-codepoint here — got {} bytes",
            out.len()
        );
        assert_eq!(
            out, "🇺🇸AAAAAAAAAAAAAA",
            "must back off to the last full codepoint (22 bytes: the flag \
             plus all 14 'A's), dropping the trailing 🌎 whole rather than \
             emitting a truncated/invalid byte sequence for it"
        );
        assert!(name.starts_with(out));
    }
}
