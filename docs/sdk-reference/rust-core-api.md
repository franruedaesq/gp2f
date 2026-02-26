# Rust Core API Reference: `policy-core`

This document describes the public API of the `policy-core` crate. This crate is the authoritative implementation of the GP2F policy evaluation engine. It compiles to both native Rust and WebAssembly via `wasm-pack`.

---

## Crate Overview

The `policy-core` crate is intentionally minimal and pure. It has no I/O dependencies, no network calls, and no global mutable state. Every function in the public API is deterministic: given the same inputs, it always produces the same outputs. This property is what makes the isomorphic evaluation guarantee possible.

**Cargo.toml dependencies:**

```toml
[dependencies]
prost = "0.12"                   # Protobuf serialization
blake3 = "1.5"                   # Content hashing
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"

[target.'cfg(target_arch = "wasm32")'.dependencies]
wasm-bindgen = "0.2"
js-sys = "0.3"

[dev-dependencies]
proptest = "1.4"
```

---

## The `PolicyCore` Struct

`PolicyCore` is the primary entry point for all policy evaluation. It holds a compiled, validated AST and exposes evaluation methods.

```rust
pub struct PolicyCore {
    ast: AstNode,
    version: String,
    version_hash: String,
}
```

### `PolicyCore::new`

```rust
pub fn new(ast: AstNode, version: String) -> Result<Self, PolicyCoreError>
```

Constructs a new `PolicyCore` from a validated `AstNode` tree and a version string. Computes the BLAKE3 hash of the serialized AST and stores it as `version_hash`.

Returns `Err(PolicyCoreError::InvalidAst)` if the AST fails structural validation (e.g., a node type that requires children has none, or a node references a field path with invalid syntax).

**Example:**

```rust
use policy_core::{PolicyCore, AstNode};

let ast = AstNode::from_json(r#"{
    "nodeType": "AND",
    "children": [
        { "nodeType": "EQUALS", "field": "document.status", "value": "draft" },
        { "nodeType": "EQUALS", "field": "user.role", "value": "contributor" }
    ]
}"#)?;

let core = PolicyCore::new(ast, "v2.3.1".to_string())?;
```

### `PolicyCore::from_protobuf`

```rust
pub fn from_protobuf(bytes: &[u8], version: String) -> Result<Self, PolicyCoreError>
```

Constructs a `PolicyCore` from a Protobuf-encoded `AstNode`. This is the preferred construction method for server deployments where ASTs are stored in Postgres as Protobuf bytes.

---

## The `evaluate` Method

```rust
pub fn evaluate(
    &self,
    state: &PolicyState,
    intent: &Intent,
) -> Result<EvalResult, PolicyCoreError>
```

The core evaluation method. Traverses the AST, evaluating each node against the provided `state` and `intent`. Returns an `EvalResult` describing the outcome and the evaluation trace.

This method is pure and thread-safe. It can be called concurrently from multiple threads; no locking is required.

**Parameters:**

`state` — The current state document. This must include all fields referenced by the AST. Missing fields are treated as `null` and may cause certain condition nodes to evaluate to `false`.

`intent` — The user's declared intent. At minimum, this must contain an `action_id` field. Additional fields are action-specific.

**Returns:**

`Ok(EvalResult)` on successful evaluation. `Err(PolicyCoreError::EvalError)` if evaluation cannot proceed (e.g., a field path in the AST references a field that does not exist in the state and the node type requires a non-null value).

**Example:**

```rust
use policy_core::{PolicyCore, PolicyState, Intent};
use serde_json::json;

let state = PolicyState::from_json(&json!({
    "document": { "status": "draft", "id": "doc_456" },
    "user": { "role": "contributor", "id": "usr_123" }
}))?;

let intent = Intent::from_json(&json!({
    "actionId": "submit_document",
    "documentId": "doc_456"
}))?;

let result = core.evaluate(&state, &intent)?;
assert!(result.permitted);
println!("Trace: {:?}", result.trace);
// Trace: ["Checking AND node", "document.status == 'draft': true", "user.role == 'contributor': true", "AND result: true"]
```

---

## The `evaluate_permitted_actions` Method

```rust
pub fn evaluate_permitted_actions(
    &self,
    state: &PolicyState,
) -> Result<PermittedActionsResult, PolicyCoreError>
```

Evaluates all registered actions against the current state and returns the set of actions for which `evaluate` would return `permitted: true`. This is used to populate the UI's list of available actions and to scope the AI sandbox's token vocabulary.

**Returns:**

```rust
pub struct PermittedActionsResult {
    pub permitted_actions: Vec<String>,
    pub snapshot_hash: String,
}
```

---

## The `EvalResult` Structure

```rust
pub struct EvalResult {
    pub permitted: bool,
    pub trace: Vec<String>,
    pub snapshot_hash: String,
}
```

**`permitted`** — Whether the intent is permitted under the current AST and state. `true` means the action is allowed; `false` means it is blocked.

**`trace`** — A human-readable log of every condition evaluated, in depth-first traversal order. Each entry describes the node type, the fields evaluated, and the outcome. This trace is stored in the event log for auditing.

**`snapshot_hash`** — The BLAKE3 hex hash of the serialized `PolicyState` at the time of evaluation. This value appears in the `op_id` payload as `client_state_hash` and is used by the server to verify that the client and server were evaluating the same state.

