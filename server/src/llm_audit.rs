//! Audit-proof LLM call tracing.
//!
//! Implements Phase 10 requirement 3: every LLM call stores
//! `{call_id, model, tool_count, vibe_hash, op_id_outcome}` in an immutable
//! Temporal / S3 log.  The entry also contains a BLAKE3 hash of the entire
//! request/response body with all PII fields zeroed.
//!
//! ## Design
//!
//! * [`LlmAuditEntry`] is the canonical record written per call.
//! * [`LlmAuditStore`] is the in-process store used in dev/test.  Replace its
//!   `append` stub with an S3 `PutObject` + Temporal signal in production.
//! * The `body_hash` field is computed over a **sanitised** copy of the
//!   request and response: user-supplied `content` strings are replaced by
//!   their BLAKE3 hash, so no PII escapes into the audit log.

use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ── entry ─────────────────────────────────────────────────────────────────────

/// A single immutable audit record for one LLM call.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmAuditEntry {
    /// Globally unique call identifier (UUIDv4 generated at call time).
    pub call_id: String,
    /// LLM model name (e.g. `"gpt-4o"`, `"claude-3-5-sonnet-20241022"`).
    pub model: String,
    /// Number of tools presented to the LLM.
    pub tool_count: u32,
    /// BLAKE3 hex of the serialised [`crate::wire::VibeVector`], or `"none"`.
    pub vibe_hash: String,
    /// `"ACCEPTED"` or `"REJECTED"` – outcome after reconciliation.
    pub op_id_outcome: String,
    /// BLAKE3 hex of the sanitised (PII-free) request + response body.
    pub body_hash: String,
    /// Wall-clock timestamp.
    pub recorded_at: DateTime<Utc>,
    /// Tenant that triggered the call.
    pub tenant_id: String,
}

// ── builder ───────────────────────────────────────────────────────────────────

/// Builder for constructing an [`LlmAuditEntry`].
#[derive(Default)]
pub struct LlmAuditEntryBuilder {
    call_id: String,
    model: String,
    tool_count: u32,
    vibe_hash: String,
    op_id_outcome: String,
    body_hash: String,
    tenant_id: String,
}

impl LlmAuditEntryBuilder {
    pub fn new() -> Self {
        Self {
            call_id: new_call_id(),
            ..Self::default()
        }
    }

    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    pub fn tool_count(mut self, n: u32) -> Self {
        self.tool_count = n;
        self
    }

    /// Hash `vibe_json` with BLAKE3 and record it (the raw JSON is not stored).
    pub fn vibe_hash_from_json(mut self, vibe_json: &str) -> Self {
        self.vibe_hash = blake3_hex(vibe_json.as_bytes());
        self
    }

    /// Record that there was no vibe signal.
    pub fn no_vibe(mut self) -> Self {
        self.vibe_hash = "none".into();
        self
    }

    pub fn op_id_outcome(mut self, outcome: impl Into<String>) -> Self {
        self.op_id_outcome = outcome.into();
        self
    }

    /// Hash `request_body + response_body` with BLAKE3 (PII excluded by the
    /// caller before calling this method).
    pub fn body_hash_from_sanitised(mut self, sanitised_req: &str, sanitised_resp: &str) -> Self {
        let combined = format!("{sanitised_req}\n{sanitised_resp}");
        self.body_hash = blake3_hex(combined.as_bytes());
        self
    }

    pub fn tenant_id(mut self, tenant_id: impl Into<String>) -> Self {
        self.tenant_id = tenant_id.into();
        self
    }

    pub fn build(self) -> LlmAuditEntry {
        LlmAuditEntry {
            call_id: self.call_id,
            model: self.model,
            tool_count: self.tool_count,
            vibe_hash: self.vibe_hash,
            op_id_outcome: self.op_id_outcome,
            body_hash: self.body_hash,
            recorded_at: Utc::now(),
            tenant_id: self.tenant_id,
        }
    }
}

// ── store ─────────────────────────────────────────────────────────────────────

/// Append-only in-process audit log (dev/test).
///
/// In production replace [`LlmAuditStore::append`] with:
/// ```ignore
/// // S3
/// s3_client.put_object()
///     .bucket(&self.s3_bucket)
///     .key(format!("llm-audit/{}/{}.json", entry.tenant_id, entry.call_id))
///     .body(serde_json::to_vec(&entry)?.into())
///     .send().await?;
///
/// // Temporal signal
/// temporal_client.signal_workflow_execution(
///     "gp2f-audit", "", "LlmAuditRecord", Some(serde_json::to_value(&entry)?),
/// ).await?;
/// ```
pub struct LlmAuditStore {
    entries: Arc<Mutex<Vec<LlmAuditEntry>>>,
}

