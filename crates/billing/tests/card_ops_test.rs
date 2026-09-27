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
    assert!(engine
        .freeze_card("card-lifecycle", "admin", "test freeze", 1_000)
        .is_ok());
    let card = engine.get_card("card-lifecycle").unwrap();
    assert_eq!(card.status, CardStatus::Frozen);
    assert_eq!(card.token_version, 2); // Token version incremented on freeze (from 1 to 2)!

    // Unfreeze card
    assert!(engine
        .unfreeze_card("card-lifecycle", "admin", "test unfreeze", 1_100)
        .is_ok());
    let card = engine.get_card("card-lifecycle").unwrap();
    assert_eq!(card.status, CardStatus::Active);

    // Ban card
    assert!(engine
        .ban_card("card-lifecycle", "admin", "policy violation", 1_200)
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
    let results = engine.batch_freeze(&ids, "admin", "batch freeze test", 1_000);
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 3);

    for id in &ids {
        assert_eq!(engine.get_card(id).unwrap().status, CardStatus::Frozen);
    }

    let results = engine.batch_unfreeze(&ids, "admin", "batch unfreeze test", 1_100);
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 3);
    for id in &ids {
        assert_eq!(engine.get_card(id).unwrap().status, CardStatus::Active);
    }

    let results = engine.batch_ban(&ids, "admin", "batch ban test", 1_200);
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
    engine.upsert_topup_code(generated.topup.clone()).unwrap();

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
    engine.upsert_topup_code(generated.topup.clone()).unwrap();

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

