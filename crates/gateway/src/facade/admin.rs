//! Administrator REST Management APIs (Spec §7, P0-01, P0-02, P1-02).
//!
//! Exposes protected endpoints for the Admin UI console:
//! - `GET /api/v1/admin/me`: verify admin auth credentials
//! - `GET /api/v1/admin/stats`: system overview and financial metrics
//! - `GET /api/v1/admin/cards`: query cards with filtering
//! - `POST /api/v1/admin/cards/status`: freeze, unfreeze, ban, void, archive, or unarchive cards (persisted)
//! - `POST /api/v1/admin/cards/adjust`: manual balance adjustment (persisted)
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
use billing::card::CardStatus;
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

fn parse_query(uri: &axum::http::Uri, key: &str) -> Option<String> {
    uri.query()?
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find_map(|(name, value)| (name == key).then(|| value.to_string()))
}

/// Shared admin authorization state.
#[derive(Clone)]
pub struct AdminAuthState {
    pub admin_key: String,
    sessions: Arc<std::sync::RwLock<HashMap<String, u64>>>,
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
        let token = headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let Some(token) = token else { return false };

        let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::HS256);
        validation.validate_exp = true;
        validation.set_issuer(&["kiro-byok-admin"]);
        let decoded = jsonwebtoken::decode::<AdminSessionClaims>(
            token,
            &jsonwebtoken::DecodingKey::from_secret(self.admin_key.as_bytes()),
            &validation,
        );
        let Ok(data) = decoded else { return false };
        let claims = data.claims;
        if claims.sub != "admin" || claims.role != "admin" {
            return false;
        }
        if claims.epoch != self.session_epoch.load(Ordering::Acquire) {
            return false;
        }
        let now = now_secs();
        if claims.exp <= now {
            return false;
        }
        let mut sessions = match self.sessions.write() {
            Ok(value) => value,
            Err(_) => return false,
        };
        sessions.retain(|_, exp| *exp > now);
        sessions
            .get(&claims.jti)
            .is_some_and(|exp| *exp >= claims.exp)
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

    pub fn issue_session(&self, ttl_secs: u64) -> Result<AdminSessionResponse, String> {
        let now = now_secs();
        let ttl = ttl_secs.clamp(60, 3600);
        let exp = now.saturating_add(ttl);
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
        sessions.retain(|_, expires| *expires > now);
        if sessions.len() >= 256 {
            return Err("too many active admin sessions".to_string());
        }
        sessions.insert(jti, exp);
        Ok(AdminSessionResponse {
            access_token: token,
            token_type: "Bearer".to_string(),
            expires_in: ttl,
            expires_at: exp,
        })
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

pub struct AdminFinancialsHandler {
    pub billing: BillingEngine,
    pub auth: Arc<AdminAuthState>,
}

pub struct AdminProvidersHandler {
    pub billing: BillingEngine,
    pub auth: Arc<AdminAuthState>,
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
            let keys = self.billing.list_provider_keys(None);
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
            // One snapshot keeps estimates, coverage and their settings consistent.
            let snapshot = self.billing.export_snapshot();
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
            json_response(
                StatusCode::OK,
                &serde_json::json!({
                    "success": true,
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
            let card_id = parse_query(req.uri(), "card_id");
            let limit = parse_query(req.uri(), "limit")
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(100)
                .clamp(1, 500);
            let traces = self.billing.list_traces(card_id.as_deref(), limit);
            json_response(
                StatusCode::OK,
                &serde_json::json!({
                    "success": true,
                    "count": traces.len(),
                    "traces": traces,
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

            json_response(
                StatusCode::OK,
                &serde_json::json!({
                    "success": true,
                    // Saves fail at the ceiling; archive the ledger well before it.
                    "stateBytes": state_bytes,
                    "stateWarningBytes": billing::engine::STATE_WARNING_BYTES,
                    "stateCeilingBytes": state_ceiling_bytes,
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
    pub credit_total: i64,
    pub credit_used: i64,
    pub available_credits: i64,
    pub points_total: f64,
    pub points_available: f64,
    pub bound_devices: Vec<String>,
    pub max_devices: u32,
    pub archived_at: Option<u64>,
    pub activated_at: Option<u64>,
    pub valid_until: Option<u64>,
    pub group_id: String,
    pub note: Option<String>,
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
            let micro = billing::MICRO_CREDITS_PER_CREDIT as f64;
            let offset = parse_query(req.uri(), "offset")
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(0);
            let limit = parse_query(req.uri(), "limit")
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(100)
                .clamp(1, 500);

            let items: Vec<AdminCardItem> = cards
                .into_iter()
                .skip(offset)
                .take(limit)
                .map(|c| AdminCardItem {
                    id: c.id.clone(),
                    code_recoverable: c.code_encrypted.is_some(),
                    status: match c.status {
                        CardStatus::Active => "active",
                        CardStatus::Unactivated => "unactivated",
                        CardStatus::Frozen => "frozen",
                        CardStatus::Banned => "banned",
                        CardStatus::Voided => "voided",
                        CardStatus::Expired => "expired",
                    }
                    .to_string(),
                    credit_total: c.credit_total,
                    credit_used: c.credit_used,
                    available_credits: c.available_credits(),
                    points_total: (c.credit_total as f64) / micro,
                    points_available: (c.available_credits() as f64) / micro,
                    bound_devices: c.bound_devices,
                    max_devices: c.max_devices,
                    archived_at: c.archived_at,
                    activated_at: c.activated_at,
                    valid_until: c.valid_until,
                    group_id: c.group_id,
                    note: c.note,
                })
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
    pub action: String, // "freeze" | "unfreeze" | "ban" | "void" | "archive" | "unarchive"
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
                "freeze" => self.billing.freeze_card(card_id, &reason),
                "unfreeze" => self.billing.unfreeze_card(card_id),
                "ban" => self.billing.ban_card(card_id, &reason),
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
                            "error": "Invalid action: must be 'freeze', 'unfreeze', 'ban', 'void', 'archive', or 'unarchive'",
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

            let now = now_secs();
            match self.billing.adjust_balance_idempotent(
                &req_data.card_id,
                delta_credits,
                operator_id,
                &reason,
                now,
                Some(idempotency_key),
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
