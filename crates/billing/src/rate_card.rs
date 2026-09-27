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
use std::collections::BTreeMap;

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
///
/// Unknown fields are ignored, so an older release still loads settings a later one saved.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BillingSettings {
    /// RMB face value of 1 credit (e.g. 0.01 = 0.01 CNY per credit, i.e. 1分钱/积分).
    pub credit_face_value_cny: f64,
    /// USD to CNY exchange rate (e.g. 7.25).
    pub usd_cny_rate: f64,
    /// Unix timestamp when the exchange rate was updated.
    #[serde(default)]
    pub rate_updated_at_secs: u64,
    /// CNY per official US dollar in prices computed from official ones; 1.0 when unset.
    /// Not `usd_cny_rate`, which converts USD cost-plus prices.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub official_usd_cny: Option<f64>,
    /// 计费倍率 offered for a price computed from an official one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_price_multiplier: Option<f64>,
    /// 成本倍率 offered for a provider with none of its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_cost_multiplier: Option<f64>,
    /// 成本倍率 by provider ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_cost_multipliers: Option<BTreeMap<String, f64>>,
    /// Official list prices by model name (an upstream model a route targets, or a customer
    /// model): the basis for route costs and the start for new prices.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub official_prices: Option<BTreeMap<String, OfficialPrice>>,
    /// What a route, `<provider>/<upstream model>`, really bills where it differs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_costs: Option<BTreeMap<String, RouteCost>>,
}

/// An official list price in USD per 1M tokens.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OfficialPrice {
    pub input_usd_per_m: f64,
    pub output_usd_per_m: f64,
    pub cache_creation_usd_per_m: f64,
    pub cache_read_usd_per_m: f64,
    /// Where the price comes from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// When its prices last changed; set by the server.
    #[serde(default)]
    pub updated_at_secs: u64,
}

impl OfficialPrice {
    /// Input, output, cache write and cache read.
    pub fn usd_per_m(&self) -> [f64; 4] {
        [
            self.input_usd_per_m,
            self.output_usd_per_m,
            self.cache_creation_usd_per_m,
            self.cache_read_usd_per_m,
        ]
    }
}

/// What one upstream bills for one model, where that is not its provider's 成本倍率 on the
/// model's official price.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RouteCost {
    /// Instead of the provider's 成本倍率.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_multiplier: Option<f64>,
    /// Input, output, cache write and cache read, when it bills other prices than the official
    /// ones.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub basis_usd_per_m: Option<[f64; 4]>,
}

impl Default for BillingSettings {
    fn default() -> Self {
        Self {
            credit_face_value_cny: 0.01,
            usd_cny_rate: 7.25,
            rate_updated_at_secs: 0,
            official_usd_cny: None,
            default_price_multiplier: None,
            default_cost_multiplier: None,
            provider_cost_multipliers: None,
            official_prices: None,
            route_costs: None,
        }
    }
}

impl BillingSettings {
    /// CNY per official US dollar: ¥1 = $1 unless set.
    pub fn official_usd_cny(&self) -> f64 {
        self.official_usd_cny.unwrap_or(1.0)
    }

