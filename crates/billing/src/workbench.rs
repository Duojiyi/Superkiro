//! Pricing workbench, WYSIWYG sheet, pricing simulator, and plan calibrator (Spec §14.10.3).
//!
//! Provides operators with tools to:
//! 1. Import preset upstream costs for major models (Claude, GPT, DeepSeek).
//! 2. Interactively link cost (¥/1M) <-> multiplier <-> selling price (Credits/1M) <-> gross margin.
//! 3. Simulate historical usage under candidate rate cards to prevent pricing accidents.
//! 4. Reverse-calibrate plan pricing ("Card = X credits, typical user lasts 30 days").
//! 5. One-click publish new RateCardVersion with immutable audit logs.

use crate::ledger::{LedgerEntry, LedgerKind};
use crate::rate_card::{BillingSettings, Currency, PricingMode, RateCardVersion};
use crate::MICRO_CREDITS_PER_CREDIT;
use serde::{Deserialize, Serialize};

/// Audit log record for rate card version publication (Spec §14.10.3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RateCardAuditLog {
    pub id: String,
    pub rate_card_id: String,
    pub version_id: String,
    pub operator_id: String,
    pub reason: String,
    pub created_at_secs: u64,
    pub previous_version_id: Option<String>,
}

/// Built-in official upstream pricing presets for mainstream models (Spec §14.10.3).
pub struct ModelPresets;

impl ModelPresets {
    /// Return standard RateCardVersion preset for given model ID.
    pub fn get_preset(
        preset_name: &str,
        rate_card_id: &str,
        version_id: &str,
        now_secs: u64,
    ) -> Option<RateCardVersion> {
        let (model, currency, in_p, out_p, cc_p, cr_p) = match preset_name {
            "claude-3-5-sonnet" | "claude-sonnet-4.5" => {
                ("claude-sonnet-4.5", Currency::Usd, 3.00, 15.00, 3.75, 0.30)
            }
            "claude-3-5-haiku" => ("claude-haiku", Currency::Usd, 0.80, 4.00, 1.00, 0.08),
            "gpt-4o" => ("gpt-4o", Currency::Usd, 2.50, 10.00, 2.50, 1.25),
            "gpt-4o-mini" => ("gpt-4o-mini", Currency::Usd, 0.15, 0.60, 0.15, 0.075),
            "deepseek-v3" | "deepseek-chat" => {
                ("deepseek-chat", Currency::Usd, 0.14, 0.28, 0.14, 0.014)
            }
            "deepseek-r1" | "deepseek-reasoner" => {
                ("deepseek-reasoner", Currency::Usd, 0.55, 2.19, 0.55, 0.14)
            }
            _ => return None,
        };

        Some(RateCardVersion {
            id: version_id.to_string(),
            rate_card_id: rate_card_id.to_string(),
            model: model.to_string(),
            currency,
            pricing_mode: PricingMode::CostPlus,
            input_price_per_m: in_p,
            output_price_per_m: out_p,
            cache_creation_price_per_m: cc_p,
            cache_read_price_per_m: cr_p,
            fixed_input_credit_per_m: 0,
            fixed_output_credit_per_m: 0,
            fixed_cache_creation_credit_per_m: 0,
            fixed_cache_read_credit_per_m: 0,
            per_call_credit: 0,
            margin_multiplier: 1.30, // 30% default target gross margin
            effective_from_secs: now_secs,
        })
    }
}

/// WYSIWYG pricing sheet row linking cost, multipliers, selling credits, RMB price, and margin rate (Spec §14.10.3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PricingSheetRow {
    pub model_id: String,
    pub input_cost_cny_per_m: f64,
    pub output_cost_cny_per_m: f64,
    pub margin_multiplier: f64,
    pub selling_credits_input_per_m: f64,
    pub selling_credits_output_per_m: f64,
    pub selling_cny_input_per_m: f64,
    pub selling_cny_output_per_m: f64,
    pub gross_margin_rate: f64,
}

impl PricingSheetRow {
    /// Calculate row from upstream costs, gross margin multiplier, and credit face value.
    pub fn from_multiplier(
        model_id: impl Into<String>,
        input_cost_cny_per_m: f64,
        output_cost_cny_per_m: f64,
        margin_multiplier: f64,
        face_value_cny: f64,
    ) -> Self {
        let fv = if face_value_cny > 0.0 {
            face_value_cny
        } else {
            0.01
        };
        let selling_cny_in = input_cost_cny_per_m * margin_multiplier;
        let selling_cny_out = output_cost_cny_per_m * margin_multiplier;

        let selling_credits_in = selling_cny_in / fv;
        let selling_credits_out = selling_cny_out / fv;

        // gross margin rate = (selling - cost) / selling = 1.0 - 1.0 / multiplier
        let gross_margin_rate = if margin_multiplier > 0.0 {
            1.0 - (1.0 / margin_multiplier)
        } else {
            0.0
        };

        Self {
            model_id: model_id.into(),
            input_cost_cny_per_m,
            output_cost_cny_per_m,
            margin_multiplier,
            selling_credits_input_per_m: selling_credits_in,
            selling_credits_output_per_m: selling_credits_out,
            selling_cny_input_per_m: selling_cny_in,
            selling_cny_output_per_m: selling_cny_out,
            gross_margin_rate,
        }
    }

