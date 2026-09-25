//! Comprehensive test suite for P2-5:
//! Rate-card versioning, tiered cache token billing, cost-side ledger, and 3 pricing modes (Spec §5, §6.4, §6.5, §14.10).

use billing::card::Card;
use billing::engine::{BillingEngine, BillingError};
use billing::group::{Group, ModelMap};
use billing::ledger::{LedgerKind, UsageTokens};
use billing::rate_card::{BillingSettings, Currency, PricingMode, RateCard, RateCardVersion};
use billing::reservation::ReservationEstimateParams;
use billing::MICRO_CREDITS_PER_CREDIT;

#[test]
fn test_pricing_mode_cost_plus_with_tiered_cache_tokens() {
    let engine = BillingEngine::new();
    let now_secs = 1000;

    // 1. Configure settings: 1 credit = 0.01 CNY (1分钱/积分), USD/CNY rate = 7.20
    let settings = BillingSettings {
        credit_face_value_cny: 0.01,
        usd_cny_rate: 7.20,
        rate_updated_at_secs: now_secs,
    };
    engine.update_settings(settings);

    // 2. Setup group with rate_card_id = "rc-standard", margin_multiplier = 1.0
    let mut group = Group::pro_plus("group-costplus", "CostPlus Group");
    group.rate_card_id = "rc-standard".to_string();
    group.margin_multiplier = 1.0;
    engine.upsert_group(group);

    // 3. Setup RateCard & RateCardVersion for claude-sonnet-4.5
    // Base cost:
    // Input: $3.00 / 1M
    // Output: $15.00 / 1M
    // Cache creation: $3.75 / 1M
    // Cache read: $0.30 / 1M
    // Gross margin multiplier: 1.25 (25% gross margin)
    let rate_card = RateCard::new("rc-standard", "Standard Pricing", now_secs);
    engine.upsert_rate_card(rate_card);

    let version = RateCardVersion {
        id: "rcv-claude-v1".to_string(),
        rate_card_id: "rc-standard".to_string(),
        model: "claude-sonnet-4.5".to_string(),
        currency: Currency::Usd,
        pricing_mode: PricingMode::CostPlus,
        input_price_per_m: 3.0,
        output_price_per_m: 15.0,
        cache_creation_price_per_m: 3.75,
        cache_read_price_per_m: 0.30,
        fixed_input_credit_per_m: 0,
        fixed_output_credit_per_m: 0,
        fixed_cache_creation_credit_per_m: 0,
        fixed_cache_read_credit_per_m: 0,
        per_call_credit: 0,
        margin_multiplier: 1.25,
        effective_from_secs: now_secs,
    };
    engine.upsert_rate_card_version(version);

    // 4. Create card with 100 credits
    let card_id = "card-costplus-01";
    let mut card = Card::new(card_id, "group-costplus", 100 * MICRO_CREDITS_PER_CREDIT);
    card.activate(now_secs, 86400 * 30).unwrap();
    engine.upsert_card(card);

    // 5. Reserve credits with model specified
    let reserve_params =
        ReservationEstimateParams::new(2_000, 4_000).with_model("claude-sonnet-4.5");
    let reservation = engine
        .reserve(card_id, "inv-costplus-01", &reserve_params, now_secs, 300)
        .expect("Reserve should succeed");

    assert_eq!(
        reservation.rate_card_version.as_deref(),
        Some("rcv-claude-v1")
    );

    // 6. Settle with tiered cache tokens:
    // Uncached input: 1,000 tokens ($3.00/1M = $0.003)
    // Output: 500 tokens ($15.00/1M = $0.0075)
    // Cache creation: 2,000 tokens ($3.75/1M = $0.0075)
    // Cache read: 10,000 tokens ($0.30/1M = $0.003)
    // Total USD cost = 0.003 + 0.0075 + 0.0075 + 0.003 = $0.0210
    // Total CNY cost = $0.0210 * 7.20 = 0.1512 CNY
    // Provider cost micro CNY = 151,200 micro-CNY
    // User charge with 1.25 margin:
    // Credits = (0.1512 * 1.25) / 0.01 = 18.9 credits = 18,900,000 micro-credits
    let tokens = UsageTokens {
        uncached_input_tokens: 1_000,
        output_tokens: 500,
        cache_creation_tokens: 2_000,
        cache_read_tokens: 10_000,
    };

    let entry = engine
        .settle(
            "inv-costplus-01",
            &tokens,
            "claude-sonnet-4.5",
            "anthropic",
            "claude-3-5-sonnet",
            now_secs + 2,
        )
        .expect("Settle should succeed");

    assert_eq!(entry.provider_cost_micro_cny, 151_200);
    assert_eq!(entry.credits_charged, 18_900_000);
    assert_eq!(entry.rate_card_version.as_deref(), Some("rcv-claude-v1"));

    // Verify card balance after settlement
    let card_after = engine.get_card(card_id).unwrap();
    assert_eq!(card_after.credit_used, 18_900_000);
    assert_eq!(card_after.credit_reserved, 0);
}

