//! Dual-blind audit test for provider import from Cherry Studio and CC Switch (Spec §14.3, P4-6).

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use billing::provider::ProviderFormat;
use gateway::facade::provider_import::{ProviderImportHandler, ProviderImportResponse};
use gateway::facade::virtualization::VirtualizationStore;
use gateway::facade::FacadeHandler;
use gateway::provider::import::{import_providers, SourceFormat};
use serde_json::json;

#[test]
fn test_cherry_studio_export_import() {
    let cherry_json = json!({
        "version": 1,
        "providers": [
            {
                "id": "openai-official",
                "name": "OpenAI Official",
                "apiType": "openai",
                "baseUrl": "https://api.openai.com/v1",
                "apiKey": "sk-test-openai-12345",
                "enabled": true,
                "models": [
                    { "id": "gpt-4o", "name": "GPT-4o (Omni)" },
                    { "id": "o1-preview", "name": "OpenAI o1" }
                ]
            },
            {
                "id": "anthropic-direct",
                "name": "Anthropic Direct",
                "apiType": "anthropic",
                "baseUrl": "https://api.anthropic.com",
                "apiKey": "sk-ant-test-99999",
                "enabled": true,
                "models": [
                    { "id": "claude-3-5-sonnet-20241022", "name": "Claude 3.5 Sonnet" }
                ]
            }
        ]
    })
    .to_string();

    let providers = import_providers(SourceFormat::CherryStudio, &cherry_json).unwrap();
    assert_eq!(providers.len(), 2);

    // Verify first provider (OpenAI)
    let p1 = &providers[0];
    assert_eq!(p1.id, "openai-official");
    assert_eq!(p1.name, "OpenAI Official");
    assert_eq!(p1.format, ProviderFormat::OpenAi);
    assert_eq!(p1.base_url, "https://api.openai.com/v1");
    assert_eq!(p1.api_key, "sk-test-openai-12345");
    assert_eq!(p1.models, vec!["gpt-4o", "o1-preview"]);
    assert!(p1.enabled);

    let (domain_p1, domain_k1) = p1.to_provider_and_key();
    assert_eq!(domain_p1.id, "openai-official");
    assert_eq!(domain_k1.api_key, "sk-test-openai-12345");

    let model_infos = p1.to_model_infos();
    assert_eq!(model_infos.len(), 2);
    assert!(!model_infos[0].supports_reasoning); // gpt-4o
    assert!(model_infos[1].supports_reasoning); // o1-preview

    // Verify second provider (Anthropic)
    let p2 = &providers[1];
    assert_eq!(p2.id, "anthropic-direct");
    assert_eq!(p2.format, ProviderFormat::Anthropic);
    assert_eq!(p2.base_url, "https://api.anthropic.com");
    assert_eq!(p2.models, vec!["claude-3-5-sonnet-20241022"]);
}

#[test]
fn test_cc_switch_config_import() {
    let cc_switch_json = json!({
        "providers": [
            {
                "name": "DeepSeek API",
                "api_type": "openai",
                "base_url": "https://api.deepseek.com/v1/",
                "api_key": "sk-ds-secret-key",
                "models": ["deepseek-chat", "deepseek-reasoner"],
                "enabled": true
            },
            {
                "name": "Custom Claude Gateway",
                "type": "claude",
                "url": "https://proxy.example.com/anthropic",
                "key": "sk-proxy-token",
                "models": ["claude-3-haiku-20240307"]
            }
        ]
    })
    .to_string();

    let providers = import_providers(SourceFormat::CcSwitch, &cc_switch_json).unwrap();
    assert_eq!(providers.len(), 2);

    let ds = &providers[0];
    assert_eq!(ds.id, "deepseek-api");
    assert_eq!(ds.base_url, "https://api.deepseek.com/v1"); // trailing slash stripped
    assert_eq!(ds.format, ProviderFormat::OpenAi);
    assert_eq!(ds.models, vec!["deepseek-chat", "deepseek-reasoner"]);

    let cc = &providers[1];
    assert_eq!(cc.id, "custom-claude-gateway");
    assert_eq!(cc.format, ProviderFormat::Anthropic);
    assert_eq!(cc.api_key, "sk-proxy-token");
}

#[test]
fn test_cc_switch_dict_format() {
    let dict_json = json!({
        "providers": {
            "siliconflow": {
                "name": "SiliconFlow Cloud",
                "baseUrl": "https://api.siliconflow.cn/v1",
                "apiKey": "sk-sf-test",
                "models": ["Qwen/Qwen2.5-72B-Instruct"]
            }
        }
    })
    .to_string();

    let providers = import_providers(SourceFormat::CcSwitch, &dict_json).unwrap();
    assert_eq!(providers.len(), 1);
    assert_eq!(providers[0].id, "siliconflow");
    assert_eq!(providers[0].name, "SiliconFlow Cloud");
    assert_eq!(providers[0].base_url, "https://api.siliconflow.cn/v1");
}

