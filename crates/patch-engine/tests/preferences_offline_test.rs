//! Integration tests for user preferences, offline grace period evaluation, and announcements.
//!
//! Spec §9, §10, §14.2.

use patch_engine::doctor::TakeoverStatus;
use patch_engine::preferences::{
    Announcement, ClientPreferences, Language, ReasoningEffort, DEFAULT_OFFLINE_GRACE_SECONDS,
};
use std::fs;

#[test]
fn test_preferences_defaults_and_serde_cycle() {
    let prefs = ClientPreferences::default();
    assert_eq!(prefs.default_model, "claude-3-7-sonnet");
    assert_eq!(prefs.reasoning_effort, ReasoningEffort::Medium);
    assert_eq!(prefs.language, Language::ZhCn);
    assert_eq!(prefs.offline_grace_seconds, DEFAULT_OFFLINE_GRACE_SECONDS);
    assert_eq!(prefs.last_online_timestamp, 0);
    assert_eq!(prefs.last_dismissed_announcement_id, None);

    let json = serde_json::to_string(&prefs).unwrap();
    let deserialized: ClientPreferences = serde_json::from_str(&json).unwrap();
    assert_eq!(prefs, deserialized);
}

#[test]
fn test_preferences_atomic_save_and_load() {
    let temp_dir = std::env::temp_dir().join(format!("kiro_test_prefs_{}", std::process::id()));
    let prefs_file = temp_dir.join("preferences.json");
    let _ = fs::remove_dir_all(&temp_dir);

    // 1. Loading non-existent file returns defaults
    let loaded = ClientPreferences::load_or_default(&prefs_file).unwrap();
    assert_eq!(loaded, ClientPreferences::default());

    // 2. Modify and save atomically
    let mut modified = loaded;
    modified.default_model = "deepseek-reasoner".to_string();
    modified.reasoning_effort = ReasoningEffort::High;
    modified.language = Language::EnUs;
    modified.mark_online(1725900000);
    modified.last_dismissed_announcement_id = Some("ann-001".to_string());
    modified.save_to_file(&prefs_file).unwrap();

    // 3. Reload and assert
    let reloaded = ClientPreferences::load_or_default(&prefs_file).unwrap();
    assert_eq!(reloaded.default_model, "deepseek-reasoner");
    assert_eq!(reloaded.reasoning_effort, ReasoningEffort::High);
    assert_eq!(reloaded.language, Language::EnUs);
    assert_eq!(reloaded.last_online_timestamp, 1725900000);
    assert_eq!(
        reloaded.last_dismissed_announcement_id.as_deref(),
        Some("ann-001")
    );

    let _ = fs::remove_dir_all(&temp_dir);
}

#[test]
fn test_offline_grace_evaluation() {
    let mut prefs = ClientPreferences::default();
    let baseline_online = 1_700_000_000u64;

    // Never online -> grace period is false
    assert!(!prefs.is_within_offline_grace(baseline_online));
    assert_eq!(prefs.remaining_grace_seconds(baseline_online), 0);

    // Online verification logged
    prefs.mark_online(baseline_online);

    // 1 hour offline -> within 72h grace
    let one_hour_later = baseline_online + 3600;
    assert!(prefs.is_within_offline_grace(one_hour_later));
    assert_eq!(
        prefs.remaining_grace_seconds(one_hour_later),
        DEFAULT_OFFLINE_GRACE_SECONDS - 3600
    );

    // 71 hours offline -> still within grace
    let seventy_one_hours_later = baseline_online + (71 * 3600);
    assert!(prefs.is_within_offline_grace(seventy_one_hours_later));
    assert_eq!(prefs.remaining_grace_seconds(seventy_one_hours_later), 3600);

    // Exactly 72 hours -> boundary check
    let exactly_72h = baseline_online + (72 * 3600);
    assert!(prefs.is_within_offline_grace(exactly_72h));
    assert_eq!(prefs.remaining_grace_seconds(exactly_72h), 0);

    // 73 hours offline -> grace window has expired
    let seventy_three_hours_later = baseline_online + (73 * 3600);
    assert!(!prefs.is_within_offline_grace(seventy_three_hours_later));
    assert_eq!(prefs.remaining_grace_seconds(seventy_three_hours_later), 0);
}

#[test]
fn test_announcement_serialization() {
    let ann = Announcement {
        id: "ann_2026_09_10".to_string(),
        title: "Kiro BYOK Gateway Update".to_string(),
        content: "Claude 3.7 Sonnet thinking mode supported".to_string(),
        level: "info".to_string(),
        published_at: 1725900000,
    };

    let json = serde_json::to_string(&ann).unwrap();
    let deserialized: Announcement = serde_json::from_str(&json).unwrap();
    assert_eq!(ann, deserialized);
}

#[test]
fn test_takeover_status_offline_grace_serde() {
    let status = TakeoverStatus::OfflineGrace;
    let json = serde_json::to_string(&status).unwrap();
    assert_eq!(json, "\"offline_grace\"");

    let back: TakeoverStatus = serde_json::from_str(&json).unwrap();
    assert_eq!(back, TakeoverStatus::OfflineGrace);
}
