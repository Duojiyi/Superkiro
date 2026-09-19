//! Conversation state, messages, and GenerateAssistantResponseRequest types.

use super::tool::{Tool, ToolResult, ToolUseEntry};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GenerateAssistantResponseRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_model_request_fields: Option<ModelRequestFields>,
    pub conversation_state: ConversationState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_arn: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ConversationState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_model_request_fields: Option<ModelRequestFields>,
    pub conversation_id: String,
    pub current_message: CurrentMessage,
    #[serde(default)]
    pub history: Vec<Message>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CurrentMessage {
    pub user_input_message: UserInputMessage,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UserInputMessage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_model_request_fields: Option<ModelRequestFields>,
    #[serde(default)]
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<KiroImage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_input_message_context: Option<UserInputMessageContext>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UserInputMessageContext {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_model_request_fields: Option<ModelRequestFields>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<Tool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_results: Vec<ToolResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub editor_state: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct KiroImage {
    pub format: String,
    pub source: KiroImageSource,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KiroImageSource {
    pub bytes: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum Message {
    User(HistoryUserMessage),
    Assistant(HistoryAssistantMessage),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HistoryUserMessage {
    pub user_input_message: UserInputMessage,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HistoryAssistantMessage {
    pub assistant_response_message: AssistantResponseMessage,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AssistantResponseMessage {
    #[serde(default)]
    pub content: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_uses: Vec<ToolUseEntry>,
}

/// Only recognized inference controls cross the gateway boundary.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ModelRequestFields {
    #[serde(default)]
    pub output_config: Option<EffortConfig>,
    #[serde(default)]
    pub reasoning: Option<EffortConfig>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EffortConfig {
    #[serde(default)]
    pub effort: Option<ReasoningEffort>,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningEffort {
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}
impl GenerateAssistantResponseRequest {
    pub fn reasoning_effort(&self) -> Option<ReasoningEffort> {
        let input = &self.conversation_state.current_message.user_input_message;
        [
            input
                .user_input_message_context
                .as_ref()
                .and_then(|c| c.additional_model_request_fields.as_ref()),
            input.additional_model_request_fields.as_ref(),
            self.additional_model_request_fields.as_ref(),
            self.conversation_state
                .additional_model_request_fields
                .as_ref(),
        ]
        .into_iter()
        .flatten()
        .find_map(|f| {
            f.output_config
                .as_ref()
                .or(f.reasoning.as_ref())
                .and_then(|c| c.effort)
        })
    }
}
