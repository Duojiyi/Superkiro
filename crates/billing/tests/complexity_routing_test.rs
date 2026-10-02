use billing::engine::{
    BillingEngine, BillingError, BillingSnapshot, Complexity, ComplexityRoutingConfig,
    ComplexityRoutingState, ComplexityRoutingUpdate, RoutingClassifier, RoutingDecision,
    RoutingMode, RoutingPolicy,
};
use billing::{Group, ModelMap, Provider, ProviderFormat};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

const DAY: u64 = 86_400;
const NOW: u64 = 20 * DAY + 100;

struct TestFile(PathBuf);
impl TestFile {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "billing-complexity-routing-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        std::fs::create_dir(&dir).unwrap();
        Self(dir)
    }
    fn path(&self) -> PathBuf {
        self.0.join("state.json")
    }
}
impl Drop for TestFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn engine() -> BillingEngine {
    let e = BillingEngine::new();
    e.upsert_group(Group::pro_plus("group", "Routing test"));
    for id in ["classifier", "simple", "complex", "outside"] {
        e.upsert_provider(Provider::new(
            id,
            id,
            ProviderFormat::OpenAi,
            "http://localhost.invalid",
        ));
    }
    e.upsert_model_map(
        ModelMap::new("model", "group", "visible", "complex", "upstream-complex")
            .with_fallback("simple", "upstream-simple"),
    );
    e
}

fn update(e: &BillingEngine) -> ComplexityRoutingUpdate {
    ComplexityRoutingUpdate {
        expected_revision: e.complexity_routing_config().revision,
        reason: "routing test publication".into(),
        classifier: Some(RoutingClassifier {
            provider_id: "classifier".into(),
            model: "classifier-model".into(),
            timeout_ms: 1000,
            max_input_chars: 4000,
            daily_request_limit: 10,
            daily_budget_micro_cny: 1000,
            input_price_micro_cny_per_million: 100_000,
            output_price_micro_cny_per_million: 300_000,
        }),
        policies: vec![RoutingPolicy {
            model_map_id: "model".into(),
            mode: RoutingMode::Enforce,
            simple_provider_ids: vec!["simple".into()],
            complex_provider_ids: vec!["complex".into()],
        }],
    }
}

fn configured() -> BillingEngine {
    let e = engine();
    e.publish_complexity_routing(update(&e), NOW).unwrap();
    e
}

fn decision(e: &BillingEngine, id: &str, pending: bool) -> RoutingDecision {
    RoutingDecision {
        invocation_id: id.into(),
        scope: "card:conversation:model".into(),
        request_hash: "a".repeat(64),
        revision: e.complexity_routing_config().revision,
        model_map_id: "model".into(),
        mode: RoutingMode::Enforce,
        complexity: Complexity::Unknown,
        reason: "default_route".into(),
        provider_ids: vec!["complex".into()],
        created_at_secs: NOW,
        pending,
        classifier_attempted: false,
        classifier_latency_ms: 0,
        input_tokens: 0,
        output_tokens: 0,
        classifier_cost_micro_cny: 0,
        usage_estimated: false,
        preview: false,
        served_provider_id: None,
    }
}

fn completed(mut d: RoutingDecision, cost: u64) -> RoutingDecision {
    d.pending = false;
    d.complexity = Complexity::Simple;
    d.reason = "semantic_simple".into();
    d.provider_ids = vec!["simple".into()];
    d.input_tokens = 100;
    d.output_tokens = 10;
    d.classifier_latency_ms = 20;
    d.classifier_cost_micro_cny = cost;
    d.usage_estimated = false;
    d
}

fn write_snapshot(file: &TestFile, snapshot: &BillingSnapshot) {
    std::fs::write(file.path(), serde_json::to_vec(snapshot).unwrap()).unwrap();
}

#[test]
fn legacy_snapshot_defaults_off_and_optional_decision_fields_default() {
    let e = engine();
    let mut value = serde_json::to_value(e.export_snapshot()).unwrap();
    value.as_object_mut().unwrap().remove("complexity_routing");
    let snapshot: BillingSnapshot = serde_json::from_value(value).unwrap();
    assert_eq!(
        snapshot.complexity_routing,
        ComplexityRoutingState::default()
    );
    let file = TestFile::new();
    write_snapshot(&file, &snapshot);
    let restored = BillingEngine::new();
    restored.load_from_file(file.path()).unwrap();
    assert_eq!(
        restored.complexity_routing_config(),
        ComplexityRoutingConfig::default()
    );
    assert_eq!(serde_json::to_string(&RoutingMode::Off).unwrap(), "\"off\"");
    assert_eq!(
        serde_json::to_string(&Complexity::Unknown).unwrap(),
        "\"unknown\""
    );
    let mut value = serde_json::to_value(decision(&e, "legacy", false)).unwrap();
    value.as_object_mut().unwrap().remove("preview");
    value.as_object_mut().unwrap().remove("served_provider_id");
    let d: RoutingDecision = serde_json::from_value(value).unwrap();
    assert!(!d.preview);
    assert!(d.served_provider_id.is_none());
}

