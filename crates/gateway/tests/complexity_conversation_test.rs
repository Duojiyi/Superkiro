//! Real conversation pipeline: routing must not bypass capabilities or user settlement.
//! All classifier and answer requests terminate at this test's loopback wiremock server.
use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
    Router,
};
use billing::{
    card::Card,
    engine::{
        Complexity, ComplexityRoutingUpdate, ResponseTemplateRule, ResponseTemplateUpdate,
        ResponseTemplateVariant, RoutingClassifier, RoutingDecision, RoutingMode, RoutingPolicy,
    },
    group::{Group, ModelMap},
    provider::{Provider, ProviderFormat, ProviderKey},
    BillingEngine, LedgerKind,
};
use gateway::{
    auth::AuthClaims,
    facade::{conversation::GenerateAssistantResponseHandler, FacadeRegistry},
    provider::{install_provider_options, ProviderOptionsTable, ProviderRuntimeRegistry},
};
use serde_json::{json, Value};
use std::time::Duration;
use tower::ServiceExt;
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

mod support;

const CARD: &str = "complexity-card";
const GROUP: &str = "complexity-group";
const MODEL: &str = "assistant-model";
const MAP: &str = "assistant-map";
const CHEAP: &str = "cheap";
const STRONG: &str = "strong";
const CLASSIFIER: &str = "classifier";
const BALANCE: i64 = 10_000_000;
// The published user price is 15/60 microcredits per input/output token.
const ANSWER_FEE: i64 = 1_000 * 15 + 100 * 60;
const TASK: &str = "Design a multi-service database migration with rollback and failure recovery.";

fn sse(delta: Value, stop: &str) -> String {
    [
        json!({"id":"answer", "choices":[{"index":0, "delta":delta}]}),
        json!({"id":"answer", "choices":[{"index":0, "delta":{}, "finish_reason":stop}]}),
        json!({"id":"answer", "choices":[], "usage":{
            "prompt_tokens":1000, "completion_tokens":100, "total_tokens":1100
        }}),
    ]
    .iter()
    .map(|event| format!("data: {event}\n\n"))
    .collect::<String>()
        + "data: [DONE]\n\n"
}

fn body(content: &str) -> Value {
    json!({"conversationState":{
        "conversationId":"complexity-conversation",
        "currentMessage":{"userInputMessage":{
            "content":content, "modelId":MODEL, "userInputMessageContext":{}
        }}
    }})
}

struct Fixture {
    server: MockServer,
    billing: BillingEngine,
    app: Router,
    runtime: ProviderRuntimeRegistry,
}

