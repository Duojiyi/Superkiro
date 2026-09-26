//! Kiro's other profiles read settings of their own. A takeover configures each one as it
//! does Default's and records it; a restore puts every file back as it was.
use patch_engine::patch::{ExtensionPatcher, PatchStatus, RUNTIME_ENDPOINT_NEEDLE};
use patch_engine::settings::SettingsManager;
use patch_engine::snapshot::SnapshotManager;
use patch_engine::token_storage::TokenStorage;
use patch_engine::Leftovers;
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;

const GATEWAY: &str = "https://gw.profiles.test";
const DEFAULT_SETTINGS: &str = "{\n  \"editor.fontSize\": 14\n}\n";
/// Work's own settings, with a comment, tabs and CRLF line endings.
const WORK_SETTINGS: &str = "{\r\n\t// mine\r\n\t\"editor.fontSize\": 16\r\n}\r\n";
/// Work has settings of its own, Play has no folder yet, and Shared uses Default's.
const STORAGE: &str = r#"{
  "userDataProfiles": [
    {"location": "-5c1a0b", "name": "Work"},
    {"location": "31d9f2", "name": "Play"},
    {"location": "-77b0aa", "name": "Shared", "useDefaultFlags": {"settings": true, "keybindings": true}}
  ],
  "profileAssociations": {
    "workspaces": {"file:///d%3A/work": "-5c1a0b", "file:///d%3A/shared": "-77b0aa"},
    "emptyWindows": {"1": "__default__profile__"}
  }
}"#;

/// Kiro's user data and a synthetic bundle, in a directory of their own.
struct Machine {
    root: PathBuf,
    settings: SettingsManager,
    patcher: ExtensionPatcher,
    snapshots: SnapshotManager,
}

