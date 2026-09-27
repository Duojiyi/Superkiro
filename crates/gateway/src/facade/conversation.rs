//! Conversation streaming handler.
//!
//! Spec §4.2, §4.3, §4.6, §4.7, §6.1, §6.2, §6.3, §15.1.
//!
//! End-to-end pipeline:
//! 1. `amz-sdk-invocation-id` idempotency deduplication (Spec §4.7).
//! 2. Intent classifier interception (saves 1 round-trip LLM call).
//! 3. Credit reservation check & freezing (Spec §6.2).
//! 4. Request translation, tool name shortening & image shrinking (Spec §4.3).
//! 5. Upstream provider streaming invocation (Spec §15.2).
//! 6. Binary AWS EventStream encoding, 20s keepalive injection, and client disconnect cancellation (Spec §4.6, §6.3).

use super::models::SIMPLE_TASK_MODEL;
use super::{error_response, input_too_long, validation_error, BoxFuture, FacadeHandler, Response};
use crate::auth::AuthClaims;
use crate::guardrail::{format_kiro_throttle_response, CapacityGuardrail, LargeBodyGate};
use crate::idempotency::IdempotencyManager;
use crate::ops::{CardRateLimiter, RateLimitError};
use crate::provider::governance::{
    execute_stream_with_model_fallback, GovernanceError, ProviderKeyPool,
};
use crate::provider::retry::UpstreamLimits;
use crate::provider::ProviderRuntimeRegistry;
use crate::provider::{ModelProvider, ProviderConfig, ProviderError};
use crate::security::{ContentGuardrailConfig, GuardrailError};
use crate::stream::{create_stream_guard, BillingSettler, KiroError, StreamGuardConfig};
use crate::translate::to_provider::{
    prepare_images, translate_kiro_to_chat_request, TranslationContext,
};
use crate::watchdog::WatchdogStream;
use axum::{
    body::Body,
    http::{header, Method, Request, StatusCode},
    response::IntoResponse,
};
use billing::engine::BillingEngine;
use billing::reservation::ReservationEstimateParams;
use kiro_wire::encoder::{encode_assistant_response, encode_event};
use kiro_wire::requests::conversation::GenerateAssistantResponseRequest;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const INTENT_CLASSIFIER_SIGN_A: &str = "You are an intent classifier for a language model";
const INTENT_CLASSIFIER_SIGN_B: &str = "(chat, do, spec)";

/// How long one request may spend shrinking its images before those left over reach the
/// model as notes for this turn. The request holds a credit reservation meanwhile.
const IMAGE_PREPARE_BUDGET: Duration = Duration::from_secs(10);

/// Handler for `POST /generateAssistantResponse`
#[derive(Clone)]
pub struct GenerateAssistantResponseHandler {
    pub client: reqwest::Client,
    pub provider: Option<Arc<dyn ModelProvider>>,
    pub provider_config: Option<ProviderConfig>,
    pub pool: Option<ProviderKeyPool>,
    pub runtime: Option<ProviderRuntimeRegistry>,
    pub fallback_pools: std::collections::HashMap<String, ProviderKeyPool>,
    pub billing: BillingEngine,
    pub idempotency: IdempotencyManager,
    pub guardrail: CapacityGuardrail,
    pub card_rate_limiter: CardRateLimiter,
    pub intercept_intent: bool,
    pub vision_config: Option<crate::translate::VisionFallbackConfig>,
    pub vision_cache: crate::translate::VisionFallbackCache,
    pub content_guardrail: ContentGuardrailConfig,
    /// Shared by every clone: the places for bodies over 10 MB across the gateway.
    pub large_bodies: LargeBodyGate,
    /// Limits for every upstream attempt in place of each target model's own (tests use
    /// short ones).
    pub upstream_limits: Option<UpstreamLimits>,
}

impl Default for GenerateAssistantResponseHandler {
    fn default() -> Self {
        Self {
            client: reqwest::Client::new(),
            provider: None,
            provider_config: None,
            pool: None,
            runtime: None,
            fallback_pools: std::collections::HashMap::new(),
            billing: BillingEngine::default(),
            idempotency: IdempotencyManager::default(),
            guardrail: CapacityGuardrail::default(),
            card_rate_limiter: CardRateLimiter::new(60),
            intercept_intent: true,
            vision_config: None,
            vision_cache: crate::translate::VisionFallbackCache::default(),
            content_guardrail: ContentGuardrailConfig::default(),
            large_bodies: LargeBodyGate::default(),
            upstream_limits: None,
        }
    }
}

impl GenerateAssistantResponseHandler {
    pub fn new(
        client: reqwest::Client,
        provider: Arc<dyn ModelProvider>,
        provider_config: ProviderConfig,
        billing: BillingEngine,
        idempotency: IdempotencyManager,
    ) -> Self {
        Self {
            client,
            provider: Some(provider),
            provider_config: Some(provider_config),
            pool: None,
            runtime: None,
            fallback_pools: std::collections::HashMap::new(),
            billing,
            idempotency,
            guardrail: CapacityGuardrail::default(),
            card_rate_limiter: CardRateLimiter::new(60),
            intercept_intent: true,
            vision_config: None,
            vision_cache: crate::translate::VisionFallbackCache::default(),
            content_guardrail: ContentGuardrailConfig::default(),
            large_bodies: LargeBodyGate::default(),
            upstream_limits: None,
        }
    }

    pub fn with_guardrail(mut self, guardrail: CapacityGuardrail) -> Self {
        self.guardrail = guardrail;
        self
    }

    pub fn with_card_rate_limiter(mut self, limiter: CardRateLimiter) -> Self {
        self.card_rate_limiter = limiter;
        self
    }

    pub fn with_pool(mut self, pool: ProviderKeyPool) -> Self {
        self.pool = Some(pool);
        self
    }

    pub fn with_runtime(mut self, runtime: ProviderRuntimeRegistry) -> Self {
        self.runtime = Some(runtime);
        self
    }

    pub fn with_fallback_pool(
        mut self,
        provider_id: impl Into<String>,
        pool: ProviderKeyPool,
    ) -> Self {
        self.fallback_pools.insert(provider_id.into(), pool);
        self
    }

    pub fn with_vision_fallback(mut self, config: crate::translate::VisionFallbackConfig) -> Self {
        self.vision_config = Some(config);
        self
    }

    pub fn with_content_guardrail(mut self, config: ContentGuardrailConfig) -> Self {
        self.content_guardrail = config;
        self
    }
}

impl FacadeHandler for GenerateAssistantResponseHandler {
    fn method(&self) -> Method {
        Method::POST
    }

    fn path(&self) -> &'static str {
        "/generateAssistantResponse"
    }

    /// Every answer names its request (`x-amzn-requestid`): Kiro shows the ID with an
    /// error and keeps it with the turn's usage, so a customer's report can be traced.
    fn handle<'a>(&'a self, req: Request<Body>) -> BoxFuture<'a, Response> {
        Box::pin(async move {
            let request_id = req
                .headers()
                .get("amz-sdk-invocation-id")
                .and_then(|value| value.to_str().ok())
                .filter(|id| valid_invocation_id(id))
                .map_or_else(generated_invocation_id, str::to_string);
            let mut response = self.respond(req, &request_id).await;
            if let Ok(value) = axum::http::HeaderValue::from_str(&request_id) {
                response.headers_mut().insert("x-amzn-requestid", value);
            }
            response
        })
    }
}

