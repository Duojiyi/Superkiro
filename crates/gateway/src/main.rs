use billing::engine::BillingEngine;
use billing::CardTemplate;
use gateway::auth::AuthState;
use gateway::facade::client::{ClientBeaconHandler, ClientBrandHandler, ClientNegotiateHandler};
use gateway::facade::conversation::GenerateAssistantResponseHandler;
use gateway::facade::healthz::{HealthzHandler, MetricsHandler};
use gateway::facade::mcp::McpHandler;
use gateway::facade::oauth::{OAuthTokenHandler, RefreshTokenHandler};
use gateway::facade::virtualization::VirtualizationStore;
use gateway::facade::FacadeRegistry;
use gateway::guardrail::CapacityGuardrail;
use gateway::idempotency::IdempotencyManager;
use gateway::ops::CardRateLimiter;
use gateway::ops::GatewayMetricsCollector;
use gateway::provider::anthropic::AnthropicProvider;
use gateway::provider::openai::OpenAiProvider;
use gateway::provider::{ModelProvider, ProviderConfig};
use gateway::security::{validate_trusted_proxy_config, BruteForceProtector};
use gateway::translate::VisionFallbackConfig;
use gateway::SERVICE_NAME;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
fn vision_fallback_config() -> Option<VisionFallbackConfig> {
    // Vision subrequest costs are not settled by the main-model biller yet.
    // Require explicit operator opt-in; never enable from credentials alone.
    let enabled = env_flag("VISION_FALLBACK_ENABLED", false);
    if !enabled {
        return None;
    }
    let api_key = std::env::var("VISION_FALLBACK_API_KEY")
        .ok()?
        .trim()
        .to_string();
    let base_url = std::env::var("VISION_FALLBACK_BASE_URL")
        .ok()?
        .trim()
        .to_string();
    let model = std::env::var("VISION_FALLBACK_MODEL")
        .ok()?
        .trim()
        .to_string();
    if api_key.is_empty() || base_url.is_empty() || model.is_empty() {
        return None;
    }
    Some(VisionFallbackConfig {
        enabled,
        fallback_provider_url: Some(base_url),
        fallback_api_key: Some(api_key),
        fallback_model: model,
        max_tokens: bounded_env_u32("VISION_FALLBACK_MAX_TOKENS", 1024, 128, 8192).ok()?,
    })
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 && (args[1] == "verify-snapshot" || args[1] == "--verify-snapshot") {
        let snapshot_file = args
            .get(2)
            .ok_or("Usage: gateway verify-snapshot <path_to_snapshot.json>")?;
        let kek = billing::MasterKek::from_env("KIRO_MASTER_KEK")
            .or_else(|_| {
                if let Ok(file_path) = std::env::var("KIRO_MASTER_KEK_FILE") {
                    billing::MasterKek::from_file(file_path.trim())
                } else {
                    Err(billing::CryptoError::EnvVarNotFound(
                        "KIRO_MASTER_KEK".into(),
                    ))
                }
            })
            .ok();
        match BillingEngine::verify_snapshot_integrity(snapshot_file, kek) {
            Ok(report) => {
                println!("{}", serde_json::to_string_pretty(&report)?);
                return Ok(());
            }
            Err(e) => {
                eprintln!("Error: Snapshot verification failed: {e}");
                std::process::exit(1);
            }
        }
    }

    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(19820);

    // P1-04: Default to loopback 127.0.0.1 for safety; container environments set HOST=0.0.0.0 explicitly
    let host = std::env::var("HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
    let ip: std::net::IpAddr = host
        .parse()
        .map_err(|e| format!("Invalid HOST '{host}': {e}"))?;
    let addr = SocketAddr::from((ip, port));
    validate_trusted_proxy_config()?;

    // 1. 初始化单机持久化存储与账本引擎 (Single-Server Durable Engine)
    let data_dir = std::env::var("DATA_DIR").unwrap_or_else(|_| "./data".to_string());
    let state_file = Path::new(&data_dir).join("billing_state.json");
    let billing = BillingEngine::new();
    billing.set_persistence_path(&state_file);

    // Optional Master KEK for snapshot AEAD encryption (Spec §7, P1-03, P1-05)
    let master_kek = if std::env::var("KIRO_MASTER_KEK").is_ok() {
        Some(
            billing::MasterKek::from_env("KIRO_MASTER_KEK")
                .map_err(|e| format!("Invalid KIRO_MASTER_KEK: {e}"))?,
        )
    } else if let Ok(file_path) = std::env::var("KIRO_MASTER_KEK_FILE") {
        Some(
            billing::MasterKek::from_file(file_path.trim())
                .map_err(|e| format!("Failed to read KIRO_MASTER_KEK_FILE ({file_path}): {e}"))?,
        )
    } else {
        None
    };
    let require_encrypted_snapshots = env_flag("REQUIRE_ENCRYPTED_SNAPSHOTS", true)
        || !env_flag("ALLOW_PLAINTEXT_SNAPSHOTS", false);
    match master_kek {
        Some(kek) => {
            billing.set_master_kek(kek);
            println!("[√] Billing state AEAD encryption enabled (KIRO_MASTER_KEK)");
        }
        None if require_encrypted_snapshots => {
            return Err(
                "REQUIRE_ENCRYPTED_SNAPSHOTS is enabled but Master KEK is unavailable: set KIRO_MASTER_KEK or KIRO_MASTER_KEK_FILE"
                    .into(),
            );
        }
        None => {
            println!("[*] Notice: No KIRO_MASTER_KEK set; running in standard JSON snapshot mode");
        }
    }

    let state_anchor = Path::new(&format!("{}.anchor", state_file.display())).to_path_buf();
    // An anchor plus generation file is a committed snapshot even when the mirror
    // rename was interrupted. Let BillingEngine recover and self-heal it.
    if state_file.exists() || state_anchor.exists() {
        if require_encrypted_snapshots {
            let content = if state_file.exists() {
                std::fs::read_to_string(&state_file)?
            } else {
                let anchor: billing::SnapshotAnchor =
                    serde_json::from_str(&std::fs::read_to_string(&state_anchor)?)?;
                let generation = anchor
                    .generation_file
                    .ok_or("snapshot anchor has no recoverable generation")?;
                std::fs::read_to_string(Path::new(&data_dir).join(generation))?
            };
            let is_encrypted = serde_json::from_str::<serde_json::Value>(&content)
                .ok()
                .and_then(|value| {
                    value
                        .get("format")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned)
                })
                .as_deref()
                == Some("kiro-billing-aead-v1");
            if !is_encrypted {
                return Err(format!(
                    "REQUIRE_ENCRYPTED_SNAPSHOTS is enabled but {:?} is not an encrypted billing snapshot",
                    state_file
                )
                .into());
            }
        }
        match billing.load_from_file(&state_file) {
            // The release tooling checks this line: the sequence must be the one it left.
            Ok(_) => println!(
                "[√] Restored billing state at sequence {} from {:?}",
                billing.snapshot_sequence(),
                state_file
            ),
            Err(e) => {
                return Err(format!(
                    "Failed to load billing state snapshot from {:?}: {e}; refusing to start with unreadable or corrupted ledger",
                    state_file
                )
                .into());
            }
        }
    }

    // Seed the explicitly configured bootstrap card once, without storing its
    // plaintext code.  Existing state always wins and is never overwritten.
    if let Some(dev_code) = get_secret_from_env_or_file("DEV_CARD_CODE")? {
        if dev_code.chars().count() < 16 || dev_code.chars().count() > 256 {
            return Err("DEV_CARD_CODE must contain 16..256 characters".into());
        }
        if billing.find_card_by_secret(&dev_code).is_none() {
            let dev_template =
                CardTemplate::tier("standard-monthly", "group-pro-plus").expect("built-in tier");
            let card = billing::Card::from_template(
                "card-dev-bootstrap",
                billing::hash_card_code(&dev_code),
                &dev_template,
                Some("bootstrap card".to_string()),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
            );
            billing.upsert_card(card);
        }
    }

    // 2. 装配共享认证与虚拟化上下文
    let auth_secret = required_secret("AUTH_SECRET")?;
    let auth_state = AuthState::with_billing(&auth_secret, billing.clone());
    let store = VirtualizationStore::with_billing(billing.clone(), "group-pro-plus");

    let runtime = gateway::provider::ProviderRuntimeRegistry::new();
    runtime.sync_from_billing(&billing);
    let mut registry = FacadeRegistry::new().with_runtime(runtime.clone());

    // 3. 注册健康检查 (/healthz) 与 Kiro IDE 冒充路由 (Spec §4.2)
    registry.register_virtualized_facades(store.clone());
    let metrics = GatewayMetricsCollector::default();
    registry
        .register(HealthzHandler::default().with_billing(billing.clone()))
        .register(MetricsHandler::new(metrics.clone()));

    // 4. 注册连接真实账本的 OAuth 生产处理器
    let login_protector = Arc::new(BruteForceProtector::default());
    let oauth_handler = OAuthTokenHandler::new(Arc::new(billing.clone()), auth_state.clone())
        .with_protector(login_protector);
    let refresh_handler = RefreshTokenHandler::new(Arc::new(billing.clone()), auth_state.clone());
    registry.register(oauth_handler);
    registry.register(refresh_handler);

    // 5. 注册 Web 自助门户。发卡平台接口 (/api/v1/cards/*) 已移除：没有接入方，
    // 却是一组只凭一个共享密钥就能拉取卡密和充值的公开接口。
    registry.register_portal_facades(billing.clone(), Some(store.clone()));

    // 5.1 注册管理端 REST 接口 (P0-01, P0-02, P1-02)
    let admin_key = required_secret("ADMIN_KEY").or_else(|_| required_secret("ADMIN_SECRET"))?;
    if admin_key == auth_secret {
        return Err("ADMIN_KEY must be different from AUTH_SECRET".into());
    }
    gateway::facade::admin_login::BrowserAuth::from_env()?;
    registry.register_admin_facades_secure(billing.clone(), admin_key);

    // 6. 注册客户端协商、信标与白标
    registry.register(ClientNegotiateHandler);
    registry.register(ClientBeaconHandler);
    registry.register(ClientBrandHandler::default());

    // 7. 注册 MCP 与供应商一键导入
    registry.register(McpHandler::default());
    registry.register_provider_import_facade(store.clone(), billing.clone());

    // 8. 注册对话流式接管处理器 (P1-05, T04)
    let has_persistent_providers = runtime.has_available_provider();
    let upstream_api_key = get_secret_from_env_or_file("UPSTREAM_API_KEY")?;

    if upstream_api_key.is_some() || has_persistent_providers {
        let (provider, config) = if let Some(api_key) = upstream_api_key {
            let provider_type =
                std::env::var("PROVIDER_TYPE").unwrap_or_else(|_| "openai".to_string());
            let base_url = std::env::var("UPSTREAM_BASE_URL").unwrap_or_else(|_| {
                if provider_type == "anthropic" {
                    "https://api.anthropic.com".to_string()
                } else {
                    "https://api.openai.com".to_string()
                }
            });
            let model = std::env::var("UPSTREAM_MODEL").unwrap_or_else(|_| {
                if provider_type == "anthropic" {
                    "claude-3-5-sonnet-20241022".to_string()
                } else {
                    "gpt-4o".to_string()
                }
            });

            let provider: Arc<dyn ModelProvider> = match provider_type.as_str() {
                "anthropic" => Arc::new(AnthropicProvider),
                "openai" => Arc::new(OpenAiProvider),
                other => return Err(format!("unsupported PROVIDER_TYPE: {other}").into()),
            };

            store.set_fallback_model(&model);
            let config = ProviderConfig {
                base_url: base_url.clone(),
                api_key,
                model,
                timeout: Duration::from_secs(600),
                group_id: None,
            };
            println!(
                "[*] Configured fallback upstream provider: {} ({})",
                provider_type, base_url
            );
            (Some(provider), Some(config))
        } else {
            println!("[*] Using persistent upstream providers loaded from billing state");
            (None, None)
        };

        let max_inflight = bounded_env_u32("BYOK_MAX_INFLIGHT", 100, 1, 10_000)? as usize;
        let card_qps = bounded_env_u32("BYOK_CARD_QPS", 60, 1, 100_000)?;
        let handler = GenerateAssistantResponseHandler {
            client: reqwest::Client::new(),
            provider,
            provider_config: config,
            pool: None,
            runtime: Some(runtime.clone()),
            fallback_pools: std::collections::HashMap::new(),
            billing: billing.clone(),
            idempotency: IdempotencyManager::default(),
            guardrail: CapacityGuardrail::new(max_inflight, 2),
            card_rate_limiter: CardRateLimiter::new(card_qps),
            intercept_intent: true,
            vision_config: vision_fallback_config(),
            vision_cache: Default::default(),
            content_guardrail: Default::default(),
        };
        registry.register(handler);
    } else if env_flag("ALLOW_STUB_MODE", false) {
        println!("[*] Notice: ALLOW_STUB_MODE=true; running in explicit local mock/stub mode for conversation.");
    } else {
        return Err(
            "UPSTREAM_API_KEY (or UPSTREAM_API_KEY_FILE) is required, or imported providers must exist; set ALLOW_STUB_MODE=true only for isolated demos"
                .into(),
        );
    }

    // Reclaim abandoned reservations after a crash or disconnected client.
    let janitor_billing = billing.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(30));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut recovery = billing::engine::PendingSettlementRecovery::default();
        let recovery_started = std::time::Instant::now();
        loop {
            interval.tick().await;
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            // Snapshot persistence is blocking IO; do not block an async worker.
            let engine = janitor_billing.clone();
            let retry_now = recovery_started.elapsed().as_secs();
            let result = tokio::task::spawn_blocking(move || {
                for (invocation_id, result) in recovery.tick(&engine, retry_now) {
                    match result {
                        Ok(_) => eprintln!("[kiro-billing] pending settlement recovered: invocation={invocation_id}"),
                        Err(error) => eprintln!("[kiro-billing] ERROR pending recovery: invocation={invocation_id} error={error}; hold retained, retry backs off up to 300s"),
                    }
                }
                (recovery, engine.run_janitor(now))
            }).await;
            let reclaimed;
            match result {
                Ok((schedule, count)) => {
                    recovery = schedule;
                    reclaimed = count;
                }
                Err(error) => {
                    eprintln!("[kiro-billing] ERROR maintenance task failed: {error}");
                    recovery = billing::engine::PendingSettlementRecovery::default();
                    continue;
                }
            }
            if reclaimed > 0 {
                eprintln!("[kiro-billing] reclaimed {reclaimed} expired reservations");
            }
        }
    });

    let router = registry.into_router_with_auth(auth_state);

    // 9. 全局安全 middleware (P1-06: body limit, timeout, request-id)
    let router = router
        .layer(axum::extract::DefaultBodyLimit::max(10 * 1024 * 1024)) // 10 MB
        .layer(tower_http::timeout::TimeoutLayer::new(Duration::from_secs(
            300,
        )))
        .layer(axum::middleware::from_fn_with_state(
            metrics,
            gateway::ops::metrics::metrics_middleware,
        ));

    println!("========================================================");
    println!("  {} (Single-Server All-In-One Appliance)", SERVICE_NAME);
    println!("========================================================");
    println!("[√] Listening on http://{}", addr);
    println!("[√] Health Check Endpoint: http://{}/healthz", addr);
    println!("[√] Web Self-Service Portal: http://{}/portal", addr);
    println!(
        "[√] Client Negotiate Endpoint: http://{}/client/negotiate",
        addr
    );
    println!("[√] Global body limit: 10 MB, request timeout: 300s");

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("Failed to bind TCP listener");

    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await?;

    // 9. 停机持久化保证 (Durability Guarantee on Shutdown)
    if let Err(e) = billing.save_to_file(&state_file) {
        eprintln!(
            "[!] Warning: failed to save billing snapshot to {:?}: {}",
            state_file, e
        );
    } else {
        println!("[√] Cleanly saved billing snapshot to {:?}", state_file);
    }
    Ok(())
}

