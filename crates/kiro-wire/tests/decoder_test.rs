use kiro_wire::{DecoderState, EventStreamDecoder, ParseError};
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
fn test_decode_assistant_response_corpus() {
    let path = corpus_dir().join("01_assistant_response.bin");
    let bytes = fs::read(&path).expect("Failed to read 01_assistant_response.bin");

    let mut decoder = EventStreamDecoder::new();
    decoder.feed(&bytes).unwrap();

    let (frames, errors) = decoder.decode_all();
    assert!(errors.is_empty(), "Decoding errors: {:?}", errors);
    assert_eq!(frames.len(), 3);

    assert_eq!(frames[0].event_type(), Some("assistantResponseEvent"));
    assert_eq!(frames[0].message_type(), Some("event"));
    assert!(frames[0]
        .payload_to_string_lossy()
        .contains("claude-3-7-sonnet"));
    assert!(frames[0].payload_to_string_lossy().contains("Hello! "));

    assert_eq!(frames[1].event_type(), Some("assistantResponseEvent"));
    assert!(frames[1]
        .payload_to_string_lossy()
        .contains("I am Kiro Assistant. "));

    assert_eq!(frames[2].event_type(), Some("assistantResponseEvent"));
    assert!(frames[2]
        .payload_to_string_lossy()
        .contains("How can I assist you with your project today?"));
}

#[test]
fn test_decode_tool_use_corpus() {
    let path = corpus_dir().join("02_tool_use.bin");
    let bytes = fs::read(&path).expect("Failed to read 02_tool_use.bin");

    let mut decoder = EventStreamDecoder::new();
    decoder.feed(&bytes).unwrap();

    let (frames, errors) = decoder.decode_all();
    assert!(errors.is_empty(), "Decoding errors: {:?}", errors);
    assert_eq!(frames.len(), 3);

    for frame in &frames {
        assert_eq!(frame.event_type(), Some("toolUseEvent"));
        assert!(frame.payload_to_string_lossy().contains("tooluse_abc123"));
        assert!(frame.payload_to_string_lossy().contains("fs_read"));
    }

    assert!(frames[2]
        .payload_to_string_lossy()
        .contains("\"stop\": true"));
}

#[test]
fn test_decode_reasoning_corpus() {
    let path = corpus_dir().join("03_reasoning_content.bin");
    let bytes = fs::read(&path).expect("Failed to read 03_reasoning_content.bin");

    let mut decoder = EventStreamDecoder::new();
    decoder.feed(&bytes).unwrap();

    let (frames, errors) = decoder.decode_all();
    assert!(errors.is_empty(), "Decoding errors: {:?}", errors);
    assert_eq!(frames.len(), 2);

    assert_eq!(frames[0].event_type(), Some("reasoningContentEvent"));
    assert!(frames[0]
        .payload_to_string_lossy()
        .contains("Let me analyze"));

    assert_eq!(frames[1].event_type(), Some("reasoningContentEvent"));
    assert!(frames[1]
        .payload_to_string_lossy()
        .contains("sig_reasoning_token_signature_hex_123"));
}

#[test]
fn test_decode_context_and_metadata_corpus() {
    let path = corpus_dir().join("04_context_and_metadata.bin");
    let bytes = fs::read(&path).expect("Failed to read 04_context_and_metadata.bin");

    let mut decoder = EventStreamDecoder::new();
    decoder.feed(&bytes).unwrap();

    let (frames, errors) = decoder.decode_all();
    assert!(errors.is_empty(), "Decoding errors: {:?}", errors);
    assert_eq!(frames.len(), 2);

    assert_eq!(frames[0].event_type(), Some("contextUsageEvent"));
    assert!(frames[0]
        .payload_to_string_lossy()
        .contains("\"contextUsagePercentage\": 0.125"));

    assert_eq!(frames[1].event_type(), Some("metadataEvent"));
    assert!(frames[1]
        .payload_to_string_lossy()
        .contains("\"uncachedInputTokens\": 180"));
    assert!(frames[1]
        .payload_to_string_lossy()
        .contains("\"outputTokens\": 75"));
    assert!(frames[1]
        .payload_to_string_lossy()
        .contains("\"stopReason\": \"end_turn\""));
}