    /// Of these settings, what settling a request served by one of `routes` (provider,
    /// upstream model) reads: the face value, the exchange rates, the default 成本倍率, and
    /// each route's own cost, its provider's 成本倍率 and its upstream model's official price,
    /// without its note. With no routes, for a request no mapping serves, which is sent to the
    /// model it names by whichever provider: what any provider serving `model` reads.
    pub fn for_routes(&self, routes: &[(String, String)], model: &str) -> BillingSettings {
        let provider_serves = |provider: &str| {
            routes.is_empty() || routes.iter().any(|(serving, _)| serving == provider)
        };
        let target_serves = |target: &str| {
            if routes.is_empty() {
                target == model
            } else {
                routes.iter().any(|(_, serving)| serving == target)
            }
        };
        let route_serves = |route: &str| {
            if routes.is_empty() {
                route
                    .split_once('/')
                    .is_some_and(|(_, target)| target == model)
            } else {
                routes
                    .iter()
                    .any(|(provider, target)| route == format!("{provider}/{target}"))
            }
        };
        fn kept<V: Clone>(
            map: &Option<BTreeMap<String, V>>,
            keep: impl Fn(&str) -> bool,
            copy: impl Fn(&V) -> V,
        ) -> Option<BTreeMap<String, V>> {
            let kept: BTreeMap<String, V> = map
                .iter()
                .flatten()
                .filter(|(key, _)| keep(key))
                .map(|(key, value)| (key.clone(), copy(value)))
                .collect();
            (!kept.is_empty()).then_some(kept)
        }
        BillingSettings {
            credit_face_value_cny: self.credit_face_value_cny,
            usd_cny_rate: self.usd_cny_rate,
            rate_updated_at_secs: self.rate_updated_at_secs,
            official_usd_cny: self.official_usd_cny,
            // Offered for new prices; no settlement reads it.
            default_price_multiplier: None,
            default_cost_multiplier: self.default_cost_multiplier,
            provider_cost_multipliers: kept(
                &self.provider_cost_multipliers,
                provider_serves,
                |m| *m,
            ),
            official_prices: kept(&self.official_prices, target_serves, |price| {
                OfficialPrice {
                    note: None,
                    ..price.clone()
                }
            }),
            route_costs: kept(&self.route_costs, route_serves, RouteCost::clone),
        }
    }

    /// What `provider` bills for `tokens` of its upstream model `target`, in micro-CNY, from
    /// official prices: the route's own basis, else `target`'s official price, times the
    /// route's 成本倍率, else the provider's, else the default, at `official_usd_cny`. None
    /// when the basis or the multiplier is unknown; the cost then comes from price versions.
    pub fn official_cost_micro_cny(
        &self,
        provider: &str,
        target: &str,
        tokens: &UsageTokens,
    ) -> Option<i64> {
        let route = self
            .route_costs
            .as_ref()
            .and_then(|routes| routes.get(&format!("{provider}/{target}")));
        let basis = route
            .and_then(|route| route.basis_usd_per_m)
            .or_else(|| Some(self.official_prices.as_ref()?.get(target)?.usd_per_m()))?;
        let multiplier = route
            .and_then(|route| route.cost_multiplier)
            .or_else(|| {
                self.provider_cost_multipliers
                    .as_ref()?
                    .get(provider)
                    .copied()
            })
            .or(self.default_cost_multiplier)?;
        // CNY per 1M tokens, as a version computed from the official price records them.
        let [input, output, cache_creation, cache_read] =
            basis.map(|usd| usd * multiplier * self.official_usd_cny());
        let cny = (tokens.uncached_input_tokens as f64) * input / 1_000_000.0
            + (tokens.output_tokens as f64) * output / 1_000_000.0
            + (tokens.cache_creation_tokens as f64) * cache_creation / 1_000_000.0
            + (tokens.cache_read_tokens as f64) * cache_read / 1_000_000.0;
        Some(ceil_nonnegative_to_i64((cny * 1_000_000.0).round()))
    }
}

/// What a version's prices were computed from: official list prices in USD per 1M tokens, the
/// multipliers, and the settings used. Publication checks that it gives the version's credits
/// and costs; settlement still charges the version's own fields.
///
/// Unknown fields are ignored here and on the version, so an older release still loads it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OfficialPricing {
    pub input_usd_per_m: f64,
    pub output_usd_per_m: f64,
    pub cache_creation_usd_per_m: f64,
    pub cache_read_usd_per_m: f64,
    /// 计费倍率: our price is official × this, in CNY at `usd_cny`.
    pub price_multiplier: f64,
    /// 成本倍率: our cost is the cost basis × this, in CNY at `usd_cny`.
    pub cost_multiplier: f64,
    /// Input, output, cache write and cache read, when the upstream bills other prices than
    /// the official ones.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_basis_usd_per_m: Option<[f64; 4]>,
    /// `BillingSettings::official_usd_cny` used.
    pub usd_cny: f64,
    /// `BillingSettings::credit_face_value_cny` used.
    pub credit_face_value_cny: f64,
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

    /// What the fixed credits and CNY costs were computed from, when priced from an official
    /// price.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub official: Option<OfficialPricing>,
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
