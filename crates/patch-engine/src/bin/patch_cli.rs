//! CLI entrypoint for desktop runner and scripts to execute patch-engine operations.

use patch_engine::{
    detect_kiro, detect_kiro_process_state, ExtensionPatcher, MemoryGuard, ProcessState,
    SettingsManager, SnapshotManager,
};
use serde_json::json;
use std::env;
use std::io::Read;

#[tokio::main]
async fn main() {
    let args: Vec<String> = env::args().collect();
    let subcommand = args.get(1).map(|s| s.as_str()).unwrap_or("help");

    match subcommand {
        "desktop-activate" | "desktop-unbind" | "desktop-logout" | "desktop-launch" => {
            let result = desktop_operation(subcommand, &args).await;
            match result {
                Ok(()) => println!("{}", json!({"success": true})),
                Err(e) => {
                    println!("{}", json!({"success": false, "error": e}));
                    std::process::exit(1);
                }
            }
        }
        "desktop-usage" => {
            let result = match patch_engine::desktop::DesktopSession::system() {
                Ok(session) => session.usage().await,
                Err(error) => Err(error),
            };
            match result {
                Ok(usage) => println!("{}", desktop_usage_output(usage)),
                Err(error) => {
                    println!("{}", json!({"success": false, "error": error}));
                    std::process::exit(1);
                }
            }
        }
        "status" => {
            let proc_state = detect_kiro_process_state();
            let install = detect_kiro(None).ok();
            let snapshot_mgr = SnapshotManager::default();
            let has_snapshot = snapshot_mgr.has_active_snapshot();

            let out = json!({
                "process_state": proc_state.to_string(),
                "kiro_installed": install.is_some(),
                "kiro_install_path": install.as_ref().map(|i| &i.install_dir),
                "kiro_version": install.as_ref().map(|i| &i.version),
                "has_snapshot": has_snapshot,
                "authenticated": patch_engine::desktop::DesktopSession::system().map(|s| s.authenticated()).unwrap_or(false),
                "gateway_url": patch_engine::desktop::DesktopSession::system().ok().and_then(|s| s.gateway()),
            });
            println!("{}", out);
        }
        "activate" => {
            let mut gw = "http://127.0.0.1:44040".to_string();
            let mut i = 2;
            while i < args.len() {
                if args[i] == "--gateway-url" && i + 1 < args.len() {
                    gw = args[i + 1].clone();
                    i += 1;
                }
                i += 1;
            }

            match detect_kiro_process_state() {
                ProcessState::Running => {
                    let out = json!({
                        "success": false,
                        "error": "Kiro is running. Please close Kiro before activating."
                    });
                    println!("{}", out);
                    std::process::exit(1);
                }
                ProcessState::Unknown => {
                    let out = json!({
                        "success": false,
                        "error": "Cannot safely verify Kiro process state."
                    });
                    println!("{}", out);
                    std::process::exit(1);
                }
                ProcessState::Stopped => {}
            }

            let install = detect_kiro(None).ok();
            let patcher = install.as_ref().and_then(|inst| {
                inst.agent_extension_dir
                    .as_ref()
                    .map(|dir| ExtensionPatcher::new(dir.join("dist").join("extension.js")))
            });

            let settings_mgr = SettingsManager::default();
            let snapshot_mgr = SnapshotManager::default();

            match snapshot_mgr.takeover(&settings_mgr, patcher.as_ref(), &gw) {
                Ok(snap) => {
                    let out = json!({
                        "success": true,
                        "snapshot": snap
                    });
                    println!("{}", out);
                }
                Err(e) => {
                    let out = json!({
                        "success": false,
                        "error": e.to_string()
                    });
                    println!("{}", out);
                    std::process::exit(1);
                }
            }
        }
        "restore" => {
            match detect_kiro_process_state() {
                ProcessState::Running => {
                    let out = json!({
                        "success": false,
                        "error": "Kiro is running. Please close Kiro before restoring."
                    });
                    println!("{}", out);
                    std::process::exit(1);
                }
                ProcessState::Unknown => {
                    let out = json!({
                        "success": false,
                        "error": "Cannot safely verify Kiro process state."
                    });
                    println!("{}", out);
                    std::process::exit(1);
                }
                ProcessState::Stopped => {}
            }

            let snapshot_mgr = SnapshotManager::default();
            match snapshot_mgr.restore_official() {
                Ok(summary) => {
                    let out = json!({
                        "success": true,
                        "summary": summary
                    });
                    println!("{}", out);
                }
                Err(e) => {
                    let out = json!({
                        "success": false,
                        "error": e.to_string()
                    });
                    println!("{}", out);
                    std::process::exit(1);
                }
            }
        }
        "doctor" => {
            let mut gw = "http://127.0.0.1:44040".to_string();
            let mut i = 2;
            while i < args.len() {
                if args[i] == "--gateway-url" && i + 1 < args.len() {
                    gw = args[i + 1].clone();
                    i += 1;
                }
                i += 1;
            }
            let doctor = patch_engine::Doctor::default();
            let report = doctor.diagnose(&gw, None).await;
            println!("{}", serde_json::to_string(&report).unwrap_or_default());
        }
        "trim-memory" => {
            let res = MemoryGuard::trim_working_set(None);
            println!("{}", serde_json::to_string(&res).unwrap_or_default());
        }
        "purge-orphans" => {
            let res = MemoryGuard::purge_orphan_processes();
            println!("{}", serde_json::to_string(&res).unwrap_or_default());
        }
        "sample-memory" => {
            let res = MemoryGuard::sample_memory();
            if let Some(error) = &res.error {
                println!("{}", json!({"success": false, "error": error}));
                std::process::exit(1);
            }
            println!("{}", serde_json::to_string(&res).unwrap_or_default());
        }
        _ => {
            eprintln!("Usage: patch-cli <status|activate|restore|doctor|trim-memory|purge-orphans|sample-memory>");
            std::process::exit(2);
        }
    }
}

