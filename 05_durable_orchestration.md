# 05_durable_orchestration.md

## Architecture Review: Durable Orchestration

### Overview
This domain uses Temporal (via the Rust SDK) to manage long-running workflows, reliable event sourcing, and state persistence. Temporal acts as the "durable supervisor" for critical operations that cannot be lost, such as applying an `op_id` to the authoritative ledger (backed by Postgres).

### Pros
*   **Durability:** Temporal guarantees that once a workflow starts, it will complete, even in the face of process crashes, network failures, or server restarts.
*   **Retries & Backoff:** Built-in policies for retrying failed activities (e.g., DB writes, external API calls) reduce the need for custom, error-prone retry logic.
*   **Sagas:** The Saga pattern is naturally supported, allowing for defined compensation steps (rollbacks) if a multi-step workflow fails halfway, ensuring data consistency.
*   **Visibility:** The Temporal UI provides a clear audit trail of every workflow execution, including inputs, outputs, timing, and errors, which is vital for debugging distributed systems.

### Cons & Risks
*   **Complexity:** Temporal adds a significant infrastructure component (Server, Cassandra/Postgres, Worker fleet) that must be managed, monitored, and scaled.
*   **Latency:** While fast, the overhead of persisting every workflow state change to Temporal's DB adds latency compared to a raw in-memory or direct-DB approach, which might conflict with the < 5ms goal if not pipelined.
*   **Versioning:** Updating workflow code (which must be strictly deterministic) requires careful versioning practices to avoid breaking in-flight workflows.
*   **DB Load:** Temporal can generate heavy write load on its backing store (Postgres) if not tuned correctly for high-throughput short workflows, potentially competing with application queries.

### Single Points of Failure (SPOF)
*   **Temporal Cluster:** If the Temporal cluster goes down, new state transitions cannot be processed, effectively freezing the authoritative state of the application.
*   **Postgres:** As the backing store for both application state and Temporal history (in this architecture), Postgres is a critical bottleneck and SPOF.

## Testing Strategy

### Disaster Recovery (DR) Testing
We must verify that the system is truly "durable" and can recover from catastrophic failures without data loss or corruption.

*   **Worker Killing:** Violently kill (SIGKILL) Temporal worker processes in the middle of executing a critical workflow (e.g., `ApplyOp`). Verify that a new worker picks up the task and completes it successfully from the last checkpoint.
*   **DB Outage:** Simulate a Postgres outage during high load. Verify that Temporal buffers workflows and retries effectively once the DB is back online, with zero dropped operations.
*   **Split-Brain:** Simulate a network partition between Temporal nodes (if clustered). Verify that the system prefers consistency over availability (or vice versa, depending on config) and recovers cleanly without corrupting history.

### Specific Test Cases & Scenarios
*   **Compensation Sagas:** Create a workflow that fails at step 3 of 5. Verify that the defined compensation activities for steps 2 and 1 are executed in reverse order to restore system consistency.
*   **Workflow Determinism:** Introduce a non-deterministic change (e.g., using `SystemTime::now()` or random numbers inside a workflow decision task). Verify that Temporal's replay check catches this and flags the workflow as non-deterministic.
*   **Idempotency:** Force-retry a successful `ApplyOp` activity (e.g., via the Temporal UI). Verify that the underlying logic is idempotent and does not apply the same operation twice to the domain state.
*   **Versioning Migration:** Start a long-running workflow with Version 1 logic. Deploy Version 2 logic. Verify that the in-flight workflow completes using V1 logic (or patches to V2) without crashing or getting stuck.

### Tools
*   **Temporal CLI (tctl):** For managing workflows, terminating instances, and inspecting history during tests.
*   **Chaos Mesh / Pumba:** For killing pods/processes in a Kubernetes/Docker environment to simulate hard failures.
*   **Postgres Fault Injection:** Tools to simulate slow queries, connection drops, or deadlock scenarios in the database.
*   **Custom Replay Tester:** A utility to download workflow histories from production and replay them against the current worker code to ensure backward compatibility.
