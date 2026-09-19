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
