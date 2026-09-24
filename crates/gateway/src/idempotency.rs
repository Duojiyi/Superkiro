//! Request idempotency and retry deduplication (Spec §4.7).
//!
//! Tracks in-progress and completed invocations keyed by `amz-sdk-invocation-id`.
//! Prevents concurrent duplicate requests from forwarding to upstream providers
//! or triggering double-billing.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Completed invocation metadata cached for idempotency short-circuiting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletedInvocation {
    pub completed_at: Instant,
    pub model_id: String,
    pub total_input_tokens: u32,
    pub total_output_tokens: u32,
}

/// Idempotency rejection errors.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum IdempotencyError {
    #[error("Request with invocation ID '{0}' is already in progress")]
    InProgress(String),

    #[error("Request with invocation ID '{0}' has already completed")]
    AlreadyCompleted(String),

    #[error("Request with invocation ID '{0}' previously failed")]
    AlreadyFailed(String),
}

/// RAII guard representing an in-flight invocation.
///
/// If dropped before calling [`IdempotencyGuard::commit`], automatically aborts
/// and removes the invocation from the in-progress set, allowing client retries.
#[derive(Debug)]
pub struct IdempotencyGuard {
    invocation_id: String,
    manager: IdempotencyManager,
    committed: bool,
}

impl IdempotencyGuard {
    /// Mark the invocation as successfully completed and cache its result.
    pub fn commit(mut self, record: CompletedInvocation) {
        self.committed = true;
        self.manager.commit(&self.invocation_id, record);
    }

    /// Permanently record a terminal failure after billable partial work was
    /// settled.  Retrying the same invocation must not execute or charge it a
    /// second time.
    pub fn fail(mut self) {
        self.committed = true;
        self.manager.fail(&self.invocation_id);
    }

    /// Invocation ID tracked by this guard.
    pub fn invocation_id(&self) -> &str {
        &self.invocation_id
    }
}

impl Drop for IdempotencyGuard {
    fn drop(&mut self) {
        if !self.committed {
            self.manager.abort(&self.invocation_id);
        }
    }
}

#[derive(Debug)]
struct IdempotencyStore {
    in_progress: HashMap<String, Instant>,
    completed: HashMap<String, CompletedInvocation>,
    failed: HashMap<String, Instant>,
    ttl: Duration,
    /// Expiry is swept at most once a second, not on every request.
    last_sweep: Option<Instant>,
}

impl IdempotencyStore {
    fn clean_expired(&mut self, now: Instant) {
        if self
            .last_sweep
            .is_some_and(|last| now.duration_since(last) < Duration::from_secs(1))
        {
            return;
        }
        self.last_sweep = Some(now);
        self.completed
            .retain(|_, v| now.duration_since(v.completed_at) < self.ttl);
        self.failed
            .retain(|_, timestamp| now.duration_since(*timestamp) < self.ttl);
    }

    /// Keep the finished invocations remembered for replay protection within a bound,
    /// forgetting the oldest first. In-progress ones are bounded by request concurrency.
    fn make_room(&mut self) {
        if self.completed.len() + self.failed.len() < MAX_REMEMBERED {
            return;
        }
        let mut finished: Vec<(Instant, String)> = self
            .completed
            .iter()
            .map(|(id, record)| (record.completed_at, id.clone()))
            .chain(self.failed.iter().map(|(id, at)| (*at, id.clone())))
            .collect();
        finished.sort_unstable();
        for (_, id) in finished.into_iter().take(MAX_REMEMBERED / 10) {
            self.completed.remove(&id);
            self.failed.remove(&id);
        }
    }
}

/// Finished invocations remembered at most, across completed and failed.
const MAX_REMEMBERED: usize = 50_000;

/// In-memory idempotency deduplication manager.
#[derive(Debug, Clone)]
pub struct IdempotencyManager {
    inner: Arc<Mutex<IdempotencyStore>>,
}

impl Default for IdempotencyManager {
    fn default() -> Self {
        Self::new(Duration::from_secs(600)) // 10 minutes default TTL
    }
}

impl IdempotencyManager {
    /// Create a new idempotency manager with the specified completed cache TTL.
    pub fn new(ttl: Duration) -> Self {
        Self {
            inner: Arc::new(Mutex::new(IdempotencyStore {
                in_progress: HashMap::new(),
                completed: HashMap::new(),
                failed: HashMap::new(),
                ttl,
                last_sweep: None,
            })),
        }
    }

