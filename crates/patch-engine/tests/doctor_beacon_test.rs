//! Integration tests for Doctor health inspection, one-click repair, process helpers, and version beacon.
//!
//! Spec §9, §10, §14.5.

use axum::routing::post;
use axum::Router;
use patch_engine::beacon::{BeaconClient, ClientNegotiateRequest, HealthBeacon};
use patch_engine::detect::inspect_installation_dir;
use patch_engine::doctor::{Doctor, TakeoverStatus};
use patch_engine::process::{launch_kiro, ProcessError};
use patch_engine::settings::SettingsManager;
use patch_engine::token_storage::TokenStorage;
use std::fs;
use tokio::net::TcpListener;

#[tokio::test]
async fn test_doctor_diagnostics_and_one_click_repair() {
    let temp_dir = std::env::temp_dir().join(format!("kiro_test_doctor_{}", std::process::id()));
    let settings_file = temp_dir.join("settings.json");
    let token_file = temp_dir.join("token.json");
    fs::create_dir_all(&temp_dir).unwrap();

    let settings_mgr = SettingsManager::at(&settings_file);
    let token_storage = TokenStorage::at(&token_file);

    let doctor = Doctor::new(settings_mgr.clone(), token_storage.clone());

    // Every installation and credential path is synthetic; the gateway is a local mock.
    let app_dir = if cfg!(target_os = "macos") {
        temp_dir.join("Contents/Resources/app")
    } else {
        temp_dir.join("resources/app")
    };
    let agent_dir = app_dir.join("extensions/kiro.kiro-agent");
    fs::create_dir_all(agent_dir.join("dist")).unwrap();
    fs::write(
        app_dir.join("product.json"),
        r#"{"nameShort":"Kiro","win32MutexName":"kiro"}"#,
    )
    .unwrap();
    fs::write(app_dir.join("package.json"), r#"{"version":"1.0.437"}"#).unwrap();
    fs::write(agent_dir.join("package.json"), r#"{"version":"1.0.794"}"#).unwrap();
    fs::write(
        agent_dir.join("dist/extension.js"),
        "const endpoint = `https://runtime.${t}.kiro.dev`;",
    )
    .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().route("/healthz", axum::routing::get(|| async { "ok" })),
        )
        .await
        .unwrap();
    });
    let report_initial = doctor.diagnose(&url, Some(&temp_dir)).await;
    assert_eq!(report_initial.overall_status, TakeoverStatus::NotTakenOver);
    assert_eq!(report_initial.items.len(), 7);
    let install = inspect_installation_dir(&temp_dir).unwrap();
    doctor.one_click_fix(&url, &install).unwrap();
    assert!(settings_mgr.is_byok_active(Some(&url)));
    server.abort();
    let _ = server.await;

    let _ = fs::remove_dir_all(temp_dir);
}

#[tokio::test]
async fn test_version_negotiation_and_health_beacon_handshake() {
    // 1. Start lightweight local mock server for negotiate and beacon
    let app = Router::new()
        .route(
            "/client/negotiate",
            post(|| async {
                axum::Json(serde_json::json!({
                    "supported": true,
                    "serverVersion": "0.1.0",
                    "minClientVersion": "0.1.0",
                    "recommendedPatchVersion": "v1",
                    "announcements": ["Maintenance window scheduled"]
                }))
            }),
        )
        .route(
            "/client/beacon",
            post(|| async { axum::Json(serde_json::json!({ "status": "ok" })) }),
        );

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let server_url = format!("http://{}", addr);
    let client = BeaconClient::new();

    // 2. Negotiate version
    let neg_req = ClientNegotiateRequest {
        client_version: "0.1.0".to_string(),
        kiro_version: "1.0.437".to_string(),
        os: "windows".to_string(),
        arch: "x86_64".to_string(),
        patch_status: "patched".to_string(),
    };

    let neg_resp = client
        .negotiate(&server_url, &neg_req)
        .await
        .expect("Negotiation must succeed");
    assert!(neg_resp.supported);
    assert_eq!(neg_resp.server_version, "0.1.0");
    assert_eq!(
        neg_resp.announcements,
        vec!["Maintenance window scheduled".to_string()]
    );

    // 3. Send health beacon
    let beacon = HealthBeacon {
        client_version: "0.1.0".to_string(),
        device_id: "dev_test_beacon".to_string(),
        is_kiro_running: false,
        timestamp: 1_700_000_000,
    };

    let beacon_ok = client
        .send_beacon(&server_url, &beacon)
        .await
        .expect("Beacon must succeed");
    assert!(beacon_ok);
}

#[test]
fn test_process_lifecycle_helpers() {
    // Never stop or launch an installed application in the test suite.
    // Launch with non-existent executable must return clean error
    let fake_install = patch_engine::detect::KiroInstallation {
        install_dir: std::path::PathBuf::from("/non/existent/dir"),
        executable_path: std::path::PathBuf::from("/non/existent/Kiro.exe"),
        product_json_path: std::path::PathBuf::from("/non/existent/product.json"),
        version: "1.0.0".to_string(),
        vscode_version: None,
        commit: None,
        quality: None,
        win32_mutex_name: "kiro".to_string(),
        agent_extension_dir: None,
        agent_version: None,
        is_user_level: true,
    };

    let launch_res = launch_kiro(&fake_install, "http://127.0.0.1:8080", &[]);
    assert!(matches!(
        launch_res,
        Err(ProcessError::ExecutableNotFound(_))
    ));
}

