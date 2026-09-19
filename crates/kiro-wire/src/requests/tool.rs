//! Tool definition and tool result request types.

use serde::{Deserialize, Serialize};

/// Tool definition. Supports both AWS wrapped format (`toolSpecification`) and direct format.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum Tool {
    Wrapped {
        #[serde(rename = "toolSpecification")]
        tool_specification: ToolSpecification,
    },
    Direct(ToolSpecification),
}

impl Tool {
    pub fn spec(&self) -> &ToolSpecification {
        match self {
            Self::Wrapped { tool_specification } => tool_specification,
            Self::Direct(spec) => spec,
        }
    }

    pub fn name(&self) -> &str {
        &self.spec().name
    }

    pub fn description(&self) -> &str {
        &self.spec().description
    }

    pub fn input_schema(&self) -> &serde_json::Value {
        &self.spec().input_schema
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ToolSpecification {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub input_schema: serde_json::Value,
}

impl ToolSpecification {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        input_schema: serde_json::Value,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            input_schema,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ToolResult {
    pub tool_use_id: String,
    #[serde(default)]
    pub content: Vec<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ToolUseEntry {
    pub tool_use_id: String,
    pub name: String,
    #[serde(default)]
    pub input: serde_json::Value,
}
