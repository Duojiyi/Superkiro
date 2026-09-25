//! Dual-blind audit test for Web Self-Service Activation & Management Portal (Spec §14.2, P4-9).

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use billing::engine::BillingEngine;
use billing::CardTemplate;
use gateway::facade::portal::{
    PortalActivateResponse, PortalQueryResponse, PortalTopupResponse, PortalUnbindResponse,
};
use gateway::facade::FacadeRegistry;
use serde_json::json;

fn setup_portal() -> (BillingEngine, CardTemplate, String, String) {
    let billing = BillingEngine::default();
    let template = CardTemplate::monthly("monthly-portal", "group-portal-1");

    // 1. Unactivated card
    let gen = billing::generate_card(&template, None, 1700000000).unwrap();
    let card = gen.card;
    let raw_code = gen.raw_code;
    billing.upsert_card(card.clone());

    (billing, template, card.id, raw_code)
}

#[tokio::test]
async fn test_portal_query_and_activation_flow() {
    let (billing, _template, card_id, raw_code) = setup_portal();

    let mut registry = FacadeRegistry::new();
    registry.register_default_facades();
    registry.register_portal_facades(billing.clone(), None);

    let auth = gateway::auth::AuthState::new("test-portal-secret-key-32bytes-long-ok!!");
    let app = registry.into_router_with_auth(auth);

    // 1. Query by raw code while unactivated
    let req = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/portal/query")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({ "card": raw_code.clone() }).to_string()))
        .unwrap();

    let resp = tower::ServiceExt::oneshot(app.clone(), req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .unwrap();
    let query_resp: PortalQueryResponse = serde_json::from_slice(&bytes).unwrap();
    assert!(query_resp.success);
    assert_eq!(query_resp.card_id, card_id);
    assert_eq!(query_resp.status, "unactivated");
    assert_eq!(query_resp.bound_devices.len(), 0);

    // 2. Activate card with initial device
    let req_act = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/portal/activate")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({
                "card": raw_code.clone(),
                "device": "portal-device-fp-101",
                "validity_days": 30
            })
            .to_string(),
        ))
        .unwrap();

    let resp_act = tower::ServiceExt::oneshot(app.clone(), req_act)
        .await
        .unwrap();
    assert_eq!(resp_act.status(), StatusCode::OK);

    let bytes_act = axum::body::to_bytes(resp_act.into_body(), 64 * 1024)
        .await
        .unwrap();
    let act_resp: PortalActivateResponse = serde_json::from_slice(&bytes_act).unwrap();
    assert!(act_resp.success);
    assert_eq!(act_resp.status, "active");

    // 3. Query again - should now be active with bound device
    let req_q2 = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/portal/query")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({ "card": raw_code }).to_string()))
        .unwrap();

    let resp_q2 = tower::ServiceExt::oneshot(app.clone(), req_q2)
        .await
        .unwrap();
    let bytes_q2 = axum::body::to_bytes(resp_q2.into_body(), 64 * 1024)
        .await
        .unwrap();
    let query_resp2: PortalQueryResponse = serde_json::from_slice(&bytes_q2).unwrap();
    assert_eq!(query_resp2.status, "active");
    // A card code alone shows only enough of the device to recognise it.
    assert_eq!(query_resp2.bound_devices, vec!["****fp-101"]);
}

