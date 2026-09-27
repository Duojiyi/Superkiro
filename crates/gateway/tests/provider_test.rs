use futures_util::StreamExt;
use gateway::provider::anthropic::AnthropicProvider;
use gateway::provider::openai::OpenAiProvider;
use gateway::provider::{
    ChatMessage, ChatRequest, ModelProvider, ProviderConfig, ProviderDelta, ProviderError,
    ProviderStreamEvent, TokenUsage,
};
use reqwest::StatusCode;
use std::time::Duration;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn create_test_request(model: &str) -> ChatRequest {
    ChatRequest {
        reasoning_effort: None,
        model: model.to_string(),
        messages: vec![
            ChatMessage::new(
                "system",
                serde_json::json!("You are a helpful coding assistant."),
            ),
            ChatMessage::new("user", serde_json::json!("Hello!")),
        ],
        temperature: Some(0.7),
        max_tokens: Some(1024),
        stream: true,
        tools: Vec::new(),
    }
}

// --------------------------------------------------------------------------
// 1. OpenAI Provider Happy Path
// --------------------------------------------------------------------------
#[tokio::test]
async fn test_openai_provider_streaming_happy_path() {
    let mock_server = MockServer::start().await;

    let sse_body = [
        r#"data: {"id":"1","choices":[{"delta":{"content":"Hello "}}]}"#,
        r#"data: {"id":"2","choices":[{"delta":{"reasoning_content":"Thinking deeply..."}}]}"#,
        r#"data: {"id":"3","choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_123","function":{"name":"read_file","arguments":"{\"path\":\"src/main.rs\"}"}}]}}]}"#,
        r#"data: {"id":"4","choices":[{"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":30,"completion_tokens":20,"total_tokens":50,"prompt_tokens_details":{"cached_tokens":10}}}"#,
        "data: [DONE]",
        "",
    ]
    .join("\n\n");

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer test-api-key"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse_body, "text/event-stream"))
        .mount(&mock_server)
        .await;

    let client = reqwest::Client::new();
    let config = ProviderConfig {
        base_url: mock_server.uri(),
        api_key: "test-api-key".to_string(),
        model: "deepseek-chat".to_string(),
        timeout: Duration::from_secs(5),
        group_id: None,
    };

    let req = create_test_request("deepseek-chat");
    let provider = OpenAiProvider;

    let mut stream = provider
        .chat_stream(&client, &config, &req)
        .await
        .expect("chat_stream must succeed");

    let mut events = Vec::new();
    while let Some(res) = stream.next().await {
        events.push(res.expect("stream event should be ok"));
    }

    // Assert delta content
    assert!(events
        .iter()
        .any(|e| matches!(e, ProviderStreamEvent::Delta(ProviderDelta::Text(t)) if t == "Hello ")));
    assert!(events.iter().any(|e| matches!(e, ProviderStreamEvent::Delta(ProviderDelta::Reasoning(r)) if r == "Thinking deeply...")));
    assert!(events.iter().any(|e| matches!(e, ProviderStreamEvent::Delta(ProviderDelta::ToolCallChunk { name: Some(n), .. }) if n == "read_file")));

    // Assert usage
    let usage = events
        .iter()
        .find_map(|e| match e {
            ProviderStreamEvent::Usage(u) => Some(u),
            _ => None,
        })
        .expect("Usage should be parsed");
    assert_eq!(usage.prompt_tokens, 30);
    assert_eq!(usage.completion_tokens, 20);
    assert_eq!(usage.cache_read_input_tokens, Some(10));

    // Assert stop reason and done
    assert!(events
        .iter()
        .any(|e| matches!(e, ProviderStreamEvent::StopReason(r) if r == "stop")));
    assert!(events
        .iter()
        .any(|e| matches!(e, ProviderStreamEvent::Done)));
}

