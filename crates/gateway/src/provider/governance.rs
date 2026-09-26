//! Provider governance: multi-key weighted round-robin, cooldown, failover, and benchmarking.
//!
//! Spec §5 (Data Model) & Spec §14.3 (Provider Governance).

use super::anthropic::AnthropicProvider;
use super::openai::OpenAiProvider;
use super::{
    BoxStream, ChatMessage, ChatRequest, ModelProvider, ProviderConfig, ProviderDelta,
    ProviderError, ProviderStreamEvent,
};
use billing::provider::{HealthState, Provider, ProviderFormat, ProviderKey};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum GovernanceError {
    #[error("All keys for provider {provider_id} are currently in cooldown (next recovery in {next_recovery_secs}s)")]
    AllKeysInCooldown {
        provider_id: String,
        next_recovery_secs: u64,
    },

    #[error("No available or enabled keys configured for provider {provider_id}")]
    NoAvailableKeys { provider_id: String },

    #[error("All candidate keys exhausted during failover")]
    AllCandidatesExhausted,

    #[error("All candidate keys failed during failover: {attempts:?}")]
    AllCandidatesFailed { attempts: Vec<(String, String)> },

    #[error("Non-retryable provider error: {0}")]
    NonRetryable(ProviderError),

    #[error("Provider error: {0}")]
    Provider(#[from] ProviderError),
}

/// The longest a key rests after repeated transient failures before it is tried again.
pub const MAX_KEY_BACKOFF: Duration = Duration::from_secs(15 * 60);

/// Internal state entry for each key in the pool, tracking SWRR current weight and cooldown.
#[derive(Debug, Clone)]
struct KeyEntry {
    key: ProviderKey,
    current_weight: i64,
    consecutive_failures: u32,
    /// The last failure that cooled the key down or retired it, and when.
    last_error: Option<(String, u64)>,
}

/// A key's health in the running gateway, as the admin console shows it. Kept in memory:
/// a restart starts every key healthy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KeyHealth {
    /// "healthy", "cooldown", "degraded" (tried again after a cooldown, not yet recovered)
    /// or "unhealthy" (rejected as invalid; left out until reset or its secret changes).
    pub health_state: &'static str,
    pub cooldown_until: Option<u64>,
    pub last_error: Option<String>,
    pub last_error_at: Option<u64>,
}

impl KeyHealth {
    /// The health `key` records, with no failure noted.
    pub fn of(key: &ProviderKey, now_secs: u64) -> Self {
        let cooldown_until = key.cooldown_until.filter(|until| *until > now_secs);
        Self {
            health_state: match key.health_state {
                HealthState::Unhealthy => "unhealthy",
                _ if cooldown_until.is_some() => "cooldown",
                HealthState::Degraded => "degraded",
                HealthState::Healthy => "healthy",
            },
            cooldown_until,
            last_error: None,
            last_error_at: None,
        }
    }
}

#[derive(Debug)]
struct PoolInner {
    provider: Provider,
    keys: Vec<KeyEntry>,
}

/// Thread-safe governance pool for a provider and its multiple API keys.
#[derive(Debug, Clone)]
pub struct ProviderKeyPool {
    inner: Arc<Mutex<PoolInner>>,
}

impl ProviderKeyPool {
    /// Create a new key pool with a provider and optional initial keys.
    pub fn new(provider: Provider, keys: Vec<ProviderKey>) -> Self {
        let entries = keys
            .into_iter()
            .map(|k| KeyEntry {
                key: k,
                current_weight: 0,
                consecutive_failures: 0,
                last_error: None,
            })
            .collect();

        Self {
            inner: Arc::new(Mutex::new(PoolInner {
                provider,
                keys: entries,
            })),
        }
    }

    /// Add or update a key in the pool.
    pub fn add_key(&self, mut key: ProviderKey) {
        let mut guard = self.inner.lock().unwrap();
        if let Some(existing) = guard.keys.iter_mut().find(|e| e.key.id == key.id) {
            if existing.key.api_key == key.api_key {
                key.health_state = existing.key.health_state;
                key.cooldown_until = existing.key.cooldown_until;
            }
            existing.key = key;
        } else {
            guard.keys.push(KeyEntry {
                key,
                current_weight: 0,
                consecutive_failures: 0,
                last_error: None,
            });
        }
    }

    /// Remove a key from the pool by ID.
    pub fn remove_key(&self, key_id: &str) -> bool {
        let mut guard = self.inner.lock().unwrap();
        let init_len = guard.keys.len();
        guard.keys.retain(|e| e.key.id != key_id);
        guard.keys.len() < init_len
    }

