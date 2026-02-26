# Production Readiness Plan: Moving `gp2f` from POC to Production

## Executive Summary

The current `gp2f` codebase is a high-quality Proof of Concept (POC) demonstrating a local-first, policy-driven workflow engine. However, it is **critically unfit for production** in its current state due to total data loss on restart (in-memory storage), inability to scale horizontally (local mutexes), and ephemeral security controls.

This document outlines the step-by-step architectural changes required to achieve **Durability**, **Scalability**, and **Security** for a production deployment.

---

## 1. Durability (Critical Priority)

**Current Status:**
- `EventStore` is an in-memory `HashMap`. All workflow history is lost on process restart.
- `ActorRegistry` defaults to `InMemoryStore`.
- `TemporalStore` exists in the codebase but contains stub implementations for `connect()` and `append()`.

### 1.1 Immediate Fix: Implement `PostgresStore`
While Temporal is the long-term goal for durable execution, a direct Postgres backing for the event log provides immediate durability with lower operational complexity for the MVP.

**Step 1: Define the Schema**
Create a migration (e.g., `migrations/20240522_init.sql`) for the event log:

```sql
CREATE TABLE event_log (
    tenant_id TEXT NOT NULL,
    workflow_id TEXT NOT NULL,
    instance_id TEXT NOT NULL,
    seq BIGINT NOT NULL,
    op_id TEXT NOT NULL,
    ingested_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    hlc_ts BIGINT NOT NULL,
    outcome TEXT NOT NULL, -- 'ACCEPTED' or 'REJECTED'
    payload JSONB NOT NULL,
    PRIMARY KEY (tenant_id, workflow_id, instance_id, seq)
);

-- Index for efficient replay
CREATE INDEX idx_event_log_replay
ON event_log (tenant_id, workflow_id, instance_id, seq ASC);
```

**Step 2: Implement `PersistentStore` for Postgres**
In `server/src/postgres_store.rs`, implement the `PersistentStore` trait using `sqlx`:

```rust
// Pseudo-code implementation
#[async_trait]
impl PersistentStore for PostgresStore {
    async fn append(&self, msg: ClientMessage, outcome: OpOutcome) -> u64 {
        let seq = self.get_next_seq(&msg).await?;
        sqlx::query!(
            "INSERT INTO event_log (tenant_id, workflow_id, instance_id, seq, op_id, outcome, payload) VALUES ($1, $2, $3, $4, $5, $6, $7)",
            msg.tenant_id, msg.workflow_id, msg.instance_id, seq, msg.op_id, outcome.as_str(), msg.payload
        ).execute(&self.pool).await?;
        Ok(seq)
    }
}
```

**Step 3: Wire into `main.rs`**
Replace the default `InMemoryStore` initialization:

```rust
let store: Arc<dyn PersistentStore> = if let Ok(url) = std::env::var("DATABASE_URL") {
    Arc::new(PostgresStore::new(&url).await?)
} else {
    // Fallback or Temporal path
    ...
};
```

---

## 2. Scalability (High Priority)

**Current Status:**
- **Actor State**: Local `Mutex<HashMap>`. If you run two server replicas, they will have split-brain actors for the same workflow instance.
- **Token Service**: In-memory `HashMap`. Tokens minted on Node A cannot be redeemed on Node B.
- **Broadcast**: Redis PubSub is implemented (`server/src/redis_broadcast.rs`) but hidden behind a feature flag.

### 2.1 Distributed Actor Placement
To run multiple replicas, we must ensure requests for a specific `workflow_instance` go to the same node, OR that nodes coordinate state.

**Option A: Consistent Hashing (Ring Pop) - Recommended for MVP**
Use a consistent hashing load balancer (e.g., HAProxy or Envoy) to route WebSocket connections based on `instance_id`.
- **Pros**: Zero code changes to `ActorRegistry`.
- **Cons**: Resharding (adding nodes) disrupts connections.

**Option B: Redis-Coordinated Registry**
Modify `ActorRegistry` to use Redis to "lease" an actor.
1. When `get_or_spawn` is called, try to acquire a lock in Redis: `SET actor:{id} {node_id} NX EX 300`.
2. If locked by another node, forward the request (gRPC) or reject (simplest: client reconnects to correct node).

### 2.2 Global Token Service
The `TokenService` must be moved to Redis to support any horizontal scaling.

