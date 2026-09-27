use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
};
use billing::{BillingEngine, CardTemplate};
use gateway::{
    auth::AuthState,
    facade::{virtualization::VirtualizationStore, FacadeRegistry},
};
use serde_json::{json, Value};
use tower::ServiceExt;

#[tokio::test]
async fn issued_tiers_reach_portal_usage_and_subscriptions() {
    for (points, name, kind) in [
        (1000, "PRO", "PRO"),
        (2000, "PRO+", "PRO_PLUS"),
        (5000, "PRO Max", "PRO_MAX"),
        (10000, "Power", "POWER"),
    ] {
        let billing = BillingEngine::new();
        let template = CardTemplate::tier(&format!("tier-{points}"), "group-pro-plus").unwrap();
        let generated = billing::generate_card(&template, None, 0).unwrap();
        let mut card = generated.card;
        card.activate(gateway::now_secs(), 0).unwrap();
        card.credit_used = card.credit_total - 1;
        card.credit_total += 1_000_000; // balance adjustments never change the issued tier
        let id = card.id.clone();
        billing.upsert_card(card);
        let auth =
            AuthState::with_billing("test-tier-secret-at-least-32-characters", billing.clone());
        let token = auth.issue_token_for_card(&id, 3600).unwrap();
        let mut registry = FacadeRegistry::new();
        registry.register_virtualized_facades(VirtualizationStore::with_billing(
            billing.clone(),
            "group-pro-plus",
        ));
        registry.register_portal_facades(billing, None);
        let app = registry.into_router_with_auth(auth);
        for (method, path, body) in [
            (
                Method::POST,
                "/api/v1/portal/query",
                json!({"card": generated.raw_code}),
            ),
            (Method::GET, "/getUsageLimits", Value::Null),
            (Method::POST, "/listAvailableSubscriptions", json!({})),
        ] {
            let req = Request::builder()
                .method(method)
                .uri(path)
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap();
            let response = app.clone().oneshot(req).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let bytes = axum::body::to_bytes(response.into_body(), 65536)
                .await
                .unwrap();
            let value: Value = serde_json::from_slice(&bytes).unwrap();
            match path {
                "/api/v1/portal/query" => {
                    assert_eq!(value["virtualPlanName"], name);
                    assert_eq!(value["maxDevices"], 1);
                }
                "/getUsageLimits" => {
                    // The compact badge contains no expiry suffix.
                    assert_eq!(value["subscriptionInfo"]["subscriptionTitle"], name);
                    assert!(value["validUntil"].as_u64().is_some());
                    assert_eq!(value["subscriptionInfo"]["type"], kind);
                }
                _ => {
                    assert_eq!(value["subscriptionPlans"][0]["qSubscriptionType"], kind);
                    assert!(value["disclaimer"]
                        .as_str()
                        .unwrap()
                        .contains("not an official"));
                }
            }
        }
    }
}

/// A card issued from a plan tells the customer's client the plan it was sold as, though the
/// plan is renamed and retyped afterwards.
#[tokio::test]
async fn a_card_reports_the_plan_it_was_issued_from() {
    let billing = BillingEngine::new();
    billing.set_master_kek(billing::MasterKek::from_bytes([11; 32]));
    let plan = |name: &str, kind: &str| billing::template::Plan {
        id: "trial-7d".into(),
        name: name.into(),
        points: 300,
        price_cny: 9.9,
        validity_days: 7,
        max_devices: 1,
        concurrency: 1,
        default_group_id: "group-pro-plus".into(),
        kiro_plan_type: kind.into(),
        on_sale: true,
        sort_order: 5,
    };
    let publish = |plan: billing::template::Plan| {
        let update: billing::engine::CommercialUpdate = serde_json::from_value(json!({
            "expected_revision": billing.commercial_config().revision,
            "reason": "套餐",
            "plans": [plan],
        }))
        .unwrap();
        billing
            .publish_commercial_config(update, gateway::now_secs())
            .unwrap();
    };
    publish(plan("体验卡", "PRO"));
    let generated = billing
        .issue_cards(
            &billing.plan("trial-7d").unwrap().template("group-pro-plus"),
            1,
            None,
            gateway::now_secs(),
        )
        .unwrap()
        .remove(0);
    let id = generated.card.id.clone();
    let card = billing.activate_card(&id, gateway::now_secs(), 0).unwrap();
    assert_eq!(
        card.valid_until,
        card.activated_at.map(|at| at + 7 * 86_400)
    );
    publish(plan("体验卡Plus", "POWER"));
    let auth = AuthState::with_billing("test-tier-secret-at-least-32-characters", billing.clone());
    let token = auth.issue_token_for_card(&id, 3600).unwrap();
    let mut registry = FacadeRegistry::new();
    registry.register_virtualized_facades(VirtualizationStore::with_billing(
        billing.clone(),
        "group-pro-plus",
    ));
    registry.register_portal_facades(billing, None);
    let app = registry.into_router_with_auth(auth);
    for (method, path, body) in [
        (
            Method::POST,
            "/api/v1/portal/query",
            json!({"card": generated.raw_code}),
        ),
        (Method::GET, "/getUsageLimits", Value::Null),
        (Method::POST, "/listAvailableSubscriptions", json!({})),
    ] {
        let req = Request::builder()
            .method(method)
            .uri(path)
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), 65536)
            .await
            .unwrap();
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        match path {
            "/api/v1/portal/query" => assert_eq!(value["virtualPlanName"], "体验卡"),
            "/getUsageLimits" => {
                assert_eq!(value["virtualPlanName"], "体验卡");
                assert_eq!(value["subscriptionInfo"]["subscriptionTitle"], "体验卡");
                assert_eq!(value["subscriptionInfo"]["type"], "PRO");
                assert_eq!(value["usageBreakdownList"][0]["usageLimit"], 300.0);
            }
            _ => {
                assert_eq!(value["subscriptionPlans"][0]["qSubscriptionType"], "PRO");
                assert!(value["subscriptionPlans"][0]["description"]
                    .as_str()
                    .unwrap()
                    .starts_with("体验卡 - "));
            }
        }
    }
}

#[test]
fn legacy_multiple_bindings_deny_access_and_refresh() {
    let billing = BillingEngine::new();
    let mut card = billing::Card::new("legacy", "group-pro-plus", 1_000_000_000);
    card.activate(gateway::now_secs(), 0).unwrap();
    billing.upsert_card(card.clone());
    let auth = AuthState::with_billing("test-tier-secret-at-least-32-characters", billing.clone());
    let token = auth.issue_token_for_card("legacy", 3600).unwrap();
    let refresh = auth
        .issue_refresh_token("legacy", "group-pro-plus", 1, 1, 3600)
        .unwrap();
    card.bound_devices = vec!["a".into(), "b".into()];
    billing.upsert_card(card);
    assert!(auth.verify_token(&token).is_err());
    assert!(auth.rotate_refresh_token(&refresh, 3600, 3600).is_err());
}
