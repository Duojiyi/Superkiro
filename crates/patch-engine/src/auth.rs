//! Client authentication and token lifecycle handshake client (Spec §4.1, P0-4).
//!
//! Orchestrates the login handshake (card key + device fingerprint -> short-lived JWT + refresh token),
//! local atomic token storage, and automatic/manual token refresh.

use crate::device::generate_device_fingerprint;
use crate::token_storage::{KiroAuthToken, TokenStorage, TokenStorageError};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AuthClientError {
    #[error("HTTP client initialization failed: {0}")]
    Initialization(String),

    #[error("Network HTTP error: {}", network_error(.0))]
    Network(#[from] reqwest::Error),

    #[error("Storage error: {0}")]
    Storage(#[from] TokenStorageError),

    #[error("Authentication rejected: {status} - {message}")]
    AuthRejected {
        status: u16,
        code: &'static str,
        retry_after: Option<u64>,
        message: String,
    },

    #[error("Token expired and no refresh token available")]
    NoRefreshToken,

    #[error("Invalid server response format: {0}")]
    InvalidResponse(String),
}

impl AuthClientError {
    pub fn is_authorization_rejected(&self) -> bool {
        matches!(
            self,
            Self::AuthRejected {
                status: 401 | 403,
                ..
            }
        )
    }

    pub(crate) async fn from_response(response: reqwest::Response) -> Self {
        let status = response.status().as_u16();
        // Only bounded delta-seconds are retained; never echo a raw header/body.
        let retry_after = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|v| *v <= 86400);
        let value = crate::http::bounded_json(response, 65536)
            .await
            .unwrap_or_default();
        let category = value["code"]
            .as_str()
            .and_then(auth_category)
            .or_else(|| value["__type"].as_str().and_then(auth_category));
        let (code, text) = category.unwrap_or(match status {
            429 => ("throttled", "Too many requests; retry later"),
            500..=599 => ("server-error", "Service temporarily unavailable"),
            _ => ("auth-rejected", "Gateway rejected authorization"),
        });
        let retry_after = retry_after.or_else(|| {
            (code == "rebind-cooldown")
                .then(|| value["retryAfterSecs"].as_u64().filter(|v| *v <= 86400))
                .flatten()
        });
        let retry = retry_after
            .map(|seconds| format!(" [retry-after:{seconds}]"))
            .unwrap_or_default();
        Self::AuthRejected {
            status,
            code,
            retry_after,
            message: format!("[auth:{code}] {text}{retry}"),
        }
    }
}

fn auth_category(code: &str) -> Option<(&'static str, &'static str)> {
    Some(match code {
        "UnrecognizedClientException" | "InvalidTokenException" | "invalid-card" => {
            ("invalid-card", "Card or token is invalid")
        }
        "AccessDeniedException" | "access-denied" => ("access-denied", "Authorization is denied"),
        "ExpiredTokenException" | "expired" => ("expired", "Authorization has expired"),
        "DeviceBindingException" | "device-binding" => {
            ("device-binding", "Device binding requires attention")
        }
        "rebind_cooldown" => ("rebind-cooldown", "Device rebind cooldown; retry later"),
        "rebind_limit_exceeded" => (
            "rebind-limit",
            "Device rebind limit reached; contact support",
        ),
        "ThrottlingException" | "throttled" => ("throttled", "Too many requests; retry later"),
        "LockoutException" | "locked-out" => (
            "locked-out",
            "Authentication temporarily locked; retry later",
        ),
        "SerializationException" | "InvalidRequestException" | "invalid-request" => {
            ("invalid-request", "Authentication request is invalid")
        }
        "InternalServerException" | "TokenGenerationException" | "server-error" => {
            ("server-error", "Service temporarily unavailable")
        }
        _ => return None,
    })
}

/// Request body sent to gateway `/oauth/token` endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientLoginRequest {
    pub card_key: String,
    pub device_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_version: Option<String>,
}

/// Request body sent to gateway `/refreshToken` endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientRefreshRequest {
    pub refresh_token: String,
}

