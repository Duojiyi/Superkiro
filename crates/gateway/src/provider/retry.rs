//! Retry only while no text, reasoning or tool event has been exposed.
use super::*;
use crate::watchdog::{WatchdogConfig, WatchdogStream};
use futures_util::stream;

tokio::task_local! {
    pub static ATTEMPTS: std::sync::Mutex<Vec<billing::observability::AttemptRecord>>;
    pub static ATTEMPT_KEY: (String, String);
    /// The request's attempts that ended empty with a usage report.
    pub static EMPTY_ATTEMPT: std::sync::Mutex<EmptyAttempts>;
    /// The request's way to a started model, which all its attempts share.
    pub static ROUTE: std::sync::Arc<Route>;
}

/// The limits an attempt of `request` keeps: its route's, when it is on one.
pub(crate) fn limits_for(request: &ChatRequest) -> UpstreamLimits {
    ROUTE
        .try_with(|route| route.limits_for(request))
        .unwrap_or_else(|_| UpstreamLimits::for_request(request))
}

/// How long an upstream may take: its response headers, one attempt through the model's
/// start, the wait for a start across every attempt of the request, a started model's way
/// to its first content, and a silence (every line counts, pings included). Kiro abandons a
/// request that sends it nothing for 60 seconds (KIRO_CONVERSE_REQUEST_TIMEOUT_MS), so its
/// answer begins by `commit` whatever the route has reached, and keepalives cover the rest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpstreamLimits {
    pub headers: Duration,
    pub attempt: Duration,
    pub total: Duration,
    /// How long after a request arrived its answer to Kiro begins, content or not: Kiro
    /// abandons a request that sends it nothing for 60 seconds, so the answer (its headers,
    /// then keepalives) begins by then, and the route goes on behind it.
    pub commit: Duration,
    /// A model that has started may think silently for minutes before it writes anything;
    /// only its silence is watched meanwhile (`idle`). Ten minutes is as long as an attempt
    /// may take at all (the upstream request's timeout, the watchdog's cap and the answer's
    /// send deadline), so no model is cut off for thinking that could still be answered.
    pub started: Duration,
    pub idle: Duration,
}

impl UpstreamLimits {
    const STANDARD: Self = Self {
        headers: Duration::from_secs(15),
        attempt: Duration::from_secs(65),
        total: Duration::from_secs(150),
        commit: Duration::from_secs(45),
        started: Duration::from_secs(600),
        idle: Duration::from_secs(90),
    };
    /// A model that reasons before it answers: some upstreams hold their response headers
    /// until the model has produced something, which took three 15-second tries to fail,
    /// and an OpenAI-format upstream sends nothing while the model reasons.
    const REASONING: Self = Self {
        headers: Duration::from_secs(45),
        attempt: Duration::from_secs(90),
        total: Duration::from_secs(150),
        commit: Duration::from_secs(45),
        started: Duration::from_secs(600),
        idle: Duration::from_secs(180),
    };

    /// The watchdog for an answer in progress.
    pub fn watchdog(&self) -> WatchdogConfig {
        WatchdogConfig {
            ttfb_timeout: self.attempt,
            idle_timeout: self.idle,
            ..WatchdogConfig::default()
        }
    }

    /// An OpenAI-format upstream sends nothing at all while its model reasons, so its
    /// silence is the model thinking. Once the answer to Kiro has begun, Kiro waits as long
    /// as keepalives come (its own watchdog allows five minutes), so the thinking is given
    /// that long too: cut at three minutes, it was retried from the start and unbilled.
    const OPENAI_REASONING_IDLE: Duration = Duration::from_secs(300);

    pub fn for_request(request: &ChatRequest) -> Self {
        Self::for_model(&request.model, request.reasoning_effort)
    }

    /// The limits of `request` on an upstream of `format`, its adapter's name
    /// ([`ModelProvider::name`]).
    pub fn for_provider(request: &ChatRequest, format: &str) -> Self {
        Self::for_model_on(&request.model, request.reasoning_effort, format)
    }

    /// The limits of `model` on an upstream of `format`.
    pub fn for_model_on(
        model: &str,
        effort: Option<kiro_wire::requests::conversation::ReasoningEffort>,
        format: &str,
    ) -> Self {
        let limits = Self::for_model(model, effort);
        if format == "openai" && super::family::family(model).reasons(effort) {
            Self {
                idle: Self::OPENAI_REASONING_IDLE,
                ..limits
            }
        } else {
            limits
        }
    }

