//! Administrator REST Management APIs (Spec §7, P0-01, P0-02, P1-02).
//!
//! Exposes protected endpoints for the Admin UI console:
//! - `GET /api/v1/admin/me`: verify admin auth credentials
//! - `GET /api/v1/admin/stats`: system overview and financial metrics
//! - `GET /api/v1/admin/cards`: query cards with filtering
//! - `POST /api/v1/admin/cards/status`: freeze, unfreeze, ban, unban, void, archive, or unarchive cards (persisted)
//! - `GET /api/v1/admin/cards/history`: what happened to one card, who did it and why
//! - `POST /api/v1/admin/cards/adjust`: manual balance adjustment (persisted)
//! - `POST /api/v1/admin/cards/devices/unbind`, `.../rebinds/reset`, `.../validity`,
//!   `.../note`, `.../group`: support actions, each written to the card's history
//! - `POST /api/v1/admin/cards/batch`: batch generate cards from template
//! - `GET /api/v1/admin/announcements`: list announcements
//! - `POST /api/v1/admin/announcements`: publish announcement
//! - `GET /api/v1/admin/providers`: provider, model mapping, and rate card config
//! - `GET /api/v1/admin/traces`: request execution traces
//! - `GET /api/v1/admin/financials`: margin dashboard and model cost rankings

use super::{error_response, json_response, BoxFuture, FacadeHandler, Response};
use crate::security::ct_eq;
use axum::{
    body::Body,
    http::{header, Method, Request, StatusCode},
    response::IntoResponse,
};
use billing::card::{Card, CardStatus};
use billing::engine::BillingEngine;
use billing::observability::{Announcement, AnnouncementLevel};
use billing::CardTemplate;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn valid_text(value: &str, max: usize) -> bool {
    let trimmed = value.trim();
    !trimmed.is_empty() && trimmed.chars().count() <= max
}

/// The reason a support action is taken for, as the card's history keeps it: 1 to 200
/// bytes once trimmed.
fn support_reason(reason: Option<&str>) -> Option<&str> {
    reason
        .map(str::trim)
        .filter(|reason| !reason.is_empty() && reason.len() <= 200)
}

/// A refusal as the admin API answers one: nothing was changed.
fn failure(status: StatusCode, message: &str) -> Response {
    json_response(
        status,
        &serde_json::json!({ "success": false, "error": message }),
    )
}

fn parse_query(uri: &axum::http::Uri, key: &str) -> Option<String> {
    uri.query()?
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find_map(|(name, value)| (name == key).then(|| value.to_string()))
}

/// The period a report covers, from `fromSecs` up to but not including `toSecs`; an end
/// not given is open.
fn report_period(uri: &axum::http::Uri) -> Result<(Option<u64>, Option<u64>), &'static str> {
    let bound = |key: &str| {
        parse_query(uri, key)
            .map(|value| value.parse::<u64>())
            .transpose()
            .map_err(|_| "fromSecs and toSecs must be whole seconds")
    };
    let (from, to) = (bound("fromSecs")?, bound("toSecs")?);
    if from.zip(to).is_some_and(|(from, to)| from >= to) {
        return Err("fromSecs must be before toSecs");
    }
    Ok((from, to))
}

fn within_period(ts: u64, (from, to): (Option<u64>, Option<u64>)) -> bool {
    from.is_none_or(|from| ts >= from) && to.is_none_or(|to| ts < to)
}

/// A request carrying this header is the console refreshing itself: it keeps the session
/// alive no longer, so an unattended console still signs out when idle.
pub const BACKGROUND_HEADER: &str = "x-admin-background";

/// A live session: the token's own expiry, how long it may sit unused, and its last use.
#[derive(Debug, Clone, Copy)]
struct SessionEntry {
    exp: u64,
    idle_secs: u64,
    last_used: u64,
}

impl SessionEntry {
    /// When it ends if not used again: idle time after its last use, never past the token.
    fn deadline(&self) -> u64 {
        self.last_used.saturating_add(self.idle_secs).min(self.exp)
    }
}

/// Shared admin authorization state.
#[derive(Clone)]
pub struct AdminAuthState {
    pub admin_key: String,
    sessions: Arc<std::sync::RwLock<HashMap<String, SessionEntry>>>,
    session_epoch: Arc<AtomicU64>,
    legacy_key_allowed: bool,
    pub browser: Option<super::admin_login::BrowserAuth>,
}

impl std::fmt::Debug for AdminAuthState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdminAuthState")
            .field("admin_key", &"[redacted]")
            .field(
                "sessions",
                &self
                    .sessions
                    .read()
                    .map(|sessions| sessions.len())
                    .unwrap_or(0),
            )
            .field("session_epoch", &self.session_epoch)
            .field("legacy_key_allowed", &self.legacy_key_allowed)
            .finish()
    }
}

impl AdminAuthState {
    pub fn new(key: impl Into<String>) -> Self {
        Self::with_legacy_mode(key.into(), true)
    }

    /// Production constructor. Browser requests require a secure session cookie.
    /// Missing/invalid browser configuration fails closed in the middleware.
    pub fn new_production(key: impl Into<String>) -> Self {
        Self {
            browser: super::admin_login::BrowserAuth::from_env().ok(),
            ..Self::with_legacy_mode(key.into(), false)
        }
    }

    fn with_legacy_mode(key: String, legacy_key_allowed: bool) -> Self {
        Self {
            admin_key: key,
            sessions: Arc::new(std::sync::RwLock::new(HashMap::new())),
            session_epoch: Arc::new(AtomicU64::new(1)),
            legacy_key_allowed,
            browser: None,
        }
    }

    /// Constant-time verification of the bootstrap administrator key.
    pub fn verify_bootstrap(&self, headers: &axum::http::HeaderMap) -> bool {
        if self.admin_key.is_empty() {
            return false;
        }

        if let Some(val) = headers.get("x-admin-key").and_then(|v| v.to_str().ok()) {
            if ct_eq(val.as_bytes(), self.admin_key.as_bytes()) {
                return true;
            }
        }

        if let Some(auth_val) = headers.get("authorization").and_then(|v| v.to_str().ok()) {
            if let Some(token) = auth_val.strip_prefix("Bearer ") {
                if ct_eq(token.trim().as_bytes(), self.admin_key.as_bytes()) {
                    return true;
                }
            }
        }

        false
    }

    /// Verify a short-lived signed administrator session token.
    pub fn verify_session(&self, headers: &axum::http::HeaderMap) -> bool {
        self.verify_session_deadline(headers).is_some()
    }

