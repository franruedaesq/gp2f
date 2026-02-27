# Decoupling Feasibility Report

## Executive Summary

The GP2F project is well-structured for decoupling, particularly because its core logic is already isolated in separate Rust crates (`policy-core`, `gp2f-vibe`, `gp2f-crdt`).

Decoupling these components into independent npm packages (via WebAssembly/WASM) is **highly feasible** and **strongly recommended**. It would allow:
1.  **Client-side Policy Evaluation**: Running the policy engine in the browser for instant feedback and offline support.
2.  **Edge/Client-side AI**: Running the Vibe Classifier on the client to reduce latency and server load.
3.  **Universal Logic**: Sharing the exact same validation and reconciliation logic between server (Node.js/Rust) and client (Browser/React).

The effort required is **Medium**, primarily involving build configuration (`wasm-pack`) rather than code rewriting.

---

## Candidate Modules for Extraction

### 1. Policy Engine (`policy-core`)

*   **Description**: The isomorphic AST evaluator that governs workflow transitions.
*   **Current State**: Already a standalone crate with `no_std` support. The `client-sdk` already anticipates a WASM module (`@gp2f/policy-core-wasm`).
*   **Feasibility**: **High**. The code is pure Rust and designed for portability.
*   **Target Output**: `@gp2f/policy-engine-wasm` (npm package).
*   **Value**: **High**. Enables "optimistic UI" updates that are guaranteed to match server behavior, and allows offline policy checks.
*   **Effort**: **Low**. Add `wasm-bindgen` annotations and a `wasm-pack` build pipeline.

### 2. Vibe Classifier (`gp2f-vibe`)

*   **Description**: The behavioral signal engine (telemetry -> intent/confidence).
*   **Current State**: Contains a rule-based heuristic and a placeholder for ONNX. The logic is pure arithmetic.
*   **Feasibility**: **High**. The rule-based engine is trivial to port to WASM. The ONNX part would require `ort` or similar WASM runtimes, which is more complex but feasible.
*   **Target Output**: `@gp2f/vibe-engine-wasm` (npm package).
*   **Value**: **Medium/High**. Allows the client to compute the "Vibe Vector" locally, reducing payload size and server processing.
*   **Effort**: **Low** (for rule-based), **High** (for full ONNX support in WASM).

### 3. Conflict Resolution Logic (`gp2f-crdt`)

*   **Description**: The 3-way merge algorithm, CRDT application (`yrs`), and patch generation.
*   **Current State**: The `Reconciler` struct is coupled to server IO (EventStore, Redis). However, core functions like `three_way_merge` and `apply_op_with_crdt` are pure functions.
*   **Feasibility**: **Medium**. We cannot extract the full `Reconciler` (which acts as a server actor), but we *can* extract the **Merge Logic**.
*   **Target Output**: `@gp2f/crdt-logic-wasm` (npm package).
*   **Value**: **Medium**. Useful for building "thick clients" or P2P extensions that need to perform merges locally.
*   **Effort**: **Medium**. Requires refactoring `gp2f-crdt` to strictly separate pure logic from IO/Persistence.

---

## Implementation Strategy & Backward Compatibility

The recommended strategy is the **"Core + Bindings"** approach. We do not need to change the existing `client-sdk` or `server` implementation immediately.

### 1. Create WASM Wrappers
Create new sub-directories (or crates) for the WASM bindings:
*   `modules/policy-engine-wasm`: Wraps `policy-core` with `wasm-bindgen`.
*   `modules/vibe-engine-wasm`: Wraps `gp2f-vibe` with `wasm-bindgen`.

### 2. Backward Compatibility
*   **`@gp2f/server`**: Remains a Node.js native addon (`napi-rs`). It continues to link against the **native Rust crates** directly for maximum performance. **No changes required.**
*   **`@gp2f/client-sdk`**: The SDK already has a lazy-loader for the policy engine. We simply publish the new WASM package and update the SDK to import it if available. **No breaking changes.**

### 3. Build Pipeline
*   Use `wasm-pack` to compile the Rust code to WebAssembly.
*   Publish these as standard scoped npm packages (`@gp2f/...`).

---

## Work Estimation

| Task | Estimate | Complexity | Status |
| :--- | :--- | :--- | :--- |
| **Extract Policy Engine** | 2-3 Days | Low | **Ready** (Code is `no_std`) |
| **Extract Vibe Classifier** | 1-2 Days | Low | **Ready** (Pure logic) |
| **Extract CRDT Logic** | 3-5 Days | Medium | **Needs Refactor** (Separate Logic from IO) |
| **CI/CD Setup (WASM)** | 1-2 Days | Medium | **New** (Requires `wasm-pack` pipeline) |

## Recommendation

**Yes, it is a good idea.** It aligns perfectly with the "Local-First" philosophy of the project.

**Start with `policy-core`**. It delivers the highest immediate value (client-side policy enforcement) and is the "low hanging fruit" as the codebase is already prepared for it.
