use futures_util::{stream, StreamExt};
use gateway::provider::{
    retry::start_stream, BoxFuture, BoxStream, ChatRequest, ModelProvider, ProviderConfig,
    ProviderDelta, ProviderError, ProviderStreamEvent, TokenUsage,
};
use std::{
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

struct Scripted {
    calls: AtomicUsize,
    mode: u8,
}
impl ModelProvider for Scripted {
    fn name(&self) -> &'static str {
        "scripted"
    }
    fn endpoint_url(&self, _: &str) -> String {
        unreachable!()
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
            if self.mode == 0 {
                return Err(ProviderError::Http(
                    reqwest::StatusCode::SERVICE_UNAVAILABLE,
                    String::new(),
                ));
            }
            let mut events = vec![];
            if self.mode == 1 {
                events.push(Ok(ProviderStreamEvent::Usage(TokenUsage {
                    prompt_tokens: if call == 0 { 999 } else { 7 },
                    ..Default::default()
                })));
                if call == 0 {
                    events.push(Err(ProviderError::StreamDisconnected));
                } else {
                    events.push(Ok(ProviderStreamEvent::Delta(ProviderDelta::Text(
                        "OK".into(),
                    ))));
                    events.push(Ok(ProviderStreamEvent::Done));
                }
            } else {
                events.push(Ok(ProviderStreamEvent::Delta(
                    ProviderDelta::ToolCallChunk {
                        index: 0,
                        id: Some("tool-id".into()),
                        name: Some("test".into()),
                        arguments: "{}".into(),
                    },
                )));
                events.push(Err(ProviderError::StreamDisconnected));
            }
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
fn config() -> ProviderConfig {
    ProviderConfig::new("", "", "test", Duration::from_secs(5))
}
#[tokio::test]
async fn attempt_count_is_capped_even_if_caller_requests_more() {
    let provider = Scripted {
        calls: AtomicUsize::new(0),
        mode: 0,
    };
    assert!(start_stream(
        &provider,
        &reqwest::Client::new(),
        &config(),
        &request(),
        100
    )
    .await
    .is_err());
    assert_eq!(provider.calls.load(Ordering::SeqCst), 3);
}
#[tokio::test]
async fn failed_attempt_usage_is_not_forwarded_to_billing() {
    let provider = Scripted {
        calls: AtomicUsize::new(0),
        mode: 1,
    };
    let events: Vec<_> = start_stream(&provider, &reqwest::Client::new(), &config(), &request(), 3)
        .await
        .unwrap()
        .collect()
        .await;
    assert_eq!(events.len(), 3);
    assert!(matches!(&events[0], Ok(ProviderStreamEvent::Usage(u)) if u.prompt_tokens == 7));
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
}
#[tokio::test]
async fn tool_fragment_permanently_disables_replay() {
    let provider = Scripted {
        calls: AtomicUsize::new(0),
        mode: 2,
    };
    let events: Vec<_> = start_stream(&provider, &reqwest::Client::new(), &config(), &request(), 3)
        .await
        .unwrap()
        .collect()
        .await;
    assert!(matches!(
        &events[0],
        Ok(ProviderStreamEvent::Delta(
            ProviderDelta::ToolCallChunk { .. }
        ))
    ));
    assert!(events[1].is_err());
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn failed_attempts_and_selected_key_are_recorded_without_secrets() {
    use gateway::provider::retry::{ATTEMPTS, ATTEMPT_KEY};
    ATTEMPTS
        .scope(std::sync::Mutex::new(Vec::new()), async {
            let provider = Scripted {
                calls: AtomicUsize::new(0),
                mode: 1,
            };
            let result = ATTEMPT_KEY
                .scope(
                    ("provider-a".into(), "key-a".into()),
                    start_stream(&provider, &reqwest::Client::new(), &config(), &request(), 3),
                )
                .await;
            assert!(result.is_ok());
            ATTEMPTS.with(|records| {
                let records = records.lock().unwrap();
                assert_eq!(records.len(), 2);
                assert!(!records[0].success);
                assert_eq!(records[0].error.as_deref(), Some("transport"));
                assert!(records[1].success);
                assert!(records
                    .iter()
                    .all(|r| r.key_id == "key-a" && r.provider_id == "provider-a"));
            });
        })
        .await;
}
