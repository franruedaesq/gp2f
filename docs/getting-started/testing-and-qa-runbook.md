# Testing and QA Runbook

This runbook covers every test suite in the GP2F monorepo, including unit tests, integration tests, property-based tests, load tests, and resilience tests. Run the test suites in the order presented when validating a release candidate. Individual suites may be run independently for targeted validation during development.

---

## Prerequisites

All tests require the development infrastructure (Postgres, Redis, Temporal) to be running. See `docs/getting-started/local-dev-setup.md` for setup instructions. Additionally, the following tools are required for specific test suites:

**k6:** Load testing tool. Install from `https://k6.io/docs/get-started/installation/` or via Homebrew: `brew install k6`.

**Toxiproxy:** Network fault injection proxy. Install from `https://github.com/Shopify/toxiproxy/releases` or via Homebrew: `brew install toxiproxy`.

**cargo-nextest:** A faster Rust test runner. Install with: `cargo install cargo-nextest`.

**cargo-fuzz / cargo-proptest:** Property-based testing tools. Install with: `cargo install cargo-fuzz` and `cargo install proptest-cli` (the latter is optional; `cargo test` runs proptest targets).

---

## Suite 1: Rust Unit and Integration Tests

### Running All Rust Tests

```bash
cargo nextest run --workspace
```

`cargo-nextest` provides better parallel execution and output formatting than the default `cargo test`. On a 10-core laptop, the full Rust test suite completes in approximately 45 seconds.

To run only a specific crate:

```bash
cargo nextest run -p policy-core
cargo nextest run -p gp2f-server
```

### Understanding Test Organization

Tests are organized as follows:

- `policy-core/src/` — Unit tests for the AST evaluator, inline in source files as `#[cfg(test)]` modules.
- `policy-core/tests/` — Integration tests that load serialized AST fixtures and verify evaluation outcomes.
- `server/src/` — Unit tests for request handlers and business logic.
- `server/tests/` — Integration tests that start a test server against a real Postgres/Redis instance.
- `tests/` — End-to-end integration tests for cross-crate behavior.

### Expected Output

All tests should pass with zero failures. Any test failure in `policy-core` is a blocker for release; any test failure in `server` requires triage before release. Test output is captured per-test by `nextest`; failing tests display their output automatically.

---

## Suite 2: Property-Based Tests with cargo-proptest (WASM/Native Parity)

The most critical correctness property of GP2F is evaluator parity: the WASM target and the native Rust target must produce identical results for every possible input. Property-based testing with `proptest` generates thousands of random inputs to search for divergence.

### Running the Parity Tests

```bash
cargo test --test parity -- --include-ignored
```

The `--include-ignored` flag is required because parity tests are marked `#[ignore]` by default (they are slow and are run explicitly in CI, not on every `cargo test` invocation).

The parity test harness works as follows:

1. `proptest` generates a random `PolicyState` and a random `Intent` using derived `Arbitrary` implementations.
2. The test evaluates the AST natively (direct Rust call to `PolicyCore::evaluate`).
3. The test evaluates the same AST and state through Wasmtime (loading the WASM binary built by `wasm-pack`).
4. The test asserts that `native_result == wasm_result` for `permitted`, `trace`, and `snapshot_hash`.

```rust
// Example parity test (from tests/parity.rs)
proptest! {
    #[test]
    fn native_wasm_evaluate_parity(
        state in arb_policy_state(),
        intent in arb_intent()
    ) {
        let ast = load_test_ast();

        let native_result = PolicyCore::evaluate(&ast, &state, &intent)?;
        let wasm_result = wasmtime_evaluate(&ast, &state, &intent)?;

        prop_assert_eq!(native_result.permitted, wasm_result.permitted);
        prop_assert_eq!(native_result.trace, wasm_result.trace);
        prop_assert_eq!(native_result.snapshot_hash, wasm_result.snapshot_hash);
    }
}
```

### Configuration

The number of test cases generated per property is controlled by the `PROPTEST_CASES` environment variable. The default is 256 cases. For pre-release validation, run with 10,000 cases:

```bash
PROPTEST_CASES=10000 cargo test --test parity -- --include-ignored
```

This takes approximately 8 minutes. Any failure will print the minimized failing input (proptest performs shrinking automatically).

### Interpreting Failures

A failure indicates that the native and WASM evaluators produced different results for a specific input. The failure output will include the minimized `PolicyState` and `Intent` that triggered the divergence. File a P0 incident and do not release until the divergence is resolved. The root cause is typically a float/integer handling difference between native and WASM runtimes, or a serialization boundary issue in the Protobuf encoding.

---

## Suite 3: TypeScript Tests

### Running Frontend Unit Tests

```bash
cd client-sdk
pnpm test
```

This runs Vitest in single-pass mode. The test files are in `client-sdk/src/**/__tests__/`. Tests cover the Zustand store, the `op_id` generation logic, the IndexedDB queue adapter, and the WebCrypto KMS integration.

### Running TypeScript Type Checks

Type errors are not caught by the test runner. Run the type checker separately:

```bash
cd client-sdk
pnpm typecheck
```

This must pass with zero errors before any TypeScript changes are considered complete.

---

## Suite 4: WebSocket Load Testing with k6

This suite validates that the Axum WebSocket server can handle 10,000 concurrent connections with the required end-to-end latency of less than 16ms (P95).

### Prerequisites

The full stack must be running (Steps 6 and 7 of the setup guide). The load test is designed to run against `localhost:3000`. Do not run it against production.

### Running the Load Test

