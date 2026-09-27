//! Validated atomic publication. No secrets are exposed by this view.
use super::*;
use crate::rate_card::{Currency, OfficialPricing, PricingMode};
use crate::template::{plan_catalog, seed_plans, MAX_PLANS};
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommercialAudit {
    pub previous_revision: String,
    pub revision: String,
    pub reason: String,
    pub operator: String,
    pub created_at_secs: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommercialUpdate {
    pub expected_revision: String,
    pub reason: String,
    #[serde(default)]
    pub settings: Option<BillingSettings>,
    #[serde(default)]
    pub groups: Vec<Group>,
    #[serde(default)]
    pub models: Vec<ModelMap>,
    #[serde(default)]
    pub rate_cards: Vec<RateCard>,
    #[serde(default)]
    pub versions: Vec<RateCardVersion>,
    /// Mappings to delete, by ID; only a hidden or retired one can be.
    #[serde(default)]
    pub removed_models: Vec<String>,
    /// Price versions to withdraw, by ID; only one not yet in force can be.
    #[serde(default)]
    pub cancelled_versions: Vec<String>,
    /// Plans to add or replace, by ID. The first publication that changes plans stores the
    /// catalog, the seed included.
    #[serde(default)]
    pub plans: Vec<Plan>,
    /// Plans to delete, by ID; only one no card was issued from can be. The others are taken
    /// off sale instead.
    #[serde(default)]
    pub removed_plans: Vec<String>,
}
#[derive(Debug, Serialize)]
pub struct CommercialConfig {
    pub revision: String,
    pub settings: BillingSettings,
    pub audit: Vec<CommercialAudit>,
    pub groups: Vec<Group>,
    pub models: Vec<ModelMap>,
    pub rate_cards: Vec<RateCard>,
    pub versions: Vec<RateCardVersion>,
    /// The plan catalog in force, by sort order then ID.
    pub plans: Vec<Plan>,
    /// Cards issued from each plan of the catalog, those issued before it counted under the
    /// tier their credits name. A plan with any cannot be removed, only taken off sale.
    pub cards_by_plan: BTreeMap<String, u64>,
}
fn view(s: &BillingSnapshot) -> CommercialConfig {
    let mut groups: Vec<_> = s.groups.values().cloned().collect();
    groups.sort_by(|a, b| a.id.cmp(&b.id));
    let mut models = s.model_maps.clone();
    models.sort_by(|a, b| a.id.cmp(&b.id));
    let mut rate_cards: Vec<_> = s.rate_cards.values().cloned().collect();
    rate_cards.sort_by(|a, b| a.id.cmp(&b.id));
    let mut versions = s.rate_card_versions.clone();
    versions.sort_by(|a, b| a.id.cmp(&b.id));
    // Until plans are stored, the revision is what it was before the catalog.
    let bytes = match &s.plans {
        None => serde_json::to_vec(&(&groups, &models, &rate_cards, &versions, &s.settings)),
        Some(plans) => {
            serde_json::to_vec(&(&groups, &models, &rate_cards, &versions, &s.settings, plans))
        }
    }
    .unwrap();
    let revision = ring::digest::digest(&ring::digest::SHA256, &bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let plans = plan_catalog(s.plans.as_deref(), &s.groups);
    let mut cards_by_plan: BTreeMap<String, u64> =
        plans.iter().map(|plan| (plan.id.clone(), 0)).collect();
    for card in s.cards.values() {
        if let Some(count) = card.plan_id().and_then(|id| cards_by_plan.get_mut(id)) {
            *count += 1;
        }
    }
    CommercialConfig {
        revision,
        settings: s.settings.clone(),
        audit: s.commercial_audit_logs.clone(),
        groups,
        models,
        rate_cards,
        versions,
        plans,
        cards_by_plan,
    }
}
fn text(s: &str, max: usize) -> bool {
    crate::valid_id(s, max)
}

/// The most providers the settings give a 成本倍率 of their own.
pub const MAX_PROVIDER_MULTIPLIERS: usize = 200;
/// The most official prices the settings hold.
pub const MAX_OFFICIAL_PRICES: usize = 200;
/// The most routes the settings give a cost of their own.
pub const MAX_ROUTE_COSTS: usize = 200;
/// The largest publication body the admin API reads. The largest settings the counts above
/// allow, every character of every name and note escaped, take under half of it, leaving
/// the rest for the groups, models and prices a publication carries.
pub const MAX_COMMERCIAL_UPDATE_BYTES: usize = 1024 * 1024;
fn positive(n: f64) -> bool {
    n.is_finite() && n > 0.0 && n <= 1000.0
}
/// 计费倍率 and 成本倍率.
fn multiplier(n: f64) -> bool {
    n.is_finite() && n > 0.0 && n <= 100.0
}
/// Why a version's official block does not describe it. Its credits are official × 计费倍率 ×
/// CNY per official dollar ÷ face value, computed in this order in f64 as the console does, to
/// within one micro-credit; a route's cost charges nothing, so its credits are 0. Its costs are
/// the cost basis × 成本倍率 × CNY per official dollar, in CNY.
fn official_mismatch(
    v: &RateCardVersion,
    o: &OfficialPricing,
    route_cost: bool,
) -> Option<&'static str> {
    let official = [
        o.input_usd_per_m,
        o.output_usd_per_m,
        o.cache_creation_usd_per_m,
        o.cache_read_usd_per_m,
    ];
    let basis = o.cost_basis_usd_per_m.unwrap_or(official);
    if v.pricing_mode != PricingMode::Fixed
        || v.currency != Currency::Cny
        || v.margin_multiplier != 1.0
        || v.per_call_credit != 0
        || official
            .iter()
            .chain(&basis)
            .any(|usd| !(0.0..=10_000.0).contains(usd))
        || !official.iter().any(|usd| *usd > 0.0)
        || !multiplier(o.price_multiplier)
        || !multiplier(o.cost_multiplier)
        || !positive(o.usd_cny)
        || !positive(o.credit_face_value_cny)
    {
        return Some("Invalid official pricing");
    }
    let credits = [
        v.fixed_input_credit_per_m,
        v.fixed_output_credit_per_m,
        v.fixed_cache_creation_credit_per_m,
        v.fixed_cache_read_credit_per_m,
    ];
    if official.iter().zip(credits).any(|(usd, fixed)| {
        if route_cost {
            return fixed != 0;
        }
        let expected =
            (usd * o.price_multiplier * o.usd_cny / o.credit_face_value_cny * 1_000_000.0).round();
        (fixed as f64 - expected).abs() > 1.0
    }) {
        return Some("Official pricing does not match the credits of");
    }
    let costs = [
        v.input_price_per_m,
        v.output_price_per_m,
        v.cache_creation_price_per_m,
        v.cache_read_price_per_m,
    ];
    if basis.iter().zip(costs).any(|(usd, cost)| {
        let expected = usd * o.cost_multiplier * o.usd_cny;
        (cost - expected).abs() > (1e-9 * cost.abs().max(expected.abs())).max(1e-12)
    }) {
        return Some("Official pricing does not match the cost of");
    }
    None
}
/// Version margin x group margin x model credit multiplier, as met by one request. Each is
/// bounded on its own, but together they reached 10^9, which saturates the reservation and
/// takes the model offline.
const MAX_COMBINED_MULTIPLIER: f64 = 100.0;
/// The charge divides by the face value, so a mistyped one does the same.
const MIN_CREDIT_FACE_VALUE_CNY: f64 = 0.0001;
fn invalid(s: &str) -> BillingError {
    BillingError::InvalidState(s.into())
}
fn invalid_ids(what: &str, ids: &[&str]) -> BillingError {
    BillingError::InvalidState(format!("{what}: {}", ids.join(", ")))
}
impl BillingEngine {
    pub fn commercial_config(&self) -> CommercialConfig {
        view(&self.export_snapshot())
    }
    pub fn publish_commercial_config(
        &self,
        u: CommercialUpdate,
        now: u64,
    ) -> Result<CommercialConfig, BillingError> {
        if !text(&u.reason, 500) {
            return Err(invalid("Publication reason required (max 500 bytes)"));
        }
        if u.groups.len()
            + u.models.len()
            + u.rate_cards.len()
            + u.versions.len()
            + u.removed_models.len()
            + u.cancelled_versions.len()
            + u.plans.len()
            + u.removed_plans.len()
            == 0
            && u.settings.is_none()
        {
            return Err(invalid("Empty publication"));
        }
        let _guard = self.state_lock.write().unwrap();
        let mut c = self.export_snapshot_locked(
            self.snapshot_sequence
                .load(Ordering::Acquire)
                .saturating_add(1),
            self.last_snapshot_checksum.read().unwrap().clone(),
        );
        if view(&c).revision != u.expected_revision {
            return Err(invalid("Configuration changed; reload before publishing"));
        }
        // A request in flight settles at the price version, margins and settings it captured
        // when it was reserved, and a pending settlement is already priced, so a publication
        // reprices neither and need not wait for idle. Only a hold that captured nothing (it
        // named no model) would settle at whatever is current when it settles.
        if c.reservations.iter().any(|(id, r)| {
            r.state == ReservationState::Held
                && r.pricing.is_none()
                && !c.pending_settlements.contains_key(id)
        }) {
            return Err(invalid("Requests are still settling; publish when idle"));
        }
        let mut repriced = false;
        if let Some(mut settings) = u.settings {
            if !positive(settings.credit_face_value_cny)
                || settings.credit_face_value_cny < MIN_CREDIT_FACE_VALUE_CNY
                || !positive(settings.usd_cny_rate)
            {
                return Err(invalid(
                    "Face value must be 0.0001-1000 and the exchange rate positive and at most 1000",
                ));
            }
            // Left out, these keep their value: a console that does not know them never
            // wipes them.
            let current = &c.settings;
            settings.official_usd_cny = settings.official_usd_cny.or(current.official_usd_cny);
            settings.default_price_multiplier = settings
                .default_price_multiplier
                .or(current.default_price_multiplier);
            settings.default_cost_multiplier = settings
                .default_cost_multiplier
                .or(current.default_cost_multiplier);
            if settings.provider_cost_multipliers.is_none() {
                settings.provider_cost_multipliers = current.provider_cost_multipliers.clone();
            }
            if settings.official_prices.is_none() {
                settings.official_prices = current.official_prices.clone();
            }
            if settings.route_costs.is_none() {
                settings.route_costs = current.route_costs.clone();
            }
            if settings
                .official_usd_cny
                .is_some_and(|rate| !positive(rate))
            {
                return Err(invalid(
                    "The official dollar rate must be positive and at most 1000",
                ));
            }
            if settings
                .default_price_multiplier
                .into_iter()
                .chain(settings.default_cost_multiplier)
                .any(|m| !multiplier(m))
                || settings
                    .provider_cost_multipliers
                    .as_ref()
                    .is_some_and(|by_provider| {
                        by_provider.len() > MAX_PROVIDER_MULTIPLIERS
                            || by_provider
                                .iter()
                                .any(|(id, m)| !text(id, 128) || !multiplier(*m))
                    })
            {
                return Err(invalid(&format!(
                    "Multipliers must be positive and at most 100, for at most {MAX_PROVIDER_MULTIPLIERS} providers"
                )));
            }
            let usd = |prices: &[f64]| prices.iter().all(|p| (0.0..=10_000.0).contains(p));
            if settings.official_prices.as_ref().is_some_and(|prices| {
                prices.len() > MAX_OFFICIAL_PRICES
                    || prices.iter().any(|(name, price)| {
                        !text(name, 256)
                            || !usd(&price.usd_per_m())
                            || price.note.as_deref().is_some_and(|note| {
                                note.len() > 256 || note.chars().any(char::is_control)
                            })
                    })
            }) {
                return Err(invalid(&format!(
                    "Official prices: at most {MAX_OFFICIAL_PRICES}, named in 1-256 bytes, priced 0-10000, notes of at most 256 bytes"
                )));
            }
            if settings.route_costs.as_ref().is_some_and(|routes| {
                routes.len() > MAX_ROUTE_COSTS
                    || routes.iter().any(|(route, cost)| {
                        !text(route, 256)
                            || !route.split_once('/').is_some_and(|(provider, model)| {
                                !provider.is_empty() && !model.is_empty()
                            })
                            || cost.cost_multiplier.is_some_and(|m| !multiplier(m))
                            || cost.basis_usd_per_m.is_some_and(|basis| !usd(&basis))
                    })
            }) {
                return Err(invalid(&format!(
                    "Route costs: at most {MAX_ROUTE_COSTS}, named <provider>/<upstream model> in at most 256 bytes, multipliers positive and at most 100, prices 0-10000"
                )));
            }
            // An official price's time is when its prices last changed.
            for (name, price) in settings.official_prices.iter_mut().flatten() {
                price.updated_at_secs = match current
                    .official_prices
                    .as_ref()
                    .and_then(|old| old.get(name))
                {
                    Some(old) if old.usd_per_m() == price.usd_per_m() => old.updated_at_secs,
                    _ => now,
                };
            }
            // Either one reprices every price computed from an official one (see below).
            repriced = settings.credit_face_value_cny != current.credit_face_value_cny
                || settings.official_usd_cny() != current.official_usd_cny();
            settings.rate_updated_at_secs = now;
            c.settings = settings;
        }
        let mut ids = std::collections::HashSet::new();
        for r in u.rate_cards {
            if !text(&r.id, 128) || !text(&r.name, 256) || !ids.insert(("rate", r.id.clone())) {
                return Err(invalid("Invalid or duplicate rate card"));
            }
            c.rate_cards.insert(r.id.clone(), r);
        }
        for g in u.groups {
            if !text(&g.id, crate::MAX_GROUP_ID_BYTES)
                || !text(&g.name, 256)
                || !text(&g.virtual_plan_name, 256)
                || !positive(g.margin_multiplier)
                || !g.virtual_usage_limit.is_finite()
                || g.virtual_usage_limit < 0.0
                || g.system_prompt_prefix
                    .as_ref()
                    .is_some_and(|s| s.len() > 16000)
                || !c.rate_cards.contains_key(&g.rate_card_id)
                || !ids.insert(("group", g.id.clone()))
            {
                return Err(invalid("Invalid group or unknown rate card"));
            }
            c.groups.insert(g.id.clone(), g);
        }
        // The catalog is stored by the first publication that changes a plan, the seed with
        // it; only the plans a publication lists are checked, against its groups.
        if !u.plans.is_empty() || !u.removed_plans.is_empty() {
            let mut plans = c.plans.take().unwrap_or_else(|| seed_plans(&c.groups));
            let mut listed = std::collections::HashSet::new();
            for plan in u.plans {
                if let Some(problem) = plan.problem() {
                    return Err(invalid_ids(problem, &[plan.id.as_str()]));
                }
                if !listed.insert(plan.id.clone()) {
                    return Err(invalid_ids("Duplicate plan", &[plan.id.as_str()]));
                }
                if !c.groups.contains_key(&plan.default_group_id) {
                    return Err(invalid_ids(
                        "Unknown default group of plan",
                        &[plan.id.as_str()],
                    ));
                }
                match plans.iter_mut().find(|old| old.id == plan.id) {
                    Some(old) => *old = plan,
                    None => plans.push(plan),
                }
            }
            // A card keeps the plan it was issued from; the catalog keeps it too.
            for id in &u.removed_plans {
                let Some(index) = plans.iter().position(|plan| plan.id == *id) else {
                    return Err(invalid_ids("Unknown plan", &[id.as_str()]));
                };
                if c.cards
                    .values()
                    .any(|card| card.plan_id() == Some(id.as_str()))
                {
                    return Err(invalid_ids(
                        "Plans cards were issued from can only be taken off sale",
                        &[id.as_str()],
                    ));
                }
                plans.remove(index);
            }
            if plans.len() > MAX_PLANS {
                return Err(invalid("At most 100 plans"));
            }
            plans.sort_by(|a, b| a.id.cmp(&b.id));
            c.plans = Some(plans);
        }
        // Only the mappings this publication lists are held to the model ID rule requests
        // meet, and to being servable: one published before either rule still loads.
        let mut published = std::collections::HashSet::new();
        for m in u.models {
            if let Some(id) = std::iter::once(&m.exposed_model_id)
                .chain(&m.aliases)
                .find(|id| !crate::group::valid_model_id(id))
            {
                return Err(invalid_ids("Invalid model ID", &[id.as_str()]));
            }
            if !text(&m.id, 128)
                || !text(&m.target_model, 256)
                || !positive(m.credit_multiplier)
                || m.max_output == 0
                || m.max_output > m.context_window
                || m.context_window > 10_000_000
                || m.aliases.len() > 32
                || m.fallback_chain.len() > 8
                || m.display_name
                    .as_deref()
                    .is_some_and(|name| !text(name, 64))
                || m.description
                    .as_deref()
                    .is_some_and(|text_| !text(text_, 256))
                || m.rate_multiplier.is_some_and(|rate| !positive(rate))
                || !ids.insert(("map", m.id.clone()))
            {
                return Err(invalid("Invalid or duplicate model mapping"));
            }
            published.insert(m.id.clone());
            if let Some(old) = c.model_maps.iter_mut().find(|v| v.id == m.id) {
                if old.group_id != m.group_id {
                    return Err(invalid("Cannot move mapping between groups"));
                }
                *old = m;
            } else {
                c.model_maps.push(m);
            }
        }
        // A model customers can see is withdrawn first, by hiding or retiring it.
        for id in &u.removed_models {
            match c.model_maps.iter().position(|m| m.id == *id) {
                Some(i) if !c.model_maps[i].is_listed() => {
                    c.model_maps.remove(i);
                }
                _ => {
                    return Err(invalid_ids(
                        "Only hidden or retired mappings can be removed",
                        &[id.as_str()],
                    ))
                }
            }
        }
        let mut exposed = std::collections::HashSet::new();
        let mut unroutable = Vec::new();
        for m in &c.model_maps {
            let g = c
                .groups
                .get(&m.group_id)
                .ok_or_else(|| invalid("Unknown model group"))?;
            for (id, model) in std::iter::once((&m.target_provider_id, &m.target_model)).chain(
                m.fallback_chain
                    .iter()
                    .map(|f| (&f.provider_id, &f.target_model)),
            ) {
                let p = c
                    .providers
                    .get(id)
                    .ok_or_else(|| invalid("Unknown target provider"))?;
                if !text(model, 256) || !g.can_access_provider(p.group_id.as_deref()) {
                    return Err(invalid("Provider not accessible to model group"));
                }
            }
            // A model this publication lists for customers must be servable by its primary
            // target. One published earlier that has since lost its provider or Key no
            // longer blocks every later publication, and a fallback may be disabled:
            // requests pass over it.
            if published.contains(&m.id)
                && m.is_listed()
                && !(c
                    .providers
                    .get(&m.target_provider_id)
                    .is_some_and(|p| p.enabled)
                    && c.provider_keys.values().any(|k| {
                        k.provider_id == m.target_provider_id
                            && k.enabled
                            && k.supports_model(&m.target_model)
                    }))
                && !unroutable.contains(&m.exposed_model_id.as_str())
            {
                unroutable.push(m.exposed_model_id.as_str());
            }
            for name in std::iter::once(&m.exposed_model_id).chain(m.aliases.iter()) {
                if !exposed.insert((&m.group_id, name)) {
                    return Err(invalid("Ambiguous model ID or alias"));
                }
            }
        }
        if !unroutable.is_empty() {
            return Err(invalid_ids(
                "Visible model target has no enabled compatible key",
                &unroutable,
            ));
        }
        // A price that is or was in force may have priced a request; only a scheduled one
        // is withdrawn, before this publication's prices, which may replace it.
        for id in &u.cancelled_versions {
            match c.rate_card_versions.iter().position(|v| v.id == *id) {
                Some(i) if c.rate_card_versions[i].effective_from_secs > now => {
                    c.rate_card_versions.remove(i);
                }
                _ => {
                    return Err(invalid_ids(
                        "Only scheduled prices can be cancelled",
                        &[id.as_str()],
                    ))
                }
            }
        }
        // The models that already have a price, before this publication adds any.
        let priced: std::collections::HashSet<(String, String)> = c
            .rate_card_versions
            .iter()
            .map(|v| (v.rate_card_id.clone(), v.model.clone()))
            .collect();
        let mut new_versions = std::collections::HashSet::new();
        for mut v in u.versions {
            let prices = [
                v.input_price_per_m,
                v.output_price_per_m,
                v.cache_creation_price_per_m,
                v.cache_read_price_per_m,
            ];
            let fixed = [
                v.fixed_input_credit_per_m,
                v.fixed_output_credit_per_m,
                v.fixed_cache_creation_credit_per_m,
                v.fixed_cache_read_credit_per_m,
                v.per_call_credit,
            ];
            if !text(&v.id, 128)
                || !text(&v.model, 256)
                || !positive(v.margin_multiplier)
                || prices
                    .iter()
                    .any(|p| !p.is_finite() || *p < 0.0 || *p > 1_000_000.0)
                // A million credits per million tokens (or per call). The old ceiling of
                // 10^18 priced a single request past what a balance can hold.
                || fixed.iter().any(|p| *p < 0 || *p > 1_000_000_000_000)
                || !c.rate_cards.contains_key(&v.rate_card_id)
            {
                return Err(invalid(
                    "Invalid pricing; retroactive publication forbidden",
                ));
            }
            if let Some(o) = &v.official {
                // `<provider>/<upstream model>`: what that route costs, charging nothing.
                let route_cost = c.providers.keys().any(|id| {
                    v.model
                        .strip_prefix(id.as_str())
                        .is_some_and(|rest| rest.starts_with('/'))
                });
                if let Some(problem) = official_mismatch(&v, o, route_cost) {
                    return Err(invalid_ids(problem, &[v.model.as_str()]));
                }
            }
            if v.effective_from_secs < now {
                // A model's first price may start now, whatever time it names: it replaces
                // no price of its own, and a request in flight keeps the price it was
                // reserved at. So may a price computed from an official one when this
                // publication changes what it is computed at. Any other later price is
                // scheduled, never back-dated.
                if priced.contains(&(v.rate_card_id.clone(), v.model.clone()))
                    && !(repriced && v.official.is_some())
                {
                    return Err(invalid(
                        "Invalid pricing; retroactive publication forbidden",
                    ));
                }
                v.effective_from_secs = now;
            }
            if c.rate_card_versions.iter().any(|x| {
                x.id == v.id
                    || (x.rate_card_id == v.rate_card_id
                        && x.model == v.model
                        && x.effective_from_secs == v.effective_from_secs)
            }) {
                return Err(invalid(
                    "Published prices immutable; use new ID and timestamp",
                ));
            }
            c.rate_card_audit_logs.push(RateCardAuditLog {
                id: format!("publish-{}", v.id),
                rate_card_id: v.rate_card_id.clone(),
                version_id: v.id.clone(),
                operator_id: "admin-session".into(),
                reason: u.reason.clone(),
                created_at_secs: now,
                previous_version_id: self
                    .resolve_rate_card_version(&v.rate_card_id, &v.model, now)
                    .map(|x| x.id),
            });
            new_versions.insert(v.id.clone());
            c.rate_card_versions.push(v);
        }
        // In force now, or scheduled: a version no later one in force has superseded.
        let live = |v: &RateCardVersion| {
            v.effective_from_secs > now
                || !c.rate_card_versions.iter().any(|w| {
                    w.rate_card_id == v.rate_card_id
                        && w.model == v.model
                        && w.effective_from_secs > v.effective_from_secs
                        && w.effective_from_secs <= now
                })
        };
        // A price computed from an official one is at the face value and official dollar rate
        // in force: one this publication makes, and after it changes either, every one in
        // force or scheduled, which the console reprices or withdraws. Other prices keep their
        // credits, and an older stale one blocks no unrelated publication.
        let mut stale = Vec::new();
        for v in c.rate_card_versions.iter().filter(|v| live(v)) {
            if v.official.as_ref().is_some_and(|o| {
                (repriced || new_versions.contains(&v.id))
                    && (o.credit_face_value_cny != c.settings.credit_face_value_cny
                        || o.usd_cny != c.settings.official_usd_cny())
            }) && !stale.contains(&v.model.as_str())
            {
                stale.push(v.model.as_str());
            }
        }
        if !stale.is_empty() {
            return Err(invalid_ids(
                "Official pricing is at a stale face value or rate",
                &stale,
            ));
        }
        // Superseded versions price no new request (one in flight settles at what it was
        // admitted with, which passed this bound when published), and they are immutable, so
        // counting them would block every later publication.
        let live_margin = c
            .rate_card_versions
            .iter()
            .filter(|v| live(v))
            .map(|v| v.margin_multiplier)
            .fold(1.0, f64::max);
        let group_margin = c
            .groups
            .values()
            .map(|g| g.margin_multiplier)
            .fold(1.0, f64::max);
        let model_multiplier = c
            .model_maps
            .iter()
            .map(|m| m.credit_multiplier)
            .fold(1.0, f64::max);
        if live_margin * group_margin * model_multiplier > MAX_COMBINED_MULTIPLIER {
            return Err(invalid(
                "Margins and model multipliers combine to more than 100x",
            ));
        }
        let revision = view(&c).revision;
        c.commercial_audit_logs.push(CommercialAudit {
            previous_revision: u.expected_revision,
            revision,
            reason: u.reason,
            operator: "admin-session".into(),
            created_at_secs: now,
        });
        let result = view(&c);
        self.commit_candidate_snapshot(&c, || {
            *self.settings.write().unwrap() = c.settings.clone();
            *self.commercial_audit_logs.write().unwrap() = c.commercial_audit_logs.clone();
            *self.groups.write().unwrap() = c.groups.clone();
            *self.plans.write().unwrap() = c.plans.clone();
            *self.model_maps.write().unwrap() = c.model_maps.clone();
            *self.rate_cards.write().unwrap() = c.rate_cards.clone();
            *self.rate_card_versions.write().unwrap() = c.rate_card_versions.clone();
            *self.rate_card_audit_logs.write().unwrap() = c.rate_card_audit_logs.clone();
        })?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rate_card::{OfficialPrice, RouteCost};
    fn update(e: &BillingEngine) -> CommercialUpdate {
        CommercialUpdate {
            expected_revision: e.commercial_config().revision,
            reason: "reviewed publication".into(),
            settings: None,
            groups: vec![Group::pro_plus("new-tier", "New tier")],
            models: vec![],
            rate_cards: vec![RateCard::new("default", "Default", 100)],
            versions: vec![],
            removed_models: vec![],
            cancelled_versions: vec![],
            plans: vec![],
            removed_plans: vec![],
        }
    }
    #[test]
    fn publication_is_revision_checked_and_audited() {
        let e = BillingEngine::new();
        let u = update(&e);
        let stale = u.clone();
        let before = u.expected_revision.clone();
        let result = e.publish_commercial_config(u, 100).unwrap();
        assert_ne!(before, result.revision);
        assert_eq!(result.audit.len(), 1);
        assert_eq!(result.audit[0].previous_revision, before);
        assert!(result.groups.iter().any(|g| g.id == "new-tier"));
        assert!(e.publish_commercial_config(stale, 101).is_err());
        assert_eq!(e.commercial_config().audit.len(), 1);
        let restored = BillingEngine::new();
        restored.import_snapshot(e.export_snapshot());
        assert_eq!(restored.commercial_config().revision, result.revision);
        assert_eq!(restored.commercial_config().audit.len(), 1);
    }
    #[test]
    fn invalid_or_inflight_publication_changes_nothing() {
        let e = BillingEngine::new();
        let before = e.commercial_config().revision;
        let mut u = update(&e);
        u.groups[0].margin_multiplier = f64::NAN;
        assert!(e.publish_commercial_config(u, 100).is_err());
        assert_eq!(e.commercial_config().revision, before);
        e.reservations.write().unwrap().insert(
            "held".into(),
            CreditReservation::new("held", "card", "call", 1, 100, 60),
        );
        assert!(e.publish_commercial_config(update(&e), 100).is_err());
        assert_eq!(e.commercial_config().revision, before);
        assert!(e.commercial_config().audit.is_empty());
    }
    /// A request in flight settles at the price version, margins and settings it captured
    /// when it was reserved, so a publication never reprices it and need not wait for
    /// idle: while any customer kept a stream open, a mispriced model could not be fixed.
    #[test]
    fn publication_in_flight_never_reprices_a_held_request() {
        use crate::card::Card;
        use crate::provider::{Provider, ProviderFormat};
        use crate::reservation::ReservationEstimateParams;
        let credit = crate::MICRO_CREDITS_PER_CREDIT;
        let e = BillingEngine::new();
        e.upsert_provider(Provider::new(
            "shared",
            "Shared",
            ProviderFormat::Anthropic,
            "https://upstream.invalid",
        ));
        e.upsert_provider_key(ProviderKey::new("shared-key", "shared", "test"));
        let price = |id: &str, cny_per_m_output: f64, from: u64| -> RateCardVersion {
            serde_json::from_value(serde_json::json!({
                "id": id, "rate_card_id": "default", "model": "model-a",
                "currency": "CNY", "pricing_mode": "cost_plus",
                "input_price_per_m": 0.0, "output_price_per_m": cny_per_m_output,
                "cache_creation_price_per_m": 0.0, "cache_read_price_per_m": 0.0,
                "fixed_input_credit_per_m": 0, "fixed_output_credit_per_m": 0,
                "fixed_cache_creation_credit_per_m": 0, "fixed_cache_read_credit_per_m": 0,
                "per_call_credit": 0, "margin_multiplier": 1.0, "effective_from_secs": from
            }))
            .unwrap()
        };
        let model = |multiplier: f64| {
            let mut m = ModelMap::new("map-a", "new-tier", "model-a", "shared", "target-a");
            m.credit_multiplier = multiplier;
            m
        };
        let mut u = update(&e);
        u.models.push(model(1.0));
        u.versions.push(price("v1", 0.1, 100));
        e.publish_commercial_config(u, 100).unwrap();
        let mut card = Card::new("card", "new-tier", 1_000 * credit);
        card.activate(100, 86_400).unwrap();
        e.upsert_card(card);
        let params = ReservationEstimateParams::new(0, 1_000_000).with_model("model-a");
        e.reserve("card", "held", &params, 101, 600).unwrap();

        // The price, the group margin, the model multiplier and the face value all change
        // while "held" is in flight.
        let mut u = update(&e);
        u.groups[0].margin_multiplier = 2.0;
        u.models.push(model(4.0));
        u.versions.push(price("v2", 2.0, 102));
        u.settings = Some(BillingSettings {
            credit_face_value_cny: 0.5,
            ..e.get_settings()
        });
        e.publish_commercial_config(u, 102).unwrap();

        let tokens = UsageTokens {
            output_tokens: 1_000_000,
            ..UsageTokens::default()
        };
        // 0.1 CNY at 0.01 CNY a credit, as when it was reserved.
        let held = e
            .settle("held", &tokens, "model-a", "shared", "target-a", 103)
            .unwrap();
        assert_eq!(held.rate_card_version.as_deref(), Some("v1"));
        assert_eq!(held.credits_charged, 10 * credit);
        // 2 CNY at 0.5 CNY a credit, times 2 and 4.
        e.reserve("card", "after", &params, 103, 600).unwrap();
        let after = e
            .settle("after", &tokens, "model-a", "shared", "target-a", 104)
            .unwrap();
        assert_eq!(after.rate_card_version.as_deref(), Some("v2"));
        assert_eq!(after.credits_charged, 32 * credit);
    }
    #[test]
    fn stacked_multipliers_and_face_value_are_bounded() {
        let e = BillingEngine::new();
        let before = e.commercial_config().revision;
        let mut u = update(&e);
        // Within its own bound of 1000, but 150x once combined.
        u.groups[0].margin_multiplier = 150.0;
        assert!(e.publish_commercial_config(u, 100).is_err());
        let mut u = update(&e);
        u.settings = Some(BillingSettings {
            credit_face_value_cny: 0.00001,
            ..e.export_snapshot().settings
        });
        assert!(e.publish_commercial_config(u, 100).is_err());
        assert_eq!(e.commercial_config().revision, before);
        let mut u = update(&e);
        u.groups[0].margin_multiplier = 100.0;
        assert!(e.publish_commercial_config(u, 100).is_ok());
    }
    #[test]
    fn model_list_display_fields_are_bounded() {
        use crate::provider::{Provider, ProviderFormat};
        let e = BillingEngine::new();
        e.upsert_provider(Provider::new(
            "shared",
            "Shared",
            ProviderFormat::Anthropic,
            "https://upstream.invalid",
        ));
        e.upsert_provider_key(ProviderKey::new("shared-key", "shared", "test"));
        let with = |edit: &dyn Fn(&mut ModelMap)| {
            let mut u = update(&e);
            let mut m = ModelMap::new("map-1", "new-tier", "model-a", "shared", "target-a");
            edit(&mut m);
            u.models.push(m);
            u
        };
        for bad in [
            with(&|m| m.display_name = Some("x".repeat(65))),
            with(&|m| m.display_name = Some(" ".into())),
            with(&|m| m.description = Some("line\nbreak".into())),
            with(&|m| m.rate_multiplier = Some(0.0)),
            with(&|m| m.rate_multiplier = Some(f64::NAN)),
            with(&|m| m.rate_multiplier = Some(1001.0)),
        ] {
            assert!(e.publish_commercial_config(bad, 100).is_err());
        }
        let published = e
            .publish_commercial_config(
                with(&|m| {
                    m.display_name = Some("Model A".into());
                    m.description = Some("Balanced reasoning and coding".into());
                    m.rate_multiplier = Some(1.3);
                }),
                100,
            )
            .unwrap();
        assert_eq!(published.models[0].rate_multiplier, Some(1.3));
        assert_eq!(published.models[0].display_name.as_deref(), Some("Model A"));
    }
    #[test]
    fn model_aliases_and_provider_isolation_are_enforced() {
        use crate::provider::{Provider, ProviderFormat};
        let e = BillingEngine::new();
        e.upsert_provider(Provider::new(
            "shared",
            "Shared",
            ProviderFormat::Anthropic,
            "https://upstream.invalid",
        ));
        e.upsert_provider(
            Provider::new(
                "private",
                "Private",
                ProviderFormat::Anthropic,
                "https://upstream.invalid",
            )
            .with_group("other-tenant"),
        );
        e.upsert_provider_key(ProviderKey::new("shared-key", "shared", "test"));
        let mut u = update(&e);
        u.models.push(ModelMap::new(
            "map-1", "new-tier", "model-a", "shared", "target-a",
        ));
        let published = e.publish_commercial_config(u, 100).unwrap();
        assert_eq!(published.models[0].target_model, "target-a");
        let mut ambiguous = update(&e);
        ambiguous.models.push(
            ModelMap::new("map-2", "new-tier", "model-b", "shared", "target-b")
                .with_alias("model-a"),
        );
        assert!(e.publish_commercial_config(ambiguous, 101).is_err());
        let mut forbidden = update(&e);
        forbidden.models.push(ModelMap::new(
            "map-2", "new-tier", "model-b", "private", "target-b",
        ));
        assert!(e.publish_commercial_config(forbidden, 101).is_err());
        let mut fallback = update(&e);
        fallback.models.push(
            ModelMap::new("map-2", "new-tier", "model-b", "shared", "target-b")
                .with_fallback("private", "target-c"),
        );
        assert!(e.publish_commercial_config(fallback, 101).is_err());
        assert_eq!(e.commercial_config().revision, published.revision);
    }
    #[test]
    fn visible_publication_requires_a_compatible_enabled_key() {
        let e = BillingEngine::new();
        e.upsert_provider(Provider::new(
            "p",
            "P",
            crate::provider::ProviderFormat::OpenAi,
            "https://example.com",
        ));
        let mut k = ProviderKey::new("k", "p", "test");
        k.allowed_models = Some(vec!["sonnet".into()]);
        e.upsert_provider_key(k.clone());
        let mut u = update(&e);
        u.models
            .push(ModelMap::new("m", "new-tier", "opus", "p", "opus"));
        assert!(e.publish_commercial_config(u.clone(), 100).is_err());
        k.allowed_models = Some(vec!["opus".into()]);
        k.enabled = false;
        e.upsert_provider_key(k.clone());
        assert!(e.publish_commercial_config(u.clone(), 100).is_err());
        k.enabled = true;
        e.upsert_provider_key(k);
        assert!(e.publish_commercial_config(u, 100).is_ok());
    }

    #[test]
    fn published_prices_are_immutable_and_never_retroactive() {
        let e = BillingEngine::new();
        let mut u = update(&e);
        let version: RateCardVersion = serde_json::from_value(serde_json::json!({
            "id":"price-1", "rate_card_id":"default", "model":"*",
            "currency":"CNY", "pricing_mode":"per_call",
            "input_price_per_m":0.0,"output_price_per_m":0.0,
            "cache_creation_price_per_m":0.0,"cache_read_price_per_m":0.0,
            "fixed_input_credit_per_m":0,"fixed_output_credit_per_m":0,
            "fixed_cache_creation_credit_per_m":0,"fixed_cache_read_credit_per_m":0,
            "per_call_credit":1000,"margin_multiplier":1.0,"effective_from_secs":110
        }))
        .unwrap();
        u.versions.push(version.clone());
        let result = e.publish_commercial_config(u, 100).unwrap();
        let mut duplicate = update(&e);
        duplicate.versions.push(version.clone());
        assert!(e.publish_commercial_config(duplicate, 101).is_err());
        let mut retroactive = update(&e);
        let mut old = version;
        old.id = "price-2".into();
        old.effective_from_secs = 99;
        retroactive.versions.push(old);
        assert!(e.publish_commercial_config(retroactive, 101).is_err());
        assert_eq!(e.commercial_config().revision, result.revision);
        assert_eq!(e.commercial_config().audit.len(), 1);
    }
    #[test]
    fn persistence_failure_does_not_publish() {
        let e = BillingEngine::new();
        let blocker = std::env::temp_dir().join(format!(
            "kiro-commercial-blocker-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&blocker, "blocked").unwrap();
        *e.persistence_path.write().unwrap() = Some(blocker.join("state.json"));
        let before = e.commercial_config().revision;
        let result = e.publish_commercial_config(update(&e), 100);
        std::fs::remove_file(&blocker).unwrap();
        assert!(matches!(result, Err(BillingError::Persistence(_))));
        assert_eq!(e.commercial_config().revision, before);
        assert!(e.commercial_config().audit.is_empty());
    }

    /// An engine with one tier and a provider "p" whose one Key may call anything.
    fn serving_engine() -> BillingEngine {
        let e = BillingEngine::new();
        e.upsert_provider(Provider::new(
            "p",
            "P",
            crate::provider::ProviderFormat::OpenAi,
            "https://example.com",
        ));
        e.upsert_provider_key(ProviderKey::new("k", "p", "test"));
        e.publish_commercial_config(update(&e), 100).unwrap();
        e
    }
    fn publish(
        e: &BillingEngine,
        u: CommercialUpdate,
        now: u64,
    ) -> Result<CommercialConfig, String> {
        e.publish_commercial_config(u, now)
            .map_err(|error| match error {
                BillingError::InvalidState(message) => message,
                other => other.to_string(),
            })
    }
    fn publish_models(e: &BillingEngine, models: Vec<ModelMap>) -> Result<(), String> {
        let mut u = update(e);
        u.models = models;
        publish(e, u, 100).map(|_| ())
    }
    fn wildcard(id: &str, per_call: i64, from: u64) -> RateCardVersion {
        price(id, "*", per_call, from)
    }
    fn price(id: &str, model: &str, per_call: i64, from: u64) -> RateCardVersion {
        serde_json::from_value(serde_json::json!({
            "id": id, "rate_card_id": "default", "model": model,
            "currency": "CNY", "pricing_mode": "per_call",
            "input_price_per_m": 0.0, "output_price_per_m": 0.0,
            "cache_creation_price_per_m": 0.0, "cache_read_price_per_m": 0.0,
            "fixed_input_credit_per_m": 0, "fixed_output_credit_per_m": 0,
            "fixed_cache_creation_credit_per_m": 0, "fixed_cache_read_credit_per_m": 0,
            "per_call_credit": per_call, "margin_multiplier": 1.0, "effective_from_secs": from
        }))
        .unwrap()
    }

    /// Disabling a provider used to freeze every later publication, whatever it changed:
    /// each re-checked every visible model, fallbacks included. Only a model the
    /// publication lists for customers must now be servable, by its primary target.
    #[test]
    fn a_broken_model_blocks_only_its_own_publication() {
        let e = serving_engine();
        let model = |id: &str| ModelMap::new(format!("map-{id}"), "new-tier", id, "p", "target");
        publish_models(&e, vec![model("model-a")]).unwrap();
        e.set_provider_enabled("p", false).unwrap();

        // Unrelated publications go through.
        e.publish_commercial_config(update(&e), 101).unwrap();
        // The broken models are named when they are published for customers.
        assert_eq!(
            publish_models(&e, vec![model("model-a"), model("model-b")]),
            Err("Visible model target has no enabled compatible key: model-a, model-b".into())
        );
        // Hidden or retired, they need no route.
        let mut hidden = model("model-a");
        hidden.visible = false;
        let mut retired = model("model-b");
        retired.retired = true;
        publish_models(&e, vec![hidden, retired]).unwrap();

        // A Key that may not call the target is no route either.
        e.set_provider_enabled("p", true).unwrap();
        let mut key = ProviderKey::new("k", "p", "test");
        key.allowed_models = Some(vec!["other".into()]);
        e.upsert_provider_key(key);
        assert_eq!(
            publish_models(&e, vec![model("model-c")]),
            Err("Visible model target has no enabled compatible key: model-c".into())
        );
        e.upsert_provider_key(ProviderKey::new("k", "p", "test"));

        // A fallback may be disabled, but must name a provider.
        let mut idle = Provider::new(
            "idle",
            "Idle",
            crate::provider::ProviderFormat::OpenAi,
            "https://example.com",
        );
        idle.enabled = false;
        e.upsert_provider(idle);
        publish_models(&e, vec![model("model-c").with_fallback("idle", "spare")]).unwrap();
        assert_eq!(
            publish_models(&e, vec![model("model-d").with_fallback("ghost", "spare")]),
            Err("Unknown target provider".into())
        );
        // Structure is still checked for every mapping, not only the published ones.
        e.upsert_model_map(ModelMap::new(
            "map-stray",
            "new-tier",
            "stray",
            "ghost",
            "t",
        ));
        assert!(e.publish_commercial_config(update(&e), 102).is_err());
    }

    /// A model's first price may be published to start now, which is how a client asks
    /// for it; the server's clock decides when that is. A request in flight still settles
    /// at the price it was reserved at.
    #[test]
    fn a_models_first_price_starts_now_and_spares_requests_in_flight() {
        use crate::card::Card;
        use crate::reservation::ReservationEstimateParams;
        let credit = crate::MICRO_CREDITS_PER_CREDIT;
        let e = serving_engine();
        let mut u = update(&e);
        u.models.push(ModelMap::new(
            "map-a", "new-tier", "model-a", "p", "target-a",
        ));
        u.versions.push(wildcard("v-star", credit, 100));
        e.publish_commercial_config(u, 100).unwrap();
        let mut card = Card::new("card", "new-tier", 1_000 * credit);
        card.activate(100, 86_400).unwrap();
        e.upsert_card(card);
        let params = ReservationEstimateParams::new(0, 1).with_model("model-a");
        let held = e.reserve("card", "held", &params, 101, 600).unwrap();
        assert_eq!(held.rate_card_version.as_deref(), Some("v-star"));

        let mut first = update(&e);
        first.versions.push(price("v-a", "model-a", 5 * credit, 0));
        let published = e.publish_commercial_config(first, 102).unwrap();
        let stamped = published.versions.iter().find(|v| v.id == "v-a").unwrap();
        assert_eq!(stamped.effective_from_secs, 102);
        let audit = e.list_rate_card_audit_logs(Some("default"));
        let entry = audit.iter().find(|log| log.version_id == "v-a").unwrap();
        assert_eq!(entry.created_at_secs, 102);
        assert_eq!(entry.previous_version_id.as_deref(), Some("v-star"));
        assert_eq!(published.audit.last().unwrap().created_at_secs, 102);

        // The model has a price now: another one may not start in the past.
        let mut second = update(&e);
        second.versions.push(price("v-a2", "model-a", credit, 0));
        assert!(e.publish_commercial_config(second, 103).is_err());

        let tokens = UsageTokens::default();
        let settled = e
            .settle("held", &tokens, "model-a", "p", "target-a", 103)
            .unwrap();
        assert_eq!(settled.rate_card_version.as_deref(), Some("v-star"));
        assert_eq!(settled.credits_charged, credit);
        e.reserve("card", "after", &params, 103, 600).unwrap();
        let after = e
            .settle("after", &tokens, "model-a", "p", "target-a", 104)
            .unwrap();
        assert_eq!(after.rate_card_version.as_deref(), Some("v-a"));
        assert_eq!(after.credits_charged, 5 * credit);
    }

    /// Whether a model has a price is decided by what was published before: a first price
    /// from now and a later one publish together in either order, and a model gets one
    /// first price.
    #[test]
    fn a_first_price_from_now_may_come_with_later_prices() {
        for later_first in [false, true] {
            let e = serving_engine();
            let mut u = update(&e);
            u.versions = vec![price("v-now", "m", 1, 0), price("v-later", "m", 2, 500)];
            if later_first {
                u.versions.reverse();
            }
            let config = e.publish_commercial_config(u, 100).unwrap();
            let stamped = config.versions.iter().find(|v| v.id == "v-now").unwrap();
            assert_eq!(stamped.effective_from_secs, 100);
        }
        let e = serving_engine();
        let mut u = update(&e);
        u.versions = vec![price("v-a", "m", 1, 0), price("v-b", "m", 2, 0)];
        assert!(e.publish_commercial_config(u, 100).is_err());
        assert!(e.commercial_config().versions.is_empty());
    }

    #[test]
    fn only_withdrawn_models_are_removed_and_only_scheduled_prices_cancelled() {
        let e = serving_engine();
        let mut listed = ModelMap::new("map-listed", "new-tier", "listed", "p", "t");
        let mut hidden = ModelMap::new("map-hidden", "new-tier", "hidden", "p", "t");
        hidden.visible = false;
        let mut retired = ModelMap::new("map-retired", "new-tier", "retired", "p", "t");
        retired.retired = true;
        let mut u = update(&e);
        u.models = vec![listed.clone(), hidden, retired];
        u.versions = vec![
            price("v-now", "listed", 1, 100),
            price("v-later", "listed", 2, 500),
        ];
        e.publish_commercial_config(u, 100).unwrap();
        let attempt = |removed: &[&str], cancelled: &[&str]| {
            let u = CommercialUpdate {
                groups: vec![],
                rate_cards: vec![],
                removed_models: removed.iter().map(|id| id.to_string()).collect(),
                cancelled_versions: cancelled.iter().map(|id| id.to_string()).collect(),
                ..update(&e)
            };
            e.publish_commercial_config(u, 200)
        };
        for (removed, message) in [
            (
                "map-listed",
                "Only hidden or retired mappings can be removed: map-listed",
            ),
            (
                "map-unknown",
                "Only hidden or retired mappings can be removed: map-unknown",
            ),
        ] {
            assert_eq!(
                attempt(&[removed], &[]).unwrap_err(),
                BillingError::InvalidState(message.into())
            );
        }
        for (cancelled, message) in [
            ("v-now", "Only scheduled prices can be cancelled: v-now"),
            (
                "v-unknown",
                "Only scheduled prices can be cancelled: v-unknown",
            ),
        ] {
            assert_eq!(
                attempt(&[], &[cancelled]).unwrap_err(),
                BillingError::InvalidState(message.into())
            );
        }
        let config = attempt(&["map-hidden", "map-retired"], &["v-later"]).unwrap();
        let ids: Vec<_> = config.models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["map-listed"]);
        let ids: Vec<_> = config.versions.iter().map(|v| v.id.as_str()).collect();
        assert_eq!(ids, ["v-now"]);

        // Withdrawn in one publication and removed in the next.
        listed.retired = true;
        publish_models(&e, vec![listed]).unwrap();
        assert!(attempt(&["map-listed"], &[]).unwrap().models.is_empty());
        // A scheduled price is replaced in one publication: cancelled, then published anew.
        let mut u = update(&e);
        u.versions = vec![price("v-soon", "listed", 3, 800)];
        e.publish_commercial_config(u, 300).unwrap();
        let mut u = update(&e);
        u.cancelled_versions = vec!["v-soon".into()];
        u.versions = vec![price("v-soon-2", "listed", 4, 800)];
        let config = e.publish_commercial_config(u, 300).unwrap();
        assert!(config.versions.iter().any(|v| v.id == "v-soon-2"));
        assert!(!config.versions.iter().any(|v| v.id == "v-soon"));
    }

    #[test]
    fn a_retired_flag_survives_a_restart_and_older_mappings_load_without_it() {
        let e = serving_engine();
        let mut retired = ModelMap::new("map-r", "new-tier", "model-r", "p", "t");
        retired.retired = true;
        publish_models(&e, vec![retired]).unwrap();
        let restored = BillingEngine::new();
        restored.import_snapshot(e.export_snapshot());
        assert!(restored.commercial_config().models[0].retired);
        assert!(restored.list_models_for_group("new-tier", true).is_empty());

        let mut older = serde_json::to_value(ModelMap::new("m", "g", "x", "p", "t")).unwrap();
        older.as_object_mut().unwrap().remove("retired");
        let older: ModelMap = serde_json::from_value(older).unwrap();
        assert!(!older.retired && older.is_listed());
    }

    /// A published ID or alias is what requests may name; an older mapping that is not
    /// still loads, and blocks no publication that leaves it alone.
    #[test]
    fn published_model_ids_follow_the_request_rule() {
        let e = serving_engine();
        let model = |id: &str| ModelMap::new("map-x", "new-tier", id, "p", "t");
        let long = "m".repeat(129);
        for (mapping, bad) in [
            (model("model a"), "model a"),
            (model("model\u{e9}"), "model\u{e9}"),
            (model(&long), long.as_str()),
            (model(""), ""),
            (model("model-x").with_alias("alias@x"), "alias@x"),
        ] {
            assert_eq!(
                publish_models(&e, vec![mapping]),
                Err(format!("Invalid model ID: {bad}"))
            );
        }
        publish_models(&e, vec![model("vendor/model-1.5:beta_2").with_alias("m.x")]).unwrap();

        e.upsert_model_map(ModelMap::new("map-old", "new-tier", "old model", "p", "t"));
        let file = std::env::temp_dir().join(format!(
            "kiro-model-id-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        e.save_to_file(&file).unwrap();
        let restored = BillingEngine::new();
        let loaded = restored.load_from_file(&file);
        std::fs::remove_file(&file).unwrap();
        loaded.unwrap();
        assert!(restored
            .commercial_config()
            .models
            .iter()
            .any(|m| m.exposed_model_id == "old model"));
        restored
            .publish_commercial_config(update(&restored), 200)
            .unwrap();
    }

    /// Claude Opus 5's official prices ($5 / $25 / $6.25 / $0.50 per million tokens) at 计费倍率
    /// 0.24 and 成本倍率 0.08, ¥1 to the official dollar, at `face` CNY a credit.
    fn official(face: f64) -> OfficialPricing {
        OfficialPricing {
            input_usd_per_m: 5.0,
            output_usd_per_m: 25.0,
            cache_creation_usd_per_m: 6.25,
            cache_read_usd_per_m: 0.5,
            price_multiplier: 0.24,
            cost_multiplier: 0.08,
            cost_basis_usd_per_m: None,
            usd_cny: 1.0,
            credit_face_value_cny: face,
        }
    }
    /// A fixed CNY price computed from `o` the way the console computes it.
    fn priced_from(id: &str, model: &str, from: u64, o: OfficialPricing) -> RateCardVersion {
        let official = [
            o.input_usd_per_m,
            o.output_usd_per_m,
            o.cache_creation_usd_per_m,
            o.cache_read_usd_per_m,
        ];
        let credits = official.map(|usd| {
            (usd * o.price_multiplier * o.usd_cny / o.credit_face_value_cny * 1_000_000.0).round()
                as i64
        });
        let costs = o
            .cost_basis_usd_per_m
            .unwrap_or(official)
            .map(|usd| usd * o.cost_multiplier * o.usd_cny);
        RateCardVersion {
            id: id.into(),
            rate_card_id: "default".into(),
            model: model.into(),
            currency: Currency::Cny,
            pricing_mode: PricingMode::Fixed,
            input_price_per_m: costs[0],
            output_price_per_m: costs[1],
            cache_creation_price_per_m: costs[2],
            cache_read_price_per_m: costs[3],
            fixed_input_credit_per_m: credits[0],
            fixed_output_credit_per_m: credits[1],
            fixed_cache_creation_credit_per_m: credits[2],
            fixed_cache_read_credit_per_m: credits[3],
            per_call_credit: 0,
            margin_multiplier: 1.0,
            effective_from_secs: from,
            official: Some(o),
        }
    }
    fn at_face_value(e: &BillingEngine, face: f64, now: u64) {
        let mut u = update(e);
        u.settings = Some(BillingSettings {
            credit_face_value_cny: face,
            ..e.get_settings()
        });
        e.publish_commercial_config(u, now).unwrap();
    }

    /// A price computed from an official one carries what it was computed from, and is
    /// published only when that gives its credits (to within one micro-credit) and its costs.
    /// A route's cost charges nothing; a model ID with a '/' that names no provider is a model.
    #[test]
    fn an_official_price_publishes_only_when_it_gives_its_credits_and_costs() {
        let e = serving_engine();
        at_face_value(&e, 0.03, 100);
        let attempt = |v: RateCardVersion| {
            let mut u = update(&e);
            u.versions = vec![v];
            publish(&e, u, 100)
        };
        let opus = |edit: &dyn Fn(&mut RateCardVersion)| {
            let mut v = priced_from("v-opus", "claude-opus-5", 0, official(0.03));
            edit(&mut v);
            v
        };
        let block =
            |edit: &dyn Fn(&mut OfficialPricing)| opus(&|v| edit(v.official.as_mut().unwrap()));
        // Eight credits an official dollar: 40, 200, 50 and 4 credits a million tokens.
        let v = opus(&|_| {});
        assert_eq!(
            [
                v.fixed_input_credit_per_m,
                v.fixed_output_credit_per_m,
                v.fixed_cache_creation_credit_per_m,
                v.fixed_cache_read_credit_per_m,
            ],
            [40_000_000, 200_000_000, 50_000_000, 4_000_000]
        );
        let credits = "Official pricing does not match the credits of: claude-opus-5";
        let cost = "Official pricing does not match the cost of: claude-opus-5";
        let invalid = "Invalid official pricing: claude-opus-5";
        for (v, message) in [
            (opus(&|v| v.fixed_output_credit_per_m += 2), credits),
            (opus(&|v| v.fixed_cache_read_credit_per_m = 0), credits),
            (opus(&|v| v.output_price_per_m = 2.000_001), cost),
            (
                block(&|o| o.cost_basis_usd_per_m = Some([2.0, 25.0, 6.25, 0.5])),
                cost,
            ),
            (opus(&|v| v.pricing_mode = PricingMode::CostPlus), invalid),
            (opus(&|v| v.currency = Currency::Usd), invalid),
            (opus(&|v| v.margin_multiplier = 1.3), invalid),
            (opus(&|v| v.per_call_credit = 1), invalid),
            (block(&|o| o.input_usd_per_m = f64::NAN), invalid),
            (block(&|o| o.output_usd_per_m = 10_000.5), invalid),
            (
                block(&|o| o.cost_basis_usd_per_m = Some([-1.0, 25.0, 6.25, 0.5])),
                invalid,
            ),
            (block(&|o| o.price_multiplier = 0.0), invalid),
            (block(&|o| o.cost_multiplier = 100.5), invalid),
            (block(&|o| o.credit_face_value_cny = 0.0), invalid),
            (
                block(&|o| {
                    o.input_usd_per_m = 0.0;
                    o.output_usd_per_m = 0.0;
                    o.cache_creation_usd_per_m = 0.0;
                    o.cache_read_usd_per_m = 0.0;
                }),
                invalid,
            ),
        ] {
            assert_eq!(attempt(v).unwrap_err(), message);
        }

        // Within one micro-credit, as the console may round; read back with its block.
        let config = attempt(opus(&|v| v.fixed_cache_read_credit_per_m += 1)).unwrap();
        let read = config.versions.iter().find(|v| v.id == "v-opus").unwrap();
        assert_eq!(read.official, Some(official(0.03)));
        assert_eq!(read.fixed_cache_read_credit_per_m, 4_000_001);
        // An upstream that bills other prices: the costs are of those, the credits of the
        // official ones.
        let billed = OfficialPricing {
            cost_multiplier: 0.22,
            cost_basis_usd_per_m: Some([2.0, 25.0, 6.25, 0.5]),
            ..official(0.03)
        };
        let config = attempt(priced_from("v-billed", "claude-opus-5-5", 0, billed)).unwrap();
        let read = config.versions.iter().find(|v| v.id == "v-billed").unwrap();
        assert_eq!(read.fixed_input_credit_per_m, 40_000_000);
        assert!((read.input_price_per_m - 0.44).abs() < 1e-12);

        let mut route = priced_from("v-route", "p/target", 0, official(0.03));
        assert_eq!(
            attempt(route.clone()).unwrap_err(),
            "Official pricing does not match the credits of: p/target"
        );
        route.fixed_input_credit_per_m = 0;
        route.fixed_output_credit_per_m = 0;
        route.fixed_cache_creation_credit_per_m = 0;
        route.fixed_cache_read_credit_per_m = 0;
        attempt(route).unwrap();
        attempt(priced_from("v-vendor", "vendor/model", 0, official(0.03))).unwrap();
    }

    /// The official pricing settings are bounded, and one a publication leaves out keeps its
    /// value: a console that does not know them never wipes them.
    #[test]
    fn official_pricing_settings_are_bounded_and_kept_when_left_out() {
        let e = serving_engine();
        assert_eq!(e.get_settings().official_usd_cny(), 1.0);
        let with = |edit: &dyn Fn(&mut BillingSettings)| {
            let mut settings = e.get_settings();
            edit(&mut settings);
            let mut u = update(&e);
            u.settings = Some(settings);
            publish(&e, u, 100)
        };
        let providers = |entries: Vec<(String, f64)>| -> Option<BTreeMap<String, f64>> {
            Some(entries.into_iter().collect())
        };
        let rate = "The official dollar rate must be positive and at most 1000";
        let multipliers = "Multipliers must be positive and at most 100, for at most 200 providers";
        for (refused, message) in [
            (with(&|s| s.official_usd_cny = Some(0.0)), rate),
            (with(&|s| s.official_usd_cny = Some(1000.5)), rate),
            (with(&|s| s.official_usd_cny = Some(f64::INFINITY)), rate),
            (
                with(&|s| s.default_price_multiplier = Some(0.0)),
                multipliers,
            ),
            (
                with(&|s| s.default_cost_multiplier = Some(100.5)),
                multipliers,
            ),
            (
                with(&|s| s.provider_cost_multipliers = providers(vec![(" ".into(), 0.08)])),
                multipliers,
            ),
            (
                with(&|s| s.provider_cost_multipliers = providers(vec![("p".repeat(129), 0.08)])),
                multipliers,
            ),
            (
                with(&|s| s.provider_cost_multipliers = providers(vec![("p".into(), f64::NAN)])),
                multipliers,
            ),
            (
                with(&|s| {
                    s.provider_cost_multipliers =
                        providers((0..201).map(|i| (format!("p{i}"), 0.08)).collect())
                }),
                multipliers,
            ),
        ] {
            assert_eq!(refused.unwrap_err(), message);
        }

        let set = with(&|s| {
            s.official_usd_cny = Some(1.0);
            s.default_price_multiplier = Some(0.24);
            s.default_cost_multiplier = Some(0.08);
            s.provider_cost_multipliers = providers(vec![
                ("hanyue-max".into(), 0.22),
                ("kimera-direct".into(), 0.06),
            ]);
        })
        .unwrap()
        .settings;
        // What the console sends today: the face value and the exchange rate alone.
        let mut u = update(&e);
        u.settings = Some(
            serde_json::from_value(
                serde_json::json!({"credit_face_value_cny": 0.03, "usd_cny_rate": 7.0}),
            )
            .unwrap(),
        );
        let kept = publish(&e, u, 101).unwrap().settings;
        assert_eq!((kept.credit_face_value_cny, kept.usd_cny_rate), (0.03, 7.0));
        assert_eq!(
            BillingSettings {
                credit_face_value_cny: 0.01,
                usd_cny_rate: 7.25,
                rate_updated_at_secs: 100,
                ..kept
            },
            set
        );
    }

    /// Changing the face value (or the CNY an official dollar counts) reprices every price
    /// computed from an official one in the same publication, from now: one left at the old
    /// value, in force or scheduled, is refused by name. Other prices keep their credits and
    /// are never back-dated, and a request in flight settles at what it was reserved at.
    #[test]
    fn a_face_value_change_reprices_every_official_price_at_once() {
        use crate::card::Card;
        use crate::reservation::ReservationEstimateParams;
        let credit = crate::MICRO_CREDITS_PER_CREDIT;
        let e = serving_engine();
        let mut u = update(&e);
        u.settings = Some(BillingSettings {
            credit_face_value_cny: 0.03,
            provider_cost_multipliers: Some([("p".to_string(), 0.08)].into()),
            ..e.get_settings()
        });
        u.models = vec![ModelMap::new(
            "map-a", "new-tier", "model-a", "p", "target-a",
        )];
        u.versions = vec![
            priced_from("a-1", "model-a", 0, official(0.03)),
            priced_from("a-later", "model-a", 500, official(0.03)),
            priced_from("b-1", "model-b", 0, official(0.03)),
            price("c-1", "model-c", 7, 0),
        ];
        e.publish_commercial_config(u, 100).unwrap();
        let mut card = Card::new("card", "new-tier", 1_000 * credit);
        card.activate(100, 86_400).unwrap();
        e.upsert_card(card);
        let params = ReservationEstimateParams::new(0, 1_000_000).with_model("model-a");
        let hold = e.reserve("card", "held", &params, 101, 600).unwrap();
        assert_eq!(hold.pricing.unwrap().settings, e.get_settings());

        let reprice = |versions: Vec<RateCardVersion>, cancelled: &[&str]| {
            let mut u = update(&e);
            u.settings = Some(BillingSettings {
                credit_face_value_cny: 0.05,
                ..e.get_settings()
            });
            u.versions = versions;
            u.cancelled_versions = cancelled.iter().map(|id| id.to_string()).collect();
            publish(&e, u, 200)
        };
        let at_new_face_value = || {
            vec![
                priced_from("a-2", "model-a", 0, official(0.05)),
                priced_from("b-2", "model-b", 0, official(0.05)),
            ]
        };
        let stale = "Official pricing is at a stale face value or rate";
        assert_eq!(
            reprice(vec![], &[]).unwrap_err(),
            format!("{stale}: model-a, model-b")
        );
        // model-a's scheduled price is stale too, until it is withdrawn or repriced.
        assert_eq!(
            reprice(at_new_face_value(), &[]).unwrap_err(),
            format!("{stale}: model-a")
        );
        let mut old = at_new_face_value();
        old[1] = priced_from("b-2", "model-b", 0, official(0.03));
        assert_eq!(
            reprice(old, &["a-later"]).unwrap_err(),
            format!("{stale}: model-b")
        );
        // Only a price computed from an official one may start now.
        let mut back_dated = at_new_face_value();
        back_dated.push(price("c-2", "model-c", 8, 0));
        assert_eq!(
            reprice(back_dated, &["a-later"]).unwrap_err(),
            "Invalid pricing; retroactive publication forbidden"
        );
        let config = reprice(at_new_face_value(), &["a-later"]).unwrap();
        for id in ["a-2", "b-2"] {
            let v = config.versions.iter().find(|v| v.id == id).unwrap();
            assert_eq!(v.effective_from_secs, 200);
        }
        let other = e
            .resolve_rate_card_version("default", "model-c", 200)
            .unwrap();
        assert_eq!(other.id, "c-1");

        let tokens = UsageTokens {
            output_tokens: 1_000_000,
            ..UsageTokens::default()
        };
        // $25 at 0.24 and 0.03 CNY a credit, as when it was reserved.
        let held = e
            .settle("held", &tokens, "model-a", "p", "target-a", 201)
            .unwrap();
        assert_eq!(held.rate_card_version.as_deref(), Some("a-1"));
        assert_eq!(held.credits_charged, 200 * credit);
        // Settled, its record only refuses a replay: what it was priced with is not kept.
        assert!(e.export_snapshot().reservations["held"].pricing.is_none());
        // At 0.05 CNY a credit.
        e.reserve("card", "after", &params, 202, 600).unwrap();
        let after = e
            .settle("after", &tokens, "model-a", "p", "target-a", 203)
            .unwrap();
        assert_eq!(after.rate_card_version.as_deref(), Some("a-2"));
        assert_eq!(after.credits_charged, 120 * credit);

        // The CNY an official dollar counts reprices them the same way.
        let mut u = update(&e);
        u.settings = Some(BillingSettings {
            official_usd_cny: Some(2.0),
            ..e.get_settings()
        });
        assert_eq!(
            publish(&e, u, 300).unwrap_err(),
            format!("{stale}: model-a, model-b")
        );
    }

    /// A price left at an older face value (an older release changed it, knowing nothing of
    /// official prices) blocks no unrelated publication, and a new official price is still
    /// held to the face value in force.
    #[test]
    fn a_stale_official_price_blocks_no_unrelated_publication() {
        let e = serving_engine();
        at_face_value(&e, 0.03, 100);
        let mut u = update(&e);
        u.versions = vec![priced_from("a-1", "model-a", 0, official(0.03))];
        e.publish_commercial_config(u, 100).unwrap();
        e.update_settings(BillingSettings {
            credit_face_value_cny: 0.05,
            ..e.get_settings()
        });
        publish(&e, update(&e), 101).unwrap();
        let mut u = update(&e);
        u.versions = vec![priced_from("b-1", "model-b", 0, official(0.03))];
        assert_eq!(
            publish(&e, u, 101).unwrap_err(),
            "Official pricing is at a stale face value or rate: model-b"
        );
        let mut u = update(&e);
        u.versions = vec![priced_from("b-1", "model-b", 0, official(0.05))];
        publish(&e, u, 101).unwrap();
    }

    /// An older release ignores what it does not know, so it still loads prices and settings
    /// a later one saved: the official block, the new settings, and any field added later.
    /// Without them, prices and settings are saved as before, so the revision is unchanged.
    #[test]
    fn prices_and_settings_with_unknown_fields_still_load() {
        let mut saved = serde_json::to_value(priced_from("v", "m", 0, official(0.03))).unwrap();
        saved["added_later"] = serde_json::json!(1);
        saved["official"]["added_later"] = serde_json::json!("x");
        let loaded: RateCardVersion = serde_json::from_value(saved).unwrap();
        assert_eq!(loaded.official, Some(official(0.03)));
        let settings: BillingSettings = serde_json::from_value(serde_json::json!({
            "credit_face_value_cny": 0.03,
            "usd_cny_rate": 7.25,
            "official_usd_cny": 1.0,
            "added_later": true
        }))
        .unwrap();
        assert_eq!(settings.official_usd_cny, Some(1.0));

        let plain = serde_json::to_value(price("p", "m", 1, 0)).unwrap();
        assert!(plain.get("official").is_none());
        assert_eq!(
            serde_json::to_value(BillingSettings::default()).unwrap(),
            serde_json::json!({
                "credit_face_value_cny": 0.01,
                "usd_cny_rate": 7.25,
                "rate_updated_at_secs": 0
            })
        );
    }

    /// The largest settings the validation allows: every count at its most, every name and
    /// note at its longest and made of quotes, which JSON escapes to two bytes each, and
    /// prices of the longest digits. They fit in half the publication body limit, and are
    /// published.
    #[test]
    fn largest_settings_are_published_within_the_body_limit() {
        let e = serving_engine();
        // `n` bytes of quotes, told apart by a numbered end, such as `"""…"/7`.
        let quotes = |n: usize, tail: String| format!("{}{tail}", "\"".repeat(n - tail.len()));
        let long = 1_234.567_890_123_457;
        let settings = BillingSettings {
            default_price_multiplier: Some(0.123_456_789_012_345_67),
            default_cost_multiplier: Some(0.123_456_789_012_345_67),
            provider_cost_multipliers: Some(
                (0..MAX_PROVIDER_MULTIPLIERS)
                    .map(|i| (quotes(128, format!("-{i}")), 0.123_456_789_012_345_67))
                    .collect(),
            ),
            official_prices: Some(
                (0..MAX_OFFICIAL_PRICES)
                    .map(|i| {
                        let mut price = official_price([long; 4], Some(quotes(256, String::new())));
                        price.updated_at_secs = u64::MAX;
                        (quotes(256, format!("-{i}")), price)
                    })
                    .collect(),
            ),
            route_costs: Some(
                (0..MAX_ROUTE_COSTS)
                    .map(|i| {
                        let cost = RouteCost {
                            cost_multiplier: Some(0.123_456_789_012_345_67),
                            basis_usd_per_m: Some([long; 4]),
                        };
                        (quotes(256, format!("/{i}")), cost)
                    })
                    .collect(),
            ),
            ..e.get_settings()
        };
        let mut u = update(&e);
        u.settings = Some(settings);
        let size = serde_json::to_vec(&u).unwrap().len();
        assert!(
            size < MAX_COMMERCIAL_UPDATE_BYTES / 2,
            "the largest settings take {size} bytes"
        );
        e.publish_commercial_config(u, 100).unwrap();
    }

    fn official_price(usd: [f64; 4], note: Option<String>) -> OfficialPrice {
        OfficialPrice {
            input_usd_per_m: usd[0],
            output_usd_per_m: usd[1],
            cache_creation_usd_per_m: usd[2],
            cache_read_usd_per_m: usd[3],
            note,
            updated_at_secs: 5,
        }
    }

    /// A request costs what the upstream that served it bills, whatever group or price table
    /// it came from: the route's own basis or its model's official price, times the route's,
    /// the provider's or the default 成本倍率, at the settings it was reserved at. A route
    /// without both is costed from price versions, as before.
    #[test]
    fn a_request_costs_what_the_route_that_served_it_bills() {
        use crate::card::Card;
        use crate::reservation::ReservationEstimateParams;
        let credit = crate::MICRO_CREDITS_PER_CREDIT;
        let e = serving_engine();
        for id in ["hanyue", "kimera"] {
            e.upsert_provider(Provider::new(
                id,
                id,
                crate::provider::ProviderFormat::Anthropic,
                "https://upstream.invalid",
            ));
            e.upsert_provider_key(ProviderKey::new(format!("{id}-key"), id, "test"));
        }
        // One credit and `cny` of cost per million input tokens.
        let version = |id: &str, model: &str, cny: f64| -> RateCardVersion {
            serde_json::from_value(serde_json::json!({
                "id": id, "rate_card_id": "default", "model": model,
                "currency": "CNY", "pricing_mode": "fixed",
                "input_price_per_m": cny, "output_price_per_m": 0.0,
                "cache_creation_price_per_m": 0.0, "cache_read_price_per_m": 0.0,
                "fixed_input_credit_per_m": credit, "fixed_output_credit_per_m": 0,
                "fixed_cache_creation_credit_per_m": 0, "fixed_cache_read_credit_per_m": 0,
                "per_call_credit": 0, "margin_multiplier": 1.0, "effective_from_secs": 0
            }))
            .unwrap()
        };
        let mut u = update(&e);
        // A second group on the same price table.
        u.groups.push(Group::pro_plus("other-tier", "Other tier"));
        u.settings = Some(BillingSettings {
            default_cost_multiplier: Some(0.1),
            provider_cost_multipliers: Some(
                [("hanyue".to_string(), 0.22), ("kimera".to_string(), 0.08)].into(),
            ),
            official_prices: Some(
                [
                    (
                        "opus".to_string(),
                        official_price([4.0, 20.0, 5.0, 0.2], None),
                    ),
                    (
                        "sonnet".to_string(),
                        official_price([3.0, 15.0, 3.75, 0.3], None),
                    ),
                ]
                .into(),
            ),
            route_costs: Some(
                [
                    (
                        "hanyue/opus".to_string(),
                        RouteCost {
                            cost_multiplier: None,
                            basis_usd_per_m: Some([2.0, 25.0, 6.25, 0.5]),
                        },
                    ),
                    (
                        "p/sonnet".to_string(),
                        RouteCost {
                            cost_multiplier: Some(0.5),
                            basis_usd_per_m: None,
                        },
                    ),
                ]
                .into(),
            ),
            ..e.get_settings()
        });
        u.models = vec![
            ModelMap::new("map-a", "new-tier", "opus", "hanyue", "opus")
                .with_fallback("kimera", "opus"),
            ModelMap::new("map-b", "other-tier", "opus", "kimera", "opus"),
            ModelMap::new("map-c", "new-tier", "legacy", "kimera", "legacy"),
        ];
        // The price's own cost is the primary's, as it had to be typed before.
        u.versions = vec![
            version("v-opus", "opus", 1.1),
            version("v-legacy", "legacy", 3.0),
        ];
        e.publish_commercial_config(u, 100).unwrap();
        for (card, group) in [("card-a", "new-tier"), ("card-b", "other-tier")] {
            let mut card = Card::new(card, group, 1_000 * credit);
            card.activate(100, 86_400).unwrap();
            e.upsert_card(card);
        }
        let tokens = UsageTokens {
            uncached_input_tokens: 1_000_000,
            ..UsageTokens::default()
        };
        let reserve = |card: &str, id: &str, model: &str, at: u64| {
            let params = ReservationEstimateParams::new(1_000_000, 0).with_model(model);
            e.reserve(card, id, &params, at, 600).unwrap();
        };
        let serve = |card: &str, id: &str, model: &str, route: (&str, &str), at: u64| {
            reserve(card, id, model, at);
            let entry = e
                .settle(id, &tokens, model, route.0, route.1, at + 1)
                .unwrap();
            (entry.provider_cost_micro_cny, entry.reason.unwrap())
        };
        let official = |route: &str| format!("provider_cost:official={route}");
        // hanyue bills its own basis ($2 input) x its 0.22.
        assert_eq!(
            serve("card-a", "a-1", "opus", ("hanyue", "opus"), 110),
            (440_000, official("hanyue/opus"))
        );
        // The backup, and the other group's primary: opus's $4 x kimera's 0.08.
        assert_eq!(
            serve("card-a", "a-2", "opus", ("kimera", "opus"), 120),
            (320_000, official("kimera/opus"))
        );
        assert_eq!(
            serve("card-b", "b-1", "opus", ("kimera", "opus"), 130),
            (320_000, official("kimera/opus"))
        );
        // A provider without its own multiplier, and a route's own multiplier.
        assert_eq!(
            serve("card-a", "a-3", "opus", ("p", "opus"), 140),
            (400_000, official("p/opus"))
        );
        assert_eq!(
            serve("card-a", "a-4", "opus", ("p", "sonnet"), 150),
            (1_500_000, official("p/sonnet"))
        );
        // No official price for the upstream model: from the price versions, as before.
        assert_eq!(
            serve("card-a", "a-5", "legacy", ("kimera", "legacy"), 160),
            (
                3_000_000,
                "provider_cost:rate_card_version=v-legacy".to_string()
            )
        );

        // kimera's multiplier changes while a request is in flight: it keeps the one it was
        // reserved at.
        reserve("card-b", "held", "opus", 200);
        let mut u = update(&e);
        u.settings = Some(BillingSettings {
            provider_cost_multipliers: Some(
                [("hanyue".to_string(), 0.22), ("kimera".to_string(), 0.1)].into(),
            ),
            ..e.get_settings()
        });
        e.publish_commercial_config(u, 201).unwrap();
        let held = e
            .settle("held", &tokens, "opus", "kimera", "opus", 202)
            .unwrap();
        assert_eq!(held.provider_cost_micro_cny, 320_000);
        assert_eq!(
            serve("card-b", "after", "opus", ("kimera", "opus"), 203),
            (400_000, official("kimera/opus"))
        );
    }

    /// The official price table and the route costs are bounded like the other settings and
    /// kept when a publication leaves them out; an official price's time is when its prices
    /// last changed.
    #[test]
    fn official_prices_and_route_costs_are_bounded_and_stamped() {
        let e = serving_engine();
        let price =
            |input: f64, note: Option<String>| official_price([input, 25.0, 6.25, 0.5], note);
        let route = |multiplier: Option<f64>, basis: Option<[f64; 4]>| RouteCost {
            cost_multiplier: multiplier,
            basis_usd_per_m: basis,
        };
        let with = |prices: Option<Vec<(String, OfficialPrice)>>,
                    routes: Option<Vec<(String, RouteCost)>>,
                    now: u64| {
            let mut u = update(&e);
            u.settings = Some(BillingSettings {
                official_prices: prices.map(|prices| prices.into_iter().collect()),
                route_costs: routes.map(|routes| routes.into_iter().collect()),
                ..e.get_settings()
            });
            publish(&e, u, now)
        };
        let invalid_prices = "Official prices: at most 200, named in 1-256 bytes, priced 0-10000, notes of at most 256 bytes";
        for prices in [
            vec![(" ".to_string(), price(5.0, None))],
            vec![("m".repeat(257), price(5.0, None))],
            vec![("m".to_string(), price(f64::NAN, None))],
            vec![("m".to_string(), price(10_000.5, None))],
            vec![("m".to_string(), price(5.0, Some("n".repeat(257))))],
            vec![("m".to_string(), price(5.0, Some("line\nbreak".into())))],
            (0..=MAX_OFFICIAL_PRICES)
                .map(|i| (format!("m{i}"), price(5.0, None)))
                .collect(),
        ] {
            assert_eq!(with(Some(prices), None, 100).unwrap_err(), invalid_prices);
        }
        let invalid_routes = "Route costs: at most 200, named <provider>/<upstream model> in at most 256 bytes, multipliers positive and at most 100, prices 0-10000";
        for routes in [
            vec![("opus".to_string(), route(Some(0.2), None))],
            vec![("/opus".to_string(), route(Some(0.2), None))],
            vec![("p/".to_string(), route(Some(0.2), None))],
            vec![("p/opus".to_string(), route(Some(0.0), None))],
            vec![("p/opus".to_string(), route(Some(100.5), None))],
            vec![(
                "p/opus".to_string(),
                route(None, Some([f64::NAN, 25.0, 6.25, 0.5])),
            )],
            (0..=MAX_ROUTE_COSTS)
                .map(|i| (format!("p/m{i}"), route(Some(0.2), None)))
                .collect(),
        ] {
            assert_eq!(with(None, Some(routes), 100).unwrap_err(), invalid_routes);
        }

        let first = with(
            Some(vec![("opus".into(), price(5.0, None))]),
            Some(vec![(
                "hanyue/opus".into(),
                route(None, Some([2.0, 25.0, 6.25, 0.5])),
            )]),
            100,
        )
        .unwrap()
        .settings;
        assert_eq!(
            first.official_prices.as_ref().unwrap()["opus"].updated_at_secs,
            100
        );
        let kept = with(None, None, 150).unwrap().settings;
        assert_eq!(
            (kept.official_prices, kept.route_costs),
            (first.official_prices, first.route_costs)
        );
        // A new note keeps the time; a new or changed price is stamped.
        let later = with(
            Some(vec![
                ("opus".into(), price(5.0, Some("list price".into()))),
                ("sonnet".into(), price(3.0, None)),
            ]),
            None,
            200,
        )
        .unwrap()
        .settings
        .official_prices
        .unwrap();
        assert_eq!(
            (
                later["opus"].updated_at_secs,
                later["sonnet"].updated_at_secs
            ),
            (100, 200)
        );
        let changed = with(Some(vec![("opus".into(), price(4.0, None))]), None, 300)
            .unwrap()
            .settings
            .official_prices
            .unwrap();
        assert_eq!(changed["opus"].updated_at_secs, 300);
    }

    /// A face value change reprices a scheduled official price at its own time: in the same
    /// publication as the new face value, the console withdraws it and publishes a new ID for
    /// the same price table, model and time. Withdrawals apply before the check that a model
    /// has one price per time and before the check that every official price in force or
    /// scheduled is at the new face value.
    #[test]
    fn a_face_value_change_reprices_a_scheduled_official_price_at_its_own_time() {
        let e = serving_engine();
        at_face_value(&e, 0.03, 100);
        let mut u = update(&e);
        u.versions = vec![
            priced_from("a-1", "model-a", 0, official(0.03)),
            priced_from("a-later", "model-a", 500, official(0.03)),
        ];
        e.publish_commercial_config(u, 100).unwrap();
        let reprice = |cancelled: &[&str]| {
            let mut u = update(&e);
            u.settings = Some(BillingSettings {
                credit_face_value_cny: 0.05,
                ..e.get_settings()
            });
            u.cancelled_versions = cancelled.iter().map(|id| id.to_string()).collect();
            u.versions = vec![
                priced_from("a-2", "model-a", 0, official(0.05)),
                priced_from("a-later-2", "model-a", 500, official(0.05)),
            ];
            publish(&e, u, 200)
        };
        // Not withdrawn, the model would have two prices from the same time.
        assert_eq!(
            reprice(&[]).unwrap_err(),
            "Published prices immutable; use new ID and timestamp"
        );
        let config = reprice(&["a-later"]).unwrap();
        let versions: Vec<_> = config
            .versions
            .iter()
            .map(|v| {
                let face = v.official.as_ref().unwrap().credit_face_value_cny;
                (v.id.as_str(), v.effective_from_secs, face)
            })
            .collect();
        assert_eq!(
            versions,
            [
                ("a-1", 100, 0.03),
                ("a-2", 200, 0.05),
                ("a-later-2", 500, 0.05)
            ]
        );
    }

    /// A seven-day trial into "new-tier", listed first.
    fn trial_plan() -> Plan {
        Plan {
            id: "trial-7d".into(),
            name: "体验卡".into(),
            points: 300,
            price_cny: 9.9,
            validity_days: 7,
            max_devices: 1,
            concurrency: 1,
            default_group_id: "new-tier".into(),
            kiro_plan_type: "CUSTOM".into(),
            on_sale: true,
            sort_order: 5,
        }
    }

    /// A state that has stored no plans has the four tiers as they were issued, into
    /// group-pro-plus while it exists, and keeps the revision it had before the catalog. The
    /// first publication that changes a plan stores the whole catalog, which a restart keeps.
    #[test]
    fn a_state_without_plans_has_the_four_tiers_until_one_is_changed() {
        let e = BillingEngine::new();
        let config = e.commercial_config();
        let seeded: Vec<_> = config
            .plans
            .iter()
            .map(|p| {
                let kind = p.kiro_plan_type.as_str();
                (
                    p.id.as_str(),
                    p.name.as_str(),
                    p.points,
                    p.price_cny,
                    kind,
                    p.sort_order,
                )
            })
            .collect();
        assert_eq!(
            seeded,
            [
                ("tier-1000", "PRO", 1000, 30.0, "PRO", 10),
                ("tier-2000", "PRO+", 2000, 55.0, "PRO_PLUS", 20),
                ("tier-5000", "PRO Max", 5000, 130.0, "PRO_MAX", 30),
                ("tier-10000", "Power", 10000, 250.0, "POWER", 40),
            ]
        );
        assert!(config.plans.iter().all(|p| p.validity_days == 30
            && p.max_devices == 1
            && p.concurrency == 2
            && p.on_sale
            && p.default_group_id == "group-pro-plus"
            && p.problem().is_none()));
        // The revision as it was computed before the catalog.
        let s = e.export_snapshot();
        let groups: Vec<_> = s.groups.values().cloned().collect();
        let rate_cards: Vec<_> = s.rate_cards.values().cloned().collect();
        let no_models: Vec<ModelMap> = vec![];
        let no_versions: Vec<RateCardVersion> = vec![];
        let bytes =
            serde_json::to_vec(&(&groups, &no_models, &rate_cards, &no_versions, &s.settings))
                .unwrap();
        let revision: String = ring::digest::digest(&ring::digest::SHA256, &bytes)
            .as_ref()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(config.revision, revision);

        // A publication that leaves plans alone stores none.
        e.publish_commercial_config(update(&e), 100).unwrap();
        assert!(e.export_snapshot().plans.is_none());
        // Without group-pro-plus, the seed issues into the first group by ID.
        let mut without = e.export_snapshot();
        without.groups.remove("group-pro-plus");
        let restored = BillingEngine::new();
        restored.import_snapshot(without);
        assert!(restored
            .plans()
            .iter()
            .all(|p| p.default_group_id == "new-tier"));

        let mut u = update(&e);
        let mut pro = e.plan("tier-1000").unwrap();
        pro.price_cny = 35.0;
        u.plans = vec![pro];
        let config = e.publish_commercial_config(u, 200).unwrap();
        assert_eq!(e.export_snapshot().plans.map(|plans| plans.len()), Some(4));
        assert_eq!(e.plan("tier-1000").unwrap().price_cny, 35.0);
        assert_eq!(e.plan("tier-2000").unwrap().price_cny, 55.0);
        assert_eq!(config.plans, e.plans());
        let restored = BillingEngine::new();
        restored.import_snapshot(e.export_snapshot());
        assert_eq!(restored.commercial_config().revision, config.revision);
        assert_eq!(restored.plans(), e.plans());
    }

    /// Each field of a published plan is bounded, its default group is one the configuration
    /// has, and a refused publication changes nothing.
    #[test]
    fn plan_publication_is_bounded_and_refusals_change_nothing() {
        let e = serving_engine();
        let before = e.commercial_config().revision;
        type Edit = Box<dyn Fn(&mut Plan)>;
        let id = "Plan IDs are 1-64 of a-z, 0-9 and -";
        let name = "Plan names are 1-32 bytes";
        let points = "Plan points must be 1-10000000";
        let price = "Plan prices must be 0-100000 yuan, to the fen";
        let days = "Plan validity must be 1-3650 days";
        let devices = "Plans allow 1-10 devices";
        let concurrency = "Plan concurrency must be 1-20";
        let kind = "Plan Kiro types are PRO, PRO_PLUS, PRO_MAX, POWER or CUSTOM";
        let cases: Vec<(Edit, &str)> = vec![
            (Box::new(|p| p.id = "Trial".into()), id),
            (Box::new(|p| p.id = "trial_7d".into()), id),
            (Box::new(|p| p.id = "t".repeat(65)), id),
            (Box::new(|p| p.id = String::new()), id),
            (Box::new(|p| p.name = " ".into()), name),
            (Box::new(|p| p.name = "体验卡".repeat(4)), name),
            (Box::new(|p| p.name = "line\nbreak".into()), name),
            (Box::new(|p| p.points = 0), points),
            (Box::new(|p| p.points = 10_000_001), points),
            (Box::new(|p| p.price_cny = -0.01), price),
            (Box::new(|p| p.price_cny = 100_000.01), price),
            (Box::new(|p| p.price_cny = 9.999), price),
            (Box::new(|p| p.price_cny = f64::NAN), price),
            (Box::new(|p| p.validity_days = 0), days),
            (Box::new(|p| p.validity_days = 3651), days),
            (Box::new(|p| p.max_devices = 0), devices),
            (Box::new(|p| p.max_devices = 11), devices),
            (Box::new(|p| p.concurrency = 0), concurrency),
            (Box::new(|p| p.concurrency = 21), concurrency),
            (Box::new(|p| p.kiro_plan_type = "PRO_ULTRA".into()), kind),
            (
                Box::new(|p| p.default_group_id = "ghost".into()),
                "Unknown default group of plan",
            ),
        ];
        for (edit, message) in cases {
            let mut plan = trial_plan();
            edit(&mut plan);
            let expected = format!("{message}: {}", plan.id);
            let mut u = update(&e);
            u.plans = vec![plan];
            assert_eq!(publish(&e, u, 100).unwrap_err(), expected);
        }
        let mut u = update(&e);
        u.plans = vec![trial_plan(), trial_plan()];
        assert_eq!(publish(&e, u, 100).unwrap_err(), "Duplicate plan: trial-7d");
        let mut u = update(&e);
        u.removed_plans = vec!["ghost".into()];
        assert_eq!(publish(&e, u, 100).unwrap_err(), "Unknown plan: ghost");
        let mut u = update(&e);
        u.plans = (0..97)
            .map(|i| Plan {
                id: format!("plan-{i}"),
                ..trial_plan()
            })
            .collect();
        assert_eq!(publish(&e, u, 100).unwrap_err(), "At most 100 plans");
        assert_eq!(e.commercial_config().revision, before);
        assert!(e.export_snapshot().plans.is_none());

        // At the bounds; into a group published with it; listed by its sort order.
        let mut u = update(&e);
        u.groups.push(Group::pro_plus("vip", "VIP"));
        u.plans = vec![
            Plan {
                name: "x".repeat(32),
                points: 10_000_000,
                price_cny: 100_000.0,
                validity_days: 3650,
                max_devices: 10,
                concurrency: 20,
                default_group_id: "vip".into(),
                ..trial_plan()
            },
            Plan {
                id: "f".repeat(64),
                price_cny: 0.0,
                points: 1,
                validity_days: 1,
                sort_order: 50,
                ..trial_plan()
            },
        ];
        let config = publish(&e, u, 100).unwrap();
        let order: Vec<_> = config.plans.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(order[0], "trial-7d");
        assert_eq!(order[5], "f".repeat(64));
        // Published again, a plan is replaced whole.
        let mut u = update(&e);
        u.plans = vec![Plan {
            on_sale: false,
            price_cny: 0.01,
            ..trial_plan()
        }];
        let config = publish(&e, u, 101).unwrap();
        let trial = config.plans.iter().find(|p| p.id == "trial-7d").unwrap();
        assert_eq!(
            (trial.on_sale, trial.price_cny, trial.points),
            (false, 0.01, 300)
        );
        assert_eq!(config.plans.len(), 6);
    }

    /// A plan cards were issued from, from the catalog or before it, cannot be removed, only
    /// taken off sale; its cards keep the plan as it was when they were issued.
    #[test]
    fn only_a_plan_no_card_was_issued_from_can_be_removed() {
        use crate::card::Card;
        let e = serving_engine();
        e.set_master_kek(crate::crypto::MasterKek::from_bytes([7; 32]));
        let mut u = update(&e);
        u.plans = vec![trial_plan()];
        e.publish_commercial_config(u, 100).unwrap();
        let trial = e.plan("trial-7d").unwrap();
        let issued = e
            .issue_cards(&trial.template("new-tier"), 2, None, 150)
            .unwrap();
        let card = issued[0].card.clone();
        assert_eq!(card.plan, Some(trial.snapshot()));
        assert_eq!(
            (
                card.credit_total,
                card.activation_duration_secs,
                card.max_concurrency,
                card.group_id.as_str(),
                card.plan_type(),
            ),
            (300_000_000, Some(7 * 86_400), 1, "new-tier", "CUSTOM")
        );
        e.upsert_card(Card::new("legacy", "new-tier", 5_000_000_000));
        let counts = e.commercial_config().cards_by_plan;
        assert_eq!(
            (counts["trial-7d"], counts["tier-5000"], counts["tier-1000"]),
            (2, 1, 0)
        );
        let remove = |ids: &[&str]| {
            let mut u = update(&e);
            u.removed_plans = ids.iter().map(|id| id.to_string()).collect();
            publish(&e, u, 200)
        };
        for id in ["trial-7d", "tier-5000"] {
            assert_eq!(
                remove(&[id]).unwrap_err(),
                format!("Plans cards were issued from can only be taken off sale: {id}")
            );
        }
        let mut u = update(&e);
        u.plans = vec![Plan {
            name: "旧体验卡".into(),
            on_sale: false,
            ..trial.clone()
        }];
        e.publish_commercial_config(u, 200).unwrap();
        assert!(!e.plan("trial-7d").unwrap().on_sale);
        assert_eq!(e.get_card(&card.id).unwrap().plan_name(), Some("体验卡"));
        let config = remove(&["tier-10000"]).unwrap();
        assert!(!config.plans.iter().any(|p| p.id == "tier-10000"));
        assert!(!config.cards_by_plan.contains_key("tier-10000"));
        assert_eq!(config.plans.len(), 4);
    }
}
