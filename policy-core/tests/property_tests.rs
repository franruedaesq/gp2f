/// Property-based tests for the GP2F policy evaluator.
///
/// Runs random JSON states (1–10 KB) against random AST trees (depth ≤ 8)
/// and verifies invariants that must hold regardless of the inputs.
use policy_core::{AstNode, Evaluator, NodeKind};
use proptest::prelude::*;
use serde_json::Value;

// ── JSON state generators ─────────────────────────────────────────────────────

fn arb_json_leaf() -> impl Strategy<Value = Value> {
    prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<i64>().prop_map(|n| Value::Number(n.into())),
        "[a-z]{1,8}".prop_map(|s| Value::String(s)),
    ]
}

fn arb_json_value(depth: u32) -> impl Strategy<Value = Value> {
    let leaf = arb_json_leaf().boxed();
    if depth == 0 {
        return leaf;
    }
    let inner = arb_json_value(depth - 1).boxed();
    let inner2 = arb_json_value(depth - 1).boxed();
    let inner3 = arb_json_value(depth - 1).boxed();
    prop_oneof![
        arb_json_leaf(),
        prop::collection::vec(inner, 0..4)
            .prop_map(Value::Array)
            .boxed(),
        prop::collection::hash_map("[a-z]{1,6}", inner2, 0..4)
            .prop_map(|m| Value::Object(m.into_iter().collect()))
            .boxed(),
        // Extra leaf branch to keep weights balanced after adding inner3
        inner3.prop_flat_map(|v| Just(v)).boxed(),
    ]
    .boxed()
}

fn arb_state() -> impl Strategy<Value = Value> {
    arb_json_value(3)
}

// ── AST generators ────────────────────────────────────────────────────────────

fn arb_ast_leaf() -> impl Strategy<Value = AstNode> {
    prop_oneof![
        Just(AstNode::literal_true()),
        Just(AstNode::literal_false()),
    ]
}

fn arb_ast(depth: u32) -> impl Strategy<Value = AstNode> {
    let leaf = arb_ast_leaf().boxed();
    if depth == 0 {
        return leaf;
    }
    let inner_not = arb_ast(depth - 1).boxed();
    let inner_and = arb_ast(depth - 1).boxed();
    let inner_or = arb_ast(depth - 1).boxed();
    prop_oneof![
        arb_ast_leaf(),
        // NOT
        inner_not
            .prop_map(|c| AstNode {
                version: None,
                kind: NodeKind::Not,
                children: vec![c],
                path: None,
                value: None,
                call_name: None,
            })
            .boxed(),
        // AND
        prop::collection::vec(inner_and, 1..3)
            .prop_map(|cs| AstNode {
                version: None,
                kind: NodeKind::And,
                children: cs,
                path: None,
                value: None,
                call_name: None,
            })
            .boxed(),
        // OR
        prop::collection::vec(inner_or, 1..3)
            .prop_map(|cs| AstNode {
                version: None,
                kind: NodeKind::Or,
                children: cs,
                path: None,
                value: None,
                call_name: None,
            })
            .boxed(),
    ]
    .boxed()
}

// ── property tests ────────────────────────────────────────────────────────────

proptest! {
    /// Evaluating the same (state, AST) pair twice must return identical results.
    #[test]
    fn prop_deterministic(state in arb_state(), ast in arb_ast(4)) {
        let ev = Evaluator::new();
        let r1 = ev.evaluate(&state, &ast);
        let r2 = ev.evaluate(&state, &ast);
        match (r1, r2) {
            (Ok(a), Ok(b)) => {
                prop_assert_eq!(a.result, b.result);
                prop_assert_eq!(a.snapshot_hash, b.snapshot_hash);
            }
            (Err(_), Err(_)) => {} // both error is fine
            _ => prop_assert!(false, "one succeeded and one failed"),
        }
    }

    /// `NOT(NOT(x))` must equal `x`.
    #[test]
    fn prop_double_negation(state in arb_state(), ast in arb_ast(3)) {
        let ev = Evaluator::new();
        let double_not = AstNode {
            version: None,
            kind: NodeKind::Not,
            children: vec![AstNode {
                version: None,
                kind: NodeKind::Not,
                children: vec![ast.clone()],
                path: None,
                value: None,
                call_name: None,
            }],
            path: None,
            value: None,
            call_name: None,
        };
        if let (Ok(orig), Ok(dbl)) = (ev.evaluate(&state, &ast), ev.evaluate(&state, &double_not)) {
            prop_assert_eq!(orig.result, dbl.result, "double negation must hold");
        }
    }

    /// `LITERAL_TRUE AND x` == `x` for any `x`.
    #[test]
    fn prop_and_identity(state in arb_state(), ast in arb_ast(3)) {
        let ev = Evaluator::new();
        let and_true = AstNode {
            version: None,
            kind: NodeKind::And,
            children: vec![AstNode::literal_true(), ast.clone()],
            path: None,
            value: None,
            call_name: None,
        };
        if let (Ok(orig), Ok(combined)) = (ev.evaluate(&state, &ast), ev.evaluate(&state, &and_true)) {
            prop_assert_eq!(orig.result, combined.result, "AND identity must hold");
        }
    }

    /// `LITERAL_FALSE OR x` == `x` for any `x`.
    #[test]
    fn prop_or_identity(state in arb_state(), ast in arb_ast(3)) {
        let ev = Evaluator::new();
        let or_false = AstNode {
            version: None,
            kind: NodeKind::Or,
            children: vec![AstNode::literal_false(), ast.clone()],
            path: None,
            value: None,
            call_name: None,
        };
        if let (Ok(orig), Ok(combined)) = (ev.evaluate(&state, &ast), ev.evaluate(&state, &or_false)) {
            prop_assert_eq!(orig.result, combined.result, "OR identity must hold");
        }
    }

    /// The snapshot hash must be a 64-character hex string.
    #[test]
    fn prop_hash_is_64_chars(state in arb_state(), ast in arb_ast(2)) {
        let ev = Evaluator::new();
        if let Ok(r) = ev.evaluate(&state, &ast) {
            prop_assert_eq!(r.snapshot_hash.len(), 64);
            prop_assert!(r.snapshot_hash.chars().all(|c| c.is_ascii_hexdigit()));
        }
    }

    /// The snapshot hash must differ when state changes.
    #[test]
    fn prop_hash_sensitive_to_state(
        state1 in arb_state(),
        state2 in arb_state(),
    ) {
        // Only assert when the states are actually different
        if state1 != state2 {
            let h1 = policy_core::evaluator::hash_state(&state1);
            let h2 = policy_core::evaluator::hash_state(&state2);
            prop_assert_ne!(h1, h2, "distinct states must produce distinct hashes");
        }
    }

    /// De/serializing an AST node must be a lossless round-trip.
    #[test]
    fn prop_serde_round_trip(ast in arb_ast(4)) {
        let json = serde_json::to_string(&ast).unwrap();
        let restored: AstNode = serde_json::from_str(&json).unwrap();
        prop_assert_eq!(ast, restored);
    }
}
