use billing::engine::{
    Complexity, ComplexityRoutingUpdate, RoutingClassifier, RoutingMode, RoutingPolicy,
};
use billing::{BillingEngine, Group, ModelMap, Provider, ProviderFormat, ProviderKey};
use gateway::complexity_routing::{
    apply_decision, digest, ComplexityRouter, RoutingInput, RoutingRequest,
};
use gateway::provider::governance::ProviderKeyPool;
use std::time::Duration;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn fixture(url: &str, mode: RoutingMode) -> (BillingEngine, ProviderKeyPool) {
    let e = BillingEngine::new();
    e.upsert_group(Group::pro_plus("group", "Test group"));
    for id in ["cheap", "reliable", "classifier"] {
        e.upsert_provider(Provider::new(id, id, ProviderFormat::OpenAi, url));
    }
    e.upsert_model_map(
        ModelMap::new("mapping", "group", "opus-test", "cheap", "opus-test")
            .with_fallback("reliable", "opus-test"),
    );
    let provider = e.get_provider("classifier").unwrap();
    let key = ProviderKey::new("classifier-key", "classifier", "fixture-secret");
    e.upsert_provider_key(key.clone());
    let pool = ProviderKeyPool::new(provider, vec![key]);
    let config = e.complexity_routing_config();
    e.publish_complexity_routing(
        ComplexityRoutingUpdate {
            expected_revision: config.revision,
            reason: "test".into(),
            classifier: Some(RoutingClassifier {
                provider_id: "classifier".into(),
                model: "classifier-model".into(),
                timeout_ms: 500,
                max_input_chars: 4096,
                daily_request_limit: 100,
                daily_budget_micro_cny: 1_000_000,
                input_price_micro_cny_per_million: 1_000_000,
                output_price_micro_cny_per_million: 2_000_000,
            }),
            policies: vec![RoutingPolicy {
                model_map_id: "mapping".into(),
                mode,
                simple_provider_ids: vec!["cheap".into(), "reliable".into()],
                complex_provider_ids: vec!["reliable".into()],
            }],
        },
        86400,
    )
    .unwrap();
    (e, pool)
}
fn request(id: &str) -> RoutingRequest {
    let input = RoutingInput {
        text: "你好".into(),
        history: String::new(),
        continuation: false,
        has_attachments: false,
        high_reasoning: false,
        context_complete: true,
        input_chars: 2,
    };
    RoutingRequest {
        invocation_id: id.into(),
        scope: digest(&"card:conversation:mapping"),
        request_hash: digest(&input),
        input,
        eligible_provider_ids: vec!["cheap".into(), "reliable".into()],
        preview: false,
        now: 86401,
    }
}
fn completion(complexity: &str, task: &str) -> serde_json::Value {
    serde_json::json!({"choices":[{"finish_reason":"stop","message":{"content":serde_json::json!({
        "complexity":complexity,"task_type":task,"context_sufficient":true,"reason_codes":["SHORT_STANDALONE"]
    }).to_string()}}],"usage":{"prompt_tokens":123,"completion_tokens":30}})
}
async fn decide(
    router: &ComplexityRouter,
    e: &BillingEngine,
    pool: &ProviderKeyPool,
    req: RoutingRequest,
) -> billing::engine::RoutingDecision {
    let c = e.complexity_routing_config();
    router
        .decide(e, &c, &c.policies[0], req, Some(pool.clone()))
        .await
        .unwrap()
}

#[tokio::test]
async fn simple_routes_once_and_retry_survives_restart_without_customer_billing() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(completion("simple", "general")))
        .expect(1)
        .mount(&server)
        .await;
    let (e, pool) = fixture(&server.uri(), RoutingMode::Enforce);
    let router = ComplexityRouter::default();
    let d = decide(&router, &e, &pool, request("one")).await;
    assert_eq!(d.complexity, Complexity::Simple);
    assert_eq!(d.provider_ids, vec!["cheap", "reliable"]);
    assert_eq!(d.classifier_cost_micro_cny, 183);
    assert!(!d.usage_estimated);
    let snapshot = e.export_snapshot();
    assert!(snapshot.ledger.is_empty());
    let restored = BillingEngine::new();
    restored.import_snapshot(snapshot);
    assert_eq!(decide(&router, &restored, &pool, request("one")).await, d);
    assert_eq!(restored.routing_status().budget.calls, 1);
    let requests = server.received_requests().await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(payload["model"], "classifier-model");
    assert_eq!(payload["stream"], false);
    assert!(payload.get("tools").is_none());
}

