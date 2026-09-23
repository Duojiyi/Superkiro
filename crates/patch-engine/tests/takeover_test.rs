//! Integration tests for Kiro takeover, settings safe merge, extension patch, and reversible snapshot.
//!
//! Spec §2.1, §2.5, §9, §14.5, P0-2, P0-3.

use patch_engine::patch::{
    get_launcher_env, ExtensionPatcher, PatchError, PatchStatus, PATCH_MARKER_V1,
    RUNTIME_ENDPOINT_NEEDLE,
};
use patch_engine::settings::SettingsManager;
use patch_engine::snapshot::SnapshotManager;
use serde_json::json;
use std::fs;

#[test]
fn test_settings_safe_merge_and_revert() {
    let temp_dir = std::env::temp_dir().join(format!("kiro_test_settings_{}", std::process::id()));
    let settings_file = temp_dir.join("settings.json");
    fs::create_dir_all(&temp_dir).unwrap();

    // 1. Initial user settings with custom keys
    let initial_user_settings = json!({
        "editor.fontSize": 16,
        "workbench.colorTheme": "Default Dark Modern",
        "update.mode": "manual",
        "customExtension.key": "customValue",
        "http.noProxy": ["existing.example"],
        "http.proxy": "http://localhost:7897"
    });
    fs::write(
        &settings_file,
        serde_json::to_string_pretty(&initial_user_settings).unwrap(),
    )
    .unwrap();

    let mgr = SettingsManager::at(&settings_file);

    // 2. Merge BYOK settings
    let gateway_url = "https://gateway.kiro-byok.test:8080";
    let prior_state = mgr.merge_byok(gateway_url).expect("Merge must succeed");

    assert!(mgr.is_byok_active(Some(gateway_url)));

    let merged = mgr.read_settings().unwrap();
    // Verify user keys were strictly preserved
    assert_eq!(merged.get("editor.fontSize").unwrap(), &json!(16));
    assert_eq!(
        merged.get("workbench.colorTheme").unwrap(),
        &json!("Default Dark Modern")
    );
    assert_eq!(
        merged.get("customExtension.key").unwrap(),
        &json!("customValue")
    );

    // Verify BYOK overrides
    assert_eq!(merged.get("update.mode").unwrap(), &json!("none"));
    assert_eq!(
        merged.get("kiroAgent.enableTabAutocomplete").unwrap(),
        &json!(false)
    );
    assert_eq!(
        merged.get("telemetry.telemetryLevel").unwrap(),
        &json!("off")
    );
    assert!(merged.contains_key("kiroAuthConfig"));
    assert!(merged.contains_key("codewhisperer.config"));

    assert_eq!(
        merged.get("http.noProxy").unwrap(),
        &json!(["existing.example", "gateway.kiro-byok.test"])
    );
    assert_eq!(
        merged.get("http.proxy").unwrap(),
        &json!("http://localhost:7897")
    );
    mgr.merge_byok(gateway_url).unwrap();
    assert_eq!(
        mgr.read_settings().unwrap().get("http.noProxy"),
        merged.get("http.noProxy")
    );
    // 3. Revert to original state
    mgr.revert(&prior_state).expect("Revert must succeed");

    assert!(!mgr.is_byok_active(None));
    let reverted = mgr.read_settings().unwrap();

    // Verify original values restored
    assert_eq!(reverted.get("editor.fontSize").unwrap(), &json!(16));
    assert_eq!(reverted.get("update.mode").unwrap(), &json!("manual"));
    assert!(!reverted.contains_key("kiroAuthConfig"));
    assert!(!reverted.contains_key("codewhisperer.config"));
    assert!(!reverted.contains_key("kiroAgent.enableTabAutocomplete"));

    let _ = fs::remove_dir_all(temp_dir);
}

#[test]
fn test_settings_empty_file_creation_and_revert() {
    let temp_dir =
        std::env::temp_dir().join(format!("kiro_test_nosettings_{}", std::process::id()));
    let settings_file = temp_dir.join("settings.json");

    let mgr = SettingsManager::at(&settings_file);
    assert!(!settings_file.exists());

    let prior_state = mgr.merge_byok("https://gateway.test").unwrap();
    assert!(settings_file.exists());
    assert!(mgr.is_byok_active(None));

    mgr.revert(&prior_state).unwrap();
    // Since file did not exist before BYOK, revert safely removes the file
    assert!(!settings_file.exists());

    let _ = fs::remove_dir_all(temp_dir);
}

