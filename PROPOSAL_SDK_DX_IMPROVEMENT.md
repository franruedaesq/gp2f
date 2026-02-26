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

*   **Chainable Methods**: `p.field(...).equal(...)`, `p.and(...)`.
*   **Explicit Naming**: Use verbose, human-readable method names (e.g., `greaterThan`, `notEqual`) to eliminate ambiguity.
*   **Type Safety**: TypeScript definitions ensure correct usage of operators and values.
*   **Readability**: Code reads like natural language sentences.
*   **IDE Autocompletion**: Discover available methods and operators via IntelliSense.
*   **Zero-Cost Abstraction**: The builder compiles down to the exact same JSON AST used by the Rust engine today.

## 3. API Design Proposal

We propose a `PolicyBuilder` class (exposed as `p` or `Policy`) with static methods for root nodes and instance methods for chaining.

### 3.1. Field Comparisons

We propose using verbose names for clarity, avoiding abbreviations like `eq` or `gte` which can be confusing.

| Operator | Method Name | Example |
| :--- | :--- | :--- |
| **Equal** | `equal(value)` | `p.field('/role').equal('admin')` |
| **Not Equal** | `notEqual(value)` | `p.field('/status').notEqual('archived')` |
| **Greater Than** | `greaterThan(value)` | `p.field('/score').greaterThan(50)` |
| **Greater/Equal** | `greaterThanOrEqual(value)` | `p.field('/age').greaterThanOrEqual(18)` |
| **Less Than** | `lessThan(value)` | `p.field('/latency').lessThan(100)` |
| **Less/Equal** | `lessThanOrEqual(value)` | `p.field('/attempts').lessThanOrEqual(3)` |
| **In Array** | `in(array)` | `p.field('/role').in(['admin', 'editor'])` |
| **Contains** | `contains(value)` | `p.field('/tags').contains('urgent')` |
| **Exists** | `exists(path)` | `p.exists('/user/email')` |

### 3.2. Logical Operators

Logical operators combine multiple conditions.

**Current:**
```typescript
{
  kind: 'Or',
  children: [
    { kind: 'Field', path: '/role', value: 'admin' },
    {
      kind: 'And',
      children: [
        { kind: 'Field', path: '/role', value: 'user' },
        { kind: 'Exists', path: '/session/valid' }
      ]
    }
  ]
}
```

**Proposed:**
```typescript
p.or(
  p.field('/role').equal('admin'),
  p.and(
    p.field('/role').equal('user'),
    p.exists('/session/valid')
  )
)
```

### 3.3. Vibe Check (AI Confidence)

**Current:**
```typescript
{ kind: 'VibeCheck', value: 'frustrated', path: '0.8' }
```

**Proposed:**
```typescript
// Explicit semantic methods
p.vibe('frustrated').confidence(0.8)

// Or generic form
p.vibe().intent('frustrated').minConfidence(0.8)
```

### 3.4. Complex Real-World Example

**Scenario**: A "Approve Loan" activity.
*   **Allowed if**:
    *   User is an "approver" AND loan amount is < 10,000
    *   OR
    *   User is "senior_manager" (any amount)
    *   OR
    *   AI detects "urgent_request" intent with > 90% confidence.

**Current (JSON AST):**
```typescript
{
  kind: 'Or',
  children: [
    {
      kind: 'And',
      children: [
        { kind: 'Field', path: '/user/role', value: 'approver' },
        { kind: 'Lt', children: [ { kind: 'Field', path: '/loan/amount' }, { kind: 'Field', value: '10000' } ] }
      ]
    },
    { kind: 'Field', path: '/user/role', value: 'senior_manager' },
    { kind: 'VibeCheck', value: 'urgent_request', path: '0.9' }
  ]
}
```

**Proposed (Fluent Builder):**
```typescript
p.or(
  // Rule 1: Standard approver for small loans
  p.and(
    p.field('/user/role').equal('approver'),
    p.field('/loan/amount').lessThan(10000)
  ),
  // Rule 2: Senior manager override
  p.field('/user/role').equal('senior_manager'),
  // Rule 3: AI Emergency override
  p.vibe('urgent_request').confidence(0.9)
)
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
| **Learning Curve** | Medium (must learn AST schema) | Low (discoverable API, standard naming) |

## 6. Next Steps

1.  Approve this proposal.
2.  Implement `PolicyBuilder` in `@gp2f/server`.
3.  Update documentation and examples to use the new fluent syntax.
