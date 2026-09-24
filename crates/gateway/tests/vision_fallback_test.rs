//! Vision Fallback integration tests (Spec §14.3, P4-2).

use gateway::translate::{
    format_fallback_description, model_supports_vision, transcribe_image_with_provider,
    translate_kiro_to_chat_request, TranslationContext, VisionFallbackCache,
};
use kiro_wire::requests::conversation::GenerateAssistantResponseRequest;
use std::fs;
use std::path::PathBuf;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn sample_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("docs")
        .join("p0")
        .join("samples")
}

#[test]
fn test_model_vision_capability_detection() {
    // Multimodal models
    assert!(model_supports_vision("claude-3-7-sonnet"));
    assert!(model_supports_vision("claude-sonnet-4.5"));
    assert!(model_supports_vision("gpt-4o"));
    assert!(model_supports_vision("gpt-4o-mini"));
    assert!(model_supports_vision("gemini-2.0-pro"));
    assert!(model_supports_vision("qwen-vl-max"));
    assert!(model_supports_vision("mimo-2.5-vision"));

    // Pure-text models (require vision fallback)
    assert!(!model_supports_vision("deepseek-chat"));
    assert!(!model_supports_vision("deepseek-reasoner"));
    assert!(!model_supports_vision("deepseek-coder"));
    assert!(!model_supports_vision("qwen-turbo"));
    assert!(!model_supports_vision("llama-3.3-70b"));
    assert!(!model_supports_vision("mistral-large"));
}

#[test]
fn test_vision_fallback_on_text_only_model() {
    let sample_path = sample_dir().join("conversation_request_with_tools_and_images.json");
    let json_bytes = fs::read(&sample_path).expect("Read P0 conversation sample");
    let kiro_req: GenerateAssistantResponseRequest =
        serde_json::from_slice(&json_bytes).expect("Deserialize P0 sample");

    // DeepSeek target model: text-only, should trigger vision fallback
    let mut ctx = TranslationContext::new("deepseek-chat");
    assert!(!ctx.supports_vision);

    let chat_req = translate_kiro_to_chat_request(&kiro_req, &mut ctx);

    assert_eq!(chat_req.model, "deepseek-chat");
    let user_msg = chat_req
        .messages
        .iter()
        .find(|m| m.role == "user")
        .expect("Must have user message");

    // Must NOT use raw attached image tag
    assert!(!user_msg
        .content
        .to_string()
        .contains("[Attached Image (format:"));
    // Must contain Vision Fallback structured text block
    assert!(user_msg
        .content
        .to_string()
        .contains("视觉降级/Vision Fallback"));
    assert!(user_msg.content.to_string().contains("图片格式: png"));
    assert!(user_msg.content.to_string().contains("视觉降级保护"));
}

#[test]
fn test_multimodal_model_preserves_attached_image() {
    let sample_path = sample_dir().join("conversation_request_with_tools_and_images.json");
    let json_bytes = fs::read(&sample_path).expect("Read P0 conversation sample");
    let kiro_req: GenerateAssistantResponseRequest =
        serde_json::from_slice(&json_bytes).expect("Deserialize P0 sample");

    // Claude Sonnet: supports vision natively
    let mut ctx = TranslationContext::new("claude-sonnet-4.5");
    assert!(ctx.supports_vision);

    let chat_req = translate_kiro_to_chat_request(&kiro_req, &mut ctx);

    let user_msg = chat_req
        .messages
        .iter()
        .find(|m| m.role == "user")
        .expect("Must have user message");

    // Keeps normal attached image
    // BUG-1 fix: vision models receive actual image data
    let cs = user_msg.content.to_string();
    assert!(
        cs.contains("image_url") || cs.contains("image") || cs.contains("base64"),
        "Vision model must receive image data"
    );
}

#[test]
fn test_fallback_transcription_cache() {
    let cache = VisionFallbackCache::new();
    let key = "model:png:1024".to_string();

    assert_eq!(cache.get(&key), None);

    cache.set(key.clone(), "这是一个登录界面的按钮截图".to_string());
    assert_eq!(
        cache.get(&key),
        Some("这是一个登录界面的按钮截图".to_string())
    );
}

#[tokio::test]
async fn test_upstream_vision_transcription_mock() {
    let mock_server = MockServer::start().await;

    let vision_reply = serde_json::json!({
        "id": "chatcmpl-vision-1",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": "图中展示了一个带有绿色提交按钮的卡密激活界面，包含输入框与错误提示。"
            },
            "finish_reason": "stop"
        }]
    });

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer test-vision-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(vision_reply))
        .mount(&mock_server)
        .await;

    let client = reqwest::Client::new();
    let desc = transcribe_image_with_provider(
        &client,
        &mock_server.uri(),
        "test-vision-key",
        "gpt-4o-mini",
        "png",
        "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==",
        Some("解释这张截图中的界面"),
        512,
    )
    .await
    .expect("Vision transcription succeeds");

    assert!(desc.contains("绿色提交按钮"));

    let formatted = format_fallback_description(1, "png", 120, Some((64, 32)), Some(&desc));
    assert!(formatted.contains("[视觉降级/Vision Fallback - 图片转文字 #1]"));
    assert!(formatted.contains("绿色提交按钮"));
}