impl GenerateAssistantResponseHandler {
    /// The answer to one conversation request, which `request_id` names when the client
    /// sent no invocation id of its own.
    async fn respond(&self, req: Request<Body>, request_id: &str) -> Response {
        {
            // What a response's time to first output is measured from.
            let received_at = std::time::Instant::now();
            let (parts, body) = req.into_parts();

            // 1. Extract amz-sdk-invocation-id header (Spec §4.7)
            let invocation_id = match parts.headers.get("amz-sdk-invocation-id") {
                None => request_id.to_string(),
                Some(value) => match value.to_str().ok().filter(|id| valid_invocation_id(id)) {
                    Some(id) => id.to_string(),
                    None => {
                        return error_response(
                            StatusCode::BAD_REQUEST,
                            "InvalidRequestException",
                            "amz-sdk-invocation-id must be 1 to 128 letters, digits, '.', '_', ':' or '-'",
                        )
                    }
                },
            };

            // Extract claims if present
            let claims = parts.extensions.get::<AuthClaims>().cloned();

            // Card QPS limiting is keyed by authenticated card identity. Anonymous
            // test/stub handlers deliberately bypass this limiter because they do
            // not represent a production caller.
            if let Some(ref caller) = claims {
                if let Err(RateLimitError::CardQpsExceeded { limit, .. }) =
                    self.card_rate_limiter.check_and_consume(&caller.card_id)
                {
                    return format_kiro_throttle_response(
                        StatusCode::TOO_MANY_REQUESTS,
                        "ThrottlingException",
                        "CARD_QPS_LIMIT_EXCEEDED",
                        &format!("Card request rate limit exceeded ({limit} QPS)"),
                        Some(1),
                    );
                }
            }

            // 2. Idempotency acquisition is scoped to the authenticated card.
            // Client invocation IDs are only unique per client/device, not globally.
            let invocation_key = claims
                .as_ref()
                .map(|c| format!("{}:{}", c.card_id, invocation_id))
                .unwrap_or_else(|| format!("anonymous:{}", invocation_id));
            let idempotency_guard = match self.idempotency.try_acquire(&invocation_key) {
                Ok(guard) => guard,
                Err(crate::idempotency::IdempotencyError::InProgress(_)) => {
                    return error_response(
                        StatusCode::CONFLICT,
                        "ConcurrentInvocationException",
                        "Request with amz-sdk-invocation-id is already in progress",
                    );
                }
                Err(crate::idempotency::IdempotencyError::AlreadyCompleted(_)) => {
                    // The completion cache contains billing metadata only, not the
                    // original event stream. Never turn an unreplayable request
                    // into a synthetic successful assistant answer.
                    return error_response(
                        StatusCode::CONFLICT,
                        "InvocationAlreadyCompletedException",
                        "Request with amz-sdk-invocation-id has already completed; use a new invocation id",
                    );
                }
                Err(crate::idempotency::IdempotencyError::AlreadyFailed(_)) => {
                    return error_response(
                        StatusCode::BAD_GATEWAY,
                        "PriorInvocationFailedException",
                        "This invocation previously failed after partial upstream work; use a new invocation id",
                    );
                }
            };

            // 3. Buffer request body. One over the limit is refused as too long, which Kiro
            // compacts the conversation for, whether its length is declared or found. A
            // large one first takes a place in the gateway's large-body gate.
            let body_limit = self.content_guardrail.max_body_bytes;
            let declared_length = parts
                .headers
                .get(header::CONTENT_LENGTH)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.trim().parse::<u64>().ok());
            let read = read_body(body, body_limit, declared_length, &self.large_bodies).await;
            let body_bytes = match read {
                Ok(BodyRead::Body(b)) => b,
                Ok(BodyRead::TooLarge) => {
                    self.record_refusal(claims.as_ref(), &invocation_key, "", "input_too_long");
                    return body_too_large(body_limit);
                }
                Ok(BodyRead::Throttled) => return self.large_bodies.throttled_response(),
                Err(e) => {
                    return error_response(
                        StatusCode::BAD_REQUEST,
                        "SerializationException",
                        &format!("Failed to read request body: {}", e),
                    );
                }
            };

            let real_provider = self.provider.is_some()
                || self.pool.is_some()
                || self
                    .runtime
                    .as_ref()
                    .is_some_and(|rt| rt.has_available_provider());
            if real_provider && claims.is_none() {
                return error_response(
                    StatusCode::UNAUTHORIZED,
                    "MissingAuthenticationTokenException",
                    "A valid bearer token is required for upstream model requests",
                );
            }

            if claims.is_some() && !self.billing.persistence_ready() {
                return error_response(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "ServiceUnavailableException",
                    "Billing persistence engine is not ready or in read-only recovery state",
                );
            }

            // 4. Local Intent Classifier Interception (Optimization)
            if self.intercept_intent && is_intent_classifier_call(&body_bytes) {
                // Do is the classifier's own default, and the answer when unsure. Guessing
                // spec from words in the message sent "代码规范" (coding conventions) and
                // "the OpenAPI specification" to spec editing; a spec is still one click.
                let probs = serde_json::json!({ "chat": 0, "do": 0.95, "spec": 0.05 });

                let meta_frame = encode_event(
                    "messageMetadataEvent",
                    &serde_json::json!({ "conversationId": invocation_id }),
                )
                .unwrap_or_default();

                let resp_frame =
                    encode_assistant_response(&probs.to_string(), Some("kiro-intent-classifier"));

                let mut combined = meta_frame;
                combined.extend_from_slice(&resp_frame);

                // Mark idempotency guard committed
                idempotency_guard.commit(crate::idempotency::CompletedInvocation {
                    completed_at: std::time::Instant::now(),
                    model_id: "kiro-intent-classifier".to_string(),
                    total_input_tokens: 50,
                    total_output_tokens: 20,
                });

                return (
                    StatusCode::OK,
                    [(header::CONTENT_TYPE, "application/vnd.amazon.eventstream")],
                    combined,
                )
                    .into_response();
            }

            let fallback_model = self
                .provider_config
                .as_ref()
                .map(|c| c.model.as_str())
                .unwrap_or("claude-3-5-sonnet-20241022");

            // Parse and validate before reserving credit.  A malformed or
            // over-sized request must never consume a reservation.
            let mut parsed_request = None;
            if real_provider || claims.is_some() {
                let parsed: GenerateAssistantResponseRequest =
                    match serde_json::from_slice(&body_bytes) {
                        Ok(request) => request,
                        Err(error) => {
                            return error_response(
                                StatusCode::BAD_REQUEST,
                                "SerializationException",
                                &format!(
                                    "Invalid GenerateAssistantResponseRequest payload: {error}"
                                ),
                            );
                        }
                    };
                if let Err(error) = validate_conversation_request(&parsed, &self.content_guardrail)
                {
                    if let Some(detail) = error.overflow_detail() {
                        self.record_refusal(
                            claims.as_ref(),
                            &invocation_key,
                            &requested_model_id(
                                &parsed,
                                claims.as_ref(),
                                &self.billing,
                                fallback_model,
                            ),
                            "input_too_long",
                        );
                        return input_too_long(&detail);
                    }
                    return error_response(
                        StatusCode::BAD_REQUEST,
                        "InvalidRequestException",
                        &error.to_string(),
                    );
                }
                parsed_request = Some(parsed);
            }

            // 4b. Capacity Guardrail Fast-Fail (Spec §14.9)
            let capacity_permit = match self.guardrail.try_acquire() {
                Ok(p) => p,
                Err(_) => {
                    return self.guardrail.build_retry_response();
                }
            };

            // 5. Credit Reservation (Spec §6.2)
            let mut has_reservation = false;
            let reservation_lease = self.billing.protect_reservation(&invocation_key);
            let mut hold = HoldRelease {
                billing: self.billing.clone(),
                invocation_id: invocation_key.clone(),
                armed: false,
            };
            let mut reserved_estimated_input = 2_000u64;
            let mut reserved_max_output = 4_096u32;
            if claims.is_some() || real_provider {
                let card_id = claims
                    .as_ref()
                    .map(|c| c.card_id.as_str())
                    .unwrap_or("card-dev-001");
                let now_secs = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_secs();

                let request_for_reservation = parsed_request.as_ref();
                // Held and billed as the model the request is sent to.
                let requested_model_for_reservation = request_for_reservation.map_or_else(
                    || fallback_model.to_string(),
                    |request| {
                        requested_model_id(request, claims.as_ref(), &self.billing, fallback_model)
                    },
                );
                let requested_model_for_reservation = requested_model_for_reservation.as_str();
                if !valid_model_id(requested_model_for_reservation) {
                    self.record_refusal(
                        claims.as_ref(),
                        &invocation_key,
                        requested_model_for_reservation,
                        "invalid_model",
                    );
                    return error_response(
                        StatusCode::BAD_REQUEST,
                        "InvalidRequestException",
                        "modelId is invalid",
                    );
                }
                // A retired model is refused as one its group does not list, before
                // anything is held.
                if claims.as_ref().is_some_and(|claims| {
                    self.billing
                        .list_models_for_group(&claims.group_id, false)
                        .iter()
                        .any(|m| m.retired && m.matches_model(requested_model_for_reservation))
                }) {
                    self.record_refusal(
                        claims.as_ref(),
                        &invocation_key,
                        requested_model_for_reservation,
                        "model_retired",
                    );
                    return error_response(
                        StatusCode::BAD_REQUEST,
                        "ValidationException",
                        "Requested model is not available for this card group",
                    );
                }
                let input_limit = input_limit_for_model(
                    requested_model_for_reservation,
                    claims.as_ref(),
                    &self.billing,
                );
                // The group's system prompt prefix is added after translation, but it is
                // sent, so the hold includes it.
                let prefix_tokens = claims
                    .as_ref()
                    .and_then(|claims| self.billing.get_group(&claims.group_id))
                    .and_then(|group| group.system_prompt_prefix)
                    .map_or(0, |prefix| {
                        crate::usage_estimate::tokens_from_units(
                            crate::usage_estimate::token_units(&prefix),
                        )
                    });
                let estimated_input_tokens = request_for_reservation
                    .map(|request| estimate_input_tokens(request).saturating_add(prefix_tokens))
                    .unwrap_or(2_000)
                    .max(1);
                // Sent anyway, a prompt over the model's limit is refused upstream, and Kiro
                // could not tell that refusal from an outage. Refused here, it compacts the
                // conversation and tries again.
                if estimated_input_tokens > input_limit {
                    self.record_refusal(
                        claims.as_ref(),
                        &invocation_key,
                        requested_model_for_reservation,
                        "input_too_long",
                    );
                    return input_too_long(&format!(
                        "本次请求估计约 {estimated_input_tokens} 个 token，超过该模型 {input_limit} 个 token 的输入上限"
                    ));
                }
                reserved_estimated_input = estimated_input_tokens;
                reserved_max_output = max_output_tokens_for_model(
                    requested_model_for_reservation,
                    claims.as_ref(),
                    &self.billing,
                );
                let reserve_params = ReservationEstimateParams {
                    estimated_input_tokens,
                    max_output_tokens: reserved_max_output as u64,
                    input_rate_per_m: 15_000_000,
                    output_rate_per_m: 60_000_000,
                    credit_multiplier: 1.0,
                    margin_multiplier: 1.0,
                    model: Some(requested_model_for_reservation.to_string()),
                };

                if let Err(e) =
                    self.billing
                        .reserve(card_id, &invocation_key, &reserve_params, now_secs, 660)
                {
                    match e {
                        billing::engine::BillingError::ConcurrencyLimitExceeded {
                            current,
                            max,
                        } => {
                            return crate::guardrail::format_kiro_throttle_response(
                                StatusCode::TOO_MANY_REQUESTS,
                                "ThrottlingException",
                                "CONCURRENCY_LIMIT_EXCEEDED",
                                &format!("Card concurrency quota exceeded ({}/{}). Please wait for active requests to finish.", current, max),
                                Some(2),
                            );
                        }
                        billing::engine::BillingError::DailyLimitExceeded {
                            limit,
                            current,
                            held,
                            needed,
                        } => {
                            return limit_refusal(LimitWindow::Day, limit, current, held, needed);
                        }
                        billing::engine::BillingError::MonthlyLimitExceeded {
                            limit,
                            current,
                            held,
                            needed,
                        } => {
                            return limit_refusal(
                                LimitWindow::ThirtyDays,
                                limit,
                                current,
                                held,
                                needed,
                            );
                        }
                        billing::engine::BillingError::Persistence(_) => {
                            return error_response(
                                StatusCode::SERVICE_UNAVAILABLE,
                                "ServiceUnavailableException",
                                // The underlying io::Error carries server filesystem
                                // paths and the snapshot size, and the caller cannot act
                                // on either.
                                "Billing persistence is temporarily unavailable",
                            );
                        }
                        billing::engine::BillingError::ModelNotPriced(_) => {
                            self.record_refusal(
                                claims.as_ref(),
                                &invocation_key,
                                requested_model_for_reservation,
                                "no_price",
                            );
                            return error_response(
                                StatusCode::BAD_REQUEST,
                                "ValidationException",
                                "This model has no published price; choose another model or ask your administrator to publish one",
                            );
                        }
                        other => return reservation_refusal(&other),
                    }
                }
                has_reservation = true;
                hold.armed = true;
            }

            // 6. If no real provider is configured, return fallback stub frame
            let has_upstream = self.pool.is_some()
                || self
                    .runtime
                    .as_ref()
                    .is_some_and(|rt| rt.has_available_provider())
                || (self.provider.is_some() && self.provider_config.is_some());

            if !has_upstream {
                // A configured runtime/pool/provider with no enabled candidate
                // is an outage, not the development-only stub path.
                if self.runtime.is_some() || self.pool.is_some() || self.provider.is_some() {
                    if has_reservation {
                        let _ = self.billing.release(&invocation_key);
                    }
                    let model = parsed_request.as_ref().map(|request| {
                        requested_model_id(request, claims.as_ref(), &self.billing, fallback_model)
                    });
                    self.record_refusal(
                        claims.as_ref(),
                        &invocation_key,
                        model.as_deref().unwrap_or_default(),
                        "no_route",
                    );
                    return error_response(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "ServiceUnavailableException",
                        "No enabled upstream provider is available",
                    );
                }
                if has_reservation {
                    let _ = self.billing.release(&invocation_key);
                }
                let frame_bytes = encode_assistant_response(
                    "Hello from Kiro BYOK Gateway Stub!",
                    Some("claude-sonnet-4.5"),
                );
                idempotency_guard.commit(crate::idempotency::CompletedInvocation {
                    completed_at: std::time::Instant::now(),
                    model_id: "claude-sonnet-4.5".to_string(),
                    total_input_tokens: 10,
                    total_output_tokens: 10,
                });
                return (
                    StatusCode::OK,
                    [(header::CONTENT_TYPE, "application/vnd.amazon.eventstream")],
                    frame_bytes,
                )
                    .into_response();
            }

            // 7. Deserialize Kiro conversation request
            let kiro_req: GenerateAssistantResponseRequest = match parsed_request {
                Some(request) => request,
                None => match serde_json::from_slice(&body_bytes) {
                    Ok(request) => request,
                    Err(error) => {
                        if has_reservation {
                            let _ = self.billing.release(&invocation_key);
                        }
                        return error_response(
                            StatusCode::BAD_REQUEST,
                            "SerializationException",
                            &format!("Invalid GenerateAssistantResponseRequest payload: {error}"),
                        );
                    }
                },
            };

            // 7b. Enforce Group Provider Binding Mode for static provider config (Spec §5)
            // (Dynamic runtime pools validate per-provider group binding during candidate resolution)
            if let Some(ref claims) = claims {
                if let Some(group) = self.billing.get_group(&claims.group_id) {
                    if let Some(ref cfg) = self.provider_config {
                        if self.runtime.is_none()
                            && self.pool.is_none()
                            && !group.can_access_provider(cfg.group_id.as_deref())
                        {
                            if has_reservation {
                                let _ = self.billing.release(&invocation_key);
                            }
                            self.record_refusal(
                                Some(claims),
                                &invocation_key,
                                &requested_model_id(
                                    &kiro_req,
                                    Some(claims),
                                    &self.billing,
                                    fallback_model,
                                ),
                                "no_route",
                            );
                            return no_route_for_group();
                        }
                    }
                }
            }

            let requested_model =
                requested_model_id(&kiro_req, claims.as_ref(), &self.billing, fallback_model);
            let requested_model = requested_model.as_str();
            if !valid_model_id(requested_model) {
                if has_reservation {
                    let _ = self.billing.release(&invocation_key);
                }
                self.record_refusal(
                    claims.as_ref(),
                    &invocation_key,
                    requested_model,
                    "invalid_model",
                );
                return error_response(
                    StatusCode::BAD_REQUEST,
                    "InvalidRequestException",
                    "modelId is invalid",
                );
            }
            // Kept 24 hours for tracing: the request as it arrived, its reply once it ends.
            if let (Some(archive), Some(claims)) = (crate::archive::active(), claims.as_ref()) {
                archive.keep_request(
                    &invocation_key,
                    &claims.card_id,
                    requested_model,
                    body_bytes.clone(),
                );
            }

            let mut target_model = fallback_model.to_string();
            let mut fallback_targets = Vec::new();
            let mut model_supports_vision = false;
            let mut model_supports_reasoning = false;
            let mut configured_context_window = None;
            let mut mapped_model = false;

            if let Some(ref claims) = claims {
                if let Some(group) = self.billing.get_group(&claims.group_id) {
                    let billing_models = self.billing.list_models_for_group(&group.id, false);
                    // A retired mapping is never routed; a hidden one may still be named.
                    if let Some(m) = billing_models
                        .iter()
                        .find(|bm| !bm.retired && bm.matches_model(requested_model))
                    {
                        target_model = m.target_model.clone();
                        fallback_targets = m.full_target_chain();
                        model_supports_vision = m.supports_vision;
                        model_supports_reasoning = m.supports_reasoning;
                        configured_context_window = Some(
                            super::models::TokenLimits::configured(m.context_window, m.max_output)
                                .max_input_tokens as u32,
                        );
                        mapped_model = true;
                    } else if kiro_req
                        .conversation_state
                        .current_message
                        .user_input_message
                        .model_id
                        .as_deref()
                        .is_some_and(|model| model != SIMPLE_TASK_MODEL)
                        && !billing_models.is_empty()
                    {
                        if has_reservation {
                            let _ = self.billing.release(&invocation_key);
                        }
                        self.record_refusal(
                            Some(claims),
                            &invocation_key,
                            requested_model,
                            "model_not_listed",
                        );
                        return error_response(
                            StatusCode::BAD_REQUEST,
                            "ValidationException",
                            "Requested model is not available for this card group",
                        );
                    }
                }
            }

            // An unmapped request is sent to the fallback model, so that is the only model it
            // may name: it is held and billed as the model it names.
            if !mapped_model && requested_model != fallback_model {
                if has_reservation {
                    let _ = self.billing.release(&invocation_key);
                }
                self.record_refusal(
                    claims.as_ref(),
                    &invocation_key,
                    requested_model,
                    "model_not_listed",
                );
                return error_response(
                    StatusCode::BAD_REQUEST,
                    "ValidationException",
                    "Requested model is not configured",
                );
            }
            if kiro_req.reasoning_effort().is_some() && !model_supports_reasoning {
                if has_reservation {
                    let _ = self.billing.release(&invocation_key);
                }
                self.record_refusal(
                    claims.as_ref(),
                    &invocation_key,
                    requested_model,
                    "unsupported_capability",
                );
                return error_response(
                    StatusCode::BAD_REQUEST,
                    "ValidationException",
                    "Thinking is not enabled for this model; select a configured reasoning model",
                );
            }
            // One resolved capability for the whole request. An operator declaration
            // wins whenever the model is mapped; the name heuristic is only the default
            // for unmapped targets. A single value keeps the gate, the translation
            // context and the degradation path from disagreeing with each other.
            let vision_supported = if mapped_model {
                model_supports_vision
            } else {
                crate::translate::vision::model_supports_vision(&target_model)
            };
            let degradation_available = self.vision_config.as_ref().is_some_and(|v| {
                v.enabled && v.fallback_provider_url.is_some() && v.fallback_api_key.is_some()
            });
            let historical_images = kiro_req.conversation_state.history.iter().any(|message| matches!(message,
                kiro_wire::requests::conversation::Message::User(user) if !user.user_input_message.images.is_empty()));
            let current_images = !kiro_req
                .conversation_state
                .current_message
                .user_input_message
                .images
                .is_empty();
            // Reject only when the target cannot take images and nothing can degrade
            // them. History is covered by the same escape, so a single attachment no
            // longer makes every later turn of the conversation fail.
            if !vision_supported && (historical_images || current_images) && !degradation_available
            {
                if has_reservation {
                    let _ = self.billing.release(&invocation_key);
                }
                self.record_refusal(
                    claims.as_ref(),
                    &invocation_key,
                    requested_model,
                    "unsupported_capability",
                );
                return error_response(
                    StatusCode::BAD_REQUEST,
                    "ValidationException",
                    "This model is not configured for image input. Send the prompt without an image, or ask your administrator to enable image support for it.",
                );
            }
            // An attachment no upstream path takes is refused with the reason Kiro shows
            // document refusals for, naming the file. An earlier message's becomes a note.
            if let Some((reason, message)) = crate::translate::documents::refusal(
                &kiro_req
                    .conversation_state
                    .current_message
                    .user_input_message
                    .documents,
                vision_supported,
            ) {
                if has_reservation {
                    let _ = self.billing.release(&invocation_key);
                }
                self.record_refusal(
                    claims.as_ref(),
                    &invocation_key,
                    requested_model,
                    "unsupported_capability",
                );
                return validation_error(reason, &message);
            }

            // 8. Translate to provider format (Spec §4.3, §14.3 Vision Fallback)
            let mut ctx =
                TranslationContext::new(&target_model).with_vision_support(vision_supported);

            if !ctx.supports_vision {
                if let Some(ref v_cfg) = self.vision_config {
                    let current_input = &kiro_req
                        .conversation_state
                        .current_message
                        .user_input_message;
                    let transcriptions = crate::translate::vision::transcribe_images(
                        &self.client,
                        v_cfg,
                        &self.vision_cache,
                        &current_input.images,
                        &current_input.content,
                        crate::translate::vision::TRANSCRIPTION_BUDGET,
                    )
                    .await;
                    if transcriptions.iter().any(Option::is_some) {
                        ctx = ctx.with_image_transcriptions(transcriptions);
                    }
                }
            }

            // A missing transcription degrades to the image-metadata placeholder in
            // to_provider::format_user_content. P4-2 §35 requires an unreachable vision
            // service to degrade rather than block the conversation trunk.

            if ctx.supports_vision && (historical_images || current_images) {
                let prepared = prepare_images(
                    &kiro_req,
                    self.content_guardrail.max_images_per_request,
                    IMAGE_PREPARE_BUDGET,
                )
                .await;
                ctx = ctx.with_prepared_images(prepared);
            }
            let mut chat_req = translate_kiro_to_chat_request(&kiro_req, &mut ctx);
            // Only the translation is used from here on. A large request gives its place in
            // the gate back now (or once the archive is done with its copy), not when the
            // answer ends.
            drop(kiro_req);
            drop(body_bytes);
            // The wire protocol currently has no client-controlled max_tokens
            // field.  Keep the provider request bounded by the exposed model
            // contract instead of relying on a provider's default.
            chat_req.max_tokens = Some(reserved_max_output);

            // 8b. Inject Group System Prompt Prefix if present (Spec §5)
            if let Some(ref claims) = claims {
                if let Some(group) = self.billing.get_group(&claims.group_id) {
                    if let Some(ref prefix) = group.system_prompt_prefix {
                        if !prefix.trim().is_empty() {
                            let rendered =
                                render_system_prompt_template(prefix, Some(claims), &group);
                            if let Some(first_system) =
                                chat_req.messages.iter_mut().find(|m| m.role == "system")
                            {
                                let existing = match &first_system.content {
                                    serde_json::Value::String(s) => s.clone(),
                                    other => other.to_string(),
                                };
                                first_system.content = serde_json::Value::String(format!(
                                    "{}\n\n{}",
                                    rendered.trim(),
                                    existing
                                ));
                            } else {
                                chat_req.messages.insert(
                                    0,
                                    crate::provider::ChatMessage::new(
                                        "system",
                                        serde_json::Value::String(rendered.trim().to_string()),
                                    ),
                                );
                            }
                        }
                    }
                }
            }

            // Billed when the upstream reports no input usage: what is actually sent — the
            // group prefix, translated tool schemas, transcriptions in place of images —
            // rather than the Kiro request the hold was estimated from.
            let translated_input_estimate = serde_json::to_value(&chat_req)
                .map(|value| crate::usage_estimate::estimate_json_tokens(&value))
                .unwrap_or(reserved_estimated_input)
                .clamp(
                    1,
                    input_limit_for_model(&target_model, claims.as_ref(), &self.billing),
                );

            // 9. Initiate upstream streaming (with multi-key pool failover & model fallback chain, Spec §14.3, §14.6)
            let card_group = claims
                .as_ref()
                .and_then(|c| self.billing.get_group(&c.group_id));

            let find_pool = |pid: &str| -> Option<ProviderKeyPool> {
                if let Some(runtime) = &self.runtime {
                    if let Some(pool) = runtime.pool_for(pid) {
                        return Some(pool);
                    }
                }
                if let Some(pool) = self.fallback_pools.get(pid) {
                    return Some(pool.clone());
                }
                if let Some(ref pool) = self.pool {
                    if pool.provider().id == pid {
                        return Some(pool.clone());
                    }
                }
                None
            };

            let now_secs = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();

            let mut candidates = Vec::new();
            if !fallback_targets.is_empty() {
                for ft in &fallback_targets {
                    if let Some(pool) = find_pool(&ft.provider_id) {
                        let p = pool.provider();
                        if !p.enabled {
                            continue;
                        }
                        if let Some(ref group) = card_group {
                            if !group.can_access_provider(p.group_id.as_deref()) {
                                continue;
                            }
                        }
                        candidates.push((pool, ft.target_model.clone()));
                    }
                }
                if candidates.is_empty() {
                    let _ = self.billing.release(&invocation_key);
                    self.record_refusal(
                        claims.as_ref(),
                        &invocation_key,
                        requested_model,
                        "no_route",
                    );
                    return no_route_for_group();
                }
            } else {
                let default_pool = self
                    .runtime
                    .as_ref()
                    .and_then(|rt| {
                        if let Some(ref g) = card_group {
                            rt.find_pool_for_group(g)
                        } else {
                            rt.default_pool()
                        }
                    })
                    .or_else(|| {
                        self.pool.clone().filter(|p| {
                            p.provider().enabled
                                && card_group.as_ref().is_none_or(|g| {
                                    g.can_access_provider(p.provider().group_id.as_deref())
                                })
                        })
                    });

                if let Some(pool) = default_pool {
                    candidates.push((pool, target_model.clone()));
                }
            }

            let remaining_attempts =
                3usize.saturating_sub(self.billing.invocation_attempts(&invocation_key));
            if remaining_attempts == 0 {
                let _ = self.billing.release(&invocation_key);
                return error_response(
                    StatusCode::BAD_GATEWAY,
                    "InternalServerException",
                    "Request upstream attempt budget exhausted; submit a new request to retry",
                );
            }
            // The route primes an attempt through the model's first content, retrying and
            // failing over within the attempt budget. Kiro gives up on a request that sends it
            // nothing for 60 seconds, so a route still going at `commit` (after the request
            // arrived) begins the answer: headers, then keepalives while the route finishes
            // behind them, and a failure ends it with the exception frame Kiro reads as it
            // would the error response. A route that ends sooner is answered as before.
            let limits = self
                .upstream_limits
                .unwrap_or_else(|| crate::provider::retry::UpstreamLimits::for_request(&chat_req));
            let route = match self.upstream_limits {
                Some(limits) => crate::provider::retry::Route::with_limits(limits),
                None => crate::provider::retry::Route::new(limits.total),
            };
            let commit_at = tokio::time::Instant::from_std(received_at) + limits.commit;
            let (upstream_stream, actual_provider_id, actual_target_model, committed) =
                if !candidates.is_empty() {
                    let routing = {
                        let candidates = candidates.clone();
                        let client = self.client.clone();
                        let chat_req = chat_req.clone();
                        let billing = self.billing.clone();
                        let invocation_key = invocation_key.clone();
                        let requested_model = requested_model.to_string();
                        let card_id = claims
                            .as_ref()
                            .map(|c| c.card_id.clone())
                            .unwrap_or_default();
                        async move {
                            let ((route_result, attempts), empty_attempt) =
                                with_empty_attempt(crate::provider::retry::ATTEMPTS.scope(
                                    std::sync::Mutex::new(Vec::new()),
                                    async {
                                        let result = execute_stream_with_model_fallback(
                                            &candidates,
                                            &client,
                                            &chat_req,
                                            Duration::from_secs(60),
                                            remaining_attempts,
                                            now_secs,
                                        )
                                        .await;
                                        let attempts = crate::provider::retry::ATTEMPTS
                                            .with(|records| records.lock().unwrap().clone());
                                        (result, attempts)
                                    },
                                ))
                                .await;
                            billing.record_trace(billing::observability::RequestTrace {
                                id: format!(
                                    "attempt-{}-{}",
                                    invocation_key,
                                    SystemTime::now()
                                        .duration_since(UNIX_EPOCH)
                                        .unwrap_or_default()
                                        .as_nanos()
                                ),
                                card_id,
                                ts: crate::now_secs(),
                                invocation_id: invocation_key,
                                exposed_model: requested_model,
                                status: if route_result.is_ok() {
                                    billing::observability::TraceStatus::InProgress
                                } else {
                                    billing::observability::TraceStatus::Error
                                },
                                ttft_ms: None,
                                tokens_per_second: None,
                                error_class: route_result
                                    .as_ref()
                                    .err()
                                    .map(|error| route_failure_class(error, &attempts).into()),
                                provider_id: attempts.last().map(|a| a.provider_id.clone()),
                                input_tokens: 0,
                                output_tokens: 0,
                                credits_charged: 0,
                                provider_cost_micro_cny: 0,
                                attempt_chain: attempts,
                            });
                            (route_result, empty_attempt)
                        }
                    };
                    let mut routing =
                        Box::pin(crate::provider::retry::ROUTE.scope(route.clone(), routing));
                    match until_committed(&mut routing, commit_at).await {
                        Some((route_result, empty_attempt)) => match route_result {
                            Ok(res) => (res.stream, res.provider.id, res.target_model, false),
                            // Only when every attempt ended empty: their input was consumed.
                            Err(_)
                                if self.bill_empty_attempt(
                                    &invocation_key,
                                    requested_model,
                                    translated_input_estimate,
                                    empty_attempt,
                                ) =>
                            {
                                idempotency_guard.fail();
                                return empty_attempts_response();
                            }
                            Err(e) => {
                                let _ = self.billing.release(&invocation_key);
                                return route_error(&e).into_response();
                            }
                        },
                        None => {
                            // Named until the route says which attempt serves the answer.
                            let (pool, target_model) = &candidates[0];
                            let answer = deferred_answer(async move {
                                let (route_result, empty_attempt) = routing.await;
                                (
                                    route_result
                                        .map(|res| (res.stream, res.provider.id, res.target_model))
                                        .map_err(|error| route_error(&error)),
                                    empty_attempt,
                                )
                            });
                            (answer, pool.provider().id, target_model.clone(), true)
                        }
                    }
                } else if let (Some(ref provider), Some(ref provider_config)) =
                    (&self.provider, &self.provider_config)
                {
                    if let Some(ref g) = card_group {
                        if !g.can_access_provider(provider_config.group_id.as_deref()) {
                            let _ = self.billing.release(&invocation_key);
                            self.record_refusal(
                                claims.as_ref(),
                                &invocation_key,
                                requested_model,
                                "no_route",
                            );
                            return no_route_for_group();
                        }
                    }
                    let mut direct_config = provider_config.clone();
                    direct_config.model = target_model.clone();
                    let routing = {
                        let provider = provider.clone();
                        let client = self.client.clone();
                        let chat_req = chat_req.clone();
                        async move {
                            with_empty_attempt(crate::provider::retry::start_stream(
                                provider.as_ref(),
                                &client,
                                &direct_config,
                                &chat_req,
                                3,
                            ))
                            .await
                        }
                    };
                    let mut routing =
                        Box::pin(crate::provider::retry::ROUTE.scope(route.clone(), routing));
                    let provider_id = provider.name().to_string();
                    match until_committed(&mut routing, commit_at).await {
                        Some((started, empty_attempt)) => match started {
                            Ok(s) => (s, provider_id, target_model.clone(), false),
                            Err(_)
                                if self.bill_empty_attempt(
                                    &invocation_key,
                                    requested_model,
                                    translated_input_estimate,
                                    empty_attempt,
                                ) =>
                            {
                                idempotency_guard.fail();
                                return empty_attempts_response();
                            }
                            Err(e) => {
                                // Upstream initiation failed; release reservation in full
                                let _ = self.billing.release(&invocation_key);
                                if provider_input_too_long(&e) {
                                    self.record_refusal(
                                        claims.as_ref(),
                                        &invocation_key,
                                        requested_model,
                                        "input_too_long",
                                    );
                                }
                                return start_error(&e).into_response();
                            }
                        },
                        None => {
                            let answer = {
                                let (provider_id, target_model) =
                                    (provider_id.clone(), target_model.clone());
                                deferred_answer(async move {
                                    let (started, empty_attempt) = routing.await;
                                    (
                                        started
                                            .map(|stream| (stream, provider_id, target_model))
                                            .map_err(|error| start_error(&error)),
                                        empty_attempt,
                                    )
                                })
                            };
                            (answer, provider_id, target_model.clone(), true)
                        }
                    }
                } else {
                    let _ = self.billing.release(&invocation_key);
                    self.record_refusal(
                        claims.as_ref(),
                        &invocation_key,
                        requested_model,
                        "no_route",
                    );
                    return no_route_for_group();
                };

            // 10. Wrap in Stream Guard (keepalive + cancellation + tool name restoration + billing settlement + contextUsage)
            let context_window = configured_context_window.unwrap_or_else(|| {
                billing::context::ModelContextLibrary::resolve(&actual_target_model).context_window
            });
            let guard_config = StreamGuardConfig {
                keepalive_interval: Duration::from_secs(20),
                // The model the customer asked for. The target, a fallback included, is
                // internal routing and stays in the server-side traces.
                model_id: requested_model.to_string(),
                context_window: Some(context_window),
            };

            // Pings and empty deltas count as liveness; a model that reasons first may go
            // silent longer.
            let watchdog = self
                .upstream_limits
                .unwrap_or_else(|| {
                    crate::provider::retry::UpstreamLimits::for_model(
                        &actual_target_model,
                        chat_req.reasoning_effort,
                    )
                })
                .watchdog();

            // The stream's settler bills or returns the hold from here on.
            hold.armed = false;
            let settler = BillingSettler::new(
                self.billing.clone(),
                invocation_key.clone(),
                requested_model.to_string(),
                actual_provider_id,
                actual_target_model,
            )
            .with_metrics(
                parts
                    .extensions
                    .get::<crate::ops::metrics::RequestMetrics>()
                    .cloned(),
            )
            .with_permit(capacity_permit)
            .with_started_at(received_at)
            .with_estimated_input(translated_input_estimate)
            .with_reservation_lease(reservation_lease);

            // A route still finishing is watched by its attempts' own watchdogs: the wait for
            // its first content is not a silence of the answer.
            let upstream_stream: UpstreamStream = if committed {
                upstream_stream
            } else {
                Box::pin(WatchdogStream::new(upstream_stream, watchdog))
            };
            let guarded_stream = create_stream_guard(
                upstream_stream,
                guard_config,
                Some(ctx.tool_registry),
                Some(idempotency_guard),
                Some(settler),
            );

            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "application/vnd.amazon.eventstream")],
                Body::from_stream(guarded_stream),
            )
                .into_response()
        }
    }

    /// Records an authenticated request refused before it was routed: nothing was sent
    /// upstream and nothing is charged. The model it named is kept only when it is a valid
    /// model ID; the request's content is never kept.
    fn record_refusal(
        &self,
        claims: Option<&AuthClaims>,
        invocation_key: &str,
        model: &str,
        error_class: &str,
    ) {
        let Some(claims) = claims else {
            return;
        };
        self.billing
            .record_trace(billing::observability::RequestTrace {
                id: format!(
                    "refused-{}-{}",
                    invocation_key,
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_nanos()
                ),
                card_id: claims.card_id.clone(),
                ts: crate::now_secs(),
                invocation_id: invocation_key.to_string(),
                exposed_model: if valid_model_id(model) {
                    model.trim().to_string()
                } else {
                    String::new()
                },
                status: billing::observability::TraceStatus::Error,
                ttft_ms: None,
                tokens_per_second: None,
                error_class: Some(error_class.to_string()),
                provider_id: None,
                input_tokens: 0,
                output_tokens: 0,
                credits_charged: 0,
                provider_cost_micro_cny: 0,
                attempt_chain: Vec::new(),
            });
    }

    /// Bill what the last empty attempt reported, when no attempt of the request produced
    /// anything: each consumed the input it reported, and the request pays for it once,
    /// however many times it was retried. An attempt that answers is billed by its stream
    /// instead, and an empty attempt before it is not billed as well. Whether it billed;
    /// a failed save still keeps the charge for the recovery task.
    fn bill_empty_attempt(
        &self,
        invocation_key: &str,
        exposed_model: &str,
        estimated_input: u64,
        empty: Option<crate::provider::retry::EmptyAttempt>,
    ) -> bool {
        let Some(empty) = empty else {
            return false;
        };
        let Some(tokens) = crate::stream::resolve_settlement_tokens(
            &empty.usage,
            crate::stream::reports_input(&empty.usage),
            0,
            false,
            estimated_input,
        ) else {
            return false;
        };
        let settled = self.billing.settle(
            invocation_key,
            &tokens,
            exposed_model,
            &empty.provider_id,
            &empty.target_model,
            crate::now_secs(),
        );
        if let Err(error) = &settled {
            eprintln!("[kiro-gateway] settlement failed: {error}");
        }
        self.billing.finish_trace(
            invocation_key,
            billing::observability::TraceStatus::Error,
            Some(if settled.is_ok() {
                "empty_completion"
            } else {
                "settlement_failed"
            }),
        );
        true
    }
}