#[test]
fn a_topup_code_that_could_not_be_saved_does_not_exist() {
    let dir = std::env::temp_dir().join(format!(
        "kiro-topup-durable-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let engine = BillingEngine::new();
    engine.set_persistence_path(dir.join("billing_state.json"));
    let generated = generate_topup_code(20_000_000, 3_600, 5_000).unwrap();

    engine.inject_persistence_fault(true);
    assert!(matches!(
        engine.upsert_topup_code(generated.topup.clone()),
        Err(BillingError::Persistence(_))
    ));
    engine.inject_persistence_fault(false);
    assert!(
        engine.get_topup_code(&generated.topup.id).is_none(),
        "a code that is not on disk must not be redeemable"
    );

    engine.upsert_topup_code(generated.topup.clone()).unwrap();
    assert!(engine.get_topup_code(&generated.topup.id).is_some());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn card_history_names_who_changed_a_card_and_why_newest_first() {
    let engine = BillingEngine::new();
    let mut card = create_test_card("card-history", 2, 2, 300);
    card.created_at = 900;
    card.issued_credits = Some(100_000_000);
    engine.upsert_card(card);

    engine
        .freeze_card("card-history", "admin", "客户要求暂停", 1_100)
        .unwrap();
    engine
        .unfreeze_card("card-history", "admin", " ", 1_200)
        .unwrap();
    engine
        .adjust_balance("card-history", 5_000_000, "admin", "补偿", 1_300)
        .unwrap();
    engine
        .ban_card("card-history", "system", "滥用", 1_400)
        .unwrap();

    // A refused change leaves nothing behind, and every change names who made it.
    assert!(engine
        .unfreeze_card("card-history", "admin", "retry", 1_500)
        .is_err());
    let spare = create_test_card("card-spare", 2, 2, 300);
    engine.upsert_card(spare.clone());
    assert!(engine
        .freeze_card("card-spare", " ", "no one", 1_500)
        .is_err());
    assert_eq!(engine.get_card("card-spare").unwrap(), spare);
    assert_eq!(engine.card_history("card-spare").unwrap().len(), 1);

    let history = engine.card_history("card-history").unwrap();
    let summary: Vec<_> = history
        .iter()
        .map(|event| {
            (
                event.ts_secs,
                event.action.as_str(),
                event.credits,
                event.operator.as_deref(),
                event.reason.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            (1_400, "ban", 0, Some("system"), Some("滥用")),
            (1_300, "adjust", 5_000_000, Some("admin"), Some("补偿")),
            (1_200, "unfreeze", 0, Some("admin"), None),
            (1_100, "freeze", 0, Some("admin"), Some("客户要求暂停")),
            (1_000, "activated", 0, None, None),
            (900, "issued", 100_000_000, None, None),
        ]
    );
    // Status changes move no credits: only the adjustment changed the balance.
    assert_eq!(
        engine.get_card("card-history").unwrap().credit_total,
        105_000_000
    );
    assert!(engine.card_history("no-such-card").is_none());
}

/// The events of `action` in a card's history, newest first.
fn history_of(
    engine: &BillingEngine,
    card_id: &str,
    action: &str,
) -> Vec<billing::card::CardEvent> {
    engine
        .card_history(card_id)
        .unwrap()
        .into_iter()
        .filter(|event| event.action == action)
        .collect()
}

#[test]
fn unbanning_restores_the_card_but_not_its_sessions() {
    let engine = BillingEngine::new();
    engine.upsert_card(create_test_card("card-unban", 1, 5, 0));
    let mut never_used = Card::new("card-unban-new", "grp-1", 1_000_000);
    never_used.status = CardStatus::Unactivated;
    engine.upsert_card(never_used);
    for id in ["card-unban", "card-unban-new"] {
        engine.ban_card(id, "admin", "滥用", 1_100).unwrap();
    }
    let banned = engine.get_card("card-unban").unwrap();

    let card = engine
        .unban_card("card-unban", "admin", "误封", 1_200)
        .unwrap();
    assert_eq!(card.status, CardStatus::Active);
    // The ban revoked its sessions; lifting it does not bring them back.
    assert_eq!(card.token_version, banned.token_version);
    // A card banned before it was ever used goes back to waiting for activation.
    let card = engine
        .unban_card("card-unban-new", "admin", "误封", 1_200)
        .unwrap();
    assert_eq!(card.status, CardStatus::Unactivated);
    assert_eq!(card.valid_until, None);

    let events = history_of(&engine, "card-unban", "unban");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].operator.as_deref(), Some("admin"));
    assert_eq!(events[0].reason.as_deref(), Some("误封"));
    assert_eq!(events[0].credits, 0);

    // Only a banned card, and not an archived one, can be unbanned.
    let before = engine.get_card("card-unban").unwrap();
    assert!(matches!(
        engine.unban_card("card-unban", "admin", "again", 1_300),
        Err(BillingError::InvalidState(_))
    ));
    assert_eq!(engine.get_card("card-unban").unwrap(), before);
    engine
        .ban_card("card-unban", "admin", "滥用", 1_400)
        .unwrap();
    engine
        .set_card_archived("card-unban", true, "admin", "清理", 1_500)
        .unwrap();
    assert!(matches!(
        engine.unban_card("card-unban", "admin", "误封", 1_600),
        Err(BillingError::InvalidState(message)) if message.contains("unarchived")
    ));
    assert_eq!(
        engine.get_card("card-unban").unwrap().status,
        CardStatus::Banned
    );
}

#[test]
fn an_operator_unbinding_frees_the_seat_without_using_the_customers_allowance() {
    let engine = BillingEngine::new();
    let mut card = create_test_card("card-seat", 1, 1, 86_400);
    card.rebind_count = 1;
    card.last_rebind_at = Some(1_000);
    engine.upsert_card(card);
    engine
        .bind_device("card-seat", "device-old", 1_000)
        .unwrap();
    let before = engine.get_card("card-seat").unwrap();
    // The customer has used their only unbinding: they cannot free the seat themselves.
    assert!(engine.unbind_device("card-seat", "device-old").is_err());

    let card = engine
        .admin_unbind_device("card-seat", "device-old", "admin", "换电脑", 1_100)
        .unwrap();
    assert!(card.bound_devices.is_empty());
    assert_eq!(card.token_version, before.token_version + 1);
    assert_eq!((card.rebind_count, card.last_rebind_at), (1, Some(1_000)));
    // The next sign-in binds the new device.
    engine
        .bind_device("card-seat", "device-new", 1_200)
        .unwrap();
    assert_eq!(
        engine.get_card("card-seat").unwrap().bound_devices,
        ["device-new"]
    );

    let events = history_of(&engine, "card-seat", "unbind");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].reason.as_deref(), Some("换电脑"));
    assert_eq!(
        events[0].detail,
        Some(serde_json::json!({ "deviceId": "device-old" }))
    );

    let before = engine.get_card("card-seat").unwrap();
    assert!(matches!(
        engine.admin_unbind_device("card-seat", "device-old", "admin", "again", 1_300),
        Err(BillingError::DeviceNotFound { .. })
    ));
    assert_eq!(engine.get_card("card-seat").unwrap(), before);
}

#[test]
fn resetting_the_rebind_allowance_clears_the_count_and_the_cooldown() {
    let engine = BillingEngine::new();
    let mut card = create_test_card("card-rebinds", 1, 2, 86_400);
    card.rebind_count = 2;
    card.last_rebind_at = Some(1_000);
    engine.upsert_card(card);
    engine.bind_device("card-rebinds", "device", 1_000).unwrap();

    let card = engine
        .reset_rebinds("card-rebinds", "admin", "客户多次换机", 1_100)
        .unwrap();
    assert_eq!((card.rebind_count, card.last_rebind_at), (0, None));
    assert_eq!(card.rebind_cooldown_until(1_100), None);
    // The customer may unbind at once again.
    engine.unbind_device("card-rebinds", "device").unwrap();

    let events = history_of(&engine, "card-rebinds", "rebinds_reset");
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].detail,
        Some(serde_json::json!({ "previousRebinds": 2, "previousCooldownUntil": 87_400 }))
    );
    // Nothing to reset writes nothing.
    engine.upsert_card(create_test_card("card-clear", 1, 2, 86_400));
    engine
        .reset_rebinds("card-clear", "admin", "nothing", 1_100)
        .unwrap();
    assert!(history_of(&engine, "card-clear", "rebinds_reset").is_empty());
}

