//! Test suite for P2-5b: Pricing workbench, WYSIWYG pricing sheet, simulator, plan calibrator, and audit logs.
//! Spec §14.10.3, §14.10.4.

use billing::card::Card;
use billing::engine::BillingEngine;
use billing::group::Group;
use billing::ledger::UsageTokens;
use billing::rate_card::{BillingSettings, Currency, PricingMode, RateCard, RateCardVersion};
use billing::workbench::{calibrate_plan, ModelPresets, PlanCalibrationInput, PricingSheetRow};
use billing::MICRO_CREDITS_PER_CREDIT;

#[test]
fn test_builtin_model_presets() {
    let now_secs = 1000;

    // Test Claude Sonnet 4.5 preset
    let claude = ModelPresets::get_preset("claude-sonnet-4.5", "rc-default", "v-sonnet", now_secs)
        .expect("Preset must exist");
    assert_eq!(claude.model, "claude-sonnet-4.5");
    assert_eq!(claude.currency, Currency::Usd);
    assert_eq!(claude.input_price_per_m, 3.00);
    assert_eq!(claude.output_price_per_m, 15.00);
    assert_eq!(claude.cache_creation_price_per_m, 3.75);
    assert_eq!(claude.cache_read_price_per_m, 0.30);
    assert_eq!(claude.margin_multiplier, 1.30);

    // Test DeepSeek V3 preset
    let deepseek = ModelPresets::get_preset("deepseek-v3", "rc-default", "v-ds", now_secs)
        .expect("DeepSeek preset must exist");
    assert_eq!(deepseek.model, "deepseek-chat");
    assert_eq!(deepseek.input_price_per_m, 0.14);
    assert_eq!(deepseek.output_price_per_m, 0.28);
    assert_eq!(deepseek.cache_read_price_per_m, 0.014);

    // Unknown preset returns None
    assert!(ModelPresets::get_preset("unknown-model", "rc", "v", now_secs).is_none());
}

#[test]
fn test_wysiwyg_pricing_sheet_bidirectional_linking() {
    let face_value = 0.01; // 1 积分 = 0.01 元 (1分钱)
    let input_cost_cny = 21.6; // ¥21.60 / 1M ($3.00 * 7.2)
    let output_cost_cny = 108.0; // ¥108.00 / 1M ($15.00 * 7.2)

    // 1. Forward derivation from multiplier 1.25 (20% gross margin)
    let sheet1 = PricingSheetRow::from_multiplier(
        "claude-sonnet",
        input_cost_cny,
        output_cost_cny,
        1.25,
        face_value,
    );
    assert_eq!(sheet1.selling_cny_input_per_m, 27.0); // 21.6 * 1.25 = 27.0
    assert_eq!(sheet1.selling_credits_input_per_m, 2700.0); // 27.0 / 0.01 = 2700 credits
    assert!((sheet1.gross_margin_rate - 0.20).abs() < 1e-4); // 1 - 1/1.25 = 0.20 (20%)

    // 2. Reverse derivation from target margin: Want 30% gross margin
    let sheet2 = PricingSheetRow::from_target_margin(
        "claude-sonnet",
        input_cost_cny,
        output_cost_cny,
        0.30,
        face_value,
    );
    // multiplier = 1 / (1 - 0.3) = 1.42857...
    assert!((sheet2.margin_multiplier - (1.0 / 0.7)).abs() < 1e-4);
    assert!((sheet2.gross_margin_rate - 0.30).abs() < 1e-4);

    // 3. Reverse derivation from target credit price: Want 3,000 credits/1M input
    let sheet3 = PricingSheetRow::from_selling_credit_price(
        "claude-sonnet",
        input_cost_cny,
        output_cost_cny,
        3000.0,
        face_value,
    );
    // selling cny = 3000 * 0.01 = 30.0 CNY
    // multiplier = 30.0 / 21.6 = 1.3888...
    assert!((sheet3.selling_cny_input_per_m - 30.0).abs() < 1e-4);
    assert!((sheet3.margin_multiplier - (30.0 / 21.6)).abs() < 1e-4);
}