#[test]
fn publication_conflicts_are_atomic_and_audit_is_bounded() {
    let e = engine();
    let first = update(&e);
    let published = e.publish_complexity_routing(first.clone(), NOW).unwrap();
    assert_ne!(first.expected_revision, published.revision);
    assert_eq!(
        published.audit[0].previous_revision,
        first.expected_revision
    );
    assert!(e.publish_complexity_routing(first, NOW + 1).is_err());
    assert_eq!(e.complexity_routing_config(), published);
    for n in 1..140 {
        e.publish_complexity_routing(update(&e), NOW + n).unwrap();
    }
    let cfg = e.complexity_routing_config();
    assert_eq!(cfg.audit.len(), 128);
    assert_eq!(cfg.audit.last().unwrap().revision, cfg.revision);
    let file = TestFile::new();
    e.save_to_file(file.path()).unwrap();
    let restarted = BillingEngine::new();
    restarted.load_from_file(file.path()).unwrap();
    assert_eq!(restarted.complexity_routing_config(), cfg);
}

#[test]
fn failed_publication_begin_finish_and_served_leave_memory_and_disk_unchanged() {
    let e = configured();
    let file = TestFile::new();
    e.save_to_file(file.path()).unwrap();
    let before = e.routing_status();
    e.inject_persistence_fault(true);
    assert!(matches!(
        e.publish_complexity_routing(update(&e), NOW + 1),
        Err(BillingError::Persistence(_))
    ));
    assert!(matches!(
        e.begin_routing_decision(decision(&e, "failure", true), 100, NOW),
        Err(BillingError::Persistence(_))
    ));
    assert_eq!(e.routing_status(), before);
    let restarted = BillingEngine::new();
    restarted.load_from_file(file.path()).unwrap();
    assert_eq!(restarted.routing_status(), before);
    e.inject_persistence_fault(false);
    let pending = e
        .begin_routing_decision(decision(&e, "failure", true), 100, NOW)
        .unwrap();
    let before = e.routing_status();
    e.inject_persistence_fault(true);
    assert!(matches!(
        e.finish_routing_decision(completed(pending.clone(), 20)),
        Err(BillingError::Persistence(_))
    ));
    assert_eq!(e.routing_status(), before);
    let restarted = BillingEngine::new();
    restarted.load_from_file(file.path()).unwrap();
    assert_eq!(restarted.routing_status(), before);
    e.inject_persistence_fault(false);
    e.finish_routing_decision(completed(pending, 20)).unwrap();
    let before = e.routing_status();
    e.inject_persistence_fault(true);
    assert!(matches!(
        e.record_routing_served("failure", "simple"),
        Err(BillingError::Persistence(_))
    ));
    assert_eq!(e.routing_status(), before);
    let restarted = BillingEngine::new();
    restarted.load_from_file(file.path()).unwrap();
    assert_eq!(restarted.routing_status(), before);
}

#[test]
fn budget_limit_is_durable_and_duplicate_begin_never_charges_again() {
    let e = engine();
    let mut u = update(&e);
    u.classifier.as_mut().unwrap().daily_request_limit = 1;
    e.publish_complexity_routing(u, NOW).unwrap();
    let first = e
        .begin_routing_decision(decision(&e, "first", true), 100, NOW)
        .unwrap();
    assert!(first.pending && first.classifier_attempted);
    assert_eq!(first.classifier_cost_micro_cny, 100);
    assert_eq!(
        e.begin_routing_decision(decision(&e, "first", true), 300, NOW + 1)
            .unwrap(),
        first
    );
    let mut next = decision(&e, "second", true);
    // The gateway may populate reservation intent before calling billing.
    next.classifier_attempted = true;
    next.classifier_cost_micro_cny = 100;
    next.usage_estimated = true;
    let denied = e
        .begin_routing_decision(next.clone(), 100, NOW + 1)
        .unwrap();
    assert!(!denied.pending && !denied.classifier_attempted && !denied.usage_estimated);
    assert_eq!(denied.reason, "budget_exhausted");
    assert_eq!(denied.provider_ids, next.provider_ids);
    assert_eq!(denied.classifier_cost_micro_cny, 0);
    assert_eq!(e.routing_status().budget.calls, 1);
    assert_eq!(e.routing_status().budget.cost_micro_cny, 100);
    assert_eq!(
        e.begin_routing_decision(next, 100, NOW + DAY).unwrap(),
        denied
    );
    assert_eq!(e.routing_status().budget.calls, 1);
    let file = TestFile::new();
    e.save_to_file(file.path()).unwrap();
    let restored = BillingEngine::new();
    restored.load_from_file(file.path()).unwrap();
    assert_eq!(restored.routing_status(), e.routing_status());
    assert_eq!(
        restored
            .begin_routing_decision(decision(&restored, "first", true), 200, NOW)
            .unwrap(),
        first
    );
}

#[test]
fn money_cap_and_preview_share_budget_without_new_charge_for_fallbacks() {
    let e = engine();
    let mut u = update(&e);
    u.classifier.as_mut().unwrap().daily_budget_micro_cny = 100;
    e.publish_complexity_routing(u, NOW).unwrap();
    let mut d = decision(&e, "preview", true);
    d.preview = true;
    e.begin_routing_decision(d, 80, NOW).unwrap();
    let d = e
        .begin_routing_decision(decision(&e, "user", true), 21, NOW + 1)
        .unwrap();
    assert_eq!(d.reason, "budget_exhausted");
    e.begin_routing_decision(decision(&e, "free", false), 0, NOW)
        .unwrap();
    assert_eq!(e.routing_status().budget.cost_micro_cny, 80);
    assert_eq!(e.routing_status().budget.calls, 1);
}

