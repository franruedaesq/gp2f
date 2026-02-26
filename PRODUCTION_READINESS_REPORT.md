# Production Readiness Report & Remediation Plan

This document evaluates the current "High-Fidelity Prototype" status of the GP2F server and outlines a step-by-step plan to achieve production-grade durability, scalability, and security.

## 1. Executive Summary

The current codebase is a functional prototype with significant components implemented as in-memory stubs or "demo mode" implementations. While the core logic (actor model, policy engine, CRDT reconciliation) is sound, the persistence and distributed coordination layers are currently mocked or incomplete.

To move to production, we must replace these stubs with durable, distributed implementations (Postgres, Temporal, Redis) and harden the security posture.

## 2. Identified Stubs & Gaps

The following components are currently implemented as in-memory stubs or contain explicit `TODO` markers that prevent production use:

| Component | File | Status | Gap |
| :--- | :--- | :--- | :--- |
| **Temporal Store** | `server/src/temporal_store.rs` | **STUB** | `connect()` and `route_to_temporal()` are no-ops. Missing `temporal-client` dependency. |
| **Token Service** | `server/src/token_service.rs` | **In-Memory** | Uses `Mutex<HashMap>` for token storage. No TTL eviction or distributed locking. |
| **Replay Guard** | `server/src/replay_protection.rs` | **In-Memory** | Uses local `HashSet` + Bloom filters. Cannot share state across replicas. |
| **Event Store** | `server/src/event_store.rs` | **In-Memory** | Default store. `PostgresStore` exists but is optional. |
| **Public Key Store** | `server/src/middleware.rs` | **In-Memory** | Defaults to `InMemoryPublicKeyStore` if `KEYS_JSON` is unset. |
| **Actor Registry** | `server/src/actor_registry.rs` | **Local** | Local `Mutex<HashMap>`. No cluster-wide actor addressing or sticky sessions. |

## 3. Remediation Plan

### Phase 1: Durability (Data Safety)

**Goal:** Ensure zero data loss on process restart or crash.

#### Step 1.1: Finalize Postgres Event Store
- **File:** `server/src/postgres_store.rs`
- **Action:**
    - Verify `sqlx` dependency is enabled in `Cargo.toml` (currently optional).
    - Ensure `migrations/` folder contains the `20240522_init.sql` schema and it is applied on startup or via CI/CD.
    - **Optimization:** Configure `PgPoolOptions` for production (max_connections, timeouts) based on expected load.
    - **Verify:** Run integration tests with a real Postgres container to ensure `append` and `events_for` work correctly.

#### Step 1.2: Implement Temporal Store (Production Path)
- **File:** `server/src/temporal_store.rs`
- **Action:**
    - Add `temporal-client` to `Cargo.toml`.
    - Implement `connect()` to actually connect to the Temporal cluster.
    - Implement `route_to_temporal()`:
        - Use `client.signal_workflow_execution()` to send the `ApplyOp` signal.
        - Handle `WorkflowExecutionAlreadyStartedError` by signaling the existing execution.
    - **Validation:** Ensure the Temporal workflow definitions (outside this repo or in a separate worker crate) match the signal payload schema.

### Phase 2: Scalability (Horizontal Scaling)

**Goal:** Allow running multiple server replicas behind a load balancer.

#### Step 2.1: Redis-backed Token Service
- **File:** `server/src/token_service.rs`
- **Action:**
    - Replace `Mutex<HashMap>` with a `RedisTokenStore` struct using the `redis` crate.
    - **Mint:** `SET token:{id} {metadata} EX {ttl} NX`.
    - **Redeem:** Use a Lua script to atomically check, validate hash/op, and `DEL` (or rename to `consumed:{id}`).
    - **Lock:** Use `SET ... NX` with a short TTL for the locking phase.

#### Step 2.2: Distributed Replay Protection
- **File:** `server/src/replay_protection.rs`
- **Action:**
    - The current in-memory Bloom filter is effective but local.
    - **Strategy:**
        - **Option A (Simpler):** Use Redis Sets with TTL for the "recent window" (`SISMEMBER`, `SADD`, `EXPIRE`).
        - **Option B (High Throughput):** Use RedisBloom module (`BF.ADD`, `BF.EXISTS`).
    - **Refactor:** Change `ReplayGuard` to take a `redis::Client` (or connection pool) and perform checks remotely.

#### Step 2.3: Pub/Sub Actor Invalidation (or Sticky Sessions)
- **File:** `server/src/actor_registry.rs`
- **Issue:** If Client A connects to Pod 1 and Client B to Pod 2, they might spawn two different actors for the same instance, leading to split-brain (though optimistic concurrency in DB might catch it, it's inefficient).
- **Action:**
    - **Immediate Fix:** Ensure Load Balancer uses **Sticky Sessions** (based on `tenant_id` or `instance_id` in URL/Cookies).
    - **Long-term Fix:** Use Redis PubSub to broadcast "I am hosting instance X" messages. If another pod receives a request for X, it proxies it or redirects.

### Phase 3: Security (Hardening)

**Goal:** Protect against unauthorized access and malicious inputs.

#### Step 3.1: Production Key Management
- **File:** `server/src/middleware.rs`
- **Action:**
    - Deprecate `InMemoryPublicKeyStore` for production builds.
    - Ensure `KEYS_JSON` is populated from a secure secret store (Kubernetes Secrets, AWS Secrets Manager) at runtime.
    - Implement key rotation support (periodic reload of `KEYS_JSON` or polling a JWKS endpoint).

#### Step 3.2: Enhanced Guardrails & Sanitization
- **File:** `server/src/main.rs`
- **Action:**
    - `guardrail_check` currently uses a simple string matching blocklist.
    - **Improvement:** Integrate a local NLP model (e.g., using `rust-bert` or ONNX) or a dedicated sidecar for prompt injection detection if latency permits.
    - **Sanitization:** `sanitize_prompt_input` is basic. Use a proper library to strip invisible control characters, zero-width spaces, and other homoglyph attacks.

#### Step 3.3: Rate Limiting
- **File:** `server/src/rate_limit.rs` (implied)
- **Action:**
    - Ensure `AiRateLimiter` uses Redis for distributed counting, not local atomic counters.

## 4. Summary of Code Changes Required

1.  **Dependencies**: Add `temporal-client`, `redis` (ensure features enabled), `sqlx` (ensure features enabled).
2.  **`server/src/temporal_store.rs`**: Complete the stub implementation.
3.  **`server/src/token_service.rs`**: Rewrite to use Redis.
4.  **`server/src/replay_protection.rs`**: Rewrite to use Redis.
5.  **`server/src/main.rs`**: Wire up the Redis-backed services instead of the in-memory ones based on `APP_ENV=production` or feature flags.

## 5. Verification Strategy

1.  **Unit Tests**: Update tests in `token_service.rs` and `replay_protection.rs` to use `redis-mock` or a real Redis container via `testcontainers`.
2.  **Integration Tests**: Run the full `tests/integration_test.rs` suite with `docker-compose up` (Postgres + Redis + Temporal) to verify end-to-end durability.
3.  **Load Testing**: Use `k6` to simulate concurrent access to the same workflow instance across multiple server processes (if possible locally) to verify locking and replay protection.
