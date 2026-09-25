//! A request the client gives up on before the model's first token.
//!
//! The hold was returned only by the stream's settler, which exists once the upstream has
//! started. A client that went away while the model was still thinking (Kiro's stop, a
//! disconnect, the request timeout) left the hold for the janitor, eleven minutes later:
//! two such stops locked a two-request card out, and its retry was told it had no credit.

use axum::body::Body;
use axum::http::{header, Method, Request};
use billing::group::{Group, ModelMap};
use billing::provider::{Provider, ProviderFormat, ProviderKey};
use billing::{BillingEngine, ReservationState};
use gateway::auth::AuthState;
use gateway::facade::conversation::GenerateAssistantResponseHandler;
use gateway::facade::FacadeRegistry;
use gateway::provider::governance::ProviderKeyPool;
use serde_json::json;
use std::time::Duration;
use wiremock::matchers::method as wm_method;
use wiremock::{Mock, MockServer, ResponseTemplate};

mod support;

const GROUP: &str = "group-cancel";
const CARD: &str = "card-cancel";
const MODEL: &str = "claude-sonnet-4-6";

#[tokio::test]
async fn a_request_cancelled_before_its_first_token_returns_its_hold_at_once() {
    // The model is still thinking: nothing arrives for longer than the client waits.
    let upstream = MockServer::start().await;
    Mock::given(wm_method("POST"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(30)))
        .mount(&upstream)
        .await;

    let billing = BillingEngine::default();
    billing.upsert_rate_card_version(support::wildcard_price("default"));
    let auth = AuthState::with_billing(
        "test-secret-key-request-cancel-contract-32",
        billing.clone(),
    );
    billing.upsert_group(Group::pro_plus(GROUP, "Cancel Group"));
    let template = billing::CardTemplate::monthly("tpl-cancel", GROUP);
    let mut card = billing::generate_card(&template, None, 1_700_000_000)
        .unwrap()
        .card;
    card.id = CARD.to_string();
    card.credit_total = 1_000_000_000;
    card.max_concurrency = 2;
    card.activate(gateway::now_secs(), 86_400 * 30).unwrap();
    billing.upsert_card(card);
    let provider = Provider::new(
        "provider-cancel",
        "Slow",
        ProviderFormat::OpenAi,
        upstream.uri(),
    );
    let key = ProviderKey::new("key-cancel", "provider-cancel", "sk-test");
    billing.upsert_provider(provider.clone());
    billing.upsert_provider_key(key.clone());
    billing.upsert_model_map(ModelMap::new(
        "map-cancel",
        GROUP,
        MODEL,
        "provider-cancel",
        MODEL,
    ));

    let handler = GenerateAssistantResponseHandler {
        billing: billing.clone(),
        pool: Some(ProviderKeyPool::new(provider, vec![key])),
        ..Default::default()
    };
    let token = auth.issue_token_for_card(CARD, 3600).unwrap();
    let mut registry = FacadeRegistry::new();
    registry.register(handler);
    let app = registry.into_router_with_auth(auth);
    let request = |invocation_id: &str| {
        Request::builder()
            .method(Method::POST)
            .uri("/generateAssistantResponse")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::CONTENT_TYPE, "application/json")
            .header("amz-sdk-invocation-id", invocation_id)
            .body(Body::from(
                serde_json::to_vec(&json!({"conversationState": {
                    "conversationId": "conv-cancel",
                    "currentMessage": {"userInputMessage": {"content": "think hard", "modelId": MODEL}}
                }}))
                .unwrap(),
            ))
            .unwrap()
    };

    for invocation_id in ["cancel-0", "cancel-1"] {
        // The client gives up: the request is dropped while the upstream is starting.
        let pending = tower::ServiceExt::oneshot(app.clone(), request(invocation_id));
        assert!(
            tokio::time::timeout(Duration::from_millis(500), pending)
                .await
                .is_err(),
            "the upstream answered before the client gave up"
        );
        assert_eq!(
            billing.get_card(CARD).unwrap().credit_reserved,
            0,
            "the hold of {invocation_id} outlived its request"
        );
    }
    assert_eq!(billing.get_active_concurrency(CARD), 0);
    // A retry of a cancelled invocation is a new attempt, not a duplicate.
    let reservations = billing.export_snapshot().reservations;
    for invocation_id in ["cancel-0", "cancel-1"] {
        assert_eq!(
            reservations[&format!("{CARD}:{invocation_id}")].state,
            ReservationState::Released
        );
    }
}
