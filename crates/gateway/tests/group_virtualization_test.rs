//! Tests for P2-4: Group & Virtualization.
//!
//! Covers:
//! - Virtual plan name & quota isolation (`virtual_plan_name`, `virtual_usage_limit`)
//! - Model visibility filtering per group (`model_map.visible`, `sort_order`)
//! - System prompt prefix injection (`system_prompt_prefix`)
//! - Provider binding isolation (`provider_binding_mode: Shared | Dedicated`)
//!
//! Spec §1.4, §5, §15.

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use billing::card::{Card, CardStatus};
use billing::engine::BillingEngine;
use billing::group::{Group, ModelMap};
use gateway::auth::AuthState;
use gateway::facade::conversation::GenerateAssistantResponseHandler;
use gateway::facade::virtualization::VirtualizationStore;
use gateway::facade::FacadeRegistry;
use gateway::idempotency::IdempotencyManager;
use gateway::provider::openai::OpenAiProvider;
use gateway::provider::ProviderConfig;
use http_body_util::BodyExt;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt;
use wiremock::matchers::{method as wm_method, path as wm_path};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod support;

const SECRET: &str = "test-secret-key-32bytes-for-p2-4-groups!!";

fn setup_environment() -> (BillingEngine, AuthState) {
    let billing = BillingEngine::new();
    for rate_card in ["default", "enterprise"] {
        billing.upsert_rate_card_version(support::wildcard_price(rate_card));
    }
    let auth = AuthState::with_billing(SECRET, billing.clone());
    (billing, auth)
}

fn create_activated_card(id: &str, group_id: &str) -> Card {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let mut card = Card::new(id, group_id, 100_000_000);
    card.status = CardStatus::Active;
    card.activated_at = Some(now);
    card.valid_until = Some(now + 86_400);
    card
}

async fn send_req(
    app: axum::Router,
    method: Method,
    uri: &str,
    token: &str,
    body: Body,
) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {}", token))
        .header(header::CONTENT_TYPE, "application/json")
        .body(body)
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

#[tokio::test]
async fn test_audit_b_group_virtual_plan_and_quota_isolation() {
    let (billing, auth) = setup_environment();

    // Group 1: Standard Pro+
    let group_pro = Group::pro_plus("group-pro", "Pro Plus Tenant");
    billing.upsert_group(group_pro);

    // Group 2: Enterprise
    let mut group_ent = Group::enterprise(
        "group-ent",
        "Enterprise Tenant",
        "KIRO ULTRA ENTERPRISE",
        None,
    );
    group_ent.virtual_usage_limit = 999_999.0;
    billing.upsert_group(group_ent);

    // Cards in respective groups
    let card_pro = create_activated_card("card-pro-1", "group-pro");
    billing.upsert_card(card_pro);
    let mut card_ent = create_activated_card("card-ent-1", "group-ent");
    card_ent.credit_total = 200_000_000;
    billing.upsert_card(card_ent);

    let token_pro = auth.issue_token_for_card("card-pro-1", 3600).unwrap();
    let token_ent = auth.issue_token_for_card("card-ent-1", 3600).unwrap();

    let store = VirtualizationStore::with_billing(billing.clone(), "group-pro");
    let mut registry = FacadeRegistry::new();
    registry.register_virtualized_facades(store);
    let app = registry.into_router_with_auth(auth);

    // Query /getUsageLimits as Pro+ card
    let (status_pro, json_pro) = send_req(
        app.clone(),
        Method::GET,
        "/getUsageLimits",
        &token_pro,
        Body::empty(),
    )
    .await;
    assert_eq!(status_pro, StatusCode::OK);
    assert_eq!(
        json_pro["subscriptionInfo"]["subscriptionTitle"],
        "Legacy service plan"
    );
    let breakdown_pro = json_pro["usageBreakdownList"].as_array().unwrap();
    assert_eq!(breakdown_pro[0]["usageLimit"].as_f64().unwrap(), 100.0);
    assert_eq!(json_pro["overageConfiguration"]["overageEnabled"], false);

    // Query /getUsageLimits as Enterprise card
    let (status_ent, json_ent) = send_req(
        app.clone(),
        Method::GET,
        "/getUsageLimits",
        &token_ent,
        Body::empty(),
    )
    .await;
    assert_eq!(status_ent, StatusCode::OK);
    assert_eq!(
        json_ent["subscriptionInfo"]["subscriptionTitle"],
        "Legacy service plan"
    );
    let breakdown_ent = json_ent["usageBreakdownList"].as_array().unwrap();
    assert_eq!(breakdown_ent[0]["usageLimit"].as_f64().unwrap(), 200.0);
}

