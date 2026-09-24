//! Translate Kiro conversation request into provider-compatible ChatRequest.
//!
//! Spec §4.3 & T05:
//! - Full conversation state assembly (history, current message, tools, images).
//! - Tool name shortening and description relocation.
//! - Images: `prepare_images` shrinks them off the async workers before translation;
//!   translation itself reads headers only and never decodes pixels.
//! - Chronological orphan tool pairing repair without future foresight.
//! - Preservation of tool call names, arguments, and execution statuses in history.

use super::images::{inspect_image, shrink_image, Inspection, Omission, PreparedImage};
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
    /// How each image reaches a vision model, from [`prepare_images`]. An image it does
    /// not cover is judged from its header alone and omitted if it would need shrinking.
    pub prepared_images: PreparedImages,
}

impl TranslationContext {
    pub fn new(target_model: &str) -> Self {
        Self {
            tool_registry: ToolRegistry::new(),
            target_model: target_model.to_string(),
            supports_vision: model_supports_vision(target_model),
            image_transcriptions: Vec::new(),
            prepared_images: PreparedImages::default(),
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

    pub fn with_prepared_images(mut self, prepared: PreparedImages) -> Self {
        self.prepared_images = prepared;
        self
    }
}

/// Where an image sits in the request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Turn {
    History(usize),
    Current,
}

/// How each image of one request reaches the provider, by position.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PreparedImages {
    /// By history index; empty for turns without images.
    history: Vec<Vec<PreparedImage>>,
    current: Vec<PreparedImage>,
}

impl PreparedImages {
    fn get(&self, turn: Turn, index: usize) -> Option<&PreparedImage> {
        match turn {
            Turn::History(position) => self.history.get(position)?.get(index),
            Turn::Current => self.current.get(index),
        }
    }
}

/// Every image of the request, oldest first.
fn images_in_order(req: &GenerateAssistantResponseRequest) -> Vec<(Turn, &KiroImage)> {
    let history = req
        .conversation_state
        .history
        .iter()
        .enumerate()
        .filter_map(|(position, message)| match message {
            Message::User(user) => Some((Turn::History(position), &user.user_input_message)),
            Message::Assistant(_) => None,
        });
    let current = std::iter::once((
        Turn::Current,
        &req.conversation_state.current_message.user_input_message,
    ));
    history
        .chain(current)
        .flat_map(|(turn, input)| input.images.iter().map(move |image| (turn, image)))
        .collect()
}

/// Decide how each image of a request for a vision model reaches the provider.
///
/// Only the most recent `max_images` go as images: a conversation resends every earlier
/// image on every turn, and older ones become notes without being read at all. Of those
/// kept, images too large to forward are shrunk under the process-wide decode limit,
/// newest first, and any not done within `budget` become notes for this turn.
pub async fn prepare_images(
    req: &GenerateAssistantResponseRequest,
    max_images: usize,
    budget: std::time::Duration,
) -> PreparedImages {
    let images = images_in_order(req);
    let keep_from = images.len().saturating_sub(max_images);
    let mut decided = Vec::with_capacity(images.len());
    let mut to_shrink = Vec::new();
    for (position, (_, image)) in images.iter().enumerate() {
        if position < keep_from {
            decided.push(PreparedImage::Omitted(Omission::OverCount));
            continue;
        }
        match inspect_image(&image.source.bytes) {
            Inspection::Ready(prepared) => decided.push(prepared),
            Inspection::NeedsShrink(raw_bytes) => {
                decided.push(PreparedImage::Omitted(Omission::Busy));
                to_shrink.push((position, raw_bytes));
            }
        }
    }
    let deadline = tokio::time::Instant::now() + budget;
    for (position, raw_bytes) in to_shrink.into_iter().rev() {
        decided[position] = shrink_image(raw_bytes, deadline).await;
    }

    let mut prepared = PreparedImages {
        history: vec![Vec::new(); req.conversation_state.history.len()],
        current: Vec::new(),
    };
    for ((turn, _), image) in images.into_iter().zip(decided) {
        match turn {
            Turn::History(position) => prepared.history[position].push(image),
            Turn::Current => prepared.current.push(image),
        }
    }
    prepared
}

fn media_type(format: &str) -> &'static str {
    match format {
        "png" => "image/png",
        "webp" => "image/webp",
        "gif" => "image/gif",
        _ => "image/jpeg",
    }
}

/// The note a vision model reads in place of image `number` of a message.
fn omission_note(number: usize, omission: Omission) -> String {
    let reason = match omission {
        Omission::Unreadable => "图片数据无法读取 / the image data cannot be read",
        Omission::OverCount => {
            "对话中较早的图片不再随每轮发送 / earlier images in the conversation are not resent on every turn"
        }
        Omission::Busy => {
            "网关暂时无法处理这张图片 / the gateway could not process this image in time"
        }
    };
    format!("[图片 #{number} 已省略 / Image #{number} omitted: {reason}]")
}

/// Transcriptions are used only for the current message. They are collected per request
/// for that message alone, so indexing them from a history message would caption a past
/// image with unrelated text.
fn format_user_content(
    content: &str,
    images: &[KiroImage],
    ctx: &TranslationContext,
    turn: Turn,
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

        for (idx, img) in images.iter().enumerate() {
            let prepared = match ctx.prepared_images.get(turn, idx) {
                Some(prepared) => prepared.clone(),
                None => match inspect_image(&img.source.bytes) {
                    Inspection::Ready(prepared) => prepared,
                    Inspection::NeedsShrink(_) => PreparedImage::Omitted(Omission::Busy),
                },
            };
            let url = match prepared {
                PreparedImage::Original { format } => {
                    format!(
                        "data:{};base64,{}",
                        media_type(format),
                        img.source.bytes.trim()
                    )
                }
                PreparedImage::Resized { base64_data } => {
                    format!("data:image/jpeg;base64,{base64_data}")
                }
                PreparedImage::Omitted(omission) => {
                    parts.push(serde_json::json!({
                        "type": "text",
                        "text": omission_note(idx + 1, omission)
                    }));
                    continue;
                }
            };
            parts.push(serde_json::json!({
                "type": "image_url",
                "image_url": { "url": url }
            }));
        }
        serde_json::Value::Array(parts)
    } else {
        let mut text = content.to_string();
        for (idx, img) in images.iter().enumerate() {
            let transcription = if turn == Turn::Current {
                ctx.image_transcriptions.get(idx).map(|s| s.as_str())
            } else {
                None
            };
            // Header only: a text model gets a description, never the pixels.
            let fallback_text = match super::images::validate_image_input("", &img.source.bytes) {
                Ok((raw_bytes, format, width, height)) => format_fallback_description(
                    idx + 1,
                    format,
                    raw_bytes.len(),
                    Some((width, height)),
                    transcription,
                ),
                Err(_) => format_fallback_description(
                    idx + 1,
                    img.format.trim(),
                    img.source.bytes.trim().len() / 4 * 3,
                    None,
                    transcription,
                ),
            };
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
    for (position, history_item) in kiro_req.conversation_state.history.iter().enumerate() {
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
                    let content_val = format_user_content(
                        &user_msg.content,
                        &user_msg.images,
                        ctx,
                        Turn::History(position),
                    );
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

    let final_current_content = format_user_content(
        &current_input.content,
        &current_input.images,
        ctx,
        Turn::Current,
    );
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
