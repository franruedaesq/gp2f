# Production Readiness Report: GP2F Architecture

**Date:** 2024-05-24
**Status:** **USABLE for Production** (with Postgres backend)
**Version:** 1.0.0

---

## Executive Summary

The GP2F (Global Policy-Driven Framework) architecture is **ready for production deployment** when configured with the **Postgres Store** (`postgres-store`) and **Redis Broadcast** (`redis-broadcast`) features.

While the system supports a pluggable architecture, the **Temporal Store** (`temporal-store`) and **ONNX Vibe Classifier** are currently in a **Beta / Experimental** state due to missing dependencies and incomplete integrations.

---

## 1. Durability & Persistence

### Current Status: **READY (Postgres)** / **EXPERIMENTAL (Temporal)**

*   **Postgres Store (`gp2f-store/src/postgres_store.rs`)**:
    *   **Implementation**: Fully implemented using `sqlx`.
    *   **Concurrency Control**: Uses `pg_advisory_xact_lock` based on a hash of the `(tenant_id, workflow_id, instance_id)` tuple. This correctly serializes concurrent writes to the same workflow instance, preventing race conditions.
    *   **Idempotency**: The `event_log` table has a unique index on `op_id` (via migration `20240524_op_id_unique.sql`), ensuring that retried operations do not result in duplicate events.
    *   **Schema Management**: Migrations are automatically applied on startup.

*   **Temporal Store (`gp2f-store/src/temporal_store.rs`)**:
    *   **Implementation**: Partially implemented. The core logic for routing to Temporal exists but relies on a `TODO` for the actual gRPC client connection.
    *   **Missing Dependency**: The `temporal-client` crate is not yet added to `Cargo.toml`.
    *   **Recommendation**: Do **not** use `temporal-production` feature for immediate production deployment.

### Action Items:
1.  **Deploy Postgres**: Ensure a high-availability Postgres cluster (v14+) is available.
2.  **Configure Env**: Set `DATABASE_URL` to the Postgres connection string.
3.  **Run Migrations**: The server will auto-migrate, but for zero-downtime deployments, run `sqlx migrate run` in your CI/CD pipeline.

---

## 2. Scalability & Concurrency

### Current Status: **READY**

*   **Actor Model (`gp2f-actor/src/actor.rs`)**:
    *   **Isolation**: Each workflow instance is handled by a dedicated `WorkflowActor` task, ensuring serial processing of operations.
    *   **State Recovery**: Actors correctly replay events from the `PersistentStore` on startup, restoring the latest state.

*   **Split-Brain Protection (`redis-broadcast`)**:
    *   **Mechanism**: The `RedisActorCoordinator` implements a distributed lock using Redis (`SET NX EX`).
    *   **Safety**: Before spawning a local actor, the registry attempts to claim the instance ID in Redis. If another pod holds the lock, the local spawn is rejected, preventing split-brain scenarios where two pods modify the same instance concurrently.
    *   **Fail-Open**: If Redis is down, the system fails open (spawns locally) to maintain availability, logging a warning. This is a reasonable trade-off for availability but carries a risk of data inconsistency during Redis outages.

### Action Items:
1.  **Deploy Redis**: Ensure a Redis Cluster or Sentinel setup is available.
2.  **Configure Env**: Set `REDIS_URL` and enable the `redis-broadcast` feature flag.

---

## 3. Security

### Current Status: **READY**

*   **Authentication & Authorization (`gp2f-security`)**:
    *   **RBAC**: Role-Based Access Control is implemented via `RbacRegistry` and AST-based guards.
    *   **Configuration**: Roles and permissions can be loaded from `RBAC_CONFIG_JSON`, allowing dynamic updates without code changes.
    *   **Default Policies**: A sensible default policy (Admin/Reviewer/Agent) is built-in.

*   **Input Validation**:
    *   **Sanitization**: `sanitize_prompt_input` strips control characters and invisible Unicode to prevent prompt injection and homoglyph attacks.
    *   **Guardrails**: A regex-based `guardrail_check` blocks common jailbreak patterns ("ignore previous instructions", etc.) before they reach the LLM.