// --------------------------------------------------------------------------
// 2. Anthropic Provider Happy Path
// --------------------------------------------------------------------------
#[tokio::test]
async fn test_anthropic_provider_streaming_happy_path() {
    let mock_server = MockServer::start().await;

    let sse_body = [
        r#"data: {"type":"message_start","message":{"id":"msg_123","usage":{"input_tokens":45,"cache_read_input_tokens":15}}}"#,
        r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"tu_456","name":"fs_read"}}"#,
        r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"path\":\"Cargo.toml\"}"}}"#,
        r#"data: {"type":"content_block_delta","index":1,"delta":{"type":"thinking_delta","thinking":"Analyzing Cargo dependencies..."}}"#,
        r#"data: {"type":"content_block_delta","index":2,"delta":{"type":"text_delta","text":"Here is the file content."}}"#,
        r#"data: {"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":25}}"#,
        r#"data: {"type":"message_stop"}"#,
        "",
    ]
    .join("\n\n");

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", "anthropic-key-abc"))
        .and(header("anthropic-version", "2023-06-01"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse_body, "text/event-stream"))
        .mount(&mock_server)
        .await;

    let client = reqwest::Client::new();
    let config = ProviderConfig {
        base_url: mock_server.uri(),
        api_key: "anthropic-key-abc".to_string(),
        model: "claude-3-7-sonnet".to_string(),
        timeout: Duration::from_secs(5),
        group_id: None,
    };

    let req = create_test_request("claude-3-7-sonnet");
    let provider = AnthropicProvider;

    let mut stream = provider
        .chat_stream(&client, &config, &req)
        .await
        .expect("chat_stream must succeed");

    let mut events = Vec::new();
    while let Some(res) = stream.next().await {
        events.push(res.expect("stream event should be ok"));
    }

    // Assert deltas
    assert!(events.iter().any(|e| matches!(e, ProviderStreamEvent::Delta(ProviderDelta::Text(t)) if t == "Here is the file content.")));
    assert!(events.iter().any(|e| matches!(e, ProviderStreamEvent::Delta(ProviderDelta::Reasoning(r)) if r == "Analyzing Cargo dependencies...")));
    assert!(events.iter().any(|e| matches!(e, ProviderStreamEvent::Delta(ProviderDelta::ToolCallChunk { id: Some(id), .. }) if id == "tu_456")));

    // Assert usage
    let prompt_usage = events
        .iter()
        .find_map(|e| match e {
            ProviderStreamEvent::Usage(TokenUsage {
                uncached_prompt_tokens,
                prompt_tokens,
                cache_read_input_tokens,
                ..
            }) if *uncached_prompt_tokens == 45 => Some((
                *uncached_prompt_tokens,
                *prompt_tokens,
                *cache_read_input_tokens,
            )),
            _ => None,
        })
        .expect("Prompt usage must be extracted");
    assert_eq!(prompt_usage.0, 45); // uncached_prompt_tokens
    assert_eq!(prompt_usage.1, 60); // total prompt_tokens = 45 + 15
    assert_eq!(prompt_usage.2, Some(15));

    // Assert stop reason and done
    assert!(events
        .iter()
        .any(|e| matches!(e, ProviderStreamEvent::StopReason(r) if r == "tool_use")));
    assert!(events
        .iter()
        .any(|e| matches!(e, ProviderStreamEvent::Done)));
}

// --------------------------------------------------------------------------
// 3. Audit B - Upstream Exception 1: 断流 (Premature Stream Disconnect)
// --------------------------------------------------------------------------
#[tokio::test]
async fn test_audit_b_upstream_stream_disconnect() {
    let mock_server = MockServer::start().await;

    // Upstream sends 1 chunk then closes connection without Done / message_stop
    let premature_body = r#"data: {"id":"1","choices":[{"delta":{"content":"Half response..."}}]}"#;

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(premature_body, "text/event-stream"))
        .mount(&mock_server)
        .await;

    let client = reqwest::Client::new();
    let config = ProviderConfig {
        base_url: mock_server.uri(),
        api_key: "key".to_string(),
        model: "m".to_string(),
        timeout: Duration::from_secs(5),
        group_id: None,
    };

    let req = create_test_request("m");
    let mut stream = OpenAiProvider
        .chat_stream(&client, &config, &req)
        .await
        .unwrap();

    let first = stream.next().await.unwrap();
    assert!(first.is_ok());

    // Next item must be Err(ProviderError::StreamDisconnected)
    let second = stream.next().await.unwrap();
    match second {
        Err(ProviderError::StreamDisconnected) => {}
        other => panic!("Expected StreamDisconnected error, got: {:?}", other),
    }
}

