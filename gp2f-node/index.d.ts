/* eslint-disable */
/* tslint:disable */
/* auto-generated – mirrors gp2f-node/src/*.rs napi exports */

// ── Node kinds ────────────────────────────────────────────────────────────────

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

// ── Core AST ──────────────────────────────────────────────────────────────────

/** A node in the GP2F policy AST. */
export interface AstNode {
    /** The operation this node performs (required). */
    kind: string
    /** Child nodes for composite operators (AND, OR, NOT, comparison, …). */
    children?: AstNode[]
    /** JSON-pointer path used by `FIELD` and `EXISTS` nodes (e.g. `/user/role`). */
    path?: string
    /** String-encoded scalar value for leaf nodes (e.g. `"admin"`, `"42"`). */
    value?: string
    /** Name of the external function – used only by `CALL` nodes. */
    callName?: string
}

// ── Activity & server configuration ──────────────────────────────────────────

/** Configuration object for a single workflow activity. */
export interface ActivityConfig {
    /** Policy AST that governs whether this activity is permitted. */
    policy: AstNode | PolicyInput
    /** Optional name of a registered compensation handler. */
    compensationRef?: string
    /** When `true`, this activity runs as a Local Activity. */
    isLocal?: boolean
}

/** Server startup configuration. */
export interface ServerConfig {
    /** TCP port to listen on. Defaults to 3000. */
    port?: number
    /** Bind address. Defaults to `"0.0.0.0"`. */
    host?: string
}

/** Context passed to every `onExecute` callback. */
export interface ExecutionContext {
    /** Unique workflow execution identifier. */
    instanceId: string
    /** Tenant/organisation this execution belongs to. */
    tenantId: string
    /** Name of the activity currently executing. */
    activityName: string
    /** The JSON-encoded state document. Use `JSON.parse(ctx.stateJson)`. */
    stateJson: string
}

/** Result of a policy evaluation including the decision trace. */
export interface EvalResult {
    /** `true` when the policy permits the operation. */
    result: boolean
    /** Human-readable trace of each evaluation step (for debugging). */
    trace: string[]
}

// ── Native functions ──────────────────────────────────────────────────────────

/**
 * Evaluate a policy AST against a JSON state object.
 *
 * Returns `true` when the policy permits the operation.
 */
export function evaluate(policy: AstNode, state: object): boolean

/**
 * Evaluate a policy AST and return the full evaluation trace.
 *
 * Useful for debugging policies.
 */
export function evaluateWithTrace(policy: AstNode, state: object): EvalResult

// ── Workflow class ────────────────────────────────────────────────────────────

export class Workflow {
    constructor(workflowId: string)

    /** Add an activity to this workflow. */
    addActivity(
        name: string,
        config: ActivityConfig,
        onExecute?: (ctx: ExecutionContext) => Promise<void> | void,
    ): string

    /** The workflow identifier. */
    readonly id: string

    /** Number of registered activities. */
    readonly activityCount: number

    /** Evaluate all activity policies against `state` (no side-effects). */
    dryRun(state: object): boolean
}

// ── GP2FServer class ──────────────────────────────────────────────────────────

export class GP2FServer {
    constructor(config?: ServerConfig)

    /** Register a workflow with this server. */
    register(workflow: Workflow): void

    /** Start the HTTP server. */
    start(): Promise<void>

    /** Stop the HTTP server. */
    stop(): Promise<void>

    /** The configured TCP port. */
    readonly port: number

    /** `true` when the server is currently accepting connections. */
    readonly isRunning: boolean
}

// ── Fluent Policy Builder ─────────────────────────────────────────────────────

/** Shared interface for all builder objects that can produce an AstNode. */
export interface Builder {
    build(): AstNode
    toJSON(): AstNode
}

/** A policy value that is either a raw AstNode or a Builder. */
export type PolicyInput = AstNode | Builder

/** Builder for field-specific policy assertions. */
export declare class FieldBuilder implements Builder {
    constructor(path: string)

    equal(value: string | number): AstNode
    eq(value: string | number): AstNode
    notEqual(value: string | number): AstNode
    neq(value: string | number): AstNode

    greaterThan(value: string | number): AstNode
    gt(value: string | number): AstNode
    greaterThanOrEqual(value: string | number): AstNode
    gte(value: string | number): AstNode
    lessThan(value: string | number): AstNode
    lt(value: string | number): AstNode
    lessThanOrEqual(value: string | number): AstNode
    lte(value: string | number): AstNode

    in(values: string[]): AstNode
    contains(value: string): AstNode

    build(): AstNode
    toJSON(): AstNode
}

/** Builder for Semantic Vibe Engine checks. */
export declare class VibeBuilder implements Builder {
    constructor(intent: string)

    withConfidence(threshold: number): this

    build(): AstNode
    toJSON(): AstNode
}

/** Entry point for the fluent policy builder API. */
export declare class PolicyBuilder {
    static field(path: string): FieldBuilder
    static and(...nodes: PolicyInput[]): AstNode
    static or(...nodes: PolicyInput[]): AstNode
    static not(node: PolicyInput): AstNode
    static exists(path: string): AstNode
    static literalTrue(): AstNode
    static literalFalse(): AstNode
    static vibe(intent: string): VibeBuilder
}

/** Shorthand alias for {@link PolicyBuilder}. */
export declare const p: typeof PolicyBuilder
