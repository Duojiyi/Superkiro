//! Unified billing engine managing cards, reservations, settlements, and ledger (Spec §5, §6).

use crate::card::{Card, CardError, CardStatus};
use crate::ledger::{LedgerEntry, LedgerKind, PricingRates, UsageTokens};
use crate::provider::{Provider, ProviderKey};
use crate::reservation::{CreditReservation, ReservationEstimateParams, ReservationState};
use crate::topup::TopupCode;
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use thiserror::Error;

#[path = "commercial.rs"]
mod commercial;
pub use commercial::{CommercialAudit, CommercialConfig, CommercialUpdate};

#[cfg(test)]
type SnapshotSaveHook = (Arc<std::sync::Barrier>, Arc<std::sync::Barrier>);

#[derive(Debug, Error, PartialEq, Eq)]
pub enum BillingError {
    #[error("Group does not allow new card issuance")]
    GroupIssuanceDisabled,

    #[error("Card {0} not found")]
    CardNotFound(String),

    #[error("Card error: {0}")]
    Card(#[from] CardError),

    #[error("Reservation with invocation ID '{0}' not found")]
    ReservationNotFound(String),

    #[error("Duplicate invocation ID '{0}': already reserved or settled")]
    DuplicateInvocation(String),

    #[error("Reservation '{0}' has already been released")]
    ReservationAlreadyReleased(String),

    #[error("Card is already bound to another device; unbind it on the portal before logging in on a new device")]
    DeviceAlreadyBound,

    #[error("Device rebind limit reached ({current}/{max})")]
    RebindLimitExceeded { current: u32, max: u32 },

    #[error("Device rebind in cooldown: remaining {remaining_secs}s")]
    RebindCooldown { remaining_secs: u64 },

    #[error("Device {device} not found for card {card_id}")]
    DeviceNotFound { card_id: String, device: String },

    #[error("Top-up code is invalid or already redeemed")]
    InvalidOrRedeemedTopupCode,

    #[error("Cannot void card {card_id}: only unactivated cards can be voided (current status: {current_status:?})")]
    CannotVoidActivatedCard {
        card_id: String,
        current_status: CardStatus,
    },

    #[error("Invalid balance adjustment: {0}")]
    InvalidAdjustment(String),

    #[error("Card concurrency limit exceeded ({current}/{max})")]
    ConcurrencyLimitExceeded { current: u32, max: u32 },

    #[error(
        "Daily credit limit exceeded (limit: {limit}, used today: {current}, needed: {needed})"
    )]
    DailyLimitExceeded {
        limit: i64,
        current: i64,
        needed: i64,
    },

    #[error("Monthly credit limit exceeded (limit: {limit}, used this month: {current}, needed: {needed})")]
    MonthlyLimitExceeded {
        limit: i64,
        current: i64,
        needed: i64,
    },

    #[error("Settlement charge {charge} exceeds the reservation {reserved}")]
    SettlementExceedsReservation { charge: i64, reserved: i64 },

    #[error("Settlement charge {charge} exceeds the card's remaining balance {available}")]
    SettlementExceedsBalance { charge: i64, available: i64 },

    #[error("Provider returned no billable usage for a reserved invocation")]
    MissingUsage,

    #[error("Invalid billing state: {0}")]
    InvalidState(String),

    #[error("Billing persistence failed: {0}")]
    Persistence(String),
}

/// Card balance reconciliation details against the immutable usage ledger (Spec §5, §14.9, §14.10.5).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CardReconciliation {
    pub card_id: String,
    pub initial_credits: i64,
    pub total_topup_credits: i64,
    pub total_positive_adjustments: i64,
    pub total_negative_adjustments: i64,
    pub total_usage_charges: i64,
    pub expected_credit_total: i64,
    pub actual_credit_total: i64,
    pub expected_credit_used: i64,
    pub actual_credit_used: i64,
    pub available_credits: i64,
    pub is_balanced: bool,
}

use crate::group::{Group, ModelMap};
use crate::rate_card::{BillingSettings, MarginSummary, RateCard, RateCardVersion};
use crate::workbench::{RateCardAuditLog, SimulationResult};

use crate::observability::{
    compute_margin_dashboard, compute_model_cost_rankings, compute_provider_health,
    export_reconciliation_csv, export_reconciliation_json, prune_traces_in_place, Announcement,
    AnomalyAction, AnomalyAlert, DailyUsageSummary, MarginDashboard, ModelCostRanking,
    ProviderHealthSummary, RequestTrace, TraceStatus,
};

/// In-memory billing engine for concurrency-safe reservations and settlements.
/// What a refresh did to the card's refresh family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshRotation {
    /// The family advanced to this version, durably.
    Rotated(u64),
    /// A retry of the refresh that just happened: nothing changed; this is the version.
    Reissued(u64),
}

impl RefreshRotation {
    /// The version the new refresh token must carry.
    pub fn version(self) -> u64 {
        match self {
            Self::Rotated(version) | Self::Reissued(version) => version,
        }
    }
}

#[derive(Debug, Clone)]
pub struct BillingEngine {
    cards: Arc<RwLock<HashMap<String, Card>>>,
    consumed_refresh_tokens: Arc<RwLock<HashMap<String, u64>>>,
    active_reservations: Arc<Mutex<HashMap<String, usize>>>,
    reservations: Arc<RwLock<HashMap<String, CreditReservation>>>,
    ledger: Arc<RwLock<Vec<LedgerEntry>>>,
    rates: Arc<RwLock<HashMap<String, PricingRates>>>,
    topup_codes: Arc<RwLock<HashMap<String, TopupCode>>>,
    groups: Arc<RwLock<HashMap<String, Group>>>,
    model_maps: Arc<RwLock<Vec<ModelMap>>>,
    rate_cards: Arc<RwLock<HashMap<String, RateCard>>>,
    rate_card_versions: Arc<RwLock<Vec<RateCardVersion>>>,
    rate_card_audit_logs: Arc<RwLock<Vec<RateCardAuditLog>>>,
    commercial_audit_logs: Arc<RwLock<Vec<CommercialAudit>>>,
    settings: Arc<RwLock<BillingSettings>>,
    traces: Arc<RwLock<Vec<RequestTrace>>>,
    announcements: Arc<RwLock<Vec<Announcement>>>,
    providers: Arc<RwLock<HashMap<String, Provider>>>,
    provider_keys: Arc<RwLock<HashMap<String, ProviderKey>>>,
    default_rates: PricingRates,
    persistence_path: Arc<RwLock<Option<std::path::PathBuf>>>,
    master_kek: Arc<RwLock<Option<crate::crypto::MasterKek>>>,
    /// Serializes snapshot publication. Without this lock concurrent requests can
    /// overwrite the shared temporary file or publish an older snapshot last.
    persistence_lock: Arc<Mutex<()>>,
    snapshot_sequence: Arc<AtomicU64>,
    last_snapshot_checksum: Arc<RwLock<Option<String>>>,
    last_persistence_error: Arc<RwLock<Option<String>>>,
    /// Serializes cross-table mutations with snapshot reads so a published file
    /// represents one logical billing state rather than a mixture of revisions.
    state_lock: Arc<RwLock<()>>,
    injected_persistence_fault: Arc<RwLock<bool>>,
    archived_ledger_receipts: Arc<RwLock<Vec<ArchivedLedgerReceipt>>>,
    archived_ledger_summary: Arc<RwLock<ArchivedLedgerSummary>>,
    issuance_orders: Arc<RwLock<HashMap<String, IssuanceOrder>>>,
    unpaid_ledger: Arc<RwLock<Vec<UnpaidCharge>>>,
    pending_settlements: Arc<RwLock<HashMap<String, PendingSettlement>>>,
    last_snapshot_mirror_error: Arc<RwLock<Option<String>>>,
    #[cfg(test)]
    injected_mirror_fault: Arc<RwLock<bool>>,
    #[cfg(test)]
    fail_after_pending: Arc<RwLock<bool>>,
    #[cfg(test)]
    save_snapshot_hook: Arc<Mutex<Option<SnapshotSaveHook>>>,
    require_anchor: Arc<RwLock<bool>>,
}

/// Request-owned protection against orphan reclamation.
#[derive(Debug)]
pub struct ReservationLease {
    active: Arc<Mutex<HashMap<String, usize>>>,
    invocation_id: String,
}

impl Drop for ReservationLease {
    fn drop(&mut self) {
        let mut active = self.active.lock().unwrap();
        if let Some(count) = active.get_mut(&self.invocation_id) {
            *count -= 1;
            if *count == 0 {
                active.remove(&self.invocation_id);
            }
        }
    }
}

const SNAPSHOT_VERSION: u32 = 2;
const MAX_SNAPSHOT_BYTES: usize = 256 * 1024 * 1024;
/// Request traces are rewritten with the whole state on every mutation; a hundred
/// thousand of them made traces the largest thing in it.
const MAX_RETAINED_TRACES: usize = 10_000;
/// Ceiling for one settlement: ten million credits. Far above any real request, and
/// low enough that `credit_used` cannot overflow — a card in debt cannot reserve
/// again, so only its in-flight requests can ever add to it.
const MAX_SETTLEMENT_MICRO_CREDITS: i64 = 10_000_000 * crate::MICRO_CREDITS_PER_CREDIT;

/// Authenticated encrypted snapshot envelope for billing state persistence (Spec §7, P1-03).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EncryptedSnapshotEnvelope {
    pub format: String,
    pub version: u32,
    pub timestamp: u64,
    #[serde(default)]
    pub sequence: u64,
    #[serde(default)]
    pub previous_checksum: Option<String>,
    pub ciphertext: String,
    pub sha256_checksum: String,
}

/// Serializable snapshot of BillingEngine state for persistence and disaster recovery.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BillingSnapshot {
    pub version: u32,
    pub timestamp: u64,
    /// Monotonic local publication sequence. Zero is accepted only for legacy files.
    #[serde(default)]
    pub sequence: u64,
    /// Hash of the previously published snapshot, when available.
    #[serde(default)]
    pub previous_checksum: Option<String>,
    pub cards: HashMap<String, Card>,
    #[serde(default)]
    pub consumed_refresh_tokens: HashMap<String, u64>,
    #[serde(default)]
    pub reservations: HashMap<String, CreditReservation>,
    pub ledger: Vec<LedgerEntry>,
    #[serde(default)]
    pub rates: HashMap<String, PricingRates>,
    pub topup_codes: HashMap<String, TopupCode>,
    pub groups: HashMap<String, Group>,
    pub model_maps: Vec<ModelMap>,
    pub rate_cards: HashMap<String, RateCard>,
    pub rate_card_versions: Vec<RateCardVersion>,
    #[serde(default)]
    pub rate_card_audit_logs: Vec<RateCardAuditLog>,
    #[serde(default)]
    pub commercial_audit_logs: Vec<CommercialAudit>,
    pub settings: BillingSettings,
    #[serde(default)]
    pub traces: Vec<RequestTrace>,
    #[serde(default)]
    pub announcements: Vec<Announcement>,
    #[serde(default)]
    pub providers: HashMap<String, Provider>,
    #[serde(default)]
    pub provider_keys: HashMap<String, ProviderKey>,
    #[serde(default)]
    pub archived_ledger_receipts: Vec<ArchivedLedgerReceipt>,
    #[serde(default)]
    pub archived_ledger_summary: ArchivedLedgerSummary,
    #[serde(default)]
    pub issuance_orders: HashMap<String, IssuanceOrder>,
    #[serde(default)]
    pub unpaid_ledger: Vec<UnpaidCharge>,
    #[serde(default)]
    pub pending_settlements: HashMap<String, PendingSettlement>,
}

/// Durable issuance replay record. Snapshots containing these records require AEAD.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct IssuanceOrder {
    pub fingerprint: String,
    pub response_json: String,
}

/// Historical shortfall when actual usage exhausted prepaid credit. Full usage
/// remains in the ledger/card balance; this record is not a second debit.
/// Current debt is Card::outstanding_debt(), so later topups pay it automatically.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct UnpaidCharge {
    pub invocation_id: String,
    pub card_id: String,
    pub amount_micro_credits: i64,
    pub ts_secs: u64,
}

/// A consumed request, priced once and retained until its debit commits.
/// The matching Held reservation must never be released or reclaimed.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PendingSettlement {
    pub entry: LedgerEntry,
    pub tokens: UsageTokens,
}

/// Financial facts that must outlive ledger detail retention.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ArchivedCardSummary {
    pub topups: i64,
    pub positive_adjustments: i64,
    pub negative_adjustments: i64,
    pub usage: i64,
    #[serde(default)]
    pub provider_cost_micro_cny: i64,
    // Exact timestamps preserve the existing rolling-window semantics.
    pub usage_by_second: BTreeMap<u64, i64>,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ArchivedLedgerSummary {
    pub cards: HashMap<String, ArchivedCardSummary>,
    pub adjustments: HashMap<String, LedgerEntry>,
    #[serde(default)]
    pub entries_count: usize,
}

impl ArchivedLedgerSummary {
    fn include(&mut self, entry: &LedgerEntry) {
        self.entries_count += 1;
        let card = self.cards.entry(entry.card_id.clone()).or_default();
        match entry.kind {
            LedgerKind::Usage => {
                card.usage = card.usage.saturating_add(entry.credits_charged);
                card.provider_cost_micro_cny = card
                    .provider_cost_micro_cny
                    .saturating_add(entry.provider_cost_micro_cny);
                let usage = card.usage_by_second.entry(entry.ts_secs).or_default();
                *usage = usage.saturating_add(entry.credits_charged);
            }
            LedgerKind::Topup => card.topups = card.topups.saturating_add(entry.credits_charged),
            LedgerKind::Adjustment => {
                if entry.credits_charged >= 0 {
                    card.positive_adjustments = card
                        .positive_adjustments
                        .saturating_add(entry.credits_charged);
                } else {
                    card.negative_adjustments = card
                        .negative_adjustments
                        .saturating_add(entry.credits_charged.saturating_neg());
                }
                if let Some(key) = &entry.invocation_id {
                    self.adjustments.insert(key.clone(), entry.clone());
                }
            }
        }
    }

    fn usage_since(&self, card_id: &str, since: u64) -> i64 {
        self.cards.get(card_id).map_or(0, |card| {
            card.usage_by_second
                .range(since..)
                .fold(0i64, |sum, (_, amount)| sum.saturating_add(*amount))
        })
    }
}

impl BillingSnapshot {
    fn usage_since(&self, card_id: &str, since: u64) -> i64 {
        self.ledger
            .iter()
            .filter(|e| e.card_id == card_id && e.kind == LedgerKind::Usage && e.ts_secs >= since)
            .fold(
                self.archived_ledger_summary.usage_since(card_id, since),
                |sum, e| sum.saturating_add(e.credits_charged),
            )
    }

    fn check_quota(&self, card: &Card, additional: i64, now_secs: u64) -> Result<(), BillingError> {
        if self
            .pending_settlements
            .values()
            .any(|p| p.entry.card_id == card.id)
        {
            return Err(BillingError::InvalidState(
                "card has a pending settlement".into(),
            ));
        }
        // A hold remains exposure in the current window until settled/released;
        // settlement usage is attributed to completion time.
        let held = self
            .reservations
            .values()
            .filter(|r| r.card_id == card.id && r.state == ReservationState::Held)
            .fold(0i64, |sum, r| sum.saturating_add(r.reserved_micro_credits));
        if let Some(limit) = card.daily_credit_limit {
            let current = self
                .usage_since(&card.id, now_secs / 86_400 * 86_400)
                .saturating_add(held);
            if additional > limit.saturating_sub(current) {
                return Err(BillingError::DailyLimitExceeded {
                    limit,
                    current,
                    needed: additional,
                });
            }
        }
        if let Some(limit) = card.monthly_credit_limit {
            let current = self
                .usage_since(&card.id, now_secs.saturating_sub(30 * 86_400))
                .saturating_add(held);
            if additional > limit.saturating_sub(current) {
                return Err(BillingError::MonthlyLimitExceeded {
                    limit,
                    current,
                    needed: additional,
                });
            }
        }
        Ok(())
    }
}

/// Immutable audit receipt for an archived batch of settled ledger entries (Spec §14.6, T03).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ArchivedLedgerReceipt {
    pub archive_id: String,
    pub archive_file: String,
    pub drained_entries_count: usize,
    pub sha256_checksum: String,
    pub before_ts_secs: u64,
    pub created_at_secs: u64,
}

/// On-disk payload structure for archived ledger files (T03).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ArchivedLedgerPayload {
    pub archive_id: String,
    pub created_at_secs: u64,
    pub before_ts_secs: u64,
    pub entries_count: usize,
    pub entries: Vec<LedgerEntry>,
}

/// Cryptographic anchor pointing to the committed snapshot generation (T03).
///
/// Serves as the single atomic commit point: an update is officially committed
/// once this anchor file is atomically replaced and synced to disk.
///
/// # Threat Boundary & Rollback Limitations (T03 Requirement 5)
/// The local snapshot anchor (`.anchor`) records the monotonically increasing sequence
/// number, SHA-256 integrity hash, and committed generation filename.
/// It protects against partial writes, torn pages, crash-induced state inconsistencies,
/// and single-file rollback (e.g. where an old state file is swapped while the anchor is intact).
///
/// HOWEVER, if an adversary with filesystem access replaces the ENTIRE state directory
/// (both the data file and the `.anchor` file) with an older consistent pair from a previous
/// backup, the local verification cannot detect this rollback on cold start without an external
/// monotonic source of truth (e.g. distributed consensus, TPM NVRAM, or cloud KMS version).
/// Therefore, KIRO BYOK does NOT claim protection against whole-directory external rollback
/// attacks at the local filesystem layer.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SnapshotAnchor {
    pub version: u32,
    pub sequence: u64,
    pub checksum: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation_file: Option<String>,
}

impl Default for BillingEngine {
    fn default() -> Self {
        Self::new()
    }
}

/// Runtime retry schedule. Durable pending intents remain the source of truth.
/// Restart resets delays so recovery begins immediately; retries never reprice usage.
#[derive(Default)]
pub struct PendingSettlementRecovery {
    retries: HashMap<String, (u64, u64)>,
}

impl PendingSettlementRecovery {
    /// At most 64 attempts per tick, with 30s exponential backoff capped at 5 minutes.
    /// Errors are returned to the runtime for alerting, never discarded or refunded.
    pub fn tick(
        &mut self,
        engine: &BillingEngine,
        now_secs: u64,
    ) -> Vec<(String, Result<LedgerEntry, BillingError>)> {
        // One failed write refuses every reservation until something commits, and the
        // refused path is the one that would. Probe instead of waiting. The probe writes
        // the current in-memory state, so an intent that could only be kept in memory
        // during the outage becomes durable here rather than being lost on restart.
        if !engine.persistence_ready() {
            // Logged, because the only other visible error is the original write failure:
            // an operator could not tell a probe refusing the in-memory state from an
            // outage that is still going on.
            if let Err(error) = engine.probe_persistence() {
                eprintln!("[kiro-billing] persistence probe failed: {error}");
            }
        }
        let ids: std::collections::HashSet<_> = engine
            .pending_settlements
            .read()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        self.retries.retain(|id, _| ids.contains(id));
        let mut due: Vec<_> = ids
            .into_iter()
            .filter_map(|id| {
                let (next, delay) = self.retries.get(&id).copied().unwrap_or((0, 0));
                (next <= now_secs).then_some((next, id, delay))
            })
            .collect();
        due.sort();
        due.into_iter()
            .take(64)
            .map(|(_, id, delay)| {
                let result = engine.retry_pending_settlement(&id);
                if result.is_ok() {
                    self.retries.remove(&id);
                } else {
                    let delay = delay.saturating_mul(2).clamp(30, 300);
                    self.retries
                        .insert(id.clone(), (now_secs.saturating_add(delay), delay));
                }
                (id, result)
            })
            .collect()
    }
}

impl BillingEngine {
    pub fn new() -> Self {
        let groups = Arc::new(RwLock::new(HashMap::new()));
        let default_group = Group::pro_plus("group-pro-plus", "标准模型与计费组");
        groups
            .write()
            .unwrap()
            .insert(default_group.id.clone(), default_group);

        let rate_cards = Arc::new(RwLock::new(HashMap::new()));
        let default_rate_card = RateCard::new("default", "Standard Rate Card", 0);
        rate_cards
            .write()
            .unwrap()
            .insert(default_rate_card.id.clone(), default_rate_card);

        let master_kek = crate::crypto::MasterKek::from_env("KIRO_MASTER_KEK").ok();

        Self {
            cards: Arc::new(RwLock::new(HashMap::new())),
            consumed_refresh_tokens: Arc::new(RwLock::new(HashMap::new())),
            active_reservations: Arc::new(Mutex::new(HashMap::new())),
            reservations: Arc::new(RwLock::new(HashMap::new())),
            ledger: Arc::new(RwLock::new(Vec::new())),
            rates: Arc::new(RwLock::new(HashMap::new())),
            topup_codes: Arc::new(RwLock::new(HashMap::new())),
            groups,
            model_maps: Arc::new(RwLock::new(Vec::new())),
            rate_cards,
            rate_card_versions: Arc::new(RwLock::new(Vec::new())),
            rate_card_audit_logs: Arc::new(RwLock::new(Vec::new())),
            commercial_audit_logs: Arc::new(RwLock::new(Vec::new())),
            settings: Arc::new(RwLock::new(BillingSettings::default())),
            traces: Arc::new(RwLock::new(Vec::new())),
            announcements: Arc::new(RwLock::new(Vec::new())),
            providers: Arc::new(RwLock::new(HashMap::new())),
            provider_keys: Arc::new(RwLock::new(HashMap::new())),
            default_rates: PricingRates::default(),
            persistence_path: Arc::new(RwLock::new(None)),
            master_kek: Arc::new(RwLock::new(master_kek)),
            persistence_lock: Arc::new(Mutex::new(())),
            snapshot_sequence: Arc::new(AtomicU64::new(0)),
            last_snapshot_checksum: Arc::new(RwLock::new(None)),
            last_persistence_error: Arc::new(RwLock::new(None)),
            state_lock: Arc::new(RwLock::new(())),
            injected_persistence_fault: Arc::new(RwLock::new(false)),
            archived_ledger_receipts: Arc::new(RwLock::new(Vec::new())),
            archived_ledger_summary: Arc::new(RwLock::new(ArchivedLedgerSummary::default())),
            issuance_orders: Arc::new(RwLock::new(HashMap::new())),
            unpaid_ledger: Arc::new(RwLock::new(Vec::new())),
            pending_settlements: Arc::new(RwLock::new(HashMap::new())),
            last_snapshot_mirror_error: Arc::new(RwLock::new(None)),
            #[cfg(test)]
            injected_mirror_fault: Arc::new(RwLock::new(false)),
            #[cfg(test)]
            fail_after_pending: Arc::new(RwLock::new(false)),
            #[cfg(test)]
            save_snapshot_hook: Arc::new(Mutex::new(None)),
            require_anchor: Arc::new(RwLock::new(
                std::env::var("KIRO_REQUIRE_ANCHOR")
                    .map(|v| v == "true" || v == "1")
                    .unwrap_or(false),
            )),
        }
    }

