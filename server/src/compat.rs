//! AST version compatibility layer.
//!
//! When a client connects with an older AST version the server can, in some
//! cases, transparently **downcast** the incoming message to the format the
//! current engine expects, instead of requiring an immediate client reload.
//!
//! ## Version negotiation decision tree
//!
//! ```text
//!  Client ast_version
//!       │
//!       ├─ current (1.0.0)  ──► pass through unchanged
//!       │
//!       ├─ older, compat    ──► transform_ast() → normalised message
//!       │   (e.g. "1.0.x")
//!       │
//!       └─ too old / unknown ──► ReloadRequired
//! ```
//!
//! ## Adding a new schema version
//!
//! 1. Bump `CURRENT_AST_VERSION` to the new semver string.
//! 2. Add a transformation arm in [`transform_ast`] that converts the old
//!    format to the new one.
//! 3. Add the new version string to `COMPAT_VERSIONS`.

use crate::wire::{ClientMessage, ReloadRequired};

/// The AST schema version this server instance natively understands.
pub const CURRENT_AST_VERSION: &str = "1.0.0";

/// All declared compat versions that can be transparently up-converted.
/// Does NOT need to include `CURRENT_AST_VERSION` (that is always accepted
/// via the equality check in [`check_version`]).
pub const COMPAT_VERSIONS: &[&str] = &[
    // Add older versions here as the schema evolves.
    // e.g. "0.9.0" when upgrading from 0.9 → 1.0 with a transformation.
];

/// Determine whether the given client `ast_version` requires a reload.
///
/// Returns `Ok(())` if the version is compatible (either current or covered
/// by the compat layer), or `Err(ReloadRequired)` if the client must fetch a
/// fresh policy bundle.
pub fn check_version(ast_version: &str) -> Result<(), ReloadRequired> {
    if COMPAT_VERSIONS.contains(&ast_version) || ast_version == CURRENT_AST_VERSION {
        Ok(())
    } else {
        Err(ReloadRequired {
            min_required_version: CURRENT_AST_VERSION.to_owned(),
            reason: format!(
                "AST version '{}' is not supported; minimum required version is '{}'",
                ast_version, CURRENT_AST_VERSION,
            ),
        })
    }
}

/// Apply any necessary schema transformations to bring `msg` up to the
/// current AST format.
///
/// For versions already at `CURRENT_AST_VERSION` this is a no-op clone.
/// For older compat versions the function rewrites the relevant fields.
pub fn transform_ast(msg: &ClientMessage) -> ClientMessage {
    match msg.ast_version.as_str() {
        // Current version — pass through unchanged.
        v if v == CURRENT_AST_VERSION => msg.clone(),
        // Future: add transformation arms here for older compat versions.
        // e.g. "0.9.0" => apply_v0_9_to_v1_0(msg),
        _ => {
            // Unknown version: return as-is; caller already checked via
            // `check_version` and will have rejected if truly incompatible.
            msg.clone()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_version_accepted() {
        assert!(check_version(CURRENT_AST_VERSION).is_ok());
    }

    #[test]
    fn unknown_version_rejected() {
        let err = check_version("99.0.0").unwrap_err();
        assert_eq!(err.min_required_version, CURRENT_AST_VERSION);
        assert!(err.reason.contains("99.0.0"));
    }

    #[test]
    fn compat_version_accepted() {
        for v in COMPAT_VERSIONS {
            assert!(
                check_version(v).is_ok(),
                "compat version {} should be accepted",
                v
            );
        }
    }

    #[test]
    fn transform_current_is_identity() {
        let msg = make_msg(CURRENT_AST_VERSION);
        let out = transform_ast(&msg);
        assert_eq!(out.ast_version, CURRENT_AST_VERSION);
        assert_eq!(out.op_id, msg.op_id);
    }

    // ── helpers ──────────────────────────────────────────────────────────────

    fn make_msg(version: &str) -> ClientMessage {
        ClientMessage {
            op_id: "test-op".into(),
            ast_version: version.into(),
            action: "test".into(),
            payload: serde_json::Value::Null,
            client_snapshot_hash: "abc123".into(),
            tenant_id: "t1".into(),
            workflow_id: "w1".into(),
            instance_id: "i1".into(),
            client_signature: None,
            role: "default".into(),
            vibe: None,
        }
    }
}
