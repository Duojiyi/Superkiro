use kiro_wire::{
    AssistantResponseEvent, ContextUsageEvent, ConversationState, MetadataEvent, MeteringEvent,
    ReasoningContentEvent, ToolUseEvent,
};
use std::fs;
use std::path::PathBuf;

fn samples_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("docs")
        .join("p0")
        .join("samples")
}

#[test]
fn test_deserialize_p0_conversation_sample() {
    let path = samples_dir().join("conversation_request_with_tools_and_images.json");
    let content = fs::read_to_string(&path)
        .expect("Failed to read conversation_request_with_tools_and_images.json");

    #[derive(serde::Deserialize)]
    struct Wrapper {
        #[serde(rename = "conversationState")]
        conversation_state: ConversationState,
    }

    let wrapper: Wrapper = serde_json::from_str(&content).expect("Deserialization failed");
    let state = wrapper.conversation_state;

    assert_eq!(state.conversation_id, "conv_abc123_xyz");
    let user_msg = &state.current_message.user_input_message;
    assert!(user_msg.content.contains("inspect this architecture"));
    assert_eq!(user_msg.model_id.as_deref(), Some("claude-3-7-sonnet"));
    assert_eq!(user_msg.origin.as_deref(), Some("AI_EDITOR"));

    // Images
    assert_eq!(user_msg.images.len(), 1);
    assert_eq!(user_msg.images[0].format, "png");
    assert!(user_msg.images[0].source.bytes.starts_with("iVBORw0KGgo"));

    // Tools
    let ctx = user_msg.user_input_message_context.as_ref().unwrap();
    assert_eq!(ctx.tools.len(), 1);
    assert_eq!(ctx.tools[0].name(), "fs_read");
    assert_eq!(
        ctx.tools[0].description(),
        "Read file contents from filesystem"
    );
    assert!(ctx.tool_results.is_empty());
}

#[test]
fn test_events_deserialization_and_serialization() {
    // 1. AssistantResponseEvent
    let json_assistant = r#"{"content":"Hi there","modelId":"claude-3-7-sonnet"}"#;
    let evt_assistant: AssistantResponseEvent = serde_json::from_str(json_assistant).unwrap();
    assert_eq!(evt_assistant.content, "Hi there");
    assert_eq!(evt_assistant.model_id.as_deref(), Some("claude-3-7-sonnet"));

    // 2. ToolUseEvent
    let json_tool =
        r#"{"name":"fs_read","toolUseId":"call_123","input":"{\"path\":\"a.rs\"}","stop":true}"#;
    let evt_tool: ToolUseEvent = serde_json::from_str(json_tool).unwrap();
    assert_eq!(evt_tool.name, "fs_read");
    assert_eq!(evt_tool.tool_use_id, "call_123");
    assert!(evt_tool.stop);

    // 3. ReasoningContentEvent
    let json_reasoning = r#"{"text":"Thinking step 1","signature":"sig_123"}"#;
    let evt_reasoning: ReasoningContentEvent = serde_json::from_str(json_reasoning).unwrap();
    assert_eq!(evt_reasoning.text.as_deref(), Some("Thinking step 1"));
    assert_eq!(evt_reasoning.signature.as_deref(), Some("sig_123"));

    // 4. ContextUsageEvent
    let json_ctx = r#"{"contextUsagePercentage":0.42}"#;
    let evt_ctx: ContextUsageEvent = serde_json::from_str(json_ctx).unwrap();
    assert_eq!(evt_ctx.context_usage_percentage, 0.42);

    // 5. MetadataEvent
    let json_meta = r#"{"tokenUsage":{"uncachedInputTokens":10,"outputTokens":20,"cacheReadInputTokens":5,"cacheWriteInputTokens":0},"stopReason":"end_turn"}"#;
    let evt_meta: MetadataEvent = serde_json::from_str(json_meta).unwrap();
    let usage = evt_meta.token_usage.unwrap();
    assert_eq!(usage.uncached_input_tokens, 10);
    assert_eq!(usage.output_tokens, 20);
    assert_eq!(usage.cache_read_input_tokens, 5);
    assert_eq!(evt_meta.stop_reason.as_deref(), Some("end_turn"));

    // 6. MeteringEvent
    let json_metering = r#"{"usage":1.5,"unit":"credit","unitPlural":"credits"}"#;
    let evt_metering: MeteringEvent = serde_json::from_str(json_metering).unwrap();
    assert_eq!(evt_metering.usage, 1.5);
    assert_eq!(evt_metering.unit.as_deref(), Some("credit"));
}

#[test]
fn test_serde_tolerance_extra_and_missing_fields() {
    // Missing optional fields (like modelId, stop) defaults cleanly
    let minimal_tool = r#"{"name":"test_tool","toolUseId":"id_001"}"#;
    let parsed_tool: ToolUseEvent = serde_json::from_str(minimal_tool).unwrap();
    assert_eq!(parsed_tool.name, "test_tool");
    assert!(!parsed_tool.stop);
    assert_eq!(parsed_tool.input, "");

    // Extra unknown fields ignored safely without error
    let extra_fields_json = r#"{
        "content": "some text",
        "unknownField1": 12345,
        "unexpectedObject": {"foo": "bar"}
    }"#;
    let parsed_assistant: AssistantResponseEvent = serde_json::from_str(extra_fields_json).unwrap();
    assert_eq!(parsed_assistant.content, "some text");
}
