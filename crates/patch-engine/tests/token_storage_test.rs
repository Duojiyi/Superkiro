//! Tests for Kiro 1.0.x token disk storage and atomic writes (Task P3-1).

use patch_engine::token_storage::{
    parse_iso8601_to_epoch, KiroAuthToken, TokenStorage, KIRO_TOKEN_FILENAME,
};
use std::fs;

#[test]
fn test_token_serde_schema_against_p0_format() {
    let raw_json = r#"{
        "accessToken": "aoaAAAAAGqiTE0y...",
        "refreshToken": "aorAAAAAGsXiEcs...",
        "profileArn": "arn:aws:codewhisperer:us-east-1:699475941385:profile/EHGA3GRVQMUK",
        "expiresAt": "2026-09-10T06:21:02.273Z",
        "authMethod": "social",
        "provider": "Google"
    }"#;

    let token: KiroAuthToken =
        serde_json::from_str(raw_json).expect("Must deserialize P0 token schema");
    assert_eq!(token.access_token, "aoaAAAAAGqiTE0y...");
    assert_eq!(token.refresh_token, "aorAAAAAGsXiEcs...");
    assert_eq!(
        token.profile_arn,
        "arn:aws:codewhisperer:us-east-1:699475941385:profile/EHGA3GRVQMUK"
    );
    assert_eq!(token.expires_at, "2026-09-10T06:21:02.273Z");
    assert_eq!(token.auth_method, "social");
    assert_eq!(token.provider, "Google");

    let serialized = serde_json::to_string(&token).expect("Must serialize");
    assert!(serialized.contains(r#""accessToken":""#));
    assert!(serialized.contains(r#""refreshToken":""#));
    assert!(serialized.contains(r#""profileArn":""#));
    assert!(serialized.contains(r#""expiresAt":""#));
    assert!(serialized.contains(r#""authMethod":""#));
    assert!(serialized.contains(r#""provider":""#));
}

#[test]
fn test_atomic_save_load_clear_cycle() {
    let sandbox_dir =
        std::env::temp_dir().join(format!("kiro_p3_1_storage_{}", std::process::id()));
    let target_file = sandbox_dir
        .join(".aws")
        .join("sso")
        .join("cache")
        .join(KIRO_TOKEN_FILENAME);

    let storage = TokenStorage::at(&target_file);
    assert!(!storage.exists());

    let token = KiroAuthToken::new(
        "jwt-access-abc-123",
        "rt-refresh-xyz-789",
        "arn:aws:codewhisperer:us-east-1:123456789012:profile/PRO",
        "2030-01-01T12:00:00Z",
    );

    // Atomic write
    storage
        .save(&token)
        .expect("Save must succeed and create parents");
    assert!(storage.exists());

    // Atomic load
    let loaded = storage.load().expect("Load must succeed");
    assert_eq!(loaded, token);

    // Check expiration (2030 is far in future)
    assert!(!storage.is_expired(60));

    // Clear (logout)
    let cleared = storage.clear().expect("Clear must succeed");
    assert!(cleared);
    assert!(!storage.exists());

    // Subsequent clear is false
    assert!(!storage.clear().unwrap());

    // Clean up
    let _ = fs::remove_dir_all(sandbox_dir);
}

#[test]
fn test_expired_detection() {
    let sandbox_dir = std::env::temp_dir().join(format!("kiro_p3_1_exp_{}", std::process::id()));
    let target_file = sandbox_dir.join(KIRO_TOKEN_FILENAME);
    let storage = TokenStorage::at(&target_file);

    // 1. Expired token in past (1990)
    let expired_token = KiroAuthToken::new(
        "jwt-expired",
        "rt-expired",
        "arn:aws:profile",
        "1990-01-01T00:00:00Z",
    );
    storage.save(&expired_token).unwrap();
    assert!(storage.is_expired(0));

    // 2. Token with future date
    let year = 2026;
    let s = format!("{:04}-09-10T12:00:00Z", year);
    let token_near = KiroAuthToken::new("jwt-near", "rt-near", "arn:aws:profile", s);
    storage.save(&token_near).unwrap();

    let _ = fs::remove_dir_all(sandbox_dir);
}

#[test]
fn test_parse_iso8601_to_epoch() {
    assert_eq!(parse_iso8601_to_epoch("1970-01-01T00:00:00Z"), Some(0));
    assert_eq!(parse_iso8601_to_epoch("1970-01-01T01:00:00Z"), Some(3600));

    let epoch = parse_iso8601_to_epoch("2026-09-10T12:00:00Z").unwrap();
    assert!(epoch > 1_700_000_000);
}
