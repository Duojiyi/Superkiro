use billing::provider::{HealthState, Provider, ProviderFormat, ProviderKey};
use futures_util::StreamExt;
use gateway::provider::governance::{
    execute_stream_with_failover, execute_stream_with_model_fallback, probe_provider_key,
    GovernanceError, ProviderKeyPool,
};
use gateway::provider::{ChatMessage, ChatRequest, ProviderDelta, ProviderStreamEvent};
use std::time::Duration;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[test]
fn test_swrr_weighted_distribution() {
    let provider = Provider::new(
        "p1",
        "DeepSeek",
        ProviderFormat::OpenAi,
        "https://api.deepseek.com",
    );
    let key_a = ProviderKey::new("key-a", "p1", "sk-aaa").with_weight(3);
    let key_b = ProviderKey::new("key-b", "p1", "sk-bbb").with_weight(1);

    let pool = ProviderKeyPool::new(provider, vec![key_a, key_b]);

    let mut selections = Vec::new();
    for _ in 0..8 {
        let selected = pool.select_key(1000, &[]).expect("must select key");
        selections.push(selected.id);
    }

    // Mathematical SWRR with weights 3 and 1 produces: A, A, B, A, A, A, B, A
    assert_eq!(
        selections,
        vec!["key-a", "key-a", "key-b", "key-a", "key-a", "key-a", "key-b", "key-a"]
    );

    let count_a = selections
        .iter()
        .filter(|id| id.as_str() == "key-a")
        .count();
    let count_b = selections
        .iter()
        .filter(|id| id.as_str() == "key-b")
        .count();
    assert_eq!(count_a, 6);
    assert_eq!(count_b, 2);
    assert_eq!(count_a / count_b, 3);
}

#[test]
fn test_cooldown_skips_key_and_recovers_after_expiry() {
    let provider = Provider::new(
        "p1",
        "OpenAI",
        ProviderFormat::OpenAi,
        "https://api.openai.com",
    );
    let key_a = ProviderKey::new("key-a", "p1", "sk-aaa").with_weight(2);
    let key_b = ProviderKey::new("key-b", "p1", "sk-bbb").with_weight(2);

    let pool = ProviderKeyPool::new(provider, vec![key_a, key_b]);

    // Key A fails at t=1000 with 60s cooldown
    pool.mark_key_failure("key-a", 1000, Duration::from_secs(60));

    // At t=1030, only Key B is available
    for _ in 0..5 {
        let key = pool.select_key(1030, &[]).expect("must select key-b");
        assert_eq!(key.id, "key-b");
    }

    // At t=1060, Key A cooldown has expired; selections resume alternating
    let mut recovered_selections = Vec::new();
    for _ in 0..4 {
        let key = pool.select_key(1060, &[]).expect("must select");
        recovered_selections.push(key.id);
    }
    assert!(recovered_selections.contains(&"key-a".to_string()));
    assert!(recovered_selections.contains(&"key-b".to_string()));
}

#[test]
fn test_all_keys_in_cooldown_error() {
    let provider = Provider::new(
        "p1",
        "OpenAI",
        ProviderFormat::OpenAi,
        "https://api.openai.com",
    );
    let key_a = ProviderKey::new("key-a", "p1", "sk-aaa");
    let key_b = ProviderKey::new("key-b", "p1", "sk-bbb");

    let pool = ProviderKeyPool::new(provider, vec![key_a, key_b]);

    pool.mark_key_failure("key-a", 1000, Duration::from_secs(30));
    pool.mark_key_failure("key-b", 1000, Duration::from_secs(60));

    match pool.select_key(1010, &[]) {
        Err(GovernanceError::AllKeysInCooldown {
            provider_id,
            next_recovery_secs,
        }) => {
            assert_eq!(provider_id, "p1");
            assert_eq!(next_recovery_secs, 20); // 1030 - 1010 = 20s
        }
        other => panic!("Expected AllKeysInCooldown, got {:?}", other),
    }
}