#[test]
fn extending_validity_moves_expiries_and_revives_expired_cards() {
    let engine = BillingEngine::new();
    let now = 100_000;
    let mut current = create_test_card("card-current", 1, 5, 0);
    current.valid_until = Some(now + 86_400);
    let mut lapsed = create_test_card("card-lapsed", 1, 5, 0);
    lapsed.valid_until = Some(now - 86_400);
    let mut expired = create_test_card("card-expired", 1, 5, 0);
    expired.status = CardStatus::Expired;
    expired.valid_until = Some(now - 10);
    let mut frozen = create_test_card("card-frozen", 1, 5, 0);
    frozen.valid_until = Some(now - 10);
    let mut waiting = Card::new("card-waiting", "grp-1", 1_000_000);
    waiting.activation_duration_secs = Some(30 * 86_400);
    let legacy = Card::new("card-legacy", "grp-1", 1_000_000);
    for card in [current, lapsed, expired, frozen, waiting, legacy] {
        engine.upsert_card(card);
    }
    engine
        .freeze_card("card-frozen", "admin", "暂停", now - 5)
        .unwrap();
    let ids: Vec<String> = [
        "card-current",
        "card-lapsed",
        "card-expired",
        "card-frozen",
        "card-waiting",
        "card-legacy",
        "card-current",
    ]
    .iter()
    .map(|id| id.to_string())
    .collect();

    let cards = engine
        .extend_validity(
            &ids,
            billing::ValidityExtension::Days(10),
            "admin",
            "补偿停机",
            now,
        )
        .unwrap();
    assert_eq!(cards.len(), 6, "a card named twice is extended once");
    let card = |id: &str| engine.get_card(id).unwrap();
    // From its expiry while it runs, from now once it has ended.
    assert_eq!(card("card-current").valid_until, Some(now + 11 * 86_400));
    assert_eq!(card("card-lapsed").valid_until, Some(now + 10 * 86_400));
    assert!(card("card-lapsed").check_active(now).is_ok());
    assert_eq!(card("card-expired").status, CardStatus::Active);
    assert!(card("card-expired").check_active(now).is_ok());
    // A frozen card stays frozen, and unfreezes into a usable card.
    assert_eq!(card("card-frozen").status, CardStatus::Frozen);
    engine
        .unfreeze_card("card-frozen", "admin", "恢复", now + 1)
        .unwrap();
    assert!(card("card-frozen").check_active(now + 1).is_ok());
    // Not yet activated: valid that much longer from activation.
    assert_eq!(
        card("card-waiting").activation_duration_secs,
        Some(40 * 86_400)
    );
    assert_eq!(card("card-waiting").valid_until, None);
    assert_eq!(
        card("card-legacy").activation_duration_secs,
        Some(40 * 86_400)
    );

    let events = history_of(&engine, "card-current", "extend");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].reason.as_deref(), Some("补偿停机"));
    // The new expiry and the one it replaced, so that a mistaken extension can be undone.
    assert_eq!(
        events[0].detail,
        Some(serde_json::json!({
            "validUntil": now + 11 * 86_400,
            "previousValidUntil": now + 86_400,
        }))
    );
    assert_eq!(
        history_of(&engine, "card-waiting", "extend")[0].detail,
        Some(serde_json::json!({
            "activationDurationSecs": 40 * 86_400,
            "previousActivationDurationSecs": 30 * 86_400,
        }))
    );
    // A legacy card counted from the legacy validity.
    assert_eq!(
        history_of(&engine, "card-legacy", "extend")[0]
            .detail
            .as_ref()
            .unwrap()["previousActivationDurationSecs"],
        30 * 86_400
    );

    // To a time: activated cards only, and never earlier than their expiry.
    let until = now + 20 * 86_400;
    let cards = engine
        .extend_validity(
            &["card-current".to_string()],
            billing::ValidityExtension::Until(until),
            "admin",
            "续期",
            now,
        )
        .unwrap();
    assert_eq!(cards[0].valid_until, Some(until));
}

