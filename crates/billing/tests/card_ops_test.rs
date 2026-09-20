//! Tests for P2-2: Card operations & renewal.
//!
//! Covers:
//! - Device binding, multi-device limit, and idempotency
//! - Explicit rebind, quota limit, cooldown, and token_version increment
//! - Manual unbind and device listing
//! - Freeze, unfreeze, ban, and batch status mutations
//! - Top-up code generation, batch generation, redemption, balance and validity extension, replay rejection
//!
//! Spec §5, §14.9.

use billing::card::{Card, CardStatus};
use billing::engine::BillingEngine;
use billing::ledger::LedgerKind;
use billing::topup::{
    export_topup_csv, export_topup_json, generate_topup_batch, generate_topup_code,
};
use billing::BillingError;

fn create_test_card(id: &str, max_devices: u32, max_rebinds: u32, cooldown: u64) -> Card {
    let mut card = Card::new(id, "grp-1", 100_000_000);
    card.code_hash = format!("hash-{}", id);
    card.status = CardStatus::Active;
    card.activated_at = Some(1_000);
    card.valid_until = Some(1_000 + 86_400);
    card.max_devices = max_devices;
    card.max_rebinds = max_rebinds;
    card.rebind_cooldown_secs = cooldown;
    card
}

#[test]
fn test_device_binding_within_limit_and_idempotence() {
    let engine = BillingEngine::new();
    let card = create_test_card("card-dev-1", 2, 3, 300);
    engine.upsert_card(card);

    // Bind first device
    assert!(engine
        .bind_device("card-dev-1", "device-alpha", 1_000)
        .is_ok());
    let devices = engine.list_devices("card-dev-1").unwrap();
    assert_eq!(devices, vec!["device-alpha".to_string()]);

    // Re-binding the same device is a no-op (idempotent)
    assert!(engine
        .bind_device("card-dev-1", "device-alpha", 1_050)
        .is_ok());
    let devices = engine.list_devices("card-dev-1").unwrap();
    assert_eq!(devices.len(), 1);

    // Occupied bindings cannot be replaced, even for legacy capacity=2
    let before = engine.get_card("card-dev-1").unwrap();
    assert!(matches!(
        engine.bind_device("card-dev-1", "device-beta", 1100),
        Err(BillingError::DeviceAlreadyBound)
    ));
    assert_eq!(engine.get_card("card-dev-1").unwrap(), before);
    engine.unbind_device("card-dev-1", "device-alpha").unwrap();
    assert!(engine
        .bind_device("card-dev-1", "device-beta", 1_100)
        .is_ok());
    let devices = engine.list_devices("card-dev-1").unwrap();
    assert_eq!(devices, vec!["device-beta".to_string()]);

    // Replacement revokes the previous device session
    let card = engine.get_card("card-dev-1").unwrap();
    assert_eq!(card.token_version, 2);
    assert_eq!(card.rebind_count, 1);
}

#[test]
fn test_explicit_rebind_cooldown_and_limit() {
    let engine = BillingEngine::new();
    engine.upsert_card(create_test_card("card-rebind", 2, 2, 300));
    engine.bind_device("card-rebind", "dev-1", 1000).unwrap();
    engine.unbind_device("card-rebind", "dev-1").unwrap();
    engine.bind_device("card-rebind", "dev-2", 1050).unwrap();
    let before = engine.get_card("card-rebind").unwrap();
    assert!(matches!(
        engine.unbind_device("card-rebind", "dev-2"),
        Err(BillingError::RebindCooldown { .. })
    ));
    assert_eq!(engine.get_card("card-rebind").unwrap(), before);
    let mut elapsed = before;
    elapsed.last_rebind_at = Some(1);
    engine.upsert_card(elapsed);
    engine.unbind_device("card-rebind", "dev-2").unwrap();
    engine.bind_device("card-rebind", "dev-3", 1360).unwrap();
    let before = engine.get_card("card-rebind").unwrap();
    assert_eq!(before.rebind_count, 2);
    assert_eq!(before.token_version, 3);
    assert!(matches!(
        engine.unbind_device("card-rebind", "dev-3"),
        Err(BillingError::RebindLimitExceeded { current: 2, max: 2 })
    ));
    assert_eq!(engine.get_card("card-rebind").unwrap(), before);
}

#[test]
fn test_unbind_device() {
    let engine = BillingEngine::new();
    let card = create_test_card("card-unbind", 2, 2, 300);
    engine.upsert_card(card);

    engine.bind_device("card-unbind", "dev-1", 1_000).unwrap();

    // Unbind non-existent device
    let err = engine
        .unbind_device("card-unbind", "dev-unknown")
        .unwrap_err();
    assert!(matches!(err, BillingError::DeviceNotFound { .. }));

    // Unbind valid device
    assert!(engine.unbind_device("card-unbind", "dev-1").is_ok());
    let devices = engine.list_devices("card-unbind").unwrap();
    assert!(devices.is_empty());

    // Token version incremented on unbind to invalidate removed device's token (from 1 to 2)
    let card = engine.get_card("card-unbind").unwrap();
    assert_eq!(card.token_version, 2);
}

