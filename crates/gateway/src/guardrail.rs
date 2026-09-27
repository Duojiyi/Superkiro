//! Capacity guardrails for fast-fail overload protection and Kiro retry signals (Spec §14.9).
//!
//! When global or upstream capacity is saturated, instead of queuing and hanging
//! (which risks hitting Kiro's 300s session idle watchdog), the gateway fails fast
//! and returns HTTP 429 Too Many Requests with AWS SDK-compatible `ThrottlingException`
//! and `Retry-After` headers to trigger client adaptive backoff without errors.

use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CapacityError {
    #[error("Capacity limit saturated ({current}/{max}), retry after {retry_after_secs}s")]
    CapacitySaturated {
        current: usize,
        max: usize,
        retry_after_secs: u64,
    },
}

/// RAII permit for in-flight requests that releases capacity on drop.
#[derive(Debug)]
pub struct CapacityPermit {
    current: Arc<AtomicUsize>,
}

impl Drop for CapacityPermit {
    fn drop(&mut self) {
        self.current.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Global / upstream capacity guardrail.
#[derive(Debug, Clone)]
pub struct CapacityGuardrail {
    max_concurrency: usize,
    current_concurrency: Arc<AtomicUsize>,
    retry_after_secs: u64,
}

impl Default for CapacityGuardrail {
    fn default() -> Self {
        Self::new(100, 2)
    }
}

impl CapacityGuardrail {
    pub fn new(max_concurrency: usize, retry_after_secs: u64) -> Self {
        Self {
            max_concurrency,
            current_concurrency: Arc::new(AtomicUsize::new(0)),
            retry_after_secs,
        }
    }

    /// Current number of active in-flight permits.
    pub fn current_concurrency(&self) -> usize {
        self.current_concurrency.load(Ordering::SeqCst)
    }

    /// Maximum allowed concurrent requests.
    pub fn max_concurrency(&self) -> usize {
        self.max_concurrency
    }

    /// Configured Retry-After interval in seconds.
    pub fn retry_after_secs(&self) -> u64 {
        self.retry_after_secs
    }

    /// Try to acquire an execution permit.
    ///
    /// If concurrency limit is reached, returns `CapacitySaturated` immediately
    /// without queuing or blocking.
    pub fn try_acquire(&self) -> Result<CapacityPermit, CapacityError> {
        let mut curr = self.current_concurrency.load(Ordering::SeqCst);
        loop {
            if curr >= self.max_concurrency {
                return Err(CapacityError::CapacitySaturated {
                    current: curr,
                    max: self.max_concurrency,
                    retry_after_secs: self.retry_after_secs,
                });
            }
            match self.current_concurrency.compare_exchange_weak(
                curr,
                curr + 1,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => {
                    return Ok(CapacityPermit {
                        current: Arc::clone(&self.current_concurrency),
                    });
                }
                Err(actual) => curr = actual,
            }
        }
    }

    /// Build a Kiro-compatible HTTP 429 response when capacity is saturated.
    pub fn build_retry_response(&self) -> Response {
        format_kiro_throttle_response(
            StatusCode::TOO_MANY_REQUESTS,
            "ThrottlingException",
            "CAPACITY_LIMIT_REACHED",
            &format!(
                "Upstream capacity limit reached. Please retry after {} seconds.",
                self.retry_after_secs
            ),
            Some(self.retry_after_secs),
        )
    }
}

/// Conversations with a body over 10 MB are read and processed at most two at a time across
/// the gateway. Until it is translated, one is held about three times over (read, parsed,
/// archived), close to 100 MB at the 32 MB limit, and the gateway runs in 1 GB: five at once
/// would be an out-of-memory kill for every customer. One that finds both places taken
/// waits a few seconds for one to free up, then is throttled, which Kiro retries. Smaller
/// bodies never wait.
#[derive(Debug, Clone)]
pub struct LargeBodyGate {
    places: Arc<tokio::sync::Semaphore>,
    max: usize,
    threshold: usize,
    wait: std::time::Duration,
}

impl Default for LargeBodyGate {
    fn default() -> Self {
        Self::new(2, 10 * 1024 * 1024, std::time::Duration::from_secs(3))
    }
}

impl LargeBodyGate {
    /// Retry-After of a large body that found no place.
    pub const RETRY_AFTER_SECS: u64 = 2;

    /// `max` bodies over `threshold` bytes at once; another waits up to `wait` for a place.
    pub fn new(max: usize, threshold: usize, wait: std::time::Duration) -> Self {
        Self {
            places: Arc::new(tokio::sync::Semaphore::new(max)),
            max,
            threshold,
            wait,
        }
    }

    /// The body size over which a conversation needs a place.
    pub fn threshold(&self) -> usize {
        self.threshold
    }

    /// How many large bodies hold a place now.
    pub fn in_use(&self) -> usize {
        self.max - self.places.available_permits()
    }

    /// A place for one large body, given back when it is dropped, or `None` when none freed
    /// up in time.
    pub async fn enter(&self) -> Option<tokio::sync::OwnedSemaphorePermit> {
        tokio::time::timeout(self.wait, self.places.clone().acquire_owned())
            .await
            .ok()?
            .ok()
    }

    /// The answer to a large body that found no place: a throttle Kiro retries.
    pub fn throttled_response(&self) -> Response {
        format_kiro_throttle_response(
            StatusCode::TOO_MANY_REQUESTS,
            "ThrottlingException",
            "LARGE_REQUEST_CAPACITY",
            &format!(
                "Too many large conversations are being processed. Please retry after {} seconds.",
                Self::RETRY_AFTER_SECS
            ),
            Some(Self::RETRY_AFTER_SECS),
        )
    }
}

/// Helper to construct a standard Kiro / AWS SDK structured throttling response.
pub fn format_kiro_throttle_response(
    status: StatusCode,
    error_type: &str,
    reason: &str,
    message: &str,
    retry_after_secs: Option<u64>,
) -> Response {
    let payload = json!({
        "__type": error_type,
        "message": message,
        "reason": reason,
    });

    let mut response = (status, axum::Json(payload)).into_response();
    let headers = response.headers_mut();

    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );

    if let Ok(hv) = HeaderValue::from_str(error_type) {
        headers.insert("x-amzn-errortype", hv);
    }

    if let Some(secs) = retry_after_secs {
        if let Ok(hv) = HeaderValue::from_str(&secs.to_string()) {
            headers.insert(header::RETRY_AFTER, hv);
        }
    }

    response
}
