//! Card entity, status lifecycle, and balance checking (Spec §5, §7).
//!
//! All credit calculations use integer micro-credits (1 credit = 1_000_000 micro-credits)
//! to prevent floating-point drift.
//!
//! Features:
//! - High-entropy key hashing (SHA-256) and constant-time verification.
//! - "激活即计时" (activation begins the timer).
//! - Card status lifecycle (Unactivated -> Active -> Expired/Frozen/Banned).

use crate::template::CardTemplate;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Status lifecycle of a card key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CardStatus {
    Unactivated,
    Active,
    Frozen,
    Banned,
    Expired,
    Voided,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CardError {
    #[error("Card is not active: status is {0:?}")]
    NotActive(CardStatus),

    #[error("Card validity period has expired")]
    Expired,

    #[error("Insufficient credit: available {available} micro-credits, needed {needed}")]
    InsufficientCredit { available: i64, needed: i64 },

    #[error("Card code generation failed")]
    GenerationFailed,
    #[error("Legacy multiple-device bindings require explicit administrator resolution")]
    MultipleDevices,
    #[error("New card issuance requires max_devices=1")]
    InvalidDeviceLimit,
}

/// Card model representing a user account (Identity is Card).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Card {
    pub id: String,
    pub code_hash: String,
    pub template_id: Option<String>,
    /// Activation duration inherited from the issuing template. `None` is used
    /// for legacy cards whose template metadata is unavailable; those cards use
    /// the gateway's explicit legacy default rather than client input.
    #[serde(default)]
    pub activation_duration_secs: Option<u64>,
    pub group_id: String,
    pub credit_total: i64,
    /// Immutable issued entitlement, absent for legacy snapshots.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issued_credits: Option<i64>,
    pub credit_used: i64,
    pub credit_reserved: i64,
    pub status: CardStatus,
    pub activated_at: Option<u64>,
    pub valid_until: Option<u64>,
    pub max_devices: u32,
    pub bound_devices: Vec<String>,
    pub rebind_count: u32,
    pub max_rebinds: u32,
    pub last_rebind_at: Option<u64>,
    pub rebind_cooldown_secs: u64,
    pub max_concurrency: u32,
    pub daily_credit_limit: Option<i64>,
    pub monthly_credit_limit: Option<i64>,
    pub token_version: u64,
    /// Monotonic refresh-token family version. Rotating a refresh token must
    /// not revoke unrelated access sessions, so it is separate from
    /// `token_version` (which is used for card-wide emergency revocation).
    #[serde(default = "default_refresh_version")]
    pub refresh_version: u64,
    pub note: Option<String>,
    pub created_at: u64,
}

impl Card {
    /// Convenience constructor for development, unit tests, and seed records.
    pub fn new(id: impl Into<String>, group_id: impl Into<String>, credit_total: i64) -> Self {
        Self {
            id: id.into(),
            code_hash: String::new(),
            template_id: None,
            activation_duration_secs: None,
            group_id: group_id.into(),
            credit_total,
            issued_credits: Some(credit_total),
            credit_used: 0,
            credit_reserved: 0,
            status: CardStatus::Unactivated,
            activated_at: None,
            valid_until: None,
            max_devices: 1,
            bound_devices: Vec::new(),
            rebind_count: 0,
            max_rebinds: 5,
            last_rebind_at: None,
            rebind_cooldown_secs: 86_400,
            max_concurrency: 5,
            daily_credit_limit: None,
            monthly_credit_limit: None,
            token_version: 1,
            refresh_version: 1,
            note: None,
            created_at: 0,
        }
    }

    /// Construct an unactivated card from a CardTemplate.
    pub fn from_template(
        id: impl Into<String>,
        code_hash: impl Into<String>,
        template: &CardTemplate,
        note: Option<String>,
        created_at: u64,
    ) -> Self {
        Self {
            id: id.into(),
            code_hash: code_hash.into(),
            template_id: Some(template.id.clone()),
            activation_duration_secs: Some(template.duration_secs),
            group_id: template.group_id.clone(),
            credit_total: template.credit_total,
            issued_credits: Some(template.credit_total),
            credit_used: 0,
            credit_reserved: 0,
            status: CardStatus::Unactivated,
            activated_at: None,
            valid_until: None,
            max_devices: 1,
            bound_devices: Vec::new(),
            rebind_count: 0,
            max_rebinds: 5,
            last_rebind_at: None,
            rebind_cooldown_secs: 86_400,
            max_concurrency: template.max_concurrency,
            daily_credit_limit: template.daily_credit_limit,
            monthly_credit_limit: template.monthly_credit_limit,
            token_version: 1,
            refresh_version: 1,
            note,
            created_at,
        }
    }

