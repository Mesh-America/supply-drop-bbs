//! `Message` — a post.
//!
//! A message has a sender (always) and an optional recipient.
//! When `recipient` is `None`, the message is a public room post;
//! the room association lives in the `room_messages` join table
//! that the persistence layer manages — `Message` itself doesn't
//! carry a room id.
//!
//! When `recipient` is `Some(username)`, the message is a direct
//! message. DMs are not in any room.
//!
//! Messages are append-only in normal operation. A sysop can
//! delete one (with audit trail); the FK from `user_room_state`
//! has `ON DELETE SET NULL` so a delete doesn't cascade-destroy
//! anyone's "last seen" pointer.

use crate::ids::MessageId;
use crate::timestamp::Timestamp;
use bbs_plugin_api::Username;
use serde::{Deserialize, Serialize};

/// A posted message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    /// Stable internal identifier.
    pub id: MessageId,
    /// Author. Always set; even system-generated messages have a
    /// reserved sender name (e.g., the BBS's own username).
    pub sender: Username,
    /// `Some(user)` for a DM; `None` for a public room post.
    pub recipient: Option<Username>,
    /// The message body. Validated per [`Message::validate_content`].
    pub content: String,
    /// When it was posted.
    pub timestamp: Timestamp,
}

impl Message {
    /// Maximum allowed message body length, in bytes.
    ///
    /// 4096 is a deliberate cap. BBS messages are short-form by
    /// nature; mesh radio carries them in chunks the transport
    /// layer manages. A 4 KB ceiling prevents pathological abuse
    /// (an attacker posting a megabyte of garbage) without
    /// constraining ordinary use.
    pub const CONTENT_MAX_LEN: usize = 4096;

    /// Validate a candidate message body.
    pub fn validate_content(s: &str) -> Result<(), InvalidMessageContent> {
        if s.is_empty() {
            return Err(InvalidMessageContent::Empty);
        }
        if s.len() > Self::CONTENT_MAX_LEN {
            return Err(InvalidMessageContent::TooLong {
                actual: s.len(),
                max: Self::CONTENT_MAX_LEN,
            });
        }
        if s.contains('\0') {
            return Err(InvalidMessageContent::NullByte);
        }
        Ok(())
    }

    /// Convenience: is this a direct message (vs a public post)?
    #[must_use]
    pub fn is_direct(&self) -> bool {
        self.recipient.is_some()
    }
}

/// Why a candidate message body failed validation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InvalidMessageContent {
    /// Empty body.
    #[error("message content must not be empty")]
    Empty,

    /// Body exceeds [`Message::CONTENT_MAX_LEN`].
    #[error("message content is {actual} bytes; maximum is {max}")]
    TooLong {
        /// Actual byte length.
        actual: usize,
        /// The maximum allowed.
        max: usize,
    },

    /// Body contains a NUL byte.
    #[error("message content contains a NUL byte")]
    NullByte,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_message() -> Message {
        Message {
            id: MessageId::new(1),
            sender: Username::new("alice").unwrap(),
            recipient: None,
            content: "hello, mesh".to_owned(),
            timestamp: Timestamp::now(),
        }
    }

    #[test]
    fn content_validation_accepts_normal() {
        for c in ["hi", "Multi\nline\nworks", &"x".repeat(4096)] {
            Message::validate_content(c).unwrap_or_else(|e| {
                panic!("expected {} byte content to validate, got {e:?}", c.len());
            });
        }
    }

    #[test]
    fn content_validation_rejects_empty() {
        assert_eq!(
            Message::validate_content(""),
            Err(InvalidMessageContent::Empty)
        );
    }

    #[test]
    fn content_validation_rejects_too_long() {
        let too_long = "x".repeat(4097);
        match Message::validate_content(&too_long) {
            Err(InvalidMessageContent::TooLong {
                actual: 4097,
                max: 4096,
            }) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn content_validation_rejects_null_byte() {
        assert_eq!(
            Message::validate_content("foo\0bar"),
            Err(InvalidMessageContent::NullByte)
        );
    }

    #[test]
    fn is_direct_only_when_recipient_set() {
        let mut m = sample_message();
        assert!(!m.is_direct());
        m.recipient = Some(Username::new("bob").unwrap());
        assert!(m.is_direct());
    }

    #[test]
    fn message_serde_round_trip() {
        let m = sample_message();
        let json = serde_json::to_string(&m).unwrap();
        let back: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(m, back);
    }
}
