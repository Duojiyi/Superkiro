use super::{admin::AdminAuthState, json_response, BoxFuture, FacadeHandler, Response};
use crate::complexity_routing::{
    apply_decision, digest, ComplexityRouter, RoutingInput, RoutingRequest,
};
use crate::provider::{governance::ProviderKeyPool, ProviderRuntimeRegistry};
use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
};
use billing::engine::{BillingEngine, BillingError, ComplexityRoutingUpdate};
use serde::Deserialize;
use std::sync::Arc;

pub struct ComplexityRoutingHandler {
    pub billing: BillingEngine,
    pub auth: Arc<AdminAuthState>,
    pub publish: bool,
}
fn error(status: StatusCode, message: &str) -> Response {
    json_response(
        status,
        &serde_json::json!({"success":false,"error":message}),
    )
}
fn failed(e: BillingError) -> Response {
    match e {
        BillingError::Persistence(_) => {
            error(StatusCode::SERVICE_UNAVAILABLE, super::admin::NOT_SAVED)
        }
        _ => error(StatusCode::BAD_REQUEST, &e.to_string()),
    }
}
impl FacadeHandler for ComplexityRoutingHandler {
    fn method(&self) -> Method {
        if self.publish {
            Method::POST
        } else {
            Method::GET
        }
    }
    fn path(&self) -> &'static str {
        "/api/v1/admin/complexity-routing"
    }
    fn handle<'a>(&'a self, req: Request<Body>) -> BoxFuture<'a, Response> {
        Box::pin(async move {
            if !self.auth.verify(req.headers()) {
                return error(StatusCode::UNAUTHORIZED, "Unauthorized");
            }
            if !self.publish {
                let state = self.billing.routing_status();
                let retained_decisions = state.decisions.len();
                let mut recent = state.decisions;
                recent.sort_by_key(|d| std::cmp::Reverse(d.created_at_secs));
                recent.truncate(50);
                return json_response(
                    StatusCode::OK,
                    &serde_json::json!({"success":true,
                    "config":state.config,"status":{"budget":state.budget,"retained_decisions":retained_decisions,"recent_decisions":recent}}),
                );
            }
            let bytes = match axum::body::to_bytes(req.into_body(), 256 * 1024).await {
                Ok(v) => v,
                Err(_) => return error(StatusCode::BAD_REQUEST, "Invalid or oversized body"),
            };
            let update: ComplexityRoutingUpdate = match serde_json::from_slice(&bytes) {
                Ok(v) => v,
                Err(_) => return error(StatusCode::BAD_REQUEST, "Invalid routing configuration"),
            };
            if update.expected_revision != self.billing.complexity_routing_config().revision {
                return error(
                    StatusCode::CONFLICT,
                    "Routing configuration changed; reload before publishing",
                );
            }
            match self
                .billing
                .publish_complexity_routing(update, crate::now_secs())
            {
                Ok(config) => json_response(
                    StatusCode::OK,
                    &serde_json::json!({"success":true,"config":config}),
                ),
                Err(e) => {
                    if e.to_string().contains("changed") || e.to_string().contains("revision") {
                        error(
                            StatusCode::CONFLICT,
                            "Routing configuration changed; reload before publishing",
                        )
                    } else {
                        failed(e)
                    }
                }
            }
        })
    }
}

