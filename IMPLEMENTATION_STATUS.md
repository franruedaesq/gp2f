# Implementation Status Report

This document analyzes the current state of the codebase against the requested feature roadmap (Phases 1-4).

## Phase 1: Reconciliation Robustness

**Goal:** Eliminate "UI Jitter" and infinite reconciliation loops.

*   **Step 1.1: Formalize "3-Way Merge" logic (Base, Local, Server)**
    *   **Status:** ⚠️ **Partially Implemented**
    *   **Details:**
        *   The `server/src/reconciler.rs` file contains a `build_three_way_patch` function, but it primarily performs a shallow comparison between the server state and the client payload.
        *   It does not leverage a true "Base" state (common ancestor) for a full 3-way merge.
        *   While `policy-core/src/crdt.rs` defines `CrdtDoc` using `yrs`, the integration in `reconciler.rs` explicitly stubs out the `YjsText` merge logic (`FieldStrategy::YjsText => server_val.clone()`), stating it is "out of scope for the resolver stub".
    *   **Missing:**
        *   True 3-way merge logic using a stored Base state.
        *   Full integration with `yrs` for `FieldStrategy::YjsText` in the reconciler.
        *   Refactoring the merge logic into a standalone, pure function that can be unit-tested with extensive edge cases.

*   **Step 1.2: Implement "Settle Duration" in UI**
    *   **Status:** ❌ **Not Implemented**
    *   **Details:**
        *   `client-sdk/src/MergeModal.tsx` handles manual conflict resolution (UI for "Keep mine" vs "Use server").
        *   However, `client-sdk/src/client.ts` lacks logic to pause optimistic updates for 500ms after a conflict occurs.
    *   **Missing:**
        *   Logic to pause optimistic updates for a "Settle Duration" upon conflict.

*   **Testing: "Spoofer CLI"**
    *   **Status:** ❌ **Not Implemented**
    *   **Details:**
        *   `cli/src/main.rs` implements `eval` and `replay` commands.
        *   There is no command to inject intentionally conflicting operations for testing purposes.
    *   **Missing:**
        *   A "Spoofer" CLI command or tool.

## Phase 2: CRDT Hygiene & Performance

**Goal:** Prevent memory bloat from long-lived documents.

*   **Step 2.1: Implement "Tombstone Garbage Collection" in Yrs**
    *   **Status:** ❌ **Not Implemented**
    *   **Details:**
        *   There is no configuration for `yrs` garbage collection or tombstone purging in `policy-core/src/crdt.rs` or `server/src/reconciler.rs`.
    *   **Missing:**
        *   Yrs GC configuration to purge deleted items after a retention period.

*   **Step 2.2: Use "Snapshotting" to S3/Blob storage**
    *   **Status:** ⚠️ **Partially Implemented (In-Memory Only)**
    *   **Details:**
        *   `server/src/event_store.rs` implements in-memory compaction (merging accepted ops into a snapshot event) when the log exceeds a threshold (currently 1,000 events).
        *   It does not implement persistence to external storage like S3.
    *   **Missing:**
        *   Persistence of snapshots to S3/Blob storage.
        *   Configuration to snapshot more frequently (e.g., every 100 ops).

## Phase 3: Traffic Control & Backpressure

**Goal:** Prevent "Thundering Herd" during reconnection.

*   **Step 3.1: Token Bucket rate limiter on client**
    *   **Status:** ❌ **Not Implemented**
    *   **Details:**
        *   `client-sdk/src/client.ts` sends messages immediately via `ws.send()` without any rate limiting.
    *   **Missing:**
        *   Token Bucket rate limiter logic on the client's sync queue.

*   **Step 3.2: Add "Server-Side Backpressure" headers**
    *   **Status:** ⚠️ **Partially Implemented**
    *   **Details:**
        *   `server/src/limits.rs` implements backpressure logic (returning `QueueFull` signals).
        *   `server/src/reconciler.rs` translates this into a JSON `Reject` message.
        *   However, it does **not** include a `Retry-After` header or field in the response protocol.
    *   **Missing:**
        *   Implementation of the `Retry-After` header/field in the rejection response.
        *   Client-side logic to respect the `Retry-After` directive.

## Phase 4: Time Synchronization

**Goal:** Mitigate clock skew issues.

*   **Step 4.1: Adopt Hybrid Logical Clocks (HLC)**
    *   **Status:** ❌ **Not Implemented**
    *   **Details:**
        *   No implementation of Hybrid Logical Clocks was found in `server/src` or `client-sdk/src`. Timestamps appear to be standard wall-clock time (`Utc::now()`).
    *   **Missing:**
        *   HLC implementation for causal ordering.

*   **Step 4.2: Server sends current time on connect**
    *   **Status:** ❌ **Not Implemented**
    *   **Details:**
        *   The handshake process in `client-sdk/src/client.ts` and the server does not involve exchanging the server's current time.
    *   **Missing:**
        *   Server sending its time on connection.
        *   Client calculating and applying a time offset.
