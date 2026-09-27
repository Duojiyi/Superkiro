//! What balance adjustments gave back for the requests they name, and why a compensation is
//! refused.
//!
//! An adjustment that makes up for requests names them in its detail: `invocationIds`, and
//! `invocationId` as well when it names exactly one (older releases wrote only that), with
//! `chargedMicroCredits`, what each was charged, in the same order. A positive one names
//! them as compensated. One naming several shares its amount among them in proportion to
//! what each was charged (evenly when none was), so the shares add up to the amount.
//!
//! Every balance adjustment also records its kind and, for a refund or a correction, the
//! money that went with it: `kind` and `cashMicroCny`.

use crate::ledger::{LedgerEntry, LedgerKind};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// The card event an upgrade or a renewal writes; it moves credits, but is a sale, not an
/// adjustment.
pub const UPGRADE_EVENT: &str = "upgrade_card";

/// The most money one adjustment or upgrade records: ¥100,000, the dearest plan.
pub const MAX_CASH_MICRO_CNY: i64 = 100_000 * 1_000_000;

/// What a balance adjustment is, as the financials count it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdjustmentKind {
    /// Credits given back for requests.
    Compensation,
    /// Credits given for nothing owed.
    Gift,
    /// Money returned to the customer, the credits it paid for taken back.
    Refund,
    /// A balance set right.
    Correction,
}

impl AdjustmentKind {
    pub const ALL: [Self; 4] = [
        Self::Compensation,
        Self::Gift,
        Self::Refund,
        Self::Correction,
    ];

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.as_str() == name)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Compensation => "compensation",
            Self::Gift => "gift",
            Self::Refund => "refund",
            Self::Correction => "correction",
        }
    }

    /// The kind an adjustment is when none is said: a correction when it takes credits, a
    /// compensation when it gives them for requests it names, else a gift.
    pub fn default_for(delta_micro_credits: i64, names_requests: bool) -> Self {
        if delta_micro_credits < 0 {
            Self::Correction
        } else if names_requests {
            Self::Compensation
        } else {
            Self::Gift
        }
    }

    /// Why money cannot go with this kind as asked, if it cannot: a refund says what was
    /// returned, and a compensation or a gift returns none.
    pub fn cash_problem(self, cash_micro_cny: Option<i64>) -> Option<&'static str> {
        match (self, cash_micro_cny) {
            (_, Some(cash)) if !(0..=MAX_CASH_MICRO_CNY).contains(&cash) => {
                Some("cashMicroCny must be 0-100000000000 micro-CNY")
            }
            (Self::Refund, None) => {
                Some("A refund needs cashMicroCny, the money returned to the customer")
            }
            (Self::Compensation | Self::Gift, Some(_)) => {
                Some("cashMicroCny is refused for a compensation or a gift")
            }
            _ => None,
        }
    }
}

/// A kept balance adjustment's kind and the money it records: its own, or, for one made
/// before kinds, the kind its sign and requests give. `None` for an entry that is not a
/// balance adjustment: usage, a top-up, a card event, or an upgrade.
pub fn adjustment_kind(entry: &LedgerEntry) -> Option<(AdjustmentKind, Option<i64>)> {
    if entry.kind != LedgerKind::Adjustment
        || entry.credits_charged == 0
        || entry.exposed_model == UPGRADE_EVENT
    {
        return None;
    }
    let detail = entry.event_detail();
    let cash = detail
        .as_ref()
        .and_then(|detail| detail.get("cashMicroCny"))
        .and_then(serde_json::Value::as_i64);
    let kind = detail
        .as_ref()
        .and_then(|detail| detail.get("kind"))
        .and_then(serde_json::Value::as_str)
        .and_then(AdjustmentKind::parse)
        .unwrap_or_else(|| {
            AdjustmentKind::default_for(
                entry.credits_charged,
                !linked_requests(detail.as_ref()).is_empty(),
            )
        });
    Some((kind, cash))
}