    /// Verify a session token and count the request as a use of it, unless it is marked as
    /// background; returns when the session now ends if it is not used again.
    pub fn verify_session_deadline(&self, headers: &axum::http::HeaderMap) -> Option<u64> {
        let token = headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let token = token?;

        let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::HS256);
        validation.validate_exp = true;
        validation.set_issuer(&["kiro-byok-admin"]);
        let decoded = jsonwebtoken::decode::<AdminSessionClaims>(
            token,
            &jsonwebtoken::DecodingKey::from_secret(self.admin_key.as_bytes()),
            &validation,
        );
        let claims = decoded.ok()?.claims;
        if claims.sub != "admin" || claims.role != "admin" {
            return None;
        }
        if claims.epoch != self.session_epoch.load(Ordering::Acquire) {
            return None;
        }
        let now = now_secs();
        if claims.exp <= now {
            return None;
        }
        let mut sessions = self.sessions.write().ok()?;
        sessions.retain(|_, entry| entry.deadline() > now);
        let entry = sessions
            .get_mut(&claims.jti)
            .filter(|entry| entry.exp >= claims.exp)?;
        if !headers.contains_key(BACKGROUND_HEADER) {
            entry.last_used = now;
        }
        Some(entry.deadline())
    }

    /// Verify either a session or the legacy key. The latter exists only for
    /// compatibility with callers constructed through `new()`; production uses
    /// `new_production()` and therefore never accepts it on data endpoints.
    pub fn verify(&self, headers: &axum::http::HeaderMap) -> bool {
        self.verify_session(headers) || (self.legacy_key_allowed && self.verify_bootstrap(headers))
    }

    /// Both browser sessions and legacy credentials authenticate the sole admin account.
    pub fn authenticated_operator(&self, headers: &axum::http::HeaderMap) -> Option<&'static str> {
        self.verify(headers).then_some("admin")
    }

    /// A session that ends `ttl_secs` after it is issued, used or not.
    pub fn issue_session(&self, ttl_secs: u64) -> Result<AdminSessionResponse, String> {
        let ttl = ttl_secs.clamp(60, 3600);
        self.issue(ttl, ttl)
    }

    /// A session that ends after `idle_secs` without use, and `max_secs` after sign-in at the
    /// latest. The reply names the idle deadline; each use moves it on.
    pub fn issue_idle_session(
        &self,
        idle_secs: u64,
        max_secs: u64,
    ) -> Result<AdminSessionResponse, String> {
        let max = max_secs.clamp(1, 12 * 3600);
        self.issue(idle_secs.clamp(1, max), max)
    }

    fn issue(&self, idle_secs: u64, max_secs: u64) -> Result<AdminSessionResponse, String> {
        let now = now_secs();
        let exp = now.saturating_add(max_secs);
        // Random IDs prevent a pre-restart session matching a newly issued one.
        use ring::rand::SecureRandom;
        let mut random = [0u8; 32];
        ring::rand::SystemRandom::new()
            .fill(&mut random)
            .map_err(|_| "session randomness unavailable")?;
        let jti = random
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        let claims = AdminSessionClaims {
            sub: "admin".to_string(),
            role: "admin".to_string(),
            iss: "kiro-byok-admin".to_string(),
            jti: jti.clone(),
            iat: now,
            exp,
            epoch: self.session_epoch.load(Ordering::Acquire),
        };
        let token = jsonwebtoken::encode(
            &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::HS256),
            &claims,
            &jsonwebtoken::EncodingKey::from_secret(self.admin_key.as_bytes()),
        )
        .map_err(|error| error.to_string())?;
        let mut sessions = self
            .sessions
            .write()
            .map_err(|_| "admin session store poisoned".to_string())?;
        sessions.retain(|_, entry| entry.deadline() > now);
        if sessions.len() >= 256 {
            return Err("too many active admin sessions".to_string());
        }
        let entry = SessionEntry {
            exp,
            idle_secs,
            last_used: now,
        };
        sessions.insert(jti, entry);
        Ok(AdminSessionResponse {
            access_token: token,
            token_type: "Bearer".to_string(),
            expires_in: entry.deadline() - now,
            expires_at: entry.deadline(),
        })
    }

    /// Tests only: move every session's last use `secs` into the past.
    #[cfg(test)]
    pub(crate) fn backdate_session_use(&self, secs: u64) {
        for entry in self.sessions.write().unwrap().values_mut() {
            entry.last_used = entry.last_used.saturating_sub(secs);
        }
    }

    pub fn revoke_all_sessions(&self) {
        self.session_epoch.fetch_add(1, Ordering::AcqRel);
        if let Ok(mut sessions) = self.sessions.write() {
            sessions.clear();
        }
    }

    pub fn revoke_single_session(&self, headers: &axum::http::HeaderMap) -> bool {
        let auth_header = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok());
        let token = auth_header.and_then(|h| h.strip_prefix("Bearer "));
        let Some(token) = token else { return false };

        let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::HS256);
        validation.validate_exp = false;
        validation.set_issuer(&["kiro-byok-admin"]);
        if let Ok(data) = jsonwebtoken::decode::<AdminSessionClaims>(
            token,
            &jsonwebtoken::DecodingKey::from_secret(self.admin_key.as_bytes()),
            &validation,
        ) {
            if let Ok(mut sessions) = self.sessions.write() {
                return sessions.remove(&data.claims.jti).is_some();
            }
        }
        false
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AdminSessionClaims {
    sub: String,
    role: String,
    iss: String,
    jti: String,
    iat: u64,
    exp: u64,
    epoch: u64,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminSessionResponse {
    pub access_token: String,
    pub token_type: String,
    pub expires_in: u64,
    pub expires_at: u64,
}

/// Uniform middleware guard for every `/api/v1/admin/*` route.
///
/// Individual handlers keep their local checks as defense in depth, while this
/// boundary ensures newly added administrator handlers cannot be exposed by
/// forgetting to duplicate the check.
pub async fn admin_auth_middleware(
    axum::extract::State(auth): axum::extract::State<Arc<AdminAuthState>>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    if let Some(browser) = &auth.browser {
        return super::admin_login::handle(&auth, browser, req, next).await;
    }
    if !auth.legacy_key_allowed {
        return unauthorized_response();
    }
    let is_session_bootstrap =
        req.uri().path() == "/api/v1/admin/session" && req.method() == Method::POST;
    let authenticated = if is_session_bootstrap {
        auth.verify_bootstrap(req.headers())
    } else {
        auth.verify_session(req.headers())
            || (auth.legacy_key_allowed && auth.verify_bootstrap(req.headers()))
    };
    if authenticated && is_safe_admin_origin(&req) {
        next.run(req).await
    } else {
        unauthorized_response()
    }
}

fn is_safe_admin_origin(req: &axum::extract::Request) -> bool {
    if matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS) {
        return true;
    }
    if req
        .headers()
        .get("sec-fetch-site")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("cross-site"))
    {
        return false;
    }
    let Some(origin) = req
        .headers()
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
    else {
        return true;
    };
    let Some(host) = req
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
    else {
        return false;
    };
    origin
        .strip_prefix("https://")
        .or_else(|| origin.strip_prefix("http://"))
        .is_some_and(|authority| authority.trim_end_matches('/') == host)
}

pub struct AdminSessionHandler {
    pub auth: Arc<AdminAuthState>,
}

impl FacadeHandler for AdminSessionHandler {
    fn method(&self) -> Method {
        Method::POST
    }

    fn path(&self) -> &'static str {
        "/api/v1/admin/session"
    }

    fn handle<'a>(&'a self, req: Request<Body>) -> BoxFuture<'a, Response> {
        Box::pin(async move {
            if !self.auth.verify_bootstrap(req.headers()) {
                return unauthorized_response();
            }
            match self.auth.issue_session(900) {
                Ok(session) => json_response(StatusCode::OK, &session),
                Err(error) => (
                    StatusCode::TOO_MANY_REQUESTS,
                    [(header::CONTENT_TYPE, "application/json")],
                    axum::Json(serde_json::json!({ "success": false, "error": error })),
                )
                    .into_response(),
            }
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AdminRevokeSessionRequest {
    pub all: Option<bool>,
}

pub struct AdminRevokeSessionsHandler {
    pub auth: Arc<AdminAuthState>,
}

impl FacadeHandler for AdminRevokeSessionsHandler {
    fn method(&self) -> Method {
        Method::POST
    }

    fn path(&self) -> &'static str {
        "/api/v1/admin/session/revoke"
    }

    fn handle<'a>(&'a self, req: Request<Body>) -> BoxFuture<'a, Response> {
        Box::pin(async move {
            let headers = req.headers().clone();
            if !self.auth.verify(&headers) {
                return unauthorized_response();
            }
            let bytes = match axum::body::to_bytes(req.into_body(), 16 * 1024).await {
                Ok(b) => b,
                Err(e) => return (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
            };
            let req_data: AdminRevokeSessionRequest =
                serde_json::from_slice(&bytes).unwrap_or_default();
            let is_all = req_data.all.unwrap_or(false);
            if is_all {
                self.auth.revoke_all_sessions();
            } else {
                self.auth.revoke_single_session(&headers);
            }
            json_response(
                StatusCode::OK,
                &serde_json::json!({ "success": true, "all": is_all }),
            )
        })
    }
}

fn unauthorized_response() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(header::CONTENT_TYPE, "application/json")],
        axum::Json(serde_json::json!({
            "success": false,
            "error": "Unauthorized: administrator login required",
        })),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// 1. Admin Me Handler (GET /api/v1/admin/me)
// ---------------------------------------------------------------------------

pub struct AdminMeHandler {
    pub auth: Arc<AdminAuthState>,
}

impl FacadeHandler for AdminMeHandler {
    fn method(&self) -> Method {
        Method::GET
    }

    fn path(&self) -> &'static str {
        "/api/v1/admin/me"
    }

    fn handle<'a>(&'a self, req: Request<Body>) -> BoxFuture<'a, Response> {
        Box::pin(async move {
            if !self.auth.verify(req.headers()) {
                return unauthorized_response();
            }

            json_response(
                StatusCode::OK,
                &serde_json::json!({
                    "success": true,
                    "role": "admin",
                    "authenticated": true,
                    "serverTime": now_secs(),
                    "csrfToken": super::admin_login::csrf_token(req.headers()),
                }),
            )
        })
    }
}

// ---------------------------------------------------------------------------
// 2. Admin Stats Overview Handler (GET /api/v1/admin/stats)
// ---------------------------------------------------------------------------

pub struct AdminStatsHandler {
    pub billing: BillingEngine,
    pub auth: Arc<AdminAuthState>,
}

/// Why the saved state cannot be written, in words the operator can act on. The error
/// itself names server paths and sizes, which are not shown.
fn persistence_problem(error: Option<&str>) -> &'static str {
    let error = error.unwrap_or_default().to_ascii_lowercase();
    let names = |needles: &[&str]| needles.iter().any(|needle| error.contains(needle));
    if names(&["exceeds maximum allowable limit"]) {
        "The saved state has reached its size ceiling; archive old ledger entries"
    } else if names(&[
        "os error 28",
        "os error 112",
        "no space left",
        "not enough space",
    ]) {
        "The disk holding the saved state is full"
    } else if names(&[
        "os error 5)",
        "os error 13",
        "permission denied",
        "access is denied",
    ]) {
        "The saved state cannot be written: permission denied"
    } else if names(&["master kek"]) {
        "Saving this state needs the master key, which is not configured"
    } else {
        "The saved state could not be written"
    }
}

pub struct AdminFinancialsHandler {
    pub billing: BillingEngine,
    pub auth: Arc<AdminAuthState>,
}

pub struct AdminProvidersHandler {
    pub billing: BillingEngine,
    pub auth: Arc<AdminAuthState>,
    pub runtime: Option<crate::provider::ProviderRuntimeRegistry>,
}

