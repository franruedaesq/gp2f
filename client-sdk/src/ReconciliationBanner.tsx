import React from "react";
import type { RejectResponse } from "./wire";
import { UndoButton } from "./UndoButton";

/**
 * Displayed as a dismissible banner at the top of the form whenever the server
 * REJECTs an operation.  Shows the rejection reason and provides a "Undo" and
 * "Resolve conflicts" action.
 */
export interface ReconciliationBannerProps {
  /** The full server REJECT response. */
  rejection: RejectResponse;
  /** Called when the user clicks "Undo". */
  onUndo: () => void;
  /** Called when the user clicks "Resolve conflicts". */
  onResolve: () => void;
  /** Called when the user dismisses the banner. */
  onDismiss: () => void;
}

export function ReconciliationBanner({
  rejection,
  onUndo,
  onResolve,
  onDismiss,
}: ReconciliationBannerProps): React.ReactElement {
  const hasConflicts = rejection.patch.conflicts.length > 0;

  return (
    <div
      role="alert"
      aria-live="assertive"
      style={{
        display: "flex",
        alignItems: "center",
        gap: "0.75rem",
        padding: "0.75rem 1rem",
        borderRadius: "0.375rem",
        backgroundColor: "#fef3c7",
        borderLeft: "4px solid #f59e0b",
        color: "#92400e",
        fontSize: "0.875rem",
      }}
    >
      <span style={{ flex: 1 }}>
        <strong>Sync conflict:</strong> {rejection.reason}
      </span>

      <UndoButton onUndo={onUndo} />

      {hasConflicts && (
        <button
          type="button"
          onClick={onResolve}
          style={{
            padding: "0.25rem 0.75rem",
            borderRadius: "0.25rem",
            border: "1px solid #b45309",
            backgroundColor: "transparent",
            color: "#92400e",
            cursor: "pointer",
            fontSize: "0.875rem",
          }}
        >
          Resolve conflicts ({rejection.patch.conflicts.length})
        </button>
      )}

      <button
        type="button"
        aria-label="Dismiss"
        onClick={onDismiss}
        style={{
          background: "none",
          border: "none",
          cursor: "pointer",
          color: "#92400e",
          fontSize: "1rem",
          lineHeight: 1,
          padding: "0.125rem",
        }}
      >
        ✕
      </button>
    </div>
  );
}
