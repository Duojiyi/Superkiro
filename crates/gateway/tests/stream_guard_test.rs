//! Integration tests for P1-9: Stream guard, keepalive, and idempotency deduplication.
//!
//! Spec §4.6, §4.7, §6.3.

use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use gateway::idempotency::{CompletedInvocation, IdempotencyError, IdempotencyManager};
use gateway::provider::{
    ProviderDelta, ProviderError, ProviderStreamEvent, ReceiverStream, TokenUsage,
};
use gateway::stream::{create_stream_guard, StreamGuardConfig};
use gateway::translate::tools::ToolRegistry;
use kiro_wire::decoder::EventStreamDecoder;
use kiro_wire::events::{
    AssistantResponseEvent, MetadataEvent, ReasoningContentEvent, ToolUseEvent,
};
use std::time::Duration;
use tokio::sync::mpsc;

fn decode_single_event<T: serde::de::DeserializeOwned>(payload: &[u8]) -> T {
    serde_json::from_slice(payload).expect("Failed to deserialize event payload")
}

/// Helper to decode binary frames from a stream of bytes chunks.
async fn collect_and_decode_frames(
    mut stream: impl Stream<Item = Result<Bytes, std::io::Error>> + Unpin,
) -> Vec<(String, Vec<u8>)> {
    let mut decoder = EventStreamDecoder::new();
    let mut decoded = Vec::new();

    while let Some(chunk_res) = stream.next().await {
        let chunk = chunk_res.expect("Stream chunk error");
        decoder.feed(&chunk).expect("Feed chunk error");
        while let Some(f) = decoder.decode().expect("Decode frame error") {
            let event_type = f
                .headers
                .get(":event-type")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let exception_type = f
                .headers
                .get(":exception-type")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let name = if !event_type.is_empty() {
                event_type
            } else {
                exception_type
            };
            decoded.push((name, f.payload));
        }
    }
    decoded
}

#[tokio::test]
async fn test_stream_guard_keepalive_injection_during_idle() {
    // Upstream stream that stays idle for 180ms before sending a text token
    let (tx, rx) = mpsc::channel(16);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(180)).await;
        let _ = tx
            .send(Ok(ProviderStreamEvent::Delta(ProviderDelta::Text(
                "Hello after delay".to_string(),
            ))))
            .await;
        let _ = tx.send(Ok(ProviderStreamEvent::Done)).await;
    });

    let config = StreamGuardConfig {
        keepalive_interval: Duration::from_millis(50), // 50ms keepalive for fast test
        model_id: "test-model".to_string(),
        context_window: None,
    };

    let stream = create_stream_guard(ReceiverStream::new(rx), config, None, None, None);
    let frames = collect_and_decode_frames(stream).await;

    // We should receive at least 2 keepalive frames during the 180ms idle period,
    // followed by the actual text frame
    assert!(
        frames.len() >= 3,
        "Expected at least 2 keepalives + 1 text frame, got {}",
        frames.len()
    );

    // Keepalive frames are empty assistantResponseEvent (content == "")
    let keepalives: Vec<_> = frames
        .iter()
        .filter(|(name, payload)| {
            if name == "assistantResponseEvent" {
                let evt: AssistantResponseEvent = decode_single_event(payload);
                evt.content.is_empty()
            } else {
                false
            }
        })
        .collect();

    assert!(
        keepalives.len() >= 2,
        "Expected >=2 keepalive pings during idle, got {}",
        keepalives.len()
    );

    // The final content is followed by normalized completion metadata.
    let (_, last_payload) = frames
        .iter()
        .rev()
        .find(|(name, _)| name == "assistantResponseEvent")
        .unwrap();
    let (last_name, metadata) = frames.last().unwrap();
    assert_eq!(last_name, "metadataEvent");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(metadata).unwrap()["stopReason"],
        "end_turn"
    );
    let text_evt: AssistantResponseEvent = decode_single_event(last_payload);
    assert_eq!(text_evt.content, "Hello after delay");
}