#[tokio::test]
async fn test_audit_b_model_visibility_and_sorting_per_group() {
    let (billing, auth) = setup_environment();

    let group_a = Group::pro_plus("group-a", "Tenant A");
    billing.upsert_group(group_a);
    let group_b = Group::pro_plus("group-b", "Tenant B");
    billing.upsert_group(group_b);

    // Group A mappings:
    // deepseek-chat (visible = true, sort 1)
    // claude-sonnet-4.5 (visible = false, sort 2) -> HIDDEN from Group A
    let mut m1 = ModelMap::new("m1", "group-a", "deepseek-chat", "prov-1", "deepseek-v3");
    m1.visible = true;
    m1.sort_order = 1;
    billing.upsert_model_map(m1);

    let mut m2 = ModelMap::new("m2", "group-a", "claude-sonnet-4.5", "prov-1", "claude-3-5");
    m2.visible = false; // Hidden!
    m2.sort_order = 2;
    billing.upsert_model_map(m2);

    // Group B mappings:
    // claude-sonnet-4.5 (visible = true, sort 1)
    // deepseek-chat (visible = true, sort 2)
    let mut m3 = ModelMap::new("m3", "group-b", "claude-sonnet-4.5", "prov-1", "claude-3-5");
    m3.visible = true;
    m3.sort_order = 1;
    billing.upsert_model_map(m3);

    let mut m4 = ModelMap::new("m4", "group-b", "deepseek-chat", "prov-1", "deepseek-v3");
    m4.visible = true;
    m4.sort_order = 2;
    billing.upsert_model_map(m4);

    let card_a = create_activated_card("card-a", "group-a");
    billing.upsert_card(card_a);
    let card_b = create_activated_card("card-b", "group-b");
    billing.upsert_card(card_b);

    let token_a = auth.issue_token_for_card("card-a", 3600).unwrap();
    let token_b = auth.issue_token_for_card("card-b", 3600).unwrap();

    let store = VirtualizationStore::with_billing(billing.clone(), "group-a");
    let mut registry = FacadeRegistry::new();
    registry.register_virtualized_facades(store);
    let app = registry.into_router_with_auth(auth);

    // Group A query: claude-sonnet-4.5 must NOT appear
    let (status_a, json_a) = send_req(
        app.clone(),
        Method::GET,
        "/ListAvailableModels",
        &token_a,
        Body::empty(),
    )
    .await;
    assert_eq!(status_a, StatusCode::OK);
    let models_a = json_a["models"].as_array().unwrap();
    assert_eq!(models_a.len(), 1);
    assert_eq!(models_a[0]["modelId"], "deepseek-chat");

    // Group B query: both models appear, sorted: claude first, then deepseek
    let (status_b, json_b) = send_req(
        app.clone(),
        Method::GET,
        "/ListAvailableModels",
        &token_b,
        Body::empty(),
    )
    .await;
    assert_eq!(status_b, StatusCode::OK);
    let models_b = json_b["models"].as_array().unwrap();
    assert_eq!(models_b.len(), 2);
    assert_eq!(models_b[0]["modelId"], "claude-sonnet-4.5");
    assert_eq!(models_b[1]["modelId"], "deepseek-chat");
}

