use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use gateway::provider::anthropic::AnthropicProvider;
use gateway::provider::openai::OpenAiProvider;
use gateway::provider::{
    ChatMessage, ChatRequest, ModelProvider, ProviderDelta, ProviderStreamEvent, TokenUsage,
    ToolCallEntry,
};
use gateway::translate::{
    maybe_shrink_image, process_tools_for_provider, repair_orphan_tool_pairs,
    translate_kiro_to_chat_request, translate_provider_event_to_frames, validate_image_input,
    ImageValidationError, StreamTranslationState, ToolRegistry, TranslationContext,
};
use image::{ImageBuffer, Rgb};
use kiro_wire::requests::conversation::GenerateAssistantResponseRequest;
use std::fs;
use std::io::Cursor;
use std::path::PathBuf;

fn sample_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("docs")
        .join("p0")
        .join("samples")
}

// --------------------------------------------------------------------------
// 1. P0 Sample Verification
// --------------------------------------------------------------------------
#[test]
fn test_translate_p0_conversation_sample() {
    let sample_path = sample_dir().join("conversation_request_with_tools_and_images.json");
    let json_bytes = fs::read(&sample_path).expect("Read P0 conversation sample");

    let kiro_req: GenerateAssistantResponseRequest =
        serde_json::from_slice(&json_bytes).expect("Deserialize P0 sample");

    let mut ctx = TranslationContext::new("claude-3-7-sonnet");
    let chat_req = translate_kiro_to_chat_request(&kiro_req, &mut ctx);

    assert_eq!(chat_req.model, "claude-3-7-sonnet");
    assert!(!chat_req.messages.is_empty());
    // Tools were present in P0 sample
    assert!(!chat_req.tools.is_empty());

    // Current message content has prompt and image notice
    let user_msg = chat_req
        .messages
        .iter()
        .find(|m| m.role == "user")
        .expect("Must have user message");
    assert!(user_msg
        .content
        .to_string()
        .contains("inspect this architecture screenshot"));
    // BUG-1 fix: vision models now get actual image data as multipart content
    let content_str = user_msg.content.to_string();
    assert!(
        content_str.contains("image_url")
            || content_str.contains("image")
            || content_str.contains("base64"),
        "Vision model must receive actual image data, got: {}...",
        &content_str[..content_str.len().min(200)]
    );
}

// --------------------------------------------------------------------------
// 2. Audit B - Counterexample 1: 孤立 tool_result 自动修复
// --------------------------------------------------------------------------
#[test]
fn test_audit_b_orphan_tool_result_repaired() {
    use gateway::translate::tools::ConversationMessage;

    // History has NO assistant tool_use with id "orphan_call_1"
    let messages = vec![
        ConversationMessage {
            role: "user".to_string(),
            content: serde_json::Value::String("Please read the config".to_string()),
            tool_use_id: None,
            tool_calls: vec![],
            is_error: None,
        },
        ConversationMessage {
            role: "tool".to_string(),
            content: serde_json::Value::String("config: key=value".to_string()),
            tool_use_id: Some("orphan_call_1".to_string()),
            tool_calls: vec![],
            is_error: None,
        },
    ];

    let repaired = repair_orphan_tool_pairs(messages);
    // Orphan tool result must be converted to user message, preventing 400 from provider
    assert_eq!(repaired.len(), 2);
    assert_eq!(repaired[1].role, "user");
    assert!(repaired[1]
        .content
        .to_string()
        .contains("[Previous Tool Result]: config: key=value"));
    assert!(repaired[1].tool_use_id.is_none());
}