    /// Reverse derive multiplier and selling credits from a target gross margin rate (e.g. 0.35 for 35%).
    pub fn from_target_margin(
        model_id: impl Into<String>,
        input_cost_cny_per_m: f64,
        output_cost_cny_per_m: f64,
        target_margin_rate: f64,
        face_value_cny: f64,
    ) -> Self {
        // margin_rate = 1 - 1/mult => mult = 1 / (1 - margin_rate)
        let margin_rate = target_margin_rate.clamp(0.0, 0.99);
        let multiplier = 1.0 / (1.0 - margin_rate);
        Self::from_multiplier(
            model_id,
            input_cost_cny_per_m,
            output_cost_cny_per_m,
            multiplier,
            face_value_cny,
        )
    }

    /// Reverse derive multiplier and gross margin from a target fixed credit selling price.
    pub fn from_selling_credit_price(
        model_id: impl Into<String>,
        input_cost_cny_per_m: f64,
        output_cost_cny_per_m: f64,
        selling_credits_input_per_m: f64,
        face_value_cny: f64,
    ) -> Self {
        let fv = if face_value_cny > 0.0 {
            face_value_cny
        } else {
            0.01
        };
        let selling_cny_in = selling_credits_input_per_m * fv;
        let multiplier = if input_cost_cny_per_m > 0.0 {
            selling_cny_in / input_cost_cny_per_m
        } else {
            1.0
        };

        Self::from_multiplier(
            model_id,
            input_cost_cny_per_m,
            output_cost_cny_per_m,
            multiplier,
            face_value_cny,
        )
    }
}

/// Results produced by pricing simulation over historical usage entries (Spec §14.10.3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SimulationResult {
    pub sample_entries_count: usize,
    pub original_credits_charged: i64,
    pub simulated_credits_charged: i64,
    pub credits_delta: i64,
    pub original_revenue_micro_cny: i64,
    pub simulated_revenue_micro_cny: i64,
    pub revenue_delta_micro_cny: i64,
    pub provider_cost_micro_cny: i64,
    pub original_gross_profit_micro_cny: i64,
    pub simulated_gross_profit_micro_cny: i64,
    pub original_gross_margin_rate: f64,
    pub simulated_gross_margin_rate: f64,
    /// Ratio of simulated consumption vs original (1.2 = users burn credits 20% faster)
    pub burn_rate_ratio: f64,
    /// Projected average days for a standard 30-day card to deplete under new price
    pub projected_monthly_card_days: f64,
}

/// Simulate applying a candidate RateCardVersion to a set of historical ledger entries.
pub fn simulate_candidate_pricing(
    historical_entries: &[LedgerEntry],
    candidate_version: &RateCardVersion,
    settings: &BillingSettings,
    default_monthly_card_days: f64,
) -> SimulationResult {
    let mut count = 0;
    let mut original_credits = 0i64;
    let mut simulated_credits = 0i64;
    let mut total_provider_cost = 0i64;

    for entry in historical_entries {
        if entry.kind != LedgerKind::Usage {
            continue;
        }

        // Only evaluate entries targeting candidate model (or wildcard)
        if candidate_version.model != "*" && entry.exposed_model != candidate_version.model {
            continue;
        }

        count += 1;
        original_credits = original_credits.saturating_add(entry.credits_charged);
        total_provider_cost = total_provider_cost.saturating_add(entry.provider_cost_micro_cny);

        let tokens = crate::ledger::UsageTokens {
            uncached_input_tokens: entry
                .input_tokens
                .saturating_sub(entry.cache_creation_tokens + entry.cache_read_tokens),
            output_tokens: entry.output_tokens,
            cache_creation_tokens: entry.cache_creation_tokens,
            cache_read_tokens: entry.cache_read_tokens,
        };

        let sim_charge = candidate_version.calculate_charge(&tokens, 1.0, 1.0, settings);
        simulated_credits = simulated_credits.saturating_add(sim_charge);
    }

    let fv = if settings.credit_face_value_cny > 0.0 {
        settings.credit_face_value_cny
    } else {
        0.01
    };
    let orig_rev = ((original_credits as f64) * fv).round() as i64;
    let sim_rev = ((simulated_credits as f64) * fv).round() as i64;

    let orig_profit = orig_rev.saturating_sub(total_provider_cost);
    let sim_profit = sim_rev.saturating_sub(total_provider_cost);

    let orig_margin = if orig_rev > 0 {
        (orig_profit as f64) / (orig_rev as f64)
    } else {
        0.0
    };

    let sim_margin = if sim_rev > 0 {
        (sim_profit as f64) / (sim_rev as f64)
    } else {
        0.0
    };

    let burn_rate_ratio = if original_credits > 0 {
        (simulated_credits as f64) / (original_credits as f64)
    } else {
        1.0
    };

    let projected_monthly_card_days = if burn_rate_ratio > 0.0 {
        default_monthly_card_days / burn_rate_ratio
    } else {
        default_monthly_card_days
    };

    SimulationResult {
        sample_entries_count: count,
        original_credits_charged: original_credits,
        simulated_credits_charged: simulated_credits,
        credits_delta: simulated_credits - original_credits,
        original_revenue_micro_cny: orig_rev,
        simulated_revenue_micro_cny: sim_rev,
        revenue_delta_micro_cny: sim_rev - orig_rev,
        provider_cost_micro_cny: total_provider_cost,
        original_gross_profit_micro_cny: orig_profit,
        simulated_gross_profit_micro_cny: sim_profit,
        original_gross_margin_rate: orig_margin,
        simulated_gross_margin_rate: sim_margin,
        burn_rate_ratio,
        projected_monthly_card_days,
    }
}

