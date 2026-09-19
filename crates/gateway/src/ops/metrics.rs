//! Health check and Prometheus metrics exposition.
//!
//! Spec §8 (Operations: /healthz and /metrics endpoints).

use futures_util::{Stream, StreamExt};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

#[derive(Debug, Serialize, Deserialize)]
pub struct HealthStatus {
    pub status: String,
    pub uptime_secs: u64,
    pub inflight_requests: usize,
    pub active_reservations: usize,
    pub providers_healthy: usize,
    pub providers_cooldown: usize,
}

/// Thread-safe Prometheus metrics collector for gateway operations.
#[derive(Clone, Debug)]
pub struct GatewayMetricsCollector {
    started_at: Instant,
    requests_success: Arc<AtomicU64>,
    requests_error: Arc<AtomicU64>,
    total_credits_settled: Arc<AtomicU64>,
    current_inflight: Arc<AtomicUsize>,
}

impl Default for GatewayMetricsCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl GatewayMetricsCollector {
    pub fn new() -> Self {
        Self {
            started_at: Instant::now(),
            requests_success: Arc::new(AtomicU64::new(0)),
            requests_error: Arc::new(AtomicU64::new(0)),
            total_credits_settled: Arc::new(AtomicU64::new(0)),
            current_inflight: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub fn record_request_success(&self, credits_charged: i64) {
        self.requests_success.fetch_add(1, Ordering::Relaxed);
        self.record_credits(credits_charged);
    }

    pub fn record_credits(&self, credits_charged: i64) {
        if credits_charged > 0 {
            self.total_credits_settled
                .fetch_add(credits_charged as u64, Ordering::Relaxed);
        }
    }

    pub fn record_request_error(&self) {
        self.requests_error.fetch_add(1, Ordering::Relaxed);
    }

    pub fn set_inflight(&self, count: usize) {
        self.current_inflight.store(count, Ordering::Relaxed);
    }

    pub fn begin_request(&self) {
        self.current_inflight.fetch_add(1, Ordering::Relaxed);
    }

    pub fn end_request(&self) {
        self.current_inflight.fetch_sub(1, Ordering::Relaxed);
    }

    pub fn record_response(&self, success: bool) {
        if success {
            self.requests_success.fetch_add(1, Ordering::Relaxed);
        } else {
            self.requests_error.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn uptime_secs(&self) -> u64 {
        self.started_at.elapsed().as_secs()
    }

    /// Produce health status JSON structure for `/healthz`.
    pub fn get_health(
        &self,
        active_reservations: usize,
        healthy_providers: usize,
        cooldown_providers: usize,
    ) -> HealthStatus {
        HealthStatus {
            status: "healthy".to_string(),
            uptime_secs: self.uptime_secs(),
            inflight_requests: self.current_inflight.load(Ordering::Relaxed),
            active_reservations,
            providers_healthy: healthy_providers,
            providers_cooldown: cooldown_providers,
        }
    }

    /// Render standard Prometheus text exposition format (Spec §8).
    pub fn render_prometheus(&self) -> String {
        let uptime = self.uptime_secs();
        let succ = self.requests_success.load(Ordering::Relaxed);
        let err = self.requests_error.load(Ordering::Relaxed);
        let credits = self.total_credits_settled.load(Ordering::Relaxed);
        let inflight = self.current_inflight.load(Ordering::Relaxed);

        format!(
            "# HELP kiro_gateway_uptime_seconds Total runtime uptime in seconds\n\
             # TYPE kiro_gateway_uptime_seconds counter\n\
             kiro_gateway_uptime_seconds {}\n\
             # HELP kiro_gateway_requests_total Total number of HTTP requests processed\n\
             # TYPE kiro_gateway_requests_total counter\n\
             kiro_gateway_requests_total{{status=\"success\"}} {}\n\
             kiro_gateway_requests_total{{status=\"error\"}} {}\n\
             # HELP kiro_gateway_inflight_requests Current number of active in-flight requests\n\
             # TYPE kiro_gateway_inflight_requests gauge\n\
             kiro_gateway_inflight_requests {}\n\
             # HELP kiro_gateway_credits_settled_total Total micro-credits successfully billed\n\
             # TYPE kiro_gateway_credits_settled_total counter\n\
             kiro_gateway_credits_settled_total {}\n",
            uptime, succ, err, inflight, credits
        )
    }
}

/// Shared with the event-stream producer so in-band exceptions count as errors.
#[derive(Clone, Debug)]
pub struct RequestMetrics {
    pub collector: GatewayMetricsCollector,
    failed: Arc<AtomicBool>,
}

impl RequestMetrics {
    pub fn mark_error(&self) {
        self.failed.store(true, Ordering::Relaxed);
    }
}

struct ResponseObservation {
    metrics: RequestMetrics,
    completed: bool,
}

impl Drop for ResponseObservation {
    fn drop(&mut self) {
        self.metrics.collector.end_request();
        self.metrics
            .collector
            .record_response(self.completed && !self.metrics.failed.load(Ordering::Relaxed));
    }
}

struct ObservedBody {
    inner: std::pin::Pin<Box<dyn Stream<Item = Result<bytes::Bytes, axum::Error>> + Send>>,
    observation: ResponseObservation,
}

impl Stream for ObservedBody {
    type Item = Result<bytes::Bytes, axum::Error>;
    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        let result = self.inner.as_mut().poll_next(cx);
        match &result {
            std::task::Poll::Ready(None) => self.observation.completed = true,
            std::task::Poll::Ready(Some(Err(_))) => self.observation.metrics.mark_error(),
            _ => {}
        }
        result
    }
}

pub async fn metrics_middleware(
    axum::extract::State(metrics): axum::extract::State<GatewayMetricsCollector>,
    mut req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    metrics.begin_request();
    let observation = ResponseObservation {
        metrics: RequestMetrics {
            collector: metrics,
            failed: Arc::new(AtomicBool::new(false)),
        },
        completed: false,
    };
    req.extensions_mut().insert(observation.metrics.clone());
    let response = next.run(req).await;
    if !response.status().is_success() {
        observation.metrics.mark_error();
    }
    let (parts, body) = response.into_parts();
    let stream = ObservedBody {
        inner: body.into_data_stream().boxed(),
        observation,
    };
    axum::response::Response::from_parts(parts, axum::body::Body::from_stream(stream))
}