#[test]
fn test_pricing_mode_fixed_decoupled_from_cost_with_cost_ledger() {
    let engine = BillingEngine::new();
    let now_secs = 2000;

    let settings = BillingSettings {
        credit_face_value_cny: 0.01,
        usd_cny_rate: 7.20,
        rate_updated_at_secs: now_secs,
    };
    engine.update_settings(settings);

    let mut group = Group::pro_plus("group-fixed", "Fixed Pricing Group");
    group.rate_card_id = "rc-fixed".to_string();
    group.margin_multiplier = 1.0;
    engine.upsert_group(group);

    // Fixed pricing:
    // 10 credits / 1M input = 10,000,000 micro-credits
    // 30 credits / 1M output = 30,000,000 micro-credits
    // 2.5 credits / 1M cache read = 2,500,000 micro-credits
    // Upstream cost is recorded at $2.00 / $10.00 / $0.20
    let rate_card = RateCard::new("rc-fixed", "Fixed Rate Card", now_secs);
    engine.upsert_rate_card(rate_card);

    let version = RateCardVersion {
        id: "rcv-fixed-v1".to_string(),
        rate_card_id: "rc-fixed".to_string(),
        model: "deepseek-v3".to_string(),
        currency: Currency::Usd,
        pricing_mode: PricingMode::Fixed,
        input_price_per_m: 2.0,
        output_price_per_m: 10.0,
        cache_creation_price_per_m: 2.0,
        cache_read_price_per_m: 0.20,
        fixed_input_credit_per_m: 10_000_000,
        fixed_output_credit_per_m: 30_000_000,
        fixed_cache_creation_credit_per_m: 10_000_000,
        fixed_cache_read_credit_per_m: 2_500_000,
        per_call_credit: 0,
        margin_multiplier: 1.0,
        effective_from_secs: now_secs,
    };
    engine.upsert_rate_card_version(version);

    let card_id = "card-fixed-01";
    let mut card = Card::new(card_id, "group-fixed", 100 * MICRO_CREDITS_PER_CREDIT);
    card.activate(now_secs, 86400 * 30).unwrap();
    engine.upsert_card(card);

    let reserve_params = ReservationEstimateParams::new(5_000, 2_000).with_model("deepseek-v3");
    let reservation = engine
        .reserve(card_id, "inv-fixed-01", &reserve_params, now_secs, 300)
        .unwrap();

    // Reserve = (5000 * 10 + 2000 * 30) / 1000 = 0.05 + 0.06 = 0.11 credits = 110,000 micro-credits
    assert_eq!(reservation.reserved_micro_credits, 110_000);

    // Settle with 2,000 uncached input, 1,000 output, 10,000 cache read
    // User charge (fixed):
    // input: 2,000 * 10 = 0.02 credits = 20,000 micro-credits
    // output: 1,000 * 30 = 0.03 credits = 30,000 micro-credits
    // cache_read: 10,000 * 2.5 = 0.025 credits = 25,000 micro-credits
    // Total charge = 75,000 micro-credits
    //
    // Upstream cost (Cost-side ledger):
    // input: 2,000 * $2.00 / 1M = $0.004
    // output: 1,000 * $10.00 / 1M = $0.010
    // cache_read: 10,000 * $0.20 / 1M = $0.002
    // total USD = $0.0160 -> CNY = $0.0160 * 7.20 = 0.1152 CNY = 115,200 micro-CNY
    let tokens = UsageTokens {
        uncached_input_tokens: 2_000,
        output_tokens: 1_000,
        cache_creation_tokens: 0,
        cache_read_tokens: 10_000,
    };

    let entry = engine
        .settle(
            "inv-fixed-01",
            &tokens,
            "deepseek-v3",
            "openai",
            "deepseek-chat",
            now_secs + 1,
        )
        .unwrap();

    assert_eq!(entry.credits_charged, 75_000);
    assert_eq!(entry.provider_cost_micro_cny, 115_200);
    assert_eq!(entry.rate_card_version.as_deref(), Some("rcv-fixed-v1"));
}

