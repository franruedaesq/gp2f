# GP2F Architecture

## Overview

GP2F (Generic Policy & Prediction Framework) is a deterministic, local-first framework that decouples **business rules** (as versioned ASTs) from **routing and UI state**, enabling zero-latency predictive UIs, safe AI participation, and enterprise-grade auditability.

## The 4 Pillars

### Pillar 1: Isomorphic AST Policy Engine (Core)

All rules live as versioned JSON/Protobuf ASTs. The same evaluator binary runs on:

- **Client**: compiled to WASM via `wasm-pack`
- **Server**: native Rust or via Wasmtime

The evaluator is pure (zero I/O, zero side effects) and returns:

```rust
EvalResult {
    result: bool,
    trace: Vec<String>,
    snapshot_hash: String,   // BLAKE3 hex of the state document
}
```

### Pillar 2: Event-Sourced Synchronization (Latency Killer)

```
Client predicts → emits signed op_id → server reconciles → broadcasts ACCEPT/REJECT + 3-way patch
```

- **CRDTs** (Yrs / Yjs) for mergeable fields
- **LWW** (Last-Write-Wins) for simple scalar fields  
- **Transactional** for fields that must not conflict

### Pillar 3: Tokenized Agent Sandbox (Safe AI)

The LLM receives only:
- Semantic Vibe vector (intent, confidence, bottleneck)
- Ephemeral `tool_req_xxx` tokens (5-minute Redis TTL)
- Current AST-allowed actions

AI can only **propose** `op_id`s; the engine drops anything disallowed.

### Pillar 4: Semantic Vibe Engine (Context Management)

Lightweight on-device classifier → `{ intent, confidence, bottleneck }` payload.  
The policy engine uses vibe to proactively unlock AI assistance.

---

## C4 Context Diagram

```
┌─────────────────────────────────────────────────────────────┐
│                        GP2F System                          │
│                                                             │
│  ┌──────────┐     ┌──────────────┐     ┌────────────────┐  │
│  │  Client  │────▶│  WebSocket   │────▶│  Axum Server   │  │
│  │ (WASM +  │◀────│   Channel    │◀────│ (Reconciler)   │  │
│  │   TS)    │     └──────────────┘     └───────┬────────┘  │
│  └──────────┘                                  │            │
│                                     ┌──────────▼──────────┐ │
│                                     │  Temporal Workflow  │ │
│                                     │  (Append-only log)  │ │
│                                     └─────────────────────┘ │
└─────────────────────────────────────────────────────────────┘
```

---

## Repository Structure

```
gp2f/
├── proto/              # Protobuf definitions (ASTNode, OpId, wire protocol)
├── policy-core/        # Pure Rust AST evaluator (no I/O)
│   ├── src/
│   │   ├── ast.rs      # AstNode + NodeKind types
│   │   ├── evaluator.rs# Evaluator::evaluate() + unit tests
│   │   └── version.rs  # VersionPolicy (allow-list for AST versions)
│   └── tests/
│       └── property_tests.rs  # proptest: random state × random AST
├── wasm-sdk/           # WASM bindings (wasm-bindgen) – Phase 1
├── server/             # Axum backend (WebSocket + HTTP)
│   └── src/
│       ├── wire.rs     # ClientMessage / AcceptResponse / RejectResponse
│       └── reconciler.rs  # Stateful reconciler (append-only log)
├── client-sdk/         # TypeScript npm package – Phase 2
├── visual-editor/      # React + React Flow AST editor – Phase 6
├── cli/                # gp2f eval / replay / spoofer binaries
├── docs/               # This file + wire-protocol.md + ADRs
└── chaos-tests/        # Spoofer + chaos scenarios
```

---

## ADRs (Architecture Decision Records)

### ADR-001: WASM Isomorphism

**Decision**: Compile `policy-core` to WASM for the browser client; run the same binary via Wasmtime on the server.

**Rationale**: Guarantees 100% evaluation parity. Any divergence is caught by the property test suite that runs in both environments.

### ADR-002: Yrs (Yjs in Rust) for CRDTs

**Decision**: Use the `yrs` crate (Rust port of Yjs) for CRDT fields.

**Rationale**:
- Fastest real-world performance for text/array collaboration (2025–2026 benchmarks)
- Mature ecosystem with excellent WebSocket sync primitives
- Smaller memory footprint than Automerge (no full-history bloat by default)

### ADR-003: op_id Cryptographic Construction

**Decision**: `op_id = base64url(version | tenant_id | client_id | counter | ts_ms | nonce_16 | HMAC-SHA256)`

**Rationale**:
- Globally unique (counter + nonce)  
- Tamper-evident (HMAC)
- Replay-protected (server stores last 10 000 op_ids per client via bloom filter)
- Sortable by time for event sourcing

---

## Golden Metrics

| Metric | Target |
|--------|--------|
| `reconciliation_rate` | ≥ 99.9% client/server agreement |
| `eval_latency` p99 | < 2 ms client, < 5 ms server |
| `agent_tool_failure_rate` | 0 for disallowed actions |
| `offline_success_rate` | ≥ 99.9% queued ops reconcile on reconnect |
