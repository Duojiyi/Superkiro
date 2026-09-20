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

/// Internal state entry for each key in the pool, tracking SWRR current weight and cooldown.
#[derive(Debug, Clone)]
struct KeyEntry {
    key: ProviderKey,
    current_weight: i64,
    consecutive_failures: u32,
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
    pub fn mark_key_failure(&self, key_id: &str, now_secs: u64, cooldown: Duration) {
        let mut guard = self.inner.lock().unwrap();
        if let Some(entry) = guard.keys.iter_mut().find(|e| e.key.id == key_id) {
            entry.consecutive_failures += 1;
            entry.key.mark_failure(now_secs, cooldown);
            if entry.consecutive_failures >= 5 {
                entry.key.health_state = HealthState::Unhealthy;
            }
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
    }
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
    let provider_impl: Box<dyn ModelProvider> = match provider.format {
        ProviderFormat::OpenAi => Box::new(OpenAiProvider),
        ProviderFormat::Anthropic => Box::new(AnthropicProvider),
    };

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

        let key_id = key.id.clone();
        attempted_keys.push(key_id.clone());

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

        match super::retry::ATTEMPT_KEY
            .scope(
                (provider.id.clone(), key_id.clone()),
                super::retry::start_stream(
                    provider_impl.as_ref(),
                    client,
                    &config,
                    req_ref,
                    if pool.list_keys().len() == 1 {
                        max_attempts.min(3)
                    } else {
                        1
                    },
                ),
            )
            .await
        {
            Ok(stream) => {
                pool.mark_key_success(&key_id);
                return Ok((key, stream));
            }
            Err(e) => {
                let err_msg = e.to_string();
                failure_records.push((key_id.clone(), err_msg));

                let failed_at = crate::now_secs().max(now_secs);
                if is_cooldown_error(&e) {
                    if let ProviderError::Http(status, _) = e {
                        if status.as_u16() == 401 {
                            pool.mark_key_unhealthy(&key_id);
                        } else {
                            pool.mark_key_failure(&key_id, failed_at, default_cooldown);
                        }
                    } else {
                        pool.mark_key_failure(&key_id, failed_at, default_cooldown);
                    }
                    // Failover continues to next key
                } else {
                    // Non-retryable client error
                    return Err(GovernanceError::NonRetryable(e));
                }
            }
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
pub async fn execute_stream_with_model_fallback(
    candidates: &[(ProviderKeyPool, String)],
    client: &reqwest::Client,
    chat_req: &ChatRequest,
    default_cooldown: Duration,
    max_key_attempts_per_candidate: usize,
    now_secs: u64,
) -> Result<ModelFallbackResult, GovernanceError> {
    if candidates.is_empty() {
        return Err(GovernanceError::AllCandidatesExhausted);
    }

    let mut last_err = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(150);
    for (idx, (pool, target_model)) in candidates
        .iter()
        .take(max_key_attempts_per_candidate.clamp(1, 3))
        .enumerate()
    {
        match tokio::time::timeout_at(
            deadline,
            execute_stream_with_failover(
                pool,
                client,
                target_model,
                chat_req,
                default_cooldown,
                if candidates.len() > 1 {
                    1
                } else {
                    max_key_attempts_per_candidate
                },
                now_secs,
            ),
        )
        .await
        .unwrap_or(Err(GovernanceError::NonRetryable(ProviderError::Timeout)))
        {
            Ok((key, stream)) => {
                return Ok(ModelFallbackResult {
                    provider: pool.provider(),
                    key,
                    target_model: target_model.clone(),
                    stream,
                    candidate_index: idx,
                    was_fallback: idx > 0,
                });
            }
            Err(e @ GovernanceError::NonRetryable(_)) => return Err(e),
            Err(e) => {
                last_err = Some(e);
            }
        }
    }

    Err(last_err.unwrap_or(GovernanceError::AllCandidatesExhausted))
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
}

/// Perform a connectivity probe and latency/throughput benchmark on a provider key (Spec §14.3).
///
/// Sends a lightweight ping prompt ("ping", max_tokens=5), measures TTFT and tok/s,
/// and returns a comprehensive benchmark result.
pub async fn probe_provider_key(
    client: &reqwest::Client,
    provider: &Provider,
    key: &ProviderKey,
    target_model: &str,
    now_secs: u64,
) -> ProbeBenchmarkResult {
    let start = Instant::now();
    let provider_impl: Box<dyn ModelProvider> = match provider.format {
        ProviderFormat::OpenAi => Box::new(OpenAiProvider),
        ProviderFormat::Anthropic => Box::new(AnthropicProvider),
    };

    let config = ProviderConfig::new(
        &provider.base_url,
        &key.api_key,
        target_model,
        Duration::from_secs(15),
    );

    let probe_req = ChatRequest {
        reasoning_effort: None,
        model: target_model.to_string(),
        messages: vec![ChatMessage::new(
            "user",
            serde_json::Value::String("ping".to_string()),
        )],
        temperature: Some(0.0),
        max_tokens: Some(5),
        stream: true,
        tools: vec![],
    };

    match provider_impl.chat_stream(client, &config, &probe_req).await {
        Err(e) => {
            let total_latency_ms = start.elapsed().as_millis() as u64;
            let (status_code, err_text) = match e {
                ProviderError::Http(status, msg) => (Some(status.as_u16()), msg),
                other => (None, other.to_string()),
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
                error: Some(err_text),
                timestamp_secs: now_secs,
            }
        }
        Ok(mut stream) => {
            let mut ttft_ms = None;
            let mut tokens_emitted = 0u64;
            let mut stream_error = None;

            while let Some(event_res) = stream.next().await {
                match event_res {
                    Ok(ProviderStreamEvent::Delta(ProviderDelta::Text(text))) => {
                        if ttft_ms.is_none() {
                            ttft_ms = Some(start.elapsed().as_millis() as u64);
                        }
                        tokens_emitted += text.split_whitespace().count().max(1) as u64;
                    }
                    Ok(ProviderStreamEvent::Done) => break,
                    Ok(_) => {}
                    Err(e) => {
                        stream_error = Some(e.to_string());
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
            }
        }
    }
}
