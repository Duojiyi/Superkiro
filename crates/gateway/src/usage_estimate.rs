//! Token estimates for when an upstream does not report usage.
//!
//! The estimate reserves credit before a request and bills it when a provider omits
//! usage, so both errors cost someone: too low and the prepaid cap is defeated and the
//! shortfall becomes uncollectable debt; too high and the customer is overcharged.
//! Counting `chars / 4` over the serialized request did both at once — it billed a 1 MB
//! image's base64 as ~200,000 tokens (about 130× what providers charge for it) and
//! under-counted Chinese about fourfold, since each CJK character is roughly one token.

/// Upper bound of the providers' per-image formulas after their own resizing
/// (Anthropic caps an image near 1,600 tokens).
pub const IMAGE_TOKENS: u64 = 1_600;

/// Size of `text` in quarter-token units. ASCII letters, digits and spaces are about
/// four characters per token; a run of ASCII punctuation is about one token per four
/// characters, however short, which is what makes code and JSON denser than prose.
/// CJK ideographs, kana, hangul and fullwidth forms are about one token per character;
/// other non-ASCII scripts about two characters per token.
pub fn token_units(text: &str) -> u64 {
    let mut units = 0u64;
    let mut punctuation_run = 0u64;
    for c in text.chars() {
        if c.is_ascii_punctuation() {
            if punctuation_run.is_multiple_of(4) {
                units += 4;
            }
            punctuation_run += 1;
            continue;
        }
        punctuation_run = 0;
        units += match c as u32 {
            0..=0x7F => 1,
            0x1100..=0x11FF
            | 0x2E80..=0x9FFF
            | 0xAC00..=0xD7AF
            | 0xF900..=0xFAFF
            | 0xFE30..=0xFE4F
            | 0xFF00..=0xFFEF
            | 0x20000..=0x3FFFF => 4,
            _ => 2,
        };
    }
    units
}

/// Whole tokens for a unit count, rounded up.
pub fn tokens_from_units(units: u64) -> u64 {
    units.div_ceil(4)
}

/// Estimate the input tokens of a request from its JSON: a Kiro request, or the
/// translated provider request. Image payloads, Kiro's `images` or a `data:image/` URL,
/// count at a fixed cost and their bytes are skipped; everything else, including tool
/// schemas, tool results and editor state, counts as text.
pub fn estimate_json_tokens(value: &serde_json::Value) -> u64 {
    tokens_from_units(json_units(value))
}

fn json_units(value: &serde_json::Value) -> u64 {
    use serde_json::Value;
    match value {
        Value::String(text) if text.starts_with("data:image/") => IMAGE_TOKENS * 4,
        Value::String(text) => token_units(text),
        Value::Array(items) => items.iter().map(json_units).sum(),
        Value::Object(map) => map
            .iter()
            .map(|(key, value)| match (key.as_str(), value) {
                ("images", Value::Array(images)) => images.len() as u64 * IMAGE_TOKENS * 4,
                _ => token_units(key) + json_units(value),
            })
            .sum(),
        Value::Null | Value::Bool(_) | Value::Number(_) => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn punctuation_runs_count_like_tokens() {
        // `{"`, `":"`, `","` and `"}` each cost about a token, and letters four a token:
        // nine tokens with a real tokenizer, where four characters a token said eight.
        let json = r#"{"type":"object","path":"src"}"#;
        assert_eq!(tokens_from_units(token_units(json)), 10);
        // Long runs still grow: one token per four characters.
        assert_eq!(tokens_from_units(token_units(&"=".repeat(16))), 4);
        // Prose is barely affected.
        assert_eq!(
            tokens_from_units(token_units("The quick brown fox jumps over the lazy dog.")),
            12
        );
    }

    #[test]
    fn a_translated_image_url_counts_as_an_image() {
        let url = format!("data:image/png;base64,{}", "A".repeat(1_400_000));
        let request = json!({"messages": [{"role": "user", "content": [
            {"type": "text", "text": "what is this"},
            {"type": "image_url", "image_url": {"url": url}}
        ]}]});
        let estimate = estimate_json_tokens(&request);
        assert!(
            (IMAGE_TOKENS..IMAGE_TOKENS + 100).contains(&estimate),
            "one image estimated at {estimate} tokens"
        );
    }

    #[test]
    fn scripts_are_weighted_by_how_providers_tokenize_them() {
        assert_eq!(tokens_from_units(token_units("abcdefgh")), 2);
        // One token per ideograph: the old `chars / 4` billed this as one token.
        assert_eq!(tokens_from_units(token_units("你好世界")), 4);
        assert_eq!(tokens_from_units(token_units("こんにちは")), 5);
        assert_eq!(tokens_from_units(token_units("안녕")), 2);
        assert_eq!(tokens_from_units(token_units("привет")), 3);
    }

    #[test]
    fn an_image_costs_what_a_provider_charges_not_its_base64_length() {
        let base64 = "A".repeat(1_400_000);
        let request = json!({"conversationState": {"currentMessage": {"userInputMessage": {
            "content": "what is this",
            "images": [{"format": "png", "source": {"bytes": base64}}]
        }}}});
        let estimate = estimate_json_tokens(&request);
        assert!(
            (IMAGE_TOKENS..IMAGE_TOKENS + 100).contains(&estimate),
            "one image estimated at {estimate} tokens"
        );
    }

    #[test]
    fn tool_results_and_history_still_count_as_text() {
        let text = "x".repeat(4_000);
        let request = json!({"history": [{"toolResults": [{"content": [{"text": text}]}]}]});
        assert!(estimate_json_tokens(&request) >= 1_000);
    }
}
