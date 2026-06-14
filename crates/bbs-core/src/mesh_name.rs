//! Validation and truncation for the MeshCore advert node name.
//!
//! The MeshCore firmware caps an advertisement's `app_data` at
//! `MAX_ADVERT_DATA_SIZE` = **32 bytes** (firmware `MeshCore.h`). The advert
//! `app_data` is laid out as `flags(1) + [lat(4) lon(4) if location] + name`,
//! so with no shared location the node name may be at most **31 bytes**.
//!
//! This limit is not advisory: a name that pushes `app_data` past 32 bytes is
//! signed by the originator over its full length, but **every receiver clamps
//! `app_data` to 32 bytes before verifying the signature** (firmware
//! `Mesh.cpp` `onRecvPacket`). The clamped bytes no longer match the
//! signature, so the advert is rejected as forged and silently dropped — the
//! node never appears on the mesh, even though it transmits fine.
//!
//! `bbs.name` doubles as the MeshCore node name, so it is validated against
//! this limit wherever it is set (setup wizard, web UI), and truncated
//! defensively on the advert output path.

/// Maximum node-name length in **bytes** (UTF-8) that fits in a MeshCore
/// advert with no shared location: `MAX_ADVERT_DATA_SIZE (32) − flags (1)`.
///
/// If an advert ever carries a shared GPS location (`+8` bytes), the usable
/// budget drops to 23. The BBS does not currently share location in adverts.
pub const MAX_MESH_NODE_NAME_BYTES: usize = 31;

/// Why a candidate node name was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InvalidNodeName {
    /// Empty string.
    #[error("node name must not be empty")]
    Empty,

    /// Longer than [`MAX_MESH_NODE_NAME_BYTES`] bytes.
    #[error("node name is {actual} bytes; maximum is {max} (MeshCore advert limit)")]
    TooLong {
        /// Actual byte length (UTF-8).
        actual: usize,
        /// The maximum allowed.
        max: usize,
    },

    /// Contains a control character (newline, tab, NUL, etc.).
    #[error("node name contains a control character: {0:?}")]
    ControlCharacter(char),
}

/// Validate a candidate MeshCore node name (also used as the BBS display name).
///
/// Rejects empty names, names longer than [`MAX_MESH_NODE_NAME_BYTES`] **bytes**
/// (not chars — a flag emoji is 8 bytes), and names containing control
/// characters.
pub fn validate_mesh_node_name(s: &str) -> Result<(), InvalidNodeName> {
    if s.is_empty() {
        return Err(InvalidNodeName::Empty);
    }
    if s.len() > MAX_MESH_NODE_NAME_BYTES {
        return Err(InvalidNodeName::TooLong {
            actual: s.len(),
            max: MAX_MESH_NODE_NAME_BYTES,
        });
    }
    if let Some(c) = s.chars().find(|c| c.is_control()) {
        return Err(InvalidNodeName::ControlCharacter(c));
    }
    Ok(())
}

/// Truncate `s` to at most [`MAX_MESH_NODE_NAME_BYTES`] bytes **without
/// splitting a UTF-8 character** — a multi-byte emoji at the boundary is
/// dropped whole rather than cut mid-codepoint.
///
/// This is the defensive last line of defence on the advert output path: input
/// is validated up front, but a hand-edited config could still carry an
/// over-length name, and an over-length advert is silently un-deliverable.
#[must_use]
pub fn truncate_mesh_node_name(s: &str) -> &str {
    if s.len() <= MAX_MESH_NODE_NAME_BYTES {
        return s;
    }
    let mut end = MAX_MESH_NODE_NAME_BYTES;
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
                validate_mesh_node_name(name).is_ok(),
                "expected {name:?} ({} bytes) to validate",
                name.len()
            );
        }
    }

    #[test]
    fn rejects_empty() {
        assert_eq!(validate_mesh_node_name(""), Err(InvalidNodeName::Empty));
    }

    #[test]
    fn rejects_too_long() {
        // Two flag emoji (8 bytes each) + text = 34 bytes > 31.
        let name = "🇺🇸 Mesh America BBS 🇺🇸";
        assert_eq!(name.len(), 34);
        assert_eq!(
            validate_mesh_node_name(name),
            Err(InvalidNodeName::TooLong {
                actual: 34,
                max: 31
            })
        );
    }

    #[test]
    fn rejects_control_character() {
        assert_eq!(
            validate_mesh_node_name("Mesh\nBBS"),
            Err(InvalidNodeName::ControlCharacter('\n'))
        );
    }

    #[test]
    fn boundary_exactly_31_bytes_ok() {
        let name = "a".repeat(31);
        assert!(validate_mesh_node_name(&name).is_ok());
        let too_long = "a".repeat(32);
        assert!(matches!(
            validate_mesh_node_name(&too_long),
            Err(InvalidNodeName::TooLong {
                actual: 32,
                max: 31
            })
        ));
    }

    #[test]
    fn truncate_leaves_short_names_unchanged() {
        assert_eq!(
            truncate_mesh_node_name("Mesh America BBS"),
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
        let out = truncate_mesh_node_name(name);
        assert!(
            out.len() <= MAX_MESH_NODE_NAME_BYTES,
            "got {} bytes",
            out.len()
        );
        assert!(name.starts_with(out));
        assert_ne!(out, name, "an over-length name must actually be truncated");
    }
}
