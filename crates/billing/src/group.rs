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
    /// Disable new issuance without changing existing cards or model access.
    #[serde(default = "issuance_enabled_by_default")]
    pub issuance_enabled: bool,
    pub provider_binding_mode: ProviderBindingMode,
    pub rate_card_id: String,
    pub margin_multiplier: f64,
    pub virtual_plan_name: String,
    pub virtual_usage_limit: f64,
    pub system_prompt_prefix: Option<String>,
}

fn issuance_enabled_by_default() -> bool {
    true
}

impl Group {
    /// Create a standard Pro+ group (Spec §1.4).
    pub fn pro_plus(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            issuance_enabled: true,
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
            issuance_enabled: true,
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
    /// Optional request-specific surcharge when reasoning reaches this effort.
    #[serde(default)]
    pub thinking_surcharge_multiplier: Option<f64>,
    #[serde(default)]
    pub thinking_surcharge_threshold: Option<String>,
    /// Optional request-specific surcharge when estimated input exceeds this token count.
    #[serde(default)]
    pub context_surcharge_multiplier: Option<f64>,
    #[serde(default)]
    pub context_surcharge_threshold: Option<u64>,
    pub visible: bool,
    pub sort_order: i32,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub fallback_chain: Vec<FallbackTarget>,
    /// The name in the customer's model list; derived from the exposed id when unset.
    #[serde(default)]
    pub display_name: Option<String>,
    /// The description in the customer's model list; never names the upstream target.
    #[serde(default)]
    pub description: Option<String>,
    /// The cost multiplier the model list shows, as in "2.2x Credit". Informational: what
    /// a request is charged comes from the price versions. Derived from prices when unset.
    #[serde(default)]
    pub rate_multiplier: Option<f64>,
    /// Withdrawn for good: never listed, and a request for it is refused as for a model the
    /// group does not list. Unlike a hidden model, it cannot be requested by its ID.
    #[serde(default)]
    pub retired: bool,
}

/// What a request may name as its model, and what a publication may expose: 1-128 ASCII
/// letters, digits and `- _ . : /`.
pub fn valid_model_id(id: &str) -> bool {
    (1..=128).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':' | b'/'))
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
            thinking_surcharge_multiplier: None,
            thinking_surcharge_threshold: None,
            context_surcharge_multiplier: None,
            context_surcharge_threshold: None,
            visible: true,
            sort_order: 0,
            aliases: Vec::new(),
            fallback_chain: Vec::new(),
            display_name: None,
            description: None,
            rate_multiplier: None,
            retired: false,
        }
    }

    /// Resolve the optional request surcharge. Built-in defaults keep the initial FABLE/GPT
    /// rollout enabled while every other model remains at 1x; published fields override them.
    pub fn billing_multiplier(&self, effort: Option<&str>, input_tokens: u64) -> f64 {
        let fable = self.exposed_model_id.starts_with("claude-fable-")
            || self.target_model.starts_with("claude-fable-");
        let gpt =
            self.exposed_model_id.starts_with("gpt-") || self.target_model.starts_with("gpt-");
        let thinking_threshold = self
            .thinking_surcharge_threshold
            .as_deref()
            .or(fable.then_some("max"));
        let thinking_multiplier = self.thinking_surcharge_multiplier.or(fable.then_some(3.0));
        let context_threshold = self.context_surcharge_threshold.or(gpt.then_some(272_000));
        let context_multiplier = self.context_surcharge_multiplier.or(gpt.then_some(2.0));
        let effort_rank = |value: &str| match value.to_ascii_lowercase().as_str() {
            "low" => 0,
            "medium" => 1,
            "high" => 2,
            "xhigh" => 3,
            "max" => 4,
            "ultra" => 5,
            _ => -1,
        };
        let thinking_on = match (effort, thinking_threshold) {
            (Some(actual), Some(threshold)) => effort_rank(actual) >= effort_rank(threshold),
            _ => false,
        };
        let thinking = if thinking_on {
            thinking_multiplier.unwrap_or(1.0)
        } else {
            1.0
        };
        let context = if context_threshold.is_some_and(|threshold| input_tokens > threshold) {
            context_multiplier.unwrap_or(1.0)
        } else {
            1.0
        };
        (thinking * context).max(1.0)
    }

    /// Whether customers see the model: visible and not retired.
    pub fn is_listed(&self) -> bool {
        self.visible && !self.retired
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

#[cfg(test)]
mod tests {
    use super::ModelMap;

    fn model(id: &str) -> ModelMap {
        ModelMap::new("m", "g", id, "p", id)
    }

    #[test]
    fn fable_max_effort_is_three_times_and_other_efforts_are_normal() {
        let m = model("claude-fable-5");
        assert_eq!(m.billing_multiplier(Some("high"), 1), 1.0);
        assert_eq!(m.billing_multiplier(Some("max"), 1), 3.0);
        assert_eq!(m.billing_multiplier(Some("ultra"), 1), 3.0);
    }

    #[test]
    fn gpt_context_threshold_is_strictly_greater_than_272k() {
        let m = model("gpt-6-astra");
        assert_eq!(m.billing_multiplier(None, 272_000), 1.0);
        assert_eq!(m.billing_multiplier(None, 272_001), 2.0);
    }

    #[test]
    fn unrelated_models_are_not_surcharged() {
        let m = model("claude-sonnet");
        assert_eq!(m.billing_multiplier(Some("max"), 1_000_000), 1.0);
    }

    #[test]
    fn published_overrides_are_generic_and_composable() {
        let mut m = model("custom-model");
        m.thinking_surcharge_threshold = Some("high".into());
        m.thinking_surcharge_multiplier = Some(1.5);
        m.context_surcharge_threshold = Some(100);
        m.context_surcharge_multiplier = Some(2.0);
        assert_eq!(m.billing_multiplier(Some("max"), 101), 3.0);
        assert_eq!(m.billing_multiplier(Some("medium"), 101), 2.0);
    }
}
