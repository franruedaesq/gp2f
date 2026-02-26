# Project Proposal 3: Algorithmic Crop Insurance Adjuster (ACIA)

## 1. Project Definition
**ACIA** is a specialized field application for agricultural insurance adjusters. It allows them to assess crop damage (hail, drought, flood) on-site, calculate payouts instantly using complex parametric formulas defined in `gp2f` policies, and issue settlement offers immediately—even without internet connectivity.

By using **gp2f**, ACIA ensures that payout calculations are deterministic (identical results on the tablet and the server), fraud detection rules (like geofencing) are applied in real-time before data leaves the device, and all adjustment steps are cryptographically signed for regulatory audits.

## 2. Objectives
*   **Instant Adjudication:** Calculate complex payout formulas (e.g., `(YieldLoss * BasePrice) - Deductible`) locally in < 50ms.
*   **Geospatial Validation:** Enforce strict geofencing rules (e.g., "Adjuster must be standing inside the insured polygon") using custom AST geometry functions.
*   **Offline Capability:** Fully functional in remote rural areas; syncs claims when back in range.
*   **Transparency:** Every calculation step is trace-logged by the policy engine for dispute resolution.

## 3. Product Requirements Document (PRD)

### 3.1 Frontend Screens (iPad/Tablet)

#### Screen 1: Claims Queue & Map
*   **View:** Map showing assigned claims pin-dropped on field locations.
*   **Filter:** "High Priority" (Severe damage reports), "Pending Review".
*   **Action:** Tap a pin to "Start Adjustment".

#### Screen 2: Field Assessment
*   **Input:** Record GPS coordinates (auto-captured), enter sample counts (e.g., plants per row), select damage type.
*   **Validation:** AST rules block submission if mandatory evidence is missing or if the GPS coordinate is outside the field boundary (using Ray Casting algorithm in `policy-core`).
    *   *Feedback:* "Error: You are 50m outside the insured plot boundary."

#### Screen 3: The Payout Calculator
*   **Display:** Real-time calculation of the indemnity amount.
*   **Logic:** The `gp2f` WASM engine executes the `Policy.PayoutFormula` against the collected data.
    *   *Example:* `Payout = MIN(CoverageLimit, (ExpectedYield - ActualYield) * Price)`
    *   This formula is versioned. If the policy changes mid-season, the app updates the JSON AST and re-calculates correctly.
*   **Trace:** A "Show Math" button reveals the exact AST execution path (e.g., `BasePrice(5.00) * Acres(100) * Damage(0.6) = $300,000`).

#### Screen 4: Settlement Offer
*   **Action:** "Generate Offer".
*   **Output:** A digitally signed PDF (generated locally) with the calculation trace.
*   **Signature:** Farmer signs on-screen. The signature is captured as an `op` payload.

### 3.2 Backend Architecture (Rust/Axum)

#### Data Model (Proto)
*   **Aggregate:** `Claim`
*   **State:**
    ```protobuf
    message ClaimState {
      string policy_id = 1;
      double latitude = 2;
      double longitude = 3;
      repeated Evidence photos = 4;
      double assessed_damage_pct = 5;
      double final_payout = 6;
      string status = 7; // OPEN, ASSESSED, OFFERED, SETTLED
    }
    ```

#### Policy Definitions (AST) - The "Parametric Engine"
*   **File:** `policies/corn_hail_2024.json`
*   **Logic (Complex Math):**
    *   `Payout = MIN(CoverageLimit, (ExpectedYield - ActualYield) * Price)`
    *   Implemented using `NodeKind::CALL` for math functions or composed arithmetic nodes.
*   **Fraud Rules (Geometry):**
    *   `Assert(GeometryContains(Field.Polygon, Claim.GPS))`
    *   This requires implementing a `NodeKind::CALL` handler in Rust that performs point-in-polygon checks.

### 3.3 Implementation Steps

#### Step 1: Define Parametric Policies
*   Create the "Corn Hail" policy using the `gp2f` AST builder.
*   Register the `geometry_contains` function in the `Evaluator` context on both client (TS/WASM) and server (Rust).

#### Step 2: Implement Geospatial Logic
*   In `policy-core/src/evaluator.rs`, add a `Call` handler for `geometry_contains`.
*   Implement the Ray Casting algorithm in pure Rust (no external crate needed for simple polygons) so it compiles to WASM easily.

#### Step 3: Server-Side Settlement Verification
*   In `gp2f_server`, listen for the `SignSettlement` op.
*   Re-run the `PayoutFormula` policy on the server state to verify the amount matches the client's claim.
*   If valid -> `ACCEPT` and trigger payment via external API (mocked for demo).
*   If invalid -> `REJECT` (Client was tampered with or policy changed).
