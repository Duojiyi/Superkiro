//! Vision fallback for a text-only model: every image keeps its own transcription, and a
//! failed one leaves only its own image undescribed.
use axum::{
    body::Body,
    http::{header, Method, Request, StatusCode},
};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use billing::{card::Card, engine::BillingEngine};
use gateway::{
    auth::AuthClaims,
    facade::{conversation::GenerateAssistantResponseHandler, FacadeRegistry},
    idempotency::IdempotencyManager,
    provider::{anthropic::AnthropicProvider, ProviderConfig},
    translate::VisionFallbackConfig,
};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::{io::Cursor, sync::Arc, time::Duration};
use tower::ServiceExt;
use wiremock::{
    matchers::{body_string_contains, method, path},
    Mock, MockServer, ResponseTemplate,
};

fn png(width: u32, height: u32) -> String {
    let image = image::RgbImage::from_pixel(width, height, image::Rgb([90, 90, 90]));
    let mut out = Vec::new();
    image
        .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
        .unwrap();
    BASE64.encode(out)
}

fn stream() -> String {
    [
        "event: message_start\n",
        r#"data: {"type":"message_start","message":{"id":"m","model":"deepseek-chat","role":"assistant","content":[],"usage":{"input_tokens":10,"output_tokens":0}}}"#,
        "\n\n",
        "event: content_block_start\n",
        r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
        "\n\n",
        "event: content_block_delta\n",
        r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"ok"}}"#,
        "\n\n",
        "event: message_delta\n",
        r#"data: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":1}}"#,
        "\n\n",
        "event: message_stop\n",
        r#"data: {"type":"message_stop"}"#,
        "\n\n",
    ]
    .concat()
}

/// Posts `images` to a text-only model with vision fallback through `upstream`, which
/// serves both the model and the transcriptions, and returns the model's prompt.
async fn post_images(upstream: &MockServer, images: &[String]) -> String {
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(stream()),
        )
        .mount(upstream)
        .await;
    let billing = BillingEngine::new();
    let mut card = Card::new("card", "group", 100_000_000);
    card.activate(gateway::now_secs(), 86_400).unwrap();
    billing.upsert_card(card);
    let mut handler = GenerateAssistantResponseHandler::new(
        reqwest::Client::new(),
        Arc::new(AnthropicProvider),
        ProviderConfig {
            base_url: upstream.uri(),
            api_key: "key".into(),
            model: "deepseek-chat".into(),
            timeout: Duration::from_secs(5),
            group_id: None,
        },
        billing,
        IdempotencyManager::default(),
    );
    handler.vision_config = Some(VisionFallbackConfig {
        enabled: true,
        fallback_provider_url: Some(upstream.uri()),
        fallback_api_key: Some("vision-key".into()),
        ..Default::default()
    });
    let mut registry = FacadeRegistry::default();
    registry.register(handler);
    let images: Vec<Value> = images
        .iter()
        .map(|bytes| json!({"format": "png", "source": {"bytes": bytes}}))
        .collect();
    let body = json!({"conversationState": {
        "conversationId": "conversation",
        "currentMessage": {"userInputMessage": {"content": "compare these", "images": images}},
    }});
    let mut request = Request::builder()
        .method(Method::POST)
        .uri("/generateAssistantResponse")
        .header(header::CONTENT_TYPE, "application/json")
        .header("amz-sdk-invocation-id", "invocation")
        .body(Body::from(body.to_string()))
        .unwrap();
    request.extensions_mut().insert(AuthClaims {
        card_id: "card".into(),
        group_id: "group".into(),
        token_version: 1,
        exp: 9_999_999_999,
        iat: 1_000_000_000,
    });
    let response = registry.into_router().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    response.into_body().collect().await.unwrap();
    let requests = upstream.received_requests().await.unwrap();
    let prompt = requests
        .iter()
        .find(|request| request.url.path() == "/v1/messages")
        .expect("the model is called");
    String::from_utf8_lossy(&prompt.body).to_string()
}

// Transcriptions run before the first byte is streamed, while the request holds a credit
// reservation and a concurrency slot. They run together, not one after another.
#[tokio::test]
async fn images_are_transcribed_together() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"choices": [{"message": {"content": "A DESCRIPTION"}}]}))
                .set_delay(Duration::from_millis(1_500)),
        )
        .mount(&upstream)
        .await;
    let images = [png(3, 3), png(4, 4), png(5, 5)];

    let started = std::time::Instant::now();
    let prompt = post_images(&upstream, &images).await;
    let elapsed = started.elapsed();

    assert_eq!(prompt.matches("A DESCRIPTION").count(), 3, "{prompt}");
    assert!(
        elapsed < Duration::from_millis(3_500),
        "three transcriptions took {elapsed:?}"
    );
}

#[tokio::test]
async fn a_failed_transcription_does_not_caption_another_image() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(stream()),
        )
        .mount(&upstream)
        .await;
    let (first, second) = (png(3, 3), png(5, 5));
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_string_contains(second.as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"choices": [{"message": {"content": "SECOND-IMAGE-DESCRIPTION"}}]}),
        ))
        .with_priority(1)
        .mount(&upstream)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(500))
        .with_priority(2)
        .mount(&upstream)
        .await;

    let billing = BillingEngine::new();
    let mut card = Card::new("card", "group", 100_000_000);
    card.activate(gateway::now_secs(), 86_400).unwrap();
    billing.upsert_card(card);
    let mut handler = GenerateAssistantResponseHandler::new(
        reqwest::Client::new(),
        Arc::new(AnthropicProvider),
        ProviderConfig {
            base_url: upstream.uri(),
            api_key: "key".into(),
            model: "deepseek-chat".into(),
            timeout: Duration::from_secs(5),
            group_id: None,
        },
        billing,
        IdempotencyManager::default(),
    );
    handler.vision_config = Some(VisionFallbackConfig {
        enabled: true,
        fallback_provider_url: Some(upstream.uri()),
        fallback_api_key: Some("vision-key".into()),
        ..Default::default()
    });
    let mut registry = FacadeRegistry::default();
    registry.register(handler);

    let body = json!({"conversationState": {
        "conversationId": "conversation",
        "currentMessage": {"userInputMessage": {
            "content": "compare these",
            "images": [
                {"format": "png", "source": {"bytes": first}},
                {"format": "png", "source": {"bytes": second}},
            ],
        }},
    }});
    let mut request = Request::builder()
        .method(Method::POST)
        .uri("/generateAssistantResponse")
        .header(header::CONTENT_TYPE, "application/json")
        .header("amz-sdk-invocation-id", "invocation")
        .body(Body::from(body.to_string()))
        .unwrap();
    request.extensions_mut().insert(AuthClaims {
        card_id: "card".into(),
        group_id: "group".into(),
        token_version: 1,
        exp: 9_999_999_999,
        iat: 1_000_000_000,
    });
    let response = registry.into_router().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    response.into_body().collect().await.unwrap();

    let sent: Vec<Value> = upstream
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|request| request.url.path() == "/v1/messages")
        .map(|request| serde_json::from_slice(&request.body).unwrap())
        .collect();
    let prompt = sent[0].to_string();
    let first_note = prompt.find("图片转文字 #1").expect("a note for image 1");
    let second_note = prompt.find("图片转文字 #2").expect("a note for image 2");
    let description = prompt
        .find("SECOND-IMAGE-DESCRIPTION")
        .expect("the second image is described");
    assert!(
        description > second_note && second_note > first_note,
        "the description must sit under image 2, not image 1: {prompt}"
    );
}
