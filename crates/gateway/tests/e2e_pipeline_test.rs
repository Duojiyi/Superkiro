//! End-to-end integration test for P1-12:
//! Gateway conversation pipeline wiring, provider streaming, billing reservation & settlement,
//! keepalive, and idempotency deduplication.
//!
//! Spec §4.2, §4.3, §4.6, §4.7, §5, §6.1, §6.2, §6.3, §15.1, §15.2.

use axum::{
    body::Body,
    http::{header, Method, Request, StatusCode},
};
use billing::card::Card;
use billing::engine::BillingEngine;
use futures_util::StreamExt;
use gateway::auth::AuthClaims;
use gateway::facade::conversation::GenerateAssistantResponseHandler;
use gateway::facade::FacadeRegistry;
use gateway::idempotency::IdempotencyManager;
use gateway::provider::anthropic::AnthropicProvider;
use gateway::provider::ProviderConfig;
use http_body_util::BodyExt;
use kiro_wire::decoder::EventStreamDecoder;
use kiro_wire::events::AssistantResponseEvent;
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn create_auth_claims(card_id: &str) -> AuthClaims {
    AuthClaims {
        card_id: card_id.to_string(),
        group_id: "group-default".to_string(),
        token_version: 1,
        exp: 9999999999,
        iat: 1000000000,
    }
}

#[tokio::test]
async fn test_e2e_conversation_pipeline_with_provider_and_billing_settlement() {
    // 1. Setup mock upstream Anthropic provider server
    let mock_server = MockServer::start().await;

    let sse_body = [
        "event: message_start\n",
        r#"data: {"type":"message_start","message":{"id":"msg_123","model":"claude-3-5-sonnet","role":"assistant","content":[],"usage":{"input_tokens":150,"output_tokens":0}}}"#,
        "\n\n",
        "event: content_block_start\n",
        r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
        "\n\n",
        "event: content_block_delta\n",
        r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello world from BYOK!"}}"#,
        "\n\n",
        "event: message_delta\n",
        r#"data: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":40}}"#,
        "\n\n",
        "event: message_stop\n",
        r#"data: {"type":"message_stop"}"#,
        "\n\n",
    ]
    .concat();

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse_body),
        )
        .mount(&mock_server)
        .await;

    // 2. Setup Billing Engine with a test card
    let billing = BillingEngine::new();
    let initial_credits = 100_000_000i64; // 100 credits
    let mut card = Card::new("card-e2e-001", "group-default", initial_credits);
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    card.activate(now_secs, 86400 * 30).unwrap();
    billing.upsert_card(card);

    // 3. Setup Idempotency Manager and Provider
    let idempotency = IdempotencyManager::new(Duration::from_secs(60));
    let provider = Arc::new(AnthropicProvider);
    let provider_config = ProviderConfig {
        base_url: mock_server.uri(),
        api_key: "sk-ant-test-key".to_string(),
        model: "claude-3-5-sonnet-20241022".to_string(),
        timeout: Duration::from_secs(5),
        group_id: None,
    };

    let client = reqwest::Client::new();
    let conv_handler = GenerateAssistantResponseHandler::new(
        client,
        provider,
        provider_config,
        billing.clone(),
        idempotency.clone(),
    );

    let mut registry = FacadeRegistry::default();
    registry.register(conv_handler);
    let app = registry.into_router();

    // 4. Construct Kiro conversation request
    let kiro_req_body = serde_json::json!({
        "conversationState": {
            "conversationId": "conv-e2e-999",
            "currentMessage": {
                "userInputMessage": {
                    "content": "Say hello world!",
                    "modelId": "claude-3-5-sonnet-20241022"
                }
            },
            "history": []
        }
    });

    let invocation_id = "inv-e2e-round-1";
    let claims = create_auth_claims("card-e2e-001");

    let mut req = Request::builder()
        .method(Method::POST)
        .uri("/generateAssistantResponse")
        .header(header::CONTENT_TYPE, "application/json")
        .header("amz-sdk-invocation-id", invocation_id)
        .body(Body::from(kiro_req_body.to_string()))
        .unwrap();

    // Inject claims extension
    req.extensions_mut().insert(claims);

    // 5. Execute request
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers().get(header::CONTENT_TYPE).unwrap(),
        "application/vnd.amazon.eventstream"
    );

    // 6. Decode AWS EventStream frames from body
    let mut body_stream = resp.into_body().into_data_stream();
    let mut decoder = EventStreamDecoder::new();
    let mut decoded_text = String::new();

    while let Some(chunk_res) = body_stream.next().await {
        let chunk = chunk_res.unwrap();
        decoder.feed(&chunk).unwrap();
        while let Some(frame) = decoder.decode().unwrap() {
            if frame.event_type() == Some("assistantResponseEvent") {
                let evt: AssistantResponseEvent =
                    serde_json::from_slice(&frame.payload).expect("Decode AssistantResponseEvent");
                decoded_text.push_str(&evt.content);
            }
        }
    }

    assert_eq!(decoded_text, "Hello world from BYOK!");

    // 7. Verify Billing Settlement
    let card_after = billing.get_card("card-e2e-001").expect("Card must exist");
    assert_eq!(
        card_after.credit_reserved, 0,
        "Credit reservation must be fully unfrozen"
    );
    assert!(
        card_after.credit_used > 0,
        "Actual usage must be charged to credit_used (got {})",
        card_after.credit_used
    );
    assert_eq!(
        card_after.available_credits(),
        initial_credits - card_after.credit_used,
        "Balance must equal initial minus used"
    );

    // 8. Test Idempotent Replay (Spec §4.7)
    // Sending the same invocation_id again must return the cached short-circuit
    let mut replay_req = Request::builder()
        .method(Method::POST)
        .uri("/generateAssistantResponse")
        .header(header::CONTENT_TYPE, "application/json")
        .header("amz-sdk-invocation-id", invocation_id)
        .body(Body::from(kiro_req_body.to_string()))
        .unwrap();

    let replay_claims = create_auth_claims("card-e2e-001");
    replay_req.extensions_mut().insert(replay_claims);

    let replay_resp = app.clone().oneshot(replay_req).await.unwrap();
    assert_eq!(replay_resp.status(), StatusCode::OK);

    let bytes = replay_resp.into_body().collect().await.unwrap().to_bytes();
    let mut replay_decoder = EventStreamDecoder::new();
    replay_decoder.feed(&bytes).unwrap();
    let replay_frame = replay_decoder.decode().unwrap().unwrap();
    assert_eq!(replay_frame.event_type(), Some("assistantResponseEvent"));
    let replay_evt: AssistantResponseEvent = serde_json::from_slice(&replay_frame.payload).unwrap();
    assert!(replay_evt.content.contains("idempotent replay"));

    // Credit must not be billed twice!
    let card_after_replay = billing.get_card("card-e2e-001").unwrap();
    assert_eq!(card_after_replay.credit_used, card_after.credit_used);
}