#[test]
fn final_cost_corrects_reservation_both_directions_and_final_decision_is_frozen() {
    let e = configured();
    let first = e
        .begin_routing_decision(decision(&e, "first", true), 100, NOW)
        .unwrap();
    let mut second = decision(&e, "second", true);
    second.classifier_attempted = true;
    second.classifier_cost_micro_cny = 100;
    second.usage_estimated = true;
    let second = e.begin_routing_decision(second, 100, NOW).unwrap();
    let first = e.finish_routing_decision(completed(first, 30)).unwrap();
    assert_eq!(e.routing_status().budget.cost_micro_cny, 130);
    e.finish_routing_decision(completed(second, 150)).unwrap();
    assert_eq!(e.routing_status().budget.cost_micro_cny, 180);
    assert_eq!(e.routing_status().budget.calls, 2);
    let mut changed = first.clone();
    changed.provider_ids = vec!["complex".into()];
    changed.classifier_cost_micro_cny = 999;
    assert_eq!(e.finish_routing_decision(changed).unwrap(), first);
    assert_eq!(e.routing_status().budget.cost_micro_cny, 180);
}

#[test]
fn failed_classifier_keeps_reservation_and_late_finish_does_not_touch_new_day() {
    let e = configured();
    let mut failed = e
        .begin_routing_decision(decision(&e, "failed", true), 100, NOW)
        .unwrap();
    failed.pending = false;
    failed.reason = "classifier_timeout".into();
    failed.classifier_cost_micro_cny = 0;
    failed.usage_estimated = false;
    let result = e.finish_routing_decision(failed).unwrap();
    assert_eq!(result.classifier_cost_micro_cny, 100);
    assert!(result.usage_estimated);
    let late = e
        .begin_routing_decision(decision(&e, "late", true), 100, NOW)
        .unwrap();
    e.begin_routing_decision(decision(&e, "new-day", true), 60, NOW + DAY)
        .unwrap();
    let budget = e.routing_status().budget;
    e.finish_routing_decision(completed(late, 500)).unwrap();
    assert_eq!(e.routing_status().budget, budget);
    assert_eq!(budget.calls, 1);
    assert_eq!(budget.cost_micro_cny, 60);
    assert!(e
        .begin_routing_decision(decision(&e, "backwards", true), 50, NOW)
        .is_err());
    assert_eq!(e.routing_status().budget, budget);
}

#[test]
fn configuration_changes_neither_reset_budget_nor_invalidate_inflight_finish() {
    let e = configured();
    let pending = e
        .begin_routing_decision(decision(&e, "old", true), 100, NOW)
        .unwrap();
    let budget = e.routing_status().budget;
    let mut u = update(&e);
    u.classifier.as_mut().unwrap().daily_budget_micro_cny = 50;
    e.publish_complexity_routing(u, NOW + 1).unwrap();
    assert_eq!(e.routing_status().budget, budget);
    assert_eq!(
        e.begin_routing_decision(decision(&e, "new", true), 1, NOW + 1)
            .unwrap()
            .reason,
        "budget_exhausted"
    );
    e.finish_routing_decision(completed(pending, 10)).unwrap();
    assert_eq!(e.routing_status().budget.cost_micro_cny, 10);
}

#[test]
fn restart_keeps_config_decisions_budget_and_confirmed_actual_provider() {
    let e = configured();
    let file = TestFile::new();
    e.save_to_file(file.path()).unwrap();
    let pending = e
        .begin_routing_decision(decision(&e, "served", true), 100, NOW)
        .unwrap();
    e.finish_routing_decision(completed(pending, 20)).unwrap();
    e.record_routing_served("served", "simple").unwrap();
    e.begin_routing_decision(decision(&e, "still-pending", true), 100, NOW + 1)
        .unwrap();
    let restored = BillingEngine::new();
    restored.load_from_file(file.path()).unwrap();
    assert_eq!(restored.routing_status(), e.routing_status());
    assert_eq!(
        restored
            .routing_decision("served")
            .unwrap()
            .served_provider_id
            .as_deref(),
        Some("simple")
    );
    assert!(restored.routing_decision("still-pending").unwrap().pending);
}

#[test]
fn latest_is_scope_isolated_recent_final_and_non_preview() {
    let e = configured();
    let a = e
        .begin_routing_decision(decision(&e, "a", false), 0, NOW)
        .unwrap();
    let mut b = decision(&e, "b", false);
    b.scope = "another-scope".into();
    let b = e.begin_routing_decision(b, 0, NOW + 1).unwrap();
    let mut preview = decision(&e, "preview", false);
    preview.preview = true;
    e.begin_routing_decision(preview, 0, NOW + 2).unwrap();
    e.begin_routing_decision(decision(&e, "pending", true), 100, NOW + 3)
        .unwrap();
    e.begin_routing_decision(decision(&e, "future", false), 0, NOW + DAY + 10)
        .unwrap();
    assert_eq!(
        e.latest_routing_decision(&a.scope, NOW + 4),
        Some(a.clone())
    );
    assert_eq!(e.latest_routing_decision(&b.scope, NOW + 4), Some(b));
    assert!(e.latest_routing_decision("absent", NOW + 4).is_none());
    assert!(e.latest_routing_decision(&a.scope, NOW + DAY).is_none());
    let mut mismatch = decision(&e, "a", false);
    mismatch.scope = "another-scope".into();
    assert!(e.begin_routing_decision(mismatch, 0, NOW).is_err());
}

