//! Runtime provider registry shared by the admin importer and conversation
//! pipeline.  Persisting a provider is not useful if the request path keeps a
//! stale startup-only provider list, so both surfaces update this registry.

use super::governance::{KeyHealth, ProviderKeyPool};
use billing::group::Group;
use billing::provider::{Provider, ProviderKey};
use billing::BillingEngine;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};

#[derive(Clone, Default)]
pub struct ProviderRuntimeRegistry {
    inner: Arc<RwLock<RuntimeState>>,
}

#[derive(Default)]
struct RuntimeState {
    pools: HashMap<String, ProviderKeyPool>,
    default_provider_id: Option<String>,
}

impl ProviderRuntimeRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&self, provider: Provider, key: ProviderKey) {
        let mut state = self.inner.write().expect("provider runtime lock poisoned");
        let provider_id = provider.id.clone();
        if let Some(existing) = state.pools.get(&provider_id) {
            existing.update_provider(provider);
            existing.add_key(key);
        } else {
            state.pools.insert(
                provider_id.clone(),
                ProviderKeyPool::new(provider, vec![key]),
            );
        }
        state.default_provider_id.get_or_insert(provider_id);
    }

    pub fn sync_from_billing(&self, billing: &BillingEngine) {
        let billing_providers = billing.list_providers();
        let billing_keys = billing.get_runtime_provider_keys(None);

        let mut keys_by_provider: HashMap<String, Vec<ProviderKey>> = HashMap::new();
        for key in billing_keys {
            keys_by_provider
                .entry(key.provider_id.clone())
                .or_default()
                .push(key);
        }

        let mut state = self.inner.write().expect("provider runtime lock poisoned");
        let active_ids: HashSet<String> = billing_providers.iter().map(|p| p.id.clone()).collect();
        state.pools.retain(|id, _| active_ids.contains(id));

        for provider in billing_providers {
            let provider_id = provider.id.clone();
            let keys = keys_by_provider.remove(&provider_id).unwrap_or_default();
            if let Some(existing) = state.pools.get(&provider_id) {
                existing.update_provider(provider);
                let current_key_ids: HashSet<String> =
                    existing.list_keys().into_iter().map(|k| k.id).collect();
                let new_key_ids: HashSet<String> = keys.iter().map(|k| k.id.clone()).collect();
                for id in current_key_ids {
                    if !new_key_ids.contains(&id) {
                        existing.remove_key(&id);
                    }
                }
                for key in keys {
                    existing.add_key(key);
                }
            } else {
                state
                    .pools
                    .insert(provider_id.clone(), ProviderKeyPool::new(provider, keys));
            }
        }

        if state
            .default_provider_id
            .as_ref()
            .is_none_or(|id| !state.pools.get(id).is_some_and(|p| p.provider().enabled))
        {
            state.default_provider_id = state
                .pools
                .iter()
                .find(|(_, p)| p.provider().enabled)
                .map(|(id, _)| id.clone());
        }
    }

    pub fn pool_for(&self, provider_id: &str) -> Option<ProviderKeyPool> {
        self.inner.read().ok()?.pools.get(provider_id).cloned()
    }

    /// The live health of every key the gateway routes with, by key ID.
    pub fn key_health(&self, now_secs: u64) -> HashMap<String, KeyHealth> {
        let pools: Vec<ProviderKeyPool> = self
            .inner
            .read()
            .map(|state| state.pools.values().cloned().collect())
            .unwrap_or_default();
        pools
            .iter()
            .flat_map(|pool| pool.key_health(now_secs))
            .collect()
    }

    /// Clear a key's cooldown or retirement. A retired key was also switched off here, so
    /// the registry is synced first: the key serves again unless it is saved disabled.
    /// False when the gateway does not route with it.
    pub fn reset_key(&self, billing: &BillingEngine, provider_id: &str, key_id: &str) -> bool {
        self.sync_from_billing(billing);
        self.pool_for(provider_id)
            .is_some_and(|pool| pool.reset_key(key_id))
    }

    pub fn default_pool(&self) -> Option<ProviderKeyPool> {
        let state = self.inner.read().ok()?;
        if let Some(id) = state.default_provider_id.as_deref() {
            if let Some(pool) = state.pools.get(id) {
                if pool.provider().enabled {
                    return Some(pool.clone());
                }
            }
        }
        state
            .pools
            .values()
            .find(|pool| pool.provider().enabled)
            .cloned()
    }

    pub fn find_pool_for_group(&self, group: &Group) -> Option<ProviderKeyPool> {
        let state = self.inner.read().ok()?;
        if let Some(id) = state.default_provider_id.as_deref() {
            if let Some(pool) = state.pools.get(id) {
                let p = pool.provider();
                if p.enabled && group.can_access_provider(p.group_id.as_deref()) {
                    return Some(pool.clone());
                }
            }
        }
        state
            .pools
            .values()
            .find(|pool| {
                let p = pool.provider();
                p.enabled && group.can_access_provider(p.group_id.as_deref())
            })
            .cloned()
    }

    pub fn has_available_provider(&self) -> bool {
        self.inner
            .read()
            .map(|state| state.pools.values().any(|p| p.provider().enabled))
            .unwrap_or(false)
    }

    pub fn provider_ids(&self) -> Vec<String> {
        let mut ids: Vec<_> = self
            .inner
            .read()
            .map(|state| state.pools.keys().cloned().collect())
            .unwrap_or_default();
        ids.sort();
        ids
    }
}