// --------------------------------------------------------------------------
// 4. Audit B - Upstream Exception 2: 超时 (Timeout)
// --------------------------------------------------------------------------
#[tokio::test]
async fn test_audit_b_upstream_timeout() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_millis(500))
                .set_body_raw("data: [DONE]\n\n", "text/event-stream"),
        )
        .mount(&mock_server)
        .await;

    let client = reqwest::Client::new();
    let config = ProviderConfig {
        base_url: mock_server.uri(),
        api_key: "key".to_string(),
        model: "m".to_string(),
        timeout: Duration::from_millis(100), // very short timeout
        group_id: None,
    };

    let req = create_test_request("m");
    let res = OpenAiProvider.chat_stream(&client, &config, &req).await;

    match res {
        Err(ProviderError::Timeout) => {}
        Err(e) => panic!("Expected Timeout error, got different error: {:?}", e),
        Ok(_) => panic!("Expected Timeout error, but got Ok stream"),
    }
}

// --------------------------------------------------------------------------
// 5. Audit B - Upstream Exception 3: 非 200 (Non-200 HTTP error)
// --------------------------------------------------------------------------
#[tokio::test]
async fn test_audit_b_upstream_non_200_error() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(429).set_body_json(serde_json::json!({
            "error": {
                "message": "Rate limit reached for requests per minute",
                "type": "rate_limit_exceeded"
            }
        })))
        .mount(&mock_server)
        .await;

    let client = reqwest::Client::new();
    let config = ProviderConfig {
        base_url: mock_server.uri(),
        api_key: "key".to_string(),
        model: "m".to_string(),
        timeout: Duration::from_secs(5),
        group_id: None,
    };

    let req = create_test_request("m");
    let res = OpenAiProvider.chat_stream(&client, &config, &req).await;

    match res {
        Err(ProviderError::Http(status, body)) => {
            assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
            assert!(body.contains("rate_limit_exceeded"));
        }
        Err(e) => panic!("Expected Http error, got different error: {:?}", e),
        Ok(_) => panic!("Expected Http error, but got Ok stream"),
    }
}

// --------------------------------------------------------------------------
// 6. Audit B - Upstream Exception 4: 无 usage (No usage reported)
// --------------------------------------------------------------------------
#[tokio::test]
async fn test_audit_b_upstream_no_usage_reported() {
    let mock_server = MockServer::start().await;

    // Stream with text only, without usage chunk
    let sse_body = [
        r#"data: {"id":"1","choices":[{"delta":{"content":"Response without usage statistics"}}]}"#,
        "data: [DONE]",
        "",
    ]
    .join("\n\n");

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse_body, "text/event-stream"))
        .mount(&mock_server)
        .await;

    let client = reqwest::Client::new();
    let config = ProviderConfig {
        base_url: mock_server.uri(),
        api_key: "key".to_string(),
        model: "m".to_string(),
        timeout: Duration::from_secs(5),
        group_id: None,
    };

    let req = create_test_request("m");
    let mut stream = OpenAiProvider
        .chat_stream(&client, &config, &req)
        .await
        .unwrap();

    let mut events = Vec::new();
    while let Some(res) = stream.next().await {
        events.push(res.expect("should not fail"));
    }

    // Must have text delta
    assert!(events.iter().any(|e| matches!(e, ProviderStreamEvent::Delta(ProviderDelta::Text(t)) if t == "Response without usage statistics")));
    // Must finish with Done
    assert!(events
        .iter()
        .any(|e| matches!(e, ProviderStreamEvent::Done)));
    // Must NOT contain any Usage event, without causing panic or errors
    assert!(!events
        .iter()
        .any(|e| matches!(e, ProviderStreamEvent::Usage(_))));
}

