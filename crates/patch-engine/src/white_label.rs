//! Client white-label configuration model, local file persistence, and environment variable overrides (Spec §14.6, P4-8).
//!
//! Enables multi-tenant and partner re-branding:
//! - App Name & Subtitle
//! - App Icon (URL / Base64 / Local URI)
//! - Primary & Accent Theme Colors
//! - Theme Mode (Auto / Dark / Light)
//! - Official Website, Documentation URL, and Support Contacts
//! - Copyright notice & optional custom styling CSS

use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum WhiteLabelError {
    #[error("I/O error handling white-label config: {0}")]
    Io(#[from] std::io::Error),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
}

/// Theme appearance mode for client UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ThemeMode {
    #[default]
    Auto,
    Dark,
    Light,
}

/// White-label configuration driving client branding and appearance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WhiteLabelConfig {
    pub app_name: String,
    pub app_slug: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subtitle: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon_uri: Option<String>,
    pub primary_color: String,
    pub accent_color: String,
    pub theme_mode: ThemeMode,
    pub official_website: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub docs_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub support_contact: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub copyright: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_css: Option<String>,
}

impl Default for WhiteLabelConfig {
    fn default() -> Self {
        Self {
            app_name: "KIRO 极速接管中心".to_string(),
            app_slug: "kiro-byok".to_string(),
            subtitle: Some("专业企业级 AI 研发加速专线".to_string()),
            icon_uri: Some("asset://icons/brand-logo.svg".to_string()),
            primary_color: "#6366F1".to_string(), // Indigo-500
            accent_color: "#4F46E5".to_string(),  // Indigo-600
            theme_mode: ThemeMode::Auto,
            official_website: "https://byok.kiro.dev".to_string(),
            docs_url: Some("https://byok.kiro.dev/docs".to_string()),
            support_contact: Some("support@kiro.dev".to_string()),
            copyright: Some("© 2026 Kiro BYOK Studio".to_string()),
            custom_css: None,
        }
    }
}

impl WhiteLabelConfig {
    /// Create new config with app name and official website.
    pub fn new(app_name: impl Into<String>, official_website: impl Into<String>) -> Self {
        let name = app_name.into();
        let slug = name
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() {
                    c.to_ascii_lowercase()
                } else {
                    '-'
                }
            })
            .collect::<String>()
            .trim_matches('-')
            .to_string();

        Self {
            app_name: name,
            app_slug: if slug.is_empty() {
                "branded-client".to_string()
            } else {
                slug
            },
            official_website: official_website.into(),
            ..Default::default()
        }
    }

    /// Apply environment variable overrides if present.
    pub fn with_env_overrides(mut self) -> Self {
        if let Ok(val) = std::env::var("KIRO_BRAND_APP_NAME") {
            if !val.trim().is_empty() {
                self.app_name = val.trim().to_string();
            }
        }
        if let Ok(val) = std::env::var("KIRO_BRAND_APP_SLUG") {
            if !val.trim().is_empty() {
                self.app_slug = val.trim().to_string();
            }
        }
        if let Ok(val) = std::env::var("KIRO_BRAND_SUBTITLE") {
            self.subtitle = Some(val.trim().to_string());
        }
        if let Ok(val) = std::env::var("KIRO_BRAND_ICON_URI") {
            self.icon_uri = Some(val.trim().to_string());
        }
        if let Ok(val) = std::env::var("KIRO_BRAND_PRIMARY_COLOR") {
            if !val.trim().is_empty() {
                self.primary_color = val.trim().to_string();
            }
        }
        if let Ok(val) = std::env::var("KIRO_BRAND_ACCENT_COLOR") {
            if !val.trim().is_empty() {
                self.accent_color = val.trim().to_string();
            }
        }
        if let Ok(val) = std::env::var("KIRO_BRAND_WEBSITE") {
            if !val.trim().is_empty() {
                self.official_website = val.trim().to_string();
            }
        }
        if let Ok(val) = std::env::var("KIRO_BRAND_DOCS_URL") {
            self.docs_url = Some(val.trim().to_string());
        }
        if let Ok(val) = std::env::var("KIRO_BRAND_SUPPORT") {
            self.support_contact = Some(val.trim().to_string());
        }
        if let Ok(val) = std::env::var("KIRO_BRAND_COPYRIGHT") {
            self.copyright = Some(val.trim().to_string());
        }
        self
    }

    /// Merge server-distributed configuration overrides into local configuration.
    pub fn merge_with(&mut self, server: &WhiteLabelConfig) {
        self.app_name = server.app_name.clone();
        self.app_slug = server.app_slug.clone();
        if server.subtitle.is_some() {
            self.subtitle = server.subtitle.clone();
        }
        if server.icon_uri.is_some() {
            self.icon_uri = server.icon_uri.clone();
        }
        self.primary_color = server.primary_color.clone();
        self.accent_color = server.accent_color.clone();
        self.theme_mode = server.theme_mode;
        self.official_website = server.official_website.clone();
        if server.docs_url.is_some() {
            self.docs_url = server.docs_url.clone();
        }
        if server.support_contact.is_some() {
            self.support_contact = server.support_contact.clone();
        }
        if server.copyright.is_some() {
            self.copyright = server.copyright.clone();
        }
        if server.custom_css.is_some() {
            self.custom_css = server.custom_css.clone();
        }
    }

    /// Save configuration atomically to disk.
    pub fn save_to_file<P: AsRef<Path>>(&self, path: P) -> Result<(), WhiteLabelError> {
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

    /// Load configuration from disk, falling back to default with env overrides if missing.
    pub fn load_or_default<P: AsRef<Path>>(path: P) -> Result<Self, WhiteLabelError> {
        let path = path.as_ref();
        if !path.exists() {
            return Ok(Self::default().with_env_overrides());
        }
        let raw = fs::read_to_string(path)?;
        let parsed: Self = serde_json::from_str(&raw)?;
        Ok(parsed.with_env_overrides())
    }
}

/// Return default path for local brand configuration file.
pub fn default_brand_config_path() -> PathBuf {
    crate::preferences::default_preferences_path()
        .parent()
        .map(|p| p.join("brand.json"))
        .unwrap_or_else(|| std::env::temp_dir().join("kiro_brand.json"))
}
