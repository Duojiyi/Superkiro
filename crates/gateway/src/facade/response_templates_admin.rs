use super::{admin::AdminAuthState, json_response, BoxFuture, FacadeHandler, Response};
use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
};
use billing::engine::{BillingEngine, BillingError, ResponseTemplateUpdate};
use std::sync::Arc;
pub struct ResponseTemplatesHandler {
    pub billing: BillingEngine,
    pub auth: Arc<AdminAuthState>,
    pub publish: bool,
}
impl FacadeHandler for ResponseTemplatesHandler {
    fn method(&self) -> Method {
        if self.publish {
            Method::POST
        } else {
            Method::GET
        }
    }
    fn path(&self) -> &'static str {
        "/api/v1/admin/response-templates"
    }
    fn handle<'a>(&'a self, req: Request<Body>) -> BoxFuture<'a, Response> {
        Box::pin(async move {
            if !self.auth.verify(req.headers()) {
                return json_response(
                    StatusCode::UNAUTHORIZED,
                    &serde_json::json!({"success":false,"error":"Unauthorized"}),
                );
            }
            if !self.publish {
                return json_response(
                    StatusCode::OK,
                    &serde_json::json!({"success":true,"config":self.billing.response_template_config()}),
                );
            }
            let bytes = match axum::body::to_bytes(req.into_body(), 16 * 1024 * 1024).await {
                Ok(b) => b,
                Err(_) => {
                    return json_response(
                        StatusCode::BAD_REQUEST,
                        &serde_json::json!({"success":false,"error":"Invalid or oversized body"}),
                    )
                }
            };
            let update: ResponseTemplateUpdate = match serde_json::from_slice(&bytes) {
                Ok(v) => v,
                Err(e) => {
                    return json_response(
                        StatusCode::BAD_REQUEST,
                        &serde_json::json!({"success":false,"error":e.to_string()}),
                    )
                }
            };
            match self
                .billing
                .publish_response_templates(update, crate::now_secs())
            {
                Ok(config) => json_response(
                    StatusCode::OK,
                    &serde_json::json!({"success":true,"config":config}),
                ),
                // One that could not be saved says so, without the storage error, which names
                // server paths.
                Err(BillingError::Persistence(_)) => json_response(
                    StatusCode::SERVICE_UNAVAILABLE,
                    &serde_json::json!({"success":false,"error":super::admin::NOT_SAVED}),
                ),
                Err(e) => json_response(
                    StatusCode::CONFLICT,
                    &serde_json::json!({"success":false,"error":e.to_string()}),
                ),
            }
        })
    }
}
