# 01_ast_policy_engine.md

## Architecture Review: Pillar 1 - Isomorphic AST Policy Engine

### Overview
This pillar forms the foundational logic layer of GP2F. By compiling a single Rust codebase to WASM for the client and native/Wasmtime for the server, the system ensures that business rules are applied consistently across all environments. The use of versioned Protobuf ASTs guarantees strict schema adherence and auditability.

### Pros
*   **Guaranteed Consistency:** Using the same Rust source for client and server logic eliminates the "dual implementation" problem common in web apps (JS vs Python/Java), ensuring that `eval(ast, context)` is always deterministic.
*   **Performance:** WASM execution on the client provides near-native speed, which is critical for achieving the < 16ms end-to-end latency target.
*   **Type Safety:** Protobuf for AST definition ensures strict schema validation, efficient binary serialization, and backward compatibility.
*   **Auditability:** Every evaluation trace is hashed (blake3), providing a cryptographic proof of the decision-making process that can be stored and verified later.

### Cons & Risks
*   **WASM Bundle Size:** Including the full policy engine and dependencies in the WASM bundle could impact initial load times, especially on mobile networks.
*   **Serialization Overhead:** Constant serialization/deserialization of ASTs and context between JS and WASM boundaries can add latency if not optimized.
*   **Floating Point Determinism:** Different architectures (ARM vs x86) and environments (Browser WASM vs Server Native) can handle floating-point operations slightly differently (e.g., NaN handling, precision), potentially breaking consensus.
*   **Timezone Handling:** If `chrono::DateTime` logic relies on system time without strict normalization to UTC, client/server divergence is likely due to timezone differences.

### Single Points of Failure (SPOF)
*   **The Evaluator Binary:** A bug in the core evaluator affects both client prediction and server reconciliation. If the evaluator is flawed, the entire system's state transitions are compromised.
*   **Schema Registry:** If schema versioning is mishandled, clients with old WASM binaries might generate ops that the server (on a new schema) rejects, or vice versa, leading to widespread synchronization failures.

## Testing Strategy

### Property-Based Testing (proptest)
To guarantee 100% evaluation parity between the browser and server environments, we must use property-based testing to generate millions of random ASTs and contexts.

*   **Strategy:** Define a `proptest` strategy that generates valid arbitrary `AstNode` structures and corresponding context data.
*   **Oracle:** The "oracle" is that `eval(ast, context)` must return the exact same `EvalResult` (result, trace, hash) on both the Native target and the WASM target.
*   **Implementation:** Create a test runner that executes the native evaluation, then instantiates a Wasmtime runtime to execute the same logic via WASM, comparing the outputs bit-for-bit.

### Specific Test Cases & Scenarios
*   **Floating Point Math:** Explicitly test `NaN`, `Infinity`, `-0.0`, and denormalized numbers. Verify that the WASM compilation target uses strict floating-point rules to match the server implementation.
*   **Timezone & Date Handling:** Inject context with timestamps in various offsets. Ensure all time math is done in UTC or a fixed offset within the engine. Verify `chrono` behavior matches exactly across targets.
*   **Serialization Round-Trips:** Fuzz the Protobuf serialization/deserialization to ensure no data corruption or precision loss occurs during the JS <-> WASM boundary crossing.
*   **Recursion & Stack Depth:** Generate deep ASTs to test for stack overflow limits in the WASM environment vs the native server environment.
*   **Memory Limits:** Test with large ASTs to ensure the WASM linear memory growth is handled correctly and doesn't crash the browser tab or server Wasmtime instance.

### Tools
*   **cargo-proptest:** For generating arbitrary inputs and shrinking failing cases to minimal reproductions.
*   **wasm-bindgen-test:** For running tests in a headless browser environment to catch browser-specific WASM quirks.
*   **wasmtime:** For running WASM tests in the server environment to ensure the server's Wasmtime instance matches the native build.
*   **criterion:** For benchmarking evaluation latency to ensure the < 2ms target is met consistently.

## Mitigation Plan of Action

### Phase 1: Bundle Optimization & Performance Tuning
**Goal:** Reduce initial load impact and serialization overhead.
*   **Step 1.1:** Implement `wasm-opt` in the build pipeline to strip debug symbols and optimize binary size.
*   **Step 1.2:** Use "Lazy Loading" for the policy engine. The UI shell loads first; the WASM blob fetches in the background.
*   **Step 1.3:** Benchmark Protobuf vs. FlatBuffers. If Protobuf serialization > 2ms, migrate hot paths to FlatBuffers (Zero-Copy).
*   **Testing:** Add a CI step that fails if `policy_engine.wasm` > 2MB. Use `criterion` to benchmark the JS<->WASM boundary cost.

### Phase 2: Determinism Hardening
**Goal:** Guarantee bit-for-bit parity across all platforms.
*   **Step 2.1:** Enforce `no_std` compatibility (or strict WASI profiles) for core logic to prevent accidental usage of OS-specific behavior.
*   **Step 2.2:** Replace standard math libraries with a deterministic fixed-point math crate (e.g., `fixed` or `rust_decimal`) if floating-point variance persists.
*   **Step 2.3:** Normalize all `DateTime` inputs to Unix timestamps (i64) at the boundary. Disallow timezone-aware structs inside the AST evaluation logic.
*   **Testing:** Run the "Oracle" proptest suite on CI against Linux (x86), macOS (ARM), and Windows agents.

### Phase 3: Schema Evolution Safety
**Goal:** Prevent client/server version mismatches from breaking sync.
*   **Step 3.1:** Implement a "Schema Negotiation" handshake. Client sends `schema_version` on connect; Server responds with `reload_required` if too old.
*   **Step 3.2:** Maintain a "compat-layer" in the server that can downcast new ASTs to older versions for read-only views if possible.
*   **Testing:** Create a "Time Travel" test suite where a v1 Client tries to sync with a v2 Server, verifying the graceful failure/upgrade path.
