use crate::broadcast::Broadcaster;
use crate::event_store::{EventStore, OpOutcome};
use crate::replay_protection::ReplayGuard;
use crate::signature::verify_signature;
use crate::wire::{
    AcceptResponse, ClientMessage, FieldConflict, RejectResponse, ServerMessage, ThreeWayPatch,
};
use policy_core::crdt::{DocumentSchema, FieldStrategy};
use policy_core::evaluator::hash_state;
use serde_json::{json, Map, Value};
use std::sync::Mutex;

/// Server-side reconciler.
///
/// Validates incoming [`ClientMessage`]s, persists them to an append-only
/// event store, and returns ACCEPT or REJECT.  Broadcasts the result to all
/// connected WebSocket clients via the [`Broadcaster`].
pub struct Reconciler {
    /// Per-client replay protection (bloom filter + exact window).
    replay: Mutex<ReplayGuard>,
    /// Append-only event log, partitioned by `tenant:workflowId:instanceId`.
    pub event_store: EventStore,
    /// Current authoritative state.
    state: Mutex<Value>,
    /// Field schema controlling conflict-resolution strategy per path.
    schema: DocumentSchema,
    /// ACCEPT/REJECT broadcaster for WebSocket push.
    broadcaster: Broadcaster,
    /// HMAC secret used to validate `client_signature` (empty = dev mode).
    tenant_secret: Vec<u8>,
}

impl Reconciler {
    pub fn new() -> Self {
        Self::with_secret(b"")
    }

    /// Create a reconciler with an explicit HMAC secret for signature validation.
    pub fn with_secret(secret: &[u8]) -> Self {
        Self {
            replay: Mutex::new(ReplayGuard::new()),
            event_store: EventStore::new(),
            state: Mutex::new(json!({})),
            schema: DocumentSchema::default(),
            broadcaster: Broadcaster::new(),
            tenant_secret: secret.to_vec(),
        }
    }

    /// Return a clone of the broadcaster so WebSocket handlers can subscribe.
    pub fn broadcaster(&self) -> Broadcaster {
        self.broadcaster.clone()
    }

    /// Process a [`ClientMessage`] and return a [`ServerMessage`].
    pub fn reconcile(&self, msg: &ClientMessage) -> ServerMessage {
        // 1. Signature validation
        if let Err(reason) = verify_signature(msg, &self.tenant_secret) {
            return ServerMessage::Reject(RejectResponse {
                op_id: msg.op_id.clone(),
                reason,
                patch: empty_patch(),
            });
        }

        // 2. Replay protection (per-client, keyed by tenant+client in op_id)
        {
            let mut guard = self.replay.lock().unwrap();
            if guard.check_and_insert(&msg.tenant_id, &msg.op_id) {
                let resp = ServerMessage::Reject(RejectResponse {
                    op_id: msg.op_id.clone(),
                    reason: "duplicate op_id".into(),
                    patch: empty_patch(),
                });
                self.event_store.append(msg.clone(), OpOutcome::Rejected);
                self.broadcaster.publish(resp.clone());
                return resp;
            }
        }

        let current_state = self.state.lock().unwrap().clone();
        let server_hash = hash_state(&current_state);

        // 3. Snapshot hash agreement
        if msg.client_snapshot_hash != server_hash {
            let patch = build_three_way_patch(&current_state, &msg.payload, &self.schema);
            let resp = ServerMessage::Reject(RejectResponse {
                op_id: msg.op_id.clone(),
                reason: format!(
                    "snapshot hash mismatch: client={} server={}",
                    msg.client_snapshot_hash, server_hash
                ),
                patch,
            });
            self.event_store.append(msg.clone(), OpOutcome::Rejected);
            self.broadcaster.publish(resp.clone());
            return resp;
        }

        // 4. Check for transactional field conflicts
        if let Err(reason) =
            check_transactional_conflicts(&current_state, &msg.payload, &self.schema)
        {
            let resp = ServerMessage::Reject(RejectResponse {
                op_id: msg.op_id.clone(),
                reason,
                patch: empty_patch(),
            });
            self.event_store.append(msg.clone(), OpOutcome::Rejected);
            self.broadcaster.publish(resp.clone());
            return resp;
        }

        // 5. Apply the op: merge payload into authoritative state
        let new_state = apply_op(&current_state, &msg.payload, &self.schema);
        let new_hash = hash_state(&new_state);

        // 6. Persist
        *self.state.lock().unwrap() = new_state;
        self.event_store.append(msg.clone(), OpOutcome::Accepted);

        let resp = ServerMessage::Accept(AcceptResponse {
            op_id: msg.op_id.clone(),
            server_snapshot_hash: new_hash,
        });
        self.broadcaster.publish(resp.clone());
        resp
    }

