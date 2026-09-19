//! `toolUseEvent` model.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ToolUseEvent {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub tool_use_id: String,
    #[serde(default)]
    pub input: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stop: bool,
}

impl ToolUseEvent {
    pub fn new(
        name: impl Into<String>,
        tool_use_id: impl Into<String>,
        input: impl Into<String>,
        stop: bool,
    ) -> Self {
        Self {
            name: name.into(),
            tool_use_id: tool_use_id.into(),
            input: input.into(),
            stop,
        }
    }
}