#[test]
fn test_pricing_simulator_over_historical_usage() {
    let engine = BillingEngine::new();
    let now_secs = 2000;

    let settings = BillingSettings {
        credit_face_value_cny: 0.01,
        usd_cny_rate: 7.20,
        rate_updated_at_secs: now_secs,
    };
    engine.update_settings(settings);

    let mut group = Group::pro_plus("group-sim", "Sim Group");
    group.rate_card_id = "rc-sim".to_string();
    engine.upsert_group(group);

    // Rate card Version 1: $3.00 / $15.00, margin multiplier 1.2
    let v1 = RateCardVersion {
        id: "rcv-v1".to_string(),
        rate_card_id: "rc-sim".to_string(),
        model: "gpt-4o".to_string(),
        currency: Currency::Usd,
        pricing_mode: PricingMode::CostPlus,
        input_price_per_m: 3.00,
        output_price_per_m: 15.00,
        cache_creation_price_per_m: 0.0,
        cache_read_price_per_m: 0.0,
        fixed_input_credit_per_m: 0,
        fixed_output_credit_per_m: 0,
        fixed_cache_creation_credit_per_m: 0,
        fixed_cache_read_credit_per_m: 0,
        per_call_credit: 0,
        margin_multiplier: 1.20,
        effective_from_secs: now_secs,
    };
    engine.upsert_rate_card_version(v1);

    // Seed card and generate 3 historical invocations under v1
    let card_id = "card-sim-01";
    let mut card = Card::new(card_id, "group-sim", 1000 * MICRO_CREDITS_PER_CREDIT);
    card.activate(now_secs, 86400 * 30).unwrap();
    engine.upsert_card(card);

    let params =
        billing::reservation::ReservationEstimateParams::new(1000, 1000).with_model("gpt-4o");
    let tokens = UsageTokens {
        uncached_input_tokens: 10_000,
        output_tokens: 2_000,
        cache_creation_tokens: 0,
        cache_read_tokens: 0,
    };

    for i in 1..=3 {
        let inv = format!("inv-sim-{}", i);
        engine
            .reserve(card_id, &inv, &params, now_secs + i, 300)
            .unwrap();
        engine
            .settle(
                &inv,
                &tokens,
                "gpt-4o",
                "openai",
                "gpt-4o",
                now_secs + i + 1,
            )
            .unwrap();
    }

    // Candidate version to simulate: Price hike to $4.00 / $20.00, margin multiplier 1.50
    let candidate = RateCardVersion {
        id: "rcv-candidate".to_string(),
        rate_card_id: "rc-sim".to_string(),
        model: "gpt-4o".to_string(),
        currency: Currency::Usd,
        pricing_mode: PricingMode::CostPlus,
        input_price_per_m: 4.00,
        output_price_per_m: 20.00,
        cache_creation_price_per_m: 0.0,
        cache_read_price_per_m: 0.0,
        fixed_input_credit_per_m: 0,
        fixed_output_credit_per_m: 0,
        fixed_cache_creation_credit_per_m: 0,
        fixed_cache_read_credit_per_m: 0,
        per_call_credit: 0,
        margin_multiplier: 1.50,
        effective_from_secs: now_secs + 100,
    };

    let sim = engine.simulate_candidate_pricing(&candidate, None, now_secs + 50, 30.0);

    assert_eq!(sim.sample_entries_count, 3);
    assert!(sim.simulated_credits_charged > sim.original_credits_charged);
    assert!(sim.simulated_revenue_micro_cny > sim.original_revenue_micro_cny);
    assert!(sim.simulated_gross_margin_rate > sim.original_gross_margin_rate);
    assert!(sim.burn_rate_ratio > 1.0);
    // If users burn credits faster, the 30-day card will deplete in fewer days
    assert!(sim.projected_monthly_card_days < 30.0);
}

