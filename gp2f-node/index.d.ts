/* auto-generated TypeScript declarations for @gp2f/server (gp2f-node) */

// ── Policy AST ─────────────────────────────────────────────────────────────

/**
 * Every node kind supported by the GP2F AST policy evaluator.
 */
export type NodeKind =
  | 'LiteralTrue'
  | 'LiteralFalse'
  | 'And'
  | 'Or'
  | 'Not'
  | 'Eq'
  | 'Neq'
  | 'Gt'
  | 'Gte'
  | 'Lt'
  | 'Lte'
  | 'In'
  | 'Contains'
  | 'Exists'
  | 'Field'
  | 'Call'
  | 'VibeCheck'

/**
 * A node in the GP2F policy AST.
 *
 * Build a tree of these to express the policy that governs whether a given
 * activity is permitted.
 *
 * @example
 * ```typescript
 * const policy: AstNode = {
 *   kind: 'And',
 *   children: [
 *     { kind: 'Field', path: '/user/role', value: 'admin' },
 *     { kind: 'Exists', path: '/session/token' },
 *   ],
 * };
 * ```
 */
export interface AstNode {
  /** The operation this node performs (required). */
  kind: NodeKind
  /** Child nodes for composite operators (And, Or, Not, comparison, …). */
  children?: AstNode[]
  /** JSON-pointer path used by `Field` and `Exists` nodes. */
  path?: string
  /** String-encoded scalar value for leaf nodes, e.g. `"admin"` or `"42"`. */
  value?: string
  /** Name of the external function – used only by `Call` nodes. */
  callName?: string
}

/**
 * Result of a policy evaluation including the decision trace.
 */
export interface EvalResult {
  /** `true` when the policy permits the operation. */
  result: boolean
  /** Human-readable trace of each evaluation step (useful for debugging). */
  trace: string[]
}

/**
 * Evaluate a policy AST against a JSON state object.
 *
 * @param policy - Root node of the policy AST.
 * @param state  - Arbitrary JSON object representing the current state.
 * @returns `true` when the policy permits the operation, `false` otherwise.
 *
 * @example
 * ```typescript
 * import { evaluate } from '@gp2f/server';
 *
 * const allowed = evaluate(
 *   { kind: 'Field', path: '/role', value: 'admin' },
 *   { role: 'admin' }
 * );
 * // => true
 * ```
 */
export function evaluate(policy: AstNode, state: unknown): boolean

/**
 * Evaluate a policy AST and return the full evaluation trace.
 *
 * @param policy - Root node of the policy AST.
 * @param state  - Arbitrary JSON object representing the current state.
 * @returns An {@link EvalResult} with the boolean decision and a trace array.
 */
export function evaluateWithTrace(policy: AstNode, state: unknown): EvalResult

// ── Workflow ────────────────────────────────────────────────────────────────

/**
 * Context object passed to every `onExecute` callback.
 */
export interface ExecutionContext {
  /** Unique workflow execution identifier. */
  instanceId: string
  /** Tenant/organisation this execution belongs to. */
  tenantId: string
  /** Name of the activity currently executing. */
  activityName: string
  /**
   * The JSON-encoded state document evaluated by the policy engine.
   *
   * Parse with `JSON.parse(ctx.stateJson)` to get the object.
   */
  stateJson: string
}

/**
 * Configuration object for a single workflow activity.
 */
export interface ActivityConfig {
  /** Policy AST that governs whether this activity is permitted. */
  policy: AstNode
  /**
   * Optional name of a registered compensation handler to undo this activity
   * if a later step fails (Saga pattern).
   */
  compensationRef?: string
  /**
   * When `true`, this activity runs as a Local Activity (no Temporal
   * persistence round-trip).  Use for short, idempotent operations.
   */
  isLocal?: boolean
}

