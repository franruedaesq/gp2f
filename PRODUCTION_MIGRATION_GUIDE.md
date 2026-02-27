# Production Migration Guide

This guide details the steps required to address the critical gaps identified in the [Production Readiness Report](PRODUCTION_READINESS_REPORT.md). Following these steps will upgrade GP2F from a "Demo/POC" system to a production-grade distributed event-sourcing platform.

---

## Step 1: Implement Temporal History Replay (Critical Durability Fix)

**Context:**
The `TemporalStore` currently lacks the implementation for `events_for`, meaning actor restarts result in data loss for that workflow instance.

**Action:**
Implement `TemporalStore::events_for` in `gp2f-store/src/temporal_store.rs`.

**Implementation Details:**

1.  **Dependency:** Ensure `temporalio-client` is available (feature `temporal-production`).
2.  **Logic:**
    *   Use `client.get_workflow_handle(workflow_id).fetch_history()`.
    *   Iterate through history events.
    *   Filter for `EventType::ActivityTaskCompleted` where the activity type is `ApplyOp`.
    *   Deserialize the `result` payload back into a `StoredEvent`.
    *   Sort by `event_id` or sequence number to ensure strict ordering.

**Code Snippet (Conceptual):**

```rust
// gp2f-store/src/temporal_store.rs

async fn events_for(&self, key: &str) -> Vec<StoredEvent> {
    #[cfg(feature = "temporal-production")]
    {
        if let Some(client) = self.client.lock().await.as_ref() {
            let workflow_id = Self::workflow_id_for(key);
            let handle = client.get_workflow_handle(workflow_id);

            // Fetch history (paginated)
            let history = handle.fetch_history(None).await?;
            let mut events = Vec::new();

            for event in history.events {
                if let Some(attr) = event.activity_task_completed_event_attributes {
                    // This assumes the activity result is the StoredEvent JSON
                    if let Ok(stored_event) = serde_json::from_slice(&attr.result.payloads[0].data) {
                        events.push(stored_event);
                    }
                }
            }
            return events;
        }
    }
    // Fallback to in-memory store if not connected
    self.fallback.events_for(key).await
}
```

---

## Step 2: Implement "Strict Consistency" Mode (Critical Scalability Fix)

**Context:**
`RedisActorCoordinator` currently "fails open," allowing split-brain scenarios if Redis is unavailable.

**Action:**
Modify `gp2f-actor/src/actor.rs` to support a `STRICT_CONSISTENCY` configuration.

**Implementation Details:**

1.  **Configuration:** Add `CONSISTENCY_MODE` env var (values: `eventual`, `strict`). Default to `strict` for production.
2.  **Logic:**
    *   In `ActorRegistry::get_or_spawn`:
    *   If `coordinator.try_claim` fails (Redis error):
        *   If `strict`: Return `Err(ServiceUnavailable("Distributed lock unreachable"))`.
        *   If `eventual`: Log warning and proceed (current behavior).

**Code Snippet (Conceptual):**

```rust
// gp2f-actor/src/actor.rs

pub enum ConsistencyMode {
    Strict,
    Eventual,
}

// In ActorRegistry::get_or_spawn
Err(e) => {
    match self.consistency_mode {
        ConsistencyMode::Strict => {
             tracing::error!(..., "Strict consistency violation: Redis lock unreachable. Refusing to spawn.");
             return Err(SplitBrainError::LockUnreachable);
        }
        ConsistencyMode::Eventual => {
             tracing::warn!(..., "Redis actor lock unavailable; spawning actor (fail-open)");
        }
    }
}
```

---

## Step 3: Integrate ONNX Runtime (Feature Completion)

**Context:**
`VibeClassifier` supports hot-swapping models but lacks the inference engine.

**Action:**
Add `ort` crate dependency and implement `VibeClassifier::classify`.

**Implementation Details:**

1.  **Dependency:** Add `ort = { version = "2.0", features = ["ndarray"] }` to `gp2f-vibe/Cargo.toml`.
2.  **Logic:**
    *   In `VibeClassifier::classify`:
    *   Create an `ort::Session` from `model_bytes`.
    *   Convert `VibeInput` to an `ndarray::Array1`.
    *   Run `session.run(inputs![array])?`.
    *   Extract output tensor and map to `(intent, confidence, bottleneck)`.

**Code Snippet (Conceptual):**

```rust
// gp2f-vibe/src/vibe_classifier.rs

use ort::{GraphOptimizationLevel, Session};

// Inside classify()
ModelSource::Onnx(bytes) => {
    let session = Session::builder()?
        .with_optimization_level(GraphOptimizationLevel::Level3)?
        .with_intra_threads(1)?
        .commit_from_memory(&bytes)?;

    let input_tensor = ndarray::arr1(&[
        input.mouse_velocity,
        input.keypress_deltas,
        input.error_count as f64,
        input.sentiment,
    ]);

    let outputs = session.run(ort::inputs![input_tensor]?)?;
    let output_tensor = outputs["output_label"].extract_tensor::<f32>()?;

    // Map output_tensor[0..2] -> intent, confidence...
}
```

---

## Step 4: Add Cryptographic Event Verification (Security Hardening)

**Context:**
To ensure `events_for` returns a valid, untampered history.

**Action:**
Update `StoredEvent` to include a `previous_hash` field.

**Implementation Details:**

1.  **Schema Change:** Add `previous_hash: String` to `StoredEvent`.
2.  **Logic:**
    *   When appending an event: `current_event.previous_hash = hash(last_event)`.
    *   When replaying in `events_for`: Verify `hash(events[i-1]) == events[i].previous_hash`.
    *   If verification fails, panic or return `Err(DataCorruption)`.

---

## Verification Plan

After implementing these changes:

1.  **Durability:** Start a server with `TEMPORAL_ENDPOINT` configured. Submit 10 ops. Kill the server. Restart it. Verify via `GET /state` that the state is restored.
2.  **Scalability:** Configure `CONSISTENCY_MODE=strict`. Block Redis port (simulate partition). Attempt to spawn a new actor. Verify the request fails with 503.
3.  **Features:** Load a dummy `.onnx` model via `VibeClassifier::load_model_from_url`. Send an op. Verify logs show "ONNX inference successful".