impl Fixture {
    async fn new(mode: RoutingMode) -> Self {
        // This binary owns the process-wide table. Every fixture installs the same table
        // before constructing a handler, including when cargo runs the tests in parallel.
        install_provider_options(ProviderOptionsTable::default().with_no_documents(&[CHEAP]));
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/classifier/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices":[{"finish_reason":"stop", "message":{"content":json!({
                    "complexity":"complex", "task_type":"architecture",
                    "context_sufficient":true, "reason_codes":["MULTI_STEP"]
                }).to_string()}}],
                "usage":{"prompt_tokens":400, "completion_tokens":20}
            })))
            .mount(&server)
            .await;
        for provider in [CHEAP, STRONG] {
            Mock::given(method("POST"))
                .and(path(format!("/{provider}/v1/chat/completions")))
                .respond_with(ResponseTemplate::new(200).set_body_raw(
                    sse(json!({"content":format!("answer from {provider}")}), "stop"),
                    "text/event-stream",
                ))
                .mount(&server)
                .await;
        }

        let billing = BillingEngine::new();
        billing.upsert_group(Group::pro_plus(GROUP, "Complexity integration"));
        let mut card = Card::new(CARD, GROUP, BALANCE);
        card.activate(gateway::now_secs(), 86_400).unwrap();
        billing.upsert_card(card);
        billing.upsert_rate_card_version(support::wildcard_price("default"));
        for provider in [CHEAP, STRONG, CLASSIFIER] {
            billing.upsert_provider(Provider::new(
                provider,
                provider,
                ProviderFormat::OpenAi,
                format!("{}/{provider}", server.uri()),
            ));
            billing.upsert_provider_key(ProviderKey::new(
                format!("key-{provider}"),
                provider,
                "local-fixture-only",
            ));
        }
        let mut model = ModelMap::new(MAP, GROUP, MODEL, CHEAP, "cheap-answer")
            .with_fallback(STRONG, "strong-answer");
        model.supports_reasoning = true;
        billing.upsert_model_map(model);
        billing
            .publish_complexity_routing(
                ComplexityRoutingUpdate {
                    expected_revision: billing.complexity_routing_config().revision,
                    reason: "Isolated conversation integration test".into(),
                    classifier: Some(RoutingClassifier {
                        provider_id: CLASSIFIER.into(),
                        model: "classifier-model".into(),
                        timeout_ms: 5000,
                        max_input_chars: 4096,
                        daily_request_limit: 100,
                        daily_budget_micro_cny: 1_000_000,
                        input_price_micro_cny_per_million: 1_000_000,
                        output_price_micro_cny_per_million: 2_000_000,
                    }),
                    policies: vec![RoutingPolicy {
                        model_map_id: MAP.into(),
                        mode,
                        simple_provider_ids: vec![CHEAP.into()],
                        complex_provider_ids: vec![STRONG.into()],
                    }],
                },
                gateway::now_secs(),
            )
            .unwrap();

        let runtime = ProviderRuntimeRegistry::new();
        runtime.sync_from_billing(&billing);
        let mut registry = FacadeRegistry::new().with_runtime(runtime.clone());
        registry.register(GenerateAssistantResponseHandler {
            billing: billing.clone(),
            runtime: Some(runtime.clone()),
            ..Default::default()
        });
        Self {
            server,
            billing,
            app: registry.into_router(),
            runtime,
        }
    }

    async fn send(&self, invocation: &str, body: Value) -> (StatusCode, Vec<Value>) {
        let mut request = Request::builder()
            .method(Method::POST)
            .uri("/generateAssistantResponse")
            .header("content-type", "application/json")
            .header("amz-sdk-invocation-id", invocation)
            .body(Body::from(body.to_string()))
            .unwrap();
        request.extensions_mut().insert(AuthClaims {
            card_id: CARD.into(),
            group_id: GROUP.into(),
            token_version: 1,
            exp: 9_999_999_999,
            iat: 1_000_000_000,
        });
        tokio::time::timeout(Duration::from_secs(15), async {
            let response = self.app.clone().oneshot(request).await.unwrap();
            let status = response.status();
            let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
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
        })
        .await
        .expect("local conversation must complete within 15 seconds")
    }

    fn decision(&self, invocation: &str) -> RoutingDecision {
        let decision = self
            .billing
            .routing_decision(&format!("{CARD}:{invocation}"))
            .unwrap();
        assert!(!decision.pending);
        assert!(!decision.preview);
        decision
    }

    async fn assert_calls(&self, classifier: usize, cheap: usize, strong: usize) {
        let requests = self.server.received_requests().await.unwrap();
        assert_eq!(
            requests.len(),
            classifier + cheap + strong,
            "unexpected endpoint call"
        );
        for (provider, model, count) in [
            (CLASSIFIER, "classifier-model", classifier),
            (CHEAP, "cheap-answer", cheap),
            (STRONG, "strong-answer", strong),
        ] {
            let calls: Vec<_> = requests
                .iter()
                .filter(|r| r.url.path() == format!("/{provider}/v1/chat/completions"))
                .collect();
            assert_eq!(calls.len(), count, "calls to {provider}");
            for call in calls {
                let value: Value = serde_json::from_slice(&call.body).unwrap();
                assert_eq!(value["model"], model);
                assert_eq!(value["stream"], provider != CLASSIFIER);
            }
        }
    }

    async fn assert_answer_bills(&self, invocations: &[&str], provider: &str) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while self.billing.ledger_entries().len() < invocations.len() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("answer settlement must finish");
        let entries = self.billing.ledger_entries();
        // In particular there is no user ledger entry for classifier consumption.
        assert_eq!(entries.len(), invocations.len());
        for invocation in invocations {
            let key = format!("{CARD}:{invocation}");
            let entry = entries
                .iter()
                .find(|e| e.invocation_id.as_deref() == Some(&key))
                .unwrap();
            assert_eq!(entry.kind, LedgerKind::Usage);
            assert_eq!(entry.card_id, CARD);
            assert_eq!(entry.exposed_model, MODEL);
            assert_eq!(entry.provider_id, provider);
            assert_eq!(entry.target_model, format!("{provider}-answer"));
            assert_eq!(
                entry.rate_card_version.as_deref(),
                Some("test-wildcard-default")
            );
            assert_eq!((entry.input_tokens, entry.output_tokens), (1000, 100));
            assert_eq!(
                (entry.cache_creation_tokens, entry.cache_read_tokens),
                (0, 0)
            );
            assert_eq!(entry.credits_charged, ANSWER_FEE);
        }
        let card = self.billing.get_card(CARD).unwrap();
        assert_eq!(card.credit_reserved, 0);
        assert_eq!(card.credit_used, ANSWER_FEE * invocations.len() as i64);
        assert_eq!(card.available_credits(), BALANCE - card.credit_used);
    }

    fn complex_chain(&self, providers: &[&str]) {
        let config = self.billing.complexity_routing_config();
        let mut policies = config.policies;
        policies[0].complex_provider_ids = providers.iter().map(|id| (*id).into()).collect();
        self.billing
            .publish_complexity_routing(
                ComplexityRoutingUpdate {
                    expected_revision: config.revision,
                    reason: "Test capability intersection".into(),
                    classifier: config.classifier,
                    policies,
                },
                gateway::now_secs(),
            )
            .unwrap();
    }
}

