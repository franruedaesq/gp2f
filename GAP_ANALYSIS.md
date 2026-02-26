# Gap Analysis: Temporal Implementation

This document details the missing components for the requested Temporal integration.

## Phase 1: Operational Simplicity

**Goal:** Manage infrastructure complexity effectively.

- [ ] **Step 1.1: Use "Temporal Cloud" or a managed K8s Helm chart.**
    - **Status:** Missing.
    - **Findings:** `helm/gp2f/values.yaml` contains placeholders for `TEMPORAL_ENDPOINT` but does not bundle Temporal or configure Cloud connectivity. No `temporal-operator` integration found.
- [ ] **Step 1.2: Implement strict "Workflow Linting" in CI.**
    - **Status:** Missing.
    - **Findings:** `.github/workflows/ci.yml` only runs standard Rust checks (`cargo check`, `cargo clippy`, `cargo test`). No `temporal-lint` or similar tools are present.

## Phase 2: Latency Optimization

**Goal:** Achieve < 16ms updates.

- [ ] **Step 2.1: Use "Local Activities".**
    - **Status:** Missing.
    - **Findings:** No usage of `LocalActivity` found in `server/`. Workflows are currently implemented using a custom in-memory engine (`server/src/workflow.rs`), not Temporal SDK.
- [ ] **Step 2.2: Implement an "Async Ingestion" pattern.**
    - **Status:** Missing.
    - **Findings:** `Reconciler` (`server/src/reconciler.rs`) processes operations synchronously in-memory. The `TemporalStore` (`server/src/temporal_store.rs`) is a stub that falls back to in-memory storage.

## Phase 3: Versioning Safety

**Goal:** Seamless upgrades.

- [ ] **Step 3.1: Adopt the "Patching API" (`workflow.patched()`).**
    - **Status:** Missing.
    - **Findings:** No usage of `patched()` found in the codebase.
- [ ] **Step 3.2: Use "Worker Versioning".**
    - **Status:** Missing.
    - **Findings:** No worker versioning logic or task queue separation found.
- [ ] **Testing: Create a "Replay Test Suite".**
    - **Status:** Missing.
    - **Findings:** No replay tests found. `server/tests/ai_e2e.rs` tests the in-memory engine.

## Phase 4: Database Scalability

**Goal:** Prevent Temporal internal writes from choking the DB.

- [ ] **Step 4.1: Isolate Temporal's persistence.**
    - **Status:** Missing.
    - **Findings:** Since Temporal is not integrated, there is no persistence configuration to isolate.
- [ ] **Step 4.2: Tune Postgres autovacuum and WAL settings.**
    - **Status:** Missing.
    - **Findings:** No Postgres configuration found in Helm charts or Dockerfiles.
- [ ] **Testing: Load test.**
    - **Status:** Partial.
    - **Findings:** `tests/load/ai_load.js` exists but tests the current in-memory implementation, not a Temporal-backed system.
