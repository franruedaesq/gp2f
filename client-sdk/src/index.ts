// Wire types
export type {
  ClientMessage,
  ServerMessage,
  AcceptResponse,
  RejectResponse,
  ThreeWayPatch,
  FieldConflict,
  HelloMessage,
  ReloadRequiredMessage,
} from "./wire";

// WebSocket client
export { Gp2fClient, applyOptimisticUpdate } from "./client";
export type { Gp2fClientOptions, MessageHandler, ErrorHandler, TokenHandler, OptimisticUpdateOptions, ReloadRequiredHandler } from "./client";

// Reconciliation UX components
export { ReconciliationBanner } from "./ReconciliationBanner";
export type { ReconciliationBannerProps } from "./ReconciliationBanner";

export { UndoButton } from "./UndoButton";
export type { UndoButtonProps } from "./UndoButton";

export { MergeModal } from "./MergeModal";
export type { MergeModalProps } from "./MergeModal";

// ── Lazy Policy Engine ────────────────────────────────────────────────────────

/**
 * The shape of the lazily-loaded policy engine module.
 *
 * When the WASM build of `policy-core` is published as an npm package
 * (e.g. `@gp2f/policy-core-wasm`), this interface describes its public API.
 * The lazy loader below imports it on-demand so that the WASM binary is NOT
 * included in the initial JS bundle, reducing Time-To-Interactive.
 */
export interface PolicyEngineModule {
  /** Evaluate a policy AST against a JSON state document. */
  evaluate(stateJson: string, astJson: string): { result: boolean; trace: string[] };
}

/**
 * Lazily load the GP2F WASM policy engine.
 *
 * The module is fetched and instantiated on the **first call** only; subsequent
 * calls return the cached instance with no additional network cost.
 *
 * This pattern ("lazy loading") keeps the initial JS bundle small and defers
 * the WASM download until the moment the policy engine is actually needed.
 *
 * @example
 * ```ts
 * const engine = await loadPolicyEngine();
 * const { result } = engine.evaluate(JSON.stringify(state), JSON.stringify(ast));
 * ```
 */
export async function loadPolicyEngine(): Promise<PolicyEngineModule> {
  return _policyEngineCache ?? (_policyEngineCache = await _importPolicyEngine());
}

/** Cached module instance (populated after the first successful load). */
let _policyEngineCache: PolicyEngineModule | null = null;

/**
 * Perform the actual dynamic import.
 *
 * Replace the module path with the real WASM package once it is published.
 * The `/* webpackChunkName  magic comment tells bundlers (webpack / Vite)
 * to emit this as a separate chunk so it is only downloaded on demand.
 */
async function _importPolicyEngine(): Promise<PolicyEngineModule> {
  // Dynamic import – bundlers will split this into a separate chunk.
  // We use a try/catch so that the SDK remains usable even when the optional
  // @gp2f/policy-core-wasm peer package is not installed.
  try {
    // @ts-expect-error: @gp2f/policy-core-wasm is an optional peer package
    // that may not be installed.  The try/catch below handles the absence case.
    const mod = await import(/* webpackChunkName: "policy-engine" */ "@gp2f/policy-core-wasm");
    return mod as PolicyEngineModule;
  } catch {
    // WASM package not installed – return a stub that always delegates to the
    // server-side evaluator.  Log a warning so developers know the lazy engine
    // is inactive.
    if (typeof console !== "undefined") {
      console.warn(
        "[gp2f] WASM policy engine not found (@gp2f/policy-core-wasm). " +
        "All policy evaluation will be performed server-side.",
      );
    }
    return {
      evaluate(_stateJson: string, _astJson: string) {
        throw new Error(
          "WASM policy engine is not available. Install @gp2f/policy-core-wasm to enable client-side evaluation."
        );
      },
    };
  }
}