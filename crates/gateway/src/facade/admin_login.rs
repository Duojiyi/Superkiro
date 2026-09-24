//! Browser-only administrator authentication. No bootstrap key leaves the server.
use super::{admin::AdminAuthState, json_response, Response};
use axum::{
    body::to_bytes,
    extract::Request,
    http::{header, HeaderMap, StatusCode},
    middleware::Next,
};
use base64::Engine;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::Semaphore;

const COOKIE: &str = "__Host-admin_session";
const TTL: u64 = 900;
const MAX_LOGIN_SOURCES: usize = 4096;
const MAX_PASSWORD_CHECKS: usize = 4;
/// Sources that logged in successfully keep a lane of their own for this long.
const KNOWN_SOURCE_TTL: Duration = Duration::from_secs(30 * 86_400);
const MAX_KNOWN_SOURCES: usize = 32;
/// Password checks only known sources may use, when strangers hold all the others.
const KNOWN_SOURCE_CHECKS: usize = 1;

#[derive(Clone)]
pub struct BrowserAuth {
    origin: String,
    password_hash: String,
    totp: Option<Arc<Totp>>,
    // Source budgets include malformed requests; one peer cannot lock out all peers.
    attempts: Arc<Mutex<HashMap<String, (Instant, u32)>>>,
    password_checks: Arc<Semaphore>,
    // A source that has logged in before gets its own budgets and a reserved password
    // check. Strangers can neither fill nor use them, so a flood from new sources, which
    // fills the source table and keeps every check busy, cannot lock the operator out.
    known_sources: Arc<Mutex<HashMap<String, Instant>>>,
    known_attempts: Arc<Mutex<HashMap<String, (Instant, u32)>>>,
    known_checks: Arc<Semaphore>,
    refusal_reported: Arc<Mutex<Option<Instant>>>,
}

impl BrowserAuth {
    pub fn new(origin: String, password_hash: String) -> Result<Self, String> {
        let url = reqwest::Url::parse(&origin).map_err(|_| "invalid ADMIN_ORIGIN")?;
        if url.scheme() != "https" || url.origin().ascii_serialization() != origin {
            return Err("ADMIN_ORIGIN must be an exact HTTPS origin (no trailing slash)".into());
        }
        let parts = password_hash
            .parse::<bcrypt::HashParts>()
            .map_err(|_| "invalid ADMIN_PASSWORD_HASH (expected bcrypt)")?;
        if !(4..=16).contains(&parts.get_cost()) {
            return Err("ADMIN_PASSWORD_HASH bcrypt cost must be between 4 and 16".into());
        }
        Ok(Self {
            origin,
            password_hash,
            totp: None,
            attempts: Arc::new(Mutex::new(HashMap::new())),
            password_checks: Arc::new(Semaphore::new(MAX_PASSWORD_CHECKS)),
            known_sources: Arc::new(Mutex::new(HashMap::new())),
            known_attempts: Arc::new(Mutex::new(HashMap::new())),
            known_checks: Arc::new(Semaphore::new(KNOWN_SOURCE_CHECKS)),
            refusal_reported: Arc::new(Mutex::new(None)),
        })
    }

