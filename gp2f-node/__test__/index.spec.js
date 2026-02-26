'use strict'

/**
 * Unit tests for @gp2f/server native bindings.
 *
 * These tests validate the JavaScript API contract via the TypeScript
 * declarations without requiring a compiled native addon – they test the
 * structural / contract behaviour that can be observed at the JS layer.
 *
 * Integration tests that actually load the .node binary live in the
 * `__test__/integration.spec.js` file and are skipped unless the
 * GP2F_NATIVE_BUILD environment variable is set.
 */

// ── Contract tests (no native addon required) ─────────────────────────────

describe('@gp2f/server – module contract', () => {
  test('package.json names the package @gp2f/server', () => {
    const pkg = require('../package.json')
    expect(pkg.name).toBe('@gp2f/server')
  })

  test('index.d.ts declares GP2FServer, Workflow, evaluate, evaluateWithTrace', () => {
    const fs = require('fs')
    const path = require('path')
    const dts = fs.readFileSync(path.join(__dirname, '..', 'index.d.ts'), 'utf8')

    expect(dts).toContain('export class GP2FServer')
    expect(dts).toContain('export class Workflow')
    expect(dts).toContain('export function evaluate')
    expect(dts).toContain('export function evaluateWithTrace')
  })

  test('index.d.ts declares ActivityConfig interface', () => {
    const fs = require('fs')
    const path = require('path')
    const dts = fs.readFileSync(path.join(__dirname, '..', 'index.d.ts'), 'utf8')

    expect(dts).toContain('export interface ActivityConfig')
    expect(dts).toContain('policy: AstNode')
    expect(dts).toContain('compensationRef?')
    expect(dts).toContain('isLocal?')
  })

  test('index.d.ts declares ServerConfig interface', () => {
    const fs = require('fs')
    const path = require('path')
    const dts = fs.readFileSync(path.join(__dirname, '..', 'index.d.ts'), 'utf8')

    expect(dts).toContain('export interface ServerConfig')
    expect(dts).toContain('port?')
    expect(dts).toContain('host?')
  })

  test('index.d.ts declares ExecutionContext interface', () => {
    const fs = require('fs')
    const path = require('path')
    const dts = fs.readFileSync(path.join(__dirname, '..', 'index.d.ts'), 'utf8')

    expect(dts).toContain('export interface ExecutionContext')
    expect(dts).toContain('instanceId')
    expect(dts).toContain('tenantId')
    expect(dts).toContain('activityName')
    expect(dts).toContain('stateJson')
  })

  test('index.d.ts declares NodeKind union type', () => {
    const fs = require('fs')
    const path = require('path')
    const dts = fs.readFileSync(path.join(__dirname, '..', 'index.d.ts'), 'utf8')

    expect(dts).toContain("'LiteralTrue'")
    expect(dts).toContain("'LiteralFalse'")
    expect(dts).toContain("'And'")
    expect(dts).toContain("'Or'")
    expect(dts).toContain("'Field'")
  })

  test('index.js loader file exists', () => {
    const fs = require('fs')
    const path = require('path')
    expect(fs.existsSync(path.join(__dirname, '..', 'index.js'))).toBe(true)
  })
})

// ── Native addon tests (skipped if .node binary not present) ──────────────

const NATIVE_SKIP = !process.env.GP2F_NATIVE_BUILD
const describeNative = NATIVE_SKIP ? describe.skip : describe