impl FacadeHandler for AdminProvidersHandler {
    fn method(&self) -> Method {
        Method::GET
    }
    fn path(&self) -> &'static str {
        "/api/v1/admin/providers"
    }
    fn handle<'a>(&'a self, req: Request<Body>) -> BoxFuture<'a, Response> {
        Box::pin(async move {
            if !self.auth.verify(req.headers()) {
                return unauthorized_response();
            }
            let providers = self.billing.list_providers();
            // A key's health is the running gateway's: cooldowns and retirements after an
            // invalid-key reply live in memory, never in the saved key.
            let now = now_secs();
            let live = self
                .runtime
                .as_ref()
                .map(|runtime| runtime.key_health(now))
                .unwrap_or_default();
            let keys: Vec<serde_json::Value> =
                self.billing
                    .list_provider_keys(None)
                    .into_iter()
                    .map(|key| {
                        let health = live.get(&key.id).cloned().unwrap_or_else(|| {
                            crate::provider::governance::KeyHealth::of(&key, now)
                        });
                        let mut value = serde_json::to_value(&key).unwrap_or_default();
                        if let (Some(fields), Ok(serde_json::Value::Object(health))) =
                            (value.as_object_mut(), serde_json::to_value(health))
                        {
                            fields.extend(health);
                        }
                        value
                    })
                    .collect();
            json_response(
                StatusCode::OK,
                &serde_json::json!({ "success": true, "providers": providers, "keys": keys }),
            )
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminProviderStatusRequest {
    pub provider_id: String,
    pub enabled: bool,
}

pub struct AdminProviderStatusHandler {
    pub billing: BillingEngine,
    pub auth: Arc<AdminAuthState>,
    pub runtime: Option<crate::provider::ProviderRuntimeRegistry>,
}

impl FacadeHandler for AdminProviderStatusHandler {
    fn method(&self) -> Method {
        Method::POST
    }
    fn path(&self) -> &'static str {
        "/api/v1/admin/providers/status"
    }
    fn handle<'a>(&'a self, req: Request<Body>) -> BoxFuture<'a, Response> {
        Box::pin(async move {
            if !self.auth.verify(req.headers()) {
                return unauthorized_response();
            }
            let bytes = match axum::body::to_bytes(req.into_body(), 64 * 1024).await {
                Ok(bytes) => bytes,
                Err(error) => {
                    return error_response(
                        StatusCode::BAD_REQUEST,
                        "SerializationException",
                        &error.to_string(),
                    )
                }
            };
            let body: AdminProviderStatusRequest = match serde_json::from_slice(&bytes) {
                Ok(body) => body,
                Err(error) => {
                    return error_response(
                        StatusCode::BAD_REQUEST,
                        "SerializationException",
                        &error.to_string(),
                    )
                }
            };
            if !valid_text(&body.provider_id, 128) {
                return error_response(
                    StatusCode::BAD_REQUEST,
                    "InvalidRequestException",
                    "provider_id is invalid",
                );
            }
            match self
                .billing
                .set_provider_enabled(body.provider_id.trim(), body.enabled)
            {
                Ok(true) => {}
                Ok(false) => {
                    return error_response(
                        StatusCode::NOT_FOUND,
                        "ResourceNotFoundException",
                        "provider not found",
                    );
                }
                Err(_error) => {
                    return error_response(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "PersistenceException",
                        "provider status could not be durably persisted; please retry",
                    );
                }
            }
            if let Some(ref runtime) = self.runtime {
                runtime.sync_from_billing(&self.billing);
            }
            json_response(
                StatusCode::OK,
                &serde_json::json!({ "success": true, "providerId": body.provider_id, "enabled": body.enabled }),
            )
        })
    }
}

impl FacadeHandler for AdminFinancialsHandler {
    fn method(&self) -> Method {
        Method::GET
    }
    fn path(&self) -> &'static str {
        "/api/v1/admin/financials"
    }
    fn handle<'a>(&'a self, req: Request<Body>) -> BoxFuture<'a, Response> {
        Box::pin(async move {
            if !self.auth.verify(req.headers()) {
                return unauthorized_response();
            }
            let period = match report_period(req.uri()) {
                Ok(period) => period,
                Err(message) => return failure(StatusCode::BAD_REQUEST, message),
            };
            // One snapshot keeps estimates, coverage and their settings consistent.
            let mut snapshot = self.billing.export_snapshot();
            snapshot
                .ledger
                .retain(|entry| within_period(entry.ts_secs, period));
            let dashboard = billing::observability::compute_margin_dashboard(
                &snapshot.ledger,
                &snapshot.settings,
            );
            let rankings = billing::observability::compute_model_cost_rankings(
                &snapshot.ledger,
                &snapshot.settings,
            );
            let costed_requests = snapshot
                .ledger
                .iter()
                .filter(|entry| {
                    entry.kind == billing::ledger::LedgerKind::Usage
                        && entry.rate_card_version.is_some()
                })
                .count() as u64;
            let uncosted_requests = dashboard.total_requests.saturating_sub(costed_requests);
            let (from_secs, to_secs) = period;
            json_response(
                StatusCode::OK,
                &serde_json::json!({
                    "success": true,
                    "fromSecs": from_secs,
                    "toSecs": to_secs,
                    // What each upstream should bill for the period.
                    "byProvider": billing::observability::compute_provider_costs(&snapshot.ledger),
                    "margin": billing::observability::compute_costed_margin(&snapshot.ledger, &snapshot.settings),
                    "sales": billing::observability::compute_sales(snapshot.cards.values(), from_secs, to_secs),
                    // Balances still owed, now, whatever the period.
                    "liability": billing::observability::compute_liability(snapshot.cards.values(), &snapshot.settings, now_secs()),
                    "planPrices": billing::template::PLAN_PRICES,
                    "dashboard": dashboard,
                    "modelRankings": rankings,
                    "basis": "retained_usage_ledger_estimate_not_cash_revenue",
                    "settings": snapshot.settings,
                    "actualRevenueMicroCny": null,
                    "actualGrossProfitMicroCny": null,
                    "estimates": {
                        "usageFaceValueMicroCny": dashboard.revenue_micro_cny,
                        "configuredProviderCostMicroCny": dashboard.provider_cost_micro_cny,
                        "faceValueLessCostMicroCny": if uncosted_requests == 0 { Some(dashboard.gross_profit_micro_cny) } else { None },
                        "faceValueMarginPercentage": if uncosted_requests == 0 && dashboard.revenue_micro_cny > 0 { Some(dashboard.gross_margin_percentage) } else { None },
                        "costedRequests": costed_requests,
                        "uncostedRequests": uncosted_requests,
                        "retainedLedgerOnly": true,
                    },
                }),
            )
        })
    }
}

pub struct AdminTracesHandler {
    pub billing: BillingEngine,
    pub auth: Arc<AdminAuthState>,
}

impl FacadeHandler for AdminTracesHandler {
    fn method(&self) -> Method {
        Method::GET
    }
    fn path(&self) -> &'static str {
        "/api/v1/admin/traces"
    }
    fn handle<'a>(&'a self, req: Request<Body>) -> BoxFuture<'a, Response> {
        Box::pin(async move {
            if !self.auth.verify(req.headers()) {
                return unauthorized_response();
            }
            let (from_secs, to_secs) = match report_period(req.uri()) {
                Ok(period) => period,
                Err(message) => return failure(StatusCode::BAD_REQUEST, message),
            };
            let text = |key: &str| {
                parse_query(req.uri(), key)
                    .and_then(|value| crate::archive::percent_decode(&value))
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty())
            };
            let status = match text("status") {
                None => None,
                Some(status) => match serde_json::from_value(serde_json::json!(status)) {
                    Ok(status) => Some(status),
                    Err(_) => {
                        return failure(
                            StatusCode::BAD_REQUEST,
                            "status must be success, error, client_aborted or in_progress",
                        )
                    }
                },
            };
            let filter = billing::observability::TraceFilter {
                from_secs,
                to_secs,
                card_id: text("cardId").or_else(|| text("card_id")),
                model: text("model"),
                provider: text("provider"),
                status,
            };
            let limit = parse_query(req.uri(), "limit")
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(100)
                .clamp(1, 500);
            let (traces, totals) = self.billing.search_traces(&filter, limit);
            json_response(
                StatusCode::OK,
                &serde_json::json!({
                    "success": true,
                    "count": traces.len(),
                    "traces": traces,
                    // Over every retained trace that matched, not only those returned.
                    "totals": totals,
                }),
            )
        })
    }
}

/// One request's content and the upstream model's reply, kept for 24 hours for tracing.
/// Each read is logged, naming the operator and the request but none of its content.
pub struct AdminTraceContentHandler {
    pub auth: Arc<AdminAuthState>,
}

impl FacadeHandler for AdminTraceContentHandler {
    fn method(&self) -> Method {
        Method::GET
    }
    fn path(&self) -> &'static str {
        "/api/v1/admin/traces/content"
    }
    fn handle<'a>(&'a self, req: Request<Body>) -> BoxFuture<'a, Response> {
        Box::pin(async move {
            if !self.auth.verify(req.headers()) {
                return unauthorized_response();
            }
            let Some(invocation_id) = parse_query(req.uri(), "invocation_id")
                .and_then(|value| crate::archive::percent_decode(&value))
                .filter(|value| !value.is_empty() && value.len() <= 512)
            else {
                return error_response(
                    StatusCode::BAD_REQUEST,
                    "ValidationException",
                    "invocation_id is required",
                );
            };
            let Some(archive) = crate::archive::active() else {
                return error_response(
                    StatusCode::NOT_FOUND,
                    "ResourceNotFoundException",
                    "请求内容未开启保存",
                );
            };
            let operator = self
                .auth
                .authenticated_operator(req.headers())
                .unwrap_or("admin");
            let lookup = invocation_id.clone();
            let record = tokio::task::spawn_blocking(move || archive.read(&lookup, now_secs()))
                .await
                .ok()
                .flatten();
            eprintln!(
                "[admin] request content viewed operator={operator} request={} found={}",
                crate::archive::log_key(&invocation_id),
                record.is_some()
            );
            match record {
                Some(mut record) => {
                    record["success"] = serde_json::json!(true);
                    json_response(StatusCode::OK, &record)
                }
                None => error_response(
                    StatusCode::NOT_FOUND,
                    "ResourceNotFoundException",
                    "没有这次请求的内容（只保留 24 小时）",
                ),
            }
        })
    }
}

pub struct AdminLedgerExportHandler {
    pub billing: BillingEngine,
    pub auth: Arc<AdminAuthState>,
    pub json: bool,
}

