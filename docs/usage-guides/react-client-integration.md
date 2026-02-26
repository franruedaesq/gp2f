# React Client Integration Guide

This guide walks through a complete TypeScript/React implementation that integrates the GP2F WASM evaluator, Zustand policy store, optimistic UI updates, and the encrypted IndexedDB `op_id` queue.

---

## Architecture Overview

The client integration has four layers that work together:

**Layer 1: WASM Evaluator** — the `policy-core` WASM module loaded once at startup and used for local AST evaluation.

**Layer 2: Policy Store** — a Zustand store that holds the current policy AST, the document state, and the optimistic UI state.

**Layer 3: Op ID Pipeline** — the cryptographic `op_id` generation, IndexedDB queue persistence, and WebSocket emission logic.

**Layer 4: Sync Manager** — the WebSocket handler that processes `ACCEPT`/`REJECT` acknowledgments and applies CRDT patches.

---

## Step 1: Initialize the WASM Evaluator

The WASM module is loaded asynchronously and should be initialized once at application startup, before any React components render. Create a module initializer in `src/wasm/init.ts`:

```typescript
import init, { PolicyCore } from '../../wasm-out/policy_core.js';

let policyCoreInstance: PolicyCore | null = null;

export async function initWasm(): Promise<void> {
  await init();
  policyCoreInstance = new PolicyCore();
}

export function getPolicyCore(): PolicyCore {
  if (!policyCoreInstance) {
    throw new Error('WASM evaluator not initialized. Call initWasm() before accessing getPolicyCore().');
  }
  return policyCoreInstance;
}
```

In your application entry point (`src/main.tsx`), initialize WASM before mounting the React tree:

```typescript
import React from 'react';
import ReactDOM from 'react-dom/client';
import { initWasm } from './wasm/init';
import App from './App';

initWasm().then(() => {
  ReactDOM.createRoot(document.getElementById('root')!).render(
    <React.StrictMode>
      <App />
    </React.StrictMode>
  );
}).catch((err) => {
  console.error('Failed to initialize GP2F WASM evaluator:', err);
  // Render a fallback error UI here
});
```

---

## Step 2: Define the Policy Store with Zustand

The Zustand `usePolicyStore` is the single source of truth for the client's policy state. It holds the serialized AST, the current document state, the list of permitted actions (derived from the last evaluation), and the optimistic overlay.

Create `src/stores/policyStore.ts`:

