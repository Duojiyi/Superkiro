//! Tests for P2-5c: Manual balance adjustment via ledger entries (Spec §5, §14.9, §14.10.5)
//! and unactivated card voiding/recycling (Spec §14.9).

use billing::card::{Card, CardError, CardStatus};
use billing::engine::{BillingEngine, BillingError};
use billing::generator::generate_batch;
use billing::ledger::{LedgerKind, UsageTokens};
use billing::reservation::ReservationEstimateParams;
use billing::template::CardTemplate;
use billing::topup::TopupCode;
use billing::MICRO_CREDITS_PER_CREDIT;

const NOW_SECS: u64 = 1_773_100_000;

#[test]
fn test_manual_adjustment_positive_credit_grant() {
    let engine = BillingEngine::new();
    let card = Card::new(
        "card-adj-1",
        "group-default",
        100 * MICRO_CREDITS_PER_CREDIT,
    );
    engine.upsert_card(card);

    let grant_amount = 50 * MICRO_CREDITS_PER_CREDIT;
    let entry = engine
        .adjust_balance(
            "card-adj-1",
            grant_amount,
            "operator-ops-1",
            "Goodwill compensation for downtime",
            NOW_SECS,
        )
        .expect("Adjustment should succeed");

    assert_eq!(entry.card_id, "card-adj-1");
    assert_eq!(entry.kind, LedgerKind::Adjustment);
    assert_eq!(entry.credits_charged, grant_amount);
    assert_eq!(entry.operator_id.as_deref(), Some("operator-ops-1"));
    assert_eq!(
        entry.reason.as_deref(),
        Some("Goodwill compensation for downtime")
    );
    assert_eq!(entry.provider_cost_micro_cny, 0);

    let updated = engine.get_card("card-adj-1").unwrap();
    assert_eq!(updated.credit_total, 150 * MICRO_CREDITS_PER_CREDIT);
    assert_eq!(updated.credit_used, 0);
    assert_eq!(updated.available_credits(), 150 * MICRO_CREDITS_PER_CREDIT);
}

#[test]
fn test_manual_adjustment_negative_deduction() {
    let engine = BillingEngine::new();
    let card = Card::new(
        "card-adj-2",
        "group-default",
        100 * MICRO_CREDITS_PER_CREDIT,
    );
    engine.upsert_card(card);

    let deduct_amount = -30 * MICRO_CREDITS_PER_CREDIT;
    let entry = engine
        .adjust_balance(
            "card-adj-2",
            deduct_amount,
            "operator-finance-1",
            "Manual deduction for offline refund",
            NOW_SECS,
        )
        .expect("Deduction should succeed");

    assert_eq!(entry.card_id, "card-adj-2");
    assert_eq!(entry.kind, LedgerKind::Adjustment);
    assert_eq!(entry.credits_charged, deduct_amount);
    assert_eq!(entry.operator_id.as_deref(), Some("operator-finance-1"));
    assert_eq!(
        entry.reason.as_deref(),
        Some("Manual deduction for offline refund")
    );

    let updated = engine.get_card("card-adj-2").unwrap();
    assert_eq!(updated.credit_total, 100 * MICRO_CREDITS_PER_CREDIT);
    assert_eq!(updated.credit_used, 30 * MICRO_CREDITS_PER_CREDIT);
    assert_eq!(updated.available_credits(), 70 * MICRO_CREDITS_PER_CREDIT);
}

#[test]
fn test_manual_adjustment_insufficient_credit_rejection() {
    let engine = BillingEngine::new();
    let card = Card::new("card-adj-3", "group-default", 50 * MICRO_CREDITS_PER_CREDIT);
    engine.upsert_card(card);

    // Attempt to deduct 60 credits when only 50 are available
    let res = engine.adjust_balance(
        "card-adj-3",
        -60 * MICRO_CREDITS_PER_CREDIT,
        "operator-finance-1",
        "Excessive deduction attempt",
        NOW_SECS,
    );

    match res {
        Err(BillingError::Card(CardError::InsufficientCredit { available, needed })) => {
            assert_eq!(available, 50 * MICRO_CREDITS_PER_CREDIT);
            assert_eq!(needed, 60 * MICRO_CREDITS_PER_CREDIT);
        }
        other => panic!("Expected InsufficientCredit error, got {:?}", other),
    }

    // Verify card is untouched
    let card = engine.get_card("card-adj-3").unwrap();
    assert_eq!(card.credit_total, 50 * MICRO_CREDITS_PER_CREDIT);
    assert_eq!(card.credit_used, 0);

    // Verify no ledger entries were created
    let entries = engine.list_ledger_entries_for_card("card-adj-3", None);
    assert!(entries.is_empty());
}

