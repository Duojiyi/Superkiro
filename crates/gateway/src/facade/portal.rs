//! Web Self-Service Activation & Management Portal (Spec §14.2, P4-9).
//!
//! Provides both JSON APIs and an embedded, zero-dependency HTML/CSS/JS Single Page Application
//! for end-user self-service:
//! - Card status, balance, and expiration inquiry (`POST /api/v1/portal/query`)
//! - One-click card self-activation (`POST /api/v1/portal/activate`)
//! - Device unbind / rebind self-service (`POST /api/v1/portal/unbind`)
//! - Top-up card balance redemption (`POST /api/v1/portal/topup`)
//! - Embedded web portal page (`GET /portal`)

use super::virtualization::VirtualizationStore;
use super::{json_response, BoxFuture, FacadeHandler, Response};
use crate::security::{
    BruteForceError, BruteForceProtector, IpRateLimiter, PortalChallengeManager,
};
use axum::{
    body::Body,
    http::{header, Method, Request, StatusCode},
    response::IntoResponse,
};
use billing::card::CardStatus;
use billing::engine::BillingEngine;
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Extract a rate-limit identity from the connected peer.
/// Forwarded headers are opt-in because clients can forge them on direct access.
fn client_ip(req: &Request<Body>) -> String {
    crate::security::client_ip(req)
}

/// Build a 429 rate-limit response for portal endpoints.
fn rate_limit_response(retry_after: u64) -> Response {
    let mut response = (
        StatusCode::TOO_MANY_REQUESTS,
        [(header::CONTENT_TYPE, "application/json")],
        axum::Json(serde_json::json!({
            "success": false,
            "error": format!("Too many requests. Please retry after {retry_after} seconds."),
        })),
    )
        .into_response();
    if let Ok(value) = header::HeaderValue::from_str(&retry_after.max(1).to_string()) {
        response.headers_mut().insert(header::RETRY_AFTER, value);
    }
    response
}

/// Unified error for portal endpoints — intentionally vague to avoid enumeration (P1-01).
fn portal_deny_response() -> Response {
    (
        StatusCode::BAD_REQUEST,
        [(header::CONTENT_TYPE, "application/json")],
        axum::Json(serde_json::json!({
            "success": false,
            "error": "Invalid card or request parameters",
        })),
    )
        .into_response()
}