#[test]
fn test_pricing_mode_per_call_flat_fee_and_reservation() {
    let engine = BillingEngine::new();
    let now_secs = 3000;

    let settings = BillingSettings {
        credit_face_value_cny: 0.01,
        usd_cny_rate: 7.00,
        rate_updated_at_secs: now_secs,
    };
    engine.update_settings(settings);

    let mut group = Group::pro_plus("group-per-call", "PerCall Group");
    group.rate_card_id = "rc-per-call".to_string();
    group.margin_multiplier = 1.0;
    engine.upsert_group(group);

    // Flat fee per call: 0.5 credits = 500_000 micro-credits per invocation
    let rate_card = RateCard::new("rc-per-call", "Per Call Rate Card", now_secs);
    engine.upsert_rate_card(rate_card);

    let version = RateCardVersion {
        id: "rcv-percall-v1".to_string(),
        rate_card_id: "rc-per-call".to_string(),
        model: "o3-mini".to_string(),
        currency: Currency::Usd,
        pricing_mode: PricingMode::PerCall,
        input_price_per_m: 1.10,
        output_price_per_m: 4.40,
        cache_creation_price_per_m: 0.0,
        cache_read_price_per_m: 0.55,
        fixed_input_credit_per_m: 0,
        fixed_output_credit_per_m: 0,
        fixed_cache_creation_credit_per_m: 0,
        fixed_cache_read_credit_per_m: 0,
        per_call_credit: 500_000,
        margin_multiplier: 1.0,
        effective_from_secs: now_secs,
    };
    engine.upsert_rate_card_version(version);

    let card_id = "card-percall-01";
    let mut card = Card::new(card_id, "group-per-call", 10 * MICRO_CREDITS_PER_CREDIT);
    card.activate(now_secs, 86400 * 30).unwrap();
    engine.upsert_card(card);

    // 1. Reserve: Spec §14.10.2: "预留即 N" -> exact 500_000 micro-credits
    let reserve_params = ReservationEstimateParams::new(10_000, 20_000).with_model("o3-mini");
    let reservation = engine
        .reserve(card_id, "inv-percall-01", &reserve_params, now_secs, 300)
        .unwrap();

    assert_eq!(reservation.reserved_micro_credits, 500_000);

    // 2. Settle: User charged exact flat fee (500_000 micro-credits) regardless of tokens
    // Upstream cost is still recorded from real tokens:
    // 5,000 input tokens * $1.10 / 1M = $0.0055
    // 2,000 output tokens * $4.40 / 1M = $0.0088
    // Total USD = $0.0143 * 7.00 = 0.1001 CNY = 100,100 micro-CNY
    let tokens = UsageTokens {
        uncached_input_tokens: 5_000,
        output_tokens: 2_000,
        cache_creation_tokens: 0,
        cache_read_tokens: 0,
    };

    let entry = engine
        .settle(
            "inv-percall-01",
            &tokens,
            "o3-mini",
            "openai",
            "o3-mini",
            now_secs + 2,
        )
        .unwrap();

    assert_eq!(entry.credits_charged, 500_000);
    assert_eq!(entry.provider_cost_micro_cny, 100_100);
    assert_eq!(entry.rate_card_version.as_deref(), Some("rcv-percall-v1"));
}

#[test]
fn test_rate_card_versioning_and_historical_immutability() {
    let engine = BillingEngine::new();
    let mut group = Group::pro_plus("group-versioning", "Versioning Group");
    group.rate_card_id = "rc-versioning".to_string();
    engine.upsert_group(group);

    // Version 1: Effective from t = 100
    // Price: $3.00 / 1M input, $15.00 / 1M output
    let v1 = RateCardVersion {
        id: "rcv-v1".to_string(),
        rate_card_id: "rc-versioning".to_string(),
        model: "claude-sonnet".to_string(),
        currency: Currency::Usd,
        pricing_mode: PricingMode::CostPlus,
        input_price_per_m: 3.0,
        output_price_per_m: 15.0,
        cache_creation_price_per_m: 0.0,
        cache_read_price_per_m: 0.0,
        fixed_input_credit_per_m: 0,
        fixed_output_credit_per_m: 0,
        fixed_cache_creation_credit_per_m: 0,
        fixed_cache_read_credit_per_m: 0,
        per_call_credit: 0,
        margin_multiplier: 1.0,
        effective_from_secs: 100,
    };
    engine.upsert_rate_card_version(v1);

    // Version 2: Provider price increase effective from t = 200
    // Price: $4.00 / 1M input, $20.00 / 1M output
    let v2 = RateCardVersion {
        id: "rcv-v2".to_string(),
        rate_card_id: "rc-versioning".to_string(),
        model: "claude-sonnet".to_string(),
        currency: Currency::Usd,
        pricing_mode: PricingMode::CostPlus,
        input_price_per_m: 4.0,
        output_price_per_m: 20.0,
        cache_creation_price_per_m: 0.0,
        cache_read_price_per_m: 0.0,
        fixed_input_credit_per_m: 0,
        fixed_output_credit_per_m: 0,
        fixed_cache_creation_credit_per_m: 0,
        fixed_cache_read_credit_per_m: 0,
        per_call_credit: 0,
        margin_multiplier: 1.0,
        effective_from_secs: 200,
    };
    engine.upsert_rate_card_version(v2);

    // Version 3: Scheduled future change at t = 500 (must NOT take effect early)
    let v3 = RateCardVersion {
        id: "rcv-v3".to_string(),
        rate_card_id: "rc-versioning".to_string(),
        model: "claude-sonnet".to_string(),
        currency: Currency::Usd,
        pricing_mode: PricingMode::CostPlus,
        input_price_per_m: 5.0,
        output_price_per_m: 25.0,
        cache_creation_price_per_m: 0.0,
        cache_read_price_per_m: 0.0,
        fixed_input_credit_per_m: 0,
        fixed_output_credit_per_m: 0,
        fixed_cache_creation_credit_per_m: 0,
        fixed_cache_read_credit_per_m: 0,
        per_call_credit: 0,
        margin_multiplier: 1.0,
        effective_from_secs: 500,
    };
    engine.upsert_rate_card_version(v3);

    // Verify resolution at different points in time
    let at_150 = engine
        .resolve_rate_card_version("rc-versioning", "claude-sonnet", 150)
        .expect("Should resolve v1 at t=150");
    assert_eq!(at_150.id, "rcv-v1");

    let at_250 = engine
        .resolve_rate_card_version("rc-versioning", "claude-sonnet", 250)
        .expect("Should resolve v2 at t=250");
    assert_eq!(at_250.id, "rcv-v2");

    let at_300 = engine
        .resolve_rate_card_version("rc-versioning", "claude-sonnet", 300)
        .expect("Should resolve v2 at t=300 (v3 is future)");
    assert_eq!(at_300.id, "rcv-v2");

    let at_501 = engine
        .resolve_rate_card_version("rc-versioning", "claude-sonnet", 501)
        .expect("Should resolve v3 at t=501");
    assert_eq!(at_501.id, "rcv-v3");

    // Perform live requests at t = 150 and t = 250
    let card_id = "card-versioning-01";
    let mut card = Card::new(card_id, "group-versioning", 50 * MICRO_CREDITS_PER_CREDIT);
    card.activate(50, 86400 * 30).unwrap();
    engine.upsert_card(card);

    let params = ReservationEstimateParams::new(1000, 1000).with_model("claude-sonnet");
    engine
        .reserve(card_id, "inv-t150", &params, 150, 300)
        .unwrap();

    let tokens = UsageTokens {
        uncached_input_tokens: 1_000,
        output_tokens: 1_000,
        cache_creation_tokens: 0,
        cache_read_tokens: 0,
    };

    let entry_t150 = engine
        .settle(
            "inv-t150",
            &tokens,
            "claude-sonnet",
            "anthropic",
            "claude",
            155,
        )
        .unwrap();
    assert_eq!(entry_t150.rate_card_version.as_deref(), Some("rcv-v1"));

    // Request at t = 250
    engine
        .reserve(card_id, "inv-t250", &params, 250, 300)
        .unwrap();
    let entry_t250 = engine
        .settle(
            "inv-t250",
            &tokens,
            "claude-sonnet",
            "anthropic",
            "claude",
            255,
        )
        .unwrap();
    assert_eq!(entry_t250.rate_card_version.as_deref(), Some("rcv-v2"));

    // Provider cost at t=150: ($3 + $15) / 1000 = $0.018 * 7.25 = 0.1305 CNY = 130,500 micro-CNY
    // Provider cost at t=250: ($4 + $20) / 1000 = $0.024 * 7.25 = 0.1740 CNY = 174,000 micro-CNY
    assert!(entry_t250.provider_cost_micro_cny > entry_t150.provider_cost_micro_cny);
    assert_eq!(entry_t150.provider_cost_micro_cny, 130_500);
    assert_eq!(entry_t250.provider_cost_micro_cny, 174_000);
}