#[tokio::test]
async fn test_transparent_failover_on_429() {
    let mock_server = MockServer::start().await;

    // Key A encounters 429 Too Many Requests
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer sk-key-a"))
        .respond_with(ResponseTemplate::new(429).set_body_string("Rate limit reached"))
        .mount(&mock_server)
        .await;

    // Key B succeeds with SSE stream
    let sse_chunk = "data: {\"id\":\"1\",\"choices\":[{\"delta\":{\"content\":\"Hello from key B\"}}]}\n\ndata: [DONE]\n\n";
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer sk-key-b"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse_chunk),
        )
        .mount(&mock_server)
        .await;

    let provider = Provider::new(
        "p1",
        "MockProvider",
        ProviderFormat::OpenAi,
        mock_server.uri(),
    );
    let key_a = ProviderKey::new("key-a", "p1", "sk-key-a").with_weight(10); // Higher weight
    let key_b = ProviderKey::new("key-b", "p1", "sk-key-b").with_weight(1);

    let pool = ProviderKeyPool::new(provider, vec![key_a, key_b]);
    let client = reqwest::Client::new();
    let chat_req = ChatRequest {
        reasoning_effort: None,
        model: "gpt-4o".to_string(),
        messages: vec![ChatMessage::new(
            "user",
            serde_json::Value::String("Hello".to_string()),
        )],
        temperature: None,
        max_tokens: None,
        stream: true,
        tools: vec![],
    };

    let (winning_key, mut stream) = execute_stream_with_failover(
        &pool,
        &client,
        "gpt-4o",
        &chat_req,
        Duration::from_secs(60),
        3,
        1000,
    )
    .await
    .expect("failover to key-b should succeed");

    assert_eq!(winning_key.id, "key-b");

    // Verify stream received content from Key B
    let mut got_hello = false;
    while let Some(event) = stream.next().await {
        if let Ok(ProviderStreamEvent::Delta(ProviderDelta::Text(txt))) = event {
            if txt.contains("Hello from key B") {
                got_hello = true;
            }
        }
    }
    assert!(got_hello);

    // Verify key A was placed into cooldown
    let keys = pool.list_keys();
    let key_a_entry = keys.iter().find(|k| k.id == "key-a").unwrap();
    assert_eq!(key_a_entry.health_state, HealthState::Degraded);
    assert!(key_a_entry.cooldown_until.is_some());
}

#[tokio::test]
async fn test_failover_on_401_marks_key_unhealthy() {
    let mock_server = MockServer::start().await;

    // Key A encounters 401 Unauthorized (invalid key)
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer sk-bad-key"))
        .respond_with(ResponseTemplate::new(401).set_body_string("Invalid API Key"))
        .mount(&mock_server)
        .await;

    // Key B succeeds
    let sse_chunk = "data: {\"id\":\"2\",\"choices\":[{\"delta\":{\"content\":\"Key B success\"}}]}\n\ndata: [DONE]\n\n";
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer sk-good-key"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse_chunk),
        )
        .mount(&mock_server)
        .await;

    let provider = Provider::new(
        "p1",
        "MockProvider",
        ProviderFormat::OpenAi,
        mock_server.uri(),
    );
    let key_bad = ProviderKey::new("key-bad", "p1", "sk-bad-key").with_weight(5);
    let key_good = ProviderKey::new("key-good", "p1", "sk-good-key").with_weight(1);

    let pool = ProviderKeyPool::new(provider, vec![key_bad, key_good]);
    let client = reqwest::Client::new();
    let chat_req = ChatRequest {
        reasoning_effort: None,
        model: "gpt-4o".to_string(),
        messages: vec![ChatMessage::new(
            "user",
            serde_json::Value::String("Ping".to_string()),
        )],
        temperature: None,
        max_tokens: None,
        stream: true,
        tools: vec![],
    };

    let (winning_key, _) = execute_stream_with_failover(
        &pool,
        &client,
        "gpt-4o",
        &chat_req,
        Duration::from_secs(60),
        2,
        1000,
    )
    .await
    .expect("should failover to good key");

    assert_eq!(winning_key.id, "key-good");

    // Key bad should now be permanently unhealthy and disabled
    let keys = pool.list_keys();
    let bad_entry = keys.iter().find(|k| k.id == "key-bad").unwrap();
    assert_eq!(bad_entry.health_state, HealthState::Unhealthy);
    assert!(!bad_entry.enabled);
}

