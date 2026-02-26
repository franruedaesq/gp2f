# Project Proposal 1: Global Logistics Integrity Platform (GLIP)

## 1. Project Definition
**GLIP** is a local-first, decentralized compliance and tracking system for high-value supply chains (e.g., pharmaceuticals, aerospace parts). It enables field inspectors, customs agents, and logistics providers to verify shipment integrity and compliance against complex, multi-jurisdictional regulatory frameworks, even in disconnected environments (ports, warehouses, cargo ships).

Unlike traditional cloud-based ERPs that fail offline or rely on eventual consistency without audit proofs, GLIP uses **gp2f** to cryptographically sign every inspection step, enforce policy rules locally (via WASM), and reconcile data deterministically when connectivity is restored.

## 2. Objectives
*   **Zero Data Loss:** Capture 100% of field inspection data regardless of network status.
*   **Verifiable Audit Trail:** Every state change (scan, temperature check, approval) is cryptographically signed and immutable.
*   **Real-time Compliance:** Enforce complex regulatory logic (FDA, EAR, GDPR) instantaneously on the client device using `gp2f`'s isomorphic policy engine.
*   **Conflict Resolution:** Automatically merge non-conflicting updates (e.g., GPS ping vs. humidity log) and flag critical conflicts (e.g., two inspectors sealing the same container with different IDs).

## 3. Product Requirements Document (PRD)

### 3.1 Frontend Screens (Mobile/Tablet App)

#### Screen 1: The Manifest Dashboard (Offline-Ready)
*   **View:** List of assigned shipments with status indicators (Pending, In-Transit, Held, Cleared).
*   **Action:** "Scan QR/Barcode" button floating at the bottom.
*   **Data:** Local `IndexedDB` query of the `Shipment` state projection.
*   **Policy Check:** Filter visible shipments based on the user's role (e.g., `CustomsAgent` sees all, `Driver` sees only their truck).

#### Screen 2: Dynamic Compliance Checklist
*   **Trigger:** Scanning a shipment ID.
*   **Logic:** The UI renders a form based on the `ShipmentType` and current `Jurisdiction`.
    *   *Example:* If `type == "PHARMA_COLD_CHAIN"`, show "Temperature Log Upload" and "Seal Verification".
*   **Interaction:**
    *   User inputs data (e.g., "Seal #12345 verified").
    *   **Immediate Feedback:** The `gp2f` WASM engine evaluates the `CompliancePolicy` locally. If the seal doesn't match the manifest, the UI shows a "REJECTED" error instantly, blocking submission.
*   **Optimistic Update:** On valid submission, the shipment status updates to "Verified" immediately, queuing the `op` for sync.

#### Screen 3: Dispute Resolution & Merge
*   **Trigger:** A server-side rejection (e.g., `REJECTED: Concurrent Modification`).
*   **View:** Split-screen showing "Your Value" (Local) vs. "Server Value" (Remote) vs. "Base Value".
*   **Action:** "Accept Remote", "Force Local" (if authorized), or "Merge Manually".
*   **Backend Support:** Uses `gp2f`'s `ThreeWayPatch` structure to visualize the exact field conflict.

### 3.2 Backend Architecture (Rust/Axum)

#### Data Model (Proto)
*   **Aggregate:** `Shipment`
*   **State:**
    ```protobuf
    message ShipmentState {
      string id = 1;
      string status = 2; // CREATED, IN_TRANSIT, HELD, CLEARED
      repeated Inspection inspections = 3;
      map<string, string> metadata = 4; // Dynamic fields
    }
    ```
*   **Events:** `ShipmentCreated`, `InspectionCompleted`, `CustomsHoldApplied`, `LocationUpdated`.

#### Policy Definitions (AST)
*   **File:** `policies/compliance_v1.json`
*   **Logic:**
    *   *Rule 1:* `InspectionCompleted` is only valid if `user.role == "INSPECTOR"`.
    *   *Rule 2:* `ClearShipment` is only allowed if `AllMandatoryChecks == true`.
    *   *Rule 3 (Complex):* If `origin == "HighRiskZone"`, require *two* distinct signatures (Dual Control).
*   **Format:** Defined using `NodeKind::AND`, `NodeKind::EQ`, `NodeKind::VIBE_CHECK` (for AI risk scoring).

### 3.3 Implementation Steps

#### Step 1: Define the Domain Model
*   Create `proto/glip.proto` defining the `Shipment` state and specific operation payloads.
*   Run `protoc` to generate Rust and TypeScript bindings.

#### Step 2: Policy Authoring
*   Write the "Dual Control" policy in JSON AST format.
*   Use `gp2f-cli eval` to test the policy against mock shipment states (e.g., ensure it fails with only one signature).

#### Step 3: Server-Side Reconciler
*   Initialize `gp2f_server::Reconciler` with the `Shipment` event store.
*   Implement `apply_op` trait to mutate the `ShipmentState` based on `op.action`.
*   Configure `RBAC` to map `user_id` -> `role` (e.g., `inspector`, `admin`).

#### Step 4: Client SDK Integration
*   Initialize `Gp2fClient` with `offlineQueue` enabled.
*   **Optimistic UI:**
    ```typescript
    const handleScan = async (scanData) => {
      // 1. Construct Op
      const op = { action: "Inspection", payload: scanData };
      // 2. Validate Local Policy (WASM)
      if (!policyEngine.evaluate(currentState, op)) {
        alert("Policy Violation: Missing required fields");
        return;
      }
      // 3. Send (Optimistic Apply)
      await client.send(op);
    };
    ```

#### Step 5: AI Risk Analysis (Optional)
*   Use `gp2f`'s `VibeEngine` (local ONNX) to analyze photos of the cargo.
*   If `VibeEngine` detects "Damaged Packaging" (confidence > 0.9), automatically flag the shipment as "HELD" via a `VIBE_CHECK` policy rule.
