//! Observability, metrics aggregation, gross margin reporting, and anomaly detection.
//!
//! Spec §5 (Data Model) & Spec §14.4 (Observability & Financial Reconciliation).

use crate::ledger::{EarnedCredits, LedgerEntry};
use crate::rate_card::BillingSettings;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Request execution status (Spec §5, §14.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TraceStatus {
    #[default]
    Success,
    Error,
    ClientAborted,
    InProgress,
}

/// What happened over one period: requests by outcome from the traces, and what was
/// billed for them from the ledger.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityWindow {
    pub requests: u64,
    pub succeeded: u64,
    pub failed: u64,
    pub client_aborted: u64,
    /// Requests refused for the card or the request itself (see [`CARD_REFUSALS`]),
    /// counted apart: neither requests nor failed include them.
    pub refused: u64,
    pub credits_charged: i64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub provider_cost_micro_cny: i64,
    /// Cards billed at least once in the period.
    pub active_cards: u64,
    /// Finished requests whose time to first output was measured.
    pub timed_requests: u64,
    pub ttft_median_ms: Option<u32>,
    pub ttft_p90_ms: Option<u32>,
}

/// One provider's requests over the last 24 hours.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderActivity {
    pub provider_id: String,
    pub requests: u64,
    pub failed: u64,
    /// Requests it refused for the request itself, such as a prompt too long.
    pub refused: u64,
    pub ttft_median_ms: Option<u32>,
}

/// Requests in one clock hour.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityHour {
    pub start_secs: u64,
    pub requests: u64,
    pub failed: u64,
    pub refused: u64,
}

/// The last 24 hours and 7 days, and the last 24 clock hours one by one.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Activity {
    #[serde(rename = "last24h")]
    pub last_24h: ActivityWindow,
    #[serde(rename = "last7d")]
    pub last_7d: ActivityWindow,
    /// Oldest first; the last is the current, unfinished hour.
    pub hourly: Vec<ActivityHour>,
    /// The last 24 hours by provider, busiest first.
    pub providers: Vec<ProviderActivity>,
    /// The oldest trace kept: request counts reach back no further than this.
    pub traces_cover_from_secs: Option<u64>,
    /// Every upstream attempt by provider, from each request's attempt chain, busiest
    /// first: a provider that fails over to a backup shows its failures, though the backup
    /// answered.
    #[serde(rename = "providerAttempts")]
    pub provider_attempts: Vec<ProviderAttempts>,
    /// The same by Key.
    #[serde(rename = "keyAttempts")]
    pub key_attempts: Vec<KeyAttempts>,
    /// Requests by the model the customer asked for, busiest first.
    #[serde(rename = "modelHealth")]
    pub model_health: Vec<ModelHealth>,
    /// Billed requests and the cards that made them by customer model over the last 7 days,
    /// from the ledger, busiest first.
    #[serde(rename = "modelUsage7d")]
    pub model_usage_7d: Vec<ModelUsage>,
}

/// Error classes of requests refused for the card or the request itself: the card's
/// balance or limits, a prompt too long for the model, a capability the model lacks, or a
/// model the card may not or cannot name. They say nothing about a model's or an
/// upstream's health, and are counted apart from requests and failures.
pub const CARD_REFUSALS: [&str; 8] = [
    "insufficient_balance",
    "concurrency_limit",
    "usage_limit",
    "input_too_long",
    "unsupported_capability",
    "invalid_model",
    "model_not_listed",
    "model_retired",
];

/// Upstream attempts over one period.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttemptWindow {
    pub attempts: u64,
    pub failures: u64,
    /// Attempts the upstream rightly refused for the request itself, such as a prompt too
    /// long: neither attempts nor failures include them.
    pub refused: u64,
    /// Failed attempts after which another provider answered the request.
    pub taken_over: u64,
    /// Failures by the kind attempt chains name: http_429, http_401, timeout, transport,
    /// upstream_service, protocol, empty.
    pub failures_by_kind: BTreeMap<String, u64>,
}

impl AttemptWindow {
    pub(crate) fn count(&mut self, attempt: &AttemptRecord, taken_over: bool, refused: bool) {
        if refused {
            self.refused += 1;
            return;
        }
        self.attempts += 1;
        if !attempt.success {
            self.failures += 1;
            self.taken_over += u64::from(taken_over);
            let kind = attempt.error.as_deref().unwrap_or("unknown");
            *self.failures_by_kind.entry(kind.to_string()).or_default() += 1;
        }
    }
}

/// One provider's attempts over the last hour, 24 hours and 7 days.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderAttempts {
    pub provider_id: String,
    #[serde(rename = "last1h")]
    pub last_1h: AttemptWindow,
    #[serde(rename = "last24h")]
    pub last_24h: AttemptWindow,
    #[serde(rename = "last7d")]
    pub last_7d: AttemptWindow,
}

/// One Key's attempts over the last hour, 24 hours and 7 days.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyAttempts {
    pub key_id: String,
    pub provider_id: String,
    #[serde(rename = "last1h")]
    pub last_1h: AttemptWindow,
    #[serde(rename = "last24h")]
    pub last_24h: AttemptWindow,
    #[serde(rename = "last7d")]
    pub last_7d: AttemptWindow,
}

