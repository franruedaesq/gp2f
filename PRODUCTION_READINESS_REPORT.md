# Production Readiness Report

**Date:** 2024-05-23
**Author:** Staff Software Architect
**Status:** DRAFT

## Executive Summary

The `gp2f` system, in its current state, is an advanced Proof-of-Concept (PoC) suitable for demos but **critically unsafe for production deployment**. While the architectural vision is sound (event sourcing, actor model, policy-as-code), the implementation relies heavily on in-memory structures, stubbed persistence layers, and non-atomic database operations that guarantee data loss and corruption under load.

**Key Findings:**
- **Data Durability (P0):** The primary persistence mechanism (`PostgresStore`) contains a race condition that allows duplicate sequence numbers and silently swallows insertion errors, leading to immediate data corruption.
- **Scalability (P1):** The system is currently single-node only. Distributed coordination (Redis) is implemented but feature-gated and not default.
- **Security (P1):** Input sanitization and guardrails are present for LLM interactions but rely on local rule-based heuristics. Auth token storage is ephemeral by default.
- **WASM Portability (P2):** The `policy-core` library depends on `std`, preventing deterministic execution in restricted WASM environments.

---

## 1. Critical Gaps & Remediation (P0)

### 1.1. `PostgresStore` Race Condition & Error Swallowing
**Location:** `server/src/postgres_store.rs`

The current implementation of `append` is catastrophic:
1.  **Race Condition:** It calculates `seq` using `SELECT MAX(seq) + 1` and then performs an `INSERT` in a separate round-trip without a transaction or locking. Concurrent requests for the same instance will generate the same `seq`, violating the append-only log invariant.
2.  **Silent Failure:** The `INSERT` operation is wrapped in a block that catches errors, logs them, and *continues execution returning the invalid sequence number*. The caller assumes success while data is discarded.

**Remediation Plan:**
- **Immediate Fix:** Rewrite `append` to use a single atomic `INSERT ... RETURNING seq` statement relying on a database sequence or use `SERIALIZABLE` transaction isolation.
- **Better Fix:** Use Optimistic Concurrency Control (OCC) by adding a unique constraint on `(tenant_id, workflow_id, instance_id, seq)` and retrying on violation.

### 1.2. Stubbed `TemporalStore`
**Location:** `server/src/temporal_store.rs`

The `TemporalStore` is designed to provide durable workflow execution but is currently stubbed. The `temporal-production` feature flag exists, but the actual client connection and signal logic are commented out with `TODO`.

**Remediation Plan:**
- Uncomment and implement the `temporal-client` integration.
- Ensure the `ApplyOp` signal payload matches the Temporal workflow definition.
- Configure the Temporal namespace and retention policies as documented.

---

## 2. Scalability & Infrastructure (P1)

### 2.1. In-Memory State & Distributed Locking
**Locations:**
- `server/src/token_service.rs`
- `server/src/replay_protection.rs`
- `server/src/actor_registry.rs`

By default, these services use local `Mutex<HashMap<...>>`. This means:
- **No Horizontal Scaling:** A second server instance will have its own isolated state, leading to split-brain scenarios (e.g., a token redeemed on node A is still valid on node B).
- **Data Loss on Restart:** All active tokens and replay protection windows are lost on process restart.

**Remediation Plan:**
- **Enable Redis:** The code contains production-ready `RedisTokenStore` and `RedisReplayGuard` implementations. These must be enabled by:
    1.  Setting the `redis-broadcast` Cargo feature.
    2.  Providing a `REDIS_URL` environment variable.
- **Verify Lua Scripts:** The implemented Lua scripts for atomic "check-and-set" operations (e.g., `REDEEM_LUA`) appear correct but must be load-tested against a real Redis cluster.

### 2.2. Actor Registry Coordination
**Location:** `server/src/actor_registry.rs`

The `ActorRegistry` spawns local actors. Without the `RedisActorCoordinator` (guarded by `redis-broadcast`), multiple nodes might spawn duplicate actors for the same `instance_id`, processing events concurrently and violating the serialized execution model.

**Remediation Plan:**
- Enable `redis-broadcast`.
- Verify the `RedisActorCoordinator` correctly handles actor placement and "lock acquisition" for instances.

---

## 3. Security Audit (P1)

### 3.1. Input Sanitization & Guardrails
**Location:** `server/src/main.rs`

- **LLM Inputs:** `agent_propose_handler` correctly implements `sanitize_prompt_input` (stripping control chars) and `guardrail_check` (rejecting jailbreaks). This is good.
- **Op Payloads:** `op_handler` and `ai_propose_handler` rely on the `policy-core` WASM engine to validate business logic. However, the *structure* of the JSON payload is trusted once signed.
- **Signatures:** `OpIdLayer` verifies Ed25519 signatures on `op_id`. This effectively authenticates the request source.

**Remediation Plan:**
- **Audit `OpIdLayer`:** Ensure it is applied to *all* state-mutating routes (`/op`, `/token/mint`, etc.). Currently, it is applied globally to the `Router`, which is correct.
- **Secret Management:** Move `KEYS_JSON` and `REDIS_URL` to a secure secret store (e.g., Vault, AWS Secrets Manager) and inject them at runtime, rather than baking them into env vars if possible.

---

## 4. WASM & Core Logic (P2)

### 4.1. `std` Dependency in `policy-core`
**Location:** `policy-core/Cargo.toml`

The `policy-core` crate has `default = ["std"]` and depends on `chrono` and `yrs`.
- **Impact:** This makes the policy engine harder to run in restricted WASM environments (e.g., edge workers, browser) where `std` is unavailable or where determinism is strict (no wall-clock time).
- **Remediation:** Refactor `policy-core` to be `no_std` by default, using `alloc` for dynamic types and pushing time/randomness concerns to the caller (dependency injection).

---

## 5. Step-by-Step Implementation Plan

### Phase 1: Durability (Immediate)
1.  **Fix `PostgresStore`**:
    - Modify `server/src/postgres_store.rs` to use a transaction or atomic insert.
    - **MUST FIX:** Remove the error swallowing `if let Err(e) ...`. Propagate errors to the caller so the API returns 500 instead of 200 OK.
2.  **Enable Temporal**:
    - Add `temporal-client` to `server/Cargo.toml`.
    - Uncomment implementation in `server/src/temporal_store.rs`.

### Phase 2: Scalability (Pre-Deploy)
3.  **Infrastructure Config**:
    - Update `Dockerfile` or `helm/` charts to ensure `REDIS_URL` and `DATABASE_URL` are mandatory.
4.  **Feature Flags**:
    - Update build pipeline to compile `server` with `--features "redis-broadcast postgres-store temporal-production"`.
    - Verify `RedisActorCoordinator` startup logs.

### Phase 3: Hardening (Post-Deploy)
5.  **Load Testing**:
    - Run `k6` tests against the `PostgresStore` fix to verify no duplicate sequence numbers under high concurrency.
6.  **Chaos Testing**:
    - Kill random nodes to verify `RedisReplayGuard` prevents operation replay.

## Conclusion

The codebase contains the *skeletons* of a production system but is currently wired for a developer demo. **Do not deploy to production** without addressing the `PostgresStore` data corruption bug (Section 1.1) and enabling the distributed state stores (Section 2.1).