#[test]
fn observe_keeps_suggested_chain_and_actual_provider_can_use_original_model_chain() {
    let e = engine();
    let mut u = update(&e);
    u.policies[0].mode = RoutingMode::Observe;
    e.publish_complexity_routing(u, NOW).unwrap();
    let mut d = decision(&e, "observe", true);
    d.mode = RoutingMode::Observe;
    let pending = e.begin_routing_decision(d, 100, NOW).unwrap();
    assert_eq!(pending.provider_ids, vec!["complex"]);
    assert!(e.record_routing_served("observe", "complex").is_err());
    let final_d = e.finish_routing_decision(completed(pending, 20)).unwrap();
    assert_eq!(final_d.provider_ids, vec!["simple"]);
    assert_eq!(final_d.complexity, Complexity::Simple);
    assert!(e.record_routing_served("observe", "outside").is_err());
    e.record_routing_served("observe", "complex").unwrap();
    e.record_routing_served("observe", "complex").unwrap();
    assert!(e.record_routing_served("observe", "simple").is_err());
    let recorded = e.routing_decision("observe").unwrap();
    assert_eq!(recorded.served_provider_id.as_deref(), Some("complex"));
    assert_eq!(recorded.provider_ids, final_d.provider_ids);
    assert_eq!(recorded.complexity, final_d.complexity);
    assert_eq!(recorded.revision, final_d.revision);
    let file = TestFile::new();
    e.save_to_file(file.path()).unwrap();
    let restored = BillingEngine::new();
    restored.load_from_file(file.path()).unwrap();
    assert_eq!(restored.routing_status(), e.routing_status());
}

#[test]
fn changed_finish_identity_is_rejected_without_budget_change() {
    let e = configured();
    let pending = e
        .begin_routing_decision(decision(&e, "pending", true), 100, NOW)
        .unwrap();
    let before = e.routing_status();
    let changes: Vec<fn(&mut RoutingDecision)> = vec![
        |d| d.scope = "other".into(),
        |d| d.request_hash = "other".into(),
        |d| d.revision = "complexity-routing:v1:other".into(),
        |d| d.model_map_id = "other".into(),
        |d| d.preview = true,
        |d| d.mode = RoutingMode::Off,
        |d| d.created_at_secs += 1,
        |d| d.served_provider_id = Some("simple".into()),
    ];
    for change in changes {
        let mut d = completed(pending.clone(), 20);
        change(&mut d);
        assert!(e.finish_routing_decision(d).is_err());
        assert_eq!(e.routing_status(), before);
    }
}

#[test]
fn configuration_bounds_and_unknown_fields_are_rejected() {
    let e = engine();
    let mutations: Vec<fn(&mut ComplexityRoutingUpdate)> = vec![
        |u| u.reason = " ".into(),
        |u| u.reason = "x".repeat(1025),
        |u| u.classifier.as_mut().unwrap().timeout_ms = 199,
        |u| u.classifier.as_mut().unwrap().timeout_ms = 5001,
        |u| u.classifier.as_mut().unwrap().max_input_chars = 255,
        |u| u.classifier.as_mut().unwrap().max_input_chars = 16001,
        |u| u.classifier.as_mut().unwrap().daily_request_limit = 0,
        |u| u.classifier.as_mut().unwrap().daily_request_limit = 100001,
        |u| u.classifier.as_mut().unwrap().daily_budget_micro_cny = 0,
        |u| u.classifier.as_mut().unwrap().daily_budget_micro_cny = u64::MAX,
        |u| {
            u.classifier
                .as_mut()
                .unwrap()
                .input_price_micro_cny_per_million = 0
        },
        |u| {
            u.classifier
                .as_mut()
                .unwrap()
                .output_price_micro_cny_per_million = u64::MAX
        },
        |u| u.classifier.as_mut().unwrap().model = "bad\nmodel".into(),
        |u| u.classifier = None,
        |u| u.policies[0].simple_provider_ids.clear(),
        |u| u.policies[0].complex_provider_ids.clear(),
        |u| u.policies[0].simple_provider_ids = vec!["simple".into(); 2],
        |u| u.policies[0].simple_provider_ids = (0..17).map(|n| n.to_string()).collect(),
        |u| u.policies.push(u.policies[0].clone()),
        |u| u.policies = vec![u.policies[0].clone(); 257],
    ];
    let before = e.routing_status();
    for change in mutations {
        let mut u = update(&e);
        change(&mut u);
        assert!(e.publish_complexity_routing(u, NOW).is_err());
        assert_eq!(e.routing_status(), before);
    }
    let mut json = serde_json::to_value(update(&e)).unwrap();
    json["unexpected"] = true.into();
    assert!(serde_json::from_value::<ComplexityRoutingUpdate>(json).is_err());
    let mut u = update(&e);
    u.policies[0].mode = RoutingMode::Off;
    u.classifier = None;
    assert!(e.publish_complexity_routing(u, NOW).is_ok());
}

