# Production Readiness Report & Remediation Plan

**Date:** 2024-05-23
**Author:** Staff Software Architect
**Status:** **NOT PRODUCTION READY** (Critical P0 Blockers Identified)

## 1. Executive Summary

The `gp2f` system, while demonstrating a strong local-first architecture with CRDTs and event sourcing, is currently in a **Demo / Prototype state**. It lacks essential guarantees for data durability, cluster consistency, and security required for a production deployment.

Critical flaws include:
1.  **Split-Brain Risk:** The `ActorRegistry` allows multiple pods to host the same workflow instance simultaneously, leading to divergent state and data corruption.
2.  **Persistence Race Conditions:** The Postgres storage implementation lacks optimistic concurrency control, allowing parallel writes to corrupt the event log sequence.
3.  **Memory Leaks:** The Redis-backed Replay Guard implementation causes unbounded memory growth.
4.  **Security Gaps:** Missing global input sanitization and rely on in-memory keys for authentication.

## 2. Critical Blockers (P0)

### 2.1. Split-Brain in Actor Registry (Scalability & Integrity)
**Location:** `server/src/actor.rs`

The `ActorRegistry::get_or_spawn` method spawns a local actor *before* attempting to claim the distributed lock via `RedisActorCoordinator`. If the lock claim fails (because another pod owns the instance), the local actor continues running.

**Impact:**
- Multiple pods can accept commands for the same instance.
- They will generate conflicting events with the same sequence numbers (if using `InMemoryStore`) or interleave events without causal consistency (if using `PostgresStore`).
- **Result:** Permanent data corruption and state divergence.

### 2.2. Race Condition in Postgres Store (Durability)
**Location:** `server/src/postgres_store.rs`

The `append` method performs a blind `INSERT` without checking the expected sequence number or the last event's hash.

```rust
// Current Implementation
sqlx::query("INSERT INTO event_log ... RETURNING seq")
```

**Impact:**
- If two actors (due to split-brain) write to the same instance log, they will both succeed.
- The `seq` numbers will be unique (due to `BIGSERIAL`), but the *causal chain* is broken.
- **Result:** The event log becomes a soup of interleaved events from different timelines, making replay impossible or incorrect.

### 2.3. Memory Leak in Redis Replay Guard (Scalability)
**Location:** `server/src/replay_protection.rs`

The Redis Lua script refreshes the TTL of the *Set key* (`expire key ttl`) but adds new members indefinitely.

```lua
-- Current Implementation
redis.call('SADD', key, op_id)
redis.call('EXPIRE', key, ttl)
```

**Impact:**
- `key` (the set for a client) never expires as long as the client is active.
- The set grows unbounded (every op_id ever sent).
- **Result:** Redis eventually runs out of memory (OOM), crashing the entire cluster.

## 3. High Priority Improvements (P1)

### 3.1. Missing Input Sanitization (Security)
**Location:** `server/src/main.rs`

Sanitization (`sanitize_prompt_input`) and guardrails (`guardrail_check`) are only applied to the `/agent/propose` endpoint. The main `/op` (HTTP) and `/ws` (WebSocket) endpoints deserialize JSON payloads directly into the engine without sanitization.

**Impact:**
- Malicious payloads (XSS vectors, huge strings, control characters) are stored permanently in the event log.
- Clients replaying these events will execute the malicious payloads.

### 3.2. Stubbed Temporal Integration (Durability)
**Location:** `server/src/temporal_store.rs`

The `TemporalStore` is a stub with `TODO` comments. Real-world long-running workflows (timers, external signals) cannot be orchestrated reliably without this.

### 3.3. Ephemeral Keys (Security)
**Location:** `server/src/main.rs`

Default configuration uses `InMemoryPublicKeyStore`. Production requires `polling` or `json` key provider to be enforced, with key rotation support.

## 4. Remediation Plan (Step-by-Step)

To move to production, the following steps must be executed in order.

### Step 1: Fix Persistence Integrity (Postgres)

**Goal:** Prevent concurrent writes and ensure causal history.

1.  **Modify Schema:** Ensure `event_log` has a composite unique constraint on `(tenant_id, workflow_id, instance_id, seq)`.
2.  **Update `append` Signature:** Change `PersistentStore::append` to accept `expected_seq`.
3.  **Implement Optimistic Locking:**
    - In `PostgresStore::append`, use a transaction.
    - `SELECT MAX(seq) FROM event_log WHERE ...`
    - Verify `max_seq == expected_seq`.
    - `INSERT ...`
    - Commit.
    - *Alternative (Better):* Use a conditional insert or a stored procedure to do this atomically.

### Step 2: Enforce Cluster Singularity (Actor Registry)

**Goal:** Ensure only one pod hosts an actor at a time.

1.  **Update `RedisActorCoordinator`:**
    - Change `claim_and_announce` to return a `Result<bool, Error>`.
2.  **Update `ActorRegistry::get_or_spawn`:**
    - **Before spawning:** Attempt to `claim` the instance in Redis.
    - **If claim fails:**
        - Check who owns it.
        - **If owning pod is ALIVE:** Return an error (HTTP 429 / 503) or redirect (HTTP 307) to the owning pod.
        - **If owning pod is DEAD (TTL expired):** Steal the lock and spawn locally.
    - **If claim succeeds:** Spawn the actor.

### Step 3: Fix Replay Guard Memory Leak

**Goal:** Prevent Redis OOM.

1.  **Switch to Rolling Windows:**
    - Instead of one Set `replay:{client_id}`, use time-bucketed sets: `replay:{client_id}:{minute_epoch}`.
    - `SADD` to current bucket.
    - Check membership in `current` and `prev` buckets.
    - Set fixed TTL (e.g., 2 hours) on each bucket.
    - This creates a sliding window without unbounded growth.

### Step 4: Implement Defense-in-Depth

1.  **Global Sanitization Middleware:**
    - Implement an Axum middleware that scans all incoming JSON bodies.
    - Recursively walk JSON objects.
    - Strip control characters and enforce max string length (e.g., 64KB) on all string values.
2.  **Verify `op_id` format:** Enforce the `version|tenant|...` structure if expected.

### Step 5: Activate Durable Workflows (Temporal)

1.  **Uncomment `temporal-client`** in `Cargo.toml`.
2.  **Implement `TemporalStore`:**
    - Map `append` to `client.signal_workflow_execution`.
    - Use Temporal Signals to push events into the Workflow history.
    - This provides durability + timers + retries out of the box.

## 5. Conclusion

The system requires significant re-engineering of the **Persistence** and **Coordination** layers before it can be safely deployed. Addressing the Split-Brain and Data Corruption risks (P0) is mandatory.