// --------------------------------------------------------------------------
// 3. Audit B - Counterexample 2: 超长工具名缩短与原样还原往返
// --------------------------------------------------------------------------
#[test]
fn test_audit_b_ultra_long_tool_name_shortening_and_restoration() {
    let long_tool_name = "kiro_builtin_developer__run_terminal_command_with_super_long_arbitrary_suffix_that_definitely_exceeds_sixty_four_characters";
    assert!(long_tool_name.len() > 64);

    let mut registry = ToolRegistry::new();
    let shortened = registry.register(long_tool_name);

    // 1. Safe shortened name for provider (<= 64 chars)
    assert!(shortened.len() <= 64);
    assert!(shortened.starts_with("kiro_builtin_developer__run_term"));

    // 2. Restore when provider streams tool call
    let restored = registry.restore(&shortened);
    assert_eq!(
        restored, long_tool_name,
        "Tool name must restore 100% identically"
    );

    // Also test through from_provider event translation
    let ctx = TranslationContext {
        tool_registry: registry,
        target_model: "test-model".to_string(),
        supports_vision: true,
        image_transcriptions: Vec::new(),
        prepared_images: Default::default(),
    };
    let mut state = StreamTranslationState::new("test-model");

    let chunk_event = ProviderStreamEvent::Delta(ProviderDelta::ToolCallChunk {
        index: Some(0),
        id: Some("call_abc".to_string()),
        name: Some(shortened),
        arguments: "{\"cmd\": \"ls\"}".to_string(),
    });

    let frames = translate_provider_event_to_frames(&chunk_event, &ctx, &mut state);
    assert_eq!(frames.len(), 1);

    let mut decoder = kiro_wire::EventStreamDecoder::new();
    decoder.feed(&frames[0]).unwrap();
    let frame = decoder.decode().unwrap().unwrap();
    assert_eq!(frame.event_type(), Some("toolUseEvent"));
    let payload_str = frame.payload_to_string_lossy();
    assert!(
        payload_str.contains(long_tool_name),
        "EventStream payload must use restored full tool name"
    );
}

// --------------------------------------------------------------------------
// 4. Audit B - Counterexample 3: 超长工具文档挪入 system prompt
// --------------------------------------------------------------------------
#[test]
fn test_audit_b_ultra_long_tool_description_relocation() {
    let long_desc = "A".repeat(1500); // 1500 chars > 1024
    let tool = serde_json::json!({
        "name": "super_tool",
        "description": long_desc,
        "inputSchema": { "type": "object" }
    });

    let mut registry = ToolRegistry::new();
    let (tools, doc_append) = process_tools_for_provider(&[tool], &mut registry);

    assert_eq!(tools.len(), 1);
    let desc = tools[0]["description"].as_str().unwrap();
    assert!(desc.contains("Documentation for super_tool is provided in the system prompt."));
    assert!(desc.len() < 100);

    let doc = doc_append.expect("Must produce system documentation append");
    assert!(doc.contains("## Extended Tool Documentation"));
    assert!(doc.contains("super_tool"));
    assert!(doc.contains(&"A".repeat(100)));
}

// --------------------------------------------------------------------------
// 5. Audit B - Counterexample 4: 超大图片下采样与 GIF 保留
// --------------------------------------------------------------------------
#[test]
fn test_audit_b_image_compression_and_gif_preservation() {
    // 1. Create a large 2000x2000 image
    let img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::new(2000, 2000);
    let mut raw_png = Vec::new();
    img.write_to(&mut Cursor::new(&mut raw_png), image::ImageFormat::Png)
        .unwrap();
    let large_b64 = BASE64.encode(&raw_png);

    let result = maybe_shrink_image("png", &large_b64);
    assert_eq!(result.format, "jpeg");
    assert!(result.was_resized);

    // Verify decoded dimensions are <= 1568
    let decoded_bytes = BASE64.decode(&result.base64_data).unwrap();
    let reader = image::ImageReader::new(Cursor::new(&decoded_bytes))
        .with_guessed_format()
        .unwrap();
    let (w, h) = reader.into_dimensions().unwrap();
    assert!(w <= 1568);
    assert!(h <= 1568);

    // 2. GIF must be preserved as-is (animated protection)
    let gif_b64 = BASE64.encode(b"GIF89a_mock_gif_data");
    let gif_result = maybe_shrink_image("gif", &gif_b64);
    assert_eq!(gif_result.format, "gif");
    assert!(!gif_result.was_resized);
    assert_eq!(gif_result.base64_data, gif_b64);
}

