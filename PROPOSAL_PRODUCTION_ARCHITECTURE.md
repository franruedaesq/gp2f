# Proposal: Production Architecture & Hardening

This document proposes a production architecture and outlines the critical hardening steps required to safely deploy GP2F.

## 1. Target Architecture

The recommended production architecture for GP2F is:

-   **Event Store:** PostgreSQL (`gp2f-store/postgres_store.rs`).
    -   Provides strong durability via `pg_advisory_xact_lock` for strict serialization.
    -   Supports point-in-time recovery and existing backup tooling.
-   **Actor Coordination:** Redis (`gp2f-actor/actor.rs`).
    -   Uses distributed locks to ensure only one active actor per workflow instance.
    -   Must fail-closed to prevent split-brain scenarios.
-   **Replay Protection:** Redis (`gp2f-security/replay_protection.rs`).
    -   Uses shared state to detect duplicate `op_id` submissions across the cluster.
    -   Must fail-closed to prevent replay attacks during outages.
-   **Temporal Integration:** Deferred until fully implemented.
    -   The current implementation lacks `events_for` (history fetch), risking data loss.

## 2. Hardening Strategy: "Fail-Closed"

The primary reliability issue identified in the codebase is "fail-open" logic in critical security and coordination paths. In production (`APP_ENV=production`), the system must prioritize **Safety over Availability**.

### 2.1 Actor Coordination (Split-Brain Prevention)

**Current Behavior:**
-   `ActorRegistry::get_or_spawn` attempts to acquire a Redis lock.
-   If Redis is unreachable, it logs a warning and **spawns the actor locally**.
-   **Risk:** Multiple pods serving the same instance, leading to divergent state.

**Proposed Change:**
-   Modify `gp2f-actor/src/actor.rs` to check `APP_ENV`.
-   If `APP_ENV=production` and `try_claim` fails (Redis error), return a `SplitBrainError` or a new `CoordinationError`.
-   Do **not** spawn a local actor. The request will fail (503), but data integrity is preserved.

### 2.2 Replay Protection (Security)

**Current Behavior:**
-   `RedisReplayGuard::check_and_insert` attempts to check Redis for duplicates.
-   If Redis is unreachable, it logs a warning and returns `false` (not a duplicate).
-   **Risk:** An attacker could exploit a Redis outage to replay financial or state-changing operations.

**Proposed Change:**
-   Modify `gp2f-security/src/replay_protection.rs` to check `APP_ENV`.
-   If `APP_ENV=production` and Redis fails, return `true` (treat as duplicate) to **block** the request.
-   The user receives a rejection, but the system is secure against replays.

### 2.3 Temporal Safety

**Current Behavior:**
-   `TemporalStore::events_for` falls back to an empty in-memory store.
-   **Risk:** Silent data loss on pod restart if configured to use Temporal.

**Proposed Change:**
-   Modify `gp2f-store/src/temporal_store.rs` to panic or error loudly if `events_for` is called when `temporal-production` feature is active.
-   This forces operators to acknowledge the limitation or implement the missing functionality before deployment.

## 3. Implementation Plan

1.  **Actor Update:** Modify `gp2f-actor` to respect `APP_ENV`.
2.  **Security Update:** Modify `gp2f-security` to respect `APP_ENV`.
3.  **Store Safety:** Add panic guard to `gp2f-store` (Temporal).
4.  **Verification:** Verify behavior with unit tests simulating production mode.