#[test]
fn test_auto_detection() {
    let cherry_raw = json!({
        "version": 1,
        "providers": [
            {
                "name": "OpenAI",
                "apiType": "openai",
                "baseUrl": "https://api.openai.com",
                "apiKey": "sk-123"
            }
        ]
    })
    .to_string();

    let detected_cherry = import_providers(SourceFormat::Auto, &cherry_raw).unwrap();
    assert_eq!(detected_cherry.len(), 1);
    assert_eq!(detected_cherry[0].name, "OpenAI");

    let cc_raw = json!([
        {
            "name": "DeepSeek",
            "url": "https://api.deepseek.com",
            "key": "sk-ds"
        }
    ])
    .to_string();

    let detected_cc = import_providers(SourceFormat::Auto, &cc_raw).unwrap();
    assert_eq!(detected_cc.len(), 1);
    assert_eq!(detected_cc[0].name, "DeepSeek");
}

#[tokio::test]
async fn test_provider_import_does_not_publish_models() {
    let store = VirtualizationStore::default();
    let handler = ProviderImportHandler::new().with_store(store.clone());

    let payload = json!({
        "format": "cherry_studio",
        "content": {
            "version": 1,
            "providers": [
                {
                    "name": "Groq Llama Fast",
                    "apiType": "openai",
                    "baseUrl": "https://api.groq.com/openai/v1",
                    "apiKey": "gsk-test-key",
                    "models": [
                        { "id": "llama-3.3-70b-versatile", "name": "Llama 3.3 70B" }
                    ]
                }
            ]
        }
    });

    let mut req = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/admin/providers/import")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(payload.to_string()))
        .unwrap();
    // SEC-3: admin auth required
    req.extensions_mut().insert(gateway::auth::AuthClaims {
        card_id: "admin-card".to_string(),
        group_id: "admin".to_string(),
        token_version: 1,
        exp: u64::MAX,
        iat: 0,
    });

    let resp = handler.handle(req).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .unwrap();
    let import_resp: ProviderImportResponse = serde_json::from_slice(&bytes).unwrap();

    assert!(import_resp.success);
    assert_eq!(import_resp.imported_count, 1);
    assert_eq!(import_resp.providers[0].name, "Groq Llama Fast");

    // Imported candidates must not automatically become visible to users.
    let group = store.get_group(None);
    let imported_model = group
        .models
        .iter()
        .find(|m| m.model_id == "llama-3.3-70b-versatile");
    assert!(
        imported_model.is_none(),
        "Imported model must remain unpublished"
    );
}

#[tokio::test]
async fn test_invalid_import_payload_rejection() {
    let handler = ProviderImportHandler::new();

    // Invalid JSON
    let mut req_bad_json = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/admin/providers/import")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from("{ not valid json"))
        .unwrap();
    req_bad_json
        .extensions_mut()
        .insert(gateway::auth::AuthClaims {
            card_id: "admin-card".to_string(),
            group_id: "admin".to_string(),
            token_version: 1,
            exp: u64::MAX,
            iat: 0,
        });

    let resp = handler.handle(req_bad_json).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    // Empty providers schema
    let mut req_empty = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/admin/providers/import")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({ "providers": [] }).to_string()))
        .unwrap();
    req_empty
        .extensions_mut()
        .insert(gateway::auth::AuthClaims {
            card_id: "admin-card".to_string(),
            group_id: "admin".to_string(),
            token_version: 1,
            exp: u64::MAX,
            iat: 0,
        });

    let resp2 = handler.handle(req_empty).await;
    assert_eq!(resp2.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

#[test]
fn imported_key_permissions_do_not_invent_models_or_use_display_names() {
    let raw = json!({"providers": [
        {"id":"empty", "name":"Empty", "base_url":"https://example.com", "api_key":"test", "models":[]},
        {"id":"named", "name":"Named", "base_url":"https://example.com", "api_key":"test", "models":[{"id":"actual-id", "name":"Display Name"}]}
    ]}).to_string();
    let providers = import_providers(SourceFormat::CcSwitch, &raw).unwrap();
    assert_eq!(
        providers[0].to_provider_and_key().1.allowed_models,
        Some(vec![])
    );
    assert_eq!(
        providers[1].to_provider_and_key().1.allowed_models,
        Some(vec!["actual-id".into()])
    );
}
