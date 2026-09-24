//! End-to-end integration tests for P3-1:
//! Login, device fingerprint binding, short-lived token issuance, token refresh, and instant revocation.
//!
//! Spec §4.1, §5, §7, P0-4.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use billing::card::{hash_card_code, Card, CardStatus};
use billing::engine::BillingEngine;
use gateway::auth::AuthState;
use gateway::facade::oauth::{
    OAuthTokenHandler, OAuthTokenResponse, RefreshTokenHandler, RefreshTokenResponse,
};
use gateway::facade::FacadeHandler;
use gateway::security::BruteForceProtector;
use http_body_util::BodyExt;
use std::sync::Arc;

fn setup_test_environment() -> (
    Arc<BillingEngine>,
    AuthState,
    Arc<BruteForceProtector>,
    OAuthTokenHandler,
    RefreshTokenHandler,
) {
    let engine = Arc::new(BillingEngine::new());
    let auth_state = AuthState::with_billing("kiro-test-secret-32-chars-long!", (*engine).clone());
    let protector = Arc::new(BruteForceProtector::default());

    let token_handler = OAuthTokenHandler::new(Arc::clone(&engine), auth_state.clone())
        .with_protector(Arc::clone(&protector));
    let refresh_handler = RefreshTokenHandler::new(Arc::clone(&engine), auth_state.clone());

    (
        engine,
        auth_state,
        protector,
        token_handler,
        refresh_handler,
    )
}

#[tokio::test]
async fn test_first_login_activates_card_and_binds_device() {
    let (engine, auth_state, _protector, token_handler, _refresh_handler) =
        setup_test_environment();

    // Create an unactivated card
    let raw_key = "CARD-KEY-TEST-ALPHA-123";
    let mut card = Card::new("card-alpha", "grp-pro", 500_000_000);
    card.code_hash = hash_card_code(raw_key);
    card.status = CardStatus::Unactivated;
    card.max_devices = 2;
    engine.upsert_card(card);

    let initial = engine.get_card("card-alpha").unwrap();
    assert_eq!(initial.status, CardStatus::Unactivated);
    assert_eq!(initial.activated_at, None);
    assert_eq!(initial.valid_until, None);
    assert!(initial.bound_devices.is_empty());

    // Perform Login
    let req_body = serde_json::json!({
        "card_key": raw_key,
        "device_id": "dev_0123456789abcdef0123456789abcdef"
    });

    let req = Request::builder()
        .method("POST")
        .uri("/oauth/token")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&req_body).unwrap()))
        .unwrap();

    let resp = token_handler.handle(req).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let body_bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let token_resp: OAuthTokenResponse = serde_json::from_slice(&body_bytes).unwrap();

    assert_eq!(token_resp.auth_method, "social");
    assert_eq!(token_resp.provider, "Google");
    assert!(token_resp.profile_arn.contains("grp-pro"));
    assert_eq!(
        token_resp.expires_in, 3600,
        "Kiro computes its expiry from expiresIn"
    );
    assert!(
        !token_resp.refresh_token.is_empty(),
        "refresh_token must be present"
    );
    // SEC-1 fix: refresh token is now a JWT, not a predictable string
    assert!(
        token_resp.refresh_token.contains('.'),
        "refresh_token must be a JWT"
    );

    // Verify JWT payload
    let claims = auth_state
        .verify_token(&token_resp.access_token)
        .expect("JWT must be valid");
    assert_eq!(claims.card_id, "card-alpha");
    assert_eq!(claims.group_id, "grp-pro");
    assert_eq!(claims.token_version, 1);

    // Verify card is now Activated ("激活即计时") and device is bound
    let updated = engine.get_card("card-alpha").unwrap();
    assert_eq!(updated.status, CardStatus::Active);
    assert!(updated.activated_at.is_some());
    assert!(updated.valid_until.is_some());
    assert_eq!(
        updated.bound_devices,
        vec!["dev_0123456789abcdef0123456789abcdef".to_string()]
    );
}

