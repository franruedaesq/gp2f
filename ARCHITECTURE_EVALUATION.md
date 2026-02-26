# Architecture Evaluation & Modular Monolith Transition Plan

## 1. Evaluation of Current Architecture

The current architecture is a **Hybrid Monolithic Workspace** consisting of 4 distinct crates:

1.  **`server`**: A monolithic crate containing the majority of the business logic, service boundaries, and application state. It encompasses approximately 30 modules (`actor`, `event_store`, `token_service`, etc.) within a single compilation unit.
2.  **`policy-core`**: A standalone library crate responsible for the deterministic policy evaluation engine.
3.  **`cli`**: A command-line interface crate.
4.  **`gp2f-node`**: Node.js bindings via `napi-rs`.

### Comparison with "Modular Monolith" Definition

The provided definition of a "Modular Monolith" includes specific criteria:

*   **Internal Modularity (14 Crates):** **MISMATCH**. The current system has high *logical* modularity (clean module boundaries within `server/src`), but low *physical* modularity (only 4 crates, not 14). The `server` crate is a large monolithic block.
*   **Unified Binary:** **MATCH**. The `server` crate compiles into a single executable binary (`gp2f-server`).
*   **Simple Deployment:** **MATCH**. Deployment involves a single artifact without complex microservice orchestration.
*   **High Performance:** **MATCH**. Communication between logical components (e.g., Actor -> Event Store) happens via direct Rust function calls, avoiding network serialization overhead.

### Assessment

The current architecture is **poised for transition**. It already exhibits the "Modular Monolith" philosophy in spirit (logical separation, single binary), but lacks the physical enforcement of boundaries that separate crates provide. Transitioning to a 14-crate structure is a natural evolution of the existing codebase.

## 2. Feasibility of Transition

**Feasibility:** **HIGH**

The transition is highly feasible for the following reasons:

1.  **Existing Module Structure**: The `server/src` directory is already organized into distinct modules with clear responsibilities (e.g., `actor`, `security`, `store`).
2.  **Rust Workspace Support**: Rust's workspace feature natively supports multi-crate projects that compile into a single binary, making the refactoring mechanical rather than architectural.
3.  **Shared Types**: The presence of `policy-core` as a separate crate demonstrates the project already handles shared dependencies correctly.
4.  **Performance Retention**: Breaking the monolith into crates within the same workspace preserves the "Unified Binary" and "High Performance" characteristics, as cross-crate calls are still just function calls at runtime (monomorphization happens at compile time).

## 3. Step-by-Step Transition Plan

The following plan outlines the steps to decompose the `server` monolith into ~14 specialized crates.

### Phase 1: Foundation (Shared Types)

The goal is to extract common types to prevent circular dependencies.

1.  **Create `gp2f-core` crate**:
    *   **Responsibility**: Common types, traits, and utilities used across the system.
    *   **Move**: `server/src/wire.rs`, `server/src/hlc.rs`, `server/src/error.rs` (if exists), and shared traits from `lib.rs`.
    *   **Dependencies**: `serde`, `thiserror`, `chrono`.

2.  **Create `gp2f-crdt` crate**:
    *   **Responsibility**: Conflict resolution logic.
    *   **Move**: `server/src/reconciler.rs`.
    *   **Dependencies**: `gp2f-core`, `policy-core`.

### Phase 2: Infrastructure & Security

Extract low-level system components.

3.  **Create `gp2f-security` crate**:
    *   **Responsibility**: Authentication, authorization, and cryptography.
    *   **Move**: `server/src/rbac.rs`, `server/src/secrets.rs`, `server/src/signature.rs`, `server/src/replay_protection.rs`.
    *   **Dependencies**: `gp2f-core`, `ring`/`ed25519-dalek`.

4.  **Create `gp2f-store` crate**:
    *   **Responsibility**: Persistence abstractions and implementations.
    *   **Move**: `server/src/event_store.rs`, `server/src/postgres_store.rs`, `server/src/temporal_store.rs`.
    *   **Dependencies**: `gp2f-core`, `sqlx`.

