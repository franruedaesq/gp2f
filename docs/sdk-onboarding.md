# GP2F SDK – Onboarding Guide & API Reference

> **Version**: 0.1.0  
> **Status**: Phase 9 – Production Hardening  
> Covers the TypeScript client SDK (`client-sdk/`) and the Rust server library (`gp2f-server`).

---

## Table of Contents

1. [What GP2F Is](#1-what-gp2f-is)
2. [Quick Start (5 minutes)](#2-quick-start-5-minutes)
3. [Core Concepts](#3-core-concepts)
4. [TypeScript Client SDK](#4-typescript-client-sdk)
5. [Rust Server Library](#5-rust-server-library)
6. [Pilot Workflow Recipes](#6-pilot-workflow-recipes)
7. [Configuration Reference](#7-configuration-reference)
8. [Troubleshooting](#8-troubleshooting)
9. [Migration & Upgrade Guide](#9-migration--upgrade-guide)

---

## 1. What GP2F Is

GP2F (**Generic Policy & Prediction Framework**) is a deterministic, local-first
framework for building enterprise UIs that:

- **Predict locally** – the same AST policy evaluator runs in the browser (WASM) and
  on the server (native Rust), guaranteeing zero-latency optimistic updates.
- **Reconcile safely** – every operation is cryptographically signed, replay-protected,
  and validated against an immutable AST policy before being committed.
- **Collaborate conflict-free** – CRDT (Yrs/Yjs), Last-Write-Wins, and Transactional
  conflict-resolution strategies keep multi-user state consistent.
- **Sandbox AI agents** – language-model proposals go through the same reconciler
  pipeline; the engine silently drops anything disallowed by policy.
- **Audit everything** – every op and its outcome is stored in an append-only event
  log that supports deterministic replay.

---

## 2. Quick Start (5 minutes)

### Prerequisites
- Node.js ≥ 18 (for the TypeScript SDK)
- Rust ≥ 1.75 (for the server and CLI)

### Step 1 – Clone the repository

```bash
git clone https://github.com/franruedaesq/gp2f.git
cd gp2f
```

### Step 2 – Start the reconciliation server

```bash
cargo run --release -p gp2f-server
# Server is now listening on http://0.0.0.0:3000
```

### Step 3 – Install the TypeScript client SDK

```bash
cd client-sdk
npm install
npm run build
```

### Step 4 – Send your first operation

```bash
# Get the initial snapshot hash
INIT_HASH=$(curl -s http://localhost:3000/health)

# Submit a signed operation
curl -s -X POST http://localhost:3000/op \
  -H 'Content-Type: application/json' \
  -d '{
    "opId": "op-001",
    "astVersion": "1.0.0",
    "action": "update",
    "payload": {"name": "Alice"},
    "clientSnapshotHash": "<hash-from-step-above>",
    "tenantId": "demo-tenant",
    "workflowId": "demo",
    "instanceId": "inst-1",
    "role": "default"
  }'
```

### Step 5 – Evaluate a policy with the CLI

```bash
# Create a simple policy and state file
echo '{"kind":"LITERAL_TRUE"}' > /tmp/policy.json
echo '{}' > /tmp/state.json

cargo run -p gp2f-cli -- eval \
  --state /tmp/state.json \
  --policy /tmp/policy.json \
  --version "1.0.0"
```

---

## 3. Core Concepts

### 3.1 AST Policy Node

Every business rule is expressed as a JSON AST.  The evaluator is **pure** (zero I/O,
zero side effects) and returns `{ result, trace, snapshot_hash }`.

```json
{
  "version": "1.0.0",
  "kind": "AND",
  "children": [
    {
      "kind": "EQ",
      "children": [
        { "kind": "FIELD", "path": "/role" },
        { "kind": "EQ",    "value": "admin" }
      ]
    },
    {
      "kind": "EQ",
      "children": [
        { "kind": "FIELD", "path": "/consent_given" },
        { "kind": "EQ",    "value": "true" }
      ]
    }
  ]
}
```

**Supported operators**:

| Operator | Description |
|----------|-------------|
| `LITERAL_TRUE` / `LITERAL_FALSE` | Boolean constants |
| `AND` / `OR` / `NOT` | Logical combinators (short-circuit) |
| `EQ` / `NEQ` | Equality / inequality |
| `GT` / `GTE` / `LT` / `LTE` | Numeric / string ordering |
| `IN` | Left operand contained in right array |
| `CONTAINS` | Right operand contained in left array/string |
| `EXISTS` | JSON path present and non-null |
| `FIELD` | Resolve a JSON-pointer path from state |
| `VIBE_CHECK` | Match intent/confidence from VibeVector |

### 3.2 ClientMessage Wire Format

```typescript
interface ClientMessage {
  opId: string;                  // Cryptographic op identifier
  astVersion: string;            // Semver of the policy being evaluated
  action: string;                // Intent label (e.g. "update", "approve")
  payload: Record<string, unknown>; // State delta
  clientSnapshotHash: string;    // BLAKE3 hex of the client's current state
  tenantId: string;
  workflowId: string;
  instanceId: string;
  clientSignature?: string;      // HMAC-SHA256 base64url; required in prod
  role: string;                  // Caller's role (used for RBAC)
  vibe?: VibeVector;             // Optional semantic intent signal
}
```

### 3.3 ServerMessage Wire Format

```typescript
type ServerMessage =
  | { type: "ACCEPT"; opId: string; serverSnapshotHash: string }
  | { type: "REJECT"; opId: string; reason: string; patch: ThreeWayPatch };
```

### 3.4 op_id Construction

```
op_id = base64url(
  version(1) | tenant_id | client_id | counter(4) |
  ts_ms(8) | nonce_16 | HMAC-SHA256(all-above, tenant_secret)
)
```

The `op_id` is globally unique, tamper-evident, and sortable by time.

---

## 4. TypeScript Client SDK

### Installation

```bash
npm install @gp2f/client-sdk
# or
yarn add @gp2f/client-sdk
```

### Connecting

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
  console.log('Op accepted:', accept.opId);
  // Confirm optimistic update
});

client.onReject((reject) => {
  console.warn('Op rejected:', reject.reason);
  // Show ReconciliationBanner with the 3-way patch
});

await client.connect();
```

### Sending an Operation

```typescript
const response = await client.send({
  action: 'update',
  payload: { name: 'Alice', age: 30 },
});
```

### React Components

#### `<UndoButton>`

Renders a button that reverts the last accepted op (triggers `workflow:cancel`).

```tsx
import { UndoButton } from '@gp2f/client-sdk/react';

<UndoButton
  opId={lastAcceptedOpId}
  onUndo={() => console.log('Undo triggered')}
/>
```

#### `<ReconciliationBanner>`

Displays a dismissible banner when an op is rejected, showing the rejection reason.

```tsx
import { ReconciliationBanner } from '@gp2f/client-sdk/react';

<ReconciliationBanner
  rejection={lastRejection}
  onDismiss={() => setLastRejection(null)}
/>
```

#### `<MergeModal>`

Shows a 3-way patch conflict resolution dialog when the client receives a
`ThreeWayPatch` with conflicts.

```tsx
import { MergeModal } from '@gp2f/client-sdk/react';

<MergeModal
  patch={conflictPatch}
  onResolve={(resolved) => client.send({ action: 'merge', payload: resolved })}
  onCancel={() => setConflictPatch(null)}
/>
```

### Offline Support

The SDK automatically queues ops during network outages using `IndexedDB` (configurable):

```typescript
const client = new Gp2fClient({
  url: 'wss://...',
  tenantId: 'acme',
  workflowId: 'supply-chain',
  instanceId: 'delivery-001',
  role: 'driver',
  offlineQueue: {
    storageKey: 'gp2f-offline-queue',
    maxSize: 1000,            // max queued ops before rejecting locally
    flushOnReconnect: true,   // auto-flush queue when WebSocket reconnects
  },
});
```

---

## 5. Rust Server Library

### Adding to Cargo.toml

```toml
[dependencies]
gp2f-server = { git = "https://github.com/franruedaesq/gp2f" }
```

### Creating a Reconciler

```rust
use gp2f_server::reconciler::Reconciler;

// Development mode (no signature validation)
let reconciler = Reconciler::new();

// Production mode (HMAC validation)
let tenant_secret = std::env::var("GP2F_TENANT_SECRET")
    .expect("GP2F_TENANT_SECRET must be set");
let reconciler = Reconciler::with_secret(tenant_secret.as_bytes());
```

### Registering Pilot Workflows

```rust
use gp2f_server::{
    pilot_workflows::register_pilot_workflows,
    workflow::WorkflowRegistry,
};

let mut registry = WorkflowRegistry::new();
register_pilot_workflows(&mut registry);

// Start a medical triage workflow
let def = registry.get("medical_triage_intake").unwrap();
let mut instance = WorkflowInstance::start("inst-001", "hospital-a", def);
```

### Processing Operations

```rust
use gp2f_server::wire::ClientMessage;
use serde_json::json;

let msg = ClientMessage {
    op_id: "op-001".into(),
    ast_version: "1.0.0".into(),
    action: "update".into(),
    payload: json!({ "consent_given": true }),
    client_snapshot_hash: current_hash,
    tenant_id: "hospital-a".into(),
    workflow_id: "medical_triage_intake".into(),
    instance_id: "inst-001".into(),
    client_signature: Some(compute_hmac(&msg_bytes, &secret)),
    role: "clinician".into(),
    vibe: None,
};

let response = reconciler.reconcile(&msg);
```

### Load and Chaos Testing

```rust
use gp2f_server::chaos::{ChaosScenario, LoadSimulator};

// Simulate 10 000 concurrent users across 50 tenants
let sim = LoadSimulator::new(10_000, 50);
sim.run_scenario(ChaosScenario::ConcurrentEdits);

let metrics = sim.metrics();
assert!(metrics.reconciliation_rate() >= 0.999);
assert!(metrics.p99_latency_ms() <= 5);
```

### Per-Tenant Limits

```rust
use gp2f_server::limits::{LimitsGuard, TenantLimits};

let limits = LimitsGuard::new();
limits.set_limits("enterprise-tenant", TenantLimits {
    max_queued_ops: 5_000,
    max_ws_connections: 500,
});
```

---

## 6. Pilot Workflow Recipes

### 6.1 Medical Triage Intake

```rust
use gp2f_server::pilot_workflows::medical_triage_intake;
use gp2f_server::workflow::WorkflowInstance;
use serde_json::json;

let def = medical_triage_intake();
let mut inst = WorkflowInstance::start("triage-001", "hospital-a", &def);

// Activity 1: register patient (clinician role required)
inst.execute_next(&def, &json!({ "role": "clinician" }), "op-1")?;

// Activity 2: collect vitals (clinician + patient consent required)
inst.execute_next(&def, &json!({
    "role": "clinician",
    "consent_given": true
}), "op-2")?;

// Activity 3: assign triage level (clinician + vitals recorded)
inst.execute_next(&def, &json!({
    "role": "clinician",
    "vitals_recorded": true
}), "op-3")?;

// Activity 4: finalize intake (admin only)
inst.execute_next(&def, &json!({ "role": "admin" }), "op-4")?;
assert_eq!(inst.status, WorkflowStatus::Completed);
```

### 6.2 Supply-Chain Offline Delivery Update

This workflow is designed for network-unreliable environments.  Activities can
be executed while offline; ops queue and reconcile automatically on reconnect.

```rust
use gp2f_server::pilot_workflows::supply_chain_delivery_update;

let def = supply_chain_delivery_update();
let mut inst = WorkflowInstance::start("delivery-999", "logistics-co", &def);

inst.execute_next(&def, &json!({ "role": "driver" }), "op-1")?;
inst.execute_next(&def, &json!({ "role": "driver", "gps_signed": true }), "op-2")?;
inst.execute_next(&def, &json!({
    "role": "driver",
    "delivery_location_confirmed": true
}), "op-3")?;
inst.execute_next(&def, &json!({
    "role": "driver",
    "proof_of_delivery_recorded": true
}), "op-4")?;
```

### 6.3 Multi-Party Contract Negotiation

```rust
use gp2f_server::pilot_workflows::multi_party_contract_negotiation;

let def = multi_party_contract_negotiation();
let mut inst = WorkflowInstance::start("contract-42", "law-firm", &def);

inst.execute_next(&def, &json!({ "role": "legal",   "draft_uploaded": true }), "op-1")?;
inst.execute_next(&def, &json!({ "role": "finance",  "legal_signed_off": true }), "op-2")?;
inst.execute_next(&def, &json!({ "role": "executive","finance_signed_off": true }), "op-3")?;
inst.execute_next(&def, &json!({ "role": "signatory","executive_approved": true }), "op-4")?;
```

---

## 7. Configuration Reference

### Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `RUST_LOG` | `info` | Log level (`trace`, `debug`, `info`, `warn`, `error`) |
| `GP2F_TENANT_SECRET` | *(none)* | HMAC secret for op_id signature validation (required in production) |
| `GP2F_BIND_ADDR` | `0.0.0.0:3000` | Server listen address |
| `GP2F_MAX_QUEUED_OPS` | `500` | Default per-tenant max queued ops (overridable per tenant) |
| `GP2F_MAX_WS_CONNS` | `100` | Default per-tenant max WebSocket connections |
| `GP2F_COMPACTION_THRESHOLD` | `1000` | Events per partition before compaction |

### Golden Metric Thresholds

| Metric | Target | Measured by |
|--------|--------|-------------|
| `reconciliation_rate` | ≥ 99.9 % | `SimMetrics::reconciliation_rate()` |
| `eval_latency_p99` | < 5 ms (server) | `SimMetrics::p99_latency_ms()` |
| `agent_tool_failure_rate` | 0 for disallowed actions | Reconciler REJECT audit log |
| `offline_success_rate` | ≥ 99.9 % | `SimMetrics::offline_success_rate()` |

---

## 8. Troubleshooting

### "snapshot hash mismatch"

The client's BLAKE3 hash of the state doesn't match the server's.

**Fix**: Refresh the client's local state snapshot before retrying the op, or
display the `<MergeModal>` to let the user resolve the conflict.

### "duplicate op_id"

The same `op_id` was submitted twice (replay attack or client bug).

**Fix**: Generate a new `op_id` for each distinct operation.  Never reuse an `op_id`.

### "tenant X has reached the maximum queued ops limit"

Too many ops are in-flight for this tenant.

**Fix**: Increase `max_queued_ops` for the tenant via `LimitsGuard::set_limits`, or
implement client-side exponential backoff before re-submitting.

### Policy evaluation returns `false` unexpectedly

Enable tracing to see the full evaluation path:

```bash
RUST_LOG=debug cargo run -p gp2f-cli -- eval \
  --state state.json --policy policy.json --version "1.0.0"
```

The `trace` field in the response lists every node visited.

---

## 9. Migration & Upgrade Guide

### 0.0.x → 0.1.0

- **Wire format change**: `op_id` renamed from `opId` in JSON (now `camelCase` via
  `serde(rename_all = "camelCase")`).  Update all JSON serialization.
- **New required field**: `role` is now required on `ClientMessage`; defaults to
  `"default"` if absent, but should be set explicitly.
- **Compaction**: `EventStore` now auto-compacts at 1 000 events per partition.
  Snapshot markers are backward-compatible with existing replay consumers.
- **Pilot workflows**: Three new workflow definitions added in
  `gp2f_server::pilot_workflows`.  No breaking changes to existing workflows.
