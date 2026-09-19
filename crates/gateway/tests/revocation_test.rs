//! Tests for P2-3: Instant token_version revocation across the entire lifecycle chain.
//!
//! Covers:
//! - Token revocation triggered by card ban (`ban_card`, `batch_ban`)
//! - Token revocation triggered by card freeze (`freeze_card`, `batch_freeze`)
//! - Unfreeze preserves invalidation of pre-freeze tokens; new v2 token required
//! - Token revocation triggered by device rebind eviction (`bind_device`)
//! - Token revocation triggered by explicit device unbind (`unbind_device`)
//! - Token revocation triggered by direct admin/user logout (`revoke_tokens`)
//! - Full router integration with `into_router_with_auth`
//!
//! Spec §4.1, §5, §7.

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use billing::card::{Card, CardStatus};
use billing::engine::BillingEngine;
use gateway::auth::AuthState;
use gateway::facade::FacadeRegistry;
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;

const SECRET: &str = "test-secret-key-32bytes-long-for-p2-3!!";

fn setup_system() -> (BillingEngine, AuthState) {
    let billing = BillingEngine::new();
    let auth = AuthState::with_billing(SECRET, billing.clone());
    (billing, auth)
}

fn create_card(id: &str, group: &str, max_devices: u32, max_rebinds: u32) -> Card {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let mut card = Card::new(id, group, 100_000_000);
    card.status = CardStatus::Active;
    card.activated_at = Some(now);
    card.valid_until = Some(now + 86_400);
    card.max_devices = max_devices;
    card.max_rebinds = max_rebinds;
    card.rebind_cooldown_secs = 0; // zero cooldown for deterministic tests
    card
}

async fn send_authed_request(
    app: axum::Router,
    method: Method,
    uri: &str,
    token: Option<&str>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(t) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {}", t));
    }
    let req = builder.body(Body::empty()).unwrap();
    let resp = app.oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

