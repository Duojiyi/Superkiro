//! Production lifecycle test for provider import, runtime routing, status toggling, and crash recovery (Spec §14.3, P4-6, T04).

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use billing::card::Card;
use billing::crypto::MasterKek;
use billing::engine::BillingEngine;
use gateway::auth::AuthClaims;
use gateway::facade::admin::AdminAuthState;
use gateway::facade::conversation::GenerateAssistantResponseHandler;
use gateway::facade::provider_import::ProviderImportResponse;
use gateway::facade::virtualization::VirtualizationStore;
use gateway::facade::FacadeRegistry;
use gateway::provider::ProviderRuntimeRegistry;
use serde_json::json;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tower::ServiceExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod support;

fn create_auth_claims(card_id: &str) -> AuthClaims {
    AuthClaims {
        card_id: card_id.to_string(),
        group_id: "group-default".to_string(),
        token_version: 1,
        exp: 9999999999,
        iat: 1000000000,
    }
}

fn create_kiro_conversation_payload(prompt: &str, model_id: &str) -> serde_json::Value {
    json!({
        "conversationState": {
            "conversationId": "conv-t04-lifecycle",
            "currentMessage": {
                "userInputMessage": {
                    "content": prompt,
                    "modelId": model_id
                }
            }
        }
    })
}