#[test]
fn test_gross_margin_report_and_manual_adjustment() {
    let engine = BillingEngine::new();
    let now_secs = 5000;

    let settings = BillingSettings {
        credit_face_value_cny: 0.01,
        usd_cny_rate: 7.00,
        rate_updated_at_secs: now_secs,
    };
    engine.update_settings(settings);

    let mut group = Group::pro_plus("group-report", "Report Group");
    group.rate_card_id = "rc-report".to_string();
    engine.upsert_group(group);

    // Rate card with 50% gross margin (multiplier 1.5)
    let version = RateCardVersion {
        id: "rcv-report-v1".to_string(),
        rate_card_id: "rc-report".to_string(),
        model: "gpt-4o".to_string(),
        currency: Currency::Cny,
        pricing_mode: PricingMode::CostPlus,
        input_price_per_m: 10.0,
        output_price_per_m: 30.0,
        cache_creation_price_per_m: 0.0,
        cache_read_price_per_m: 0.0,
        fixed_input_credit_per_m: 0,
        fixed_output_credit_per_m: 0,
        fixed_cache_creation_credit_per_m: 0,
        fixed_cache_read_credit_per_m: 0,
        per_call_credit: 0,
        margin_multiplier: 1.5,
        effective_from_secs: now_secs,
    };
    engine.upsert_rate_card_version(version);

    let card_id = "card-report-01";
    // The measured request costs 600 credits. Keep sufficient balance so the
    // production invariant (never allow a negative card balance) is exercised
    // without making the fixture depend on saturating arithmetic.
    let mut card = Card::new(card_id, "group-report", 1_000 * MICRO_CREDITS_PER_CREDIT);
    card.activate(now_secs, 86400 * 30).unwrap();
    engine.upsert_card(card);

    let params = ReservationEstimateParams::new(10_000, 10_000).with_model("gpt-4o");
    engine
        .reserve(card_id, "inv-rep-01", &params, now_secs, 300)
        .unwrap();

    let tokens = UsageTokens {
        uncached_input_tokens: 100_000, // 100k * 10 / 1M = 1.0 CNY
        output_tokens: 100_000,         // 100k * 30 / 1M = 3.0 CNY
        cache_creation_tokens: 0,
        cache_read_tokens: 0,
    };
    // Total cost = 4.0 CNY = 4,000,000 micro-CNY
    // User charge (1.5x): 6.0 CNY / 0.01 = 600 credits = 600,000,000 micro-credits

    engine
        .settle(
            "inv-rep-01",
            &tokens,
            "gpt-4o",
            "openai",
            "gpt-4o",
            now_secs + 5,
        )
        .unwrap();

    // Verify gross margin report
    let margin = engine.get_margin_summary();
    assert_eq!(margin.total_credits_charged, 600_000_000);
    assert_eq!(margin.total_revenue_micro_cny, 6_000_000); // 600 credits * 0.01 = 6 CNY
    assert_eq!(margin.total_provider_cost_micro_cny, 4_000_000); // 4 CNY
    assert_eq!(margin.gross_profit_micro_cny, 2_000_000); // 2 CNY profit
    assert!((margin.gross_margin_rate - (2.0 / 6.0)).abs() < 1e-4);

    // Test manual balance adjustment (Spec §5, §14.10.5)
    // Operator refunds 50 credits (= 50,000,000 micro-credits)
    let adj_entry = engine
        .adjust_balance(
            card_id,
            50 * MICRO_CREDITS_PER_CREDIT,
            "admin-007",
            "Customer compensation for network glitch",
            now_secs + 10,
        )
        .unwrap();

    assert_eq!(adj_entry.kind, LedgerKind::Adjustment);
    assert_eq!(adj_entry.credits_charged, 50_000_000);
    assert_eq!(adj_entry.provider_cost_micro_cny, 0);
    assert_eq!(adj_entry.operator_id.as_deref(), Some("admin-007"));
    assert_eq!(
        adj_entry.reason.as_deref(),
        Some("Customer compensation for network glitch")
    );

    // Margin summary must ignore non-usage ledger entries for cost/margin
    let margin_after_adj = engine.get_margin_summary();
    assert_eq!(margin_after_adj.total_provider_cost_micro_cny, 4_000_000);
}

