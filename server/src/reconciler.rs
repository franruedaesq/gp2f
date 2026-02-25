use crate::wire::{AcceptResponse, ClientMessage, RejectResponse, ServerMessage, ThreeWayPatch};
use policy_core::evaluator::hash_state;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Mutex;

/// Server-side reconciler.
///
/// Validates incoming [`ClientMessage`]s, persists them to an in-memory
/// append-only log, and returns ACCEPT or REJECT.
pub struct Reconciler {
    /// Append-only event log: op_id → payload.
    log: Mutex<Vec<ClientMessage>>,
    /// Per-op_id replay protection.
    seen_ops: Mutex<HashMap<String, ()>>,
    /// Current authoritative state.
    state: Mutex<Value>,
}

impl Reconciler {
    pub fn new() -> Self {
        Self {
            log: Mutex::new(Vec::new()),
            seen_ops: Mutex::new(HashMap::new()),
            state: Mutex::new(json!({})),
        }
    }

    /// Process a [`ClientMessage`] and return a [`ServerMessage`].
    pub fn reconcile(&self, msg: &ClientMessage) -> ServerMessage {
        // Replay protection
        {
            let mut seen = self.seen_ops.lock().unwrap();
            if seen.contains_key(&msg.op_id) {
                return ServerMessage::Reject(RejectResponse {
                    op_id: msg.op_id.clone(),
                    reason: "duplicate op_id".into(),
                    patch: empty_patch(),
                });
            }
            seen.insert(msg.op_id.clone(), ());
        }

        let current_state = self.state.lock().unwrap().clone();
        let server_hash = hash_state(&current_state);

        // Check snapshot hash agreement
        if msg.client_snapshot_hash != server_hash {
            return ServerMessage::Reject(RejectResponse {
                op_id: msg.op_id.clone(),
                reason: format!(
                    "snapshot hash mismatch: client={} server={}",
                    msg.client_snapshot_hash, server_hash
                ),
                patch: ThreeWayPatch {
                    base_snapshot: current_state.clone(),
                    local_diff: Value::Null,
                    server_diff: Value::Null,
                    conflicts: vec![],
                },
            });
        }

        // Apply the op (stub: merge payload into state)
        let new_state = apply_op(&current_state, &msg.payload);
        let new_hash = hash_state(&new_state);

        // Persist
        {
            let mut state = self.state.lock().unwrap();
            *state = new_state;
        }
        {
            let mut log = self.log.lock().unwrap();
            log.push(msg.clone());
        }

        ServerMessage::Accept(AcceptResponse {
            op_id: msg.op_id.clone(),
            server_snapshot_hash: new_hash,
        })
    }

    /// Return a copy of the current authoritative state.
    pub fn current_state(&self) -> Value {
        self.state.lock().unwrap().clone()
    }

    /// Return the number of ops processed.
    pub fn op_count(&self) -> usize {
        self.log.lock().unwrap().len()
    }
}

impl Default for Reconciler {
    fn default() -> Self {
        Self::new()
    }
}

/// Naïve op application: merge payload object into state.
fn apply_op(state: &Value, payload: &Value) -> Value {
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

fn empty_patch() -> ThreeWayPatch {
    ThreeWayPatch {
        base_snapshot: Value::Null,
        local_diff: Value::Null,
        server_diff: Value::Null,
        conflicts: vec![],
    }
}

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
}
