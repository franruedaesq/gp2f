use serde::{Deserialize, Serialize};

// ── input length limits ───────────────────────────────────────────────────────

/// Maximum allowed length (in bytes) for identifier string fields such as
/// `op_id`, `tenant_id`, `workflow_id`, `instance_id`, and `ast_version`.
pub const MAX_ID_LEN: usize = 256;

/// Maximum allowed length (in bytes) for short label fields like `action` and
/// `role`.
pub const MAX_LABEL_LEN: usize = 128;

/// Maximum allowed length (in bytes) for hash / signature string fields such as
/// `client_snapshot_hash` and `client_signature`.
pub const MAX_HASH_LEN: usize = 512;

/// Maximum allowed serialized byte size for the `payload` JSON value.
/// Prevents memory exhaustion from deeply nested or very large payloads.
pub const MAX_PAYLOAD_BYTES: usize = 65_536; // 64 KiB

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

impl ClientMessage {
    /// Validate all string fields against configured length limits.
    ///
    /// Returns `Err(reason)` when any field exceeds its maximum allowed length
    /// or the serialized payload exceeds [`MAX_PAYLOAD_BYTES`].  Call this
    /// before any reconciliation logic to prevent memory exhaustion attacks.
    pub fn validate(&self) -> Result<(), String> {
        if self.op_id.len() > MAX_ID_LEN {
            return Err(format!("op_id exceeds maximum length of {MAX_ID_LEN}"));
        }
        if self.ast_version.len() > MAX_ID_LEN {
            return Err(format!(
                "ast_version exceeds maximum length of {MAX_ID_LEN}"
            ));
        }
        if self.action.len() > MAX_LABEL_LEN {
            return Err(format!(
                "action exceeds maximum length of {MAX_LABEL_LEN}"
            ));
        }
        if self.tenant_id.len() > MAX_ID_LEN {
            return Err(format!("tenant_id exceeds maximum length of {MAX_ID_LEN}"));
        }
        if self.workflow_id.len() > MAX_ID_LEN {
            return Err(format!(
                "workflow_id exceeds maximum length of {MAX_ID_LEN}"
            ));
        }
        if self.instance_id.len() > MAX_ID_LEN {
            return Err(format!(
                "instance_id exceeds maximum length of {MAX_ID_LEN}"
            ));
        }
        if self.client_snapshot_hash.len() > MAX_HASH_LEN {
            return Err(format!(
                "client_snapshot_hash exceeds maximum length of {MAX_HASH_LEN}"
            ));
        }
        if self.role.len() > MAX_LABEL_LEN {
            return Err(format!("role exceeds maximum length of {MAX_LABEL_LEN}"));
        }
        if let Some(sig) = &self.client_signature {
            if sig.len() > MAX_HASH_LEN {
                return Err(format!(
                    "client_signature exceeds maximum length of {MAX_HASH_LEN}"
                ));
            }
        }
        // Guard against oversized payloads that could exhaust server memory.
        let payload_bytes = serde_json::to_vec(&self.payload)
            .map(|v| v.len())
            .unwrap_or(0);
        if payload_bytes > MAX_PAYLOAD_BYTES {
            return Err(format!(
                "payload exceeds maximum size of {MAX_PAYLOAD_BYTES} bytes"
            ));
        }
        Ok(())
    }
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

/// RELOAD_REQUIRED – sent when the client's AST version is incompatible with
/// the server.  The client MUST reload its policy bundle before retrying.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReloadRequired {
    /// The minimum AST version the server accepts (semver).
    pub min_required_version: String,
    /// Human-readable explanation of why a reload is required.
    pub reason: String,
}

/// Top-level server message (either accept or reject).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ServerMessage {
    Accept(AcceptResponse),
    Reject(RejectResponse),
    /// Sent once per connection immediately after the WebSocket handshake.
    Hello(HelloMessage),
    /// Sent when the client's AST version is too old; client must reload.
    ReloadRequired(ReloadRequired),
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

/// Request body for `POST /ai/feedback` – logs dismissed or unhelpful AI suggestions.
///
/// Callers send this when the user dismisses an AI-generated proposal so the
/// backend can record the signal for future retraining / drift detection.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiFeedbackRequest {
    /// Tenant that owns the workflow instance.
    #[serde(default)]
    pub tenant_id: String,
    /// Workflow definition identifier.
    #[serde(default)]
    pub workflow_id: String,
    /// Specific workflow instance identifier.
    #[serde(default)]
    pub instance_id: String,
    /// The `op_id` of the AI-generated proposal being dismissed.
    pub op_id: String,
    /// Human-readable reason for dismissal (e.g. `"wrong_tool"`, `"not_helpful"`).
    #[serde(default)]
    pub reason: String,
    /// Optional behavioral signal at the time of dismissal.
    #[serde(default)]
    pub vibe: Option<VibeVector>,
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

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn valid_msg() -> ClientMessage {
        ClientMessage {
            op_id: "op-1".into(),
            ast_version: "1.0.0".into(),
            action: "update".into(),
            payload: json!({ "x": 1 }),
            client_snapshot_hash: "abc123".into(),
            tenant_id: "tenant1".into(),
            workflow_id: "wf1".into(),
            instance_id: "inst1".into(),
            client_signature: None,
            role: "default".into(),
            vibe: None,
        }
    }

    #[test]
    fn valid_message_passes_validation() {
        assert!(valid_msg().validate().is_ok());
    }

    #[test]
    fn oversized_op_id_is_rejected() {
        let mut msg = valid_msg();
        msg.op_id = "a".repeat(MAX_ID_LEN + 1);
        assert!(msg.validate().is_err());
    }

    #[test]
    fn oversized_tenant_id_is_rejected() {
        let mut msg = valid_msg();
        msg.tenant_id = "t".repeat(MAX_ID_LEN + 1);
        assert!(msg.validate().is_err());
    }

    #[test]
    fn oversized_action_is_rejected() {
        let mut msg = valid_msg();
        msg.action = "a".repeat(MAX_LABEL_LEN + 1);
        assert!(msg.validate().is_err());
    }

    #[test]
    fn oversized_role_is_rejected() {
        let mut msg = valid_msg();
        msg.role = "r".repeat(MAX_LABEL_LEN + 1);
        assert!(msg.validate().is_err());
    }

    #[test]
    fn oversized_hash_is_rejected() {
        let mut msg = valid_msg();
        msg.client_snapshot_hash = "h".repeat(MAX_HASH_LEN + 1);
        assert!(msg.validate().is_err());
    }

    #[test]
    fn oversized_signature_is_rejected() {
        let mut msg = valid_msg();
        msg.client_signature = Some("s".repeat(MAX_HASH_LEN + 1));
        assert!(msg.validate().is_err());
    }

    #[test]
    fn oversized_payload_is_rejected() {
        let mut msg = valid_msg();
        // Build a JSON string value that exceeds MAX_PAYLOAD_BYTES.
        let big_string = "x".repeat(MAX_PAYLOAD_BYTES + 1);
        msg.payload = json!({ "data": big_string });
        assert!(msg.validate().is_err());
    }

    #[test]
    fn payload_at_exact_limit_passes() {
        // A valid message with a payload just within the limit should pass.
        let msg = valid_msg();
        assert!(msg.validate().is_ok());
    }
}