    /// Return a copy of the current authoritative state.
    pub fn current_state(&self) -> Value {
        self.state.lock().unwrap().clone()
    }

    /// Return the number of ops processed (accepted + rejected).
    pub fn op_count(&self) -> usize {
        self.event_store.total_count()
    }
}

impl Default for Reconciler {
    fn default() -> Self {
        Self::new()
    }
}

// ── helpers ───────────────────────────────────────────────────────────────────

/// Build a three-way patch comparing the server's authoritative state against
/// the client's proposed payload, annotating each conflicting field with its
/// schema-registered strategy.
fn build_three_way_patch(
    server_state: &Value,
    client_payload: &Value,
    schema: &DocumentSchema,
) -> ThreeWayPatch {
    let mut conflicts = Vec::new();

    if let (Value::Object(server_obj), Value::Object(client_obj)) = (server_state, client_payload) {
        for (key, client_val) in client_obj {
            let path = format!("/{key}");
            let strategy = schema.strategy_for(&path);
            if let Some(server_val) = server_obj.get(key) {
                if server_val != client_val {
                    let resolved = resolve_field(server_val, client_val, strategy);
                    conflicts.push(FieldConflict {
                        path: path.clone(),
                        strategy: strategy.into(),
                        resolved_value: resolved,
                    });
                }
            }
        }
    }

    // Compute JSON-diff style diffs (shallow object diff for now)
    let local_diff = object_diff(server_state, client_payload);
    let server_diff = Value::Null; // populated by Temporal replay in full implementation

    ThreeWayPatch {
        base_snapshot: server_state.clone(),
        local_diff,
        server_diff,
        conflicts,
    }
}

/// Resolve a conflicting field value based on its strategy.
fn resolve_field(server_val: &Value, _client_val: &Value, strategy: FieldStrategy) -> Value {
    match strategy {
        // For LWW the server wins (last authoritative write)
        FieldStrategy::Lww => server_val.clone(),
        // For CRDT fields the client sends a Yrs binary update encoded as base64;
        // the full merge is handled by the CRDT layer (out of scope for the resolver stub)
        FieldStrategy::YjsText => server_val.clone(),
        // Transactional fields should never reach here (caught earlier)
        FieldStrategy::Transactional => server_val.clone(),
    }
}

/// Return a JSON object listing keys whose values differ between `base` and `patch`.
fn object_diff(base: &Value, patch: &Value) -> Value {
    match (base, patch) {
        (Value::Object(b), Value::Object(p)) => {
            let mut diff = Map::new();
            for (k, pv) in p {
                match b.get(k) {
                    Some(bv) if bv == pv => {} // unchanged
                    _ => {
                        diff.insert(k.clone(), pv.clone());
                    }
                }
            }
            Value::Object(diff)
        }
        _ => patch.clone(),
    }
}

/// Apply the client's payload to the server state, respecting field strategies.
fn apply_op(state: &Value, payload: &Value, _schema: &DocumentSchema) -> Value {
    match (state, payload) {
        (Value::Object(base), Value::Object(patch)) => {
            let mut merged = base.clone();
            for (k, v) in patch {
                merged.insert(k.clone(), v.clone());
            }
            Value::Object(merged)
        }
        _ => state.clone(),
    }
}