#[test]
fn test_manual_adjustment_validation_guards() {
    let engine = BillingEngine::new();
    let card = Card::new(
        "card-adj-4",
        "group-default",
        100 * MICRO_CREDITS_PER_CREDIT,
    );
    engine.upsert_card(card);

    // Guard 1: Zero delta
    let res_zero = engine.adjust_balance("card-adj-4", 0, "op-1", "reason", NOW_SECS);
    assert!(matches!(res_zero, Err(BillingError::InvalidAdjustment(_))));

    // Guard 2: Empty operator
    let res_empty_op = engine.adjust_balance("card-adj-4", 10, "   ", "reason", NOW_SECS);
    assert!(matches!(
        res_empty_op,
        Err(BillingError::InvalidAdjustment(_))
    ));

    // Guard 3: Empty reason
    let res_empty_reason = engine.adjust_balance("card-adj-4", 10, "op-1", "\t\n ", NOW_SECS);
    assert!(matches!(
        res_empty_reason,
        Err(BillingError::InvalidAdjustment(_))
    ));

    // Guard 4: Non-existent card
    let res_not_found = engine.adjust_balance("non-existent", 10, "op-1", "reason", NOW_SECS);
    assert!(matches!(res_not_found, Err(BillingError::CardNotFound(_))));
}

#[test]
fn test_ledger_reconciliation_mathematical_consistency() {
    let engine = BillingEngine::new();
    let initial_credits = 100 * MICRO_CREDITS_PER_CREDIT;
    let mut card = Card::new("card-reconcile", "group-default", initial_credits);
    card.activate(NOW_SECS, 30 * 86_400).unwrap();
    engine.upsert_card(card);

    // 1. First usage: reserve + settle
    let params = ReservationEstimateParams::new(1_000, 500);
    engine
        .reserve("card-reconcile", "inv-rec-1", &params, NOW_SECS, 300)
        .unwrap();
    let tokens = UsageTokens {
        uncached_input_tokens: 1_000,
        output_tokens: 500,
        cache_creation_tokens: 0,
        cache_read_tokens: 0,
    };
    let settle_entry = engine
        .settle(
            "inv-rec-1",
            &tokens,
            "claude-3-5-sonnet",
            "anthropic",
            "claude-3-5-sonnet",
            NOW_SECS + 5,
        )
        .unwrap();
    let usage_charge = settle_entry.credits_charged;

    // 2. Top-up redemption
    let topup_code = "kiro-topup-reconcile-001";
    let code_hash = billing::card::hash_card_code(topup_code);
    let topup = TopupCode {
        id: "topup-rec-1".to_string(),
        code_hash,
        credit_amount: 50 * MICRO_CREDITS_PER_CREDIT,
        duration_extension_secs: 86400,
        is_used: false,
        used_by_card_id: None,
        used_at: None,
        created_at: NOW_SECS,
    };
    engine.upsert_topup_code(topup).unwrap();
    engine
        .redeem_topup(
            "card-reconcile",
            topup_code,
            NOW_SECS + 10,
            "admin-topup-ops",
        )
        .unwrap();

    // 3. Positive manual adjustment
    engine
        .adjust_balance(
            "card-reconcile",
            20 * MICRO_CREDITS_PER_CREDIT,
            "op-1",
            "Bonus credits",
            NOW_SECS + 15,
        )
        .unwrap();

    // 4. Negative manual adjustment
    engine
        .adjust_balance(
            "card-reconcile",
            -10 * MICRO_CREDITS_PER_CREDIT,
            "op-2",
            "Correction deduction",
            NOW_SECS + 20,
        )
        .unwrap();

    // 5. Run mathematical reconciliation against ledger (Spec §5, §14.9)
    let reconciliation = engine
        .reconcile_card_balance("card-reconcile", initial_credits)
        .expect("Reconciliation should succeed");

    assert_eq!(reconciliation.initial_credits, initial_credits);
    assert_eq!(
        reconciliation.total_topup_credits,
        50 * MICRO_CREDITS_PER_CREDIT
    );
    assert_eq!(
        reconciliation.total_positive_adjustments,
        20 * MICRO_CREDITS_PER_CREDIT
    );
    assert_eq!(
        reconciliation.total_negative_adjustments,
        10 * MICRO_CREDITS_PER_CREDIT
    );
    assert_eq!(reconciliation.total_usage_charges, usage_charge);

    let expected_total =
        initial_credits + 50 * MICRO_CREDITS_PER_CREDIT + 20 * MICRO_CREDITS_PER_CREDIT;
    let expected_used = usage_charge + 10 * MICRO_CREDITS_PER_CREDIT;

    assert_eq!(reconciliation.expected_credit_total, expected_total);
    assert_eq!(reconciliation.actual_credit_total, expected_total);
    assert_eq!(reconciliation.expected_credit_used, expected_used);
    assert_eq!(reconciliation.actual_credit_used, expected_used);
    assert!(
        reconciliation.is_balanced,
        "Card balance must be strictly reconciled with ledger"
    );

    // 6. Test list_ledger_entries_for_card
    let all_entries = engine.list_ledger_entries_for_card("card-reconcile", None);
    assert_eq!(all_entries.len(), 4); // 1 usage, 1 topup, 2 adjustments

    let adj_only =
        engine.list_ledger_entries_for_card("card-reconcile", Some(LedgerKind::Adjustment));
    assert_eq!(adj_only.len(), 2);
}

