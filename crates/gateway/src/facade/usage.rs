//! Usage and credit limits handler.
//!
//! Spec §4.2, P0-5 (mgmt-schema.md §2.4).

use super::virtualization::VirtualizationStore;
use super::{json_response, BoxFuture, FacadeHandler, Response};
use crate::auth::AuthClaims;
use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubscriptionInfo {
    pub subscription_title: String,
    #[serde(rename = "type")]
    pub sub_type: String,
    pub overage_capability: String,
    pub subscription_management_target: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Currency {
    pub code: String,
    pub symbol: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageBreakdown {
    pub display_name: String,
    pub display_name_plural: String,
    pub current_usage: f64,
    pub current_usage_with_precision: f64,
    pub usage_limit: f64,
    pub usage_limit_with_precision: f64,
    pub currency: Currency,
    pub unit: String,
    pub dimension_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_date_reset: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OverageConfiguration {
    pub overage_status: String,
    pub overage_enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInfo {
    pub email: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetUsageLimitsResponse {
    /// Spendable credits after settled usage and active reservations; null without a card.
    #[serde(default)]
    pub available_credits: Option<f64>,
    #[serde(default)]
    pub virtual_plan_name: String,
    #[serde(default)]
    pub valid_until: Option<u64>,
    #[serde(default)]
    pub settled_usage: Option<billing::settled_usage::SettledUsage>,
    #[serde(default)]
    pub settled_usage_unavailable_reason: Option<String>,
    pub subscription_info: SubscriptionInfo,
    pub usage_breakdown_list: Vec<UsageBreakdown>,
    pub overage_configuration: OverageConfiguration,
    pub user_info: UserInfo,
    /// Prepaid credits never reset: these give the card's expiry, or [`NEVER_EXPIRES`] for
    /// a card that never expires. Kiro's account page prints "resets on MM/DD" from them
    /// whatever they are, "NaN/NaN" when they are left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub days_until_reset: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_date_reset: Option<u64>,
}

/// The date a card that never expires is given as its reset: 9999-12-31, which Kiro's
/// account page shows as "12/31", no near reset.
pub const NEVER_EXPIRES: u64 = 253_402_214_400;

/// A compact badge, without changing the stored plan or its independent expiry.
fn compact_plan_title(plan: &str) -> String {
    if plan.eq_ignore_ascii_case("pro") {
        "PRO"
    } else {
        plan
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    #[test]
    fn compact_plan_titles() {
        for plan in ["pro", "Pro", "PRO"] {
            assert_eq!(super::compact_plan_title(plan), "PRO");
        }
        for plan in ["PRO+", "PRO Max", "Power", "体验卡", "Legacy service plan"] {
            assert_eq!(super::compact_plan_title(plan), plan);
        }
    }
}

/// Credits to the hundredth, as the rest of Kiro shows them.
fn two_places(credits: f64) -> f64 {
    (credits * 100.0).round() / 100.0
}

/// What Kiro shows as the signed-in account, and its initial as the avatar: the card, by
/// the end of its ID, which the operator's console lists (not of its code, which the
/// portal shows as "卡密 •••• …"). `<card id>@kiro-byok.local` read like an address that
/// does not exist.
fn account_label(card_id: &str) -> String {
    let skip = card_id.chars().count().saturating_sub(4);
    let tail: String = card_id.chars().skip(skip).collect();
    format!("卡号 ····{tail}")
}

/// Handler for `POST /setUserPreference`, which Kiro sends to turn overages on or off. A
/// card is prepaid and has no overages; Kiro shows the message as "Unable to enable
/// overages: {message}", where a 404 said only that the resource was not found.
pub struct SetUserPreferenceHandler;

impl FacadeHandler for SetUserPreferenceHandler {
    fn method(&self) -> Method {
        Method::POST
    }

    fn path(&self) -> &'static str {
        "/setUserPreference"
    }

    fn handle<'a>(&'a self, req: Request<Body>) -> BoxFuture<'a, Response> {
        Box::pin(async move {
            super::discard_body(req.into_body(), 64 * 1024).await;
            super::error_response(
                StatusCode::BAD_REQUEST,
                "ValidationException",
                "本服务的积分为预付费，没有超额用量（overage）可以开启或关闭。",
            )
        })
    }
}

/// Handler for `GET /getUsageLimits`
#[derive(Clone, Default)]
pub struct GetUsageLimitsHandler {
    store: VirtualizationStore,
}

impl GetUsageLimitsHandler {
    pub fn new(store: VirtualizationStore) -> Self {
        Self { store }
    }
}

impl FacadeHandler for GetUsageLimitsHandler {
    fn method(&self) -> Method {
        Method::GET
    }

    fn path(&self) -> &'static str {
        "/getUsageLimits"
    }

    fn handle<'a>(&'a self, req: Request<Body>) -> BoxFuture<'a, Response> {
        Box::pin(async move {
            let now_secs = crate::now_secs();
            let claims = req.extensions().get::<AuthClaims>();
            let group = self.store.get_group(claims.map(|c| c.group_id.as_str()));
            let card_id = claims.map(|c| c.card_id.as_str()).unwrap_or("dev-user");

            // Purchased card balance, not the group's display quota.
            let card = self
                .store
                .billing()
                .and_then(|billing| billing.get_card(card_id));
            let current_usage = card
                .as_ref()
                .map(|c| c.credit_used as f64 / 1_000_000.0)
                .unwrap_or(group.current_usage);
            let usage_limit = card
                .as_ref()
                .map(|c| c.credit_total as f64 / 1_000_000.0)
                .unwrap_or(group.virtual_usage_limit.max(0.0));

            let settled_usage = self
                .store
                .billing()
                .and_then(|b| b.settled_usage(card_id, now_secs));
            // A card's credits last until it expires; they never refill, and Kiro has no
            // way to say so: its account page prints "resets on MM/DD" whatever it is sent,
            // "NaN/NaN" without a date. It gets the card's expiry, or a date that is plainly
            // no near refill for a card that never expires. Expiry remains a separate
            // field instead of being appended to the badge. The usage line carries no date: Kiro
            // announced one that moved later as "Your usage is reset. You now have N
            // Credits in the new month", so extending cards' validity read as a monthly
            // refill to each of them. Without it, Kiro's low-credit warning ends with its
            // words for no date, "resets at the end of the calendar month".
            let valid_until = card.as_ref().and_then(|c| c.valid_until);
            let next_reset = valid_until.unwrap_or(NEVER_EXPIRES);
            let days_until_reset =
                (next_reset.saturating_sub(now_secs).div_ceil(86_400)).min(u32::MAX as u64) as u32;
            let plan = card
                .as_ref()
                .and_then(|c| c.plan_name())
                .unwrap_or("Legacy service plan");
            let resp = GetUsageLimitsResponse {
                available_credits: card
                    .as_ref()
                    .map(|c| c.available_credits() as f64 / 1_000_000.0),
                virtual_plan_name: plan.to_string(),
                valid_until,
                settled_usage_unavailable_reason: settled_usage
                    .is_none()
                    .then(|| "Settled ledger detail unavailable for this UTC window".to_string()),
                settled_usage,
                subscription_info: SubscriptionInfo {
                    // Keep the compact Kiro badge to the plan name; expiry remains in valid_until.
                    subscription_title: compact_plan_title(plan),
                    sub_type: card
                        .as_ref()
                        .map(|c| c.plan_type())
                        .unwrap_or("CUSTOM")
                        .to_string(),
                    overage_capability: "DISABLED".to_string(),
                    subscription_management_target: "MANAGE".to_string(),
                },
                usage_breakdown_list: vec![UsageBreakdown {
                    // Keep the native Kiro credit terminology independent of portal branding.
                    display_name: "Credit".to_string(),
                    display_name_plural: "Credits".to_string(),
                    // Kiro prints these as they come: "12.345678/100" in its status bar.
                    current_usage: two_places(current_usage),
                    current_usage_with_precision: two_places(current_usage),
                    usage_limit: two_places(usage_limit),
                    usage_limit_with_precision: two_places(usage_limit),
                    currency: Currency {
                        code: "USD".to_string(),
                        symbol: "$".to_string(),
                    },
                    unit: "INVOCATIONS".to_string(),
                    dimension_type: "CREDIT".to_string(),
                    next_date_reset: None,
                }],
                overage_configuration: OverageConfiguration {
                    overage_status: "DISABLED".to_string(),
                    overage_enabled: false,
                },
                user_info: UserInfo {
                    email: account_label(card_id),
                },
                days_until_reset: Some(days_until_reset),
                next_date_reset: Some(next_reset),
            };
            let mut response = json_response(StatusCode::OK, &resp);
            response.headers_mut().insert(
                axum::http::header::CACHE_CONTROL,
                axum::http::HeaderValue::from_static("no-store"),
            );
            response
        })
    }
}
