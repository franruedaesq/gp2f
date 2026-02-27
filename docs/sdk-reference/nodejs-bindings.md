# Node.js Native Bindings: `@gp2f/server`

The `gp2f-node` directory contains the **`@gp2f/server`** npm package, which provides native Node.js bindings for the GP2F policy engine and workflow runtime, powered by [napi-rs](https://napi.rs/). The Rust evaluator runs directly inside the Node.js process as a compiled `.node` addon — no WASM overhead, no child processes, no HTTP round-trips.

---

## Installation

```bash
cd gp2f-node
npm install
```

The package ships pre-built binaries for the following platforms. If a pre-built binary is not available for your platform, the package falls back to building from source (Rust ≥ 1.78 required).

| Platform | Architecture |
|----------|-------------|
| Linux (glibc) | x86\_64, aarch64 |
| Linux (musl) | x86\_64 |
| macOS | x86\_64, aarch64 (Apple Silicon) |
| Windows | x86\_64 |

---

## Quick Start

```typescript
import { evaluate, evaluateWithTrace, Workflow, GP2FServer, p } from '@gp2f/server';

// 1. Build a policy with the fluent builder
const policy = p.and(
  p.field('/role').eq('admin'),
  p.exists('/session/token'),
);

// 2. Evaluate it
const allowed = evaluate(policy, { role: 'admin', session: { token: 'abc' } }); // => true

// 3. Build a workflow and start a server
const wf = new Workflow('my-workflow');
wf.addActivity(
  'approve',
  { policy: p.field('/role').eq('reviewer') },
  async (ctx) => { console.log('approved by', ctx.tenantId); }
);

const server = new GP2FServer({ port: 3000 });
server.register(wf);
await server.start();
// Listening on http://127.0.0.1:3000
```

---

## API Reference

### `evaluate(policy, state): boolean`

Evaluates a policy AST against a state object and returns a boolean decision.

**Parameters:**

| Parameter | Type | Description |
|-----------|------|-------------|
| `policy` | `AstNode` | Root node of the policy AST. |
| `state` | `unknown` | Arbitrary JSON object representing the current state. |

**Returns:** `true` when the policy permits the operation, `false` otherwise.

```typescript
import { evaluate } from '@gp2f/server';

evaluate({ kind: 'LiteralTrue' }, {});
// => true

evaluate({ kind: 'LiteralFalse' }, {});
// => false

evaluate(
  {
    kind: 'And',
    children: [
      { kind: 'Field', path: '/role', value: 'clinician' },
      { kind: 'Exists', path: '/consent_token' },
    ],
  },
  { role: 'clinician', consent_token: 'abc123' }
);
// => true
```

---

### `evaluateWithTrace(policy, state): EvalResult`

Evaluates a policy AST and returns the boolean decision together with a human-readable evaluation trace. Use this for debugging and audit logging.

**Returns:**

```typescript
interface EvalResult {
  result: boolean;
  trace: string[]; // one entry per evaluated node
}
```

```typescript
import { evaluateWithTrace } from '@gp2f/server';

const { result, trace } = evaluateWithTrace(
  {
    kind: 'And',
    children: [
      { kind: 'Field', path: '/role', value: 'admin' },
      { kind: 'LiteralTrue' },
    ],
  },
  { role: 'admin' }
);
// result: true
// trace:  ['[0] And', '[1] Field /role == admin => true', '[2] LiteralTrue => true', '[0] And => true']
```

---

### `NodeKind`

Union type of all supported AST node kinds:

```typescript
type NodeKind =
  | 'LiteralTrue' | 'LiteralFalse'
  | 'And' | 'Or' | 'Not'
  | 'Eq' | 'Neq'
  | 'Gt' | 'Gte' | 'Lt' | 'Lte'
  | 'In' | 'Contains'
  | 'Exists'
  | 'Field'
  | 'VibeCheck'
  | 'Call'
```

---

### `AstNode`

```typescript
interface AstNode {
  kind: NodeKind;
  children?: AstNode[];  // for composite operators
  path?: string;         // JSON-pointer path for Field / Exists nodes
  value?: string;        // scalar value for leaf nodes (e.g. "admin", "42")
  callName?: string;     // external function name for Call nodes
}
```

**Examples:**

```typescript
// Always true
{ kind: 'LiteralTrue' }

// Role check
{ kind: 'Field', path: '/role', value: 'admin' }

// Field existence
{ kind: 'Exists', path: '/session/token' }

// Logical AND
{
  kind: 'And',
  children: [
    { kind: 'Field', path: '/role', value: 'admin' },
    { kind: 'Exists', path: '/session/token' },
  ],
}

// Numeric comparison (GTE)
{
  kind: 'Gte',
  children: [
    { kind: 'Field', path: '/score' },
    { kind: 'Field', value: '80' },
  ],
}

// Vibe check (AI confidence gate)
{ kind: 'VibeCheck', value: 'frustrated', path: '0.8' }
// true when intent == 'frustrated' AND confidence >= 0.8
```

---

### `class Workflow`

A GP2F workflow definition. Build a `Workflow`, register activities with their policy ASTs, then pass it to `GP2FServer.register`.

#### Constructor

```typescript
new Workflow(workflowId: string)
```

#### Properties

| Property | Type | Description |
|----------|------|-------------|
| `id` | `string` | The workflow identifier (read-only). |
| `activityCount` | `number` | Number of registered activities (read-only). |

#### `addActivity(name, config, onExecute?): string`

Adds an activity to the workflow. Activities are executed in registration order.

```typescript
wf.addActivity(
  'collect-vitals',
  {
    policy: {
      kind: 'And',
      children: [
        { kind: 'Field', path: '/role', value: 'clinician' },
        { kind: 'Exists', path: '/patient_id' },
      ],
    },
    compensationRef: 'rollback-vitals', // optional Saga compensation
    isLocal: true,                      // optional: skip Temporal persistence
  },
  async (ctx: ExecutionContext) => {
    const state = JSON.parse(ctx.stateJson);
    console.log(`Collecting vitals for patient ${state.patient_id}`);
  }
);
```

**`onExecute` callback** receives an `ExecutionContext`:

```typescript
interface ExecutionContext {
  instanceId: string;   // unique workflow execution identifier
  tenantId: string;     // tenant/organisation
  activityName: string; // name of the current activity
  stateJson: string;    // JSON-encoded state document
}
```

#### `dryRun(state): boolean`

Evaluates all activity policies against `state` without executing any callbacks. Returns `true` only if every activity policy is satisfied.

```typescript
const ok = wf.dryRun({ role: 'clinician', patient_id: 'pt-001' });
// => true (both activity policies pass)
```

---

### `class GP2FServer`

Hosts an Axum-backed HTTP server that exposes the registered workflows over a REST API.

#### HTTP API

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/health` | Health check, returns `"ok"`. |
| `POST` | `/workflow/run` | Execute the next activity of a workflow. |
| `POST` | `/workflow/dry-run` | Evaluate policies without side-effects. |

#### Constructor

```typescript
new GP2FServer(config?: ServerConfig)

interface ServerConfig {
  port?: number; // default: 3000
  host?: string; // default: "127.0.0.1"
}
```

#### Properties

| Property | Type | Description |
|----------|------|-------------|
| `port` | `number` | The port the server is listening on (read-only). |
| `isRunning` | `boolean` | `true` while the server is running (read-only). |

#### Methods

**`register(workflow: Workflow): void`**

Registers a workflow with the server. Can be called before or after `start()`.

**`start(): Promise<void>`**

Starts the HTTP server. Resolves once the TCP listener is bound.

**`stop(): Promise<void>`**

Stops the server gracefully, waiting for in-flight requests to drain.

#### Full example

```typescript
import { GP2FServer, Workflow } from '@gp2f/server';

const server = new GP2FServer({ port: 8080, host: '0.0.0.0' });

// Medical triage workflow
const triage = new Workflow('medical-triage');
triage.addActivity('register-patient',   { policy: { kind: 'Field', path: '/role', value: 'clinician' } });
triage.addActivity('collect-vitals',     { policy: { kind: 'Field', path: '/role', value: 'clinician' } });
triage.addActivity('assign-triage-level',{ policy: { kind: 'Field', path: '/role', value: 'clinician' } });
triage.addActivity('finalize-intake',    { policy: { kind: 'Field', path: '/role', value: 'admin' } });

server.register(triage);
await server.start();

// POST /workflow/run
// { "workflowId": "medical-triage", "instanceId": "pt-001", "tenantId": "hospital-a", "state": { "role": "clinician" } }

process.on('SIGTERM', () => server.stop());
```

---

## Fluent Policy Builder

The `@gp2f/server` package exports a **fluent policy builder** (`PolicyBuilder` / `p`) as a chainable alternative to writing raw JSON AST objects.

```javascript
const { p } = require('@gp2f/server');

// Simple field check
const policy = p.field('/role').eq('admin');

// Logical AND of multiple conditions
const policy = p.and(
  p.field('/role').eq('clinician'),
  p.exists('/patient_id'),
  p.not(p.field('/patient/status').eq('discharged')),
);

// Role allow-list using `in`
const policy = p.field('/role').in(['admin', 'editor', 'reviewer']);

// Numeric threshold
const policy = p.field('/score').gte(80);

// Vibe Engine gate
const policy = p.vibe('frustrated').withConfidence(0.8).build();
```

The builder output is a plain `AstNode` object and can be passed anywhere a policy AST is accepted—`evaluate()`, `evaluateWithTrace()`, `addActivity()`, or stored as JSON.

See the full [Fluent Policy Builder Reference](policy-builder.md) for all operators and examples.

---

## Running the Tests

```bash
cd gp2f-node
npm test                          # contract tests (no native build required)
GP2F_NATIVE_BUILD=1 npm test      # full tests including native addon
```

Contract tests validate the TypeScript declarations and package structure without requiring a compiled `.node` binary. Native addon tests exercise the actual Rust evaluator.

---

## Building from Source

```bash
cd gp2f-node
cargo build --release
npm test
```

The build produces a platform-specific `gp2f_node.<target>.node` file in the package root.

---

## Related Documentation

- [Policy AST Reference](../../README.md#policy-ast-reference)
- [Fluent Policy Builder Reference](policy-builder.md)
- [Rust Core API](rust-core-api.md)
- [TypeScript Frontend Bindings](typescript-bindings.md)
- [Architecture Overview](../architecture.md)