/// Finished requests for one customer model over one period. Refusals for the card or the
/// request itself are counted apart, as refused.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelHealthWindow {
    pub requests: u64,
    pub failures: u64,
    pub refused: u64,
    pub last_failure_at: Option<u64>,
    /// The commonest kind of failure, the earliest in name on a tie.
    pub top_failure_kind: Option<String>,
    /// Failures by kind: the request's error class, else its last failed attempt's kind.
    pub failures_by_kind: BTreeMap<String, u64>,
}

impl ModelHealthWindow {
    /// Counts a trace's requests: `times` of them, the last at `ts`.
    pub(crate) fn count(&mut self, ts: u64, failure: Option<&str>, times: u64) {
        self.requests += times;
        if let Some(kind) = failure {
            self.failures += times;
            self.last_failure_at = self.last_failure_at.max(Some(ts));
            *self.failures_by_kind.entry(kind.to_string()).or_default() += times;
            self.top_failure_kind = self
                .failures_by_kind
                .iter()
                .max_by(|a, b| a.1.cmp(b.1).then_with(|| b.0.cmp(a.0)))
                .map(|(kind, _)| kind.clone());
        }
    }
}

/// One customer model's requests over the last hour, 24 hours and 7 days.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelHealth {
    pub model: String,
    #[serde(rename = "last1h")]
    pub last_1h: ModelHealthWindow,
    #[serde(rename = "last24h")]
    pub last_24h: ModelHealthWindow,
    #[serde(rename = "last7d")]
    pub last_7d: ModelHealthWindow,
}

/// One customer model's billed use over the last 7 days.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelUsage {
    pub model: String,
    pub requests: u64,
    /// Distinct cards billed for it.
    pub cards: u64,
}

/// Single failover or execution attempt record within a request trace (Spec §5, §14.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptRecord {
    pub key_id: String,
    pub provider_id: String,
    pub success: bool,
    pub error: Option<String>,
    pub latency_ms: u64,
}

/// Request execution trace (Spec §5, §14.4).
///
/// Strictly captures metrics, tokens, costs, and attempt chains without conversation text.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RequestTrace {
    pub id: String,
    pub card_id: String,
    pub ts: u64,
    pub invocation_id: String,
    pub exposed_model: String,
    pub status: TraceStatus,
    pub ttft_ms: Option<u32>,
    pub tokens_per_second: Option<f64>,
    pub error_class: Option<String>,
    pub provider_id: Option<String>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub credits_charged: i64,
    pub provider_cost_micro_cny: i64,
    /// For a request refused for want of balance: the micro-credits it needed to start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub needed_micro_credits: Option<i64>,
    /// And the micro-credits the card had.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub available_micro_credits: Option<i64>,
    pub attempt_chain: Vec<AttemptRecord>,
    /// For a refusal: how many more times the card was refused for the same reason within
    /// a minute of it. They add no trace of their own; this one counts them.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub repeats: u64,
    /// When the last of those was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_seen_secs: Option<u64>,
}

fn is_zero(value: &u64) -> bool {
    *value == 0
}

impl RequestTrace {
    /// The requests a trace stands for: its own and the repeated refusals it counts.
    pub fn occurrences(&self) -> u64 {
        self.repeats.saturating_add(1)
    }

    /// When the last request it stands for was made.
    pub fn last_seen(&self) -> u64 {
        self.last_seen_secs.unwrap_or(self.ts).max(self.ts)
    }

    /// Whether it records a request refused for the card or the request itself, one of
    /// [`CARD_REFUSALS`], which says nothing about a model's or an upstream's health.
    pub fn refused_for_card(&self) -> bool {
        self.status == TraceStatus::Error
            && self
                .error_class
                .as_deref()
                .is_some_and(|class| CARD_REFUSALS.contains(&class))
    }

    /// Whether `later` is the same refusal again: the same card, refused before any
    /// upstream was called for the same reason, within a minute of this one.
    pub(crate) fn is_repeated_by(&self, later: &RequestTrace) -> bool {
        const REPEAT_WINDOW_SECS: u64 = 60;
        let refusal = |trace: &RequestTrace| {
            trace.status == TraceStatus::Error
                && trace.error_class.is_some()
                && trace.attempt_chain.is_empty()
                && trace.credits_charged == 0
        };
        refusal(self)
            && refusal(later)
            && self.card_id == later.card_id
            && self.error_class == later.error_class
            && later.ts >= self.ts
            && later.ts < self.ts.saturating_add(REPEAT_WINDOW_SECS)
    }
}

/// Which traces a search keeps; all of them when nothing is set.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TraceFilter {
    /// From this time on.
    pub from_secs: Option<u64>,
    /// Before this time.
    pub to_secs: Option<u64>,
    pub card_id: Option<String>,
    /// The model the customer asked for.
    pub model: Option<String>,
    /// The provider that answered, or any provider an attempt was made to.
    pub provider: Option<String>,
    pub status: Option<TraceStatus>,
}

impl TraceFilter {
    pub fn matches(&self, trace: &RequestTrace) -> bool {
        self.from_secs.is_none_or(|from| trace.ts >= from)
            && self.to_secs.is_none_or(|to| trace.ts < to)
            && self.card_id.as_ref().is_none_or(|id| trace.card_id == *id)
            && self
                .model
                .as_ref()
                .is_none_or(|model| trace.exposed_model == *model)
            && self.provider.as_ref().is_none_or(|provider| {
                trace.provider_id.as_ref() == Some(provider)
                    || trace
                        .attempt_chain
                        .iter()
                        .any(|attempt| attempt.provider_id == *provider)
            })
            && self.status.is_none_or(|status| trace.status == status)
    }
}

