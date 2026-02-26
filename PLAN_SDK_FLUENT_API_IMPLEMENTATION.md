# Technical Implementation Plan: Fluent Policy Builder API

This document provides a detailed, step-by-step technical guide for implementing the Fluent Policy Builder API proposed in `PROPOSAL_SDK_DX_IMPROVEMENT.md`.

## Phase 1: Foundation & Design

The goal is to create a TypeScript class that accumulates state and outputs a JSON AST conforming to the `AstNode` interface. This logic should be identical (or shared) between `@gp2f/client-sdk` and `@gp2f/server`.

### 1.1 Core Class Structure

The builder will use a chainable pattern. To support both top-level logical operators (AND/OR) and field-level comparisons, we need two main constructs:

1.  **`PolicyBuilder`**: The entry point and container for logical groupings.
2.  **`FieldBuilder`**: A specialized builder for field-specific assertions.

```typescript
// Shared Interface (from existing code)
interface AstNode {
  kind: string;
  children?: AstNode[];
  path?: string;
  value?: string;
  // ...
}

export interface Builder {
  build(): AstNode;
  toJSON(): AstNode;
}
```

## Phase 2: Client SDK Implementation (`client-sdk`)

### Step 2.1: Create `src/policy-builder.ts`

Create a new file to house the implementation.

### Step 2.2: Implement `FieldBuilder`

This class handles `p.field('/path').op(val)`.

```typescript
export class FieldBuilder implements Builder {
  constructor(private path: string) {}

  // -- Equality --
  equal(value: string): AstNode { return this.op('Eq', value); }
  eq(value: string): AstNode { return this.equal(value); }

  notEqual(value: string): AstNode { return this.op('Neq', value); }
  neq(value: string): AstNode { return this.notEqual(value); }

  // -- Comparisons --
  greaterThan(value: string | number): AstNode { return this.op('Gt', String(value)); }
  gt(value: string | number): AstNode { return this.greaterThan(value); }
  // ... implement gte, lt, lte similarly ...

  // -- Collection --
  in(values: string[]): AstNode {
    // Construct 'In' node: left=value(arg), right=field(path) ???
    // Actually standard is: value IN field (array) OR field IN value (array)
    // Adjust based on engine semantics.
    return {
      kind: 'In',
      children: [
        { kind: 'Literal', value: JSON.stringify(values) }, // Simplified
        { kind: 'Field', path: this.path }
      ]
    };
  }

  // Internal helper
  private op(kind: string, value: string): AstNode {
    return {
      kind,
      children: [
        { kind: 'Field', path: this.path },
        { kind: 'Literal', value } // or simple value node depending on AST spec
      ]
    };
  }

  build(): AstNode { throw new Error("FieldBuilder must end with an operator"); }
  toJSON(): AstNode { return this.build(); }
}
```

*Correction*: The current AST spec uses `value` property on leaf nodes for scalars, or `children` for binary ops. Ensure `op()` matches the Rust `Evaluator` expectation exactly.

### Step 2.3: Implement `PolicyBuilder` (The Entry Point)

```typescript
export class PolicyBuilder {
  // -- Field Entry Point --
  static field(path: string): FieldBuilder {
    return new FieldBuilder(path);
  }

  // -- Logical Operators --
  static and(...nodes: (AstNode | Builder)[]): AstNode {
    return {
      kind: 'And',
      children: nodes.map(n => 'build' in n ? n.build() : n)
    };
  }

  static or(...nodes: (AstNode | Builder)[]): AstNode {
    return {
      kind: 'Or',
      children: nodes.map(n => 'build' in n ? n.build() : n)
    };
  }

  static not(node: AstNode | Builder): AstNode {
    return {
      kind: 'Not',
      children: ['build' in node ? node.build() : node]
    };
  }

  // -- Existence --
  static exists(path: string): AstNode {
    return { kind: 'Exists', path };
  }

  // -- Vibe --
  static vibe(intent: string): VibeBuilder {
    return new VibeBuilder(intent);
  }
}

// Shorthand export
export const p = PolicyBuilder;
```

### Step 2.4: Export in `src/index.ts`

```typescript
export { p, PolicyBuilder } from './policy-builder';
```

## Phase 3: Server SDK Implementation (`gp2f-node`)

The server SDK is a native Node.js addon. We cannot easily "import" the client SDK TS code directly without a build step that might complicate the native bindings.

**Recommendation**: Duplicate the lightweight TS implementation into `gp2f-node` to avoid complex monorepo linking issues for now, OR create a shared `gp2f-common` package later. For this task, we will implement it directly in `gp2f-node`.

### Step 3.1: Create `gp2f-node/lib/policy-builder.js`

Since `gp2f-node` often ships as a native module, we should ensure the builder is available as a standard JS module that accompanies the binary.

1.  Write the implementation in `lib/policy-builder.js` (or `.ts` if there's a compilation step).
2.  Update `index.js` to re-export `p`:
    ```javascript
    const { p } = require('./lib/policy-builder');
    module.exports = { ...native, p, PolicyBuilder: p };
    ```

### Step 3.2: Update `index.d.ts`

Manually update the definition file to include the types for `PolicyBuilder` so users get IntelliSense.

## Phase 4: Integration & Helper Methods

### Step 4.1: Update `Workflow.addActivity`

Modify the type definition for `addActivity` to accept the builder.

**Before:**
```typescript
addActivity(name: string, config: { policy: AstNode, ... }): void
```

**After:**
```typescript
type PolicyInput = AstNode | Builder;

addActivity(name: string, config: { policy: PolicyInput, ... }): void
```

**Runtime logic:**
Inside `addActivity`, check if `policy` has a `.build()` or `.toJSON()` method. If so, call it to get the raw AST before passing it to the Rust layer.

## Phase 5: Testing

### Step 5.1: Unit Tests

Create `__tests__/policy-builder.spec.ts` in both packages.

*   **Test Equality**: `p.field('a').eq('b')` should output `{ kind: 'Eq', ... }`.
*   **Test Nesting**: `p.and(p.field('a').eq('1'), p.or(...))` should nest children correctly.
*   **Test Aliases**: Verify `eq` and `equal` produce identical output.

## Phase 6: Documentation

### Step 6.1: Update README

Replace verbose JSON examples with the new fluent syntax in the main `README.md` and package-specific READMEs. Keep one "Advanced" section showing the raw JSON AST for reference.
