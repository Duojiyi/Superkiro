use billing::card::Card;
use billing::ledger::LedgerEntry;
use gateway::ops::{
    CardRateLimiter, DbJitterBuffer, GatewayMetricsCollector, InflightConcurrencyGate,
    RateLimitError, ShutdownCoordinator,
};
use std::time::Duration;
use tokio::time::sleep;

#[test]
fn test_card_qps_rate_limiter_burst_and_recovery() {
    let limiter = CardRateLimiter::new(3);
    let card = "card-user-qps";

    // First 3 requests succeed immediately (burst = 3)
    assert!(limiter.check_and_consume(card).is_ok());
    assert!(limiter.check_and_consume(card).is_ok());
    assert!(limiter.check_and_consume(card).is_ok());

    // 4th request in same instant exceeds QPS
    let err = limiter.check_and_consume(card);
    assert_eq!(
        err,
        Err(RateLimitError::CardQpsExceeded {
            card_id: card.to_string(),
            limit: 3
        })
    );
}

#[test]
fn test_inflight_concurrency_gate_saturation_and_release() {
    let gate = InflightConcurrencyGate::new(2);
    assert_eq!(gate.current_count(), 0);

    let permit_a = gate.try_acquire().unwrap();
    let permit_b = gate.try_acquire().unwrap();
    assert_eq!(gate.current_count(), 2);

    // 3rd attempt is saturated
    let err = gate.try_acquire();
    assert!(matches!(
        err,
        Err(RateLimitError::UpstreamCapacitySaturated { current: 2, max: 2 })
    ));

    // Release slot A
    drop(permit_a);
    assert_eq!(gate.current_count(), 1);

    // Slot freed -> can acquire again
    let permit_c = gate.try_acquire().unwrap();
    assert_eq!(gate.current_count(), 2);

    drop(permit_b);
    drop(permit_c);
    assert_eq!(gate.current_count(), 0);
}

#[tokio::test]
async fn test_graceful_shutdown_tracking_and_drain() {
    let coordinator = ShutdownCoordinator::new();
    assert!(!coordinator.is_shutting_down());

    // Accept active requests
    let guard1 = coordinator.track_request().unwrap();
    let guard2 = coordinator.track_request().unwrap();
    assert_eq!(coordinator.inflight_streams(), 2);

    // Signal shutdown
    coordinator.initiate_shutdown();
    assert!(coordinator.is_shutting_down());

    // Subsequent requests are immediately rejected
    assert!(coordinator.track_request().is_none());

    // Spawn async background tasks that complete after 50ms
    tokio::spawn(async move {
        sleep(Duration::from_millis(50)).await;
        drop(guard1);
        drop(guard2);
    });

    // Clean drain within 500ms
    let drained = coordinator.wait_drain(Duration::from_millis(500)).await;
    assert!(drained);
    assert_eq!(coordinator.inflight_streams(), 0);
}

#[test]
fn test_prometheus_metrics_format_and_healthz() {
    let metrics = GatewayMetricsCollector::new();

    // Record traffic
    metrics.record_request_success(5_000_000);
    metrics.record_request_success(2_500_000);
    metrics.record_request_error();
    metrics.set_inflight(3);

    // Verify /healthz output
    let health = metrics.get_health(10, 4, 1);
    assert_eq!(health.status, "healthy");
    assert_eq!(health.inflight_requests, 3);
    assert_eq!(health.active_reservations, 10);
    assert_eq!(health.providers_healthy, 4);
    assert_eq!(health.providers_cooldown, 1);

    // Verify Prometheus text exposition format
    let prom = metrics.render_prometheus();
    assert!(prom.contains("kiro_gateway_requests_total{status=\"success\"} 2"));
    assert!(prom.contains("kiro_gateway_requests_total{status=\"error\"} 1"));
    assert!(prom.contains("kiro_gateway_inflight_requests 3"));
    assert!(prom.contains("kiro_gateway_credits_settled_total 7500000"));
}

#[test]
fn test_db_jitter_isolation_queue_and_auth_cache() {
    let jitter = DbJitterBuffer::new(5, 10);

    // Auth caching during DB drops
    let card = Card::new("card-offline-auth", "group-pro-plus", 50_000_000);
    jitter.cache_card(card.clone());

    let hit = jitter.get_cached_card("card-offline-auth").unwrap();
    assert_eq!(hit.id, "card-offline-auth");

    // Ledger enqueue during DB drops
    for i in 0..5 {
        let entry = LedgerEntry {
            id: format!("l-{i}"),
            card_id: "card-offline-auth".to_string(),
            kind: billing::ledger::LedgerKind::Usage,
            invocation_id: Some(format!("inv-{i}")),
            exposed_model: "claude-3-5".to_string(),
            provider_id: "anthropic".to_string(),
            target_model: "claude-3-5".to_string(),
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            credits_charged: 100,
            provider_cost_micro_cny: 10,
            rate_card_version: Some("v1".to_string()),
            ts_secs: 1000 + i as u64,
            operator_id: None,
            reason: None,
        };
        assert!(jitter.enqueue_ledger_entry(entry).is_ok());
    }

    assert_eq!(jitter.pending_count(), 5);

    // 6th entry exceeds capacity limit
    let entry_overflow = LedgerEntry {
        id: "l-overflow".to_string(),
        card_id: "card-offline-auth".to_string(),
        kind: billing::ledger::LedgerKind::Usage,
        invocation_id: Some("inv-overflow".to_string()),
        exposed_model: "claude-3-5".to_string(),
        provider_id: "anthropic".to_string(),
        target_model: "claude-3-5".to_string(),
        input_tokens: 0,
        output_tokens: 0,
        cache_creation_tokens: 0,
        cache_read_tokens: 0,
        credits_charged: 100,
        provider_cost_micro_cny: 10,
        rate_card_version: Some("v1".to_string()),
        ts_secs: 2000,
        operator_id: None,
        reason: None,
    };
    assert!(jitter.enqueue_ledger_entry(entry_overflow).is_err());

    // Once DB recovers, drain for replay
    let replayed = jitter.drain_pending_entries();
    assert_eq!(replayed.len(), 5);
    assert_eq!(jitter.pending_count(), 0);
}

#[tokio::test]
async fn test_healthz_facade_handler() {
    use axum::body::Body;
    use axum::http::{Method, Request, StatusCode};
    use gateway::facade::healthz::HealthzHandler;
    use gateway::facade::FacadeHandler;

    let handler = HealthzHandler::default();
    assert_eq!(handler.path(), "/healthz");
    assert_eq!(handler.method(), Method::GET);

    let req = Request::builder()
        .method(Method::GET)
        .uri("/healthz")
        .body(Body::empty())
        .unwrap();

    let resp = handler.handle(req).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .unwrap();
    let val: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(val["status"], "healthy");
    assert_eq!(val["service"], gateway::SERVICE_NAME);
    assert!(val.get("uptimeSecs").is_some());
    assert!(val.get("version").is_some());
}
