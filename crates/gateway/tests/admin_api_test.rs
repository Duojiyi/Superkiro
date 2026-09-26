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
