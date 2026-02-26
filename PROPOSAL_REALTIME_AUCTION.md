# Project Proposal 4: Nanosecond Energy Trading Exchange (NETE)

## 1. Project Definition
**NETE** is a high-frequency, peer-to-peer energy trading platform designed for microgrids (solar farms, battery storage). Unlike traditional offline-first use cases, NETE operates in a **strictly connected, ultra-low-latency environment**. It uses **gp2f** not for offline sync, but for its **deterministic state machine** capabilities to handle thousands of concurrent bid/ask orders per second without race conditions.

The core value is using the `gp2f` Policy Engine to enforce complex grid stability rules (e.g., "Voltage must stay within 5% variance") *before* a trade is executed, preventing blackout scenarios caused by algorithmic trading errors.

## 2. Objectives
*   **Zero-Race Execution:** Ensure that if two batteries bid for the same surplus energy packet, the winner is determined deterministically by timestamp and price, with zero ambiguity.
*   **Grid Safety Policy:** Embed physical grid constraints (Voltage, Frequency) directly into the trading logic as AST rules. A trade that violates physics is rejected instantly.
*   **High Throughput:** Process 10,000+ ops/sec by sharding the `gp2f` event store.
*   **Auditability:** Every kilowatt-hour traded is cryptographically signed and re-playable for regulatory audits (FERC compliance).

## 3. Product Requirements Document (PRD)

### 3.1 Frontend Screens (Web Dashboard - Real-Time)

#### Screen 1: The Order Book (Live Canvas)
*   **View:** Waterfall chart of Buy/Sell orders updating at 60fps.
*   **Tech:** Uses `gp2f` WebSocket subscription to stream `AcceptResponse` events directly to a WebGL canvas.
*   **Interaction:** "One-Click Trade" buttons.
    *   *Constraint:* If the user's account balance < Order Price, the local WASM policy disables the button instantly (0ms latency check).

#### Screen 2: Grid Stability Monitor
*   **View:** Heatmap of the microgrid voltage levels.
*   **Logic:** The `gp2f` engine subscribes to `MeterReading` ops.
    *   *Rule:* If `Voltage < 110V`, the policy triggers an automatic `EmergencyBuy` op to inject power from batteries. This logic runs on the *server* for authority but is mirrored on the client for transparency.

#### Screen 3: Algorithmic Strategy Builder
*   **View:** A visual node editor for traders to build their own automated bots.
*   **Output:** Generates a `gp2f` AST (JSON) that is uploaded to the server.
    *   *Example:* "IF Price < $0.10 AND BatteryLevel < 50% THEN Buy()."
*   **Safety:** The server sandboxes these user-submitted ASTs and runs them with strict resource limits (fuel usage) enforced by the `gp2f` runtime.

### 3.2 Backend Architecture (Rust/Axum + Redis)

#### Data Model (Proto)
*   **Aggregate:** `OrderBook`, `GridState`
*   **State:**
    ```protobuf
    message Order {
      string id = 1;
      string trader_id = 2;
      double price_per_kwh = 3;
      double quantity_kwh = 4;
      int64 expiration_ts = 5;
    }
    message GridState {
      double voltage = 1;
      double frequency = 2;
      double total_load = 3;
    }
    ```

#### Policy Definitions (AST) - The "Physics Engine"
*   **File:** `policies/grid_safety_v1.json`
*   **Logic (Sequential & Deterministic):**
    *   *Matching Engine:* A `REDUCE` operation over the `OrderBook` to find overlapping Price/Quantity.
    *   *Safety Check:* `Assert(Grid.Frequency > 59.8 && Grid.Frequency < 60.2)`.
    *   *Financial Check:* `Assert(Buyer.Balance >= Trade.TotalCost)`.
*   **Concurrency Strategy:** `TRANSACTIONAL`. If two orders conflict (e.g., buying the same unique asset), the second one is rejected with `RetryNeeded`.

### 3.3 Implementation Steps

#### Step 1: High-Performance WebSocket Server
*   Tune `gp2f_server` for high throughput. Disable persistence-to-disk for the hot path (use Redis Streams as the WAL) to minimize I/O latency.
*   Implement `Sharding` logic in the `EventStore` based on `Microgrid_ID`.

#### Step 2: The Matching Engine (Rust)
*   Implement a custom `NodeKind::MATCH_ORDERS` in `policy-core`.
    *   This node takes `[BuyOrders, SellOrders]` and returns `[Matches]`.
    *   This ensures the matching logic is part of the deterministic consensus, not a side effect.

#### Step 3: Real-Time Visualization
*   Build a React client using `react-three-fiber` for the order book.
*   Connect the `Gp2fClient` via WebSocket.
    *   **Optimization:** Use `client.subscribe_raw()` to bypass the overhead of full state reconstruction for the visualization layer (just visualize the deltas).

#### Step 4: Bot Sandbox
*   Allow users to `POST /policy/upload`.
*   Validate the AST depth and complexity (to prevent DoS) before accepting it.
*   Run these user policies in a separate `Wasmtime` instance on the server side, triggered by every `Tick` event.
