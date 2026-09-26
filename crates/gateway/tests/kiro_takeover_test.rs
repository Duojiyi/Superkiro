//! What Kiro does with the gateway's answers: a refusal it can act on, a model it asks for
//! by a name it never lists, attachments, and the frames it reads per turn.

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use billing::card::Card;
use billing::group::{Group, ModelMap};
use billing::provider::{Provider, ProviderFormat, ProviderKey};
use billing::rate_card::{Currency, PricingMode, RateCardVersion};
use billing::BillingEngine;
use gateway::auth::AuthClaims;
use gateway::facade::conversation::GenerateAssistantResponseHandler;
use gateway::facade::FacadeRegistry;
use gateway::provider::ProviderRuntimeRegistry;
use serde_json::{json, Value};
use tower::ServiceExt;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

const GROUP: &str = "group-takeover";
const CARD: &str = "card-takeover";

fn price(model: &str) -> RateCardVersion {
    RateCardVersion {
        id: format!("price-{model}"),
        rate_card_id: "default".to_string(),
        model: model.to_string(),
        currency: Currency::Cny,
        pricing_mode: PricingMode::Fixed,
        input_price_per_m: 0.0,
        output_price_per_m: 0.0,
        cache_creation_price_per_m: 0.0,
        cache_read_price_per_m: 0.0,
        fixed_input_credit_per_m: 1_000_000,
        fixed_output_credit_per_m: 1_000_000,
        fixed_cache_creation_credit_per_m: 0,
        fixed_cache_read_credit_per_m: 0,
        per_call_credit: 0,
        margin_multiplier: 1.0,
        effective_from_secs: 0,
    }
}

/// A short Anthropic-format answer.
fn anthropic_answer(text: &str) -> String {
    [
        json!({"type": "message_start", "message": {"usage": {"input_tokens": 10, "output_tokens": 1}}}),
        json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": text}}),
        json!({"type": "content_block_stop", "index": 0}),
        json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 3}}),
        json!({"type": "message_stop"}),
    ]
    .iter()
    .map(|event| format!("data: {event}\n\n"))
    .collect()
}

/// An upstream answering every request with `response`.
async fn upstream(response: ResponseTemplate) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(response)
        .mount(&server)
        .await;
    server
}

/// A funded card in `GROUP` whose models `maps` route to one provider at `url`.
fn engine(format: ProviderFormat, url: &str, maps: Vec<ModelMap>) -> BillingEngine {
    let billing = BillingEngine::new();
    billing.upsert_group(Group::pro_plus(GROUP, "Takeover"));
    let mut card = Card::new(CARD, GROUP, 100 * billing::MICRO_CREDITS_PER_CREDIT);
    card.activate(gateway::now_secs(), 86_400).unwrap();
    billing.upsert_card(card);
    billing.upsert_provider(Provider::new("prov", "Upstream", format, url));
    billing.upsert_provider_key(ProviderKey::new("key", "prov", "sk-test"));
    for map in maps {
        billing.upsert_rate_card_version(price(&map.exposed_model_id));
        billing.upsert_model_map(map);
    }
    billing
}

fn serve(billing: &BillingEngine) -> axum::Router {
    let runtime = ProviderRuntimeRegistry::new();
    runtime.sync_from_billing(billing);
    let mut registry = FacadeRegistry::new().with_runtime(runtime.clone());
    registry.register(GenerateAssistantResponseHandler {
        billing: billing.clone(),
        runtime: Some(runtime),
        intercept_intent: false,
        ..Default::default()
    });
    registry.into_router()
}

fn claims() -> AuthClaims {
    AuthClaims {
        card_id: CARD.into(),
        group_id: GROUP.into(),
        token_version: 0,
        exp: u64::MAX,
        iat: 0,
    }
}

/// A turn whose current message is `message`, with `history` before it.
fn body(message: Value, history: Vec<Value>) -> Value {
    json!({"conversationState": {
        "conversationId": "conv-takeover",
        "history": history,
        "currentMessage": {"userInputMessage": message}
    }})
}

struct Reply {
    status: StatusCode,
    bytes: Vec<u8>,
}

impl Reply {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.bytes).unwrap_or(Value::Null)
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.bytes).into_owned()
    }
}

async fn send(app: &axum::Router, invocation: &str, body: Value) -> Reply {
    let mut request = Request::builder()
        .method(Method::POST)
        .uri("/generateAssistantResponse")
        .header(header::CONTENT_TYPE, "application/json")
        .header("amz-sdk-invocation-id", invocation)
        .body(Body::from(body.to_string()))
        .unwrap();
    request.extensions_mut().insert(claims());
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap()
        .to_vec();
    Reply { status, bytes }
}