#[test]
fn test_void_unactivated_card_success() {
    let engine = BillingEngine::new();
    let template = CardTemplate::monthly("template-monthly", "group-default");
    let batch = generate_batch(&template, 1, Some("batch-void"), NOW_SECS).unwrap();
    let generated = batch.into_iter().next().unwrap();
    let card_id = generated.card.id.clone();
    engine.upsert_card(generated.card);

    // Verify initial status is Unactivated
    assert_eq!(
        engine.get_card(&card_id).unwrap().status,
        CardStatus::Unactivated
    );
    let initial_token_version = engine.get_card(&card_id).unwrap().token_version;

    // Void the unactivated card
    let voided_card = engine
        .void_unactivated_card(
            &card_id,
            "operator-inventory-1",
            "Batch misprint recycling",
            NOW_SECS + 100,
        )
        .expect("Voiding unactivated card must succeed");

    assert_eq!(voided_card.status, CardStatus::Voided);
    assert_eq!(voided_card.token_version, initial_token_version + 1);
    assert!(voided_card
        .note
        .as_deref()
        .unwrap()
        .contains("[VOIDED by operator-inventory-1 at 1773100100: Batch misprint recycling]"));

    // Verify in engine
    let stored = engine.get_card(&card_id).unwrap();
    assert_eq!(stored.status, CardStatus::Voided);
}

#[test]
fn test_voided_card_cannot_be_activated() {
    let mut card = Card::new(
        "card-voided",
        "group-default",
        100 * MICRO_CREDITS_PER_CREDIT,
    );
    card.status = CardStatus::Voided;

    let res = card.activate(NOW_SECS, 86400);
    assert_eq!(res, Err(CardError::NotActive(CardStatus::Voided)));
}

#[test]
fn test_cannot_void_activated_or_used_card() {
    let engine = BillingEngine::new();

    // 1. Active card
    let mut active_card = Card::new(
        "card-active",
        "group-default",
        100 * MICRO_CREDITS_PER_CREDIT,
    );
    active_card.status = CardStatus::Active;
    engine.upsert_card(active_card);

    let err_active = engine.void_unactivated_card("card-active", "op-1", "test", NOW_SECS);
    assert_eq!(
        err_active,
        Err(BillingError::CannotVoidActivatedCard {
            card_id: "card-active".to_string(),
            current_status: CardStatus::Active,
        })
    );

    // 2. Frozen card
    let mut frozen_card = Card::new(
        "card-frozen",
        "group-default",
        100 * MICRO_CREDITS_PER_CREDIT,
    );
    frozen_card.status = CardStatus::Frozen;
    engine.upsert_card(frozen_card);

    let err_frozen = engine.void_unactivated_card("card-frozen", "op-1", "test", NOW_SECS);
    assert_eq!(
        err_frozen,
        Err(BillingError::CannotVoidActivatedCard {
            card_id: "card-frozen".to_string(),
            current_status: CardStatus::Frozen,
        })
    );

    // 3. Banned card
    let mut banned_card = Card::new(
        "card-banned",
        "group-default",
        100 * MICRO_CREDITS_PER_CREDIT,
    );
    banned_card.status = CardStatus::Banned;
    engine.upsert_card(banned_card);

    let err_banned = engine.void_unactivated_card("card-banned", "op-1", "test", NOW_SECS);
    assert_eq!(
        err_banned,
        Err(BillingError::CannotVoidActivatedCard {
            card_id: "card-banned".to_string(),
            current_status: CardStatus::Banned,
        })
    );

    // 4. Expired card
    let mut expired_card = Card::new(
        "card-expired",
        "group-default",
        100 * MICRO_CREDITS_PER_CREDIT,
    );
    expired_card.status = CardStatus::Expired;
    engine.upsert_card(expired_card);

    let err_expired = engine.void_unactivated_card("card-expired", "op-1", "test", NOW_SECS);
    assert_eq!(
        err_expired,
        Err(BillingError::CannotVoidActivatedCard {
            card_id: "card-expired".to_string(),
            current_status: CardStatus::Expired,
        })
    );
}

