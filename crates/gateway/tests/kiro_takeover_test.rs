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
    assert_eq!(event["unit"], "Credit");
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
    let post = |invocation: &str, length: Option<usize>, body: Body| {
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
        let app = app.clone();
        async move {
            let response = app.oneshot(request).await.unwrap();
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
    };
    let turn = body(json!({"content": "hello", "modelId": "model"}), vec![]).to_string();

    // 12 MB, padded with JSON whitespace.
    let large = format!("{turn}{}", " ".repeat(12 * 1024 * 1024));
    let reply = post("inv-large", Some(large.len()), Body::from(large)).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
    assert_eq!(usage_entries(&billing).len(), 1);

    // Declared over the limit: refused before it is read.
    let reply = post(
        "inv-declared",
        Some(33 * 1024 * 1024),
        Body::from(turn.clone()),
    )
    .await;
    assert_overflow(&reply);
    assert!(reply.text().contains("32 MB"), "{}", reply.text());
    assert_eq!(reply.headers["x-amzn-requestid"], "inv-declared");

    // Sent without a length, and found over it while reading.
    let padding = bytes::Bytes::from(vec![b' '; 1024 * 1024]);
    let chunks = std::iter::once(bytes::Bytes::from(turn))
        .chain(std::iter::repeat_n(padding, 33))
        .map(Ok::<_, std::io::Error>);
    let reply = post(
        "inv-streamed",
        None,
        Body::from_stream(futures_util::stream::iter(chunks)),
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
