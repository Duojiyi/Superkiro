//! Tenant group virtualization store for models, plans, quotas, and profiles (Spec §1.4, §5, §16-6).
//!
//! Maps each card/group to:
//! - Custom exposed model list (`model_map`)
//! - Virtual subscription plan name (e.g. "KIRO PRO+")
//! - Virtual credit quota and usage limit
//! - Stable profile ARN to satisfy Kiro's `ProfileArnGuard`

use super::models::{ModelInfo, TokenLimits};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use billing::group::ProviderBindingMode;

pub const DEFAULT_PROFILE_ARN: &str =
    "arn:aws:codewhisperer:us-east-1:123456789012:profile/KIRO_BYOK_DEFAULT";
pub const DEFAULT_GROUP_ID: &str = "group-pro-plus";

/// Virtualized group settings driving Kiro IDE UI presentation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VirtualGroup {
    pub group_id: String,
    pub group_name: String,
    pub virtual_plan_name: String,
    pub virtual_usage_limit: f64,
    pub current_usage: f64,
    pub profile_arn: String,
    pub default_model_id: String,
    pub models: Vec<ModelInfo>,
    #[serde(default)]
    pub provider_binding_mode: ProviderBindingMode,
    #[serde(default)]
    pub system_prompt_prefix: Option<String>,
}

impl Default for VirtualGroup {
    fn default() -> Self {
        Self {
            group_id: DEFAULT_GROUP_ID.to_string(),
            group_name: "Kiro Pro+ Group".to_string(),
            virtual_plan_name: "KIRO PRO+".to_string(),
            virtual_usage_limit: 50_000.0,
            current_usage: 0.0,
            profile_arn: DEFAULT_PROFILE_ARN.to_string(),
            default_model_id: "claude-sonnet-4.5".to_string(),
            provider_binding_mode: ProviderBindingMode::Shared,
            system_prompt_prefix: None,
            models: vec![
                ModelInfo {
                    model_id: "claude-sonnet-4.5".to_string(),
                    model_name: Some("Claude Sonnet 4.5 (Thinking)".to_string()),
                    description: Some("Anthropic balanced flagship reasoning model".to_string()),
                    token_limits: Some(TokenLimits {
                        max_input_tokens: 200_000,
                        max_output_tokens: 64_000,
                    }),
                    supports_reasoning: true,
                    supports_vision: true,
                    default_effort_level: Some("high".to_string()),
                },
                ModelInfo {
                    model_id: "deepseek-chat".to_string(),
                    model_name: Some("DeepSeek V3".to_string()),
                    description: Some(
                        "High performance open architecture coding model".to_string(),
                    ),
                    token_limits: Some(TokenLimits {
                        max_input_tokens: 128_000,
                        max_output_tokens: 8_192,
                    }),
                    supports_reasoning: false,
                    supports_vision: false,
                    default_effort_level: None,
                },
                ModelInfo {
                    model_id: "deepseek-reasoner".to_string(),
                    model_name: Some("DeepSeek R1".to_string()),
                    description: Some("DeepSeek reasoning model with chain of thought".to_string()),
                    token_limits: Some(TokenLimits {
                        max_input_tokens: 128_000,
                        max_output_tokens: 32_000,
                    }),
                    supports_reasoning: true,
                    supports_vision: false,
                    default_effort_level: Some("medium".to_string()),
                },
            ],
        }
    }
}

/// In-memory virtualization store holding group definitions and model mappings.
#[derive(Debug, Clone)]
pub struct VirtualizationStore {
    groups: Arc<RwLock<HashMap<String, VirtualGroup>>>,
    default_group_id: String,
    billing: Option<billing::engine::BillingEngine>,
    fallback_model: Arc<RwLock<Option<ModelInfo>>>,
}

impl Default for VirtualizationStore {
    fn default() -> Self {
        let store = Self {
            groups: Arc::new(RwLock::new(HashMap::new())),
            default_group_id: DEFAULT_GROUP_ID.to_string(),
            billing: None,
            fallback_model: Arc::new(RwLock::new(None)),
        };
        store.upsert_group(VirtualGroup::default());
        store
    }
}

impl VirtualizationStore {
    /// Create new empty store with specified default group ID.
    pub fn new(default_group_id: &str) -> Self {
        Self {
            groups: Arc::new(RwLock::new(HashMap::new())),
            default_group_id: default_group_id.to_string(),
            billing: None,
            fallback_model: Arc::new(RwLock::new(None)),
        }
    }