impl LlmAuditStore {
    pub fn new() -> Self {
        Self {
            entries: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Append an audit entry.  Returns the index of the entry.
    pub fn append(&self, entry: LlmAuditEntry) -> usize {
        let mut entries = self.entries.lock().unwrap();
        let idx = entries.len();
        tracing::info!(
            call_id = %entry.call_id,
            model   = %entry.model,
            tenant  = %entry.tenant_id,
            outcome = %entry.op_id_outcome,
            body_hash = %entry.body_hash,
            "LLM audit record written"
        );
        entries.push(entry);
        idx
    }

    /// Return all entries (dev/test only).
    pub fn all_entries(&self) -> Vec<LlmAuditEntry> {
        self.entries.lock().unwrap().clone()
    }

    /// Return the total number of recorded calls.
    pub fn len(&self) -> usize {
        self.entries.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for LlmAuditStore {
    fn default() -> Self {
        Self::new()
    }
}

// ── helpers ───────────────────────────────────────────────────────────────────

/// Generate a short pseudo-random call ID (UUID-like, no external dep).
fn new_call_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    // Mix in a static counter for uniqueness within the same nanosecond.
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("llm-{nanos:08x}-{seq:04x}")
}

/// BLAKE3 hex of arbitrary bytes.
fn blake3_hex(data: &[u8]) -> String {
    blake3::hash(data).to_hex().to_string()
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_entry(outcome: &str) -> LlmAuditEntry {
        LlmAuditEntryBuilder::new()
            .model("gpt-4o")
            .tool_count(3)
            .vibe_hash_from_json(r#"{"intent":"focused","confidence":0.8,"bottleneck":"step"}"#)
            .op_id_outcome(outcome)
            .body_hash_from_sanitised(
                r#"{"model":"gpt-4o","messages":["[HASH]"]}"#,
                r#"{"choices":[]}"#,
            )
            .tenant_id("tenant1")
            .build()
    }

    #[test]
    fn entry_fields_are_populated() {
        let e = make_entry("ACCEPTED");
        assert_eq!(e.model, "gpt-4o");
        assert_eq!(e.tool_count, 3);
        assert_eq!(e.op_id_outcome, "ACCEPTED");
        assert_eq!(e.tenant_id, "tenant1");
        assert!(!e.call_id.is_empty());
        assert!(!e.vibe_hash.is_empty());
        assert!(!e.body_hash.is_empty());
    }

    #[test]
    fn no_vibe_hash_is_none_literal() {
        let e = LlmAuditEntryBuilder::new()
            .model("claude-3-5-sonnet-20241022")
            .tool_count(0)
            .no_vibe()
            .op_id_outcome("REJECTED")
            .body_hash_from_sanitised("{}", "{}")
            .tenant_id("t2")
            .build();
        assert_eq!(e.vibe_hash, "none");
    }

    #[test]
    fn body_hash_is_deterministic() {
        let h1 = {
            LlmAuditEntryBuilder::new()
                .body_hash_from_sanitised("req", "resp")
                .no_vibe()
                .op_id_outcome("ACCEPTED")
                .build()
                .body_hash
        };
        let h2 = {
            LlmAuditEntryBuilder::new()
                .body_hash_from_sanitised("req", "resp")
                .no_vibe()
                .op_id_outcome("ACCEPTED")
                .build()
                .body_hash
        };
        assert_eq!(h1, h2);
    }

    #[test]
    fn body_hash_differs_for_different_bodies() {
        let h1 = {
            LlmAuditEntryBuilder::new()
                .body_hash_from_sanitised("req_a", "resp")
                .no_vibe()
                .op_id_outcome("ACCEPTED")
                .build()
                .body_hash
        };
        let h2 = {
            LlmAuditEntryBuilder::new()
                .body_hash_from_sanitised("req_b", "resp")
                .no_vibe()
                .op_id_outcome("ACCEPTED")
                .build()
                .body_hash
        };
        assert_ne!(h1, h2);
    }

    #[test]
    fn store_appends_and_retrieves_entries() {
        let store = LlmAuditStore::new();
        store.append(make_entry("ACCEPTED"));
        store.append(make_entry("REJECTED"));
        assert_eq!(store.len(), 2);
        let entries = store.all_entries();
        assert_eq!(entries[0].op_id_outcome, "ACCEPTED");
        assert_eq!(entries[1].op_id_outcome, "REJECTED");
    }

    #[test]
    fn store_is_empty_initially() {
        let store = LlmAuditStore::new();
        assert!(store.is_empty());
    }

    #[test]
    fn call_ids_are_unique() {
        let store = LlmAuditStore::new();
        for i in 0..10 {
            store.append(make_entry(if i % 2 == 0 { "ACCEPTED" } else { "REJECTED" }));
        }
        let ids: std::collections::HashSet<_> = store
            .all_entries()
            .iter()
            .map(|e| e.call_id.clone())
            .collect();
        assert_eq!(ids.len(), 10);
    }

    #[test]
    fn entry_is_serialisable_to_json() {
        let e = make_entry("ACCEPTED");
        let json = serde_json::to_string(&e).unwrap();
        let back: LlmAuditEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(back.call_id, e.call_id);
        assert_eq!(back.body_hash, e.body_hash);
    }
}