    /// Hold before reserving, and keep alive through stream settlement. The janitor
    /// only reclaims orphans; a live request must not expire during backpressure.
    /// This process-local ownership is intentionally not restored after a crash.
    pub fn protect_reservation(&self, invocation_id: &str) -> ReservationLease {
        let _state_guard = self.state_lock.write().unwrap();
        *self
            .active_reservations
            .lock()
            .unwrap()
            .entry(invocation_id.to_string())
            .or_default() += 1;
        ReservationLease {
            active: self.active_reservations.clone(),
            invocation_id: invocation_id.to_string(),
        }
    }

    /// Consume a refresh JTI in the same durable transaction as billing state.
    pub fn consume_refresh_token(
        &self,
        jti: &str,
        expires: u64,
        now: u64,
    ) -> Result<(), BillingError> {
        let _state_guard = self.state_lock.write().unwrap();
        if expires <= now || jti.is_empty() || jti.len() > 256 {
            return Err(BillingError::InvalidState(
                "expired or empty refresh token".into(),
            ));
        }
        let mut candidate = self.export_snapshot_locked(
            self.snapshot_sequence
                .load(Ordering::Acquire)
                .saturating_add(1),
            self.last_snapshot_checksum.read().unwrap().clone(),
        );
        candidate
            .consumed_refresh_tokens
            .retain(|_, exp| *exp > now);
        if candidate.consumed_refresh_tokens.contains_key(jti) {
            return Err(BillingError::InvalidState(
                "refresh token already used".into(),
            ));
        }
        candidate
            .consumed_refresh_tokens
            .insert(jti.to_string(), expires);
        self.commit_candidate_snapshot(&candidate, || {
            *self.consumed_refresh_tokens.write().unwrap() =
                candidate.consumed_refresh_tokens.clone();
        })
    }

    /// Export engine state as a serializable snapshot.
    pub fn export_snapshot(&self) -> BillingSnapshot {
        let _state_guard = self.state_lock.read().unwrap();
        self.export_snapshot_locked(
            self.snapshot_sequence.load(Ordering::Acquire),
            self.last_snapshot_checksum.read().unwrap().clone(),
        )
    }

    pub(crate) fn export_snapshot_locked(
        &self,
        sequence: u64,
        previous_checksum: Option<String>,
    ) -> BillingSnapshot {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        BillingSnapshot {
            version: SNAPSHOT_VERSION,
            timestamp: now,
            sequence,
            previous_checksum,
            cards: self.cards.read().unwrap().clone(),
            consumed_refresh_tokens: self.consumed_refresh_tokens.read().unwrap().clone(),
            reservations: self.reservations.read().unwrap().clone(),
            ledger: self.ledger.read().unwrap().clone(),
            rates: self.rates.read().unwrap().clone(),
            topup_codes: self.topup_codes.read().unwrap().clone(),
            groups: self.groups.read().unwrap().clone(),
            model_maps: self.model_maps.read().unwrap().clone(),
            rate_cards: self.rate_cards.read().unwrap().clone(),
            rate_card_versions: self.rate_card_versions.read().unwrap().clone(),
            rate_card_audit_logs: self.rate_card_audit_logs.read().unwrap().clone(),
            commercial_audit_logs: self.commercial_audit_logs.read().unwrap().clone(),
            settings: self.settings.read().unwrap().clone(),
            traces: self.traces.read().unwrap().clone(),
            announcements: self.announcements.read().unwrap().clone(),
            providers: self.providers.read().unwrap().clone(),
            provider_keys: self.provider_keys.read().unwrap().clone(),
            archived_ledger_receipts: self.archived_ledger_receipts.read().unwrap().clone(),
            archived_ledger_summary: self.archived_ledger_summary.read().unwrap().clone(),
            issuance_orders: self.issuance_orders.read().unwrap().clone(),
            unpaid_ledger: self.unpaid_ledger.read().unwrap().clone(),
            pending_settlements: self.pending_settlements.read().unwrap().clone(),
        }
    }

    /// Import engine state from a snapshot, overwriting matching tables.
    pub fn import_snapshot(&self, snapshot: BillingSnapshot) {
        let _state_guard = self.state_lock.write().unwrap();
        let mut cards = snapshot.cards;
        for card in cards.values_mut() {
            card.credit_reserved = 0;
        }
        let mut reservations = snapshot.reservations;
        // Migrate legacy snapshots once, not on every model request.
        for entry in &snapshot.ledger {
            if entry.kind == LedgerKind::Usage {
                if let Some(id) = &entry.invocation_id {
                    reservations.entry(id.clone()).or_insert_with(|| {
                        let mut reservation = CreditReservation::new(
                            format!("res-{id}"),
                            &entry.card_id,
                            id,
                            0,
                            entry.ts_secs,
                            0,
                        );
                        reservation.state = ReservationState::Settled;
                        reservation
                    });
                }
            }
        }
        reservations.retain(|id, reservation| {
            reservation.state != ReservationState::Held
                || snapshot.pending_settlements.contains_key(id)
        });
        for reservation in reservations
            .values()
            .filter(|r| r.state == ReservationState::Held)
        {
            if let Some(card) = cards.get_mut(&reservation.card_id) {
                card.credit_reserved = card
                    .credit_reserved
                    .saturating_add(reservation.reserved_micro_credits);
            }
        }
        *self.consumed_refresh_tokens.write().unwrap() = snapshot.consumed_refresh_tokens;
        *self.cards.write().unwrap() = cards;
        *self.reservations.write().unwrap() = reservations;
        *self.ledger.write().unwrap() = snapshot.ledger;
        *self.rates.write().unwrap() = snapshot.rates;
        *self.topup_codes.write().unwrap() = snapshot.topup_codes;
        *self.groups.write().unwrap() = snapshot.groups;
        *self.model_maps.write().unwrap() = snapshot.model_maps;
        *self.rate_cards.write().unwrap() = snapshot.rate_cards;
        *self.rate_card_versions.write().unwrap() = snapshot.rate_card_versions;
        *self.rate_card_audit_logs.write().unwrap() = snapshot.rate_card_audit_logs;
        *self.commercial_audit_logs.write().unwrap() = snapshot.commercial_audit_logs;
        *self.settings.write().unwrap() = snapshot.settings;
        *self.traces.write().unwrap() = snapshot.traces;
        *self.announcements.write().unwrap() = snapshot.announcements;
        *self.providers.write().unwrap() = snapshot.providers;
        let mut provider_keys = snapshot.provider_keys;
        if let Some(kek) = self.master_kek.read().unwrap().clone() {
            for key in provider_keys.values_mut() {
                if key.api_key.is_empty() {
                    if let Some(ciphertext) = key.api_key_encrypted.as_deref() {
                        key.api_key = kek.decrypt(ciphertext).unwrap_or_default();
                    }
                }
            }
        }
        *self.provider_keys.write().unwrap() = provider_keys;
        *self.archived_ledger_receipts.write().unwrap() = snapshot.archived_ledger_receipts;
        *self.archived_ledger_summary.write().unwrap() = snapshot.archived_ledger_summary;
        *self.issuance_orders.write().unwrap() = snapshot.issuance_orders;
        *self.unpaid_ledger.write().unwrap() = snapshot.unpaid_ledger;
        *self.pending_settlements.write().unwrap() = snapshot.pending_settlements;
        self.snapshot_sequence
            .store(snapshot.sequence, Ordering::Release);
        *self.last_snapshot_checksum.write().unwrap() = snapshot.previous_checksum;
    }

    /// Require snapshot anchor during load (strict production mode).
    pub fn set_require_anchor(&self, require: bool) {
        *self.require_anchor.write().unwrap() = require;
    }

    /// Retrieve active archival receipts recorded in billing engine.
    pub fn list_archived_ledger_receipts(&self) -> Vec<ArchivedLedgerReceipt> {
        self.archived_ledger_receipts.read().unwrap().clone()
    }

    /// Configure persistence path for write-through auto-sync.
    pub fn set_persistence_path<P: AsRef<std::path::Path>>(&self, path: P) {
        *self.persistence_path.write().unwrap() = Some(path.as_ref().to_path_buf());
    }

    /// Configure Master KEK for snapshot AEAD encryption & integrity protection (Spec §7, P1-03).
    pub fn set_master_kek(&self, kek: crate::crypto::MasterKek) {
        *self.master_kek.write().unwrap() = Some(kek);
    }

    /// Retrieve the current Master KEK, if configured.
    pub fn master_kek(&self) -> Option<crate::crypto::MasterKek> {
        self.master_kek.read().unwrap().clone()
    }

    /// Flush current state to disk if persistence_path is configured.
    pub fn sync_to_disk(&self) {
        if let Err(error) = self.sync_to_disk_checked() {
            eprintln!("[kiro-billing] sync_to_disk failed: {error}");
        }
    }

    /// Try to restore persistence after a failed write by committing the current state
    /// — but only a state the loader would accept. Writing whatever is in memory would
    /// turn an in-memory fault into a service that cannot start.
    pub fn probe_persistence(&self) -> Result<(), BillingError> {
        if self.persistence_path.read().unwrap().is_none() {
            return Ok(());
        }
        validate_snapshot(&self.export_snapshot())
            .map_err(|error| BillingError::Persistence(error.to_string()))?;
        self.sync_to_disk_checked()
    }

    /// Persist the current state and propagate failures to the caller.
    ///
    /// Financial and identity mutations use this method so an I/O failure is
    /// never silently converted into a successful API response.  In-memory
    /// engines without a configured path remain valid for isolated tests.
    pub fn sync_to_disk_checked(&self) -> Result<(), BillingError> {
        let path_opt = self.persistence_path.read().unwrap().clone();
        let Some(path) = path_opt else {
            return Ok(());
        };
        self.save_to_file(path).map_err(|error| {
            let message = error.to_string();
            *self.last_persistence_error.write().unwrap() = Some(message.clone());
            BillingError::Persistence(message)
        })
    }

    /// A committed generation is recoverable even when its convenience mirror
    /// needs repair. sync_to_disk_checked retries the mirror on its next publish.
    pub fn last_snapshot_mirror_error(&self) -> Option<String> {
        self.last_snapshot_mirror_error.read().unwrap().clone()
    }

    pub fn list_unpaid_charges(&self, card_id: Option<&str>) -> Vec<UnpaidCharge> {
        self.unpaid_ledger
            .read()
            .unwrap()
            .iter()
            .filter(|entry| card_id.is_none_or(|id| entry.card_id == id))
            .cloned()
            .collect()
    }

    /// Serialize order replay and card creation across all platform managers.
    pub(crate) fn fulfill_card_order<F>(
        &self,
        order_id: &str,
        fingerprint: &str,
        generate: F,
    ) -> Result<String, BillingError>
    where
        F: FnOnce() -> Result<(Vec<Card>, String), BillingError>,
    {
        let _state_guard = self.state_lock.write().unwrap();
        if let Some(existing) = self.issuance_orders.read().unwrap().get(order_id) {
            if existing.fingerprint != fingerprint {
                return Err(BillingError::InvalidState(
                    "order_id was already used with a different request".into(),
                ));
            }
            return Ok(existing.response_json.clone());
        }
        let sequence = self
            .snapshot_sequence
            .load(Ordering::Acquire)
            .saturating_add(1);
        let mut candidate = self.export_snapshot_locked(
            sequence,
            self.last_snapshot_checksum.read().unwrap().clone(),
        );
        let (cards, response_json) = generate()?;
        for card in &cards {
            if candidate
                .groups
                .get(&card.group_id)
                .is_some_and(|g| !g.issuance_enabled)
            {
                return Err(BillingError::GroupIssuanceDisabled);
            }
            if candidate.cards.contains_key(&card.id) {
                return Err(BillingError::InvalidState(
                    "generated card ID collision".into(),
                ));
            }
            candidate.cards.insert(card.id.clone(), card.clone());
        }
        let record = IssuanceOrder {
            fingerprint: fingerprint.to_string(),
            response_json: response_json.clone(),
        };
        candidate
            .issuance_orders
            .insert(order_id.to_string(), record.clone());
        self.commit_candidate_snapshot(&candidate, || {
            let mut current = self.cards.write().unwrap();
            for card in cards {
                current.insert(card.id.clone(), card);
            }
            self.issuance_orders
                .write()
                .unwrap()
                .insert(order_id.to_string(), record);
            response_json
        })
    }

    /// Get the current snapshot publication sequence.
    pub fn snapshot_sequence(&self) -> u64 {
        self.snapshot_sequence.load(Ordering::Acquire)
    }

    /// Get the checksum of the last published snapshot.
    pub fn last_snapshot_checksum(&self) -> Option<String> {
        self.last_snapshot_checksum.read().unwrap().clone()
    }

    /// Return the last persistence failure, if any. Readiness checks should fail
    /// closed when this is set because successful in-memory mutations are not durable.
    pub fn last_persistence_error(&self) -> Option<String> {
        self.last_persistence_error.read().unwrap().clone()
    }

    /// Whether the configured persistence target has acknowledged the latest
    /// in-memory mutation.  A missing path is intentionally considered ready
    /// only for embedded/unit-test engines; production startup configures one.
    pub fn persistence_ready(&self) -> bool {
        self.persistence_path.read().unwrap().is_none()
            || self.last_persistence_error.read().unwrap().is_none()
    }

    /// Fault injection hook for automated testing of persistence failures (T02).
    pub fn inject_persistence_fault(&self, fail: bool) {
        *self.injected_persistence_fault.write().unwrap() = fail;
    }

    /// Atomically persist snapshot to file (writes to .tmp then renames).
    /// If MasterKek is configured, state is encrypted via AES-256-GCM AEAD and wrapped in an integrity envelope (Spec §7, P1-03).
    pub fn save_to_file<P: AsRef<std::path::Path>>(&self, path: P) -> std::io::Result<()> {
        // Same lock order as financial transactions: state -> persistence.
        // Hold state through publication, including sequence allocation.
        let _state_guard = self.state_lock.write().unwrap();
        let sequence = self
            .snapshot_sequence
            .load(Ordering::Acquire)
            .saturating_add(1);
        let previous_checksum = self.last_snapshot_checksum.read().unwrap().clone();
        let snapshot = self.export_snapshot_locked(sequence, previous_checksum);
        #[cfg(test)]
        if let Some((captured, resume)) = self.save_snapshot_hook.lock().unwrap().clone() {
            captured.wait();
            resume.wait();
        }
        self.persist_snapshot_to_file(&snapshot, path.as_ref())
    }