    /// Get current provider metadata.
    pub fn provider(&self) -> Provider {
        self.inner.lock().unwrap().provider.clone()
    }

    /// Update provider metadata and status in place without wiping key health history (T04).
    pub fn update_provider(&self, provider: Provider) {
        let mut guard = self.inner.lock().unwrap();
        guard.provider = provider;
    }

    /// List all keys in the pool.
    pub fn list_keys(&self) -> Vec<ProviderKey> {
        self.inner
            .lock()
            .unwrap()
            .keys
            .iter()
            .map(|e| e.key.clone())
            .collect()
    }

    /// Select the next eligible key using Smooth Weighted Round-Robin (SWRR, Spec §14.3).
    ///
    /// Excludes any keys listed in `exclude_key_ids` (used for failover retries).
    pub fn select_key(
        &self,
        now_secs: u64,
        exclude_key_ids: &[String],
    ) -> Result<ProviderKey, GovernanceError> {
        self.select_key_for_model(now_secs, exclude_key_ids, None)
    }

    pub fn select_key_for_model(
        &self,
        now_secs: u64,
        exclude_key_ids: &[String],
        model: Option<&str>,
    ) -> Result<ProviderKey, GovernanceError> {
        let mut guard = self.inner.lock().unwrap();
        let provider_id = guard.provider.id.clone();

        if !guard.provider.enabled {
            return Err(GovernanceError::NoAvailableKeys { provider_id });
        }

        if guard.keys.is_empty() {
            return Err(GovernanceError::NoAvailableKeys { provider_id });
        }

        // Check availability of candidates
        let mut eligible_indices = Vec::new();
        let mut in_cooldown_min_secs = None;
        let mut any_enabled = false;

        for (idx, entry) in guard.keys.iter().enumerate() {
            if model.is_some_and(|m| !entry.key.supports_model(m))
                || exclude_key_ids.contains(&entry.key.id)
            {
                continue;
            }
            if !entry.key.enabled || entry.key.health_state == HealthState::Unhealthy {
                continue;
            }
            any_enabled = true;

            if let Some(cooldown) = entry.key.cooldown_until {
                if now_secs < cooldown {
                    let diff = cooldown - now_secs;
                    in_cooldown_min_secs = Some(match in_cooldown_min_secs {
                        Some(prev) => std::cmp::min(prev, diff),
                        None => diff,
                    });
                    continue;
                }
            }

            eligible_indices.push(idx);
        }

        if eligible_indices.is_empty() {
            if !exclude_key_ids.is_empty() && any_enabled {
                return Err(GovernanceError::AllCandidatesExhausted);
            }
            if let Some(min_secs) = in_cooldown_min_secs {
                return Err(GovernanceError::AllKeysInCooldown {
                    provider_id,
                    next_recovery_secs: min_secs,
                });
            }
            return Err(GovernanceError::NoAvailableKeys { provider_id });
        }

        // Smooth Weighted Round-Robin (SWRR) algorithm
        let total_weight: i64 = eligible_indices
            .iter()
            .map(|&idx| guard.keys[idx].key.weight.max(1) as i64)
            .sum();

        let mut best_idx = eligible_indices[0];
        let mut best_current_weight = i64::MIN;

        for &idx in &eligible_indices {
            let weight = guard.keys[idx].key.weight.max(1) as i64;
            guard.keys[idx].current_weight += weight;
            if guard.keys[idx].current_weight > best_current_weight {
                best_current_weight = guard.keys[idx].current_weight;
                best_idx = idx;
            }
        }

        guard.keys[best_idx].current_weight -= total_weight;
        Ok(guard.keys[best_idx].key.clone())
    }

    /// Mark a key failure and apply cooldown (Spec §14.3).
    ///
    /// Failures from a fifth in a row on double the cooldown each time, up to
    /// [`MAX_KEY_BACKOFF`], but never retire the key: a rate limit or a provider incident
    /// passes. When a cooldown ends, the next request is the key's trial, and a success
    /// restores it. Only an invalid key is retired, by [`Self::mark_key_unhealthy`].
    pub fn mark_key_failure(&self, key_id: &str, now_secs: u64, cooldown: Duration) {
        let mut guard = self.inner.lock().unwrap();
        if let Some(entry) = guard.keys.iter_mut().find(|e| e.key.id == key_id) {
            entry.consecutive_failures = entry.consecutive_failures.saturating_add(1);
            let doublings = entry.consecutive_failures.saturating_sub(4).min(10);
            let backoff = cooldown
                .saturating_mul(1 << doublings)
                .min(MAX_KEY_BACKOFF.max(cooldown));
            entry.key.mark_failure(now_secs, backoff);
        }
    }