    pub fn for_model(
        model: &str,
        effort: Option<kiro_wire::requests::conversation::ReasoningEffort>,
    ) -> Self {
        let family = super::family::family(model);
        let limits = if family.reasons(effort) {
            Self::REASONING
        } else {
            Self::STANDARD
        };
        // Extend Claude startup for upstream queuing, preserving client and stream limits.
        if matches!(
            family.reasoning,
            super::family::Reasoning::Adaptive { .. } | super::family::Reasoning::Budget
        ) {
            Self {
                headers: Duration::from_secs(90),
                attempt: Duration::from_secs(180),
                total: Duration::from_secs(300),
                ..limits
            }
        } else {
            limits
        }
    }
}

/// A request's way to a started model, shared by its attempts: how long it may still wait
/// for a model to start, and the limits its attempts keep.
#[derive(Debug)]
pub struct Route {
    /// When the request stops waiting for a model to start. It moves on by as long as a
    /// started model ran before its attempt failed: that was not a wait for a start.
    deadline: std::sync::Mutex<tokio::time::Instant>,
    /// Limits for every attempt in place of each target model's own, when set.
    limits: Option<UpstreamLimits>,
    /// Whether the request's prompt may be read again soon, which a prompt-cache write pays
    /// for.
    read_again: std::sync::atomic::AtomicBool,
}

impl Route {
    /// A route that waits at most `total` for a model to start.
    pub fn new(total: Duration) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            deadline: std::sync::Mutex::new(tokio::time::Instant::now() + total),
            limits: None,
            read_again: true.into(),
        })
    }

    /// A route whose attempts all keep `limits`, whatever their target model.
    pub fn with_limits(limits: UpstreamLimits) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            deadline: std::sync::Mutex::new(tokio::time::Instant::now() + limits.total),
            limits: Some(limits),
            read_again: true.into(),
        })
    }

    /// The request's prompt is one nothing reads again soon, such as a commit message
    /// Kiro's fast model writes: its attempts write no prompt cache, which costs a quarter
    /// over the input price and is never read.
    pub fn not_read_again(&self) {
        self.read_again
            .store(false, std::sync::atomic::Ordering::Relaxed);
    }

    pub(crate) fn read_again(&self) -> bool {
        self.read_again.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// The route the current request is on, or a new one for `request`.
    pub(crate) fn current(request: &ChatRequest) -> std::sync::Arc<Self> {
        ROUTE
            .try_with(std::sync::Arc::clone)
            .unwrap_or_else(|_| Self::new(UpstreamLimits::for_request(request).total))
    }

    /// The limits an attempt of `request` keeps.
    pub(crate) fn limits_for(&self, request: &ChatRequest) -> UpstreamLimits {
        self.limits
            .unwrap_or_else(|| UpstreamLimits::for_request(request))
    }

    /// The limits an attempt of `request` keeps on an upstream of `format`.
    fn limits_on(&self, request: &ChatRequest, format: &str) -> UpstreamLimits {
        self.limits
            .unwrap_or_else(|| UpstreamLimits::for_provider(request, format))
    }

    pub(crate) fn deadline(&self) -> tokio::time::Instant {
        *self.deadline.lock().unwrap()
    }

    fn extend(&self, by: Duration) {
        *self.deadline.lock().unwrap() += by;
    }
}

/// Whether a delta is output Kiro shows: text, reasoning, or a tool call's name, id or input.
/// A signature seals thinking already shown, and an empty delta shows nothing.
fn carries_content(delta: &ProviderDelta) -> bool {
    let named = |value: &Option<String>| value.as_deref().is_some_and(|value| !value.is_empty());
    match delta {
        ProviderDelta::Text(text) | ProviderDelta::Reasoning(text) => !text.is_empty(),
        ProviderDelta::ReasoningSignature(_) => false,
        ProviderDelta::ToolCallChunk {
            id,
            name,
            arguments,
            ..
        } => !arguments.is_empty() || named(id) || named(name),
    }
}

