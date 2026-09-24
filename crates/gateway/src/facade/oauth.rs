//! Authentication and token refresh handlers.
//!
//! Spec §4.1, §4.2, P0-4.
//! Supports both mock fallback (for backward compatibility) and full production
//! card validation, device binding, activation timing ("激活即计时"), brute-force protection,
//! and token issuance.

use super::{error_response, json_response, BoxFuture, FacadeHandler, Response};
use crate::auth::{AuthError, AuthState};
use crate::security::BruteForceProtector;
use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
};
use billing::card::CardStatus;
use billing::engine::BillingEngine;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthTokenResponse {
    pub access_token: String,
    pub refresh_token: String,
    pub profile_arn: String,
    pub expires_at: String,
    /// Seconds until `expires_at`. Kiro computes its own expiry as
    /// `now + expiresIn * 1000` and throws when the field is missing — after the
    /// refresh token has already been rotated, which then logs the user out.
    pub expires_in: u64,
    pub auth_method: String,
    pub provider: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RefreshTokenResponse {
    pub access_token: String,
    pub refresh_token: String,
    pub profile_arn: String,
    pub expires_at: String,
    /// Seconds until `expires_at`. Kiro computes its own expiry as
    /// `now + expiresIn * 1000` and throws when the field is missing — after the
    /// refresh token has already been rotated, which then logs the user out.
    pub expires_in: u64,
}

#[derive(Deserialize)]
struct LoginPayload {
    card_key: Option<String>,
    device_id: Option<String>,
    code: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RefreshPayload {
    refresh_token: Option<String>,
}

/// Handler for `POST /oauth/token`
#[derive(Clone, Default)]
pub struct OAuthTokenHandler {
    engine: Option<Arc<BillingEngine>>,
    auth_state: Option<AuthState>,
    protector: Option<Arc<BruteForceProtector>>,
    default_duration_secs: u64,
}

impl OAuthTokenHandler {
    pub fn mock() -> Self {
        Self::default()
    }

    pub fn new(engine: Arc<BillingEngine>, auth_state: AuthState) -> Self {
        Self {
            engine: Some(engine),
            auth_state: Some(auth_state),
            protector: None,
            default_duration_secs: 30 * 86_400, // 30 days default duration upon activation
        }
    }

    pub fn with_protector(mut self, protector: Arc<BruteForceProtector>) -> Self {
        self.protector = Some(protector);
        self
    }
}

impl FacadeHandler for OAuthTokenHandler {
    fn method(&self) -> Method {
        Method::POST
    }

    fn path(&self) -> &'static str {
        "/oauth/token"
    }

    fn handle<'a>(&'a self, req: Request<Body>) -> BoxFuture<'a, Response> {
        Box::pin(async move {
            let (parts, body) = req.into_parts();

            // Mock fallback if unconfigured
            if self.engine.is_none() || self.auth_state.is_none() {
                let resp = OAuthTokenResponse {
                    access_token: "kiro-byok-mock-jwt-token".to_string(),
                    refresh_token: "kiro-byok-mock-refresh-token".to_string(),
                    profile_arn:
                        "arn:aws:codewhisperer:us-east-1:123456789012:profile/KIRO_BYOK_DEFAULT"
                            .to_string(),
                    expires_at: "2030-01-01T00:00:00.000Z".to_string(),
                    expires_in: 3600,
                    auth_method: "social".to_string(),
                    provider: "Google".to_string(),
                };
                return json_response(StatusCode::OK, &resp);
            }

            let (engine, auth_state) = match (&self.engine, &self.auth_state) {
                (Some(e), Some(a)) => (e, a),
                _ => {
                    return error_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "InternalServerException",
                        "OAuth handler not properly configured with BillingEngine or AuthState",
                    );
                }
            };

            // Extract caller IP for brute force tracking
            let caller_ip = crate::security::client_ip_parts(&parts.extensions, &parts.headers);

            let now_secs = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);

            // 1. Check brute force lockout
            if let Some(ref protector) = self.protector {
                if let Err(crate::security::BruteForceError::LockedOut { remaining_secs }) =
                    protector.check_lockout(&caller_ip, now_secs)
                {
                    return error_response(
                        StatusCode::TOO_MANY_REQUESTS,
                        "LockoutException",
                        &format!(
                            "Account locked due to consecutive failures: {}s remaining",
                            remaining_secs
                        ),
                    );
                }
            }

            // 2. Read body bytes
            let body_bytes = match axum::body::to_bytes(body, 1024 * 1024).await {
                Ok(b) => b,
                Err(e) => {
                    return error_response(
                        StatusCode::BAD_REQUEST,
                        "SerializationException",
                        &format!("Failed to read request body: {}", e),
                    );
                }
            };

            if body_bytes.is_empty() {
                return error_response(
                    StatusCode::BAD_REQUEST,
                    "SerializationException",
                    "Login request body must not be empty",
                );
            }

            let payload: LoginPayload = match serde_json::from_slice(&body_bytes) {
                Ok(p) => p,
                Err(_) => {
                    return error_response(
                        StatusCode::BAD_REQUEST,
                        "SerializationException",
                        "Invalid JSON login payload",
                    );
                }
            };

            let card_code = payload.card_key.or(payload.code).unwrap_or_default();
            if card_code.is_empty() {
                if let Some(ref protector) = self.protector {
                    let _ = protector.record_failure(&caller_ip, now_secs);
                }
                return error_response(
                    StatusCode::UNAUTHORIZED,
                    "UnrecognizedClientException",
                    "Missing card key",
                );
            }

            // 3. Lookup card
            let card = match engine.find_card_by_secret(&card_code) {
                Some(c) => c,
                None => {
                    if let Some(ref protector) = self.protector {
                        let _ = protector.record_failure(&caller_ip, now_secs);
                    }
                    return error_response(
                        StatusCode::UNAUTHORIZED,
                        "UnrecognizedClientException",
                        "Invalid card key",
                    );
                }
            };

            // 4. Validate status before the atomic activation/bind transition.
            match card.status {
                CardStatus::Frozen => {
                    return error_response(
                        StatusCode::FORBIDDEN,
                        "AccessDeniedException",
                        "Card is frozen",
                    );
                }
                CardStatus::Banned => {
                    return error_response(
                        StatusCode::FORBIDDEN,
                        "AccessDeniedException",
                        "Card is banned",
                    );
                }
                CardStatus::Voided => {
                    return error_response(
                        StatusCode::FORBIDDEN,
                        "AccessDeniedException",
                        "Card is voided",
                    );
                }
                CardStatus::Expired => {
                    return error_response(
                        StatusCode::FORBIDDEN,
                        "ExpiredTokenException",
                        "Card validity period has expired",
                    );
                }
                CardStatus::Unactivated => {
                    // Activation is performed together with device binding below.
                }
                CardStatus::Active => {
                    if let Some(valid_until) = card.valid_until {
                        if now_secs >= valid_until {
                            return error_response(
                                StatusCode::FORBIDDEN,
                                "ExpiredTokenException",
                                "Card validity period has expired",
                            );
                        }
                    }
                }
            }

            // 5. Device binding is mandatory for a real login. A shared default
            // device would let unrelated clients consume the same card session.
            let device_id = match payload.device_id.as_deref().map(str::trim) {
                Some(device) if !device.is_empty() && device.len() <= 256 => device,
                _ => {
                    return error_response(
                        StatusCode::BAD_REQUEST,
                        "InvalidRequestException",
                        "A non-empty device_id of at most 256 characters is required",
                    );
                }
            };

            let updated_card = match engine.activate_card_with_device(
                &card.id,
                now_secs,
                self.default_duration_secs,
                Some(device_id),
            ) {
                Ok(updated) => updated,
                Err(err) => match err {
                    billing::BillingError::DeviceAlreadyBound => {
                        return error_response(
                            StatusCode::FORBIDDEN,
                            "DeviceBindingException",
                            "Card is already bound to another device; unbind it on the portal before logging in on a new device",
                        );
                    }
                    billing::BillingError::RebindCooldown { remaining_secs } => {
                        return error_response(
                            StatusCode::TOO_MANY_REQUESTS,
                            "ThrottlingException",
                            &format!(
                                "Device rebind cooldown active: {}s remaining",
                                remaining_secs
                            ),
                        );
                    }
                    billing::BillingError::RebindLimitExceeded { current, max } => {
                        return error_response(
                            StatusCode::FORBIDDEN,
                            "AccessDeniedException",
                            &format!("Device rebind limit reached ({}/{})", current, max),
                        );
                    }
                    other => {
                        return error_response(
                            StatusCode::BAD_REQUEST,
                            "DeviceBindingException",
                            &format!("Device binding failed: {:?}", other),
                        );
                    }
                },
            };

            // Record success to reset consecutive brute-force failures
            if let Some(ref protector) = self.protector {
                protector.record_success(&caller_ip);
            }

            // 6. Issue JWT token (TTL 1 hour)
            let ttl_secs = 3600;
            let access_token = match auth_state.issue_token(
                &updated_card.id,
                &updated_card.group_id,
                updated_card.token_version,
                ttl_secs,
            ) {
                Ok(tok) => tok,
                Err(e) => {
                    return error_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "TokenGenerationException",
                        &format!("Failed to generate JWT token: {:?}", e),
                    );
                }
            };

            let refresh_token = match auth_state.issue_refresh_token(
                &updated_card.id,
                &updated_card.group_id,
                updated_card.token_version,
                updated_card.refresh_version,
                30 * 86400, // 30 days for refresh token
            ) {
                Ok(tok) => tok,
                Err(e) => {
                    return error_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "TokenGenerationException",
                        &format!("Failed to generate refresh token: {:?}", e),
                    );
                }
            };

            let profile_arn = format!(
                "arn:aws:codewhisperer:us-east-1:123456789012:profile/{}",
                updated_card.group_id
            );
            let expires_at = format_epoch_to_iso8601(now_secs + ttl_secs);

            let resp = OAuthTokenResponse {
                access_token,
                refresh_token,
                profile_arn,
                expires_at,
                expires_in: ttl_secs,
                auth_method: "social".to_string(),
                provider: "Google".to_string(),
            };

            json_response(StatusCode::OK, &resp)
        })
    }
}

