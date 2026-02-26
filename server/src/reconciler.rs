use crate::broadcast::Broadcaster;
use crate::event_store::{EventStore, OpOutcome};
use crate::limits::{BackpressureSignal, LimitsGuard};
use crate::rbac::RbacRegistry;
use crate::replay_protection::ReplayGuard;
use crate::signature::verify_signature;
use crate::wire::{
    AcceptResponse, ClientMessage, FieldConflict, RejectResponse, ServerMessage, ThreeWayPatch,
};
use base64::Engine as _;
use policy_core::crdt::{CrdtDoc, DocumentSchema, FieldStrategy};
use policy_core::evaluator::hash_state;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

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
    /// Snapshot history: maps `state_hash` → `state_value`.
    ///
    /// Allows the 3-way merge to reconstruct the **base** state (last common
    /// ancestor) that the client was working from when it produced a payload.
    snapshot_history: Mutex<HashMap<String, Value>>,
    /// Per-field CRDT documents: maps `"instance_key:field"` → [`CrdtDoc`].
    ///
    /// Maintained for every `YjsText` field so the server can auto-merge
    /// concurrent client updates rather than doing a naïve last-write-wins.
    crdt_docs: Mutex<HashMap<String, CrdtDoc>>,
    /// Field schema controlling conflict-resolution strategy per path.
    schema: DocumentSchema,
    /// ACCEPT/REJECT broadcaster for WebSocket push.
    broadcaster: Broadcaster,
    /// HMAC secret used to validate `client_signature` (empty = dev mode).
    tenant_secret: Vec<u8>,
    /// Per-tenant operational limits and backpressure enforcement.
    pub limits: Arc<LimitsGuard>,
    /// RBAC registry for role-based access control.
    pub rbac: Arc<RbacRegistry>,
}

impl Reconciler {
    pub fn new() -> Self {
        Self::with_secret(b"")
    }

    /// Create a reconciler with an explicit HMAC secret for signature validation.
    pub fn with_secret(secret: &[u8]) -> Self {
        let initial_state = json!({});
        let initial_hash = hash_state(&initial_state);
        let mut snapshot_history = HashMap::new();
        snapshot_history.insert(initial_hash, initial_state.clone());
        Self {
            replay: Mutex::new(ReplayGuard::new()),
            event_store: EventStore::new(),
            state: Mutex::new(initial_state),
            snapshot_history: Mutex::new(snapshot_history),
            crdt_docs: Mutex::new(HashMap::new()),
            schema: DocumentSchema::default(),
            broadcaster: Broadcaster::new(),
            tenant_secret: secret.to_vec(),
            limits: Arc::new(LimitsGuard::new()),
            rbac: Arc::new(RbacRegistry::with_defaults()),
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
                retry_after_ms: None,
            });
        }

        // 2. Backpressure / per-tenant queue limit
        if let Err(signal) = self.limits.try_enqueue_op(&msg.tenant_id) {
            let retry_after_ms = match &signal {
                BackpressureSignal::QueueFull { .. } => Some(1_000u32),
                _ => None,
            };
            let resp = ServerMessage::Reject(RejectResponse {
                op_id: msg.op_id.clone(),
                reason: signal.to_string(),
                patch: empty_patch(),
                retry_after_ms,
            });
            self.broadcaster.publish(resp.clone());
            return resp;
        }

        // 3. Replay protection (per-client, keyed by tenant+client in op_id)
        {
            let mut guard = self.replay.lock().unwrap();
            if guard.check_and_insert(&msg.tenant_id, &msg.op_id) {
                let resp = ServerMessage::Reject(RejectResponse {
                    op_id: msg.op_id.clone(),
                    reason: "duplicate op_id".into(),
                    patch: empty_patch(),
                    retry_after_ms: None,
                });
                self.event_store.append(msg.clone(), OpOutcome::Rejected);
                self.broadcaster.publish(resp.clone());
                self.limits.dequeue_op(&msg.tenant_id);
                return resp;
            }
        }

        let current_state = self.state.lock().unwrap().clone();
        let server_hash = hash_state(&current_state);

        // 4. Snapshot hash agreement
        if msg.client_snapshot_hash != server_hash {
            // Look up the base state the client was working from.
            let base_state = self
                .snapshot_history
                .lock()
                .unwrap()
                .get(&msg.client_snapshot_hash)
                .cloned()
                .unwrap_or_else(|| current_state.clone());

            let patch = three_way_merge(&base_state, &current_state, &msg.payload, &self.schema);
            let resp = ServerMessage::Reject(RejectResponse {
                op_id: msg.op_id.clone(),
                reason: format!(
                    "snapshot hash mismatch: client={} server={}",
                    msg.client_snapshot_hash, server_hash
                ),
                patch,
                retry_after_ms: None,
            });
            self.event_store.append(msg.clone(), OpOutcome::Rejected);
            self.broadcaster.publish(resp.clone());
            self.limits.dequeue_op(&msg.tenant_id);
            return resp;
        }

        // 5. Check for transactional field conflicts
        if let Err(reason) =
            check_transactional_conflicts(&current_state, &msg.payload, &self.schema)
        {
            let resp = ServerMessage::Reject(RejectResponse {
                op_id: msg.op_id.clone(),
                reason,
                patch: empty_patch(),
                retry_after_ms: None,
            });
            self.event_store.append(msg.clone(), OpOutcome::Rejected);
            self.broadcaster.publish(resp.clone());
            self.limits.dequeue_op(&msg.tenant_id);
            return resp;
        }

        // 6. Apply the op: merge payload into authoritative state (CRDT-aware)
        let instance_key = EventStore::partition_key(msg);
        let new_state = {
            let mut crdt_docs = self.crdt_docs.lock().unwrap();
            apply_op_with_crdt(&current_state, &msg.payload, &self.schema, &mut crdt_docs, &instance_key)
        };
        let new_hash = hash_state(&new_state);

        // 7. Persist and update snapshot history
        *self.state.lock().unwrap() = new_state.clone();
        self.snapshot_history
            .lock()
            .unwrap()
            .insert(new_hash.clone(), new_state);
        self.event_store.append(msg.clone(), OpOutcome::Accepted);
        self.limits.dequeue_op(&msg.tenant_id);

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

