//! `reasoningContentEvent` model.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ReasoningContentEvent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redacted_content: Option<String>,
}

impl ReasoningContentEvent {
    pub fn with_text(text: impl Into<String>) -> Self {
        Self {
            text: Some(text.into()),
            signature: None,
            redacted_content: None,
        }
    }

    pub fn with_signature(mut self, signature: impl Into<String>) -> Self {
        self.signature = Some(signature.into());
        self
    }
}
