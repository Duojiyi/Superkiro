use super::{admin::AdminAuthState, json_response, BoxFuture, FacadeHandler, Response};
use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
};
use billing::engine::{BillingEngine, BillingError, CommercialUpdate};
use std::sync::Arc;
pub struct CommercialHandler {
    pub billing: BillingEngine,
    pub auth: Arc<AdminAuthState>,
    pub publish: bool,
}
impl FacadeHandler for CommercialHandler {
    fn method(&self) -> Method {
        if self.publish {
            Method::POST
        } else {
            Method::GET
        }
    }
    fn path(&self) -> &'static str {
        "/api/v1/admin/commercial-config"
    }
    fn handle<'a>(&'a self, req: Request<Body>) -> BoxFuture<'a, Response> {
        Box::pin(async move {
            if !self.auth.verify(req.headers()) {
                return json_response(
                    StatusCode::UNAUTHORIZED,
                    &serde_json::json!({"success":false,"error":"Unauthorized"}),
                );
            }
            if !self.publish {
                return json_response(
                    StatusCode::OK,
                    &serde_json::json!({"success":true,"config":self.billing.commercial_config()}),
                );
            }
            let bytes = match axum::body::to_bytes(req.into_body(), 256 * 1024).await {
                Ok(b) => b,
                Err(_) => {
                    return json_response(
                        StatusCode::BAD_REQUEST,
                        &serde_json::json!({"success":false,"error":"Invalid or oversized body"}),
                    )
                }
            };
            let update: CommercialUpdate = match serde_json::from_slice(&bytes) {
                Ok(v) => v,
                Err(e) => {
                    return json_response(
                        StatusCode::BAD_REQUEST,
                        &serde_json::json!({"success":false,"error":e.to_string()}),
                    )
                }
            };
            match self
                .billing
                .publish_commercial_config(update, crate::now_secs())
            {
                Ok(config) => json_response(
                    StatusCode::OK,
                    &serde_json::json!({"success":true,"config":config}),
                ),
                Err(e) => {
                    let status = if matches!(e, BillingError::Persistence(_)) {
                        StatusCode::SERVICE_UNAVAILABLE
                    } else {
                        StatusCode::CONFLICT
                    };
                    json_response(
                        status,
                        &serde_json::json!({"success":false,"error":e.to_string()}),
                    )
                }
            }
        })
    }
}

