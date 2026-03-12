# PROPOSAL: Enterprise Global Payroll Operations Dashboard

## Executive Summary

This proposal outlines the development of a unified **Global Payroll Operations Dashboard** for a multinational corporation. The system will leverage the **GP2F Framework** to manage complex, multi-jurisdictional payroll workflows, ensuring strict compliance, immutable audit trails, and zero-latency collaboration for distributed HR and Finance teams.

By utilizing `@gp2f/client-sdk` for the frontend and `@gp2f/server` (Node.js bindings) for the backend policy engine, we will replace disparate spreadsheets and legacy region-specific tools with a single, deterministic source of truth.

## Business Requirements

### 1. Multi-Jurisdictional Compliance
*   **Challenge:** Different countries have unique tax rules, deduction logic, and approval requirements.
*   **Solution:** Use **GP2F AST Policies** to encode country-specific logic.
    *   *Example:* A policy for "France" might require specific social security deductions that are invalid in "USA".
    *   *Mechanism:* The `policy-core` engine evaluates these rules in real-time. Changes to tax laws are deployed as versioned AST updates, not code rewrites.

### 2. Strict Role-Based Access Control (RBAC) & Segregation of Duties
*   **Challenge:** Preventing fraud and unauthorized changes. A "Payroll Entry Clerk" cannot "Approve" a batch they created.
*   **Solution:** Enforce RBAC via GP2F's `role` field in `ClientMessage`.
    *   *Policy:* `AND(EQ(role, "approver"), NEQ(author_id, current_user))` ensures the approver is different from the creator.
    *   *Mechanism:* The `gp2f-security` module validates signatures and roles before any state change is accepted.

### 3. Immutable Financial Audit Trail
*   **Challenge:** Auditors require a tamper-proof history of who changed what and when.
*   **Solution:** GP2F's **Event-Sourced Architecture**.
    *   Every adjustment (e.g., "Bonus +$500") is a cryptographically signed `op_id`.
    *   The `gp2f-store` (Postgres) provides a linear, replayable history of all events.
    *   *UX:* An "Audit Mode" in the dashboard allows auditors to replay the state of the payroll ledger at any specific point in time using the CLI `replay` tool or the SDK's history view.

### 4. Real-Time Collaboration
*   **Challenge:** During "payroll close," multiple admins in different time zones may edit the same pay cycle simultaneously.
*   **Solution:** **CRDT (Yrs/Yjs)** conflict resolution.
    *   Two admins editing different employees in the same batch will have their changes merged automatically.
    *   Conflicts (e.g., two admins editing the *same* field) are resolved via the configured strategy (LWW or Transactional), with the UI showing a `<ReconciliationBanner>` or `<MergeModal>` if manual intervention is needed.

## User Experience (UX) & Process Flow

### Phase 1: Data Ingestion & Validation
1.  **HR System Sync:** Employee data is ingested into the dashboard.
2.  **Auto-Validation:** The `@gp2f/client-sdk` runs policies *locally* in the browser (WASM).
    *   *UX:* As a Payroll Admin types a bonus amount, the UI immediately validates it against the AST policy (e.g., "Bonus cannot exceed 10% of base salary without VP approval").
    *   *Benefit:* Instant feedback without server round-trips.

### Phase 2: Review & Adjustment
1.  **Interactive Grid:** Admins view a grid of payroll entries.
2.  **Collaborative Editing:** Admins see each other's cursors and updates in real-time (powered by `gp2f-broadcast`).
3.  **Conflict Handling:** If an admin tries to submit a batch that was modified by another user, the **Optimistic UI** updates immediately. If the server rejects the op (e.g., due to a constraint violation), the SDK's `UndoButton` or `MergeModal` appears to resolve the discrepancy.

### Phase 3: Approval Chain
1.  **Submission:** Admin submits the batch. State transitions to `PENDING_APPROVAL`.
2.  **Gatekeeper Policy:** The workflow engine (`gp2f-workflow`) checks if the batch total exceeds a threshold (e.g., $1M).
    *   *If < $1M:* Requires 1 Approver.
    *   *If >= $1M:* Requires 2 Approvers (Finance Director + VP).
3.  **Signing:** Approvers use their cryptographic keys (managed transparently by the SDK) to sign the approval `op_id`.

### Phase 4: Finalization & Export
1.  **Locking:** The state transitions to `LOCKED`. No further edits are permitted by the AST.
2.  **Export:** The canonical state is exported to the banking system.

## Technical Architecture

*   **Frontend:** React SPA using `@gp2f/client-sdk`.
    *   *Offline Support:* Enabled for regional offices with poor connectivity. Ops are queued in IndexedDB and flushed upon reconnection.
*   **Backend:** Node.js service using `@gp2f/server`.
    *   Acts as the policy decision point (PDP).
    *   Integrates with existing HRIS (Workday/BambooHR) via standard REST APIs.
*   **Data Store:** PostgreSQL (via `gp2f-store`) for the immutable event log.

## Success Metrics
*   **Payroll Accuracy:** 100% adherence to encoded AST policies.
*   **Processing Time:** Reduction in "payroll close" window from 5 days to 2 days due to real-time collaboration.
*   **Audit Compliance:** Zero findings in annual financial audits regarding data integrity.