fn arg_value(args: &[String], name: &str) -> Option<String> {
    args.windows(2)
        .find(|pair| pair[0] == name)
        .map(|pair| pair[1].clone())
}

async fn desktop_operation(operation: &str, args: &[String]) -> Result<(), String> {
    let session = patch_engine::desktop::DesktopSession::system()?;
    let snapshots = SnapshotManager::default();
    if operation == "desktop-launch" {
        return session.launch(&detect_kiro(None).map_err(|e| e.to_string())?);
    }
    if operation == "desktop-logout" {
        return session.restore_and_logout(&snapshots);
    }
    // Credentials travel only over stdin, never process argv or stdout.
    let mut raw = String::new();
    std::io::stdin()
        .take(16 * 1024)
        .read_to_string(&mut raw)
        .map_err(|e| e.to_string())?;
    let request: serde_json::Value =
        serde_json::from_str(&raw).map_err(|_| "Invalid desktop request".to_string())?;
    let card = request
        .get("card_key")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if operation == "desktop-unbind" {
        return session.unbind(&snapshots, card).await;
    }
    let gateway = arg_value(args, "--gateway-url").ok_or("Gateway URL is required")?;
    let install = detect_kiro(None).map_err(|e| e.to_string())?;
    let close_confirmed = request
        .get("close_kiro_confirmed")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    session
        .activate_and_launch(&install, &gateway, card, close_confirmed)
        .await
}

// Preserve the gateway payload while exposing the desktop's top-level statistics contract.
fn desktop_usage_output(usage: serde_json::Value) -> serde_json::Value {
    json!({"success": true, "settledUsage": usage.get("settledUsage"), "usage": usage})
}

#[cfg(test)]
mod usage_output_tests {
    use super::*;

    #[test]
    fn forwards_real_statistics_and_live_entitlement() {
        let usage = json!({"virtualPlanName":"PRO", "validUntil":1234,
            "settledUsage":{"totalTokens":12,"todayPoints":0.5,"todayTokens":12,
                "daily":[{"date":"2026-09-19","points":0.5,"tokens":12}],
                "models":[{"name":"model","points":0.5,"tokens":12}]}});
        let output = desktop_usage_output(usage.clone());
        assert_eq!(output["success"], true);
        assert_eq!(output["settledUsage"], usage["settledUsage"]);
        assert_eq!(output["usage"], usage);
        assert!(output["settledUsage"].get("usd").is_none());
        assert!(output["settledUsage"].get("referencePrice").is_none());
    }

    #[test]
    fn old_gateway_does_not_fabricate_zero_statistics() {
        let output = desktop_usage_output(json!({"usageBreakdownList":[]}));
        assert!(output["settledUsage"].is_null());
    }
}
