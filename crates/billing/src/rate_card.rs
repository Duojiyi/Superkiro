//! Rate card versioning, tiered cache pricing, and cost-side tracking (Spec §5, §6.4, §6.5, §14.10).
//!
//! Three pricing modes:
//! 1. CostPlus: `credit = tokens * provider_cost * rate * multipliers / face_value` (per input/output/cache_read/cache_creation)
//! 2. Fixed: Fixed micro-credits per 1M tokens, decoupled from upstream cost
//! 3. PerCall: Fixed micro-credits per request invocation
//!
//! Upstream provider cost in micro-CNY is always tracked in the authoritative ledger for gross margin reporting.

use crate::ledger::{ceil_nonnegative_to_i64, UsageTokens};
use crate::MICRO_CREDITS_PER_CREDIT;
use serde::{Deserialize, Serialize};

/// Currency denomination for upstream provider prices (Spec §5, §14.10.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Currency {
    Usd,
    Cny,
}

/// Three pricing modes configurable by operator per model (Spec §14.10.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PricingMode {
    /// Standard cost-plus markup over upstream provider token costs.
    #[default]
    CostPlus,
    /// Fixed credits per 1M tokens, decoupled from upstream provider cost fluctuations.
    Fixed,
    /// Fixed flat credit charge per request invocation.
    PerCall,
}

/// Global billing anchor settings (Spec §5, §14.10.1, §14.10.5).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BillingSettings {
    /// RMB face value of 1 credit (e.g. 0.01 = 0.01 CNY per credit, i.e. 1分钱/积分).
    pub credit_face_value_cny: f64,
    /// USD to CNY exchange rate (e.g. 7.25).
    pub usd_cny_rate: f64,
    /// Unix timestamp when the exchange rate was updated.
    #[serde(default)]
    pub rate_updated_at_secs: u64,
}

impl Default for BillingSettings {
    fn default() -> Self {
        Self {
            credit_face_value_cny: 0.01,
            usd_cny_rate: 7.25,
            rate_updated_at_secs: 0,
        }
    }
}

/// Rate card header group (Spec §5).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RateCard {
    pub id: String,
    pub name: String,
    pub created_at_secs: u64,
}

impl RateCard {
    pub fn new(id: impl Into<String>, name: impl Into<String>, created_at_secs: u64) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            created_at_secs,
        }
    }
}

/// Versioned rate card entry for a specific model (Spec §5, §6.4, §14.10.2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RateCardVersion {
    pub id: String,
    pub rate_card_id: String,
    pub model: String,
    pub currency: Currency,
    pub pricing_mode: PricingMode,

    // Upstream provider base cost per 1M tokens in `currency` (Spec §5, §14.10.1)
    pub input_price_per_m: f64,
    pub output_price_per_m: f64,
    pub cache_creation_price_per_m: f64,
    pub cache_read_price_per_m: f64,

    // Fixed pricing mode: micro-credits charged per 1M tokens (Spec §14.10.2)
    pub fixed_input_credit_per_m: i64,
    pub fixed_output_credit_per_m: i64,
    pub fixed_cache_creation_credit_per_m: i64,
    pub fixed_cache_read_credit_per_m: i64,

    // Per-call pricing mode: micro-credits charged per invocation (Spec §14.10.2)
    pub per_call_credit: i64,

    // Rate card level gross margin multiplier (Spec §5, §14.10.1; e.g. 1.3 = 30% margin)
    pub margin_multiplier: f64,

    // Version activation timestamp (Spec §6.4)
    pub effective_from_secs: u64,
}

impl RateCardVersion {
    /// Calculate upstream provider cost in micro-CNY (Spec §5, §14.10.5).
    /// Always tracked for gross margin reporting, regardless of pricing mode.
    pub fn calculate_cost_micro_cny(
        &self,
        tokens: &UsageTokens,
        settings: &BillingSettings,
    ) -> i64 {
        let uncached = (tokens.uncached_input_tokens as f64) * self.input_price_per_m / 1_000_000.0;
        let output = (tokens.output_tokens as f64) * self.output_price_per_m / 1_000_000.0;
        let cache_create =
            (tokens.cache_creation_tokens as f64) * self.cache_creation_price_per_m / 1_000_000.0;
        let cache_read =
            (tokens.cache_read_tokens as f64) * self.cache_read_price_per_m / 1_000_000.0;

        let total_currency_cost = uncached + output + cache_create + cache_read;
        let total_cny_cost = match self.currency {
            Currency::Usd => total_currency_cost * settings.usd_cny_rate,
            Currency::Cny => total_currency_cost,
        };

        // 1 CNY = 1_000_000 micro-CNY
        ceil_nonnegative_to_i64((total_cny_cost * 1_000_000.0).round())
    }

