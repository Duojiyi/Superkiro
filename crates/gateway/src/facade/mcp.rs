//! MCP (Model Context Protocol) endpoint handler for Kiro IDE (Spec §15.1, §18.7, P4-3).
//!
//! Handles JSON-RPC 2.0 MCP requests from Kiro IDE:
//! - `tools/list`: Lists available MCP tools (including `web_search`).
//! - `tools/call`: Executes tool calls, routing `web_search` to the configured search source.
//! - `initialize`: Responds to MCP handshake and protocol negotiation.

use super::{BoxFuture, FacadeHandler, Response};
use axum::{
    body::Body,
    http::{header, Method, Request, StatusCode},
    response::IntoResponse,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::time::Duration;

/// Individual web search result item matching Kiro expectation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SearchResultItem {
    pub title: String,
    pub url: String,
    pub snippet: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub published_date: Option<u64>,
}

/// Structured search payload serialized into `result.content[0].text`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SearchResponsePayload {
    pub query: String,
    pub results: Vec<SearchResultItem>,
    pub total_results: usize,
}

/// Configurable search source settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchSourceConfig {
    pub custom_backend_url: Option<String>,
    pub api_key: Option<String>,
    pub max_results: usize,
    pub timeout_secs: u64,
}

impl Default for SearchSourceConfig {
    fn default() -> Self {
        Self {
            custom_backend_url: None,
            api_key: None,
            max_results: 10,
            timeout_secs: 15,
        }
    }
}

/// JSON-RPC 2.0 Request representation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    #[serde(default = "default_jsonrpc")]
    pub jsonrpc: String,
    #[serde(default)]
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Option<Value>,
}

fn default_jsonrpc() -> String {
    "2.0".to_string()
}

/// MCP `tools/call` parameters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolCallParams {
    pub name: String,
    #[serde(default)]
    pub arguments: HashMap<String, Value>,
}

/// Search engine runner querying either a custom search backend or DuckDuckGo.
pub async fn execute_search(
    client: &reqwest::Client,
    config: &SearchSourceConfig,
    query: &str,
) -> Result<SearchResponsePayload, String> {
    let query_trimmed = query.trim();
    if query_trimmed.is_empty() {
        return Ok(SearchResponsePayload {
            query: String::new(),
            results: Vec::new(),
            total_results: 0,
        });
    }

    if let Some(ref backend_url) = config.custom_backend_url {
        // Custom search source backend (e.g. SearXNG, custom search API)
        let mut req = client
            .get(backend_url)
            .query(&[("q", query_trimmed), ("format", "json")]);
        if let Some(ref key) = config.api_key {
            req = req.header("Authorization", format!("Bearer {}", key));
        }

        let resp = req
            .timeout(Duration::from_secs(config.timeout_secs))
            .send()
            .await
            .map_err(|e| format!("Custom search backend request failed: {e}"))?;

        if !resp.status().is_success() {
            return Err(format!(
                "Custom search backend returned status {}",
                resp.status()
            ));
        }

        let json_body: Value = resp
            .json()
            .await
            .map_err(|e| format!("Failed to parse custom search backend response: {e}"))?;

        // Standard SearXNG / generic array mapping
        let mut results = Vec::new();
        if let Some(items) = json_body["results"].as_array() {
            for item in items.iter().take(config.max_results) {
                let title = item["title"].as_str().unwrap_or("Untitled").to_string();
                let url = item["url"].as_str().unwrap_or("").to_string();
                let snippet = item["content"]
                    .as_str()
                    .or_else(|| item["snippet"].as_str())
                    .unwrap_or("")
                    .to_string();
                results.push(SearchResultItem {
                    title,
                    url,
                    snippet,
                    published_date: None,
                });
            }
        }

        let total = results.len();
        return Ok(SearchResponsePayload {
            query: query_trimmed.to_string(),
            results,
            total_results: total,
        });
    }

    // Default built-in source: DuckDuckGo Instant Answer API
    let ddg_url = "https://api.duckduckgo.com/";
    let resp = client
        .get(ddg_url)
        .query(&[
            ("q", query_trimmed),
            ("format", "json"),
            ("no_html", "1"),
            ("skip_disambig", "1"),
        ])
        .timeout(Duration::from_secs(config.timeout_secs))
        .send()
        .await;

    match resp {
        Ok(r) if r.status().is_success() => {
            if let Ok(ddg) = r.json::<Value>().await {
                let mut results = Vec::new();

                // 1. Primary abstract if available
                if let Some(heading) = ddg["Heading"].as_str() {
                    let abstract_text = ddg["AbstractText"].as_str().unwrap_or("");
                    let abstract_url = ddg["AbstractURL"].as_str().unwrap_or("");
                    if !heading.is_empty() && !abstract_text.is_empty() {
                        results.push(SearchResultItem {
                            title: heading.to_string(),
                            url: abstract_url.to_string(),
                            snippet: abstract_text.to_string(),
                            published_date: None,
                        });
                    }
                }

                // 2. Related topics
                if let Some(topics) = ddg["RelatedTopics"].as_array() {
                    for topic in topics
                        .iter()
                        .take(config.max_results.saturating_sub(results.len()))
                    {
                        if let (Some(text), Some(url)) =
                            (topic["Text"].as_str(), topic["FirstURL"].as_str())
                        {
                            let title = text.split(" - ").next().unwrap_or("Result").to_string();
                            results.push(SearchResultItem {
                                title,
                                url: url.to_string(),
                                snippet: text.to_string(),
                                published_date: None,
                            });
                        }
                    }
                }

                let total = results.len();
                return Ok(SearchResponsePayload {
                    query: query_trimmed.to_string(),
                    results,
                    total_results: total,
                });
            }
        }
        _ => {}
    }

    // Never manufacture evidence when an external search request fails.
    Err("Search backend unavailable or returned an invalid response".to_string())
}