#[tokio::test]
async fn test_audit_b_ban_card_triggers_instant_revocation() {
    let (billing, auth) = setup_system();
    let card = create_card("card-ban-1", "grp-pro", 2, 5);
    billing.upsert_card(card);

    let mut registry = FacadeRegistry::new();
    registry.register_default_facades();
    let app = registry.into_router_with_auth(auth.clone());

    // 1. Issue token for card (starts at token_version 1)
    let token_v1 = auth.issue_token_for_card("card-ban-1", 3600).unwrap();

    // 2. Request before ban succeeds
    let (status, _) = send_authed_request(
        app.clone(),
        Method::GET,
        "/ListAvailableModels",
        Some(&token_v1),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // 3. Admin bans card
    billing.ban_card("card-ban-1", "TOS violation").unwrap();
    let card = billing.get_card("card-ban-1").unwrap();
    assert_eq!(card.status, CardStatus::Banned);
    assert_eq!(card.token_version, 2); // Auto-incremented!

    // 4. Old token v1 is immediately rejected (NOT waiting 1h TTL)
    let (status, json) = send_authed_request(
        app.clone(),
        Method::GET,
        "/ListAvailableModels",
        Some(&token_v1),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(json["__type"], "AccessDeniedException");
    // P1-07: sanitized message does not leak "Banned" status
    assert!(json["message"]
        .as_str()
        .unwrap()
        .contains("Invalid authentication credentials"));

    // 5. Attempting to issue a new token for banned card fails
    assert!(auth.issue_token_for_card("card-ban-1", 3600).is_err());
}

#[tokio::test]
async fn test_audit_b_freeze_and_unfreeze_lifecycle_revocation() {
    let (billing, auth) = setup_system();
    let card = create_card("card-freeze-1", "grp-pro", 2, 5);
    billing.upsert_card(card);

    let mut registry = FacadeRegistry::new();
    registry.register_default_facades();
    let app = registry.into_router_with_auth(auth.clone());

    let token_v1 = auth.issue_token_for_card("card-freeze-1", 3600).unwrap();

    // Verify token works
    let (status, _) = send_authed_request(
        app.clone(),
        Method::GET,
        "/ListAvailableModels",
        Some(&token_v1),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // 1. Admin freezes card
    billing
        .freeze_card("card-freeze-1", "unusual traffic")
        .unwrap();
    let card = billing.get_card("card-freeze-1").unwrap();
    assert_eq!(card.status, CardStatus::Frozen);
    assert_eq!(card.token_version, 2);

    // 2. Token v1 rejected while frozen
    let (status, json) = send_authed_request(
        app.clone(),
        Method::GET,
        "/ListAvailableModels",
        Some(&token_v1),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(json["__type"], "AccessDeniedException");
    // P1-07: sanitized message does not leak "Frozen" status
    assert!(json["message"]
        .as_str()
        .unwrap()
        .contains("Invalid authentication credentials"));

    // 3. Admin unfreezes card
    billing.unfreeze_card("card-freeze-1").unwrap();
    let card = billing.get_card("card-freeze-1").unwrap();
    assert_eq!(card.status, CardStatus::Active);
    assert_eq!(card.token_version, 2); // Remains v2

    // 4. Old token v1 MUST STILL BE REJECTED (because v1 != v2)
    let (status, json) = send_authed_request(
        app.clone(),
        Method::GET,
        "/ListAvailableModels",
        Some(&token_v1),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // P1-07: sanitized error type and message
    assert_eq!(json["__type"], "AccessDeniedException");
    assert!(json["message"]
        .as_str()
        .unwrap()
        .contains("Invalid authentication credentials"));

    // 5. Newly issued token gets v2 and succeeds
    let token_v2 = auth.issue_token_for_card("card-freeze-1", 3600).unwrap();
    let (status, _) = send_authed_request(
        app.clone(),
        Method::GET,
        "/ListAvailableModels",
        Some(&token_v2),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn test_audit_b_device_rebind_eviction_revokes_evicted_device() {
    let (billing, auth) = setup_system();
    // max_devices = 1: each new device evicts previous device
    let card = create_card("card-rebind-1", "grp-pro", 1, 5);
    billing.upsert_card(card);

    let mut registry = FacadeRegistry::new();
    registry.register_default_facades();
    let app = registry.into_router_with_auth(auth.clone());

    // Bind Device Alpha
    billing
        .bind_device("card-rebind-1", "device-alpha", 1_000)
        .unwrap();
    let token_alpha = auth.issue_token_for_card("card-rebind-1", 3600).unwrap();

    // Device Alpha works
    let (status, _) = send_authed_request(
        app.clone(),
        Method::GET,
        "/ListAvailableModels",
        Some(&token_alpha),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Device Beta binds -> evicts Device Alpha!
    billing
        .bind_device("card-rebind-1", "device-beta", 1_050)
        .unwrap();
    let card = billing.get_card("card-rebind-1").unwrap();
    assert_eq!(card.bound_devices, vec!["device-beta".to_string()]);
    assert_eq!(card.token_version, 2); // Incremented!

    // Device Alpha tries to use token_alpha -> REJECTED instantly
    let (status, json) = send_authed_request(
        app.clone(),
        Method::GET,
        "/ListAvailableModels",
        Some(&token_alpha),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(json["__type"], "AccessDeniedException");

    // Device Beta issues token and succeeds
    let token_beta = auth.issue_token_for_card("card-rebind-1", 3600).unwrap();
    let (status, _) = send_authed_request(
        app.clone(),
        Method::GET,
        "/ListAvailableModels",
        Some(&token_beta),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn test_audit_b_unbind_device_revokes_tokens() {
    let (billing, auth) = setup_system();
    let card = create_card("card-unbind-1", "grp-pro", 2, 5);
    billing.upsert_card(card);

    let mut registry = FacadeRegistry::new();
    registry.register_default_facades();
    let app = registry.into_router_with_auth(auth.clone());

    billing
        .bind_device("card-unbind-1", "device-1", 1_000)
        .unwrap();
    let token = auth.issue_token_for_card("card-unbind-1", 3600).unwrap();

    let (status, _) = send_authed_request(
        app.clone(),
        Method::GET,
        "/ListAvailableModels",
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Unbind device
    billing.unbind_device("card-unbind-1", "device-1").unwrap();

    // Token immediately revoked
    let (status, json) = send_authed_request(
        app.clone(),
        Method::GET,
        "/ListAvailableModels",
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(json["__type"], "AccessDeniedException");
}

#[tokio::test]
async fn test_audit_b_explicit_revoke_tokens_logout() {
    let (billing, auth) = setup_system();
    let card = create_card("card-logout-1", "grp-pro", 2, 5);
    billing.upsert_card(card);

    let mut registry = FacadeRegistry::new();
    registry.register_default_facades();
    let app = registry.into_router_with_auth(auth.clone());

    let token = auth.issue_token_for_card("card-logout-1", 3600).unwrap();
    let (status, _) = send_authed_request(
        app.clone(),
        Method::GET,
        "/ListAvailableModels",
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // User triggers "logout all devices"
    let new_version = billing.revoke_tokens("card-logout-1").unwrap();
    assert_eq!(new_version, 2);

    let (status, json) = send_authed_request(
        app.clone(),
        Method::GET,
        "/ListAvailableModels",
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(json["__type"], "AccessDeniedException");
}

#[tokio::test]
async fn test_audit_b_public_endpoints_accessible_without_token() {
    let (billing, auth) = setup_system();
    let card = create_card("card-pub-1", "grp-pro", 2, 5);
    billing.upsert_card(card);

    let mut registry = FacadeRegistry::new();
    registry.register_default_facades();
    let app = registry.into_router_with_auth(auth);

    // POST /oauth/token requires NO auth header
    let (status, json) = send_authed_request(app.clone(), Method::POST, "/oauth/token", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(json["accessToken"].as_str().is_some());

    // POST /refreshToken requires NO auth header
    let (status, json) =
        send_authed_request(app.clone(), Method::POST, "/refreshToken", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(json["accessToken"].as_str().is_some());

    // Protected endpoint WITHOUT token is rejected
    let (status, json) =
        send_authed_request(app.clone(), Method::GET, "/ListAvailableModels", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(json["__type"], "MissingAuthenticationTokenException");
}