    /// Attempt to acquire an execution lock for an `amz-sdk-invocation-id`.
    ///
    /// Returns:
    /// - `Ok(guard)`: New invocation; caller may proceed to upstream.
    /// - `Err(InProgress)`: Duplicate concurrent request; caller should reject (409 Conflict).
    /// - `Err(AlreadyCompleted)`: Request already completed; caller should short-circuit.
    pub fn try_acquire(&self, invocation_id: &str) -> Result<IdempotencyGuard, IdempotencyError> {
        let mut store = self.inner.lock().unwrap();
        let now = Instant::now();
        store.clean_expired(now);
        store.make_room();

        if store.in_progress.contains_key(invocation_id) {
            return Err(IdempotencyError::InProgress(invocation_id.to_string()));
        }

        // Expiry is swept lazily, so an entry still present may already have expired.
        let ttl = store.ttl;
        if store
            .completed
            .get(invocation_id)
            .is_some_and(|record| now.duration_since(record.completed_at) < ttl)
        {
            return Err(IdempotencyError::AlreadyCompleted(
                invocation_id.to_string(),
            ));
        }

        if store
            .failed
            .get(invocation_id)
            .is_some_and(|at| now.duration_since(*at) < ttl)
        {
            return Err(IdempotencyError::AlreadyFailed(invocation_id.to_string()));
        }

        store.in_progress.insert(invocation_id.to_string(), now);

        Ok(IdempotencyGuard {
            invocation_id: invocation_id.to_string(),
            manager: self.clone(),
            committed: false,
        })
    }

    /// Check whether an invocation is currently in progress.
    pub fn is_in_progress(&self, invocation_id: &str) -> bool {
        let store = self.inner.lock().unwrap();
        store.in_progress.contains_key(invocation_id)
    }

    /// Check whether an invocation was completed and is still cached.
    pub fn get_completed(&self, invocation_id: &str) -> Option<CompletedInvocation> {
        let mut store = self.inner.lock().unwrap();
        let now = Instant::now();
        store.clean_expired(now);
        let ttl = store.ttl;
        store
            .completed
            .get(invocation_id)
            .filter(|record| now.duration_since(record.completed_at) < ttl)
            .cloned()
    }

    pub(crate) fn commit(&self, invocation_id: &str, record: CompletedInvocation) {
        let mut store = self.inner.lock().unwrap();
        store.in_progress.remove(invocation_id);
        store.completed.insert(invocation_id.to_string(), record);
    }

    pub(crate) fn abort(&self, invocation_id: &str) {
        let mut store = self.inner.lock().unwrap();
        store.in_progress.remove(invocation_id);
    }

    pub(crate) fn fail(&self, invocation_id: &str) {
        let mut store = self.inner.lock().unwrap();
        store.in_progress.remove(invocation_id);
        store
            .failed
            .insert(invocation_id.to_string(), Instant::now());
    }
}

#[cfg(test)]
mod bound_tests {
    use super::*;

    #[test]
    fn finished_invocations_are_remembered_within_a_bound() {
        let manager = IdempotencyManager::new(Duration::from_secs(600));
        for n in 0..(MAX_REMEMBERED + 100) {
            manager.try_acquire(&format!("id-{n}")).unwrap().fail();
        }
        let store = manager.inner.lock().unwrap();
        assert!(store.completed.len() + store.failed.len() <= MAX_REMEMBERED);
        // The newest are the ones kept.
        assert!(store
            .failed
            .contains_key(&format!("id-{}", MAX_REMEMBERED + 99)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_idempotency_happy_path_commit() {
        let manager = IdempotencyManager::new(Duration::from_secs(60));
        let id = "test-invocation-1";

        let guard = manager
            .try_acquire(id)
            .expect("Should acquire first attempt");
        assert!(manager.is_in_progress(id));

        // Concurrent attempt rejected
        let err = manager.try_acquire(id).unwrap_err();
        assert_eq!(err, IdempotencyError::InProgress(id.to_string()));

        // Commit completion
        guard.commit(CompletedInvocation {
            completed_at: Instant::now(),
            model_id: "claude-sonnet".to_string(),
            total_input_tokens: 100,
            total_output_tokens: 50,
        });

        assert!(!manager.is_in_progress(id));
        let completed = manager
            .get_completed(id)
            .expect("Should find completed record");
        assert_eq!(completed.model_id, "claude-sonnet");

        // Subsequent attempt rejected as AlreadyCompleted
        let err2 = manager.try_acquire(id).unwrap_err();
        assert_eq!(err2, IdempotencyError::AlreadyCompleted(id.to_string()));
    }

    #[test]
    fn test_idempotency_abort_on_drop_allows_retry() {
        let manager = IdempotencyManager::new(Duration::from_secs(60));
        let id = "test-invocation-abort";

        {
            let _guard = manager.try_acquire(id).expect("Acquire");
            assert!(manager.is_in_progress(id));
            // Drops here without commit (simulating client disconnect / upstream failure)
        }

        assert!(!manager.is_in_progress(id));
        assert!(manager.get_completed(id).is_none());

        // Subsequent retry can now acquire successfully!
        let _guard2 = manager.try_acquire(id).expect("Retry should succeed");
    }

    #[test]
    fn test_idempotency_ttl_expiration() {
        let manager = IdempotencyManager::new(Duration::from_millis(50));
        let id = "test-invocation-expire";

        let guard = manager.try_acquire(id).unwrap();
        guard.commit(CompletedInvocation {
            completed_at: Instant::now(),
            model_id: "m1".to_string(),
            total_input_tokens: 10,
            total_output_tokens: 20,
        });

        assert!(manager.get_completed(id).is_some());
        std::thread::sleep(Duration::from_millis(60));

        // After TTL expires, completed entry is pruned, allowing new invocation
        assert!(manager.get_completed(id).is_none());
        let _guard2 = manager.try_acquire(id).expect("Should acquire after TTL");
    }
}
