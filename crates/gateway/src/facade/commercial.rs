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

/// Provider upkeep outside model publication. Every change is saved before the running
/// gateway takes it up.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ProviderAction {
    /// Edit a provider's name, URL or format; its Keys are left as they are.
    Update,
    /// Delete a provider no mapping names and no Key belongs to.
    Delete,
    /// Delete a Key no model customers see depends on.
    DeleteKey,
    /// Clear a Key's cooldown or retirement in the running gateway.
    ResetKey,
    /// One test call, sent as traffic is.
    ProbeKey,
}

pub struct ProviderMaintenanceHandler {
    billing: BillingEngine,
    auth: Arc<AdminAuthState>,
    runtime: Option<crate::provider::ProviderRuntimeRegistry>,
    action: ProviderAction,
}

impl ProviderMaintenanceHandler {
    pub fn new(
        billing: BillingEngine,
        auth: Arc<AdminAuthState>,
        runtime: Option<crate::provider::ProviderRuntimeRegistry>,
        action: ProviderAction,
    ) -> Self {
        Self {
            billing,
            auth,
            runtime,
            action,
        }
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderEdit {
    id: String,
    name: Option<String>,
    base_url: Option<String>,
    format: Option<billing::provider::ProviderFormat>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderRef {
    id: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct KeyRef {
    provider_id: String,
    key_id: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ProbeRequest {
    provider_id: String,
    model: String,
    key_id: Option<String>,
}

fn failure(status: StatusCode, message: &str) -> Response {
    json_response(
        status,
        &serde_json::json!({"success":false,"error":message}),
    )
}

fn plain_text(value: &str, max: usize) -> bool {
    !value.trim().is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}

impl ProviderMaintenanceHandler {
    /// A refused change names what blocks it; one that could not be saved changed nothing.
    fn refused(&self, error: BillingError) -> Response {
        match error {
            BillingError::InvalidState(message) => failure(StatusCode::CONFLICT, &message),
            _ => failure(
                StatusCode::SERVICE_UNAVAILABLE,
                "Provider persistence failed",
            ),
        }
    }

    fn sync(&self) {
        if let Some(runtime) = &self.runtime {
            runtime.sync_from_billing(&self.billing);
        }
    }

    fn update(&self, bytes: &[u8]) -> Response {
        let Ok(ProviderEdit {
            id,
            name,
            base_url,
            format,
        }) = serde_json::from_slice(bytes)
        else {
            return failure(StatusCode::BAD_REQUEST, "Invalid provider update");
        };
        if name.is_none() && base_url.is_none() && format.is_none() {
            return failure(StatusCode::BAD_REQUEST, "Nothing to update");
        }
        if name.as_deref().is_some_and(|name| !plain_text(name, 256)) {
            return failure(StatusCode::BAD_REQUEST, "Invalid provider name");
        }
        if let Some(Err(message)) = base_url
            .as_deref()
            .map(super::provider_import::check_base_url)
        {
            return failure(StatusCode::BAD_REQUEST, message);
        }
        let edited = self.billing.edit_provider(&id, |provider| {
            if let Some(name) = name {
                provider.name = name.trim().to_string();
            }
            if let Some(base_url) = base_url {
                provider.base_url = base_url;
            }
            if let Some(format) = format {
                provider.format = format;
            }
        });
        match edited {
            Ok(true) => {}
            Ok(false) => return failure(StatusCode::NOT_FOUND, "Unknown provider"),
            Err(error) => return self.refused(error),
        }
        self.sync();
        json_response(
            StatusCode::OK,
            &serde_json::json!({"success":true,"provider":self.billing.get_provider(&id)}),
        )
    }

    fn delete(&self, bytes: &[u8]) -> Response {
        let Ok(target) = serde_json::from_slice::<ProviderRef>(bytes) else {
            return failure(StatusCode::BAD_REQUEST, "Invalid provider reference");
        };
        match self.billing.delete_provider(&target.id) {
            Ok(true) => {}
            Ok(false) => return failure(StatusCode::NOT_FOUND, "Unknown provider"),
            Err(error) => return self.refused(error),
        }
        self.sync();
        json_response(StatusCode::OK, &serde_json::json!({"success":true}))
    }

    /// The Key the request names, as saved, or why it names none.
    fn key(&self, bytes: &[u8]) -> Result<KeyRef, (StatusCode, &'static str)> {
        let target = serde_json::from_slice::<KeyRef>(bytes)
            .map_err(|_| (StatusCode::BAD_REQUEST, "Invalid key reference"))?;
        if self.billing.get_provider(&target.provider_id).is_none() {
            return Err((StatusCode::NOT_FOUND, "Unknown provider"));
        }
        if !self
            .billing
            .list_provider_keys(Some(&target.provider_id))
            .iter()
            .any(|key| key.id == target.key_id)
        {
            return Err((StatusCode::NOT_FOUND, "Unknown key"));
        }
        Ok(target)
    }

    fn delete_key(&self, bytes: &[u8]) -> Response {
        let target = match self.key(bytes) {
            Ok(target) => target,
            Err((status, message)) => return failure(status, message),
        };
        match self
            .billing
            .delete_provider_key(&target.provider_id, &target.key_id)
        {
            Ok(true) => {}
            Ok(false) => return failure(StatusCode::NOT_FOUND, "Unknown key"),
            Err(error) => return self.refused(error),
        }
        self.sync();
        json_response(StatusCode::OK, &serde_json::json!({"success":true}))
    }

    fn reset_key(&self, bytes: &[u8]) -> Response {
        let target = match self.key(bytes) {
            Ok(target) => target,
            Err((status, message)) => return failure(status, message),
        };
        if let Some(runtime) = &self.runtime {
            runtime.reset_key(&self.billing, &target.provider_id, &target.key_id);
        }
        json_response(StatusCode::OK, &serde_json::json!({"success":true}))
    }

    /// Without a Key named, the first enabled Key allowed to call the model is used. A Key
    /// named is tested as it is, disabled or not yet allowed the model included.
    async fn probe(&self, bytes: &[u8]) -> Response {
        let Ok(probe) = serde_json::from_slice::<ProbeRequest>(bytes) else {
            return failure(StatusCode::BAD_REQUEST, "Invalid test call");
        };
        if !plain_text(&probe.model, 256) {
            return failure(StatusCode::BAD_REQUEST, "Invalid model");
        }
        let model = probe.model.trim();
        let Some(provider) = self.billing.get_provider(&probe.provider_id) else {
            return failure(StatusCode::NOT_FOUND, "Unknown provider");
        };
        let mut keys = self
            .billing
            .get_runtime_provider_keys(Some(&provider.id))
            .into_iter();
        let key = match &probe.key_id {
            Some(id) => match keys.find(|key| key.id == *id) {
                Some(key) => key,
                None => return failure(StatusCode::NOT_FOUND, "Unknown key"),
            },
            None => match keys.find(|key| key.enabled && key.supports_model(model)) {
                Some(key) => key,
                None => {
                    return failure(
                        StatusCode::CONFLICT,
                        "No enabled Key of this provider may call this model",
                    )
                }
            },
        };
        if key.api_key.is_empty() {
            return failure(StatusCode::CONFLICT, "The Key's secret is unavailable");
        }
        // Like model discovery, a test call follows no redirect.
        let Ok(client) = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
        else {
            return failure(StatusCode::INTERNAL_SERVER_ERROR, "HTTP client unavailable");
        };
        let result = crate::provider::governance::probe_provider_key(
            &client,
            &provider,
            &key,
            model,
            crate::now_secs(),
        )
        .await;
        json_response(
            StatusCode::OK,
            &serde_json::json!({
                "success": true,
                "ok": result.success,
                "status": result.status_code,
                "latency_ms": result.total_latency_ms,
                "ttft_ms": result.ttft_ms,
                "error": result.error,
                "reply": result.reply,
                "key_id": key.id,
            }),
        )
    }
}

impl FacadeHandler for ProviderMaintenanceHandler {
    fn method(&self) -> Method {
        Method::POST
    }
    fn path(&self) -> &'static str {
        match self.action {
            ProviderAction::Update => "/api/v1/admin/providers/update",
            ProviderAction::Delete => "/api/v1/admin/providers/delete",
            ProviderAction::DeleteKey => "/api/v1/admin/providers/keys/delete",
            ProviderAction::ResetKey => "/api/v1/admin/providers/keys/reset",
            ProviderAction::ProbeKey => "/api/v1/admin/providers/keys/probe",
        }
    }
    fn handle<'a>(&'a self, req: Request<Body>) -> BoxFuture<'a, Response> {
        Box::pin(async move {
            if !self.auth.verify(req.headers()) {
                return failure(StatusCode::UNAUTHORIZED, "Unauthorized");
            }
            let Ok(bytes) = axum::body::to_bytes(req.into_body(), 64 * 1024).await else {
                return failure(StatusCode::BAD_REQUEST, "Invalid body");
            };
            match self.action {
                ProviderAction::Update => self.update(&bytes),
                ProviderAction::Delete => self.delete(&bytes),
                ProviderAction::DeleteKey => self.delete_key(&bytes),
                ProviderAction::ResetKey => self.reset_key(&bytes),
                ProviderAction::ProbeKey => self.probe(&bytes).await,
            }
        })
    }
}