```bash
k6 run tests/load/websocket_load.js \
  --vus 10000 \
  --duration 60s \
  --out json=tests/load/results/$(date +%Y%m%d_%H%M%S).json
```

The test script (`tests/load/websocket_load.js`) simulates the following per-virtual-user flow:

1. Establish a WebSocket connection with a valid session token.
2. Send 5 `op_id` payloads at 1-second intervals.
3. Wait for `ACCEPT`/`REJECT` acknowledgments.
4. Record the round-trip latency for each operation.
5. Close the connection.

### Pass/Fail Thresholds

The test script enforces the following thresholds, which k6 evaluates automatically:

```javascript
export const options = {
  thresholds: {
    'ws_session_duration': ['p(95)<5000'],   // 95% of sessions complete in <5s
    'ws_msgs_received': ['count>45000'],      // At least 45k ACKs received (90% success)
    'gp2f_op_latency': ['p(95)<16'],          // P95 end-to-end latency <16ms
    'gp2f_op_latency': ['p(99)<50'],          // P99 end-to-end latency <50ms
  },
};
```

If any threshold fails, k6 exits with a non-zero status code and prints the failing thresholds. A threshold failure is a release blocker.

### Interpreting Results

The JSON output file contains per-metric time series data. Use the Grafana dashboard (`tests/load/grafana-dashboard.json`) to visualize the results. Import the dashboard to a local Grafana instance running via Docker:

```bash
docker compose up -d grafana
```

Then open `http://localhost:3001` and import the dashboard file.

**Typical bottlenecks at 10k connections:**

The Axum server is tuned with `SO_REUSEPORT` and a thread pool matching the CPU core count. If P95 latency exceeds the threshold, check: Temporal task queue depth (visible in the Temporal Web UI at `http://localhost:8088`), Redis pipeline throughput (`redis-cli info stats | grep instantaneous_ops`), and PostgreSQL connection pool saturation (`SELECT count(*) FROM pg_stat_activity`).

---

## Suite 5: Offline Queue Backpressure Testing with Toxiproxy

This suite validates that the GP2F client queue handles network partition scenarios correctly: operations queued during an outage are successfully replayed on reconnection, and queue overflow is handled gracefully without data loss.

### Starting Toxiproxy

```bash
toxiproxy-server &
```

Configure a proxy for the WebSocket server:

```bash
toxiproxy-cli create gp2f_ws --listen localhost:3001 --upstream localhost:3000
```

All Toxiproxy-based tests connect to `localhost:3001`, not `localhost:3000`. Update the `VITE_WEBSOCKET_URL` in `client-sdk/.env.test` to `ws://localhost:3001/ws`.

### Test Scenario 1: Clean Partition and Recovery

This scenario simulates a complete network partition followed by recovery. It validates that all operations queued during the partition are replayed and acknowledged after reconnection.

```bash
# Start the test (it will automatically configure Toxiproxy)
cargo test --test resilience -- partition_and_recovery --nocapture
```

The test performs the following steps:
1. Establish a connection through the Toxiproxy proxy.
2. Emit 50 `op_id`s and confirm they are acknowledged.
3. Add a `down` toxic to drop all traffic: `toxiproxy-cli toxic add gp2f_ws --type down --toxicName network_down`.
4. Emit 100 more `op_id`s (these are queued in IndexedDB).
5. Wait 5 seconds.
6. Remove the toxic: `toxiproxy-cli toxic remove gp2f_ws --toxicName network_down`.
7. Wait for all 100 queued `op_id`s to be replayed and acknowledged (timeout: 30 seconds).
8. Assert that the event log in Postgres contains exactly 150 events for the test session with no gaps in the sequence number.

### Test Scenario 2: Latency Degradation and Backpressure

This scenario validates behavior under high latency (simulating a congested mobile network). The queue should not overflow; the client should apply backpressure and slow `op_id` emission rather than dropping operations.

```bash
cargo test --test resilience -- latency_backpressure --nocapture
```

The test adds a `latency` toxic with a 2,000ms delay and 500ms jitter, then emits operations at the normal rate and verifies that no operations are dropped.

### Test Scenario 3: Bandwidth Throttling

```bash
# Add a bandwidth toxic limiting to 10KB/s (simulates a 2G connection)
toxiproxy-cli toxic add gp2f_ws --type bandwidth --toxicName slow_connection --rate 10

cargo test --test resilience -- bandwidth_throttle --nocapture

# Clean up
toxiproxy-cli toxic remove gp2f_ws --toxicName slow_connection
```

### Expected Results

All three scenarios must pass. The key invariant is: no `op_id` emitted by the client should be absent from the server's event log after the connection is restored, assuming the client's IndexedDB was not cleared. Any violation of this invariant is a data-loss bug and a P0 incident.

---

## Suite 6: End-to-End Tests

E2E tests run the complete browser-to-database path using Playwright.

```bash
cd client-sdk
pnpm playwright test
```

The E2E tests require the full stack to be running and the browser to be built:

```bash
pnpm build
```

Then:

```bash
pnpm playwright test --reporter=html
```

After the tests complete, open `playwright-report/index.html` to view the test report with screenshots of any failures.

---

## CI Pipeline Integration

The CI pipeline (`.github/workflows/ci.yml`) runs the following suites on every pull request:

1. `cargo nextest run --workspace` — must pass with zero failures.
2. `cargo test --test parity -- --include-ignored` with `PROPTEST_CASES=1000` — must pass.
3. `pnpm test` and `pnpm typecheck` — must pass.
4. `pnpm playwright test` — must pass.

The load test (k6) and resilience tests (Toxiproxy) are not run on every PR due to their duration. They are run on every merge to `main` and as part of the release candidate validation process.
