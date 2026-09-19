//! Card dispensing platform HTTP endpoints and Webhook receiver (Spec §14.1, P4-5).
//!
//! Provides standard HTTP endpoints for automated card shops:
//! - `GET /api/v1/cards/inventory`: Query current unactivated stock count.
//! - `POST /api/v1/cards/pull`: Idempotently dispense cards for an order.
//! - `POST /api/v1/cards/redeem`: Verify, activate, or top up cards via callback.

use super::{BoxFuture, FacadeHandler, Response};
use axum::{
    body::Body,
    http::{header, Method, Request, StatusCode},
    response::IntoResponse,
};
use billing::{CardPlatformManager, CardTemplate, PullCardsRequest, RedeemCallbackRequest};
use serde_json::json;
use std::time::{SystemTime, UNIX_EPOCH};

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Constant-time byte comparison to prevent timing side-channel on API key.
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    // ponytail: XOR-accumulate is sufficient for a shared-secret comparison;
    // upgrade to `subtle::ConstantTimeEq` if this crate already pulls it in.
    let diff = a
        .iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y));
    diff == 0
}

fn verify_card_platform_auth(headers: &axum::http::HeaderMap, expected_key: Option<&str>) -> bool {
    let Some(expected) = expected_key else {
        return false;
    };
    if expected.is_empty() {
        return false;
    }

    if let Some(val) = headers
        .get("x-card-platform-key")
        .and_then(|v| v.to_str().ok())
    {
        if ct_eq(val.as_bytes(), expected.as_bytes()) {
            return true;
        }
    }

    if let Some(auth_val) = headers.get("authorization").and_then(|v| v.to_str().ok()) {
        if let Some(token) = auth_val.strip_prefix("Bearer ") {
            if ct_eq(token.trim().as_bytes(), expected.as_bytes()) {
                return true;
            }
        }
    }

    false
}

/// Handler for `GET /api/v1/cards/inventory`.
pub struct CardInventoryHandler {
    pub billing: billing::BillingEngine,
    pub platform: CardPlatformManager,
    pub api_key: Option<String>,
}

impl FacadeHandler for CardInventoryHandler {
    fn method(&self) -> Method {
        Method::GET
    }

    fn path(&self) -> &'static str {
        "/api/v1/cards/inventory"
    }

    fn handle<'a>(&'a self, req: Request<Body>) -> BoxFuture<'a, Response> {
        Box::pin(async move {
            if !verify_card_platform_auth(req.headers(), self.api_key.as_deref()) {
                return (
                    StatusCode::UNAUTHORIZED,
                    [(header::CONTENT_TYPE, "application/json")],
                    json!({ "error": "Unauthorized: invalid or missing x-card-platform-key" })
                        .to_string(),
                )
                    .into_response();
            }

            let query_str = req.uri().query().unwrap_or_default();
            let mut template_id = None;
            let mut group_id = None;

            for pair in query_str.split('&') {
                let mut parts = pair.splitn(2, '=');
                match (parts.next(), parts.next()) {
                    (Some("template_id"), Some(v)) => template_id = Some(v),
                    (Some("group_id"), Some(v)) => group_id = Some(v),
                    _ => {}
                }
            }

            let resp = self
                .platform
                .query_inventory(&self.billing, template_id, group_id);
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "application/json")],
                axum::Json(resp),
            )
                .into_response()
        })
    }
}

/// Handler for `POST /api/v1/cards/pull`.
pub struct CardPullHandler {
    pub billing: billing::BillingEngine,
    pub platform: CardPlatformManager,
    pub default_template: CardTemplate,
    pub api_key: Option<String>,
}

impl FacadeHandler for CardPullHandler {
    fn method(&self) -> Method {
        Method::POST
    }

