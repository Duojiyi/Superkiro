use kiro_wire::{
    encode_assistant_response, encode_context_usage, encode_exception, encode_frame,
    encode_keepalive, encode_metadata, encode_reasoning, encode_tool_use, parse_frame,
    AssistantResponseEvent, ContextUsageEvent, EventStreamDecoder, MetadataEvent,
    ReasoningContentEvent, TokenUsage, ToolUseEvent,
};
use std::fs;
use std::path::PathBuf;

fn corpus_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("docs")
        .join("p0")
        .join("stream")
}

#[test]
fn test_roundtrip_assistant_response() {
    let bytes = encode_assistant_response("Hello from Rust encoder!", Some("claude-3-7-sonnet"));
    let (frame, consumed) = parse_frame(&bytes).unwrap().unwrap();
    assert_eq!(consumed, bytes.len());
    assert_eq!(frame.event_type(), Some("assistantResponseEvent"));
    assert_eq!(frame.content_type(), Some("application/json"));
    assert_eq!(frame.message_type(), Some("event"));

    let evt: AssistantResponseEvent = frame.payload_as_json().unwrap();
    assert_eq!(evt.content, "Hello from Rust encoder!");
    assert_eq!(evt.model_id.as_deref(), Some("claude-3-7-sonnet"));
}

#[test]
fn test_roundtrip_tool_use() {
    let bytes = encode_tool_use("fs_read", "tool_call_001", "{\"path\": \"test.rs\"}", true);
    let (frame, consumed) = parse_frame(&bytes).unwrap().unwrap();
    assert_eq!(consumed, bytes.len());
    assert_eq!(frame.event_type(), Some("toolUseEvent"));

    let evt: ToolUseEvent = frame.payload_as_json().unwrap();
    assert_eq!(evt.name, "fs_read");
    assert_eq!(evt.tool_use_id, "tool_call_001");
    assert_eq!(evt.input, "{\"path\": \"test.rs\"}");
    assert!(evt.stop);
}

#[test]
fn test_roundtrip_reasoning() {
    let bytes = encode_reasoning(Some("Deep reasoning tokens..."), Some("sig_999"), None);
    let (frame, consumed) = parse_frame(&bytes).unwrap().unwrap();
    assert_eq!(consumed, bytes.len());

    let evt: ReasoningContentEvent = frame.payload_as_json().unwrap();
    assert_eq!(evt.text.as_deref(), Some("Deep reasoning tokens..."));
    assert_eq!(evt.signature.as_deref(), Some("sig_999"));
}

#[test]
fn test_roundtrip_context_and_metadata() {
    let ctx_bytes = encode_context_usage(0.35);
    let (ctx_frame, _) = parse_frame(&ctx_bytes).unwrap().unwrap();
    let ctx_evt: ContextUsageEvent = ctx_frame.payload_as_json().unwrap();
    assert_eq!(ctx_evt.context_usage_percentage, 0.35);

    let meta_bytes = encode_metadata(
        Some(TokenUsage {
            uncached_input_tokens: 250,
            output_tokens: 120,
            cache_read_input_tokens: 50,
            cache_write_input_tokens: 0,
        }),
        Some("end_turn"),
    );
    let (meta_frame, _) = parse_frame(&meta_bytes).unwrap().unwrap();
    let meta_evt: MetadataEvent = meta_frame.payload_as_json().unwrap();
    let usage = meta_evt.token_usage.unwrap();
    assert_eq!(usage.uncached_input_tokens, 250);
    assert_eq!(usage.output_tokens, 120);
    assert_eq!(meta_evt.stop_reason.as_deref(), Some("end_turn"));
}

#[test]
fn test_roundtrip_exception() {
    let bytes = encode_exception("ValidationException", "Invalid parameters provided");
    let (frame, consumed) = parse_frame(&bytes).unwrap().unwrap();
    assert_eq!(consumed, bytes.len());
    assert_eq!(frame.message_type(), Some("exception"));
    assert_eq!(frame.exception_type(), Some("ValidationException"));
    assert!(frame
        .payload_to_string_lossy()
        .contains("Invalid parameters provided"));
}

#[test]
fn test_roundtrip_frame_preserves_all_headers() {
    let mut decoder = EventStreamDecoder::new();
    let original_bytes = encode_assistant_response("Ping", None);
    decoder.feed(&original_bytes).unwrap();
    let frame = decoder.decode().unwrap().unwrap();

    let re_encoded = encode_frame(&frame);
    assert_eq!(
        original_bytes, re_encoded,
        "Re-encoded frame must match original bytes exactly"
    );
}

#[test]
fn test_byte_exact_match_with_p0_keepalive_corpus() {
    let corpus_path = corpus_dir().join("05_empty_keepalive.bin");
    let corpus_bytes = fs::read(&corpus_path).expect("Failed to read 05_empty_keepalive.bin");

    let mut dec_corpus = EventStreamDecoder::new();
    dec_corpus.feed(&corpus_bytes).unwrap();
    let frame_c = dec_corpus.decode().unwrap().unwrap();

    let re_encoded = encode_frame(&frame_c);
    assert_eq!(
        corpus_bytes, re_encoded,
        "Re-encoding decoded P0 corpus frame must match original corpus bytes byte-for-byte"
    );

    // Also verify that encode_keepalive() decodes cleanly and has empty content
    let generated_bytes = encode_keepalive();
    let mut dec_gen = EventStreamDecoder::new();
    dec_gen.feed(&generated_bytes).unwrap();
    let frame_g = dec_gen.decode().unwrap().unwrap();

    assert_eq!(frame_g.event_type(), Some("assistantResponseEvent"));
    assert_eq!(frame_g.message_type(), Some("event"));
    assert_eq!(frame_g.content_type(), Some("application/json"));
    let parsed: serde_json::Value = serde_json::from_slice(&frame_g.payload).unwrap();
    assert_eq!(parsed["content"], "");
}
