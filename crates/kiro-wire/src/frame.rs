//! AWS Event Stream Message Frame representation and stateless frame parser.

use crate::crc::crc32;
use crate::error::{ParseError, ParseResult};
use crate::header::{parse_headers, Headers};

/// Prelude size is fixed at 12 bytes.
pub const PRELUDE_SIZE: usize = 12;

/// Minimum message size is prelude (12B) + message crc (4B) = 16 bytes.
pub const MIN_MESSAGE_SIZE: usize = PRELUDE_SIZE + 4;

/// Maximum message size limit (16MB).
pub const MAX_MESSAGE_SIZE: u32 = 16 * 1024 * 1024;

/// A parsed AWS Event Stream frame.
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    pub headers: Headers,
    pub payload: Vec<u8>,
}

impl Frame {
    pub fn new(headers: Headers, payload: Vec<u8>) -> Self {
        Self { headers, payload }
    }

    pub fn message_type(&self) -> Option<&str> {
        self.headers.message_type()
    }

    pub fn event_type(&self) -> Option<&str> {
        self.headers.event_type()
    }

    pub fn exception_type(&self) -> Option<&str> {
        self.headers.exception_type()
    }

    pub fn content_type(&self) -> Option<&str> {
        self.headers.content_type()
    }

    pub fn payload_as_json<T: serde::de::DeserializeOwned>(&self) -> ParseResult<T> {
        serde_json::from_slice(&self.payload).map_err(ParseError::PayloadDeserialize)
    }

    pub fn payload_to_string_lossy(&self) -> String {
        String::from_utf8_lossy(&self.payload).to_string()
    }

    #[deprecated(note = "use payload_to_string_lossy instead")]
    pub fn payload_as_str(&self) -> String {
        self.payload_to_string_lossy()
    }
}

/// Try to parse a single complete frame from a byte buffer.
///
/// Returns:
/// - `Ok(Some((frame, consumed_bytes)))` on complete valid frame.
/// - `Ok(None)` if more bytes are needed.
/// - `Err(ParseError)` if frame is corrupt or invalid.
pub fn parse_frame(buffer: &[u8]) -> ParseResult<Option<(Frame, usize)>> {
    if buffer.len() < PRELUDE_SIZE {
        return Ok(None);
    }

    let total_length = u32::from_be_bytes([buffer[0], buffer[1], buffer[2], buffer[3]]);
    let header_length = u32::from_be_bytes([buffer[4], buffer[5], buffer[6], buffer[7]]);
    let prelude_crc = u32::from_be_bytes([buffer[8], buffer[9], buffer[10], buffer[11]]);

    if total_length < MIN_MESSAGE_SIZE as u32 {
        return Err(ParseError::MessageTooSmall {
            length: total_length,
            min: MIN_MESSAGE_SIZE as u32,
        });
    }

    if total_length > MAX_MESSAGE_SIZE {
        return Err(ParseError::MessageTooLarge {
            length: total_length,
            max: MAX_MESSAGE_SIZE,
        });
    }

    let total_len = total_length as usize;
    let header_len = header_length as usize;

    // Verify Prelude CRC
    let actual_prelude_crc = crc32(&buffer[..8]);
    if actual_prelude_crc != prelude_crc {
        return Err(ParseError::PreludeCrcMismatch {
            expected: prelude_crc,
            actual: actual_prelude_crc,
        });
    }

    if buffer.len() < total_len {
        return Ok(None);
    }

    // Verify Message CRC
    let message_crc = u32::from_be_bytes([
        buffer[total_len - 4],
        buffer[total_len - 3],
        buffer[total_len - 2],
        buffer[total_len - 1],
    ]);
    let actual_message_crc = crc32(&buffer[..total_len - 4]);
    if actual_message_crc != message_crc {
        return Err(ParseError::MessageCrcMismatch {
            expected: message_crc,
            actual: actual_message_crc,
        });
    }

    // Parse Headers
    let headers_start = PRELUDE_SIZE;
    let headers_end = headers_start + header_len;
    if headers_end > total_len - 4 {
        return Err(ParseError::HeaderParseFailed(
            "Headers length exceeds frame boundary".to_string(),
        ));
    }

    let headers = parse_headers(&buffer[headers_start..headers_end], header_len)?;
    let payload = buffer[headers_end..total_len - 4].to_vec();

    Ok(Some((Frame { headers, payload }, total_len)))
}
