//! A customer's request and the model's reply, kept 24 hours and read back by an
//! administrator. This binary installs the process-wide archive; no other test does.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use billing::card::Card;
use billing::crypto::MasterKek;
use billing::engine::BillingEngine;
use billing::reservation::ReservationEstimateParams;
use bytes::Bytes;
use futures_util::StreamExt;
use gateway::archive::{self, RequestArchive};
use gateway::facade::admin::{AdminAuthState, AdminTraceContentHandler};
use gateway::facade::FacadeHandler;
use gateway::provider::{ProviderDelta, ProviderStreamEvent, ReceiverStream, TokenUsage};
use gateway::stream::{create_stream_guard, BillingSettler, StreamGuardConfig};
use http_body_util::BodyExt;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;

mod support;

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn billing_with_card(card_id: &str) -> BillingEngine {
    let dir = std::env::temp_dir().join(format!("archive-billing-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let billing = BillingEngine::new();
    billing.set_persistence_path(dir.join("billing_state.json"));
    billing.set_master_kek(MasterKek::from_bytes([7u8; 32]));
    billing.upsert_rate_card_version(support::wildcard_price("default"));
    let mut card = Card::new(card_id, "group-default", 10_000_000);
    card.activate(now(), 86400 * 30).unwrap();
    billing.upsert_card(card);
    billing
}

async fn view(invocation: Option<&str>, authenticated: bool) -> (StatusCode, serde_json::Value) {
    let handler = AdminTraceContentHandler {
        auth: Arc::new(AdminAuthState::new("admin")),
    };
    let uri = match invocation {
        Some(id) => format!(
            "/api/v1/admin/traces/content?invocation_id={}",
            id.replace(':', "%3A")
        ),
        None => "/api/v1/admin/traces/content".to_string(),
    };
    let mut request = Request::builder().method("GET").uri(uri);
    if authenticated {
        request = request.header("x-admin-key", "admin");
    }
    let response = handler.handle(request.body(Body::empty()).unwrap()).await;
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn an_administrator_reads_a_request_and_the_models_reply_for_24_hours() {
    let dir = std::env::temp_dir().join(format!("archive-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    archive::install(RequestArchive::open(&dir, MasterKek::generate_random().unwrap()).unwrap());
    let store = archive::active().unwrap();

    // Only an administrator, for a named request.
    assert_eq!(
        view(Some("card-e2e:inv-1"), false).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(view(None, true).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(
        view(Some("card-e2e:unknown"), true).await.0,
        StatusCode::NOT_FOUND
    );

    // The request as the handler keeps it when it arrives.
    let card_id = "card-e2e";
    let invocation = "card-e2e:inv-1";
    let body = serde_json::json!({"conversationState": {
        "currentMessage": {"userInputMessage": {"content": "登录按钮点了没反应，帮我修一下"}},
        "history": []
    }});
    store.keep_request(
        invocation,
        card_id,
        "claude-opus-5",
        Bytes::from(body.to_string()),
    );

    // The reply streams through the gateway as it would to Kiro.
    let billing = billing_with_card(card_id);
    let params = ReservationEstimateParams {
        estimated_input_tokens: 1000,
        max_output_tokens: 2000,
        input_rate_per_m: 15_000_000,
        output_rate_per_m: 60_000_000,
        credit_multiplier: 1.0,
        margin_multiplier: 1.0,
        model: Some("claude-opus-5".to_string()),
    };
    billing
        .reserve(card_id, invocation, &params, now(), 300)
        .unwrap();
    let (tx, rx) = mpsc::channel(16);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let events = [
            ProviderStreamEvent::Delta(ProviderDelta::Reasoning("先看点击处理".into())),
            ProviderStreamEvent::Delta(ProviderDelta::Text("按钮缺少 onClick，已补上。".into())),
            ProviderStreamEvent::Delta(ProviderDelta::ToolCallChunk {
                index: Some(0),
                id: Some("call-1".into()),
                name: Some("fsWrite".into()),
                arguments: "{\"path\":\"src/Login.tsx\"}".into(),
            }),
            ProviderStreamEvent::Usage(TokenUsage {
                uncached_prompt_tokens: 900,
                prompt_tokens: 1000,
                completion_tokens: 30,
                total_tokens: 1030,
                output_tokens_final: true,
                cache_read_input_tokens: Some(100),
                cache_creation_input_tokens: None,
            }),
            ProviderStreamEvent::Done,
        ];
        for event in events {
            let _ = tx.send(Ok(event)).await;
        }
    });
    let settler = BillingSettler::new(
        billing.clone(),
        invocation.to_string(),
        "claude-opus-5".to_string(),
        "provider-a".to_string(),
        "claude-opus-5-upstream".to_string(),
    )
    .with_estimated_input(1000);
    let config = StreamGuardConfig {
        keepalive_interval: Duration::from_secs(60),
        model_id: "claude-opus-5".to_string(),
        context_window: None,
    };
    let mut frames =
        create_stream_guard(ReceiverStream::new(rx), config, None, None, Some(settler));
    while frames.next().await.is_some() {}

    // The writes run off the request's thread: wait for the reply to land.
    let mut record = serde_json::Value::Null;
    for _ in 0..100 {
        let (status, value) = view(Some(invocation), true).await;
        if status == StatusCode::OK && !value["reply"].is_null() {
            record = value;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(record["success"], true, "{record}");
    assert_eq!(record["cardId"], card_id);
    assert_eq!(record["model"], "claude-opus-5");
    assert_eq!(
        record["request"]["conversationState"]["currentMessage"]["userInputMessage"]["content"],
        "登录按钮点了没反应，帮我修一下"
    );
    let reply = &record["reply"];
    assert_eq!(reply["status"], "success");
    assert_eq!(reply["text"], "按钮缺少 onClick，已补上。");
    assert_eq!(reply["reasoning"], "先看点击处理");
    assert_eq!(reply["toolCalls"][0]["name"], "fsWrite");
    assert_eq!(
        reply["toolCalls"][0]["arguments"],
        "{\"path\":\"src/Login.tsx\"}"
    );
    assert_eq!(reply["providerId"], "provider-a");
    assert_eq!(reply["targetModel"], "claude-opus-5-upstream");
    assert_eq!(reply["inputTokens"], 1000);
    assert_eq!(reply["outputTokens"], 30);
    assert_eq!(reply["cacheReadTokens"], 100);
    assert!(reply["ttftMs"].as_u64().is_some(), "{reply}");
    let expires = record["expiresAt"].as_u64().unwrap();
    assert!(
        expires > now() + 23 * 3600 && expires <= now() + 24 * 3600,
        "{expires}"
    );

    // A day later it is gone, and pruning removes its files.
    assert!(store
        .read(invocation, now() + archive::RETENTION_SECS)
        .is_none());
    store.prune(now() + archive::RETENTION_SECS);
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
    let _ = std::fs::remove_dir_all(&dir);
}
