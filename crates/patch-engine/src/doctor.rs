//! Doctor diagnostic health check and one-click repair assistant (Spec §9, §14.5).
//!
//! Features:
//! - 5-dimension diagnostic engine: Installation, Gateway Connectivity, Settings, Patch, Token.
//! - Visual status enum: `Active`, `NotTakenOver`, `Offline`, `UpgradeDetected`, `TokenExpired`, `Incomplete`.
//! - One-click auto-repair restoring healthy takeover state.

use crate::detect::{detect_kiro, KiroInstallation};
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
    pub can_one_click_fix: bool,
}

/// Diagnostic health doctor.
#[derive(Debug, Clone)]
pub struct Doctor {
    http: Client,
    settings_mgr: SettingsManager,
    token_storage: TokenStorage,
}

impl Default for Doctor {
    fn default() -> Self {
        Self {
            http: crate::http::client_builder()
                .timeout(Duration::from_secs(4))
                .build()
                .expect("TLS HTTP client initialization failed"),
            settings_mgr: SettingsManager::default(),
            token_storage: TokenStorage::default(),
        }
    }
}

impl Doctor {
    pub fn new(settings_mgr: SettingsManager, token_storage: TokenStorage) -> Self {
        Self {
            http: crate::http::client_builder()
                .timeout(Duration::from_secs(4))
                .build()
                .expect("TLS HTTP client initialization failed"),
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
        let gateway_reachable = match self.http.get(&health_url).send().await {
            Ok(resp) => resp.status().is_success(),
            Err(_) => false,
        };

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
                detail: format!("Gateway unreachable at {}", gw),
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

        let can_one_click_fix = matches!(
            overall_status,
            TakeoverStatus::UpgradeDetected
                | TakeoverStatus::Incomplete
                | TakeoverStatus::NotTakenOver
        );

        DoctorReport {
            overall_status,
            items,
            kiro_version,
            patch_status,
            is_running,
            gateway_reachable,
            can_one_click_fix,
        }
    }

    /// One-click repair: merges settings and applies extension patch.
    pub fn one_click_fix(
        &self,
        gateway_url: &str,
        installation: &KiroInstallation,
    ) -> Result<(), DoctorError> {
        // 1. Merge settings
        self.settings_mgr
            .merge_byok(gateway_url)
            .map_err(|e| DoctorError::Io(e.to_string()))?;

        // 2. Patch extension if present
        if let Some(ref ext_dir) = installation.agent_extension_dir {
            let patcher = ExtensionPatcher::new(ext_dir.join("dist").join("extension.js"));
            if patcher.status() != PatchStatus::Patched {
                patcher
                    .apply(gateway_url)
                    .map_err(|e| DoctorError::Io(e.to_string()))?;
            }
        }

        // 3. Purge orphan agent processes and compact working set
        let _ = crate::mem_guard::MemoryGuard::purge_orphan_processes();
        let _ = crate::mem_guard::MemoryGuard::trim_working_set(None);

        Ok(())
    }
}
