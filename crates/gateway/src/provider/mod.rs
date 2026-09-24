//! Model provider abstraction for upstream LLM services (Spec §15.2).
//!
//! Defines uniform traits for translating requests, parsing SSE streams,
//! and extracting token usage metrics across different AI providers.

use futures_util::{Stream, StreamExt};
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;
use thiserror::Error;

pub mod anthropic;
pub mod governance;
pub mod import;
pub mod openai;
pub mod retry;
pub mod runtime;

/// Join a provider base URL to a versioned endpoint without duplicating an API prefix.
pub(crate) fn endpoint_with_suffix(base_url: &str, default_prefix: &str, suffix: &str) -> String {
    let base = base_url.trim_end_matches('/');
    if base.ends_with(suffix) {
        // Allow callers to provide a provider's complete custom endpoint, such
        // as a vendor-specific compatible path, without appending it twice.
        base.to_string()
    } else if base.ends_with(default_prefix) {
        format!("{base}{suffix}")
    } else {
        format!("{base}{default_prefix}{suffix}")
    }
}

pub use governance::{
    execute_stream_with_failover, execute_stream_with_model_fallback, is_cooldown_error,
    probe_provider_key, GovernanceError, ModelFallbackResult, ProbeBenchmarkResult,
    ProviderKeyPool,
};
pub use import::{import_providers, ImportError, ImportedProvider, SourceFormat};
pub use runtime::ProviderRuntimeRegistry;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
pub type BoxStream<'a, T> = Pin<Box<dyn Stream<Item = T> + Send + 'a>>;

#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("Upstream HTTP error {0}: {1}")]
    Http(reqwest::StatusCode, String),

    #[error("Network error connecting to provider: {0}")]
    Network(String),

    #[error("Upstream request timed out")]
    Timeout,

    #[error("Stream disconnected prematurely")]
    StreamDisconnected,

    #[error("Failed to parse provider response or stream chunk: {0}")]
    Parse(String),

    #[error("Upstream reported a service error")]
    Service,

    #[error("Serialization error: {0}")]
    Serialization(String),

    #[error("Upstream watchdog triggered: {0}")]
    Watchdog(#[from] crate::watchdog::WatchdogError),

    /// The upstream ended normally with nothing in it and no reason (such as the output
    /// limit or a content filter) that makes an empty answer final. A failing relay sends the
    /// same thing. The key did answer, so this is retried without cooling the key down.
    #[error("Upstream completed without producing any output")]
    EmptyCompletion,
}

/// Bound and normalize vendor error bodies before they enter logs or error
/// values.  Upstream responses are untrusted and may contain prompts, account
/// identifiers, or huge HTML pages.
pub(crate) fn sanitize_upstream_error_body(body: String) -> String {
    let cleaned: String = body
        .chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\r' | '\t'))
        .collect();
    let mut bounded: String = cleaned.chars().take(512).collect();
    if cleaned.chars().count() > 512 {
        bounded.push('…');
    }
    bounded
}

/// Upstream connection and authentication configuration.
#[derive(Clone)]
pub struct ProviderConfig {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub timeout: Duration,
    pub group_id: Option<String>,
}

impl std::fmt::Debug for ProviderConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderConfig")
            .field("base_url", &self.base_url)
            .field("api_key", &"[redacted]")
            .field("model", &self.model)
            .field("timeout", &self.timeout)
            .field("group_id", &self.group_id)
            .finish()
    }
}

impl ProviderConfig {
    pub fn new(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        model: impl Into<String>,
        timeout: Duration,
    ) -> Self {
        Self {
            base_url: base_url.into(),
            api_key: api_key.into(),
            model: model.into(),
            timeout,
            group_id: None,
        }
    }

    pub fn with_group(mut self, group_id: impl Into<String>) -> Self {
        self.group_id = Some(group_id.into());
        self
    }
}

/// Generic assistant tool call entry (T05).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolCallEntry {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

/// Generic chat message representation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatMessage {
    pub role: String,
    pub content: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCallEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
}

impl ChatMessage {
    pub fn new(role: impl Into<String>, content: serde_json::Value) -> Self {
        Self {
            role: role.into(),
            content,
            name: None,
            tool_call_id: None,
            tool_calls: Vec::new(),
            is_error: None,
        }
    }
}

/// Generic chat request representation before provider translation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChatRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<kiro_wire::requests::conversation::ReasoningEffort>,
    pub model: String,
    pub messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    pub stream: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<serde_json::Value>,
}