    /// Mark a key permanently unhealthy / disabled (e.g. HTTP 401 invalid key).
    pub fn mark_key_unhealthy(&self, key_id: &str) {
        let mut guard = self.inner.lock().unwrap();
        if let Some(entry) = guard.keys.iter_mut().find(|e| e.key.id == key_id) {
            entry.key.health_state = HealthState::Unhealthy;
            entry.key.enabled = false;
        }
    }

    /// Mark a key success and clear cooldown / degraded status.
    pub fn mark_key_success(&self, key_id: &str) {
        let mut guard = self.inner.lock().unwrap();
        if let Some(entry) = guard.keys.iter_mut().find(|e| e.key.id == key_id) {
            entry.consecutive_failures = 0;
            entry.key.mark_success();
        }
    }

    /// Note the failure that just cooled a key down or retired it, named as its attempt is.
    pub fn note_key_error(&self, key_id: &str, error: &ProviderError, at_secs: u64) {
        let mut guard = self.inner.lock().unwrap();
        if let Some(entry) = guard.keys.iter_mut().find(|e| e.key.id == key_id) {
            entry.last_error = Some((super::retry::failure_class(error), at_secs));
        }
    }

    /// The operator's reset: the key leaves its cooldown or its retirement, as after a
    /// success; its last failure stays on record. A retired key was also switched off, and
    /// is on again once synced with its saved state. False when the pool has no such key.
    pub fn reset_key(&self, key_id: &str) -> bool {
        let mut guard = self.inner.lock().unwrap();
        let Some(entry) = guard.keys.iter_mut().find(|e| e.key.id == key_id) else {
            return false;
        };
        entry.consecutive_failures = 0;
        entry.key.mark_success();
        true
    }

    /// Each key's live health, by key ID.
    pub fn key_health(&self, now_secs: u64) -> Vec<(String, KeyHealth)> {
        self.inner
            .lock()
            .unwrap()
            .keys
            .iter()
            .map(|entry| {
                let mut health = KeyHealth::of(&entry.key, now_secs);
                if let Some((error, at)) = &entry.last_error {
                    health.last_error = Some(error.clone());
                    health.last_error_at = Some(*at);
                }
                (entry.key.id.clone(), health)
            })
            .collect()
    }
}

/// Check whether an upstream error warrants key cooldown and failover (Spec §14.3).
pub fn is_cooldown_error(err: &ProviderError) -> bool {
    match err {
        ProviderError::Http(status, _) => {
            status.as_u16() == 429 || status.is_server_error() || status.as_u16() == 401
        }
        ProviderError::Timeout | ProviderError::Network(_) | ProviderError::StreamDisconnected => {
            true
        }
        ProviderError::Parse(_) | ProviderError::Serialization(_) => false,
        ProviderError::Service => false,
        ProviderError::Watchdog(_) => true,
        // The key answered; an empty answer says nothing about the key.
        ProviderError::EmptyCompletion => false,
    }
}

fn provider_adapter(provider: &Provider) -> Box<dyn ModelProvider> {
    match provider.format {
        ProviderFormat::OpenAi => Box::new(OpenAiProvider),
        ProviderFormat::Anthropic => Box::new(AnthropicProvider),
    }
}

/// Whether a failed attempt leaves another key or target worth trying for this request.
/// Anything else is a problem with the request itself, which every key would share.
fn worth_another_attempt(error: &ProviderError) -> bool {
    matches!(error, ProviderError::EmptyCompletion) || is_cooldown_error(error)
}