#[tokio::test]
async fn observe_reports_suggestion_without_changing_candidate_order() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(completion("complex", "debugging")))
        .mount(&server)
        .await;
    let (e, pool) = fixture(&server.uri(), RoutingMode::Observe);
    let d = decide(&ComplexityRouter::default(), &e, &pool, request("observe")).await;
    assert_eq!(d.provider_ids, vec!["reliable"]);
    let original = vec![("cheap".into(), 1), ("reliable".into(), 2)];
    assert_eq!(apply_decision(&original, &d), original);
}

#[tokio::test]
async fn timeout_invalid_response_and_unknown_are_conservative_and_not_retried() {
    for (id, response, reason) in [
        (
            "timeout",
            ResponseTemplate::new(200).set_delay(Duration::from_millis(800)),
            "classifier_timeout",
        ),
        ("http", ResponseTemplate::new(429), "classifier_http_error"),
        (
            "invalid",
            ResponseTemplate::new(200).set_body_string("bad json"),
            "classifier_invalid_response",
        ),
        (
            "unknown",
            ResponseTemplate::new(200).set_body_json(completion("unknown", "unknown")),
            "semantic_uncertain",
        ),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(response)
            .expect(1)
            .mount(&server)
            .await;
        let (e, pool) = fixture(&server.uri(), RoutingMode::Enforce);
        let router = ComplexityRouter::default();
        let d = decide(&router, &e, &pool, request(id)).await;
        assert_eq!(d.reason, reason);
        assert_eq!(d.provider_ids, vec!["reliable"]);
        assert_eq!(decide(&router, &e, &pool, request(id)).await, d);
        assert!(d.classifier_cost_micro_cny > 0);
    }
}

#[tokio::test]
async fn budget_exhaustion_and_hard_constraints_never_call_classifier() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;
    let (e, pool) = fixture(&server.uri(), RoutingMode::Enforce);
    let mut c = e.complexity_routing_config();
    c.classifier.as_mut().unwrap().daily_budget_micro_cny = 1;
    e.publish_complexity_routing(
        ComplexityRoutingUpdate {
            expected_revision: c.revision,
            reason: "budget".into(),
            classifier: c.classifier,
            policies: c.policies,
        },
        86401,
    )
    .unwrap();
    let router = ComplexityRouter::default();
    let d = decide(&router, &e, &pool, request("budget")).await;
    assert_eq!(d.reason, "budget_exhausted");
    assert!(!d.classifier_attempted);
    let mut r = request("attachments");
    r.input.has_attachments = true;
    assert_eq!(
        decide(&router, &e, &pool, r).await.reason,
        "capability_required"
    );
    let mut r = request("long");
    r.input.input_chars = 50000;
    assert_eq!(
        decide(&router, &e, &pool, r).await.reason,
        "insufficient_context"
    );
    let mut r = request("history");
    r.input.context_complete = false;
    assert_eq!(
        decide(&router, &e, &pool, r).await.reason,
        "insufficient_context"
    );
}

#[tokio::test]
async fn continuation_uses_actual_provider_and_cross_card_scope_cannot_inherit() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(completion("simple", "general")))
        .expect(1)
        .mount(&server)
        .await;
    let (e, pool) = fixture(&server.uri(), RoutingMode::Enforce);
    let router = ComplexityRouter::default();
    decide(&router, &e, &pool, request("start")).await;
    e.record_routing_served("start", "reliable").unwrap();
    let mut r = request("follow");
    r.input.continuation = true;
    r.now += 1;
    let d = decide(&router, &e, &pool, r).await;
    assert_eq!(d.reason, "task_continuation");
    assert_eq!(d.provider_ids[0], "reliable");
    assert!(!d.classifier_attempted);
    let mut r = request("other");
    r.input.continuation = true;
    r.scope = digest(&"another-card");
    assert_eq!(
        decide(&router, &e, &pool, r).await.reason,
        "continuation_without_state"
    );
}

