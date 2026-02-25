use serde::{Deserialize, Serialize};

/// Message the client sends to the server.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientMessage {
    pub op_id: String,
    pub ast_version: String,
    pub action: String,
    #[serde(default)]
    pub payload: serde_json::Value,
    pub client_snapshot_hash: String,
    /// Tenant identifier (part of the `tenant:workflowId:instanceId` event-store key).
    #[serde(default)]
    pub tenant_id: String,
    /// Workflow definition identifier.
    #[serde(default)]
    pub workflow_id: String,
    /// Specific workflow instance identifier.
    #[serde(default)]
    pub instance_id: String,
    /// HMAC-SHA256 over the canonical op fields, base64url-encoded.
    /// When present the server validates it; absent ⇒ unauthenticated (dev only).
    #[serde(default)]
    pub client_signature: Option<String>,
}

/// ACCEPT response – the operation was applied successfully.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcceptResponse {
    pub op_id: String,
    pub server_snapshot_hash: String,
}

/// REJECT response – the operation was rejected, includes a patch.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RejectResponse {
    pub op_id: String,
    pub reason: String,
    pub patch: ThreeWayPatch,
}

/// 3-way patch used for conflict resolution.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreeWayPatch {
    pub base_snapshot: serde_json::Value,
    pub local_diff: serde_json::Value,
    pub server_diff: serde_json::Value,
    pub conflicts: Vec<FieldConflict>,
}

/// A single field conflict with its chosen resolution strategy.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldConflict {
    pub path: String,
    pub strategy: FieldConflictStrategy,
    pub resolved_value: serde_json::Value,
}

/// Per-field conflict resolution strategy.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum FieldConflictStrategy {
    Crdt,
    Lww,
    Transactional,
}

/// Top-level server message (either accept or reject).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ServerMessage {
    Accept(AcceptResponse),
    Reject(RejectResponse),
}

// ── conversions ───────────────────────────────────────────────────────────────

impl From<policy_core::FieldStrategy> for FieldConflictStrategy {
    fn from(s: policy_core::FieldStrategy) -> Self {
        match s {
            policy_core::FieldStrategy::YjsText => FieldConflictStrategy::Crdt,
            policy_core::FieldStrategy::Lww => FieldConflictStrategy::Lww,
            policy_core::FieldStrategy::Transactional => FieldConflictStrategy::Transactional,
        }
    }
}
