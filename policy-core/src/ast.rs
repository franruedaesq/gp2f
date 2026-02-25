use serde::{Deserialize, Serialize};

/// Every rule-tree node.
///
/// Mirrors the `ASTNode` message in `proto/gp2f.proto`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AstNode {
    /// Semver string, e.g. `"1.0.0"`.  Only required on the root node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,

    /// The operation this node performs.
    pub kind: NodeKind,

    /// Child nodes (for composite operators: AND, OR, NOT, EQ, …).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<AstNode>,

    /// JSON-pointer path used by `FIELD` and `EXISTS` nodes (e.g. `/user/role`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,

    /// String-encoded scalar value for leaf nodes (e.g. `"admin"`, `"42"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,

    /// Name of the external function – used only by `CALL` nodes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_name: Option<String>,
}

/// All node kinds supported by the GP2F AST evaluator.
///
/// Matches `NodeKind` enum in `proto/gp2f.proto`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum NodeKind {
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
    /// Left operand is contained in the right (array).
    In,
    /// Right operand is contained in the left (array/string).
    Contains,
    /// Path exists in state (non-null).
    Exists,
    /// Resolve a JSON-pointer path from the state document.
    Field,
    /// Future-proof stub for external function calls.
    Call,
}

impl AstNode {
    /// Convenience constructor for a literal-true leaf.
    pub fn literal_true() -> Self {
        Self {
            version: None,
            kind: NodeKind::LiteralTrue,
            children: vec![],
            path: None,
            value: None,
            call_name: None,
        }
    }

    /// Convenience constructor for a literal-false leaf.
    pub fn literal_false() -> Self {
        Self {
            version: None,
            kind: NodeKind::LiteralFalse,
            children: vec![],
            path: None,
            value: None,
            call_name: None,
        }
    }

    /// Build a versioned root node wrapping another node.
    pub fn versioned(version: impl Into<String>, inner: AstNode) -> Self {
        let mut node = inner;
        node.version = Some(version.into());
        node
    }
}