impl FacadeHandler for AdminLedgerExportHandler {
    fn method(&self) -> Method {
        Method::GET
    }
    fn path(&self) -> &'static str {
        if self.json {
            "/api/v1/admin/exports/ledger.json"
        } else {
            "/api/v1/admin/exports/ledger.csv"
        }
    }
    fn handle<'a>(&'a self, req: Request<Body>) -> BoxFuture<'a, Response> {
        Box::pin(async move {
            if !self.auth.verify(req.headers()) {
                return unauthorized_response();
            }
            let card_id = parse_query(req.uri(), "card_id");
            if self.json {
                match self.billing.export_ledger_json(card_id.as_deref()) {
                    Ok(body) => (
                        StatusCode::OK,
                        [
                            (header::CONTENT_TYPE, "application/json"),
                            (
                                header::CONTENT_DISPOSITION,
                                "attachment; filename=ledger.json",
                            ),
                        ],
                        body,
                    )
                        .into_response(),
                    Err(error) => error_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "SerializationException",
                        &error.to_string(),
                    ),
                }
            } else {
                (
                    StatusCode::OK,
                    [
                        (header::CONTENT_TYPE, "text/csv; charset=utf-8"),
                        (
                            header::CONTENT_DISPOSITION,
                            "attachment; filename=ledger.csv",
                        ),
                    ],
                    self.billing.export_ledger_csv(card_id.as_deref()),
                )
                    .into_response()
            }
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminPruneTracesRequest {
    pub cutoff_secs: Option<u64>,
}

pub struct AdminPruneTracesHandler {
    pub billing: BillingEngine,
    pub auth: Arc<AdminAuthState>,
}

impl FacadeHandler for AdminPruneTracesHandler {
    fn method(&self) -> Method {
        Method::POST
    }
    fn path(&self) -> &'static str {
        "/api/v1/admin/traces/prune"
    }
    fn handle<'a>(&'a self, req: Request<Body>) -> BoxFuture<'a, Response> {
        Box::pin(async move {
            if !self.auth.verify(req.headers()) {
                return unauthorized_response();
            }
            let bytes = match axum::body::to_bytes(req.into_body(), 64 * 1024).await {
                Ok(bytes) => bytes,
                Err(error) => {
                    return error_response(
                        StatusCode::BAD_REQUEST,
                        "SerializationException",
                        &error.to_string(),
                    )
                }
            };
            let body: AdminPruneTracesRequest = if bytes.is_empty() {
                AdminPruneTracesRequest { cutoff_secs: None }
            } else {
                match serde_json::from_slice(&bytes) {
                    Ok(body) => body,
                    Err(error) => {
                        return error_response(
                            StatusCode::BAD_REQUEST,
                            "SerializationException",
                            &error.to_string(),
                        )
                    }
                }
            };
            let cutoff = body
                .cutoff_secs
                .unwrap_or_else(|| now_secs().saturating_sub(30 * 86_400));
            let pruned = self.billing.prune_traces(cutoff);
            json_response(
                StatusCode::OK,
                &serde_json::json!({ "success": true, "pruned": pruned, "cutoffSecs": cutoff }),
            )
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminArchiveLedgerRequest {
    /// Ledger entries older than this move out of the saved state into an archive file.
    pub before_ts_secs: u64,
}

/// The way to shrink the saved state before it reaches its ceiling: moves old ledger
/// entries into an archive file beside it (encrypted with the master KEK when one is
/// configured), keeping per-card totals so balances and quotas are unchanged.
pub struct AdminArchiveLedgerHandler {
    pub billing: BillingEngine,
    pub auth: Arc<AdminAuthState>,
}

impl FacadeHandler for AdminArchiveLedgerHandler {
    fn method(&self) -> Method {
        Method::POST
    }
    fn path(&self) -> &'static str {
        "/api/v1/admin/ledger/archive"
    }
    fn handle<'a>(&'a self, req: Request<Body>) -> BoxFuture<'a, Response> {
        Box::pin(async move {
            if !self.auth.verify(req.headers()) {
                return unauthorized_response();
            }
            let body: AdminArchiveLedgerRequest =
                match axum::body::to_bytes(req.into_body(), 64 * 1024)
                    .await
                    .map_err(|error| error.to_string())
                    .and_then(|bytes| {
                        serde_json::from_slice(&bytes).map_err(|error| error.to_string())
                    }) {
                    Ok(body) => body,
                    Err(error) => {
                        return error_response(
                            StatusCode::BAD_REQUEST,
                            "SerializationException",
                            &error,
                        )
                    }
                };
            if body.before_ts_secs > now_secs() {
                return error_response(
                    StatusCode::BAD_REQUEST,
                    "ValidationException",
                    "beforeTsSecs must not be in the future",
                );
            }
            let Some(dir) = self.billing.ledger_archive_dir() else {
                return error_response(
                    StatusCode::CONFLICT,
                    "InvalidStateException",
                    "Billing state is not persisted, so there is nothing to archive",
                );
            };
            let (before_bytes, ceiling) = self.billing.state_size();
            match self.billing.archive_ledger(body.before_ts_secs, &dir) {
                Ok(receipt) => json_response(
                    StatusCode::OK,
                    &serde_json::json!({
                        "success": true,
                        // What moved, and where to: the console shows it as a receipt.
                        "movedEntries": receipt.drained_entries_count,
                        "archiveFile": receipt.archive_file,
                        "receipt": receipt,
                        "stateBytesBefore": before_bytes,
                        "stateBytesAfter": self.billing.state_size().0,
                        "stateCeilingBytes": ceiling,
                    }),
                ),
                Err(billing::BillingError::InvalidAdjustment(message)) => {
                    error_response(StatusCode::BAD_REQUEST, "ValidationException", &message)
                }
                Err(error) => error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "PersistenceException",
                    &error.to_string(),
                ),
            }
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminBatchCardsRequest {
    #[serde(alias = "credit_total")]
    pub credit_total: Option<i64>,
    #[serde(alias = "max_devices")]
    pub max_devices: Option<u32>,
    pub count: usize,
    pub template_id: Option<String>,
    pub group_id: Option<String>,
    pub note: Option<String>,
}

pub struct AdminBatchCardsHandler {
    pub billing: BillingEngine,
    pub auth: Arc<AdminAuthState>,
}

impl FacadeHandler for AdminBatchCardsHandler {
    fn method(&self) -> Method {
        Method::POST
    }
    fn path(&self) -> &'static str {
        "/api/v1/admin/cards/batch"
    }
    fn handle<'a>(&'a self, req: Request<Body>) -> BoxFuture<'a, Response> {
        Box::pin(async move {
            if !self.auth.verify(req.headers()) {
                return unauthorized_response();
            }
            let bytes = match axum::body::to_bytes(req.into_body(), 64 * 1024).await {
                Ok(bytes) => bytes,
                Err(error) => {
                    return error_response(
                        StatusCode::BAD_REQUEST,
                        "SerializationException",
                        &error.to_string(),
                    )
                }
            };
            let body: AdminBatchCardsRequest = match serde_json::from_slice(&bytes) {
                Ok(body) => body,
                Err(error) => {
                    return error_response(
                        StatusCode::BAD_REQUEST,
                        "SerializationException",
                        &error.to_string(),
                    )
                }
            };
            if body.count == 0 || body.count > 1000 {
                return error_response(
                    StatusCode::BAD_REQUEST,
                    "InvalidRequestException",
                    "count must be between 1 and 1000",
                );
            }
            let Some(group_id) = body.group_id else {
                return error_response(
                    StatusCode::BAD_REQUEST,
                    "InvalidRequestException",
                    "Select an issuance group explicitly",
                );
            };
            if !valid_text(&group_id, 64) {
                return error_response(
                    StatusCode::BAD_REQUEST,
                    "InvalidRequestException",
                    "invalid group_id",
                );
            }
            let template_id = body.template_id.as_deref().unwrap_or("standard-monthly");
            let Some(template) = CardTemplate::tier(template_id, &group_id) else {
                return error_response(
                    StatusCode::BAD_REQUEST,
                    "InvalidRequestException",
                    "unknown tier template",
                );
            };
            if self
                .billing
                .get_group(&group_id)
                .is_none_or(|group| !group.issuance_enabled)
                || body.max_devices.is_some_and(|n| n != 1)
                || body
                    .credit_total
                    .is_some_and(|n| n != template.credit_total)
            {
                return error_response(
                    StatusCode::BAD_REQUEST,
                    "InvalidRequestException",
                    "issuance requires an enabled group, matching tier credits, and maxDevices=1",
                );
            }
            let now = now_secs();
            let generated =
                match self
                    .billing
                    .issue_cards(&template, body.count, body.note.as_deref(), now)
                {
                    Ok(cards) => cards,
                    Err(billing::BillingError::GroupIssuanceDisabled) => {
                        return error_response(
                            StatusCode::CONFLICT,
                            "IssuanceDisabledException",
                            "Group no longer allows new card issuance; refresh the configuration",
                        )
                    }
                    Err(error) => {
                        return error_response(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            "GenerationException",
                            &error.to_string(),
                        )
                    }
                };
            let response_cards: Vec<_> = generated
                .iter()
                .map(|generated| {
                    serde_json::json!({
                        "cardId": generated.card.id,
                        "rawCode": generated.raw_code,
                        "groupId": generated.card.group_id,
                        "creditTotal": generated.card.credit_total,
                        "maxDevices": 1,
                        "virtualPlanName": generated.card.plan_name(),
                        "status": generated.card.status,
                    })
                })
                .collect();
            eprintln!(
                "{}",
                serde_json::json!({"event": "admin_cards_issued", "count": response_cards.len()})
            );
            let mut response = json_response(
                StatusCode::OK,
                &serde_json::json!({ "success": true, "count": response_cards.len(), "cards": response_cards }),
            );
            response.headers_mut().insert(
                axum::http::header::CACHE_CONTROL,
                axum::http::HeaderValue::from_static("no-store"),
            );
            response
        })
    }
}

