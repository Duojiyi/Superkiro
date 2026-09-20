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

        let resp = self.http()?.post(&url).json(&req_body).send().await?;

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