    fn persist_snapshot_to_file(
        &self,
        snapshot: &BillingSnapshot,
        path: &std::path::Path,
    ) -> std::io::Result<()> {
        let _persistence_guard = self
            .persistence_lock
            .lock()
            .map_err(|_| std::io::Error::other("billing persistence lock poisoned"))?;

        if *self.injected_persistence_fault.read().unwrap() {
            return Err(std::io::Error::other("injected persistence fault"));
        }

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Compact: every mutation rewrites the whole state, so indentation was paid for on
        // each request, and it counted toward the size ceiling. Both forms load everywhere.
        let json = serde_json::to_string(snapshot)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

        if !snapshot.issuance_orders.is_empty() && self.master_kek.read().unwrap().is_none() {
            return Err(std::io::Error::other(
                "persisting issuance replay secrets requires a master KEK",
            ));
        }
        let file_content = if let Some(ref kek) = *self.master_kek.read().unwrap() {
            let ciphertext = kek.encrypt(&json).map_err(|e| {
                std::io::Error::other(format!("Snapshot AEAD encryption failed: {e}"))
            })?;
            let digest_val = ring::digest::digest(&ring::digest::SHA256, ciphertext.as_bytes());
            let sha256_checksum = crate::crypto::hex::encode(digest_val.as_ref());
            let envelope = EncryptedSnapshotEnvelope {
                format: "kiro-billing-aead-v1".to_string(),
                version: snapshot.version,
                timestamp: snapshot.timestamp,
                sequence: snapshot.sequence,
                previous_checksum: snapshot.previous_checksum.clone(),
                ciphertext,
                sha256_checksum,
            };
            serde_json::to_string_pretty(&envelope)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?
        } else {
            json
        };

        // Enforce hard ceiling check to prevent "can write but cannot read" unrecoverable state (T03)
        if file_content.len() > MAX_SNAPSHOT_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "billing snapshot size {} bytes exceeds maximum allowable limit of {} bytes; snapshot generation aborted to prevent unrecoverable state",
                    file_content.len(),
                    MAX_SNAPSHOT_BYTES
                ),
            ));
        }

        let published_checksum = sha256_hex(file_content.as_bytes());

        let parent = match path.parent() {
            Some(p) if !p.as_os_str().is_empty() => p,
            _ => std::path::Path::new("."),
        };
        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("billing_state.json");
        let gen_file_name = format!("{}.gen_{}", file_name, snapshot.sequence);
        let gen_path = parent.join(&gen_file_name);

        // Step 1: Write generation file with fsync
        write_atomic_bytes(&gen_path, file_content.as_bytes())?;

        // Step 2: ATOMIC COMMIT POINT: Write generation anchor file pointing to this generation
        let anchor = SnapshotAnchor {
            version: SNAPSHOT_VERSION,
            sequence: snapshot.sequence,
            checksum: published_checksum.clone(),
            generation_file: Some(gen_file_name),
        };
        let anchor_json = serde_json::to_string_pretty(&anchor)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        write_atomic_bytes(&snapshot_anchor_path(path), anchor_json.as_bytes())?;

        // The anchor is committed. A target mirror error must NOT roll back
        // memory or report this transaction as uncommitted.
        #[cfg(test)]
        let mirror_result = if *self.injected_mirror_fault.read().unwrap() {
            Err(std::io::Error::other("injected post-anchor mirror fault"))
        } else {
            write_atomic_bytes(path, file_content.as_bytes())
        };
        #[cfg(not(test))]
        let mirror_result = write_atomic_bytes(path, file_content.as_bytes());
        *self.last_snapshot_mirror_error.write().unwrap() =
            mirror_result.err().map(|error| error.to_string());

        // Step 4: Directory sync to persist directory entry changes
        if let Ok(dir) = std::fs::File::open(parent) {
            let _ = dir.sync_all();
        }

        // Step 5: Retain previous recoverable generation; prune older generations
        prune_old_generations(path, snapshot.sequence);

        // Step 6: Update in-memory metadata
        *self.persistence_path.write().unwrap() = Some(path.to_path_buf());
        self.snapshot_sequence
            .store(snapshot.sequence, Ordering::Release);
        *self.last_snapshot_checksum.write().unwrap() = Some(published_checksum);
        *self.last_persistence_error.write().unwrap() = None;
        Ok(())
    }

    pub(crate) fn commit_candidate_snapshot<R, F>(
        &self,
        candidate: &BillingSnapshot,
        publish_fn: F,
    ) -> Result<R, BillingError>
    where
        F: FnOnce() -> R,
    {
        let path_opt = self.persistence_path.read().unwrap().clone();
        if let Some(path) = path_opt {
            if let Err(error) = self.persist_snapshot_to_file(candidate, &path) {
                let message = error.to_string();
                *self.last_persistence_error.write().unwrap() = Some(message.clone());
                return Err(BillingError::Persistence(message));
            }
        }
        let result = publish_fn();
        *self.last_persistence_error.write().unwrap() = None;
        Ok(result)
    }

    /// Load snapshot from file, replacing memory state.
    /// Supports both AEAD-encrypted envelopes and legacy unencrypted JSON snapshots (P1-03, T03).
    ///
    /// If an anchor file exists and target `path` was interrupted mid-rename during a crash,
    /// this loader verifies and self-heals `path` from the committed generation file referenced
    /// by the anchor.
    pub fn load_from_file<P: AsRef<std::path::Path>>(&self, path: P) -> std::io::Result<()> {
        let path = path.as_ref();
        let parent = match path.parent() {
            Some(p) if !p.as_os_str().is_empty() => p,
            _ => std::path::Path::new("."),
        };

        let anchor_path = snapshot_anchor_path(path);
        let (content, anchor_verified) = if anchor_path.exists() {
            let anchor = read_snapshot_anchor(path)?;

            if anchor.version > SNAPSHOT_VERSION {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("unsupported snapshot anchor version {}", anchor.version),
                ));
            }

            let known_sequence = self.snapshot_sequence.load(Ordering::Acquire);
            if anchor.sequence != 0 && known_sequence != 0 && anchor.sequence < known_sequence {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!(
                        "billing snapshot rollback detected: sequence {} is older than known {}",
                        anchor.sequence, known_sequence
                    ),
                ));
            }

            let path_valid = if path.exists() {
                let bytes = std::fs::read(path)?;
                if bytes.len() > MAX_SNAPSHOT_BYTES {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("billing snapshot exceeds {} bytes", MAX_SNAPSHOT_BYTES),
                    ));
                }
                if sha256_hex(&bytes) == anchor.checksum {
                    Some(
                        String::from_utf8(bytes)
                            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?,
                    )
                } else {
                    None
                }
            } else {
                None
            };

            let loaded_content = if let Some(valid_content) = path_valid {
                valid_content
            } else if let Some(ref gen_file) = anchor.generation_file {
                let gen_path = parent.join(gen_file);
                if gen_path.exists() {
                    let gen_bytes = std::fs::read(&gen_path)?;
                    if gen_bytes.len() > MAX_SNAPSHOT_BYTES {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            format!("generation file exceeds {} bytes", MAX_SNAPSHOT_BYTES),
                        ));
                    }
                    if sha256_hex(&gen_bytes) == anchor.checksum {
                        // Crash occurred between anchor update and path update: recover from generation file and self-heal path
                        let _ = write_atomic_bytes(path, &gen_bytes);
                        String::from_utf8(gen_bytes)
                            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?
                    } else {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "billing snapshot anchor checksum mismatch: neither target path nor generation file matched anchor checksum",
                        ));
                    }
                } else {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!(
                            "billing snapshot anchor points to missing generation file {:?} and target path checksum mismatched",
                            gen_file
                        ),
                    ));
                }
            } else {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "billing snapshot anchor checksum mismatch and no generation file specified",
                ));
            };

            (loaded_content, true)
        } else {
            if *self.require_anchor.read().unwrap() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "Snapshot anchor is required (KIRO_REQUIRE_ANCHOR) but .anchor file is missing",
                ));
            }
            let metadata = std::fs::metadata(path)?;
            if metadata.len() > MAX_SNAPSHOT_BYTES as u64 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("billing snapshot exceeds {} bytes", MAX_SNAPSHOT_BYTES),
                ));
            }
            (std::fs::read_to_string(path)?, false)
        };

        // 1. Try decoding as AEAD encrypted envelope
        if let Ok(envelope) = serde_json::from_str::<EncryptedSnapshotEnvelope>(&content) {
            if envelope.format == "kiro-billing-aead-v1" {
                let kek = self.master_kek.read().unwrap().clone().ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "Snapshot is encrypted with AEAD; MasterKek required to decrypt",
                    )
                })?;

                // Validate integrity checksum
                let digest_val =
                    ring::digest::digest(&ring::digest::SHA256, envelope.ciphertext.as_bytes());
                let actual_checksum = crate::crypto::hex::encode(digest_val.as_ref());
                if envelope.sha256_checksum != actual_checksum {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "Snapshot checksum mismatch; snapshot may be corrupted or tampered",
                    ));
                }

                // Decrypt authenticated ciphertext
                let decrypted_json = kek.decrypt(&envelope.ciphertext).map_err(|e| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!(
                            "Snapshot AEAD decryption failed (invalid key or tampered data): {e}"
                        ),
                    )
                })?;

                let snapshot: BillingSnapshot = serde_json::from_str(&decrypted_json)
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
                validate_snapshot(&snapshot)?;
                if snapshot.version > SNAPSHOT_VERSION
                    || envelope.version != snapshot.version
                    || envelope.sequence != snapshot.sequence
                {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "unsupported or inconsistent billing snapshot version/sequence",
                    ));
                }
                if anchor_verified {
                    let anchor = read_snapshot_anchor(path)?;
                    if anchor.sequence != snapshot.sequence || anchor.version != snapshot.version {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "snapshot anchor version/sequence mismatch",
                        ));
                    }
                } else {
                    self.ensure_snapshot_not_rolled_back(path, snapshot.sequence)?;
                }
                self.import_snapshot(snapshot);
                *self.persistence_path.write().unwrap() = Some(path.to_path_buf());
                *self.last_snapshot_checksum.write().unwrap() =
                    Some(sha256_hex(content.as_bytes()));
                return Ok(());
            }
        }

        // 2. Backward compatibility fallback for unencrypted JSON snapshots
        if self.master_kek.read().unwrap().is_some()
            && std::env::var("ALLOW_PLAINTEXT_SNAPSHOTS").as_deref() != Ok("true")
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "Plaintext snapshot rejected: MasterKek is configured and ALLOW_PLAINTEXT_SNAPSHOTS is not enabled. Explicit authorization required for legacy migration.",
            ));
        }

        let snapshot: BillingSnapshot = serde_json::from_str(&content)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        validate_snapshot(&snapshot)?;
        if snapshot.version > SNAPSHOT_VERSION {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "unsupported billing snapshot version",
            ));
        }
        if anchor_verified {
            let anchor = read_snapshot_anchor(path)?;
            if anchor.sequence != snapshot.sequence || anchor.version != snapshot.version {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "snapshot anchor version/sequence mismatch",
                ));
            }
        } else {
            self.ensure_snapshot_not_rolled_back(path, snapshot.sequence)?;
        }
        self.import_snapshot(snapshot);
        *self.persistence_path.write().unwrap() = Some(path.to_path_buf());
        *self.last_snapshot_checksum.write().unwrap() = Some(sha256_hex(content.as_bytes()));
        Ok(())
    }

    fn ensure_snapshot_not_rolled_back(
        &self,
        path: &std::path::Path,
        sequence: u64,
    ) -> std::io::Result<()> {
        let known_sequence = self.snapshot_sequence.load(Ordering::Acquire);
        if sequence != 0 && known_sequence != 0 && sequence < known_sequence {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "billing snapshot rollback detected: sequence {} is older than known {}",
                    sequence, known_sequence
                ),
            ));
        }

        let anchor_path = snapshot_anchor_path(path);
        if !anchor_path.exists() {
            return Ok(());
        }
        let anchor_content = std::fs::read_to_string(&anchor_path)?;
        let anchor: SnapshotAnchor = serde_json::from_str(&anchor_content).map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("invalid billing snapshot anchor: {e}"),
            )
        })?;
        let actual_checksum = sha256_hex(&std::fs::read(path)?);
        if anchor.version > SNAPSHOT_VERSION
            || anchor.checksum != actual_checksum
            || (anchor.sequence != 0 && anchor.sequence != sequence)
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "billing snapshot anchor mismatch or rollback detected",
            ));
        }
        Ok(())
    }

    /// Add or update a card.
    pub fn upsert_card(&self, card: Card) {
        let _ = self.upsert_cards_checked(std::iter::once(card));
    }

    /// Atomically insert a batch of cards and publish one durable snapshot.
    pub fn upsert_cards_checked<I>(&self, cards: I) -> Result<(), BillingError>
    where
        I: IntoIterator<Item = Card>,
    {
        self.write_cards_checked(cards, false)
    }

    /// Issuance is insert-only: a collision must never replace an existing balance.
    pub fn insert_new_cards_checked<I>(&self, cards: I) -> Result<(), BillingError>
    where
        I: IntoIterator<Item = Card>,
    {
        self.write_cards_checked(cards, true)
    }

    fn write_cards_checked<I>(&self, cards: I, insert_only: bool) -> Result<(), BillingError>
    where
        I: IntoIterator<Item = Card>,
    {
        let cards_vec: Vec<Card> = cards.into_iter().collect();
        let _state_guard = self.state_lock.write().unwrap();
        let sequence = self
            .snapshot_sequence
            .load(Ordering::Acquire)
            .saturating_add(1);
        let previous_checksum = self.last_snapshot_checksum.read().unwrap().clone();
        let mut candidate = self.export_snapshot_locked(sequence, previous_checksum);

        for card in &cards_vec {
            if !candidate.cards.contains_key(&card.id)
                && candidate
                    .groups
                    .get(&card.group_id)
                    .is_some_and(|g| !g.issuance_enabled)
            {
                return Err(BillingError::GroupIssuanceDisabled);
            }
            if insert_only && candidate.cards.contains_key(&card.id) {
                return Err(BillingError::InvalidState(
                    "Card ID collision; batch not issued".into(),
                ));
            }
            candidate.cards.insert(card.id.clone(), card.clone());
        }

        self.commit_candidate_snapshot(&candidate, || {
            let mut w = self.cards.write().unwrap();
            for card in cards_vec {
                w.insert(card.id.clone(), card);
            }
        })
    }

    /// Issue recoverable cards atomically; missing KEK never falls back to plaintext.
    pub fn issue_cards(
        &self,
        template: &crate::template::CardTemplate,
        count: usize,
        note: Option<&str>,
        now_secs: u64,
    ) -> Result<Vec<crate::generator::GeneratedCard>, BillingError> {
        let generated = self.generate_recoverable_cards(template, count, note, now_secs)?;
        self.insert_new_cards_checked(generated.iter().map(|item| item.card.clone()))?;
        Ok(generated)
    }

    pub(crate) fn generate_recoverable_cards(
        &self,
        template: &crate::template::CardTemplate,
        count: usize,
        note: Option<&str>,
        now_secs: u64,
    ) -> Result<Vec<crate::generator::GeneratedCard>, BillingError> {
        let kek = self.master_kek().ok_or_else(|| {
            BillingError::InvalidState("MasterKek required for card issuance".into())
        })?;
        let mut generated = crate::generator::generate_batch(template, count, note, now_secs)?;
        for item in &mut generated {
            // Authenticated identity and purpose prevent ciphertext substitution.
            let payload = serde_json::to_string(&(
                "card-code-v1",
                &item.card.id,
                &item.card.code_hash,
                &item.raw_code,
            ))
            .map_err(|_| BillingError::InvalidState("Card encryption failed".into()))?;
            item.card.code_encrypted = Some(
                kek.encrypt(&payload)
                    .map_err(|_| BillingError::InvalidState("Card encryption failed".into()))?,
            );
        }
        Ok(generated)
    }

    /// Legacy cards have no recovery material. Never return unverified plaintext.
    pub fn reveal_card_code(&self, card_id: &str) -> Result<Option<String>, BillingError> {
        let card = self
            .get_card(card_id)
            .ok_or_else(|| BillingError::CardNotFound(card_id.to_owned()))?;
        let Some(ciphertext) = card.code_encrypted.as_deref() else {
            return Ok(None);
        };
        let failed = || BillingError::InvalidState("Card code recovery failed".into());
        // The KEK wire format is ASCII hex; reject malformed UTF-8 text before decoding.
        if !ciphertext.is_ascii() {
            return Err(failed());
        }
        let kek = self.master_kek().ok_or_else(failed)?;
        let plaintext = kek.decrypt(ciphertext).map_err(|_| failed())?;
        let (purpose, id, hash, raw): (String, String, String, String) =
            serde_json::from_str(&plaintext).map_err(|_| failed())?;
        if purpose != "card-code-v1"
            || id != card.id
            || hash != card.code_hash
            || !crate::card::constant_time_eq(
                crate::card::hash_card_code(&raw).as_bytes(),
                card.code_hash.as_bytes(),
            )
        {
            return Err(failed());
        }
        Ok(Some(raw))
    }

    /// Get card snapshot.
    pub fn get_card(&self, card_id: &str) -> Option<Card> {
        let r = self.cards.read().unwrap();
        r.get(card_id).cloned()
    }

    /// Find a card by its raw secret only. Stored hashes are not credentials.
    /// Card IDs are intentionally excluded so public authentication cannot be
    /// performed with an identifier that is visible in normal API responses.
    pub fn find_card_by_secret(&self, raw_secret: &str) -> Option<Card> {
        if raw_secret.trim().is_empty() {
            return None;
        }
        let hash = crate::card::hash_card_code(raw_secret);
        let r = self.cards.read().unwrap();
        r.values()
            .find(|c| crate::card::constant_time_eq(c.code_hash.as_bytes(), hash.as_bytes()))
            .cloned()
    }

    /// Find a card by a platform-supplied secret code, hash, or internal ID.
    /// This broader lookup is restricted to authenticated platform callbacks.
    pub fn find_card_by_code_or_id(&self, raw_or_hash_or_id: &str) -> Option<Card> {
        self.find_card_by_secret(raw_or_hash_or_id).or_else(|| {
            let cards = self.cards.read().unwrap();
            cards.get(raw_or_hash_or_id).cloned().or_else(|| {
                cards
                    .values()
                    .find(|c| {
                        crate::card::constant_time_eq(
                            c.code_hash.as_bytes(),
                            raw_or_hash_or_id.as_bytes(),
                        )
                    })
                    .cloned()
            })
        })
    }

    /// Backward-compatible alias for internal callers. Public login/portal
    /// handlers use `find_card_by_secret` explicitly.
    pub fn find_card_by_code(&self, raw_or_hash: &str) -> Option<Card> {
        self.find_card_by_code_or_id(raw_or_hash)
    }

    /// List all card snapshots in store.
    pub fn list_all_cards(&self) -> Vec<Card> {
        let r = self.cards.read().unwrap();
        r.values().cloned().collect()
    }

    /// Activate a card on first login ("激活即计时", Spec §5).
    pub fn activate_card(
        &self,
        card_id: &str,
        now_secs: u64,
        duration_secs: u64,
    ) -> Result<Card, BillingError> {
        let _state_guard = self.state_lock.write().unwrap();
        let sequence = self
            .snapshot_sequence
            .load(Ordering::Acquire)
            .saturating_add(1);
        let previous_checksum = self.last_snapshot_checksum.read().unwrap().clone();
        let mut candidate = self.export_snapshot_locked(sequence, previous_checksum);

        let card = candidate
            .cards
            .get_mut(card_id)
            .ok_or_else(|| BillingError::CardNotFound(card_id.to_string()))?;

        if matches!(
            card.status,
            CardStatus::Frozen | CardStatus::Banned | CardStatus::Voided
        ) {
            return Err(BillingError::Card(CardError::NotActive(card.status)));
        }
        card.activate(now_secs, duration_secs)?;
        let updated_card = card.clone();

        self.commit_candidate_snapshot(&candidate, || {
            self.cards
                .write()
                .unwrap()
                .insert(card_id.to_string(), updated_card.clone());
            updated_card
        })
    }

    /// Atomically activate a card and optionally bind its first device.
    /// This prevents a failed device bind from leaving a card activated while
    /// reporting an unsuccessful login/activation operation.
    pub fn activate_card_with_device(
        &self,
        card_id: &str,
        now_secs: u64,
        duration_secs: u64,
        device_fp: Option<&str>,
    ) -> Result<Card, BillingError> {
        let _state_guard = self.state_lock.write().unwrap();
        let sequence = self
            .snapshot_sequence
            .load(Ordering::Acquire)
            .saturating_add(1);
        let previous_checksum = self.last_snapshot_checksum.read().unwrap().clone();
        let mut candidate = self.export_snapshot_locked(sequence, previous_checksum);

        // A card has one device; a device may use independently purchased cards.
        let card = candidate
            .cards
            .get_mut(card_id)
            .ok_or_else(|| BillingError::CardNotFound(card_id.to_string()))?;
        card.activate(now_secs, duration_secs)?;
        if let Some(device) = device_fp.filter(|d| !d.trim().is_empty()) {
            bind_device_locked(card, device.trim())?;
        }
        // A sign-in starts a new refresh family: every refresh token issued before it,
        // including one copied off the device, stops working.
        card.refresh_version = card.refresh_version.saturating_add(1).max(1);
        card.refresh_rotated_at = None;
        let updated_card = card.clone();

        self.commit_candidate_snapshot(&candidate, || {
            self.cards
                .write()
                .unwrap()
                .insert(card_id.to_string(), updated_card.clone());
            updated_card
        })
    }

    /// Set pricing rates for a specific model.
    pub fn set_model_rates(&self, model: &str, rates: PricingRates) {
        {
            let _state_guard = self.state_lock.write().unwrap();
            let mut w = self.rates.write().unwrap();
            w.insert(model.to_string(), rates);
        }
        self.sync_to_disk();
    }

    pub fn upsert_provider(&self, provider: Provider) {
        // Compatibility API for in-memory callers. Production mutations use
        // `upsert_providers_checked`, which propagates persistence failures.
        {
            let _state_guard = self.state_lock.write().unwrap();
            self.providers
                .write()
                .unwrap()
                .insert(provider.id.clone(), provider);
        }
        self.sync_to_disk();
    }

    pub fn upsert_providers_checked<I, K>(&self, providers: I, keys: K) -> Result<(), BillingError>
    where
        I: IntoIterator<Item = Provider>,
        K: IntoIterator<Item = ProviderKey>,
    {
        self.upsert_provider_batch(providers, keys, false)
            .map(|_| ())
    }

    /// Import preserves existing routing ownership atomically with credential updates.
    pub fn import_providers_checked<I, K>(
        &self,
        providers: I,
        keys: K,
    ) -> Result<Vec<Provider>, BillingError>
    where
        I: IntoIterator<Item = Provider>,
        K: IntoIterator<Item = ProviderKey>,
    {
        self.upsert_provider_batch(providers, keys, true)
    }

    fn upsert_provider_batch<I, K>(
        &self,
        providers: I,
        keys: K,
        preserve_group: bool,
    ) -> Result<Vec<Provider>, BillingError>
    where
        I: IntoIterator<Item = Provider>,
        K: IntoIterator<Item = ProviderKey>,
    {
        let mut providers: Vec<Provider> = providers.into_iter().collect();
        let mut keys: Vec<ProviderKey> = keys.into_iter().collect();
        let kek = if keys.is_empty() {
            None
        } else {
            Some(self.master_kek.read().unwrap().clone().ok_or_else(|| {
                BillingError::Persistence(
                    "provider key persistence requires KIRO_MASTER_KEK".to_string(),
                )
            })?)
        };
        if let Some(ref kek) = kek {
            for key in &mut keys {
                key.api_key_encrypted = Some(
                    kek.encrypt(&key.api_key)
                        .map_err(|error| BillingError::Persistence(error.to_string()))?,
                );
            }
        }
        let _state_guard = self.state_lock.write().unwrap();
        let sequence = self
            .snapshot_sequence
            .load(Ordering::Acquire)
            .saturating_add(1);
        let previous_checksum = self.last_snapshot_checksum.read().unwrap().clone();
        let mut candidate = self.export_snapshot_locked(sequence, previous_checksum);

        for provider in &mut providers {
            if preserve_group {
                if let Some(existing) = candidate.providers.get(&provider.id) {
                    provider.group_id = existing.group_id.clone();
                }
            }
            candidate
                .providers
                .insert(provider.id.clone(), provider.clone());
        }
        for key in &keys {
            candidate.provider_keys.insert(key.id.clone(), key.clone());
        }

        let providers_clone = providers.clone();
        let keys_clone = keys;

        self.commit_candidate_snapshot(&candidate, || {
            let mut provider_store = self.providers.write().unwrap();
            for provider in providers_clone {
                provider_store.insert(provider.id.clone(), provider);
            }
            let mut key_store = self.provider_keys.write().unwrap();
            for key in keys_clone {
                key_store.insert(key.id.clone(), key);
            }
        })?;
        Ok(providers)
    }

    pub fn list_providers(&self) -> Vec<Provider> {
        let mut providers: Vec<_> = self.providers.read().unwrap().values().cloned().collect();
        providers.sort_by(|a, b| a.id.cmp(&b.id));
        providers
    }

    /// Retrieve a single provider by ID (T04).
    pub fn get_provider(&self, provider_id: &str) -> Option<Provider> {
        self.providers.read().unwrap().get(provider_id).cloned()
    }

    /// Retrieve unmasked provider keys for internal runtime routing and key pooling (T04).
    /// Public/admin APIs must use `list_provider_keys` which strictly zeroes out API keys.
    pub fn get_runtime_provider_keys(&self, provider_id: Option<&str>) -> Vec<ProviderKey> {
        let kek = self.master_kek.read().unwrap().clone();
        let mut keys: Vec<_> = self
            .provider_keys
            .read()
            .unwrap()
            .values()
            .filter(|key| provider_id.is_none_or(|id| key.provider_id == id))
            .cloned()
            .collect();
        for key in &mut keys {
            if key.api_key.is_empty() {
                if let Some(ref kek) = kek {
                    if let Some(ciphertext) = key.api_key_encrypted.as_deref() {
                        key.api_key = kek.decrypt(ciphertext).unwrap_or_default();
                    }
                }
            }
        }
        keys.sort_by(|a, b| a.id.cmp(&b.id));
        keys
    }

    pub fn upsert_provider_key(&self, mut key: ProviderKey) {
        if let Some(kek) = self.master_kek.read().unwrap().clone() {
            if let Ok(encrypted) = kek.encrypt(&key.api_key) {
                key.api_key_encrypted = Some(encrypted);
            }
        }
        {
            let _state_guard = self.state_lock.write().unwrap();
            self.provider_keys
                .write()
                .unwrap()
                .insert(key.id.clone(), key);
        }
        self.sync_to_disk();
    }

    /// Insert a provider key only when it can be encrypted for durable
    /// storage. Runtime/admin import uses this checked path.
    pub fn upsert_provider_key_checked(&self, mut key: ProviderKey) -> Result<(), BillingError> {
        let kek = self.master_kek.read().unwrap().clone().ok_or_else(|| {
            BillingError::Persistence(
                "provider key persistence requires KIRO_MASTER_KEK".to_string(),
            )
        })?;
        key.api_key_encrypted = Some(
            kek.encrypt(&key.api_key)
                .map_err(|error| BillingError::Persistence(error.to_string()))?,
        );

        let _state_guard = self.state_lock.write().unwrap();
        let sequence = self
            .snapshot_sequence
            .load(Ordering::Acquire)
            .saturating_add(1);
        let previous_checksum = self.last_snapshot_checksum.read().unwrap().clone();
        let mut candidate = self.export_snapshot_locked(sequence, previous_checksum);

        candidate.provider_keys.insert(key.id.clone(), key.clone());
        let key_clone = key;

        self.commit_candidate_snapshot(&candidate, || {
            self.provider_keys
                .write()
                .unwrap()
                .insert(key_clone.id.clone(), key_clone);
        })
    }

    pub fn list_provider_keys(&self, provider_id: Option<&str>) -> Vec<ProviderKey> {
        let mut keys: Vec<_> = self
            .provider_keys
            .read()
            .unwrap()
            .values()
            .filter(|key| provider_id.is_none_or(|id| key.provider_id == id))
            .cloned()
            .collect();
        for key in &mut keys {
            key.api_key.clear();
            key.api_key_encrypted = None;
        }
        keys.sort_by(|a, b| a.id.cmp(&b.id));
        keys
    }

    pub fn set_provider_enabled(
        &self,
        provider_id: &str,
        enabled: bool,
    ) -> Result<bool, BillingError> {
        let _state_guard = self.state_lock.write().unwrap();
        let sequence = self
            .snapshot_sequence
            .load(Ordering::Acquire)
            .saturating_add(1);
        let previous_checksum = self.last_snapshot_checksum.read().unwrap().clone();
        let mut candidate = self.export_snapshot_locked(sequence, previous_checksum);

        let Some(provider) = candidate.providers.get_mut(provider_id) else {
            return Ok(false);
        };
        provider.enabled = enabled;
        let updated_provider = provider.clone();
        let updated_provider_id = updated_provider.id.clone();

        self.commit_candidate_snapshot(&candidate, || {
            self.providers
                .write()
                .unwrap()
                .insert(updated_provider_id, updated_provider);
            true
        })
    }

    /// Step 1: Pre-request credit reservation (Spec §6.2).
    ///
    /// Freezes upper-bound estimated credits before forwarding request to upstream provider.
    pub fn reserve(
        &self,
        card_id: &str,
        invocation_id: &str,
        params: &ReservationEstimateParams,
        now_secs: u64,
        ttl_secs: u64,
    ) -> Result<CreditReservation, BillingError> {
        if !self.persistence_ready() {
            return Err(BillingError::Persistence(
                self.last_persistence_error()
                    .unwrap_or_else(|| "persistence engine is unhealthy".to_string()),
            ));
        }

        let _state_guard = self.state_lock.write().unwrap();
        let sequence = self
            .snapshot_sequence
            .load(Ordering::Acquire)
            .saturating_add(1);
        let previous_checksum = self.last_snapshot_checksum.read().unwrap().clone();
        let mut candidate = self.export_snapshot_locked(sequence, previous_checksum);

        // 1. Check duplicate invocation ID. A released reservation represents
        // zero consumed work and may be retried with the same client ID; held,
        // settled and pending records remain immutable for billing safety.
        if let Some(existing) = candidate.reservations.get(invocation_id) {
            if existing.state == ReservationState::Released
                && !candidate.pending_settlements.contains_key(invocation_id)
            {
                candidate.reservations.remove(invocation_id);
            } else {
                return Err(BillingError::DuplicateInvocation(invocation_id.to_string()));
            }
        }

        // 2. Fetch card
        let mut card = candidate
            .cards
            .get(card_id)
            .cloned()
            .ok_or_else(|| BillingError::CardNotFound(card_id.to_string()))?;

        // 3. Resolve rate card version if model is specified
        let (reserve_amount, resolved_version) = if let Some(ref model) = params.model {
            let group = candidate.groups.get(&card.group_id);
            let rate_card_id = group.map(|g| g.rate_card_id.as_str()).unwrap_or("default");
            let group_margin = group.map(|g| g.margin_multiplier).unwrap_or(1.0);

            let model_map = candidate
                .model_maps
                .iter()
                .find(|m| m.group_id == card.group_id && m.matches_model(model));
            let model_multiplier = model_map
                .map(|m| m.credit_multiplier)
                .unwrap_or(params.credit_multiplier);

            let settings = candidate.settings.clone();
            let resolved_rcv = self
                .resolve_rate_card_version(rate_card_id, model, now_secs)
                .or_else(|| {
                    model_map.and_then(|m| {
                        self.resolve_rate_card_version(rate_card_id, &m.target_model, now_secs)
                    })
                });
            if let Some(rcv) = resolved_rcv {
                let amt = rcv.calculate_reserve_amount(
                    params.estimated_input_tokens,
                    params.max_output_tokens,
                    group_margin,
                    model_multiplier,
                    &settings,
                );
                (amt, Some(rcv.id))
            } else {
                (params.calculate_reserve_amount(), None)
            }
        } else {
            (params.calculate_reserve_amount(), None)
        };

        if reserve_amount < 0 {
            return Err(BillingError::InvalidState(
                "calculated reservation amount is negative".to_string(),
            ));
        }

        // 3a. Check card concurrency quota (Spec §14.9 Fair-Use)
        let active_concurrency = candidate
            .reservations
            .values()
            .filter(|r| r.card_id == card_id && r.state == ReservationState::Held)
            .count() as u32;

        if active_concurrency >= card.max_concurrency {
            return Err(BillingError::ConcurrencyLimitExceeded {
                current: active_concurrency,
                max: card.max_concurrency,
            });
        }

        candidate.check_quota(&card, reserve_amount, now_secs)?;
        if card.outstanding_debt() > 0 {
            return Err(BillingError::Card(CardError::InsufficientCredit {
                available: 0,
                needed: reserve_amount.max(1),
            }));
        }

        // 4. Check card status and balance
        card.check_can_reserve(reserve_amount, now_secs)?;

        // 5. Freeze reservation
        card.credit_reserved = card.credit_reserved.saturating_add(reserve_amount);

        let reservation_id = format!("res-{}", invocation_id);
        let mut reservation = CreditReservation::new(
            reservation_id,
            card_id,
            invocation_id,
            reserve_amount,
            now_secs,
            ttl_secs,
        );
        reservation.rate_card_version = resolved_version;

        candidate
            .reservations
            .insert(invocation_id.to_string(), reservation.clone());

        candidate.cards.insert(card_id.to_string(), card.clone());
        let updated_card = card.clone();
        let reservation_clone = reservation.clone();
        let inv_id_clone = invocation_id.to_string();
        let card_id_clone = card_id.to_string();

        self.commit_candidate_snapshot(&candidate, || {
            self.cards
                .write()
                .unwrap()
                .insert(card_id_clone, updated_card);
            self.reservations
                .write()
                .unwrap()
                .insert(inv_id_clone, reservation_clone);
            reservation
        })
    }

    /// Authorize a larger upper bound BEFORE consuming more provider work.
    /// Post-consumption settle always records the actual bill, even beyond quota.
    pub fn extend_reservation(
        &self,
        invocation_id: &str,
        new_amount: i64,
        now_secs: u64,
    ) -> Result<CreditReservation, BillingError> {
        let _state_guard = self.state_lock.write().unwrap();
        let sequence = self
            .snapshot_sequence
            .load(Ordering::Acquire)
            .saturating_add(1);
        let mut candidate = self.export_snapshot_locked(
            sequence,
            self.last_snapshot_checksum.read().unwrap().clone(),
        );
        let mut reservation = candidate
            .reservations
            .get(invocation_id)
            .cloned()
            .ok_or_else(|| BillingError::ReservationNotFound(invocation_id.to_string()))?;
        if reservation.state != ReservationState::Held {
            return Err(BillingError::InvalidState(
                "only a held reservation can be extended".into(),
            ));
        }
        if new_amount < reservation.reserved_micro_credits {
            return Err(BillingError::InvalidState(
                "extension cannot reduce reservation".into(),
            ));
        }
        let extra = new_amount - reservation.reserved_micro_credits;
        let mut card = candidate
            .cards
            .get(&reservation.card_id)
            .cloned()
            .ok_or_else(|| BillingError::CardNotFound(reservation.card_id.clone()))?;
        candidate.check_quota(&card, extra, now_secs)?;
        card.check_can_reserve(extra, now_secs)?;
        if card.outstanding_debt() > 0 {
            return Err(BillingError::Card(CardError::InsufficientCredit {
                available: 0,
                needed: extra.max(1),
            }));
        }
        card.credit_reserved = card
            .credit_reserved
            .checked_add(extra)
            .ok_or_else(|| BillingError::InvalidState("reservation amount overflow".into()))?;
        reservation.reserved_micro_credits = new_amount;
        candidate.cards.insert(card.id.clone(), card.clone());
        candidate
            .reservations
            .insert(invocation_id.to_string(), reservation.clone());
        self.commit_candidate_snapshot(&candidate, || {
            self.cards.write().unwrap().insert(card.id.clone(), card);
            self.reservations
                .write()
                .unwrap()
                .insert(invocation_id.to_string(), reservation.clone());
            reservation
        })
    }

    /// Step 2: Post-request settlement from real provider usage (Spec §6.1, §6.7).
    ///
    /// Deducts actual usage from `credit_used`, unfreezes `credit_reserved`, and writes append-only ledger.
    pub fn settle(
        &self,
        invocation_id: &str,
        tokens: &UsageTokens,
        exposed_model: &str,
        provider_id: &str,
        target_model: &str,
        now_secs: u64,
    ) -> Result<LedgerEntry, BillingError> {
        // Clamp rather than refuse. A refusal here happens before the intent is durable,
        // and a request whose settlement is refused is refunded once the janitor
        // reclaims the hold — so every new refusal would be a free request.
        let tokens = &tokens.clamped();
        let _state_guard = self.state_lock.write().unwrap();
        let existing = self
            .pending_settlements
            .read()
            .unwrap()
            .get(invocation_id)
            .cloned();
        if let Some(pending) = existing {
            if pending.tokens.clamped() != *tokens
                || pending.entry.exposed_model != exposed_model
                || pending.entry.provider_id != provider_id
                || pending.entry.target_model != target_model
            {
                return Err(BillingError::InvalidState(
                    "pending settlement replay conflict".into(),
                ));
            }
            return self.complete_pending_settlement_locked(invocation_id, &pending);
        }
        let sequence = self
            .snapshot_sequence
            .load(Ordering::Acquire)
            .saturating_add(1);
        let previous_checksum = self.last_snapshot_checksum.read().unwrap().clone();
        let mut candidate = self.export_snapshot_locked(sequence, previous_checksum);

        // 1. Fetch reservation
        let (reservation_card_id, reservation_created_at, locked_version) = {
            let reservation = candidate
                .reservations
                .get(invocation_id)
                .ok_or_else(|| BillingError::ReservationNotFound(invocation_id.to_string()))?;
            match reservation.state {
                ReservationState::Settled => {
                    return Err(BillingError::DuplicateInvocation(invocation_id.to_string()));
                }
                ReservationState::Released => {
                    return Err(BillingError::ReservationAlreadyReleased(
                        invocation_id.to_string(),
                    ));
                }
                ReservationState::Held => {}
            }
            (
                reservation.card_id.clone(),
                reservation.created_at_secs,
                reservation.rate_card_version.clone(),
            )
        };

        // 2. Fetch card
        let card = candidate
            .cards
            .get_mut(&reservation_card_id)
            .ok_or_else(|| BillingError::CardNotFound(reservation_card_id.clone()))?;

        // 3. Resolve rate card version & multipliers
        let group = candidate.groups.get(&card.group_id);
        let rate_card_id = group.map(|g| g.rate_card_id.as_str()).unwrap_or("default");
        let group_margin = group.map(|g| g.margin_multiplier).unwrap_or(1.0);

        let model_map = candidate
            .model_maps
            .iter()
            .find(|m| m.group_id == card.group_id && m.matches_model(exposed_model));
        let model_multiplier = model_map
            .as_ref()
            .map(|m| m.credit_multiplier)
            .unwrap_or(1.0);

        let resolved_rcv = if let Some(ref vid) = locked_version {
            self.get_rate_card_version(vid)
        } else {
            self.resolve_rate_card_version(rate_card_id, exposed_model, reservation_created_at)
                .or_else(|| {
                    model_map.as_ref().and_then(|m| {
                        self.resolve_rate_card_version(
                            rate_card_id,
                            &m.target_model,
                            reservation_created_at,
                        )
                    })
                })
                .or_else(|| {
                    self.resolve_rate_card_version(
                        rate_card_id,
                        target_model,
                        reservation_created_at,
                    )
                })
        };

        let settings = candidate.settings.clone();
        let (charge, mut cost_micro_cny, version_id) = if let Some(ref rcv) = resolved_rcv {
            let cost = rcv.calculate_cost_micro_cny(tokens, &settings);
            let charge = rcv.calculate_charge(tokens, group_margin, model_multiplier, &settings);
            (charge, cost, Some(rcv.id.clone()))
        } else {
            let rates_read = self.rates.read().unwrap();
            let model_rate = rates_read.get(exposed_model).unwrap_or(&self.default_rates);
            let charge = model_rate.calculate_charge(tokens);
            let cost = crate::ledger::ceil_nonnegative_to_i64(
                (tokens.uncached_input_tokens as f64 * 15.0
                    + tokens.cache_creation_tokens as f64 * 15.0
                    + tokens.cache_read_tokens as f64 * 3.75
                    + tokens.output_tokens as f64 * 60.0)
                    / 1_000_000.0
                    * 10_000.0,
            );
            (charge, cost, None)
        };

        // Provider-qualified model prices take precedence over shared target-model prices.
        let qualified_model = format!("{provider_id}/{target_model}");
        let cost_version = [qualified_model.as_str(), target_model, "*"]
            .into_iter()
            .find_map(|model| {
                candidate
                    .rate_card_versions
                    .iter()
                    .filter(|v| {
                        v.rate_card_id == rate_card_id
                            && v.model == model
                            && v.effective_from_secs <= reservation_created_at
                    })
                    .max_by_key(|v| v.effective_from_secs)
            })
            .or_else(|| {
                model_map
                    .filter(|m| {
                        m.target_provider_id == provider_id && m.target_model == target_model
                    })
                    .and(resolved_rcv.as_ref())
            });
        let cost_source = if let Some(version) = cost_version {
            cost_micro_cny = version.calculate_cost_micro_cny(tokens, &settings);
            format!("provider_cost:rate_card_version={}", version.id)
        } else {
            // ponytail: legacy configurations lack a cost catalog; label the estimate
            // until an operator configures the actual provider/model price.
            "provider_cost:estimated_missing_target_rate".to_string()
        };

        if charge < 0 {
            return Err(BillingError::InvalidState(
                "calculated settlement charge is negative".to_string(),
            ));
        }
        let charge = charge.min(MAX_SETTLEMENT_MICRO_CREDITS);

        let entry = LedgerEntry {
            id: format!("led-{}", invocation_id),
            card_id: card.id.clone(),
            kind: LedgerKind::Usage,
            invocation_id: Some(invocation_id.to_string()),
            exposed_model: exposed_model.to_string(),
            provider_id: provider_id.to_string(),
            target_model: target_model.to_string(),
            input_tokens: tokens
                .uncached_input_tokens
                .saturating_add(tokens.cache_read_tokens)
                .saturating_add(tokens.cache_creation_tokens),
            output_tokens: tokens.output_tokens,
            cache_creation_tokens: tokens.cache_creation_tokens,
            cache_read_tokens: tokens.cache_read_tokens,
            credits_charged: charge,
            provider_cost_micro_cny: cost_micro_cny,
            rate_card_version: version_id,
            ts_secs: now_secs,
            operator_id: None,
            reason: Some(cost_source),
        };
        let pending = PendingSettlement {
            entry,
            tokens: *tokens,
        };
        candidate
            .pending_settlements
            .insert(invocation_id.to_string(), pending.clone());
        // Retain in memory even when storage is wholly unavailable. A successful
        // write makes this intent durable before any debit is attempted.
        let commit = self.commit_candidate_snapshot(&candidate, || {
            self.pending_settlements
                .write()
                .unwrap()
                .insert(invocation_id.to_string(), pending.clone());
        });
        if let Err(error) = commit {
            self.pending_settlements
                .write()
                .unwrap()
                .insert(invocation_id.to_string(), pending);
            return Err(error);
        }
        #[cfg(test)]
        if *self.fail_after_pending.read().unwrap() {
            self.inject_persistence_fault(true);
        }
        self.complete_pending_settlement_locked(invocation_id, &pending)
    }

    pub fn list_pending_settlements(&self) -> Vec<PendingSettlement> {
        self.pending_settlements
            .read()
            .unwrap()
            .values()
            .cloned()
            .collect()
    }

    /// Retry the stored bill without repricing or consulting current quotas.
    pub fn retry_pending_settlement(
        &self,
        invocation_id: &str,
    ) -> Result<LedgerEntry, BillingError> {
        let _state_guard = self.state_lock.write().unwrap();
        if let Some(entry) = self
            .ledger
            .read()
            .unwrap()
            .iter()
            .find(|entry| {
                entry.kind == LedgerKind::Usage
                    && entry.invocation_id.as_deref() == Some(invocation_id)
            })
            .cloned()
        {
            return Ok(entry);
        }
        let pending = self
            .pending_settlements
            .read()
            .unwrap()
            .get(invocation_id)
            .cloned()
            .ok_or_else(|| {
                BillingError::InvalidState("no pending settlement for invocation".into())
            })?;
        self.complete_pending_settlement_locked(invocation_id, &pending)
    }

    fn complete_pending_settlement_locked(
        &self,
        invocation_id: &str,
        pending: &PendingSettlement,
    ) -> Result<LedgerEntry, BillingError> {
        let sequence = self
            .snapshot_sequence
            .load(Ordering::Acquire)
            .saturating_add(1);
        let mut candidate = self.export_snapshot_locked(
            sequence,
            self.last_snapshot_checksum.read().unwrap().clone(),
        );
        let reservation = candidate
            .reservations
            .get_mut(invocation_id)
            .ok_or_else(|| BillingError::ReservationNotFound(invocation_id.into()))?;
        if reservation.state != ReservationState::Held {
            return Err(BillingError::InvalidState(
                "pending reservation is not Held".into(),
            ));
        }
        let mut entry = pending.entry.clone();
        // An intent stored by an older release may carry a saturated charge that could
        // never be added to `credit_used`; clamping it is what lets it complete.
        entry.credits_charged = entry.credits_charged.min(MAX_SETTLEMENT_MICRO_CREDITS);
        if entry.invocation_id.as_deref() != Some(invocation_id)
            || entry.card_id != reservation.card_id
            || entry.kind != LedgerKind::Usage
            || entry.credits_charged < 0
            || entry.provider_cost_micro_cny < 0
        {
            return Err(BillingError::InvalidState(
                "invalid pending settlement identity or amount".into(),
            ));
        }
        // A zero charge is a valid outcome — zero prices are valid configuration — and
        // is recorded like any other. This used to be refused here, after the intent was
        // durable. Recovery retries without repricing, so the refusal was permanent: the
        // card could never reserve again, could not be voided, and pricing publication
        // was refused deployment-wide. Completion may now fail only on persistence.
        let card = candidate
            .cards
            .get_mut(&entry.card_id)
            .ok_or_else(|| BillingError::CardNotFound(entry.card_id.clone()))?;
        // Consumption is a liability, not a new authorization. Debit the full
        // bill even beyond balance/quota; subsequent reservations are blocked.
        let previous_debt = card.outstanding_debt();
        // Saturate rather than fail: failing here is permanent (recovery never reprices),
        // and only a balance an older release already saturated can get this close to
        // the limit. Ledger reconciliation saturates the same way, so the two still agree.
        card.credit_used = card.credit_used.saturating_add(entry.credits_charged);
        card.credit_reserved = card
            .credit_reserved
            .saturating_sub(reservation.reserved_micro_credits);
        let added_debt = card.outstanding_debt().saturating_sub(previous_debt);
        if added_debt > 0 {
            candidate.unpaid_ledger.push(UnpaidCharge {
                invocation_id: invocation_id.into(),
                card_id: card.id.clone(),
                amount_micro_credits: added_debt,
                ts_secs: entry.ts_secs,
            });
        }
        reservation.state = ReservationState::Settled;
        candidate.pending_settlements.remove(invocation_id);
        candidate.ledger.push(entry.clone());
        let mut attempt_chain = Vec::new();
        candidate.traces.retain(|trace| {
            if trace.invocation_id == invocation_id {
                attempt_chain.extend(trace.attempt_chain.clone());
                false
            } else {
                true
            }
        });
        candidate.traces.push(RequestTrace {
            id: format!("trace-{invocation_id}"),
            card_id: entry.card_id.clone(),
            ts: entry.ts_secs,
            invocation_id: invocation_id.into(),
            exposed_model: entry.exposed_model.clone(),
            status: TraceStatus::Success,
            ttft_ms: None,
            tokens_per_second: None,
            error_class: None,
            provider_id: Some(entry.provider_id.clone()),
            input_tokens: entry.input_tokens,
            output_tokens: entry.output_tokens,
            credits_charged: entry.credits_charged,
            provider_cost_micro_cny: entry.provider_cost_micro_cny,
            attempt_chain,
        });
        if candidate.traces.len() > MAX_RETAINED_TRACES {
            candidate
                .traces
                .drain(..candidate.traces.len() - MAX_RETAINED_TRACES);
        }
        self.commit_candidate_snapshot(&candidate, || {
            *self.cards.write().unwrap() = candidate.cards.clone();
            *self.reservations.write().unwrap() = candidate.reservations.clone();
            *self.ledger.write().unwrap() = candidate.ledger.clone();
            *self.unpaid_ledger.write().unwrap() = candidate.unpaid_ledger.clone();
            *self.pending_settlements.write().unwrap() = candidate.pending_settlements.clone();
            *self.traces.write().unwrap() = candidate.traces.clone();
            entry
        })
    }

    /// Step 3: Cancellation / Abort refund (Spec §6.3).
    ///
    /// When a request fails before producing output or client disconnects immediately,
    /// releases the held reservation in full without charging usage.
    pub fn release(&self, invocation_id: &str) -> Result<(), BillingError> {
        let _state_guard = self.state_lock.write().unwrap();
        if self
            .pending_settlements
            .read()
            .unwrap()
            .contains_key(invocation_id)
        {
            return Err(BillingError::InvalidState(
                "consumed reservation is pending settlement, not refundable".into(),
            ));
        }
        let sequence = self
            .snapshot_sequence
            .load(Ordering::Acquire)
            .saturating_add(1);
        let previous_checksum = self.last_snapshot_checksum.read().unwrap().clone();
        let mut candidate = self.export_snapshot_locked(sequence, previous_checksum);

        let (card_id, reserved_micro_credits) = {
            let reservation = match candidate.reservations.get_mut(invocation_id) {
                Some(r) => r,
                None => return Ok(()), // Already gone or no reservation
            };

            if reservation.state != ReservationState::Held {
                return Ok(());
            }

            reservation.state = ReservationState::Released;
            (
                reservation.card_id.clone(),
                reservation.reserved_micro_credits,
            )
        };

        if let Some(card) = candidate.cards.get_mut(&card_id) {
            card.credit_reserved = card.credit_reserved.saturating_sub(reserved_micro_credits);
        }

        let updated_card = candidate.cards.get(&card_id).cloned();
        let inv_id = invocation_id.to_string();

        self.commit_candidate_snapshot(&candidate, || {
            if let Some(card) = updated_card {
                self.cards.write().unwrap().insert(card_id, card);
            }
            if let Some(r) = self.reservations.write().unwrap().get_mut(&inv_id) {
                r.state = ReservationState::Released;
            }
        })
    }

    /// Janitor service: Reclaim expired orphan reservations (Spec §6.2).
    ///
    /// Unfreezes held credit reservations that exceeded TTL due to gateway crash or network drop.
    pub fn run_janitor(&self, now_secs: u64) -> usize {
        let _state_guard = self.state_lock.write().unwrap();
        let sequence = self
            .snapshot_sequence
            .load(Ordering::Acquire)
            .saturating_add(1);
        let previous_checksum = self.last_snapshot_checksum.read().unwrap().clone();
        let mut candidate = self.export_snapshot_locked(sequence, previous_checksum);

        let mut reclaimed_count = 0;
        let mut affected_cards = HashMap::new();
        let mut released_reservations = Vec::new();
        const TERMINAL_RESERVATION_RETENTION_SECS: u64 = 7 * 24 * 60 * 60;

        for res in candidate.reservations.values_mut() {
            if res.is_expired(now_secs)
                && !self
                    .active_reservations
                    .lock()
                    .unwrap()
                    .contains_key(&res.invocation_id)
                && !candidate
                    .pending_settlements
                    .contains_key(&res.invocation_id)
            {
                if let Some(card) = candidate.cards.get_mut(&res.card_id) {
                    card.credit_reserved = card
                        .credit_reserved
                        .saturating_sub(res.reserved_micro_credits);
                    affected_cards.insert(res.card_id.clone(), card.clone());
                }
                res.state = ReservationState::Released;
                released_reservations.push(res.invocation_id.clone());
                reclaimed_count += 1;
            }
        }

        let prune_before = now_secs.saturating_sub(TERMINAL_RESERVATION_RETENTION_SECS);
        let prune_ids: Vec<String> = candidate
            .reservations
            .values()
            .filter(|res| {
                // ponytail: retain settled IDs for replay safety, including after ledger archival.
                // Compact tombstones only when snapshot size warrants a schema migration.
                res.state == ReservationState::Released
                    && res.created_at_secs < prune_before
                    && !candidate
                        .pending_settlements
                        .contains_key(&res.invocation_id)
            })
            .map(|res| res.invocation_id.clone())
            .collect();
        for id in &prune_ids {
            candidate.reservations.remove(id);
        }
        let pruned_count = prune_ids.len();

        if reclaimed_count == 0 && pruned_count == 0 {
            return 0;
        }

        let commit_res = self.commit_candidate_snapshot(&candidate, || {
            let mut cards = self.cards.write().unwrap();
            for (id, card) in affected_cards {
                cards.insert(id, card);
            }
            let mut reservations = self.reservations.write().unwrap();
            for inv_id in released_reservations {
                if let Some(r) = reservations.get_mut(&inv_id) {
                    r.state = ReservationState::Released;
                }
            }
            for id in &prune_ids {
                reservations.remove(id);
            }
            reclaimed_count
        });

        match commit_res {
            Ok(count) => count,
            Err(e) => {
                eprintln!("[kiro-billing] janitor commit failed: {e}");
                0
            }
        }
    }

    /// Total ledger records.
    pub fn ledger_entries(&self) -> Vec<LedgerEntry> {
        let r = self.ledger.read().unwrap();
        r.clone()
    }

    // ==========================================
    // Device Management & Rebinding (Spec §5)
    // ==========================================

    /// Bind a device fingerprint to a card key.
    ///
    /// If device is already bound, returns Ok.
    /// Binds an empty slot; legacy multi-device records require explicit unbinding.
    /// A different device is rejected until explicit unbinding frees the slot.
    pub fn bind_device(
        &self,
        card_id: &str,
        device_fp: &str,
        _now_secs: u64,
    ) -> Result<(), BillingError> {
        let _state_guard = self.state_lock.write().unwrap();
        let sequence = self
            .snapshot_sequence
            .load(Ordering::Acquire)
            .saturating_add(1);
        let previous_checksum = self.last_snapshot_checksum.read().unwrap().clone();
        let mut candidate = self.export_snapshot_locked(sequence, previous_checksum);

        let device_fp = device_fp.trim();
        // A card has one device; a device may use independently purchased cards.
        let card = candidate
            .cards
            .get_mut(card_id)
            .ok_or_else(|| BillingError::CardNotFound(card_id.to_string()))?;
        let changed = bind_device_locked(card, device_fp)?;
        if !changed {
            return Ok(());
        }
        let updated_card = card.clone();
        let cid = card_id.to_string();

        self.commit_candidate_snapshot(&candidate, || {
            self.cards.write().unwrap().insert(cid, updated_card);
        })
    }

    /// Unbind a specific device fingerprint from a card.
    ///
    /// Consumes one rebind subject to the configured quota/cooldown,
    /// and revokes existing tokens. Filling the empty slot completes this change
    /// without consuming another rebind or waiting for the newly started cooldown.
    pub fn unbind_device(&self, card_id: &str, device_fp: &str) -> Result<(), BillingError> {
        let _state_guard = self.state_lock.write().unwrap();
        let sequence = self
            .snapshot_sequence
            .load(Ordering::Acquire)
            .saturating_add(1);
        let previous_checksum = self.last_snapshot_checksum.read().unwrap().clone();
        let mut candidate = self.export_snapshot_locked(sequence, previous_checksum);

        let card = candidate
            .cards
            .get_mut(card_id)
            .ok_or_else(|| BillingError::CardNotFound(card_id.to_string()))?;

        if let Some(pos) = card.bound_devices.iter().position(|d| d == device_fp) {
            let now_secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            check_rebind_allowed(card, now_secs)?;
            card.bound_devices.remove(pos);
            card.rebind_count = card.rebind_count.saturating_add(1);
            card.last_rebind_at = Some(now_secs);
            card.token_version = card.token_version.saturating_add(1);
        } else {
            return Err(BillingError::DeviceNotFound {
                card_id: card_id.to_string(),
                device: device_fp.to_string(),
            });
        }
        let updated_card = card.clone();
        let cid = card_id.to_string();

        self.commit_candidate_snapshot(&candidate, || {
            self.cards.write().unwrap().insert(cid, updated_card);
        })
    }

    /// List all currently bound devices for a card.
    pub fn list_devices(&self, card_id: &str) -> Result<Vec<String>, BillingError> {
        let cards = self.cards.read().unwrap();
        let card = cards
            .get(card_id)
            .ok_or_else(|| BillingError::CardNotFound(card_id.to_string()))?;
        Ok(card.bound_devices.clone())
    }

    // ==========================================
    // Card Operations & Lifecycle (Spec §5, §14.9)
    // ==========================================

    /// Freeze a card key (suspends usage, increments token_version to revoke active sessions).
    pub fn freeze_card(&self, card_id: &str, reason: &str) -> Result<Card, BillingError> {
        let _state_guard = self.state_lock.write().unwrap();
        let sequence = self
            .snapshot_sequence
            .load(Ordering::Acquire)
            .saturating_add(1);
        let previous_checksum = self.last_snapshot_checksum.read().unwrap().clone();
        let mut candidate = self.export_snapshot_locked(sequence, previous_checksum);

        let card = candidate
            .cards
            .get_mut(card_id)
            .ok_or_else(|| BillingError::CardNotFound(card_id.to_string()))?;

        if !matches!(
            card.status,
            CardStatus::Unactivated | CardStatus::Active | CardStatus::Expired
        ) {
            return Err(BillingError::InvalidState(format!(
                "cannot freeze {:?}",
                card.status
            )));
        }
        card.frozen_from = Some(card.status);
        card.status = CardStatus::Frozen;
        let prev_note = card.note.as_deref().unwrap_or("");
        card.note = Some(
            format!("[FROZEN: {}] {}", reason, prev_note)
                .trim()
                .to_string(),
        );
        card.token_version = card.token_version.saturating_add(1);
        let updated_card = card.clone();

        self.commit_candidate_snapshot(&candidate, || {
            self.cards
                .write()
                .unwrap()
                .insert(card_id.to_string(), updated_card.clone());
            updated_card
        })
    }

    /// Unfreeze a card key back to Active.
    pub fn unfreeze_card(&self, card_id: &str) -> Result<Card, BillingError> {
        let _state_guard = self.state_lock.write().unwrap();
        let sequence = self
            .snapshot_sequence
            .load(Ordering::Acquire)
            .saturating_add(1);
        let previous_checksum = self.last_snapshot_checksum.read().unwrap().clone();
        let mut candidate = self.export_snapshot_locked(sequence, previous_checksum);

        let card = candidate
            .cards
            .get_mut(card_id)
            .ok_or_else(|| BillingError::CardNotFound(card_id.to_string()))?;

        if card.status != CardStatus::Frozen {
            return Err(BillingError::InvalidState(format!(
                "cannot unfreeze {:?}",
                card.status
            )));
        }
        let restored = card.frozen_from.unwrap_or_else(|| {
            if card.activated_at.is_none() {
                CardStatus::Unactivated
            } else {
                CardStatus::Active
            }
        });
        if !matches!(
            restored,
            CardStatus::Unactivated | CardStatus::Active | CardStatus::Expired
        ) {
            return Err(BillingError::InvalidState("invalid frozen origin".into()));
        }
        card.status = restored;
        card.frozen_from = None;
        let updated_card = card.clone();

        self.commit_candidate_snapshot(&candidate, || {
            self.cards
                .write()
                .unwrap()
                .insert(card_id.to_string(), updated_card.clone());
            updated_card
        })
    }

    /// Ban a card key permanently (revokes active sessions).
    pub fn ban_card(&self, card_id: &str, reason: &str) -> Result<Card, BillingError> {
        let _state_guard = self.state_lock.write().unwrap();
        let sequence = self
            .snapshot_sequence
            .load(Ordering::Acquire)
            .saturating_add(1);
        let previous_checksum = self.last_snapshot_checksum.read().unwrap().clone();
        let mut candidate = self.export_snapshot_locked(sequence, previous_checksum);

        let card = candidate
            .cards
            .get_mut(card_id)
            .ok_or_else(|| BillingError::CardNotFound(card_id.to_string()))?;

        if card.status == CardStatus::Voided {
            return Err(BillingError::InvalidState(
                "cannot ban a voided card".into(),
            ));
        }
        card.status = CardStatus::Banned;
        let prev_note = card.note.as_deref().unwrap_or("");
        card.note = Some(
            format!("[BANNED: {}] {}", reason, prev_note)
                .trim()
                .to_string(),
        );
        card.token_version = card.token_version.saturating_add(1);
        let updated_card = card.clone();

        self.commit_candidate_snapshot(&candidate, || {
            self.cards
                .write()
                .unwrap()
                .insert(card_id.to_string(), updated_card.clone());
            updated_card
        })
    }

    /// Update notes/remarks on a card key.
    pub fn update_card_note(
        &self,
        card_id: &str,
        note: Option<String>,
    ) -> Result<(), BillingError> {
        let _state_guard = self.state_lock.write().unwrap();
        let sequence = self
            .snapshot_sequence
            .load(Ordering::Acquire)
            .saturating_add(1);
        let previous_checksum = self.last_snapshot_checksum.read().unwrap().clone();
        let mut candidate = self.export_snapshot_locked(sequence, previous_checksum);

        let card = candidate
            .cards
            .get_mut(card_id)
            .ok_or_else(|| BillingError::CardNotFound(card_id.to_string()))?;
        card.note = note;
        let updated_card = card.clone();
        let cid = card_id.to_string();

        self.commit_candidate_snapshot(&candidate, || {
            self.cards.write().unwrap().insert(cid, updated_card);
        })
    }

    /// Batch freeze cards.
    pub fn batch_freeze(
        &self,
        card_ids: &[String],
        reason: &str,
    ) -> Vec<Result<Card, BillingError>> {
        card_ids
            .iter()
            .map(|id| self.freeze_card(id, reason))
            .collect()
    }

    /// Batch unfreeze cards.
    pub fn batch_unfreeze(&self, card_ids: &[String]) -> Vec<Result<Card, BillingError>> {
        card_ids.iter().map(|id| self.unfreeze_card(id)).collect()
    }

    /// Batch ban cards.
    pub fn batch_ban(&self, card_ids: &[String], reason: &str) -> Vec<Result<Card, BillingError>> {
        card_ids
            .iter()
            .map(|id| self.ban_card(id, reason))
            .collect()
    }

    /// Manually revoke all active tokens for a card key by incrementing its `token_version`.
    pub fn revoke_tokens(&self, card_id: &str) -> Result<u64, BillingError> {
        let _state_guard = self.state_lock.write().unwrap();
        let sequence = self
            .snapshot_sequence
            .load(Ordering::Acquire)
            .saturating_add(1);
        let previous_checksum = self.last_snapshot_checksum.read().unwrap().clone();
        let mut candidate = self.export_snapshot_locked(sequence, previous_checksum);

        let card = candidate
            .cards
            .get_mut(card_id)
            .ok_or_else(|| BillingError::CardNotFound(card_id.to_string()))?;
        card.token_version = card.token_version.saturating_add(1);
        let ver = card.token_version;
        let updated_card = card.clone();

        self.commit_candidate_snapshot(&candidate, || {
            self.cards
                .write()
                .unwrap()
                .insert(card_id.to_string(), updated_card);
            ver
        })
    }

    /// Rotate only the refresh-token family for a card. Access JWTs remain
    /// valid until their normal expiry or an explicit card revocation.
    /// Rotate a card's refresh-token family, keeping O(1) state per card.
    ///
    /// Only a token carrying the card's current `refresh_version` rotates it, so replaying
    /// any older token fails without a record of each one used: the consumed-token set this
    /// replaces grew by an entry per refresh, for 30 days, and one card chaining refreshes
    /// could fill the state until billing stopped. The set is still read, so a token issued
    /// before this release and already used stays refused; nothing is added to it, and it
    /// drains as those tokens expire.
    ///
    /// A token exactly one version behind, presented within `grace_secs` of that rotation,
    /// is the same refresh retried — a response lost on the way back, a second Kiro window,
    /// or the desktop client refreshing alongside Kiro — and gets the current version again
    /// with no change, instead of a 401 that signs the customer out.
    pub fn rotate_refresh(
        &self,
        card_id: &str,
        presented_version: u64,
        jti: &str,
        expires: u64,
        now: u64,
        grace_secs: u64,
    ) -> Result<RefreshRotation, BillingError> {
        let _state_guard = self.state_lock.write().unwrap();
        if expires <= now || jti.is_empty() || jti.len() > 256 {
            return Err(BillingError::InvalidState(
                "expired or empty refresh token".into(),
            ));
        }
        let used = || BillingError::InvalidState("refresh token already used".into());
        if self
            .consumed_refresh_tokens
            .read()
            .unwrap()
            .get(jti)
            .is_some_and(|exp| *exp > now)
        {
            return Err(used());
        }
        let current = self
            .cards
            .read()
            .unwrap()
            .get(card_id)
            .map(|card| (card.refresh_version, card.refresh_rotated_at))
            .ok_or_else(|| BillingError::CardNotFound(card_id.to_string()))?;
        match current {
            (version, _) if presented_version == version => {}
            (version, Some(rotated))
                if presented_version.saturating_add(1) == version
                    && now.saturating_sub(rotated) < grace_secs =>
            {
                return Ok(RefreshRotation::Reissued(version));
            }
            _ => return Err(used()),
        }
        let mut candidate = self.export_snapshot_locked(
            self.snapshot_sequence
                .load(Ordering::Acquire)
                .saturating_add(1),
            self.last_snapshot_checksum.read().unwrap().clone(),
        );
        candidate
            .consumed_refresh_tokens
            .retain(|_, exp| *exp > now);
        let card = candidate
            .cards
            .get_mut(card_id)
            .ok_or_else(|| BillingError::CardNotFound(card_id.to_string()))?;
        card.refresh_version = card.refresh_version.saturating_add(1).max(1);
        card.refresh_rotated_at = Some(now);
        let (version, updated) = (card.refresh_version, card.clone());
        self.commit_candidate_snapshot(&candidate, || {
            self.cards
                .write()
                .unwrap()
                .insert(card_id.to_string(), updated);
            *self.consumed_refresh_tokens.write().unwrap() =
                candidate.consumed_refresh_tokens.clone();
            RefreshRotation::Rotated(version)
        })
    }

    pub fn refresh_version(&self, card_id: &str) -> Option<u64> {
        self.cards
            .read()
            .ok()?
            .get(card_id)
            .map(|card| card.refresh_version)
    }

    /// Void an unactivated card key and mark it recycled (Spec §14.9).
    ///
    /// "未激活卡密可作废并回收到库存；已激活只能冻结/封禁（不做按比例退款）"
    pub fn void_unactivated_card(
        &self,
        card_id: &str,
        operator_id: &str,
        reason: &str,
        now_secs: u64,
    ) -> Result<Card, BillingError> {
        self.void_card_inner(card_id, operator_id, reason, now_secs, true)
    }

    /// Permanently revoke a card while retaining its balance and audit history.
    pub fn void_card(
        &self,
        card_id: &str,
        operator_id: &str,
        reason: &str,
        now_secs: u64,
    ) -> Result<Card, BillingError> {
        self.void_card_inner(card_id, operator_id, reason, now_secs, false)
    }

    fn void_card_inner(
        &self,
        card_id: &str,
        operator_id: &str,
        reason: &str,
        now_secs: u64,
        unactivated_only: bool,
    ) -> Result<Card, BillingError> {
        let op = operator_id.trim();
        if op.is_empty() {
            return Err(BillingError::InvalidAdjustment(
                "Operator ID is required to void card".to_string(),
            ));
        }
        let res = reason.trim();
        if res.is_empty() {
            return Err(BillingError::InvalidAdjustment(
                "Reason is required to void card".to_string(),
            ));
        }

        let _state_guard = self.state_lock.write().unwrap();
        let sequence = self
            .snapshot_sequence
            .load(Ordering::Acquire)
            .saturating_add(1);
        let previous_checksum = self.last_snapshot_checksum.read().unwrap().clone();
        let mut candidate = self.export_snapshot_locked(sequence, previous_checksum);

        let card = candidate
            .cards
            .get_mut(card_id)
            .ok_or_else(|| BillingError::CardNotFound(card_id.to_string()))?;

        // The state lock serializes retries, including concurrent batch callers.
        if card.status == CardStatus::Voided {
            return Ok(card.clone());
        }
        if unactivated_only
            && (card.status != CardStatus::Unactivated
                || card.activated_at.is_some()
                || card.valid_until.is_some()
                || card.credit_used != 0
                || card.credit_reserved != 0
                || !card.bound_devices.is_empty())
        {
            return Err(BillingError::CannotVoidActivatedCard {
                card_id: card_id.to_string(),
                current_status: card.status,
            });
        }

        if card.credit_reserved != 0
            || candidate.reservations.values().any(|r| {
                r.card_id == card_id && r.state == crate::reservation::ReservationState::Held
            })
        {
            return Err(BillingError::InvalidState(
                "Card has in-flight requests; freeze it and retry after settlement".into(),
            ));
        }

        card.status = CardStatus::Voided;
        let prev_note = card.note.as_deref().unwrap_or("");
        card.note = Some(
            format!("[VOIDED by {} at {}: {}] {}", op, now_secs, res, prev_note)
                .trim()
                .to_string(),
        );
        card.token_version = card.token_version.saturating_add(1);

        let adjustment_id = format!("void-{}-{}-{}", card_id, now_secs, candidate.ledger.len());
        let entry = LedgerEntry {
            id: format!("ledger-{}", adjustment_id),
            card_id: card_id.to_string(),
            kind: LedgerKind::Adjustment,
            invocation_id: Some(adjustment_id),
            exposed_model: "void_card".to_string(),
            provider_id: "system".to_string(),
            target_model: "void_card".to_string(),
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            credits_charged: 0,
            provider_cost_micro_cny: 0,
            rate_card_version: None,
            ts_secs: now_secs,
            operator_id: Some(op.to_string()),
            reason: Some(res.to_string()),
        };
        candidate.ledger.push(entry.clone());
        let updated_card = card.clone();

        self.commit_candidate_snapshot(&candidate, || {
            self.cards
                .write()
                .unwrap()
                .insert(card_id.to_string(), updated_card.clone());
            self.ledger.write().unwrap().push(entry);
            updated_card
        })
    }

    /// Archive visibility metadata only, preserving card status and the financial ledger.
    pub fn set_card_archived(
        &self,
        card_id: &str,
        archived: bool,
        operator_id: &str,
        reason: &str,
        now_secs: u64,
    ) -> Result<Card, BillingError> {
        let op = operator_id.trim();
        if op.is_empty() {
            return Err(BillingError::InvalidAdjustment(
                "Operator ID is required to archive/unarchive card".to_string(),
            ));
        }
        let res = reason.trim();
        if res.is_empty() {
            return Err(BillingError::InvalidAdjustment(
                "Reason is required to archive/unarchive card".to_string(),
            ));
        }

        let _state_guard = self.state_lock.write().unwrap();
        let sequence = self
            .snapshot_sequence
            .load(Ordering::Acquire)
            .saturating_add(1);
        let previous_checksum = self.last_snapshot_checksum.read().unwrap().clone();
        let mut candidate = self.export_snapshot_locked(sequence, previous_checksum);

        let card = candidate
            .cards
            .get_mut(card_id)
            .ok_or_else(|| BillingError::CardNotFound(card_id.to_string()))?;

        if card.archived_at.is_some() == archived {
            return Ok(card.clone());
        }
        if archived
            && (card.credit_reserved != 0
                || !(matches!(
                    card.status,
                    CardStatus::Banned | CardStatus::Expired | CardStatus::Voided
                ) || card.valid_until.is_some_and(|until| now_secs >= until)))
        {
            return Err(BillingError::InvalidState(
                "only banned, expired, or voided cards without reservations can be archived".into(),
            ));
        }
        card.archived_at = archived.then_some(now_secs);
        let action = if archived {
            "archive_card"
        } else {
            "unarchive_card"
        };

        let adjustment_id = format!(
            "{}-{}-{}-{}",
            action,
            card_id,
            now_secs,
            candidate.ledger.len()
        );
        let entry = LedgerEntry {
            id: format!("ledger-{}", adjustment_id),
            card_id: card_id.to_string(),
            kind: LedgerKind::Adjustment,
            invocation_id: Some(adjustment_id),
            exposed_model: action.to_string(),
            provider_id: "system".to_string(),
            target_model: action.to_string(),
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            credits_charged: 0,
            provider_cost_micro_cny: 0,
            rate_card_version: None,
            ts_secs: now_secs,
            operator_id: Some(op.to_string()),
            reason: Some(res.to_string()),
        };
        candidate.ledger.push(entry.clone());
        let updated_card = card.clone();

        self.commit_candidate_snapshot(&candidate, || {
            self.cards
                .write()
                .unwrap()
                .insert(card_id.to_string(), updated_card.clone());
            self.ledger.write().unwrap().push(entry);
            updated_card
        })
    }

    /// Batch void unactivated cards (Spec §14.9).
    pub fn batch_void_unactivated_cards(
        &self,
        card_ids: &[String],
        operator_id: &str,
        reason: &str,
        now_secs: u64,
    ) -> Vec<Result<Card, BillingError>> {
        card_ids
            .iter()
            .map(|id| self.void_unactivated_card(id, operator_id, reason, now_secs))
            .collect()
    }

    /// List all cards matching a specific status.
    pub fn list_cards_by_status(&self, status: CardStatus) -> Vec<Card> {
        let cards = self.cards.read().unwrap();
        let mut list: Vec<Card> = cards
            .values()
            .filter(|c| c.status == status)
            .cloned()
            .collect();
        list.sort_by(|a, b| a.id.cmp(&b.id));
        list
    }

    /// Count cards grouped by template and status (inventory tracking).
    pub fn count_cards_by_template_and_status(
        &self,
        template_id: &str,
        status: CardStatus,
    ) -> usize {
        let cards = self.cards.read().unwrap();
        cards
            .values()
            .filter(|c| c.template_id.as_deref() == Some(template_id) && c.status == status)
            .count()
    }

    /// Count the number of active in-flight held reservations for a card (Spec §6.2, §14.9).
    pub fn get_active_concurrency(&self, card_id: &str) -> u32 {
        let reservations = self.reservations.read().unwrap();
        reservations
            .values()
            .filter(|r| r.card_id == card_id && r.state == ReservationState::Held)
            .count() as u32
    }

    /// Calculate the sum of usage micro-credits for a card since a given timestamp (Spec §5, §6.6).
    pub fn get_usage_since(&self, card_id: &str, since_secs: u64) -> i64 {
        let _state_guard = self.state_lock.read().unwrap();
        let archived = self
            .archived_ledger_summary
            .read()
            .unwrap()
            .usage_since(card_id, since_secs);
        self.ledger
            .read()
            .unwrap()
            .iter()
            .filter(|e| {
                e.card_id == card_id && e.kind == LedgerKind::Usage && e.ts_secs >= since_secs
            })
            .fold(archived, |sum, e| sum.saturating_add(e.credits_charged))
    }

    /// Calculate daily usage micro-credits for a card for the current calendar day (Spec §5, §6.6).
    pub fn get_daily_usage(&self, card_id: &str, now_secs: u64) -> i64 {
        let day_start = (now_secs / 86_400) * 86_400;
        self.get_usage_since(card_id, day_start)
    }

    /// Calculate monthly usage micro-credits for a card for the trailing 30-day window (Spec §5, §6.6).
    pub fn get_monthly_usage(&self, card_id: &str, now_secs: u64) -> i64 {
        let month_start = now_secs.saturating_sub(30 * 86_400);
        self.get_usage_since(card_id, month_start)
    }

    /// Update card quota limits dynamically (Spec §5, §6.6, §14.9).
    pub fn update_card_quotas(
        &self,
        card_id: &str,
        max_concurrency: Option<u32>,
        daily_limit: Option<Option<i64>>,
        monthly_limit: Option<Option<i64>>,
    ) -> Result<Card, BillingError> {
        let _state_guard = self.state_lock.write().unwrap();
        let sequence = self
            .snapshot_sequence
            .load(Ordering::Acquire)
            .saturating_add(1);
        let previous_checksum = self.last_snapshot_checksum.read().unwrap().clone();
        let mut candidate = self.export_snapshot_locked(sequence, previous_checksum);

        let card = candidate
            .cards
            .get_mut(card_id)
            .ok_or_else(|| BillingError::CardNotFound(card_id.to_string()))?;

        if let Some(concurrency) = max_concurrency {
            card.max_concurrency = concurrency;
        }
        if let Some(daily) = daily_limit {
            card.daily_credit_limit = daily;
        }
        if let Some(monthly) = monthly_limit {
            card.monthly_credit_limit = monthly;
        }
        let updated_card = card.clone();
        let cid = card_id.to_string();

        self.commit_candidate_snapshot(&candidate, || {
            self.cards
                .write()
                .unwrap()
                .insert(cid, updated_card.clone());
            updated_card
        })
    }

    // ==========================================
    // Top-up & Renewal Operations (Spec §14.9)
    // ==========================================

    /// Register a top-up code, durably: it exists only once the snapshot holding it is
    /// saved. Under the state lock, so a concurrent commit cannot publish a snapshot taken
    /// before the insert and drop the code again.
    pub fn upsert_topup_code(&self, code: TopupCode) -> Result<(), BillingError> {
        let _state_guard = self.state_lock.write().unwrap();
        let sequence = self
            .snapshot_sequence
            .load(Ordering::Acquire)
            .saturating_add(1);
        let previous_checksum = self.last_snapshot_checksum.read().unwrap().clone();
        let mut candidate = self.export_snapshot_locked(sequence, previous_checksum);
        candidate.topup_codes.insert(code.id.clone(), code.clone());
        self.commit_candidate_snapshot(&candidate, || {
            self.topup_codes
                .write()
                .unwrap()
                .insert(code.id.clone(), code);
        })
    }

    /// Look up top-up code by ID.
    pub fn get_topup_code(&self, id: &str) -> Option<TopupCode> {
        let topups = self.topup_codes.read().unwrap();
        topups.get(id).cloned()
    }

    /// Redeem a top-up code against a card.
    ///
    /// Atomically:
    /// 1. Verifies code has not been redeemed.
    /// 2. Marks code redeemed by this card.
    /// 3. Increases card credit balance.
    /// 4. Extends card validity duration (reactivates expired cards).
    /// 5. Writes append-only `usage_ledger` entry with `kind = topup`.
    pub fn redeem_topup(
        &self,
        card_id: &str,
        raw_code: &str,
        now_secs: u64,
        operator: &str,
    ) -> Result<LedgerEntry, BillingError> {
        let code_hash = crate::card::hash_card_code(raw_code);
        let _state_guard = self.state_lock.write().unwrap();
        let sequence = self
            .snapshot_sequence
            .load(Ordering::Acquire)
            .saturating_add(1);
        let previous_checksum = self.last_snapshot_checksum.read().unwrap().clone();
        let mut candidate = self.export_snapshot_locked(sequence, previous_checksum);

        // 1. Locate unredeemed topup code matching code hash
        let topup = candidate
            .topup_codes
            .values_mut()
            .find(|t| t.code_hash == code_hash && !t.is_used)
            .ok_or(BillingError::InvalidOrRedeemedTopupCode)?;

        // 2. Fetch card
        let card = candidate
            .cards
            .get_mut(card_id)
            .ok_or_else(|| BillingError::CardNotFound(card_id.to_string()))?;

        if matches!(
            card.status,
            CardStatus::Frozen | CardStatus::Banned | CardStatus::Voided
        ) {
            return Err(BillingError::Card(CardError::NotActive(card.status)));
        }

        let duration_ext = topup.duration_extension_secs;
        if duration_ext > 0 {
            if card.status == CardStatus::Unactivated {
                return Err(BillingError::InvalidState(
                    "activate the card before redeeming a duration top-up; the top-up code has not been used"
                        .to_string(),
                ));
            }
            if card.valid_until.is_none() {
                return Err(BillingError::InvalidState(
                    "duration top-ups are not supported for perpetual cards; the top-up code has not been used"
                        .to_string(),
                ));
            }
        }

        // 3. Mark redeemed
        topup.is_used = true;
        topup.used_by_card_id = Some(card_id.to_string());
        topup.used_at = Some(now_secs);
        let topup_id = topup.id.clone();
        let credit_amount = topup.credit_amount;
        let updated_topup = topup.clone();

        // 4. Update card credits & validity
        card.credit_total = card.credit_total.saturating_add(credit_amount);
        if duration_ext > 0 {
            let base = card.valid_until.unwrap_or(now_secs).max(now_secs);
            card.valid_until = Some(base.saturating_add(duration_ext));
        }
        if card.status == CardStatus::Expired {
            card.status = CardStatus::Active;
        }
        let updated_card = card.clone();

        // 5. Append-only ledger record (Spec §14.9)
        let entry = LedgerEntry {
            id: format!("ledger-{}", topup_id),
            card_id: card_id.to_string(),
            kind: LedgerKind::Topup,
            invocation_id: Some(format!("topup-{}", topup_id)),
            exposed_model: "topup".to_string(),
            provider_id: "system".to_string(),
            target_model: "topup".to_string(),
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            credits_charged: credit_amount,
            provider_cost_micro_cny: 0,
            rate_card_version: None,
            ts_secs: now_secs,
            operator_id: Some(operator.to_string()),
            reason: Some(format!("Redeemed top-up code {}", topup_id)),
        };
        candidate.ledger.push(entry.clone());

        self.commit_candidate_snapshot(&candidate, || {
            self.cards
                .write()
                .unwrap()
                .insert(card_id.to_string(), updated_card);
            self.topup_codes
                .write()
                .unwrap()
                .insert(updated_topup.id.clone(), updated_topup);
            self.ledger.write().unwrap().push(entry.clone());
            entry
        })
    }

    // ==========================================
    // Tenant Groups & Model Virtualization (Spec §1.4, §5)
    // ==========================================

    /// Add or update a tenant group.
    pub fn upsert_group(&self, group: Group) {
        {
            let _state_guard = self.state_lock.write().unwrap();
            let mut w = self.groups.write().unwrap();
            w.insert(group.id.clone(), group);
        }
        self.sync_to_disk();
    }

    /// Retrieve a tenant group by ID.
    pub fn get_group(&self, id: &str) -> Option<Group> {
        let r = self.groups.read().unwrap();
        r.get(id).cloned()
    }

    /// List all tenant groups.
    pub fn list_groups(&self) -> Vec<Group> {
        let r = self.groups.read().unwrap();
        r.values().cloned().collect()
    }

    /// Add or update a model mapping entry for a group.
    pub fn upsert_model_map(&self, mapping: ModelMap) {
        {
            let _state_guard = self.state_lock.write().unwrap();
            let mut w = self.model_maps.write().unwrap();
            if let Some(existing) = w.iter_mut().find(|m| m.id == mapping.id) {
                *existing = mapping;
            } else {
                w.push(mapping);
            }
        }
        self.sync_to_disk();
    }

    /// List models mapped to a group, optionally filtering by visibility and sorted by sort_order.
    pub fn list_models_for_group(&self, group_id: &str, only_visible: bool) -> Vec<ModelMap> {
        let r = self.model_maps.read().unwrap();
        let mut list: Vec<ModelMap> = r
            .iter()
            .filter(|m| m.group_id == group_id && (!only_visible || m.visible))
            .cloned()
            .collect();
        list.sort_by_key(|m| m.sort_order);
        list
    }

    // ==========================================
    // Rate Cards, Versioning & Pricing (Spec §5, §6.4, §14.10)
    // ==========================================

    /// Add or update a rate card header.
    pub fn upsert_rate_card(&self, card: RateCard) {
        {
            let _state_guard = self.state_lock.write().unwrap();
            let mut w = self.rate_cards.write().unwrap();
            w.insert(card.id.clone(), card);
        }
        self.sync_to_disk();
    }

    /// Get rate card by ID.
    pub fn get_rate_card(&self, rate_card_id: &str) -> Option<RateCard> {
        let r = self.rate_cards.read().unwrap();
        r.get(rate_card_id).cloned()
    }

    /// List all rate cards.
    pub fn list_rate_cards(&self) -> Vec<RateCard> {
        let r = self.rate_cards.read().unwrap();
        let mut list: Vec<RateCard> = r.values().cloned().collect();
        list.sort_by(|a, b| a.id.cmp(&b.id));
        list
    }

    /// Add or update a rate card version.
    pub fn upsert_rate_card_version(&self, version: RateCardVersion) {
        {
            let _state_guard = self.state_lock.write().unwrap();
            let mut w = self.rate_card_versions.write().unwrap();
            if let Some(pos) = w.iter().position(|v| v.id == version.id) {
                w[pos] = version;
            } else {
                w.push(version);
            }
        }
        self.sync_to_disk();
    }

    /// Get rate card version by ID.
    pub fn get_rate_card_version(&self, version_id: &str) -> Option<RateCardVersion> {
        let r = self.rate_card_versions.read().unwrap();
        r.iter().find(|v| v.id == version_id).cloned()
    }

    /// List versions for a rate card, ordered by effective timestamp ascending.
    pub fn list_rate_card_versions(&self, rate_card_id: &str) -> Vec<RateCardVersion> {
        let r = self.rate_card_versions.read().unwrap();
        let mut list: Vec<RateCardVersion> = r
            .iter()
            .filter(|v| v.rate_card_id == rate_card_id)
            .cloned()
            .collect();
        list.sort_by_key(|v| v.effective_from_secs);
        list
    }

    /// Resolve the active version of a rate card for a given model at timestamp `at_secs` (Spec §6.4).
    ///
    /// Matches exact model first; falls back to wildcard `*`.
    /// Among matching versions with `effective_from_secs <= at_secs`, chooses the highest timestamp.
    pub fn resolve_rate_card_version(
        &self,
        rate_card_id: &str,
        model: &str,
        at_secs: u64,
    ) -> Option<RateCardVersion> {
        let versions = self.rate_card_versions.read().unwrap();

        let exact = versions
            .iter()
            .filter(|v| {
                v.rate_card_id == rate_card_id
                    && v.model == model
                    && v.effective_from_secs <= at_secs
            })
            .max_by_key(|v| v.effective_from_secs)
            .cloned();

        if exact.is_some() {
            return exact;
        }

        versions
            .iter()
            .filter(|v| {
                v.rate_card_id == rate_card_id && v.model == "*" && v.effective_from_secs <= at_secs
            })
            .max_by_key(|v| v.effective_from_secs)
            .cloned()
    }

    /// Retrieve global billing settings.
    pub fn get_settings(&self) -> BillingSettings {
        self.settings.read().unwrap().clone()
    }

    /// Update global billing settings.
    pub fn update_settings(&self, settings: BillingSettings) {
        {
            let _state_guard = self.state_lock.write().unwrap();
            let mut w = self.settings.write().unwrap();
            *w = settings;
        }
        self.sync_to_disk();
    }

    /// Adjust card balance manually and record an audit ledger entry (Spec §5, §14.10.5).
    /// "人工调账也以条目形式写入，不直接改余额"
    pub fn adjust_balance(
        &self,
        card_id: &str,
        delta_micro_credits: i64,
        operator_id: &str,
        reason: &str,
        now_secs: u64,
    ) -> Result<LedgerEntry, BillingError> {
        self.adjust_balance_idempotent(
            card_id,
            delta_micro_credits,
            operator_id,
            reason,
            now_secs,
            None,
        )
    }

    /// Idempotent balance adjustment with candidate state calculation & durable atomic commit (Spec §5, §14.10.5, T02).
    pub fn adjust_balance_idempotent(
        &self,
        card_id: &str,
        delta_micro_credits: i64,
        operator_id: &str,
        reason: &str,
        now_secs: u64,
        idempotency_key: Option<&str>,
    ) -> Result<LedgerEntry, BillingError> {
        let op = operator_id.trim();
        if op.is_empty() {
            return Err(BillingError::InvalidAdjustment(
                "Operator ID is required for audit trail".to_string(),
            ));
        }

        let res = reason.trim();
        if res.is_empty() {
            return Err(BillingError::InvalidAdjustment(
                "Reason is required for audit trail".to_string(),
            ));
        }

        if delta_micro_credits == 0 {
            return Err(BillingError::InvalidAdjustment(
                "Adjustment delta cannot be zero".to_string(),
            ));
        }

        let _state_guard = self.state_lock.write().unwrap();

        // Persistent idempotency check: check if already committed to the ledger
        if let Some(key) = idempotency_key {
            let existing = {
                let ledger = self.ledger.read().unwrap();
                ledger
                    .iter()
                    .find(|e| {
                        e.kind == LedgerKind::Adjustment && e.invocation_id.as_deref() == Some(key)
                    })
                    .cloned()
            }
            .or_else(|| {
                self.archived_ledger_summary
                    .read()
                    .unwrap()
                    .adjustments
                    .get(key)
                    .cloned()
            });
            if let Some(existing) = existing {
                if existing.card_id == card_id
                    && existing.credits_charged == delta_micro_credits
                    && existing.operator_id.as_deref() == Some(op)
                    && existing.reason.as_deref() == Some(res)
                {
                    return Ok(existing);
                } else {
                    return Err(BillingError::InvalidAdjustment(format!(
                        "Idempotency conflict: key '{key}' already used with different parameters"
                    )));
                }
            }
        }

        let sequence = self
            .snapshot_sequence
            .load(Ordering::Acquire)
            .saturating_add(1);
        let previous_checksum = self.last_snapshot_checksum.read().unwrap().clone();
        let mut candidate = self.export_snapshot_locked(sequence, previous_checksum);

        let card = candidate
            .cards
            .get_mut(card_id)
            .ok_or_else(|| BillingError::CardNotFound(card_id.to_string()))?;

        if card.status == CardStatus::Voided {
            return Err(BillingError::InvalidState(
                "cannot adjust a voided card".into(),
            ));
        }
        if delta_micro_credits > 0 {
            card.credit_total = card.credit_total.saturating_add(delta_micro_credits);
        } else {
            let deduct = delta_micro_credits.saturating_neg();
            let available = card.available_credits();
            if deduct > available {
                return Err(BillingError::Card(CardError::InsufficientCredit {
                    available,
                    needed: deduct,
                }));
            }
            card.credit_used = card.credit_used.saturating_add(deduct);
        }

        let adjustment_id = idempotency_key
            .map(ToString::to_string)
            .unwrap_or_else(|| format!("adj-{}-{}-{}", card_id, now_secs, candidate.ledger.len()));

        let entry = LedgerEntry {
            id: format!("ledger-{}", adjustment_id),
            card_id: card_id.to_string(),
            kind: LedgerKind::Adjustment,
            invocation_id: Some(adjustment_id),
            exposed_model: "adjustment".to_string(),
            provider_id: "system".to_string(),
            target_model: "adjustment".to_string(),
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            credits_charged: delta_micro_credits,
            provider_cost_micro_cny: 0,
            rate_card_version: None,
            ts_secs: now_secs,
            operator_id: Some(op.to_string()),
            reason: Some(res.to_string()),
        };

        candidate.ledger.push(entry.clone());
        let updated_card = card.clone();

        self.commit_candidate_snapshot(&candidate, || {
            self.cards
                .write()
                .unwrap()
                .insert(card_id.to_string(), updated_card);
            self.ledger.write().unwrap().push(entry.clone());
            entry
        })
    }

    /// Audit & reconcile card balance against the immutable ledger (Spec §5, §14.9, §14.10.5).
    /// "保证账实相符、可追溯"
    pub fn reconcile_card_balance(
        &self,
        card_id: &str,
        initial_template_credits: i64,
    ) -> Result<CardReconciliation, BillingError> {
        let _state_guard = self.state_lock.read().unwrap();
        let cards = self.cards.read().unwrap();
        let card = cards
            .get(card_id)
            .ok_or_else(|| BillingError::CardNotFound(card_id.to_string()))?;

        let ledger = self.ledger.read().unwrap();
        let archived = self.archived_ledger_summary.read().unwrap();
        let archived_card = archived.cards.get(card_id).cloned().unwrap_or_default();
        let mut total_topup_credits = archived_card.topups;
        let mut total_positive_adjustments = archived_card.positive_adjustments;
        let mut total_negative_adjustments = archived_card.negative_adjustments;
        let mut total_usage_charges = archived_card.usage;

        for entry in ledger.iter().filter(|e| e.card_id == card_id) {
            match entry.kind {
                LedgerKind::Topup => {
                    total_topup_credits = total_topup_credits.saturating_add(entry.credits_charged);
                }
                LedgerKind::Adjustment => {
                    if entry.credits_charged > 0 {
                        total_positive_adjustments =
                            total_positive_adjustments.saturating_add(entry.credits_charged);
                    } else if entry.credits_charged < 0 {
                        total_negative_adjustments = total_negative_adjustments
                            .saturating_add(entry.credits_charged.saturating_neg());
                    }
                }
                LedgerKind::Usage => {
                    total_usage_charges = total_usage_charges.saturating_add(entry.credits_charged);
                }
            }
        }

        let expected_credit_total = initial_template_credits
            .saturating_add(total_topup_credits)
            .saturating_add(total_positive_adjustments);
        let expected_credit_used = total_usage_charges.saturating_add(total_negative_adjustments);

        let is_balanced =
            expected_credit_total == card.credit_total && expected_credit_used == card.credit_used;

        Ok(CardReconciliation {
            card_id: card_id.to_string(),
            initial_credits: initial_template_credits,
            total_topup_credits,
            total_positive_adjustments,
            total_negative_adjustments,
            total_usage_charges,
            expected_credit_total,
            actual_credit_total: card.credit_total,
            expected_credit_used,
            actual_credit_used: card.credit_used,
            available_credits: card.available_credits(),
            is_balanced,
        })
    }

    /// UTC today plus the preceding 29 days, through now. Never substitute traces
    /// for missing ledger details: archived summaries lack tokens/model dimensions.
    pub fn settled_usage(
        &self,
        card_id: &str,
        now_secs: u64,
    ) -> Option<crate::settled_usage::SettledUsage> {
        let _state_guard = self.state_lock.read().unwrap();
        if !self.cards.read().unwrap().contains_key(card_id) {
            return None;
        }
        let (start, end) = crate::settled_usage::window(now_secs);
        if self
            .archived_ledger_summary
            .read()
            .unwrap()
            .cards
            .get(card_id)
            .is_some_and(|c| c.usage_by_second.range(start..end).next().is_some())
        {
            return None;
        }
        // ponytail: scan the retained ledger; add a per-card index if measured traffic warrants it.
        let ledger = self.ledger.read().unwrap();
        Some(crate::settled_usage::aggregate(
            ledger.iter(),
            card_id,
            now_secs,
        ))
    }

    /// List append-only ledger entries for a card, optionally filtered by ledger kind.
    pub fn list_ledger_entries_for_card(
        &self,
        card_id: &str,
        kind: Option<LedgerKind>,
    ) -> Vec<LedgerEntry> {
        let ledger = self.ledger.read().unwrap();
        ledger
            .iter()
            .filter(|e| e.card_id == card_id && kind.is_none_or(|k| e.kind == k))
            .cloned()
            .collect()
    }

    /// Calculate aggregate gross margin summary directly from the append-only ledger (Spec §6.8, §14.4).
    pub fn get_margin_summary(&self) -> MarginSummary {
        let _state_guard = self.state_lock.read().unwrap();
        let ledger = self.ledger.read().unwrap();
        let settings = self.get_settings();
        let face_val = if settings.credit_face_value_cny > 0.0 {
            settings.credit_face_value_cny
        } else {
            0.01
        };

        let archived = self.archived_ledger_summary.read().unwrap();
        let mut total_credits_charged = archived
            .cards
            .values()
            .fold(0i64, |sum, c| sum.saturating_add(c.usage));
        let mut total_provider_cost_micro_cny = archived
            .cards
            .values()
            .fold(0i64, |sum, c| sum.saturating_add(c.provider_cost_micro_cny));

        for entry in ledger.iter() {
            if entry.kind == LedgerKind::Usage {
                total_credits_charged = total_credits_charged.saturating_add(entry.credits_charged);
                total_provider_cost_micro_cny =
                    total_provider_cost_micro_cny.saturating_add(entry.provider_cost_micro_cny);
            }
        }

        // Revenue in micro-CNY = (credits / 1_000_000) * face_value * 1_000_000
        //                      = credits * face_value
        let total_revenue_micro_cny = ((total_credits_charged as f64) * face_val).round() as i64;
        let gross_profit_micro_cny =
            total_revenue_micro_cny.saturating_sub(total_provider_cost_micro_cny);
        let gross_margin_rate = if total_revenue_micro_cny > 0 {
            (gross_profit_micro_cny as f64) / (total_revenue_micro_cny as f64)
        } else {
            0.0
        };

        MarginSummary {
            total_credits_charged,
            total_revenue_micro_cny,
            total_provider_cost_micro_cny,
            gross_profit_micro_cny,
            gross_margin_rate,
        }
    }

    // ==========================================
    // Pricing Workbench & Audit Logs (Spec §14.10.3)
    // ==========================================

    /// Publish a new RateCardVersion and write an immutable audit log entry (Spec §14.10.3).
    /// Historical invocations and ledger entries remain pegged to their effective versions.
    pub fn publish_rate_card_version(
        &self,
        version: RateCardVersion,
        operator_id: &str,
        reason: &str,
        now_secs: u64,
    ) -> RateCardAuditLog {
        // 1. Check previous version if any
        let prev_version =
            self.resolve_rate_card_version(&version.rate_card_id, &version.model, now_secs);
        let prev_id = prev_version.map(|v| v.id);

        let audit_log = RateCardAuditLog {
            id: format!("rc-audit-{}", now_secs),
            rate_card_id: version.rate_card_id.clone(),
            version_id: version.id.clone(),
            operator_id: operator_id.to_string(),
            reason: reason.to_string(),
            created_at_secs: now_secs,
            previous_version_id: prev_id,
        };

        // 2. Insert new version
        self.upsert_rate_card_version(version);

        // 3. Record audit log
        {
            let mut audit_logs = self.rate_card_audit_logs.write().unwrap();
            audit_logs.push(audit_log.clone());
        }
        self.sync_to_disk();

        audit_log
    }

    /// List rate card audit logs, optionally filtered by rate card ID.
    pub fn list_rate_card_audit_logs(&self, rate_card_id: Option<&str>) -> Vec<RateCardAuditLog> {
        let r = self.rate_card_audit_logs.read().unwrap();
        r.iter()
            .filter(|log| rate_card_id.is_none_or(|id| log.rate_card_id == id))
            .cloned()
            .collect()
    }

    /// Run pricing simulation against historical ledger records (Spec §14.10.3).
    ///
    /// Evaluates impact on total revenue, credits burned, gross profit, and user card lifespan
    /// before pushing prices to production.
    pub fn simulate_candidate_pricing(
        &self,
        candidate_version: &RateCardVersion,
        window_secs: Option<u64>,
        now_secs: u64,
        default_monthly_card_days: f64,
    ) -> SimulationResult {
        let ledger = self.ledger.read().unwrap();
        let settings = self.get_settings();

        let filtered_entries: Vec<LedgerEntry> = if let Some(win) = window_secs {
            let threshold = now_secs.saturating_sub(win);
            ledger
                .iter()
                .filter(|e| e.ts_secs >= threshold)
                .cloned()
                .collect()
        } else {
            ledger.clone()
        };

        crate::workbench::simulate_candidate_pricing(
            &filtered_entries,
            candidate_version,
            &settings,
            default_monthly_card_days,
        )
    }

    // =========================================================================
    // Observability, Gross Margin, and Anomaly Monitoring (Spec §14.4)
    // =========================================================================

    /// Record a structured request execution trace (Spec §5, §14.4).
    /// Traces are observability data. They ride along with the next commit, which saves
    /// the whole state and follows in the same request (its reservation's release or
    /// settlement), instead of each costing a full save of its own. A crash before that
    /// commit loses only the trace, and with it the attempt count it holds.
    pub fn record_trace(&self, trace: RequestTrace) {
        let _state_guard = self.state_lock.write().unwrap();
        let mut traces = self.traces.write().unwrap();
        traces.push(trace);
        if traces.len() > MAX_RETAINED_TRACES {
            let overflow = traces.len() - MAX_RETAINED_TRACES;
            traces.drain(..overflow);
        }
    }

    pub fn invocation_attempts(&self, invocation_id: &str) -> usize {
        self.traces
            .read()
            .unwrap()
            .iter()
            .filter(|t| t.invocation_id == invocation_id)
            .map(|t| t.attempt_chain.len())
            .sum()
    }

    /// Final delivery outcome is distinct from whether partial output was billed. Like
    /// [`Self::record_trace`], it is saved with the next commit rather than on its own.
    pub fn finish_trace(&self, invocation_id: &str, status: TraceStatus, error: Option<&str>) {
        let _guard = self.state_lock.write().unwrap();
        let mut traces = self.traces.write().unwrap();
        if let Some(trace) = traces
            .iter_mut()
            .rev()
            .find(|t| t.invocation_id == invocation_id)
        {
            trace.status = status;
            trace.error_class = error.map(str::to_owned);
        }
    }

    /// List recorded request execution traces.
    pub fn list_traces(&self, card_id: Option<&str>, limit: usize) -> Vec<RequestTrace> {
        let traces = self.traces.read().unwrap();
        traces
            .iter()
            .rev()
            .filter(|t| card_id.is_none_or(|id| t.card_id == id))
            .take(limit)
            .cloned()
            .collect()
    }

    /// Compute gross margin dashboard over given timestamp interval (Spec §14.4).
    pub fn get_margin_dashboard(
        &self,
        since_secs: Option<u64>,
        until_secs: Option<u64>,
    ) -> MarginDashboard {
        let ledger = self.ledger.read().unwrap();
        let settings = self.get_settings();
        let filtered: Vec<LedgerEntry> = ledger
            .iter()
            .filter(|e| since_secs.is_none_or(|s| e.ts_secs >= s))
            .filter(|e| until_secs.is_none_or(|u| e.ts_secs <= u))
            .cloned()
            .collect();

        compute_margin_dashboard(&filtered, &settings)
    }

    /// Get model consumption and cost rankings over timestamp interval (Spec §14.4).
    pub fn get_model_cost_rankings(
        &self,
        since_secs: Option<u64>,
        until_secs: Option<u64>,
    ) -> Vec<ModelCostRanking> {
        let ledger = self.ledger.read().unwrap();
        let settings = self.get_settings();
        let filtered: Vec<LedgerEntry> = ledger
            .iter()
            .filter(|e| since_secs.is_none_or(|s| e.ts_secs >= s))
            .filter(|e| until_secs.is_none_or(|u| e.ts_secs <= u))
            .cloned()
            .collect();

        compute_model_cost_rankings(&filtered, &settings)
    }

    /// Compute provider health metrics from request traces (Spec §14.4).
    pub fn get_provider_health(&self, provider_id: &str) -> ProviderHealthSummary {
        let traces = self.traces.read().unwrap();
        compute_provider_health(&traces, provider_id)
    }

    /// Get daily aggregated usage summary over timestamp interval (Spec §14.4).
    pub fn get_daily_summary(
        &self,
        since_secs: Option<u64>,
        until_secs: Option<u64>,
    ) -> Vec<DailyUsageSummary> {
        let ledger = self.ledger.read().unwrap();
        let mut map: HashMap<String, DailyUsageSummary> = HashMap::new();

        for e in ledger.iter() {
            if e.kind != crate::ledger::LedgerKind::Usage {
                continue;
            }
            if let Some(s) = since_secs {
                if e.ts_secs < s {
                    continue;
                }
            }
            if let Some(u) = until_secs {
                if e.ts_secs > u {
                    continue;
                }
            }

            let days = e.ts_secs / 86400;
            let date_str = format!("Day-{}", days);
            let entry = map
                .entry(date_str.clone())
                .or_insert_with(|| DailyUsageSummary {
                    date_str,
                    ..Default::default()
                });

            entry.requests_count += 1;
            entry.input_tokens += e.input_tokens;
            entry.output_tokens += e.output_tokens;
            entry.cache_creation_tokens += e.cache_creation_tokens;
            entry.cache_read_tokens += e.cache_read_tokens;
            entry.credits_charged += e.credits_charged;
            entry.provider_cost_micro_cny += e.provider_cost_micro_cny;
        }

        let mut res: Vec<DailyUsageSummary> = map.into_values().collect();
        res.sort_by(|a, b| a.date_str.cmp(&b.date_str));
        res
    }

    /// Evaluate card usage rate spike and apply automated protective action (Spec §14.4).
    pub fn evaluate_card_spike(
        &self,
        card_id: &str,
        now_secs: u64,
        window_secs: u64,
        threshold_credits: i64,
        action: AnomalyAction,
    ) -> Option<AnomalyAlert> {
        let cutoff = now_secs.saturating_sub(window_secs);
        let usage = self.get_usage_since(card_id, cutoff);
        if usage > threshold_credits {
            if action == AnomalyAction::AutoFrozen {
                let _ = self.freeze_card(card_id, "Automated anomaly freeze: rate spike detected");
            }
            Some(AnomalyAlert {
                card_id: card_id.to_string(),
                window_secs,
                credits_used_in_window: usage,
                threshold_credits,
                action_taken: action,
                detected_at: now_secs,
            })
        } else {
            None
        }
    }

    /// Add platform degradation announcement (Spec §14.4).
    pub fn add_announcement(&self, announcement: Announcement) {
        {
            let _state_guard = self.state_lock.write().unwrap();
            let mut anns = self.announcements.write().unwrap();
            anns.retain(|a| a.id != announcement.id);
            anns.push(announcement);
        }
        self.sync_to_disk();
    }

    /// List active platform announcements (Spec §14.4).
    pub fn list_active_announcements(&self, now_secs: u64) -> Vec<Announcement> {
        let anns = self.announcements.read().unwrap();
        anns.iter()
            .filter(|a| a.is_active(now_secs))
            .cloned()
            .collect()
    }

    /// Export ledger entries to CSV (Spec §14.4).
    pub fn export_ledger_csv(&self, card_id: Option<&str>) -> String {
        let ledger = self.ledger.read().unwrap();
        let entries: Vec<LedgerEntry> = ledger
            .iter()
            .filter(|e| card_id.is_none_or(|id| e.card_id == id))
            .cloned()
            .collect();
        export_reconciliation_csv(&entries)
    }

    /// Export ledger entries to JSON (Spec §14.4).
    pub fn export_ledger_json(&self, card_id: Option<&str>) -> Result<String, serde_json::Error> {
        let ledger = self.ledger.read().unwrap();
        let entries: Vec<LedgerEntry> = ledger
            .iter()
            .filter(|e| card_id.is_none_or(|id| e.card_id == id))
            .cloned()
            .collect();
        export_reconciliation_json(&entries)
    }

    /// Prune expired request traces older than cutoff seconds (Spec §14.6 Data Retention Policy).
    pub fn prune_traces(&self, cutoff_secs: u64) -> usize {
        let pruned = {
            let _state_guard = self.state_lock.write().unwrap();
            let mut traces = self.traces.write().unwrap();
            prune_traces_in_place(&mut traces, cutoff_secs)
        };
        if pruned > 0 {
            self.sync_to_disk();
        }
        pruned
    }

    /// Archives settled ledger entries older than `before_ts_secs` into an immutable verifiable archive file (T03).
    /// Drained entries are removed from active in-memory ledger and committed atomically to disk snapshot.
    /// Card balances and historical summaries remain completely intact.
    pub fn archive_ledger(
        &self,
        before_ts_secs: u64,
        archive_dir: &std::path::Path,
    ) -> Result<ArchivedLedgerReceipt, BillingError> {
        let _state_guard = self.state_lock.write().unwrap();
        std::fs::create_dir_all(archive_dir).map_err(|e| {
            BillingError::Persistence(format!("Failed to create archive directory: {e}"))
        })?;

        let sequence = self
            .snapshot_sequence
            .load(Ordering::Acquire)
            .saturating_add(1);
        let previous_checksum = self.last_snapshot_checksum.read().unwrap().clone();
        let mut candidate = self.export_snapshot_locked(sequence, previous_checksum);

        let mut to_archive = Vec::new();
        let mut to_keep = Vec::new();
        for entry in candidate.ledger {
            if entry.ts_secs < before_ts_secs {
                to_archive.push(entry);
            } else {
                to_keep.push(entry);
            }
        }

        if to_archive.is_empty() {
            return Err(BillingError::InvalidAdjustment(
                "No ledger entries match the archival cutoff timestamp".to_string(),
            ));
        }

        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let archive_id = format!(
            "arc-{}-{}-{}",
            now_secs,
            candidate.archived_ledger_receipts.len(),
            sequence
        );
        let archive_filename = format!("ledger_archive_{}.json", archive_id);
        let archive_path = archive_dir.join(&archive_filename);

        let payload = ArchivedLedgerPayload {
            archive_id: archive_id.clone(),
            created_at_secs: now_secs,
            before_ts_secs,
            entries_count: to_archive.len(),
            entries: to_archive,
        };
        let payload_json = serde_json::to_string_pretty(&payload)
            .map_err(|e| BillingError::InvalidState(format!("Failed to serialize archive: {e}")))?;
        let checksum = sha256_hex(payload_json.as_bytes());

        write_atomic_bytes(&archive_path, payload_json.as_bytes())
            .map_err(|e| BillingError::Persistence(format!("Failed to write archive file: {e}")))?;

        let receipt = ArchivedLedgerReceipt {
            archive_id,
            archive_file: archive_filename,
            drained_entries_count: payload.entries_count,
            sha256_checksum: checksum,
            before_ts_secs,
            created_at_secs: now_secs,
        };

        for entry in &payload.entries {
            candidate.archived_ledger_summary.include(entry);
        }
        candidate.ledger = to_keep.clone();
        candidate.archived_ledger_receipts.push(receipt.clone());

        self.commit_candidate_snapshot(&candidate, || {
            *self.ledger.write().unwrap() = to_keep;
            *self.archived_ledger_summary.write().unwrap() =
                candidate.archived_ledger_summary.clone();
            self.archived_ledger_receipts
                .write()
                .unwrap()
                .push(receipt.clone());
            receipt
        })
    }

    /// Complete engine verification for disaster recovery / restore operations (Spec §7, T07).
    ///
    /// Validates encryption, version, sequence, file size, anchor matching, deserialization,
    /// and business invariants (including balance reconciliation for every card against the append-only ledger).
    pub fn verify_snapshot_integrity<P: AsRef<std::path::Path>>(
        path: P,
        master_kek: Option<crate::crypto::MasterKek>,
    ) -> Result<SnapshotVerificationReport, BillingError> {
        let path = path.as_ref();
        let bytes = if snapshot_anchor_path(path).exists() {
            let anchor =
                read_snapshot_anchor(path).map_err(|e| BillingError::Persistence(e.to_string()))?;
            match std::fs::read(path) {
                Ok(bytes) if sha256_hex(&bytes) == anchor.checksum => bytes,
                _ => {
                    let generation = anchor.generation_file.ok_or_else(|| {
                        BillingError::Persistence(
                            "snapshot checksum mismatch and no generation available".into(),
                        )
                    })?;
                    std::fs::read(
                        path.parent()
                            .unwrap_or_else(|| std::path::Path::new("."))
                            .join(generation),
                    )
                    .map_err(|e| BillingError::Persistence(e.to_string()))?
                }
            }
        } else {
            std::fs::read(path).map_err(|e| BillingError::Persistence(e.to_string()))?
        };
        if bytes.is_empty() {
            return Err(BillingError::InvalidState("Snapshot file is empty".into()));
        }
        if bytes.len() > MAX_SNAPSHOT_BYTES {
            return Err(BillingError::InvalidState(format!(
                "Snapshot file size {} exceeds maximum allowed {}",
                bytes.len(),
                MAX_SNAPSHOT_BYTES
            )));
        }

        let is_encrypted = if let Ok(val) = serde_json::from_slice::<serde_json::Value>(&bytes) {
            val.get("format")
                .and_then(|f| f.as_str())
                .map(|s| s == "kiro-billing-aead-v1")
                .unwrap_or(false)
        } else {
            false
        };

        if is_encrypted && master_kek.is_none() {
            return Err(BillingError::Persistence(
                "Snapshot is AEAD-encrypted but no Master KEK was provided".into(),
            ));
        }

        let temp_engine = BillingEngine::new();
        if let Some(kek) = master_kek {
            temp_engine.set_master_kek(kek);
        }

        temp_engine
            .load_from_file(path)
            .map_err(|e| BillingError::Persistence(e.to_string()))?;

        let cards = temp_engine.list_all_cards();
        let ledger = temp_engine.ledger_entries();

        // Verify ledger reconciliation: for each card, usage entries must match credit_used
        for card in &cards {
            let archived = temp_engine.archived_ledger_summary.read().unwrap();
            let archived_card = archived.cards.get(&card.id).cloned().unwrap_or_default();
            let mut expected_credit_used = archived_card
                .usage
                .saturating_add(archived_card.negative_adjustments);
            for entry in ledger.iter().filter(|e| e.card_id == card.id) {
                match entry.kind {
                    LedgerKind::Usage => {
                        expected_credit_used =
                            expected_credit_used.saturating_add(entry.credits_charged);
                    }
                    LedgerKind::Adjustment if entry.credits_charged < 0 => {
                        expected_credit_used = expected_credit_used
                            .saturating_add(entry.credits_charged.saturating_neg());
                    }
                    _ => {}
                }
            }

            if card.credit_used != expected_credit_used {
                return Err(BillingError::InvalidState(format!(
                    "Card '{}' failed ledger reconciliation: credit_used={}, ledger_usage_sum={}",
                    card.id, card.credit_used, expected_credit_used
                )));
            }

            if card.available_credits() < 0 {
                return Err(BillingError::InvalidState(format!(
                    "Card '{}' has negative available credits: {}",
                    card.id,
                    card.available_credits()
                )));
            }
        }

        let sequence = temp_engine.snapshot_sequence.load(Ordering::Acquire);
        let checksum = temp_engine
            .last_snapshot_checksum
            .read()
            .unwrap()
            .clone()
            .unwrap_or_else(|| sha256_hex(&bytes));
        let ledger_count = ledger.len();
        let providers_count = temp_engine.providers.read().unwrap().len();

        Ok(SnapshotVerificationReport {
            is_encrypted,
            version: SNAPSHOT_VERSION,
            sequence,
            checksum,
            cards_count: cards.len(),
            ledger_entries_count: ledger_count,
            providers_count,
            is_balanced: true,
        })
    }
}