/// Totals over every trace a search matched, not only those returned.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceTotals {
    pub count: u64,
    pub failures: u64,
    pub credits_charged: i64,
    pub cost_micro_cny: i64,
}

/// Daily aggregated usage summary (Spec §14.4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct DailyUsageSummary {
    pub date_str: String,
    pub requests_count: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
    pub credits_charged: i64,
    pub provider_cost_micro_cny: i64,
}

/// Cost vs Revenue Gross Margin Dashboard (Spec §14.4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct MarginDashboard {
    pub total_requests: u64,
    pub total_credits_charged: i64,
    pub revenue_micro_cny: i64,
    pub provider_cost_micro_cny: i64,
    pub gross_profit_micro_cny: i64,
    pub gross_margin_percentage: f64,
}

/// Model ranking by consumption and cost (Spec §14.4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelCostRanking {
    pub model_id: String,
    pub requests: u64,
    pub total_tokens: u64,
    pub provider_cost_micro_cny: i64,
    pub credits_charged: i64,
    pub margin_percentage: f64,
}

/// Provider health metrics summary (Spec §14.4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ProviderHealthSummary {
    pub provider_id: String,
    pub total_requests: u64,
    pub success_requests: u64,
    pub error_requests: u64,
    pub success_rate: f64,
    pub avg_ttft_ms: Option<f64>,
    pub avg_tokens_per_second: Option<f64>,
}

/// Actions taken when an anomaly is detected (Spec §14.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnomalyAction {
    AlertOnly,
    ThrottleApplied,
    AutoFrozen,
}

/// Usage anomaly alert for sudden traffic/credit spikes (Spec §14.4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnomalyAlert {
    pub card_id: String,
    pub window_secs: u64,
    pub credits_used_in_window: i64,
    pub threshold_credits: i64,
    pub action_taken: AnomalyAction,
    pub detected_at: u64,
}

/// Platform announcements and degradation notices (Spec §14.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnnouncementLevel {
    #[default]
    Info,
    Warning,
    Critical,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Announcement {
    pub id: String,
    pub title: String,
    pub content: String,
    pub level: AnnouncementLevel,
    pub enabled: bool,
    pub created_at: u64,
    pub expires_at: Option<u64>,
    /// When it is first shown. Unset, it is shown from when it was published. Older releases
    /// ignore it and show a scheduled announcement at once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub starts_at: Option<u64>,
    /// The groups whose cards are shown it; every customer when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub audience: Vec<String>,
    /// Its edits, oldest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub edits: Vec<AnnouncementEdit>,
}

/// Who changed a published announcement, when, and which of its fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnnouncementEdit {
    pub operator: String,
    pub at_secs: u64,
    /// Of title, content, level, starts_at, expires_at and audience.
    pub changed: Vec<String>,
}

impl Announcement {
    pub fn new(
        id: impl Into<String>,
        title: impl Into<String>,
        content: impl Into<String>,
        level: AnnouncementLevel,
        now_secs: u64,
    ) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            content: content.into(),
            level,
            enabled: true,
            created_at: now_secs,
            expires_at: None,
            starts_at: None,
            audience: Vec::new(),
            edits: Vec::new(),
        }
    }

    pub fn with_expiry(mut self, expires_at: u64) -> Self {
        self.expires_at = Some(expires_at);
        self
    }

    pub fn with_start(mut self, starts_at: u64) -> Self {
        self.starts_at = Some(starts_at);
        self
    }

    pub fn with_audience(mut self, group_ids: Vec<String>) -> Self {
        self.audience = group_ids;
        self
    }

    /// When it is first shown: its start, or when it was published.
    pub fn start_secs(&self) -> u64 {
        self.starts_at.unwrap_or(self.created_at)
    }

    /// Shown now: not withdrawn, and its window, from its start up to its end, holds `now_secs`.
    pub fn is_active(&self, now_secs: u64) -> bool {
        self.enabled
            && self.starts_at.is_none_or(|start| now_secs >= start)
            && self.expires_at.is_none_or(|end| now_secs < end)
    }

    /// Whether a card of `group_id` is shown it; a caller of no known group sees only what is
    /// shown to everyone.
    pub fn is_for(&self, group_id: Option<&str>) -> bool {
        self.audience.is_empty()
            || group_id.is_some_and(|group| self.audience.iter().any(|id| id == group))
    }
}

