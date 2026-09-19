use billing::{
    BillingEngine, Card, CardStatus, RequestTrace, ReservationEstimateParams, TraceStatus,
    UsageTokens,
};

fn setup() -> BillingEngine {
    let engine = BillingEngine::new();
    for id in ["card", "other"] {
        let mut card = Card::new(id, "group-pro-plus", 1_000_000_000);
        card.status = CardStatus::Active;
        engine.upsert_card(card);
    }
    engine
}

fn settle(engine: &BillingEngine, card: &str, invocation: &str, model: &str, ts: u64) -> i64 {
    engine
        .reserve(
            card,
            invocation,
            &ReservationEstimateParams::new(1, 2),
            ts,
            660,
        )
        .unwrap();
    engine
        .settle(
            invocation,
            &UsageTokens {
                uncached_input_tokens: 1,
                output_tokens: 2,
                cache_creation_tokens: 3,
                cache_read_tokens: 4,
            },
            model,
            "private-provider",
            "private-model",
            ts,
        )
        .unwrap()
        .credits_charged
}

#[test]
fn aggregates_only_committed_usage_in_utc_window_without_retry_or_cache_double_counting() {
    let engine = setup();
    let today = 40 * 86_400;
    let now = today + 100;
    let start = 11 * 86_400;
    settle(&engine, "card", "old", "model-one", start - 1);
    let charge = settle(&engine, "card", "boundary", "model-one", start);
    settle(&engine, "card", "yesterday", "model-two", today - 1);
    settle(&engine, "card", "today", "model-one", today);
    // Rejected replay does not add tokens/points, even if caller supplies other data.
    let replay = engine.settle(
        "today",
        &UsageTokens {
            output_tokens: 999,
            ..Default::default()
        },
        "wrong",
        "retry",
        "retry",
        now,
    );
    assert!(matches!(
        replay,
        Err(billing::BillingError::DuplicateInvocation(_))
    ));
    settle(&engine, "card", "future", "model-one", now + 1);
    settle(&engine, "other", "other-card", "secret-other-model", now);
    engine
        .adjust_balance("card", 1_000_000, "admin", "not usage", now)
        .unwrap();
    engine
        .reserve(
            "card",
            "held",
            &ReservationEstimateParams::new(1, 2),
            now,
            660,
        )
        .unwrap();
    engine.record_trace(RequestTrace {
        id: "failed-attempt".into(),
        card_id: "card".into(),
        ts: now,
        invocation_id: "held".into(),
        exposed_model: "failed-model".into(),
        status: TraceStatus::Error,
        ttft_ms: None,
        tokens_per_second: None,
        error_class: None,
        provider_id: None,
        input_tokens: 999,
        output_tokens: 999,
        credits_charged: 999,
        provider_cost_micro_cny: 999,
        attempt_chain: vec![],
    });
    let stats = engine.settled_usage("card", now).unwrap();
    assert_eq!(stats.total_tokens, 30); // (1 uncached + 3 creation + 4 cached + 2 output) * 3
    assert_eq!(stats.today_tokens, 10);
    assert_eq!(stats.today_points, charge as f64 / 1_000_000.0);
    assert_eq!(stats.window_start, start);
    assert_eq!(stats.window_end, now + 1);
    assert_eq!(stats.timezone, "UTC");
    assert_eq!(stats.daily.len(), 30);
    assert_eq!(stats.daily[0].date, "1970-01-12");
    assert_eq!(stats.daily[1].tokens, 0);
    assert_eq!(stats.daily.last().unwrap().date, "1970-02-10");
    assert_eq!(stats.models.len(), 2);
    assert_eq!(stats.models[0].name, "model-one");
    assert_eq!(stats.models[0].tokens, 20);
    assert_eq!(stats.models[0].points, (charge * 2) as f64 / 1_000_000.0);
    let json = serde_json::to_string(&stats).unwrap();
    for forbidden in [
        "provider",
        "cost",
        "usd",
        "referencePrice",
        "secret-other",
        "failed-model",
    ] {
        assert!(!json.contains(forbidden));
    }

    let archive_dir = std::env::temp_dir().join(format!(
        "settled-usage-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    engine.archive_ledger(start, &archive_dir).unwrap();
    assert_eq!(engine.settled_usage("card", now).unwrap(), stats); // older archive is outside the window
    engine.archive_ledger(today + 1, &archive_dir).unwrap();
    assert!(engine.settled_usage("card", now).is_none()); // never guess missing archived tokens
}

#[test]
fn empty_usage_is_real_zero_and_unknown_card_is_unavailable() {
    let engine = setup();
    let stats = engine.settled_usage("card", 100).unwrap();
    assert_eq!(stats.total_tokens, 0);
    assert_eq!(stats.today_points, 0.0);
    assert!(stats.daily.is_empty());
    assert!(stats.models.is_empty());
    assert!(engine.settled_usage("missing", 100).is_none());
}
