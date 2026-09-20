//! Integration tests for P1-10: Tenant group virtualization of models, plans, quotas, and profiles.
//!
//! Spec §1.4, §4.2, P0-5.

use axum::{
    body::Body,
    http::{header, Method, Request, StatusCode},
};
use gateway::auth::{AuthState, CardRecord};
use gateway::facade::models::{ListAvailableModelsResponse, ModelInfo, TokenLimits};
use gateway::facade::profiles::ListAvailableProfilesResponse;
use gateway::facade::subscriptions::ListAvailableSubscriptionsResponse;
use gateway::facade::usage::GetUsageLimitsResponse;
use gateway::facade::virtualization::{VirtualGroup, VirtualizationStore};
use gateway::facade::FacadeRegistry;
use http_body_util::BodyExt;
use tower::ServiceExt;

fn create_test_app(store: VirtualizationStore, auth: AuthState) -> axum::Router {
    let mut registry = FacadeRegistry::new();
    registry.register_virtualized_facades(store);
    let api_router = registry.into_router();

    // Wrap with auth middleware
    axum::Router::new()
        .merge(api_router)
        .layer(axum::middleware::from_fn_with_state(
            auth.clone(),
            gateway::auth::auth_middleware,
        ))
}

#[tokio::test]
async fn test_virtualization_spec_endpoints_and_schemas() {
    let auth = AuthState::new("secret-key-32bytes-for-test-12345");
    auth.upsert_card(CardRecord {
        card_id: "card-free-to-pro".to_string(),
        group_id: "group-pro-plus".to_string(),
        current_token_version: 1,
        is_active: true,
    });
    let token = auth
        .issue_token("card-free-to-pro", "group-pro-plus", 1, 3600)
        .unwrap();

    let store = VirtualizationStore::default();
    let app = create_test_app(store, auth);

    // 1. GET /ListAvailableModels
    let req = Request::builder()
        .method(Method::GET)
        .uri("/ListAvailableModels")
        .header(header::AUTHORIZATION, format!("Bearer {}", token))
        .body(Body::empty())
        .unwrap();

    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let models_resp: ListAvailableModelsResponse =
        serde_json::from_slice(&body).expect("Valid ListAvailableModelsResponse");
    assert!(!models_resp.models.is_empty());
    assert_eq!(models_resp.default_model, "claude-sonnet-4.5");

    // 2. GET /getUsageLimits
    let req = Request::builder()
        .method(Method::GET)
        .uri("/getUsageLimits")
        .header(header::AUTHORIZATION, format!("Bearer {}", token))
        .body(Body::empty())
        .unwrap();

    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let usage_resp: GetUsageLimitsResponse =
        serde_json::from_slice(&body).expect("Valid GetUsageLimitsResponse");
    assert_eq!(
        usage_resp.subscription_info.subscription_title,
        "Legacy service plan"
    );
    assert_eq!(usage_resp.overage_configuration.overage_status, "DISABLED");
    assert!(usage_resp.usage_breakdown_list[0].usage_limit > 0.0);

    // 3. POST /listAvailableSubscriptions
    let req = Request::builder()
        .method(Method::POST)
        .uri("/listAvailableSubscriptions")
        .header(header::AUTHORIZATION, format!("Bearer {}", token))
        .body(Body::empty())
        .unwrap();

    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let sub_resp: ListAvailableSubscriptionsResponse =
        serde_json::from_slice(&body).expect("Valid ListAvailableSubscriptionsResponse");
    assert_eq!(sub_resp.subscription_plans[0].q_subscription_type, "CUSTOM");

    // 4. POST /ListAvailableProfiles
    let req = Request::builder()
        .method(Method::POST)
        .uri("/ListAvailableProfiles")
        .header(header::AUTHORIZATION, format!("Bearer {}", token))
        .body(Body::empty())
        .unwrap();

    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let profile_resp: ListAvailableProfilesResponse =
        serde_json::from_slice(&body).expect("Valid ListAvailableProfilesResponse");
    assert!(!profile_resp.profiles.is_empty());
    assert!(profile_resp.profiles[0]
        .arn
        .starts_with("arn:aws:codewhisperer:"));
}

