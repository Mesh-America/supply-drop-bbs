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

/// Whether `c` is a Unicode codepoint that can spoof displayed text without
/// being caught by [`char::is_control`] — `validate_mesh_node_name` only
/// rejects `Cc` (Control) characters, not `Cf` (Format): the bidirectional
/// overrides/embeddings/isolates (e.g. U+202E RIGHT-TO-LEFT OVERRIDE), the
/// zero-width joiner/non-joiner, and the byte-order mark all fall under
/// General_Category=Format, not Control, and can make a node name render as
/// something other than its actual bytes wherever it's displayed.
///
/// This covers the Format category specifically (supply-drop-bbs / #228's
/// reported vector), not every way Unicode can be used to mislead a reader —
/// stacked combining marks ("zalgo" text, category Mn/Me) and confusable
/// homoglyphs are different techniques this does not address, and are not
/// filtered here since Mn is also how ordinary accented text in many
/// languages is represented; blocking it would break legitimate names.
fn is_display_spoofing_codepoint(c: char) -> bool {
    use unicode_properties::UnicodeGeneralCategory as _;
    c.general_category() == unicode_properties::GeneralCategory::Format
}

/// Strip Unicode display-spoofing codepoints from `s` — no length
/// limit applied. For contexts that display `bbs.name` but aren't
/// constrained by MeshCore's advert byte budget, e.g. the connect-time
/// welcome banner's `{name}` substitution. [`truncate_mesh_node_name`] is
/// preferred wherever the advert length budget also applies; this is that
/// function's stripping step, factored out for callers where it doesn't
/// (supply-drop-bbs / #228).
#[must_use]
pub fn strip_display_spoofing_codepoints(s: &str) -> String {
    s.chars()
        .filter(|c| !is_display_spoofing_codepoint(*c))
        .collect()
}

/// Truncate `s` to at most [`max_mesh_node_name_bytes`] bytes for the given
/// `sharing_location` **without splitting a UTF-8 character** — a multi-byte
/// emoji at the boundary is dropped whole rather than cut mid-codepoint —
/// and strip any Unicode display-spoofing codepoints (see
/// [`strip_display_spoofing_codepoints`]).
///
/// This is the defensive last line of defence on the advert output path: input
/// is validated up front, but a hand-edited config could still carry an
/// over-length or spoofing-capable name — or a valid name could be paired
/// with location-sharing turned on afterward — and both an over-length
/// advert and a spoofed display are failure modes this function exists to
/// close regardless of how the name reached it (supply-drop-bbs / #228).
///
/// Stripping runs before the byte-budget trim, so the budget is measured
/// against what will actually be sent, not bytes that are about to be
/// removed anyway.
#[must_use]
pub fn truncate_mesh_node_name(s: &str, sharing_location: bool) -> String {
    let cleaned = strip_display_spoofing_codepoints(s);

    let max = max_mesh_node_name_bytes(sharing_location);
    if cleaned.len() <= max {
        return cleaned;
    }
    let mut end = max;
    while end > 0 && !cleaned.is_char_boundary(end) {
        end -= 1;
    }
    cleaned[..end].to_owned()
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
        assert!(name.starts_with(&out));
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
        assert!(name.starts_with(&out));
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
        assert!(name.starts_with(&out));
    }

    #[test]
    fn strip_display_spoofing_codepoints_has_no_length_limit() {
        // Unlike truncate_mesh_node_name, this must never shorten a name
        // for byte-budget reasons — only spoofing codepoints come out.
        let long_clean = "a".repeat(100);
        assert_eq!(strip_display_spoofing_codepoints(&long_clean), long_clean);

        let name = format!("\u{202E}{long_clean}");
        assert_eq!(strip_display_spoofing_codepoints(&name), long_clean);
    }

    #[test]
    fn truncate_strips_rtl_override() {
        // supply-drop-bbs#228: "Admin" preceded by U+202E RIGHT-TO-LEFT
        // OVERRIDE renders reversed ("nimdA") wherever it's displayed, even
        // though the actual bytes are unchanged — a display-spoofing vector
        // char::is_control() (used by validate_mesh_node_name) never catches,
        // since U+202E is General_Category=Format, not Control.
        let name = "\u{202E}Admin BBS";
        let out = truncate_mesh_node_name(name, false);
        assert_eq!(out, "Admin BBS");
        assert!(!out.contains('\u{202E}'));
    }

    #[test]
    fn truncate_strips_zero_width_and_bom_characters() {
        let name = "Mesh\u{200D}\u{200C}\u{FEFF} BBS";
        let out = truncate_mesh_node_name(name, false);
        assert_eq!(out, "Mesh BBS");
    }

    #[test]
    fn truncate_leaves_legitimate_unicode_untouched() {
        // Must not over-filter: accented letters and emoji are not Format
        // codepoints and must survive unchanged.
        for name in ["Café Node", "🇺🇸 Mesh America BBS", "日本語ノード"] {
            assert_eq!(truncate_mesh_node_name(name, false), name);
        }
    }

    #[test]
    fn truncate_becomes_empty_for_an_all_spoofing_name() {
        let name = "\u{202E}\u{200D}\u{200C}";
        assert_eq!(truncate_mesh_node_name(name, false), "");
    }

    #[test]
    fn truncate_strips_and_length_trims_together() {
        // A name that only overflows the byte budget once you count the
        // (soon to be removed) spoofing characters must NOT be truncated
        // short of the real limit — stripping must happen before the
        // byte-budget trim, not after, so the budget reflects what's
        // actually sent.
        let clean = "a".repeat(31);
        let name = format!("\u{202E}{clean}\u{200D}");
        assert_eq!(name.len(), 31 + 3 + 3, "fixture sanity: 3-byte codepoints");
        let out = truncate_mesh_node_name(&name, false);
        assert_eq!(out, clean, "the full 31 legitimate bytes must survive");
    }

    #[test]
    fn validate_does_not_reject_format_codepoints() {
        // Deliberately documents the current, intentional split: rejection
        // only happens for Cc (Control) at the three input surfaces;
        // sanitization of Cf (Format) happens only in
        // truncate_mesh_node_name, the shared choke point right before
        // every advert transmission (supply-drop-bbs#228's chosen fix, to
        // avoid duplicating rejection logic across the CLI, web, and setup
        // wizard input surfaces, and so it also covers a hand-edited
        // config.toml that never went through any of them). If this
        // assertion starts failing, `is_display_spoofing_codepoint`
        // coverage and `validate_mesh_node_name` have diverged from that
        // design — reconcile deliberately, don't just update the test.
        assert!(validate_mesh_node_name("\u{202E}Admin BBS", false).is_ok());
    }
}