// --------------------------------------------------------------------------
// 6. Audit B - Counterexample 5: 空 thinking 容错与非空正常下发
// --------------------------------------------------------------------------
#[test]
fn test_audit_b_empty_thinking_tolerance() {
    let ctx = TranslationContext::new("model");
    let mut state = StreamTranslationState::new("model");

    // Empty thinking delta -> must emit 0 frames (not empty invalid frames)
    let empty_thinking = ProviderStreamEvent::Delta(ProviderDelta::Reasoning("".to_string()));
    let frames = translate_provider_event_to_frames(&empty_thinking, &ctx, &mut state);
    assert_eq!(frames.len(), 0);

    // Non-empty thinking delta -> emits valid reasoningContentEvent
    let valid_thinking = ProviderStreamEvent::Delta(ProviderDelta::Reasoning(
        "Thinking about step 1...".to_string(),
    ));
    let frames2 = translate_provider_event_to_frames(&valid_thinking, &ctx, &mut state);
    assert_eq!(frames2.len(), 1);

    let mut decoder = kiro_wire::EventStreamDecoder::new();
    decoder.feed(&frames2[0]).unwrap();
    let frame = decoder.decode().unwrap().unwrap();
    assert_eq!(frame.event_type(), Some("reasoningContentEvent"));
    assert!(frame
        .payload_to_string_lossy()
        .contains("Thinking about step 1..."));
}

// --------------------------------------------------------------------------
// 7. Stop reason mapping and metadata event translation
// --------------------------------------------------------------------------
#[test]
fn test_stop_reason_and_usage_translation() {
    let ctx = TranslationContext::new("claude-3-7-sonnet");
    let mut state = StreamTranslationState::new("claude-3-7-sonnet");

    let usage_ev = ProviderStreamEvent::Usage(TokenUsage {
        uncached_prompt_tokens: 90,
        prompt_tokens: 150,
        completion_tokens: 45,
        total_tokens: 195,
        output_tokens_final: true,
        prompt_final: false,
        cache_read_input_tokens: Some(60),
        cache_creation_input_tokens: None,
    });
    let stop_ev = ProviderStreamEvent::StopReason("stop".to_string());
    let done_ev = ProviderStreamEvent::Done;

    translate_provider_event_to_frames(&usage_ev, &ctx, &mut state);
    translate_provider_event_to_frames(&stop_ev, &ctx, &mut state);
    let final_frames = translate_provider_event_to_frames(&done_ev, &ctx, &mut state);

    assert_eq!(final_frames.len(), 1);
    let mut decoder = kiro_wire::EventStreamDecoder::new();
    decoder.feed(&final_frames[0]).unwrap();
    let frame = decoder.decode().unwrap().unwrap();

    assert_eq!(frame.event_type(), Some("metadataEvent"));
    let payload = frame.payload_to_string_lossy();
    assert!(payload.contains("\"stopReason\":\"end_turn\""));
    assert!(payload.contains("\"uncachedInputTokens\":90"));
    assert!(payload.contains("\"outputTokens\":45"));
    assert!(payload.contains("\"cacheReadInputTokens\":60"));
}

