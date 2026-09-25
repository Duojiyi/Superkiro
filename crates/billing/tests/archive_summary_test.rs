//! A saved state whose archive summary does not account for its archive receipts.
//!
//! The loader refused such a state ("restore/rebuild from verified archives") and nothing
//! rebuilt the summary, so a state saved before the summary counted its entries, with the
//! ledger archived, could not be started from.

use billing::{BillingEngine, Card, CardStatus, ReservationEstimateParams, UsageTokens};

fn state_dir(name: &str) -> std::path::PathBuf {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target/archive-summary-tests")
        .join(format!(
            "{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A state with archived usage, saved in the older shape: a summary without its entry
/// count. Returns the file and the card's usage.
fn legacy_state(dir: &std::path::Path) -> (std::path::PathBuf, i64) {
    let engine = BillingEngine::new();
    engine.set_persistence_path(dir.join("current.json"));
    let mut card = Card::new("card", "group", 1_000_000);
    card.status = CardStatus::Active;
    engine.upsert_card(card);
    for (id, at) in [("old-1", 10), ("old-2", 20), ("recent", 500)] {
        engine
            .reserve("card", id, &ReservationEstimateParams::new(0, 1), at, 600)
            .unwrap();
        engine
            .settle(
                id,
                &UsageTokens {
                    output_tokens: 1,
                    ..UsageTokens::default()
                },
                "m",
                "p",
                "t",
                at + 1,
            )
            .unwrap();
    }
    engine
        .archive_ledger(100, &dir.join("ledger_archives"))
        .unwrap();
    let mut value = serde_json::to_value(engine.export_snapshot()).unwrap();
    value["archived_ledger_summary"]
        .as_object_mut()
        .unwrap()
        .remove("entries_count");
    let path = dir.join("billing_state.json");
    std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    (path, engine.get_card("card").unwrap().credit_used)
}

#[test]
fn a_summary_without_its_entry_count_is_rebuilt_from_the_archives() {
    let dir = state_dir("rebuild");
    let (path, used) = legacy_state(&dir);
    let restored = BillingEngine::new();
    restored.load_from_file(&path).unwrap();
    assert_eq!(restored.get_card("card").unwrap().credit_used, used);
    // The archived usage still counts toward the card's windows.
    assert_eq!(restored.get_usage_since("card", 0), used);
    let reconciliation = restored.reconcile_card_balance("card", 1_000_000).unwrap();
    assert!(reconciliation.is_balanced, "{reconciliation:?}");
    // What it saves next loads without rebuilding.
    restored.sync_to_disk_checked().unwrap();
    std::fs::remove_dir_all(dir.join("ledger_archives")).unwrap();
    BillingEngine::new().load_from_file(&path).unwrap();
}

#[test]
fn without_its_archives_the_state_is_still_refused() {
    let dir = state_dir("missing");
    let (path, _) = legacy_state(&dir);
    std::fs::remove_dir_all(dir.join("ledger_archives")).unwrap();
    let error = BillingEngine::new().load_from_file(&path).unwrap_err();
    assert!(error.to_string().contains("ledger_archive_"), "{error}");
}
