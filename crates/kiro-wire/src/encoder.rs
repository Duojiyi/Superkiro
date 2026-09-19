//! AWS Event Stream binary frame encoder.
//! Produces valid frames conforming to AWS Event Stream specification.

use crate::crc::crc32;
use crate::events::*;
use crate::frame::Frame;
use crate::header::{HeaderValue, Headers};
use serde::Serialize;

/// Encode a single header key-value pair into raw bytes.
pub fn encode_header(name: &str, value: &HeaderValue) -> Vec<u8> {
    let mut buf = Vec::new();
    // Header names are protocol data and may ultimately include client/vendor
    // supplied values.  Never panic while encoding a controllable string.
    let name_bytes = name.as_bytes();
    let name_len = name_bytes.len().min(u8::MAX as usize);
    buf.push(name_len as u8);
    buf.extend_from_slice(&name_bytes[..name_len]);

    match value {
        HeaderValue::Bool(true) => {
            buf.push(0); // BoolTrue
        }
        HeaderValue::Bool(false) => {
            buf.push(1); // BoolFalse
        }
        HeaderValue::Byte(v) => {
            buf.push(2);
            buf.push(*v as u8);
        }
        HeaderValue::Short(v) => {
            buf.push(3);
            buf.extend_from_slice(&v.to_be_bytes());
        }
        HeaderValue::Integer(v) => {
            buf.push(4);
            buf.extend_from_slice(&v.to_be_bytes());
        }
        HeaderValue::Long(v) => {
            buf.push(5);
            buf.extend_from_slice(&v.to_be_bytes());
        }
        HeaderValue::ByteArray(bytes) => {
            let bytes = &bytes[..bytes.len().min(u16::MAX as usize)];
            buf.push(6);
            buf.extend_from_slice(&(bytes.len() as u16).to_be_bytes());
            buf.extend_from_slice(bytes);
        }
        HeaderValue::String(s) => {
            buf.push(7); // String
            let s_bytes = s.as_bytes();
            let s_bytes = &s_bytes[..s_bytes.len().min(u16::MAX as usize)];
            buf.extend_from_slice(&(s_bytes.len() as u16).to_be_bytes());
            buf.extend_from_slice(s_bytes);
        }
        HeaderValue::Timestamp(v) => {
            buf.push(8);
            buf.extend_from_slice(&v.to_be_bytes());
        }
        HeaderValue::Uuid(uuid) => {
            buf.push(9);
            buf.extend_from_slice(uuid);
        }
    }

    buf
}

/// Encode raw headers collection into wire bytes in standard deterministic order.
pub fn encode_headers(headers: &Headers) -> Vec<u8> {
    let mut buf = Vec::new();

    // Standard headers order: event-type/exception-type, content-type, message-type, then others
    let mut keys: Vec<&String> = headers.iter().map(|(k, _)| k).collect();
    keys.sort_by(|a, b| {
        let rank = |k: &str| match k {
            ":event-type" | ":exception-type" => 0,
            ":content-type" => 1,
            ":message-type" => 2,
            _ => 3,
        };
        rank(a).cmp(&rank(b)).then(a.cmp(b))
    });

    for key in keys {
        if let Some(val) = headers.get(key) {
            buf.extend(encode_header(key, val));
        }
    }

    buf
}

/// Encode a `Frame` into full binary wire bytes.
pub fn encode_frame(frame: &Frame) -> Vec<u8> {
    let headers_bytes = encode_headers(&frame.headers);
    let payload_bytes = &frame.payload;

    let headers_len = headers_bytes.len();
    let total_len = 12 + headers_len + payload_bytes.len() + 4;

    let mut msg = Vec::with_capacity(total_len);

    // Prelude (12 bytes)
    msg.extend_from_slice(&(total_len as u32).to_be_bytes());
    msg.extend_from_slice(&(headers_len as u32).to_be_bytes());
    let prelude_crc = crc32(&msg[..8]);
    msg.extend_from_slice(&prelude_crc.to_be_bytes());

    // Headers & Payload
    msg.extend_from_slice(&headers_bytes);
    msg.extend_from_slice(payload_bytes);

    // Message CRC (4 bytes)
    let msg_crc = crc32(&msg);
    msg.extend_from_slice(&msg_crc.to_be_bytes());

    msg
}

/// Encode a typed event into an event-stream frame.
pub fn encode_event<T: Serialize>(
    event_type: &str,
    payload: &T,
) -> Result<Vec<u8>, serde_json::Error> {
    let payload_bytes = serde_json::to_vec(payload)?;

    let mut headers = Headers::new();
    headers.insert(":event-type", HeaderValue::String(event_type.to_string()));
    headers.insert(
        ":content-type",
        HeaderValue::String("application/json".to_string()),
    );
    headers.insert(":message-type", HeaderValue::String("event".to_string()));

    let frame = Frame::new(headers, payload_bytes);
    Ok(encode_frame(&frame))
}

/// Encode an exception into an event-stream frame.
pub fn encode_exception(exception_type: &str, message: &str) -> Vec<u8> {
    let payload_obj = serde_json::json!({ "message": message });
    let payload_bytes = serde_json::to_vec(&payload_obj).unwrap_or_default();

    let mut headers = Headers::new();
    headers.insert(
        ":exception-type",
        HeaderValue::String(exception_type.to_string()),
    );
    headers.insert(
        ":content-type",
        HeaderValue::String("application/json".to_string()),
    );
    headers.insert(
        ":message-type",
        HeaderValue::String("exception".to_string()),
    );

    let frame = Frame::new(headers, payload_bytes);
    encode_frame(&frame)
}

/// Helper: Encode assistant text response chunk.
pub fn encode_assistant_response(content: &str, model_id: Option<&str>) -> Vec<u8> {
    let mut evt = AssistantResponseEvent::new(content);
    if let Some(m) = model_id {
        evt = evt.with_model_id(m);
    }
    encode_event("assistantResponseEvent", &evt).unwrap_or_default()
}

/// Helper: Encode keepalive ping frame (empty assistantResponseEvent content).
pub fn encode_keepalive() -> Vec<u8> {
    encode_assistant_response("", None)
}

/// Helper: Encode tool use chunk.
pub fn encode_tool_use(name: &str, tool_use_id: &str, input: &str, stop: bool) -> Vec<u8> {
    let evt = ToolUseEvent::new(name, tool_use_id, input, stop);
    encode_event("toolUseEvent", &evt).unwrap_or_default()
}

/// Helper: Encode reasoning thinking chunk.
pub fn encode_reasoning(
    text: Option<&str>,
    signature: Option<&str>,
    redacted: Option<&str>,
) -> Vec<u8> {
    let evt = ReasoningContentEvent {
        text: text.map(ToString::to_string),
        signature: signature.map(ToString::to_string),
        redacted_content: redacted.map(ToString::to_string),
    };
    encode_event("reasoningContentEvent", &evt).unwrap_or_default()
}

/// Helper: Encode context usage percentage frame.
pub fn encode_context_usage(percentage: f64) -> Vec<u8> {
    let evt = ContextUsageEvent::new(percentage);
    encode_event("contextUsageEvent", &evt).unwrap_or_default()
}

/// Helper: Encode metadata usage snapshot frame.
pub fn encode_metadata(usage: Option<TokenUsage>, stop_reason: Option<&str>) -> Vec<u8> {
    let evt = MetadataEvent {
        token_usage: usage,
        stop_reason: stop_reason.map(ToString::to_string),
    };
    encode_event("metadataEvent", &evt).unwrap_or_default()
}
