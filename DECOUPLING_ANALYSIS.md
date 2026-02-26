# Decoupling Analysis

This document analyzes the `gp2f` codebase to identify components that can be extracted into standalone libraries or tools. The goal is to decouple reusable features to enhance modularity and enable their use in other projects.

## Overview

The `gp2f` project contains several well-defined components that, while currently integrated into the server or policy core, have generic utility. Extracting these components would:
1.  **Reduce Coupling**: Cleaner boundaries between business logic and infrastructure.
2.  **Enable Reusability**: Components like the Hybrid Logical Clock or Vibe Classifier can be used in other distributed or local-first systems.
3.  **Simplify Testing**: Standalone components are easier to unit test in isolation.

## Identified Reusable Components

The following components have been identified as strong candidates for extraction:

1.  **`policy-core`** (Isomorphic AST Policy Engine)
2.  **`hlc-rs`** (Hybrid Logical Clock)
3.  **`replay-guard`** (Replay Protection)
4.  **`vibe-classifier`** (Behavioral Signal Engine)
5.  **`token-service`** (Ephemeral Token Service)
6.  **`simple-event-store`** (In-Memory Event Store)

---

### 1. `policy-core` (Isomorphic AST Policy Engine)

**Current Location**: `policy-core/` (Already a crate)

**Description**:
A pure Rust engine for evaluating versioned JSON ASTs against a JSON state. It supports a wide range of operators (logic, comparison, collection) and is designed to compile to WASM for client-side use.

**Dependencies**:
- `serde`, `serde_json` (Serialization)
- `thiserror` (Error handling)
- `blake3` (Hashing)
- `rust_decimal` (Fixed-point arithmetic)
- `yrs` (CRDT support - optional/std)
- `chrono` (Time - optional/std)

**Refactoring Needed**:
- The crate is already well-structured.
- Ensure `no_std` support is fully verified if targeting embedded or strict WASM environments without `std`.
- Documentation could be improved to highlight its standalone nature.

**Potential Usage**:
- Any project requiring shared validation logic between backend and frontend (e.g., form validation, permission checks, feature flagging).
- As a foundation for other rule-based systems.

---

### 2. `hlc-rs` (Hybrid Logical Clock)

**Current Location**: `server/src/hlc.rs`

**Description**:
A thread-safe implementation of a Hybrid Logical Clock (HLC). It combines physical time with a logical counter to guarantee monotonic, causally ordered timestamps in distributed systems.

**Dependencies**:
- `chrono` (Time)
- `std::sync::Mutex`

**Refactoring Needed**:
- Extract `server/src/hlc.rs` into a new crate `hlc-rs`.
- It is self-contained and requires no changes to logic.

**Potential Usage**:
- Distributed databases or sync engines needing causal ordering.
- logging systems requiring strictly ordered timestamps.

---

### 3. `replay-guard` (Replay Protection)

**Current Location**: `server/src/replay_protection.rs`

**Description**:
A replay protection mechanism using a sliding window of exact op_ids and Bloom filters for older history. It provides a memory-efficient way to detect duplicate operations in high-throughput systems.

**Dependencies**:
- `std` (Collections: HashMap, HashSet, VecDeque)

**Refactoring Needed**:
- Extract `server/src/replay_protection.rs` into a new crate `replay-guard`.
- The `ClientEntry` struct and logic are generic over `op_id` (String).
- Could be made generic over the key type (currently hardcoded to `String` for client_id and op_id).

**Potential Usage**:
- Idempotency keys in API servers.
- Deduplication in message processing pipelines.

---

### 4. `vibe-classifier` (Behavioral Signal Engine)

**Current Location**: `server/src/vibe_classifier.rs`

**Description**:
A lightweight engine to classify user intent based on behavioral telemetry (mouse velocity, keypress deltas, errors). It features a rule-based heuristic fallback and support for hot-swapping ONNX models.

**Dependencies**:
- `serde`
- `reqwest` (for fetching models - optional)
- `tracing` (Logging)
- `std`

**Refactoring Needed**:
- Extract `server/src/vibe_classifier.rs`.
- Decouple `VibeVector` and `VibeInput` from `crate::wire` (move definitions into the library).
- Make `reqwest` an optional feature for the model loader.

**Potential Usage**:
- Client-side or server-side user intent detection in web apps.
- Bot detection or frustration monitoring.

---

### 5. `token-service` (Ephemeral Token Service)

**Current Location**: `server/src/token_service.rs`

**Description**:
A service for minting and redeeming one-time-use tokens with expiry and locking mechanisms. It is currently used for AI agent tool authorization.

**Dependencies**:
- `serde`
- `thiserror`
- `blake3`
- `std`

**Refactoring Needed**:
- Extract `server/src/token_service.rs`.
- The struct `TokenRecord` is currently coupled to specific domain fields (`tenant_id`, `workflow_id`, `instance_id`, `op_name`).
- **Refactor**: Make the metadata payload generic (`T: Serialize + Deserialize`). The service would then manage `TokenRecord<T>`.

**Potential Usage**:
- Magic link generation.
- One-time password (OTP) verification.
- Temporary API access grants.

---

### 6. `simple-event-store` (In-Memory Event Store)

**Current Location**: `server/src/event_store.rs`

**Description**:
An in-memory, append-only event store with partition-based compaction. It supports storing events, retrieving by partition, and compacting old events into snapshots.

**Dependencies**:
- `serde`, `serde_json`
- `chrono`
- `std`
- `crate::hlc` (which would be `hlc-rs`)
- `crate::wire` (ClientMessage)

**Refactoring Needed**:
- Extract `server/src/event_store.rs`.
- Decouple from `ClientMessage`. Make the `Event` type generic, or define a trait `StoredMessage` that provides necessary fields (partition key, payload for compaction).
- The compaction logic (`compact_partition`) assumes a specific JSON structure (merging payloads). This logic might need to be injectable or trait-based.

**Potential Usage**:
- Prototyping event-sourced applications.
- Local-first state management where a full database isn't needed (e.g., embedded devices, testing).

## Integration Strategy

To integrate these extracted components back into `gp2f`:

1.  **Workspace Structure**: Move these components into a `crates/` directory in the root (or keep them at root level) and add them to the Cargo workspace.
    ```toml
    [workspace]
    members = [
        "cli",
        "gp2f-node",
        "policy-core",
        "server",
        "crates/hlc-rs",
        "crates/replay-guard",
        "crates/vibe-classifier",
        # ...
    ]
    ```

2.  **Server Dependency**: Update `server/Cargo.toml` to depend on these new local crates.
    ```toml
    [dependencies]
    hlc-rs = { path = "../crates/hlc-rs" }
    replay-guard = { path = "../crates/replay-guard" }
    # ...
    ```

3.  **Client-Side Integration**:
    - `policy-core` is already WASM-ready.
    - `vibe-classifier` could be compiled to WASM for client-side inference, enabling the "on-device" promise fully (currently it runs on server or via `gp2f-node`).

## Conclusion

The `gp2f` codebase contains valuable, separable logic. Extracting `hlc-rs`, `replay-guard`, and `vibe-classifier` requires minimal effort and yields immediate benefits. `token-service` and `simple-event-store` require moderate refactoring to generalize but offer significant reuse potential. `policy-core` serves as the strong, standalone foundation it was designed to be.
