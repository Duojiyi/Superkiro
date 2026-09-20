//! Offline facade security regressions: in-memory billing, no upstream or production state.
use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
    Router,
};
use billing::{
    card::{hash_card_code, CardStatus},
    BillingEngine, Card,
};
use gateway::{
    auth::AuthState,
    facade::{
        oauth::{OAuthTokenHandler, RefreshTokenHandler},
        usage::GetUsageLimitsHandler,
        virtualization::VirtualizationStore,
        FacadeRegistry,
    },
};
use serde_json::{json, Value};
use std::sync::Arc;
use tower::ServiceExt;

fn setup() -> (BillingEngine, AuthState, Router) {
    let billing = BillingEngine::new();
    for (id, secret, balance) in [
        ("a", "test-card-a", 10_000_000),
        ("b", "test-card-b", 90_000_000),
    ] {
        let mut card = Card::new(id, "group", balance);
        card.code_hash = hash_card_code(secret);
        card.activation_duration_secs = Some(86400);
        card.max_rebinds = 5;
        card.rebind_cooldown_secs = 0;
        billing.upsert_card(card);
        billing
            .activate_card_with_device(id, gateway::now_secs(), 86400, Some("device"))
            .unwrap();
    }
    let auth = AuthState::with_billing("facade-security-local-test-signing-key", billing.clone());
    let mut registry = FacadeRegistry::new();
    registry.register_portal_facades(billing.clone(), None);
    registry.register(OAuthTokenHandler::new(
        Arc::new(billing.clone()),
        auth.clone(),
    ));
    registry.register(RefreshTokenHandler::new(
        Arc::new(billing.clone()),
        auth.clone(),
    ));
    registry.register(GetUsageLimitsHandler::new(
        VirtualizationStore::with_billing(billing.clone(), "group"),
    ));
    registry.register_admin_facades(billing.clone(), "test-admin-only".into());
    (billing, auth.clone(), registry.into_router_with_auth(auth))
}

