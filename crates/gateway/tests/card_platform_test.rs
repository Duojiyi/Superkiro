use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use billing::{
    BillingEngine, CardPlatformManager, CardStatus, CardTemplate, InventoryStockResponse,
    PullCardsResponse, RedeemCallbackResponse,
};
use gateway::facade::card_platform::{CardInventoryHandler, CardPullHandler, CardRedeemHandler};
use gateway::facade::FacadeHandler;
use serde_json::json;

fn setup_test_env() -> (BillingEngine, CardPlatformManager, CardTemplate) {
    let billing = BillingEngine::new();
    billing.set_master_kek(billing::MasterKek::from_bytes([37; 32]));
    let platform = CardPlatformManager::new();
    let template = CardTemplate::monthly("tpl-month-pro", "group-pro-plus");
    (billing, platform, template)
}

#[tokio::test]
async fn test_card_platform_inventory_query() {
    let (billing, platform, template) = setup_test_env();

    // Initially 0 cards
    let handler = CardInventoryHandler {
        billing: billing.clone(),
        platform: platform.clone(),
        api_key: Some("test-card-platform-key-32-characters".to_string()),
    };

    let req = Request::builder()
        .method(Method::GET)
        .uri("/api/v1/cards/inventory?template_id=tpl-month-pro")
        .header(
            "x-card-platform-key",
            "test-card-platform-key-32-characters",
        )
        .body(Body::empty())
        .unwrap();

    let resp = handler.handle(req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .unwrap();
    let stock: InventoryStockResponse = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(stock.unactivated_count, 0);

    // Generate 3 unactivated cards
    for _ in 0..3 {
        let gen = billing::generate_card(&template, None, 1700000000).unwrap();
        billing.upsert_card(gen.card);
    }

    let req2 = Request::builder()
        .method(Method::GET)
        .uri("/api/v1/cards/inventory?template_id=tpl-month-pro")
        .header(
            "x-card-platform-key",
            "test-card-platform-key-32-characters",
        )
        .body(Body::empty())
        .unwrap();

    let resp2 = handler.handle(req2).await;
    assert_eq!(resp2.status(), StatusCode::OK);
    let bytes2 = axum::body::to_bytes(resp2.into_body(), 64 * 1024)
        .await
        .unwrap();
    let stock2: InventoryStockResponse = serde_json::from_slice(&bytes2).unwrap();
    assert_eq!(stock2.unactivated_count, 3);
    assert_eq!(stock2.total_count, 3);
}

#[tokio::test]
async fn test_card_platform_pull_cards_idempotency() {
    let (billing, platform, template) = setup_test_env();

    let handler = CardPullHandler {
        billing: billing.clone(),
        platform: platform.clone(),
        default_template: template,
        api_key: Some("test-card-platform-key-32-characters".to_string()),
    };

    let pull_body = json!({
        "order_id": "order-shop-9999",
        "count": 2,
        "note": "Shop automatic dispatch"
    });

    let req1 = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/cards/pull")
        .header(header::CONTENT_TYPE, "application/json")
        .header(
            "x-card-platform-key",
            "test-card-platform-key-32-characters",
        )
        .body(Body::from(pull_body.to_string()))
        .unwrap();

    let resp1 = handler.handle(req1).await;
    assert_eq!(resp1.status(), StatusCode::OK);
    let bytes1 = axum::body::to_bytes(resp1.into_body(), 64 * 1024)
        .await
        .unwrap();
    let pull_res1: PullCardsResponse = serde_json::from_slice(&bytes1).unwrap();

    assert_eq!(pull_res1.order_id, "order-shop-9999");
    assert_eq!(pull_res1.cards.len(), 2);
    assert_eq!(pull_res1.cards[0].status, CardStatus::Unactivated);
    assert!(pull_res1.cards[0].raw_code.starts_with("kiro-"));

    // Repeat identical pull with same order_id -> MUST return identical cards (idempotency)
    let req2 = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/cards/pull")
        .header(header::CONTENT_TYPE, "application/json")
        .header(
            "x-card-platform-key",
            "test-card-platform-key-32-characters",
        )
        .body(Body::from(pull_body.to_string()))
        .unwrap();

    let resp2 = handler.handle(req2).await;
    assert_eq!(resp2.status(), StatusCode::OK);
    let bytes2 = axum::body::to_bytes(resp2.into_body(), 64 * 1024)
        .await
        .unwrap();
    let pull_res2: PullCardsResponse = serde_json::from_slice(&bytes2).unwrap();

    assert_eq!(pull_res1, pull_res2);
}

#[tokio::test]
async fn test_card_platform_redeem_verify_and_activate() {
    let (billing, platform, template) = setup_test_env();

    // 1. Generate an unactivated card
    let gen = billing::generate_card(&template, None, 1700000000).unwrap();
    billing.upsert_card(gen.card.clone());

    let handler = CardRedeemHandler {
        billing: billing.clone(),
        platform: platform.clone(),
        api_key: Some("test-card-platform-key-32-characters".to_string()),
    };

    // 2. Action: VerifyOnly
    let verify_req = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/cards/redeem")
        .header(header::CONTENT_TYPE, "application/json")
        .header(
            "x-card-platform-key",
            "test-card-platform-key-32-characters",
        )
        .body(Body::from(
            json!({
                "order_id": "verify-order-01",
                "card_code_or_id": gen.raw_code,
                "action": { "verify_only": null }
            })
            .to_string(),
        ))
        .unwrap();

    let resp_v = handler.handle(verify_req).await;
    assert_eq!(resp_v.status(), StatusCode::OK);
    let bytes_v = axum::body::to_bytes(resp_v.into_body(), 64 * 1024)
        .await
        .unwrap();
    let redeem_v: RedeemCallbackResponse = serde_json::from_slice(&bytes_v).unwrap();
    assert!(redeem_v.success);
    assert_eq!(redeem_v.status, CardStatus::Unactivated);
    assert_eq!(redeem_v.remaining_credits, template.credit_total);

    // 3. Action: Activate (30 days = 2592000s)
    let activate_req = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/cards/redeem")
        .header(header::CONTENT_TYPE, "application/json")
        .header(
            "x-card-platform-key",
            "test-card-platform-key-32-characters",
        )
        .body(Body::from(
            json!({
                "order_id": "activate-order-01",
                "card_code_or_id": gen.raw_code,
                "action": { "activate": { "duration_secs": 2592000 } }
            })
            .to_string(),
        ))
        .unwrap();

    let resp_a = handler.handle(activate_req).await;
    assert_eq!(resp_a.status(), StatusCode::OK);
    let bytes_a = axum::body::to_bytes(resp_a.into_body(), 64 * 1024)
        .await
        .unwrap();
    let redeem_a: RedeemCallbackResponse = serde_json::from_slice(&bytes_a).unwrap();
    assert!(redeem_a.success);
    assert_eq!(redeem_a.status, CardStatus::Active);
    assert!(redeem_a.valid_until.is_some());
}

#[tokio::test]
async fn test_card_platform_redeem_topup() {
    let (billing, platform, template) = setup_test_env();

    // 1. Generate and activate card
    let gen = billing::generate_card(&template, None, 1700000000).unwrap();
    let mut card = gen.card;
    card.activate(1700000000, 86400 * 30).unwrap();
    billing.upsert_card(card.clone());

    // 2. Generate a topup code for 20,000 credits
    let topup_gen = billing::generate_topup_code(
        20_000 * billing::MICRO_CREDITS_PER_CREDIT,
        86400 * 30,
        1700000000,
    )
    .unwrap();
    billing.upsert_topup_code(topup_gen.topup);
    let raw_topup_code = topup_gen.raw_code;

    let handler = CardRedeemHandler {
        billing: billing.clone(),
        platform: platform.clone(),
        api_key: Some("test-card-platform-key-32-characters".to_string()),
    };

    let topup_req = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/cards/redeem")
        .header(header::CONTENT_TYPE, "application/json")
        .header(
            "x-card-platform-key",
            "test-card-platform-key-32-characters",
        )
        .body(Body::from(
            json!({
                "order_id": "topup-order-exec-1",
                "card_code_or_id": card.id,
                "action": { "topup": { "topup_code": raw_topup_code } }
            })
            .to_string(),
        ))
        .unwrap();

    let resp = handler.handle(topup_req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .unwrap();
    let redeem_resp: RedeemCallbackResponse = serde_json::from_slice(&bytes).unwrap();

    assert!(redeem_resp.success);
    assert_eq!(
        redeem_resp.remaining_credits,
        template.credit_total + 20_000 * billing::MICRO_CREDITS_PER_CREDIT
    );
}

#[tokio::test]
async fn test_card_platform_api_key_auth_enforcement() {
    let (billing, platform, template) = setup_test_env();

    let handler = CardPullHandler {
        billing: billing.clone(),
        platform: platform.clone(),
        default_template: template,
        api_key: Some("super-secret-shop-key-9988".to_string()),
    };

    let pull_body = json!({
        "order_id": "order-auth-test-01",
        "count": 1
    });

    // 1. Request without auth header -> MUST be 401 UNAUTHORIZED
    let req_unauth = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/cards/pull")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(pull_body.to_string()))
        .unwrap();

    let resp_unauth = handler.handle(req_unauth).await;
    assert_eq!(resp_unauth.status(), StatusCode::UNAUTHORIZED);

    // 2. Request with wrong key -> MUST be 401 UNAUTHORIZED
    let req_wrong = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/cards/pull")
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-card-platform-key", "wrong-key")
        .body(Body::from(pull_body.to_string()))
        .unwrap();

    let resp_wrong = handler.handle(req_wrong).await;
    assert_eq!(resp_wrong.status(), StatusCode::UNAUTHORIZED);

    // 3. Request with valid x-card-platform-key -> MUST be 200 OK
    let req_ok = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/cards/pull")
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-card-platform-key", "super-secret-shop-key-9988")
        .body(Body::from(pull_body.to_string()))
        .unwrap();

    let resp_ok = handler.handle(req_ok).await;
    assert_eq!(resp_ok.status(), StatusCode::OK);

    // 4. Request with valid Bearer token in Authorization header -> MUST be 200 OK
    let req_bearer_ok = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/cards/pull")
        .header(header::CONTENT_TYPE, "application/json")
        .header("authorization", "Bearer super-secret-shop-key-9988")
        .body(Body::from(pull_body.to_string()))
        .unwrap();

    let resp_bearer_ok = handler.handle(req_bearer_ok).await;
    assert_eq!(resp_bearer_ok.status(), StatusCode::OK);
}
