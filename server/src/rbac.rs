//! Role-Based Access Control (RBAC) with AST-based guards.
//!
//! Workflows are scoped by `(tenant_id, role)`.  Every signal and activity
//! start is guarded by an AST policy evaluated against the caller's
//! [`TenantContext`].  Cross-tenant data access is structurally impossible
//! because all state lookups are partitioned by `tenant_id`.

use policy_core::{AstNode, Evaluator};
use serde::{Deserialize, Serialize};

/// A named role within a tenant (e.g. `"admin"`, `"reviewer"`, `"agent"`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Role(pub String);

/// A granular permission string (e.g. `"workflow:start"`, `"workflow:signal"`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Permission(pub String);

/// All identity and authorization context for a single request.
///
/// Built from the incoming [`crate::wire::ClientMessage`] before any
/// workflow or activity logic runs.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TenantContext {
    /// Tenant the caller belongs to.
    pub tenant_id: String,
    /// The caller's role within the tenant.
    pub role: Role,
    /// Effective permissions derived from the role (expanded by the
    /// [`RbacRegistry`]).
    pub permissions: Vec<Permission>,
}

impl TenantContext {
    /// Return `true` iff this context grants `permission`.
    pub fn has_permission(&self, permission: &str) -> bool {
        self.permissions.iter().any(|p| p.0 == permission)
    }
}

/// Evaluate an AST policy node against a [`TenantContext`].
///
/// The context is serialized to a JSON object so that policies can reference
/// paths like `/role`, `/tenant_id`, and `/permissions`.
///
/// Returns `true` when the policy allows access, `false` on any error or
/// explicit denial.
pub fn evaluate_rbac_guard(ctx: &TenantContext, policy: &AstNode) -> bool {
    let state = match serde_json::to_value(ctx) {
        Ok(v) => v,
        Err(_) => return false,
    };
    Evaluator::new()
        .evaluate(&state, policy)
        .map(|r| r.result)
        .unwrap_or(false)
}

/// In-memory RBAC registry mapping roles to their permissions.
///
/// In production this would be backed by a database or an external
/// identity provider.  For now it is initialized with a sensible default
/// role hierarchy.
#[derive(Debug, Default)]
pub struct RbacRegistry {
    /// role name → list of permissions
    roles: std::collections::HashMap<String, Vec<Permission>>,
}

impl RbacRegistry {
    /// Create a registry pre-populated with the built-in role hierarchy.
    pub fn with_defaults() -> Self {
        let mut r = Self::default();
        r.define(
            "admin",
            &[
                "workflow:start",
                "workflow:signal",
                "workflow:cancel",
                "activity:execute",
                "token:mint",
                "token:redeem",
                "ai:propose",
            ],
        );
        r.define(
            "reviewer",
            &[
                "workflow:signal",
                "activity:execute",
                "token:redeem",
                "ai:propose",
            ],
        );
        r.define("agent", &["ai:propose", "token:redeem"]);
        r.define("default", &["workflow:start", "activity:execute"]);
        r
    }

