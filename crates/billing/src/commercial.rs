//! Validated atomic publication. No secrets are exposed by this view.
use super::*;
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommercialAudit {
    pub previous_revision: String,
    pub revision: String,
    pub reason: String,
    pub operator: String,
    pub created_at_secs: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommercialUpdate {
    pub expected_revision: String,
    pub reason: String,
    #[serde(default)]
    pub settings: Option<BillingSettings>,
    #[serde(default)]
    pub groups: Vec<Group>,
    #[serde(default)]
    pub models: Vec<ModelMap>,
    #[serde(default)]
    pub rate_cards: Vec<RateCard>,
    #[serde(default)]
    pub versions: Vec<RateCardVersion>,
}
#[derive(Debug, Serialize)]
pub struct CommercialConfig {
    pub revision: String,
    pub settings: BillingSettings,
    pub audit: Vec<CommercialAudit>,
    pub groups: Vec<Group>,
    pub models: Vec<ModelMap>,
    pub rate_cards: Vec<RateCard>,
    pub versions: Vec<RateCardVersion>,
}
fn view(s: &BillingSnapshot) -> CommercialConfig {
    let mut groups: Vec<_> = s.groups.values().cloned().collect();
    groups.sort_by(|a, b| a.id.cmp(&b.id));
    let mut models = s.model_maps.clone();
    models.sort_by(|a, b| a.id.cmp(&b.id));
    let mut rate_cards: Vec<_> = s.rate_cards.values().cloned().collect();
    rate_cards.sort_by(|a, b| a.id.cmp(&b.id));
    let mut versions = s.rate_card_versions.clone();
    versions.sort_by(|a, b| a.id.cmp(&b.id));
    let bytes =
        serde_json::to_vec(&(&groups, &models, &rate_cards, &versions, &s.settings)).unwrap();
    let revision = ring::digest::digest(&ring::digest::SHA256, &bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    CommercialConfig {
        revision,
        settings: s.settings.clone(),
        audit: s.commercial_audit_logs.clone(),
        groups,
        models,
        rate_cards,
        versions,
    }
}
fn text(s: &str, max: usize) -> bool {
    !s.trim().is_empty() && s.len() <= max && !s.chars().any(char::is_control)
}
fn positive(n: f64) -> bool {
    n.is_finite() && n > 0.0 && n <= 1000.0
}
fn invalid(s: &str) -> BillingError {
    BillingError::InvalidState(s.into())
}
impl BillingEngine {
    pub fn commercial_config(&self) -> CommercialConfig {
        view(&self.export_snapshot())
    }
    pub fn publish_commercial_config(
        &self,
        u: CommercialUpdate,
        now: u64,
    ) -> Result<CommercialConfig, BillingError> {
        if !text(&u.reason, 500) {
            return Err(invalid("Publication reason required (max 500 bytes)"));
        }
        if u.groups.len() + u.models.len() + u.rate_cards.len() + u.versions.len() == 0
            && u.settings.is_none()
        {
            return Err(invalid("Empty publication"));
        }
        let _guard = self.state_lock.write().unwrap();
        let mut c = self.export_snapshot_locked(
            self.snapshot_sequence
                .load(Ordering::Acquire)
                .saturating_add(1),
            self.last_snapshot_checksum.read().unwrap().clone(),
        );
        if view(&c).revision != u.expected_revision {
            return Err(invalid("Configuration changed; reload before publishing"));
        }
        if c.reservations
            .values()
            .any(|r| r.state == ReservationState::Held)
            || !c.pending_settlements.is_empty()
        {
            return Err(invalid("Requests are still settling; publish when idle"));
        }
        if let Some(mut settings) = u.settings {
            if !positive(settings.credit_face_value_cny) || !positive(settings.usd_cny_rate) {
                return Err(invalid(
                    "Face value and exchange rate must be finite, positive, and at most 1000",
                ));
            }
            settings.rate_updated_at_secs = now;
            c.settings = settings;
        }
        let mut ids = std::collections::HashSet::new();
        for r in u.rate_cards {
            if !text(&r.id, 128) || !text(&r.name, 256) || !ids.insert(("rate", r.id.clone())) {
                return Err(invalid("Invalid or duplicate rate card"));
            }
            c.rate_cards.insert(r.id.clone(), r);
        }
        for g in u.groups {
            if !text(&g.id, 128)
                || !text(&g.name, 256)
                || !text(&g.virtual_plan_name, 256)
                || !positive(g.margin_multiplier)
                || !g.virtual_usage_limit.is_finite()
                || g.virtual_usage_limit < 0.0
                || g.system_prompt_prefix
                    .as_ref()
                    .is_some_and(|s| s.len() > 16000)
                || !c.rate_cards.contains_key(&g.rate_card_id)
                || !ids.insert(("group", g.id.clone()))
            {
                return Err(invalid("Invalid group or unknown rate card"));
            }
            c.groups.insert(g.id.clone(), g);
        }
        for m in u.models {
            if !text(&m.id, 128)
                || !text(&m.exposed_model_id, 128)
                || !text(&m.target_model, 256)
                || !positive(m.credit_multiplier)
                || m.max_output == 0
                || m.max_output > m.context_window
                || m.context_window > 10_000_000
                || m.aliases.len() > 32
                || m.aliases.iter().any(|a| !text(a, 128))
                || m.fallback_chain.len() > 8
                || !ids.insert(("map", m.id.clone()))
            {
                return Err(invalid("Invalid or duplicate model mapping"));
            }
            if let Some(old) = c.model_maps.iter_mut().find(|v| v.id == m.id) {
                if old.group_id != m.group_id {
                    return Err(invalid("Cannot move mapping between groups"));
                }
                *old = m;
            } else {
                c.model_maps.push(m);
            }
        }
        let mut exposed = std::collections::HashSet::new();
        for m in &c.model_maps {
            let g = c
                .groups
                .get(&m.group_id)
                .ok_or_else(|| invalid("Unknown model group"))?;
            for (id, model) in std::iter::once((&m.target_provider_id, &m.target_model)).chain(
                m.fallback_chain
                    .iter()
                    .map(|f| (&f.provider_id, &f.target_model)),
            ) {
                let p = c
                    .providers
                    .get(id)
                    .ok_or_else(|| invalid("Unknown target provider"))?;
                if m.visible
                    && (!p.enabled
                        || !c
                            .provider_keys
                            .values()
                            .any(|k| k.provider_id == *id && k.enabled && k.supports_model(model)))
                {
                    return Err(invalid(
                        "Visible model target has no enabled compatible key",
                    ));
                }
                if !text(model, 256) || !g.can_access_provider(p.group_id.as_deref()) {
                    return Err(invalid("Provider not accessible to model group"));
                }
            }
            for name in std::iter::once(&m.exposed_model_id).chain(m.aliases.iter()) {
                if !exposed.insert((&m.group_id, name)) {
                    return Err(invalid("Ambiguous model ID or alias"));
                }
            }
        }
        for v in u.versions {
            let prices = [
                v.input_price_per_m,
                v.output_price_per_m,
                v.cache_creation_price_per_m,
                v.cache_read_price_per_m,
            ];
            let fixed = [
                v.fixed_input_credit_per_m,
                v.fixed_output_credit_per_m,
                v.fixed_cache_creation_credit_per_m,
                v.fixed_cache_read_credit_per_m,
                v.per_call_credit,
            ];
            if !text(&v.id, 128)
                || !text(&v.model, 256)
                || !positive(v.margin_multiplier)
                || prices
                    .iter()
                    .any(|p| !p.is_finite() || *p < 0.0 || *p > 1_000_000.0)
                || fixed.iter().any(|p| *p < 0 || *p > 1_000_000_000_000_000)
                || !c.rate_cards.contains_key(&v.rate_card_id)
                || v.effective_from_secs < now
            {
                return Err(invalid(
                    "Invalid pricing; retroactive publication forbidden",
                ));
            }
            if c.rate_card_versions.iter().any(|x| {
                x.id == v.id
                    || (x.rate_card_id == v.rate_card_id
                        && x.model == v.model
                        && x.effective_from_secs == v.effective_from_secs)
            }) {
                return Err(invalid(
                    "Published prices immutable; use new ID and timestamp",
                ));
            }
            c.rate_card_audit_logs.push(RateCardAuditLog {
                id: format!("publish-{}", v.id),
                rate_card_id: v.rate_card_id.clone(),
                version_id: v.id.clone(),
                operator_id: "admin-session".into(),
                reason: u.reason.clone(),
                created_at_secs: now,
                previous_version_id: self
                    .resolve_rate_card_version(&v.rate_card_id, &v.model, now)
                    .map(|x| x.id),
            });
            c.rate_card_versions.push(v);
        }
        let revision = view(&c).revision;
        c.commercial_audit_logs.push(CommercialAudit {
            previous_revision: u.expected_revision,
            revision,
            reason: u.reason,
            operator: "admin-session".into(),
            created_at_secs: now,
        });
        let result = view(&c);
        self.commit_candidate_snapshot(&c, || {
            *self.settings.write().unwrap() = c.settings.clone();
            *self.commercial_audit_logs.write().unwrap() = c.commercial_audit_logs.clone();
            *self.groups.write().unwrap() = c.groups.clone();
            *self.model_maps.write().unwrap() = c.model_maps.clone();
            *self.rate_cards.write().unwrap() = c.rate_cards.clone();
            *self.rate_card_versions.write().unwrap() = c.rate_card_versions.clone();
            *self.rate_card_audit_logs.write().unwrap() = c.rate_card_audit_logs.clone();
        })?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn update(e: &BillingEngine) -> CommercialUpdate {
        CommercialUpdate {
            expected_revision: e.commercial_config().revision,
            reason: "reviewed publication".into(),
            settings: None,
            groups: vec![Group::pro_plus("new-tier", "New tier")],
            models: vec![],
            rate_cards: vec![RateCard::new("default", "Default", 100)],
            versions: vec![],
        }
    }
    #[test]
    fn publication_is_revision_checked_and_audited() {
        let e = BillingEngine::new();
        let u = update(&e);
        let stale = u.clone();
        let before = u.expected_revision.clone();
        let result = e.publish_commercial_config(u, 100).unwrap();
        assert_ne!(before, result.revision);
        assert_eq!(result.audit.len(), 1);
        assert_eq!(result.audit[0].previous_revision, before);
        assert!(result.groups.iter().any(|g| g.id == "new-tier"));
        assert!(e.publish_commercial_config(stale, 101).is_err());
        assert_eq!(e.commercial_config().audit.len(), 1);
        let restored = BillingEngine::new();
        restored.import_snapshot(e.export_snapshot());
        assert_eq!(restored.commercial_config().revision, result.revision);
        assert_eq!(restored.commercial_config().audit.len(), 1);
    }
    #[test]
    fn invalid_or_inflight_publication_changes_nothing() {
        let e = BillingEngine::new();
        let before = e.commercial_config().revision;
        let mut u = update(&e);
        u.groups[0].margin_multiplier = f64::NAN;
        assert!(e.publish_commercial_config(u, 100).is_err());
        assert_eq!(e.commercial_config().revision, before);
        e.reservations.write().unwrap().insert(
            "held".into(),
            CreditReservation::new("held", "card", "call", 1, 100, 60),
        );
        assert!(e.publish_commercial_config(update(&e), 100).is_err());
        assert_eq!(e.commercial_config().revision, before);
        assert!(e.commercial_config().audit.is_empty());
    }
    #[test]
    fn model_aliases_and_provider_isolation_are_enforced() {
        use crate::provider::{Provider, ProviderFormat};
        let e = BillingEngine::new();
        e.upsert_provider(Provider::new(
            "shared",
            "Shared",
            ProviderFormat::Anthropic,
            "https://upstream.invalid",
        ));
        e.upsert_provider(
            Provider::new(
                "private",
                "Private",
                ProviderFormat::Anthropic,
                "https://upstream.invalid",
            )
            .with_group("other-tenant"),
        );
        e.upsert_provider_key(ProviderKey::new("shared-key", "shared", "test"));
        let mut u = update(&e);
        u.models.push(ModelMap::new(
            "map-1", "new-tier", "model-a", "shared", "target-a",
        ));
        let published = e.publish_commercial_config(u, 100).unwrap();
        assert_eq!(published.models[0].target_model, "target-a");
        let mut ambiguous = update(&e);
        ambiguous.models.push(
            ModelMap::new("map-2", "new-tier", "model-b", "shared", "target-b")
                .with_alias("model-a"),
        );
        assert!(e.publish_commercial_config(ambiguous, 101).is_err());
        let mut forbidden = update(&e);
        forbidden.models.push(ModelMap::new(
            "map-2", "new-tier", "model-b", "private", "target-b",
        ));
        assert!(e.publish_commercial_config(forbidden, 101).is_err());
        let mut fallback = update(&e);
        fallback.models.push(
            ModelMap::new("map-2", "new-tier", "model-b", "shared", "target-b")
                .with_fallback("private", "target-c"),
        );
        assert!(e.publish_commercial_config(fallback, 101).is_err());
        assert_eq!(e.commercial_config().revision, published.revision);
    }
    #[test]
    fn visible_publication_requires_a_compatible_enabled_key() {
        let e = BillingEngine::new();
        e.upsert_provider(Provider::new(
            "p",
            "P",
            crate::provider::ProviderFormat::OpenAi,
            "https://example.com",
        ));
        let mut k = ProviderKey::new("k", "p", "test");
        k.allowed_models = Some(vec!["sonnet".into()]);
        e.upsert_provider_key(k.clone());
        let mut u = update(&e);
        u.models
            .push(ModelMap::new("m", "new-tier", "opus", "p", "opus"));
        assert!(e.publish_commercial_config(u.clone(), 100).is_err());
        k.allowed_models = Some(vec!["opus".into()]);
        k.enabled = false;
        e.upsert_provider_key(k.clone());
        assert!(e.publish_commercial_config(u.clone(), 100).is_err());
        k.enabled = true;
        e.upsert_provider_key(k);
        assert!(e.publish_commercial_config(u, 100).is_ok());
    }

    #[test]
    fn published_prices_are_immutable_and_never_retroactive() {
        let e = BillingEngine::new();
        let mut u = update(&e);
        let version: RateCardVersion = serde_json::from_value(serde_json::json!({
            "id":"price-1", "rate_card_id":"default", "model":"*",
            "currency":"CNY", "pricing_mode":"per_call",
            "input_price_per_m":0.0,"output_price_per_m":0.0,
            "cache_creation_price_per_m":0.0,"cache_read_price_per_m":0.0,
            "fixed_input_credit_per_m":0,"fixed_output_credit_per_m":0,
            "fixed_cache_creation_credit_per_m":0,"fixed_cache_read_credit_per_m":0,
            "per_call_credit":1000,"margin_multiplier":1.0,"effective_from_secs":110
        }))
        .unwrap();
        u.versions.push(version.clone());
        let result = e.publish_commercial_config(u, 100).unwrap();
        let mut duplicate = update(&e);
        duplicate.versions.push(version.clone());
        assert!(e.publish_commercial_config(duplicate, 101).is_err());
        let mut retroactive = update(&e);
        let mut old = version;
        old.id = "price-2".into();
        old.effective_from_secs = 99;
        retroactive.versions.push(old);
        assert!(e.publish_commercial_config(retroactive, 101).is_err());
        assert_eq!(e.commercial_config().revision, result.revision);
        assert_eq!(e.commercial_config().audit.len(), 1);
    }
    #[test]
    fn persistence_failure_does_not_publish() {
        let e = BillingEngine::new();
        let blocker = std::env::temp_dir().join(format!(
            "kiro-commercial-blocker-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&blocker, "blocked").unwrap();
        *e.persistence_path.write().unwrap() = Some(blocker.join("state.json"));
        let before = e.commercial_config().revision;
        let result = e.publish_commercial_config(update(&e), 100);
        std::fs::remove_file(&blocker).unwrap();
        assert!(matches!(result, Err(BillingError::Persistence(_))));
        assert_eq!(e.commercial_config().revision, before);
        assert!(e.commercial_config().audit.is_empty());
    }
}