#[test]
fn policy_references_enforce_membership_permissions_and_unambiguous_targets() {
    let e = engine();
    let mut u = update(&e);
    u.policies[0].simple_provider_ids = vec!["outside".into()];
    assert!(e.publish_complexity_routing(u, NOW).is_err());
    let mut u = update(&e);
    u.policies[0].model_map_id = "missing".into();
    assert!(e.publish_complexity_routing(u, NOW).is_err());
    let mut u = update(&e);
    u.classifier.as_mut().unwrap().provider_id = "missing".into();
    assert!(e.publish_complexity_routing(u, NOW).is_err());
    e.set_provider_enabled("simple", false).unwrap();
    assert!(e.publish_complexity_routing(update(&e), NOW).is_err());
    e.set_provider_enabled("simple", true).unwrap();
    e.set_provider_enabled("classifier", false).unwrap();
    assert!(e.publish_complexity_routing(update(&e), NOW).is_err());
    e.set_provider_enabled("classifier", true).unwrap();
    let mut simple = e.get_provider("simple").unwrap();
    simple.group_id = Some("other".into());
    e.upsert_provider(simple.clone());
    assert!(e.publish_complexity_routing(update(&e), NOW).is_err());
    simple.group_id = None;
    e.upsert_provider(simple);
    let mut classifier = e.get_provider("classifier").unwrap();
    classifier.group_id = Some("other".into());
    e.upsert_provider(classifier.clone());
    assert!(e.publish_complexity_routing(update(&e), NOW).is_err());
    classifier.group_id = None;
    e.upsert_provider(classifier);
    e.upsert_model_map(
        ModelMap::new("model", "group", "visible", "complex", "first")
            .with_fallback("complex", "second")
            .with_fallback("simple", "third"),
    );
    assert!(e.publish_complexity_routing(update(&e), NOW).is_err());
    e.upsert_model_map(
        ModelMap::new("model", "group", "visible", "complex", "first")
            .with_fallback("simple", "third"),
    );
    assert!(e.publish_complexity_routing(update(&e), NOW).is_ok());
}

#[test]
fn restore_validates_metadata_but_allows_deleted_historical_providers() {
    let e = configured();
    let d = e
        .begin_routing_decision(decision(&e, "historical", true), 100, NOW)
        .unwrap();
    e.finish_routing_decision(completed(d, 20)).unwrap();
    let mut snapshot = e.export_snapshot();
    snapshot.providers.clear();
    snapshot.model_maps.clear();
    let file = TestFile::new();
    write_snapshot(&file, &snapshot);
    let restored = BillingEngine::new();
    restored.load_from_file(file.path()).unwrap();
    assert_eq!(restored.routing_status(), e.routing_status());
    assert!(restored
        .publish_complexity_routing(update(&restored), NOW)
        .is_err());
    let changes: Vec<fn(&mut ComplexityRoutingState)> = vec![
        |s| s.decisions[0].scope.clear(),
        |s| s.decisions[0].invocation_id = "x".repeat(258),
        |s| s.decisions[0].request_hash = "bad\nvalue".into(),
        |s| s.decisions[0].reason = "x".repeat(1025),
        |s| s.decisions[0].input_tokens = u64::MAX,
        |s| s.decisions[0].provider_ids.clear(),
        |s| s.decisions.push(s.decisions[0].clone()),
        |s| s.decisions[0].served_provider_id = Some("outside".into()),
        |s| s.budget.cost_micro_cny = 0,
        |s| s.budget.calls = 100_001,
        |s| s.config.audit[0].revision = "other".into(),
    ];
    for change in changes {
        let mut bad = snapshot.clone();
        change(&mut bad.complexity_routing);
        let file = TestFile::new();
        write_snapshot(&file, &bad);
        let reader = BillingEngine::new();
        assert!(reader.load_from_file(file.path()).is_err());
        assert_eq!(reader.routing_status(), ComplexityRoutingState::default());
    }
}

#[test]
fn expired_pending_and_final_are_pruned_but_full_unexpired_capacity_is_not_evicted() {
    let e = configured();
    let pending = e
        .begin_routing_decision(decision(&e, "expired-pending", true), 100, NOW)
        .unwrap();
    e.begin_routing_decision(decision(&e, "expired-final", false), 0, NOW)
        .unwrap();
    e.begin_routing_decision(decision(&e, "fresh", false), 0, NOW + 7 * DAY + 1)
        .unwrap();
    assert!(e.routing_decision("expired-pending").is_none());
    assert!(e.routing_decision("expired-final").is_none());
    assert!(e.finish_routing_decision(completed(pending, 20)).is_err());
    let mut snapshot = e.export_snapshot();
    let prototype = snapshot.complexity_routing.decisions[0].clone();
    snapshot.complexity_routing.decisions = (0..20_000)
        .map(|i| RoutingDecision {
            invocation_id: format!("capacity-{i}"),
            ..prototype.clone()
        })
        .collect();
    let file = TestFile::new();
    write_snapshot(&file, &snapshot);
    let full = BillingEngine::new();
    full.load_from_file(file.path()).unwrap();
    let before = full.routing_status();
    let (fallback, claimed) = full
        .begin_routing_decision_with_claim(
            decision(&full, "over-capacity", false),
            0,
            NOW + 7 * DAY + 2,
        )
        .unwrap();
    assert_eq!(fallback.reason, "decision_capacity_exhausted");
    assert_eq!(fallback.provider_ids, ["complex"]);
    assert!(!claimed && !fallback.pending && !fallback.classifier_attempted);
    assert!(full.routing_decision("over-capacity").is_none());
    assert_eq!(full.routing_status(), before);
    assert!(full
        .begin_routing_decision(decision(&full, "capacity-0", false), 0, NOW + 7 * DAY + 2)
        .is_ok());
    let mut bad = snapshot;
    bad.complexity_routing.decisions.push(RoutingDecision {
        invocation_id: "too-many".into(),
        ..prototype
    });
    let file = TestFile::new();
    write_snapshot(&file, &bad);
    assert!(BillingEngine::new().load_from_file(file.path()).is_err());
}

