# Proposal: Fluent Policy Builder API for Improved Developer Experience (DX)

## 1. Problem Statement

The current Developer Experience (DX) for defining policies in `@gp2f/server` and `@gp2f/client-sdk` relies on constructing raw JSON Abstract Syntax Trees (ASTs). This approach, while flexible and language-agnostic, has significant drawbacks:

*   **Verbosity**: Simple logic requires deeply nested object structures.
*   **Poor Readability**: The intent of the policy is often obscured by structural boilerplate (`kind`, `children`, `path`, `value`).
*   **Lack of Type Safety**: Stringly-typed fields (`kind: 'Field'`, `kind: 'And'`) are prone to typos that are only caught at runtime.
*   **No IDE Support**: Without TypeScript definition files specifically mirroring the AST structure as a builder, developers get minimal autocompletion or contextual help.

### Example: Current State

```typescript
// Allow if role is 'clinician' AND patient_id exists
{
  kind: 'And',
  children: [
    { kind: 'Field', path: '/role', value: 'clinician' },
    { kind: 'Exists', path: '/patient_id' },
  ],
}
```

## 2. Proposed Solution: Fluent Policy Builder API

Introduce a **Fluent API** (chainable methods) that generates the underlying AST. This wrapper layer will provide a type-safe, readable, and concise way to define policies in JavaScript and TypeScript.

### Key Features:

*   **Chainable Methods**: `p.field(...).eq(...)`, `p.and(...)`.
*   **Type Safety**: TypeScript definitions ensure correct usage of operators and values.
*   **Readability**: Code reads like natural language sentences.
*   **IDE Autocompletion**: Discover available methods and operators via IntelliSense.
*   **Zero-Cost Abstraction**: The builder compiles down to the exact same JSON AST used by the Rust engine today.

## 3. API Design Proposal

We propose a `PolicyBuilder` class (exposed as `p` or `Policy`) with static methods for root nodes and instance methods for chaining.

### 3.1. Field Comparisons

**Current:**
```typescript
{ kind: 'Field', path: '/role', value: 'admin' }
```

**Proposed:**
```typescript
import { p } from '@gp2f/server';

p.field('/role').eq('admin')
p.field('/score').gte(80)
p.field('/status').in(['active', 'pending'])
```

### 3.2. Logical Operators

**Current:**
```typescript
{
  kind: 'And',
  children: [
    { kind: 'Field', path: '/role', value: 'admin' },
    { kind: 'Exists', path: '/session/token' },
  ],
}
```

**Proposed:**
```typescript
p.and(
  p.field('/role').eq('admin'),
  p.exists('/session/token')
)
```

### 3.3. Vibe Check (AI Confidence)

**Current:**
```typescript
{ kind: 'VibeCheck', value: 'frustrated', path: '0.8' }
```

**Proposed:**
```typescript
p.vibe('frustrated').confidence(0.8)
// or just confidence:
p.vibe().confidence(0.9)
```

### 3.4. Complex Example

**Current:**
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
    // ...
  }
);
```

**Proposed:**
```typescript
wf.addActivity(
  'collect-vitals',
  {
    policy: p.and(
      p.field('/role').eq('clinician'),
      p.exists('/patient_id')
    ),
    // ...
  }
);
```

## 4. Implementation Strategy

1.  **Shared Library**: Create the builder logic in a shared location (or replicate in both `@gp2f/server` and `@gp2f/client-sdk`) to ensure consistency.
2.  **Builder Class**: Implement a `PolicyBuilder` class that holds the AST state.
3.  **Static Factory**: Expose a constant instance or static class `p` to start the chain.
4.  **`build()` method**: The builder instances should either:
    *   Have a `.build()` method that returns `AstNode`.
    *   Or simply implement `toJSON()` so they can be passed directly where `AstNode` is expected (if the API accepts objects with `toJSON`).
    *   **Preferred**: The API (e.g., `wf.addActivity`) should be updated to accept `AstNode | PolicyBuilder`.

## 5. Benefits

| Feature | Raw JSON AST | Fluent Builder |
| :--- | :--- | :--- |
| **Readability** | Low (verbose, structural noise) | High (concise, intent-focused) |
| **Write Speed** | Slow (manual structure) | Fast (autocompletion) |
| **Safety** | Low (runtime errors common) | High (compile-time checks) |
| **Learning Curve** | Medium (must learn AST schema) | Low (discoverable API) |

## 6. Next Steps

1.  Approve this proposal.
2.  Implement `PolicyBuilder` in `@gp2f/server`.
3.  Update documentation and examples to use the new fluent syntax.
