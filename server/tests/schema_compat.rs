//! "Time Travel" schema-compatibility tests.
//!
//! Phase 3 requirement: verify that a v1 client connecting to a v2 server
//! (or any unsupported version) receives a `RELOAD_REQUIRED` message and that
//! the connection is terminated gracefully, while a compatible client proceeds
//! normally.
//!
//! These tests exercise the `compat` module and the schema-negotiation logic
//! wired into the WebSocket handler.

use gp2f_server::{
    compat::{check_version, transform_ast, COMPAT_VERSIONS, CURRENT_AST_VERSION},
    wire::{ClientMessage, ServerMessage},
};
use serde_json::json;

// ── helpers ───────────────────────────────────────────────────────────────────

fn make_client_message(version: &str) -> ClientMessage {
    ClientMessage {
        op_id: "test-op-1".into(),
        ast_version: version.into(),
        action: "update".into(),
        payload: json!({ "field": "value" }),
        client_snapshot_hash: "deadbeef".into(),
        tenant_id: "tenant-1".into(),
        workflow_id: "workflow-1".into(),
        instance_id: "instance-1".into(),
        client_signature: None,
        role: "default".into(),
        vibe: None,
        trace_id: None,
    }
}

// ── compat::check_version ─────────────────────────────────────────────────────

/// A client running the current AST version must be accepted.
#[test]
fn current_version_is_accepted() {
    assert!(
        check_version(CURRENT_AST_VERSION).is_ok(),
        "current version {} must always be accepted",
        CURRENT_AST_VERSION,
    );
}

/// All declared compat versions must be accepted without a reload.
#[test]
fn all_compat_versions_accepted() {
    for v in COMPAT_VERSIONS {
        assert!(
            check_version(v).is_ok(),
            "compat version {} should be accepted",
            v
        );
    }
}

/// An unknown / future version must be rejected with RELOAD_REQUIRED.
#[test]
fn unknown_version_triggers_reload_required() {
    let err = check_version("99.0.0").unwrap_err();
    assert_eq!(
        err.min_required_version, CURRENT_AST_VERSION,
        "min_required_version must point to the current server version"
    );
    assert!(
        err.reason.contains("99.0.0"),
        "error reason should mention the rejected version"
    );
}

/// An empty version string must be rejected.
#[test]
fn empty_version_triggers_reload_required() {
    assert!(check_version("").is_err());
}

/// A semver pre-release that is not in the compat list must be rejected.
#[test]
fn prerelease_version_not_in_compat_list_rejected() {
    assert!(check_version("1.0.0-beta.1").is_err());
}

// ── compat::transform_ast ─────────────────────────────────────────────────────

/// Transforming a current-version message must be a no-op identity.
#[test]
fn transform_current_version_is_identity() {
    let msg = make_client_message(CURRENT_AST_VERSION);
    let transformed = transform_ast(&msg);

    assert_eq!(transformed.op_id, msg.op_id);
    assert_eq!(transformed.ast_version, msg.ast_version);
    assert_eq!(transformed.action, msg.action);
    assert_eq!(transformed.tenant_id, msg.tenant_id);
}

/// Transforming an unknown version must return the message unchanged (the
/// caller is responsible for having rejected incompatible versions first via
/// `check_version`).
#[test]
fn transform_unknown_version_returns_unchanged() {
    let msg = make_client_message("0.0.1");
    let transformed = transform_ast(&msg);
    assert_eq!(transformed.op_id, msg.op_id);
}

// ── RELOAD_REQUIRED wire format ───────────────────────────────────────────────

/// `ServerMessage::ReloadRequired` must serialise with `type = RELOAD_REQUIRED`
/// and include the `minRequiredVersion` and `reason` fields.
#[test]
fn reload_required_serialises_correctly() {
    let reload = gp2f_server::wire::ReloadRequired {
        min_required_version: CURRENT_AST_VERSION.to_owned(),
        reason: "test reason".into(),
    };
    let msg = ServerMessage::ReloadRequired(reload);
    let json = serde_json::to_value(&msg).unwrap();

    assert_eq!(json["type"], "RELOAD_REQUIRED");
    assert_eq!(json["minRequiredVersion"], CURRENT_AST_VERSION);
    assert_eq!(json["reason"], "test reason");
}

/// `ServerMessage::ReloadRequired` must round-trip through JSON.
#[test]
fn reload_required_deserialises_correctly() {
    let original = ServerMessage::ReloadRequired(gp2f_server::wire::ReloadRequired {
        min_required_version: "1.0.0".into(),
        reason: "version mismatch".into(),
    });

    let json_str = serde_json::to_string(&original).unwrap();
    let restored: ServerMessage = serde_json::from_str(&json_str).unwrap();

    if let ServerMessage::ReloadRequired(r) = restored {
        assert_eq!(r.min_required_version, "1.0.0");
        assert_eq!(r.reason, "version mismatch");
    } else {
        panic!("Expected ReloadRequired, got something else");
    }
}

// ── reconciler integration ────────────────────────────────────────────────────

/// The reconciler itself processes messages that have already passed version
/// negotiation.  An accepted-version message must return ACCEPT or REJECT,
/// never RELOAD_REQUIRED.
#[test]
fn reconciler_does_not_produce_reload_required_for_valid_version() {
    use gp2f_server::reconciler::Reconciler;
    use policy_core::evaluator::hash_state;
    use serde_json::json;

    let reconciler = Reconciler::new();
    let state = json!({});
    let hash = hash_state(&state);

    let msg = ClientMessage {
        op_id: "reconciler-test-op".into(),
        ast_version: CURRENT_AST_VERSION.into(),
        action: "update".into(),
        payload: json!({ "x": 1 }),
        client_snapshot_hash: hash,
        tenant_id: "t1".into(),
        workflow_id: "w1".into(),
        instance_id: "i1".into(),
        client_signature: None,
        role: "default".into(),
        vibe: None,
        trace_id: None,
    };

    let response = reconciler.reconcile(&msg);
    assert!(
        matches!(
            response,
            ServerMessage::Accept(_) | ServerMessage::Reject(_)
        ),
        "reconciler must not produce RELOAD_REQUIRED or HELLO for a valid op"
    );
}