impl FacadeHandler for AdminStatsHandler {
    fn method(&self) -> Method {
        Method::GET
    }

    fn path(&self) -> &'static str {
        "/api/v1/admin/stats"
    }

    fn handle<'a>(&'a self, req: Request<Body>) -> BoxFuture<'a, Response> {
        Box::pin(async move {
            if !self.auth.verify(req.headers()) {
                return unauthorized_response();
            }

            let cards = self.billing.list_all_cards();
            let total_cards = cards.len();
            let mut active_cards = 0;
            let mut unactivated_cards = 0;
            let mut frozen_cards = 0;
            let mut banned_cards = 0;
            let mut total_credits = 0i64;
            let mut used_credits = 0i64;

            for c in &cards {
                match c.status {
                    CardStatus::Active => active_cards += 1,
                    CardStatus::Unactivated => unactivated_cards += 1,
                    CardStatus::Frozen => frozen_cards += 1,
                    CardStatus::Banned => banned_cards += 1,
                    _ => {}
                }
                total_credits += c.credit_total;
                used_credits += c.credit_used;
            }

            let remaining_credits = total_credits.saturating_sub(used_credits);
            let micro = billing::MICRO_CREDITS_PER_CREDIT as f64;
            let (state_bytes, state_ceiling_bytes) = self.billing.state_size();
            let persistence_ready = self.billing.persistence_ready();

            json_response(
                StatusCode::OK,
                &serde_json::json!({
                    "success": true,
                    // Saves fail at the ceiling; archive the ledger well before it.
                    "stateBytes": state_bytes,
                    "stateWarningBytes": billing::engine::STATE_WARNING_BYTES,
                    "stateCeilingBytes": state_ceiling_bytes,
                    "lastSavedAtSecs": self.billing.last_saved_at(),
                    // While a write has failed, every change and every request is refused.
                    "persistenceReady": persistence_ready,
                    "persistenceError": (!persistence_ready).then(|| {
                        persistence_problem(self.billing.last_persistence_error().as_deref())
                    }),
                    "totalCards": total_cards,
                    "activeCards": active_cards,
                    "unactivatedCards": unactivated_cards,
                    "frozenCards": frozen_cards,
                    "bannedCards": banned_cards,
                    "totalCredits": total_credits,
                    "usedCredits": used_credits,
                    "remainingCredits": remaining_credits,
                    "totalPoints": (total_credits as f64) / micro,
                    "usedPoints": (used_credits as f64) / micro,
                    "remainingPoints": (remaining_credits as f64) / micro,
                    // Real totals for the overview, not a sample of the latest traces.
                    "activity": self.billing.activity(now_secs()),
                }),
            )
        })
    }
}

// ---------------------------------------------------------------------------
// 3. Admin Cards List Handler (GET /api/v1/admin/cards)
// ---------------------------------------------------------------------------

/// Runs outside admin authentication so even rejected secret-bearing requests are not cached.
pub async fn card_secret_no_store(req: Request<Body>, next: axum::middleware::Next) -> Response {
    let reveal = req.uri().path() == "/api/v1/admin/cards/reveal";
    let sensitive = matches!(
        req.uri().path(),
        "/api/v1/admin/cards/reveal" | "/api/v1/admin/cards/batch"
    );
    let mut response = next.run(req).await;
    if reveal && matches!(response.status().as_u16(), 400 | 401 | 403 | 429) {
        eprintln!(
            "{}",
            serde_json::json!({"event":"admin_card_reveal_rejected",
            "timestamp":crate::now_secs(),"httpStatus":response.status().as_u16()})
        );
    }
    if sensitive {
        response.headers_mut().insert(
            axum::http::header::CACHE_CONTROL,
            axum::http::HeaderValue::from_static("no-store"),
        );
    }
    response
}

pub struct AdminCardRevealHandler {
    pub billing: BillingEngine,
    pub auth: Arc<AdminAuthState>,
}

impl FacadeHandler for AdminCardRevealHandler {
    fn method(&self) -> Method {
        Method::POST
    }
    fn path(&self) -> &'static str {
        "/api/v1/admin/cards/reveal"
    }
    fn handle<'a>(&'a self, req: Request<Body>) -> BoxFuture<'a, Response> {
        Box::pin(async move {
            let mut response = async {
                if !self.auth.verify(req.headers()) { return unauthorized_response(); }
                #[derive(Deserialize)]
                #[serde(rename_all = "camelCase", deny_unknown_fields)]
                struct RevealRequest { card_id: String }
                let body = match axum::body::to_bytes(req.into_body(), 4096).await {
                    Ok(bytes) => serde_json::from_slice::<RevealRequest>(&bytes).ok(),
                    Err(_) => None,
                };
                let Some(body) = body.filter(|b| valid_text(&b.card_id, 128)) else {
                    return error_response(StatusCode::BAD_REQUEST, "InvalidRequestException", "invalid cardId");
                };
                let result = self.billing.reveal_card_code(&body.card_id);
                // Never include raw card codes, session cookies or request bodies.
                eprintln!("{}", serde_json::json!({
                    "event": "admin_card_reveal", "actor": "admin",
                    "cardId": body.card_id, "timestamp": crate::now_secs(),
                    "result": match &result {
                        Ok(Some(_)) => "success",
                        Ok(None) | Err(billing::BillingError::CardNotFound(_)) => "not_recoverable",
                        Err(_) => "recovery_failed",
                    }
                }));
                match result {
                    Ok(Some(raw_code)) => json_response(StatusCode::OK,
                        &serde_json::json!({"success": true, "rawCode": raw_code})),
                    Ok(None) | Err(billing::BillingError::CardNotFound(_)) => error_response(
                        StatusCode::NOT_FOUND, "NotFoundException", "Card code is not recoverable"),
                    Err(_) => error_response(StatusCode::SERVICE_UNAVAILABLE,
                        "RecoveryException", "Card code recovery failed"),
                }
            }.await;
            response.headers_mut().insert(
                axum::http::header::CACHE_CONTROL,
                axum::http::HeaderValue::from_static("no-store"),
            );
            response
        })
    }
}

pub struct AdminCardsHandler {
    pub billing: BillingEngine,
    pub auth: Arc<AdminAuthState>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminCardItem {
    pub id: String,
    pub code_recoverable: bool,
    pub status: String,
    /// The status as the customer meets it: `expired` for an active card past its expiry.
    pub effective_status: String,
    pub credit_total: i64,
    pub credit_used: i64,
    pub available_credits: i64,
    pub points_total: f64,
    pub points_available: f64,
    pub bound_devices: Vec<String>,
    pub max_devices: u32,
    /// Unbindings the customer has used of `max_rebinds`.
    pub rebinds_used: u32,
    pub max_rebinds: u32,
    /// When the customer may unbind again, while a cooldown runs.
    pub rebind_cooldown_until: Option<u64>,
    pub archived_at: Option<u64>,
    pub activated_at: Option<u64>,
    pub valid_until: Option<u64>,
    /// For a card not yet activated, how long it is valid from activation.
    pub activation_duration_secs: Option<u64>,
    pub group_id: String,
    pub note: Option<String>,
}

fn status_name(status: CardStatus) -> &'static str {
    match status {
        CardStatus::Active => "active",
        CardStatus::Unactivated => "unactivated",
        CardStatus::Frozen => "frozen",
        CardStatus::Banned => "banned",
        CardStatus::Voided => "voided",
        CardStatus::Expired => "expired",
    }
}

/// A card as the console lists and shows it.
fn card_view(card: Card, now: u64) -> AdminCardItem {
    let micro = billing::MICRO_CREDITS_PER_CREDIT as f64;
    AdminCardItem {
        code_recoverable: card.code_encrypted.is_some(),
        status: status_name(card.status).to_string(),
        effective_status: status_name(card.effective_status(now)).to_string(),
        credit_total: card.credit_total,
        credit_used: card.credit_used,
        available_credits: card.available_credits(),
        points_total: (card.credit_total as f64) / micro,
        points_available: (card.available_credits() as f64) / micro,
        max_devices: card.max_devices,
        rebinds_used: card.rebind_count,
        max_rebinds: card.max_rebinds,
        rebind_cooldown_until: card.rebind_cooldown_until(now),
        archived_at: card.archived_at,
        activated_at: card.activated_at,
        valid_until: card.valid_until,
        activation_duration_secs: card
            .activation_duration_secs
            .filter(|_| card.activated_at.is_none()),
        id: card.id,
        bound_devices: card.bound_devices,
        group_id: card.group_id,
        note: card.note,
    }
}

impl FacadeHandler for AdminCardsHandler {
    fn method(&self) -> Method {
        Method::GET
    }

    fn path(&self) -> &'static str {
        "/api/v1/admin/cards"
    }

    fn handle<'a>(&'a self, req: Request<Body>) -> BoxFuture<'a, Response> {
        Box::pin(async move {
            if !self.auth.verify(req.headers()) {
                return unauthorized_response();
            }

            let mut cards = self.billing.list_all_cards();
            cards.sort_unstable_by(|a, b| a.id.cmp(&b.id));
            let card_ids: Vec<&str> = cards.iter().map(|card| card.id.as_str()).collect();
            let revision_input = serde_json::to_vec(&card_ids).expect("card IDs are serializable");
            let revision = billing::card::hex_encode(
                ring::digest::digest(&ring::digest::SHA256, &revision_input).as_ref(),
            );
            let offset = parse_query(req.uri(), "offset")
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(0);
            let limit = parse_query(req.uri(), "limit")
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(100)
                .clamp(1, 500);

            let now = now_secs();
            let items: Vec<AdminCardItem> = cards
                .into_iter()
                .skip(offset)
                .take(limit)
                .map(|card| card_view(card, now))
                .collect();

            json_response(
                StatusCode::OK,
                &serde_json::json!({
                    "success": true,
                    "count": items.len(),
                    "offset": offset,
                    "limit": limit,
                    "revision": revision,
                    "cards": items,
                }),
            )
        })
    }
}