/// Detailed verification report for a billing snapshot (Spec §7, T07).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SnapshotVerificationReport {
    pub is_encrypted: bool,
    pub version: u32,
    pub sequence: u64,
    pub checksum: String,
    pub cards_count: usize,
    pub ledger_entries_count: usize,
    pub providers_count: usize,
    pub is_balanced: bool,
}

/// Verify the integrity and entry count of an exported ledger archive file (T03).
pub fn verify_ledger_archive(
    archive_path: &std::path::Path,
    expected_checksum: &str,
) -> Result<ArchivedLedgerPayload, BillingError> {
    let bytes = std::fs::read(archive_path)
        .map_err(|e| BillingError::Persistence(format!("Failed to read archive file: {e}")))?;
    let actual_checksum = sha256_hex(&bytes);
    if actual_checksum != expected_checksum {
        return Err(BillingError::InvalidState(format!(
            "Ledger archive checksum mismatch: expected {}, got {}",
            expected_checksum, actual_checksum
        )));
    }
    let payload: ArchivedLedgerPayload = serde_json::from_slice(&bytes)
        .map_err(|e| BillingError::InvalidState(format!("Invalid ledger archive payload: {e}")))?;
    if payload.entries_count != payload.entries.len() {
        return Err(BillingError::InvalidState(format!(
            "Ledger archive entry count mismatch: header reports {}, array has {}",
            payload.entries_count,
            payload.entries.len()
        )));
    }
    Ok(payload)
}