// --------------------------------------------------------------------------
// 8. T05 - Normalized Token Usage Fixtures (Anthropic & OpenAI)
// --------------------------------------------------------------------------
#[test]
fn test_t05_official_usage_normalization() {
    let anthropic = AnthropicProvider;
    let openai = OpenAiProvider;

    // Anthropic official fixture:
    // input_tokens=100 (uncached only), cache_read=200, cache_creation=50, output_tokens=20
    let anthropic_fixture = serde_json::json!({
        "usage": {
            "input_tokens": 100,
            "cache_read_input_tokens": 200,
            "cache_creation_input_tokens": 50,
            "output_tokens": 20
        }
    });
    let anthropic_usage = anthropic
        .extract_usage(&anthropic_fixture)
        .expect("Anthropic usage");
    assert_eq!(anthropic_usage.uncached_prompt_tokens, 100);
    assert_eq!(anthropic_usage.prompt_tokens, 350); // 100 + 200 + 50
    assert_eq!(anthropic_usage.completion_tokens, 20);
    assert_eq!(anthropic_usage.total_tokens, 370);
    assert_eq!(anthropic_usage.cache_read_input_tokens, Some(200));
    assert_eq!(anthropic_usage.cache_creation_input_tokens, Some(50));

    // OpenAI official fixture:
    // prompt_tokens=350 (includes cached), prompt_tokens_details.cached_tokens=200, completion_tokens=20
    let openai_fixture = serde_json::json!({
        "usage": {
            "prompt_tokens": 350,
            "completion_tokens": 20,
            "total_tokens": 370,
            "prompt_tokens_details": {
                "cached_tokens": 200
            }
        }
    });
    let openai_usage = openai.extract_usage(&openai_fixture).expect("OpenAI usage");
    assert_eq!(openai_usage.uncached_prompt_tokens, 150); // 350 - 200
    assert_eq!(openai_usage.prompt_tokens, 350);
    assert_eq!(openai_usage.completion_tokens, 20);
    assert_eq!(openai_usage.total_tokens, 370);
    assert_eq!(openai_usage.cache_read_input_tokens, Some(200));

    // Anthropic stream usage merge (message_start + message_delta):
    let mut initial_usage = anthropic
        .extract_usage(&serde_json::json!({
            "usage": {
                "input_tokens": 100,
                "cache_read_input_tokens": 200,
                "cache_creation_input_tokens": 50,
                "output_tokens": 1
            }
        }))
        .unwrap();

    let delta_events = anthropic
        .parse_stream_line(
            "data: {\"type\": \"message_delta\", \"usage\": {\"output_tokens\": 25}}",
        )
        .unwrap();
    let delta_usage = delta_events
        .into_iter()
        .find_map(|e| match e {
            ProviderStreamEvent::Usage(u) => Some(u),
            _ => None,
        })
        .expect("Must parse usage from message_delta");

    initial_usage.merge(&delta_usage);
    // Verify prompt tokens and cache stats are preserved, not zeroed
    assert_eq!(initial_usage.uncached_prompt_tokens, 100);
    assert_eq!(initial_usage.prompt_tokens, 350);
    assert_eq!(initial_usage.completion_tokens, 25);
    assert_eq!(initial_usage.total_tokens, 375);
    assert_eq!(initial_usage.cache_read_input_tokens, Some(200));
    assert_eq!(initial_usage.cache_creation_input_tokens, Some(50));
}