// ---------------------------------------------------------------------------
// 4. Admin Card Status Change Handler (POST /api/v1/admin/cards/status)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminCardStatusRequest {
    pub card_id: String,
    // "freeze" | "unfreeze" | "ban" | "unban" | "void" | "archive" | "unarchive"
    pub action: String,
    pub reason: Option<String>,
}

pub struct AdminCardStatusHandler {
    pub billing: BillingEngine,
    pub auth: Arc<AdminAuthState>,
}

impl FacadeHandler for AdminCardStatusHandler {
    fn method(&self) -> Method {
        Method::POST
    }

    fn path(&self) -> &'static str {
        "/api/v1/admin/cards/status"
    }

    fn handle<'a>(&'a self, req: Request<Body>) -> BoxFuture<'a, Response> {
        Box::pin(async move {
            let Some(operator_id) = self.auth.authenticated_operator(req.headers()) else {
                return unauthorized_response();
            };

            let bytes = match axum::body::to_bytes(req.into_body(), 64 * 1024).await {
                Ok(b) => b,
                Err(e) => return (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
            };

            let req_data: AdminCardStatusRequest = match serde_json::from_slice(&bytes) {
                Ok(d) => d,
                Err(e) => {
                    return (
                        StatusCode::BAD_REQUEST,
                        [(header::CONTENT_TYPE, "application/json")],
                        axum::Json(serde_json::json!({ "success": false, "error": e.to_string() })),
                    )
                        .into_response();
                }
            };

            // Lifting a ban is never done without saying why.
            if req_data.action == "unban" && support_reason(req_data.reason.as_deref()).is_none() {
                return failure(
                    StatusCode::BAD_REQUEST,
                    "A reason of 1 to 200 bytes is required",
                );
            }
            let reason = req_data
                .reason
                .unwrap_or_else(|| "admin-action".to_string());
            let card_id = req_data.card_id.trim();
            if !valid_text(card_id, 128) || !valid_text(&reason, 512) {
                return error_response(
                    StatusCode::BAD_REQUEST,
                    "InvalidRequestException",
                    "card_id and reason must be non-empty and within length limits",
                );
            }

            let res = match req_data.action.as_str() {
                "freeze" => self
                    .billing
                    .freeze_card(card_id, operator_id, &reason, now_secs()),
                "unfreeze" => self
                    .billing
                    .unfreeze_card(card_id, operator_id, &reason, now_secs()),
                "ban" => self
                    .billing
                    .ban_card(card_id, operator_id, &reason, now_secs()),
                "unban" => self
                    .billing
                    .unban_card(card_id, operator_id, &reason, now_secs()),
                "void" => self
                    .billing
                    .void_card(card_id, operator_id, &reason, now_secs()),
                "archive" | "unarchive" => self.billing.set_card_archived(
                    card_id,
                    req_data.action == "archive",
                    operator_id,
                    &reason,
                    now_secs(),
                ),
                _ => {
                    return (
                        StatusCode::BAD_REQUEST,
                        [(header::CONTENT_TYPE, "application/json")],
                        axum::Json(serde_json::json!({
                            "success": false,
                            "error": "Invalid action: must be 'freeze', 'unfreeze', 'ban', 'unban', 'void', 'archive', or 'unarchive'",
                        })),
                    )
                        .into_response();
                }
            };

            match res {
                Ok(card) => json_response(
                    StatusCode::OK,
                    &serde_json::json!({
                        "success": true,
                        "cardId": card_id,
                        "newStatus": card.status,
                        "archivedAt": card.archived_at,
                        "card": card_view(card, now_secs()),
                    }),
                ),
                Err(e) => (
                    StatusCode::BAD_REQUEST,
                    [(header::CONTENT_TYPE, "application/json")],
                    axum::Json(serde_json::json!({ "success": false, "error": e.to_string() })),
                )
                    .into_response(),
            }
        })
    }
}

/// One card's history for the console: issued, activated, top-ups, adjustments and
/// status changes, newest first, each with who made it and why.
pub struct AdminCardHistoryHandler {
    pub billing: BillingEngine,
    pub auth: Arc<AdminAuthState>,
}

impl FacadeHandler for AdminCardHistoryHandler {
    fn method(&self) -> Method {
        Method::GET
    }

    fn path(&self) -> &'static str {
        "/api/v1/admin/cards/history"
    }

    fn handle<'a>(&'a self, req: Request<Body>) -> BoxFuture<'a, Response> {
        Box::pin(async move {
            if !self.auth.verify(req.headers()) {
                return unauthorized_response();
            }
            let Some(card_id) = parse_query(req.uri(), "card_id")
                .and_then(|value| crate::archive::percent_decode(&value))
                .map(|value| value.trim().to_string())
                .filter(|value| valid_text(value, 128))
            else {
                return error_response(
                    StatusCode::BAD_REQUEST,
                    "ValidationException",
                    "card_id is required",
                );
            };
            let Some(events) = self.billing.card_history(&card_id) else {
                return error_response(
                    StatusCode::NOT_FOUND,
                    "ResourceNotFoundException",
                    "没有这张卡密",
                );
            };
            let micro = billing::MICRO_CREDITS_PER_CREDIT as f64;
            let events: Vec<serde_json::Value> = events
                .into_iter()
                .map(|event| {
                    serde_json::json!({
                        "ts": event.ts_secs,
                        "action": event.action,
                        "credits": event.credits,
                        "points": (event.credits as f64) / micro,
                        "operator": event.operator,
                        "reason": event.reason,
                        "invocationId": event.invocation_id,
                        "detail": event.detail,
                    })
                })
                .collect();
            let card = self
                .billing
                .get_card(&card_id)
                .map(|card| card_view(card, now_secs()));
            json_response(
                StatusCode::OK,
                &serde_json::json!({
                    "success": true,
                    "cardId": card_id,
                    "card": card,
                    "events": events,
                }),
            )
        })
    }
}

// ---------------------------------------------------------------------------
// 5. Admin Balance Adjustment Handler (POST /api/v1/admin/cards/adjust)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminCardAdjustRequest {
    pub card_id: String,
    pub delta_points: f64,
    pub reason: Option<String>,
    #[serde(alias = "idempotency_key")]
    pub idempotency_key: Option<String>,
    /// The request the adjustment makes up for, as its trace names it.
    pub invocation_id: Option<String>,
}

pub struct AdminCardAdjustHandler {
    pub billing: BillingEngine,
    pub auth: Arc<AdminAuthState>,
}

impl FacadeHandler for AdminCardAdjustHandler {
    fn method(&self) -> Method {
        Method::POST
    }