#[tokio::test]
async fn misleading_simple_label_for_coding_cannot_pick_cheap_channel() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(completion("simple", "coding")))
        .mount(&server)
        .await;
    let (e, pool) = fixture(&server.uri(), RoutingMode::Enforce);
    let d = decide(&ComplexityRouter::default(), &e, &pool, request("coding")).await;
    assert_eq!(d.complexity, Complexity::Complex);
    assert_eq!(d.provider_ids, vec!["reliable"]);
}

#[tokio::test]
async fn three_classifier_failures_open_circuit_but_main_request_keeps_default_chain() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500))
        .expect(3)
        .mount(&server)
        .await;
    let (e, pool) = fixture(&server.uri(), RoutingMode::Enforce);
    let router = ComplexityRouter::default();
    for i in 0..3 {
        assert_eq!(
            decide(&router, &e, &pool, request(&format!("fail-{i}")))
                .await
                .reason,
            "classifier_http_error"
        );
    }
    let d = decide(&router, &e, &pool, request("circuit")).await;
    assert_eq!(d.reason, "classifier_circuit_open");
    assert!(!d.classifier_attempted);
}

#[tokio::test]
async fn pending_record_after_crash_does_not_repeat_external_call() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(5)))
        .expect(1)
        .mount(&server)
        .await;
    let (e, pool) = fixture(&server.uri(), RoutingMode::Enforce);
    let router = ComplexityRouter::default();
    let mut c = e.complexity_routing_config();
    c.classifier.as_mut().unwrap().timeout_ms = 5000;
    e.publish_complexity_routing(
        ComplexityRoutingUpdate {
            expected_revision: c.revision,
            reason: "slow test".into(),
            classifier: c.classifier,
            policies: c.policies,
        },
        86401,
    )
    .unwrap();
    assert!(tokio::time::timeout(
        Duration::from_millis(100),
        decide(&router, &e, &pool, request("crash"))
    )
    .await
    .is_err());
    let restored = BillingEngine::new();
    restored.import_snapshot(e.export_snapshot());
    let d = decide(&router, &restored, &pool, request("crash")).await;
    assert!(d.pending);
    assert_eq!(d.provider_ids, vec!["reliable"]);
    assert_eq!(restored.routing_status().budget.calls, 1);
}

#[test]
fn capability_filtered_candidates_cannot_be_reintroduced() {
    let d:billing::engine::RoutingDecision=serde_json::from_value(serde_json::json!({
        "invocation_id":"test","scope":"scope","request_hash":"hash","revision":"revision","model_map_id":"model",
        "mode":"enforce","complexity":"complex","reason":"test","provider_ids":["not-eligible"],
        "created_at_secs":1,"pending":false,"classifier_attempted":false,"classifier_latency_ms":0,"input_tokens":0,
        "output_tokens":0,"classifier_cost_micro_cny":0,"usage_estimated":false
    })).unwrap();
    assert!(apply_decision(&[("cheap".into(), 1)], &d).is_empty());
}

