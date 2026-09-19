//! Dual-blind audit test for Gateway Client White-labeling API (Spec §14.6, P4-8).

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use gateway::facade::client::{
    ClientBrandHandler, ClientNegotiateHandler, ClientNegotiateResponse, WhiteLabelConfig,
};
use gateway::facade::{FacadeHandler, FacadeRegistry};

#[tokio::test]
async fn test_gateway_client_brand_endpoint_public_access() {
    let brand = WhiteLabelConfig {
        app_name: "Apex AI Copilot".to_string(),
        official_website: "https://apex.corp.internal".to_string(),
        primary_color: "#0284C7".to_string(),
        ..WhiteLabelConfig::default()
    };

    let handler = ClientBrandHandler {
        config: brand.clone(),
    };

    let req = Request::builder()
        .method(Method::GET)
        .uri("/client/brand")
        .body(Body::empty())
        .unwrap();

    let resp = handler.handle(req).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .unwrap();
    let body_json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    assert_eq!(body_json["status"], "ok");
    assert_eq!(body_json["brand"]["appName"], "Apex AI Copilot");
    assert_eq!(
        body_json["brand"]["officialWebsite"],
        "https://apex.corp.internal"
    );
    assert_eq!(body_json["brand"]["primaryColor"], "#0284C7");
}

#[tokio::test]
async fn test_gateway_negotiate_delivers_white_label_config() {
    let handler = ClientNegotiateHandler;

    let req = Request::builder()
        .method(Method::POST)
        .uri("/client/negotiate")
        .body(Body::empty())
        .unwrap();

    let resp = handler.handle(req).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .unwrap();
    let neg_resp: ClientNegotiateResponse = serde_json::from_slice(&bytes).unwrap();

    let wl = neg_resp
        .white_label
        .expect("white_label must be present in negotiation response");
    assert_eq!(wl.app_name, "KIRO 极速接管中心");
    assert_eq!(wl.official_website, "https://byok.kiro.dev");
    assert_eq!(wl.primary_color, "#6366F1");
}

#[tokio::test]
async fn test_router_with_auth_allows_client_brand_without_token() {
    let auth = gateway::auth::AuthState::new("test-secret-key-brand-public-access-32bytes!!");

    let mut registry = FacadeRegistry::new();
    registry.register_default_facades();

    let app = registry.into_router_with_auth(auth);

    // Call /client/brand without Authorization header
    let req = Request::builder()
        .method(Method::GET)
        .uri("/client/brand")
        .body(Body::empty())
        .unwrap();

    let resp = tower::ServiceExt::oneshot(app, req).await.unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "Public /client/brand must be accessible without Bearer token"
    );
}
