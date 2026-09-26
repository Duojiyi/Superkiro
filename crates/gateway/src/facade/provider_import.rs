//! Facade handler for importing provider configurations (Spec §14.3, P4-6).
//!
//! Route: `POST /api/v1/admin/providers/import`

use super::virtualization::VirtualizationStore;
use super::{error_response, json_response, BoxFuture, FacadeHandler, Response};
use crate::facade::admin::AdminAuthState;
use crate::provider::import::{import_providers, ImportError, SourceFormat};
use crate::provider::ProviderRuntimeRegistry;
use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
    response::IntoResponse,
};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Clone, Serialize, Deserialize)]
pub struct ProviderImportEnvelope {
    #[serde(default)]
    pub format: SourceFormat,
    pub content: Option<serde_json::Value>,
    pub target_group_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderImportResponse {
    pub success: bool,
    pub imported_count: usize,
    pub providers: Vec<ImportedProviderSummary>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportedProviderSummary {
    pub id: String,
    pub name: String,
    pub format: String,
    pub base_url: String,
    pub models: Vec<String>,
}

/// A provider's base URL uses HTTPS, except a loopback endpoint for development.
pub(crate) fn check_base_url(base_url: &str) -> Result<(), &'static str> {
    let url = Url::parse(base_url).map_err(|_| "provider base_url must be a valid URL")?;
    if url.scheme() != "https"
        && !url
            .host_str()
            .is_some_and(|host| host == "127.0.0.1" || host == "localhost")
    {
        return Err("provider base_url must use HTTPS except for loopback development endpoints");
    }
    Ok(())
}

pub struct ProviderImportHandler {
    pub store: Option<VirtualizationStore>,
    pub billing: Option<billing::BillingEngine>,
    pub admin_auth: Option<Arc<AdminAuthState>>,
    pub runtime: Option<ProviderRuntimeRegistry>,
}

impl ProviderImportHandler {
    pub fn new() -> Self {
        Self {
            store: None,
            billing: None,
            admin_auth: None,
            runtime: None,
        }
    }

    pub fn with_store(mut self, store: VirtualizationStore) -> Self {
        self.store = Some(store);
        self
    }

    pub fn with_billing(mut self, billing: billing::BillingEngine) -> Self {
        self.billing = Some(billing);
        self
    }

    pub fn with_admin_auth(mut self, auth: Arc<AdminAuthState>) -> Self {
        self.admin_auth = Some(auth);
        self
    }

    pub fn with_runtime(mut self, runtime: ProviderRuntimeRegistry) -> Self {
        self.runtime = Some(runtime);
        self
    }
}

impl Default for ProviderImportHandler {
    fn default() -> Self {
        Self::new()
    }
}

impl FacadeHandler for ProviderImportHandler {
    fn method(&self) -> Method {
        Method::POST
    }

    fn path(&self) -> &'static str {
        "/api/v1/admin/providers/import"
    }

