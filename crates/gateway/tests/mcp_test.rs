//! Integration tests for Kiro `/mcp` web search endpoint (Spec §15.1, §18.7, P4-3).

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use gateway::facade::mcp::{McpHandler, SearchResponsePayload, SearchSourceConfig};
use gateway::facade::FacadeHandler;
use serde_json::json;
use wiremock::matchers::{method, path, query_param};
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

    let tools = json_resp["result"]["tools"]
        .as_array()
        .expect("tools array");
    assert!(tools.iter().any(|t| t["name"] == "web_search"));
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

#[tokio::test]
async fn test_mcp_web_search_failure_is_reported() {
    let handler = McpHandler::default().with_client(
        reqwest::Client::builder()
            .proxy(reqwest::Proxy::all("http://127.0.0.1:1").unwrap())
            .build()
            .unwrap(),
    );

    let call_req = Request::builder()
        .method(Method::POST)
        .uri("/mcp")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({
                "jsonrpc": "2.0",
                "id": "query-safe",
                "method": "tools/call",
                "params": {
                    "name": "web_search",
                    "arguments": {
                        "query": "kiro ide byok architecture"
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

    assert_eq!(json_resp["result"]["isError"], true);
    let text = json_resp["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("Search backend unavailable"));
    assert!(!text.contains("https://duckduckgo.com/?q="));
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
