# @gp2f/server

Native Node.js bindings for the GP2F policy engine – powered by napi-rs.

## Overview
This package provides native bindings to use the core functionality of the GP2F policy engine directly from Node.js applications. It allows for high-performance rule evaluation and state processing.

## Installation
```bash
npm install @gp2f/server
```

## Usage

### 1. Evaluating a Policy (Stateless)
You can evaluate a state document against a GP2F AST policy directly:

```typescript
import { evaluate, evaluateWithTrace, AstNode } from '@gp2f/server';

const policy: AstNode = {
  kind: 'And',
  children: [
    { kind: 'Field', path: '/role', value: 'admin' },
    { kind: 'Exists', path: '/session/token' }
  ]
};

const state = {
  role: 'admin',
  session: { token: 'abc-123' }
};

// Simple boolean evaluation
const isAllowed = evaluate(policy, state);
console.log('Allowed:', isAllowed); // true

// Evaluation with a step-by-step trace
const { result, trace } = evaluateWithTrace(policy, state);
```

### 2. Embedding the GP2F Server & Workflows
You can create a complete reconciliation server and define workflows in Node.js:

```typescript
import { GP2FServer, Workflow } from '@gp2f/server';

async function main() {
  const server = new GP2FServer({ port: 3000 });

  // Define a new workflow
  const wf = new Workflow('document-approval');

  // Register an activity with a policy and a callback
  wf.addActivity(
    'review-step',
    { policy: { kind: 'LiteralTrue' } },
    async (ctx) => {
      console.log(`Executing ${ctx.activityName} for instance ${ctx.instanceId}`);
      console.log('State:', JSON.parse(ctx.stateJson));
    }
  );

  // Register workflow to the server
  server.register(wf);

  // Start handling HTTP requests
  await server.start();
  console.log(`GP2F Server listening on port ${server.port}`);
}

main().catch(console.error);
```

## Development
This package uses `napi-rs` to build the Rust bindings.

- Build cross-platform artifacts: `npm run artifacts`

## License
MIT