async fn request(
    app: &Router,
    method: &str,
    path: &str,
    body: Value,
    token: Option<&str>,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json");
    if let Some(token) = token {
        req = req.header("authorization", format!("Bearer {token}"));
    }
    let response = app
        .clone()
        .oneshot(req.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let body =
        serde_json::from_slice(&to_bytes(response.into_body(), 65536).await.unwrap()).unwrap();
    (status, body)
}

#[tokio::test]
async fn a05_expired_activation_is_denied_with_or_without_device_without_mutation() {
    for until in [1, gateway::now_secs()] {
        for device in [Value::Null, json!(""), json!("device"), json!("other")] {
            let (billing, _, app) = setup();
            let mut card = billing.get_card("a").unwrap();
            card.valid_until = Some(until);
            billing.upsert_card(card.clone());
            let (_, query) = request(
                &app,
                "POST",
                "/api/v1/portal/query",
                json!({"card":"test-card-a"}),
                None,
            )
            .await;
            assert_eq!(query["status"], "expired");
            let (status, result) = request(
                &app,
                "POST",
                "/api/v1/portal/activate",
                json!({"card":"test-card-a", "device":device}),
                None,
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert_eq!(result["success"], false);
            assert_eq!(billing.get_card("a").unwrap(), card);
        }
    }
}

#[tokio::test]
async fn activation_replay_cannot_extend_validity_or_replace_device() {
    let (billing, _, app) = setup();
    let before = billing.get_card("a").unwrap();
    for body in [
        json!({"card":"test-card-a"}),
        json!({"card":"test-card-a","device":"device","validity_days":99999}),
    ] {
        assert_eq!(
            request(&app, "POST", "/api/v1/portal/activate", body, None)
                .await
                .0,
            StatusCode::OK
        );
        assert_eq!(billing.get_card("a").unwrap(), before);
    }
    for (path, body) in [
        (
            "/api/v1/portal/activate",
            json!({"card":"test-card-a","device":"other"}),
        ),
        (
            "/oauth/token",
            json!({"cardKey":"test-card-a","deviceId":"other"}),
        ),
        ("/oauth/token", json!({"cardKey":"test-card-a"})),
    ] {
        assert!(request(&app, "POST", path, body, None)
            .await
            .0
            .is_client_error());
        assert_eq!(billing.get_card("a").unwrap(), before);
    }
}

#[tokio::test]
async fn server_rechecks_card_state_for_access_refresh_and_login() {
    for status in [
        CardStatus::Frozen,
        CardStatus::Banned,
        CardStatus::Voided,
        CardStatus::Expired,
        CardStatus::Unactivated,
        CardStatus::Active,
    ] {
        let (billing, auth, app) = setup();
        let access = auth.issue_token_for_card("a", 3600).unwrap();
        let before = billing.get_card("a").unwrap();
        let refresh = auth
            .issue_refresh_token(
                "a",
                "group",
                before.token_version,
                before.refresh_version,
                3600,
            )
            .unwrap();
        let mut card = before;
        card.status = status;
        if status == CardStatus::Active {
            card.valid_until = Some(1);
        }
        billing.upsert_card(card.clone());
        assert!(
            request(&app, "GET", "/getUsageLimits", json!({}), Some(&access))
                .await
                .0
                .is_client_error()
        );
        assert!(request(
            &app,
            "POST",
            "/refreshToken",
            json!({"refreshToken":refresh}),
            None
        )
        .await
        .0
        .is_client_error());
        if status != CardStatus::Unactivated {
            assert!(request(
                &app,
                "POST",
                "/oauth/token",
                json!({"cardKey":"test-card-a","deviceId":"device"}),
                None
            )
            .await
            .0
            .is_client_error());
            assert!(request(
                &app,
                "POST",
                "/api/v1/portal/activate",
                json!({"card":"test-card-a"}),
                None
            )
            .await
            .0
            .is_client_error());
        }
        assert_eq!(billing.get_card("a").unwrap(), card);
    }
}

#[tokio::test]
async fn caller_cannot_select_another_card_or_use_user_token_as_admin() {
    let (billing, auth, app) = setup();
    let access = auth.issue_token_for_card("a", 3600).unwrap();
    let before_b = billing.get_card("b").unwrap();
    let (status, usage) = request(
        &app,
        "GET",
        "/getUsageLimits?cardId=b&groupId=other",
        json!({"cardId":"b"}),
        Some(&access),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(usage["availableCredits"], 10.0);
    assert_eq!(usage["userInfo"]["email"], "a@kiro-byok.local");
    for token in [None, Some(access.as_str()), Some("forged-admin-session")] {
        assert_eq!(
            request(&app, "GET", "/api/v1/admin/cards", json!({}), token)
                .await
                .0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            request(
                &app,
                "POST",
                "/api/v1/admin/cards/status",
                json!({"cardId":"b","action":"ban","reason":"attack"}),
                token
            )
            .await
            .0,
            StatusCode::UNAUTHORIZED
        );
    }
    assert_eq!(
        request(
            &app,
            "POST",
            "/api/v1/portal/activate",
            json!({"card":"b", "cardId":"b"}),
            Some(&access)
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(billing.get_card("b").unwrap(), before_b);
}

#[tokio::test]
async fn refresh_rotation_rejects_replay_and_preserves_card_identity() {
    let (_, auth, app) = setup();
    let refresh = auth.issue_refresh_token("a", "group", 1, 1, 3600).unwrap();
    let payload = json!({"refreshToken":refresh,"cardId":"b","groupId":"other"});
    let (status, result) = request(&app, "POST", "/refreshToken", payload.clone(), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        auth.verify_token(result["accessToken"].as_str().unwrap())
            .unwrap()
            .card_id,
        "a"
    );
    assert!(request(&app, "POST", "/refreshToken", payload, None)
        .await
        .0
        .is_client_error());
    assert_eq!(
        request(&app, "GET", "/getUsageLimits", json!({}), Some(&refresh))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn unbind_challenge_cannot_be_replayed_against_another_card() {
    let (billing, auth, app) = setup();
    let access = auth.issue_token_for_card("a", 3600).unwrap();
    let (_, challenge) = request(
        &app,
        "POST",
        "/api/v1/portal/challenge",
        json!({"action":"unbind"}),
        None,
    )
    .await;
    let payload = json!({"card":"test-card-a","device":"device","challengeToken":challenge["challengeToken"],"cardId":"b"});
    let before_b = billing.get_card("b").unwrap();
    assert_eq!(
        request(&app, "POST", "/api/v1/portal/unbind", payload.clone(), None)
            .await
            .0,
        StatusCode::OK
    );
    assert!(billing.get_card("a").unwrap().bound_devices.is_empty());
    assert!(auth.verify_token(&access).is_err());
    let mut replay = payload;
    replay["card"] = json!("test-card-b");
    assert_eq!(
        request(&app, "POST", "/api/v1/portal/unbind", replay, None)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(billing.get_card("b").unwrap(), before_b);
}