#[tokio::test]
async fn test_subsequent_login_from_same_device_is_idempotent() {
    let (engine, _auth_state, _protector, token_handler, _refresh_handler) =
        setup_test_environment();

    let raw_key = "CARD-IDEMPOTENT-456";
    let mut card = Card::new("card-idem", "grp-pro", 100_000_000);
    card.code_hash = hash_card_code(raw_key);
    card.status = CardStatus::Unactivated;
    engine.upsert_card(card);

    let payload = serde_json::json!({
        "card_key": raw_key,
        "device_id": "dev_same_device"
    });

    // 1st login
    let req1 = Request::builder()
        .method("POST")
        .uri("/oauth/token")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&payload).unwrap()))
        .unwrap();
    let resp1 = token_handler.handle(req1).await;
    assert_eq!(resp1.status(), StatusCode::OK);

    // 2nd login
    let req2 = Request::builder()
        .method("POST")
        .uri("/oauth/token")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&payload).unwrap()))
        .unwrap();
    let resp2 = token_handler.handle(req2).await;
    assert_eq!(resp2.status(), StatusCode::OK);

    let card = engine.get_card("card-idem").unwrap();
    assert_eq!(card.bound_devices.len(), 1);
    assert_eq!(card.rebind_count, 0);
    assert_eq!(card.token_version, 1);
}

