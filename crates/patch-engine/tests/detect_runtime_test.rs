//! Tests for Kiro installation detection, versioning, runtime inspection, and single-instance lock.
//!
//! Spec §9, P0-2, P0-7.

use patch_engine::detect::{get_candidate_install_paths, inspect_installation_dir, DetectError};
use patch_engine::runtime::{is_kiro_running, is_pid_alive, SingleInstanceLock};
use std::fs;

#[test]
fn test_candidate_paths_are_available_without_reading_host_installation() {
    assert!(!get_candidate_install_paths().is_empty());
}

#[test]
fn test_inspect_synthetic_sandbox_installation() {
    let sandbox_dir =
        std::env::temp_dir().join(format!("kiro_test_sandbox_{}", std::process::id()));
    let app_dir = if cfg!(target_os = "macos") {
        sandbox_dir.join("Contents/Resources/app")
    } else {
        sandbox_dir.join("resources/app")
    };
    let agent_dir = app_dir.join("extensions").join("kiro.kiro-agent");
    fs::create_dir_all(&agent_dir).unwrap();

    // 1. Write product.json
    let product_json = serde_json::json!({
        "vsCodeVersion": "1.109.5",
        "nameShort": "Kiro",
        "win32MutexName": "kiro",
        "quality": "stable"
    });
    fs::write(
        app_dir.join("product.json"),
        serde_json::to_string_pretty(&product_json).unwrap(),
    )
    .unwrap();

    // 2. Write package.json
    let package_json = serde_json::json!({
        "name": "Kiro",
        "version": "1.0.437",
        "distro": "commit-hash-abc-123"
    });
    fs::write(
        app_dir.join("package.json"),
        serde_json::to_string_pretty(&package_json).unwrap(),
    )
    .unwrap();

    // 3. Write agent package.json
    let agent_package_json = serde_json::json!({
        "name": "kiroAgent",
        "version": "1.0.794"
    });
    fs::write(
        agent_dir.join("package.json"),
        serde_json::to_string_pretty(&agent_package_json).unwrap(),
    )
    .unwrap();

    // Inspect synthetic dir
    let installation =
        inspect_installation_dir(&sandbox_dir).expect("Synthetic detection must succeed");
    assert_eq!(installation.version, "1.0.437");
    assert_eq!(installation.vscode_version.as_deref(), Some("1.109.5"));
    assert_eq!(installation.commit.as_deref(), Some("commit-hash-abc-123"));
    assert_eq!(installation.quality.as_deref(), Some("stable"));
    assert_eq!(installation.win32_mutex_name, "kiro");
    assert_eq!(installation.agent_version.as_deref(), Some("1.0.794"));

    // Cleanup
    let _ = fs::remove_dir_all(sandbox_dir);
}

#[test]
fn test_invalid_installation_detection() {
    let empty_dir = std::env::temp_dir().join(format!("kiro_empty_{}", std::process::id()));
    fs::create_dir_all(&empty_dir).unwrap();

    let res = inspect_installation_dir(&empty_dir);
    assert!(matches!(res, Err(DetectError::InvalidInstallation(_))));

    let _ = fs::remove_dir_all(empty_dir);
}

#[test]
fn test_runtime_kiro_detection_no_panic() {
    // Calling runtime detection must return a bool without panicking
    let _running = is_kiro_running();
}

#[test]
fn test_single_instance_lock_contention_and_stale_recovery() {
    let lock_file =
        std::env::temp_dir().join(format!("kiro_p3_2_lock_{}.lock", std::process::id()));

    // 1. Acquire lock
    let lock = SingleInstanceLock::acquire(Some(&lock_file)).expect("Must acquire lock");
    assert!(lock_file.exists());

    // 2. Competing acquisition with our own active PID returns re-entrant success
    let lock_reentrant = SingleInstanceLock::acquire(Some(&lock_file));
    assert!(lock_reentrant.is_ok());

    drop(lock_reentrant);
    drop(lock);

    assert!(!lock_file.exists());

    // 3. Stale PID recovery
    // Fictitious dead PID
    fs::write(&lock_file, "99999999").unwrap();
    assert!(!is_pid_alive(99999999));

    let recovered_lock =
        SingleInstanceLock::acquire(Some(&lock_file)).expect("Must recover stale dead PID lock");
    assert!(lock_file.exists());
    drop(recovered_lock);
    assert!(!lock_file.exists());
}
