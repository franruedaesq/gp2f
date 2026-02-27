# Production Readiness Report: GP2F

**Date:** 2024-05-25
**Status:** **PARTIALLY READY** (Usable with Postgres; **NOT** Usable with Temporal)

## Executive Summary

GP2F is architecturally sound for a "Demo" or "POC" environment but requires critical durability and consistency hardening before it can be considered production-ready for enterprise use cases.

While the core specific components (`policy-core`, `gp2f-crdt`) are robust and well-tested, the distributed system scaffolding (`gp2f-store`, `gp2f-actor`) contains several "happy path" implementations that will cause data loss or split-brain inconsistencies in a real-world partitioned network environment.

The system is **currently usable** for single-node deployments or multi-node deployments using the Postgres backend. However, the Temporal backend—a key selling point for long-running workflows—is incomplete and unsafe for production use.

---

## 1. Durability & Data Integrity

### Critical Finding: Temporal History Replay is Missing
**Severity:** **CRITICAL (Data Loss Risk)**
**Component:** `gp2f-store/src/temporal_store.rs`

The `TemporalStore::events_for` method, responsible for replaying event history to restore an actor's state after a crash or restart, is currently a stub:

```rust
// gp2f-store/src/temporal_store.rs
async fn events_for(&self, key: &str) -> Vec<StoredEvent> {
    if *self.connected.lock().await {
        tracing::debug!(..., "events_for: Temporal history query (not yet implemented; using fallback)");
    }
    self.fallback.events_for(key)
}
```

**Impact:**
If an actor pod restarts, it attempts to load its state from `events_for`. Since this returns an empty list (or local-only fallback data) in the Temporal implementation, the actor will restart with a blank state. **All previous accepted operations for that workflow instance will be lost to the application layer**, even though they exist in Temporal's history.

### Recommendation
Implement `events_for` using the `temporal-client` SDK to:
1.  Query the Temporal workflow execution history.
2.  Filter for `ApplyOp` signals or `ActivityTaskCompleted` events.
3.  Deserialize them into `StoredEvent` structs.
4.  Return the reconstructed history to the `ActorRegistry`.

---

## 2. Scalability & Distributed Consistency

### Critical Finding: "Fail-Open" Split-Brain Risk
**Severity:** **HIGH (Consistency Risk)**
**Component:** `gp2f-actor/src/actor.rs` (`ActorRegistry::get_or_spawn`)

The `RedisActorCoordinator` is designed to prevent two pods from running the same workflow instance (split-brain). However, if Redis is unavailable, the system defaults to "fail-open" behavior:

```rust
// gp2f-actor/src/actor.rs
Err(e) => {
    // Redis unavailable – fail open (spawn locally) so a Redis outage
    // does not take down the entire cluster.
    tracing::warn!(..., "Redis actor lock unavailable; spawning actor without distributed claim (fail-open)");
}
```

**Impact:**
In a network partition where Redis is reachable by some pods but not others (or during a Redis outage), **multiple pods will spawn the same actor locally**. They will accept conflicting operations and diverge. When the partition heals, there is no automatic mechanism to merge these divergent actor states, leading to permanent data inconsistency or operator intervention.

### Recommendation
Introduce a `STRICT_CONSISTENCY` mode (default for production) that "fails closed." If the distributed lock cannot be acquired, the request should be rejected with HTTP 503, preventing split-brain scenarios at the cost of availability during infrastructure outages.

---

## 3. Features & AI Capabilities

### Finding: Missing ONNX Runtime
**Severity:** **MEDIUM (Feature Gap)**
**Component:** `gp2f-vibe/src/vibe_classifier.rs`

The `VibeClassifier` supports hot-swapping models but currently lacks the actual ONNX inference logic:

```rust
// gp2f-vibe/src/vibe_classifier.rs
ModelSource::Onnx(_bytes) => {
    // TODO(onnx): deserialize session from `_bytes` and run inference.
    tracing::warn!(..., "falling back to rule-based classifier");
}
```

**Impact:**
The "Semantic Vibe Engine" described in the documentation effectively does not exist in its advanced form. The system falls back to a simple rule-based heuristic. While not a crash risk, this misleads users expecting ML-based intent classification.

### Recommendation
Integrate the `ort` (ONNX Runtime) crate to enable true inference.

---

## 4. Proposal for Top-Tier Standards

To meet the highest industry standards for a distributed event-sourced system:

1.  **Immutability & Audit:**
    *   **Enforce:** Every `events_for` replay must cryptographically verify the hash chain of events to detect tampering.
    *   **Add:** A "Sealed" state for workflow instances that have completed, allowing their history to be archived to cold storage (S3) and pruned from the active hot store (Redis/Postgres).

2.  **Observability:**
    *   **Add:** Distributed tracing (OpenTelemetry) context propagation is partially present but should be strictly enforced at the `Ingest` layer.
    *   **Metric:** `actor_recovery_latency` must be tracked. If `events_for` takes > 500ms, the system should alert.

3.  **Security:**
    *   **Add:** The current `guardrail_check` is a simple keyword match. Integrate a lightweight local BERT model for "Prompt Injection Detection" alongside the Vibe Classifier.

---

## Conclusion

**GP2F is NOT ready for production with the Temporal backend.** It is **READY** for production usage with the **Postgres** backend (`postgres-store` feature), provided that:
1.  The `RedisActorCoordinator` is configured to "Fail Closed" (requires code change).
2.  Sticky sessions or a deterministic load balancer are used if Redis is unstable.

**Next Steps:** Follow the `PRODUCTION_MIGRATION_GUIDE.md` to implement the critical fixes.
