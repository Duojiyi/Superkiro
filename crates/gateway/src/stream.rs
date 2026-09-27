//! Stream orchestration, keepalive pinging, and client disconnect cancellation (Spec §4.6, §6.3).
//!
//! Bridges upstream provider event streams with Kiro IDE AWS EventStream binary frames.
//! Injects periodic empty `assistantResponseEvent` keepalive frames (every 20s) to reset
//! Kiro's 60s/300s watchdog, and detects downstream socket disconnects to cancel upstream
//! requests immediately without leaking resources or double-billing.

use crate::idempotency::{CompletedInvocation, IdempotencyGuard};
use crate::provider::governance::GovernanceError;
use crate::provider::{ProviderDelta, ProviderError, ProviderStreamEvent};
use crate::translate::{from_provider::StreamTranslationState, tools::ToolRegistry};
use billing::engine::{BillingEngine, BillingError};
use billing::ledger::UsageTokens;
use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use std::collections::HashMap;
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
    reservation_lease: Option<billing::engine::ReservationLease>,
    metrics: Option<crate::ops::metrics::RequestMetrics>,
    started_at: std::time::Instant,
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
            reservation_lease: None,
            metrics: None,
            started_at: std::time::Instant::now(),
        }
    }

    /// When the request arrived, so its time to first output counts all the customer waited.
    pub fn with_started_at(mut self, started_at: std::time::Instant) -> Self {
        self.started_at = started_at;
        self
    }

    pub fn with_reservation_lease(mut self, lease: billing::engine::ReservationLease) -> Self {
        self.reservation_lease = Some(lease);
        self
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

    /// Bill `tokens`; the micro-credits charged.
    pub fn settle(&mut self, tokens: &UsageTokens) -> Result<i64, BillingError> {
        if self.settled {
            return Ok(0);
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
        result.map(|entry| entry.credits_charged)
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
    /// Model context window size in tokens to drive contextUsageEvent percentage, 0 to 100
    /// (Spec §4.5).
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

const DEFAULT_SEND_DEADLINE: Duration = Duration::from_secs(600);

/// Set once, when the gateway is stopping and its drain is over.
static STOPPING: std::sync::OnceLock<tokio::sync::watch::Sender<bool>> = std::sync::OnceLock::new();

fn stopping() -> &'static tokio::sync::watch::Sender<bool> {
    STOPPING.get_or_init(|| tokio::sync::watch::channel(false).0)
}

/// End every open stream, and any that starts from now on, as a response cut off by a
/// restart, each billed for what it streamed. The gateway calls this when it stops: it
/// used to wait for every stream, which may run ten minutes, so it was killed with
/// streams still running, and their holds were dropped at the next start unbilled.
pub fn cut_open_streams() {
    stopping().send_replace(true);
}

/// How long a terminal frame may wait for the client, deadline or not. Without it Kiro
/// sees a response that simply stops, with no reason and no end.
const TERMINAL_FRAME_GRACE: Duration = Duration::from_secs(3);

/// Tool-call arguments one response may accumulate, across all its calls, and how many
/// calls it may open. A file-writing call legitimately carries hundreds of kilobytes.
pub(crate) const MAX_TOOL_ARGUMENT_BYTES: usize = 8 * 1024 * 1024;
const MAX_TOOL_CALLS: usize = 128;

async fn send_frame(
    tx: &tokio::sync::mpsc::Sender<Result<Bytes, std::io::Error>>,
    frame: Bytes,
    deadline: tokio::time::Instant,
) -> bool {
    // Do not enqueue a frame once the absolute request deadline has elapsed,
    // even when the bounded channel currently has capacity.
    if tokio::time::Instant::now() >= deadline {
        return false;
    }
    matches!(
        tokio::time::timeout_at(deadline, tx.send(Ok(frame))).await,
        Ok(Ok(()))
    )
}

/// Send a frame that ends the response, allowing [`TERMINAL_FRAME_GRACE`] even past
/// the request deadline.
async fn send_terminal_frame(
    tx: &tokio::sync::mpsc::Sender<Result<Bytes, std::io::Error>>,
    frame: Vec<u8>,
) -> bool {
    matches!(
        tokio::time::timeout(TERMINAL_FRAME_GRACE, tx.send(Ok(Bytes::from(frame)))).await,
        Ok(Ok(()))
    )
}

/// An error as Kiro is told it: as an error response, or, once the answer has begun, as the
/// exception frame that ends it, which Kiro reads the same way (the reason included).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KiroError {
    pub status: axum::http::StatusCode,
    pub exception: &'static str,
    pub message: String,
    pub reason: Option<&'static str>,
    pub retry_after_secs: Option<u64>,
}

impl KiroError {
    pub fn new(
        status: axum::http::StatusCode,
        exception: &'static str,
        message: impl Into<String>,
    ) -> Self {
        Self {
            status,
            exception,
            message: message.into(),
            reason: None,
            retry_after_secs: None,
        }
    }

    pub fn with_reason(mut self, reason: &'static str) -> Self {
        self.reason = Some(reason);
        self
    }

    pub fn with_retry_after(mut self, secs: u64) -> Self {
        self.retry_after_secs = Some(secs);
        self
    }

    /// The error response.
    pub fn into_response(self) -> axum::response::Response {
        match self.reason {
            Some(reason) => crate::guardrail::format_kiro_throttle_response(
                self.status,
                self.exception,
                reason,
                &self.message,
                self.retry_after_secs,
            ),
            None => crate::facade::error_response(self.status, self.exception, &self.message),
        }
    }
}

/// Why a response ended before completing, as the client is told at its end: by an
/// exception frame alone. Text added to the answer to explain it stayed in the conversation
/// the model reads from then on.
struct Failure {
    /// The exception that ends the stream.
    error: String,
    exception: &'static str,
    reason: Option<&'static str>,
    retry_after_secs: Option<u64>,
}

impl Failure {
    /// A temporary failure, which Kiro retries when nothing it cannot take back (a tool
    /// call) has been shown, and otherwise reports as a temporary error.
    fn new(error: impl Into<String>) -> Self {
        Self {
            error: error.into(),
            exception: "InternalServerException",
            reason: None,
            retry_after_secs: None,
        }
    }

    /// An ending a retry would meet again: a ValidationException without a reason, which
    /// Kiro shows as written and does not retry.
    fn shown(message: impl Into<String>) -> Self {
        Self {
            exception: "ValidationException",
            ..Self::new(message)
        }
    }

    /// A route that failed after the answer had begun, as the error response would have
    /// told Kiro.
    fn told(error: KiroError) -> Self {
        Self {
            error: error.message,
            exception: error.exception,
            reason: error.reason,
            retry_after_secs: error.retry_after_secs,
        }
    }

    fn deadline() -> Self {
        Self::shown(
            "响应超过了网关的时间上限，已被截断。可以让模型从中断处继续，或把任务拆小后重试。",
        )
    }

    /// The next gateway serves Kiro's retry.
    fn restarting() -> Self {
        Self::new("The gateway is restarting; the response was cut off")
    }

    fn oversized_tool_call(what: &str) -> Self {
        Self::shown(format!(
            "模型生成的工具调用超出了网关的上限（{what}），本次响应已中止。可以让模型分几步完成，或把任务拆小后重试。"
        ))
    }
}

#[derive(Default)]
struct ToolBuffer {
    id: String,
    name: String,
    arguments: String,
    /// Kiro has been told of the call, and of `sent` bytes of its arguments.
    announced: bool,
    sent: usize,
}

impl ToolBuffer {
    /// Whether its arguments are a whole JSON value, so the model has moved past it.
    fn complete(&self) -> bool {
        let arguments = self.arguments.trim();
        !arguments.is_empty() && serde_json::from_str::<serde::de::IgnoredAny>(arguments).is_ok()
    }
}

/// Tool calls being assembled, in the order they opened.
///
/// Their input streams to Kiro as it arrives, so it shows a file being written and a call
/// the model has begun, rather than the whole call once the response ends: held back, a
/// long write was minutes of silence, and a failure in it was retried by Kiro as if nothing
/// had been started. Kiro adds each fragment to the call it heard of last, so one call at a
/// time streams: a later call waits until the streaming one's arguments are whole JSON, or
/// until the response ends, which keeps calls apart however an upstream interleaves them.
#[derive(Default)]
struct ToolCalls {
    calls: Vec<ToolBuffer>,
    by_index: HashMap<usize, usize>,
    by_id: HashMap<String, usize>,
    /// The call streaming to Kiro.
    live: Option<usize>,
}

/// A fragment for a call Kiro was told had ended: it would be read as another call's.
struct ContinuedAfterEnd;

impl ToolCalls {
    /// The call a fragment belongs to: by its index when it has one, otherwise by its id,
    /// otherwise the latest call. `None` when it opens a new call.
    fn find(&self, index: Option<usize>, id: Option<&str>) -> Option<usize> {
        match (index, id) {
            (Some(index), _) => self.by_index.get(&index).copied(),
            (None, Some(id)) => self.by_id.get(id).copied(),
            (None, None) => self.calls.len().checked_sub(1),
        }
    }

    fn open(&mut self, index: Option<usize>) -> usize {
        self.calls.push(ToolBuffer::default());
        let position = self.calls.len() - 1;
        if let Some(index) = index {
            self.by_index.insert(index, position);
        }
        position
    }

    /// The frames that bring Kiro up to date after a fragment of call `position`.
    fn advance(&mut self, position: usize) -> Result<Vec<Vec<u8>>, ContinuedAfterEnd> {
        let mut frames = Vec::new();
        loop {
            match self.live {
                Some(live) if live == position => {
                    frames.extend(self.rest(live));
                    return Ok(frames);
                }
                Some(live) => {
                    if self.calls[position].announced {
                        return Err(ContinuedAfterEnd);
                    }
                    if !self.calls[live].complete() {
                        return Ok(frames);
                    }
                    frames.push(self.end(live));
                }
                None => {
                    let Some(next) = self.calls.iter().position(|call| {
                        !call.announced && !call.id.is_empty() && !call.name.is_empty()
                    }) else {
                        return Ok(frames);
                    };
                    let call = &mut self.calls[next];
                    call.announced = true;
                    call.sent = call.arguments.len();
                    frames.push(kiro_wire::encoder::encode_tool_use(
                        &call.name,
                        &call.id,
                        &call.arguments,
                        false,
                    ));
                    self.live = Some(next);
                }
            }
        }
    }

    /// The arguments of the streaming call Kiro has not seen yet.
    fn rest(&mut self, live: usize) -> Option<Vec<u8>> {
        let call = &mut self.calls[live];
        if call.sent >= call.arguments.len() {
            return None;
        }
        let frame = kiro_wire::encoder::encode_tool_use(
            &call.name,
            &call.id,
            &call.arguments[call.sent..],
            false,
        );
        call.sent = call.arguments.len();
        Some(frame)
    }

    fn end(&mut self, live: usize) -> Vec<u8> {
        self.live = None;
        let call = &self.calls[live];
        kiro_wire::encoder::encode_tool_use(&call.name, &call.id, "", true)
    }

    /// The frames that end the response's calls: the streaming one ends, and any that never
    /// streamed go whole, as before.
    fn finish(&mut self) -> Vec<Vec<u8>> {
        let mut frames: Vec<Vec<u8>> = self.live.map(|live| self.end(live)).into_iter().collect();
        frames.extend(
            self.calls
                .iter()
                .filter(|call| !call.announced && (!call.name.is_empty() || !call.id.is_empty()))
                .map(|call| {
                    kiro_wire::encoder::encode_tool_use(&call.name, &call.id, &call.arguments, true)
                }),
        );
        frames
    }
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
    idempotency_guard: Option<IdempotencyGuard>,
    billing_settler: Option<BillingSettler>,
) -> FrameStream {
    create_stream_guard_with_send_deadline(
        upstream_stream,
        config,
        tool_registry,
        idempotency_guard,
        billing_settler,
        DEFAULT_SEND_DEADLINE,
    )
}

/// Testable variant of [`create_stream_guard`] with a shorter absolute send deadline.
/// Production callers should use [`create_stream_guard`], which uses the 10-minute
/// request ceiling shared with the upstream watchdog.
pub fn create_stream_guard_with_send_deadline(
    upstream_stream: impl Stream<Item = Result<ProviderStreamEvent, ProviderError>> + Send + 'static,
    config: StreamGuardConfig,
    tool_registry: Option<ToolRegistry>,
    mut idempotency_guard: Option<IdempotencyGuard>,
    mut billing_settler: Option<BillingSettler>,
    send_timeout: Duration,
) -> FrameStream {
    let (tx, rx) = tokio::sync::mpsc::channel(64);
    let send_deadline = tokio::time::Instant::now() + send_timeout;

    tokio::spawn(async move {
        // Keepalives fill any silence the client sees. Only a frame sent restarts the
        // clock: a tool call waiting for the one streaming before it sends nothing.
        let mut interval = tokio::time::interval(config.keepalive_interval);
        let deadline_sleep = tokio::time::sleep_until(send_deadline);
        tokio::pin!(deadline_sleep);
        let mut stop = stopping().subscribe();
        interval.tick().await;
        let mut upstream = Box::pin(upstream_stream);
        let mut tool_calls = ToolCalls::default();
        let mut tool_argument_bytes = 0usize;
        let mut failure: Option<Failure> = None;
        let mut usage = crate::provider::TokenUsage::default();
        let mut has_input_usage = false;
        let mut saw_usage_frame = false;
        let mut output_units = 0u64;
        let mut saw_output = false;
        // Text or a tool call: output that answers, which thinking alone does not.
        let mut saw_answer = false;
        let mut first_output_at: Option<std::time::Instant> = None;
        // Kept 24 hours for tracing when the archive is on: what the model sent back.
        let archiving = crate::archive::active().is_some();
        let mut reply = crate::archive::ArchivedReply::default();
        let mut completed = false;
        let mut rejected_empty = false;
        let mut empty_turn = false;
        let context_window = config
            .context_window
            .unwrap_or_else(|| {
                billing::context::ModelContextLibrary::resolve(&config.model_id).context_window
            })
            .max(1);
        let mut stop_reason = None;
        let mut refusal = None;
        // The upstream model the response comes from, which its thinking may go back to.
        let mut signing_model = billing_settler
            .as_ref()
            .map(|settler| settler.target_model.clone());
        // What the gateway estimates the request holds, which the context Kiro is told of
        // never falls below.
        let estimated_input = billing_settler
            .as_ref()
            .map_or(0, |settler| settler.estimated_input_tokens);

        'stream: loop {
            tokio::select! {
                _ = tx.closed() => break,
                _ = &mut deadline_sleep => break,
                _ = async { drop(stop.wait_for(|stop| *stop).await) } => {
                    failure = Some(Failure::restarting());
                    break;
                }
                _ = interval.tick() => {
                    if !send_frame(&tx, Bytes::from(kiro_wire::encoder::encode_keepalive()), send_deadline).await {
                        break;
                    }
                }
                event = upstream.next() => {
                    match event {
                        Some(Ok(ProviderStreamEvent::Delta(delta))) => {
                            let frames = match delta {
                                ProviderDelta::Text(text) => {
                                    saw_output |= !text.is_empty();
                                    saw_answer |= !text.is_empty();
                                    output_units = output_units.saturating_add(crate::usage_estimate::token_units(&text));
                                    if archiving {
                                        reply.truncated |= crate::archive::append_capped(&mut reply.text, &text);
                                    }
                                    vec![kiro_wire::encoder::encode_assistant_response(&text, Some(&config.model_id))]
                                }
                                ProviderDelta::Reasoning(text) => {
                                    saw_output |= !text.is_empty();
                                    output_units = output_units.saturating_add(crate::usage_estimate::token_units(&text));
                                    if archiving {
                                        reply.truncated |= crate::archive::append_capped(&mut reply.reasoning, &text);
                                    }
                                    vec![kiro_wire::encoder::encode_reasoning(Some(&text), None, None)]
                                }
                                // Kiro keeps a thinking block in its history only with a signature,
                                // sent before the text or tool call after it; tagged, it goes back
                                // to the model that wrote it alone.
                                ProviderDelta::ReasoningSignature(signature) => {
                                    let signature = match &signing_model {
                                        Some(model) => crate::provider::tag_signature(model, &signature),
                                        None => signature,
                                    };
                                    vec![kiro_wire::encoder::encode_reasoning(None, Some(&signature), None)]
                                }
                                ProviderDelta::ToolCallChunk { index, id, name, arguments } => {
                                    // An empty id or name on a continuation names nothing; it
                                    // must not blank the call it continues.
                                    let id = id.filter(|value| !value.is_empty());
                                    let name = name.filter(|value| !value.is_empty());
                                    saw_output |= !arguments.is_empty() || id.is_some() || name.is_some();
                                    saw_answer |= !arguments.is_empty() || id.is_some() || name.is_some();
                                    output_units = output_units.saturating_add(crate::usage_estimate::token_units(&arguments))
                                        .saturating_add(name.as_ref().map_or(0, |n| crate::usage_estimate::token_units(n)));
                                    // Truncated arguments would reach Kiro as a malformed tool
                                    // call, so a response over the limits ends instead, billed
                                    // for what it produced.
                                    let position = tool_calls.find(index, id.as_deref());
                                    if position.is_none() && tool_calls.calls.len() >= MAX_TOOL_CALLS {
                                        failure = Some(Failure::oversized_tool_call(&format!(
                                            "一次最多 {MAX_TOOL_CALLS} 个调用"
                                        )));
                                        break 'stream;
                                    }
                                    tool_argument_bytes = tool_argument_bytes.saturating_add(arguments.len());
                                    if tool_argument_bytes > MAX_TOOL_ARGUMENT_BYTES {
                                        failure = Some(Failure::oversized_tool_call(&format!(
                                            "参数合计最多 {} MB",
                                            MAX_TOOL_ARGUMENT_BYTES / (1024 * 1024)
                                        )));
                                        break 'stream;
                                    }
                                    let position = position.unwrap_or_else(|| tool_calls.open(index));
                                    if let Some(id) = id {
                                        tool_calls.by_id.entry(id.clone()).or_insert(position);
                                        tool_calls.calls[position].id = id;
                                    }
                                    let buf = &mut tool_calls.calls[position];
                                    if let Some(name) = name {
                                        buf.name = tool_registry.as_ref().map_or_else(|| name.clone(), |r| r.restore(&name));
                                    }
                                    buf.arguments.push_str(&arguments);
                                    match tool_calls.advance(position) {
                                        Ok(frames) => frames,
                                        Err(ContinuedAfterEnd) => {
                                            failure = Some(Failure::new(
                                                "Upstream continued a tool call after starting the next one",
                                            ));
                                            break 'stream;
                                        }
                                    }
                                }
                            };
                            if saw_output && first_output_at.is_none() {
                                first_output_at = Some(std::time::Instant::now());
                            }
                            if !frames.is_empty() {
                                for frame in frames {
                                    if !send_frame(&tx, Bytes::from(frame), send_deadline).await { break 'stream; }
                                }
                                interval.reset();
                            }
                        }
                        // Liveness for the watchdog; nothing for Kiro, which keepalives serve.
                        Some(Ok(ProviderStreamEvent::Started | ProviderStreamEvent::Heartbeat)) => {}
                        // Another attempt took over before any content: it is the one billed,
                        // and the one its thinking goes back to.
                        Some(Ok(ProviderStreamEvent::Served { provider_id, target_model })) => {
                            if let Some(settler) = billing_settler.as_mut() {
                                settler.provider_id = provider_id;
                                settler.target_model = target_model;
                            }
                            signing_model = billing_settler
                                .as_ref()
                                .map(|settler| settler.target_model.clone());
                        }
                        // The route ended without an answer after the answer had begun.
                        Some(Ok(ProviderStreamEvent::Failed(error))) => {
                            failure = Some(Failure::told(error));
                            break;
                        }
                        Some(Ok(ProviderStreamEvent::Refusal { category, explanation })) => {
                            refusal = Some(kiro_wire::events::Refusal {
                                category,
                                explanation,
                                recommended_model: None,
                            });
                        }
                        Some(Ok(ProviderStreamEvent::Usage(next))) => {
                            saw_usage_frame = true;
                            // Output-only Anthropic message_delta must not erase prompt/cache observations.
                            has_input_usage |= reports_input(&next);
                            usage.merge(&next);
                            let wire_usage = kiro_wire::events::TokenUsage {
                                uncached_input_tokens: usage.uncached_prompt_tokens.min(i64::MAX as u64) as i64,
                                output_tokens: usage.completion_tokens.min(i64::MAX as u64) as i64,
                                cache_read_input_tokens: usage.cache_read_input_tokens.unwrap_or(0).min(i64::MAX as u64) as i64,
                                cache_write_input_tokens: usage.cache_creation_input_tokens.unwrap_or(0).min(i64::MAX as u64) as i64,
                            };
                            // Kiro reads a percentage: it summarizes the conversation at 80 and
                            // truncates it at 95. A fraction never reached either. An upstream
                            // that leaves the system prompt and tool definitions out of its
                            // usage (kimera-primary reports 3 input tokens for a 9K-token system
                            // prompt) would have Kiro compact late and then overflow, so the
                            // gateway's own estimate is the floor. Billing still follows the
                            // upstream's report.
                            let context_tokens = usage.total_tokens.max(estimated_input.saturating_add(
                                crate::usage_estimate::tokens_from_units(output_units),
                            ));
                            let frames = [
                                kiro_wire::encoder::encode_metadata(Some(wire_usage), None),
                                kiro_wire::encoder::encode_context_usage((context_tokens as f64 * 100.0 / context_window as f64).clamp(0.0, 100.0)),
                            ];
                            for frame in frames {
                                if !send_frame(&tx, Bytes::from(frame), send_deadline).await { break 'stream; }
                            }
                            interval.reset();
                        }
                        Some(Ok(ProviderStreamEvent::StopReason(reason))) => {
                            stop_reason = Some(StreamTranslationState::map_stop_reason(&reason));
                        }
                        Some(Ok(ProviderStreamEvent::Done)) => {
                            // An empty turn with an explicit reason — the output limit, or a
                            // content filter — was reported in full, so it ends like any other
                            // and bills its usage; out of output room, it says why it is empty
                            // (Kiro shows its own refusal for a filter). An empty turn ending
                            // with an ordinary stop looks exactly like a failed relay and gives
                            // the user nothing, so it stays an unbilled, retryable error below.
                            if !saw_output && saw_usage_frame && stop_reason.as_deref().is_some_and(ends_empty_turn) {
                                if let Some(notice) = empty_turn_notice(stop_reason.as_deref()) {
                                    let frame = kiro_wire::encoder::encode_assistant_response(notice, Some(&config.model_id));
                                    if !send_frame(&tx, Bytes::from(frame), send_deadline).await { break 'stream; }
                                }
                                completed = true;
                                break;
                            }
                            // A terminal marker alone does not constitute a response, and is
                            // never cached as a successful invocation. The model had started,
                            // so it is not retried here. When the upstream reported the input
                            // it read, that input is billed, as for an empty attempt before
                            // the start, and the invocation is not replayed for free; without
                            // a report it looks like a failed relay and stays unbilled.
                            if !saw_output {
                                rejected_empty = !has_input_usage;
                                empty_turn = true;
                                failure = Some(Failure::new(
                                    if has_input_usage {
                                        "The upstream model returned an empty response; the input it read has been billed"
                                    } else {
                                        "Upstream completed without producing any output"
                                    },
                                ));
                                break;
                            }
                            // Thinking that used up the output limit left no answer: said so,
                            // or the turn ends on its thinking alone and Kiro completes it.
                            if !saw_answer {
                                if let Some(notice) = empty_turn_notice(stop_reason.as_deref()) {
                                    let frame = kiro_wire::encoder::encode_assistant_response(notice, Some(&config.model_id));
                                    if !send_frame(&tx, Bytes::from(frame), send_deadline).await { break 'stream; }
                                }
                            }
                            for frame in tool_calls.finish() {
                                if !send_frame(&tx, Bytes::from(frame), send_deadline).await { break 'stream; }
                            }
                            if archiving {
                                for buf in std::mem::take(&mut tool_calls.calls) {
                                    reply.truncated |= archive_tool_call(&mut reply, &buf);
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
                            // The exception ends the turn; text in the answer would stay in the
                            // conversation the model reads from then on.
                            failure = Some(Failure::new(error));
                            break;
                        }
                    }
                }
            }
        }
        // One finalization path for Done, EOF, provider errors, cancellation and failed sends.
        // Dropping this receiver cancels the provider pump even while it waits for bytes.
        drop(upstream);
        // Every other way out that leaves the client connected is the deadline: it fired,
        // or a send was refused because it had passed.
        if !completed && failure.is_none() && !tx.is_closed() {
            failure = Some(Failure::deadline());
        }
        let mut settlement_ok = true;
        let mut billable_attempt = false;
        let mut credits_charged = None;
        // A stream cut short never completed its calls; they still show what they carried.
        if archiving {
            for buf in std::mem::take(&mut tool_calls.calls) {
                reply.truncated |= archive_tool_call(&mut reply, &buf);
            }
        }
        if let Some(mut settler) = billing_settler.take() {
            if !completed {
                if let Some(metrics) = &settler.metrics {
                    metrics.mark_error();
                }
            }
            // The only way to tune the estimator: what it said, against what was billed.
            if has_input_usage {
                eprintln!(
                    "usage_calibration model={} estimated_input={} uncached_input={} cache_read={} cache_write={}",
                    settler.target_model,
                    settler.estimated_input_tokens,
                    usage.uncached_prompt_tokens,
                    usage.cache_read_input_tokens.unwrap_or(0),
                    usage.cache_creation_input_tokens.unwrap_or(0),
                );
            }
            let resolved = resolve_settlement_tokens(
                &usage,
                has_input_usage,
                output_units,
                saw_output,
                settler.estimated_input_tokens,
            );
            let output_tokens = resolved.as_ref().map_or(0, |tokens| tokens.output_tokens);
            // What delivered nothing is never billed, whatever the upstream reported: Kiro
            // sends its retry of a failed turn as a new request, so a bill for the failure
            // would be one more. An answer that genuinely ended empty, with a stop reason,
            // consumed its input and is billed.
            let billable = saw_output || completed || (empty_turn && stop_reason.is_some());
            if let Some(tokens) = resolved.filter(|_| billable && !rejected_empty) {
                billable_attempt = true;
                match settler.settle(&tokens) {
                    Ok(charged) => credits_charged = Some(charged),
                    Err(error) => {
                        settlement_ok = false;
                        eprintln!("[kiro-gateway] settlement failed: {error}");
                    }
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
            if archiving {
                reply.status = match status {
                    billing::observability::TraceStatus::Success => "success",
                    billing::observability::TraceStatus::ClientAborted => "client_aborted",
                    _ => "error",
                }
                .into();
                reply.error = failure
                    .as_ref()
                    .map(|failure| failure.error.clone())
                    .or_else(|| (!settlement_ok).then(|| "Billing settlement failed".to_string()));
                reply.provider_id = settler.provider_id.clone();
                reply.target_model = settler.target_model.clone();
                reply.stop_reason = stop_reason.clone();
                reply.input_tokens = usage.prompt_tokens;
                reply.output_tokens = output_tokens;
                reply.cache_read_tokens = usage.cache_read_input_tokens.unwrap_or(0);
                reply.cache_write_tokens = usage.cache_creation_input_tokens.unwrap_or(0);
            }
            settler.billing.finish_trace(
                &settler.invocation_id,
                status,
                if !settlement_ok {
                    Some("settlement_failed")
                } else if empty_turn {
                    Some("empty_completion")
                } else if !completed {
                    Some("stream_incomplete")
                } else {
                    None
                },
            );
            let (ttft_ms, tokens_per_second) =
                stream_timing(settler.started_at, first_output_at, output_tokens);
            settler
                .billing
                .note_trace_timing(&settler.invocation_id, ttft_ms, tokens_per_second);
            if let Some(archive) = crate::archive::active().filter(|_| archiving) {
                reply.ttft_ms = ttft_ms;
                reply.tokens_per_second = tokens_per_second;
                archive.keep_reply(&settler.invocation_id, std::mem::take(&mut reply));
            }
        }
        if completed && settlement_ok {
            // What the turn cost, which Kiro shows as the prompt's usage summary, in the unit
            // its model list shows beside each model.
            if let Some(charged) = credits_charged.filter(|_| !tx.is_closed()) {
                let frame = kiro_wire::encoder::encode_metering(
                    charged as f64 / billing::MICRO_CREDITS_PER_CREDIT as f64,
                    crate::facade::models::RATE_UNIT,
                    crate::facade::models::RATE_UNIT_PLURAL,
                );
                let _ = send_terminal_frame(&tx, frame).await;
            }
            if !tx.is_closed() {
                // A refusal says why, and Kiro shows it with its refusal.
                let stop = stop_reason.as_deref().unwrap_or("end_turn");
                let details = refusal
                    .filter(|_| stop == "content_filtered")
                    .map(|refusal| kiro_wire::events::StopDetails {
                        refusal: Some(refusal),
                    });
                let frame = kiro_wire::encoder::encode_stop(None, Some(stop), details);
                if send_terminal_frame(&tx, frame).await {
                    if let Some(guard) = idempotency_guard.take() {
                        guard.commit(CompletedInvocation {
                            completed_at: Instant::now(),
                            model_id: config.model_id.clone(),
                            total_input_tokens: usage.prompt_tokens.min(u32::MAX as u64) as u32,
                            total_output_tokens: usage.completion_tokens.min(u32::MAX as u64)
                                as u32,
                        });
                    }
                }
            }
            if billable_attempt {
                if let Some(guard) = idempotency_guard.take() {
                    guard.fail();
                }
            }
            return;
        }
        // Record the outcome before telling a client that may be slow to read: billable
        // work is never replayed, and anything else is released for an immediate retry.
        match idempotency_guard.take() {
            Some(guard) if billable_attempt => guard.fail(),
            released => drop(released),
        }
        let failure = match failure {
            Some(failure) => failure,
            None if !settlement_ok => {
                Failure::new("Billing settlement failed; request retained for reconciliation")
            }
            // The client left; there is no one to tell.
            None => return,
        };
        let frame = kiro_wire::encoder::encode_exception_with(
            failure.exception,
            &failure.error,
            failure.reason,
            failure
                .retry_after_secs
                .map(|secs| secs.saturating_mul(1000)),
        );
        let _ = send_terminal_frame(&tx, frame).await;
    });
    FrameStream { inner: rx }
}

/// What to show when a turn ends without an answer, neither text nor a tool call, so it is
/// not simply blank. Stop reasons arrive mapped to Kiro's vocabulary. A content filter has
/// none: Kiro shows its own refusal for `content_filtered`, and a second message beside it
/// stayed in the conversation.
fn empty_turn_notice(stop_reason: Option<&str>) -> Option<&'static str> {
    match stop_reason? {
        "max_tokens" => Some("（模型在给出可见回答前已达到输出上限。请重试，或缩短本次请求。）"),
        _ => None,
    }
}

/// Whether a stop reason, in Kiro's vocabulary, makes a response with no output final: the
/// output limit, or a content filter.
fn ends_empty_turn(stop_reason: &str) -> bool {
    matches!(stop_reason, "max_tokens" | "content_filtered")
}

/// Whether a provider stop reason makes an empty response final: it was reported in full,
/// so it is billed rather than retried. `retry.rs` and the stream agree on this through
/// here.
pub(crate) fn is_explicit_empty_stop(raw_reason: &str) -> bool {
    ends_empty_turn(&StreamTranslationState::map_stop_reason(raw_reason))
}

/// Whether a usage report counts the input. A present-but-zero cache field does not: the
/// OpenAI adapter fills it from `cached_tokens`, which is usually 0.
pub(crate) fn reports_input(usage: &crate::provider::TokenUsage) -> bool {
    usage.prompt_tokens > 0
        || usage.uncached_prompt_tokens > 0
        || usage
            .cache_read_input_tokens
            .is_some_and(|tokens| tokens > 0)
        || usage
            .cache_creation_input_tokens
            .is_some_and(|tokens| tokens > 0)
}

/// Adds a tool call to the kept reply; true when its arguments were longer than is kept.
fn archive_tool_call(reply: &mut crate::archive::ArchivedReply, buf: &ToolBuffer) -> bool {
    if buf.name.is_empty() && buf.id.is_empty() {
        return false;
    }
    let mut arguments = String::new();
    let cut = crate::archive::append_capped(&mut arguments, &buf.arguments);
    reply.tool_calls.push(crate::archive::ArchivedToolCall {
        id: buf.id.clone(),
        name: buf.name.clone(),
        arguments,
    });
    cut
}

/// Time to first output since the request arrived, and output speed after it.
fn stream_timing(
    started_at: std::time::Instant,
    first_output_at: Option<std::time::Instant>,
    output_tokens: u64,
) -> (Option<u32>, Option<f64>) {
    let Some(first) = first_output_at else {
        return (None, None);
    };
    let ttft = first.saturating_duration_since(started_at).as_millis();
    let generating = first.elapsed().as_secs_f64();
    let speed = (output_tokens > 0 && generating > 0.0).then(|| output_tokens as f64 / generating);
    (Some(ttft.min(u32::MAX as u128) as u32), speed)
}

pub(crate) fn resolve_settlement_tokens(
    usage: &crate::provider::TokenUsage,
    has_input_usage: bool,
    output_units: u64,
    saw_output: bool,
    estimated_input: u64,
) -> Option<UsageTokens> {
    if !has_input_usage && !saw_output && usage.completion_tokens == 0 {
        return None;
    }
    let streamed = if saw_output {
        crate::usage_estimate::tokens_from_units(output_units).max(1)
    } else {
        0
    };
    // An exact report wins, unless it is below half of what was streamed. The estimate
    // is within that of a real tokenizer (it overcounts CJK and deep indentation at
    // most about twofold), so such a report is wrong: an upstream that sent a usage
    // frame of zeros, or an Anthropic-format relay whose final count of 0 left its
    // opening count of 1, which billed thousands of streamed words as one token.
    let output =
        if usage.output_tokens_final && usage.completion_tokens.saturating_mul(2) >= streamed {
            usage.completion_tokens
        } else {
            usage.completion_tokens.max(streamed)
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
/// Client-facing text for a failover error.
///
/// `GovernanceError`'s `Display` embeds internal key ids, provider ids and the
/// upstream vendor's own response body, so it must never be formatted into a
/// response. Keep what the caller can act on - how long to wait, how many
/// candidates were tried - and drop the rest.
pub(crate) fn safe_governance_error(error: &GovernanceError) -> String {
    match error {
        GovernanceError::AllKeysInCooldown {
            next_recovery_secs, ..
        } => format!("all upstream keys are in cooldown; retry in {next_recovery_secs}s"),
        GovernanceError::NoAvailableKeys { .. } => {
            "no upstream key is available for this model".to_string()
        }
        GovernanceError::AllCandidatesExhausted => {
            "all upstream candidates were exhausted".to_string()
        }
        GovernanceError::AllCandidatesFailed { attempts } => {
            format!("all {} upstream candidates failed", attempts.len())
        }
        GovernanceError::NonRetryable(e) | GovernanceError::Provider(e) => safe_provider_error(e),
    }
}

pub(crate) fn safe_provider_error(error: &ProviderError) -> String {
    match error {
        ProviderError::Http(status, _) => format!("upstream HTTP status {}", status.as_u16()),
        ProviderError::Network(_) => "upstream network error".to_string(),
        ProviderError::Timeout => "upstream request timed out".to_string(),
        ProviderError::NoAnswer => "upstream did not answer".to_string(),
        ProviderError::StreamDisconnected => "upstream stream disconnected".to_string(),
        ProviderError::Parse(_) => "upstream response parse error".to_string(),
        ProviderError::Unavailable => "upstream reported a temporary failure".to_string(),
        ProviderError::Serialization(_) => "upstream request serialization error".to_string(),
        ProviderError::Watchdog(_) => "upstream watchdog timeout".to_string(),
        ProviderError::EmptyCompletion => {
            "upstream completed without producing any output".to_string()
        }
    }
}