impl Machine {
    fn new(name: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("kiro-profiles-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let user = root.join("User");
        fs::create_dir_all(user.join("globalStorage")).unwrap();
        fs::write(user.join("globalStorage").join("storage.json"), STORAGE).unwrap();
        fs::write(user.join("settings.json"), DEFAULT_SETTINGS).unwrap();
        fs::create_dir_all(user.join("profiles").join("-5c1a0b")).unwrap();
        fs::write(
            user.join("profiles").join("-5c1a0b").join("settings.json"),
            WORK_SETTINGS,
        )
        .unwrap();
        fs::create_dir_all(user.join("profiles").join("-77b0aa")).unwrap();
        fs::write(
            user.join("profiles")
                .join("-77b0aa")
                .join("extensions.json"),
            "[]",
        )
        .unwrap();
        fs::write(
            root.join("extension.js"),
            format!("const e = \"{RUNTIME_ENDPOINT_NEEDLE}\";"),
        )
        .unwrap();
        Self {
            settings: SettingsManager::at(user.join("settings.json")),
            patcher: ExtensionPatcher::new(root.join("extension.js")),
            snapshots: SnapshotManager::at(root.join("snapshot.json")),
            root,
        }
    }

    fn folder(&self, profile: &str) -> PathBuf {
        self.root.join("User").join("profiles").join(profile)
    }

    fn profile(&self, profile: &str) -> SettingsManager {
        SettingsManager::at(self.folder(profile).join("settings.json"))
    }

    fn text(&self, settings: &SettingsManager) -> String {
        fs::read_to_string(settings.path()).unwrap()
    }

    fn takeover(&self) {
        self.snapshots
            .takeover(&self.settings, Some(&self.patcher), GATEWAY)
            .unwrap();
    }

    fn recorded(&self) -> Vec<String> {
        let record = self.snapshots.load().unwrap();
        record.profiles.into_iter().map(|p| p.name).collect()
    }
}

impl Drop for Machine {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// Default, a profile with settings of its own and one without a settings file yet are
/// all configured; a profile using Default's settings needs nothing of its own. The
/// restore puts each file back byte for byte and takes away the file and folder it made.
#[test]
fn a_takeover_configures_every_profile_and_a_restore_puts_each_back_exactly() {
    let machine = Machine::new("every");
    machine.takeover();

    for settings in [
        machine.settings.clone(),
        machine.profile("-5c1a0b"),
        machine.profile("31d9f2"),
    ] {
        assert!(
            settings.is_byok_active(Some(GATEWAY)),
            "{:?}",
            settings.path()
        );
        let values = settings.read_settings().unwrap();
        assert!(values.contains_key("codewhisperer.config"));
        assert_eq!(values["kiroAgent.enableTabAutocomplete"], json!(false));
        assert_eq!(values["http.noProxy"], json!(["gw.profiles.test"]));
    }
    let work = machine.text(&machine.profile("-5c1a0b"));
    assert!(work.starts_with("{\r\n\t// mine\r\n"), "{work:?}");
    assert!(work.contains("\r\n\t\"kiroAuthConfig\""), "{work:?}");
    assert!(
        !machine.folder("-77b0aa").join("settings.json").exists(),
        "Shared reads Default's settings"
    );
    assert_eq!(machine.recorded(), ["Work", "Play"]);

    let summary = machine.snapshots.restore_official().unwrap();
    assert!(summary.settings_restored && summary.extension_restored);
    assert_eq!(machine.text(&machine.settings), DEFAULT_SETTINGS);
    assert_eq!(machine.text(&machine.profile("-5c1a0b")), WORK_SETTINGS);
    assert!(
        !machine.folder("31d9f2").exists(),
        "the folder it made is gone"
    );
    assert_eq!(fs::read_dir(machine.folder("-77b0aa")).unwrap().count(), 1);
    assert_eq!(machine.patcher.status(), PatchStatus::Official);
    assert!(!machine.snapshots.has_active_snapshot());
}

/// Folders the takeover made for a profile's settings go with the restore, `profiles`
/// itself included; one Kiro has since put something in stays, as Kiro's.
#[test]
fn folders_made_for_a_profile_go_unless_kiro_has_used_them_since() {
    for used in [false, true] {
        let machine = Machine::new(&format!("folders-{used}"));
        fs::remove_dir_all(machine.root.join("User").join("profiles")).unwrap();
        machine.takeover();
        assert!(machine.profile("31d9f2").is_byok_active(Some(GATEWAY)));
        let kiro_state = machine.folder("31d9f2").join("globalStorage");
        if used {
            fs::create_dir_all(&kiro_state).unwrap();
            fs::write(kiro_state.join("state.vscdb"), b"kiro").unwrap();
        }

        machine.snapshots.restore_official().unwrap();
        assert!(!machine.profile("31d9f2").path().exists());
        assert_eq!(machine.folder("31d9f2").exists(), used);
        assert_eq!(machine.root.join("User").join("profiles").exists(), used);
        if used {
            assert_eq!(fs::read(kiro_state.join("state.vscdb")).unwrap(), b"kiro");
        }
        assert_eq!(machine.text(&machine.settings), DEFAULT_SETTINGS);
    }
}

/// A record written before profiles were configured (0.1.2 kept Default's settings only)
/// still restores exactly, and leaves the profiles it never wrote alone.
#[test]
fn a_record_from_before_profiles_restores_default_and_leaves_profiles_alone() {
    let machine = Machine::new("old-record");
    machine.settings.merge_byok(GATEWAY).unwrap();
    let old = json!({
        "created_at": 1,
        "gateway_url": GATEWAY,
        "settings_path": machine.settings.path(),
        "settings_state": {
            "had_settings_file": true,
            "proxy_bypass_managed": true,
            "prior_values": {},
            "prior_raw": DEFAULT_SETTINGS.as_bytes(),
        },
        "extension_path": null,
    });
    fs::write(machine.snapshots.snapshot_path(), old.to_string()).unwrap();
    assert!(machine.snapshots.load().unwrap().profiles.is_empty());

    let summary = machine.snapshots.restore_official().unwrap();
    assert!(summary.settings_restored && summary.snapshot_removed);
    assert_eq!(machine.text(&machine.settings), DEFAULT_SETTINGS);
    assert_eq!(machine.text(&machine.profile("-5c1a0b")), WORK_SETTINGS);
    assert!(!machine.folder("31d9f2").exists());
}

/// Without other profiles the record is written exactly as 0.1.2 wrote it, so a client
/// rolled back to 0.1.2 can still restore it.
#[test]
fn without_profiles_the_record_keeps_its_earlier_form() {
    let machine = Machine::new("record-form");
    fs::remove_file(
        machine
            .root
            .join("User")
            .join("globalStorage")
            .join("storage.json"),
    )
    .unwrap();
    machine.takeover();
    let record: Value =
        serde_json::from_slice(&fs::read(machine.snapshots.snapshot_path()).unwrap()).unwrap();
    let mut keys: Vec<&str> = record
        .as_object()
        .unwrap()
        .keys()
        .map(|k| k.as_str())
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "created_at",
            "extension_path",
            "gateway_url",
            "settings_path",
            "settings_state"
        ]
    );
    machine.snapshots.restore_official().unwrap();
    assert_eq!(machine.text(&machine.settings), DEFAULT_SETTINGS);
}

