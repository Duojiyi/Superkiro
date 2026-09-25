//! Vision Fallback module (Spec §14.3, referencing AnyBridge).
//!
//! When an upstream target model does not natively support multimodal image input
//! (e.g. DeepSeek V3, DeepSeek R1, pure text LLMs), this module intercepts image
//! inputs and converts them into structured text transcriptions, preventing
//! upstream 400 Bad Request errors and preserving conversation context.

use futures_util::StreamExt;
use kiro_wire::requests::conversation::KiroImage;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;

/// Bound on a transcription sub-request. It runs inline before any byte is
/// streamed, holding a credit reservation and a concurrency slot.
const VISION_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// How long one request may spend transcribing the images of its current message, all of
/// them together; an image not described by then reaches the model as its placeholder.
/// With image preparation and the upstream start, this keeps the time before the first
/// byte well inside the gateway's response timeout.
pub const TRANSCRIPTION_BUDGET: Duration = VISION_REQUEST_TIMEOUT;

/// Transcriptions one request runs at a time.
const TRANSCRIPTION_CONCURRENCY: usize = 4;

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

/// Transcriptions are shared by every card, so the key must name the image and its
/// context exactly: SHA-256 over each length-prefixed part, not a 64-bit hash.
fn transcription_cache_key(model: &str, format: &str, bytes: &str, context: &str) -> String {
    let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
    for part in [model, format, bytes, context] {
        digest.update(&(part.len() as u64).to_le_bytes());
        digest.update(part.as_bytes());
    }
    let hash: String = digest
        .finish()
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("{model}:{format}:{hash}")
}

/// Describe each image of the current message for a text-only model, several at a time
/// and all within `budget`. One slot per image, so a failed or late transcription leaves
/// its own image undescribed instead of shifting the rest. All `None` when no vision
/// provider is configured.
pub async fn transcribe_images(
    client: &reqwest::Client,
    config: &VisionFallbackConfig,
    cache: &VisionFallbackCache,
    images: &[KiroImage],
    context: &str,
    budget: Duration,
) -> Vec<Option<String>> {
    let (true, Some(base_url), Some(api_key)) = (
        config.enabled,
        config.fallback_provider_url.as_deref(),
        config.fallback_api_key.as_deref(),
    ) else {
        return vec![None; images.len()];
    };
    let deadline = tokio::time::Instant::now() + budget;
    let pending: Vec<_> = images
        .iter()
        .map(|image| {
            let format =
                super::images::sniff_format(&image.source.bytes).unwrap_or(image.format.as_str());
            let key = transcription_cache_key(
                &config.fallback_model,
                format,
                &image.source.bytes,
                context,
            );
            let request = transcribe_image_with_provider(
                client,
                base_url,
                api_key,
                &config.fallback_model,
                format,
                &image.source.bytes,
                Some(context),
                config.max_tokens,
            );
            cached_transcription(cache, key, tokio::time::timeout_at(deadline, request))
        })
        .collect();
    futures_util::stream::iter(pending)
        .buffered(TRANSCRIPTION_CONCURRENCY)
        .collect()
        .await
}

/// A cached transcription, or the outcome of `request`, remembered when it succeeds.
async fn cached_transcription(
    cache: &VisionFallbackCache,
    key: String,
    request: impl std::future::Future<
        Output = Result<Result<String, String>, tokio::time::error::Elapsed>,
    >,
) -> Option<String> {
    if let Some(cached) = cache.get(&key) {
        return Some(cached);
    }
    let description = request.await.ok()?.ok()?;
    cache.set(key, description.clone());
    Some(description)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_key_is_a_full_digest_of_unambiguous_parts() {
        let key = transcription_cache_key("model", "png", "ab", "c");
        let digest = key.rsplit(':').next().unwrap();
        assert_eq!(digest.len(), 64, "SHA-256, not a 64-bit hash: {key}");
        assert!(digest.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(key, transcription_cache_key("model", "png", "ab", "c"));
        assert_ne!(key, transcription_cache_key("model", "png", "a", "bc"));
        assert_ne!(key, transcription_cache_key("model", "png", "ab", "d"));
    }

    // However many images a turn carries and however slowly the vision provider answers,
    // transcription ends at its budget and the images left over keep their placeholder.
    #[tokio::test]
    async fn transcription_ends_at_its_budget() {
        // A local listener that accepts connections and never answers.
        let silent = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let config = VisionFallbackConfig {
            enabled: true,
            fallback_provider_url: Some(format!("http://{}", silent.local_addr().unwrap())),
            fallback_api_key: Some("vision-key".into()),
            ..Default::default()
        };
        let images: Vec<KiroImage> = (0..8)
            .map(|n| KiroImage {
                format: "png".into(),
                source: kiro_wire::requests::conversation::KiroImageSource {
                    bytes: format!("image-{n}"),
                },
            })
            .collect();

        let started = std::time::Instant::now();
        let transcriptions = transcribe_images(
            &reqwest::Client::new(),
            &config,
            &VisionFallbackCache::default(),
            &images,
            "what is this",
            Duration::from_millis(300),
        )
        .await;
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "took {:?}",
            started.elapsed()
        );
        assert_eq!(transcriptions, vec![None; images.len()]);
        drop(silent);
    }
}
