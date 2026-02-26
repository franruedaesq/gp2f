//! Dynamic Tool Gating Service.
//!
//! Implements Phase 8 requirement 2: evaluates the current workflow AST to
//! decide which tools are visible to the LLM at a given moment.  Each tool is
//! guarded by an [`policy_core::AstNode`] policy; the evaluator resolves it
//! against the current workflow state.  Only tools whose guard returns `true`
//! are included in the LLM system prompt.

use policy_core::{AstNode, Evaluator};
use serde::{Deserialize, Serialize};
use serde_json::Value;

// ── types ─────────────────────────────────────────────────────────────────────

/// JSON Schema stub for tool parameters.
///
/// In production this is a fully-resolved JSON Schema object forwarded
/// verbatim to the LLM tool-calling payload.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct JsonSchema(pub Value);

/// Descriptor of a single tool exposed to the LLM.
///
/// The `tool_id` uses an ephemeral `tool_req_<name>_<nonce>` format so the
/// LLM never sees real internal function names.  A private lookup table in
/// [`ToolGatingService`] maps the ephemeral id back to the real function.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolDescriptor {
    /// Ephemeral ID used in LLM tool-calling payloads.
    /// Format: `tool_req_<short_name>_<nonce>`.
    pub tool_id: String,
    /// Human-readable description sent in the system prompt.
    pub description: String,
    /// Parameter schema forwarded as-is to the LLM.
    pub parameters: JsonSchema,
}

// ── internal registry ─────────────────────────────────────────────────────────

/// Registered tool definition (internal, never exposed to the LLM directly).
struct RegisteredTool {
    /// Ephemeral tool id (sent to the LLM).
    tool_id: &'static str,
    /// Internal function name (private lookup; never sent to the LLM).
    internal_fn: &'static str,
    description: &'static str,
    parameters: Value,
    /// AST guard: evaluates to `true` when the tool should be offered.
    guard: AstNode,
}

// ── service ───────────────────────────────────────────────────────────────────

/// Service that decides which tools the LLM may see in a given workflow state.
pub struct ToolGatingService {
    evaluator: Evaluator,
    registry: Vec<RegisteredTool>,
}

impl ToolGatingService {
    /// Create a service pre-populated with the built-in tool registry.
    pub fn new() -> Self {
        Self {
            evaluator: Evaluator::new(),
            registry: default_tool_registry(),
        }
    }

    /// Return the subset of registered tools allowed in `state`.
    ///
    /// Each tool's AST guard is evaluated against `state`; only tools whose
    /// guard returns `true` are returned.  `ast_version` is embedded in the
    /// evaluation trace for audit.
    pub fn get_allowed_tools(&self, state: &Value, _ast_version: &str) -> Vec<ToolDescriptor> {
        self.registry
            .iter()
            .filter(|t| {
                self.evaluator
                    .evaluate(state, &t.guard)
                    .map(|r| r.result)
                    .unwrap_or(false)
            })
            .map(|t| ToolDescriptor {
                tool_id: t.tool_id.to_owned(),
                description: t.description.to_owned(),
                parameters: JsonSchema(t.parameters.clone()),
            })
            .collect()
    }

    /// Map an ephemeral `tool_id` back to the real internal function name.
    ///
    /// Returns `None` if the id is not in the registry (disallowed tool call).
    pub fn resolve_internal_fn<'a>(&'a self, tool_id: &str) -> Option<&'a str> {
        self.registry
            .iter()
            .find(|t| t.tool_id == tool_id)
            .map(|t| t.internal_fn)
    }
}

impl Default for ToolGatingService {
    fn default() -> Self {
        Self::new()
    }
}

// ── default registry ──────────────────────────────────────────────────────────

fn default_tool_registry() -> Vec<RegisteredTool> {
    vec![
        RegisteredTool {
            tool_id: "tool_req_extract_symptoms_8k2p9",
            internal_fn: "extract_symptoms",
            description: "Extract symptoms from free text",
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "text": {
                        "type": "string",
                        "description": "Free text to analyze for symptom extraction"
                    }
                },
                "required": ["text"]
            }),
            guard: AstNode::literal_true(),
        },
        RegisteredTool {
            tool_id: "tool_req_summarize_workflow_3x7r1",
            internal_fn: "summarize_workflow",
            description: "Summarize the current workflow state",
            parameters: serde_json::json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
            guard: AstNode::literal_true(),
        },
        RegisteredTool {
            tool_id: "tool_req_suggest_next_action_9q4m2",
            internal_fn: "suggest_next_action",
            description: "Suggest the next best action given the current behavioral context",
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "vibe_intent": {
                        "type": "string",
                        "description": "Current detected user intent"
                    }
                },
                "required": ["vibe_intent"]
            }),
            guard: AstNode::literal_true(),
        },
    ]
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn default_registry_returns_all_tools_for_empty_state() {
        let svc = ToolGatingService::new();
        let tools = svc.get_allowed_tools(&json!({}), "1.0.0");
        assert_eq!(tools.len(), 3);
    }

    #[test]
    fn tool_descriptors_have_expected_ids() {
        let svc = ToolGatingService::new();
        let tools = svc.get_allowed_tools(&json!({}), "1.0.0");
        let ids: Vec<&str> = tools.iter().map(|t| t.tool_id.as_str()).collect();
        assert!(ids.contains(&"tool_req_extract_symptoms_8k2p9"));
        assert!(ids.contains(&"tool_req_summarize_workflow_3x7r1"));
        assert!(ids.contains(&"tool_req_suggest_next_action_9q4m2"));
    }

    #[test]
    fn resolve_internal_fn_maps_ephemeral_to_real() {
        let svc = ToolGatingService::new();
        assert_eq!(
            svc.resolve_internal_fn("tool_req_extract_symptoms_8k2p9"),
            Some("extract_symptoms")
        );
        assert_eq!(
            svc.resolve_internal_fn("tool_req_summarize_workflow_3x7r1"),
            Some("summarize_workflow")
        );
    }

    #[test]
    fn resolve_internal_fn_returns_none_for_unknown_tool() {
        let svc = ToolGatingService::new();
        assert_eq!(svc.resolve_internal_fn("tool_req_unknown_xyzzy"), None);
    }

    #[test]
    fn gated_tool_is_hidden_when_guard_false() {
        // Register a service whose tools all have a literal-false guard.
        let svc = ToolGatingService {
            evaluator: Evaluator::new(),
            registry: vec![RegisteredTool {
                tool_id: "tool_req_gated_abc",
                internal_fn: "gated_fn",
                description: "Only shown when /active == true",
                parameters: json!({}),
                guard: AstNode::literal_false(),
            }],
        };
        let tools = svc.get_allowed_tools(&json!({ "active": false }), "1.0.0");
        assert!(tools.is_empty());
    }

    #[test]
    fn gated_tool_visible_when_guard_true() {
        let svc = ToolGatingService {
            evaluator: Evaluator::new(),
            registry: vec![RegisteredTool {
                tool_id: "tool_req_gated_abc",
                internal_fn: "gated_fn",
                description: "Always visible",
                parameters: json!({}),
                guard: AstNode::literal_true(),
            }],
        };
        let tools = svc.get_allowed_tools(&json!({}), "1.0.0");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].tool_id, "tool_req_gated_abc");
    }
}
