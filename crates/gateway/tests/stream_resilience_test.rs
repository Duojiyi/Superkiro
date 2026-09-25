//! Comprehensive resilience tests for streaming exceptions, idempotency, and billing settlement (Spec §4.6, §4.7, §6.1, §6.3, T06).

use billing::card::Card;
use billing::crypto::MasterKek;
use billing::engine::BillingEngine;
use billing::reservation::ReservationEstimateParams;
use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use gateway::guardrail::CapacityGuardrail;
use gateway::idempotency::IdempotencyManager;
use gateway::provider::{
    ProviderDelta, ProviderError, ProviderStreamEvent, ReceiverStream, TokenUsage,
};
use gateway::stream::{
    create_stream_guard, create_stream_guard_with_send_deadline, BillingSettler, StreamGuardConfig,
};
use kiro_wire::decoder::EventStreamDecoder;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;

mod support;

fn create_test_billing() -> (BillingEngine, String) {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let temp_dir = std::env::temp_dir().join(format!(
        "kiro_test_t06_{}_{}_{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&temp_dir).unwrap();
    let state_file = temp_dir.join("billing_state.json");

    let kek = MasterKek::from_bytes([9u8; 32]);
    let billing = BillingEngine::new();
    billing.set_persistence_path(&state_file);
    billing.set_master_kek(kek);
    billing.upsert_rate_card_version(support::wildcard_price("default"));

    let mut card = Card::new("card-t06-test", "group-default", 10_000_000);
    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    card.activate(now_secs, 86400 * 30).unwrap();
    billing.upsert_card(card);

    (billing, "card-t06-test".to_string())
}

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

// --------------------------------------------------------------------------
// 1. TTFB Timeout before first token releases reservation with 0 charge
// --------------------------------------------------------------------------
#[tokio::test]
async fn test_t06_ttfb_timeout_releases_reservation() {
    let (billing, card_id) = create_test_billing();
    let initial_balance = billing.get_card(&card_id).unwrap().available_credits();

    let inv_id = "inv-ttfb-timeout";
    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let params = ReservationEstimateParams {
        estimated_input_tokens: 1000,
        max_output_tokens: 2000,
        input_rate_per_m: 15_000_000,
        output_rate_per_m: 60_000_000,
        credit_multiplier: 1.0,
        margin_multiplier: 1.0,
        model: Some("claude-3-5-sonnet".to_string()),
    };
    let _res = billing
        .reserve(&card_id, inv_id, &params, now_secs, 300)
        .unwrap();

    let (tx, rx) = mpsc::channel(16);
    // Upstream stream that never sends anything (triggers TTFB watchdog timeout)
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let _ = tx.send(Err(ProviderError::Timeout)).await;
    });

    let config = StreamGuardConfig {
        keepalive_interval: Duration::from_secs(60),
        model_id: "claude-3-5-sonnet".to_string(),
        context_window: None,
    };

    let settler = BillingSettler::new(
        billing.clone(),
        inv_id.to_string(),
        "claude-3-5-sonnet".to_string(),
        "provider-1".to_string(),
        "claude-3-5-sonnet".to_string(),
    )
    .with_estimated_input(1000);

    let idemp_mgr = IdempotencyManager::default();
    let idemp_guard = idemp_mgr.try_acquire(inv_id).unwrap();

    let stream = create_stream_guard(
        ReceiverStream::new(rx),
        config,
        None,
        Some(idemp_guard),
        Some(settler),
    );
    let frames = collect_and_decode_frames(stream).await;

    // Must emit exception frame to client
    assert!(frames
        .iter()
        .any(|(name, _)| name == "InternalServerException"));

    // Assert: reservation is released, card balance is untouched, credit_reserved is 0
    let card = billing.get_card(&card_id).unwrap();
    assert_eq!(card.credit_reserved, 0);
    assert_eq!(card.available_credits(), initial_balance);
    assert_eq!(
        billing.list_ledger_entries_for_card(&card_id, None).len(),
        0
    );

    // Assert: idempotency was not committed as completed, client can retry immediately
    assert!(idemp_mgr.get_completed(inv_id).is_none());
    assert!(idemp_mgr.try_acquire(inv_id).is_ok());
}