    /// Create a registry loaded from the `RBAC_CONFIG_JSON` environment variable.
    ///
    /// The variable must contain a JSON object whose keys are role names and
    /// whose values are arrays of permission strings.  Example:
    ///
    /// ```json
    /// {
    ///   "admin": ["workflow:start", "workflow:signal", "workflow:cancel"],
    ///   "reviewer": ["workflow:signal", "activity:execute"]
    /// }
    /// ```
    ///
    /// Falls back to [`Self::with_defaults()`] when the variable is absent or
    /// cannot be parsed, emitting a warning so operators notice the misconfiguration.
    pub fn from_env() -> Self {
        if let Ok(raw) = std::env::var("RBAC_CONFIG_JSON") {
            match serde_json::from_str::<std::collections::HashMap<String, Vec<String>>>(&raw) {
                Ok(map) => {
                    let mut registry = Self::default();
                    for (role, perms) in &map {
                        let refs: Vec<&str> = perms.iter().map(String::as_str).collect();
                        registry.define(role, &refs);
                    }
                    tracing::info!(roles = map.len(), "RBAC registry loaded from RBAC_CONFIG_JSON");
                    return registry;
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "RBAC_CONFIG_JSON parse error; falling back to built-in defaults"
                    );
                }
            }
        }
        Self::with_defaults()
    }

    /// Register (or overwrite) the permissions for a role.
    pub fn define(&mut self, role: &str, permissions: &[&str]) {
        self.roles.insert(
            role.to_owned(),
            permissions
                .iter()
                .map(|p| Permission(p.to_string()))
                .collect(),
        );
    }

    /// Build a [`TenantContext`] for the given `(tenant_id, role)` pair.
    ///
    /// Unknown roles receive an empty permission list.
    pub fn build_context(&self, tenant_id: &str, role: &str) -> TenantContext {
        let permissions = self.roles.get(role).cloned().unwrap_or_default();
        TenantContext {
            tenant_id: tenant_id.to_owned(),
            role: Role(role.to_owned()),
            permissions,
        }
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use policy_core::ast::{AstNode, NodeKind};

    fn registry() -> RbacRegistry {
        RbacRegistry::with_defaults()
    }

    #[test]
    fn admin_has_all_permissions() {
        let reg = registry();
        let ctx = reg.build_context("tenant1", "admin");
        assert!(ctx.has_permission("workflow:start"));
        assert!(ctx.has_permission("ai:propose"));
        assert!(ctx.has_permission("token:mint"));
    }

    #[test]
    fn agent_cannot_start_workflow() {
        let reg = registry();
        let ctx = reg.build_context("tenant1", "agent");
        assert!(!ctx.has_permission("workflow:start"));
    }

    #[test]
    fn unknown_role_has_no_permissions() {
        let reg = registry();
        let ctx = reg.build_context("tenant1", "unknown");
        assert!(ctx.permissions.is_empty());
    }

    #[test]
    fn tenant_ids_are_isolated() {
        let reg = registry();
        let ctx1 = reg.build_context("tenant-A", "admin");
        let ctx2 = reg.build_context("tenant-B", "admin");
        // Same role, different tenants — contexts are separate objects
        assert_ne!(ctx1.tenant_id, ctx2.tenant_id);
    }

    #[test]
    fn ast_guard_allows_admin() {
        let reg = registry();
        let ctx = reg.build_context("tenant1", "admin");
        // Policy: role == "admin"
        let policy = AstNode {
            version: None,
            kind: NodeKind::Eq,
            children: vec![
                AstNode {
                    version: None,
                    kind: NodeKind::Field,
                    children: vec![],
                    path: Some("/role".into()),
                    value: None,
                    call_name: None,
                },
                AstNode {
                    version: None,
                    kind: NodeKind::Eq,
                    children: vec![],
                    path: None,
                    value: Some("admin".into()),
                    call_name: None,
                },
            ],
            path: None,
            value: None,
            call_name: None,
        };
        assert!(evaluate_rbac_guard(&ctx, &policy));
    }

    #[test]
    fn ast_guard_denies_non_admin() {
        let reg = registry();
        let ctx = reg.build_context("tenant1", "reviewer");
        let policy = AstNode {
            version: None,
            kind: NodeKind::Eq,
            children: vec![
                AstNode {
                    version: None,
                    kind: NodeKind::Field,
                    children: vec![],
                    path: Some("/role".into()),
                    value: None,
                    call_name: None,
                },
                AstNode {
                    version: None,
                    kind: NodeKind::Eq,
                    children: vec![],
                    path: None,
                    value: Some("admin".into()),
                    call_name: None,
                },
            ],
            path: None,
            value: None,
            call_name: None,
        };
        assert!(!evaluate_rbac_guard(&ctx, &policy));
    }

    /// Shared mutex to serialize env-var–touching tests and prevent races when
    /// Rust runs unit tests in parallel within the same process.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn from_env_falls_back_to_defaults_when_var_unset() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::remove_var("RBAC_CONFIG_JSON");
        let reg = RbacRegistry::from_env();
        let ctx = reg.build_context("t1", "admin");
        assert!(ctx.has_permission("workflow:start"));
    }

    #[test]
    fn from_env_loads_custom_roles_from_env_var() {
        let _guard = ENV_LOCK.lock().unwrap();
        let json = r#"{"ops": ["deploy:run", "deploy:rollback"], "reader": ["deploy:view"]}"#;
        std::env::set_var("RBAC_CONFIG_JSON", json);
        let reg = RbacRegistry::from_env();
        std::env::remove_var("RBAC_CONFIG_JSON");

        let ctx = reg.build_context("t1", "ops");
        assert!(ctx.has_permission("deploy:run"));
        assert!(ctx.has_permission("deploy:rollback"));
        assert!(!ctx.has_permission("deploy:view"));

        let reader = reg.build_context("t1", "reader");
        assert!(reader.has_permission("deploy:view"));
        assert!(!reader.has_permission("deploy:run"));
    }

    #[test]
    fn from_env_falls_back_to_defaults_on_invalid_json() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::set_var("RBAC_CONFIG_JSON", "not-valid-json");
        let reg = RbacRegistry::from_env();
        std::env::remove_var("RBAC_CONFIG_JSON");
        let ctx = reg.build_context("t1", "admin");
        assert!(ctx.has_permission("workflow:start"));
    }
}