fn assert_answer(status: StatusCode, payloads: &[Value], provider: &str) {
    assert_eq!(status, StatusCode::OK, "{payloads:?}");
    assert!(
        payloads
            .iter()
            .any(|p| p["content"] == format!("answer from {provider}")),
        "{payloads:?}"
    );
    assert_eq!(payloads.last().unwrap()["stopReason"], "end_turn");
}

#[tokio::test]
async fn enforce_selects_strong_channel_and_keeps_classifier_cost_off_user_bill() {
    let f = Fixture::new(RoutingMode::Enforce).await;
    let (status, payloads) = f.send("enforce", body(TASK)).await;
    assert_answer(status, &payloads, STRONG);
    f.assert_calls(1, 0, 1).await;
    let decision = f.decision("enforce");
    assert_eq!(decision.mode, RoutingMode::Enforce);
    assert_eq!(decision.complexity, Complexity::Complex);
    assert_eq!(decision.reason, "semantic_complex");
    assert_eq!(decision.provider_ids, [STRONG]);
    assert_eq!(decision.served_provider_id.as_deref(), Some(STRONG));
    assert!(decision.classifier_attempted);
    assert!(!decision.usage_estimated);
    assert_eq!((decision.input_tokens, decision.output_tokens), (400, 20));
    assert_eq!(decision.classifier_cost_micro_cny, 440);
    let budget = f.billing.routing_status().budget;
    assert_eq!((budget.calls, budget.cost_micro_cny), (1, 440));
    f.assert_answer_bills(&["enforce"], STRONG).await;
}

#[tokio::test]
async fn observe_records_strong_suggestion_but_serves_original_cheap_channel() {
    let f = Fixture::new(RoutingMode::Observe).await;
    let (status, payloads) = f.send("observe", body(TASK)).await;
    assert_answer(status, &payloads, CHEAP);
    f.assert_calls(1, 1, 0).await;
    let decision = f.decision("observe");
    assert_eq!(decision.mode, RoutingMode::Observe);
    assert_eq!(decision.complexity, Complexity::Complex);
    assert_eq!(decision.provider_ids, [STRONG]);
    assert_eq!(decision.served_provider_id.as_deref(), Some(CHEAP));
    assert!(decision.classifier_attempted);
    assert_eq!(decision.classifier_cost_micro_cny, 440);
    f.assert_answer_bills(&["observe"], CHEAP).await;
}

#[tokio::test]
async fn pdf_capability_filter_is_not_bypassed_by_complexity_allowlist() {
    let f = Fixture::new(RoutingMode::Enforce).await;
    f.complex_chain(&[CHEAP, STRONG]);
    let mut request = body("Summarize the attached document.");
    request["conversationState"]["currentMessage"]["userInputMessage"]["documents"] = json!([{
        "name":"fixture", "format":"pdf", "source":{"bytes":base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD, b"%PDF-1.4 <</Type /Page>> fixture"
        )}
    }]);
    let (status, payloads) = f.send("pdf", request.clone()).await;
    assert_answer(status, &payloads, STRONG);
    let decision = f.decision("pdf");
    assert_eq!(decision.provider_ids, [STRONG]);
    assert_eq!(decision.reason, "capability_required");
    assert_eq!(decision.complexity, Complexity::Complex);
    assert!(!decision.classifier_attempted);
    assert_eq!(decision.served_provider_id.as_deref(), Some(STRONG));
    f.assert_answer_bills(&["pdf"], STRONG).await;

    // With only the incapable provider allowed, refuse rather than escaping either filter.
    f.complex_chain(&[CHEAP]);
    let (status, payloads) = f.send("pdf-no-route", request).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{payloads:?}");
    assert_eq!(payloads[0]["__type"], "ValidationException");
    assert_eq!(f.decision("pdf-no-route").served_provider_id, None);
    f.assert_calls(0, 0, 1).await;
    assert_eq!(f.billing.routing_status().budget.calls, 0);
    f.assert_answer_bills(&["pdf"], STRONG).await;
}

