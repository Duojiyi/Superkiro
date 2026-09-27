use futures_util::{stream, StreamExt};
use gateway::provider::{ProviderDelta, ProviderError, ProviderStreamEvent, TokenUsage};
use gateway::stream::{create_stream_guard, StreamGuardConfig};
use kiro_wire::decoder::EventStreamDecoder;
use kiro_wire::events::ContextUsageEvent;
use std::time::Duration;

mod support;

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
            prompt_final: false,
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
    // The prompt and the answer the next request sends back, of 200,000 tokens: Kiro reads
    // a percentage, and compacts at 80.
    let answer = gateway::usage_estimate::tokens_from_units(gateway::usage_estimate::token_units(
        "Hello context!",
    ));
    let expected = (40_000 + answer) as f64 * 100.0 / 200_000.0;
    assert!(
        (verified_percentage - expected).abs() < 1e-6,
        "{verified_percentage}"
    );
}

/// The percentages Kiro is told while `events` stream against a 200,000-token window.
async fn context_percentages(events: Vec<ProviderStreamEvent>) -> Vec<f64> {
    let config = StreamGuardConfig {
        keepalive_interval: Duration::from_secs(60),
        model_id: "claude-3-5-sonnet".to_string(),
        context_window: Some(200_000),
    };
    let mut guarded_stream = create_stream_guard(
        stream::iter(events.into_iter().map(Ok::<_, ProviderError>)),
        config,
        None,
        None,
        None,
    );
    let mut decoder = EventStreamDecoder::new();
    let mut percentages = Vec::new();
    while let Some(chunk) = guarded_stream.next().await {
        decoder.feed(&chunk.unwrap()).unwrap();
        let (frames, _) = decoder.decode_all();
        for frame in frames {
            if frame.event_type() == Some("contextUsageEvent") {
                let event: ContextUsageEvent = frame.payload_as_json().unwrap();
                percentages.push(event.context_usage_percentage);
            }
        }
    }
    percentages
}

/// Thinking is not sent back with the next request unless the provider replays it, so it
/// does not fill the context Kiro compacts on: counted, a long think had Kiro summarize the
/// conversation early. The upstream's output count includes it, so the answer is counted
/// from what was streamed.
#[tokio::test]
async fn thinking_that_is_not_sent_back_does_not_count_toward_the_context() {
    let percentages = context_percentages(vec![
        ProviderStreamEvent::Delta(ProviderDelta::Reasoning("think ".repeat(20_000))),
        ProviderStreamEvent::Delta(ProviderDelta::Text("answer".to_string())),
        ProviderStreamEvent::Usage(TokenUsage {
            uncached_prompt_tokens: 40_000,
            prompt_tokens: 40_000,
            completion_tokens: 30_002,
            total_tokens: 70_002,
            output_tokens_final: true,
            prompt_final: false,
            cache_read_input_tokens: None,
            cache_creation_input_tokens: None,
        }),
        ProviderStreamEvent::Done,
    ])
    .await;
    // 40,000 of prompt and the two tokens of "answer".
    assert_eq!(percentages.len(), 1);
    assert!(
        (percentages[0] - 40_002.0 * 100.0 / 200_000.0).abs() < 1e-6,
        "{percentages:?}"
    );
}

#[tokio::test]
async fn test_stream_guard_context_usage_custom_window_and_clamping() {
    // Upstream with 120,000 tokens against a 100,000 custom window (120% -> clamped to 100)
    let upstream_events: Vec<Result<ProviderStreamEvent, ProviderError>> = vec![
        Ok(ProviderStreamEvent::Usage(TokenUsage {
            uncached_prompt_tokens: 100_000,
            prompt_tokens: 100_000,
            completion_tokens: 20_000,
            total_tokens: 120_000,
            output_tokens_final: true,
            prompt_final: false,
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

    assert_eq!(verified_percentage, 100.0);
}

/// Thinking a provider is sent again with the next request does fill the context. The
/// provider options are process-wide, so this is the only test here that serves a provider.
#[tokio::test]
async fn thinking_a_provider_is_sent_again_counts_toward_the_context() {
    std::env::set_var("PROVIDER_THINKING_REPLAY", "replaying");
    gateway::provider::install_provider_options(gateway::provider::ProviderOptionsTable::from_env());
    let billing = billing::BillingEngine::new();
    billing.upsert_rate_card_version(support::wildcard_price("default"));
    let mut card = billing::Card::new("card-context", "group-default", 1_000_000_000);
    card.activate(gateway::now_secs(), 86_400).unwrap();
    billing.upsert_card(card);
    billing
        .reserve(
            "card-context",
            "inv-context",
            &billing::ReservationEstimateParams::new(40_000, 32_000).with_model("model"),
            gateway::now_secs(),
            660,
        )
        .unwrap();
    let settler = gateway::stream::BillingSettler::new(
        billing.clone(),
        "inv-context".into(),
        "model".into(),
        "replaying".into(),
        "model".into(),
    );
    let events = vec![
        ProviderStreamEvent::Delta(ProviderDelta::Reasoning("think ".repeat(20_000))),
        ProviderStreamEvent::Delta(ProviderDelta::Text("answer".to_string())),
        ProviderStreamEvent::Usage(TokenUsage {
            uncached_prompt_tokens: 40_000,
            prompt_tokens: 40_000,
            completion_tokens: 30_002,
            total_tokens: 70_002,
            output_tokens_final: true,
            prompt_final: false,
            cache_read_input_tokens: None,
            cache_creation_input_tokens: None,
        }),
        ProviderStreamEvent::Done,
    ];
    let config = StreamGuardConfig {
        keepalive_interval: Duration::from_secs(60),
        model_id: "model".to_string(),
        context_window: Some(200_000),
    };
    let mut guarded_stream = create_stream_guard(
        stream::iter(events.into_iter().map(Ok::<_, ProviderError>)),
        config,
        None,
        None,
        Some(settler),
    );
    let mut decoder = EventStreamDecoder::new();
    let mut percentages = Vec::new();
    while let Some(chunk) = guarded_stream.next().await {
        decoder.feed(&chunk.unwrap()).unwrap();
        let (frames, _) = decoder.decode_all();
        for frame in frames {
            if frame.event_type() == Some("contextUsageEvent") {
                let event: ContextUsageEvent = frame.payload_as_json().unwrap();
                percentages.push(event.context_usage_percentage);
            }
        }
    }
    // 40,000 of prompt, 30,000 of thinking and the two tokens of "answer".
    assert_eq!(percentages.len(), 1);
    assert!(
        (percentages[0] - 70_002.0 * 100.0 / 200_000.0).abs() < 1e-6,
        "{percentages:?}"
    );
}
