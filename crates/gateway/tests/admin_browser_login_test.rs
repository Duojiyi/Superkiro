//! Full production router: Cookie login, CSRF, real reveal handler and revocation.
use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use billing::{BillingEngine, CardTemplate, MasterKek};
use gateway::facade::FacadeRegistry;
use serde_json::{json, Value};
use tower::ServiceExt;

const KEY: &str = "browser-test-signing-key-32bytes-long";
fn app(billing: &BillingEngine) -> axum::Router {
    let mut registry = FacadeRegistry::new();
    registry.register_admin_facades_secure(billing.clone(), KEY.into());
    registry.into_router_with_auth(gateway::auth::AuthState::new(
        "browser-test-client-secret-32bytes-long",
    ))
}
fn request(
    method: &str,
    path: &str,
    cookie: Option<&str>,
    csrf: Option<&str>,
    body: Value,
) -> Request<Body> {
    let mut req = Request::builder()
        .method(method)
        .uri(format!("/api/v1/admin/{path}"))
        .header("origin", "https://admin.test")
        .header("content-type", "application/json");
    if let Some(value) = cookie {
        req = req.header("cookie", value);
    }
    if let Some(value) = csrf {
        req = req.header("x-csrf-token", value);
    }
    req.body(Body::from(body.to_string())).unwrap()
}
async fn json_body(response: axum::response::Response) -> Value {
    serde_json::from_slice(&to_bytes(response.into_body(), 65536).await.unwrap()).unwrap()
}

// One test owns process-local env; no other tests in this binary mutate it concurrently.
#[tokio::test]
async fn production_cookie_login_csrf_reveal_logout_and_fail_closed() {
    std::env::remove_var("ADMIN_TOTP_SECRET");
    let billing = BillingEngine::new();
    billing.set_master_kek(MasterKek::from_bytes([23; 32]));
    let issued = billing
        .issue_cards(&CardTemplate::monthly("monthly", "group"), 1, None, 1)
        .unwrap();
    std::env::remove_var("ADMIN_BROWSER_LOGIN");
    std::env::set_var("ADMIN_ORIGIN", "https://admin.test");
    std::env::set_var(
        "ADMIN_PASSWORD_HASH",
        bcrypt::hash("test-password", 4).unwrap(),
    );
    let closed = app(&billing);
    for (method, path) in [("GET", "me"), ("POST", "session"), ("POST", "cards/reveal")] {
        let mut req = request(
            method,
            path,
            None,
            None,
            json!({"cardId": issued[0].card.id}),
        );
        req.headers_mut()
            .insert("x-admin-key", KEY.parse().unwrap());
        req.headers_mut()
            .insert("authorization", format!("Bearer {KEY}").parse().unwrap());
        let response = closed.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(!response.headers().contains_key("www-authenticate"));
    }
    std::env::set_var("ADMIN_BROWSER_LOGIN", "true");
    let app = app(&billing);
    let response = app
        .clone()
        .oneshot(request(
            "POST",
            "session",
            None,
            None,
            json!({"username":"admin","password":"test-password"}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let cookie_header = response.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .to_owned();
    for flag in [
        "__Host-admin_session=",
        "Secure",
        "HttpOnly",
        "SameSite=Strict",
        "Path=/",
    ] {
        assert!(cookie_header.contains(flag));
    }
    let cookie = cookie_header.split(';').next().unwrap();
    let login = json_body(response).await;
    assert!(login.get("accessToken").is_none());
    assert_eq!(login["twoFactorEnabled"], false);
    assert_eq!(login["totpRequired"], false);
    assert_eq!(login["authenticated"], true);
    assert!(login["expiresAt"].as_u64().is_some());
    let response = app
        .clone()
        .oneshot(request("GET", "session", Some(cookie), None, json!(null)))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let session = json_body(response).await;
    assert_eq!(session["expiresAt"], login["expiresAt"]);
    assert!(session["expiresIn"].as_u64().unwrap() <= 900);
    assert_eq!(session["twoFactorEnabled"], false);
    let csrf = session["csrfToken"].as_str().unwrap();
    for (token, expected) in [
        (None, StatusCode::FORBIDDEN),
        (Some("wrong"), StatusCode::FORBIDDEN),
        (Some(csrf), StatusCode::OK),
    ] {
        let response = app
            .clone()
            .oneshot(request(
                "POST",
                "cards/reveal",
                Some(cookie),
                token,
                json!({"cardId":issued[0].card.id}),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
        assert_eq!(response.headers()["cache-control"], "no-store");
        let body = json_body(response).await;
        if expected == StatusCode::OK {
            assert_eq!(body["rawCode"], issued[0].raw_code);
        } else {
            assert!(body.get("rawCode").is_none());
        }
    }
    let response = app
        .clone()
        .oneshot(request(
            "POST",
            "session/revoke",
            Some(cookie),
            Some(csrf),
            json!({"all":false}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .contains("Max-Age=0"));
    for (method, path) in [("GET", "session"), ("POST", "cards/reveal")] {
        assert_eq!(
            app.clone()
                .oneshot(request(
                    method,
                    path,
                    Some(cookie),
                    Some(csrf),
                    json!({"cardId":issued[0].card.id})
                ))
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
    }
    // Configured but invalid must fail closed, never downgrade to password-only.
    std::env::set_var("ADMIN_TOTP_SECRET", "invalid");
    let invalid = self::app(&billing);
    assert_eq!(
        invalid
            .oneshot(request(
                "POST",
                "session",
                None,
                None,
                json!({"username":"admin","password":"test-password"})
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    // Published fixture, checked only for safe challenge/status and startup quarantine.
    std::env::set_var("ADMIN_TOTP_SECRET", "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ");
    let enabled = self::app(&billing);
    let response = enabled
        .clone()
        .oneshot(request("GET", "session", None, None, json!(null)))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body = json_body(response).await;
    assert_eq!(body["twoFactorEnabled"], true);
    assert_eq!(body["totpRequired"], true);
    assert!(body.get("secret").is_none());
    assert_eq!(
        enabled
            .oneshot(request(
                "POST",
                "session",
                None,
                None,
                json!({"username":"admin","password":"test-password"})
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    for key in [
        "ADMIN_BROWSER_LOGIN",
        "ADMIN_ORIGIN",
        "ADMIN_PASSWORD_HASH",
        "ADMIN_TOTP_SECRET",
    ] {
        std::env::remove_var(key);
    }
}
