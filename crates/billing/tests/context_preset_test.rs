use billing::context::ModelContextLibrary;

#[test]
fn test_builtin_model_presets_resolution() {
    // 1. Claude Sonnet
    let sonnet = ModelContextLibrary::resolve("claude-3-5-sonnet");
    assert_eq!(sonnet.context_window, 200_000);
    assert_eq!(sonnet.max_output, 64_000);
    assert_eq!(sonnet.compression_threshold, 0.80);
    assert!(sonnet.supports_tools);
    assert!(sonnet.supports_vision);

    // 2. Claude with dated version suffix
    let sonnet_dated = ModelContextLibrary::resolve("claude-3-5-sonnet-20241022");
    assert_eq!(sonnet_dated.context_window, 200_000);

    // 3. GPT-4o
    let gpt4o = ModelContextLibrary::resolve("gpt-4o");
    assert_eq!(gpt4o.context_window, 128_000);
    assert_eq!(gpt4o.max_output, 16_384);
    assert_eq!(gpt4o.compression_threshold, 0.80);

    // 4. DeepSeek Chat
    let ds_chat = ModelContextLibrary::resolve("deepseek-chat");
    assert_eq!(ds_chat.context_window, 64_000);
    assert_eq!(ds_chat.compression_threshold, 0.85);

    // 5. DeepSeek Reasoner
    let ds_r1 = ModelContextLibrary::resolve("deepseek-reasoner");
    assert_eq!(ds_r1.context_window, 64_000);
    assert!(ds_r1.supports_reasoning);

    // 6. Gemini 2.0 Flash
    let gemini = ModelContextLibrary::resolve("gemini-2.0-flash");
    assert_eq!(gemini.context_window, 1_048_576);

    // 7. Unknown model safe fallback
    let unknown = ModelContextLibrary::resolve("my-custom-finetuned-llama");
    assert_eq!(unknown.context_window, 128_000);
    assert_eq!(unknown.compression_threshold, 0.80);
}

#[test]
fn test_context_usage_calculation_and_compression_triggers() {
    let model = "claude-3-5-sonnet"; // context_window = 200,000, threshold = 0.80

    // 1. Light usage: 20,000 tokens (10%)
    let m1 = ModelContextLibrary::calculate_usage(model, 20_000);
    assert_eq!(m1.used_tokens, 20_000);
    assert_eq!(m1.context_window, 200_000);
    assert!((m1.percentage - 0.10).abs() < 1e-6);
    assert_eq!(m1.remaining_tokens, 180_000);
    assert!(!m1.should_compress);

    // 2. Near threshold: 150,000 tokens (75%)
    let m2 = ModelContextLibrary::calculate_usage(model, 150_000);
    assert!((m2.percentage - 0.75).abs() < 1e-6);
    assert_eq!(m2.remaining_tokens, 50_000);
    assert!(!m2.should_compress);

    // 3. Exactly at threshold: 160,000 tokens (80%) -> Trigger compression!
    let m3 = ModelContextLibrary::calculate_usage(model, 160_000);
    assert!((m3.percentage - 0.80).abs() < 1e-6);
    assert_eq!(m3.remaining_tokens, 40_000);
    assert!(m3.should_compress);

    // 4. Over threshold: 190,000 tokens (95%)
    let m4 = ModelContextLibrary::calculate_usage(model, 190_000);
    assert!((m4.percentage - 0.95).abs() < 1e-6);
    assert!(m4.should_compress);

    // 5. Overflow safety: 250,000 tokens (> 100%) -> Clamped to 1.0, remaining 0
    let m5 = ModelContextLibrary::calculate_usage(model, 250_000);
    assert_eq!(m5.percentage, 1.0);
    assert_eq!(m5.remaining_tokens, 0);
    assert!(m5.should_compress);
}
