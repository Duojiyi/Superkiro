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
    previous_token: Option<KiroAuthToken>,
    authenticated: bool,
    #[serde(default)]
    ca_path: Option<PathBuf>,
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
        crate::snapshot::OperationLock::acquire(self.path.with_extension("session-lock")).map_err(|_| "Another desktop operation or an interrupted operation holds the session lock; inspect recovery state before retrying".into())
    }
    pub fn gateway(&self) -> Option<String> {
        self.load().ok().map(|s| s.gateway)
    }
    pub fn authenticated(&self) -> bool {
        self.load().is_ok_and(|s| s.authenticated) && self.storage.exists()
    }
    /// Fetch account usage without exposing credentials to the webview.
    pub async fn usage(&self) -> Result<serde_json::Value, String> {
        let session = self.load()?;
        if !session.authenticated {
            return Err("Not authenticated".into());
        }
        let gateway =
            crate::patch::validate_gateway_url(&session.gateway).map_err(|e| e.to_string())?;
        let token = self
            .storage
            .load()
            .map_err(|_| "Cannot read authorization".to_string())?;
        let response = crate::http::client_builder()
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
            return Err(format!("Cloud balance HTTP {}", response.status().as_u16()));
        }
        response
            .json()
            .await
            .map_err(|_| "Invalid cloud usage response".to_string())
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
            let previous_token = if self.storage.exists() {
                Some(self.storage.load().map_err(|e| e.to_string())?)
            } else {
                None
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
        AuthClient::new(self.storage.clone())
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
            .map_err(|e| e.to_string())?;
        let mut launch = crate::process::prepare_kiro_launch(installation, gateway, &[])
            .map_err(|e| e.to_string())?;
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
            .map_err(|e| e.to_string())?;
        if close_confirmed {
            crate::stop_kiro(std::time::Duration::from_secs(30)).map_err(|e| e.to_string())?;
        } else {
            crate::ensure_kiro_stopped()?;
        }
        self.activate_locked(&snapshots, &settings, &patcher, gateway, card, Some(token))
            .await?;
        launch.spawn().map_err(|e| format!("Takeover completed, but Kiro launch failed: {e}. Recovery backup retained; do not assume IDE is ready."))?;
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
        launch.spawn().map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn restore_and_logout(&self, snapshots: &SnapshotManager) -> Result<(), String> {
        let _lock = self.lock()?;
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
                Some(token) => self.storage.save(&token).map_err(|e| e.to_string())?,
                None => {
                    self.storage.clear().map_err(|e| e.to_string())?;
                }
            }
            fs::remove_file(&self.path).map_err(|e| e.to_string())?;
        }
        Ok(())
    }
    pub async fn unbind(&self, snapshots: &SnapshotManager, card: &str) -> Result<(), String> {
        let _lock = self.lock()?;
        crate::ensure_kiro_stopped()?;
        if card.trim().is_empty() || card.chars().count() > 256 {
            return Err("Re-enter the card key to unbind this device".into());
        }
        let session = self.load()?;
        AuthClient::new(self.storage.clone())
            .unbind(&session.gateway, card, &session.device)
            .await
            .map_err(|e| e.to_string())?;
        let cleanup = (|| {
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