#[tokio::test]
async fn test_provider_import_to_real_request_and_lifecycle_loop() {
    // 1. Setup mock upstream OpenAI provider
    let mock_server = MockServer::start().await;

    let openai_sse_chunks = [
        "data: {\"id\":\"chatcmpl-1\",\"object\":\"chat.completion.chunk\",\"created\":1700000000,\"model\":\"gpt-4o\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"\"},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"chatcmpl-1\",\"object\":\"chat.completion.chunk\",\"created\":1700000000,\"model\":\"gpt-4o\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hello from imported upstream!\"},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"chatcmpl-1\",\"object\":\"chat.completion.chunk\",\"created\":1700000000,\"model\":\"gpt-4o\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":5,\"total_tokens\":15}}\n\n",
        "data: [DONE]\n\n"
    ].concat();

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(openai_sse_chunks),
        )
        .mount(&mock_server)
        .await;

    // 2. Setup persistent BillingEngine with MasterKEK in temporary directory
    let temp_dir = std::env::temp_dir().join(format!(
        "kiro_test_t04_{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&temp_dir).unwrap();
    let state_file = temp_dir.join("billing_state.json");

    let kek_bytes = [7u8; 32];
    let kek = MasterKek::from_bytes(kek_bytes);

    let billing = BillingEngine::new();
    billing.upsert_rate_card_version(support::wildcard_price("default"));
    billing.set_persistence_path(&state_file);
    billing.set_master_kek(kek.clone());

    // Create and activate funded test card
    let initial_credits = 50_000_000i64;
    let mut card = Card::new("card-t04-test", "group-default", initial_credits);
    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    card.activate(now_secs, 86400 * 30).unwrap();
    billing.upsert_card(card);

    // 3. Setup ProviderRuntimeRegistry & FacadeRegistry with router
    let runtime = ProviderRuntimeRegistry::new();
    runtime.sync_from_billing(&billing);

    let store = VirtualizationStore::with_billing(billing.clone(), "group-default");
    let admin_auth = Arc::new(AdminAuthState::new("admin-secret-token-t04".to_string()));

    let mut registry = FacadeRegistry::new().with_runtime(runtime.clone());
    registry.register_virtualized_facades(store.clone());

    let conv_handler = GenerateAssistantResponseHandler {
        client: reqwest::Client::new(),
        provider: None,
        provider_config: None,
        pool: None,
        runtime: Some(runtime.clone()),
        fallback_pools: std::collections::HashMap::new(),
        billing: billing.clone(),
        idempotency: Default::default(),
        guardrail: Default::default(),
        card_rate_limiter: gateway::ops::CardRateLimiter::new(100),
        intercept_intent: false,
        vision_config: None,
        vision_cache: Default::default(),
        content_guardrail: Default::default(),
    };
    registry.register(conv_handler);

    // Register admin & import surfaces
    let mut import_handler = gateway::facade::provider_import::ProviderImportHandler::new()
        .with_store(store.clone())
        .with_billing(billing.clone())
        .with_admin_auth(admin_auth.clone());
    import_handler = import_handler.with_runtime(runtime.clone());
    registry.register(import_handler);

    registry.register(gateway::facade::admin::AdminProviderStatusHandler {
        billing: billing.clone(),
        auth: admin_auth.clone(),
        runtime: Some(runtime.clone()),
    });

    let app = registry.into_router();

    // 4. Import provider via POST /api/v1/admin/providers/import
    let import_payload = json!({
        "format": "cherry_studio",
        "content": {
            "version": 1,
            "providers": [
                {
                    "id": "mock-openai-provider",
                    "name": "Mock OpenAI Upstream",
                    "apiType": "openai",
                    "baseUrl": format!("{}/v1", mock_server.uri()),
                    "apiKey": "sk-mock-key-12345",
                    "enabled": true,
                    "models": [
                        { "id": "gpt-4o", "name": "GPT-4o" }
                    ]
                }
            ]
        }
    });

    let import_req = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/admin/providers/import")
        .header(header::AUTHORIZATION, "Bearer admin-secret-token-t04")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(import_payload.to_string()))
        .unwrap();

    let resp = app.clone().oneshot(import_req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body_bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .unwrap();
    let import_res: ProviderImportResponse = serde_json::from_slice(&body_bytes).unwrap();
    assert!(import_res.success);
    assert_eq!(import_res.imported_count, 1);

    // Verify runtime registry and billing persistence have the imported provider
    assert!(runtime.has_available_provider());
    assert_eq!(runtime.provider_ids(), vec!["mock-openai-provider"]);
    let providers = billing.list_providers();
    assert_eq!(providers.len(), 1);
    assert_eq!(providers[0].id, "mock-openai-provider");
    assert!(providers[0].enabled);

    // Explicit operator publication, separate from credential import.
    billing.upsert_group(billing::group::Group::pro_plus("group-default", "Test"));
    billing.upsert_model_map(billing::group::ModelMap::new(
        "test-map",
        "group-default",
        "gpt-4o",
        "mock-openai-provider",
        "gpt-4o",
    ));

    // 5. Issue real conversation request -> should route to imported provider!
    let conv_payload = create_kiro_conversation_payload("Say hello", "gpt-4o");
    let mut conv_req = Request::builder()
        .method(Method::POST)
        .uri("/generateAssistantResponse")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, "Bearer test-user-token")
        .body(Body::from(conv_payload.to_string()))
        .unwrap();
    conv_req
        .extensions_mut()
        .insert(create_auth_claims("card-t04-test"));

    let conv_resp = app.clone().oneshot(conv_req).await.unwrap();
    assert_eq!(conv_resp.status(), StatusCode::OK);
    assert_eq!(
        conv_resp.headers().get(header::CONTENT_TYPE).unwrap(),
        "application/vnd.amazon.eventstream"
    );

    // 6. Disable provider via POST /api/v1/admin/providers/status
    let disable_payload = json!({
        "providerId": "mock-openai-provider",
        "enabled": false
    });
    let disable_req = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/admin/providers/status")
        .header(header::AUTHORIZATION, "Bearer admin-secret-token-t04")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(disable_payload.to_string()))
        .unwrap();

    let disable_resp = app.clone().oneshot(disable_req).await.unwrap();
    assert_eq!(disable_resp.status(), StatusCode::OK);

    // Verify runtime is immediately synced to disabled
    assert!(!runtime.has_available_provider());

    // 7. Request with disabled provider must FAIL CLOSED (503 Service Unavailable, no silent hijacking)
    let conv_payload_disabled = create_kiro_conversation_payload("Are you there?", "gpt-4o");
    let mut conv_req_disabled = Request::builder()
        .method(Method::POST)
        .uri("/generateAssistantResponse")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, "Bearer test-user-token")
        .body(Body::from(conv_payload_disabled.to_string()))
        .unwrap();
    conv_req_disabled
        .extensions_mut()
        .insert(create_auth_claims("card-t04-test"));

    let conv_resp_disabled = app.clone().oneshot(conv_req_disabled).await.unwrap();
    assert_eq!(conv_resp_disabled.status(), StatusCode::SERVICE_UNAVAILABLE);

    // 8. Re-enable provider via POST /api/v1/admin/providers/status
    let enable_payload = json!({
        "providerId": "mock-openai-provider",
        "enabled": true
    });
    let enable_req = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/admin/providers/status")
        .header(header::AUTHORIZATION, "Bearer admin-secret-token-t04")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(enable_payload.to_string()))
        .unwrap();

    let enable_resp = app.clone().oneshot(enable_req).await.unwrap();
    assert_eq!(enable_resp.status(), StatusCode::OK);
    assert!(runtime.has_available_provider());

    // 9. Re-enabled provider successfully routes again
    let conv_payload_re = create_kiro_conversation_payload("Back online?", "gpt-4o");
    let mut conv_req_re = Request::builder()
        .method(Method::POST)
        .uri("/generateAssistantResponse")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, "Bearer test-user-token")
        .body(Body::from(conv_payload_re.to_string()))
        .unwrap();
    conv_req_re
        .extensions_mut()
        .insert(create_auth_claims("card-t04-test"));

    let conv_resp_re = app.clone().oneshot(conv_req_re).await.unwrap();
    assert_eq!(conv_resp_re.status(), StatusCode::OK);

    // 10. Cold Restart verification: load fresh BillingEngine from disk snapshot
    let reboot_billing = BillingEngine::new();
    reboot_billing.set_persistence_path(&state_file);
    reboot_billing.set_master_kek(kek);
    reboot_billing.load_from_file(&state_file).unwrap();

    let reboot_runtime = ProviderRuntimeRegistry::new();
    reboot_runtime.sync_from_billing(&reboot_billing);

    assert!(reboot_runtime.has_available_provider());
    assert_eq!(reboot_runtime.provider_ids(), vec!["mock-openai-provider"]);
    let reboot_pool = reboot_runtime.pool_for("mock-openai-provider").unwrap();
    assert!(reboot_pool.provider().enabled);
    assert_eq!(reboot_pool.list_keys().len(), 1);
    assert_eq!(reboot_pool.list_keys()[0].api_key, "sk-mock-key-12345");

    let _ = std::fs::remove_dir_all(&temp_dir);
}
