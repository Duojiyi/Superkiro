//! AWS Event Stream Header serialization and deserialization.

use crate::error::{ParseError, ParseResult};
use std::collections::HashMap;

/// Header value types supported by AWS Event Stream protocol.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderValueType {
    BoolTrue = 0,
    BoolFalse = 1,
    Byte = 2,
    Short = 3,
    Integer = 4,
    Long = 5,
    ByteArray = 6,
    String = 7,
    Timestamp = 8,
    Uuid = 9,
}

impl TryFrom<u8> for HeaderValueType {
    type Error = ParseError;

    fn try_from(value: u8) -> ParseResult<Self> {
        match value {
            0 => Ok(Self::BoolTrue),
            1 => Ok(Self::BoolFalse),
            2 => Ok(Self::Byte),
            3 => Ok(Self::Short),
            4 => Ok(Self::Integer),
            5 => Ok(Self::Long),
            6 => Ok(Self::ByteArray),
            7 => Ok(Self::String),
            8 => Ok(Self::Timestamp),
            9 => Ok(Self::Uuid),
            _ => Err(ParseError::InvalidHeaderType(value)),
        }
    }
}

/// Header value representation.
#[derive(Debug, Clone, PartialEq)]
pub enum HeaderValue {
    Bool(bool),
    Byte(i8),
    Short(i16),
    Integer(i32),
    Long(i64),
    ByteArray(Vec<u8>),
    String(String),
    Timestamp(i64),
    Uuid([u8; 16]),
}

impl HeaderValue {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(s) => Some(s),
            _ => None,
        }
    }
}

/// Collection of message headers.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Headers {
    inner: HashMap<String, HeaderValue>,
}

impl Headers {
    pub fn new() -> Self {
        Self {
            inner: HashMap::new(),
        }
    }

    pub fn insert(&mut self, name: impl Into<String>, value: HeaderValue) {
        self.inner.insert(name.into(), value);
    }

    pub fn get(&self, name: &str) -> Option<&HeaderValue> {
        self.inner.get(name)
    }

    pub fn get_string(&self, name: &str) -> Option<&str> {
        self.get(name).and_then(|v| v.as_str())
    }

    pub fn message_type(&self) -> Option<&str> {
        self.get_string(":message-type")
    }

    pub fn event_type(&self) -> Option<&str> {
        self.get_string(":event-type")
    }

    pub fn exception_type(&self) -> Option<&str> {
        self.get_string(":exception-type")
    }

    pub fn content_type(&self) -> Option<&str> {
        self.get_string(":content-type")
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &HeaderValue)> {
        self.inner.iter()
    }
}

/// Parse headers from raw byte slice.
pub fn parse_headers(data: &[u8], header_length: usize) -> ParseResult<Headers> {
    if data.len() < header_length {
        return Err(ParseError::Incomplete {
            needed: header_length,
            available: data.len(),
        });
    }
    let data = &data[..header_length]; // ponytail: boundary spill fix

    let mut headers = Headers::new();
    let mut offset = 0;

    while offset < header_length {
        if offset >= data.len() {
            break;
        }
        let name_len = data[offset] as usize;
        offset += 1;

        if name_len == 0 {
            return Err(ParseError::HeaderParseFailed(
                "Header name length cannot be 0".to_string(),
            ));
        }

        if offset + name_len > data.len() {
            return Err(ParseError::Incomplete {
                needed: name_len,
                available: data.len() - offset,
            });
        }
        let name = String::from_utf8_lossy(&data[offset..offset + name_len]).to_string();
        offset += name_len;

        if offset >= data.len() {
            return Err(ParseError::Incomplete {
                needed: 1,
                available: 0,
            });
        }
        let value_type = HeaderValueType::try_from(data[offset])?;
        offset += 1;

        let value = parse_header_value(&data[offset..], value_type, &mut offset)?;
        headers.insert(name, value);
    }

    Ok(headers)
}

fn parse_header_value(
    data: &[u8],
    value_type: HeaderValueType,
    global_offset: &mut usize,
) -> ParseResult<HeaderValue> {
    let mut local_offset = 0;

    let result = match value_type {
        HeaderValueType::BoolTrue => Ok(HeaderValue::Bool(true)),
        HeaderValueType::BoolFalse => Ok(HeaderValue::Bool(false)),
        HeaderValueType::Byte => {
            if data.is_empty() {
                return Err(ParseError::Incomplete {
                    needed: 1,
                    available: 0,
                });
            }
            local_offset = 1;
            Ok(HeaderValue::Byte(data[0] as i8))
        }
        HeaderValueType::Short => {
            if data.len() < 2 {
                return Err(ParseError::Incomplete {
                    needed: 2,
                    available: data.len(),
                });
            }
            local_offset = 2;
            Ok(HeaderValue::Short(i16::from_be_bytes([data[0], data[1]])))
        }
        HeaderValueType::Integer => {
            if data.len() < 4 {
                return Err(ParseError::Incomplete {
                    needed: 4,
                    available: data.len(),
                });
            }
            local_offset = 4;
            Ok(HeaderValue::Integer(i32::from_be_bytes([
                data[0], data[1], data[2], data[3],
            ])))
        }
        HeaderValueType::Long => {
            if data.len() < 8 {
                return Err(ParseError::Incomplete {
                    needed: 8,
                    available: data.len(),
                });
            }
            local_offset = 8;
            Ok(HeaderValue::Long(i64::from_be_bytes([
                data[0], data[1], data[2], data[3], data[4], data[5], data[6], data[7],
            ])))
        }
        HeaderValueType::ByteArray => {
            if data.len() < 2 {
                return Err(ParseError::Incomplete {
                    needed: 2,
                    available: data.len(),
                });
            }
            let len = u16::from_be_bytes([data[0], data[1]]) as usize;
            if data.len() < 2 + len {
                return Err(ParseError::Incomplete {
                    needed: 2 + len,
                    available: data.len(),
                });
            }
            local_offset = 2 + len;
            Ok(HeaderValue::ByteArray(data[2..2 + len].to_vec()))
        }
        HeaderValueType::String => {
            if data.len() < 2 {
                return Err(ParseError::Incomplete {
                    needed: 2,
                    available: data.len(),
                });
            }
            let len = u16::from_be_bytes([data[0], data[1]]) as usize;
            if data.len() < 2 + len {
                return Err(ParseError::Incomplete {
                    needed: 2 + len,
                    available: data.len(),
                });
            }
            local_offset = 2 + len;
            let s = String::from_utf8_lossy(&data[2..2 + len]).to_string();
            Ok(HeaderValue::String(s))
        }
        HeaderValueType::Timestamp => {
            if data.len() < 8 {
                return Err(ParseError::Incomplete {
                    needed: 8,
                    available: data.len(),
                });
            }
            local_offset = 8;
            Ok(HeaderValue::Timestamp(i64::from_be_bytes([
                data[0], data[1], data[2], data[3], data[4], data[5], data[6], data[7],
            ])))
        }
        HeaderValueType::Uuid => {
            if data.len() < 16 {
                return Err(ParseError::Incomplete {
                    needed: 16,
                    available: data.len(),
                });
            }
            local_offset = 16;
            let mut uuid = [0u8; 16];
            uuid.copy_from_slice(&data[..16]);
            Ok(HeaderValue::Uuid(uuid))
        }
    };

    *global_offset += local_offset;
    result
}
