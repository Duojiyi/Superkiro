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
async fn test_rebind_eviction_and_cooldown_rejection() {
    let (engine, _auth_state, _protector, token_handler, _refresh_handler) =
        setup_test_environment();

    let raw_key = "CARD-REBIND-789";
    let mut card = Card::new("card-rebind", "grp-pro", 100_000_000);
    card.code_hash = hash_card_code(raw_key);
    card.status = CardStatus::Active;
    card.max_devices = 1;
    card.max_rebinds = 2;
    card.rebind_cooldown_secs = 600; // 10 minutes
    engine.upsert_card(card);

    // Device 1 binds
    let req1 = Request::builder()
        .method("POST")
        .uri("/oauth/token")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({
                "card_key": raw_key,
                "device_id": "dev_one"
            }))
            .unwrap(),
        ))
        .unwrap();
    assert_eq!(token_handler.handle(req1).await.status(), StatusCode::OK);

    // Device 2 binds (evicts dev_one, increments token_version to 2)
    let req2 = Request::builder()
        .method("POST")
        .uri("/oauth/token")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({
                "card_key": raw_key,
                "device_id": "dev_two"
            }))
            .unwrap(),
        ))
        .unwrap();
    assert_eq!(token_handler.handle(req2).await.status(), StatusCode::OK);

    let card = engine.get_card("card-rebind").unwrap();
    assert_eq!(card.bound_devices, vec!["dev_two".to_string()]);
    assert_eq!(card.rebind_count, 1);
    assert_eq!(card.token_version, 2);

    // Device 3 binds immediately -> rejected by rebind cooldown (HTTP 429)
    let req3 = Request::builder()
        .method("POST")
        .uri("/oauth/token")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({
                "card_key": raw_key,
                "device_id": "dev_three"
            }))
            .unwrap(),
        ))
        .unwrap();
    let resp3 = token_handler.handle(req3).await;
    assert_eq!(resp3.status(), StatusCode::TOO_MANY_REQUESTS);
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