#[test]
fn upstream_in_band_errors_are_not_silently_ignored() {
    for provider in [
        &OpenAiProvider as &dyn ModelProvider,
        &AnthropicProvider as &dyn ModelProvider,
    ] {
        for line in [
            r#"data: {"error":{"message":"private upstream diagnostic","type":"overloaded_error"}}"#,
            r#"data: {"type":"error","error":null}"#,
        ] {
            let error = provider.parse_stream_line(line).unwrap_err();
            assert!(matches!(
                error,
                ProviderError::Http(_, _) | ProviderError::Service
            ));
            assert!(!error.to_string().contains("private upstream diagnostic"));
        }
    }
}

/// Events parsed from an Anthropic stream delivered as `chunks` of bytes.
async fn anthropic_events(chunks: Vec<String>) -> Vec<Result<ProviderStreamEvent, ProviderError>> {
    let bytes = futures_util::stream::iter(
        chunks
            .into_iter()
            .map(|chunk| Ok::<_, reqwest::Error>(bytes::Bytes::from(chunk))),
    );
    gateway::provider::process_byte_stream(bytes, |line| AnthropicProvider.parse_stream_line(line))
        .collect()
        .await
}

/// A tool call's whole input in one `input_json_delta` event, streamed in 64 KB chunks.
fn one_event_tool_call(input_bytes: usize) -> Vec<String> {
    let partial = serde_json::to_string(&format!(
        "{{\"path\":\"big.txt\",\"text\":\"{}\"}}",
        "x".repeat(input_bytes)
    ))
    .unwrap();
    let body = [
        r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"tu_big","name":"fsWrite","input":{}}}"#.to_string(),
        format!(r#"data: {{"type":"content_block_delta","index":0,"delta":{{"type":"input_json_delta","partial_json":{partial}}}}}"#),
        r#"data: {"type":"message_stop"}"#.to_string(),
        String::new(),
    ]
    .join("\n\n");
    body.as_bytes()
        .chunks(64 * 1024)
        .map(|chunk| String::from_utf8(chunk.to_vec()).unwrap())
        .collect()
}

// Some providers send a tool call's whole input in a single event. One carrying a large
// file is a healthy response, up to what the stream accepts for tool calls at all.
#[tokio::test]
async fn a_large_single_event_does_not_abort_the_stream() {
    let events = anthropic_events(one_event_tool_call(3 * 1024 * 1024)).await;
    assert!(
        events.iter().all(Result::is_ok),
        "{:?}",
        events.iter().find(|event| event.is_err())
    );
    let arguments: usize = events
        .iter()
        .filter_map(|event| match event {
            Ok(ProviderStreamEvent::Delta(ProviderDelta::ToolCallChunk { arguments, .. })) => {
                Some(arguments.len())
            }
            _ => None,
        })
        .sum();
    assert!(arguments > 3 * 1024 * 1024);
    assert!(matches!(events.last(), Some(Ok(ProviderStreamEvent::Done))));

    // The ceiling stays: an event beyond it ends the stream with an error.
    let events = anthropic_events(one_event_tool_call(40 * 1024 * 1024)).await;
    assert!(matches!(
        events.last(),
        Some(Err(ProviderError::Parse(message))) if message.contains("exceeded limit")
    ));
}