#[tokio::test]
async fn test_stream_guard_normal_flow_with_tool_use_and_usage() {
    let (tx, rx) = mpsc::channel(16);
    let mut tool_reg = ToolRegistry::new();
    let safe_tool_name = tool_reg
        .register("super_ultra_long_tool_name_that_exceeds_sixty_four_chars_and_needs_shortening");

    tokio::spawn(async move {
        // 1. Thinking chunk
        let _ = tx
            .send(Ok(ProviderStreamEvent::Delta(ProviderDelta::Reasoning(
                "Planning solution...".to_string(),
            ))))
            .await;

        // 2. Text chunk
        let _ = tx
            .send(Ok(ProviderStreamEvent::Delta(ProviderDelta::Text(
                "I will call the tool.".to_string(),
            ))))
            .await;

        // 3. Tool call chunk (with shortened name)
        let _ = tx
            .send(Ok(ProviderStreamEvent::Delta(
                ProviderDelta::ToolCallChunk {
                    index: 0,
                    id: Some("call_abc123".to_string()),
                    name: Some(safe_tool_name),
                    arguments: "{\"arg\":42}".to_string(),
                },
            )))
            .await;

        // 4. Token usage
        let _ = tx
            .send(Ok(ProviderStreamEvent::Usage(TokenUsage {
                uncached_prompt_tokens: 80,
                prompt_tokens: 150,
                completion_tokens: 45,
                total_tokens: 195,
                output_tokens_final: true,
                cache_read_input_tokens: Some(50),
                cache_creation_input_tokens: Some(20),
            })))
            .await;

        // 5. Done
        let _ = tx.send(Ok(ProviderStreamEvent::Done)).await;
    });

    let config = StreamGuardConfig {
        keepalive_interval: Duration::from_secs(10),
        model_id: "claude-sonnet".to_string(),
        context_window: None,
    };

    let stream = create_stream_guard(ReceiverStream::new(rx), config, Some(tool_reg), None, None);
    let frames = collect_and_decode_frames(stream).await;

    // Verify reasoning
    let reasoning_frame = frames
        .iter()
        .find(|(name, _)| name == "reasoningContentEvent")
        .expect("Missing reasoningContentEvent");
    let reasoning: ReasoningContentEvent = decode_single_event(&reasoning_frame.1);
    assert_eq!(reasoning.text.as_deref(), Some("Planning solution..."));

    // Verify text
    let text_frame = frames
        .iter()
        .find(|(name, payload)| {
            if name == "assistantResponseEvent" {
                let e: AssistantResponseEvent = decode_single_event(payload);
                e.content == "I will call the tool."
            } else {
                false
            }
        })
        .expect("Missing assistant response");
    let text_evt: AssistantResponseEvent = decode_single_event(&text_frame.1);
    assert_eq!(text_evt.model_id.as_deref(), Some("claude-sonnet"));

    // Verify tool use name was restored to original long name
    let tool_frame = frames
        .iter()
        .find(|(name, _)| name == "toolUseEvent")
        .expect("Missing toolUseEvent");
    let tool_evt: ToolUseEvent = decode_single_event(&tool_frame.1);
    assert_eq!(
        tool_evt.name,
        "super_ultra_long_tool_name_that_exceeds_sixty_four_chars_and_needs_shortening"
    );
    assert_eq!(tool_evt.tool_use_id, "call_abc123");
    assert_eq!(tool_evt.input, "{\"arg\":42}");

    // Verify metadata usage
    let metadata_frame = frames
        .iter()
        .find(|(name, _)| name == "metadataEvent")
        .expect("Missing metadataEvent");
    let meta_evt: MetadataEvent = decode_single_event(&metadata_frame.1);
    let u = meta_evt.token_usage.expect("Missing token usage");
    assert_eq!(u.output_tokens, 45);
    assert_eq!(u.cache_read_input_tokens, 50);
    assert_eq!(u.cache_write_input_tokens, 20);
    assert_eq!(u.uncached_input_tokens, 80); // 150 - (50 + 20) = 80
}

#[tokio::test]
async fn test_audit_b_concurrent_replay_deduplication() {
    let manager = IdempotencyManager::new(Duration::from_secs(30));
    let invocation_id = "550e8400-e29b-41d4-a716-446655440000";

    // Attempt 1 acquires execution lock
    let guard1 = manager
        .try_acquire(invocation_id)
        .expect("Attempt 1 must succeed");
    assert!(manager.is_in_progress(invocation_id));

    // Attempt 2 with same invocation_id while Attempt 1 is in-progress must be rejected
    let err = manager.try_acquire(invocation_id).unwrap_err();
    assert_eq!(err, IdempotencyError::InProgress(invocation_id.to_string()));

    // Attempt 1 finishes successfully and commits
    guard1.commit(CompletedInvocation {
        completed_at: std::time::Instant::now(),
        model_id: "claude-sonnet".to_string(),
        total_input_tokens: 150,
        total_output_tokens: 45,
    });

    assert!(!manager.is_in_progress(invocation_id));
    assert!(manager.get_completed(invocation_id).is_some());

    // Attempt 3 replay after completion is short-circuited
    let err2 = manager.try_acquire(invocation_id).unwrap_err();
    assert_eq!(
        err2,
        IdempotencyError::AlreadyCompleted(invocation_id.to_string())
    );
}