#[test]
fn a_restated_prompt_in_message_delta_replaces_the_message_start_estimate() {
    let anthropic = AnthropicProvider;
    let merged = |start: serde_json::Value, end: serde_json::Value| {
        let usage_of = |event: serde_json::Value| {
            anthropic
                .parse_stream_line(&format!("data: {event}"))
                .unwrap()
                .into_iter()
                .find_map(|e| match e {
                    ProviderStreamEvent::Usage(u) => Some(u),
                    _ => None,
                })
                .expect("usage")
        };
        let mut usage = usage_of(serde_json::json!({
            "type": "message_start", "message": {"usage": start}}));
        usage.merge(&usage_of(serde_json::json!({
            "type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": end})));
        (
            usage.uncached_prompt_tokens,
            usage.cache_creation_input_tokens,
            usage.cache_read_input_tokens,
            usage.prompt_tokens,
            usage.completion_tokens,
        )
    };

    // Two real relay requests: the counts restated at the end are the ones billed,
    // whether they are above the opening estimate or below it.
    assert_eq!(
        merged(
            serde_json::json!({"input_tokens": 4, "cache_creation_input_tokens": 50_366,
                "cache_read_input_tokens": 144_948, "output_tokens": 1}),
            serde_json::json!({"input_tokens": 22_943, "cache_creation_input_tokens": 67_994,
                "cache_read_input_tokens": 195_680, "output_tokens": 907}),
        ),
        (22_943, Some(67_994), Some(195_680), 286_617, 907)
    );
    assert_eq!(
        merged(
            serde_json::json!({"input_tokens": 1_679, "cache_creation_input_tokens": 0,
                "cache_read_input_tokens": 0, "output_tokens": 1}),
            serde_json::json!({"input_tokens": 43, "cache_creation_input_tokens": 9_038,
                "cache_read_input_tokens": 0, "output_tokens": 1}),
        ),
        (43, Some(9_038), Some(0), 9_081, 1)
    );

    let start = serde_json::json!({"input_tokens": 100, "cache_read_input_tokens": 200,
        "cache_creation_input_tokens": 50, "output_tokens": 1});
    // A restatement of zeros is a placeholder: the opening counts stand.
    assert_eq!(
        merged(
            start.clone(),
            serde_json::json!({"input_tokens": 0, "cache_creation_input_tokens": 0,
                "cache_read_input_tokens": 0, "output_tokens": 25}),
        ),
        (100, Some(50), Some(200), 350, 25)
    );
    // A cache count the restatement leaves out keeps its opening value.
    assert_eq!(
        merged(
            start,
            serde_json::json!({"input_tokens": 120, "cache_read_input_tokens": null,
                "output_tokens": 25}),
        ),
        (120, Some(50), Some(200), 370, 25)
    );
}

