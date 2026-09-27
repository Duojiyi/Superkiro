use billing::card::{Card, CardStatus};
use billing::engine::BillingEngine;
use billing::ledger::UsageTokens;
use billing::observability::{
    Announcement, AnnouncementLevel, AnomalyAction, AttemptRecord, RequestTrace, TraceStatus,
};
use billing::rate_card::BillingSettings;
use billing::reservation::ReservationEstimateParams;

#[test]
fn test_record_and_list_request_traces() {
    let engine = BillingEngine::new();

    let trace1 = RequestTrace {
        id: "tr-1".to_string(),
        card_id: "card-user-1".to_string(),
        ts: 1000,
        invocation_id: "inv-1".to_string(),
        exposed_model: "claude-3-5-sonnet".to_string(),
        status: TraceStatus::Success,
        ttft_ms: Some(250),
        tokens_per_second: Some(48.5),
        error_class: None,
        provider_id: Some("deepseek-direct".to_string()),
        input_tokens: 1200,
        output_tokens: 350,
        credits_charged: 25_000_000,
        provider_cost_micro_cny: 15_000,
        needed_micro_credits: None,
        available_micro_credits: None,
        attempt_chain: vec![AttemptRecord {
            key_id: "k-1".to_string(),
            provider_id: "deepseek-direct".to_string(),
            success: true,
            error: None,
            latency_ms: 1200,
        }],
        repeats: 0,
        last_seen_secs: None,
    };

    let trace2 = RequestTrace {
        id: "tr-2".to_string(),
        card_id: "card-user-2".to_string(),
        ts: 1010,
        invocation_id: "inv-2".to_string(),
        exposed_model: "deepseek-chat".to_string(),
        status: TraceStatus::Error,
        ttft_ms: None,
        tokens_per_second: None,
        error_class: Some("ProviderTimeout".to_string()),
        provider_id: Some("deepseek-direct".to_string()),
        input_tokens: 500,
        output_tokens: 0,
        credits_charged: 0,
        provider_cost_micro_cny: 0,
        needed_micro_credits: None,
        available_micro_credits: None,
        attempt_chain: vec![AttemptRecord {
            key_id: "k-2".to_string(),
            provider_id: "deepseek-direct".to_string(),
            success: false,
            error: Some("Timeout".to_string()),
            latency_ms: 5000,
        }],
        repeats: 0,
        last_seen_secs: None,
    };

    engine.record_trace(trace1);
    engine.record_trace(trace2);

    let all_traces = engine.list_traces(None, 10);
    assert_eq!(all_traces.len(), 2);

    let user1_traces = engine.list_traces(Some("card-user-1"), 10);
    assert_eq!(user1_traces.len(), 1);
    assert_eq!(user1_traces[0].id, "tr-1");
    assert_eq!(user1_traces[0].attempt_chain.len(), 1);
}

#[test]
fn test_daily_usage_aggregation_and_multi_day_reports() {
    let engine = BillingEngine::new();
    let mut card = Card::new("card-a", "group-pro-plus", 100_000_000);
    card.status = CardStatus::Active;
    engine.upsert_card(card);

    let params = ReservationEstimateParams::new(1000, 500);

    // Day 1 (ts = 1000)
    let _ = engine.reserve("card-a", "inv-d1", &params, 1000, 60);
    let tokens_d1 = UsageTokens {
        uncached_input_tokens: 1000,
        output_tokens: 500,
        cache_creation_tokens: 0,
        cache_read_tokens: 0,
    };
    let _ = engine.settle(
        "inv-d1",
        &tokens_d1,
        "gpt-4o",
        "openai-prov",
        "gpt-4o",
        1005,
    );

    // Day 2 (ts = 1000 + 86400 = 87400)
    let _ = engine.reserve("card-a", "inv-d2", &params, 87400, 60);
    let tokens_d2 = UsageTokens {
        uncached_input_tokens: 2000,
        output_tokens: 800,
        cache_creation_tokens: 0,
        cache_read_tokens: 0,
    };
    let _ = engine.settle(
        "inv-d2",
        &tokens_d2,
        "gpt-4o",
        "openai-prov",
        "gpt-4o",
        87405,
    );

    let daily_summaries = engine.get_daily_summary(None, None);
    assert_eq!(daily_summaries.len(), 2);
    assert_eq!(daily_summaries[0].requests_count, 1);
    assert_eq!(daily_summaries[0].input_tokens, 1000);
    assert_eq!(daily_summaries[0].output_tokens, 500);

    assert_eq!(daily_summaries[1].requests_count, 1);
    assert_eq!(daily_summaries[1].input_tokens, 2000);
    assert_eq!(daily_summaries[1].output_tokens, 800);
}

#[test]
fn test_cost_vs_revenue_gross_margin_dashboard() {
    let engine = BillingEngine::new();
    let mut card = Card::new("card-fin", "group-pro-plus", 200_000_000);
    card.status = CardStatus::Active;
    engine.upsert_card(card);

    // Set face value: 1 credit = 0.01 CNY (1 credit = 1_000_000 micro-credits)
    let settings = BillingSettings {
        credit_face_value_cny: 0.01,
        usd_cny_rate: 7.25,
        rate_updated_at_secs: 1000,
        ..BillingSettings::default()
    };
    engine.update_settings(settings);

    let params = ReservationEstimateParams::new(5000, 1000);
    let _ = engine.reserve("card-fin", "inv-fin", &params, 1000, 60);
    let tokens = UsageTokens {
        uncached_input_tokens: 5000,
        output_tokens: 1000,
        cache_creation_tokens: 0,
        cache_read_tokens: 0,
    };
    let _ = engine.settle(
        "inv-fin",
        &tokens,
        "claude-sonnet-4.5",
        "anthropic-pool",
        "claude-3-5",
        1005,
    );

    let margin = engine.get_margin_dashboard(None, None);
    assert_eq!(margin.total_requests, 1);
    assert!(margin.total_credits_charged > 0);
    assert!(margin.revenue_micro_cny > 0);
    // Profit = Revenue - Provider Cost
    assert_eq!(
        margin.gross_profit_micro_cny,
        margin.revenue_micro_cny - margin.provider_cost_micro_cny
    );
    assert!(margin.gross_margin_percentage >= 0.0);
}

#[test]
fn test_model_cost_rankings() {
    let engine = BillingEngine::new();
    let mut card = Card::new("card-rank", "group-pro-plus", 200_000_000);
    card.status = CardStatus::Active;
    engine.upsert_card(card);

    let params = ReservationEstimateParams::new(2000, 500);

    let _ = engine.reserve("card-rank", "inv-r1", &params, 1000, 60);
    let _ = engine.settle(
        "inv-r1",
        &UsageTokens {
            uncached_input_tokens: 1000,
            output_tokens: 200,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
        },
        "expensive-model",
        "p1",
        "exp-1",
        1005,
    );

    let _ = engine.reserve("card-rank", "inv-r2", &params, 1010, 60);
    let _ = engine.settle(
        "inv-r2",
        &UsageTokens {
            uncached_input_tokens: 2000,
            output_tokens: 400,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
        },
        "cheaper-model",
        "p1",
        "cheap-1",
        1015,
    );

    let rankings = engine.get_model_cost_rankings(None, None);
    assert_eq!(rankings.len(), 2);
    let models: Vec<String> = rankings.iter().map(|r| r.model_id.clone()).collect();
    assert!(models.contains(&"expensive-model".to_string()));
    assert!(models.contains(&"cheaper-model".to_string()));
}

#[test]
fn test_provider_health_summary_metrics() {
    let engine = BillingEngine::new();

    // 2 successes, 1 error for prov-alpha
    engine.record_trace(RequestTrace {
        id: "t1".to_string(),
        card_id: "c".to_string(),
        ts: 100,
        invocation_id: "i1".to_string(),
        exposed_model: "m".to_string(),
        status: TraceStatus::Success,
        ttft_ms: Some(200),
        tokens_per_second: Some(50.0),
        error_class: None,
        provider_id: Some("prov-alpha".to_string()),
        input_tokens: 100,
        output_tokens: 50,
        credits_charged: 1000,
        provider_cost_micro_cny: 500,
        needed_micro_credits: None,
        available_micro_credits: None,
        attempt_chain: vec![],
        repeats: 0,
        last_seen_secs: None,
    });

    engine.record_trace(RequestTrace {
        id: "t2".to_string(),
        card_id: "c".to_string(),
        ts: 110,
        invocation_id: "i2".to_string(),
        exposed_model: "m".to_string(),
        status: TraceStatus::Success,
        ttft_ms: Some(400),
        tokens_per_second: Some(30.0),
        error_class: None,
        provider_id: Some("prov-alpha".to_string()),
        input_tokens: 100,
        output_tokens: 50,
        credits_charged: 1000,
        provider_cost_micro_cny: 500,
        needed_micro_credits: None,
        available_micro_credits: None,
        attempt_chain: vec![],
        repeats: 0,
        last_seen_secs: None,
    });

    engine.record_trace(RequestTrace {
        id: "t3".to_string(),
        card_id: "c".to_string(),
        ts: 120,
        invocation_id: "i3".to_string(),
        exposed_model: "m".to_string(),
        status: TraceStatus::Error,
        ttft_ms: None,
        tokens_per_second: None,
        error_class: Some("InternalError".to_string()),
        provider_id: Some("prov-alpha".to_string()),
        input_tokens: 100,
        output_tokens: 0,
        credits_charged: 0,
        provider_cost_micro_cny: 0,
        needed_micro_credits: None,
        available_micro_credits: None,
        attempt_chain: vec![],
        repeats: 0,
        last_seen_secs: None,
    });

    let health = engine.get_provider_health("prov-alpha");
    assert_eq!(health.total_requests, 3);
    assert_eq!(health.success_requests, 2);
    assert_eq!(health.error_requests, 1);
    assert!((health.success_rate - 66.666).abs() < 0.1);
    assert_eq!(health.avg_ttft_ms, Some(300.0)); // (200 + 400) / 2
    assert_eq!(health.avg_tokens_per_second, Some(40.0)); // (50 + 30) / 2
}

