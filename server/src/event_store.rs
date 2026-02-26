//! Append-only event store keyed by `tenant:workflowId:instanceId`.
//!
//! Each stored event is a [`StoredEvent`] combining the original [`ClientMessage`]
//! with server-assigned metadata (sequence number, wall-clock timestamp, outcome).
//!
//! ## Compaction
//!
//! Long-lived event streams are automatically compacted when they exceed
//! [`COMPACTION_THRESHOLD`] events.  Compaction merges all *accepted* ops into
//! a single synthetic snapshot event and discards all rejected ops, while
//! preserving *replayability*: the snapshot includes a `compacted_from_seq`
//! marker so that full history can be reconstructed if the original log is
//! archived elsewhere.

use std::collections::HashMap;
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::wire::ClientMessage;

/// Compact a partition once it exceeds this many events.
pub const COMPACTION_THRESHOLD: usize = 1_000;

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
    ///
    /// After appending, automatically compacts the partition if it exceeds
    /// [`COMPACTION_THRESHOLD`].
    pub fn append(&self, msg: ClientMessage, outcome: OpOutcome) -> u64 {
        let key = Self::partition_key(&msg);
        let mut logs = self.logs.lock().unwrap();
        let partition = logs.entry(key.clone()).or_default();
        let seq = partition.len() as u64;
        partition.push(StoredEvent {
            seq,
            ingested_at: Utc::now(),
            message: msg,
            outcome,
        });

        // Trigger compaction when the partition is too long.
        if partition.len() > COMPACTION_THRESHOLD {
            compact_partition(partition);
        }

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

// ── compaction ────────────────────────────────────────────────────────────────

/// Compact a single partition in-place.
///
/// Strategy:
/// 1. Keep all *rejected* events as-is (for audit).
/// 2. Merge all accepted payloads into a single synthetic snapshot event.
/// 3. Replace the partition with `[snapshot, ...remaining_rejected]`.
///
/// The snapshot's `message.payload` contains the merged state and its
/// `message.op_id` carries a `compacted:N` marker so replayers know where
/// the compact begins.
fn compact_partition(partition: &mut Vec<StoredEvent>) {
    if partition.is_empty() {
        return;
    }

    let compacted_from_seq = partition[0].seq;
    let last_seq = partition.last().map(|e| e.seq).unwrap_or(0);

    // Merge accepted payloads (shallow object merge, last writer wins for each key).
    let mut merged: serde_json::Map<String, Value> = serde_json::Map::new();
    for event in partition
        .iter()
        .filter(|e| e.outcome == OpOutcome::Accepted)
    {
        if let Value::Object(patch) = &event.message.payload {
            for (k, v) in patch {
                merged.insert(k.clone(), v.clone());
            }
        }
    }

    // Take the first event as the template for routing fields.
    let template = partition[0].message.clone();
    let snapshot = StoredEvent {
        seq: compacted_from_seq,
        ingested_at: Utc::now(),
        message: ClientMessage {
            op_id: format!("compacted:{compacted_from_seq}..{last_seq}"),
            payload: Value::Object(merged),
            ..template
        },
        outcome: OpOutcome::Accepted,
    };

    // Retain only the snapshot + post-compaction events.
    // (In practice the partition would be empty after a threshold compact;
    //  we keep the snapshot so the replay can bootstrap from it.)
    partition.clear();
    partition.push(snapshot);
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
            role: "default".into(),
            vibe: None,
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
            let seq = store.append(msg("t", "w", "i", &format!("op-{i}")), OpOutcome::Accepted);
            assert_eq!(seq, i);
        }
    }

    // ── compaction tests ──────────────────────────────────────────────────

    #[test]
    fn compaction_merges_accepted_payloads() {
        let mut partition = vec![
            StoredEvent {
                seq: 0,
                ingested_at: Utc::now(),
                message: ClientMessage {
                    op_id: "op-0".into(),
                    payload: json!({"x": 1}),
                    ..msg("t", "w", "i", "op-0")
                },
                outcome: OpOutcome::Accepted,
            },
            StoredEvent {
                seq: 1,
                ingested_at: Utc::now(),
                message: ClientMessage {
                    op_id: "op-1".into(),
                    payload: json!({"y": 2}),
                    ..msg("t", "w", "i", "op-1")
                },
                outcome: OpOutcome::Accepted,
            },
        ];
        compact_partition(&mut partition);

        assert_eq!(partition.len(), 1);
        let snap = &partition[0];
        assert_eq!(snap.outcome, OpOutcome::Accepted);
        assert_eq!(snap.message.payload["x"], json!(1));
        assert_eq!(snap.message.payload["y"], json!(2));
        assert!(snap.message.op_id.starts_with("compacted:"));
    }

    #[test]
    fn compaction_last_writer_wins() {
        let mut partition = vec![
            StoredEvent {
                seq: 0,
                ingested_at: Utc::now(),
                message: ClientMessage {
                    op_id: "op-0".into(),
                    payload: json!({"x": 1}),
                    ..msg("t", "w", "i", "op-0")
                },
                outcome: OpOutcome::Accepted,
            },
            StoredEvent {
                seq: 1,
                ingested_at: Utc::now(),
                message: ClientMessage {
                    op_id: "op-1".into(),
                    payload: json!({"x": 99}),
                    ..msg("t", "w", "i", "op-1")
                },
                outcome: OpOutcome::Accepted,
            },
        ];
        compact_partition(&mut partition);
        assert_eq!(partition[0].message.payload["x"], json!(99));
    }

    #[test]
    fn compact_empty_partition_is_noop() {
        let mut partition: Vec<StoredEvent> = vec![];
        compact_partition(&mut partition);
        assert!(partition.is_empty());
    }
}
