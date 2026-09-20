//! Security enhancements for Kiro BYOK Gateway.
//!
//! Spec §4.1 (Login Brute-force Protection), Spec §7 (Security Architecture, Body Limits, Dual-Port Listener Separation).

use axum::{
    body::Body,
    extract::ConnectInfo,
    http::{Extensions, HeaderMap, Request, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Router,
};
use ring::rand::SecureRandom;
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, RwLock};
use thiserror::Error;

/// Constant-time byte comparison to prevent timing side-channel attacks on secrets.
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let diff = a
        .iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y));
    diff == 0
}

pub(crate) fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

/// Return the client address only after proving that the connected peer is a
/// configured trusted proxy. Forwarded headers are otherwise attacker input.
pub fn client_ip(req: &Request<Body>) -> String {
    client_ip_parts(req.extensions(), req.headers())
}

pub fn client_ip_parts(extensions: &Extensions, headers: &HeaderMap) -> String {
    let peer = extensions
        .get::<ConnectInfo<SocketAddr>>()
        .map(|info| info.0.ip());
    if let Some(peer_ip) = peer {
        if is_trusted_proxy(peer_ip) {
            if let Some(value) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
                let chain: Vec<IpAddr> = value
                    .split(',')
                    .filter_map(|part| part.trim().parse::<IpAddr>().ok())
                    .collect();
                // Walk from the trusted edge toward the client.  The first
                // address outside the configured proxy networks is the
                // right-most untrusted hop; taking the first header element
                // is spoofable when multiple proxies are present.
                if let Some(client) = chain
                    .iter()
                    .rev()
                    .find(|candidate| !is_trusted_proxy(**candidate))
                    .or_else(|| chain.first())
                {
                    return client.to_string();
                }
            }
            if let Some(real_ip) = headers
                .get("x-real-ip")
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.trim().parse::<IpAddr>().ok())
            {
                return real_ip.to_string();
            }
        }
        return peer_ip.to_string();
    }
    "unknown".to_string()
}

pub fn validate_trusted_proxy_config() -> Result<(), String> {
    if proxy_headers_enabled() && trusted_proxy_networks().is_empty() {
        return Err(
            "TRUSTED_PROXY_HEADERS=true requires TRUSTED_PROXY_CIDRS; refusing to trust forged forwarding headers"
                .to_string(),
        );
    }
    Ok(())
}

fn proxy_headers_enabled() -> bool {
    std::env::var("TRUSTED_PROXY_HEADERS")
        .map(|value| value.eq_ignore_ascii_case("true") || value == "1")
        .unwrap_or(false)
}

fn trusted_proxy_networks() -> Vec<(IpAddr, u8)> {
    std::env::var("TRUSTED_PROXY_CIDRS")
        .unwrap_or_default()
        .split(',')
        .filter_map(|entry| {
            let mut parts = entry.trim().splitn(2, '/');
            let ip = parts.next()?.parse().ok()?;
            let prefix = parts
                .next()
                .and_then(|value| value.parse::<u8>().ok())
                .unwrap_or(match ip {
                    IpAddr::V4(_) => 32,
                    IpAddr::V6(_) => 128,
                });
            let max = if ip.is_ipv4() { 32 } else { 128 };
            (prefix <= max).then_some((ip, prefix))
        })
        .collect()
}

fn is_trusted_proxy(ip: IpAddr) -> bool {
    if !proxy_headers_enabled() {
        return false;
    }
    trusted_proxy_networks()
        .iter()
        .any(|(network, prefix)| ip_in_network(ip, *network, *prefix))
}