#[test]
fn test_stacked_multipliers_model_map_group_and_rate_card() {
    let engine = BillingEngine::new();
    let now_secs = 6000;

    let settings = BillingSettings {
        credit_face_value_cny: 0.01,
        usd_cny_rate: 1.0, // 1:1 for simple math
        rate_updated_at_secs: now_secs,
    };
    engine.update_settings(settings);

    // Group level multiplier: 1.2
    let mut group = Group::pro_plus("group-stacked", "Stacked Multipliers Group");
    group.rate_card_id = "rc-stacked".to_string();
    group.margin_multiplier = 1.2;
    engine.upsert_group(group);

    // Rate card version level multiplier: 1.5
    let version = RateCardVersion {
        id: "rcv-stacked-v1".to_string(),
        rate_card_id: "rc-stacked".to_string(),
        model: "special-model".to_string(),
        currency: Currency::Cny,
        pricing_mode: PricingMode::CostPlus,
        input_price_per_m: 10.0,
        output_price_per_m: 20.0,
        cache_creation_price_per_m: 0.0,
        cache_read_price_per_m: 0.0,
        fixed_input_credit_per_m: 0,
        fixed_output_credit_per_m: 0,
        fixed_cache_creation_credit_per_m: 0,
        fixed_cache_read_credit_per_m: 0,
        per_call_credit: 0,
        margin_multiplier: 1.5,
        effective_from_secs: now_secs,
    };
    engine.upsert_rate_card_version(version);

    // Model map level multiplier: 2.0
    let model_map = ModelMap {
        id: "mm-stacked-01".to_string(),
        group_id: "group-stacked".to_string(),
        exposed_model_id: "special-model".to_string(),
        target_provider_id: "prov-1".to_string(),
        target_model: "special-model-upstream".to_string(),
        context_window: 100_000,
        max_output: 10_000,
        supports_tools: true,
        supports_vision: false,
        supports_reasoning: false,
        credit_multiplier: 2.0,
        visible: true,
        sort_order: 1,
        aliases: Vec::new(),
        fallback_chain: Vec::new(),
        display_name: None,
        description: None,
        rate_multiplier: None,
    };
    engine.upsert_model_map(model_map);

    let card_id = "card-stacked-01";
    let mut card = Card::new(card_id, "group-stacked", 500 * MICRO_CREDITS_PER_CREDIT);
    card.activate(now_secs, 86400).unwrap();
    engine.upsert_card(card);

    let params = ReservationEstimateParams::new(1000, 1000).with_model("special-model");
    engine
        .reserve(card_id, "inv-stacked", &params, now_secs, 300)
        .unwrap();

    // 100,000 input tokens = 1.0 CNY base cost
    // Multipliers: 1.5 (rate_card) * 1.2 (group) * 2.0 (model) = 3.6x
    // Cost: 1.0 CNY = 1,000,000 micro-CNY
    // Charge: (1.0 * 3.6) / 0.01 = 360 credits = 360,000,000 micro-credits
    let tokens = UsageTokens {
        uncached_input_tokens: 100_000,
        output_tokens: 0,
        cache_creation_tokens: 0,
        cache_read_tokens: 0,
    };

    let entry = engine
        .settle(
            "inv-stacked",
            &tokens,
            "special-model",
            "prov-1",
            "upstream",
            now_secs + 1,
        )
        .unwrap();

    assert_eq!(entry.provider_cost_micro_cny, 1_000_000);
    assert_eq!(entry.credits_charged, 360_000_000);
}