#[tokio::test]
async fn test_audit_b_client_disconnect_cancellation_and_no_leak() {
    let manager = IdempotencyManager::new(Duration::from_secs(30));
    let invocation_id = "test-client-disconnect-uuid";

    let guard = manager
        .try_acquire(invocation_id)
        .expect("Initial acquire succeeds");
    assert!(manager.is_in_progress(invocation_id));

    let (tx, rx) = mpsc::channel(16);

    // Simulate an infinite upstream stream
    tokio::spawn(async move {
        loop {
            if tx
                .send(Ok(ProviderStreamEvent::Delta(ProviderDelta::Text(
                    "chunk".to_string(),
                ))))
                .await
                .is_err()
            {
                // Receiver was dropped / disconnected
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    });

    let config = StreamGuardConfig {
        keepalive_interval: Duration::from_secs(1),
        model_id: "model".to_string(),
        context_window: None,
    };

    let stream = create_stream_guard(ReceiverStream::new(rx), config, None, Some(guard), None);

    // Client drops stream abruptly after reading 1 chunk
    let mut boxed = Box::pin(stream);
    let first = boxed.next().await;
    assert!(first.is_some());

    // Client drops socket
    drop(boxed);

    // Allow worker select! loop to detect disconnect and drop guard
    tokio::time::sleep(Duration::from_millis(60)).await;

    // Verify guard was aborted automatically on drop: invocation is no longer in progress
    assert!(!manager.is_in_progress(invocation_id));
    assert!(manager.get_completed(invocation_id).is_none());

    // Client retry can now immediately re-acquire without being blocked!
    let _retry_guard = manager
        .try_acquire(invocation_id)
        .expect("Retry after disconnect must succeed");
}

#[tokio::test]
async fn test_audit_b_mid_stream_provider_error_friendly_presentation() {
    let (tx, rx) = mpsc::channel(16);

    tokio::spawn(async move {
        // Emit 1 token
        let _ = tx
            .send(Ok(ProviderStreamEvent::Delta(ProviderDelta::Text(
                "Starting...".to_string(),
            ))))
            .await;

        // Then emit upstream provider error
        let _ = tx
            .send(Err(ProviderError::Http(
                reqwest::StatusCode::TOO_MANY_REQUESTS,
                "Rate limit exceeded (429)".to_string(),
            )))
            .await;
    });

    let config = StreamGuardConfig {
        keepalive_interval: Duration::from_secs(10),
        model_id: "m".to_string(),
        context_window: None,
    };

    let stream = create_stream_guard(ReceiverStream::new(rx), config, None, None, None);
    let frames = collect_and_decode_frames(stream).await;

    // Must contain the user-friendly Markdown error text chunk
    let friendly_frame = frames
        .iter()
        .find(|(name, payload)| {
            if name == "assistantResponseEvent" {
                let e: AssistantResponseEvent = decode_single_event(payload);
                e.content.contains("上游模型服务异常")
            } else {
                false
            }
        })
        .expect("Must emit user-friendly markdown error event");
    let friendly_evt: AssistantResponseEvent = decode_single_event(&friendly_frame.1);
    // Vendor response bodies are intentionally redacted at the client
    // boundary; only the stable upstream category/status is exposed.
    assert!(friendly_evt.content.contains("upstream HTTP status 429"));

    // Must also contain structured AWS exception frame
    let exc_frame = frames
        .iter()
        .find(|(name, _)| name == "InternalServerException")
        .expect("Must emit InternalServerException frame");
    let exc_json: serde_json::Value =
        serde_json::from_slice(&exc_frame.1).expect("Parse exception json");
    assert!(exc_json["message"].as_str().unwrap().contains("429"));
}
