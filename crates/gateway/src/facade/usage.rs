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
    pub next_date_reset: u64,
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
    pub subscription_info: SubscriptionInfo,
    pub usage_breakdown_list: Vec<UsageBreakdown>,
    pub overage_configuration: OverageConfiguration,
    pub user_info: UserInfo,
    pub days_until_reset: u32,
    pub next_date_reset: u64,
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
            let next_reset = now_secs
                .saturating_div(86_400)
                .saturating_add(30)
                .saturating_mul(86_400);
            let credit_label = std::env::var("CREDIT_LABEL")
                .ok()
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| "算力积分".to_string());
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

            let resp = GetUsageLimitsResponse {
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
                    display_name: credit_label.clone(),
                    display_name_plural: credit_label,
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
                days_until_reset: 30,
                next_date_reset: next_reset,
            };
            json_response(StatusCode::OK, &resp)
        })
    }
}