**Step 1: Redis-backed Token Storage**
Modify `server/src/token_service.rs`:
- Replace `Mutex<HashMap>` with a `redis::Client`.
- **Mint**: `SET token:{id} {metadata} EX 300`
- **Lock**: `SET token:{id}:lock {node_id} NX EX 30` (Distributed Lock)
- **Redeem**: Lua script to atomically check lock, verify metadata, and delete/mark consumed.

### 2.3 Enable Redis Broadcast
Ensure the `redis-broadcast` feature is enabled in `Cargo.toml` for the production build profile:

```toml
[features]
default = ["redis-broadcast"]
```

And configure `REDIS_URL` in the deployment environment.

---

## 3. Security (High Priority)

**Current Status:**
- **Replay Protection**: `ReplayGuard` uses in-memory HashSets/Bloom filters. A restart clears this, opening a window for replay attacks.
- **Input Sanitization**: Exists for `agent_propose` but relies on actors for standard `op_handler`.
- **Secrets**: `InMemoryPublicKeyStore` is hardcoded or empty.

### 3.1 Persistent Replay Protection
Move the `ReplayGuard` (`server/src/replay_protection.rs`) to Redis.

**Design:**
- Instead of in-memory Bloom filters, use **Redis Sets** with TTLs for recent `op_ids`.
- **Check**: `EXISTS op_dedup:{client_id}:{op_id}`
- **Insert**: `SET op_dedup:{client_id}:{op_id} 1 EX {window_duration}`
- *Optimization*: For high volume, use Redis Bloom Filter module (`BF.ADD`, `BF.EXISTS`).

### 3.2 Global Input Sanitization Middleware
Move the `sanitize_prompt_input` logic from `server/src/main.rs` into a global Tower middleware (`server/src/middleware/sanitization.rs`).

- Apply this middleware to **all** JSON bodies, not just LLM prompts.
- Strictly enforce `Content-Type: application/json`.
- Reject payloads exceeding 1MB (configurable).

### 3.3 Key Management
Replace `InMemoryPublicKeyStore` with a provider that fetches keys from a secure source.

**Step 1: Interface Definition**
Enhance `server/src/middleware.rs`:

```rust
pub trait PublicKeyProvider: Send + Sync {
    async fn get_key(&self, key_id: &str) -> Option<ed25519_dalek::VerifyingKey>;
}
```

**Step 2: Implementations**
- `EnvVarKeyProvider`: Reads `KEYS_JSON` env var (Good for K8s Secrets).
- `AwsKmsProvider`: Fetches from AWS KMS/SSM (Better for Enterprise).

---

## 4. Implementation Roadmap

### Phase 1: Persistence (The "No Data Loss" Milestone)
1.  [ ] Create `PostgresStore` implementing `PersistentStore`.
2.  [ ] Add `sqlx` migration files.
3.  [ ] Wire `DATABASE_URL` in `main.rs`.
4.  [ ] **Verification**: Kill server, restart, verify history is replayed from DB.

### Phase 2: Statelessness (The "Scale Out" Milestone)
1.  [ ] Refactor `TokenService` to use Redis.
2.  [ ] Refactor `ReplayGuard` to use Redis.
3.  [ ] Enable `redis-broadcast` feature by default.
4.  [ ] **Verification**: Spin up 2 replicas, mint token on A, redeem on B.

### Phase 3: Hardening (The "Production" Milestone)
1.  [ ] Implement Global Input Sanitization Middleware.
2.  [ ] Implement `EnvVarKeyProvider` for key rotation.
3.  [ ] Add structured logging (JSON) via `tracing-subscriber`.
4.  [ ] **Verification**: Run `cargo audit` and penetration test suite.

## 5. Deployment Architecture Recommendation

```mermaid
graph TD
    LB[Load Balancer] -->|WS /op (Sticky)| Node1[GP2F Server Replica 1]
    LB -->|WS /op (Sticky)| Node2[GP2F Server Replica 2]

    Node1 -->|Persist Events| DB[(Postgres Primary)]
    Node2 -->|Persist Events| DB

    Node1 -->|PubSub / Tokens / Replay| Redis[(Redis Cluster)]
    Node2 -->|PubSub / Tokens / Replay| Redis
```

**Note on Temporal**: While the codebase has hooks for Temporal, a direct Postgres implementation (`Phase 1`) allows you to go to production *now* without managing a Temporal cluster. Temporal integration can be resumed as a "Phase 4" when workflow complexity demands it.
