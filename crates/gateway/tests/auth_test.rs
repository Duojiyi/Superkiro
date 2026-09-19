use axum::{
    body::Body,
    extract::Extension,
    http::{header, Method, Request, StatusCode},
    middleware::from_fn_with_state,
    response::IntoResponse,
    routing::get,
    Router,
};
use gateway::auth::{auth_middleware, AuthClaims, AuthState};
use http_body_util::BodyExt;
use jsonwebtoken::{encode, EncodingKey, Header};
use serde_json::Value;
use tower::ServiceExt;

async fn protected_endpoint(Extension(claims): Extension<AuthClaims>) -> impl IntoResponse {
    axum::Json(serde_json::json!({
        "status": "authenticated",
        "card_id": claims.card_id,
        "group_id": claims.group_id,
        "token_version": claims.token_version,
    }))
}

fn create_test_app(auth_state: AuthState) -> Router {
    Router::new()
        .route("/protected", get(protected_endpoint))
        .layer(from_fn_with_state(auth_state, auth_middleware))
}

async fn call_endpoint(app: Router, auth_header: Option<&str>) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(Method::GET).uri("/protected");
    if let Some(h) = auth_header {
        builder = builder.header(header::AUTHORIZATION, h);
    }
    let req = builder.body(Body::empty()).unwrap();
    let response = app.oneshot(req).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

#[tokio::test]
async fn test_auth_valid_token_allowed() {
    let auth = AuthState::with_default_dev_card();
    let app = create_test_app(auth.clone());

    let token = auth
        .issue_token("card-dev-001", "group-pro-plus", 1, 3600)
        .expect("token should issue");

    let (status, json) = call_endpoint(app, Some(&format!("Bearer {}", token))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["status"], "authenticated");
    assert_eq!(json["card_id"], "card-dev-001");
    assert_eq!(json["group_id"], "group-pro-plus");
    assert_eq!(json["token_version"], 1);
}

#[tokio::test]
async fn test_auth_missing_header_rejected() {
    let auth = AuthState::with_default_dev_card();
    let app = create_test_app(auth);

    let (status, json) = call_endpoint(app, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(json["__type"], "MissingAuthenticationTokenException");
}

#[tokio::test]
async fn test_auth_malformed_header_rejected() {
    let auth = AuthState::with_default_dev_card();
    let app = create_test_app(auth);

    let (status, json) = call_endpoint(app, Some("Basic dXNlcjpwYXNz")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(json["__type"], "UnrecognizedClientException");
}

/// Audit B - Counterexample 1: Tampered signature must be rejected with 401
#[tokio::test]
async fn test_audit_b_tampered_signature_rejected() {
    let auth = AuthState::with_default_dev_card();
    let app = create_test_app(auth.clone());

    // Generate token signed with an alien secret key
    let alien_key = EncodingKey::from_secret(b"completely-wrong-alien-secret-key-1234");
    let claims = AuthClaims {
        card_id: "card-dev-001".to_string(),
        group_id: "group-pro-plus".to_string(),
        token_version: 1,
        exp: 2_000_000_000,
        iat: 1_000_000_000,
    };
    let tampered_token = encode(&Header::default(), &claims, &alien_key).unwrap();

    let (status, json) = call_endpoint(app, Some(&format!("Bearer {}", tampered_token))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(json["__type"], "UnrecognizedClientException");
    // P1-07: sanitized — no longer leaks "Invalid signature" detail
    assert!(json["message"]
        .as_str()
        .unwrap()
        .contains("Invalid authentication credentials"));
}

/// Audit B - Counterexample 2: Modified token_version must trigger instant revocation (401)
#[tokio::test]
async fn test_audit_b_revoked_token_version_mismatch_rejected() {
    let auth = AuthState::with_default_dev_card();
    let app = create_test_app(auth.clone());

    // Issue token v1
    let token_v1 = auth
        .issue_token("card-dev-001", "group-pro-plus", 1, 3600)
        .unwrap();

    // Verify it works initially
    let (status1, _) = call_endpoint(app.clone(), Some(&format!("Bearer {}", token_v1))).await;
    assert_eq!(status1, StatusCode::OK);

    // Now revoke the card (increments token_version from 1 -> 2)
    let new_ver = auth.revoke_card("card-dev-001");
    assert_eq!(new_ver, Some(2));

    // Existing token v1 MUST be rejected immediately (NOT waiting for 1h TTL)
    let (status2, json2) = call_endpoint(app, Some(&format!("Bearer {}", token_v1))).await;
    assert_eq!(status2, StatusCode::UNAUTHORIZED);
    // P1-07: sanitized — no longer leaks version numbers or "TokenRevokedException"
    assert_eq!(json2["__type"], "AccessDeniedException");
    assert!(json2["message"]
        .as_str()
        .unwrap()
        .contains("Invalid authentication credentials"));
}

/// Audit B - Counterexample 3: Expired token must be rejected with 401
#[tokio::test]
async fn test_audit_b_expired_token_rejected() {
    let auth = AuthState::with_default_dev_card();
    let app = create_test_app(auth.clone());

    // Create an expired token (exp in the past)
    let claims = AuthClaims {
        card_id: "card-dev-001".to_string(),
        group_id: "group-pro-plus".to_string(),
        token_version: 1,
        exp: 1_000_000_000, // Year 2001
        iat: 900_000_000,
    };
    let expired_token = encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(auth.secret.as_bytes()),
    )
    .unwrap();

    let (status, json) = call_endpoint(app, Some(&format!("Bearer {}", expired_token))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(json["__type"], "ExpiredTokenException");
}

/// Test frozen or inactive card rejection
#[tokio::test]
async fn test_frozen_card_rejected() {
    let auth = AuthState::with_default_dev_card();
    let app = create_test_app(auth.clone());

    let token = auth
        .issue_token("card-dev-001", "group-pro-plus", 1, 3600)
        .unwrap();

    // Freeze card
    auth.set_card_active("card-dev-001", false);

    let (status, json) = call_endpoint(app, Some(&format!("Bearer {}", token))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(json["__type"], "AccessDeniedException");
    // P1-07: sanitized — no longer leaks card status details
    assert!(json["message"]
        .as_str()
        .unwrap()
        .contains("Invalid authentication credentials"));
}