#[tokio::test]
async fn test_audit_b_system_prompt_prefix_injection() {
    let mock_server = MockServer::start().await;

    // Mock upstream OpenAI SSE completion
    let sse_body = [
        r#"data: {"id":"1","choices":[{"delta":{"content":"Response from model"}}]}"#,
        r#"data: {"id":"1","choices":[],"usage":{"prompt_tokens":20,"completion_tokens":10,"total_tokens":30}}"#,
        "data: [DONE]",
        "",
    ]
    .join("\n\n");

    Mock::given(wm_method("POST"))
        .and(wm_path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse_body, "text/event-stream"))
        .mount(&mock_server)
        .await;

    let (billing, auth) = setup_environment();

    // Group with custom system prompt prefix
    let group = Group::enterprise(
        "group-with-prefix",
        "Secure Enterprise",
        "SECURE ENTERPRISE",
        Some("SECURITY_GUARD: All code must be memory-safe and follow Rust idioms.".to_string()),
    );
    billing.upsert_group(group);

    let card = create_activated_card("card-prefix-1", "group-with-prefix");
    billing.upsert_card(card);
    let token = auth.issue_token_for_card("card-prefix-1", 3600).unwrap();

    let provider = Arc::new(OpenAiProvider);
    let provider_config = ProviderConfig::new(
        mock_server.uri(),
        "sk-test-key",
        "gpt-4o",
        Duration::from_secs(5),
    )
    .with_group("group-with-prefix");

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

    let req_body = serde_json::json!({
        "conversationState": {
            "conversationId": "conv-prefix-test",
            "currentMessage": {
                "userInputMessage": {
                    "content": "Write a function"
                }
            }
        }
    });

    let (status, _) = send_req(
        app,
        Method::POST,
        "/generateAssistantResponse",
        &token,
        Body::from(serde_json::to_vec(&req_body).unwrap()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Verify upstream received the injected system prompt prefix
    let requests = mock_server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let body_str = String::from_utf8(requests[0].body.clone()).unwrap();
    let sent_json: Value = serde_json::from_str(&body_str).unwrap();

    let messages = sent_json["messages"].as_array().unwrap();
    let system_msg = messages
        .iter()
        .find(|m| m["role"] == "system")
        .expect("system message must be present");
    assert!(system_msg["content"]
        .as_str()
        .unwrap()
        .contains("SECURITY_GUARD: All code must be memory-safe"));
}

#[tokio::test]
async fn test_audit_b_provider_binding_mode_isolation_shared_vs_dedicated() {
    let mock_server = MockServer::start().await;

    let sse_body = [
        r#"data: {"id":"1","choices":[{"delta":{"content":"OK"}}]}"#,
        r#"data: {"id":"1","choices":[],"usage":{"prompt_tokens":5,"completion_tokens":5,"total_tokens":10}}"#,
        "data: [DONE]",
        "",
    ]
    .join("\n\n");

    Mock::given(wm_method("POST"))
        .and(wm_path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse_body, "text/event-stream"))
        .mount(&mock_server)
        .await;

    let (billing, auth) = setup_environment();

    // Group 1: Shared pool
    let group_shared = Group::pro_plus("grp-shared", "Shared Group");
    billing.upsert_group(group_shared);

    // Group 2: Dedicated
    let group_dedicated = Group::enterprise("grp-dedicated", "Dedicated Group", "DEDICATED", None);
    billing.upsert_group(group_dedicated);

    let card_shared = create_activated_card("card-shared", "grp-shared");
    billing.upsert_card(card_shared);
    let card_dedicated = create_activated_card("card-dedicated", "grp-dedicated");
    billing.upsert_card(card_dedicated);

    let token_shared = auth.issue_token_for_card("card-shared", 3600).unwrap();
    let token_dedicated = auth.issue_token_for_card("card-dedicated", 3600).unwrap();

    let provider = Arc::new(OpenAiProvider);

    // Configured with dedicated group_id = "grp-dedicated"
    let dedicated_config = ProviderConfig::new(
        mock_server.uri(),
        "sk-test",
        "gpt-4o",
        Duration::from_secs(5),
    )
    .with_group("grp-dedicated");

    let conv_handler = GenerateAssistantResponseHandler::new(
        reqwest::Client::new(),
        provider,
        dedicated_config,
        billing.clone(),
        IdempotencyManager::default(),
    );

    let mut registry = FacadeRegistry::new();
    registry.register(conv_handler);
    let app = registry.into_router_with_auth(auth);

    let req_body = serde_json::json!({
        "conversationState": {
            "conversationId": "conv-binding-test",
            "currentMessage": {
                "userInputMessage": {
                    "content": "Hello"
                }
            }
        }
    });

    // 1. Shared group attempts to call dedicated provider -> FORBIDDEN 403
    let (status_shared, json_shared) = send_req(
        app.clone(),
        Method::POST,
        "/generateAssistantResponse",
        &token_shared,
        Body::from(serde_json::to_vec(&req_body).unwrap()),
    )
    .await;
    assert_eq!(status_shared, StatusCode::FORBIDDEN);
    assert_eq!(json_shared["__type"], "AccessDeniedException");
    assert!(json_shared["message"]
        .as_str()
        .unwrap()
        .contains("cannot access provider"));

    // 2. Dedicated group calls its dedicated provider -> OK 200
    let (status_dedicated, _) = send_req(
        app.clone(),
        Method::POST,
        "/generateAssistantResponse",
        &token_dedicated,
        Body::from(serde_json::to_vec(&req_body).unwrap()),
    )
    .await;
    assert_eq!(status_dedicated, StatusCode::OK);
}