// --------------------------------------------------------------------------
// 2. Partial text emitted then EOF without Usage performs partial settlement
// --------------------------------------------------------------------------
#[tokio::test]
async fn test_t06_partial_text_then_eof_without_usage_settles_and_fails_idempotency() {
    let (billing, card_id) = create_test_billing();
    let initial_balance = billing.get_card(&card_id).unwrap().available_credits();

    let inv_id = "inv-partial-eof";
    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let params = ReservationEstimateParams {
        estimated_input_tokens: 1000,
        max_output_tokens: 2000,
        input_rate_per_m: 15_000_000,
        output_rate_per_m: 60_000_000,
        credit_multiplier: 1.0,
        margin_multiplier: 1.0,
        model: Some("claude-3-5-sonnet".to_string()),
    };
    let _res = billing
        .reserve(&card_id, inv_id, &params, now_secs, 300)
        .unwrap();

    let (tx, rx) = mpsc::channel(16);
    tokio::spawn(async move {
        // Emit 80 characters of text
        let _ = tx
            .send(Ok(ProviderStreamEvent::Delta(ProviderDelta::Text(
                "This is a partial streaming chunk containing eighty chars of output text."
                    .to_string(),
            ))))
            .await;
        // Upstream abruptly closes connection (EOF) without Usage or Done
    });

    let config = StreamGuardConfig {
        keepalive_interval: Duration::from_secs(60),
        model_id: "claude-3-5-sonnet".to_string(),
        context_window: None,
    };

    let settler = BillingSettler::new(
        billing.clone(),
        inv_id.to_string(),
        "claude-3-5-sonnet".to_string(),
        "provider-1".to_string(),
        "claude-3-5-sonnet".to_string(),
    )
    .with_estimated_input(1000);

    let idemp_mgr = IdempotencyManager::default();
    let idemp_guard = idemp_mgr.try_acquire(inv_id).unwrap();

    let stream = create_stream_guard(
        ReceiverStream::new(rx),
        config,
        None,
        Some(idemp_guard),
        Some(settler),
    );
    let frames = collect_and_decode_frames(stream).await;

    // Stream must notify client of abrupt end
    assert!(frames
        .iter()
        .any(|(name, _)| name == "InternalServerException"));

    // T06: Must NOT let customer consume compute for free!
    // Estimated tokens: 1000 input, ~19 output tokens
    let card = billing.get_card(&card_id).unwrap();
    assert_eq!(card.credit_reserved, 0);
    assert!(
        card.available_credits() < initial_balance,
        "Partial output consumption must be billed"
    );
    let ledger = billing.list_ledger_entries_for_card(&card_id, None);
    assert_eq!(ledger.len(), 1);
    assert!(ledger[0].output_tokens > 0);
    assert_eq!(ledger[0].input_tokens, 1000);

    // Idempotency: must be marked failed, not completed
    assert!(idemp_mgr.get_completed(inv_id).is_none());
    assert!(matches!(
        idemp_mgr.try_acquire(inv_id),
        Err(gateway::idempotency::IdempotencyError::AlreadyFailed(_))
    ));
}

// --------------------------------------------------------------------------
// 3. Error after Usage received settles actual usage
// --------------------------------------------------------------------------
#[tokio::test]
async fn test_t06_error_after_usage_settles_actual_usage() {
    let (billing, card_id) = create_test_billing();
    let initial_balance = billing.get_card(&card_id).unwrap().available_credits();

    let inv_id = "inv-error-after-usage";
    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let params = ReservationEstimateParams {
        estimated_input_tokens: 1000,
        max_output_tokens: 2000,
        input_rate_per_m: 15_000_000,
        output_rate_per_m: 60_000_000,
        credit_multiplier: 1.0,
        margin_multiplier: 1.0,
        model: Some("claude-3-5-sonnet".to_string()),
    };
    let _res = billing
        .reserve(&card_id, inv_id, &params, now_secs, 300)
        .unwrap();

    let (tx, rx) = mpsc::channel(16);
    tokio::spawn(async move {
        let _ = tx
            .send(Ok(ProviderStreamEvent::Delta(ProviderDelta::Text(
                "Hello!".to_string(),
            ))))
            .await;
        // Provider reported exact token usage
        let _ = tx
            .send(Ok(ProviderStreamEvent::Usage(TokenUsage {
                uncached_prompt_tokens: 50,
                prompt_tokens: 80,
                completion_tokens: 15,
                total_tokens: 95,
                output_tokens_final: true,
                cache_read_input_tokens: Some(30),
                cache_creation_input_tokens: None,
            })))
            .await;
        // Then connection dropped
        let _ = tx.send(Err(ProviderError::StreamDisconnected)).await;
    });

    let config = StreamGuardConfig {
        keepalive_interval: Duration::from_secs(60),
        model_id: "claude-3-5-sonnet".to_string(),
        context_window: None,
    };

    let settler = BillingSettler::new(
        billing.clone(),
        inv_id.to_string(),
        "claude-3-5-sonnet".to_string(),
        "provider-1".to_string(),
        "claude-3-5-sonnet".to_string(),
    );

    let idemp_mgr = IdempotencyManager::default();
    let idemp_guard = idemp_mgr.try_acquire(inv_id).unwrap();

    let stream = create_stream_guard(
        ReceiverStream::new(rx),
        config,
        None,
        Some(idemp_guard),
        Some(settler),
    );
    let frames = collect_and_decode_frames(stream).await;

    // Both user-friendly text chunk and exception frame emitted
    assert!(frames
        .iter()
        .any(|(name, _)| name == "assistantResponseEvent"));
    assert!(frames
        .iter()
        .any(|(name, _)| name == "InternalServerException"));

    // Settled actual usage (uncached: 50, output: 15, cache_read: 30)
    let card = billing.get_card(&card_id).unwrap();
    assert_eq!(card.credit_reserved, 0);
    assert!(card.available_credits() < initial_balance);
    let ledger = billing.list_ledger_entries_for_card(&card_id, None);
    assert_eq!(ledger.len(), 1);
    assert_eq!(ledger[0].output_tokens, 15);
    assert_eq!(ledger[0].cache_read_tokens, 30);
    assert_eq!(ledger[0].input_tokens, 80); // 50 uncached + 30 cache_read

    // Marked failed in idempotency
    assert!(matches!(
        idemp_mgr.try_acquire(inv_id),
        Err(gateway::idempotency::IdempotencyError::AlreadyFailed(_))
    ));
}