#[tokio::test]
async fn test_audit_b_group_isolation_custom_models_and_plan() {
    let auth = AuthState::new("secret-key-32bytes-for-test-12345");
    auth.upsert_card(CardRecord {
        card_id: "card-user-std".to_string(),
        group_id: "group-standard".to_string(),
        current_token_version: 1,
        is_active: true,
    });
    auth.upsert_card(CardRecord {
        card_id: "card-user-vip".to_string(),
        group_id: "group-vip-enterprise".to_string(),
        current_token_version: 1,
        is_active: true,
    });

    let token_std = auth
        .issue_token("card-user-std", "group-standard", 1, 3600)
        .unwrap();
    let token_vip = auth
        .issue_token("card-user-vip", "group-vip-enterprise", 1, 3600)
        .unwrap();

    let store = VirtualizationStore::new("group-standard");

    // Group Standard (Basic plan, only DeepSeek)
    store.upsert_group(VirtualGroup {
        group_id: "group-standard".to_string(),
        group_name: "Standard Group".to_string(),
        virtual_plan_name: "KIRO STANDARD".to_string(),
        virtual_usage_limit: 10_000.0,
        current_usage: 500.0,
        profile_arn: "arn:aws:codewhisperer:us-east-1:111111111111:profile/STANDARD".to_string(),
        default_model_id: "deepseek-chat".to_string(),
        models: vec![ModelInfo {
            model_id: "deepseek-chat".to_string(),
            model_name: Some("DeepSeek V3".to_string()),
            description: Some("Standard coding model".to_string()),
            token_limits: Some(TokenLimits {
                max_input_tokens: 64_000,
                max_output_tokens: 4_096,
            }),
            supports_reasoning: false,
            supports_vision: false,
            default_effort_level: None,
        }],
        provider_binding_mode: billing::group::ProviderBindingMode::Shared,
        system_prompt_prefix: None,
    });

    // Group VIP (Unlimited VIP plan, Claude Sonnet 4.5 + Opus)
    store.upsert_group(VirtualGroup {
        group_id: "group-vip-enterprise".to_string(),
        group_name: "VIP Enterprise Group".to_string(),
        virtual_plan_name: "KIRO ENTERPRISE ULTRA".to_string(),
        virtual_usage_limit: 1_000_000.0,
        current_usage: 2500.0,
        profile_arn: "arn:aws:codewhisperer:us-east-1:999999999999:profile/ENTERPRISE".to_string(),
        default_model_id: "claude-sonnet-4.5".to_string(),
        models: vec![
            ModelInfo {
                model_id: "claude-sonnet-4.5".to_string(),
                model_name: Some("Claude Sonnet 4.5 VIP".to_string()),
                description: Some("Dedicated enterprise model".to_string()),
                token_limits: Some(TokenLimits {
                    max_input_tokens: 200_000,
                    max_output_tokens: 64_000,
                }),
                supports_reasoning: true,
                supports_vision: true,
                default_effort_level: Some("max".to_string()),
            },
            ModelInfo {
                model_id: "claude-opus-4.8".to_string(),
                model_name: Some("Claude Opus 4.8 Ultra".to_string()),
                description: Some("Ultimate flagship intelligence".to_string()),
                token_limits: Some(TokenLimits {
                    max_input_tokens: 1_000_000,
                    max_output_tokens: 128_000,
                }),
                supports_reasoning: true,
                supports_vision: true,
                default_effort_level: Some("max".to_string()),
            },
        ],
        provider_binding_mode: billing::group::ProviderBindingMode::Shared,
        system_prompt_prefix: None,
    });

    let app = create_test_app(store, auth);

    // Verify Standard User receives Standard Plan & Models
    let req_std_models = Request::builder()
        .method(Method::GET)
        .uri("/ListAvailableModels")
        .header(header::AUTHORIZATION, format!("Bearer {}", token_std))
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req_std_models).await.unwrap();
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let models: ListAvailableModelsResponse = serde_json::from_slice(&body).unwrap();
    assert_eq!(models.models.len(), 1);
    assert_eq!(models.models[0].model_id, "deepseek-chat");
    assert_eq!(models.default_model, "deepseek-chat");

    let req_std_usage = Request::builder()
        .method(Method::GET)
        .uri("/getUsageLimits")
        .header(header::AUTHORIZATION, format!("Bearer {}", token_std))
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req_std_usage).await.unwrap();
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let usage: GetUsageLimitsResponse = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        usage.subscription_info.subscription_title,
        "Legacy service plan"
    );
    assert_eq!(usage.usage_breakdown_list[0].usage_limit, 10_000.0);
    assert_eq!(usage.usage_breakdown_list[0].current_usage, 500.0);

    // Verify VIP User receives Enterprise Plan & Models
    let req_vip_models = Request::builder()
        .method(Method::GET)
        .uri("/ListAvailableModels")
        .header(header::AUTHORIZATION, format!("Bearer {}", token_vip))
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req_vip_models).await.unwrap();
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let models_vip: ListAvailableModelsResponse = serde_json::from_slice(&body).unwrap();
    assert_eq!(models_vip.models.len(), 2);
    assert_eq!(models_vip.models[0].model_id, "claude-sonnet-4.5");
    assert_eq!(models_vip.models[1].model_id, "claude-opus-4.8");
    assert_eq!(models_vip.default_model, "claude-sonnet-4.5");

    let req_vip_usage = Request::builder()
        .method(Method::GET)
        .uri("/getUsageLimits")
        .header(header::AUTHORIZATION, format!("Bearer {}", token_vip))
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req_vip_usage).await.unwrap();
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let usage_vip: GetUsageLimitsResponse = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        usage_vip.subscription_info.subscription_title,
        "Legacy service plan"
    );
    assert_eq!(usage_vip.usage_breakdown_list[0].usage_limit, 1_000_000.0);
    assert_eq!(usage_vip.usage_breakdown_list[0].current_usage, 2500.0);
}

