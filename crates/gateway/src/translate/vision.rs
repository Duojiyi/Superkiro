//! Vision Fallback module (Spec §14.3, referencing AnyBridge).
//!
//! When an upstream target model does not natively support multimodal image input
//! (e.g. DeepSeek V3, DeepSeek R1, pure text LLMs), this module intercepts image
//! inputs and converts them into structured text transcriptions, preventing
//! upstream 400 Bad Request errors and preserving conversation context.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;

/// Bound on a transcription sub-request. It runs inline before any byte is
/// streamed, holding a credit reservation and a concurrency slot.
const VISION_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Returns whether a model identifier natively supports vision/image input.
pub fn model_supports_vision(model_id: &str) -> bool {
    let lower = model_id.to_lowercase();

    // Explicit vision indicators always override text-only family names
    if lower.contains("vision") || lower.contains("-vl") || lower.contains("_vl") {
        return true;
    }

    // Known text-only model families
    if lower.contains("deepseek")
        || lower.contains("qwen-turbo")
        || lower.contains("qwen-plus")
        || lower.contains("o1-mini")
        || lower.contains("llama")
        || lower.contains("mistral")
        || lower.contains("gemma")
        || lower.contains("command-r")
    {
        return false;
    }

    // Known multimodal models (Claude 3+, GPT-4o, Gemini, etc.)
    true
}

/// Configuration for vision fallback routing.
#[derive(Clone, Serialize, Deserialize)]
pub struct VisionFallbackConfig {
    pub enabled: bool,
    pub fallback_provider_url: Option<String>,
    pub fallback_api_key: Option<String>,
    pub fallback_model: String,
    pub max_tokens: u32,
}

impl std::fmt::Debug for VisionFallbackConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VisionFallbackConfig")
            .field("enabled", &self.enabled)
            .field("fallback_provider_url", &self.fallback_provider_url)
            .field(
                "fallback_api_key",
                &self.fallback_api_key.as_ref().map(|_| "[redacted]"),
            )
            .field("fallback_model", &self.fallback_model)
            .field("max_tokens", &self.max_tokens)
            .finish()
    }
}

impl Default for VisionFallbackConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            fallback_provider_url: None,
            fallback_api_key: None,
            fallback_model: "claude-sonnet-4.5".to_string(),
            max_tokens: 1024,
        }
    }
}

/// In-memory cache for vision transcriptions to avoid redundant calls.
#[derive(Debug, Default, Clone)]
pub struct VisionFallbackCache {
    entries: Arc<RwLock<HashMap<String, String>>>,
}

impl VisionFallbackCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, key: &str) -> Option<String> {
        self.entries.read().ok()?.get(key).cloned()
    }

    pub fn set(&self, key: String, val: String) {
        if let Ok(mut map) = self.entries.write() {
            if map.len() >= 500 {
                if let Some(first_key) = map.keys().next().cloned() {
                    map.remove(&first_key);
                }
            }
            map.insert(key, val);
        }
    }
}

/// Format a structured text fallback description for an image.
pub fn format_fallback_description(
    index: usize,
    format: &str,
    byte_len: usize,
    dimensions: Option<(u32, u32)>,
    vision_transcription: Option<&str>,
) -> String {
    let detail = match vision_transcription {
        Some(desc) => format!("- 视觉模型转写结果:\n{}", desc.trim()),
        None => "- 图像内容转录: [用户附加了图片。当前目标模型为纯文本模型，网关已自动执行视觉降级保护，将图像转为文本上下文]".to_string(),
    };
    let size = match dimensions {
        Some((width, height)) => format!("尺寸: {width}x{height}"),
        None => "无法读取".to_string(),
    };

    format!(
        "[视觉降级/Vision Fallback - 图片转文字 #{index}]\n- 图片格式: {format} (体积: {byte_len} 字节, {size})\n{detail}"
    )
}

/// Call an upstream vision provider (e.g. OpenAI/Claude compatible endpoint) to transcribe an image into text.
#[allow(clippy::too_many_arguments)]
pub async fn transcribe_image_with_provider(
    client: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    model: &str,
    format: &str,
    base64_data: &str,
    context_prompt: Option<&str>,
    max_tokens: u32,
) -> Result<String, String> {
    let prompt = format!(
        "用户问题及上下文：{}\n请用中文详细描述这张图片中的可见文字、关键代码、UI 元素、图表和布局结构，供不能直接看图的纯文本模型解答用户问题。",
        context_prompt.unwrap_or("（无前置上下文）")
    );
    let data_url = format!("data:image/{};base64,{}", format, base64_data);
    let body = serde_json::json!({
        "model": model,
        "messages": [
            {
                "role": "user",
                "content": [
                    { "type": "text", "text": prompt },
                    { "type": "image_url", "image_url": { "url": data_url } }
                ]
            }
        ],
        "max_tokens": max_tokens,
        "temperature": 0.2
    });

    let url = format!("{}/v1/chat/completions", base_url.trim_end_matches('/'));
    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/json")
        .timeout(VISION_REQUEST_TIMEOUT)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("Vision HTTP request failed: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let err_body = resp.text().await.unwrap_or_default();
        return Err(format!(
            "Vision provider returned status {status}: {err_body}"
        ));
    }

    let json: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse vision response JSON: {e}"))?;

    let content = json["choices"][0]["message"]["content"]
        .as_str()
        .ok_or_else(|| "Missing choices[0].message.content in vision response".to_string())?;

    Ok(content.to_string())
}