#[tokio::test]
async fn xhigh_and_max_effort_skip_classifier_and_use_complex_channel() {
    let f = Fixture::new(RoutingMode::Enforce).await;
    for effort in ["xhigh", "max"] {
        let mut request = body("Hello");
        request["additionalModelRequestFields"] = json!({"output_config":{"effort":effort}});
        let (status, payloads) = f.send(effort, request).await;
        assert_answer(status, &payloads, STRONG);
        let decision = f.decision(effort);
        assert_eq!(decision.reason, "capability_required", "effort={effort}");
        assert_eq!(decision.complexity, Complexity::Complex);
        assert!(!decision.classifier_attempted);
        assert_eq!(decision.classifier_cost_micro_cny, 0);
        assert_eq!(decision.served_provider_id.as_deref(), Some(STRONG));
    }
    f.assert_calls(0, 0, 2).await;
    assert_eq!(f.billing.routing_status().budget.calls, 0);
    f.assert_answer_bills(&["xhigh", "max"], STRONG).await;
}

#[tokio::test]
async fn template_hit_precedes_classifier_and_answer_calls() {
    let f = Fixture::new(RoutingMode::Enforce).await;
    f.billing
        .publish_response_templates(
            ResponseTemplateUpdate {
                expected_revision: f.billing.response_template_config().revision,
                reason: "Test template precedence with enforce enabled".into(),
                rules: vec![ResponseTemplateRule {
                    id: "local-template".into(),
                    name: "Local fixture".into(),
                    enabled: true,
                    match_mode: "exact".into(),
                    intent: None,
                    match_text: TASK.into(),
                    variants: vec![ResponseTemplateVariant {
                        model_id: MODEL.into(),
                        file_path: "index.html".into(),
                        content: "<main>Deterministic local fixture.</main>".into(),
                        delay_ms: 0,
                        delivery: None,
                        content_alternatives: vec![],
                        content_alternative_index: None,
                        preamble: "Prepared template".into(),
                        completion: "Template delivered".into(),
                        price_microcredits: 12_345,
                    }],
                }],
            },
            gateway::now_secs(),
        )
        .unwrap();
    let mut request = body(TASK);
    request["conversationState"]["currentMessage"]["userInputMessage"]["userInputMessageContext"]
        ["tools"] = json!([{
        "toolSpecification":{"name":"fsWrite", "description":"Write a file", "inputSchema":{"json":{
            "type":"object", "properties":{"path":{"type":"string"}, "text":{"type":"string"}},
            "required":["path", "text"]
        }}}
    }]);
    let (status, payloads) = f.send("template", request).await;
    assert_eq!(status, StatusCode::OK, "{payloads:?}");
    let tool = payloads.iter().find(|p| p["name"] == "fsWrite").unwrap();
    let input: Value = serde_json::from_str(tool["input"].as_str().unwrap()).unwrap();
    assert_eq!(
        input,
        json!({"path":"index.html", "text":"<main>Deterministic local fixture.</main>"})
    );
    assert_eq!(payloads.last().unwrap()["stopReason"], "tool_use");
    f.assert_calls(0, 0, 0).await;
    let routing = f.billing.routing_status();
    assert!(routing.decisions.is_empty());
    assert_eq!(
        (routing.budget.calls, routing.budget.cost_micro_cny),
        (0, 0)
    );
    let entries = f.billing.ledger_entries();
    assert_eq!(entries.len(), 1);
    let entry = &entries[0];
    assert_eq!(entry.kind, LedgerKind::Usage);
    assert_eq!(entry.provider_id, "response-template:local-template");
    assert_eq!(entry.credits_charged, 12_345);
    assert_eq!(
        (
            entry.input_tokens,
            entry.output_tokens,
            entry.provider_cost_micro_cny
        ),
        (0, 0, 0)
    );
    let card = f.billing.get_card(CARD).unwrap();
    assert_eq!(card.credit_reserved, 0);
    assert_eq!(card.available_credits(), BALANCE - 12_345);
}

