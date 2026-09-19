//! Rate limiting and upstream concurrency gate.
//!
//! Spec §8 (Operations: Rate Limiting & Upstream In-flight Concurrency Gate).

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Instant;
use thiserror::Error;

#[derive(Error, Debug, PartialEq, Eq)]
pub enum RateLimitError {
    #[error("Rate limit exceeded for card '{card_id}': limit is {limit} QPS")]
    CardQpsExceeded { card_id: String, limit: u32 },

    #[error("Upstream capacity saturated: {current} inflight requests reaches maximum capacity of {max}")]
    UpstreamCapacitySaturated { current: usize, max: usize },
}

/// Token bucket state for a single entity (card or IP).
#[derive(Debug, Clone)]
struct TokenBucket {
    tokens: f64,
    last_update: Instant,
    capacity: f64,
    refill_rate_per_sec: f64,
}

impl TokenBucket {
    fn new(capacity: f64, refill_rate_per_sec: f64) -> Self {
        Self {
            tokens: capacity,
            last_update: Instant::now(),
            capacity,
            refill_rate_per_sec,
        }
    }

    fn try_consume(&mut self, cost: f64) -> bool {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_update).as_secs_f64();
        self.last_update = now;

        self.tokens = (self.tokens + elapsed * self.refill_rate_per_sec).min(self.capacity);

        if self.tokens >= cost {
            self.tokens -= cost;
            true
        } else {
            false
        }
    }
}

/// In-memory card QPS rate limiter using token bucket algorithm.
#[derive(Clone, Debug)]
#[allow(dead_code)] // ponytail: rate limiter ready, pending middleware mount
pub struct CardRateLimiter {
    qps_limit: u32,
    buckets: Arc<RwLock<HashMap<String, TokenBucket>>>,
}

impl CardRateLimiter {
    pub fn new(qps_limit: u32) -> Self {
        Self {
            qps_limit,
            buckets: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Attempt to acquire rate limit permit for card.
    pub fn check_and_consume(&self, card_id: &str) -> Result<(), RateLimitError> {
        let mut buckets = self.buckets.write().unwrap();
        let bucket = buckets
            .entry(card_id.to_string())
            .or_insert_with(|| TokenBucket::new(self.qps_limit as f64, self.qps_limit as f64));

        if bucket.try_consume(1.0) {
            Ok(())
        } else {
            Err(RateLimitError::CardQpsExceeded {
                card_id: card_id.to_string(),
                limit: self.qps_limit,
            })
        }
    }
}

/// Global upstream in-flight concurrency gate (Spec §8: BYOK_MAX_INFLIGHT).
#[derive(Debug, Clone)]
pub struct InflightConcurrencyGate {
    max_inflight: usize,
    current_inflight: Arc<AtomicUsize>,
}

impl InflightConcurrencyGate {
    pub fn new(max_inflight: usize) -> Self {
        Self {
            max_inflight,
            current_inflight: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Try to acquire an inflight slot. Returns RAII permit on success.
    pub fn try_acquire(&self) -> Result<InflightPermit, RateLimitError> {
        let mut current = self.current_inflight.load(Ordering::SeqCst);
        loop {
            if current >= self.max_inflight {
                return Err(RateLimitError::UpstreamCapacitySaturated {
                    current,
                    max: self.max_inflight,
                });
            }

            match self.current_inflight.compare_exchange_weak(
                current,
                current + 1,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => {
                    return Ok(InflightPermit {
                        counter: Arc::clone(&self.current_inflight),
                    });
                }
                Err(actual) => current = actual,
            }
        }
    }

    pub fn current_count(&self) -> usize {
        self.current_inflight.load(Ordering::SeqCst)
    }

    pub fn max_capacity(&self) -> usize {
        self.max_inflight
    }
}

/// RAII permit that decrements the in-flight counter when dropped.
#[derive(Debug)]
pub struct InflightPermit {
    counter: Arc<AtomicUsize>,
}

impl Drop for InflightPermit {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_card_rate_limiter_allows_burst_then_blocks() {
        let limiter = CardRateLimiter::new(2);
        let card = "card-test-limit";

        // Consume 2 permits (burst capacity)
        assert!(limiter.check_and_consume(card).is_ok());
        assert!(limiter.check_and_consume(card).is_ok());

        // 3rd permit immediately fails
        let err = limiter.check_and_consume(card);
        assert_eq!(
            err,
            Err(RateLimitError::CardQpsExceeded {
                card_id: card.to_string(),
                limit: 2
            })
        );
    }

    #[test]
    fn test_inflight_concurrency_gate_raii() {
        let gate = InflightConcurrencyGate::new(2);
        assert_eq!(gate.current_count(), 0);

        let permit1 = gate.try_acquire().unwrap();
        assert_eq!(gate.current_count(), 1);

        let permit2 = gate.try_acquire().unwrap();
        assert_eq!(gate.current_count(), 2);

        // 3rd attempt is saturated
        assert!(gate.try_acquire().is_err());

        // Drop permit1 -> slot freed
        drop(permit1);
        assert_eq!(gate.current_count(), 1);

        // Can acquire again
        let permit3 = gate.try_acquire().unwrap();
        assert_eq!(gate.current_count(), 2);

        drop(permit2);
        drop(permit3);
        assert_eq!(gate.current_count(), 0);
    }
}