/// The money an upgrade or a renewal records the customer paid; `None` for another entry.
pub fn upgrade_cash(entry: &LedgerEntry) -> Option<i64> {
    (entry.kind == LedgerKind::Adjustment && entry.exposed_model == UPGRADE_EVENT).then(|| {
        entry
            .event_detail()
            .and_then(|detail| {
                detail
                    .get("cashMicroCny")
                    .and_then(serde_json::Value::as_i64)
            })
            .unwrap_or(0)
    })
}

/// Most requests one adjustment makes up for.
pub const MAX_COMPENSATED_REQUESTS: usize = 50;

/// What one positive adjustment gave one request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Compensation {
    /// Its share of the adjustment, in micro-credits.
    pub micro_credits: i64,
    pub at_secs: u64,
    pub operator: Option<String>,
    pub reason: Option<String>,
}

/// Every positive adjustment that made up for one request: their shares added up, and the
/// latest of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestCompensation {
    pub total_micro_credits: i64,
    pub latest: Compensation,
}

/// The requests an adjustment makes up for, as its detail names them.
pub fn linked_requests(detail: Option<&serde_json::Value>) -> Vec<String> {
    let Some(detail) = detail else {
        return Vec::new();
    };
    if let Some(ids) = detail
        .get("invocationIds")
        .and_then(serde_json::Value::as_array)
    {
        return ids
            .iter()
            .filter_map(serde_json::Value::as_str)
            .map(str::to_string)
            .collect();
    }
    detail
        .get("invocationId")
        .and_then(serde_json::Value::as_str)
        .map(|id| vec![id.to_string()])
        .unwrap_or_default()
}

/// The detail a balance adjustment records: its kind, the money that went with it, and the
/// requests it makes up for, `charged` being what each was charged.
pub(crate) fn adjustment_detail(
    kind: AdjustmentKind,
    cash_micro_cny: Option<i64>,
    ids: &[&str],
    charged: &[i64],
) -> serde_json::Value {
    let mut detail = serde_json::json!({ "kind": kind });
    if let Some(cash) = cash_micro_cny {
        detail["cashMicroCny"] = serde_json::json!(cash);
    }
    if !ids.is_empty() {
        detail["invocationIds"] = serde_json::json!(ids);
        detail["chargedMicroCredits"] = serde_json::json!(charged);
    }
    if let [only] = ids {
        detail["invocationId"] = serde_json::json!(only);
    }
    detail
}

/// `amount` shared in proportion to `weights` (evenly when they add up to nothing), rounded
/// down along the running total so that the shares add up to it exactly.
fn split(amount: i64, weights: &[i64]) -> Vec<i64> {
    let mut weights: Vec<i128> = weights.iter().map(|w| i128::from((*w).max(0))).collect();
    if weights.iter().sum::<i128>() <= 0 {
        weights.iter_mut().for_each(|w| *w = 1);
    }
    let total: i128 = weights.iter().sum();
    let (mut running, mut given) = (0i128, 0i128);
    weights
        .iter()
        .map(|weight| {
            running += weight;
            let upto = i128::from(amount) * running / total;
            let share = upto - given;
            given = upto;
            share as i64
        })
        .collect()
}

/// Each request an adjustment names, with its share of the adjustment's amount.
fn shares(entry: &LedgerEntry) -> Vec<(String, i64)> {
    let detail = entry.event_detail();
    let ids = linked_requests(detail.as_ref());
    let charged: Vec<i64> = detail
        .as_ref()
        .and_then(|detail| detail.get("chargedMicroCredits"))
        .and_then(serde_json::Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(serde_json::Value::as_i64)
                .collect()
        })
        .unwrap_or_default();
    let weights = if charged.len() == ids.len() {
        charged
    } else {
        vec![1; ids.len()]
    };
    let shares = split(entry.credits_charged, &weights);
    ids.into_iter().zip(shares).collect()
}

