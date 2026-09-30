use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use billing::engine::{BillingEngine, RuntimeSettingsUpdate};
use gateway::facade::{
    admin::AdminAuthState, runtime_settings_admin::RuntimeSettingsHandler, FacadeHandler,
};
use std::sync::Arc;
#[tokio::test]
async fn runtime_settings_require_auth_version_and_valid_bounds() {
    let billing = BillingEngine::new();
    let auth = Arc::new(AdminAuthState::new("runtime-test-key"));
    let get = RuntimeSettingsHandler {
        billing: billing.clone(),
        auth: auth.clone(),
        publish: false,
    };
    let post = RuntimeSettingsHandler {
        billing: billing.clone(),
        auth,
        publish: true,
    };
    assert_eq!(
        get.handle(Request::builder().body(Body::empty()).unwrap())
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        post.handle(Request::builder().body(Body::empty()).unwrap())
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let original = billing.runtime_settings_config();
    let mut update = RuntimeSettingsUpdate {
        expected_revision: original.revision,
        reason: "runtime test".into(),
        settings: original.settings,
    };
    update.settings.claude.headers_secs = 100;
    let request = |u: &RuntimeSettingsUpdate| {
        Request::builder()
            .method("POST")
            .header("x-admin-key", "runtime-test-key")
            .body(Body::from(serde_json::to_vec(u).unwrap()))
            .unwrap()
    };
    assert_eq!(post.handle(request(&update)).await.status(), StatusCode::OK);
    assert_eq!(
        post.handle(request(&update)).await.status(),
        StatusCode::CONFLICT
    );
    let saved = billing.runtime_settings_config();
    update.expected_revision = saved.revision.clone();
    update.settings.keepalive_secs = 0;
    assert!(!post.handle(request(&update)).await.status().is_success());
    assert_eq!(billing.runtime_settings_config(), saved);
    let bytes = serde_json::to_vec(&billing.export_snapshot()).unwrap();
    let restored = BillingEngine::new();
    restored.import_snapshot(serde_json::from_slice(&bytes).unwrap());
    assert_eq!(restored.runtime_settings_config(), saved);
}
