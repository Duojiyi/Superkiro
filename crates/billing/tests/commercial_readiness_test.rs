//! Isolated A01/A03/A04 regressions. No upstream or production state.
use billing::{
    card::{Card, CardStatus},
    engine::BillingEngine,
    ledger::UsageTokens,
    rate_card::{Currency, PricingMode, RateCardVersion},
    reservation::ReservationEstimateParams,
};

#[test]
fn freeze_matrix_preserves_origin_and_rejects_terminal_states() {
    for status in [
        CardStatus::Unactivated,
        CardStatus::Active,
        CardStatus::Expired,
        CardStatus::Frozen,
        CardStatus::Banned,
        CardStatus::Voided,
    ] {
        let engine = BillingEngine::new();
        let mut card = Card::new("card", "group", 1_000_000);
        card.status = status;
        card.activation_duration_secs = Some(100);
        engine.upsert_card(card.clone());
        let result = engine
            .batch_freeze(&["card".into()], "audit")
            .pop()
            .unwrap();
        if matches!(
            status,
            CardStatus::Frozen | CardStatus::Banned | CardStatus::Voided
        ) {
            assert!(result.is_err());
            assert_eq!(engine.get_card("card").unwrap(), card);
        } else {
            result.unwrap();
            let restored = engine
                .batch_unfreeze(&["card".into()])
                .pop()
                .unwrap()
                .unwrap();
            assert_eq!(restored.status, status);
            assert_eq!(restored.activated_at, card.activated_at);
            assert_eq!(restored.valid_until, card.valid_until);
            assert!(engine.unfreeze_card("card").is_err());
            if status == CardStatus::Unactivated {
                engine.activate_card("card", 1000, 999).unwrap();
                let activated = engine.get_card("card").unwrap();
                assert_eq!(activated.activated_at, Some(1000));
                assert_eq!(activated.valid_until, Some(1100));
            }
        }
    }
}

#[test]
fn legacy_frozen_card_does_not_skip_first_activation() {
    let engine = BillingEngine::new();
    let mut card = Card::new("card", "group", 1000);
    card.status = CardStatus::Frozen;
    engine.upsert_card(card);
    assert_eq!(
        engine.unfreeze_card("card").unwrap().status,
        CardStatus::Unactivated
    );
}

#[test]
fn concurrent_ban_and_freeze_never_resurrect_card() {
    for _ in 0..20 {
        let engine = BillingEngine::new();
        engine.upsert_card(Card::new("card", "group", 1000));
        let other = engine.clone();
        let worker = std::thread::spawn(move || {
            let _ = other.freeze_card("card", "audit");
            let _ = other.unfreeze_card("card");
        });
        engine.ban_card("card", "terminal").unwrap();
        worker.join().unwrap();
        assert_eq!(engine.get_card("card").unwrap().status, CardStatus::Banned);
    }
}

fn rate(id: &str, model: &str, price: f64) -> RateCardVersion {
    RateCardVersion {
        id: id.into(),
        rate_card_id: "default".into(),
        model: model.into(),
        currency: Currency::Cny,
        pricing_mode: PricingMode::Fixed,
        input_price_per_m: price,
        output_price_per_m: price,
        cache_creation_price_per_m: price,
        cache_read_price_per_m: price,
        fixed_input_credit_per_m: 1_000_000,
        fixed_output_credit_per_m: 1_000_000,
        fixed_cache_creation_credit_per_m: 1_000_000,
        fixed_cache_read_credit_per_m: 1_000_000,
        per_call_credit: 0,
        margin_multiplier: 1.0,
        effective_from_secs: 0,
    }
}

#[test]
fn fallback_cost_uses_actual_target_and_provider_but_charge_stays_locked() {
    for (qualified, expected, source) in [(false, 10000, "b"), (true, 20000, "pb")] {
        let engine = BillingEngine::new();
        let mut card = Card::new("card", "group", 1_000_000_000);
        card.activate(1, 0).unwrap();
        engine.upsert_card(card);
        engine.upsert_rate_card_version(rate("a", "A", 1.0));
        engine.upsert_rate_card_version(rate("wild", "*", 99.0));
        engine.upsert_rate_card_version(rate("b", "B", 10.0));
        if qualified {
            engine.upsert_rate_card_version(rate("pb", "provider/B", 20.0));
        }
        engine
            .reserve(
                "card",
                "use",
                &ReservationEstimateParams::new(1000, 0).with_model("A"),
                10,
                100,
            )
            .unwrap();
        let mut future = rate("future", "B", 500.0);
        future.effective_from_secs = 11;
        engine.upsert_rate_card_version(future);
        let entry = engine
            .settle(
                "use",
                &UsageTokens {
                    uncached_input_tokens: 1000,
                    ..Default::default()
                },
                "A",
                "provider",
                "B",
                20,
            )
            .unwrap();
        assert_eq!(entry.credits_charged, 1000);
        assert_eq!(entry.provider_cost_micro_cny, expected);
        assert_eq!(entry.rate_card_version.as_deref(), Some("a"));
        assert_eq!(
            entry.reason,
            Some(format!("provider_cost:rate_card_version={source}"))
        );
    }
}

#[test]
fn janitor_prunes_terminal_memory_but_preserves_live_holds() {
    let engine = BillingEngine::new();
    let mut card = Card::new("card", "group", 1_000_000);
    card.activate(1, 0).unwrap();
    engine.upsert_card(card);
    for id in ["released", "settled", "live"] {
        engine
            .reserve("card", id, &ReservationEstimateParams::new(0, 1), 1, 10)
            .unwrap();
    }
    engine.release("released").unwrap();
    engine
        .settle(
            "settled",
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
    let guard = engine.protect_reservation("live");
    assert_eq!(engine.run_janitor(700000), 0);
    let snapshot = engine.export_snapshot();
    assert!(!snapshot.reservations.contains_key("released"));
    assert!(!snapshot.reservations.contains_key("settled"));
    assert!(snapshot.reservations.contains_key("live"));
    assert_eq!(engine.run_janitor(700000), 0);
    drop(guard);
    assert_eq!(engine.run_janitor(700000), 1);
    assert!(engine.export_snapshot().reservations.is_empty());
}