#[test]
fn test_usage_anomaly_detection_and_auto_freeze() {
    let engine = BillingEngine::new();
    let mut card = Card::new("card-spike", "group-pro-plus", 500_000_000);
    card.status = CardStatus::Active;
    engine.upsert_card(card);

    let now = 10_000;
    let params = ReservationEstimateParams::new(100_000, 50_000);
    // Massive usage spike: 100 credits in 1 minute
    engine
        .reserve("card-spike", "inv-spike", &params, now, 60)
        .unwrap();
    let _res = engine
        .settle(
            "inv-spike",
            &UsageTokens {
                uncached_input_tokens: 100_000,
                output_tokens: 50_000,
                cache_creation_tokens: 0,
                cache_read_tokens: 0,
            },
            "claude-sonnet-4.5",
            "p1",
            "claude-3-5",
            now + 5,
        )
        .unwrap();

    // Threshold = 4_000_000 micro-credits within 300 seconds
    let alert = engine.evaluate_card_spike(
        "card-spike",
        now + 10,
        300,
        4_000_000,
        AnomalyAction::AutoFrozen,
    );

    assert!(alert.is_some());
    let a = alert.unwrap();
    assert_eq!(a.card_id, "card-spike");
    assert_eq!(a.action_taken, AnomalyAction::AutoFrozen);

    // Verify card was automatically frozen
    let card = engine.get_card("card-spike").unwrap();
    assert_eq!(card.status, CardStatus::Frozen);
}

#[test]
fn test_announcements_downlink_and_expiry() {
    let engine = BillingEngine::new();

    let ann1 = Announcement::new(
        "ann-1",
        "Scheduled Maintenance",
        "Database maintenance scheduled tonight at 02:00 UTC",
        AnnouncementLevel::Info,
        1000,
    )
    .with_expiry(2000);

    let ann2 = Announcement::new(
        "ann-2",
        "Claude Upstream Degraded",
        "Anthropic API experiencing elevated error rates",
        AnnouncementLevel::Warning,
        1000,
    ); // No expiry

    engine.add_announcement(ann1);
    engine.add_announcement(ann2);

    // At t=1500, both are active
    let active_1500 = engine.list_active_announcements(1500);
    assert_eq!(active_1500.len(), 2);

    // At t=2500, ann-1 has expired
    let active_2500 = engine.list_active_announcements(2500);
    assert_eq!(active_2500.len(), 1);
    assert_eq!(active_2500[0].id, "ann-2");
}

#[test]
fn test_financial_reconciliation_export_csv_and_json() {
    let engine = BillingEngine::new();
    let mut card = Card::new("card-exp", "group-pro-plus", 100_000_000);
    card.status = CardStatus::Active;
    engine.upsert_card(card);

    let params = ReservationEstimateParams::new(500, 100);
    let _ = engine.reserve("card-exp", "inv-exp", &params, 1000, 60);
    let _ = engine.settle(
        "inv-exp",
        &UsageTokens {
            uncached_input_tokens: 500,
            output_tokens: 100,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
        },
        "gpt-4o",
        "prov-1",
        "gpt-4o",
        1005,
    );

    let csv = engine.export_ledger_csv(Some("card-exp"));
    assert!(csv.contains("id,card_id,ts,kind"));
    assert!(csv.contains("card-exp"));
    assert!(csv.contains("gpt-4o"));

    let json = engine.export_ledger_json(Some("card-exp")).unwrap();
    assert!(json.contains("card-exp"));
    assert!(json.contains("inv-exp"));
}

/// RFC 4180 cells of each line: quoted cells may hold commas and doubled quotes.
fn csv_rows(csv: &str) -> Vec<Vec<String>> {
    csv.lines()
        .map(|line| {
            let (mut cells, mut cell, mut quoted, mut chars) =
                (Vec::new(), String::new(), false, line.chars().peekable());
            while let Some(c) = chars.next() {
                match (c, quoted) {
                    ('"', true) if chars.peek() == Some(&'"') => {
                        cell.push('"');
                        chars.next();
                    }
                    ('"', _) => quoted = !quoted,
                    (',', false) => cells.push(std::mem::take(&mut cell)),
                    _ => cell.push(c),
                }
            }
            cells.push(cell);
            cells
        })
        .collect()
}

#[test]
fn a_client_supplied_field_can_neither_split_a_row_nor_run_as_a_formula() {
    let engine = BillingEngine::new();
    let mut card = Card::new("card-csv", "group-pro-plus", 100_000_000);
    card.status = CardStatus::Active;
    engine.upsert_card(card);
    let hostile = "=HYPERLINK(\"https://evil.example/?\"&A1,\"open\"),0,0,0";
    let params = ReservationEstimateParams::new(500, 100);
    engine
        .reserve("card-csv", hostile, &params, 1000, 60)
        .unwrap();
    engine
        .settle(
            hostile,
            &UsageTokens {
                uncached_input_tokens: 500,
                output_tokens: 100,
                cache_creation_tokens: 0,
                cache_read_tokens: 0,
            },
            "@model",
            "prov-1",
            "gpt-4o",
            1005,
        )
        .unwrap();

    let rows = csv_rows(&engine.export_ledger_csv(Some("card-csv")));
    assert!(rows.len() >= 2);
    for row in &rows {
        assert_eq!(row.len(), rows[0].len(), "{row:?}");
        for cell in row {
            assert!(
                !cell.starts_with(['=', '+', '@']),
                "{cell:?} would run as a formula"
            );
        }
    }
    let usage = rows
        .iter()
        .find(|row| row[4].contains("HYPERLINK"))
        .unwrap();
    assert!(
        usage[4].ends_with(hostile),
        "the id itself is kept: {:?}",
        usage[4]
    );
    // Numbers stay numbers for reconciliation.
    assert!(usage[7].parse::<u64>().is_ok() && usage[9].parse::<i64>().is_ok());
}

#[test]
fn test_data_retention_pruning_policy() {
    let engine = BillingEngine::new();

    let make_trace = |id: &str, ts: u64| RequestTrace {
        id: id.to_string(),
        card_id: "c".to_string(),
        ts,
        invocation_id: id.to_string(),
        exposed_model: "m".to_string(),
        status: TraceStatus::Success,
        ttft_ms: None,
        tokens_per_second: None,
        error_class: None,
        provider_id: None,
        input_tokens: 0,
        output_tokens: 0,
        credits_charged: 0,
        provider_cost_micro_cny: 0,
        needed_micro_credits: None,
        available_micro_credits: None,
        attempt_chain: vec![],
        repeats: 0,
        last_seen_secs: None,
    };

    engine.record_trace(make_trace("t1", 1000));
    engine.record_trace(make_trace("t2", 2000));
    engine.record_trace(make_trace("t3", 3000));

    assert_eq!(engine.list_traces(None, 10).len(), 3);

    // Prune records older than cutoff 2500 (removes t1 at 1000 and t2 at 2000)
    let pruned = engine.prune_traces(2500);
    assert_eq!(pruned, 2);

    let remaining = engine.list_traces(None, 10);
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].id, "t3");
}

#[test]
fn retry_history_survives_single_settlement_and_final_delivery_error() {
    let engine = BillingEngine::new();
    let mut card = Card::new("retry-card", "group-pro-plus", 100_000_000);
    card.status = CardStatus::Active;
    engine.upsert_card(card);
    engine
        .reserve(
            "retry-card",
            "retry-inv",
            &ReservationEstimateParams::new(1000, 500),
            1000,
            60,
        )
        .unwrap();
    for (index, success) in [false, true].into_iter().enumerate() {
        engine.record_trace(RequestTrace {
            id: format!("attempt-{index}"),
            card_id: "retry-card".into(),
            ts: 1000,
            invocation_id: "retry-inv".into(),
            exposed_model: "gpt-4o".into(),
            status: if success {
                TraceStatus::InProgress
            } else {
                TraceStatus::Error
            },
            ttft_ms: None,
            tokens_per_second: None,
            error_class: None,
            provider_id: Some("provider".into()),
            input_tokens: 0,
            output_tokens: 0,
            credits_charged: 0,
            provider_cost_micro_cny: 0,
            needed_micro_credits: None,
            available_micro_credits: None,
            attempt_chain: vec![AttemptRecord {
                key_id: format!("key-{index}"),
                provider_id: "provider".into(),
                success,
                error: if success {
                    None
                } else {
                    Some("http_503".into())
                },
                latency_ms: 10,
            }],
            repeats: 0,
            last_seen_secs: None,
        });
    }
    assert_eq!(engine.invocation_attempts("retry-inv"), 2);
    let tokens = UsageTokens {
        uncached_input_tokens: 10,
        output_tokens: 5,
        cache_creation_tokens: 0,
        cache_read_tokens: 0,
    };
    engine
        .settle("retry-inv", &tokens, "gpt-4o", "provider", "gpt-4o", 1005)
        .unwrap();
    let balance = engine.get_card("retry-card").unwrap().available_credits();
    let _ = engine.settle("retry-inv", &tokens, "gpt-4o", "provider", "gpt-4o", 1006);
    assert_eq!(
        engine.get_card("retry-card").unwrap().available_credits(),
        balance
    );
    assert_eq!(engine.invocation_attempts("retry-inv"), 2);
    engine.finish_trace("retry-inv", TraceStatus::Error, Some("stream_incomplete"));
    let traces = engine.list_traces(Some("retry-card"), 10);
    assert_eq!(traces.len(), 1);
    assert_eq!(traces[0].attempt_chain.len(), 2);
    assert_eq!(traces[0].status, TraceStatus::Error);
    assert!(traces[0].credits_charged > 0);
}