type UpstreamStream = futures_util::stream::BoxStream<
    'static,
    Result<crate::provider::ProviderStreamEvent, ProviderError>,
>;

/// `routing` to its end, or `None` at `commit_at` if it is still going, with `routing` left
/// to finish.
async fn until_committed<F>(routing: &mut F, commit_at: tokio::time::Instant) -> Option<F::Output>
where
    F: std::future::Future + Unpin,
{
    tokio::select! {
        biased;
        outcome = routing => Some(outcome),
        _ = tokio::time::sleep_until(commit_at) => None,
    }
}

/// The answer of a request whose route was still going when the answer had to begin. Kiro
/// hears keepalives while `routing` finishes; then comes the answer, after the attempt that
/// serves it, or how the route failed, as the error response would have told Kiro. A route
/// whose every attempt ended empty ends as that empty turn: its input billed once, as when
/// the answer had not begun.
fn deferred_answer(
    routing: impl std::future::Future<
            Output = (
                Result<(UpstreamStream, String, String), KiroError>,
                Option<crate::provider::retry::EmptyAttempt>,
            ),
        > + Send
        + 'static,
) -> UpstreamStream {
    use crate::provider::ProviderStreamEvent;
    use futures_util::StreamExt;
    futures_util::stream::once(routing)
        .flat_map(|(result, empty_attempt)| match (result, empty_attempt) {
            (Ok((answer, provider_id, target_model)), _) => {
                futures_util::stream::iter([Ok(ProviderStreamEvent::Served {
                    provider_id,
                    target_model,
                })])
                .chain(answer)
                .boxed()
            }
            (Err(_), Some(empty)) => {
                let mut usage = empty.usage;
                usage.output_tokens_final = true;
                futures_util::stream::iter([
                    Ok(ProviderStreamEvent::Served {
                        provider_id: empty.provider_id,
                        target_model: empty.target_model,
                    }),
                    Ok(ProviderStreamEvent::Usage(usage)),
                    Ok(ProviderStreamEvent::StopReason("end_turn".into())),
                    Ok(ProviderStreamEvent::Done),
                ])
                .boxed()
            }
            (Err(error), None) => {
                futures_util::stream::iter([Ok(ProviderStreamEvent::Failed(error))]).boxed()
            }
        })
        .boxed()
}