/// Read and parse a snapshot anchor file (T03).
pub fn read_snapshot_anchor(path: &std::path::Path) -> std::io::Result<SnapshotAnchor> {
    let anchor_path = snapshot_anchor_path(path);
    let content = std::fs::read_to_string(&anchor_path)?;
    let anchor: SnapshotAnchor = serde_json::from_str(&content).map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("invalid billing snapshot anchor: {e}"),
        )
    })?;
    if anchor.generation_file.as_deref().is_some_and(|name| {
        name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\', ':'])
    }) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "invalid snapshot generation filename",
        ));
    }
    Ok(anchor)
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, bytes);
    crate::crypto::hex::encode(digest.as_ref())
}

fn snapshot_anchor_path(path: &std::path::Path) -> std::path::PathBuf {
    let mut anchor = path.as_os_str().to_os_string();
    anchor.push(".anchor");
    std::path::PathBuf::from(anchor)
}

pub fn write_atomic_text(path: &std::path::Path, content: &str) -> std::io::Result<()> {
    write_atomic_bytes(path, content.as_bytes())
}

fn write_atomic_bytes(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temp = path.with_file_name(format!(
        "{}.tmp.{}.{}",
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("anchor"),
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    {
        use std::io::Write;
        let mut file = std::fs::File::create(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    atomic_replace(&temp, path)
}

fn prune_old_generations(path: &std::path::Path, current_sequence: u64) {
    let parent = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => std::path::Path::new("."),
    };
    let file_prefix = match path.file_name().and_then(|n| n.to_str()) {
        Some(name) => format!("{name}.gen_"),
        None => return,
    };
    if let Ok(entries) = std::fs::read_dir(parent) {
        for entry in entries.flatten() {
            let file_name = entry.file_name();
            let name_str = file_name.to_string_lossy();
            if let Some(suffix) = name_str.strip_prefix(&file_prefix) {
                if let Ok(seq) = suffix.parse::<u64>() {
                    // Keep at least current and previous generation (last 2 generations)
                    if seq < current_sequence.saturating_sub(1) {
                        let _ = std::fs::remove_file(entry.path());
                    }
                } else if name_str.contains(".tmp.") {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
    }
}

fn atomic_replace(temp: &std::path::Path, target: &std::path::Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use std::path::Path;

        const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
        const MOVEFILE_WRITE_THROUGH: u32 = 0x8;

        #[link(name = "kernel32")]
        extern "system" {
            fn MoveFileExW(
                existing_file_name: *const u16,
                new_file_name: *const u16,
                flags: u32,
            ) -> i32;
        }

        let source: Vec<u16> = Path::new(temp)
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let destination: Vec<u16> = Path::new(target)
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        if unsafe {
            MoveFileExW(
                source.as_ptr(),
                destination.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        std::fs::rename(temp, target)
    }
}

fn validate_snapshot(snapshot: &BillingSnapshot) -> std::io::Result<()> {
    if snapshot.version == 0
        || snapshot.cards.len() > 10_000_000
        || snapshot.ledger.len() > 50_000_000
        || snapshot.providers.len() > 100_000
        || snapshot.provider_keys.len() > 1_000_000
        || snapshot.consumed_refresh_tokens.len() > 10_000_000
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "billing snapshot contains invalid or excessive state",
        ));
    }
    if snapshot
        .consumed_refresh_tokens
        .iter()
        .any(|(jti, exp)| jti.is_empty() || jti.len() > 256 || *exp == 0)
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "billing snapshot contains invalid consumed refresh tokens",
        ));
    }
    if !snapshot.settings.credit_face_value_cny.is_finite()
        || !snapshot.settings.usd_cny_rate.is_finite()
        || snapshot.settings.credit_face_value_cny < 0.0
        || snapshot.settings.usd_cny_rate < 0.0
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "billing snapshot contains invalid billing settings",
        ));
    }
    for rates in snapshot.rates.values() {
        if !rates.credit_multiplier.is_finite()
            || !rates.margin_multiplier.is_finite()
            || rates.credit_multiplier < 0.0
            || rates.margin_multiplier < 0.0
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "billing snapshot contains invalid pricing multipliers",
            ));
        }
    }
    for version in &snapshot.rate_card_versions {
        if !version.input_price_per_m.is_finite()
            || !version.output_price_per_m.is_finite()
            || !version.cache_creation_price_per_m.is_finite()
            || !version.cache_read_price_per_m.is_finite()
            || !version.margin_multiplier.is_finite()
            || version.margin_multiplier < 0.0
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "billing snapshot contains invalid rate card values",
            ));
        }
    }
    let invalid = |message: String| std::io::Error::new(std::io::ErrorKind::InvalidData, message);
    let receipt_entries = snapshot
        .archived_ledger_receipts
        .iter()
        .try_fold(0usize, |sum, r| sum.checked_add(r.drained_entries_count))
        .ok_or_else(|| invalid("archive count overflow".into()))?;
    if receipt_entries != snapshot.archived_ledger_summary.entries_count {
        return Err(invalid("archive summary missing or inconsistent; restore/rebuild from verified archives before loading".into()));
    }
    for (id, summary) in &snapshot.archived_ledger_summary.cards {
        let usage = summary
            .usage_by_second
            .values()
            .try_fold(0i64, |sum, value| {
                if *value < 0 {
                    None
                } else {
                    sum.checked_add(*value)
                }
            });
        if !snapshot.cards.contains_key(id)
            || usage != Some(summary.usage)
            || summary.topups < 0
            || summary.positive_adjustments < 0
            || summary.negative_adjustments < 0
        {
            return Err(invalid("invalid archived card summary".into()));
        }
    }
    for (key, entry) in &snapshot.archived_ledger_summary.adjustments {
        if entry.kind != LedgerKind::Adjustment
            || entry.invocation_id.as_deref() != Some(key)
            || !snapshot.cards.contains_key(&entry.card_id)
        {
            return Err(invalid("invalid archived idempotency entry".into()));
        }
    }
    for (id, pending) in &snapshot.pending_settlements {
        let r = snapshot
            .reservations
            .get(id)
            .ok_or_else(|| invalid("pending settlement missing reservation".into()))?;
        let e = &pending.entry;
        if r.state != ReservationState::Held
            || r.card_id != e.card_id
            || e.invocation_id.as_deref() != Some(id)
            || e.kind != LedgerKind::Usage
            || e.credits_charged < 0
            || !snapshot.cards.contains_key(&e.card_id)
            || snapshot.ledger.iter().any(|entry| {
                entry.kind == LedgerKind::Usage && entry.invocation_id.as_deref() == Some(id)
            })
        {
            return Err(invalid("invalid pending settlement".into()));
        }
    }
    for card in snapshot.cards.values() {
        if card.credit_total < 0 || card.credit_used < 0 || card.credit_reserved < 0 {
            return Err(invalid(format!(
                "invalid balance invariant for card {}",
                card.id
            )));
        }
        let archived = snapshot
            .archived_ledger_summary
            .cards
            .get(&card.id)
            .cloned()
            .unwrap_or_default();
        let mut used = archived.usage.saturating_add(archived.negative_adjustments);
        for entry in snapshot.ledger.iter().filter(|e| e.card_id == card.id) {
            match entry.kind {
                LedgerKind::Usage => {
                    if entry.credits_charged < 0 {
                        return Err(invalid("negative usage charge".into()));
                    }
                    used = used.saturating_add(entry.credits_charged);
                }
                LedgerKind::Adjustment if entry.credits_charged < 0 => {
                    used = used.saturating_add(entry.credits_charged.saturating_neg())
                }
                _ => {}
            }
        }
        if card.credit_used != used {
            return Err(invalid(format!(
                "Card '{}' failed ledger reconciliation",
                card.id
            )));
        }
    }
    Ok(())
}