/// Kiro sends each tool's JSON Schema wrapped as `inputSchema.json`. Both providers must hand the
/// upstream the schema itself: given `{"json": {...}}`, a model sees no parameters and guesses
/// its own names, and Kiro refuses the call (fs_write with file_path/content, not path/text).
#[test]
fn kiro_tool_schemas_reach_the_upstream_unwrapped() {
    let schema = serde_json::json!({
        "type": "object",
        "properties": {"path": {"type": "string"}, "text": {"type": "string"}},
        "required": ["path", "text"],
    });
    let mut req = create_test_request("model");
    req.tools = vec![
        serde_json::json!({"toolSpecification": {"name": "fs_write", "description": "Write a file", "inputSchema": {"json": schema}}}),
        serde_json::json!({"toolSpecification": {"name": "fs_read", "description": "Read a file", "inputSchema": schema}}),
    ];

    let anthropic = AnthropicProvider.translate_request(&req).unwrap();
    assert_eq!(anthropic["tools"][0]["name"], "fs_write");
    assert_eq!(anthropic["tools"][0]["input_schema"], schema);
    assert_eq!(
        anthropic["tools"][1]["input_schema"], schema,
        "an unwrapped schema is kept as it is"
    );

    let openai = OpenAiProvider.translate_request(&req).unwrap();
    assert_eq!(openai["tools"][0]["function"]["parameters"], schema);
    assert_eq!(openai["tools"][1]["function"]["parameters"], schema);
}

#[test]
fn a_thinking_signature_reaches_the_stream_and_replays_only_to_its_model() {
    use gateway::provider::{ProviderOptions, ThinkingBlock};
    let events = AnthropicProvider
        .parse_stream_line(
            r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"EqQBCgIYAh"}}"#,
        )
        .unwrap();
    assert_eq!(
        events,
        vec![ProviderStreamEvent::Delta(
            ProviderDelta::ReasoningSignature("EqQBCgIYAh".into())
        )]
    );

    let mut assistant = ChatMessage::new("assistant", serde_json::json!("Done."));
    assistant.thinking = Some(ThinkingBlock {
        text: "Checking the file.".into(),
        signature: "EqQBCgIYAh".into(),
        model: "claude-opus-5-5".into(),
    });
    let request = |model: &str| ChatRequest {
        reasoning_effort: None,
        model: model.into(),
        messages: vec![
            ChatMessage::new("user", serde_json::json!("check it")),
            assistant.clone(),
            ChatMessage::new("user", serde_json::json!("and now?")),
        ],
        temperature: None,
        max_tokens: Some(8192),
        stream: true,
        tools: vec![],
    };
    let replay = ProviderOptions {
        replay_thinking: true,
        ..Default::default()
    };
    let first_block = |body: serde_json::Value| body["messages"][1]["content"][0].clone();

    let body = AnthropicProvider
        .translate_request_with(&request("claude-opus-5-5"), &replay)
        .unwrap();
    assert_eq!(
        first_block(body),
        serde_json::json!({"type": "thinking", "thinking": "Checking the file.", "signature": "EqQBCgIYAh"})
    );
    // Off by default, and never to another model.
    for (model, options) in [
        ("claude-opus-5-5", ProviderOptions::default()),
        ("claude-opus-5", replay),
    ] {
        let body = AnthropicProvider
            .translate_request_with(&request(model), &options)
            .unwrap();
        assert_eq!(first_block(body)["type"], "text", "{model}");
    }
    // OpenAI has no thinking blocks to take back.
    let body = OpenAiProvider
        .translate_request(&request("claude-opus-5-5"))
        .unwrap();
    assert!(!body.to_string().contains("EqQBCgIYAh"));
}