#[test]
fn test_batch_void_unactivated_cards() {
    let engine = BillingEngine::new();
    let template = CardTemplate::weekly("tpl-weekly", "group-default");
    let batch = generate_batch(&template, 3, Some("batch-test"), NOW_SECS).unwrap();

    let id0 = batch[0].card.id.clone();
    let id1 = batch[1].card.id.clone();
    let id2 = batch[2].card.id.clone();

    for g in batch {
        engine.upsert_card(g.card);
    }

    // Activate id1
    let mut c1 = engine.get_card(&id1).unwrap();
    c1.activate(NOW_SECS, 604800).unwrap();
    engine.upsert_card(c1);

    // Batch void all three
    let ids = vec![id0.clone(), id1.clone(), id2.clone()];
    let results = engine.batch_void_unactivated_cards(
        &ids,
        "operator-inventory-batch",
        "Inventory clean up",
        NOW_SECS + 50,
    );

    assert_eq!(results.len(), 3);
    assert!(results[0].is_ok());
    assert_eq!(results[0].as_ref().unwrap().status, CardStatus::Voided);

    assert!(results[1].is_err());
    assert_eq!(
        results[1].as_ref().unwrap_err(),
        &BillingError::CannotVoidActivatedCard {
            card_id: id1.clone(),
            current_status: CardStatus::Active,
        }
    );

    assert!(results[2].is_ok());
    assert_eq!(results[2].as_ref().unwrap().status, CardStatus::Voided);
}

#[test]
fn test_card_inventory_tracking_by_template_and_status() {
    let engine = BillingEngine::new();
    let template = CardTemplate::monthly("tpl-inv-monthly", "group-default");
    let batch = generate_batch(&template, 5, Some("inv-batch"), NOW_SECS).unwrap();
    let card_ids: Vec<String> = batch.iter().map(|g| g.card.id.clone()).collect();

    for g in batch {
        engine.upsert_card(g.card);
    }

    // Initial count: 5 unactivated, 0 voided, 0 active
    assert_eq!(
        engine.count_cards_by_template_and_status("tpl-inv-monthly", CardStatus::Unactivated),
        5
    );
    assert_eq!(
        engine.count_cards_by_template_and_status("tpl-inv-monthly", CardStatus::Voided),
        0
    );

    // Void cards 0 and 1
    engine
        .void_unactivated_card(&card_ids[0], "op-inv", "Void 1", NOW_SECS)
        .unwrap();
    engine
        .void_unactivated_card(&card_ids[1], "op-inv", "Void 2", NOW_SECS)
        .unwrap();

    // Activate card 2
    let mut c2 = engine.get_card(&card_ids[2]).unwrap();
    c2.activate(NOW_SECS, 2592000).unwrap();
    engine.upsert_card(c2);

    // Counts: 2 unactivated, 2 voided, 1 active
    assert_eq!(
        engine.count_cards_by_template_and_status("tpl-inv-monthly", CardStatus::Unactivated),
        2
    );
    assert_eq!(
        engine.count_cards_by_template_and_status("tpl-inv-monthly", CardStatus::Voided),
        2
    );
    assert_eq!(
        engine.count_cards_by_template_and_status("tpl-inv-monthly", CardStatus::Active),
        1
    );

    let voided_list = engine.list_cards_by_status(CardStatus::Voided);
    assert_eq!(voided_list.len(), 2);
    assert!(voided_list.iter().any(|c| c.id == card_ids[0]));
    assert!(voided_list.iter().any(|c| c.id == card_ids[1]));
}

