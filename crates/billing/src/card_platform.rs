//! Card dispensing platform integration models and business logic (Spec §14.1, P4-5).
//!
//! Provides:
//! - Inventory pulling and stock queries for third-party automated card shops.
//! - Idempotent order fulfillment using `order_id` deduplication.
//! - Redeem / verification callbacks (activation, balance check, device binding, topup).

use crate::card::CardStatus;
use crate::engine::BillingEngine;
use crate::template::CardTemplate;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

const IDEMPOTENCY_TTL_SECS: u64 = 24 * 60 * 60;
const MAX_IDEMPOTENCY_ENTRIES: usize = 10_000;

/// Stock inventory summary for a template or group.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InventoryStockResponse {
    pub template_id: Option<String>,
    pub group_id: Option<String>,
    pub unactivated_count: usize,
    pub active_count: usize,
    pub total_count: usize,
}

/// Request to pull/dispense a batch of unactivated cards for a customer order.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PullCardsRequest {
    pub order_id: String,
    pub template_id: Option<String>,
    pub group_id: Option<String>,
    pub count: usize,
    pub note: Option<String>,
}

/// Dispensed card item returned to the card platform.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DispensedCardItem {
    pub card_id: String,
    pub raw_code: String,
    pub group_id: String,
    pub initial_credits: i64,
    pub status: CardStatus,
}

/// Response returned from a card pull request.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PullCardsResponse {
    pub order_id: String,
    pub cards: Vec<DispensedCardItem>,
    pub dispensed_at: u64,
}

/// Redeem callback action type.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RedeemAction {
    VerifyOnly,
    Activate { duration_secs: u64 },
    BindDevice { device_id: String },
    Topup { topup_code: String },
}

/// Request payload for card redemption/fulfillment callback from card platform.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RedeemCallbackRequest {
    pub order_id: String,
    pub card_code_or_id: String,
    pub action: RedeemAction,
    pub buyer_contact: Option<String>,
    pub trade_no: Option<String>,
}

/// Response payload returned to the card platform.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RedeemCallbackResponse {
    pub success: bool,
    pub order_id: String,
    pub card_id: String,
    pub status: CardStatus,
    pub remaining_credits: i64,
    pub valid_until: Option<u64>,
    pub message: String,
}

/// Card platform manager coordinating inventory allocation and callback processing.
#[derive(Debug, Clone, Default)]
pub struct CardPlatformManager {
    redeem_cache: Arc<RwLock<HashMap<String, CachedRedeem>>>,
}

#[derive(Debug, Clone)]
struct CachedRedeem {
    fingerprint: String,
    response: RedeemCallbackResponse,
    stored_at: u64,
}