    pub fn from_env() -> Result<Self, String> {
        if std::env::var("ADMIN_BROWSER_LOGIN").as_deref() != Ok("true") {
            return Err(
                "ADMIN_BROWSER_LOGIN=true is required; raw-key browser fallback is disabled".into(),
            );
        }
        let mut browser = Self::new(
            std::env::var("ADMIN_ORIGIN").map_err(|_| "ADMIN_ORIGIN is required")?,
            std::env::var("ADMIN_PASSWORD_HASH").map_err(|_| "ADMIN_PASSWORD_HASH is required")?,
        )?;
        match std::env::var("ADMIN_TOTP_SECRET") {
            Ok(secret) => browser.totp = Some(Arc::new(Totp::new(&secret, now_secs())?)),
            Err(std::env::VarError::NotPresent) => {}
            Err(_) => return Err("invalid ADMIN_TOTP_SECRET".into()),
        }
        // Once TOTP is provisioned, ADMIN_TOTP_REQUIRED=true keeps a later deployment
        // that lost the secret from silently falling back to password-only login.
        if browser.totp.is_none() {
            if std::env::var("ADMIN_TOTP_REQUIRED").as_deref() == Ok("true") {
                return Err("ADMIN_TOTP_REQUIRED=true but ADMIN_TOTP_SECRET is not set".into());
            }
            eprintln!(
                "[kiro-admin] administrator login is password-only: provision ADMIN_TOTP_SECRET, \
                 then set ADMIN_TOTP_REQUIRED=true (docs/ADMIN-TOTP-DEPLOYMENT.md)"
            );
        }
        Ok(browser)
    }

    fn session_error(&self, message: &str) -> Response {
        no_store(json_response(
            StatusCode::UNAUTHORIZED,
            &serde_json::json!({
                "success": false, "error": message, "authenticated": false,
                "twoFactorEnabled": self.totp.is_some(), "totpRequired": self.totp.is_some()
            }),
        ))
    }

    fn reserve_attempt(&self, source: &str) -> bool {
        if self.is_known(source) {
            return take_attempt(&self.known_attempts, source, MAX_KNOWN_SOURCES).unwrap_or(false);
        }
        take_attempt(&self.attempts, source, MAX_LOGIN_SOURCES).unwrap_or_else(|| {
            self.report_refusal("the login source table is full");
            false
        })
    }

    fn is_known(&self, source: &str) -> bool {
        self.known_sources.lock().is_ok_and(|known| {
            known
                .get(source)
                .is_some_and(|at| at.elapsed() < KNOWN_SOURCE_TTL)
        })
    }

    fn remember_source(&self, source: &str) {
        // The source's budget moves with it to its own lane, spent attempts included.
        if let (Ok(mut shared), Ok(mut own)) = (self.attempts.lock(), self.known_attempts.lock()) {
            if let Some(budget) = shared.remove(source) {
                own.insert(source.to_owned(), budget);
            }
        }
        let Ok(mut known) = self.known_sources.lock() else {
            return;
        };
        known.insert(source.to_owned(), Instant::now());
        known.retain(|_, at| at.elapsed() < KNOWN_SOURCE_TTL);
        while known.len() > MAX_KNOWN_SOURCES {
            let Some(oldest) = known
                .iter()
                .min_by_key(|(_, at)| **at)
                .map(|(source, _)| source.clone())
            else {
                break;
            };
            known.remove(&oldest);
        }
    }

    /// A password check for this login: one of the shared ones, or the reserved one
    /// when every shared check is busy and the source has logged in before.
    fn password_check(&self, source: &str) -> Option<tokio::sync::OwnedSemaphorePermit> {
        if let Ok(permit) = self.password_checks.clone().try_acquire_owned() {
            return Some(permit);
        }
        if self.is_known(source) {
            if let Ok(permit) = self.known_checks.clone().try_acquire_owned() {
                return Some(permit);
            }
        }
        self.report_refusal("every password check is busy");
        None
    }

    /// Log, at most once a minute, that logins from new sources are being refused.
    /// Only a log line: it must not become another way to lock anyone out.
    fn report_refusal(&self, why: &str) {
        let Ok(mut last) = self.refusal_reported.lock() else {
            return;
        };
        if last.is_some_and(|at| at.elapsed() < Duration::from_secs(60)) {
            return;
        }
        *last = Some(Instant::now());
        eprintln!(
            "[kiro-admin] refusing administrator logins from new sources: {why}; \
             sources that logged in before keep their own lane"
        );
    }
}

