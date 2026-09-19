//! Stateful streaming decoder for AWS Event Stream protocol.
//! Implements 4-state recovery state machine (Ready, Parsing, Recovering, Stopped).

use crate::error::{ParseError, ParseResult};
use crate::frame::{parse_frame, Frame, PRELUDE_SIZE};
use bytes::{Buf, BytesMut};

pub const DEFAULT_MAX_BUFFER_SIZE: usize = 16 * 1024 * 1024;
pub const DEFAULT_MAX_ERRORS: usize = 5;
pub const DEFAULT_BUFFER_CAPACITY: usize = 8192;

/// Decoder state machine states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecoderState {
    Ready,
    Parsing,
    Recovering,
    Stopped,
}

/// Stateful stream decoder.
pub struct EventStreamDecoder {
    buffer: BytesMut,
    state: DecoderState,
    frames_decoded: usize,
    error_count: usize,
    max_errors: usize,
    max_buffer_size: usize,
    bytes_skipped: usize,
}

impl Default for EventStreamDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl EventStreamDecoder {
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_BUFFER_CAPACITY)
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            buffer: BytesMut::with_capacity(capacity),
            state: DecoderState::Ready,
            frames_decoded: 0,
            error_count: 0,
            max_errors: DEFAULT_MAX_ERRORS,
            max_buffer_size: DEFAULT_MAX_BUFFER_SIZE,
            bytes_skipped: 0,
        }
    }

    pub fn state(&self) -> DecoderState {
        self.state
    }

    pub fn frames_decoded(&self) -> usize {
        self.frames_decoded
    }

    pub fn bytes_skipped(&self) -> usize {
        self.bytes_skipped
    }

    /// Reset the decoder state to recover from Stopped.
    pub fn reset(&mut self) {
        self.buffer.clear();
        self.state = DecoderState::Ready;
        self.error_count = 0;
    }

    /// Feed incoming byte chunk into internal buffer.
    pub fn feed(&mut self, data: &[u8]) -> ParseResult<()> {
        if self.state == DecoderState::Stopped {
            return Err(ParseError::TooManyErrors {
                count: self.error_count,
                last_error: "Decoder stopped due to excessive errors".to_string(),
            });
        }

        let new_size = self.buffer.len() + data.len();
        if new_size > self.max_buffer_size {
            return Err(ParseError::BufferOverflow {
                size: new_size,
                max: self.max_buffer_size,
            });
        }

        self.buffer.extend_from_slice(data);

        if self.state == DecoderState::Recovering {
            self.state = DecoderState::Ready;
        }

        Ok(())
    }

    /// Try to decode the next frame from buffer.
    pub fn decode(&mut self) -> ParseResult<Option<Frame>> {
        if self.state == DecoderState::Stopped {
            return Err(ParseError::TooManyErrors {
                count: self.error_count,
                last_error: "Decoder stopped due to excessive errors".to_string(),
            });
        }

        if self.buffer.is_empty() {
            self.state = DecoderState::Ready;
            return Ok(None);
        }

        self.state = DecoderState::Parsing;

        match parse_frame(&self.buffer) {
            Ok(Some((frame, consumed))) => {
                self.buffer.advance(consumed);
                self.state = DecoderState::Ready;
                self.frames_decoded += 1;
                self.error_count = 0;
                Ok(Some(frame))
            }
            Ok(None) => {
                self.state = DecoderState::Ready;
                Ok(None)
            }
            Err(e) => {
                self.error_count += 1;
                let error_msg = e.to_string();

                if self.error_count >= self.max_errors {
                    self.state = DecoderState::Stopped;
                    return Err(ParseError::TooManyErrors {
                        count: self.error_count,
                        last_error: error_msg,
                    });
                }

                self.try_recover(&e);
                self.state = DecoderState::Recovering;
                Err(e)
            }
        }
    }

    /// Decode all available frames currently in buffer.
    pub fn decode_all(&mut self) -> (Vec<Frame>, Vec<ParseError>) {
        let mut frames = Vec::new();
        let mut errors = Vec::new();

        while !self.buffer.is_empty() && self.state != DecoderState::Stopped {
            match self.decode() {
                Ok(Some(frame)) => frames.push(frame),
                Ok(None) => break,
                Err(err) => errors.push(err),
            }
        }

        (frames, errors)
    }

    /// Error recovery strategy:
    /// - Prelude errors: Advance 1 byte to search for next frame alignment
    /// - Data errors: Advance total_length bytes if reasonable, else 1 byte
    fn try_recover(&mut self, error: &ParseError) {
        if self.buffer.is_empty() {
            return;
        }

        match error {
            ParseError::PreludeCrcMismatch { .. }
            | ParseError::MessageTooSmall { .. }
            | ParseError::MessageTooLarge { .. } => {
                self.buffer.advance(1);
                self.bytes_skipped += 1;
            }
            ParseError::MessageCrcMismatch { .. } | ParseError::HeaderParseFailed(_) => {
                if self.buffer.len() >= PRELUDE_SIZE {
                    let total_length = u32::from_be_bytes([
                        self.buffer[0],
                        self.buffer[1],
                        self.buffer[2],
                        self.buffer[3],
                    ]) as usize;

                    if total_length >= 16 && total_length <= self.buffer.len() {
                        self.buffer.advance(total_length);
                        self.bytes_skipped += total_length;
                        return;
                    }
                }

                self.buffer.advance(1);
                self.bytes_skipped += 1;
            }
            _ => {
                self.buffer.advance(1);
                self.bytes_skipped += 1;
            }
        }
    }
}
