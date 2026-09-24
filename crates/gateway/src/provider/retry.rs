//! Retry only while no text, reasoning or tool event has been exposed.
use super::*;
use crate::watchdog::{WatchdogConfig, WatchdogStream};
use futures_util::stream;

tokio::task_local! {
    pub static ATTEMPTS: std::sync::Mutex<Vec<billing::observability::AttemptRecord>>;
    pub static ATTEMPT_KEY: (String, String);
}

pub(crate) fn stream_error(value: &serde_json::Value) -> ProviderError {
    // Do not retain vendor messages: they can contain prompts or credentials.
    let kind = value
        .pointer("/error/type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    match kind {
        "overloaded_error" => {
            ProviderError::Http(reqwest::StatusCode::SERVICE_UNAVAILABLE, String::new())
        }
        "api_error" => {
            ProviderError::Http(reqwest::StatusCode::INTERNAL_SERVER_ERROR, String::new())
        }
        _ => ProviderError::Service,
    }
}

fn retryable(error: &ProviderError) -> bool {
    match error {
        // 429 is deliberately left to key cooldown: this adapter cannot yet retain Retry-After.
        ProviderError::Http(status, _) => matches!(status.as_u16(), 500 | 502 | 503 | 504),
        ProviderError::Network(_)
        | ProviderError::Timeout
        | ProviderError::StreamDisconnected
        | ProviderError::Watchdog(_) => true,
        _ => false,
    }
}

/// Prime the stream through its first delta. Failed-attempt usage is discarded,
/// while the caller owns a single reservation across attempts. Dropping this
/// future drops the provider receiver and cancels the underlying HTTP pump.
/// The first delta (including reasoning/tool fragments) permanently ends retries.
pub async fn start_stream(
    provider: &dyn ModelProvider,
    client: &reqwest::Client,
    config: &ProviderConfig,
    request: &ChatRequest,
    max_attempts: usize,
) -> Result<BoxStream<'static, Result<ProviderStreamEvent, ProviderError>>, ProviderError> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(150);
    let attempts = max_attempts.clamp(1, 3);
    for attempt in 0..attempts {
        let started = std::time::Instant::now();
        let prime = async {
            let upstream = provider.chat_stream(client, config, request).await?;
            let mut upstream = Box::pin(WatchdogStream::new(upstream, WatchdogConfig::default()));
            let mut prefix = Vec::new();
            let (mut stopped, mut metered) = (false, false);
            while let Some(event) = upstream.next().await {
                let event = event?;
                let output = matches!(event, ProviderStreamEvent::Delta(_));
                stopped |= matches!(event, ProviderStreamEvent::StopReason(_));
                metered |= matches!(event, ProviderStreamEvent::Usage(_));
                if matches!(event, ProviderStreamEvent::Done) {
                    // A stop reason and a usage report before the end marker are a
                    // completed response that happens to be empty — a content filter, or a
                    // thinking budget that consumed the output limit. Retrying it tripled
                    // the vendor bill and put a shared key into cooldown for every tenant.
                    // Anything less is what a failed relay looks like, and is retried.
                    if stopped && metered {
                        prefix.push(Ok(event));
                        return Ok(Box::pin(stream::iter(prefix)) as BoxStream<'static, _>);
                    }
                    return Err(ProviderError::StreamDisconnected);
                }
                prefix.push(Ok(event));
                if output {
                    return Ok(
                        Box::pin(stream::iter(prefix).chain(upstream)) as BoxStream<'static, _>
                    );
                }
                if prefix.len() >= 64 {
                    return Err(ProviderError::Parse("too many events before output".into()));
                }
            }
            Err(ProviderError::StreamDisconnected)
        };
        let attempt_deadline = deadline.min(tokio::time::Instant::now() + Duration::from_secs(65));
        let result = tokio::time::timeout_at(attempt_deadline, prime)
            .await
            .unwrap_or(Err(ProviderError::Timeout));
        let _ = ATTEMPT_KEY.try_with(|(provider_id, key_id)| {
            let _ = ATTEMPTS.try_with(|records| {
                records
                    .lock()
                    .unwrap()
                    .push(billing::observability::AttemptRecord {
                        key_id: key_id.clone(),
                        provider_id: provider_id.clone(),
                        success: result.is_ok(),
                        error: result.as_ref().err().map(|e| match e {
                            ProviderError::Http(status, _) => format!("http_{}", status.as_u16()),
                            ProviderError::Service => "upstream_service".into(),
                            ProviderError::Parse(_) => "protocol".into(),
                            ProviderError::Timeout | ProviderError::Watchdog(_) => "timeout".into(),
                            _ => "transport".into(),
                        }),
                        latency_ms: started.elapsed().as_millis().min(u64::MAX as u128) as u64,
                    });
            });
        });
        match result {
            Ok(stream) => return Ok(stream),
            Err(error) => {
                // No body, key, URL query, or prompt is logged.
                let class = match &error {
                    ProviderError::Http(s, _) => format!("http_{}", s.as_u16()),
                    ProviderError::Parse(_) => "protocol".into(),
                    ProviderError::Service => "service".into(),
                    ProviderError::Timeout | ProviderError::Watchdog(_) => "timeout".into(),
                    _ => "transport".into(),
                };
                let can_retry = attempt + 1 < attempts && retryable(&error);
                eprintln!(
                    "upstream_start model={} attempt={} class={} retry={}",
                    config.model,
                    attempt + 1,
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
                if tokio::time::Instant::now() + delay >= deadline {
                    return Err(error);
                }
                tokio::time::sleep(delay).await;
            }
        }
    }
    unreachable!("at least one attempt")
}

#[cfg(test)]
mod tests {
    use super::*;
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
    #[test]
    fn permanent_and_rate_limit_errors_are_not_blindly_retried() {
        for status in [400, 401, 403, 404, 429] {
            assert!(!retryable(&ProviderError::Http(
                reqwest::StatusCode::from_u16(status).unwrap(),
                String::new()
            )));
        }
        assert!(!retryable(&ProviderError::Parse("bad JSON".into())));
        assert!(!retryable(&ProviderError::Service));
        assert!(retryable(&stream_error(
            &serde_json::json!({"error":{"type":"overloaded_error"}})
        )));
    }
}