/// Per card and request, what positive adjustments gave it: kept beside the ledger, updated
/// with each adjustment and rebuilt from the ledger and its archive when the state loads, so
/// the traces never read the ledger for it.
#[derive(Debug, Clone, Default)]
pub(crate) struct CompensationIndex(HashMap<String, HashMap<String, RequestCompensation>>);

impl CompensationIndex {
    /// From every adjustment kept, live or archived, each once.
    pub(crate) fn build<'a>(entries: impl IntoIterator<Item = &'a LedgerEntry>) -> Self {
        let mut seen = HashSet::new();
        let mut adjustments: Vec<&LedgerEntry> = entries
            .into_iter()
            .filter(|entry| is_compensation(entry) && seen.insert(entry.id.as_str()))
            .collect();
        // Oldest first, so the latest is the last one added; stable within a second.
        adjustments.sort_by_key(|entry| entry.ts_secs);
        let mut index = Self::default();
        for entry in adjustments {
            index.add(entry);
        }
        index
    }

    /// Counts one more adjustment, if it compensates requests.
    pub(crate) fn add(&mut self, entry: &LedgerEntry) {
        if !is_compensation(entry) {
            return;
        }
        let card = self.0.entry(entry.card_id.clone()).or_default();
        for (id, share) in shares(entry) {
            let compensation = Compensation {
                micro_credits: share,
                at_secs: entry.ts_secs,
                operator: entry.operator_id.clone(),
                reason: entry.reason.clone(),
            };
            match card.get_mut(&id) {
                Some(known) => {
                    known.total_micro_credits = known.total_micro_credits.saturating_add(share);
                    if entry.ts_secs >= known.latest.at_secs {
                        known.latest = compensation;
                    }
                }
                None => {
                    card.insert(
                        id,
                        RequestCompensation {
                            total_micro_credits: share,
                            latest: compensation,
                        },
                    );
                }
            }
        }
    }

    pub(crate) fn get(&self, card_id: &str, invocation_id: &str) -> Option<&RequestCompensation> {
        self.0.get(card_id)?.get(invocation_id)
    }
}

/// A positive balance adjustment naming requests.
fn is_compensation(entry: &LedgerEntry) -> bool {
    entry.kind == LedgerKind::Adjustment
        && entry.credits_charged > 0
        && entry.event_detail().is_some_and(|detail| {
            detail.get("invocationIds").is_some() || detail.get("invocationId").is_some()
        })
}

/// Why a compensation was refused, as the console reads it besides the message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum RefusalKind {
    /// A request that was not found.
    Unknown,
    /// A request another card made.
    OtherCard,
    /// A request already compensated.
    Repeat,
    /// More than the requests were charged.
    Over,
}

/// One request a refusal names, and what was found for it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RefusedRequest {
    pub invocation_id: String,
    /// The card that made it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub card_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub charged_micro_credits: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub charged_at_secs: Option<u64>,
    /// For a repeat: the earlier compensation, its share for this request, when, by whom and why.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compensated_micro_credits: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compensated_at_secs: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operator: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// A refused compensation: the message, and every request that caused it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompensationRefusal {
    #[serde(skip)]
    pub message: String,
    pub kind: RefusalKind,
    pub requests: Vec<RefusedRequest>,
    /// For an amount above the charge: what was asked, and what the requests were charged.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub asked_micro_credits: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub charged_total_micro_credits: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shares_follow_the_charges_and_add_up_to_the_amount() {
        assert_eq!(split(10, &[1, 1, 1]), vec![3, 3, 4]);
        assert_eq!(
            split(21_588_324, &[3_100_000, 0, 18_488_324])
                .iter()
                .sum::<i64>(),
            21_588_324
        );
        assert_eq!(split(6, &[0, 0]), vec![3, 3]);
        assert_eq!(split(5, &[2, 3]), vec![2, 3]);
    }
}
