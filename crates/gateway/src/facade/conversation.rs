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

use super::{error_response, BoxFuture, FacadeHandler, Response};
use crate::auth::AuthClaims;
use crate::guardrail::{format_kiro_throttle_response, CapacityGuardrail};
use crate::idempotency::IdempotencyManager;
use crate::ops::{CardRateLimiter, RateLimitError};
use crate::provider::governance::{
    execute_stream_with_model_fallback, GovernanceError, ProviderKeyPool,
};
use crate::provider::ProviderRuntimeRegistry;
use crate::provider::{ModelProvider, ProviderConfig};
use crate::security::{ContentGuardrailConfig, GuardrailError};
use crate::stream::{create_stream_guard, BillingSettler, StreamGuardConfig};
use crate::translate::to_provider::{
    prepare_images, translate_kiro_to_chat_request, TranslationContext,
};
use crate::watchdog::{WatchdogConfig, WatchdogStream};
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

    fn handle<'a>(&'a self, req: Request<Body>) -> BoxFuture<'a, Response> {
        Box::pin(async move {
            let (parts, body) = req.into_parts();

            // 1. Extract amz-sdk-invocation-id header (Spec §4.7)
            let invocation_id = parts
                .headers
                .get("amz-sdk-invocation-id")
                .and_then(|h| h.to_str().ok())
                .map(ToString::to_string)
                .unwrap_or_else(|| {
                    format!(
                        "inv-{}",
                        SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_nanos()
                    )
                });

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

            // 3. Buffer request body
            let body_bytes = match axum::body::to_bytes(body, 10 * 1024 * 1024).await {
                Ok(b) => b,
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
            let body_str = String::from_utf8_lossy(&body_bytes);
            if self.intercept_intent
                && body_str.contains(INTENT_CLASSIFIER_SIGN_A)
                && body_str.contains(INTENT_CLASSIFIER_SIGN_B)
            {
                let is_spec = body_str.contains("create a spec")
                    || body_str.contains("specification")
                    || body_str.contains("需求文档")
                    || body_str.contains("规范");
                let probs = if is_spec {
                    serde_json::json!({ "chat": 0, "do": 0.1, "spec": 0.9 })
                } else {
                    serde_json::json!({ "chat": 0, "do": 0.95, "spec": 0.05 })
                };

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
                let requested_model_for_reservation = request_for_reservation
                    .and_then(|request| {
                        request
                            .conversation_state
                            .current_message
                            .user_input_message
                            .model_id
                            .as_deref()
                    })
                    .filter(|model| !model.trim().is_empty())
                    .unwrap_or_else(|| {
                        self.provider_config
                            .as_ref()
                            .map(|config| config.model.as_str())
                            .unwrap_or("default-model")
                    });
                let estimated_input_tokens = request_for_reservation
                    .map(estimate_input_tokens)
                    .unwrap_or(2_000);
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
                            needed,
                        } => {
                            return crate::guardrail::format_kiro_throttle_response(
                                StatusCode::TOO_MANY_REQUESTS,
                                "ThrottlingException",
                                "DAILY_LIMIT_EXCEEDED",
                                &format!("Daily credit limit reached (limit: {}, used today: {}, needed: {}).", limit, current, needed),
                                None,
                            );
                        }
                        billing::engine::BillingError::MonthlyLimitExceeded {
                            limit,
                            current,
                            needed,
                        } => {
                            return crate::guardrail::format_kiro_throttle_response(
                                StatusCode::TOO_MANY_REQUESTS,
                                "ThrottlingException",
                                "MONTHLY_LIMIT_EXCEEDED",
                                &format!("Monthly credit limit reached (limit: {}, used this month: {}, needed: {}).", limit, current, needed),
                                None,
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
                        _ => {
                            return error_response(
                                StatusCode::PAYMENT_REQUIRED,
                                "InsufficientCreditException",
                                &format!("Credit reservation failed: {}", e),
                            );
                        }
                    }
                }
                has_reservation = true;
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
                            return error_response(
                                StatusCode::FORBIDDEN,
                                "AccessDeniedException",
                                &format!(
                                    "Group '{}' ({:?}) cannot access provider with group_id {:?}",
                                    group.id, group.provider_binding_mode, cfg.group_id
                                ),
                            );
                        }
                    }
                }
            }

            let fallback_model = self
                .provider_config
                .as_ref()
                .map(|c| c.model.as_str())
                .unwrap_or("claude-3-5-sonnet-20241022");
            let requested_model = kiro_req
                .conversation_state
                .current_message
                .user_input_message
                .model_id
                .as_deref()
                .unwrap_or(fallback_model);
            if !valid_model_id(requested_model) {
                if has_reservation {
                    let _ = self.billing.release(&invocation_key);
                }
                return error_response(
                    StatusCode::BAD_REQUEST,
                    "InvalidRequestException",
                    "modelId is invalid",
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
                    if let Some(m) = billing_models
                        .iter()
                        .find(|bm| bm.matches_model(requested_model))
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
                        .is_some()
                        && !billing_models.is_empty()
                    {
                        if has_reservation {
                            let _ = self.billing.release(&invocation_key);
                        }
                        return error_response(
                            StatusCode::BAD_REQUEST,
                            "ValidationException",
                            "Requested model is not available for this card group",
                        );
                    }
                }
            }

            if !mapped_model && self.provider.is_some() && requested_model != fallback_model {
                if has_reservation {
                    let _ = self.billing.release(&invocation_key);
                }
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
                return error_response(
                    StatusCode::BAD_REQUEST,
                    "ValidationException",
                    "This model is not configured for image input. Send the prompt without an image, or ask your administrator to enable image support for it.",
                );
            }

            // 8. Translate to provider format (Spec §4.3, §14.3 Vision Fallback)
            let mut ctx =
                TranslationContext::new(&target_model).with_vision_support(vision_supported);

            if !ctx.supports_vision {
                if let Some(ref v_cfg) = self.vision_config {
                    if v_cfg.enabled {
                        if let (Some(ref v_url), Some(ref v_key)) =
                            (&v_cfg.fallback_provider_url, &v_cfg.fallback_api_key)
                        {
                            let current_input = &kiro_req
                                .conversation_state
                                .current_message
                                .user_input_message;
                            // One slot per image, so a failed transcription leaves its
                            // own image undescribed instead of shifting the rest.
                            let mut transcriptions = Vec::with_capacity(current_input.images.len());
                            for img in &current_input.images {
                                let format =
                                    crate::translate::images::sniff_format(&img.source.bytes)
                                        .unwrap_or(img.format.as_str());
                                let cache_key = vision_cache_key(
                                    &v_cfg.fallback_model,
                                    format,
                                    &img.source.bytes,
                                    &current_input.content,
                                );
                                let transcription = match self.vision_cache.get(&cache_key) {
                                    Some(cached) => Some(cached),
                                    None => {
                                        crate::translate::vision::transcribe_image_with_provider(
                                            &self.client,
                                            v_url,
                                            v_key,
                                            &v_cfg.fallback_model,
                                            format,
                                            &img.source.bytes,
                                            Some(&current_input.content),
                                            v_cfg.max_tokens,
                                        )
                                        .await
                                        .ok()
                                        .inspect(|desc| {
                                            self.vision_cache.set(cache_key, desc.clone())
                                        })
                                    }
                                };
                                transcriptions.push(transcription);
                            }
                            if transcriptions.iter().any(Option::is_some) {
                                ctx = ctx.with_image_transcriptions(transcriptions);
                            }
                        }
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
                    return error_response(
                        StatusCode::BAD_GATEWAY,
                        "RoutingException",
                        "No enabled or accessible upstream providers available for requested model targets",
                    );
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
            let (upstream_stream, actual_provider_id, actual_target_model) =
                if !candidates.is_empty() {
                    let (route_result, attempts) = crate::provider::retry::ATTEMPTS
                        .scope(std::sync::Mutex::new(Vec::new()), async {
                            let result = execute_stream_with_model_fallback(
                                &candidates,
                                &self.client,
                                &chat_req,
                                Duration::from_secs(60),
                                remaining_attempts,
                                now_secs,
                            )
                            .await;
                            let attempts = crate::provider::retry::ATTEMPTS
                                .with(|records| records.lock().unwrap().clone());
                            (result, attempts)
                        })
                        .await;
                    self.billing
                        .record_trace(billing::observability::RequestTrace {
                            id: format!(
                                "attempt-{}-{}",
                                invocation_key,
                                SystemTime::now()
                                    .duration_since(UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_nanos()
                            ),
                            card_id: claims
                                .as_ref()
                                .map(|c| c.card_id.clone())
                                .unwrap_or_default(),
                            ts: crate::now_secs(),
                            invocation_id: invocation_key.clone(),
                            exposed_model: requested_model.to_string(),
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
                                .map(|_| "upstream_start_failed".into()),
                            provider_id: attempts.last().map(|a| a.provider_id.clone()),
                            input_tokens: 0,
                            output_tokens: 0,
                            credits_charged: 0,
                            provider_cost_micro_cny: 0,
                            attempt_chain: attempts,
                        });
                    match route_result {
                        Ok(res) => (res.stream, res.provider.id, res.target_model),
                        Err(GovernanceError::AllKeysInCooldown {
                            next_recovery_secs, ..
                        }) => {
                            let _ = self.billing.release(&invocation_key);
                            return format_kiro_throttle_response(
                                StatusCode::TOO_MANY_REQUESTS,
                                "ThrottlingException",
                                "ALL_KEYS_IN_COOLDOWN",
                                "All upstream provider keys are currently in cooldown",
                                Some(next_recovery_secs.max(1)),
                            );
                        }
                        Err(e) => {
                            let _ = self.billing.release(&invocation_key);
                            return error_response(
                                StatusCode::BAD_GATEWAY,
                                "InternalServerException",
                                &crate::stream::safe_governance_error(&e),
                            );
                        }
                    }
                } else if let (Some(ref provider), Some(ref provider_config)) =
                    (&self.provider, &self.provider_config)
                {
                    if let Some(ref g) = card_group {
                        if !g.can_access_provider(provider_config.group_id.as_deref()) {
                            let _ = self.billing.release(&invocation_key);
                            return error_response(
                                StatusCode::FORBIDDEN,
                                "AccessDeniedException",
                                "Card group cannot access the configured upstream provider",
                            );
                        }
                    }
                    let mut direct_config = provider_config.clone();
                    direct_config.model = target_model.clone();
                    match crate::provider::retry::start_stream(
                        provider.as_ref(),
                        &self.client,
                        &direct_config,
                        &chat_req,
                        3,
                    )
                    .await
                    {
                        Ok(s) => (s, provider.name().to_string(), target_model.clone()),
                        Err(e) => {
                            // Upstream initiation failed; release reservation in full
                            let _ = self.billing.release(&invocation_key);
                            return error_response(
                                StatusCode::BAD_GATEWAY,
                                "InternalServerException",
                                &crate::stream::safe_provider_error(&e),
                            );
                        }
                    }
                } else {
                    let _ = self.billing.release(&invocation_key);
                    return error_response(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "ServiceUnavailableException",
                    "No upstream provider configured, enabled, or accessible for this card group",
                );
                };

            // 10. Wrap in Stream Guard (keepalive + cancellation + tool name restoration + billing settlement + contextUsage)
            let context_window = configured_context_window.unwrap_or_else(|| {
                billing::context::ModelContextLibrary::resolve(&actual_target_model).context_window
            });
            let guard_config = StreamGuardConfig {
                keepalive_interval: Duration::from_secs(20),
                model_id: actual_target_model.clone(),
                context_window: Some(context_window),
            };

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
            .with_estimated_input(reserved_estimated_input)
            .with_reservation_lease(reservation_lease);

            let watchdog_stream = WatchdogStream::new(upstream_stream, WatchdogConfig::default());
            let guarded_stream = create_stream_guard(
                watchdog_stream,
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
        })
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

fn valid_model_id(model: &str) -> bool {
    let model = model.trim();
    !model.is_empty()
        && model.chars().count() <= 128
        && model
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':' | '/'))
}

fn estimate_input_tokens(request: &GenerateAssistantResponseRequest) -> u64 {
    // The whole request counts — tool schemas, tool results, history, editor state —
    // except image payloads, which count at what a provider charges for an image.
    // Provider usage still settles the final charge whenever it is reported.
    serde_json::to_value(request)
        .map(|value| crate::usage_estimate::estimate_json_tokens(&value))
        .unwrap_or(200_000)
        .clamp(1, 200_000)
}

fn validate_conversation_request(
    request: &GenerateAssistantResponseRequest,
    guardrail: &ContentGuardrailConfig,
) -> Result<(), GuardrailError> {
    if request.conversation_state.conversation_id.trim().is_empty()
        || request.conversation_state.conversation_id.chars().count() > 256
    {
        return Err(GuardrailError::PromptTooLong {
            actual: request.conversation_state.conversation_id.chars().count(),
            max: 256,
        });
    }
    if request.conversation_state.history.len() > 1_000 {
        return Err(GuardrailError::PromptTooLong {
            actual: request.conversation_state.history.len(),
            max: 1_000,
        });
    }

    // Validate the complete serialized conversation, including tool schemas,
    // tool results, tool calls and metadata, rather than only visible text.
    let prompt_chars = serde_json::to_string(request)
        .map(|json| json.chars().count())
        .unwrap_or(usize::MAX);
    let current = &request
        .conversation_state
        .current_message
        .user_input_message;
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

/// Transcriptions are shared by every card, so the key must name the image and its
/// context exactly: SHA-256 over each length-prefixed part, not a 64-bit hash.
fn vision_cache_key(model: &str, format: &str, bytes: &str, context: &str) -> String {
    let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
    for part in [model, format, bytes, context] {
        digest.update(&(part.len() as u64).to_le_bytes());
        digest.update(part.as_bytes());
    }
    let hash: String = digest
        .finish()
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("{model}:{format}:{hash}")
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
}

#[cfg(test)]
mod vision_cache_key_tests {
    use super::vision_cache_key;

    #[test]
    fn key_is_a_full_digest_of_unambiguous_parts() {
        let key = vision_cache_key("model", "png", "ab", "c");
        let digest = key.rsplit(':').next().unwrap();
        assert_eq!(digest.len(), 64, "SHA-256, not a 64-bit hash: {key}");
        assert!(digest.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(key, vision_cache_key("model", "png", "ab", "c"));
        assert_ne!(key, vision_cache_key("model", "png", "a", "bc"));
        assert_ne!(key, vision_cache_key("model", "png", "ab", "d"));
    }
}
