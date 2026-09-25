//! What one request costs the saved state: how often it is written, and what it leaves.
//!
//! Every request rewrote and synced the whole state three times under one global lock
//! (the hold, the priced intent, the debit), and left a record of its invocation id that
//! was never removed. At about a hundred thousand requests one request took seconds and
//! the state grew until the size ceiling stopped billing for everyone.

use billing::{
    BillingEngine, BillingError, Card, CardStatus, ReservationEstimateParams, UsageTokens,
};

const DAY: u64 = 24 * 60 * 60;

fn state_path(name: &str) -> std::path::PathBuf {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target/state-growth-tests")
        .join(format!(
            "{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("state.json")
}

fn engine_with_card() -> BillingEngine {
    let engine = BillingEngine::new();
    let mut card = Card::new("card", "group", 1_000_000_000);
    card.status = CardStatus::Active;
    card.max_concurrency = 1_000;
    engine.upsert_card(card);
    engine
}

fn one_output_token() -> UsageTokens {
    UsageTokens {
        output_tokens: 1,
        ..UsageTokens::default()
    }
}

/// A hold is not a money movement: a restart drops every hold without a settlement
/// anyway. The settlement is saved once, the priced intent and its debit together.
#[test]
fn a_billed_request_is_written_once_and_a_released_one_not_at_all() {
    let path = state_path("writes");
    let engine = engine_with_card();
    engine.set_persistence_path(&path);
    engine.sync_to_disk_checked().unwrap();
    let before = engine.snapshot_sequence();
    let params = ReservationEstimateParams::new(0, 1);

    engine.reserve("card", "billed", &params, 100, 600).unwrap();
    assert_eq!(engine.snapshot_sequence(), before, "a hold is written");
    engine
        .settle("billed", &one_output_token(), "m", "p", "t", 101)
        .unwrap();
    assert_eq!(engine.snapshot_sequence(), before + 1);

    engine
        .reserve("card", "released", &params, 102, 600)
        .unwrap();
    engine.release("released").unwrap();
    assert_eq!(
        engine.snapshot_sequence(),
        before + 1,
        "a release is written"
    );
    assert_eq!(engine.get_card("card").unwrap().credit_reserved, 0);

    // The settlement is durable: a restart has the debit and refuses a replay.
    let restarted = BillingEngine::new();
    restarted.load_from_file(&path).unwrap();
    assert_eq!(restarted.get_card("card").unwrap().credit_used, 60);
    assert_eq!(restarted.ledger_entries().len(), 1);
    assert!(matches!(
        restarted.reserve("card", "billed", &params, 103, 600),
        Err(BillingError::DuplicateInvocation(_))
    ));
}

/// A settled invocation id stays refused for a day, which covers any client retry, and
/// is then forgotten. The ledger entry, the balance and the usage are untouched.
#[test]
fn settled_and_released_records_are_kept_for_a_day_not_forever() {
    let engine = engine_with_card();
    let params = ReservationEstimateParams::new(0, 1);
    for n in 0..50 {
        engine
            .reserve("card", &format!("settled-{n}"), &params, 100, 600)
            .unwrap();
        engine
            .settle(
                &format!("settled-{n}"),
                &one_output_token(),
                "m",
                "p",
                "t",
                101,
            )
            .unwrap();
        engine
            .reserve("card", &format!("released-{n}"), &params, 100, 600)
            .unwrap();
        engine.release(&format!("released-{n}")).unwrap();
    }

    engine.run_janitor(100 + DAY - 1);
    assert_eq!(engine.export_snapshot().reservations.len(), 100);
    assert!(matches!(
        engine.reserve("card", "settled-0", &params, 100 + DAY - 1, 600),
        Err(BillingError::DuplicateInvocation(_))
    ));

    engine.run_janitor(100 + DAY + 1);
    assert!(engine.export_snapshot().reservations.is_empty());
    assert_eq!(engine.ledger_entries().len(), 50);
    assert_eq!(engine.get_card("card").unwrap().credit_used, 50 * 60);
    assert_eq!(engine.get_card("card").unwrap().credit_reserved, 0);
}

/// Loading a snapshot recreates the record of a settled invocation from its ledger entry
/// only while that invocation is inside the day: a record older than that is pruned,
/// and recreating it on every restart would grow the state back.
#[test]
fn a_restart_does_not_recreate_records_past_the_window() {
    let engine = engine_with_card();
    let params = ReservationEstimateParams::new(0, 1);
    for (id, at) in [("old", 100), ("recent", 100 + 2 * DAY)] {
        engine.reserve("card", id, &params, at, 600).unwrap();
        engine
            .settle(id, &one_output_token(), "m", "p", "t", at + 1)
            .unwrap();
    }
    let mut legacy = engine.export_snapshot();
    legacy.reservations.clear();
    let restarted = BillingEngine::new();
    restarted.import_snapshot(legacy);
    let reservations = restarted.export_snapshot().reservations;
    assert_eq!(
        reservations.keys().collect::<Vec<_>>(),
        ["recent"],
        "{reservations:?}"
    );
}
