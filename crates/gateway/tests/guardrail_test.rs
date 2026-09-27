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

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// A card for the limit tests, with balance and concurrency to spare.
fn limit_card(billing: &BillingEngine, card_id: &str) {
    let mut card = Card::new(card_id, "group-default", 500 * MICRO_CREDITS_PER_CREDIT);
    card.status = CardStatus::Active;
    billing.upsert_card(card);
}

/// Sets the card's daily or 30-day credit limit, keeping its holds.
fn set_limit(billing: &BillingEngine, card_id: &str, daily: bool, limit: i64) {
    let (daily_limit, monthly_limit) = if daily {
        (Some(Some(limit)), Some(None))
    } else {
        (Some(None), Some(Some(limit)))
    };
    billing
        .update_card_quotas(card_id, None, daily_limit, monthly_limit)
        .unwrap();
}

/// What the conversation endpoint answers the card: status, Retry-After and body.
async fn limit_reply(
    billing: &BillingEngine,
    card_id: &str,
    invocation: &str,
) -> (StatusCode, Option<String>, serde_json::Value) {
    let now = unix_now();
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
        .header("amz-sdk-invocation-id", invocation)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            r#"{"conversationState": {"conversationId": "c1", "currentMessage": {"userInputMessage": {"content": "quota test"}}}}"#,
        ))
        .unwrap();
    req.extensions_mut().insert(AuthClaims {
        card_id: card_id.to_string(),
        group_id: "group-default".to_string(),
        token_version: 1,
        exp: now + 3600,
        iat: now,
    });

    let resp = app.oneshot(req).await.unwrap();
    let status = resp.status();
    let retry_after = resp
        .headers()
        .get("Retry-After")
        .map(|v| v.to_str().unwrap().to_string());
    let body_bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let body = serde_json::from_slice(&body_bytes).unwrap_or(serde_json::Value::Null);
    (status, retry_after, body)
}

/// Settled usage at the limit stays refused until the window frees up. Kiro shows a
/// ValidationException without a reason as written, so the customer reads the limit, the
/// usage and when it frees up; its own limit reasons show "return tomorrow / next month".
#[tokio::test]
async fn test_gateway_daily_and_monthly_quota_rejection() {
    for (daily, window, frees) in [
        (
            true,
            "今日积分用量已达上限",
            "额度按 UTC 自然日计算，每天北京时间 8:00 重置",
        ),
        (
            false,
            "近 30 天积分用量已达上限",
            "额度按滚动 30 天计算，每笔用量满 30 天后释放",
        ),
    ] {
        let now = unix_now();
        let billing = BillingEngine::new();
        billing.upsert_rate_card_version(support::wildcard_price("default"));
        limit_card(&billing, "card-period-gw");

        // Consume the limit
        let p = ReservationEstimateParams::new(100, 100);
        billing
            .reserve("card-period-gw", "inv-fill", &p, now, 300)
            .unwrap();
        let tokens = UsageTokens {
            uncached_input_tokens: 1_000_000,
            output_tokens: 0,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
        }; // 15 credits
        billing
            .settle("inv-fill", &tokens, "m", "p", "t", now)
            .unwrap();
        set_limit(
            &billing,
            "card-period-gw",
            daily,
            10 * MICRO_CREDITS_PER_CREDIT,
        );

        let (status, retry_after, json) =
            limit_reply(&billing, "card-period-gw", "inv-limit-reached").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(retry_after, None);
        assert_eq!(json["__type"], "ValidationException");
        assert!(json.get("reason").is_none(), "{json}");
        let message = json["message"].as_str().unwrap();
        assert!(
            message.starts_with(&format!("{window}：上限 10.00，已用 15.00。")),
            "{message}"
        );
        assert!(message.contains(frees), "{message}");
        // Kiro shows at most 200 characters of it.
        assert!(message.chars().count() <= 200, "{message}");
        assert_eq!(
            billing.get_card("card-period-gw").unwrap().credit_reserved,
            0
        );
    }
}

/// Other requests' open holds settle within minutes, mostly for less than they hold, so
/// a refusal they alone cause is a throttle Kiro retries, not a limit it reports as reached.
#[tokio::test]
async fn test_gateway_quota_refusal_caused_by_open_holds_is_a_throttle() {
    for daily in [true, false] {
        let now = unix_now();
        let billing = BillingEngine::new();
        billing.upsert_rate_card_version(support::wildcard_price("default"));
        limit_card(&billing, "card-holds-gw");

        // Another request holds nearly all of the window; nothing is settled yet.
        let p = ReservationEstimateParams::new(0, 15_000); // 0.9 credits
        billing
            .reserve("card-holds-gw", "inv-other", &p, now, 300)
            .unwrap();
        let held = billing.get_card("card-holds-gw").unwrap().credit_reserved;
        assert_eq!(held, 900_000);
        set_limit(&billing, "card-holds-gw", daily, held + 100_000);

        let (status, retry_after, json) =
            limit_reply(&billing, "card-holds-gw", "inv-behind-holds").await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(retry_after.as_deref(), Some("1"));
        assert_eq!(json["__type"], "ThrottlingException");
        // Not a reason Kiro reads as a usage limit, so it retries.
        assert_eq!(json["reason"], "CREDIT_HOLDS_PENDING");
        assert_eq!(
            billing.get_card("card-holds-gw").unwrap().credit_reserved,
            held
        );

        // Once the other request is done, the retry goes through.
        billing.release("inv-other").unwrap();
        let (status, _, _) = limit_reply(&billing, "card-holds-gw", "inv-after-holds").await;
        assert_eq!(status, StatusCode::OK);
    }
}

/// A hold larger than what the window has left is refused until the window frees up
/// however many retries follow: a readable ValidationException, never retried.
#[tokio::test]
async fn test_gateway_quota_refusal_of_a_hold_larger_than_what_is_left() {
    let billing = BillingEngine::new();
    billing.upsert_rate_card_version(support::wildcard_price("default"));
    limit_card(&billing, "card-small-limit");
    // 0.1 credits, less than the 4,096-token output hold at 60 credits per million.
    set_limit(&billing, "card-small-limit", true, 100_000);

    let (status, retry_after, json) =
        limit_reply(&billing, "card-small-limit", "inv-hold-too-large").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(retry_after, None);
    assert_eq!(json["__type"], "ValidationException");
    assert!(json.get("reason").is_none(), "{json}");
    let message = json["message"].as_str().unwrap();
    assert!(
        message.starts_with("今日剩余积分额度 0.10 不足以预留本次请求所需的 "),
        "{message}"
    );
    assert!(
        message.contains("（按该模型的最大输出估算）：上限 0.10，已用 0.00。"),
        "{message}"
    );
    assert!(message.contains("每天北京时间 8:00 重置"), "{message}");
    assert!(message.chars().count() <= 200, "{message}");
}