#[tokio::test]
async fn test_e2e_intent_classifier_interception_optimization() {
    let billing = BillingEngine::new();
    let idempotency = IdempotencyManager::default();

    let conv_handler = GenerateAssistantResponseHandler {
        client: reqwest::Client::new(),
        provider: None,
        provider_config: None,
        pool: None,
        fallback_pools: Default::default(),
        billing: billing.clone(),
        idempotency,
        guardrail: gateway::guardrail::CapacityGuardrail::default(),
        card_rate_limiter: gateway::ops::CardRateLimiter::new(60),
        intercept_intent: true,
        vision_config: None,
        vision_cache: Default::default(),
        content_guardrail: gateway::security::ContentGuardrailConfig::default(),
        runtime: None,
    };

    let mut registry = FacadeRegistry::default();
    registry.register(conv_handler);
    let app = registry.into_router();

    // Request containing Kiro's internal intent classifier prompt
    let intent_prompt = "You are an intent classifier for a language model. Classify the user query into (chat, do, spec). User query: Please write a rust function to parse JSON.";
    let req_body = serde_json::json!({
        "conversationState": {
            "currentMessage": {
                "userInputMessage": {
                    "content": intent_prompt
                }
            }
        }
    });

    let req = Request::builder()
        .method(Method::POST)
        .uri("/generateAssistantResponse")
        .header(header::CONTENT_TYPE, "application/json")
        .header("amz-sdk-invocation-id", "inv-intent-001")
        .body(Body::from(req_body.to_string()))
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let mut decoder = EventStreamDecoder::new();
    decoder.feed(&bytes).unwrap();

    // First frame is messageMetadataEvent
    let f1 = decoder.decode().unwrap().expect("Frame 1");
    assert_eq!(f1.event_type(), Some("messageMetadataEvent"));

    // Second frame is assistantResponseEvent with probabilities JSON
    let f2 = decoder.decode().unwrap().expect("Frame 2");
    assert_eq!(f2.event_type(), Some("assistantResponseEvent"));
    let evt: AssistantResponseEvent = serde_json::from_slice(&f2.payload).unwrap();
    let probs: serde_json::Value = serde_json::from_str(&evt.content).unwrap();
    assert_eq!(probs["chat"], 0);
    assert_eq!(probs["do"], 0.95);
    assert_eq!(probs["spec"], 0.05);

    // No upstream was called and no card credits were touched!
}

#[tokio::test]
async fn test_e2e_insufficient_credit_rejection() {
    let billing = BillingEngine::new();
    // Card with zero balance
    let mut card = Card::new("card-broke-001", "group-default", 0);
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    card.activate(now_secs, 86400 * 30).unwrap();
    billing.upsert_card(card);

    let conv_handler = GenerateAssistantResponseHandler {
        client: reqwest::Client::new(),
        provider: None,
        provider_config: None,
        pool: None,
        fallback_pools: Default::default(),
        billing: billing.clone(),
        idempotency: IdempotencyManager::default(),
        guardrail: gateway::guardrail::CapacityGuardrail::default(),
        card_rate_limiter: gateway::ops::CardRateLimiter::new(60),
        intercept_intent: true,
        vision_config: None,
        vision_cache: Default::default(),
        content_guardrail: gateway::security::ContentGuardrailConfig::default(),
        runtime: None,
    };

    let mut registry = FacadeRegistry::default();
    registry.register(conv_handler);
    let app = registry.into_router();

    let req_body = serde_json::json!({
        "conversationState": {
            "conversationId": "conv-broke-001",
            "currentMessage": {
                "userInputMessage": {
                    "content": "Regular user question"
                }
            }
        }
    });

    let mut req = Request::builder()
        .method(Method::POST)
        .uri("/generateAssistantResponse")
        .header(header::CONTENT_TYPE, "application/json")
        .header("amz-sdk-invocation-id", "inv-broke-001")
        .body(Body::from(req_body.to_string()))
        .unwrap();

    req.extensions_mut()
        .insert(create_auth_claims("card-broke-001"));

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::PAYMENT_REQUIRED);

    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let err_json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(err_json["__type"], "InsufficientCreditException");
}
