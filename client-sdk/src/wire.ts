/**
 * Wire-protocol types shared between the GP2F server and this SDK.
 * These mirror the Rust structs in `server/src/wire.rs`.
 */

export interface ClientMessage {
  opId: string;
  astVersion: string;
  action: string;
  payload: unknown;
  clientSnapshotHash: string;
  tenantId?: string;
  workflowId?: string;
  instanceId?: string;
  /** base64url HMAC-SHA256 over canonical op fields */
  clientSignature?: string;
}

export interface AcceptResponse {
  opId: string;
  serverSnapshotHash: string;
}

export interface ThreeWayPatch {
  baseSnapshot: unknown;
  localDiff: unknown;
  serverDiff: unknown;
  conflicts: FieldConflict[];
}

export interface FieldConflict {
  path: string;
  strategy: "CRDT" | "LWW" | "TRANSACTIONAL";
  resolvedValue: unknown;
}

export interface RejectResponse {
  opId: string;
  reason: string;
  patch: ThreeWayPatch;
  /**
   * Suggested back-off interval in milliseconds (Retry-After semantics).
   * Present when the rejection is caused by server-side backpressure; the
   * client SHOULD pause sending new ops for at least this duration.
   */
  retryAfterMs?: number;
}

/**
 * Sent by the server once per connection, immediately after the WebSocket
 * handshake, for time-synchronisation purposes.
 */
export interface HelloMessage {
  /** Server wall-clock time in milliseconds since the Unix epoch. */
  serverTimeMs: number;
  /** Server HLC timestamp at the moment of the hello. */
  serverHlcTs: number;
}

/**
 * Sent by the server when the client's AST schema version is incompatible.
 * The client MUST fetch a fresh policy bundle before reconnecting.
 */
export interface ReloadRequiredMessage {
  /** The minimum AST version the server accepts (semver). */
  minRequiredVersion: string;
  /** Human-readable explanation. */
  reason: string;
}

export type ServerMessage =
  | { type: "ACCEPT" } & AcceptResponse
  | { type: "REJECT" } & RejectResponse
  | { type: "HELLO" } & HelloMessage
  | { type: "RELOAD_REQUIRED" } & ReloadRequiredMessage;
