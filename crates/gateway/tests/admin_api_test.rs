//! Integration tests for Admin REST API endpoints (P0-01, P0-02, P1-02).

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use billing::card::{Card, CardStatus};
use billing::engine::BillingEngine;
use gateway::facade::FacadeRegistry;
use serde_json::json;

const TEST_ADMIN_KEY: &str = "test-super-secret-admin-key-32chars!!";

fn setup_admin_app() -> (BillingEngine, axum::Router) {
    let billing = BillingEngine::default();
    let card1 = Card::new("card-admin-01", "group-admin", 10_000_000);
    billing.upsert_card(card1);

    let mut card2 = Card::new("card-admin-02", "group-admin", 5_000_000);
    card2.status = CardStatus::Active;
    billing.upsert_card(card2);

    let mut registry = FacadeRegistry::new();
    registry.register_default_facades();
    registry.register_admin_facades(billing.clone(), TEST_ADMIN_KEY.to_string());

    let auth = gateway::auth::AuthState::new("test-auth-secret-key-32bytes-long-ok!!");
    let app = registry.into_router_with_auth(auth);

    (billing, app)
}

#[tokio::test]
async fn test_admin_unauthorized_without_valid_key() {
    let (_billing, app) = setup_admin_app();

    // 1. Missing header -> 401
    let req = Request::builder()
        .method(Method::GET)
        .uri("/api/v1/admin/me")
        .body(Body::empty())
        .unwrap();

    let resp = tower::ServiceExt::oneshot(app.clone(), req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    // 2. Wrong key -> 401
    let req = Request::builder()
        .method(Method::GET)
        .uri("/api/v1/admin/me")
        .header("x-admin-key", "wrong-key")
        .body(Body::empty())
        .unwrap();

    let resp = tower::ServiceExt::oneshot(app, req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_admin_stats_and_cards_query() {
    let (_billing, app) = setup_admin_app();

    // 1. GET /api/v1/admin/stats
    let req = Request::builder()
        .method(Method::GET)
        .uri("/api/v1/admin/stats")
        .header("x-admin-key", TEST_ADMIN_KEY)
        .body(Body::empty())
        .unwrap();

    let resp = tower::ServiceExt::oneshot(app.clone(), req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["totalCards"], 2);
    assert_eq!(json["activeCards"], 1);
    assert_eq!(json["unactivatedCards"], 1);

    // 2. GET /api/v1/admin/cards
    let req = Request::builder()
        .method(Method::GET)
        .uri("/api/v1/admin/cards")
        .header("x-admin-key", TEST_ADMIN_KEY)
        .body(Body::empty())
        .unwrap();

    let resp = tower::ServiceExt::oneshot(app, req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["count"], 2);
    assert!(json["cards"].as_array().unwrap().len() >= 2);
}

#[tokio::test]
async fn test_admin_card_freeze_unfreeze_and_ban_lifecycle() {
    let (billing, app) = setup_admin_app();

    // 1. Freeze card-admin-02
    let freeze_req = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/admin/cards/status")
        .header("x-admin-key", TEST_ADMIN_KEY)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({
                "cardId": "card-admin-02",
                "action": "freeze",
                "reason": "suspicious-activity"
            })
            .to_string(),
        ))
        .unwrap();

    let resp = tower::ServiceExt::oneshot(app.clone(), freeze_req)
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let card = billing.get_card("card-admin-02").unwrap();
    assert_eq!(card.status, CardStatus::Frozen);

    // 2. Unfreeze card-admin-02
    let unfreeze_req = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/admin/cards/status")
        .header("x-admin-key", TEST_ADMIN_KEY)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({
                "cardId": "card-admin-02",
                "action": "unfreeze"
            })
            .to_string(),
        ))
        .unwrap();

    let resp = tower::ServiceExt::oneshot(app.clone(), unfreeze_req)
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let card = billing.get_card("card-admin-02").unwrap();
    assert_eq!(card.status, CardStatus::Active);

    // 3. Ban card-admin-02
    let ban_req = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/admin/cards/status")
        .header("x-admin-key", TEST_ADMIN_KEY)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({
                "cardId": "card-admin-02",
                "action": "ban",
                "reason": "abuse"
            })
            .to_string(),
        ))
        .unwrap();

    let resp = tower::ServiceExt::oneshot(app, ban_req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let card = billing.get_card("card-admin-02").unwrap();
    assert_eq!(card.status, CardStatus::Banned);
}

#[tokio::test]
async fn test_admin_card_adjust_balance() {
    let (billing, app) = setup_admin_app();

    let adjust_req = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/admin/cards/adjust")
        .header("idempotency-key", "test-admin-adjust-01")
        .header("x-admin-key", TEST_ADMIN_KEY)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({
                "cardId": "card-admin-01",
                "deltaPoints": 100.0,
                "reason": "compensation"
            })
            .to_string(),
        ))
        .unwrap();

    let resp = tower::ServiceExt::oneshot(app, adjust_req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let card = billing.get_card("card-admin-01").unwrap();
    // 10 credits original + 100 credits adjusted = 110 credits = 110_000_000 micro-credits
    assert_eq!(card.available_credits(), 110_000_000);
}

#[tokio::test]
async fn test_admin_me_and_announcements_flow() {
    let (_billing, app) = setup_admin_app();

    // 1. GET /api/v1/admin/me
    let req = Request::builder()
        .method(Method::GET)
        .uri("/api/v1/admin/me")
        .header("x-admin-key", TEST_ADMIN_KEY)
        .body(Body::empty())
        .unwrap();

    let resp = tower::ServiceExt::oneshot(app.clone(), req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), 16 * 1024)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["success"], true);
    assert_eq!(json["role"], "admin");
    assert_eq!(json["authenticated"], true);

    // 2. POST /api/v1/admin/announcements
    let create_req = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/admin/announcements")
        .header("x-admin-key", TEST_ADMIN_KEY)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({
                "title": "Cluster Maintenance",
                "content": "Scheduled network cutover tonight",
                "level": "warning",
                "ttlSecs": 3600
            })
            .to_string(),
        ))
        .unwrap();

    let resp = tower::ServiceExt::oneshot(app.clone(), create_req)
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // 3. GET /api/v1/admin/announcements
    let list_req = Request::builder()
        .method(Method::GET)
        .uri("/api/v1/admin/announcements")
        .header("x-admin-key", TEST_ADMIN_KEY)
        .body(Body::empty())
        .unwrap();

    let resp = tower::ServiceExt::oneshot(app, list_req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), 16 * 1024)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["success"], true);
    assert_eq!(json["announcements"].as_array().unwrap().len(), 1);
    assert_eq!(json["announcements"][0]["title"], "Cluster Maintenance");
}

#[tokio::test]
async fn test_admin_session_single_revoke_vs_revoke_all() {
    let (_billing, app) = setup_admin_app();

    // 1. Issue Session A
    let req_a = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/admin/session")
        .header("x-admin-key", TEST_ADMIN_KEY)
        .body(Body::empty())
        .unwrap();
    let resp_a = tower::ServiceExt::oneshot(app.clone(), req_a)
        .await
        .unwrap();
    assert_eq!(resp_a.status(), StatusCode::OK);
    let bytes_a = axum::body::to_bytes(resp_a.into_body(), 16 * 1024)
        .await
        .unwrap();
    let json_a: serde_json::Value = serde_json::from_slice(&bytes_a).unwrap();
    let token_a = json_a["accessToken"].as_str().unwrap();

    // 2. Issue Session B
    let req_b = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/admin/session")
        .header("x-admin-key", TEST_ADMIN_KEY)
        .body(Body::empty())
        .unwrap();
    let resp_b = tower::ServiceExt::oneshot(app.clone(), req_b)
        .await
        .unwrap();
    assert_eq!(resp_b.status(), StatusCode::OK);
    let bytes_b = axum::body::to_bytes(resp_b.into_body(), 16 * 1024)
        .await
        .unwrap();
    let json_b: serde_json::Value = serde_json::from_slice(&bytes_b).unwrap();
    let token_b = json_b["accessToken"].as_str().unwrap();

    // 3. Both sessions can query /api/v1/admin/me
    let me_a = Request::builder()
        .method(Method::GET)
        .uri("/api/v1/admin/me")
        .header("authorization", format!("Bearer {}", token_a))
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        tower::ServiceExt::oneshot(app.clone(), me_a)
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );

    let me_b = Request::builder()
        .method(Method::GET)
        .uri("/api/v1/admin/me")
        .header("authorization", format!("Bearer {}", token_b))
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        tower::ServiceExt::oneshot(app.clone(), me_b)
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );

    // 4. Session A logs out (single session revoke)
    let revoke_a = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/admin/session/revoke")
        .header("authorization", format!("Bearer {}", token_a))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({ "all": false }).to_string()))
        .unwrap();
    let resp_rev_a = tower::ServiceExt::oneshot(app.clone(), revoke_a)
        .await
        .unwrap();
    assert_eq!(resp_rev_a.status(), StatusCode::OK);

    // 5. Session A is now 401 Unauthorized!
    let me_a2 = Request::builder()
        .method(Method::GET)
        .uri("/api/v1/admin/me")
        .header("authorization", format!("Bearer {}", token_a))
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        tower::ServiceExt::oneshot(app.clone(), me_a2)
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );

    // 6. Session B is STILL VALID! (Independent session lifecycle)
    let me_b2 = Request::builder()
        .method(Method::GET)
        .uri("/api/v1/admin/me")
        .header("authorization", format!("Bearer {}", token_b))
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        tower::ServiceExt::oneshot(app.clone(), me_b2)
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );

    // 7. Session B triggers "Revoke All"
    let revoke_all = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/admin/session/revoke")
        .header("authorization", format!("Bearer {}", token_b))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({ "all": true }).to_string()))
        .unwrap();
    assert_eq!(
        tower::ServiceExt::oneshot(app.clone(), revoke_all)
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );

    // 8. Now Session B is ALSO 401 Unauthorized!
    let me_b3 = Request::builder()
        .method(Method::GET)
        .uri("/api/v1/admin/me")
        .header("authorization", format!("Bearer {}", token_b))
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        tower::ServiceExt::oneshot(app, me_b3)
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn test_admin_batch_cards_and_provider_toggle() {
    let (billing, app) = setup_admin_app();
    billing.set_master_kek(billing::MasterKek::from_bytes([37; 32]));

    // 1. POST /api/v1/admin/cards/batch
    let batch_req = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/admin/cards/batch")
        .header("x-admin-key", TEST_ADMIN_KEY)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({
                "count": 5,
                "groupId": "group-pro-plus"
            })
            .to_string(),
        ))
        .unwrap();

    let resp = tower::ServiceExt::oneshot(app.clone(), batch_req)
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers()["cache-control"], "no-store");
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .unwrap();
    let batch_json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(batch_json["success"], true);
    assert_eq!(batch_json["count"], 5);
    let cards = batch_json["cards"].as_array().unwrap();
    assert_eq!(cards.len(), 5);

    // Verify all 5 were persisted into billing
    for card in cards {
        let card_id = card["cardId"].as_str().unwrap();
        assert!(billing.get_card(card_id).unwrap().code_encrypted.is_some());
        assert_eq!(
            billing.reveal_card_code(card_id).unwrap().as_deref(),
            card["rawCode"].as_str()
        );
    }

    // 2. GET /api/v1/admin/providers
    let prov_req = Request::builder()
        .method(Method::GET)
        .uri("/api/v1/admin/providers")
        .header("x-admin-key", TEST_ADMIN_KEY)
        .body(Body::empty())
        .unwrap();
    let resp = tower::ServiceExt::oneshot(app.clone(), prov_req)
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // 3. Register a test provider and toggle status
    billing.upsert_provider(billing::Provider::new(
        "anthropic-test",
        "Anthropic Test",
        billing::ProviderFormat::Anthropic,
        "https://api.anthropic.com",
    ));
    assert!(billing.get_provider("anthropic-test").unwrap().enabled);

    let toggle_req = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/admin/providers/status")
        .header("x-admin-key", TEST_ADMIN_KEY)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({
                "providerId": "anthropic-test",
                "enabled": false
            })
            .to_string(),
        ))
        .unwrap();
    let resp = tower::ServiceExt::oneshot(app.clone(), toggle_req)
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(!billing.get_provider("anthropic-test").unwrap().enabled);
}

