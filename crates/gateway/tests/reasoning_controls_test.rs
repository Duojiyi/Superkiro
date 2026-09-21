use gateway::provider::{anthropic::AnthropicProvider, openai::OpenAiProvider, ModelProvider};
use gateway::translate::to_provider::{translate_kiro_to_chat_request, TranslationContext};
use kiro_wire::requests::conversation::{GenerateAssistantResponseRequest, ReasoningEffort};
use serde_json::{json, Value};

fn wire() -> Value {
    json!({"conversationState":{"conversationId":"controls-test", "currentMessage":{"userInputMessage":{"content":"hello"}}}})
}
fn translated(value: Value, model: &str) -> gateway::provider::ChatRequest {
    let request: GenerateAssistantResponseRequest = serde_json::from_value(value).unwrap();
    translate_kiro_to_chat_request(&request, &mut TranslationContext::new(model))
}
#[test]
fn all_four_wire_locations_reach_provider() {
    for path in ["/additionalModelRequestFields", "/conversationState/additionalModelRequestFields",
        "/conversationState/currentMessage/userInputMessage/additionalModelRequestFields",
        "/conversationState/currentMessage/userInputMessage/userInputMessageContext/additionalModelRequestFields"] {
        let mut value = wire();
        value["conversationState"]["currentMessage"]["userInputMessage"]["userInputMessageContext"] = json!({});
        let (parent, key) = path.rsplit_once('/').unwrap();
        value.pointer_mut(parent).unwrap()[key] = json!({"output_config":{"effort":"high"}, "untrusted_secret":"ignored"});
        let request = translated(value, "reasoning-model");
        assert_eq!(request.reasoning_effort, Some(ReasoningEffort::High));
        let body = OpenAiProvider.translate_request(&request).unwrap();
        assert_eq!(body["reasoning_effort"], "high");
        assert!(body.get("temperature").is_none());
        assert!(body.get("untrusted_secret").is_none());
    }
}
#[test]
fn precedence_alias_and_invalid_effort() {
    let mut value = wire();
    value["additionalModelRequestFields"] = json!({"reasoning":{"effort":"low"}});
    assert_eq!(
        translated(value.clone(), "model").reasoning_effort,
        Some(ReasoningEffort::Low)
    );
    value["conversationState"]["currentMessage"]["userInputMessage"]
        ["additionalModelRequestFields"] = json!({"output_config":{"effort":"max"}});
    assert_eq!(
        translated(value.clone(), "model").reasoning_effort,
        Some(ReasoningEffort::Max)
    );
    value["additionalModelRequestFields"] = json!({"output_config":{"effort":"arbitrary"}});
    assert!(serde_json::from_value::<GenerateAssistantResponseRequest>(value).is_err());
}
#[test]
fn legacy_budget_respects_reserved_output_and_adaptive_models_use_effort() {
    for (name, expected) in [
        ("low", 1024),
        ("medium", 4096),
        ("high", 8192),
        ("xhigh", 16384),
        ("max", 24576),
    ] {
        let mut value = wire();
        value["additionalModelRequestFields"] = json!({"output_config":{"effort":name}});
        let mut request = translated(value, "claude-sonnet-4-5");
        request.max_tokens = Some(32000);
        let body = AnthropicProvider.translate_request(&request).unwrap();
        assert_eq!(body["thinking"]["budget_tokens"], expected);
        assert!(body.get("temperature").is_none());
        request.max_tokens = Some(2048);
        assert!(
            AnthropicProvider.translate_request(&request).unwrap()["thinking"]["budget_tokens"]
                .as_u64()
                .unwrap()
                < 2048
        );
        request.max_tokens = Some(1024);
        assert!(AnthropicProvider.translate_request(&request).is_err());
        request.max_tokens = Some(32000);
        request.model = "claude-sonnet-4-6".into();
        let adaptive = AnthropicProvider.translate_request(&request).unwrap();
        assert_eq!(adaptive["thinking"]["type"], "adaptive");
        assert!(adaptive["thinking"].get("budget_tokens").is_none());
        assert_ne!(adaptive["output_config"]["effort"], "max");
    }
}
#[test]
fn no_effort_preserves_existing_requests() {
    let request = translated(wire(), "plain-model");
    for body in [
        AnthropicProvider.translate_request(&request).unwrap(),
        OpenAiProvider.translate_request(&request).unwrap(),
    ] {
        assert!(body.get("thinking").is_none());
        assert!(body.get("reasoning_effort").is_none());
        assert!(body.get("temperature").is_some());
    }
}

#[test]
fn independent_system_prompt_survives_translation() {
    let mut value = wire();
    value["systemPrompt"] = json!("Use the supplied workspace instructions.");
    let request = translated(value, "model");
    assert_eq!(request.messages[0].role, "system");
    assert_eq!(
        AnthropicProvider.translate_request(&request).unwrap()["system"],
        "Use the supplied workspace instructions."
    );
    assert_eq!(
        OpenAiProvider.translate_request(&request).unwrap()["messages"][0]["content"],
        "Use the supplied workspace instructions."
    );
}

#[test]
fn high_output_budget_includes_thinking_without_inflation() {
    let mut value = wire();
    value["additionalModelRequestFields"] = json!({"output_config":{"effort":"max"}});
    let mut request = translated(value, "claude-sonnet-4-5");
    request.max_tokens = Some(128_000);
    let body = AnthropicProvider.translate_request(&request).unwrap();
    assert_eq!(body["max_tokens"], 128_000);
    assert_eq!(body["thinking"]["budget_tokens"], 24_576);
    assert_eq!(
        OpenAiProvider.translate_request(&request).unwrap()["max_tokens"],
        128_000
    );
}
