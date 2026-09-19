//! Tests for P2-1: Card template, high-entropy key generation, batch creation,
//! CSV/JSON export, and "激活即计时" lifecycle.
//!
//! Spec §5, §7, §14.9.

use billing::card::{hash_card_code, normalize_card_code, verify_card_code, CardError, CardStatus};
use billing::generator::{export_csv, export_json, generate_batch, generate_card, GeneratedCard};
use billing::template::CardTemplate;
use std::collections::HashSet;

#[test]
fn test_template_presets() {
    let t_daily = CardTemplate::daily("tpl-day", "grp-1");
    assert_eq!(t_daily.duration_secs, 86_400);
    assert_eq!(t_daily.credit_total, 10_000_000);
    assert_eq!(t_daily.group_id, "grp-1");

    let t_weekly = CardTemplate::weekly("tpl-week", "grp-1");
    assert_eq!(t_weekly.duration_secs, 604_800);
    assert_eq!(t_weekly.credit_total, 50_000_000);

    let t_monthly = CardTemplate::monthly("tpl-month", "grp-1");
    assert_eq!(t_monthly.duration_secs, 2_592_000);
    assert_eq!(t_monthly.credit_total, 200_000_000);

    let t_payg = CardTemplate::pay_as_you_go("tpl-payg", 500_000_000, "grp-2");
    assert_eq!(t_payg.duration_secs, 0);
    assert_eq!(t_payg.credit_total, 500_000_000);
}

#[test]
fn test_high_entropy_generation_and_uniqueness() {
    let tpl = CardTemplate::monthly("tpl-month", "group-default");
    let count = 500;
    let cards = generate_batch(&tpl, count, Some("test-batch"), 1_000_000).unwrap();

    assert_eq!(cards.len(), count);

    let mut raw_codes = HashSet::new();
    let mut code_hashes = HashSet::new();
    let mut card_ids = HashSet::new();

    for c in &cards {
        assert!(c.raw_code.starts_with("kiro-"));
        // 4 hex chars per group * 8 groups = 32 hex chars + 8 hyphens + 4 prefix chars = 44 chars
        assert_eq!(c.raw_code.len(), 44);

        assert!(
            raw_codes.insert(c.raw_code.clone()),
            "Duplicate raw code generated!"
        );
        assert!(
            code_hashes.insert(c.card.code_hash.clone()),
            "Duplicate hash generated!"
        );
        assert!(
            card_ids.insert(c.card.id.clone()),
            "Duplicate card id generated!"
        );

        // Card must be unactivated initially
        assert_eq!(c.card.status, CardStatus::Unactivated);
        assert_eq!(c.card.activated_at, None);
        assert_eq!(c.card.valid_until, None);
        assert_eq!(c.card.credit_total, 200_000_000);
        assert_eq!(c.card.credit_used, 0);
        assert_eq!(c.card.credit_reserved, 0);
    }
}

#[test]
fn test_normalization_and_constant_time_verification() {
    let raw = "kiro-9F83-A1B2-C3D4-E5F6-7A8B-9C0D-1E2F-3A4B";
    let normalized = normalize_card_code(raw);
    assert_eq!(normalized, "kiro9f83a1b2c3d4e5f67a8b9c0d1e2f3a4b");

    let hash = hash_card_code(raw);
    assert_eq!(hash.len(), 64); // SHA-256 is 64 hex characters

    // Tolerant verification with variations (spaces, lowercase, mixed)
    assert!(verify_card_code(
        "kiro-9f83-a1b2-c3d4-e5f6-7a8b-9c0d-1e2f-3a4b",
        &hash
    ));
    assert!(verify_card_code(
        "kiro 9f83 a1b2 c3d4 e5f6 7a8b 9c0d 1e2f 3a4b",
        &hash
    ));
    assert!(verify_card_code(
        "  kiro9f83a1b2c3d4e5f67a8b9c0d1e2f3a4b  ",
        &hash
    ));

    // Tampered verification must fail
    assert!(!verify_card_code(
        "kiro-0000-a1b2-c3d4-e5f6-7a8b-9c0d-1e2f-3a4b",
        &hash
    ));
}

