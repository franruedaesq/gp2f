import React from "react";

/**
 * A small undo button shown inline (e.g. in the {@link ReconciliationBanner}).
 * Reverts the optimistic local change that was rejected by the server.
 */
export interface UndoButtonProps {
  /** Callback invoked when the user clicks the undo button. */
  onUndo: () => void;
  /** Optional label (defaults to "Undo"). */
  label?: string;
  /** Whether the undo action is currently available. */
  disabled?: boolean;
}

export function UndoButton({
  onUndo,
  label = "Undo",
  disabled = false,
}: UndoButtonProps): React.ReactElement {
  return (
    <button
      type="button"
      onClick={onUndo}
      disabled={disabled}
      aria-label="Undo last change"
      style={{
        display: "inline-flex",
        alignItems: "center",
        gap: "0.25rem",
        padding: "0.25rem 0.75rem",
        borderRadius: "0.25rem",
        border: "1px solid #b45309",
        backgroundColor: "#ffffff",
        color: "#92400e",
        cursor: disabled ? "not-allowed" : "pointer",
        fontSize: "0.875rem",
        opacity: disabled ? 0.5 : 1,
        transition: "background-color 150ms",
      }}
    >
      ↩ {label}
    </button>
  );
}