/// Compute gross margin dashboard from ledger entries and global settings (Spec §14.4).
pub fn compute_margin_dashboard(
    entries: &[LedgerEntry],
    settings: &BillingSettings,
) -> MarginDashboard {
    let mut total_requests = 0u64;
    let mut total_credits_charged = 0i64;
    let mut provider_cost_micro_cny = 0i64;
    let mut earned = EarnedCredits::default();

    for entry in entries {
        if entry.kind == crate::ledger::LedgerKind::Usage {
            // Saturating: release builds wrap on overflow, and one stored entry can
            // carry an extreme cost from before settlement was bounded.
            total_requests += 1;
            total_credits_charged = total_credits_charged.saturating_add(entry.credits_charged);
            provider_cost_micro_cny =
                provider_cost_micro_cny.saturating_add(entry.provider_cost_micro_cny);
            earned.add(entry_face_value(entry, settings), entry.credits_charged);
        }
    }

    // Revenue in micro-CNY: (credits_charged * credit_face_value_cny)
    // 1 credit = 1_000_000 micro-credits.
    // revenue_micro_cny = (credits_charged / 1_000_000) * (credit_face_value_cny * 1_000_000)
    //                   = credits_charged * credit_face_value_cny
    // Each entry's credits count at the face value they were earned at, so a later change
    // of it does not restate past revenue; an entry that records none, at the current one.
    let revenue_micro_cny = earned.revenue_micro_cny();
    let gross_profit_micro_cny = revenue_micro_cny.saturating_sub(provider_cost_micro_cny);

    let gross_margin_percentage = if revenue_micro_cny > 0 {
        ((gross_profit_micro_cny as f64) / (revenue_micro_cny as f64)) * 100.0
    } else {
        0.0
    };

    MarginDashboard {
        total_requests,
        total_credits_charged,
        revenue_micro_cny,
        provider_cost_micro_cny,
        gross_profit_micro_cny,
        gross_margin_percentage,
    }
}

/// Rank models by total cost and compute individual margins (Spec §14.4).
pub fn compute_model_cost_rankings(
    entries: &[LedgerEntry],
    settings: &BillingSettings,
) -> Vec<ModelCostRanking> {
    use std::collections::HashMap;

    #[derive(Default)]
    struct Agg {
        requests: u64,
        total_tokens: u64,
        cost_micro_cny: i64,
        credits: i64,
        earned: EarnedCredits,
    }

    let mut map: HashMap<String, Agg> = HashMap::new();

    for entry in entries {
        if entry.kind == crate::ledger::LedgerKind::Usage {
            let agg = map.entry(entry.exposed_model.clone()).or_default();
            agg.requests += 1;
            agg.total_tokens = agg
                .total_tokens
                .saturating_add(entry.input_tokens.saturating_add(entry.output_tokens));
            agg.cost_micro_cny = agg
                .cost_micro_cny
                .saturating_add(entry.provider_cost_micro_cny);
            agg.credits = agg.credits.saturating_add(entry.credits_charged);
            agg.earned
                .add(entry_face_value(entry, settings), entry.credits_charged);
        }
    }

    let mut rankings: Vec<ModelCostRanking> = map
        .into_iter()
        .map(|(model_id, agg)| {
            // At the face value each entry was earned at, as in the dashboard.
            let revenue = agg.earned.revenue_micro_cny();
            let profit = revenue.saturating_sub(agg.cost_micro_cny);
            let margin = if revenue > 0 {
                ((profit as f64) / (revenue as f64)) * 100.0
            } else {
                0.0
            };
            ModelCostRanking {
                model_id,
                requests: agg.requests,
                total_tokens: agg.total_tokens,
                provider_cost_micro_cny: agg.cost_micro_cny,
                credits_charged: agg.credits,
                margin_percentage: margin,
            }
        })
        .collect();

    // Sort by highest provider cost descending
    rankings.sort_by_key(|b| std::cmp::Reverse(b.provider_cost_micro_cny));
    rankings
}

/// The ¥ face value of one credit a ledger entry was sold at: the one recorded when it was
/// settled, else (entries settled before it was recorded) the current settings' face value.
pub fn entry_face_value(entry: &LedgerEntry, settings: &BillingSettings) -> f64 {
    entry
        .credit_face_value_cny
        .unwrap_or(settings.credit_face_value_cny)
}

/// Micro-credits at a ¥ face value per credit, in micro-CNY.
pub fn face_value_micro_cny(micro_credits: i64, face_value_cny: f64) -> i64 {
    (micro_credits as f64 * face_value_cny).round() as i64
}

/// What one upstream served over a period, and what it should bill for it.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderCost {
    pub provider_id: String,
    pub requests: u64,
    pub uncached_input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub cost_micro_cny: i64,
}

/// Billed requests by the provider that served them, dearest first.
pub fn compute_provider_costs(entries: &[LedgerEntry]) -> Vec<ProviderCost> {
    let mut providers: BTreeMap<&str, ProviderCost> = BTreeMap::new();
    for entry in entries
        .iter()
        .filter(|entry| entry.kind == crate::ledger::LedgerKind::Usage)
    {
        let row = providers
            .entry(entry.provider_id.as_str())
            .or_insert_with(|| ProviderCost {
                provider_id: entry.provider_id.clone(),
                ..ProviderCost::default()
            });
        row.requests += 1;
        // The entry's input is the whole prompt: uncached, cache reads and cache writes.
        row.uncached_input_tokens = row.uncached_input_tokens.saturating_add(
            entry
                .input_tokens
                .saturating_sub(entry.cache_read_tokens)
                .saturating_sub(entry.cache_creation_tokens),
        );
        row.output_tokens = row.output_tokens.saturating_add(entry.output_tokens);
        row.cache_read_tokens = row
            .cache_read_tokens
            .saturating_add(entry.cache_read_tokens);
        row.cache_write_tokens = row
            .cache_write_tokens
            .saturating_add(entry.cache_creation_tokens);
        row.cost_micro_cny = row
            .cost_micro_cny
            .saturating_add(entry.provider_cost_micro_cny);
    }
    let mut rows: Vec<ProviderCost> = providers.into_values().collect();
    rows.sort_by_key(|row| std::cmp::Reverse(row.cost_micro_cny));
    rows
}

