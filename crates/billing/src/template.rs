//! Card template definitions, the plan catalog, and presets (Spec §5, §14.9).

use crate::group::Group;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// What a card of an issuance tier sells for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanPrice {
    pub template_id: &'static str,
    pub name: &'static str,
    pub points: i64,
    pub price_micro_cny: i64,
    /// What Kiro is told a card of the tier subscribes to.
    pub kiro_plan_type: &'static str,
}

/// The tiers cards were issued from before plans could be edited: the plans a state that has
/// stored none still has, and what a card issued before the catalog is taken to be, by the
/// credits it was issued with.
pub const PLAN_PRICES: [PlanPrice; 4] = [
    PlanPrice {
        template_id: "tier-1000",
        name: "PRO",
        points: 1000,
        price_micro_cny: 30_000_000,
        kiro_plan_type: "PRO",
    },
    PlanPrice {
        template_id: "tier-2000",
        name: "PRO+",
        points: 2000,
        price_micro_cny: 55_000_000,
        kiro_plan_type: "PRO_PLUS",
    },
    PlanPrice {
        template_id: "tier-5000",
        name: "PRO Max",
        points: 5000,
        price_micro_cny: 130_000_000,
        kiro_plan_type: "PRO_MAX",
    },
    PlanPrice {
        template_id: "tier-10000",
        name: "Power",
        points: 10000,
        price_micro_cny: 250_000_000,
        kiro_plan_type: "POWER",
    },
];

/// What Kiro accepts as a subscription type, as the gateway reports a card's.
pub const KIRO_PLAN_TYPES: [&str; 5] = ["PRO", "PRO_PLUS", "PRO_MAX", "POWER", "CUSTOM"];

/// Most plans a catalog holds.
pub const MAX_PLANS: usize = 100;

/// A plan (套餐) in the catalog cards are issued from: what a card sells for and gives.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Plan {
    /// 1-64 of a-z, 0-9 and -.
    pub id: String,
    /// What the customer's client calls it; 1-32 bytes.
    pub name: String,
    /// Credits a card is issued with.
    pub points: i64,
    /// What a card sells for, in yuan to the fen.
    pub price_cny: f64,
    /// How long a card is valid from its activation.
    pub validity_days: u32,
    /// Devices a card may bind: 1, the only number issuance takes. Kept so that cards with
    /// more devices can raise it once they exist.
    pub max_devices: u32,
    /// Requests a card may have in flight at once.
    pub concurrency: u32,
    /// The group issuance offers first; a card may be issued into another.
    pub default_group_id: String,
    /// One of [`KIRO_PLAN_TYPES`].
    pub kiro_plan_type: String,
    /// Off sale, it issues no card; the cards issued from it keep it.
    pub on_sale: bool,
    pub sort_order: i32,
}

/// The plan a card was issued from, as it was then: the card keeps it whatever the catalog
/// becomes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssuedPlan {
    pub id: String,
    pub name: String,
    pub points: i64,
    pub price_micro_cny: i64,
    pub validity_days: u32,
    pub max_devices: u32,
    pub concurrency: u32,
    pub kiro_plan_type: String,
}

impl Plan {
    /// Its price in micro-CNY.
    pub fn price_micro_cny(&self) -> i64 {
        (self.price_cny * 1_000_000.0).round() as i64
    }

    /// What a card issued from it keeps.
    pub fn snapshot(&self) -> IssuedPlan {
        IssuedPlan {
            id: self.id.clone(),
            name: self.name.clone(),
            points: self.points,
            price_micro_cny: self.price_micro_cny(),
            validity_days: self.validity_days,
            max_devices: self.max_devices,
            concurrency: self.concurrency,
            kiro_plan_type: self.kiro_plan_type.clone(),
        }
    }

    /// The template that issues its cards into `group_id`.
    pub fn template(&self, group_id: &str) -> CardTemplate {
        let mut template = CardTemplate::new(
            self.id.as_str(),
            self.name.as_str(),
            u64::from(self.validity_days) * 86_400,
            self.points.saturating_mul(crate::MICRO_CREDITS_PER_CREDIT),
            group_id,
        );
        template.max_devices = self.max_devices;
        template.max_concurrency = self.concurrency;
        template.plan = Some(self.snapshot());
        template
    }