/**
 * A GP2F workflow definition.
 *
 * Construct a workflow, register activities, then pass it to
 * {@link GP2FServer.register}.
 *
 * @example
 * ```typescript
 * import { Workflow } from '@gp2f/server';
 *
 * const wf = new Workflow('document-approval');
 * wf.addActivity(
 *   'review',
 *   { policy: { kind: 'LiteralTrue' } },
 *   async (ctx) => { console.log('executing', ctx.activityName); }
 * );
 * ```
 */
export class Workflow {
  /**
   * Create a new workflow with the given identifier.
   * @param workflowId - Stable identifier for this workflow.
   */
  constructor(workflowId: string)

  /** The workflow identifier. */
  readonly id: string

  /** The number of registered activities. */
  readonly activityCount: number

  /**
   * Add an activity to this workflow.
   *
   * Activities are executed in the order they are added.  Each activity has a
   * policy AST that determines whether the operation is permitted.
   *
   * The optional `onExecute` callback is invoked when the activity is
   * accepted.  It receives an {@link ExecutionContext} and may be async; the
   * Rust runtime invokes it on the Node.js event loop via a threadsafe
   * function handle.
   *
   * @param name      - Unique name for this activity within the workflow.
   * @param config    - Activity configuration including the policy AST.
   * @param onExecute - Optional async callback invoked when the activity runs.
   * @returns The workflow identifier (for informational purposes).
   */
  addActivity(
    name: string,
    config: ActivityConfig,
    onExecute?: (ctx: ExecutionContext) => void | Promise<unknown>
  ): string

  /**
   * Evaluate the workflow against a state document without side-effects.
   *
   * @param state - JSON state document to evaluate against.
   * @returns `true` when *every* activity policy is satisfied.
   */
  dryRun(state: unknown): boolean
}

// ── Server ──────────────────────────────────────────────────────────────────

/**
 * Configuration object for {@link GP2FServer}.
 */
export interface ServerConfig {
  /** TCP port the server should listen on.  Defaults to `3000`. */
  port?: number
  /** Hostname / bind address.  Defaults to `"127.0.0.1"`. */
  host?: string
}

/**
 * The GP2F server.
 *
 * Hosts an Axum-backed HTTP server that makes the GP2F workflow engine
 * accessible from Node.js and other HTTP clients.
 *
 * ## HTTP API
 *
 * | Method | Path                  | Description                              |
 * |--------|-----------------------|------------------------------------------|
 * | GET    | `/health`             | Health-check, returns `"ok"`.            |
 * | POST   | `/workflow/run`       | Execute the next activity of a workflow. |
 * | POST   | `/workflow/dry-run`   | Evaluate policies without side-effects.  |
 *
 * @example
 * ```typescript
 * import { GP2FServer, Workflow } from '@gp2f/server';
 *
 * const server = new GP2FServer({ port: 3000 });
 *
 * const wf = new Workflow('my-workflow');
 * wf.addActivity('step1', { policy: { kind: 'LiteralTrue' } });
 *
 * server.register(wf);
 * await server.start();
 * // ... later:
 * await server.stop();
 * ```
 */
export class GP2FServer {
  /**
   * Create a new server instance.
   *
   * The server is not started until {@link GP2FServer.start} is called.
   *
   * @param config - Optional server configuration.
   */
  constructor(config?: ServerConfig)

  /** The port the server is (or will be) listening on. */
  readonly port: number

  /** `true` while the server is running. */
  readonly isRunning: boolean

  /**
   * Register a workflow definition with the server.
   *
   * Workflows can be registered at any time, even while the server is running.
   *
   * @param workflow - The workflow to register.
   */
  register(workflow: Workflow): void

  /**
   * Start the HTTP server.
   *
   * Resolves once the TCP listener is bound and the server is ready to accept
   * connections.
   */
  start(): Promise<void>

  /**
   * Stop the server gracefully.
   *
   * Sends a shutdown signal and waits for in-flight requests to drain.
   */
  stop(): Promise<void>
}