---

## The `AstNode` Structure and Protobuf Schema

`AstNode` is the Rust representation of a policy AST node. It maps directly to the Protobuf definition in `proto/policy_ast.proto`.

```rust
pub struct AstNode {
    pub node_type: NodeType,
    pub children: Vec<AstNode>,
    pub field: Option<String>,
    pub value: Option<AstValue>,
    pub action_id: Option<String>,
}

pub enum NodeType {
    And,
    Or,
    Not,
    Equals,
    NotEquals,
    GreaterThan,
    LessThan,
    Contains,
    In,
    Action,
    AllowAll,
    DenyAll,
}

pub enum AstValue {
    StringValue(String),
    IntValue(i64),
    FloatValue(f64),
    BoolValue(bool),
    NullValue,
    ListValue(Vec<AstValue>),
}
```

**Protobuf schema (`proto/policy_ast.proto`):**

```protobuf
syntax = "proto3";
package gp2f.policy;

message AstNode {
  NodeType node_type = 1;
  repeated AstNode children = 2;
  optional string field = 3;
  oneof value {
    string string_value = 4;
    int64 int_value = 5;
    double float_value = 6;
    bool bool_value = 7;
    bool null_value = 8;
    ListValue list_value = 9;
  }
  optional string action_id = 10;
}

message ListValue {
  repeated AstNode values = 1;
}

enum NodeType {
  AND = 0;
  OR = 1;
  NOT = 2;
  EQUALS = 3;
  NOT_EQUALS = 4;
  GREATER_THAN = 5;
  LESS_THAN = 6;
  CONTAINS = 7;
  IN = 8;
  ACTION = 9;
  ALLOW_ALL = 10;
  DENY_ALL = 11;
}
```

---

## The `PolicyState` Structure

```rust
pub struct PolicyState {
    pub fields: serde_json::Value,
    pub vector_clock: HashMap<String, u64>,
}
```

`PolicyState` is a dynamic document. The `fields` value is a JSON object with arbitrary keys and values. Field paths in the AST are dot-separated key sequences (e.g., `"document.status"` refers to `fields["document"]["status"]`).

The `vector_clock` is a map from peer ID to operation sequence number. It is used by the server to detect and resolve concurrent operations, but it is not evaluated by the AST directly.

### `PolicyState::from_json`

```rust
pub fn from_json(value: &serde_json::Value) -> Result<Self, PolicyCoreError>
```

Constructs a `PolicyState` from a `serde_json::Value`. The value must be a JSON object. Nested objects are supported; array fields are supported but can only be tested with the `Contains` and `In` node types.

---

## The `Intent` Structure

```rust
pub struct Intent {
    pub action_id: String,
    pub fields: serde_json::Value,
}
```

`Intent` represents the user's declared purpose. `action_id` is the primary discriminator used by `Action` nodes in the AST. Additional fields in `intent.fields` are accessible in the AST via field paths prefixed with `"intent."` (e.g., `"intent.documentId"`).

---

## Error Types

```rust
pub enum PolicyCoreError {
    InvalidAst(String),
    EvalError { node_type: String, reason: String },
    SerializationError(String),
    VersionMismatch { expected: String, actual: String },
}
```

**`InvalidAst`** — The AST tree is structurally invalid. The string contains a description of the specific validation failure.

**`EvalError`** — Evaluation failed at a specific node. `node_type` identifies which type of node caused the failure; `reason` describes the cause.

**`SerializationError`** — A Protobuf or JSON serialization/deserialization operation failed.

**`VersionMismatch`** — The AST version embedded in an `op_id` payload does not match the currently loaded AST version. This indicates that the client was evaluating a stale AST.

---

## WASM Bindings

When compiled with `wasm-pack`, the following JavaScript-visible functions are exported:

```typescript
// Corresponds to PolicyCore::new
export function createPolicyCore(astJson: string, version: string): PolicyCore;

// Corresponds to PolicyCore::evaluate
export function evaluate(
  core: PolicyCore,
  stateJson: string,
  intentJson: string
): string; // JSON-encoded EvalResult

// Corresponds to PolicyCore::evaluate_permitted_actions
export function evaluatePermittedActions(
  core: PolicyCore,
  stateJson: string
): string; // JSON-encoded PermittedActionsResult
```

All WASM-exported functions use JSON for input and output serialization to simplify JavaScript interop. The native Rust API uses strongly typed structs. This is the only API difference between the two targets.

---

## Performance Characteristics

All performance figures are measured on an M2 MacBook Pro with the release build.

**Native evaluation:** A typical 20-node AST evaluates in approximately 15 microseconds per call. A 200-node AST evaluates in approximately 80 microseconds.

**WASM evaluation (Wasmtime, server):** Approximately 1.3x the native cost due to the WASM runtime overhead. For a 20-node AST: approximately 20 microseconds.

**WASM evaluation (browser, V8):** Approximately 2x the native cost due to V8's WASM tier-up behavior on first evaluation. After JIT compilation (first 10–20 calls), performance approaches the Wasmtime figures.

**Memory:** A `PolicyCore` instance holds the compiled AST in memory. A 20-node AST occupies approximately 2KB. A 500-node enterprise policy AST occupies approximately 40KB. Instance creation is the expensive operation; evaluation is cheap.
