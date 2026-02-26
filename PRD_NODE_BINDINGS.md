# Product Requirements Document: Node.js Native Bindings

## Objective
Enable TypeScript developers to utilize the high-performance Rust engine (`policy-core`, `server`) seamlessly within a Node.js environment, abstracting the complexity of Rust while maintaining performance and determinism.

## Approach
Wrap the core Rust engine into a native Node.js addon using `napi-rs`. This allows the heavy lifting to remain in Rust while providing an idiomatic TypeScript API.

## Target Developer Experience
```typescript
import { GP2FServer, Workflow, Activity } from '@gp2f/server';

const server = new GP2FServer({ port: 3000 });

const approvalWorkflow = new Workflow('document-approval')
  .addActivity('review', {
    policy: { kind: 'AND', ... }, // JSON AST or fluent builder
    onExecute: async (ctx) => {
      // Custom TS logic runs here (bridged to Rust)
    }
  });

server.register(approvalWorkflow);
await server.start();
```

## Implementation Phases

### Phase 1: Infrastructure and Scaffolding
1.  **Initialize Bridge Crate:** Create a new Rust crate (e.g., `gp2f-node`) configured with `napi-rs`.
2.  **Workspace Integration:** Add the new crate to the existing Cargo workspace.
3.  **Build Configuration:** Configure `build.rs` and `package.json` to handle cross-compilation targets (Linux, macOS, Windows).
4.  **Type Generation:** Configure automatic generation of `.d.ts` definition files from Rust structs.

### Phase 2: Core Data Types and Policy Binding
1.  **JSON AST Mapping:** Create `napi` compatible structs that map to `policy-core`'s internal policy definitions.
2.  **Context Serialization:** Implement efficient serialization/deserialization for the execution context passed between Rust and Node.js.
3.  **Policy Builder API:** (Optional) Implement helper functions in Rust exposed to JS to construct policy ASTs programmatically instead of raw JSON.

### Phase 3: Workflow and Activity API
1.  **Workflow Class:** Implement the `Workflow` struct in Rust utilizing `#[napi]` macros.
    *   Constructor accepting a unique identifier.
    *   Methods for configuration (timeouts, retries).
2.  **Activity Definition:** Implement the `addActivity` method.
    *   Accept configuration objects.
    *   Accept policy definitions.
3.  **Async Callback Bridge:** Implement the mechanism to invoke Node.js `async` functions (`onExecute`) from the Rust runtime.
    *   Handle `JsFunction` storage.
    *   Convert Rust `Future` results back to Node.js Promises.
    *   Ensure thread safety (Send/Sync) when crossing the FFI boundary.

### Phase 4: Server Integration
1.  **Server Wrapper:** Create the `GP2FServer` class exposing the underlying Axum/Tower server logic.
2.  **Configuration Mapping:** Map JS configuration objects (port, database options) to Rust `ServerConfig` structs.
3.  **Registration Logic:** Implement `server.register(workflow)` to store the hybrid Rust/JS workflow definitions in the engine.
4.  **Lifecycle Management:** Expose `start()`, `stop()`, and graceful shutdown methods to Node.js.

### Phase 5: Determinism and Sandboxing
1.  **Context Isolation:** Ensure that `onExecute` callbacks receive a strictly defined context object.
2.  **Documentation:** Clearly document strict strictures for user code (e.g., avoiding `Date.now()`, global state) to maintain determinism, as the Rust engine cannot enforce this inside the V8 runtime.

### Phase 6: Testing and Distribution
1.  **Unit Testing:** Write `vitest` or `jest` tests that import the native addon and verify basic functionality.
2.  **Integration Testing:** Create a sample Node.js application implementing the "Example DX" to verify end-to-end flow.
3.  **Artifact Generation:** Configure GitHub Actions to build binaries for all supported platforms/architectures.
4.  **NPM Publishing:** Setup scripts to publish the package under `@gp2f/server`.
