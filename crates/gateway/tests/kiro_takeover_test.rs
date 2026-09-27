//! What Kiro does with the gateway's answers: a refusal it can act on, a model it asks for
//! by a name it never lists, attachments, and the frames it reads per turn.

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use billing::card::Card;
use billing::group::{Group, ModelMap};
use billing::provider::{Provider, ProviderFormat, ProviderKey};
use billing::rate_card::{Currency, PricingMode, RateCardVersion};
use billing::BillingEngine;
use futures_util::StreamExt;
use gateway::auth::AuthClaims;
use gateway::facade::conversation::GenerateAssistantResponseHandler;
use gateway::facade::FacadeRegistry;
use gateway::guardrail::LargeBodyGate;
use gateway::provider::ProviderRuntimeRegistry;
use serde_json::{json, Value};
use std::time::Duration;
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
        official: None,
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
    serve_gated(billing, &LargeBodyGate::default())
}

/// The conversation route, with `gate` for its large bodies.
fn serve_gated(billing: &BillingEngine, gate: &LargeBodyGate) -> axum::Router {
    let runtime = ProviderRuntimeRegistry::new();
    runtime.sync_from_billing(billing);
    let mut registry = FacadeRegistry::new().with_runtime(runtime.clone());
    registry.register(GenerateAssistantResponseHandler {
        billing: billing.clone(),
        runtime: Some(runtime),
        intercept_intent: false,
        large_bodies: gate.clone(),
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
    headers: axum::http::HeaderMap,
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
    post(app, invocation, None, Body::from(body.to_string())).await
}

const MB: usize = 1024 * 1024;

type Chunk = Result<bytes::Bytes, std::io::Error>;

/// A plain turn for `model`.
fn turn() -> String {
    body(json!({"content": "hello", "modelId": "model"}), vec![]).to_string()
}

/// `turn` padded with JSON whitespace to `size` bytes.
fn padded(turn: &str, size: usize) -> String {
    format!("{turn}{}", " ".repeat(size - turn.len()))
}

/// `turn`, then `megabytes` of JSON whitespace a megabyte at a time.
fn chunks(turn: &str, megabytes: usize) -> impl futures_util::Stream<Item = Chunk> + Send {
    let padding = bytes::Bytes::from(vec![b' '; MB]);
    futures_util::stream::iter(
        std::iter::once(bytes::Bytes::from(turn.to_string()))
            .chain(std::iter::repeat_n(padding, megabytes))
            .map(Ok),
    )
}

/// Sends `body` as a conversation, declaring `length` when given.
async fn post(app: &axum::Router, invocation: &str, length: Option<usize>, body: Body) -> Reply {
    let mut request = Request::builder()
        .method(Method::POST)
        .uri("/generateAssistantResponse")
        .header(header::CONTENT_TYPE, "application/json")
        .header("amz-sdk-invocation-id", invocation);
    if let Some(length) = length {
        request = request.header(header::CONTENT_LENGTH, length);
    }
    let mut request = request.body(body).unwrap();
    request.extensions_mut().insert(claims());
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap()
        .to_vec();
    Reply {
        status,
        headers,
        bytes,
    }
}

/// Waits, up to five seconds, until `condition` holds.
async fn until(condition: impl Fn() -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !condition() {
        assert!(std::time::Instant::now() < deadline, "condition never held");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
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

fn base64(bytes: &[u8]) -> String {
    base64::Engine::encode(&base64::engine::general_purpose::STANDARD, bytes)
}

/// A document as Kiro attaches it: the name without its extension, which is the format.
fn document(name: &str, format: &str, bytes: &[u8]) -> Value {
    json!({"name": name, "format": format, "source": {"bytes": base64(bytes)}})
}

/// The request body the upstream received last.
async fn upstream_body(server: &MockServer) -> Value {
    let requests = server.received_requests().await.unwrap();
    serde_json::from_slice(&requests.last().expect("the upstream was called").body).unwrap()
}

const PDF: &[u8] = b"%PDF-1.4 1 0 obj <</Type /Page>> endobj";

#[tokio::test]
async fn attachments_reach_the_model_as_documents_and_text() {
    for format in [ProviderFormat::Anthropic, ProviderFormat::OpenAi] {
        let server = upstream(match format {
            ProviderFormat::Anthropic => ResponseTemplate::new(200)
                .set_body_raw(anthropic_answer("read"), "text/event-stream"),
            ProviderFormat::OpenAi => ResponseTemplate::new(200).set_body_raw(
                [
                    json!({"choices": [{"delta": {"content": "read"}}]}),
                    json!({"choices": [{"delta": {}, "finish_reason": "stop"}]}),
                    json!({"choices": [], "usage": {"prompt_tokens": 10, "completion_tokens": 1, "total_tokens": 11}}),
                ]
                .iter()
                .map(|chunk| format!("data: {chunk}\n\n"))
                .collect::<String>()
                    + "data: [DONE]\n\n",
                "text/event-stream",
            ),
        })
        .await;
        let billing = engine(
            format,
            &server.uri(),
            vec![ModelMap::new("map", GROUP, "model", "prov", "up-model")],
        );
        let app = serve(&billing);
        let reply = send(
            &app,
            "inv-docs",
            body(
                json!({"content": "summarize", "modelId": "model", "documents": [
                    document("paper", "pdf", PDF),
                    document("notes", "md", "# 发布说明\nv2".as_bytes()),
                ]}),
                vec![],
            ),
        )
        .await;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
        let sent = upstream_body(&server).await;
        let last = sent["messages"].as_array().unwrap().last().unwrap().clone();
        assert_eq!(last["role"], "user");
        let parts = last["content"].as_array().unwrap();
        match format {
            ProviderFormat::Anthropic => {
                assert_eq!(parts[0]["type"], "document", "{last}");
                assert_eq!(parts[0]["source"]["media_type"], "application/pdf");
                assert_eq!(parts[0]["source"]["data"], base64(PDF));
                assert_eq!(parts[0]["title"], "paper.pdf");
            }
            ProviderFormat::OpenAi => {
                assert_eq!(parts[0]["type"], "file", "{last}");
                assert_eq!(parts[0]["file"]["filename"], "paper.pdf");
            }
        }
        assert!(parts[1]["text"]
            .as_str()
            .unwrap()
            .contains("<document name=\"notes.md\">\n# 发布说明\nv2"));
        assert_eq!(parts[2]["text"], "summarize");
    }
}

/// A message carrying nothing but a file is still the user's turn. Its file was dropped,
/// and the request ended on the assistant's turn, which current models refuse.
#[tokio::test]
async fn a_message_with_only_a_file_stays_the_users_turn() {
    let server = upstream(
        ResponseTemplate::new(200).set_body_raw(anthropic_answer("ok"), "text/event-stream"),
    )
    .await;
    let billing = engine(
        ProviderFormat::Anthropic,
        &server.uri(),
        vec![ModelMap::new("map", GROUP, "model", "prov", "up-model")],
    );
    let app = serve(&billing);
    let reply = send(
        &app,
        "inv-only-file",
        body(
            json!({"content": "", "modelId": "model",
                "documents": [document("todo", "txt", b"buy milk")]}),
            vec![
                json!({"userInputMessage": {"content": "hi"}}),
                json!({"assistantResponseMessage": {"content": "hello"}}),
            ],
        ),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    let sent = upstream_body(&server).await;
    let last = sent["messages"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(last["role"], "user", "{sent}");
    assert!(last.to_string().contains("buy milk"), "{last}");
}

#[tokio::test]
async fn an_attachment_no_upstream_reads_is_refused_naming_it() {
    let server = upstream(
        ResponseTemplate::new(200).set_body_raw(anthropic_answer("ok"), "text/event-stream"),
    )
    .await;
    let mut text_only = ModelMap::new("map-text", GROUP, "text-model", "prov", "up-text");
    text_only.supports_vision = false;
    let billing = engine(
        ProviderFormat::Anthropic,
        &server.uri(),
        vec![
            ModelMap::new("map", GROUP, "model", "prov", "up-model"),
            text_only,
        ],
    );
    let app = serve(&billing);
    for (invocation, model, attached, named) in [
        (
            "inv-docx",
            "model",
            document("report", "docx", b"PK\x03\x04"),
            ".docx",
        ),
        (
            "inv-pdf-text",
            "text-model",
            document("paper", "pdf", PDF),
            "paper.pdf",
        ),
    ] {
        let reply = send(
            &app,
            invocation,
            body(
                json!({"content": "read it", "modelId": model, "documents": [attached]}),
                vec![],
            ),
        )
        .await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.text());
        let refusal = reply.json();
        assert_eq!(refusal["__type"], "ValidationException");
        assert_eq!(refusal["reason"], "DOCUMENT_MODEL_NOT_SUPPORTED");
        assert!(
            refusal["message"].as_str().unwrap().contains(named),
            "{refusal}"
        );
        assert_eq!(
            trace_class(&billing, invocation),
            vec!["unsupported_capability"]
        );
    }
    assert!(server.received_requests().await.unwrap().is_empty());
    nothing_charged(&billing);

    // Attached to an earlier message, it becomes a note and the conversation goes on.
    let reply = send(
        &app,
        "inv-docx-history",
        body(
            json!({"content": "and now?", "modelId": "model"}),
            vec![
                json!({"userInputMessage": {"content": "read it",
                    "documents": [document("report", "docx", b"PK\x03\x04")]}}),
                json!({"assistantResponseMessage": {"content": "I cannot."}}),
            ],
        ),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    let sent = upstream_body(&server).await.to_string();
    assert!(sent.contains("report.docx"), "{sent}");
    assert!(!sent.contains(&base64(b"PK\x03\x04")), "{sent}");
}

/// The frames of an event-stream body, as (event type, JSON payload).
fn frames(bytes: &[u8]) -> Vec<(String, Value)> {
    let mut decoder = kiro_wire::decoder::EventStreamDecoder::new();
    decoder.feed(bytes).unwrap();
    let (frames, _) = decoder.decode_all();
    frames
        .iter()
        .map(|frame| {
            (
                frame
                    .event_type()
                    .or_else(|| frame.exception_type())
                    .unwrap_or_default()
                    .to_string(),
                frame.payload_as_json().unwrap_or(Value::Null),
            )
        })
        .collect()
}

/// Kiro shows a thinking summary as it streams and keeps the thinking in its history only
/// with a signature, which it sends back on the next turn.
#[tokio::test]
async fn thinking_streams_with_the_signature_of_the_model_that_wrote_it() {
    let events = [
        json!({"type": "message_start", "message": {"usage": {"input_tokens": 10, "output_tokens": 1}}}),
        json!({"type": "content_block_start", "index": 0, "content_block": {"type": "thinking", "thinking": ""}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": "Checking the file."}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "signature_delta", "signature": "EqQBCgIYAh"}}),
        json!({"type": "content_block_stop", "index": 0}),
        json!({"type": "content_block_start", "index": 1, "content_block": {"type": "text", "text": ""}}),
        json!({"type": "content_block_delta", "index": 1, "delta": {"type": "text_delta", "text": "Done."}}),
        json!({"type": "content_block_stop", "index": 1}),
        json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 9}}),
        json!({"type": "message_stop"}),
    ]
    .iter()
    .map(|event| format!("data: {event}\n\n"))
    .collect::<String>();
    let server =
        upstream(ResponseTemplate::new(200).set_body_raw(events, "text/event-stream")).await;
    let billing = engine(
        ProviderFormat::Anthropic,
        &server.uri(),
        vec![ModelMap::new("map", GROUP, "model", "prov", "up-model")],
    );
    let app = serve(&billing);
    let reply = send(
        &app,
        "inv-thinking",
        body(json!({"content": "check it", "modelId": "model"}), vec![]),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK);
    let reasoning: Vec<Value> = frames(&reply.bytes)
        .into_iter()
        .filter(|(kind, _)| kind == "reasoningContentEvent")
        .map(|(_, payload)| payload)
        .collect();
    assert_eq!(reasoning[0]["text"], "Checking the file.");
    assert_eq!(reasoning[1]["signature"], "up-model#EqQBCgIYAh");
    // Before the answer's text, so Kiro seals the thinking with its signature.
    let kinds: Vec<String> = frames(&reply.bytes)
        .into_iter()
        .map(|(kind, _)| kind)
        .collect();
    let signed = kinds
        .iter()
        .rposition(|kind| kind == "reasoningContentEvent")
        .unwrap();
    let answered = kinds
        .iter()
        .position(|kind| kind == "assistantResponseEvent")
        .unwrap();
    assert!(signed < answered, "{kinds:?}");

    // Sent back, it is replayed only where the operator switched replay on: off by default.
    let reply = send(
        &app,
        "inv-thinking-2",
        body(
            json!({"content": "and now?", "modelId": "model"}),
            vec![
                json!({"userInputMessage": {"content": "check it"}}),
                json!({"assistantResponseMessage": {"content": "Done.", "reasoningContent": {
                    "reasoningText": {"text": "Checking the file.", "signature": "up-model#EqQBCgIYAh"}
                }}}),
            ],
        ),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    let sent = upstream_body(&server).await;
    assert!(
        !sent.to_string().contains("\"thinking\":\"Checking"),
        "{sent}"
    );
}

/// An upstream's refusal of the request itself is a ValidationException, which Kiro shows
/// as it is and does not retry; as a 502 it read "temporary error" and was retried
/// unchanged. Its words are the gateway's, never the upstream's.
#[tokio::test]
async fn an_upstream_refusal_is_shown_and_not_retried() {
    let server = upstream(ResponseTemplate::new(400).set_body_json(json!({
        "type": "error",
        "error": {"type": "invalid_request_error", "message": "tools.0: secret detail"}
    })))
    .await;
    let billing = engine(
        ProviderFormat::Anthropic,
        &server.uri(),
        vec![ModelMap::new("map", GROUP, "model", "prov", "up-model")],
    );
    let app = serve(&billing);
    let reply = send(
        &app,
        "inv-refused",
        body(json!({"content": "hello", "modelId": "model"}), vec![]),
    )
    .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.text());
    let refusal = reply.json();
    assert_eq!(refusal["__type"], "ValidationException");
    assert!(refusal["message"].as_str().unwrap().contains("HTTP 400"));
    assert!(!reply.text().contains("secret detail"));
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
    nothing_charged(&billing);
    assert_eq!(reply.headers["x-amzn-requestid"], "inv-refused");
}

/// A refusal ends the turn as Kiro's `content_filtered`, with its category, so Kiro shows
/// its refusal instead of an ordinary end.
#[tokio::test]
async fn a_refusal_ends_as_content_filtered_with_its_category() {
    let events = [
        json!({"type": "message_start", "message": {"usage": {"input_tokens": 10, "output_tokens": 1}}}),
        json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "I can't help with that."}}),
        json!({"type": "content_block_stop", "index": 0}),
        json!({"type": "message_delta", "delta": {"stop_reason": "refusal",
            "stop_details": {"type": "refusal", "category": "cyber", "explanation": "Declined."}},
            "usage": {"output_tokens": 7}}),
        json!({"type": "message_stop"}),
    ]
    .iter()
    .map(|event| format!("data: {event}\n\n"))
    .collect::<String>();
    let server =
        upstream(ResponseTemplate::new(200).set_body_raw(events, "text/event-stream")).await;
    let billing = engine(
        ProviderFormat::Anthropic,
        &server.uri(),
        vec![ModelMap::new("map", GROUP, "model", "prov", "up-model")],
    );
    let app = serve(&billing);
    let reply = send(
        &app,
        "inv-refusal",
        body(json!({"content": "hack it", "modelId": "model"}), vec![]),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.headers["x-amzn-requestid"], "inv-refusal");
    let end = frames(&reply.bytes)
        .into_iter()
        .rev()
        .find(|(kind, payload)| kind == "metadataEvent" && payload.get("stopReason").is_some())
        .map(|(_, payload)| payload)
        .unwrap();
    assert_eq!(end["stopReason"], "content_filtered");
    assert_eq!(end["stopDetails"]["refusal"]["category"], "cyber");
    assert_eq!(end["stopDetails"]["refusal"]["explanation"], "Declined.");
}

/// Kiro shows each prompt's credits from a meteringEvent; none was sent, so its usage
/// summary was always empty. It comes once the turn is billed, before the final metadata.
#[tokio::test]
async fn a_billed_turn_reports_its_credits_before_it_ends() {
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
    let reply = send(
        &app,
        "inv-metering",
        body(json!({"content": "hello", "modelId": "model"}), vec![]),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK);
    let frames = frames(&reply.bytes);
    let metering = frames
        .iter()
        .position(|(kind, _)| kind == "meteringEvent")
        .expect("a meteringEvent");
    let charged = usage_entries(&billing)[0].credits_charged;
    assert!(charged > 0);
    let event = &frames[metering].1;
    // Kiro's own unit: it adds a turn's usage up by it, and its telemetry knows only it.
    assert_eq!(event["unit"], "credit");
    assert_eq!(event["unitPlural"], "Credits");
    assert!(
        (event["usage"].as_f64().unwrap() - charged as f64 / 1_000_000.0).abs() < 1e-9,
        "{event}"
    );
    let end = frames
        .iter()
        .rposition(|(kind, payload)| kind == "metadataEvent" && payload.get("stopReason").is_some())
        .unwrap();
    assert!(metering < end, "{frames:?}");
}

/// Kiro posts every line of its session transcripts to the takeover's endpoint every three
/// seconds and has no setting that stops it. The gateway takes the batch without reading
/// it, and only from a signed-in client.
#[tokio::test]
async fn kiros_activity_log_is_accepted_and_never_kept() {
    let batch = json!({"payload": [{"sessionId": "s", "activityType": "text",
        "content": {"type": "user", "content": "private source code"}}]})
    .to_string();
    let post = |app: axum::Router, token: Option<String>| {
        let batch = batch.clone();
        async move {
            let mut request = Request::builder()
                .method(Method::POST)
                .uri("/agents/activity")
                .header(header::CONTENT_TYPE, "application/json");
            if let Some(token) = token {
                request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
            }
            app.oneshot(request.body(Body::from(batch)).unwrap())
                .await
                .unwrap()
        }
    };
    let response = post(FacadeRegistry::default().into_router(), None).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert!(bytes.is_empty());

    let secured = FacadeRegistry::default().into_router_with_auth(gateway::auth::AuthState::new(
        "activity-test-secret-32-characters-long",
    ));
    assert_eq!(post(secured, None).await.status(), StatusCode::UNAUTHORIZED);
}

/// Prepaid credits never reset: the usage panel said "resets in 30 days" for every card.
/// It now gives the card's expiry, or nothing for a card that never expires.
#[tokio::test]
async fn usage_reports_the_cards_expiry_not_a_monthly_reset() {
    let billing = BillingEngine::new();
    billing.upsert_group(Group::pro_plus(GROUP, "Takeover"));
    let now = gateway::now_secs();
    let mut expiring = Card::new("card-expiring", GROUP, billing::MICRO_CREDITS_PER_CREDIT);
    expiring.activate(now, 10 * 86_400).unwrap();
    let until = expiring.valid_until.unwrap();
    billing.upsert_card(expiring);
    let mut lasting = Card::new("card-lasting", GROUP, billing::MICRO_CREDITS_PER_CREDIT);
    lasting.activate(now, 86_400).unwrap();
    lasting.valid_until = None;
    billing.upsert_card(lasting);

    let mut registry = FacadeRegistry::new();
    registry.register(gateway::facade::usage::GetUsageLimitsHandler::new(
        gateway::facade::virtualization::VirtualizationStore::with_billing(billing.clone(), GROUP),
    ));
    let app = registry.into_router();
    let usage = |card: &'static str| {
        let app = app.clone();
        async move {
            let mut request = Request::builder()
                .uri("/getUsageLimits")
                .body(Body::empty())
                .unwrap();
            request.extensions_mut().insert(AuthClaims {
                card_id: card.into(),
                group_id: GROUP.into(),
                token_version: 0,
                exp: u64::MAX,
                iat: 0,
            });
            let response = app.oneshot(request).await.unwrap();
            let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            serde_json::from_slice::<Value>(&bytes).unwrap()
        }
    };
    let expiring = usage("card-expiring").await;
    assert_eq!(expiring["nextDateReset"], until);
    assert_eq!(expiring["daysUntilReset"], 10);
    assert_eq!(expiring["usageBreakdownList"][0]["nextDateReset"], until);
    let lasting = usage("card-lasting").await;
    assert!(lasting.get("nextDateReset").is_none(), "{lasting}");
    assert!(lasting.get("daysUntilReset").is_none());
    assert!(lasting["usageBreakdownList"][0]
        .get("nextDateReset")
        .is_none());
}

/// Kiro sends every image in a conversation again with each turn, so a long session with
/// screenshots outgrew the 10 MB the gateway read, and then every turn failed with
/// "Failed to read request body". Up to 32 MB is read now; a larger body, whether its
/// length is declared or found while reading, is the overflow Kiro compacts for.
#[tokio::test]
async fn a_large_conversation_is_read_and_a_larger_one_is_compacted() {
    let server = upstream(
        ResponseTemplate::new(200).set_body_raw(anthropic_answer("hi"), "text/event-stream"),
    )
    .await;
    let billing = engine(
        ProviderFormat::Anthropic,
        &server.uri(),
        vec![ModelMap::new("map", GROUP, "model", "prov", "up-model")],
    );
    // As served, with the router-wide default limit, which no facade handler reads.
    let app = serve(&billing).layer(axum::extract::DefaultBodyLimit::max(10 * 1024 * 1024));
    let turn = turn();

    // 12 MB, padded with JSON whitespace.
    let large = padded(&turn, 12 * MB);
    let reply = post(&app, "inv-large", Some(large.len()), Body::from(large)).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
    assert_eq!(usage_entries(&billing).len(), 1);

    // Declared over the limit: refused before it is read.
    let reply = post(
        &app,
        "inv-declared",
        Some(33 * MB),
        Body::from(turn.clone()),
    )
    .await;
    assert_overflow(&reply);
    assert!(reply.text().contains("32 MB"), "{}", reply.text());
    assert_eq!(reply.headers["x-amzn-requestid"], "inv-declared");

    // Sent without a length, and found over it while reading.
    let reply = post(
        &app,
        "inv-streamed",
        None,
        Body::from_stream(chunks(&turn, 33)),
    )
    .await;
    assert_overflow(&reply);

    assert_eq!(server.received_requests().await.unwrap().len(), 1);
    assert_eq!(usage_entries(&billing).len(), 1);
    assert_eq!(
        trace_class(&billing, "inv-declared"),
        vec!["input_too_long"]
    );
    assert_eq!(
        trace_class(&billing, "inv-streamed"),
        vec!["input_too_long"]
    );
}

/// Held about three times over until it is translated, a body at the 32 MB limit comes to
/// about 100 MB, and the gateway runs in 1 GB. Bodies over 10 MB are read and processed two
/// at a time across the gateway: a third waits briefly for a place, then is throttled, which
/// Kiro retries. Smaller bodies never wait.
#[tokio::test]
async fn a_third_large_conversation_at_once_is_throttled_and_small_ones_are_not() {
    let server = upstream(
        ResponseTemplate::new(200).set_body_raw(anthropic_answer("hi"), "text/event-stream"),
    )
    .await;
    let billing = engine(
        ProviderFormat::Anthropic,
        &server.uri(),
        vec![ModelMap::new("map", GROUP, "model", "prov", "up-model")],
    );
    let gate = LargeBodyGate::new(2, 10 * MB, Duration::from_millis(300));
    let app = serve_gated(&billing, &gate);
    let turn = turn();

    // One large by the length it declares, holding its place before any of it arrives...
    let (release_declared, held) = tokio::sync::oneshot::channel::<()>();
    let rest = bytes::Bytes::from(padded("", 12 * MB - turn.len()));
    let body = futures_util::stream::iter([Chunk::Ok(bytes::Bytes::from(turn.clone()))]).chain(
        futures_util::stream::once(async move {
            let _ = held.await;
            Chunk::Ok(rest)
        }),
    );
    let declared = tokio::spawn({
        let app = app.clone();
        async move { post(&app, "inv-declared", Some(12 * MB), Body::from_stream(body)).await }
    });
    // ...and one sent without a length, holding its place once 10 MB of it has arrived.
    let (release_streamed, held) = tokio::sync::oneshot::channel::<()>();
    let body = chunks(&turn, 11).chain(futures_util::stream::once(async move {
        let _ = held.await;
        Ok(bytes::Bytes::new())
    }));
    let streamed = tokio::spawn({
        let app = app.clone();
        async move { post(&app, "inv-streamed", None, Body::from_stream(body)).await }
    });
    until(|| gate.in_use() == 2).await;

    // A third waits for a place, then is throttled in the form Kiro retries...
    let third = padded(&turn, 12 * MB);
    let started = std::time::Instant::now();
    let reply = post(
        &app,
        "inv-third",
        Some(third.len()),
        Body::from(third.clone()),
    )
    .await;
    assert!(started.elapsed() >= Duration::from_millis(300));
    assert_eq!(
        reply.status,
        StatusCode::TOO_MANY_REQUESTS,
        "{}",
        reply.text()
    );
    assert_eq!(reply.json()["__type"], "ThrottlingException");
    assert_eq!(reply.json()["reason"], "LARGE_REQUEST_CAPACITY");
    assert_eq!(reply.headers[header::RETRY_AFTER], "2");
    assert_eq!(reply.headers["x-amzn-requestid"], "inv-third");
    // ...while a small one goes straight through.
    let started = std::time::Instant::now();
    let reply = post(&app, "inv-small", None, Body::from(turn.clone())).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    assert!(started.elapsed() < Duration::from_millis(300));
    assert_eq!(gate.in_use(), 2);

    release_declared.send(()).unwrap();
    release_streamed.send(()).unwrap();
    assert_eq!(declared.await.unwrap().status, StatusCode::OK);
    assert_eq!(streamed.await.unwrap().status, StatusCode::OK);
    assert_eq!(gate.in_use(), 0);
    // Kiro's retry of the third finds a place.
    let reply = post(&app, "inv-third", Some(third.len()), Body::from(third)).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    assert_eq!(gate.in_use(), 0);
    assert_eq!(server.received_requests().await.unwrap().len(), 4);
}

/// A large body's place comes back however its request ends: refused, broken off while the
/// body arrives, dropped when the client goes away, or served.
#[tokio::test]
async fn a_large_conversations_place_is_given_back_on_every_path() {
    let server = upstream(
        ResponseTemplate::new(200).set_body_raw(anthropic_answer("hi"), "text/event-stream"),
    )
    .await;
    let billing = engine(
        ProviderFormat::Anthropic,
        &server.uri(),
        vec![ModelMap::new("map", GROUP, "model", "prov", "up-model")],
    );
    let gate = LargeBodyGate::new(2, 10 * MB, Duration::from_millis(300));
    let app = serve_gated(&billing, &gate);
    let turn = turn();

    // Not a conversation.
    let junk = "x".repeat(12 * MB);
    let reply = post(&app, "inv-junk", Some(junk.len()), Body::from(junk)).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.text());
    assert_eq!(reply.json()["__type"], "SerializationException");
    assert_eq!(gate.in_use(), 0);

    // Past the limit after it took its place.
    let reply = post(&app, "inv-over", None, Body::from_stream(chunks(&turn, 33))).await;
    assert_overflow(&reply);
    assert_eq!(gate.in_use(), 0);

    // The connection breaks while the body arrives.
    let broken = chunks(&turn, 11).chain(futures_util::stream::once(async {
        Err(std::io::Error::new(
            std::io::ErrorKind::ConnectionReset,
            "reset",
        ))
    }));
    let reply = post(&app, "inv-broken", None, Body::from_stream(broken)).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.text());
    assert_eq!(gate.in_use(), 0);

    // The client goes away mid-body, and the server drops the request.
    let stalled = chunks(&turn, 11).chain(futures_util::stream::pending());
    let request = tokio::spawn({
        let app = app.clone();
        async move { post(&app, "inv-gone", None, Body::from_stream(stalled)).await }
    });
    until(|| gate.in_use() == 1).await;
    request.abort();
    assert!(matches!(request.await, Err(error) if error.is_cancelled()));
    assert_eq!(gate.in_use(), 0);

    // Served.
    let served = padded(&turn, 12 * MB);
    let reply = post(&app, "inv-served", Some(served.len()), Body::from(served)).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    assert_eq!(gate.in_use(), 0);
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