/// Gateway response for `/oauth/token`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayOAuthResponse {
    pub access_token: String,
    pub refresh_token: String,
    pub profile_arn: String,
    pub expires_at: String,
    #[serde(default = "default_auth_method")]
    pub auth_method: String,
    #[serde(default = "default_provider")]
    pub provider: String,
}

fn default_auth_method() -> String {
    "social".to_string()
}

fn default_provider() -> String {
    "Google".to_string()
}

/// Gateway response for `/refreshToken`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayRefreshResponse {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub profile_arn: Option<String>,
    pub expires_at: String,
}

/// High-level authentication client for Kiro BYOK desktop/CLI.
#[derive(Debug, Clone)]
pub struct AuthClient {
    http: Result<Client, String>,
    storage: TokenStorage,
}

impl Default for AuthClient {
    fn default() -> Self {
        Self::new(TokenStorage::default())
    }
}

impl AuthClient {
    /// Create an AuthClient with custom TokenStorage (e.g. for isolated testing).
    /// Initialization failures are returned by HTTP operations; construction never falls back
    /// to a client that ignores the configured CA.
    pub fn new(storage: TokenStorage) -> Self {
        Self::with_ca(
            storage,
            std::env::var_os("KIRO_GATEWAY_CA_CERT")
                .as_deref()
                .map(std::path::Path::new),
        )
    }

    /// Explicit session trust, independent of the environment after restart.
    pub fn with_ca(storage: TokenStorage, ca: Option<&std::path::Path>) -> Self {
        let http = crate::http::client_builder_with_ca(ca).and_then(|builder| {
            builder
                .timeout(Duration::from_secs(15))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|e| e.to_string())
        });

