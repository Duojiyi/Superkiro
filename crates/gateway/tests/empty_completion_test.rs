//! A response that is complete but empty.
//!
//! A content filter, or a thinking budget that consumes the whole output limit, ends a
//! stream with a stop reason and a usage report but no content. That used to be treated
//! as a dropped connection: retried (tripling the vendor bill), the shared key put into
//! cooldown for every tenant, and a 502 returned. A failed relay that sends only an end
//! marker must still be treated as a failure.

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
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use wiremock::matchers::{method as wm_method, path as wm_path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const GROUP: &str = "group-empty";
const CARD: &str = "card-empty";
const MODEL: &str = "claude-sonnet-4-6";

struct Outcome {
    status: StatusCode,
    body: String,
    upstream_calls: usize,
    pool: ProviderKeyPool,
    billing: BillingEngine,
}

async fn run(frames: &[Value]) -> Outcome {
    let upstream = MockServer::start().await;
    let mut sse: Vec<String> = frames
        .iter()
        .map(|frame| format!("data: {frame}"))
        .collect();
    sse.push("data: [DONE]".into());
    sse.push(String::new());
    Mock::given(wm_method("POST"))
        .and(wm_path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(sse.join("\n\n"), "text/event-stream"),
        )
        .mount(&upstream)
        .await;

    let billing = BillingEngine::default();
    let auth = AuthState::with_billing(
        "test-secret-key-empty-completion-contract-32",
        billing.clone(),
    );
    billing.upsert_group(Group::pro_plus(GROUP, "Empty Completion Group"));
    let template = billing::CardTemplate::monthly("tpl-empty", GROUP);
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
        "provider-e",
        "OpenAI Compatible",
        ProviderFormat::OpenAi,
        upstream.uri(),
    );
    let key = ProviderKey::new("key-e", "provider-e", "sk-test");
    billing.upsert_provider(provider.clone());
    billing.upsert_provider_key(key.clone());
    let mut mapping = ModelMap::new("mm-empty", GROUP, MODEL, "provider-e", MODEL);
    mapping.max_output = 8_192;
    mapping.context_window = 200_000;
    billing
        .publish_commercial_config(
            billing::engine::CommercialUpdate {
                expected_revision: billing.commercial_config().revision,
                reason: "empty completion contract".into(),
                settings: None,
                groups: vec![],
                models: vec![mapping],
                rate_cards: vec![],
                versions: vec![],
            },
            1_700_000_000,
        )
        .unwrap();

    let pool = ProviderKeyPool::new(provider, vec![key]);
    let handler = GenerateAssistantResponseHandler::new(
        reqwest::Client::new(),
        Arc::new(OpenAiProvider),
        ProviderConfig::new(upstream.uri(), "sk-test", MODEL, Duration::from_secs(5)),
        billing.clone(),
        IdempotencyManager::default(),
    )
    .with_pool(pool.clone());
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
                "conversationId": "conv-empty",
                "currentMessage": {"userInputMessage": {"content": "hello", "modelId": MODEL}}
            }}))
            .unwrap(),
        ))
        .unwrap();
    let response = tower::ServiceExt::oneshot(app, request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    Outcome {
        status,
        body: String::from_utf8_lossy(&bytes).to_string(),
        upstream_calls: upstream.received_requests().await.unwrap().len(),
        pool,
        billing,
    }
}

#[tokio::test]
async fn a_complete_but_empty_response_is_relayed_once_and_billed() {
    let outcome = run(&[
        json!({"id": "1", "choices": [{"delta": {}, "finish_reason": "content_filter"}]}),
        json!({"id": "1", "choices": [], "usage": {"prompt_tokens": 50, "completion_tokens": 0, "total_tokens": 50}}),
    ])
    .await;
    assert_eq!(
        outcome.upstream_calls, 1,
        "a complete response must not be retried"
    );
    assert_eq!(outcome.status, StatusCode::OK);
    assert!(
        !outcome.body.contains("InternalServerException"),
        "an empty turn is not a server error: {}",
        outcome.body
    );
    assert!(
        outcome.body.contains("内容安全策略"),
        "the turn must explain itself"
    );
    for key in outcome.pool.list_keys() {
        assert!(
            key.cooldown_until.is_none(),
            "a shared key was put into cooldown"
        );
    }
    for _ in 0..200 {
        if let Some(entry) = outcome
            .billing
            .ledger_entries()
            .into_iter()
            .find(|entry| entry.card_id == CARD && entry.kind == billing::LedgerKind::Usage)
        {
            assert_eq!(
                entry.input_tokens, 50,
                "the reported usage is what gets billed"
            );
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the empty turn was never settled");
}

#[tokio::test]
async fn an_end_marker_without_a_usage_report_is_still_a_failed_relay() {
    let outcome =
        run(&[json!({"id": "1", "choices": [{"delta": {}, "finish_reason": "stop"}]})]).await;
    assert!(
        outcome.upstream_calls > 1,
        "a relay that reports nothing must be retried like any dropped stream"
    );
    assert!(outcome.body.contains("InternalServerException") || !outcome.status.is_success());
}

/// An empty turn ending with an ordinary stop gives the user nothing, and a failing relay
/// sends exactly the same thing, so it is retried and never billed. The key answered every
/// time, so it is not put into cooldown and keeps serving everyone else.
#[tokio::test]
async fn an_empty_ordinary_stop_is_retried_without_harming_the_key() {
    let outcome = run(&[
        json!({"id": "1", "choices": [{"delta": {}, "finish_reason": "stop"}]}),
        json!({"id": "1", "choices": [], "usage": {"prompt_tokens": 50, "completion_tokens": 0, "total_tokens": 50}}),
    ])
    .await;
    assert_eq!(
        outcome.upstream_calls, 3,
        "an empty ordinary stop is retried like a failed relay"
    );
    for key in outcome.pool.list_keys() {
        assert!(
            key.cooldown_until.is_none(),
            "a shared key was put into cooldown"
        );
    }
    assert!(outcome.body.contains("InternalServerException"));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        outcome
            .billing
            .ledger_entries()
            .iter()
            .all(|entry| entry.card_id != CARD),
        "an empty ordinary stop must not be billed"
    );
}