fn get_secret_from_env_or_file(name: &str) -> Result<Option<String>, Box<dyn std::error::Error>> {
    if let Ok(value) = std::env::var(name) {
        let trimmed = value.trim().to_string();
        if !trimmed.is_empty() {
            return Ok(Some(trimmed));
        }
    }
    let file_var = format!("{}_FILE", name);
    if let Ok(file_path) = std::env::var(&file_var) {
        let content = std::fs::read_to_string(file_path.trim()).map_err(|e| {
            format!("Failed to read secret file from {file_var} ({file_path}): {e}")
        })?;
        let trimmed = content.trim().to_string();
        if !trimmed.is_empty() {
            return Ok(Some(trimmed));
        }
    }
    Ok(None)
}

fn required_secret(name: &str) -> Result<String, Box<dyn std::error::Error>> {
    let value = get_secret_from_env_or_file(name)?.ok_or_else(|| {
        format!(
            "{name} (or {name}_FILE) must be set; refusing to start without production credentials"
        )
    })?;
    if value.len() < 32 {
        return Err(format!("{name} must be at least 32 characters").into());
    }
    Ok(value)
}

fn env_flag(name: &str, default: bool) -> bool {
    std::env::var(name)
        .map(|value| value.eq_ignore_ascii_case("true") || value == "1")
        .unwrap_or(default)
}

fn bounded_env_u32(
    name: &str,
    default: u32,
    min: u32,
    max: u32,
) -> Result<u32, Box<dyn std::error::Error>> {
    let value = std::env::var(name)
        .ok()
        .map(|raw| raw.parse::<u32>())
        .transpose()
        .map_err(|_| format!("{name} must be an integer"))?
        .unwrap_or(default);
    if !(min..=max).contains(&value) {
        return Err(format!("{name} must be between {min} and {max}").into());
    }
    Ok(value)
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("Failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("Failed to install signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    println!(
        "
[*] Graceful shutdown signal received, draining server connections..."
    );
}
