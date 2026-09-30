//! Integration tests for Kiro `/mcp` web search endpoint (Spec §15.1, §18.7, P4-3).

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use gateway::facade::mcp::{McpHandler, SearchBackend, SearchResponsePayload, SearchSourceConfig};
use gateway::facade::FacadeHandler;
use serde_json::json;
use wiremock::matchers::{body_partial_json, header as header_is, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn test_mcp_initialize_and_tools_list() {
    let handler = McpHandler::default();

    // 1. initialize
    let init_req = Request::builder()
        .method(Method::POST)
        .uri("/mcp")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize"
            })
            .to_string(),
        ))
        .unwrap();

    let resp = handler.handle(init_req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .unwrap();
    let json_resp: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    assert_eq!(json_resp["id"], 1);
    assert_eq!(json_resp["result"]["protocolVersion"], "2024-11-05");
    assert_eq!(
        json_resp["result"]["serverInfo"]["name"],
        "kiro-byok-mcp-gateway"
    );

    // 2. tools/list
    let list_req = Request::builder()
        .method(Method::POST)
        .uri("/mcp")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({
                "jsonrpc": "2.0",
                "id": "list-1",
                "method": "tools/list"
            })
            .to_string(),
        ))
        .unwrap();

    let resp = handler.handle(list_req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .unwrap();
    let json_resp: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    // No backend: every search would fail, so Kiro is offered none, and the model does not
    // keep calling a tool that cannot work.
    let tools = json_resp["result"]["tools"]
        .as_array()
        .expect("tools array");
    assert!(tools.is_empty(), "{tools:?}");
}

/// The tools a handler lists.
async fn listed_tools(handler: &McpHandler) -> Vec<serde_json::Value> {
    let request = Request::builder()
        .method(Method::POST)
        .uri("/mcp")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({"jsonrpc": "2.0", "id": "list", "method": "tools/list"}).to_string(),
        ))
        .unwrap();
    let response = handler.handle(request).await;
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    body["result"]["tools"]
        .as_array()
        .expect("tools array")
        .clone()
}

#[tokio::test]
async fn web_search_is_listed_only_with_a_backend() {
    assert!(listed_tools(&McpHandler::default()).await.is_empty());
    for config in [
        // SearXNG, or a service like it: no key needed.
        SearchSourceConfig {
            custom_backend_url: Some("http://127.0.0.1:9/search".into()),
            ..Default::default()
        },
        SearchSourceConfig {
            backend: SearchBackend::Brave,
            api_key: Some("brave-key".into()),
            ..Default::default()
        },
    ] {
        let tools = listed_tools(&McpHandler::new(config)).await;
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "web_search");
        assert_eq!(tools[0]["inputSchema"]["required"], json!(["query"]));
    }
}

#[tokio::test]
async fn test_mcp_web_search_with_custom_backend() {
    let mock_server = MockServer::start().await;

    // Simulate custom search backend (e.g. SearXNG / internal search engine)
    let mock_search_response = json!({
        "results": [
            {
                "title": "Rust 2026 Edition Guide",
                "url": "https://doc.rust-lang.org/edition-guide/rust-2026/index.html",
                "content": "Rust 2026 edition introduces streamlined asynchronous iteration and enhanced type inference."
            },
            {
                "title": "What is new in Rust 2026",
                "url": "https://blog.rust-lang.org/2026/01/01/rust-2026.html",
                "content": "Official announcement of the Rust 2026 edition release and ecosystem migration roadmap."
            }
        ]
    });

    Mock::given(method("GET"))
        .and(path("/search"))
        .and(query_param("q", "rust 2026 edition"))
        .and(query_param("format", "json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_search_response))
        .mount(&mock_server)
        .await;

    let config = SearchSourceConfig {
        custom_backend_url: Some(format!("{}/search", mock_server.uri())),
        api_key: Some("secret-search-token".to_string()),
        max_results: 5,
        timeout_secs: 5,
        ..Default::default()
    };

    let handler = McpHandler::new(config);

    // Call web_search with standard prefix
    let call_req = Request::builder()
        .method(Method::POST)
        .uri("/mcp")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({
                "jsonrpc": "2.0",
                "id": "call-1",
                "method": "tools/call",
                "params": {
                    "name": "web_search",
                    "arguments": {
                        "query": "Perform a web search for the query: rust 2026 edition"
                    }
                }
            })
            .to_string(),
        ))
        .unwrap();

    let resp = handler.handle(call_req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .unwrap();
    let json_resp: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    assert_eq!(json_resp["id"], "call-1");
    assert_eq!(json_resp["result"]["isError"], false);

    let content_text = json_resp["result"]["content"][0]["text"].as_str().unwrap();
    let parsed_payload: SearchResponsePayload = serde_json::from_str(content_text).unwrap();

    assert_eq!(parsed_payload.query, "rust 2026 edition");
    assert_eq!(parsed_payload.total_results, 2);
    assert_eq!(parsed_payload.results[0].title, "Rust 2026 Edition Guide");
    assert_eq!(
        parsed_payload.results[0].url,
        "https://doc.rust-lang.org/edition-guide/rust-2026/index.html"
    );
    assert!(parsed_payload.results[0]
        .snippet
        .contains("streamlined asynchronous iteration"));
}

async fn call_search(handler: &McpHandler, query: &str) -> serde_json::Value {
    let request = Request::builder()
        .method(Method::POST)
        .uri("/mcp")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({
                "jsonrpc": "2.0",
                "id": "search-1",
                "method": "tools/call",
                "params": {"name": "web_search", "arguments": {"query": query}}
            })
            .to_string(),
        ))
        .unwrap();
    let response = handler.handle(request).await;
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