#[test]
fn test_launcher_env_generation() {
    let envs = get_launcher_env("https://api.kiro-byok.test:8080/");
    assert_eq!(
        envs.get("KIRO_AUTH_PORTAL_URL").unwrap(),
        "https://api.kiro-byok.test:8080"
    );
    assert_eq!(
        envs.get("AWS_ENDPOINT_URL").unwrap(),
        "https://api.kiro-byok.test:8080"
    );
    assert_eq!(envs.get("KIRO_DISABLE_SESSION_TITLE_LLM").unwrap(), "true");
    assert_eq!(envs.get("KIRO_DISABLE_RECAP").unwrap(), "true");
    assert_eq!(
        envs.get("KIRO_GATEWAY_URL").unwrap(),
        "https://api.kiro-byok.test:8080"
    );
}

#[test]
fn test_extension_patcher_lifecycle() {
    let temp_dir = std::env::temp_dir().join(format!("kiro_test_patch_{}", std::process::id()));
    let ext_file = temp_dir.join("extension.js");
    fs::create_dir_all(&temp_dir).unwrap();

    let original_bundle = format!(
        "// Synthetic bundle\nfunction getEndpoint(t) {{ return \"{}\"; }}\nconsole.log('init');\n",
        RUNTIME_ENDPOINT_NEEDLE
    );
    fs::write(&ext_file, &original_bundle).unwrap();

    let patcher = ExtensionPatcher::new(&ext_file);

    // Initial status: Official
    assert_eq!(patcher.status(), PatchStatus::Official);
    assert!(patcher.dry_run().unwrap());

    // 1. Apply patch
    patcher.apply("https://my-byok-gateway.test").unwrap();
    assert_eq!(patcher.status(), PatchStatus::Patched);
    assert!(patcher.backup_path().exists());

    let patched_content = fs::read_to_string(&ext_file).unwrap();
    assert!(patched_content.starts_with(PATCH_MARKER_V1));
    assert!(patched_content.contains("process.env.KIRO_GATEWAY_URL"));

    // 2. Re-applying is idempotent
    patcher.apply("https://my-byok-gateway.test").unwrap();
    assert_eq!(patcher.status(), PatchStatus::Patched);

    // 3. Simulate official Kiro upgrade overwriting extension.js
    let upgraded_bundle = format!(
        "// Upgraded bundle v1.0.438\nfunction getEndpoint(t) {{ return \"{}\"; }}\n",
        RUNTIME_ENDPOINT_NEEDLE
    );
    fs::write(&ext_file, &upgraded_bundle).unwrap();

    // Now backup exists but file lacks marker -> UpgradeDetected
    assert_eq!(patcher.status(), PatchStatus::UpgradeDetected);

    // An unknown upgraded digest must fail closed, preserving recovery evidence.
    assert!(matches!(
        patcher.restore(),
        Err(PatchError::ExtensionChanged)
    ));
    assert_eq!(patcher.status(), PatchStatus::UpgradeDetected);
    assert!(patcher.backup_path().exists());
    assert_eq!(fs::read_to_string(&ext_file).unwrap(), upgraded_bundle);
    assert_eq!(
        fs::read_to_string(patcher.backup_path()).unwrap(),
        original_bundle
    );
    assert!(patcher.apply("https://my-byok-gateway.test").is_err());

    let _ = fs::remove_dir_all(temp_dir);
}

