//! Health check endpoint for container health checks, client diagnostics, and load balancers.

use super::{BoxFuture, FacadeHandler, Response};
use crate::ops::GatewayMetricsCollector;
use axum::{
    body::Body,
    http::{header, Method, Request, StatusCode},
    response::IntoResponse,
};
use billing::engine::BillingEngine;
use serde::Serialize;
use std::time::Instant;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthzResponse {
    pub status: &'static str,
    pub service: &'static str,
    pub uptime_secs: u64,
    pub version: &'static str,
    pub persistence_ready: bool,
}

#[derive(Clone)]
pub struct HealthzHandler {
    started_at: Instant,
    billing: Option<BillingEngine>,
}

impl Default for HealthzHandler {
    fn default() -> Self {
        Self {
            started_at: Instant::now(),
            billing: None,
        }
    }
}

impl HealthzHandler {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_billing(mut self, billing: BillingEngine) -> Self {
        self.billing = Some(billing);
        self
    }
}

impl FacadeHandler for HealthzHandler {
    fn method(&self) -> Method {
        Method::GET
    }

    fn path(&self) -> &'static str {
        "/healthz"
    }

    fn handle<'a>(&'a self, _req: Request<Body>) -> BoxFuture<'a, Response> {
        Box::pin(async move {
            let persistence_ready = self
                .billing
                .as_ref()
                .map(BillingEngine::persistence_ready)
                .unwrap_or(true);
            let resp = HealthzResponse {
                status: if persistence_ready {
                    "healthy"
                } else {
                    "degraded"
                },
                service: crate::SERVICE_NAME,
                uptime_secs: self.started_at.elapsed().as_secs(),
                version: env!("CARGO_PKG_VERSION"),
                persistence_ready,
            };

            (
                if persistence_ready {
                    StatusCode::OK
                } else {
                    StatusCode::SERVICE_UNAVAILABLE
                },
                [(header::CONTENT_TYPE, "application/json")],
                axum::Json(resp),
            )
                .into_response()
        })
    }
}

#[derive(Clone, Default)]
pub struct MetricsHandler {
    pub collector: GatewayMetricsCollector,
}

impl MetricsHandler {
    pub fn new(collector: GatewayMetricsCollector) -> Self {
        Self { collector }
    }
}

impl FacadeHandler for MetricsHandler {
    fn method(&self) -> Method {
        Method::GET
    }

    fn path(&self) -> &'static str {
        "/metrics"
    }

    fn handle<'a>(&'a self, _req: Request<Body>) -> BoxFuture<'a, Response> {
        Box::pin(async move {
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "text/plain; version=0.0.4")],
                self.collector.render_prometheus(),
            )
                .into_response()
        })
    }
}
