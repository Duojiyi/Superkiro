//! Integration and adversarial tests for P1-11: Billing engine, reservation, settlement, and janitor.
//!
//! Spec §5 (Data Model) & Spec §6 (Billing & Metering).

use billing::card::{Card, CardError};
use billing::engine::{BillingEngine, BillingError};
use billing::ledger::UsageTokens;
use billing::reservation::ReservationEstimateParams;
use billing::MICRO_CREDITS_PER_CREDIT;
use std::sync::Arc;
use std::thread;

fn default_estimate_params() -> ReservationEstimateParams {
    ReservationEstimateParams {
        estimated_input_tokens: 2_000,
        max_output_tokens: 4_000,
        input_rate_per_m: 15_000_000,  // 15 credits / 1M
        output_rate_per_m: 60_000_000, // 60 credits / 1M
        credit_multiplier: 1.0,
        margin_multiplier: 1.0,
        model: None,
    }
}

#[test]
fn test_billing_happy_path_reserve_settle_ledger() {
    let engine = BillingEngine::new();
    let card_id = "card-test-01";
    let mut card = Card::new(card_id, "group-pro-plus", 10 * MICRO_CREDITS_PER_CREDIT); // 10 credits
    card.activate(1_000, 3600 * 24 * 30).unwrap();
    engine.upsert_card(card);

    let invocation_id = "inv-happy-path-001";
    let params = default_estimate_params();
    let reserve_needed = params.calculate_reserve_amount(); // 2000*15 + 4000*60 = 30 + 240 = 270 micro-credits -> 270,000 micro-credits
    assert_eq!(reserve_needed, 270_000);

    // 1. Reserve
    let res = engine
        .reserve(card_id, invocation_id, &params, 1_000, 300)
        .expect("Reserve must succeed");
    assert_eq!(res.reserved_micro_credits, 270_000);

    let card_after_res = engine.get_card(card_id).unwrap();
    assert_eq!(card_after_res.credit_reserved, 270_000);
    assert_eq!(
        card_after_res.available_credits(),
        10 * MICRO_CREDITS_PER_CREDIT - 270_000
    );

    // 2. Settle with real usage tokens
    let usage = UsageTokens {
        uncached_input_tokens: 1_000, // 1000 * 15 / 1M = 0.015 credits = 15,000 micro-credits
        output_tokens: 500,           // 500 * 60 / 1M = 0.03 credits = 30,000 micro-credits
        cache_creation_tokens: 0,
        cache_read_tokens: 0,
    };

    let entry = engine
        .settle(
            invocation_id,
            &usage,
            "claude-sonnet-4.5",
            "anthropic",
            "claude-3-7-sonnet-20250219",
            1_005,
        )
        .expect("Settle must succeed");

    assert_eq!(entry.credits_charged, 45_000); // 15,000 + 30,000 = 45,000 micro-credits

    // 3. Verify card balance state
    let card_final = engine.get_card(card_id).unwrap();
    assert_eq!(card_final.credit_used, 45_000);
    assert_eq!(card_final.credit_reserved, 0); // Reservation unfrozen
    assert_eq!(
        card_final.available_credits(),
        10 * MICRO_CREDITS_PER_CREDIT - 45_000
    );

    // 4. Verify ledger
    let entries = engine.ledger_entries();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].invocation_id.as_deref(), Some(invocation_id));
    assert_eq!(entries[0].credits_charged, 45_000);
}

#[test]
fn test_audit_b_idempotent_billing_prevents_double_charge() {
    let engine = BillingEngine::new();
    let card_id = "card-idempotency";
    let mut card = Card::new(card_id, "group-pro-plus", 5 * MICRO_CREDITS_PER_CREDIT);
    card.activate(1_000, 3600).unwrap();
    engine.upsert_card(card);

    let invocation_id = "inv-retry-002";
    let params = default_estimate_params();

    engine
        .reserve(card_id, invocation_id, &params, 1_000, 300)
        .unwrap();

    let usage = UsageTokens {
        uncached_input_tokens: 1_000,
        output_tokens: 200,
        cache_creation_tokens: 0,
        cache_read_tokens: 0,
    };

    // First settlement succeeds
    let entry1 = engine
        .settle(
            invocation_id,
            &usage,
            "claude-sonnet-4.5",
            "anthropic",
            "claude",
            1_010,
        )
        .unwrap();

    // Duplicate settlement must be rejected (Spec §4.7 / §6.7)
    let err = engine
        .settle(
            invocation_id,
            &usage,
            "claude-sonnet-4.5",
            "anthropic",
            "claude",
            1_012,
        )
        .unwrap_err();

    assert_eq!(
        err,
        BillingError::DuplicateInvocation(invocation_id.to_string())
    );

    // Verify card credit_used was not charged twice
    let card = engine.get_card(card_id).unwrap();
    assert_eq!(card.credit_used, entry1.credits_charged);
    assert_eq!(engine.ledger_entries().len(), 1);
}

#[test]
fn test_audit_b_insufficient_credit_rejection() {
    let engine = BillingEngine::new();
    let card_id = "card-broke";
    let mut card = Card::new(card_id, "group-pro-plus", 100_000); // Only 0.1 credits
    card.activate(1_000, 3600).unwrap();
    engine.upsert_card(card);

    let params = default_estimate_params(); // Needs 270,000 micro-credits
    let err = engine
        .reserve(card_id, "inv-overdraft", &params, 1_000, 300)
        .unwrap_err();

    assert_eq!(
        err,
        BillingError::Card(CardError::InsufficientCredit {
            available: 100_000,
            needed: 270_000,
        })
    );

    let card = engine.get_card(card_id).unwrap();
    assert_eq!(card.credit_reserved, 0);
    assert_eq!(card.available_credits(), 100_000);
}

