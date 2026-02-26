/**
 * Fluent Policy Builder API for constructing GP2F policy AST nodes.
 *
 * Provides a chainable, type-safe alternative to writing raw JSON AST objects.
 *
 * @example
 * ```ts
 * import { p } from '@gp2f/client-sdk';
 *
 * const policy = p.and(
 *   p.field('/user/role').eq('admin'),
 *   p.field('/session/active').equal('true'),
 *   p.exists('/session/token'),
 * );
 * ```
 */

// ── Shared interfaces ─────────────────────────────────────────────────────────

/**
 * A node in the GP2F policy AST.
 *
 * Mirrors `policy_core::AstNode` from Rust.
 */
export interface AstNode {
  /** The operation this node performs. */
  kind: string;
  /** Child nodes for composite operators (And, Or, Not, comparison ops). */
  children?: AstNode[];
  /** JSON-pointer path used by Field and Exists nodes (e.g. `/user/role`). */
  path?: string;
  /** String-encoded scalar value for leaf nodes (e.g. `"admin"`, `"42"`). */
  value?: string;
  /** Name of the external function – used only by Call nodes. */
  callName?: string;
}

/**
 * Shared interface for all builder objects that can produce an AstNode.
 */
export interface Builder {
  build(): AstNode;
  toJSON(): AstNode;
}

// ── FieldBuilder ──────────────────────────────────────────────────────────────

/**
 * A builder for field-specific policy assertions.
 *
 * Obtained via `p.field('/some/path')`.
 */
export class FieldBuilder implements Builder {
  constructor(private readonly _path: string) {}

  // -- Equality --

  /** Assert that the field equals `value`. */
  equal(value: string | number): AstNode {
    return this._op("Eq", String(value));
  }

  /** Alias for {@link equal}. */
  eq(value: string | number): AstNode {
    return this.equal(value);
  }

  /** Assert that the field does not equal `value`. */
  notEqual(value: string | number): AstNode {
    return this._op("Neq", String(value));
  }

  /** Alias for {@link notEqual}. */
  neq(value: string | number): AstNode {
    return this.notEqual(value);
  }

  // -- Comparisons --

  /** Assert that the field is greater than `value`. */
  greaterThan(value: string | number): AstNode {
    return this._op("Gt", String(value));
  }

  /** Alias for {@link greaterThan}. */
  gt(value: string | number): AstNode {
    return this.greaterThan(value);
  }

  /** Assert that the field is greater than or equal to `value`. */
  greaterThanOrEqual(value: string | number): AstNode {
    return this._op("Gte", String(value));
  }

  /** Alias for {@link greaterThanOrEqual}. */
  gte(value: string | number): AstNode {
    return this.greaterThanOrEqual(value);
  }

  /** Assert that the field is less than `value`. */
  lessThan(value: string | number): AstNode {
    return this._op("Lt", String(value));
  }

  /** Alias for {@link lessThan}. */
  lt(value: string | number): AstNode {
    return this.lessThan(value);
  }

  /** Assert that the field is less than or equal to `value`. */
  lessThanOrEqual(value: string | number): AstNode {
    return this._op("Lte", String(value));
  }

  /** Alias for {@link lessThanOrEqual}. */
  lte(value: string | number): AstNode {
    return this.lessThanOrEqual(value);
  }

  // -- Collection --

  /**
   * Assert that the field value is contained in the given array.
   *
   * Produces an `In` node where the left child is a Field and the right child
   * holds the serialised array value.
   */
  in(values: string[]): AstNode {
    return {
      kind: "In",
      children: [
        { kind: "Field", path: this._path },
        { kind: "Literal", value: JSON.stringify(values) },
      ],
    };
  }

  /**
   * Assert that the field (string or array) contains `value`.
   *
   * Produces a `Contains` node.
   */
  contains(value: string): AstNode {
    return {
      kind: "Contains",
      children: [
        { kind: "Field", path: this._path },
        { kind: "Literal", value },
      ],
    };
  }

  // -- Builder interface --

