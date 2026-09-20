use billing::{BillingEngine, BillingError, Card};

fn setup(max_rebinds: u32, cooldown: u64) -> BillingEngine {
    let engine = BillingEngine::new();
    let mut card = Card::new("card", "group-pro-plus", 1_000_000_000);
    card.max_rebinds = max_rebinds;
    card.rebind_cooldown_secs = cooldown;
    engine.upsert_card(card);
    engine.bind_device("card", "old", 1).unwrap();
    engine
}

#[test]
fn unbind_then_bind_consumes_one_rebind_and_cannot_bypass_limit() {
    let engine = setup(1, 0);
    engine.unbind_device("card", "old").unwrap();
    let unbound = engine.get_card("card").unwrap();
    assert_eq!(unbound.rebind_count, 1);
    assert!(unbound.last_rebind_at.is_some());
    assert_eq!(unbound.token_version, 2);
    engine
        .bind_device("card", "new", unbound.last_rebind_at.unwrap())
        .unwrap();
    let bound = engine.get_card("card").unwrap();
    assert_eq!(
        bound.rebind_count, 1,
        "filling the paid-for slot must not charge twice"
    );
    assert!(matches!(
        engine.unbind_device("card", "new"),
        Err(BillingError::RebindLimitExceeded { current: 1, max: 1 })
    ));
    assert_eq!(engine.get_card("card").unwrap(), bound);
}

#[test]
fn unbind_obeys_cooldown_and_direct_replacement_is_always_rejected() {
    let engine = setup(5, 300);
    engine.unbind_device("card", "old").unwrap();
    let last = engine.get_card("card").unwrap().last_rebind_at.unwrap();
    engine.bind_device("card", "new", last).unwrap();
    let before = engine.get_card("card").unwrap();
    assert!(matches!(
        engine.unbind_device("card", "new"),
        Err(BillingError::RebindCooldown { .. })
    ));
    assert!(matches!(
        engine.bind_device("card", "third", last + 299),
        Err(BillingError::DeviceAlreadyBound)
    ));
    assert_eq!(engine.get_card("card").unwrap(), before);
    assert!(matches!(
        engine.bind_device("card", "third", last + 300),
        Err(BillingError::DeviceAlreadyBound)
    ));
    assert_eq!(engine.get_card("card").unwrap(), before);
}

#[test]
fn exhausted_or_unknown_device_unbind_is_non_mutating() {
    let engine = setup(0, 0);
    let before = engine.get_card("card").unwrap();
    assert!(matches!(
        engine.unbind_device("card", "missing"),
        Err(BillingError::DeviceNotFound { .. })
    ));
    assert!(matches!(
        engine.unbind_device("card", "old"),
        Err(BillingError::RebindLimitExceeded { current: 0, max: 0 })
    ));
    assert_eq!(engine.get_card("card").unwrap(), before);
}

#[test]
fn concurrent_unbinds_charge_and_revoke_only_once() {
    let engine = setup(1, 0);
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let workers: Vec<_> = (0..2)
        .map(|_| {
            let engine = engine.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                engine.unbind_device("card", "old")
            })
        })
        .collect();
    let results: Vec<_> = workers.into_iter().map(|w| w.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    let card = engine.get_card("card").unwrap();
    assert!(card.bound_devices.is_empty());
    assert_eq!(card.rebind_count, 1);
    assert_eq!(card.token_version, 2);
}

#[test]
fn concurrent_first_bindings_keep_one_device_with_rebind_allowance() {
    let engine = BillingEngine::new();
    let mut card = Card::new("card", "group-pro-plus", 1_000_000_000);
    card.max_rebinds = 5;
    engine.upsert_card(card);
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let workers: Vec<_> = ["first", "second"]
        .into_iter()
        .map(|device| {
            let engine = engine.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                engine.activate_card_with_device("card", 100, 86400, Some(device))
            })
        })
        .collect();
    let results: Vec<_> = workers.into_iter().map(|w| w.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    let card = engine.get_card("card").unwrap();
    assert_eq!(card.bound_devices.len(), 1);
    assert_eq!(card.rebind_count, 0);
    assert_eq!(card.token_version, 1);
}