    /// Calculate micro-credits to charge user according to configured PricingMode (Spec §14.10.2).
    /// Stacked multipliers: `self.margin_multiplier * group_margin * model_multiplier` (Spec §14.10.1).
    pub fn calculate_charge(
        &self,
        tokens: &UsageTokens,
        group_margin: f64,
        model_multiplier: f64,
        settings: &BillingSettings,
    ) -> i64 {
        let multipliers = self.margin_multiplier * group_margin * model_multiplier;

        match self.pricing_mode {
            PricingMode::CostPlus => {
                let uncached =
                    (tokens.uncached_input_tokens as f64) * self.input_price_per_m / 1_000_000.0;
                let output = (tokens.output_tokens as f64) * self.output_price_per_m / 1_000_000.0;
                let cache_create = (tokens.cache_creation_tokens as f64)
                    * self.cache_creation_price_per_m
                    / 1_000_000.0;
                let cache_read =
                    (tokens.cache_read_tokens as f64) * self.cache_read_price_per_m / 1_000_000.0;

                let total_currency = uncached + output + cache_create + cache_read;
                let total_cny = match self.currency {
                    Currency::Usd => total_currency * settings.usd_cny_rate,
                    Currency::Cny => total_currency,
                };

                let face_val = if settings.credit_face_value_cny > 0.0 {
                    settings.credit_face_value_cny
                } else {
                    0.01
                };

                let micro_credits =
                    (total_cny * multipliers / face_val) * (MICRO_CREDITS_PER_CREDIT as f64);
                ceil_nonnegative_to_i64(micro_credits)
            }
            PricingMode::Fixed => {
                let uncached = (tokens.uncached_input_tokens as f64)
                    * (self.fixed_input_credit_per_m as f64)
                    / 1_000_000.0;
                let output = (tokens.output_tokens as f64)
                    * (self.fixed_output_credit_per_m as f64)
                    / 1_000_000.0;
                let cache_create = (tokens.cache_creation_tokens as f64)
                    * (self.fixed_cache_creation_credit_per_m as f64)
                    / 1_000_000.0;
                let cache_read = (tokens.cache_read_tokens as f64)
                    * (self.fixed_cache_read_credit_per_m as f64)
                    / 1_000_000.0;

                let base_charge = uncached + output + cache_create + cache_read;
                let final_charge = base_charge * multipliers;
                ceil_nonnegative_to_i64(final_charge)
            }
            PricingMode::PerCall => {
                let final_charge = (self.per_call_credit as f64) * multipliers;
                ceil_nonnegative_to_i64(final_charge)
            }
        }
    }

    /// Calculate estimated reservation amount in micro-credits before upstream invocation (Spec §6.2).
    pub fn calculate_reserve_amount(
        &self,
        estimated_input: u64,
        max_output: u64,
        group_margin: f64,
        model_multiplier: f64,
        settings: &BillingSettings,
    ) -> i64 {
        let multipliers = self.margin_multiplier * group_margin * model_multiplier;

        match self.pricing_mode {
            // The estimate does not know how the upstream will split input between
            // uncached, cache-creation and cache-read, so it reserves at the most
            // expensive of the three. Reserving at the uncached price let a version priced
            // only on cache tokens reserve nothing, pass the balance check on an empty
            // card, and then charge without bound.
            PricingMode::CostPlus => {
                let input_price = self
                    .input_price_per_m
                    .max(self.cache_creation_price_per_m)
                    .max(self.cache_read_price_per_m);
                let input_cost = (estimated_input as f64) * input_price / 1_000_000.0;
                let output_cost = (max_output as f64) * self.output_price_per_m / 1_000_000.0;
                let total_curr = input_cost + output_cost;
                let total_cny = match self.currency {
                    Currency::Usd => total_curr * settings.usd_cny_rate,
                    Currency::Cny => total_curr,
                };
                let face_val = if settings.credit_face_value_cny > 0.0 {
                    settings.credit_face_value_cny
                } else {
                    0.01
                };
                let micro_credits =
                    (total_cny * multipliers / face_val) * (MICRO_CREDITS_PER_CREDIT as f64);
                ceil_nonnegative_to_i64(micro_credits)
            }
            PricingMode::Fixed => {
                let input_credit = self
                    .fixed_input_credit_per_m
                    .max(self.fixed_cache_creation_credit_per_m)
                    .max(self.fixed_cache_read_credit_per_m);
                let input_charge = (estimated_input as f64) * (input_credit as f64) / 1_000_000.0;
                let output_charge =
                    (max_output as f64) * (self.fixed_output_credit_per_m as f64) / 1_000_000.0;
                let base = (input_charge + output_charge) * multipliers;
                ceil_nonnegative_to_i64(base)
            }
            PricingMode::PerCall => {
                let charge = (self.per_call_credit as f64) * multipliers;
                ceil_nonnegative_to_i64(charge)
            }
        }
    }
}

/// Aggregated gross margin summary across ledger entries (Spec §6.8, §14.4).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MarginSummary {
    pub total_credits_charged: i64,
    pub total_revenue_micro_cny: i64,
    pub total_provider_cost_micro_cny: i64,
    pub gross_profit_micro_cny: i64,
    pub gross_margin_rate: f64,
}