/// Every upstream line proves it alive to the watchdogs: a model thinking silently, or
/// buffering a tool input, sends pings and empty thinking deltas for minutes.
#[tokio::test]
async fn pings_and_empty_deltas_count_as_liveness() {
    let sse = [
        "event: message_start",
        r#"data: {"type":"message_start","message":{"usage":{"input_tokens":5,"output_tokens":1}}}"#,
        "",
        ": keepalive",
        "event: ping",
        r#"data: {"type":"ping"}"#,
        "",
        r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#,
        r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":""}}"#,
        r#"data: {"type":"message_stop"}"#,
        "",
    ]
    .join("\n");
    let events: Vec<_> = gateway::provider::process_byte_stream(
        futures_util::stream::iter(vec![Ok::<_, reqwest::Error>(bytes::Bytes::from(sse))]),
        |line| AnthropicProvider.parse_stream_line(line),
    )
    .map(Result::unwrap)
    .collect()
    .await;
    assert_eq!(events[0], ProviderStreamEvent::Started);
    assert!(matches!(events[1], ProviderStreamEvent::Usage(_)));
    // The comment, the ping's data line and the empty delta; never an `event:` line.
    let heartbeats = events
        .iter()
        .filter(|event| **event == ProviderStreamEvent::Heartbeat)
        .count();
    assert_eq!(heartbeats, 3, "{events:?}");
    // A thinking block's start is the model at work too.
    assert_eq!(
        events
            .iter()
            .filter(|event| **event == ProviderStreamEvent::Started)
            .count(),
        2
    );
    assert_eq!(events.last(), Some(&ProviderStreamEvent::Done));

    // OpenAI's opening chunk names the role.
    let opening = OpenAiProvider
        .parse_stream_line(r#"data: {"choices":[{"delta":{"role":"assistant","content":""}}]}"#)
        .unwrap();
    assert_eq!(opening, vec![ProviderStreamEvent::Started]);
}

#[test]
fn eager_tool_input_is_asked_for_only_where_it_is_switched_on() {
    use gateway::provider::ProviderOptions;
    let request = ChatRequest {
        reasoning_effort: None,
        model: "claude-opus-5-5".into(),
        messages: vec![ChatMessage::new("user", serde_json::json!("write it"))],
        temperature: None,
        max_tokens: Some(8192),
        stream: true,
        tools: vec![serde_json::json!({"toolSpecification": {
            "name": "fsWrite",
            "description": "Write a file",
            "inputSchema": {"json": {"type": "object", "properties": {"path": {"type": "string"}}}}
        }})],
    };
    let body = AnthropicProvider
        .translate_request_with(&request, &ProviderOptions::default())
        .unwrap();
    assert!(body["tools"][0].get("eager_input_streaming").is_none());
    let body = AnthropicProvider
        .translate_request_with(
            &request,
            &ProviderOptions {
                eager_tool_input: true,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(body["tools"][0]["eager_input_streaming"], true);
    assert_eq!(body["tools"][0]["name"], "fsWrite");
}

/// Every agent step resends the same tools, system prompt and history. Breakpoints after
/// the tools, the system prompt and the last two user turns let the next step read that
/// prefix from the cache, at a tenth of the input price.
#[test]
fn prompt_cache_breakpoints_follow_the_request_shape() {
    use gateway::provider::ProviderOptions;
    let tool = |name: &str| {
        serde_json::json!({"toolSpecification": {"name": name, "description": "d",
            "inputSchema": {"json": {"type": "object"}}}})
    };
    let step = |turns: usize| {
        let mut messages = vec![
            ChatMessage::new("system", serde_json::json!("Workspace rules.")),
            ChatMessage::new("user", serde_json::json!("<kiro system prompt>")),
        ];
        for turn in 0..turns {
            let mut call = ChatMessage::new("assistant", serde_json::json!(""));
            call.tool_calls = vec![gateway::provider::ToolCallEntry {
                id: format!("t{turn}"),
                name: "readFile".into(),
                arguments: serde_json::json!({"path": format!("f{turn}")}),
            }];
            let mut result = ChatMessage::new("tool", serde_json::json!(format!("file {turn}")));
            result.tool_call_id = Some(format!("t{turn}"));
            messages.extend([call, result]);
        }
        ChatRequest {
            reasoning_effort: None,
            model: "claude-opus-5-5".into(),
            messages,
            temperature: None,
            max_tokens: Some(8192),
            stream: true,
            tools: vec![tool("readFile"), tool("fsWrite")],
        }
    };
    let marked = |value: &serde_json::Value| value.to_string().matches("cache_control").count();

    let body = AnthropicProvider.translate_request(&step(2)).unwrap();
    assert_eq!(body["tools"][1]["cache_control"]["type"], "ephemeral");
    assert!(body["tools"][0].get("cache_control").is_none());
    assert_eq!(body["system"][0]["text"], "Workspace rules.");
    assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");
    let messages = body["messages"].as_array().unwrap();
    let last = messages.len() - 1;
    assert_eq!(
        messages[last]["content"][0]["cache_control"]["type"],
        "ephemeral"
    );
    assert_eq!(
        messages[last - 2]["content"][0]["cache_control"]["type"],
        "ephemeral"
    );
    assert_eq!(marked(&body), 4, "at most four breakpoints");

    // The next step's prefix is this step's, byte for byte, bar the moving breakpoints.
    let strip = |mut value: serde_json::Value| {
        fn walk(value: &mut serde_json::Value) {
            match value {
                serde_json::Value::Object(map) => {
                    map.remove("cache_control");
                    map.values_mut().for_each(walk);
                }
                serde_json::Value::Array(items) => items.iter_mut().for_each(walk),
                _ => {}
            }
        }
        walk(&mut value);
        value
    };
    let next = AnthropicProvider.translate_request(&step(3)).unwrap();
    let next_messages = next["messages"].as_array().unwrap();
    assert_eq!(
        strip(serde_json::Value::Array(messages.clone())),
        strip(serde_json::Value::Array(
            next_messages[..messages.len()].to_vec()
        ))
    );
    // The breakpoint this step read up to is where the last step wrote one.
    assert_eq!(
        next_messages[messages.len() - 1]["content"][0]["cache_control"]["type"],
        "ephemeral"
    );
    assert_eq!(strip(next["tools"].clone()), strip(body["tools"].clone()));

    // Off for an upstream that refuses them.
    let off = AnthropicProvider
        .translate_request_with(
            &step(2),
            &ProviderOptions {
                prompt_cache: false,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(marked(&off), 0);
    assert_eq!(off["system"], "Workspace rules.");
    // OpenAI caches prefixes by itself.
    assert_eq!(
        marked(&OpenAiProvider.translate_request(&step(2)).unwrap()),
        0
    );
}

/// Kiro's turn that carries tool results has no text of its own. The OpenAI format has no
/// error flag for a tool result, and an empty user message after the results is one some
/// models answer ("your message seems to be empty").
#[test]
fn openai_tool_results_carry_their_failure_and_no_empty_turn_follows() {
    let mut assistant = ChatMessage::new("assistant", serde_json::json!(""));
    assistant.tool_calls = vec![
        gateway::provider::ToolCallEntry {
            id: "call-ok".into(),
            name: "read".into(),
            arguments: serde_json::json!({"path": "a.rs"}),
        },
        gateway::provider::ToolCallEntry {
            id: "call-bad".into(),
            name: "read".into(),
            arguments: serde_json::json!({"path": "missing.rs"}),
        },
    ];
    let mut ok = ChatMessage::new("tool", serde_json::json!("fn main() {}"));
    ok.tool_call_id = Some("call-ok".into());
    ok.is_error = Some(false);
    let mut failed = ChatMessage::new("tool", serde_json::json!("No such file: missing.rs"));
    failed.tool_call_id = Some("call-bad".into());
    failed.is_error = Some(true);
    let mut req = create_test_request("gpt-4o");
    req.messages.extend([
        assistant,
        ok,
        failed,
        ChatMessage::new("user", serde_json::json!("")),
    ]);

    let body = OpenAiProvider.translate_request(&req).unwrap();
    let messages = body["messages"].as_array().unwrap();
    let last = messages.last().unwrap();
    assert_eq!(last["role"], "tool", "{messages:?}");
    assert!(last["content"]
        .as_str()
        .unwrap()
        .starts_with("[工具调用失败 / tool call failed]\nNo such file"));
    let succeeded = &messages[messages.len() - 2];
    assert_eq!(succeeded["content"], "fn main() {}");
    // A user turn with words of its own is kept.
    assert!(messages.iter().any(|m| m["content"] == "Hello!"));
}