/// How Kiro is told a route failed: the overflow it compacts for, a throttle it waits out,
/// the upstream's refusal of the request, or a temporary error it retries.
fn route_error(error: &GovernanceError) -> KiroError {
    if upstream_input_too_long(error) {
        return overflow_error();
    }
    if let GovernanceError::AllKeysInCooldown {
        next_recovery_secs, ..
    } = error
    {
        return KiroError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "ThrottlingException",
            "All upstream provider keys are currently in cooldown",
        )
        .with_reason("ALL_KEYS_IN_COOLDOWN")
        .with_retry_after((*next_recovery_secs).max(1));
    }
    governance_upstream_refusal(error).unwrap_or_else(|| {
        KiroError::new(
            StatusCode::BAD_GATEWAY,
            "InternalServerException",
            crate::stream::safe_governance_error(error),
        )
    })
}

/// How Kiro is told the configured upstream failed to answer.
fn start_error(error: &ProviderError) -> KiroError {
    if provider_input_too_long(error) {
        return overflow_error();
    }
    upstream_refusal(error).unwrap_or_else(|| {
        KiroError::new(
            StatusCode::BAD_GATEWAY,
            "InternalServerException",
            crate::stream::safe_provider_error(error),
        )
    })
}

/// The upstream's report of a prompt over its model's context, as the overflow Kiro
/// compacts the conversation for.
fn overflow_error() -> KiroError {
    KiroError::new(
        StatusCode::BAD_REQUEST,
        "ValidationException",
        format!("Input is too long: {UPSTREAM_OVERFLOW}"),
    )
    .with_reason("CONTENT_LENGTH_EXCEEDS_THRESHOLD")
}

