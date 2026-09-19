use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
};
use gateway::facade::{json_response, BoxFuture, FacadeHandler, FacadeRegistry, Response};
use http_body_util::BodyExt;
use kiro_wire::EventStreamDecoder;
use serde_json::Value;
use tower::ServiceExt;

/// Helper: send a request to router and collect status and JSON body.
async fn send_request(
    app: axum::Router,
    method: Method,
    uri: &str,
    body: Body,
) -> (StatusCode, String, Value) {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(body)
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    let status = response.status();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);

    (status, content_type, json)
}

#[tokio::test]
async fn test_facade_all_spec_4_2_endpoints_routable() {
    let app = FacadeRegistry::default().into_router();

    // 1. POST /oauth/token
    let (status, _, json) = send_request(
        app.clone(),
        Method::POST,
        "/oauth/token",
        Body::from(r#"{"grant_type": "card_key"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(json["accessToken"].as_str().is_some());
    assert!(json["refreshToken"].as_str().is_some());
    assert!(json["profileArn"].as_str().unwrap().starts_with("arn:aws:"));

    // 2. POST /refreshToken
    let (status, _, json) = send_request(
        app.clone(),
        Method::POST,
        "/refreshToken",
        Body::from(r#"{"refreshToken": "some-token"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(json["accessToken"].as_str().is_some());

    // 3. GET /ListAvailableModels
    let (status, _, json) = send_request(
        app.clone(),
        Method::GET,
        "/ListAvailableModels",
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let models = json["models"].as_array().expect("models array");
    assert!(!models.is_empty());
    assert_eq!(json["defaultModel"], "claude-sonnet-4.5");

    // 4. GET /getUsageLimits
    let (status, _, json) =
        send_request(app.clone(), Method::GET, "/getUsageLimits", Body::empty()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        json["subscriptionInfo"]["subscriptionTitle"],
        "Legacy service plan"
    );
    assert_eq!(json["subscriptionInfo"]["type"], "CUSTOM");
    assert_eq!(json["overageConfiguration"]["overageStatus"], "DISABLED");
    let breakdown = json["usageBreakdownList"].as_array().unwrap();
    assert!(!breakdown.is_empty());
    assert!(breakdown[0]["usageLimit"].as_f64().unwrap() > 0.0);

    // 5. POST /listAvailableSubscriptions
    let (status, _, json) = send_request(
        app.clone(),
        Method::POST,
        "/listAvailableSubscriptions",
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let plans = json["subscriptionPlans"].as_array().unwrap();
    assert_eq!(plans[0]["qSubscriptionType"], "CUSTOM");

    // 6. POST /CreateSubscriptionToken
    let (status, _, json) = send_request(
        app.clone(),
        Method::POST,
        "/CreateSubscriptionToken",
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(json["subscriptionToken"].as_str().is_some());

    // 7. POST /ListAvailableProfiles
    let (status, _, json) = send_request(
        app.clone(),
        Method::POST,
        "/ListAvailableProfiles",
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let profiles = json["profiles"].as_array().unwrap();
    assert!(!profiles.is_empty());
    assert!(profiles[0]["arn"].as_str().unwrap().starts_with("arn:aws:"));

    // 8. POST /GenerateCompletions (Graceful 429 rejection for autocomplete)
    let (status, _, json) = send_request(
        app.clone(),
        Method::POST,
        "/GenerateCompletions",
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(json["__type"], "ThrottlingException");
    assert_eq!(json["reason"], "MONTHLY_REQUEST_COUNT");
}

#[tokio::test]
async fn test_facade_conversation_streaming_eventstream_response() {
    let app = FacadeRegistry::default().into_router();

    let req = Request::builder()
        .method(Method::POST)
        .uri("/generateAssistantResponse")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"conversationState": {}}"#))
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get("content-type").unwrap(),
        "application/vnd.amazon.eventstream"
    );

    let bytes = response.into_body().collect().await.unwrap().to_bytes();

    // Verify binary decoding using kiro_wire decoder
    let mut decoder = EventStreamDecoder::new();
    decoder.feed(&bytes).unwrap();
    let frame = decoder
        .decode()
        .unwrap()
        .expect("Must decode a valid frame");

    assert_eq!(frame.event_type(), Some("assistantResponseEvent"));
    assert_eq!(frame.message_type(), Some("event"));
    let payload_str = frame.payload_to_string_lossy();
    assert!(payload_str.contains("Hello from Kiro BYOK Gateway Stub!"));
}

/// Verification of Spec §15.1: 1-line registration for new facade handlers.
#[tokio::test]
async fn test_spec_15_1_pluggable_facade_handler_registration() {
    // Custom dummy handler
    struct DummyCustomHandler;
    impl FacadeHandler for DummyCustomHandler {
        fn method(&self) -> Method {
            Method::GET
        }
        fn path(&self) -> &'static str {
            "/custom/plugin/ping"
        }
        fn handle<'a>(&'a self, _req: Request<Body>) -> BoxFuture<'a, Response> {
            Box::pin(async move {
                json_response(
                    StatusCode::OK,
                    &serde_json::json!({ "plugin": "ok", "version": 1 }),
                )
            })
        }
    }

    // 1-line registration
    let mut registry = FacadeRegistry::new();
    registry.register(DummyCustomHandler);

    let app = registry.into_router();
    let (status, _, json) =
        send_request(app, Method::GET, "/custom/plugin/ping", Body::empty()).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["plugin"], "ok");
    assert_eq!(json["version"], 1);
}

/// Audit B: Verify unregistered routes return structured AWS error instead of 500 or plain 404.
#[tokio::test]
async fn test_audit_b_unregistered_route_returns_structured_error_not_500() {
    let app = FacadeRegistry::default().into_router();

    // 1. Random GET path
    let (status, ct, json) = send_request(
        app.clone(),
        Method::GET,
        "/non_existent_path",
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(ct, "application/x-amz-json-1.1");
    assert_eq!(json["__type"], "ResourceNotFoundException");
    assert!(json["message"]
        .as_str()
        .unwrap()
        .contains("Cannot GET /non_existent_path"));

    // 2. Random POST path
    let (status, ct, json) = send_request(
        app.clone(),
        Method::POST,
        "/unmapped/action",
        Body::from(r#"{"foo": "bar"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(ct, "application/x-amz-json-1.1");
    assert_eq!(json["__type"], "ResourceNotFoundException");
    assert!(json["message"]
        .as_str()
        .unwrap()
        .contains("Cannot POST /unmapped/action"));
}

/// P4-1: Verify gateway negotiate endpoint returns patch recipe and UI overrides
#[tokio::test]
async fn test_p4_1_gateway_negotiate_returns_recipe_and_ui_overrides() {
    let app = FacadeRegistry::default().into_router();

    let (status, _, json) = send_request(
        app,
        Method::POST,
        "/client/negotiate",
        Body::from(r#"{"clientVersion": "0.1.0", "kiroVersion": "1.0.437", "os": "windows", "arch": "x86_64", "patchStatus": "official"}"#),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["supported"], true);
    assert!(json["patchRecipe"].is_object());
    assert_eq!(json["patchRecipe"]["recipeId"], "v1-standard");
    assert!(json["uiOverrides"].is_object());
    assert_eq!(json["uiOverrides"]["brandName"], "KIRO 极速接管中心");
    assert_eq!(json["uiOverrides"]["creditLabel"], "算力积分");
}