fn ip_in_network(ip: IpAddr, network: IpAddr, prefix: u8) -> bool {
    match (ip, network) {
        (IpAddr::V4(ip), IpAddr::V4(network)) => {
            let mask = if prefix == 0 {
                0
            } else {
                u32::MAX << (32 - prefix)
            };
            u32::from(ip) & mask == u32::from(network) & mask
        }
        (IpAddr::V6(ip), IpAddr::V6(network)) => {
            let ip = u128::from(ip);
            let network = u128::from(network);
            let mask = if prefix == 0 {
                0
            } else {
                u128::MAX << (128 - prefix)
            };
            ip & mask == network & mask
        }
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// 1. Login Brute-Force Protection (Spec §4.1, §7)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BruteForceConfig {
    /// Maximum allowable consecutive failed login attempts within window.
    pub max_failures: u32,
    /// Sliding window duration in seconds.
    pub window_secs: u64,
    /// Lockout duration in seconds once threshold is breached.
    pub lockout_secs: u64,
}

impl Default for BruteForceConfig {
    fn default() -> Self {
        Self {
            max_failures: 5,
            window_secs: 300,  // 5 minutes
            lockout_secs: 900, // 15 minutes
        }
    }
}

#[derive(Debug, Clone)]
struct AttemptRecord {
    failure_count: u32,
    first_failure_at: u64,
    locked_until: Option<u64>,
}

#[derive(Error, Debug, PartialEq, Eq)]
pub enum BruteForceError {
    #[error("Account temporarily locked due to excessive failed attempts. Try again in {remaining_secs} seconds.")]
    LockedOut { remaining_secs: u64 },
}

/// In-memory thread-safe login brute-force protector.
#[derive(Debug, Clone, Default)]
pub struct BruteForceProtector {
    config: BruteForceConfig,
    records: Arc<RwLock<HashMap<String, AttemptRecord>>>,
}

impl BruteForceProtector {
    pub fn new(config: BruteForceConfig) -> Self {
        Self {
            config,
            records: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Check whether an identifier (IP or device fingerprint) is currently locked out.
    pub fn check_lockout(&self, identifier: &str, now_secs: u64) -> Result<(), BruteForceError> {
        let records = self.records.read().unwrap();
        if let Some(record) = records.get(identifier) {
            if let Some(locked_until) = record.locked_until {
                if now_secs < locked_until {
                    let remaining_secs = locked_until.saturating_sub(now_secs);
                    return Err(BruteForceError::LockedOut { remaining_secs });
                }
            }
        }
        Ok(())
    }

    /// Record a failed login attempt and return whether the identifier was locked out.
    pub fn record_failure(&self, identifier: &str, now_secs: u64) -> Result<(), BruteForceError> {
        let mut records = self.records.write().unwrap();
        // Drop inactive sources instead of retaining failed attempts for process lifetime.
        // Active lockouts must survive expiry of the shorter counting window.
        records.retain(|_, record| match record.locked_until {
            Some(until) => until > now_secs,
            None => now_secs.saturating_sub(record.first_failure_at) <= self.config.window_secs,
        });
        let entry = records
            .entry(identifier.to_string())
            .or_insert_with(|| AttemptRecord {
                failure_count: 0,
                first_failure_at: now_secs,
                locked_until: None,
            });

        // If currently locked and still in lockout period
        if let Some(locked_until) = entry.locked_until {
            if now_secs < locked_until {
                let remaining_secs = locked_until.saturating_sub(now_secs);
                return Err(BruteForceError::LockedOut { remaining_secs });
            } else {
                // Lockout expired, reset counter for new window
                entry.failure_count = 0;
                entry.first_failure_at = now_secs;
                entry.locked_until = None;
            }
        }

        // Check if previous window expired
        if now_secs.saturating_sub(entry.first_failure_at) > self.config.window_secs {
            entry.failure_count = 1;
            entry.first_failure_at = now_secs;
            entry.locked_until = None;
        } else {
            entry.failure_count += 1;
        }

        if entry.failure_count >= self.config.max_failures {
            let locked_until = now_secs + self.config.lockout_secs;
            entry.locked_until = Some(locked_until);
            return Err(BruteForceError::LockedOut {
                remaining_secs: self.config.lockout_secs,
            });
        }

        Ok(())
    }

    /// Reset failure counts on successful login.
    pub fn record_success(&self, identifier: &str) {
        let mut records = self.records.write().unwrap();
        records.remove(identifier);
    }
}

// ---------------------------------------------------------------------------
// 2. Request Body Limit & Content Guardrails (Spec §7)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContentGuardrailConfig {
    /// Maximum overall HTTP request body size (default: 10MB).
    pub max_body_bytes: usize,
    /// Maximum number of images allowed in a single conversation turn (default: 20).
    pub max_images_per_request: usize,
    /// Maximum size of an individual image before compression (Spec §4.3: 400KB).
    pub max_image_bytes: usize,
    /// Maximum prompt/context characters allowed (default: 2,000,000).
    pub max_prompt_chars: usize,
}

impl Default for ContentGuardrailConfig {
    fn default() -> Self {
        Self {
            max_body_bytes: 10 * 1024 * 1024, // 10 MB
            max_images_per_request: 20,
            max_image_bytes: 400 * 1024, // 400 KB
            max_prompt_chars: 2_000_000,
        }
    }
}

#[derive(Error, Debug, PartialEq, Eq)]
pub enum GuardrailError {
    #[error("Request body too large: {actual} bytes exceeds limit of {max} bytes")]
    BodyTooLarge { actual: usize, max: usize },

    #[error("Too many images: {actual} exceeds maximum allowed {max}")]
    TooManyImages { actual: usize, max: usize },

    #[error("Image too large: {actual} bytes exceeds individual limit of {max} bytes")]
    ImageTooLarge { actual: usize, max: usize },

    #[error("Prompt content too long: {actual} characters exceeds limit of {max}")]
    PromptTooLong { actual: usize, max: usize },

    #[error("Image payload is not valid base64")]
    InvalidImage,
}

impl ContentGuardrailConfig {
    /// Validate prompt length and image parameters against guardrail constraints.
    pub fn validate_payload(
        &self,
        prompt_chars: usize,
        images: &[usize], // slice of image sizes in bytes
    ) -> Result<(), GuardrailError> {
        if prompt_chars > self.max_prompt_chars {
            return Err(GuardrailError::PromptTooLong {
                actual: prompt_chars,
                max: self.max_prompt_chars,
            });
        }

        if images.len() > self.max_images_per_request {
            return Err(GuardrailError::TooManyImages {
                actual: images.len(),
                max: self.max_images_per_request,
            });
        }

        for &img_size in images {
            if img_size > self.max_image_bytes {
                return Err(GuardrailError::ImageTooLarge {
                    actual: img_size,
                    max: self.max_image_bytes,
                });
            }
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// 3. Admin & Kiro Dual-Listener Separation (Spec §7)
// ---------------------------------------------------------------------------

/// Configuration for separate Kiro facade and Admin listeners.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DualServerConfig {
    /// Public listening socket for Kiro IDE facade (e.g. 0.0.0.0:8080).
    pub kiro_listen_addr: SocketAddr,
    /// Private / internal listening socket for Admin API (e.g. 127.0.0.1:9090).
    pub admin_listen_addr: SocketAddr,
}

impl Default for DualServerConfig {
    fn default() -> Self {
        Self {
            kiro_listen_addr: "0.0.0.0:8080".parse().unwrap(),
            admin_listen_addr: "127.0.0.1:9090".parse().unwrap(),
        }
    }
}

/// Router builder that enforces strict boundary isolation between Kiro and Admin routes.
pub struct DualRouterBuilder;

impl DualRouterBuilder {
    /// Build public Kiro Facade Router.
    ///
    /// Any route under `/admin/*` will return HTTP 404/403 to prevent public access.
    pub fn build_kiro_router(facade_router: Router) -> Router {
        facade_router
            .route("/admin", get(reject_admin_on_kiro_port))
            .route(
                "/admin/*path",
                get(reject_admin_on_kiro_port).post(reject_admin_on_kiro_port),
            )
    }

    /// Build internal Admin Router.
    ///
    /// Exclusively serves `/admin/*` management routes; does NOT mount Kiro endpoints.
    pub fn build_admin_router(admin_router: Router) -> Router {
        admin_router.route(
            "/generateAssistantResponse",
            post(reject_kiro_on_admin_port),
        )
    }
}

async fn reject_admin_on_kiro_port() -> Response {
    (
        StatusCode::NOT_FOUND,
        "Admin endpoints are not exposed on Kiro facade port",
    )
        .into_response()
}

async fn reject_kiro_on_admin_port() -> Response {
    (
        StatusCode::NOT_FOUND,
        "Kiro facade endpoints are not exposed on Admin management port",
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// 4. IP-based Rate Limiter for Portal / Public Endpoints (P1-01)
// ---------------------------------------------------------------------------

/// Simple sliding-window IP rate limiter.
/// ponytail: HashMap + RwLock, O(n) cleanup; upgrade to dashmap + token bucket
/// if portal traffic exceeds ~1k concurrent IPs.
#[derive(Debug, Clone)]
pub struct IpRateLimiter {
    /// Max requests per window.
    max_requests: u32,
    /// Window duration in seconds.
    window_secs: u64,
    /// IP -> (count, window_start)
    counters: Arc<RwLock<HashMap<String, (u32, u64)>>>,
}

impl Default for IpRateLimiter {
    fn default() -> Self {
        Self::new(30, 60) // 30 requests per 60 seconds per IP
    }
}

impl IpRateLimiter {
    pub fn new(max_requests: u32, window_secs: u64) -> Self {
        Self {
            max_requests: max_requests.max(1),
            window_secs: window_secs.max(1),
            counters: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Check and consume a request slot for the given IP.
    /// Returns Ok(()) if allowed, Err(remaining_secs) if rate limited.
    pub fn check_rate_limit(&self, ip: &str, now_secs: u64) -> Result<(), u64> {
        let mut counters = self.counters.write().unwrap();
        counters.retain(|_, (_, started)| now_secs.saturating_sub(*started) < self.window_secs);
        if counters.len() >= 100_000 && !counters.contains_key(ip) {
            if let Some(oldest) = counters
                .iter()
                .min_by_key(|(_, (_, started))| *started)
                .map(|(key, _)| key.clone())
            {
                counters.remove(&oldest);
            }
        }
        let entry = counters.entry(ip.to_string()).or_insert((0, now_secs));

        // Reset window if expired
        if now_secs.saturating_sub(entry.1) >= self.window_secs {
            entry.0 = 0;
            entry.1 = now_secs;
        }

        if entry.0 >= self.max_requests {
            let retry_after = self
                .window_secs
                .saturating_sub(now_secs.saturating_sub(entry.1));
            return Err(retry_after.max(1));
        }

        entry.0 = entry.0.saturating_add(1);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// 5. One-Time Ephemeral Challenge Manager for Portal Mutations (P1-01)
// ---------------------------------------------------------------------------

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct ChallengeClaims {
    action: String,
    ip: String,
    nonce: String,
    exp: u64,
}

/// Ephemeral challenge manager for anti-replay and anti-bot validation on sensitive portal operations.
#[derive(Debug, Clone)]
pub struct PortalChallengeManager {
    secret: String,
    used_nonces: Arc<RwLock<HashMap<String, u64>>>,
    ttl_secs: u64,
}

impl Default for PortalChallengeManager {
    fn default() -> Self {
        let mut secret_bytes = [0u8; 32];
        let secret = if ring::rand::SystemRandom::new()
            .fill(&mut secret_bytes)
            .is_ok()
        {
            format!(
                "kiro-portal-chal-{}",
                crate::security::hex_encode(&secret_bytes)
            )
        } else {
            // Startup should still be deterministic in an entropy failure; a
            // production caller can provide a configured secret through new().
            "kiro-portal-chal-invalid-entropy".to_string()
        };
        Self {
            secret,
            used_nonces: Arc::new(RwLock::new(HashMap::new())),
            ttl_secs: 120, // 2 minutes
        }
    }
}

impl PortalChallengeManager {
    pub fn new(secret: impl Into<String>, ttl_secs: u64) -> Self {
        Self {
            secret: secret.into(),
            used_nonces: Arc::new(RwLock::new(HashMap::new())),
            ttl_secs,
        }
    }

    /// Issue a signed challenge token for a specific action and client IP.
    pub fn issue_challenge(&self, action: &str, ip: &str, now_secs: u64) -> Result<String, String> {
        if action.trim().is_empty() || action.chars().count() > 64 || ip.chars().count() > 128 {
            return Err("invalid challenge parameters".to_string());
        }
        let mut nonce_bytes = [0u8; 32];
        ring::rand::SystemRandom::new()
            .fill(&mut nonce_bytes)
            .map_err(|_| "secure random generator unavailable".to_string())?;
        let nonce = crate::security::hex_encode(&nonce_bytes);
        let claims = ChallengeClaims {
            action: action.to_string(),
            ip: ip.to_string(),
            nonce,
            exp: now_secs.saturating_add(self.ttl_secs),
        };
        jsonwebtoken::encode(
            &jsonwebtoken::Header::default(),
            &claims,
            &jsonwebtoken::EncodingKey::from_secret(self.secret.as_bytes()),
        )
        .map_err(|e| e.to_string())
    }

    /// Verify challenge token: checks signature, action, IP, expiration, and prevents replay.
    pub fn verify_and_consume(
        &self,
        token: &str,
        expected_action: &str,
        ip: &str,
        now_secs: u64,
    ) -> Result<(), &'static str> {
        let mut validation = jsonwebtoken::Validation::default();
        validation.validate_exp = true;

        let data = jsonwebtoken::decode::<ChallengeClaims>(
            token,
            &jsonwebtoken::DecodingKey::from_secret(self.secret.as_bytes()),
            &validation,
        )
        .map_err(|_| "Invalid or expired challenge token")?;

        let claims = data.claims;
        // jsonwebtoken applies a default expiration leeway. A consumed nonce is
        // pruned at exact expiry, so accepting that leeway would permit replay.
        if claims.exp <= now_secs {
            return Err("Invalid or expired challenge token");
        }
        if claims.action != expected_action {
            return Err("Challenge token action mismatch");
        }
        if claims.ip != ip {
            return Err("Challenge token IP mismatch");
        }

        let mut nonces = self.used_nonces.write().unwrap();
        // Prune expired nonces
        nonces.retain(|_, exp| *exp > now_secs);
        if nonces.contains_key(&claims.nonce) {
            return Err("Challenge token already used (replay attack)");
        }
        nonces.insert(claims.nonce, claims.exp);
        Ok(())
    }
}

#[cfg(test)]
mod brute_force_cleanup_tests {
    use super::*;

    #[test]
    fn expired_sources_are_pruned_without_dropping_active_lockouts() {
        let protector = BruteForceProtector::new(BruteForceConfig {
            max_failures: 2,
            window_secs: 60,
            lockout_secs: 300,
        });
        assert!(protector.record_failure("expired", 1000).is_ok());
        assert!(protector.record_failure("locked", 1000).is_ok());
        assert!(protector.record_failure("locked", 1000).is_err());
        assert!(protector.record_failure("new", 1100).is_ok());
        assert!(!protector.records.read().unwrap().contains_key("expired"));
        assert_eq!(
            protector.check_lockout("locked", 1100),
            Err(BruteForceError::LockedOut {
                remaining_secs: 200
            })
        );
        assert!(protector.record_failure("later", 1300).is_ok());
        assert_eq!(protector.records.read().unwrap().len(), 1);
        assert!(protector.check_lockout("locked", 1300).is_ok());
    }
}