#[test]
fn test_freeze_unfreeze_and_ban_lifecycle() {
    let engine = BillingEngine::new();
    let card = create_test_card("card-lifecycle", 2, 2, 300);
    engine.upsert_card(card);

    // Freeze card
    assert!(engine.freeze_card("card-lifecycle", "test freeze").is_ok());
    let card = engine.get_card("card-lifecycle").unwrap();
    assert_eq!(card.status, CardStatus::Frozen);
    assert_eq!(card.token_version, 2); // Token version incremented on freeze (from 1 to 2)!

    // Unfreeze card
    assert!(engine.unfreeze_card("card-lifecycle").is_ok());
    let card = engine.get_card("card-lifecycle").unwrap();
    assert_eq!(card.status, CardStatus::Active);

    // Ban card
    assert!(engine
        .ban_card("card-lifecycle", "policy violation")
        .is_ok());
    let card = engine.get_card("card-lifecycle").unwrap();
    assert_eq!(card.status, CardStatus::Banned);
    assert_eq!(card.token_version, 3); // incremented again on ban (from 2 to 3)!
}

#[test]
fn test_batch_card_status_operations() {
    let engine = BillingEngine::new();
    for i in 1..=3 {
        let card = create_test_card(&format!("batch-card-{}", i), 2, 2, 300);
        engine.upsert_card(card);
    }

    let ids = vec![
        "batch-card-1".to_string(),
        "batch-card-2".to_string(),
        "batch-card-3".to_string(),
    ];
    let results = engine.batch_freeze(&ids, "batch freeze test");
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 3);

    for id in &ids {
        assert_eq!(engine.get_card(id).unwrap().status, CardStatus::Frozen);
    }

    let results = engine.batch_unfreeze(&ids);
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 3);
    for id in &ids {
        assert_eq!(engine.get_card(id).unwrap().status, CardStatus::Active);
    }

    let results = engine.batch_ban(&ids, "batch ban test");
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 3);
    for id in &ids {
        assert_eq!(engine.get_card(id).unwrap().status, CardStatus::Banned);
    }
}

#[test]
fn test_update_card_note() {
    let engine = BillingEngine::new();
    let card = create_test_card("card-note", 2, 2, 300);
    engine.upsert_card(card);

    assert!(engine
        .update_card_note("card-note", Some("VIP customer renewal".to_string()))
        .is_ok());
    let card = engine.get_card("card-note").unwrap();
    assert_eq!(card.note, Some("VIP customer renewal".to_string()));
}

#[test]
fn test_topup_generation_export_and_redemption() {
    let engine = BillingEngine::new();
    let mut card = create_test_card("card-topup", 2, 2, 300);
    card.credit_total = 10_000_000;
    card.valid_until = Some(10_000);
    engine.upsert_card(card);

    // Generate single top-up code: +20M credits, +3600s validity
    let generated = generate_topup_code(20_000_000, 3_600, 5_000).unwrap();
    assert!(generated.raw_code.starts_with("topup-"));
    engine.upsert_topup_code(generated.topup.clone());

    // Redeem top-up code
    let ledger_entry = engine
        .redeem_topup("card-topup", &generated.raw_code, 6_000, "admin")
        .unwrap();
    assert_eq!(ledger_entry.credits_charged, 20_000_000);
    assert_eq!(ledger_entry.kind, LedgerKind::Topup);

    // Verify card balance and validity updated
    let card = engine.get_card("card-topup").unwrap();
    assert_eq!(card.credit_total, 30_000_000);
    assert_eq!(card.valid_until, Some(13_600)); // 10_000 + 3_600

    // Check ledger recorded the topup event
    let ledger = engine.ledger_entries();
    assert_eq!(ledger.len(), 1);
    assert_eq!(ledger[0].kind, LedgerKind::Topup);
    assert_eq!(ledger[0].credits_charged, 20_000_000);

    // Replay attempt fails
    let err = engine
        .redeem_topup("card-topup", &generated.raw_code, 7_000, "admin")
        .unwrap_err();
    assert!(matches!(err, BillingError::InvalidOrRedeemedTopupCode));
}

#[test]
fn test_topup_reactivates_expired_card() {
    let engine = BillingEngine::new();
    let mut card = create_test_card("card-expired-topup", 2, 2, 300);
    card.status = CardStatus::Expired;
    card.valid_until = Some(5_000);
    engine.upsert_card(card);

    let generated = generate_topup_code(15_000_000, 7_200, 8_000).unwrap();
    engine.upsert_topup_code(generated.topup.clone());

    // Redeem when current time is 10_000 (past old valid_until of 5_000)
    let _ = engine
        .redeem_topup("card-expired-topup", &generated.raw_code, 10_000, "admin")
        .unwrap();
    // New valid until should start from now (10_000) + 7_200 = 17_200
    let card = engine.get_card("card-expired-topup").unwrap();
    assert_eq!(card.status, CardStatus::Active); // Reactivated!
    assert_eq!(card.valid_until, Some(17_200));
}

#[test]
fn test_topup_batch_and_csv_json_export() {
    let batch = generate_topup_batch(50_000_000, 86_400, 10, 100).unwrap();
    assert_eq!(batch.len(), 10);

    let csv = export_topup_csv(&batch);
    assert!(csv.contains("id,raw_code,credit_amount,duration_extension_secs"));
    assert!(csv.contains("topup-"));

    let json = export_topup_json(&batch).unwrap();
    assert!(json.contains("topup-"));
    assert!(json.contains("50000000"));
}