#[tokio::test]
async fn test_p4_1_server_patch_recipe_and_ui_overrides() {
    use patch_engine::{ClientModelCatalogItem, ExtensionPatcher};
    use std::collections::HashMap;

    // 1. Mock server with P4-1 patch recipe and UI overrides
    let mut rename_rules = HashMap::new();
    rename_rules.insert(
        "anthropic.claude-3-7-sonnet".to_string(),
        "Claude 3.7 Sonnet (极速推理)".to_string(),
    );

    let app = Router::new().route(
        "/client/negotiate",
        post(move || async move {
            axum::Json(serde_json::json!({
                "supported": true,
                "serverVersion": "0.1.0",
                "minClientVersion": "0.1.0",
                "recommendedPatchVersion": "v2",
                "announcements": ["P4-1 Cloud Distribution Ready"],
                "patchRecipe": {
                    "recipeId": "v2-hotfix",
                    "marker": "/* @patched-kiro-byok v2 */",
                    "needle": "https://runtime.${t}.kiro.dev",
                    "replacement": "http://127.0.0.1:44040/custom_endpoint",
                    "extraEnvs": {
                        "CUSTOM_FLAG": "enabled"
                    }
                },
                "uiOverrides": {
                    "brandName": "KIRO 极速接管定制版",
                    "brandSubtitle": "旗舰专线",
                    "creditLabel": "商业算力",
                    "announcements": ["P4-1 Cloud Distribution Ready"],
                    "modelRenameRules": {
                        "anthropic.claude-3-7-sonnet": "Claude 3.7 Sonnet (极速推理)"
                    },
                    "hiddenModels": ["deprecated-model-v1"]
                }
            }))
        }),
    );

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let server_url = format!("http://{}", addr);
    let client = BeaconClient::new();
    let neg_req = ClientNegotiateRequest {
        client_version: "0.1.0".to_string(),
        kiro_version: "1.0.437".to_string(),
        os: "windows".to_string(),
        arch: "x86_64".to_string(),
        patch_status: "official".to_string(),
    };

    let neg_resp = client.negotiate(&server_url, &neg_req).await.unwrap();

    // 2. Verify server-issued patch recipe
    let recipe = neg_resp.patch_recipe.expect("Recipe must be present");
    assert_eq!(recipe.recipe_id, "v2-hotfix");
    assert_eq!(recipe.marker, "/* @patched-kiro-byok v2 */");
    assert_eq!(recipe.needle, "https://runtime.${t}.kiro.dev");

    // 3. Test apply_with_recipe on a synthetic extension.js
    let temp_dir = std::env::temp_dir().join(format!("kiro_recipe_test_{}", std::process::id()));
    fs::create_dir_all(&temp_dir).unwrap();
    let ext_path = temp_dir.join("extension.js");
    fs::write(
        &ext_path,
        "function connect(){ return 'https://runtime.${t}.kiro.dev'; }",
    )
    .unwrap();

    let patcher = ExtensionPatcher::new(&ext_path);
    let patch_res = patcher.apply_with_recipe("http://127.0.0.1:44040", &recipe);
    assert!(patch_res.is_ok());

    let patched_content = fs::read_to_string(&ext_path).unwrap();
    assert!(patched_content.starts_with("/* @patched-kiro-byok v2 */"));
    assert!(patched_content.contains("http://127.0.0.1:44040/custom_endpoint"));

    // 4. Verify UI overrides model filtering and renaming
    let ui = neg_resp.ui_overrides.expect("UI Overrides must be present");
    assert_eq!(ui.brand_name, "KIRO 极速接管定制版");
    assert_eq!(ui.credit_label, "商业算力");

    let mut models = vec![
        ClientModelCatalogItem {
            model_id: "anthropic.claude-3-7-sonnet".to_string(),
            model_name: "Claude 3.7 Sonnet".to_string(),
            description: None,
        },
        ClientModelCatalogItem {
            model_id: "deprecated-model-v1".to_string(),
            model_name: "Old Model".to_string(),
            description: None,
        },
        ClientModelCatalogItem {
            model_id: "deepseek-chat".to_string(),
            model_name: "DeepSeek V3".to_string(),
            description: None,
        },
    ];

    ui.apply_to_models(&mut models);

    // Verify deprecated model is filtered out
    assert_eq!(models.len(), 2);
    assert!(!models.iter().any(|m| m.model_id == "deprecated-model-v1"));

    // Verify Claude was renamed according to server rule
    let claude = models
        .iter()
        .find(|m| m.model_id == "anthropic.claude-3-7-sonnet")
        .unwrap();
    assert_eq!(claude.model_name, "Claude 3.7 Sonnet (极速推理)");

    let _ = fs::remove_dir_all(temp_dir);
}
