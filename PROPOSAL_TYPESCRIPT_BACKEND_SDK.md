# Proposal: TypeScript Backend SDK & Wrappers

**Objective:** Enable a TypeScript developer to build, configure, and extend the entire GP2F backend framework without writing Rust code.

Currently, adding a new `WorkflowDefinition` requires writing a Rust struct and recompiling the server binary. This proposal evaluates three architectural approaches to expose the core engine to the Node.js/TypeScript ecosystem.

---

## 1. Problem Statement
The current architecture is a monolithic Rust binary:
- **Pros:** Maximum performance, memory safety, zero-overhead FFI.
- **Cons:** High barrier to entry. TypeScript developers can only work on the frontend client SDK.
- **Goal:** Allow `import { Server, Workflow } from '@gp2f/server';` in a standard Node.js project.

---

## 2. Feasibility Evaluation

We evaluated three potential architectures for enabling TypeScript backend development.

### Option A: Node.js Native Bindings (Recommended)
**Approach:** Wrap the core Rust engine (`policy-core`, `server`) into a native Node.js addon using `napi-rs` or `neon`.
**Workflow:**
1. Developer installs `npm install @gp2f/server`.
2. Developer writes a standard `index.ts` file.
3. The TS code calls into the high-performance Rust engine for execution.

**Example DX:**
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

| Metric | Rating | Notes |
|--------|--------|-------|
| **Performance** | ⭐⭐⭐⭐ | High. The core loop stays in Rust; overhead is only at the TS<->Rust boundary. |
| **Determinism** | ⭐⭐⭐ | Good, but requires strict sandboxing of user-provided TS logic (e.g. `Date.now()`). |
| **Complexity** | ⭐⭐⭐ | Medium. Requires maintaining a `napi-rs` bridge crate. |

### Option B: WASM Plugin Architecture
**Approach:** The Rust server remains the host. Developers write logic in AssemblyScript or TypeScript (compiled to WASM via Javy/QuickJS) and upload `.wasm` modules.
**Workflow:**
1. Developer writes `workflow.ts`.
2. Compiles to `workflow.wasm`.
3. Configures the Rust server to load this module at runtime.

| Metric | Rating | Notes |
|--------|--------|-------|
| **Performance** | ⭐⭐⭐ | Good (Wasmtime is fast), but serialization overhead for complex objects. |
| **Determinism** | ⭐⭐⭐⭐⭐ | Excellent. The WASM sandbox guarantees deterministic execution. |
| **DX** | ⭐⭐ | Poor. Requires a complex build toolchain and lacks full Node.js API access. |

### Option C: Sidecar / RPC Model
**Approach:** A thin "Runner" in Node.js handles workflow logic and communicates with the Rust engine via gRPC or Redis streams.
**Workflow:**
1. Start the Rust `gp2f-engine` process.
2. Start a Node.js `gp2f-worker` process.
3. Workers poll for tasks and report results.

| Metric | Rating | Notes |
|--------|--------|-------|
| **Performance** | ⭐⭐ | Lowest. Network/IPC latency for every activity execution. |
| **Determinism** | ⭐⭐ | Hard to enforce. The Node process has full system access. |
| **Complexity** | ⭐⭐⭐⭐ | High. Distributed system complexity (two processes to manage). |

---

## 3. Detailed Proposal: Node.js Native Bindings

We recommend **Option A (Node.js Bindings)** as the primary solution. It provides the best balance of Developer Experience (DX) and performance, allowing a TS developer to "own" the backend.

### Phase 1: Core Bindings
Create a new crate `gp2f-node` using `napi-rs`.
- Expose `WorkflowDefinition` builder pattern.
- Expose `Server` configuration (port, database connection).
- Map Rust `AstNode` structs to TypeScript interfaces.

### Phase 2: Activity Execution Bridge
Allow TypeScript functions to be registered as activity handlers.
- The Rust engine calls a JS callback when an activity is triggered.
- Use `napi::threadsafe_function` to handle async TS promises.

### Phase 3: CLI Tools
- `gp2f-cli` (npm) to scaffold new projects: `npx gp2f create my-app`.
- Bundled binary distribution for Linux/macOS/Windows.

## 4. Risks & Mitigations

**Risk:** TypeScript user code is not deterministic (e.g., using `Math.random()`).
**Mitigation:**
- Provide a `Context` object to all handlers with deterministic helpers (`ctx.random()`, `ctx.now()`).
- Use ESLint rules to ban non-deterministic built-ins.
- Alternatively, run user handlers inside a V8 Isolate with strict limits (Cloudflare Workers style), though this increases complexity.

**Risk:** FFI Overhead.
**Mitigation:**
- Batch operations where possible.
- Keep the "hot loop" (reconciliation logic) entirely in Rust; only call into TS for custom business logic or side effects.

## 5. Conclusion
Implementing a **Node.js SDK (`@gp2f/server`)** backed by the existing Rust core is highly feasible. It would democratize the framework, allowing any TypeScript developer to build, deploy, and maintain GP2F applications without learning Rust, while retaining the performance and safety guarantees of the underlying engine.
