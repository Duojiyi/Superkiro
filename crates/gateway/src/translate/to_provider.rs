//! Translate Kiro conversation request into provider-compatible ChatRequest.
//!
//! Spec §4.3 & T05:
//! - Full conversation state assembly (history, current message, tools, images).
//! - Tool name shortening and description relocation.
//! - Image compression via images::maybe_shrink_image with pre-decode validation.
//! - Chronological orphan tool pairing repair without future foresight.
//! - Preservation of tool call names, arguments, and execution statuses in history.

use super::images::maybe_shrink_image;
use super::tools::{
    process_tools_for_provider, repair_orphan_tool_pairs, ConversationMessage, ToolRegistry,
};
use crate::provider::{ChatMessage, ChatRequest, ToolCallEntry};
use kiro_wire::requests::conversation::{GenerateAssistantResponseRequest, KiroImage, Message};

use super::vision::{format_fallback_description, model_supports_vision};

pub struct TranslationContext {
    pub tool_registry: ToolRegistry,
    pub target_model: String,
    pub supports_vision: bool,
    pub image_transcriptions: Vec<String>,
}

impl TranslationContext {
    pub fn new(target_model: &str) -> Self {
        Self {
            tool_registry: ToolRegistry::new(),
            target_model: target_model.to_string(),
            supports_vision: model_supports_vision(target_model),
            image_transcriptions: Vec::new(),
        }
    }

    pub fn with_vision_support(mut self, supports_vision: bool) -> Self {
        self.supports_vision = supports_vision;
        self
    }

    pub fn with_image_transcriptions(mut self, transcriptions: Vec<String>) -> Self {
        self.image_transcriptions = transcriptions;
        self
    }
}

fn format_user_content(
    content: &str,
    images: &[KiroImage],
    ctx: &TranslationContext,
) -> serde_json::Value {
    if images.is_empty() {
        return serde_json::Value::String(content.to_string());
    }

    if ctx.supports_vision {
        let mut parts = Vec::new();
        if !content.is_empty() {
            parts.push(serde_json::json!({
                "type": "text",
                "text": content
            }));
        }

        for img in images {
            let shrunk = maybe_shrink_image(&img.format, &img.source.bytes);
            let mime_type = match shrunk.format.as_str() {
                "png" => "image/png",
                "webp" => "image/webp",
                "gif" => "image/gif",
                _ => "image/jpeg",
            };
            parts.push(serde_json::json!({
                "type": "image_url",
                "image_url": {
                    "url": format!("data:{};base64,{}", mime_type, shrunk.base64_data)
                }
            }));
        }
        serde_json::Value::Array(parts)
    } else {
        let mut text = content.to_string();
        for (idx, img) in images.iter().enumerate() {
            let shrunk = maybe_shrink_image(&img.format, &img.source.bytes);
            let transcription = ctx.image_transcriptions.get(idx).map(|s| s.as_str());
            let fallback_text = format_fallback_description(
                idx + 1,
                &shrunk.format,
                shrunk.base64_data.len(),
                shrunk.was_resized,
                transcription,
            );
            text.push_str(&format!("\n{}", fallback_text));
        }
        serde_json::Value::String(text)
    }
}