#[test]
fn full_capacity_returns_unclaimed_fallback_without_save_charge_or_eviction() {
    let e = configured();
    let classified = e
        .begin_routing_decision(decision(&e, "classified", true), 100, NOW)
        .unwrap();
    e.finish_routing_decision(completed(classified, 20))
        .unwrap();
    e.record_routing_served("classified", "simple").unwrap();
    let classified = e.routing_decision("classified").unwrap();
    let pending = e
        .begin_routing_decision(decision(&e, "pending", true), 100, NOW)
        .unwrap();
    let mut snapshot = e.export_snapshot();
    let prototype = decision(&e, "retained", false);
    snapshot
        .complexity_routing
        .decisions
        .extend((2..20_000).map(|i| RoutingDecision {
            invocation_id: format!("retained-{i}"),
            ..prototype.clone()
        }));
    let file = TestFile::new();
    write_snapshot(&file, &snapshot);
    let full = BillingEngine::new();
    full.load_from_file(file.path()).unwrap();
    let before = full.export_snapshot();
    let bytes = std::fs::read(file.path()).unwrap();
    full.inject_persistence_fault(true);

    for (id, is_pending, empty, mode, preview) in [
        (
            "new-classification",
            true,
            false,
            RoutingMode::Enforce,
            false,
        ),
        ("new-preview", true, false, RoutingMode::Enforce, true),
        ("new-complex", false, false, RoutingMode::Enforce, false),
        ("no-route", false, true, RoutingMode::Enforce, false),
        ("observe-no-route", false, true, RoutingMode::Observe, false),
    ] {
        let mut d = decision(&full, id, is_pending);
        d.mode = mode;
        d.preview = preview;
        d.classifier_cost_micro_cny = if is_pending { 100 } else { 0 };
        d.usage_estimated = is_pending;
        if empty {
            d.provider_ids.clear();
            d.reason = "no_eligible_route".into();
        }
        let mut expected = d.clone();
        expected.reason = "decision_capacity_exhausted".into();
        expected.created_at_secs = NOW + 1;
        expected.pending = false;
        expected.classifier_cost_micro_cny = 0;
        expected.usage_estimated = false;
        let (fallback, claimed) = full
            .begin_routing_decision_with_claim(d, if is_pending { 100 } else { 0 }, NOW + 1)
            .unwrap();
        assert_eq!(fallback, expected);
        assert!(!claimed);
        assert!(full.routing_decision(id).is_none());
        full.record_routing_served(id, "complex").unwrap();
    }
    // Replays precede current revision and new-decision validation, even at capacity.
    for original in [classified.clone(), pending.clone()] {
        let mut d = decision(&full, &original.invocation_id, true);
        d.revision = "complexity-routing:v1:stale".into();
        let (replay, claimed) = full
            .begin_routing_decision_with_claim(d, 100, NOW + 1)
            .unwrap();
        assert_eq!(replay, original);
        assert!(!claimed);
    }
    let changes: &[fn(&mut RoutingDecision)] = &[
        |d| d.reason.clear(),
        |d| d.request_hash = "bad\nvalue".into(),
        |d| d.model_map_id = "missing".into(),
        |d| d.provider_ids = vec!["outside".into()],
        |d| d.provider_ids = vec!["complex".into(); 2],
        |d| d.provider_ids.clear(),
        |d| d.mode = RoutingMode::Off,
        |d| d.input_tokens = 1,
        |d| d.served_provider_id = Some("complex".into()),
    ];
    for change in changes {
        let mut d = decision(&full, "invalid-new", true);
        change(&mut d);
        assert!(full
            .begin_routing_decision_with_claim(d, 100, NOW + 1)
            .is_err());
    }
    let mut mismatch = decision(&full, "classified", true);
    mismatch.request_hash = "different".into();
    assert!(full
        .begin_routing_decision_with_claim(mismatch, 100, NOW + 1)
        .is_err());
    assert_eq!(full.routing_status(), before.complexity_routing);
    assert_eq!(full.export_snapshot().sequence, before.sequence);
    assert_eq!(std::fs::read(file.path()).unwrap(), bytes);
    let restored = BillingEngine::new();
    restored.load_from_file(file.path()).unwrap();
    assert_eq!(restored.routing_status(), before.complexity_routing);

    // Once retention frees capacity, unrelated persistence failures still propagate.
    assert!(matches!(
        full.begin_routing_decision_with_claim(
            decision(&full, "after-retention", true),
            100,
            NOW + 7 * DAY + 1,
        ),
        Err(BillingError::Persistence(_))
    ));
    assert_eq!(full.routing_status(), before.complexity_routing);
    assert_eq!(full.export_snapshot().sequence, before.sequence);
    assert_eq!(std::fs::read(file.path()).unwrap(), bytes);
}

