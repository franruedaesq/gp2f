# GP2F – Generic Policy & Prediction Framework

[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.78%2B-orange.svg)](https://www.rust-lang.org/)
[![Version](https://img.shields.io/badge/version-0.1.0-blue.svg)](Cargo.toml)
[![npm](https://img.shields.io/badge/npm-%40gp2f%2Fserver-red.svg)](gp2f-node/package.json)

GP2F is a **deterministic, local-first framework** that decouples business rules (expressed as versioned JSON ASTs) from routing and UI state. It enables zero-latency optimistic UIs, safe AI participation, conflict-free multi-user collaboration, and enterprise-grade auditability.

---

## Table of Contents

1. [What is GP2F?](#what-is-gp2f)
2. [Why GP2F?](#why-gp2f)
3. [Use Cases](#use-cases)
4. [Architecture](#architecture)
5. [Getting Started](#getting-started)
6. [CLI Usage](#cli-usage)
7. [Server Usage](#server-usage)
8. [TypeScript Client SDK](#typescript-client-sdk)
9. [Node.js Native Bindings](#nodejs-native-bindings)
10. [Policy AST Reference](#policy-ast-reference)
11. [Deployment](#deployment)
12. [Repository Structure](#repository-structure)

---

## What is GP2F?

GP2F stands for **Generic Policy & Prediction Framework**. It is a Rust-based system with four core pillars:

| Pillar | What it does |
|--------|-------------|
| **Isomorphic AST Policy Engine** | The same pure Rust evaluator runs in the browser (compiled to WASM) and on the server (native/Wasmtime), guaranteeing 100% evaluation parity and zero-latency optimistic updates. |
| **Event-Sourced Synchronization** | Every client operation is a cryptographically signed, replay-protected `op_id`. The server reconciles and responds with ACCEPT or REJECT plus a three-way patch for conflict resolution. |
| **Tokenized Agent Sandbox** | LLM proposals go through the same reconciler pipeline. The engine silently drops anything the AST policy disallows, so AI agents can only do what policy permits. |
| **Semantic Vibe Engine** | A lightweight on-device classifier emits a compact `{ intent, confidence, bottleneck }` vector with every op. Policies can use `VIBE_CHECK` nodes to proactively unlock AI assistance. |

---

## Why GP2F?

- **Predictable UIs** – apply policies client-side in WASM before the server round-trip, so the UI updates instantly.
- **Safe multi-user collaboration** – CRDT (Yrs/Yjs), Last-Write-Wins, and Transactional conflict-resolution strategies keep state consistent across concurrent edits.
- **Auditable by design** – every operation and its outcome are stored in an append-only event log with deterministic replay.
- **AI-safe** – language-model proposals are validated by the same reconciler; no special-casing required.
- **Works offline** – the client SDK queues ops in IndexedDB and flushes them when the WebSocket reconnects.

---

## Use Cases

### 1. Medical Triage Intake (HIPAA-aligned)

Role-gated patient intake for urgent-care facilities. Activities require the `"clinician"` or `"admin"` role and patient-consent fields. Compensation actions roll back data ingestion on failure.

```
register patient → collect vitals → assign triage level → finalize intake
```

### 2. Supply-Chain Offline Delivery Update

Designed for network-unreliable environments. A delivery driver completes all activities locally; ops queue and reconcile automatically on reconnect, guaranteeing an offline success rate of ≥ 99.9%.

```
scan package → confirm GPS location → record delivery proof → complete
```

### 3. Multi-Party Contract Negotiation

Sequential review stages for legal, financial, and executive stakeholders. Each stage requires the prior stage to have accepted before advancing.

```
legal draft → finance review → executive approval → final signature
```

### 4. Enterprise Access-Controlled Workflows

Any multi-step workflow where role-based access control (RBAC), audit trails, and conflict resolution matter: loan approvals, HR onboarding, compliance sign-offs.

### 5. AI-Augmented UIs

Embed an LLM co-pilot that proposes state changes via `POST /ai/propose`. The engine enforces policy — the LLM cannot escalate privileges or bypass business rules.

---

## Architecture

```
┌─────────────────────────────────────────────────────────────────────┐
│                           GP2F System                               │
│                                                                     │
│  ┌──────────┐     ┌──────────────┐     ┌────────────────────────┐  │
│  │  Client  │────▶│  WebSocket   │────▶│    Axum Server         │  │
│  │ (WASM +  │◀────│   Channel    │◀────│  (gp2f-actor/server)   │  │
│  │   TS)    │     └──────────────┘     └──────────┬─────────────┘  │
│  └──────────┘                                     │                 │
│                          ┌────────────────────────┤                 │
│                          │                        │                 │
│               ┌──────────▼──────┐    ┌────────────▼────────────┐   │
│               │  gp2f-crdt      │    │  gp2f-store             │   │
│               │  (Reconciler)   │    │  (Event Store)          │   │
│               └─────────────────┘    └─────────────────────────┘   │
└─────────────────────────────────────────────────────────────────────┘
```

**Reconciliation flow:**

```
Client predicts locally (WASM / gp2f-runtime)
  → emits signed op_id
    → server validates signature (gp2f-security), replay protection, snapshot hash, RBAC
      → gp2f-actor serialises ops per workflow instance
        → gp2f-crdt reconciles state, applies CRDT/LWW/Transactional strategies
          → ACCEPT: gp2f-broadcast fans out new state hash to all WebSocket clients
          → REJECT: returns 3-way patch so the client can reconcile
```

---

## Getting Started

### Prerequisites

- **Rust ≥ 1.78** – [install rustup](https://rustup.rs/)
- **Node.js ≥ 18** – for the TypeScript client SDK

### Clone

```bash
git clone https://github.com/franruedaesq/gp2f.git
cd gp2f
```

### Build everything

```bash
cargo build --release
```

### Run tests

```bash
cargo test --workspace
```

### Start the reconciliation server

```bash
cargo run --release -p gp2f-server
# Listening on http://0.0.0.0:3000
```

### Send your first operation

```bash
# Submit an operation (the initial state hash of an empty document is well-known)
curl -s -X POST http://localhost:3000/op \
  -H 'Content-Type: application/json' \
  -d '{
    "opId": "op-001",
    "astVersion": "1.0.0",
    "action": "update",
    "payload": {"name": "Alice"},
    "clientSnapshotHash": "<blake3-hash-of-empty-state>",
    "tenantId": "demo",
    "workflowId": "demo-wf",
    "instanceId": "inst-1",
    "role": "default"
  }'
```

---

## CLI Usage

The `gp2f` CLI provides two commands.

### `eval` – evaluate a policy against a state

```bash
# Create a policy file
echo '{"version":"1.0.0","kind":"LITERAL_TRUE"}' > /tmp/policy.json

# Create a state file
echo '{"role":"admin"}' > /tmp/state.json

# Evaluate
cargo run -p gp2f-cli -- eval \
  --state /tmp/state.json \
  --policy /tmp/policy.json \
  --version "1.0.0"
```

Output:
```
result:  true
hash:    <blake3-hex>
trace:
  [0] LITERAL_TRUE => true
```

The process exits with code `0` on success, `2` when the policy evaluates to `false`, and `1` on any error.

### `replay` – deterministic replay of an event log

```bash
cargo run -p gp2f-cli -- replay \
  --events events.json \
  --policy policy.json \
  --op-id op-42
```

The replay command reconstructs the authoritative state by applying accepted ops in sequence, optionally re-evaluating a policy at each step.

---

## Server Usage

### HTTP Endpoints

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/health` | Readiness probe – checks that the backing store is reachable |
| `GET` | `/livez` | Liveness probe – returns 200 as long as the process is running |
| `GET` | `/ws` | WebSocket upgrade |
| `POST` | `/op` | Submit an operation synchronously (same as WebSocket) |
| `POST` | `/op/async` | Low-latency async ingestion – acknowledges immediately (<1 ms) and pushes the result via WebSocket |
| `POST` | `/token/mint` | Mint a short-lived AI agent token |
| `POST` | `/token/redeem` | Redeem a token for an op |
| `POST` | `/ai/propose` | Submit an AI-generated op proposal |
| `POST` | `/ai/feedback` | Record a dismissed AI suggestion for model drift detection |
| `POST` | `/agent/propose` | Submit an op proposal from an autonomous agent |

### Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `RUST_LOG` | `info` | Log level (`trace`, `debug`, `info`, `warn`, `error`) |
| `LOG_FORMAT` | *(unset)* | Set to `json` for structured JSON logging (recommended in production) |
| `APP_ENV` | *(unset)* | Set to `production` to enforce production guards (requires `REDIS_URL` and signing keys) |
| `DATABASE_URL` | *(none)* | PostgreSQL connection string. When set, uses Postgres as the event store |
| `TEMPORAL_ENDPOINT` | *(none)* | Temporal server address (e.g. `http://temporal:7233`). Used when `DATABASE_URL` is not set |
| `TEMPORAL_NAMESPACE` | `gp2f-prod` | Temporal namespace to use |
| `REDIS_URL` | *(none)* | Redis connection string. Required in production for distributed actor coordination and broadcast |
| `KEYS_JSON` | *(none)* | JSON array of signing key objects for HMAC/JWT validation |
| `KEYS_POLL_INTERVAL_SECS` | `60` | How often (in seconds) to refresh signing keys from the key provider |
| `INGESTION_QUEUE_SIZE` | `512` | Capacity of the async ingestion queue (`/op/async` endpoint) |
| `POLICY_WASM_PATH` | `policy_wasm_bg.wasm` | Path to the compiled policy WASM module |

### Embedding the reconciler in your own Axum app

```rust
use gp2f_crdt::reconciler::Reconciler;

// Development mode (no signature validation)
let reconciler = Reconciler::new();

// Production mode (HMAC validation)
let reconciler = Reconciler::with_secret(b"your-hmac-secret");

// Process an operation
let response = reconciler.reconcile(&client_message);
```

### RBAC Roles

| Role | Permissions |
|------|-------------|
| `admin` | workflow:start, workflow:signal, workflow:cancel, activity:execute, token:mint, token:redeem, ai:propose |
| `reviewer` | workflow:signal, activity:execute, token:redeem, ai:propose |
| `agent` | ai:propose, token:redeem |
| `default` | workflow:start, activity:execute |

---

## TypeScript Client SDK

### Install

```bash
cd client-sdk && npm install && npm run build
```

### Connect

```typescript
import { Gp2fClient } from '@gp2f/client-sdk';

const client = new Gp2fClient({
  url: 'wss://your-server.example.com/ws',
  tenantId: 'acme-corp',
  workflowId: 'my-workflow',
  instanceId: 'inst-001',
  role: 'default',
});

client.onAccept((accept) => {
  // Confirm optimistic update
  console.log('Op accepted:', accept.opId);
});

client.onReject((reject) => {
  // Show merge modal with the 3-way patch
  console.warn('Op rejected:', reject.reason);
});

await client.connect();
```

### Send an operation

```typescript
await client.send({
  action: 'update',
  payload: { name: 'Alice', age: 30 },
});
```

### Offline support

```typescript
const client = new Gp2fClient({
  url: 'wss://...',
  tenantId: 'acme',
  workflowId: 'supply-chain',
  instanceId: 'delivery-001',
  role: 'driver',
  offlineQueue: {
    storageKey: 'gp2f-offline-queue',
    maxSize: 1000,
    flushOnReconnect: true,
  },
});
```

### React components

```tsx
import { UndoButton, ReconciliationBanner, MergeModal } from '@gp2f/client-sdk/react';

// Undo the last accepted op
<UndoButton opId={lastAcceptedOpId} onUndo={() => {}} />

// Show rejection reason
<ReconciliationBanner rejection={lastRejection} onDismiss={() => setLastRejection(null)} />

// 3-way conflict resolution dialog
<MergeModal
  patch={conflictPatch}
  onResolve={(resolved) => client.send({ action: 'merge', payload: resolved })}
  onCancel={() => setConflictPatch(null)}
/>
```

---

## Node.js Native Bindings

The `gp2f-node` package (`@gp2f/server`) provides **native Node.js bindings** for the GP2F policy engine via [napi-rs](https://napi.rs/). No WASM, no child processes — the Rust evaluator runs directly inside Node.js as a native addon.

### Prerequisites

- **Node.js ≥ 16**
- **Rust ≥ 1.78** (to build from source)

### Install

```bash
cd gp2f-node
npm install
```

### Evaluate a policy

```typescript
import { evaluate, evaluateWithTrace } from '@gp2f/server';

// Simple boolean result
const allowed = evaluate(
  { kind: 'Field', path: '/role', value: 'admin' },
  { role: 'admin' }
);
// => true

// Full result with evaluation trace
const { result, trace } = evaluateWithTrace(
  { kind: 'LiteralTrue' },
  {}
);
// => { result: true, trace: ['[0] LiteralTrue => true'] }
```

### Define and run a workflow

```typescript
import { Workflow, GP2FServer } from '@gp2f/server';

const wf = new Workflow('document-approval');

wf.addActivity(
  'review',
  {
    policy: {
      kind: 'Field',
      path: '/role',
      value: 'reviewer',
    },
  },
  async (ctx) => {
    console.log(`Executing ${ctx.activityName} for tenant ${ctx.tenantId}`);
  }
);

const server = new GP2FServer({ port: 3000 });
server.register(wf);
await server.start();
// Listening on http://127.0.0.1:3000
// POST /workflow/run  – execute next activity
// POST /workflow/dry-run – evaluate policies without side-effects
// GET  /health        – health check
```

### Dry-run (policy check without side-effects)

```typescript
const permitted = wf.dryRun({ role: 'reviewer' });
// => true
```

See [`gp2f-node/index.d.ts`](gp2f-node/index.d.ts) for the full TypeScript API reference and [`docs/sdk-reference/nodejs-bindings.md`](docs/sdk-reference/nodejs-bindings.md) for detailed documentation.

---

## Policy AST Reference

Policies are JSON documents that form a tree of `AstNode` objects.

### Node structure

```json
{
  "version": "1.0.0",
  "kind": "AND",
  "children": [ ... ],
  "path": "/some/json-pointer",
  "value": "scalar-string",
  "callName": "external-fn"
}
```

`version` is only required on the root node. `children`, `path`, `value`, and `callName` are optional depending on the `kind`.

### Supported operators

| Kind | Description |
|------|-------------|
| `LITERAL_TRUE` / `LITERAL_FALSE` | Boolean constants |
| `AND` | All children must be true (short-circuits on first false) |
| `OR` | At least one child must be true (short-circuits on first true) |
| `NOT` | Negates its single child |
| `EQ` / `NEQ` | Equality / inequality between two children |
| `GT` / `GTE` / `LT` / `LTE` | Numeric or lexicographic ordering |
| `IN` | Left operand is contained in the right (array) |
| `CONTAINS` | Right operand is contained in the left (array or string) |
| `EXISTS` | JSON-pointer path is present and non-null in state |
| `FIELD` | Resolve a JSON-pointer `path` from the state document |
| `VIBE_CHECK` | Match `intent` and/or `confidence` from the VibeVector |
| `CALL` | Future-proof stub for external function calls |

### Example: role-gated consent check

```json
{
  "version": "1.0.0",
  "kind": "AND",
  "children": [
    {
      "kind": "EQ",
      "children": [
        { "kind": "FIELD", "path": "/role" },
        { "kind": "EQ", "value": "clinician" }
      ]
    },
    {
      "kind": "EQ",
      "children": [
        { "kind": "FIELD", "path": "/consent_given" },
        { "kind": "EQ", "value": "true" }
      ]
    }
  ]
}
```

### Example: vibe-based AI unlock

```json
{
  "version": "1.0.0",
  "kind": "VIBE_CHECK",
  "value": "frustrated",
  "path": "0.8"
}
```

Evaluates to `true` when the user's detected intent is `"frustrated"` **and** the classifier confidence is ≥ 0.8.

### Conflict resolution strategies

| Strategy | Description |
|----------|-------------|
| `CRDT` | Field uses Yrs (Yjs) CRDT merge |
| `LWW` | Last-Write-Wins by server timestamp |
| `TRANSACTIONAL` | Entire op is rejected if this field conflicts |

---

## Deployment

### Docker

The repository ships two Dockerfiles:

- `Dockerfile` – builds and runs the Axum reconciliation server.
- `Dockerfile.worker` – builds and runs the background Temporal worker (used when `TEMPORAL_ENDPOINT` is set).

```bash
docker build -t gp2f/server .
docker run -p 3000:3000 -e RUST_LOG=info gp2f/server
```

### Kubernetes (Helm)

A production-ready Helm chart is included under `helm/gp2f/`.

```bash
# Install with default values
helm install gp2f ./helm/gp2f

# Override for production
helm install gp2f ./helm/gp2f \
  --set server.image.tag=1.0.0 \
  --set server.autoscaling.enabled=true \
  --set server.env.APP_ENV=production \
  --set-string server.envFrom[0].secretRef.name=gp2f-secrets
```

Key chart features:

- Horizontal Pod Autoscaler (min 3, max 10 replicas by default)
- Separate worker Deployment for Temporal activity workers
- Bundled `temporal-postgres.yaml` template for a minimal Temporal + Postgres stack in dev/staging

See [`helm/gp2f/values.yaml`](helm/gp2f/values.yaml) for all configurable values.

### Database migrations

When using the Postgres event store, apply migrations in order:

```bash
psql $DATABASE_URL -f migrations/20240522_init.sql
psql $DATABASE_URL -f migrations/20240523_auto_seq.sql
psql $DATABASE_URL -f migrations/20240524_op_id_unique.sql
```

---

## Repository Structure

```
gp2f/
├── proto/              # Protobuf definitions (ASTNode, wire protocol)
├── policy-core/        # Pure Rust AST evaluator (no I/O, compilable to WASM)
│   ├── src/
│   │   ├── ast.rs      # AstNode + NodeKind types
│   │   ├── evaluator.rs# Evaluator::evaluate() + unit tests
│   │   ├── crdt.rs     # CRDT document schema + field strategies
│   │   └── version.rs  # VersionPolicy (allow-list for AST versions)
│   └── tests/
│       └── property_tests.rs  # proptest: random state × random AST
├── gp2f-core/          # Shared wire types (ClientMessage, VibeVector, HLC timestamps)
├── gp2f-security/      # RBAC, HMAC signature validation, replay protection, secrets
├── gp2f-store/         # Append-only event store (in-memory, Postgres, Temporal backends)
├── gp2f-broadcast/     # ACCEPT/REJECT broadcaster (tokio channels; Redis-ready)
├── gp2f-token/         # Ephemeral AI agent token service with rate limiting
├── gp2f-crdt/          # CRDT-based conflict-free state reconciler
├── gp2f-actor/         # Per-instance actor model (serialises ops; Redis cluster coordination)
├── gp2f-workflow/      # WorkflowDefinition + WorkflowInstance + built-in pilot workflows
├── gp2f-vibe/          # Semantic Vibe classifier + LLM provider abstraction + LLM audit log
├── gp2f-runtime/       # Wasmtime-based WASM engine for isomorphic policy evaluation
├── gp2f-ingest/        # Async ingestion queue (< 1 ms HTTP ack; result via WebSocket push)
├── gp2f-api/           # Shared HTTP handler utilities, middleware, and tool-gating
├── gp2f-canary/        # Replay-based canary test suite for determinism regression detection
├── server/             # Axum HTTP + WebSocket reconciliation server
│   └── src/
│       ├── main.rs           # Routes, AppState, all HTTP/WS handlers
│       └── lib.rs            # Re-exports for integration tests
├── gp2f-node/          # Native Node.js bindings (@gp2f/server) via napi-rs
│   ├── src/
│   │   ├── policy.rs   # evaluate / evaluateWithTrace bindings
│   │   ├── workflow.rs # Workflow class bindings
│   │   └── server.rs   # GP2FServer class bindings
│   ├── index.d.ts      # TypeScript declarations (auto-generated)
│   └── __test__/       # Jest contract + integration tests
├── cli/                # gp2f eval / replay CLI
├── client-sdk/         # TypeScript npm package (@gp2f/client-sdk)
├── migrations/         # PostgreSQL schema migrations (apply in order)
├── helm/               # Helm chart for Kubernetes deployment
└── docs/               # Architecture, wire protocol, SDK onboarding
```

---

## License

MIT – see [LICENSE](LICENSE).
