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
    let dir = std::env::temp_dir()
        .join("billing-state-growth-tests")
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
    // The template feature must not silently change ordinary invocation retention.
    engine
        .reserve("card", "settled-0", &params, 100 + DAY + 2, 600)
        .unwrap();
    engine.release("settled-0").unwrap();
    assert_eq!(engine.get_card("card").unwrap().credit_used, 50 * 60);
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

// Permanent template-only tombstones survive receipt expiry and ordinary retention.
// No ordinary invocation tombstone is added. Fixtures honor TEMP/TMP, not the C: tree.
use billing::engine::{
    ResponseTemplateReceipt, ResponseTemplateRule, ResponseTemplateUpdate, ResponseTemplateVariant,
};
use billing::{Group, ModelMap};

fn template_engine(path: &std::path::Path) -> (BillingEngine, String) {
    let engine = engine_with_card();
    engine.set_persistence_path(path);
    engine.upsert_group(Group::pro_plus("group", "Group"));
    engine.upsert_model_map(ModelMap::new(
        "mapping", "group", "model-a", "upstream", "target",
    ));
    let revision = engine
        .publish_response_templates(
            ResponseTemplateUpdate {
                expected_revision: engine.response_template_config().revision,
                reason: "index regression".into(),
                rules: vec![ResponseTemplateRule {
                    id: "landing".into(),
                    name: "Landing".into(),
                    enabled: true,
                    match_mode: "contains".into(),
                    match_text: "make a page".into(),
                    variants: vec![ResponseTemplateVariant {
                        model_id: "model-a".into(),
                        file_path: "index.html".into(),
                        content: "<html>page</html>".into(),
                        preamble: "Creating".into(),
                        completion: "Done".into(),
                        price_microcredits: 10,
                    }],
                }],
            },
            100,
        )
        .unwrap()
        .revision;
    (engine, revision)
}

fn charge_template(
    engine: &BillingEngine,
    revision: &str,
    id: &str,
    now: u64,
) -> Result<billing::LedgerEntry, BillingError> {
    engine.charge_response_template(
        revision,
        "landing",
        "model-a",
        ResponseTemplateReceipt {
            tool_use_id: format!("template_{id}"),
            invocation_id: id.into(),
            card_id: "card".into(),
            conversation_id: "conversation".into(),
            model_id: "model-a".into(),
            file_path: "index.html".into(),
            completion: "Done".into(),
            tool_name: "fsWrite".into(),
            path_key: "path".into(),
            content_key: "text".into(),
            content: "<html>page</html>".into(),
            price_microcredits: 10,
            created_at_secs: now,
        },
        now,
    )
}

#[test]
fn archive_index_tracks_template_history_not_all_usage_and_migrates_old_ids() {
    let path = state_path("bounded-archive-index");
    let (engine, revision) = template_engine(&path);
    let params = ReservationEstimateParams::new(0, 1);
    for n in 0..32 {
        let id = format!("ordinary-{n}");
        engine.reserve("card", &id, &params, 100, 600).unwrap();
        engine
            .settle(&id, &one_output_token(), "m", "p", "t", 101)
            .unwrap();
    }
    charge_template(&engine, &revision, "fixed", 102).unwrap();
    engine
        .archive_ledger(103, &engine.ledger_archive_dir().unwrap())
        .unwrap();
    let mut snapshot = engine.export_snapshot();
    assert_eq!(
        snapshot
            .archived_ledger_summary
            .usage_invocation_ids
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["fixed"]
    );
    // Simulate the earlier feature snapshot which indexed all ordinary requests.
    snapshot
        .archived_ledger_summary
        .template_invocations_complete = false;
    snapshot
        .archived_ledger_summary
        .usage_invocation_ids
        .extend((0..32).map(|n| format!("ordinary-{n}")));
    let legacy_path = path.parent().unwrap().join("legacy.json");
    std::fs::write(&legacy_path, serde_json::to_vec(&snapshot).unwrap()).unwrap();
    let restored = BillingEngine::new();
    restored.load_from_file(&legacy_path).unwrap();
    assert_eq!(
        restored
            .export_snapshot()
            .archived_ledger_summary
            .usage_invocation_ids
            .len(),
        1
    );
    restored.run_janitor(103 + 2 * DAY);
    assert!(matches!(
        restored.reserve("card", "fixed", &params, 104 + 2 * DAY, 600),
        Err(BillingError::DuplicateInvocation(_))
    ));
    assert!(matches!(
        charge_template(&restored, &revision, "fixed", 104 + 2 * DAY),
        Err(BillingError::DuplicateInvocation(_))
    ));
    // Retention for ordinary usage is unchanged, even after archival and restart.
    restored
        .reserve("card", "ordinary-0", &params, 104 + 2 * DAY, 600)
        .unwrap();
    restored.release("ordinary-0").unwrap();
    assert_eq!(restored.get_card("card").unwrap().credit_used, 32 * 60 + 10);
    assert!(
        restored
            .reconcile_card_balance("card", 1_000_000_000)
            .unwrap()
            .is_balanced
    );
}