#[test]
fn test_audit_b_zero_and_empty_tokens_tolerance() {
    let engine = BillingEngine::new();
    let now_secs = 7000;

    let card_id = "card-zero-tokens";
    let mut card = Card::new(card_id, "group-pro-plus", 10 * MICRO_CREDITS_PER_CREDIT);
    card.activate(now_secs, 86400).unwrap();
    engine.upsert_card(card);

    let params = ReservationEstimateParams::new(0, 0);
    let reservation = engine
        .reserve(card_id, "inv-zero-01", &params, now_secs, 300)
        .expect("Zero reservation should succeed");
    assert_eq!(reservation.reserved_micro_credits, 0);

    let empty_tokens = UsageTokens::default();
    let entry = engine
        .settle(
            "inv-zero-01",
            &empty_tokens,
            "claude-sonnet-4.5",
            "anthropic",
            "claude-3-5-sonnet",
            now_secs + 1,
        )
        .expect("Zero token settlement should succeed");

    assert_eq!(entry.input_tokens, 0);
    assert_eq!(entry.output_tokens, 0);
    assert_eq!(entry.credits_charged, 0);
    assert_eq!(entry.provider_cost_micro_cny, 0);
}

#[test]
fn test_audit_b_fractional_sub_cent_rounding_precision() {
    let engine = BillingEngine::new();
    let now_secs = 8000;

    // Ultra low cost model: DeepSeek V3 ($0.14 / 1M input, $0.28 / 1M output)
    let version = RateCardVersion {
        id: "rcv-deepseek-v3".to_string(),
        rate_card_id: "default".to_string(),
        model: "deepseek-chat".to_string(),
        currency: Currency::Usd,
        pricing_mode: PricingMode::CostPlus,
        input_price_per_m: 0.14,
        output_price_per_m: 0.28,
        cache_creation_price_per_m: 0.14,
        cache_read_price_per_m: 0.014,
        fixed_input_credit_per_m: 0,
        fixed_output_credit_per_m: 0,
        fixed_cache_creation_credit_per_m: 0,
        fixed_cache_read_credit_per_m: 0,
        per_call_credit: 0,
        margin_multiplier: 1.0,
        effective_from_secs: now_secs,
    };
    engine.upsert_rate_card_version(version);

    let card_id = "card-subcent";
    let mut card = Card::new(card_id, "group-pro-plus", 10 * MICRO_CREDITS_PER_CREDIT);
    card.activate(now_secs, 86400).unwrap();
    engine.upsert_card(card);

    let params = ReservationEstimateParams::new(100, 100).with_model("deepseek-chat");
    engine
        .reserve(card_id, "inv-subcent", &params, now_secs, 300)
        .unwrap();

    // 10 input tokens ($0.14 / 1M = $0.0000014 USD = 0.00001015 CNY)
    // Even for tiny token counts, integer micro-units prevent underflow or panic
    let tokens = UsageTokens {
        uncached_input_tokens: 10,
        output_tokens: 5,
        cache_creation_tokens: 0,
        cache_read_tokens: 0,
    };

    let entry = engine
        .settle(
            "inv-subcent",
            &tokens,
            "deepseek-chat",
            "deepseek",
            "deepseek-chat",
            now_secs + 1,
        )
        .unwrap();

    assert!(entry.provider_cost_micro_cny >= 0);
    assert!(entry.credits_charged >= 0);
}

#[test]
fn test_audit_b_wildcard_model_fallback() {
    let engine = BillingEngine::new();
    let now_secs = 9000;

    // Rate card with wildcard model "*"
    let wildcard_version = RateCardVersion {
        id: "rcv-wildcard".to_string(),
        rate_card_id: "default".to_string(),
        model: "*".to_string(),
        currency: Currency::Cny,
        pricing_mode: PricingMode::PerCall,
        input_price_per_m: 1.0,
        output_price_per_m: 2.0,
        cache_creation_price_per_m: 0.0,
        cache_read_price_per_m: 0.0,
        fixed_input_credit_per_m: 0,
        fixed_output_credit_per_m: 0,
        fixed_cache_creation_credit_per_m: 0,
        fixed_cache_read_credit_per_m: 0,
        per_call_credit: 250_000,
        margin_multiplier: 1.0,
        effective_from_secs: now_secs,
    };
    engine.upsert_rate_card_version(wildcard_version);

    // Any model without an explicit rate card version should resolve to the wildcard version
    let resolved = engine
        .resolve_rate_card_version("default", "arbitrary-unknown-model", now_secs + 10)
        .expect("Should resolve wildcard version");

    assert_eq!(resolved.id, "rcv-wildcard");
    assert_eq!(resolved.pricing_mode, PricingMode::PerCall);
}