#[tokio::test]
async fn admin_routes_require_auth_and_preview_uses_saved_policy() {
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use gateway::facade::{
        admin::AdminAuthState,
        complexity_routing_admin::{ComplexityRoutingHandler, ComplexityRoutingPreviewHandler},
        FacadeHandler,
    };
    use std::sync::Arc;
    let (e, _) = fixture("https://example.invalid", RoutingMode::Off);
    let auth = Arc::new(AdminAuthState::new("test-admin"));
    let get = ComplexityRoutingHandler {
        billing: e.clone(),
        auth: auth.clone(),
        publish: false,
    };
    let post = ComplexityRoutingHandler {
        billing: e.clone(),
        auth: auth.clone(),
        publish: true,
    };
    let preview = ComplexityRoutingPreviewHandler {
        billing: e.clone(),
        auth: auth.clone(),
        runtime: None,
    };
    for handler in [&get as &dyn FacadeHandler, &post, &preview] {
        assert_eq!(
            handler.handle(Request::new(Body::empty())).await.status(),
            StatusCode::UNAUTHORIZED
        );
    }
    let r = Request::builder()
        .method("POST")
        .header("x-admin-key", "test-admin")
        .body(Body::from(r#"{"model_map_id":"mapping","text":"hello"}"#))
        .unwrap();
    let result = preview.handle(r).await;
    assert_eq!(result.status(), StatusCode::OK);
    let value: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(result.into_body(), 100000)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(value["decision"]["reason"], "disabled");
    assert_eq!(value["decision"]["preview"], true);
    assert_eq!(e.routing_status().budget.calls, 0);
}

#[test]
fn classifier_input_excludes_system_tools_editor_and_reasoning_payloads() {
    let req:kiro_wire::requests::conversation::GenerateAssistantResponseRequest=serde_json::from_value(serde_json::json!({
        "systemPrompt":"PRIVATE_SYSTEM", "profileArn":"PRIVATE_PROFILE",
        "conversationState": {"conversationId":"id","currentMessage":{"userInputMessage":{
            "content":"继续", "userInputMessageContext":{
                "editorState":{"file":"PRIVATE_EDITOR"},
                "tools":[{"name":"private_tool","description":"PRIVATE_SCHEMA","inputSchema":{}}],
                "toolResults":[{"toolUseId":"tool-1","content":[{"text":"PRIVATE_RESULT"}]}]
            },
            "images":[{"format":"png","source":{"bytes":"PRIVATE_IMAGE"}}],
            "documents":[{"format":"pdf","name":"private_document","source":{"bytes":"PRIVATE_DOCUMENT"}}]
        }},"history":[{"assistantResponseMessage":{"content":"visible recent answer",
            "reasoningContent":{"reasoningText":{"text":"PRIVATE_REASONING","signature":"PRIVATE_SIGNATURE"}},
            "toolUses":[{"toolUseId":"tool-1","name":"PRIVATE_TOOL","input":{"secret":"PRIVATE_ARGS"}}]
        }}]}
    })).unwrap();
    let input = RoutingInput::from_kiro(&req, false);
    assert!(input.continuation);
    assert!(input.has_attachments);
    let serialized = serde_json::to_string(&input).unwrap();
    assert!(serialized.contains("visible recent answer"));
    assert!(!serialized.contains("PRIVATE_"));
    assert!(!serialized.contains("private_"));
}

#[test]
fn overlong_history_is_bounded_and_cannot_claim_complete_context() {
    let history:Vec<_>=(0..8).map(|i|serde_json::json!({"userInputMessage":{"content":format!("item-{i} {}", "x".repeat(5000))}})).collect();
    let req=serde_json::from_value(serde_json::json!({"conversationState":{
        "conversationId":"id", "currentMessage":{"userInputMessage":{"content":"hello"}},"history":history
    }})).unwrap();
    let input = RoutingInput::from_kiro(&req, false);
    assert!(!input.context_complete);
    assert!(input.input_chars > 40000);
    assert!(input.history.chars().count() < 16500);
    assert!(!input.history.contains("item-0"));
    assert!(input.history.contains("item-7"));
}

#[tokio::test]
async fn anthropic_classifier_uses_messages_and_reconciles_usage() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "stop_reason":"end_turn","content":[{"type":"text","text":serde_json::json!({
                "complexity":"simple","task_type":"translation","context_sufficient":true,"reason_codes":["SHORT_STANDALONE"]
            }).to_string()}],"usage":{"input_tokens":100,"output_tokens":20}
        }))).expect(1).mount(&server).await;
    let (e, _) = fixture(&server.uri(), RoutingMode::Enforce);
    let p = Provider::new(
        "classifier",
        "Classifier",
        ProviderFormat::Anthropic,
        server.uri(),
    );
    e.upsert_provider(p.clone());
    let pool = ProviderKeyPool::new(
        p,
        vec![ProviderKey::new(
            "classifier-key",
            "classifier",
            "fixture-secret",
        )],
    );
    let d = decide(
        &ComplexityRouter::default(),
        &e,
        &pool,
        request("anthropic"),
    )
    .await;
    assert_eq!(d.complexity, Complexity::Simple);
    assert_eq!(d.classifier_cost_micro_cny, 140);
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests[0].headers["anthropic-version"], "2023-06-01");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_requests_claim_a_single_durable_classifier_call() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_millis(50))
                .set_body_json(completion("simple", "general")),
        )
        .expect(1)
        .mount(&server)
        .await;
    let (e, pool) = fixture(&server.uri(), RoutingMode::Enforce);
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(16));
    let mut jobs = Vec::new();
    for _ in 0..16 {
        let (e, pool, barrier) = (e.clone(), pool.clone(), barrier.clone());
        jobs.push(tokio::spawn(async move {
            let router = ComplexityRouter::default();
            barrier.wait().await;
            decide(&router, &e, &pool, request("concurrent")).await
        }));
    }
    for job in jobs {
        job.await.unwrap();
    }
    let d = e.routing_decision("concurrent").unwrap();
    assert!(!d.pending);
    assert_eq!(d.complexity, Complexity::Simple);
    assert_eq!(e.routing_status().budget.calls, 1);
}