/// Translate Kiro's `GenerateAssistantResponseRequest` into generic `ChatRequest`.
pub fn translate_kiro_to_chat_request(
    kiro_req: &GenerateAssistantResponseRequest,
    ctx: &mut TranslationContext,
) -> ChatRequest {
    let mut system_prompts: Vec<String> = kiro_req
        .system_prompt
        .iter()
        .filter(|s| !s.trim().is_empty())
        .cloned()
        .collect();
    let mut raw_messages = Vec::new();

    let current_input = &kiro_req
        .conversation_state
        .current_message
        .user_input_message;

    // 1. Process tools and extract long descriptions
    let mut processed_tools = Vec::new();
    if let Some(ref ctx_data) = current_input.user_input_message_context {
        if !ctx_data.tools.is_empty() {
            let tools_value: Vec<serde_json::Value> = ctx_data
                .tools
                .iter()
                .map(|t| serde_json::to_value(t).unwrap_or_default())
                .collect();

            let (tools, doc_append) =
                process_tools_for_provider(&tools_value, &mut ctx.tool_registry);
            processed_tools = tools;
            if let Some(doc) = doc_append {
                system_prompts.push(doc);
            }
        }
    }

    // 2. Process conversation history (including historical tool calls, results, and images)
    for history_item in &kiro_req.conversation_state.history {
        match history_item {
            Message::Assistant(a) => {
                let assistant_msg = &a.assistant_response_message;
                let tool_calls: Vec<ToolCallEntry> = assistant_msg
                    .tool_uses
                    .iter()
                    .map(|tu| ToolCallEntry {
                        id: tu.tool_use_id.clone(),
                        name: ctx.tool_registry.register(&tu.name),
                        arguments: tu.input.clone(),
                    })
                    .collect();

                raw_messages.push(ConversationMessage {
                    role: "assistant".to_string(),
                    content: serde_json::Value::String(assistant_msg.content.clone()),
                    tool_use_id: None,
                    tool_calls,
                    is_error: None,
                });
            }
            Message::User(u) => {
                let user_msg = &u.user_input_message;
                // Historical tool results in user context
                if let Some(ref ctx_data) = user_msg.user_input_message_context {
                    for tr in &ctx_data.tool_results {
                        let content_str = serde_json::to_string(&tr.content).unwrap_or_default();
                        let is_error = tr.status.as_deref().map(|s| s == "error" || s == "failed");
                        raw_messages.push(ConversationMessage {
                            role: "tool".to_string(),
                            content: serde_json::Value::String(content_str),
                            tool_use_id: Some(tr.tool_use_id.clone()),
                            tool_calls: Vec::new(),
                            is_error,
                        });
                    }
                }
                // Historical user message content (with image support)
                if !user_msg.content.is_empty() || !user_msg.images.is_empty() {
                    let content_val = format_user_content(&user_msg.content, &user_msg.images, ctx);
                    raw_messages.push(ConversationMessage {
                        role: "user".to_string(),
                        content: content_val,
                        tool_use_id: None,
                        tool_calls: Vec::new(),
                        is_error: None,
                    });
                }
            }
        }
    }

    // 3. Process current user message: tool results and prompt content
    if let Some(ref ctx_data) = current_input.user_input_message_context {
        for tr in &ctx_data.tool_results {
            let content_str = serde_json::to_string(&tr.content).unwrap_or_default();
            let is_error = tr.status.as_deref().map(|s| s == "error" || s == "failed");
            raw_messages.push(ConversationMessage {
                role: "tool".to_string(),
                content: serde_json::Value::String(content_str),
                tool_use_id: Some(tr.tool_use_id.clone()),
                tool_calls: Vec::new(),
                is_error,
            });
        }
    }

    let final_current_content =
        format_user_content(&current_input.content, &current_input.images, ctx);
    raw_messages.push(ConversationMessage {
        role: "user".to_string(),
        content: final_current_content,
        tool_use_id: None,
        tool_calls: Vec::new(),
        is_error: None,
    });

    // 4. Run strictly chronological orphan repair on conversation messages
    let repaired_messages = repair_orphan_tool_pairs(raw_messages);

    // 5. Compile into final ChatMessage list
    let mut final_messages = Vec::new();

    if !system_prompts.is_empty() {
        final_messages.push(ChatMessage {
            role: "system".to_string(),
            content: serde_json::Value::String(system_prompts.join("\n")),
            name: None,
            tool_call_id: None,
            tool_calls: Vec::new(),
            is_error: None,
        });
    }

    for rm in repaired_messages {
        final_messages.push(ChatMessage {
            role: rm.role,
            content: rm.content,
            name: None,
            tool_call_id: rm.tool_use_id,
            tool_calls: rm.tool_calls,
            is_error: rm.is_error,
        });
    }

    ChatRequest {
        reasoning_effort: kiro_req.reasoning_effort(),
        model: ctx.target_model.clone(),
        messages: final_messages,
        temperature: Some(0.7),
        max_tokens: Some(4096),
        stream: true,
        tools: processed_tools,
    }
}
