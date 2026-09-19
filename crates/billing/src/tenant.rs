//! Multi-tenant Row-Level Security and Scope Isolation.
//!
//! Spec §5 (Data Model: Tenant Group Virtualization) & Spec §7 (Security: Multi-tenant RLS).

use crate::card::Card;
use crate::group::ModelMap;
use thiserror::Error;

#[derive(Error, Debug, PartialEq, Eq, Clone)]
pub enum TenantViolation {
    #[error("Cross-tenant access forbidden: expected tenant group '{expected}', attempted to access '{attempted}'")]
    CrossTenantAccessForbidden { expected: String, attempted: String },

    #[allow(dead_code)] // ponytail: reserved for future RLS enforcement
    #[error(
        "Tenant context required: operation requires an active tenant group but none was provided"
    )]
    TenantContextRequired,
}

/// Tenant execution context for enforcing isolation boundaries (Spec §7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TenantContext {
    /// Active group / tenant ID. `None` indicates system admin scope.
    pub active_group_id: Option<String>,
}

impl TenantContext {
    /// Create context scoped to a specific tenant group.
    pub fn tenant(group_id: impl Into<String>) -> Self {
        Self {
            active_group_id: Some(group_id.into()),
        }
    }

    /// Create system admin context with unrestricted access across all groups.
    pub fn system_admin() -> Self {
        Self {
            active_group_id: None,
        }
    }

    /// Returns `true` if this is a system admin context.
    pub fn is_admin(&self) -> bool {
        self.active_group_id.is_none()
    }

    /// Validate access to a card entity against the tenant context.
    pub fn enforce_card_access(&self, card: &Card) -> Result<(), TenantViolation> {
        if let Some(expected) = &self.active_group_id {
            if card.group_id != *expected {
                return Err(TenantViolation::CrossTenantAccessForbidden {
                    expected: expected.clone(),
                    attempted: card.group_id.clone(),
                });
            }
        }
        Ok(())
    }

    /// Validate access to a target group ID.
    pub fn enforce_group_access(&self, target_group_id: &str) -> Result<(), TenantViolation> {
        if let Some(expected) = &self.active_group_id {
            if target_group_id != expected {
                return Err(TenantViolation::CrossTenantAccessForbidden {
                    expected: expected.clone(),
                    attempted: target_group_id.to_string(),
                });
            }
        }
        Ok(())
    }

    /// Validate access to a model map configuration.
    pub fn enforce_model_access(&self, model: &ModelMap) -> Result<(), TenantViolation> {
        if let Some(expected) = &self.active_group_id {
            if model.group_id != *expected {
                return Err(TenantViolation::CrossTenantAccessForbidden {
                    expected: expected.clone(),
                    attempted: model.group_id.clone(),
                });
            }
        }
        Ok(())
    }

    /// Filter a slice of model mappings according to tenant group boundaries.
    pub fn filter_models(&self, models: &[ModelMap]) -> Vec<ModelMap> {
        match &self.active_group_id {
            Some(group_id) => models
                .iter()
                .filter(|m| m.group_id == *group_id)
                .cloned()
                .collect(),
            None => models.to_vec(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tenant_context_allows_own_group() {
        let ctx = TenantContext::tenant("grp-alpha");
        let card = Card::new("card-1", "grp-alpha", 1_000_000);
        assert!(ctx.enforce_card_access(&card).is_ok());
        assert!(ctx.enforce_group_access("grp-alpha").is_ok());
    }

    #[test]
    fn test_tenant_context_rejects_other_group() {
        let ctx = TenantContext::tenant("grp-alpha");
        let card = Card::new("card-2", "grp-beta", 1_000_000);
        let err = ctx.enforce_card_access(&card);
        assert_eq!(
            err,
            Err(TenantViolation::CrossTenantAccessForbidden {
                expected: "grp-alpha".to_string(),
                attempted: "grp-beta".to_string()
            })
        );
        assert!(ctx.enforce_group_access("grp-beta").is_err());
    }

    #[test]
    fn test_system_admin_accesses_all_groups() {
        let admin = TenantContext::system_admin();
        let card_a = Card::new("card-1", "grp-alpha", 1_000_000);
        let card_b = Card::new("card-2", "grp-beta", 1_000_000);
        assert!(admin.enforce_card_access(&card_a).is_ok());
        assert!(admin.enforce_card_access(&card_b).is_ok());
        assert!(admin.enforce_group_access("grp-alpha").is_ok());
        assert!(admin.enforce_group_access("grp-beta").is_ok());
    }
}