```typescript
import { create } from 'zustand';
import { immer } from 'zustand/middleware/immer';
import { getPolicyCore } from '../wasm/init';
import type { AstNode, PolicyState, EvalResult, Intent } from '../types/policy';

interface OptimisticEntry {
  opId: string;
  previousState: PolicyState;
  predictedState: PolicyState;
  intent: Intent;
  emittedAt: number;
}

interface PolicyStoreState {
  // Policy AST, received from server and cached in IndexedDB
  currentAst: AstNode | null;
  astVersion: string | null;

  // Canonical document state (last server-confirmed state)
  canonicalState: PolicyState | null;

  // Optimistic overlay: the predicted state including unconfirmed ops
  optimisticState: PolicyState | null;

  // Permitted actions from the last evaluation
  permittedActions: string[];

  // Queue of unconfirmed operations
  pendingOps: Map<string, OptimisticEntry>;

  // Actions
  loadAst: (ast: AstNode, version: string) => void;
  loadState: (state: PolicyState) => void;
  evaluateIntent: (intent: Intent) => EvalResult | null;
  applyOptimisticUpdate: (opId: string, intent: Intent, predictedState: PolicyState) => void;
  confirmOp: (opId: string, outcome: 'ACCEPT' | 'REJECT', canonicalPatch: PolicyState | null) => void;
}

export const usePolicyStore = create<PolicyStoreState>()(
  immer((set, get) => ({
    currentAst: null,
    astVersion: null,
    canonicalState: null,
    optimisticState: null,
    permittedActions: [],
    pendingOps: new Map(),

    loadAst: (ast, version) => {
      set((state) => {
        state.currentAst = ast;
        state.astVersion = version;
      });
      // Re-evaluate permissions with the new AST
      const { canonicalState } = get();
      if (canonicalState) {
        const core = getPolicyCore();
        const evalResult = core.evaluatePermittedActions(
          JSON.stringify(ast),
          JSON.stringify(canonicalState)
        );
        set((state) => {
          state.permittedActions = evalResult.permittedActions;
        });
      }
    },

    loadState: (state) => {
      set((draft) => {
        draft.canonicalState = state;
        draft.optimisticState = state;
      });
      // Evaluate permitted actions for the loaded state
      const { currentAst } = get();
      if (currentAst) {
        const core = getPolicyCore();
        const evalResult = core.evaluatePermittedActions(
          JSON.stringify(currentAst),
          JSON.stringify(state)
        );
        set((draft) => {
          draft.permittedActions = evalResult.permittedActions;
        });
      }
    },

    evaluateIntent: (intent) => {
      const { currentAst, optimisticState } = get();
      if (!currentAst || !optimisticState) return null;

      const core = getPolicyCore();
      const result = core.evaluate(
        JSON.stringify(currentAst),
        JSON.stringify(optimisticState),
        JSON.stringify(intent)
      );
      return JSON.parse(result) as EvalResult;
    },

    applyOptimisticUpdate: (opId, intent, predictedState) => {
      const { optimisticState } = get();
      set((draft) => {
        draft.pendingOps.set(opId, {
          opId,
          previousState: optimisticState!,
          predictedState,
          intent,
          emittedAt: Date.now(),
        });
        draft.optimisticState = predictedState;
      });
    },

    confirmOp: (opId, outcome, canonicalPatch) => {
      set((draft) => {
        const entry = draft.pendingOps.get(opId);
        if (!entry) return;

        // Remove the confirmed op from pending before replaying remaining ops
        draft.pendingOps.delete(opId);

        if (outcome === 'ACCEPT' && canonicalPatch) {
          draft.canonicalState = canonicalPatch;
          // Recompute optimistic state by replaying remaining pending ops over
          // the new canonical state (excluding the now-confirmed op)
          let state = canonicalPatch;
          for (const [, pending] of draft.pendingOps) {
            state = pending.predictedState;
          }
          draft.optimisticState = state;
        } else if (outcome === 'REJECT') {
          // Roll back canonical state to before the rejected op
          draft.canonicalState = entry.previousState;
          // Recompute optimistic state by replaying remaining ops (those submitted
          // after the rejected op) over the rolled-back canonical state
          let state: PolicyState = entry.previousState;
          for (const [, pending] of draft.pendingOps) {
            if (pending.emittedAt > entry.emittedAt) {
              state = pending.predictedState;
            }
          }
          draft.optimisticState = state;
        }
      });
    },
  }))
);
```

---

## Step 3: Implement the Op ID Generator

The `op_id` generator uses the WebCrypto API to produce a tamper-evident operation identifier. Create `src/crypto/opId.ts`:

```typescript
import { getSessionKey } from './kms';
import type { Intent } from '../types/policy';
import * as cbor from 'cbor-web';

export interface OpIdPayload {
  opId: string;        // Hex-encoded HMAC-SHA256
  intent: Intent;
  timestampMs: number;
  clientStateHash: string;
  sequenceNumber: number;
}

let sequenceCounter = 0;

export async function generateOpId(
  intent: Intent,
  clientStateHash: string
): Promise<OpIdPayload> {
  const sessionKey = await getSessionKey();
  const timestampMs = Date.now();
  const sequenceNumber = ++sequenceCounter;

  const payload = await cbor.encodeAsync({
    intent,
    timestampMs,
    clientStateHash,
    sequenceNumber,
  });

  const signature = await crypto.subtle.sign('HMAC', sessionKey, payload);
  const opId = bufferToHex(signature);

  return { opId, intent, timestampMs, clientStateHash, sequenceNumber };
}

function bufferToHex(buffer: ArrayBuffer): string {
  return Array.from(new Uint8Array(buffer))
    .map((b) => b.toString(16).padStart(2, '0'))
    .join('');
}
```

