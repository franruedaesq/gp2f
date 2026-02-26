# Production Readiness Report

**Date:** 2024-05-24
**Author:** Staff Software Architect
**Status:** **NOT READY FOR PRODUCTION** (Demo/Alpha Quality)

## Executive Summary

The current codebase is in a "Demo Mode" state. While the core architecture (Actor Model, CRDTs, Policy Engine) is sound, the critical infrastructure required for data durability, horizontal scalability, and production security is either stubbed out, disabled by default, or incomplete.

**The system is NOT usable for production workloads in its current state.** Using it as-is will result in data loss (in-memory storage), split-brain scenarios (missing distributed locks), and potential security vulnerabilities (weak key management).

## Critical Gaps (P0)

### 1. Persistence & Durability (Critical)
*   **Temporal Store is a Stub:** The `TemporalStore` implementation in `gp2f-store/src/temporal_store.rs` contains commented-out code for the `temporal-client` integration. It forces a fallback to in-memory storage, meaning **all workflow history is lost on restart** if Temporal is the intended backend.
*   **Postgres Store Fragility:** The `PostgresStore` implementation swallows serialization errors, returning them as loose strings rather than structured errors.
*   **Default In-Memory Fallback:** The server defaults to `InMemoryStore` if `DATABASE_URL` or `TEMPORAL_ENDPOINT` are missing or if connections fail. In production, the service should crash-loop rather than silently failing over to ephemeral storage.

### 2. Scalability (High)
*   **Redis Dependency for Actors:** The `ActorRegistry` correctly implements a split-brain protection mechanism (`get_or_spawn` claims a Redis lock). However, this relies entirely on the `redis-broadcast` feature flag and the `REDIS_URL` environment variable. Without this, the system cannot be scaled horizontally as multiple pods would spawn independent actors for the same instance.
*   **Async Ingestion:** The async ingestion queue is present but its reliability depends on the underlying persistence which, as noted above, is currently compromised.

### 3. Security (High)
*   **Ephemeral Key Management:** The `main.rs` configuration defaults to `InMemoryPublicKeyStore` if `KEYS_POLL_INTERVAL_SECS` or `KEYS_JSON` are not set. This generates a new keypair on every restart, invalidating all previous client signatures.
*   **Input Sanitization:** While `sanitize_prompt_input` exists, there is no global middleware for request sanitization. It is applied ad-hoc in handlers.

## Detailed Audit

### Durability & Persistence

| Component | Status | Issues |
| :--- | :--- | :--- |
| **EventStore** | ❌ Toy | Pure in-memory `HashMap`. Suitable for unit tests only. |
| **PostgresStore** | ⚠️ Risky | Implemented but lacks robust error handling. Uses `pg_advisory_xact_lock` correctly for serialization. |
| **TemporalStore** | ❌ Broken | **Code is commented out.** It is a non-functional stub waiting for `temporal-client` crate. |

**Code Reference (`gp2f-store/src/temporal_store.rs`):**
```rust
// TODO(temporal-production): uncomment once temporal-client is added:
// let opts = temporal_client::ClientOptions::default() ...
return Err(TemporalError::Connection("temporal-production feature is enabled but...".into()));
```

### Scalability & Concurrency

| Component | Status | Issues |
| :--- | :--- | :--- |
| **ActorRegistry** | ✅ Good | `RedisActorCoordinator` prevents split-brain. Requires `redis-broadcast` feature. |
| **ReplayGuard** | ✅ Good | `RedisReplayGuard` uses Lua scripts for atomic sliding windows. |
| **TokenService** | ✅ Good | `RedisTokenStore` uses Lua scripts for atomic mint/redeem. |

**Observation:** The distributed logic is sound, but it is strictly gated behind the `redis-broadcast` feature. The deployment pipeline must ensure this feature is enabled in the release build.

### Security

| Component | Status | Issues |
| :--- | :--- | :--- |
| **Input Validation** | ⚠️ Mixed | `sanitize_prompt_input` handles control chars and invisible unicode, but is manually called in handlers. |
| **Auth/Signatures** | ⚠️ Risky | Defaults to `InMemoryPublicKeyStore` (ephemeral). Production must force `PollingKeyProvider` or `EnvVarKeyProvider`. |
| **Replay Protection** | ✅ Good | `RedisReplayGuard` implementation correctly handles sliding windows to prevent replay attacks. |

## Remediation Plan

To move from "Demo Mode" to "Production Ready", the following steps must be executed in order:

### Step 1: Fix Persistence (The "Data Loss" Blocker)
1.  **Enable Temporal Client:**
    *   Add `temporal-client` to `server/Cargo.toml`.
    *   Uncomment the connection and signaling logic in `gp2f-store/src/temporal_store.rs`.
    *   Verify `temporal-production` feature compiles and connects to a real Temporal cluster.
2.  **Harden Postgres Store:**
    *   Refactor `gp2f-store/src/postgres_store.rs` to return structured errors (e.g., `PersistenceError::Serialization`, `PersistenceError::Conflict`) instead of strings.
    *   Ensure the `RETURNING seq` relies on a gapless sequence generator or that the application can handle gaps.
3.  **Disable Silent Fallbacks:**
    *   Modify `server/src/main.rs` to **panic** at startup if `DATABASE_URL` or `TEMPORAL_ENDPOINT` is provided but connection fails. Do not fall back to `InMemoryStore` in a production profile.

### Step 2: Enable Distributed Coordination
1.  **Enforce Redis:**
    *   Ensure the Docker build enables `--features redis-broadcast`.
    *   Update `server/src/main.rs` to fail startup if `REDIS_URL` is missing when in production mode.
2.  **Verify Locking:**
    *   Run a load test with 2+ replicas to verify `ActorRegistry` correctly rejects split-brain spawns.

### Step 3: Security Hardening
1.  **Force Persistent Keys:**
    *   In `server/src/main.rs`, remove the fallback to `InMemoryPublicKeyStore` when the environment is `production`. Require `KEYS_JSON` or `KEYS_POLL_INTERVAL_SECS`.
2.  **Global Sanitization:**
    *   Move `client_msg.sanitize()` into a `Tower` middleware layer to ensure it runs for *all* ingress requests, not just those manually instrumented in handlers.

### Step 4: Observability
1.  **Structured Logging:**
    *   Ensure `LOG_FORMAT=json` is set in the production environment variables.
    *   Verify `tracing` spans correctly propagate `trace_id` through the `Actor` -> `Reconciler` -> `Persistence` call chain.

## Conclusion

The software architecturally supports production requirements but is currently implemented as a prototype. **Do not deploy to production until Step 1 and Step 2 of the Remediation Plan are complete.**