#[tokio::test]
async fn test_non_retryable_400_does_not_failover() {
    let mock_server = MockServer::start().await;

    // Upstream returns 400 Bad Request
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(400).set_body_string("Invalid request format"))
        .mount(&mock_server)
        .await;

    let provider = Provider::new(
        "p1",
        "MockProvider",
        ProviderFormat::OpenAi,
        mock_server.uri(),
    );
    let key_a = ProviderKey::new("key-a", "p1", "sk-a");
    let key_b = ProviderKey::new("key-b", "p1", "sk-b");

    let pool = ProviderKeyPool::new(provider, vec![key_a, key_b]);
    let client = reqwest::Client::new();
    let chat_req = ChatRequest {
        reasoning_effort: None,
        model: "gpt-4o".to_string(),
        messages: vec![],
        temperature: None,
        max_tokens: None,
        stream: true,
        tools: vec![],
    };

    let res = execute_stream_with_failover(
        &pool,
        &client,
        "gpt-4o",
        &chat_req,
        Duration::from_secs(60),
        2,
        1000,
    )
    .await;

    match res {
        Err(GovernanceError::NonRetryable(_)) => {}
        _ => panic!("Expected NonRetryable error"),
    }

    // Keys should NOT be cooled down
    let keys = pool.list_keys();
    let key_a_entry = keys.iter().find(|k| k.id == "key-a").unwrap();
    assert_eq!(key_a_entry.health_state, HealthState::Healthy);
    assert!(key_a_entry.cooldown_until.is_none());
}

#[tokio::test]
async fn test_connectivity_probe_and_benchmark() {
    let mock_server = MockServer::start().await;

    let sse_chunks = "data: {\"id\":\"probe-1\",\"choices\":[{\"delta\":{\"content\":\"pong one\"}}]}\n\ndata: {\"id\":\"probe-2\",\"choices\":[{\"delta\":{\"content\":\"pong two\"}}]}\n\ndata: [DONE]\n\n";
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse_chunks),
        )
        .mount(&mock_server)
        .await;

    let provider = Provider::new(
        "p-probe",
        "ProbeProvider",
        ProviderFormat::OpenAi,
        mock_server.uri(),
    );
    let key = ProviderKey::new("k-probe", "p-probe", "sk-probe-123");
    let client = reqwest::Client::new();

    let benchmark = probe_provider_key(&client, &provider, &key, "gpt-4o", 1000).await;

    assert!(benchmark.success);
    assert_eq!(benchmark.status_code, Some(200));
    assert_eq!(benchmark.key_id, "k-probe");
    assert_eq!(benchmark.provider_id, "p-probe");
    assert!(benchmark.ttft_ms.is_some());
    assert!(benchmark.tokens_per_second.is_some());
    assert!(benchmark.tokens_emitted > 0);
    assert!(benchmark.error.is_none());
}

#[test]
fn model_permissions_filter_keys_and_empty_denies_all() {
    let p = Provider::new("p", "P", ProviderFormat::OpenAi, "https://example.com");
    let mut a = ProviderKey::new("a", "p", "secret-a").with_weight(100);
    a.allowed_models = Some(vec!["sonnet".into()]);
    let mut b = ProviderKey::new("b", "p", "secret-b");
    b.allowed_models = Some(vec!["opus".into()]);
    let mut denied = ProviderKey::new("denied", "p", "secret-c");
    denied.allowed_models = Some(vec![]);
    let pool = ProviderKeyPool::new(p, vec![a, b, denied]);
    for _ in 0..20 {
        assert_eq!(
            pool.select_key_for_model(100, &[], Some("opus"))
                .unwrap()
                .id,
            "b"
        );
        assert_eq!(
            pool.select_key_for_model(100, &[], Some("sonnet"))
                .unwrap()
                .id,
            "a"
        );
    }
    assert!(pool
        .select_key_for_model(100, &[], Some("unknown"))
        .is_err());
    assert!(pool
        .select_key_for_model(100, &["b".into()], Some("opus"))
        .is_err());
}