/// Run `attempts` with an [`crate::provider::retry::EMPTY_ATTEMPT`] slot, and take what it
/// holds at the end.
async fn with_empty_attempt<T>(
    attempts: impl std::future::Future<Output = T>,
) -> (T, Option<crate::provider::retry::EmptyAttempt>) {
    crate::provider::retry::EMPTY_ATTEMPT
        .scope(std::sync::Mutex::new(Default::default()), async {
            let result = attempts.await;
            let empty =
                crate::provider::retry::EMPTY_ATTEMPT.with(|slot| slot.lock().unwrap().take());
            (result, empty)
        })
        .await
}

/// How a request whose upstream could not be started is traced. When no Key may call its
/// target, nothing was sent: it is refused for want of a route.
fn route_failure_class(
    error: &GovernanceError,
    attempts: &[billing::observability::AttemptRecord],
) -> &'static str {
    match error {
        GovernanceError::NoAvailableKeys { .. } if attempts.is_empty() => "no_route",
        error if upstream_input_too_long(error) => "input_too_long",
        error if governance_upstream_refusal(error).is_some() => "upstream_refused",
        _ => "upstream_start_failed",
    }
}

/// Micro-credits as the credits a customer sees.
fn credits(micro: i64) -> String {
    let credits = micro as f64 / billing::MICRO_CREDITS_PER_CREDIT as f64;
    format!("{:.2}", credits)
}

