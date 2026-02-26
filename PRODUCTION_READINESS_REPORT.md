# Production Readiness Report

**Date:** 2024-05-24
**Evaluator:** Staff Software Architect
**Scope:** Durability, Scalability, Security

## Executive Summary

The GP2F framework is currently **NOT Ready for Production**.

While the core architecture is sound (Actor Model, Event Sourcing, Wasm Policy Engine), several critical components rely on in-memory implementations or lack necessary database constraints for data integrity. The system is currently suitable for **Single-Node Demos** or **Non-Critical Prototypes**, but deploying it as-is to a multi-replica production environment will lead to data loss, split-brain scenarios (if Redis is down), and broken idempotency.

### Usability Verdict
*   **Demo / Local Dev:** ✅ **Usable.** The system works out-of-the-box with in-memory stores.
*   **Production (Multi-Node):** ❌ **NOT Usable.** Critical gaps in persistence and idempotency must be addressed first.

---

## 1. Durability

### Critical Findings

1.  **Missing Idempotency Constraint (P0):**
    The PostgreSQL schema (`migrations/20240523_auto_seq.sql`) defines `seq` as the primary key but **does not enforce uniqueness on `op_id`**.
    *   *Impact:* If a client retries an operation (e.g., due to a network timeout), the database will accept the duplicate `op_id` with a new sequence number. The application logic in `PostgresStore::append` expects a unique constraint violation (Error 23505) to detect conflicts, but the database will never raise it.
    *   *Result:* Duplicate events in the log, breaking the "exactly-once" processing guarantee.

2.  **Temporal Storage Stubbed (P1):**
    The `TemporalStore` implementation is a stub. The `temporal-client` dependency is commented out in `Cargo.toml`, and the code performs a TCP probe instead of a real gRPC connection.
    *   *Impact:* Users attempting to use Temporal for durability will find it non-functional or falling back to in-memory storage without warning (if they don't read the logs).

3.  **Token Storage Persistence:**
    The `TokenService` defaults to in-memory storage unless `redis-broadcast` is enabled *and* `REDIS_URL` is set.
    *   *Impact:* Tokens are lost on restart, potentially causing workflow failures if a token was issued but not yet redeemed.

### Remediation Plan

#### Step 1: Fix Idempotency (Postgres)
Create a new migration file `migrations/20240525_unique_op_id.sql` to enforce uniqueness on `op_id` scoped to the partition.

```sql
-- Enforce idempotency: prevent duplicate op_ids within the same partition.
CREATE UNIQUE INDEX CONCURRENTLY idx_event_log_op_id_unique
    ON event_log (tenant_id, workflow_id, instance_id, op_id);
```

#### Step 2: Enable Temporal (If required)
If Temporal is the chosen backend:
1.  Uncomment `temporal-client` in `server/Cargo.toml`.
2.  Implement the `TODO` blocks in `gp2f-store/src/temporal_store.rs` to use the real SDK clients (`WorkflowClient`, `RetryClient`).

---

## 2. Scalability

### Critical Findings

1.  **Split-Brain Protection (P1):**
    The `ActorRegistry` correctly uses `RedisActorCoordinator` to acquire a distributed lock (`SET NX`) before spawning a local actor. This is a robust design.
    *   *Risk:* If Redis is unavailable, the system **fails open** and spawns a local actor anyway (logging a warning). In a network partition, this could lead to two pods processing the same workflow instance independently, diverging state.

2.  **Redis Dependency:**
    Multi-node scaling **strictly requires** Redis. The `redis-broadcast` feature must be enabled. Without it, WebSocket notifications (`Accept`/`Reject`) will not reach clients connected to other pods.

### Remediation Plan

#### Step 1: Configure Redis for Production
Ensure `REDIS_URL` is set in the production environment.

#### Step 2: Review Fail-Open Logic
Decide if "availability" or "consistency" is more important.
*   *Current (Availability):* If Redis is down, spawn anyway. Risk: Data corruption.
*   *Recommended (Consistency):* If Redis is down, **refuse to spawn** the actor. Return 503 Service Unavailable. This prevents split-brain.

---

## 3. Security

### Critical Findings

1.  **Nonce Replay Protection (P1):**
    The `NonceStore` (used by `OpIdLayer` middleware) is **in-memory** (`HashSet` + Bloom Filter).
    *   *Impact:* If a pod restarts, its replay history is lost. An attacker could replay a captured request (valid for 30s) immediately after a restart, and it would be accepted.
    *   *Fix:* The `NonceStore` must be backed by Redis (with 30s TTL on keys) to survive restarts.

2.  **Sanitization:**
    The `SanitizeLayer` strips control characters and "invisible unicode". This is a good baseline but insufficient for deep defense against Prompt Injection.
    *   *Gap:* The `guardrail_check` in `agent_propose_handler` is a simple keyword blocklist. It does not use a semantic model (e.g., Llama Guard) to detect sophisticated attacks.

3.  **Key Management:**
    The `OpIdLayer` relies on `KEYS_JSON` env var or `KEYS_POLL_INTERVAL_SECS`.
    *   *Good:* It refuses to start in production (`APP_ENV=production`) without keys.
    *   *Gap:* Rotation requires updating the env var (and waiting for the poller). Ensure the secrets infrastructure supports this.

### Remediation Plan

#### Step 1: Redis-Backed Nonce Store
Refactor `server/src/middleware.rs` to use Redis for checking and storing nonces when `redis-broadcast` is enabled.

#### Step 2: Enhanced Guardrails
Integrate a dedicated "Safety Classifier" (e.g., a small ONNX model or external API) in `agent_propose_handler` before calling the LLM.

---

## Implementation Roadmap (Step-by-Step)

1.  **Database Migration:** Apply the unique index on `op_id`. (Duration: < 1 hour)
2.  **Persistence Hardening:** Switch `ActorRegistry` to "Fail-Closed" on Redis error. (Duration: 2 hours)
3.  **Security Hardening:** Implement Redis-backed `NonceStore`. (Duration: 4 hours)
4.  **Temporal Integration:** (Optional) Finish the SDK implementation. (Duration: 2 days)