fn check_rebind_allowed(card: &Card, now_secs: u64) -> Result<(), BillingError> {
    if card.rebind_count >= card.max_rebinds {
        return Err(BillingError::RebindLimitExceeded {
            current: card.rebind_count,
            max: card.max_rebinds,
        });
    }
    if let Some(last) = card.last_rebind_at {
        let cooldown_until = last.saturating_add(card.rebind_cooldown_secs);
        if now_secs < cooldown_until {
            return Err(BillingError::RebindCooldown {
                remaining_secs: cooldown_until.saturating_sub(now_secs),
            });
        }
    }
    Ok(())
}

fn bind_device_locked(card: &mut Card, device_fp: &str) -> Result<bool, BillingError> {
    card.check_device_policy()?;
    if device_fp.trim().is_empty() {
        return Err(BillingError::InvalidState(
            "device fingerprint is required".into(),
        ));
    }
    let normalized = card.max_devices != 1;
    card.max_devices = 1;
    if card.bound_devices.iter().any(|d| d == device_fp) {
        return Ok(normalized);
    }
    if card.bound_devices.is_empty() {
        card.bound_devices.push(device_fp.to_string());
        return Ok(true);
    }
    Err(BillingError::DeviceAlreadyBound)
}

#[cfg(test)]
mod durability_regressions {
    use super::*;
    use std::sync::Barrier;

