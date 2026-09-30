//! Fixed-price local responses. Configuration, debit and receipts share the engine transaction.
use super::*;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

const RECEIPT_TTL: u64 = 86_400;
const MAX_RECEIPTS: usize = 4096;
const MAX_RECEIPTS_PER_CARD: usize = 256;
// Bound serialized bytes too: code can expand during JSON escaping.
const MAX_RECEIPT_BYTES: usize = 16 * 1024 * 1024;
const MAX_RECEIPT_BYTES_PER_CARD: usize = 2 * 1024 * 1024;

fn receipt_bytes(receipt: &ResponseTemplateReceipt) -> Result<usize, BillingError> {
    serde_json::to_vec(receipt)
        .map(|v| v.len() + 1)
        .map_err(|_| invalid("cannot encode receipt"))
}

fn validate_receipt_budget(receipts: &[ResponseTemplateReceipt]) -> Result<(), BillingError> {
    let mut total = 2usize;
    let mut cards: HashMap<&str, (usize, usize)> = HashMap::new();
    for receipt in receipts {
        let size = receipt_bytes(receipt)?;
        total = total.saturating_add(size);
        let card = cards.entry(&receipt.card_id).or_default();
        card.0 += 1;
        card.1 = card.1.saturating_add(size);
        if total > MAX_RECEIPT_BYTES
            || card.0 > MAX_RECEIPTS_PER_CARD
            || card.1 > MAX_RECEIPT_BYTES_PER_CARD
        {
            return Err(invalid(
                "receipt capacity exhausted; retry after receipts expire",
            ));
        }
    }
    Ok(())
}
const MAX_AUDIT: usize = 128;
const INITIAL_REVISION: &str = "response-template:v1:empty";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResponseTemplateIntent {
    /// AND across groups; OR across explicit synonyms inside each group.
    pub groups: Vec<Vec<String>>,
    #[serde(default)]
    pub exclude: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResponseTemplateMessage {
    pub at_ms: u32,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResponseTemplateDelivery {
    pub write_min_ms: u32,
    pub write_max_ms: u32,
    pub messages: Vec<ResponseTemplateMessage>,
    pub dispatch: String,
    pub success: String,
    pub failure: String,
    pub unknown: String,
    pub replay: String,
    pub continuation: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResponseTemplateVariant {
    pub model_id: String,
    pub file_path: String,
    pub content: String,
    pub preamble: String,
    pub completion: String,
    pub price_microcredits: i64,
    #[serde(default)]
    pub delay_ms: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery: Option<ResponseTemplateDelivery>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResponseTemplateRule {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub match_mode: String,
    pub match_text: String,
    pub variants: Vec<ResponseTemplateVariant>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent: Option<ResponseTemplateIntent>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResponseTemplateAudit {
    pub previous_revision: String,
    pub revision: String,
    pub reason: String,
    pub created_at_secs: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResponseTemplateConfig {
    pub revision: String,
    pub rules: Vec<ResponseTemplateRule>,
    pub audit: Vec<ResponseTemplateAudit>,
}

impl Default for ResponseTemplateConfig {
    fn default() -> Self {
        Self {
            revision: INITIAL_REVISION.into(),
            rules: Vec::new(),
            audit: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResponseTemplateUpdate {
    pub expected_revision: String,
    pub reason: String,
    pub rules: Vec<ResponseTemplateRule>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResponseTemplateReceipt {
    pub tool_use_id: String,
    pub invocation_id: String,
    pub card_id: String,
    pub conversation_id: String,
    pub model_id: String,
    pub file_path: String,
    pub completion: String,
    #[serde(default)]
    pub tool_name: String,
    #[serde(default)]
    pub path_key: String,
    #[serde(default)]
    pub content_key: String,
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub price_microcredits: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery: Option<ResponseTemplateDelivery>,
    pub created_at_secs: u64,
}

fn invalid(message: &str) -> BillingError {
    BillingError::InvalidState(format!("response template: {message}"))
}

/// Portable relative HTML paths, not filesystem access. Every component must also be
/// safe on Windows (including device names with extensions and trailing dots/spaces).
fn safe_html_path(path: &str) -> bool {
    if path.is_empty()
        || path.len() > 240
        || path
            .chars()
            .any(|c| c.is_control() || matches!(c, '\\' | ':' | '<' | '>' | '"' | '|' | '?' | '*'))
    {
        return false;
    }
    let lower = path.to_ascii_lowercase();
    if !(lower.ends_with(".html") || lower.ends_with(".htm")) {
        return false;
    }
    path.split('/').all(|part| {
        if part.is_empty()
            || part == "."
            || part == ".."
            || part.trim() != part
            || part.ends_with('.')
        {
            return false;
        }
        let stem = part.split('.').next().unwrap().trim_end().to_uppercase();
        if matches!(
            stem.as_str(),
            "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
        ) {
            return false;
        }
        !["COM", "LPT"].iter().any(|prefix| {
            stem.strip_prefix(prefix).is_some_and(|suffix| {
                matches!(
                    suffix,
                    "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                )
            })
        })
    })
}

fn message(s: &str) -> bool {
    s.len() <= 4096
        && !s
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
}

pub fn validate_response_template_rules(
    rules: &[ResponseTemplateRule],
) -> Result<(), BillingError> {
    if rules.len() > 32 {
        return Err(invalid("at most 32 rules are allowed"));
    }
    let mut ids = HashSet::new();
    let mut total = 0usize;
    for rule in rules {
        if !crate::valid_id(&rule.id, 128)
            || !ids.insert(&rule.id)
            || !crate::valid_id(&rule.name, 256)
            || !matches!(rule.match_mode.as_str(), "exact" | "contains" | "intent")
            || rule.match_text.trim().is_empty()
            || !message(&rule.match_text)
            || rule.variants.is_empty()
            || rule.variants.len() > 32
        {
            return Err(invalid(
                "invalid or duplicate rule, matcher, or variant count",
            ));
        }
        if rule.match_mode == "intent" && rule.intent.is_none() {
            return Err(invalid("intent mode requires concept groups"));
        }
        if let Some(intent) = &rule.intent {
            if !(2..=12).contains(&intent.groups.len())
                || intent
                    .groups
                    .iter()
                    .any(|g| g.is_empty() || g.len() > 32 || g.iter().any(|s| !valid_term(s)))
                || intent.exclude.len() > 64
                || intent.exclude.iter().any(|s| !valid_term(s))
            {
                return Err(invalid("intent requires 2–12 groups, 1–32 synonyms per group and at most 64 exclusions (2–128 characters)"));
            }
        }
        let mut models = HashSet::new();
        for variant in &rule.variants {
            if !crate::group::valid_model_id(&variant.model_id)
                || !models.insert(&variant.model_id)
                || !safe_html_path(&variant.file_path)
                || variant.content.len() > 256 * 1024
                || variant.delay_ms > 30_000
                || !message(&variant.preamble)
                || !message(&variant.completion)
                || !(0..=1_000_000_000).contains(&variant.price_microcredits)
            {
                return Err(invalid(
                    "invalid or duplicate model variant, HTML path, content, message, or price",
                ));
            }
            if let Some(delivery) = &variant.delivery {
                validate_delivery(delivery)?;
            }
            total += variant.content.len();
            if total > 2 * 1024 * 1024 {
                return Err(invalid("HTML content exceeds 2 MiB"));
            }
        }
    }
    // Bound the complete replacement, not just HTML: otherwise 1024 variants of
    // individually bounded messages can still exceed the configuration budget.
    if serde_json::to_vec(rules)
        .map_err(|_| invalid("cannot encode rules"))?
        .len()
        > 2 * 1024 * 1024
    {
        return Err(invalid("serialized rules exceed 2 MiB"));
    }
    Ok(())
}

fn valid_term(s: &str) -> bool {
    (2..=128).contains(&s.trim().chars().count())
        && message(s)
        && s.chars().any(char::is_alphanumeric)
}
fn validate_delivery(d: &ResponseTemplateDelivery) -> Result<(), BillingError> {
    if d.write_min_ms > d.write_max_ms
        || d.write_max_ms > 300_000
        || d.messages.len() > 16
        || d.messages
            .iter()
            .any(|m| m.at_ms > d.write_min_ms || m.text.trim().is_empty() || !message(&m.text))
        || d.messages.windows(2).any(|w| w[0].at_ms >= w[1].at_ms)
        || [
            &d.dispatch,
            &d.success,
            &d.failure,
            &d.unknown,
            &d.replay,
            &d.continuation,
        ]
        .iter()
        .any(|s| !message(s))
    {
        return Err(invalid("invalid delivery: 0–300000ms, ordered absolute message times before earliest dispatch, up to 16 messages"));
    }
    Ok(())
}

fn validate_receipt(receipt: &ResponseTemplateReceipt) -> Result<(), BillingError> {
    if let Some(d) = &receipt.delivery {
        validate_delivery(d)?;
    }
    if [
        &receipt.tool_use_id,
        &receipt.invocation_id,
        &receipt.card_id,
        &receipt.conversation_id,
    ]
    .iter()
    .any(|id| !crate::valid_id(id, 256))
        || !crate::group::valid_model_id(&receipt.model_id)
        || !safe_html_path(&receipt.file_path)
        || !message(&receipt.completion)
        || !crate::valid_id(&receipt.tool_name, 128)
        || !crate::valid_id(&receipt.path_key, 32)
        || !crate::valid_id(&receipt.content_key, 32)
        || receipt.content.len() > 256 * 1024
        || receipt.price_microcredits < 0
        || !matches!(
            (
                receipt.tool_name.as_str(),
                receipt.path_key.as_str(),
                receipt.content_key.as_str()
            ),
            ("fsWrite", "path", "text")
                | ("fs_write", "path", "text")
                | ("Write", "file_path", "content")
                | ("write_file" | "writeFile", "path", "content")
        )
    {
        return Err(invalid("invalid receipt"));
    }
    Ok(())
}

fn live(receipt: &ResponseTemplateReceipt, now: u64) -> bool {
    now.checked_sub(receipt.created_at_secs)
        .is_some_and(|age| age < RECEIPT_TTL)
}

pub(super) fn validate_snapshot_state(s: &BillingSnapshot) -> Result<(), BillingError> {
    validate_receipt_budget(&s.response_template_receipts)?;
    let config = &s.response_templates;
    validate_response_template_rules(&config.rules)?;
    if !crate::valid_id(&config.revision, 128)
        || !config.revision.starts_with("response-template:v1:")
        || config.audit.len() > MAX_AUDIT
        || s.response_template_receipts.len() > MAX_RECEIPTS
    {
        return Err(invalid("invalid snapshot bounds or revision"));
    }
    if config.audit.is_empty() {
        if config != &ResponseTemplateConfig::default() {
            return Err(invalid("missing config audit"));
        }
    } else if config.audit.last().unwrap().revision != config.revision {
        return Err(invalid("config audit revision mismatch"));
    }
    for (i, audit) in config.audit.iter().enumerate() {
        if !crate::valid_id(&audit.reason, 1024)
            || !crate::valid_id(&audit.revision, 128)
            || !crate::valid_id(&audit.previous_revision, 128)
            || (i > 0 && config.audit[i - 1].revision != audit.previous_revision)
        {
            return Err(invalid("invalid config audit"));
        }
    }
    if s.response_template_receipts.is_empty() {
        return Ok(());
    }
    let receipt_ids: HashSet<_> = s
        .response_template_receipts
        .iter()
        .map(|r| r.invocation_id.as_str())
        .collect();
    let entries: HashMap<_, _> = s
        .ledger
        .iter()
        .filter_map(|entry| {
            let id = entry.invocation_id.as_deref()?;
            receipt_ids.contains(id).then_some((id, entry))
        })
        .collect();
    let mut keys = HashSet::new();
    let mut invocations = HashSet::new();
    for receipt in &s.response_template_receipts {
        validate_receipt(receipt)?;
        if !keys.insert((
            &receipt.card_id,
            &receipt.conversation_id,
            &receipt.tool_use_id,
        )) || !invocations.insert(&receipt.invocation_id)
            || !s.cards.contains_key(&receipt.card_id)
        {
            return Err(invalid("duplicate or orphan receipt"));
        }
        let entry = entries.get(receipt.invocation_id.as_str());
        if let Some(entry) = entry {
            if entry.kind != LedgerKind::Usage
                || entry.card_id != receipt.card_id
                || entry.exposed_model != receipt.model_id
                || entry.ts_secs != receipt.created_at_secs
                || !entry.provider_id.starts_with("response-template:")
                || entry.input_tokens != 0
                || entry.output_tokens != 0
                || entry.cache_creation_tokens != 0
                || entry.cache_read_tokens != 0
                || entry.provider_cost_micro_cny != 0
                || entry.credits_charged != receipt.price_microcredits
            {
                return Err(invalid("receipt does not match service charge"));
            }
        } else if !s
            .archived_ledger_summary
            .usage_invocation_ids
            .contains(&receipt.invocation_id)
        {
            return Err(invalid("receipt missing durable charge"));
        }
    }
    Ok(())
}

impl BillingEngine {
    pub fn response_template_config(&self) -> ResponseTemplateConfig {
        let _guard = self.state_lock.read().unwrap();
        self.response_templates.read().unwrap().clone()
    }

    /// Full replacement. The revision chain prevents ABA even when rules are restored.
    pub fn publish_response_templates(
        &self,
        update: ResponseTemplateUpdate,
        now: u64,
    ) -> Result<ResponseTemplateConfig, BillingError> {
        validate_response_template_rules(&update.rules)?;
        // Old snapshots may contain enabled empty variants; keep them loadable, but
        // refuse new publications and execution of such variants.
        if update
            .rules
            .iter()
            .any(|r| r.enabled && r.variants.iter().any(|v| v.content.trim().is_empty()))
        {
            return Err(invalid("enabled templates require nonempty HTML"));
        }
        if !crate::valid_id(&update.expected_revision, 128)
            || !crate::valid_id(&update.reason, 1024)
        {
            return Err(invalid("revision and reason are required and bounded"));
        }
        let _guard = self.state_lock.write().unwrap();
        let mut candidate = self.export_snapshot_locked(
            self.snapshot_sequence
                .load(Ordering::Acquire)
                .saturating_add(1),
            self.last_snapshot_checksum.read().unwrap().clone(),
        );
        if candidate.response_templates.revision != update.expected_revision {
            return Err(invalid("configuration changed; reload before publishing"));
        }
        let bytes = serde_json::to_vec(&(&update, now))
            .map_err(|_| invalid("cannot encode configuration"))?;
        let digest: String = ring::digest::digest(&ring::digest::SHA256, &bytes)
            .as_ref()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let revision = format!("response-template:v1:{digest}");
        let config = &mut candidate.response_templates;
        config.audit.push(ResponseTemplateAudit {
            previous_revision: update.expected_revision,
            revision: revision.clone(),
            reason: update.reason,
            created_at_secs: now,
        });
        if config.audit.len() > MAX_AUDIT {
            config.audit.remove(0);
        }
        config.revision = revision;
        config.rules = update.rules;
        let result = config.clone();
        self.commit_candidate_snapshot(&candidate, || {
            *self.response_templates.write().unwrap() = result.clone();
        })?;
        Ok(result)
    }

    /// Admit a local template before its cancellable delay, without reserving money.
    pub fn begin_response_template_wait(
        &self,
        expected_revision: &str,
        rule_id: &str,
        model_id: &str,
        card_id: &str,
        invocation_id: &str,
        now: u64,
    ) -> Result<TemplateWaitSlot, BillingError> {
        let _guard = self.state_lock.write().unwrap();
        let config = self.response_templates.read().unwrap();
        if config.revision != expected_revision {
            return Err(invalid("configuration changed; reload before waiting"));
        }
        let variant = config
            .rules
            .iter()
            .find(|rule| rule.id == rule_id && rule.enabled)
            .and_then(|rule| rule.variants.iter().find(|v| v.model_id == model_id))
            .ok_or_else(|| invalid("enabled rule or exact model variant not found"))?;
        if variant.content.trim().is_empty() {
            return Err(invalid("empty legacy template cannot be executed"));
        }
        let card = self
            .cards
            .read()
            .unwrap()
            .get(card_id)
            .cloned()
            .ok_or_else(|| BillingError::CardNotFound(card_id.into()))?;
        self.check_response_template_card(&card, model_id, variant.price_microcredits, now)?;
        let key = (card_id.to_string(), invocation_id.to_string());
        let reservations = self.reservations.read().unwrap();
        let pending = self.pending_settlements.read().unwrap();
        if self.template_waits.lock().unwrap().contains(&key)
            || reservations
                .get(invocation_id)
                .is_some_and(|r| r.state != ReservationState::Released)
            || pending.contains_key(invocation_id)
        {
            return Err(BillingError::DuplicateInvocation(invocation_id.into()));
        }
        let current = self.active_card_concurrency(card_id, &reservations, None);
        if current >= card.max_concurrency {
            return Err(BillingError::ConcurrencyLimitExceeded {
                current,
                max: card.max_concurrency,
            });
        }
        check_quota(
            &card,
            variant.price_microcredits,
            now,
            &reservations,
            &pending,
            &self.ledger.read().unwrap(),
            &self.archived_ledger_summary.read().unwrap(),
        )?;
        self.template_waits.lock().unwrap().insert(key.clone());
        Ok(TemplateWaitSlot {
            waits: self.template_waits.clone(),
            key,
        })
    }

    // Both admission and final debit run this under state_lock. Eligibility can change
    // during the delay, and the wait slot intentionally reserves no credit.
    fn check_response_template_card(
        &self,
        card: &Card,
        model_id: &str,
        price: i64,
        now: u64,
    ) -> Result<(), BillingError> {
        card.check_can_reserve(price, now)?;
        if card.outstanding_debt() > 0 {
            return Err(invalid("card has outstanding debt"));
        }
        if !self.groups.read().unwrap().contains_key(&card.group_id)
            || !self
                .model_maps
                .read()
                .unwrap()
                .iter()
                .any(|m| m.group_id == card.group_id && !m.retired && m.matches_model(model_id))
        {
            return Err(invalid("model is not allowed for the card group"));
        }
        Ok(())
    }

    /// The caller authorizes a match; billing resolves the exact model variant under the
    /// state lock. Replays return DuplicateInvocation, never a second charge. Retrieve
    /// the scoped receipt to recover a response after a lost acknowledgement.
    pub fn charge_response_template(
        &self,
        expected_revision: &str,
        rule_id: &str,
        model_id: &str,
        mut receipt: ResponseTemplateReceipt,
        now: u64,
    ) -> Result<LedgerEntry, BillingError> {
        validate_receipt(&receipt)?;
        let _guard = self.state_lock.write().unwrap();
        let mut candidate = self.export_snapshot_locked(
            self.snapshot_sequence
                .load(Ordering::Acquire)
                .saturating_add(1),
            self.last_snapshot_checksum.read().unwrap().clone(),
        );
        if candidate.response_templates.revision != expected_revision {
            return Err(invalid("configuration changed; reload before charging"));
        }
        let variant = candidate
            .response_templates
            .rules
            .iter()
            .find(|rule| rule.id == rule_id && rule.enabled)
            .and_then(|rule| rule.variants.iter().find(|v| v.model_id == model_id))
            .ok_or_else(|| invalid("enabled rule or exact model variant not found"))?
            .clone();
        if variant.content.trim().is_empty() {
            return Err(invalid("empty legacy template cannot be executed"));
        }
        if receipt.model_id != model_id
            || receipt.file_path != variant.file_path
            || receipt.delivery != variant.delivery
            || receipt.completion != variant.completion
            || receipt.content != variant.content
            || receipt.price_microcredits != variant.price_microcredits
        {
            return Err(invalid("receipt disagrees with the configured variant"));
        }
        let id = &receipt.invocation_id;
        if candidate
            .reservations
            .get(id)
            .is_some_and(|r| r.state != ReservationState::Released)
            || candidate.pending_settlements.contains_key(id)
            || candidate
                .ledger
                .iter()
                .any(|e| e.invocation_id.as_ref() == Some(id))
            || candidate
                .archived_ledger_summary
                .usage_invocation_ids
                .contains(id)
            || candidate
                .archived_ledger_summary
                .adjustments
                .contains_key(id)
            || candidate.response_template_receipts.iter().any(|r| {
                r.invocation_id == *id
                    || (r.card_id == receipt.card_id
                        && r.conversation_id == receipt.conversation_id
                        && r.tool_use_id == receipt.tool_use_id
                        && (live(r, now) || r.created_at_secs > now))
            })
        {
            return Err(BillingError::DuplicateInvocation(id.clone()));
        }
        // Never evict an unexpired receipt to admit work. Clock rollback also cannot
        // discard receipts whose timestamps are in the future.
        candidate
            .response_template_receipts
            .retain(|r| live(r, now) || r.created_at_secs > now);
        if candidate.response_template_receipts.len() >= MAX_RECEIPTS
            || candidate
                .response_template_receipts
                .iter()
                .filter(|r| r.card_id == receipt.card_id)
                .count()
                >= MAX_RECEIPTS_PER_CARD
        {
            return Err(invalid(
                "receipt capacity exhausted; retry after receipts expire",
            ));
        }
        let mut card = candidate
            .cards
            .get(&receipt.card_id)
            .cloned()
            .ok_or_else(|| BillingError::CardNotFound(receipt.card_id.clone()))?;
        self.check_response_template_card(&card, model_id, variant.price_microcredits, now)?;
        let current = self.active_card_concurrency(
            &card.id,
            &candidate.reservations,
            Some(&receipt.invocation_id),
        );
        if current >= card.max_concurrency {
            return Err(BillingError::ConcurrencyLimitExceeded {
                current,
                max: card.max_concurrency,
            });
        }
        candidate.check_quota(&card, variant.price_microcredits, now)?;
        card.credit_used = card
            .credit_used
            .checked_add(variant.price_microcredits)
            .ok_or_else(|| invalid("credit usage overflow"))?;
        receipt.created_at_secs = now;
        let entry = LedgerEntry {
            id: format!("response-template:{}", receipt.invocation_id),
            card_id: card.id.clone(),
            kind: LedgerKind::Usage,
            invocation_id: Some(receipt.invocation_id.clone()),
            exposed_model: model_id.into(),
            provider_id: format!("response-template:{rule_id}"),
            target_model: model_id.into(),
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            credits_charged: variant.price_microcredits,
            provider_cost_micro_cny: 0,
            rate_card_version: Some(expected_revision.into()),
            ts_secs: now,
            operator_id: None,
            reason: Some("fixed local response service".into()),
            credit_face_value_cny: Some(candidate.settings.credit_face_value_cny),
            detail: Some(serde_json::json!({"response_template_rule_id": rule_id,
                "file_path": variant.file_path, "conversation_id": receipt.conversation_id,
                "tool_use_id": receipt.tool_use_id})),
        };
        candidate.cards.insert(card.id.clone(), card.clone());
        candidate.ledger.push(entry.clone());
        candidate.response_template_receipts.push(receipt);
        validate_receipt_budget(&candidate.response_template_receipts)?;
        self.commit_candidate_snapshot(&candidate, || {
            self.cards.write().unwrap().insert(card.id.clone(), card);
            self.ledger.write().unwrap().push(entry.clone());
            *self.response_template_receipts.write().unwrap() =
                candidate.response_template_receipts.clone();
        })?;
        Ok(entry)
    }

    /// Scoped, read-only recovery. Expired receipts are hidden even before lazy pruning.
    pub fn response_template_receipt(
        &self,
        card_id: &str,
        conversation_id: &str,
        tool_use_id: &str,
        now: u64,
    ) -> Option<ResponseTemplateReceipt> {
        self.response_template_receipt_any(card_id, conversation_id, tool_use_id)
            .filter(|receipt| live(receipt, now))
    }

    /// Finds a receipt without applying TTL so stale results cannot fall through to billing.
    pub fn response_template_receipt_any(
        &self,
        card_id: &str,
        conversation_id: &str,
        tool_use_id: &str,
    ) -> Option<ResponseTemplateReceipt> {
        let _guard = self.state_lock.read().unwrap();
        self.response_template_receipts
            .read()
            .unwrap()
            .iter()
            .find(|r| {
                r.card_id == card_id
                    && r.conversation_id == conversation_id
                    && r.tool_use_id == tool_use_id
            })
            .cloned()
    }

    pub fn response_template_receipt_for_invocation(
        &self,
        card_id: &str,
        invocation_id: &str,
        now: u64,
    ) -> Option<ResponseTemplateReceipt> {
        let _guard = self.state_lock.read().unwrap();
        self.response_template_receipts
            .read()
            .unwrap()
            .iter()
            .find(|r| r.card_id == card_id && r.invocation_id == invocation_id && live(r, now))
            .cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 2_000_000;

    fn rules(price: i64) -> Vec<ResponseTemplateRule> {
        vec![ResponseTemplateRule {
            id: "landing".into(),
            name: "Landing page".into(),
            enabled: true,
            match_mode: "contains".into(),
            intent: None,
            match_text: "make a page".into(),
            variants: vec![ResponseTemplateVariant {
                delay_ms: 0,
                delivery: None,
                model_id: "model-a".into(),
                file_path: "pages/index.html".into(),
                content: "<html>model A</html>".into(),
                preamble: "Creating page".into(),
                completion: "Page created".into(),
                price_microcredits: price,
            }],
        }]
    }

    #[test]
    fn optional_template_fields_do_not_expand_legacy_capacity() {
        let mut legacy = rules(0);
        let v = legacy[0].variants[0].clone();
        legacy[0].variants = (0..8)
            .map(|i| {
                let mut v = v.clone();
                v.model_id = format!("model-{i}");
                v.content = "a".repeat(256 * 1024);
                v
            })
            .collect();
        let limit = 2 * 1024 * 1024;
        let size = serde_json::to_vec(&legacy).unwrap().len();
        legacy[0].variants[0]
            .content
            .truncate(256 * 1024 - (size - (limit - 1)));
        let bytes = serde_json::to_vec(&legacy).unwrap();
        assert_eq!(bytes.len(), limit - 1);
        let shape: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(shape[0].get("intent").is_none());
        assert!(shape[0]["variants"]
            .as_array()
            .unwrap()
            .iter()
            .all(|v| v.get("delivery").is_none()));
        let loaded: Vec<ResponseTemplateRule> = serde_json::from_slice(&bytes).unwrap();
        validate_response_template_rules(&loaded).unwrap();
        assert_eq!(serde_json::to_vec(&loaded).unwrap(), bytes);
        let r = serde_json::to_value(receipt("legacy-fields")).unwrap();
        assert!(r.get("delivery").is_none());
        assert_eq!(
            serde_json::to_value(
                serde_json::from_value::<ResponseTemplateReceipt>(r.clone()).unwrap()
            )
            .unwrap(),
            r
        );
    }

    fn update(e: &BillingEngine, rules: Vec<ResponseTemplateRule>) -> ResponseTemplateUpdate {
        ResponseTemplateUpdate {
            expected_revision: e.response_template_config().revision,
            reason: "test publication".into(),
            rules,
        }
    }

    fn setup(price: i64) -> (BillingEngine, String) {
        let e = BillingEngine::new();
        let mut group = Group::pro_plus("group", "Group");
        group.margin_multiplier = 99.0;
        e.upsert_group(group);
        let mut model = ModelMap::new("mapping", "group", "model-a", "upstream", "target");
        model.credit_multiplier = 99.0;
        e.upsert_model_map(model);
        let mut card = Card::new("card", "group", 1000);
        card.status = CardStatus::Active;
        card.max_concurrency = 2;
        e.upsert_card(card);
        let revision = e
            .publish_response_templates(update(&e, rules(price)), NOW)
            .unwrap()
            .revision;
        (e, revision)
    }

    fn receipt(id: &str) -> ResponseTemplateReceipt {
        ResponseTemplateReceipt {
            tool_use_id: format!("tool-{id}"),
            invocation_id: id.into(),
            card_id: "card".into(),
            conversation_id: "conversation".into(),
            model_id: "model-a".into(),
            file_path: "pages/index.html".into(),
            completion: "Page created".into(),
            tool_name: "fsWrite".into(),
            path_key: "path".into(),
            content_key: "text".into(),
            content: "<html>model A</html>".into(),
            price_microcredits: 10,
            delivery: None,
            created_at_secs: 0,
        }
    }

    #[test]
    fn template_wait_admission_is_atomic_and_process_local() {
        let (e, revision) = setup(10);
        let mut card = e.get_card("card").unwrap();
        card.max_concurrency = 1;
        e.upsert_card(card);
        let barrier = Arc::new(std::sync::Barrier::new(8));
        let threads: Vec<_> = (0..8)
            .map(|i| {
                let e = e.clone();
                let revision = revision.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    e.begin_response_template_wait(
                        &revision,
                        "landing",
                        "model-a",
                        "card",
                        &format!("wait-{i}"),
                        NOW,
                    )
                })
            })
            .collect();
        // Keep every successful guard alive until all admission attempts finish.
        let results: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
        assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
        for result in &results {
            if let Err(error) = result {
                assert!(matches!(
                    error,
                    BillingError::ConcurrencyLimitExceeded { current: 1, max: 1 }
                ));
            }
        }
        let winner = results.iter().position(|r| r.is_ok()).unwrap();
        assert!(matches!(
            e.begin_response_template_wait(
                &revision,
                "landing",
                "model-a",
                "card",
                &format!("wait-{winner}"),
                NOW,
            ),
            Err(BillingError::DuplicateInvocation(_))
        ));
        let snapshot = e.export_snapshot();
        assert!(snapshot.reservations.is_empty());
        assert!(snapshot.ledger.is_empty());
        assert!(snapshot.response_template_receipts.is_empty());
        assert_eq!(snapshot.cards["card"].credit_used, 0);
        assert_eq!(snapshot.cards["card"].credit_reserved, 0);
        let restarted = BillingEngine::new();
        restarted.import_snapshot(snapshot);
        let _fresh = restarted
            .begin_response_template_wait(
                &revision,
                "landing",
                "model-a",
                "card",
                "after-restart",
                NOW,
            )
            .unwrap();
        drop(results);
        let _next = e
            .begin_response_template_wait(
                &revision,
                "landing",
                "model-a",
                "card",
                "after-drop",
                NOW,
            )
            .unwrap();
    }

    fn charge(
        e: &BillingEngine,
        revision: &str,
        id: &str,
        now: u64,
    ) -> Result<LedgerEntry, BillingError> {
        let mut receipt = receipt(id);
        let config = e.response_template_config();
        let variant = &config.rules[0].variants[0];
        receipt.content = variant.content.clone();
        receipt.price_microcredits = variant.price_microcredits;
        e.charge_response_template(revision, "landing", "model-a", receipt, now)
    }

    struct Temp(std::path::PathBuf);
    impl Temp {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "billing-response-template-{}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn state(&self) -> std::path::PathBuf {
            self.0.join("state.json")
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn response_template_fixed_charge_has_no_tokens_cost_or_multipliers() {
        let (e, revision) = setup(123);
        let commercial = e.commercial_config().revision;
        let entry = charge(&e, &revision, "one", NOW).unwrap();
        assert_eq!(entry.credits_charged, 123);
        assert_eq!(entry.provider_cost_micro_cny, 0);
        assert_eq!(
            (
                entry.input_tokens,
                entry.output_tokens,
                entry.cache_creation_tokens,
                entry.cache_read_tokens
            ),
            (0, 0, 0, 0)
        );
        assert_eq!(entry.provider_id, "response-template:landing");
        assert_eq!(entry.rate_card_version.as_deref(), Some(revision.as_str()));
        assert_eq!(entry.kind, LedgerKind::Usage);
        let card = e.get_card("card").unwrap();
        assert_eq!(
            (
                card.credit_used,
                card.credit_reserved,
                card.available_credits()
            ),
            (123, 0, 877)
        );
        assert_eq!(
            e.response_template_receipt("card", "conversation", "tool-one", NOW)
                .unwrap()
                .created_at_secs,
            NOW
        );
        assert_eq!(e.commercial_config().revision, commercial);
        validate_snapshot(&e.export_snapshot()).unwrap();
    }

    #[test]
    fn response_template_free_service_at_zero_balance_still_records_usage() {
        let (e, revision) = setup(0);
        let mut card = e.get_card("card").unwrap();
        card.credit_total = 0;
        card.daily_credit_limit = Some(0);
        card.monthly_credit_limit = Some(0);
        e.upsert_card(card);
        assert_eq!(
            charge(&e, &revision, "free", NOW).unwrap().credits_charged,
            0
        );
        assert_eq!(e.get_card("card").unwrap().credit_used, 0);
        assert_eq!(e.ledger_entries().len(), 1);
        assert!(matches!(
            charge(&e, &revision, "free", NOW),
            Err(BillingError::DuplicateInvocation(_))
        ));
        validate_snapshot(&e.export_snapshot()).unwrap();
    }

    #[test]
    fn response_template_distinct_models_use_exact_configured_variants() {
        let (e, _) = setup(10);
        e.upsert_model_map(ModelMap::new(
            "mapping-b",
            "group",
            "model-b",
            "upstream",
            "target-b",
        ));
        let mut r = rules(10);
        let mut variant = r[0].variants[0].clone();
        variant.model_id = "model-b".into();
        variant.file_path = "other.htm".into();
        variant.content = "<html>model B</html>".into();
        variant.completion = "Other page".into();
        variant.price_microcredits = 70;
        r[0].variants.push(variant);
        let c = e.publish_response_templates(update(&e, r), NOW).unwrap();
        let mut receipt = receipt("model-b");
        receipt.model_id = "model-b".into();
        receipt.file_path = "other.htm".into();
        receipt.completion = "Other page".into();
        receipt.content = "<html>model B</html>".into();
        receipt.price_microcredits = 70;
        assert_eq!(
            e.charge_response_template(&c.revision, "landing", "model-b", receipt, NOW)
                .unwrap()
                .credits_charged,
            70
        );
        assert_eq!(
            charge(&e, &c.revision, "model-a", NOW)
                .unwrap()
                .credits_charged,
            10
        );
        assert_ne!(
            c.rules[0].variants[0].content,
            c.rules[0].variants[1].content
        );
    }

    #[test]
    fn response_template_balance_and_active_expiry_are_enforced_even_when_free() {
        let (e, revision) = setup(1001);
        assert!(matches!(
            charge(&e, &revision, "poor", NOW),
            Err(BillingError::Card(CardError::InsufficientCredit { .. }))
        ));
        for status in [
            CardStatus::Unactivated,
            CardStatus::Frozen,
            CardStatus::Banned,
            CardStatus::Voided,
        ] {
            let (e, revision) = setup(0);
            let mut card = e.get_card("card").unwrap();
            card.status = status;
            e.upsert_card(card);
            assert!(charge(&e, &revision, "inactive", NOW).is_err());
            assert!(e.ledger_entries().is_empty());
        }
        let (e, revision) = setup(0);
        let mut card = e.get_card("card").unwrap();
        card.valid_until = Some(NOW);
        e.upsert_card(card);
        assert!(matches!(
            charge(&e, &revision, "expired", NOW),
            Err(BillingError::Card(CardError::Expired))
        ));
    }

    #[test]
    fn response_template_daily_monthly_and_archived_usage_limits() {
        for monthly in [false, true] {
            let (e, revision) = setup(60);
            let mut card = e.get_card("card").unwrap();
            if monthly {
                card.monthly_credit_limit = Some(100);
            } else {
                card.daily_credit_limit = Some(100);
            }
            e.upsert_card(card);
            charge(&e, &revision, "first", NOW).unwrap();
            let temp = Temp::new();
            e.archive_ledger(NOW + 1, &temp.0).unwrap();
            let error = charge(&e, &revision, "second", NOW + 1).unwrap_err();
            if monthly {
                assert!(matches!(error, BillingError::MonthlyLimitExceeded { .. }));
            } else {
                assert!(matches!(error, BillingError::DailyLimitExceeded { .. }));
            }
            assert_eq!(e.get_card("card").unwrap().credit_used, 60);
        }
    }

    #[test]
    fn response_template_holds_share_balance_quota_and_concurrency() {
        for case in ["balance", "quota", "concurrency"] {
            let (e, revision) = setup(100);
            let mut card = e.get_card("card").unwrap();
            if case == "quota" {
                card.daily_credit_limit = Some(150);
            }
            if case == "concurrency" {
                card.max_concurrency = 1;
            }
            e.upsert_card(card);
            let mut params = ReservationEstimateParams::new(1, 0);
            params.input_rate_per_m = if case == "balance" {
                950_000_000
            } else {
                100_000_000
            };
            e.reserve("card", "held", &params, NOW, 60).unwrap();
            let error = charge(&e, &revision, "service", NOW).unwrap_err();
            match case {
                "balance" => assert!(matches!(
                    error,
                    BillingError::Card(CardError::InsufficientCredit { .. })
                )),
                "quota" => assert!(matches!(error, BillingError::DailyLimitExceeded { .. })),
                _ => assert!(matches!(
                    error,
                    BillingError::ConcurrencyLimitExceeded { .. }
                )),
            }
            assert!(matches!(
                charge(&e, &revision, "held", NOW),
                Err(BillingError::DuplicateInvocation(_))
            ));
            assert!(e.ledger_entries().is_empty());
        }
    }

    #[test]
    fn response_template_group_and_retired_model_are_rechecked() {
        let (e, revision) = setup(0);
        let mut snapshot = e.export_snapshot();
        snapshot.model_maps[0].group_id = "other".into();
        e.import_snapshot(snapshot);
        assert!(charge(&e, &revision, "wrong-group", NOW).is_err());
        let mut snapshot = e.export_snapshot();
        snapshot.model_maps[0].group_id = "group".into();
        snapshot.model_maps[0].retired = true;
        e.import_snapshot(snapshot);
        assert!(charge(&e, &revision, "retired", NOW).is_err());
        let mut snapshot = e.export_snapshot();
        snapshot.groups.remove("group");
        e.import_snapshot(snapshot);
        assert!(charge(&e, &revision, "missing-group", NOW).is_err());
    }

    #[test]
    fn response_template_uses_actual_card_group_after_reassignment() {
        let (e, revision) = setup(0);
        e.upsert_group(Group::pro_plus("new-group", "New group"));
        let mut card = e.get_card("card").unwrap();
        card.group_id = "new-group".into();
        e.upsert_card(card);
        assert!(charge(&e, &revision, "old-claim", NOW).is_err());
        assert!(e.ledger_entries().is_empty());
        e.upsert_model_map(ModelMap::new(
            "new-mapping",
            "new-group",
            "model-a",
            "upstream",
            "target",
        ));
        charge(&e, &revision, "authorized", NOW).unwrap();
    }

    #[test]
    fn response_template_pending_settlement_blocks_even_free_service() {
        let (e, revision) = setup(0);
        let mut entry = charge(&e, &revision, "first", NOW).unwrap();
        e.reserve(
            "card",
            "pending",
            &ReservationEstimateParams::new(0, 0),
            NOW,
            60,
        )
        .unwrap();
        let mut snapshot = e.export_snapshot();
        entry.invocation_id = Some("pending".into());
        snapshot.pending_settlements.insert(
            "pending".into(),
            PendingSettlement {
                entry,
                tokens: UsageTokens::default(),
            },
        );
        e.import_snapshot(snapshot);
        assert!(charge(&e, &revision, "blocked", NOW).is_err());
        assert_eq!(e.ledger_entries().len(), 1);
    }

    #[test]
    fn response_template_stale_disabled_and_forged_receipt_do_not_charge() {
        let (e, revision) = setup(10);
        assert!(charge(&e, "stale", "stale", NOW).is_err());
        let mut r = receipt("forged");
        r.file_path = "other.html".into();
        assert!(e
            .charge_response_template(&revision, "landing", "model-a", r, NOW)
            .is_err());
        let mut r = receipt("forged");
        r.completion = "forged".into();
        assert!(e
            .charge_response_template(&revision, "landing", "model-a", r, NOW)
            .is_err());
        assert!(e
            .charge_response_template(&revision, "landing", "other", receipt("unknown"), NOW)
            .is_err());
        let mut r = rules(10);
        r[0].enabled = false;
        let config = e.publish_response_templates(update(&e, r), NOW).unwrap();
        assert!(charge(&e, &revision, "old", NOW).is_err());
        assert!(charge(&e, &config.revision, "disabled", NOW).is_err());
        assert!(e.ledger_entries().is_empty());
    }

    #[test]
    fn response_template_duplicate_receipt_scope_and_exact_expiry() {
        let (e, revision) = setup(10);
        charge(&e, &revision, "one", NOW).unwrap();
        assert!(e
            .response_template_receipt("other", "conversation", "tool-one", NOW)
            .is_none());
        assert!(e
            .response_template_receipt("card", "other", "tool-one", NOW)
            .is_none());
        assert!(e
            .response_template_receipt("card", "conversation", "tool-one", NOW - 1)
            .is_none());
        assert!(e
            .response_template_receipt("card", "conversation", "tool-one", NOW + RECEIPT_TTL - 1)
            .is_some());
        assert!(e
            .response_template_receipt("card", "conversation", "tool-one", NOW + RECEIPT_TTL)
            .is_none());
        let mut duplicate = receipt("two");
        duplicate.tool_use_id = "tool-one".into();
        assert!(matches!(
            e.charge_response_template(&revision, "landing", "model-a", duplicate, NOW),
            Err(BillingError::DuplicateInvocation(_))
        ));
        charge(&e, &revision, "new", NOW + RECEIPT_TTL).unwrap();
        assert_eq!(e.export_snapshot().response_template_receipts.len(), 1);
        assert!(matches!(
            charge(&e, &revision, "one", NOW + RECEIPT_TTL),
            Err(BillingError::DuplicateInvocation(_))
        ));
    }

    #[test]
    fn response_template_receipt_cap_refuses_without_evicting_or_charging() {
        let (e, revision) = setup(0);
        let mut snapshot = e.export_snapshot();
        snapshot.response_template_receipts = (0..MAX_RECEIPTS)
            .map(|i| {
                let mut r = receipt(&format!("old-{i}"));
                r.created_at_secs = NOW;
                r
            })
            .collect();
        e.import_snapshot(snapshot);
        assert!(charge(&e, &revision, "overflow", NOW).is_err());
        assert_eq!(
            e.export_snapshot().response_template_receipts.len(),
            MAX_RECEIPTS
        );
        assert!(e.ledger_entries().is_empty());
        assert!(charge(&e, &revision, "rollback", NOW - 1).is_err());
        charge(&e, &revision, "after-expiry", NOW + RECEIPT_TTL).unwrap();
        assert_eq!(e.export_snapshot().response_template_receipts.len(), 1);
    }

    #[test]
    fn response_template_atomic_failure_then_durable_restart_and_archive_replay() {
        let (e, revision) = setup(50);
        let temp = Temp::new();
        e.set_persistence_path(temp.state());
        e.save_to_file(temp.state()).unwrap();
        let before = e.export_snapshot();
        e.inject_persistence_fault(true);
        assert!(matches!(
            charge(&e, &revision, "one", NOW),
            Err(BillingError::Persistence(_))
        ));
        assert_eq!(e.get_card("card").unwrap().credit_used, 0);
        assert!(e
            .response_template_receipt("card", "conversation", "tool-one", NOW)
            .is_none());
        assert!(e.ledger_entries().is_empty());
        assert!(matches!(
            e.publish_response_templates(update(&e, rules(90)), NOW),
            Err(BillingError::Persistence(_))
        ));
        assert_eq!(e.response_template_config(), before.response_templates);
        let unchanged = BillingEngine::new();
        unchanged.load_from_file(temp.state()).unwrap();
        assert_eq!(unchanged.get_card("card").unwrap().credit_used, 0);
        e.inject_persistence_fault(false);
        charge(&e, &revision, "one", NOW).unwrap();
        let restarted = BillingEngine::new();
        restarted.load_from_file(temp.state()).unwrap();
        assert_eq!(restarted.get_card("card").unwrap().credit_used, 50);
        assert_eq!(
            restarted.response_template_config(),
            e.response_template_config()
        );
        assert!(restarted
            .response_template_receipt("card", "conversation", "tool-one", NOW)
            .is_some());
        assert!(matches!(
            charge(&restarted, &revision, "one", NOW),
            Err(BillingError::DuplicateInvocation(_))
        ));
        e.archive_ledger(NOW + 1, &temp.0.join("archives")).unwrap();
        let restarted = BillingEngine::new();
        restarted.load_from_file(temp.state()).unwrap();
        assert!(restarted.ledger_entries().is_empty());
        assert!(restarted
            .response_template_receipt("card", "conversation", "tool-one", NOW)
            .is_some());
        assert!(matches!(
            charge(&restarted, &revision, "one", NOW + RECEIPT_TTL * 2),
            Err(BillingError::DuplicateInvocation(_))
        ));
        assert!(matches!(
            restarted.reserve(
                "card",
                "one",
                &ReservationEstimateParams::new(0, 0),
                NOW + RECEIPT_TTL * 2,
                60
            ),
            Err(BillingError::DuplicateInvocation(_))
        ));
        validate_snapshot(&restarted.export_snapshot()).unwrap();
    }

    #[test]
    fn response_template_snapshot_roundtrip_and_legacy_defaults() {
        let (e, revision) = setup(10);
        charge(&e, &revision, "one", NOW).unwrap();
        let json = serde_json::to_vec(&e.export_snapshot()).unwrap();
        let snapshot: BillingSnapshot = serde_json::from_slice(&json).unwrap();
        validate_snapshot(&snapshot).unwrap();
        let restored = BillingEngine::new();
        restored.import_snapshot(snapshot);
        assert_eq!(
            restored.response_template_config(),
            e.response_template_config()
        );
        assert_eq!(
            restored.response_template_receipt("card", "conversation", "tool-one", NOW),
            e.response_template_receipt("card", "conversation", "tool-one", NOW)
        );
        let mut historical = serde_json::to_value(BillingEngine::new().export_snapshot()).unwrap();
        historical
            .as_object_mut()
            .unwrap()
            .remove("response_templates");
        historical
            .as_object_mut()
            .unwrap()
            .remove("response_template_receipts");
        historical["archived_ledger_summary"]
            .as_object_mut()
            .unwrap()
            .remove("usage_invocation_ids");
        let snapshot: BillingSnapshot = serde_json::from_value(historical).unwrap();
        assert_eq!(
            snapshot.response_templates,
            ResponseTemplateConfig::default()
        );
        assert!(snapshot.response_template_receipts.is_empty());
        validate_snapshot(&snapshot).unwrap();
    }

    #[test]
    fn response_template_revision_is_independent_full_replacement_and_no_aba() {
        let (e, first) = setup(10);
        let commercial = e.commercial_config().revision;
        let stale = update(&e, rules(20));
        let empty = e
            .publish_response_templates(update(&e, vec![]), NOW)
            .unwrap();
        assert!(empty.rules.is_empty());
        assert!(e.publish_response_templates(stale, NOW).is_err());
        let restored = e
            .publish_response_templates(update(&e, rules(10)), NOW)
            .unwrap();
        assert_ne!(first, restored.revision);
        assert_eq!(restored.audit.len(), 3);
        assert_eq!(restored.audit[2].previous_revision, empty.revision);
        assert_eq!(e.commercial_config().revision, commercial);
        validate_snapshot(&e.export_snapshot()).unwrap();
    }

    #[test]
    fn response_template_rejects_unsafe_html_paths() {
        for path in [
            "/abs.html",
            "../out.html",
            "a/../out.html",
            "./out.html",
            "a//out.html",
            "C:/out.html",
            "C:out.html",
            "a\\out.html",
            "a\0.html",
            "a\n.html",
            "a:b.html",
            "CON.html",
            "con/ok.html",
            "AUX.x.html",
            "nul.html",
            "COM1.html",
            "Lpt9/ok.html",
            "COM¹.html",
            "CONIN$.html",
            "a /ok.html",
            "a./ok.html",
            "a.html ",
            "a?.html",
            "a.txt",
        ] {
            let mut r = rules(0);
            r[0].variants[0].file_path = path.into();
            assert!(
                validate_response_template_rules(&r).is_err(),
                "accepted {path:?}"
            );
        }
        for path in [
            "index.html",
            "dir/index.HTML",
            "file.htm",
            "页面/页面.html",
            "company/index.html",
        ] {
            assert!(safe_html_path(path), "rejected {path:?}");
        }
    }

    #[test]
    fn response_template_validation_limits_and_unknown_fields() {
        let (e, _) = setup(10);
        let before = e.response_template_config();
        let mut cases = Vec::new();
        let mut r = rules(-1);
        cases.push(r.clone());
        r[0].variants[0].price_microcredits = 1_000_000_001;
        cases.push(r);
        let mut r = rules(0);
        r[0].match_mode = "regex".into();
        cases.push(r);
        let mut r = rules(0);
        r[0].match_text = " ".into();
        cases.push(r);
        let mut r = rules(0);
        r[0].name = "x".repeat(257);
        cases.push(r);
        let mut r = rules(0);
        r[0].variants[0].completion = "x".repeat(4097);
        cases.push(r);
        let mut r = rules(0);
        r[0].variants[0].content = "x".repeat(256 * 1024 + 1);
        cases.push(r);
        let mut r = rules(0);
        let duplicate = r[0].variants[0].clone();
        r[0].variants.push(duplicate);
        cases.push(r);
        let mut r = rules(0);
        r.push(r[0].clone());
        cases.push(r);
        let mut r = rules(0);
        r[0].variants.clear();
        cases.push(r);
        let mut r = rules(0);
        r[0].variants = (0..33)
            .map(|i| {
                let mut v = rules(0).remove(0).variants.remove(0);
                v.model_id = format!("model-{i}");
                v
            })
            .collect();
        cases.push(r);
        let mut r = rules(0);
        r[0].variants = (0..9)
            .map(|i| {
                let mut v = rules(0).remove(0).variants.remove(0);
                v.model_id = format!("model-{i}");
                v.content = "x".repeat(256 * 1024);
                v
            })
            .collect();
        cases.push(r);
        cases.push(
            (0..33)
                .map(|i| {
                    let mut r = rules(0).remove(0);
                    r.id = format!("rule-{i}");
                    r
                })
                .collect(),
        );
        for r in cases {
            assert!(e.publish_response_templates(update(&e, r), NOW).is_err());
        }
        let mut bad = update(&e, rules(0));
        bad.reason.clear();
        assert!(e.publish_response_templates(bad, NOW).is_err());
        assert_eq!(e.response_template_config(), before);
        let value = serde_json::to_value(update(&e, rules(0))).unwrap();
        for level in 0..3 {
            let mut v = value.clone();
            let object = match level {
                0 => &mut v,
                1 => &mut v["rules"][0],
                _ => &mut v["rules"][0]["variants"][0],
            };
            object["unknown"] = true.into();
            assert!(serde_json::from_value::<ResponseTemplateUpdate>(v).is_err());
        }
        assert!(validate_response_template_rules(&rules(1_000_000_000)).is_ok());
        let mut r = rules(0);
        r[0].variants[0].content = "x".repeat(256 * 1024);
        assert!(validate_response_template_rules(&r).is_ok());
    }

    #[test]
    fn response_template_concurrent_duplicate_and_distinct_debits_are_serialized() {
        for same_id in [true, false] {
            let (e, revision) = setup(600);
            let barrier = Arc::new(std::sync::Barrier::new(3));
            let workers: Vec<_> = (0..2)
                .map(|i| {
                    let e = e.clone();
                    let revision = revision.clone();
                    let barrier = barrier.clone();
                    std::thread::spawn(move || {
                        barrier.wait();
                        charge(
                            &e,
                            &revision,
                            if same_id || i == 0 { "one" } else { "two" },
                            NOW,
                        )
                    })
                })
                .collect();
            barrier.wait();
            assert_eq!(
                workers
                    .into_iter()
                    .map(|w| usize::from(w.join().unwrap().is_ok()))
                    .sum::<usize>(),
                1
            );
            assert_eq!(e.get_card("card").unwrap().credit_used, 600);
            assert_eq!(e.ledger_entries().len(), 1);
            validate_snapshot(&e.export_snapshot()).unwrap();
        }
    }
    #[test]
    fn response_template_serialized_bytes_and_forged_payload_are_bounded() {
        let mut r = receipt("budget");
        r.content = "\u{0001}".repeat(256 * 1024);
        assert!(validate_receipt_budget(&[r.clone()]).is_ok());
        assert!(validate_receipt_budget(&[r.clone(), r.clone()]).is_err());
        let receipts: Vec<_> = (0..12)
            .map(|i| {
                let mut item = r.clone();
                item.card_id = format!("card-{i}");
                item
            })
            .collect();
        assert!(validate_receipt_budget(&receipts).is_err());
        let (e, revision) = setup(10);
        for field in ["content", "price", "keys"] {
            let mut r = receipt(field);
            match field {
                "content" => r.content = "different".into(),
                "price" => r.price_microcredits = 11,
                _ => r.content_key = "content".into(),
            }
            assert!(e
                .charge_response_template(&revision, "landing", "model-a", r, NOW)
                .is_err());
        }
        assert!(e.ledger_entries().is_empty());
    }
}
