//! Isolated OAuth and runtime recovery regressions for audit A01/A02.
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use billing::{
    card::{hash_card_code, Card, CardStatus},
    engine::{BillingEngine, PendingSettlementRecovery},
    ledger::UsageTokens,
    reservation::ReservationEstimateParams,
};
use gateway::{
    auth::AuthState,
    facade::{oauth::OAuthTokenHandler, FacadeHandler},
};
use std::sync::Arc;

#[tokio::test]
async fn oauth_after_freeze_unfreeze_starts_template_timer_and_rejects_terminal_cards() {
    for status in [
        CardStatus::Unactivated,
        CardStatus::Banned,
        CardStatus::Voided,
    ] {
        let engine = Arc::new(BillingEngine::new());
        let mut card = Card::new("card", "group", 1_000_000);
        card.code_hash = hash_card_code("audit-isolated-key");
        card.activation_duration_secs = Some(3600);
        card.status = status;
        engine.upsert_card(card);
        let frozen = engine.freeze_card("card", "admin", "test", gateway::now_secs());
        if status == CardStatus::Unactivated {
            frozen.unwrap();
            engine
                .unfreeze_card("card", "admin", "test", gateway::now_secs())
                .unwrap();
        } else {
            assert!(frozen.is_err());
            assert!(engine
                .unfreeze_card("card", "admin", "test", gateway::now_secs())
                .is_err());
        }
        let auth =
            AuthState::with_billing("isolated-audit-test-secret-32-chars", (*engine).clone());
        let handler = OAuthTokenHandler::new(engine.clone(), auth);
        let request = Request::builder()
            .method("POST")
            .uri("/oauth/token")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({"card_key":"audit-isolated-key",
                "device_id":"dev_0123456789abcdef0123456789abcdef"})
                .to_string(),
            ))
            .unwrap();
        let response = handler.handle(request).await;
        let card = engine.get_card("card").unwrap();
        if status == CardStatus::Unactivated {
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(card.status, CardStatus::Active);
            assert_eq!(card.valid_until.unwrap() - card.activated_at.unwrap(), 3600);
        } else {
            assert!(!response.status().is_success());
            assert_eq!(card.status, status);
        }
    }
}

#[tokio::test]
async fn blocking_runtime_worker_recovers_pending_once_without_upstream() {
    let engine = BillingEngine::new();
    let mut card = Card::new("card", "group", 1000);
    card.activate(1, 0).unwrap();
    engine.upsert_card(card);
    engine
        .reserve("card", "use", &ReservationEstimateParams::new(0, 1), 1, 10)
        .unwrap();
    // A local unwritable persistence target leaves the consumed intent in memory.
    // No file is created: fault injection fails before filesystem IO.
    let path = std::env::temp_dir().join(format!("audit-unused-{}.json", std::process::id()));
    engine.set_persistence_path(path);
    engine.inject_persistence_fault(true);
    assert!(engine
        .settle(
            "use",
            &UsageTokens {
                output_tokens: 3,
                ..Default::default()
            },
            "m",
            "p",
            "t",
            2
        )
        .is_err());
    assert_eq!(engine.run_janitor(1000), 0);
    assert!(engine.release("use").is_err());
    // Continue as an isolated in-memory engine with the same intent.
    let isolated = BillingEngine::new();
    isolated.import_snapshot(engine.export_snapshot());
    let worker = isolated.clone();
    tokio::task::spawn_blocking(move || {
        let mut recovery = PendingSettlementRecovery::default();
        assert!(recovery.tick(&worker, 1000)[0].1.is_ok());
        assert!(recovery.tick(&worker, 1000).is_empty());
        worker.retry_pending_settlement("use").unwrap();
    })
    .await
    .unwrap();
    assert_eq!(isolated.get_card("card").unwrap().credit_used, 180);
    assert_eq!(isolated.get_card("card").unwrap().credit_reserved, 0);
    assert_eq!(isolated.ledger_entries().len(), 1);
}