/// Take one attempt from `source`'s budget of ten a minute. `None` when the table is
/// full of live budgets, `Some(false)` when this source's budget is spent.
fn take_attempt(
    table: &Mutex<HashMap<String, (Instant, u32)>>,
    source: &str,
    capacity: usize,
) -> Option<bool> {
    let Ok(mut budgets) = table.lock() else {
        return Some(false);
    };
    let now = Instant::now();
    let window = Duration::from_secs(60);
    if !budgets.contains_key(source) && budgets.len() >= capacity {
        budgets.retain(|_, (start, _)| now.duration_since(*start) < window);
        // Do not evict live lockouts: source churn must not reset their budgets.
        if budgets.len() >= capacity {
            return None;
        }
    }
    let budget = budgets.entry(source.to_owned()).or_insert((now, 0));
    if now.duration_since(budget.0) >= window {
        *budget = (now, 0);
    }
    if budget.1 >= 10 {
        return Some(false);
    }
    budget.1 += 1;
    Some(true)
}

/// What a login budget is kept for: an IPv4 address, or an IPv6 /64, which one
/// subscriber usually holds whole and could otherwise spread across endless addresses.
fn login_source(ip: &str) -> String {
    match ip.parse::<std::net::IpAddr>().map(|ip| ip.to_canonical()) {
        Ok(std::net::IpAddr::V6(v6)) => {
            let network = std::net::Ipv6Addr::from(u128::from(v6) & (u128::MAX << 64));
            format!("{network}/64")
        }
        Ok(v4) => v4.to_string(),
        Err(_) => ip.to_owned(),
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

struct Totp {
    key: ring::hmac::Key,
    last_used: Mutex<u64>,
}

impl Totp {
    fn new(secret: &str, now: u64) -> Result<Self, String> {
        // Canonical unpadded RFC4648 Base32, 160..512 bits. Never echo input.
        let invalid = || {
            "invalid ADMIN_TOTP_SECRET (expected unpadded Base32, 20..64 decoded bytes)".to_string()
        };
        if !(32..=103).contains(&secret.len()) {
            return Err(invalid());
        }
        let mut bytes = Vec::new();
        let (mut buffer, mut bits) = (0u32, 0u32);
        for c in secret.bytes() {
            let n = match c {
                b'A'..=b'Z' => c - b'A',
                b'2'..=b'7' => c - b'2' + 26,
                _ => return Err(invalid()),
            };
            buffer = (buffer << 5) | u32::from(n);
            bits += 5;
            if bits >= 8 {
                bits -= 8;
                bytes.push((buffer >> bits) as u8);
            }
            buffer &= (1 << bits) - 1;
        }
        if !(20..=64).contains(&bytes.len()) || bits >= 5 || buffer != 0 {
            return Err(invalid());
        }
        Ok(Self {
            key: ring::hmac::Key::new(ring::hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY, &bytes),
            // Reject steps potentially consumed by a previous process, including its future window.
            last_used: Mutex::new(now / 30 + 1),
        })
    }

    fn code(&self, step: u64) -> String {
        let tag = ring::hmac::sign(&self.key, &step.to_be_bytes());
        let bytes = tag.as_ref();
        let offset = usize::from(bytes[19] & 15);
        let value = u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap()) & 0x7fff_ffff;
        format!("{:06}", value % 1_000_000)
    }

    fn verify(&self, code: &str, now: u64) -> bool {
        if code.len() != 6 || !code.bytes().all(|c| c.is_ascii_digit()) {
            return false;
        }
        let Ok(mut last) = self.last_used.lock() else {
            return false;
        };
        let step = now / 30;
        let mut matched = None;
        // Always compare the whole window using ring's constant-time primitive.
        for candidate in step.saturating_sub(1)..=step.saturating_add(1) {
            #[allow(deprecated)]
            let equal = ring::constant_time::verify_slices_are_equal(
                self.code(candidate).as_bytes(),
                code.as_bytes(),
            )
            .is_ok();
            if equal & (candidate > *last) {
                matched = Some(candidate);
            }
        }
        if let Some(candidate) = matched {
            *last = candidate;
            true
        } else {
            false
        }
    }
}

// Read exp only after signature, issuer, epoch and revocation have been verified.
fn verified_expiry(token: &str) -> Option<u64> {
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(token.split('.').nth(1)?)
        .ok()?;
    serde_json::from_slice::<serde_json::Value>(&payload)
        .ok()?
        .get("exp")?
        .as_u64()
}

fn reply(status: StatusCode, message: &str) -> Response {
    json_response(
        status,
        &serde_json::json!({"success": false, "error": message}),
    )
}

fn login_throttled() -> Response {
    let mut response = reply(
        StatusCode::TOO_MANY_REQUESTS,
        "登录尝试过于频繁，请一分钟后重试",
    );
    response
        .headers_mut()
        .insert(header::RETRY_AFTER, "60".parse().unwrap());
    no_store(response)
}

fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    response
}

