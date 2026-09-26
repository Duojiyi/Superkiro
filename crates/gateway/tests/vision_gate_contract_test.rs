//! Image-input contract at the conversation facade.
//!
//! Nothing else in this suite posts an image through the handler, so the gate,
//! the translation context and the degradation path were free to disagree. These
//! tests pin the resolved behaviour for the declared capability, for a configured
//! degradation path, and for images carried in conversation history.

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
use gateway::translate::VisionFallbackConfig;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use wiremock::matchers::{method as wm_method, path as wm_path};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod support;

const GROUP: &str = "group-vision";
const CARD: &str = "card-vision";
const EXPOSED: &str = "claude-opus-5";
const TARGET: &str = "claude-opus-5";
/// 1x1 PNG. Real bytes so the shrink/decode path runs as it does in production.
const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

fn image() -> Value {
    json!({"format": "png", "source": {"bytes": PNG}})
}

/// A PNG of random pixels, which PNG cannot compress: as large as a detailed screenshot.
fn noise_png(width: u32, height: u32) -> String {
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    let pixels = image::RgbImage::from_fn(width, height, |_, _| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let [r, g, b, ..] = state.to_le_bytes();
        image::Rgb([r, g, b])
    });
    let mut out = Vec::new();
    pixels
        .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .unwrap();
    base64::Engine::encode(&base64::engine::general_purpose::STANDARD, out)
}

struct Harness {
    app: axum::Router,
    token: String,
    billing: BillingEngine,
    upstream: MockServer,
    vision: MockServer,
}

/// `declared_vision` is the operator declaration on the model mapping;
/// `enable_fallback` wires a reachable transcription provider.
async fn harness(declared_vision: bool, enable_fallback: bool) -> Harness {
    let upstream = MockServer::start().await;
    let sse = [
        r#"data: {"id":"1","choices":[{"delta":{"content":"ok"}}]}"#,
        r#"data: {"id":"1","choices":[],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}}"#,
        "data: [DONE]",
        "",
    ]
    .join("\n\n");
    Mock::given(wm_method("POST"))
        .and(wm_path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse, "text/event-stream"))
        .mount(&upstream)
        .await;

    let vision = MockServer::start().await;
    Mock::given(wm_method("POST"))
        .and(wm_path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{"message": {"content": "CURRENT-IMAGE-TRANSCRIPT"}}]
        })))
        .mount(&vision)
        .await;

    let billing = BillingEngine::default();
    billing.upsert_rate_card_version(support::wildcard_price("default"));
    let auth = AuthState::with_billing(
        "test-secret-key-vision-gate-contract-32-chars-ok",
        billing.clone(),
    );
    billing.upsert_group(Group::pro_plus(GROUP, "Vision Testing Group"));

    let template = billing::CardTemplate::monthly("tpl-vision", GROUP);
    let gen = billing::generate_card(&template, None, 1_700_000_000).unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let mut card = gen.card;
    card.id = CARD.to_string();
    card.credit_total = 1_000_000_000;
    card.activate(now, 86_400 * 30).unwrap();
    billing.upsert_card(card);

    let provider = Provider::new(
        "provider-v",
        "OpenAI Compatible",
        ProviderFormat::OpenAi,
        upstream.uri(),
    );
    let key = ProviderKey::new("key-v", "provider-v", "sk-test");
    billing.upsert_provider(provider.clone());
    billing.upsert_provider_key(key.clone());

    let mut model_map = ModelMap::new("mm-vision", GROUP, EXPOSED, "provider-v", TARGET);
    model_map.supports_vision = declared_vision;
    model_map.max_output = 8_192;
    model_map.context_window = 200_000;
    billing
        .publish_commercial_config(
            billing::engine::CommercialUpdate {
                expected_revision: billing.commercial_config().revision,
                reason: "vision gate contract".into(),
                settings: None,
                groups: vec![],
                models: vec![model_map],
                rate_cards: vec![],
                versions: vec![],
                removed_models: vec![],
                cancelled_versions: vec![],
            },
            1_700_000_000,
        )
        .unwrap();

    let mut handler = GenerateAssistantResponseHandler::new(
        reqwest::Client::new(),
        Arc::new(OpenAiProvider),
        ProviderConfig::new(upstream.uri(), "sk-test", TARGET, Duration::from_secs(5)),
        billing.clone(),
        IdempotencyManager::default(),
    )
    .with_pool(ProviderKeyPool::new(provider, vec![key]));
    if enable_fallback {
        handler = handler.with_vision_fallback(VisionFallbackConfig {
            enabled: true,
            fallback_provider_url: Some(vision.uri()),
            fallback_api_key: Some("sk-vision".into()),
            fallback_model: "vision-model".into(),
            max_tokens: 512,
        });
    }

    let token = auth.issue_token_for_card(CARD, 3600).unwrap();
    let mut registry = FacadeRegistry::new();
    registry.register(handler);
    Harness {
        app: registry.into_router_with_auth(auth),
        token,
        billing,
        upstream,
        vision,
    }
}

