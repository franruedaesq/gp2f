use serde::{Deserialize, Serialize};

/// Compact behavioral signal produced by the on-device Semantic Vibe Engine.
///
/// The classifier runs entirely locally and never sends raw behavioral data.
/// Only this ultra-compact vector is attached to every op.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct VibeVector {
    /// Detected user intent (e.g. `"frustrated"`, `"focused"`, `"confused"`).
    pub intent: String,
    /// Classifier confidence in [0.0, 1.0].
    pub confidence: f64,
    /// The UI element or flow step that is the current bottleneck.
    pub bottleneck: String,
}

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
    /// Caller's role within the tenant (used for RBAC checks).
    /// Defaults to `"default"` when absent.
    #[serde(default = "default_role")]
    pub role: String,
    /// Optional behavioral signal from the on-device Semantic Vibe Engine.
    #[serde(default)]
    pub vibe: Option<VibeVector>,
}

fn default_role() -> String {
    "default".to_owned()
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
    /// Suggested back-off interval in milliseconds (Retry-After semantics).
    /// Present when the rejection is caused by server-side backpressure; the
    /// client SHOULD pause sending new ops for at least this duration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_after_ms: Option<u32>,
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

/// Server hello – sent once per connection immediately after the WebSocket
/// handshake to provide the client with time-synchronisation data.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HelloMessage {
    /// Server wall-clock time in milliseconds since the Unix epoch.
    pub server_time_ms: u64,
    /// Server HLC timestamp at the moment of the hello.
    pub server_hlc_ts: u64,
}

/// Top-level server message (either accept or reject).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ServerMessage {
    Accept(AcceptResponse),
    Reject(RejectResponse),
    /// Sent once per connection immediately after the WebSocket handshake.
    Hello(HelloMessage),
}

/// Request body for `POST /agent/propose`.
///
/// The caller supplies the tenant/workflow/instance identifiers and an optional
/// user-facing prompt.  The server uses the current workflow state to decide
/// which tools the LLM may see, calls the configured LLM provider, and submits
/// the resulting op through the normal reconciliation pipeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentProposeRequest {
    /// Tenant that owns the workflow instance.
    #[serde(default)]
    pub tenant_id: String,
    /// Workflow definition identifier.
    #[serde(default)]
    pub workflow_id: String,
    /// Specific workflow instance identifier.
    #[serde(default)]
    pub instance_id: String,
    /// AST version string (used to gate tool visibility and audit the trace).
    #[serde(default = "default_ast_version")]
    pub ast_version: String,
    /// Optional natural-language prompt from the user or an upstream system.
    #[serde(default)]
    pub prompt: Option<String>,
    /// Optional behavioral signal from the on-device Semantic Vibe Engine.
    #[serde(default)]
    pub vibe: Option<VibeVector>,
}

fn default_ast_version() -> String {
    "1.0.0".to_owned()
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