// --------------------------------------------------------------------------
// 9. T05 - Multi-turn tool calls, results, images & role alternation
// --------------------------------------------------------------------------
#[test]
fn test_t05_multi_turn_payload_translation() {
    let anthropic = AnthropicProvider;
    let openai = OpenAiProvider;

    let tool_def = serde_json::json!({
        "toolSpecification": {
            "name": "bash_tool",
            "description": "Execute bash command",
            "input": {
                "json": {
                    "type": "object",
                    "properties": {
                        "command": { "type": "string" }
                    },
                    "required": ["command"]
                }
            }
        }
    });

    let chat_req = ChatRequest {
        reasoning_effort: None,
        model: "test-model".to_string(),
        messages: vec![
            ChatMessage::new("system", serde_json::json!("You are an AI assistant.")),
            ChatMessage::new("user", serde_json::json!("List files")),
            ChatMessage {
                role: "assistant".to_string(),
                content: serde_json::json!("Running bash:"),
                name: None,
                tool_call_id: None,
                tool_calls: vec![ToolCallEntry {
                    id: "tool_call_01".to_string(),
                    name: "bash_tool".to_string(),
                    arguments: serde_json::json!({ "command": "ls -la" }),
                }],
                is_error: None,
            },
            ChatMessage {
                role: "tool".to_string(),
                content: serde_json::json!("file1.txt\nfile2.png"),
                name: None,
                tool_call_id: Some("tool_call_01".to_string()),
                tool_calls: vec![],
                is_error: Some(false),
            },
            ChatMessage::new(
                "user",
                serde_json::json!([
                    { "type": "text", "text": "Inspect this image" },
                    { "type": "image_url", "image_url": { "url": "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==" } }
                ]),
            ),
        ],
        temperature: Some(0.7),
        max_tokens: Some(2048),
        stream: true,
        tools: vec![tool_def],
    };

    // 1. Anthropic Translation
    let anthropic_payload = anthropic
        .translate_request(&chat_req)
        .expect("Anthropic translate");
    assert_eq!(anthropic_payload["system"], "You are an AI assistant.");

    let anthropic_tools = anthropic_payload["tools"].as_array().expect("tools array");
    assert_eq!(anthropic_tools.len(), 1);
    assert_eq!(anthropic_tools[0]["name"], "bash_tool");
    assert!(anthropic_tools[0].get("input_schema").is_some());

    let anthropic_msgs = anthropic_payload["messages"]
        .as_array()
        .expect("messages array");
    // Anthropic roles must strictly alternate:
    // Turn 0: user ("List files")
    // Turn 1: assistant (text + tool_use)
    // Turn 2: user (tool_result + merged user text & image!)
    assert_eq!(anthropic_msgs.len(), 3);
    assert_eq!(anthropic_msgs[0]["role"], "user");
    assert_eq!(anthropic_msgs[1]["role"], "assistant");
    assert_eq!(anthropic_msgs[2]["role"], "user");

    // Verify assistant has tool_use block
    let assistant_blocks = anthropic_msgs[1]["content"].as_array().unwrap();
    assert!(assistant_blocks
        .iter()
        .any(|b| b["type"] == "tool_use" && b["id"] == "tool_call_01"));

    // Verify user turn 2 merged tool_result AND image block (converted from image_url!)
    let user_blocks = anthropic_msgs[2]["content"].as_array().unwrap();
    assert!(user_blocks
        .iter()
        .any(|b| b["type"] == "tool_result" && b["tool_use_id"] == "tool_call_01"));
    let img_block = user_blocks
        .iter()
        .find(|b| b["type"] == "image")
        .expect("Must have image block");
    assert_eq!(img_block["source"]["type"], "base64");
    assert_eq!(img_block["source"]["media_type"], "image/png");

    // 2. OpenAI Translation
    let openai_payload = openai
        .translate_request(&chat_req)
        .expect("OpenAI translate");
    let openai_tools = openai_payload["tools"].as_array().expect("tools array");
    assert_eq!(openai_tools.len(), 1);
    assert_eq!(openai_tools[0]["type"], "function");
    assert_eq!(openai_tools[0]["function"]["name"], "bash_tool");

    let openai_msgs = openai_payload["messages"]
        .as_array()
        .expect("messages array");
    let assistant_msg = openai_msgs
        .iter()
        .find(|m| m["role"] == "assistant")
        .expect("assistant msg");
    assert_eq!(assistant_msg["tool_calls"][0]["id"], "tool_call_01");
    assert_eq!(
        assistant_msg["tool_calls"][0]["function"]["name"],
        "bash_tool"
    );

    let tool_msg = openai_msgs
        .iter()
        .find(|m| m["role"] == "tool")
        .expect("tool msg");
    assert_eq!(tool_msg["tool_call_id"], "tool_call_01");
}

// --------------------------------------------------------------------------
// 10. T05 - Pre-decode image validation
// --------------------------------------------------------------------------
#[test]
fn test_t05_image_pre_decode_validation() {
    // 1. Invalid base64
    let res = validate_image_input("png", "this is not valid base64!!!");
    assert!(matches!(res, Err(ImageValidationError::InvalidBase64(_))));

    // 2. Magic byte / MIME mismatch (declares PNG, but bytes are JPEG)
    let fake_png_bytes = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, 0x4A, 0x46]; // JPEG magic
    let fake_b64 = BASE64.encode(&fake_png_bytes);
    let res = validate_image_input("png", &fake_b64);
    assert!(matches!(
        res,
        Err(ImageValidationError::MimeMismatch { .. })
    ));

    // 3. Valid PNG passes validation
    let valid_png_b64 = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";
    let (bytes, fmt, w, h) =
        validate_image_input("png", valid_png_b64).expect("Valid PNG must pass");
    assert_eq!(fmt, "png");
    assert_eq!(w, 1);
    assert_eq!(h, 1);
    assert!(!bytes.is_empty());
}

#[test]
fn openai_usage_without_a_total_saturates() {
    let usage = OpenAiProvider
        .extract_usage(&serde_json::json!({"usage": {
            "prompt_tokens": u64::MAX,
            "completion_tokens": u64::MAX,
        }}))
        .expect("usage");
    assert_eq!(usage.total_tokens, u64::MAX);
}
