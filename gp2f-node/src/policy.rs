//! Policy AST types exposed to JavaScript.
//!
//! [`JsAstNode`] mirrors `policy_core::AstNode` as a plain JS object so that
//! TypeScript callers can construct policy trees without any Rust knowledge.

use napi::bindgen_prelude::*;
use napi_derive::napi;
use policy_core::{AstNode, NodeKind};
use serde_json::Value;

// ── NodeKind string enum ──────────────────────────────────────────────────────

/// Every node kind supported by the GP2F AST policy evaluator.
///
/// Mirrors `policy_core::NodeKind` as a JavaScript string enum.
#[napi(string_enum)]
#[derive(Debug)]
pub enum JsNodeKind {
    LiteralTrue,
    LiteralFalse,
    And,
    Or,
    Not,
    Eq,
    Neq,
    Gt,
    Gte,
    Lt,
    Lte,
    In,
    Contains,
    Exists,
    Field,
    Call,
    VibeCheck,
}

impl From<JsNodeKind> for NodeKind {
    fn from(k: JsNodeKind) -> Self {
        match k {
            JsNodeKind::LiteralTrue => NodeKind::LiteralTrue,
            JsNodeKind::LiteralFalse => NodeKind::LiteralFalse,
            JsNodeKind::And => NodeKind::And,
            JsNodeKind::Or => NodeKind::Or,
            JsNodeKind::Not => NodeKind::Not,
            JsNodeKind::Eq => NodeKind::Eq,
            JsNodeKind::Neq => NodeKind::Neq,
            JsNodeKind::Gt => NodeKind::Gt,
            JsNodeKind::Gte => NodeKind::Gte,
            JsNodeKind::Lt => NodeKind::Lt,
            JsNodeKind::Lte => NodeKind::Lte,
            JsNodeKind::In => NodeKind::In,
            JsNodeKind::Contains => NodeKind::Contains,
            JsNodeKind::Exists => NodeKind::Exists,
            JsNodeKind::Field => NodeKind::Field,
            JsNodeKind::Call => NodeKind::Call,
            JsNodeKind::VibeCheck => NodeKind::VibeCheck,
        }
    }
}

// ── AstNode object ────────────────────────────────────────────────────────────

/// A node in the GP2F policy AST.
///
/// Pass a tree of these to [`JsWorkflow::add_activity`] to define the policy
/// that governs whether a given activity is permitted.
///
/// Example (TypeScript):
/// ```typescript
/// const policy: AstNode = {
///   kind: 'AND',
///   children: [
///     { kind: 'FIELD', path: '/user/role', value: 'admin' },
///   ],
/// };
/// ```
#[napi(object)]
#[derive(Debug, Clone)]
pub struct JsAstNode {
    /// The operation this node performs (required).
    pub kind: String,
    /// Child nodes for composite operators (AND, OR, NOT, comparison, …).
    pub children: Option<Vec<JsAstNode>>,
    /// JSON-pointer path used by `FIELD` and `EXISTS` nodes (e.g. `/user/role`).
    pub path: Option<String>,
    /// String-encoded scalar value for leaf nodes (e.g. `"admin"`, `"42"`).
    pub value: Option<String>,
    /// Name of the external function – used only by `CALL` nodes.
    pub call_name: Option<String>,
}

impl TryFrom<JsAstNode> for AstNode {
    type Error = napi::Error;

    fn try_from(js: JsAstNode) -> Result<Self> {
        let kind = parse_node_kind(&js.kind)?;
        let children = js
            .children
            .unwrap_or_default()
            .into_iter()
            .map(AstNode::try_from)
            .collect::<Result<Vec<_>>>()?;
        Ok(AstNode {
            version: None,
            kind,
            children,
            path: js.path,
            value: js.value,
            call_name: js.call_name,
        })
    }
}

fn parse_node_kind(s: &str) -> Result<NodeKind> {
    match s {
        "LITERAL_TRUE" | "LiteralTrue" => Ok(NodeKind::LiteralTrue),
        "LITERAL_FALSE" | "LiteralFalse" => Ok(NodeKind::LiteralFalse),
        "AND" | "And" => Ok(NodeKind::And),
        "OR" | "Or" => Ok(NodeKind::Or),
        "NOT" | "Not" => Ok(NodeKind::Not),
        "EQ" | "Eq" => Ok(NodeKind::Eq),
        "NEQ" | "Neq" => Ok(NodeKind::Neq),
        "GT" | "Gt" => Ok(NodeKind::Gt),
        "GTE" | "Gte" => Ok(NodeKind::Gte),
        "LT" | "Lt" => Ok(NodeKind::Lt),
        "LTE" | "Lte" => Ok(NodeKind::Lte),
        "IN" | "In" => Ok(NodeKind::In),
        "CONTAINS" | "Contains" => Ok(NodeKind::Contains),
        "EXISTS" | "Exists" => Ok(NodeKind::Exists),
        "FIELD" | "Field" => Ok(NodeKind::Field),
        "CALL" | "Call" => Ok(NodeKind::Call),
        "VIBE_CHECK" | "VibeCheck" => Ok(NodeKind::VibeCheck),
        other => Err(napi::Error::new(
            napi::Status::InvalidArg,
            format!("unknown NodeKind: {other}"),
        )),
    }
}

// ── evaluate helper ───────────────────────────────────────────────────────────

/// Evaluate a policy AST against a JSON state object.
///
/// Returns `true` when the policy permits the operation, `false` when it
/// denies it.  The `state` argument must be a serialisable JavaScript object.
///
/// Example (TypeScript):
/// ```typescript
/// import { evaluate } from '@gp2f/server';
///
/// const allowed = evaluate(
///   { kind: 'FIELD', path: '/role', value: 'admin' },
///   { role: 'admin' }
/// );
/// // => true
/// ```
#[napi]
pub fn evaluate(policy: JsAstNode, state: Value) -> Result<bool> {
    let ast = AstNode::try_from(policy)?;
    let result = policy_core::Evaluator::new()
        .evaluate(&state, &ast)
        .map_err(|e| napi::Error::new(napi::Status::GenericFailure, e.to_string()))?;
    Ok(result.result)
}

/// Evaluate a policy AST and return the full evaluation trace.
///
/// Useful for debugging policies – the trace lists each decision step.
#[napi]
pub fn evaluate_with_trace(policy: JsAstNode, state: Value) -> Result<EvalResult> {
    let ast = AstNode::try_from(policy)?;
    let result = policy_core::Evaluator::new()
        .evaluate(&state, &ast)
        .map_err(|e| napi::Error::new(napi::Status::GenericFailure, e.to_string()))?;
    Ok(EvalResult {
        result: result.result,
        trace: result.trace,
    })
}

/// Result of a policy evaluation including the decision trace.
#[napi(object)]
pub struct EvalResult {
    /// `true` when the policy permits the operation.
    pub result: bool,
    /// Human-readable trace of each evaluation step (for debugging).
    pub trace: Vec<String>,
}
