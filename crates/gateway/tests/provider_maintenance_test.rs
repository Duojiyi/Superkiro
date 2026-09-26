//! Provider upkeep from the admin console: editing a provider, deleting a provider or a
//! Key, each Key's live health and clearing it, and a test call sent as traffic is.

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use billing::group::{Group, ModelMap};
use billing::provider::{HealthState, Provider, ProviderFormat, ProviderKey};
use billing::{BillingEngine, MasterKek};
use gateway::facade::FacadeRegistry;
use gateway::provider::governance::execute_stream_with_failover;
use gateway::provider::{ChatMessage, ChatRequest, ProviderRuntimeRegistry};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::time::Duration;
use tower::ServiceExt;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const ADMIN_KEY: &str = "test-super-secret-admin-key-32chars!!";

/// The admin routes over `billing`, with the running gateway's provider registry.
fn console(billing: &BillingEngine) -> (axum::Router, ProviderRuntimeRegistry) {
    let runtime = ProviderRuntimeRegistry::new();
    runtime.sync_from_billing(billing);
    let mut registry = FacadeRegistry::new().with_runtime(runtime.clone());
    registry.register_admin_facades(billing.clone(), ADMIN_KEY.to_string());
    (registry.into_router(), runtime)
}

async fn send(app: &axum::Router, request: Request<Body>) -> (StatusCode, Value, String) {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let text = String::from_utf8_lossy(&bytes).into_owned();
    (
        status,
        serde_json::from_str(&text).unwrap_or(Value::Null),
        text,
    )
}

async fn post(app: &axum::Router, route: &str, body: Value) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(Method::POST)
        .uri(format!("/api/v1/admin/{route}"))
        .header("x-admin-key", ADMIN_KEY)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let (status, value, _) = send(app, request).await;
    (status, value)
}

/// The Keys `GET /api/v1/admin/providers` lists, by ID, and the listing as sent.
async fn listed_keys(app: &axum::Router) -> (HashMap<String, Value>, String) {
    let request = Request::builder()
        .uri("/api/v1/admin/providers")
        .header("x-admin-key", ADMIN_KEY)
        .body(Body::empty())
        .unwrap();
    let (status, value, text) = send(app, request).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let keys = value["keys"]
        .as_array()
        .unwrap()
        .iter()
        .map(|key| (key["id"].as_str().unwrap().to_string(), key.clone()))
        .collect();
    (keys, text)
}

fn state_file(name: &str) -> std::path::PathBuf {
    std::env::temp_dir()
        .join(format!(
            "kiro_provider_maintenance_{name}_{}_{}",
            std::process::id(),
            gateway::now_secs()
        ))
        .join("billing_state.json")
}