#[tokio::test]
async fn test_audit_b_p0_5_minimum_required_fields_non_empty() {
    let auth = AuthState::with_default_dev_card();
    let token = auth
        .issue_token("card-dev-001", "group-pro-plus", 1, 3600)
        .unwrap();
    let store = VirtualizationStore::default();
    let app = create_test_app(store, auth);

    // 1. ListAvailableModels must contain models array with modelId and defaultModel
    let req = Request::builder()
        .method(Method::GET)
        .uri("/ListAvailableModels")
        .header(header::AUTHORIZATION, format!("Bearer {}", token))
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let body: serde_json::Value =
        serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let models_arr = body["models"].as_array().expect("models must be array");
    assert!(!models_arr.is_empty(), "models array cannot be empty");
    for m in models_arr {
        assert!(
            m["modelId"].as_str().is_some() && !m["modelId"].as_str().unwrap().is_empty(),
            "each model must have non-empty modelId"
        );
    }
    assert!(
        body["defaultModel"].as_str().is_some(),
        "defaultModel must be present"
    );

    // 2. getUsageLimits must have usageLimit > 0.0 (anti-NaN% / division by zero UI crash)
    let req = Request::builder()
        .method(Method::GET)
        .uri("/getUsageLimits")
        .header(header::AUTHORIZATION, format!("Bearer {}", token))
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let body: serde_json::Value =
        serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let usage_list = body["usageBreakdownList"].as_array().unwrap();
    assert!(!usage_list.is_empty());
    let limit = usage_list[0]["usageLimit"].as_f64().unwrap();
    assert!(
        limit > 0.0,
        "usageLimit must be strictly greater than zero to prevent UI NaN crash"
    );
    assert!(
        body["subscriptionInfo"]["subscriptionTitle"]
            .as_str()
            .is_some(),
        "subscriptionTitle must be present"
    );

    // 3. ListAvailableProfiles must contain >=1 profile with valid ARN structure
    let req = Request::builder()
        .method(Method::POST)
        .uri("/ListAvailableProfiles")
        .header(header::AUTHORIZATION, format!("Bearer {}", token))
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let body: serde_json::Value =
        serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let profiles = body["profiles"].as_array().unwrap();
    assert!(
        !profiles.is_empty(),
        "profiles cannot be empty or Kiro ProfileArnGuard blocks chat"
    );
    let arn = profiles[0]["arn"].as_str().unwrap();
    assert!(
        arn.starts_with("arn:aws:codewhisperer:"),
        "Profile ARN must be valid AWS ARN format"
    );
}

