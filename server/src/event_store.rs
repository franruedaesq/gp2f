//! Append-only event store keyed by `tenant:workflowId:instanceId`.
//!
//! Each stored event is a [`StoredEvent`] combining the original [`ClientMessage`]
//! with server-assigned metadata (sequence number, wall-clock timestamp, outcome).

use std::collections::HashMap;
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::wire::ClientMessage;

/// The outcome of processing an op – stored alongside the original message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum OpOutcome {
    Accepted,
    Rejected,
}

/// A single entry in the append-only event log.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredEvent {
    /// Monotonically increasing sequence number within the instance log.
    pub seq: u64,
    /// Server wall-clock time of ingestion.
    pub ingested_at: DateTime<Utc>,
    /// The original client operation.
    pub message: ClientMessage,
    /// Whether the op was accepted or rejected.
    pub outcome: OpOutcome,
}

/// Append-only, in-memory event store.
///
/// Events are partitioned by `tenant:workflowId:instanceId` – the canonical
/// event-sourcing key described in the problem statement.
pub struct EventStore {
    /// Map from `"tenant:workflow:instance"` → ordered list of events.
    logs: Mutex<HashMap<String, Vec<StoredEvent>>>,
}

impl EventStore {
    pub fn new() -> Self {
        Self {
            logs: Mutex::new(HashMap::new()),
        }
    }

    /// Build the partition key from the message's routing fields.
    pub fn partition_key(msg: &ClientMessage) -> String {
        format!("{}:{}:{}", msg.tenant_id, msg.workflow_id, msg.instance_id)
    }

    /// Append an event to the log for the message's partition.
    ///
    /// Returns the assigned sequence number.
    pub fn append(&self, msg: ClientMessage, outcome: OpOutcome) -> u64 {
        let key = Self::partition_key(&msg);
        let mut logs = self.logs.lock().unwrap();
        let partition = logs.entry(key).or_default();
        let seq = partition.len() as u64;
        partition.push(StoredEvent {
            seq,
            ingested_at: Utc::now(),
            message: msg,
            outcome,
        });
        seq
    }

    /// Return all events for a given `tenant:workflowId:instanceId` key.
    pub fn events_for(&self, key: &str) -> Vec<StoredEvent> {
        self.logs
            .lock()
            .unwrap()
            .get(key)
            .cloned()
            .unwrap_or_default()
    }

    /// Total number of events across all partitions.
    pub fn total_count(&self) -> usize {
        self.logs.lock().unwrap().values().map(Vec::len).sum()
    }
}

impl Default for EventStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn msg(tenant: &str, workflow: &str, instance: &str, op: &str) -> ClientMessage {
        ClientMessage {
            op_id: op.into(),
            ast_version: "1.0.0".into(),
            action: "update".into(),
            payload: json!({}),
            client_snapshot_hash: "hash".into(),
            tenant_id: tenant.into(),
            workflow_id: workflow.into(),
            instance_id: instance.into(),
            client_signature: None,
        }
    }

    #[test]
    fn partition_key_format() {
        let m = msg("t1", "wf1", "i1", "op1");
        assert_eq!(EventStore::partition_key(&m), "t1:wf1:i1");
    }

    #[test]
    fn append_and_retrieve() {
        let store = EventStore::new();
        let m = msg("t1", "wf1", "i1", "op1");
        let seq = store.append(m.clone(), OpOutcome::Accepted);
        assert_eq!(seq, 0);

        let events = store.events_for("t1:wf1:i1");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].message.op_id, "op1");
        assert_eq!(events[0].outcome, OpOutcome::Accepted);
    }

    #[test]
    fn multiple_partitions_are_isolated() {
        let store = EventStore::new();
        store.append(msg("t1", "wf1", "i1", "op1"), OpOutcome::Accepted);
        store.append(msg("t1", "wf1", "i2", "op2"), OpOutcome::Rejected);

        assert_eq!(store.events_for("t1:wf1:i1").len(), 1);
        assert_eq!(store.events_for("t1:wf1:i2").len(), 1);
    }

    #[test]
    fn sequence_numbers_are_monotonic() {
        let store = EventStore::new();
        for i in 0..5u64 {
            let seq = store.append(
                msg("t", "w", "i", &format!("op-{i}")),
                OpOutcome::Accepted,
            );
            assert_eq!(seq, i);
        }
    }
}
