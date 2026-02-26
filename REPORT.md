# Implementation Status Report

## Phase 1: Bundle Optimization & Performance Tuning

**Goal: Reduce initial load impact and serialization overhead.**

*   **Step 1.1: Implement wasm-opt in the build pipeline.**
    *   **Status:** Not Implemented.
    *   **Missing:** No `wasm-opt` usage found in `policy-core` or root build scripts (Cargo.toml, Github Workflows). The current build process uses standard `cargo build` / `cargo test`.

*   **Step 1.2: Use "Lazy Loading" for the policy engine.**
    *   **Status:** Not Implemented.
    *   **Missing:** The `client-sdk` currently relies solely on server-side evaluation via WebSocket. There is no code in `client-sdk` (src/client.ts, src/index.ts) that loads a WASM module, lazy or otherwise.

*   **Step 1.3: Benchmark Protobuf vs. FlatBuffers.**
    *   **Status:** Not Implemented.
    *   **Missing:** The current wire protocol is JSON over WebSocket (`serde_json`). While `gp2f.proto` exists, it is not used for the client-server communication path in the SDK. There is no FlatBuffers implementation or benchmarking infrastructure for serialization.

*   **Testing:**
    *   **Status:** Not Implemented.
    *   **Missing:** No CI step checks `policy_engine.wasm` size. No benchmarks for JS<->WASM boundary cost (as the client doesn't use WASM).

## Phase 2: Determinism Hardening

**Goal: Guarantee bit-for-bit parity across all platforms.**

*   **Step 2.1: Enforce no_std compatibility.**
    *   **Status:** Not Implemented.
    *   **Missing:** `policy-core` uses `std` (standard library) features like `Vec`, `String`, and `format!`. The `#![no_std]` attribute is absent in `lib.rs`.

*   **Step 2.2: Replace standard math libraries with a deterministic fixed-point math crate.**
    *   **Status:** Not Implemented.
    *   **Missing:** `policy-core/src/evaluator.rs` uses standard floating-point numbers (`f64`). No `fixed` or `rust_decimal` dependencies are present in `Cargo.toml`.

*   **Step 2.3: Normalize all DateTime inputs to Unix timestamps (i64).**
    *   **Status:** Not Implemented.
    *   **Missing:** `policy-core` depends on `chrono`. While `gp2f.proto` defines `int64 timestamp`, the core evaluator logic does not explicitly enforce Unix timestamp normalization or disallow timezone-aware structs.

*   **Testing:**
    *   **Status:** Not Implemented.
    *   **Missing:** No "Oracle" proptest suite running on multiple platforms (Linux, macOS, Windows) in CI. The current `ci.yml` only runs on `ubuntu-latest`.

## Phase 3: Schema Evolution Safety

**Goal: Prevent client/server version mismatches from breaking sync.**

*   **Step 3.1: Implement a "Schema Negotiation" handshake.**
    *   **Status:** Not Implemented.
    *   **Missing:** The `client-sdk` sends an `astVersion` field in messages, but there is no initial handshake mechanism where the server validates the version upon connection and responds with a "reload_required" message if the version is too old.

*   **Step 3.2: Maintain a "compat-layer" in the server.**
    *   **Status:** Not Implemented.
    *   **Missing:** No explicit compatibility layer or downcasting logic for older AST versions was found in `server/src`. The server passes the version string to the actor/reconciler but doesn't seem to have a dedicated transformation layer for backward compatibility.

*   **Testing:**
    *   **Status:** Not Implemented.
    *   **Missing:** No "Time Travel" test suite found that specifically tests v1 Client against v2 Server scenarios.
