//! Card key generator and batch export utilities (Spec §5, §7, §14.9).
//!
//! Features:
//! - Cryptographically secure ≥128-bit random card key generation.
//! - Batch generation with template parameter binding.
//! - CSV and JSON format exporters for operational distribution.

use crate::card::{hash_card_code, hex_encode, Card, CardError};
use crate::template::CardTemplate;
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};

/// High-entropy card key pairing with plaintext secret code (Spec §5, §7).
///
/// Plaintext `raw_code` is only exposed upon generation or export and NEVER stored in DB.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeneratedCard {
    pub raw_code: String,
    pub card: Card,
}

/// Generate a cryptographically secure 128-bit random card code.
///
/// Format: `kiro-xxxx-xxxx-xxxx-xxxx-xxxx-xxxx-xxxx-xxxx` (32 hex characters = 128 bits entropy).
pub fn generate_raw_code(rng: &SystemRandom) -> Result<String, CardError> {
    let mut bytes = [0u8; 16]; // 16 bytes = 128 bits entropy
    rng.fill(&mut bytes)
        .map_err(|_| CardError::GenerationFailed)?;
    let hex = hex_encode(&bytes);

    let mut code = String::with_capacity(5 + 32 + 7);
    code.push_str("kiro");
    for (i, c) in hex.chars().enumerate() {
        if i % 4 == 0 {
            code.push('-');
        }
        code.push(c);
    }
    Ok(code)
}

/// Generate a single unactivated card key from a template.
pub fn generate_card(
    template: &CardTemplate,
    note: Option<&str>,
    now_secs: u64,
) -> Result<GeneratedCard, CardError> {
    if template.max_devices != 1 {
        return Err(CardError::InvalidDeviceLimit);
    }
    let rng = SystemRandom::new();
    let raw_code = generate_raw_code(&rng)?;
    let code_hash = hash_card_code(&raw_code);
    let card_id = format!("card-{}", &code_hash[..16]);

    let card = Card::from_template(
        card_id,
        code_hash,
        template,
        note.map(ToString::to_string),
        now_secs,
    );

    Ok(GeneratedCard { raw_code, card })
}

/// Generate a batch of cards from a template (Spec §14.9).
pub fn generate_batch(
    template: &CardTemplate,
    count: usize,
    note_prefix: Option<&str>,
    now_secs: u64,
) -> Result<Vec<GeneratedCard>, CardError> {
    if template.max_devices != 1 {
        return Err(CardError::InvalidDeviceLimit);
    }
    let rng = SystemRandom::new();
    let mut cards = Vec::with_capacity(count);

    for i in 0..count {
        let raw_code = generate_raw_code(&rng)?;
        let code_hash = hash_card_code(&raw_code);
        let card_id = format!("card-{}", &code_hash[..16]);

        let note = note_prefix.map(|p| format!("{}-#{}", p, i + 1));
        let card = Card::from_template(card_id, code_hash, template, note, now_secs);

        cards.push(GeneratedCard { raw_code, card });
    }

    Ok(cards)
}

/// Export generated cards to CSV format.
pub fn export_csv(cards: &[GeneratedCard]) -> String {
    let mut csv = String::from(
        "id,raw_code,code_hash,template_id,group_id,credit_total,status,note,created_at\n",
    );
    for g in cards {
        let template_id = g.card.template_id.as_deref().unwrap_or("");
        let note = g.card.note.as_deref().unwrap_or("").replace('"', "\"\"");
        let status_str = match serde_json::to_string(&g.card.status) {
            Ok(s) => s.trim_matches('"').to_string(),
            Err(_) => "unactivated".to_string(),
        };
        csv.push_str(&format!(
            "{},{},{},{},{},{},{},\"{}\",{}\n",
            g.card.id,
            g.raw_code,
            g.card.code_hash,
            template_id,
            g.card.group_id,
            g.card.credit_total,
            status_str,
            note,
            g.card.created_at,
        ));
    }
    csv
}

/// Export generated cards to formatted JSON.
pub fn export_json(cards: &[GeneratedCard]) -> String {
    serde_json::to_string_pretty(cards).unwrap_or_else(|_| "[]".to_string())
}