        Self { http, storage }
    }

    fn http(&self) -> Result<&Client, AuthClientError> {
        self.http
            .as_ref()
            .map_err(|e| AuthClientError::Initialization(e.clone()))
    }

    /// Access reference to underlying token storage.
    pub fn storage(&self) -> &TokenStorage {
        &self.storage
    }

    /// Perform login handshake with gateway using card key and device fingerprint.
    ///
    /// If `device_id` is None, automatically generates a stable hardware fingerprint.
    /// Upon successful authentication, writes `kiro-auth-token.json` atomically.
    pub async fn login(
        &self,
        gateway_base_url: &str,
        card_key: &str,
        device_id: Option<&str>,
    ) -> Result<KiroAuthToken, AuthClientError> {
        let token = self
            .authenticate(gateway_base_url, card_key, device_id)
            .await?;
        self.storage.save(&token)?;
        Ok(token)
    }

    /// Validate a card without touching IDE credentials. Safe before asking IDE to close.
    pub async fn authenticate(
        &self,
        gateway_base_url: &str,
        card_key: &str,
        device_id: Option<&str>,
    ) -> Result<KiroAuthToken, AuthClientError> {
        crate::patch::validate_gateway_url(gateway_base_url)
            .map_err(|e| AuthClientError::InvalidResponse(e.to_string()))?;
        if card_key.trim().is_empty() || card_key.chars().count() > 256 {
            return Err(AuthClientError::InvalidResponse(
                "Card key must contain 1..256 characters".into(),
            ));
        }
        let dev_id = match device_id {
            Some(id) => id.to_string(),
            None => generate_device_fingerprint(None),
        };

        let req_body = ClientLoginRequest {
            card_key: card_key.to_string(),
            device_id: dev_id,
            client_version: Some(env!("CARGO_PKG_VERSION").to_string()),
        };

        let url = format!("{}/oauth/token", gateway_base_url.trim_end_matches('/'));

        let http = self.http()?;
        let request = || http.post(&url).json(&req_body).send();
        // A sign-in cut off before any answer (a local proxy dropping the TLS handshake,
        // say) is sent once more. That is safe even if the first one reached the gateway:
        // it binds the same device, bound to this card by then, which the gateway accepts
        // as it is, and it only replaces tokens that never arrived. An answer, whatever it
        // says, is final.
        let resp = match request().await {
            Err(error) if unanswered(&error) => {
                tokio::time::sleep(LOGIN_RETRY_DELAY).await;
                request().await?
            }
            result => result?,
        };

        if !resp.status().is_success() {
            return Err(AuthClientError::from_response(resp).await);
        }

        let oauth_resp: GatewayOAuthResponse = resp
            .json()
            .await
            .map_err(|e| AuthClientError::InvalidResponse(e.to_string()))?;

        let token = KiroAuthToken {
            access_token: oauth_resp.access_token,
            refresh_token: oauth_resp.refresh_token,
            profile_arn: oauth_resp.profile_arn,
            expires_at: oauth_resp.expires_at,
            auth_method: oauth_resp.auth_method,
            provider: oauth_resp.provider,
        };

        if token.access_token.is_empty()
            || token.refresh_token.is_empty()
            || token.profile_arn.is_empty()
            || crate::token_storage::parse_iso8601_to_epoch(&token.expires_at).is_none()
        {
            return Err(AuthClientError::InvalidResponse(
                "Missing or invalid token fields".into(),
            ));
        }
        if !crate::settings::in_redirected_region(&token.profile_arn) {
            return Err(AuthClientError::InvalidResponse(
                "The service issued a profile outside the redirected region".into(),
            ));
        }
        Ok(token)
    }

    /// Refresh current authentication token using the stored refresh token.
    ///
    /// Updates `kiro-auth-token.json` atomically upon success.
    pub async fn refresh(&self, gateway_base_url: &str) -> Result<KiroAuthToken, AuthClientError> {
        crate::patch::validate_gateway_url(gateway_base_url)
            .map_err(|e| AuthClientError::InvalidResponse(e.to_string()))?;
        let mut current_token = self.storage.load()?;

        if current_token.refresh_token.is_empty() {
            return Err(AuthClientError::NoRefreshToken);
        }

        let req_body = ClientRefreshRequest {
            refresh_token: current_token.refresh_token.clone(),
        };

        let url = format!("{}/refreshToken", gateway_base_url.trim_end_matches('/'));

        let resp = self.http()?.post(&url).json(&req_body).send().await?;

        if !resp.status().is_success() {
            return Err(AuthClientError::from_response(resp).await);
        }

        let refresh_resp: GatewayRefreshResponse = resp
            .json()
            .await
            .map_err(|e| AuthClientError::InvalidResponse(e.to_string()))?;

        current_token.access_token = refresh_resp.access_token;
        current_token.expires_at = refresh_resp.expires_at;

        if let Some(new_refresh) = refresh_resp.refresh_token {
            if !new_refresh.is_empty() {
                current_token.refresh_token = new_refresh;
            }
        }
        if let Some(new_profile) = refresh_resp.profile_arn {
            if !new_profile.is_empty() {
                // Saved, a profile outside the redirected region would send the gateway's
                // bearer to real Kiro on the next request.
                if !crate::settings::in_redirected_region(&new_profile) {
                    return Err(AuthClientError::InvalidResponse(
                        "The service issued a profile outside the redirected region".into(),
                    ));
                }
                current_token.profile_arn = new_profile;
            }
        }

        if current_token.access_token.is_empty()
            || crate::token_storage::parse_iso8601_to_epoch(&current_token.expires_at).is_none()
        {
            return Err(AuthClientError::InvalidResponse(
                "Missing or invalid refreshed token fields".into(),
            ));
        }
        // Atomically save updated token to disk
        self.storage.save(&current_token)?;

        Ok(current_token)
    }

    /// Portal device unbind requires a fresh, IP-bound one-time challenge.
    pub async fn unbind(
        &self,
        gateway: &str,
        card: &str,
        device: &str,
    ) -> Result<(), AuthClientError> {
        let gateway = crate::patch::validate_gateway_url(gateway)
            .map_err(|e| AuthClientError::InvalidResponse(e.to_string()))?;
        let response = self
            .http()?
            .post(format!("{gateway}/api/v1/portal/challenge"))
            .json(&serde_json::json!({"action": "unbind"}))
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(AuthClientError::from_response(response).await);
        }
        let challenge = crate::http::bounded_json(response, 65536)
            .await
            .map_err(AuthClientError::InvalidResponse)?;
        let token = challenge
            .get("challengeToken")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| AuthClientError::InvalidResponse("No unbind challenge".into()))?;
        let response = self
            .http()?
            .post(format!("{gateway}/api/v1/portal/unbind"))
            .json(&serde_json::json!({"card": card, "device": device, "challenge_token": token}))
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(AuthClientError::from_response(response).await);
        }
        let result = crate::http::bounded_json(response, 65536)
            .await
            .map_err(AuthClientError::InvalidResponse)?;
        if result.get("success").and_then(|v| v.as_bool()) != Some(true) {
            return Err(AuthClientError::InvalidResponse(
                "Gateway did not confirm device unbind".into(),
            ));
        }
        Ok(())
    }

    /// Logout and clean local token storage.
    pub fn logout(&self) -> Result<bool, AuthClientError> {
        Ok(self.storage.clear()?)
    }
}

