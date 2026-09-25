//! Tests for P2-6 Capacity Guardrails and Kiro-compatible retry signals (Spec §14.9).

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use billing::card::{Card, CardStatus};
use billing::engine::BillingEngine;
use billing::ledger::UsageTokens;
use billing::reservation::ReservationEstimateParams;
use billing::MICRO_CREDITS_PER_CREDIT;
use gateway::auth::AuthClaims;
use gateway::facade::conversation::GenerateAssistantResponseHandler;
use gateway::facade::FacadeRegistry;
use gateway::guardrail::{CapacityError, CapacityGuardrail};
use tower::ServiceExt;

mod support;

#[test]
fn test_capacity_guardrail_fast_fail_and_raii_release() {
    let guardrail = CapacityGuardrail::new(2, 3);
    assert_eq!(guardrail.max_concurrency(), 2);
    assert_eq!(guardrail.current_concurrency(), 0);

    // 1. Acquire permit 1
    let permit1 = guardrail.try_acquire().expect("Permit 1 should succeed");
    assert_eq!(guardrail.current_concurrency(), 1);

    // 2. Acquire permit 2
    let permit2 = guardrail.try_acquire().expect("Permit 2 should succeed");
    assert_eq!(guardrail.current_concurrency(), 2);

    // 3. Acquire permit 3 -> Saturated!
    let err = guardrail.try_acquire().unwrap_err();
    assert_eq!(
        err,
        CapacityError::CapacitySaturated {
            current: 2,
            max: 2,
            retry_after_secs: 3,
        }
    );

    // 4. Validate Kiro retry response shape
    let resp = guardrail.build_retry_response();
    assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        resp.headers().get("x-amzn-errortype").unwrap(),
        "ThrottlingException"
    );
    assert_eq!(resp.headers().get("Retry-After").unwrap(), "3");

    // 5. Release permit 1 via drop
    drop(permit1);
    assert_eq!(guardrail.current_concurrency(), 1);

    // 6. Acquire permit 3 now succeeds
    let permit3 = guardrail
        .try_acquire()
        .expect("Permit 3 should now succeed");
    assert_eq!(guardrail.current_concurrency(), 2);

    drop(permit2);
    drop(permit3);
    assert_eq!(guardrail.current_concurrency(), 0);
}

#[tokio::test]
async fn test_gateway_capacity_saturation_fast_fail_response() {
    let billing = BillingEngine::new();
    let saturated_guardrail = CapacityGuardrail::new(0, 5); // 0 capacity -> 100% saturated

    let handler = GenerateAssistantResponseHandler {
        billing,
        intercept_intent: false,
        guardrail: saturated_guardrail,
        ..Default::default()
    };

    let mut registry = FacadeRegistry::default();
    registry.register(handler);
    let app = registry.into_router();

    let req = Request::builder()
        .method(Method::POST)
        .uri("/generateAssistantResponse")
        .header("amz-sdk-invocation-id", "inv-guard-test-1")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            r#"{"conversationState": {"conversationId": "c1", "currentMessage": {"userInputMessage": {"content": "quota test"}}}}"#,
        ))
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();

    // Verify fast-fail HTTP 429
    assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        resp.headers().get("x-amzn-errortype").unwrap(),
        "ThrottlingException"
    );
    assert_eq!(resp.headers().get("Retry-After").unwrap(), "5");

    let body_bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(json["__type"], "ThrottlingException");
    assert_eq!(json["reason"], "CAPACITY_LIMIT_REACHED");
    assert!(json["message"]
        .as_str()
        .unwrap()
        .contains("Please retry after 5 seconds"));
}

