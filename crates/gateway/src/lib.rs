//! `gateway`: Kiro IDE reverse proxy facade, authentication, and translation gateway.

pub const SERVICE_NAME: &str = "kiro-byok-gateway";

pub mod auth;
pub mod facade;
pub mod guardrail;
pub mod idempotency;
pub mod notification;
pub mod ops;
pub mod provider;
pub mod security;
pub mod stream;
pub mod translate;
pub mod usage_estimate;
pub mod watchdog;

pub use notification::{now_secs, NotificationChannel, NotificationDispatcher, NotificationEvent};
pub use ops::{
    CardRateLimiter, DbJitterBuffer, GatewayMetricsCollector, HealthStatus,
    InflightConcurrencyGate, InflightPermit, JitterBufferError, RateLimitError,
    ShutdownCoordinator, ShutdownGuard,
};
pub use security::{
    BruteForceConfig, BruteForceError, BruteForceProtector, ContentGuardrailConfig,
    DualRouterBuilder, DualServerConfig, GuardrailError,
};
pub use watchdog::{WatchdogConfig, WatchdogError, WatchdogStream};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_service_name() {
        assert_eq!(SERVICE_NAME, "kiro-byok-gateway");
    }
}
