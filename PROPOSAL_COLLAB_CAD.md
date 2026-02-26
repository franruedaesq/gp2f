# Project Proposal 5: Collaborative Architectural Design System (CADS)

## 1. Project Definition
**CADS** is a real-time, browser-based BIM (Building Information Modeling) platform for large-scale architectural firms. It is the architectural equivalent of Google Docs, but for complex 3D structures.

It uses **gp2f**'s deep integration with `yrs` (the Rust port of Yjs) to manage the state of millions of 3D objects (walls, windows, HVAC ducts) concurrently. Multiple architects edit the same building model simultaneously, seeing each other's changes instantly, without locking files or checking out versions.

## 2. Objectives
*   **Massive Scale Collaboration:** Support 50+ concurrent users editing a 10GB model without lag.
*   **Conflict-Free Editing:** Use CRDTs to merge changes intelligently (e.g., Architect A moves a wall, Architect B changes its material -> Result: Moved wall with new material).
*   **Constraint Solving:** Enforce building codes (e.g., "Fire Exit width >= 1.2m") in real-time using `gp2f` policies. Invalid edits are highlighted immediately.
*   **Zero-Save:** Every stroke is an `op` that is persisted instantly. No "File > Save" needed.

## 3. Product Requirements Document (PRD)

### 3.1 Frontend Screens (React + Three.js + WebAssembly)

#### Screen 1: The 3D Viewport
*   **View:** Interactive 3D render of the building.
*   **Tech:** `react-three-fiber` rendering `gp2f` state.
    *   **Optimization:** The `CrdtDoc` structure maps directly to the scene graph.
*   **Interaction:** Click-and-drag to move objects.
    *   *Constraint:* If an object intersects another (collision), the policy engine flags it Red.
*   **Collaboration:** 3D cursors of other users visible in real-time.

#### Screen 2: Properties Panel & Chat
*   **View:** Sidebar showing selected object attributes (Material, Height, Cost).
*   **Logic:** Two-way binding to `gp2f` state. Editing a value emits an `op`.
*   **Chat:** Threaded comments attached directly to 3D coordinates.

#### Screen 3: Version History Time-Machine
*   **View:** Scrubbable timeline of the project.
*   **Action:** "Restore to Yesterday 4:00 PM".
*   **Tech:** Because `gp2f` is an event-sourced log, restoring is just replaying up to `timestamp - X`.

### 3.2 Backend Architecture (Rust/Axum)

#### Data Model (Proto + Yrs)
*   **Aggregate:** `BuildingModel`
*   **State:**
    *   Uses `yrs::Doc` internally for the heavy lifting.
    *   The `gp2f` state is a wrapper around the binary Yjs update.
    ```protobuf
    message BuildingUpdate {
      bytes yjs_update = 1; // The binary diff
      string user_id = 2;
    }
    ```

#### Policy Definitions (AST) - The "Building Code"
*   **File:** `policies/fire_code_nyc.json`
*   **Logic (Geometric Constraints):**
    *   *Egress Width:* `Assert(Object.Type == "Door" => Object.Width >= 1.2)`.
    *   *Material Safety:* `Assert(Object.Floor > 10 => Material.FireRating >= "2hr")`.
*   **Implementation:** These checks run *after* the CRDT merge. If a rule is violated, the system doesn't reject the edit (to allow temporary invalid states during drafting) but marks the object with a `LintError` annotation.

### 3.3 Implementation Steps

#### Step 1: Integrate `yrs` into `gp2f` State
*   Ensure `policy-core` can serialize/deserialize `yrs::Update` blobs.
*   Implement a `YrsMerge` strategy in the `Reconciler`.

#### Step 2: High-Performance Broadcasting
*   Optimize the WebSocket server to broadcast binary updates efficiently.
*   Implement "Awareness" protocol (ephemeral state for cursors) alongside the durable state.

#### Step 3: Client-Side Scene Graph Binding
*   Create a React hook `useBuildingState()` that binds specific branches of the CRDT (e.g., "Level 2 Walls") to Three.js meshes.
*   Use `mobx` or similar for reactive updates to avoid re-rendering the whole scene.

#### Step 4: Building Code Validation
*   Write the validation logic in Rust (using `parry3d` for collision detection).
*   Compile to WASM.
*   Run the validator in a WebWorker to avoid blocking the UI thread.