#[test]
fn pruned_and_archived_template_replay_is_rejected_after_restart_without_recharge() {
    let path = state_path("pruned-template-replay");
    let (engine, revision) = template_engine(&path);
    charge_template(&engine, &revision, "old", 100).unwrap();
    engine
        .archive_ledger(101, &engine.ledger_archive_dir().unwrap())
        .unwrap();
    charge_template(&engine, &revision, "next", 100 + DAY).unwrap();
    let snapshot = engine.export_snapshot();
    assert_eq!(snapshot.response_template_receipts.len(), 1);
    assert_eq!(snapshot.response_template_receipts[0].invocation_id, "next");
    assert_eq!(
        snapshot.archived_ledger_summary.usage_invocation_ids.len(),
        2
    );
    assert!(snapshot
        .archived_ledger_summary
        .usage_invocation_ids
        .contains("old"));
    let restored = BillingEngine::new();
    restored.load_from_file(&path).unwrap();
    let config = restored.response_template_config();
    let mut rules = config.rules;
    rules[0].enabled = false;
    restored
        .publish_response_templates(
            ResponseTemplateUpdate {
                expected_revision: config.revision,
                reason: "disable rule".into(),
                rules,
            },
            101 + DAY,
        )
        .unwrap();
    restored.run_janitor(100 + 2 * DAY);
    assert!(matches!(
        restored.reserve(
            "card",
            "old",
            &ReservationEstimateParams::new(0, 1),
            101 + 2 * DAY,
            600
        ),
        Err(BillingError::DuplicateInvocation(_))
    ));
    // Re-enable to exercise the direct template-charge guard as well.
    let config = restored.response_template_config();
    let mut rules = config.rules;
    rules[0].enabled = true;
    let revision = restored
        .publish_response_templates(
            ResponseTemplateUpdate {
                expected_revision: config.revision,
                reason: "enable rule".into(),
                rules,
            },
            102 + 2 * DAY,
        )
        .unwrap()
        .revision;
    let before = serde_json::to_value(restored.export_snapshot()).unwrap();
    let sequence = restored.snapshot_sequence();
    assert!(matches!(
        charge_template(&restored, &revision, "old", 101 + DAY),
        Err(BillingError::DuplicateInvocation(_))
    ));
    let after = serde_json::to_value(restored.export_snapshot()).unwrap();
    for field in [
        "cards",
        "ledger",
        "response_template_receipts",
        "archived_ledger_summary",
    ] {
        assert_eq!(before[field], after[field], "failed replay changed {field}");
    }
    assert_eq!(restored.snapshot_sequence(), sequence);
    assert_eq!(restored.get_card("card").unwrap().credit_used, 20);
    assert!(
        restored
            .reconcile_card_balance("card", 1_000_000_000)
            .unwrap()
            .is_balanced
    );
}

#[test]
fn template_and_ordinary_requests_never_read_historical_archives() {
    for corrupt in [false, true] {
        let path = state_path(if corrupt {
            "corrupt-template-archive"
        } else {
            "missing-template-archive"
        });
        let (engine, revision) = template_engine(&path);
        charge_template(&engine, &revision, "old", 100).unwrap();
        let dir = engine.ledger_archive_dir().unwrap();
        let archive = engine.archive_ledger(101, &dir).unwrap();
        charge_template(&engine, &revision, "next", 100 + DAY).unwrap();
        if corrupt {
            std::fs::write(dir.join(&archive.archive_file), b"not the verified archive").unwrap();
        } else {
            std::fs::remove_file(dir.join(&archive.archive_file)).unwrap();
        }
        let sequence = engine.snapshot_sequence();
        assert!(matches!(
            charge_template(&engine, &revision, "old", 101 + DAY),
            Err(BillingError::DuplicateInvocation(_))
        ));
        assert_eq!(engine.snapshot_sequence(), sequence);
        // New templates succeed even if historical archives cannot be read.
        charge_template(&engine, &revision, "new", 101 + DAY).unwrap();
        assert_eq!(engine.get_card("card").unwrap().credit_used, 30);
        assert_eq!(engine.ledger_entries().len(), 2);
        assert_eq!(engine.export_snapshot().response_template_receipts.len(), 2);
        let params = ReservationEstimateParams::new(0, 1);
        engine
            .reserve("card", "ordinary", &params, 101 + DAY, 600)
            .unwrap();
        engine
            .settle("ordinary", &one_output_token(), "m", "p", "t", 102 + DAY)
            .unwrap();
        assert_eq!(engine.get_card("card").unwrap().credit_used, 90);
    }
}

