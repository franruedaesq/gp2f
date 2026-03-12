# Publishing and Testing Guide

This document outlines how to publish the GP2F packages to npm, run tests, and understand the infrastructure requirements.

## 1. How to Publish to npm

The repository contains two main packages that are published to npm:

### `@gp2f/client-sdk` (TypeScript Client)
Located in `client-sdk/`. This is a standard TypeScript project.

1.  Navigate to the directory:
    ```bash
    cd client-sdk
    ```
2.  Install dependencies and build:
    ```bash
    npm install
    npm run build
    ```
3.  Publish:
    ```bash
    npm publish --access public
    ```

### `@gp2f/server` (Node.js Bindings)
Located in `gp2f-node/`. This package contains native Node.js bindings for the Rust policy engine, built using `napi-rs`.

**Important:** Because this package includes a compiled binary (`.node` file), simply running `npm publish` locally will only publish the binary for your current architecture (e.g., macOS ARM64).

To publish for all supported platforms (Linux x64, Windows x64, macOS x64/ARM64), you should typically use the CI workflow (see `.github/workflows/ci.yml`).

**Local Publishing (Single Architecture):**
If you only need to publish for your current machine's architecture:

1.  Navigate to the directory:
    ```bash
    cd gp2f-node
    ```
2.  Build the native addon:
    ```bash
    npm install
    npm run build  # This calls 'cargo build --release' internally via napi-rs
    ```
3.  Publish:
    ```bash
    npm publish --access public
    ```

---

## 2. How to Run Tests

### Rust Tests (Backend & Core Logic)
The majority of the business logic, including the policy engine, reconciler, and event store, is written in Rust.

To run all Rust tests across the workspace:
```bash
cargo test --workspace
```

This includes:
- Unit tests for the policy engine (`policy-core`).
- Integration tests for the server (`server/tests`), including AI/LLM flows mocked with `wiremock`.

### Node.js Binding Tests
To verify that the Node.js bindings correctly wrap the Rust logic:

```bash
cd gp2f-node
npm install
npm test
```
This runs Jest tests that load the compiled `.node` addon and execute scenarios.

### Client SDK Tests
To run the client-side tests:

```bash
cd client-sdk
npm install
npm test
```

---

## 3. Infrastructure Requirements for Testing

**Question:** *Do I need Redis, Databases, Docker, Kubernetes, etc., to run tests?*

**Answer: No.**

You generally do **not** need any external infrastructure running to execute the standard test suite (`cargo test` or `npm test`).

-   **In-Memory Defaults:** The default test configuration uses in-memory implementations for the event store and other components.
-   **Mocking:** Integration tests (like `server/tests/ai_e2e.rs`) use libraries like `wiremock` to mock external HTTP services (OpenAI/Anthropic) and `wiremock` avoids the need for a real LLM provider API key.
-   **Local Development:** The system is designed to be "zero-config" by default for development ("Demo Mode"), where it runs entirely in-memory.

**When do you need infrastructure?**
You only need Redis and Postgres if you explicitly enable the production feature flags (`postgres-store`, `redis-broadcast`) and want to run full production-simulation integration tests or run the server in a production mode that persists data to disk.

---

## 4. What Else Do I Need to Know?

### WASM Optimization (`wasm-opt`)
If you are building the `policy-core` for usage in a browser (WASM), you need `wasm-opt` (from the `binaryen` toolset) to optimize the binary size.
-   The CI pipeline checks that the WASM binary is under a certain size budget (e.g., 512KB).
-   If you see build errors related to `wasm-opt`, ensure it is installed or skip the WASM build steps locally if you are only working on the server.

### Deterministic Workflows
The workflow engine relies on **determinism**.
-   **Linting:** There is a CI job (`workflow-lint`) that scans `server/src/workflow*.rs` and `pilot_workflows.rs` for non-deterministic code (like `std::time::Instant::now()` or `rand::thread_rng()`).
-   **Rule:** Never use system time or randomness directly inside a workflow definition. Always use the deterministic context provided by the framework (or pass inputs in via the operation payload).

### N-API (Node.js Addons)
The `gp2f-node` package uses `napi-rs`.
-   It acts as a bridge. When you change Rust code in `server/` or `policy-core/`, you must rebuild the `gp2f-node` package (`npm run build`) for those changes to be reflected in the Node.js layer.