/// Start the stream with one key of `pool`, making up to `retries` attempts with it, and
/// record the outcome on the key.
#[allow(clippy::too_many_arguments)]
async fn attempt_with_key(
    pool: &ProviderKeyPool,
    provider: &Provider,
    key: &ProviderKey,
    client: &reqwest::Client,
    model: &str,
    chat_req: &ChatRequest,
    default_cooldown: Duration,
    retries: usize,
    now_secs: u64,
) -> Result<BoxStream<'static, Result<ProviderStreamEvent, ProviderError>>, ProviderError> {
    let config = ProviderConfig::new(
        &provider.base_url,
        &key.api_key,
        model,
        Duration::from_secs(600),
    );

    let req_to_use;
    let req_ref = if chat_req.model == model {
        chat_req
    } else {
        req_to_use = {
            let mut r = chat_req.clone();
            r.model = model.to_string();
            r
        };
        &req_to_use
    };

    let result = super::retry::ATTEMPT_KEY
        .scope(
            (provider.id.clone(), key.id.clone()),
            super::retry::start_stream(
                provider_adapter(provider).as_ref(),
                client,
                &config,
                req_ref,
                retries,
            ),
        )
        .await;
    let failed_at = crate::now_secs().max(now_secs);
    match &result {
        Ok(_) => pool.mark_key_success(&key.id),
        // Worth another key, but not a reason to cool this one down.
        Err(ProviderError::EmptyCompletion) => {}
        Err(error @ ProviderError::Http(status, _)) if status.as_u16() == 401 => {
            pool.mark_key_unhealthy(&key.id);
            pool.note_key_error(&key.id, error, failed_at);
        }
        Err(error) if is_cooldown_error(error) => {
            pool.mark_key_failure(&key.id, failed_at, default_cooldown);
            pool.note_key_error(&key.id, error, failed_at);
        }
        // A problem with the request says nothing about the key.
        Err(_) => {}
    }
    result
}

/// Execute a streaming chat request with automatic multi-key failover and cooldown (Spec §14.3).
pub async fn execute_stream_with_failover(
    pool: &ProviderKeyPool,
    client: &reqwest::Client,
    model: &str,
    chat_req: &ChatRequest,
    default_cooldown: Duration,
    max_attempts: usize,
    now_secs: u64,
) -> Result<
    (
        ProviderKey,
        BoxStream<'static, Result<ProviderStreamEvent, ProviderError>>,
    ),
    GovernanceError,
> {
    let provider = pool.provider();
    let mut attempted_keys = Vec::new();
    let mut failure_records = Vec::new();

    let attempts_limit = max_attempts.clamp(1, 3);

    for _ in 0..attempts_limit {
        let key = match pool.select_key_for_model(
            crate::now_secs().max(now_secs),
            &attempted_keys,
            Some(model),
        ) {
            Ok(k) => k,
            Err(GovernanceError::AllCandidatesExhausted) => break,
            Err(e) => return Err(e),
        };
        attempted_keys.push(key.id.clone());

        // A lone key gets the retries a pool would spend on its other keys.
        let retries = if pool.list_keys().len() == 1 {
            max_attempts.min(3)
        } else {
            1
        };
        match attempt_with_key(
            pool,
            &provider,
            &key,
            client,
            model,
            chat_req,
            default_cooldown,
            retries,
            now_secs,
        )
        .await
        {
            Ok(stream) => return Ok((key, stream)),
            Err(e) if worth_another_attempt(&e) => {
                failure_records.push((key.id.clone(), e.to_string()));
            }
            Err(e) => return Err(GovernanceError::NonRetryable(e)),
        }
    }

    Err(GovernanceError::AllCandidatesFailed {
        attempts: failure_records,
    })
}

/// Result of executing a streaming request through a model fallback chain (Spec §14.6, P4-10).
pub struct ModelFallbackResult {
    pub provider: Provider,
    pub key: ProviderKey,
    pub target_model: String,
    pub stream: BoxStream<'static, Result<ProviderStreamEvent, ProviderError>>,
    pub candidate_index: usize,
    pub was_fallback: bool,
}

impl std::fmt::Debug for ModelFallbackResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelFallbackResult")
            .field("provider", &self.provider)
            .field("key", &self.key)
            .field("target_model", &self.target_model)
            .field("candidate_index", &self.candidate_index)
            .field("was_fallback", &self.was_fallback)
            .finish()
    }
}

