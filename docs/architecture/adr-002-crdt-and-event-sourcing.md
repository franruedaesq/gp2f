# ADR-002: CRDT-Based Event Sourcing with Yrs over Traditional Database Updates

**Status:** Accepted
**Date:** 2025-01-22
**Deciders:** Principal Engineering, Data Architecture, Infrastructure

---

## Context

GP2F operates in an environment with three irreconcilable constraints that make traditional database mutation patterns inadequate.

**Constraint 1: Offline-first operation.** Clients must be able to perform reads and writes while disconnected from the server. Operations queued offline must be mergeable with server state on reconnection without requiring user intervention or producing data loss.

**Constraint 2: Zero-latency UI updates.** The UI must reflect the outcome of a user action immediately, before network confirmation. Reverting an optimistic update after a server rejection must be handled cleanly, without visual artifacts.

**Constraint 3: Complete auditability.** Every mutation to application state must be traceable to a specific user, a specific time, a specific AST evaluation result, and a specific device session. The canonical record must be reconstructible from the event log alone, independent of the current database state.

Traditional database `UPDATE` and `PATCH` patterns fail all three constraints. A `PATCH /resource/123` request cannot be issued offline. It cannot be applied optimistically before server confirmation without complex rollback logic. And it destroys the prior state, making reconstruction from the log dependent on having captured a snapshot—an operational burden that is frequently incomplete.

---

## Decision

We will implement a fully event-sourced data layer using the following components:

**Yrs** (the Rust port of Yjs) for CRDT document storage of all collaborative or concurrently-modified fields.

**Signed `op_id`s** as the atomic unit of mutation. Every intended state change is represented as a cryptographically signed operation record before any network communication occurs.

**Postgres append-only event tables** as the canonical event store. No `UPDATE` or `DELETE` statements are issued against event rows.

**Redis PubSub** for broadcasting reconciled patches from the server to all connected clients.

---

## The `op_id` Construction Protocol

An `op_id` is a 32-byte value computed as:

```
op_id = HMAC-SHA256(
  key  = device_session_key,
  data = CBOR(intent || timestamp_ms || client_state_hash || sequence_number)
)
```

The `client_state_hash` is the BLAKE3 hash of the Yrs document snapshot at the time of the operation. The `sequence_number` is a monotonically increasing per-session counter stored in IndexedDB. Together, these fields make `op_id` collision-resistant across sessions and replay-resistant within a session.

The `op_id` is written to two durability targets atomically before the network emit: the in-memory Yrs document (as a CRDT operation) and the encrypted IndexedDB queue. The AES-GCM encryption key for IndexedDB is derived from the device session key, ensuring that queued operations are encrypted at rest and tied to the originating session.

---

## The Server-Side Reconciliation Protocol

When the server receives an `op_id`, it performs the following steps in a Temporal workflow activity:

1. Deserialize the CBOR payload and verify the HMAC against the session key stored in Postgres.
2. Reconstruct the client state at the time of the operation using the `client_state_hash` to fetch the corresponding Yrs document snapshot from the event store.
3. Re-evaluate the AST policy against the reconstructed state and the declared intent.
4. If the evaluation result is `ACCEPT`, apply the CRDT operations to the canonical Yrs document and append the `op_id` to the event log with outcome `ACCEPT`.
5. If the evaluation result is `REJECT`, append the `op_id` to the event log with outcome `REJECT` and construct a compensating CRDT patch that reverts the client's optimistic update.
6. Broadcast via Redis PubSub: the `op_id`, the outcome, and the 3-way CRDT diff (the delta between the client's predicted state and the canonical post-merge state).

All connected clients receive this broadcast. Clients that submitted the `op_id` use it to dequeue the operation from their IndexedDB queue and finalize or revert their optimistic update. Other clients apply the CRDT diff to synchronize to the canonical state.

---

## Why Yrs Specifically

Yrs was selected over other CRDT implementations for the following reasons.

**Rust-native implementation.** Yrs is a faithful port of Yjs to Rust with a stable FFI. This means the same CRDT logic runs natively on the server and can be compiled to WASM for the browser client. There is no polyglot impedance mismatch.

**Mature Yjs ecosystem.** The Yjs ecosystem includes `y-indexeddb` (offline persistence), `y-websocket` (sync transport), and integrations with CodeMirror, ProseMirror, and other editors. GP2F's CRDT layer is compatible with these integrations at the wire protocol level.

**Strong consistency guarantees.** Yrs implements the YATA algorithm, which provides strong eventual consistency: given the same set of operations applied in any order, all replicas converge to the same state. This guarantee holds across arbitrarily long offline periods.

**Structured data types.** Yrs supports `YMap`, `YArray`, `YText`, and `YXmlFragment` types. GP2F maps document fields to these types based on their merge semantics: collaborative text fields use `YText`, tag collections use `YArray`, and key-value metadata uses `YMap`.

---

## Alternatives Considered

**Alternative 1: Automerge**

Automerge is another mature CRDT library with a Rust implementation. It was evaluated and found to have higher per-operation memory overhead than Yrs for the GP2F access patterns (frequent small updates to structured documents). Automerge also lacks the established JavaScript ecosystem of Yjs, making browser integration more complex.

**Verdict:** Automerge was not selected due to higher memory overhead and weaker JavaScript ecosystem integration.

**Alternative 2: PostgreSQL with Row-Level Locking and OCC**

Optimistic Concurrency Control with PostgreSQL version columns is a well-understood pattern for handling concurrent writes. Each row has a `version` column; a write that finds a different version than the one it read is rejected, and the client retries.

This approach fails for GP2F because it is inherently online-only: the version column lives on the server, and an offline client cannot determine the current version or predict conflicts. It also fails the auditability requirement—OCC resolves conflicts by accepting the winning write and discarding the loser, which destroys information rather than merging it.

**Verdict:** PostgreSQL OCC was not selected due to online-only constraint and conflict resolution by discard.

**Alternative 3: Operational Transforms (OT)**

Operational Transforms are the algorithm underlying Google Docs' collaborative editing. They are proven in production at massive scale.

OT requires a central server to mediate and transform all operations. This is incompatible with the offline-first requirement, where clients generate operations without a server and must be able to merge them later. OT also requires that all clients receive operations in the same order from the server, which is a coordination requirement that is difficult to maintain in a distributed system.

**Verdict:** OT was not selected due to centralization requirement and online-only semantics.

---

## The Compaction Strategy

Yrs documents accumulate a tombstone log of all deleted operations to maintain merge semantics. This log grows monotonically and is the primary operational cost of the CRDT approach. Left unmanaged, a high-churn document can grow to tens of megabytes.

GP2F implements a checkpoint-based compaction strategy. After a Temporal workflow confirms that all clients in a session have acknowledged operations up to a given vector clock position (a checkpoint), the Yrs document is snapshotted and the tombstone log prior to the checkpoint is pruned. The snapshot replaces the incremental log as the base for future operations.

The checkpoint interval is configurable per-deployment. The formula for estimating required server memory per WebSocket connection before compaction is:

```
expected_memory = avg_mutations_per_session × avg_operation_size_bytes × checkpoint_interval_minutes / 60
```

For a typical deployment with 100 mutations per session hour, 64 bytes per operation, and a 30-minute checkpoint interval, this is approximately 3.2KB per connection—negligible.

---

## Consequences

**Positive Consequences**

The event log is the complete, immutable history of every mutation in the system. Any state at any point in time is reconstructible by replaying the log from the beginning. This satisfies auditability requirements without any additional logging infrastructure.

Clients can operate indefinitely offline. On reconnection, their queued `op_id`s are replayed against the authoritative state. The CRDT merge algorithm handles any conflicts without data loss.

The `op_id` HMAC provides non-repudiation at the operation level. An operation cannot be attributed to a session that did not hold the corresponding key.

**Negative Consequences**

The event store grows without bound. Postgres partitioning and archival policies are required to manage storage costs. GP2F recommends monthly partition rotation and cold archival to object storage (S3-compatible) after 90 days.

Implementing correct CRDT merge logic for all field types requires careful schema design. Fields that should not be merged concurrently must be marked `transactional` and will block on in-flight operations. Incorrect classification leads to either unintended concurrent writes or unnecessary contention.

The Temporal workflow for reconciliation introduces latency between the client's `op_id` emission and the server's `ACCEPT`/`REJECT` broadcast. Under normal conditions this is 8–15ms. Under high load, Temporal's task queue may introduce additional latency. This is a known trade-off accepted in exchange for the workflow's durability and retry guarantees.