#[test]
fn reports_saturate_instead_of_wrapping_on_extreme_entries() {
    use billing::ledger::{LedgerEntry, LedgerKind};
    use billing::observability::{compute_margin_dashboard, compute_model_cost_rankings};
    let entry = |id: &str| LedgerEntry {
        id: id.into(),
        card_id: "card".into(),
        kind: LedgerKind::Usage,
        invocation_id: None,
        exposed_model: "model".into(),
        provider_id: "provider".into(),
        target_model: "target".into(),
        input_tokens: u64::MAX,
        output_tokens: u64::MAX,
        cache_creation_tokens: 0,
        cache_read_tokens: 0,
        credits_charged: i64::MAX,
        provider_cost_micro_cny: i64::MAX,
        rate_card_version: None,
        ts_secs: 1,
        operator_id: None,
        reason: None,
        credit_face_value_cny: None,
        detail: None,
    };
    let entries = [entry("a"), entry("b")];
    let settings = BillingSettings {
        credit_face_value_cny: 0.01,
        usd_cny_rate: 7.25,
        rate_updated_at_secs: 1,
        ..BillingSettings::default()
    };

    let margin = compute_margin_dashboard(&entries, &settings);
    assert_eq!(margin.total_credits_charged, i64::MAX);
    assert_eq!(margin.provider_cost_micro_cny, i64::MAX);
    assert!(
        margin.gross_profit_micro_cny <= 0,
        "a loss, not a wrapped profit"
    );

    let rankings = compute_model_cost_rankings(&entries, &settings);
    assert_eq!(rankings.len(), 1);
    assert_eq!(rankings[0].total_tokens, u64::MAX);
    assert_eq!(rankings[0].provider_cost_micro_cny, i64::MAX);
}

