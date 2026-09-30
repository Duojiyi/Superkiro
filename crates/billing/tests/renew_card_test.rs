use billing::{
    card::{hash_card_code, Card, CardStatus},
    engine::BillingEngine,
};

fn engine() -> BillingEngine {
    let engine = BillingEngine::new();
    let mut target = Card::new("target", "group", 1000);
    target.activate(100, 86400).unwrap();
    engine.upsert_card(target);
    let mut source = Card::new("source", "group", 500);
    source.code_hash = hash_card_code("unused-card");
    engine.upsert_card(source);
    engine
}

#[test]
fn renewal_transfers_once_and_reconciles_after_restart() {
    let engine = engine();
    engine
        .renew_with_card("target", " unused-card ", 200)
        .unwrap();
    let target = engine.get_card("target").unwrap();
    assert_eq!(target.credit_total, 1500);
    assert_eq!(target.valid_until, Some(200 + 30 * 86400));
    assert_eq!(target.issued_credits, Some(1000));
    let source = engine.get_card("source").unwrap();
    assert_eq!(source.status, CardStatus::Voided);
    assert_eq!(source.available_credits(), 0);
    for (id, initial) in [("target", 1000), ("source", 500)] {
        assert!(
            engine
                .reconcile_card_balance(id, initial)
                .unwrap()
                .is_balanced
        );
    }
    let restored = BillingEngine::new();
    restored.import_snapshot(engine.export_snapshot());
    assert!(restored
        .renew_with_card("target", "unused-card", 201)
        .is_err());
    assert_eq!(restored.get_card("target").unwrap().credit_total, 1500);
}

#[test]
fn renewal_rejects_unusable_targets_without_consuming_source() {
    for status in [
        CardStatus::Frozen,
        CardStatus::Banned,
        CardStatus::Voided,
        CardStatus::Unactivated,
    ] {
        let engine = engine();
        let mut target = engine.get_card("target").unwrap();
        target.status = status;
        engine.upsert_card(target);
        assert!(engine
            .renew_with_card("target", "unused-card", 200)
            .is_err());
        assert_eq!(
            engine.get_card("source").unwrap().status,
            CardStatus::Unactivated
        );
        assert!(engine.ledger_entries().is_empty());
    }
}

#[test]
fn concurrent_renewal_consumes_source_only_once() {
    let engine = engine();
    let results: Vec<_> = (0..8)
        .map(|_| {
            let engine = engine.clone();
            std::thread::spawn(move || engine.renew_with_card("target", "unused-card", 200).is_ok())
        })
        .collect();
    assert_eq!(
        results
            .into_iter()
            .filter_map(|r| r.join().ok())
            .filter(|ok| *ok)
            .count(),
        1
    );
    assert_eq!(engine.get_card("target").unwrap().credit_total, 1500);
    assert_eq!(engine.ledger_entries().len(), 2);
}

#[test]
fn expired_card_can_renew_and_perpetual_card_stays_perpetual() {
    for perpetual in [false, true] {
        let engine = engine();
        let mut target = engine.get_card("target").unwrap();
        target.status = if perpetual {
            CardStatus::Active
        } else {
            CardStatus::Expired
        };
        target.valid_until = if perpetual { None } else { Some(150) };
        engine.upsert_card(target);
        engine
            .renew_with_card("target", "unused-card", 200)
            .unwrap();
        let target = engine.get_card("target").unwrap();
        assert_eq!(target.status, CardStatus::Active);
        assert_eq!(
            target.valid_until,
            if perpetual {
                None
            } else {
                Some(200 + 30 * 86400)
            }
        );
    }
}