5.  **Create `gp2f-broadcast` crate**:
    *   **Responsibility**: Real-time event propagation.
    *   **Move**: `server/src/broadcast.rs`, `server/src/redis_broadcast.rs`.
    *   **Dependencies**: `gp2f-core`, `redis`.

### Phase 3: Domain Logic

Extract the core business logic.

6.  **Create `gp2f-actor` crate**:
    *   **Responsibility**: Actor model implementation and lifecycle.
    *   **Move**: `server/src/actor.rs`.
    *   **Dependencies**: `gp2f-core`, `gp2f-store`, `gp2f-security`.

7.  **Create `gp2f-workflow` crate**:
    *   **Responsibility**: Workflow definitions and orchestration.
    *   **Move**: `server/src/workflow.rs`, `server/src/pilot_workflows.rs`.
    *   **Dependencies**: `gp2f-core`.

8.  **Create `gp2f-token` crate**:
    *   **Responsibility**: Token management and rate limiting.
    *   **Move**: `server/src/token_service.rs`, `server/src/rate_limit.rs`, `server/src/limits.rs`.
    *   **Dependencies**: `gp2f-core`, `gp2f-store`.

9.  **Create `gp2f-vibe` crate**:
    *   **Responsibility**: AI/ML classification and analysis.
    *   **Move**: `server/src/vibe_classifier.rs`, `server/src/llm_audit.rs`, `server/src/llm_provider.rs`.
    *   **Dependencies**: `gp2f-core`, `reqwest`.

10. **Create `gp2f-runtime` crate**:
    *   **Responsibility**: WASM execution environment.
    *   **Move**: `server/src/wasm_engine.rs`, `server/src/compat.rs`.
    *   **Dependencies**: `gp2f-core`, `wasmtime`.

### Phase 4: Application Assembly

Reassemble the components into the final application.

11. **Create `gp2f-ingest` crate**:
    *   **Responsibility**: Async ingestion pipeline.
    *   **Move**: `server/src/async_ingestion.rs`.
    *   **Dependencies**: `gp2f-core`, `gp2f-store`, `gp2f-broadcast`.

12. **Create `gp2f-api` crate**:
    *   **Responsibility**: HTTP/WebSocket handlers and middleware.
    *   **Move**: `server/src/handlers.rs`, `server/src/middleware.rs`, `server/src/tool_gating.rs`.
    *   **Dependencies**: `axum`, `gp2f-core`, `gp2f-actor`, `gp2f-security`.

13. **Create `gp2f-canary` crate**:
    *   **Responsibility**: Testing and stability tools.
    *   **Move**: `server/src/canary.rs`, `server/src/chaos.rs`, `server/src/replay_testing.rs`.
    *   **Dependencies**: `gp2f-core`, `gp2f-actor`.

14. **Refactor `server` crate**:
    *   **Responsibility**: The "Main" binary entrypoint.
    *   **Action**: This crate becomes the "glue" code. It will contain `main.rs` and depend on all the above crates to wire them together.
    *   **Content**: `main.rs` wiring logic.

### Summary of Target Crates (14 Total)

1.  `gp2f-core`
2.  `gp2f-crdt`
3.  `gp2f-security`
4.  `gp2f-store`
5.  `gp2f-broadcast`
6.  `gp2f-actor`
7.  `gp2f-workflow`
8.  `gp2f-token`
9.  `gp2f-vibe`
10. `gp2f-runtime`
11. `gp2f-ingest`
12. `gp2f-api`
13. `gp2f-canary`
14. `gp2f-server` (Main)

*(Note: `policy-core`, `cli`, and `gp2f-node` remain as separate auxiliary crates.)*

### Execution Checklist

- [ ] Create new crate directories in `crates/` or root.
- [ ] Add new crates to root `Cargo.toml` workspace members.
- [ ] Move source files from `server/src/` to respective crates.
- [ ] Update `Cargo.toml` for each new crate with required dependencies.
- [ ] Update `server/Cargo.toml` to depend on the new crates.
- [ ] Fix visibility (`pub`) and imports (`use crate::...` -> `use gp2f_core::...`).
- [ ] Verify `cargo check` passes at each step to ensure no circular dependencies are introduced.