/// A request's attempts that ended empty. Their input is billed once, but only when every
/// attempt ended so: one that failed in any other way delivered nothing, and a request that
/// delivered nothing is never billed.
#[derive(Debug, Default)]
pub struct EmptyAttempts {
    last: Option<EmptyAttempt>,
    other_failure: bool,
}

impl EmptyAttempts {
    fn ended_empty(&mut self, attempt: EmptyAttempt) {
        if !self.other_failure {
            self.last = Some(attempt);
        }
    }

    fn failed_otherwise(&mut self) {
        self.other_failure = true;
        self.last = None;
    }

    /// The attempt to bill: the last one, when every attempt ended empty.
    pub fn take(&mut self) -> Option<EmptyAttempt> {
        self.last.take()
    }
}

/// An attempt that ended with an ordinary stop and nothing in it, after reporting usage.
/// It is retried, but the upstream consumed its input: when no attempt of the request
/// produces output, the request is billed what this one reported.
#[derive(Debug, Clone)]
pub struct EmptyAttempt {
    pub provider_id: String,
    pub target_model: String,
    pub usage: TokenUsage,
}

/// An error an upstream reports in its stream, as the HTTP status that would have said the
/// same, so it is refused, retried or cooled as that status is. Vendor messages are not
/// retained (they can contain prompts or credentials): only whether one says the prompt is
/// too long, which Kiro compacts the conversation for.
pub(crate) fn stream_error(value: &serde_json::Value) -> ProviderError {
    use reqwest::StatusCode;
    let field = |name: &str| {
        value
            .pointer(&format!("/error/{name}"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
    };
    let http = |status: StatusCode| ProviderError::Http(status, String::new());
    let kind = field("type");
    if kind == "request_too_large"
        || says_input_too_long(field("code"))
        || says_input_too_long(field("message"))
    {
        return http(StatusCode::PAYLOAD_TOO_LARGE);
    }
    match kind {
        "overloaded_error" => http(StatusCode::SERVICE_UNAVAILABLE),
        "api_error" => http(StatusCode::INTERNAL_SERVER_ERROR),
        "rate_limit_error" => http(StatusCode::TOO_MANY_REQUESTS),
        "invalid_request_error" => http(StatusCode::BAD_REQUEST),
        // A relay may forward its own upstream credential failure inside a successful
        // HTTP response. Only an actual HTTP 401 proves our relay key is invalid.
        "authentication_error" => ProviderError::Unavailable,
        "permission_error" => http(StatusCode::FORBIDDEN),
        "not_found_error" => http(StatusCode::NOT_FOUND),
        // A relay's own failure: kimera-primary answers about one request in twenty with
        // `upstream_error` ("当前模型暂时不可用") as its first frame, and the next succeeds.
        _ => ProviderError::Unavailable,
    }
}

/// A failed attempt as traces and the console name it: its kind only, never the upstream's
/// message, which can carry prompts or credentials.
pub(crate) fn failure_class(error: &ProviderError) -> String {
    match error {
        ProviderError::Http(status, _) => format!("http_{}", status.as_u16()),
        ProviderError::ThinkingNeedsOutput { .. } => "configuration".into(),
        ProviderError::Unavailable => "upstream_service".into(),
        ProviderError::Parse(_) => "protocol".into(),
        ProviderError::Timeout | ProviderError::Watchdog(_) | ProviderError::NoAnswer => {
            "timeout".into()
        }
        ProviderError::EmptyCompletion => "empty".into(),
        _ => "transport".into(),
    }
}

fn retryable(error: &ProviderError) -> bool {
    match error {
        // 429 is deliberately left to key cooldown: this adapter cannot yet retain Retry-After.
        ProviderError::Http(status, _) => matches!(status.as_u16(), 500 | 502 | 503 | 504),
        // Only ever seen here before the model's first content: an error after it reaches
        // the answer, and is not retried.
        ProviderError::Network(_)
        | ProviderError::Timeout
        | ProviderError::NoAnswer
        | ProviderError::StreamDisconnected
        | ProviderError::Watchdog(_)
        | ProviderError::EmptyCompletion
        | ProviderError::Unavailable => true,
        _ => false,
    }
}

/// Prime the stream through the model's first content: text, reasoning, or a tool call.
/// Until then nothing has reached Kiro, so an attempt that fails (an error frame, a dropped
/// stream, a silence, an answer with nothing in it) is retried, on this key or by the
/// caller on another, and its usage is discarded, except that of an empty attempt, kept in
/// [`EMPTY_ATTEMPT`], while the caller owns a single reservation across attempts. Waiting
/// for a model to start is bounded by `headers`, `attempt` and the route's `total`; a model
/// that has started only by `started` and its silence. Dropping this future drops the
/// provider receiver and cancels the underlying HTTP pump.
pub async fn start_stream(
    provider: &dyn ModelProvider,
    client: &reqwest::Client,
    config: &ProviderConfig,
    request: &ChatRequest,
    max_attempts: usize,
) -> Result<BoxStream<'static, Result<ProviderStreamEvent, ProviderError>>, ProviderError> {
    let route = Route::current(request);
    let limits = route.limits_on(request, provider.name());
    let provider_id = ATTEMPT_KEY
        .try_with(|(provider_id, _)| provider_id.clone())
        .unwrap_or_else(|_| provider.name().to_string());
    let attempts = max_attempts.clamp(1, 3);
    for attempt in 0..attempts {
        let began = std::time::Instant::now();
        let (result, started_at) = prime(
            provider,
            client,
            config,
            request,
            &limits,
            &route,
            &provider_id,
        )
        .await;
        if let Some(started_at) = started_at {
            route.extend(started_at.elapsed());
        }
        let _ = ATTEMPT_KEY.try_with(|(provider_id, key_id)| {
            let _ = ATTEMPTS.try_with(|records| {
                records
                    .lock()
                    .unwrap()
                    .push(billing::observability::AttemptRecord {
                        key_id: key_id.clone(),
                        provider_id: provider_id.clone(),
                        success: result.is_ok(),
                        error: result.as_ref().err().map(failure_class),
                        latency_ms: began.elapsed().as_millis().min(u64::MAX as u128) as u64,
                    });
            });
        });
        match result {
            Ok(stream) => return Ok(stream),
            Err(error) => {
                if !matches!(error, ProviderError::EmptyCompletion) {
                    let _ = EMPTY_ATTEMPT.try_with(|slot| slot.lock().unwrap().failed_otherwise());
                }
                // No body, key, URL query, or prompt is logged.
                let class = match &error {
                    ProviderError::Http(s, _) => format!("http_{}", s.as_u16()),
                    ProviderError::Parse(_) => "protocol".into(),
                    ProviderError::Unavailable => "unavailable".into(),
                    ProviderError::Timeout | ProviderError::Watchdog(_) => "timeout".into(),
                    ProviderError::NoAnswer => "no_answer".into(),
                    ProviderError::EmptyCompletion => "empty".into(),
                    _ => "transport".into(),
                };
                let can_retry = attempt + 1 < attempts && retryable(&error);
                eprintln!(
                    "upstream_start model={} attempt={} started={} class={} retry={}",
                    config.model,
                    attempt + 1,
                    started_at.is_some(),
                    class,
                    can_retry
                );
                if !can_retry {
                    return Err(error);
                }
                let jitter = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .subsec_millis() as u64
                    % 251;
                let delay = Duration::from_millis(if attempt == 0 { 1000 } else { 3000 } + jitter);
                if tokio::time::Instant::now() + delay >= route.deadline() {
                    return Err(error);
                }
                tokio::time::sleep(delay).await;
            }
        }
    }
    unreachable!("at least one attempt")
}

/// One attempt, primed through its model's first content, or through an empty answer
/// whose stop reason makes it final: the stream from there, or why the attempt failed; and
/// when its model started, if it did.
async fn prime(
    provider: &dyn ModelProvider,
    client: &reqwest::Client,
    config: &ProviderConfig,
    request: &ChatRequest,
    limits: &UpstreamLimits,
    route: &Route,
    provider_id: &str,
) -> (
    Result<BoxStream<'static, Result<ProviderStreamEvent, ProviderError>>, ProviderError>,
    Option<tokio::time::Instant>,
) {
    let mut started_at = None;
    let result = async {
        // Until the model starts: this attempt's share of the request's wait for a start.
        let mut until = route
            .deadline()
            .min(tokio::time::Instant::now() + limits.attempt);
        if tokio::time::Instant::now() >= until {
            return Err(ProviderError::Timeout);
        }
        let upstream =
            tokio::time::timeout_at(until, provider.chat_stream(client, config, request))
                .await
                .unwrap_or(Err(ProviderError::Timeout))?;
        let mut upstream = Box::pin(WatchdogStream::new(upstream, limits.watchdog()));
        let mut prefix = Vec::new();
        let (mut stop_reason, mut metered) = (None, false);
        loop {
            let event = match tokio::time::timeout_at(until, upstream.next()).await {
                Err(_) => return Err(ProviderError::Timeout),
                Ok(None) => return Err(ProviderError::StreamDisconnected),
                Ok(Some(event)) => event?,
            };
            if event == ProviderStreamEvent::Done {
                // An empty response whose stop reason explains it — the output limit, a
                // content filter — is final: retrying it tripled the vendor bill and put a
                // shared key into cooldown for every tenant. An ordinary stop with nothing
                // in it is also what a failing relay sends, so it is retried, but the key
                // answered and is not cooled down for it.
                return match stop_reason.as_deref() {
                    Some(reason) if metered && crate::stream::is_explicit_empty_stop(reason) => {
                        prefix.push(Ok(event));
                        Ok(Box::pin(stream::iter(prefix)) as BoxStream<'static, _>)
                    }
                    Some(_) if metered => {
                        let mut usage = TokenUsage::default();
                        for event in &prefix {
                            if let Ok(ProviderStreamEvent::Usage(next)) = event {
                                usage.merge(next);
                            }
                        }
                        let _ = EMPTY_ATTEMPT.try_with(|slot| {
                            slot.lock().unwrap().ended_empty(EmptyAttempt {
                                provider_id: provider_id.to_string(),
                                target_model: config.model.clone(),
                                usage,
                            })
                        });
                        Err(ProviderError::EmptyCompletion)
                    }
                    _ => Err(ProviderError::StreamDisconnected),
                };
            }
            let content = match &event {
                // Liveness for the watchdog, nothing to keep.
                ProviderStreamEvent::Heartbeat => continue,
                ProviderStreamEvent::Delta(ProviderDelta::ReasoningSignature(_)) => false,
                ProviderStreamEvent::Delta(delta) if !carries_content(delta) => continue,
                ProviderStreamEvent::Delta(_) => true,
                ProviderStreamEvent::Started => {
                    if started_at.is_none() {
                        let now = tokio::time::Instant::now();
                        started_at = Some(now);
                        until = now + limits.started;
                    }
                    false
                }
                ProviderStreamEvent::StopReason(reason) => {
                    stop_reason = Some(reason.clone());
                    false
                }
                ProviderStreamEvent::Usage(_) => {
                    metered = true;
                    false
                }
                _ => false,
            };
            prefix.push(Ok(event));
            if content {
                return Ok(Box::pin(stream::iter(prefix).chain(upstream)) as BoxStream<'static, _>);
            }
            if prefix.len() >= 64 {
                return Err(ProviderError::Parse("too many events before output".into()));
            }
        }
    }
    .await;
    (result, started_at)
}

#[cfg(test)]
mod tests {
    use super::*;

    // An OpenAI-format upstream sends nothing while its model reasons: the silence is given
    // as long as Kiro waits once its answer has begun. Other upstreams and models keep
    // theirs.
    #[test]
    fn an_openai_reasoning_model_may_think_silently_for_five_minutes() {
        let idle =
            |model: &str, format: &str| UpstreamLimits::for_model_on(model, None, format).idle;
        assert_eq!(idle("o3", "openai"), Duration::from_secs(300));
        assert_eq!(idle("gpt-5", "openai"), Duration::from_secs(300));
        assert_eq!(idle("o3", "anthropic"), UpstreamLimits::REASONING.idle);
        assert_eq!(idle("gpt-4o", "openai"), UpstreamLimits::STANDARD.idle);
        assert_eq!(
            UpstreamLimits::for_model_on(
                "claude-sonnet-4-5",
                Some(kiro_wire::requests::conversation::ReasoningEffort::High),
                "openai"
            )
            .idle,
            Duration::from_secs(300)
        );
    }
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Fake {
        calls: AtomicUsize,
        partial: bool,
        auth_error: bool,
    }
    impl ModelProvider for Fake {
        fn name(&self) -> &'static str {
            "fake"
        }
        fn endpoint_url(&self, _: &str) -> String {
            String::new()
        }
        fn translate_request(&self, _: &ChatRequest) -> Result<serde_json::Value, ProviderError> {
            unreachable!()
        }
        fn parse_stream_line(&self, _: &str) -> Result<Vec<ProviderStreamEvent>, ProviderError> {
            unreachable!()
        }
        fn extract_usage(&self, _: &serde_json::Value) -> Option<TokenUsage> {
            None
        }
        fn chat_stream<'a>(
            &'a self,
            _: &'a reqwest::Client,
            _: &'a ProviderConfig,
            _: &'a ChatRequest,
        ) -> BoxFuture<
            'a,
            Result<BoxStream<'static, Result<ProviderStreamEvent, ProviderError>>, ProviderError>,
        > {
            Box::pin(async move {
                let call = self.calls.fetch_add(1, Ordering::SeqCst);
                if self.auth_error {
                    return Err(ProviderError::Http(
                        reqwest::StatusCode::UNAUTHORIZED,
                        String::new(),
                    ));
                }
                let mut events = vec![];
                if self.partial || call > 0 {
                    events.push(Ok(ProviderStreamEvent::Delta(ProviderDelta::Text(
                        "ok".into(),
                    ))));
                }
                events.push(if call == 0 {
                    Err(ProviderError::StreamDisconnected)
                } else {
                    Ok(ProviderStreamEvent::Done)
                });
                Ok(Box::pin(stream::iter(events)) as BoxStream<'static, _>)
            })
        }
    }
    fn request() -> ChatRequest {
        ChatRequest {
            model: "test".into(),
            messages: vec![],
            temperature: None,
            max_tokens: Some(8),
            stream: true,
            tools: vec![],
            reasoning_effort: None,
        }
    }
    #[tokio::test]
    async fn preoutput_disconnect_retries_but_partial_output_never_replays() {
        for partial in [false, true] {
            let provider = Fake {
                calls: AtomicUsize::new(0),
                partial,
                auth_error: false,
            };
            let mut output = start_stream(
                &provider,
                &reqwest::Client::new(),
                &ProviderConfig::new("", "", "test", Duration::from_secs(5)),
                &request(),
                3,
            )
            .await
            .unwrap();
            assert!(matches!(
                output.next().await,
                Some(Ok(ProviderStreamEvent::Delta(_)))
            ));
            let terminal = output.next().await.unwrap();
            assert_eq!(terminal.is_err(), partial);
            assert_eq!(
                provider.calls.load(Ordering::SeqCst),
                if partial { 1 } else { 2 }
            );
        }
    }
    #[tokio::test]
    async fn authentication_is_not_retried() {
        let provider = Fake {
            calls: AtomicUsize::new(0),
            partial: false,
            auth_error: true,
        };
        assert!(start_stream(
            &provider,
            &reqwest::Client::new(),
            &ProviderConfig::new("", "", "test", Duration::from_secs(5)),
            &request(),
            3
        )
        .await
        .is_err());
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    }
    /// What an upstream answers one call with: events, each after a pause in milliseconds.
    type Script = Vec<(u64, Result<ProviderStreamEvent, ProviderError>)>;

    /// An upstream answering each call with its script.
    struct Scripted {
        calls: AtomicUsize,
        script: fn(usize) -> Script,
    }
    impl ModelProvider for Scripted {
        fn name(&self) -> &'static str {
            "scripted"
        }
        fn endpoint_url(&self, _: &str) -> String {
            String::new()
        }
        fn translate_request(&self, _: &ChatRequest) -> Result<serde_json::Value, ProviderError> {
            unreachable!()
        }
        fn parse_stream_line(&self, _: &str) -> Result<Vec<ProviderStreamEvent>, ProviderError> {
            unreachable!()
        }
        fn extract_usage(&self, _: &serde_json::Value) -> Option<TokenUsage> {
            None
        }
        fn chat_stream<'a>(
            &'a self,
            _: &'a reqwest::Client,
            _: &'a ProviderConfig,
            _: &'a ChatRequest,
        ) -> BoxFuture<
            'a,
            Result<BoxStream<'static, Result<ProviderStreamEvent, ProviderError>>, ProviderError>,
        > {
            Box::pin(async move {
                let script = (self.script)(self.calls.fetch_add(1, Ordering::SeqCst));
                Ok(
                    Box::pin(stream::iter(script).then(|(pause, event)| async move {
                        tokio::time::sleep(Duration::from_millis(pause)).await;
                        event
                    })) as BoxStream<'static, _>,
                )
            })
        }
    }

    /// Limits short enough to pass in a test.
    fn short_limits() -> UpstreamLimits {
        UpstreamLimits {
            headers: Duration::from_millis(200),
            attempt: Duration::from_millis(200),
            total: Duration::from_secs(10),
            commit: Duration::from_millis(100),
            started: Duration::from_secs(5),
            idle: Duration::from_millis(300),
        }
    }

    /// `provider` primed on a route with [`short_limits`].
    async fn start_briefly(
        provider: &Scripted,
    ) -> Result<BoxStream<'static, Result<ProviderStreamEvent, ProviderError>>, ProviderError> {
        ROUTE
            .scope(
                Route::with_limits(short_limits()),
                start_stream(
                    provider,
                    &reqwest::Client::new(),
                    &ProviderConfig::new("", "", "test", Duration::from_secs(5)),
                    &request(),
                    3,
                ),
            )
            .await
    }

    /// The events `provider` is primed to, with how many calls it took.
    async fn primed(script: fn(usize) -> Script) -> (Vec<ProviderStreamEvent>, usize) {
        let provider = Scripted {
            calls: AtomicUsize::new(0),
            script,
        };
        let mut output = start_briefly(&provider).await.unwrap();
        let mut events = Vec::new();
        while let Some(event) = output.next().await {
            events.push(event.unwrap());
        }
        (events, provider.calls.load(Ordering::SeqCst))
    }

    fn answer() -> Script {
        vec![
            (0, Ok(ProviderStreamEvent::Started)),
            (
                0,
                Ok(ProviderStreamEvent::Delta(ProviderDelta::Text("ok".into()))),
            ),
            (0, Ok(ProviderStreamEvent::Done)),
        ]
    }

    fn answered() -> Vec<ProviderStreamEvent> {
        answer()
            .into_iter()
            .map(|(_, event)| event.unwrap())
            .collect()
    }

    /// A model that fails after it has started, before any content, is retried: nothing of
    /// it has reached Kiro, and the retry is what the customer is answered and billed by.
    #[tokio::test]
    async fn a_started_model_that_fails_before_content_is_retried() {
        let (events, calls) = primed(|call| match call {
            0 => vec![
                (0, Ok(ProviderStreamEvent::Heartbeat)),
                (0, Ok(ProviderStreamEvent::Started)),
                (0, Err(ProviderError::StreamDisconnected)),
            ],
            _ => answer(),
        })
        .await;
        assert_eq!(events, answered());
        assert_eq!(calls, 2);
    }

    /// Once it has started, a model may think silently past the wait for a start, for as
    /// long as it stays alive: every line, pings included, keeps it so.
    #[tokio::test]
    async fn a_started_model_thinks_as_long_as_it_stays_alive() {
        let (events, calls) = primed(|_| {
            let mut script = vec![(0, Ok(ProviderStreamEvent::Started))];
            script.extend((0..10).map(|_| (100, Ok(ProviderStreamEvent::Heartbeat))));
            script.extend(answer().into_iter().skip(1));
            script
        })
        .await;
        assert_eq!(events, answered());
        assert_eq!(calls, 1);
    }

    /// A started model that falls silent is given up on, and the request tried again.
    #[tokio::test]
    async fn a_started_model_that_falls_silent_is_retried() {
        let (events, calls) = primed(|call| match call {
            0 => vec![
                (0, Ok(ProviderStreamEvent::Started)),
                (
                    1_000,
                    Ok(ProviderStreamEvent::Delta(ProviderDelta::Text(
                        "late".into(),
                    ))),
                ),
            ],
            _ => answer(),
        })
        .await;
        assert_eq!(events, answered());
        assert_eq!(calls, 2);
    }

    /// After its first content a model's failure reaches the answer, and is not retried.
    #[tokio::test]
    async fn a_failure_after_content_is_not_retried() {
        let provider = Scripted {
            calls: AtomicUsize::new(0),
            script: |_| {
                vec![
                    (0, Ok(ProviderStreamEvent::Started)),
                    (
                        0,
                        Ok(ProviderStreamEvent::Delta(ProviderDelta::Text("ok".into()))),
                    ),
                    (0, Err(ProviderError::StreamDisconnected)),
                ]
            },
        };
        let mut output = start_briefly(&provider).await.unwrap();
        assert_eq!(
            output.next().await.unwrap().unwrap(),
            ProviderStreamEvent::Started
        );
        assert!(output.next().await.unwrap().is_ok());
        assert!(output.next().await.unwrap().is_err());
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn models_that_reason_first_get_longer_to_start() {
        let mut request = request();
        request.model = "claude-opus-5-5".into();
        let reasoning = UpstreamLimits::for_request(&request);
        request.model = "gpt-4o".into();
        let standard = UpstreamLimits::for_request(&request);
        assert_eq!(standard.headers, Duration::from_secs(15));
        assert!(reasoning.headers > standard.headers);
        assert!(reasoning.attempt > standard.attempt);
        assert!(reasoning.idle > standard.idle);
        request.reasoning_effort = Some(kiro_wire::requests::conversation::ReasoningEffort::Low);
        assert_eq!(
            UpstreamLimits::for_request(&request),
            UpstreamLimits::REASONING
        );
    }

    #[test]
    fn claude_startup_budget_preserves_stream_and_client_limits() {
        use kiro_wire::requests::conversation::ReasoningEffort;
        for model in ["claude-opus-5-5", "claude-sonnet-4-6", "claude-sonnet-4-5"] {
            for effort in [None, Some(ReasoningEffort::Medium)] {
                let reasons = super::super::family::family(model).reasons(effort);
                let base = if reasons {
                    UpstreamLimits::REASONING
                } else {
                    UpstreamLimits::STANDARD
                };
                let expected = UpstreamLimits {
                    headers: Duration::from_secs(90),
                    attempt: Duration::from_secs(180),
                    total: Duration::from_secs(300),
                    ..base
                };
                let mut req = request();
                req.model = model.into();
                req.reasoning_effort = effort;
                assert_eq!(UpstreamLimits::for_request(&req), expected);
                assert_eq!(limits_for(&req), expected);
                assert_eq!(UpstreamLimits::for_provider(&req, "anthropic"), expected);
                assert_eq!(
                    UpstreamLimits::for_provider(&req, "openai"),
                    UpstreamLimits {
                        idle: if reasons {
                            Duration::from_secs(300)
                        } else {
                            base.idle
                        },
                        ..expected
                    }
                );
            }
        }
        assert_eq!(
            UpstreamLimits::for_model("gpt-4o", None),
            UpstreamLimits::STANDARD
        );
        assert_eq!(
            UpstreamLimits::for_model("gpt-5", None),
            UpstreamLimits::REASONING
        );
    }

    #[test]
    fn permanent_and_rate_limit_errors_are_not_blindly_retried() {
        for status in [400, 401, 403, 404, 429] {
            assert!(!retryable(&ProviderError::Http(
                reqwest::StatusCode::from_u16(status).unwrap(),
                String::new()
            )));
        }
        assert!(!retryable(&ProviderError::Parse("bad JSON".into())));
        assert!(retryable(&stream_error(
            &serde_json::json!({"error":{"type":"overloaded_error"}})
        )));
    }

    #[test]
    fn a_relays_own_failure_is_retried_and_a_fault_of_the_request_is_not() {
        let event = |kind: &str| {
            stream_error(&serde_json::json!({"type": "error", "error": {"type": kind}}))
        };
        assert!(matches!(
            event("upstream_error"),
            ProviderError::Unavailable
        ));
        assert!(retryable(&event("upstream_error")));
        assert!(matches!(
            event("authentication_error"),
            ProviderError::Unavailable
        ));
        assert!(retryable(&event("authentication_error")));
        assert!(retryable(&event("")));
        for (fault, status) in [
            ("invalid_request_error", 400),
            ("permission_error", 403),
            ("not_found_error", 404),
            ("request_too_large", 413),
        ] {
            assert!(
                matches!(event(fault), ProviderError::Http(got, _) if got.as_u16() == status),
                "{fault}"
            );
            assert!(!retryable(&event(fault)), "{fault}");
        }
        assert!(matches!(
            event("rate_limit_error"),
            ProviderError::Http(status, _) if status.as_u16() == 429
        ));
    }
}
