use billing::card::Card;
use billing::crypto::MasterKek;
use billing::engine::{read_snapshot_anchor, verify_ledger_archive, BillingEngine};
use std::fs;
use std::path::PathBuf;

fn temp_state_dir(prefix: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "kiro_test_{}_{}_{}",
        prefix,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn test_crash_recovery_before_anchor_update_loads_old_version() {
    let dir = temp_state_dir("crash_pre_anchor");
    let state_file = dir.join("billing_state.json");

    let engine = BillingEngine::new();
    engine.set_persistence_path(&state_file);

    let card_v1 = Card::new("card-crash-01", "grp-pro", 100_000);
    engine.upsert_card(card_v1);

    // Initial snapshot published successfully
    let card_check = engine.get_card("card-crash-01").unwrap();
    assert_eq!(card_check.credit_total, 100_000);

    let anchor_v1 = read_snapshot_anchor(&state_file).expect("anchor v1 must exist");
    assert_eq!(anchor_v1.sequence, 1);

    // Simulate crash during subsequent write: an incomplete or uncommitted generation file
    // exists on disk, but anchor STILL points to sequence 1.
    let uncommitted_gen = dir.join("billing_state.json.gen_2");
    fs::write(&uncommitted_gen, "{\"uncommitted\":\"garbage\"}").unwrap();

    // Restart engine and load state
    let recovered = BillingEngine::new();
    recovered
        .load_from_file(&state_file)
        .expect("load must succeed from committed anchor v1");

    let recovered_card = recovered.get_card("card-crash-01").unwrap();
    assert_eq!(recovered_card.credit_total, 100_000);

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_crash_recovery_after_anchor_commit_heals_path_and_loads_new_version() {
    let dir = temp_state_dir("crash_post_anchor");
    let state_file = dir.join("billing_state.json");

    let engine = BillingEngine::new();
    engine.set_persistence_path(&state_file);

    let card_v1 = Card::new("card-crash-02", "grp-pro", 100_000);
    engine.upsert_card(card_v1);

    // Update to v2: adjust balance
    let _ = engine
        .adjust_balance("card-crash-02", 50_000, "admin", "bonus", 1000)
        .unwrap();

    let anchor_v2 = read_snapshot_anchor(&state_file).expect("anchor v2 must exist");
    assert_eq!(anchor_v2.sequence, 2);

    // Simulate crash after anchor update but before target state_file was updated:
    // Revert state_file to old/stale content, leaving anchor pointing to committed gen_2
    let stale_content = "{\"stale\":\"data_before_atomic_replace\"}";
    fs::write(&state_file, stale_content).unwrap();

    // Restart engine and load state
    let recovered = BillingEngine::new();
    recovered
        .load_from_file(&state_file)
        .expect("must recover from committed generation referenced by anchor");

    // The recovered engine must have the new balance (150,000)
    let recovered_card = recovered.get_card("card-crash-02").unwrap();
    assert_eq!(recovered_card.credit_total, 150_000);

    // Target state_file must have been self-healed!
    let healed_content = fs::read_to_string(&state_file).unwrap();
    assert!(!healed_content.contains("stale"));
    assert!(healed_content.contains("card-crash-02"));

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_corrupted_anchor_fails_closed() {
    let dir = temp_state_dir("corrupt_anchor");
    let state_file = dir.join("billing_state.json");

    let engine = BillingEngine::new();
    engine.set_persistence_path(&state_file);

    let card = Card::new("card-crash-03", "grp-pro", 50_000);
    engine.upsert_card(card);

    // Corrupt anchor file
    let anchor_path = dir.join("billing_state.json.anchor");
    fs::write(&anchor_path, "invalid json anchor content").unwrap();

    let recovered = BillingEngine::new();
    let load_res = recovered.load_from_file(&state_file);
    assert!(
        load_res.is_err(),
        "Load must fail closed when anchor is corrupted"
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_generation_file_pruning_and_retention() {
    let dir = temp_state_dir("gen_pruning");
    let state_file = dir.join("billing_state.json");

    let engine = BillingEngine::new();
    engine.set_persistence_path(&state_file);

    let card = Card::new("card-crash-04", "grp-pro", 10_000);
    engine.upsert_card(card); // seq 1

    let _ = engine.adjust_balance("card-crash-04", 1_000, "op", "r1", 1001); // seq 2
    let _ = engine.adjust_balance("card-crash-04", 1_000, "op", "r2", 1002); // seq 3
    let _ = engine.adjust_balance("card-crash-04", 1_000, "op", "r3", 1003); // seq 4

    let gen4 = dir.join("billing_state.json.gen_4");
    let gen3 = dir.join("billing_state.json.gen_3");
    let gen2 = dir.join("billing_state.json.gen_2");
    let gen1 = dir.join("billing_state.json.gen_1");

    // Current generation (4) and previous recoverable generation (3) must be retained
    assert!(gen4.exists(), "Generation 4 must exist");
    assert!(gen3.exists(), "Generation 3 must exist");

    // Older generations (< sequence - 1) must be pruned to avoid unbounded disk usage
    assert!(!gen2.exists(), "Generation 2 should be pruned");
    assert!(!gen1.exists(), "Generation 1 should be pruned");

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_require_anchor_strict_mode() {
    let dir = temp_state_dir("strict_anchor");
    let state_file = dir.join("billing_state.json");

    let engine = BillingEngine::new();
    let card = Card::new("card-crash-05", "grp-pro", 20_000);
    engine.upsert_card(card);

    // Save snapshot without anchor
    let snapshot = engine.export_snapshot();
    let json = serde_json::to_string_pretty(&snapshot).unwrap();
    fs::write(&state_file, json).unwrap();

    // Default loader accepts unanchored snapshot for backward compatibility
    let recovered_default = BillingEngine::new();
    assert!(recovered_default.load_from_file(&state_file).is_ok());

    // Strict mode requires anchor: must fail if anchor is missing
    let recovered_strict = BillingEngine::new();
    recovered_strict.set_require_anchor(true);
    let strict_res = recovered_strict.load_from_file(&state_file);
    assert!(
        strict_res.is_err(),
        "Strict mode must reject unanchored snapshot"
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_master_kek_rejects_unauthorized_plaintext_snapshot() {
    let dir = temp_state_dir("kek_reject_plain");
    let state_file = dir.join("billing_state.json");

    let engine = BillingEngine::new();
    let card = Card::new("card-crash-06", "grp-pro", 30_000);
    engine.upsert_card(card);

    // Save plain unencrypted JSON
    let snapshot = engine.export_snapshot();
    let json = serde_json::to_string_pretty(&snapshot).unwrap();
    fs::write(&state_file, json).unwrap();

    // When MasterKek is set, loading unencrypted snapshot without explicit authorization must fail
    let kek = MasterKek::generate_random().unwrap();
    let secure_engine = BillingEngine::new();
    secure_engine.set_master_kek(kek);

    let load_res = secure_engine.load_from_file(&state_file);
    assert!(
        load_res.is_err(),
        "MasterKek engine must reject plaintext snapshot without explicit authorization"
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_ledger_archival_drains_ledger_and_preserves_card_balance() {
    let dir = temp_state_dir("ledger_archival");
    let state_file = dir.join("billing_state.json");
    let archive_dir = dir.join("archives");

    let engine = BillingEngine::new();
    engine.set_persistence_path(&state_file);

    let card = Card::new("card-archive-01", "grp-pro", 1_000_000);
    engine.upsert_card(card);

    // Add 3 adjustments at timestamps 100, 200, 300
    let _ = engine.adjust_balance("card-archive-01", 10_000, "admin", "adj1", 100);
    let _ = engine.adjust_balance("card-archive-01", 20_000, "admin", "adj2", 200);
    let _ = engine.adjust_balance("card-archive-01", 30_000, "admin", "adj3", 300);

    let card_before = engine.get_card("card-archive-01").unwrap();
    assert_eq!(card_before.credit_total, 1_060_000);

    let entries_before = engine.list_ledger_entries_for_card("card-archive-01", None);
    assert_eq!(entries_before.len(), 3);

    // Archive entries before timestamp 250 (should drain entries at 100 and 200)
    let receipt = engine
        .archive_ledger(250, &archive_dir)
        .expect("archival must succeed");

    assert_eq!(receipt.drained_entries_count, 2);
    assert_eq!(receipt.before_ts_secs, 250);

    // Card balance MUST be completely unaffected by archival of historical ledger
    let card_after = engine.get_card("card-archive-01").unwrap();
    assert_eq!(card_after.credit_total, 1_060_000);

    // Active ledger now contains only 1 entry (ts 300)
    let entries_after = engine.list_ledger_entries_for_card("card-archive-01", None);
    assert_eq!(entries_after.len(), 1);
    assert_eq!(entries_after[0].ts_secs, 300);

    // Archive file on disk must be verifiable
    let archive_path = archive_dir.join(&receipt.archive_file);
    assert!(archive_path.exists());
    let verified_payload = verify_ledger_archive(&archive_path, &receipt.sha256_checksum)
        .expect("archive verification must succeed");
    assert_eq!(verified_payload.entries_count, 2);
    assert_eq!(verified_payload.entries[0].ts_secs, 100);
    assert_eq!(verified_payload.entries[1].ts_secs, 200);

    // Archival receipts are recorded in the engine
    let receipts = engine.list_archived_ledger_receipts();
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].archive_id, receipt.archive_id);

    // Restart engine from disk: state is preserved including archive receipts and active ledger
    let recovered = BillingEngine::new();
    recovered.load_from_file(&state_file).unwrap();

    let recovered_card = recovered.get_card("card-archive-01").unwrap();
    assert_eq!(recovered_card.credit_total, 1_060_000);

    let recovered_receipts = recovered.list_archived_ledger_receipts();
    assert_eq!(recovered_receipts.len(), 1);

    let recovered_active_entries = recovered.list_ledger_entries_for_card("card-archive-01", None);
    assert_eq!(recovered_active_entries.len(), 1);
    assert_eq!(recovered_active_entries[0].ts_secs, 300);

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn topup_duration_rejection_preserves_card_code_and_ledger_across_restart() {
    use billing::{generate_topup_code, BillingError};
    use serde_json::json;

    for perpetual in [false, true] {
        let dir = temp_state_dir("topup_duration_rejection");
        let path = dir.join("billing_state.json");
        let engine = BillingEngine::new();
        engine.set_master_kek(MasterKek::from_bytes([41; 32]));
        let mut card = Card::new("card", "group", 1_000);
        if perpetual {
            card.activate(100, 0).unwrap();
        } else {
            card.activation_duration_secs = Some(30 * 86_400);
        }
        engine.upsert_card(card);
        let topup = generate_topup_code(500, 7 * 86_400, 100).unwrap();
        let topup_id = topup.topup.id.clone();
        engine.upsert_topup_code(topup.topup).unwrap();
        engine.save_to_file(&path).unwrap();
        let before = engine.export_snapshot();
        let persisted = fs::read(&path).unwrap();

        let error = engine
            .redeem_topup("card", &topup.raw_code, 101, "test")
            .unwrap_err();
        assert!(matches!(&error, BillingError::InvalidState(_)));
        let message = error.to_string();
        assert!(message.contains(if perpetual { "perpetual" } else { "activate" }));
        assert!(message.contains("code has not been used"));
        assert_eq!(engine.snapshot_sequence(), before.sequence);
        assert_eq!(fs::read(&path).unwrap(), persisted);
        assert_eq!(
            json!(engine.get_card("card")),
            json!(before.cards.get("card"))
        );
        assert_eq!(
            json!(engine.export_snapshot().topup_codes),
            json!(before.topup_codes)
        );
        assert_eq!(json!(engine.ledger_entries()), json!(before.ledger));

        let recovered = BillingEngine::new();
        recovered.set_master_kek(MasterKek::from_bytes([41; 32]));
        recovered.load_from_file(&path).unwrap();
        assert_eq!(
            json!(recovered.get_card("card")),
            json!(before.cards.get("card"))
        );
        assert!(!recovered.export_snapshot().topup_codes[&topup_id].is_used);
        assert!(recovered.ledger_entries().is_empty());

        // Rejected codes remain usable, including on the same card after activation.
        let target = if perpetual {
            let mut finite = Card::new("finite", "group", 1_000);
            finite.activate(200, 30 * 86_400).unwrap();
            recovered.upsert_card(finite);
            "finite"
        } else {
            recovered.activate_card("card", 200, 30 * 86_400).unwrap();
            "card"
        };
        recovered
            .redeem_topup(target, &topup.raw_code, 201, "test")
            .unwrap();
        let restarted = BillingEngine::new();
        restarted.set_master_kek(MasterKek::from_bytes([41; 32]));
        restarted.load_from_file(&path).unwrap();
        let card = restarted.get_card(target).unwrap();
        assert_eq!(card.credit_total, 1_500);
        assert_eq!(card.valid_until, Some(200 + 37 * 86_400));
        assert_eq!(restarted.ledger_entries().len(), 1);
        assert_eq!(
            restarted.export_snapshot().topup_codes[&topup_id]
                .used_by_card_id
                .as_deref(),
            Some(target)
        );
        assert!(matches!(
            restarted.redeem_topup(target, &topup.raw_code, 202, "test"),
            Err(BillingError::InvalidOrRedeemedTopupCode)
        ));
        assert!(dir
            .canonicalize()
            .unwrap()
            .starts_with(std::env::temp_dir().canonicalize().unwrap()));
        fs::remove_dir_all(dir).unwrap();
    }
}

#[test]
fn topup_credit_only_preserves_unactivated_and_perpetual_validity() {
    for perpetual in [false, true] {
        let engine = BillingEngine::new();
        let mut card = Card::new("card", "group", 1_000);
        if perpetual {
            card.activate(100, 0).unwrap();
        } else {
            card.activation_duration_secs = Some(30 * 86_400);
        }
        engine.upsert_card(card.clone());
        let topup = billing::generate_topup_code(500, 0, 100).unwrap();
        engine.upsert_topup_code(topup.topup).unwrap();
        engine
            .redeem_topup("card", &topup.raw_code, 101, "test")
            .unwrap();
        let updated = engine.get_card("card").unwrap();
        assert_eq!(updated.credit_total, 1_500);
        assert_eq!(updated.status, card.status);
        assert_eq!(updated.valid_until, card.valid_until);
        assert_eq!(
            updated.activation_duration_secs,
            card.activation_duration_secs
        );
        assert_eq!(updated.activated_at, card.activated_at);
    }
}

#[test]
fn topup_write_failure_preserves_code_until_durable_retry() {
    use billing::{generate_topup_code, BillingError};
    use serde_json::json;

    let dir = temp_state_dir("topup_write_failure");
    let path = dir.join("billing_state.json");
    let engine = BillingEngine::new();
    let mut card = Card::new("card", "group", 1_000);
    card.activate(100, 30 * 86_400).unwrap();
    engine.upsert_card(card);
    let topup = generate_topup_code(500, 7 * 86_400, 100).unwrap();
    let topup_id = topup.topup.id.clone();
    engine.upsert_topup_code(topup.topup).unwrap();
    engine.save_to_file(&path).unwrap();
    let before = engine.export_snapshot();
    let persisted = fs::read(&path).unwrap();
    engine.inject_persistence_fault(true);
    assert!(matches!(
        engine.redeem_topup("card", &topup.raw_code, 101, "test"),
        Err(BillingError::Persistence(_))
    ));
    assert_eq!(engine.snapshot_sequence(), before.sequence);
    assert_eq!(fs::read(&path).unwrap(), persisted);
    assert_eq!(
        json!(engine.get_card("card")),
        json!(before.cards.get("card"))
    );
    assert!(!engine.export_snapshot().topup_codes[&topup_id].is_used);
    assert!(engine.ledger_entries().is_empty());

    let recovered = BillingEngine::new();
    recovered.load_from_file(&path).unwrap();
    assert_eq!(
        json!(recovered.get_card("card")),
        json!(before.cards.get("card"))
    );
    assert!(!recovered.export_snapshot().topup_codes[&topup_id].is_used);
    recovered
        .redeem_topup("card", &topup.raw_code, 102, "test")
        .unwrap();
    let restarted = BillingEngine::new();
    restarted.load_from_file(&path).unwrap();
    assert_eq!(restarted.get_card("card").unwrap().credit_total, 1_500);
    assert_eq!(
        restarted.get_card("card").unwrap().valid_until,
        Some(100 + 37 * 86_400)
    );
    assert!(restarted.export_snapshot().topup_codes[&topup_id].is_used);
    assert_eq!(restarted.ledger_entries().len(), 1);
    assert!(matches!(
        restarted.redeem_topup("card", &topup.raw_code, 103, "test"),
        Err(BillingError::InvalidOrRedeemedTopupCode)
    ));
    assert!(dir
        .canonicalize()
        .unwrap()
        .starts_with(std::env::temp_dir().canonicalize().unwrap()));
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn the_saved_state_size_is_reported_against_its_ceiling() {
    let dir = std::env::temp_dir().join(format!(
        "kiro-state-size-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("billing_state.json");
    let engine = BillingEngine::new();
    assert_eq!(engine.state_size().0, 0, "nothing saved yet");
    engine.set_persistence_path(&path);
    engine.upsert_card(billing::Card::new("card-size", "group", 1));

    let (bytes, ceiling) = engine.state_size();
    assert_eq!(bytes, std::fs::metadata(&path).unwrap().len());
    assert_eq!(ceiling, 256 * 1024 * 1024);
    const {
        assert!(billing::engine::STATE_WARNING_BYTES < billing::engine::STATE_URGENT_BYTES);
    }
    assert!(billing::engine::STATE_URGENT_BYTES < ceiling);
    let _ = std::fs::remove_dir_all(dir);
}