#[tokio::test]
async fn test_portal_unbind_device_flow() {
    let (billing, _template, card_id, raw_code) = setup_portal();

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    // Bind two devices
    let mut card = billing.get_card(&card_id).unwrap();
    card.activate(now, 86400 * 30).unwrap();
    card.bound_devices.push("device-alpha".to_string());
    card.bound_devices.push("device-beta".to_string());
    billing.upsert_card(card);

    let mut registry = FacadeRegistry::new();
    registry.register_portal_facades(billing.clone(), None);
    let auth = gateway::auth::AuthState::new("test-portal-secret-key-32bytes-long-ok!!");
    let app = registry.into_router_with_auth(auth);

    // Acquire the mandatory anti-replay challenge.
    let challenge_req = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/portal/challenge")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({ "action": "unbind" }).to_string()))
        .unwrap();
    let challenge_resp = tower::ServiceExt::oneshot(app.clone(), challenge_req)
        .await
        .unwrap();
    let challenge_bytes = axum::body::to_bytes(challenge_resp.into_body(), 64 * 1024)
        .await
        .unwrap();
    let challenge_token = serde_json::from_slice::<serde_json::Value>(&challenge_bytes).unwrap()
        ["challengeToken"]
        .as_str()
        .unwrap()
        .to_string();

    // Unbind device-alpha
    let req = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/portal/unbind")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({
                "card": raw_code.clone(),
                "device": "device-alpha",
                "challengeToken": challenge_token
            })
            .to_string(),
        ))
        .unwrap();

    let resp = tower::ServiceExt::oneshot(app.clone(), req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .unwrap();
    let unbind_resp: PortalUnbindResponse = serde_json::from_slice(&bytes).unwrap();
    assert!(unbind_resp.success);
    assert_eq!(unbind_resp.remaining_devices, vec!["****eta"]);

    // Attempting to unbind device-alpha again should fail
    let req_again = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/portal/unbind")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({
                "card": raw_code.clone(),
                "device": "device-alpha"
            })
            .to_string(),
        ))
        .unwrap();

    let resp_again = tower::ServiceExt::oneshot(app, req_again).await.unwrap();
    assert_eq!(resp_again.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_portal_topup_recharge_flow() {
    let (billing, _template, card_id, raw_code) = setup_portal();

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    let mut card = billing.get_card(&card_id).unwrap();
    card.activate(now, 86400 * 30).unwrap();
    let initial_credits = card.available_credits();
    billing.upsert_card(card);

    // Create a 50,000 credit top-up code
    let topup_gen =
        billing::generate_topup_code(50_000 * billing::MICRO_CREDITS_PER_CREDIT, 86400 * 30, now)
            .unwrap();
    billing.upsert_topup_code(topup_gen.topup).unwrap();
    let raw_topup = topup_gen.raw_code;

    let mut registry = FacadeRegistry::new();
    registry.register_portal_facades(billing.clone(), None);
    let auth = gateway::auth::AuthState::new("test-portal-secret-key-32bytes-long-ok!!");
    let app = registry.into_router_with_auth(auth);

    // Acquire the mandatory anti-replay challenge.
    let challenge_req = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/portal/challenge")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({ "action": "topup" }).to_string()))
        .unwrap();
    let challenge_resp = tower::ServiceExt::oneshot(app.clone(), challenge_req)
        .await
        .unwrap();
    let challenge_bytes = axum::body::to_bytes(challenge_resp.into_body(), 64 * 1024)
        .await
        .unwrap();
    let challenge_token = serde_json::from_slice::<serde_json::Value>(&challenge_bytes).unwrap()
        ["challengeToken"]
        .as_str()
        .unwrap()
        .to_string();

    // Redeem top-up
    let req = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/portal/topup")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({
                "card": raw_code.clone(),
                "topup_code": raw_topup,
                "challengeToken": challenge_token
            })
            .to_string(),
        ))
        .unwrap();

    let resp = tower::ServiceExt::oneshot(app.clone(), req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .unwrap();
    let topup_resp: PortalTopupResponse = serde_json::from_slice(&bytes).unwrap();
    assert!(topup_resp.success);
    assert_eq!(
        topup_resp.remaining_credits,
        initial_credits + 50_000 * billing::MICRO_CREDITS_PER_CREDIT
    );
    assert_eq!(topup_resp.added_points, 50_000.0);
}

