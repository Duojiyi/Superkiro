//! Dual-blind audit test for Client White-labeling (Spec §14.6, P4-8).

use patch_engine::white_label::{ThemeMode, WhiteLabelConfig};
use std::fs;

#[test]
fn test_client_white_label_defaults_and_env_overrides() {
    // 1. Default configuration
    let default_cfg = WhiteLabelConfig::default();
    assert_eq!(default_cfg.app_name, "KIRO 极速接管中心");
    assert_eq!(default_cfg.app_slug, "kiro-byok");
    assert_eq!(default_cfg.primary_color, "#6366F1");
    assert_eq!(default_cfg.theme_mode, ThemeMode::Auto);
    assert_eq!(default_cfg.official_website, "https://byok.kiro.dev");

    // 2. Custom instantiation
    let custom = WhiteLabelConfig::new("Nova AI Studio", "https://nova.ai");
    assert_eq!(custom.app_name, "Nova AI Studio");
    assert_eq!(custom.app_slug, "nova-ai-studio");
    assert_eq!(custom.official_website, "https://nova.ai");

    // 3. Env overrides
    std::env::set_var("KIRO_BRAND_APP_NAME", "Enterprise Dev Accelerator");
    std::env::set_var("KIRO_BRAND_PRIMARY_COLOR", "#10B981");
    let env_cfg = WhiteLabelConfig::default().with_env_overrides();
    assert_eq!(env_cfg.app_name, "Enterprise Dev Accelerator");
    assert_eq!(env_cfg.primary_color, "#10B981");
    std::env::remove_var("KIRO_BRAND_APP_NAME");
    std::env::remove_var("KIRO_BRAND_PRIMARY_COLOR");
}

#[test]
fn test_client_white_label_file_persistence_and_merge() {
    let temp_dir = std::env::temp_dir().join(format!("kiro_test_brand_{}", std::process::id()));
    let _ = fs::create_dir_all(&temp_dir);
    let config_path = temp_dir.join("brand.json");

    let mut local_cfg = WhiteLabelConfig::new("Local Custom", "https://local.dev");
    local_cfg.primary_color = "#FF5722".to_string();
    local_cfg.save_to_file(&config_path).unwrap();

    let loaded = WhiteLabelConfig::load_or_default(&config_path).unwrap();
    assert_eq!(loaded.app_name, "Local Custom");
    assert_eq!(loaded.primary_color, "#FF5722");

    // Server-side override merge
    let mut server_override = WhiteLabelConfig::new("Server Global Brand", "https://server.cloud");
    server_override.accent_color = "#00E676".to_string();
    server_override.theme_mode = ThemeMode::Dark;

    local_cfg.merge_with(&server_override);
    assert_eq!(local_cfg.app_name, "Server Global Brand");
    assert_eq!(local_cfg.official_website, "https://server.cloud");
    assert_eq!(local_cfg.accent_color, "#00E676");
    assert_eq!(local_cfg.theme_mode, ThemeMode::Dark);

    let _ = fs::remove_dir_all(&temp_dir);
}
