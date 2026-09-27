//! The model list shows what Kiro's own list shows: a readable name, a description that
//! never names the upstream target, and the credit multiplier ("2.2x Credit").
use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use billing::card::{Card, CardStatus};
use billing::engine::BillingEngine;
use billing::group::{Group, ModelMap};
use billing::rate_card::{Currency, PricingMode, RateCardVersion};
use gateway::auth::AuthState;
use gateway::facade::virtualization::{display_name, VirtualizationStore};
use gateway::facade::FacadeRegistry;
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;

const SECRET: &str = "test-secret-key-32bytes-for-model-display!!";

fn price(model: &str, input: i64, output: i64) -> RateCardVersion {
    RateCardVersion {
        id: format!("price-{model}"),
        rate_card_id: "default".into(),
        model: model.into(),
        currency: Currency::Cny,
        pricing_mode: PricingMode::Fixed,
        input_price_per_m: 0.0,
        output_price_per_m: 0.0,
        cache_creation_price_per_m: 0.0,
        cache_read_price_per_m: 0.0,
        fixed_input_credit_per_m: input,
        fixed_output_credit_per_m: output,
        fixed_cache_creation_credit_per_m: 0,
        fixed_cache_read_credit_per_m: 0,
        per_call_credit: 0,
        margin_multiplier: 1.0,
        effective_from_secs: 0,
        official: None,
    }
}

fn model(id: &str, order: i32, rate: Option<f64>) -> ModelMap {
    let mut map = ModelMap::new(
        format!("map-{id}"),
        "group",
        id,
        "provider",
        format!("internal-{id}"),
    );
    map.sort_order = order;
    map.rate_multiplier = rate;
    map
}

async fn list(billing: &BillingEngine) -> Vec<Value> {
    let auth = AuthState::with_billing(SECRET, billing.clone());
    let now = gateway::now_secs();
    let mut card = Card::new("card", "group", 100_000_000);
    card.status = CardStatus::Active;
    card.activated_at = Some(now);
    card.valid_until = Some(now + 86_400);
    billing.upsert_card(card);
    let token = auth.issue_token_for_card("card", 3600).unwrap();
    let mut registry = FacadeRegistry::new();
    registry
        .register_virtualized_facades(VirtualizationStore::with_billing(billing.clone(), "group"));
    let app = registry.into_router_with_auth(auth);
    let response = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/ListAvailableModels")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    body["models"].as_array().unwrap().clone()
}

fn engine() -> BillingEngine {
    let billing = BillingEngine::new();
    billing.upsert_group(Group::pro_plus("group", "Group"));
    // Per million tokens, in the ratio of the official prices: Sonnet 4.6 3/15,
    // Opus 5/25, Sonnet 5 2/10.
    for (id, input, output) in [
        ("claude-sonnet-4-6", 3_000_000, 15_000_000),
        ("claude-opus-4-8", 5_000_000, 25_000_000),
        ("claude-sonnet-5", 2_000_000, 10_000_000),
    ] {
        billing.upsert_rate_card_version(price(id, input, output));
    }
    billing
}

#[tokio::test]
async fn the_list_reads_like_kiros_own_and_never_names_the_upstream_target() {
    let billing = engine();
    billing.upsert_model_map(model("claude-sonnet-4-6", 0, Some(1.3)));
    billing.upsert_model_map(model("claude-opus-4-8", 1, Some(2.2)));
    billing.upsert_model_map(model("claude-sonnet-5", 2, None));

    let models = list(&billing).await;
    assert_eq!(models[0]["modelName"], "Claude Sonnet 4.6");
    assert_eq!(models[0]["description"], "Claude Sonnet 4.6 model");
    assert_eq!(models[0]["rateMultiplier"], 1.3);
    assert_eq!(models[0]["rateUnit"], "Credit");
    assert_eq!(models[1]["modelName"], "Claude Opus 4.8");
    assert_eq!(models[1]["rateMultiplier"], 2.2);
    // Not configured: follows its price from the first configured model (1.3 x 12 / 18).
    assert_eq!(models[2]["rateMultiplier"], 0.87);
    for listed in &models {
        assert!(!listed.to_string().contains("internal-"), "{listed}");
    }
}

