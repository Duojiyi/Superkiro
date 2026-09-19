use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use billing::{BillingEngine, Card, ReservationEstimateParams, UsageTokens};
use gateway::{
    auth::AuthState,
    facade::{virtualization::VirtualizationStore, FacadeRegistry},
};
use serde_json::Value;
use tower::ServiceExt;

#[tokio::test]
async fn usage_exposes_only_own_settlements_and_live_entitlement() {
    let billing = BillingEngine::new();
    let now = gateway::now_secs();
    let mut card = Card::new("card", "group-pro-plus", 1_000_000_000);
    card.activate(now, 86400).unwrap();
    billing.upsert_card(card);
    billing
        .reserve(
            "card",
            "settled",
            &ReservationEstimateParams::new(0, 10),
            now,
            600,
        )
        .unwrap();
    let entry = billing
        .settle(
            "settled",
            &UsageTokens {
                output_tokens: 10,
                ..Default::default()
            },
            "public-model",
            "private-provider",
            "private-target",
            now,
        )
        .unwrap();
    let auth = AuthState::with_billing("usage-statistics-test-secret-32-bytes", billing.clone());
    let token = auth.issue_token_for_card("card", 3600).unwrap();
    let mut registry = FacadeRegistry::new();
    registry.register_virtualized_facades(VirtualizationStore::with_billing(
        billing.clone(),
        "group-pro-plus",
    ));
    let app = registry.into_router_with_auth(auth);
    let request = || {
        Request::builder()
            .uri("/getUsageLimits")
            .header("authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap()
    };
    let response = app.clone().oneshot(request()).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), 65536)
        .await
        .unwrap();
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["virtualPlanName"], "PRO");
    assert_eq!(value["validUntil"], now + 86400);
    assert_eq!(value["settledUsage"]["totalTokens"], 10);
    assert_eq!(
        value["settledUsage"]["todayPoints"],
        entry.credits_charged as f64 / 1_000_000.0
    );
    assert_eq!(value["settledUsage"]["models"][0]["name"], "public-model");
    assert!(value["settledUsageUnavailableReason"].is_null());
    for secret in [
        "private-provider",
        "private-target",
        "providerCost",
        "provider_cost",
        "referencePrice",
        "\"usd\"",
    ] {
        assert!(!String::from_utf8_lossy(&bytes).contains(secret));
    }
    // validUntil comes from current state on every refresh, not the login snapshot.
    let mut card = billing.get_card("card").unwrap();
    card.valid_until = None;
    billing.upsert_card(card);
    let response = app.oneshot(request()).await.unwrap();
    let bytes = axum::body::to_bytes(response.into_body(), 65536)
        .await
        .unwrap();
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    assert!(value["validUntil"].is_null());
}
