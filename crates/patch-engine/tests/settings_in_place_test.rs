//! settings.json is edited in place: takeover and rollback change only the keys they
//! manage, and the user's comments, formatting and later edits survive both.
use patch_engine::{SettingsError, SettingsManager};
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;

const GATEWAY: &str = "https://gateway.test";

fn settings_file(name: &str, content: &[u8]) -> (PathBuf, SettingsManager) {
    let dir = std::env::temp_dir().join(format!(
        "settings-in-place-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("settings.json");
    fs::write(&path, content).unwrap();
    (dir, SettingsManager::at(&path))
}

fn text(manager: &SettingsManager) -> String {
    String::from_utf8(fs::read(manager.path()).unwrap()).unwrap()
}

/// The user edits the file by hand, as they would in the editor.
fn edit(manager: &SettingsManager, from: &str, to: &str) {
    let content = text(manager);
    assert!(content.contains(from), "{content}");
    fs::write(manager.path(), content.replacen(from, to, 1)).unwrap();
}

#[test]
fn comments_order_and_line_endings_survive_takeover_and_a_rollback_after_a_user_edit() {
    let original = "{\r\n\t// my font\r\n\t\"editor.fontSize\": 15,\r\n\t/* keep */ \"update.mode\": \"manual\",\r\n\t\"workbench.colorTheme\": \"Dark\",\r\n}\r\n";
    let (dir, manager) = settings_file("comments", original.as_bytes());

    let prior = manager.merge_byok(GATEWAY).unwrap();
    let merged = text(&manager);
    assert!(merged.contains("// my font"), "{merged}");
    assert!(
        merged.contains("/* keep */ \"update.mode\": \"none\""),
        "{merged}"
    );
    assert!(
        !merged.replace("\r\n", "").contains('\n'),
        "CRLF throughout: {merged:?}"
    );
    assert!(
        merged.contains("\r\n\t\"kiroAuthConfig\""),
        "tab indent: {merged:?}"
    );
    assert!(merged.find("editor.fontSize") < merged.find("workbench.colorTheme"));

    // While taken over, the user adds a setting of their own.
    edit(
        &manager,
        "\"workbench.colorTheme\": \"Dark\",",
        "\"workbench.colorTheme\": \"Dark\",\r\n\t\"editor.tabSize\": 2, // mine",
    );
    manager.revert(&prior, GATEWAY).unwrap();

    let reverted = text(&manager);
    assert!(reverted.contains("// my font"), "{reverted}");
    assert!(
        reverted.contains("/* keep */ \"update.mode\": \"manual\""),
        "{reverted}"
    );
    assert!(
        reverted.contains("\"editor.tabSize\": 2, // mine"),
        "{reverted}"
    );
    for managed in [
        "kiroAuthConfig",
        "codewhisperer.config",
        "http.noProxy",
        "telemetry",
    ] {
        assert!(!reverted.contains(managed), "{managed} left: {reverted}");
    }
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_byte_order_mark_is_kept_and_rollback_is_byte_exact() {
    let original = "\u{feff}{\n  \"editor.fontSize\": 15\n}\n";
    let (dir, manager) = settings_file("bom", original.as_bytes());

    let prior = manager.merge_byok(GATEWAY).unwrap();
    let merged = text(&manager);
    assert!(merged.starts_with('\u{feff}'));
    assert_eq!(
        manager.read_settings().unwrap()["update.mode"],
        json!("none")
    );

    manager.revert(&prior, GATEWAY).unwrap();
    assert_eq!(text(&manager), original);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn duplicate_managed_keys_leave_one_value_in_force() {
    // Kiro reads the last of duplicate keys, so editing only the first would do nothing.
    let original = "{\n  \"update.mode\": \"manual\",\n  \"editor.fontSize\": 15,\n  \"update.mode\": \"start\"\n}\n";
    let (dir, manager) = settings_file("duplicates", original.as_bytes());

    let prior = manager.merge_byok(GATEWAY).unwrap();
    let merged = text(&manager);
    assert_eq!(merged.matches("\"update.mode\"").count(), 1, "{merged}");
    assert_eq!(
        manager.read_settings().unwrap()["update.mode"],
        json!("none")
    );

    // A later edit rules out the byte-exact path, so the value comes back in place.
    edit(
        &manager,
        "\"editor.fontSize\": 15",
        "\"editor.fontSize\": 16",
    );
    manager.revert(&prior, GATEWAY).unwrap();
    assert_eq!(
        manager.read_settings().unwrap()["update.mode"],
        json!("start")
    );
    assert_eq!(text(&manager).matches("\"update.mode\"").count(), 1);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn proxy_bypass_rollback_removes_only_the_gateway_the_takeover_added() {
    let original = "{\n  \"http.noProxy\": [\"corp.example\"]\n}\n";
    let (dir, manager) = settings_file("no-proxy", original.as_bytes());
    let prior = manager.merge_byok(GATEWAY).unwrap();
    assert_eq!(
        manager.read_settings().unwrap()["http.noProxy"],
        json!(["corp.example", "gateway.test"])
    );
    // While taken over, the user adds an exception of their own.
    edit(
        &manager,
        "\"corp.example\",",
        "\"corp.example\", \"added.example\",",
    );
    manager.revert(&prior, GATEWAY).unwrap();
    assert_eq!(
        manager.read_settings().unwrap()["http.noProxy"],
        json!(["corp.example", "added.example"])
    );
    fs::remove_dir_all(dir).unwrap();

    // The user had listed the gateway themselves: it is theirs and stays.
    let original = "{\n  \"http.noProxy\": [\"gateway.test\"],\n  \"a\": 1\n}\n";
    let (dir, manager) = settings_file("no-proxy-own", original.as_bytes());
    let prior = manager.merge_byok(GATEWAY).unwrap();
    edit(&manager, "\"a\": 1", "\"a\": 2");
    manager.revert(&prior, GATEWAY).unwrap();
    assert_eq!(
        manager.read_settings().unwrap()["http.noProxy"],
        json!(["gateway.test"])
    );
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_preference_the_user_changed_while_taken_over_is_theirs() {
    let original =
        "{\n  \"update.mode\": \"manual\",\n  \"telemetry.telemetryLevel\": \"all\"\n}\n";
    let (dir, manager) = settings_file("preference", original.as_bytes());
    let prior = manager.merge_byok(GATEWAY).unwrap();
    edit(
        &manager,
        "\"update.mode\": \"none\"",
        "\"update.mode\": \"default\"",
    );
    manager.revert(&prior, GATEWAY).unwrap();

    let settings = manager.read_settings().unwrap();
    assert_eq!(
        settings["update.mode"],
        json!("default"),
        "the user's choice"
    );
    assert_eq!(
        settings["telemetry.telemetryLevel"],
        json!("all"),
        "ours, rolled back"
    );
    assert!(!settings.contains_key("kiroAuthConfig"));
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn new_keys_follow_the_file_indentation() {
    let original = "{\n    \"editor.fontSize\": 15\n}\n";
    let (dir, manager) = settings_file("indent", original.as_bytes());
    manager.merge_byok(GATEWAY).unwrap();
    let merged = text(&manager);
    assert!(merged.contains("\n    \"kiroAuthConfig\""), "{merged}");
    assert!(merged.contains("\n        \"endpoint\""), "{merged}");
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_file_kiro_would_not_read_is_refused_and_left_alone() {
    for original in [
        "{ 'editor.fontSize': 15 }",
        "{ \"a\": 1 \"b\": 2 }",
        "{ \"a\": 0x10 }",
        "[\"not\", \"an\", \"object\"]",
    ] {
        let (dir, manager) = settings_file("refused", original.as_bytes());
        assert!(manager.merge_byok(GATEWAY).is_err(), "{original}");
        assert_eq!(text(&manager), original);
        fs::remove_dir_all(dir).unwrap();
    }
}

/// Kiro reads past a missing comma, so a customer can leave one while taken over and
/// never notice. The rollback reads the file the same way, takes out only what the
/// takeover put in, and leaves the customer's typo exactly where it is.
#[test]
fn a_rollback_reads_past_a_missing_comma_and_leaves_the_typo_where_it_is() {
    let original = "{\n  // mine\n  \"editor.tabSize\": 2,\n  \"editor.fontSize\": 14,\n  \"update.mode\": \"manual\"\n}\n";
    let (dir, manager) = settings_file("missing-comma", original.as_bytes());
    let prior = manager.merge_byok(GATEWAY).unwrap();
    edit(&manager, "\"editor.tabSize\": 2,", "\"editor.tabSize\": 2");
    assert!(manager.read_settings().is_err(), "no longer strict JSONC");

    manager.revert(&prior, GATEWAY).unwrap();
    assert_eq!(
        text(&manager),
        original.replacen("\"editor.tabSize\": 2,", "\"editor.tabSize\": 2", 1)
    );
    fs::remove_dir_all(dir).unwrap();
}

/// A missing comma right next to a member the takeover added: taking that member out
/// must not take one of the customer's commas instead, which would move their typo
/// onto their own line.
#[test]
fn a_missing_comma_next_to_a_takeover_key_costs_the_customer_no_comma() {
    // The customer adds a line at the end and leaves out the comma above it, which
    // ends the takeover's last member.
    let original = "{\n  \"editor.tabSize\": 2,\n  \"editor.fontSize\": 14\n}\n";
    let (dir, manager) = settings_file("comma-after-ours", original.as_bytes());
    let prior = manager.merge_byok(GATEWAY).unwrap();
    let merged = text(&manager);
    let end = merged.rfind('}').unwrap();
    fs::write(
        manager.path(),
        format!(
            "{}  \"editor.wordWrap\": \"on\"\n{}",
            &merged[..end],
            &merged[end..]
        ),
    )
    .unwrap();
    manager.revert(&prior, GATEWAY).unwrap();
    assert_eq!(
        text(&manager),
        "{\n  \"editor.tabSize\": 2,\n  \"editor.fontSize\": 14,\n  \"editor.wordWrap\": \"on\"\n}\n"
    );
    fs::remove_dir_all(dir).unwrap();

    // The customer deletes the comma between their last setting and the takeover's first.
    let (dir, manager) = settings_file("comma-before-ours", original.as_bytes());
    let prior = manager.merge_byok(GATEWAY).unwrap();
    edit(
        &manager,
        "\"editor.fontSize\": 14,",
        "\"editor.fontSize\": 14",
    );
    manager.revert(&prior, GATEWAY).unwrap();
    assert_eq!(text(&manager), original);
    fs::remove_dir_all(dir).unwrap();
}

/// A syntax error even a tolerant reading stops at (here a stray closing brace) is named
/// by line and column, and nothing is written.
#[test]
fn a_syntax_error_no_reading_gets_past_is_located_and_nothing_is_written() {
    let original = "{\n  \"editor.fontSize\": 14\n}\n";
    let (dir, manager) = settings_file("stray-brace", original.as_bytes());
    let prior = manager.merge_byok(GATEWAY).unwrap();
    let broken = format!("{}}}\n", text(&manager));
    fs::write(manager.path(), &broken).unwrap();
    let line = broken.matches('\n').count();

    let error = manager.revert(&prior, GATEWAY).unwrap_err();
    assert!(
        matches!(error, SettingsError::Syntax { line: l, column: 1 } if l == line),
        "{error:?}"
    );
    assert!(
        error.to_string().contains(&format!(
            "settings.json has a syntax error at line {line}, column 1"
        )),
        "{error}"
    );
    let hosts = ["gateway.test".to_string()];
    assert!(manager.names_gateway(&hosts), "blind to a broken file");
    assert!(matches!(
        manager.remove_orphaned_takeover(&hosts),
        Err(SettingsError::Syntax { .. })
    ));
    assert_eq!(text(&manager), broken);
    fs::remove_dir_all(dir).unwrap();
}

/// A symbolic link at `link` to `target`.
fn symlink(target: &std::path::Path, link: &std::path::Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_file(target, link)
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link)
    }
}

/// A settings.json linked into a dotfiles repository stays linked: the takeover and its
/// rollback edit the file it points to. Replaced by rename, the link became a detached
/// copy, and every later change missed the repository.
#[test]
fn a_linked_settings_file_stays_linked_through_takeover_and_rollback() {
    let original = "{\n  \"editor.fontSize\": 15\n}\n";
    let (dir, _) = settings_file("linked", b"");
    let target = dir.join("dotfiles").join("settings.json");
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    fs::write(&target, original).unwrap();
    let link = dir.join("User").join("settings.json");
    fs::create_dir_all(link.parent().unwrap()).unwrap();
    if let Err(error) = symlink(&target, &link) {
        eprintln!("skipped: symbolic links cannot be created here ({error})");
        fs::remove_dir_all(dir).unwrap();
        return;
    }
    let manager = SettingsManager::at(&link);

    let prior = manager.merge_byok(GATEWAY).unwrap();
    assert!(fs::symlink_metadata(&link)
        .unwrap()
        .file_type()
        .is_symlink());
    assert!(fs::read_to_string(&target)
        .unwrap()
        .contains("kiroAuthConfig"));
    manager.revert(&prior, GATEWAY).unwrap();
    assert!(fs::symlink_metadata(&link)
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(fs::read_to_string(&target).unwrap(), original);
    for side in [link.parent().unwrap(), target.parent().unwrap()] {
        assert_eq!(fs::read_dir(side).unwrap().count(), 1, "{side:?}");
    }
    fs::remove_dir_all(dir).unwrap();
}

/// A settings.json with a second name (a hard link) is refused: replaced by rename, only
/// one name would carry the takeover, and the rollback could not join them again.
#[test]
fn a_settings_file_with_another_hard_link_is_refused_and_left_alone() {
    let original = "{\n  \"editor.fontSize\": 15\n}\n";
    let (dir, manager) = settings_file("hard-link", original.as_bytes());
    let other = dir.join("settings-elsewhere.json");
    fs::hard_link(manager.path(), &other).unwrap();

    let error = manager.merge_byok(GATEWAY).unwrap_err();
    assert!(
        error.to_string().contains("more than one hard link"),
        "{error}"
    );
    assert!(manager.plan_merge(GATEWAY).is_err());
    assert_eq!(text(&manager), original);
    assert_eq!(fs::read_to_string(&other).unwrap(), original);

    fs::remove_file(&other).unwrap();
    manager.merge_byok(GATEWAY).unwrap();
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_takeover_of_no_file_rolls_back_to_no_file() {
    let dir = std::env::temp_dir().join(format!("settings-in-place-none-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let manager = SettingsManager::at(dir.join("settings.json"));
    let prior = manager.merge_byok(GATEWAY).unwrap();
    let merged: Value = serde_json::from_slice(&fs::read(manager.path()).unwrap()).unwrap();
    assert_eq!(merged["update.mode"], json!("none"));
    manager.revert(&prior, GATEWAY).unwrap();
    assert!(!manager.path().exists());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn windows_on_a_profile_other_than_default_are_reported() {
    let (dir, manager) = settings_file("profiles", b"{}");
    let storage = dir.join("globalStorage").join("storage.json");
    fs::create_dir_all(storage.parent().unwrap()).unwrap();
    assert!(
        manager.profiles_in_use().is_empty(),
        "no storage, nothing known"
    );

    fs::write(
        &storage,
        r#"{"profileAssociations": {"workspaces": {"file:///d%3A/a": "__default__profile__"}, "emptyWindows": {"1": "__default__profile__"}}}"#,
    )
    .unwrap();
    assert!(manager.profiles_in_use().is_empty());

    fs::write(
        &storage,
        r#"{"userDataProfiles": [{"location": "-7a1b", "name": "Work"}], "profileAssociations": {"workspaces": {"file:///d%3A/a": "-7a1b", "file:///d%3A/b": "__default__profile__"}, "emptyWindows": {"1": "-7a1b"}}}"#,
    )
    .unwrap();
    assert_eq!(manager.profiles_in_use(), vec!["-7a1b".to_string()]);
    fs::remove_dir_all(dir).unwrap();
}
