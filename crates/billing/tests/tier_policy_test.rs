use billing::{BillingEngine, Card, CardTemplate};

#[test]
fn issued_tiers_survive_balance_changes_and_serialization() {
    for (points, name) in [
        (1000, "PRO"),
        (2000, "PRO+"),
        (5000, "PRO Max"),
        (10000, "Power"),
    ] {
        let template = CardTemplate::tier(&format!("tier-{points}"), "group-pro-plus").unwrap();
        let mut card = Card::from_template("test", "hash", &template, None, 0);
        assert_eq!(card.max_devices, 1);
        card.credit_used = card.credit_total;
        card.credit_total += 50_000_000_000;
        let restored: Card = serde_json::from_str(&serde_json::to_string(&card).unwrap()).unwrap();
        assert_eq!(restored.plan_name(), Some(name));
    }
    assert_eq!(
        CardTemplate::tier("standard-monthly", "g")
            .unwrap()
            .credit_total,
        2_000_000_000
    );
    assert!(CardTemplate::tier("unknown", "g").is_none());
}

#[test]
fn legacy_entitlement_is_not_guessed_from_balance() {
    let card = Card::new("legacy", "g", 1_000_000_000);
    let mut value = serde_json::to_value(card).unwrap();
    value.as_object_mut().unwrap().remove("issued_credits");
    let card: Card = serde_json::from_value(value).unwrap();
    assert_eq!(card.plan_name(), None);
    assert!(serde_json::to_value(card)
        .unwrap()
        .get("issued_credits")
        .is_none());
}

#[test]
fn single_device_rebind_and_cross_card_conflict() {
    let engine = BillingEngine::new();
    let mut card = Card::new("a", "g", 1_000_000_000);
    card.max_devices = 3; // old capacity does not enable extra devices
    engine.upsert_card(card);
    engine.upsert_card(Card::new("b", "g", 1_000_000_000));
    engine.bind_device("a", "first", 1).unwrap();
    assert!(engine.bind_device("b", "first", 2).is_err());
    engine.bind_device("a", "second", 2).unwrap();
    let card = engine.get_card("a").unwrap();
    assert_eq!(card.max_devices, 1);
    assert_eq!(card.bound_devices, vec!["second"]);
    assert_eq!(card.token_version, 2);
    assert!(engine.bind_device("a", "third", 3).is_err());
}

#[test]
fn legacy_multiple_bindings_require_explicit_resolution() {
    let engine = BillingEngine::new();
    let mut card = Card::new("a", "g", 1_000_000_000);
    card.max_devices = 3;
    card.bound_devices = vec!["first".into(), "second".into()];
    engine.upsert_card(card.clone());
    assert!(engine.bind_device("a", "first", 1).is_err());
    assert!(engine
        .activate_card_with_device("a", 1, 0, Some("third"))
        .is_err());
    assert_eq!(engine.get_card("a").unwrap(), card);
    engine.unbind_device("a", "second").unwrap();
    engine.bind_device("a", "first", 2).unwrap();
    assert_eq!(engine.get_card("a").unwrap().max_devices, 1);
}

#[test]
fn generators_reject_multiple_device_issuance() {
    let mut template = CardTemplate::tier("tier-1000", "g").unwrap();
    template.max_devices = 2;
    assert!(billing::generate_card(&template, None, 0).is_err());
    assert!(billing::generate_batch(&template, 1, None, 0).is_err());
}

#[test]
fn entitlement_survives_engine_snapshot_reload() {
    let engine = BillingEngine::new();
    let card = Card::new("persisted", "group-pro-plus", 5_000_000_000);
    engine.upsert_card(card);
    engine.adjust_balance("persisted", -4_999_999_999, "admin", "test balance change", 1).unwrap();
    let directory = std::env::temp_dir().join(format!(
        "billing-tier-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("state.json");
    engine.save_to_file(&path).unwrap();
    let restored = BillingEngine::new();
    restored.load_from_file(&path).unwrap();
    assert_eq!(
        restored.get_card("persisted").unwrap().plan_name(),
        Some("PRO Max")
    );
}