/// Key management is separate from model publication: discovery never grants access.
pub struct ProviderKeysHandler {
    pub billing: BillingEngine,
    pub auth: Arc<AdminAuthState>,
    pub discover: bool,
    pub runtime: Option<crate::provider::ProviderRuntimeRegistry>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct KeyUpdate {
    provider_id: String,
    key_id: String,
    api_key: Option<String>,
    allowed_models: Option<Vec<String>>,
    enabled: Option<bool>,
    weight: Option<u32>,
}

impl FacadeHandler for ProviderKeysHandler {
    fn method(&self) -> Method {
        Method::POST
    }
    fn path(&self) -> &'static str {
        if self.discover {
            "/api/v1/admin/providers/keys/discover"
        } else {
            "/api/v1/admin/providers/keys"
        }
    }
    fn handle<'a>(&'a self, req: Request<Body>) -> BoxFuture<'a, Response> {
        Box::pin(async move {
            let fail = |status, message: &str| {
                json_response(
                    status,
                    &serde_json::json!({"success":false,"error":message}),
                )
            };
            if !self.auth.verify(req.headers()) {
                return fail(StatusCode::UNAUTHORIZED, "Unauthorized");
            }
            let Ok(bytes) = axum::body::to_bytes(req.into_body(), 128 * 1024).await else {
                return fail(StatusCode::BAD_REQUEST, "Invalid body");
            };
            let Ok(u) = serde_json::from_slice::<KeyUpdate>(&bytes) else {
                return fail(StatusCode::BAD_REQUEST, "Invalid key configuration");
            };
            let valid = |v: &str| {
                !v.trim().is_empty() && v.len() <= 256 && !v.chars().any(char::is_control)
            };
            if !valid(&u.key_id) {
                return fail(StatusCode::BAD_REQUEST, "Invalid key ID");
            }
            let Some(provider) = self.billing.get_provider(&u.provider_id) else {
                return fail(StatusCode::NOT_FOUND, "Unknown provider");
            };
            let all_keys = self.billing.get_runtime_provider_keys(None);
            if all_keys
                .iter()
                .any(|k| k.id == u.key_id && k.provider_id != u.provider_id)
            {
                return fail(StatusCode::CONFLICT, "Key belongs to another provider");
            }
            let old = all_keys.into_iter().find(|k| k.id == u.key_id);
            let mut key = old.clone().unwrap_or_else(|| {
                billing::provider::ProviderKey::new(&u.key_id, &u.provider_id, "")
            });
            if let Some(secret) = u.api_key {
                if secret.trim().is_empty()
                    || secret.len() > 4096
                    || secret.chars().any(char::is_control)
                {
                    return fail(StatusCode::BAD_REQUEST, "Invalid API key");
                }
                if secret != key.api_key {
                    key.health_state = billing::provider::HealthState::Healthy;
                    key.cooldown_until = None;
                }
                key.api_key = secret;
            }
            if key.api_key.is_empty() {
                return fail(StatusCode::BAD_REQUEST, "API key required for new key");
            }
            if self.discover {
                let Ok(url) = reqwest::Url::parse(&provider.base_url) else {
                    return fail(StatusCode::BAD_REQUEST, "Invalid upstream URL");
                };
                if url.scheme() != "https"
                    && !matches!(url.host_str(), Some("localhost" | "127.0.0.1"))
                {
                    return fail(StatusCode::BAD_REQUEST, "HTTPS required");
                }
                let Ok(client) = reqwest::Client::builder()
                    .redirect(reqwest::redirect::Policy::none())
                    .timeout(std::time::Duration::from_secs(20))
                    .build()
                else {
                    return fail(StatusCode::INTERNAL_SERVER_ERROR, "HTTP client unavailable");
                };
                let base = provider.base_url.trim_end_matches('/');
                let url = format!(
                    "{}{}",
                    base,
                    if base.ends_with("/v1") {
                        "/models"
                    } else {
                        "/v1/models"
                    }
                );
                let mut request = client.get(url);
                request = match provider.format {
                    billing::provider::ProviderFormat::Anthropic => request
                        .header("x-api-key", &key.api_key)
                        .header("anthropic-version", "2023-06-01"),
                    billing::provider::ProviderFormat::OpenAi => request.bearer_auth(&key.api_key),
                };
                let Ok(mut response) = request.send().await else {
                    return fail(StatusCode::BAD_GATEWAY, "Model discovery transport failure");
                };
                if !response.status().is_success() {
                    return fail(
                        StatusCode::BAD_GATEWAY,
                        "Upstream rejected model discovery; existing permissions unchanged",
                    );
                }
                let mut data = Vec::new();
                loop {
                    match response.chunk().await {
                        Ok(Some(chunk)) if data.len() + chunk.len() <= 1024 * 1024 => {
                            data.extend_from_slice(&chunk)
                        }
                        Ok(None) => break,
                        _ => {
                            return fail(
                                StatusCode::BAD_GATEWAY,
                                "Invalid or oversized discovery response",
                            )
                        }
                    }
                }
                let Ok(value) = serde_json::from_slice::<serde_json::Value>(&data) else {
                    return fail(StatusCode::BAD_GATEWAY, "Invalid discovery JSON");
                };
                let Some(items) = value.get("data").and_then(|v| v.as_array()) else {
                    return fail(StatusCode::BAD_GATEWAY, "Missing model data array");
                };
                let models: std::collections::BTreeSet<_> = items
                    .iter()
                    .filter_map(|v| v.get("id").and_then(|v| v.as_str()))
                    .filter(|v| valid(v))
                    .collect();
                return json_response(
                    StatusCode::OK,
                    &serde_json::json!({"success":true,"models":models,"has_more":value.get("has_more").and_then(|v|v.as_bool()).unwrap_or(false),"published":false,"note":"Candidate IDs only; verify capability and pricing before explicit publication"}),
                );
            }
            let Some(mut models) = u.allowed_models else {
                return fail(
                    StatusCode::BAD_REQUEST,
                    "Explicit allowed_models required; empty array denies all models",
                );
            };
            if models.len() > 1000 || models.iter().any(|m| !valid(m)) {
                return fail(StatusCode::BAD_REQUEST, "Invalid model permissions");
            }
            models.sort();
            models.dedup();
            key.allowed_models = Some(models);
            if let Some(enabled) = u.enabled {
                key.enabled = enabled;
            }
            if let Some(weight) = u.weight {
                if weight == 0 || weight > 1000 {
                    return fail(StatusCode::BAD_REQUEST, "Weight must be 1..1000");
                }
                key.weight = weight;
            }
            if self
                .billing
                .upsert_providers_checked(Vec::new(), vec![key])
                .is_err()
            {
                return fail(StatusCode::SERVICE_UNAVAILABLE, "Key persistence failed");
            }
            if let Some(runtime) = &self.runtime {
                runtime.sync_from_billing(&self.billing);
            }
            json_response(
                StatusCode::OK,
                &serde_json::json!({"success":true,"keys":self.billing.list_provider_keys(Some(&u.provider_id)),"published":false}),
            )
        })
    }
}