#[test]
fn a_validity_extension_is_refused_whole_naming_the_cards_it_cannot_extend() {
    let engine = BillingEngine::new();
    let now = 100_000;
    let mut current = create_test_card("card-ok", 1, 5, 0);
    current.valid_until = Some(now + 86_400);
    let mut voided = create_test_card("card-voided", 1, 5, 0);
    voided.status = CardStatus::Voided;
    let mut archived = create_test_card("card-archived", 1, 5, 0);
    archived.status = CardStatus::Banned;
    archived.archived_at = Some(1);
    let mut perpetual = create_test_card("card-perpetual", 1, 5, 0);
    perpetual.valid_until = None;
    let mut forever = Card::new("card-forever", "grp-1", 1_000_000);
    forever.activation_duration_secs = Some(0);
    let waiting = Card::new("card-waiting", "grp-1", 1_000_000);
    for card in [current, voided, archived, perpetual, forever, waiting] {
        engine.upsert_card(card);
    }
    let days = billing::ValidityExtension::Days(5);
    let refused = |ids: &[&str], extension| {
        let ids: Vec<String> = ids.iter().map(|id| id.to_string()).collect();
        engine
            .extend_validity(&ids, extension, "admin", "续期", now)
            .unwrap_err()
            .to_string()
    };
    let before = engine.export_snapshot();
    assert_eq!(
        refused(&["card-ok", "card-missing"], days),
        "Card card-missing not found"
    );
    assert!(refused(&["card-ok", "card-voided"], days)
        .ends_with("Voided cards cannot be extended: card-voided"));
    assert!(refused(&["card-archived", "card-ok"], days)
        .ends_with("Archived cards must be unarchived before they are extended: card-archived"));
    assert!(refused(&["card-perpetual", "card-forever"], days)
        .ends_with("Cards that never expire cannot be extended: card-perpetual, card-forever"));
    let until = billing::ValidityExtension::Until(now + 3_600);
    assert!(refused(&["card-ok", "card-waiting"], until)
        .ends_with("An expiry date applies only to activated cards: card-waiting"));
    assert!(refused(&["card-ok"], until)
        .ends_with("The new expiry is earlier than the current one of: card-ok"));
    for extension in [
        billing::ValidityExtension::Days(0),
        billing::ValidityExtension::Until(now),
    ] {
        assert!(matches!(
            engine.extend_validity(&["card-ok".into()], extension, "admin", "x", now),
            Err(BillingError::InvalidAdjustment(_))
        ));
    }
    // Nothing changed, not even the cards that could have been extended.
    let after = engine.export_snapshot();
    assert_eq!(after.cards, before.cards);
    assert_eq!(after.ledger.len(), before.ledger.len());
}

#[test]
fn a_note_change_records_who_made_it() {
    let engine = BillingEngine::new();
    engine.upsert_card(create_test_card("card-noted", 1, 5, 0));
    let card = engine
        .set_card_note("card-noted", Some("VIP 续费客户"), "admin", 1_100)
        .unwrap();
    assert_eq!(card.note.as_deref(), Some("VIP 续费客户"));
    // The same note again is no change.
    engine
        .set_card_note("card-noted", Some("VIP 续费客户"), "admin", 1_150)
        .unwrap();
    let card = engine
        .set_card_note("card-noted", None, "admin", 1_200)
        .unwrap();
    assert_eq!(card.note, None);
    let events = history_of(&engine, "card-noted", "note");
    assert_eq!(events.len(), 2);
    assert!(events
        .iter()
        .all(|event| event.operator.as_deref() == Some("admin") && event.reason.is_none()));
    // Each records the note it wrote and the one it replaced, newest first.
    let details: Vec<_> = events.iter().map(|event| event.detail.clone()).collect();
    assert_eq!(
        details,
        [
            Some(serde_json::json!({ "note": null, "previousNote": "VIP 续费客户" })),
            Some(serde_json::json!({ "note": "VIP 续费客户", "previousNote": null })),
        ]
    );
}

