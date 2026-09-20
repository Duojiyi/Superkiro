use billing::*;
use std::{fs, path::PathBuf};

fn path(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target/cloud-audit-tests")
        .join(format!(
            "{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
    fs::create_dir_all(&dir).unwrap();
    dir.join("state.json")
}
fn engine() -> BillingEngine {
    let b = BillingEngine::new();
    let mut c = Card::new("card", "group", 1_000_000);
    c.status = CardStatus::Active;
    c.code_hash = billing::card::hash_card_code("raw-secret");
    b.upsert_card(c);
    b
}

#[test]
fn refresh_consumption_is_durable_atomic_and_prunes_only_expired_entries() {
    let b = engine();
    let file = path("refresh");
    b.set_persistence_path(&file);
    b.consume_refresh_token("first", 200, 100).unwrap();
    b.inject_persistence_fault(true);
    assert!(b.consume_refresh_token("retry", 300, 100).is_err());
    assert!(!b
        .export_snapshot()
        .consumed_refresh_tokens
        .contains_key("retry"));
    b.inject_persistence_fault(false);
    b.consume_refresh_token("retry", 300, 100).unwrap();
    let recovered = BillingEngine::new();
    recovered.load_from_file(&file).unwrap();
    recovered.set_persistence_path(&file);
    assert!(recovered.consume_refresh_token("first", 200, 101).is_err());
    assert!(recovered.consume_refresh_token("retry", 300, 101).is_err());
    recovered.consume_refresh_token("next", 400, 200).unwrap();
    assert!(!recovered
        .export_snapshot()
        .consumed_refresh_tokens
        .contains_key("first"));
    assert!(recovered
        .consume_refresh_token("expired", 200, 200)
        .is_err());
    BillingEngine::verify_snapshot_integrity(&file, None).unwrap();
}

#[test]
fn refresh_concurrent_consumers_have_one_winner() {
    let b = engine();
    let gate = std::sync::Arc::new(std::sync::Barrier::new(8));
    let workers: Vec<_> = (0..8)
        .map(|_| {
            let b = b.clone();
            let gate = gate.clone();
            std::thread::spawn(move || {
                gate.wait();
                b.consume_refresh_token("one", 200, 100).is_ok()
            })
        })
        .collect();
    assert_eq!(
        workers
            .into_iter()
            .map(|w| usize::from(w.join().unwrap()))
            .sum::<usize>(),
        1
    );
}

#[test]
fn public_secret_lookup_never_accepts_hash_or_id() {
    let b = engine();
    let hash = b.get_card("card").unwrap().code_hash;
    assert!(b.find_card_by_secret("raw-secret").is_some());
    assert!(b.find_card_by_secret(&hash).is_none());
    assert!(b.find_card_by_secret("card").is_none());
    assert!(b.find_card_by_code_or_id(&hash).is_some());
}

#[test]
fn request_owned_reservation_survives_long_backpressure_and_settles_once() {
    let b = engine();
    let file = path("lease");
    b.set_persistence_path(&file);
    let lease = b.protect_reservation("long-stream");
    b.reserve(
        "card",
        "long-stream",
        &ReservationEstimateParams::new(0, 2),
        100,
        660,
    )
    .unwrap();
    let janitor = b.clone();
    std::thread::spawn(move || {
        for now in [761, 1000, 10_000] {
            assert_eq!(janitor.run_janitor(now), 0);
        }
    })
    .join()
    .unwrap();
    let usage = UsageTokens {
        output_tokens: 1,
        ..UsageTokens::default()
    };
    b.settle("long-stream", &usage, "m", "p", "m", 10_001)
        .unwrap();
    drop(lease);
    assert!(b
        .settle("long-stream", &usage, "m", "p", "m", 10_002)
        .is_err());
    let recovered = BillingEngine::new();
    recovered.load_from_file(&file).unwrap();
    assert_eq!(recovered.get_card("card").unwrap().credit_used, 60);
    assert_eq!(recovered.get_card("card").unwrap().credit_reserved, 0);
    assert_eq!(recovered.export_snapshot().ledger.len(), 1);
}

#[test]
fn abandoned_lease_is_reclaimed_and_live_ownership_is_not_restored() {
    let b = engine();
    let file = path("orphan");
    b.set_persistence_path(&file);
    let lease = b.protect_reservation("orphan");
    b.reserve(
        "card",
        "orphan",
        &ReservationEstimateParams::new(0, 1),
        100,
        660,
    )
    .unwrap();
    let recovered = BillingEngine::new();
    recovered.load_from_file(&file).unwrap();
    assert_eq!(recovered.get_card("card").unwrap().credit_reserved, 0);
    drop(lease);
    assert_eq!(b.run_janitor(761), 1);
    assert_eq!(b.get_card("card").unwrap().credit_reserved, 0);
}

#[test]
fn adjustment_replay_survives_restart_and_rejects_changed_intent() {
    let b = engine();
    let file = path("adjust");
    b.set_persistence_path(&file);
    let first = b
        .adjust_balance_idempotent("card", 10, "admin", "bonus", 100, Some("intent"))
        .unwrap();
    let recovered = BillingEngine::new();
    recovered.load_from_file(&file).unwrap();
    recovered.set_persistence_path(&file);
    let replay = recovered
        .adjust_balance_idempotent("card", 10, "admin", "bonus", 200, Some("intent"))
        .unwrap();
    assert_eq!(first.id, replay.id);
    assert!(recovered
        .adjust_balance_idempotent("card", 10, "admin", "different", 200, Some("intent"))
        .is_err());
    assert_eq!(recovered.get_card("card").unwrap().credit_total, 1_000_010);
    assert_eq!(recovered.export_snapshot().ledger.len(), 1);
}

#[test]
fn legacy_snapshot_without_refresh_field_upgrades_and_verifies() {
    let b = engine();
    let file = path("legacy-refresh");
    let mut legacy = serde_json::to_value(b.export_snapshot()).unwrap();
    legacy
        .as_object_mut()
        .unwrap()
        .remove("consumed_refresh_tokens");
    fs::write(&file, serde_json::to_vec(&legacy).unwrap()).unwrap();
    let recovered = BillingEngine::new();
    recovered.load_from_file(&file).unwrap();
    assert!(recovered
        .export_snapshot()
        .consumed_refresh_tokens
        .is_empty());
    BillingEngine::verify_snapshot_integrity(&file, None).unwrap();
    recovered
        .consume_refresh_token("post-upgrade", 200, 100)
        .unwrap();
    BillingEngine::verify_snapshot_integrity(&file, None).unwrap();
    let restarted = BillingEngine::new();
    restarted.load_from_file(&file).unwrap();
    assert!(restarted
        .consume_refresh_token("post-upgrade", 200, 101)
        .is_err());
}

#[test]
fn recovery_verifier_rejects_invalid_refresh_state() {
    let file = path("invalid-refresh");
    let mut snapshot = engine().export_snapshot();
    snapshot.consumed_refresh_tokens.insert(String::new(), 100);
    fs::write(&file, serde_json::to_vec(&snapshot).unwrap()).unwrap();
    assert!(BillingEngine::new().load_from_file(&file).is_err());
    assert!(BillingEngine::verify_snapshot_integrity(&file, None).is_err());
}

#[test]
fn financial_settings_publish_is_atomic_revisioned_and_restart_safe() {
    let b = engine();
    let file = path("financial-settings");
    b.set_persistence_path(&file);
    let make = |revision: String, rate: f64| {
        serde_json::from_value::<billing::engine::CommercialUpdate>(serde_json::json!({
            "expected_revision":revision,"reason":"reviewed cost estimate settings",
            "settings":{"credit_face_value_cny":0.02,"usd_cny_rate":rate}
        }))
        .unwrap()
    };
    let before = b.commercial_config().revision;
    assert!(b
        .publish_commercial_config(make(before.clone(), 0.0), 100)
        .is_err());
    b.inject_persistence_fault(true);
    assert!(b
        .publish_commercial_config(make(before.clone(), 7.0), 100)
        .is_err());
    assert_eq!(b.commercial_config().revision, before);
    b.inject_persistence_fault(false);
    // Disabling injection does not clear the latched persistence failure.
    // Recover through a real successful write before accepting new reservations.
    assert!(!b.persistence_ready());
    b.sync_to_disk_checked().unwrap();
    assert!(b.persistence_ready());
    assert_eq!(b.commercial_config().revision, before);
    b.reserve(
        "card",
        "inflight",
        &ReservationEstimateParams::new(0, 1),
        100,
        660,
    )
    .unwrap();
    assert!(b
        .publish_commercial_config(make(before.clone(), 7.0), 100)
        .is_err());
    b.release("inflight").unwrap();
    let result = b
        .publish_commercial_config(make(before.clone(), 7.0), 101)
        .unwrap();
    assert_ne!(result.revision, before);
    assert_eq!(result.settings.usd_cny_rate, 7.0);
    assert_eq!(result.settings.rate_updated_at_secs, 101);
    assert!(b.publish_commercial_config(make(before, 8.0), 102).is_err());
    let recovered = BillingEngine::new();
    recovered.load_from_file(&file).unwrap();
    assert_eq!(recovered.commercial_config().revision, result.revision);
    assert_eq!(recovered.get_settings(), result.settings);
    assert_eq!(recovered.commercial_config().audit.len(), 1);
}
