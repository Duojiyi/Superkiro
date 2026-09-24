use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use billing::{BillingEngine, Card, CardStatus};
use gateway::{
    auth::AuthState,
    facade::{
        admin::{AdminAuthState, AdminCardAdjustHandler},
        provider_import::ProviderImportHandler,
        FacadeHandler,
    },
    provider::ProviderRuntimeRegistry,
};
use serde_json::json;
use std::{
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}
fn file(name: &str) -> std::path::PathBuf {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target/cloud-audit-tests")
        .join(format!(
            "{name}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("state.json")
}
fn engine() -> BillingEngine {
    let b = BillingEngine::new();
    let mut c = Card::new("card", "group", 10_000_000);
    c.status = CardStatus::Active;
    c.code_hash = billing::card::hash_card_code("secret");
    b.upsert_card(c);
    b
}

#[test]
fn rotation_survives_auth_and_engine_restart_and_retires_older_tokens() {
    let b = engine();
    let file = file("refresh");
    b.set_persistence_path(&file);
    let auth =
        AuthState::with_billing("local-test-signing-secret", b.clone()).with_refresh_grace(0);
    let old = auth
        .issue_refresh_token("card", "group", 1, 1, 3600)
        .unwrap();
    // Another token of the same family, as an earlier sign-in on the same device leaves.
    let sibling = auth
        .issue_refresh_token("card", "group", 1, 1, 3600)
        .unwrap();
    b.inject_persistence_fault(true);
    assert!(auth.rotate_refresh_token(&old, 300, 3600).is_err());
    b.inject_persistence_fault(false);
    let (_, access, next) = auth.rotate_refresh_token(&old, 300, 3600).unwrap();
    let recovered = BillingEngine::new();
    recovered.load_from_file(&file).unwrap();
    recovered.set_persistence_path(&file);
    let restored = AuthState::with_billing("local-test-signing-secret", recovered.clone())
        .with_refresh_grace(0);
    assert!(restored.rotate_refresh_token(&old, 300, 3600).is_err());
    assert!(restored.verify_token(&access).is_ok());
    // A card binds one device and has one refresh family: the rotation retired every
    // older token, which is what keeps the refresh state O(1) per card.
    assert!(restored.rotate_refresh_token(&sibling, 300, 3600).is_err());
    assert!(restored.rotate_refresh_token(&next, 300, 3600).is_ok());
    assert_eq!(recovered.get_card("card").unwrap().token_version, 1);
    // Fresh AuthState instances within the same second must not reuse JTIs.
    let a = AuthState::with_billing("local-test-signing-secret", recovered.clone());
    let b = AuthState::with_billing("local-test-signing-secret", recovered);
    assert_ne!(
        a.issue_refresh_token("card", "group", 1, 1, 3600).unwrap(),
        b.issue_refresh_token("card", "group", 1, 1, 3600).unwrap()
    );
}

fn adjustment(body: serde_json::Value, key: Option<&str>) -> Request<Body> {
    let mut req = Request::builder()
        .method("POST")
        .uri("/api/v1/admin/cards/adjust")
        .header("x-admin-key", "test-admin");
    if let Some(key) = key {
        req = req.header("idempotency-key", key);
    }
    req.body(Body::from(body.to_string())).unwrap()
}

#[tokio::test]
async fn adjustment_requires_intent_key_and_response_loss_replay_is_safe() {
    let b = engine();
    let path = file("adjust");
    b.set_persistence_path(&path);
    let auth = Arc::new(AdminAuthState::new("test-admin"));
    let handler = AdminCardAdjustHandler {
        billing: b.clone(),
        auth: auth.clone(),
    };
    let body = json!({"cardId":"card", "deltaPoints":1, "reason":"bonus"});
    for key in [None, Some(""), Some("invalid key")] {
        assert_eq!(
            handler.handle(adjustment(body.clone(), key)).await.status(),
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(b.export_snapshot().ledger.len(), 0);
    assert_eq!(
        handler
            .handle(adjustment(body.clone(), Some("intent-1")))
            .await
            .status(),
        StatusCode::OK
    );
    let recovered = BillingEngine::new();
    recovered.load_from_file(&path).unwrap();
    recovered.set_persistence_path(&path);
    let handler = AdminCardAdjustHandler {
        billing: recovered.clone(),
        auth,
    };
    assert_eq!(
        handler
            .handle(adjustment(body.clone(), Some("intent-1")))
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(recovered.export_snapshot().ledger.len(), 1);
    let balance = recovered.get_card("card").unwrap().credit_total;
    let mut changed_reason = body.clone();
    changed_reason["reason"] = json!("different purpose");
    assert_eq!(
        handler
            .handle(adjustment(changed_reason, Some("intent-1")))
            .await
            .status(),
        StatusCode::CONFLICT
    );
    assert!(recovered
        .adjust_balance_idempotent(
            "card",
            1_000_000,
            "another-operator",
            "bonus",
            now(),
            Some("intent-1")
        )
        .is_err());
    assert_eq!(recovered.get_card("card").unwrap().credit_total, balance);
    assert_eq!(recovered.export_snapshot().ledger.len(), 1);
    let mut conflict = body.clone();
    conflict["idempotencyKey"] = json!("different");
    assert_eq!(
        handler
            .handle(adjustment(conflict, Some("intent-1")))
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
    let mut snake = body;
    snake["idempotency_key"] = json!("intent-1");
    assert_eq!(
        handler.handle(adjustment(snake, None)).await.status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn import_preserves_group_in_committed_state_runtime_and_restart() {
    use billing::{
        crypto::MasterKek,
        provider::{Provider, ProviderFormat},
    };
    let b = engine();
    let path = file("import");
    let kek = MasterKek::from_hex(&"ab".repeat(32)).unwrap();
    b.set_master_kek(kek.clone());
    b.set_persistence_path(&path);
    let mut provider = Provider::new(
        "p",
        "old",
        ProviderFormat::OpenAi,
        "https://example.invalid/v1",
    );
    provider.group_id = Some("private-group".into());
    b.upsert_providers_checked(vec![provider], vec![]).unwrap();
    let runtime = ProviderRuntimeRegistry::new();
    runtime.sync_from_billing(&b);
    let handler = ProviderImportHandler::new()
        .with_billing(b.clone())
        .with_runtime(runtime.clone())
        .with_admin_auth(Arc::new(AdminAuthState::new("test-admin")));
    let content = json!({"providers":[{"id":"p", "name":"new", "apiType":"openai", "baseUrl":"https://example.invalid/v1", "apiKey":"local-test", "models":[{"id":"m"}]}]});
    let req = |body: serde_json::Value| {
        Request::builder()
            .method("POST")
            .uri("/api/v1/admin/providers/import")
            .header("x-admin-key", "test-admin")
            .body(Body::from(body.to_string()))
            .unwrap()
    };
    assert_eq!(
        handler
            .handle(req(
                json!({"content":content.clone(),"target_group_id":"other"})
            ))
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
    let mut malformed_target = content.clone();
    malformed_target["target_group_id"] = json!(123);
    assert_eq!(
        handler.handle(req(malformed_target)).await.status(),
        StatusCode::BAD_REQUEST
    );
    b.inject_persistence_fault(true);
    assert_eq!(
        handler.handle(req(content.clone())).await.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(runtime.pool_for("p").unwrap().provider().name, "old");
    b.inject_persistence_fault(false);
    assert_eq!(handler.handle(req(content)).await.status(), StatusCode::OK);
    assert_eq!(
        b.get_provider("p").unwrap().group_id.as_deref(),
        Some("private-group")
    );
    assert_eq!(
        runtime
            .pool_for("p")
            .unwrap()
            .provider()
            .group_id
            .as_deref(),
        Some("private-group")
    );
    let recovered = BillingEngine::new();
    recovered.set_master_kek(kek);
    recovered.load_from_file(&path).unwrap();
    assert_eq!(
        recovered.get_provider("p").unwrap().group_id.as_deref(),
        Some("private-group")
    );
}

#[tokio::test]
async fn cooldown_starts_at_failure_not_stale_request_start() {
    use billing::provider::{Provider, ProviderFormat, ProviderKey};
    use gateway::provider::{
        governance::{execute_stream_with_failover, ProviderKeyPool},
        ChatRequest,
    };
    use wiremock::matchers::{header, method};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(header("authorization", "Bearer bad"))
        .respond_with(ResponseTemplate::new(429))
        .mount(&server)
        .await;
    let pool = ProviderKeyPool::new(
        Provider::new("p", "test", ProviderFormat::OpenAi, server.uri()),
        vec![
            ProviderKey::new("bad", "p", "bad").with_weight(10),
            ProviderKey::new("good", "p", "good").with_weight(1),
        ],
    );
    let request = ChatRequest {
        model: "m".into(),
        messages: vec![],
        temperature: None,
        max_tokens: None,
        reasoning_effort: None,
        stream: true,
        tools: vec![],
    };
    let failed_after = now();
    // Controlled request start models a 65s elapsed request, without waiting or remote I/O.
    assert!(execute_stream_with_failover(
        &pool,
        &reqwest::Client::new(),
        "m",
        &request,
        Duration::from_secs(60),
        1,
        failed_after - 65
    )
    .await
    .is_err());
    let bad = pool
        .list_keys()
        .into_iter()
        .find(|k| k.id == "bad")
        .unwrap();
    assert!(bad.cooldown_until.unwrap() >= failed_after + 60);
    assert_eq!(pool.select_key(failed_after + 59, &[]).unwrap().id, "good");
}

#[tokio::test]
async fn public_login_and_portal_reject_stored_hash() {
    use gateway::facade::{oauth::OAuthTokenHandler, FacadeRegistry};
    let b = engine();
    let hash = b.get_card("card").unwrap().code_hash;
    let auth = AuthState::with_billing("local-test-signing-secret", b.clone());
    let login = OAuthTokenHandler::new(Arc::new(b.clone()), auth.clone());
    let req = Request::builder()
        .method("POST")
        .uri("/oauth/token")
        .body(Body::from(
            json!({"card_key": hash, "device_id":"device"}).to_string(),
        ))
        .unwrap();
    assert_eq!(login.handle(req).await.status(), StatusCode::UNAUTHORIZED);
    let mut registry = FacadeRegistry::new();
    registry.register_portal_facades(b, None);
    let app = registry.into_router_with_auth(auth);
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/portal/query")
        .body(Body::from(json!({"card":hash}).to_string()))
        .unwrap();
    let response = tower::ServiceExt::oneshot(app, req).await.unwrap();
    assert!(!response.status().is_success());
}

#[tokio::test]
async fn financials_reports_estimates_and_never_invents_cash_revenue() {
    use gateway::facade::admin::AdminFinancialsHandler;
    let b = engine();
    b.reserve(
        "card",
        "unpriced",
        &billing::ReservationEstimateParams::new(0, 1),
        100,
        660,
    )
    .unwrap();
    b.settle(
        "unpriced",
        &billing::UsageTokens {
            output_tokens: 1,
            ..Default::default()
        },
        "m",
        "p",
        "m",
        101,
    )
    .unwrap();
    let handler = AdminFinancialsHandler {
        billing: b,
        auth: Arc::new(AdminAuthState::new("test-admin")),
    };
    let req = Request::builder()
        .uri("/api/v1/admin/financials")
        .header("x-admin-key", "test-admin")
        .body(Body::empty())
        .unwrap();
    let response = handler.handle(req).await;
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), 65536)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(body["actualRevenueMicroCny"].is_null());
    assert!(body["actualGrossProfitMicroCny"].is_null());
    assert_eq!(body["estimates"]["uncostedRequests"], 1);
    assert!(body["estimates"]["faceValueLessCostMicroCny"].is_null());
    assert_eq!(
        body["basis"],
        "retained_usage_ledger_estimate_not_cash_revenue"
    );
}

#[tokio::test]
async fn usage_reports_spendable_balance_after_reservations() {
    use gateway::facade::{usage::GetUsageLimitsHandler, virtualization::VirtualizationStore};
    let b = engine();
    let mut card = b.get_card("card").unwrap();
    card.credit_total = 2_000_000_000;
    card.credit_used = 11_595_800;
    card.credit_reserved = 3_000_000;
    b.upsert_card(card);
    let handler = GetUsageLimitsHandler::new(VirtualizationStore::with_billing(b, "group"));
    for (card_id, expected) in [
        ("card", json!(1985.4042)),
        ("missing", serde_json::Value::Null),
    ] {
        let mut req = Request::builder()
            .uri("/getUsageLimits")
            .body(Body::empty())
            .unwrap();
        req.extensions_mut().insert(gateway::auth::AuthClaims {
            card_id: card_id.into(),
            group_id: "group".into(),
            token_version: 1,
            exp: now() + 60,
            iat: now(),
        });
        let response = handler.handle(req).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["cache-control"], "no-store");
        let bytes = axum::body::to_bytes(response.into_body(), 65536)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["availableCredits"], expected);
        let credit = &body["usageBreakdownList"][0];
        assert_eq!(credit["displayName"], "Credit");
        assert_eq!(credit["displayNamePlural"], "Credits");
        if card_id == "card" {
            assert_eq!(credit["currentUsageWithPrecision"], json!(11.5958));
            assert_eq!(credit["usageLimitWithPrecision"], json!(2000.0));
        }
    }
}

#[test]
fn hidden_mappings_do_not_restore_environment_fallback() {
    use gateway::facade::virtualization::VirtualizationStore;
    let b = engine();
    b.upsert_group(billing::group::Group::pro_plus("group", "test"));
    let store = VirtualizationStore::with_billing(b.clone(), "group");
    store.set_fallback_model("environment-F");
    assert_eq!(
        store.get_group(Some("group")).models[0].model_id,
        "environment-F"
    );
    let mut mapping = billing::group::ModelMap::new("map", "group", "M", "p", "upstream");
    mapping.visible = false;
    b.upsert_model_map(mapping.clone());
    let hidden = store.get_group(Some("group"));
    assert!(hidden.models.is_empty());
    assert!(hidden.default_model_id.is_empty());
    mapping.visible = true;
    b.upsert_model_map(mapping);
    let visible = store.get_group(Some("group"));
    assert_eq!(visible.models.len(), 1);
    assert_eq!(visible.default_model_id, "M");
}
