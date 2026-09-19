//! `kiro-wire`: AWS Event Stream & Kiro protocol wire format codec.
//!
//! Pure library with no I/O dependencies.

pub mod crc;
pub mod decoder;
pub mod encoder;
pub mod error;
pub mod events;
pub mod frame;
pub mod header;
pub mod requests;

pub use crc::crc32;
pub use decoder::{DecoderState, EventStreamDecoder};
pub use encoder::*;
pub use error::{ParseError, ParseResult};
pub use events::*;
pub use frame::{parse_frame, Frame};
pub use header::{HeaderValue, HeaderValueType, Headers};
pub use requests::*;

#[allow(dead_code)] // ponytail: reserved for future version negotiation
pub const PROTOCOL_VERSION: &str = "1.0";
