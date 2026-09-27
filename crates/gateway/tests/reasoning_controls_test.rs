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
    for (name, budget, effort_4_6) in [
        ("low", 1024, "low"),
        ("medium", 4096, "medium"),
        ("high", 8192, "high"),
        ("xhigh", 16384, "high"),
        // A quarter of the output stays for the answer.
        ("max", 24000, "max"),
    ] {
        let mut value = wire();
        value["additionalModelRequestFields"] = json!({"output_config":{"effort":name}});
        let mut request = translated(value, "claude-sonnet-4-5");
        request.max_tokens = Some(32000);
        let body = AnthropicProvider.translate_request(&request).unwrap();
        assert_eq!(body["thinking"]["budget_tokens"], budget, "{name}");
        assert!(body.get("temperature").is_none());
        assert!(body.get("output_config").is_none());
        request.max_tokens = Some(2048);
        assert_eq!(
            AnthropicProvider.translate_request(&request).unwrap()["thinking"]["budget_tokens"],
            1024
        );
        request.max_tokens = Some(1024);
        assert!(AnthropicProvider.translate_request(&request).is_err());

        // 4.6 thinks adaptively, without xhigh, and summarizes by default.
        request.max_tokens = Some(32000);
        request.model = "claude-sonnet-4-6".into();
        let adaptive = AnthropicProvider.translate_request(&request).unwrap();
        assert_eq!(adaptive["thinking"], json!({"type": "adaptive"}), "{name}");
        assert_eq!(adaptive["output_config"]["effort"], effort_4_6, "{name}");
        assert!(adaptive.get("temperature").is_none());

        // Current models take every level and are asked for a readable summary.
        request.model = "claude-opus-5-5".into();
        let current = AnthropicProvider.translate_request(&request).unwrap();
        assert_eq!(
            current["thinking"],
            json!({"type": "adaptive", "display": "summarized"})
        );
        assert_eq!(current["output_config"]["effort"], name);
        assert!(current.get("temperature").is_none());
        // A tiny output limit is no reason to refuse: adaptive thinking has no budget.
        request.max_tokens = Some(1024);
        assert!(AnthropicProvider.translate_request(&request).is_ok());
    }
}

/// Only a model known to take sampling parameters is sent a temperature; current Claude
/// models refuse one with a 400, and a model the table does not know gets none.
#[test]
fn without_effort_each_family_gets_only_what_it_takes() {
    let body = |model: &str, anthropic: bool| {
        let request = translated(wire(), model);
        if anthropic {
            AnthropicProvider.translate_request(&request).unwrap()
        } else {
            OpenAiProvider.translate_request(&request).unwrap()
        }
    };
    for anthropic in [true, false] {
        let plain = body("plain-model", anthropic);
        assert!(plain.get("thinking").is_none());
        assert!(plain.get("reasoning_effort").is_none());
        assert!(plain.get("temperature").is_none());
    }
    assert!(body("claude-sonnet-4-6", true).get("temperature").is_some());
    assert!(body("claude-haiku-4-5", true).get("temperature").is_some());
    assert!(body("gpt-4o", false).get("temperature").is_some());

    // Opus 4.7 does not think unasked; Opus 5.5 always does, visibly only with a summary.
    let opus_4_7 = body("claude-opus-4-7", true);
    assert!(opus_4_7.get("thinking").is_none() && opus_4_7.get("temperature").is_none());
    let opus_5_5 = body("claude-opus-5-5", true);
    assert_eq!(
        opus_5_5["thinking"],
        json!({"type": "adaptive", "display": "summarized"})
    );
    assert!(opus_5_5.get("output_config").is_none());
    assert!(opus_5_5.get("temperature").is_none());

    // OpenAI reasoning models count output in max_completion_tokens.
    let mut request = translated(wire(), "o3");
    request.max_tokens = Some(8192);
    let o3 = OpenAiProvider.translate_request(&request).unwrap();
    assert_eq!(o3["max_completion_tokens"], 8192);
    assert!(o3.get("max_tokens").is_none() && o3.get("temperature").is_none());
    request.model = "deepseek-chat".into();
    let other = OpenAiProvider.translate_request(&request).unwrap();
    assert_eq!(other["max_tokens"], 8192);
    assert!(other.get("max_completion_tokens").is_none());
}

#[test]
fn independent_system_prompt_survives_translation() {
    let mut value = wire();
    value["systemPrompt"] = json!("Use the supplied workspace instructions.");
    let request = translated(value, "model");
    assert_eq!(request.messages[0].role, "system");
    // A one-shot writes no prompt cache, so the system prompt goes as plain text.
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
