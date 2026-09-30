//! MCP (Model Context Protocol) endpoint handler for Kiro IDE (Spec §15.1, §18.7, P4-3).
//!
//! Handles JSON-RPC 2.0 MCP requests from Kiro IDE:
//! - `tools/list`: Lists available MCP tools: `web_search` when a search backend is configured.
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

/// Which search service answers Kiro's `web_search`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchBackend {
    /// None configured, unless a custom backend URL is: Kiro is offered no web search, and a
    /// search that still arrives (from a tool list it kept) fails as a tool error Kiro
    /// reports, never as an empty result.
    #[default]
    None,
    /// SearXNG, or any service answering `GET <url>?q=...&format=json` with SearXNG's
    /// `results` (`title`, `url`, `content`), with an optional Bearer key.
    Searxng,
    /// Brave Search API: `GET /res/v1/web/search`, key in `X-Subscription-Token`.
    Brave,
    /// Tavily: `POST /search`, Bearer key.
    Tavily,
}

/// Configurable search source settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchSourceConfig {
    #[serde(default)]
    pub backend: SearchBackend,
    /// The SearXNG endpoint; for Brave and Tavily, an optional replacement for their own.
    pub custom_backend_url: Option<String>,
    pub api_key: Option<String>,
    pub max_results: usize,
    pub timeout_secs: u64,
}

impl Default for SearchSourceConfig {
    fn default() -> Self {
        Self {
            backend: SearchBackend::None,
            custom_backend_url: None,
            api_key: None,
            max_results: 10,
            timeout_secs: 15,
        }
    }
}

impl SearchSourceConfig {
    /// The backend searches go to: the one named, or SearXNG when only its URL is given.
    pub fn effective_backend(&self) -> SearchBackend {
        match self.backend {
            SearchBackend::None if self.custom_backend_url.is_some() => SearchBackend::Searxng,
            backend => backend,
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

/// Run a search on the configured backend. An error is shown to the model and the
/// customer as the tool's failure, so it names what failed and never a key or a URL.
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
    let max_results = config.max_results.clamp(1, 20);
    let key = config.api_key.as_deref().filter(|key| !key.is_empty());
    let (request, pick): (_, fn(&Value) -> Vec<SearchResultItem>) = match config.effective_backend()
    {
        SearchBackend::None => {
            return Err("web search is not configured on this gateway".to_string())
        }
        SearchBackend::Searxng => {
            let url = config.custom_backend_url.as_deref().unwrap_or_default();
            let mut request = client
                .get(url)
                .query(&[("q", query_trimmed), ("format", "json")]);
            if let Some(key) = key {
                request = request.bearer_auth(key);
            }
            (request, searxng_results)
        }
        SearchBackend::Brave => {
            let Some(key) = key else {
                return Err("the Brave Search API key is not configured".to_string());
            };
            let url = config
                .custom_backend_url
                .as_deref()
                .unwrap_or("https://api.search.brave.com/res/v1/web/search");
            let request = client
                .get(url)
                .header("Accept", "application/json")
                .header("X-Subscription-Token", key)
                .query(&[("q", query_trimmed), ("count", &max_results.to_string())]);
            (request, brave_results)
        }
        SearchBackend::Tavily => {
            let Some(key) = key else {
                return Err("the Tavily API key is not configured".to_string());
            };
            let url = config
                .custom_backend_url
                .as_deref()
                .unwrap_or("https://api.tavily.com/search");
            let request = client.post(url).bearer_auth(key).json(&json!({
                "query": query_trimmed,
                "max_results": max_results,
                "search_depth": "basic",
            }));
            (request, tavily_results)
        }
    };

    let response = request
        .timeout(Duration::from_secs(config.timeout_secs))
        .send()
        .await
        .map_err(|error| {
            if error.is_timeout() {
                "the search backend did not answer in time".to_string()
            } else {
                "the search backend could not be reached".to_string()
            }
        })?;
    if !response.status().is_success() {
        return Err(format!(
            "the search backend answered HTTP {}",
            response.status().as_u16()
        ));
    }
    let body: Value = response
        .json()
        .await
        .map_err(|_| "the search backend returned an unreadable answer".to_string())?;
    let mut results = pick(&body);
    results.truncate(max_results);
    let total = results.len();
    Ok(SearchResponsePayload {
        query: query_trimmed.to_string(),
        results,
        total_results: total,
    })
}

fn searxng_results(body: &Value) -> Vec<SearchResultItem> {
    items(&body["results"], "content")
}

fn brave_results(body: &Value) -> Vec<SearchResultItem> {
    items(&body["web"]["results"], "description")
}

fn tavily_results(body: &Value) -> Vec<SearchResultItem> {
    items(&body["results"], "content")
}

/// The fields Kiro prints from each result: title, URL and snippet, as plain text.
fn items(results: &Value, snippet_field: &str) -> Vec<SearchResultItem> {
    results
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let url = item["url"].as_str().filter(|url| !url.is_empty())?;
            let snippet = item[snippet_field]
                .as_str()
                .or_else(|| item["snippet"].as_str())
                .unwrap_or_default();
            Some(SearchResultItem {
                title: plain_text(item["title"].as_str().unwrap_or("Untitled")),
                url: url.to_string(),
                snippet: plain_text(snippet),
                published_date: None,
            })
        })
        .collect()
}

/// Search snippets highlight matches with markup (`<strong>`) and escape entities.
fn plain_text(html: &str) -> String {
    let mut text = String::with_capacity(html.len());
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => text.push(c),
            _ => {}
        }
    }
    text.replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
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
                    // Without a backend every search fails, and a model offered the tool
                    // keeps calling it. Kiro offers the tools listed here as its
                    // remote_web_search; given none, it keeps the empty list and offers no
                    // web search at all.
                    let tools = if self.config.effective_backend() == SearchBackend::None {
                        json!([])
                    } else {
                        json!([
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
                        ])
                    };
                    let resp = json!({
                        "jsonrpc": "2.0",
                        "id": req_id,
                        "result": { "tools": tools }
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
                        // Kiro reads a result for its `results` alone, so a failure sent as
                        // one became "Found 0 search result(s)", reported as a success. As a
                        // JSON-RPC error it is the tool's failure, which the model can tell
                        // apart from nothing found.
                        Err(e) => {
                            let resp = json!({
                                "jsonrpc": "2.0",
                                "id": req_id,
                                "error": {
                                    "code": -32000,
                                    "message": format!("Web search failed: {e}")
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