// --------------------------------------------------------------------------
// 4. Clean Done stream settles and commits idempotency
// --------------------------------------------------------------------------
#[tokio::test]
async fn test_t06_clean_stream_done_settles_and_commits_idempotency() {
    let (billing, card_id) = create_test_billing();
    let initial_balance = billing.get_card(&card_id).unwrap().available_credits();

    let inv_id = "inv-clean-done";
    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let params = ReservationEstimateParams {
        estimated_input_tokens: 1000,
        max_output_tokens: 2000,
        input_rate_per_m: 15_000_000,
        output_rate_per_m: 60_000_000,
        credit_multiplier: 1.0,
        margin_multiplier: 1.0,
        model: Some("claude-3-5-sonnet".to_string()),
    };
    let _res = billing
        .reserve(&card_id, inv_id, &params, now_secs, 300)
        .unwrap();

    let (tx, rx) = mpsc::channel(16);
    tokio::spawn(async move {
        let _ = tx
            .send(Ok(ProviderStreamEvent::Delta(ProviderDelta::Text(
                "Complete response.".to_string(),
            ))))
            .await;
        let _ = tx
            .send(Ok(ProviderStreamEvent::Usage(TokenUsage {
                uncached_prompt_tokens: 100,
                prompt_tokens: 100,
                completion_tokens: 20,
                total_tokens: 120,
                output_tokens_final: true,
                cache_read_input_tokens: None,
                cache_creation_input_tokens: None,
            })))
            .await;
        let _ = tx.send(Ok(ProviderStreamEvent::Done)).await;
    });

    let config = StreamGuardConfig {
        keepalive_interval: Duration::from_secs(60),
        model_id: "claude-3-5-sonnet".to_string(),
        context_window: None,
    };

    let settler = BillingSettler::new(
        billing.clone(),
        inv_id.to_string(),
        "claude-3-5-sonnet".to_string(),
        "provider-1".to_string(),
        "claude-3-5-sonnet".to_string(),
    );

    let idemp_mgr = IdempotencyManager::default();
    let idemp_guard = idemp_mgr.try_acquire(inv_id).unwrap();

    let stream = create_stream_guard(
        ReceiverStream::new(rx),
        config,
        None,
        Some(idemp_guard),
        Some(settler),
    );
    let frames = collect_and_decode_frames(stream).await;

    // Normal termination: metadata event emitted, no exceptions
    assert!(frames.iter().any(|(name, _)| name == "metadataEvent"));
    assert!(!frames
        .iter()
        .any(|(name, _)| name == "InternalServerException"));

    // Settled
    let card = billing.get_card(&card_id).unwrap();
    assert_eq!(card.credit_reserved, 0);
    assert!(card.available_credits() < initial_balance);
    assert_eq!(
        billing.list_ledger_entries_for_card(&card_id, None).len(),
        1
    );

    // Idempotency committed!
    let completed = idemp_mgr.get_completed(inv_id).expect("Must be committed");
    assert_eq!(completed.model_id, "claude-3-5-sonnet");
    assert_eq!(completed.total_output_tokens, 20);

    // Retry with same invocation ID returns AlreadyCompleted
    assert!(matches!(
        idemp_mgr.try_acquire(inv_id),
        Err(gateway::idempotency::IdempotencyError::AlreadyCompleted(_))
    ));
}

// --------------------------------------------------------------------------
// 5. Duplicate Done events cause no duplicate settlement
// --------------------------------------------------------------------------
#[tokio::test]
async fn test_t06_duplicate_done_events_are_idempotent() {
    let (billing, card_id) = create_test_billing();

    let inv_id = "inv-dup-done";
    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let params = ReservationEstimateParams {
        estimated_input_tokens: 500,
        max_output_tokens: 500,
        input_rate_per_m: 15_000_000,
        output_rate_per_m: 60_000_000,
        credit_multiplier: 1.0,
        margin_multiplier: 1.0,
        model: Some("claude-3-5-sonnet".to_string()),
    };
    let _res = billing
        .reserve(&card_id, inv_id, &params, now_secs, 300)
        .unwrap();

    let (tx, rx) = mpsc::channel(16);
    tokio::spawn(async move {
        let _ = tx
            .send(Ok(ProviderStreamEvent::Delta(ProviderDelta::Text(
                "Hello".to_string(),
            ))))
            .await;
        let _ = tx
            .send(Ok(ProviderStreamEvent::Usage(TokenUsage {
                uncached_prompt_tokens: 50,
                prompt_tokens: 50,
                completion_tokens: 10,
                total_tokens: 60,
                output_tokens_final: true,
                cache_read_input_tokens: None,
                cache_creation_input_tokens: None,
            })))
            .await;
        let _ = tx.send(Ok(ProviderStreamEvent::Done)).await;
        // Duplicate trailing Done
        let _ = tx.send(Ok(ProviderStreamEvent::Done)).await;
    });

    let config = StreamGuardConfig {
        keepalive_interval: Duration::from_secs(60),
        model_id: "claude-3-5-sonnet".to_string(),
        context_window: None,
    };

    let settler = BillingSettler::new(
        billing.clone(),
        inv_id.to_string(),
        "claude-3-5-sonnet".to_string(),
        "provider-1".to_string(),
        "claude-3-5-sonnet".to_string(),
    );

    let stream = create_stream_guard(ReceiverStream::new(rx), config, None, None, Some(settler));
    let _frames = collect_and_decode_frames(stream).await;

    // Exactly 1 ledger entry, no double charge
    let ledger = billing.list_ledger_entries_for_card(&card_id, None);
    assert_eq!(ledger.len(), 1);
    assert_eq!(billing.get_card(&card_id).unwrap().credit_reserved, 0);
}

