# 02_event_sourcing_and_crdt.md

## Architecture Review: Pillar 2 - Event-Sourced Synchronization

### Overview
This pillar manages state distribution and consistency. By treating the client as a predictive game client that emits signed operations (`op_id`), and the server as an authoritative reconciler, GP2F achieves 0ms perceived latency. It leverages Yrs (CRDTs) for mergeable fields and an event-sourced architecture for transactional integrity.

### Pros
*   **Zero-Latency UX:** Clients update local state immediately (optimistic UI), providing a snappy experience regardless of network conditions.
*   **Offline-First:** Operations are queued locally (IndexedDB) and synchronized when the connection is restored, enabling work in disconnected environments.
*   **Conflict Resolution:** Using Yrs (CRDTs) allows for automatic merging of concurrent edits to text and list structures, reducing manual conflict resolution.
*   **Security:** Cryptographically signed `op_id`s prevent tampering and replay attacks, ensuring that only valid clients can propose state changes.

### Cons & Risks
*   **Reconciliation Complexity:** Implementing correct rollback and re-application of operations when the server rejects an op or sends a conflicting patch is notoriously difficult and prone to "jittery" UI bugs.
*   **CRDT Overhead:** CRDTs can grow in size over time (tombstones), potentially impacting memory and network bandwidth if not compacted or garbage-collected properly.
*   **Backpressure Management:** A flood of offline operations upon reconnection can overwhelm the server or the client's processing capability, leading to timeouts.
*   **Clock Skew:** While the system uses logical clocks/counters, reliance on timestamps for user-facing history or ordering can be problematic if client clocks are significantly skewed.

### Single Points of Failure (SPOF)
*   **The Reconciler:** If the server-side reconciliation logic differs even slightly from the client's prediction logic, infinite loops of "prediction -> rejection -> rollback -> prediction" can occur, rendering the app unusable.
*   **CRDT State Corruption:** If the underlying CRDT structure is corrupted (e.g., due to a bug in Yrs or improper serialization), the document state may become unrecoverable across all clients.

## Testing Strategy

### Chaos Testing
To ensure the system is resilient to network failures, distributed concurrency issues, and hostile environments, we must employ rigorous chaos testing.

*   **Network Partitions:** Use **Toxiproxy** to simulate network cuts, high latency, and packet loss between client and server. Verify that the client queues ops correctly and the server handles the eventual burst without data loss.
*   **Concurrency Stress:** Simulate multiple clients modifying the same resource simultaneously. Verify that all clients eventually converge to the same state (Eventual Consistency) using **Jepsen**-style verification techniques.
*   **Clock Skew:** Run clients with skewed system clocks and verify `op_id` ordering, acceptance logic, and that the server's authoritative timestamp takes precedence.

### Specific Test Cases & Scenarios
*   **3-Way Patch Conflict:** Manually craft scenarios where a client's predicted state diverges from the server's authoritative state (e.g., simultaneous edits to a non-CRDT field). Verify the 3-way patch (Base, Local, Authoritative) is applied correctly and the UI updates without jitter.
*   **Offline Queue Backpressure:** Fill the client's offline queue with thousands of operations. Restore connection and verify the server processes them in batches (throttling) without timing out or crashing, and that the client UI remains responsive.
*   **CRDT Fuzzing:** Use property-based testing to apply random Yrs operations (insert, delete, format) from multiple "clients" in random orders. Verify that all clients end up with identical document state regardless of operation order.
*   **Rejection Handling:** Force the server to reject a valid-looking operation (e.g., due to a simulated policy change). Verify the client performs a smooth rollback/undo animation and notifies the user appropriately.

### Tools
*   **Toxiproxy:** For deterministic network failure simulation (latency, bandwidth, partitions).
*   **k6:** For simulating concurrent user load and "syncing storms" where many clients reconnect simultaneously.
*   **Yrs Fuzzers:** Leverage the built-in fuzzing tools within the Yrs crate (or custom wrappers) to test CRDT convergence under extreme conditions.
*   **Spoofer CLI:** As specified, use the Spoofer CLI to inject malformed, replayed, or out-of-order ops to test server resilience and signature verification.

## Mitigation Plan of Action

### Phase 1: Reconciliation Robustness
**Goal:** Eliminate "UI Jitter" and infinite reconciliation loops.
*   **Step 1.1:** Formalize the "3-Way Merge" logic (Base, Local, Server) into a standalone, pure function that can be unit tested with thousands of edge cases.
*   **Step 1.2:** Implement a "Settle Duration" in the UI. If a conflict occurs, pause optimistic updates for 500ms to allow the server state to stabilize before unlocking the UI.
*   **Testing:** Use the "Spoofer CLI" to inject intentionally conflicting ops and verify that the client stabilizes within 2 round-trips.

### Phase 2: CRDT Hygiene & Performance
**Goal:** Prevent memory bloat from long-lived documents.
*   **Step 2.1:** Implement "Tombstone Garbage Collection" in Yrs. Configure a policy to purge deleted items after a set retention period (e.g., 30 days).
*   **Step 2.2:** Use "Snapshotting." Periodically (e.g., every 100 ops) save a flattened snapshot of the Yrs doc to S3/Blob storage, so new clients don't have to replay the entire history.
*   **Testing:** Run a "Long-Haul" test where a document is edited 1 million times. Verify memory usage remains constant after GC runs.

### Phase 3: Traffic Control & Backpressure
**Goal:** Prevent "Thundering Herd" during reconnection.
*   **Step 3.1:** Implement a Token Bucket rate limiter on the client's sync queue. Only release 50 ops/second to the server upon reconnection.
*   **Step 3.2:** Add "Server-Side Backpressure" headers. If the server is overloaded, it responds with `Retry-After: N`. The client must respect this.
*   **Testing:** Simulate a 10,000-op offline queue. Reconnect and verify the client trickles ops rather than flooding, and respects 429 responses.

### Phase 4: Time Synchronization
**Goal:** Mitigate clock skew issues.
*   **Step 4.1:** Adopt Hybrid Logical Clocks (HLC). Use a combination of physical time and a logical counter to order events causally, independent of wall-clock skew.
*   **Step 4.2:** On connect, the server sends its current time. The client calculates an offset and applies it to all local timestamps before signing.
*   **Testing:** Set client system time to 1 year in the future. Verify `op_id`s are still ordered correctly relative to other clients using HLC/Offset.