    fn path(&self) -> &'static str {
        "/api/v1/cards/pull"
    }

    fn handle<'a>(&'a self, req: Request<Body>) -> BoxFuture<'a, Response> {
        Box::pin(async move {
            if !verify_card_platform_auth(req.headers(), self.api_key.as_deref()) {
                return (
                    StatusCode::UNAUTHORIZED,
                    [(header::CONTENT_TYPE, "application/json")],
                    json!({ "error": "Unauthorized: invalid or missing x-card-platform-key" })
                        .to_string(),
                )
                    .into_response();
            }

            let body_bytes = match axum::body::to_bytes(req.into_body(), 64 * 1024).await {
                Ok(b) => b,
                Err(e) => {
                    return (
                        StatusCode::BAD_REQUEST,
                        [(header::CONTENT_TYPE, "application/json")],
                        json!({ "error": format!("Failed to read body: {e}") }).to_string(),
                    )
                        .into_response();
                }
            };

            let pull_req: PullCardsRequest = match serde_json::from_slice(&body_bytes) {
                Ok(r) => r,
                Err(e) => {
                    return (
                        StatusCode::BAD_REQUEST,
                        [(header::CONTENT_TYPE, "application/json")],
                        json!({ "error": format!("Invalid JSON request: {e}") }).to_string(),
                    )
                        .into_response();
                }
            };

            match self.platform.pull_cards(
                &self.billing,
                &self.default_template,
                &pull_req,
                now_secs(),
            ) {
                Ok(resp) => (
                    StatusCode::OK,
                    [(header::CONTENT_TYPE, "application/json")],
                    axum::Json(resp),
                )
                    .into_response(),
                Err(e) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    [(header::CONTENT_TYPE, "application/json")],
                    json!({ "error": e }).to_string(),
                )
                    .into_response(),
            }
        })
    }
}

/// Handler for `POST /api/v1/cards/redeem`.
pub struct CardRedeemHandler {
    pub billing: billing::BillingEngine,
    pub platform: CardPlatformManager,
    pub api_key: Option<String>,
}

impl FacadeHandler for CardRedeemHandler {
    fn method(&self) -> Method {
        Method::POST
    }

    fn path(&self) -> &'static str {
        "/api/v1/cards/redeem"
    }

    fn handle<'a>(&'a self, req: Request<Body>) -> BoxFuture<'a, Response> {
        Box::pin(async move {
            if !verify_card_platform_auth(req.headers(), self.api_key.as_deref()) {
                return (
                    StatusCode::UNAUTHORIZED,
                    [(header::CONTENT_TYPE, "application/json")],
                    json!({ "error": "Unauthorized: invalid or missing x-card-platform-key" })
                        .to_string(),
                )
                    .into_response();
            }

            let body_bytes = match axum::body::to_bytes(req.into_body(), 64 * 1024).await {
                Ok(b) => b,
                Err(e) => {
                    return (
                        StatusCode::BAD_REQUEST,
                        [(header::CONTENT_TYPE, "application/json")],
                        json!({ "error": format!("Failed to read body: {e}") }).to_string(),
                    )
                        .into_response();
                }
            };

            let redeem_req: RedeemCallbackRequest = match serde_json::from_slice(&body_bytes) {
                Ok(r) => r,
                Err(e) => {
                    return (
                        StatusCode::BAD_REQUEST,
                        [(header::CONTENT_TYPE, "application/json")],
                        json!({ "error": format!("Invalid JSON request: {e}") }).to_string(),
                    )
                        .into_response();
                }
            };

            match self
                .platform
                .process_redeem(&self.billing, &redeem_req, now_secs())
            {
                Ok(resp) => (
                    StatusCode::OK,
                    [(header::CONTENT_TYPE, "application/json")],
                    axum::Json(resp),
                )
                    .into_response(),
                Err(e) => (
                    StatusCode::BAD_REQUEST,
                    [(header::CONTENT_TYPE, "application/json")],
                    json!({ "error": e }).to_string(),
                )
                    .into_response(),
            }
        })
    }
}