*   **Cryptography**:
    *   **Signatures**: Ed25519 signatures are verified on `op_id` if a key provider is configured.
    *   **Key Management**: Supports loading keys from `KEYS_JSON` (env var) or polling a URL (`KEYS_POLL_INTERVAL_SECS`).

### Action Items:
1.  **Key Rotation**: Configure `KEYS_POLL_INTERVAL_SECS` to point to your JWKS/Key endpoint for key rotation.
2.  **RBAC Policy**: Define your production roles in `RBAC_CONFIG_JSON`.
3.  **HTTPS**: Terminate TLS at the load balancer (Ingress/Gateway).

---

## 4. AI & Vibe Engine

### Current Status: **PARTIAL (Rule-Based Only)**

*   **Vibe Classifier (`gp2f-vibe/src/vibe_classifier.rs`)**:
    *   **Rule-Based Fallback**: The system currently uses a heuristic engine (checking mouse velocity, error counts) to determine user intent ("frustrated", "confused"). This is functional and fast.
    *   **ONNX Model**: The code contains `TODO(onnx)` placeholders. The `VibeClassifier` can load model bytes but cannot yet run inference because the ONNX runtime is not linked.

*   **LLM Integration**:
    *   **Providers**: Supports OpenAI, Anthropic, and Groq via the `LlmProvider` trait.
    *   **Safety**: The `agent_propose_handler` includes rate limiting, tool gating, and the aforementioned guardrails.

### Action Items:
1.  **Accept Rule-Based Limitations**: Be aware that "Vibe" detection will be heuristic-based until the ONNX runtime is integrated.

---

## Step-by-Step Production Deployment Guide

### Prerequisites
*   **Database**: PostgreSQL 14+
*   **Cache/Lock**: Redis 6+
*   **Orchestrator**: Kubernetes (recommended)

### 1. Build the Artifact
Compile the server with the production feature set. Do **not** include `temporal-production`.

```bash
cargo build --release --features postgres-store,redis-broadcast
```

### 2. Configure Environment Variables
Set the following variables in your deployment manifest (k8s ConfigMap/Secret):

| Variable | Description | Required | Example |
| :--- | :--- | :--- | :--- |
| `APP_ENV` | Mode flag | Yes | `production` |
| `DATABASE_URL` | Postgres connection | Yes | `postgres://user:pass@db:5432/gp2f` |
| `REDIS_URL` | Redis connection | Yes | `redis://redis:6379` |
| `RUST_LOG` | Log level | No | `info,gp2f_server=debug` |
| `LOG_FORMAT` | Log format | No | `json` |
| `KEYS_POLL_INTERVAL_SECS` | Key rotation | Yes | `300` |
| `RBAC_CONFIG_JSON` | RBAC Policy | Yes | `{"admin": ["*"]}` |

### 3. Database Migration
Run the migration before rolling out the new pods.

```bash
# Using sqlx CLI (if installed)
sqlx migrate run --source migrations

# OR let the server run it on startup (default behavior)
```

### 4. Health Checks
Configure Kubernetes probes:

*   **Readiness Probe**: `GET /health` (Checks DB connectivity)
*   **Liveness Probe**: `GET /livez` (Checks process uptime)

### 5. Verify Deployment
Once deployed, verify:
1.  **Logs**: Check for "Postgres event store connected" and "Actor registry initialized".
2.  **Split-Brain**: Scale to 2 replicas and check logs for "actor lock claimed".
3.  **Traffic**: Send a `POST /op` and verify it is persisted in the `event_log` table.

---

## Future Work (Post-Launch)

1.  **Enable Temporal**:
    *   Add `temporal-client` dependency.
    *   Implement the gRPC connection logic in `TemporalStore::connect`.
    *   Replace the `TODO` in `route_to_temporal`.

2.  **Enable ONNX**:
    *   Add `ort` (ONNX Runtime) dependency.
    *   Implement the inference logic in `VibeClassifier::classify`.

3.  **Observability**:
    *   Integrate OpenTelemetry for distributed tracing beyond basic logs.
