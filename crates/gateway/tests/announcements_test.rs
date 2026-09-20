//! Public announcements must use the shared billing store without user/admin credentials.
use axum::{
    body::{to_bytes, Body},
    http::{Method, Request, StatusCode},
};
use billing::{
    observability::{Announcement, AnnouncementLevel},
    BillingEngine,
};
use gateway::facade::FacadeRegistry;
use serde_json::{json, Value};
use tower::ServiceExt;

fn setup() -> (BillingEngine, axum::Router) {
    let billing = BillingEngine::default();
    let mut registry = FacadeRegistry::new();
    registry.register_portal_facades(billing.clone(), None);
    let auth = gateway::auth::AuthState::new("test-announcements-auth-secret-32bytes");
    (billing, registry.into_router_with_auth(auth))
}

#[tokio::test]
async fn anonymous_announcements_are_active_display_fields_only() {
    let (billing, app) = setup();
    for (id, level) in [
        ("a", AnnouncementLevel::Info),
        ("b", AnnouncementLevel::Warning),
        ("c", AnnouncementLevel::Critical),
    ] {
        billing.add_announcement(Announcement::new(id, "Title", "Content", level, 42));
    }
    let mut disabled =
        Announcement::new("disabled", "Hidden", "Hidden", AnnouncementLevel::Info, 0);
    disabled.enabled = false;
    billing.add_announcement(disabled);
    billing.add_announcement(
        Announcement::new("expired", "Hidden", "Hidden", AnnouncementLevel::Info, 0).with_expiry(1),
    );
    billing.add_announcement(
        Announcement::new("future", "Title", "Content", AnnouncementLevel::Info, 42)
            .with_expiry(u64::MAX),
    );
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/announcements")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let value: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 65536).await.unwrap()).unwrap();
    assert_eq!(value.as_object().unwrap().len(), 2);
    assert_eq!(value["success"], true);
    let rows = value["announcements"].as_array().unwrap();
    assert_eq!(rows.len(), 4);
    for (id, level) in [
        ("a", "info"),
        ("b", "warning"),
        ("c", "critical"),
        ("future", "info"),
    ] {
        let row = rows.iter().find(|r| r["id"] == id).unwrap();
        assert_eq!(
            row,
            &json!({"id":id,"level":level,"title":"Title","content":"Content",
            "created_at":42,"expires_at":if id == "future" { json!(u64::MAX) } else { Value::Null }})
        );
    }
}

#[tokio::test]
async fn empty_is_success_and_writes_are_not_allowed() {
    let (_, app) = setup();
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/announcements")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let value: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 65536).await.unwrap()).unwrap();
    assert_eq!(value, json!({"success":true,"announcements":[]}));
    for method in [Method::POST, Method::PUT, Method::PATCH, Method::DELETE] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri("/api/v1/announcements")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    }
}