/// Margin over the requests whose cost is known, and what is left out: one model without
/// a cost no longer blanks the whole.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CostedMargin {
    pub costed_requests: u64,
    pub costed_credits: i64,
    pub revenue_micro_cny: i64,
    pub cost_micro_cny: i64,
    pub gross_profit_micro_cny: i64,
    /// Null without revenue.
    pub margin_percentage: Option<f64>,
    pub uncosted_requests: u64,
    pub uncosted_credits: i64,
    /// Of the uncosted, those settlement costed at an estimate, for want of the serving
    /// route's own cost: a fallback costed at its primary's price, say.
    pub estimated_requests: u64,
}

/// A billed request is costed when its cost is what the route that served it bills, from
/// official prices or a price version for that route; one costed at an estimate is not.
pub fn compute_costed_margin(entries: &[LedgerEntry], settings: &BillingSettings) -> CostedMargin {
    let mut margin = CostedMargin::default();
    for entry in entries
        .iter()
        .filter(|entry| entry.kind == crate::ledger::LedgerKind::Usage)
    {
        if !entry.cost_is_known() {
            margin.uncosted_requests += 1;
            margin.uncosted_credits = margin
                .uncosted_credits
                .saturating_add(entry.credits_charged);
            margin.estimated_requests += u64::from(entry.cost_is_estimated());
            continue;
        }
        margin.costed_requests += 1;
        margin.costed_credits = margin.costed_credits.saturating_add(entry.credits_charged);
        margin.revenue_micro_cny = margin
            .revenue_micro_cny
            .saturating_add(face_value_micro_cny(
                entry.credits_charged,
                entry_face_value(entry, settings),
            ));
        margin.cost_micro_cny = margin
            .cost_micro_cny
            .saturating_add(entry.provider_cost_micro_cny);
    }
    margin.gross_profit_micro_cny = margin
        .revenue_micro_cny
        .saturating_sub(margin.cost_micro_cny);
    margin.margin_percentage = (margin.revenue_micro_cny > 0)
        .then(|| margin.gross_profit_micro_cny as f64 / margin.revenue_micro_cny as f64 * 100.0);
    margin
}

/// Credits given and taken by balance adjustments over a period: compensations, promotions
/// and corrections, which usage revenue does not show. A card event (a note, an extension)
/// changes no balance and is not one; nor is an upgrade, which is a sale.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdjustmentTotals {
    pub count: u64,
    /// Credits given.
    pub positive_micro_credits: i64,
    /// Credits taken, as a negative amount.
    pub negative_micro_credits: i64,
    pub net_micro_credits: i64,
    /// The same by kind, each with the money recorded with it.
    pub by_kind: AdjustmentsByKind,
}

/// Balance adjustments of one kind over a period.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KindTotals {
    pub count: u64,
    pub positive_micro_credits: i64,
    /// As a negative amount.
    pub negative_micro_credits: i64,
    /// The money recorded with them: returned for refunds, as said for corrections.
    pub cash_micro_cny: i64,
}

impl KindTotals {
    fn add(&mut self, micro_credits: i64, cash_micro_cny: Option<i64>) {
        self.count += 1;
        if micro_credits > 0 {
            self.positive_micro_credits = self.positive_micro_credits.saturating_add(micro_credits);
        } else {
            self.negative_micro_credits = self.negative_micro_credits.saturating_add(micro_credits);
        }
        self.cash_micro_cny = self
            .cash_micro_cny
            .saturating_add(cash_micro_cny.unwrap_or(0));
    }
}

/// Balance adjustments by kind; one made before kinds counts as the kind its sign and
/// requests give.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdjustmentsByKind {
    pub compensation: KindTotals,
    pub gift: KindTotals,
    pub refund: KindTotals,
    pub correction: KindTotals,
}

/// Adds up the balance adjustments among `entries`, each entry once by its ID.
pub fn compute_adjustments<'a>(
    entries: impl IntoIterator<Item = &'a LedgerEntry>,
) -> AdjustmentTotals {
    use crate::compensation::AdjustmentKind;
    let mut totals = AdjustmentTotals::default();
    let mut seen = std::collections::HashSet::new();
    for entry in entries {
        let Some((kind, cash)) = crate::compensation::adjustment_kind(entry) else {
            continue;
        };
        if !seen.insert(entry.id.as_str()) {
            continue;
        }
        totals.count += 1;
        if entry.credits_charged > 0 {
            totals.positive_micro_credits = totals
                .positive_micro_credits
                .saturating_add(entry.credits_charged);
        } else {
            totals.negative_micro_credits = totals
                .negative_micro_credits
                .saturating_add(entry.credits_charged);
        }
        let by_kind = match kind {
            AdjustmentKind::Compensation => &mut totals.by_kind.compensation,
            AdjustmentKind::Gift => &mut totals.by_kind.gift,
            AdjustmentKind::Refund => &mut totals.by_kind.refund,
            AdjustmentKind::Correction => &mut totals.by_kind.correction,
        };
        by_kind.add(entry.credits_charged, cash);
    }
    totals.net_micro_credits = totals
        .positive_micro_credits
        .saturating_add(totals.negative_micro_credits);
    totals
}