#[tokio::test]
async fn without_configured_multipliers_the_default_model_is_1x() {
    let billing = engine();
    billing.upsert_model_map(model("claude-sonnet-4-6", 0, None));
    billing.upsert_model_map(model("claude-opus-4-8", 1, None));
    let mut unpriced = model("mystery-model", 2, None);
    unpriced.display_name = Some("Mystery".into());
    unpriced.description = Some("Configured by the operator".into());
    billing.upsert_model_map(unpriced);

    let models = list(&billing).await;
    assert_eq!(models[0]["rateMultiplier"], 1.0);
    assert_eq!(models[1]["rateMultiplier"], 1.67);
    // A model without a price shows no multiplier rather than a guess.
    assert!(models[2].get("rateMultiplier").is_none());
    assert!(models[2].get("rateUnit").is_none());
    assert_eq!(models[2]["modelName"], "Mystery");
    assert_eq!(models[2]["description"], "Configured by the operator");
}

#[test]
fn model_ids_read_as_names() {
    for (id, name) in [
        ("claude-sonnet-4-6", "Claude Sonnet 4.6"),
        ("claude-opus-4-8", "Claude Opus 4.8"),
        ("claude-opus-5", "Claude Opus 5"),
        ("deepseek-chat", "DeepSeek Chat"),
        ("gpt-4o-mini", "GPT 4o Mini"),
        ("glm-5", "GLM 5"),
        // Snapshot dates name a build, not the model.
        ("claude-sonnet-4-20250514", "Claude Sonnet 4"),
        ("claude-opus-4-1-20250805", "Claude Opus 4.1"),
        ("claude-3-5-haiku-20241022", "Claude 3.5 Haiku"),
    ] {
        assert_eq!(display_name(id), name, "{id}");
    }
}

/// What Kiro's registry makes of a model's effort schema: its levels from the `enum` of the
/// first effort path it knows (`cOl`), and its default from that enum's `default`, else the
/// first level (`Rgd`: `defaultEffortLevel ?? effortLevels[0]`).
fn kiro_effort(listed: &Value) -> Option<(Vec<String>, String)> {
    let schema = &listed["additionalModelRequestFieldsSchema"];
    let effort = ["output_config", "reasoning"]
        .iter()
        .map(|path| &schema["properties"][path]["properties"]["effort"])
        .find(|effort| {
            effort["enum"]
                .as_array()
                .is_some_and(|levels| !levels.is_empty())
        })?;
    let levels: Vec<String> = effort["enum"]
        .as_array()?
        .iter()
        .map(|level| level.as_str().unwrap().to_string())
        .collect();
    let default = effort["default"]
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| levels[0].clone());
    Some((levels, default))
}

#[tokio::test]
async fn kiro_starts_each_reasoning_model_at_the_upstream_models_own_default() {
    let billing = engine();
    for (order, exposed, target) in [
        (0, "opus", "claude-opus-5-5"),
        (1, "opus-classic", "claude-opus-4-6"),
        (2, "sonnet", "claude-sonnet-5"),
        (3, "codex", "gpt-5.6"),
        (4, "plain", "claude-sonnet-5"),
    ] {
        let mut map = model(exposed, order, None);
        map.target_model = target.into();
        map.supports_reasoning = exposed != "plain";
        billing.upsert_model_map(map);
    }

    let models = list(&billing).await;
    let effort = |id: &str| {
        kiro_effort(models.iter().find(|m| m["modelId"] == id).unwrap())
            .map(|(levels, default)| (levels.join(","), default))
    };
    // Named after the exposed model, a list was chosen for "opus"; the target decides.
    assert_eq!(
        effort("opus"),
        Some(("low,medium,high,xhigh,max".into(), "medium".into()))
    );
    // Opus 4.6 has no xhigh, so Kiro never offers it.
    assert_eq!(
        effort("opus-classic"),
        Some(("low,medium,high,max".into(), "high".into()))
    );
    assert_eq!(
        effort("sonnet"),
        Some(("low,medium,high,xhigh,max".into(), "high".into()))
    );
    // OpenAI's reasoning models take at most "high".
    assert_eq!(
        effort("codex"),
        Some(("low,medium,high".into(), "medium".into()))
    );
    assert_eq!(effort("plain"), None);
    // The top-level field agrees with the schema for clients that read it.
    let opus = models.iter().find(|m| m["modelId"] == "opus").unwrap();
    assert_eq!(opus["defaultEffortLevel"], "medium");
}
