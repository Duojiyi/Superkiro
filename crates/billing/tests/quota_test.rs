//! Tests for P2-6 Quota Engine: Card Concurrency (Fair Use), Daily/Monthly Limits, and Dynamic Updates.
//! Spec §5, §6.6, §14.9.

use billing::card::{Card, CardStatus};
use billing::engine::{BillingEngine, BillingError};
use billing::ledger::UsageTokens;
use billing::reservation::ReservationEstimateParams;
use billing::template::CardTemplate;
use billing::MICRO_CREDITS_PER_CREDIT;

const BASE_TIME: u64 = 1_773_100_000;

#[test]
fn test_card_concurrency_quota_fair_use() {
    let engine = BillingEngine::new();
    let mut card = Card::new(
        "card-concurrency",
        "group-default",
        500 * MICRO_CREDITS_PER_CREDIT,
    );
    card.status = CardStatus::Active;
    card.max_concurrency = 2; // Maximum 2 concurrent in-flight requests (Fair-use)
    engine.upsert_card(card);

    let params = ReservationEstimateParams::new(1_000, 2_000);

    // 1. First concurrent request -> Held
    let res1 = engine.reserve("card-concurrency", "inv-conc-1", &params, BASE_TIME, 300);
    assert!(res1.is_ok());
    assert_eq!(engine.get_active_concurrency("card-concurrency"), 1);

    // 2. Second concurrent request -> Held
    let res2 = engine.reserve("card-concurrency", "inv-conc-2", &params, BASE_TIME, 300);
    assert!(res2.is_ok());
    assert_eq!(engine.get_active_concurrency("card-concurrency"), 2);

    // 3. Third concurrent request -> Exceeds max_concurrency of 2 -> Rejected!
    let res3 = engine.reserve("card-concurrency", "inv-conc-3", &params, BASE_TIME, 300);
    assert_eq!(
        res3,
        Err(BillingError::ConcurrencyLimitExceeded { current: 2, max: 2 })
    );
    assert_eq!(engine.get_active_concurrency("card-concurrency"), 2);

    // 4. Release first request
    engine.release("inv-conc-1").unwrap();
    assert_eq!(engine.get_active_concurrency("card-concurrency"), 1);

    // 5. Retry third request -> Now succeeds!
    let res3_retry = engine.reserve("card-concurrency", "inv-conc-3", &params, BASE_TIME, 300);
    assert!(res3_retry.is_ok());
    assert_eq!(engine.get_active_concurrency("card-concurrency"), 2);
}

#[test]
fn test_daily_credit_limit_and_midnight_rollover() {
    let engine = BillingEngine::new();
    let mut card = Card::new(
        "card-daily",
        "group-default",
        1_000 * MICRO_CREDITS_PER_CREDIT,
    );
    card.status = CardStatus::Active;
    card.daily_credit_limit = Some(50 * MICRO_CREDITS_PER_CREDIT); // 50 credits daily cap
    engine.upsert_card(card);

    // Day 1 timestamp: exactly 12:00:00 UTC
    let day_1 = (BASE_TIME / 86_400) * 86_400 + 43_200;

    // Request 1: reserve and settle 30 credits on Day 1
    let params = ReservationEstimateParams::new(500, 500); // 37.5 credits reserve
    engine
        .reserve("card-daily", "inv-d1-1", &params, day_1, 300)
        .unwrap();

    let tokens = UsageTokens {
        uncached_input_tokens: 1_000_000, // 15 credits
        output_tokens: 250_000,           // 15 credits
        cache_creation_tokens: 0,
        cache_read_tokens: 0,
    }; // total charge: 30 credits
    let settle = engine
        .settle("inv-d1-1", &tokens, "m", "p", "t", day_1 + 5)
        .unwrap();
    assert_eq!(settle.credits_charged, 30 * MICRO_CREDITS_PER_CREDIT);

    assert_eq!(
        engine.get_daily_usage("card-daily", day_1),
        30 * MICRO_CREDITS_PER_CREDIT
    );

    // Request 2 on Day 1: reserve 27 credits. 30 + 27 = 57 > 50 -> Rejection!
    let params_large = ReservationEstimateParams::new(1_000_000, 200_000); // 27 credits (27M micro-credits)
    let err = engine.reserve("card-daily", "inv-d1-2", &params_large, day_1 + 10, 300);
    assert!(
        matches!(err, Err(BillingError::DailyLimitExceeded { limit, current, .. }) if limit == 50 * MICRO_CREDITS_PER_CREDIT && current == 30 * MICRO_CREDITS_PER_CREDIT)
    );

    // Rollover to Day 2: 24 hours later
    let day_2 = day_1 + 86_400;
    assert_eq!(engine.get_daily_usage("card-daily", day_2), 0);

    // Request on Day 2: reserve 25 credits -> Succeeds because daily limit reset!
    let res_day2 = engine.reserve("card-daily", "inv-d2-1", &params_large, day_2, 300);
    assert!(res_day2.is_ok());
}

