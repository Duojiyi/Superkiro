//! What a request costs when the upstream's usage report cannot be trusted.
//!
//! OpenAI-compatible relays commonly end a stream with a usage frame of zeros, and the
//! OpenAI adapter reports `cached_tokens: 0` as a present cache field. That frame used
//! to be taken as an exact report: visible output billed as nothing, and the input
//! estimate skipped because a zero cache field counted as reported input.

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

const GROUP: &str = "group-usage";
const CARD: &str = "card-usage";
const MODEL: &str = "claude-sonnet-4-6";
const ANSWER: &str = "这是一个用于验证计费的中文回答，包含若干汉字。";

async fn settle_after_zero_usage_frame() -> billing::LedgerEntry {
    let upstream = MockServer::start().await;
    let delta = json!({"id": "1", "choices": [{"delta": {"content": ANSWER}}]});
    let stop = json!({"id": "1", "choices": [{"delta": {}, "finish_reason": "stop"}]});
    let zeros = json!({"id": "1", "choices": [], "usage": {
        "prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0,
        "prompt_tokens_details": {"cached_tokens": 0}
    }});
    let sse = [
        format!("data: {delta}"),
        format!("data: {stop}"),
        format!("data: {zeros}"),
        "data: [DONE]".into(),
        String::new(),
    ]
    .join("\n\n");
    Mock::given(wm_method("POST"))
        .and(wm_path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse, "text/event-stream"))
        .mount(&upstream)
        .await;

    let billing = BillingEngine::default();
    let auth = AuthState::with_billing(
        "test-secret-key-usage-fallback-contract-32ch",
        billing.clone(),
    );
    billing.upsert_group(Group::pro_plus(GROUP, "Usage Fallback Group"));
    let template = billing::CardTemplate::monthly("tpl-usage", GROUP);
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
        "provider-u",
        "OpenAI Compatible",
        ProviderFormat::OpenAi,
        upstream.uri(),
    );
    let key = ProviderKey::new("key-u", "provider-u", "sk-test");
    billing.upsert_provider(provider.clone());
    billing.upsert_provider_key(key.clone());
    let mut mapping = ModelMap::new("mm-usage", GROUP, MODEL, "provider-u", MODEL);
    mapping.max_output = 8_192;
    mapping.context_window = 200_000;
    billing
        .publish_commercial_config(
            billing::engine::CommercialUpdate {
                expected_revision: billing.commercial_config().revision,
                reason: "usage fallback contract".into(),
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
                "conversationId": "conv-usage",
                "currentMessage": {"userInputMessage": {
                    "content": "请用中文回答一个简单的问题，并解释原因。",
                    "modelId": MODEL
                }}
            }}))
            .unwrap(),
        ))
        .unwrap();
    let response = tower::ServiceExt::oneshot(app, request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();

    // Settlement completes on the stream task; give it a moment.
    for _ in 0..200 {
        if let Some(entry) = billing
            .ledger_entries()
            .into_iter()
            .find(|entry| entry.card_id == CARD && entry.kind == billing::LedgerKind::Usage)
        {
            return entry;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the request was never settled");
}

#[tokio::test]
async fn a_usage_frame_of_zeros_does_not_make_streamed_output_free() {
    let entry = settle_after_zero_usage_frame().await;
    let answer_chars = ANSWER.chars().count() as u64;
    assert!(
        entry.output_tokens >= answer_chars,
        "streamed output billed as {} tokens; the answer was {answer_chars} CJK characters",
        entry.output_tokens
    );
    assert!(
        entry.input_tokens > 0,
        "a zero cache field is not a report of input usage"
    );
    assert!(entry.credits_charged > 0, "the request was billed as free");
}