#[test]
fn test_audit_b_activation_begins_timing_lifecycle() {
    let tpl = CardTemplate::daily("tpl-daily", "grp-1"); // 86,400s (1 day)
    let gen = generate_card(&tpl, Some("promo-card"), 1_000_000).unwrap();
    let mut card = gen.card;

    // 1. Before activation: status is Unactivated, cannot reserve or pass active check
    assert_eq!(card.status, CardStatus::Unactivated);
    assert_eq!(card.activated_at, None);
    assert_eq!(card.valid_until, None);
    assert_eq!(
        card.check_active(1_000_000),
        Err(CardError::NotActive(CardStatus::Unactivated))
    );

    // 2. User activates card at t = 1,700,000
    let activation_time = 1_700_000u64;
    card.activate(activation_time, tpl.duration_secs).unwrap();

    assert_eq!(card.status, CardStatus::Active);
    assert_eq!(card.activated_at, Some(activation_time));
    assert_eq!(card.valid_until, Some(activation_time + 86_400));

    // 3. During validity period (e.g. t = activation + 3600s) -> Active
    assert!(card.check_active(activation_time + 3600).is_ok());
    assert!(card
        .check_can_reserve(5_000_000, activation_time + 3600)
        .is_ok());

    // 4. After expiration (e.g. t = activation + 86,401s) -> Expired
    assert_eq!(
        card.check_active(activation_time + 86_401),
        Err(CardError::Expired)
    );
    assert_eq!(
        card.check_can_reserve(5_000_000, activation_time + 86_401),
        Err(CardError::Expired)
    );
}

#[test]
fn test_audit_b_perpetual_pay_as_you_go_never_expires() {
    let tpl = CardTemplate::pay_as_you_go("tpl-payg", 100_000_000, "grp-1");
    let gen = generate_card(&tpl, None, 1_000_000).unwrap();
    let mut card = gen.card;

    card.activate(1_000_000, tpl.duration_secs).unwrap();
    assert_eq!(card.status, CardStatus::Active);
    assert_eq!(card.valid_until, None); // Never expires

    // Even 100 years into the future (3 billion seconds later), still valid!
    assert!(card.check_active(4_000_000_000).is_ok());
}

#[test]
fn test_audit_b_frozen_or_banned_card_cannot_activate() {
    let tpl = CardTemplate::daily("tpl-daily", "grp-1");
    let gen = generate_card(&tpl, None, 1_000_000).unwrap();
    let mut card = gen.card;

    card.status = CardStatus::Frozen;
    assert_eq!(
        card.activate(1_100_000, tpl.duration_secs),
        Err(CardError::NotActive(CardStatus::Frozen))
    );

    card.status = CardStatus::Banned;
    assert_eq!(
        card.activate(1_100_000, tpl.duration_secs),
        Err(CardError::NotActive(CardStatus::Banned))
    );
}

#[test]
fn test_export_csv_and_json_roundtrip() {
    let tpl = CardTemplate::monthly("tpl-month", "group-vip");
    let batch = generate_batch(&tpl, 3, Some("promo-2026"), 1_780_000_000).unwrap();

    // 1. Test CSV Export
    let csv = export_csv(&batch);
    assert!(csv.starts_with(
        "id,raw_code,code_hash,template_id,group_id,credit_total,status,note,created_at\n"
    ));
    let lines: Vec<&str> = csv.trim().lines().collect();
    assert_eq!(lines.len(), 4); // Header + 3 records
    for line in lines.iter().skip(1).take(3) {
        assert!(line.contains("promo-2026-#"));
        assert!(line.contains("group-vip"));
        assert!(line.contains("200000000"));
        assert!(line.contains("unactivated"));
    }

    // 2. Test JSON Export
    let json_str = export_json(&batch);
    let parsed: Vec<GeneratedCard> = serde_json::from_str(&json_str).expect("Valid JSON array");
    assert_eq!(parsed.len(), 3);
    for (orig, p) in batch.iter().zip(parsed.iter()) {
        assert_eq!(orig.raw_code, p.raw_code);
        assert_eq!(orig.card.id, p.card.id);
        assert_eq!(orig.card.code_hash, p.card.code_hash);
        assert_eq!(orig.card.credit_total, p.card.credit_total);
    }
}
