//! Tab autocomplete handler and evaluation (Spec §2.5, §16-10, P0-7, P4-11).
//!
//! ### Evaluation Summary (P4-11):
//! 1. **Latency Budget**: Tab autocomplete (FIM) requires <200ms TTFT to be useful.
//!    Standard commercial LLMs (e.g. Claude Sonnet, GPT-4o) introduce 500-1500ms latency,
//!    causing keystroke lag or stale suggestions.
//! 2. **Token & Rate Limit Consumption**: Autocomplete triggers on typing pauses (20-50 req/min).
//!    Routing completions to billable frontier models exhausts user credits and provider rate limits.
//! 3. **Architectural Modes**:
//!    - `Throttled` (Default): Returns HTTP 429 with `reason: "MONTHLY_REQUEST_COUNT"`.
//!      Kiro's native runtime cleanly captures this and displays a non-disruptive message without retrying.
//!    - `Empty`: Returns HTTP 200 `{"completions": []}` for completely silent no-op.
//!    - `Forward`: Forwards FIM context to a dedicated fast, cheap completion model or local endpoint.

use super::{BoxFuture, FacadeHandler, Response};
use axum::{
    body::Body,
    http::{header, Method, Request, StatusCode},
    response::IntoResponse,
};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Operating mode for `GenerateCompletions` endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutocompleteMode {
    /// Returns HTTP 429 ThrottlingException with MONTHLY_REQUEST_COUNT (Spec §2.5, P0-7 default).
    #[default]
    Throttled,
    /// Returns HTTP 200 with `{"completions": []}` for a silent no-op.
    Empty,
    /// Forwards completion request to a dedicated fast FIM upstream endpoint.
    Forward,
}

/// Configuration for autocomplete forwarding.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CompletionConfig {
    pub mode: AutocompleteMode,
    pub provider_url: Option<String>,
    pub api_key: Option<String>,
    pub model: Option<String>,
    pub max_tokens: Option<u32>,
    pub timeout_ms: Option<u64>,
}

/// Handler for `POST /GenerateCompletions`
#[derive(Clone)]
pub struct GenerateCompletionsHandler {
    pub client: reqwest::Client,
    pub config: CompletionConfig,
}

impl Default for GenerateCompletionsHandler {
    fn default() -> Self {
        Self {
            client: reqwest::Client::new(),
            config: CompletionConfig::default(),
        }
    }
}

impl GenerateCompletionsHandler {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_config(mut self, config: CompletionConfig) -> Self {
        self.config = config;
        self
    }

    pub fn with_mode(mut self, mode: AutocompleteMode) -> Self {
        self.config.mode = mode;
        self
    }
}

impl FacadeHandler for GenerateCompletionsHandler {
    fn method(&self) -> Method {
        Method::POST
    }

    fn path(&self) -> &'static str {
        "/GenerateCompletions"
    }

    fn handle<'a>(&'a self, req: Request<Body>) -> BoxFuture<'a, Response> {
        Box::pin(async move {
            match self.config.mode {
                AutocompleteMode::Throttled => {
                    let body = serde_json::json!({
                        "__type": "ThrottlingException",
                        "message": "Maximum Kiro usage reached for this month.",
                        "reason": "MONTHLY_REQUEST_COUNT"
                    });

                    (
                        StatusCode::TOO_MANY_REQUESTS,
                        [
                            (header::CONTENT_TYPE, "application/x-amz-json-1.1"),
                            (
                                axum::http::HeaderName::from_static("x-amzn-errortype"),
                                "ThrottlingException",
                            ),
                        ],
                        axum::Json(body),
                    )
                        .into_response()
                }
                AutocompleteMode::Empty => {
                    let body = serde_json::json!({
                        "completions": []
                    });

                    (
                        StatusCode::OK,
                        [(header::CONTENT_TYPE, "application/x-amz-json-1.1")],
                        axum::Json(body),
                    )
                        .into_response()
                }
                AutocompleteMode::Forward => {
                    let (url, api_key, model) = match (
                        &self.config.provider_url,
                        &self.config.api_key,
                        &self.config.model,
                    ) {
                        (Some(u), Some(k), Some(m)) => (u, k, m),
                        _ => {
                            // Fallback to empty completions if config is incomplete
                            let body = serde_json::json!({ "completions": [] });
                            return (
                                StatusCode::OK,
                                [(header::CONTENT_TYPE, "application/x-amz-json-1.1")],
                                axum::Json(body),
                            )
                                .into_response();
                        }
                    };

                    let body_bytes = match axum::body::to_bytes(req.into_body(), 1024 * 1024).await
                    {
                        Ok(b) => b,
                        Err(_) => {
                            let body = serde_json::json!({ "completions": [] });
                            return (
                                StatusCode::OK,
                                [(header::CONTENT_TYPE, "application/x-amz-json-1.1")],
                                axum::Json(body),
                            )
                                .into_response();
                        }
                    };

                    let req_val: serde_json::Value =
                        serde_json::from_slice(&body_bytes).unwrap_or_default();
                    let left = req_val["fileContext"]["leftFileContent"]
                        .as_str()
                        .unwrap_or("");
                    let right = req_val["fileContext"]["rightFileContent"]
                        .as_str()
                        .unwrap_or("");
                    let filename = req_val["fileContext"]["filename"]
                        .as_str()
                        .unwrap_or("file");

                    let max_tokens = self.config.max_tokens.unwrap_or(64);
                    let timeout = Duration::from_millis(self.config.timeout_ms.unwrap_or(2000));

                    let chat_payload = serde_json::json!({
                        "model": model,
                        "messages": [
                            {
                                "role": "system",
                                "content": format!("You are an inline code completion assistant for file '{}'. Return ONLY the code to fill in the middle between <PREFIX> and <SUFFIX>. Do not wrap in markdown quotes or repeat the prefix/suffix.", filename)
                            },
                            {
                                "role": "user",
                                "content": format!("<PREFIX>\n{}\n</PREFIX>\n<SUFFIX>\n{}\n</SUFFIX>", left, right)
                            }
                        ],
                        "max_tokens": max_tokens,
                        "temperature": 0.0,
                        "stream": false
                    });

                    let ep = if url.ends_with("/chat/completions") {
                        url.clone()
                    } else {
                        format!("{}/chat/completions", url.trim_end_matches('/'))
                    };

                    let upstream_res = self
                        .client
                        .post(&ep)
                        .bearer_auth(api_key)
                        .json(&chat_payload)
                        .timeout(timeout)
                        .send()
                        .await;

                    let mut completions = Vec::new();
                    if let Ok(resp) = upstream_res {
                        if resp.status().is_success() {
                            if let Ok(res_json) = resp.json::<serde_json::Value>().await {
                                if let Some(content) =
                                    res_json["choices"][0]["message"]["content"].as_str()
                                {
                                    if !content.trim().is_empty() {
                                        completions.push(serde_json::json!({
                                            "content": content,
                                            "score": 1.0
                                        }));
                                    }
                                }
                            }
                        }
                    }

                    (
                        StatusCode::OK,
                        [(header::CONTENT_TYPE, "application/x-amz-json-1.1")],
                        axum::Json(serde_json::json!({ "completions": completions })),
                    )
                        .into_response()
                }
            }
        })
    }
}
