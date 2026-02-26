# Project Proposal 1: Sovereign Pharma Cold-Chain (SPCC)

## 1. Project Definition
**SPCC** is a high-integrity logistics platform for tracking temperature-sensitive pharmaceuticals (vaccines, insulin) across multiple custodians (manufacturer -> distributor -> hospital). It operates entirely offline-first to handle connectivity gaps in transit.

Instead of relying on centralized servers or trust-based APIs, SPCC uses **gp2f** to embed complex regulatory compliance rules directly into the shipment data. Every handover requires cryptographic proof of compliance (e.g., "Temperature stayed within 2°C-8°C for 99.9% of the trip"), enforced deterministically by the local policy engine before a signature is accepted.

## 2. Objectives
*   **Proof of Integrity:** Ensure that physical goods match digital records without relying on external trust anchors.
*   **Complex Temporal Compliance:** Enforce rules like "If temp > 8°C for > 30 mins cumulatively, mark as SPOILED".
*   **Multi-Party Custody:** Support "Dual Control" handovers where both Driver and Pharmacist must sign an `op` within 5 minutes of each other.
*   **Offline Conflict Resolution:** Automatically merge sensor logs from the crate (IoT) with manual inspection notes from the driver.

## 3. Product Requirements Document (PRD)

### 3.1 Frontend Screens (Ruggedized Android Tablet)

#### Screen 1: Custody Dashboard
*   **View:** List of active shipments in the user's possession.
*   **Status Indicators:**
    *   *Green:* Compliant.
    *   *Yellow:* Warning (Temp nearing threshold).
    *   *Red:* **SPOILED** (Policy violation detected).
*   **Logic:** The status is calculated *live* on the client by replaying the sensor log against the `QualityControl.ast` policy.

#### Screen 2: Handover Protocol (The "Handshake")
*   **Action:** "Initiate Transfer".
*   **QR Code:** Generates a dynamic QR code containing the `ShipmentID` and a cryptographic nonce.
*   **Interaction:**
    1.  Receiver scans Sender's QR.
    2.  Receiver's app validates the shipment state (Checking 500+ data points of temp history).
    3.  If valid, Receiver signs an `AcceptCustody` op.
    4.  Sender scans Receiver's confirmation QR to sign a `ReleaseCustody` op.
*   **Constraint:** If the policy engine detects a cumulative temp excursion > 30 mins, the "Accept" button is strictly disabled. The Receiver *cannot* accept a spoiled shipment.

#### Screen 3: Audit & Dispute
*   **View:** A timeline of every `op` (Creation, SensorReading, CustodyTransfer).
*   **Feature:** "Replay Trace". Tap any event to see the exact policy state at that millisecond.
*   **Conflict:** If two sensors report different temps for the same timestamp, the UI shows a "Data Mismatch" warning and prompts for a manual "Quality Override" (requires Manager Role).

### 3.2 Backend Architecture (Rust/Axum)

#### Data Model (Proto)
*   **Aggregate:** `Shipment`
*   **State:**
    ```protobuf
    message ShipmentState {
      string id = 1;
      string custodian_id = 2; // Current owner
      repeated SensorReading temp_log = 3; // Time-series data
      enum Status { OK = 0; WARNING = 1; SPOILED = 2; }
      Status quality_status = 4;
      int64 cumulative_excursion_seconds = 5;
    }
    ```
*   **Events:** `CustodyTransfer`, `SensorLogBatch`, `ManualInspection`.

#### Policy Definitions (AST) - The "Business Logic"
*   **File:** `policies/vaccine_v2.json`
*   **Complex Logic (No AI, just Math):**
    *   *Excursion Check:* Iterate over `temp_log`. If `reading.temp > 8.0` OR `reading.temp < 2.0`, add `(reading.time - prev.time)` to `cumulative_excursion_seconds`.
    *   *Spoilage Rule:* `IF cumulative_excursion_seconds > 1800 THEN status = SPOILED`.
    *   *Role Check:* `CustodyTransfer` requires `signer.role == "LicensedPharmacist"`.
*   **Implementation:** These rules are compiled to WASM. The client runs them every time a new sensor reading comes in via Bluetooth.

### 3.3 Implementation Steps

#### Step 1: Define the Proto Schema
*   Create `proto/spcc.proto`. Define `SensorReading` (timestamp, value, device_id).
*   Generate Rust/TS code.

#### Step 2: Implement Temporal Logic in AST
*   This is the core complexity. You might need a custom `NodeKind::REDUCE` or similar iterator in `policy-core` (or perform the reduction in the `payload` projection logic and just check the *result* in the Policy).
    *   *Simpler Approach:* The `Shipment` entity has a `apply(SensorLogBatch)` method in Rust (shared Wasm/Native). The Policy just checks `state.cumulative_excursion_seconds < 1800`.
    *   **Task:** Write the `Shipment::apply` logic in `policy-core/src/crdt.rs` to handle time-series aggregation deterministically.

#### Step 3: The Handshake Flow
*   Build the QR code generation in the Client SDK.
*   Implement the "Atomic Swap" logic: Sender doesn't release until Receiver accepts. This creates two signed `op`s that must be sequenced correctly by the server.

#### Step 4: Offline Sync & Merge
*   Use `gp2f`'s `LWW` (Last-Write-Wins) for the `custodian_id` field.
*   Use `CRDT` (Append-Only Set) for the `temp_log`. Even if logs arrive out of order, they are sorted by timestamp during replay.

#### Step 5: Verification
*   Write a test case: "Simulate a 40-minute excursion. Assert that `AcceptCustody` fails with `PolicyError: SPOILED`."
