# Project Proposal 2: Disaster Response Triage System (DRTS)

## 1. Project Definition
**DRTS** is a rapid-deployment, offline-first application for managing patient triage, treatment, and evacuation during mass casualty incidents (earthquakes, war zones, remote areas). It enables medical teams to collaborate seamlessly even when cellular infrastructure is down, syncing data opportunistically via mesh networks or sporadic satellite uplinks.

Leveraging **gp2f**, DRTS ensures that critical patient data (vitals, medication history, triage status) is never lost, conflicts are resolved safely (e.g., two medics updating the same patient record), and treatment protocols are enforced locally to prevent medical errors.

## 2. Objectives
*   **Offline Reliability:** 100% functionality without internet; automatic background sync when connectivity returns.
*   **Protocol Adherence:** Enforce standardized triage algorithms (START/SALT) via deterministic policy execution.
*   **Resource Management:** Prevent over-allocation of scarce resources (ventilators, OR slots) using strict `TRANSACTIONAL` conflict resolution.
*   **Data Integrity:** Immutable, signed event logs for every treatment action, ensuring post-incident accountability.

## 3. Product Requirements Document (PRD)

### 3.1 Frontend Screens (Ruggedized Tablet/Mobile)

#### Screen 1: Rapid Intake & Triage
*   **Input:** Scan a physical triage tag (QR/NFC) or manually enter a unique ID.
*   **Form:** Quick-tap interface for vitals (Pulse, Resp Rate, Mental Status).
*   **Logic:** The app automatically calculates the triage category (Red/Yellow/Green/Black) based on the START algorithm policy running in WASM.
    *   *Constraint:* Status cannot be downgraded without a documented physician override (policy enforced).

#### Screen 2: Patient Dashboard & Handover
*   **View:** List of patients sorted by acuity (Red first).
*   **Action:** "Assign Bed", "Administer Meds", "Mark for Evac".
*   **Collaboration:** Shows real-time indicators of other medics viewing/editing the same patient (via ephemeral presence, if online).
*   **Sync Status:** Visual indicator for "Unsynced Changes" (e.g., "3 updates pending upload").

#### Screen 3: Resource Allocation Map
*   **View:** GIS overlay showing incident zones and available assets (Ambulances, Field Hospital Beds).
*   **Interaction:** Drag-and-drop a patient to a resource.
*   **Conflict Handling:** If two medics claim the last ventilator simultaneously, the server accepts the first valid `op` and rejects the second with a clear "Resource Unavailable" message (using `TRANSACTIONAL` strategy).

### 3.2 Backend Architecture (Rust/Axum)

#### Data Model (Proto)
*   **Aggregate:** `PatientRecord`, `ResourcePool`
*   **State:**
    ```protobuf
    enum TriageCategory { GREEN = 0; YELLOW = 1; RED = 2; BLACK = 3; }
    message Patient {
      string id = 1;
      TriageCategory category = 2;
      repeated VitalSign vitals = 3;
      repeated string medications = 4;
      string assigned_bed_id = 5;
    }
    ```

#### Policy Definitions (AST)
*   **File:** `policies/triage_start_v1.json`
*   **Logic:**
    *   *Rule 1:* If `RespRate > 30`, Category MUST be `RED`.
    *   *Rule 2:* `AdministerMorphine` allowed only if `SystolicBP > 90`.
    *   *Rule 3:* `Discharge` only allowed if `Category == GREEN` and `ChiefComplaint == RESOLVED`.
*   **Implementation:** Defined as `gp2f` AST nodes (e.g., `GT`, `AND`, `FIELD`).

### 3.3 Implementation Steps

#### Step 1: Initialize Project & SDK
*   Set up the `gp2f` server with the `PatientRecord` event store.
*   Configure the client SDK with `offlineQueue` persistence (IndexedDB).
    ```typescript
    const client = new Gp2fClient({
      url: 'wss://drts-server...',
      offlineQueue: { maxSize: 5000, flushOnReconnect: true }
    });
    ```

#### Step 2: Define Triage Logic (Rust -> WASM)
*   Write the START algorithm as a `gp2f` Policy AST.
    *   Use `NodeKind::GT` for respiratory rate checks.
    *   Use `NodeKind::AND` for verifying adequate perfusion.
*   Compile the policy to WASM and bundle it with the client app.

#### Step 3: Implement Conflict Resolution
*   For **Patient Vitals**: Use `CRDT` (Yrs) strategy. Merging two heart rate readings is just appending to the history log.
*   For **Bed Assignment**: Use `TRANSACTIONAL` strategy.
    *   Define the field conflict strategy in the `ResourcePool` schema.
    *   Handle `RejectResponse` in the UI by showing a "Bed Taken" toast notification.

#### Step 4: Secure the Data
*   Enable `gp2f`'s HMAC signature validation (`GP2F_TENANT_SECRET`).
*   Ensure all `OpId`s are signed by the medic's device key before submission.