#[test]
fn test_snapshot_takeover_and_one_click_official_restore() {
    let temp_dir = std::env::temp_dir().join(format!("kiro_test_snapshot_{}", std::process::id()));
    let settings_file = temp_dir.join("settings.json");
    let ext_file = temp_dir.join("extension.js");
    let snapshot_file = temp_dir.join("snapshot.json");
    fs::create_dir_all(&temp_dir).unwrap();

    // Setup initial files
    fs::write(
        &settings_file,
        r#"{"editor.tabSize": 2, "update.mode": "manual"}"#,
    )
    .unwrap();
    let original_ext = format!("const endpoint = \"{}\";", RUNTIME_ENDPOINT_NEEDLE);
    fs::write(&ext_file, &original_ext).unwrap();

    let settings_mgr = SettingsManager::at(&settings_file);
    let patcher = ExtensionPatcher::new(&ext_file);
    let snapshot_mgr = SnapshotManager::at(&snapshot_file);

    assert!(!snapshot_mgr.has_active_snapshot());

    // Execute Takeover
    let gw = "https://gw.takeover.test";
    let snapshot = snapshot_mgr
        .takeover(&settings_mgr, Some(&patcher), gw)
        .unwrap();
    assert_eq!(snapshot.gateway_url, gw);
    assert!(snapshot_mgr.has_active_snapshot());
    assert_eq!(patcher.status(), PatchStatus::Patched);
    assert!(settings_mgr.is_byok_active(Some(gw)));

    // Execute One-click Restore
    let summary = snapshot_mgr.restore_official().unwrap();
    assert!(summary.settings_restored);
    assert!(summary.extension_restored);
    assert!(summary.snapshot_removed);
    assert!(!snapshot_mgr.has_active_snapshot());

    // Verify file contents are 100% pristine official
    assert_eq!(patcher.status(), PatchStatus::Official);
    let final_settings = settings_mgr.read_settings().unwrap();
    assert_eq!(final_settings.get("editor.tabSize").unwrap(), &json!(2));
    assert_eq!(final_settings.get("update.mode").unwrap(), &json!("manual"));
    assert!(!final_settings.contains_key("kiroAuthConfig"));
    assert!(!final_settings.contains_key("codewhisperer.config"));

    let final_ext = fs::read_to_string(&ext_file).unwrap();
    assert_eq!(final_ext, original_ext);

    let _ = fs::remove_dir_all(temp_dir);
}

#[test]
fn test_settings_revert_preserves_newly_added_user_keys() {
    let temp_dir = std::env::temp_dir().join(format!("kiro_test_user_keys_{}", std::process::id()));
    let settings_file = temp_dir.join("settings.json");
    fs::create_dir_all(&temp_dir).unwrap();

    let mgr = SettingsManager::at(&settings_file);

    // Initial state: no settings file existed
    let prior_state = mgr.merge_byok("https://gateway.test").unwrap();
    assert!(settings_file.exists());

    // While BYOK was active, the user opened settings and added their custom key:
    let mut current_map = mgr.read_settings().unwrap();
    current_map.insert("user.customPreference".to_string(), json!("hello-world"));
    fs::write(
        &settings_file,
        serde_json::to_string_pretty(&current_map).unwrap(),
    )
    .unwrap();

    // Now restore official settings
    mgr.revert(&prior_state).unwrap();

    // File should NOT have been deleted; user.customPreference should be preserved!
    assert!(settings_file.exists());
    let restored = mgr.read_settings().unwrap();
    assert_eq!(
        restored.get("user.customPreference").unwrap(),
        &json!("hello-world")
    );
    assert!(!restored.contains_key("kiroAuthConfig"));
    assert!(!restored.contains_key("codewhisperer.config"));

    let _ = fs::remove_dir_all(temp_dir);
}

#[test]
fn test_url_injection_rejected() {
    let temp_dir = std::env::temp_dir().join(format!("kiro_test_injection_{}", std::process::id()));
    let ext_file = temp_dir.join("extension.js");
    fs::create_dir_all(&temp_dir).unwrap();

    let original_bundle = format!("const endpoint = \"{}\";", RUNTIME_ENDPOINT_NEEDLE);
    fs::write(&ext_file, &original_bundle).unwrap();

    let patcher = ExtensionPatcher::new(&ext_file);

    // Injection attempts with quotes, newlines, spaces
    let evil_urls = [
        "https://evil.com\"); malicious_code(); //",
        "https://evil.com\nmalicious()",
        "https://evil.com\" || true",
        "ftp://invalid-scheme.com",
    ];

    for evil in evil_urls {
        let err = patcher.apply(evil).unwrap_err();
        match err {
            patch_engine::PatchError::InvalidUrl(_) => {}
            other => panic!("Expected InvalidUrl error for '{}', got: {:?}", evil, other),
        }
    }

    // File should remain untouched
    assert_eq!(patcher.status(), PatchStatus::Official);
    let current_content = fs::read_to_string(&ext_file).unwrap();
    assert_eq!(current_content, original_bundle);

    let _ = fs::remove_dir_all(temp_dir);
}

