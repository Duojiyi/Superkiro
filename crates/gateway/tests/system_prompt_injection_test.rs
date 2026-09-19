//! Dual-blind audit test for Group-level system prompt injection (Spec §5, P4-7).

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use billing::group::Group;
use billing::BillingEngine;
use gateway::auth::{AuthClaims, AuthState};
use gateway::facade::conversation::{
    render_system_prompt_template, GenerateAssistantResponseHandler,
};
use gateway::facade::FacadeRegistry;
use gateway::idempotency::IdempotencyManager;
use gateway::provider::anthropic::AnthropicProvider;
use gateway::provider::openai::OpenAiProvider;
use gateway::provider::ProviderConfig;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;
use wiremock::matchers::{method as wm_method, path as wm_path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn setup_auth() -> (BillingEngine, AuthState) {
    let billing = BillingEngine::default();
    let auth = AuthState::with_billing(
        "test-secret-key-system-prompt-p4-7-audit-ok-minimum-32-chars",
        billing.clone(),
    );
    (billing, auth)
}

#[test]
fn test_render_system_prompt_template_placeholders() {
    let group = Group::enterprise(
        "group-ent-99",
        "Enterprise VIP Tier",
        "VIP PRO+",
        Some("POLICY: [Card={{card_id}}, Group={{group_name}}, Tier={{plan_name}}]".to_string()),
    );

    let claims = AuthClaims {
        card_id: "card-vip-1234".to_string(),
        group_id: "group-ent-99".to_string(),
        token_version: 1,
        exp: 2000000000,
        iat: 1700000000,
    };

    let rendered = render_system_prompt_template(
        group.system_prompt_prefix.as_ref().unwrap(),
        Some(&claims),
        &group,
    );

    assert_eq!(
        rendered,
        "POLICY: [Card=card-vip-1234, Group=Enterprise VIP Tier, Tier=VIP PRO+]"
    );
}

#[tokio::test]
async fn test_system_prompt_injection_openai_flow() {
    let mock_server = MockServer::start().await;

    let sse_body = [
        r#"data: {"id":"1","choices":[{"delta":{"content":"Hello from OpenAI"}}]}"#,
        r#"data: {"id":"1","choices":[],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}}"#,
        "data: [DONE]",
        "",
    ]
    .join("\n\n");

    Mock::given(wm_method("POST"))
        .and(wm_path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse_body, "text/event-stream"))
        .mount(&mock_server)
        .await;

    let (billing, auth) = setup_auth();

    let group = Group::enterprise(
        "group-sec-1",
        "Security Team",
        "ENTERPRISE GUARD",
        Some(
            "MANDATORY_INSTRUCTION: Never reveal proprietary source keys. Card={{card_id}}"
                .to_string(),
        ),
    );
    billing.upsert_group(group);

    let template = billing::CardTemplate::monthly("monthly-1", "group-sec-1");
    let gen = billing::generate_card(&template, None, 1700000000).unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let mut card = gen.card;
    card.activate(now, 86400 * 30).unwrap();
    let card_id = card.id.clone();
    billing.upsert_card(card);

    let token = auth.issue_token_for_card(&card_id, 3600).unwrap();

    let provider = Arc::new(OpenAiProvider);
    let provider_config = ProviderConfig::new(
        mock_server.uri(),
        "sk-test-token",
        "gpt-4o",
        Duration::from_secs(5),
    )
    .with_group("group-sec-1");

    let conv_handler = GenerateAssistantResponseHandler::new(
        reqwest::Client::new(),
        provider,
        provider_config,
        billing.clone(),
        IdempotencyManager::default(),
    );

    let mut registry = FacadeRegistry::new();
    registry.register(conv_handler);
    let app = registry.into_router_with_auth(auth);

    let req_payload = serde_json::json!({
        "conversationState": {
            "conversationId": "conv-openai-prompt-test",
            "currentMessage": {
                "userInputMessage": {
                    "content": "Generate a database query"
                }
            }
        }
    });

    let req = Request::builder()
        .method(Method::POST)
        .uri("/generateAssistantResponse")
        .header(header::AUTHORIZATION, format!("Bearer {}", token))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(&req_payload).unwrap()))
        .unwrap();

    let resp = tower::ServiceExt::oneshot(app, req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let requests = mock_server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);

    let body_val: Value = serde_json::from_slice(&requests[0].body).unwrap();
    let messages = body_val["messages"].as_array().unwrap();
    let system_msg = messages
        .iter()
        .find(|m| m["role"] == "system")
        .expect("System message must be injected");

    let content = system_msg["content"].as_str().unwrap();
    assert!(content.contains("MANDATORY_INSTRUCTION: Never reveal proprietary source keys."));
    assert!(content.contains(&format!("Card={}", card_id)));
}