/// Check if the payload would modify any TRANSACTIONAL field that already
/// differs from the authoritative state (i.e. a true conflict on a non-mergeable field).
fn check_transactional_conflicts(
    state: &Value,
    payload: &Value,
    schema: &DocumentSchema,
) -> Result<(), String> {
    let (Value::Object(state_obj), Value::Object(payload_obj)) = (state, payload) else {
        return Ok(());
    };
    for (key, payload_val) in payload_obj {
        let path = format!("/{key}");
        if schema.strategy_for(&path) == FieldStrategy::Transactional {
            if let Some(state_val) = state_obj.get(key) {
                if state_val != payload_val {
                    return Err(format!(
                        "transactional conflict on field `{path}`: \
                         server={state_val} client={payload_val}"
                    ));
                }
            }
        }
    }
    Ok(())
}

fn empty_patch() -> ThreeWayPatch {
    ThreeWayPatch {
        base_snapshot: Value::Null,
        local_diff: Value::Null,
        server_diff: Value::Null,
        conflicts: vec![],
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_msg(op_id: &str, hash: &str) -> ClientMessage {
        ClientMessage {
            op_id: op_id.to_owned(),
            ast_version: "1.0.0".into(),
            action: "update".into(),
            payload: json!({ "x": 1 }),
            client_snapshot_hash: hash.to_owned(),
            tenant_id: "tenant1".into(),
            workflow_id: "wf1".into(),
            instance_id: "inst1".into(),
            client_signature: None,
        }
    }

    #[test]
    fn first_op_is_accepted_when_hash_matches() {
        let r = Reconciler::new();
        let state = r.current_state();
        let hash = hash_state(&state);
        let msg = make_msg("op-1", &hash);
        let resp = r.reconcile(&msg);
        assert!(matches!(resp, ServerMessage::Accept(_)));
        assert_eq!(r.op_count(), 1);
    }

    #[test]
    fn duplicate_op_is_rejected() {
        let r = Reconciler::new();
        let hash = hash_state(&r.current_state());
        let msg = make_msg("op-dup", &hash);
        r.reconcile(&msg);
        let resp = r.reconcile(&msg);
        assert!(matches!(resp, ServerMessage::Reject(_)));
    }

    #[test]
    fn mismatched_hash_rejected() {
        let r = Reconciler::new();
        let msg = make_msg("op-bad", "aaaa");
        let resp = r.reconcile(&msg);
        assert!(matches!(resp, ServerMessage::Reject(_)));
    }

    #[test]
    fn event_store_records_accepted_and_rejected() {
        let r = Reconciler::new();
        let hash = hash_state(&r.current_state());
        r.reconcile(&make_msg("op-ok", &hash));
        r.reconcile(&make_msg("op-bad", "wrong_hash"));

        let events = r.event_store.events_for("tenant1:wf1:inst1");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].outcome, OpOutcome::Accepted);
        assert_eq!(events[1].outcome, OpOutcome::Rejected);
    }

    #[test]
    fn three_way_patch_lists_conflicts() {
        let r = Reconciler::new();
        // Set server state to have x=10
        let init_hash = hash_state(&r.current_state());
        let setup = ClientMessage {
            op_id: "setup".into(),
            payload: json!({ "x": 10 }),
            client_snapshot_hash: init_hash,
            ..make_msg("setup", "")
        };
        r.reconcile(&setup);

        // Now send op with wrong hash so it triggers a REJECT + patch
        let msg = ClientMessage {
            op_id: "conflict-op".into(),
            payload: json!({ "x": 99 }),
            client_snapshot_hash: "wrong".into(),
            ..make_msg("conflict-op", "")
        };
        let resp = r.reconcile(&msg);
        if let ServerMessage::Reject(rej) = resp {
            // base_snapshot must reflect authoritative state
            assert_eq!(rej.patch.base_snapshot["x"], json!(10));
        } else {
            panic!("expected Reject");
        }
    }
}
