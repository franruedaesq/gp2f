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
*   **Dual Naming Strategy**: Support both verbose (e.g., `greaterThan`) and abbreviated (e.g., `gt`) method names to suit different developer preferences.
*   **Type Safety**: TypeScript definitions ensure correct usage of operators and values.
*   **Readability**: Code reads like natural language sentences.
*   **IDE Autocompletion**: Discover available methods and operators via IntelliSense.
*   **Zero-Cost Abstraction**: The builder compiles down to the exact same JSON AST used by the Rust engine today.

## 3. API Design Proposal

We propose a `PolicyBuilder` class (exposed as `p` or `Policy`) with static methods for root nodes and instance methods for chaining.

### 3.1. Field Comparisons

We will support aliases to provide flexibility:

| Operator | Verbose Name | Alias | Example |
| :--- | :--- | :--- | :--- |
| **Equal** | `equal(value)` | `eq(value)` | `p.field('/role').eq('admin')` |
| **Not Equal** | `notEqual(value)` | `neq(value)` | `p.field('/status').notEqual('archived')` |
| **Greater Than** | `greaterThan(value)` | `gt(value)` | `p.field('/score').gt(50)` |
| **Greater/Equal** | `greaterThanOrEqual(value)` | `gte(value)` | `p.field('/age').gte(18)` |
| **Less Than** | `lessThan(value)` | `lt(value)` | `p.field('/latency').lessThan(100)` |
| **Less/Equal** | `lessThanOrEqual(value)` | `lte(value)` | `p.field('/attempts').lte(3)` |
| **In Array** | `in(array)` | - | `p.field('/role').in(['admin', 'editor'])` |
| **Contains** | `contains(value)` | - | `p.field('/tags').contains('urgent')` |
| **Exists** | `exists(path)` | - | `p.exists('/user/email')` |

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
    p.field('/user/role').eq('approver'),
    p.field('/loan/amount').lt(10000)
  ),
  // Rule 2: Senior manager override
  p.field('/user/role').equal('senior_manager'),
  // Rule 3: AI Emergency override
  p.vibe('urgent_request').confidence(0.9)
)
```

## 4. Implementation Strategy & Impact

### 4.1. Difficulty Level: Low to Medium

Supporting aliases (e.g., `eq` alongside `equal`) is trivial in the builder implementation; they can simply map to the same underlying AST generation logic. The main effort lies in ensuring the TypeScript definitions (`.d.ts`) are comprehensive and well-documented for both sets of methods.

### 4.2. Impacted Files

The implementation will primarily involve creating new files rather than modifying the core engine, minimizing risk.

*   **`client-sdk/src/policy-builder.ts`** (New):
    *   Primary implementation of the fluent API for the client SDK.
    *   Will export the `p` (or `Policy`) builder object.
*   **`gp2f-node/src/policy-builder.ts`** (New):
    *   Equivalent implementation for the server-side Node.js bindings.
    *   Ideally, this code should be shared/reused from a common package to avoid duplication.
*   **`client-sdk/src/index.ts`** & **`gp2f-node/index.d.ts`**:
    *   Updated to export the new builder API.
*   **Documentation**:
    *   `README.md`, `docs/sdk-reference/*.md`: Examples will need to be updated to showcase the new syntax.

### 4.3. Migration Path

This is an **additive change**. The existing raw JSON AST format will continue to work exactly as before. Users can mix and match or migrate incrementally. No breaking changes are required.

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
