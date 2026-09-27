//! Model list handler.
//!
//! Spec §4.2, P0-5 (mgmt-schema.md §2.1).

use super::virtualization::VirtualizationStore;
use super::{json_response, BoxFuture, FacadeHandler, Response};
use crate::auth::AuthClaims;
use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TokenLimits {
    pub max_input_tokens: u64,
    pub max_output_tokens: u64,
}

impl TokenLimits {
    /// Match commercial config validation: 0 < output <= context <= 10M.
    /// Also bound legacy or programmatically inserted mappings before u32 conversion.
    pub(super) fn configured(context_window: u64, max_output: u64) -> Self {
        let max_input_tokens = context_window.clamp(1, 10_000_000);
        Self {
            max_input_tokens,
            max_output_tokens: max_output.clamp(1, max_input_tokens),
        }
    }
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    pub model_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_limits: Option<TokenLimits>,
    #[serde(default)]
    pub supports_reasoning: bool,
    #[serde(default = "default_true")]
    pub supports_vision: bool,
    /// The level Kiro starts a reasoning model at; the model's own default when unset or
    /// not one of its levels.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_effort_level: Option<String>,
    /// The effort levels the model takes, lowest first; empty when they follow from its ID.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effort_levels: Vec<String>,
    /// Shown by Kiro beside the model as "{rate}x Credit".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_multiplier: Option<f64>,
}

impl ModelInfo {
    /// The effort levels Kiro offers for this model, lowest first, and the one it starts
    /// at, when the model reasons.
    pub fn effort(&self) -> Option<(Vec<String>, String)> {
        if !self.supports_reasoning {
            return None;
        }
        let (own_levels, own_default) = crate::provider::family::effort_levels(&self.model_id);
        let levels: Vec<String> = if self.effort_levels.is_empty() {
            own_levels.iter().map(|level| level.to_string()).collect()
        } else {
            self.effort_levels.clone()
        };
        let default = self
            .default_effort_level
            .clone()
            .filter(|level| levels.contains(level))
            .or_else(|| {
                levels
                    .iter()
                    .find(|level| level.as_str() == own_default)
                    .cloned()
            })
            .or_else(|| levels.first().cloned())?;
        Some((levels, default))
    }
}

/// The request fields a reasoning model takes, as Kiro reads them: the levels from the
/// `enum` of `output_config.effort` and the level it starts at from its `default`. With no
/// `default` Kiro started every model at its first level, "low", whatever the operator or
/// the model meant.
fn effort_schema(levels: &[String], default: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "output_config": {
                "type": "object",
                "properties": {
                    "effort": {
                        "type": "string",
                        "enum": levels,
                        "default": default
                    }
                }
            }
        }
    })
}

/// The unit a turn's credits are metered in: Kiro's own "credit". Kiro adds up a turn's
/// usage by this unit, starts an interrupted turn at 0 in it, and its telemetry knows only
/// this one ("Credit" counted as "other").
pub const RATE_UNIT: &str = "credit";
/// The same unit for an amount other than one, as Kiro's usage summary prints it ("Est.
/// Credits Used").
pub const RATE_UNIT_PLURAL: &str = "Credits";
/// The unit Kiro prints after a model's multiplier in its model list: "2.2x Credit".
pub const MODEL_RATE_UNIT: &str = "Credit";

/// The model Kiro asks for when it wants a quick answer: commit messages, spec sub-intents,
/// session recaps and titles. Its own service never lists it, and neither does the gateway.
pub const SIMPLE_TASK_MODEL: &str = "simple-task";

/// Kiro's "Auto (model selected dynamically on the server)", which a custom agent may pin.
/// The gateway's choice is the group's default model, the one its model list names first
/// and Kiro starts a session on: "auto" is the customer leaving the choice to the service.
pub const AUTO_MODEL: &str = "auto";

/// Why a group's `simple-task` requests go to the model they go to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum FastModelChoice {
    /// The mapping the operator gave the alias `simple-task`.
    Alias,
    /// No alias: the listed model that costs least at the price in force.
    Cheapest,
    /// No alias and no listed model with a price: the group's default model.
    Default,
}