/// The money that came in and went out over a period, as recorded: cards sold at the price
/// actually paid, upgrade and renewal payments, and refunds.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Cash {
    /// Cards issued in the period, each at the price paid for it, else its plan's price.
    pub sales_micro_cny: i64,
    pub upgrades_micro_cny: i64,
    pub refunds_micro_cny: i64,
    /// Sales and upgrades less refunds.
    pub net_micro_cny: i64,
}

/// The period's cash: `sales` as computed for it, and the upgrades and refunds among
/// `entries`, each entry once by its ID.
pub fn compute_cash<'a>(sales: &Sales, entries: impl IntoIterator<Item = &'a LedgerEntry>) -> Cash {
    let mut cash = Cash {
        sales_micro_cny: sales.issued_value_micro_cny,
        ..Cash::default()
    };
    let mut seen = std::collections::HashSet::new();
    for entry in entries {
        if !seen.insert(entry.id.as_str()) {
            continue;
        }
        if let Some(paid) = crate::compensation::upgrade_cash(entry) {
            cash.upgrades_micro_cny = cash.upgrades_micro_cny.saturating_add(paid);
        } else if let Some((crate::compensation::AdjustmentKind::Refund, returned)) =
            crate::compensation::adjustment_kind(entry)
        {
            cash.refunds_micro_cny = cash.refunds_micro_cny.saturating_add(returned.unwrap_or(0));
        }
    }
    cash.net_micro_cny = cash
        .sales_micro_cny
        .saturating_add(cash.upgrades_micro_cny)
        .saturating_sub(cash.refunds_micro_cny);
    cash
}

/// The cards as they were issued: one an upgrade or a renewal put on another plan is given
/// back the plan its first one replaced, so its sale stays at what it was issued for.
/// `entries` are the ledger's, live and archived, over all time.
pub fn cards_as_issued<'a>(
    cards: impl IntoIterator<Item = &'a crate::card::Card>,
    entries: impl IntoIterator<Item = &'a LedgerEntry>,
) -> Vec<crate::card::Card> {
    let mut first: std::collections::HashMap<&str, (u64, Option<crate::template::IssuedPlan>)> =
        std::collections::HashMap::new();
    for entry in entries {
        if crate::compensation::upgrade_cash(entry).is_none() {
            continue;
        }
        if first
            .get(entry.card_id.as_str())
            .is_some_and(|(ts, _)| *ts <= entry.ts_secs)
        {
            continue;
        }
        let previous = entry
            .event_detail()
            .and_then(|detail| detail.get("previousPlan").cloned())
            .and_then(|plan| serde_json::from_value::<crate::template::PlanRecord>(plan).ok())
            .map(crate::template::IssuedPlan::from);
        first.insert(entry.card_id.as_str(), (entry.ts_secs, previous));
    }
    cards
        .into_iter()
        .map(|card| {
            let mut card = card.clone();
            if let Some((_, plan)) = first.get(card.id.as_str()) {
                card.plan = plan.clone();
            }
            card
        })
        .collect()
}

/// Cards of one plan issued and activated over a period, and what they sold for.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanSales {
    /// The plan's ID, under the name the tiers had.
    pub template_id: String,
    pub plan_id: String,
    pub name: String,
    pub points: i64,
    /// Its price in the catalog, or, for a plan no longer in it, what its cards sold for.
    pub price_micro_cny: i64,
    pub issued_cards: u64,
    pub activated_cards: u64,
    /// Each card at the price it was issued at.
    pub issued_value_micro_cny: i64,
    pub activated_value_micro_cny: i64,
}

/// Cards issued and activated over a period, and what they sold for.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Sales {
    pub issued_cards: u64,
    pub issued_value_micro_cny: i64,
    pub activated_cards: u64,
    pub activated_value_micro_cny: i64,
    /// Cards of no plan (issued before the catalog with credits no tier had): counted, not
    /// valued.
    pub unpriced_issued_cards: u64,
    pub unpriced_activated_cards: u64,
    /// The catalog's plans in its order, then any plan cards were sold from that it no
    /// longer holds.
    pub by_plan: Vec<PlanSales>,
}