  build(): AstNode {
    throw new Error(
      "FieldBuilder must be terminated with an operator (e.g. .eq(), .gt())",
    );
  }

  toJSON(): AstNode {
    return this.build();
  }

  // -- Internal --

  private _op(kind: string, value: string): AstNode {
    return {
      kind,
      children: [
        { kind: "Field", path: this._path },
        { kind: "Literal", value },
      ],
    };
  }
}

// ── VibeBuilder ───────────────────────────────────────────────────────────────

/**
 * Builder for Semantic Vibe Engine checks.
 *
 * Obtained via `p.vibe('intent')`.
 */
export class VibeBuilder implements Builder {
  private _threshold?: number;

  constructor(private readonly _intent: string) {}

  /**
   * Set the minimum confidence threshold (0–1) for the vibe check.
   *
   * @example
   * ```ts
   * p.vibe('frustrated').withConfidence(0.8)
   * ```
   */
  withConfidence(threshold: number): this {
    this._threshold = threshold;
    return this;
  }

  build(): AstNode {
    return {
      kind: "VibeCheck",
      value: this._intent,
      ...(this._threshold !== undefined
        ? { path: String(this._threshold) }
        : {}),
    };
  }

  toJSON(): AstNode {
    return this.build();
  }
}

// ── PolicyBuilder ─────────────────────────────────────────────────────────────

/** Resolve a value that may be a raw `AstNode` or any `Builder` instance. */
function resolve(node: AstNode | Builder): AstNode {
  return "build" in node && typeof (node as Builder).build === "function"
    ? (node as Builder).build()
    : (node as AstNode);
}

/**
 * Entry point for the fluent policy builder API.
 *
 * All methods are static – use `p` (the shorthand export) or `PolicyBuilder`
 * directly without instantiation.
 *
 * @example
 * ```ts
 * import { p } from '@gp2f/client-sdk';
 *
 * const policy = p.and(
 *   p.field('/role').eq('admin'),
 *   p.exists('/session/token'),
 * );
 * ```
 */
export class PolicyBuilder {
  // -- Field entry point --

  /**
   * Begin a field assertion for the JSON-pointer `path`.
   *
   * Returns a {@link FieldBuilder} that can be chained with comparison
   * operators (`eq`, `gt`, `in`, …).
   */
  static field(path: string): FieldBuilder {
    return new FieldBuilder(path);
  }

  // -- Logical operators --

  /** Combine multiple nodes with a logical AND. */
  static and(...nodes: (AstNode | Builder)[]): AstNode {
    return {
      kind: "And",
      children: nodes.map(resolve),
    };
  }

  /** Combine multiple nodes with a logical OR. */
  static or(...nodes: (AstNode | Builder)[]): AstNode {
    return {
      kind: "Or",
      children: nodes.map(resolve),
    };
  }

  /** Negate a node with a logical NOT. */
  static not(node: AstNode | Builder): AstNode {
    return {
      kind: "Not",
      children: [resolve(node)],
    };
  }

  // -- Existence --

  /** Assert that the JSON-pointer `path` exists (is non-null) in the state. */
  static exists(path: string): AstNode {
    return { kind: "Exists", path };
  }

  // -- Literal shortcuts --

  /** A policy that always evaluates to `true`. */
  static literalTrue(): AstNode {
    return { kind: "LiteralTrue" };
  }

  /** A policy that always evaluates to `false`. */
  static literalFalse(): AstNode {
    return { kind: "LiteralFalse" };
  }

  // -- Vibe Engine --

  /**
   * Begin a Semantic Vibe Engine check for the given intent string.
   *
   * Optionally chain `.withConfidence(threshold)` before using the node.
   */
  static vibe(intent: string): VibeBuilder {
    return new VibeBuilder(intent);
  }
}

/**
 * Shorthand alias for {@link PolicyBuilder}.
 *
 * @example
 * ```ts
 * import { p } from '@gp2f/client-sdk';
 * const policy = p.and(p.field('/role').eq('admin'), p.exists('/token'));
 * ```
 */
export const p = PolicyBuilder;