#[test]
fn test_audit_b_cross_group_rate_card_isolation() {
    let engine = BillingEngine::new();
    let now_secs = 10000;

    // Group A (Standard rate card): Fixed 50 credits per call
    let mut group_a = Group::pro_plus("group-standard", "Standard Group");
    group_a.rate_card_id = "rc-standard".to_string();
    engine.upsert_group(group_a);

    let v_standard = RateCardVersion {
        id: "rcv-std".to_string(),
        rate_card_id: "rc-standard".to_string(),
        model: "gpt-model".to_string(),
        currency: Currency::Cny,
        pricing_mode: PricingMode::PerCall,
        input_price_per_m: 1.0,
        output_price_per_m: 1.0,
        cache_creation_price_per_m: 0.0,
        cache_read_price_per_m: 0.0,
        fixed_input_credit_per_m: 0,
        fixed_output_credit_per_m: 0,
        fixed_cache_creation_credit_per_m: 0,
        fixed_cache_read_credit_per_m: 0,
        per_call_credit: 50_000_000, // 50 credits
        margin_multiplier: 1.0,
        effective_from_secs: now_secs,
    };
    engine.upsert_rate_card_version(v_standard);

    // Group B (Enterprise rate card): Fixed 10 credits per call (discounted)
    let mut group_b = Group::enterprise("group-ent", "Enterprise Group", "KIRO ENTERPRISE", None);
    group_b.rate_card_id = "rc-enterprise".to_string();
    group_b.margin_multiplier = 1.0;
    engine.upsert_group(group_b);

    let v_ent = RateCardVersion {
        id: "rcv-ent".to_string(),
        rate_card_id: "rc-enterprise".to_string(),
        model: "gpt-model".to_string(),
        currency: Currency::Cny,
        pricing_mode: PricingMode::PerCall,
        input_price_per_m: 1.0,
        output_price_per_m: 1.0,
        cache_creation_price_per_m: 0.0,
        cache_read_price_per_m: 0.0,
        fixed_input_credit_per_m: 0,
        fixed_output_credit_per_m: 0,
        fixed_cache_creation_credit_per_m: 0,
        fixed_cache_read_credit_per_m: 0,
        per_call_credit: 10_000_000, // 10 credits
        margin_multiplier: 1.0,
        effective_from_secs: now_secs,
    };
    engine.upsert_rate_card_version(v_ent);

    // Create cards for each group
    let mut card_a = Card::new(
        "card-std-01",
        "group-standard",
        100 * MICRO_CREDITS_PER_CREDIT,
    );
    card_a.activate(now_secs, 86400).unwrap();
    engine.upsert_card(card_a);

    let mut card_b = Card::new("card-ent-01", "group-ent", 100 * MICRO_CREDITS_PER_CREDIT);
    card_b.activate(now_secs, 86400).unwrap();
    engine.upsert_card(card_b);

    let params = ReservationEstimateParams::new(1000, 1000).with_model("gpt-model");

    // Standard card invocation
    engine
        .reserve("card-std-01", "inv-std", &params, now_secs, 300)
        .unwrap();
    let entry_a = engine
        .settle(
            "inv-std",
            &UsageTokens::default(),
            "gpt-model",
            "openai",
            "gpt-model",
            now_secs + 1,
        )
        .unwrap();

    // Enterprise card invocation
    engine
        .reserve("card-ent-01", "inv-ent", &params, now_secs, 300)
        .unwrap();
    let entry_b = engine
        .settle(
            "inv-ent",
            &UsageTokens::default(),
            "gpt-model",
            "openai",
            "gpt-model",
            now_secs + 1,
        )
        .unwrap();

    assert_eq!(entry_a.credits_charged, 50_000_000);
    assert_eq!(entry_b.credits_charged, 10_000_000);
    assert_eq!(entry_a.rate_card_version.as_deref(), Some("rcv-std"));
    assert_eq!(entry_b.rate_card_version.as_deref(), Some("rcv-ent"));
}

/// The reservation is the prepaid cap. Estimating input at the uncached price let a
/// version priced only on cache tokens reserve nothing, pass the balance check on an
/// empty card, and then charge without bound once the upstream reported cache reads.
#[test]
fn a_version_priced_only_on_cache_tokens_still_reserves() {
    let settings = BillingSettings::default();
    let mut version = RateCardVersion {
        id: "rcv-cache-only".to_string(),
        rate_card_id: "rc".to_string(),
        model: "m".to_string(),
        currency: Currency::Usd,
        pricing_mode: PricingMode::CostPlus,
        input_price_per_m: 0.0,
        output_price_per_m: 0.0,
        cache_creation_price_per_m: 0.0,
        cache_read_price_per_m: 3.0,
        fixed_input_credit_per_m: 0,
        fixed_output_credit_per_m: 0,
        fixed_cache_creation_credit_per_m: 0,
        fixed_cache_read_credit_per_m: 0,
        per_call_credit: 0,
        margin_multiplier: 1.0,
        effective_from_secs: 0,
    };
    let reserve = version.calculate_reserve_amount(10_000, 0, 1.0, 1.0, &settings);
    let charge = version.calculate_charge(
        &UsageTokens {
            cache_read_tokens: 10_000,
            ..UsageTokens::default()
        },
        1.0,
        1.0,
        &settings,
    );
    assert!(charge > 0);
    assert!(
        reserve >= charge,
        "reserved {reserve} for a charge of {charge}"
    );

    version.pricing_mode = PricingMode::Fixed;
    version.cache_read_price_per_m = 0.0;
    version.fixed_cache_read_credit_per_m = 5_000_000;
    assert!(version.calculate_reserve_amount(10_000, 0, 1.0, 1.0, &settings) > 0);
}