/// The trace of `invocation`'s refusal or routing.
fn trace_class(billing: &BillingEngine, invocation: &str) -> Vec<String> {
    let invocation_id = format!("{CARD}:{invocation}");
    billing
        .list_traces(Some(CARD), 100)
        .into_iter()
        .filter(|trace| trace.invocation_id == invocation_id)
        .filter_map(|trace| trace.error_class)
        .collect()
}

fn nothing_charged(billing: &BillingEngine) {
    let card = billing.get_card(CARD).unwrap();
    assert_eq!((card.credit_reserved, card.credit_used), (0, 0));
    assert!(billing
        .ledger_entries()
        .iter()
        .all(|entry| entry.kind != billing::LedgerKind::Usage));
}

/// Kiro compacts the conversation for a ValidationException with this reason, or one whose
/// message opens with these words, and for nothing else it can be sent.
fn assert_overflow(reply: &Reply) {
    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.text());
    let body = reply.json();
    assert_eq!(body["__type"], "ValidationException", "{body}");
    assert_eq!(body["reason"], "CONTENT_LENGTH_EXCEEDS_THRESHOLD", "{body}");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .starts_with("Input is too long: "),
        "{body}"
    );
}

#[tokio::test]
async fn an_upstream_prompt_too_long_is_the_overflow_kiro_compacts_for() {
    for (format, refusal) in [
        (
            ProviderFormat::Anthropic,
            json!({"type": "error", "error": {"type": "invalid_request_error",
                "message": "prompt is too long: 250000 tokens > 200000 maximum"}}),
        ),
        (
            ProviderFormat::OpenAi,
            json!({"error": {"code": "context_length_exceeded", "type": "invalid_request_error",
                "message": "This model's maximum context length is 128000 tokens."}}),
        ),
    ] {
        let server = upstream(ResponseTemplate::new(400).set_body_json(refusal)).await;
        let billing = engine(
            format,
            &server.uri(),
            vec![ModelMap::new("map", GROUP, "model", "prov", "up-model")],
        );
        let app = serve(&billing);
        let reply = send(
            &app,
            "inv-long",
            body(json!({"content": "hello", "modelId": "model"}), vec![]),
        )
        .await;
        assert_overflow(&reply);
        // Its message is the gateway's, never the upstream's.
        assert!(!reply.text().contains("250000") && !reply.text().contains("128000"));
        // Refused, not retried: the same prompt is as long the next time.
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
        nothing_charged(&billing);
        assert_eq!(trace_class(&billing, "inv-long"), vec!["input_too_long"]);
    }
}

