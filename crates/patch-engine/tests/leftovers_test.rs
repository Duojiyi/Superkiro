//! A takeover whose rollback records are lost is still found from the files it changed,
//! and undone without them.
use patch_engine::patch::{ExtensionPatcher, PatchOwnership, PatchStatus, RUNTIME_ENDPOINT_NEEDLE};
use patch_engine::settings::SettingsManager;
use patch_engine::token_storage::{KiroAuthToken, TokenStorage};
use patch_engine::Leftovers;
use serde_json::json;
use std::fs;
use std::path::PathBuf;

const GATEWAY: &str = "https://my-byok-gateway.test";

struct Machine {
    dir: PathBuf,
    extension: PathBuf,
    settings: SettingsManager,
    token: TokenStorage,
}

impl Machine {
    /// A machine taken over, whose snapshot and session were then lost.
    fn taken_over(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "leftovers-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        let extension = dir.join("extension.js");
        fs::write(
            &extension,
            format!("// bundle\nfunction e(t) {{ return \"{RUNTIME_ENDPOINT_NEEDLE}\"; }}\n"),
        )
        .unwrap();
        ExtensionPatcher::new(&extension).apply(GATEWAY).unwrap();
        let settings = SettingsManager::at(dir.join("settings.json"));
        fs::write(
            settings.path(),
            "{\n  // mine\n  \"editor.fontSize\": 15\n}\n",
        )
        .unwrap();
        settings.merge_byok(GATEWAY).unwrap();
        let token = TokenStorage::at(dir.join("kiro-auth-token.json"));
        token
            .save(&KiroAuthToken {
                access_token: "access".into(),
                refresh_token: "refresh".into(),
                profile_arn: "arn:aws:codewhisperer:us-east-1:123456789012:profile/group".into(),
                expires_at: "2030-01-01T00:00:00.000Z".into(),
                auth_method: "social".into(),
                provider: "Google".into(),
            })
            .unwrap();
        Self {
            dir,
            extension,
            settings,
            token,
        }
    }

    fn scan(&self) -> Leftovers {
        Leftovers::scan(
            std::slice::from_ref(&self.extension),
            &self.settings,
            &self.token,
            &["my-byok-gateway.test".to_string()],
        )
    }

    fn remove(&self, found: &Leftovers) -> Result<(), String> {
        found.remove(
            &self.settings,
            &self.token,
            &["my-byok-gateway.test".to_string()],
        )
    }
}

impl Drop for Machine {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn a_takeover_whose_records_are_lost_is_found_and_undone() {
    let machine = Machine::taken_over("lost");
    let found = machine.scan();
    assert_eq!(found.patches, vec![machine.extension.clone()]);
    assert!(found.unrecoverable.is_empty());
    assert!(found.settings && found.token && found.found());

    machine.remove(&found).unwrap();