/// Issued counts cards created in `[from, to)`, less those voided without ever being
/// activated, which were never sold; activated counts cards activated in it. A card counts
/// under the plan it was issued from at the price it was issued at; one issued before the
/// catalog, under the tier its credits name at that tier's list price.
pub fn compute_sales<'a>(
    cards: impl IntoIterator<Item = &'a crate::card::Card>,
    plans: &[crate::template::Plan],
    from: Option<u64>,
    to: Option<u64>,
) -> Sales {
    let within = |ts: u64| from.is_none_or(|from| ts >= from) && to.is_none_or(|to| ts < to);
    let row = |id: &str, name: &str, points: i64, price_micro_cny: i64| PlanSales {
        template_id: id.to_string(),
        plan_id: id.to_string(),
        name: name.to_string(),
        points,
        price_micro_cny,
        issued_cards: 0,
        activated_cards: 0,
        issued_value_micro_cny: 0,
        activated_value_micro_cny: 0,
    };
    let mut sales = Sales {
        by_plan: plans
            .iter()
            .map(|plan| row(&plan.id, &plan.name, plan.points, plan.price_micro_cny()))
            .collect(),
        ..Sales::default()
    };
    for card in cards {
        let never_sold =
            card.status == crate::card::CardStatus::Voided && card.activated_at.is_none();
        let issued = within(card.created_at) && !never_sold;
        let activated = card.activated_at.is_some_and(within);
        if !issued && !activated {
            continue;
        }
        sales.issued_cards += u64::from(issued);
        sales.activated_cards += u64::from(activated);
        // At what was paid for it, when recorded.
        let sold = match (&card.plan, card.legacy_tier()) {
            (Some(plan), _) => Some((
                &*plan.id,
                &*plan.name,
                plan.points,
                plan.sale_price_micro_cny(),
            )),
            (None, Some(tier)) => Some((
                tier.template_id,
                tier.name,
                tier.points,
                tier.price_micro_cny,
            )),
            (None, None) => None,
        };
        let Some((id, name, points, price)) = sold else {
            sales.unpriced_issued_cards += u64::from(issued);
            sales.unpriced_activated_cards += u64::from(activated);
            continue;
        };
        let index = match sales.by_plan.iter().position(|plan| plan.plan_id == id) {
            Some(index) => index,
            None => {
                sales.by_plan.push(row(id, name, points, price));
                sales.by_plan.len() - 1
            }
        };
        let plan = &mut sales.by_plan[index];
        if issued {
            plan.issued_cards += 1;
            plan.issued_value_micro_cny = plan.issued_value_micro_cny.saturating_add(price);
            sales.issued_value_micro_cny = sales.issued_value_micro_cny.saturating_add(price);
        }
        if activated {
            plan.activated_cards += 1;
            plan.activated_value_micro_cny = plan.activated_value_micro_cny.saturating_add(price);
            sales.activated_value_micro_cny = sales.activated_value_micro_cny.saturating_add(price);
        }
    }
    sales
}

/// Credits still owed to customers: the balances of cards that can still be used.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Liability {
    pub cards: u64,
    pub micro_credits: i64,
    /// At the current face value.
    pub value_micro_cny: i64,
    /// Of those, cards not yet activated, which may not have been sold yet.
    pub unactivated_cards: u64,
    pub unactivated_micro_credits: i64,
}

/// Every card that is not expired, voided, banned or archived; frozen ones included, unless
/// their validity has passed: unfrozen, they would be expired.
pub fn compute_liability<'a>(
    cards: impl IntoIterator<Item = &'a crate::card::Card>,
    settings: &BillingSettings,
    now_secs: u64,
) -> Liability {
    use crate::card::CardStatus;
    let mut liability = Liability::default();
    for card in cards {
        let usable = card.archived_at.is_none()
            && !matches!(
                card.effective_status(now_secs),
                CardStatus::Expired | CardStatus::Voided | CardStatus::Banned
            )
            && card.valid_until.is_none_or(|until| now_secs < until);
        if !usable {
            continue;
        }
        let balance = card.credit_total.saturating_sub(card.credit_used).max(0);
        liability.cards += 1;
        liability.micro_credits = liability.micro_credits.saturating_add(balance);
        if card.status == CardStatus::Unactivated {
            liability.unactivated_cards += 1;
            liability.unactivated_micro_credits =
                liability.unactivated_micro_credits.saturating_add(balance);
        }
    }
    liability.value_micro_cny =
        face_value_micro_cny(liability.micro_credits, settings.credit_face_value_cny);
    liability
}

/// Compute provider health metrics from request traces (Spec §14.4).
pub fn compute_provider_health(
    traces: &[RequestTrace],
    provider_id: &str,
) -> ProviderHealthSummary {
    let mut total = 0u64;
    let mut successes = 0u64;
    let mut errors = 0u64;
    let mut ttft_sum = 0u64;
    let mut ttft_count = 0u64;
    let mut tps_sum = 0.0f64;
    let mut tps_count = 0u64;

    for trace in traces {
        if trace.status == TraceStatus::InProgress {
            continue;
        }
        if trace.provider_id.as_deref() == Some(provider_id) {
            total += 1;
            match trace.status {
                TraceStatus::Success => successes += 1,
                TraceStatus::Error => errors += 1,
                TraceStatus::ClientAborted | TraceStatus::InProgress => {}
            }
            if let Some(ttft) = trace.ttft_ms {
                ttft_sum = ttft_sum.saturating_add(ttft as u64);
                ttft_count += 1;
            }
            if let Some(tps) = trace.tokens_per_second {
                tps_sum += tps;
                tps_count += 1;
            }
        }
    }

    let success_rate = if total > 0 {
        ((successes as f64) / (total as f64)) * 100.0
    } else {
        100.0
    };

    let avg_ttft_ms = if ttft_count > 0 {
        Some((ttft_sum as f64) / (ttft_count as f64))
    } else {
        None
    };

    let avg_tokens_per_second = if tps_count > 0 {
        Some(tps_sum / (tps_count as f64))
    } else {
        None
    };

    ProviderHealthSummary {
        provider_id: provider_id.to_string(),
        total_requests: total,
        success_requests: successes,
        error_requests: errors,
        success_rate,
        avg_ttft_ms,
        avg_tokens_per_second,
    }
}

