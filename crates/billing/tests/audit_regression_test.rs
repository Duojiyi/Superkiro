use billing::*;
use std::{fs, path::PathBuf};

fn state_path(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target/audit-tests")
        .join(format!(
            "{}-{}-{}",
            name,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
    fs::create_dir_all(&dir).unwrap();
    dir.join("state.json")
}

fn engine_with_card(total: i64) -> BillingEngine {
    let engine = BillingEngine::new();
    let mut card = Card::new("card", "group", total);
    card.status = CardStatus::Active;
    engine.upsert_card(card);
    engine
}

fn usage(output_tokens: u64) -> UsageTokens {
    UsageTokens {
        output_tokens,
        ..UsageTokens::default()
    }
}

#[test]
fn template_duration_is_authoritative_in_all_activation_paths() {
    for duration in [86_400, 604_800, 0] {
        let template = CardTemplate::new("template", "test", duration, 1_000_000, "group");
        let expected = (duration != 0).then_some(100 + duration);
        let mut card = Card::from_template("direct", "hash", &template, None, 0);
        card.activate(100, 1).unwrap();
        assert_eq!(card.valid_until, expected);
        let engine = BillingEngine::new();
        let card = Card::from_template("engine", "hash", &template, None, 0);
        engine.upsert_card(card);
        engine.activate_card("engine", 100, 1).unwrap();
        assert_eq!(engine.get_card("engine").unwrap().valid_until, expected);
        let card = Card::from_template("device", "hash", &template, None, 0);
        engine.upsert_card(card);
        engine
            .activate_card_with_device("device", 100, 1, Some("device-fp"))
            .unwrap();
        assert_eq!(engine.get_card("device").unwrap().valid_until, expected);
    }
    let mut legacy = Card::new("legacy", "group", 100);
    legacy.activate(100, 500).unwrap();
    assert_eq!(legacy.valid_until, Some(600));
}

#[test]
fn held_quota_and_extension_are_atomic_across_day_boundary() {
    for monthly in [false, true] {
        let engine = engine_with_card(1_000_000);
        engine
            .update_card_quotas(
                "card",
                None,
                Some((!monthly).then_some(100)),
                Some(monthly.then_some(100)),
            )
            .unwrap();
        let params = ReservationEstimateParams::new(0, 1); // 60 microcredits
        engine.reserve("card", "a", &params, 86_399, 660).unwrap();
        let error = engine
            .reserve("card", "b", &params, 86_401, 660)
            .unwrap_err();
        assert!(matches!(
            error,
            BillingError::DailyLimitExceeded { .. } | BillingError::MonthlyLimitExceeded { .. }
        ));
        assert!(engine.extend_reservation("a", 101, 86_401).is_err());
        assert_eq!(engine.get_card("card").unwrap().credit_reserved, 60);
        engine.extend_reservation("a", 100, 86_401).unwrap();
        // A post-consumption bill can exceed authorization, but cannot be dropped.
        assert_eq!(
            engine
                .settle("a", &usage(2), "m", "p", "t", 86_402)
                .unwrap()
                .credits_charged,
            120
        );
        assert!(engine.reserve("card", "c", &params, 86_403, 660).is_err());
    }
}

#[test]
fn full_consumption_debt_survives_restart_and_topup_pays_it() {
    let path = state_path("debt");
    let engine = engine_with_card(100);
    engine.set_persistence_path(&path);
    engine
        .reserve(
            "card",
            "use",
            &ReservationEstimateParams::new(0, 1),
            100,
            660,
        )
        .unwrap();
    let entry = engine.settle("use", &usage(3), "m", "p", "t", 101).unwrap();
    assert_eq!(entry.credits_charged, 180);
    assert_eq!(engine.get_card("card").unwrap().outstanding_debt(), 80);
    assert_eq!(
        engine.list_unpaid_charges(Some("card"))[0].amount_micro_credits,
        80
    );
    assert!(engine.list_pending_settlements().is_empty());
    assert_eq!(engine.run_janitor(1000), 0);
    assert_eq!(
        engine.export_snapshot().reservations["use"].state,
        ReservationState::Settled
    );
    BillingEngine::verify_snapshot_integrity(&path, None).unwrap();
    let recovered = BillingEngine::new();
    recovered.load_from_file(&path).unwrap();
    assert!(recovered
        .reserve(
            "card",
            "blocked",
            &ReservationEstimateParams::new(0, 0),
            102,
            660
        )
        .is_err());
    let topup = generate_topup_code(50, 0, 102).unwrap();
    recovered.upsert_topup_code(topup.topup);
    recovered
        .redeem_topup("card", &topup.raw_code, 102, "test")
        .unwrap();
    assert_eq!(recovered.get_card("card").unwrap().outstanding_debt(), 30);
    assert!(
        recovered
            .reconcile_card_balance("card", 100)
            .unwrap()
            .is_balanced
    );
    assert!(recovered
        .settle("use", &usage(3), "m", "p", "t", 103)
        .is_err());
    assert_eq!(recovered.list_unpaid_charges(None).len(), 1);
}

#[test]
fn failed_intent_write_retains_usage_until_storage_recovers() {
    let path = state_path("pending");
    let engine = engine_with_card(100);
    engine.set_persistence_path(&path);
    engine
        .reserve("card", "use", &ReservationEstimateParams::new(0, 1), 100, 1)
        .unwrap();
    engine.inject_persistence_fault(true);
    assert!(engine.settle("use", &usage(3), "m", "p", "t", 101).is_err());
    assert_eq!(engine.list_pending_settlements().len(), 1);
    assert!(engine.release("use").is_err());
    assert_eq!(engine.run_janitor(1000), 0);
    assert!(!engine.persistence_ready());
    engine.inject_persistence_fault(false);
    engine.sync_to_disk_checked().unwrap();
    let recovered = BillingEngine::new();
    recovered.load_from_file(&path).unwrap();
    assert_eq!(recovered.get_card("card").unwrap().credit_reserved, 60);
    assert_eq!(recovered.run_janitor(1000), 0);
    assert!(recovered
        .reserve(
            "card",
            "blocked",
            &ReservationEstimateParams::new(0, 0),
            1000,
            660
        )
        .is_err());
    assert!(recovered
        .settle("use", &usage(4), "m", "p", "t", 1000)
        .is_err());
    recovered.set_model_rates(
        "m",
        PricingRates {
            output_rate_per_m: 1,
            ..PricingRates::default()
        },
    );
    assert_eq!(
        recovered
            .retry_pending_settlement("use")
            .unwrap()
            .credits_charged,
        180
    );
    assert_eq!(recovered.get_card("card").unwrap().outstanding_debt(), 80);
    assert_eq!(
        recovered
            .retry_pending_settlement("use")
            .unwrap()
            .credits_charged,
        180
    );
    assert_eq!(recovered.ledger_entries().len(), 1);
    assert_eq!(recovered.get_card("card").unwrap().outstanding_debt(), 80);
}

#[test]
fn issuance_replays_across_managers_and_encrypted_restart() {
    let path = state_path("issuance");
    let engine = BillingEngine::new();
    let kek = MasterKek::generate_random().unwrap();
    engine.set_master_kek(kek.clone());
    engine.set_persistence_path(&path);
    let template = CardTemplate::daily("template", "group");
    let mut request = PullCardsRequest {
        order_id: "order".into(),
        template_id: Some("template".into()),
        group_id: Some("group".into()),
        count: 2,
        note: None,
    };
    let first = CardPlatformManager::new()
        .pull_cards(&engine, &template, &request, 100)
        .unwrap();
    assert_eq!(
        CardPlatformManager::new()
            .pull_cards(&engine, &template, &request, 900_000)
            .unwrap(),
        first
    );
    assert!(!fs::read_to_string(&path)
        .unwrap()
        .contains(&first.cards[0].raw_code));
    let recovered = BillingEngine::new();
    recovered.set_master_kek(kek);
    recovered.load_from_file(&path).unwrap();
    assert_eq!(
        CardPlatformManager::new()
            .pull_cards(&recovered, &template, &request, 900_000)
            .unwrap(),
        first
    );
    request.count = 3;
    assert!(CardPlatformManager::new()
        .pull_cards(&recovered, &template, &request, 900_001)
        .is_err());
    assert_eq!(recovered.list_all_cards().len(), 2);
    let unencrypted = BillingEngine::new();
    unencrypted.set_persistence_path(state_path("unencrypted-issuance"));
    assert!(CardPlatformManager::new()
        .pull_cards(&unencrypted, &template, &request, 100)
        .is_err());
    assert!(unencrypted.list_all_cards().is_empty());
}

#[test]
fn archived_facts_preserve_quota_reconciliation_replay_and_recovery() {
    let path = state_path("archive");
    let engine = engine_with_card(1_000_000);
    engine.set_persistence_path(&path);
    engine
        .update_card_quotas("card", None, Some(Some(100)), None)
        .unwrap();
    let adjustment = engine
        .adjust_balance_idempotent("card", 20, "op", "bonus", 100, Some("adjust"))
        .unwrap();
    engine
        .adjust_balance("card", -5, "op", "correction", 101)
        .unwrap();
    let topup = generate_topup_code(10, 0, 101).unwrap();
    engine.upsert_topup_code(topup.topup);
    engine
        .redeem_topup("card", &topup.raw_code, 101, "op")
        .unwrap();
    engine
        .reserve(
            "card",
            "use",
            &ReservationEstimateParams::new(0, 1),
            102,
            660,
        )
        .unwrap();
    engine.settle("use", &usage(1), "m", "p", "t", 103).unwrap();
    let margin = engine.get_margin_summary().total_credits_charged;
    let first = engine.archive_ledger(104, path.parent().unwrap()).unwrap();
    assert_eq!(engine.get_daily_usage("card", 105), 60);
    assert!(engine
        .reserve(
            "card",
            "blocked",
            &ReservationEstimateParams::new(0, 1),
            105,
            660
        )
        .is_err());
    assert_eq!(
        engine
            .adjust_balance_idempotent("card", 20, "op", "bonus", 105, Some("adjust"))
            .unwrap()
            .id,
        adjustment.id
    );
    assert!(engine
        .adjust_balance_idempotent("card", 21, "op", "bonus", 105, Some("adjust"))
        .is_err());
    assert!(
        engine
            .reconcile_card_balance("card", 1_000_000)
            .unwrap()
            .is_balanced
    );
    assert_eq!(engine.get_margin_summary().total_credits_charged, margin);
    engine
        .adjust_balance("card", 1, "op", "extra", 105)
        .unwrap();
    let second = engine.archive_ledger(106, path.parent().unwrap()).unwrap();
    assert_ne!(first.archive_file, second.archive_file);
    verify_ledger_archive(
        &path.parent().unwrap().join(&first.archive_file),
        &first.sha256_checksum,
    )
    .unwrap();
    BillingEngine::verify_snapshot_integrity(&path, None).unwrap();
    let recovered = BillingEngine::new();
    recovered.load_from_file(&path).unwrap();
    assert_eq!(recovered.get_daily_usage("card", 106), 60);
    assert_eq!(
        recovered
            .adjust_balance_idempotent("card", 20, "op", "bonus", 106, Some("adjust"))
            .unwrap()
            .id,
        adjustment.id
    );
    assert!(
        recovered
            .reconcile_card_balance("card", 1_000_000)
            .unwrap()
            .is_balanced
    );
    let mut corrupt = recovered.export_snapshot();
    corrupt.archived_ledger_summary = ArchivedLedgerSummary::default();
    let invalid = state_path("missing-summary");
    fs::write(&invalid, serde_json::to_vec(&corrupt).unwrap()).unwrap();
    assert!(BillingEngine::new().load_from_file(&invalid).is_err());
}

/// A settlement that prices to zero must complete. It used to be refused after its
/// intent was already durable, and the refusal was permanent: recovery retried it
/// forever without repricing, the card could never reserve again, it could not be
/// voided, and publishing any pricing change was refused for the whole deployment
/// ("Requests are still settling"). The state validated on load, so a restart kept it.
///
/// All-zero usage is what an OpenAI-compatible upstream reports when it sends a usage
/// frame of zeros; zero-priced configuration reaches the same place with real usage.
#[test]
fn a_zero_charge_settlement_completes_instead_of_wedging_the_card() {
    let engine = engine_with_card(1_000_000_000);
    engine
        .reserve(
            "card",
            "zero-charge",
            &ReservationEstimateParams::new(1_000, 1_000),
            100,
            600,
        )
        .unwrap();
    assert!(engine.get_card("card").unwrap().credit_reserved > 0);

    let entry = engine
        .settle("zero-charge", &UsageTokens::default(), "m", "p", "m", 101)
        .expect("a zero charge must settle");
    assert_eq!(entry.credits_charged, 0);

    assert!(
        engine.list_pending_settlements().is_empty(),
        "no intent may be left behind"
    );
    let card = engine.get_card("card").unwrap();
    assert_eq!(card.credit_reserved, 0, "the hold must be returned");
    assert_eq!(card.credit_used, 0);
    // The card keeps working.
    engine
        .reserve(
            "card",
            "after-zero-charge",
            &ReservationEstimateParams::new(1_000, 1_000),
            102,
            600,
        )
        .expect("the card must be able to reserve again");
}

/// Token counts come from the upstream. A buggy OpenAI-compatible relay reporting an
/// absurd count used to price to a saturated `i64::MAX` charge, which could then never
/// be added to a card that had any usage: the same permanent wedge as a zero charge,
/// reached through "settlement credit_used overflow".
#[test]
fn an_absurd_upstream_token_count_settles_as_a_bounded_charge() {
    let engine = engine_with_card(1_000_000_000);
    engine
        .reserve(
            "card",
            "normal",
            &ReservationEstimateParams::new(1_000, 1_000),
            100,
            600,
        )
        .unwrap();
    engine
        .settle("normal", &usage(1_000), "m", "p", "m", 101)
        .unwrap();
    let used_before = engine.get_card("card").unwrap().credit_used;
    assert!(used_before > 0);

    engine
        .reserve(
            "card",
            "absurd",
            &ReservationEstimateParams::new(1_000, 1_000),
            102,
            600,
        )
        .unwrap();
    let absurd = UsageTokens {
        uncached_input_tokens: u64::MAX,
        output_tokens: u64::MAX,
        cache_creation_tokens: u64::MAX,
        cache_read_tokens: u64::MAX,
    };
    let entry = engine
        .settle("absurd", &absurd, "m", "p", "m", 103)
        .expect("an absurd count must settle, not wedge");
    assert!(entry.credits_charged > 0 && entry.credits_charged < i64::MAX / 1_000);
    assert!(engine.list_pending_settlements().is_empty());
    let card = engine.get_card("card").unwrap();
    assert_eq!(card.credit_used, used_before + entry.credits_charged);
    assert_eq!(card.credit_reserved, 0);
}

/// One failed write marks persistence unready, and every reservation is refused until
/// something commits successfully. Nothing did: reservations are the refused path, and
/// the janitor commits only when it reclaims a hold. So a storage hiccup of a second
/// turned into an outage lasting until an unrelated request or timeout came along. The
/// recovery task, which already runs on a timer, now probes.
#[test]
fn the_recovery_task_restores_service_once_storage_is_back() {
    let path = state_path("persistence-probe");
    let engine = engine_with_card(1_000_000_000);
    engine.save_to_file(&path).unwrap();
    let params = ReservationEstimateParams::new(1_000, 1_000);

    engine.inject_persistence_fault(true);
    assert!(engine
        .reserve("card", "during-outage", &params, 100, 600)
        .is_err());
    assert!(!engine.persistence_ready());

    // Storage comes back. Nothing else happens on this deployment.
    engine.inject_persistence_fault(false);
    let mut recovery = engine::PendingSettlementRecovery::default();
    recovery.tick(&engine, 101);

    assert!(
        engine.persistence_ready(),
        "a single tick must restore readiness"
    );
    engine
        .reserve("card", "after-outage", &params, 102, 600)
        .expect("reservations must resume once storage is back");
}