/// Kiro's calls outside the conversation, at the paths and methods it uses, answered in
/// forms it acts on or shows instead of the 404 fallback.
#[tokio::test]
async fn kiros_other_calls_are_answered_at_its_own_paths() {
    let auth = gateway::auth::AuthState::with_default_dev_card();
    let token = auth
        .issue_token("card-dev-001", "group-pro-plus", 1, 3600)
        .unwrap();
    let app = FacadeRegistry::default().into_router_with_auth(auth);
    let call = |method: Method, path: &'static str, signed_in: bool, body: &'static str| {
        let app = app.clone();
        let token = token.clone();
        async move {
            let mut request = Request::builder()
                .method(method)
                .uri(path)
                .header(header::CONTENT_TYPE, "application/json");
            if signed_in {
                request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
            }
            let response = app
                .oneshot(request.body(Body::from(body)).unwrap())
                .await
                .unwrap();
            let status = response.status();
            let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            (
                status,
                serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null),
            )
        }
    };

    // Autocomplete, which Kiro posts in lower case: no suggestions, and no error popup.
    let (status, body) = call(Method::POST, "/generatecompletions", true, "{}").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["completions"], json!([]));
    // Sign-out comes after Kiro has dropped its tokens, so it carries none.
    let (status, _) = call(Method::POST, "/logout", false, r#"{"refreshToken":"r"}"#).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    // Kiro shows a 4xx's message after "Failed to delete account: ".
    let (status, body) = call(Method::DELETE, "/account", true, "").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["message"].as_str().unwrap().contains("卡密"), "{body}");
    // The overage toggle: "Unable to enable overages: {message}".
    let (status, body) = call(
        Method::POST,
        "/setUserPreference",
        true,
        r#"{"overageConfiguration":{"overageStatus":"ENABLED"}}"#,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["__type"], "ValidationException");
    assert!(
        body["message"].as_str().unwrap().contains("预付费"),
        "{body}"
    );
}

