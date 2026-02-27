'use strict'

/**
 * Fluent Policy Builder API for constructing GP2F policy AST nodes.
 *
 * Provides a chainable alternative to writing raw JSON AST objects.
 *
 * @example
 * ```js
 * const { p } = require('@gp2f/server');
 *
 * const policy = p.and(
 *   p.field('/user/role').eq('admin'),
 *   p.exists('/session/token'),
 * );
 * ```
 */

// ── Internal helper ───────────────────────────────────────────────────────────

function resolve(node) {
  return node && typeof node.build === 'function' ? node.build() : node
}

// ── FieldBuilder ──────────────────────────────────────────────────────────────

class FieldBuilder {
  constructor(path) {
    this._path = path
  }

  _op(kind, value) {
    return {
      kind,
      children: [{ kind: 'Field', path: this._path }],
      value: String(value)
    }
  }

  // Equality
  equal(value) { return this._op('Eq', value) }
  eq(value) { return this.equal(value) }
  notEqual(value) { return this._op('Neq', value) }
  neq(value) { return this.notEqual(value) }

  // Comparisons
  greaterThan(value) { return this._op('Gt', value) }
  gt(value) { return this.greaterThan(value) }
  greaterThanOrEqual(value) { return this._op('Gte', value) }
  gte(value) { return this.greaterThanOrEqual(value) }
  lessThan(value) { return this._op('Lt', value) }
  lt(value) { return this.lessThan(value) }
  lessThanOrEqual(value) { return this._op('Lte', value) }
  lte(value) { return this.lessThanOrEqual(value) }

  // Collection
  in(values) {
    return {
      kind: 'In',
      children: [{ kind: 'Field', path: this._path }],
      value: JSON.stringify(values)
    }
  }

  contains(value) {
    return {
      kind: 'Contains',
      children: [{ kind: 'Field', path: this._path }],
      value: String(value)
    }
  }

  build() {
    throw new Error(
      'FieldBuilder must be terminated with an operator (e.g. .eq(), .gt())'
    )
  }

  toJSON() {
    return this.build()
  }
}

// ── VibeBuilder ───────────────────────────────────────────────────────────────

class VibeBuilder {
  constructor(intent) {
    this._intent = intent
    this._threshold = undefined
  }

  withConfidence(threshold) {
    this._threshold = threshold
    return this
  }

  build() {
    const node = { kind: 'VibeCheck', value: this._intent }
    if (this._threshold !== undefined) {
      node.path = String(this._threshold)
    }
    return node
  }

  toJSON() {
    return this.build()
  }
}

// ── PolicyBuilder ─────────────────────────────────────────────────────────────

class PolicyBuilder {
  static field(path) {
    return new FieldBuilder(path)
  }

  static and(...nodes) {
    return { kind: 'And', children: nodes.map(resolve) }
  }

  static or(...nodes) {
    return { kind: 'Or', children: nodes.map(resolve) }
  }

  static not(node) {
    return { kind: 'Not', children: [resolve(node)] }
  }

  static exists(path) {
    return { kind: 'Exists', path }
  }

  static literalTrue() {
    return { kind: 'LiteralTrue' }
  }

  static literalFalse() {
    return { kind: 'LiteralFalse' }
  }

  static vibe(intent) {
    return new VibeBuilder(intent)
  }
}

const p = PolicyBuilder

module.exports = { p, PolicyBuilder, FieldBuilder, VibeBuilder }