#[test]
fn test_plan_calibrator() {
    let input = PlanCalibrationInput {
        card_total_credits: 50_000 * MICRO_CREDITS_PER_CREDIT, // 50,000 credits
        target_duration_days: 30,                              // 30 days
        avg_daily_input_tokens: 100_000,                       // 100k input/day
        avg_daily_output_tokens: 20_000,                       // 20k output/day
        upstream_input_cost_cny_per_m: 21.6,                   // ¥21.60 / 1M
        upstream_output_cost_cny_per_m: 108.0,                 // ¥108.00 / 1M
        credit_face_value_cny: 0.01,
    };

    let output = calibrate_plan(&input);

    // Target daily burn = 50,000 / 30 = 1,666.666... credits/day
    assert!((output.target_daily_burn_credits - (50_000.0 / 30.0)).abs() < 1e-4);

    // Daily provider cost: 100k * 21.6 / 1M + 20k * 108.0 / 1M = 2.16 + 2.16 = 4.32 CNY/day
    // Total provider cost over 30 days = 4.32 * 30 = 129.6 CNY
    assert!((output.expected_total_provider_cost_cny - 129.6).abs() < 1e-4);

    // Plan revenue = 50,000 credits * 0.01 = 500.0 CNY
    assert_eq!(output.expected_plan_revenue_cny, 500.0);

    // Gross margin = (500 - 129.6) / 500 = 74.08%
    assert!((output.expected_gross_margin_rate - 0.7408).abs() < 1e-4);

    // Recommended cost-plus multiplier: (1666.67 * 0.01) / 4.32 = 16.6667 / 4.32 = 3.858
    assert!(output.recommended_cost_plus_multiplier > 3.0);
    assert!(output.recommended_fixed_input_credit_per_m > 0);
    assert!(output.recommended_fixed_output_credit_per_m > 0);
}

#[test]
fn test_one_click_publish_rate_card_version_with_audit_log() {
    let engine = BillingEngine::new();
    let now_secs = 3000;

    let rate_card = RateCard::new("rc-pub", "Production Rate Card", now_secs);
    engine.upsert_rate_card(rate_card);

    let v1 = RateCardVersion {
        id: "rcv-pub-v1".to_string(),
        rate_card_id: "rc-pub".to_string(),
        model: "claude-sonnet".to_string(),
        currency: Currency::Usd,
        pricing_mode: PricingMode::CostPlus,
        input_price_per_m: 3.00,
        output_price_per_m: 15.00,
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

    // Publish v1
    let audit1 = engine.publish_rate_card_version(
        v1,
        "admin-alice",
        "Initial pricing release for Claude Sonnet",
        now_secs,
    );
    assert_eq!(audit1.version_id, "rcv-pub-v1");
    assert_eq!(audit1.operator_id, "admin-alice");
    assert_eq!(audit1.previous_version_id, None);

    // Publish v2
    let v2 = RateCardVersion {
        id: "rcv-pub-v2".to_string(),
        rate_card_id: "rc-pub".to_string(),
        model: "claude-sonnet".to_string(),
        currency: Currency::Usd,
        pricing_mode: PricingMode::CostPlus,
        input_price_per_m: 3.50,
        output_price_per_m: 17.50,
        cache_creation_price_per_m: 4.00,
        cache_read_price_per_m: 0.35,
        fixed_input_credit_per_m: 0,
        fixed_output_credit_per_m: 0,
        fixed_cache_creation_credit_per_m: 0,
        fixed_cache_read_credit_per_m: 0,
        per_call_credit: 0,
        margin_multiplier: 1.30,
        effective_from_secs: now_secs + 100,
    };

    let audit2 = engine.publish_rate_card_version(
        v2,
        "admin-bob",
        "Upstream price update adjustments",
        now_secs + 100,
    );
    assert_eq!(audit2.version_id, "rcv-pub-v2");
    assert_eq!(audit2.operator_id, "admin-bob");
    assert_eq!(audit2.previous_version_id.as_deref(), Some("rcv-pub-v1"));

    // Verify audit logs history
    let logs = engine.list_rate_card_audit_logs(Some("rc-pub"));
    assert_eq!(logs.len(), 2);
    assert_eq!(logs[0].version_id, "rcv-pub-v1");
    assert_eq!(logs[1].version_id, "rcv-pub-v2");
}