/// A profile's settings the takeover cannot edit safely (here a stray brace) refuse it
/// before anything is written, naming the profile and the place to fix.
#[test]
fn a_profile_whose_settings_cannot_be_edited_refuses_the_takeover_and_changes_nothing() {
    let machine = Machine::new("refused");
    let broken = "{\n  \"editor.fontSize\": 16\n}\n}\n";
    fs::write(machine.profile("-5c1a0b").path(), broken).unwrap();

    let error = machine
        .snapshots
        .plan_takeover(&machine.settings, Some(&machine.patcher), GATEWAY)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains(
            "Kiro profile \"Work\": settings.json has a syntax error at line 4, column 1"
        ),
        "{error}"
    );
    let error = machine
        .snapshots
        .takeover(&machine.settings, Some(&machine.patcher), GATEWAY)
        .unwrap_err()
        .to_string();
    assert!(error.contains("Kiro profile \"Work\""), "{error}");

    assert_eq!(machine.text(&machine.settings), DEFAULT_SETTINGS);
    assert_eq!(machine.text(&machine.profile("-5c1a0b")), broken);
    assert!(!machine.folder("31d9f2").exists());
    assert!(!machine.snapshots.has_active_snapshot());
    assert_eq!(machine.patcher.status(), PatchStatus::Official);
    assert!(!machine.patcher.backup_path().exists());
}

/// A profile made in Kiro after the takeover has none of it: the next launch through the
/// client configures it, recording it first, and a restore undoes that too. One made as a
/// copy of Default carries Default's taken-over settings; the restore takes the gateway
/// out of it rather than putting it back.
#[test]
fn a_profile_made_after_the_takeover_is_configured_at_the_next_launch_and_restored() {
    let machine = Machine::new("later");
    machine.takeover();
    let mut state: Value = serde_json::from_str(STORAGE).unwrap();
    state["userDataProfiles"].as_array_mut().unwrap().extend([
        json!({"location": "1f2e3d", "name": "Later"}),
        json!({"location": "0c0c0c", "name": "Copy"}),
    ]);
    fs::write(
        machine
            .root
            .join("User")
            .join("globalStorage")
            .join("storage.json"),
        state.to_string(),
    )
    .unwrap();
    fs::create_dir_all(machine.folder("0c0c0c")).unwrap();
    fs::copy(machine.settings.path(), machine.profile("0c0c0c").path()).unwrap();

    // Opening Kiro from the client: planned, then written, as a launch does.
    let plan = machine
        .snapshots
        .plan_takeover(&machine.settings, Some(&machine.patcher), GATEWAY)
        .unwrap();
    machine
        .snapshots
        .takeover_planned(&machine.settings, Some(&machine.patcher), GATEWAY, &plan)
        .unwrap();
    assert!(machine.profile("1f2e3d").is_byok_active(Some(GATEWAY)));
    assert_eq!(machine.recorded(), ["Work", "Play", "Later", "Copy"]);

    machine.snapshots.restore_official().unwrap();
    assert!(!machine.folder("1f2e3d").exists());
    let hosts = ["gw.profiles.test".to_string()];
    assert!(!machine.profile("0c0c0c").names_gateway(&hosts));
    let copy = machine.profile("0c0c0c").read_settings().unwrap();
    assert!(!copy.contains_key("http.noProxy"), "{copy:?}");
    assert_eq!(copy["editor.fontSize"], json!(14));
    assert_eq!(machine.text(&machine.settings), DEFAULT_SETTINGS);
    assert_eq!(machine.text(&machine.profile("-5c1a0b")), WORK_SETTINGS);
}

/// A profile's settings still naming the gateway once the records are lost are found,
/// in a profile Kiro lists or in a folder it no longer does, and cleaned up.
#[test]
fn a_leftover_in_a_profile_alone_is_found_and_removed() {
    let machine = Machine::new("leftover");
    let token = TokenStorage::at(machine.root.join("kiro-auth-token.json"));
    let hosts = ["gw.profiles.test".to_string()];
    let work = machine.profile("-5c1a0b");
    work.merge_byok(GATEWAY).unwrap();
    let unlisted = machine.profile("0a0a0a");
    unlisted.merge_byok(GATEWAY).unwrap();
    let scan = || Leftovers::scan(&[], &machine.settings, &token, &hosts);

    let found = scan();
    assert!(!found.settings, "Default is clean");
    assert_eq!(
        found.profile_settings,
        [work.path().to_path_buf(), unlisted.path().to_path_buf()]
    );
    assert!(found.found());
    found.remove(&machine.settings, &token, &hosts).unwrap();

    assert!(!scan().found());
    let text = machine.text(&work);
    assert!(
        text.contains("// mine") && !text.contains("kiroAuthConfig"),
        "{text}"
    );
    assert_eq!(machine.text(&machine.settings), DEFAULT_SETTINGS);
}
