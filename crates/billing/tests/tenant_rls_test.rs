use billing::card::Card;
use billing::group::ModelMap;
use billing::tenant::{TenantContext, TenantViolation};

#[test]
fn test_tenant_context_scoping_card_isolation() {
    let tenant_alpha = TenantContext::tenant("group-alpha");
    let tenant_beta = TenantContext::tenant("group-beta");

    let card_alpha = Card::new("card-a", "group-alpha", 10_000_000);
    let card_beta = Card::new("card-b", "group-beta", 10_000_000);

    // Tenant Alpha accesses card Alpha -> OK
    assert!(tenant_alpha.enforce_card_access(&card_alpha).is_ok());

    // Tenant Alpha accesses card Beta -> Blocked with TenantViolation
    let err = tenant_alpha.enforce_card_access(&card_beta);
    assert_eq!(
        err,
        Err(TenantViolation::CrossTenantAccessForbidden {
            expected: "group-alpha".to_string(),
            attempted: "group-beta".to_string(),
        })
    );

    // Tenant Beta accesses card Alpha -> Blocked
    assert!(tenant_beta.enforce_card_access(&card_alpha).is_err());
    // Tenant Beta accesses card Beta -> OK
    assert!(tenant_beta.enforce_card_access(&card_beta).is_ok());
}

#[test]
fn test_tenant_context_group_and_model_filtering() {
    let tenant_alpha = TenantContext::tenant("group-alpha");

    let model_a = ModelMap::new("m-1", "group-alpha", "claude-3-5", "prov-1", "claude-3-5");
    let model_b = ModelMap::new("m-2", "group-beta", "gpt-4o", "prov-2", "gpt-4o");

    assert!(tenant_alpha.enforce_model_access(&model_a).is_ok());
    assert!(tenant_alpha.enforce_model_access(&model_b).is_err());

    let all_models = vec![model_a.clone(), model_b.clone()];
    let visible = tenant_alpha.filter_models(&all_models);
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].exposed_model_id, "claude-3-5");
}

#[test]
fn test_system_admin_unrestricted_cross_tenant_access() {
    let admin = TenantContext::system_admin();
    assert!(admin.is_admin());

    let card_a = Card::new("card-a", "group-alpha", 10_000_000);
    let card_b = Card::new("card-b", "group-beta", 10_000_000);
    let model_b = ModelMap::new("m-2", "group-beta", "gpt-4o", "prov-2", "gpt-4o");

    assert!(admin.enforce_card_access(&card_a).is_ok());
    assert!(admin.enforce_card_access(&card_b).is_ok());
    assert!(admin.enforce_model_access(&model_b).is_ok());

    let all_models = vec![model_b.clone()];
    let visible = admin.filter_models(&all_models);
    assert_eq!(visible.len(), 1);
}
