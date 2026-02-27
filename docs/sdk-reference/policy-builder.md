# Fluent Policy Builder Reference

The **Fluent Policy Builder** is a chainable, type-safe API for constructing GP2F policy ASTs without writing raw JSON objects. It is available in both the TypeScript client SDK (`@gp2f/client-sdk`) and the Node.js native bindings (`@gp2f/server`).

---

## Importing

**TypeScript / ESM (client-sdk):**

```typescript
import { p, PolicyBuilder, FieldBuilder, VibeBuilder } from '@gp2f/client-sdk';
```

**Node.js / CommonJS (server bindings):**

```javascript
const { p, PolicyBuilder, FieldBuilder, VibeBuilder } = require('@gp2f/server');
```

`p` is a convenient shorthand alias for `PolicyBuilder`. The two are identical:

```typescript
p.and(...)            // shorthand
PolicyBuilder.and(...)// same thing
```

---

## Quick Example

```typescript
import { p } from '@gp2f/client-sdk';

// Require the user to be an admin OR a superuser,
// AND have an active session token.
const policy = p.and(
  p.or(
    p.field('/user/role').eq('admin'),
    p.field('/user/role').eq('superuser'),
  ),
  p.exists('/session/token'),
);
```

The resulting `policy` object is a plain `AstNode` that can be passed directly to `evaluate()`, `evaluateWithTrace()`, `addActivity()`, or any other API that accepts a policy AST.

---

## `PolicyBuilder` – Static Methods

All methods are static. Use `PolicyBuilder` or the `p` shorthand directly without instantiation.

### `PolicyBuilder.field(path: string): FieldBuilder`