#[test]
fn empty_terminal_chains_allow_no_route_but_preserve_served_provider_checks() {
    for mode in [RoutingMode::Off, RoutingMode::Observe, RoutingMode::Enforce] {
        let e = engine();
        let mut u = update(&e);
        u.policies[0].mode = mode;
        e.publish_complexity_routing(u, NOW).unwrap();
        let before = e.routing_status().budget;
        let mut d = decision(&e, "empty-terminal", false);
        d.mode = mode;
        d.reason = "no_eligible_route".into();
        d.provider_ids.clear();
        let (saved, claimed) = e
            .begin_routing_decision_with_claim(d.clone(), 0, NOW)
            .unwrap();
        assert_eq!(saved, d);
        assert!(!claimed);
        assert_eq!(e.routing_status().budget, before);
        assert!(e
            .record_routing_served("empty-terminal", "bad\nprovider")
            .is_err());
        assert!(e
            .record_routing_served("empty-terminal", "outside")
            .is_err());
        if mode == RoutingMode::Observe {
            e.set_provider_enabled("simple", false).unwrap();
            assert!(e.record_routing_served("empty-terminal", "simple").is_err());
            e.set_provider_enabled("simple", true).unwrap();
            e.record_routing_served("empty-terminal", "simple").unwrap();
        } else {
            assert!(e
                .record_routing_served("empty-terminal", "complex")
                .is_err());
        }
        let file = TestFile::new();
        e.save_to_file(file.path()).unwrap();
        let restored = BillingEngine::new();
        restored.load_from_file(file.path()).unwrap();
        assert_eq!(restored.routing_status(), e.routing_status());
        let before = e.routing_status();
        // Empty pending chains must fail even if the budget would otherwise end the attempt.
        for reserved in [100, 1001] {
            let mut pending = d.clone();
            pending.invocation_id = "empty-pending".into();
            pending.pending = true;
            pending.preview = true;
            assert!(e
                .begin_routing_decision_with_claim(pending, reserved, NOW)
                .is_err());
        }
        let mut invalid = d.clone();
        invalid.invocation_id = "empty-invalid".into();
        invalid.model_map_id = "missing".into();
        assert!(e.begin_routing_decision(invalid, 0, NOW).is_err());
        assert_eq!(e.routing_status(), before);
        for (pending, attempted) in [(false, true), (true, false), (true, true)] {
            let mut snapshot = e.export_snapshot();
            snapshot.complexity_routing.decisions[0].pending = pending;
            snapshot.complexity_routing.decisions[0].classifier_attempted = attempted;
            snapshot.complexity_routing.decisions[0].served_provider_id = None;
            let file = TestFile::new();
            write_snapshot(&file, &snapshot);
            assert!(BillingEngine::new().load_from_file(file.path()).is_err());
        }
        let mut invalid = e.export_snapshot();
        invalid.complexity_routing.decisions[0].served_provider_id = Some("bad\nprovider".into());
        let file = TestFile::new();
        write_snapshot(&file, &invalid);
        assert!(BillingEngine::new().load_from_file(file.path()).is_err());
    }
}

#[test]
fn empty_policy_chains_remain_invalid_in_every_mode() {
    let e = engine();
    for mode in [RoutingMode::Off, RoutingMode::Observe, RoutingMode::Enforce] {
        for simple in [false, true] {
            let mut u = update(&e);
            u.policies[0].mode = mode;
            if simple {
                u.policies[0].simple_provider_ids.clear();
            } else {
                u.policies[0].complex_provider_ids.clear();
            }
            assert!(e.publish_complexity_routing(u, NOW).is_err());
        }
    }
    assert_eq!(e.routing_status(), ComplexityRoutingState::default());
}

#[test]
fn concurrent_duplicate_begins_reserve_only_once() {
    let e = configured();
    let d = decision(&e, "concurrent", true);
    let barrier = std::sync::Barrier::new(8);
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..8)
            .map(|_| {
                scope.spawn(|| {
                    barrier.wait();
                    e.begin_routing_decision(d.clone(), 100, NOW).unwrap()
                })
            })
            .collect();
        for handle in handles {
            assert!(handle.join().unwrap().pending);
        }
    });
    assert_eq!(e.routing_status().decisions.len(), 1);
    assert_eq!(e.routing_status().budget.calls, 1);
    assert_eq!(e.routing_status().budget.cost_micro_cny, 100);
}

#[test]
fn ordinary_served_record_is_noop_even_when_persistence_is_unavailable() {
    let e = configured();
    let file = TestFile::new();
    e.save_to_file(file.path()).unwrap();
    let before = e.export_snapshot();
    let bytes = std::fs::read(file.path()).unwrap();
    e.inject_persistence_fault(true);
    e.record_routing_served("ordinary-untracked", "any-provider")
        .unwrap();
    assert_eq!(e.export_snapshot().sequence, before.sequence);
    assert_eq!(e.routing_status(), before.complexity_routing);
    assert_eq!(std::fs::read(file.path()).unwrap(), bytes);
}

#[test]
fn continuation_may_narrow_policy_chain_but_not_expand_current_permissions() {
    let e = configured();
    let mut d = decision(&e, "continuation", false);
    d.provider_ids = vec!["simple".into()];
    d.complexity = Complexity::Simple;
    assert!(e.begin_routing_decision(d, 0, NOW).is_ok());
    let mut d = decision(&e, "outside", false);
    d.provider_ids = vec!["outside".into()];
    assert!(e.begin_routing_decision(d, 0, NOW).is_err());
    let mut p = e.get_provider("simple").unwrap();
    p.group_id = Some("another-group".into());
    e.upsert_provider(p);
    let mut d = decision(&e, "revoked", false);
    d.provider_ids = vec!["simple".into()];
    assert!(e.begin_routing_decision(d, 0, NOW).is_err());
}

#[test]
fn concurrent_classifier_claim_has_one_winner_and_restart_cannot_reclaim_pending() {
    let e = configured();
    let file = TestFile::new();
    e.save_to_file(file.path()).unwrap();
    let d = decision(&e, "claim", true);
    let barrier = std::sync::Barrier::new(8);
    let claims = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..8)
            .map(|_| {
                scope.spawn(|| {
                    barrier.wait();
                    e.begin_routing_decision_with_claim(d.clone(), 100, NOW)
                        .unwrap()
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| {
                let (d, claimed) = h.join().unwrap();
                assert!(d.pending);
                usize::from(claimed)
            })
            .sum::<usize>()
    });
    assert_eq!(claims, 1);
    assert_eq!(e.routing_status().budget.calls, 1);
    let restored = BillingEngine::new();
    restored.load_from_file(file.path()).unwrap();
    let (replay, claimed) = restored
        .begin_routing_decision_with_claim(d, 100, NOW)
        .unwrap();
    assert!(replay.pending);
    assert!(!claimed);
    let (_, claimed) = restored
        .begin_routing_decision_with_claim(decision(&restored, "no-call", false), 0, NOW)
        .unwrap();
    assert!(!claimed);
    let (denied, claimed) = restored
        .begin_routing_decision_with_claim(decision(&restored, "over-budget", true), 1000, NOW)
        .unwrap();
    assert!(!claimed);
    assert_eq!(denied.reason, "budget_exhausted");
}