#[test]
fn traces_ride_along_with_the_next_commit_instead_of_saving_on_their_own() {
    let dir = std::env::temp_dir().join(format!(
        "kiro-trace-commits-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("billing_state.json");
    let engine = BillingEngine::new();
    engine.set_persistence_path(&path);
    let mut card = Card::new("card-trace", "group", 100_000_000);
    card.status = CardStatus::Active;
    engine.upsert_card(card);

    let saves = engine.snapshot_sequence();
    engine.record_trace(RequestTrace {
        id: "trace-1".into(),
        card_id: "card-trace".into(),
        ts: 1000,
        invocation_id: "inv-trace".into(),
        exposed_model: "model".into(),
        status: TraceStatus::InProgress,
        ttft_ms: None,
        tokens_per_second: None,
        error_class: None,
        provider_id: None,
        input_tokens: 0,
        output_tokens: 0,
        credits_charged: 0,
        provider_cost_micro_cny: 0,
        needed_micro_credits: None,
        available_micro_credits: None,
        attempt_chain: Vec::new(),
        repeats: 0,
        last_seen_secs: None,
    });
    engine.finish_trace("inv-trace", TraceStatus::Success, None);
    assert_eq!(
        engine.snapshot_sequence(),
        saves,
        "a trace costs no save of its own"
    );

    // The next commit carries it: here the next request's settlement.
    engine
        .reserve(
            "card-trace",
            "inv-next",
            &ReservationEstimateParams::new(10, 10),
            1000,
            60,
        )
        .unwrap();
    engine
        .settle(
            "inv-next",
            &billing::ledger::UsageTokens::default(),
            "model",
            "provider",
            "model",
            1001,
        )
        .unwrap();
    let restored = BillingEngine::new();
    restored.load_from_file(&path).unwrap();
    let traces = restored.list_traces(None, 10);
    assert!(traces
        .iter()
        .any(|t| t.invocation_id == "inv-trace" && t.status == TraceStatus::Success));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn activity_counts_real_totals_by_period_and_hour() {
    let engine = BillingEngine::new();
    let mut card = Card::new("card-activity", "group", 100_000_000);
    card.status = CardStatus::Active;
    engine.upsert_card(card);
    // Fifty minutes into an hour, so each moment falls in a known hourly bucket.
    let now = 240 * 3600 + 3000;
    let tokens = UsageTokens {
        uncached_input_tokens: 1000,
        output_tokens: 500,
        cache_creation_tokens: 0,
        cache_read_tokens: 200,
    };
    for (invocation, at) in [
        ("inv-hour-ago", now - 3600),
        ("inv-two-days", now - 2 * 86400),
        ("inv-eight-days", now - 8 * 86400),
    ] {
        engine
            .reserve(
                "card-activity",
                invocation,
                &ReservationEstimateParams::new(1000, 500),
                at - 5,
                60,
            )
            .unwrap();
        engine
            .settle(
                invocation,
                &tokens,
                "claude-opus-5",
                "provider",
                "target",
                at,
            )
            .unwrap();
    }
    let trace = |invocation: &str, ts: u64, status: TraceStatus| RequestTrace {
        id: format!("trace-{invocation}"),
        card_id: "card-activity".into(),
        ts,
        invocation_id: invocation.into(),
        exposed_model: "claude-opus-5".into(),
        status,
        ttft_ms: None,
        tokens_per_second: None,
        error_class: None,
        provider_id: None,
        input_tokens: 0,
        output_tokens: 0,
        credits_charged: 0,
        provider_cost_micro_cny: 0,
        needed_micro_credits: None,
        available_micro_credits: None,
        attempt_chain: Vec::new(),
        repeats: 0,
        last_seen_secs: None,
    };
    engine.note_trace_timing("inv-hour-ago", Some(800), Some(40.0));
    engine.note_trace_timing("inv-two-days", Some(2000), Some(20.0));
    engine.record_trace(RequestTrace {
        provider_id: Some("provider-b".into()),
        ..trace("inv-failed", now - 1800, TraceStatus::Error)
    });
    engine.record_trace(RequestTrace {
        provider_id: Some("provider".into()),
        ttft_ms: Some(1200),
        ..trace("inv-left", now - 60, TraceStatus::ClientAborted)
    });
    engine.record_trace(RequestTrace {
        provider_id: Some("provider".into()),
        ttft_ms: Some(1),
        ..trace("inv-running", now - 10, TraceStatus::InProgress)
    });

    let activity = engine.activity(now);
    let day = &activity.last_24h;
    assert_eq!(
        (day.requests, day.succeeded, day.failed, day.client_aborted),
        (3, 1, 1, 1)
    );
    assert_eq!(day.input_tokens, 1200);
    assert_eq!(day.output_tokens, 500);
    assert_eq!(day.active_cards, 1);
    assert!(day.credits_charged > 0);
    let week = &activity.last_7d;
    assert_eq!((week.requests, week.succeeded), (4, 2));
    assert_eq!(week.credits_charged, 2 * day.credits_charged);
    assert_eq!(activity.hourly.len(), 24);
    assert_eq!(activity.hourly[23].start_secs, 240 * 3600);
    assert_eq!(
        (activity.hourly[23].requests, activity.hourly[23].failed),
        (2, 1)
    );
    assert_eq!(
        (activity.hourly[22].requests, activity.hourly[22].failed),
        (1, 0)
    );
    assert_eq!(activity.hourly.iter().map(|h| h.requests).sum::<u64>(), 3);
    assert_eq!(activity.traces_cover_from_secs, Some(now - 8 * 86400));
    // Time to first output over finished, timed requests; the running one is not counted.
    assert_eq!(
        (day.timed_requests, day.ttft_median_ms, day.ttft_p90_ms),
        (2, Some(800), Some(1200))
    );
    assert_eq!(
        (week.timed_requests, week.ttft_median_ms, week.ttft_p90_ms),
        (3, Some(1200), Some(2000))
    );
    let providers: Vec<_> = activity
        .providers
        .iter()
        .map(|p| {
            (
                p.provider_id.as_str(),
                p.requests,
                p.failed,
                p.ttft_median_ms,
            )
        })
        .collect();
    assert_eq!(
        providers,
        [("provider", 2, 0, Some(800)), ("provider-b", 1, 1, None)]
    );
    // As the stats endpoint sends it.
    let json = serde_json::to_value(&activity).unwrap();
    assert_eq!(json["last24h"]["clientAborted"], 1);
    assert_eq!(json["hourly"][23]["startSecs"], 240 * 3600);
    assert_eq!(json["tracesCoverFromSecs"], now - 8 * 86400);
    assert_eq!(json["last24h"]["ttftMedianMs"], 800);
    assert_eq!(json["providers"][0]["providerId"], "provider");
}

/// Revenue counts each request's credits at the face value they were sold at, so changing the
/// face value restates no past revenue. An entry settled before the face value was recorded
/// counts at the current one, and while it never changes, revenue is as it always was.
#[test]
fn revenue_keeps_the_face_value_it_was_earned_at() {
    use billing::ledger::LedgerEntry;
    use billing::observability::compute_margin_dashboard;
    use billing::rate_card::{Currency, PricingMode, RateCardVersion};
    let credit = billing::MICRO_CREDITS_PER_CREDIT;
    let engine = BillingEngine::new();
    let mut card = Card::new("card-face", "group-pro-plus", 1_000 * credit);
    card.status = CardStatus::Active;
    engine.upsert_card(card);
    // 10 credits and 0.1 CNY of cost per million output tokens.
    engine.upsert_rate_card_version(RateCardVersion {
        id: "m-fixed".into(),
        rate_card_id: "default".into(),
        model: "m".into(),
        currency: Currency::Cny,
        pricing_mode: PricingMode::Fixed,
        input_price_per_m: 0.0,
        output_price_per_m: 0.1,
        cache_creation_price_per_m: 0.0,
        cache_read_price_per_m: 0.0,
        fixed_input_credit_per_m: 0,
        fixed_output_credit_per_m: 10 * credit,
        fixed_cache_creation_credit_per_m: 0,
        fixed_cache_read_credit_per_m: 0,
        per_call_credit: 0,
        margin_multiplier: 1.0,
        effective_from_secs: 0,
        official: None,
    });
    let at_face_value = |face: f64| {
        engine.update_settings(BillingSettings {
            credit_face_value_cny: face,
            ..BillingSettings::default()
        })
    };
    let settle = |id: &str, now: u64| {
        let params = ReservationEstimateParams::new(0, 1_000_000).with_model("m");
        engine.reserve("card-face", id, &params, now, 60).unwrap();
        let tokens = UsageTokens {
            output_tokens: 1_000_000,
            ..UsageTokens::default()
        };
        engine.settle(id, &tokens, "m", "p", "m", now + 1).unwrap()
    };

    at_face_value(0.03);
    let first = settle("inv-1", 100);
    assert_eq!(first.credit_face_value_cny, Some(0.03));
    // 10 credits at 0.03 CNY.
    assert_eq!(
        engine.get_margin_dashboard(None, None).revenue_micro_cny,
        300_000
    );

    at_face_value(0.05);
    let second = settle("inv-2", 200);
    assert_eq!(second.credit_face_value_cny, Some(0.05));
    // Still 0.3 CNY for the first, and 10 credits at 0.05 CNY.
    let dashboard = engine.get_margin_dashboard(None, None);
    assert_eq!(dashboard.revenue_micro_cny, 800_000);
    assert_eq!(dashboard.gross_profit_micro_cny, 600_000);
    assert_eq!(engine.get_margin_summary().total_revenue_micro_cny, 800_000);
    let rankings = engine.get_model_cost_rankings(None, None);
    assert_eq!(rankings[0].margin_percentage, 75.0);
    let version = engine.get_rate_card_version("m-fixed").unwrap();
    let simulated = engine.simulate_candidate_pricing(&version, None, 300, 30.0);
    assert_eq!(simulated.original_revenue_micro_cny, 800_000);
    // The same prices sold today, at today's face value.
    assert_eq!(simulated.simulated_revenue_micro_cny, 1_000_000);

    // An entry from before the face value was recorded counts at the current one.
    let mut older = serde_json::to_value(&first).unwrap();
    older
        .as_object_mut()
        .unwrap()
        .remove("credit_face_value_cny");
    let older: LedgerEntry = serde_json::from_value(older).unwrap();
    assert_eq!(older.credit_face_value_cny, None);
    let settings = engine.get_settings();
    let revenue = compute_margin_dashboard(&[older.clone(), second], &settings).revenue_micro_cny;
    assert_eq!(revenue, 1_000_000);
    // Saved without it, as before; and an older release ignores it, as any unknown field.
    assert!(serde_json::to_value(&older)
        .unwrap()
        .get("credit_face_value_cny")
        .is_none());
    let mut later = serde_json::to_value(&first).unwrap();
    later["added_later"] = serde_json::json!(true);
    let later: LedgerEntry = serde_json::from_value(later).unwrap();
    assert_eq!(later.credit_face_value_cny, Some(0.03));
}

fn attempt(provider: &str, key: &str, error: Option<&str>) -> AttemptRecord {
    AttemptRecord {
        key_id: key.into(),
        provider_id: provider.into(),
        success: error.is_none(),
        error: error.map(str::to_string),
        latency_ms: 10,
    }
}

/// Counts every attempt of every request, not only the provider that answered: a primary
/// failing over to a backup shows its failure, taken over.
#[test]
fn activity_counts_attempts_by_provider_key_and_model() {
    let engine = BillingEngine::new();
    let now = 1_000_000;
    let trace = |id: &str, ts: u64, model: &str, status: TraceStatus| RequestTrace {
        id: id.into(),
        card_id: "card".into(),
        ts,
        invocation_id: id.into(),
        exposed_model: model.into(),
        status,
        ..RequestTrace::default()
    };
    // The primary is refused and the backup answers.
    engine.record_trace(RequestTrace {
        attempt_chain: vec![
            attempt("primary", "key-a", Some("http_429")),
            attempt("backup", "key-b", None),
        ],
        ..trace("failed-over", now - 600, "model-a", TraceStatus::Success)
    });
    // The primary times out and answers with its other Key: no provider took over.
    engine.record_trace(RequestTrace {
        attempt_chain: vec![
            attempt("primary", "key-a", Some("timeout")),
            attempt("primary", "key-a2", None),
        ],
        ..trace("retried", now - 2 * 3600, "model-a", TraceStatus::Success)
    });
    // Everything failed.
    engine.record_trace(RequestTrace {
        error_class: Some("upstream_start_failed".into()),
        attempt_chain: vec![attempt("backup", "key-b", Some("http_500"))],
        ..trace("failed", now - 3 * 86_400, "model-b", TraceStatus::Error)
    });
    // Still streaming: its attempts count, the request does not yet.
    engine.record_trace(RequestTrace {
        attempt_chain: vec![attempt("primary", "key-a", None)],
        ..trace("running", now - 50, "model-a", TraceStatus::InProgress)
    });
    // Refused for the card's balance: nothing about the model.
    engine.record_trace(RequestTrace {
        error_class: Some("insufficient_balance".into()),
        ..trace("broke", now - 100, "model-a", TraceStatus::Error)
    });
    // Refused for want of a route.
    engine.record_trace(RequestTrace {
        error_class: Some("no_route".into()),
        ..trace("unrouted", now - 10, "model-c", TraceStatus::Error)
    });
    // Older than a week.
    engine.record_trace(RequestTrace {
        attempt_chain: vec![attempt("primary", "key-a", Some("http_401"))],
        ..trace("old", now - 8 * 86_400, "model-a", TraceStatus::Error)
    });

    let activity = engine.activity(now);
    let windows = |row: &billing::observability::AttemptWindow| {
        (
            row.attempts,
            row.failures,
            row.taken_over,
            row.failures_by_kind.clone(),
        )
    };
    let kinds = |pairs: &[(&str, u64)]| {
        pairs
            .iter()
            .map(|(kind, count)| (kind.to_string(), *count))
            .collect::<std::collections::BTreeMap<_, _>>()
    };
    let ids: Vec<_> = activity
        .provider_attempts
        .iter()
        .map(|row| row.provider_id.as_str())
        .collect();
    assert_eq!(ids, ["primary", "backup"]);
    let primary = &activity.provider_attempts[0];
    assert_eq!(
        windows(&primary.last_1h),
        (2, 1, 1, kinds(&[("http_429", 1)]))
    );
    assert_eq!(
        windows(&primary.last_24h),
        (4, 2, 1, kinds(&[("http_429", 1), ("timeout", 1)]))
    );
    assert_eq!(windows(&primary.last_7d), windows(&primary.last_24h));
    let backup = &activity.provider_attempts[1];
    assert_eq!(windows(&backup.last_1h), (1, 0, 0, kinds(&[])));
    assert_eq!(
        windows(&backup.last_7d),
        (2, 1, 0, kinds(&[("http_500", 1)]))
    );

    let keys: Vec<_> = activity
        .key_attempts
        .iter()
        .map(|row| {
            (
                row.key_id.as_str(),
                row.provider_id.as_str(),
                row.last_7d.attempts,
                row.last_7d.failures,
                row.last_7d.taken_over,
            )
        })
        .collect();
    assert_eq!(
        keys,
        [
            ("key-a", "primary", 3, 2, 1),
            ("key-b", "backup", 2, 1, 0),
            ("key-a2", "primary", 1, 0, 0),
        ]
    );

    let models: Vec<_> = activity
        .model_health
        .iter()
        .map(|row| row.model.as_str())
        .collect();
    assert_eq!(models, ["model-a", "model-b", "model-c"]);
    let model_a = &activity.model_health[0];
    assert_eq!((model_a.last_1h.requests, model_a.last_1h.failures), (1, 0));
    assert_eq!(
        (model_a.last_24h.requests, model_a.last_24h.failures),
        (2, 0)
    );
    let model_b = &activity.model_health[1];
    assert_eq!(model_b.last_24h.requests, 0);
    assert_eq!(
        (
            model_b.last_7d.requests,
            model_b.last_7d.failures,
            model_b.last_7d.last_failure_at,
            model_b.last_7d.top_failure_kind.as_deref()
        ),
        (1, 1, Some(now - 3 * 86_400), Some("upstream_start_failed"))
    );
    let model_c = &activity.model_health[2];
    assert_eq!(
        model_c.last_1h.top_failure_kind.as_deref(),
        Some("no_route")
    );
    assert_eq!(model_c.last_1h.last_failure_at, Some(now - 10));

    // As the stats endpoint sends it.
    let json = serde_json::to_value(&activity).unwrap();
    assert_eq!(json["providerAttempts"][0]["providerId"], "primary");
    assert_eq!(json["providerAttempts"][0]["last1h"]["takenOver"], 1);
    assert_eq!(
        json["providerAttempts"][0]["last24h"]["failuresByKind"]["timeout"],
        1
    );
    assert_eq!(json["keyAttempts"][0]["keyId"], "key-a");
    assert_eq!(
        json["modelHealth"][1]["last7d"]["lastFailureAt"],
        now - 3 * 86_400
    );
    assert_eq!(
        json["modelHealth"][1]["last7d"]["topFailureKind"],
        "upstream_start_failed"
    );
    assert_eq!(json["modelUsage7d"], serde_json::json!([]));
}

/// A refusal for the card or the request itself says nothing about a model or an upstream:
/// it is counted apart, and the same refusal again within a minute adds no trace.
#[test]
fn refusals_are_counted_apart_and_a_repeated_one_adds_no_trace() {
    let engine = BillingEngine::new();
    let now = 1_000_000;
    let trace = |id: &str, card: &str, ts: u64, model: &str, class: Option<&str>| RequestTrace {
        id: id.into(),
        card_id: card.into(),
        ts,
        invocation_id: format!("{card}:{id}"),
        exposed_model: model.into(),
        status: if class.is_some() {
            TraceStatus::Error
        } else {
            TraceStatus::Success
        },
        error_class: class.map(str::to_string),
        ..RequestTrace::default()
    };
    // Served, and failed upstream.
    engine.record_trace(RequestTrace {
        provider_id: Some("primary".into()),
        attempt_chain: vec![attempt("primary", "key-a", None)],
        ..trace("served", "card-1", now - 500, "model-a", None)
    });
    engine.record_trace(RequestTrace {
        provider_id: Some("primary".into()),
        attempt_chain: vec![attempt("primary", "key-a", Some("http_500"))],
        ..trace(
            "failed",
            "card-1",
            now - 400,
            "model-a",
            Some("upstream_start_failed"),
        )
    });
    // The upstream refused a prompt too long: it did right.
    engine.record_trace(RequestTrace {
        provider_id: Some("primary".into()),
        attempt_chain: vec![attempt("primary", "key-a", Some("upstream_service"))],
        ..trace(
            "long",
            "card-1",
            now - 300,
            "model-a",
            Some("input_too_long"),
        )
    });
    // A card out of credits, retrying in a loop: four times within a minute of the first,
    // then again after it.
    for (id, ts) in [
        ("broke-1", now - 200),
        ("broke-2", now - 190),
        ("broke-3", now - 170),
        ("broke-4", now - 141),
        ("broke-5", now - 140),
    ] {
        engine.record_trace(trace(
            id,
            "card-2",
            ts,
            "model-a",
            Some("insufficient_balance"),
        ));
    }
    // Another card, and another reason, keep their own traces.
    engine.record_trace(trace(
        "broke-other",
        "card-3",
        now - 180,
        "model-a",
        Some("insufficient_balance"),
    ));
    engine.record_trace(trace(
        "image",
        "card-2",
        now - 160,
        "model-a",
        Some("unsupported_capability"),
    ));
    // A model no request names: counted in the totals, no row of its own.
    engine.record_trace(trace(
        "absent",
        "card-2",
        now - 150,
        "typed-model",
        Some("model_not_listed"),
    ));
    // Refused for want of a route, twice: the operator's to fix, so failures.
    engine.record_trace(trace(
        "unrouted-1",
        "card-1",
        now - 100,
        "model-a",
        Some("no_route"),
    ));
    engine.record_trace(trace(
        "unrouted-2",
        "card-1",
        now - 90,
        "model-a",
        Some("no_route"),
    ));

    let traces = engine.list_traces(None, 100);
    let kept: Vec<_> = traces
        .iter()
        .map(|trace| (trace.id.as_str(), trace.repeats, trace.last_seen_secs))
        .collect();
    assert_eq!(
        kept,
        [
            ("unrouted-1", 1, Some(now - 90)),
            ("absent", 0, None),
            ("image", 0, None),
            ("broke-other", 0, None),
            ("broke-5", 0, None),
            ("broke-1", 3, Some(now - 141)),
            ("long", 0, None),
            ("failed", 0, None),
            ("served", 0, None),
        ]
    );

    let activity = engine.activity(now);
    let window = &activity.last_24h;
    // Served, failed, and no route twice.
    assert_eq!(
        (
            window.requests,
            window.succeeded,
            window.failed,
            window.refused
        ),
        (4, 1, 3, 9)
    );
    let hourly = activity.hourly.iter().fold((0, 0, 0), |sum, hour| {
        (
            sum.0 + hour.requests,
            sum.1 + hour.failed,
            sum.2 + hour.refused,
        )
    });
    assert_eq!(hourly, (4, 3, 9));
    let provider = &activity.providers[0];
    assert_eq!(
        (provider.requests, provider.failed, provider.refused),
        (2, 1, 1)
    );
    let attempts = &activity.provider_attempts[0].last_24h;
    assert_eq!(
        (attempts.attempts, attempts.failures, attempts.refused),
        (2, 1, 1)
    );
    assert_eq!(activity.key_attempts[0].last_24h.refused, 1);
    let models: Vec<_> = activity
        .model_health
        .iter()
        .map(|model| model.model.as_str())
        .collect();
    assert_eq!(models, ["model-a"]);
    let health = &activity.model_health[0].last_24h;
    assert_eq!(
        (health.requests, health.failures, health.refused),
        (4, 3, 8)
    );
    assert_eq!(health.failures_by_kind.get("no_route"), Some(&2));
    assert_eq!(health.last_failure_at, Some(now - 90));

    // As the stats endpoint and the traces list send them.
    let json = serde_json::to_value(&activity).unwrap();
    assert_eq!(json["last24h"]["refused"], 9);
    assert_eq!(json["modelHealth"][0]["last24h"]["refused"], 8);
    assert_eq!(json["providerAttempts"][0]["last24h"]["refused"], 1);
    let json = serde_json::to_value(&traces[5]).unwrap();
    assert_eq!(
        (&json["repeats"], &json["last_seen_secs"]),
        (&serde_json::json!(3), &serde_json::json!(now - 141))
    );
    // A trace without repeats is saved as before.
    let json = serde_json::to_value(&traces[6]).unwrap();
    assert!(json.get("repeats").is_none() && json.get("last_seen_secs").is_none());
}

/// However fast one card is refused, real requests keep their traces.
#[test]
fn a_refusal_flood_cannot_push_real_requests_out_of_the_traces() {
    let engine = BillingEngine::new();
    let now = 1_000_000;
    engine.record_trace(RequestTrace {
        id: "served".into(),
        card_id: "card-1".into(),
        ts: now,
        invocation_id: "card-1:served".into(),
        exposed_model: "model".into(),
        status: TraceStatus::Success,
        ..RequestTrace::default()
    });
    // Sixty a second for five minutes: one trace a minute.
    for second in 0..300 {
        for n in 0..60 {
            engine.record_trace(RequestTrace {
                id: format!("refused-{second}-{n}"),
                card_id: "card-2".into(),
                ts: now + second,
                invocation_id: format!("card-2:{second}-{n}"),
                exposed_model: "model".into(),
                status: TraceStatus::Error,
                error_class: Some("concurrency_limit".into()),
                ..RequestTrace::default()
            });
        }
    }
    let traces = engine.list_traces(None, 100);
    assert_eq!(traces.len(), 6);
    assert!(traces.iter().any(|trace| trace.id == "served"));
    let counted: u64 = traces
        .iter()
        .filter(|trace| trace.card_id == "card-2")
        .map(|trace| trace.occurrences())
        .sum();
    assert_eq!(counted, 300 * 60);
    assert_eq!(engine.activity(now + 300).last_24h.refused, 300 * 60);
}

/// Billed use by customer model over the last week, from the ledger: requests and cards.
#[test]
fn activity_counts_a_weeks_billed_requests_and_cards_by_model() {
    let engine = BillingEngine::new();
    let now = 1_000_000;
    for id in ["card-1", "card-2"] {
        let mut card = Card::new(id, "group", 100_000_000);
        card.status = CardStatus::Active;
        engine.upsert_card(card);
    }
    let tokens = UsageTokens {
        uncached_input_tokens: 100,
        output_tokens: 100,
        cache_creation_tokens: 0,
        cache_read_tokens: 0,
    };
    for (invocation, card, model, at) in [
        ("inv-1", "card-1", "model-a", now - 60),
        ("inv-2", "card-1", "model-a", now - 86_400),
        ("inv-3", "card-2", "model-a", now - 2 * 86_400),
        ("inv-4", "card-2", "model-b", now - 3 * 86_400),
        ("inv-5", "card-2", "model-b", now - 8 * 86_400),
    ] {
        engine
            .reserve(
                card,
                invocation,
                &ReservationEstimateParams::new(100, 100),
                at - 1,
                60,
            )
            .unwrap();
        engine
            .settle(invocation, &tokens, model, "provider", "target", at)
            .unwrap();
    }
    let usage: Vec<_> = engine
        .activity(now)
        .model_usage_7d
        .iter()
        .map(|row| (row.model.clone(), row.requests, row.cards))
        .collect();
    assert_eq!(
        usage,
        [("model-a".to_string(), 3, 2), ("model-b".to_string(), 1, 1)]
    );
}

/// A billed request: the whole prompt as input, cache reads and writes within it.
fn usage_entry(
    id: &str,
    provider: &str,
    tokens: (u64, u64, u64, u64),
    credits: i64,
    cost: i64,
    costed: bool,
) -> billing::ledger::LedgerEntry {
    let (uncached, output, read, write) = tokens;
    billing::ledger::LedgerEntry {
        id: format!("led-{id}"),
        card_id: "card".into(),
        kind: billing::ledger::LedgerKind::Usage,
        invocation_id: Some(id.into()),
        exposed_model: "model".into(),
        provider_id: provider.into(),
        target_model: "target".into(),
        input_tokens: uncached + read + write,
        output_tokens: output,
        cache_creation_tokens: write,
        cache_read_tokens: read,
        credits_charged: credits,
        provider_cost_micro_cny: cost,
        rate_card_version: costed.then(|| "price-v1".to_string()),
        ts_secs: 1_000,
        operator_id: None,
        reason: None,
        credit_face_value_cny: None,
        detail: None,
    }
}

#[test]
fn finance_by_provider_and_margin_over_costed_requests_only() {
    let entries = vec![
        usage_entry(
            "a1",
            "prov-a",
            (1_000, 200, 300, 400),
            5_000_000,
            20_000,
            true,
        ),
        usage_entry("a2", "prov-a", (500, 100, 0, 0), 1_000_000, 5_000, true),
        usage_entry("b1", "prov-b", (100, 10, 0, 50), 3_000_000, 90_000, true),
        // Priced by no published version: its cost is unknown.
        usage_entry("u1", "prov-b", (100, 10, 0, 0), 7_000_000, 1, false),
    ];
    let costs = billing::observability::compute_provider_costs(&entries);
    let rows: Vec<_> = costs
        .iter()
        .map(|row| {
            (
                row.provider_id.as_str(),
                row.requests,
                row.uncached_input_tokens,
                row.output_tokens,
                row.cache_read_tokens,
                row.cache_write_tokens,
                row.cost_micro_cny,
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            ("prov-b", 2, 200, 20, 0, 50, 90_001),
            ("prov-a", 2, 1_500, 300, 300, 400, 25_000),
        ]
    );

    let settings = BillingSettings {
        credit_face_value_cny: 0.01,
        ..BillingSettings::default()
    };
    let margin = billing::observability::compute_costed_margin(&entries, &settings);
    assert_eq!(
        (margin.costed_requests, margin.costed_credits),
        (3, 9_000_000)
    );
    // Nine credits at one fen each.
    assert_eq!(margin.revenue_micro_cny, 90_000);
    assert_eq!(margin.cost_micro_cny, 115_000);
    assert_eq!(margin.gross_profit_micro_cny, -25_000);
    assert!((margin.margin_percentage.unwrap() + 27.777).abs() < 0.01);
    assert_eq!(
        (margin.uncosted_requests, margin.uncosted_credits),
        (1, 7_000_000)
    );
    let none = billing::observability::compute_costed_margin(&[], &settings);
    assert_eq!(none.margin_percentage, None);
}

#[test]
fn a_cost_estimated_for_want_of_the_serving_route_is_not_costed() {
    let labelled = |id: &str, source: &str, versioned: bool| {
        let mut entry = usage_entry(id, "prov", (100, 10, 0, 0), 1_000_000, 1_000, versioned);
        entry.reason = Some(format!("provider_cost:{source}"));
        entry
    };
    let entries = vec![
        labelled("official", "official=prov/target", false),
        labelled("version", "rate_card_version=price-v1", true),
        // A fallback without a price of its own, costed at the version it was charged at.
        labelled("fallback", "estimated_missing_target_rate", true),
        // From before costs were labelled: known when a price version charged it.
        usage_entry("legacy", "prov", (100, 10, 0, 0), 1_000_000, 1_000, true),
        usage_entry("unpriced", "prov", (100, 10, 0, 0), 1_000_000, 1_000, false),
    ];
    let classes: Vec<_> = entries
        .iter()
        .map(|entry| (entry.cost_is_known(), entry.cost_is_estimated()))
        .collect();
    assert_eq!(
        classes,
        [
            (true, false),
            (true, false),
            (false, true),
            (true, false),
            (false, false)
        ]
    );
    let settings = BillingSettings {
        credit_face_value_cny: 0.01,
        ..BillingSettings::default()
    };
    let margin = billing::observability::compute_costed_margin(&entries, &settings);
    assert_eq!(
        (
            margin.costed_requests,
            margin.uncosted_requests,
            margin.uncosted_credits,
            margin.estimated_requests
        ),
        (3, 2, 2_000_000, 1)
    );
    assert_eq!(margin.cost_micro_cny, 3_000);
    // A card event is never a billed request.
    let mut adjustment = labelled("adjust", "official=prov/target", false);
    adjustment.kind = billing::ledger::LedgerKind::Adjustment;
    assert!(!adjustment.cost_is_known() && !adjustment.cost_is_estimated());
}

#[test]
fn sales_count_cards_issued_and_activated_in_the_period_at_list_price() {
    let tier = |id: &str, points: i64, created: u64, activated: Option<u64>| {
        let mut card = Card::new(id, "group", points * 1_000_000);
        card.created_at = created;
        card.activated_at = activated;
        if activated.is_some() {
            card.status = CardStatus::Active;
        }
        card
    };
    let mut misprint = tier("misprint", 1000, 150, None);
    misprint.status = CardStatus::Voided;
    let mut refunded = tier("refunded", 2000, 150, Some(160));
    refunded.status = CardStatus::Voided;
    let cards = [
        tier("pro", 1000, 150, None),
        tier("pro-plus", 2000, 120, Some(190)),
        tier("power", 10000, 50, Some(150)),
        tier("custom", 123, 150, Some(150)),
        tier("later", 5000, 200, Some(250)),
        misprint,
        refunded,
    ];
    let plans = billing::template::seed_plans(&Default::default());
    // Without group-pro-plus, the first group that takes cards: never the acceptance group,
    // closed to issuance, however it sorts.
    let mut probe = billing::Group::pro_plus("group-acceptance", "Acceptance");
    probe.issuance_enabled = false;
    let groups: std::collections::HashMap<_, _> = [
        (probe.id.clone(), probe.clone()),
        (
            "group-retail".to_string(),
            billing::Group::pro_plus("group-retail", "Retail"),
        ),
    ]
    .into();
    let seeded = billing::template::seed_plans(&groups);
    assert!(seeded
        .iter()
        .all(|plan| plan.default_group_id == "group-retail"));
    let only_probe = [(probe.id.clone(), probe)].into();
    assert!(billing::template::seed_plans(&only_probe)
        .iter()
        .all(|plan| plan.default_group_id == "group-pro-plus"));
    let sales = billing::observability::compute_sales(cards.iter(), &plans, Some(100), Some(200));
    // Issued in [100, 200): pro, pro-plus, custom and the refunded card; the misprint was
    // voided unsold, "later" is at the end of the period and "power" before it.
    assert_eq!(sales.issued_cards, 4);
    assert_eq!(sales.unpriced_issued_cards, 1);
    assert_eq!(
        sales.issued_value_micro_cny,
        30_000_000 + 55_000_000 + 55_000_000
    );
    // Activated in it: pro-plus, power, custom, refunded.
    assert_eq!(sales.activated_cards, 4);
    assert_eq!(sales.unpriced_activated_cards, 1);
    assert_eq!(
        sales.activated_value_micro_cny,
        55_000_000 + 250_000_000 + 55_000_000
    );
    let by_plan: Vec<_> = sales
        .by_plan
        .iter()
        .map(|plan| {
            (
                plan.template_id.as_str(),
                plan.issued_cards,
                plan.activated_cards,
            )
        })
        .collect();
    assert_eq!(
        by_plan,
        [
            ("tier-1000", 1, 0),
            ("tier-2000", 2, 2),
            ("tier-5000", 0, 0),
            ("tier-10000", 0, 1)
        ]
    );
    // Without a period, every card.
    let all = billing::observability::compute_sales(cards.iter(), &plans, None, None);
    assert_eq!((all.issued_cards, all.activated_cards), (6, 5));
}

/// A card issued from the catalog counts under its plan at the price it was issued at, one
/// issued before it under the tier its credits name at the tier's list price, whatever the
/// catalog says now; a plan the catalog no longer holds still gets its row.
#[test]
fn sales_count_each_card_at_the_plan_price_it_was_issued_at() {
    let mut plans = billing::template::seed_plans(&Default::default());
    // PRO now sells for ¥35, and a trial plan was added.
    plans[0].price_cny = 35.0;
    let trial = billing::template::Plan {
        id: "trial".into(),
        name: "体验卡".into(),
        points: 300,
        price_cny: 9.9,
        validity_days: 7,
        max_devices: 1,
        concurrency: 1,
        default_group_id: "group".into(),
        kiro_plan_type: "CUSTOM".into(),
        on_sale: true,
        sort_order: 1,
    };
    plans.insert(0, trial.clone());
    let issued = |id: &str, plan: &billing::template::Plan| {
        let mut card = Card::from_template(id, "hash", &plan.template("group"), None, 150);
        card.activated_at = Some(160);
        card
    };
    let mut before_catalog = Card::new("legacy-pro", "group", 1_000_000_000);
    before_catalog.created_at = 150;
    let mut retired = trial.clone();
    retired.id = "retired".into();
    retired.price_cny = 5.0;
    let cards = [
        issued("trial-1", &trial),
        issued("trial-2", &trial),
        issued("pro-new", &plans[1]),
        before_catalog,
        issued("retired-1", &retired),
    ];
    let sales = billing::observability::compute_sales(cards.iter(), &plans, None, None);
    let rows: Vec<_> = sales
        .by_plan
        .iter()
        .map(|plan| {
            (
                plan.plan_id.as_str(),
                plan.issued_cards,
                plan.issued_value_micro_cny,
                plan.activated_cards,
                plan.activated_value_micro_cny,
                plan.price_micro_cny,
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            ("trial", 2, 19_800_000, 2, 19_800_000, 9_900_000),
            // One card at the ¥35 it was issued at, one at the ¥30 list price before it.
            ("tier-1000", 2, 65_000_000, 1, 35_000_000, 35_000_000),
            ("tier-2000", 0, 0, 0, 0, 55_000_000),
            ("tier-5000", 0, 0, 0, 0, 130_000_000),
            ("tier-10000", 0, 0, 0, 0, 250_000_000),
            ("retired", 1, 5_000_000, 1, 5_000_000, 5_000_000),
        ]
    );
    assert_eq!(
        sales.issued_value_micro_cny,
        19_800_000 + 65_000_000 + 5_000_000
    );
    assert_eq!((sales.issued_cards, sales.unpriced_issued_cards), (5, 0));
}

#[test]
fn liability_is_the_balance_of_cards_that_can_still_be_used() {
    let now = 10_000;
    let card = |id: &str, status: CardStatus, total: i64, used: i64| {
        let mut card = Card::new(id, "group", total);
        card.status = status;
        card.credit_used = used;
        card
    };
    let mut lapsed = card("lapsed", CardStatus::Active, 5_000_000, 0);
    lapsed.valid_until = Some(now - 1);
    let mut current = card("current", CardStatus::Active, 5_000_000, 1_000_000);
    current.valid_until = Some(now + 1);
    let mut archived = card("archived", CardStatus::Expired, 5_000_000, 0);
    archived.archived_at = Some(1);
    // Frozen while active and past its validity since: unfrozen, it would be expired.
    let mut frozen_lapsed = card("frozen-lapsed", CardStatus::Frozen, 7_000_000, 0);
    frozen_lapsed.frozen_from = Some(CardStatus::Active);
    frozen_lapsed.valid_until = Some(now);
    let cards = [
        current,
        lapsed,
        archived,
        frozen_lapsed,
        card("waiting", CardStatus::Unactivated, 2_000_000, 0),
        card("frozen", CardStatus::Frozen, 3_000_000, 500_000),
        card("banned", CardStatus::Banned, 9_000_000, 0),
        card("voided", CardStatus::Voided, 9_000_000, 0),
        card("expired", CardStatus::Expired, 9_000_000, 0),
        card("in-debt", CardStatus::Active, 1_000_000, 4_000_000),
    ];
    let settings = BillingSettings {
        credit_face_value_cny: 0.02,
        ..BillingSettings::default()
    };
    let liability = billing::observability::compute_liability(cards.iter(), &settings, now);
    // current 4, waiting 2, frozen 2.5, in-debt 0 credits.
    assert_eq!(liability.cards, 4);
    assert_eq!(liability.micro_credits, 8_500_000);
    assert_eq!(liability.value_micro_cny, 170_000);
    assert_eq!(
        (
            liability.unactivated_cards,
            liability.unactivated_micro_credits
        ),
        (1, 2_000_000)
    );
}

#[test]
fn the_ledger_csv_adds_readable_columns_after_the_original_ones() {
    let engine = BillingEngine::new();
    engine.upsert_provider(billing::Provider::new(
        "prov-1",
        "Upstream One",
        billing::ProviderFormat::OpenAi,
        "https://example.invalid",
    ));
    let mut card = Card::new("card-readable", "group-pro-plus", 100_000_000);
    card.status = CardStatus::Active;
    engine.upsert_card(card);
    engine
        .reserve(
            "card-readable",
            "inv-readable",
            &ReservationEstimateParams::new(500, 100),
            1_700_000_000,
            60,
        )
        .unwrap();
    let entry = engine
        .settle(
            "inv-readable",
            &UsageTokens {
                uncached_input_tokens: 500,
                output_tokens: 100,
                cache_creation_tokens: 30,
                cache_read_tokens: 70,
            },
            "gpt-4o",
            "prov-1",
            "gpt-4o",
            1_700_000_000,
        )
        .unwrap();
    engine
        .adjust_balance(
            "card-readable",
            -1_500_000,
            "admin",
            "correction",
            1_700_000_100,
        )
        .unwrap();

    let rows = csv_rows(&engine.export_ledger_csv(Some("card-readable")));
    assert_eq!(
        rows[0],
        [
            "id",
            "card_id",
            "ts",
            "kind",
            "invocation_id",
            "exposed_model",
            "provider_id",
            "input_tokens",
            "output_tokens",
            "credits_charged",
            "provider_cost_micro_cny",
            "time_utc",
            "provider_name",
            "cache_read_tokens",
            "cache_write_tokens",
            "credits",
            "revenue_cny",
            "cost_cny",
            "operator",
            "reason",
            "rate_card_version",
            "key_id",
            "kind",
            "cash_cny"
        ]
    );
    let usage = &rows[1];
    // Usage is no balance adjustment: no kind, no money.
    assert_eq!((usage[22].as_str(), usage[23].as_str()), ("", ""));
    // An adjustment made without a kind is the one its sign gives.
    assert_eq!(
        (rows[2][22].as_str(), rows[2][23].as_str()),
        ("correction", "")
    );
    assert_eq!(usage[11], "2023-11-14T22:13:20Z");
    assert_eq!(usage[12], "Upstream One");
    assert_eq!((usage[13].as_str(), usage[14].as_str()), ("70", "30"));
    let credits = entry.credits_charged;
    assert_eq!(
        usage[15].parse::<f64>().unwrap(),
        credits as f64 / 1_000_000.0
    );
    // At the default face value of one fen per credit.
    assert_eq!(
        usage[16].parse::<f64>().unwrap(),
        (credits as f64 * 0.01).round() / 1_000_000.0
    );
    assert_eq!(
        usage[17].parse::<f64>().unwrap(),
        entry.provider_cost_micro_cny as f64 / 1_000_000.0
    );
    // Where its cost came from, and no operator.
    assert_eq!(usage[18], "");
    assert!(usage[19].starts_with("provider_cost:"), "{usage:?}");
    // An adjustment: credits as a signed decimal, no revenue or cost, who and why.
    let adjustment = &rows[2];
    assert_eq!(adjustment[15], "-1.5");
    assert_eq!((adjustment[16].as_str(), adjustment[17].as_str()), ("", ""));
    assert_eq!(adjustment[12], "system");
    assert_eq!(
        (adjustment[18].as_str(), adjustment[19].as_str()),
        ("admin", "correction")
    );
}

#[test]
fn adjustments_add_up_credits_given_and_taken_once_each() {
    let entry = |id: &str, kind: billing::ledger::LedgerKind, credits: i64| {
        let mut entry = usage_entry(id, "system", (0, 0, 0, 0), credits, 0, false);
        entry.kind = kind;
        entry
    };
    use billing::ledger::LedgerKind::{Adjustment, Topup, Usage};
    let live = [
        entry("comp", Adjustment, 2_000_000),
        entry("promo", Adjustment, 500_000_000),
        entry("correction", Adjustment, -1_500_000),
        // A card event, a top-up and a request move no credits by hand.
        entry("note", Adjustment, 0),
        entry("topup", Topup, 1_000_000_000),
        entry("usage", Usage, 3_000_000),
    ];
    // An archived adjustment also still in the live ledger counts once.
    let archived = [
        entry("comp", Adjustment, 2_000_000),
        entry("old", Adjustment, 1_000_000),
    ];
    let totals = billing::observability::compute_adjustments(live.iter().chain(&archived));
    assert_eq!(
        (
            totals.count,
            totals.positive_micro_credits,
            totals.negative_micro_credits,
            totals.net_micro_credits
        ),
        (4, 503_000_000, -1_500_000, 501_500_000)
    );
    // Made before kinds, each is the kind its sign gives: credits given for no request are a
    // gift, credits taken a correction.
    let by_kind = &totals.by_kind;
    assert_eq!(
        (by_kind.gift.count, by_kind.gift.positive_micro_credits),
        (3, 503_000_000)
    );
    assert_eq!(
        (
            by_kind.correction.count,
            by_kind.correction.negative_micro_credits
        ),
        (1, -1_500_000)
    );
    assert_eq!(by_kind.compensation, Default::default());
    assert_eq!(by_kind.refund, Default::default());
    assert_eq!(
        billing::observability::compute_adjustments([]),
        Default::default()
    );
}

/// A billed request names the Key that served it, from its traced attempts, and the ledger
/// export shows it with the price version that charged it and where its cost came from.
#[test]
fn a_usage_entry_names_the_key_that_served_it() {
    let engine = BillingEngine::new();
    engine.upsert_rate_card_version(billing::RateCardVersion {
        id: "price-keyed".to_string(),
        rate_card_id: "default".to_string(),
        model: "keyed-model".to_string(),
        currency: billing::Currency::Cny,
        pricing_mode: billing::PricingMode::Fixed,
        input_price_per_m: 1.0,
        output_price_per_m: 1.0,
        cache_creation_price_per_m: 0.0,
        cache_read_price_per_m: 0.0,
        fixed_input_credit_per_m: 1_000_000,
        fixed_output_credit_per_m: 1_000_000,
        fixed_cache_creation_credit_per_m: 0,
        fixed_cache_read_credit_per_m: 0,
        per_call_credit: 0,
        margin_multiplier: 1.0,
        effective_from_secs: 0,
        official: None,
    });
    let mut card = Card::new("card-keyed", "group-pro-plus", 100_000_000);
    card.status = CardStatus::Active;
    engine.upsert_card(card);
    let tokens = UsageTokens {
        uncached_input_tokens: 1_000,
        output_tokens: 1_000,
        ..UsageTokens::default()
    };
    let settle = |invocation: &str, attempts: Vec<AttemptRecord>| {
        if !attempts.is_empty() {
            engine.record_trace(RequestTrace {
                id: format!("attempt-{invocation}"),
                card_id: "card-keyed".into(),
                ts: 1_000,
                invocation_id: invocation.into(),
                exposed_model: "keyed-model".into(),
                status: TraceStatus::InProgress,
                attempt_chain: attempts,
                ..RequestTrace::default()
            });
        }
        let params = ReservationEstimateParams::new(1_000, 1_000).with_model("keyed-model");
        engine
            .reserve("card-keyed", invocation, &params, 1_000, 60)
            .unwrap();
        engine
            .settle(
                invocation,
                &tokens,
                "keyed-model",
                "prov-b",
                "keyed-model",
                1_001,
            )
            .unwrap()
    };
    // prov-a's Key was refused, and prov-b's answered.
    let served = settle(
        "inv-keyed",
        vec![
            attempt("prov-a", "key-a", Some("http_429")),
            attempt("prov-b", "key-b", None),
        ],
    );
    assert_eq!(served.detail, Some(serde_json::json!({ "keyId": "key-b" })));
    // Without a traced attempt, no Key is named.
    let untraced = settle("inv-untraced", vec![]);
    assert_eq!(untraced.detail, None);

    let rows = csv_rows(&engine.export_ledger_csv(Some("card-keyed")));
    let column = |name: &str| rows[0].iter().position(|cell| cell == name).unwrap();
    let (reason, version, key) = (
        column("reason"),
        column("rate_card_version"),
        column("key_id"),
    );
    assert_eq!(
        (
            rows[1][reason].as_str(),
            rows[1][version].as_str(),
            rows[1][key].as_str()
        ),
        (
            "provider_cost:rate_card_version=price-keyed",
            "price-keyed",
            "key-b"
        )
    );
    assert_eq!(rows[2][key], "");
}

#[test]
fn iso_times_are_utc_calendar_dates() {
    for (secs, iso) in [
        (0, "1970-01-01T00:00:00Z"),
        (951_782_400, "2000-02-29T00:00:00Z"),
        (1_700_000_000, "2023-11-14T22:13:20Z"),
        (4_102_444_799, "2099-12-31T23:59:59Z"),
    ] {
        assert_eq!(billing::observability::iso_utc(secs), iso);
    }
}

#[test]
fn plan_prices_name_the_issuance_tiers() {
    for plan in billing::template::PLAN_PRICES {
        let template = billing::CardTemplate::tier(plan.template_id, "group").unwrap();
        assert_eq!(template.credit_total, plan.points * 1_000_000);
        assert_eq!(template.name, plan.name);
        let card = Card::new("card", "group", template.credit_total);
        assert_eq!(card.plan_name(), Some(plan.name));
    }
}

#[test]
fn trace_search_filters_the_retained_traces_and_totals_every_match() {
    let engine = BillingEngine::new();
    let trace = |id: &str, card: &str, model: &str, ts: u64, status: TraceStatus| RequestTrace {
        id: id.into(),
        card_id: card.into(),
        ts,
        invocation_id: id.into(),
        exposed_model: model.into(),
        status,
        ..RequestTrace::default()
    };
    engine.record_trace(RequestTrace {
        provider_id: Some("backup".into()),
        credits_charged: 5,
        provider_cost_micro_cny: 50,
        attempt_chain: vec![
            attempt("primary", "key-a", Some("http_429")),
            attempt("backup", "key-b", None),
        ],
        ..trace("t1", "card-a", "model-a", 100, TraceStatus::Success)
    });
    engine.record_trace(RequestTrace {
        provider_id: Some("primary".into()),
        ..trace("t2", "card-a", "model-b", 200, TraceStatus::Error)
    });
    engine.record_trace(RequestTrace {
        provider_id: Some("primary".into()),
        credits_charged: 7,
        provider_cost_micro_cny: 70,
        ..trace("t3", "card-b", "model-a", 300, TraceStatus::Success)
    });
    engine.record_trace(RequestTrace {
        provider_id: Some("backup".into()),
        credits_charged: 2,
        provider_cost_micro_cny: 20,
        ..trace("t4", "card-a", "model-a", 400, TraceStatus::ClientAborted)
    });
    let search = |filter: billing::observability::TraceFilter, limit: usize| {
        let (traces, totals) = engine.search_traces(&filter, limit);
        let ids: Vec<String> = traces.into_iter().map(|trace| trace.id).collect();
        (
            ids,
            (
                totals.count,
                totals.failures,
                totals.credits_charged,
                totals.cost_micro_cny,
            ),
        )
    };
    use billing::observability::TraceFilter;
    // Newest first, at most the limit; the totals count every match.
    assert_eq!(
        search(TraceFilter::default(), 2),
        (vec!["t4".into(), "t3".into()], (4, 1, 14, 140))
    );
    assert_eq!(
        search(
            TraceFilter {
                card_id: Some("card-a".into()),
                ..TraceFilter::default()
            },
            10
        ),
        (vec!["t4".into(), "t2".into(), "t1".into()], (3, 1, 7, 70))
    );
    assert_eq!(
        search(
            TraceFilter {
                model: Some("model-a".into()),
                from_secs: Some(150),
                ..TraceFilter::default()
            },
            10
        )
        .0,
        ["t4", "t3"]
    );
    // A provider matches the requests it answered and those it was tried for.
    assert_eq!(
        search(
            TraceFilter {
                provider: Some("primary".into()),
                ..TraceFilter::default()
            },
            10
        )
        .0,
        ["t3", "t2", "t1"]
    );
    assert_eq!(
        search(
            TraceFilter {
                status: Some(TraceStatus::Error),
                ..TraceFilter::default()
            },
            10
        ),
        (vec!["t2".into()], (1, 1, 0, 0))
    );
    // Up to, not including, the end.
    assert_eq!(
        search(
            TraceFilter {
                to_secs: Some(300),
                ..TraceFilter::default()
            },
            10
        )
        .0,
        ["t2", "t1"]
    );
}

/// Sales count each card at what was paid for it, else its plan's price, and an upgrade does
/// not restate them: the card counts at what it was issued for, the upgrade's payment apart.
#[test]
fn sales_count_what_was_paid_and_an_upgrade_does_not_restate_them() {
    let engine = BillingEngine::new();
    let plans = engine.plans();
    let pro = plans.iter().find(|plan| plan.id == "tier-1000").unwrap();
    let mut reseller = pro.template("group-pro-plus");
    reseller.plan.as_mut().unwrap().paid_micro_cny = Some(20_000_000);
    let sold = Card::from_template("card-paid", "hash-paid", &reseller, None, 100);
    let listed = Card::from_template(
        "card-list",
        "hash-list",
        &pro.template("group-pro-plus"),
        None,
        100,
    );
    engine.upsert_cards_checked([sold, listed]).unwrap();
    engine
        .upgrade_card(billing::CardUpgrade {
            card_id: "card-paid",
            plan_id: "tier-5000",
            credits_delta: 4_000_000_000,
            cash_micro_cny: 100_000_000,
            group_id: None,
            extend_days: 0,
            operator_id: "admin",
            reason: "升级",
            now_secs: 200,
            idempotency_key: None,
        })
        .unwrap();
    let snapshot = engine.export_snapshot();
    let issued =
        billing::observability::cards_as_issued(snapshot.cards.values(), snapshot.ledger.iter());
    let sales = billing::observability::compute_sales(issued.iter(), &plans, Some(0), Some(300));
    assert_eq!(sales.issued_cards, 2);
    assert_eq!(sales.issued_value_micro_cny, 20_000_000 + 30_000_000);
    let pro_row = sales
        .by_plan
        .iter()
        .find(|row| row.plan_id == "tier-1000")
        .unwrap();
    assert_eq!(pro_row.issued_cards, 2);
    let cash = billing::observability::compute_cash(&sales, snapshot.ledger.iter());
    assert_eq!(
        (
            cash.sales_micro_cny,
            cash.upgrades_micro_cny,
            cash.refunds_micro_cny,
            cash.net_micro_cny
        ),
        (50_000_000, 100_000_000, 0, 150_000_000)
    );
    // The card itself is on the plan it was upgraded to.
    assert_eq!(
        engine.get_card("card-paid").unwrap().plan_id(),
        Some("tier-5000")
    );
}

/// A request that ended without completing, the client gone or the upstream's stream cut
/// short, and was charged, is counted as a charged interruption, with its credits, wherever
/// requests are counted, and never as a failure; one cut short and not charged still fails.
#[test]
fn charged_interruptions_are_counted_apart_from_failures() {
    let engine = BillingEngine::new();
    let now = 1_000_000;
    let trace = |id: &str, status: TraceStatus, class: Option<&str>, credits: i64| RequestTrace {
        id: id.into(),
        card_id: "card".into(),
        ts: now - 600,
        invocation_id: id.into(),
        exposed_model: "claude-opus-5-5".into(),
        status,
        error_class: class.map(str::to_string),
        provider_id: Some("hanyue".into()),
        credits_charged: credits,
        attempt_chain: vec![attempt("hanyue", "key-h", None)],
        ..RequestTrace::default()
    };
    // Kiro closed a stalled stream, after the request was charged.
    engine.record_trace(trace(
        "closed",
        TraceStatus::ClientAborted,
        Some("stream_incomplete"),
        5_670_000,
    ));
    // The upstream's stream ended before its end, and the output was charged.
    engine.record_trace(trace(
        "cut",
        TraceStatus::Error,
        Some("stream_incomplete"),
        1_000_000,
    ));
    // Cut short before any output: not charged, a failure.
    engine.record_trace(trace(
        "cut-free",
        TraceStatus::Error,
        Some("stream_incomplete"),
        0,
    ));
    // Closed before anything was charged: an interruption, not a charged one.
    engine.record_trace(trace("closed-free", TraceStatus::ClientAborted, None, 0));
    engine.record_trace(trace("ok", TraceStatus::Success, None, 2_000_000));

    let activity = engine.activity(now);
    let day = &activity.last_24h;
    assert_eq!(
        (
            day.requests,
            day.succeeded,
            day.failed,
            day.client_aborted,
            day.interrupted_charged,
            day.interrupted_charged_micro_credits
        ),
        (5, 1, 1, 2, 2, 6_670_000)
    );
    assert_eq!(
        (
            activity.last_7d.interrupted_charged,
            activity.last_7d.failed
        ),
        (2, 1)
    );
    let health = &activity.model_health[0].last_1h;
    assert_eq!(
        (
            health.requests,
            health.failures,
            health.interrupted_charged,
            health.interrupted_charged_micro_credits
        ),
        (5, 1, 2, 6_670_000)
    );
    let provider = &activity.providers[0];
    assert_eq!(
        (
            provider.requests,
            provider.failed,
            provider.interrupted_charged,
            provider.interrupted_charged_micro_credits
        ),
        (5, 1, 2, 6_670_000)
    );
    // Its serving attempts succeeded: no attempt failed, and the provider and the Key show
    // the charged interruptions.
    let attempts = &activity.provider_attempts[0].last_1h;
    assert_eq!(
        (
            attempts.attempts,
            attempts.failures,
            attempts.interrupted_charged,
            attempts.interrupted_charged_micro_credits
        ),
        (5, 0, 2, 6_670_000)
    );
    assert_eq!(activity.key_attempts[0].last_24h.interrupted_charged, 2);
    let hour = activity
        .hourly
        .iter()
        .find(|hour| hour.requests > 0)
        .unwrap();
    assert_eq!(
        (
            hour.failed,
            hour.interrupted_charged,
            hour.interrupted_charged_micro_credits
        ),
        (1, 2, 6_670_000)
    );
    // The trace list's totals count them too.
    let (_, totals) = engine.search_traces(&Default::default(), 10);
    assert_eq!(
        (
            totals.count,
            totals.failures,
            totals.interrupted_charged,
            totals.interrupted_charged_micro_credits
        ),
        (5, 2, 2, 6_670_000)
    );
}