#[tokio::test]
async fn commercial_publication_requires_auth_and_fresh_revision() {
    use tower::ServiceExt;
    let (billing, app) = setup_admin_app();
    let url = "/api/v1/admin/commercial-config";
    for method in [Method::GET, Method::POST] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(url)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
    let revision = billing.commercial_config().revision;
    let payload = json!({"expected_revision":revision,"reason":"test publication","rate_cards":[{"id":"new-rate","name":"New pricing","created_at_secs":1}]});
    for expected in [StatusCode::OK, StatusCode::CONFLICT] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(url)
                    .header("x-admin-key", TEST_ADMIN_KEY)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(payload.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
    }
    let response = app
        .oneshot(
            Request::builder()
                .uri(url)
                .header("x-admin-key", TEST_ADMIN_KEY)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["config"]["audit"].as_array().unwrap().len(), 1);
    assert!(body["config"].get("provider_keys").is_none());
    assert!(body["config"].get("cards").is_none());
}

#[tokio::test]
async fn tier_issuance_validates_catalog_group_and_single_device() {
    let (billing, app) = setup_admin_app();
    billing.set_master_kek(billing::MasterKek::from_bytes([37; 32]));
    let mut disabled = billing::Group::pro_plus("disabled", "Not for issuance");
    disabled.issuance_enabled = false;
    billing.upsert_group(disabled);
    for body in [
        json!({"count":1,"templateId":"tier-1000"}),
        json!({"count":1,"templateId":"tier-1000","groupId":"disabled"}),
        json!({"count":1,"templateId":"unknown","groupId":"group-pro-plus"}),
        json!({"count":1,"maxDevices":2,"groupId":"group-pro-plus"}),
        json!({"count":1,"maxDevices":0,"groupId":"group-pro-plus"}),
        json!({"count":1,"creditTotal":100,"groupId":"group-pro-plus"}),
        json!({"count":1,"groupId":"missing"}),
    ] {
        let req = Request::builder()
            .method(Method::POST)
            .uri("/api/v1/admin/cards/batch")
            .header("x-admin-key", TEST_ADMIN_KEY)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        assert_eq!(
            tower::ServiceExt::oneshot(app.clone(), req)
                .await
                .unwrap()
                .status(),
            StatusCode::BAD_REQUEST
        );
    }
    for (points, name) in [
        (1000, "PRO"),
        (2000, "PRO+"),
        (5000, "PRO Max"),
        (10000, "Power"),
    ] {
        let req = Request::builder()
            .method(Method::POST)
            .uri("/api/v1/admin/cards/batch")
            .header("x-admin-key", TEST_ADMIN_KEY)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({"count":1,"groupId":"group-pro-plus","templateId":format!("tier-{points}"),"maxDevices":1}).to_string(),
            ))
            .unwrap();
        let resp = tower::ServiceExt::oneshot(app.clone(), req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let card = billing
            .get_card(value["cards"][0]["cardId"].as_str().unwrap())
            .unwrap();
        assert_eq!(card.plan_name(), Some(name));
        assert_eq!(card.max_devices, 1);
    }
}

#[tokio::test]
async fn test_admin_void_auth_rejections_and_idempotent_audit() {
    let (billing, app) = setup_admin_app();
    for (id, status) in [
        ("frozen", CardStatus::Frozen),
        ("banned", CardStatus::Banned),
    ] {
        let mut card = Card::new(id, "group-admin", 100_000);
        card.status = status;
        billing.upsert_card(card);
    }
    for (id, key, expected) in [
        ("card-admin-01", "invalid", StatusCode::UNAUTHORIZED),
        ("card-admin-02", TEST_ADMIN_KEY, StatusCode::OK),
        ("frozen", TEST_ADMIN_KEY, StatusCode::OK),
        ("banned", TEST_ADMIN_KEY, StatusCode::OK),
        ("card-admin-01", TEST_ADMIN_KEY, StatusCode::OK),
        ("card-admin-01", TEST_ADMIN_KEY, StatusCode::OK),
    ] {
        let before = billing.get_card(id).unwrap();
        let req = Request::builder()
            .method(Method::POST)
            .uri("/api/v1/admin/cards/status")
            .header("x-admin-key", key)
            .header("x-operator-id", "forged")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(json!({"cardId": id, "action": "void", "reason": "misprint", "operatorId": "forged"}).to_string()))
            .unwrap();
        let response = tower::ServiceExt::oneshot(app.clone(), req).await.unwrap();
        assert_eq!(response.status(), expected);
        if expected == StatusCode::OK {
            let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
                .await
                .unwrap();
            let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(body["newStatus"], "voided");
            assert_eq!(body["cardId"], id);
        } else {
            assert_eq!(billing.get_card(id).unwrap(), before);
            assert!(billing.ledger_entries().is_empty());
        }
    }
    for action in ["freeze", "unfreeze", "ban"] {
        let req = Request::builder()
            .method(Method::POST)
            .uri("/api/v1/admin/cards/status")
            .header("x-admin-key", TEST_ADMIN_KEY)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({"cardId": "card-admin-01", "action": action}).to_string(),
            ))
            .unwrap();
        let response = tower::ServiceExt::oneshot(app.clone(), req).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
    let card = billing.get_card("card-admin-01").unwrap();
    assert_eq!(card.status, CardStatus::Voided);
    assert_eq!(card.credit_total, 10_000_000);
    let ledger = billing.ledger_entries();
    assert_eq!(ledger.len(), 4);
    assert_eq!(ledger[0].operator_id.as_deref(), Some("admin"));
    assert_eq!(ledger[0].reason.as_deref(), Some("misprint"));
    assert_eq!(ledger[0].credits_charged, 0);
}

#[tokio::test]
async fn test_admin_archive_list_and_unarchive_preserve_status() {
    let (billing, app) = setup_admin_app();
    billing
        .ban_card("card-admin-02", "admin", "test", gateway::now_secs())
        .unwrap();
    for (action, archived) in [
        ("archive", true),
        ("archive", true),
        ("unarchive", false),
        ("unarchive", false),
    ] {
        let req = Request::builder()
            .method(Method::POST)
            .uri("/api/v1/admin/cards/status")
            .header("x-admin-key", TEST_ADMIN_KEY)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(json!({"cardId": "card-admin-02", "action": action, "reason": "cleanup", "operatorId": "forged"}).to_string()))
            .unwrap();
        let response = tower::ServiceExt::oneshot(app.clone(), req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["newStatus"], "banned");
        assert_eq!(body["archivedAt"].is_u64(), archived);
        let req = Request::builder()
            .uri("/api/v1/admin/cards")
            .header("x-admin-key", TEST_ADMIN_KEY)
            .body(Body::empty())
            .unwrap();
        let response = tower::ServiceExt::oneshot(app.clone(), req).await.unwrap();
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let card = body["cards"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == "card-admin-02")
            .unwrap();
        assert_eq!(card["archivedAt"].is_u64(), archived);
        assert_eq!(card["status"], "banned");
        assert_eq!(card["creditTotal"], 5_000_000);
    }
    // The ban, then one archive and one unarchive: repeats change nothing.
    let entries = billing.ledger_entries();
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].exposed_model, "ban_card");
    assert!(entries
        .iter()
        .all(|e| e.operator_id.as_deref() == Some("admin") && e.credits_charged == 0));
}

#[tokio::test]
async fn test_adjust_authenticated_operator_and_legacy_idempotency() {
    let (billing, app) = setup_admin_app();
    // Entries written before the handler used the authentication helper remain replayable.
    billing
        .adjust_balance_idempotent(
            "card-admin-01",
            1_000_000,
            "admin",
            "grant",
            1,
            Some("legacy-grant"),
        )
        .unwrap();
    billing
        .adjust_balance_idempotent(
            "card-admin-01",
            1_000_000,
            "other-operator",
            "grant",
            1,
            Some("other-grant"),
        )
        .unwrap();
    let request = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/admin/session")
        .header("x-admin-key", TEST_ADMIN_KEY)
        .body(Body::empty())
        .unwrap();
    let response = tower::ServiceExt::oneshot(app.clone(), request)
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .unwrap();
    let session: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let bearer = format!("Bearer {}", session["accessToken"].as_str().unwrap());

    for (key, credential, expected) in [
        ("new-grant", "invalid", StatusCode::UNAUTHORIZED),
        ("legacy-grant", bearer.as_str(), StatusCode::OK),
        ("new-grant", bearer.as_str(), StatusCode::OK),
        ("new-grant", TEST_ADMIN_KEY, StatusCode::OK),
        ("other-grant", bearer.as_str(), StatusCode::CONFLICT),
    ] {
        let auth_header = if credential.starts_with("Bearer ") {
            "authorization"
        } else {
            "x-admin-key"
        };
        let request = Request::builder().method(Method::POST).uri("/api/v1/admin/cards/adjust")
            .header(auth_header, credential)
            .header("idempotency-key", key)
            .header("x-operator-id", "other-operator")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(json!({"cardId":"card-admin-01", "deltaPoints":1.0, "reason":"grant", "operatorId":"other-operator"}).to_string())).unwrap();
        let response = tower::ServiceExt::oneshot(app.clone(), request)
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
    }
    let entries = billing.ledger_entries();
    assert_eq!(entries.len(), 3);
    let entry = entries
        .iter()
        .find(|e| e.invocation_id.as_deref() == Some("new-grant"))
        .unwrap();
    assert_eq!(entry.operator_id.as_deref(), Some("admin"));
    assert_eq!(entry.credits_charged, 1_000_000);
    assert_eq!(
        billing.get_card("card-admin-01").unwrap().credit_total,
        13_000_000
    );
}

