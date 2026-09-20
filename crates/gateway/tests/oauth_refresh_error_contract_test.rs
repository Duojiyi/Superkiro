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
    let auth = AuthState::with_billing(SECRET, (*engine).clone());
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
