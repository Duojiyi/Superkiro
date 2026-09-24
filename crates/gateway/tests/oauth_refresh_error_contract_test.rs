//! Refresh errors preserve safe categories without exposing card existence or state.
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use billing::{
    card::{Card, CardStatus},
    engine::BillingEngine,
};
use gateway::{
    auth::AuthState,
    facade::{oauth::RefreshTokenHandler, FacadeHandler},
};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::sync::Arc;

const SECRET: &str = "refresh-contract-test-secret-not-production";
const CARD: &str = "private-card-identity";
const GROUP: &str = "private-group-identity";

fn active_card() -> Card {
    let mut card = Card::new(CARD, GROUP, 1_000_000);
    card.status = CardStatus::Active;
    card
}

fn setup() -> (Arc<BillingEngine>, AuthState, RefreshTokenHandler) {
    let engine = Arc::new(BillingEngine::new());
    engine.upsert_card(active_card());
    // No retry window: these tests pin down that a replaced token is refused.
    let auth = AuthState::with_billing(SECRET, (*engine).clone()).with_refresh_grace(0);
    let handler = RefreshTokenHandler::new(engine.clone(), auth.clone());
    (engine, auth, handler)
}

fn token(auth: &AuthState, card: &Card) -> String {
    auth.issue_refresh_token(
        &card.id,
        &card.group_id,
        card.token_version,
        card.refresh_version,
        3600,
    )
    .unwrap()
}

