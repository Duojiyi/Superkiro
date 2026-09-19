//! Client user preferences, offline grace period evaluation, and announcements (Spec §9, §10, §14.2).
//!
//! Features:
//! - Configurable default model (`claude-3-7-sonnet`, `deepseek-chat`, etc.) and reasoning effort.
//! - Internationalization setting (`zh-CN` / `en-US`).
//! - Offline grace period management: allows offline Kiro launching within configured grace period (default 72h).
//! - Announcement model for gateway broadcasts.
//! - Atomic persistence to avoid config corruption.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Default offline grace period in seconds (72 hours).
pub const DEFAULT_OFFLINE_GRACE_SECONDS: u64 = 72 * 3600;

#[derive(Debug, Error)]
pub enum PreferencesError {
    #[error("I/O error handling preferences: {0}")]
    Io(#[from] std::io::Error),

    #[error("Serialization error handling preferences: {0}")]
    Serialization(#[from] serde_json::Error),
}

/// Thinking / reasoning budget setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningEffort {
    Off,
    Low,
    #[default]
    Medium,
    High,
}

/// UI Language.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Language {
    #[serde(rename = "zh-CN")]
    #[default]
    ZhCn,
    #[serde(rename = "en-US")]
    EnUs,
}

/// Server broadcast announcement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Announcement {
    pub id: String,
    pub title: String,
    pub content: String,
    pub level: String, // "info", "warning", "critical"
    pub published_at: u64,
}

/// User client preferences.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientPreferences {
    /// Preferred default model name.
    pub default_model: String,
    /// Reasoning / Thinking effort tier.
    pub reasoning_effort: ReasoningEffort,
    /// Language code.
    pub language: Language,
    /// Allowed offline grace window in seconds.
    pub offline_grace_seconds: u64,
    /// Last verified online timestamp (unix epoch seconds).
    pub last_online_timestamp: u64,
    /// Last acknowledged announcement ID.
    pub last_dismissed_announcement_id: Option<String>,
}

impl Default for ClientPreferences {
    fn default() -> Self {
        Self {
            default_model: "claude-3-7-sonnet".to_string(),
            reasoning_effort: ReasoningEffort::default(),
            language: Language::default(),
            offline_grace_seconds: DEFAULT_OFFLINE_GRACE_SECONDS,
            last_online_timestamp: 0,
            last_dismissed_announcement_id: None,
        }
    }
}

impl ClientPreferences {
    /// Save preferences atomically to disk.
    pub fn save_to_file<P: AsRef<Path>>(&self, path: P) -> Result<(), PreferencesError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let temp_path = path.with_extension("tmp");
        let content = serde_json::to_string_pretty(self)?;
        {
            let mut file = File::create(&temp_path)?;
            file.write_all(content.as_bytes())?;
            file.flush()?;
        }
        fs::rename(temp_path, path)?;
        Ok(())
    }

    /// Load preferences from disk or return default if missing.
    pub fn load_or_default<P: AsRef<Path>>(path: P) -> Result<Self, PreferencesError> {
        let path = path.as_ref();
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = fs::read_to_string(path)?;
        let parsed: Self = serde_json::from_str(&raw)?;
        Ok(parsed)
    }

    /// Update the last online verification timestamp.
    pub fn mark_online(&mut self, now_epoch: u64) {
        self.last_online_timestamp = now_epoch;
    }

    /// Check if currently within the offline grace window.
    pub fn is_within_offline_grace(&self, now_epoch: u64) -> bool {
        if self.last_online_timestamp == 0 {
            return false;
        }
        if now_epoch < self.last_online_timestamp {
            // Clock skew protection: if now is slightly in the past, treat as valid within reason
            return self.last_online_timestamp - now_epoch < 300; // ponytail: 5min clock skew tolerance
        }
        now_epoch - self.last_online_timestamp <= self.offline_grace_seconds
    }

    /// Return remaining grace seconds.
    pub fn remaining_grace_seconds(&self, now_epoch: u64) -> u64 {
        if !self.is_within_offline_grace(now_epoch) {
            return 0;
        }
        let elapsed = now_epoch.saturating_sub(self.last_online_timestamp);
        self.offline_grace_seconds.saturating_sub(elapsed)
    }
}

/// Return default preferences file path.
pub fn default_preferences_path() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            return PathBuf::from(appdata)
                .join("KiroBYOK")
                .join("preferences.json");
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home)
                .join("Library")
                .join("Application Support")
                .join("KiroBYOK")
                .join("preferences.json");
        }
    }
    #[cfg(target_os = "linux")]
    {
        if let Ok(config) = std::env::var("XDG_CONFIG_HOME") {
            return PathBuf::from(config)
                .join("kiro-byok")
                .join("preferences.json");
        } else if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home)
                .join(".config")
                .join("kiro-byok")
                .join("preferences.json");
        }
    }
    std::env::temp_dir().join("kiro_byok_preferences.json")
}

/// Model catalog entry presented to client UI (Spec §14.8, P4-1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientModelCatalogItem {
    pub model_id: String,
    pub model_name: String,
    #[serde(default)]
    pub description: Option<String>,
}

/// Server-distributed UI overrides for branding, announcements, credit labels, and model visibility/renaming (Spec §14.8, P4-1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiOverrides {
    #[serde(default = "default_brand_name")]
    pub brand_name: String,
    #[serde(default)]
    pub brand_subtitle: Option<String>,
    #[serde(default = "default_credit_label")]
    pub credit_label: String,
    #[serde(default)]
    pub announcements: Vec<String>,
    #[serde(default)]
    pub model_rename_rules: HashMap<String, String>,
    #[serde(default)]
    pub hidden_models: Vec<String>,
}

fn default_brand_name() -> String {
    "KIRO 极速接管中心".to_string()
}

fn default_credit_label() -> String {
    "算力积分".to_string()
}

impl Default for UiOverrides {
    fn default() -> Self {
        Self {
            brand_name: default_brand_name(),
            brand_subtitle: Some("专业企业级加速专线".to_string()),
            credit_label: default_credit_label(),
            announcements: vec![],
            model_rename_rules: HashMap::new(),
            hidden_models: vec![],
        }
    }
}

impl UiOverrides {
    /// Filter out hidden models and rename model display names based on server rules.
    pub fn apply_to_models(&self, models: &mut Vec<ClientModelCatalogItem>) {
        models.retain(|m| !self.hidden_models.contains(&m.model_id));
        for m in models.iter_mut() {
            if let Some(new_name) = self.model_rename_rules.get(&m.model_id) {
                m.model_name = new_name.clone();
            }
        }
    }
}
