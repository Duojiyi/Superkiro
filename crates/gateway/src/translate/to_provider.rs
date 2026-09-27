//! Translate Kiro conversation request into provider-compatible ChatRequest.
//!
//! Spec §4.3 & T05:
//! - Full conversation state assembly (history, current message, tools, images).
//! - Tool name shortening and description relocation.
//! - Images: `prepare_images` shrinks them off the async workers before translation;
//!   translation itself reads headers only and never decodes pixels.
//! - Chronological orphan tool pairing repair without future foresight.
//! - Preservation of tool call names, arguments, and execution statuses in history.

use super::images::{
    image_key, inspect_image, shrink_image_keyed, Inspection, Omission, PreparedImage,
};
use super::tools::{
    process_tools_for_provider, repair_orphan_tool_pairs, ConversationMessage, ToolRegistry,
};
use crate::provider::{ChatMessage, ChatRequest, ThinkingBlock, ToolCallEntry};
use kiro_wire::requests::conversation::{
    GenerateAssistantResponseRequest, KiroDocument, KiroImage, Message,
};

use super::vision::{format_fallback_description, model_supports_vision};

pub struct TranslationContext {
    pub tool_registry: ToolRegistry,
    pub target_model: String,
    pub supports_vision: bool,
    /// One slot per image of the current message; `None` where transcription failed.
    pub image_transcriptions: Vec<Option<String>>,
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