#[tokio::test]
async fn a_prompt_over_the_models_limit_is_refused_before_it_is_sent() {
    let server = upstream(
        ResponseTemplate::new(200).set_body_raw(anthropic_answer("hi"), "text/event-stream"),
    )
    .await;
    let mut small = ModelMap::new("map", GROUP, "model", "prov", "up-model");
    small.context_window = 1_000;
    small.max_output = 500;
    let billing = engine(ProviderFormat::Anthropic, &server.uri(), vec![small]);
    let app = serve(&billing);

    let reply = send(
        &app,
        "inv-over",
        body(
            json!({"content": "word ".repeat(5_000), "modelId": "model"}),
            vec![],
        ),
    )
    .await;
    assert_overflow(&reply);
    assert!(server.received_requests().await.unwrap().is_empty());
    nothing_charged(&billing);
    assert_eq!(trace_class(&billing, "inv-over"), vec!["input_too_long"]);

    // One that fits is sent.
    let reply = send(
        &app,
        "inv-fits",
        body(json!({"content": "hello", "modelId": "model"}), vec![]),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
}

#[tokio::test]
async fn the_gateways_own_length_limits_are_overflows_too() {
    let server = upstream(
        ResponseTemplate::new(200).set_body_raw(anthropic_answer("hi"), "text/event-stream"),
    )
    .await;
    let billing = engine(
        ProviderFormat::Anthropic,
        &server.uri(),
        vec![ModelMap::new("map", GROUP, "model", "prov", "up-model")],
    );
    let app = serve(&billing);
    let history: Vec<Value> = (0..1_001)
        .map(|turn| {
            if turn % 2 == 0 {
                json!({"userInputMessage": {"content": "q"}})
            } else {
                json!({"assistantResponseMessage": {"content": "a"}})
            }
        })
        .collect();
    let reply = send(
        &app,
        "inv-history",
        body(json!({"content": "hello", "modelId": "model"}), history),
    )
    .await;
    assert_overflow(&reply);
    assert!(reply.json()["message"]
        .as_str()
        .unwrap()
        .contains("1001 条消息"));
    assert_eq!(trace_class(&billing, "inv-history"), vec!["input_too_long"]);

    // A bad conversation ID is not an overflow: compacting would not help.
    let mut bad_id = body(json!({"content": "hello", "modelId": "model"}), vec![]);
    bad_id["conversationState"]["conversationId"] = json!("x".repeat(257));
    let reply = send(&app, "inv-id", bad_id).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    assert_ne!(reply.json()["reason"], "CONTENT_LENGTH_EXCEEDS_THRESHOLD");
    assert!(server.received_requests().await.unwrap().is_empty());
    nothing_charged(&billing);
}

/// The upstream models `server` was asked for, in order.
async fn models_sent(server: &MockServer) -> Vec<String> {
    server
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
        .collect()
}

fn usage_entries(billing: &BillingEngine) -> Vec<billing::LedgerEntry> {
    billing
        .ledger_entries()
        .into_iter()
        .filter(|entry| entry.card_id == CARD && entry.kind == billing::LedgerKind::Usage)
        .collect()
}

fn listed(billing: &BillingEngine) -> Vec<String> {
    gateway::facade::virtualization::VirtualizationStore::with_billing(billing.clone(), GROUP)
        .get_group(Some(GROUP))
        .models
        .into_iter()
        .map(|model| model.model_id)
        .collect()
}

/// Kiro's commit messages and spec sub-intents ask for its fast model by a name no model
/// list carries. It is the model given that name, hidden or not, and is billed as it.
#[tokio::test]
async fn kiros_fast_model_is_the_model_named_for_it_and_billed_as_it() {
    let server = upstream(
        ResponseTemplate::new(200).set_body_raw(anthropic_answer("fix: typo"), "text/event-stream"),
    )
    .await;
    let main = ModelMap::new("map-main", GROUP, "main-model", "prov", "up-main");
    let mut cheap = ModelMap::new("map-cheap", GROUP, "cheap-model", "prov", "up-cheap")
        .with_alias("simple-task");
    cheap.visible = false;
    cheap.sort_order = 1;
    let billing = engine(ProviderFormat::Anthropic, &server.uri(), vec![main, cheap]);
    let app = serve(&billing);

    let reply = send(
        &app,
        "inv-fast",
        body(
            json!({"content": "Write a commit message", "modelId": "simple-task"}),
            vec![],
        ),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    assert_eq!(models_sent(&server).await, vec!["up-cheap"]);
    let usage = usage_entries(&billing);
    assert_eq!(usage.len(), 1);
    assert_eq!(usage[0].exposed_model, "cheap-model");
    assert!(usage[0].credits_charged > 0);
    assert_eq!(listed(&billing), vec!["main-model"]);
}

#[tokio::test]
async fn without_a_model_named_for_it_kiros_fast_model_is_the_group_default() {
    let server = upstream(
        ResponseTemplate::new(200).set_body_raw(anthropic_answer("fix: typo"), "text/event-stream"),
    )
    .await;
    let main = ModelMap::new("map-main", GROUP, "main-model", "prov", "up-main");
    let mut other = ModelMap::new("map-other", GROUP, "other-model", "prov", "up-other");
    other.sort_order = 1;
    // Exposed under the name itself, it is still never listed.
    let mut named = ModelMap::new("map-named", GROUP, "simple-task", "prov", "up-named");
    named.sort_order = 2;
    let billing = engine(ProviderFormat::Anthropic, &server.uri(), vec![main, other]);
    let app = serve(&billing);

    let reply = send(
        &app,
        "inv-default",
        body(
            json!({"content": "Summarize", "modelId": "simple-task"}),
            vec![],
        ),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    assert_eq!(models_sent(&server).await, vec!["up-main"]);
    assert_eq!(usage_entries(&billing)[0].exposed_model, "main-model");

    billing.upsert_rate_card_version(price("simple-task"));
    billing.upsert_model_map(named);
    assert_eq!(listed(&billing), vec!["main-model", "other-model"]);
    let reply = send(
        &app,
        "inv-named",
        body(
            json!({"content": "Summarize", "modelId": "simple-task"}),
            vec![],
        ),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    assert_eq!(models_sent(&server).await, vec!["up-main", "up-named"]);
}