/// Stream delta chunk types emitted by providers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderDelta {
    Text(String),
    Reasoning(String),
    ToolCallChunk {
        index: usize,
        id: Option<String>,
        name: Option<String>,
        arguments: String,
    },
}

/// Normalized token usage metrics (Spec §15.2, T05).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TokenUsage {
    /// Non-cached prompt tokens
    #[serde(default)]
    pub uncached_prompt_tokens: u64,
    /// Total prompt tokens = uncached + cache_read + cache_creation
    pub prompt_tokens: u64,
    /// Completion / output tokens
    pub completion_tokens: u64,
    /// Total tokens = prompt_tokens + completion_tokens
    pub total_tokens: u64,
    /// True only for a final completion-usage report (not Anthropic message_start).
    #[serde(default)]
    pub output_tokens_final: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_read_input_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_creation_input_tokens: Option<u64>,
}

impl TokenUsage {
    /// Merge a subsequent usage snapshot (e.g. Anthropic message_delta output_tokens)
    /// without erasing previously received prompt tokens or cache statistics.
    pub fn merge(&mut self, other: &TokenUsage) {
        self.uncached_prompt_tokens = self
            .uncached_prompt_tokens
            .max(other.uncached_prompt_tokens);
        self.prompt_tokens = self.prompt_tokens.max(other.prompt_tokens);
        self.completion_tokens = self.completion_tokens.max(other.completion_tokens);
        let cr = match (self.cache_read_input_tokens, other.cache_read_input_tokens) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        };
        self.cache_read_input_tokens = cr;
        let cw = match (
            self.cache_creation_input_tokens,
            other.cache_creation_input_tokens,
        ) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        };
        self.cache_creation_input_tokens = cw;
        self.output_tokens_final |= other.output_tokens_final;
        self.total_tokens = self.prompt_tokens.saturating_add(self.completion_tokens);
    }
}

/// Stream events parsed from provider SSE responses.
#[derive(Debug, Clone, PartialEq)]
pub enum ProviderStreamEvent {
    Delta(ProviderDelta),
    Usage(TokenUsage),
    StopReason(String),
    Done,
}

/// Pluggable model provider adapter trait (Spec §15.2).
pub trait ModelProvider: Send + Sync {
    /// Provider identifier (e.g. "openai", "anthropic").
    fn name(&self) -> &'static str;

    /// Build target upstream endpoint URL from base URL.
    fn endpoint_url(&self, base_url: &str) -> String;

    /// Translate a generic ChatRequest into provider-specific JSON payload.
    fn translate_request(&self, req: &ChatRequest) -> Result<serde_json::Value, ProviderError>;

    /// Parse a single SSE data payload string into normalized stream events.
    fn parse_stream_line(&self, line: &str) -> Result<Vec<ProviderStreamEvent>, ProviderError>;

    /// Extract token usage statistics from provider JSON value.
    fn extract_usage(&self, value: &serde_json::Value) -> Option<TokenUsage>;

    /// Initiate streaming chat completion and return an asynchronous event stream.
    fn chat_stream<'a>(
        &'a self,
        client: &'a reqwest::Client,
        config: &'a ProviderConfig,
        request: &'a ChatRequest,
    ) -> BoxFuture<
        'a,
        Result<BoxStream<'static, Result<ProviderStreamEvent, ProviderError>>, ProviderError>,
    >;
}

/// Simple zero-dependency Stream wrapper around tokio mpsc::Receiver.
pub struct ReceiverStream<T> {
    inner: tokio::sync::mpsc::Receiver<T>,
}

impl<T> ReceiverStream<T> {
    pub fn new(inner: tokio::sync::mpsc::Receiver<T>) -> Self {
        Self { inner }
    }
}

impl<T> Stream for ReceiverStream<T> {
    type Item = T;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.inner.poll_recv(cx)
    }
}