    fn path(&self) -> &'static str {
        "/api/v1/admin/cards/adjust"
    }

    fn handle<'a>(&'a self, req: Request<Body>) -> BoxFuture<'a, Response> {
        Box::pin(async move {
            let Some(operator_id) = self.auth.authenticated_operator(req.headers()) else {
                return unauthorized_response();
            };

            if !self.billing.persistence_ready() {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    [(header::CONTENT_TYPE, "application/json")],
                    axum::Json(serde_json::json!({
                        "success": false,
                        "error": "Billing persistence engine is not ready or in read-only recovery state"
                    })),
                )
                    .into_response();
            }

            let idempotency_header = req
                .headers()
                .get("idempotency-key")
                .or_else(|| req.headers().get("x-idempotency-key"))
                .and_then(|h| h.to_str().ok())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());

            let bytes = match axum::body::to_bytes(req.into_body(), 64 * 1024).await {
                Ok(b) => b,
                Err(e) => return (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
            };

            let req_data: AdminCardAdjustRequest = match serde_json::from_slice(&bytes) {
                Ok(d) => d,
                Err(e) => {
                    return (
                        StatusCode::BAD_REQUEST,
                        [(header::CONTENT_TYPE, "application/json")],
                        axum::Json(serde_json::json!({ "success": false, "error": e.to_string() })),
                    )
                        .into_response();
                }
            };

            let body_key = req_data.idempotency_key.as_deref().map(str::trim);
            if idempotency_header
                .as_deref()
                .zip(body_key)
                .is_some_and(|(a, b)| a != b)
            {
                return error_response(
                    StatusCode::BAD_REQUEST,
                    "InvalidRequestException",
                    "conflicting idempotency keys",
                );
            }
            let idempotency_key = idempotency_header.as_deref().or(body_key);
            let Some(idempotency_key) = idempotency_key.filter(|key| {
                !key.is_empty()
                    && key.len() <= 128
                    && key
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"-_.:".contains(&b))
            }) else {
                return error_response(
                    StatusCode::BAD_REQUEST,
                    "InvalidRequestException",
                    "a valid idempotency_key is required (1-128 ASCII letters, digits, -_.:)",
                );
            };

            if !req_data.delta_points.is_finite()
                || req_data.delta_points == 0.0
                || req_data.delta_points.abs() > 1_000_000.0
                || !valid_text(req_data.card_id.trim(), 128)
            {
                return error_response(
                    StatusCode::BAD_REQUEST,
                    "InvalidRequestException",
                    "delta_points must be finite, non-zero, bounded, and card_id must be valid",
                );
            }
            let scaled = req_data.delta_points * billing::MICRO_CREDITS_PER_CREDIT as f64;
            if !scaled.is_finite() || scaled.abs() >= i64::MAX as f64 {
                return error_response(
                    StatusCode::BAD_REQUEST,
                    "InvalidRequestException",
                    "delta_points is out of range",
                );
            }
            let delta_credits = scaled.round() as i64;
            let reason = req_data
                .reason
                .unwrap_or_else(|| "admin-manual-adjustment".to_string());
            if !valid_text(&reason, 512) {
                return error_response(
                    StatusCode::BAD_REQUEST,
                    "InvalidRequestException",
                    "reason is invalid",
                );
            }
            // As a trace names a request: the card, a colon and the client's invocation id.
            let invocation_id = req_data.invocation_id.as_deref().map(str::trim);
            if invocation_id.is_some_and(|id| {
                id.is_empty()
                    || id.len() > 128
                    || !id
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"-_.:".contains(&b))
            }) {
                return error_response(
                    StatusCode::BAD_REQUEST,
                    "InvalidRequestException",
                    "invocationId must be 1-128 ASCII letters, digits, -_.:",
                );
            }

            let now = now_secs();
            match self.billing.adjust_balance_linked(
                &req_data.card_id,
                delta_credits,
                operator_id,
                &reason,
                now,
                Some(idempotency_key),
                invocation_id,
            ) {
                Ok(_entry) => {
                    let card = self.billing.get_card(&req_data.card_id);
                    let new_available = card.as_ref().map(|c| c.available_credits()).unwrap_or(0);
                    json_response(
                        StatusCode::OK,
                        &serde_json::json!({
                            "success": true,
                            "cardId": req_data.card_id,
                            "newAvailableCredits": new_available,
                            "newAvailablePoints": (new_available as f64) / (billing::MICRO_CREDITS_PER_CREDIT as f64),
                        }),
                    )
                }
                Err(e) => {
                    let status = match &e {
                        billing::engine::BillingError::Persistence(_) => {
                            StatusCode::SERVICE_UNAVAILABLE
                        }
                        billing::engine::BillingError::CardNotFound(_) => StatusCode::NOT_FOUND,
                        billing::engine::BillingError::InvalidAdjustment(msg)
                            if msg.starts_with("Idempotency conflict") =>
                        {
                            StatusCode::CONFLICT
                        }
                        billing::engine::BillingError::Card(
                            billing::card::CardError::InsufficientCredit { .. },
                        ) => StatusCode::CONFLICT,
                        billing::engine::BillingError::DuplicateInvocation(_) => {
                            StatusCode::CONFLICT
                        }
                        _ => StatusCode::BAD_REQUEST,
                    };
                    (
                        status,
                        [(header::CONTENT_TYPE, "application/json")],
                        axum::Json(serde_json::json!({ "success": false, "error": e.to_string() })),
                    )
                        .into_response()
                }
            }
        })
    }
}

// ---------------------------------------------------------------------------
// 5b. Card support actions (POST /api/v1/admin/cards/{devices/unbind, rebinds/reset,
//     validity, note, group})
// ---------------------------------------------------------------------------

/// What an operator does for a customer's card. Each is written to the card's history
/// with the operator, and the reply shows the card as it now is.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CardAction {
    /// Free the device seat; the customer's rebind allowance is left as it is.
    UnbindDevice,
    /// Clear the unbindings used and the cooldown.
    ResetRebinds,
    /// Extend up to 500 cards' validity, by days or to a time.
    ExtendValidity,
    /// Replace the note.
    Note,
    /// Move the card to another group.
    ChangeGroup,
}

pub struct AdminCardActionHandler {
    pub billing: BillingEngine,
    pub auth: Arc<AdminAuthState>,
    pub action: CardAction,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UnbindRequest {
    card_id: String,
    device_id: String,
    reason: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ResetRebindsRequest {
    card_id: String,
    reason: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ValidityRequest {
    card_ids: Vec<String>,
    days: Option<u64>,
    valid_until_secs: Option<u64>,
    reason: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct NoteRequest {
    card_id: String,
    note: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GroupRequest {
    card_id: String,
    group_id: String,
    reason: Option<String>,
}

/// Longest extension, by days or to a time from now.
const MAX_EXTENSION_DAYS: u64 = 3650;

/// Why a support action was refused; a change that could not be saved changed nothing.
fn card_action_refused(error: billing::BillingError) -> Response {
    match error {
        billing::BillingError::CardNotFound(_) | billing::BillingError::DeviceNotFound { .. } => {
            failure(StatusCode::NOT_FOUND, &error.to_string())
        }
        billing::BillingError::InvalidAdjustment(message) => {
            failure(StatusCode::BAD_REQUEST, &message)
        }
        billing::BillingError::InvalidState(message) => failure(StatusCode::CONFLICT, &message),
        billing::BillingError::Persistence(_) => failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "The change could not be saved, so nothing was changed; retry shortly",
        ),
        other => failure(StatusCode::CONFLICT, &other.to_string()),
    }
}

impl AdminCardActionHandler {
    fn one_card(result: Result<Card, billing::BillingError>) -> Response {
        match result {
            Ok(card) => json_response(
                StatusCode::OK,
                &serde_json::json!({ "success": true, "card": card_view(card, now_secs()) }),
            ),
            Err(error) => card_action_refused(error),
        }
    }

    fn unbind(&self, operator: &str, body: UnbindRequest) -> Response {
        let device_id = body.device_id.trim();
        if !valid_text(&body.card_id, 128) || device_id.is_empty() || device_id.len() > 256 {
            return failure(StatusCode::BAD_REQUEST, "cardId and deviceId are required");
        }
        let Some(reason) = support_reason(body.reason.as_deref()) else {
            return failure(
                StatusCode::BAD_REQUEST,
                "A reason of 1 to 200 bytes is required",
            );
        };
        Self::one_card(self.billing.admin_unbind_device(
            body.card_id.trim(),
            device_id,
            operator,
            reason,
            now_secs(),
        ))
    }

    fn reset_rebinds(&self, operator: &str, body: ResetRebindsRequest) -> Response {
        if !valid_text(&body.card_id, 128) {
            return failure(StatusCode::BAD_REQUEST, "cardId is required");
        }
        let Some(reason) = support_reason(body.reason.as_deref()) else {
            return failure(
                StatusCode::BAD_REQUEST,
                "A reason of 1 to 200 bytes is required",
            );
        };
        Self::one_card(self.billing.reset_rebinds(
            body.card_id.trim(),
            operator,
            reason,
            now_secs(),
        ))
    }

    fn extend(&self, operator: &str, body: ValidityRequest) -> Response {
        if body.card_ids.is_empty() || body.card_ids.len() > 500 {
            return failure(StatusCode::BAD_REQUEST, "cardIds must name 1 to 500 cards");
        }
        if !body.card_ids.iter().all(|id| valid_text(id, 128)) {
            return failure(StatusCode::BAD_REQUEST, "Invalid card ID in cardIds");
        }
        let now = now_secs();
        let extension = match (body.days, body.valid_until_secs) {
            (Some(days), None) if (1..=MAX_EXTENSION_DAYS).contains(&days) => {
                billing::ValidityExtension::Days(days)
            }
            (Some(_), None) => {
                return failure(StatusCode::BAD_REQUEST, "days must be between 1 and 3650")
            }
            (None, Some(until))
                if until > now && until <= now.saturating_add(MAX_EXTENSION_DAYS * 86_400) =>
            {
                billing::ValidityExtension::Until(until)
            }
            (None, Some(_)) => {
                return failure(
                    StatusCode::BAD_REQUEST,
                    "validUntilSecs must be in the future and within 3650 days",
                )
            }
            _ => {
                return failure(
                    StatusCode::BAD_REQUEST,
                    "Give exactly one of days and validUntilSecs",
                )
            }
        };
        let Some(reason) = support_reason(body.reason.as_deref()) else {
            return failure(
                StatusCode::BAD_REQUEST,
                "A reason of 1 to 200 bytes is required",
            );
        };
        let card_ids: Vec<String> = body
            .card_ids
            .iter()
            .map(|id| id.trim().to_string())
            .collect();
        match self
            .billing
            .extend_validity(&card_ids, extension, operator, reason, now)
        {
            Ok(cards) => {
                let cards: Vec<AdminCardItem> =
                    cards.into_iter().map(|card| card_view(card, now)).collect();
                json_response(
                    StatusCode::OK,
                    &serde_json::json!({ "success": true, "count": cards.len(), "cards": cards }),
                )
            }
            Err(error) => card_action_refused(error),
        }
    }

    fn note(&self, operator: &str, body: NoteRequest) -> Response {
        if !valid_text(&body.card_id, 128) {
            return failure(StatusCode::BAD_REQUEST, "cardId is required");
        }
        let note = body.note.as_deref().map(str::trim).unwrap_or_default();
        if note.len() > 256 || note.chars().any(char::is_control) {
            return failure(
                StatusCode::BAD_REQUEST,
                "note must be at most 256 bytes, without control characters",
            );
        }
        Self::one_card(self.billing.set_card_note(
            body.card_id.trim(),
            (!note.is_empty()).then_some(note),
            operator,
            now_secs(),
        ))
    }

    fn change_group(&self, operator: &str, body: GroupRequest) -> Response {
        if !valid_text(&body.card_id, 128) || !valid_text(&body.group_id, 64) {
            return failure(StatusCode::BAD_REQUEST, "cardId and groupId are required");
        }
        let Some(reason) = support_reason(body.reason.as_deref()) else {
            return failure(
                StatusCode::BAD_REQUEST,
                "A reason of 1 to 200 bytes is required",
            );
        };
        Self::one_card(self.billing.change_card_group(
            body.card_id.trim(),
            body.group_id.trim(),
            operator,
            reason,
            now_secs(),
        ))
    }
}

/// The request body, or what is wrong with it.
fn card_action_body<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, String> {
    serde_json::from_slice(bytes).map_err(|error| format!("Invalid request body: {error}"))
}

impl FacadeHandler for AdminCardActionHandler {
    fn method(&self) -> Method {
        Method::POST
    }

    fn path(&self) -> &'static str {
        match self.action {
            CardAction::UnbindDevice => "/api/v1/admin/cards/devices/unbind",
            CardAction::ResetRebinds => "/api/v1/admin/cards/rebinds/reset",
            CardAction::ExtendValidity => "/api/v1/admin/cards/validity",
            CardAction::Note => "/api/v1/admin/cards/note",
            CardAction::ChangeGroup => "/api/v1/admin/cards/group",
        }
    }

    fn handle<'a>(&'a self, req: Request<Body>) -> BoxFuture<'a, Response> {
        Box::pin(async move {
            let Some(operator) = self.auth.authenticated_operator(req.headers()) else {
                return unauthorized_response();
            };
            let Ok(bytes) = axum::body::to_bytes(req.into_body(), 64 * 1024).await else {
                return failure(StatusCode::BAD_REQUEST, "Invalid request body");
            };
            let response = match self.action {
                CardAction::UnbindDevice => {
                    card_action_body(&bytes).map(|body| self.unbind(operator, body))
                }
                CardAction::ResetRebinds => {
                    card_action_body(&bytes).map(|body| self.reset_rebinds(operator, body))
                }
                CardAction::ExtendValidity => {
                    card_action_body(&bytes).map(|body| self.extend(operator, body))
                }
                CardAction::Note => card_action_body(&bytes).map(|body| self.note(operator, body)),
                CardAction::ChangeGroup => {
                    card_action_body(&bytes).map(|body| self.change_group(operator, body))
                }
            };
            response.unwrap_or_else(|message| failure(StatusCode::BAD_REQUEST, &message))
        })
    }
}

// ---------------------------------------------------------------------------
// 6. Admin Announcements Handlers (GET / POST /api/v1/admin/announcements)
// ---------------------------------------------------------------------------

pub struct AdminGetAnnouncementsHandler {
    pub billing: BillingEngine,
    pub auth: Arc<AdminAuthState>,
}

impl FacadeHandler for AdminGetAnnouncementsHandler {
    fn method(&self) -> Method {
        Method::GET
    }