    /// Which of its own fields is out of bounds, if any; its default group is checked
    /// against the configuration it is published into.
    pub fn problem(&self) -> Option<&'static str> {
        let price = self.price_cny * 100.0;
        if !(1..=64).contains(&self.id.len())
            || !self
                .id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            Some("Plan IDs are 1-64 of a-z, 0-9 and -")
        } else if self.name.trim().is_empty()
            || self.name.len() > 32
            || self.name.chars().any(char::is_control)
        {
            Some("Plan names are 1-32 bytes")
        } else if !(1..=10_000_000).contains(&self.points) {
            Some("Plan points must be 1-10000000")
        } else if !(0.0..=100_000.0).contains(&self.price_cny)
            || (price.round() - price).abs() > 1e-6
        {
            Some("Plan prices must be 0-100000 yuan, to the fen")
        } else if !(1..=3650).contains(&self.validity_days) {
            Some("Plan validity must be 1-3650 days")
        } else if self.max_devices != 1 {
            // A plan for more would be on sale and never issue a card.
            Some("Plans allow exactly 1 device, as cards bind one")
        } else if !(1..=20).contains(&self.concurrency) {
            Some("Plan concurrency must be 1-20")
        } else if !KIRO_PLAN_TYPES.contains(&self.kiro_plan_type.as_str()) {
            Some("Plan Kiro types are PRO, PRO_PLUS, PRO_MAX, POWER or CUSTOM")
        } else {
            None
        }
    }
}

/// The plans a state that has stored none has: the four tiers as they were issued before
/// the catalog, 30 days, one device, two requests at once, into group-pro-plus or, without
/// it, the first group by ID that takes cards: never one closed to issuance, such as the
/// acceptance probe group, which no customer is in.
pub fn seed_plans(groups: &HashMap<String, Group>) -> Vec<Plan> {
    let default_group = if groups.contains_key("group-pro-plus") {
        "group-pro-plus"
    } else {
        groups
            .values()
            .filter(|group| group.issuance_enabled)
            .map(|group| group.id.as_str())
            .min()
            .unwrap_or("group-pro-plus")
    };
    PLAN_PRICES
        .iter()
        .zip([10, 20, 30, 40])
        .map(|(tier, sort_order)| Plan {
            id: tier.template_id.to_string(),
            name: tier.name.to_string(),
            points: tier.points,
            price_cny: tier.price_micro_cny as f64 / 1_000_000.0,
            validity_days: 30,
            max_devices: 1,
            concurrency: 2,
            default_group_id: default_group.to_string(),
            kiro_plan_type: tier.kiro_plan_type.to_string(),
            on_sale: true,
            sort_order,
        })
        .collect()
}

/// The catalog in force, in its order: the plans stored or, while none are, the seed.
pub fn plan_catalog(stored: Option<&[Plan]>, groups: &HashMap<String, Group>) -> Vec<Plan> {
    let mut plans = stored.map_or_else(|| seed_plans(groups), <[Plan]>::to_vec);
    plans.sort_by(|a, b| {
        a.sort_order
            .cmp(&b.sort_order)
            .then_with(|| a.id.cmp(&b.id))
    });
    plans
}

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
    /// The plan its cards keep, when it issues from the catalog.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<IssuedPlan>,
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
            plan: None,
        }
    }

    /// A tier as it was issued before the plan catalog. Preserve the old default ID as a PRO+
    /// alias.
    pub fn tier(id: &str, group_id: &str) -> Option<Self> {
        let tier_id = if id == "standard-monthly" {
            "tier-2000"
        } else {
            id
        };
        let tier = PLAN_PRICES
            .iter()
            .find(|tier| tier.template_id == tier_id)?;
        Some(Self::new(
            id,
            tier.name,
            2_592_000,
            tier.points * 1_000_000,
            group_id,
        ))
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
