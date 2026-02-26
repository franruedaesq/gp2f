// Wire types
export type {
  ClientMessage,
  ServerMessage,
  AcceptResponse,
  RejectResponse,
  ThreeWayPatch,
  FieldConflict,
  HelloMessage,
} from "./wire";

// WebSocket client
export { Gp2fClient, applyOptimisticUpdate } from "./client";
export type { Gp2fClientOptions, MessageHandler, ErrorHandler, TokenHandler, OptimisticUpdateOptions } from "./client";

// Reconciliation UX components
export { ReconciliationBanner } from "./ReconciliationBanner";
export type { ReconciliationBannerProps } from "./ReconciliationBanner";

export { UndoButton } from "./UndoButton";
export type { UndoButtonProps } from "./UndoButton";

export { MergeModal } from "./MergeModal";
export type { MergeModalProps } from "./MergeModal";