#[tokio::test]
async fn tool_result_continues_served_channel_without_reclassification() {
    let f = Fixture::new(RoutingMode::Enforce).await;
    Mock::given(method("POST"))
        .and(path("/strong/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            sse(
                json!({"tool_calls":[{
                    "index":0, "id":"read-1", "type":"function",
                    "function":{"name":"read_file", "arguments":"{\"path\":\"notes.txt\"}"}
                }]}),
                "tool_calls",
            ),
            "text/event-stream",
        ))
        .with_priority(1)
        .up_to_n_times(1)
        .expect(1)
        .mount(&f.server)
        .await;
    let tools = json!([{"toolSpecification":{
        "name":"read_file", "description":"Read a local file", "inputSchema":{"json":{
            "type":"object", "properties":{"path":{"type":"string"}}, "required":["path"]
        }}
    }}]);
    let mut first = body(TASK);
    first["conversationState"]["currentMessage"]["userInputMessage"]["userInputMessageContext"]
        ["tools"] = tools.clone();
    let (status, payloads) = f.send("tool-first", first.clone()).await;
    assert_eq!(status, StatusCode::OK, "{payloads:?}");
    assert_eq!(payloads.last().unwrap()["stopReason"], "tool_use");
    let tool = payloads.iter().find(|p| p["name"] == "read_file").unwrap();
    assert_eq!(tool["toolUseId"], "read-1");
    let mut next = body("");
    next["conversationState"]["history"] = json!([
        first["conversationState"]["currentMessage"],
        {"assistantResponseMessage":{"content":"", "toolUses":[{
            "toolUseId":tool["toolUseId"], "name":"read_file", "input":{"path":"notes.txt"}
        }]}}
    ]);
    next["conversationState"]["currentMessage"]["userInputMessage"]["userInputMessageContext"] = json!({
        "tools":tools, "toolResults":[{
            "toolUseId":tool["toolUseId"], "status":"success", "content":[{"text":"local file contents"}]
        }]
    });
    let (status, payloads) = f.send("tool-next", next).await;
    assert_answer(status, &payloads, STRONG);
    let first_decision = f.decision("tool-first");
    assert_eq!(first_decision.reason, "semantic_complex");
    let next_decision = f.decision("tool-next");
    assert_eq!(next_decision.reason, "task_continuation");
    assert_eq!(next_decision.scope, first_decision.scope);
    assert_eq!(next_decision.provider_ids, first_decision.provider_ids);
    assert_eq!(next_decision.served_provider_id.as_deref(), Some(STRONG));
    assert!(!next_decision.classifier_attempted);
    assert_eq!(next_decision.classifier_cost_micro_cny, 0);
    f.assert_calls(1, 0, 2).await;
    assert_eq!(f.billing.routing_status().budget.calls, 1);
    f.assert_answer_bills(&["tool-first", "tool-next"], STRONG)
        .await;
    let requests = f.server.received_requests().await.unwrap();
    let last: Value = serde_json::from_slice(&requests.last().unwrap().body).unwrap();
    assert!(last["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m["role"] == "tool"
            && m["tool_call_id"] == "read-1"
            && m["content"] == "local file contents"));
}

#[tokio::test]
async fn disabled_backup_is_removed_before_persistence_in_both_modes() {
    for mode in [RoutingMode::Observe, RoutingMode::Enforce] {
        let f = Fixture::new(mode).await;
        f.complex_chain(&[CHEAP, STRONG]);
        f.billing.set_provider_enabled(CHEAP, false).unwrap();
        f.runtime.sync_from_billing(&f.billing);
        let (status, payloads) = f.send("disabled-backup", body(TASK)).await;
        assert_answer(status, &payloads, STRONG);
        let decision = f.decision("disabled-backup");
        assert_eq!(decision.provider_ids, [STRONG]);
        assert_eq!(decision.served_provider_id.as_deref(), Some(STRONG));
        f.assert_calls(1, 0, 1).await;
        f.assert_answer_bills(&["disabled-backup"], STRONG).await;
    }
}

#[tokio::test]
async fn observe_empty_suggestion_does_not_block_the_original_answer_chain() {
    let f = Fixture::new(RoutingMode::Observe).await;
    f.billing.set_provider_enabled(STRONG, false).unwrap();
    f.runtime.sync_from_billing(&f.billing);
    let (status, payloads) = f.send("observe-empty", body(TASK)).await;
    assert_answer(status, &payloads, CHEAP);
    let decision = f.decision("observe-empty");
    assert!(decision.provider_ids.is_empty());
    assert_eq!(decision.reason, "no_eligible_route");
    assert!(!decision.classifier_attempted);
    assert_eq!(decision.served_provider_id.as_deref(), Some(CHEAP));
    f.assert_calls(0, 1, 0).await;
    f.assert_answer_bills(&["observe-empty"], CHEAP).await;
}
