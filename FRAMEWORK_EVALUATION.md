# Framework Evaluation: GP2F (Global Policy 2 Framework)

**Author:** Staff Architect
**Date:** Current
**Scope:** Backend (`server`), Policy Engine (`policy-core`), Client SDK (`client-sdk`)

---

## 1. Executive Summary

GP2F presents a compelling vision for a local-first, policy-driven workflow engine using CRDTs and event sourcing. The architectural choices—Rust for performance, WASM for portable policy execution, and the Actor Model for concurrency—are sound in principle.

However, the current implementation is strictly a **Proof of Concept (PoC)**. It lacks critical durability, scalability, and security features required for production deployment. The most severe issue is that **state is not persisted**; while a `PersistentStore` interface exists, it is not connected to the `ActorRegistry`, meaning all workflow state is lost upon server restart or actor eviction.

---

## 2. Critical Risks (P0)

### 2.1. Data Loss & Persistence Disconnect
The framework initializes a `PersistentStore` (e.g., Temporal or InMemory) in `main.rs`, but **it is never used by the workflow actors**.
- **Evidence**: `ActorRegistry::new()` uses a factory closure `|| Arc::new(Reconciler::new())`.
- **Impact**: `Reconciler::new()` initializes an empty in-memory `EventStore`. When an actor is spawned, it starts with zero history.
- **Consequence**: Any data written to the `Reconciler` exists only in that actor's RAM. If the pod restarts or the actor is dropped, **all data is lost**. The `PersistentStore` in `AppState` is effectively dead code.

### 2.2. `no_std` / WASM Incompatibility for CRDTs
The `policy-core` crate is designed to be `no_std` compatible for WASM environments. However, the `crdt` module (which provides the core conflict resolution logic via `yrs`) is gated behind the `std` feature.
- **Evidence**: `policy-core/src/lib.rs`: `#[cfg(feature = "std")] pub mod crdt;`.
- **Impact**: A WASM client or policy engine running in a restricted `no_std` environment cannot use the CRDT features. This breaks the "isomorphic policy" promise if CRDT merging is required on the client side without `std`.

### 2.3. Single Point of Failure (In-Memory State)
The architecture relies entirely on local `Mutex<HashMap<...>>` structures for:
- Event Storage (`server/src/event_store.rs`)
- CRDT Documents (`server/src/reconciler.rs`)
- Token Service (`server/src/token_service.rs`)
- Actor Registry (`server/src/actor.rs`)
- **Impact**: The system cannot be scaled horizontally. Running multiple server instances will result in split-brain scenarios as they do not share state.

---

## 3. Architectural Observations

### 3.1. The Actor Model
**Strengths**:
- The use of `ActorRegistry` to spawn a `WorkflowActor` per `tenant:workflow:instance` is excellent. It serializes operations for a specific entity, preventing race conditions without heavy global locking.
- `tokio::sync::mpsc` provides natural backpressure.

**Weaknesses**:
- **Lifecycle Management**: There is no "passivation" or "rehydration" logic. Actors live forever in memory until the server stops. There is no mechanism to unload idle actors or restore them from the database when needed.

### 3.2. Event Sourcing & Compaction
**Strengths**:
- The `EventStore` correctly implements an append-only log with HLC (Hybrid Logical Clock) timestamps.

**Weaknesses**:
- **Destructive Compaction**: The current compaction strategy (`compact_partition`) merges all accepted payloads into a snapshot and **discards** the individual events. This destroys the audit trail (who did what and when), leaving only the final state.
- **Naive Snapshotting**: Snapshots are just the current state JSON. For large workflows, this will grow unbounded.

### 3.3. Wire Protocol
- **Current**: Uses `serde_json` for everything (`ClientMessage`, `ServerMessage`).
- **Issues**:
    - **Performance**: JSON parsing is CPU-intensive and requires allocation.
    - **Type Safety**: No schema enforcement on the wire (unlike Protobuf).
    - **WASM**: Parsing JSON in WASM is slower than zero-copy formats like `rkyv` or `FlatBuffers`.
- **Recommendation**: Adopt the Protobuf definitions in `proto/gp2f.proto` as the source of truth and use `prost`.

---

## 4. Security & Reliability

### 4.1. Token Service
- **Issue**: `TokenService` stores tokens in a local `HashMap`.
- **Risk**: If the server restarts, all active tokens (e.g., for "approval" links emailed to users) become invalid.
- **Fix**: Store tokens in Redis with TTL.

### 4.2. LLM Guardrails
- **Issue**: `guardrail_check` in `main.rs` relies on simple string matching (e.g., `contains("ignore previous instructions")`).
- **Risk**: Trivial to bypass with variations (e.g., "IgNoRe", base64 encoding, or non-English prompts).
- **Fix**: Use a dedicated safety model (e.g., Llama Guard) or a more robust classification service.

### 4.3. Input Sanitization
- **Issue**: `sanitize_prompt_input` only strips control characters.
- **Risk**: Does not prevent prompt injection or structural attacks if the LLM output is used in downstream systems (e.g., SQL generation, though not currently visible).

---

## 5. Recommendations

### Immediate (P0 - Fixes)
1.  **Connect Persistence**: Modify `ActorRegistry` to accept the `PersistentStore`. When spawning an actor:
    - Load the event history (or snapshot) from the store.
    - Replay events into the `Reconciler` to restore `crdt_docs` and `state`.
    - Ensure new ops are written to the `PersistentStore` (not just the in-memory one) *before* acknowledging the client.
2.  **Enable `no_std` CRDTs**: Investigate if `yrs` can be compiled with `no_std` (using `alloc`), or abstract the CRDT layer to allow a pluggable backend.

### Short Term (P1 - Scalability)
3.  **Externalize State**:
    - Move `TokenService` to Redis.
    - Implement a Redis PubSub backend for `Broadcaster` (already hinted at in `Cargo.toml`).
4.  **Wire Protocol**: Switch from JSON to Protobuf (`prost`) to enforce schema and improve performance.

### Long Term (P2 - Maturity)
5.  **Passivation**: Implement an LRU cache for actors to unload idle workflows from memory.
6.  **Safety**: Integrate a proper AI Safety Gateway.
7.  **Testing**: Add simulation tests that restart the server and verify state is preserved.
