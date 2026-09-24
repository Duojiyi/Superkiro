//! Doctor diagnostic health check and one-click repair assistant (Spec §9, §14.5).
//!
//! Features:
//! - 5-dimension diagnostic engine: Installation, Gateway Connectivity, Settings, Patch, Token.
//! - Visual status enum: `Active`, `NotTakenOver`, `Offline`, `UpgradeDetected`, `TokenExpired`, `Incomplete`.
//! - One-click auto-repair restoring healthy takeover state.

use crate::detect::detect_kiro;
use crate::patch::{ExtensionPatcher, PatchStatus};
use crate::settings::SettingsManager;
use crate::token_storage::TokenStorage;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::Duration;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum DoctorError {
    #[allow(dead_code)]
    #[error("Kiro installation check failed: {0}")]
    Installation(String),

    #[error("I/O or network error: {0}")]
    Io(String),
}

/// Overall visual health and takeover state of the system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TakeoverStatus {
    /// Fully healthy: gateway reachable, settings merged, extension patched, token valid.
    Active,
    /// Local checks passed; real IDE interaction has not been verified.
    ReadyForIdeCheck,
    /// Pristine un-hijacked official state.
    NotTakenOver,
    /// Gateway unreachable or network offline.
    Offline,
    /// Gateway unreachable, but client is operating within valid offline grace period.
    OfflineGrace,
    /// Kiro was updated by official installer; extension patch was overwritten.
    UpgradeDetected,
    /// Local auth token has expired or is missing.
    TokenExpired,
    /// Partially configured state requiring repair.
    Incomplete,
}

/// Status of an individual diagnostic check item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckLevel {
    Pass,
    Warning,
    Fail,
}

/// A single diagnostic item report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckItem {
    pub name: String,
    pub level: CheckLevel,
    pub detail: String,
}

/// Full comprehensive diagnostic report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoctorReport {
    pub overall_status: TakeoverStatus,
    pub items: Vec<CheckItem>,
    pub kiro_version: Option<String>,
    pub patch_status: PatchStatus,
    pub is_running: bool,
    pub gateway_reachable: bool,
}

/// Diagnostic health doctor.
#[derive(Debug, Clone)]
pub struct Doctor {
    http: Result<Client, String>,
    settings_mgr: SettingsManager,
    token_storage: TokenStorage,
}

impl Default for Doctor {
    fn default() -> Self {
        Self {
            http: crate::http::client_builder().and_then(|builder| {
                builder
                    .timeout(Duration::from_secs(4))
                    .build()
                    .map_err(|e| e.to_string())
            }),
            settings_mgr: SettingsManager::default(),
            token_storage: TokenStorage::default(),
        }
    }
}

impl Doctor {
    pub fn new(settings_mgr: SettingsManager, token_storage: TokenStorage) -> Self {
        Self {
            http: crate::http::client_builder().and_then(|builder| {
                builder
                    .timeout(Duration::from_secs(4))
                    .build()
                    .map_err(|e| e.to_string())
            }),
            settings_mgr,
            token_storage,
        }
    }

