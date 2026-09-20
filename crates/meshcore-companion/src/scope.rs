//! MeshCore flood scope ("region"): the name and 16-byte key a radio stamps on
//! the floods it originates, so only repeaters that know the region pass them
//! on.
//!
//! The firmware derives a region's key from its name (`getAutoKeyFor` in
//! `TransportKeyStore`): the first 16 bytes of the SHA-256 of `#` followed by
//! the name. The radio stores the name without the `#`.

use sha2::{Digest, Sha256};

/// The most bytes a region name can have: the radio keeps it in a 31-byte field
/// that ends in a NUL.
pub const MAX_REGION_NAME_BYTES: usize = 30;

/// A flood scope as the radio holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FloodScope {
    /// The region name, without the leading `#`.
    pub name: String,
    /// The 16-byte transport key for the region.
    pub key: [u8; 16],
}

/// Why a region name was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RegionNameError {
    /// Nothing left after trimming and dropping the `#`.
    #[error("the region name is empty")]
    Empty,
    /// More than [`MAX_REGION_NAME_BYTES`] bytes.
    #[error("the region name is {0} bytes; the radio holds at most {MAX_REGION_NAME_BYTES}")]
    TooLong(usize),
    /// A control character (a NUL would end the name early on the radio).
    #[error("the region name contains a control character")]
    ControlChar,
}

/// The region name as the radio stores it: whitespace trimmed, one leading `#`
/// dropped. Case is kept, because the name is hashed as given and a different
/// case is a different key.
///
/// # Errors
/// [`RegionNameError`] if nothing is left, the name is too long for the radio,
/// or it holds a control character.
pub fn normalize_region_name(input: &str) -> Result<String, RegionNameError> {
    let trimmed = input.trim();
    let name = trimmed.strip_prefix('#').unwrap_or(trimmed).trim();
    if name.is_empty() {
        return Err(RegionNameError::Empty);
    }
    if name.chars().any(char::is_control) {
        return Err(RegionNameError::ControlChar);
    }
    if name.len() > MAX_REGION_NAME_BYTES {
        return Err(RegionNameError::TooLong(name.len()));
    }
    Ok(name.to_owned())
}

/// The transport key for a region `name` (already normalised).
#[must_use]
pub fn region_key(name: &str) -> [u8; 16] {
    let mut hasher = Sha256::new();
    hasher.update(b"#");
    hasher.update(name.as_bytes());
    let digest = hasher.finalize();
    let mut key = [0u8; 16];
    key.copy_from_slice(&digest[..16]);
    key
}

impl FloodScope {
    /// The scope for the region called `input` (see [`normalize_region_name`]).
    ///
    /// # Errors
    /// [`RegionNameError`] if the name is not one the radio can hold.
    pub fn for_region(input: &str) -> Result<Self, RegionNameError> {
        let name = normalize_region_name(input)?;
        let key = region_key(&name);
        Ok(Self { name, key })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    // Known answer: `printf '#usa' | sha256sum` is
    // 418f6396cea59e2cc79f06d1566eec31d38d6fe1baa7f4c3b404d2150f32b9fe, computed
    // outside this code; the key is its first 16 bytes.
    #[test]
    fn the_key_is_the_first_16_bytes_of_sha256_of_hash_and_name() {
        assert_eq!(hex(&region_key("usa")), "418f6396cea59e2cc79f06d1566eec31");
        assert_eq!(
            region_key("usa"),
            FloodScope::for_region("#usa").unwrap().key
        );
    }

    #[test]
    fn a_leading_hash_and_surrounding_space_are_not_part_of_the_name() {
        for input in ["usa", "#usa", "  #usa  ", "# usa"] {
            assert_eq!(normalize_region_name(input).unwrap(), "usa", "{input:?}");
        }
        // Only one `#` is dropped, and case is kept.
        assert_eq!(normalize_region_name("##usa").unwrap(), "#usa");
        assert_ne!(region_key("USA"), region_key("usa"));
    }

    #[test]
    fn names_the_radio_cannot_hold_are_refused() {
        assert_eq!(normalize_region_name(""), Err(RegionNameError::Empty));
        assert_eq!(normalize_region_name(" # "), Err(RegionNameError::Empty));
        assert_eq!(
            normalize_region_name("a\0b"),
            Err(RegionNameError::ControlChar)
        );
        assert_eq!(
            normalize_region_name("a\nb"),
            Err(RegionNameError::ControlChar)
        );
        let long = "x".repeat(MAX_REGION_NAME_BYTES + 1);
        assert_eq!(
            normalize_region_name(&long),
            Err(RegionNameError::TooLong(MAX_REGION_NAME_BYTES + 1))
        );
        assert!(normalize_region_name(&"x".repeat(MAX_REGION_NAME_BYTES)).is_ok());
        // Bytes, not characters: 16 two-byte letters is 32 bytes.
        assert!(matches!(
            normalize_region_name(&"é".repeat(16)),
            Err(RegionNameError::TooLong(32))
        ));
    }
}
