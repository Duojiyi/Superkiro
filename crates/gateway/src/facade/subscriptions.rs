//! Subscriptions and subscription token handlers.
//!
//! Spec §4.2, P0-5 (mgmt-schema.md §2.3).

use super::virtualization::VirtualizationStore;
use super::{json_response, BoxFuture, FacadeHandler, Response};
use crate::auth::AuthClaims;
use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubscriptionPricing {
    pub currency: String,
    pub amount: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubscriptionPlan {
    pub q_subscription_type: String,
    pub description: String,
    pub pricing: SubscriptionPricing,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListAvailableSubscriptionsResponse {
    pub subscription_plans: Vec<SubscriptionPlan>,
    pub disclaimer: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateSubscriptionTokenResponse {
    pub subscription_token: String,
    pub expires_at: String,
    /// What the "Manage Plan" button of Kiro's account page opens: the gateway's customer
    /// portal, where the card is looked up. Without it the button did nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encoded_verification_url: Option<String>,
    /// Kiro's own names for the rest of the answer.
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub token: String,
}

/// The customer portal's address as the client reached the gateway: behind the proxy, the
/// host it forwarded and the scheme it was reached by.
fn portal_url(headers: &axum::http::HeaderMap) -> Option<String> {
    let value = |name: &str| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(|value| value.split(',').next().unwrap_or(value).trim().to_string())
            .filter(|value| !value.is_empty())
    };
    let host = value("x-forwarded-host").or_else(|| value("host"))?;
    if host.contains(['/', '?', '#', '@', ' ', '\\']) {
        return None;
    }
    let local = host.starts_with("localhost") || host.starts_with("127.0.0.1");
    let scheme = value("x-forwarded-proto")
        .filter(|scheme| scheme == "http" || scheme == "https")
        .unwrap_or_else(|| if local { "http" } else { "https" }.to_string());
    Some(format!("{scheme}://{host}/portal"))
}

/// Handler for `POST /listAvailableSubscriptions`
#[derive(Clone, Default)]
pub struct ListAvailableSubscriptionsHandler {
    store: VirtualizationStore,
}

impl ListAvailableSubscriptionsHandler {
    pub fn new(store: VirtualizationStore) -> Self {
        Self { store }
    }
}

impl FacadeHandler for ListAvailableSubscriptionsHandler {
    fn method(&self) -> Method {
        Method::POST
    }

    fn path(&self) -> &'static str {
        "/listAvailableSubscriptions"
    }

    fn handle<'a>(&'a self, req: Request<Body>) -> BoxFuture<'a, Response> {
        Box::pin(async move {
            let claims = req.extensions().get::<AuthClaims>();
            let card = claims.and_then(|c| self.store.billing()?.get_card(&c.card_id));
            let plan = card
                .as_ref()
                .and_then(|c| c.plan_name())
                .unwrap_or("Legacy service plan");

            let resp = ListAvailableSubscriptionsResponse {
                subscription_plans: vec![SubscriptionPlan {
                    q_subscription_type: card
                        .as_ref()
                        .map(|c| c.plan_type())
                        .unwrap_or("CUSTOM")
                        .to_string(),
                    description: format!("{} - service card entitlement", plan),
                    pricing: SubscriptionPricing {
                        currency: "USD".to_string(),
                        amount: 0.0,
                    },
                }],
                disclaimer: Some(
                    "Our service only; not an official Kiro subscription.".to_string(),
                ),
            };
            json_response(StatusCode::OK, &resp)
        })
    }
}

/// Handler for `POST /CreateSubscriptionToken`
pub struct CreateSubscriptionTokenHandler;

impl FacadeHandler for CreateSubscriptionTokenHandler {
    fn method(&self) -> Method {
        Method::POST
    }

    fn path(&self) -> &'static str {
        "/CreateSubscriptionToken"
    }

    fn handle<'a>(&'a self, req: Request<Body>) -> BoxFuture<'a, Response> {
        Box::pin(async move {
            let portal = portal_url(req.headers());
            let mut token_bytes = [0u8; 24];
            if SystemRandom::new().fill(&mut token_bytes).is_err() {
                return super::error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "TokenGenerationException",
                    "secure random generator unavailable",
                );
            }
            let token = format!(
                "sub-{}",
                token_bytes
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>()
            );
            let expires_at =
                super::oauth::format_epoch_to_iso8601(crate::now_secs().saturating_add(900));
            let resp = CreateSubscriptionTokenResponse {
                subscription_token: token.clone(),
                expires_at,
                encoded_verification_url: portal,
                status: "ACTIVE".to_string(),
                token,
            };
            json_response(StatusCode::OK, &resp)
        })
    }
}
