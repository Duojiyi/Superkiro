//! Credit reservation for concurrency protection (Spec §6.2).
//!
//! Freezes estimated upper bound at the start of a request (`credit_reservation`),
//! and settles against real usage on completion, releasing the remainder.
//! Unsettled orphan reservations are automatically reclaimed by the janitor after TTL.

use crate::ledger::ceil_nonnegative_to_i64;
use serde::{Deserialize, Serialize};

/// Lifecycle states of a credit reservation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReservationState {
    Held,
    Settled,
    Released,
}

/// Parameters for estimating the required reservation upper-bound.
#[derive(Debug, Clone, PartialEq)]
pub struct ReservationEstimateParams {
    pub estimated_input_tokens: u64,
    pub max_output_tokens: u64,
    /// Rate in micro-credits per 1M input tokens
    pub input_rate_per_m: i64,
    /// Rate in micro-credits per 1M output tokens
    pub output_rate_per_m: i64,
    pub credit_multiplier: f64,
    pub margin_multiplier: f64,
    pub model: Option<String>,
}

impl ReservationEstimateParams {
    pub fn new(estimated_input_tokens: u64, max_output_tokens: u64) -> Self {
        Self {
            estimated_input_tokens,
            max_output_tokens,
            input_rate_per_m: 15_000_000,
            output_rate_per_m: 60_000_000,
            credit_multiplier: 1.0,
            margin_multiplier: 1.0,
            model: None,
        }
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    /// Calculate required reservation in micro-credits.
    /// Formula: reserve = (input_tokens * input_rate + max_output * output_rate) * multipliers.
    pub fn calculate_reserve_amount(&self) -> i64 {
        let input_cost =
            (self.estimated_input_tokens as f64) * (self.input_rate_per_m as f64) / 1_000_000.0;
        let output_cost =
            (self.max_output_tokens as f64) * (self.output_rate_per_m as f64) / 1_000_000.0;
        let raw = (input_cost + output_cost) * self.credit_multiplier * self.margin_multiplier;
        ceil_nonnegative_to_i64(raw)
    }
}

/// Record tracking an active or completed credit reservation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreditReservation {
    pub id: String,
    pub card_id: String,
    pub invocation_id: String,
    pub reserved_micro_credits: i64,
    pub state: ReservationState,
    pub created_at_secs: u64,
    pub expires_at_secs: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate_card_version: Option<String>,
}

impl CreditReservation {
    pub fn new(
        id: impl Into<String>,
        card_id: impl Into<String>,
        invocation_id: impl Into<String>,
        reserved_micro_credits: i64,
        now_secs: u64,
        ttl_secs: u64,
    ) -> Self {
        Self {
            id: id.into(),
            card_id: card_id.into(),
            invocation_id: invocation_id.into(),
            reserved_micro_credits,
            state: ReservationState::Held,
            created_at_secs: now_secs,
            expires_at_secs: now_secs.saturating_add(ttl_secs),
            rate_card_version: None,
        }
    }

    pub fn with_rate_card_version(mut self, version: impl Into<String>) -> Self {
        self.rate_card_version = Some(version.into());
        self
    }

    /// Check if this held reservation has expired and should be reclaimed by janitor.
    pub fn is_expired(&self, now_secs: u64) -> bool {
        self.state == ReservationState::Held && now_secs >= self.expires_at_secs
    }
}