fn cookie(response: &mut Response, token: &str, age: u64) {
    response.headers_mut().insert(
        header::SET_COOKIE,
        format!("{COOKIE}={token}; Path=/; Max-Age={age}; HttpOnly; Secure; SameSite=Strict")
            .parse()
            .unwrap(),
    );
}

pub fn csrf_token(headers: &HeaderMap) -> String {
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    ring::digest::digest(&ring::digest::SHA256, token.as_bytes())
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[derive(Deserialize)]
struct Credentials {
    username: String,
    password: String,
    #[serde(default, rename = "totpCode")]
    totp_code: Option<String>,
}

pub async fn handle(
    auth: &AdminAuthState,
    browser: &BrowserAuth,
    mut req: Request,
    next: Next,
) -> Response {
    let origin = req
        .headers()
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok());
    let mutation = !matches!(req.method().as_str(), "GET" | "HEAD" | "OPTIONS");
    if (mutation && origin != Some(browser.origin.as_str()))
        || origin.is_some_and(|v| v != browser.origin)
        || req
            .headers()
            .get("sec-fetch-site")
            .is_some_and(|v| v == "cross-site")
    {
        return no_store(reply(
            StatusCode::FORBIDDEN,
            "Invalid administrator request origin",
        ));
    }
    if req.uri().path() == "/api/v1/admin/session" && req.method() == "POST" {
        // Only configured proxy peers may supply forwarded addresses. Missing
        // ConnectInfo intentionally shares a fail-closed "unknown" source budget.
        let source = login_source(&crate::security::client_ip(&req));
        if !browser.reserve_attempt(&source) {
            return login_throttled();
        }
        if req
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(';').next())
            != Some("application/json")
        {
            return no_store(reply(StatusCode::UNSUPPORTED_MEDIA_TYPE, "JSON required"));
        }
        let Ok(bytes) = to_bytes(std::mem::take(req.body_mut()), 4096).await else {
            return no_store(reply(StatusCode::BAD_REQUEST, "Invalid login request"));
        };
        let Ok(credentials) = serde_json::from_slice::<Credentials>(&bytes) else {
            return no_store(reply(StatusCode::BAD_REQUEST, "Invalid login request"));
        };
        if credentials.password.is_empty()
            || credentials.password.len() > 72
            || credentials.username.len() > 128
        {
            return browser.session_error("Invalid administrator credentials or verification code");
        }
        // Bound expensive work globally, without a long-lived anonymous global
        // lockout or an unbounded spawn_blocking queue. Malformed bodies never
        // acquire a permit. Keep it in the worker even if the caller disconnects.
        let Some(permit) = browser.password_check(&source) else {
            return login_throttled();
        };
        let code = credentials.totp_code.clone().unwrap_or_default();
        let hash = browser.password_hash.clone();
        let valid = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let valid_password = bcrypt::verify(credentials.password, &hash).unwrap_or(false);
            valid_password & crate::security::ct_eq(credentials.username.as_bytes(), b"admin")
        })
        .await
        .unwrap_or(false);
        if !valid
            || browser
                .totp
                .as_ref()
                .is_some_and(|totp| !totp.verify(&code, now_secs()))
        {
            return browser.session_error("Invalid administrator credentials or verification code");
        }
        browser.remember_source(&source);
        let Ok(session) = auth.issue_session(TTL) else {
            return no_store(reply(
                StatusCode::TOO_MANY_REQUESTS,
                "Too many administrator sessions",
            ));
        };
        // Successful re-login rotates and revokes the previous browser session.
        if let Some(token) = session_cookie(req.headers()) {
            let mut headers = HeaderMap::new();
            if let Ok(value) = format!("Bearer {token}").parse() {
                headers.insert(header::AUTHORIZATION, value);
                auth.revoke_single_session(&headers);
            }
        }
        let mut response = json_response(
            StatusCode::OK,
            &serde_json::json!({"success": true, "authenticated": true,
                "expiresIn": session.expires_at.saturating_sub(now_secs()), "expiresAt": session.expires_at,
                "twoFactorEnabled": browser.totp.is_some(), "totpRequired": browser.totp.is_some()}),
        );
        cookie(&mut response, &session.access_token, TTL);
        return no_store(response);
    }
    // Browser production surface accepts only the host-only session cookie, never an admin key.
    let Some(token) = session_cookie(req.headers()) else {
        return browser.session_error("Invalid administrator credentials or verification code");
    };
    let Ok(value) = format!("Bearer {token}").parse() else {
        return browser.session_error("Invalid administrator credentials or verification code");
    };
    req.headers_mut().insert(header::AUTHORIZATION, value);
    if !auth.verify_session(req.headers()) {
        let mut response =
            browser.session_error("Invalid administrator credentials or verification code");
        cookie(&mut response, "", 0);
        return no_store(response);
    }
    if req.uri().path() == "/api/v1/admin/session" && req.method() == "GET" {
        let Some(expires_at) = verified_expiry(&token) else {
            return browser.session_error("Invalid session");
        };
        return no_store(json_response(
            StatusCode::OK,
            &serde_json::json!({
                "success": true, "authenticated": true, "role": "admin", "csrfToken": csrf_token(req.headers()),
                "expiresAt": expires_at, "expiresIn": expires_at.saturating_sub(now_secs()),
                "twoFactorEnabled": browser.totp.is_some(), "totpRequired": browser.totp.is_some()
            }),
        ));
    }
    if mutation {
        let supplied = req
            .headers()
            .get("x-csrf-token")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if !crate::security::ct_eq(supplied.as_bytes(), csrf_token(req.headers()).as_bytes()) {
            return no_store(reply(StatusCode::FORBIDDEN, "Invalid CSRF token"));
        }
    }
    let logout = req.uri().path() == "/api/v1/admin/session/revoke" && req.method() == "POST";
    let mut response = next.run(req).await;
    if logout && response.status().is_success() {
        cookie(&mut response, "", 0);
    }
    no_store(response)
}