#[tokio::test]
async fn test_portal_web_page_html_endpoint() {
    let mut registry = FacadeRegistry::new();
    registry.register_portal_facades(billing::BillingEngine::default(), None);

    let auth = gateway::auth::AuthState::new("test-portal-secret-key-32bytes-long-ok!!");
    let app = registry.into_router_with_auth(auth);

    let req = Request::builder()
        .method(Method::GET)
        .uri("/portal")
        .body(Body::empty())
        .unwrap();

    let resp = tower::ServiceExt::oneshot(app, req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(resp
        .headers()
        .get(header::CONTENT_TYPE)
        .unwrap()
        .to_str()
        .unwrap()
        .contains("text/html"));

    // The standalone page embeds licensed font subsets; keep a bounded 256 KiB budget.
    let bytes = axum::body::to_bytes(resp.into_body(), 256 * 1024)
        .await
        .unwrap();
    let html_str = String::from_utf8(bytes.to_vec()).unwrap();
    // Check the embedded page contract, not marketing copy or visual styling.
    // Browser tests cover routing, read-only verification and confirmed unbinding.
    for marker in [
        "Superkiro",
        r#"id="home""#,
        r#"id="device""#,
        r#"id="docs""#,
        r#"href="/""#,
        r#"href="/device""#,
        r#"href="/docs""#,
        r#"id="verify-form""#,
        r#"id="card""#,
        r#"type="password""#,
        r#"id="bound-device""#,
        r#"id="confirm-dialog""#,
        r#"id="confirm-unbind""#,
        "/downloads/releases.json",
        r#"data-download="windows-x64""#,
        r#"data-download="macos-arm64""#,
        r#"data-download="macos-x64""#,
    ] {
        assert!(
            html_str.contains(marker),
            "missing portal contract: {marker}"
        );
    }
    // The footer leads to the administrator console, which asks for its own sign-in.
    assert!(html_str.contains(r#"<a href="/admin/">管理后台 ↗</a>"#));
}

#[tokio::test]
async fn test_portal_challenge_issuance_and_replay_protection() {
    let (billing, _template, card_id, raw_code) = setup_portal();

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    let mut card = billing.get_card(&card_id).unwrap();
    card.activate(now, 86400 * 30).unwrap();
    card.bound_devices.push("device-alpha".to_string());
    card.bound_devices.push("device-beta".to_string());
    billing.upsert_card(card);

    let mut registry = FacadeRegistry::new();
    registry.register_portal_facades(billing.clone(), None);
    let auth = gateway::auth::AuthState::new("test-portal-secret-key-32bytes-long-ok!!");
    let app = registry.into_router_with_auth(auth);

    // 1. Issue challenge token
    let chal_req = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/portal/challenge")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({ "action": "unbind" }).to_string()))
        .unwrap();

    let chal_resp = tower::ServiceExt::oneshot(app.clone(), chal_req)
        .await
        .unwrap();
    assert_eq!(chal_resp.status(), StatusCode::OK);
    let chal_bytes = axum::body::to_bytes(chal_resp.into_body(), 64 * 1024)
        .await
        .unwrap();
    let chal_json: serde_json::Value = serde_json::from_slice(&chal_bytes).unwrap();
    assert!(chal_json["success"].as_bool().unwrap());
    let token = chal_json["challengeToken"].as_str().unwrap().to_string();

    // 2. Use challenge token in unbind -> succeeds
    let req = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/portal/unbind")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({
                "card": raw_code.clone(),
                "device": "device-alpha",
                "challengeToken": token
            })
            .to_string(),
        ))
        .unwrap();

    let resp = tower::ServiceExt::oneshot(app.clone(), req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // 3. Replay attack with same challenge token -> REJECTED with 403 Forbidden
    let replay_req = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/portal/unbind")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({
                "card": raw_code.clone(),
                "device": "device-beta",
                "challengeToken": token
            })
            .to_string(),
        ))
        .unwrap();

    let replay_resp = tower::ServiceExt::oneshot(app.clone(), replay_req)
        .await
        .unwrap();
    assert_eq!(replay_resp.status(), StatusCode::FORBIDDEN);

    // 4. Invalid challenge token -> REJECTED with 403 Forbidden
    let invalid_req = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/portal/unbind")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({
                "card": raw_code.clone(),
                "device": "device-beta",
                "challengeToken": "totally-fake-invalid-token"
            })
            .to_string(),
        ))
        .unwrap();

    let invalid_resp = tower::ServiceExt::oneshot(app, invalid_req).await.unwrap();
    assert_eq!(invalid_resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_portal_consecutive_failure_lockout() {
    let billing = BillingEngine::default();
    let mut registry = FacadeRegistry::new();
    registry.register_portal_facades(billing, None);
    let auth = gateway::auth::AuthState::new("test-portal-secret-key-32bytes-long-ok!!");
    let app = registry.into_router_with_auth(auth);

    // Send 5 invalid unbind attempts to trigger brute force lockout
    for _ in 0..5 {
        let req = Request::builder()
            .method(Method::POST)
            .uri("/api/v1/portal/unbind")
            .header("x-real-ip", "192.168.10.50")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "card": "nonexistent-card-id",
                    "device": "device-x"
                })
                .to_string(),
            ))
            .unwrap();

        let resp = tower::ServiceExt::oneshot(app.clone(), req).await.unwrap();
        // 400 Bad Request
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    // 6th attempt must be locked out with 429 Too Many Requests
    let locked_req = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/portal/unbind")
        .header("x-real-ip", "192.168.10.50")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({
                "card": "nonexistent-card-id",
                "device": "device-x"
            })
            .to_string(),
        ))
        .unwrap();

    let locked_resp = tower::ServiceExt::oneshot(app, locked_req).await.unwrap();
    assert_eq!(locked_resp.status(), StatusCode::TOO_MANY_REQUESTS);
    let bytes = axum::body::to_bytes(locked_resp.into_body(), 64 * 1024)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(json["error"].as_str().unwrap().contains("Locked out"));
}