/// Inputs for plan calibrator (Spec §14.10.3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanCalibrationInput {
    pub card_total_credits: i64,
    pub target_duration_days: u32,
    pub avg_daily_input_tokens: u64,
    pub avg_daily_output_tokens: u64,
    pub upstream_input_cost_cny_per_m: f64,
    pub upstream_output_cost_cny_per_m: f64,
    pub credit_face_value_cny: f64,
}

/// Output recommendation from plan calibrator (Spec §14.10.3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanCalibrationOutput {
    pub target_daily_burn_credits: f64,
    pub recommended_fixed_input_credit_per_m: i64,
    pub recommended_fixed_output_credit_per_m: i64,
    pub recommended_cost_plus_multiplier: f64,
    pub expected_total_provider_cost_cny: f64,
    pub expected_plan_revenue_cny: f64,
    pub expected_gross_margin_rate: f64,
}

/// Calibrate plan pricing to ensure typical user card lasts exactly target days (Spec §14.10.3).
pub fn calibrate_plan(input: &PlanCalibrationInput) -> PlanCalibrationOutput {
    let days = input.target_duration_days.max(1) as f64;
    let card_credits = (input.card_total_credits as f64) / (MICRO_CREDITS_PER_CREDIT as f64);
    let target_daily_burn = card_credits / days;

    // Daily provider cost in CNY
    let daily_cost_cny =
        (input.avg_daily_input_tokens as f64) * input.upstream_input_cost_cny_per_m / 1_000_000.0
            + (input.avg_daily_output_tokens as f64) * input.upstream_output_cost_cny_per_m
                / 1_000_000.0;

    let fv = if input.credit_face_value_cny > 0.0 {
        input.credit_face_value_cny
    } else {
        0.01
    };
    let expected_plan_revenue_cny = card_credits * fv;
    let expected_total_provider_cost_cny = daily_cost_cny * days;

    // Multiplier needed so that daily cost * multiplier / face_value = target_daily_burn
    let recommended_cost_plus_multiplier = if daily_cost_cny > 0.0 {
        (target_daily_burn * fv) / daily_cost_cny
    } else {
        1.0
    };

    // Recommended fixed credit prices per 1M tokens matching the same ratio
    let rec_fixed_in = ((input.upstream_input_cost_cny_per_m * recommended_cost_plus_multiplier
        / fv)
        * (MICRO_CREDITS_PER_CREDIT as f64))
        .round() as i64;
    let rec_fixed_out = ((input.upstream_output_cost_cny_per_m * recommended_cost_plus_multiplier
        / fv)
        * (MICRO_CREDITS_PER_CREDIT as f64))
        .round() as i64;

    let expected_gross_margin_rate = if expected_plan_revenue_cny > 0.0 {
        (expected_plan_revenue_cny - expected_total_provider_cost_cny) / expected_plan_revenue_cny
    } else {
        0.0
    };

    PlanCalibrationOutput {
        target_daily_burn_credits: target_daily_burn,
        recommended_fixed_input_credit_per_m: rec_fixed_in,
        recommended_fixed_output_credit_per_m: rec_fixed_out,
        recommended_cost_plus_multiplier,
        expected_total_provider_cost_cny,
        expected_plan_revenue_cny,
        expected_gross_margin_rate,
    }
}
