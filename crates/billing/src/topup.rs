//! Top-up and renewal codes for card balance and validity extension (Spec §14.9).

use crate::card::{hash_card_code, hex_encode};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};

/// High-entropy renewal / top-up code entity (Spec §14.9).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TopupCode {
    pub id: String,
    pub code_hash: String,
    pub credit_amount: i64, // In micro-credits (1 credit = 1_000_000 micro-credits)
    pub duration_extension_secs: u64, // Seconds to extend validity period
    pub is_used: bool,
    pub used_by_card_id: Option<String>,
    pub used_at: Option<u64>,
    pub created_at: u64,
}

impl TopupCode {
    pub fn new(
        id: impl Into<String>,
        code_hash: impl Into<String>,
        credit_amount: i64,
        duration_extension_secs: u64,
        created_at: u64,
    ) -> Self {
        Self {
            id: id.into(),
            code_hash: code_hash.into(),
            credit_amount,
            duration_extension_secs,
            is_used: false,
            used_by_card_id: None,
            used_at: None,
            created_at,
        }
    }
}

/// Pairing of plaintext top-up code and its hashed entity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeneratedTopupCode {
    pub raw_code: String,
    pub topup: TopupCode,
}

/// Generate a high-entropy raw top-up code (128-bit).
/// Format: `topup-xxxx-xxxx-xxxx-xxxx-xxxx-xxxx-xxxx-xxxx`
pub fn generate_raw_topup_code(rng: &SystemRandom) -> Result<String, crate::card::CardError> {
    let mut bytes = [0u8; 16];
    rng.fill(&mut bytes)
        .map_err(|_| crate::card::CardError::GenerationFailed)?;
    let hex = hex_encode(&bytes);

    let mut code = String::with_capacity(6 + 32 + 8);
    code.push_str("topup");
    for (i, c) in hex.chars().enumerate() {
        if i % 4 == 0 {
            code.push('-');
        }
        code.push(c);
    }
    Ok(code)
}

/// Generate a single top-up code.
pub fn generate_topup_code(
    credit_amount: i64,
    duration_extension_secs: u64,
    now_secs: u64,
) -> Result<GeneratedTopupCode, crate::card::CardError> {
    let rng = SystemRandom::new();
    let raw_code = generate_raw_topup_code(&rng)?;
    let code_hash = hash_card_code(&raw_code);
    let id = format!("topup-{}", &code_hash[..16]);

    let topup = TopupCode::new(
        id,
        code_hash,
        credit_amount,
        duration_extension_secs,
        now_secs,
    );

    Ok(GeneratedTopupCode { raw_code, topup })
}

/// Generate a batch of top-up codes.
pub fn generate_topup_batch(
    credit_amount: i64,
    duration_extension_secs: u64,
    count: usize,
    now_secs: u64,
) -> Result<Vec<GeneratedTopupCode>, crate::card::CardError> {
    let rng = SystemRandom::new();
    let mut batch = Vec::with_capacity(count);

    for _ in 0..count {
        let raw_code = generate_raw_topup_code(&rng)?;
        let code_hash = hash_card_code(&raw_code);
        let id = format!("topup-{}", &code_hash[..16]);

        let topup = TopupCode::new(
            id,
            code_hash,
            credit_amount,
            duration_extension_secs,
            now_secs,
        );

        batch.push(GeneratedTopupCode { raw_code, topup });
    }

    Ok(batch)
}

/// Export generated top-up codes to CSV format.
pub fn export_topup_csv(codes: &[GeneratedTopupCode]) -> String {
    let mut csv = String::from("id,raw_code,credit_amount,duration_extension_secs,created_at\n");
    for g in codes {
        csv.push_str(&format!(
            "{},{},{},{},{}\n",
            g.topup.id,
            g.raw_code,
            g.topup.credit_amount,
            g.topup.duration_extension_secs,
            g.topup.created_at,
        ));
    }
    csv
}

/// Export generated top-up codes to JSON format.
pub fn export_topup_json(codes: &[GeneratedTopupCode]) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(codes)
}
