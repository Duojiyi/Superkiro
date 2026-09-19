//! Error types for AWS Event Stream parsing.

use thiserror::Error;

/// Errors that can occur during event-stream parsing.
#[derive(Debug, Error)]
pub enum ParseError {
    #[error("Incomplete frame: needed {needed} bytes, available {available} bytes")]
    Incomplete { needed: usize, available: usize },

    #[error("Prelude CRC mismatch: expected 0x{expected:08x}, actual 0x{actual:08x}")]
    PreludeCrcMismatch { expected: u32, actual: u32 },

    #[error("Message CRC mismatch: expected 0x{expected:08x}, actual 0x{actual:08x}")]
    MessageCrcMismatch { expected: u32, actual: u32 },

    #[error("Invalid header value type: {0}")]
    InvalidHeaderType(u8),

    #[error("Header parse failed: {0}")]
    HeaderParseFailed(String),

    #[error("Message too large: length {length} exceeds maximum {max}")]
    MessageTooLarge { length: u32, max: u32 },

    #[error("Message too small: length {length} below minimum {min}")]
    MessageTooSmall { length: u32, min: u32 },

    #[error("Invalid message type: {0}")]
    InvalidMessageType(String),

    #[error("Payload JSON deserialization failed: {0}")]
    PayloadDeserialize(#[from] serde_json::Error),

    #[error("Buffer overflow: size {size} exceeds maximum {max}")]
    BufferOverflow { size: usize, max: usize },

    #[error("Too many consecutive errors ({count}): {last_error}")]
    TooManyErrors { count: usize, last_error: String },
}

pub type ParseResult<T> = Result<T, ParseError>;