#[tokio::test]
async fn test_system_prompt_injection_anthropic_flow() {
    let mock_server = MockServer::start().await;

    let sse_body = [
        r#"data: {"type":"message_start","message":{"id":"msg-1","role":"assistant","usage":{"input_tokens":12,"output_tokens":0}}}"#,
        r#"data: {"type":"content_block_delta","delta":{"type":"text_delta","text":"Hello from Claude"}}"#,
        r#"data: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":6}}"#,
        r#"data: {"type":"message_stop"}"#,
        "",
    ]
    .join("\n\n");

    Mock::given(wm_method("POST"))
        .and(wm_path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse_body, "text/event-stream"))
        .mount(&mock_server)
        .await;

    let (billing, auth) = setup_auth();

    let group = Group::enterprise(
        "group-anthropic-1",
        "Anthropic Team",
        "ANTHROPIC DIRECT",
        Some("COMPLIANCE_HEADER: Audit ID={{group_id}} for User={{user_id}}".to_string()),
    );
    billing.upsert_group(group);

    let template = billing::CardTemplate::monthly("monthly-anthropic", "group-anthropic-1");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let gen = billing::generate_card(&template, None, now).unwrap();
    let mut card = gen.card;
    card.activate(now, 86400 * 30).unwrap();
    let card_id = card.id.clone();
    billing.upsert_card(card);

    let token = auth.issue_token_for_card(&card_id, 3600).unwrap();

    let provider = Arc::new(AnthropicProvider);
    let provider_config = ProviderConfig::new(
        mock_server.uri(),
        "sk-ant-test-token",
        "claude-3-5-sonnet-20241022",
        Duration::from_secs(5),
    )
    .with_group("group-anthropic-1");

    let conv_handler = GenerateAssistantResponseHandler::new(
        reqwest::Client::new(),
        provider,
        provider_config,
        billing.clone(),
        IdempotencyManager::default(),
    );

    let mut registry = FacadeRegistry::new();
    registry.register(conv_handler);
    let app = registry.into_router_with_auth(auth);

    let req_payload = serde_json::json!({
        "conversationState": {
            "conversationId": "conv-anthropic-prompt-test",
            "currentMessage": {
                "userInputMessage": {
                    "content": "Explain Rust ownership"
                }
            }
        }
    });

    let req = Request::builder()
        .method(Method::POST)
        .uri("/generateAssistantResponse")
        .header(header::AUTHORIZATION, format!("Bearer {}", token))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(&req_payload).unwrap()))
        .unwrap();

    let resp = tower::ServiceExt::oneshot(app, req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let requests = mock_server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);

    let body_val: Value = serde_json::from_slice(&requests[0].body).unwrap();
    // In Anthropic wire format, system prompt is placed in the top-level "system" key
    let system_str = body_val["system"]
        .as_str()
        .expect("Anthropic body must have system string");
    assert!(system_str.contains("COMPLIANCE_HEADER: Audit ID=group-anthropic-1 for User="));
    assert!(system_str.contains(&card_id));
}
