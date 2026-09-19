//! Model context presets library and conversation compression threshold logic.
//!
//! Spec §4.5 (contextUsageEvent & compression thresholds) & Spec §14.3 (Model Context Presets).

use serde::{Deserialize, Serialize};

/// Predefined model context window, output limit, and compression rules (Spec §4.5, §14.3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelContextPreset {
    pub model_id: String,
    pub display_name: String,
    pub context_window: u32,
    pub max_output: u32,
    pub compression_threshold: f64, // e.g. 0.80 = 80% of context window triggers auto-summarization
    pub supports_tools: bool,
    pub supports_vision: bool,
    pub supports_reasoning: bool,
}

/// Calculated metrics from token usage against model context window (Spec §4.5).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextUsageMetric {
    pub model_id: String,
    pub context_window: u32,
    pub used_tokens: u64,
    pub percentage: f64,
    pub remaining_tokens: u32,
    pub should_compress: bool,
}

/// Catalog of builtin presets for mainstream LLM models.
pub struct ModelContextLibrary;

impl ModelContextLibrary {
    /// Return the list of builtin model presets.
    pub fn builtin() -> Vec<ModelContextPreset> {
        vec![
            ModelContextPreset {
                model_id: "claude-3-5-sonnet".to_string(),
                display_name: "Claude 3.5 Sonnet".to_string(),
                context_window: 200_000,
                max_output: 64_000,
                compression_threshold: 0.80,
                supports_tools: true,
                supports_vision: true,
                supports_reasoning: true,
            },
            ModelContextPreset {
                model_id: "claude-sonnet-4.5".to_string(),
                display_name: "Claude 3.7 / 4.5 Sonnet".to_string(),
                context_window: 200_000,
                max_output: 64_000,
                compression_threshold: 0.80,
                supports_tools: true,
                supports_vision: true,
                supports_reasoning: true,
            },
            ModelContextPreset {
                model_id: "claude-3-5-haiku".to_string(),
                display_name: "Claude 3.5 Haiku".to_string(),
                context_window: 200_000,
                max_output: 8_192,
                compression_threshold: 0.80,
                supports_tools: true,
                supports_vision: true,
                supports_reasoning: false,
            },
            ModelContextPreset {
                model_id: "claude-3-opus".to_string(),
                display_name: "Claude 3 Opus".to_string(),
                context_window: 200_000,
                max_output: 4_096,
                compression_threshold: 0.80,
                supports_tools: true,
                supports_vision: true,
                supports_reasoning: false,
            },
            ModelContextPreset {
                model_id: "gpt-4o".to_string(),
                display_name: "GPT-4o".to_string(),
                context_window: 128_000,
                max_output: 16_384,
                compression_threshold: 0.80,
                supports_tools: true,
                supports_vision: true,
                supports_reasoning: false,
            },
            ModelContextPreset {
                model_id: "gpt-4o-mini".to_string(),
                display_name: "GPT-4o mini".to_string(),
                context_window: 128_000,
                max_output: 16_384,
                compression_threshold: 0.80,
                supports_tools: true,
                supports_vision: true,
                supports_reasoning: false,
            },
            ModelContextPreset {
                model_id: "o1".to_string(),
                display_name: "OpenAI o1".to_string(),
                context_window: 200_000,
                max_output: 100_000,
                compression_threshold: 0.80,
                supports_tools: true,
                supports_vision: true,
                supports_reasoning: true,
            },
            ModelContextPreset {
                model_id: "o3-mini".to_string(),
                display_name: "OpenAI o3-mini".to_string(),
                context_window: 200_000,
                max_output: 100_000,
                compression_threshold: 0.80,
                supports_tools: true,
                supports_vision: false,
                supports_reasoning: true,
            },
            ModelContextPreset {
                model_id: "deepseek-chat".to_string(),
                display_name: "DeepSeek V3".to_string(),
                context_window: 64_000,
                max_output: 8_000,
                compression_threshold: 0.85,
                supports_tools: true,
                supports_vision: false,
                supports_reasoning: false,
            },
            ModelContextPreset {
                model_id: "deepseek-reasoner".to_string(),
                display_name: "DeepSeek R1".to_string(),
                context_window: 64_000,
                max_output: 8_000,
                compression_threshold: 0.85,
                supports_tools: true,
                supports_vision: false,
                supports_reasoning: true,
            },
            ModelContextPreset {
                model_id: "gemini-2.0-flash".to_string(),
                display_name: "Gemini 2.0 Flash".to_string(),
                context_window: 1_048_576,
                max_output: 8_192,
                compression_threshold: 0.85,
                supports_tools: true,
                supports_vision: true,
                supports_reasoning: true,
            },
            ModelContextPreset {
                model_id: "qwen-2.5-coder".to_string(),
                display_name: "Qwen 2.5 Coder".to_string(),
                context_window: 131_072,
                max_output: 8_192,
                compression_threshold: 0.80,
                supports_tools: true,
                supports_vision: false,
                supports_reasoning: false,
            },
        ]
    }

    /// Resolve preset by model ID (fuzzy, prefix, and fallback matching).
    pub fn resolve(model_id: &str) -> ModelContextPreset {
        let normalized = model_id.to_lowercase();
        let presets = Self::builtin();

        // 1. Exact match
        if let Some(p) = presets.iter().find(|p| p.model_id == normalized) {
            return p.clone();
        }

        // 2. Alias / prefix matching
        for p in &presets {
            if normalized.starts_with(&p.model_id)
                || (!normalized.is_empty() && p.model_id.starts_with(&normalized))
            {
                return p.clone();
            }
        }

        // Specific alias mapping
        if normalized.contains("claude") {
            if normalized.contains("haiku") {
                return Self::resolve("claude-3-5-haiku");
            }
            return Self::resolve("claude-3-5-sonnet");
        }
        if normalized.contains("deepseek") {
            if normalized.contains("reasoner") || normalized.contains("r1") {
                return Self::resolve("deepseek-reasoner");
            }
            return Self::resolve("deepseek-chat");
        }
        if normalized.contains("gpt-4") {
            if normalized.contains("mini") {
                return Self::resolve("gpt-4o-mini");
            }
            return Self::resolve("gpt-4o");
        }
        if normalized.contains("gemini") {
            return Self::resolve("gemini-2.0-flash");
        }
        if normalized.contains("qwen") {
            return Self::resolve("qwen-2.5-coder");
        }

        // 3. Fallback safe default
        ModelContextPreset {
            model_id: model_id.to_string(),
            display_name: format!("Custom ({})", model_id),
            context_window: 128_000,
            max_output: 8_192,
            compression_threshold: 0.80,
            supports_tools: true,
            supports_vision: true,
            supports_reasoning: false,
        }
    }

    /// Calculate context usage percentage and evaluate whether compression should be triggered.
    pub fn calculate_usage(model_id: &str, used_tokens: u64) -> ContextUsageMetric {
        let preset = Self::resolve(model_id);
        let window = preset.context_window.max(1);
        let percentage = ((used_tokens as f64) / (window as f64)).clamp(0.0, 1.0);
        let remaining_tokens = window.saturating_sub(used_tokens.min(u32::MAX as u64) as u32);
        let should_compress = percentage >= preset.compression_threshold;

        ContextUsageMetric {
            model_id: preset.model_id,
            context_window: window,
            used_tokens,
            percentage,
            remaining_tokens,
            should_compress,
        }
    }
}
