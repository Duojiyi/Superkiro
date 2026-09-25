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

/// Why a response ended before completing, as the client is told at its end.
struct Failure {
    /// Shown in the conversation, where Kiro renders it.
    note: Option<String>,
    /// The exception that ends the stream.
    error: String,
}

impl Failure {
    fn new(note: Option<String>, error: impl Into<String>) -> Self {
        Self {
            note,
            error: error.into(),
        }
    }

    fn deadline() -> Self {
        Self::new(
            Some(
                "\n\n**响应超过时间上限，已被截断。**可以让模型从这里继续，或缩短本次请求。\n"
                    .into(),
            ),
            "Response exceeded the time limit and was cut off",
        )
    }

    fn restarting() -> Self {
        Self::new(
            Some("\n\n**网关正在重启，本次响应已被截断。**可以让模型从这里继续。\n".into()),
            "The gateway is restarting; the response was cut off",
        )
    }

    fn oversized_tool_call(error: &str) -> Self {
        Self::new(
            Some("\n\n**模型生成的工具调用超出网关上限，本次响应已中止。**\n".into()),
            error,
        )
    }
}

#[derive(Default)]
struct ToolBuffer {
    id: String,
    name: String,
    arguments: String,
}

/// Tool calls being assembled, in the order they opened.
#[derive(Default)]
struct ToolCalls {
    calls: Vec<ToolBuffer>,
    by_index: HashMap<usize, usize>,
    by_id: HashMap<String, usize>,
}

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
        // clock: the fragments of a tool call arrive for minutes but are forwarded only
        // once it is complete.
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
        let mut first_output_at: Option<std::time::Instant> = None;
        // Kept 24 hours for tracing when the archive is on: what the model sent back.
        let archiving = crate::archive::active().is_some();
        let mut reply = crate::archive::ArchivedReply::default();
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
                            let frame = match delta {
                                ProviderDelta::Text(text) => {
                                    saw_output |= !text.is_empty();
                                    output_units = output_units.saturating_add(crate::usage_estimate::token_units(&text));
                                    if archiving {
                                        reply.truncated |= crate::archive::append_capped(&mut reply.text, &text);
                                    }
                                    Some(kiro_wire::encoder::encode_assistant_response(&text, Some(&config.model_id)))
                                }
                                ProviderDelta::Reasoning(text) => {
                                    saw_output |= !text.is_empty();
                                    output_units = output_units.saturating_add(crate::usage_estimate::token_units(&text));
                                    if archiving {
                                        reply.truncated |= crate::archive::append_capped(&mut reply.reasoning, &text);
                                    }
                                    Some(kiro_wire::encoder::encode_reasoning(Some(&text), None, None))
                                }
                                ProviderDelta::ToolCallChunk { index, id, name, arguments } => {
                                    // An empty id or name on a continuation names nothing; it
                                    // must not blank the call it continues.
                                    let id = id.filter(|value| !value.is_empty());
                                    let name = name.filter(|value| !value.is_empty());
                                    saw_output |= !arguments.is_empty() || id.is_some() || name.is_some();
                                    output_units = output_units.saturating_add(crate::usage_estimate::token_units(&arguments))
                                        .saturating_add(name.as_ref().map_or(0, |n| crate::usage_estimate::token_units(n)));
                                    // Truncated arguments would reach Kiro as a malformed tool
                                    // call, so a response over the limits ends instead, billed
                                    // for what it produced.
                                    let position = tool_calls.find(index, id.as_deref());
                                    if position.is_none() && tool_calls.calls.len() >= MAX_TOOL_CALLS {
                                        failure = Some(Failure::oversized_tool_call(
                                            "Response opened more tool calls than the gateway accepts",
                                        ));
                                        break 'stream;
                                    }
                                    tool_argument_bytes = tool_argument_bytes.saturating_add(arguments.len());
                                    if tool_argument_bytes > MAX_TOOL_ARGUMENT_BYTES {
                                        failure = Some(Failure::oversized_tool_call(
                                            "Tool call arguments exceeded the gateway limit",
                                        ));
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
                                    None
                                }
                            };
                            if saw_output && first_output_at.is_none() {
                                first_output_at = Some(std::time::Instant::now());
                            }
                            if let Some(frame) = frame {
                                if !send_frame(&tx, Bytes::from(frame), send_deadline).await { break; }
                                interval.reset();
                            }
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
                            let frames = [
                                kiro_wire::encoder::encode_metadata(Some(wire_usage), None),
                                kiro_wire::encoder::encode_context_usage((usage.total_tokens as f64 / context_window as f64).clamp(0.0, 1.0)),
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
                            // content filter — was reported in full, so it ends like any other,
                            // bills its usage, and says why it is empty. An empty turn ending
                            // with an ordinary stop looks exactly like a failed relay and gives
                            // the user nothing, so it stays an unbilled, retryable error below.
                            if !saw_output && saw_usage_frame {
                                if let Some(notice) = empty_turn_notice(stop_reason.as_deref()) {
                                    let frame = kiro_wire::encoder::encode_assistant_response(notice, Some(&config.model_id));
                                    if !send_frame(&tx, Bytes::from(frame), send_deadline).await { break 'stream; }
                                    completed = true;
                                    break;
                                }
                            }
                            // A terminal marker alone does not constitute a response. Do not
                            // charge input-only usage or cache this invocation as successful.
                            if !saw_output {
                                rejected_empty = true;
                                failure = Some(Failure::new(None, "Upstream completed without producing any output"));
                                break;
                            }
                            for buf in std::mem::take(&mut tool_calls.calls) {
                                if archiving {
                                    reply.truncated |= archive_tool_call(&mut reply, &buf);
                                }
                                if !buf.name.is_empty() || !buf.id.is_empty() {
                                    let frame = kiro_wire::encoder::encode_tool_use(&buf.name, &buf.id, &buf.arguments, true);
                                    if !send_frame(&tx, Bytes::from(frame), send_deadline).await { break 'stream; }
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
                            failure = Some(Failure::new(Some(friendly), error));
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
            if let Some(tokens) = resolved.filter(|_| !rejected_empty) {
                billable_attempt = true;
                if let Err(error) = settler.settle(&tokens) {
                    settlement_ok = false;
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
            if !tx.is_closed() {
                let frame = kiro_wire::encoder::encode_metadata(
                    None,
                    Some(stop_reason.as_deref().unwrap_or("end_turn")),
                );
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
            None if !settlement_ok => Failure::new(
                None,
                "Billing settlement failed; request retained for reconciliation",
            ),
            // The client left; there is no one to tell.
            None => return,
        };
        if let Some(note) = failure.note {
            let frame =
                kiro_wire::encoder::encode_assistant_response(&note, Some(&config.model_id));
            if !send_terminal_frame(&tx, frame).await {
                return;
            }
        }
        let frame = kiro_wire::encoder::encode_exception("InternalServerException", &failure.error);
        let _ = send_terminal_frame(&tx, frame).await;
    });
    FrameStream { inner: rx }
}

/// What to show when a turn ends with no visible output, so it is not simply blank.
/// Stop reasons arrive in each provider's own vocabulary; only the output limit is
/// normalised before this point.
fn empty_turn_notice(stop_reason: Option<&str>) -> Option<&'static str> {
    match stop_reason? {
        "max_tokens" => Some("（模型在给出可见回答前已达到输出上限。请重试，或缩短本次请求。）"),
        "content_filter" | "refusal" => {
            Some("（上游模型未返回内容：本次请求被其内容安全策略拦截。）")
        }
        _ => None,
    }
}

/// Whether a provider stop reason makes an empty response final: it was reported in full,
/// so it is billed and explained rather than retried. `retry.rs` and the stream agree on
/// this through here.
pub(crate) fn is_explicit_empty_stop(raw_reason: &str) -> bool {
    empty_turn_notice(Some(&StreamTranslationState::map_stop_reason(raw_reason))).is_some()
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
        ProviderError::StreamDisconnected => "upstream stream disconnected".to_string(),
        ProviderError::Parse(_) => "upstream response parse error".to_string(),
        ProviderError::Service => "upstream reported a service error".to_string(),
        ProviderError::Serialization(_) => "upstream request serialization error".to_string(),
        ProviderError::Watchdog(_) => "upstream watchdog timeout".to_string(),
        ProviderError::EmptyCompletion => {
            "upstream completed without producing any output".to_string()
        }
    }
}