#[test]
fn template_tombstone_capacity_refuses_new_charge_without_evicting_or_blocking_ordinary_usage() {
    use billing::engine::{MAX_TEMPLATE_INVOCATIONS, MAX_TEMPLATE_INVOCATION_BYTES};
    for byte_limit in [false, true] {
        let path = state_path(if byte_limit {
            "template-byte-cap"
        } else {
            "template-count-cap"
        });
        let (engine, revision) = template_engine(&path);
        let mut snapshot = engine.export_snapshot();
        let ids = &mut snapshot.archived_ledger_summary.usage_invocation_ids;
        if byte_limit {
            // Each ASCII ID encodes to 131 bytes including comma; leave room for
            // only 1-131 more bytes. The next 128-byte ID must exceed the budget.
            let count = (MAX_TEMPLATE_INVOCATION_BYTES - 2) / 131;
            ids.extend((0..count).map(|n| format!("{n:0128}")));
            assert!(ids.len() < MAX_TEMPLATE_INVOCATIONS);
        } else {
            ids.extend((0..MAX_TEMPLATE_INVOCATIONS).map(|n| format!("history-{n}")));
        }
        assert!(serde_json::to_vec(ids).unwrap().len() <= MAX_TEMPLATE_INVOCATION_BYTES);
        engine.import_snapshot(snapshot);
        engine.sync_to_disk_checked().unwrap();
        let before = engine.export_snapshot();
        let sequence = engine.snapshot_sequence();
        let new_id = if byte_limit {
            "n".repeat(128)
        } else {
            "new".into()
        };
        let error = charge_template(&engine, &revision, &new_id, 100).unwrap_err();
        assert!(error.to_string().contains("capacity exhausted"), "{error}");
        assert!(error.to_string().contains("administrator"), "{error}");
        assert_eq!(engine.snapshot_sequence(), sequence);
        let after = engine.export_snapshot();
        assert_eq!(
            before.archived_ledger_summary.usage_invocation_ids,
            after.archived_ledger_summary.usage_invocation_ids
        );
        assert!(after.ledger.is_empty());
        assert!(after.response_template_receipts.is_empty());
        assert_eq!(engine.get_card("card").unwrap().credit_used, 0);
        let params = ReservationEstimateParams::new(0, 1);
        let old_id = before
            .archived_ledger_summary
            .usage_invocation_ids
            .first()
            .unwrap();
        assert!(matches!(
            engine.reserve("card", old_id, &params, 101, 600),
            Err(BillingError::DuplicateInvocation(_))
        ));
        engine
            .reserve("card", "ordinary", &params, 101, 600)
            .unwrap();
        engine
            .settle("ordinary", &one_output_token(), "m", "p", "t", 102)
            .unwrap();
        assert_eq!(engine.get_card("card").unwrap().credit_used, 60);
        let restored = BillingEngine::new();
        restored.load_from_file(&path).unwrap();
        assert!(matches!(
            restored.reserve("card", old_id, &params, 103, 600),
            Err(BillingError::DuplicateInvocation(_))
        ));
    }
}

#[test]
fn legacy_pruned_template_history_is_rebuilt_once_from_verified_archives() {
    let path = state_path("legacy-pruned-template");
    let (engine, revision) = template_engine(&path);
    charge_template(&engine, &revision, "old", 100).unwrap();
    let archive = engine
        .archive_ledger(101, &engine.ledger_archive_dir().unwrap())
        .unwrap();
    charge_template(&engine, &revision, "next", 100 + DAY).unwrap();
    let mut legacy = serde_json::to_value(engine.export_snapshot()).unwrap();
    // The earlier bounded proof implementation had already forgotten this ID.
    legacy["archived_ledger_summary"]["usage_invocation_ids"] = serde_json::json!([]);
    legacy["archived_ledger_summary"]
        .as_object_mut()
        .unwrap()
        .remove("template_invocations_complete");
    // A direct, non-fallible import cannot prove forgotten archived IDs. It must
    // refuse traffic until the operator uses the verified file-loader migration.
    let unverified = BillingEngine::new();
    unverified.import_snapshot(serde_json::from_value(legacy.clone()).unwrap());
    assert!(matches!(
        unverified.reserve(
            "card",
            "old",
            &ReservationEstimateParams::new(0, 1),
            102 + 2 * DAY,
            600
        ),
        Err(BillingError::InvalidState(_))
    ));
    let legacy_path = path.parent().unwrap().join("legacy.json");
    std::fs::write(&legacy_path, serde_json::to_vec(&legacy).unwrap()).unwrap();
    let restored = BillingEngine::new();
    restored.load_from_file(&legacy_path).unwrap();
    assert!(restored
        .export_snapshot()
        .archived_ledger_summary
        .usage_invocation_ids
        .contains("old"));
    restored.sync_to_disk_checked().unwrap();
    std::fs::remove_file(
        engine
            .ledger_archive_dir()
            .unwrap()
            .join(archive.archive_file),
    )
    .unwrap();
    // After migration, neither reload nor new charges require old archive I/O.
    let again = BillingEngine::new();
    again.load_from_file(&legacy_path).unwrap();
    again.run_janitor(101 + 2 * DAY);
    assert!(matches!(
        again.reserve(
            "card",
            "old",
            &ReservationEstimateParams::new(0, 1),
            102 + 2 * DAY,
            600
        ),
        Err(BillingError::DuplicateInvocation(_))
    ));
    charge_template(&again, &revision, "new", 102 + 2 * DAY).unwrap();
    assert_eq!(again.get_card("card").unwrap().credit_used, 30);
}
