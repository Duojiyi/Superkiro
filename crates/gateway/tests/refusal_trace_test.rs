//! A request refused before it is routed leaves a trace naming why, and is charged nothing.
//! Such refusals left no trace, so the console could not tell a customer's failed requests
//! from ones never made. A retired model is never listed, and a request for it is refused as
//! for a model its group does not list. A fallback target may be disabled: requests pass it
//! over.

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use billing::card::Card;
use billing::engine::CommercialUpdate;
use billing::group::{Group, ModelMap};
use billing::provider::{Provider, ProviderFormat, ProviderKey};
use billing::rate_card::{Currency, PricingMode, RateCardVersion};
use billing::BillingEngine;
use gateway::auth::AuthClaims;
use gateway::facade::conversation::GenerateAssistantResponseHandler;
use gateway::facade::virtualization::VirtualizationStore;
use gateway::facade::FacadeRegistry;
use gateway::provider::ProviderRuntimeRegistry;
use serde_json::{json, Value};
use tower::ServiceExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const GROUP: &str = "group-refusals";
const CARD: &str = "card-refusals";
/// A 1x1 PNG.
const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

fn price(model: &str) -> RateCardVersion {
    RateCardVersion {
        id: format!("price-{model}"),
        rate_card_id: "default".to_string(),
        model: model.to_string(),
        currency: Currency::Cny,
        pricing_mode: PricingMode::Fixed,
        input_price_per_m: 0.0,
        output_price_per_m: 0.0,
        cache_creation_price_per_m: 0.0,
        cache_read_price_per_m: 0.0,
        fixed_input_credit_per_m: 1_000_000,
        fixed_output_credit_per_m: 1_000_000,
        fixed_cache_creation_credit_per_m: 0,
        fixed_cache_read_credit_per_m: 0,
        per_call_credit: 0,
        margin_multiplier: 1.0,
        effective_from_secs: 0,
    }
}

/// An OpenAI-format upstream answering every request with `status`, and a short answer
/// when that is 200.
async fn upstream(status: u16) -> MockServer {
    let server = MockServer::start().await;
    let answer = [
        json!({"id": "1", "choices": [{"delta": {"content": "Hello"}}]}),
        json!({"id": "1", "choices": [{"delta": {}, "finish_reason": "stop"}]}),
        json!({"id": "1", "choices": [], "usage": {"prompt_tokens": 10, "completion_tokens": 2, "total_tokens": 12}}),
    ]
    .iter()
    .map(|frame| format!("data: {frame}\n\n"))
    .collect::<String>()
        + "data: [DONE]\n\n";
    let response = match status {
        200 => ResponseTemplate::new(200).set_body_raw(answer, "text/event-stream"),
        other => ResponseTemplate::new(other),
    };
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(response)
        .mount(&server)
        .await;
    server
}

/// A funded card in `GROUP`, with the providers, Keys and mappings given, and a price for
/// each model in `priced`.
fn engine(
    providers: Vec<Provider>,
    keys: Vec<ProviderKey>,
    maps: Vec<ModelMap>,
    priced: &[&str],
) -> BillingEngine {
    let billing = BillingEngine::new();
    billing.upsert_group(Group::pro_plus(GROUP, "Refusals"));
    let mut card = Card::new(CARD, GROUP, 100 * billing::MICRO_CREDITS_PER_CREDIT);
    card.activate(gateway::now_secs(), 86_400).unwrap();
    billing.upsert_card(card);
    for provider in providers {
        billing.upsert_provider(provider);
    }
    for key in keys {
        billing.upsert_provider_key(key);
    }
    for map in maps {
        billing.upsert_model_map(map);
    }
    for model in priced {
        billing.upsert_rate_card_version(price(model));
    }
    billing
}

