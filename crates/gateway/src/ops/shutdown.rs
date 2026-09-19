//! Graceful shutdown coordination and in-flight stream drainage.
//!
//! Spec §8 (Operations & Reliability: Graceful Shutdown).
//! Prevents rolling deployments from aborting user sessions mid-flight or leaking held reservations.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::time::sleep;

#[derive(Clone, Debug)]
#[allow(dead_code)] // ponytail: coordinator ready, pending integration with main.rs
pub struct ShutdownCoordinator {
    is_shutting_down: Arc<AtomicBool>,
    inflight_count: Arc<AtomicUsize>,
}

impl Default for ShutdownCoordinator {
    fn default() -> Self {
        Self::new()
    }
}

impl ShutdownCoordinator {
    pub fn new() -> Self {
        Self {
            is_shutting_down: Arc::new(AtomicBool::new(false)),
            inflight_count: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Check if the gateway is in the process of shutting down.
    pub fn is_shutting_down(&self) -> bool {
        self.is_shutting_down.load(Ordering::SeqCst)
    }

    /// Initiate graceful shutdown: stop accepting new requests.
    pub fn initiate_shutdown(&self) {
        self.is_shutting_down.store(true, Ordering::SeqCst);
    }

    /// Track a new in-flight stream request. Returns `None` if already shutting down.
    pub fn track_request(&self) -> Option<ShutdownGuard> {
        if self.is_shutting_down() {
            return None;
        }

        self.inflight_count.fetch_add(1, Ordering::SeqCst);
        Some(ShutdownGuard {
            counter: Arc::clone(&self.inflight_count),
        })
    }

    /// Current number of active in-flight streams.
    pub fn inflight_streams(&self) -> usize {
        self.inflight_count.load(Ordering::SeqCst)
    }

    /// Wait for all in-flight streams to complete or until max timeout is reached.
    pub async fn wait_drain(&self, timeout: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        while tokio::time::Instant::now() < deadline {
            if self.inflight_streams() == 0 {
                return true; // Clean drain completed!
            }
            sleep(Duration::from_millis(50)).await;
        }
        self.inflight_streams() == 0
    }
}

/// RAII guard representing an active request during graceful shutdown tracking.
#[derive(Debug)]
pub struct ShutdownGuard {
    counter: Arc<AtomicUsize>,
}

impl Drop for ShutdownGuard {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_shutdown_coordinator_lifecycle() {
        let coord = ShutdownCoordinator::new();
        assert!(!coord.is_shutting_down());

        let guard1 = coord.track_request().unwrap();
        let guard2 = coord.track_request().unwrap();
        assert_eq!(coord.inflight_streams(), 2);

        // Initiate shutdown
        coord.initiate_shutdown();
        assert!(coord.is_shutting_down());

        // New requests are rejected
        assert!(coord.track_request().is_none());

        // Drop guard1
        drop(guard1);
        assert_eq!(coord.inflight_streams(), 1);

        // Background drain task
        let coord_clone = coord.clone();
        tokio::spawn(async move {
            sleep(Duration::from_millis(100)).await;
            drop(guard2);
        });

        // Drain finishes cleanly within 1s
        let drained = coord_clone.wait_drain(Duration::from_secs(1)).await;
        assert!(drained);
        assert_eq!(coord.inflight_streams(), 0);
    }
}