/// Lifting a ban lifts no freeze, and takes back what the ban put before the note.
#[test]
fn unbanning_a_card_banned_while_frozen_leaves_it_frozen() {
    let engine = BillingEngine::new();
    let mut card = create_test_card("card-held", 1, 5, 0);
    card.note = Some("VIP 客户".into());
    engine.upsert_card(card);
    engine
        .freeze_card("card-held", "admin", "调查中", 1_000)
        .unwrap();
    // Banned twice, once for a reason with a bracket of its own.
    engine
        .ban_card("card-held", "admin", "滥用 [第二次]", 1_100)
        .unwrap();
    engine
        .ban_card("card-held", "admin", "再次滥用", 1_150)
        .unwrap();
    assert_eq!(
        engine.get_card("card-held").unwrap().note.as_deref(),
        Some("[BANNED: 再次滥用] [BANNED: 滥用 [第二次]] [FROZEN: 调查中] VIP 客户")
    );

    let card = engine
        .unban_card("card-held", "admin", "误封", 1_200)
        .unwrap();
    assert_eq!(card.status, CardStatus::Frozen);
    assert_eq!(card.frozen_from, Some(CardStatus::Active));
    assert_eq!(card.note.as_deref(), Some("[FROZEN: 调查中] VIP 客户"));
    assert_eq!(
        history_of(&engine, "card-held", "unban")[0].detail,
        Some(serde_json::json!({ "status": "frozen" }))
    );
    // Unfrozen, it is what it was frozen from.
    let card = engine
        .unfreeze_card("card-held", "admin", "调查结束", 1_300)
        .unwrap();
    assert_eq!(card.status, CardStatus::Active);

    // A ban's note alone leaves none; one no ban in the history wrote ends at its bracket.
    let mut legacy = create_test_card("card-legacy-ban", 1, 5, 0);
    legacy.status = CardStatus::Banned;
    legacy.note = Some("[BANNED: 旧版封禁] 老客户".into());
    engine.upsert_card(legacy);
    let card = engine
        .unban_card("card-legacy-ban", "admin", "误封", 1_400)
        .unwrap();
    assert_eq!(
        (card.status, card.note.as_deref()),
        (CardStatus::Active, Some("老客户"))
    );
    engine.upsert_card(create_test_card("card-bare", 1, 5, 0));
    engine
        .ban_card("card-bare", "admin", "滥用", 1_500)
        .unwrap();
    let card = engine
        .unban_card("card-bare", "admin", "误封", 1_600)
        .unwrap();
    assert_eq!(card.note, None);
    assert_eq!(
        history_of(&engine, "card-bare", "unban")[0].detail,
        Some(serde_json::json!({ "status": "active" }))
    );
}

fn fixed_price(id: &str) -> billing::rate_card::RateCardVersion {
    billing::rate_card::RateCardVersion {
        id: id.to_string(),
        rate_card_id: "default".to_string(),
        model: "support-model".to_string(),
        currency: billing::rate_card::Currency::Cny,
        pricing_mode: billing::rate_card::PricingMode::Fixed,
        input_price_per_m: 0.0,
        output_price_per_m: 0.0,
        cache_creation_price_per_m: 0.0,
        cache_read_price_per_m: 0.0,
        fixed_input_credit_per_m: 1_000_000,
        fixed_output_credit_per_m: 1_000_000,
        fixed_cache_creation_credit_per_m: 0,
        fixed_cache_read_credit_per_m: 0,
        per_call_credit: 0,
        margin_multiplier: 1.0,
        effective_from_secs: 0,
        official: None,
    }
}

#[test]
fn a_group_change_ends_sessions_and_keeps_in_flight_requests_at_their_price() {
    let engine = BillingEngine::new();
    let mut dearer = billing::Group::pro_plus("group-dearer", "Dearer");
    dearer.margin_multiplier = 3.0;
    engine.upsert_group(dearer);
    let mut closed = billing::Group::pro_plus("group-acceptance", "Acceptance");
    closed.issuance_enabled = false;
    engine.upsert_group(closed);
    engine.upsert_rate_card_version(fixed_price("price-support"));
    let mut card = create_test_card("card-moved", 1, 5, 0);
    card.valid_until = None;
    engine.upsert_card(card);
    let tokens = billing::ledger::UsageTokens {
        uncached_input_tokens: 1_000_000,
        output_tokens: 1_000_000,
        cache_creation_tokens: 0,
        cache_read_tokens: 0,
    };
    let params = billing::reservation::ReservationEstimateParams::new(1_000, 1_000)
        .with_model("support-model");
    engine
        .reserve("card-moved", "inv-in-flight", &params, 1_000, 600)
        .unwrap();
    let before = engine.get_card("card-moved").unwrap();

    let card = engine
        .change_card_group("card-moved", "group-dearer", "admin", "升级套餐", 1_100)
        .unwrap();
    assert_eq!(card.group_id, "group-dearer");
    assert_eq!(card.token_version, before.token_version + 1);
    // Reserved in the old group: charged as it was reserved.
    let in_flight = engine
        .settle(
            "inv-in-flight",
            &tokens,
            "support-model",
            "prov",
            "target",
            1_200,
        )
        .unwrap();
    assert_eq!(in_flight.credits_charged, 2_000_000);
    engine
        .reserve("card-moved", "inv-after", &params, 1_300, 600)
        .unwrap();
    let after = engine
        .settle(
            "inv-after",
            &tokens,
            "support-model",
            "prov",
            "target",
            1_300,
        )
        .unwrap();
    assert_eq!(after.credits_charged, 6_000_000);

    let events = history_of(&engine, "card-moved", "group");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].reason.as_deref(), Some("升级套餐"));
    assert_eq!(
        events[0].detail,
        Some(serde_json::json!({ "previousGroupId": "grp-1", "groupId": "group-dearer" }))
    );

    let before = engine.get_card("card-moved").unwrap();
    for (group, message) in [
        ("group-missing", "Unknown group: group-missing"),
        (
            "group-acceptance",
            "Group does not take cards: group-acceptance",
        ),
    ] {
        assert!(matches!(
            engine.change_card_group("card-moved", group, "admin", "x", 1_400),
            Err(BillingError::InvalidState(refusal)) if refusal == message
        ));
    }
    // Already there: nothing changes and nothing is written.
    engine
        .change_card_group("card-moved", "group-dearer", "admin", "again", 1_400)
        .unwrap();
    assert_eq!(engine.get_card("card-moved").unwrap(), before);
    assert_eq!(history_of(&engine, "card-moved", "group").len(), 1);
}