/// Perform a **true 3-way merge** of `base` (last common ancestor), `server`
/// (current authoritative state), and `client` (proposed payload).
///
/// This is a **pure function** with no side-effects, making it straightforward
/// to unit-test with arbitrary input triples.
///
/// ## Three states
/// | Symbol | Meaning |
/// |--------|---------|
/// | `base` | The snapshot the client was working from (keyed by `client_snapshot_hash`). |
/// | `server` | The current authoritative state (may have advanced past `base`). |
/// | `client` | The payload the client wants to apply. |
///
/// ## Conflict detection
///
/// A conflict arises only when **both** the server and the client changed the
/// same field relative to `base`.  Fields changed only by one side are
/// non-conflicting and can be auto-applied.
///
/// ## Output
/// * `base_snapshot` – the base state (so the UI can show "what you started from").
/// * `local_diff` – fields changed by the client relative to `base`.
/// * `server_diff` – fields changed by the server relative to `base`.
/// * `conflicts` – fields changed by *both* sides (non-CRDT) with resolved values.
pub fn three_way_merge(
    base: &Value,
    server: &Value,
    client: &Value,
    schema: &DocumentSchema,
) -> ThreeWayPatch {
    let mut conflicts = Vec::new();

    if let (
        Value::Object(base_obj),
        Value::Object(server_obj),
        Value::Object(client_obj),
    ) = (base, server, client)
    {
        for (key, client_val) in client_obj {
            let path = format!("/{key}");
            let strategy = schema.strategy_for(&path);
            let base_val = base_obj.get(key);
            let server_val = server_obj.get(key);

            // Client changed this field relative to base?
            let client_changed = base_val != Some(client_val);
            // Server changed this field relative to base?
            let server_changed = base_val != server_val;

            // A true conflict: both sides diverged from base on the same field.
            if client_changed && server_changed {
                let resolved = resolve_field(
                    server_val.unwrap_or(&Value::Null),
                    client_val,
                    strategy,
                );
                conflicts.push(FieldConflict {
                    path,
                    strategy: strategy.into(),
                    resolved_value: resolved,
                });
            }
        }
    }

    // Compute diffs relative to the base state.
    let local_diff = object_diff(base, client);
    let server_diff = object_diff(base, server);

    ThreeWayPatch {
        base_snapshot: base.clone(),
        local_diff,
        server_diff,
        conflicts,
    }
}

