use billing::card_platform::{CardPlatformManager, PullCardsRequest};
use billing::{BillingEngine, Card, CardTemplate, Group, MasterKek};

#[test]
fn legacy_groups_default_to_issuance_enabled() {
    let mut value = serde_json::to_value(Group::pro_plus("legacy", "Legacy")).unwrap();
    value.as_object_mut().unwrap().remove("issuance_enabled");
    assert!(
        serde_json::from_value::<Group>(value)
            .unwrap()
            .issuance_enabled
    );
}

#[test]
fn disabled_group_blocks_new_issuance_without_touching_existing_cards() {
    let engine = BillingEngine::new();
    engine.set_master_kek(MasterKek::from_bytes([37; 32]));
    let mut group = Group::pro_plus("group", "Standard");
    engine.upsert_group(group.clone());
    let template = CardTemplate::tier("tier-1000", "group").unwrap();
    let existing = engine.issue_cards(&template, 1, None, 1).unwrap().remove(0);
    group.issuance_enabled = false;
    engine.upsert_group(group);
    let before = serde_json::to_value(engine.get_card(&existing.card.id)).unwrap();
    assert!(engine.issue_cards(&template, 2, None, 2).is_err());
    assert!(engine
        .insert_new_cards_checked([Card::new("new", "group", 10)])
        .is_err());
    assert_eq!(engine.list_all_cards().len(), 1);
    assert_eq!(
        serde_json::to_value(engine.get_card(&existing.card.id)).unwrap(),
        before
    );
    assert_eq!(
        engine.reveal_card_code(&existing.card.id).unwrap(),
        Some(existing.raw_code)
    );
    let restored = BillingEngine::new();
    restored.import_snapshot(engine.export_snapshot());
    assert!(!restored.get_group("group").unwrap().issuance_enabled);
}

#[test]
fn disabled_group_blocks_platform_orders_but_keeps_idempotent_replay() {
    let engine = BillingEngine::new();
    engine.set_master_kek(MasterKek::from_bytes([37; 32]));
    let mut group = Group::pro_plus("group", "Standard");
    engine.upsert_group(group.clone());
    let manager = CardPlatformManager::new();
    let template = CardTemplate::tier("tier-1000", "group").unwrap();
    let mut request = PullCardsRequest {
        order_id: "order1".into(),
        template_id: Some(template.id.clone()),
        group_id: Some("group".into()),
        count: 1,
        note: None,
    };
    let first = manager.pull_cards(&engine, &template, &request, 1).unwrap();
    group.issuance_enabled = false;
    engine.upsert_group(group);
    assert_eq!(
        manager.pull_cards(&engine, &template, &request, 2).unwrap(),
        first
    );
    request.order_id = "order2".into();
    assert!(manager.pull_cards(&engine, &template, &request, 3).is_err());
    assert_eq!(engine.list_all_cards().len(), 1);
}