    /// Available uncommitted and unreserved balance in micro-credits.
    pub fn available_credits(&self) -> i64 {
        self.credit_total
            .saturating_sub(self.credit_used.saturating_add(self.credit_reserved))
            .max(0)
    }

    /// Our service entitlement, never an official Kiro subscription.
    pub fn plan_name(&self) -> Option<&'static str> {
        match self.issued_credits? {
            1_000_000_000 => Some("PRO"),
            2_000_000_000 => Some("PRO+"),
            5_000_000_000 => Some("PRO Max"),
            10_000_000_000 => Some("Power"),
            _ => None,
        }
    }

    pub fn plan_type(&self) -> &'static str {
        match self.plan_name() {
            Some("PRO") => "PRO",
            Some("PRO+") => "PRO_PLUS",
            Some("PRO Max") => "PRO_MAX",
            Some("Power") => "POWER",
            _ => "CUSTOM",
        }
    }

    pub fn check_device_policy(&self) -> Result<(), CardError> {
        if self.bound_devices.len() > 1 {
            return Err(CardError::MultipleDevices);
        }
        Ok(())
    }

    /// Actual unpaid consumption, excluding holds. A topup reduces this amount
    /// without deleting the original debit/audit evidence.
    pub fn outstanding_debt(&self) -> i64 {
        self.credit_used.saturating_sub(self.credit_total).max(0)
    }

    /// Activate card key on first login (Spec §5: valid_until = activated_at + duration).
    ///
    /// "激活即计时": Unactivated cards begin their validity duration upon activation.
    pub fn activate(&mut self, now_secs: u64, duration_secs: u64) -> Result<(), CardError> {
        self.check_device_policy()?;
        match self.status {
            CardStatus::Frozen => Err(CardError::NotActive(CardStatus::Frozen)),
            CardStatus::Banned => Err(CardError::NotActive(CardStatus::Banned)),
            CardStatus::Voided => Err(CardError::NotActive(CardStatus::Voided)),
            CardStatus::Expired => Err(CardError::Expired),
            CardStatus::Active => Ok(()), // Already active
            CardStatus::Unactivated => {
                self.activated_at = Some(now_secs);
                // Template-issued cards carry the authoritative duration. The argument
                // remains a compatibility fallback for legacy cards without metadata.
                let effective_duration = self.activation_duration_secs.unwrap_or(duration_secs);
                if effective_duration > 0 {
                    self.valid_until = Some(now_secs.saturating_add(effective_duration));
                } else {
                    self.valid_until = None; // Perpetual
                }
                self.status = CardStatus::Active;
                Ok(())
            }
        }
    }

    /// Verify card is active and not expired.
    pub fn check_active(&self, now_secs: u64) -> Result<(), CardError> {
        self.check_device_policy()?;
        if self.status != CardStatus::Active {
            return Err(CardError::NotActive(self.status));
        }
        if let Some(until) = self.valid_until {
            if now_secs >= until {
                return Err(CardError::Expired);
            }
        }
        Ok(())
    }

    /// Verify card can cover the required credit reservation amount.
    pub fn check_can_reserve(
        &self,
        needed_micro_credits: i64,
        now_secs: u64,
    ) -> Result<(), CardError> {
        self.check_active(now_secs)?;
        let available = self.available_credits();
        if available < needed_micro_credits {
            return Err(CardError::InsufficientCredit {
                available,
                needed: needed_micro_credits,
            });
        }
        Ok(())
    }
}

fn default_refresh_version() -> u64 {
    1
}

/// Normalize card key input (trim, lowercase, strip hyphens/spaces).
pub fn normalize_card_code(raw: &str) -> String {
    raw.trim().to_ascii_lowercase().replace(['-', ' '], "")
}

/// Encode byte slice into lowercase hexadecimal string.
pub fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(char::from(b"0123456789abcdef"[(b >> 4) as usize]));
        s.push(char::from(b"0123456789abcdef"[(b & 0xf) as usize]));
    }
    s
}

/// Hash a card code using SHA-256 for secure storage (Spec §7).
pub fn hash_card_code(raw_code: &str) -> String {
    let normalized = normalize_card_code(raw_code);
    let digest = ring::digest::digest(&ring::digest::SHA256, normalized.as_bytes());
    hex_encode(digest.as_ref())
}

/// Compare two byte slices in constant time.
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (&x, &y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Verify a raw card code against an expected SHA-256 hash in constant time.
pub fn verify_card_code(raw_code: &str, expected_hash: &str) -> bool {
    let actual_hash = hash_card_code(raw_code);
    constant_time_eq(actual_hash.as_bytes(), expected_hash.as_bytes())
}
