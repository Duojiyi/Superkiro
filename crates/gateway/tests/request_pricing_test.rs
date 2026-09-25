//! Which model, and which price, a conversation request is billed at.
//!
//! A request that named no model was priced under a name nobody publishes a price for
//! ("default-model", or the fallback model) and sent to the fallback model, so it was
//! billed at the built-in default rates whatever the group's models cost. In a group
//! without model maps, any model id was accepted as the price key while the upstream
//! always received the fallback model.

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use billing::group::{Group, ModelMap};
use billing::provider::{Provider, ProviderFormat, ProviderKey};
use billing::rate_card::{Currency, PricingMode, RateCardVersion};
use billing::BillingEngine;
use gateway::auth::AuthState;
use gateway::facade::conversation::GenerateAssistantResponseHandler;
use gateway::facade::FacadeRegistry;
use gateway::provider::governance::ProviderKeyPool;
use serde_json::{json, Value};
use std::time::Duration;
use wiremock::matchers::{method as wm_method, path as wm_path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const GROUP: &str = "group-pricing";
const CARD: &str = "card-pricing";
const PROVIDER: &str = "provider-pricing";

fn price(
    id: &str,
    model: &str,
    input_credits_per_m: i64,
    output_credits_per_m: i64,
) -> RateCardVersion {
    RateCardVersion {
        id: id.to_string(),
        rate_card_id: "default".to_string(),
        model: model.to_string(),
        currency: Currency::Cny,
        pricing_mode: PricingMode::Fixed,
        input_price_per_m: 0.0,
        output_price_per_m: 0.0,
        cache_creation_price_per_m: 0.0,
        cache_read_price_per_m: 0.0,
        fixed_input_credit_per_m: input_credits_per_m * 1_000_000,
        fixed_output_credit_per_m: output_credits_per_m * 1_000_000,
        fixed_cache_creation_credit_per_m: 0,
        fixed_cache_read_credit_per_m: 0,
        per_call_credit: 0,
        margin_multiplier: 1.0,
        effective_from_secs: 0,
    }
}

struct Outcome {
    status: StatusCode,
    body: String,
    upstream_models: Vec<String>,
    billing: BillingEngine,
}

/// One conversation turn through a pool-only handler (no static provider, as in
/// production), against an upstream that reports 100k input and 2k output tokens.
async fn run(
    models: Vec<ModelMap>,
    versions: Vec<RateCardVersion>,
    model_id: Option<&str>,
) -> Outcome {
    let upstream = MockServer::start().await;
    let sse = [
        json!({"id": "1", "choices": [{"delta": {"content": "Hello"}}]}),
        json!({"id": "1", "choices": [{"delta": {}, "finish_reason": "stop"}]}),
        json!({"id": "1", "choices": [], "usage": {"prompt_tokens": 100_000, "completion_tokens": 2_000, "total_tokens": 102_000}}),
    ]
    .iter()
    .map(|frame| format!("data: {frame}\n\n"))
    .collect::<String>()
        + "data: [DONE]\n\n";
    Mock::given(wm_method("POST"))
        .and(wm_path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse, "text/event-stream"))
        .mount(&upstream)
        .await;

    let billing = BillingEngine::default();
    let auth = AuthState::with_billing(
        "test-secret-key-request-pricing-contract-32",
        billing.clone(),
    );
    billing.upsert_group(Group::pro_plus(GROUP, "Pricing Group"));
    let template = billing::CardTemplate::monthly("tpl-pricing", GROUP);
    let mut card = billing::generate_card(&template, None, 1_700_000_000)
        .unwrap()
        .card;
    card.id = CARD.to_string();
    card.credit_total = 1_000_000_000;
    card.activate(gateway::now_secs(), 86_400 * 30).unwrap();
    billing.upsert_card(card);
    let provider = Provider::new(
        PROVIDER,
        "OpenAI Compatible",
        ProviderFormat::OpenAi,
        upstream.uri(),
    );
    let key = ProviderKey::new("key-pricing", PROVIDER, "sk-test");
    billing.upsert_provider(provider.clone());
    billing.upsert_provider_key(key.clone());
    for model in models {
        billing.upsert_model_map(model);
    }
    for version in versions {
        billing.upsert_rate_card_version(version);
    }

    let handler = GenerateAssistantResponseHandler {
        billing: billing.clone(),
        pool: Some(ProviderKeyPool::new(provider, vec![key])),
        ..Default::default()
    };
    let token = auth.issue_token_for_card(CARD, 3600).unwrap();
    let mut registry = FacadeRegistry::new();
    registry.register(handler);
    let app = registry.into_router_with_auth(auth);
    let mut message = json!({"content": "hello"});
    if let Some(model_id) = model_id {
        message["modelId"] = json!(model_id);
    }
    let request = Request::builder()
        .method(Method::POST)
        .uri("/generateAssistantResponse")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&json!({"conversationState": {
                "conversationId": "conv-pricing",
                "currentMessage": {"userInputMessage": message}
            }}))
            .unwrap(),
        ))
        .unwrap();
    let response = tower::ServiceExt::oneshot(app, request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let upstream_models = upstream
        .received_requests()
        .await
        .unwrap()
        .iter()
        .map(|request| {
            serde_json::from_slice::<Value>(&request.body).unwrap()["model"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect();
    Outcome {
        status,
        body: String::from_utf8_lossy(&bytes).to_string(),
        upstream_models,
        billing,
    }
}

async fn usage_entry(billing: &BillingEngine) -> billing::LedgerEntry {
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

fn mapped(id: &str, exposed: &str, target: &str, sort_order: i32) -> ModelMap {
    let mut model = ModelMap::new(id, GROUP, exposed, PROVIDER, target);
    model.sort_order = sort_order;
    model
}

/// A request that names no model is for the group's default model, the first its model
/// list shows: it goes to that model's target and pays that model's price.
#[tokio::test]
async fn a_request_naming_no_model_is_for_the_groups_default_model() {
    let outcome = run(
        vec![
            mapped("map-second", "second-model", "second-upstream", 1),
            mapped("map-first", "first-model", "first-upstream", 0),
        ],
        vec![
            price("v-first", "first-model", 10, 50),
            price("v-second", "second-model", 1, 1),
        ],
        None,
    )
    .await;
    assert_eq!(outcome.status, StatusCode::OK, "{}", outcome.body);
    assert_eq!(outcome.upstream_models, ["first-upstream"]);
    let entry = usage_entry(&outcome.billing).await;
    assert_eq!(entry.exposed_model, "first-model");
    assert_eq!(entry.rate_card_version.as_deref(), Some("v-first"));
    // 100k input at 10 credits per million and 2k output at 50.
    assert_eq!(entry.credits_charged, 1_100_000);
}

/// A model with no published price is refused before any upstream work, with nothing
/// held, instead of being billed at the built-in default rates.
#[tokio::test]
async fn a_model_without_a_price_is_refused_before_the_upstream() {
    let outcome = run(
        vec![mapped("map-first", "first-model", "first-upstream", 0)],
        vec![price("v-elsewhere", "priced-elsewhere", 10, 50)],
        Some("first-model"),
    )
    .await;
    assert_eq!(outcome.status, StatusCode::BAD_REQUEST, "{}", outcome.body);
    assert!(
        outcome.body.contains("ValidationException"),
        "{}",
        outcome.body
    );
    assert!(outcome.upstream_models.is_empty());
    let card = outcome.billing.get_card(CARD).unwrap();
    assert_eq!((card.credit_reserved, card.credit_used), (0, 0));
}

/// Without model maps a request is sent to the fallback model, so that is the only model
/// it may name: naming another priced it at that model while the upstream served the
/// fallback.
#[tokio::test]
async fn an_unmapped_request_is_priced_as_the_model_it_is_sent_to() {
    let fallback = "claude-3-5-sonnet-20241022";
    let prices = || {
        vec![
            price("v-fallback", fallback, 10, 50),
            price("v-dear", "claude-opus-4-6", 1, 1),
        ]
    };
    let named_other = run(vec![], prices(), Some("claude-opus-4-6")).await;
    assert_eq!(
        named_other.status,
        StatusCode::BAD_REQUEST,
        "{}",
        named_other.body
    );
    assert!(named_other.upstream_models.is_empty());

    let unnamed = run(vec![], prices(), None).await;
    assert_eq!(unnamed.status, StatusCode::OK, "{}", unnamed.body);
    assert_eq!(unnamed.upstream_models, [fallback]);
    let entry = usage_entry(&unnamed.billing).await;
    assert_eq!(entry.rate_card_version.as_deref(), Some("v-fallback"));
    assert_eq!(entry.credits_charged, 1_100_000);
}