#[tokio::test]
async fn capacity_exhaustion_keeps_answer_route_without_calling_classifier() {
    for mode in [RoutingMode::Observe, RoutingMode::Enforce] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;
        let (e, pool) = fixture(&server.uri(), mode);
        let router = ComplexityRouter::default();
        let mut seed = request("capacity-seed");
        seed.input.high_reasoning = true;
        let original = decide(&router, &e, &pool, seed).await;
        let mut snapshot = e.export_snapshot();
        for i in 1..20_000 {
            let mut retained = original.clone();
            retained.invocation_id = format!("retained-{i}");
            snapshot.complexity_routing.decisions.push(retained);
        }
        e.import_snapshot(snapshot);
        let budget = e.routing_status().budget;
        let fallback = decide(&router, &e, &pool, request("capacity-new")).await;
        assert_eq!(fallback.reason, "decision_capacity_exhausted");
        assert_eq!(fallback.provider_ids, ["reliable"]);
        assert!(!fallback.pending && !fallback.classifier_attempted);
        assert_eq!(fallback.classifier_cost_micro_cny, 0);
        assert!(e.routing_decision("capacity-new").is_none());
        assert_eq!(e.routing_status().budget, budget);
        assert_eq!(e.routing_decision("capacity-seed"), Some(original));
        let candidates = vec![("cheap".into(), 1), ("reliable".into(), 2)];
        let applied = apply_decision(&candidates, &fallback);
        assert_eq!(
            applied,
            if mode == RoutingMode::Observe {
                candidates
            } else {
                vec![("reliable".into(), 2)]
            }
        );
        assert!(server.received_requests().await.unwrap().is_empty());
    }
}

#[tokio::test]
async fn simple_and_continuation_routes_are_narrowed_to_eligible_channels() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(completion("simple", "general")))
        .expect(1)
        .mount(&server)
        .await;
    let (e, pool) = fixture(&server.uri(), RoutingMode::Enforce);
    let router = ComplexityRouter::default();
    let original = decide(&router, &e, &pool, request("before-disable")).await;
    assert_eq!(original.provider_ids, ["cheap", "reliable"]);
    e.set_provider_enabled("cheap", false).unwrap();
    let mut continuation = request("after-disable");
    continuation.eligible_provider_ids = vec!["reliable".into()];
    continuation.input.continuation = true;
    let inherited = decide(&router, &e, &pool, continuation).await;
    assert_eq!(inherited.provider_ids, ["reliable"]);
    assert_eq!(inherited.reason, "task_continuation");
    assert!(!inherited.classifier_attempted);
}
