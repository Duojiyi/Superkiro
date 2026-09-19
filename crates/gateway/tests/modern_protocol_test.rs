use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use gateway::{
    auth::AuthState,
    facade::{json_response, BoxFuture, FacadeHandler, FacadeRegistry, Response},
};
use serde_json::{json, Value};
use tower::ServiceExt;

async fn call(app: axum::Router, path: &str, target: Option<&str>, body: &str) -> Response {
    let mut req =
        Request::builder()
            .uri(path)
            .method(if target.is_some() { "POST" } else { "GET" });
    if let Some(target) = target {
        req = req.header("x-amz-target", target);
    }
    app.oneshot(req.body(Body::from(body.to_owned())).unwrap())
        .await
        .unwrap()
}
async fn value(response: Response) -> Value {
    assert_eq!(response.status(), StatusCode::OK);
    serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap()).unwrap()
}
#[tokio::test]
async fn modern_models_preserve_legacy_schema() {
    let app = FacadeRegistry::default().into_router();
    let old = value(call(app.clone(), "/ListAvailableModels", None, "").await).await;
    assert!(old["defaultModel"].is_string());
    for response in [
        call(
            app.clone(),
            "/List-Available-Models?origin=AI_EDITOR",
            None,
            "",
        )
        .await,
        call(
            app,
            "/",
            Some("KiroControlPlaneBearerService.ListAvailableModels"),
            "{}",
        )
        .await,
    ] {
        let new = value(response).await;
        assert_eq!(new["models"], old["models"]);
        assert_eq!(new["defaultModel"]["modelId"], old["defaultModel"]);
    }
}
#[tokio::test]
async fn modern_routes_require_auth_and_reject_unknown_targets() {
    let app = FacadeRegistry::default()
        .into_router_with_auth(AuthState::new("modern-test-secret-32-characters-long"));
    for (path, target) in [
        ("/List-Available-Models", None),
        ("/", Some("KiroRuntimeService.GenerateAssistantResponse")),
        ("/", Some("KiroRuntimeService.GetFeatureConfiguration")),
    ] {
        assert_eq!(
            call(app.clone(), path, target, "{}").await.status(),
            StatusCode::UNAUTHORIZED
        );
    }
    let app = FacadeRegistry::default().into_router();
    for target in [
        "OtherService.GenerateAssistantResponse",
        "KiroRuntimeService.DeleteAccount",
        "",
    ] {
        assert_eq!(
            call(app.clone(), "/", Some(target), "{}").await.status(),
            StatusCode::NOT_FOUND
        );
    }
}
#[tokio::test]
async fn rpc_reuses_latest_configured_handler_and_preserves_request() {
    struct Probe;
    impl FacadeHandler for Probe {
        fn path(&self) -> &'static str {
            "/generateAssistantResponse"
        }
        fn method(&self) -> axum::http::Method {
            axum::http::Method::POST
        }
        fn handle<'a>(&'a self, req: Request<Body>) -> BoxFuture<'a, Response> {
            Box::pin(async move {
                let bytes = to_bytes(req.into_body(), 1024).await.unwrap();
                let body: Value = serde_json::from_slice(&bytes).unwrap();
                json_response(StatusCode::OK, &body)
            })
        }
    }
    let mut registry = FacadeRegistry::default();
    registry.register(Probe);
    let expected = json!({"conversationState":{},"systemPrompt":"test","additionalModelRequestFields":{"effort":"low"}});
    let actual = value(
        call(
            registry.into_router(),
            "/",
            Some("KiroRuntimeService.GenerateAssistantResponse"),
            &expected.to_string(),
        )
        .await,
    )
    .await;
    assert_eq!(actual, expected);
}
#[tokio::test]
async fn runtime_discovery_and_stream_are_compatible() {
    let app = FacadeRegistry::default().into_router();
    let config = value(
        call(
            app.clone(),
            "/",
            Some("KiroRuntimeService.GetFeatureConfiguration"),
            "{}",
        )
        .await,
    )
    .await;
    assert_eq!(config, json!({"configuration":{}}));
    let tools = value(
        call(
            app.clone(),
            "/",
            Some("KiroRuntimeService.InvokeMCP"),
            r#"{"jsonrpc":"2.0","id":"1","method":"tools/list","params":{}}"#,
        )
        .await,
    )
    .await;
    assert!(tools["result"]["tools"].is_array());
    let response = call(
        app,
        "/",
        Some("KiroRuntimeService.GenerateAssistantResponse"),
        r#"{"conversationState":{}}"#,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()["content-type"],
        "application/vnd.amazon.eventstream"
    );
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    let mut decoder = kiro_wire::EventStreamDecoder::new();
    decoder.feed(&bytes).unwrap();
    assert_eq!(
        decoder.decode().unwrap().unwrap().event_type(),
        Some("assistantResponseEvent")
    );
}

#[tokio::test]
async fn modern_routes_enforce_immediate_card_revocation() {
    use billing::{
        card::{Card, CardStatus},
        engine::BillingEngine,
    };
    let billing = BillingEngine::new();
    let mut card = Card::new("modern-card", "grp-pro", 100_000_000);
    card.status = CardStatus::Active;
    card.valid_until = Some(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 3600,
    );
    billing.upsert_card(card);
    let auth = AuthState::with_billing("modern-test-secret-32-characters-long", billing.clone());
    let token = auth.issue_token_for_card("modern-card", 3600).unwrap();
    let app = FacadeRegistry::default().into_router_with_auth(auth);
    for banned in [false, true] {
        if banned {
            billing.ban_card("modern-card", "regression").unwrap();
        }
        for (path, target) in [
            ("/List-Available-Models", None),
            (
                "/",
                Some("KiroControlPlaneBearerService.ListAvailableModels"),
            ),
        ] {
            let mut req = Request::builder()
                .uri(path)
                .method(if target.is_some() { "POST" } else { "GET" })
                .header("authorization", format!("Bearer {token}"));
            if let Some(target) = target {
                req = req.header("x-amz-target", target);
            }
            let response = app
                .clone()
                .oneshot(req.body(Body::from("{}")).unwrap())
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                if banned {
                    StatusCode::UNAUTHORIZED
                } else {
                    StatusCode::OK
                }
            );
        }
    }
}