    fn state_path(name: &str) -> std::path::PathBuf {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target/audit-tests")
            .join(format!(
                "{name}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("state.json")
    }

    #[test]
    fn mirror_failure_after_anchor_is_committed_and_retry_is_idempotent() {
        let path = state_path("mirror");
        let engine = BillingEngine::new();
        engine.set_persistence_path(&path);
        engine.upsert_card(Card::new("card", "group", 100));
        *engine.injected_mirror_fault.write().unwrap() = true;
        let entry = engine
            .adjust_balance_idempotent("card", 10, "op", "bonus", 1, Some("adjust"))
            .unwrap();
        assert!(engine.last_snapshot_mirror_error().is_some());
        assert!(engine.persistence_ready());
        assert_eq!(engine.get_card("card").unwrap().credit_total, 110);
        // The mirror can even be absent; the anchor still makes this recoverable.
        std::fs::remove_file(&path).unwrap();
        BillingEngine::verify_snapshot_integrity(&path, None).unwrap();
        let recovered = BillingEngine::new();
        recovered.load_from_file(&path).unwrap();
        assert_eq!(recovered.get_card("card").unwrap().credit_total, 110);
        assert_eq!(
            recovered
                .adjust_balance_idempotent("card", 10, "op", "bonus", 2, Some("adjust"))
                .unwrap()
                .id,
            entry.id
        );
        assert_eq!(recovered.ledger_entries().len(), 1);
        *engine.injected_mirror_fault.write().unwrap() = false;
        engine.sync_to_disk_checked().unwrap();
        assert!(engine.last_snapshot_mirror_error().is_none());
    }

    #[test]
    fn sync_holds_state_through_publish_and_cannot_overwrite_newer_debit() {
        let path = state_path("sync-race");
        let engine = BillingEngine::new();
        engine.set_persistence_path(&path);
        engine.upsert_card(Card::new("card", "group", 100));
        let captured = Arc::new(Barrier::new(2));
        let resume = Arc::new(Barrier::new(2));
        *engine.save_snapshot_hook.lock().unwrap() = Some((captured.clone(), resume.clone()));
        let sync_engine = engine.clone();
        let save = std::thread::spawn(move || sync_engine.sync_to_disk_checked());
        captured.wait();
        assert!(
            engine.state_lock.try_write().is_err(),
            "snapshot export must retain state until commit"
        );
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let transaction_engine = engine.clone();
        let transaction = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            transaction_engine.adjust_balance("card", 10, "op", "bonus", 100)
        });
        started_rx.recv().unwrap();
        resume.wait();
        save.join().unwrap().unwrap();
        transaction.join().unwrap().unwrap();
        *engine.save_snapshot_hook.lock().unwrap() = None;
        let recovered = BillingEngine::new();
        recovered.load_from_file(&path).unwrap();
        assert_eq!(recovered.get_card("card").unwrap().credit_total, 110);
        assert_eq!(recovered.snapshot_sequence(), engine.snapshot_sequence());
    }