---

## Step 4: Implement the IndexedDB Queue with AES-GCM Encryption

Pending `op_id`s are encrypted at rest in IndexedDB before network emission. Create `src/queue/indexedDbQueue.ts`:

```typescript
import { getEncryptionKey } from '../crypto/kms';

const DB_NAME = 'gp2f_queue';
const STORE_NAME = 'pending_ops';
const DB_VERSION = 1;

let db: IDBDatabase | null = null;

async function getDb(): Promise<IDBDatabase> {
  if (db) return db;
  return new Promise((resolve, reject) => {
    const req = indexedDB.open(DB_NAME, DB_VERSION);
    req.onupgradeneeded = () => {
      req.result.createObjectStore(STORE_NAME, { keyPath: 'opId' });
    };
    req.onsuccess = () => { db = req.result; resolve(req.result); };
    req.onerror = () => reject(req.error);
  });
}

export async function enqueueOp(opIdPayload: object): Promise<void> {
  const encKey = await getEncryptionKey();
  const plaintext = new TextEncoder().encode(JSON.stringify(opIdPayload));
  const iv = crypto.getRandomValues(new Uint8Array(12));

  const ciphertext = await crypto.subtle.encrypt(
    { name: 'AES-GCM', iv },
    encKey,
    plaintext
  );

  const record = {
    opId: (opIdPayload as { opId: string }).opId,
    iv: Array.from(iv),
    ciphertext: Array.from(new Uint8Array(ciphertext)),
    enqueuedAt: Date.now(),
  };

  const database = await getDb();
  return new Promise((resolve, reject) => {
    const tx = database.transaction(STORE_NAME, 'readwrite');
    tx.objectStore(STORE_NAME).put(record);
    tx.oncomplete = () => resolve();
    tx.onerror = () => reject(tx.error);
  });
}

export async function dequeueOp(opId: string): Promise<void> {
  const database = await getDb();
  return new Promise((resolve, reject) => {
    const tx = database.transaction(STORE_NAME, 'readwrite');
    tx.objectStore(STORE_NAME).delete(opId);
    tx.oncomplete = () => resolve();
    tx.onerror = () => reject(tx.error);
  });
}

export async function getAllPendingOps(): Promise<object[]> {
  const encKey = await getEncryptionKey();
  const database = await getDb();

  const records: { iv: number[]; ciphertext: number[] }[] = await new Promise((resolve, reject) => {
    const tx = database.transaction(STORE_NAME, 'readonly');
    const req = tx.objectStore(STORE_NAME).getAll();
    req.onsuccess = () => resolve(req.result);
    req.onerror = () => reject(req.error);
  });

  const decrypted: object[] = [];
  for (const record of records) {
    const plaintext = await crypto.subtle.decrypt(
      { name: 'AES-GCM', iv: new Uint8Array(record.iv) },
      encKey,
      new Uint8Array(record.ciphertext)
    );
    decrypted.push(JSON.parse(new TextDecoder().decode(plaintext)));
  }
  return decrypted;
}
```

---

## Step 5: Wire It All Together in a React Component

This component renders a document action button that is only enabled when the AST policy permits the action, applies an optimistic update on click, and emits the signed `op_id`:

```typescript
import React, { useCallback } from 'react';
import { usePolicyStore } from '../stores/policyStore';
import { generateOpId } from '../crypto/opId';
import { enqueueOp } from '../queue/indexedDbQueue';
import { useWebSocket } from '../sync/useWebSocket';
import { blake3Hash } from '../crypto/hash';

interface ActionButtonProps {
  actionId: string;
  label: string;
  predictStateTransition: (currentState: PolicyState) => PolicyState;
}

export function ActionButton({
  actionId,
  label,
  predictStateTransition,
}: ActionButtonProps) {
  const {
    permittedActions,
    optimisticState,
    evaluateIntent,
    applyOptimisticUpdate,
  } = usePolicyStore();

  const { emit } = useWebSocket();
  const isPermitted = permittedActions.includes(actionId);

  const handleClick = useCallback(async () => {
    if (!optimisticState || !isPermitted) return;

    const intent = { actionId, documentId: optimisticState.documentId };
    const evalResult = evaluateIntent(intent);

    // Final check: if the local evaluation says not permitted, abort.
    // This handles race conditions where permittedActions is stale.
    if (!evalResult?.permitted) return;

    const stateHash = await blake3Hash(JSON.stringify(optimisticState));
    const opIdPayload = await generateOpId(intent, stateHash);

    // 1. Apply optimistic update immediately (zero latency)
    const predictedState = predictStateTransition(optimisticState);
    applyOptimisticUpdate(opIdPayload.opId, intent, predictedState);

    // 2. Persist to encrypted IndexedDB queue
    await enqueueOp(opIdPayload);

    // 3. Emit over WebSocket (non-blocking)
    emit({ type: 'OP_ID', payload: opIdPayload });
  }, [actionId, optimisticState, isPermitted, evaluateIntent, applyOptimisticUpdate, emit]);

  return (
    <button
      onClick={handleClick}
      disabled={!isPermitted}
      aria-disabled={!isPermitted}
      className={`action-btn ${isPermitted ? 'action-btn--enabled' : 'action-btn--disabled'}`}
    >
      {label}
    </button>
  );
}
```

---

## Step 6: Handle Sync Acknowledgments

The `useWebSocket` hook handles incoming messages from the server. Create `src/sync/useWebSocket.ts`:

```typescript
import { useEffect, useRef, useCallback } from 'react';
import { usePolicyStore } from '../stores/policyStore';
import { dequeueOp } from '../queue/indexedDbQueue';
import { getAllPendingOps } from '../queue/indexedDbQueue';

export function useWebSocket() {
  const ws = useRef<WebSocket | null>(null);
  const { confirmOp, loadAst, loadState } = usePolicyStore();

  useEffect(() => {
    const socket = new WebSocket(import.meta.env.VITE_WEBSOCKET_URL);
    ws.current = socket;

    socket.onopen = async () => {
      // Replay any ops that were queued while offline
      const pendingOps = await getAllPendingOps();
      for (const op of pendingOps) {
        socket.send(JSON.stringify({ type: 'OP_ID', payload: op }));
      }
    };

    socket.onmessage = async (event) => {
      const message = JSON.parse(event.data as string);

      if (message.type === 'OP_ACK') {
        const { opId, outcome, canonicalPatch } = message.payload;
        // Remove from durable queue
        await dequeueOp(opId);
        // Update store (accept or revert optimistic update)
        confirmOp(opId, outcome, canonicalPatch);
      }

      if (message.type === 'AST_UPDATE') {
        const { ast, version } = message.payload;
        loadAst(ast, version);
      }

      if (message.type === 'STATE_SYNC') {
        loadState(message.payload.state);
      }
    };

    return () => socket.close();
  }, [confirmOp, loadAst, loadState]);

  const emit = useCallback((message: object) => {
    if (ws.current?.readyState === WebSocket.OPEN) {
      ws.current.send(JSON.stringify(message));
    }
  }, []);

  return { emit };
}
```

---

## Type Definitions Reference

The core types used throughout the integration are defined in `src/types/policy.ts`:

```typescript
export interface Intent {
  actionId: string;
  documentId: string;
  [key: string]: unknown;
}

export interface EvalResult {
  permitted: boolean;
  trace: string[];
  snapshotHash: string;
  permittedActions?: string[];
}

export interface PolicyState {
  documentId: string;
  status: string;
  userRole: string;
  fields: Record<string, unknown>;
  vectorClock: Record<string, number>;
}

export interface AstNode {
  nodeType: string;
  children?: AstNode[];
  value?: unknown;
  condition?: string;
}
```
