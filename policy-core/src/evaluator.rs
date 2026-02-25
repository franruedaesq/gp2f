use serde_json::Value;
use thiserror::Error;

use crate::ast::{AstNode, NodeKind};

/// The result returned by [`Evaluator::evaluate`].
#[derive(Debug, Clone, PartialEq)]
pub struct EvalResult {
    /// Whether the policy evaluated to `true`.
    pub result: bool,
    /// Human-readable evaluation trace (one entry per node visited).
    pub trace: Vec<String>,
    /// BLAKE3 hex digest of the state document at evaluation time.
    pub snapshot_hash: String,
}

/// Errors that can occur during policy evaluation.
#[derive(Debug, Error)]
pub enum EvalError {
    #[error("unsupported CALL node: '{0}' (stubs not resolved)")]
    UnresolvedCall(String),

    #[error("operator {0:?} requires exactly {1} child(ren), got {2}")]
    ArityMismatch(NodeKind, usize, usize),

    #[error("operator {0:?} requires at least {1} child(ren), got {2}")]
    ArityAtLeast(NodeKind, usize, usize),

    #[error("FIELD node is missing a `path`")]
    MissingPath,

    #[error("value node is missing a `value`")]
    MissingValue,

    #[error("JSON pointer error: {0}")]
    Pointer(String),

    #[error("type mismatch: cannot compare {0} and {1}")]
    TypeMismatch(String, String),
}

/// Zero-I/O, deterministic AST policy evaluator.
///
/// The same evaluator logic runs on the client (compiled to WASM) and
/// on the server (native / Wasmtime) to guarantee 100 % parity.
#[derive(Default)]
pub struct Evaluator;

impl Evaluator {
    pub fn new() -> Self {
        Self
    }

    /// Evaluate `node` against `state` and return an [`EvalResult`].
    ///
    /// `state` must be a valid `serde_json::Value` (any JSON document).
    pub fn evaluate(&self, state: &Value, node: &AstNode) -> Result<EvalResult, EvalError> {
        let snapshot_hash = hash_state(state);
        let mut trace = Vec::new();
        let result = self.eval_node(state, node, &mut trace)?;
        Ok(EvalResult {
            result,
            trace,
            snapshot_hash,
        })
    }

    // ── recursive evaluation ──────────────────────────────────────────────

