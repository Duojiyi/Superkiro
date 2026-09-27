//! PDFs attached in Kiro's chat, as far as the upstreams go: one that reads none, a file too
//! large for the model on its own, and a file an upstream refuses.
//!
//! Its own test binary: which providers read no PDFs is set once per process.

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

const GROUP: &str = "group-documents";
const CARD: &str = "card-documents";

/// kimera-primary, as the deployment names it: its models answer "I can't read the PDF".
const NO_PDF_PROVIDER: &str = "prov-no-pdf";

fn setup() {
    gateway::provider::install_provider_options(
        gateway::provider::ProviderOptionsTable::default().with_no_documents(&[NO_PDF_PROVIDER]),
    );
}

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

/// A funded card whose model "model" goes to `provider` at `url`.
fn engine(provider: &str, url: &str) -> BillingEngine {
    let billing = BillingEngine::new();
    billing.upsert_group(Group::pro_plus(GROUP, "Documents"));
    let mut card = Card::new(CARD, GROUP, 100 * billing::MICRO_CREDITS_PER_CREDIT);
    card.activate(gateway::now_secs(), 86_400).unwrap();
    billing.upsert_card(card);
    billing.upsert_provider(Provider::new(
        provider,
        "Upstream",
        ProviderFormat::Anthropic,
        url,
    ));
    billing.upsert_provider_key(ProviderKey::new("key", provider, "sk-test"));
    billing.upsert_rate_card_version(price("model"));
    billing.upsert_model_map(ModelMap::new("map", GROUP, "model", provider, "up-model"));
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

fn base64(bytes: &[u8]) -> String {
    base64::Engine::encode(&base64::engine::general_purpose::STANDARD, bytes)
}

fn pdf(name: &str, bytes: &[u8]) -> Value {
    json!({"name": name, "format": "pdf", "source": {"bytes": base64(bytes)}})
}

/// A turn whose current message is `message`, with `history` before it.
fn turn(message: Value, history: Vec<Value>) -> Value {
    json!({"conversationState": {
        "conversationId": "conv-documents",
        "history": history,
        "currentMessage": {"userInputMessage": message}
    }})
}

async fn send(app: &axum::Router, invocation: &str, body: Value) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(Method::POST)
        .uri("/generateAssistantResponse")
        .header(header::CONTENT_TYPE, "application/json")
        .header("amz-sdk-invocation-id", invocation)
        .body(Body::from(body.to_string()))
        .unwrap();
    request.extensions_mut().insert(AuthClaims {
        card_id: CARD.into(),
        group_id: GROUP.into(),
        token_version: 0,
        exp: u64::MAX,
        iat: 0,
    });
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn upstream(response: ResponseTemplate) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(response)
        .mount(&server)
        .await;
    server
}

async fn bodies_sent(server: &MockServer) -> Vec<String> {
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .map(|request| String::from_utf8_lossy(&request.body).into_owned())
        .collect()
}

fn document_refusal(status: StatusCode, body: &Value) -> String {
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["__type"], "ValidationException", "{body}");
    body["reason"].as_str().unwrap_or_default().to_string()
}