#[test]
fn test_fake_kiro_directory_hash_integrity() {
    use ring::digest::{Context, SHA256};

    let temp_dir = std::env::temp_dir().join(format!("kiro_test_fake_dir_{}", std::process::id()));
    let settings_file = temp_dir.join("settings.json");
    let ext_file = temp_dir.join("extension.js");
    let snapshot_file = temp_dir.join("snapshot.json");
    fs::create_dir_all(&temp_dir).unwrap();

    let orig_settings_content = r#"{"editor.fontSize": 14, "update.mode": "manual"}"#;
    let orig_ext_content = format!(
        "function getEndpoint() {{ return \"{}\"; }}\nconsole.log('boot');",
        RUNTIME_ENDPOINT_NEEDLE
    );

    fs::write(&settings_file, orig_settings_content).unwrap();
    fs::write(&ext_file, &orig_ext_content).unwrap();

    let hash_content = |data: &[u8]| -> String {
        let mut ctx = Context::new(&SHA256);
        ctx.update(data);
        ctx.finish()
            .as_ref()
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect()
    };

    let orig_settings_hash = hash_content(orig_settings_content.as_bytes());
    let orig_ext_hash = hash_content(orig_ext_content.as_bytes());

    let settings_mgr = SettingsManager::at(&settings_file);
    let patcher = ExtensionPatcher::new(&ext_file);
    let snapshot_mgr = SnapshotManager::at(&snapshot_file);

    // 1. Takeover
    let snap = snapshot_mgr
        .takeover(
            &settings_mgr,
            Some(&patcher),
            "https://gateway.secure.local:44040",
        )
        .expect("Takeover must succeed");
    assert_eq!(snap.gateway_url, "https://gateway.secure.local:44040");

    // Hashes must be modified
    let modified_settings = fs::read(&settings_file).unwrap();
    let modified_ext = fs::read(&ext_file).unwrap();
    assert_ne!(hash_content(&modified_settings), orig_settings_hash);
    assert_ne!(hash_content(&modified_ext), orig_ext_hash);

    // 2. Restore official
    let summary = snapshot_mgr
        .restore_official()
        .expect("Restore must succeed");
    assert!(summary.settings_restored);
    assert!(summary.extension_restored);
    assert!(summary.snapshot_removed);

    // Exact byte-for-byte hash check
    let restored_settings = fs::read(&settings_file).unwrap();
    let restored_ext = fs::read(&ext_file).unwrap();
    assert_eq!(
        hash_content(&restored_settings),
        orig_settings_hash,
        "Settings content hash must match original byte-for-byte"
    );
    assert_eq!(
        hash_content(&restored_ext),
        orig_ext_hash,
        "Extension bundle hash must match original byte-for-byte"
    );

    let _ = fs::remove_dir_all(temp_dir);
}

#[test]
fn test_single_instance_lock_reentrancy_and_collision() {
    let temp_dir = std::env::temp_dir().join(format!("kiro_test_lock_{}", std::process::id()));
    let lock_file = temp_dir.join("kiro.lock");
    fs::create_dir_all(&temp_dir).unwrap();

    // First lock acquisition succeeds
    let lock1 = patch_engine::SingleInstanceLock::acquire(Some(&lock_file)).unwrap();
    assert!(lock_file.exists());

    // Same-process acquisition is intentionally reentrant.
    let lock2 = patch_engine::SingleInstanceLock::acquire(Some(&lock_file)).unwrap();
    drop(lock2);
    assert!(lock_file.exists());

    // Drop first lock -> file deleted cleanly
    drop(lock1);
    assert!(!lock_file.exists());

    // Subsequent lock acquisition succeeds
    let lock3 = patch_engine::SingleInstanceLock::acquire(Some(&lock_file)).unwrap();
    assert!(lock_file.exists());
    drop(lock3);
    assert!(!lock_file.exists());

    let _ = fs::remove_dir_all(temp_dir);
}