/// A request the card made and was charged `credits` for, as its trace records it.
fn charged_request(engine: &BillingEngine, card_id: &str, invocation_id: &str, credits: i64) {
    engine.record_trace(billing::RequestTrace {
        id: format!("trace-{invocation_id}"),
        card_id: card_id.into(),
        ts: 1_050,
        invocation_id: invocation_id.into(),
        exposed_model: "model".into(),
        status: billing::TraceStatus::Success,
        credits_charged: credits,
        ..billing::RequestTrace::default()
    });
}

fn request(invocation_id: &str) -> billing::CompensatedRequest<'_> {
    billing::CompensatedRequest {
        invocation_id,
        allow_repeat: false,
    }
}

#[test]
fn an_adjustment_keeps_the_request_it_makes_up_for() {
    let engine = BillingEngine::new();
    engine.upsert_card(create_test_card("card-comp", 1, 5, 0));
    charged_request(&engine, "card-comp", "card-comp:inv-broken", 5_000_000);
    let adjust = |invocation: Option<&str>| {
        engine.adjust_balance_linked(
            "card-comp",
            2_000_000,
            "admin",
            "补偿失败请求",
            1_100,
            Some("comp-1"),
            invocation.map(request),
        )
    };
    adjust(Some("card-comp:inv-broken")).unwrap();
    // A retry is the same adjustment; the same key naming another request is not.
    adjust(Some("card-comp:inv-broken")).unwrap();
    for other in [None, Some("card-comp:inv-other")] {
        assert!(matches!(
            adjust(other),
            Err(BillingError::InvalidAdjustment(message)) if message.starts_with("Idempotency conflict")
        ));
    }
    assert_eq!(
        engine.get_card("card-comp").unwrap().credit_total,
        102_000_000
    );
    let events = history_of(&engine, "card-comp", "adjust");
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].invocation_id.as_deref(),
        Some("card-comp:inv-broken")
    );
    assert_eq!(events[0].detail, None);
    // An adjustment without one names none.
    engine
        .adjust_balance("card-comp", 1_000_000, "admin", "赠送", 1_200)
        .unwrap();
    assert_eq!(
        history_of(&engine, "card-comp", "adjust")[0].invocation_id,
        None
    );
}