// --------------------------------------------------------------------------
// 6. Client disconnect midway cancels upstream and settles partial compute
// --------------------------------------------------------------------------
#[tokio::test]
async fn test_t06_client_disconnect_settles_and_aborts() {
    let (billing, card_id) = create_test_billing();
    let initial_balance = billing.get_card(&card_id).unwrap().available_credits();

    let inv_id = "inv-client-disconnect";
    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let params = ReservationEstimateParams {
        estimated_input_tokens: 1000,
        max_output_tokens: 2000,
        input_rate_per_m: 15_000_000,
        output_rate_per_m: 60_000_000,
        credit_multiplier: 1.0,
        margin_multiplier: 1.0,
        model: Some("claude-3-5-sonnet".to_string()),
    };
    let _res = billing
        .reserve(&card_id, inv_id, &params, now_secs, 300)
        .unwrap();

    let (tx, rx) = mpsc::channel(16);
    tokio::spawn(async move {
        loop {
            if tx
                .send(Ok(ProviderStreamEvent::Delta(ProviderDelta::Text(
                    "continuous chunk ".to_string(),
                ))))
                .await
                .is_err()
            {
                break; // Client dropped stream
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    });

    let config = StreamGuardConfig {
        keepalive_interval: Duration::from_secs(60),
        model_id: "claude-3-5-sonnet".to_string(),
        context_window: None,
    };

    let settler = BillingSettler::new(
        billing.clone(),
        inv_id.to_string(),
        "claude-3-5-sonnet".to_string(),
        "provider-1".to_string(),
        "claude-3-5-sonnet".to_string(),
    )
    .with_estimated_input(1000);

    let idemp_mgr = IdempotencyManager::default();
    let idemp_guard = idemp_mgr.try_acquire(inv_id).unwrap();

    let stream = create_stream_guard(
        ReceiverStream::new(rx),
        config,
        None,
        Some(idemp_guard),
        Some(settler),
    );

    // Read 2 chunks then drop receiver
    let mut boxed = Box::pin(stream);
    let _c1 = boxed.next().await;
    let _c2 = boxed.next().await;
    drop(boxed);

    // Wait for worker task to detect disconnect
    tokio::time::sleep(Duration::from_millis(60)).await;

    // Partial settlement executed
    let card = billing.get_card(&card_id).unwrap();
    assert_eq!(card.credit_reserved, 0);
    assert!(card.available_credits() < initial_balance);
    assert_eq!(
        billing.list_ledger_entries_for_card(&card_id, None).len(),
        1
    );

    // Guard marked failed (cannot be replayed or charged again)
    assert!(matches!(
        idemp_mgr.try_acquire(inv_id),
        Err(gateway::idempotency::IdempotencyError::AlreadyFailed(_))
    ));
}

// --------------------------------------------------------------------------
// 7. Cold restart recovery discards held reservations without freezing credit
// --------------------------------------------------------------------------
#[tokio::test]
async fn test_t06_cold_restart_recovers_unsettled_reservations() {
    let (billing, card_id) = create_test_billing();
    let initial_balance = billing.get_card(&card_id).unwrap().available_credits();

    // Create a reservation that never got settled (server crashed midway)
    let inv_id = "inv-unsettled-crash";
    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let params = ReservationEstimateParams {
        estimated_input_tokens: 1000,
        max_output_tokens: 2000,
        input_rate_per_m: 15_000_000,
        output_rate_per_m: 60_000_000,
        credit_multiplier: 1.0,
        margin_multiplier: 1.0,
        model: Some("claude-3-5-sonnet".to_string()),
    };
    let _res = billing
        .reserve(&card_id, inv_id, &params, now_secs, 300)
        .unwrap();

    // During in-flight, credit_reserved > 0 and available < initial
    assert!(billing.get_card(&card_id).unwrap().credit_reserved > 0);
    assert!(billing.get_card(&card_id).unwrap().available_credits() < initial_balance);

    // Simulate node reboot: export snapshot and reload it into a fresh BillingEngine
    let snap = billing.export_snapshot();
    let recovered_billing = BillingEngine::new();
    recovered_billing.import_snapshot(snap);

    // Assert: Held reservation is discarded, credit_reserved is reset to 0, available balance restored!
    let recovered_card = recovered_billing.get_card(&card_id).unwrap();
    assert_eq!(recovered_card.credit_reserved, 0);
    assert_eq!(recovered_card.available_credits(), initial_balance);
}

// --------------------------------------------------------------------------
// 8. Malformed / illegal SSE error mid-stream settles partial text & fails idempotency
// --------------------------------------------------------------------------
#[tokio::test]
async fn test_t06_malformed_sse_error_settles_and_fails_idempotency() {
    let (billing, card_id) = create_test_billing();
    let initial_balance = billing.get_card(&card_id).unwrap().available_credits();

    let inv_id = "inv-malformed-sse";
    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let params = ReservationEstimateParams {
        estimated_input_tokens: 500,
        max_output_tokens: 1000,
        input_rate_per_m: 15_000_000,
        output_rate_per_m: 60_000_000,
        credit_multiplier: 1.0,
        margin_multiplier: 1.0,
        model: Some("claude-3-5-sonnet".to_string()),
    };
    let _res = billing
        .reserve(&card_id, inv_id, &params, now_secs, 300)
        .unwrap();

    let (tx, rx) = mpsc::channel(16);
    tokio::spawn(async move {
        let _ = tx
            .send(Ok(ProviderStreamEvent::Delta(ProviderDelta::Text(
                "Good prefix before corrupted chunk: ".to_string(),
            ))))
            .await;
        // Upstream sends malformed protocol parse error
        let _ = tx
            .send(Err(ProviderError::Parse(
                "invalid json in SSE chunk".to_string(),
            )))
            .await;
    });

    let config = StreamGuardConfig {
        keepalive_interval: Duration::from_secs(60),
        model_id: "claude-3-5-sonnet".to_string(),
        context_window: None,
    };

    let settler = BillingSettler::new(
        billing.clone(),
        inv_id.to_string(),
        "claude-3-5-sonnet".to_string(),
        "provider-1".to_string(),
        "claude-3-5-sonnet".to_string(),
    )
    .with_estimated_input(500);

    let idemp_mgr = IdempotencyManager::default();
    let idemp_guard = idemp_mgr.try_acquire(inv_id).unwrap();

    let stream = create_stream_guard(
        ReceiverStream::new(rx),
        config,
        None,
        Some(idemp_guard),
        Some(settler),
    );
    let frames = collect_and_decode_frames(stream).await;

    // Both text and exception frame emitted
    assert!(frames
        .iter()
        .any(|(name, _)| name == "assistantResponseEvent"));
    assert!(frames
        .iter()
        .any(|(name, _)| name == "InternalServerException"));

    // Settled partial usage (500 estimated input, proportional output tokens)
    let card = billing.get_card(&card_id).unwrap();
    assert_eq!(card.credit_reserved, 0);
    assert!(card.available_credits() < initial_balance);
    assert_eq!(
        billing.list_ledger_entries_for_card(&card_id, None).len(),
        1
    );

    // Idempotency marked failed
    assert!(matches!(
        idemp_mgr.try_acquire(inv_id),
        Err(gateway::idempotency::IdempotencyError::AlreadyFailed(_))
    ));
}

// --------------------------------------------------------------------------
// 9. Concurrent invocation conflict (409) and settlement I/O persistence fault
// --------------------------------------------------------------------------
#[tokio::test]
async fn test_t06_concurrent_retry_conflict_and_settlement_io_fault() {
    let (billing, card_id) = create_test_billing();
    let inv_id = "inv-concurrent-conflict";
    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let params = ReservationEstimateParams {
        estimated_input_tokens: 500,
        max_output_tokens: 1000,
        input_rate_per_m: 15_000_000,
        output_rate_per_m: 60_000_000,
        credit_multiplier: 1.0,
        margin_multiplier: 1.0,
        model: Some("claude-3-5-sonnet".to_string()),
    };

    // First reservation succeeds
    let _res1 = billing
        .reserve(&card_id, inv_id, &params, now_secs, 300)
        .unwrap();

    // Concurrent second reservation with the SAME invocation ID must be rejected as duplicate
    let res2 = billing.reserve(&card_id, inv_id, &params, now_secs, 300);
    assert!(matches!(
        res2,
        Err(billing::BillingError::DuplicateInvocation(_))
    ));

    // Idempotency manager also rejects concurrent acquire
    let idemp_mgr = IdempotencyManager::default();
    let guard1 = idemp_mgr.try_acquire(inv_id).unwrap();
    let acquire2 = idemp_mgr.try_acquire(inv_id);
    assert!(matches!(
        acquire2,
        Err(gateway::idempotency::IdempotencyError::InProgress(_))
    ));

    // Test persistence fault during settlement
    billing.inject_persistence_fault(true);
    let mut settler = BillingSettler::new(
        billing.clone(),
        inv_id.to_string(),
        "claude-3-5-sonnet".to_string(),
        "provider-1".to_string(),
        "claude-3-5-sonnet".to_string(),
    );

    let usage = billing::UsageTokens {
        uncached_input_tokens: 100,
        output_tokens: 50,
        cache_read_tokens: 0,
        cache_creation_tokens: 0,
    };

    // Settle with injected fault returns persistence error
    let err = settler.settle(&usage).unwrap_err();
    assert!(err.to_string().contains("persistence"));

    // Reset fault and guard
    billing.inject_persistence_fault(false);
    guard1.fail();
}

// A normal protocol terminator is not proof that the upstream generated a response.
#[tokio::test]
async fn empty_completed_stream_releases_credits_and_allows_retry() {
    for input_usage in [false, true] {
        let (billing, card_id) = create_test_billing();
        let initial_balance = billing.get_card(&card_id).unwrap().available_credits();
        let inv_id = "empty-completed";
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let params = ReservationEstimateParams {
            estimated_input_tokens: 1000,
            max_output_tokens: 2000,
            input_rate_per_m: 15_000_000,
            output_rate_per_m: 60_000_000,
            credit_multiplier: 1.0,
            margin_multiplier: 1.0,
            model: Some("claude-3-5-sonnet".to_string()),
        };
        billing
            .reserve(&card_id, inv_id, &params, now, 300)
            .unwrap();
        let mut events = vec![Ok(ProviderStreamEvent::Delta(ProviderDelta::Text(
            String::new(),
        )))];
        events.push(Ok(ProviderStreamEvent::Delta(
            ProviderDelta::ToolCallChunk {
                index: Some(0),
                id: None,
                name: None,
                arguments: String::new(),
            },
        )));
        if input_usage {
            events.push(Ok(ProviderStreamEvent::Usage(TokenUsage {
                prompt_tokens: 1000,
                uncached_prompt_tokens: 1000,
                total_tokens: 1000,
                ..Default::default()
            })));
        }
        events.push(Ok(ProviderStreamEvent::StopReason("stop".into())));
        events.push(Ok(ProviderStreamEvent::Done));
        let manager = IdempotencyManager::default();
        let settler = BillingSettler::new(
            billing.clone(),
            inv_id.into(),
            "claude-3-5-sonnet".into(),
            "provider-1".into(),
            "claude-3-5-sonnet".into(),
        )
        .with_estimated_input(1000);
        let frames = collect_and_decode_frames(create_stream_guard(
            futures_util::stream::iter(events),
            StreamGuardConfig::default(),
            None,
            Some(manager.try_acquire(inv_id).unwrap()),
            Some(settler),
        ))
        .await;
        assert!(frames
            .iter()
            .any(|(name, _)| name == "InternalServerException"));
        assert!(!frames.iter().any(|(name, payload)| name == "metadataEvent"
            && serde_json::from_slice::<serde_json::Value>(payload).unwrap()["stopReason"]
                .is_string()));
        let card = billing.get_card(&card_id).unwrap();
        assert_eq!(card.credit_reserved, 0);
        assert_eq!(card.available_credits(), initial_balance);
        assert!(billing
            .list_ledger_entries_for_card(&card_id, None)
            .is_empty());
        assert!(manager.get_completed(inv_id).is_none());
        assert!(manager.try_acquire(inv_id).is_ok());
    }
}

#[tokio::test]
async fn completed_tool_invocation_settles_once_and_replay_is_rejected() {
    let (billing, card_id) = create_test_billing();
    let inv_id = "inv-tool-completed-replay";
    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let params = ReservationEstimateParams {
        estimated_input_tokens: 1000,
        max_output_tokens: 2000,
        input_rate_per_m: 15_000_000,
        output_rate_per_m: 60_000_000,
        credit_multiplier: 1.0,
        margin_multiplier: 1.0,
        model: Some("claude-3-5-sonnet".to_string()),
    };
    billing
        .reserve(&card_id, inv_id, &params, now_secs, 300)
        .unwrap();

    let events = vec![
        Ok(ProviderStreamEvent::Delta(ProviderDelta::ToolCallChunk {
            index: Some(0),
            id: Some("call-tool-1".to_string()),
            name: Some("read_file".to_string()),
            arguments: r#"{"path":"README.md"}"#.to_string(),
        })),
        Ok(ProviderStreamEvent::Usage(TokenUsage {
            uncached_prompt_tokens: 40,
            prompt_tokens: 40,
            completion_tokens: 12,
            total_tokens: 52,
            output_tokens_final: true,
            cache_read_input_tokens: None,
            cache_creation_input_tokens: None,
        })),
        Ok(ProviderStreamEvent::Done),
    ];
    let idemp_mgr = IdempotencyManager::default();
    let settler = BillingSettler::new(
        billing.clone(),
        inv_id.to_string(),
        "claude-3-5-sonnet".to_string(),
        "provider-1".to_string(),
        "claude-3-5-sonnet".to_string(),
    );
    let frames = collect_and_decode_frames(create_stream_guard(
        futures_util::stream::iter(events),
        StreamGuardConfig::default(),
        None,
        Some(idemp_mgr.try_acquire(inv_id).unwrap()),
        Some(settler),
    ))
    .await;

    let tool_frame = frames
        .iter()
        .find(|(name, _)| name == "toolUseEvent")
        .expect("completed tool invocation must emit a tool event");
    let tool_event: serde_json::Value = serde_json::from_slice(&tool_frame.1).unwrap();
    assert_eq!(tool_event["name"], "read_file");
    assert_eq!(tool_event["toolUseId"], "call-tool-1");
    assert_eq!(tool_event["input"], r#"{"path":"README.md"}"#);
    assert_eq!(billing.get_card(&card_id).unwrap().credit_reserved, 0);
    assert_eq!(
        billing.list_ledger_entries_for_card(&card_id, None).len(),
        1,
        "tool completion must settle exactly once"
    );
    assert!(idemp_mgr.get_completed(inv_id).is_some());
    assert!(matches!(
        idemp_mgr.try_acquire(inv_id),
        Err(gateway::idempotency::IdempotencyError::AlreadyCompleted(_))
    ));
}

// A client that stops consuming must not leave the provider, reservation, or
// concurrency permit alive forever while the worker waits on the bounded frame
// channel.  This uses a short absolute deadline so the real blocked-send path
// is exercised without waiting for the production ten-minute ceiling.
#[tokio::test]
async fn send_backpressure_deadline_drops_upstream_settles_once_and_releases_permit() {
    let (billing, card_id) = create_test_billing();
    let inv_id = "inv-send-backpressure-deadline";
    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let params = ReservationEstimateParams {
        estimated_input_tokens: 1000,
        max_output_tokens: 2000,
        input_rate_per_m: 15_000_000,
        output_rate_per_m: 60_000_000,
        credit_multiplier: 1.0,
        margin_multiplier: 1.0,
        model: Some("claude-3-5-sonnet".to_string()),
    };
    billing
        .reserve(&card_id, inv_id, &params, now_secs, 300)
        .unwrap();

    // The output channel inside the guard has capacity 64.  Queue one more
    // text event than that and never poll the returned FrameStream, forcing a
    // send to await until the injected 50ms absolute deadline.
    let (upstream_tx, upstream_rx) = mpsc::channel(128);
    for _ in 0..=64 {
        upstream_tx
            .send(Ok(ProviderStreamEvent::Delta(ProviderDelta::Text(
                "x".to_string(),
            ))))
            .await
            .unwrap();
    }

    let capacity = CapacityGuardrail::new(1, 1);
    let permit = capacity.try_acquire().unwrap();
    let idemp_mgr = IdempotencyManager::default();
    let idemp_guard = idemp_mgr.try_acquire(inv_id).unwrap();
    let settler = BillingSettler::new(
        billing.clone(),
        inv_id.to_string(),
        "claude-3-5-sonnet".to_string(),
        "provider-1".to_string(),
        "claude-3-5-sonnet".to_string(),
    )
    .with_estimated_input(1000)
    .with_permit(permit);
    let config = StreamGuardConfig {
        keepalive_interval: Duration::from_secs(60),
        model_id: "claude-3-5-sonnet".to_string(),
        context_window: None,
    };

    let stream = create_stream_guard_with_send_deadline(
        ReceiverStream::new(upstream_rx),
        config,
        None,
        Some(idemp_guard),
        Some(settler),
        Duration::from_millis(50),
    );

    // Wait until the worker has completed settlement after the blocked send.
    for _ in 0..30 {
        if billing.list_ledger_entries_for_card(&card_id, None).len() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // The receiver is dropped before settlement, so the provider-side sender
    // observes cancellation rather than remaining attached to the worker.
    assert!(upstream_tx
        .send(Ok(ProviderStreamEvent::Delta(ProviderDelta::Text(
            "after-deadline".to_string(),
        ))))
        .await
        .is_err());
    assert_eq!(
        billing.list_ledger_entries_for_card(&card_id, None).len(),
        1,
        "blocked send must settle exactly once"
    );
    assert_eq!(billing.get_card(&card_id).unwrap().credit_reserved, 0);
    assert_eq!(capacity.current_concurrency(), 0);
    assert!(matches!(
        idemp_mgr.try_acquire(inv_id),
        Err(gateway::idempotency::IdempotencyError::AlreadyFailed(_))
    ));

    // Keep the unconsumed receiver alive until all state assertions above have
    // observed the worker's terminal cleanup.
    drop(stream);
}

/// A reserved invocation, its settler, and a stream guard over `upstream` with the given
/// absolute deadline.
fn guarded_stream(
    billing: &BillingEngine,
    card_id: &str,
    inv_id: &str,
    upstream: mpsc::Receiver<Result<ProviderStreamEvent, ProviderError>>,
    deadline: Duration,
) -> gateway::stream::FrameStream {
    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let params = ReservationEstimateParams::new(1000, 2000);
    billing
        .reserve(card_id, inv_id, &params, now_secs, 300)
        .unwrap();
    let settler = BillingSettler::new(
        billing.clone(),
        inv_id.to_string(),
        "claude-3-5-sonnet".to_string(),
        "provider-1".to_string(),
        "claude-3-5-sonnet".to_string(),
    )
    .with_estimated_input(1000);
    let config = StreamGuardConfig {
        keepalive_interval: Duration::from_secs(60),
        model_id: "claude-3-5-sonnet".to_string(),
        context_window: None,
    };
    create_stream_guard_with_send_deadline(
        ReceiverStream::new(upstream),
        config,
        None,
        None,
        Some(settler),
        deadline,
    )
}

fn exception_message(frames: &[(String, Vec<u8>)]) -> Option<String> {
    let (_, payload) = frames
        .iter()
        .find(|(name, _)| name.ends_with("Exception"))?;
    let body: serde_json::Value = serde_json::from_slice(payload).ok()?;
    body["message"].as_str().map(str::to_string)
}

// Cut off by the deadline, a response must still end with a reason. The deadline used to
// break the loop silently, and every terminal frame after it was refused as late.
#[tokio::test]
async fn a_response_cut_off_by_the_deadline_ends_with_an_exception() {
    let (billing, card_id) = create_test_billing();
    let (upstream_tx, upstream_rx) = mpsc::channel(8);
    upstream_tx
        .send(Ok(ProviderStreamEvent::Delta(ProviderDelta::Text(
            "partial".into(),
        ))))
        .await
        .unwrap();
    // The upstream stays open and silent past the deadline.
    let stream = guarded_stream(
        &billing,
        &card_id,
        "inv-deadline-terminal",
        upstream_rx,
        Duration::from_millis(100),
    );
    let frames = collect_and_decode_frames(stream).await;

    let message = exception_message(&frames).expect("the stream ends with an exception");
    assert!(message.contains("time limit"), "{message}");
    assert_eq!(frames.last().unwrap().0, "InternalServerException");
    assert!(frames
        .iter()
        .any(|(_, payload)| String::from_utf8_lossy(payload).contains("截断")));
    // What was streamed is billed.
    assert_eq!(
        billing.list_ledger_entries_for_card(&card_id, None).len(),
        1
    );
    drop(upstream_tx);
}

// Tool arguments are buffered until the call completes. Past the limit the response ends
// with an exception and is billed, instead of buffering without bound or forwarding a
// truncated, malformed tool call.
#[tokio::test]
async fn an_oversized_tool_call_ends_the_response_and_is_billed() {
    for (inv_id, chunks) in [
        (
            "inv-tool-bytes",
            (0..9)
                .map(|_| (0usize, "x".repeat(1024 * 1024)))
                .collect::<Vec<_>>(),
        ),
        (
            "inv-tool-count",
            (0..129).map(|index| (index, "{}".to_string())).collect(),
        ),
    ] {
        let (billing, card_id) = create_test_billing();
        let (upstream_tx, upstream_rx) = mpsc::channel(256);
        for (index, arguments) in chunks {
            upstream_tx
                .send(Ok(ProviderStreamEvent::Delta(
                    ProviderDelta::ToolCallChunk {
                        index: Some(index),
                        id: Some(format!("call-{index}")),
                        name: Some("fsWrite".into()),
                        arguments,
                    },
                )))
                .await
                .unwrap();
        }
        upstream_tx
            .send(Ok(ProviderStreamEvent::Done))
            .await
            .unwrap();
        let stream = guarded_stream(
            &billing,
            &card_id,
            inv_id,
            upstream_rx,
            Duration::from_secs(30),
        );
        let frames = collect_and_decode_frames(stream).await;

        assert!(
            !frames.iter().any(|(name, _)| name == "toolUseEvent"),
            "{inv_id}: no tool call is forwarded"
        );
        let message = exception_message(&frames).expect("the stream ends with an exception");
        assert!(
            message.to_lowercase().contains("tool call"),
            "{inv_id}: {message}"
        );
        assert_eq!(
            billing.list_ledger_entries_for_card(&card_id, None).len(),
            1,
            "{inv_id}: billed for what was produced"
        );
    }
}

// A completed response whose settlement fails still tells the client, and does not also
// claim a normal end.
#[tokio::test]
async fn a_failed_settlement_is_reported_instead_of_a_normal_end() {
    let (billing, card_id) = create_test_billing();
    let (upstream_tx, upstream_rx) = mpsc::channel(8);
    for event in [
        ProviderStreamEvent::Delta(ProviderDelta::Text("answer".into())),
        ProviderStreamEvent::Done,
    ] {
        upstream_tx.send(Ok(event)).await.unwrap();
    }
    let stream = guarded_stream(
        &billing,
        &card_id,
        "inv-settlement-fault",
        upstream_rx,
        Duration::from_secs(30),
    );
    billing.inject_persistence_fault(true);
    let frames = collect_and_decode_frames(stream).await;
    billing.inject_persistence_fault(false);

    let message = exception_message(&frames).expect("the stream ends with an exception");
    assert!(message.contains("settlement"), "{message}");
    assert_eq!(frames.last().unwrap().0, "InternalServerException");
}

/// An Anthropic-format relay that reports one output token at the start and zero at the
/// end: merged, that was a final count of 1, and thousands of streamed words were billed
/// as one token. Only a report of exactly zero was checked against what was streamed.
/// A final count below half of what was streamed cannot be right, so the streamed
/// estimate is billed; an exact report above that still wins.
#[tokio::test]
async fn an_output_count_far_below_what_was_streamed_is_not_trusted() {
    use gateway::provider::anthropic::AnthropicProvider;
    use gateway::provider::ModelProvider;
    let words = "word ".repeat(3_600);
    let bill = |final_output: u64| {
        let words = words.clone();
        async move {
            let (billing, card_id) = create_test_billing();
            let inv_id = "inv-underreport";
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs();
            billing
                .reserve(
                    &card_id,
                    inv_id,
                    &ReservationEstimateParams::new(1_000, 8_000).with_model("claude-3-5-sonnet"),
                    now,
                    300,
                )
                .unwrap();
            let lines = [
                json_line(
                    serde_json::json!({"type": "message_start", "message": {"usage": {"input_tokens": 1000, "output_tokens": 1}}}),
                ),
                json_line(
                    serde_json::json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": words}}),
                ),
                json_line(
                    serde_json::json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": final_output}}),
                ),
                json_line(serde_json::json!({"type": "message_stop"})),
            ];
            let events: Vec<_> = lines
                .iter()
                .flat_map(|line| AnthropicProvider.parse_stream_line(line).unwrap())
                .map(Ok)
                .collect();
            let settler = BillingSettler::new(
                billing.clone(),
                inv_id.into(),
                "claude-3-5-sonnet".into(),
                "provider-1".into(),
                "claude-3-5-sonnet".into(),
            );
            collect_and_decode_frames(create_stream_guard(
                futures_util::stream::iter(events),
                StreamGuardConfig::default(),
                None,
                None,
                Some(settler),
            ))
            .await;
            let ledger = billing.list_ledger_entries_for_card(&card_id, None);
            assert_eq!(ledger.len(), 1);
            (ledger[0].input_tokens, ledger[0].output_tokens)
        }
    };
    let (input, output) = bill(0).await;
    assert_eq!(input, 1_000);
    assert!(
        output >= 3_600,
        "{output} output tokens billed for 3,600 words"
    );
    // A plausible exact count is what is billed.
    assert_eq!(bill(3_000).await, (1_000, 3_000));
}

fn json_line(value: serde_json::Value) -> String {
    format!("data: {value}")
}