fn session_cookie(headers: &HeaderMap) -> Option<String> {
    let mut values = headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|v| v.trim().split_once('='))
        .filter(|(name, _)| *name == COOKIE)
        .map(|(_, value)| value.to_owned());
    let value = values.next()?;
    if values.next().is_some() || value.is_empty() {
        return None;
    }
    Some(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, middleware, routing::any, Router};
    use tower::ServiceExt;

    // Published RFC 6238 test fixture, never a deployment credential.
    const FIXTURE: &str = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ";

    #[test]
    fn totp_rfc_vectors_window_replay_and_restart() {
        let totp = Totp::new(FIXTURE, 0).unwrap();
        for (time, expected) in [
            (59, "287082"),
            (1111111109, "081804"),
            (1111111111, "050471"),
            (1234567890, "005924"),
            (2000000000, "279037"),
            (20000000000, "353130"),
        ] {
            assert_eq!(totp.code(time / 30), expected);
        }
        for offset in [-1i64, 0, 1] {
            let totp = Totp::new(FIXTURE, 0).unwrap();
            let code = totp.code((100 + offset) as u64);
            assert!(totp.verify(&code, 3000));
            assert!(!totp.verify(&code, 3000));
        }
        let totp = Totp::new(FIXTURE, 0).unwrap();
        for step in [98, 102] {
            assert!(!totp.verify(&totp.code(step), 3000));
        }
        for code in ["", "12345", "1234567", "abcdef", " 12345"] {
            assert!(!totp.verify(code, 3000));
        }
        assert!(totp.verify(&totp.code(101), 3000));
        assert!(!totp.verify(&totp.code(100), 3000));
        let restarted = Totp::new(FIXTURE, 3000).unwrap();
        assert!(!restarted.verify(&restarted.code(101), 3030));
        assert!(restarted.verify(&restarted.code(102), 3060));
        for invalid in [
            "",
            "short",
            "gez dgnbv",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        ] {
            assert!(Totp::new(invalid, 0).is_err());
        }
    }

    #[test]
    fn totp_concurrent_replay_is_atomic() {
        let totp = Arc::new(Totp::new(FIXTURE, 0).unwrap());
        let threads: Vec<_> = (0..12)
            .map(|_| {
                let totp = totp.clone();
                std::thread::spawn(move || totp.verify(&totp.code(100), 3000))
            })
            .collect();
        assert_eq!(
            threads
                .into_iter()
                .filter_map(|t| t.join().ok())
                .filter(|v| *v)
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn totp_login_contract_and_failures() {
        let browser = BrowserAuth {
            totp: Some(Arc::new(Totp::new(FIXTURE, 0).unwrap())),
            ..BrowserAuth::new(
                "https://admin.test".into(),
                bcrypt::hash("test-password", 4).unwrap(),
            )
            .unwrap()
        };
        let code = browser.totp.as_ref().unwrap().code(now_secs() / 30);
        let app = app_with_browser(browser);
        let anonymous = app
            .clone()
            .oneshot(request("GET", "session", None, None, None, ""))
            .await
            .unwrap();
        assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED);
        let body: serde_json::Value =
            serde_json::from_slice(&to_bytes(anonymous.into_body(), 4096).await.unwrap()).unwrap();
        assert_eq!(body["totpRequired"], true);
        assert_eq!(body["authenticated"], false);
        let login = |password: &str, code: Option<&str>| {
            request(
                "POST",
                "session",
                None,
                None,
                Some("https://admin.test"),
                &serde_json::json!({"username":"admin", "password":password, "totpCode":code})
                    .to_string(),
            )
        };
        for (password, supplied) in [
            ("wrong", Some(code.as_str())),
            ("test-password", None),
            ("test-password", Some("bad")),
        ] {
            let response = app
                .clone()
                .oneshot(login(password, supplied))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
            assert!(!response.headers().contains_key(header::SET_COOKIE));
        }
        let response = app
            .clone()
            .oneshot(login("test-password", Some(&code)))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let cookie_header = response.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .to_owned();
        let body: serde_json::Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
        assert_eq!(body["twoFactorEnabled"], true);
        let expires = body["expiresAt"].as_u64().unwrap();
        assert!((now_secs() + 898..=now_secs() + 900).contains(&expires));
        let session = app
            .clone()
            .oneshot(request(
                "GET",
                "session",
                cookie_header.split(';').next(),
                None,
                None,
                "",
            ))
            .await
            .unwrap();
        assert_eq!(session.status(), StatusCode::OK);
        let body: serde_json::Value =
            serde_json::from_slice(&to_bytes(session.into_body(), 4096).await.unwrap()).unwrap();
        assert_eq!(body["expiresAt"], expires);
        assert!(body["expiresIn"].as_u64().unwrap() <= 900);
        for _ in 0..6 {
            assert_eq!(
                app.clone()
                    .oneshot(login("test-password", Some(&code)))
                    .await
                    .unwrap()
                    .status(),
                StatusCode::UNAUTHORIZED
            );
        }
        assert_eq!(
            app.oneshot(login("test-password", Some(&code)))
                .await
                .unwrap()
                .status(),
            StatusCode::TOO_MANY_REQUESTS
        );
    }

    #[tokio::test]
    async fn session_deadline_is_actual_non_sliding_and_expired_is_cleared() {
        let auth = AdminAuthState::new("test-signing-key");
        let session = auth.issue_session(60).unwrap();
        let browser = BrowserAuth::new(
            "https://admin.test".into(),
            bcrypt::hash("test-password", 4).unwrap(),
        )
        .unwrap();
        let app = app_with_auth(browser, auth);
        let cookie = format!("{COOKIE}={}", session.access_token);
        let mut remaining = 0;
        for attempt in 0..2 {
            let response = app
                .clone()
                .oneshot(request("GET", "session", Some(&cookie), None, None, ""))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert!(!response.headers().contains_key(header::SET_COOKIE));
            let body: serde_json::Value =
                serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap())
                    .unwrap();
            assert_eq!(body["expiresAt"], session.expires_at);
            let ttl = body["expiresIn"].as_u64().unwrap();
            assert!(ttl <= 60);
            if attempt == 1 {
                assert!(ttl < remaining);
            }
            remaining = ttl;
            if attempt == 0 {
                tokio::time::sleep(Duration::from_millis(1100)).await;
            }
        }
        // Signed with the fixture key and existing jti; expiry is strictly enforced,
        // independently of JWT library leeway and the server-side session entry.
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(session.access_token.split('.').nth(1).unwrap())
            .unwrap();
        let mut claims: serde_json::Value = serde_json::from_slice(&payload).unwrap();
        claims["exp"] = serde_json::json!(now_secs() - 1);
        let expired = jsonwebtoken::encode(
            &jsonwebtoken::Header::default(),
            &claims,
            &jsonwebtoken::EncodingKey::from_secret(b"test-signing-key"),
        )
        .unwrap();
        let response = app
            .oneshot(request(
                "GET",
                "session",
                Some(&format!("{COOKIE}={expired}")),
                None,
                None,
                "",
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(response.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .contains("Max-Age=0"));
        let body: serde_json::Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
        assert_eq!(body["authenticated"], false);
        assert!(body.get("expiresAt").is_none());
        assert!(body.get("csrfToken").is_none());
    }

    fn app() -> Router {
        let browser = BrowserAuth::new(
            "https://admin.test".into(),
            bcrypt::hash("test-password", 4).unwrap(),
        )
        .unwrap();
        app_with_browser(browser)
    }

    fn app_with_browser(browser: BrowserAuth) -> Router {
        app_with_auth(browser, AdminAuthState::new("test-signing-key"))
    }

    fn app_with_auth(browser: BrowserAuth, auth: AdminAuthState) -> Router {
        Router::new()
            .route(
                "/api/v1/admin/*path",
                any(|req: Request| async move {
                    json_response(
                        StatusCode::OK,
                        &serde_json::json!({"csrfToken": csrf_token(req.headers())}),
                    )
                }),
            )
            .layer(middleware::from_fn(move |req, next| {
                let auth = auth.clone();
                let browser = browser.clone();
                async move { handle(&auth, &browser, req, next).await }
            }))
    }

    fn request(
        method: &str,
        path: &str,
        cookie: Option<&str>,
        csrf: Option<&str>,
        origin: Option<&str>,
        body: &str,
    ) -> Request {
        let mut builder = Request::builder()
            .method(method)
            .uri(format!("/api/v1/admin/{path}"));
        if let Some(value) = cookie {
            builder = builder.header(header::COOKIE, value);
        }
        if let Some(value) = csrf {
            builder = builder.header("x-csrf-token", value);
        }
        if let Some(value) = origin {
            builder = builder.header(header::ORIGIN, value);
        }
        builder
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_owned()))
            .unwrap()
    }

    #[tokio::test]
    async fn browser_cookie_origin_csrf_and_rotation() {
        let app = app();
        let login = || {
            request(
                "POST",
                "session",
                None,
                None,
                Some("https://admin.test"),
                r#"{"username":"admin","password":"test-password"}"#,
            )
        };
        let anonymous = app
            .clone()
            .oneshot(request("GET", "me", None, None, None, ""))
            .await
            .unwrap();
        assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED);
        assert!(!anonymous.headers().contains_key(header::WWW_AUTHENTICATE));
        let response = app.clone().oneshot(login()).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        let set_cookie = response.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .to_owned();
        for flag in [
            "HttpOnly",
            "Secure",
            "SameSite=Strict",
            "Path=/",
            "Max-Age=900",
        ] {
            assert!(set_cookie.contains(flag));
        }
        let cookie = set_cookie.split(';').next().unwrap();
        let body = to_bytes(response.into_body(), 4096).await.unwrap();
        assert!(!String::from_utf8_lossy(&body).contains("accessToken"));
        let me = app
            .clone()
            .oneshot(request("GET", "me", Some(cookie), None, None, ""))
            .await
            .unwrap();
        assert_eq!(me.status(), StatusCode::OK);
        let json: serde_json::Value =
            serde_json::from_slice(&to_bytes(me.into_body(), 4096).await.unwrap()).unwrap();
        let csrf = json["csrfToken"].as_str().unwrap();
        for (origin, token, expected) in [
            (Some("https://evil.test"), Some(csrf), StatusCode::FORBIDDEN),
            (None, Some(csrf), StatusCode::FORBIDDEN),
            (Some("https://admin.test"), None, StatusCode::FORBIDDEN),
            (Some("https://admin.test"), Some(csrf), StatusCode::OK),
        ] {
            assert_eq!(
                app.clone()
                    .oneshot(request(
                        "POST",
                        "cards/reveal",
                        Some(cookie),
                        token,
                        origin,
                        "{}"
                    ))
                    .await
                    .unwrap()
                    .status(),
                expected
            );
        }
        let mut rotate = login();
        rotate
            .headers_mut()
            .insert(header::COOKIE, cookie.parse().unwrap());
        assert_eq!(
            app.clone().oneshot(rotate).await.unwrap().status(),
            StatusCode::OK
        );
        assert_eq!(
            app.oneshot(request("GET", "me", Some(cookie), None, None, ""))
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn login_budget_and_invalid_credentials() {
        let app = app();
        for _ in 0..10 {
            let response = app
                .clone()
                .oneshot(request(
                    "POST",
                    "session",
                    None,
                    None,
                    Some("https://admin.test"),
                    r#"{"username":"admin","password":"wrong"}"#,
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        }
        let response = app
            .oneshot(request(
                "POST",
                "session",
                None,
                None,
                Some("https://admin.test"),
                "{}",
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(response.headers()[header::RETRY_AFTER], "60");
    }

    #[test]
    fn duplicate_cookies_and_invalid_configuration() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            format!("{COOKIE}=one; {COOKIE}=two").parse().unwrap(),
        );
        assert!(session_cookie(&headers).is_none());
        let hash = bcrypt::hash("test", 4).unwrap();
        for origin in [
            "http://admin.test",
            "https://admin.test/",
            "https://admin.test/path",
        ] {
            assert!(BrowserAuth::new(origin.into(), hash.clone()).is_err());
        }
    }
}

#[cfg(test)]
#[path = "../../tests/admin_login_security/audit.rs"]
mod audit_login_security;
