//! Translate ProviderStreamEvent into Kiro AWS EventStream binary frames.
//!
//! Spec §4.3, §4.4:
//! - Streaming delta encoding (assistantResponse, reasoningContent, toolUse).
//! - Tool name restoration via ToolRegistry.
//! - Stop reason mapping and TokenUsage metadata.

use super::to_provider::TranslationContext;
use crate::provider::{ProviderDelta, ProviderStreamEvent, TokenUsage};
use kiro_wire::encoder::{
    encode_assistant_response, encode_metadata, encode_reasoning, encode_tool_use,
};
use kiro_wire::events::metadata::TokenUsage as KiroTokenUsage;

/// State tracker for streaming translation from provider to Kiro client.
pub struct StreamTranslationState {
    pub model_id: String,
    pub last_tool_id: String,
    pub last_tool_name: String,
    pub accumulated_tool_input: String,
    pub last_stop_reason: Option<String>,
    pub last_usage: Option<TokenUsage>,
}

impl StreamTranslationState {
    pub fn new(model_id: &str) -> Self {
        Self {
            model_id: model_id.to_string(),
            last_tool_id: String::new(),
            last_tool_name: String::new(),
            accumulated_tool_input: String::new(),
            last_stop_reason: None,
            last_usage: None,
        }
    }

    /// Map provider finish/stop reason into Kiro expected stopReason string.
    pub fn map_stop_reason(reason: &str) -> String {
        match reason {
            "stop" => "end_turn".to_string(),
            "tool_calls" | "tool_use" => "tool_use".to_string(),
            "length" | "max_tokens" => "max_tokens".to_string(),
            other => other.to_string(),
        }
    }
}

/// Translate a single `ProviderStreamEvent` into zero or more AWS EventStream binary frames.
pub fn translate_provider_event_to_frames(
    event: &ProviderStreamEvent,
    ctx: &TranslationContext,
    state: &mut StreamTranslationState,
) -> Vec<Vec<u8>> {
    let mut frames = Vec::new();

    match event {
        ProviderStreamEvent::Delta(delta) => match delta {
            ProviderDelta::Text(text) => {
                if !text.is_empty() {
                    frames.push(encode_assistant_response(text, Some(&state.model_id)));
                }
            }
            ProviderDelta::Reasoning(reasoning) => {
                if !reasoning.is_empty() {
                    frames.push(encode_reasoning(Some(reasoning), None, None));
                }
            }
            ProviderDelta::ToolCallChunk {
                id,
                name,
                arguments,
                ..
            } => {
                if let Some(ref new_id) = id {
                    state.last_tool_id = new_id.clone();
                }
                if let Some(ref new_name) = name {
                    // Restore original tool name if shortened
                    state.last_tool_name = ctx.tool_registry.restore(new_name);
                }

                state.accumulated_tool_input.push_str(arguments);

                // Emit tool use chunk
                frames.push(encode_tool_use(
                    &state.last_tool_name,
                    &state.last_tool_id,
                    arguments,
                    false,
                ));
            }
        },
        ProviderStreamEvent::StopReason(reason) => {
            let mapped = StreamTranslationState::map_stop_reason(reason);
            state.last_stop_reason = Some(mapped);
        }
        ProviderStreamEvent::Usage(usage) => {
            if let Some(ref mut existing) = state.last_usage {
                existing.merge(usage);
            } else {
                state.last_usage = Some(usage.clone());
            }
        }
        ProviderStreamEvent::Done => {
            // Emit final stop if tool use was active
            if !state.last_tool_id.is_empty() {
                frames.push(encode_tool_use(
                    &state.last_tool_name,
                    &state.last_tool_id,
                    "",
                    true, // stop = true
                ));
            }

            // Emit final metadata event with usage and stop reason
            let kiro_usage = state.last_usage.as_ref().map(|u| KiroTokenUsage {
                uncached_input_tokens: u.uncached_prompt_tokens as i64,
                output_tokens: u.completion_tokens as i64,
                cache_read_input_tokens: u.cache_read_input_tokens.unwrap_or(0) as i64,
                cache_write_input_tokens: u.cache_creation_input_tokens.unwrap_or(0) as i64,
            });

            frames.push(encode_metadata(
                kiro_usage,
                state.last_stop_reason.as_deref().or(Some("end_turn")),
            ));
        }
    }

    frames
}
