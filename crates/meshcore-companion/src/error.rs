/// Errors produced while decoding an inbound frame payload.
#[derive(thiserror::Error, Debug, PartialEq, Eq)]
pub enum FrameDecodeError {
    #[error("wrong frame prefix: expected 0x{expected:02X}, got 0x{got:02X}")]
    WrongPrefix { expected: u8, got: u8 },

    #[error("frame payload length {0} exceeds MAX_PAYLOAD_SIZE (169)")]
    PayloadTooLarge(usize),

    #[error("frame type 0x{type_byte:02X} body too short: need ≥{needed} bytes, got {got}")]
    BodyTooShort {
        type_byte: u8,
        needed: usize,
        got: usize,
    },

    #[error("invalid UTF-8 in frame field")]
    InvalidUtf8,
}