/// Execute a streaming chat request with multi-model fallback chain support (Spec §14.6, P4-10).
///
/// Iterates through candidate model targets `(ProviderKeyPool, target_model)` in priority order:
/// If the primary target fails (all keys in cooldown or unrecoverable error), automatically falls back
/// to the next candidate model in the chain.
///
/// `max_attempts` bounds the upstream requests actually sent, across the whole chain. A
/// chain tries one key of each target in order, then untried keys again from the top
/// while attempts remain; a target with no key to try right now is passed over without
/// spending one, so the whole chain is always considered.
pub async fn execute_stream_with_model_fallback(
    candidates: &[(ProviderKeyPool, String)],
    client: &reqwest::Client,
    chat_req: &ChatRequest,
    default_cooldown: Duration,
    max_attempts: usize,
    now_secs: u64,
) -> Result<ModelFallbackResult, GovernanceError> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(150);
    let (pool, target_model) = match candidates {
        [] => return Err(GovernanceError::AllCandidatesExhausted),
        [only] => only,
        chain => {
            return execute_chain(
                chain,
                client,
                chat_req,
                default_cooldown,
                max_attempts,
                now_secs,
                deadline,
            )
            .await
        }
    };
    // A single target spends every attempt on its own keys.
    let (key, stream) = tokio::time::timeout_at(
        deadline,
        execute_stream_with_failover(
            pool,
            client,
            target_model,
            chat_req,
            default_cooldown,
            max_attempts,
            now_secs,
        ),
    )
    .await
    .unwrap_or(Err(GovernanceError::NonRetryable(ProviderError::Timeout)))?;
    Ok(ModelFallbackResult {
        provider: pool.provider(),
        key,
        target_model: target_model.clone(),
        stream,
        candidate_index: 0,
        was_fallback: false,
    })
}

async fn execute_chain(
    chain: &[(ProviderKeyPool, String)],
    client: &reqwest::Client,
    chat_req: &ChatRequest,
    default_cooldown: Duration,
    max_attempts: usize,
    now_secs: u64,
    deadline: tokio::time::Instant,
) -> Result<ModelFallbackResult, GovernanceError> {
    let mut attempts_left = max_attempts.clamp(1, 3);
    let mut tried: Vec<Vec<String>> = vec![Vec::new(); chain.len()];
    let mut failures = Vec::new();
    // Why the last target could not serve, if not a failed attempt, as each target was
    // first considered. Later passes only spend attempts left on untried keys.
    let mut unavailable = None;
    let mut first_pass = true;
    while attempts_left > 0 {
        let mut attempted = false;
        for (idx, (pool, target_model)) in chain.iter().enumerate() {
            while attempts_left > 0 {
                let key = match pool.select_key_for_model(
                    crate::now_secs().max(now_secs),
                    &tried[idx],
                    Some(target_model),
                ) {
                    Ok(key) => key,
                    Err(e) => {
                        if first_pass && tried[idx].is_empty() {
                            unavailable = Some(e);
                        }
                        break;
                    }
                };
                tried[idx].push(key.id.clone());
                attempts_left -= 1;
                attempted = true;
                let provider = pool.provider();
                let outcome = tokio::time::timeout_at(
                    deadline,
                    attempt_with_key(
                        pool,
                        &provider,
                        &key,
                        client,
                        target_model,
                        chat_req,
                        default_cooldown,
                        1,
                        now_secs,
                    ),
                )
                .await
                .unwrap_or(Err(ProviderError::Timeout));
                match outcome {
                    Ok(stream) => {
                        return Ok(ModelFallbackResult {
                            provider,
                            key,
                            target_model: target_model.clone(),
                            stream,
                            candidate_index: idx,
                            was_fallback: idx > 0,
                        })
                    }
                    Err(e)
                        if worth_another_attempt(&e) && tokio::time::Instant::now() < deadline =>
                    {
                        let own_key_problem = matches!(
                            &e,
                            ProviderError::Http(status, _) if matches!(status.as_u16(), 401 | 429)
                        );
                        failures.push((key.id.clone(), e.to_string()));
                        if first_pass {
                            unavailable = None;
                        }
                        // A rate-limited or invalid key says nothing about its target's
                        // other keys, which keep the model asked for. Anything else may
                        // be the provider failing, so the next target goes first.
                        if !own_key_problem {
                            break;
                        }
                    }
                    Err(e) => return Err(GovernanceError::NonRetryable(e)),
                }
            }
        }
        first_pass = false;
        if !attempted {
            break;
        }
    }
    Err(match unavailable {
        Some(e) => e,
        None if !failures.is_empty() => GovernanceError::AllCandidatesFailed { attempts: failures },
        None => GovernanceError::AllCandidatesExhausted,
    })
}

/// Latency and throughput benchmark probe results (Spec §14.3).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProbeBenchmarkResult {
    pub key_id: String,
    pub provider_id: String,
    pub success: bool,
    pub status_code: Option<u16>,
    pub total_latency_ms: u64,
    pub ttft_ms: Option<u64>,
    pub tokens_per_second: Option<f64>,
    pub tokens_emitted: u64,
    pub error: Option<String>,
    pub timestamp_secs: u64,
    /// The start of the model's answer, at most [`PROBE_REPLY_CHARS`] characters.
    pub reply: Option<String>,
}