// Reuse the access-token contract: fixed messages, no card existence/status details.
fn refresh_auth_error(error: AuthError) -> Response {
    let (status, kind, message) = error.to_aws_error();
    error_response(status, kind, &message)
}

/// Lifetime of the access token issued by a refresh.
const REFRESH_ACCESS_TTL_SECS: u64 = 3600;

/// Handler for `POST /refreshToken`
#[derive(Clone, Default)]
pub struct RefreshTokenHandler {
    engine: Option<Arc<BillingEngine>>,
    auth_state: Option<AuthState>,
}

impl RefreshTokenHandler {
    pub fn mock() -> Self {
        Self::default()
    }

    pub fn new(engine: Arc<BillingEngine>, auth_state: AuthState) -> Self {
        Self {
            engine: Some(engine),
            auth_state: Some(auth_state),
        }
    }
}

impl FacadeHandler for RefreshTokenHandler {
    fn method(&self) -> Method {
        Method::POST
    }

    fn path(&self) -> &'static str {
        "/refreshToken"
    }

    fn handle<'a>(&'a self, req: Request<Body>) -> BoxFuture<'a, Response> {
        Box::pin(async move {
            if self.engine.is_none() || self.auth_state.is_none() {
                let resp = RefreshTokenResponse {
                    access_token: "kiro-byok-mock-refreshed-jwt".to_string(),
                    refresh_token: "kiro-byok-mock-refresh-token".to_string(),
                    profile_arn:
                        "arn:aws:codewhisperer:us-east-1:123456789012:profile/KIRO_BYOK_DEFAULT"
                            .to_string(),
                    expires_at: "2030-01-01T00:00:00.000Z".to_string(),
                    expires_in: 3600,
                };
                return json_response(StatusCode::OK, &resp);
            }

            let (engine, auth_state) = match (&self.engine, &self.auth_state) {
                (Some(e), Some(a)) => (e, a),
                _ => {
                    return error_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "InternalServerException",
                        "Refresh handler not properly configured with BillingEngine or AuthState",
                    );
                }
            };

            let body = req.into_body();
            let body_bytes = match axum::body::to_bytes(body, 64 * 1024).await {
                Ok(b) => b,
                Err(e) => {
                    return error_response(
                        StatusCode::BAD_REQUEST,
                        "SerializationException",
                        &format!("Failed to read request body: {}", e),
                    );
                }
            };

            let payload: RefreshPayload = match serde_json::from_slice(&body_bytes) {
                Ok(p) => p,
                Err(_) => {
                    return error_response(
                        StatusCode::BAD_REQUEST,
                        "SerializationException",
                        "Invalid refresh request payload",
                    );
                }
            };

            let raw_rt = payload.refresh_token.unwrap_or_default();
            let (claims, access_token, rotated_refresh_token) = match auth_state
                .rotate_refresh_token(&raw_rt, REFRESH_ACCESS_TTL_SECS, 30 * 86400)
            {
                Ok(pair) => pair,
                Err(error) => return refresh_auth_error(error),
            };
            let card_id = claims.card_id;
            let expected_version = claims.token_version;

            let card = match engine.get_card(&card_id) {
                Some(c) => c,
                None => return refresh_auth_error(AuthError::CardNotFound(card_id)),
            };

            // Retain the post-rotation check without disclosing live card state.
            let now_secs = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            if card.status != CardStatus::Active
                || card.valid_until.is_some_and(|until| now_secs >= until)
            {
                return refresh_auth_error(AuthError::CardInactive(card_id));
            }
            if card.token_version != expected_version {
                return refresh_auth_error(AuthError::TokenRevoked {
                    token_version: expected_version,
                    card_version: card.token_version,
                });
            }

            let profile_arn = format!(
                "arn:aws:codewhisperer:us-east-1:123456789012:profile/{}",
                card.group_id
            );
            let expires_at = format_epoch_to_iso8601(now_secs + REFRESH_ACCESS_TTL_SECS);

            let resp = RefreshTokenResponse {
                access_token,
                refresh_token: rotated_refresh_token,
                profile_arn,
                expires_at,
                expires_in: REFRESH_ACCESS_TTL_SECS,
            };

            json_response(StatusCode::OK, &resp)
        })
    }
}

/// Convert Unix timestamp seconds to ISO-8601 string (e.g. `2026-09-10T12:00:00Z`).
pub fn format_epoch_to_iso8601(epoch_secs: u64) -> String {
    let days = epoch_secs / 86400;
    let rem_secs = epoch_secs % 86400;
    let hours = rem_secs / 3600;
    let minutes = (rem_secs % 3600) / 60;
    let seconds = rem_secs % 60;

    let z = (days as i64) + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        y, m, d, hours, minutes, seconds
    )
}
