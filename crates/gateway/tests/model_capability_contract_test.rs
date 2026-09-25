//! Configured capability contract from publication through reservation and streaming.

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
async fn configured_capabilities_reach_catalog_upstream_billing_and_usage() {
    for (output, credits, quota) in [
        (128_000, 1_000_000_000, "none"),
        (10_000_000, 1_000_000_000, "none"),
        (128_000, 3_000_000, "none"),
        (128_000, 1_000_000_000, "daily"),
        (128_000, 1_000_000_000, "monthly"),
    ] {
        let mock_server = MockServer::start().await;

        let sse_body = [
        r#"data: {"id":"1","choices":[{"delta":{"content":"Hello from aliased model"}}]}"#,
        r#"data: {"id":"1","choices":[],"usage":{"prompt_tokens":100000,"completion_tokens":6000,"total_tokens":106000}}"#,
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
        let mut model_map = ModelMap::new(
            "mm-1",
            "group-alias-1",
            "claude-3-7-sonnet",
            "provider-1",
            "claude-3-7-sonnet-upstream",
        )
        .with_alias("claude-3.7-sonnet");
        model_map.context_window = if output == 10_000_000 {
            10_000_000
        } else {
            1_000_000
        };
        model_map.max_output = output;
        let mut card = billing.get_card("card-alias-1").unwrap();
        card.credit_total = credits;
        card.daily_credit_limit = (quota == "daily").then_some(3_000_000);
        card.monthly_credit_limit = (quota == "monthly").then_some(3_000_000);
        billing.upsert_card(card);

        let provider = Provider::new(
            "provider-1",
            "OpenAI Compatible",
            ProviderFormat::OpenAi,
            mock_server.uri(),
        );
        let key = ProviderKey::new("key-1", "provider-1", "sk-test");
        billing.upsert_provider(provider.clone());
        billing.upsert_provider_key(key.clone());
        for (context, max_output) in [
            (0, 1),
            (1_000_000, 0),
            (1_000_000, 1_000_001),
            (10_000_001, 1),
        ] {
            let mut invalid = model_map.clone();
            invalid.context_window = context;
            invalid.max_output = max_output;
            assert!(billing
                .publish_commercial_config(
                    billing::engine::CommercialUpdate {
                        expected_revision: billing.commercial_config().revision,
                        reason: "invalid capabilities".into(),
                        settings: None,
                        groups: vec![],
                        models: vec![invalid],
                        rate_cards: vec![],
                        versions: vec![],
                    },
                    1_700_000_000
                )
                .is_err());
        }
        let config = billing
            .publish_commercial_config(
                billing::engine::CommercialUpdate {
                    expected_revision: billing.commercial_config().revision,
                    reason: "capability regression".into(),
                    settings: None,
                    groups: vec![],
                    models: vec![model_map.clone()],
                    rate_cards: vec![],
                    versions: vec![],
                },
                1_700_000_000,
            )
            .unwrap();
        assert_eq!(config.models[0].max_output, output);
        let store = gateway::facade::virtualization::VirtualizationStore::with_billing(
            billing.clone(),
            "group-alias-1",
        );
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
        registry.register(gateway::facade::models::ListAvailableModelsHandler::new(
            store,
        ));
        let app = registry.into_router_with_auth(auth);

        let catalog = tower::ServiceExt::oneshot(
            app.clone(),
            Request::builder()
                .uri("/ListAvailableModels")
                .header(header::AUTHORIZATION, format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(catalog.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(catalog.into_body(), usize::MAX)
            .await
            .unwrap();
        let catalog: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            catalog["models"][0]["tokenLimits"]["maxOutputTokens"],
            output
        );
        assert_eq!(
            catalog["models"][0]["tokenLimits"]["maxInputTokens"],
            model_map.context_window
        );

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
        if credits == 3_000_000 || quota != "none" {
            // Enough for the old 32K cap, insufficient for the configured 128K budget.
            assert_eq!(
                resp.status(),
                if quota == "none" {
                    StatusCode::PAYMENT_REQUIRED
                } else {
                    StatusCode::TOO_MANY_REQUESTS
                }
            );
            assert!(mock_server.received_requests().await.unwrap().is_empty());
            assert_eq!(billing.get_card("card-alias-1").unwrap().credit_reserved, 0);
            continue;
        }
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let mut decoder = kiro_wire::decoder::EventStreamDecoder::new();
        decoder.feed(&bytes).unwrap();
        let (frames, _) = decoder.decode_all();
        let event = frames
            .iter()
            .find(|f| f.event_type() == Some("contextUsageEvent"))
            .unwrap();
        let usage: kiro_wire::events::ContextUsageEvent = event.payload_as_json().unwrap();
        assert!(
            (usage.context_usage_percentage - 106_000.0 / model_map.context_window as f64).abs()
                < 1e-6
        );
        assert_eq!(billing.get_card("card-alias-1").unwrap().credit_reserved, 0);

        // Verify upstream received the resolved target model
        let requests = mock_server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        let req_val: Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(req_val["model"], "claude-3-7-sonnet-upstream");
        assert_eq!(req_val["max_tokens"], output);
    }
}

#[test]
fn unknown_environment_model_catalog_is_conservative() {
    let store = gateway::facade::virtualization::VirtualizationStore::with_billing(
        BillingEngine::default(),
        "fallback-group",
    );
    store
        .billing()
        .unwrap()
        .upsert_group(Group::pro_plus("fallback-group", "Fallback"));
    store.set_fallback_model("gemini-future-custom");
    let group = store.get_group(None);
    let limits = group.models[0].token_limits.as_ref().unwrap();
    assert_eq!(limits.max_input_tokens, 128_000);
    assert_eq!(limits.max_output_tokens, 4096);
}