    #[test]
    fn committed_pending_survives_failed_debit_and_restart_without_refund() {
        let path = state_path("pending-debit");
        let engine = BillingEngine::new();
        engine.set_persistence_path(&path);
        let mut card = Card::new("card", "group", 100);
        card.status = CardStatus::Active;
        engine.upsert_card(card);
        engine
            .reserve("card", "use", &ReservationEstimateParams::new(0, 1), 1, 1)
            .unwrap();
        *engine.fail_after_pending.write().unwrap() = true;
        assert!(engine
            .settle(
                "use",
                &UsageTokens {
                    output_tokens: 3,
                    ..UsageTokens::default()
                },
                "m",
                "p",
                "t",
                2
            )
            .is_err());
        assert_eq!(engine.get_card("card").unwrap().credit_used, 0);
        assert_eq!(engine.list_pending_settlements().len(), 1);
        let recovered = BillingEngine::new();
        recovered.load_from_file(&path).unwrap();
        assert_eq!(recovered.list_pending_settlements().len(), 1);
        assert_eq!(recovered.get_card("card").unwrap().credit_reserved, 60);
        assert_eq!(recovered.run_janitor(1000), 0);
        assert!(recovered.release("use").is_err());
        let entry = recovered.retry_pending_settlement("use").unwrap();
        assert_eq!(entry.credits_charged, 180);
        assert_eq!(recovered.get_card("card").unwrap().outstanding_debt(), 80);
        BillingEngine::verify_snapshot_integrity(&path, None).unwrap();
    }
    #[test]
    fn automatic_recovery_backoff_restart_and_invalid_intent() {
        let path = state_path("automatic-recovery");
        let engine = BillingEngine::new();
        engine.set_persistence_path(&path);
        let mut card = Card::new("card", "group", 1000);
        card.activate(1, 0).unwrap();
        engine.upsert_card(card);
        engine
            .reserve("card", "use", &ReservationEstimateParams::new(0, 1), 1, 10)
            .unwrap();
        // Before intent commit: no partial debit; restart reclaims the orphan without a durable intent.
        *engine.injected_persistence_fault.write().unwrap() = true;
        assert!(engine
            .settle(
                "use",
                &UsageTokens {
                    output_tokens: 3,
                    ..Default::default()
                },
                "m",
                "p",
                "t",
                2
            )
            .is_err());
        let before = BillingEngine::new();
        before.load_from_file(&path).unwrap();
        assert!(before.list_pending_settlements().is_empty());
        assert_eq!(before.get_card("card").unwrap().credit_reserved, 0);
        assert_eq!(before.get_card("card").unwrap().credit_used, 0);
        let engine = before;
        engine
            .reserve("card", "use", &ReservationEstimateParams::new(0, 1), 1, 10)
            .unwrap();
        *engine.fail_after_pending.write().unwrap() = true;
        assert!(engine
            .settle(
                "use",
                &UsageTokens {
                    output_tokens: 3,
                    ..Default::default()
                },
                "m",
                "p",
                "t",
                2
            )
            .is_err());
        let recovered = BillingEngine::new();
        recovered.load_from_file(&path).unwrap();
        let mut recovery = PendingSettlementRecovery::default();
        *recovered.injected_persistence_fault.write().unwrap() = true;
        let mut now = 100;
        for delay in [30, 60, 120, 240, 300, 300] {
            let results = recovery.tick(&recovered, now);
            assert_eq!(results.len(), 1);
            assert!(results[0].1.is_err());
            assert!(recovery.tick(&recovered, now + delay - 1).is_empty());
            assert_eq!(recovered.run_janitor(now + delay), 0);
            assert!(recovered.release("use").is_err());
            assert_eq!(recovered.get_card("card").unwrap().credit_used, 0);
            now += delay;
        }
        *recovered.injected_persistence_fault.write().unwrap() = false;
        // A corrupt invocation field must alert and retain the hold, not disappear from the retry queue.
        recovered
            .pending_settlements
            .write()
            .unwrap()
            .get_mut("use")
            .unwrap()
            .entry
            .invocation_id = None;
        assert!(recovery.tick(&recovered, now)[0].1.is_err());
        assert_eq!(recovered.get_card("card").unwrap().credit_reserved, 60);
        // Restart from the uncorrupted durable intent resets backoff and succeeds.
        let restarted = BillingEngine::new();
        restarted.load_from_file(&path).unwrap();
        *restarted.injected_mirror_fault.write().unwrap() = true;
        let mut fresh = PendingSettlementRecovery::default();
        assert!(fresh.tick(&restarted, now)[0].1.is_ok());
        assert!(fresh.tick(&restarted, now).is_empty());
        restarted.retry_pending_settlement("use").unwrap();
        assert_eq!(restarted.get_card("card").unwrap().credit_used, 180);
        assert_eq!(restarted.get_card("card").unwrap().credit_reserved, 0);
        assert_eq!(restarted.ledger_entries().len(), 1);
        assert!(restarted.last_snapshot_mirror_error().is_some());
        *restarted.injected_mirror_fault.write().unwrap() = false;
        restarted.run_janitor(700000);
        assert_eq!(
            restarted.export_snapshot().reservations["use"].state,
            ReservationState::Settled
        );
        let disk = BillingEngine::new();
        disk.load_from_file(&path).unwrap();
        assert_eq!(
            disk.export_snapshot().reservations["use"].state,
            ReservationState::Settled
        );
        disk.retry_pending_settlement("use").unwrap();
        assert_eq!(disk.ledger_entries().len(), 1);
    }

    #[test]
    fn automatic_recovery_limits_batch_without_starvation() {
        let engine = BillingEngine::new();
        let mut card = Card::new("card", "group", 10000);
        card.activate(1, 0).unwrap();
        card.max_concurrency = 100;
        engine.upsert_card(card);
        for i in 0..65 {
            engine
                .reserve(
                    "card",
                    &format!("use-{i:03}"),
                    &ReservationEstimateParams::new(0, 1),
                    1,
                    10,
                )
                .unwrap();
        }
        let mut snapshot = engine.export_snapshot();
        let tokens = UsageTokens {
            output_tokens: 1,
            ..Default::default()
        };
        let prototype = engine.settle("use-000", &tokens, "m", "p", "t", 2).unwrap();
        for i in 0..65 {
            let id = format!("use-{i:03}");
            let mut entry = prototype.clone();
            entry.id = format!("led-{id}");
            entry.invocation_id = Some(id.clone());
            snapshot
                .pending_settlements
                .insert(id, PendingSettlement { entry, tokens });
        }
        engine.import_snapshot(snapshot);
        let mut recovery = PendingSettlementRecovery::default();
        let first = recovery.tick(&engine, 100);
        assert_eq!(first.len(), 64);
        assert!(first.iter().all(|(_, result)| result.is_ok()));
        let second = recovery.tick(&engine, 100);
        assert_eq!(second.len(), 1);
        assert!(second[0].1.is_ok());
        assert!(recovery.tick(&engine, 100).is_empty());
        assert_eq!(engine.get_card("card").unwrap().credit_used, 3900);
        assert_eq!(engine.get_card("card").unwrap().credit_reserved, 0);
    }
}