#[test]
fn test_audit_b_concurrent_5_requests_anti_puncture() {
    let engine = Arc::new(BillingEngine::new());
    let card_id = "card-concurrent-pool";
    // Total balance: exactly 3 reservations worth (3 * 270,000 = 810,000)
    let mut card = Card::new(card_id, "group-pro-plus", 810_000);
    card.activate(1_000, 3600).unwrap();
    engine.upsert_card(card);

    let mut handles = Vec::new();

    // Spawn 5 concurrent threads all requesting a 270,000 micro-credit reservation
    for i in 0..5 {
        let engine_clone = Arc::clone(&engine);
        let handle = thread::spawn(move || {
            let inv_id = format!("inv-conc-{}", i);
            let params = default_estimate_params();
            engine_clone.reserve(card_id, &inv_id, &params, 1_000, 300)
        });
        handles.push(handle);
    }

    let mut success_count = 0;
    let mut rejected_count = 0;

    for h in handles {
        match h.join().unwrap() {
            Ok(_) => success_count += 1,
            Err(BillingError::Card(CardError::InsufficientCredit { .. })) => rejected_count += 1,
            Err(other) => panic!("Unexpected error: {:?}", other),
        }
    }

    // Exactly 3 must succeed, exactly 2 must be rejected
    assert_eq!(success_count, 3, "Exactly 3 requests should be accepted");
    assert_eq!(rejected_count, 2, "Exactly 2 requests should be rejected");

    let card = engine.get_card(card_id).unwrap();
    assert_eq!(card.credit_reserved, 810_000);
    assert_eq!(card.available_credits(), 0); // Balance protected, NEVER punctured into negative!
}

#[test]
fn test_audit_b_orphan_reservation_reclaimed_by_janitor() {
    let engine = BillingEngine::new();
    let card_id = "card-orphan-test";
    let mut card = Card::new(card_id, "group-pro-plus", 1_000_000);
    card.activate(1_000, 3600).unwrap();
    engine.upsert_card(card);

    let params = default_estimate_params();
    // Reservation expires after 30 seconds (at t = 1_030)
    engine
        .reserve(card_id, "inv-orphan-01", &params, 1_000, 30)
        .unwrap();

    let card = engine.get_card(card_id).unwrap();
    assert_eq!(card.credit_reserved, 270_000);
    assert_eq!(card.available_credits(), 730_000);

    // At t = 1_015 (before TTL): janitor does not reclaim
    let reclaimed_early = engine.run_janitor(1_015);
    assert_eq!(reclaimed_early, 0);
    assert_eq!(engine.get_card(card_id).unwrap().credit_reserved, 270_000);

    // At t = 1_035 (after TTL): janitor unfreezes orphan reservation
    let reclaimed_late = engine.run_janitor(1_035);
    assert_eq!(reclaimed_late, 1);

    let card_restored = engine.get_card(card_id).unwrap();
    assert_eq!(card_restored.credit_reserved, 0);
    assert_eq!(card_restored.available_credits(), 1_000_000); // 100% recovered!
}

#[test]
fn test_audit_b_cancellation_release_refund() {
    let engine = BillingEngine::new();
    let card_id = "card-cancel";
    let mut card = Card::new(card_id, "group-pro-plus", 1_000_000);
    card.activate(1_000, 3600).unwrap();
    engine.upsert_card(card);

    let invocation_id = "inv-canceled";
    let params = default_estimate_params();
    engine
        .reserve(card_id, invocation_id, &params, 1_000, 300)
        .unwrap();

    assert_eq!(engine.get_card(card_id).unwrap().credit_reserved, 270_000);

    // Client drops connection before output: release reservation
    engine.release(invocation_id).unwrap();

    let card = engine.get_card(card_id).unwrap();
    assert_eq!(card.credit_reserved, 0);
    assert_eq!(card.credit_used, 0);
    assert_eq!(card.available_credits(), 1_000_000);
}

#[test]
fn test_billing_snapshot_save_load_persistence() {
    let engine = BillingEngine::new();
    let card_id = "card-persist-test";
    let mut card = Card::new(card_id, "group-pro-plus", 50 * MICRO_CREDITS_PER_CREDIT);
    card.activate(1_700_000_000, 3600 * 24 * 365).unwrap();
    card.bound_devices.push("fp-dev-device-01".to_string());
    engine.upsert_card(card);

    let temp_dir = std::env::temp_dir().join(format!("kiro_test_{}", std::process::id()));
    let temp_file = temp_dir.join("billing_snapshot.json");

    // 1. Save to file
    engine
        .save_to_file(&temp_file)
        .expect("save_to_file must succeed");
    assert!(temp_file.exists());

    // 2. Load into a fresh new BillingEngine
    let restored_engine = BillingEngine::new();
    assert!(restored_engine.get_card(card_id).is_none());

    restored_engine
        .load_from_file(&temp_file)
        .expect("load_from_file must succeed");

    // 3. Verify card and its state survived 100%
    let restored_card = restored_engine
        .get_card(card_id)
        .expect("Card must be restored");
    assert_eq!(restored_card.id, card_id);
    assert_eq!(
        restored_card.available_credits(),
        50 * MICRO_CREDITS_PER_CREDIT
    );
    assert_eq!(
        restored_card.bound_devices,
        vec!["fp-dev-device-01".to_string()]
    );
    assert_eq!(restored_card.status, billing::card::CardStatus::Active);

    // Clean up
    let _ = std::fs::remove_file(temp_file);
    let _ = std::fs::remove_dir_all(temp_dir);
}
