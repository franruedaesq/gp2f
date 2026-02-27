'use strict'

/**
 * Unit tests for the Fluent Policy Builder API in @gp2f/server.
 */

const { p, PolicyBuilder, FieldBuilder, VibeBuilder } = require('../lib/policy-builder')

describe('PolicyBuilder – exports', () => {
  test('p is an alias for PolicyBuilder', () => {
    expect(p).toBe(PolicyBuilder)
  })

  test('PolicyBuilder.field returns a FieldBuilder', () => {
    expect(p.field('/role')).toBeInstanceOf(FieldBuilder)
  })

  test('PolicyBuilder.vibe returns a VibeBuilder', () => {
    expect(p.vibe('frustrated')).toBeInstanceOf(VibeBuilder)
  })
})

describe('FieldBuilder – equality operators', () => {
  test('eq produces an Eq node with value property', () => {
    const node = p.field('/role').eq('admin')
    expect(node).toEqual({
      kind: 'Eq',
      children: [
        { kind: 'Field', path: '/role' },
      ],
      value: 'admin'
    })
  })

  test('equal is an alias for eq', () => {
    expect(p.field('/role').equal('admin')).toEqual(p.field('/role').eq('admin'))
  })

  test('neq produces a Neq node', () => {
    const node = p.field('/status').neq('banned')
    expect(node.kind).toBe('Neq')
    expect(node.value).toBe('banned')
  })

  test('notEqual is an alias for neq', () => {
    expect(p.field('/x').notEqual('y')).toEqual(p.field('/x').neq('y'))
  })
})

describe('FieldBuilder – comparison operators', () => {
  test('gt produces a Gt node', () => {
    const node = p.field('/age').gt(18)
    expect(node.kind).toBe('Gt')
    expect(node.value).toBe('18')
  })

  test('greaterThan is an alias for gt', () => {
    expect(p.field('/age').greaterThan(18)).toEqual(p.field('/age').gt(18))
  })

  test('gte produces a Gte node', () => {
    expect(p.field('/score').gte(100).kind).toBe('Gte')
  })

  test('greaterThanOrEqual is an alias for gte', () => {
    expect(p.field('/score').greaterThanOrEqual(100)).toEqual(p.field('/score').gte(100))
  })

  test('lt produces a Lt node', () => {
    expect(p.field('/count').lt(5).kind).toBe('Lt')
  })

  test('lessThan is an alias for lt', () => {
    expect(p.field('/count').lessThan(5)).toEqual(p.field('/count').lt(5))
  })

  test('lte produces a Lte node', () => {
    expect(p.field('/count').lte(10).kind).toBe('Lte')
  })

  test('lessThanOrEqual is an alias for lte', () => {
    expect(p.field('/count').lessThanOrEqual(10)).toEqual(p.field('/count').lte(10))
  })
})

describe('FieldBuilder – collection operators', () => {
  test('in produces an In node with JSON-serialised array', () => {
    const node = p.field('/role').in(['admin', 'editor'])
    expect(node.kind).toBe('In')
    expect(node.children[0]).toEqual({ kind: 'Field', path: '/role' })
    expect(node.value).toBe('["admin","editor"]')
  })

  test('contains produces a Contains node', () => {
    const node = p.field('/tags').contains('urgent')
    expect(node.kind).toBe('Contains')
    expect(node.value).toBe('urgent')
  })
})

describe('PolicyBuilder – logical operators', () => {
  test('and produces an And node with children', () => {
    const node = p.and(
      p.field('/role').eq('admin'),
      p.field('/active').eq('true'),
    )
    expect(node.kind).toBe('And')
    expect(node.children).toHaveLength(2)
    expect(node.children[0].kind).toBe('Eq')
    expect(node.children[1].kind).toBe('Eq')
  })

  test('or produces an Or node with children', () => {
    const node = p.or(
      p.field('/role').eq('admin'),
      p.field('/role').eq('superuser'),
    )
    expect(node.kind).toBe('Or')
    expect(node.children).toHaveLength(2)
  })

  test('not produces a Not node with one child', () => {
    const node = p.not(p.field('/banned').eq('true'))
    expect(node.kind).toBe('Not')
    expect(node.children).toHaveLength(1)
    expect(node.children[0].kind).toBe('Eq')
  })

  test('and accepts raw AstNode objects alongside builders', () => {
    const rawNode = { kind: 'LiteralTrue' }
    const node = p.and(rawNode, p.field('/x').eq('1'))
    expect(node.children[0]).toEqual(rawNode)
    expect(node.children[1].kind).toBe('Eq')
  })
})

describe('PolicyBuilder – existence and literals', () => {
  test('exists produces an Exists node with path', () => {
    const node = p.exists('/session/token')
    expect(node).toEqual({ kind: 'Exists', path: '/session/token' })
  })

  test('literalTrue produces a LiteralTrue node', () => {
    expect(p.literalTrue()).toEqual({ kind: 'LiteralTrue' })
  })

  test('literalFalse produces a LiteralFalse node', () => {
    expect(p.literalFalse()).toEqual({ kind: 'LiteralFalse' })
  })
})

describe('VibeBuilder', () => {
  test('vibe builds a VibeCheck node with intent', () => {
    const node = p.vibe('frustrated').build()
    expect(node).toEqual({ kind: 'VibeCheck', value: 'frustrated' })
  })

  test('withConfidence adds a path threshold', () => {
    const node = p.vibe('frustrated').withConfidence(0.8).build()
    expect(node).toEqual({ kind: 'VibeCheck', value: 'frustrated', path: '0.8' })
  })

  test('toJSON calls build', () => {
    const vb = p.vibe('calm')
    expect(vb.toJSON()).toEqual(vb.build())
  })
})

describe('Nested composition', () => {
  test('deeply nested and/or/not resolves correctly', () => {
    const policy = p.and(
      p.field('/role').eq('admin'),
      p.or(
        p.field('/tier').eq('gold'),
        p.not(p.field('/suspended').eq('true')),
      ),
    )
    expect(policy.kind).toBe('And')
    expect(policy.children[1].kind).toBe('Or')
    expect(policy.children[1].children[1].kind).toBe('Not')
  })
})
