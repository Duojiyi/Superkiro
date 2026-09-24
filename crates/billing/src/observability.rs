//! Observability, metrics aggregation, gross margin reporting, and anomaly detection.
//!
//! Spec §5 (Data Model) & Spec §14.4 (Observability & Financial Reconciliation).

use crate::ledger::LedgerEntry;
use crate::rate_card::BillingSettings;
use serde::{Deserialize, Serialize};

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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    pub attempt_chain: Vec<AttemptRecord>,
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
        }
    }

    pub fn with_expiry(mut self, expires_at: u64) -> Self {
        self.expires_at = Some(expires_at);
        self
    }

    pub fn is_active(&self, now_secs: u64) -> bool {
        if !self.enabled {
            return false;
        }
        if let Some(exp) = self.expires_at {
            if now_secs >= exp {
                return false;
            }
        }
        true
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

    for entry in entries {
        if entry.kind == crate::ledger::LedgerKind::Usage {
            // Saturating: release builds wrap on overflow, and one stored entry can
            // carry an extreme cost from before settlement was bounded.
            total_requests += 1;
            total_credits_charged = total_credits_charged.saturating_add(entry.credits_charged);
            provider_cost_micro_cny =
                provider_cost_micro_cny.saturating_add(entry.provider_cost_micro_cny);
        }
    }

    // Revenue in micro-CNY: (credits_charged * credit_face_value_cny)
    // 1 credit = 1_000_000 micro-credits.
    // revenue_micro_cny = (credits_charged / 1_000_000) * (credit_face_value_cny * 1_000_000)
    //                   = credits_charged * credit_face_value_cny
    let revenue_micro_cny =
        (total_credits_charged as f64 * settings.credit_face_value_cny).round() as i64;
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
        }
    }

    let mut rankings: Vec<ModelCostRanking> = map
        .into_iter()
        .map(|(model_id, agg)| {
            let revenue = (agg.credits as f64 * settings.credit_face_value_cny).round() as i64;
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
    let mut csv = String::from("id,card_id,ts,kind,invocation_id,exposed_model,provider_id,input_tokens,output_tokens,credits_charged,provider_cost_micro_cny\n");
    for e in entries {
        csv.push_str(&format!(
            "{},{},{},{:?},{},{},{},{},{},{},{}\n",
            e.id,
            e.card_id,
            e.ts_secs,
            e.kind,
            e.invocation_id.as_deref().unwrap_or(""),
            e.exposed_model,
            e.provider_id,
            e.input_tokens,
            e.output_tokens,
            e.credits_charged,
            e.provider_cost_micro_cny
        ));
    }
    csv
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
