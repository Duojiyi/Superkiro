//! Database jitter and temporary outage isolation.
//!
//! Spec §8 (Operations: DB Jitter Isolation / Local Fallback Queue).
//! Prevents momentary Postgres drops from causing all user requests to fail with 5xx.

use billing::card::Card;
use billing::ledger::LedgerEntry;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, RwLock};
use std::time::Instant;
use thiserror::Error;

#[derive(Error, Debug, PartialEq, Eq)]
pub enum JitterBufferError {
    #[error(
        "Local fallback ledger queue full: capacity of {capacity} reached; rejecting new write"
    )]
    QueueFull { capacity: usize },
}

/// In-memory bounded ledger replay queue and auth cache during database jitter (Spec §8).
#[derive(Clone, Debug)]
#[allow(dead_code)] // ponytail: jitter buffer ready, pending billing integration
pub struct DbJitterBuffer {
    max_queue_size: usize,
    auth_cache_ttl_secs: u64,
    pending_ledger: Arc<RwLock<VecDeque<LedgerEntry>>>,
    auth_cache: Arc<RwLock<HashMap<String, (Card, Instant)>>>,
}

impl DbJitterBuffer {
    pub fn new(max_queue_size: usize, auth_cache_ttl_secs: u64) -> Self {
        Self {
            max_queue_size,
            auth_cache_ttl_secs,
            pending_ledger: Arc::new(RwLock::new(VecDeque::new())),
            auth_cache: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Cache active card info for resilient offline auth during DB jitter.
    pub fn cache_card(&self, card: Card) {
        let mut cache = self.auth_cache.write().unwrap();
        cache.insert(card.id.clone(), (card, Instant::now()));
    }

    /// Try to retrieve card from short TTL cache if DB is unreachable.
    pub fn get_cached_card(&self, card_id: &str) -> Option<Card> {
        let cache = self.auth_cache.read().unwrap();
        if let Some((card, cached_at)) = cache.get(card_id) {
            if cached_at.elapsed().as_secs() < self.auth_cache_ttl_secs {
                return Some(card.clone());
            }
        }
        None
    }

    /// Enqueue a completed ledger settlement to local queue when DB write fails.
    pub fn enqueue_ledger_entry(&self, entry: LedgerEntry) -> Result<(), JitterBufferError> {
        let mut queue = self.pending_ledger.write().unwrap();
        if queue.len() >= self.max_queue_size {
            return Err(JitterBufferError::QueueFull {
                capacity: self.max_queue_size,
            });
        }
        queue.push_back(entry);
        Ok(())
    }

    /// Number of queued ledger entries awaiting replay.
    pub fn pending_count(&self) -> usize {
        self.pending_ledger.read().unwrap().len()
    }

    /// Drain queued entries for batch replay once DB recovers.
    pub fn drain_pending_entries(&self) -> Vec<LedgerEntry> {
        let mut queue = self.pending_ledger.write().unwrap();
        queue.drain(..).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_jitter_buffer_card_caching_and_ttl() {
        let buffer = DbJitterBuffer::new(100, 2);
        let card = Card::new("card-jitter", "grp-1", 10_000_000);
        buffer.cache_card(card.clone());

        // Cache hit
        let hit = buffer.get_cached_card("card-jitter");
        assert!(hit.is_some());
        assert_eq!(hit.unwrap().id, "card-jitter");

        // Non-existent card
        assert!(buffer.get_cached_card("non-existent").is_none());
    }

    fn make_test_entry(id: &str, credits: i64) -> LedgerEntry {
        LedgerEntry {
            id: id.to_string(),
            card_id: "card-1".to_string(),
            kind: billing::ledger::LedgerKind::Usage,
            invocation_id: Some(format!("inv-{id}")),
            exposed_model: "m".to_string(),
            provider_id: "p".to_string(),
            target_model: "t".to_string(),
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            credits_charged: credits,
            provider_cost_micro_cny: 10,
            rate_card_version: Some("v1".to_string()),
            ts_secs: 1000,
            operator_id: None,
            reason: None,
        }
    }

    #[test]
    fn test_jitter_buffer_ledger_enqueue_and_drain() {
        let buffer = DbJitterBuffer::new(2, 60);

        let entry1 = make_test_entry("l-1", 100);
        let entry2 = make_test_entry("l-2", 200);
        let entry3 = make_test_entry("l-3", 300);

        assert!(buffer.enqueue_ledger_entry(entry1).is_ok());
        assert!(buffer.enqueue_ledger_entry(entry2).is_ok());

        // Buffer full (capacity = 2)
        assert!(matches!(
            buffer.enqueue_ledger_entry(entry3),
            Err(JitterBufferError::QueueFull { capacity: 2 })
        ));

        assert_eq!(buffer.pending_count(), 2);

        // Drain for replay
        let drained = buffer.drain_pending_entries();
        assert_eq!(drained.len(), 2);
        assert_eq!(buffer.pending_count(), 0);
    }
}