#[tokio::test]
async fn test_new_device_requires_explicit_unbind() {
    let (engine, auth, _protector, token_handler, _refresh_handler) = setup_test_environment();
    let raw_key = "CARD-REBIND-789";
    let mut card = Card::new("card-rebind", "grp-pro", 100_000_000);
    card.code_hash = hash_card_code(raw_key);
    card.status = CardStatus::Active;
    card.max_rebinds = 2;
    card.rebind_cooldown_secs = 600;
    engine.upsert_card(card);
    let login = |device| {
        Request::builder()
            .method("POST")
            .uri("/oauth/token")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({"card_key":raw_key,"device_id":device}).to_string(),
            ))
            .unwrap()
    };
    let first = token_handler.handle(login("dev_one")).await;
    assert_eq!(first.status(), StatusCode::OK);
    let bytes = first.into_body().collect().await.unwrap().to_bytes();
    let tokens: OAuthTokenResponse = serde_json::from_slice(&bytes).unwrap();
    let before = engine.get_card("card-rebind").unwrap();
    let rejected = token_handler.handle(login("dev_two")).await;
    assert_eq!(rejected.status(), StatusCode::FORBIDDEN);
    let bytes = rejected.into_body().collect().await.unwrap().to_bytes();
    let error: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(error["__type"], "DeviceBindingException");
    assert!(error["message"]
        .as_str()
        .unwrap()
        .contains("unbind it on the portal"));
    assert_eq!(engine.get_card("card-rebind").unwrap(), before);
    assert!(auth.verify_token(&tokens.access_token).is_ok());
    let rotated = auth
        .rotate_refresh_token(&tokens.refresh_token, 3600, 3600)
        .unwrap();
    engine.unbind_device("card-rebind", "dev_one").unwrap();
    assert!(auth.verify_token(&tokens.access_token).is_err());
    assert!(auth.rotate_refresh_token(&rotated.2, 3600, 3600).is_err());
    assert_eq!(
        token_handler.handle(login("dev_two")).await.status(),
        StatusCode::OK
    );
    let card = engine.get_card("card-rebind").unwrap();
    assert_eq!(card.bound_devices, vec!["dev_two"]);
    assert_eq!(card.rebind_count, 1);
    assert_eq!(card.token_version, 2);
    assert_eq!(
        token_handler.handle(login("dev_three")).await.status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        token_handler.handle(login("dev_two")).await.status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn test_token_refresh_and_instant_revocation_on_version_mismatch() {
    let (engine, auth_state, _protector, token_handler, refresh_handler) = setup_test_environment();

    let raw_key = "CARD-REFRESH-101";
    let mut card = Card::new("card-ref", "grp-pro", 100_000_000);
    card.code_hash = hash_card_code(raw_key);
    card.status = CardStatus::Active;
    engine.upsert_card(card);

    // Login to obtain refresh token
    let login_req = Request::builder()
        .method("POST")
        .uri("/oauth/token")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({
                "card_key": raw_key,
                "device_id": "dev_test"
            }))
            .unwrap(),
        ))
        .unwrap();
    let login_resp = token_handler.handle(login_req).await;
    let body_bytes = login_resp.into_body().collect().await.unwrap().to_bytes();
    let token_resp: OAuthTokenResponse = serde_json::from_slice(&body_bytes).unwrap();
    let refresh_token = token_resp.refresh_token;

    // 1. Successful token refresh
    let ref_req = Request::builder()
        .method("POST")
        .uri("/refreshToken")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({
                "refreshToken": &refresh_token
            }))
            .unwrap(),
        ))
        .unwrap();
    let ref_resp = refresh_handler.handle(ref_req).await;
    assert_eq!(ref_resp.status(), StatusCode::OK);

    let ref_body = ref_resp.into_body().collect().await.unwrap().to_bytes();
    let refreshed: RefreshTokenResponse = serde_json::from_slice(&ref_body).unwrap();
    assert!(auth_state.verify_token(&refreshed.access_token).is_ok());

    // 2. Instant Revocation (e.g. Card banned or rebind increments token_version)
    let mut card = engine.get_card("card-ref").unwrap();
    card.token_version += 1; // Instant revocation triggered
    engine.upsert_card(card);

    // Old refresh token must now be rejected
    let ref_req2 = Request::builder()
        .method("POST")
        .uri("/refreshToken")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({
                "refreshToken": &refresh_token
            }))
            .unwrap(),
        ))
        .unwrap();
    let ref_resp2 = refresh_handler.handle(ref_req2).await;
    assert_eq!(ref_resp2.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_login_brute_force_lockout() {
    let (_engine, _auth_state, _protector, token_handler, _refresh_handler) =
        setup_test_environment();

    // Perform 5 failed attempts with non-existent cards
    for i in 1..=5 {
        let req = Request::builder()
            .method("POST")
            .uri("/oauth/token")
            .header("x-forwarded-for", "198.51.100.25")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&serde_json::json!({
                    "card_key": format!("NON-EXISTENT-{}", i),
                    "device_id": "dev_attacker"
                }))
                .unwrap(),
            ))
            .unwrap();
        let resp = token_handler.handle(req).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    // 6th attempt (even with valid card or invalid) is locked out with HTTP 429
    let req6 = Request::builder()
        .method("POST")
        .uri("/oauth/token")
        .header("x-forwarded-for", "198.51.100.25")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({
                "card_key": "ANY-KEY",
                "device_id": "dev_attacker"
            }))
            .unwrap(),
        ))
        .unwrap();
    let resp6 = token_handler.handle(req6).await;
    assert_eq!(resp6.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn same_computer_can_use_new_card_without_sharing_card_across_devices() {
    let (engine, auth, _, handler, _) = setup_test_environment();
    for key in ["old-card", "new-card"] {
        let mut card = Card::new(key, "grp-pro", 1_000_000_000);
        card.code_hash = hash_card_code(key);
        card.status = CardStatus::Unactivated;
        engine.upsert_card(card);
    }
    for (key, device, expected) in [
        ("old-card", "same-computer", StatusCode::OK),
        ("new-card", "same-computer", StatusCode::OK),
        ("new-card", "different-computer", StatusCode::FORBIDDEN),
    ] {
        let response = handler
            .handle(
                Request::builder()
                    .method("POST")
                    .uri("/oauth/token")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({"card_key":key,"device_id":device}).to_string(),
                    ))
                    .unwrap(),
            )
            .await;
        assert_eq!(response.status(), expected, "{key} on {device}");
        if expected == StatusCode::OK {
            let body = response.into_body().collect().await.unwrap().to_bytes();
            let token: OAuthTokenResponse = serde_json::from_slice(&body).unwrap();
            assert_eq!(auth.verify_token(&token.access_token).unwrap().card_id, key);
        }
    }
    for key in ["old-card", "new-card"] {
        assert_eq!(
            engine.get_card(key).unwrap().bound_devices,
            vec!["same-computer"]
        );
    }
}