    pub fn with_image_transcriptions(mut self, transcriptions: Vec<Option<String>>) -> Self {
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

/// Every image of the request, oldest first, with its place in its message.
fn images_in_order(req: &GenerateAssistantResponseRequest) -> Vec<(Turn, usize, &KiroImage)> {
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
        .flat_map(|(turn, input)| {
            input
                .images
                .iter()
                .enumerate()
                .map(move |(place, image)| (turn, place, image))
        })
        .collect()
}

/// How long an image left out as busy stays out on its conversation's later steps, from
/// the last step that sent its message: each step then sends what the step before sent.
/// Shown on the next step, it rewrote the prompt a provider had cached from its message
/// on; a conversation resting this long finds that cache expired anyway, and the image is
/// shown then.
const BUSY_KEPT_FOR: std::time::Duration = std::time::Duration::from_secs(5 * 60);

/// At most this many images are kept out as busy at once.
const BUSY_PLACES: usize = 4_096;

/// An image by its conversation, the message it is in (its place in the history, which the
/// current message takes next), its place in that message, and its bytes.
type BusyPlace = (String, usize, usize, [u8; 32]);

fn busy_places(
) -> &'static std::sync::Mutex<std::collections::HashMap<BusyPlace, tokio::time::Instant>> {
    static PLACES: std::sync::LazyLock<
        std::sync::Mutex<std::collections::HashMap<BusyPlace, tokio::time::Instant>>,
    > = std::sync::LazyLock::new(Default::default);
    &PLACES
}

/// Whether the image at `place` was left out as busy on a recent step; if so it stays out
/// for another [`BUSY_KEPT_FOR`].
fn still_busy(place: &BusyPlace, now: tokio::time::Instant) -> bool {
    let Ok(mut places) = busy_places().lock() else {
        return false;
    };
    match places.get_mut(place) {
        Some(until) if *until > now => {
            *until = now + BUSY_KEPT_FOR;
            true
        }
        _ => false,
    }
}

fn keep_busy(place: BusyPlace, now: tokio::time::Instant) {
    let Ok(mut places) = busy_places().lock() else {
        return;
    };
    places.retain(|_, until| *until > now);
    if places.len() >= BUSY_PLACES {
        if let Some(soonest) = places
            .iter()
            .min_by_key(|(_, until)| **until)
            .map(|(place, _)| place.clone())
        {
            places.remove(&soonest);
        }
    }
    places.insert(place, now + BUSY_KEPT_FOR);
}

/// How many images a request carries, in its history and its current message.
pub fn image_count(req: &GenerateAssistantResponseRequest) -> usize {
    images_in_order(req).len()
}

/// Of `total` images, oldest first, how many a request for a vision model sends as notes
/// instead. It sends at most the most recent `max_images`, and past that leaves out the
/// oldest half of them at once: the prompt a provider cached changes where the first image
/// becomes a note, and one image at a time that was on every new image.
pub fn images_left_as_notes(total: usize, max_images: usize) -> usize {
    if total <= max_images {
        return 0;
    }
    let block = (max_images / 2).max(1);
    (total - max_images).div_ceil(block) * block
}

/// The tokens of the note a vision model reads in place of an image left out for its age.
pub fn older_image_note_tokens() -> u64 {
    crate::usage_estimate::tokens_from_units(crate::usage_estimate::token_units(&omission_note(
        1,
        Omission::OverCount,
    )))
}

/// Decide how each image of a request for a vision model reaches the provider.
///
/// Only the most recent images go as images (see [`images_left_as_notes`]): a
/// conversation resends every earlier image on every turn, and older ones become notes
/// without being read at all. Of those kept, images too large to forward are shrunk under
/// the process-wide decode limit, newest first, and any not done within `budget` become
/// notes, on this step and on the conversation's next ones while it keeps going.
pub async fn prepare_images(
    req: &GenerateAssistantResponseRequest,
    max_images: usize,
    budget: std::time::Duration,
) -> PreparedImages {
    let images = images_in_order(req);
    let keep_from = images_left_as_notes(images.len(), max_images);
    let conversation = &req.conversation_state.conversation_id;
    let history_len = req.conversation_state.history.len();
    let now = tokio::time::Instant::now();
    let mut decided = Vec::with_capacity(images.len());
    let mut to_shrink = Vec::new();
    for (position, (turn, place, image)) in images.iter().enumerate() {
        if position < keep_from {
            decided.push(PreparedImage::Omitted(Omission::OverCount));
            continue;
        }
        match inspect_image(&image.source.bytes) {
            Inspection::Ready(prepared) => decided.push(prepared),
            Inspection::NeedsShrink(raw_bytes) => {
                decided.push(PreparedImage::Omitted(Omission::Busy));
                let message = match turn {
                    Turn::History(message) => *message,
                    Turn::Current => history_len,
                };
                let key = image_key(&raw_bytes);
                let busy_place = (conversation.clone(), message, *place, key);
                if !still_busy(&busy_place, now) {
                    to_shrink.push((position, raw_bytes, key, busy_place));
                }
            }
        }
    }
    let deadline = now + budget;
    for (position, raw_bytes, key, busy_place) in to_shrink.into_iter().rev() {
        decided[position] = shrink_image_keyed(raw_bytes, key, deadline).await;
        if decided[position] == PreparedImage::Omitted(Omission::Busy) {
            keep_busy(busy_place, tokio::time::Instant::now());
        }
    }

    let mut prepared = PreparedImages {
        history: vec![Vec::new(); req.conversation_state.history.len()],
        current: Vec::new(),
    };
    for ((turn, _, _), image) in images.into_iter().zip(decided) {
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
///
/// Attachments come first, as providers advise, so a message with nothing but a file is
/// still the user's turn: dropped, it left the request ending on the assistant's turn,
/// which current models refuse as a prefill.
fn format_user_content(
    content: &str,
    images: &[KiroImage],
    documents: &[KiroDocument],
    ctx: &TranslationContext,
    turn: Turn,
) -> serde_json::Value {
    if images.is_empty() && documents.is_empty() {
        return serde_json::Value::String(content.to_string());
    }
    let attachments = documents
        .iter()
        .map(|document| super::documents::content_part(document, ctx.supports_vision));

    if ctx.supports_vision {
        let mut parts: Vec<serde_json::Value> = attachments.collect();
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
        // Without vision every attachment is text: a text file's own, or a note.
        let mut text: String = attachments
            .filter_map(|part| part["text"].as_str().map(|text| format!("{text}\n\n")))
            .collect();
        text.push_str(content);
        for (idx, img) in images.iter().enumerate() {
            let transcription = if turn == Turn::Current {
                ctx.image_transcriptions.get(idx).and_then(|s| s.as_deref())
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

/// A tool result as the model should read it: its text blocks joined, a JSON block pretty
/// printed. Sent as the JSON of the block list, every newline, quote and backslash of a
/// file a tool read reached the model escaped, and a Windows path doubled, so an exact
/// `oldStr` for str_replace had to be un-escaped by the model first.
fn tool_result_text(blocks: &[serde_json::Value]) -> String {
    blocks
        .iter()
        .map(|block| match block {
            serde_json::Value::String(text) => text.clone(),
            _ => match (block.get("text"), block.get("json")) {
                (Some(serde_json::Value::String(text)), _) => text.clone(),
                (_, Some(json)) => serde_json::to_string_pretty(json).unwrap_or_default(),
                _ => block.to_string(),
            },
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Translate Kiro's `GenerateAssistantResponseRequest` into generic `ChatRequest`.
pub fn translate_kiro_to_chat_request(
    kiro_req: &GenerateAssistantResponseRequest,
    ctx: &mut TranslationContext,
) -> ChatRequest {
    let system_prompts: Vec<String> = kiro_req
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

    // 1. Process tools (provider-safe names; descriptions stay whole)
    let mut processed_tools = Vec::new();
    if let Some(ref ctx_data) = current_input.user_input_message_context {
        if !ctx_data.tools.is_empty() {
            let tools_value: Vec<serde_json::Value> = ctx_data
                .tools
                .iter()
                .map(|t| serde_json::to_value(t).unwrap_or_default())
                .collect();

            processed_tools = process_tools_for_provider(&tools_value, &mut ctx.tool_registry);
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

                // Kiro sends a turn's thinking back only with the signature it kept for it,
                // tagged with the upstream model that wrote it; an untagged one is dropped.
                let thinking = assistant_msg
                    .reasoning_content
                    .as_ref()
                    .and_then(|reasoning| reasoning.reasoning_text.as_ref())
                    .filter(|reasoning| !reasoning.text.is_empty())
                    .and_then(|reasoning| {
                        let (model, signature) =
                            crate::provider::untag_signature(reasoning.signature.as_deref()?)?;
                        Some(ThinkingBlock {
                            text: reasoning.text.clone(),
                            signature: signature.to_string(),
                            model: model.to_string(),
                        })
                    });
                raw_messages.push(ConversationMessage {
                    role: "assistant".to_string(),
                    content: serde_json::Value::String(assistant_msg.content.clone()),
                    tool_use_id: None,
                    tool_calls,
                    is_error: None,
                    thinking,
                });
            }
            Message::User(u) => {
                let user_msg = &u.user_input_message;
                // Historical tool results in user context
                if let Some(ref ctx_data) = user_msg.user_input_message_context {
                    for tr in &ctx_data.tool_results {
                        let content_str = tool_result_text(&tr.content);
                        let is_error = tr.status.as_deref().map(|s| s == "error" || s == "failed");
                        raw_messages.push(ConversationMessage {
                            role: "tool".to_string(),
                            content: serde_json::Value::String(content_str),
                            tool_use_id: Some(tr.tool_use_id.clone()),
                            tool_calls: Vec::new(),
                            is_error,
                            thinking: None,
                        });
                    }
                }
                // Historical user message content (with image and attachment support)
                if !user_msg.content.is_empty()
                    || !user_msg.images.is_empty()
                    || !user_msg.documents.is_empty()
                {
                    let content_val = format_user_content(
                        &user_msg.content,
                        &user_msg.images,
                        &user_msg.documents,
                        ctx,
                        Turn::History(position),
                    );
                    raw_messages.push(ConversationMessage {
                        role: "user".to_string(),
                        content: content_val,
                        tool_use_id: None,
                        tool_calls: Vec::new(),
                        is_error: None,
                        thinking: None,
                    });
                }
            }
        }
    }

    // 3. Process current user message: tool results and prompt content
    if let Some(ref ctx_data) = current_input.user_input_message_context {
        for tr in &ctx_data.tool_results {
            let content_str = tool_result_text(&tr.content);
            let is_error = tr.status.as_deref().map(|s| s == "error" || s == "failed");
            raw_messages.push(ConversationMessage {
                role: "tool".to_string(),
                content: serde_json::Value::String(content_str),
                tool_use_id: Some(tr.tool_use_id.clone()),
                tool_calls: Vec::new(),
                is_error,
                thinking: None,
            });
        }
    }

    let final_current_content = format_user_content(
        &current_input.content,
        &current_input.images,
        &current_input.documents,
        ctx,
        Turn::Current,
    );
    raw_messages.push(ConversationMessage {
        role: "user".to_string(),
        content: final_current_content,
        tool_use_id: None,
        tool_calls: Vec::new(),
        is_error: None,
        thinking: None,
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
            thinking: None,
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
            thinking: rm.thinking,
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