    fn eval_node(
        &self,
        state: &Value,
        node: &AstNode,
        trace: &mut Vec<String>,
    ) -> Result<bool, EvalError> {
        let result = match node.kind {
            NodeKind::LiteralTrue => {
                trace.push("LITERAL_TRUE => true".into());
                true
            }
            NodeKind::LiteralFalse => {
                trace.push("LITERAL_FALSE => false".into());
                false
            }

            NodeKind::And => {
                self.require_arity_at_least(&node.kind, 1, node.children.len())?;
                let mut acc = true;
                for child in &node.children {
                    let v = self.eval_node(state, child, trace)?;
                    acc = acc && v;
                    if !acc {
                        break; // short-circuit
                    }
                }
                trace.push(format!("AND => {acc}"));
                acc
            }

            NodeKind::Or => {
                self.require_arity_at_least(&node.kind, 1, node.children.len())?;
                let mut acc = false;
                for child in &node.children {
                    let v = self.eval_node(state, child, trace)?;
                    acc = acc || v;
                    if acc {
                        break; // short-circuit
                    }
                }
                trace.push(format!("OR => {acc}"));
                acc
            }

            NodeKind::Not => {
                self.require_arity(&node.kind, 1, node.children.len())?;
                let v = self.eval_node(state, &node.children[0], trace)?;
                let result = !v;
                trace.push(format!("NOT => {result}"));
                result
            }

            NodeKind::Eq => {
                self.require_arity(&node.kind, 2, node.children.len())?;
                let (l, r) = self.eval_two_values(state, node, trace)?;
                let result = values_equal(&l, &r);
                trace.push(format!("EQ({l}, {r}) => {result}"));
                result
            }

            NodeKind::Neq => {
                self.require_arity(&node.kind, 2, node.children.len())?;
                let (l, r) = self.eval_two_values(state, node, trace)?;
                let result = !values_equal(&l, &r);
                trace.push(format!("NEQ({l}, {r}) => {result}"));
                result
            }

            NodeKind::Gt => {
                self.require_arity(&node.kind, 2, node.children.len())?;
                let (l, r) = self.eval_two_values(state, node, trace)?;
                let result = compare_values(&l, &r)? > 0;
                trace.push(format!("GT({l}, {r}) => {result}"));
                result
            }

            NodeKind::Gte => {
                self.require_arity(&node.kind, 2, node.children.len())?;
                let (l, r) = self.eval_two_values(state, node, trace)?;
                let result = compare_values(&l, &r)? >= 0;
                trace.push(format!("GTE({l}, {r}) => {result}"));
                result
            }

            NodeKind::Lt => {
                self.require_arity(&node.kind, 2, node.children.len())?;
                let (l, r) = self.eval_two_values(state, node, trace)?;
                let result = compare_values(&l, &r)? < 0;
                trace.push(format!("LT({l}, {r}) => {result}"));
                result
            }

            NodeKind::Lte => {
                self.require_arity(&node.kind, 2, node.children.len())?;
                let (l, r) = self.eval_two_values(state, node, trace)?;
                let result = compare_values(&l, &r)? <= 0;
                trace.push(format!("LTE({l}, {r}) => {result}"));
                result
            }

            NodeKind::In => {
                // left IN right  →  right must be an array containing left
                self.require_arity(&node.kind, 2, node.children.len())?;
                let (needle, haystack) = self.eval_two_values(state, node, trace)?;
                let result = match &haystack {
                    Value::Array(arr) => arr.iter().any(|el| values_equal(el, &needle)),
                    Value::String(s) => needle.as_str().map(|n| s.contains(n)).unwrap_or(false),
                    _ => false,
                };
                trace.push(format!("IN({needle}, {haystack}) => {result}"));
                result
            }

            NodeKind::Contains => {
                // left CONTAINS right  →  left must be an array/string containing right
                self.require_arity(&node.kind, 2, node.children.len())?;
                let (haystack, needle) = self.eval_two_values(state, node, trace)?;
                let result = match &haystack {
                    Value::Array(arr) => arr.iter().any(|el| values_equal(el, &needle)),
                    Value::String(s) => needle.as_str().map(|n| s.contains(n)).unwrap_or(false),
                    _ => false,
                };
                trace.push(format!("CONTAINS({haystack}, {needle}) => {result}"));
                result
            }

            NodeKind::Exists => {
                let path = node.path.as_deref().ok_or(EvalError::MissingPath)?;
                let resolved = resolve_path(state, path);
                let result = !matches!(resolved, None | Some(Value::Null));
                trace.push(format!("EXISTS({path}) => {result}"));
                result
            }

            NodeKind::Field => {
                // FIELD nodes resolve to a value; as a boolean: null/false → false, else true
                let path = node.path.as_deref().ok_or(EvalError::MissingPath)?;
                let resolved = resolve_path(state, path).cloned().unwrap_or(Value::Null);
                let result = value_as_bool(&resolved);
                trace.push(format!("FIELD({path}) = {resolved} => {result}"));
                result
            }

            NodeKind::Call => {
                let name = node.call_name.as_deref().unwrap_or("<unnamed>").to_string();
                return Err(EvalError::UnresolvedCall(name));
            }
        };

        Ok(result)
    }

    /// Evaluate the two children of a binary operator and return their resolved
    /// JSON values.
    fn eval_two_values(
        &self,
        state: &Value,
        node: &AstNode,
        trace: &mut Vec<String>,
    ) -> Result<(Value, Value), EvalError> {
        let left = self.resolve_child_value(state, &node.children[0], trace)?;
        let right = self.resolve_child_value(state, &node.children[1], trace)?;
        Ok((left, right))
    }

