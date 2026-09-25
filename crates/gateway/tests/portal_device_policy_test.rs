use axum::{
    body::Body,
    http::{Request, StatusCode},
    Router,
};
use billing::{BillingEngine, Card};
use gateway::{auth::AuthState, facade::FacadeRegistry};
use serde_json::{json, Value};
use tower::ServiceExt;

const CARD_CODE: &str = "test-only-unbind-policy-card";

fn setup(max_rebinds: u32) -> (BillingEngine, AuthState, Router) {
    let billing = BillingEngine::new();
    let mut card = Card::new("card", "group-pro-plus", 1_000_000_000);
    card.code_hash = billing::card::hash_card_code(CARD_CODE);
    card.max_rebinds = max_rebinds;
    card.rebind_cooldown_secs = 300;
    billing.upsert_card(card);
    billing
        .activate_card_with_device("card", gateway::now_secs(), 86400, Some("old"))
        .unwrap();
    let auth =
        AuthState::with_billing("test-only-unbind-secret-at-least-32-bytes", billing.clone());
    let mut registry = FacadeRegistry::new();
    registry.register_portal_facades(billing.clone(), None);
    let app = registry.into_router_with_auth(auth.clone());
    (billing, auth, app)
}

async fn post(app: &Router, path: &str, payload: Value) -> (StatusCode, Value) {
    let req = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json")
        .body(Body::from(payload.to_string()))
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 65536)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

async fn unbind(app: &Router, device: &str) -> (StatusCode, Value) {
    let (status, challenge) =
        post(app, "/api/v1/portal/challenge", json!({"action":"unbind"})).await;
    assert_eq!(status, StatusCode::OK);
    post(
        app,
        "/api/v1/portal/unbind",
        json!({
            "card": CARD_CODE, "device": device, "challengeToken": challenge["challengeToken"]
        }),
    )
    .await
}

#[tokio::test]
async fn portal_unbind_updates_query_revokes_both_tokens_and_enforces_cooldown() {
    let (billing, auth, app) = setup(2);
    let before = billing.get_card("card").unwrap();
    let (status, query) = post(&app, "/api/v1/portal/query", json!({"card": CARD_CODE})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(query["rebindCount"], 0);
    assert_eq!(query["maxRebinds"], 2);
    assert_eq!(
        billing.get_card("card").unwrap(),
        before,
        "query must be read-only"
    );
    let access = auth.issue_token_for_card("card", 3600).unwrap();
    let refresh = auth
        .issue_refresh_token(
            "card",
            &before.group_id,
            before.token_version,
            before.refresh_version,
            3600,
        )
        .unwrap();
    assert!(auth.verify_token(&access).is_ok());

    let (status, result) = unbind(&app, "old").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["remainingDevices"], json!([]));
    assert!(auth.verify_token(&access).is_err());
    assert!(auth.rotate_refresh_token(&refresh, 3600, 3600).is_err());
    let (_, query) = post(&app, "/api/v1/portal/query", json!({"card": CARD_CODE})).await;
    assert_eq!(query["rebindCount"], 1);
    assert_eq!(query["maxRebinds"], 2);

    billing
        .activate_card_with_device("card", gateway::now_secs(), 86400, Some("new"))
        .unwrap();
    let bound = billing.get_card("card").unwrap();
    assert_eq!(bound.rebind_count, 1);
    let (status, result) = unbind(&app, "new").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(result["success"], false);
    assert_eq!(billing.get_card("card").unwrap(), bound);
}

#[tokio::test]
async fn portal_unbind_limit_rejection_preserves_binding_and_tokens() {
    let (billing, auth, app) = setup(0);
    let before = billing.get_card("card").unwrap();
    let access = auth.issue_token_for_card("card", 3600).unwrap();
    let refresh = auth
        .issue_refresh_token(
            "card",
            &before.group_id,
            before.token_version,
            before.refresh_version,
            3600,
        )
        .unwrap();
    let (status, result) = unbind(&app, "old").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(result["success"], false);
    assert_eq!(billing.get_card("card").unwrap(), before);
    assert!(auth.verify_token(&access).is_ok());
    assert!(auth.rotate_refresh_token(&refresh, 3600, 3600).is_ok());
}

#[tokio::test]
async fn portal_activate_cannot_replace_device_and_can_fill_explicitly_unbound_slot() {
    let (billing, _auth, app) = setup(1);
    let before = billing.get_card("card").unwrap();
    let (status, result) = post(
        &app,
        "/api/v1/portal/activate",
        json!({"card":CARD_CODE,"device":"new"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(result["success"], false);
    assert!(result["error"]
        .as_str()
        .unwrap()
        .contains("unbind it on the portal"));
    assert_eq!(billing.get_card("card").unwrap(), before);
    assert_eq!(unbind(&app, "old").await.0, StatusCode::OK);
    let (status, _) = post(
        &app,
        "/api/v1/portal/activate",
        json!({"card":CARD_CODE,"device":"new"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let card = billing.get_card("card").unwrap();
    assert_eq!(card.bound_devices, vec!["new"]);
    assert_eq!(card.rebind_count, 1);
    assert_eq!(card.token_version, 2);
}

#[tokio::test]
async fn the_card_code_alone_never_reveals_the_bound_device_yet_the_portal_can_unbind_it() {
    let fingerprint = format!("dev_{}", "4f".repeat(30));
    let billing = BillingEngine::new();
    let mut card = Card::new("card", "group-pro-plus", 1_000_000_000);
    card.code_hash = billing::card::hash_card_code(CARD_CODE);
    card.max_rebinds = 2;
    billing.upsert_card(card);
    billing
        .activate_card_with_device("card", gateway::now_secs(), 86400, Some(&fingerprint))
        .unwrap();
    let auth =
        AuthState::with_billing("test-only-unbind-secret-at-least-32-bytes", billing.clone());
    let mut registry = FacadeRegistry::new();
    registry.register_portal_facades(billing.clone(), None);
    let app = registry.into_router_with_auth(auth);

    let (status, query) = post(&app, "/api/v1/portal/query", json!({"card": CARD_CODE})).await;
    assert_eq!(status, StatusCode::OK);
    let shown = query["boundDevices"][0].as_str().unwrap().to_string();
    assert!(!shown.contains(&fingerprint[..20]), "{shown}");
    assert!(shown.len() < 16, "{shown}");
    assert!(
        fingerprint.ends_with(&shown[4..]),
        "the customer can still recognise it"
    );

    // The portal unbinds the device it was shown, and never learns the id afterwards either.
    let (status, result) = unbind(&app, &shown).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["remainingDevices"], json!([]));
    assert!(billing.get_card("card").unwrap().bound_devices.is_empty());
}

#[tokio::test]
async fn a_masked_name_that_matches_no_bound_device_unbinds_nothing() {
    let (billing, _, app) = setup(2);
    let (status, _) = unbind(&app, "****zzzz").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(billing.get_card("card").unwrap().bound_devices, vec!["old"]);
}
