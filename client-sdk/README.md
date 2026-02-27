# @gp2f/client-sdk

GP2F client SDK – reconciliation UX components and WebSocket client.

## Overview
This SDK provides React components for opportunistic UI state reconciliation, along with a WebSocket client that directly interacts with the GP2F backend server using CRDTs. It is designed to create collaborative, real-time shared experiences seamlessly.

## Installation
```bash
npm install @gp2f/client-sdk
```

## Usage

### 1. WebSocket Client (`Gp2fClient`)
The client provides robust sync over WebSockets with built-in Token Bucket rate-limiting and time-offset tracking.

```typescript
import { Gp2fClient } from '@gp2f/client-sdk';

const client = new Gp2fClient({
  url: 'ws://localhost:3000/ws',
  onMessage: (msg) => {
    console.log('Received from server:', msg);
  },
  onToken: (token, done) => {
    // Helpful for streaming AI responses ("Time to First Token")
    console.log(`Token: ${token}, Done: ${done}`);
  },
  onReloadRequired: (version, reason) => {
    console.warn(`Incompatible AST schema (${version}): ${reason}`);
    window.location.reload();
  }
});

client.connect();

// Send messages (automatically rate-limited)
client.send({
  type: 'SYNC',
  payload: { /* ... */ }
});
```

### 2. Optimistic Updates
Visual indication for optimistic updates or LLM loading limits.

```typescript
import { applyOptimisticUpdate } from '@gp2f/client-sdk';

const container = document.getElementById('my-loader-div');

// Shows a shimmering skeleton loader
const cleanup = applyOptimisticUpdate({
  container,
  confidence: 0.8, // >= 0.7 triggers high-confidence skeleton loader
  thinkingText: "Vibe checking..."
});

// Later, when the update completes:
cleanup();
```

### 3. React Components
The SDK ships with React components to handle state conflicts and reconciliation UX:

```tsx
import React from 'react';
import { ReconciliationBanner, UndoButton, MergeModal } from '@gp2f/client-sdk';

export function EditorHeader() {
  return (
    <header>
      {/* Banner handles showing connection states and sync issues */}
      <ReconciliationBanner 
        status="conflict" 
        onResolve={() => console.log('Resolve clicked')} 
      />
      
      {/* Undo integration */}
      <UndoButton 
        canUndo={true} 
        onUndo={() => console.log('Undo triggered!')} 
      />
    </header>
  );
}
```

### 4. Fluent Policy Builder

Construct policy ASTs with a chainable, type-safe API instead of writing raw JSON:

```typescript
import { p } from '@gp2f/client-sdk';

// Role and session check
const policy = p.and(
  p.field('/user/role').eq('admin'),
  p.exists('/session/token'),
);

// Role allow-list
const policy = p.field('/role').in(['admin', 'editor', 'reviewer']);

// Numeric threshold with vibe gate
const policy = p.and(
  p.field('/score').gte(80),
  p.vibe('needs_help').withConfidence(0.7),
);
```

The builder output is a plain `AstNode` that can be passed to `evaluate()`, stored as JSON, or sent to the server.

### 5. Lazy Policy Evaluation (WASM Engine)
Load the fast WASM policy engine dynamically when needed without blocking the main thread.

```typescript
import { loadPolicyEngine } from '@gp2f/client-sdk';

async function checkPolicy(state: any, ast: any) {
  const engine = await loadPolicyEngine();
  const { result } = engine.evaluate(
    JSON.stringify(state),
    JSON.stringify(ast)
  );
  return result;
}
```

## Peer Dependencies
Please note that you'll need `react` and `react-dom` version `>=18.0.0` installed in your host project.

## Scripts
- **Build**: `npm run build`
- **Test**: `npm run test`

## License
MIT