#[tokio::test]
async fn test_portal_unbind_public_error_contract() {
    async fn post(
        app: &axum::Router,
        path: &str,
        body: serde_json::Value,
    ) -> (StatusCode, serde_json::Value) {
        let request = Request::builder()
            .method(Method::POST)
            .uri(path)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let response = tower::ServiceExt::oneshot(app.clone(), request)
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 65536)
            .await
            .unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }
    for case in [
        "cooldown",
        "limit",
        "missing-card",
        "missing-device",
        "invalid-challenge",
        "frozen",
    ] {
        let (billing, _, card_id, raw_code) = setup_portal();
        let now = gateway::now_secs();
        let mut card = billing.get_card(&card_id).unwrap();
        card.activate(now, 86400).unwrap();
        card.bound_devices = vec!["private-device".into()];
        card.max_rebinds = if case == "limit" { 0 } else { 5 };
        card.rebind_cooldown_secs = 300;
        if case == "cooldown" {
            card.last_rebind_at = Some(now);
        }
        if case == "frozen" {
            card.status = billing::card::CardStatus::Frozen;
        }
        billing.upsert_card(card.clone());
        let mut registry = FacadeRegistry::new();
        registry.register_portal_facades(billing.clone(), None);
        let app = registry.into_router_with_auth(gateway::auth::AuthState::new(
            "test-portal-secret-key-32bytes-long-ok!!",
        ));
        let (status, challenge) =
            post(&app, "/api/v1/portal/challenge", json!({"action":"unbind"})).await;
        assert_eq!(status, StatusCode::OK);
        let (status, result) = post(&app, "/api/v1/portal/unbind", json!({
            "card": if case == "missing-card" { "unknown-secret" } else { &raw_code },
            "device": if case == "missing-device" { "unknown-device" } else { "private-device" },
            "challengeToken": if case == "invalid-challenge" { json!("invalid-token") } else { challenge["challengeToken"].clone() },
        })).await;
        if case == "frozen" {
            assert_eq!(status, StatusCode::OK);
            assert_eq!(result["success"], true);
            assert_eq!(result["remainingDevices"], json!([]));
            assert_eq!(
                billing.get_card(&card_id).unwrap().status,
                billing::card::CardStatus::Frozen
            );
            let (status, _) = post(
                &app,
                "/api/v1/portal/activate",
                json!({"card":raw_code,"device":"new-device"}),
            )
            .await;
            assert_eq!(
                status,
                StatusCode::BAD_REQUEST,
                "unbind cannot make frozen authorization usable"
            );
            continue;
        }
        assert_eq!(
            status,
            if case == "invalid-challenge" {
                StatusCode::FORBIDDEN
            } else {
                StatusCode::BAD_REQUEST
            },
            "{case}"
        );
        assert_eq!(result["success"], false);
        assert_eq!(
            billing.get_card(&card_id).unwrap(),
            card,
            "{case} must not mutate the card"
        );
        let serialized = result.to_string();
        for secret in [
            &raw_code,
            &card_id,
            "private-device",
            "unknown-secret",
            "unknown-device",
            "invalid-token",
        ] {
            assert!(!serialized.contains(secret), "{case} leaked {secret}");
        }
        match case {
            "cooldown" => {
                assert_eq!(result["code"], "rebind_cooldown");
                assert!((1..=300).contains(&result["retryAfterSecs"].as_u64().unwrap()));
            }
            "limit" => {
                assert_eq!(result["code"], "rebind_limit_exceeded");
                assert!(result.get("retryAfterSecs").is_none());
            }
            "missing-card" | "missing-device" => assert_eq!(
                result,
                json!({"success":false,"error":"Invalid card or request parameters"})
            ),
            _ => {
                assert!(result.get("code").is_none());
                assert!(result.get("retryAfterSecs").is_none());
            }
        }
    }
}