/// Resolve a conflicting field value based on its strategy.
///
/// For `YjsText` fields the server value is already the CRDT-merged result
/// (computed by [`apply_op_with_crdt`] during the last accepted op).  For
/// `Lww` fields the server value wins (authoritative last-write).
fn resolve_field(server_val: &Value, _client_val: &Value, strategy: FieldStrategy) -> Value {
    match strategy {
        // For LWW the server wins (last authoritative write)
        FieldStrategy::Lww => server_val.clone(),
        // For CRDT fields the server value already reflects the auto-merge
        // performed by the reconciler's CrdtDoc layer.
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
///
/// For [`FieldStrategy::YjsText`] fields the payload value is expected to be
/// a base64url-encoded [yrs v1 update](https://docs.rs/yrs).  The update is
/// applied to the per-instance [`CrdtDoc`] and the merged text string is stored
/// in the authoritative state.  Plain string values are treated as direct
/// overrides (for backwards-compatibility with non-CRDT clients).
fn apply_op_with_crdt(
    state: &Value,
    payload: &Value,
    schema: &DocumentSchema,
    crdt_docs: &mut HashMap<String, CrdtDoc>,
    instance_key: &str,
) -> Value {
    match (state, payload) {
        (Value::Object(base), Value::Object(patch)) => {
            let mut merged = base.clone();
            for (k, v) in patch {
                let path = format!("/{k}");
                if schema.strategy_for(&path) == FieldStrategy::YjsText {
                    let doc_key = format!("{instance_key}:{k}");
                    let doc = crdt_docs
                        .entry(doc_key)
                        .or_insert_with(|| CrdtDoc::new(k.as_str()));
                    // Client sends a base64-encoded yrs binary update.
                    let merged_val = if let Some(b64) = v.as_str() {
                        match base64::engine::general_purpose::STANDARD.decode(b64) {
                            Ok(bytes) => match doc.apply_update(&bytes) {
                                Ok(()) => Value::String(doc.get_string()),
                                Err(_) => v.clone(),
                            },
                            // Not a valid base64 string – treat as a plain-text override.
                            Err(_) => v.clone(),
                        }
                    } else {
                        v.clone()
                    };
                    merged.insert(k.clone(), merged_val);
                } else {
                    merged.insert(k.clone(), v.clone());
                }
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
    use policy_core::crdt::{FieldSchema, FieldStrategy};
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
            role: "default".into(),
            vibe: None,
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
            // base_snapshot: "wrong" hash not in history, falls back to current state
            assert_eq!(rej.patch.base_snapshot["x"], json!(10));
        } else {
            panic!("expected Reject");
        }
    }

    /// Test the pure `three_way_merge` function independently of the reconciler.
    #[test]
    fn three_way_merge_computes_server_diff_and_local_diff() {
        let schema = DocumentSchema::default();
        let base = json!({ "a": 1, "b": 2 });
        // Server advanced field `a` to 5 since the base.
        let server = json!({ "a": 5, "b": 2 });
        // Client changed field `b` to 9 based on the base.
        let client = json!({ "a": 1, "b": 9 });

        let patch = three_way_merge(&base, &server, &client, &schema);

        // local_diff should show the client's change to `b`.
        assert_eq!(patch.local_diff["b"], json!(9));
        assert!(patch.local_diff.get("a").is_none());

        // server_diff should show the server's change to `a`.
        assert_eq!(patch.server_diff["a"], json!(5));
        assert!(patch.server_diff.get("b").is_none());

        // There should be no conflict on `b` (only server changed `a`, only
        // client changed `b`).
        assert!(
            patch.conflicts.iter().all(|c| c.path != "/b"),
            "b should not be a conflict"
        );
    }

    /// Test that when both sides change the same field it appears in conflicts.
    #[test]
    fn three_way_merge_detects_field_conflict() {
        let schema = DocumentSchema::default();
        let base = json!({ "x": 1 });
        let server = json!({ "x": 10 });
        let client = json!({ "x": 99 });

        let patch = three_way_merge(&base, &server, &client, &schema);

        // /x is changed by both sides → should be in conflicts.
        assert_eq!(patch.conflicts.len(), 1);
        assert_eq!(patch.conflicts[0].path, "/x");
        // LWW → server wins.
        assert_eq!(patch.conflicts[0].resolved_value, json!(10));
    }

    /// Test that known base state is used when the client hash is in history.
    #[test]
    fn reconciler_uses_stored_base_for_three_way_merge() {
        let r = Reconciler::new();

        // Step 1: accept an op that changes `a` from 0 to 1.
        let h0 = hash_state(&r.current_state());
        let op1 = ClientMessage {
            op_id: "op1".into(),
            payload: json!({ "a": 1 }),
            client_snapshot_hash: h0.clone(),
            ..make_msg("op1", "")
        };
        r.reconcile(&op1);

        // The hash after op1.
        let h1 = hash_state(&r.current_state());

        // Step 2: server advances `b` while client is still at h1.
        let op2 = ClientMessage {
            op_id: "op2".into(),
            payload: json!({ "b": 99 }),
            client_snapshot_hash: h1.clone(),
            ..make_msg("op2", "")
        };
        r.reconcile(&op2);

        // Step 3: client (still at h1) tries to change `a`.
        // Hash mismatch → reject with 3-way patch where base=state@h1.
        let op3 = ClientMessage {
            op_id: "op3".into(),
            payload: json!({ "a": 42 }),
            client_snapshot_hash: h1.clone(), // client was at h1, not h2
            ..make_msg("op3", "")
        };
        let resp = r.reconcile(&op3);
        if let ServerMessage::Reject(rej) = resp {
            // base_snapshot should be the state at h1 = { a: 1 }
            assert_eq!(rej.patch.base_snapshot["a"], json!(1));
            // server_diff should show the b=99 change the server made.
            assert_eq!(rej.patch.server_diff["b"], json!(99));
        } else {
            panic!("expected Reject");
        }
    }

    /// Backpressure rejection must carry a Retry-After hint.
    #[test]
    fn backpressure_rejection_has_retry_after_ms() {
        let r = Reconciler::new();
        // Configure a very small queue limit for tenant1.
        r.limits.set_limits(
            "tenant1",
            crate::limits::TenantLimits {
                max_queued_ops: 0,
                max_ws_connections: 100,
            },
        );
        let hash = hash_state(&r.current_state());
        let msg = make_msg("op-bp", &hash);
        let resp = r.reconcile(&msg);
        if let ServerMessage::Reject(rej) = resp {
            assert!(
                rej.retry_after_ms.is_some(),
                "backpressure rejection must set retry_after_ms"
            );
        } else {
            panic!("expected Reject");
        }
    }

    /// Test that the snapshot history is stored and keyed by hash.
    #[test]
    fn snapshot_history_is_populated_after_accepted_op() {
        let r = Reconciler::new();
        let h0 = hash_state(&r.current_state());
        let msg = ClientMessage {
            op_id: "snap-op".into(),
            payload: json!({ "z": 7 }),
            client_snapshot_hash: h0,
            ..make_msg("snap-op", "")
        };
        r.reconcile(&msg);
        let h1 = hash_state(&r.current_state());
        // The new hash should be stored in snapshot history.
        let history = r.snapshot_history.lock().unwrap();
        assert!(history.contains_key(&h1), "h1 must be in snapshot_history");
        assert_eq!(history[&h1]["z"], json!(7));
    }

    /// Test YjsText CRDT merge path via base64-encoded yrs updates.
    #[test]
    fn crdt_yjs_text_field_is_merged() {
        use policy_core::crdt::CrdtDoc;
        let schema = DocumentSchema {
            fields: vec![FieldSchema {
                path: "/notes".into(),
                strategy: FieldStrategy::YjsText,
            }],
        };

        // Build a yrs update from a local doc that inserts "hello".
        let local_doc = CrdtDoc::new("notes");
        local_doc.insert(0, "hello");
        let update_bytes = local_doc.encode_state();
        let b64 = base64::engine::general_purpose::STANDARD.encode(&update_bytes);

        let mut crdt_docs = HashMap::new();
        let result = apply_op_with_crdt(
            &json!({}),
            &json!({ "notes": b64 }),
            &schema,
            &mut crdt_docs,
            "tenant1:wf1:inst1",
        );

        assert_eq!(result["notes"], json!("hello"));
    }
}