/// 1x1 PNG.
const PNG_1X1: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

/// The gateway's own refusals carry the reasons Kiro's service gives the same refusals:
/// Kiro shows an image one as an image error with its detail. InvalidRequestException read
/// "Something went wrong: [InvalidRequestException] ...".
#[tokio::test]
async fn the_gateways_own_refusals_carry_the_reasons_kiro_knows() {
    let server = upstream(ResponseTemplate::new(200)).await;
    let billing = engine(
        ProviderFormat::Anthropic,
        &server.uri(),
        vec![ModelMap::new("map", GROUP, "model", "prov", "up")],
    );
    let app = serve(&billing);
    let image = json!({"format": "png", "source": {"bytes": PNG_1X1}});
    let refusal = |reply: &Reply| {
        assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.text());
        let body = reply.json();
        assert_eq!(body["__type"], "ValidationException", "{body}");
        (
            body["reason"].as_str().unwrap_or_default().to_string(),
            body["message"].as_str().unwrap_or_default().to_string(),
        )
    };

    let images = vec![image.clone(); 21];
    let reply = send(
        &app,
        "inv-images",
        body(
            json!({"content": "look", "modelId": "model", "images": images}),
            vec![],
        ),
    )
    .await;
    let (reason, message) = refusal(&reply);
    assert_eq!(reason, "IMAGE_COUNT_EXCEEDED");
    assert!(
        message.contains("21") && message.contains("20"),
        "{message}"
    );

    let broken = json!({"format": "png", "source": {"bytes": base64(b"not an image")}});
    let reply = send(
        &app,
        "inv-broken",
        body(
            json!({"content": "look", "modelId": "model", "images": [broken]}),
            vec![],
        ),
    )
    .await;
    assert_eq!(refusal(&reply).0, "IMAGE_FORMAT_UNSUPPORTED");

    let mut empty_id = body(json!({"content": "hi", "modelId": "model"}), vec![]);
    empty_id["conversationState"]["conversationId"] = json!("");
    let reply = send(&app, "inv-conversation", empty_id).await;
    assert_eq!(refusal(&reply).0, "INVALID_CONVERSATION_ID");

    let reply = send(
        &app,
        "inv-model",
        body(json!({"content": "hi", "modelId": "bad model!"}), vec![]),
    )
    .await;
    assert_eq!(refusal(&reply).0, "INVALID_MODEL_ID");
    nothing_charged(&billing);
}

