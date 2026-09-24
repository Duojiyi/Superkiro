//! `billing`: Card keys, groups, ledger, credit reservation, and rate card domain logic.
//!
//! Spec §5 (Data Model) & Spec §6 (Billing & Metering).

pub mod card;
pub mod card_platform;
pub mod context;
pub mod crypto;
pub mod engine;
pub mod generator;
pub mod group;
pub mod ledger;
pub mod observability;
pub mod provider;
pub mod rate_card;
pub mod reservation;
pub mod settled_usage;
pub mod template;
pub mod tenant;
pub mod topup;
pub mod workbench;

pub use context::{ContextUsageMetric, ModelContextLibrary, ModelContextPreset};
pub use crypto::{
    mask_provider_key, rotate_all_provider_keys, rotate_provider_key, CryptoError, MasterKek,
    KEK_LEN,
};
pub use observability::{
    compute_margin_dashboard, compute_model_cost_rankings, compute_provider_health,
    export_reconciliation_csv, export_reconciliation_json, prune_traces_in_place, Announcement,
    AnnouncementLevel, AnomalyAction, AnomalyAlert, AttemptRecord, DailyUsageSummary,
    MarginDashboard, ModelCostRanking, ProviderHealthSummary, RequestTrace, TraceStatus,
};
pub use provider::{HealthState, Provider, ProviderFormat, ProviderKey};
pub use tenant::{TenantContext, TenantViolation};

pub use card::{
    hash_card_code, normalize_card_code, verify_card_code, Card, CardError, CardStatus,
};
pub use card_platform::{
    CardPlatformManager, DispensedCardItem, InventoryStockResponse, PullCardsRequest,
    PullCardsResponse, RedeemAction, RedeemCallbackRequest, RedeemCallbackResponse,
};
pub use engine::{
    read_snapshot_anchor, verify_ledger_archive, verify_ledger_archive_with, ArchivedCardSummary,
    ArchivedLedgerPayload, ArchivedLedgerReceipt, ArchivedLedgerSummary, BillingEngine,
    BillingError, BillingSnapshot, CardReconciliation, IssuanceOrder, PendingSettlement,
    SnapshotAnchor, SnapshotVerificationReport, UnpaidCharge,
};
pub use generator::{export_csv, export_json, generate_batch, generate_card, GeneratedCard};
pub use group::{FallbackTarget, Group, ModelMap, ProviderBindingMode};
pub use ledger::{LedgerEntry, LedgerKind, PricingRates, UsageTokens};
pub use rate_card::{
    BillingSettings, Currency, MarginSummary, PricingMode, RateCard, RateCardVersion,
};
pub use reservation::{CreditReservation, ReservationEstimateParams, ReservationState};
pub use template::CardTemplate;
pub use topup::{
    export_topup_csv, export_topup_json, generate_topup_batch, generate_topup_code,
    GeneratedTopupCode, TopupCode,
};
pub use workbench::{
    calibrate_plan, simulate_candidate_pricing, ModelPresets, PlanCalibrationInput,
    PlanCalibrationOutput, PricingSheetRow, RateCardAuditLog, SimulationResult,
};

/// 1 credit = 1_000_000 micro credits (integer micro-credits precision, Spec §5).
pub const MICRO_CREDITS_PER_CREDIT: i64 = 1_000_000;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_micro_credits_precision() {
        assert_eq!(MICRO_CREDITS_PER_CREDIT, 1_000_000);
    }
}