/// An upstream that reads no PDFs is never sent one: attached now, the PDF is refused
/// naming it; attached earlier, it is a note, and the conversation goes on.
#[tokio::test]
async fn an_upstream_that_reads_no_pdfs_is_never_sent_one() {
    setup();
    let server = upstream(
        ResponseTemplate::new(200).set_body_raw(anthropic_answer("ok"), "text/event-stream"),
    )
    .await;
    let billing = engine(NO_PDF_PROVIDER, &server.uri());
    let app = serve(&billing);
    let paper = pdf("paper", b"%PDF-1.4 <</Type /Page>> paper");

    let (status, body) = send(
        &app,
        "inv-now",
        turn(
            json!({"content": "summarize", "modelId": "model", "documents": [paper.clone()]}),
            vec![],
        ),
    )
    .await;
    assert_eq!(
        document_refusal(status, &body),
        "DOCUMENT_MODEL_NOT_SUPPORTED"
    );
    assert!(
        body["message"].as_str().unwrap().contains("paper.pdf"),
        "{body}"
    );
    assert!(bodies_sent(&server).await.is_empty());

    let (status, body) = send(
        &app,
        "inv-later",
        turn(
            json!({"content": "and now?", "modelId": "model"}),
            vec![
                json!({"userInputMessage": {"content": "summarize", "documents": [paper]}}),
                json!({"assistantResponseMessage": {"content": "It is a paper."}}),
            ],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let sent = bodies_sent(&server).await;
    assert_eq!(sent.len(), 1);
    assert!(!sent[0].contains("\"document\""), "{}", sent[0]);
    assert!(sent[0].contains("paper.pdf"), "{}", sent[0]);
}

/// A file the model cannot take on its own is refused as the attachment it is: refused as
/// a context overflow, Kiro compacted the conversation, left the file in place, and failed
/// again.
#[tokio::test]
async fn a_file_too_large_for_the_model_is_refused_as_the_file() {
    setup();
    let server = upstream(ResponseTemplate::new(200)).await;
    let billing = engine("prov-pages", &server.uri());
    let app = serve(&billing);
    let mut manual = b"%PDF-1.4 1 0 obj <</Type /Pages /Count 150>>".to_vec();
    for _ in 0..150 {
        manual.extend_from_slice(b" <</Type /Page /Parent 1 0 R>>");
    }

    let (status, body) = send(
        &app,
        "inv-pages",
        turn(
            json!({"content": "read", "modelId": "model", "documents": [pdf("manual", &manual)]}),
            vec![],
        ),
    )
    .await;
    assert_eq!(
        document_refusal(status, &body),
        "DOCUMENT_MAXIMUM_PAGES_EXCEEDED"
    );
    assert!(body["message"].as_str().unwrap().contains("150"), "{body}");

    let locked = pdf(
        "locked",
        b"%PDF-1.4 trailer <</Root 1 0 R /Encrypt 9 0 R>> locked",
    );
    let (status, body) = send(
        &app,
        "inv-locked",
        turn(
            json!({"content": "read", "modelId": "model", "documents": [locked]}),
            vec![],
        ),
    )
    .await;
    assert_eq!(
        document_refusal(status, &body),
        "DOCUMENT_PASSWORD_PROTECTED"
    );
    assert!(bodies_sent(&server).await.is_empty());
}

/// A PDF an upstream refuses is the document refusal Kiro shows, not "adjust the request";
/// sent again from the conversation's history it is a note, so the next turn goes through.
#[tokio::test]
async fn a_pdf_an_upstream_refused_does_not_break_the_conversation() {
    setup();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "type": "error",
            "error": {"type": "invalid_request_error",
                "message": "messages.0.content.0.document.source.base64.data: The PDF specified was not valid."}
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(anthropic_answer("ok"), "text/event-stream"),
        )
        .mount(&server)
        .await;
    let billing = engine("prov-refuses", &server.uri());
    let app = serve(&billing);
    let broken = pdf("broken", b"%PDF-1.4 <</Type /Page>> not really a pdf");

    let (status, body) = send(
        &app,
        "inv-refused",
        turn(
            json!({"content": "read", "modelId": "model", "documents": [broken.clone()]}),
            vec![],
        ),
    )
    .await;
    assert_eq!(
        document_refusal(status, &body),
        "DOCUMENT_MODEL_NOT_SUPPORTED"
    );
    assert!(
        body["message"].as_str().unwrap().contains("broken.pdf"),
        "{body}"
    );

    let (status, body) = send(
        &app,
        "inv-after",
        turn(
            json!({"content": "never mind, hello", "modelId": "model"}),
            vec![
                json!({"userInputMessage": {"content": "read", "documents": [broken]}}),
                json!({"assistantResponseMessage": {"content": "Sorry."}}),
            ],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let sent = bodies_sent(&server).await;
    assert_eq!(sent.len(), 2);
    assert!(!sent[1].contains("\"document\""), "{}", sent[1]);
    assert!(sent[1].contains("broken.pdf"), "{}", sent[1]);
}