/// What a change records besides who and why is kept in the entry's own `detail`, and the
/// history reads the same from entries written before it, which kept it as JSON where a
/// usage entry names its model; a retried adjustment is still recognised across the two.
#[test]
fn event_detail_has_its_own_field_and_older_entries_still_read() {
    use serde_json::json;
    let engine = BillingEngine::new();
    let mut card = create_test_card("card-detail", 1, 5, 0);
    card.bound_devices = vec!["device-old".into()];
    engine.upsert_card(card);
    let mut waiting = Card::new("card-detail-new", "grp-1", 1_000_000);
    waiting.activation_duration_secs = Some(86_400);
    engine.upsert_card(waiting);
    engine.upsert_group(billing::Group::pro_plus("grp-2", "Second"));
    engine
        .admin_unbind_device("card-detail", "device-old", "admin", "换电脑", 1_100)
        .unwrap();
    engine
        .extend_validity(
            &["card-detail".into(), "card-detail-new".into()],
            billing::ValidityExtension::Days(2),
            "admin",
            "补偿",
            1_200,
        )
        .unwrap();
    engine
        .change_card_group("card-detail", "grp-2", "admin", "升级", 1_300)
        .unwrap();
    charged_request(&engine, "card-detail", "card-detail:inv-1", 3_000_000);
    let linked = |engine: &BillingEngine, invocation: &str| {
        engine.adjust_balance_linked(
            "card-detail",
            1_000_000,
            "admin",
            "补偿失败请求",
            1_400,
            Some("comp-detail"),
            Some(request(invocation)),
        )
    };
    linked(&engine, "card-detail:inv-1").unwrap();
    engine
        .set_card_note("card-detail", Some("VIP"), "admin", 1_500)
        .unwrap();
    engine
        .freeze_card("card-detail", "admin", "暂停", 1_600)
        .unwrap();

    let entries: Vec<_> = engine
        .ledger_entries()
        .into_iter()
        .filter(|entry| entry.kind == LedgerKind::Adjustment)
        .collect();
    let written: Vec<_> = entries
        .iter()
        .map(|entry| (entry.target_model.as_str(), entry.detail.clone()))
        .collect();
    assert_eq!(
        written,
        [
            ("unbind_device", Some(json!({ "deviceId": "device-old" }))),
            // Its expiry, 87,400, two days later.
            (
                "extend_validity",
                Some(json!({ "validUntil": 260_200, "previousValidUntil": 87_400 }))
            ),
            (
                "extend_validity",
                Some(json!({
                    "activationDurationSecs": 259_200,
                    "previousActivationDurationSecs": 86_400,
                }))
            ),
            (
                "change_group",
                Some(json!({ "previousGroupId": "grp-1", "groupId": "grp-2" }))
            ),
            (
                "adjustment",
                Some(json!({ "invocationId": "card-detail:inv-1" }))
            ),
            (
                "note_card",
                Some(json!({ "note": "VIP", "previousNote": null }))
            ),
            ("freeze_card", None),
        ]
    );
    // Absent, the field is not written at all.
    let freeze = serde_json::to_value(&entries[6]).unwrap();
    assert!(freeze.get("detail").is_none());
    let history = |engine: &BillingEngine| {
        (
            engine.card_history("card-detail").unwrap(),
            engine.card_history("card-detail-new").unwrap(),
        )
    };
    let now = history(&engine);
    let adjusted = now.0.iter().find(|e| e.action == "adjust").unwrap();
    assert_eq!(adjusted.invocation_id.as_deref(), Some("card-detail:inv-1"));
    assert_eq!(adjusted.detail, None);
    let unbound = now.0.iter().find(|e| e.action == "unbind").unwrap();
    assert_eq!(unbound.detail, Some(json!({ "deviceId": "device-old" })));

    // The same state as a release before the field saved it.
    let mut older = engine.export_snapshot();
    for entry in &mut older.ledger {
        if let Some(detail) = entry.detail.take() {
            entry.target_model = detail.to_string();
        }
    }
    let restored = BillingEngine::new();
    restored.import_snapshot(older);
    assert_eq!(history(&restored), now);
    // A retry of the adjustment is recognised, and the same key naming another request
    // is still refused.
    let retried = linked(&restored, "card-detail:inv-1").unwrap();
    assert_eq!(retried.detail, None);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&retried.target_model).unwrap(),
        json!({ "invocationId": "card-detail:inv-1" })
    );
    assert!(matches!(
        linked(&restored, "card-detail:inv-2"),
        Err(BillingError::InvalidAdjustment(message)) if message.starts_with("Idempotency conflict")
    ));
    assert_eq!(
        restored.get_card("card-detail").unwrap().credit_total,
        engine.get_card("card-detail").unwrap().credit_total
    );
}