#[test]
fn configuration_refresh_preserves_cooldown_but_rotation_recovers() {
    let p = Provider::new("p", "P", ProviderFormat::OpenAi, "https://example.com");
    let key = ProviderKey::new("a", "p", "old-secret");
    let pool = ProviderKeyPool::new(p, vec![key.clone()]);
    pool.mark_key_failure("a", 100, Duration::from_secs(60));
    pool.add_key(key);
    assert!(pool.select_key(110, &[]).is_err());
    pool.add_key(ProviderKey::new("a", "p", "new-secret"));
    assert!(pool.select_key(110, &[]).is_ok());
}

fn chat_request() -> ChatRequest {
    ChatRequest {
        reasoning_effort: None,
        model: "primary-model".to_string(),
        messages: vec![ChatMessage::new(
            "user",
            serde_json::Value::String("Hello".to_string()),
        )],
        temperature: None,
        max_tokens: None,
        stream: true,
        tools: vec![],
    }
}

/// An OpenAI-compatible upstream answering every request with `status`, and with a
/// short streamed answer when that is 200.
async fn upstream(status: u16) -> MockServer {
    let server = MockServer::start().await;
    let sse = "data: {\"id\":\"1\",\"choices\":[{\"delta\":{\"content\":\"answer\"}}]}\n\ndata: [DONE]\n\n";
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(status)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(if status == 200 { sse } else { "unavailable" }),
        )
        .mount(&server)
        .await;
    server
}

fn pool(id: &str, base_url: &str, keys: Vec<ProviderKey>) -> ProviderKeyPool {
    ProviderKeyPool::new(
        Provider::new(id, id, ProviderFormat::OpenAi, base_url),
        keys,
    )
}

// A fallback chain exists for when earlier targets cannot serve. Targets with no key to
// try do not use up the attempts, however long the chain or small the budget left.
#[tokio::test]
async fn a_chain_reaches_its_healthy_target_past_targets_without_a_key() {
    for budget in [1, 3] {
        let healthy = upstream(200).await;
        // Never contacted; a closed local port should anything go wrong.
        let keyless = pool("keyless", "http://127.0.0.1:9", vec![]);
        let cooling = pool(
            "cooling",
            "http://127.0.0.1:9",
            vec![ProviderKey::new("cooling-key", "cooling", "sk-cooling")],
        );
        cooling.mark_key_failure(
            "cooling-key",
            gateway::now_secs(),
            Duration::from_secs(3_600),
        );
        let candidates = vec![
            (keyless.clone(), "model-a".to_string()),
            (cooling, "model-b".to_string()),
            (keyless, "model-c".to_string()),
            (
                pool(
                    "healthy",
                    &healthy.uri(),
                    vec![ProviderKey::new("healthy-key", "healthy", "sk-healthy")],
                ),
                "model-d".to_string(),
            ),
        ];

        let result = execute_stream_with_model_fallback(
            &candidates,
            &reqwest::Client::new(),
            &chat_request(),
            Duration::from_secs(60),
            budget,
            gateway::now_secs(),
        )
        .await
        .unwrap_or_else(|error| panic!("budget {budget}: {error}"));
        assert_eq!(result.candidate_index, 3);
        assert_eq!(result.target_model, "model-d");
        assert_eq!(healthy.received_requests().await.unwrap().len(), 1);
    }
}