/// Kiro reads a tool result for its `results` alone: a failure sent as a result became
/// "Found 0 search result(s)", reported as a success. A JSON-RPC error is the tool's
/// failure, which the model can tell apart from nothing found.
#[tokio::test]
async fn a_search_that_cannot_run_is_the_tools_failure_not_zero_results() {
    // No backend configured.
    let reply = call_search(&McpHandler::default(), "kiro ide byok architecture").await;
    assert_eq!(reply["id"], "search-1");
    assert!(reply.get("result").is_none(), "{reply}");
    assert_eq!(reply["error"]["code"], -32000);
    assert!(reply["error"]["message"]
        .as_str()
        .unwrap()
        .contains("not configured"));

    // A backend that fails, named by its status only.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(503).set_body_string("upstream secret detail"))
        .mount(&server)
        .await;
    let handler = McpHandler::new(SearchSourceConfig {
        custom_backend_url: Some(format!("{}/search", server.uri())),
        api_key: Some("searx-token".into()),
        ..Default::default()
    });
    let reply = call_search(&handler, "rust").await;
    let message = reply["error"]["message"].as_str().unwrap();
    assert!(message.contains("HTTP 503"), "{reply}");
    assert!(!message.contains("secret") && !message.contains("searx-token"));
    assert!(!message.contains(&server.uri()));

    // An unreachable one.
    let handler = McpHandler::new(SearchSourceConfig {
        custom_backend_url: Some("http://127.0.0.1:1/search".into()),
        ..Default::default()
    });
    let reply = call_search(&handler, "rust").await;
    assert_eq!(reply["error"]["code"], -32000, "{reply}");
}

#[tokio::test]
async fn brave_results_reach_kiro_as_title_url_and_plain_snippet() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/res/v1/web/search"))
        .and(query_param("q", "tokio 1.40 release notes"))
        .and(query_param("count", "3"))
        .and(header_is("X-Subscription-Token", "brave-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "web": {"results": [
                {"title": "Tokio <strong>1.40</strong>", "url": "https://tokio.rs/blog/2024-09-tokio-1-40",
                 "description": "Release notes for <strong>Tokio</strong> 1.40 &amp; more"},
                {"title": "No URL", "description": "skipped"}
            ]}
        })))
        .mount(&server)
        .await;
    let handler = McpHandler::new(SearchSourceConfig {
        backend: SearchBackend::Brave,
        custom_backend_url: Some(format!("{}/res/v1/web/search", server.uri())),
        api_key: Some("brave-key".into()),
        max_results: 3,
        timeout_secs: 5,
    });
    let reply = call_search(
        &handler,
        "Perform a web search for the query: tokio 1.40 release notes",
    )
    .await;
    let payload: SearchResponsePayload =
        serde_json::from_str(reply["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(payload.query, "tokio 1.40 release notes");
    assert_eq!(payload.total_results, 1);
    assert_eq!(payload.results[0].title, "Tokio 1.40");
    assert_eq!(
        payload.results[0].url,
        "https://tokio.rs/blog/2024-09-tokio-1-40"
    );
    assert_eq!(
        payload.results[0].snippet,
        "Release notes for Tokio 1.40 & more"
    );
}

#[tokio::test]
async fn tavily_results_reach_kiro_as_title_url_and_snippet() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/search"))
        .and(header_is("Authorization", "Bearer tvly-key"))
        .and(body_partial_json(
            json!({"query": "rust 2026 edition", "max_results": 10}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "results": [{"title": "Rust 2026", "url": "https://blog.rust-lang.org/2026",
                         "content": "The 2026 edition.", "score": 0.9}]
        })))
        .mount(&server)
        .await;
    let handler = McpHandler::new(SearchSourceConfig {
        backend: SearchBackend::Tavily,
        custom_backend_url: Some(format!("{}/search", server.uri())),
        api_key: Some("tvly-key".into()),
        ..Default::default()
    });
    let reply = call_search(&handler, "rust 2026 edition").await;
    assert_eq!(reply["result"]["isError"], false, "{reply}");
    let payload: SearchResponsePayload =
        serde_json::from_str(reply["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(payload.results[0].snippet, "The 2026 edition.");
    assert_eq!(payload.results[0].url, "https://blog.rust-lang.org/2026");

    // A hosted API without its key is not configured.
    let handler = McpHandler::new(SearchSourceConfig {
        backend: SearchBackend::Tavily,
        ..Default::default()
    });
    let reply = call_search(&handler, "rust").await;
    assert!(reply["error"]["message"].as_str().unwrap().contains("key"));
}

#[tokio::test]
async fn test_mcp_invalid_tool_returns_jsonrpc_error() {
    let handler = McpHandler::default();

    let bad_tool_req = Request::builder()
        .method(Method::POST)
        .uri("/mcp")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({
                "jsonrpc": "2.0",
                "id": 99,
                "method": "tools/call",
                "params": {
                    "name": "unknown_tool",
                    "arguments": {}
                }
            })
            .to_string(),
        ))
        .unwrap();

    let resp = handler.handle(bad_tool_req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .unwrap();
    let json_resp: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    assert_eq!(json_resp["id"], 99);
    assert_eq!(json_resp["error"]["code"], -32601);
}

#[tokio::test]
async fn unavailable_default_search_never_fabricates_results() {
    let client = reqwest::Client::builder()
        .proxy(reqwest::Proxy::all("http://127.0.0.1:1").unwrap())
        .build()
        .unwrap();
    let result = gateway::facade::mcp::execute_search(
        &client,
        &SearchSourceConfig::default(),
        "acceptance network failure",
    )
    .await;
    assert!(
        result.is_err(),
        "Unreachable search backend must not yield synthetic evidence"
    );
}
