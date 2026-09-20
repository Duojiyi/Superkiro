use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use billing::{BillingEngine, Card, CardTemplate, MasterKek};
use gateway::facade::FacadeRegistry;
use serde_json::{json, Value};
use tower::ServiceExt;

const KEY: &str = "test-card-recovery-admin-key-32chars";

#[tokio::test]
async fn authenticated_recovery_and_no_store() {
    let billing = BillingEngine::new();
    billing.set_master_kek(MasterKek::from_bytes([37; 32]));
    let cards = billing
        .issue_cards(&CardTemplate::monthly("monthly", "group"), 1, None, 1)
        .unwrap();
    billing.upsert_card(Card::new("legacy", "group", 10));
    let mut registry = FacadeRegistry::new();
    registry.register_admin_facades(billing.clone(), KEY.into());
    let app = registry.into_router_with_auth(gateway::auth::AuthState::new(
        "test-auth-secret-key-32bytes-long-ok!!",
    ));
    for (key, body, status) in [
        (
            "",
            json!({"cardId": cards[0].card.id}),
            StatusCode::UNAUTHORIZED,
        ),
        (
            "wrong",
            json!({"cardId": cards[0].card.id}),
            StatusCode::UNAUTHORIZED,
        ),
        (KEY, json!({"cardId": "legacy"}), StatusCode::NOT_FOUND),
        (KEY, json!({"cardId": "missing"}), StatusCode::NOT_FOUND),
        (KEY, json!({"cardId": ""}), StatusCode::BAD_REQUEST),
        (KEY, json!({}), StatusCode::BAD_REQUEST),
        (KEY, json!({"cardId": cards[0].card.id}), StatusCode::OK),
    ] {
        let req = Request::post("/api/v1/admin/cards/reveal")
            .header("x-admin-key", key)
            .body(Body::from(body.to_string()))
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), status);
        assert_eq!(response.headers()["cache-control"], "no-store");
        let bytes = axum::body::to_bytes(response.into_body(), 65536)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        if status == StatusCode::OK {
            assert_eq!(body, json!({"success": true, "rawCode": cards[0].raw_code}));
        } else {
            assert!(body.get("rawCode").is_none());
        }
    }
    let req = Request::get("/api/v1/admin/cards")
        .header("x-admin-key", KEY)
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    let bytes = axum::body::to_bytes(response.into_body(), 65536)
        .await
        .unwrap();
    let text = std::str::from_utf8(&bytes).unwrap();
    assert!(!text.contains(&cards[0].raw_code));
    assert!(!text.contains(cards[0].card.code_encrypted.as_ref().unwrap()));
    assert!(!text.contains(&cards[0].card.code_hash));
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    let listed = body["cards"].as_array().unwrap();
    for card in listed {
        assert_eq!(card["codeRecoverable"], card["id"] != "legacy");
        assert!(card.get("codeEncrypted").is_none());
        assert!(card.get("rawCode").is_none());
    }
    billing.set_master_kek(MasterKek::from_bytes([38; 32]));
    let req = Request::post("/api/v1/admin/cards/reveal")
        .header("x-admin-key", KEY)
        .body(Body::from(json!({"cardId": cards[0].card.id}).to_string()))
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(response.headers()["cache-control"], "no-store");
}

#[tokio::test]
async fn batch_without_kek_fails_closed() {
    let billing = BillingEngine::new();
    assert!(
        billing.master_kek().is_none(),
        "run test without KIRO_MASTER_KEK"
    );
    let mut registry = FacadeRegistry::new();
    registry.register_admin_facades(billing.clone(), KEY.into());
    let app = registry.into_router_with_auth(gateway::auth::AuthState::new(
        "test-auth-secret-key-32bytes-long-ok!!",
    ));
    let req = Request::post("/api/v1/admin/cards/batch")
        .header("x-admin-key", KEY)
        .body(Body::from(
            json!({"count": 1, "groupId": "group-pro-plus"}).to_string(),
        ))
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert!(billing.list_all_cards().is_empty());
}