// With no usage from the upstream, input is billed from an estimate. It must be of what
// was actually sent, the group's system prompt prefix included, not of the Kiro request.
#[tokio::test]
async fn unreported_input_is_billed_on_the_translated_request() {
    let mock_server = MockServer::start().await;
    let sse_body = [
        r#"data: {"id":"1","choices":[{"delta":{"content":"ok"}}]}"#,
        "data: [DONE]",
        "",
    ]
    .join("\n\n");
    Mock::given(wm_method("POST"))
        .and(wm_path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse_body, "text/event-stream"))
        .mount(&mock_server)
        .await;

    let (billing, auth) = setup_environment();
    // About 4,000 tokens of prefix; the Kiro request itself is a few dozen.
    let prefix = "policy ".repeat(2_300);
    billing.upsert_group(Group::enterprise(
        "group-long-prefix",
        "Prefixed",
        "PREFIXED",
        Some(prefix),
    ));
    billing.upsert_card(create_activated_card(
        "card-long-prefix",
        "group-long-prefix",
    ));
    let token = auth.issue_token_for_card("card-long-prefix", 3600).unwrap();
    let handler = GenerateAssistantResponseHandler::new(
        reqwest::Client::new(),
        Arc::new(OpenAiProvider),
        ProviderConfig::new(
            mock_server.uri(),
            "sk-test-key",
            "gpt-4o",
            Duration::from_secs(5),
        )
        .with_group("group-long-prefix"),
        billing.clone(),
        IdempotencyManager::default(),
    );
    let mut registry = FacadeRegistry::new();
    registry.register(handler);
    let app = registry.into_router_with_auth(auth);
    let body = serde_json::json!({"conversationState": {
        "conversationId": "conv-long-prefix",
        "currentMessage": {"userInputMessage": {"content": "Write a function"}}
    }});
    let (status, _) = send_req(
        app,
        Method::POST,
        "/generateAssistantResponse",
        &token,
        Body::from(serde_json::to_vec(&body).unwrap()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let entries = billing.list_ledger_entries_for_card("card-long-prefix", None);
    assert_eq!(entries.len(), 1);
    assert!(
        entries[0].input_tokens >= 4_000,
        "billed {} input tokens for a ~4,000-token prompt",
        entries[0].input_tokens
    );
}