#[tokio::test]
async fn metrics_require_admin_not_client_authorization() {
    let (_, app) = setup_admin_app();
    let auth = gateway::auth::AuthState::new("test-auth-secret-key-32bytes-long-ok!!");
    let token = auth
        .issue_token("card-admin-01", "group-admin", 0, 3600)
        .unwrap();
    for bearer in [None, Some(token)] {
        let mut req = Request::builder().uri("/metrics");
        if let Some(token) = bearer {
            req = req.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        let resp = tower::ServiceExt::oneshot(app.clone(), req.body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap();
        assert!(!String::from_utf8_lossy(&bytes).contains("kiro_requests"));
    }
    let req = Request::builder()
        .uri("/metrics")
        .header("x-admin-key", TEST_ADMIN_KEY)
        .body(Body::empty())
        .unwrap();
    let resp = tower::ServiceExt::oneshot(app, req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(resp.headers()[header::CONTENT_TYPE]
        .to_str()
        .unwrap()
        .starts_with("text/plain"));
}

#[tokio::test]
async fn provider_status_requires_durable_commit_before_runtime_update() {
    use gateway::provider::ProviderRuntimeRegistry;

    let dir = std::env::temp_dir().join(format!(
        "kiro_provider_status_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let path = dir.join("billing_state.json");
    let billing = BillingEngine::new();
    billing.set_master_kek(billing::MasterKek::from_bytes([43; 32]));
    billing.upsert_provider(billing::Provider::new(
        "provider",
        "Test",
        billing::ProviderFormat::OpenAi,
        "https://example.invalid",
    ));
    billing.save_to_file(&path).unwrap();
    let runtime = ProviderRuntimeRegistry::new();
    runtime.sync_from_billing(&billing);
    let mut registry = FacadeRegistry::new().with_runtime(runtime.clone());
    registry.register_admin_facades(billing.clone(), TEST_ADMIN_KEY.to_string());
    let app = registry.into_router();

    // Exercise both failed disable and failed enable, retries, and a missing provider.
    for (id, enabled, fault, expected_status, expected_enabled) in [
        (
            "provider",
            false,
            true,
            StatusCode::SERVICE_UNAVAILABLE,
            true,
        ),
        ("missing", false, true, StatusCode::NOT_FOUND, true),
        ("provider", false, false, StatusCode::OK, false),
        ("provider", false, false, StatusCode::OK, false),
        (
            "provider",
            true,
            true,
            StatusCode::SERVICE_UNAVAILABLE,
            false,
        ),
        ("provider", true, false, StatusCode::OK, true),
    ] {
        let persisted = std::fs::read(&path).unwrap();
        let sequence = billing.snapshot_sequence();
        billing.inject_persistence_fault(fault);
        let request = Request::builder()
            .method(Method::POST)
            .uri("/api/v1/admin/providers/status")
            .header("x-admin-key", TEST_ADMIN_KEY)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({"providerId": id, "enabled": enabled}).to_string(),
            ))
            .unwrap();
        let response = tower::ServiceExt::oneshot(app.clone(), request)
            .await
            .unwrap();
        assert_eq!(response.status(), expected_status);
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        let result: serde_json::Value = serde_json::from_slice(&body).unwrap();
        if expected_status == StatusCode::OK {
            assert_eq!(result["success"], true);
            assert_eq!(result["enabled"], enabled);
            assert!(billing.snapshot_sequence() > sequence);
            assert!(billing.persistence_ready());
        } else {
            assert_ne!(result["success"], true);
            assert_eq!(billing.snapshot_sequence(), sequence);
            assert_eq!(std::fs::read(&path).unwrap(), persisted);
        }
        assert_eq!(
            billing.get_provider("provider").unwrap().enabled,
            expected_enabled
        );
        assert_eq!(
            runtime.pool_for("provider").unwrap().provider().enabled,
            expected_enabled
        );
        assert!(billing.get_provider("missing").is_none());
        let recovered = BillingEngine::new();
        recovered.set_master_kek(billing::MasterKek::from_bytes([43; 32]));
        recovered.load_from_file(&path).unwrap();
        let recovered_runtime = ProviderRuntimeRegistry::new();
        recovered_runtime.sync_from_billing(&recovered);
        assert_eq!(
            recovered.get_provider("provider").unwrap().enabled,
            expected_enabled
        );
        assert_eq!(
            recovered_runtime
                .pool_for("provider")
                .unwrap()
                .provider()
                .enabled,
            expected_enabled
        );
    }
    assert!(dir
        .canonicalize()
        .unwrap()
        .starts_with(std::env::temp_dir().canonicalize().unwrap()));
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn admin_cards_pagination_is_sorted_and_revision_detects_insertions() {
    let billing = BillingEngine::new();
    billing.upsert_card(Card::new("card-02", "group", 1_000));
    billing.upsert_card(Card::new("card-04", "group", 1_000));

    let mut registry = FacadeRegistry::new();
    registry.register_admin_facades(billing.clone(), TEST_ADMIN_KEY.to_string());
    let app = registry.into_router();

    let page_request = |uri: &'static str| {
        Request::builder()
            .method(Method::GET)
            .uri(uri)
            .header("x-admin-key", TEST_ADMIN_KEY)
            .body(Body::empty())
            .unwrap()
    };

    let response = tower::ServiceExt::oneshot(
        app.clone(),
        page_request("/api/v1/admin/cards?offset=0&limit=2"),
    )
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .unwrap();
    let first_page: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let first_ids: Vec<_> = first_page["cards"]
        .as_array()
        .unwrap()
        .iter()
        .map(|card| card["id"].as_str().unwrap())
        .collect();
    assert_eq!(first_ids, ["card-02", "card-04"]);
    assert_eq!(first_page["count"], 2);
    assert_eq!(first_page["revision"].as_str().unwrap().len(), 64);
    let first_revision = first_page["revision"].as_str().unwrap().to_string();

    // The new ID sorts between the two cards already observed by page zero.
    billing.upsert_card(Card::new("card-03", "group", 1_000));

    let response = tower::ServiceExt::oneshot(
        app.clone(),
        page_request("/api/v1/admin/cards?offset=2&limit=2"),
    )
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .unwrap();
    let second_page: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let second_ids: Vec<_> = second_page["cards"]
        .as_array()
        .unwrap()
        .iter()
        .map(|card| card["id"].as_str().unwrap())
        .collect();
    assert_eq!(second_ids, ["card-04"]);
    assert_eq!(second_page["count"], 1);
    assert_ne!(second_page["revision"].as_str().unwrap(), first_revision);

    // A fresh page observes the complete stable order and the same new revision.
    let response =
        tower::ServiceExt::oneshot(app, page_request("/api/v1/admin/cards?offset=0&limit=500"))
            .await
            .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .unwrap();
    let full_page: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let full_ids: Vec<_> = full_page["cards"]
        .as_array()
        .unwrap()
        .iter()
        .map(|card| card["id"].as_str().unwrap())
        .collect();
    assert_eq!(full_ids, ["card-02", "card-03", "card-04"]);
    assert_eq!(full_page["count"], 3);
    assert_eq!(full_page["revision"], second_page["revision"]);
}

#[tokio::test]
async fn the_operator_can_archive_the_ledger_to_shrink_the_saved_state() {
    let dir = std::env::temp_dir().join(format!(
        "kiro-admin-archive-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let (billing, app) = setup_admin_app();
    billing.set_persistence_path(dir.join("billing_state.json"));
    billing.set_master_kek(billing::crypto::MasterKek::from_bytes([3; 32]));
    let mut card = Card::new("card-archive", "group-admin", 100_000_000);
    card.status = CardStatus::Active;
    billing.upsert_card(card);
    let params = billing::reservation::ReservationEstimateParams::new(100, 100);
    billing
        .reserve("card-archive", "inv-archive", &params, 1_000, 60)
        .unwrap();
    billing
        .settle(
            "inv-archive",
            &billing::ledger::UsageTokens {
                uncached_input_tokens: 100,
                output_tokens: 100,
                cache_creation_tokens: 0,
                cache_read_tokens: 0,
            },
            "model",
            "provider",
            "target",
            1_005,
        )
        .unwrap();
    let used = billing.get_card("card-archive").unwrap().credit_used;

    let post = |body: serde_json::Value, key: Option<&str>| {
        let mut req = Request::builder()
            .method(Method::POST)
            .uri("/api/v1/admin/ledger/archive")
            .header(header::CONTENT_TYPE, "application/json");
        if let Some(key) = key {
            req = req.header("x-admin-key", key);
        }
        req.body(Body::from(body.to_string())).unwrap()
    };
    let status = |req: Request<Body>| {
        let app = app.clone();
        async move { tower::ServiceExt::oneshot(app, req).await.unwrap().status() }
    };
    assert_eq!(
        status(post(json!({"beforeTsSecs": 2_000}), None)).await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        status(post(
            json!({"beforeTsSecs": u64::MAX}),
            Some(TEST_ADMIN_KEY)
        ))
        .await,
        StatusCode::BAD_REQUEST
    );

    let resp = tower::ServiceExt::oneshot(
        app.clone(),
        post(json!({"beforeTsSecs": 2_000}), Some(TEST_ADMIN_KEY)),
    )
    .await
    .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .unwrap();
    let result: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(result["receipt"]["drained_entries_count"], 1);
    assert!(result["stateBytesAfter"].as_u64().unwrap() > 0);
    // The receipt: what moved, where to, and the saved state's size before and after.
    assert_eq!(result["movedEntries"], 1);
    assert_eq!(result["archiveFile"], result["receipt"]["archive_file"]);
    assert!(result["stateBytesBefore"].as_u64().unwrap() > 0);
    assert!(result["stateCeilingBytes"].as_u64().unwrap() > 0);

    // The balance is unchanged, and the archive is not readable without the key.
    assert_eq!(billing.get_card("card-archive").unwrap().credit_used, used);
    let file = dir
        .join("ledger_archives")
        .join(result["receipt"]["archive_file"].as_str().unwrap());
    let content = std::fs::read_to_string(&file).unwrap();
    assert!(
        !content.contains("card-archive"),
        "the archive is in plain text"
    );
    let checksum = result["receipt"]["sha256_checksum"].as_str().unwrap();
    assert!(billing::verify_ledger_archive(&file, checksum).is_err());
    let kek = billing::crypto::MasterKek::from_bytes([3; 32]);
    let payload = billing::verify_ledger_archive_with(&file, checksum, Some(&kek)).unwrap();
    assert_eq!(payload.entries[0].card_id, "card-archive");
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn a_published_announcement_can_be_withdrawn_and_stops_being_shown() {
    let (billing, app) = setup_admin_app();
    let now = gateway::now_secs();
    billing.add_announcement(
        billing::Announcement::new(
            "ann-wrong",
            "Wrong window",
            "Maintenance tonight",
            billing::AnnouncementLevel::Critical,
            now,
        )
        .with_expiry(now + 3600),
    );
    let withdraw = |id: &str| {
        Request::builder()
            .method(Method::POST)
            .uri("/api/v1/admin/announcements/withdraw")
            .header("x-admin-key", TEST_ADMIN_KEY)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(json!({ "id": id }).to_string()))
            .unwrap()
    };
    let resp = tower::ServiceExt::oneshot(app.clone(), withdraw("ann-wrong"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(billing.list_active_announcements(now).is_empty());
    // Kept on record, not deleted.
    assert!(billing
        .export_snapshot()
        .announcements
        .iter()
        .any(|a| a.id == "ann-wrong" && !a.enabled));
    let resp = tower::ServiceExt::oneshot(app.clone(), withdraw("ann-missing"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let anonymous = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/admin/announcements/withdraw")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({ "id": "ann-wrong" }).to_string()))
        .unwrap();
    let resp = tower::ServiceExt::oneshot(app, anonymous).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn an_announcement_is_published_or_withdrawn_only_once_saved() {
    let dir = std::env::temp_dir().join(format!(
        "kiro_announcement_save_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let path = dir.join("billing_state.json");
    let billing = BillingEngine::new();
    billing.set_master_kek(billing::MasterKek::from_bytes([44; 32]));
    billing.save_to_file(&path).unwrap();
    let mut registry = FacadeRegistry::new();
    registry.register_admin_facades(billing.clone(), TEST_ADMIN_KEY.to_string());
    let app = registry.into_router();
    let post = |uri: &str, body: serde_json::Value| {
        Request::builder()
            .method(Method::POST)
            .uri(uri)
            .header("x-admin-key", TEST_ADMIN_KEY)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    };
    let notice = json!({"title": "Maintenance", "content": "Tonight", "ttlSecs": 3600});
    let now = gateway::now_secs();

    billing.inject_persistence_fault(true);
    let resp = tower::ServiceExt::oneshot(
        app.clone(),
        post("/api/v1/admin/announcements", notice.clone()),
    )
    .await
    .unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        billing.list_active_announcements(now).is_empty(),
        "an unsaved one is not live"
    );

    billing.inject_persistence_fault(false);
    let resp = tower::ServiceExt::oneshot(app.clone(), post("/api/v1/admin/announcements", notice))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(resp.into_body(), 16 * 1024)
            .await
            .unwrap(),
    )
    .unwrap();
    let id = body["announcement"]["id"].as_str().unwrap().to_string();

    billing.inject_persistence_fault(true);
    let resp = tower::ServiceExt::oneshot(
        app.clone(),
        post("/api/v1/admin/announcements/withdraw", json!({ "id": id })),
    )
    .await
    .unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        billing.list_active_announcements(now).len(),
        1,
        "still shown"
    );

    billing.inject_persistence_fault(false);
    let resp = tower::ServiceExt::oneshot(
        app,
        post("/api/v1/admin/announcements/withdraw", json!({ "id": id })),
    )
    .await
    .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(billing.list_active_announcements(now).is_empty());
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn test_admin_card_history_names_the_operator_and_reason() {
    let (_billing, app) = setup_admin_app();
    for (action, reason) in [("freeze", "客户要求暂停"), ("unfreeze", "已核实")] {
        let req = Request::builder()
            .method(Method::POST)
            .uri("/api/v1/admin/cards/status")
            .header("x-admin-key", TEST_ADMIN_KEY)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({"cardId": "card-admin-02", "action": action, "reason": reason, "operatorId": "forged"})
                    .to_string(),
            ))
            .unwrap();
        let response = tower::ServiceExt::oneshot(app.clone(), req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
    let history = |uri: &str, authenticated: bool| {
        let mut req = Request::builder().method(Method::GET).uri(uri);
        if authenticated {
            req = req.header("x-admin-key", TEST_ADMIN_KEY);
        }
        tower::ServiceExt::oneshot(app.clone(), req.body(Body::empty()).unwrap())
    };

    let response = history("/api/v1/admin/cards/history?card_id=card-admin-02", true)
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["cardId"], "card-admin-02");
    let events = body["events"].as_array().unwrap();
    assert_eq!(events.len(), 2, "{body}");
    assert_eq!(events[0]["action"], "unfreeze");
    assert_eq!(events[0]["reason"], "已核实");
    assert_eq!(events[1]["action"], "freeze");
    assert_eq!(events[1]["reason"], "客户要求暂停");
    // The signed-in operator, never one the request names.
    assert_eq!(events[1]["operator"], "admin");
    assert_eq!(events[1]["points"], 0.0);
    assert!(events[1]["ts"].as_u64().unwrap() > 0);

    // Only an administrator, for a card that exists.
    for (uri, authenticated, expected) in [
        (
            "/api/v1/admin/cards/history?card_id=card-admin-02",
            false,
            StatusCode::UNAUTHORIZED,
        ),
        ("/api/v1/admin/cards/history", true, StatusCode::BAD_REQUEST),
        (
            "/api/v1/admin/cards/history?card_id=%20",
            true,
            StatusCode::BAD_REQUEST,
        ),
        (
            "/api/v1/admin/cards/history?card_id=no-such-card",
            true,
            StatusCode::NOT_FOUND,
        ),
    ] {
        let response = history(uri, authenticated).await.unwrap();
        assert_eq!(response.status(), expected, "{uri}");
    }
}

/// The console publishes JSON: a model's first price from now (`effective_from_secs: 0`), a
/// retired flag, and mappings and scheduled prices removed by ID. What refuses a publication
/// is named in its 409.
#[tokio::test]
async fn publication_takes_first_prices_from_now_retirement_and_removals() {
    use tower::ServiceExt;
    let (billing, app) = setup_admin_app();
    billing.upsert_group(billing::Group::pro_plus("group-admin", "Admin"));
    billing.upsert_provider(billing::Provider::new(
        "prov",
        "Upstream",
        billing::ProviderFormat::OpenAi,
        "https://example.com",
    ));
    billing.upsert_provider_key(billing::ProviderKey::new("key", "prov", "sk-test"));
    let publish = |update: serde_json::Value| {
        let app = app.clone();
        let mut update = update;
        update["expected_revision"] = json!(billing.commercial_config().revision);
        update["reason"] = json!("console publication");
        async move {
            let response = app
                .oneshot(
                    Request::builder()
                        .method(Method::POST)
                        .uri("/api/v1/admin/commercial-config")
                        .header("x-admin-key", TEST_ADMIN_KEY)
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(update.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            let status = response.status();
            let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
                .await
                .unwrap();
            (
                status,
                serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(),
            )
        }
    };
    let mapping = serde_json::to_value(billing::ModelMap::new(
        "map-new",
        "group-admin",
        "new-model",
        "prov",
        "upstream-new",
    ))
    .unwrap();
    let price = |id: &str, from: u64| {
        serde_json::to_value(billing::RateCardVersion {
            id: id.to_string(),
            rate_card_id: "default".to_string(),
            model: "new-model".to_string(),
            currency: billing::Currency::Cny,
            pricing_mode: billing::PricingMode::Fixed,
            input_price_per_m: 0.0,
            output_price_per_m: 0.0,
            cache_creation_price_per_m: 0.0,
            cache_read_price_per_m: 0.0,
            fixed_input_credit_per_m: 1_000_000,
            fixed_output_credit_per_m: 1_000_000,
            fixed_cache_creation_credit_per_m: 0,
            fixed_cache_read_credit_per_m: 0,
            per_call_credit: 0,
            margin_multiplier: 1.0,
            effective_from_secs: from,
            official: None,
        })
        .unwrap()
    };
    let before = gateway::now_secs();
    let (status, body) = publish(json!({
        "rate_cards": [{"id": "default", "name": "Default", "created_at_secs": 1}],
        "models": [mapping.clone()],
        "versions": [price("v-now", 0), price("v-later", before + 86_400)],
    }))
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let versions = body["config"]["versions"].as_array().unwrap();
    let now = versions.iter().find(|v| v["id"] == "v-now").unwrap();
    assert!(now["effective_from_secs"].as_u64().unwrap() >= before);

    let mut retired = mapping.clone();
    retired["retired"] = json!(true);
    let (status, body) = publish(json!({"models": [retired]})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["config"]["models"][0]["retired"], true);

    let mut invalid = mapping.clone();
    invalid["exposed_model_id"] = json!("new model");
    for (update, message) in [
        (
            json!({"removed_models": ["map-unknown"]}),
            "Only hidden or retired mappings can be removed: map-unknown",
        ),
        (
            json!({"cancelled_versions": ["v-now"]}),
            "Only scheduled prices can be cancelled: v-now",
        ),
        (json!({"models": [invalid]}), "Invalid model ID: new model"),
    ] {
        let (status, body) = publish(update).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert!(body["error"].as_str().unwrap().ends_with(message), "{body}");
    }
    let (status, body) = publish(json!({
        "removed_models": ["map-new"],
        "cancelled_versions": ["v-later"],
    }))
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["config"]["models"].as_array().unwrap().is_empty());
    let ids: Vec<_> = body["config"]["versions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["v-now"]);
}

/// The console publishes the official pricing settings and a price with the official price it
/// was computed from, and reads both back. A new face value that leaves that price at the old
/// one is refused with 409, naming it.
#[tokio::test]
async fn official_pricing_publishes_and_reads_back_through_the_admin_api() {
    use tower::ServiceExt;
    let (billing, app) = setup_admin_app();
    let send = |body: Option<serde_json::Value>| {
        let app = app.clone();
        let revision = billing.commercial_config().revision;
        async move {
            let request = Request::builder()
                .uri("/api/v1/admin/commercial-config")
                .header("x-admin-key", TEST_ADMIN_KEY);
            let request = match body {
                Some(mut update) => {
                    update["expected_revision"] = json!(revision);
                    update["reason"] = json!("official pricing");
                    request
                        .method(Method::POST)
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(update.to_string()))
                }
                None => request.body(Body::empty()),
            };
            let response = app.oneshot(request.unwrap()).await.unwrap();
            let status = response.status();
            let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
                .await
                .unwrap();
            (
                status,
                serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(),
            )
        }
    };
    // Claude Opus 5's official prices, billed by the upstream at others: 8 credits an
    // official dollar at 0.03 CNY a credit, and 0.22 of what the upstream bills.
    let official = json!({
        "input_usd_per_m": 5.0,
        "output_usd_per_m": 25.0,
        "cache_creation_usd_per_m": 6.25,
        "cache_read_usd_per_m": 0.5,
        "price_multiplier": 0.24,
        "cost_multiplier": 0.22,
        "cost_basis_usd_per_m": [2.0, 25.0, 6.25, 0.5],
        "usd_cny": 1.0,
        "credit_face_value_cny": 0.03
    });
    let settings = json!({
        "credit_face_value_cny": 0.03,
        "usd_cny_rate": 7.25,
        "official_usd_cny": 1.0,
        "default_price_multiplier": 0.24,
        "default_cost_multiplier": 0.08,
        "provider_cost_multipliers": {"hanyue-max": 0.22, "kimera-primary": 0.08},
        "official_prices": {"claude-opus-5-5": {
            "input_usd_per_m": 4.0, "output_usd_per_m": 20.0,
            "cache_creation_usd_per_m": 5.0, "cache_read_usd_per_m": 0.2, "note": "list price"
        }},
        "route_costs": {"hanyue-max/claude-opus-5-5": {"basis_usd_per_m": [2.0, 25.0, 6.25, 0.5]}}
    });
    let before = gateway::now_secs();
    let (status, body) = send(Some(json!({
        "settings": settings,
        "versions": [{
            "id": "opus-5-5-official", "rate_card_id": "default", "model": "claude-opus-5-5",
            "currency": "CNY", "pricing_mode": "fixed",
            "input_price_per_m": 0.44, "output_price_per_m": 5.5,
            "cache_creation_price_per_m": 1.375, "cache_read_price_per_m": 0.11,
            "fixed_input_credit_per_m": 40_000_000, "fixed_output_credit_per_m": 200_000_000,
            "fixed_cache_creation_credit_per_m": 50_000_000,
            "fixed_cache_read_credit_per_m": 4_000_000,
            "per_call_credit": 0, "margin_multiplier": 1.0, "effective_from_secs": 0,
            "official": official
        }]
    })))
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, body) = send(None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let config = &body["config"];
    for field in [
        "official_usd_cny",
        "default_price_multiplier",
        "default_cost_multiplier",
        "provider_cost_multipliers",
        "route_costs",
    ] {
        assert_eq!(config["settings"][field], settings[field], "{field}");
    }
    // The server stamps when an official price changed.
    let mut price = config["settings"]["official_prices"]["claude-opus-5-5"].clone();
    let stamped = price
        .as_object_mut()
        .unwrap()
        .remove("updated_at_secs")
        .unwrap();
    assert!(stamped.as_u64().unwrap() >= before, "{stamped}");
    assert_eq!(price, settings["official_prices"]["claude-opus-5-5"]);
    assert_eq!(config["versions"][0]["official"], official);

    // What the console sends today: the face value alone.
    let (status, body) = send(Some(json!({
        "settings": {"credit_face_value_cny": 0.05, "usd_cny_rate": 7.25}
    })))
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .ends_with("Official pricing is at a stale face value or rate: claude-opus-5-5"),
        "{body}"
    );
}

/// An administrator's request to `uri`: its status and JSON body.
async fn admin_call(
    app: &axum::Router,
    method: Method,
    uri: &str,
    body: Option<serde_json::Value>,
) -> (StatusCode, serde_json::Value) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("x-admin-key", TEST_ADMIN_KEY)
        .header(header::CONTENT_TYPE, "application/json")
        .body(body.map_or_else(Body::empty, |body| Body::from(body.to_string())))
        .unwrap();
    let response = tower::ServiceExt::oneshot(app.clone(), request)
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or_default())
}

/// The newest event of `action` in a card's history, as the console reads it.
async fn newest_event(app: &axum::Router, card_id: &str, action: &str) -> serde_json::Value {
    let (status, body) = admin_call(
        app,
        Method::GET,
        &format!("/api/v1/admin/cards/history?card_id={card_id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    body["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|event| event["action"] == action)
        .cloned()
        .unwrap_or_else(|| panic!("no {action} event: {body}"))
}

/// A balance adjustment sent under an idempotency key: its status and JSON body.
async fn adjust(
    app: &axum::Router,
    idempotency_key: &str,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let request = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/admin/cards/adjust")
        .header("x-admin-key", TEST_ADMIN_KEY)
        .header("idempotency-key", idempotency_key)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let response = tower::ServiceExt::oneshot(app.clone(), request)
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or_default())
}

/// A request the card made and was charged `credits` micro-credits for, as its trace keeps it.
fn charged_request(billing: &BillingEngine, card_id: &str, invocation_id: &str, credits: i64) {
    billing.record_trace(billing::RequestTrace {
        id: format!("trace-{invocation_id}"),
        card_id: card_id.into(),
        ts: 1_700_000_000,
        invocation_id: invocation_id.into(),
        exposed_model: "claude-opus-5".into(),
        status: billing::TraceStatus::Success,
        credits_charged: credits,
        ..billing::RequestTrace::default()
    });
}

#[tokio::test]
async fn a_compensation_names_a_request_of_the_card_and_is_paid_once() {
    let (billing, app) = setup_admin_app();
    billing.upsert_card(Card::new("card-other", "group-admin", 1_000));
    charged_request(&billing, "card-admin-02", "card-admin-02:inv-1", 3_000_000);
    charged_request(&billing, "card-other", "card-other:inv-2", 3_000_000);
    let body = |points: f64, invocation: &str| {
        json!({"cardId": "card-admin-02", "deltaPoints": points, "reason": "补偿失败请求",
            "invocationId": invocation})
    };
    let before = billing
        .get_card("card-admin-02")
        .unwrap()
        .available_credits();
    for (key, request, expected, message) in [
        (
            "comp-unknown",
            body(1.0, "card-admin-02:inv-missing"),
            StatusCode::NOT_FOUND,
            "Request card-admin-02:inv-missing was not found".to_string(),
        ),
        (
            "comp-other-card",
            body(1.0, "card-other:inv-2"),
            StatusCode::CONFLICT,
            "Request card-other:inv-2 was made by card card-other, not card-admin-02".to_string(),
        ),
        (
            "comp-too-much",
            body(4.0, "card-admin-02:inv-1"),
            StatusCode::CONFLICT,
            "Request card-admin-02:inv-1 was charged 3 credits at 2023-11-14T22:13:20Z; a \
             compensation of 4 credits is more than that; send allowRepeat with a reason to \
             compensate more"
                .to_string(),
        ),
        (
            "comp-long-id",
            body(1.0, &format!("card-admin-02:{}", "x".repeat(245))),
            StatusCode::BAD_REQUEST,
            "invocationId must be 1-257 ASCII letters, digits, -_.:".to_string(),
        ),
        (
            "comp-repeat-no-reason",
            json!({"cardId": "card-admin-02", "deltaPoints": 1.0, "allowRepeat": true,
                "invocationId": "card-admin-02:inv-1"}),
            StatusCode::BAD_REQUEST,
            "allowRepeat needs a reason".to_string(),
        ),
    ] {
        let (status, response) = adjust(&app, key, request).await;
        assert_eq!(status, expected, "{key}: {response}");
        // Malformed requests are refused in the older shape, with a message.
        let error = response["error"].as_str().or(response["message"].as_str());
        assert_eq!(error, Some(message.as_str()), "{key}");
    }
    assert_eq!(
        billing
            .get_card("card-admin-02")
            .unwrap()
            .available_credits(),
        before
    );

    let (status, response) = adjust(&app, "comp-1", body(2.0, "card-admin-02:inv-1")).await;
    assert_eq!(status, StatusCode::OK, "{response}");
    // A second compensation under a fresh key is refused and names the first.
    let (status, response) = adjust(&app, "comp-2", body(1.0, "card-admin-02:inv-1")).await;
    assert_eq!(status, StatusCode::CONFLICT, "{response}");
    let error = response["error"].as_str().unwrap();
    assert!(
        error.contains("was already compensated 2 credits at")
            && error.contains("by admin (补偿失败请求)"),
        "{error}"
    );
    // The operator can compensate again, with a reason.
    let mut repeat = body(1.0, "card-admin-02:inv-1");
    repeat["allowRepeat"] = json!(true);
    let (status, response) = adjust(&app, "comp-3", repeat).await;
    assert_eq!(status, StatusCode::OK, "{response}");
    // An adjustment that names no request is not checked against one.
    let (status, response) = adjust(
        &app,
        "comp-plain",
        json!({"cardId": "card-admin-02", "deltaPoints": 10.0, "reason": "活动赠送"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(
        billing
            .get_card("card-admin-02")
            .unwrap()
            .available_credits(),
        before + 13_000_000
    );
}

#[tokio::test]
async fn card_support_actions_are_written_to_the_history_with_the_operator() {
    let (billing, app) = setup_admin_app();
    let now = gateway::now_secs();
    let mut card = Card::new("card-support", "group-admin", 10_000_000);
    card.status = CardStatus::Active;
    card.activated_at = Some(now - 86_400);
    card.valid_until = Some(now + 86_400);
    card.max_rebinds = 2;
    card.rebind_count = 2;
    card.rebind_cooldown_secs = 86_400;
    card.last_rebind_at = Some(now - 60);
    card.bound_devices = vec!["device-old".into()];
    billing.upsert_card(card);
    billing.upsert_group(billing::Group::pro_plus("group-new", "New"));
    let post = |path: &str, body: serde_json::Value| {
        let app = app.clone();
        let uri = format!("/api/v1/admin/cards/{path}");
        async move { admin_call(&app, Method::POST, &uri, Some(body)).await }
    };

    let (status, body) = post(
        "devices/unbind",
        json!({"cardId": "card-support", "deviceId": "device-old", "reason": "换电脑"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["card"]["boundDevices"], json!([]));
    assert_eq!(
        body["card"]["rebindsUsed"], 2,
        "the customer's allowance is untouched"
    );
    let event = newest_event(&app, "card-support", "unbind").await;
    assert_eq!(event["operator"], "admin");
    assert_eq!(event["reason"], "换电脑");
    assert_eq!(event["detail"], json!({"deviceId": "device-old"}));

    let (status, body) = post(
        "rebinds/reset",
        json!({"cardId": "card-support", "reason": "客户多次换机"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["card"]["rebindsUsed"], 0);
    assert_eq!(body["card"]["rebindCooldownUntil"], serde_json::Value::Null);
    let event = newest_event(&app, "card-support", "rebinds_reset").await;
    assert_eq!(event["reason"], "客户多次换机");
    assert_eq!(event["detail"]["previousRebinds"], 2);

    let (status, body) = post(
        "validity",
        json!({"cardIds": ["card-support"], "days": 30, "reason": "补偿停机"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["count"], 1);
    let until = now + 31 * 86_400;
    assert_eq!(body["cards"][0]["validUntil"], until);
    let event = newest_event(&app, "card-support", "extend").await;
    assert_eq!(event["detail"], json!({"validUntil": until}));
    let (status, body) = post(
        "validity",
        json!({"cardIds": ["card-support"], "validUntilSecs": until + 86_400, "reason": "续期"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["cards"][0]["validUntil"], until + 86_400);

    let (status, body) = post(
        "note",
        json!({"cardId": "card-support", "note": "VIP 客户"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["card"]["note"], "VIP 客户");
    let event = newest_event(&app, "card-support", "note").await;
    assert_eq!(event["operator"], "admin");
    assert_eq!(event["reason"], serde_json::Value::Null);

    let (status, body) = post(
        "group",
        json!({"cardId": "card-support", "groupId": "group-new", "reason": "升级套餐"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["card"]["groupId"], "group-new");
    let event = newest_event(&app, "card-support", "group").await;
    assert_eq!(
        event["detail"],
        json!({"previousGroupId": "group-admin", "groupId": "group-new"})
    );

    // Unban through the status endpoint, with a reason, keeping the ban's revocation.
    let (status, _) = admin_call(
        &app,
        Method::POST,
        "/api/v1/admin/cards/status",
        Some(json!({"cardId": "card-support", "action": "ban", "reason": "滥用"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let banned = billing.get_card("card-support").unwrap();
    let (status, body) = admin_call(
        &app,
        Method::POST,
        "/api/v1/admin/cards/status",
        Some(json!({"cardId": "card-support", "action": "unban", "reason": "误封"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["newStatus"], "active");
    assert_eq!(body["card"]["effectiveStatus"], "active");
    assert_eq!(
        billing.get_card("card-support").unwrap().token_version,
        banned.token_version
    );
    let event = newest_event(&app, "card-support", "unban").await;
    assert_eq!(event["reason"], "误封");

    // A compensation names the request it makes up for.
    charged_request(
        &billing,
        "card-support",
        "card-support:inv-failed-1",
        3_000_000,
    );
    let (status, body) = adjust(
        &app,
        "comp-support-1",
        json!({"cardId": "card-support", "deltaPoints": 2.0, "reason": "补偿失败请求",
            "invocationId": "card-support:inv-failed-1"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let event = newest_event(&app, "card-support", "adjust").await;
    assert_eq!(event["invocationId"], "card-support:inv-failed-1");
    assert_eq!(event["credits"], 2_000_000);

    // The history carries the card as it now is.
    let (_, history) = admin_call(
        &app,
        Method::GET,
        "/api/v1/admin/cards/history?card_id=card-support",
        None,
    )
    .await;
    assert_eq!(history["card"]["id"], "card-support");
    assert_eq!(history["card"]["groupId"], "group-new");
}

#[tokio::test]
async fn card_support_actions_refuse_with_a_message_and_change_nothing() {
    let (billing, app) = setup_admin_app();
    let mut voided = Card::new("card-voided", "group-admin", 1_000);
    voided.status = CardStatus::Voided;
    billing.upsert_card(voided);
    let mut closed = billing::Group::pro_plus("group-closed", "Acceptance");
    closed.issuance_enabled = false;
    billing.upsert_group(closed);
    let long_reason = "x".repeat(201);
    let before = billing.export_snapshot();
    for (path, body, expected, message) in [
        (
            "devices/unbind",
            json!({"cardId": "card-admin-02", "deviceId": "device-x", "reason": "换电脑"}),
            StatusCode::NOT_FOUND,
            "Device device-x not found for card card-admin-02",
        ),
        (
            "devices/unbind",
            json!({"cardId": "card-admin-02", "deviceId": "device-x"}),
            StatusCode::BAD_REQUEST,
            "A reason of 1 to 200 bytes is required",
        ),
        (
            "rebinds/reset",
            json!({"cardId": "card-admin-02", "reason": long_reason}),
            StatusCode::BAD_REQUEST,
            "A reason of 1 to 200 bytes is required",
        ),
        (
            "rebinds/reset",
            json!({"cardId": "no-such-card", "reason": "x"}),
            StatusCode::NOT_FOUND,
            "Card no-such-card not found",
        ),
        (
            "validity",
            json!({"cardIds": ["card-admin-02", "card-voided"], "days": 5, "reason": "续期"}),
            StatusCode::CONFLICT,
            "Voided cards cannot be extended: card-voided",
        ),
        (
            "validity",
            json!({"cardIds": ["card-admin-02"], "days": 5, "validUntilSecs": 4_000_000_000u64, "reason": "续期"}),
            StatusCode::BAD_REQUEST,
            "Give exactly one of days and validUntilSecs",
        ),
        (
            "validity",
            json!({"cardIds": ["card-admin-02"], "days": 3651, "reason": "续期"}),
            StatusCode::BAD_REQUEST,
            "days must be between 1 and 3650",
        ),
        (
            "validity",
            json!({"cardIds": [], "days": 5, "reason": "续期"}),
            StatusCode::BAD_REQUEST,
            "cardIds must name 1 to 500 cards",
        ),
        (
            "note",
            json!({"cardId": "card-admin-02", "note": "a\nb"}),
            StatusCode::BAD_REQUEST,
            "note must be at most 256 bytes, without control characters",
        ),
        (
            "note",
            json!({"cardId": "card-admin-02", "note": "字".repeat(86)}),
            StatusCode::BAD_REQUEST,
            "note must be at most 256 bytes, without control characters",
        ),
        (
            "group",
            json!({"cardId": "card-admin-02", "groupId": "group-missing", "reason": "x"}),
            StatusCode::CONFLICT,
            "Unknown group: group-missing",
        ),
        (
            "group",
            json!({"cardId": "card-admin-02", "groupId": "group-closed", "reason": "x"}),
            StatusCode::CONFLICT,
            "Group does not take cards: group-closed",
        ),
    ] {
        let (status, body) = admin_call(
            &app,
            Method::POST,
            &format!("/api/v1/admin/cards/{path}"),
            Some(body),
        )
        .await;
        assert_eq!(status, expected, "{path}: {body}");
        assert_eq!(body["success"], false, "{path}");
        assert_eq!(body["error"], message, "{path}");
    }
    // An unknown field is refused rather than ignored.
    let (status, body) = admin_call(
        &app,
        Method::POST,
        "/api/v1/admin/cards/note",
        Some(json!({"cardId": "card-admin-02", "note": "x", "reason": "x"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"]
        .as_str()
        .unwrap()
        .starts_with("Invalid request body"));
    // Unbanning needs a reason, and a banned card.
    for (body, message) in [
        (
            json!({"cardId": "card-admin-02", "action": "unban"}),
            "A reason of 1 to 200 bytes is required",
        ),
        (
            json!({"cardId": "card-admin-02", "action": "unban", "reason": "误封"}),
            "Invalid billing state: cannot unban Active",
        ),
    ] {
        let (status, body) =
            admin_call(&app, Method::POST, "/api/v1/admin/cards/status", Some(body)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], message);
    }
    let after = billing.export_snapshot();
    assert_eq!(after.cards, before.cards);
    assert_eq!(after.ledger.len(), before.ledger.len());

    // Only an administrator.
    for path in [
        "devices/unbind",
        "rebinds/reset",
        "validity",
        "note",
        "group",
    ] {
        let request = Request::builder()
            .method(Method::POST)
            .uri(format!("/api/v1/admin/cards/{path}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(json!({"cardId": "card-admin-02"}).to_string()))
            .unwrap();
        let response = tower::ServiceExt::oneshot(app.clone(), request)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{path}");
    }
}

#[tokio::test]
async fn card_views_show_the_rebind_allowance_and_the_effective_status() {
    let (billing, app) = setup_admin_app();
    let now = gateway::now_secs();
    let mut lapsed = Card::new("card-lapsed", "group-admin", 1_000);
    lapsed.status = CardStatus::Active;
    lapsed.activated_at = Some(now - 40 * 86_400);
    lapsed.valid_until = Some(now - 1);
    lapsed.rebind_count = 1;
    lapsed.max_rebinds = 3;
    lapsed.last_rebind_at = Some(now - 100);
    lapsed.rebind_cooldown_secs = 3_600;
    billing.upsert_card(lapsed);
    let mut waiting = Card::new("card-waiting", "group-admin", 1_000);
    waiting.activation_duration_secs = Some(30 * 86_400);
    billing.upsert_card(waiting);

    let (status, body) = admin_call(&app, Method::GET, "/api/v1/admin/cards", None).await;
    assert_eq!(status, StatusCode::OK);
    let card = |id: &str| {
        body["cards"]
            .as_array()
            .unwrap()
            .iter()
            .find(|card| card["id"] == id)
            .cloned()
            .unwrap()
    };
    let lapsed = card("card-lapsed");
    assert_eq!(lapsed["status"], "active");
    assert_eq!(lapsed["effectiveStatus"], "expired");
    assert_eq!(lapsed["rebindsUsed"], 1);
    assert_eq!(lapsed["maxRebinds"], 3);
    assert_eq!(lapsed["rebindCooldownUntil"], now - 100 + 3_600);
    assert_eq!(lapsed["activationDurationSecs"], serde_json::Value::Null);
    let waiting = card("card-waiting");
    assert_eq!(waiting["effectiveStatus"], "unactivated");
    assert_eq!(waiting["rebindCooldownUntil"], serde_json::Value::Null);
    assert_eq!(waiting["activationDurationSecs"], 30 * 86_400);
}

#[tokio::test]
async fn financials_report_a_period_by_provider_with_sales_and_liability() {
    let (billing, app) = setup_admin_app();
    billing.upsert_rate_card_version(billing::RateCardVersion {
        id: "price-finance".to_string(),
        rate_card_id: "default".to_string(),
        model: "priced-model".to_string(),
        currency: billing::Currency::Cny,
        pricing_mode: billing::PricingMode::Fixed,
        input_price_per_m: 7.0,
        output_price_per_m: 7.0,
        cache_creation_price_per_m: 0.0,
        cache_read_price_per_m: 0.0,
        fixed_input_credit_per_m: 1_000_000,
        fixed_output_credit_per_m: 1_000_000,
        fixed_cache_creation_credit_per_m: 0,
        fixed_cache_read_credit_per_m: 0,
        per_call_credit: 0,
        margin_multiplier: 1.0,
        effective_from_secs: 0,
        official: None,
    });
    let (start, end) = (1_000_000, 2_000_000);
    let mut card = Card::new("card-finance", "group-admin", 1_000_000_000);
    card.status = CardStatus::Active;
    card.created_at = start;
    card.activated_at = Some(start + 10);
    billing.upsert_card(card);
    let tokens = billing::ledger::UsageTokens {
        uncached_input_tokens: 1_000_000,
        output_tokens: 1_000_000,
        cache_creation_tokens: 0,
        cache_read_tokens: 0,
    };
    // The second is served by a route with no price of its own: its cost is an estimate.
    for (invocation, provider, target, at) in [
        ("inv-in", "prov-a", "priced-model", start + 100),
        ("inv-out", "prov-b", "target", end),
    ] {
        let params = billing::reservation::ReservationEstimateParams::new(1_000, 1_000)
            .with_model("priced-model");
        billing
            .reserve("card-finance", invocation, &params, at, 60)
            .unwrap();
        billing
            .settle(invocation, &tokens, "priced-model", provider, target, at)
            .unwrap();
    }

    let (status, body) = admin_call(
        &app,
        Method::GET,
        &format!("/api/v1/admin/financials?fromSecs={start}&toSecs={end}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        (body["fromSecs"].as_u64(), body["toSecs"].as_u64()),
        (Some(start), Some(end))
    );
    assert_eq!(body["dashboard"]["total_requests"], 1);
    let providers = body["byProvider"].as_array().unwrap();
    assert_eq!(providers.len(), 1, "{body}");
    assert_eq!(providers[0]["providerId"], "prov-a");
    assert_eq!(providers[0]["requests"], 1);
    assert_eq!(providers[0]["uncachedInputTokens"], 1_000_000);
    // ¥7 per million tokens in and out.
    assert_eq!(providers[0]["costMicroCny"], 14_000_000);
    assert_eq!(body["margin"]["costedRequests"], 1);
    assert_eq!(body["margin"]["uncostedRequests"], 0);
    // Two credits at one fen.
    assert_eq!(body["margin"]["revenueMicroCny"], 20_000);
    assert_eq!(body["sales"]["issuedCards"], 1);
    assert_eq!(body["sales"]["issuedValueMicroCny"], 30_000_000);
    assert_eq!(body["sales"]["activatedCards"], 1);
    assert_eq!(body["planPrices"][0]["templateId"], "tier-1000");
    assert_eq!(body["planPrices"][3]["priceMicroCny"], 250_000_000);
    // Now, whatever the period: the three usable cards' balances.
    assert_eq!(body["liability"]["cards"], 3);
    assert_eq!(
        body["liability"]["microCredits"],
        10_000_000 + 5_000_000 + 1_000_000_000 - 4_000_000
    );

    let (status, body) = admin_call(&app, Method::GET, "/api/v1/admin/financials", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["dashboard"]["total_requests"], 2);
    assert_eq!(body["byProvider"].as_array().unwrap().len(), 2);
    assert_eq!(body["fromSecs"], serde_json::Value::Null);
    // An estimated cost is not a known one, wherever requests are counted as costed.
    assert_eq!(
        (
            &body["margin"]["costedRequests"],
            &body["margin"]["uncostedRequests"],
            &body["margin"]["uncostedCredits"],
            &body["margin"]["estimatedRequests"]
        ),
        (&json!(1), &json!(1), &json!(2_000_000), &json!(1))
    );
    assert_eq!(
        (
            &body["estimates"]["costedRequests"],
            &body["estimates"]["uncostedRequests"],
            &body["estimates"]["estimatedRequests"],
            &body["estimates"]["faceValueLessCostMicroCny"]
        ),
        (&json!(1), &json!(1), &json!(1), &serde_json::Value::Null)
    );
    for (query, message) in [
        ("fromSecs=soon", "fromSecs and toSecs must be whole seconds"),
        ("fromSecs=10&toSecs=10", "fromSecs must be before toSecs"),
    ] {
        let (status, body) = admin_call(
            &app,
            Method::GET,
            &format!("/api/v1/admin/financials?{query}"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body, json!({"success": false, "error": message}));
    }

    // The ledger export keeps its columns first and adds readable ones.
    let request = Request::builder()
        .uri("/api/v1/admin/exports/ledger.csv")
        .header("x-admin-key", TEST_ADMIN_KEY)
        .body(Body::empty())
        .unwrap();
    let response = tower::ServiceExt::oneshot(app, request).await.unwrap();
    let csv = String::from_utf8(
        axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(csv.starts_with(
        "id,card_id,ts,kind,invocation_id,exposed_model,provider_id,input_tokens,output_tokens,\
         credits_charged,provider_cost_micro_cny,time_utc,provider_name,cache_read_tokens,\
         cache_write_tokens,credits,revenue_cny,cost_cny\n"
    ));
}

#[tokio::test]
async fn traces_are_filtered_on_the_server_with_totals_for_the_whole_match() {
    let (billing, app) = setup_admin_app();
    for (id, card, model, ts, status, provider, credits) in [
        (
            "t1",
            "card-a",
            "claude:opus",
            100,
            billing::TraceStatus::Success,
            "prov-a",
            5,
        ),
        (
            "t2",
            "card-a",
            "claude:opus",
            200,
            billing::TraceStatus::Error,
            "prov-b",
            0,
        ),
        (
            "t3",
            "card-a",
            "claude:opus",
            300,
            billing::TraceStatus::Error,
            "prov-b",
            0,
        ),
        (
            "t4",
            "card-b",
            "claude:opus",
            400,
            billing::TraceStatus::Error,
            "prov-b",
            0,
        ),
        (
            "t5",
            "card-a",
            "other-model",
            500,
            billing::TraceStatus::Error,
            "prov-b",
            0,
        ),
    ] {
        billing.record_trace(billing::RequestTrace {
            id: id.into(),
            card_id: card.into(),
            ts,
            invocation_id: format!("{card}:{id}"),
            exposed_model: model.into(),
            status,
            provider_id: Some(provider.into()),
            credits_charged: credits,
            provider_cost_micro_cny: credits * 10,
            ..billing::RequestTrace::default()
        });
    }
    let (status, body) = admin_call(
        &app,
        Method::GET,
        "/api/v1/admin/traces?cardId=card-a&model=claude%3Aopus&fromSecs=100&toSecs=400&limit=1",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["count"], 1);
    assert_eq!(body["traces"][0]["id"], "t3");
    assert_eq!(
        body["totals"],
        json!({"count": 3, "failures": 2, "creditsCharged": 5, "costMicroCny": 50})
    );
    let (_, body) = admin_call(
        &app,
        Method::GET,
        "/api/v1/admin/traces?provider=prov-b&status=error",
        None,
    )
    .await;
    let ids: Vec<_> = body["traces"]
        .as_array()
        .unwrap()
        .iter()
        .map(|trace| trace["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["t5", "t4", "t3", "t2"]);
    assert_eq!(body["totals"]["failures"], 4);
    // The console's card filter keeps working.
    let (_, body) = admin_call(
        &app,
        Method::GET,
        "/api/v1/admin/traces?card_id=card-b",
        None,
    )
    .await;
    assert_eq!(body["count"], 1);
    for (query, message) in [
        (
            "status=lost",
            "status must be success, error, client_aborted or in_progress",
        ),
        ("toSecs=-1", "fromSecs and toSecs must be whole seconds"),
    ] {
        let (status, body) = admin_call(
            &app,
            Method::GET,
            &format!("/api/v1/admin/traces?{query}"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], message);
    }
}

#[tokio::test]
async fn stats_say_when_the_state_was_last_saved_and_whether_saving_works() {
    let dir = std::env::temp_dir().join(format!(
        "kiro-admin-stats-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let path = dir.join("billing_state.json");
    let (billing, app) = setup_admin_app();
    billing.set_master_kek(billing::MasterKek::from_bytes([5; 32]));
    let (_, stats) = admin_call(&app, Method::GET, "/api/v1/admin/stats", None).await;
    // Not saved anywhere yet.
    assert_eq!(stats["lastSavedAtSecs"], serde_json::Value::Null);
    assert_eq!(stats["persistenceReady"], true);

    let before = gateway::now_secs();
    billing.save_to_file(&path).unwrap();
    billing.record_trace(billing::RequestTrace {
        id: "trace-stats".into(),
        card_id: "card-admin-02".into(),
        ts: gateway::now_secs(),
        invocation_id: "card-admin-02:inv".into(),
        exposed_model: "model".into(),
        provider_id: Some("backup".into()),
        attempt_chain: vec![
            billing::AttemptRecord {
                key_id: "key-a".into(),
                provider_id: "primary".into(),
                success: false,
                error: Some("http_429".into()),
                latency_ms: 5,
            },
            billing::AttemptRecord {
                key_id: "key-b".into(),
                provider_id: "backup".into(),
                success: true,
                error: None,
                latency_ms: 5,
            },
        ],
        ..billing::RequestTrace::default()
    });
    let (status, stats) = admin_call(&app, Method::GET, "/api/v1/admin/stats", None).await;
    assert_eq!(status, StatusCode::OK);
    let saved_at = stats["lastSavedAtSecs"].as_u64().unwrap();
    assert!(
        saved_at >= before && saved_at <= gateway::now_secs(),
        "{stats}"
    );
    assert_eq!(stats["persistenceReady"], true);
    assert_eq!(stats["persistenceError"], serde_json::Value::Null);
    assert!(stats["stateBytes"].as_u64().unwrap() > 0);
    // Attempts by provider and Key, and requests by model, beside the existing activity.
    let activity = &stats["activity"];
    assert!(activity["providers"].is_array());
    assert_eq!(activity["providerAttempts"][0]["providerId"], "backup");
    let primary = activity["providerAttempts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["providerId"] == "primary")
        .unwrap();
    assert_eq!(primary["last1h"]["takenOver"], 1);
    assert_eq!(primary["last7d"]["failuresByKind"]["http_429"], 1);
    assert_eq!(activity["keyAttempts"].as_array().unwrap().len(), 2);
    assert_eq!(activity["modelHealth"][0]["model"], "model");
    assert!(activity["modelUsage7d"].is_array());

    // A failed write: saving stops, and the stats say why without the server's paths.
    billing.inject_persistence_fault(true);
    let (status, body) = admin_call(
        &app,
        Method::POST,
        "/api/v1/admin/cards/note",
        Some(json!({"cardId": "card-admin-02", "note": "VIP"})),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        body["error"],
        "The change could not be saved, so nothing was changed; retry shortly"
    );
    assert_eq!(billing.get_card("card-admin-02").unwrap().note, None);
    let (_, stats) = admin_call(&app, Method::GET, "/api/v1/admin/stats", None).await;
    assert_eq!(stats["persistenceReady"], false);
    assert_eq!(
        stats["persistenceError"],
        "The saved state could not be written"
    );
    assert_eq!(stats["lastSavedAtSecs"], saved_at);
    billing.inject_persistence_fault(false);

    // After a restart, the loaded state's save time and size.
    let restarted = BillingEngine::new();
    restarted.set_master_kek(billing::MasterKek::from_bytes([5; 32]));
    restarted.load_from_file(&path).unwrap();
    assert_eq!(restarted.last_saved_at(), Some(saved_at));
    assert_eq!(restarted.state_size().0, billing.state_size().0);
    let _ = std::fs::remove_dir_all(dir);
}

/// A plan as the console publishes it, into group-pro-plus.
fn plan_json(id: &str, name: &str, price_cny: f64) -> serde_json::Value {
    json!({"id": id, "name": name, "points": 300, "price_cny": price_cny, "validity_days": 7,
           "max_devices": 1, "concurrency": 1, "default_group_id": "group-pro-plus",
           "kiro_plan_type": "CUSTOM", "on_sale": true, "sort_order": 5})
}

async fn publish_plans(
    app: &axum::Router,
    billing: &BillingEngine,
    plans: Vec<serde_json::Value>,
) -> serde_json::Value {
    let (status, body) = admin_call(
        app,
        Method::POST,
        "/api/v1/admin/commercial-config",
        Some(json!({
            "expected_revision": billing.commercial_config().revision,
            "reason": "套餐调整",
            "plans": plans,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["config"].clone()
}

#[tokio::test]
async fn cards_are_issued_from_the_plan_catalog_and_keep_their_plan() {
    let (billing, app) = setup_admin_app();
    billing.set_master_kek(billing::MasterKek::from_bytes([37; 32]));
    billing.upsert_group(billing::Group::pro_plus("group-other", "Other"));
    // Before any plan is published, the four tiers.
    let (_, config) = admin_call(&app, Method::GET, "/api/v1/admin/commercial-config", None).await;
    let ids: Vec<_> = config["config"]["plans"]
        .as_array()
        .unwrap()
        .iter()
        .map(|plan| plan["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(ids, ["tier-1000", "tier-2000", "tier-5000", "tier-10000"]);
    let mut family = plan_json("family", "家庭卡", 20.0);
    family["max_devices"] = json!(2);
    let mut old = plan_json("old", "旧卡", 1.0);
    old["on_sale"] = json!(false);
    let config = publish_plans(
        &app,
        &billing,
        vec![plan_json("trial-7d", "体验卡", 9.9), family, old],
    )
    .await;
    assert_eq!(config["plans"][0]["id"], "family");
    assert_eq!(config["cards_by_plan"]["trial-7d"], 0);

    let issue = |body: serde_json::Value| {
        let app = app.clone();
        async move { admin_call(&app, Method::POST, "/api/v1/admin/cards/batch", Some(body)).await }
    };
    // Into a group other than the plan's default: the console warns, the server issues.
    let (status, body) =
        issue(json!({"count": 2, "groupId": "group-other", "planId": "trial-7d"})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let issued = &body["cards"][0];
    assert_eq!(issued["planId"], "trial-7d");
    assert_eq!(issued["virtualPlanName"], "体验卡");
    assert_eq!(issued["creditTotal"], 300_000_000);
    assert_eq!(issued["groupId"], "group-other");
    assert_eq!(
        issued["plan"],
        json!({"id": "trial-7d", "name": "体验卡", "points": 300, "priceMicroCny": 9_900_000,
               "validityDays": 7, "maxDevices": 1, "concurrency": 1, "kiroPlanType": "CUSTOM"})
    );
    let card_id = issued["cardId"].as_str().unwrap().to_string();
    // templateId still names a plan, and so does the old default.
    for (body, plan, name) in [
        (
            json!({"count": 1, "groupId": "group-pro-plus", "templateId": "tier-5000"}),
            "tier-5000",
            "PRO Max",
        ),
        (
            json!({"count": 1, "groupId": "group-pro-plus"}),
            "tier-2000",
            "PRO+",
        ),
        (
            json!({"count": 1, "groupId": "group-pro-plus", "templateId": "standard-monthly", "planId": "standard-monthly"}),
            "tier-2000",
            "PRO+",
        ),
    ] {
        let (status, response) = issue(body).await;
        assert_eq!(status, StatusCode::OK, "{response}");
        let card = billing
            .get_card(response["cards"][0]["cardId"].as_str().unwrap())
            .unwrap();
        assert_eq!(card.plan.as_ref().map(|p| p.id.as_str()), Some(plan));
        assert_eq!(card.plan_name(), Some(name));
    }
    for (body, message) in [
        (
            json!({"count": 1, "groupId": "group-pro-plus", "planId": "trial-7d", "templateId": "tier-1000"}),
            "planId and templateId name different plans",
        ),
        (
            json!({"count": 1, "groupId": "group-pro-plus", "planId": "ghost"}),
            "unknown plan",
        ),
        (
            json!({"count": 1, "groupId": "group-pro-plus", "planId": "old"}),
            "plan is not on sale",
        ),
        (
            json!({"count": 1, "groupId": "group-pro-plus", "planId": "family"}),
            "cards have one device; issue from a plan with max_devices 1",
        ),
        (
            json!({"count": 1, "groupId": "group-pro-plus", "planId": "trial-7d", "creditTotal": 2_000_000_000i64}),
            "issuance requires an enabled group, the plan's credits, and maxDevices=1",
        ),
    ] {
        let (status, response) = issue(body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(response["message"], message);
    }

    // The card keeps the plan as it was sold, whatever the catalog becomes.
    publish_plans(
        &app,
        &billing,
        vec![plan_json("trial-7d", "体验卡Plus", 12.0)],
    )
    .await;
    let (_, cards) = admin_call(&app, Method::GET, "/api/v1/admin/cards?limit=500", None).await;
    let view = |id: &str| {
        cards["cards"]
            .as_array()
            .unwrap()
            .iter()
            .find(|card| card["id"] == id)
            .cloned()
            .unwrap()
    };
    let card = view(&card_id);
    assert_eq!(
        (&card["planId"], &card["planName"], &card["kiroPlanType"]),
        (&json!("trial-7d"), &json!("体验卡"), &json!("CUSTOM"))
    );
    assert_eq!(card["plan"]["priceMicroCny"], 9_900_000);
    assert_eq!(card["activationDurationSecs"], 7 * 86_400);
    // A card from before the catalog: its tier by its credits, or none.
    let legacy = view("card-admin-01");
    assert_eq!(
        (
            &legacy["planId"],
            &legacy["planName"],
            &legacy["kiroPlanType"],
            &legacy["plan"]
        ),
        (&json!(null), &json!(null), &json!("CUSTOM"), &json!(null))
    );
    let (_, history) = admin_call(
        &app,
        Method::GET,
        &format!("/api/v1/admin/cards/history?card_id={card_id}"),
        None,
    )
    .await;
    assert_eq!(history["card"]["planName"], "体验卡");

    // Finance prices plans from the catalog and each card at what it was sold for.
    let (_, finance) = admin_call(&app, Method::GET, "/api/v1/admin/financials", None).await;
    let trial_price = finance["planPrices"]
        .as_array()
        .unwrap()
        .iter()
        .find(|plan| plan["planId"] == "trial-7d")
        .cloned()
        .unwrap();
    assert_eq!(
        trial_price,
        json!({"templateId": "trial-7d", "planId": "trial-7d", "name": "体验卡Plus",
               "points": 300, "priceMicroCny": 12_000_000, "onSale": true})
    );
    let trial_sales = finance["sales"]["byPlan"]
        .as_array()
        .unwrap()
        .iter()
        .find(|plan| plan["planId"] == "trial-7d")
        .cloned()
        .unwrap();
    assert_eq!(trial_sales["issuedCards"], 2);
    assert_eq!(trial_sales["issuedValueMicroCny"], 19_800_000);
    assert_eq!(trial_sales["priceMicroCny"], 12_000_000);

    // Cards were issued from it: it is taken off sale, not removed.
    let (status, body) = admin_call(
        &app,
        Method::POST,
        "/api/v1/admin/commercial-config",
        Some(json!({
            "expected_revision": billing.commercial_config().revision,
            "reason": "停售",
            "removed_plans": ["trial-7d"],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        body["error"],
        "Invalid billing state: Plans cards were issued from can only be taken off sale: trial-7d"
    );
    let (_, config) = admin_call(&app, Method::GET, "/api/v1/admin/commercial-config", None).await;
    assert_eq!(config["config"]["cards_by_plan"]["trial-7d"], 2);
}