/// Export ledger entries to CSV format for financial reconciliation (Spec §14.4).
pub fn export_reconciliation_csv(entries: &[LedgerEntry]) -> String {
    export_ledger_csv(
        entries,
        &std::collections::HashMap::new(),
        &BillingSettings::default(),
    )
}

/// The ledger as CSV. The original columns come first, so tools reading them keep working;
/// readable ones follow: the time in UTC, the provider's name, cache reads and writes,
/// credits as a decimal, and for usage its ¥ revenue at face value and ¥ cost. Then who made
/// an adjustment or card event and why; for usage, `reason` says where its cost came from,
/// then the price version that charged it and the Key that served it, when known. Then a
/// balance adjustment's kind (compensation, gift, refund or correction; `upgrade` for an
/// upgrade or a renewal) and the ¥ recorded with it.
pub fn export_ledger_csv(
    entries: &[LedgerEntry],
    provider_names: &std::collections::HashMap<String, String>,
    settings: &BillingSettings,
) -> String {
    let mut csv = String::from("id,card_id,ts,kind,invocation_id,exposed_model,provider_id,input_tokens,output_tokens,credits_charged,provider_cost_micro_cny,time_utc,provider_name,cache_read_tokens,cache_write_tokens,credits,revenue_cny,cost_cny,operator,reason,rate_card_version,key_id,kind,cash_cny\n");
    for e in entries {
        let usage = e.kind == crate::ledger::LedgerKind::Usage;
        let revenue = usage.then(|| {
            micro_decimal(face_value_micro_cny(
                e.credits_charged,
                entry_face_value(e, settings),
            ))
        });
        let (money_kind, cash) = match (
            crate::compensation::upgrade_cash(e),
            crate::compensation::adjustment_kind(e),
        ) {
            (Some(paid), _) => ("upgrade", Some(paid)),
            (None, Some((kind, cash))) => (kind.as_str(), cash),
            (None, None) => ("", None),
        };
        csv.push_str(&format!(
            "{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}\n",
            csv_text(&e.id),
            csv_text(&e.card_id),
            e.ts_secs,
            csv_text(&format!("{:?}", e.kind)),
            csv_text(e.invocation_id.as_deref().unwrap_or("")),
            csv_text(&e.exposed_model),
            csv_text(&e.provider_id),
            e.input_tokens,
            e.output_tokens,
            e.credits_charged,
            e.provider_cost_micro_cny,
            csv_text(&iso_utc(e.ts_secs)),
            csv_text(provider_names.get(&e.provider_id).unwrap_or(&e.provider_id)),
            e.cache_read_tokens,
            e.cache_creation_tokens,
            micro_decimal(e.credits_charged),
            revenue.unwrap_or_default(),
            if usage {
                micro_decimal(e.provider_cost_micro_cny)
            } else {
                String::new()
            },
            csv_text(e.operator_id.as_deref().unwrap_or("")),
            csv_text(e.reason.as_deref().unwrap_or("")),
            csv_text(e.rate_card_version.as_deref().unwrap_or("")),
            csv_text(
                e.detail
                    .as_ref()
                    .filter(|_| usage)
                    .and_then(|detail| detail.get("keyId"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(""),
            ),
            csv_text(money_kind),
            cash.map(micro_decimal).unwrap_or_default(),
        ));
    }
    csv
}

/// A micro-unit amount as a decimal of whole units, without trailing zeros: -1.5, 2, 0.000001.
pub(crate) fn micro_decimal(micro: i64) -> String {
    let sign = if micro < 0 { "-" } else { "" };
    let (whole, fraction) = (
        micro.unsigned_abs() / 1_000_000,
        micro.unsigned_abs() % 1_000_000,
    );
    if fraction == 0 {
        format!("{sign}{whole}")
    } else {
        let fraction = format!("{fraction:06}");
        format!("{sign}{whole}.{}", fraction.trim_end_matches('0'))
    }
}

/// Unix seconds as ISO-8601 in UTC, such as 2023-11-14T22:13:20Z.
pub fn iso_utc(secs: u64) -> String {
    // Days to a civil date, after Howard Hinnant's days_from_civil inverse.
    let days = (secs / 86_400) as i64 + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    let clock = secs % 86_400;
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        clock / 3600,
        clock % 3600 / 60,
        clock % 60
    )
}

/// A text cell for an export an operator opens in a spreadsheet. Some text comes from
/// clients (the invocation id, the model name), so every cell is quoted (RFC 4180) and one
/// that a spreadsheet would run as a formula is prefixed with a quote to stay text.
/// Numbers are written as numbers: a negative adjustment must stay a number.
fn csv_text(value: &str) -> String {
    let inert = if value.starts_with(['=', '+', '-', '@', '\t', '\r']) {
        format!("'{value}")
    } else {
        value.to_string()
    };
    format!("\"{}\"", inert.replace('"', "\"\""))
}

/// Export ledger entries to JSON format for financial reconciliation (Spec §14.4).
pub fn export_reconciliation_json(entries: &[LedgerEntry]) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(entries)
}

/// Prune expired traces older than cutoff timestamp (Spec §14.6 Data Retention Policy).
pub fn prune_traces_in_place(traces: &mut Vec<RequestTrace>, cutoff_secs: u64) -> usize {
    let initial_len = traces.len();
    traces.retain(|t| t.ts >= cutoff_secs);
    initial_len - traces.len()
}
