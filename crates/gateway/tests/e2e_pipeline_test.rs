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
    // The completion cache does not contain the original event stream, so a
    // completed invocation must return an explicit protocol error rather than a
    // synthetic successful assistant response.
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
    assert_eq!(replay_resp.status(), StatusCode::CONFLICT);
    assert_eq!(
        replay_resp.headers().get(header::CONTENT_TYPE).unwrap(),
        "application/x-amz-json-1.1"
    );

    let bytes = replay_resp.into_body().collect().await.unwrap().to_bytes();
    let replay_error: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        replay_error["__type"],
        "InvocationAlreadyCompletedException"
    );
    assert!(replay_error["message"]
        .as_str()
        .unwrap()
        .contains("use a new invocation id"));

    // Credit must not be billed twice!
    let card_after_replay = billing.get_card("card-e2e-001").unwrap();
    assert_eq!(card_after_replay.credit_used, card_after.credit_used);
}

/// Kiro's intent-classifier instructions, abridged: they open with the role, name the
/// three categories and describe spec requests, then quote the user's last message.
fn classifier_instructions(user_message: &str) -> String {
    format!(
        "\nYou are an intent classifier for a language model.\n\n\
         Your job is to classify the user's intent based on their conversation history.\n\n\
         Return ONLY a JSON object with 3 properties (chat, do, spec) representing your \
         confidence in each category.\n\n\
         Input belongs in spec mode ONLY if it EXPLICITLY:\n\
         - Asks to create a specification (or spec)\n\n\
         Here is the last user message:\n{user_message}"
    )
}

/// The request Kiro sends to classify `user_message`. With `system_field` the
/// instructions travel as the system prompt, otherwise as the first message.
fn classifier_call(user_message: &str, system_field: bool) -> serde_json::Value {
    let acknowledgement = serde_json::json!({
        "assistantResponseMessage": {"content": "I will follow these instructions"}
    });
    let mut history = vec![acknowledgement];
    let mut request = serde_json::json!({});
    if system_field {
        request["systemPrompt"] = classifier_instructions(user_message).into();
    } else {
        history.insert(
            0,
            serde_json::json!({"userInputMessage": {"content": classifier_instructions(user_message)}}),
        );
    }
    request["conversationState"] = serde_json::json!({
        "conversationId": "conv-intent",
        "history": history,
        "currentMessage": {"userInputMessage": {
            "content": user_message,
            "userInputMessageContext": {}
        }}
    });
    request
}

async fn post(
    app: &axum::Router,
    invocation_id: &str,
    body: &serde_json::Value,
    claims: Option<AuthClaims>,
) -> (StatusCode, Vec<(String, Vec<u8>)>) {
    let mut req = Request::builder()
        .method(Method::POST)
        .uri("/generateAssistantResponse")
        .header(header::CONTENT_TYPE, "application/json")
        .header("amz-sdk-invocation-id", invocation_id)
        .body(Body::from(body.to_string()))
        .unwrap();
    if let Some(claims) = claims {
        req.extensions_mut().insert(claims);
    }
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let mut decoder = EventStreamDecoder::new();
    let mut frames = Vec::new();
    if status == StatusCode::OK {
        decoder.feed(&bytes).unwrap();
        while let Some(frame) = decoder.decode().unwrap() {
            frames.push((frame.event_type().unwrap_or("").to_string(), frame.payload));
        }
    }
    (status, frames)
}

