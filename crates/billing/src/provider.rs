//! Provider and ProviderKey domain models and health lifecycle.
//!
//! Spec §5 (Data Model) & Spec §14.3 (Provider Governance).

use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Upstream provider API format (Spec §5, §15.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderFormat {
    #[default]
    OpenAi,
    Anthropic,
}

/// Provider or Key health state (Spec §5, §14.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthState {
    #[default]
    Healthy,
    Degraded,
    Unhealthy,
}

/// Upstream LLM provider configuration and routing target (Spec §5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provider {
    pub id: String,
    pub name: String,
    pub format: ProviderFormat,
    pub base_url: String,
    pub enabled: bool,
    pub weight: u32,
    pub health_state: HealthState,
    pub cooldown_until: Option<u64>,
    pub group_id: Option<String>,
}

impl Provider {
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        format: ProviderFormat,
        base_url: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            format,
            base_url: base_url.into(),
            enabled: true,
            weight: 1,
            health_state: HealthState::Healthy,
            cooldown_until: None,
            group_id: None,
        }
    }

    pub fn with_weight(mut self, weight: u32) -> Self {
        self.weight = weight.max(1);
        self
    }

    pub fn with_group(mut self, group_id: impl Into<String>) -> Self {
        self.group_id = Some(group_id.into());
        self
    }

    pub fn is_available(&self, now_secs: u64) -> bool {
        if !self.enabled || self.health_state == HealthState::Unhealthy {
            return false;
        }
        if let Some(cooldown) = self.cooldown_until {
            if now_secs < cooldown {
                return false;
            }
        }
        true
    }

    pub fn can_access(&self, group: &crate::group::Group) -> bool {
        group.can_access_provider(self.group_id.as_deref())
    }

    pub fn mark_failure(&mut self, now_secs: u64, cooldown: Duration) {
        self.health_state = HealthState::Degraded;
        self.cooldown_until = Some(now_secs + cooldown.as_secs());
    }

    pub fn mark_success(&mut self) {
        self.health_state = HealthState::Healthy;
        self.cooldown_until = None;
    }
}

/// Upstream provider API authentication key (Spec §5, §7, §14.3).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderKey {
    /// None preserves legacy unrestricted keys; Some(empty) denies all models.
    #[serde(default)]
    pub allowed_models: Option<Vec<String>>,
    pub id: String,
    pub provider_id: String,
    #[serde(skip_serializing, default)]
    pub api_key: String,
    pub api_key_encrypted: Option<String>,
    pub enabled: bool,
    pub weight: u32,
    pub health_state: HealthState,
    pub cooldown_until: Option<u64>,
}

impl std::fmt::Debug for ProviderKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderKey")
            .field("id", &self.id)
            .field("provider_id", &self.provider_id)
            .field("api_key", &crate::crypto::mask_provider_key(&self.api_key))
            .field(
                "api_key_encrypted",
                &self.api_key_encrypted.as_ref().map(|_| "[redacted]"),
            )
            .field("enabled", &self.enabled)
            .field("weight", &self.weight)
            .field("health_state", &self.health_state)
            .field("cooldown_until", &self.cooldown_until)
            .finish()
    }
}

impl ProviderKey {
    pub fn new(
        id: impl Into<String>,
        provider_id: impl Into<String>,
        api_key: impl Into<String>,
    ) -> Self {
        Self {
            allowed_models: None,
            id: id.into(),
            provider_id: provider_id.into(),
            api_key: api_key.into(),
            api_key_encrypted: None,
            enabled: true,
            weight: 1,
            health_state: HealthState::Healthy,
            cooldown_until: None,
        }
    }

    pub fn supports_model(&self, model: &str) -> bool {
        self.allowed_models
            .as_ref()
            .is_none_or(|models| models.iter().any(|m| m == model))
    }

    pub fn with_encrypted(mut self, encrypted: impl Into<String>) -> Self {
        self.api_key_encrypted = Some(encrypted.into());
        self
    }

    /// Return a safe masked version of the provider key for Admin UI and logging (Spec §7).
    pub fn masked_key(&self) -> String {
        crate::crypto::mask_provider_key(&self.api_key)
    }

    pub fn with_weight(mut self, weight: u32) -> Self {
        self.weight = weight.max(1);
        self
    }

    pub fn is_available(&self, now_secs: u64) -> bool {
        if !self.enabled || self.health_state == HealthState::Unhealthy {
            return false;
        }
        if let Some(cooldown) = self.cooldown_until {
            if now_secs < cooldown {
                return false;
            }
        }
        true
    }

    pub fn mark_failure(&mut self, now_secs: u64, cooldown: Duration) {
        self.health_state = HealthState::Degraded;
        self.cooldown_until = Some(now_secs + cooldown.as_secs());
    }

    pub fn mark_success(&mut self) {
        self.health_state = HealthState::Healthy;
        self.cooldown_until = None;
    }
}
