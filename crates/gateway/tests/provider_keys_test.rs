use axum::{
    body::{to_bytes, Body},
    http::Request,
};
use billing::{
    crypto::MasterKek,
    provider::{Provider, ProviderFormat, ProviderKey},
    BillingEngine,
};
use gateway::facade::{admin::AdminAuthState, commercial::ProviderKeysHandler, FacadeHandler};
use gateway::provider::ProviderRuntimeRegistry;
use std::sync::Arc;
use wiremock::{
    matchers::{header, method, path},
    Mock, MockServer, ResponseTemplate,
};

fn setup(
    base: &str,
    discover: bool,
) -> (ProviderKeysHandler, BillingEngine, ProviderRuntimeRegistry) {
    let billing = BillingEngine::new();
    billing.set_master_kek(MasterKek::from_bytes([3; 32]));
    billing.upsert_provider(Provider::new("p", "P", ProviderFormat::OpenAi, base));
    billing.upsert_provider_key(ProviderKey::new("k", "p", "test-secret"));
    let runtime = ProviderRuntimeRegistry::new();
    runtime.sync_from_billing(&billing);
    (
        ProviderKeysHandler {
            billing: billing.clone(),
            auth: Arc::new(AdminAuthState::new("admin")),
            discover,
            runtime: Some(runtime.clone()),
        },
        billing,
        runtime,
    )
}
fn request(payload: serde_json::Value, auth: bool) -> Request<Body> {
    let mut r = Request::builder().method("POST");
    if auth {
        r = r.header("x-admin-key", "admin");
    }
    r.body(Body::from(payload.to_string())).unwrap()
}
#[tokio::test]
async fn save_permissions_is_authenticated_persistent_and_updates_runtime() {
    let (h, b, r) = setup("https://example.com", false);
    let value = serde_json::json!({"provider_id":"p","key_id":"k","allowed_models":["opus","opus"],"weight":2});
    assert_eq!(h.handle(request(value.clone(), false)).await.status(), 401);
    let response = h.handle(request(value, true)).await;
    assert_eq!(response.status(), 200);
    let text = String::from_utf8(
        to_bytes(response.into_body(), 100000)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(!text.contains("test-secret"));
    assert!(b.list_provider_keys(None)[0].api_key_encrypted.is_none());
    let pool = r.pool_for("p").unwrap();
    assert!(pool.select_key_for_model(0, &[], Some("opus")).is_ok());
    assert!(pool.select_key_for_model(0, &[], Some("sonnet")).is_err());
    let restored = BillingEngine::new();
    restored.import_snapshot(b.export_snapshot());
    assert_eq!(
        restored.list_provider_keys(None)[0].allowed_models,
        Some(vec!["opus".into()])
    );
    assert!(b.commercial_config().models.is_empty());
}
#[tokio::test]
async fn discovery_deduplicates_without_publishing_or_changing_permissions() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .and(header("authorization", "Bearer test-secret"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            serde_json::json!({"data":[{"id":"opus"},{"id":"sonnet"},{"id":"opus"}]}),
        ))
        .expect(1)
        .mount(&server)
        .await;
    let (h, b, _) = setup(&server.uri(), true);
    let response = h
        .handle(request(
            serde_json::json!({"provider_id":"p","key_id":"k"}),
            true,
        ))
        .await;
    assert_eq!(response.status(), 200);
    let v: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 100000).await.unwrap()).unwrap();
    assert_eq!(v["models"], serde_json::json!(["opus", "sonnet"]));
    assert_eq!(b.list_provider_keys(None)[0].allowed_models, None);
    assert!(b.commercial_config().models.is_empty());
}
#[tokio::test]
async fn new_keys_require_explicit_permissions() {
    let (h, b, _) = setup("https://example.com", false);
    assert_eq!(
        h.handle(request(
            serde_json::json!({"provider_id":"p","key_id":"new","api_key":"secret"}),
            true
        ))
        .await
        .status(),
        400
    );
    assert_eq!(b.list_provider_keys(None).len(), 1);
}

#[tokio::test]
async fn empty_permission_list_disables_routing_and_rotation_is_secret_safe() {
    let (h, b, r) = setup("https://example.com", false);
    let response=h.handle(request(serde_json::json!({"provider_id":"p","key_id":"k","allowed_models":[],"api_key":"rotated-secret"}),true)).await;
    assert_eq!(response.status(), 200);
    assert!(r
        .pool_for("p")
        .unwrap()
        .select_key_for_model(0, &[], Some("opus"))
        .is_err());
    let runtime = b.get_runtime_provider_keys(None);
    assert_eq!(runtime[0].api_key, "rotated-secret");
    let body = String::from_utf8(
        to_bytes(response.into_body(), 100000)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(!body.contains("rotated-secret"));
}

#[tokio::test]
async fn rejected_discovery_preserves_permissions() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(401).set_body_string("secret must not be echoed"))
        .mount(&server)
        .await;
    let (h, b, _) = setup(&server.uri(), true);
    let response = h
        .handle(request(
            serde_json::json!({"provider_id":"p","key_id":"k"}),
            true,
        ))
        .await;
    assert_eq!(response.status(), 502);
    let body = String::from_utf8(
        to_bytes(response.into_body(), 100000)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(!body.contains("secret must not"));
    assert_eq!(b.list_provider_keys(None)[0].allowed_models, None);
}
