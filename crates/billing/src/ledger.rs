//! Append-only usage ledger and settlement calculation (Spec §5, §6.1, §6.7).
//!
//! Authoritative source of charges. Uses third-party provider usage metrics
//! to calculate exact credit charges and records each invocation idempotently.

use serde::{Deserialize, Serialize};

/// Convert a calculated monetary/credit value to a bounded non-negative integer.
/// Configuration is user-controlled, so NaN/Infinity/negative values must never
/// reach an integer cast (Rust saturates those casts in surprising ways).
pub(crate) fn ceil_nonnegative_to_i64(value: f64) -> i64 {
    if !value.is_finite() || value <= 0.0 {
        0
    } else if value >= i64::MAX as f64 {
        i64::MAX
    } else {
        value.ceil() as i64
    }
}

/// Type of ledger transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LedgerKind {
    Usage,
    Adjustment,
    Topup,
}

/// Token usage metrics to be billed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageTokens {
    pub uncached_input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
}

/// No real request comes near this in any one token class; the largest context
/// windows are around a million tokens. Counts above it are an upstream reporting
/// bug, and pricing them unclamped saturated a single charge to `i64::MAX`.
pub const MAX_TOKENS_PER_CLASS: u64 = 10_000_000;

impl UsageTokens {
    /// Every class clamped to [`MAX_TOKENS_PER_CLASS`]. Token counts come from the
    /// upstream and are untrusted.
    pub fn clamped(&self) -> Self {
        Self {
            uncached_input_tokens: self.uncached_input_tokens.min(MAX_TOKENS_PER_CLASS),
            output_tokens: self.output_tokens.min(MAX_TOKENS_PER_CLASS),
            cache_creation_tokens: self.cache_creation_tokens.min(MAX_TOKENS_PER_CLASS),
            cache_read_tokens: self.cache_read_tokens.min(MAX_TOKENS_PER_CLASS),
        }
    }
}

/// Token pricing rates for calculating charges in micro-credits.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PricingRates {
    pub input_rate_per_m: i64,
    pub output_rate_per_m: i64,
    pub cache_creation_rate_per_m: i64,
    pub cache_read_rate_per_m: i64,
    pub credit_multiplier: f64,
    pub margin_multiplier: f64,
}

impl Default for PricingRates {
    fn default() -> Self {
        Self {
            input_rate_per_m: 15_000_000,          // 15 credits per 1M tokens
            output_rate_per_m: 60_000_000,         // 60 credits per 1M tokens
            cache_creation_rate_per_m: 18_750_000, // 1.25x input rate
            cache_read_rate_per_m: 1_500_000,      // 0.1x input rate
            credit_multiplier: 1.0,
            margin_multiplier: 1.0,
        }
    }
}

impl PricingRates {
    /// Calculate exact micro-credits to charge for given token usage.
    pub fn calculate_charge(&self, tokens: &UsageTokens) -> i64 {
        let uncached_cost =
            (tokens.uncached_input_tokens as f64) * (self.input_rate_per_m as f64) / 1_000_000.0;
        let output_cost =
            (tokens.output_tokens as f64) * (self.output_rate_per_m as f64) / 1_000_000.0;
        let cache_create_cost = (tokens.cache_creation_tokens as f64)
            * (self.cache_creation_rate_per_m as f64)
            / 1_000_000.0;
        let cache_read_cost =
            (tokens.cache_read_tokens as f64) * (self.cache_read_rate_per_m as f64) / 1_000_000.0;

        let total_base = uncached_cost + output_cost + cache_create_cost + cache_read_cost;
        let final_credits = total_base * self.credit_multiplier * self.margin_multiplier;
        ceil_nonnegative_to_i64(final_credits)
    }
}

/// Append-only ledger record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LedgerEntry {
    pub id: String,
    pub card_id: String,
    pub kind: LedgerKind,
    pub invocation_id: Option<String>,
    pub exposed_model: String,
    pub provider_id: String,
    pub target_model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
    pub credits_charged: i64,
    pub provider_cost_micro_cny: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate_card_version: Option<String>,
    pub ts_secs: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operator_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}