/// A hold the card cannot take, in words Kiro shows as they are: a ValidationException
/// message, which it neither rewrites nor retries. An unknown exception type read
/// "Something went wrong".
fn reservation_refusal(error: &billing::engine::BillingError) -> Response {
    use billing::card::CardError;
    use billing::engine::BillingError;
    let message = match error {
        BillingError::Card(CardError::InsufficientCredit { available, needed }) => format!(
            "积分余额不足：本次请求需预留 {}（按该模型的最大输出估算），当前可用 {}。请充值后重试，或换用更便宜的模型。",
            credits(*needed),
            credits((*available).max(0))
        ),
        BillingError::Card(CardError::Expired) => {
            "卡密已过期，请续期或更换卡密后重试。".to_string()
        }
        BillingError::Card(CardError::NotActive(status)) => {
            format!("卡密当前不可用（状态：{status:?}），请联系管理员。")
        }
        BillingError::CardNotFound(_) => "未找到该卡密，请重新登录后重试。".to_string(),
        // A settlement still being saved, or state the next request finds consistent: a
        // retry succeeds.
        _ => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "InternalServerException",
                "Credit reservation is temporarily unavailable",
            )
        }
    };
    error_response(StatusCode::BAD_REQUEST, "ValidationException", &message)
}

/// The card's fair-use window that refused a hold.
#[derive(Clone, Copy)]
enum LimitWindow {
    /// The UTC calendar day.
    Day,
    /// The 30 days before the request, rolling.
    ThirtyDays,
}

/// A hold the card's daily or 30-day credit limit refuses. The open holds of other
/// requests settle within minutes, mostly for far less than they hold, so a refusal they
/// alone cause is a throttle Kiro retries. Settled usage at the limit, or a hold larger than
/// what the window has left, stays refused until the window frees up: a ValidationException
/// without a reason, which Kiro shows as written, with the limit, the usage and when it
/// frees up. Kiro's own limit reasons show its fixed "return tomorrow / next month" instead,
/// and the window is neither.
fn limit_refusal(
    window: LimitWindow,
    limit: i64,
    current: i64,
    held: i64,
    needed: i64,
) -> Response {
    let settled = current.saturating_sub(held).max(0);
    let left = limit.saturating_sub(settled);
    if needed <= left {
        return format_kiro_throttle_response(
            StatusCode::TOO_MANY_REQUESTS,
            "ThrottlingException",
            "CREDIT_HOLDS_PENDING",
            "卡密的用量额度正被其他进行中的请求暂时占用，请稍后重试。",
            Some(1),
        );
    }
    let (name, frees) = match window {
        LimitWindow::Day => ("今日", "额度按 UTC 自然日计算，每天北京时间 8:00 重置。"),
        LimitWindow::ThirtyDays => ("近 30 天", "额度按滚动 30 天计算，每笔用量满 30 天后释放。"),
    };
    let message = if left <= 0 {
        format!(
            "{name}积分用量已达上限：上限 {}，已用 {}。{frees}",
            credits(limit),
            credits(settled)
        )
    } else {
        format!(
            "{name}剩余积分额度 {} 不足以预留本次请求所需的 {}（按该模型的最大输出估算）：上限 {}，已用 {}。{frees}也可以换用更便宜的模型。",
            credits(left),
            credits(needed),
            credits(limit),
            credits(settled)
        )
    };
    error_response(StatusCode::BAD_REQUEST, "ValidationException", &message)
}

/// A card whose group may not use any provider that serves the model. Not an
/// AccessDeniedException: Kiro takes that for an expired login.
fn no_route_for_group() -> Response {
    error_response(
        StatusCode::BAD_REQUEST,
        "ValidationException",
        "当前卡密所在分组没有可用于该模型的上游服务，请换一个模型或联系管理员。",
    )
}