#[tokio::test]
async fn test_audit_b_reasoning_models_expose_effort_schema() {
    let auth = AuthState::with_default_dev_card();
    let token = auth
        .issue_token("card-dev-001", "group-pro-plus", 1, 3600)
        .unwrap();
    let store = VirtualizationStore::default();
    let app = create_test_app(store, auth);

    let req = Request::builder()
        .method(Method::GET)
        .uri("/ListAvailableModels")
        .header(header::AUTHORIZATION, format!("Bearer {}", token))
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    let body: ListAvailableModelsResponse =
        serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();

    // Claude Sonnet 4.5 has reasoning enabled
    let sonnet = body
        .models
        .iter()
        .find(|m| m.model_id == "claude-sonnet-4.5")
        .unwrap();
    let schema = sonnet
        .additional_model_request_fields_schema
        .as_ref()
        .expect("Reasoning model must expose additionalModelRequestFieldsSchema");
    let effort_enum = schema["properties"]["output_config"]["properties"]["effort"]["enum"]
        .as_array()
        .unwrap();
    assert_eq!(effort_enum.len(), 5); // low, medium, high, xhigh, max
    assert_eq!(sonnet.default_effort_level.as_deref(), Some("high"));

    // DeepSeek V3 does not have reasoning enabled
    let deepseek = body
        .models
        .iter()
        .find(|m| m.model_id == "deepseek-chat")
        .unwrap();
    assert!(
        deepseek.additional_model_request_fields_schema.is_none(),
        "Non-reasoning model must not expose effort schema"
    );
}

#[tokio::test]
async fn test_audit_b_get_usage_limits_reflects_live_card_credits_and_plan() {
    let billing = billing::engine::BillingEngine::new();
    let card_id = "card-limits-test-01";
    let group_id = "group-limits";

    // Setup group with custom virtual plan name and 50.0 credit limit
    let mut group = billing::group::Group::pro_plus(group_id, "Limits Test Group");
    group.virtual_plan_name = "KIRO ENTERPRISE PRO".to_string();
    group.virtual_usage_limit = 999_999.0; // Must not replace the purchased card limit.
    billing.upsert_group(group);

    // Setup card with 50 credits (50_000_000 micro-credits) and 12.5 credits used (12_500_000 micro-credits)
    let mut card =
        billing::card::Card::new(card_id, group_id, 50 * billing::MICRO_CREDITS_PER_CREDIT);
    card.credit_used = 12_500_000;
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    card.activate(now_secs, 86400 * 30).unwrap();
    billing.upsert_card(card);

    let auth = AuthState::with_billing("test-secret-32-bytes-long-key-!", billing.clone());
    let token = auth.issue_token(card_id, group_id, 1, 3600).unwrap();

    let store = VirtualizationStore::with_billing(billing, group_id);
    let app = create_test_app(store, auth);

    let req = Request::builder()
        .method(Method::GET)
        .uri("/getUsageLimits")
        .header(header::AUTHORIZATION, format!("Bearer {}", token))
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body: GetUsageLimitsResponse =
        serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();

    // Spec §14.10.4:
    // A legacy/custom card does not inherit a misleading group subscription name.
    assert_eq!(
        body.subscription_info.subscription_title,
        "Legacy service plan"
    );

    // Limits reflect live card balances in credits
    let breakdown = &body.usage_breakdown_list[0];
    assert_eq!(breakdown.usage_limit, 50.0);
    assert_eq!(breakdown.current_usage, 12.5);
    assert_eq!(breakdown.display_name, "Credit");
    assert_eq!(breakdown.display_name_plural, "Credits");
}