    /// Create new store connected directly to BillingEngine for live group and model sync.
    pub fn with_billing(billing: billing::engine::BillingEngine, default_group_id: &str) -> Self {
        let store = Self {
            groups: Arc::new(RwLock::new(HashMap::new())),
            default_group_id: default_group_id.to_string(),
            billing: Some(billing),
            fallback_model: Arc::new(RwLock::new(None)),
        };
        store.upsert_group(VirtualGroup::default());
        store
    }

    /// Advertise only the actual environment-configured fallback model.
    pub fn set_fallback_model(&self, model: &str) {
        *self.fallback_model.write().unwrap() = Some(ModelInfo {
            model_id: model.to_string(),
            model_name: Some(model.to_string()),
            description: Some("Configured upstream model".to_string()),
            token_limits: Some(TokenLimits {
                max_input_tokens: billing::context::ModelContextLibrary::resolve(model)
                    .context_window as u64,
                max_output_tokens: 4096,
            }),
            supports_reasoning: false,
            supports_vision: false,
            default_effort_level: None,
        });
    }

    /// Add or update a virtual group definition.
    pub fn upsert_group(&self, group: VirtualGroup) {
        let mut w = self.groups.write().unwrap();
        w.insert(group.group_id.clone(), group);
    }

    /// Access the underlying billing engine if attached.
    pub fn billing(&self) -> Option<&billing::engine::BillingEngine> {
        self.billing.as_ref()
    }

    /// Retrieve a group by ID, or fall back to the default group.
    pub fn get_group(&self, group_id: Option<&str>) -> VirtualGroup {
        let target_id = group_id.unwrap_or(&self.default_group_id);

        if let Some(ref billing) = self.billing {
            if let Some(bg) = billing.get_group(target_id) {
                // Fetch models from billing mapped to this group (respecting visibility and sort_order)
                let billing_models = billing.list_models_for_group(&bg.id, false);
                let models: Vec<ModelInfo> = if !billing_models.is_empty() {
                    billing_models
                        .into_iter()
                        .filter(|m| m.visible)
                        .map(|m| ModelInfo {
                            model_id: m.exposed_model_id.clone(),
                            model_name: Some(m.exposed_model_id.clone()),
                            description: Some(format!("Model mapped to target {}", m.target_model)),
                            token_limits: Some(TokenLimits {
                                max_input_tokens: m.context_window,
                                max_output_tokens: m.max_output.clamp(1, 32_000),
                            }),
                            supports_reasoning: m.supports_reasoning,
                            supports_vision: m.supports_vision,
                            default_effort_level: if m.supports_reasoning {
                                Some("medium".to_string())
                            } else {
                                None
                            },
                        })
                        .collect()
                } else {
                    self.fallback_model
                        .read()
                        .unwrap()
                        .iter()
                        .cloned()
                        .collect()
                };

                let default_model_id = models
                    .first()
                    .map(|m| m.model_id.clone())
                    .unwrap_or_default();

                return VirtualGroup {
                    group_id: bg.id,
                    group_name: bg.name,
                    virtual_plan_name: bg.virtual_plan_name,
                    virtual_usage_limit: bg.virtual_usage_limit,
                    current_usage: 0.0,
                    profile_arn: DEFAULT_PROFILE_ARN.to_string(),
                    default_model_id,
                    models,
                    provider_binding_mode: bg.provider_binding_mode,
                    system_prompt_prefix: bg.system_prompt_prefix,
                };
            }
        }

        let r = self.groups.read().unwrap();
        if let Some(g) = r.get(target_id) {
            return g.clone();
        }
        if let Some(default_group) = r.get(&self.default_group_id) {
            default_group.clone()
        } else {
            VirtualGroup::default()
        }
    }

    /// Update usage credits for a group.
    pub fn update_usage(&self, group_id: &str, current_usage: f64) {
        let mut w = self.groups.write().unwrap();
        if let Some(g) = w.get_mut(group_id) {
            g.current_usage = current_usage;
        }
    }

    /// Add or update a model definition within a group.
    pub fn upsert_model(&self, group_id: &str, model: ModelInfo) {
        let mut w = self.groups.write().unwrap();
        let group = w
            .entry(group_id.to_string())
            .or_insert_with(|| VirtualGroup {
                group_id: group_id.to_string(),
                ..Default::default()
            });
        if let Some(pos) = group
            .models
            .iter()
            .position(|m| m.model_id == model.model_id)
        {
            group.models[pos] = model;
        } else {
            group.models.push(model);
        }
    }
}