/// Helper function to transform a byte stream into line-delimited SSE event stream.
pub fn process_byte_stream<F>(
    byte_stream: impl Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Send + 'static,
    mut parse_line: F,
) -> BoxStream<'static, Result<ProviderStreamEvent, ProviderError>>
where
    F: FnMut(&str) -> Result<Vec<ProviderStreamEvent>, ProviderError> + Send + 'static,
{
    const MAX_SSE_LINE_BYTES: usize = 1024 * 1024;
    const MAX_SSE_BUFFER_BYTES: usize = 2 * 1024 * 1024;
    let (tx, rx) = tokio::sync::mpsc::channel(64);

    tokio::spawn(async move {
        let mut byte_buffer = Vec::new();
        let mut buffer = String::new();
        let mut stream = Box::pin(byte_stream);
        let mut emitted_done = false;

        loop {
            let chunk_res = tokio::select! {
                _ = tx.closed() => return,
                item = stream.next() => item,
            };
            let Some(chunk_res) = chunk_res else {
                break;
            };
            let bytes = match chunk_res {
                Ok(b) => b,
                Err(e) => {
                    if e.is_timeout() {
                        let _ = tx.send(Err(ProviderError::Timeout)).await;
                    } else {
                        let _ = tx.send(Err(ProviderError::Network(e.to_string()))).await;
                    }
                    return;
                }
            };

            byte_buffer.extend_from_slice(&bytes);

            match std::str::from_utf8(&byte_buffer) {
                Ok(text) => {
                    buffer.push_str(text);
                    byte_buffer.clear();
                }
                Err(error) => {
                    let valid_up_to = error.valid_up_to();
                    if error.error_len().is_some() {
                        let _ = tx
                            .send(Err(ProviderError::Parse(
                                "upstream stream contained invalid UTF-8".to_string(),
                            )))
                            .await;
                        return;
                    }
                    buffer.push_str(
                        std::str::from_utf8(&byte_buffer[..valid_up_to]).unwrap_or_default(),
                    );
                    byte_buffer.drain(..valid_up_to);
                }
            }

            if buffer.len() > MAX_SSE_BUFFER_BYTES {
                let _ = tx
                    .send(Err(ProviderError::Parse(
                        "upstream SSE line buffer exceeded limit".to_string(),
                    )))
                    .await;
                return;
            }

            while let Some(newline_pos) = buffer.find('\n') {
                if newline_pos > MAX_SSE_LINE_BYTES {
                    let _ = tx
                        .send(Err(ProviderError::Parse(
                            "upstream SSE line exceeded limit".to_string(),
                        )))
                        .await;
                    return;
                }
                let line = buffer[..newline_pos].trim_end_matches('\r').to_string();
                buffer.drain(..=newline_pos);

                let line = line.trim();
                if line.is_empty() || line.starts_with(':') {
                    continue;
                }

                match parse_line(line) {
                    Ok(events) => {
                        for ev in events {
                            if ev == ProviderStreamEvent::Done {
                                emitted_done = true;
                            }
                            if tx.send(Ok(ev)).await.is_err() {
                                return; // downstream dropped
                            }
                        }
                    }
                    Err(err) => {
                        let _ = tx.send(Err(err)).await;
                        return;
                    }
                }
            }
        }

        if !byte_buffer.is_empty() {
            let _ = tx
                .send(Err(ProviderError::Parse(
                    "upstream stream ended with incomplete UTF-8 data".to_string(),
                )))
                .await;
            return;
        }

        // Process any residual data in buffer
        let remaining = buffer.trim();
        if remaining.len() > MAX_SSE_LINE_BYTES {
            let _ = tx
                .send(Err(ProviderError::Parse(
                    "upstream SSE line exceeded limit".to_string(),
                )))
                .await;
            return;
        }
        if !remaining.is_empty() && !remaining.starts_with(':') {
            match parse_line(remaining) {
                Ok(events) => {
                    for ev in events {
                        if ev == ProviderStreamEvent::Done {
                            emitted_done = true;
                        }
                        if tx.send(Ok(ev)).await.is_err() {
                            return;
                        }
                    }
                }
                Err(error) => {
                    let _ = tx.send(Err(error)).await;
                    return;
                }
            }
        }

        // Every provider stream must carry an explicit terminal event.  An
        // empty response is also a disconnect, not a successful zero-token
        // completion.
        if !emitted_done {
            let _ = tx.send(Err(ProviderError::StreamDisconnected)).await;
        }
    });

    Box::pin(ReceiverStream::new(rx))
}

#[cfg(test)]
mod endpoint_tests {
    use super::endpoint_with_suffix;

    #[test]
    fn preserves_complete_custom_endpoint() {
        assert_eq!(
            endpoint_with_suffix(
                "https://provider.example/api/chat/completions",
                "/v1",
                "/chat/completions"
            ),
            "https://provider.example/api/chat/completions"
        );
    }

    #[test]
    fn avoids_duplicate_version_prefix() {
        assert_eq!(
            endpoint_with_suffix("https://provider.example/v1/", "/v1", "/chat/completions"),
            "https://provider.example/v1/chat/completions"
        );
    }
}