    fn path(&self) -> &'static str {
        "/api/v1/admin/announcements"
    }

    fn handle<'a>(&'a self, req: Request<Body>) -> BoxFuture<'a, Response> {
        Box::pin(async move {
            if !self.auth.verify(req.headers()) {
                return unauthorized_response();
            }

            let list = self.billing.list_active_announcements(now_secs());
            json_response(
                StatusCode::OK,
                &serde_json::json!({ "success": true, "announcements": list }),
            )
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminCreateAnnouncementRequest {
    pub title: String,
    pub content: String,
    pub level: Option<String>,
    pub ttl_secs: Option<u64>,
}

pub struct AdminCreateAnnouncementHandler {
    pub billing: BillingEngine,
    pub auth: Arc<AdminAuthState>,
}

impl FacadeHandler for AdminCreateAnnouncementHandler {
    fn method(&self) -> Method {
        Method::POST
    }

    fn path(&self) -> &'static str {
        "/api/v1/admin/announcements"
    }

    fn handle<'a>(&'a self, req: Request<Body>) -> BoxFuture<'a, Response> {
        Box::pin(async move {
            if !self.auth.verify(req.headers()) {
                return unauthorized_response();
            }

            let bytes = match axum::body::to_bytes(req.into_body(), 64 * 1024).await {
                Ok(b) => b,
                Err(e) => return (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
            };

            let req_data: AdminCreateAnnouncementRequest = match serde_json::from_slice(&bytes) {
                Ok(d) => d,
                Err(e) => {
                    return (
                        StatusCode::BAD_REQUEST,
                        [(header::CONTENT_TYPE, "application/json")],
                        axum::Json(serde_json::json!({ "success": false, "error": e.to_string() })),
                    )
                        .into_response();
                }
            };

            if !valid_text(&req_data.title, 256)
                || !valid_text(&req_data.content, 20_000)
                || req_data
                    .ttl_secs
                    .is_some_and(|ttl| !(60..=31 * 86_400).contains(&ttl))
            {
                return error_response(
                    StatusCode::BAD_REQUEST,
                    "InvalidRequestException",
                    "announcement fields are invalid",
                );
            }

            let now = now_secs();
            let level = match req_data.level.as_deref() {
                Some("warning") => AnnouncementLevel::Warning,
                Some("critical") => AnnouncementLevel::Critical,
                _ => AnnouncementLevel::Info,
            };

            let id = format!(
                "ann-{}",
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            );
            let mut ann = Announcement::new(id, req_data.title, req_data.content, level, now);
            if let Some(ttl) = req_data.ttl_secs {
                ann = ann.with_expiry(now + ttl);
            }

            if self.billing.publish_announcement(ann.clone()).is_err() {
                return error_response(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "ServiceUnavailableException",
                    "announcement could not be saved; nothing was published",
                );
            }

            json_response(
                StatusCode::OK,
                &serde_json::json!({ "success": true, "announcement": ann }),
            )
        })
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct AdminWithdrawAnnouncementRequest {
    pub id: String,
}

/// A published announcement pops up on every customer client until it expires; a wrong
/// one must be withdrawable instead of contradicted by a second one.
pub struct AdminWithdrawAnnouncementHandler {
    pub billing: BillingEngine,
    pub auth: Arc<AdminAuthState>,
}

impl FacadeHandler for AdminWithdrawAnnouncementHandler {
    fn method(&self) -> Method {
        Method::POST
    }

    fn path(&self) -> &'static str {
        "/api/v1/admin/announcements/withdraw"
    }

    fn handle<'a>(&'a self, req: Request<Body>) -> BoxFuture<'a, Response> {
        Box::pin(async move {
            if !self.auth.verify(req.headers()) {
                return unauthorized_response();
            }
            let id = match axum::body::to_bytes(req.into_body(), 4 * 1024)
                .await
                .ok()
                .and_then(|bytes| {
                    serde_json::from_slice::<AdminWithdrawAnnouncementRequest>(&bytes).ok()
                }) {
                Some(request) if valid_text(&request.id, 128) => request.id,
                _ => {
                    return error_response(
                        StatusCode::BAD_REQUEST,
                        "InvalidRequestException",
                        "announcement id is required",
                    )
                }
            };
            match self.billing.withdraw_announcement(&id) {
                Ok(true) => {}
                Ok(false) => {
                    return error_response(
                        StatusCode::NOT_FOUND,
                        "ResourceNotFoundException",
                        "announcement not found",
                    )
                }
                Err(_) => {
                    return error_response(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "ServiceUnavailableException",
                        "withdrawal could not be saved; the announcement is still shown",
                    )
                }
            }
            eprintln!(
                "{}",
                serde_json::json!({"event": "admin_announcement_withdrawn", "id": id})
            );
            json_response(
                StatusCode::OK,
                &serde_json::json!({ "success": true, "id": id }),
            )
        })
    }
}

// 7. Admin Snapshot Sync Handler (POST /api/v1/admin/snapshot/sync)
pub struct AdminSnapshotSyncHandler {
    pub billing: BillingEngine,
    pub auth: Arc<AdminAuthState>,
}

impl FacadeHandler for AdminSnapshotSyncHandler {
    fn method(&self) -> Method {
        Method::POST
    }
    fn path(&self) -> &'static str {
        "/api/v1/admin/snapshot/sync"
    }
    fn handle<'a>(&'a self, req: Request<Body>) -> BoxFuture<'a, Response> {
        Box::pin(async move {
            if !self.auth.verify(req.headers()) {
                return unauthorized_response();
            }
            match self.billing.sync_to_disk_checked() {
                Ok(()) => {
                    let seq = self.billing.snapshot_sequence();
                    let checksum = self.billing.last_snapshot_checksum();
                    json_response(
                        StatusCode::OK,
                        &serde_json::json!({
                            "status": "synchronized",
                            "sequence": seq,
                            "checksum": checksum,
                            "timestamp": now_secs(),
                        }),
                    )
                }
                Err(error) => error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "PersistenceException",
                    &error.to_string(),
                ),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::persistence_problem;

    #[test]
    fn a_persistence_problem_is_named_without_its_paths_or_sizes() {
        for (error, problem) in [
            (
                "billing snapshot size 300000000 bytes exceeds maximum allowable limit of 268435456 bytes",
                "The saved state has reached its size ceiling; archive old ledger entries",
            ),
            (
                "There is not enough space on the disk. (os error 112)",
                "The disk holding the saved state is full",
            ),
            (
                "No space left on device (os error 28)",
                "The disk holding the saved state is full",
            ),
            (
                "Access is denied. (os error 5)",
                "The saved state cannot be written: permission denied",
            ),
            (
                "persisting issuance replay secrets requires a master KEK",
                "Saving this state needs the master key, which is not configured",
            ),
            (
                r"C:\data\billing_state.json.gen_42: injected persistence fault",
                "The saved state could not be written",
            ),
        ] {
            assert_eq!(persistence_problem(Some(error)), problem, "{error}");
        }
        assert_eq!(
            persistence_problem(None),
            "The saved state could not be written"
        );
    }
}