    fn handle<'a>(&'a self, req: Request<Body>) -> BoxFuture<'a, Response> {
        Box::pin(async move {
            // Production instances always provide AdminAuthState.  The
            // extension-only fallback exists solely for direct unit tests;
            // request extensions are not treated as an HTTP trust boundary.
            let authorized = if let Some(auth) = &self.admin_auth {
                auth.verify(req.headers())
            } else {
                req.extensions()
                    .get::<crate::auth::AuthClaims>()
                    .is_some_and(|claims| claims.group_id == "admin")
            };
            if !authorized {
                return (
                    StatusCode::UNAUTHORIZED,
                    [("content-type", "application/json")],
                    axum::Json(serde_json::json!({
                        "success": false,
                        "error": "Admin privileges required",
                    })),
                )
                    .into_response();
            }

            let body_bytes = match axum::body::to_bytes(req.into_body(), 10 * 1024 * 1024).await {
                Ok(b) => b,
                Err(e) => {
                    return (
                        StatusCode::BAD_REQUEST,
                        [("content-type", "application/json")],
                        axum::Json(serde_json::json!({
                            "success": false,
                            "error": format!("Failed to read request body: {}", e),
                        })),
                    )
                        .into_response();
                }
            };

            let body_str = match std::str::from_utf8(&body_bytes) {
                Ok(s) => s,
                Err(e) => {
                    return (
                        StatusCode::BAD_REQUEST,
                        [("content-type", "application/json")],
                        axum::Json(serde_json::json!({
                            "success": false,
                            "error": format!("Invalid UTF-8 payload: {}", e),
                        })),
                    )
                        .into_response();
                }
            };

            // Reject unsupported ownership directives before permissive raw-export parsing.
            if serde_json::from_str::<serde_json::Value>(body_str)
                .ok()
                .and_then(|value| value.get("target_group_id").cloned())
                .is_some_and(|value| !value.is_null())
            {
                return error_response(StatusCode::BAD_REQUEST, "InvalidRequestException",
                    "target_group_id is not supported by import; existing provider ownership is preserved");
            }

            // Attempt to parse as envelope first; if it doesn't match envelope with content,
            // treat the entire body as raw client export JSON.
            let (format, raw_json_str, target_group) =
                if let Ok(env) = serde_json::from_str::<ProviderImportEnvelope>(body_str) {
                    if let Some(content_val) = env.content {
                        let content_str = if let Some(s) = content_val.as_str() {
                            s.to_string()
                        } else {
                            content_val.to_string()
                        };
                        (env.format, content_str, env.target_group_id)
                    } else {
                        (
                            SourceFormat::Auto,
                            body_str.to_string(),
                            env.target_group_id,
                        )
                    }
                } else {
                    (SourceFormat::Auto, body_str.to_string(), None)
                };

            let imported = match import_providers(format, &raw_json_str) {
                Ok(list) => list,
                Err(e) => {
                    let status = match e {
                        ImportError::Json(_) => StatusCode::BAD_REQUEST,
                        _ => StatusCode::UNPROCESSABLE_ENTITY,
                    };
                    return (
                        status,
                        [("content-type", "application/json")],
                        axum::Json(serde_json::json!({
                            "success": false,
                            "error": format!("Failed to import providers: {}", e),
                        })),
                    )
                        .into_response();
                }
            };

            for item in &imported {
                if let Err(message) = check_base_url(&item.base_url) {
                    return error_response(
                        StatusCode::BAD_REQUEST,
                        "InvalidRequestException",
                        message,
                    );
                }
                if item.api_key.trim().is_empty() || item.api_key.chars().count() > 4096 {
                    return error_response(
                        StatusCode::BAD_REQUEST,
                        "InvalidRequestException",
                        "provider API key is invalid",
                    );
                }
            }

            // Import stages credentials and capabilities only. Model exposure requires
            // explicit commercial publication with group permissions and pricing.
            if target_group.is_some() {
                return error_response(StatusCode::BAD_REQUEST, "InvalidRequestException",
                    "target_group_id is not supported by import; existing provider ownership is preserved");
            }
            let (mut providers, keys): (Vec<_>, Vec<_>) = imported
                .iter()
                .map(|item| item.to_provider_and_key())
                .unzip();
            // A runtime registry alone cannot transactionally preserve ownership.
            if self.runtime.is_some() && self.billing.is_none() {
                return error_response(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "ServiceUnavailableException",
                    "provider import requires billing persistence",
                );
            }

            if let Some(billing) = &self.billing {
                match billing.import_providers_checked(providers.clone(), keys.clone()) {
                    Ok(committed) => providers = committed,
                    Err(error) => {
                        return (
                            StatusCode::SERVICE_UNAVAILABLE,
                            [("content-type", "application/json")],
                            axum::Json(serde_json::json!({
                                "success": false,
                                "error": format!("Failed to persist imported providers: {error}"),
                            })),
                        )
                            .into_response();
                    }
                }
            }

            if let Some(runtime) = &self.runtime {
                for (provider, key) in providers.into_iter().zip(keys) {
                    runtime.register(provider, key);
                }
            }

            let summaries = imported
                .into_iter()
                .map(|p| ImportedProviderSummary {
                    id: p.id,
                    name: p.name,
                    format: match p.format {
                        billing::provider::ProviderFormat::OpenAi => "openai".to_string(),
                        billing::provider::ProviderFormat::Anthropic => "anthropic".to_string(),
                    },
                    base_url: p.base_url,
                    models: p.models,
                })
                .collect::<Vec<_>>();

            let count = summaries.len();
            json_response(
                StatusCode::OK,
                &ProviderImportResponse {
                    success: true,
                    imported_count: count,
                    providers: summaries,
                    warnings: Vec::new(),
                },
            )
        })
    }
}
