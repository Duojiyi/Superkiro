//! Card template definitions and presets (Spec §5, §14.9).

use serde::{Deserialize, Serialize};

/// Card type template (Spec §5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardTemplate {
    pub id: String,
    pub name: String,
    pub duration_secs: u64, // 0 = Perpetual / Pay-as-you-go
    pub credit_total: i64,  // In micro-credits (1 credit = 1_000_000 micro-credits)
    pub max_devices: u32,
    pub max_concurrency: u32,
    pub group_id: String,
    pub daily_credit_limit: Option<i64>,
    pub monthly_credit_limit: Option<i64>,
}

impl CardTemplate {
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        duration_secs: u64,
        credit_total: i64,
        group_id: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            duration_secs,
            credit_total,
            max_devices: 1,
            max_concurrency: 2,
            group_id: group_id.into(),
            daily_credit_limit: None,
            monthly_credit_limit: None,
        }
    }

    /// Built-in issuance catalog. Preserve the old default ID as a PRO+ alias.
    pub fn tier(id: &str, group_id: &str) -> Option<Self> {
        let (name, points) = match id {
            "tier-1000" => ("PRO", 1000),
            "standard-monthly" | "tier-2000" => ("PRO+", 2000),
            "tier-5000" => ("PRO Max", 5000),
            "tier-10000" => ("Power", 10000),
            _ => return None,
        };
        Some(Self::new(id, name, 2_592_000, points * 1_000_000, group_id))
    }

    /// Builder method to specify quotas and limits (Spec §5, §6.6, §14.9).
    pub fn with_limits(
        mut self,
        daily_limit: Option<i64>,
        monthly_limit: Option<i64>,
        max_concurrency: u32,
    ) -> Self {
        self.daily_credit_limit = daily_limit;
        self.monthly_credit_limit = monthly_limit;
        self.max_concurrency = max_concurrency;
        self
    }

    /// Preset: 1-day trial card (86,400s, 10 credits = 10,000,000 micro-credits)
    pub fn daily(id: &str, group_id: &str) -> Self {
        Self::new(id, "日卡 (1天)", 86_400, 10_000_000, group_id)
    }

    /// Preset: 7-day weekly card (604,800s, 50 credits = 50,000,000 micro-credits)
    pub fn weekly(id: &str, group_id: &str) -> Self {
        Self::new(id, "周卡 (7天)", 604_800, 50_000_000, group_id)
    }

    /// Preset: 30-day monthly card (2,592,000s, 200 credits = 200,000,000 micro-credits)
    pub fn monthly(id: &str, group_id: &str) -> Self {
        Self::new(id, "月卡 (30天)", 2_592_000, 200_000_000, group_id)
    }

    /// Preset: Perpetual pay-as-you-go card (duration = 0, never expires)
    pub fn pay_as_you_go(id: &str, credit_total: i64, group_id: &str) -> Self {
        Self::new(id, "按量卡 (永不过期)", 0, credit_total, group_id)
    }
}
