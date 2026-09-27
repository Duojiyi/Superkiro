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
    /// Prepaid credits never reset: these give the card's expiry, when it has one, and are
    /// left out when it has none, which Kiro shows as no reset date.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub days_until_reset: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_date_reset: Option<u64>,
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
            // A card's credits last until it expires; they never refill. "Resets in 30 days"
            // was always wrong.
            let next_reset = card.as_ref().and_then(|c| c.valid_until);
            let days_until_reset = next_reset.map(|until| {
                (until.saturating_sub(now_secs).div_ceil(86_400)).min(u32::MAX as u64) as u32
            });
            let resp = GetUsageLimitsResponse {
                available_credits: card
                    .as_ref()
                    .map(|c| c.available_credits() as f64 / 1_000_000.0),
                virtual_plan_name: card
                    .as_ref()
                    .and_then(|c| c.plan_name())
                    .unwrap_or("Legacy service plan")
                    .to_string(),
                valid_until: card.as_ref().and_then(|c| c.valid_until),
                settled_usage_unavailable_reason: settled_usage
                    .is_none()
                    .then(|| "Settled ledger detail unavailable for this UTC window".to_string()),
                settled_usage,
                subscription_info: SubscriptionInfo {
                    subscription_title: card
                        .as_ref()
                        .and_then(|c| c.plan_name())
                        .unwrap_or("Legacy service plan")
                        .to_string(),
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
                    current_usage,
                    current_usage_with_precision: current_usage,
                    usage_limit,
                    usage_limit_with_precision: usage_limit,
                    currency: Currency {
                        code: "USD".to_string(),
                        symbol: "$".to_string(),
                    },
                    unit: "INVOCATIONS".to_string(),
                    dimension_type: "CREDIT".to_string(),
                    next_date_reset: next_reset,
                }],
                overage_configuration: OverageConfiguration {
                    overage_status: "DISABLED".to_string(),
                    overage_enabled: false,
                },
                user_info: UserInfo {
                    email: format!("{}@kiro-byok.local", card_id),
                },
                days_until_reset,
                next_date_reset: next_reset,
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