async fn refresh(handler: &RefreshTokenHandler, body: Value) -> (StatusCode, Value) {
    let request = Request::builder()
        .method("POST")
        .uri("/refreshToken")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let response = handler.handle(request).await;
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

fn denied() -> Value {
    json!({"__type":"AccessDeniedException", "message":"Invalid authentication credentials"})
}

#[tokio::test]
async fn malformed_signature_and_wrong_token_kind_are_safe() {
    let (_, auth, handler) = setup();
    let other = AuthState::new("different-signing-secret");
    let wrong_signature = token(&other, &active_card());
    let access = auth
        .issue_token(CARD, GROUP, active_card().token_version, 3600)
        .unwrap();
    for body in [
        json!({}),
        json!({"refreshToken":"not-a-jwt-private-detail"}),
        json!({"refreshToken":wrong_signature}),
        json!({"refreshToken":access}),
    ] {
        let (status, result) = refresh(&handler, body).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(result["__type"], "UnrecognizedClientException");
        assert!(matches!(
            result["message"].as_str(),
            Some("Invalid authentication credentials" | "Invalid token format")
        ));
        assert_eq!(result.as_object().unwrap().len(), 2);
    }
}

#[tokio::test]
async fn expired_refresh_has_distinct_fixed_error() {
    let (_, _, handler) = setup();
    // A signed, deterministically expired token avoids sleeps and clock-boundary flakes.
    let expired = jsonwebtoken::encode(
        &jsonwebtoken::Header::default(),
        &json!({
            "card_id":CARD, "group_id":GROUP, "token_version":1, "refresh_version":0,
            "exp":1, "iat":0, "jti":"private-expired-jti", "kind":"refresh",
            "iss":"kiro-byok", "aud":"kiro-gateway"
        }),
        &jsonwebtoken::EncodingKey::from_secret(SECRET.as_bytes()),
    )
    .unwrap();
    assert_eq!(
        refresh(&handler, json!({"refreshToken":expired})).await,
        (
            StatusCode::UNAUTHORIZED,
            json!({"__type":"ExpiredTokenException",
            "message":"The security token included in the request is expired"})
        )
    );
}

#[tokio::test]
async fn card_failures_are_indistinguishable_and_do_not_enumerate() {
    for case in [
        "missing",
        "frozen",
        "banned",
        "voided",
        "unactivated",
        "expired",
        "validity",
        "revoked",
        "devices",
    ] {
        let (engine, auth, handler) = setup();
        let mut card = active_card();
        let raw = if case == "missing" {
            let mut missing = card.clone();
            missing.id = "private-nonexistent-card".into();
            token(&auth, &missing)
        } else {
            token(&auth, &card)
        };
        match case {
            "frozen" => card.status = CardStatus::Frozen,
            "banned" => card.status = CardStatus::Banned,
            "voided" => card.status = CardStatus::Voided,
            "unactivated" => card.status = CardStatus::Unactivated,
            "expired" => card.status = CardStatus::Expired,
            "validity" => card.valid_until = Some(1),
            "revoked" => card.token_version += 1,
            "devices" => {
                card.bound_devices = vec!["private-device-a".into(), "private-device-b".into()]
            }
            _ => {}
        }
        engine.upsert_card(card);
        assert_eq!(
            refresh(&handler, json!({"refreshToken":raw})).await,
            (StatusCode::UNAUTHORIZED, denied()),
            "{case}"
        );
    }
}

#[tokio::test]
async fn successful_rotation_and_replay_keep_their_contract() {
    let (_, auth, handler) = setup();
    let raw = token(&auth, &active_card());
    let (status, result) = refresh(&handler, json!({"refreshToken":raw})).await;
    assert_eq!(status, StatusCode::OK);
    assert!(auth
        .verify_token(result["accessToken"].as_str().unwrap())
        .is_ok());
    assert_ne!(result["refreshToken"], raw);
    assert!(result["profileArn"].as_str().unwrap().ends_with(GROUP));
    assert!(result["expiresAt"].as_str().unwrap().ends_with('Z'));
    assert_eq!(
        refresh(&handler, json!({"refreshToken":raw})).await,
        (
            StatusCode::UNAUTHORIZED,
            json!({"__type":"UnrecognizedClientException",
            "message":"Invalid authentication credentials"})
        )
    );
    assert_eq!(
        refresh(&handler, json!({"refreshToken":result["refreshToken"]}))
            .await
            .0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn post_rotation_checks_use_the_same_non_enumerating_error() {
    // Separate stores deterministically exercise the handler's second read,
    // as though the live card changed after the rotation helper validated it.
    for case in ["missing", "frozen", "validity", "revoked"] {
        let (_, auth, _) = setup();
        let engine = Arc::new(BillingEngine::new());
        let mut card = active_card();
        match case {
            "frozen" => card.status = CardStatus::Frozen,
            "validity" => card.valid_until = Some(1),
            "revoked" => card.token_version += 1,
            _ => {}
        }
        if case != "missing" {
            engine.upsert_card(card);
        }
        let handler = RefreshTokenHandler::new(engine, auth.clone());
        assert_eq!(
            refresh(
                &handler,
                json!({"refreshToken":token(&auth, &active_card())})
            )
            .await,
            (StatusCode::UNAUTHORIZED, denied()),
            "{case}"
        );
    }
}

/// Kiro computes its token expiry as `now + expiresIn * 1000`. Without the field that throws,
/// after the gateway has already rotated the refresh token, and Kiro logs the user out.
#[tokio::test]
async fn refresh_response_carries_the_expiry_kiro_reads() {
    let (_, auth, handler) = setup();
    let (status, result) = refresh(
        &handler,
        json!({"refreshToken": token(&auth, &active_card())}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        result["expiresIn"], 3600,
        "Kiro cannot complete a refresh without it"
    );
    assert!(
        result["expiresAt"].is_string(),
        "the desktop client reads expiresAt"
    );
}

/// A refresh whose write failed consumed nothing. Kiro and the desktop app log out on 401 and
/// retry a 503, and the same token must work once storage is back.
#[tokio::test]
async fn storage_failure_on_refresh_is_retryable_not_a_logout() {
    let (engine, auth, handler) = setup();
    let dir = std::env::temp_dir().join(format!(
        "refresh-storage-fault-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    engine.set_persistence_path(dir.join("state.json"));
    let refresh_token = token(&auth, &active_card());

    engine.inject_persistence_fault(true);
    let (status, result) = refresh(&handler, json!({"refreshToken": refresh_token})).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(result["__type"], "ServiceUnavailableException");

    engine.inject_persistence_fault(false);
    let (status, _) = refresh(&handler, json!({"refreshToken": refresh_token})).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the failed attempt must not consume the token"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A refresh whose response was lost, or that a second window or the desktop app repeats,
/// presents the token it replaced. Within the retry window that is the same refresh, not
/// a replay, and it must not sign the customer out.
#[tokio::test]
async fn a_refresh_retried_within_the_grace_window_is_not_a_logout() {
    let engine = Arc::new(BillingEngine::new());
    engine.upsert_card(active_card());
    let auth = AuthState::with_billing(SECRET, (*engine).clone());
    let handler = RefreshTokenHandler::new(engine.clone(), auth.clone());
    let signed_in = token(&auth, &active_card());
    let (status, _lost) = refresh(&handler, json!({"refreshToken": signed_in})).await;
    assert_eq!(status, StatusCode::OK);
    let (status, retried) = refresh(&handler, json!({"refreshToken": signed_in})).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a retried refresh signed the customer out"
    );
    let (status, _) = refresh(&handler, json!({"refreshToken": retried["refreshToken"]})).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the retry's token must carry the family on"
    );
    // Two rotations behind is no retry.
    let (status, _) = refresh(&handler, json!({"refreshToken": signed_in})).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

/// One card chaining refreshes neither grows the state nor gets past its budget, and is
/// told to retry rather than signed out.
#[tokio::test]
async fn one_card_chaining_refreshes_is_throttled_and_leaves_state_flat() {
    let (engine, auth, handler) = setup();
    let size =
        |engine: &BillingEngine| serde_json::to_vec(&engine.export_snapshot()).unwrap().len();
    let (status, first) = refresh(
        &handler,
        json!({"refreshToken": token(&auth, &active_card())}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let mut raw = first["refreshToken"].as_str().unwrap().to_string();
    let after_one = size(&engine);
    for _ in 1..gateway::auth::CARD_TOKEN_BUDGET {
        let (status, next) = refresh(&handler, json!({"refreshToken": raw})).await;
        assert_eq!(status, StatusCode::OK);
        raw = next["refreshToken"].as_str().unwrap().to_string();
    }
    assert!(
        size(&engine) <= after_one + 16,
        "the state grew with each refresh"
    );
    assert!(engine.export_snapshot().consumed_refresh_tokens.is_empty());
    let request = Request::builder()
        .method("POST")
        .uri("/refreshToken")
        .header("content-type", "application/json")
        .body(Body::from(json!({"refreshToken": raw}).to_string()))
        .unwrap();
    let response = handler.handle(request).await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    let wait: u64 = response.headers()["retry-after"]
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!((1..=3600).contains(&wait), "{wait}");
    let body: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(body["__type"], "ThrottlingException");
}

/// Kiro retries a refused refresh every minute; those refusals must not use up the budget.
#[tokio::test]
async fn refused_refreshes_do_not_use_the_budget() {
    let (_, auth, handler) = setup();
    let signed_in = token(&auth, &active_card());
    let (_, current) = refresh(&handler, json!({"refreshToken": signed_in})).await;
    for _ in 0..gateway::auth::CARD_TOKEN_BUDGET + 5 {
        assert_eq!(
            refresh(&handler, json!({"refreshToken": signed_in}))
                .await
                .0,
            StatusCode::UNAUTHORIZED
        );
    }
    let (status, _) = refresh(&handler, json!({"refreshToken": current["refreshToken"]})).await;
    assert_eq!(status, StatusCode::OK);
}

/// A sign-in starts a new refresh family, so a token copied off the device before it stops
/// working at once.
#[tokio::test]
async fn a_new_sign_in_retires_older_refresh_tokens() {
    let (engine, auth, handler) = setup();
    let before = token(&auth, &engine.get_card(CARD).unwrap());
    engine
        .activate_card_with_device(
            CARD,
            1,
            86_400,
            Some("dev_0123456789abcdef0123456789abcdef"),
        )
        .unwrap();
    let current = engine.get_card(CARD).unwrap();
    assert_eq!(current.refresh_version, 2);
    assert_eq!(
        refresh(&handler, json!({"refreshToken": before})).await.0,
        StatusCode::UNAUTHORIZED
    );
    let (status, _) = refresh(&handler, json!({"refreshToken": token(&auth, &current)})).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn the_refresh_endpoint_limits_each_source() {
    let (_, _, handler) = setup();
    for _ in 0..60 {
        let (status, _) = refresh(&handler, json!({"refreshToken": "not-a-jwt"})).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
    let (status, body) = refresh(&handler, json!({"refreshToken": "not-a-jwt"})).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(body["__type"], "ThrottlingException");
}