impl Harness {
    async fn send(&self, payload: Value) -> (StatusCode, String) {
        let req = Request::builder()
            .method(Method::POST)
            .uri("/generateAssistantResponse")
            .header(header::AUTHORIZATION, format!("Bearer {}", self.token))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(&payload).unwrap()))
            .unwrap();
        let resp = tower::ServiceExt::oneshot(self.app.clone(), req)
            .await
            .unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, String::from_utf8_lossy(&bytes).to_string())
    }

    /// Body the main model provider actually received.
    async fn upstream_body(&self) -> Value {
        let reqs = self.upstream.received_requests().await.unwrap();
        assert_eq!(reqs.len(), 1, "expected exactly one upstream call");
        serde_json::from_slice(&reqs[0].body).unwrap()
    }

    async fn vision_calls(&self) -> usize {
        self.vision.received_requests().await.unwrap().len()
    }
}

fn current_only(images: Vec<Value>) -> Value {
    json!({"conversationState": {
        "conversationId": "conv-vision",
        "currentMessage": {"userInputMessage": {
            "content": "what is in this image",
            "modelId": EXPOSED,
            "images": images
        }}
    }})
}

fn with_history(history_images: Vec<Value>, current_images: Vec<Value>) -> Value {
    json!({"conversationState": {
        "conversationId": "conv-vision",
        "history": [
            {"userInputMessage": {"content": "earlier turn", "images": history_images}},
            {"assistantResponseMessage": {"content": "earlier answer"}}
        ],
        "currentMessage": {"userInputMessage": {
            "content": "follow up",
            "modelId": EXPOSED,
            "images": current_images
        }}
    }})
}

/// Declared text-only with no degradation path: reject before spending anything.
#[tokio::test]
async fn declared_text_only_without_fallback_rejects_and_releases_reservation() {
    let h = harness(false, false).await;
    let (status, body) = h.send(current_only(vec![image()])).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body.contains("not configured for image input"),
        "unexpected message: {body}"
    );
    assert!(h.upstream.received_requests().await.unwrap().is_empty());
    assert_eq!(h.billing.get_card(CARD).unwrap().credit_reserved, 0);
}

/// The same rejection must apply to an image already in history, so the refusal
/// is consistent rather than depending on which turn carried the attachment.
#[tokio::test]
async fn declared_text_only_without_fallback_rejects_history_image() {
    let h = harness(false, false).await;
    let (status, body) = h.send(with_history(vec![image()], vec![])).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body.contains("not configured for image input"));
    assert!(h.upstream.received_requests().await.unwrap().is_empty());
    assert_eq!(h.billing.get_card(CARD).unwrap().credit_reserved, 0);
}

/// Declared vision-capable: the image reaches upstream natively and no gate fires.
/// This is the production fix for the six mapped Claude models.
#[tokio::test]
async fn declared_vision_capable_sends_the_image_upstream() {
    let h = harness(true, false).await;
    let (status, _) = h.send(current_only(vec![image()])).await;
    assert_eq!(status, StatusCode::OK);
    let body = h.upstream_body().await;
    let sent = serde_json::to_string(&body).unwrap();
    assert!(
        sent.contains("image_url"),
        "image was not forwarded: {sent}"
    );
    assert_eq!(h.vision_calls().await, 0, "no transcription should occur");
}