fn output_price(id: &str, rate_card_id: &str, model: &str, credits_per_m: i64) -> RateCardVersion {
    RateCardVersion {
        id: id.to_string(),
        rate_card_id: rate_card_id.to_string(),
        model: model.to_string(),
        currency: Currency::Cny,
        pricing_mode: PricingMode::Fixed,
        input_price_per_m: 0.0,
        output_price_per_m: 0.0,
        cache_creation_price_per_m: 0.0,
        cache_read_price_per_m: 0.0,
        fixed_input_credit_per_m: 0,
        fixed_output_credit_per_m: credits_per_m * MICRO_CREDITS_PER_CREDIT,
        fixed_cache_creation_credit_per_m: 0,
        fixed_cache_read_credit_per_m: 0,
        per_call_credit: 0,
        margin_multiplier: 1.0,
        effective_from_secs: 0,
    }
}

/// A price is resolved in one order everywhere: the exposed model's own price, then the
/// price of the model it is mapped to, then the rate card's wildcard. The wildcard was
/// tried right after the exposed model, so a cheap catch-all priced a mapped model a
/// thousand times under its target's price, in the reservation, the settlement and the
/// model list alike.
#[test]
fn a_wildcard_price_never_shadows_the_mapped_targets_price() {
    let engine = BillingEngine::new();
    let now = 1_000;
    let mut group = Group::pro_plus("group-target", "Target pricing");
    group.rate_card_id = "rc-target".to_string();
    engine.upsert_group(group);
    engine.upsert_model_map(ModelMap::new(
        "map-public",
        "group-target",
        "public",
        "prov",
        "target-x",
    ));
    engine.upsert_model_map(ModelMap::new(
        "map-other",
        "group-target",
        "other",
        "prov",
        "unpriced-target",
    ));
    engine.upsert_rate_card_version(output_price("v-target", "rc-target", "target-x", 1_000));
    engine.upsert_rate_card_version(output_price("v-star", "rc-target", "*", 1));
    let mut card = Card::new(
        "card-target",
        "group-target",
        5_000 * MICRO_CREDITS_PER_CREDIT,
    );
    card.activate(now, 86_400).unwrap();
    engine.upsert_card(card);
    let million_out = UsageTokens {
        output_tokens: 1_000_000,
        ..UsageTokens::default()
    };

    let reservation = engine
        .reserve(
            "card-target",
            "inv-reserved",
            &ReservationEstimateParams::new(0, 1_000_000).with_model("public"),
            now,
            300,
        )
        .unwrap();
    assert_eq!(reservation.rate_card_version.as_deref(), Some("v-target"));
    assert_eq!(
        reservation.reserved_micro_credits,
        1_000 * MICRO_CREDITS_PER_CREDIT
    );
    let entry = engine
        .settle(
            "inv-reserved",
            &million_out,
            "public",
            "prov",
            "target-x",
            now + 1,
        )
        .unwrap();
    assert_eq!(entry.rate_card_version.as_deref(), Some("v-target"));
    assert_eq!(entry.credits_charged, 1_000 * MICRO_CREDITS_PER_CREDIT);

    // A reservation that named no model is priced when it settles, in the same order.
    engine
        .reserve(
            "card-target",
            "inv-unnamed",
            &ReservationEstimateParams::new(0, 1),
            now,
            300,
        )
        .unwrap();
    let entry = engine
        .settle(
            "inv-unnamed",
            &million_out,
            "public",
            "prov",
            "target-x",
            now + 1,
        )
        .unwrap();
    assert_eq!(entry.rate_card_version.as_deref(), Some("v-target"));

    // The model list compares models at the price a request pays.
    assert_eq!(
        engine.display_price("group-target", "public", now),
        Some(1_000 * MICRO_CREDITS_PER_CREDIT)
    );
    // The wildcard still prices a model with no price of its own or of its target.
    assert_eq!(
        engine.display_price("group-target", "other", now),
        Some(MICRO_CREDITS_PER_CREDIT)
    );
}

/// A request is never billed at the built-in default rates. A model with no published
/// price, exact or wildcard, used to reserve and settle at 15 and 60 credits per million
/// tokens with the group margin and the model multiplier dropped: here a third of what
/// the priced model in the same group costs. It is refused before any work instead.
#[test]
fn a_model_without_a_published_price_is_refused_before_any_work() {
    let engine = BillingEngine::new();
    let now = 1_000;
    let mut group = Group::pro_plus("group-priced", "Priced");
    group.margin_multiplier = 3.0;
    engine.upsert_group(group);
    engine.upsert_rate_card_version(output_price("v-priced", "default", "priced", 60));
    let mut card = Card::new(
        "card-priced",
        "group-priced",
        1_000 * MICRO_CREDITS_PER_CREDIT,
    );
    card.activate(now, 86_400).unwrap();
    engine.upsert_card(card);

    let params = ReservationEstimateParams::new(1_000, 1_000);
    assert_eq!(
        engine.reserve(
            "card-priced",
            "inv-unpriced",
            &params.clone().with_model("unpriced"),
            now,
            300
        ),
        Err(BillingError::ModelNotPriced("unpriced".to_string()))
    );
    assert_eq!(engine.get_card("card-priced").unwrap().credit_reserved, 0);
    assert!(engine.export_snapshot().reservations.is_empty());

    let reservation = engine
        .reserve(
            "card-priced",
            "inv-priced",
            &params.with_model("priced"),
            now,
            300,
        )
        .unwrap();
    assert_eq!(reservation.rate_card_version.as_deref(), Some("v-priced"));
}
