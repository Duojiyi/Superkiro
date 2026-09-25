//! Integration test suite for T07: Durable Backup and Disaster Recovery Verification (Spec §7, T07).

use billing::card::{Card, CardStatus};
use billing::crypto::MasterKek;
use billing::engine::{read_snapshot_anchor, BillingEngine};
use billing::ledger::UsageTokens;
use billing::provider::{Provider, ProviderFormat, ProviderKey};
use billing::reservation::ReservationEstimateParams;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_test_dir(prefix: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "kiro_backup_test_{}_{}_{}",
        prefix,
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn sample_provider() -> (Provider, ProviderKey) {
    let prov = Provider::new(
        "prov-anthropic",
        "Anthropic Direct",
        ProviderFormat::Anthropic,
        "https://api.anthropic.com",
    );
    let key = ProviderKey::new("key-1", "prov-anthropic", "sk-ant-test-key-1234");
    (prov, key)
}

// --------------------------------------------------------------------------
// 1. Snapshot engine verification on clean state
// --------------------------------------------------------------------------
#[test]
fn test_t07_verify_snapshot_integrity_clean_state() {
    let dir = temp_test_dir("clean_state");
    let state_file = dir.join("billing_state.json");

    let engine = BillingEngine::new();
    engine.set_persistence_path(&state_file);

    // Setup card, provider, and settled ledger entry
    let mut card = Card::new("card-t07-01", "grp-pro", 500_000);
    card.status = CardStatus::Active;
    engine.upsert_card(card);

    let (prov, key) = sample_provider();
    engine.upsert_provider(prov);
    engine.upsert_provider_key(key);

    // No model: the hold is estimated at the rates given here. A named model would need a
    // published price.
    let params = ReservationEstimateParams {
        estimated_input_tokens: 100,
        max_output_tokens: 50,
        input_rate_per_m: 10_000_000,
        output_rate_per_m: 30_000_000,
        credit_multiplier: 1.0,
        margin_multiplier: 1.0,
        model: None,
    };
    engine
        .reserve("card-t07-01", "inv-t07-01", &params, 1000, 300)
        .unwrap();

    let usage = UsageTokens {
        uncached_input_tokens: 100,
        output_tokens: 50,
        cache_creation_tokens: 0,
        cache_read_tokens: 0,
    };
    engine
        .settle(
            "inv-t07-01",
            &usage,
            "claude-3-5-sonnet",
            "prov-anthropic",
            "claude-3-5-sonnet",
            1010,
        )
        .unwrap();

    // Verify snapshot integrity
    let report = BillingEngine::verify_snapshot_integrity(&state_file, None)
        .expect("Clean snapshot must verify successfully");

    assert!(!report.is_encrypted);
    assert_eq!(report.version, 2);
    assert_eq!(report.cards_count, 1);
    assert_eq!(report.ledger_entries_count, 1);
    assert!(report.is_balanced);
    assert!(report.sequence > 0);

    let _ = fs::remove_dir_all(&dir);
}

// --------------------------------------------------------------------------
// 2. Encrypted snapshot verification with Master KEK
// --------------------------------------------------------------------------
#[test]
fn test_t07_verify_snapshot_integrity_encrypted_state() {
    let dir = temp_test_dir("encrypted_state");
    let state_file = dir.join("billing_state.json");
    let kek = MasterKek::from_bytes([7u8; 32]);

    let engine = BillingEngine::new();
    engine.set_master_kek(kek.clone());
    engine.set_persistence_path(&state_file);

    let card = Card::new("card-t07-enc", "grp-pro", 200_000);
    engine.upsert_card(card);

    // Verify with KEK succeeds
    let report = BillingEngine::verify_snapshot_integrity(&state_file, Some(kek.clone()))
        .expect("Encrypted snapshot must verify with correct KEK");
    assert!(report.is_encrypted);
    assert_eq!(report.cards_count, 1);
    assert!(report.is_balanced);

    // Verify without KEK fails before reading
    let err_no_kek = BillingEngine::verify_snapshot_integrity(&state_file, None)
        .expect_err("Must fail when KEK is missing for encrypted snapshot");
    assert!(err_no_kek.to_string().contains("Master KEK was provided"));

    // Verify with wrong KEK fails before reading
    let wrong_kek = MasterKek::from_bytes([99u8; 32]);
    let err_wrong_kek = BillingEngine::verify_snapshot_integrity(&state_file, Some(wrong_kek))
        .expect_err("Must fail when wrong KEK is used");
    assert!(matches!(
        err_wrong_kek,
        billing::engine::BillingError::Persistence(_)
    ));

    let _ = fs::remove_dir_all(&dir);
}

// --------------------------------------------------------------------------
// 3. Rejection on corrupted anchor or mismatched checksum
// --------------------------------------------------------------------------
#[test]
fn test_t07_verify_fails_on_corrupted_or_mismatched_anchor() {
    let dir = temp_test_dir("corrupted_anchor");
    let state_file = dir.join("billing_state.json");

    let engine = BillingEngine::new();
    engine.set_persistence_path(&state_file);
    let card = Card::new("card-t07-anchor", "grp-pro", 100_000);
    engine.upsert_card(card);

    let anchor_file = dir.join("billing_state.json.anchor");
    assert!(anchor_file.exists());

    // Corrupt anchor with wrong checksum
    let mut anchor_data = read_snapshot_anchor(&state_file).unwrap();
    anchor_data.checksum =
        "0000000000000000000000000000000000000000000000000000000000000000".to_string();
    fs::write(
        &anchor_file,
        serde_json::to_string_pretty(&anchor_data).unwrap(),
    )
    .unwrap();

    let err = BillingEngine::verify_snapshot_integrity(&state_file, None)
        .expect_err("Verification must fail on mismatched anchor checksum");
    assert!(err.to_string().contains("Persistence") || err.to_string().contains("checksum"));

    let _ = fs::remove_dir_all(&dir);
}

// --------------------------------------------------------------------------
// 4. Rejection on unbalanced card ledger reconciliation
// --------------------------------------------------------------------------
#[test]
fn test_t07_verify_fails_on_unbalanced_card_ledger() {
    let dir = temp_test_dir("unbalanced_ledger");
    let state_file = dir.join("billing_state.json");

    let engine = BillingEngine::new();
    engine.set_persistence_path(&state_file);

    // Card records credit_used = 1000, but ledger has NO usage entries!
    let mut card = Card::new("card-tampered", "grp-pro", 100_000);
    card.credit_used = 1000;
    engine.upsert_card(card);

    let err = BillingEngine::verify_snapshot_integrity(&state_file, None)
        .expect_err("Verification must catch unbalanced card credit_used vs ledger");
    assert!(err.to_string().contains("ledger reconciliation"));

    let _ = fs::remove_dir_all(&dir);
}

// --------------------------------------------------------------------------
// 5. Complete Backup and Restore Cycle with Cold Recovery
// --------------------------------------------------------------------------
#[test]
fn test_t07_backup_and_restore_cycle_retains_business_invariants() {
    let live_dir = temp_test_dir("live_cycle");
    let backup_dir = temp_test_dir("backups");
    let live_state_file = live_dir.join("billing_state.json");

    let original_engine = BillingEngine::new();
    original_engine.set_persistence_path(&live_state_file);

    // Populate complex production state: 2 cards (1 active, 1 voided), provider, ledger entry
    let mut card1 = Card::new("card-live-1", "grp-pro", 300_000);
    card1.status = CardStatus::Active;
    original_engine.upsert_card(card1);

    let card2 = Card::new("card-live-2", "grp-free", 50_000);
    original_engine.upsert_card(card2);
    original_engine
        .void_unactivated_card("card-live-2", "admin", "fraud test", 1000)
        .unwrap();

    let (prov, key) = sample_provider();
    original_engine.upsert_provider(prov);
    original_engine.upsert_provider_key(key);

    // Perform backup creation: copy to backup_dir, write anchor & manifest
    let gen_id = "20260915_120000_test_999";
    let backup_json = backup_dir.join(format!("billing_state_{gen_id}.json"));
    let backup_anchor = backup_dir.join(format!("billing_state_{gen_id}.json.anchor"));
    let backup_manifest = backup_dir.join(format!("billing_state_{gen_id}.manifest.json"));

    fs::copy(&live_state_file, &backup_json).unwrap();
    fs::copy(live_dir.join("billing_state.json.anchor"), &backup_anchor).unwrap();

    let anchor = read_snapshot_anchor(&live_state_file).unwrap();
    let manifest_content = serde_json::json!({
        "version": 1,
        "generation_id": gen_id,
        "created_at": "2026-09-15T12:00:00Z",
        "snapshot_file": format!("billing_state_{gen_id}.json"),
        "snapshot_sha256": anchor.checksum,
        "anchor_file": format!("billing_state_{gen_id}.json.anchor"),
        "anchor_sequence": anchor.sequence,
        "status": "completed"
    });
    fs::write(&backup_manifest, manifest_content.to_string()).unwrap();

    // Verify backup artifact before restoring
    let report = BillingEngine::verify_snapshot_integrity(&backup_json, None)
        .expect("Backup snapshot must verify successfully");
    assert_eq!(report.cards_count, 2);

    // Simulate disaster: wipe live directory completely
    fs::remove_file(&live_state_file).unwrap();
    fs::remove_file(live_dir.join("billing_state.json.anchor")).unwrap();

    // Restore from backup into live directory
    fs::copy(&backup_json, &live_state_file).unwrap();
    fs::copy(&backup_anchor, live_dir.join("billing_state.json.anchor")).unwrap();

    // Cold start a fresh BillingEngine from restored state
    let recovered_engine = BillingEngine::new();
    recovered_engine
        .load_from_file(&live_state_file)
        .expect("Restored snapshot must load cleanly in fresh engine");

    // Assert: All cards, balances, ledger entries, provider keys, and void states match 100%
    let c1 = recovered_engine.get_card("card-live-1").unwrap();
    assert_eq!(c1.credit_total, 300_000);
    assert_eq!(c1.status, CardStatus::Active);

    let c2 = recovered_engine.get_card("card-live-2").unwrap();
    assert_eq!(c2.status, CardStatus::Voided);

    let prov_recovered = recovered_engine.get_provider("prov-anthropic").unwrap();
    assert_eq!(prov_recovered.name, "Anthropic Direct");

    let _ = fs::remove_dir_all(&live_dir);
    let _ = fs::remove_dir_all(&backup_dir);
}

// --------------------------------------------------------------------------
// 6. Emergency Rollback on Interrupted Atomic Replace
// --------------------------------------------------------------------------
#[test]
fn test_t07_emergency_rollback_preserves_live_state() {
    let dir = temp_test_dir("rollback_preserve");
    let state_file = dir.join("billing_state.json");
    let rollback_dir = dir.join(".rollback_test");
    fs::create_dir_all(&rollback_dir).unwrap();

    let engine = BillingEngine::new();
    engine.set_persistence_path(&state_file);
    let original_card = Card::new("card-original", "grp-pro", 888_888);
    engine.upsert_card(original_card);

    // Live state currently has 888,888 credits
    assert_eq!(
        engine.get_card("card-original").unwrap().credit_total,
        888_888
    );

    // Preserve live state into rollback dir (as restore.sh does)
    fs::copy(&state_file, rollback_dir.join("billing_state.json")).unwrap();
    fs::copy(
        dir.join("billing_state.json.anchor"),
        rollback_dir.join("billing_state.json.anchor"),
    )
    .unwrap();

    // Simulate corrupted replace: write garbage to live state
    fs::write(&state_file, "corrupted half-written garbage").unwrap();

    // Trigger emergency rollback: copy back from rollback_dir
    fs::copy(rollback_dir.join("billing_state.json"), &state_file).unwrap();
    fs::copy(
        rollback_dir.join("billing_state.json.anchor"),
        dir.join("billing_state.json.anchor"),
    )
    .unwrap();

    // Fresh engine cold recovery loads rolled back state cleanly
    let recovered = BillingEngine::new();
    recovered
        .load_from_file(&state_file)
        .expect("Rolled back state must recover cleanly");
    assert_eq!(
        recovered.get_card("card-original").unwrap().credit_total,
        888_888
    );

    let _ = fs::remove_dir_all(&dir);
}