/// The mapping Kiro's fast model resolves to in `group_id`: the one the operator gave the
/// alias `simple-task`, listed or hidden; else the listed model whose million input and
/// million output tokens cost least at the price in force at `now`, the first in the list
/// among equals; else the group's default. Kiro sends commit messages and one sub-intent
/// per spec-mode message to it, each held at its maximum output: sent to the group's first
/// model, that was often its flagship, at the flagship's price.
pub fn simple_task_model(
    billing: &billing::engine::BillingEngine,
    group_id: &str,
    now: u64,
) -> Option<(billing::group::ModelMap, FastModelChoice)> {
    let models = billing.list_models_for_group(group_id, false);
    if let Some(aliased) = models
        .iter()
        .find(|model| !model.retired && model.matches_model(SIMPLE_TASK_MODEL))
    {
        return Some((aliased.clone(), FastModelChoice::Alias));
    }
    let listed: Vec<_> = models
        .into_iter()
        .filter(|model| model.is_listed())
        .collect();
    listed
        .iter()
        .filter_map(|model| {
            let price = billing
                .display_price(group_id, &model.exposed_model_id, now)
                .filter(|price| *price > 0)?;
            Some((price, model))
        })
        // The first of equals: the list is in the group's order.
        .min_by_key(|(price, _)| *price)
        .map(|(_, model)| (model.clone(), FastModelChoice::Cheapest))
        .or_else(|| {
            listed
                .first()
                .map(|model| (model.clone(), FastModelChoice::Default))
        })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExposedModelItem {
    pub model_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_limits: Option<TokenLimits>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub additional_model_request_fields_schema: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_effort_level: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate_multiplier: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate_unit: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListAvailableModelsResponse {
    pub models: Vec<ExposedModelItem>,
    pub default_model: String,
    pub next_token: Option<String>,
}

/// Handler for `GET /ListAvailableModels`
#[derive(Clone, Default)]
pub struct ListAvailableModelsHandler {
    store: VirtualizationStore,
}

impl ListAvailableModelsHandler {
    pub fn new(store: VirtualizationStore) -> Self {
        Self { store }
    }
}

impl FacadeHandler for ListAvailableModelsHandler {
    fn method(&self) -> Method {
        Method::GET
    }

    fn path(&self) -> &'static str {
        "/ListAvailableModels"
    }

    fn handle<'a>(&'a self, req: Request<Body>) -> BoxFuture<'a, Response> {
        Box::pin(async move {
            let claims = req.extensions().get::<AuthClaims>();
            let group = self.store.get_group(claims.map(|c| c.group_id.as_str()));

            let exposed_models: Vec<ExposedModelItem> = group
                .models
                .into_iter()
                .map(|m| {
                    let effort = m.effort();
                    ExposedModelItem {
                        additional_model_request_fields_schema: effort
                            .as_ref()
                            .map(|(levels, default)| effort_schema(levels, default)),
                        default_effort_level: effort.map(|(_, default)| default),
                        model_id: m.model_id,
                        model_name: m.model_name,
                        description: m.description,
                        token_limits: m.token_limits,
                        rate_unit: m.rate_multiplier.map(|_| MODEL_RATE_UNIT.to_string()),
                        rate_multiplier: m.rate_multiplier,
                    }
                })
                .collect();

            let resp = ListAvailableModelsResponse {
                models: exposed_models,
                default_model: group.default_model_id,
                next_token: None,
            };

            json_response(StatusCode::OK, &resp)
        })
    }
}

#[cfg(test)]
mod capability_tests {
    use super::TokenLimits;

    #[test]
    fn configured_limits_preserve_valid_values_and_bound_legacy_data() {
        for (context, output, expected_context, expected_output) in [
            (1_000_000, 128_000, 1_000_000, 128_000),
            (10_000_000, 10_000_000, 10_000_000, 10_000_000),
            (0, 0, 1, 1),
            (1_000, 2_000, 1_000, 1_000),
            (u64::MAX, u64::MAX, 10_000_000, 10_000_000),
        ] {
            assert_eq!(
                TokenLimits::configured(context, output),
                TokenLimits {
                    max_input_tokens: expected_context,
                    max_output_tokens: expected_output,
                }
            );
        }
    }
}