/// A compensation names a request of this card that was made. It is paid once, and never
/// beyond what the request was charged, unless the operator explicitly allows it.
#[test]
fn a_request_is_compensated_once_and_at_most_what_it_was_charged() {
    let engine = BillingEngine::new();
    engine.upsert_card(create_test_card("card-a", 1, 5, 0));
    engine.upsert_card(create_test_card("card-b", 1, 5, 0));
    // Charged 3 credits, settled from the live ledger.
    engine.upsert_rate_card_version(fixed_price("price-support"));
    let params = billing::reservation::ReservationEstimateParams::new(1_000, 1_000)
        .with_model("support-model");
    engine
        .reserve("card-a", "card-a:inv-1", &params, 1_000, 600)
        .unwrap();
    let tokens = billing::ledger::UsageTokens {
        uncached_input_tokens: 1_000_000,
        output_tokens: 2_000_000,
        cache_creation_tokens: 0,
        cache_read_tokens: 0,
    };
    engine
        .settle(
            "card-a:inv-1",
            &tokens,
            "support-model",
            "prov",
            "target",
            1_700_000_000,
        )
        .unwrap();
    charged_request(&engine, "card-b", "card-b:inv-9", 1_000_000);
    let compensate = |card: &str, credits: i64, key: &str, request: billing::CompensatedRequest| {
        engine
            .adjust_balance_linked(
                card,
                credits,
                "admin",
                "补偿失败请求",
                1_700_000_100,
                Some(key),
                Some(request),
            )
            .map_err(|error| error.to_string())
    };

    // A request that was never made, or another card's.
    assert_eq!(
        compensate("card-a", 1_000_000, "k-unknown", request("card-a:nope")).unwrap_err(),
        "Request card-a:nope was not found"
    );
    assert_eq!(
        compensate("card-a", 1_000_000, "k-other", request("card-b:inv-9")).unwrap_err(),
        "Request card-b:inv-9 was made by card card-b, not card-a"
    );
    // A card that does not exist is named as such.
    assert_eq!(
        compensate("card-x", 1_000_000, "k-no-card", request("card-b:inv-9")).unwrap_err(),
        "Card card-x not found"
    );
    // More than it was charged.
    assert_eq!(
        compensate("card-a", 3_500_000, "k-more", request("card-a:inv-1")).unwrap_err(),
        "Request card-a:inv-1 was charged 3 credits at 2023-11-14T22:13:20Z; a compensation of \
         3.5 credits is more than that; send allowRepeat with a reason to compensate more"
    );
    compensate("card-a", 2_000_000, "k-first", request("card-a:inv-1")).unwrap();
    // A second one with a fresh key names the first.
    assert_eq!(
        compensate("card-a", 1_000_000, "k-second", request("card-a:inv-1")).unwrap_err(),
        "Request card-a:inv-1 was charged 3 credits at 2023-11-14T22:13:20Z, and was already \
         compensated 2 credits at 2023-11-14T22:15:00Z by admin (补偿失败请求); send \
         allowRepeat with a reason to compensate it again"
    );
    // The first one's retry is the same adjustment, not a second.
    compensate("card-a", 2_000_000, "k-first", request("card-a:inv-1")).unwrap();
    assert_eq!(engine.get_card("card-a").unwrap().credit_total, 102_000_000);

    // Explicitly allowed: again, and beyond the charge.
    let allowed = billing::CompensatedRequest {
        invocation_id: "card-a:inv-1",
        allow_repeat: true,
    };
    compensate("card-a", 5_000_000, "k-allowed", allowed).unwrap();
    assert_eq!(engine.get_card("card-a").unwrap().credit_total, 107_000_000);
    // A negative correction for the request is not a compensation, but must name it rightly.
    compensate(
        "card-a",
        -1_000_000,
        "k-correction",
        request("card-a:inv-1"),
    )
    .unwrap();
    assert!(compensate("card-a", -1_000_000, "k-wrong", request("card-b:inv-9")).is_err());
    // An adjustment naming no request is not checked against any.
    engine
        .adjust_balance_linked(
            "card-a",
            9_000_000,
            "admin",
            "赠送",
            1_700_000_200,
            Some("k-gift"),
            None,
        )
        .unwrap();
    let events = history_of(&engine, "card-a", "adjust");
    assert_eq!(events.len(), 4);
    assert!(events[1..]
        .iter()
        .all(|event| event.invocation_id.as_deref() == Some("card-a:inv-1")));
}

/// A request whose usage entry was archived can still be compensated, and still only once:
/// the archive is read for the charge, and archived adjustments are kept for the check.
#[test]
fn an_archived_request_is_found_and_its_compensation_still_counts() {
    let dir = std::env::temp_dir().join(format!(
        "kiro-compensation-archive-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let engine = BillingEngine::new();
    engine.set_persistence_path(dir.join("billing_state.json"));
    engine.upsert_card(create_test_card("card-old", 1, 5, 0));
    engine.upsert_rate_card_version(fixed_price("price-support"));
    let params = billing::reservation::ReservationEstimateParams::new(1_000, 1_000)
        .with_model("support-model");
    engine
        .reserve("card-old", "card-old:inv-1", &params, 1_000, 600)
        .unwrap();
    let tokens = billing::ledger::UsageTokens {
        uncached_input_tokens: 1_000_000,
        output_tokens: 1_000_000,
        cache_creation_tokens: 0,
        cache_read_tokens: 0,
    };
    engine
        .settle(
            "card-old:inv-1",
            &tokens,
            "support-model",
            "prov",
            "target",
            1_001,
        )
        .unwrap();
    engine
        .adjust_balance_linked(
            "card-old",
            1_000_000,
            "admin",
            "补偿",
            1_002,
            Some("k-old"),
            Some(request("card-old:inv-1")),
        )
        .unwrap();
    engine.prune_traces(u64::MAX);
    engine
        .archive_ledger(2_000, &engine.ledger_archive_dir().unwrap())
        .unwrap();
    assert!(engine.ledger_entries().is_empty());

    let again = engine.adjust_balance_linked(
        "card-old",
        1_000_000,
        "admin",
        "补偿",
        3_000,
        Some("k-again"),
        Some(request("card-old:inv-1")),
    );
    assert!(
        matches!(&again, Err(BillingError::CompensationRefused(message))
            if message.contains("was charged 2 credits") && message.contains("already compensated 1 credits")),
        "{again:?}"
    );
    let _ = std::fs::remove_dir_all(dir);
}