/// With every upstream disabled nothing can serve the request until the operator acts:
/// ServiceUnavailableException read "Too many requests, please wait" and was retried.
#[tokio::test]
async fn no_enabled_upstream_is_said_plainly_and_not_retried() {
    let server = upstream(ResponseTemplate::new(200)).await;
    let billing = engine(
        ProviderFormat::Anthropic,
        &server.uri(),
        vec![ModelMap::new("map", GROUP, "model", "prov", "up")],
    );
    let mut provider = billing.get_provider("prov").unwrap();
    provider.enabled = false;
    billing.upsert_provider(provider);
    let app = serve(&billing);

    let reply = send(
        &app,
        "inv-no-upstream",
        body(json!({"content": "hi", "modelId": "model"}), vec![]),
    )
    .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.text());
    assert_eq!(reply.json()["__type"], "ValidationException");
    assert!(reply.json()["message"].as_str().unwrap().contains("上游"));
    nothing_charged(&billing);
}

/// Kiro sends a turn again when it ends without a stop reason, taking it for cut short.
#[tokio::test]
async fn the_intent_answer_ends_with_a_stop_reason() {
    let mut registry = FacadeRegistry::new();
    registry.register(GenerateAssistantResponseHandler::default());
    let app = registry.into_router();
    let classifier = "You are an intent classifier for a language model. Return ONLY a JSON object with 3 properties (chat, do, spec).";
    let reply = send(
        &app,
        "inv-intent",
        json!({"systemPrompt": classifier, "conversationState": {"conversationId": "c",
            "history": [], "currentMessage": {"userInputMessage": {"content": "fix the bug"}}}}),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK);
    let frames = frames(&reply.bytes);
    let (kind, payload) = frames.last().unwrap();
    assert_eq!(kind, "metadataEvent", "{frames:?}");
    assert_eq!(payload["stopReason"], "end_turn");
    assert!(frames
        .iter()
        .any(|(kind, payload)| kind == "assistantResponseEvent"
            && payload["content"]
                .as_str()
                .unwrap_or_default()
                .contains("\"do\"")));
}

