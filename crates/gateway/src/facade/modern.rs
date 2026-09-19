//! Kiro 1.1 control-plane REST aliases and AWS JSON runtime dispatch.
//! These facades stay inside the normal authenticated router and reuse its handlers.
use super::{error_response, json_response, BoxFuture, FacadeHandler, Response};
use axum::{
    body::{to_bytes, Body},
    http::{Method, Request, StatusCode},
};
use serde_json::{json, Value};
use std::{collections::HashMap, sync::Arc};

struct Alias {
    path: &'static str,
    inner: Arc<dyn FacadeHandler>,
    models: bool,
}

async fn invoke(inner: &dyn FacadeHandler, req: Request<Body>, models: bool) -> Response {
    let response = inner.handle(req).await;
    if !models || !response.status().is_success() {
        return response;
    }
    let (mut parts, body) = response.into_parts();
    let bytes = match to_bytes(body, 1024 * 1024).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "InternalServerException",
                "Invalid model response",
            )
        }
    };
    let mut value: Value = match serde_json::from_slice(&bytes) {
        Ok(value) => value,
        Err(_) => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "InternalServerException",
                "Invalid model response",
            )
        }
    };
    if let Some(id) = value["defaultModel"].as_str().map(str::to_owned) {
        value["defaultModel"] = value["models"]
            .as_array()
            .and_then(|items| items.iter().find(|m| m["modelId"] == id))
            .cloned()
            .unwrap_or_else(|| json!({"modelId": id}));
    }
    parts.headers.remove("content-length");
    Response::from_parts(parts, Body::from(value.to_string()))
}

impl FacadeHandler for Alias {
    fn path(&self) -> &'static str {
        self.path
    }
    fn method(&self) -> Method {
        self.inner.method()
    }
    fn handle<'a>(&'a self, req: Request<Body>) -> BoxFuture<'a, Response> {
        Box::pin(invoke(self.inner.as_ref(), req, self.models))
    }
}

struct RuntimeRpc {
    handlers: HashMap<&'static str, (Arc<dyn FacadeHandler>, bool)>,
}
impl FacadeHandler for RuntimeRpc {
    fn path(&self) -> &'static str {
        "/"
    }
    fn method(&self) -> Method {
        Method::POST
    }
    fn handle<'a>(&'a self, req: Request<Body>) -> BoxFuture<'a, Response> {
        Box::pin(async move {
            let target = req
                .headers()
                .get("x-amz-target")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            if target == "KiroRuntimeService.GetFeatureConfiguration" {
                return json_response(StatusCode::OK, &json!({"configuration": {}}));
            }
            match self.handlers.get(target) {
                Some((handler, models)) => invoke(handler.as_ref(), req, *models).await,
                None => error_response(
                    StatusCode::NOT_FOUND,
                    "UnknownOperationException",
                    "Unsupported Kiro operation",
                ),
            }
        })
    }
}

pub(super) fn register(handlers: &mut Vec<Arc<dyn FacadeHandler>>) {
    let mut rpc = RuntimeRpc {
        handlers: HashMap::new(),
    };
    for (legacy, alias, target, models) in [
        (
            "/ListAvailableModels",
            Some("/List-Available-Models"),
            "KiroControlPlaneBearerService.ListAvailableModels",
            true,
        ),
        (
            "/getUsageLimits",
            None,
            "KiroControlPlaneBearerService.GetUsageLimits",
            false,
        ),
        (
            "/generateAssistantResponse",
            None,
            "KiroRuntimeService.GenerateAssistantResponse",
            false,
        ),
        ("/mcp", None, "KiroRuntimeService.InvokeMCP", false),
    ] {
        if let Some(inner) = handlers.iter().rev().find(|h| h.path() == legacy).cloned() {
            rpc.handlers.insert(target, (inner.clone(), models));
            if let Some(path) = alias {
                handlers.push(Arc::new(Alias {
                    path,
                    inner,
                    models,
                }));
            }
        }
    }
    handlers.push(Arc::new(rpc));
}