describeNative('@gp2f/server – native addon', () => {
  let native

  beforeAll(() => {
    // When GP2F_NATIVE_BUILD is set we expect the .node file to be present.
    native = require('../index.js')
  })

  test('exports evaluate function', () => {
    expect(typeof native.evaluate).toBe('function')
  })

  test('exports evaluateWithTrace function', () => {
    expect(typeof native.evaluateWithTrace).toBe('function')
  })

  test('exports Workflow class', () => {
    expect(typeof native.Workflow).toBe('function')
  })

  test('exports GP2FServer class', () => {
    expect(typeof native.GP2FServer).toBe('function')
  })

  test('evaluate: LITERAL_TRUE policy always returns true', () => {
    const result = native.evaluate({ kind: 'LiteralTrue' }, {})
    expect(result).toBe(true)
  })

  test('evaluate: LITERAL_FALSE policy always returns false', () => {
    const result = native.evaluate({ kind: 'LiteralFalse' }, {})
    expect(result).toBe(false)
  })

  test('evaluate: FIELD equality check passes when value matches', () => {
    const result = native.evaluate(
      { kind: 'Field', path: '/role', value: 'admin' },
      { role: 'admin' }
    )
    expect(result).toBe(true)
  })

  test('evaluate: FIELD equality check fails when value differs', () => {
    const result = native.evaluate(
      { kind: 'Field', path: '/role', value: 'admin' },
      { role: 'user' }
    )
    expect(result).toBe(false)
  })

  test('evaluateWithTrace returns result and trace array', () => {
    const result = native.evaluateWithTrace({ kind: 'LiteralTrue' }, {})
    expect(typeof result.result).toBe('boolean')
    expect(Array.isArray(result.trace)).toBe(true)
    expect(result.result).toBe(true)
  })

  test('Workflow constructor sets id property', () => {
    const wf = new native.Workflow('test-workflow')
    expect(wf.id).toBe('test-workflow')
  })

  test('Workflow.activityCount starts at 0', () => {
    const wf = new native.Workflow('empty-wf')
    expect(wf.activityCount).toBe(0)
  })

  test('Workflow.addActivity increments activityCount', () => {
    const wf = new native.Workflow('count-test')
    wf.addActivity('step1', { policy: { kind: 'LiteralTrue' } })
    expect(wf.activityCount).toBe(1)
    wf.addActivity('step2', { policy: { kind: 'LiteralFalse' } })
    expect(wf.activityCount).toBe(2)
  })

  test('Workflow.dryRun returns true when all policies pass', () => {
    const wf = new native.Workflow('dry-pass')
    wf.addActivity('a', { policy: { kind: 'LiteralTrue' } })
    wf.addActivity('b', { policy: { kind: 'LiteralTrue' } })
    expect(wf.dryRun({})).toBe(true)
  })

  test('Workflow.dryRun returns false when any policy fails', () => {
    const wf = new native.Workflow('dry-fail')
    wf.addActivity('a', { policy: { kind: 'LiteralTrue' } })
    wf.addActivity('b', { policy: { kind: 'LiteralFalse' } })
    expect(wf.dryRun({})).toBe(false)
  })

  test('GP2FServer constructor sets port property', () => {
    const server = new native.GP2FServer({ port: 9999 })
    expect(server.port).toBe(9999)
  })

  test('GP2FServer defaults port to 3000', () => {
    const server = new native.GP2FServer()
    expect(server.port).toBe(3000)
  })

  test('GP2FServer isRunning is false before start()', () => {
    const server = new native.GP2FServer({ port: 9998 })
    expect(server.isRunning).toBe(false)
  })

  test('GP2FServer register() and start()/stop() lifecycle', async () => {
    const server = new native.GP2FServer({ port: 19876 })
    const wf = new native.Workflow('lifecycle-test')
    wf.addActivity('step', { policy: { kind: 'LiteralTrue' } })

    server.register(wf)
    await server.start()
    expect(server.isRunning).toBe(true)

    // Health check via HTTP
    const http = require('http')
    const body = await new Promise((resolve, reject) => {
      http.get('http://127.0.0.1:19876/health', (res) => {
        let data = ''
        res.on('data', (chunk) => { data += chunk })
        res.on('end', () => resolve(data))
      }).on('error', reject)
    })
    expect(body).toBe('ok')

    await server.stop()
    expect(server.isRunning).toBe(false)
  }, 10000)
})
