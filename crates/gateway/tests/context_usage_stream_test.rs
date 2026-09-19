use futures_util::{stream, StreamExt};
use gateway::provider::{ProviderDelta, ProviderError, ProviderStreamEvent, TokenUsage};
use gateway::stream::{create_stream_guard, StreamGuardConfig};
use kiro_wire::decoder::EventStreamDecoder;
use kiro_wire::events::ContextUsageEvent;
use std::time::Duration;

#[tokio::test]
async fn test_stream_guard_emits_context_usage_event_on_provider_usage() {
    // Upstream stream emitting text and token usage
    let upstream_events: Vec<Result<ProviderStreamEvent, ProviderError>> = vec![
        Ok(ProviderStreamEvent::Delta(ProviderDelta::Text(
            "Hello context!".to_string(),
        ))),
        Ok(ProviderStreamEvent::Usage(TokenUsage {
            uncached_prompt_tokens: 40_000,
            prompt_tokens: 40_000,
            completion_tokens: 10_000,
            total_tokens: 50_000,
            output_tokens_final: true,
            cache_read_input_tokens: None,
            cache_creation_input_tokens: None,
        })),
        Ok(ProviderStreamEvent::Done),
    ];

    let upstream = stream::iter(upstream_events);
    // Model "claude-3-5-sonnet" has builtin preset context_window = 200,000
    let config = StreamGuardConfig {
        keepalive_interval: Duration::from_secs(60),
        model_id: "claude-3-5-sonnet".to_string(),
        context_window: None, // Resolves from ModelContextLibrary automatically
    };

    let mut guarded_stream = create_stream_guard(upstream, config, None, None, None);

    let mut decoder = EventStreamDecoder::new();
    let mut received_context_usage = false;
    let mut verified_percentage: f64 = 0.0;

    while let Some(chunk_res) = guarded_stream.next().await {
        let chunk = chunk_res.expect("stream chunk should be ok");
        decoder.feed(&chunk).expect("feed ok");
        let (frames, _) = decoder.decode_all();
        for frame in frames {
            if frame.event_type() == Some("contextUsageEvent") {
                let evt: ContextUsageEvent = frame.payload_as_json().expect("valid JSON payload");
                received_context_usage = true;
                verified_percentage = evt.context_usage_percentage;
            }
        }
    }

    assert!(received_context_usage, "contextUsageEvent must be emitted");
    // 50,000 / 200,000 = 0.25 (25% context usage)
    assert!((verified_percentage - 0.25f64).abs() < 1e-6);
}

#[tokio::test]
async fn test_stream_guard_context_usage_custom_window_and_clamping() {
    // Upstream with 120,000 tokens against a 100,000 custom window (120% -> clamped to 1.0)
    let upstream_events: Vec<Result<ProviderStreamEvent, ProviderError>> = vec![
        Ok(ProviderStreamEvent::Usage(TokenUsage {
            uncached_prompt_tokens: 100_000,
            prompt_tokens: 100_000,
            completion_tokens: 20_000,
            total_tokens: 120_000,
            output_tokens_final: true,
            cache_read_input_tokens: None,
            cache_creation_input_tokens: None,
        })),
        Ok(ProviderStreamEvent::Done),
    ];

    let upstream = stream::iter(upstream_events);
    let config = StreamGuardConfig {
        keepalive_interval: Duration::from_secs(60),
        model_id: "custom-model".to_string(),
        context_window: Some(100_000),
    };

    let mut guarded_stream = create_stream_guard(upstream, config, None, None, None);
    let mut decoder = EventStreamDecoder::new();
    let mut verified_percentage: f64 = 0.0;

    while let Some(chunk_res) = guarded_stream.next().await {
        let chunk = chunk_res.expect("chunk ok");
        decoder.feed(&chunk).expect("feed ok");
        let (frames, _) = decoder.decode_all();
        for frame in frames {
            if frame.event_type() == Some("contextUsageEvent") {
                let evt: ContextUsageEvent = frame.payload_as_json().expect("valid JSON");
                verified_percentage = evt.context_usage_percentage;
            }
        }
    }

    assert_eq!(verified_percentage, 1.0);
}