/// An upstream's refusal that a retry would meet again, in words Kiro shows as they are;
/// never the upstream's own text. Its rate limits, an invalid key and a request timeout are
/// the gateway's to handle. A key out of balance or not allowed the model (402, 403) and a
/// model the upstream does not know (404) are the operator's to fix, and are told so: the
/// request is not at fault.
fn upstream_refusal(error: &ProviderError) -> Option<KiroError> {
    let ProviderError::Http(status, _) = error else {
        return None;
    };
    let status = status.as_u16();
    if !(400..500).contains(&status) || matches!(status, 401 | 408 | 429) {
        return None;
    }
    let message = match status {
        402 | 403 => format!(
            "上游模型服务的账户余额或权限出了问题（HTTP {status}），不是本次请求的问题，需要管理员处理。可以稍后重试，或先换一个模型。"
        ),
        404 => "上游模型服务找不到这个模型（HTTP 404），是模型配置的问题，不是本次请求的问题，需要管理员处理。可以先换一个模型。".to_string(),
        _ => {
            let why = match status {
                400 | 422 => "请求内容或参数不被该上游接受",
                _ => "该上游拒绝了本次请求",
            };
            format!(
                "上游模型服务拒绝了本次请求（HTTP {status}：{why}），重试不会改变结果。可以调整请求、换一个模型，或联系管理员。"
            )
        }
    };
    Some(KiroError::new(
        StatusCode::BAD_REQUEST,
        "ValidationException",
        message,
    ))
}

fn governance_upstream_refusal(error: &GovernanceError) -> Option<KiroError> {
    match error {
        GovernanceError::NonRetryable(error) | GovernanceError::Provider(error) => {
            upstream_refusal(error)
        }
        _ => None,
    }
}

/// What a request refused upstream for its length is told.
const UPSTREAM_OVERFLOW: &str = "上游模型报告输入超过了它的上下文上限";

/// Whether the upstream refused the request for a prompt longer than its model takes.
fn upstream_input_too_long(error: &GovernanceError) -> bool {
    match error {
        GovernanceError::NonRetryable(error) | GovernanceError::Provider(error) => {
            provider_input_too_long(error)
        }
        _ => false,
    }
}

/// Anthropic's "prompt is too long", OpenAI's `context_length_exceeded` and "maximum
/// context length", and a request too large to be accepted at all. Only the refusal's kind
/// is read from the upstream's words; they never reach the client.
fn provider_input_too_long(error: &ProviderError) -> bool {
    let ProviderError::Http(status, body) = error else {
        return false;
    };
    status.as_u16() == 413 || status.as_u16() == 400 && crate::provider::says_input_too_long(body)
}

/// A conversation larger than the gateway reads. Kiro sends every image in a conversation
/// again with each turn, so that is mostly screenshots, which compacting it leaves behind.
fn body_too_large(limit: usize) -> Response {
    input_too_long(&format!(
        "请求体超过网关 {} MB 的上限，多为对话中累积的图片",
        limit / (1024 * 1024)
    ))
}

/// What reading a conversation's body came to.
enum BodyRead {
    Body(bytes::Bytes),
    /// Longer than the gateway reads.
    TooLarge,
    /// Large, and no place in the large-body gate freed up in time.
    Throttled,
}

/// A large body with its place in the gate, which it gives back when its last copy (the
/// archive's included) is dropped.
struct HeldBody {
    body: bytes::Bytes,
    _place: tokio::sync::OwnedSemaphorePermit,
}

impl AsRef<[u8]> for HeldBody {
    fn as_ref(&self) -> &[u8] {
        &self.body
    }
}

/// A request body of at most `limit` bytes. One over the gate's threshold, by the length it
/// declares or by the bytes that arrive, first takes a place in the gate, and holds it for
/// as long as the body is kept. Room for a declared length is taken at once, so a large body
/// is not copied while it arrives.
async fn read_body(
    body: Body,
    limit: usize,
    declared_length: Option<u64>,
    gate: &LargeBodyGate,
) -> Result<BodyRead, axum::Error> {
    use futures_util::StreamExt;
    if declared_length.is_some_and(|length| length > limit as u64) {
        return Ok(BodyRead::TooLarge);
    }
    let declared = declared_length.map_or(0, |length| length as usize);
    let mut place = None;
    if declared > gate.threshold() {
        let Some(entered) = gate.enter().await else {
            return Ok(BodyRead::Throttled);
        };
        place = Some(entered);
    }
    let mut data = bytes::BytesMut::with_capacity(declared);
    let mut chunks = body.into_data_stream();
    while let Some(chunk) = chunks.next().await {
        let chunk = chunk?;
        if chunk.len() > limit - data.len() {
            return Ok(BodyRead::TooLarge);
        }
        if place.is_none() && data.len() + chunk.len() > gate.threshold() {
            let Some(entered) = gate.enter().await else {
                return Ok(BodyRead::Throttled);
            };
            place = Some(entered);
        }
        data.extend_from_slice(&chunk);
    }
    let body = data.freeze();
    Ok(BodyRead::Body(match place {
        Some(place) => bytes::Bytes::from_owner(HeldBody {
            body,
            _place: place,
        }),
        None => body,
    }))
}

/// A request whose every attempt came back empty. It was billed the input consumed, so it
/// is not retried under the same invocation id.
fn empty_attempts_response() -> Response {
    error_response(
        StatusCode::BAD_GATEWAY,
        "InternalServerException",
        "The upstream model returned an empty response to every attempt; the input it read has been billed",
    )
}

/// Returns a request's hold when the request ends before its stream's settler takes over:
/// refused on the way, or dropped at an await because the client went away (Kiro's stop,
/// a disconnect, the request timeout) while the upstream was being started. That left the
/// hold for the janitor, eleven minutes later: two such stops locked a two-request card
/// out, and the retry of either was refused as a duplicate.
struct HoldRelease {
    billing: BillingEngine,
    invocation_id: String,
    armed: bool,
}

impl Drop for HoldRelease {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.billing.release(&self.invocation_id);
        }
    }
}

/// Interpolate dynamic template placeholders in group system prompt prefix (Spec §5, P4-7).
pub fn render_system_prompt_template(
    template: &str,
    claims: Option<&crate::auth::AuthClaims>,
    group: &billing::group::Group,
) -> String {
    let card_id = claims.map(|c| c.card_id.as_str()).unwrap_or("anonymous");
    let group_id = claims
        .map(|c| c.group_id.as_str())
        .unwrap_or(group.id.as_str());

    template
        .replace("{{card_id}}", card_id)
        .replace("{{user_id}}", card_id)
        .replace("{{group_id}}", group_id)
        .replace("{{group_name}}", &group.name)
        .replace("{{plan_name}}", &group.virtual_plan_name)
        .replace("{{virtual_plan_name}}", &group.virtual_plan_name)
}

/// Whether `body` is Kiro's intent-classifier call: the classifier instructions lead the
/// request, as its system prompt or its first message, and no tools are offered. The same
/// words anywhere else (a pasted log, a file a tool read, a later message) are the user's
/// own content, and that turn goes to the model.
fn is_intent_classifier_call(body: &[u8]) -> bool {
    let text = String::from_utf8_lossy(body);
    if !text.contains(INTENT_CLASSIFIER_SIGN_A) || !text.contains(INTENT_CLASSIFIER_SIGN_B) {
        return false;
    }
    let Ok(request) = serde_json::from_slice::<GenerateAssistantResponseRequest>(body) else {
        return false;
    };
    let state = &request.conversation_state;
    let current = &state.current_message.user_input_message;
    if current
        .user_input_message_context
        .as_ref()
        .is_some_and(|context| !context.tools.is_empty())
    {
        return false;
    }
    let instructions = request
        .system_prompt
        .as_deref()
        .filter(|prompt| !prompt.trim().is_empty())
        .or_else(|| match state.history.first()? {
            kiro_wire::requests::conversation::Message::User(user) => {
                Some(user.user_input_message.content.as_str())
            }
            kiro_wire::requests::conversation::Message::Assistant(_) => None,
        })
        .map(str::trim_start);
    instructions.is_some_and(|instructions| {
        instructions.starts_with(INTENT_CLASSIFIER_SIGN_A)
            && instructions.contains(INTENT_CLASSIFIER_SIGN_B)
    })
}

fn generated_invocation_id() -> String {
    format!(
        "inv-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    )
}

/// The client's invocation id keys idempotency, the credit hold and the request traces,
/// and is copied into the saved billing state. SDKs send a UUID. Anything else is
/// refused before it is used: an unbounded id would be stored in every snapshot.
fn valid_invocation_id(id: &str) -> bool {
    (1..=128).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':'))
}

