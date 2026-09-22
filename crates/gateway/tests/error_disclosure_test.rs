//! What a failing upstream is allowed to tell the customer.
//!
//! The direct-provider path has always sanitized its errors, but the pool and
//! failover path formatted `GovernanceError` straight into the response body.
//! That type's `Display` embeds internal key ids and the upstream vendor's own
//! response body, and `AllCandidatesFailed` Debug-dumps every attempt at once.

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use billing::group::{Group, ModelMap};
use billing::provider::{Provider, ProviderFormat, ProviderKey};
use billing::BillingEngine;
use gateway::auth::AuthState;
use gateway::facade::conversation::GenerateAssistantResponseHandler;
use gateway::facade::FacadeRegistry;
use gateway::idempotency::IdempotencyManager;
use gateway::provider::governance::ProviderKeyPool;
use gateway::provider::openai::OpenAiProvider;
use gateway::provider::ProviderConfig;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use wiremock::matchers::{method as wm_method, path as wm_path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const GROUP: &str = "group-disclosure";
const CARD: &str = "card-disclosure";
const MODEL: &str = "claude-sonnet-4-6";
const KEY_ID: &str = "key-internal-rotation-7f3a";
const PROVIDER_ID: &str = "provider-internal-eu-west";
/// Stands in for a real vendor body: an account identifier and a billing contact.
const VENDOR_BODY: &str =
    r#"{"error":{"message":"org org-ACCT-4711 exceeded quota; contact billing@vendor.example"}}"#;

async fn failing_upstream_response() -> (StatusCode, String) {
    let upstream = MockServer::start().await;
    Mock::given(wm_method("POST"))
        .and(wm_path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(500).set_body_raw(VENDOR_BODY, "application/json"))
        .mount(&upstream)
        .await;

    let billing = BillingEngine::default();
    let auth = AuthState::with_billing(
        "test-secret-key-error-disclosure-contract-32ch",
        billing.clone(),
    );
    billing.upsert_group(Group::pro_plus(GROUP, "Disclosure Testing Group"));

    let template = billing::CardTemplate::monthly("tpl-disclosure", GROUP);
    let generated = billing::generate_card(&template, None, 1_700_000_000).unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let mut card = generated.card;
    card.id = CARD.to_string();
    card.credit_total = 1_000_000_000;
    card.activate(now, 86_400 * 30).unwrap();
    billing.upsert_card(card);

    let provider = Provider::new(
        PROVIDER_ID,
        "OpenAI Compatible",
        ProviderFormat::OpenAi,
        upstream.uri(),
    );
    let key = ProviderKey::new(KEY_ID, PROVIDER_ID, "sk-test");
    billing.upsert_provider(provider.clone());
    billing.upsert_provider_key(key.clone());

    let mut mapping = ModelMap::new("mm-disclosure", GROUP, MODEL, PROVIDER_ID, MODEL);
    mapping.max_output = 8_192;
    mapping.context_window = 200_000;
    billing
        .publish_commercial_config(
            billing::engine::CommercialUpdate {
                expected_revision: billing.commercial_config().revision,
                reason: "error disclosure contract".into(),
                settings: None,
                groups: vec![],
                models: vec![mapping],
                rate_cards: vec![],
                versions: vec![],
            },
            1_700_000_000,
        )
        .unwrap();

    let handler = GenerateAssistantResponseHandler::new(
        reqwest::Client::new(),
        Arc::new(OpenAiProvider),
        ProviderConfig::new(upstream.uri(), "sk-test", MODEL, Duration::from_secs(5)),
        billing.clone(),
        IdempotencyManager::default(),
    )
    .with_pool(ProviderKeyPool::new(provider, vec![key]));

    let token = auth.issue_token_for_card(CARD, 3600).unwrap();
    let mut registry = FacadeRegistry::new();
    registry.register(handler);
    let app = registry.into_router_with_auth(auth);

    let request = Request::builder()
        .method(Method::POST)
        .uri("/generateAssistantResponse")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&json!({"conversationState": {
                "conversationId": "conv-disclosure",
                "currentMessage": {"userInputMessage": {
                    "content": "hello",
                    "modelId": MODEL
                }}
            }}))
            .unwrap(),
        ))
        .unwrap();

    let response = tower::ServiceExt::oneshot(app, request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).to_string())
}

#[tokio::test]
async fn failover_failure_never_discloses_key_ids_provider_ids_or_vendor_bodies() {
    let (status, body) = failing_upstream_response().await;

    assert!(
        status.is_client_error() || status.is_server_error(),
        "expected a failure status, got {status}"
    );
    for secret in [
        KEY_ID,
        PROVIDER_ID,
        "org-ACCT-4711",
        "billing@vendor.example",
        "exceeded quota",
    ] {
        assert!(
            !body.contains(secret),
            "response disclosed {secret:?}\nbody: {body}"
        );
    }
    // The caller still gets something it can act on.
    assert!(
        body.contains("upstream"),
        "response should still name the upstream as the failing side: {body}"
    );
}
