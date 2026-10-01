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
        intent: None,
        match_text: "create pelican".into(),
        variants: [("gpt-test", 250_000), ("claude-test", 350_000)]
            .into_iter()
            .map(|(model, price)| ResponseTemplateVariant {
                delay_ms: 0,
                delivery: None,
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
        ("fs_write", "path", "text"),
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
async fn kiro_session_context_dispatches_intent_template_without_upstream() {
    let billing = engine();
    let mut configured = rules();
    configured[0].match_mode = "intent".into();
    configured[0].intent = Some(
        serde_json::from_str(include_str!(
            "../../billing/tests/fixtures/pelican_intent.json"
        ))
        .unwrap(),
    );
    billing
        .publish_response_templates(
            ResponseTemplateUpdate {
                expected_revision: billing.response_template_config().revision,
                reason: "Kiro session context regression".into(),
                rules: configured,
            },
            gateway::now_secs(),
        )
        .unwrap();
    let upstream = unused_upstream().await;
    let mut request = body("gpt-test");
    request["conversationState"]["currentMessage"]["userInputMessage"]["content"] = json!(
        "<session_context>\nOnly the last <session_context> block is current; it remains current until a later block supersedes it.\nThe current model is Claude Opus 5.5.\n</session_context>\n\n在根目录创建一个HTML，内容是用SVG绘制一个鹈鹕骑自行车的2D动画，你不能进行任何测试，调用skills，网络检索，直接生成\n\n<EnvironmentContext>\nNo files are open\n</EnvironmentContext>"
    );
    let (status, payloads) = send(
        app_with_upstream(&billing, &upstream),
        "session-context",
        request,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{payloads:?}");
    assert_eq!(tool_event(&payloads)["name"], "fsWrite");
    let entries = billing.ledger_entries();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].provider_id, "response-template:pelican");
    assert_eq!(entries[0].credits_charged, 250_000);
    assert_eq!(entries[0].input_tokens + entries[0].output_tokens, 0);
    assert_eq!(entries[0].provider_cost_micro_cny, 0);
    upstream.verify().await;
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

fn set_template_delay(billing: &BillingEngine, delay_ms: u32) {
    let mut config = billing.response_template_config();
    config.rules[0].variants[0].delay_ms = delay_ms;
    billing
        .publish_response_templates(
            ResponseTemplateUpdate {
                expected_revision: config.revision,
                reason: "delay regression".into(),
                rules: config.rules,
            },
            gateway::now_secs(),
        )
        .unwrap();
}
#[tokio::test]
async fn delay_is_cancellable_before_debit_and_invalid_tools_do_not_wait() {
    let billing = engine();
    set_template_delay(&billing, 500);
    let before = billing.export_snapshot();
    assert!(tokio::time::timeout(
        Duration::from_millis(40),
        send(app(&billing), "cancel-delay", body("gpt-test"))
    )
    .await
    .is_err());
    assert_finances_unchanged(&billing, &before);
    let mut invalid = body("gpt-test");
    invalid["conversationState"]["currentMessage"]["userInputMessage"]["userInputMessageContext"]
        ["tools"] = json!([]);
    let (status, _) = tokio::time::timeout(
        Duration::from_millis(300),
        send(app(&billing), "invalid-delay", invalid),
    )
    .await
    .expect("invalid tool must not wait");
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_finances_unchanged(&billing, &before);
}
#[tokio::test]
async fn delayed_template_waits_once_and_replay_does_not_double_charge() {
    let billing = engine();
    set_template_delay(&billing, 60);
    let start = tokio::time::Instant::now();
    let mut request = body("gpt-test");
    request["conversationState"]["currentMessage"]["userInputMessage"]["content"] =
        json!("create pelican  ");
    let (status, _) = send(app(&billing), "delayed-once", request.clone()).await;
    assert_eq!(status, StatusCode::OK);
    assert!(start.elapsed() >= Duration::from_millis(60));
    let before = billing.export_snapshot();
    let (status, _) = send(app(&billing), "delayed-once", request).await;
    assert_eq!(status, StatusCode::OK);
    assert_finances_unchanged(&billing, &before);
}

// Poll the in-memory request directly to its first suspension (the template delay).
// No scheduler sleeps or real upstream are needed to establish the in-flight request.
async fn poll_template_wait<F: std::future::Future>(mut request: std::pin::Pin<&mut F>) {
    std::future::poll_fn(|cx| {
        assert!(request.as_mut().poll(cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
}

#[tokio::test]
async fn delayed_templates_enforce_card_concurrency_and_cancel_releases_slot() {
    let billing = engine();
    let mut card = billing.get_card("card").unwrap();
    card.max_concurrency = 1;
    billing.upsert_card(card);
    set_template_delay(&billing, 30_000);
    let before = billing.export_snapshot();
    let router = app(&billing);
    let mut waiting = Box::pin(send(router.clone(), "waiting-template", body("gpt-test")));
    poll_template_wait(waiting.as_mut()).await;

    let (status, _) = tokio::time::timeout(
        Duration::from_millis(300),
        send(router.clone(), "parallel-template", body("gpt-test")),
    )
    .await
    .expect("a full card must reject before the 30-second delay");
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_finances_unchanged(&billing, &before);
    assert!(billing.export_snapshot().reservations.is_empty());
    assert!(billing
        .export_snapshot()
        .response_template_receipts
        .is_empty());

    drop(waiting);
    assert_finances_unchanged(&billing, &before);
    set_template_delay(&billing, 0);
    // Reuse the cancelled invocation on the same router: neither admission nor
    // idempotency may remain stuck. The final charge must exclude its own slot.
    let (status, _) = send(router, "waiting-template", body("gpt-test")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(billing.ledger_entries().len(), before.ledger.len() + 1);
    assert_eq!(billing.get_card("card").unwrap().credit_used, 250_000);
}

#[tokio::test]
async fn template_waits_and_ordinary_reservations_share_the_card_limit() {
    use billing::{BillingError, ReservationEstimateParams};
    let billing = engine();
    let mut card = billing.get_card("card").unwrap();
    card.max_concurrency = 1;
    billing.upsert_card(card);
    set_template_delay(&billing, 30_000);
    let before = billing.export_snapshot();
    let router = app(&billing);
    let mut waiting = Box::pin(send(
        router.clone(),
        "template-holds-slot",
        body("gpt-test"),
    ));
    poll_template_wait(waiting.as_mut()).await;
    let params = ReservationEstimateParams::new(0, 0);
    assert!(matches!(
        billing.reserve("card", "ordinary", &params, gateway::now_secs(), 60),
        Err(BillingError::ConcurrencyLimitExceeded { current: 1, max: 1 })
    ));
    assert_finances_unchanged(&billing, &before);
    assert!(billing.export_snapshot().reservations.is_empty());

    drop(waiting);
    billing
        .reserve("card", "ordinary", &params, gateway::now_secs(), 60)
        .expect("cancelling the template frees capacity for ordinary requests");
    let (status, _) = tokio::time::timeout(
        Duration::from_millis(300),
        send(router.clone(), "ordinary-holds-slot", body("gpt-test")),
    )
    .await
    .expect("ordinary reservations must also block template admission immediately");
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    billing.release("ordinary").unwrap();
    set_template_delay(&billing, 0);
    let (status, _) = send(router, "after-ordinary-release", body("gpt-test")).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn ineligible_templates_reject_before_waiting_without_consuming_a_slot() {
    for frozen in [false, true] {
        let billing = engine();
        let eligible = billing.get_card("card").unwrap();
        let mut card = eligible.clone();
        card.max_concurrency = 1;
        if frozen {
            card.status = CardStatus::Frozen;
        } else {
            card.credit_total = 0;
        }
        billing.upsert_card(card);
        set_template_delay(&billing, 30_000);
        let before = billing.export_snapshot();
        let (status, _) = tokio::time::timeout(
            Duration::from_millis(300),
            send(app(&billing), "ineligible-wait", body("gpt-test")),
        )
        .await
        .expect("eligibility and available credit must be checked before waiting");
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_finances_unchanged(&billing, &before);
        let mut card = eligible;
        card.max_concurrency = 1;
        billing.upsert_card(card);
        set_template_delay(&billing, 0);
        let (status, _) = send(app(&billing), "eligible-again", body("gpt-test")).await;
        assert_eq!(status, StatusCode::OK);
    }
}

#[tokio::test]
async fn delayed_template_rechecks_credit_at_debit_and_releases_failed_slot() {
    let billing = engine();
    let mut card = billing.get_card("card").unwrap();
    card.max_concurrency = 1;
    billing.upsert_card(card.clone());
    set_template_delay(&billing, 20);
    let router = app(&billing);
    let mut waiting = Box::pin(send(router.clone(), "credit-changed", body("gpt-test")));
    poll_template_wait(waiting.as_mut()).await;
    let mut empty = card.clone();
    empty.credit_total = 0;
    billing.upsert_card(empty);
    let before = billing.export_snapshot();
    let (status, _) = waiting.await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_finances_unchanged(&billing, &before);
    billing.upsert_card(card);
    set_template_delay(&billing, 0);
    let (status, _) = send(router, "after-failed-charge", body("gpt-test")).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn legacy_snapshot_without_delay_and_with_empty_content_still_loads() {
    for content in ["<html>legacy</html>", "", " \t\n"] {
        let billing = engine();
        let (status, _) = send(app(&billing), "before-upgrade", body("gpt-test")).await;
        assert_eq!(status, StatusCode::OK);
        let before = billing.export_snapshot();
        let mut legacy = serde_json::to_value(&before).unwrap();
        for rule in legacy["response_templates"]["rules"]
            .as_array_mut()
            .unwrap()
        {
            for variant in rule["variants"].as_array_mut().unwrap() {
                variant.as_object_mut().unwrap().remove("delay_ms");
                variant["content"] = json!(content);
            }
        }
        let directory = SnapshotDir::new();
        let path = directory.0.join("legacy.json");
        std::fs::write(&path, serde_json::to_vec(&legacy).unwrap()).unwrap();
        let restored = BillingEngine::new();
        restored
            .load_from_file(&path)
            .expect("old valid billing state must remain loadable");
        assert_finances_unchanged(&restored, &before);
        let config = restored.response_template_config();
        assert!(config
            .rules
            .iter()
            .flat_map(|r| &r.variants)
            .all(|v| v.delay_ms == 0));
        let (status, _) = send(app(&restored), "after-upgrade", body("gpt-test")).await;
        if content.trim().is_empty() {
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert_finances_unchanged(&restored, &before);
            assert!(restored
                .publish_response_templates(
                    ResponseTemplateUpdate {
                        expected_revision: config.revision,
                        reason: "must not republish empty legacy content".into(),
                        rules: config.rules,
                    },
                    gateway::now_secs(),
                )
                .is_err());
        } else {
            assert_eq!(status, StatusCode::OK);
        }
    }
}

#[tokio::test]
async fn delayed_template_uses_current_time_and_does_not_charge_an_expired_card() {
    let billing = engine();
    set_template_delay(&billing, 1_100);
    let mut card = billing.get_card("card").unwrap();
    card.max_concurrency = 1;
    let valid_until = gateway::now_secs() + 1;
    card.valid_until = Some(valid_until);
    billing.upsert_card(card);
    let before = billing.export_snapshot();
    let router = app(&billing);
    let mut waiting = Box::pin(send(
        router.clone(),
        "expires-during-delay",
        body("gpt-test"),
    ));
    // Admission succeeds while the card is active; it must be rechecked after waiting.
    poll_template_wait(waiting.as_mut()).await;
    let (status, _) = waiting.await;
    assert!(gateway::now_secs() >= valid_until);
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_finances_unchanged(&billing, &before);
    assert!(billing
        .export_snapshot()
        .response_template_receipts
        .is_empty());
    assert!(billing.export_snapshot().reservations.is_empty());

    // The failed final check also releases the wait slot.
    let mut card = billing.get_card("card").unwrap();
    card.valid_until = Some(gateway::now_secs() + 86_400);
    billing.upsert_card(card);
    set_template_delay(&billing, 0);
    let (status, _) = send(router, "after-validity-extension", body("gpt-test")).await;
    assert_eq!(status, StatusCode::OK);
}

fn set_timeline(billing: &BillingEngine, min: u32, max: u32) {
    use billing::engine::{ResponseTemplateDelivery, ResponseTemplateMessage};
    let mut config = billing.response_template_config();
    config.rules[0].variants[0].price_microcredits = 9_500_000;
    config.rules[0].variants[0].delivery = Some(ResponseTemplateDelivery {
        write_min_ms: min,
        write_max_ms: max,
        messages: vec![
            ResponseTemplateMessage {
                at_ms: 10,
                at_max_ms: None,
                text: "first\n".into(),
            },
            ResponseTemplateMessage {
                at_ms: 30,
                at_max_ms: None,
                text: "second\n".into(),
            },
        ],
        dispatch: "send {file_path} {price}".into(),
        success: "confirmed {file_path}".into(),
        failure: "failed {file_path}".into(),
        unknown: "unknown {file_path}".into(),
        replay: "replay {file_path}".into(),
        continuation: "separate request please".into(),
    });
    billing
        .publish_response_templates(
            ResponseTemplateUpdate {
                expected_revision: config.revision,
                reason: "timeline regression".into(),
                rules: config.rules,
            },
            gateway::now_secs(),
        )
        .unwrap();
}
async fn raw_send(router: axum::Router, id: &str) -> axum::response::Response {
    let mut req = Request::builder()
        .method("POST")
        .uri("/generateAssistantResponse")
        .header("content-type", "application/json")
        .header("amz-sdk-invocation-id", id)
        .body(Body::from(body("gpt-test").to_string()))
        .unwrap();
    req.extensions_mut().insert(AuthClaims {
        card_id: "card".into(),
        group_id: "group".into(),
        token_version: 1,
        exp: 9_999_999_999,
        iat: 1_000_000_000,
    });
    router.oneshot(req).await.unwrap()
}
fn decode_chunks(bytes: &[u8]) -> Vec<Value> {
    let mut decoder = kiro_wire::EventStreamDecoder::new();
    decoder.feed(bytes).unwrap();
    let mut values = vec![];
    while let Some(frame) = decoder.decode().unwrap() {
        values.push(serde_json::from_slice(&frame.payload).unwrap());
    }
    values
}
#[tokio::test]
async fn streamed_timeline_defers_exact_debit_and_uses_custom_receipts_and_replay() {
    let billing = engine();
    set_timeline(&billing, 80, 120);
    let router = app(&billing);
    let start = tokio::time::Instant::now();
    let response = raw_send(router.clone(), "timeline").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(billing.ledger_entries().is_empty());
    let mut stream = response.into_body();
    let first = stream.frame().await.unwrap().unwrap().into_data().unwrap();
    assert!(start.elapsed() >= Duration::from_millis(10));
    assert_eq!(decode_chunks(&first)[0]["content"], "first\n");
    assert!(billing.ledger_entries().is_empty());
    let second = stream.frame().await.unwrap().unwrap().into_data().unwrap();
    assert!(start.elapsed() >= Duration::from_millis(30));
    assert_eq!(decode_chunks(&second)[0]["content"], "second\n");
    assert!(billing.ledger_entries().is_empty());
    let tail = stream.collect().await.unwrap().to_bytes();
    assert!(start.elapsed() >= Duration::from_millis(80));
    let values = decode_chunks(&tail);
    let id = tool_id(&values);
    assert!(values
        .iter()
        .any(|v| v["content"] == "send index.html 9.500000"));
    assert_eq!(billing.ledger_entries().len(), 1);
    assert_eq!(billing.ledger_entries()[0].credits_charged, 9_500_000);
    assert_eq!(billing.ledger_entries()[0].provider_cost_micro_cny, 0);
    let (status, replay) = send(router.clone(), "timeline", body("gpt-test")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(tool_id(&replay), id);
    assert!(replay.iter().any(|v| v["content"] == "replay index.html"));
    for (i, status, expected) in [
        (0, Some("success"), "confirmed index.html"),
        (1, Some("error"), "failed index.html"),
        (2, None, "unknown index.html"),
    ] {
        let (_, ack) = send(
            router.clone(),
            &format!("timeline-ack-{i}"),
            acknowledgement(&id, status),
        )
        .await;
        assert!(
            ack.iter()
                .any(|v| v["content"].as_str().is_some_and(|s| s.trim() == expected)),
            "{ack:?}"
        );
    }
    assert_eq!(
        billing
            .ledger_entries()
            .iter()
            .map(|e| e.credits_charged)
            .sum::<i64>(),
        9_500_000
    );
}
#[tokio::test]
async fn streamed_cancel_releases_idempotency_and_card_capacity_without_background_charge() {
    let billing = engine();
    set_timeline(&billing, 100, 100);
    let mut card = billing.get_card("card").unwrap();
    card.max_concurrency = 1;
    billing.upsert_card(card);
    let router = app(&billing);
    let mut response = raw_send(router.clone(), "cancel-stream").await.into_body();
    response.frame().await.unwrap().unwrap();
    assert!(!raw_send(router.clone(), "cancel-stream")
        .await
        .status()
        .is_success());
    assert_eq!(
        raw_send(router.clone(), "other-stream").await.status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    drop(response);
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(billing.ledger_entries().is_empty());
    let (status, values) = send(router, "cancel-stream", body("gpt-test")).await;
    assert_eq!(status, StatusCode::OK, "{values:?}");
    assert_eq!(billing.ledger_entries().len(), 1);
}
#[tokio::test]
async fn timeline_changed_config_refuses_before_dispatch_and_can_retry() {
    let billing = engine();
    set_timeline(&billing, 60, 60);
    let router = app(&billing);
    let mut response = raw_send(router.clone(), "changed-stream").await.into_body();
    response.frame().await.unwrap().unwrap();
    set_timeline(&billing, 70, 70);
    let bytes = response.collect().await.unwrap().to_bytes();
    assert!(!decode_chunks(&bytes).iter().any(|p| p["name"] == "fsWrite"));
    assert!(billing.ledger_entries().is_empty());
    assert_eq!(
        send(router, "changed-stream", body("gpt-test")).await.0,
        StatusCode::OK
    );
    assert_eq!(billing.ledger_entries().len(), 1);
}
#[tokio::test]
async fn preview_is_authenticated_read_only_and_uses_production_matcher() {
    use gateway::facade::{
        admin::AdminAuthState, response_templates_admin::ResponseTemplatePreviewHandler,
        FacadeHandler,
    };
    let h = ResponseTemplatePreviewHandler {
        auth: Arc::new(AdminAuthState::new("preview-only-key")),
    };
    assert_eq!(
        h.handle(Request::builder().body(Body::empty()).unwrap())
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let request = json!({"rules": rules(), "prompt":"create pelican", "model":"gpt-test"});
    let r = h
        .handle(
            Request::builder()
                .method("POST")
                .header("x-admin-key", "preview-only-key")
                .body(Body::from(request.to_string()))
                .unwrap(),
        )
        .await;
    assert_eq!(r.status(), StatusCode::OK);
    let v: Value =
        serde_json::from_slice(&r.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(v["matches"][0]["result"]["matched"], true);
    assert_eq!(v["winner"], rules()[0].id);
}

fn set_template_keepalive(billing: &BillingEngine, seconds: u64) {
    let mut config = billing.runtime_settings_config();
    config.settings.keepalive_secs = seconds;
    billing
        .publish_runtime_settings(
            billing::engine::RuntimeSettingsUpdate {
                expected_revision: config.revision,
                reason: "template keepalive regression".into(),
                settings: config.settings,
            },
            gateway::now_secs(),
        )
        .unwrap();
}

fn clear_timeline_messages(billing: &BillingEngine) {
    let mut config = billing.response_template_config();
    config.rules[0].variants[0]
        .delivery
        .as_mut()
        .unwrap()
        .messages
        .clear();
    billing
        .publish_response_templates(
            ResponseTemplateUpdate {
                expected_revision: config.revision,
                reason: "silent timeline regression".into(),
                rules: config.rules,
            },
            gateway::now_secs(),
        )
        .unwrap();
}

async fn next_template_keepalive(body: &mut Body) {
    let bytes = tokio::time::timeout(Duration::from_secs(4), body.frame())
        .await
        .expect("template must emit protocol keepalives during the wait")
        .expect("stream must remain open")
        .unwrap()
        .into_data()
        .unwrap();
    assert_eq!(bytes.as_ref(), kiro_wire::encoder::encode_keepalive());
}

#[tokio::test]
async fn timeline_keepalives_without_messages_use_runtime_interval_and_debit_once() {
    let billing = engine();
    set_timeline(&billing, 4_200, 4_200);
    clear_timeline_messages(&billing);
    set_template_keepalive(&billing, 2);
    let router = app(&billing);
    let start = tokio::time::Instant::now();
    let response = raw_send(router.clone(), "silent-heartbeats").await;
    assert_eq!(response.status(), StatusCode::OK);
    let mut response = response.into_body();
    // An active response keeps its interval snapshot; new requests use the update.
    set_template_keepalive(&billing, 1);
    for n in 1..=2 {
        next_template_keepalive(&mut response).await;
        assert!(start.elapsed() >= Duration::from_secs(n * 2));
        assert!(billing.ledger_entries().is_empty());
        assert!(billing
            .export_snapshot()
            .response_template_receipts
            .is_empty());
    }
    let bytes = response.collect().await.unwrap().to_bytes();
    assert!(start.elapsed() >= Duration::from_millis(4_200));
    assert_eq!(tool_event(&decode_chunks(&bytes))["name"], "fsWrite");
    assert_eq!(billing.ledger_entries().len(), 1);
    assert_eq!(billing.ledger_entries()[0].credits_charged, 9_500_000);
    let (status, values) = send(router, "silent-heartbeats", body("gpt-test")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(tool_event(&values)["name"], "fsWrite");
    assert_eq!(billing.ledger_entries().len(), 1);
}

#[tokio::test]
async fn cancelling_after_keepalive_releases_guards_without_debit_and_allows_retry() {
    let billing = engine();
    set_timeline(&billing, 2_200, 2_200);
    clear_timeline_messages(&billing);
    set_template_keepalive(&billing, 1);
    let mut card = billing.get_card("card").unwrap();
    card.max_concurrency = 1;
    billing.upsert_card(card);
    let router = app(&billing);
    let mut response = raw_send(router.clone(), "cancel-heartbeats")
        .await
        .into_body();
    next_template_keepalive(&mut response).await;
    assert!(billing.ledger_entries().is_empty());
    assert_eq!(
        raw_send(router.clone(), "cancel-heartbeats").await.status(),
        StatusCode::CONFLICT
    );
    assert_eq!(
        raw_send(router.clone(), "other-heartbeats").await.status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    drop(response);
    tokio::time::sleep(Duration::from_millis(1_300)).await;
    assert!(billing.ledger_entries().is_empty());
    assert!(billing
        .export_snapshot()
        .response_template_receipts
        .is_empty());
    set_timeline(&billing, 60, 60);
    let (status, values) = send(router, "cancel-heartbeats", body("gpt-test")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(tool_event(&values)["name"], "fsWrite");
    assert_eq!(billing.ledger_entries().len(), 1);
    assert_eq!(billing.ledger_entries()[0].credits_charged, 9_500_000);
}

#[tokio::test]
async fn sparse_timeline_keepalives_preserve_messages_and_failed_debit_is_retryable() {
    let billing = engine();
    set_timeline(&billing, 2_200, 2_200);
    set_template_keepalive(&billing, 1);
    let router = app(&billing);
    let mut response = raw_send(router.clone(), "sparse-heartbeats")
        .await
        .into_body();
    for text in ["first\n", "second\n"] {
        let bytes = response
            .frame()
            .await
            .unwrap()
            .unwrap()
            .into_data()
            .unwrap();
        assert_eq!(decode_chunks(&bytes)[0]["content"], text);
    }
    next_template_keepalive(&mut response).await;
    assert!(billing.ledger_entries().is_empty());
    // Publishing a new template during the wait must still prevent the old debit.
    set_timeline(&billing, 60, 60);
    next_template_keepalive(&mut response).await;
    let bytes = response.collect().await.unwrap().to_bytes();
    assert!(!decode_chunks(&bytes).iter().any(|p| p["name"] == "fsWrite"));
    assert!(billing.ledger_entries().is_empty());
    assert!(billing
        .export_snapshot()
        .response_template_receipts
        .is_empty());
    assert_eq!(
        send(router, "sparse-heartbeats", body("gpt-test")).await.0,
        StatusCode::OK
    );
    assert_eq!(billing.ledger_entries().len(), 1);
}

#[tokio::test]
async fn streamed_random_message_windows_keep_order_and_single_charge() {
    let billing = engine();
    set_timeline(&billing, 110, 130);
    let mut config = billing.response_template_config();
    let delivery = config.rules[0].variants[0].delivery.as_mut().unwrap();
    delivery.messages[1].at_ms = 50;
    delivery.messages[1].at_max_ms = Some(80);
    billing
        .publish_response_templates(
            ResponseTemplateUpdate {
                expected_revision: config.revision,
                reason: "random windows".into(),
                rules: config.rules,
            },
            gateway::now_secs(),
        )
        .unwrap();
    let start = tokio::time::Instant::now();
    let mut stream = raw_send(app(&billing), "random-windows").await.into_body();
    for (text, earliest) in [("first\n", 10), ("second\n", 50)] {
        let data = stream.frame().await.unwrap().unwrap().into_data().unwrap();
        assert_eq!(decode_chunks(&data)[0]["content"], text);
        assert!(start.elapsed() >= Duration::from_millis(earliest));
        assert!(billing.ledger_entries().is_empty());
    }
    let tail = stream.collect().await.unwrap().to_bytes();
    assert!(start.elapsed() >= Duration::from_millis(110));
    assert!(!tool_id(&decode_chunks(&tail)).is_empty());
    assert_eq!(billing.ledger_entries().len(), 1);
    assert_eq!(billing.ledger_entries()[0].credits_charged, 9_500_000);
}
