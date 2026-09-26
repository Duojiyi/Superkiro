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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub documents: Vec<KiroDocument>,
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

/// A file attached in chat. Kiro strips the extension from `name` and gives it as `format`
/// ("pdf", "md", "csv", "docx", ...); `source.bytes` is base64, as the SDK sends a blob.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct KiroDocument {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub format: String,
    pub source: KiroDocumentSource,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KiroDocumentSource {
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
    /// The turn's thinking, which Kiro sends back only when it kept a signature for it and
    /// the conversation is still on the model that produced it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<ReasoningContent>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ReasoningContent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_text: Option<ReasoningText>,
    /// Base64, as the SDK sends a blob.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redacted_content: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ReasoningText {
    #[serde(default)]
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
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
