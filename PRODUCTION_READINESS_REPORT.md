# Production Readiness Report: GP2F

**Date:** 2024-05-23
**Status:** **DEMO MODE (NOT PRODUCTION READY)**

## Executive Summary

The current GP2F implementation is functionally complete for demonstration purposes but lacks critical durability, scalability, and operational features required for a production deployment. The system defaults to in-memory storage for events, tokens, and replay protection, meaning **all state is lost on process restart**.

Three P0 critical issues must be resolved before any production traffic can be served:
1.  **Actor Data Loss:** Workflow actors do not replay history on startup.
2.  **Postgres Integrity:** The `PostgresStore` implementation contains a race condition and ambiguous error handling.
3.  **Missing Persistence:** `TemporalStore` is a stub, and Redis backends are optional/gated.

---

## 1. Durability & Data Integrity (P0)

### 1.1 Actor State Loss (Critical)
**Issue:**
The `ActorRegistry` initializes `WorkflowActor` instances with an empty state (via `Reconciler::new()`). Although the actor *appends* new events to the `PersistentStore`, it **never reads** existing events from the store during initialization.
**Impact:**
If a pod restarts or an actor is evicted from memory, the next operation will be processed against an empty state, causing:
-   Snapshot hash mismatches (immediate rejection of valid client ops).
-   Loss of all prior business data.
**Fix:**
Modify `WorkflowActor::new` (or a new `WorkflowActor::recover` method) to:
1.  Query `PersistentStore::events_for(key)`.
2.  Replay all events through the `Reconciler` to rebuild the in-memory state.
3.  Only start serving new requests once replay is complete.

### 1.2 PostgresStore Race Condition
**Issue:**
The `PostgresStore::append` method uses a "check-then-act" pattern:
```rust
let seq = self.next_seq(&msg).await?; // SELECT MAX(seq)
// ... then INSERT ...
```
Despite the retry loop catching unique constraint violations, the method returns `0` on error. Since `seq` starts at `0`, a return value of `0` is ambiguous (it could mean "success at seq 0" or "failure").
**Impact:**
Under high concurrency, operations may fail silently or be reported as successful when they were not persisted.
**Fix:**
1.  Use a single `INSERT` statement with a `RETURNING seq` clause, relying on the database to generate the sequence (or use `ON CONFLICT DO NOTHING`).
2.  Change the return type to `Result<u64, Error>` to explicitly handle failures.
3.  Remove the ambiguous `0` return value.

### 1.3 TemporalStore Stubs
**Issue:**
The `TemporalStore` is heavily stubbed with `TODO` comments. The actual `temporal-client` integration is commented out or missing.
**Impact:**
The system cannot use Temporal for durable execution, which is a core architectural pillar for long-running workflows.
**Fix:**
1.  Uncomment and fix the `temporal-client` dependency in `Cargo.toml`.
2.  Implement `connect` and `route_to_temporal` using the Temporal SDK.

---

## 2. Scalability & Availability (P1)

### 2.1 In-Memory Fallbacks
**Issue:**
`TokenService`, `ReplayGuard`, and `EventStore` default to in-memory implementations.
**Impact:**
-   **TokenService:** AI agent tokens are lost on restart, breaking in-flight agent operations.
-   **ReplayGuard:** Replay protection is local to the pod. A client reconnecting to a different pod can replay operations.
**Fix:**
Ensure production deployments always set `REDIS_URL` and enable the `redis-broadcast` feature. Validate that `build_token_store` and `build_replay_store` correctly initialize the Redis backends.

### 2.2 WASM Compatibility (`no_std`)
**Issue:**
`policy-core` depends on `std` for CRDT features (`yrs` crate). This prevents compilation to `no_std` WASM targets, which may be required for certain restricted client environments (e.g., blockchain runtimes or strict edge workers).
**Impact:**
Limits the "Isomorphic AST" promise if the client environment cannot support `std`.
**Fix:**
1.  Investigate if `yrs` has a `no_std` feature (unlikely).
2.  Decouple `CrdtDoc` logic from the core evaluator, or gate it behind a `crdt` feature flag that can be disabled for strict `no_std` builds.

---

## 3. Security (P1)

### 3.1 Input Sanitization
**Status:**
-   **AI Prompts:** Implemented in `agent_propose_handler` (`sanitize_prompt_input`, `guardrail_check`).
-   **General Ops:** `Reconciler` relies on `serde_json` deserialization.
**Recommendation:**
Ensure strict type checking and length limits on all string fields in `ClientMessage` payload to prevent memory exhaustion attacks.

### 3.2 RBAC
**Status:**
Implemented in `Reconciler`.
**Recommendation:**
Ensure `RbacRegistry` is loaded from a secure source in production (currently `with_defaults()`).

---

## 4. Remediation Plan (Step-by-Step)

### Phase 1: Persistence & Correctness (Immediate)
1.  **Fix PostgresStore:**
    -   Refactor `append` to return `Result<u64, Error>`.
    -   Fix the race condition using proper SQL locking or `ON CONFLICT`.
2.  **Implement Actor Replay:**
    -   Update `WorkflowActor` to accept a `PersistentStore`.
    -   In `WorkflowActor::new`, read all events for the instance.
    -   Apply them to `Reconciler` state *before* accepting new messages.
3.  **Enable Redis Backends:**
    -   Verify `REDIS_URL` parsing.
    -   Test `RedisTokenStore` and `RedisReplayGuard` with a real Redis instance.

### Phase 2: Temporal Integration (Next Sprint)
1.  **Add `temporal-client` crate:**
    -   Enable the `temporal-production` feature.
    -   Implement the `connect` and `signal` logic in `server/src/temporal_store.rs`.

### Phase 3: Hardening (Pre-Launch)
1.  **WASM / `no_std` Audit:**
    -   Check `policy-core` build without default features.
2.  **Load Testing:**
    -   Run `k6` tests against the `PostgresStore` backend to verify the fix for race conditions.