fn assistant_text(frames: &[(String, Vec<u8>)]) -> String {
    frames
        .iter()
        .filter(|(name, _)| name == "assistantResponseEvent")
        .map(|(_, payload)| {
            serde_json::from_slice::<AssistantResponseEvent>(payload)
                .unwrap()
                .content
        })
        .collect()
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

    // Kiro's classifier call is answered locally, in either of the shapes Kiro sends.
    // The instructions themselves describe spec requests; only the user's message decides.
    for (n, (message, system_field, spec)) in [
        ("Please write a rust function to parse JSON.", false, false),
        ("Please write a rust function to parse JSON.", true, false),
        ("Create a spec for the login feature", false, true),
    ]
    .into_iter()
    .enumerate()
    {
        let (status, frames) = post(
            &app,
            &format!("inv-intent-{n}"),
            &classifier_call(message, system_field),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(frames[0].0, "messageMetadataEvent");
        assert_eq!(frames[1].0, "assistantResponseEvent");
        let probs: serde_json::Value = serde_json::from_str(&assistant_text(&frames)).unwrap();
        assert_eq!(probs["chat"], 0);
        if spec {
            assert_eq!(probs["spec"], 0.9, "{message}");
        } else {
            assert_eq!(
                probs["do"], 0.95,
                "{message} (system field: {system_field})"
            );
            assert_eq!(probs["spec"], 0.05);
        }
    }

    // No upstream was called and no card credits were touched!
}

/// The classifier's words inside an ordinary conversation (a pasted log, a file a tool
/// read, the user's own message) are the user's content: the turn goes to the model.
#[tokio::test]
async fn classifier_words_inside_an_ordinary_turn_reach_the_model() {
    let mock_server = MockServer::start().await;
    let sse_body = [
        r#"data: {"type":"message_start","message":{"id":"msg_1","model":"claude-3-5-sonnet","role":"assistant","content":[],"usage":{"input_tokens":150,"output_tokens":0}}}"#,
        r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Here is the unit test."}}"#,
        r#"data: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":6}}"#,
        r#"data: {"type":"message_stop"}"#,
        "",
    ]
    .join("\n\n");
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse_body),
        )
        .mount(&mock_server)
        .await;

    let billing = BillingEngine::new();
    let mut card = Card::new("card-intent-001", "group-default", 100_000_000);
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    card.activate(now_secs, 86400 * 30).unwrap();
    billing.upsert_card(card);
    let conv_handler = GenerateAssistantResponseHandler::new(
        reqwest::Client::new(),
        Arc::new(AnthropicProvider),
        ProviderConfig {
            base_url: mock_server.uri(),
            api_key: "sk-ant-test-key".to_string(),
            model: "claude-3-5-sonnet-20241022".to_string(),
            timeout: Duration::from_secs(5),
            group_id: None,
        },
        billing.clone(),
        IdempotencyManager::default(),
    );
    let mut registry = FacadeRegistry::default();
    registry.register(conv_handler);
    let app = registry.into_router();

    let quoted = classifier_instructions("Fix the parser");
    let tools = serde_json::json!([{"toolSpecification": {
        "name": "readFile",
        "description": "Read a file",
        "inputSchema": {"json": {"type": "object"}}
    }}]);
    let kiro_prompt = serde_json::json!({"userInputMessage": {"content": "You are Kiro, working in the user's IDE."}});
    let acknowledgement = serde_json::json!({"assistantResponseMessage": {"content": "I will follow these instructions."}});
    let turns = [
        // A log the user pasted earlier in the conversation.
        serde_json::json!({"conversationState": {
            "conversationId": "conv-ordinary-1",
            "history": [
                kiro_prompt,
                acknowledgement,
                {"userInputMessage": {"content": format!("Why does Kiro log this?\n{quoted}")}},
                {"assistantResponseMessage": {"content": "That is Kiro's classifier prompt."}}
            ],
            "currentMessage": {"userInputMessage": {
                "content": "Now write a unit test",
                "userInputMessageContext": {"tools": tools}
            }}
        }}),
        // A file a tool read.
        serde_json::json!({"conversationState": {
            "conversationId": "conv-ordinary-2",
            "history": [
                kiro_prompt,
                acknowledgement,
                {"userInputMessage": {"content": "Read the prompt file"}},
                {"assistantResponseMessage": {"content": "", "toolUses": [
                    {"toolUseId": "tool-1", "name": "readFile", "input": {"path": "prompt.txt"}}
                ]}}
            ],
            "currentMessage": {"userInputMessage": {
                "content": "",
                "userInputMessageContext": {
                    "tools": tools,
                    "toolResults": [{"toolUseId": "tool-1", "status": "success", "content": [{"text": quoted}]}]
                }
            }}
        }}),
        // The user's own message, opening a conversation.
        serde_json::json!({"conversationState": {
            "conversationId": "conv-ordinary-3",
            "currentMessage": {"userInputMessage": {"content": quoted}}
        }}),
    ];
    for (n, turn) in turns.iter().enumerate() {
        let (status, frames) = post(
            &app,
            &format!("inv-ordinary-{n}"),
            turn,
            Some(create_auth_claims("card-intent-001")),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "turn {n}");
        assert_eq!(
            assistant_text(&frames),
            "Here is the unit test.",
            "turn {n}"
        );
    }
    assert_eq!(mock_server.received_requests().await.unwrap().len(), 3);
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
