//! Stream orchestration, keepalive pinging, and client disconnect cancellation (Spec §4.6, §6.3).
//!
//! Bridges upstream provider event streams with Kiro IDE AWS EventStream binary frames.
//! Injects periodic empty `assistantResponseEvent` keepalive frames (every 20s) to reset
//! Kiro's 60s/300s watchdog, and detects downstream socket disconnects to cancel upstream
//! requests immediately without leaking resources or double-billing.

use crate::idempotency::{CompletedInvocation, IdempotencyGuard};
use crate::provider::{ProviderDelta, ProviderError, ProviderStreamEvent};
use crate::translate::{from_provider::StreamTranslationState, tools::ToolRegistry};
use billing::engine::{BillingEngine, BillingError};
use billing::ledger::UsageTokens;
use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use std::collections::BTreeMap;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// RAII settler for credit reservations (Spec §6.1, §6.3, §6.7).
///
/// If settled explicitly via `settle()`, computes exact charges and writes to ledger.
/// If dropped without being settled (e.g. client disconnect with 0 tokens or upstream failure),
/// automatically releases the frozen reservation in `drop()`.
#[derive(Debug)]
pub struct BillingSettler {
    pub billing: BillingEngine,
    pub invocation_id: String,
    pub exposed_model: String,
    pub provider_id: String,
    pub target_model: String,
    pub settled: bool,
    pub permit: Option<crate::guardrail::CapacityPermit>,
    pub estimated_input_tokens: u64,
    settlement_attempted: bool,
    metrics: Option<crate::ops::metrics::RequestMetrics>,
}

impl BillingSettler {
    pub fn new(
        billing: BillingEngine,
        invocation_id: String,
        exposed_model: String,
        provider_id: String,
        target_model: String,
    ) -> Self {
        Self {
            billing,
            invocation_id,
            exposed_model,
            provider_id,
            target_model,
            settled: false,
            permit: None,
            estimated_input_tokens: 0,
            settlement_attempted: false,
            metrics: None,
        }
    }

    pub fn with_metrics(mut self, metrics: Option<crate::ops::metrics::RequestMetrics>) -> Self {
        self.metrics = metrics;
        self
    }

    pub fn with_permit(mut self, permit: crate::guardrail::CapacityPermit) -> Self {
        self.permit = Some(permit);
        self
    }

    pub fn with_estimated_input(mut self, est: u64) -> Self {
        self.estimated_input_tokens = est;
        self
    }

    pub fn settle(&mut self, tokens: &UsageTokens) -> Result<(), BillingError> {
        if self.settled {
            return Ok(());
        }
        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.settlement_attempted = true;
        let result = self.billing.settle(
            &self.invocation_id,
            tokens,
            &self.exposed_model,
            &self.provider_id,
            &self.target_model,
            now_secs,
        );
        if let Ok(entry) = &result {
            self.settled = true;
            if let Some(metrics) = &self.metrics {
                metrics.collector.record_credits(entry.credits_charged);
            }
        } else if let Some(metrics) = &self.metrics {
            metrics.mark_error();
        }
        result.map(|_| ())
    }
}

impl Drop for BillingSettler {
    fn drop(&mut self) {
        if !self.settled && !self.settlement_attempted {
            let _ = self.billing.release(&self.invocation_id);
        }
    }
}

/// Configuration for the stream guard orchestrator.
#[derive(Debug, Clone)]
pub struct StreamGuardConfig {
    /// Interval between keepalive pings if upstream produces no events.
    /// Default is 20s (Spec §4.6).
    pub keepalive_interval: Duration,
    /// Model identifier to attach to text frames.
    pub model_id: String,
    /// Model context window size in tokens to drive contextUsageEvent percentage (Spec §4.5).
    pub context_window: Option<u32>,
}

impl Default for StreamGuardConfig {
    fn default() -> Self {
        Self {
            keepalive_interval: Duration::from_secs(20),
            model_id: "default-model".to_string(),
            context_window: None,
        }
    }
}

impl StreamGuardConfig {
    pub fn with_context_window(mut self, window: u32) -> Self {
        self.context_window = Some(window);
        self
    }
}

#[derive(Default)]
struct ToolBuffer {
    id: String,
    name: String,
    arguments: String,
}

/// Simple Stream wrapper around tokio mpsc::Receiver for binary frame chunks.
pub struct FrameStream {
    inner: tokio::sync::mpsc::Receiver<Result<Bytes, std::io::Error>>,
}

impl Stream for FrameStream {
    type Item = Result<Bytes, std::io::Error>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.inner.poll_recv(cx)
    }
}