/// The customer routes over `billing`, routing with its saved providers as in production.
fn serve(billing: &BillingEngine) -> (axum::Router, ProviderRuntimeRegistry) {
    let runtime = ProviderRuntimeRegistry::new();
    runtime.sync_from_billing(billing);
    let mut registry = FacadeRegistry::new().with_runtime(runtime.clone());
    registry
        .register_virtualized_facades(VirtualizationStore::with_billing(billing.clone(), GROUP));
    registry.register(GenerateAssistantResponseHandler {
        billing: billing.clone(),
        runtime: Some(runtime.clone()),
        intercept_intent: false,
        ..Default::default()
    });
    (registry.into_router(), runtime)
}

fn claims() -> AuthClaims {
    AuthClaims {
        card_id: CARD.into(),
        group_id: GROUP.into(),
        token_version: 0,
        exp: u64::MAX,
        iat: 0,
    }
}

/// One conversation turn naming `model`, or none, edited by `edit`.
async fn turn(
    app: &axum::Router,
    invocation: &str,
    model: Option<&str>,
    edit: fn(&mut Value),
) -> (StatusCode, String) {
    let mut message = json!({"content": "hello"});
    if let Some(model) = model {
        message["modelId"] = json!(model);
    }
    let mut body = json!({"conversationState": {
        "conversationId": "conv-refusals",
        "currentMessage": {"userInputMessage": message}
    }});
    edit(&mut body);
    let mut request = Request::builder()
        .method(Method::POST)
        .uri("/generateAssistantResponse")
        .header(header::CONTENT_TYPE, "application/json")
        .header("amz-sdk-invocation-id", invocation)
        .body(Body::from(body.to_string()))
        .unwrap();
    request.extensions_mut().insert(claims());
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

/// A refused request: its invocation, the model it names, what else it asks for, and the
/// status it is refused with and the error class and model its trace records.
type Refusal = (
    &'static str,
    &'static str,
    fn(&mut Value),
    StatusCode,
    &'static str,
    &'static str,
);

fn plain(_: &mut Value) {}

fn with_reasoning(body: &mut Value) {
    body["additionalModelRequestFields"] = json!({"output_config": {"effort": "high"}});
}

fn with_image(body: &mut Value) {
    body["conversationState"]["currentMessage"]["userInputMessage"]["images"] =
        json!([{"format": "png", "source": {"bytes": PNG}}]);
}

/// The upstream models `server` was asked for, in order.
async fn models_sent(server: &MockServer) -> Vec<String> {
    server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .map(|request| {
            serde_json::from_slice::<Value>(&request.body).unwrap()["model"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

#[tokio::test]
async fn each_refusal_before_routing_is_traced_and_charged_nothing() {
    let upstream = upstream(200).await;
    let mut offline = Provider::new("prov-off", "Off", ProviderFormat::OpenAi, upstream.uri());
    offline.enabled = false;
    let mut restricted = ProviderKey::new("key-restricted", "prov-keyless", "sk-restricted");
    restricted.allowed_models = Some(vec!["another-model".into()]);
    let mut text_only = ModelMap::new("map-listed", GROUP, "listed-model", "prov", "up-listed");
    text_only.supports_vision = false;
    let mut retired = ModelMap::new("map-retired", GROUP, "retired-model", "prov", "up-retired")
        .with_alias("retired-alias");
    retired.retired = true;
    let billing = engine(
        vec![
            Provider::new("prov", "Upstream", ProviderFormat::OpenAi, upstream.uri()),
            offline,
            Provider::new(
                "prov-keyless",
                "Keyless",
                ProviderFormat::OpenAi,
                upstream.uri(),
            ),
        ],
        vec![ProviderKey::new("key", "prov", "sk-test"), restricted],
        vec![
            text_only,
            retired,
            ModelMap::new(
                "map-unpriced",
                GROUP,
                "unpriced-model",
                "prov",
                "up-unpriced",
            ),
            ModelMap::new(
                "map-offline",
                GROUP,
                "offline-model",
                "prov-off",
                "up-offline",
            ),
            ModelMap::new(
                "map-keyless",
                GROUP,
                "keyless-model",
                "prov-keyless",
                "up-keyless",
            ),
        ],
        // A retired model keeps its price: it is refused for being retired.
        &[
            "listed-model",
            "retired-model",
            "absent-model",
            "offline-model",
            "keyless-model",
        ],
    );
    let (app, _) = serve(&billing);

    let cases: [Refusal; 9] = [
        // An invalid ID is not kept: it is whatever the client sent.
        (
            "inv-invalid",
            "bad model",
            plain,
            StatusCode::BAD_REQUEST,
            "invalid_model",
            "",
        ),
        (
            "inv-unpriced",
            "unpriced-model",
            plain,
            StatusCode::BAD_REQUEST,
            "no_price",
            "unpriced-model",
        ),
        (
            "inv-absent",
            "absent-model",
            plain,
            StatusCode::BAD_REQUEST,
            "model_not_listed",
            "absent-model",
        ),
        (
            "inv-retired",
            "retired-model",
            plain,
            StatusCode::BAD_REQUEST,
            "model_retired",
            "retired-model",
        ),
        (
            "inv-alias",
            "retired-alias",
            plain,
            StatusCode::BAD_REQUEST,
            "model_retired",
            "retired-alias",
        ),
        (
            "inv-reasoning",
            "listed-model",
            with_reasoning,
            StatusCode::BAD_REQUEST,
            "unsupported_capability",
            "listed-model",
        ),
        (
            "inv-image",
            "listed-model",
            with_image,
            StatusCode::BAD_REQUEST,
            "unsupported_capability",
            "listed-model",
        ),
        // The target's provider is disabled.
        (
            "inv-offline",
            "offline-model",
            plain,
            StatusCode::BAD_GATEWAY,
            "no_route",
            "offline-model",
        ),
        // No Key may call the target.
        (
            "inv-keyless",
            "keyless-model",
            plain,
            StatusCode::BAD_GATEWAY,
            "no_route",
            "keyless-model",
        ),
    ];
    let mut bodies = std::collections::HashMap::new();
    for (invocation, model, edit, status, _, _) in cases {
        let (got, body) = turn(&app, invocation, Some(model), edit).await;
        assert_eq!(got, status, "{invocation}: {body}");
        bodies.insert(invocation, body);
    }
    // A retired model is refused as one the group does not list.
    assert_eq!(bodies["inv-retired"], bodies["inv-absent"]);
    assert_eq!(bodies["inv-alias"], bodies["inv-absent"]);

    let traces = billing.list_traces(Some(CARD), 100);
    for (invocation, _, _, _, class, exposed) in cases {
        let invocation_id = format!("{CARD}:{invocation}");
        let trace: Vec<_> = traces
            .iter()
            .filter(|trace| trace.invocation_id == invocation_id)
            .collect();
        assert_eq!(trace.len(), 1, "{invocation}");
        let trace = trace[0];
        assert_eq!(trace.status, billing::TraceStatus::Error, "{invocation}");
        assert_eq!(trace.error_class.as_deref(), Some(class), "{invocation}");
        assert_eq!(trace.exposed_model, exposed, "{invocation}");
        assert_eq!(trace.provider_id, None, "{invocation}");
        assert!(trace.attempt_chain.is_empty(), "{invocation}");
        assert_eq!(
            (
                trace.credits_charged,
                trace.input_tokens,
                trace.output_tokens
            ),
            (0, 0, 0),
            "{invocation}"
        );
    }
    // Nothing reached the upstream, nothing is held and nothing was charged.
    assert!(models_sent(&upstream).await.is_empty());
    let card = billing.get_card(CARD).unwrap();
    assert_eq!((card.credit_reserved, card.credit_used), (0, 0));
    assert!(billing
        .ledger_entries()
        .iter()
        .all(|entry| entry.kind != billing::LedgerKind::Usage));

    // The same gateway serves a listed model.
    let (status, body) = turn(&app, "inv-served", Some("listed-model"), plain).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(models_sent(&upstream).await, ["up-listed"]);
}

#[tokio::test]
async fn a_retired_model_is_never_listed_nor_the_default() {
    let upstream = upstream(200).await;
    let mut retired = ModelMap::new("map-retired", GROUP, "retired-model", "prov", "up-retired");
    retired.retired = true;
    retired.sort_order = -1;
    let mut hidden = ModelMap::new("map-hidden", GROUP, "hidden-model", "prov", "up-hidden");
    hidden.visible = false;
    let billing = engine(
        vec![Provider::new(
            "prov",
            "Upstream",
            ProviderFormat::OpenAi,
            upstream.uri(),
        )],
        vec![ProviderKey::new("key", "prov", "sk-test")],
        vec![
            retired,
            hidden,
            ModelMap::new("map-listed", GROUP, "listed-model", "prov", "up-listed"),
        ],
        &["retired-model", "hidden-model", "listed-model"],
    );
    let (app, _) = serve(&billing);

    let mut request = Request::builder()
        .uri("/ListAvailableModels")
        .body(Body::empty())
        .unwrap();
    request.extensions_mut().insert(claims());
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let list: Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    let listed: Vec<_> = list["models"]
        .as_array()
        .unwrap()
        .iter()
        .map(|model| model["modelId"].as_str().unwrap())
        .collect();
    assert_eq!(listed, ["listed-model"]);
    assert_eq!(list["defaultModel"], "listed-model");

    // A request naming no model is for the default; a hidden model may still be named.
    for (invocation, model) in [("inv-default", None), ("inv-hidden", Some("hidden-model"))] {
        let (status, body) = turn(&app, invocation, model, plain).await;
        assert_eq!(status, StatusCode::OK, "{invocation}: {body}");
    }
    assert_eq!(models_sent(&upstream).await, ["up-listed", "up-hidden"]);
}

/// A fallback target may be disabled, when published and when a request is routed: the
/// request passes it over, and it serves again once enabled.
#[tokio::test]
async fn a_disabled_fallback_is_passed_over() {
    let primary = upstream(429).await;
    let fallback = upstream(200).await;
    let mut spare = Provider::new(
        "prov-spare",
        "Spare",
        ProviderFormat::OpenAi,
        fallback.uri(),
    );
    spare.enabled = false;
    let billing = engine(
        vec![
            Provider::new("prov", "Primary", ProviderFormat::OpenAi, primary.uri()),
            spare,
        ],
        vec![
            ProviderKey::new("key", "prov", "sk-primary"),
            ProviderKey::new("key-spare", "prov-spare", "sk-spare"),
        ],
        vec![],
        &["chained-model"],
    );
    let mapping = ModelMap::new("map-chained", GROUP, "chained-model", "prov", "up-primary")
        .with_fallback("prov-spare", "up-spare");
    billing
        .publish_commercial_config(
            CommercialUpdate {
                expected_revision: billing.commercial_config().revision,
                reason: "a chain with a disabled fallback".into(),
                settings: None,
                groups: vec![],
                models: vec![mapping],
                rate_cards: vec![],
                versions: vec![],
                removed_models: vec![],
                cancelled_versions: vec![],
            },
            gateway::now_secs(),
        )
        .unwrap();
    let (app, runtime) = serve(&billing);

    let (status, body) = turn(&app, "inv-passed-over", Some("chained-model"), plain).await;
    assert_ne!(status, StatusCode::OK, "{body}");
    assert_eq!(models_sent(&primary).await, ["up-primary"]);
    assert!(models_sent(&fallback).await.is_empty());

    billing.set_provider_enabled("prov-spare", true).unwrap();
    runtime.sync_from_billing(&billing);
    let (status, body) = turn(&app, "inv-fallback", Some("chained-model"), plain).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(models_sent(&fallback).await, ["up-spare"]);
}
