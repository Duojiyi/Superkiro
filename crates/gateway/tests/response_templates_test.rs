//! Template dispatch, per-model fixed fees and honest, free file-tool acknowledgements.
use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
};
use billing::{
    card::{Card, CardStatus},
    engine::{
        BillingEngine, BillingSnapshot, ResponseTemplateRule, ResponseTemplateUpdate,
        ResponseTemplateVariant,
    },
    group::{Group, ModelMap},
};
use gateway::{
    auth::AuthClaims,
    facade::{conversation::GenerateAssistantResponseHandler, FacadeRegistry},
    idempotency::IdempotencyManager,
    provider::{openai::OpenAiProvider, ProviderConfig},
};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tower::ServiceExt;
use wiremock::{matchers::method, Mock, MockServer, ResponseTemplate};

mod support;

// Respect the temporary directory configured by the caller and CI.
struct SnapshotDir(PathBuf);
impl SnapshotDir {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let root = std::env::temp_dir();
        std::fs::create_dir_all(&root).unwrap();
        let directory = root.join(format!(
            "gateway-response-templates-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir(&directory).unwrap();
        Self(directory)
    }
    fn restart(&self, billing: &BillingEngine) -> BillingEngine {
        let path = self.0.join("state.json");
        billing.save_to_file(&path).unwrap();
        let restored = BillingEngine::new();
        restored.load_from_file(&path).unwrap();
        restored
    }
}
impl Drop for SnapshotDir {
    fn drop(&mut self) {
        // This directory is uniquely created above; never remove the shared temp root.
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
async fn unused_upstream() -> MockServer {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&upstream)
        .await;
    upstream
}
fn app_with_upstream(billing: &BillingEngine, upstream: &MockServer) -> axum::Router {
    let mut registry = FacadeRegistry::default();
    registry.register(GenerateAssistantResponseHandler::new(
        reqwest::Client::new(),
        Arc::new(OpenAiProvider),
        ProviderConfig::new(
            upstream.uri(),
            "sk-test",
            "gpt-test",
            Duration::from_secs(2),
        ),
        billing.clone(),
        IdempotencyManager::default(),
    ));
    registry.into_router()
}
fn body_for_tool(name: &str, path_key: &str, content_key: &str) -> Value {
    let mut request = body("gpt-test");
    request["conversationState"]["currentMessage"]["userInputMessage"]["userInputMessageContext"]
        ["tools"] = json!([{"toolSpecification":{
        "name":name,"description":"Write a file","inputSchema":{"json":{
            "type":"object","properties":{
                path_key:{"type":"string"},content_key:{"type":"string"}
            },"required":[path_key,content_key],"additionalProperties":false
        }}
    }}]);
    request
}
fn tool_event(payloads: &[Value]) -> &Value {
    let events: Vec<_> = payloads
        .iter()
        .filter(|p| p.get("toolUseId").is_some())
        .collect();
    assert_eq!(
        events.len(),
        1,
        "expected exactly one file instruction: {payloads:?}"
    );
    events[0]
}
fn assert_finances_unchanged(billing: &BillingEngine, before: &BillingSnapshot) {
    assert_eq!(
        billing.get_card("card").unwrap().available_credits(),
        before.cards["card"].available_credits()
    );
    assert_eq!(
        serde_json::to_value(billing.ledger_entries()).unwrap(),
        serde_json::to_value(&before.ledger).unwrap()
    );
}

fn rules() -> Vec<ResponseTemplateRule> {
    vec![ResponseTemplateRule {
        id: "pelican".into(),
        name: "Test template".into(),
        enabled: true,
        match_mode: "exact".into(),
        match_text: "create pelican".into(),
        variants: [("gpt-test", 250_000), ("claude-test", 350_000)]
            .into_iter()
            .map(|(model, price)| ResponseTemplateVariant {
                model_id: model.into(),
                file_path: "index.html".into(),
                content: format!("<!doctype html><svg aria-label=\"{model}\">中文\n</svg>"),
                preamble: "配置的开场白".into(),
                completion: "配置的完成提示".into(),
                price_microcredits: price,
            })
            .collect(),
    }]
}
fn engine() -> BillingEngine {
    let billing = BillingEngine::new();
    billing.upsert_group(Group::pro_plus("group", "Templates"));
    for model in ["gpt-test", "claude-test"] {
        billing.upsert_model_map(ModelMap::new(model, "group", model, "provider", model));
    }
    let mut card = Card::new("card", "group", 10_000_000);
    card.activate(gateway::now_secs(), 86_400).unwrap();
    billing.upsert_card(card);
    billing.upsert_rate_card_version(support::wildcard_price("default"));
    billing
        .publish_response_templates(
            ResponseTemplateUpdate {
                expected_revision: billing.response_template_config().revision,
                reason: "Isolated integration test".into(),
                rules: rules(),
            },
            gateway::now_secs(),
        )
        .unwrap();
    billing
}
fn app(billing: &BillingEngine) -> axum::Router {
    let mut registry = FacadeRegistry::default();
    registry.register(GenerateAssistantResponseHandler {
        billing: billing.clone(),
        ..Default::default()
    });
    registry.into_router()
}
fn body(model: &str) -> Value {
    json!({"conversationState":{"conversationId":"conversation","currentMessage":{"userInputMessage":{"content":"create pelican","modelId":model,"userInputMessageContext":{"tools":[{"toolSpecification":{"name":"fsWrite","description":"Write a file","inputSchema":{"json":{"type":"object","properties":{"path":{"type":"string"},"text":{"type":"string"}},"required":["path","text"]}}}}]}}}}})
}
async fn send(app: axum::Router, id: &str, body: Value) -> (StatusCode, Vec<Value>) {
    let mut req = Request::builder()
        .method(Method::POST)
        .uri("/generateAssistantResponse")
        .header("content-type", "application/json")
        .header("amz-sdk-invocation-id", id)
        .body(Body::from(body.to_string()))
        .unwrap();
    req.extensions_mut().insert(AuthClaims {
        card_id: "card".into(),
        group_id: "group".into(),
        token_version: 1,
        exp: 9_999_999_999,
        iat: 1_000_000_000,
    });
    let response = app.oneshot(req).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    if !status.is_success() {
        return (status, vec![serde_json::from_slice(&bytes).unwrap()]);
    }
    let mut decoder = kiro_wire::EventStreamDecoder::new();
    decoder.feed(&bytes).unwrap();
    let mut payloads = vec![];
    while let Some(frame) = decoder.decode().unwrap() {
        payloads.push(serde_json::from_slice(&frame.payload).unwrap());
    }
    (status, payloads)
}
fn tool_id(payloads: &[Value]) -> String {
    payloads
        .iter()
        .find_map(|p| p.get("toolUseId").and_then(Value::as_str))
        .unwrap()
        .into()
}
fn acknowledgement(id: &str, status: Option<&str>) -> Value {
    let mut result = json!({"toolUseId":id,"content":[{"text":"client result"}]});
    if let Some(status) = status {
        result["status"] = json!(status);
    }
    json!({"conversationState":{"conversationId":"conversation","currentMessage":{"userInputMessage":{"content":"","modelId":"gpt-test","userInputMessageContext":{"toolResults":[result]}}}}})
}
#[tokio::test]
async fn same_prompt_selects_model_content_and_exact_price_without_tokens() {
    let billing = engine();
    let app = app(&billing);
    for (i, model, price) in [(0, "gpt-test", 250_000), (1, "claude-test", 350_000)] {
        let (status, payloads) = send(app.clone(), &format!("model-{i}"), body(model)).await;
        assert_eq!(status, StatusCode::OK, "{payloads:?}");
        let tool = payloads.iter().find(|p| p["name"] == "fsWrite").unwrap();
        let input: Value = serde_json::from_str(tool["input"].as_str().unwrap()).unwrap();
        assert_eq!(input["path"], "index.html");
        assert!(input["text"].as_str().unwrap().contains(model));
        assert_eq!(payloads.last().unwrap()["stopReason"], "tool_use");
        assert_eq!(payloads.last().unwrap()["tokenUsage"]["outputTokens"], 0);
        let entries = billing.ledger_entries();
        let entry = entries.last().unwrap();
        assert_eq!(entry.credits_charged, price);
        assert_eq!(entry.provider_cost_micro_cny, 0);
        assert_eq!(
            entry.input_tokens
                + entry.output_tokens
                + entry.cache_read_tokens
                + entry.cache_creation_tokens,
            0
        );
        assert_eq!(entry.provider_id, "response-template:pelican");
        assert!(payloads.iter().any(|p| p["content"]
            .as_str()
            .is_some_and(|s| s.contains("非模型实时生成") && s.contains("尚未确认"))));
    }
    assert_eq!(
        billing.get_card("card").unwrap().available_credits(),
        9_400_000
    );
}
#[tokio::test]
async fn acknowledgements_are_free_scoped_honest_and_do_not_retrigger() {
    let billing = engine();
    let app = app(&billing);
    let (_, payloads) = send(app.clone(), "dispatch", body("gpt-test")).await;
    let id = tool_id(&payloads);
    for (i, status, expected) in [
        (0, Some("success"), "客户端报告已写入"),
        (1, Some("error"), "失败"),
        (2, None, "未确认写入成功"),
    ] {
        let (code, reply) = send(
            app.clone(),
            &format!("ack-{i}"),
            acknowledgement(&id, status),
        )
        .await;
        assert_eq!(code, StatusCode::OK, "{reply:?}");
        let text = reply
            .iter()
            .filter_map(|p| p["content"].as_str())
            .collect::<String>();
        assert!(text.contains(expected), "{text}");
        if status != Some("success") {
            assert!(!text.contains("配置的完成提示"));
        }
        assert!(!reply.iter().any(|p| p.get("toolUseId").is_some()));
        assert_eq!(reply.last().unwrap()["stopReason"], "end_turn");
    }
    let mut continuation = acknowledgement(&id, Some("success"));
    continuation["conversationState"]["currentMessage"]["userInputMessage"]["content"] =
        json!("Tool execution complete, continue.");
    let (code, reply) = send(app.clone(), "ack-with-text", continuation).await;
    assert_eq!(code, StatusCode::OK);
    assert!(reply.iter().any(|p| p["content"]
        .as_str()
        .is_some_and(|text| text.contains("本轮仅确认模板工具回执"))));
    assert_eq!(billing.ledger_entries().len(), 1);
    assert_eq!(
        billing.get_card("card").unwrap().available_credits(),
        9_750_000
    );
    assert!(billing
        .response_template_receipt("different-card", "conversation", &id, gateway::now_secs())
        .is_none());
    assert!(billing
        .response_template_receipt("card", "different-conversation", &id, gateway::now_secs())
        .is_none());
}
#[tokio::test]
async fn all_file_tools_replay_identical_arguments_after_durable_restart_without_rebilling() {
    let assert_metering = |payloads: &[Value], expected: f64| {
        let metering: Vec<_> = payloads
            .iter()
            .filter(|payload| payload.get("usage").is_some())
            .collect();
        assert_eq!(
            metering.len(),
            1,
            "expected one metering event: {payloads:?}"
        );
        assert_eq!(
            metering[0],
            &json!({"usage": expected, "unit": "credit", "unitPlural": "credits"})
        );
    };
    let upstream = unused_upstream().await;
    for (name, path_key, content_key) in [
        ("fsWrite", "path", "text"),
        ("Write", "file_path", "content"),
        ("write_file", "path", "content"),
        ("writeFile", "path", "content"),
    ] {
        let directory = SnapshotDir::new();
        let billing = engine();
        let request = body_for_tool(name, path_key, content_key);
        let router = app_with_upstream(&billing, &upstream);
        let (status, first) = send(router.clone(), "durable-replay", request.clone()).await;
        assert_eq!(status, StatusCode::OK, "{name}: {first:?}");
        assert_metering(&first, 0.25);
        let event = tool_event(&first);
        assert_eq!(event["name"], name);
        assert_eq!(event["stop"], true);
        assert!(tool_id(&first).starts_with("template_"));
        let input: Value = serde_json::from_str(event["input"].as_str().unwrap()).unwrap();
        let variant = rules().remove(0).variants.remove(0);
        assert_eq!(
            input,
            json!({path_key:variant.file_path,content_key:variant.content})
        );
        assert_eq!(billing.ledger_entries().len(), 1);
        assert_eq!(billing.ledger_entries()[0].credits_charged, 250_000);
        let before = billing.export_snapshot();

        let (status, cached) = send(router.clone(), "durable-replay", request.clone()).await;
        assert_eq!(status, StatusCode::OK, "{name}: {cached:?}");
        assert_metering(&cached, 0.0);
        assert_eq!(tool_event(&cached), event);
        assert_finances_unchanged(&billing, &before);

        // Serialize to disk and load a NEW engine, not just a new handler over shared memory.
        let restored = directory.restart(&billing);
        drop(router);
        drop(billing);
        let (status, replay) = send(
            app_with_upstream(&restored, &upstream),
            "durable-replay",
            request,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{name}: {replay:?}");
        assert_metering(&replay, 0.0);
        assert_eq!(tool_id(&replay), tool_id(&first));
        assert_eq!(
            tool_event(&replay),
            event,
            "{name}: full tool event changed"
        );
        assert_eq!(replay.last().unwrap()["stopReason"], "tool_use");
        assert_eq!(restored.ledger_entries().len(), 1);
        assert_finances_unchanged(&restored, &before);
    }
    assert!(upstream.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn pruned_late_template_acknowledgement_is_rejected_without_upstream_or_charge() {
    let upstream = unused_upstream().await;
    let directory = SnapshotDir::new();
    let billing = engine();
    let (status, first) = send(
        app_with_upstream(&billing, &upstream),
        "generate-a",
        body("gpt-test"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{first:?}");
    let id = tool_id(&first);
    let mut snapshot = billing.export_snapshot();
    let old = gateway::now_secs() - 86_401;
    let receipt = snapshot
        .response_template_receipts
        .iter_mut()
        .find(|r| r.tool_use_id == id)
        .unwrap();
    receipt.created_at_secs = old;
    let invocation = receipt.invocation_id.clone();
    snapshot
        .ledger
        .iter_mut()
        .find(|e| e.invocation_id.as_deref() == Some(invocation.as_str()))
        .unwrap()
        .ts_secs = old;
    billing.import_snapshot(snapshot);
    let restored = directory.restart(&billing);
    drop(billing);
    let router = app_with_upstream(&restored, &upstream);

    // Exercise the expired-but-not-yet-pruned branch first.
    let before = restored.export_snapshot();
    let (status, reply) = send(
        router.clone(),
        "late-before-prune",
        acknowledgement(&id, Some("success")),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{reply:?}");
    assert_finances_unchanged(&restored, &before);

    let (status, second) = send(router.clone(), "generate-b", body("gpt-test")).await;
    assert_eq!(status, StatusCode::OK, "{second:?}");
    assert_ne!(tool_id(&second), id);
    assert!(restored
        .response_template_receipt_any("card", "conversation", &id)
        .is_none());
    assert_eq!(
        restored.export_snapshot().response_template_receipts.len(),
        1
    );
    assert_eq!(restored.ledger_entries().len(), 2);
    assert_eq!(
        restored.get_card("card").unwrap().available_credits(),
        9_500_000
    );
    let before = restored.export_snapshot();
    for result in ["success", "error"] {
        let (status, reply) = send(
            router.clone(),
            &format!("late-after-prune-{result}"),
            acknowledgement(&id, Some(result)),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{reply:?}");
        assert_finances_unchanged(&restored, &before);
    }
    assert!(upstream.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn mixed_and_cross_conversation_template_receipts_are_rejected_for_free() {
    let upstream = unused_upstream().await;
    let billing = engine();
    let router = app_with_upstream(&billing, &upstream);
    let (status, first) = send(router.clone(), "receipt-source", body("gpt-test")).await;
    assert_eq!(status, StatusCode::OK, "{first:?}");
    let id = tool_id(&first);
    let before = billing.export_snapshot();
    for case in [
        "mixed-template-first",
        "mixed-template-last",
        "cross-conversation",
        "unknown-template",
    ] {
        let mut request = acknowledgement(&id, Some("success"));
        match case {
            "cross-conversation" => {
                request["conversationState"]["conversationId"] = json!("another-conversation")
            }
            "unknown-template" => {
                request = acknowledgement("template_unknown_receipt", Some("success"))
            }
            _ => {
                let results = request["conversationState"]["currentMessage"]["userInputMessage"]
                    ["userInputMessageContext"]["toolResults"]
                    .as_array_mut()
                    .unwrap();
                results.push(json!({"toolUseId":"ordinary-tool-result","status":"success","content":[{"text":"done"}]}));
                if case == "mixed-template-last" {
                    results.reverse();
                }
            }
        }
        let (status, reply) = send(router.clone(), case, request).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{case}: {reply:?}");
        assert_finances_unchanged(&billing, &before);
    }
    assert!(upstream.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn invalid_recovery_context_schema_or_card_is_rejected_without_rebilling() {
    let upstream = unused_upstream().await;
    for restart in [false, true] {
        for case in [
            "conversation",
            "model",
            "no-tool",
            "different-tool",
            "extra-required",
            "wrong-type",
            "content-limit",
            "banned",
            "frozen",
            "expired",
        ] {
            let directory = SnapshotDir::new();
            let billing = engine();
            let router = app_with_upstream(&billing, &upstream);
            let (status, first) = send(router.clone(), "invalid-recovery", body("gpt-test")).await;
            assert_eq!(status, StatusCode::OK, "{first:?}");
            let (billing, router) = if restart {
                let restored = directory.restart(&billing);
                drop(router);
                drop(billing);
                let router = app_with_upstream(&restored, &upstream);
                (restored, router)
            } else {
                (billing, router)
            };
            let mut request = body("gpt-test");
            match case {
                "conversation" => {
                    request["conversationState"]["conversationId"] = json!("another-conversation")
                }
                "model" => {
                    request["conversationState"]["currentMessage"]["userInputMessage"]["modelId"] =
                        json!("claude-test")
                }
                "different-tool" => request = body_for_tool("Write", "file_path", "content"),
                "banned" | "frozen" | "expired" => {
                    let mut card = billing.get_card("card").unwrap();
                    card.status = match case {
                        "banned" => CardStatus::Banned,
                        "frozen" => CardStatus::Frozen,
                        _ => CardStatus::Expired,
                    };
                    billing.upsert_card(card);
                }
                _ => {
                    let tools = &mut request["conversationState"]["currentMessage"]
                        ["userInputMessage"]["userInputMessageContext"]["tools"];
                    if case == "no-tool" {
                        *tools = json!([]);
                    } else {
                        let schema = &mut tools[0]["toolSpecification"]["inputSchema"]["json"];
                        match case {
                            "extra-required" => {
                                schema["required"] = json!(["path", "text", "root"])
                            }
                            "wrong-type" => schema["properties"]["text"]["type"] = json!("number"),
                            "content-limit" => schema["properties"]["text"]["maxLength"] = json!(1),
                            _ => unreachable!(),
                        }
                    }
                }
            }
            let before = billing.export_snapshot();
            let (status, reply) = send(router, "invalid-recovery", request).await;
            assert!(
                status.is_client_error(),
                "restart={restart}, {case}: {status} {reply:?}"
            );
            assert!(!reply.iter().any(|p| p.get("toolUseId").is_some()));
            assert_eq!(billing.ledger_entries().len(), 1);
            assert_finances_unchanged(&billing, &before);
        }
    }
    assert!(upstream.received_requests().await.unwrap().is_empty());
}
#[tokio::test]
async fn incompatible_tools_banned_cards_and_insufficient_balance_are_free() {
    let billing = engine();
    let app = app(&billing);
    let mut invalid = body("gpt-test");
    invalid["conversationState"]["currentMessage"]["userInputMessage"]["userInputMessageContext"]
        ["tools"] = json!([]);
    assert_eq!(
        send(app.clone(), "unsupported", invalid).await.0,
        StatusCode::BAD_REQUEST
    );
    let mut card = billing.get_card("card").unwrap();
    card.status = CardStatus::Banned;
    billing.upsert_card(card.clone());
    assert_eq!(
        send(app.clone(), "banned", body("gpt-test")).await.0,
        StatusCode::BAD_REQUEST
    );
    card.status = CardStatus::Active;
    card.credit_total = 1;
    billing.upsert_card(card);
    assert_eq!(
        send(app.clone(), "poor", body("gpt-test")).await.0,
        StatusCode::BAD_REQUEST
    );
    assert!(billing.ledger_entries().is_empty());
}
#[tokio::test]
async fn historical_match_and_unconfigured_models_do_not_dispatch_templates() {
    let billing = engine();
    let app = app(&billing);
    let mut request = body("gpt-test");
    request["conversationState"]["history"] =
        json!([{"userInputMessage":{"content":"create pelican"}}]);
    request["conversationState"]["currentMessage"]["userInputMessage"]["content"] =
        json!("something else");
    let (_, payloads) = send(app.clone(), "history", request).await;
    assert!(!payloads.iter().any(|p| p["name"] == "fsWrite"));
    let (_, payloads) = send(app, "unconfigured", body("other-model")).await;
    assert!(!payloads.iter().any(|p| p["name"] == "fsWrite"));
    assert!(billing
        .ledger_entries()
        .iter()
        .all(|e| !e.provider_id.starts_with("response-template:")));
}

#[tokio::test]
async fn admin_configuration_requires_auth_and_rejects_stale_updates() {
    use gateway::facade::{
        admin::AdminAuthState, response_templates_admin::ResponseTemplatesHandler, FacadeHandler,
    };
    use std::sync::Arc;
    let billing = engine();
    let commercial = billing.commercial_config().revision;
    let auth = Arc::new(AdminAuthState::new("test-only-admin-key"));
    let get = ResponseTemplatesHandler {
        billing: billing.clone(),
        auth: auth.clone(),
        publish: false,
    };
    assert_eq!(
        get.handle(Request::builder().body(Body::empty()).unwrap())
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let post = ResponseTemplatesHandler {
        billing: billing.clone(),
        auth,
        publish: true,
    };
    let update = ResponseTemplateUpdate {
        expected_revision: billing.response_template_config().revision,
        reason: "admin test".into(),
        rules: vec![],
    };
    let request = || {
        Request::builder()
            .method("POST")
            .header("x-admin-key", "test-only-admin-key")
            .body(Body::from(serde_json::to_vec(&update).unwrap()))
            .unwrap()
    };
    assert_eq!(post.handle(request()).await.status(), StatusCode::OK);
    assert_eq!(post.handle(request()).await.status(), StatusCode::CONFLICT);
    assert_eq!(billing.commercial_config().revision, commercial);
}

#[tokio::test]
async fn production_cookie_session_enforces_csrf_before_template_publication() {
    // This is the only test in this binary that changes browser-login environment.
    // Restore even on assertion failure; no process-wide settings leak to later tests.
    struct RestoreEnv(Vec<(&'static str, Option<std::ffi::OsString>)>);
    impl Drop for RestoreEnv {
        fn drop(&mut self) {
            for (name, value) in &self.0 {
                match value {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
        }
    }
    let _restore = RestoreEnv(
        [
            "ADMIN_BROWSER_LOGIN",
            "ADMIN_ORIGIN",
            "ADMIN_PASSWORD_HASH",
            "ADMIN_TOTP_SECRET",
            "ADMIN_TOTP_REQUIRED",
        ]
        .into_iter()
        .map(|name| (name, std::env::var_os(name)))
        .collect(),
    );
    std::env::set_var("ADMIN_BROWSER_LOGIN", "true");
    std::env::set_var("ADMIN_ORIGIN", "https://admin.test");
    std::env::set_var(
        "ADMIN_PASSWORD_HASH",
        bcrypt::hash("template-test-password", 4).unwrap(),
    );
    std::env::remove_var("ADMIN_TOTP_SECRET");
    std::env::remove_var("ADMIN_TOTP_REQUIRED");

    let billing = engine();
    let mut registry = FacadeRegistry::new();
    registry.register_admin_facades_secure(
        billing.clone(),
        "template-browser-signing-key-32bytes-long".into(),
    );
    let router = registry.into_router_with_auth(gateway::auth::AuthState::new(
        "template-browser-client-secret-32bytes-long",
    ));
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/v1/admin/session")
                .header("origin", "https://admin.test")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"username":"admin","password":"template-test-password"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let cookie_header = response.headers()["set-cookie"].to_str().unwrap();
    for flag in [
        "__Host-admin_session=",
        "Secure",
        "HttpOnly",
        "SameSite=Strict",
        "Path=/",
    ] {
        assert!(cookie_header.contains(flag));
    }
    let cookie = cookie_header.split(';').next().unwrap().to_owned();
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/admin/session")
                .header("origin", "https://admin.test")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let session: Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), 65536)
            .await
            .unwrap(),
    )
    .unwrap();
    let csrf = session["csrfToken"].as_str().unwrap();
    assert!(!csrf.is_empty());

    let before = serde_json::to_value(billing.response_template_config()).unwrap();
    let mut published_rules = rules();
    published_rules[0].name = "Published through cookie session".into();
    published_rules[0].variants[0].content = "<svg>cookie publication</svg>".into();
    let update = ResponseTemplateUpdate {
        expected_revision: billing.response_template_config().revision,
        reason: "Production cookie CSRF integration test".into(),
        rules: published_rules,
    };
    let request = |origin: &str, token: Option<&str>| {
        let mut req = Request::builder()
            .method(Method::POST)
            .uri("/api/v1/admin/response-templates")
            .header("origin", origin)
            .header("cookie", &cookie)
            .header("content-type", "application/json");
        if let Some(token) = token {
            req = req.header("x-csrf-token", token);
        }
        req.body(Body::from(serde_json::to_vec(&update).unwrap()))
            .unwrap()
    };
    for (origin, token) in [
        ("https://admin.test", None),
        ("https://admin.test", Some("wrong-token")),
        ("https://evil.test", Some(csrf)),
    ] {
        let response = router
            .clone()
            .oneshot(request(origin, token))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::FORBIDDEN,
            "origin={origin}, token={token:?}"
        );
        assert_eq!(
            serde_json::to_value(billing.response_template_config()).unwrap(),
            before
        );
    }
    let response = router
        .clone()
        .oneshot(request("https://admin.test", Some(csrf)))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let published: Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), 65536)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(published["success"], true);
    let config = billing.response_template_config();
    assert_ne!(config.revision, update.expected_revision);
    assert_eq!(
        serde_json::to_value(&config.rules).unwrap(),
        serde_json::to_value(&update.rules).unwrap()
    );
    assert_eq!(published["config"], serde_json::to_value(&config).unwrap());

    // Read back through the production route with only the authenticated cookie.
    let response = router
        .oneshot(
            Request::builder()
                .uri("/api/v1/admin/response-templates")
                .header("origin", "https://admin.test")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let readback: Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), 65536)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(readback["config"], published["config"]);
}
