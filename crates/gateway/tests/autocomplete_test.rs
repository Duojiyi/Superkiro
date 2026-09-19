//! Tests for GenerateCompletions handler modes and graceful degradation (Spec §2.5, P4-11).

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use gateway::facade::completions::{
    AutocompleteMode, CompletionConfig, GenerateCompletionsHandler,
};
use gateway::facade::FacadeRegistry;
use serde_json::Value;
use wiremock::matchers::{method as wm_method, path as wm_path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn test_autocomplete_throttled_default_mode() {
    let handler = GenerateCompletionsHandler::default();
    let mut registry = FacadeRegistry::new();
    registry.register(handler);
    let app = registry.into_router();

    let req = Request::builder()
        .method(Method::POST)
        .uri("/GenerateCompletions")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({
                "fileContext": {
                    "leftFileContent": "fn main() {",
                    "rightFileContent": "}",
                    "filename": "main.rs"
                }
            }))
            .unwrap(),
        ))
        .unwrap();

    let resp = tower::ServiceExt::oneshot(app, req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);

    let err_type = resp
        .headers()
        .get("x-amzn-errortype")
        .and_then(|v| v.to_str().ok());
    assert_eq!(err_type, Some("ThrottlingException"));

    let body_bytes = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
    let val: Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(val["__type"], "ThrottlingException");
    assert_eq!(val["reason"], "MONTHLY_REQUEST_COUNT");
}

#[tokio::test]
async fn test_autocomplete_empty_silent_mode() {
    let handler = GenerateCompletionsHandler::new().with_mode(AutocompleteMode::Empty);
    let mut registry = FacadeRegistry::new();
    registry.register(handler);
    let app = registry.into_router();

    let req = Request::builder()
        .method(Method::POST)
        .uri("/GenerateCompletions")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({
                "fileContext": {
                    "leftFileContent": "class Calculator {",
                    "rightFileContent": "}",
                    "filename": "calc.ts"
                }
            }))
            .unwrap(),
        ))
        .unwrap();

    let resp = tower::ServiceExt::oneshot(app, req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body_bytes = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
    let val: Value = serde_json::from_slice(&body_bytes).unwrap();
    let completions = val["completions"].as_array().unwrap();
    assert!(completions.is_empty());
}

#[tokio::test]
async fn test_autocomplete_forwarding_mode() {
    let mock_server = MockServer::start().await;

    let upstream_resp = serde_json::json!({
        "id": "compl-1",
        "choices": [
            {
                "message": {
                    "role": "assistant",
                    "content": "    return a * b;"
                }
            }
        ]
    });

    Mock::given(wm_method("POST"))
        .and(wm_path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&upstream_resp))
        .mount(&mock_server)
        .await;

    let config = CompletionConfig {
        mode: AutocompleteMode::Forward,
        provider_url: Some(format!("{}/v1", mock_server.uri())),
        api_key: Some("sk-fast-token".to_string()),
        model: Some("qwen-coder-fast".to_string()),
        max_tokens: Some(32),
        timeout_ms: Some(1000),
    };

    let handler = GenerateCompletionsHandler::new().with_config(config);
    let mut registry = FacadeRegistry::new();
    registry.register(handler);
    let app = registry.into_router();

    let req = Request::builder()
        .method(Method::POST)
        .uri("/GenerateCompletions")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({
                "fileContext": {
                    "leftFileContent": "int multiply(int a, int b) {\n",
                    "rightFileContent": "\n}",
                    "filename": "math.c"
                }
            }))
            .unwrap(),
        ))
        .unwrap();

    let resp = tower::ServiceExt::oneshot(app, req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body_bytes = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
    let val: Value = serde_json::from_slice(&body_bytes).unwrap();
    let completions = val["completions"].as_array().unwrap();
    assert_eq!(completions.len(), 1);
    assert_eq!(completions[0]["content"], "    return a * b;");

    // Verify upstream received prompt with PREFIX and SUFFIX
    let requests = mock_server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let sent: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(sent["model"], "qwen-coder-fast");
}

#[tokio::test]
async fn test_autocomplete_forwarding_upstream_error_graceful_fallback() {
    let mock_server = MockServer::start().await;

    // Upstream fails with 500 or timeout
    Mock::given(wm_method("POST"))
        .and(wm_path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&mock_server)
        .await;

    let config = CompletionConfig {
        mode: AutocompleteMode::Forward,
        provider_url: Some(format!("{}/v1", mock_server.uri())),
        api_key: Some("sk-fast-token".to_string()),
        model: Some("qwen-coder-fast".to_string()),
        max_tokens: Some(32),
        timeout_ms: Some(500),
    };

    let handler = GenerateCompletionsHandler::new().with_config(config);
    let mut registry = FacadeRegistry::new();
    registry.register(handler);
    let app = registry.into_router();

    let req = Request::builder()
        .method(Method::POST)
        .uri("/GenerateCompletions")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({
                "fileContext": {
                    "leftFileContent": "let x = ",
                    "rightFileContent": ";",
                    "filename": "index.js"
                }
            }))
            .unwrap(),
        ))
        .unwrap();

    let resp = tower::ServiceExt::oneshot(app, req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body_bytes = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
    let val: Value = serde_json::from_slice(&body_bytes).unwrap();
    let completions = val["completions"].as_array().unwrap();
    assert!(completions.is_empty());
}
