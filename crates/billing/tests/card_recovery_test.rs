use billing::{BillingEngine, Card, CardTemplate, MasterKek};

#[test]
fn recovery_roundtrip_legacy_and_identity_binding() {
    let engine = BillingEngine::new();
    engine.set_master_kek(MasterKek::from_bytes([37; 32]));
    let generated = engine
        .issue_cards(&CardTemplate::monthly("monthly", "group"), 2, None, 1)
        .unwrap();
    let a = &generated[0];
    assert_eq!(
        engine.reveal_card_code(&a.card.id).unwrap(),
        Some(a.raw_code.clone())
    );
    let serialized = serde_json::to_string(&a.card).unwrap();
    assert!(!serialized.contains(&a.raw_code));
    let restored: Card = serde_json::from_str(&serialized).unwrap();
    engine.upsert_card(restored);
    assert_eq!(
        engine.reveal_card_code(&a.card.id).unwrap(),
        Some(a.raw_code.clone())
    );

    let mut legacy = serde_json::to_value(Card::new("legacy", "group", 10)).unwrap();
    legacy.as_object_mut().unwrap().remove("code_encrypted");
    engine.upsert_card(serde_json::from_value(legacy).unwrap());
    assert_eq!(engine.reveal_card_code("legacy").unwrap(), None);
    assert!(engine.reveal_card_code("missing").is_err());

    let mut changed = a.card.clone();
    changed.code_encrypted = generated[1].card.code_encrypted.clone();
    engine.upsert_card(changed);
    assert!(engine.reveal_card_code(&a.card.id).is_err());
    let mut changed = a.card.clone();
    changed.code_hash = generated[1].card.code_hash.clone();
    engine.upsert_card(changed);
    assert!(engine.reveal_card_code(&a.card.id).is_err());
    let mut changed = a.card.clone();
    changed.id = "different-id".into();
    engine.upsert_card(changed);
    assert!(engine.reveal_card_code("different-id").is_err());
    let mut changed = a.card.clone();
    changed.code_encrypted.as_mut().unwrap().push('0');
    engine.upsert_card(changed);
    assert!(engine.reveal_card_code(&a.card.id).is_err());
    let mut changed = a.card.clone();
    changed.code_encrypted = Some("v1:0é0:00".into());
    engine.upsert_card(changed);
    assert!(engine.reveal_card_code(&a.card.id).is_err());
    engine.upsert_card(a.card.clone());
    engine.set_master_kek(MasterKek::from_bytes([38; 32]));
    assert!(engine.reveal_card_code(&a.card.id).is_err());
}

#[test]
fn missing_kek_never_issues_or_recovers() {
    let engine = BillingEngine::new();
    assert!(
        engine.master_kek().is_none(),
        "run test without KIRO_MASTER_KEK"
    );
    assert!(engine
        .issue_cards(&CardTemplate::monthly("monthly", "group"), 2, None, 1)
        .is_err());
    assert!(engine.list_all_cards().is_empty());
    let mut card = Card::new("encrypted", "group", 1);
    card.code_encrypted = Some("v1:invalid:invalid".into());
    engine.upsert_card(card);
    assert!(engine.reveal_card_code("encrypted").is_err());
}

#[test]
fn encrypted_snapshot_restores_recovery_and_failed_commit_issues_nothing() {
    let path = std::env::temp_dir().join(format!(
        "card-recovery-{}-{}.json",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let engine = BillingEngine::new();
    let kek = MasterKek::from_bytes([37; 32]);
    engine.set_master_kek(kek.clone());
    engine.set_persistence_path(&path);
    let template = CardTemplate::monthly("monthly", "group");
    let generated = engine.issue_cards(&template, 1, None, 1).unwrap();
    assert!(!std::fs::read_to_string(&path)
        .unwrap()
        .contains(&generated[0].raw_code));
    let recovered = BillingEngine::new();
    recovered.set_master_kek(kek);
    recovered.load_from_file(&path).unwrap();
    assert_eq!(
        recovered.reveal_card_code(&generated[0].card.id).unwrap(),
        Some(generated[0].raw_code.clone())
    );
    engine.inject_persistence_fault(true);
    assert!(engine.issue_cards(&template, 1, None, 2).is_err());
    assert_eq!(engine.list_all_cards().len(), 1);
    std::fs::remove_file(&path).unwrap();
}

#[test]
fn issuance_collision_never_overwrites_existing_cards_or_partially_inserts() {
    let engine = BillingEngine::new();
    engine.upsert_card(Card::new("existing", "original-group", 123));
    assert!(engine
        .insert_new_cards_checked([
            Card::new("new", "group", 50),
            Card::new("existing", "replacement", 999),
        ])
        .is_err());
    assert!(engine.get_card("new").is_none());
    let original = engine.get_card("existing").unwrap();
    assert_eq!(original.credit_total, 123);
    assert_eq!(original.group_id, "original-group");
    assert!(engine
        .insert_new_cards_checked([
            Card::new("duplicate", "group", 50),
            Card::new("duplicate", "group", 60),
        ])
        .is_err());
    assert!(engine.get_card("duplicate").is_none());
}