/// The model a request is for: the one it names, or, when it names none, its group's
/// default — the first model its model list shows — or, in a group that maps no models,
/// the fallback model every unmapped request is sent to. A request that named no model
/// was priced as "default-model" and sent to the fallback model, so it was billed at the
/// built-in default rates whatever its group's models cost.
///
/// Kiro's fast model is the model given its name, as an alias as a rule, listed or hidden,
/// or else the group's default; it is held, routed and billed as that model. Refused for want of a
/// price, it failed Kiro's commit messages and its spec sub-intents.
fn requested_model_id(
    request: &GenerateAssistantResponseRequest,
    claims: Option<&AuthClaims>,
    billing: &BillingEngine,
    fallback_model: &str,
) -> String {
    let named = request
        .conversation_state
        .current_message
        .user_input_message
        .model_id
        .as_deref();
    if let Some(model) = named.filter(|model| *model != SIMPLE_TASK_MODEL) {
        return model.to_string();
    }
    let group = claims.and_then(|claims| billing.get_group(&claims.group_id));
    let fast_model = named.and(group.as_ref()).and_then(|group| {
        billing
            .list_models_for_group(&group.id, false)
            .into_iter()
            .find(|model| !model.retired && model.matches_model(SIMPLE_TASK_MODEL))
    });
    fast_model
        .or_else(|| {
            group.and_then(|group| {
                billing
                    .list_models_for_group(&group.id, true)
                    .into_iter()
                    .next()
            })
        })
        .map_or_else(
            || fallback_model.to_string(),
            |model| model.exposed_model_id,
        )
}

/// The rule a published model ID and alias meet, around surrounding whitespace.
fn valid_model_id(model: &str) -> bool {
    billing::group::valid_model_id(model.trim())
}

fn estimate_input_tokens(request: &GenerateAssistantResponseRequest) -> u64 {
    // The whole request counts — tool schemas, tool results, history, editor state —
    // except image payloads, which count at what a provider charges for an image.
    // Provider usage still settles the final charge whenever it is reported.
    serde_json::to_value(request)
        .map(|value| crate::usage_estimate::estimate_json_tokens(&value))
        .unwrap_or(u64::MAX)
}

/// The most input tokens `model` accepts: its group's configured limit when the model
/// is mapped, otherwise its known context window. Estimates are clamped to it; a fixed
/// clamp would under-hold a model with a larger window.
fn input_limit_for_model(model: &str, claims: Option<&AuthClaims>, billing: &BillingEngine) -> u64 {
    claims
        .and_then(|claims| billing.get_group(&claims.group_id))
        .and_then(|group| {
            billing
                .list_models_for_group(&group.id, false)
                .into_iter()
                .find(|mapped| mapped.matches_model(model))
        })
        .map(|mapped| {
            super::models::TokenLimits::configured(mapped.context_window, mapped.max_output)
                .max_input_tokens
        })
        .unwrap_or_else(|| {
            u64::from(billing::context::ModelContextLibrary::resolve(model).context_window)
        })
        .max(1)
}

fn validate_conversation_request(
    request: &GenerateAssistantResponseRequest,
    guardrail: &ContentGuardrailConfig,
) -> Result<(), GuardrailError> {
    if request.conversation_state.conversation_id.trim().is_empty()
        || request.conversation_state.conversation_id.chars().count() > 256
    {
        return Err(GuardrailError::InvalidConversationId);
    }
    if request.conversation_state.history.len() > 1_000 {
        return Err(GuardrailError::HistoryTooLong {
            actual: request.conversation_state.history.len(),
            max: 1_000,
        });
    }

    let current = &request
        .conversation_state
        .current_message
        .user_input_message;
    // Validate the complete serialized conversation, including tool schemas,
    // tool results, tool calls and metadata, rather than only visible text. Image
    // payloads are left out: Kiro resends every earlier image on every turn, and an
    // image is bounded by the image limits and reaches the model as an image or a
    // note, never as text.
    let image_chars: usize = request
        .conversation_state
        .history
        .iter()
        .filter_map(|message| match message {
            kiro_wire::requests::conversation::Message::User(user) => {
                Some(&user.user_input_message.images)
            }
            kiro_wire::requests::conversation::Message::Assistant(_) => None,
        })
        .chain([&current.images])
        .flatten()
        .map(|image| image.source.bytes.chars().count())
        .sum();
    // Attachments count as what is sent: a text file as its text, never its base64.
    let documents = request
        .conversation_state
        .history
        .iter()
        .filter_map(|message| match message {
            kiro_wire::requests::conversation::Message::User(user) => {
                Some(&user.user_input_message.documents)
            }
            kiro_wire::requests::conversation::Message::Assistant(_) => None,
        })
        .chain([&current.documents])
        .flatten();
    let (document_chars, document_text_chars) =
        documents.fold((0usize, 0usize), |(encoded, text), document| {
            let sent = match crate::translate::documents::document_kind(&document.format) {
                crate::translate::documents::DocumentKind::Text => {
                    crate::translate::documents::text_of(document)
                        .map_or(0, |text| text.chars().count())
                }
                _ => 0,
            };
            (encoded + document.source.bytes.len(), text + sent)
        });
    let prompt_chars = serde_json::to_string(request)
        .map(|json| {
            json.chars()
                .count()
                .saturating_sub(image_chars)
                .saturating_sub(document_chars)
                .saturating_add(document_text_chars)
        })
        .unwrap_or(usize::MAX);
    let mut image_sizes = Vec::with_capacity(current.images.len());
    for image in &current.images {
        let decoded = base64::Engine::decode(
            &base64::engine::general_purpose::STANDARD,
            image.source.bytes.trim(),
        )
        .map_err(|_| GuardrailError::InvalidImage)?;
        crate::translate::images::validate_image_bytes(&decoded)
            .map_err(|_| GuardrailError::InvalidImage)?;
        image_sizes.push(decoded.len());
    }
    guardrail.validate_payload(prompt_chars, &image_sizes)
}

fn max_output_tokens_for_model(
    target_model: &str,
    claims: Option<&AuthClaims>,
    billing: &BillingEngine,
) -> u32 {
    let configured = claims
        .and_then(|claims| billing.get_group(&claims.group_id))
        .and_then(|group| {
            billing
                .list_models_for_group(&group.id, false)
                .into_iter()
                .find(|model| model.matches_model(target_model))
                .map(|model| {
                    super::models::TokenLimits::configured(model.context_window, model.max_output)
                        .max_output_tokens
                })
        })
        .unwrap_or(4_096);
    configured as u32
}

#[cfg(test)]
mod capability_tests {
    use super::*;
    use billing::group::{Group, ModelMap};

    #[test]
    fn reservation_matches_routing_not_another_models_target() {
        let billing = BillingEngine::default();
        billing.upsert_group(Group::pro_plus("capability-group", "Capabilities"));
        let claims = AuthClaims {
            card_id: "card".into(),
            group_id: "capability-group".into(),
            token_version: 0,
            exp: u64::MAX,
            iat: 0,
        };
        let mut shadow = ModelMap::new("shadow", "capability-group", "other", "provider", "alias");
        shadow.max_output = 8192;
        shadow.sort_order = -1;
        billing.upsert_model_map(shadow);
        let mut model = ModelMap::new(
            "model",
            "capability-group",
            "public",
            "provider",
            "upstream",
        )
        .with_alias("alias");
        model.context_window = 1_000_000;
        model.max_output = 128_000;
        billing.upsert_model_map(model);
        for id in ["public", "alias"] {
            assert_eq!(
                max_output_tokens_for_model(id, Some(&claims), &billing),
                128_000
            );
        }
        assert_eq!(
            max_output_tokens_for_model("upstream", Some(&claims), &billing),
            4096
        );
        assert_eq!(
            max_output_tokens_for_model("gemini-unknown", None, &billing),
            4096
        );
    }

    #[test]
    fn only_an_over_long_prompt_is_read_as_an_overflow() {
        let http = |status: u16, body: &str| {
            ProviderError::Http(reqwest::StatusCode::from_u16(status).unwrap(), body.into())
        };
        assert!(provider_input_too_long(&http(
            400,
            r#"{"error":{"message":"Prompt is too long: 201000 tokens > 200000 maximum"}}"#
        )));
        assert!(provider_input_too_long(&http(413, "")));
        assert!(!provider_input_too_long(&http(
            400,
            r#"{"error":{"message":"temperature is not supported"}}"#
        )));
        assert!(!provider_input_too_long(&http(500, "prompt is too long")));
        assert!(!provider_input_too_long(&ProviderError::Timeout));
    }
}

#[cfg(test)]
mod large_body_tests {
    use super::*;

    /// A large body holds its place for as long as any copy of it is kept (the archive
    /// keeps one while it writes the request down); a small one never takes a place.
    #[tokio::test]
    async fn a_large_body_holds_its_place_while_any_copy_of_it_is_kept() {
        let gate = LargeBodyGate::new(2, 1024, Duration::from_millis(50));
        let read = |size: usize, declared: Option<u64>| {
            read_body(Body::from(vec![b' '; size]), 4096, declared, &gate)
        };
        let Ok(BodyRead::Body(small)) = read(1024, Some(1024)).await else {
            panic!("a small body is read");
        };
        assert_eq!((small.len(), gate.in_use()), (1024, 0));
        for declared in [Some(2048), None] {
            let Ok(BodyRead::Body(body)) = read(2048, declared).await else {
                panic!("a large body is read");
            };
            assert_eq!((body.len(), gate.in_use()), (2048, 1));
            let archived = body.clone();
            drop(body);
            assert_eq!(gate.in_use(), 1);
            drop(archived);
            assert_eq!(gate.in_use(), 0);
        }
    }
}