/// How much of a probe's answer is kept.
pub const PROBE_REPLY_CHARS: usize = 80;

/// Perform a connectivity probe and latency/throughput benchmark on a provider key (Spec §14.3).
///
/// Sends one minimal request as traffic is sent: in the provider's format, streamed, and
/// started by the same code, without retries, asking for a one-word answer with a small
/// output limit. Measures TTFT and tok/s and keeps the start of the answer. The key's
/// runtime health is left as it is: a probe is the operator's check, not traffic, so a
/// failed one never takes a serving key out of rotation and a passing one never returns a
/// key the router retired. The secret never appears in the result, even when an upstream
/// echoes it in an error.
pub async fn probe_provider_key(
    client: &reqwest::Client,
    provider: &Provider,
    key: &ProviderKey,
    target_model: &str,
    now_secs: u64,
) -> ProbeBenchmarkResult {
    let start = Instant::now();
    let config = ProviderConfig::new(
        &provider.base_url,
        &key.api_key,
        target_model,
        Duration::from_secs(30),
    );

    let probe_req = ChatRequest {
        reasoning_effort: None,
        model: target_model.to_string(),
        messages: vec![ChatMessage::new(
            "user",
            serde_json::Value::String("Reply with OK".to_string()),
        )],
        // What a translated Kiro request carries.
        temperature: Some(0.7),
        max_tokens: Some(16),
        stream: true,
        tools: vec![],
    };
    let redact = |text: String| -> String {
        let text = if key.api_key.is_empty() {
            text
        } else {
            text.replace(&key.api_key, "[redacted]")
        };
        text.chars().take(300).collect()
    };

    match super::retry::start_stream(
        provider_adapter(provider).as_ref(),
        client,
        &config,
        &probe_req,
        1,
    )
    .await
    {
        Err(e) => {
            let total_latency_ms = start.elapsed().as_millis() as u64;
            let status_code = match &e {
                ProviderError::Http(status, _) => Some(status.as_u16()),
                _ => None,
            };
            ProbeBenchmarkResult {
                key_id: key.id.clone(),
                provider_id: provider.id.clone(),
                success: false,
                status_code,
                total_latency_ms,
                ttft_ms: None,
                tokens_per_second: None,
                tokens_emitted: 0,
                error: Some(redact(e.to_string())),
                timestamp_secs: now_secs,
                reply: None,
            }
        }
        Ok(mut stream) => {
            let mut ttft_ms = None;
            let mut tokens_emitted = 0u64;
            let mut stream_error = None;
            let mut reply = String::new();

            while let Some(event_res) = stream.next().await {
                match event_res {
                    Ok(ProviderStreamEvent::Delta(delta)) => {
                        if ttft_ms.is_none() {
                            ttft_ms = Some(start.elapsed().as_millis() as u64);
                        }
                        if let ProviderDelta::Text(text) = delta {
                            tokens_emitted += text.split_whitespace().count().max(1) as u64;
                            let room = PROBE_REPLY_CHARS.saturating_sub(reply.chars().count());
                            reply.extend(text.chars().take(room));
                        }
                    }
                    Ok(ProviderStreamEvent::Done) => break,
                    Ok(_) => {}
                    Err(e) => {
                        stream_error = Some(redact(e.to_string()));
                        break;
                    }
                }
            }

            let total_latency_ms = start.elapsed().as_millis() as u64;

            let tokens_per_second = if let Some(ttft) = ttft_ms {
                let duration_after_ttft_ms = total_latency_ms.saturating_sub(ttft);
                if duration_after_ttft_ms > 0 && tokens_emitted > 0 {
                    Some((tokens_emitted as f64) / (duration_after_ttft_ms as f64 / 1000.0))
                } else {
                    Some((tokens_emitted as f64) / (total_latency_ms.max(1) as f64 / 1000.0))
                }
            } else {
                None
            };

            let reply: String = redact(reply.trim().to_string())
                .chars()
                .take(PROBE_REPLY_CHARS)
                .collect();
            ProbeBenchmarkResult {
                key_id: key.id.clone(),
                provider_id: provider.id.clone(),
                success: stream_error.is_none(),
                status_code: Some(200),
                total_latency_ms,
                ttft_ms,
                tokens_per_second,
                tokens_emitted,
                error: stream_error,
                timestamp_secs: now_secs,
                reply: (!reply.is_empty()).then_some(reply),
            }
        }
    }
}