    /// Run complete diagnostic assessment.
    pub async fn diagnose(
        &self,
        gateway_url: &str,
        custom_install_path: Option<&Path>,
    ) -> DoctorReport {
        let mut items = Vec::new();
        let gw = gateway_url.trim_end_matches('/');

        // 1. Kiro Installation Check
        let kiro_install = detect_kiro(custom_install_path).ok();
        let kiro_version = kiro_install.as_ref().map(|i| i.version.clone());
        if let Some(ref install) = kiro_install {
            items.push(CheckItem {
                name: "Kiro Installation".to_string(),
                level: CheckLevel::Pass,
                detail: format!(
                    "Found v{} at '{}'",
                    install.version,
                    install.install_dir.display()
                ),
            });
        } else {
            items.push(CheckItem {
                name: "Kiro Installation".to_string(),
                level: CheckLevel::Fail,
                detail: "Kiro installation not detected in candidate paths".to_string(),
            });
        }

        // 2. Gateway Reachability Check
        let health_url = format!("{}/healthz", gw);
        let gateway_result = match &self.http {
            Ok(http) => http
                .get(&health_url)
                .send()
                .await
                .map(|resp| resp.status().is_success())
                .map_err(|e| e.to_string()),
            Err(error) => Err(format!("HTTP client initialization failed: {error}")),
        };
        let gateway_reachable = matches!(gateway_result, Ok(true));

        if gateway_reachable {
            items.push(CheckItem {
                name: "Gateway Connectivity".to_string(),
                level: CheckLevel::Pass,
                detail: format!("Successfully reached {}/healthz", gw),
            });
        } else {
            items.push(CheckItem {
                name: "Gateway Connectivity".to_string(),
                level: CheckLevel::Warning,
                detail: gateway_result
                    .err()
                    .unwrap_or_else(|| format!("Gateway unreachable at {}", gw)),
            });
        }

        // 3. Settings Configuration Check
        let settings_active = self.settings_mgr.is_byok_active(Some(gw));
        if settings_active {
            items.push(CheckItem {
                name: "Settings Configuration".to_string(),
                level: CheckLevel::Pass,
                detail: "BYOK redirection & update.mode:none configured".to_string(),
            });
        } else {
            items.push(CheckItem {
                name: "Settings Configuration".to_string(),
                level: CheckLevel::Warning,
                detail: "BYOK settings not applied in settings.json".to_string(),
            });
        }

        // 4. Extension Patch Check
        let patcher = kiro_install.as_ref().and_then(|inst| {
            inst.agent_extension_dir
                .as_ref()
                .map(|dir| ExtensionPatcher::new(dir.join("dist").join("extension.js")))
        });

        let patch_status = patcher
            .as_ref()
            .map(|p| p.status())
            .unwrap_or(PatchStatus::NotFound);

        let patch_verified = patcher
            .as_ref()
            .is_some_and(|p| p.verify_patched_content().is_ok());
        match patch_status {
            PatchStatus::Patched => {
                items.push(CheckItem {
                    name: "Extension Patch".to_string(),
                    level: if patch_verified {
                        CheckLevel::Pass
                    } else {
                        CheckLevel::Fail
                    },
                    detail: if patch_verified {
                        "扩展补丁内容哈希与恢复备份已验证"
                    } else {
                        "补丁完整性校验失败，请勿继续接管"
                    }
                    .to_string(),
                });
            }
            PatchStatus::UpgradeDetected => {
                items.push(CheckItem {
                    name: "Extension Patch".to_string(),
                    level: CheckLevel::Warning,
                    detail: "Kiro upgrade detected: extension.js needs repatching".to_string(),
                });
            }
            PatchStatus::Official => {
                items.push(CheckItem {
                    name: "Extension Patch".to_string(),
                    level: CheckLevel::Warning,
                    detail: "Official unpatched extension.js".to_string(),
                });
            }
            PatchStatus::NotFound => {
                items.push(CheckItem {
                    name: "Extension Patch".to_string(),
                    level: CheckLevel::Fail,
                    detail: "extension.js bundle not found".to_string(),
                });
            }
        }

        // 5. Auth Token Check
        let token_valid = if self.token_storage.exists() {
            if !self.token_storage.is_expired(60) {
                items.push(CheckItem {
                    name: "Authentication Token".to_string(),
                    level: CheckLevel::Pass,
                    detail: "Valid Kiro SSO token on disk".to_string(),
                });
                true
            } else {
                items.push(CheckItem {
                    name: "Authentication Token".to_string(),
                    level: CheckLevel::Warning,
                    detail: "SSO token on disk is expired".to_string(),
                });
                false
            }
        } else {
            items.push(CheckItem {
                name: "Authentication Token".to_string(),
                level: CheckLevel::Warning,
                detail: "No token on disk (not logged in)".to_string(),
            });
            false
        };

        let bypass_ready = reqwest::Url::parse(gw)
            .ok()
            .and_then(|u| u.host_str().map(str::to_owned))
            .is_some_and(|host| {
                self.settings_mgr
                    .read_settings()
                    .ok()
                    .and_then(|m| m.get("http.noProxy").cloned())
                    .and_then(|v| v.as_array().cloned())
                    .is_some_and(|values| values.iter().any(|v| v.as_str() == Some(&host)))
            });
        items.push(CheckItem {
            name: "网关 TLS 代理路径".into(),
            level: if bypass_ready { CheckLevel::Pass } else { CheckLevel::Warning },
            detail: if bypass_ready { "已配置网关专用直连规则，保留 TLS 证书校验" } else { "尚未配置网关直连规则，IP 经 Kiro 核心代理时可能发生证书主机名不匹配；退出 IDE 后重新接管" }.into(),
        });
        items.push(CheckItem {
            name: "真实 IDE 交互验收".into(), level: CheckLevel::Warning,
            detail: "基础检查不代表账号、模型列表或真实对话可用；需在 Kiro 中验证。该诊断不会发送付费推理请求。".into(),
        });
        let is_running = crate::runtime::is_kiro_running();

        // Determine overall visual status
        let overall_status = if !gateway_reachable {
            let prefs = crate::preferences::ClientPreferences::load_or_default(
                crate::preferences::default_preferences_path(),
            )
            .unwrap_or_default();
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            if settings_active
                && patch_verified
                && token_valid
                && prefs.is_within_offline_grace(now)
            {
                TakeoverStatus::OfflineGrace
            } else {
                TakeoverStatus::Offline
            }
        } else if patch_status == PatchStatus::UpgradeDetected {
            TakeoverStatus::UpgradeDetected
        } else if settings_active && patch_verified && token_valid && bypass_ready {
            TakeoverStatus::ReadyForIdeCheck
        } else if !settings_active
            && patch_status == PatchStatus::Official
            && !self.token_storage.exists()
        {
            TakeoverStatus::NotTakenOver
        } else if !token_valid && settings_active {
            TakeoverStatus::TokenExpired
        } else {
            TakeoverStatus::Incomplete
        };

        DoctorReport {
            overall_status,
            items,
            kiro_version,
            patch_status,
            is_running,
            gateway_reachable,
        }
    }
}

#[cfg(test)]
mod initialization_tests {
    use super::*;

    #[tokio::test]
    async fn initialization_failure_is_visible_in_diagnostics() {
        let doctor = Doctor {
            http: Err("invalid CA".into()),
            ..Doctor::default()
        };
        let report = doctor.diagnose("https://example.com", None).await;
        assert!(!report.gateway_reachable);
        assert!(report
            .items
            .iter()
            .any(|item| item.name == "Gateway Connectivity" && item.detail.contains("invalid CA")));
    }
}