pub struct ComplexityRoutingPreviewHandler {
    pub billing: BillingEngine,
    pub auth: Arc<AdminAuthState>,
    pub runtime: Option<ProviderRuntimeRegistry>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Preview {
    model_map_id: String,
    text: String,
    #[serde(default)]
    history: String,
    #[serde(default)]
    continuation: bool,
    #[serde(default)]
    has_attachments: bool,
}
impl FacadeHandler for ComplexityRoutingPreviewHandler {
    fn method(&self) -> Method {
        Method::POST
    }
    fn path(&self) -> &'static str {
        "/api/v1/admin/complexity-routing/preview"
    }
    fn handle<'a>(&'a self, req: Request<Body>) -> BoxFuture<'a, Response> {
        Box::pin(async move {
            if !self.auth.verify(req.headers()) {
                return error(StatusCode::UNAUTHORIZED, "Unauthorized");
            }
            let bytes = match axum::body::to_bytes(req.into_body(), 96 * 1024).await {
                Ok(v) => v,
                Err(_) => return error(StatusCode::BAD_REQUEST, "Invalid or oversized body"),
            };
            let preview: Preview = match serde_json::from_slice(&bytes) {
                Ok(v) => v,
                Err(_) => return error(StatusCode::BAD_REQUEST, "Invalid preview input"),
            };
            if preview.text.trim().is_empty()
                || preview
                    .text
                    .chars()
                    .count()
                    .saturating_add(preview.history.chars().count())
                    > 16000
            {
                return error(
                    StatusCode::BAD_REQUEST,
                    "Preview requires text and at most 16000 characters of context",
                );
            }
            let config = self.billing.complexity_routing_config();
            let Some(policy) = config
                .policies
                .iter()
                .find(|p| p.model_map_id == preview.model_map_id)
            else {
                return error(
                    StatusCode::BAD_REQUEST,
                    "Save a routing policy for this model first",
                );
            };
            let model = self
                .billing
                .list_groups()
                .iter()
                .flat_map(|g| self.billing.list_models_for_group(&g.id, false))
                .find(|m| m.id == preview.model_map_id && !m.retired);
            let Some(model) = model else {
                return error(StatusCode::BAD_REQUEST, "Model is unavailable");
            };
            let Some(group) = self.billing.get_group(&model.group_id) else {
                return error(StatusCode::BAD_REQUEST, "Group is unavailable");
            };
            let eligible: Vec<_> = model
                .full_target_chain()
                .into_iter()
                .filter_map(|t| {
                    self.billing
                        .get_provider(&t.provider_id)
                        .filter(|p| p.enabled && group.can_access_provider(p.group_id.as_deref()))
                        .map(|_| (t.provider_id, t.target_model))
                })
                .collect();
            let pool = config
                .classifier
                .as_ref()
                .and_then(|c| {
                    self.runtime
                        .as_ref()
                        .and_then(|rt| rt.pool_for(&c.provider_id))
                        .or_else(|| {
                            self.billing.get_provider(&c.provider_id).map(|p| {
                                ProviderKeyPool::new(
                                    p,
                                    self.billing.get_runtime_provider_keys(Some(&c.provider_id)),
                                )
                            })
                        })
                })
                .filter(|pool| group.can_access_provider(pool.provider().group_id.as_deref()));
            let input = RoutingInput {
                input_chars: preview.text.chars().count() + preview.history.chars().count(),
                text: preview.text,
                history: preview.history,
                continuation: preview.continuation,
                has_attachments: preview.has_attachments,
                high_reasoning: false,
                context_complete: true,
            };
            let now = crate::now_secs();
            let mut nonce = [0u8; 16];
            if ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut nonce).is_err()
            {
                return error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Preview is temporarily unavailable",
                );
            }
            let invocation = format!("preview:{}", digest(&nonce));
            let request = RoutingRequest {
                scope: digest(&(&invocation, &model.id)),
                request_hash: digest(&input),
                invocation_id: invocation,
                input,
                eligible_provider_ids: eligible.iter().map(|(id, _)| id.clone()).collect(),
                preview: true,
                now,
            };
            match ComplexityRouter::shared()
                .decide(&self.billing, &config, policy, request, pool)
                .await
            {
                Ok(decision) => {
                    let applied = apply_decision(&eligible, &decision);
                    json_response(
                        StatusCode::OK,
                        &serde_json::json!({"success":true,"decision":decision,
                        "eligible_provider_ids":eligible.iter().map(|(id,_)| id).collect::<Vec<_>>(),
                        "applied_provider_ids":applied.iter().map(|(id,_)| id).collect::<Vec<_>>()}),
                    )
                }
                Err(e) => failed(e),
            }
        })
    }
}
