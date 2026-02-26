# 04_network_and_websockets.md

## Architecture Review: Network Layer & WebSockets

### Overview
This layer handles the high-throughput, real-time communication between clients and the server. It uses Axum 0.7 for efficient WebSocket handling, Redis PubSub for broadcasting updates across server nodes, and integrates Wasmtime for executing policy checks on incoming messages.

### Pros
*   **High Performance:** Axum (based on Hyper/Tokio) is one of the fastest web frameworks for Rust, making it highly suitable for holding thousands of open WebSocket connections with low overhead.
*   **Scalability:** Redis PubSub allows horizontal scaling of server nodes. A client can connect to any node and receive updates relevant to their tenant/document seamlessly.
*   **Real-Time:** WebSockets provide full-duplex communication, essential for the "multiplayer" aspect of GP2F, minimizing protocol overhead compared to polling.

### Cons & Risks
*   **Connection State Management:** Managing thousands of stateful WebSocket connections is complex (heartbeats, graceful shutdown, phantom connections) and prone to resource leaks.
*   **Head-of-Line Blocking:** If a single Wasmtime evaluation takes too long, it could block the processing of other messages on that connection or thread if not properly offloaded to a blocking thread pool.
*   **Redis Bottleneck:** At very high scale (10k+ users per tenant all active), Redis PubSub could become a throughput bottleneck if the message volume is extreme.
*   **Thundering Herd:** If a server node restarts or a network partition heals, thousands of clients reconnecting simultaneously can DDoS the authentication and initial sync endpoints.

### Single Points of Failure (SPOF)
*   **Redis:** If the Redis instance fails, real-time synchronization between users on different server nodes stops, effectively breaking the multiplayer experience.
*   **Load Balancer:** Misconfiguration in sticky sessions or WebSocket timeouts at the Load Balancer level (e.g., Nginx, ALB) can break long-lived connections or cause uneven distribution.

## Testing Strategy

### Load Testing (k6)
We need to prove the system can handle 10,000 concurrent users per tenant with < 5ms server p99 latency.

*   **Scenario Design:** Create a k6 script that mimics realistic user behavior: connect, join room, emit random ops (text edits, button clicks) at human speed, and receive broadcasts.
*   **Ramp-Up:** Gradually ramp up from 0 to 10,000 users over 10 minutes. Monitor CPU/Memory usage on the server and Redis latency to identify inflection points.
*   **Latency Metrics:** Measure `ws_connecting` time and "Time to Ack" (time from sending `op_id` to receiving `ACCEPT` or `REJECT`). Assert that p99 latency remains under 5ms.
*   **Stability:** Run a soak test (steady load) for 24 hours to check for memory leaks in the Rust server or Wasmtime runtime handles.

### Specific Test Cases & Scenarios
*   **Reconnect Storms:** Disconnect all 10,000 simulated users instantly, then reconnect them all within a 10-second window. Verify the server accepts connections without crashing and that the backlog is processed efficiently.
*   **Replay Protection:** Capture a valid signed message frame. Attempt to replay it thousands of times. Verify the server's Bloom filter or nonce tracker rejects duplicates instantly without triggering full policy re-evaluation.
*   **Slow Consumers:** Simulate clients that read from the WebSocket very slowly. Ensure the server buffers correctly and eventually disconnects slow consumers to protect server memory.
*   **Wasmtime Isolation:** Send a "poison pill" operation that triggers a computationally expensive loop in the WASM policy. Verify it times out strictly and doesn't affect the latency of other connected clients.

### Tools
*   **k6:** The primary load testing tool, utilizing its built-in WebSocket module for massive concurrency generation.
*   **Redis-benchmark:** To stress test the PubSub layer independently and tune buffer sizes.
*   **Wireshark/tcpdump:** For analyzing packet-level behavior and TCP window sizing during reconnect storms.
*   **Bloom Filter Analysis:** Custom scripts to verify the false positive rate and performance of the replay protection mechanism under load.
