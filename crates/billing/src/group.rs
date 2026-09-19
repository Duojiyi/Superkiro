//! Tenant groups, model virtualization, and provider binding rules.
//!
//! Spec §1.4, §5, §15.

use serde::{Deserialize, Serialize};

/// Provider binding mode for tenant groups (Spec §5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderBindingMode {
    /// Uses shared upstream provider pool (NULL provider.group_id).
    #[default]
    Shared,
    /// Uses dedicated upstream providers strictly bound to this group (provider.group_id == group.id).
    Dedicated,
}

/// Tenant group definition representing virtual subscription tier and provider isolation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Group {
    pub id: String,
    pub name: String,
    pub provider_binding_mode: ProviderBindingMode,
    pub rate_card_id: String,
    pub margin_multiplier: f64,
    pub virtual_plan_name: String,
    pub virtual_usage_limit: f64,
    pub system_prompt_prefix: Option<String>,
}

impl Group {
    /// Create a standard Pro+ group (Spec §1.4).
    pub fn pro_plus(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            provider_binding_mode: ProviderBindingMode::Shared,
            rate_card_id: "default".to_string(),
            margin_multiplier: 1.0,
            virtual_plan_name: "KIRO PRO+".to_string(),
            virtual_usage_limit: 50_000.0,
            system_prompt_prefix: None,
        }
    }

    /// Create an enterprise group with dedicated provider binding and custom system prompt prefix.
    pub fn enterprise(
        id: impl Into<String>,
        name: impl Into<String>,
        virtual_plan_name: impl Into<String>,
        system_prompt_prefix: Option<String>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            provider_binding_mode: ProviderBindingMode::Dedicated,
            rate_card_id: "enterprise".to_string(),
            margin_multiplier: 1.2,
            virtual_plan_name: virtual_plan_name.into(),
            virtual_usage_limit: 500_000.0,
            system_prompt_prefix,
        }
    }

    /// Check whether this group has access to an upstream provider based on binding mode.
    ///
    /// Spec §5:
    /// - `Dedicated`: only accessible if `provider.group_id == Some(group.id)`
    /// - `Shared`: only accessible if `provider.group_id == None`
    pub fn can_access_provider(&self, provider_group_id: Option<&str>) -> bool {
        match self.provider_binding_mode {
            ProviderBindingMode::Dedicated => provider_group_id == Some(self.id.as_str()),
            ProviderBindingMode::Shared => provider_group_id.is_none(),
        }
    }
}

/// Target upstream model definition within a fallback chain (Spec §14.6, P4-10).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FallbackTarget {
    pub provider_id: String,
    pub target_model: String,
}

/// Model mapping entry for virtualizing exposed models to upstream providers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelMap {
    pub id: String,
    pub group_id: String,
    pub exposed_model_id: String,
    pub target_provider_id: String,
    pub target_model: String,
    pub context_window: u64,
    pub max_output: u64,
    pub supports_tools: bool,
    pub supports_vision: bool,
    pub supports_reasoning: bool,
    pub credit_multiplier: f64,
    pub visible: bool,
    pub sort_order: i32,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub fallback_chain: Vec<FallbackTarget>,
}

impl ModelMap {
    pub fn new(
        id: impl Into<String>,
        group_id: impl Into<String>,
        exposed_model_id: impl Into<String>,
        target_provider_id: impl Into<String>,
        target_model: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            group_id: group_id.into(),
            exposed_model_id: exposed_model_id.into(),
            target_provider_id: target_provider_id.into(),
            target_model: target_model.into(),
            context_window: 200_000,
            max_output: 64_000,
            supports_tools: true,
            supports_vision: true,
            supports_reasoning: false,
            credit_multiplier: 1.0,
            visible: true,
            sort_order: 0,
            aliases: Vec::new(),
            fallback_chain: Vec::new(),
        }
    }

    pub fn with_alias(mut self, alias: impl Into<String>) -> Self {
        self.aliases.push(alias.into());
        self
    }

    pub fn with_fallback(
        mut self,
        provider_id: impl Into<String>,
        target_model: impl Into<String>,
    ) -> Self {
        self.fallback_chain.push(FallbackTarget {
            provider_id: provider_id.into(),
            target_model: target_model.into(),
        });
        self
    }

    /// Check whether a requested model matches either the canonical ID or any alias.
    pub fn matches_model(&self, requested: &str) -> bool {
        self.exposed_model_id == requested || self.aliases.iter().any(|a| a == requested)
    }

    /// Full ordered upstream target sequence: primary target followed by fallback chain.
    pub fn full_target_chain(&self) -> Vec<FallbackTarget> {
        let mut targets = Vec::with_capacity(1 + self.fallback_chain.len());
        targets.push(FallbackTarget {
            provider_id: self.target_provider_id.clone(),
            target_model: self.target_model.clone(),
        });
        targets.extend(self.fallback_chain.clone());
        targets
    }
}
