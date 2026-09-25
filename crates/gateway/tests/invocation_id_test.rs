//! The client's invocation id is stored in the billing state, so only a bounded,
//! token-like id is accepted, and a refused one leaves no trace in that state.
use axum::{
    body::Body,
    http::{header, Method, Request, StatusCode},
};
use billing::{card::Card, engine::BillingEngine};
use gateway::{
    auth::AuthClaims,
    facade::{conversation::GenerateAssistantResponseHandler, FacadeRegistry},
    idempotency::IdempotencyManager,
    provider::{anthropic::AnthropicProvider, ProviderConfig},
};
use serde_json::json;
use std::{sync::Arc, time::Duration};
use tower::ServiceExt;

mod support;

async fn send(invocation_id: &str) -> (StatusCode, BillingEngine) {
    let billing = BillingEngine::new();
    billing.upsert_rate_card_version(support::wildcard_price("default"));
    let mut card = Card::new("card", "group", 100_000_000);
    card.activate(gateway::now_secs(), 86_400).unwrap();
    billing.upsert_card(card);
    let handler = GenerateAssistantResponseHandler::new(
        reqwest::Client::new(),
        Arc::new(AnthropicProvider),
        ProviderConfig {
            // Nothing listens here: a request that got as far as the upstream fails fast.
            base_url: "http://127.0.0.1:9".into(),
            api_key: "key".into(),
            model: "claude-sonnet-4.5".into(),
            timeout: Duration::from_secs(2),
            group_id: None,
        },
        billing.clone(),
        IdempotencyManager::default(),
    );
    let mut registry = FacadeRegistry::default();
    registry.register(handler);
    let body = json!({"conversationState": {
        "conversationId": "conversation",
        "currentMessage": {"userInputMessage": {"content": "hello"}},
    }});
    let mut request = Request::builder()
        .method(Method::POST)
        .uri("/generateAssistantResponse")
        .header(header::CONTENT_TYPE, "application/json")
        .header("amz-sdk-invocation-id", invocation_id)
        .body(Body::from(body.to_string()))
        .unwrap();
    request.extensions_mut().insert(AuthClaims {
        card_id: "card".into(),
        group_id: "group".into(),
        token_version: 1,
        exp: 9_999_999_999,
        iat: 1_000_000_000,
    });
    let status = registry
        .into_router()
        .oneshot(request)
        .await
        .unwrap()
        .status();
    (status, billing)
}

#[tokio::test]
async fn an_oversized_or_malformed_invocation_id_is_refused_and_stores_nothing() {
    let huge = "a".repeat(300_000);
    for id in [huge.as_str(), "", "has space", "../../etc", "é"] {
        let (status, billing) = send(id).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "id of {} bytes", id.len());
        let state = serde_json::to_string(&billing.export_snapshot()).unwrap();
        assert!(
            state.len() < 20_000,
            "the refused id reached the state: {} bytes",
            state.len()
        );
        assert!(billing.list_traces(None, 10).is_empty());
    }
}

#[tokio::test]
async fn an_sdk_uuid_is_accepted() {
    let (status, _) = send("0f0e7b2e-3c1a-4a7e-9b1d-5c2f8a9e6d41").await;
    assert_ne!(status, StatusCode::BAD_REQUEST);
}