// With a chain, a transient failure of one key moved straight on to the next target, and
// the target's other keys were never tried even with attempts left.
#[tokio::test]
async fn a_chain_tries_another_key_of_a_target_before_giving_up() {
    let primary = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer sk-busy"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&primary)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer sk-spare"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(
                    "data: {\"id\":\"1\",\"choices\":[{\"delta\":{\"content\":\"answer\"}}]}\n\ndata: [DONE]\n\n",
                ),
        )
        .mount(&primary)
        .await;
    let fallback = upstream(503).await;
    let candidates = vec![
        (
            pool(
                "primary",
                &primary.uri(),
                vec![
                    ProviderKey::new("busy", "primary", "sk-busy").with_weight(10),
                    ProviderKey::new("spare", "primary", "sk-spare").with_weight(1),
                ],
            ),
            "primary-model".to_string(),
        ),
        (
            pool(
                "fallback",
                &fallback.uri(),
                vec![ProviderKey::new("fallback-key", "fallback", "sk-fallback")],
            ),
            "fallback-model".to_string(),
        ),
    ];

    let result = execute_stream_with_model_fallback(
        &candidates,
        &reqwest::Client::new(),
        &chat_request(),
        Duration::from_secs(60),
        3,
        gateway::now_secs(),
    )
    .await
    .expect("the primary's second key serves the request");
    assert_eq!(result.key.id, "spare");
    assert_eq!(result.candidate_index, 0);
    // A server error may be the provider failing, so the fallback target was tried
    // before a second key of the same target.
    assert_eq!(fallback.received_requests().await.unwrap().len(), 1);
    assert_eq!(primary.received_requests().await.unwrap().len(), 2);
}

// A rate limit on one key is that key's problem: the target's other key serves the model
// asked for, instead of the request moving to a different model.
#[tokio::test]
async fn a_rate_limited_key_gives_way_to_another_key_of_the_same_target() {
    let primary = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer sk-limited"))
        .respond_with(ResponseTemplate::new(429))
        .mount(&primary)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer sk-spare"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(
                    "data: {\"id\":\"1\",\"choices\":[{\"delta\":{\"content\":\"answer\"}}]}\n\ndata: [DONE]\n\n",
                ),
        )
        .mount(&primary)
        .await;
    let fallback = upstream(200).await;
    let candidates = vec![
        (
            pool(
                "primary",
                &primary.uri(),
                vec![
                    ProviderKey::new("limited", "primary", "sk-limited").with_weight(10),
                    ProviderKey::new("spare", "primary", "sk-spare").with_weight(1),
                ],
            ),
            "primary-model".to_string(),
        ),
        (
            pool(
                "fallback",
                &fallback.uri(),
                vec![ProviderKey::new("fallback-key", "fallback", "sk-fallback")],
            ),
            "fallback-model".to_string(),
        ),
    ];

    let result = execute_stream_with_model_fallback(
        &candidates,
        &reqwest::Client::new(),
        &chat_request(),
        Duration::from_secs(60),
        3,
        gateway::now_secs(),
    )
    .await
    .expect("the primary's other key serves the request");
    assert_eq!(result.key.id, "spare");
    assert_eq!(result.target_model, "primary-model");
    assert!(!result.was_fallback);
    assert!(fallback.received_requests().await.unwrap().is_empty());
}

// A provider incident or a rate limit passes; the key must come back when it does.
#[test]
fn repeated_transient_failures_back_off_but_never_retire_a_key() {
    use gateway::provider::governance::MAX_KEY_BACKOFF;
    let provider = Provider::new("p", "P", ProviderFormat::OpenAi, "https://example.invalid");
    let pool = ProviderKeyPool::new(provider, vec![ProviderKey::new("k", "p", "secret")]);
    let cooldown = Duration::from_secs(60);
    let mut now = 1_000u64;
    let mut last_rest = 0;
    for failure in 1..=12 {
        let key = pool
            .select_key(now, &[])
            .unwrap_or_else(|error| panic!("failure {failure}: the key was retired: {error}"));
        pool.mark_key_failure(&key.id, now, cooldown);
        let rest = pool.list_keys()[0].cooldown_until.unwrap() - now;
        assert!(
            rest >= last_rest && rest <= MAX_KEY_BACKOFF.as_secs(),
            "failure {failure}: {rest}s"
        );
        last_rest = rest;
        now += rest;
    }
    assert_eq!(
        last_rest,
        MAX_KEY_BACKOFF.as_secs(),
        "backs off to the ceiling"
    );
    // Once the rest is over the key is tried, and a success restores it fully.
    let key = pool.select_key(now, &[]).unwrap();
    pool.mark_key_success(&key.id);
    assert_eq!(pool.list_keys()[0].health_state, HealthState::Healthy);
    pool.mark_key_failure(&key.id, now, cooldown);
    assert_eq!(pool.list_keys()[0].cooldown_until.unwrap() - now, 60);
}
