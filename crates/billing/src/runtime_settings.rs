//! Durable, versioned operational settings. New requests take one coherent snapshot.
use super::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TimeoutProfile {
    pub headers_secs: u64,
    pub attempt_secs: u64,
    pub total_secs: u64,
    pub commit_secs: u64,
    pub started_secs: u64,
    pub idle_secs: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeSettings {
    pub standard: TimeoutProfile,
    pub reasoning: TimeoutProfile,
    pub claude: TimeoutProfile,
    pub openai_reasoning_idle_secs: u64,
    pub keepalive_secs: u64,
}
impl Default for RuntimeSettings {
    fn default() -> Self {
        let standard = TimeoutProfile {
            headers_secs: 15,
            attempt_secs: 65,
            total_secs: 150,
            commit_secs: 45,
            started_secs: 600,
            idle_secs: 90,
        };
        let reasoning = TimeoutProfile {
            headers_secs: 45,
            attempt_secs: 90,
            idle_secs: 180,
            ..standard.clone()
        };
        let claude = TimeoutProfile {
            headers_secs: 90,
            attempt_secs: 180,
            total_secs: 300,
            ..reasoning.clone()
        };
        Self {
            standard,
            reasoning,
            claude,
            openai_reasoning_idle_secs: 300,
            keepalive_secs: 20,
        }
    }
}
impl RuntimeSettings {
    /// Covers all three allowed attempts, routing waits and settlement grace, including fallbacks.
    pub fn reservation_ttl_secs(&self) -> u64 {
        let profiles = [&self.standard, &self.reasoning, &self.claude];
        let started = profiles.iter().map(|p| p.started_secs).max().unwrap_or(600);
        let total = profiles.iter().map(|p| p.total_secs).max().unwrap_or(300);
        total
            .saturating_add(started.saturating_mul(3))
            .saturating_add(60)
    }

    pub fn validate(&self) -> Result<(), BillingError> {
        let bad = || {
            BillingError::InvalidState(
                "runtime settings: invalid timeout range or relationship".into(),
            )
        };
        if !(1..=25).contains(&self.keepalive_secs)
            || !(10..=3600).contains(&self.openai_reasoning_idle_secs)
        {
            return Err(bad());
        }
        for p in [&self.standard, &self.reasoning, &self.claude] {
            if !(1..=300).contains(&p.headers_secs)
                || !(1..=900).contains(&p.attempt_secs)
                || !(1..=1800).contains(&p.total_secs)
                || !(1..=45).contains(&p.commit_secs)
                || !(60..=3600).contains(&p.started_secs)
                || !(10..=3600).contains(&p.idle_secs)
                || p.headers_secs > p.attempt_secs
                || p.attempt_secs > p.total_secs
                || p.total_secs > p.started_secs
                || p.idle_secs > p.started_secs
                || self.keepalive_secs >= p.idle_secs
                || p.commit_secs > p.total_secs
            {
                return Err(bad());
            }
        }
        if self.openai_reasoning_idle_secs > self.reasoning.started_secs {
            return Err(bad());
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeSettingsConfig {
    pub revision: String,
    pub settings: RuntimeSettings,
    pub audit: Vec<ResponseTemplateAudit>,
}
impl Default for RuntimeSettingsConfig {
    fn default() -> Self {
        Self {
            revision: "runtime:v1:default".into(),
            settings: RuntimeSettings::default(),
            audit: Vec::new(),
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeSettingsUpdate {
    pub expected_revision: String,
    pub reason: String,
    pub settings: RuntimeSettings,
}
impl BillingEngine {
    pub fn runtime_settings_config(&self) -> RuntimeSettingsConfig {
        let _guard = self.state_lock.read().unwrap();
        self.runtime_settings.read().unwrap().clone()
    }
    pub fn publish_runtime_settings(
        &self,
        update: RuntimeSettingsUpdate,
        now: u64,
    ) -> Result<RuntimeSettingsConfig, BillingError> {
        update.settings.validate()?;
        if !crate::valid_id(&update.expected_revision, 128)
            || !crate::valid_id(&update.reason, 1024)
        {
            return Err(BillingError::InvalidState(
                "runtime settings: revision and reason required".into(),
            ));
        }
        let _guard = self.state_lock.write().unwrap();
        let mut candidate = self.export_snapshot_locked(
            self.snapshot_sequence
                .load(Ordering::Acquire)
                .saturating_add(1),
            self.last_snapshot_checksum.read().unwrap().clone(),
        );
        if candidate.runtime_settings.revision != update.expected_revision {
            return Err(BillingError::InvalidState(
                "runtime settings changed; reload before publishing".into(),
            ));
        }
        let bytes = serde_json::to_vec(&(&update, now))
            .map_err(|_| BillingError::InvalidState("cannot encode runtime settings".into()))?;
        let digest: String = ring::digest::digest(&ring::digest::SHA256, &bytes)
            .as_ref()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let config = &mut candidate.runtime_settings;
        config.revision = format!("runtime:v1:{digest}");
        config.audit.push(ResponseTemplateAudit {
            previous_revision: update.expected_revision,
            revision: config.revision.clone(),
            reason: update.reason,
            created_at_secs: now,
        });
        if config.audit.len() > 128 {
            config.audit.remove(0);
        }
        config.settings = update.settings;
        let result = config.clone();
        self.commit_candidate_snapshot(&candidate, || {
            *self.runtime_settings.write().unwrap() = result.clone();
        })?;
        Ok(result)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn limits_are_bounded_and_coherent() {
        let mut s = RuntimeSettings::default();
        assert!(s.validate().is_ok());
        s.claude.headers_secs = 181;
        assert!(s.validate().is_err());
        s = RuntimeSettings::default();
        s.keepalive_secs = 0;
        assert!(s.validate().is_err());
        s = RuntimeSettings::default();
        s.reasoning.started_secs = 200;
        assert!(s.validate().is_err());
    }
    #[test]
    fn failed_save_keeps_live_and_disk_settings_and_restart_recovers_publication() {
        let temp = std::env::temp_dir().join(format!(
            "billing-runtime-settings-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&temp).unwrap();
        let path = temp.join("state.json");
        let e = BillingEngine::new();
        e.set_persistence_path(&path);
        e.save_to_file(&path).unwrap();
        let before = e.runtime_settings_config();
        let mut settings = before.settings.clone();
        settings.claude.headers_secs = 100;
        let update = RuntimeSettingsUpdate {
            expected_revision: before.revision.clone(),
            reason: "durability test".into(),
            settings,
        };
        e.inject_persistence_fault(true);
        assert!(matches!(
            e.publish_runtime_settings(update.clone(), 1),
            Err(BillingError::Persistence(_))
        ));
        assert_eq!(e.runtime_settings_config(), before);
        let restarted = BillingEngine::new();
        restarted.load_from_file(&path).unwrap();
        assert_eq!(restarted.runtime_settings_config(), before);
        e.inject_persistence_fault(false);
        let saved = e.publish_runtime_settings(update, 2).unwrap();
        let restarted = BillingEngine::new();
        restarted.load_from_file(&path).unwrap();
        assert_eq!(restarted.runtime_settings_config(), saved);
        assert_eq!(saved.audit.len(), 1);
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn publish_is_versioned_and_survives_snapshot() {
        let e = BillingEngine::new();
        let config = e.runtime_settings_config();
        let mut settings = config.settings.clone();
        settings.claude.headers_secs = 100;
        let update = RuntimeSettingsUpdate {
            expected_revision: config.revision,
            reason: "test".into(),
            settings,
        };
        let saved = e.publish_runtime_settings(update.clone(), 1).unwrap();
        assert_eq!(e.export_snapshot().runtime_settings, saved);
        assert!(e.publish_runtime_settings(update, 2).is_err());
    }
}
