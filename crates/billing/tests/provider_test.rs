use billing::group::Group;
use billing::provider::{HealthState, Provider, ProviderFormat, ProviderKey};
use std::time::Duration;

#[test]
fn test_provider_initialization_and_availability() {
    let mut provider = Provider::new(
        "prov-1",
        "DeepSeek Direct",
        ProviderFormat::OpenAi,
        "https://api.deepseek.com/v1",
    )
    .with_weight(5);

    assert_eq!(provider.id, "prov-1");
    assert_eq!(provider.weight, 5);
    assert_eq!(provider.health_state, HealthState::Healthy);
    assert!(provider.is_available(1000));

    // Cooldown test
    provider.mark_failure(1000, Duration::from_secs(60));
    assert_eq!(provider.health_state, HealthState::Degraded);
    assert_eq!(provider.cooldown_until, Some(1060));
    assert!(!provider.is_available(1030));
    assert!(provider.is_available(1060));
    assert!(provider.is_available(1100));

    // Recovery test
    provider.mark_success();
    assert_eq!(provider.health_state, HealthState::Healthy);
    assert_eq!(provider.cooldown_until, None);
    assert!(provider.is_available(1030));

    // Disabled or unhealthy
    provider.health_state = HealthState::Unhealthy;
    assert!(!provider.is_available(1030));
}

#[test]
fn test_provider_group_access_shared_vs_dedicated() {
    let shared_prov = Provider::new(
        "p-shared",
        "Shared OpenAI",
        ProviderFormat::OpenAi,
        "https://api.openai.com/v1",
    );

    let dedicated_prov = Provider::new(
        "p-ded",
        "Dedicated Claude",
        ProviderFormat::Anthropic,
        "https://api.anthropic.com/v1",
    )
    .with_group("grp-vip");

    let pro_plus = Group::pro_plus("grp-pro", "Pro Users");
    let vip_group = Group::enterprise("grp-vip", "VIP Enterprise", "ENTERPRISE", None);
    let other_vip = Group::enterprise("grp-other", "Other Enterprise", "ENTERPRISE", None);

    // Shared provider should only be accessible to Shared group
    assert!(shared_prov.can_access(&pro_plus));
    assert!(!shared_prov.can_access(&vip_group));
    assert!(!shared_prov.can_access(&other_vip));

    // Dedicated provider should only be accessible to grp-vip
    assert!(!dedicated_prov.can_access(&pro_plus));
    assert!(dedicated_prov.can_access(&vip_group));
    assert!(!dedicated_prov.can_access(&other_vip));
}

#[test]
fn test_provider_key_lifecycle_and_cooldown() {
    let mut key = ProviderKey::new("key-1", "prov-1", "sk-test-secret-123").with_weight(3);

    assert_eq!(key.id, "key-1");
    assert_eq!(key.provider_id, "prov-1");
    assert_eq!(key.api_key, "sk-test-secret-123");
    assert_eq!(key.weight, 3);
    assert_eq!(key.health_state, HealthState::Healthy);
    assert!(key.is_available(2000));

    // Failure triggers cooldown
    key.mark_failure(2000, Duration::from_secs(30));
    assert_eq!(key.health_state, HealthState::Degraded);
    assert_eq!(key.cooldown_until, Some(2030));
    assert!(!key.is_available(2015));
    assert!(key.is_available(2030));

    // Success restores healthy state
    key.mark_success();
    assert_eq!(key.health_state, HealthState::Healthy);
    assert_eq!(key.cooldown_until, None);
    assert!(key.is_available(2015));

    // Disabled key is unavailable
    key.enabled = false;
    assert!(!key.is_available(2015));
}
