use billing::*;

fn engine() -> BillingEngine {
    let engine = BillingEngine::new();
    let mut card = Card::new("card", "group", 1_000_000);
    card.status = CardStatus::Active;
    card.max_concurrency = 32;
    engine.upsert_card(card);
    engine
}

#[test]
fn settled_invocation_stays_blocked_after_janitor_archive_and_restart() {
    let engine = engine();
    let params = ReservationEstimateParams::new(0, 1);
    engine.reserve("card", "use", &params, 1, 10).unwrap();
    engine
        .settle(
            "use",
            &UsageTokens {
                output_tokens: 1,
                ..Default::default()
            },
            "m",
            "p",
            "t",
            2,
        )
        .unwrap();
    engine.run_janitor(700_000);
    assert!(matches!(
        engine.reserve("card", "use", &params, 700_000, 10),
        Err(BillingError::DuplicateInvocation(_))
    ));
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target/independent-audit")
        .join(format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
    // Simulate a legacy snapshot whose settled reservation was already pruned.
    let mut legacy = engine.export_snapshot();
    legacy.reservations.clear();
    engine.import_snapshot(legacy);
    engine.archive_ledger(3, &dir).unwrap();
    let path = dir.join("state.json");
    engine.save_to_file(&path).unwrap();
    let recovered = BillingEngine::new();
    recovered.load_from_file(&path).unwrap();
    assert!(matches!(
        recovered.reserve("card", "use", &params, 700_001, 10),
        Err(BillingError::DuplicateInvocation(_))
    ));
    assert_eq!(recovered.get_card("card").unwrap().credit_used, 60);
}

#[test]
fn legacy_pruned_reservation_is_still_blocked_by_usage_ledger() {
    let engine = engine();
    let params = ReservationEstimateParams::new(0, 1);
    engine.reserve("card", "use", &params, 1, 10).unwrap();
    engine
        .settle(
            "use",
            &UsageTokens {
                output_tokens: 1,
                ..Default::default()
            },
            "m",
            "p",
            "t",
            2,
        )
        .unwrap();
    let mut snapshot = engine.export_snapshot();
    snapshot.reservations.clear();
    engine.import_snapshot(snapshot);
    assert!(matches!(
        engine.reserve("card", "use", &params, 700_000, 10),
        Err(BillingError::DuplicateInvocation(_))
    ));
}

#[test]
fn concurrent_budget_reservations_have_one_winner() {
    for monthly in [false, true] {
        let engine = engine();
        engine
            .update_card_quotas(
                "card",
                None,
                Some((!monthly).then_some(100)),
                Some(monthly.then_some(100)),
            )
            .unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(16));
        let threads: Vec<_> = (0..16)
            .map(|i| {
                let engine = engine.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    engine
                        .reserve(
                            "card",
                            &format!("use-{i}"),
                            &ReservationEstimateParams::new(0, 1),
                            100,
                            660,
                        )
                        .is_ok()
                })
            })
            .collect();
        let winners = threads
            .into_iter()
            .filter_map(|t| t.join().unwrap().then_some(()))
            .count();
        assert_eq!(winners, 1);
        assert_eq!(engine.get_card("card").unwrap().credit_reserved, 60);
    }
}

#[test]
fn revocation_blocks_new_work_but_preserves_consumed_settlement() {
    let engine = engine();
    let params = ReservationEstimateParams::new(0, 1);
    engine
        .reserve("card", "inflight", &params, 100, 660)
        .unwrap();
    let version = engine.get_card("card").unwrap().token_version;
    engine.ban_card("card", "audit").unwrap();
    assert!(engine.reserve("card", "new", &params, 101, 660).is_err());
    let entry = engine
        .settle(
            "inflight",
            &UsageTokens {
                output_tokens: 1,
                ..Default::default()
            },
            "m",
            "p",
            "t",
            102,
        )
        .unwrap();
    assert_eq!(entry.credits_charged, 60);
    let card = engine.get_card("card").unwrap();
    assert_eq!(card.status, CardStatus::Banned);
    assert!(card.token_version > version);
    assert_eq!(card.credit_reserved, 0);
    assert_eq!(card.credit_used, 60);
}