/// Orchestrate provider stream into AWS EventStream binary frames with keepalive and cancellation.
pub fn create_stream_guard(
    upstream_stream: impl Stream<Item = Result<ProviderStreamEvent, ProviderError>> + Send + 'static,
    config: StreamGuardConfig,
    tool_registry: Option<ToolRegistry>,
    mut idempotency_guard: Option<IdempotencyGuard>,
    mut billing_settler: Option<BillingSettler>,
) -> FrameStream {
    let (tx, rx) = tokio::sync::mpsc::channel(64);

    tokio::spawn(async move {
        let mut interval = tokio::time::interval(config.keepalive_interval);
        interval.tick().await;
        let mut upstream = Box::pin(upstream_stream);
        let mut tool_buffers: BTreeMap<usize, ToolBuffer> = BTreeMap::new();
        let mut usage = crate::provider::TokenUsage::default();
        let mut has_input_usage = false;
        let mut output_chars = 0usize;
        let mut saw_output = false;
        let mut completed = false;
        let mut rejected_empty = false;
        let context_window = config
            .context_window
            .unwrap_or_else(|| {
                billing::context::ModelContextLibrary::resolve(&config.model_id).context_window
            })
            .max(1);
        let mut stop_reason = None;

        'stream: loop {
            tokio::select! {
                _ = tx.closed() => break,
                _ = interval.tick() => {
                    if tx.send(Ok(Bytes::from(kiro_wire::encoder::encode_keepalive()))).await.is_err() {
                        break;
                    }
                }
                event = upstream.next() => {
                    interval.reset();
                    match event {
                        Some(Ok(ProviderStreamEvent::Delta(delta))) => {
                            let frame = match delta {
                                ProviderDelta::Text(text) => {
                                    saw_output |= !text.is_empty();
                                    output_chars = output_chars.saturating_add(text.chars().count());
                                    Some(kiro_wire::encoder::encode_assistant_response(&text, Some(&config.model_id)))
                                }
                                ProviderDelta::Reasoning(text) => {
                                    saw_output |= !text.is_empty();
                                    output_chars = output_chars.saturating_add(text.chars().count());
                                    Some(kiro_wire::encoder::encode_reasoning(Some(&text), None, None))
                                }
                                ProviderDelta::ToolCallChunk { index, id, name, arguments } => {
                                    saw_output |= !arguments.is_empty()
                                        || id.as_ref().is_some_and(|value| !value.is_empty())
                                        || name.as_ref().is_some_and(|value| !value.is_empty());
                                    output_chars = output_chars.saturating_add(arguments.chars().count())
                                        .saturating_add(name.as_ref().map_or(0, |n| n.chars().count()));
                                    let buf = tool_buffers.entry(index).or_default();
                                    if let Some(id) = id { buf.id = id; }
                                    if let Some(name) = name {
                                        buf.name = tool_registry.as_ref().map_or_else(|| name.clone(), |r| r.restore(&name));
                                    }
                                    buf.arguments.push_str(&arguments);
                                    None
                                }
                            };
                            if let Some(frame) = frame {
                                if tx.send(Ok(Bytes::from(frame))).await.is_err() { break; }
                            }
                        }
                        Some(Ok(ProviderStreamEvent::Usage(next))) => {
                            // Output-only Anthropic message_delta must not erase prompt/cache observations.
                            has_input_usage |= next.prompt_tokens > 0 || next.uncached_prompt_tokens > 0
                                || next.cache_read_input_tokens.is_some() || next.cache_creation_input_tokens.is_some();
                            usage.merge(&next);
                            let wire_usage = kiro_wire::events::TokenUsage {
                                uncached_input_tokens: usage.uncached_prompt_tokens.min(i64::MAX as u64) as i64,
                                output_tokens: usage.completion_tokens.min(i64::MAX as u64) as i64,
                                cache_read_input_tokens: usage.cache_read_input_tokens.unwrap_or(0).min(i64::MAX as u64) as i64,
                                cache_write_input_tokens: usage.cache_creation_input_tokens.unwrap_or(0).min(i64::MAX as u64) as i64,
                            };
                            let frames = [
                                kiro_wire::encoder::encode_metadata(Some(wire_usage), None),
                                kiro_wire::encoder::encode_context_usage((usage.total_tokens as f64 / context_window as f64).clamp(0.0, 1.0)),
                            ];
                            for frame in frames {
                                if tx.send(Ok(Bytes::from(frame))).await.is_err() { break 'stream; }
                            }
                        }
                        Some(Ok(ProviderStreamEvent::StopReason(reason))) => {
                            stop_reason = Some(StreamTranslationState::map_stop_reason(&reason));
                        }
                        Some(Ok(ProviderStreamEvent::Done)) => {
                            // A terminal marker alone does not constitute a response. Do not
                            // charge input-only usage or cache this invocation as successful.
                            if !saw_output {
                                rejected_empty = true;
                                let frame = kiro_wire::encoder::encode_exception(
                                    "InternalServerException",
                                    "Upstream completed without producing any output",
                                );
                                let _ = tx.send(Ok(Bytes::from(frame))).await;
                                break;
                            }
                            for (_, buf) in std::mem::take(&mut tool_buffers) {
                                if !buf.name.is_empty() || !buf.id.is_empty() {
                                    let frame = kiro_wire::encoder::encode_tool_use(&buf.name, &buf.id, &buf.arguments, true);
                                    if tx.send(Ok(Bytes::from(frame))).await.is_err() { break 'stream; }
                                }
                            }
                            completed = true;
                            break;
                        }
                        error => {
                            let error = match error {
                                Some(Err(error)) => {
                                    let safe = safe_provider_error(&error);
                                    eprintln!("stream_failure model={} class={safe}", config.model_id);
                                    safe
                                },
                                _ => "Upstream stream ended before completion".to_string(),
                            };
                            let friendly = format!("\n\n**上游模型服务异常**：{error}\n");
                            let frame = kiro_wire::encoder::encode_assistant_response(&friendly, Some(&config.model_id));
                            let _ = tx.send(Ok(Bytes::from(frame))).await;
                            let frame = kiro_wire::encoder::encode_exception("InternalServerException", &error);
                            let _ = tx.send(Ok(Bytes::from(frame))).await;
                            break;
                        }
                    }
                }
            }
        }
        // One finalization path for Done, EOF, provider errors, cancellation and failed sends.
        // Dropping this receiver cancels the provider pump even while it waits for bytes.
        drop(upstream);
        let mut settlement_ok = true;
        let mut billable_attempt = false;
        if let Some(mut settler) = billing_settler.take() {
            if !completed {
                if let Some(metrics) = &settler.metrics {
                    metrics.mark_error();
                }
            }
            if let Some(tokens) = resolve_settlement_tokens(
                &usage,
                has_input_usage,
                output_chars,
                saw_output,
                settler.estimated_input_tokens,
            )
            .filter(|_| !rejected_empty)
            {
                billable_attempt = true;
                if let Err(error) = settler.settle(&tokens) {
                    settlement_ok = false;
                    let frame = kiro_wire::encoder::encode_exception(
                        "InternalServerException",
                        "Billing settlement failed; request retained for reconciliation",
                    );
                    let _ = tx.send(Ok(Bytes::from(frame))).await;
                    eprintln!("[kiro-gateway] settlement failed: {error}");
                }
            }
            let status = if !settlement_ok {
                billing::observability::TraceStatus::Error
            } else if tx.is_closed() {
                billing::observability::TraceStatus::ClientAborted
            } else if completed {
                billing::observability::TraceStatus::Success
            } else {
                billing::observability::TraceStatus::Error
            };
            settler.billing.finish_trace(
                &settler.invocation_id,
                status,
                if !settlement_ok {
                    Some("settlement_failed")
                } else if !completed {
                    Some("stream_incomplete")
                } else {
                    None
                },
            );
        }
        if completed && settlement_ok && !tx.is_closed() {
            let frame = kiro_wire::encoder::encode_metadata(
                None,
                Some(stop_reason.as_deref().unwrap_or("end_turn")),
            );
            if tx.send(Ok(Bytes::from(frame))).await.is_ok() {
                if let Some(guard) = idempotency_guard.take() {
                    guard.commit(CompletedInvocation {
                        completed_at: Instant::now(),
                        model_id: config.model_id.clone(),
                        total_input_tokens: usage.prompt_tokens.min(u32::MAX as u64) as u32,
                        total_output_tokens: usage.completion_tokens.min(u32::MAX as u64) as u32,
                    });
                }
            }
        }
        if billable_attempt {
            if let Some(guard) = idempotency_guard.take() {
                guard.fail();
            }
        }
        // With no billable work, dropping the guard releases the invocation for retry.
    });
    FrameStream { inner: rx }
}

fn resolve_settlement_tokens(
    usage: &crate::provider::TokenUsage,
    has_input_usage: bool,
    output_chars: usize,
    saw_output: bool,
    estimated_input: u64,
) -> Option<UsageTokens> {
    if !has_input_usage && !saw_output && usage.completion_tokens == 0 {
        return None;
    }
    let output = if usage.output_tokens_final {
        usage.completion_tokens
    } else {
        usage.completion_tokens.max(if saw_output {
            (output_chars.saturating_add(3) / 4).max(1) as u64
        } else {
            0
        })
    };
    Some(UsageTokens {
        uncached_input_tokens: if has_input_usage {
            usage.uncached_prompt_tokens
        } else {
            estimated_input.max(1)
        },
        output_tokens: output,
        cache_creation_tokens: usage.cache_creation_input_tokens.unwrap_or(0),
        cache_read_tokens: usage.cache_read_input_tokens.unwrap_or(0),
    })
}

/// Provider error bodies commonly contain account identifiers, prompts, or
/// vendor-internal diagnostics.  Expose only a stable category/status to the
/// client; retain detailed text in server-side logs/telemetry.
pub(crate) fn safe_provider_error(error: &ProviderError) -> String {
    match error {
        ProviderError::Http(status, _) => format!("upstream HTTP status {}", status.as_u16()),
        ProviderError::Network(_) => "upstream network error".to_string(),
        ProviderError::Timeout => "upstream request timed out".to_string(),
        ProviderError::StreamDisconnected => "upstream stream disconnected".to_string(),
        ProviderError::Parse(_) => "upstream response parse error".to_string(),
        ProviderError::Service => "upstream reported a service error".to_string(),
        ProviderError::Serialization(_) => "upstream request serialization error".to_string(),
        ProviderError::Watchdog(_) => "upstream watchdog timeout".to_string(),
    }
}