    let patcher = ExtensionPatcher::new(&machine.extension);
    assert_eq!(patcher.status(), PatchStatus::Official);
    let settings = machine.settings.read_settings().unwrap();
    for key in [
        "kiroAuthConfig",
        "codewhisperer.config",
        "http.noProxy",
        "update.mode",
    ] {
        assert!(!settings.contains_key(key), "{key} left: {settings:?}");
    }
    assert_eq!(settings["editor.fontSize"], json!(15));
    assert!(fs::read_to_string(machine.settings.path())
        .unwrap()
        .contains("// mine"));
    assert!(
        machine.token.load().is_err(),
        "the gateway's token is cleared"
    );
    assert!(!machine.scan().found());
}

#[test]
fn a_patch_whose_backup_is_gone_needs_a_reinstall_but_the_rest_is_still_undone() {
    let machine = Machine::taken_over("unrecoverable");
    let patcher = ExtensionPatcher::new(&machine.extension);
    fs::remove_file(patcher.backup_path()).unwrap();
    fs::remove_file(machine.extension.with_extension("js.kpatch-state")).unwrap();

    let found = machine.scan();
    assert!(found.patches.is_empty());
    assert_eq!(found.unrecoverable, vec![machine.extension.clone()]);

    let error = machine.remove(&found).unwrap_err();
    assert!(error.contains("reinstall Kiro"), "{error}");
    // What sends the customer's own traffic to the gateway is gone regardless, and
    // Kiro's updater is released so an update can replace the patched bundle.
    let settings = machine.settings.read_settings().unwrap();
    assert!(!settings.contains_key("kiroAuthConfig"));
    assert!(!settings.contains_key("update.mode"));
    assert!(machine.token.load().is_err());
    assert_eq!(patcher.status(), PatchStatus::Patched);
}

#[test]
fn another_users_patch_on_a_shared_install_is_left_alone() {
    let machine = Machine::taken_over("shared");
    let state_path = machine.extension.with_extension("js.kpatch-state");
    let mut state: serde_json::Value =
        serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
    assert!(state["owner"].is_string(), "the patch records its owner");
    state["owner"] = json!("c:\\users\\someone-else");
    fs::write(&state_path, serde_json::to_vec(&state).unwrap()).unwrap();

    let patcher = ExtensionPatcher::new(&machine.extension);
    assert_eq!(patcher.ownership(), PatchOwnership::Theirs);
    let found = machine.scan();
    assert!(found.patches.is_empty() && found.unrecoverable.is_empty());
    machine.remove(&found).unwrap();
    assert_eq!(
        patcher.status(),
        PatchStatus::Patched,
        "never undone by this user"
    );
}

#[test]
fn settings_that_name_another_endpoint_are_not_ours() {
    let machine = Machine::taken_over("foreign");
    fs::write(
        machine.settings.path(),
        r#"{"kiroAuthConfig": {"portalUrl": "https://sso.company.example"}, "update.mode": "none"}"#,
    )
    .unwrap();
    let found = machine.scan();
    assert!(!found.settings);
    machine.remove(&found).unwrap();
    let settings = machine.settings.read_settings().unwrap();
    assert!(settings.contains_key("kiroAuthConfig"));
    assert_eq!(settings["update.mode"], json!("none"));
}

/// A settings.json with a syntax error that still names the gateway is a leftover: Kiro
/// may well still act on it. Removing it fails before any file changes and names the
/// spot to fix; once fixed, the same cleanup completes.
#[test]
fn settings_with_a_syntax_error_are_found_and_nothing_changes_until_they_are_fixed() {
    let machine = Machine::taken_over("syntax");
    let taken = fs::read_to_string(machine.settings.path()).unwrap();
    let broken = format!("{taken}}}\n");
    fs::write(machine.settings.path(), &broken).unwrap();

    let found = machine.scan();
    assert!(
        found.settings,
        "blind to settings that still name the gateway"
    );
    let error = machine.remove(&found).unwrap_err();
    assert!(
        error.contains("settings.json has a syntax error at line"),
        "{error}"
    );
    let patcher = ExtensionPatcher::new(&machine.extension);
    assert_eq!(
        patcher.status(),
        PatchStatus::Patched,
        "the bundle was touched"
    );
    assert!(machine.token.load().is_ok(), "the token was touched");
    assert_eq!(fs::read_to_string(machine.settings.path()).unwrap(), broken);

    fs::write(machine.settings.path(), &taken).unwrap();
    machine.remove(&machine.scan()).unwrap();
    assert!(!machine.scan().found());
    assert_eq!(patcher.status(), PatchStatus::Official);
}

/// A missing comma Kiro reads past neither hides the leftover settings nor blocks the
/// cleanup.
#[test]
fn a_missing_comma_does_not_hide_leftover_settings() {
    let machine = Machine::taken_over("missing-comma");
    let taken = fs::read_to_string(machine.settings.path()).unwrap();
    assert!(taken.contains("\"editor.fontSize\": 15,"), "{taken}");
    fs::write(
        machine.settings.path(),
        taken.replacen("\"editor.fontSize\": 15,", "\"editor.fontSize\": 15", 1),
    )
    .unwrap();

    let found = machine.scan();
    assert!(found.settings);
    machine.remove(&found).unwrap();
    assert!(!machine.scan().found());
    let settings = fs::read_to_string(machine.settings.path()).unwrap();
    assert!(!settings.contains("kiroAuthConfig"), "{settings}");
    assert!(settings.contains("// mine"), "{settings}");
}

#[test]
fn the_marker_check_reads_only_the_start_of_the_bundle() {
    let machine = Machine::taken_over("marker");
    let patcher = ExtensionPatcher::new(&machine.extension);
    assert!(patcher.marker_present().unwrap());
    fs::write(&machine.extension, "// official bundle").unwrap();
    assert!(!patcher.marker_present().unwrap());
    fs::write(&machine.extension, "/* @patched").unwrap();
    assert!(
        !patcher.marker_present().unwrap(),
        "a shorter file is not marked"
    );
    fs::remove_file(&machine.extension).unwrap();
    assert!(patcher.marker_present().is_err());
}