fn portal_deny_response_with_error(message: String) -> Response {
    (
        StatusCode::BAD_REQUEST,
        [(header::CONTENT_TYPE, "application/json")],
        axum::Json(serde_json::json!({
            "success": false,
            "error": message,
        })),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// Request & Response DTOs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortalQueryRequest {
    pub card: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortalQueryResponse {
    pub success: bool,
    pub card_id: String,
    pub status: String,
    pub remaining_credits: i64,
    pub remaining_points: f64,
    pub total_credits: i64,
    pub total_points: f64,
    pub activated_at: Option<u64>,
    pub valid_until: Option<u64>,
    pub is_expired: bool,
    pub bound_devices: Vec<String>,
    pub max_devices: u32,
    pub rebind_count: u32,
    pub max_rebinds: u32,
    pub group_id: String,
    pub group_name: String,
    pub virtual_plan_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortalActivateRequest {
    pub card: String,
    pub device: Option<String>,
    pub validity_days: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortalActivateResponse {
    pub success: bool,
    pub card_id: String,
    pub status: String,
    pub activated_at: u64,
    pub valid_until: Option<u64>,
    pub remaining_points: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortalChallengeRequest {
    pub action: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortalChallengeResponse {
    pub success: bool,
    pub challenge_token: String,
    pub expires_in: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortalUnbindRequest {
    pub card: String,
    pub device: String,
    #[serde(alias = "challengeToken")]
    pub challenge_token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortalUnbindResponse {
    pub success: bool,
    pub card_id: String,
    pub remaining_devices: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortalTopupRequest {
    pub card: String,
    pub topup_code: String,
    #[serde(alias = "challengeToken")]
    pub challenge_token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortalTopupResponse {
    pub success: bool,
    pub card_id: String,
    pub remaining_credits: i64,
    pub remaining_points: f64,
    pub added_credits: i64,
    pub added_points: f64,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

pub struct PortalQueryHandler {
    pub billing: BillingEngine,
    #[allow(dead_code)]
    pub store: Option<VirtualizationStore>,
    pub rate_limiter: IpRateLimiter,
}

impl FacadeHandler for PortalQueryHandler {
    fn method(&self) -> Method {
        Method::POST
    }

    fn path(&self) -> &'static str {
        "/api/v1/portal/query"
    }

    fn handle<'a>(&'a self, req: Request<Body>) -> BoxFuture<'a, Response> {
        Box::pin(async move {
            let ip = client_ip(&req);
            let now = now_secs();
            if let Err(retry_after) = self.rate_limiter.check_rate_limit(&ip, now) {
                return rate_limit_response(retry_after);
            }

            let bytes = match axum::body::to_bytes(req.into_body(), 64 * 1024).await {
                Ok(b) => b,
                Err(e) => return (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
            };

            let req_data: PortalQueryRequest = match serde_json::from_slice(&bytes) {
                Ok(d) => d,
                Err(_) => return portal_deny_response(),
            };
            if req_data.card.trim().is_empty() || req_data.card.chars().count() > 256 {
                return portal_deny_response();
            }

            let card = match self.billing.find_card_by_secret(&req_data.card) {
                Some(c) => c,
                None => return portal_deny_response(),
            };

            let now = now_secs();
            let is_expired = card.valid_until.map(|v| now >= v).unwrap_or(false);

            let group = self.billing.get_group(&card.group_id);
            let group_name = group
                .as_ref()
                .map(|g| g.name.clone())
                .unwrap_or_else(|| "Default Group".to_string());
            let virtual_plan_name = card
                .plan_name()
                .unwrap_or("Legacy service plan")
                .to_string();

            let resp = PortalQueryResponse {
                success: true,
                card_id: card.id.clone(),
                status: match card.status {
                    CardStatus::Active => if is_expired { "expired" } else { "active" }.to_string(),
                    CardStatus::Unactivated => "unactivated".to_string(),
                    CardStatus::Frozen => "frozen".to_string(),
                    CardStatus::Banned => "banned".to_string(),
                    CardStatus::Voided => "voided".to_string(),
                    CardStatus::Expired => "expired".to_string(),
                },
                remaining_credits: card.available_credits(),
                remaining_points: (card.available_credits() as f64)
                    / (billing::MICRO_CREDITS_PER_CREDIT as f64),
                total_credits: card.credit_total,
                total_points: (card.credit_total as f64)
                    / (billing::MICRO_CREDITS_PER_CREDIT as f64),
                activated_at: card.activated_at,
                valid_until: card.valid_until,
                is_expired,
                bound_devices: card.bound_devices.clone(),
                max_devices: 1,
                rebind_count: card.rebind_count,
                max_rebinds: card.max_rebinds,
                group_id: card.group_id.clone(),
                group_name,
                virtual_plan_name,
            };

            json_response(StatusCode::OK, &resp)
        })
    }
}

pub struct PortalActivateHandler {
    pub billing: BillingEngine,
    pub rate_limiter: IpRateLimiter,
}

impl FacadeHandler for PortalActivateHandler {
    fn method(&self) -> Method {
        Method::POST
    }

    fn path(&self) -> &'static str {
        "/api/v1/portal/activate"
    }

    fn handle<'a>(&'a self, req: Request<Body>) -> BoxFuture<'a, Response> {
        Box::pin(async move {
            let ip = client_ip(&req);
            let now = now_secs();
            if let Err(retry_after) = self.rate_limiter.check_rate_limit(&ip, now) {
                return rate_limit_response(retry_after);
            }

            let bytes = match axum::body::to_bytes(req.into_body(), 64 * 1024).await {
                Ok(b) => b,
                Err(e) => return (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
            };

            let req_data: PortalActivateRequest = match serde_json::from_slice(&bytes) {
                Ok(d) => d,
                Err(_) => return portal_deny_response(),
            };
            if req_data.card.trim().is_empty()
                || req_data.card.chars().count() > 256
                || req_data
                    .device
                    .as_deref()
                    .is_some_and(|d| d.chars().count() > 256)
            {
                return portal_deny_response();
            }

            let card = match self.billing.find_card_by_secret(&req_data.card) {
                Some(c) => c,
                None => return portal_deny_response(),
            };

            let now = now_secs();
            let validity_secs = match card.status {
                CardStatus::Unactivated => {
                    // Public callers cannot choose a duration. It must come from
                    // the issuing template; zero means perpetual only when the
                    // template explicitly says so.
                    if let Some(requested) = req_data.validity_days {
                        let template_days = card
                            .activation_duration_secs
                            .map(|seconds| seconds / 86_400);
                        if template_days != Some(requested) {
                            return portal_deny_response();
                        }
                    }
                    card.activation_duration_secs.unwrap_or(30 * 86_400)
                }
                CardStatus::Active => 0,
                _ => return portal_deny_response(),
            };

            if let Err(error) = card.check_device_policy() {
                return portal_deny_response_with_error(error.to_string());
            }

            let device = req_data
                .device
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty());
            let updated = if card.status == CardStatus::Unactivated {
                match self
                    .billing
                    .activate_card_with_device(&card.id, now, validity_secs, device)
                {
                    Ok(card) => card,
                    Err(error) => return portal_deny_response_with_error(error.to_string()),
                }
            } else if let Some(device) = device {
                if let Err(error) = self.billing.bind_device(&card.id, device, now) {
                    return portal_deny_response_with_error(error.to_string());
                }
                self.billing.get_card(&card.id).unwrap_or(card)
            } else {
                card
            };

            let resp = PortalActivateResponse {
                success: true,
                card_id: updated.id.clone(),
                status: "active".to_string(),
                activated_at: updated.activated_at.unwrap_or(now),
                valid_until: updated.valid_until,
                remaining_points: (updated.available_credits() as f64)
                    / (billing::MICRO_CREDITS_PER_CREDIT as f64),
            };

            json_response(StatusCode::OK, &resp)
        })
    }
}

pub struct PortalChallengeHandler {
    pub challenge_mgr: PortalChallengeManager,
    pub rate_limiter: IpRateLimiter,
}

impl FacadeHandler for PortalChallengeHandler {
    fn method(&self) -> Method {
        Method::POST
    }

    fn path(&self) -> &'static str {
        "/api/v1/portal/challenge"
    }

    fn handle<'a>(&'a self, req: Request<Body>) -> BoxFuture<'a, Response> {
        Box::pin(async move {
            let ip = client_ip(&req);
            let now = now_secs();
            if let Err(retry_after) = self.rate_limiter.check_rate_limit(&ip, now) {
                return rate_limit_response(retry_after);
            }

            let bytes = match axum::body::to_bytes(req.into_body(), 64 * 1024).await {
                Ok(b) => b,
                Err(e) => return (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
            };

            let req_data: PortalChallengeRequest = match serde_json::from_slice(&bytes) {
                Ok(d) => d,
                Err(_) => return portal_deny_response(),
            };
            if !matches!(req_data.action.as_str(), "unbind" | "topup") {
                return portal_deny_response();
            }

            match self
                .challenge_mgr
                .issue_challenge(&req_data.action, &ip, now)
            {
                Ok(token) => json_response(
                    StatusCode::OK,
                    &PortalChallengeResponse {
                        success: true,
                        challenge_token: token,
                        expires_in: 120,
                    },
                ),
                Err(_) => portal_deny_response(),
            }
        })
    }
}

pub struct PortalUnbindHandler {
    pub billing: BillingEngine,
    pub rate_limiter: IpRateLimiter,
    pub protector: BruteForceProtector,
    pub challenge_mgr: PortalChallengeManager,
}

impl FacadeHandler for PortalUnbindHandler {
    fn method(&self) -> Method {
        Method::POST
    }

    fn path(&self) -> &'static str {
        "/api/v1/portal/unbind"
    }

    fn handle<'a>(&'a self, req: Request<Body>) -> BoxFuture<'a, Response> {
        Box::pin(async move {
            let ip = client_ip(&req);
            let now = now_secs();
            // 1. IP rate limit check
            if let Err(retry_after) = self.rate_limiter.check_rate_limit(&ip, now) {
                return rate_limit_response(retry_after);
            }

            // 2. Anti-bruteforce consecutive failure lockout (P1-01)
            if let Err(BruteForceError::LockedOut { remaining_secs }) =
                self.protector.check_lockout(&ip, now)
            {
                return (
                    StatusCode::TOO_MANY_REQUESTS,
                    [(header::CONTENT_TYPE, "application/json")],
                    axum::Json(serde_json::json!({
                        "success": false,
                        "error": format!("Too many failed attempts. Locked out for {remaining_secs}s"),
                    })),
                )
                    .into_response();
            }

            let bytes = match axum::body::to_bytes(req.into_body(), 64 * 1024).await {
                Ok(b) => b,
                Err(e) => return (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
            };

            let req_data: PortalUnbindRequest = match serde_json::from_slice(&bytes) {
                Ok(d) => d,
                Err(_) => {
                    let _ = self.protector.record_failure(&ip, now);
                    return portal_deny_response();
                }
            };
            if req_data.card.chars().count() > 256
                || req_data.device.trim().is_empty()
                || req_data.device.chars().count() > 256
                || req_data.challenge_token.chars().count() > 4096
            {
                let _ = self.protector.record_failure(&ip, now);
                return portal_deny_response();
            }

            // 3. Challenge token verification is mandatory for sensitive actions.
            if self
                .challenge_mgr
                .verify_and_consume(&req_data.challenge_token, "unbind", &ip, now)
                .is_err()
            {
                let _ = self.protector.record_failure(&ip, now);
                return (
                    StatusCode::FORBIDDEN,
                    [(header::CONTENT_TYPE, "application/json")],
                    axum::Json(serde_json::json!({
                        "success": false,
                        "error": "Invalid, replayed or expired challenge token",
                    })),
                )
                    .into_response();
            }

            let card = match self.billing.find_card_by_secret(&req_data.card) {
                Some(c) => c,
                None => {
                    let _ = self.protector.record_failure(&ip, now);
                    return portal_deny_response();
                }
            };

            if let Err(e) = self.billing.unbind_device(&card.id, &req_data.device) {
                let _ = self.protector.record_failure(&ip, now);
                return (
                    StatusCode::BAD_REQUEST,
                    [(header::CONTENT_TYPE, "application/json")],
                    axum::Json(serde_json::json!({
                        "success": false,
                        "error": format!("Unbind error: {}", e),
                    })),
                )
                    .into_response();
            }

            // Success resets consecutive failure counter
            self.protector.record_success(&ip);

            let updated_card = match self.billing.get_card(&card.id) {
                Some(c) => c,
                None => {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        [(header::CONTENT_TYPE, "application/json")],
                        axum::Json(serde_json::json!({
                            "success": false,
                            "error": "Card record missing after unbind",
                        })),
                    )
                        .into_response();
                }
            };

            let resp = PortalUnbindResponse {
                success: true,
                card_id: updated_card.id,
                remaining_devices: updated_card.bound_devices,
            };

            json_response(StatusCode::OK, &resp)
        })
    }
}

pub struct PortalTopupHandler {
    pub billing: BillingEngine,
    pub rate_limiter: IpRateLimiter,
    pub protector: BruteForceProtector,
    pub challenge_mgr: PortalChallengeManager,
}

impl FacadeHandler for PortalTopupHandler {
    fn method(&self) -> Method {
        Method::POST
    }

    fn path(&self) -> &'static str {
        "/api/v1/portal/topup"
    }

    fn handle<'a>(&'a self, req: Request<Body>) -> BoxFuture<'a, Response> {
        Box::pin(async move {
            let ip = client_ip(&req);
            let now = now_secs();
            // 1. IP rate limit check
            if let Err(retry_after) = self.rate_limiter.check_rate_limit(&ip, now) {
                return rate_limit_response(retry_after);
            }

            // 2. Anti-bruteforce consecutive failure lockout (P1-01)
            if let Err(BruteForceError::LockedOut { remaining_secs }) =
                self.protector.check_lockout(&ip, now)
            {
                return (
                    StatusCode::TOO_MANY_REQUESTS,
                    [(header::CONTENT_TYPE, "application/json")],
                    axum::Json(serde_json::json!({
                        "success": false,
                        "error": format!("Too many failed attempts. Locked out for {remaining_secs}s"),
                    })),
                )
                    .into_response();
            }

            let bytes = match axum::body::to_bytes(req.into_body(), 64 * 1024).await {
                Ok(b) => b,
                Err(e) => return (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
            };

            let req_data: PortalTopupRequest = match serde_json::from_slice(&bytes) {
                Ok(d) => d,
                Err(_) => {
                    let _ = self.protector.record_failure(&ip, now);
                    return portal_deny_response();
                }
            };
            if req_data.card.chars().count() > 256
                || req_data.topup_code.trim().is_empty()
                || req_data.topup_code.chars().count() > 256
                || req_data.challenge_token.chars().count() > 4096
            {
                let _ = self.protector.record_failure(&ip, now);
                return portal_deny_response();
            }

            // 3. Challenge token verification is mandatory for sensitive actions.
            if self
                .challenge_mgr
                .verify_and_consume(&req_data.challenge_token, "topup", &ip, now)
                .is_err()
            {
                let _ = self.protector.record_failure(&ip, now);
                return (
                    StatusCode::FORBIDDEN,
                    [(header::CONTENT_TYPE, "application/json")],
                    axum::Json(serde_json::json!({
                        "success": false,
                        "error": "Invalid, replayed or expired challenge token",
                    })),
                )
                    .into_response();
            }

            let card = match self.billing.find_card_by_secret(&req_data.card) {
                Some(c) => c,
                None => {
                    let _ = self.protector.record_failure(&ip, now);
                    return portal_deny_response();
                }
            };

            let now = now_secs();
            let entry =
                match self
                    .billing
                    .redeem_topup(&card.id, &req_data.topup_code, now, "web-portal")
                {
                    Ok(e) => e,
                    Err(e) => {
                        let _ = self.protector.record_failure(&ip, now);
                        return (
                            StatusCode::BAD_REQUEST,
                            [(header::CONTENT_TYPE, "application/json")],
                            axum::Json(serde_json::json!({
                                "success": false,
                                "error": format!("Topup failed: {}", e),
                            })),
                        )
                            .into_response();
                    }
                };

            // Success resets consecutive failure counter
            self.protector.record_success(&ip);

            let updated_card = match self.billing.get_card(&card.id) {
                Some(c) => c,
                None => {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        [(header::CONTENT_TYPE, "application/json")],
                        axum::Json(serde_json::json!({
                            "success": false,
                            "error": "Card record missing after topup",
                        })),
                    )
                        .into_response();
                }
            };

            let resp = PortalTopupResponse {
                success: true,
                card_id: updated_card.id.clone(),
                remaining_credits: updated_card.available_credits(),
                remaining_points: (updated_card.available_credits() as f64)
                    / (billing::MICRO_CREDITS_PER_CREDIT as f64),
                added_credits: entry.credits_charged,
                added_points: (entry.credits_charged as f64)
                    / (billing::MICRO_CREDITS_PER_CREDIT as f64),
            };

            json_response(StatusCode::OK, &resp)
        })
    }
}

/// Embedded Single Page Application serving the Self-Service Web Portal (`GET /portal`).
pub struct PortalWebPageHandler;

impl FacadeHandler for PortalWebPageHandler {
    fn method(&self) -> Method {
        Method::GET
    }

    fn path(&self) -> &'static str {
        "/portal"
    }

    fn handle<'a>(&'a self, _req: Request<Body>) -> BoxFuture<'a, Response> {
        Box::pin(async move {
            let html = include_str!("../../../../apps/portal-ui/index.html");
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                html,
            )
                .into_response()
        })
    }
}