/// How long a sign-in cut off before any answer waits before its one retry.
const LOGIN_RETRY_DELAY: Duration = Duration::from_millis(1500);

/// Whether a request failed before any answer came back: it never connected, its TLS
/// handshake or proxy tunnel was cut off, or the connection closed under it. Not a
/// timeout, which has already had all of its time.
fn unanswered(error: &reqwest::Error) -> bool {
    error.is_request() && !error.is_timeout()
}

// reqwest Display omits the transport/TLS cause. Keep its source chain for diagnostics.
fn network_error(error: &reqwest::Error) -> String {
    use std::error::Error;
    let mut message = String::from("request failed");
    let mut source = error.source();
    while let Some(cause) = source {
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    message
}

#[cfg(test)]
mod initialization_tests {
    use super::*;

    #[tokio::test]
    async fn initialization_failure_is_returned_before_network_requests() {
        let client = AuthClient {
            http: Err("invalid CA".into()),
            storage: TokenStorage::default(),
        };
        let error = client
            .authenticate("https://example.com", "test-card", Some("test-device"))
            .await
            .unwrap_err();
        assert!(matches!(error, AuthClientError::Initialization(_)));
        let error = client
            .unbind("https://example.com", "test-card", "test-device")
            .await
            .unwrap_err();
        assert!(matches!(error, AuthClientError::Initialization(_)));
    }
}

#[cfg(test)]
mod client_contract_tests {
    use super::*;
    use axum::{routing::post, Json, Router};

    /// Kiro picks endpoints by the profile's region and falls back to the real service for
    /// a region without an override: such a token would carry the gateway's bearer there.
    #[tokio::test]
    async fn a_profile_outside_the_redirected_region_is_refused() {
        const FOREIGN: &str = "arn:aws:codewhisperer:eu-central-1:123456789012:profile/X";
        const HOME: &str = "arn:aws:codewhisperer:us-east-1:123456789012:profile/X";
        assert!(crate::settings::in_redirected_region(HOME));
        for other in [FOREIGN, "", "arn:aws:codewhisperer", "us-east-1"] {
            assert!(!crate::settings::in_redirected_region(other), "{other}");
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let token = |access: &str| {
            serde_json::json!({"accessToken": access, "refreshToken": "r2",
            "profileArn": FOREIGN, "expiresAt": "2030-01-01T00:00:00Z",
            "authMethod": "social", "provider": "Google"})
        };
        let (login, refreshed) = (token("a1"), token("a2"));
        let app = Router::new()
            .route("/oauth/token", post(move || async move { Json(login) }))
            .route(
                "/refreshToken",
                post(move || async move { Json(refreshed) }),
            );
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let dir = std::env::temp_dir().join(format!("region-check-{}", std::process::id()));
        let storage = TokenStorage::at(dir.join("kiro-auth-token.json"));
        let client = AuthClient::new(storage.clone());

        let error = client
            .authenticate(&base, "card", Some("device"))
            .await
            .unwrap_err();
        assert!(
            matches!(error, AuthClientError::InvalidResponse(_)),
            "{error}"
        );

        let current = KiroAuthToken {
            access_token: "a0".into(),
            refresh_token: "r0".into(),
            profile_arn: HOME.into(),
            expires_at: "2030-01-01T00:00:00Z".into(),
            auth_method: "social".into(),
            provider: "Google".into(),
        };
        storage.save(&current).unwrap();
        let error = client.refresh(&base).await.unwrap_err();
        assert!(
            matches!(error, AuthClientError::InvalidResponse(_)),
            "{error}"
        );
        assert_eq!(
            storage.load().unwrap(),
            current,
            "the refused token must not be saved"
        );
        server.abort();
        let _ = std::fs::remove_dir_all(&dir);
    }
    #[tokio::test]
    async fn rebind_policy_uses_bounded_body_retry_without_echoing_details() {
        for (category, retry, expected, seconds) in [
            (
                "rebind_cooldown",
                serde_json::json!(123),
                "rebind-cooldown",
                Some(123),
            ),
            (
                "rebind_cooldown",
                serde_json::json!("remote-secret"),
                "rebind-cooldown",
                None,
            ),
            (
                "rebind_cooldown",
                serde_json::json!(86401),
                "rebind-cooldown",
                None,
            ),
            (
                "rebind_limit_exceeded",
                serde_json::json!(123),
                "rebind-limit",
                None,
            ),
        ] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let app = Router::new().route(
                "/",
                post(move || {
                    let retry = retry.clone();
                    async move {
                        (
                            axum::http::StatusCode::BAD_REQUEST,
                            Json(serde_json::json!({
                                "code": category, "retryAfterSecs": retry, "error": "remote-secret"
                            })),
                        )
                    }
                }),
            );
            let server = tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            });
            let response = reqwest::Client::new()
                .post(format!("http://{address}/"))
                .send()
                .await
                .unwrap();
            let error = AuthClientError::from_response(response).await;
            assert!(!error.is_authorization_rejected());
            assert!(!error.to_string().contains("remote-secret"));
            match error {
                AuthClientError::AuthRejected {
                    code, retry_after, ..
                } => {
                    assert_eq!(code, expected);
                    assert_eq!(retry_after, seconds);
                }
                other => panic!("{other}"),
            }
            server.abort();
            let _ = server.await;
        }
    }
    const TOKEN: &str = r#"{"accessToken":"a1","refreshToken":"r1","profileArn":"arn:aws:codewhisperer:us-east-1:123456789012:profile/X","expiresAt":"2030-01-01T00:00:00Z"}"#;

    fn client() -> AuthClient {
        let unused = std::env::temp_dir().join(format!("retry-token-{}.json", std::process::id()));
        AuthClient::with_ca(TokenStorage::at(unused), None)
    }

    /// Read one HTTP request off `socket`, headers and body.
    async fn read_request(socket: &mut tokio::net::TcpStream) {
        use tokio::io::AsyncReadExt;
        let mut request = Vec::new();
        let mut buffer = [0u8; 4096];
        loop {
            let count = socket.read(&mut buffer).await.unwrap();
            assert!(count > 0, "the request ended early");
            request.extend_from_slice(&buffer[..count]);
            let text = String::from_utf8_lossy(&request).to_ascii_lowercase();
            if let Some(end) = text.find("\r\n\r\n") {
                let length = text
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length:"))
                    .map_or(0, |value| value.trim().parse::<usize>().unwrap());
                if request.len() >= end + 4 + length {
                    return;
                }
            }
        }
    }

    /// A sign-in whose connection is cut before any answer (here after the whole request
    /// went out) is sent once more, after a pause, and the second answer counts.
    #[tokio::test]
    async fn a_sign_in_cut_off_before_any_answer_is_sent_once_more() {
        use tokio::io::AsyncWriteExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut first, _) = listener.accept().await.unwrap();
            read_request(&mut first).await;
            drop(first);
            let (mut second, _) = listener.accept().await.unwrap();
            read_request(&mut second).await;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{TOKEN}",
                TOKEN.len()
            );
            second.write_all(response.as_bytes()).await.unwrap();
        });
        let started = std::time::Instant::now();
        let token = client()
            .authenticate(&base, "card", Some("device"))
            .await
            .unwrap();
        assert_eq!(token.access_token, "a1");
        assert!(started.elapsed() >= LOGIN_RETRY_DELAY);
        server.await.unwrap();
    }

    /// A local proxy that cuts off the TLS handshake every time: one retry, then the
    /// error, which says the handshake was cut off rather than that a certificate failed.
    #[tokio::test]
    async fn a_tls_handshake_cut_off_is_retried_once_then_reported() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::io::AsyncReadExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("https://{}", listener.local_addr().unwrap());
        let connections = std::sync::Arc::new(AtomicUsize::new(0));
        let counted = connections.clone();
        let server = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                counted.fetch_add(1, Ordering::SeqCst);
                // The whole ClientHello record, so the close reaches the client as the end
                // of the handshake rather than as a reset.
                let mut header = [0u8; 5];
                socket.read_exact(&mut header).await.unwrap();
                let mut hello = vec![0u8; usize::from(u16::from_be_bytes([header[3], header[4]]))];
                socket.read_exact(&mut hello).await.unwrap();
            }
        });
        let error = client()
            .authenticate(&base, "card", Some("device"))
            .await
            .unwrap_err();
        server.abort();
        assert_eq!(connections.load(Ordering::SeqCst), 2);
        assert!(matches!(error, AuthClientError::Network(_)), "{error}");
        assert!(error.to_string().contains("tls handshake eof"), "{error}");
        assert!(!error.to_string().contains("certificate"), "{error}");
    }

    /// Once an answer has begun, it is final: a refusal, or one cut off in its body, is
    /// reported at once and the sign-in is not sent again.
    #[tokio::test]
    async fn an_answer_is_never_followed_by_a_retry() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::io::AsyncWriteExt;
        for response in [
            "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 500\r\nConnection: close\r\n\r\n{\"accessToken\"",
        ] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let requests = std::sync::Arc::new(AtomicUsize::new(0));
            let counted = requests.clone();
            let server = tokio::spawn(async move {
                loop {
                    let (mut socket, _) = listener.accept().await.unwrap();
                    read_request(&mut socket).await;
                    counted.fetch_add(1, Ordering::SeqCst);
                    socket.write_all(response.as_bytes()).await.unwrap();
                }
            });
            let started = std::time::Instant::now();
            assert!(client()
                .authenticate(&base, "card", Some("device"))
                .await
                .is_err());
            server.abort();
            assert_eq!(requests.load(Ordering::SeqCst), 1, "{response}");
            assert!(started.elapsed() < LOGIN_RETRY_DELAY, "{response}");
        }
    }

    #[tokio::test]
    async fn rejection_retains_only_whitelisted_category_and_bounded_retry() {
        for (field, category, retry, expected, seconds) in [
            (
                "code",
                "DeviceBindingException",
                "60",
                "device-binding",
                Some(60),
            ),
            (
                "__type",
                "ExpiredTokenException",
                "86400",
                "expired",
                Some(86400),
            ),
            ("code", "remote-secret", "86401", "auth-rejected", None),
            (
                "code",
                "LockoutException",
                "remote-secret",
                "locked-out",
                None,
            ),
        ] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let app = Router::new().route(
                "/oauth/token",
                post(move || async move {
                    (
                        axum::http::StatusCode::FORBIDDEN,
                        [("retry-after", retry)],
                        Json(serde_json::json!({field:category,"message":"remote-secret"})),
                    )
                }),
            );
            let server = tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            });
            let client = AuthClient::with_ca(TokenStorage::default(), None);
            let error = client
                .authenticate(&format!("http://{address}"), "test", Some("device"))
                .await
                .unwrap_err();
            assert!(!error.to_string().contains("remote-secret"));
            assert!(error.is_authorization_rejected());
            match error {
                AuthClientError::AuthRejected {
                    code, retry_after, ..
                } => {
                    assert_eq!(code, expected);
                    assert_eq!(retry_after, seconds);
                }
                other => panic!("{other}"),
            }
            server.abort();
            let _ = server.await;
        }
    }
}