#[test]
fn test_adjust_balance_persistence_failure_leaves_memory_untouched() {
    let path = std::env::temp_dir().join(format!("kiro_test_tx_fail_{}.json", std::process::id()));
    let engine = BillingEngine::new();
    let card = Card::new(
        "card-tx-fail",
        "group-default",
        100 * MICRO_CREDITS_PER_CREDIT,
    );
    engine.upsert_card(card);
    engine.save_to_file(&path).unwrap();

    // Inject persistence failure
    engine.inject_persistence_fault(true);
    let grant_amount = 50 * MICRO_CREDITS_PER_CREDIT;
    let res = engine.adjust_balance(
        "card-tx-fail",
        grant_amount,
        "operator-1",
        "Test fault injection",
        NOW_SECS,
    );

    assert!(res.is_err(), "Adjustment must fail when persistence fails");
    match res.unwrap_err() {
        BillingError::Persistence(msg) => {
            assert!(msg.contains("injected persistence fault"));
        }
        other => panic!("Expected Persistence error, got: {:?}", other),
    }

    // Crucial check: In-memory card state must be completely unchanged
    let card = engine.get_card("card-tx-fail").unwrap();
    assert_eq!(
        card.credit_total,
        100 * MICRO_CREDITS_PER_CREDIT,
        "In-memory credit_total must NOT be modified when persistence fails"
    );
    assert_eq!(card.credit_used, 0);
    assert_eq!(
        card.available_credits(),
        100 * MICRO_CREDITS_PER_CREDIT,
        "In-memory available_credits must NOT be modified when persistence fails"
    );

    // Ledger must not contain the failed entry
    let ledger = engine.ledger_entries();
    assert!(
        !ledger.iter().any(|e| e.card_id == "card-tx-fail"),
        "Ledger must NOT contain entry when persistence fails"
    );

    // Persistence readiness must reflect the failure
    assert!(!engine.persistence_ready());

    // Recover persistence and verify subsequent operation succeeds
    engine.inject_persistence_fault(false);
    let ok_entry = engine
        .adjust_balance(
            "card-tx-fail",
            grant_amount,
            "operator-1",
            "Retry after recovery",
            NOW_SECS + 1,
        )
        .expect("Adjustment should succeed once fault is cleared");
    assert_eq!(ok_entry.credits_charged, grant_amount);
    let recovered_card = engine.get_card("card-tx-fail").unwrap();
    assert_eq!(
        recovered_card.available_credits(),
        150 * MICRO_CREDITS_PER_CREDIT
    );
    assert!(engine.persistence_ready());

    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_adjust_balance_idempotency_and_replay() {
    let path = std::env::temp_dir().join(format!("kiro_test_tx_idem_{}.json", std::process::id()));
    let engine = BillingEngine::new();
    let card = Card::new(
        "card-idempotent-1",
        "group-default",
        100 * MICRO_CREDITS_PER_CREDIT,
    );
    engine.upsert_card(card);
    engine.save_to_file(&path).unwrap();

    let key = "tx-idem-001";
    let delta = 30 * MICRO_CREDITS_PER_CREDIT;

    // 1. First execution
    let first = engine
        .adjust_balance_idempotent(
            "card-idempotent-1",
            delta,
            "op-admin",
            "First attempt",
            NOW_SECS,
            Some(key),
        )
        .expect("First adjustment must succeed");
    assert_eq!(first.credits_charged, delta);

    let c = engine.get_card("card-idempotent-1").unwrap();
    assert_eq!(c.available_credits(), 130 * MICRO_CREDITS_PER_CREDIT);

    // 2. Replay with identical key & parameters
    let replay = engine
        .adjust_balance_idempotent(
            "card-idempotent-1",
            delta,
            "op-admin",
            "First attempt",
            NOW_SECS + 10,
            Some(key),
        )
        .expect("Replay must succeed idempotently");
    assert_eq!(replay.id, first.id);
    assert_eq!(replay.credits_charged, first.credits_charged);

    // Balance must STILL be 130 (no double crediting)
    let c_after = engine.get_card("card-idempotent-1").unwrap();
    assert_eq!(
        c_after.available_credits(),
        130 * MICRO_CREDITS_PER_CREDIT,
        "Replay must not alter balance"
    );

    // Ledger must contain exactly 1 entry with this invocation_id
    let ledger = engine.ledger_entries();
    let matches: Vec<_> = ledger
        .iter()
        .filter(|e| e.invocation_id.as_deref() == Some(key))
        .collect();
    assert_eq!(
        matches.len(),
        1,
        "Ledger must contain exactly 1 entry for idempotent key"
    );

    // 3. Conflict: same key with different amount
    let conflict = engine.adjust_balance_idempotent(
        "card-idempotent-1",
        50 * MICRO_CREDITS_PER_CREDIT,
        "op-admin",
        "Conflicting attempt",
        NOW_SECS + 20,
        Some(key),
    );
    assert!(conflict.is_err(), "Conflicting key must be rejected");
    match conflict.unwrap_err() {
        BillingError::InvalidAdjustment(msg) => {
            assert!(msg.contains("Idempotency conflict"));
        }
        other => panic!("Expected Idempotency conflict, got: {:?}", other),
    }

    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_reserve_and_settle_persistence_failure_leaves_memory_untouched() {
    let path = std::env::temp_dir().join(format!("kiro_test_tx_res_{}.json", std::process::id()));
    let engine = BillingEngine::new();
    let mut card = Card::new(
        "card-res-tx",
        "group-default",
        100 * MICRO_CREDITS_PER_CREDIT,
    );
    card.status = CardStatus::Active;
    engine.upsert_card(card);
    engine.save_to_file(&path).unwrap();

    let params = ReservationEstimateParams {
        estimated_input_tokens: 1_000,
        max_output_tokens: 2_000,
        input_rate_per_m: 15_000_000,
        output_rate_per_m: 60_000_000,
        credit_multiplier: 1.0,
        margin_multiplier: 1.0,
        model: None,
    };

    // 1. A hold moves no money and is not written, so a storage fault does not refuse it.
    engine.inject_persistence_fault(true);
    let reservation = engine
        .reserve("card-res-tx", "inv-fault-1", &params, NOW_SECS, 300)
        .expect("a hold is taken in memory");
    let reserved_amt = reservation.reserved_micro_credits;
    let card = engine.get_card("card-res-tx").unwrap();
    assert_eq!(card.credit_reserved, reserved_amt);

    // 2. The settlement is written, and its failed write changes no balance.
    let tokens = UsageTokens {
        uncached_input_tokens: 100,
        output_tokens: 100,
        cache_creation_tokens: 0,
        cache_read_tokens: 0,
    };
    let settle_res = engine.settle(
        "inv-fault-1",
        &tokens,
        "claude-sonnet-4.5",
        "provider-1",
        "claude-sonnet-4.5",
        NOW_SECS + 5,
    );
    assert!(settle_res.is_err());

    // In-memory state must remain in pre-settlement state:
    let card = engine.get_card("card-res-tx").unwrap();
    assert_eq!(
        card.credit_used, 0,
        "credit_used must NOT be updated when settle persistence fails"
    );
    assert_eq!(
        card.credit_reserved, reserved_amt,
        "credit_reserved must still hold the reservation when settle persistence fails"
    );
    let ledger = engine.ledger_entries();
    assert!(
        !ledger
            .iter()
            .any(|e| e.invocation_id.as_deref() == Some("inv-fault-1")),
        "Ledger must NOT contain settlement entry when persistence fails"
    );
    // The priced usage is kept for the recovery task, never refunded.
    assert_eq!(engine.list_pending_settlements().len(), 1);

    // 3. Once a write has failed, no new hold is taken until one succeeds.
    assert!(matches!(
        engine.reserve("card-res-tx", "inv-fault-2", &params, NOW_SECS, 300),
        Err(BillingError::Persistence(_))
    ));
    assert_eq!(
        engine.get_card("card-res-tx").unwrap().credit_reserved,
        reserved_amt
    );
    // Storage is back: the recovery task completes the settlement, once, and the card
    // takes holds again.
    engine.inject_persistence_fault(false);
    let entry = engine.retry_pending_settlement("inv-fault-1").unwrap();
    assert_eq!(
        engine.get_card("card-res-tx").unwrap().credit_used,
        entry.credits_charged
    );
    engine
        .reserve("card-res-tx", "inv-fault-2", &params, NOW_SECS, 300)
        .expect("holds are taken again once storage is writable");

    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_void_persists_audit_and_retry_is_noop() {
    let path = std::env::temp_dir().join(format!("kiro_void_retry_{}.json", std::process::id()));
    let engine = BillingEngine::new();
    engine.upsert_card(Card::new("void-retry", "group-default", 100_000));
    let grant = engine
        .adjust_balance_idempotent(
            "void-retry",
            10_000,
            "admin",
            "grant",
            NOW_SECS,
            Some("pre-void-grant"),
        )
        .unwrap();
    engine.save_to_file(&path).unwrap();
    let before = engine.ledger_entries();
    let original = engine.get_card("void-retry").unwrap();

    engine.inject_persistence_fault(true);
    assert!(engine
        .void_unactivated_card("void-retry", "admin", "misprint", NOW_SECS)
        .is_err());
    assert_eq!(engine.get_card("void-retry").unwrap(), original);
    assert_eq!(
        serde_json::to_value(engine.ledger_entries()).unwrap(),
        serde_json::to_value(&before).unwrap()
    );
    engine.inject_persistence_fault(false);
    let voided = engine
        .void_unactivated_card("void-retry", "admin", "misprint", NOW_SECS)
        .unwrap();
    assert_eq!(voided.credit_total, original.credit_total);
    assert_eq!(voided.credit_used, original.credit_used);
    assert_eq!(voided.status, CardStatus::Voided);
    assert_eq!(
        engine
            .void_unactivated_card("void-retry", "other-admin", "retry", NOW_SECS + 1)
            .unwrap(),
        voided
    );

    let restored = BillingEngine::new();
    restored.load_from_file(&path).unwrap();
    assert_eq!(restored.get_card("void-retry").unwrap(), voided);
    assert_eq!(
        restored
            .void_unactivated_card("void-retry", "admin", "retry after restart", NOW_SECS + 2)
            .unwrap(),
        voided
    );
    restored
        .upsert_topup_code(TopupCode::new(
            "void-topup",
            billing::card::hash_card_code("unused-topup"),
            100,
            100,
            NOW_SECS,
        ))
        .unwrap();
    assert!(restored
        .redeem_topup("void-retry", "unused-topup", NOW_SECS, "admin")
        .is_err());
    assert!(!restored.get_topup_code("void-topup").unwrap().is_used);
    assert!(restored.freeze_card("void-retry", "test").is_err());
    assert!(restored.unfreeze_card("void-retry").is_err());
    assert!(restored.ban_card("void-retry", "test").is_err());
    assert!(restored.activate_card("void-retry", NOW_SECS, 100).is_err());
    for delta in [1, -1] {
        assert!(restored
            .adjust_balance("void-retry", delta, "admin", "test", NOW_SECS)
            .is_err());
    }
    assert_eq!(restored.get_card("void-retry").unwrap(), voided);
    let replay = restored
        .adjust_balance_idempotent(
            "void-retry",
            10_000,
            "admin",
            "grant",
            NOW_SECS + 3,
            Some("pre-void-grant"),
        )
        .unwrap();
    assert_eq!(replay.id, grant.id);
    let rejected = restored
        .adjust_balance_idempotent(
            "void-retry",
            10_000,
            "admin",
            "grant",
            NOW_SECS + 3,
            Some("uncommitted-grant"),
        )
        .unwrap_err();
    assert_eq!(
        rejected.to_string(),
        "Invalid billing state: cannot adjust a voided card"
    );
    let entries = restored.ledger_entries();
    assert_eq!(entries.len(), before.len() + 1);
    assert_eq!(
        serde_json::to_value(&entries[..before.len()]).unwrap(),
        serde_json::to_value(&before).unwrap()
    );
    let audit = entries.last().unwrap();
    assert_eq!(audit.credits_charged, 0);
    assert_eq!(audit.operator_id.as_deref(), Some("admin"));
    assert_eq!(audit.reason.as_deref(), Some("misprint"));
    assert_eq!(audit.exposed_model, "void_card");
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(path.with_extension("json.anchor"));
}

#[test]
fn test_void_rejects_unactivated_status_with_activation_or_usage_evidence() {
    let engine = BillingEngine::new();
    for evidence in 0..5 {
        let mut card = Card::new(format!("used-{evidence}"), "group-default", 100_000);
        match evidence {
            0 => card.activated_at = Some(NOW_SECS),
            1 => card.valid_until = Some(NOW_SECS + 100),
            2 => card.credit_used = 1,
            3 => card.credit_reserved = 1,
            _ => card.bound_devices.push("device".into()),
        }
        engine.upsert_card(card.clone());
        assert!(matches!(
            engine.void_unactivated_card(&card.id, "admin", "test", NOW_SECS),
            Err(BillingError::CannotVoidActivatedCard { .. })
        ));
        assert_eq!(engine.get_card(&card.id).unwrap(), card);
    }
    assert!(engine.ledger_entries().is_empty());
}

#[test]
fn test_archive_is_audited_persistent_metadata_only() {
    let engine = BillingEngine::new();
    let mut card = Card::new("archive-test", "group-default", 100_000);
    card.status = CardStatus::Banned;
    let mut legacy = serde_json::to_value(&card).unwrap();
    legacy.as_object_mut().unwrap().remove("archived_at");
    assert_eq!(serde_json::from_value::<Card>(legacy).unwrap(), card);
    engine.upsert_card(card.clone());
    let path = std::env::temp_dir().join(format!("kiro_archive_{}.json", std::process::id()));
    engine.save_to_file(&path).unwrap();
    engine.inject_persistence_fault(true);
    assert!(engine
        .set_card_archived(&card.id, true, "admin", "cleanup", NOW_SECS)
        .is_err());
    assert_eq!(engine.get_card(&card.id).unwrap(), card);
    assert!(engine.ledger_entries().is_empty());
    engine.inject_persistence_fault(false);
    let mut archived = card.clone();
    archived.archived_at = Some(NOW_SECS);
    assert_eq!(
        engine
            .set_card_archived(&card.id, true, "admin", "cleanup", NOW_SECS)
            .unwrap(),
        archived
    );
    let restored = BillingEngine::new();
    restored.load_from_file(&path).unwrap();
    assert_eq!(restored.get_card(&card.id).unwrap(), archived);
    assert_eq!(
        restored
            .set_card_archived(&card.id, true, "admin", "retry", NOW_SECS + 1)
            .unwrap(),
        archived
    );
    assert_eq!(restored.ledger_entries().len(), 1);
    for _ in 0..2 {
        assert_eq!(
            restored
                .set_card_archived(&card.id, false, "admin", "show again", NOW_SECS + 2)
                .unwrap(),
            card
        );
    }
    assert!(restored.activate_card(&card.id, NOW_SECS + 3, 100).is_err());
    let entries = restored.ledger_entries();
    assert_eq!(entries.len(), 2);
    for (entry, action, reason, time) in [
        (&entries[0], "archive_card", "cleanup", NOW_SECS),
        (&entries[1], "unarchive_card", "show again", NOW_SECS + 2),
    ] {
        assert_eq!(entry.exposed_model, action);
        assert_eq!(entry.operator_id.as_deref(), Some("admin"));
        assert_eq!(entry.reason.as_deref(), Some(reason));
        assert_eq!(entry.ts_secs, time);
        assert_eq!(entry.credits_charged, 0);
    }
    let reloaded = BillingEngine::new();
    reloaded.load_from_file(&path).unwrap();
    assert_eq!(reloaded.get_card(&card.id).unwrap(), card);
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(path.with_extension("json.anchor"));
}

#[test]
fn test_archive_eligibility_and_reservations() {
    let engine = BillingEngine::new();
    for (index, status, until, allowed) in [
        (0, CardStatus::Unactivated, None, false),
        (1, CardStatus::Active, None, false),
        (2, CardStatus::Frozen, None, false),
        (3, CardStatus::Banned, None, true),
        (4, CardStatus::Expired, None, true),
        (5, CardStatus::Voided, None, true),
        (6, CardStatus::Active, Some(NOW_SECS), true),
        (7, CardStatus::Active, Some(NOW_SECS + 1), false),
    ] {
        for reserved in [0, 1] {
            let mut card = Card::new(
                format!("archive-{index}-{reserved}"),
                "group-default",
                100_000,
            );
            card.status = status;
            card.valid_until = until;
            card.credit_reserved = reserved;
            engine.upsert_card(card.clone());
            let before = engine.ledger_entries().len();
            let result = engine.set_card_archived(&card.id, true, "admin", "cleanup", NOW_SECS);
            if allowed && reserved == 0 {
                let mut expected = card.clone();
                expected.archived_at = Some(NOW_SECS);
                assert_eq!(result.unwrap(), expected);
                assert_eq!(
                    engine
                        .set_card_archived(&card.id, false, "admin", "restore visibility", NOW_SECS)
                        .unwrap(),
                    card
                );
                assert!(engine
                    .get_card(&card.id)
                    .unwrap()
                    .check_active(NOW_SECS)
                    .is_err());
            } else {
                assert!(result.is_err());
                assert_eq!(engine.get_card(&card.id).unwrap(), card);
                assert_eq!(engine.ledger_entries().len(), before);
            }
        }
    }
}

#[test]
fn activated_card_void_preserves_accounts_and_revokes_access() {
    let engine = BillingEngine::new();
    let mut card = Card::new("used-delete", "group-default", 100_000_000);
    card.status = CardStatus::Active;
    card.activated_at = Some(NOW_SECS - 100);
    card.credit_used = 14_600_000;
    let version = card.token_version;
    engine.upsert_card(card);
    let result = engine
        .void_card("used-delete", "admin", "user requested removal", NOW_SECS)
        .unwrap();
    assert_eq!(result.status, CardStatus::Voided);
    assert_eq!(result.credit_total, 100_000_000);
    assert_eq!(result.credit_used, 14_600_000);
    assert_eq!(result.token_version, version + 1);
    assert!(result.check_can_reserve(1, NOW_SECS).is_err());
    assert!(engine.unfreeze_card("used-delete").is_err());
    let entries = engine.ledger_entries();
    assert_eq!(
        entries.last().unwrap().operator_id.as_deref(),
        Some("admin")
    );
    engine
        .void_card("used-delete", "admin", "retry", NOW_SECS)
        .unwrap();
    assert_eq!(engine.ledger_entries().len(), entries.len());
}

#[test]
fn activated_card_void_rejects_pending_settlement() {
    let engine = BillingEngine::new();
    let mut card = Card::new("pending-delete", "group-default", 100_000_000);
    card.status = CardStatus::Active;
    card.credit_reserved = 1;
    engine.upsert_card(card);
    assert!(engine
        .void_card("pending-delete", "admin", "remove", NOW_SECS)
        .is_err());
    assert_eq!(
        engine.get_card("pending-delete").unwrap().status,
        CardStatus::Active
    );
    assert!(engine.ledger_entries().is_empty());
}