#[test]
fn test_decode_empty_keepalive_corpus() {
    let path = corpus_dir().join("05_empty_keepalive.bin");
    let bytes = fs::read(&path).expect("Failed to read 05_empty_keepalive.bin");

    let mut decoder = EventStreamDecoder::new();
    decoder.feed(&bytes).unwrap();

    let (frames, errors) = decoder.decode_all();
    assert!(errors.is_empty(), "Decoding errors: {:?}", errors);
    assert_eq!(frames.len(), 1);

    assert_eq!(frames[0].event_type(), Some("assistantResponseEvent"));
    assert_eq!(frames[0].payload_to_string_lossy(), "{\"content\": \"\"}");
}

#[test]
fn test_decode_exception_corpus() {
    let path = corpus_dir().join("06_exception_frame.bin");
    let bytes = fs::read(&path).expect("Failed to read 06_exception_frame.bin");

    let mut decoder = EventStreamDecoder::new();
    decoder.feed(&bytes).unwrap();

    let (frames, errors) = decoder.decode_all();
    assert!(errors.is_empty(), "Decoding errors: {:?}", errors);
    assert_eq!(frames.len(), 1);

    assert_eq!(frames[0].message_type(), Some("exception"));
    assert_eq!(frames[0].exception_type(), Some("ValidationException"));
    assert!(frames[0]
        .payload_to_string_lossy()
        .contains("The model parameter is invalid"));
}

#[test]
fn test_streaming_byte_by_byte_feed() {
    let path = corpus_dir().join("01_assistant_response.bin");
    let bytes = fs::read(&path).expect("Failed to read 01_assistant_response.bin");

    let mut decoder = EventStreamDecoder::new();
    let mut decoded_frames = Vec::new();

    // Feed 1 byte at a time to test buffer fragmentation and state transitions
    for chunk in bytes.chunks(1) {
        decoder.feed(chunk).unwrap();
        while let Ok(Some(frame)) = decoder.decode() {
            decoded_frames.push(frame);
        }
    }

    assert_eq!(decoded_frames.len(), 3);
    assert_eq!(decoder.state(), DecoderState::Ready);
}

#[test]
fn test_corrupted_prelude_crc_recovery() {
    let path = corpus_dir().join("05_empty_keepalive.bin");
    let mut bytes = fs::read(&path).expect("Failed to read 05_empty_keepalive.bin");

    // Corrupt prelude CRC at bytes 8..12
    bytes[10] ^= 0xFF;

    let mut decoder = EventStreamDecoder::new();
    decoder.feed(&bytes).unwrap();

    let err = decoder.decode().unwrap_err();
    assert!(matches!(err, ParseError::PreludeCrcMismatch { .. }));
}

#[test]
fn test_corrupted_message_crc_recovery() {
    let path = corpus_dir().join("05_empty_keepalive.bin");
    let mut bytes = fs::read(&path).expect("Failed to read 05_empty_keepalive.bin");

    // Corrupt last byte (message CRC)
    let last = bytes.len() - 1;
    bytes[last] ^= 0xFF;

    let mut decoder = EventStreamDecoder::new();
    decoder.feed(&bytes).unwrap();

    let err = decoder.decode().unwrap_err();
    assert!(matches!(err, ParseError::MessageCrcMismatch { .. }));
}

#[test]
fn test_message_too_small_rejection() {
    // 16 bytes is minimum. Feed total_length = 8
    let mut invalid_prelude = vec![0u8; 16];
    invalid_prelude[0..4].copy_from_slice(&8u32.to_be_bytes()); // total_len = 8

    let mut decoder = EventStreamDecoder::new();
    decoder.feed(&invalid_prelude).unwrap();

    let err = decoder.decode().unwrap_err();
    assert!(matches!(err, ParseError::MessageTooSmall { .. }));
}

#[test]
fn test_excessive_errors_stops_decoder() {
    let mut decoder = EventStreamDecoder::new();
    // Feed random garbage that keeps failing prelude
    for _ in 0..10 {
        let _ = decoder.feed(&[0xFF; 20]);
        let _ = decoder.decode();
    }
    assert_eq!(decoder.state(), DecoderState::Stopped);
    let err = decoder.decode().unwrap_err();
    assert!(matches!(err, ParseError::TooManyErrors { .. }));
}
