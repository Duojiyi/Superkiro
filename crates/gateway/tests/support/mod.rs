//! Shared by the tests that serve conversation turns.

use billing::rate_card::{Currency, PricingMode, RateCardVersion};

/// A wildcard price in `rate_card_id` at the rates a request without a published price
/// used to be held and billed at: 15 credits per million input tokens (cache writes too),
/// 60 per million output tokens, 1.5 per million cache reads. Such a request is refused
/// now, so a test about something other than pricing publishes this.
pub fn wildcard_price(rate_card_id: &str) -> RateCardVersion {
    RateCardVersion {
        id: format!("test-wildcard-{rate_card_id}"),
        rate_card_id: rate_card_id.to_string(),
        model: "*".to_string(),
        currency: Currency::Cny,
        pricing_mode: PricingMode::Fixed,
        input_price_per_m: 0.0,
        output_price_per_m: 0.0,
        cache_creation_price_per_m: 0.0,
        cache_read_price_per_m: 0.0,
        fixed_input_credit_per_m: 15_000_000,
        fixed_output_credit_per_m: 60_000_000,
        fixed_cache_creation_credit_per_m: 15_000_000,
        fixed_cache_read_credit_per_m: 1_500_000,
        per_call_credit: 0,
        margin_multiplier: 1.0,
        effective_from_secs: 0,
    }
}
