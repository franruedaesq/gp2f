# Production Readiness Report

**Status:** 🟡 **PARTIALLY USABLE** (with Postgres) / 🔴 **NOT READY** (with Temporal)

This report evaluates the durability, scalability, and security features of the GP2F framework for production deployment. While the core logic is sound, critical infrastructure components (Actor Coordination, Replay Protection, Temporal Integration) currently "fail open" or lack implementation, posing severe risks in a production environment.

## 1. Executive Summary

| Feature | Status | Risk Level | Notes |
| :--- | :--- | :--- | :--- |
| **Durability (Postgres)** | ✅ Ready | Low | `postgres-store` is implemented and verified. |
| **Durability (Temporal)** | 🔴 Critical | **High** | `events_for` is not implemented; relies on in-memory fallback. **Data loss on restart.** |
| **Scalability (Actors)** | 🟡 Risky | **High** | Redis coordinator fails open (spawns local actor) on Redis error. **Split-brain risk.** |
| **Security (Replay)** | 🟡 Risky | **High** | Redis replay guard fails open (allows op) on Redis error. **Replay attack risk.** |
| **Security (RBAC)** | ✅ Ready | Low | RBAC guards and role hierarchy are implemented. |
| **Input Sanitization** | ✅ Ready | Low | Guardrails and sanitization present in ingestion pipeline. |

---

## 2. Deep Dive: Durability

### ✅ Postgres Store (`gp2f-store/src/postgres_store.rs`)
The Postgres implementation is production-ready.
- Uses `pg_advisory_xact_lock` for strict serialization of events per instance.
- Persists all events to `event_log` table.
- `events_for` correctly retrieves history for replay.

### 🔴 Temporal Store (`gp2f-store/src/temporal_store.rs`)
The Temporal implementation is **incomplete and unsafe for production**.
- **Issue:** The `events_for` method (required for actor state recovery) is not implemented for the Temporal backend.
- **Behavior:** It logs a debug message and falls back to `self.fallback.events_for(key)`, which is an in-memory store.
- **Consequence:** When a pod restarts, the in-memory fallback is empty. The actor will recover with **zero state**, leading to data loss and invariant violations.

## 3. Deep Dive: Scalability & Coordination

### 🟡 Actor Coordination (`gp2f-actor/src/actor.rs`)
The `RedisActorCoordinator` prevents multiple pods from spawning actors for the same workflow instance (Split-Brain protection).
- **Issue:** The `get_or_spawn` method "fails open". If Redis is unreachable, it logs a warning and spawns the actor locally anyway.
- **Risk:** In a network partition or Redis outage, multiple pods may spawn authoritative actors for the same instance. This leads to **split-brain**, where different clients see different states and write conflicting events to the store.
- **Fix Required:** In `APP_ENV=production`, this must "fail closed" (return error 503) if the coordination lock cannot be acquired.

## 4. Deep Dive: Security

### 🟡 Replay Protection (`gp2f-security/src/replay_protection.rs`)
The `RedisReplayGuard` ensures an `op_id` is processed only once cluster-wide.
- **Issue:** The `check_and_insert` method "fails open". If Redis is unreachable, it logs a warning and returns `false` (not a duplicate), allowing the operation to proceed.
- **Risk:** An attacker could exploit a Redis outage (or trigger one) to replay old operations (e.g., "transfer funds") which would be accepted by the system.
- **Fix Required:** In `APP_ENV=production`, this must "fail closed" (return `true` / block request) if the replay check cannot be performed.

### ✅ Input Sanitization & RBAC
- **Sanitization:** `server/src/main.rs` implements `sanitize_prompt_input` (strips control chars, invisible unicode) and `guardrail_check` (blocks jailbreaks).
- **RBAC:** `gp2f-security/src/rbac.rs` correctly enforces role-based access to workflows and activities.

---

## 5. Recommendations for Production

1.  **Immediate Fixes (P0):**
    - Modify `gp2f-actor` to **fail closed** on Redis errors when `APP_ENV=production`.
    - Modify `gp2f-security` to **fail closed** (deny op) on Redis errors when `APP_ENV=production`.
    - Modify `gp2f-store` (Temporal) to **panic/error** on `events_for` access until implemented.

2.  **Architecture:**
    - Use **Postgres** for the Event Store (`DATABASE_URL`).
    - Use **Redis** for Actor Coordination and Replay Protection (`REDIS_URL`).
    - **Do not use Temporal** for storage until `events_for` is implemented.

3.  **Deployment:**
    - Ensure `APP_ENV=production` is set in the deployment environment.
    - Ensure `REDIS_URL` and `DATABASE_URL` are strictly configured.