/// A screenshot is routinely larger than the size an image is forwarded at. It is shrunk
/// to fit instead of the turn being refused.
#[tokio::test]
async fn a_large_screenshot_is_shrunk_instead_of_refused() {
    let h = harness(true, false).await;
    let screenshot = noise_png(600, 400);
    assert!(screenshot.len() / 4 * 3 > 400 * 1024);
    let (status, body) = h
        .send(current_only(vec![
            json!({"format": "png", "source": {"bytes": screenshot}}),
        ]))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let sent = serde_json::to_string(&h.upstream_body().await).unwrap();
    assert!(
        sent.contains("data:image/jpeg;base64,"),
        "the image is forwarded shrunk"
    );
    assert!(!sent.contains(&screenshot[..200]), "and not as it was sent");
}

/// Kiro resends every earlier image on every turn. Their base64 is not prompt text: a
/// few screenshots must not push a conversation over the prompt length limit for good.
#[tokio::test]
async fn earlier_screenshots_do_not_count_toward_the_prompt_length() {
    let h = harness(true, false).await;
    let mut history = Vec::new();
    for turn in 0..4 {
        let screenshot = noise_png(360, 360);
        assert!(
            screenshot.len() / 4 * 3 <= 400 * 1024,
            "each fits on its own"
        );
        history.push(json!({"userInputMessage": {
            "content": format!("screenshot {turn}"),
            "images": [{"format": "png", "source": {"bytes": screenshot}}]
        }}));
        history.push(json!({"assistantResponseMessage": {"content": "seen"}}));
    }
    let payload = json!({"conversationState": {
        "conversationId": "conv-vision",
        "history": history,
        "currentMessage": {"userInputMessage": {"content": "and now?", "modelId": EXPOSED}}
    }});
    assert!(payload.to_string().len() > 2_000_000);
    let (status, body) = h.send(payload).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        serde_json::to_string(&h.upstream_body().await)
            .unwrap()
            .matches("data:image/png;base64,")
            .count(),
        4
    );

    // Text still counts.
    let (status, body) = h
        .send(json!({"conversationState": {
            "conversationId": "conv-vision",
            "currentMessage": {"userInputMessage": {
                "content": "x".repeat(2_000_001),
                "modelId": EXPOSED
            }}
        }}))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body.contains("Prompt content too long"), "{body}");
}

/// A configured degradation path must actually run. Before the capability sources
/// were unified this returned 502 and never called the vision provider at all.
#[tokio::test]
async fn declared_text_only_with_fallback_transcribes_instead_of_failing() {
    let h = harness(false, true).await;
    let (status, _) = h.send(current_only(vec![image()])).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(h.vision_calls().await, 1);
    let sent = serde_json::to_string(&h.upstream_body().await).unwrap();
    assert!(
        sent.contains("CURRENT-IMAGE-TRANSCRIPT"),
        "transcription missing: {sent}"
    );
    assert!(
        !sent.contains("image_url"),
        "raw image leaked to a text-only target"
    );
}

/// Transcriptions are collected for the current message only. A history image must
/// never be captioned with them, or a past image silently acquires unrelated text.
#[tokio::test]
async fn history_images_do_not_borrow_the_current_transcription() {
    let h = harness(false, true).await;
    let (status, _) = h.send(with_history(vec![image()], vec![image()])).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        h.vision_calls().await,
        1,
        "history must not trigger extra transcription"
    );

    let body = h.upstream_body().await;
    let messages = body["messages"].as_array().unwrap();
    let history = messages
        .iter()
        .find(|m| {
            m["content"]
                .as_str()
                .is_some_and(|c| c.contains("earlier turn"))
        })
        .expect("history user message missing");
    let current = messages
        .iter()
        .find(|m| {
            m["content"]
                .as_str()
                .is_some_and(|c| c.contains("follow up"))
        })
        .expect("current user message missing");

    let history_text = history["content"].as_str().unwrap();
    assert!(
        !history_text.contains("CURRENT-IMAGE-TRANSCRIPT"),
        "history image borrowed the current transcription: {history_text}"
    );
    assert!(
        history_text.contains("Vision Fallback"),
        "history image lost its placeholder: {history_text}"
    );
    assert!(current["content"]
        .as_str()
        .unwrap()
        .contains("CURRENT-IMAGE-TRANSCRIPT"));
}