#[test]
fn unrelated_stale_policies_block_publication_but_not_current_classification() {
    let e = engine();
    e.upsert_group(Group::pro_plus("other-group", "Unrelated"));
    e.upsert_model_map(ModelMap::new(
        "other-model",
        "other-group",
        "other-visible",
        "outside",
        "other-upstream",
    ));
    let mut u = update(&e);
    u.policies.push(RoutingPolicy {
        model_map_id: "other-model".into(),
        mode: RoutingMode::Enforce,
        simple_provider_ids: vec!["outside".into()],
        complex_provider_ids: vec!["outside".into()],
    });
    e.publish_complexity_routing(u, NOW).unwrap();
    let original = e.export_snapshot();
    let changes: &[fn(&mut BillingSnapshot)] = &[
        |s| s.providers.get_mut("outside").unwrap().enabled = false,
        |s| {
            s.model_maps.retain(|m| m.id != "other-model");
            s.providers.remove("outside");
        },
        |s| s.model_maps.retain(|m| m.id != "other-model"),
        |s| {
            s.groups.remove("other-group");
        },
        |s| {
            s.groups
                .get_mut("other-group")
                .unwrap()
                .provider_binding_mode = billing::ProviderBindingMode::Dedicated
        },
        // Even an unused branch of the current policy must not block this valid branch.
        |s| s.providers.get_mut("simple").unwrap().enabled = false,
    ];
    for change in changes {
        let current = BillingEngine::new();
        let mut snapshot = original.clone();
        change(&mut snapshot);
        current.import_snapshot(snapshot);
        let cfg = current.complexity_routing_config();
        let republish = ComplexityRoutingUpdate {
            expected_revision: cfg.revision.clone(),
            reason: "validate every published reference".into(),
            classifier: cfg.classifier.clone(),
            policies: cfg.policies.clone(),
        };
        assert!(current
            .publish_complexity_routing(republish, NOW + 1)
            .is_err());
        assert_eq!(current.complexity_routing_config(), cfg);
        let (started, claimed) = current
            .begin_routing_decision_with_claim(decision(&current, "unaffected", true), 100, NOW + 1)
            .unwrap();
        assert!(claimed && started.pending && started.classifier_attempted);
        assert_eq!(current.routing_status().budget.calls, 1);
        assert_eq!(current.routing_status().budget.cost_micro_cny, 100);
    }
}

#[test]
fn runtime_still_checks_current_classifier_existence_enabled_and_group_access() {
    let seed = configured().export_snapshot();
    let changes: &[fn(&mut BillingSnapshot)] = &[
        |s| {
            s.providers.remove("classifier");
        },
        |s| s.providers.get_mut("classifier").unwrap().enabled = false,
        |s| s.providers.get_mut("classifier").unwrap().group_id = Some("other-group".into()),
    ];
    for change in changes {
        for preview in [false, true] {
            let e = BillingEngine::new();
            let mut snapshot = seed.clone();
            change(&mut snapshot);
            e.import_snapshot(snapshot);
            let before = e.routing_status();
            let mut d = decision(&e, "blocked-classifier", true);
            d.preview = preview;
            assert!(e.begin_routing_decision_with_claim(d, 100, NOW).is_err());
            assert_eq!(e.routing_status(), before);
        }
    }
}

#[test]
fn runtime_still_requires_current_model_group_and_matching_enabled_user_policy() {
    let seed = configured().export_snapshot();
    let changes: &[fn(&mut BillingSnapshot)] = &[
        |s| {
            s.groups.remove("group");
        },
        |s| s.model_maps.clear(),
        |s| s.model_maps[0].retired = true,
        |s| s.complexity_routing.config.policies.clear(),
        |s| s.complexity_routing.config.policies[0].mode = RoutingMode::Off,
        |s| s.complexity_routing.config.policies[0].mode = RoutingMode::Observe,
    ];
    for change in changes {
        let e = BillingEngine::new();
        let mut snapshot = seed.clone();
        change(&mut snapshot);
        e.import_snapshot(snapshot);
        let before = e.routing_status();
        assert!(e
            .begin_routing_decision_with_claim(decision(&e, "blocked-current", true), 100, NOW)
            .is_err());
        assert_eq!(e.routing_status(), before);
    }
}

#[test]
fn emergency_shutdown_remains_available_with_stale_live_references() {
    let e = configured();
    for id in ["classifier", "simple", "complex"] {
        let mut provider = e.get_provider(id).unwrap();
        provider.enabled = false;
        e.upsert_provider(provider);
    }
    assert!(e.publish_complexity_routing(update(&e), NOW + 1).is_err());
    let mut shutdown = update(&e);
    shutdown.policies[0].mode = RoutingMode::Off;
    let saved = e.publish_complexity_routing(shutdown, NOW + 2).unwrap();
    assert_eq!(saved.policies[0].mode, RoutingMode::Off);
    assert!(e.publish_complexity_routing(update(&e), NOW + 3).is_err());
}