#[test]
fn test_monthly_credit_limit_enforcement() {
    let engine = BillingEngine::new();
    let mut card = Card::new(
        "card-monthly",
        "group-default",
        500 * MICRO_CREDITS_PER_CREDIT,
    );
    card.status = CardStatus::Active;
    card.monthly_credit_limit = Some(100 * MICRO_CREDITS_PER_CREDIT); // 100 credits monthly cap
    engine.upsert_card(card);

    let params = ReservationEstimateParams::new(1_000, 1_000);
    engine
        .reserve("card-monthly", "inv-m-1", &params, BASE_TIME, 300)
        .unwrap();

    let tokens = UsageTokens {
        uncached_input_tokens: 2_000_000, // 30 credits
        output_tokens: 1_000_000,         // 60 credits
        cache_creation_tokens: 0,
        cache_read_tokens: 0,
    }; // total charge: 90 credits
    engine
        .settle("inv-m-1", &tokens, "m", "p", "t", BASE_TIME + 10)
        .unwrap();

    assert_eq!(
        engine.get_monthly_usage("card-monthly", BASE_TIME + 20),
        90 * MICRO_CREDITS_PER_CREDIT
    );

    // Attempt to reserve 27 credits: 90 + 27 = 117 > 100 -> Rejected!
    let params2 = ReservationEstimateParams::new(1_000_000, 200_000); // 27 credits (27M micro-credits)
    let res = engine.reserve("card-monthly", "inv-m-2", &params2, BASE_TIME + 30, 300);
    assert!(
        matches!(res, Err(BillingError::MonthlyLimitExceeded { limit, current, .. }) if limit == 100 * MICRO_CREDITS_PER_CREDIT && current == 90 * MICRO_CREDITS_PER_CREDIT)
    );
}

#[test]
fn test_dynamic_quota_updates_and_template_propagation() {
    let engine = BillingEngine::new();

    // 1. Template with custom quotas
    let template = CardTemplate::monthly("tpl-quota", "group-default").with_limits(
        Some(20 * MICRO_CREDITS_PER_CREDIT),
        Some(200 * MICRO_CREDITS_PER_CREDIT),
        4,
    );
    assert_eq!(template.max_concurrency, 4);
    assert_eq!(
        template.daily_credit_limit,
        Some(20 * MICRO_CREDITS_PER_CREDIT)
    );
    assert_eq!(
        template.monthly_credit_limit,
        Some(200 * MICRO_CREDITS_PER_CREDIT)
    );

    // 2. Card generated from template inherits limits
    let card = Card::from_template("card-tpl-1", "hash", &template, None, BASE_TIME);
    assert_eq!(card.max_concurrency, 4);
    assert_eq!(card.daily_credit_limit, Some(20 * MICRO_CREDITS_PER_CREDIT));
    assert_eq!(
        card.monthly_credit_limit,
        Some(200 * MICRO_CREDITS_PER_CREDIT)
    );
    engine.upsert_card(card);

    // 3. Update quotas dynamically on live card
    let updated = engine
        .update_card_quotas(
            "card-tpl-1",
            Some(8),
            Some(Some(50 * MICRO_CREDITS_PER_CREDIT)),
            Some(None), // Remove monthly limit
        )
        .unwrap();

    assert_eq!(updated.max_concurrency, 8);
    assert_eq!(
        updated.daily_credit_limit,
        Some(50 * MICRO_CREDITS_PER_CREDIT)
    );
    assert_eq!(updated.monthly_credit_limit, None);

    let stored = engine.get_card("card-tpl-1").unwrap();
    assert_eq!(stored.max_concurrency, 8);
    assert_eq!(
        stored.daily_credit_limit,
        Some(50 * MICRO_CREDITS_PER_CREDIT)
    );
    assert_eq!(stored.monthly_credit_limit, None);
}