#[tokio::test]
async fn a_provider_is_edited_without_touching_its_keys() {
    let file = state_file("edit");
    let billing = BillingEngine::new();
    billing.set_master_kek(MasterKek::from_bytes([41; 32]));
    billing.upsert_provider(Provider::new(
        "p",
        "Before",
        ProviderFormat::OpenAi,
        "https://before.example",
    ));
    let mut key = ProviderKey::new("k", "p", "sk-kept").with_weight(3);
    key.allowed_models = Some(vec!["target".into()]);
    billing.upsert_provider_key(key);
    billing.save_to_file(&file).unwrap();
    let (app, runtime) = console(&billing);
    let keys = billing.get_runtime_provider_keys(None);

    let (status, reply) = post(
        &app,
        "providers/update",
        json!({"id": "p", "name": "After", "base_url": "https://after.example/v1", "format": "anthropic"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    let saved = billing.get_provider("p").unwrap();
    assert_eq!(
        (saved.name.as_str(), saved.base_url.as_str(), saved.format),
        (
            "After",
            "https://after.example/v1",
            ProviderFormat::Anthropic
        )
    );
    assert!(saved.enabled);
    assert_eq!(reply["provider"], serde_json::to_value(&saved).unwrap());
    assert_eq!(billing.get_runtime_provider_keys(None), keys);
    let live = runtime.pool_for("p").unwrap();
    assert_eq!(live.provider(), saved);
    assert_eq!(live.list_keys()[0].api_key, "sk-kept");

    // Only what is named changes, and a loopback endpoint may use plain HTTP.
    let (status, _) = post(
        &app,
        "providers/update",
        json!({"id": "p", "base_url": "http://localhost:9000"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let saved = billing.get_provider("p").unwrap();
    assert_eq!(
        (saved.name.as_str(), saved.base_url.as_str()),
        ("After", "http://localhost:9000")
    );
    let restored = BillingEngine::new();
    restored.set_master_kek(MasterKek::from_bytes([41; 32]));
    restored.load_from_file(&file).unwrap();
    assert_eq!(restored.get_provider("p").unwrap(), saved);
    assert_eq!(restored.get_runtime_provider_keys(None), keys);

    for (body, status, message) in [
        (
            json!({"id": "p", "base_url": "http://remote.example"}),
            StatusCode::BAD_REQUEST,
            "provider base_url must use HTTPS except for loopback development endpoints",
        ),
        (
            json!({"id": "p", "base_url": "not a url"}),
            StatusCode::BAD_REQUEST,
            "provider base_url must be a valid URL",
        ),
        (
            json!({"id": "p", "name": " "}),
            StatusCode::BAD_REQUEST,
            "Invalid provider name",
        ),
        (
            json!({"id": "p", "format": "gemini"}),
            StatusCode::BAD_REQUEST,
            "Invalid provider update",
        ),
        (
            json!({"id": "p", "enabled": false}),
            StatusCode::BAD_REQUEST,
            "Invalid provider update",
        ),
        (
            json!({"id": "p"}),
            StatusCode::BAD_REQUEST,
            "Nothing to update",
        ),
        (
            json!({"id": "missing", "name": "x"}),
            StatusCode::NOT_FOUND,
            "Unknown provider",
        ),
    ] {
        let (got, reply) = post(&app, "providers/update", body.clone()).await;
        assert_eq!(got, status, "{body}");
        assert_eq!(reply["error"], message, "{body}");
    }
    let unauthenticated = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/admin/providers/update")
        .body(Body::from(json!({"id": "p", "name": "x"}).to_string()))
        .unwrap();
    assert_eq!(
        send(&app, unauthenticated).await.0,
        StatusCode::UNAUTHORIZED
    );
    // An edit that cannot be saved changes nothing.
    billing.inject_persistence_fault(true);
    let (status, _) = post(&app, "providers/update", json!({"id": "p", "name": "Lost"})).await;
    billing.inject_persistence_fault(false);
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(billing.get_provider("p").unwrap(), saved);
    assert_eq!(runtime.pool_for("p").unwrap().provider(), saved);
    std::fs::remove_dir_all(file.parent().unwrap()).unwrap();
}

#[tokio::test]
async fn providers_and_keys_are_deleted_only_when_nothing_depends_on_them() {
    let file = state_file("delete");
    let billing = BillingEngine::new();
    billing.upsert_group(Group::pro_plus("group", "Group"));
    for id in ["p", "q", "r", "s", "t", "u"] {
        billing.upsert_provider(Provider::new(
            id,
            id,
            ProviderFormat::OpenAi,
            "https://example.com",
        ));
    }
    let mut elsewhere = ProviderKey::new("k-elsewhere", "p", "sk-2");
    elsewhere.allowed_models = Some(vec!["other-target".into()]);
    let mut off = ProviderKey::new("k-off", "p", "sk-3");
    off.enabled = false;
    for key in [
        ProviderKey::new("k-serving", "p", "sk-1"),
        elsewhere,
        off,
        ProviderKey::new("k-s", "s", "sk-4"),
    ] {
        billing.upsert_provider_key(key);
    }
    let listed = ModelMap::new("map-listed", "group", "listed-model", "p", "target");
    let mut hidden = ModelMap::new("map-hidden", "group", "hidden-model", "p", "target");
    hidden.visible = false;
    let mut retired = ModelMap::new("map-retired", "group", "retired-model", "r", "target")
        .with_fallback("q", "target");
    retired.retired = true;
    for map in [listed.clone(), hidden, retired] {
        billing.upsert_model_map(map);
    }
    billing.save_to_file(&file).unwrap();
    let (app, runtime) = console(&billing);

    // The last Key allowed to call the target of a model customers see stays.
    let (status, reply) = post(
        &app,
        "providers/keys/delete",
        json!({"provider_id": "p", "key_id": "k-serving"}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        reply["error"],
        "Key still serves visible models: listed-model"
    );
    // A Key not allowed that target, or disabled, serves none of them.
    for key in ["k-elsewhere", "k-off"] {
        let (status, reply) = post(
            &app,
            "providers/keys/delete",
            json!({"provider_id": "p", "key_id": key}),
        )
        .await;
        assert_eq!((status, reply), (StatusCode::OK, json!({"success": true})));
    }
    let live: Vec<_> = runtime
        .pool_for("p")
        .unwrap()
        .list_keys()
        .into_iter()
        .map(|key| key.id)
        .collect();
    assert_eq!(live, ["k-serving"]);
    // Once the model is hidden, its Key may go.
    let mut withdrawn = listed;
    withdrawn.visible = false;
    billing.upsert_model_map(withdrawn);
    let (status, _) = post(
        &app,
        "providers/keys/delete",
        json!({"provider_id": "p", "key_id": "k-serving"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(billing.list_provider_keys(Some("p")).is_empty());
    assert!(runtime.pool_for("p").unwrap().list_keys().is_empty());

    // A provider goes only when no mapping, in any state, names it, and it has no Keys.
    for (id, message) in [
        (
            "p",
            "Provider still routes models: hidden-model, listed-model",
        ),
        ("r", "Provider still routes models: retired-model"),
        ("q", "Provider still routes models: retired-model"),
        ("s", "Provider still has Keys: k-s"),
    ] {
        let (status, reply) = post(&app, "providers/delete", json!({"id": id})).await;
        assert_eq!(status, StatusCode::CONFLICT, "{id}");
        assert_eq!(reply["error"], message, "{id}");
        assert!(billing.get_provider(id).is_some() && runtime.pool_for(id).is_some());
    }
    let (status, reply) = post(&app, "providers/delete", json!({"id": "t"})).await;
    assert_eq!((status, reply), (StatusCode::OK, json!({"success": true})));
    assert!(billing.get_provider("t").is_none());
    assert!(runtime.pool_for("t").is_none());

    for (route, body, message) in [
        ("providers/delete", json!({"id": "t"}), "Unknown provider"),
        (
            "providers/keys/delete",
            json!({"provider_id": "t", "key_id": "k-s"}),
            "Unknown provider",
        ),
        (
            "providers/keys/delete",
            json!({"provider_id": "p", "key_id": "k-s"}),
            "Unknown key",
        ),
    ] {
        let (status, reply) = post(&app, route, body).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{route}");
        assert_eq!(reply["error"], message, "{route}");
    }
    let restored = BillingEngine::new();
    restored.load_from_file(&file).unwrap();
    let ids = |keys: Vec<ProviderKey>| keys.into_iter().map(|key| key.id).collect::<Vec<_>>();
    assert_eq!(ids(restored.list_provider_keys(None)), ["k-s"]);
    assert!(restored.get_provider("t").is_none() && restored.get_provider("u").is_some());

    // A deletion that cannot be saved deletes nothing.
    billing.inject_persistence_fault(true);
    let (status, _) = post(&app, "providers/delete", json!({"id": "u"})).await;
    billing.inject_persistence_fault(false);
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(billing.get_provider("u").is_some() && runtime.pool_for("u").is_some());
    std::fs::remove_dir_all(file.parent().unwrap()).unwrap();
}

fn ping(model: &str) -> ChatRequest {
    ChatRequest {
        reasoning_effort: None,
        model: model.to_string(),
        messages: vec![ChatMessage::new("user", json!("hello"))],
        temperature: None,
        max_tokens: Some(16),
        stream: true,
        tools: vec![],
    }
}

#[tokio::test]
async fn the_provider_list_reports_live_key_health_and_a_reset_clears_it() {
    let upstream = MockServer::start().await;
    for (secret, status) in [("sk-limited", 429), ("sk-revoked", 401)] {
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(header("authorization", format!("Bearer {secret}").as_str()))
            .respond_with(
                ResponseTemplate::new(status).set_body_string(format!("refused {secret}")),
            )
            .mount(&upstream)
            .await;
    }
    let billing = BillingEngine::new();
    for (provider, key, secret) in [
        ("p-limited", "k-limited", "sk-limited"),
        ("p-revoked", "k-revoked", "sk-revoked"),
        ("p-idle", "k-idle", "sk-idle"),
    ] {
        billing.upsert_provider(Provider::new(
            provider,
            provider,
            ProviderFormat::OpenAi,
            upstream.uri(),
        ));
        billing.upsert_provider_key(ProviderKey::new(key, provider, secret));
    }
    let (app, runtime) = console(&billing);
    let client = reqwest::Client::new();
    let before = gateway::now_secs();
    // Real traffic: a rate-limited Key cools down, a rejected one is retired.
    for provider in ["p-limited", "p-revoked"] {
        let pool = runtime.pool_for(provider).unwrap();
        let outcome = execute_stream_with_failover(
            &pool,
            &client,
            "model",
            &ping("model"),
            Duration::from_secs(60),
            1,
            before,
        )
        .await;
        assert!(outcome.is_err(), "{provider}");
    }

    let (keys, text) = listed_keys(&app).await;
    let limited = &keys["k-limited"];
    assert_eq!(limited["health_state"], "cooldown");
    let until = limited["cooldown_until"].as_u64().unwrap();
    assert!(until >= before + 60 && until <= gateway::now_secs() + 60);
    assert_eq!(limited["last_error"], "http_429");
    assert!(limited["last_error_at"].as_u64().unwrap() >= before);
    let revoked = &keys["k-revoked"];
    assert_eq!(revoked["health_state"], "unhealthy");
    assert_eq!(revoked["cooldown_until"], Value::Null);
    assert_eq!(revoked["last_error"], "http_401");
    let idle = &keys["k-idle"];
    assert_eq!(idle["health_state"], "healthy");
    for field in ["cooldown_until", "last_error", "last_error_at"] {
        assert_eq!(idle[field], Value::Null, "{field}");
    }
    // The saved Keys are as they were, and no secret or upstream reply is listed.
    assert_eq!(limited["enabled"], true);
    assert!(billing
        .list_provider_keys(None)
        .iter()
        .all(|key| key.health_state == HealthState::Healthy && key.cooldown_until.is_none()));
    assert!(!text.contains("sk-") && !text.contains("refused"), "{text}");
    // A cooldown that has run out leaves the Key degraded until it next succeeds.
    runtime.pool_for("p-idle").unwrap().mark_key_failure(
        "k-idle",
        before - 120,
        Duration::from_secs(60),
    );
    assert_eq!(
        listed_keys(&app).await.0["k-idle"]["health_state"],
        "degraded"
    );

    for (provider, key) in [("p-limited", "k-limited"), ("p-revoked", "k-revoked")] {
        let (status, reply) = post(
            &app,
            "providers/keys/reset",
            json!({"provider_id": provider, "key_id": key}),
        )
        .await;
        assert_eq!((status, reply), (StatusCode::OK, json!({"success": true})));
        // A retired Key serves again, not only a cooled one.
        let pool = runtime.pool_for(provider).unwrap();
        assert!(pool.select_key(gateway::now_secs(), &[]).is_ok(), "{key}");
    }
    let (keys, _) = listed_keys(&app).await;
    for key in ["k-limited", "k-revoked"] {
        assert_eq!(keys[key]["health_state"], "healthy", "{key}");
        assert_eq!(keys[key]["cooldown_until"], Value::Null, "{key}");
    }
    // The last failure stays on record.
    assert_eq!(keys["k-revoked"]["last_error"], "http_401");

    // A reset clears health, not a Key saved disabled.
    let mut disabled = ProviderKey::new("k-limited", "p-limited", "sk-limited");
    disabled.enabled = false;
    billing.upsert_provider_key(disabled);
    let (status, _) = post(
        &app,
        "providers/keys/reset",
        json!({"provider_id": "p-limited", "key_id": "k-limited"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let pool = runtime.pool_for("p-limited").unwrap();
    assert!(pool.select_key(gateway::now_secs(), &[]).is_err());

    for (body, message) in [
        (
            json!({"provider_id": "p-missing", "key_id": "k-limited"}),
            "Unknown provider",
        ),
        (
            json!({"provider_id": "p-limited", "key_id": "k-revoked"}),
            "Unknown key",
        ),
    ] {
        let (status, reply) = post(&app, "providers/keys/reset", body).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(reply["error"], message);
    }
}

fn openai_answer(text: &str) -> String {
    [
        json!({"id": "1", "choices": [{"delta": {"content": text}}]}),
        json!({"id": "1", "choices": [{"delta": {}, "finish_reason": "stop"}]}),
    ]
    .iter()
    .map(|frame| format!("data: {frame}\n\n"))
    .collect::<String>()
        + "data: [DONE]\n\n"
}

fn anthropic_answer(text: &str) -> String {
    [
        (
            "message_start",
            json!({"type": "message_start", "message": {"id": "m", "type": "message",
            "role": "assistant", "content": [], "model": "claude-probe",
            "usage": {"input_tokens": 5, "output_tokens": 1}}}),
        ),
        (
            "content_block_start",
            json!({"type": "content_block_start", "index": 0,
            "content_block": {"type": "text", "text": ""}}),
        ),
        (
            "content_block_delta",
            json!({"type": "content_block_delta", "index": 0,
            "delta": {"type": "text_delta", "text": text}}),
        ),
        (
            "content_block_stop",
            json!({"type": "content_block_stop", "index": 0}),
        ),
        (
            "message_delta",
            json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"},
            "usage": {"output_tokens": 1}}),
        ),
        ("message_stop", json!({"type": "message_stop"})),
    ]
    .iter()
    .map(|(event, data)| format!("event: {event}\ndata: {data}\n\n"))
    .collect()
}

#[tokio::test]
async fn a_test_call_is_sent_as_traffic_is_and_leaves_key_health_alone() {
    let upstream = MockServer::start().await;
    let long = "0123456789".repeat(20);
    for (secret, answer) in [
        ("sk-openai", openai_answer("OK")),
        ("sk-verbose", openai_answer(&long)),
    ] {
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(header("authorization", format!("Bearer {secret}").as_str()))
            .respond_with(ResponseTemplate::new(200).set_body_raw(answer, "text/event-stream"))
            .mount(&upstream)
            .await;
    }
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", "sk-anthropic"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(anthropic_answer("OK"), "text/event-stream"),
        )
        .mount(&upstream)
        .await;
    // An upstream that echoes the Key it refuses.
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer sk-rejected"))
        .respond_with(ResponseTemplate::new(401).set_body_string("invalid key sk-rejected"))
        .mount(&upstream)
        .await;

    let billing = BillingEngine::new();
    billing.set_master_kek(MasterKek::from_bytes([42; 32]));
    let mut disabled = ProviderKey::new("k-disabled", "p-picky", "sk-openai");
    disabled.enabled = false;
    let mut narrow = ProviderKey::new("k-narrow", "p-picky", "sk-openai");
    narrow.allowed_models = Some(vec!["another-model".into()]);
    for (provider, format, keys) in [
        (
            "p-openai",
            ProviderFormat::OpenAi,
            vec![ProviderKey::new("k-openai", "p-openai", "sk-openai")],
        ),
        (
            "p-anthropic",
            ProviderFormat::Anthropic,
            vec![ProviderKey::new(
                "k-anthropic",
                "p-anthropic",
                "sk-anthropic",
            )],
        ),
        (
            "p-rejected",
            ProviderFormat::OpenAi,
            vec![ProviderKey::new("k-rejected", "p-rejected", "sk-rejected")],
        ),
        (
            "p-verbose",
            ProviderFormat::OpenAi,
            vec![ProviderKey::new("k-verbose", "p-verbose", "sk-verbose")],
        ),
        ("p-picky", ProviderFormat::OpenAi, vec![disabled, narrow]),
    ] {
        billing.upsert_provider(Provider::new(provider, provider, format, upstream.uri()));
        for key in keys {
            billing.upsert_provider_key(key);
        }
    }
    let (app, runtime) = console(&billing);
    // A Key cooling down stays so after a test call that passes.
    let now = gateway::now_secs();
    runtime.pool_for("p-openai").unwrap().mark_key_failure(
        "k-openai",
        now,
        Duration::from_secs(600),
    );

    for (provider, model, key) in [
        ("p-openai", "gpt-probe", "k-openai"),
        ("p-anthropic", "claude-probe", "k-anthropic"),
    ] {
        let (status, reply) = post(
            &app,
            "providers/keys/probe",
            json!({"provider_id": provider, "model": model}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{reply}");
        assert_eq!(reply["success"], true);
        assert_eq!(reply["ok"], true, "{reply}");
        assert_eq!(reply["status"], 200);
        assert_eq!(reply["reply"], "OK");
        assert_eq!(reply["error"], Value::Null);
        assert_eq!(reply["key_id"], key);
        assert!(
            reply["latency_ms"].is_u64() && reply["ttft_ms"].is_u64(),
            "{reply}"
        );
    }
    // Each was one streamed request in its provider's format, with a small output limit.
    let sent = upstream.received_requests().await.unwrap();
    assert_eq!(sent.len(), 2);
    for (request, route) in sent.iter().zip(["/v1/chat/completions", "/v1/messages"]) {
        assert_eq!(request.url.path(), route);
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(body["stream"], true, "{body}");
        assert_eq!(body["max_tokens"], 16, "{body}");
        assert!(
            body["messages"].to_string().contains("Reply with OK"),
            "{body}"
        );
    }
    assert!(runtime
        .pool_for("p-openai")
        .unwrap()
        .select_key(gateway::now_secs(), &[])
        .is_err());

    // A refused call reports why, without the secret, and leaves the Key serving.
    let (status, reply) = post(
        &app,
        "providers/keys/probe",
        json!({"provider_id": "p-rejected", "model": "gpt-probe"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        (
            &reply["ok"],
            &reply["status"],
            &reply["reply"],
            &reply["ttft_ms"]
        ),
        (&json!(false), &json!(401), &Value::Null, &Value::Null)
    );
    let error = reply["error"].as_str().unwrap();
    assert!(
        error.contains("401") && error.contains("[redacted]"),
        "{error}"
    );
    assert!(!reply.to_string().contains("sk-rejected"), "{reply}");
    let pool = runtime.pool_for("p-rejected").unwrap();
    assert!(pool.select_key(gateway::now_secs(), &[]).is_ok());
    assert_eq!(
        listed_keys(&app).await.0["k-rejected"]["health_state"],
        "healthy"
    );

    // The start of a long answer is kept.
    let (_, reply) = post(
        &app,
        "providers/keys/probe",
        json!({"provider_id": "p-verbose", "model": "gpt-probe"}),
    )
    .await;
    assert_eq!(reply["reply"], long[..80]);

    // Without a Key named, an enabled one allowed the model is needed; one named is tested
    // as it is.
    let (status, reply) = post(
        &app,
        "providers/keys/probe",
        json!({"provider_id": "p-picky", "model": "gpt-probe"}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        reply["error"],
        "No enabled Key of this provider may call this model"
    );
    let (status, reply) = post(
        &app,
        "providers/keys/probe",
        json!({"provider_id": "p-picky", "model": "gpt-probe", "key_id": "k-disabled"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        (&reply["ok"], &reply["key_id"]),
        (&json!(true), &json!("k-disabled"))
    );

    for (body, status, message) in [
        (
            json!({"provider_id": "p-missing", "model": "gpt-probe"}),
            StatusCode::NOT_FOUND,
            "Unknown provider",
        ),
        (
            json!({"provider_id": "p-openai", "model": "gpt-probe", "key_id": "k-anthropic"}),
            StatusCode::NOT_FOUND,
            "Unknown key",
        ),
        (
            json!({"provider_id": "p-openai", "model": " "}),
            StatusCode::BAD_REQUEST,
            "Invalid model",
        ),
        (
            json!({"provider_id": "p-openai"}),
            StatusCode::BAD_REQUEST,
            "Invalid test call",
        ),
    ] {
        let (got, reply) = post(&app, "providers/keys/probe", body.clone()).await;
        assert_eq!(got, status, "{body}");
        assert_eq!(reply["error"], message, "{body}");
    }
    // A test call is not a request: nothing is traced or billed.
    assert!(billing.list_traces(None, 100).is_empty());
    assert!(billing.ledger_entries().is_empty());
}
