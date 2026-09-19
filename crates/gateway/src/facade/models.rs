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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_effort_level: Option<String>,
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
                    let schema = if m.supports_reasoning {
                        Some(serde_json::json!({
                            "type": "object",
                            "properties": {
                                "output_config": {
                                    "type": "object",
                                    "properties": {
                                        "effort": {
                                            "type": "string",
                                            "enum": ["low", "medium", "high", "xhigh", "max"]
                                        }
                                    }
                                }
                            }
                        }))
                    } else {
                        None
                    };

                    ExposedModelItem {
                        model_id: m.model_id,
                        model_name: m.model_name,
                        description: m.description,
                        token_limits: m.token_limits,
                        additional_model_request_fields_schema: schema,
                        default_effort_level: m.default_effort_level,
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