#[tokio::test]
async fn test_gateway_card_concurrency_quota_rejection() {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    let billing = BillingEngine::new();
    billing.upsert_rate_card_version(support::wildcard_price("default"));
    let mut card = Card::new(
        "card-concurrency-gw",
        "group-default",
        500 * MICRO_CREDITS_PER_CREDIT,
    );
    card.status = CardStatus::Active;
    card.max_concurrency = 1; // Limit to 1 concurrent request
    billing.upsert_card(card);

    // Pre-hold 1 reservation for this card
    let params = ReservationEstimateParams::new(1_000, 2_000);
    billing
        .reserve(
            "card-concurrency-gw",
            "inv-existing-held",
            &params,
            now,
            300,
        )
        .unwrap();

    let handler = GenerateAssistantResponseHandler {
        billing: billing.clone(),
        intercept_intent: false,
        ..Default::default()
    };

    let mut registry = FacadeRegistry::default();
    registry.register(handler);
    let app = registry.into_router();

    // Issue request with auth claims for this card
    let mut req = Request::builder()
        .method(Method::POST)
        .uri("/generateAssistantResponse")
        .header("amz-sdk-invocation-id", "inv-gw-concurrent-2")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            r#"{"conversationState": {"conversationId": "c1", "currentMessage": {"userInputMessage": {"content": "quota test"}}}}"#,
        ))
        .unwrap();

    let claims = AuthClaims {
        card_id: "card-concurrency-gw".to_string(),
        group_id: "group-default".to_string(),
        token_version: 1,
        exp: now + 3600,
        iat: now,
    };
    req.extensions_mut().insert(claims);

    let resp = app.oneshot(req).await.unwrap();

    // Verify rejection with Kiro ThrottlingException
    assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        resp.headers().get("x-amzn-errortype").unwrap(),
        "ThrottlingException"
    );
    assert_eq!(resp.headers().get("Retry-After").unwrap(), "2");

    let body_bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(json["__type"], "ThrottlingException");
    assert_eq!(json["reason"], "CONCURRENCY_LIMIT_EXCEEDED");
    assert!(json["message"]
        .as_str()
        .unwrap()
        .contains("Card concurrency quota exceeded (1/1)"));
}

#[tokio::test]
async fn test_gateway_daily_and_monthly_quota_rejection() {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    let billing = BillingEngine::new();
    billing.upsert_rate_card_version(support::wildcard_price("default"));
    let mut card = Card::new(
        "card-period-gw",
        "group-default",
        500 * MICRO_CREDITS_PER_CREDIT,
    );
    card.status = CardStatus::Active;
    card.daily_credit_limit = Some(10 * MICRO_CREDITS_PER_CREDIT); // 10 credits daily
    billing.upsert_card(card);

    // Consume the daily limit
    let p = ReservationEstimateParams::new(100, 100);
    billing
        .reserve("card-period-gw", "inv-fill-daily", &p, now, 300)
        .unwrap();
    let tokens = UsageTokens {
        uncached_input_tokens: 1_000_000,
        output_tokens: 0,
        cache_creation_tokens: 0,
        cache_read_tokens: 0,
    }; // 15 credits
    billing
        .settle("inv-fill-daily", &tokens, "m", "p", "t", now)
        .unwrap();

    let handler = GenerateAssistantResponseHandler {
        billing: billing.clone(),
        intercept_intent: false,
        ..Default::default()
    };

    let mut registry = FacadeRegistry::default();
    registry.register(handler);
    let app = registry.into_router();

    let mut req = Request::builder()
        .method(Method::POST)
        .uri("/generateAssistantResponse")
        .header("amz-sdk-invocation-id", "inv-daily-exceeded")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            r#"{"conversationState": {"conversationId": "c1", "currentMessage": {"userInputMessage": {"content": "quota test"}}}}"#,
        ))
        .unwrap();

    let claims = AuthClaims {
        card_id: "card-period-gw".to_string(),
        group_id: "group-default".to_string(),
        token_version: 1,
        exp: now + 3600,
        iat: now,
    };
    req.extensions_mut().insert(claims);

    let resp = app.oneshot(req).await.unwrap();

    // Verify rejection with DAILY_LIMIT_EXCEEDED
    assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        resp.headers().get("x-amzn-errortype").unwrap(),
        "ThrottlingException"
    );

    let body_bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(json["__type"], "ThrottlingException");
    assert_eq!(json["reason"], "DAILY_LIMIT_EXCEEDED");
}
