//! Gateway operational excellence and resilience module.
//!
//! Spec §8 (Operations, Backup/PITR, Rate Limiting, Watchdogs, Metrics, Shutdown, Jitter Buffer).

pub mod jitter_buffer;
pub mod metrics;
pub mod ratelimit;
pub mod shutdown;

pub use jitter_buffer::{DbJitterBuffer, JitterBufferError};
pub use metrics::{GatewayMetricsCollector, HealthStatus};
pub use ratelimit::{CardRateLimiter, InflightConcurrencyGate, InflightPermit, RateLimitError};
pub use shutdown::{ShutdownCoordinator, ShutdownGuard};