Begins a field assertion for the JSON-pointer `path`. Returns a [`FieldBuilder`](#fieldbuilder) that can be chained with a comparison operator.

```typescript
p.field('/user/role').eq('admin')
p.field('/item/price').lte(99.99)
p.field('/tags').contains('urgent')
```

---

### `PolicyBuilder.and(...nodes): AstNode`

Combines two or more nodes with a logical **AND**. All children must evaluate to `true` for the `And` node to be `true`.

```typescript
const policy = p.and(
  p.field('/role').eq('clinician'),
  p.exists('/patient_id'),
  p.field('/shift/active').eq('true'),
);
```

Accepts both raw `AstNode` objects and `Builder` instances interchangeably:

```typescript
const rawNode: AstNode = { kind: 'LiteralTrue' };
p.and(rawNode, p.field('/x').eq('1'));
```

---

### `PolicyBuilder.or(...nodes): AstNode`

Combines two or more nodes with a logical **OR**. At least one child must be `true`.

```typescript
const policy = p.or(
  p.field('/role').eq('admin'),
  p.field('/role').eq('superuser'),
);
```

---

### `PolicyBuilder.not(node): AstNode`

Negates a single node with a logical **NOT**.

```typescript
const policy = p.not(p.field('/account/suspended').eq('true'));
```

---

### `PolicyBuilder.exists(path: string): AstNode`

Asserts that the JSON-pointer `path` exists (is non-null) in the state document.

```typescript
p.exists('/session/token')
p.exists('/user/consent')
```

---

### `PolicyBuilder.literalTrue(): AstNode`

A policy that always evaluates to `true`. Useful as a placeholder or to allow all operations.

```typescript
p.literalTrue()
// => { kind: 'LiteralTrue' }
```

---

### `PolicyBuilder.literalFalse(): AstNode`

A policy that always evaluates to `false`. Useful to disable an activity without removing it.

```typescript
p.literalFalse()
// => { kind: 'LiteralFalse' }
```

---

### `PolicyBuilder.vibe(intent: string): VibeBuilder`

Begins a [Semantic Vibe Engine](#vibebuilder) check for the given intent string. Returns a [`VibeBuilder`](#vibebuilder).

```typescript
p.vibe('frustrated').withConfidence(0.8).build()
```

---

## `FieldBuilder`

Obtained via `p.field(path)`. Provides comparison and collection operators that each return a completed `AstNode`.

### Equality

| Method | Alias | Description |
|--------|-------|-------------|
| `equal(value)` | `eq(value)` | Field equals `value` |
| `notEqual(value)` | `neq(value)` | Field does not equal `value` |

```typescript
p.field('/status').eq('active')
p.field('/status').notEqual('banned')
```

### Comparisons

| Method | Alias | Description |
|--------|-------|-------------|
| `greaterThan(value)` | `gt(value)` | Field > `value` |
| `greaterThanOrEqual(value)` | `gte(value)` | Field >= `value` |
| `lessThan(value)` | `lt(value)` | Field < `value` |
| `lessThanOrEqual(value)` | `lte(value)` | Field <= `value` |

```typescript
p.field('/age').gt(18)
p.field('/score').gte(100)
p.field('/retries').lt(3)
p.field('/balance').lte(1000)
```

Numeric values are coerced to strings internally. The Rust evaluator parses them back to numbers for the comparison.

### Collection

| Method | Description |
|--------|-------------|
| `in(values: string[])` | Field value is one of the provided array values |
| `contains(value: string)` | Field (string or array) contains `value` |

```typescript
p.field('/role').in(['admin', 'editor', 'reviewer'])
p.field('/tags').contains('urgent')
```

---

## `VibeBuilder`

Obtained via `p.vibe(intent)`. Builds a `VibeCheck` node that gates on the Semantic Vibe Engine output.

### `withConfidence(threshold: number): this`

Sets the minimum confidence level (0–1) required for the check to pass. If omitted, only the intent is checked.

```typescript
// Passes when intent == 'frustrated' AND confidence >= 0.8
p.vibe('frustrated').withConfidence(0.8).build()
// => { kind: 'VibeCheck', value: 'frustrated', path: '0.8' }

// Passes whenever intent == 'frustrated' (any confidence)
p.vibe('frustrated').build()
// => { kind: 'VibeCheck', value: 'frustrated' }
```

### `build(): AstNode`

Produces the final `AstNode`. Called implicitly when a `VibeBuilder` is passed to `p.and()`, `p.or()`, etc.

---

## Composing Policies

Builders can be nested to any depth:

```typescript
const policy = p.and(
  // Must be a clinician or an admin
  p.or(
    p.field('/role').eq('clinician'),
    p.field('/role').eq('admin'),
  ),
  // Patient must exist and not be discharged
  p.exists('/patient_id'),
  p.not(p.field('/patient/status').eq('discharged')),
  // Proactive AI assist when confidence is high enough
  p.vibe('needs_help').withConfidence(0.75),
);
```

---

## Using Builder Output

The builder always returns a plain `AstNode` object (or a `Builder` instance for intermediate steps). You can use it anywhere an `AstNode` is accepted:

**With `evaluate` / `evaluateWithTrace`:**

```typescript
import { evaluate, p } from '@gp2f/server';

const policy = p.and(
  p.field('/role').eq('admin'),
  p.exists('/session/token'),
);

const allowed = evaluate(policy, { role: 'admin', session: { token: 'abc' } });
// => true
```

**As a workflow activity policy:**

```typescript
import { Workflow, p } from '@gp2f/server';

const wf = new Workflow('document-approval');
wf.addActivity(
  'review',
  { policy: p.field('/role').in(['reviewer', 'admin']) },
  async (ctx) => { /* ... */ },
);
```

**Serializing to JSON (for storage or transmission):**

```typescript
const policy = p.and(
  p.field('/role').eq('admin'),
  p.exists('/token'),
);
const json = JSON.stringify(policy);
// {"kind":"And","children":[{"kind":"Eq","children":[{"kind":"Field","path":"/role"},{"kind":"Literal","value":"admin"}]},{"kind":"Exists","path":"/token"}]}
```

---

## `AstNode` Shape

Every builder method returns an `AstNode`:

```typescript
interface AstNode {
  kind: string;          // e.g. 'And', 'Eq', 'Field', 'Exists', 'VibeCheck'
  children?: AstNode[];  // present for composite operators
  path?: string;         // JSON-pointer path for Field / Exists nodes
  value?: string;        // scalar value for leaf nodes
  callName?: string;     // external function name for Call nodes
}
```

The `kind` values used by the builder map directly to the `NodeKind` enum in `policy-core`:

| Builder method | `kind` produced |
|---------------|-----------------|
| `p.and(...)` | `"And"` |
| `p.or(...)` | `"Or"` |
| `p.not(...)` | `"Not"` |
| `p.field('/x').eq('v')` | `"Eq"` |
| `p.field('/x').neq('v')` | `"Neq"` |
| `p.field('/x').gt(n)` | `"Gt"` |
| `p.field('/x').gte(n)` | `"Gte"` |
| `p.field('/x').lt(n)` | `"Lt"` |
| `p.field('/x').lte(n)` | `"Lte"` |
| `p.field('/x').in([...])` | `"In"` |
| `p.field('/x').contains('v')` | `"Contains"` |
| `p.exists('/x')` | `"Exists"` |
| `p.literalTrue()` | `"LiteralTrue"` |
| `p.literalFalse()` | `"LiteralFalse"` |
| `p.vibe('intent')` | `"VibeCheck"` |

---

## Related Documentation

- [Node.js Native Bindings (`@gp2f/server`)](nodejs-bindings.md)
- [TypeScript Frontend Bindings (`@gp2f/client-sdk`)](typescript-bindings.md)
- [Rust Core API (`policy-core`)](rust-core-api.md)
- [Policy AST Reference](../../README.md#policy-ast-reference)
