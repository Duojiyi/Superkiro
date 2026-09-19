//! Tool name shortening, description relocation, and orphan repair.
//!
//! Spec §4.3:
//! - Tool name shortening & restoration (OpenAI 64-char limit).
//! - Long tool descriptions (>1024 chars) moved to system prompt.
//! - Orphan tool_use / tool_result repair.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

pub const MAX_TOOL_NAME_LEN: usize = 64;
pub const MAX_TOOL_DESC_LEN: usize = 1024;

/// Tool name mapping registry for a single conversation request.
#[derive(Debug, Clone, Default)]
pub struct ToolRegistry {
    /// Shortened name -> Original long name
    short_to_long: HashMap<String, String>,
    /// Original long name -> Shortened name
    long_to_short: HashMap<String, String>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a tool and return the provider-safe name.
    pub fn register(&mut self, full_name: &str) -> String {
        if let Some(existing) = self.long_to_short.get(full_name) {
            return existing.clone();
        }

        let safe_name = if full_name.len() <= MAX_TOOL_NAME_LEN
            && full_name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            full_name.to_string()
        } else {
            // Shorten using prefix + deterministic CRC32
            let crc = kiro_wire::crc::crc32(full_name.as_bytes());
            // Slice by characters, not bytes: client-provided tool names may
            // contain UTF-8 and byte slicing would panic at a code-point
            // boundary.
            let prefix: String = full_name.chars().take(32).collect();
            let sanitized_prefix: String = prefix
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || c == '_' {
                        c
                    } else {
                        '_'
                    }
                })
                .collect();
            format!("{}_{:08x}", sanitized_prefix, crc)
        };

        self.short_to_long
            .insert(safe_name.clone(), full_name.to_string());
        self.long_to_short
            .insert(full_name.to_string(), safe_name.clone());

        safe_name
    }

    /// Restore an original Kiro tool name from a provider's tool name.
    pub fn restore(&self, short_or_original: &str) -> String {
        self.short_to_long
            .get(short_or_original)
            .cloned()
            .unwrap_or_else(|| short_or_original.to_string())
    }
}

/// Process tools for upstream providers:
/// 1. Shortens names that exceed provider limits.
/// 2. Relocates oversized descriptions (>1024 chars) into a system prompt section.
pub fn process_tools_for_provider(
    tools: &[Value],
    registry: &mut ToolRegistry,
) -> (Vec<Value>, Option<String>) {
    let mut processed_tools = Vec::new();
    let mut relocated_docs = Vec::new();

    for tool in tools {
        let mut tool_obj = tool.clone();

        // Handle both direct tool format and AWS toolSpecification format
        let spec = if let Some(s) = tool_obj.get_mut("toolSpecification") {
            s
        } else {
            &mut tool_obj
        };

        if let Some(name_val) = spec.get("name").and_then(|n| n.as_str()) {
            let short_name = registry.register(name_val);
            spec["name"] = Value::String(short_name.clone());

            if let Some(desc) = spec.get("description").and_then(|d| d.as_str()) {
                if desc.len() > MAX_TOOL_DESC_LEN {
                    relocated_docs.push(format!("### Tool: {}\n{}", short_name, desc));
                    spec["description"] = Value::String(format!(
                        "Documentation for {} is provided in the system prompt.",
                        short_name
                    ));
                }
            }
        }

        processed_tools.push(tool_obj);
    }

    let system_append = if relocated_docs.is_empty() {
        None
    } else {
        Some(format!(
            "\n\n## Extended Tool Documentation\n{}",
            relocated_docs.join("\n\n")
        ))
    };

    (processed_tools, system_append)
}

use crate::provider::ToolCallEntry;

/// Generic representation of a conversation turn for orphan pairing check.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationMessage {
    pub role: String,
    pub content: Value,
    pub tool_use_id: Option<String>,
    pub tool_calls: Vec<ToolCallEntry>, // Full tool calls emitted by assistant
    pub is_error: Option<bool>,
}

/// Sanitizes conversation turns by repairing orphan tool_use or tool_result pairs in strict
/// chronological order (Spec §4.3, T05):
/// 1. Orphan tool_results (no matching tool_use_id in preceding active assistant turn)
///    are converted to plain user messages. Future assistant calls cannot validate past results.
/// 2. Unanswered tool_calls (assistant emitted tool_use, but turn ended or moved on without
///    client tool_result) receive synthetic omitted/error results (never synthetic success).
pub fn repair_orphan_tool_pairs(messages: Vec<ConversationMessage>) -> Vec<ConversationMessage> {
    let mut active_pending_calls: Vec<String> = Vec::new();
    let mut repaired = Vec::new();

    for mut msg in messages {
        match msg.role.as_str() {
            "assistant" => {
                // Synthesize error results for any previously unanswered tool calls before new assistant turn
                for pending_id in active_pending_calls.drain(..) {
                    repaired.push(ConversationMessage {
                        role: "tool".to_string(),
                        content: Value::String(
                            "{\"error\": \"tool call omitted or cancelled by client\"}".to_string(),
                        ),
                        tool_use_id: Some(pending_id),
                        tool_calls: Vec::new(),
                        is_error: Some(true),
                    });
                }
                for tc in &msg.tool_calls {
                    active_pending_calls.push(tc.id.clone());
                }
                repaired.push(msg);
            }
            "tool" => {
                if let Some(ref id) = msg.tool_use_id {
                    if let Some(pos) = active_pending_calls.iter().position(|p| p == id) {
                        active_pending_calls.remove(pos);
                        repaired.push(msg);
                    } else {
                        // Orphan tool result: cannot match a future or nonexistent call
                        msg.role = "user".to_string();
                        if let Some(s) = msg.content.as_str() {
                            msg.content = Value::String(format!("[Previous Tool Result]: {}", s));
                        }
                        msg.tool_use_id = None;
                        msg.is_error = None;
                        repaired.push(msg);
                    }
                } else {
                    msg.role = "user".to_string();
                    repaired.push(msg);
                }
            }
            _ => {
                // User or system turn: if previous assistant calls remain unanswered, synthesize omitted results
                if !active_pending_calls.is_empty() && msg.tool_use_id.is_none() {
                    for pending_id in active_pending_calls.drain(..) {
                        repaired.push(ConversationMessage {
                            role: "tool".to_string(),
                            content: Value::String(
                                "{\"error\": \"tool call omitted or cancelled by client\"}"
                                    .to_string(),
                            ),
                            tool_use_id: Some(pending_id),
                            tool_calls: Vec::new(),
                            is_error: Some(true),
                        });
                    }
                }
                repaired.push(msg);
            }
        }
    }

    // Trailing cleanup: synthesize error results for any remaining unanswered calls
    for pending_id in active_pending_calls.drain(..) {
        repaired.push(ConversationMessage {
            role: "tool".to_string(),
            content: Value::String(
                "{\"error\": \"tool call omitted or cancelled by client\"}".to_string(),
            ),
            tool_use_id: Some(pending_id),
            tool_calls: Vec::new(),
            is_error: Some(true),
        });
    }

    repaired
}