/// A body sent a chunk at a time, counting the chunks the server has read.
fn counted_body(
    prefix: &str,
    chunks: usize,
    read: std::sync::Arc<std::sync::atomic::AtomicUsize>,
) -> Body {
    let first = bytes::Bytes::from(prefix.to_string());
    let rest = std::iter::repeat_n(bytes::Bytes::from(vec![b' '; 64 * 1024]), chunks);
    Body::from_stream(
        futures_util::stream::iter(std::iter::once(first).chain(rest)).map(move |chunk| {
            read.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok::<_, std::io::Error>(chunk)
        }),
    )
}

/// A refusal answered while Kiro is still sending its body can reach it as a connection
/// reset, which it reports as a network error and answers by sending it all again. What is
/// refused before it is read is read and dropped first.
#[tokio::test]
async fn a_body_refused_before_it_is_read_is_read_before_the_answer() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    let server = upstream(
        ResponseTemplate::new(200).set_body_raw(anthropic_answer("ok"), "text/event-stream"),
    )
    .await;
    let billing = engine(
        ProviderFormat::Anthropic,
        &server.uri(),
        vec![ModelMap::new("map", GROUP, "model", "prov", "up")],
    );
    let runtime = ProviderRuntimeRegistry::new();
    runtime.sync_from_billing(&billing);
    let mut registry = FacadeRegistry::new().with_runtime(runtime.clone());
    registry.register(GenerateAssistantResponseHandler {
        billing: billing.clone(),
        runtime: Some(runtime),
        intercept_intent: false,
        card_rate_limiter: gateway::ops::CardRateLimiter::new(1),
        ..Default::default()
    });
    let app = registry.into_router();
    // Takes the card's one request this second, and is refused as unreadable.
    let first = post(&app, "inv-first", None, Body::from("not json")).await;
    assert_eq!(first.status, StatusCode::BAD_REQUEST, "{}", first.text());

    // Over the card's rate: refused before its body is read.
    let read = Arc::new(AtomicUsize::new(0));
    let reply = post(
        &app,
        "inv-second",
        None,
        counted_body(&turn(), 16, read.clone()),
    )
    .await;
    assert_eq!(
        reply.status,
        StatusCode::TOO_MANY_REQUESTS,
        "{}",
        reply.text()
    );
    assert_eq!(
        read.load(Ordering::SeqCst),
        17,
        "the whole body is read first"
    );

    // Kiro's activity log, which Kiro moves past only once a batch is answered.
    let read = Arc::new(AtomicUsize::new(0));
    let response = FacadeRegistry::default()
        .into_router()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/agents/activity")
                .body(counted_body("{\"payload\":[]}", 16, read.clone()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(read.load(Ordering::SeqCst), 17);
}