#[test]
fn test_old_snapshot_migrates_proxy_bypass_and_restores_user_rules() {
    let dir = std::env::temp_dir().join(format!("kiro_proxy_migrate_{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let settings = SettingsManager::at(dir.join("settings.json"));
    fs::write(
        settings.path(),
        r#"{"editor.fontSize":16,"http.noProxy":["original.example"]}"#,
    )
    .unwrap();
    let snapshots = SnapshotManager::at(dir.join("snapshot.json"));
    let gw = "https://160.202.47.98";
    snapshots.takeover(&settings, None, gw).unwrap();
    // Simulate a pre-fix snapshot, plus a user's proxy preference edited since takeover.
    let mut old = snapshots.load().unwrap();
    old.settings_state.proxy_bypass_managed = false;
    old.settings_state.prior_values.remove("http.noProxy");
    snapshots.save(&old).unwrap();
    let mut live = settings.read_settings().unwrap();
    live.insert("http.noProxy".into(), json!(["current.example"]));
    fs::write(settings.path(), serde_json::to_vec(&live).unwrap()).unwrap();
    snapshots.takeover(&settings, None, gw).unwrap();
    snapshots.takeover(&settings, None, gw).unwrap();
    assert_eq!(
        settings.read_settings().unwrap()["http.noProxy"],
        json!(["current.example", "160.202.47.98"])
    );
    snapshots.restore_official().unwrap();
    let restored = settings.read_settings().unwrap();
    assert_eq!(restored["http.noProxy"], json!(["current.example"]));
    assert_eq!(restored["editor.fontSize"], json!(16));
    for file in ["settings.json", "snapshot.json"] {
        let _ = fs::remove_file(dir.join(file));
    }
    let _ = fs::remove_dir(dir);
}

/// The mirror case: when the patch is already gone, an unrestorable extension
/// must not block the settings revert. Otherwise a customer whose Kiro updated
/// itself mid-takeover is left with gateway-pointing settings, a stock
/// extension, and no way out of either through the product.
#[test]
fn an_unrestorable_extension_does_not_block_the_settings_revert() {
    let temp_dir = std::env::temp_dir().join(format!(
        "kiro_test_restore_escape_{}",
        std::process::id()
    ));
    let settings_file = temp_dir.join("settings.json");
    let ext_file = temp_dir.join("extension.js");
    let snapshot_file = temp_dir.join("snapshot.json");
    fs::create_dir_all(&temp_dir).unwrap();
    fs::write(
        &settings_file,
        r#"{"editor.tabSize": 2, "update.mode": "manual"}"#,
    )
    .unwrap();
    let original_ext = format!("const endpoint = \"{}\";", RUNTIME_ENDPOINT_NEEDLE);
    fs::write(&ext_file, &original_ext).unwrap();

    let settings_mgr = SettingsManager::at(&settings_file);
    let patcher = ExtensionPatcher::new(&ext_file);
    let snapshot_mgr = SnapshotManager::at(&snapshot_file);
    snapshot_mgr
        .takeover(&settings_mgr, Some(&patcher), "https://gw.escape.test")
        .unwrap();

    // Kiro upgraded itself over the patch: the marker is gone and the backup no
    // longer describes what is on disk.
    fs::write(&ext_file, "const endpoint = \"https://upgraded.example\";").unwrap();
    assert_eq!(patcher.status(), PatchStatus::UpgradeDetected);

    let summary = snapshot_mgr
        .restore_official()
        .expect("settings must still be recoverable");
    assert!(summary.settings_restored);
    assert!(!summary.extension_restored);
    assert!(
        summary.extension_unrestorable.is_some(),
        "the customer must be told the extension could not be rolled back"
    );

    let after = settings_mgr.read_settings().unwrap();
    assert_eq!(after.get("update.mode"), Some(&json!("manual")));
    assert!(!after.contains_key("kiroAuthConfig"));
    assert!(!after.contains_key("codewhisperer.config"));

    let _ = fs::remove_dir_all(temp_dir);
}