    /// Resolve a child node to a raw `serde_json::Value`.
    ///
    /// `FIELD` children are looked up in the state; scalar `value` nodes are
    /// parsed as JSON; boolean-producing composite nodes return `true`/`false`.
    fn resolve_child_value(
        &self,
        state: &Value,
        node: &AstNode,
        trace: &mut Vec<String>,
    ) -> Result<Value, EvalError> {
        match node.kind {
            NodeKind::Field => {
                let path = node.path.as_deref().ok_or(EvalError::MissingPath)?;
                Ok(resolve_path(state, path).cloned().unwrap_or(Value::Null))
            }
            NodeKind::LiteralTrue => Ok(Value::Bool(true)),
            NodeKind::LiteralFalse => Ok(Value::Bool(false)),
            // Leaf nodes with an explicit `value` string
            _ if node.children.is_empty() && node.path.is_none() && node.value.is_some() => {
                parse_scalar(node.value.as_deref().unwrap())
            }
            // Composite node – evaluate it to bool
            _ => {
                let b = self.eval_node(state, node, trace)?;
                Ok(Value::Bool(b))
            }
        }
    }

    // ── helpers ───────────────────────────────────────────────────────────

    fn require_arity(&self, kind: &NodeKind, expected: usize, got: usize) -> Result<(), EvalError> {
        if got != expected {
            Err(EvalError::ArityMismatch(*kind, expected, got))
        } else {
            Ok(())
        }
    }

    fn require_arity_at_least(
        &self,
        kind: &NodeKind,
        min: usize,
        got: usize,
    ) -> Result<(), EvalError> {
        if got < min {
            Err(EvalError::ArityAtLeast(*kind, min, got))
        } else {
            Ok(())
        }
    }
}

// ── free functions ────────────────────────────────────────────────────────────

/// Compute a BLAKE3 hex digest of the JSON state.
pub fn hash_state(state: &Value) -> String {
    let serialized = serde_json::to_vec(state).unwrap_or_default();
    blake3::hash(&serialized).to_hex().to_string()
}

/// Resolve a JSON-pointer or dot-separated path in `state`.
///
/// Accepts both RFC 6901 JSON Pointer (`/user/role`) and dot-separated
/// paths (`user.role`).
fn resolve_path<'a>(state: &'a Value, path: &str) -> Option<&'a Value> {
    if path.starts_with('/') {
        // RFC 6901 JSON Pointer
        state.pointer(path)
    } else {
        // dot-separated fallback
        let mut current = state;
        for segment in path.split('.') {
            current = match current {
                Value::Object(map) => map.get(segment)?,
                Value::Array(arr) => {
                    let idx: usize = segment.parse().ok()?;
                    arr.get(idx)?
                }
                _ => return None,
            };
        }
        Some(current)
    }
}

/// Parse a string as a JSON scalar (number, bool, string, null, or array literal).
fn parse_scalar(s: &str) -> Result<Value, EvalError> {
    // Try as JSON first (handles numbers, booleans, null, arrays, objects)
    if let Ok(v) = serde_json::from_str::<Value>(s) {
        return Ok(v);
    }
    // Fall back to treating the whole string as a plain string value
    Ok(Value::String(s.to_owned()))
}

/// Structural equality between two `Value`s.
fn values_equal(a: &Value, b: &Value) -> bool {
    // Coerce numeric types for comparison
    match (a, b) {
        (Value::Number(na), Value::Number(nb)) => {
            na.as_f64().zip(nb.as_f64()).is_some_and(|(x, y)| x == y)
        }
        _ => a == b,
    }
}

/// Numeric ordering: returns -1, 0, or 1.
///
/// Only valid for numbers and strings.
fn compare_values(a: &Value, b: &Value) -> Result<i32, EvalError> {
    match (a, b) {
        (Value::Number(na), Value::Number(nb)) => {
            let x = na
                .as_f64()
                .ok_or_else(|| EvalError::TypeMismatch(a.to_string(), b.to_string()))?;
            let y = nb
                .as_f64()
                .ok_or_else(|| EvalError::TypeMismatch(a.to_string(), b.to_string()))?;
            Ok(x.partial_cmp(&y).map(|o| o as i32).unwrap_or(0))
        }
        (Value::String(sa), Value::String(sb)) => Ok(match sa.cmp(sb) {
            std::cmp::Ordering::Less => -1,
            std::cmp::Ordering::Equal => 0,
            std::cmp::Ordering::Greater => 1,
        }),
        _ => Err(EvalError::TypeMismatch(
            type_name(a).into(),
            type_name(b).into(),
        )),
    }
}

fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn value_as_bool(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|x| x != 0.0).unwrap_or(false),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{AstNode, NodeKind};
    use serde_json::json;

    fn field(path: &str) -> AstNode {
        AstNode {
            version: None,
            kind: NodeKind::Field,
            children: vec![],
            path: Some(path.to_owned()),
            value: None,
            call_name: None,
        }
    }

    fn scalar(v: &str) -> AstNode {
        AstNode {
            version: None,
            kind: NodeKind::Eq, // kind is irrelevant for leaf resolution
            children: vec![],
            path: None,
            value: Some(v.to_owned()),
            call_name: None,
        }
    }

    fn binary(kind: NodeKind, left: AstNode, right: AstNode) -> AstNode {
        AstNode {
            version: None,
            kind,
            children: vec![left, right],
            path: None,
            value: None,
            call_name: None,
        }
    }

    fn unary(kind: NodeKind, child: AstNode) -> AstNode {
        AstNode {
            version: None,
            kind,
            children: vec![child],
            path: None,
            value: None,
            call_name: None,
        }
    }

    fn nary(kind: NodeKind, children: Vec<AstNode>) -> AstNode {
        AstNode {
            version: None,
            kind,
            children,
            path: None,
            value: None,
            call_name: None,
        }
    }

    fn eval(state: Value, node: AstNode) -> EvalResult {
        Evaluator::new().evaluate(&state, &node).unwrap()
    }

    // ── literal nodes ─────────────────────────────────────────────────────

    #[test]
    fn literal_true() {
        assert!(eval(json!({}), AstNode::literal_true()).result);
    }

    #[test]
    fn literal_false() {
        assert!(!eval(json!({}), AstNode::literal_false()).result);
    }

    // ── field resolution ──────────────────────────────────────────────────

    #[test]
    fn field_json_pointer() {
        let state = json!({ "user": { "role": "admin" } });
        let node = field("/user/role");
        assert!(eval(state, node).result);
    }

    #[test]
    fn field_dot_path() {
        let state = json!({ "user": { "active": true } });
        assert!(eval(state, field("user.active")).result);
    }

    #[test]
    fn field_missing_is_false() {
        let state = json!({});
        assert!(!eval(state, field("/missing")).result);
    }

    // ── eq / neq ─────────────────────────────────────────────────────────

    #[test]
    fn eq_strings_true() {
        let state = json!({ "role": "admin" });
        let node = binary(NodeKind::Eq, field("/role"), scalar("admin"));
        assert!(eval(state, node).result);
    }

    #[test]
    fn eq_strings_false() {
        let state = json!({ "role": "user" });
        let node = binary(NodeKind::Eq, field("/role"), scalar("admin"));
        assert!(!eval(state, node).result);
    }

    #[test]
    fn neq_strings() {
        let state = json!({ "role": "user" });
        let node = binary(NodeKind::Neq, field("/role"), scalar("admin"));
        assert!(eval(state, node).result);
    }

    #[test]
    fn eq_numbers() {
        let state = json!({ "age": 30 });
        let node = binary(NodeKind::Eq, field("/age"), scalar("30"));
        assert!(eval(state, node).result);
    }

    // ── comparisons ───────────────────────────────────────────────────────

    #[test]
    fn gt_true() {
        let state = json!({ "score": 100 });
        let node = binary(NodeKind::Gt, field("/score"), scalar("50"));
        assert!(eval(state, node).result);
    }

    #[test]
    fn gt_false() {
        let state = json!({ "score": 30 });
        let node = binary(NodeKind::Gt, field("/score"), scalar("50"));
        assert!(!eval(state, node).result);
    }

    #[test]
    fn gte_equal() {
        let state = json!({ "score": 50 });
        let node = binary(NodeKind::Gte, field("/score"), scalar("50"));
        assert!(eval(state, node).result);
    }

    #[test]
    fn lt_true() {
        let state = json!({ "score": 10 });
        let node = binary(NodeKind::Lt, field("/score"), scalar("50"));
        assert!(eval(state, node).result);
    }

    #[test]
    fn lte_equal() {
        let state = json!({ "score": 50 });
        let node = binary(NodeKind::Lte, field("/score"), scalar("50"));
        assert!(eval(state, node).result);
    }

    // ── boolean logic ─────────────────────────────────────────────────────

    #[test]
    fn and_all_true() {
        let node = nary(
            NodeKind::And,
            vec![AstNode::literal_true(), AstNode::literal_true()],
        );
        assert!(eval(json!({}), node).result);
    }

    #[test]
    fn and_one_false() {
        let node = nary(
            NodeKind::And,
            vec![AstNode::literal_true(), AstNode::literal_false()],
        );
        assert!(!eval(json!({}), node).result);
    }

    #[test]
    fn or_one_true() {
        let node = nary(
            NodeKind::Or,
            vec![AstNode::literal_false(), AstNode::literal_true()],
        );
        assert!(eval(json!({}), node).result);
    }

    #[test]
    fn or_all_false() {
        let node = nary(
            NodeKind::Or,
            vec![AstNode::literal_false(), AstNode::literal_false()],
        );
        assert!(!eval(json!({}), node).result);
    }

    #[test]
    fn not_true_is_false() {
        let node = unary(NodeKind::Not, AstNode::literal_true());
        assert!(!eval(json!({}), node).result);
    }

    #[test]
    fn not_false_is_true() {
        let node = unary(NodeKind::Not, AstNode::literal_false());
        assert!(eval(json!({}), node).result);
    }

    // ── in / contains ─────────────────────────────────────────────────────

    #[test]
    fn in_array_true() {
        let state = json!({ "roles": ["admin", "editor"] });
        let node = binary(NodeKind::In, scalar("admin"), field("/roles"));
        assert!(eval(state, node).result);
    }

    #[test]
    fn in_array_false() {
        let state = json!({ "roles": ["editor"] });
        let node = binary(NodeKind::In, scalar("admin"), field("/roles"));
        assert!(!eval(state, node).result);
    }

    #[test]
    fn contains_array_true() {
        let state = json!({ "roles": ["admin", "editor"] });
        let node = binary(NodeKind::Contains, field("/roles"), scalar("admin"));
        assert!(eval(state, node).result);
    }

    #[test]
    fn contains_string() {
        let state = json!({ "name": "hello world" });
        let node = binary(NodeKind::Contains, field("/name"), scalar("world"));
        assert!(eval(state, node).result);
    }

    // ── exists ────────────────────────────────────────────────────────────

    #[test]
    fn exists_present() {
        let state = json!({ "user": { "id": 1 } });
        let node = AstNode {
            version: None,
            kind: NodeKind::Exists,
            children: vec![],
            path: Some("/user/id".into()),
            value: None,
            call_name: None,
        };
        assert!(eval(state, node).result);
    }

    #[test]
    fn exists_absent() {
        let state = json!({});
        let node = AstNode {
            version: None,
            kind: NodeKind::Exists,
            children: vec![],
            path: Some("/user/id".into()),
            value: None,
            call_name: None,
        };
        assert!(!eval(state, node).result);
    }

    #[test]
    fn exists_null_is_false() {
        let state = json!({ "x": null });
        let node = AstNode {
            version: None,
            kind: NodeKind::Exists,
            children: vec![],
            path: Some("/x".into()),
            value: None,
            call_name: None,
        };
        assert!(!eval(state, node).result);
    }

    // ── snapshot hash ─────────────────────────────────────────────────────

    #[test]
    fn snapshot_hash_deterministic() {
        let state = json!({ "a": 1 });
        let r1 = eval(state.clone(), AstNode::literal_true());
        let r2 = eval(state.clone(), AstNode::literal_true());
        assert_eq!(r1.snapshot_hash, r2.snapshot_hash);
        assert_eq!(r1.snapshot_hash.len(), 64); // hex blake3 is 64 chars
    }

    #[test]
    fn snapshot_hash_differs_for_different_states() {
        let r1 = eval(json!({ "a": 1 }), AstNode::literal_true());
        let r2 = eval(json!({ "a": 2 }), AstNode::literal_true());
        assert_ne!(r1.snapshot_hash, r2.snapshot_hash);
    }

    // ── trace ─────────────────────────────────────────────────────────────

    #[test]
    fn trace_is_populated() {
        let node = nary(
            NodeKind::And,
            vec![AstNode::literal_true(), AstNode::literal_true()],
        );
        let result = eval(json!({}), node);
        assert!(!result.trace.is_empty());
        assert!(result.trace.iter().any(|t| t.contains("AND")));
    }

    // ── error cases ───────────────────────────────────────────────────────

    #[test]
    fn call_node_returns_error() {
        let node = AstNode {
            version: None,
            kind: NodeKind::Call,
            children: vec![],
            path: None,
            value: None,
            call_name: Some("myFunc".into()),
        };
        let err = Evaluator::new().evaluate(&json!({}), &node).unwrap_err();
        assert!(matches!(err, EvalError::UnresolvedCall(_)));
    }

    #[test]
    fn arity_mismatch_not() {
        let node = unary(
            NodeKind::Not,
            nary(
                NodeKind::Not,
                vec![AstNode::literal_true(), AstNode::literal_true()],
            ),
        );
        // The outer NOT has 1 child (OK), the inner NOT has 2 children (error)
        let err = Evaluator::new().evaluate(&json!({}), &node).unwrap_err();
        assert!(matches!(err, EvalError::ArityMismatch(NodeKind::Not, 1, 2)));
    }

    // ── nested complex policy ─────────────────────────────────────────────

    #[test]
    fn complex_policy() {
        // (role == "admin" AND age >= 18) OR superuser == true
        let state = json!({ "role": "admin", "age": 20, "superuser": false });
        let role_eq = binary(NodeKind::Eq, field("/role"), scalar("admin"));
        let age_gte = binary(NodeKind::Gte, field("/age"), scalar("18"));
        let superuser = field("/superuser");
        let and_node = nary(NodeKind::And, vec![role_eq, age_gte]);
        let policy = nary(NodeKind::Or, vec![and_node, superuser]);
        assert!(eval(state, policy).result);
    }

    #[test]
    fn complex_policy_fails() {
        let state = json!({ "role": "user", "age": 15, "superuser": false });
        let role_eq = binary(NodeKind::Eq, field("/role"), scalar("admin"));
        let age_gte = binary(NodeKind::Gte, field("/age"), scalar("18"));
        let superuser = field("/superuser");
        let and_node = nary(NodeKind::And, vec![role_eq, age_gte]);
        let policy = nary(NodeKind::Or, vec![and_node, superuser]);
        assert!(!eval(state, policy).result);
    }

    // ── array index via dot path ──────────────────────────────────────────

    #[test]
    fn dot_path_array_index() {
        let state = json!({ "items": [10, 20, 30] });
        // items.1 should resolve to 20
        let node = binary(NodeKind::Eq, field("items.1"), scalar("20"));
        assert!(eval(state, node).result);
    }

    // ── short-circuit evaluation ──────────────────────────────────────────

    #[test]
    fn and_short_circuits() {
        // AND with first child=false must not crash even if second child
        // would return an error (CALL node)
        let call_node = AstNode {
            version: None,
            kind: NodeKind::Call,
            children: vec![],
            path: None,
            value: None,
            call_name: Some("neverCalled".into()),
        };
        let policy = nary(NodeKind::And, vec![AstNode::literal_false(), call_node]);
        // Short-circuit means we never evaluate the CALL node
        assert!(!eval(json!({}), policy).result);
    }

    #[test]
    fn or_short_circuits() {
        let call_node = AstNode {
            version: None,
            kind: NodeKind::Call,
            children: vec![],
            path: None,
            value: None,
            call_name: Some("neverCalled".into()),
        };
        let policy = nary(NodeKind::Or, vec![AstNode::literal_true(), call_node]);
        assert!(eval(json!({}), policy).result);
    }

    // ── serde round-trip ──────────────────────────────────────────────────

    #[test]
    fn ast_serde_round_trip() {
        let state = json!({ "x": 42 });
        let node = binary(NodeKind::Gt, field("/x"), scalar("10"));
        let json_str = serde_json::to_string(&node).unwrap();
        let deserialized: AstNode = serde_json::from_str(&json_str).unwrap();
        assert_eq!(
            eval(state.clone(), node).result,
            eval(state, deserialized).result
        );
    }
}
