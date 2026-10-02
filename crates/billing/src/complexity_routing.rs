//! Durable routing metadata only: no messages, prompts, or upstream credentials.
use super::*;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

const DAY: u64 = 86_400;
const RETENTION: u64 = 7 * DAY;
const MAX_DECISIONS: usize = 20_000;
const MAX_AUDIT: usize = 128;
const MAX_MONEY: u64 = 1_000_000_000_000;
const MAX_CALLS: u64 = 100_000;
const MAX_TIMESTAMP: u64 = 253_402_300_799;
const INITIAL_REVISION: &str = "complexity-routing:v1:default";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoutingMode {
    #[default]
    Off,
    Observe,
    Enforce,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Complexity {
    Simple,
    Complex,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutingClassifier {
    pub provider_id: String,
    pub model: String,
    pub timeout_ms: u64,
    pub max_input_chars: usize,
    pub daily_request_limit: u64,
    pub daily_budget_micro_cny: u64,
    pub input_price_micro_cny_per_million: u64,
    pub output_price_micro_cny_per_million: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutingPolicy {
    pub model_map_id: String,
    pub mode: RoutingMode,
    pub simple_provider_ids: Vec<String>,
    pub complex_provider_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComplexityRoutingConfig {
    pub revision: String,
    pub classifier: Option<RoutingClassifier>,
    pub policies: Vec<RoutingPolicy>,
    pub audit: Vec<ResponseTemplateAudit>,
}

impl Default for ComplexityRoutingConfig {
    fn default() -> Self {
        Self {
            revision: INITIAL_REVISION.into(),
            classifier: None,
            policies: Vec::new(),
            audit: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComplexityRoutingUpdate {
    pub expected_revision: String,
    pub reason: String,
    pub classifier: Option<RoutingClassifier>,
    pub policies: Vec<RoutingPolicy>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutingDecision {
    pub invocation_id: String,
    pub scope: String,
    pub request_hash: String,
    pub revision: String,
    pub model_map_id: String,
    pub mode: RoutingMode,
    pub complexity: Complexity,
    pub reason: String,
    /// Frozen suggested chain. Observe mode records suggestions but does not apply them.
    pub provider_ids: Vec<String>,
    pub created_at_secs: u64,
    pub pending: bool,
    pub classifier_attempted: bool,
    pub classifier_latency_ms: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub classifier_cost_micro_cny: u64,
    pub usage_estimated: bool,
    #[serde(default)]
    pub preview: bool,
    #[serde(default)]
    pub served_provider_id: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutingBudget {
    /// UTC Unix day, shared by normal requests and administrator previews.
    pub day: u64,
    pub calls: u64,
    pub cost_micro_cny: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComplexityRoutingState {
    pub config: ComplexityRoutingConfig,
    pub decisions: Vec<RoutingDecision>,
    pub budget: RoutingBudget,
}

fn invalid(message: &str) -> BillingError {
    BillingError::InvalidState(format!("complexity routing: {message}"))
}

fn validate_chain(ids: &[String]) -> Result<(), BillingError> {
    let mut seen = HashSet::new();
    if ids.is_empty()
        || ids.len() > 16
        || ids
            .iter()
            .any(|id| !crate::valid_id(id, 128) || !seen.insert(id))
    {
        return Err(invalid("chains require 1..16 distinct provider IDs"));
    }
    Ok(())
}

fn validate_config(config: &ComplexityRoutingConfig) -> Result<(), BillingError> {
    if !crate::valid_id(&config.revision, 128)
        || !config.revision.starts_with("complexity-routing:v1:")
        || config.policies.len() > 256
        || config.audit.len() > MAX_AUDIT
    {
        return Err(invalid("invalid configuration bounds or revision"));
    }
    if let Some(c) = &config.classifier {
        if !crate::valid_id(&c.provider_id, 128)
            || !crate::group::valid_model_id(&c.model)
            || !(200..=5000).contains(&c.timeout_ms)
            || !(256..=16000).contains(&c.max_input_chars)
            || !(1..=MAX_CALLS).contains(&c.daily_request_limit)
            || !(1..=MAX_MONEY).contains(&c.daily_budget_micro_cny)
            || !(1..=MAX_MONEY).contains(&c.input_price_micro_cny_per_million)
            || !(1..=MAX_MONEY).contains(&c.output_price_micro_cny_per_million)
        {
            return Err(invalid("invalid classifier IDs, limits, budget or prices"));
        }
    }
    let mut seen = HashSet::new();
    for policy in &config.policies {
        if !crate::valid_id(&policy.model_map_id, 128) || !seen.insert(&policy.model_map_id) {
            return Err(invalid("invalid or duplicate policy model map ID"));
        }
        if policy.mode != RoutingMode::Off && config.classifier.is_none() {
            return Err(invalid("enabled policies require a classifier"));
        }
        validate_chain(&policy.simple_provider_ids)?;
        validate_chain(&policy.complex_provider_ids)?;
    }
    if config.audit.is_empty() {
        if config != &ComplexityRoutingConfig::default() {
            return Err(invalid("missing configuration audit"));
        }
    } else if config.audit.last().unwrap().revision != config.revision {
        return Err(invalid("configuration audit revision mismatch"));
    }
    for (i, audit) in config.audit.iter().enumerate() {
        if !crate::valid_id(&audit.reason, 1024)
            || !crate::valid_id(&audit.revision, 128)
            || !crate::valid_id(&audit.previous_revision, 128)
            || !audit.revision.starts_with("complexity-routing:v1:")
            || !audit
                .previous_revision
                .starts_with("complexity-routing:v1:")
            || audit.created_at_secs > MAX_TIMESTAMP
            || (i > 0 && config.audit[i - 1].revision != audit.previous_revision)
        {
            return Err(invalid("invalid configuration audit"));
        }
    }
    Ok(())
}

/// Publication checks live references; restore checks only structure, since providers,
/// groups and model maps can legitimately have been removed after a decision was made.
fn validate_references(snapshot: &BillingSnapshot) -> Result<(), BillingError> {
    let config = &snapshot.complexity_routing.config;
    // Emergency shutdown must remain possible after a referenced channel/model is removed.
    // Structural validation still applies; re-enabling always validates live references.
    if config.policies.iter().all(|p| p.mode == RoutingMode::Off) {
        return Ok(());
    }
    let classifier = config
        .classifier
        .as_ref()
        .map(|c| {
            snapshot
                .providers
                .get(&c.provider_id)
                .filter(|p| p.enabled)
                .ok_or_else(|| invalid("classifier provider missing or disabled"))
        })
        .transpose()?;
    for policy in &config.policies {
        let maps: Vec<_> = snapshot
            .model_maps
            .iter()
            .filter(|m| m.id == policy.model_map_id)
            .collect();
        if maps.len() != 1 || maps[0].retired {
            return Err(invalid(
                "policy requires one existing, non-retired model map",
            ));
        }
        let model = maps[0];
        let group = snapshot
            .groups
            .get(&model.group_id)
            .ok_or_else(|| invalid("policy group missing"))?;
        if classifier.is_some_and(|p| !p.can_access(group)) {
            return Err(invalid("classifier provider cannot access policy group"));
        }
        let mut targets = HashMap::new();
        for target in model.full_target_chain() {
            if targets
                .insert(target.provider_id, target.target_model.clone())
                .is_some_and(|old| old != target.target_model)
            {
                return Err(invalid("model map has ambiguous targets for one provider"));
            }
        }
        for id in policy
            .simple_provider_ids
            .iter()
            .chain(&policy.complex_provider_ids)
        {
            if !targets.contains_key(id) {
                return Err(invalid("policy provider is outside the model target chain"));
            }
            if !snapshot
                .providers
                .get(id)
                .is_some_and(|p| p.enabled && p.can_access(group))
            {
                return Err(invalid(
                    "policy provider missing, disabled or inaccessible to group",
                ));
            }
        }
    }
    Ok(())
}

// Continuations can retain or narrow a historical policy chain, but never bypass
// today's model membership or group access. Do not require equality with today's policy.
fn validate_decision_references(
    snapshot: &BillingSnapshot,
    d: &RoutingDecision,
) -> Result<(), BillingError> {
    let model = snapshot
        .model_maps
        .iter()
        .find(|m| m.id == d.model_map_id && !m.retired)
        .ok_or_else(|| invalid("decision model map missing"))?;
    let group = snapshot
        .groups
        .get(&model.group_id)
        .ok_or_else(|| invalid("decision group missing"))?;
    let targets = model.full_target_chain();
    for id in &d.provider_ids {
        if !targets.iter().any(|t| t.provider_id == *id)
            || !snapshot
                .providers
                .get(id)
                .is_some_and(|p| p.enabled && p.can_access(group))
        {
            return Err(invalid(
                "decision provider is outside the accessible model chain",
            ));
        }
    }
    Ok(())
}

fn validate_decision(d: &RoutingDecision) -> Result<(), BillingError> {
    if !crate::valid_id(&d.invocation_id, crate::MAX_INVOCATION_KEY_BYTES)
        || !crate::valid_id(&d.scope, 512)
        || !crate::valid_id(&d.request_hash, 128)
        || !crate::valid_id(&d.revision, 128)
        || !d.revision.starts_with("complexity-routing:v1:")
        || !crate::valid_id(&d.model_map_id, 128)
        || !crate::valid_id(&d.reason, 1024)
        || d.created_at_secs > MAX_TIMESTAMP
        || d.classifier_latency_ms > 300_000
        || d.input_tokens > 1_000_000
        || d.output_tokens > 1_000_000
        || d.classifier_cost_micro_cny > MAX_MONEY
        || (d.pending && !d.classifier_attempted)
        || (!d.classifier_attempted
            && (d.classifier_cost_micro_cny != 0
                || d.input_tokens != 0
                || d.output_tokens != 0
                || d.classifier_latency_ms != 0
                || d.usage_estimated))
        || d.served_provider_id.as_ref().is_some_and(|id| {
            d.pending
                || !crate::valid_id(id, 128)
                || (d.mode != RoutingMode::Observe && !d.provider_ids.contains(id))
        })
    {
        return Err(invalid("invalid decision metadata or bounds"));
    }
    if d.provider_ids.is_empty() && !d.pending && !d.classifier_attempted {
        Ok(())
    } else {
        validate_chain(&d.provider_ids)
    }
}

pub(super) fn validate_snapshot_state(state: &ComplexityRoutingState) -> Result<(), BillingError> {
    validate_config(&state.config)?;
    if state.decisions.len() > MAX_DECISIONS
        || state.budget.day > MAX_TIMESTAMP / DAY
        || state.budget.calls > MAX_CALLS
        || state.budget.cost_micro_cny > state.budget.calls.saturating_mul(MAX_MONEY)
    {
        return Err(invalid("invalid state capacity or budget bounds"));
    }
    let mut seen = HashSet::new();
    let mut calls = 0u64;
    let mut cost = 0u64;
    for decision in &state.decisions {
        validate_decision(decision)?;
        if !seen.insert(&decision.invocation_id) {
            return Err(invalid("duplicate decision invocation ID"));
        }
        if decision.created_at_secs / DAY == state.budget.day && decision.classifier_attempted {
            calls += 1;
            cost += decision.classifier_cost_micro_cny;
        }
    }
    if calls > state.budget.calls || cost > state.budget.cost_micro_cny {
        return Err(invalid(
            "budget does not cover retained classifier decisions",
        ));
    }
    Ok(())
}

fn prune(state: &mut ComplexityRoutingState, now: u64) {
    // Pending calls expire too; do not refund them or retry an uncertain external call.
    state
        .decisions
        .retain(|d| now.saturating_sub(d.created_at_secs) <= RETENTION);
}

impl BillingEngine {
    pub fn complexity_routing_config(&self) -> ComplexityRoutingConfig {
        let _guard = self.state_lock.read().unwrap();
        self.complexity_routing.read().unwrap().config.clone()
    }

    /// Metadata only. Admin callers should project aggregates and a small recent slice.
    pub fn routing_status(&self) -> ComplexityRoutingState {
        let _guard = self.state_lock.read().unwrap();
        self.complexity_routing.read().unwrap().clone()
    }

    pub fn publish_complexity_routing(
        &self,
        update: ComplexityRoutingUpdate,
        now: u64,
    ) -> Result<ComplexityRoutingConfig, BillingError> {
        if !crate::valid_id(&update.expected_revision, 128)
            || !crate::valid_id(&update.reason, 1024)
            || now > MAX_TIMESTAMP
        {
            return Err(invalid("revision and reason required and bounded"));
        }
        let _guard = self.state_lock.write().unwrap();
        let mut candidate = self.export_snapshot_locked(
            self.snapshot_sequence
                .load(Ordering::Acquire)
                .saturating_add(1),
            self.last_snapshot_checksum.read().unwrap().clone(),
        );
        if candidate.complexity_routing.config.revision != update.expected_revision {
            return Err(invalid("configuration changed; reload before publishing"));
        }
        let bytes = serde_json::to_vec(&(&update, now))
            .map_err(|_| invalid("cannot encode configuration"))?;
        let digest: String = ring::digest::digest(&ring::digest::SHA256, &bytes)
            .as_ref()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let config = &mut candidate.complexity_routing.config;
        config.revision = format!("complexity-routing:v1:{digest}");
        config.audit.push(ResponseTemplateAudit {
            previous_revision: update.expected_revision,
            revision: config.revision.clone(),
            reason: update.reason,
            created_at_secs: now,
        });
        if config.audit.len() > MAX_AUDIT {
            config.audit.remove(0);
        }
        config.classifier = update.classifier;
        config.policies = update.policies;
        validate_config(config)?;
        validate_references(&candidate)?;
        prune(&mut candidate.complexity_routing, now);
        self.commit_candidate_snapshot(&candidate, || {
            *self.complexity_routing.write().unwrap() = candidate.complexity_routing.clone();
        })?;
        Ok(candidate.complexity_routing.config)
    }

    pub fn routing_decision(&self, invocation_id: &str) -> Option<RoutingDecision> {
        let _guard = self.state_lock.read().unwrap();
        // ponytail: linear lookup bounded at 20k records; add an index only if profiling needs it.
        self.complexity_routing
            .read()
            .unwrap()
            .decisions
            .iter()
            .find(|d| d.invocation_id == invocation_id)
            .cloned()
    }

    /// Previews and uncertain in-flight classifications cannot steer user tool continuations.
    pub fn latest_routing_decision(&self, scope: &str, now: u64) -> Option<RoutingDecision> {
        let _guard = self.state_lock.read().unwrap();
        self.complexity_routing
            .read()
            .unwrap()
            .decisions
            .iter()
            .filter(|d| {
                d.scope == scope
                    && !d.pending
                    && !d.preview
                    && now
                        .checked_sub(d.created_at_secs)
                        .is_some_and(|age| age < DAY)
            })
            .max_by_key(|d| d.created_at_secs)
            .cloned()
    }

    /// Compatibility wrapper. External callers must use the claim-returning method
    /// below: an existing pending decision alone is NOT permission for another call.
    pub fn begin_routing_decision(
        &self,
        decision: RoutingDecision,
        reserved_cost: u64,
        now: u64,
    ) -> Result<RoutingDecision, BillingError> {
        self.begin_routing_decision_with_claim(decision, reserved_cost, now)
            .map(|(decision, _)| decision)
    }

    /// True only for this call's newly persisted classifier reservation. Replays, even
    /// of pending records after restart, return false and MUST NOT call the classifier.
    pub fn begin_routing_decision_with_claim(
        &self,
        mut decision: RoutingDecision,
        reserved_cost: u64,
        now: u64,
    ) -> Result<(RoutingDecision, bool), BillingError> {
        let _guard = self.state_lock.write().unwrap();
        {
            let state = self.complexity_routing.read().unwrap();
            if let Some(old) = state.decisions.iter().find(|d| {
                d.invocation_id == decision.invocation_id
                    && now.saturating_sub(d.created_at_secs) <= RETENTION
            }) {
                if old.scope != decision.scope
                    || old.request_hash != decision.request_hash
                    || old.model_map_id != decision.model_map_id
                    || old.preview != decision.preview
                {
                    return Err(invalid("invocation identity mismatch"));
                }
                return Ok((old.clone(), false));
            }
        }
        if now > MAX_TIMESTAMP
            || decision.served_provider_id.is_some()
            || decision.input_tokens != 0
            || decision.output_tokens != 0
            || decision.classifier_latency_ms != 0
            || (!decision.pending
                && (decision.classifier_attempted
                    || reserved_cost != 0
                    || decision.classifier_cost_micro_cny != 0
                    || decision.usage_estimated))
            || (decision.pending
                && decision.classifier_cost_micro_cny != 0
                && decision.classifier_cost_micro_cny != reserved_cost)
        {
            return Err(invalid(
                "new decisions cannot carry prior usage or actual provider",
            ));
        }
        let mut candidate = self.export_snapshot_locked(
            self.snapshot_sequence
                .load(Ordering::Acquire)
                .saturating_add(1),
            self.last_snapshot_checksum.read().unwrap().clone(),
        );
        if decision.revision != candidate.complexity_routing.config.revision {
            return Err(invalid(
                "configuration changed; reload before beginning decision",
            ));
        }
        prune(&mut candidate.complexity_routing, now);
        decision.created_at_secs = now;
        validate_decision_references(&candidate, &decision)?;
        if decision.pending {
            // Publication validates every policy. Runtime must isolate unrelated stale
            // mappings/providers and validate only this decision's classifier and policy.
            let state = &mut candidate.complexity_routing;
            let classifier = state
                .config
                .classifier
                .as_ref()
                .ok_or_else(|| invalid("classifier is not configured"))?;
            if !decision.preview
                && !state.config.policies.iter().any(|p| {
                    p.model_map_id == decision.model_map_id
                        && p.mode == decision.mode
                        && p.mode != RoutingMode::Off
                })
            {
                return Err(invalid(
                    "pending user classification requires an enabled matching policy",
                ));
            }
            // Preview also requires access to the named model's group, even without a policy.
            let model = candidate
                .model_maps
                .iter()
                .find(|m| m.id == decision.model_map_id && !m.retired)
                .ok_or_else(|| invalid("decision model map missing"))?;
            let group = candidate
                .groups
                .get(&model.group_id)
                .ok_or_else(|| invalid("decision group missing"))?;
            let provider = candidate
                .providers
                .get(&classifier.provider_id)
                .filter(|p| p.enabled)
                .ok_or_else(|| invalid("classifier provider missing or disabled"))?;
            if !provider.can_access(group) {
                return Err(invalid("classifier cannot access decision group"));
            }
            if !(1..=MAX_MONEY).contains(&reserved_cost) || state.budget.day > now / DAY {
                return Err(invalid(
                    "invalid reservation or clock moved to an earlier budget day",
                ));
            }
            decision.classifier_attempted = true;
            decision.classifier_cost_micro_cny = reserved_cost;
            decision.usage_estimated = true;
            validate_decision(&decision)?;
            if state.budget.day != now / DAY {
                state.budget = RoutingBudget {
                    day: now / DAY,
                    ..RoutingBudget::default()
                };
            }
            if state.budget.calls >= classifier.daily_request_limit
                || reserved_cost
                    > classifier
                        .daily_budget_micro_cny
                        .saturating_sub(state.budget.cost_micro_cny)
            {
                decision.pending = false;
                decision.classifier_attempted = false;
                decision.reason = "budget_exhausted".into();
                decision.classifier_cost_micro_cny = 0;
                decision.usage_estimated = false;
            } else {
                state.budget.calls += 1;
                state.budget.cost_micro_cny += reserved_cost;
            }
        }
        validate_decision(&decision)?;
        if candidate.complexity_routing.decisions.len() >= MAX_DECISIONS {
            // Keep retained invocations and the caller's eligible conservative chain,
            // including an empty no-route decision. No claim means no classifier call.
            // Budget changes above affect only this discarded candidate snapshot.
            decision.reason = "decision_capacity_exhausted".into();
            decision.pending = false;
            decision.classifier_attempted = false;
            decision.classifier_latency_ms = 0;
            decision.input_tokens = 0;
            decision.output_tokens = 0;
            decision.classifier_cost_micro_cny = 0;
            decision.usage_estimated = false;
            return Ok((decision, false));
        }
        candidate
            .complexity_routing
            .decisions
            .push(decision.clone());
        self.commit_candidate_snapshot(&candidate, || {
            *self.complexity_routing.write().unwrap() = candidate.complexity_routing.clone();
        })?;
        let claimed = decision.pending;
        Ok((decision, claimed))
    }

    /// Unknown means the classifier failed or was inconclusive: keep at least its
    /// reservation. Final decisions are immutable, and late finishes never debit a new day.
    pub fn finish_routing_decision(
        &self,
        mut decision: RoutingDecision,
    ) -> Result<RoutingDecision, BillingError> {
        let _guard = self.state_lock.write().unwrap();
        let mut candidate = self.export_snapshot_locked(
            self.snapshot_sequence
                .load(Ordering::Acquire)
                .saturating_add(1),
            self.last_snapshot_checksum.read().unwrap().clone(),
        );
        let state = &mut candidate.complexity_routing;
        let old = state
            .decisions
            .iter_mut()
            .find(|d| d.invocation_id == decision.invocation_id)
            .ok_or_else(|| invalid("decision not found"))?;
        if old.scope != decision.scope
            || old.request_hash != decision.request_hash
            || old.revision != decision.revision
            || old.model_map_id != decision.model_map_id
            || old.mode != decision.mode
            || old.created_at_secs != decision.created_at_secs
            || old.preview != decision.preview
            || old.served_provider_id != decision.served_provider_id
        {
            return Err(invalid("final decision identity mismatch"));
        }
        if !old.pending {
            return Ok(old.clone());
        }
        decision.pending = false;
        decision.classifier_attempted = true;
        if decision.complexity == Complexity::Unknown
            && decision.classifier_cost_micro_cny < old.classifier_cost_micro_cny
        {
            decision.classifier_cost_micro_cny = old.classifier_cost_micro_cny;
            decision.usage_estimated = true;
        }
        validate_decision(&decision)?;
        if state.budget.day == old.created_at_secs / DAY {
            state.budget.cost_micro_cny = state
                .budget
                .cost_micro_cny
                .saturating_sub(old.classifier_cost_micro_cny)
                .checked_add(decision.classifier_cost_micro_cny)
                .ok_or_else(|| invalid("budget cost overflow"))?;
        }
        *old = decision.clone();
        self.commit_candidate_snapshot(&candidate, || {
            *self.complexity_routing.write().unwrap() = candidate.complexity_routing.clone();
        })?;
        Ok(decision)
    }

    /// Record only a confirmed actual upstream. Never infer it from a suggestion or a
    /// stream committed before upstream identity is known. Identical repeats are harmless.
    pub fn record_routing_served(
        &self,
        invocation_id: &str,
        provider_id: &str,
    ) -> Result<(), BillingError> {
        let _guard = self.state_lock.write().unwrap();
        let index = {
            let state = self.complexity_routing.read().unwrap();
            let Some(index) = state
                .decisions
                .iter()
                .position(|d| d.invocation_id == invocation_id)
            else {
                // Ordinary requests have no routing decision. Do not snapshot or save them.
                return Ok(());
            };
            let decision = &state.decisions[index];
            if let Some(old) = &decision.served_provider_id {
                return if old == provider_id {
                    Ok(())
                } else {
                    Err(invalid("actual provider is already recorded"))
                };
            }
            if decision.pending || !crate::valid_id(provider_id, 128) {
                return Err(invalid(
                    "actual provider requires a finalized decision and valid ID",
                ));
            }
            index
        };
        let mut candidate = self.export_snapshot_locked(
            self.snapshot_sequence
                .load(Ordering::Acquire)
                .saturating_add(1),
            self.last_snapshot_checksum.read().unwrap().clone(),
        );
        let decision = &candidate.complexity_routing.decisions[index];
        if decision.mode == RoutingMode::Observe {
            let model = candidate
                .model_maps
                .iter()
                .find(|m| m.id == decision.model_map_id && !m.retired)
                .ok_or_else(|| invalid("observe model map missing"))?;
            let group = candidate
                .groups
                .get(&model.group_id)
                .ok_or_else(|| invalid("observe group missing"))?;
            if !model
                .full_target_chain()
                .iter()
                .any(|t| t.provider_id == provider_id)
                || !candidate
                    .providers
                    .get(provider_id)
                    .is_some_and(|p| p.enabled && p.can_access(group))
            {
                return Err(invalid(
                    "observe actual provider is outside the accessible model chain",
                ));
            }
        } else if !decision.provider_ids.iter().any(|id| id == provider_id) {
            return Err(invalid(
                "actual provider is outside the frozen decision chain",
            ));
        }
        candidate.complexity_routing.decisions[index].served_provider_id = Some(provider_id.into());
        self.commit_candidate_snapshot(&candidate, || {
            *self.complexity_routing.write().unwrap() = candidate.complexity_routing.clone();
        })
    }
}
