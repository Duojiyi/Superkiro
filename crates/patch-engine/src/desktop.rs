//! Desktop operations share one authenticated gateway and preserve official credentials.
use crate::{
    AuthClient, ExtensionPatcher, KiroAuthToken, SettingsManager, SnapshotManager, TokenStorage,
};
use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf};

#[derive(Serialize, Deserialize)]
struct Session {
    gateway: String,
    device: String,
    previous_token: Option<PreviousToken>,
    authenticated: bool,
    #[serde(default)]
    ca_path: Option<PathBuf>,
}

// New sessions retain exact bytes; old six-field backups remain restorable.
#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum PreviousToken {
    Raw(Vec<u8>),
    Legacy(KiroAuthToken),
}

pub struct DesktopSession {
    storage: TokenStorage,
    path: PathBuf,
}

impl DesktopSession {
    pub fn new(storage: TokenStorage, path: PathBuf) -> Self {
        Self { storage, path }
    }
    pub fn system() -> Result<Self, String> {
        let token_path = crate::default_token_path().map_err(|e| e.to_string())?;
        let path = token_path.with_file_name("kiro-byok-desktop-session.json");
        Ok(Self::new(TokenStorage::at(token_path), path))
    }
    fn load(&self) -> Result<Session, String> {
        serde_json::from_slice(&fs::read(&self.path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())
    }
    fn save(&self, session: &Session) -> Result<(), String> {
        crate::token_storage::private_atomic_write(
            &self.path,
            &serde_json::to_vec(session).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())
    }
    fn retain_gateway_ca(&self) -> Result<Option<PathBuf>, String> {
        let Some(source) = std::env::var_os("KIRO_GATEWAY_CA_CERT") else {
            return Ok(None);
        };
        let pem = fs::read(source).map_err(|e| format!("Cannot read gateway CA: {e}"))?;
        let _ = crate::http::add_ca(reqwest::Client::builder(), &pem)?;
        let target = self.path.with_file_name("kiro-byok-gateway-ca.pem");
        crate::token_storage::private_atomic_write(&target, &pem).map_err(|e| e.to_string())?;
        fs::canonicalize(target)
            .map(Some)
            .map_err(|e| e.to_string())
    }
    fn lock(&self) -> Result<crate::snapshot::OperationLock, String> {
        crate::snapshot::OperationLock::acquire(self.path.with_extension("session-lock")).map_err(|_| "Another desktop operation holds the session lock; wait for completion before retrying".into())
    }
    pub fn gateway(&self) -> Option<String> {
        self.load().ok().map(|s| s.gateway)
    }
    /// A session retains rollback state even when authentication/takeover failed.
    /// Fail closed when the session path cannot be inspected.
    pub fn recovery_pending(&self) -> bool {
        self.path.try_exists().unwrap_or(true)
    }
    pub fn authenticated(&self) -> bool {
        self.load().is_ok_and(|s| s.authenticated) && self.storage.exists()
    }
    /// Fetch account usage without exposing credentials to the webview.
    pub async fn usage(&self) -> Result<serde_json::Value, String> {
        let _lock = self.lock()?;
        let mut session = self.load()?;
        if !session.authenticated {
            return Err("Not authenticated".into());
        }
        let gateway =
            crate::patch::validate_gateway_url(&session.gateway).map_err(|e| e.to_string())?;
        let token = if self.storage.is_expired(60) {
            match AuthClient::with_ca(self.storage.clone(), session.ca_path.as_deref())
                .refresh(&gateway)
                .await
            {
                Ok(token) => token,
                Err(error) => {
                    if error.is_authorization_rejected() {
                        session.authenticated = false;
                        self.save(&session)?;
                    }
                    return Err(error.to_string());
                }
            }
        } else {
            self.storage
                .load()
                .map_err(|_| "Cannot read authorization".to_string())?
        };
        let response = crate::http::client_builder_with_ca(session.ca_path.as_deref())?
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(20))
            .build()
            .map_err(|_| "Cannot create gateway client".to_string())?
            .get(format!("{}/getUsageLimits", gateway.trim_end_matches('/')))
            .bearer_auth(&token.access_token)
            .send()
            .await
            .map_err(|_| "Cloud balance refresh failed".to_string())?;
        if !response.status().is_success() {
            let error = crate::auth::AuthClientError::from_response(response).await;
            if error.is_authorization_rejected() {
                session.authenticated = false;
                self.save(&session)?;
            }
            return Err(error.to_string());
        }
        crate::http::bounded_json(response, 1024 * 1024).await
    }
    pub async fn activate(
        &self,
        snapshots: &SnapshotManager,
        settings: &SettingsManager,
        patcher: &ExtensionPatcher,
        gateway: &str,
        card: &str,
    ) -> Result<(), String> {
        let _lock = self.lock()?;
        self.activate_locked(snapshots, settings, patcher, gateway, card, None)
            .await
    }
    async fn activate_locked(
        &self,
        snapshots: &SnapshotManager,
        settings: &SettingsManager,
        patcher: &ExtensionPatcher,
        gateway: &str,
        card: &str,
        prepared_token: Option<KiroAuthToken>,
    ) -> Result<(), String> {
        crate::ensure_kiro_stopped()?;
        let gateway = crate::patch::validate_gateway_url(gateway).map_err(|e| e.to_string())?;
        snapshots
            .validate_takeover(settings, Some(patcher), &gateway)
            .map_err(|e| e.to_string())?;
        let mut session = if self.path.exists() {
            let existing = self.load()?;
            if existing.gateway != gateway {
                return Err("Restore and log out before changing gateway".into());
            }
            existing
        } else {
            if card.trim().is_empty() {
                return Err("Card key is required for first login".into());
            }
            let previous_token = match fs::read(self.storage.path()) {
                Ok(bytes) => Some(PreviousToken::Raw(bytes)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(e) => return Err(e.to_string()),
            };
            let session = Session {
                gateway: gateway.clone(),
                device: crate::generate_device_fingerprint(None),
                previous_token,
                authenticated: false,
                ca_path: self.retain_gateway_ca()?,
            };
            self.save(&session)?; // Durable credential rollback record before login writes.
            session
        };
        if let Some(token) = prepared_token {
            self.storage.save(&token).map_err(|e| e.to_string())?;
            session.authenticated = true;
            self.save(&session)?;
        } else {
            self.login_card(&mut session, card).await?;
        }
        snapshots
            .takeover(settings, Some(patcher), &gateway)
            .map_err(|e| {
                format!("Login succeeded but takeover failed: {e}; recovery record retained")
            })?;
        Ok(())
    }
    async fn login_card(&self, session: &mut Session, card: &str) -> Result<(), String> {
        // Always authenticate the supplied card; never silently refresh a previous card.
        AuthClient::with_ca(self.storage.clone(), session.ca_path.as_deref())
            .login(&session.gateway, card, Some(&session.device))
            .await
            .map_err(|e| e.to_string())?;
        session.authenticated = true;
        self.save(session)
    }

    /// Explicitly confirmed customer workflow. Never starts Kiro after a failed takeover.
    pub async fn activate_and_launch(
        &self,
        installation: &crate::KiroInstallation,
        gateway: &str,
        card: &str,
        close_confirmed: bool,
    ) -> Result<(), String> {
        let _workflow = self.lock()?;
        if card.trim().is_empty() || card.chars().count() > 256 {
            return Err("Card key must contain 1..256 characters".into());
        }
        let extension = installation
            .agent_extension_dir
            .as_ref()
            .ok_or("Kiro agent extension not found; takeover refused")?;
        let patcher = ExtensionPatcher::new(extension.join("dist").join("extension.js"));
        let snapshots = SnapshotManager::default();
        let settings = SettingsManager::default();
        snapshots
            .validate_takeover(&settings, Some(&patcher), gateway)
            .map_err(|e| format!("[connection:preflight] {e}"))?;
        let mut launch = crate::process::prepare_kiro_launch(installation, gateway, &[])
            .map_err(|e| format!("[connection:launch-prepare] {e}"))?;
        let device = if self.path.exists() {
            let existing = self.load()?;
            if existing.gateway
                != crate::patch::validate_gateway_url(gateway).map_err(|e| e.to_string())?
            {
                return Err("Restore and log out before changing gateway".into());
            }
            existing.device
        } else {
            crate::generate_device_fingerprint(None)
        };
        // Reject invalid cards before requesting closure. Authentication can bind a cloud device,
        // but no local token/configuration is changed until the process is confirmed stopped.
        let token = AuthClient::new(self.storage.clone())
            .authenticate(gateway, card, Some(&device))
            .await
            .map_err(|e| format!("[connection:authenticate] {e}"))?;
        if close_confirmed {
            crate::stop_kiro(std::time::Duration::from_secs(30))
                .map_err(|e| format!("[connection:close] {e}"))?;
        } else {
            crate::ensure_kiro_stopped().map_err(|e| format!("[connection:close] {e}"))?;
        }
        self.activate_locked(&snapshots, &settings, &patcher, gateway, card, Some(token))
            .await
            .map_err(|e| format!("[connection:apply] {e}"))?;
        crate::process::spawn_and_confirm(&mut launch).map_err(|e| format!("[connection:launch] Takeover completed, but Kiro launch failed: {e}. Recovery backup retained; do not assume IDE is ready."))?;
        Ok(())
    }

    /// Relaunch without re-entering or persisting card secrets.
    pub fn launch(&self, installation: &crate::KiroInstallation) -> Result<(), String> {
        let _lock = self.lock()?;
        crate::ensure_kiro_stopped()?;
        let session = self.load()?;
        if !session.authenticated || !self.storage.exists() {
            return Err("Activate a card first".into());
        }
        let extension = installation
            .agent_extension_dir
            .as_ref()
            .ok_or("Kiro agent extension missing")?;
        let patcher = ExtensionPatcher::new(extension.join("dist").join("extension.js"));
        let snapshots = SnapshotManager::default();
        if !snapshots.has_active_snapshot() {
            return Err("No active takeover; activate again".into());
        }
        snapshots
            .validate_takeover(
                &SettingsManager::default(),
                Some(&patcher),
                &session.gateway,
            )
            .map_err(|e| e.to_string())?;
        let mut launch = crate::process::prepare_kiro_launch(installation, &session.gateway, &[])
            .map_err(|e| e.to_string())?;
        if let Some(ca) = &session.ca_path {
            let pem = std::fs::read(ca).map_err(|e| e.to_string())?;
            let _ = crate::http::add_ca(reqwest::Client::builder(), &pem)?;
            launch.env("NODE_EXTRA_CA_CERTS", ca);
        }
        snapshots
            .takeover(
                &SettingsManager::default(),
                Some(&patcher),
                &session.gateway,
            )
            .map_err(|e| e.to_string())?;
        crate::process::spawn_and_confirm(&mut launch).map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn restore_and_logout(&self, snapshots: &SnapshotManager) -> Result<(), String> {
        let _lock = self.lock()?;
        if !self.recovery_pending() && !snapshots.has_active_snapshot() {
            return Ok(());
        }
        crate::ensure_kiro_stopped()?;
        if snapshots.has_active_snapshot() {
            snapshots.restore_official().map_err(|e| e.to_string())?;
        }
        self.restore_token()
    }
    fn restore_token(&self) -> Result<(), String> {
        // Never clear an unrelated official token when no desktop session exists.
        if self.path.exists() {
            let session = self.load()?;
            match session.previous_token {
                Some(PreviousToken::Raw(bytes)) => {
                    crate::token_storage::private_atomic_write(self.storage.path(), &bytes)
                        .map_err(|e| e.to_string())?
                }
                Some(PreviousToken::Legacy(token)) => {
                    self.storage.save(&token).map_err(|e| e.to_string())?
                }
                None => {
                    self.storage.clear().map_err(|e| e.to_string())?;
                }
            }
            fs::remove_file(&self.path).map_err(|e| e.to_string())?;
        }
        Ok(())
    }
    pub async fn unbind(&self, snapshots: &SnapshotManager, card: &str) -> Result<(), String> {
        let gateway = self.load()?.gateway;
        self.unbind_with_gateway(snapshots, card, &gateway).await
    }
    /// Cloud device bindings outlive local restoration. A verified card can
    /// unbind this device without recreating takeover state or touching the IDE.
    pub async fn unbind_with_gateway(
        &self,
        snapshots: &SnapshotManager,
        card: &str,
        fallback_gateway: &str,
    ) -> Result<(), String> {
        let _lock = self.lock()?;
        if card.trim().is_empty() || card.chars().count() > 256 {
            return Err("Re-enter the card key to unbind this device".into());
        }
        let session = if self.recovery_pending() {
            Some(self.load()?)
        } else {
            None
        };
        if session.is_some() || snapshots.has_active_snapshot() {
            crate::ensure_kiro_stopped()?;
        }
        let device = session
            .as_ref()
            .map(|s| s.device.clone())
            .unwrap_or_else(|| crate::generate_device_fingerprint(None));
        let gateway = session
            .as_ref()
            .map(|s| s.gateway.as_str())
            .unwrap_or(fallback_gateway);
        let client = match &session {
            Some(session) => AuthClient::with_ca(self.storage.clone(), session.ca_path.as_deref()),
            None => AuthClient::new(self.storage.clone()),
        };
        client
            .unbind(gateway, card, &device)
            .await
            .map_err(|e| e.to_string())?;
        let cleanup = (|| {
            // Remote revocation remains effective even if local restoration fails.
            if let Some(mut session) = session {
                session.authenticated = false;
                self.save(&session)?;
            }
            if snapshots.has_active_snapshot() {
                snapshots.restore_official().map_err(|e| e.to_string())?;
            }
            self.restore_token()
        })();
        cleanup.map_err(|e| format!("Remote unbind succeeded; local cleanup failed: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{routing::post, Json, Router};
    use serde_json::{json, Value};

    #[tokio::test]
    async fn successful_unbind_invalidates_session_even_when_local_restore_fails() {
        let root =
            std::env::temp_dir().join(format!("unbind-cleanup-failure-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let storage = TokenStorage::at(root.join("token.json"));
        fs::write(storage.path(), b"revoked-token").unwrap();
        let desktop = DesktopSession::new(storage.clone(), root.join("session.json"));
        let snapshots = SnapshotManager::at(root.join("snapshot.json"));
        fs::write(snapshots.snapshot_path(), b"broken snapshot").unwrap();
        let app = Router::new()
            .route(
                "/api/v1/portal/challenge",
                post(|| async { Json(json!({"challengeToken":"fixture"})) }),
            )
            .route(
                "/api/v1/portal/unbind",
                post(|| async { Json(json!({"success":true})) }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let gateway = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        desktop
            .save(&Session {
                gateway: gateway.clone(),
                device: "fixture-device".into(),
                previous_token: Some(PreviousToken::Raw(b"official-token".to_vec())),
                authenticated: true,
                ca_path: None,
            })
            .unwrap();
        let error = desktop
            .unbind_with_gateway(&snapshots, "test-card", &gateway)
            .await
            .unwrap_err();
        server.abort();
        let _ = server.await;
        assert!(error.contains("Remote unbind succeeded"), "{error}");
        let reopened = DesktopSession::new(storage.clone(), desktop.path.clone());
        assert!(!reopened.authenticated());
        assert!(reopened.recovery_pending());
        assert!(
            matches!(reopened.load().unwrap().previous_token, Some(PreviousToken::Raw(v)) if v == b"official-token")
        );
        assert_eq!(
            fs::read(snapshots.snapshot_path()).unwrap(),
            b"broken snapshot"
        );
        assert_eq!(fs::read(storage.path()).unwrap(), b"revoked-token");
        for path in [
            storage.path().to_path_buf(),
            desktop.path.clone(),
            snapshots.snapshot_path().to_path_buf(),
            desktop.path.with_extension("session-lock"),
            snapshots.snapshot_path().with_extension("operation-lock"),
        ] {
            if path.exists() {
                fs::remove_file(path).unwrap();
            }
        }
        fs::remove_dir(root).unwrap();
    }
    #[tokio::test]
    async fn unbind_after_local_restore_preserves_official_token() {
        let root =
            std::env::temp_dir().join(format!("unbind-without-session-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let storage = TokenStorage::at(root.join("token.json"));
        fs::write(storage.path(), b"official-token-bytes").unwrap();
        let desktop = DesktopSession::new(storage.clone(), root.join("session.json"));
        let snapshots = SnapshotManager::at(root.join("snapshot.json"));
        let app = Router::new()
            .route(
                "/api/v1/portal/challenge",
                post(|| async { Json(json!({"challengeToken":"fixture"})) }),
            )
            .route(
                "/api/v1/portal/unbind",
                post(|Json(body): Json<Value>| async move {
                    assert_eq!(body["card"], "test-card");
                    assert_eq!(body["device"], crate::generate_device_fingerprint(None));
                    assert_eq!(body["challenge_token"], "fixture");
                    Json(json!({"success":true}))
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let gateway = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        desktop.restore_and_logout(&snapshots).unwrap();
        desktop
            .unbind_with_gateway(&snapshots, "test-card", &gateway)
            .await
            .unwrap();
        assert_eq!(fs::read(storage.path()).unwrap(), b"official-token-bytes");
        assert!(!desktop.recovery_pending());
        assert!(!snapshots.has_active_snapshot());
        // Corrupt recovery state must never be bypassed via the fallback gateway.
        fs::write(&desktop.path, b"invalid-json").unwrap();
        assert!(desktop
            .unbind_with_gateway(&snapshots, "test-card", &gateway)
            .await
            .is_err());
        assert_eq!(fs::read(storage.path()).unwrap(), b"official-token-bytes");
        server.abort();
        let _ = server.await;
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn audit_credentials_restore_exact_bytes_and_legacy_sessions() {
        let root = std::env::temp_dir().join(format!("audit-credentials-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let storage = TokenStorage::at(root.join("token.json"));
        let desktop = DesktopSession::new(storage.clone(), root.join("session.json"));
        let raw =
            b"{ \r\n \"unknown\": {\"nested\": [1, true]}, \"accessToken\": \"fixture\" }\r\n";
        // Include unknown fields, whitespace, incomplete schema and non-UTF8 bytes.
        for bytes in [raw.as_slice(), b"\xff\x00opaque-token", b""] {
            desktop
                .save(&Session {
                    gateway: "https://fixture.invalid".into(),
                    device: "fixture".into(),
                    previous_token: Some(PreviousToken::Raw(bytes.to_vec())),
                    authenticated: false,
                    ca_path: None,
                })
                .unwrap();
            fs::write(storage.path(), b"replacement").unwrap();
            desktop.restore_token().unwrap();
            assert_eq!(fs::read(storage.path()).unwrap(), bytes);
            assert!(!desktop.path.exists());
        }
        let legacy = KiroAuthToken::new("fixture", "refresh", "profile", "2030-01-01T00:00:00Z");
        fs::write(
            &desktop.path,
            serde_json::to_vec(&json!({
                "gateway": "https://fixture.invalid", "device": "fixture",
                "previous_token": legacy, "authenticated": false
            }))
            .unwrap(),
        )
        .unwrap();
        desktop.restore_token().unwrap();
        assert_eq!(storage.load().unwrap(), legacy);
        fs::remove_file(storage.path()).unwrap();
        fs::remove_dir(root).unwrap();
    }

    #[test]
    fn recovery_pending_does_not_require_authenticated_or_readable_session() {
        let root =
            std::env::temp_dir().join(format!("desktop-recovery-test-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("session.json");
        let session = DesktopSession::new(TokenStorage::at(root.join("token.json")), path.clone());
        assert!(!session.recovery_pending());
        std::fs::write(&path, br#"{"authenticated":false,"previous_token":null}"#).unwrap();
        assert!(session.recovery_pending());
        assert!(!session.authenticated());
        std::fs::write(&path, b"corrupt rollback metadata").unwrap();
        assert!(session.recovery_pending());
        assert!(!session.authenticated());
        std::fs::remove_file(path).unwrap();
        assert!(!session.recovery_pending());
        std::fs::remove_dir(root).unwrap();
    }

    #[tokio::test]
    async fn usage_reads_exact_route_with_bearer_without_returning_token() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new().route("/getUsageLimits", axum::routing::get(|headers: axum::http::HeaderMap| async move {
            assert_eq!(headers.get("authorization").unwrap(), "Bearer usage-test-secret");
            Json(json!({"usageBreakdownList":[{"dimensionType":"CREDIT", "currentUsageWithPrecision":0.493568, "usageLimitWithPrecision":1000.0}]}))
        }));
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let root = std::env::temp_dir().join(format!("kiro-usage-test-{}", std::process::id()));
        let storage = TokenStorage::at(root.join("token.json"));
        let desktop = DesktopSession::new(storage.clone(), root.join("session.json"));
        storage
            .save(&KiroAuthToken::new(
                "usage-test-secret",
                "refresh",
                "profile",
                "2099-01-01T00:00:00Z",
            ))
            .unwrap();
        desktop
            .save(&Session {
                gateway: format!("http://{address}"),
                device: "test".into(),
                previous_token: None,
                authenticated: true,
                ca_path: None,
            })
            .unwrap();
        let usage = desktop.usage().await.unwrap();
        assert_eq!(
            usage["usageBreakdownList"][0]["currentUsageWithPrecision"],
            0.493568
        );
        assert!(!usage.to_string().contains("usage-test-secret"));
        assert_eq!(storage.load().unwrap().access_token, "usage-test-secret");
        server.abort();
        let _ = std::fs::remove_file(root.join("token.json"));
        let _ = std::fs::remove_file(root.join("session.json"));
        let _ = std::fs::remove_dir(root);
    }

    #[tokio::test]
    async fn usage_refreshes_expired_access_token_before_query() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new()
            .route(
                "/refreshToken",
                post(|Json(body): Json<Value>| async move {
                    assert_eq!(body["refreshToken"], "refresh-secret");
                    Json(
                        json!({"accessToken":"renewed-access", "refreshToken":"renewed-refresh",
                    "profileArn":"profile", "expiresAt":"2099-01-01T00:00:00Z"}),
                    )
                }),
            )
            .route(
                "/getUsageLimits",
                axum::routing::get(|headers: axum::http::HeaderMap| async move {
                    assert_eq!(headers["authorization"], "Bearer renewed-access");
                    Json(json!({"success":true}))
                }),
            );
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let root = std::env::temp_dir().join(format!("kiro-usage-refresh-{}", std::process::id()));
        let storage = TokenStorage::at(root.join("token.json"));
        let desktop = DesktopSession::new(storage.clone(), root.join("session.json"));
        storage
            .save(&KiroAuthToken::new(
                "expired",
                "refresh-secret",
                "profile",
                "2020-01-01T00:00:00Z",
            ))
            .unwrap();
        desktop
            .save(&Session {
                gateway: format!("http://{address}"),
                device: "test".into(),
                previous_token: None,
                authenticated: true,
                ca_path: None,
            })
            .unwrap();
        assert_eq!(desktop.usage().await.unwrap()["success"], true);
        assert_eq!(storage.load().unwrap().access_token, "renewed-access");
        server.abort();
        for name in ["token.json", "session.json"] {
            let _ = fs::remove_file(root.join(name));
        }
        let _ = fs::remove_dir(root);
    }

    #[tokio::test]
    async fn client_contract_usage_denial_preserves_recovery_but_invalidates_authorization() {
        for refresh in [false, true] {
            for status in [401, 403, 429, 503, 0] {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let address = listener.local_addr().unwrap();
                let app = Router::new().fallback(move || async move {
                    (
                        axum::http::StatusCode::from_u16(status).unwrap(),
                        Json(json!({"__type":"AccessDeniedException","message":"remote-secret"})),
                    )
                });
                let server = if status == 0 {
                    drop(listener);
                    None
                } else {
                    Some(tokio::spawn(async move {
                        axum::serve(listener, app).await.unwrap();
                    }))
                };
                let root = std::env::temp_dir().join(format!(
                    "client-contract-{}-{refresh}-{status}",
                    std::process::id()
                ));
                let storage = TokenStorage::at(root.join("token.json"));
                let desktop = DesktopSession::new(storage.clone(), root.join("session.json"));
                storage
                    .save(&KiroAuthToken::new(
                        "access",
                        "refresh",
                        "profile",
                        if refresh {
                            "2020-01-01T00:00:00Z"
                        } else {
                            "2099-01-01T00:00:00Z"
                        },
                    ))
                    .unwrap();
                desktop
                    .save(&Session {
                        gateway: format!("http://{address}"),
                        device: "test".into(),
                        previous_token: Some(PreviousToken::Raw(b"official-backup".to_vec())),
                        authenticated: true,
                        ca_path: None,
                    })
                    .unwrap();
                assert!(desktop.usage().await.is_err());
                let reopened = DesktopSession::new(storage.clone(), root.join("session.json"));
                assert_eq!(reopened.authenticated(), status != 401 && status != 403);
                assert!(reopened.recovery_pending());
                assert!(
                    matches!(reopened.load().unwrap().previous_token, Some(PreviousToken::Raw(v)) if v == b"official-backup")
                );
                assert_eq!(storage.load().unwrap().access_token, "access");
                assert_eq!(storage.load().unwrap().refresh_token, "refresh");
                if let Some(server) = server {
                    server.abort();
                    let _ = server.await;
                }
                if status == 401 || status == 403 {
                    assert_eq!(reopened.usage().await.unwrap_err(), "Not authenticated");
                }
                for name in ["token.json", "session.json", "session.session-lock"] {
                    fs::remove_file(root.join(name)).unwrap();
                }
                fs::remove_dir(root).unwrap();
            }
        }
    }

    #[tokio::test]
    async fn failed_usage_refresh_preserves_stored_credentials() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new().route(
            "/refreshToken",
            post(|| async { axum::http::StatusCode::SERVICE_UNAVAILABLE }),
        );
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let root =
            std::env::temp_dir().join(format!("kiro-refresh-failure-{}", std::process::id()));
        let storage = TokenStorage::at(root.join("token.json"));
        let desktop = DesktopSession::new(storage.clone(), root.join("session.json"));
        storage
            .save(&KiroAuthToken::new(
                "old-access",
                "old-refresh",
                "profile",
                "2020-01-01T00:00:00Z",
            ))
            .unwrap();
        desktop
            .save(&Session {
                gateway: format!("http://{address}"),
                device: "test".into(),
                previous_token: None,
                authenticated: true,
                ca_path: None,
            })
            .unwrap();
        assert!(desktop.usage().await.is_err());
        let retained = storage.load().unwrap();
        assert_eq!(retained.access_token, "old-access");
        assert_eq!(retained.refresh_token, "old-refresh");
        assert!(desktop.load().unwrap().authenticated);
        server.abort();
        for name in ["token.json", "session.json"] {
            let _ = fs::remove_file(root.join(name));
        }
        let _ = fs::remove_dir(root);
    }

    #[tokio::test]
    async fn card_switch_reauthenticates_and_rejection_preserves_token() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new().route(
            "/oauth/token",
            post(|Json(request): Json<Value>| async move {
                let card = request["card_key"].as_str().unwrap();
                if card == "invalid" {
                    return (
                        axum::http::StatusCode::UNAUTHORIZED,
                        Json(json!({"error":"rejected"})),
                    );
                }
                (
                    axum::http::StatusCode::OK,
                    Json(json!({
                        "accessToken":card, "refreshToken":"refresh", "profileArn":"profile",
                        "expiresAt":"2099-01-01T00:00:00Z"
                    })),
                )
            }),
        );
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let root = std::env::temp_dir().join(format!("kiro-card-switch-{}", std::process::id()));
        let storage = TokenStorage::at(root.join("token.json"));
        let desktop = DesktopSession::new(storage.clone(), root.join("session.json"));
        let mut session = Session {
            gateway: format!("http://{address}"),
            device: "test-device".into(),
            previous_token: None,
            authenticated: false,
            ca_path: None,
        };
        desktop.login_card(&mut session, "card-a").await.unwrap();
        assert_eq!(storage.load().unwrap().access_token, "card-a");
        let candidate = AuthClient::new(storage.clone())
            .authenticate(&session.gateway, "card-preview", Some(&session.device))
            .await
            .unwrap();
        assert_eq!(candidate.access_token, "card-preview");
        assert_eq!(storage.load().unwrap().access_token, "card-a");
        desktop.login_card(&mut session, "card-b").await.unwrap();
        assert_eq!(storage.load().unwrap().access_token, "card-b");
        assert!(desktop.login_card(&mut session, "invalid").await.is_err());
        assert_eq!(storage.load().unwrap().access_token, "card-b");
        assert!(desktop.load().unwrap().authenticated);
        server.abort();
        let _ = std::fs::remove_file(root.join("token.json"));
        let _ = std::fs::remove_file(root.join("session.json"));
        let _ = std::fs::remove_dir(root);
    }
}

#[cfg(test)]
mod persistent_ca_tests {
    use super::*;
    struct Child(std::process::Child);
    impl Drop for Child {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    #[test]
    fn restart_without_ca_environment() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "desktop::persistent_ca_tests::https_session_helper",
                "--nocapture",
            ])
            .env("SUPERKIRO_CA_FIXTURE", "1")
            .env_remove("KIRO_GATEWAY_CA_CERT")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    #[tokio::test]
    async fn https_session_helper() {
        if std::env::var_os("SUPERKIRO_CA_FIXTURE").is_none() {
            return;
        }
        assert!(std::env::var_os("KIRO_GATEWAY_CA_CERT").is_none());
        let root =
            std::env::temp_dir().join(format!("persistent-ca-fixture-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let port_file = root.join("port");
        let server = Child(
            std::process::Command::new("node")
                .arg(fixtures.join("local-https.cjs"))
                .arg(&port_file)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .spawn()
                .unwrap(),
        );
        // The fixture generates two RSA-2048 keys with openssl before it binds, which
        // exceeded 10s on a loaded Windows CI runner. This only guards against a hang.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while !port_file.exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "local TLS fixture did not start"
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let gateway = format!(
            "https://localhost:{}",
            fs::read_to_string(&port_file).unwrap()
        );
        let storage = TokenStorage::at(root.join("token.json"));
        storage
            .save(&KiroAuthToken::new(
                "expired",
                "refresh",
                "profile",
                "2020-01-01T00:00:00Z",
            ))
            .unwrap();
        let ca = root.join("retained-ca.pem");
        fs::copy(root.join("local-test-ca.pem"), &ca).unwrap();
        let session = Session {
            gateway: gateway.clone(),
            device: "test-device".into(),
            previous_token: None,
            authenticated: true,
            ca_path: Some(ca.clone()),
        };
        DesktopSession::new(storage.clone(), root.join("session.json"))
            .save(&session)
            .unwrap();
        // Recreate the session as after process restart. Refresh + usage must use
        // the saved trust path, and an untrusted client must still reject TLS.
        let desktop = DesktopSession::new(storage.clone(), root.join("session.json"));
        assert_eq!(desktop.usage().await.unwrap()["success"], true);
        assert!(crate::http::client_builder_with_ca(None)
            .unwrap()
            .build()
            .unwrap()
            .get(format!("{gateway}/getUsageLimits"))
            .send()
            .await
            .is_err());
        // Missing persisted trust never silently falls back to public/default CA.
        fs::remove_file(&ca).unwrap();
        assert!(desktop.usage().await.is_err());
        fs::copy(root.join("local-test-ca.pem"), &ca).unwrap();
        desktop
            .unbind(
                &SnapshotManager::at(root.join("snapshot.json")),
                "test-card",
            )
            .await
            .unwrap();
        assert!(!desktop.recovery_pending());
        assert!(!storage.exists());
        // This helper runs in its own child process. Once restored, unbinding
        // must use the host's configured CA rather than silently dropping it.
        std::env::set_var("KIRO_GATEWAY_CA_CERT", &ca);
        desktop
            .unbind_with_gateway(
                &SnapshotManager::at(root.join("snapshot.json")),
                "test-card",
                &gateway,
            )
            .await
            .unwrap();
        assert!(!desktop.recovery_pending());
        assert!(!storage.exists());
        drop(server);
        for name in [
            "port",
            "retained-ca.pem",
            "session.session-lock",
            "local-test-ca.pem",
            "local-test-cert.pem",
            "local-test-key.pem",
        ] {
            fs::remove_file(root.join(name)).unwrap();
        }
        fs::remove_dir(root).unwrap();
    }
}