impl CardPlatformManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Query available unactivated inventory count.
    pub fn query_inventory(
        &self,
        billing: &BillingEngine,
        template_id: Option<&str>,
        group_id: Option<&str>,
    ) -> InventoryStockResponse {
        let cards = billing.list_all_cards();
        let mut unactivated = 0;
        let mut active = 0;
        let mut total = 0;

        for c in cards {
            let match_tpl = template_id
                .map(|t| c.template_id.as_deref() == Some(t))
                .unwrap_or(true);
            let match_grp = group_id.map(|g| c.group_id == g).unwrap_or(true);

            if match_tpl && match_grp {
                total += 1;
                match c.status {
                    CardStatus::Unactivated => unactivated += 1,
                    CardStatus::Active => active += 1,
                    _ => {}
                }
            }
        }

        InventoryStockResponse {
            template_id: template_id.map(ToString::to_string),
            group_id: group_id.map(ToString::to_string),
            unactivated_count: unactivated,
            active_count: active,
            total_count: total,
        }
    }

    /// Idempotently pull/generate cards for an order.
    pub fn pull_cards(
        &self,
        billing: &BillingEngine,
        template: &CardTemplate,
        req: &PullCardsRequest,
        now_secs: u64,
    ) -> Result<PullCardsResponse, String> {
        if req.order_id.trim().is_empty() {
            return Err("order_id cannot be empty".to_string());
        }

        // Structured encoding prevents delimiter collisions in user fields.
        let fingerprint = serde_json::to_string(req).map_err(|e| e.to_string())?;

        if req.count == 0 || req.count > 100 {
            return Err("count must be between 1 and 100".to_string());
        }
        if req
            .template_id
            .as_deref()
            .is_some_and(|id| id != template.id)
            || req
                .group_id
                .as_deref()
                .is_some_and(|id| id != template.group_id)
        {
            return Err(
                "requested template/group does not match the configured issuance template"
                    .to_string(),
            );
        }

        let response = billing
            .fulfill_card_order(&req.order_id, &fingerprint, || {
                let mut dispensed = Vec::with_capacity(req.count);
                let mut cards = Vec::with_capacity(req.count);
                for mut generated in billing.generate_recoverable_cards(
                    template,
                    req.count,
                    req.note.as_deref(),
                    now_secs,
                )? {
                    generated.card.note = req.note.clone();
                    dispensed.push(DispensedCardItem {
                        card_id: generated.card.id.clone(),
                        raw_code: generated.raw_code,
                        group_id: generated.card.group_id.clone(),
                        initial_credits: generated.card.credit_total,
                        status: generated.card.status,
                    });
                    cards.push(generated.card);
                }
                let response = PullCardsResponse {
                    order_id: req.order_id.clone(),
                    cards: dispensed,
                    dispensed_at: now_secs,
                };
                let json = serde_json::to_string(&response)
                    .map_err(|e| crate::engine::BillingError::InvalidState(e.to_string()))?;
                Ok((cards, json))
            })
            .map_err(|e| e.to_string())?;
        serde_json::from_str(&response).map_err(|e| e.to_string())
    }

    /// Process verification/activation/topup callback from card shop.
    pub fn process_redeem(
        &self,
        billing: &BillingEngine,
        req: &RedeemCallbackRequest,
        now_secs: u64,
    ) -> Result<RedeemCallbackResponse, String> {
        if req.order_id.trim().is_empty() {
            return Err("order_id cannot be empty".to_string());
        }

        // Check idempotency cache
        let fingerprint = format!("{}|{:?}", req.card_code_or_id, req.action);
        let mut redeem_cache = self
            .redeem_cache
            .write()
            .map_err(|_| "redeem idempotency store unavailable".to_string())?;
        redeem_cache
            .retain(|_, cached| now_secs.saturating_sub(cached.stored_at) < IDEMPOTENCY_TTL_SECS);
        if let Some(cached) = redeem_cache.get(&req.order_id) {
            if cached.fingerprint != fingerprint {
                return Err("order_id was already used with a different callback".to_string());
            }
            return Ok(cached.response.clone());
        }

        let card = billing
            .find_card_by_code_or_id(&req.card_code_or_id)
            .ok_or_else(|| format!("Card not found for identifier: {}", req.card_code_or_id))?;

        let card_id = card.id.clone();

        let resp = match &req.action {
            RedeemAction::VerifyOnly => {
                let remaining = card.available_credits();
                RedeemCallbackResponse {
                    success: true,
                    order_id: req.order_id.clone(),
                    card_id,
                    status: card.status,
                    remaining_credits: remaining,
                    valid_until: card.valid_until,
                    message: "Card verification successful".to_string(),
                }
            }
            RedeemAction::Activate { duration_secs } => {
                if card.status == CardStatus::Unactivated
                    && card.activation_duration_secs != Some(*duration_secs)
                {
                    return Err("activation duration must match the card template".to_string());
                }
                let activated = billing
                    .activate_card(&card_id, now_secs, *duration_secs)
                    .map_err(|e| format!("Activation failed: {e}"))?;
                let remaining = activated.available_credits();
                RedeemCallbackResponse {
                    success: true,
                    order_id: req.order_id.clone(),
                    card_id,
                    status: activated.status,
                    remaining_credits: remaining,
                    valid_until: activated.valid_until,
                    message: "Card activated successfully".to_string(),
                }
            }
            RedeemAction::BindDevice { device_id } => {
                if device_id.trim().is_empty() || device_id.chars().count() > 256 {
                    return Err("device_id is invalid".to_string());
                }
                let now_secs = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                billing
                    .bind_device(&card_id, device_id, now_secs)
                    .map_err(|e| format!("Device bind failed: {e}"))?;
                let c = billing.get_card(&card_id).unwrap_or(card);
                let remaining = c.available_credits();
                RedeemCallbackResponse {
                    success: true,
                    order_id: req.order_id.clone(),
                    card_id,
                    status: c.status,
                    remaining_credits: remaining,
                    valid_until: c.valid_until,
                    message: format!("Device '{device_id}' bound successfully"),
                }
            }
            RedeemAction::Topup { topup_code } => {
                let entry = billing
                    .redeem_topup(&card_id, topup_code, now_secs, "card-platform")
                    .map_err(|e| format!("Topup failed: {e}"))?;
                let updated = billing
                    .get_card(&card_id)
                    .ok_or_else(|| "Card not found after topup".to_string())?;
                let remaining = updated.available_credits();
                RedeemCallbackResponse {
                    success: true,
                    order_id: req.order_id.clone(),
                    card_id,
                    status: updated.status,
                    remaining_credits: remaining,
                    valid_until: updated.valid_until,
                    message: format!("Topup successful: +{} credits added", entry.credits_charged),
                }
            }
        };

        if redeem_cache.len() >= MAX_IDEMPOTENCY_ENTRIES {
            if let Some(oldest) = redeem_cache
                .iter()
                .min_by_key(|(_, cached)| cached.stored_at)
                .map(|(key, _)| key.clone())
            {
                redeem_cache.remove(&oldest);
            }
        }
        redeem_cache.insert(
            req.order_id.clone(),
            CachedRedeem {
                fingerprint,
                response: resp.clone(),
                stored_at: now_secs,
            },
        );

        Ok(resp)
    }
}
