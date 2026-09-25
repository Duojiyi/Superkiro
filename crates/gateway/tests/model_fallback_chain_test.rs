//! Dual-blind audit test for Model Aliases and Fallback Chain (Spec §14.6, P4-10).

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
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;
use wiremock::matchers::{method as wm_method, path as wm_path};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod support;

fn setup_auth() -> (BillingEngine, AuthState) {
    let billing = BillingEngine::default();
    billing.upsert_rate_card_version(support::wildcard_price("default"));
    let auth = AuthState::with_billing(
        "test-secret-key-model-fallback-p4-10-audit-ok-32-chars",
        billing.clone(),
    );
    (billing, auth)
}

fn create_activated_card(billing: &BillingEngine, card_id: &str, group_id: &str) {
    let template = billing::CardTemplate::monthly("tpl-1", group_id);
    let gen = billing::generate_card(&template, None, 1700000000).unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let mut card = gen.card;
    card.id = card_id.to_string();
    card.activate(now, 86400 * 30).unwrap();
    billing.upsert_card(card);
}

#[tokio::test]
async fn test_model_alias_resolution() {
    let mock_server = MockServer::start().await;

    let sse_body = [
        r#"data: {"id":"1","choices":[{"delta":{"content":"Hello from aliased model"}}]}"#,
        r#"data: {"id":"1","choices":[],"usage":{"prompt_tokens":12,"completion_tokens":6,"total_tokens":18}}"#,
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

    let group = Group::pro_plus("group-alias-1", "Alias Testing Group");
    billing.upsert_group(group);
    create_activated_card(&billing, "card-alias-1", "group-alias-1");

    // Configure model map with canonical ID "claude-3-7-sonnet" and alias "claude-3.7-sonnet"
    let model_map = ModelMap::new(
        "mm-1",
        "group-alias-1",
        "claude-3-7-sonnet",
        "provider-1",
        "claude-3-7-sonnet-upstream",
    )
    .with_alias("claude-3.7-sonnet");
    billing.upsert_model_map(model_map);

    let provider = Provider::new(
        "provider-1",
        "OpenAI Compatible",
        ProviderFormat::OpenAi,
        mock_server.uri(),
    );
    let key = ProviderKey::new("key-1", "provider-1", "sk-test");
    let pool = ProviderKeyPool::new(provider.clone(), vec![key]);

    let token = auth.issue_token_for_card("card-alias-1", 3600).unwrap();

    let conv_handler = GenerateAssistantResponseHandler::new(
        reqwest::Client::new(),
        Arc::new(OpenAiProvider),
        ProviderConfig::new(
            mock_server.uri(),
            "sk-test",
            "claude-3-7-sonnet-upstream",
            Duration::from_secs(5),
        ),
        billing.clone(),
        IdempotencyManager::default(),
    )
    .with_pool(pool);

    let mut registry = FacadeRegistry::new();
    registry.register(conv_handler);
    let app = registry.into_router_with_auth(auth);

    // Request specifying the alias "claude-3.7-sonnet"
    let req_payload = serde_json::json!({
        "conversationState": {
            "conversationId": "conv-alias-test",
            "currentMessage": {
                "userInputMessage": {
                    "content": "Explain quantum computing",
                    "modelId": "claude-3.7-sonnet"
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

    // Verify upstream received the resolved target model
    let requests = mock_server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let req_val: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(req_val["model"], "claude-3-7-sonnet-upstream");
}

#[tokio::test]
async fn test_model_fallback_chain_on_upstream_failure() {
    let primary_server = MockServer::start().await;
    let fallback_server = MockServer::start().await;

    // Primary server fails with 500
    Mock::given(wm_method("POST"))
        .and(wm_path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(500).set_body_string("Internal Server Error"))
        .mount(&primary_server)
        .await;

    // Fallback server succeeds with 200 SSE
    let sse_body = [
        r#"data: {"id":"2","choices":[{"delta":{"content":"Response from fallback model"}}]}"#,
        r#"data: {"id":"2","choices":[],"usage":{"prompt_tokens":15,"completion_tokens":8,"total_tokens":23}}"#,
        "data: [DONE]",
        "",
    ]
    .join("\n\n");

    Mock::given(wm_method("POST"))
        .and(wm_path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse_body, "text/event-stream"))
        .mount(&fallback_server)
        .await;

    let (billing, auth) = setup_auth();

    let group = Group::pro_plus("group-fb-1", "Fallback Test Group");
    billing.upsert_group(group);
    create_activated_card(&billing, "card-fb-1", "group-fb-1");

    // Primary provider p-primary -> fallback provider p-fallback
    let model_map = ModelMap::new(
        "mm-fb",
        "group-fb-1",
        "claude-3-7-sonnet",
        "p-primary",
        "claude-3-7-sonnet-primary",
    )
    .with_fallback("p-fallback", "gpt-4o-fallback");
    billing.upsert_model_map(model_map);

    let p_primary = Provider::new(
        "p-primary",
        "Primary Provider",
        ProviderFormat::OpenAi,
        primary_server.uri(),
    );
    let pool_primary = ProviderKeyPool::new(
        p_primary.clone(),
        vec![ProviderKey::new("k1", "p-primary", "sk-1")],
    );

    let p_fallback = Provider::new(
        "p-fallback",
        "Fallback Provider",
        ProviderFormat::OpenAi,
        fallback_server.uri(),
    );
    let pool_fallback = ProviderKeyPool::new(
        p_fallback.clone(),
        vec![ProviderKey::new("k2", "p-fallback", "sk-2")],
    );

    let token = auth.issue_token_for_card("card-fb-1", 3600).unwrap();

    let conv_handler = GenerateAssistantResponseHandler::new(
        reqwest::Client::new(),
        Arc::new(OpenAiProvider),
        ProviderConfig::new(
            primary_server.uri(),
            "sk-1",
            "claude-3-7-sonnet-primary",
            Duration::from_secs(5),
        ),
        billing.clone(),
        IdempotencyManager::default(),
    )
    .with_pool(pool_primary)
    .with_fallback_pool("p-fallback", pool_fallback);

    let mut registry = FacadeRegistry::new();
    registry.register(conv_handler);
    let app = registry.into_router_with_auth(auth);

    let req_payload = serde_json::json!({
        "conversationState": {
            "conversationId": "conv-fallback-chain-test",
            "currentMessage": {
                "userInputMessage": {
                    "content": "Hello fallback",
                    "modelId": "claude-3-7-sonnet"
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

    // Primary received 1 failed attempt
    let primary_reqs = primary_server.received_requests().await.unwrap();
    assert_eq!(primary_reqs.len(), 1);

    // Fallback received the failover attempt with target fallback model
    let fallback_reqs = fallback_server.received_requests().await.unwrap();
    assert_eq!(fallback_reqs.len(), 1);
    let fb_body: Value = serde_json::from_slice(&fallback_reqs[0].body).unwrap();
    assert_eq!(fb_body["model"], "gpt-4o-fallback");
}

/// Where a model is routed is the operator's business. Every frame the customer receives
/// names the model they asked for, whether the primary target or a fallback answered.
#[tokio::test]
async fn frames_name_the_model_asked_for_not_the_upstream_target() {
    let primary_server = MockServer::start().await;
    let fallback_server = MockServer::start().await;
    Mock::given(wm_method("POST"))
        .and(wm_path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&primary_server)
        .await;
    let sse_body = [
        r#"data: {"id":"3","choices":[{"delta":{"content":"Hello "}}]}"#,
        r#"data: {"id":"3","choices":[{"delta":{"content":"there"}}]}"#,
        r#"data: {"id":"3","choices":[],"usage":{"prompt_tokens":15,"completion_tokens":8,"total_tokens":23}}"#,
        "data: [DONE]",
        "",
    ]
    .join("\n\n");
    Mock::given(wm_method("POST"))
        .and(wm_path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse_body, "text/event-stream"))
        .mount(&fallback_server)
        .await;

    let (billing, auth) = setup_auth();
    billing.upsert_group(Group::pro_plus("group-frames", "Frames Test Group"));
    create_activated_card(&billing, "card-frames", "group-frames");
    billing.upsert_model_map(
        ModelMap::new(
            "mm-frames",
            "group-frames",
            "claude-sonnet-4.5",
            "p-internal",
            "internal-cheap-model",
        )
        .with_fallback("p-reserve", "internal-reserve-model"),
    );
    let primary_pool = ProviderKeyPool::new(
        Provider::new(
            "p-internal",
            "Internal",
            ProviderFormat::OpenAi,
            primary_server.uri(),
        ),
        vec![ProviderKey::new("k-internal", "p-internal", "sk-1")],
    );
    let fallback_pool = ProviderKeyPool::new(
        Provider::new(
            "p-reserve",
            "Reserve",
            ProviderFormat::OpenAi,
            fallback_server.uri(),
        ),
        vec![ProviderKey::new("k-reserve", "p-reserve", "sk-2")],
    );
    let token = auth.issue_token_for_card("card-frames", 3600).unwrap();
    let conv_handler = GenerateAssistantResponseHandler::new(
        reqwest::Client::new(),
        Arc::new(OpenAiProvider),
        ProviderConfig::new(
            primary_server.uri(),
            "sk-1",
            "internal-cheap-model",
            Duration::from_secs(5),
        ),
        billing.clone(),
        IdempotencyManager::default(),
    )
    .with_pool(primary_pool)
    .with_fallback_pool("p-reserve", fallback_pool);
    let mut registry = FacadeRegistry::new();
    registry.register(conv_handler);
    let app = registry.into_router_with_auth(auth);

    let req_payload = serde_json::json!({
        "conversationState": {
            "conversationId": "conv-frames",
            "currentMessage": {"userInputMessage": {
                "content": "Hello",
                "modelId": "claude-sonnet-4.5"
            }}
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
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(fallback_server.received_requests().await.unwrap().len(), 1);

    let mut decoder = kiro_wire::decoder::EventStreamDecoder::new();
    decoder.feed(&bytes).unwrap();
    let mut text = String::new();
    while let Some(frame) = decoder.decode().unwrap() {
        if frame.event_type() == Some("assistantResponseEvent") {
            let event: Value = serde_json::from_slice(&frame.payload).unwrap();
            assert_eq!(event["modelId"], "claude-sonnet-4.5");
            text.push_str(event["content"].as_str().unwrap());
        }
    }
    assert_eq!(text, "Hello there");
    let raw = String::from_utf8_lossy(&bytes);
    assert!(
        !raw.contains("internal-"),
        "an upstream target reached the client"
    );
}

#[tokio::test]
async fn test_model_fallback_chain_all_exhausted() {
    let primary_server = MockServer::start().await;
    let fallback_server = MockServer::start().await;

    // Both servers fail
    Mock::given(wm_method("POST"))
        .and(wm_path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&primary_server)
        .await;

    Mock::given(wm_method("POST"))
        .and(wm_path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&fallback_server)
        .await;

    let (billing, auth) = setup_auth();

    let group = Group::pro_plus("group-fb-ex", "Exhaustion Test Group");
    billing.upsert_group(group);
    create_activated_card(&billing, "card-fb-ex", "group-fb-ex");

    let model_map = ModelMap::new(
        "mm-ex",
        "group-fb-ex",
        "claude-3-7-sonnet",
        "p-ex-1",
        "model-ex-1",
    )
    .with_fallback("p-ex-2", "model-ex-2");
    billing.upsert_model_map(model_map);

    let p1 = Provider::new("p-ex-1", "P1", ProviderFormat::OpenAi, primary_server.uri());
    let pool1 = ProviderKeyPool::new(p1, vec![ProviderKey::new("k1", "p-ex-1", "sk-1")]);

    let p2 = Provider::new(
        "p-ex-2",
        "P2",
        ProviderFormat::OpenAi,
        fallback_server.uri(),
    );
    let pool2 = ProviderKeyPool::new(p2, vec![ProviderKey::new("k2", "p-ex-2", "sk-2")]);

    let token = auth.issue_token_for_card("card-fb-ex", 3600).unwrap();

    let conv_handler = GenerateAssistantResponseHandler::new(
        reqwest::Client::new(),
        Arc::new(OpenAiProvider),
        ProviderConfig::new(
            primary_server.uri(),
            "sk-1",
            "model-ex-1",
            Duration::from_secs(5),
        ),
        billing.clone(),
        IdempotencyManager::default(),
    )
    .with_pool(pool1)
    .with_fallback_pool("p-ex-2", pool2);

    let mut registry = FacadeRegistry::new();
    registry.register(conv_handler);
    let app = registry.into_router_with_auth(auth);

    let req_payload = serde_json::json!({
        "conversationState": {
            "conversationId": "conv-exhaust-test",
            "currentMessage": {
                "userInputMessage": {
                    "content": "Hello all fail",
                    "modelId": "claude-3-7-sonnet"
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
    assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);

    // Verify reservation was released and not charged
    assert_eq!(billing.get_active_concurrency("card-fb-ex"), 0);
}