/// Facade handler for `/mcp`.
pub struct McpHandler {
    pub client: reqwest::Client,
    pub config: SearchSourceConfig,
}

impl Default for McpHandler {
    fn default() -> Self {
        Self {
            client: reqwest::Client::new(),
            config: SearchSourceConfig::default(),
        }
    }
}

impl McpHandler {
    pub fn new(config: SearchSourceConfig) -> Self {
        Self {
            client: reqwest::Client::new(),
            config,
        }
    }

    pub fn with_client(mut self, client: reqwest::Client) -> Self {
        self.client = client;
        self
    }
}

impl FacadeHandler for McpHandler {
    fn method(&self) -> Method {
        Method::POST
    }

    fn path(&self) -> &'static str {
        "/mcp"
    }

    fn handle<'a>(&'a self, req: Request<Body>) -> BoxFuture<'a, Response> {
        Box::pin(async move {
            let body_bytes = match axum::body::to_bytes(req.into_body(), 1024 * 1024).await {
                Ok(b) => b,
                Err(e) => {
                    return (
                        StatusCode::BAD_REQUEST,
                        [(header::CONTENT_TYPE, "application/json")],
                        json!({
                            "jsonrpc": "2.0",
                            "id": null,
                            "error": { "code": -32700, "message": format!("Parse error: {e}") }
                        })
                        .to_string(),
                    )
                        .into_response();
                }
            };

            let rpc_req: JsonRpcRequest = match serde_json::from_slice(&body_bytes) {
                Ok(r) => r,
                Err(e) => {
                    return (
                        StatusCode::BAD_REQUEST,
                        [(header::CONTENT_TYPE, "application/json")],
                        json!({
                            "jsonrpc": "2.0",
                            "id": null,
                            "error": { "code": -32700, "message": format!("Invalid JSON-RPC: {e}") }
                        })
                        .to_string(),
                    )
                        .into_response();
                }
            };

            let req_id = rpc_req.id.clone().unwrap_or(Value::Null);

            match rpc_req.method.as_str() {
                "initialize" => {
                    let resp = json!({
                        "jsonrpc": "2.0",
                        "id": req_id,
                        "result": {
                            "protocolVersion": "2024-11-05",
                            "capabilities": {
                                "tools": { "listChanged": false }
                            },
                            "serverInfo": {
                                "name": "kiro-byok-mcp-gateway",
                                "version": "0.1.0"
                            }
                        }
                    });
                    (
                        StatusCode::OK,
                        [(header::CONTENT_TYPE, "application/json")],
                        axum::Json(resp),
                    )
                        .into_response()
                }
                "tools/list" => {
                    let resp = json!({
                        "jsonrpc": "2.0",
                        "id": req_id,
                        "result": {
                            "tools": [
                                {
                                    "name": "web_search",
                                    "description": "Performs web search for up-to-date information, documentation, and news.",
                                    "inputSchema": {
                                        "type": "object",
                                        "properties": {
                                            "query": {
                                                "type": "string",
                                                "description": "The search query keywords"
                                            }
                                        },
                                        "required": ["query"]
                                    }
                                }
                            ]
                        }
                    });
                    (
                        StatusCode::OK,
                        [(header::CONTENT_TYPE, "application/json")],
                        axum::Json(resp),
                    )
                        .into_response()
                }
                "tools/call" => {
                    let params = match rpc_req.params {
                        Some(p) => match serde_json::from_value::<McpToolCallParams>(p) {
                            Ok(parsed) => parsed,
                            Err(e) => {
                                return (
                                    StatusCode::OK,
                                    [(header::CONTENT_TYPE, "application/json")],
                                    json!({
                                        "jsonrpc": "2.0",
                                        "id": req_id,
                                        "error": { "code": -32602, "message": format!("Invalid params: {e}") }
                                    })
                                    .to_string(),
                                )
                                    .into_response();
                            }
                        },
                        None => {
                            return (
                                StatusCode::OK,
                                [(header::CONTENT_TYPE, "application/json")],
                                json!({
                                    "jsonrpc": "2.0",
                                    "id": req_id,
                                    "error": { "code": -32602, "message": "Missing params for tools/call" }
                                })
                                .to_string(),
                            )
                                .into_response();
                        }
                    };

                    if params.name != "web_search" {
                        return (
                            StatusCode::OK,
                            [(header::CONTENT_TYPE, "application/json")],
                            json!({
                                "jsonrpc": "2.0",
                                "id": req_id,
                                "error": { "code": -32601, "message": format!("Method/Tool not found: {}", params.name) }
                            })
                            .to_string(),
                        )
                            .into_response();
                    }

                    // Extract query
                    let raw_query = params
                        .arguments
                        .get("query")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default();

                    // Strip common prefix if present
                    let prefix = "Perform a web search for the query: ";
                    let clean_query = raw_query.strip_prefix(prefix).unwrap_or(raw_query);

                    match execute_search(&self.client, &self.config, clean_query).await {
                        Ok(payload) => {
                            let text_content = match serde_json::to_string(&payload) {
                                Ok(s) => s,
                                Err(e) => {
                                    format!("{{\"error\": \"Failed to serialize results: {e}\"}}")
                                }
                            };

                            let resp = json!({
                                "jsonrpc": "2.0",
                                "id": req_id,
                                "result": {
                                    "content": [
                                        {
                                            "type": "text",
                                            "text": text_content
                                        }
                                    ],
                                    "isError": false
                                }
                            });

                            (
                                StatusCode::OK,
                                [(header::CONTENT_TYPE, "application/json")],
                                axum::Json(resp),
                            )
                                .into_response()
                        }
                        Err(e) => {
                            let resp = json!({
                                "jsonrpc": "2.0",
                                "id": req_id,
                                "result": {
                                    "content": [
                                        {
                                            "type": "text",
                                            "text": format!("{{\"error\": \"Web search failed: {e}\"}}")
                                        }
                                    ],
                                    "isError": true
                                }
                            });

                            (
                                StatusCode::OK,
                                [(header::CONTENT_TYPE, "application/json")],
                                axum::Json(resp),
                            )
                                .into_response()
                        }
                    }
                }
                _ => {
                    let resp = json!({
                        "jsonrpc": "2.0",
                        "id": req_id,
                        "error": {
                            "code": -32601,
                            "message": format!("Method not found: {}", rpc_req.method)
                        }
                    });
                    (
                        StatusCode::OK,
                        [(header::CONTENT_TYPE, "application/json")],
                        axum::Json(resp),
                    )
                        .into_response()
                }
            }
        })
    }
}
