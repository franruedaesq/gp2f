import React, { useState } from "react";
import type { FieldConflict } from "./wire";

/**
 * Side-by-side modal shown when a REJECT contains non-CRDT field conflicts.
 *
 * For each conflicting field the user can choose:
 * - **Keep mine** – use the local optimistic value
 * - **Use server** – accept the server's authoritative value (from `resolvedValue`)
 */
export interface MergeModalProps {
  conflicts: FieldConflict[];
  /** JSON snapshot of the state *before* the rejected op was applied. */
  baseSnapshot: unknown;
  /** JSON diff representing the local (client) changes. */
  localDiff: unknown;
  /** Called with the user's resolution choices (path → chosen value). */
  onResolve: (resolutions: Record<string, unknown>) => void;
  /** Called when the user cancels without resolving. */
  onCancel: () => void;
}

type Resolution = "mine" | "server";

export function MergeModal({
  conflicts,
  localDiff,
  onResolve,
  onCancel,
}: MergeModalProps): React.ReactElement {
  const [choices, setChoices] = useState<Record<string, Resolution>>(
    () =>
      Object.fromEntries(conflicts.map((c) => [c.path, "server" as Resolution]))
  );

  function handleChoice(path: string, choice: Resolution) {
    setChoices((prev) => ({ ...prev, [path]: choice }));
  }

  function handleResolve() {
    const resolutions: Record<string, unknown> = {};
    for (const conflict of conflicts) {
      if (choices[conflict.path] === "server") {
        resolutions[conflict.path] = conflict.resolvedValue;
      } else {
        // "mine": extract value from localDiff
        resolutions[conflict.path] = getFieldValue(localDiff, conflict.path);
      }
    }
    onResolve(resolutions);
  }

  return (
    <div
      role="dialog"
      aria-modal="true"
      aria-labelledby="merge-modal-title"
      style={{
        position: "fixed",
        inset: 0,
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        backgroundColor: "rgba(0,0,0,0.45)",
        zIndex: 1000,
      }}
    >
      <div
        style={{
          backgroundColor: "#ffffff",
          borderRadius: "0.5rem",
          boxShadow: "0 20px 60px rgba(0,0,0,0.3)",
          width: "min(90vw, 640px)",
          maxHeight: "80vh",
          display: "flex",
          flexDirection: "column",
          overflow: "hidden",
        }}
      >
        {/* Header */}
        <div
          style={{
            padding: "1rem 1.25rem",
            borderBottom: "1px solid #e5e7eb",
            display: "flex",
            alignItems: "center",
            justifyContent: "space-between",
          }}
        >
          <h2
            id="merge-modal-title"
            style={{ margin: 0, fontSize: "1rem", fontWeight: 600 }}
          >
            Resolve Conflicts ({conflicts.length})
          </h2>
          <button
            type="button"
            aria-label="Close"
            onClick={onCancel}
            style={{
              background: "none",
              border: "none",
              cursor: "pointer",
              fontSize: "1.25rem",
            }}
          >
            ✕
          </button>
        </div>

        {/* Conflict list */}
        <div style={{ overflowY: "auto", flex: 1, padding: "0.75rem 1.25rem" }}>
          {conflicts.map((conflict) => (
            <ConflictRow
              key={conflict.path}
              conflict={conflict}
              localValue={getFieldValue(localDiff, conflict.path)}
              choice={choices[conflict.path] ?? "server"}
              onChoose={(c) => handleChoice(conflict.path, c)}
            />
          ))}
        </div>

        {/* Footer */}
        <div
          style={{
            padding: "0.75rem 1.25rem",
            borderTop: "1px solid #e5e7eb",
            display: "flex",
            justifyContent: "flex-end",
            gap: "0.5rem",
          }}
        >
          <button
            type="button"
            onClick={onCancel}
            style={{
              padding: "0.5rem 1rem",
              borderRadius: "0.375rem",
              border: "1px solid #d1d5db",
              background: "#ffffff",
              cursor: "pointer",
            }}
          >
            Cancel
          </button>
          <button
            type="button"
            onClick={handleResolve}
            style={{
              padding: "0.5rem 1rem",
              borderRadius: "0.375rem",
              border: "none",
              backgroundColor: "#2563eb",
              color: "#ffffff",
              cursor: "pointer",
              fontWeight: 500,
            }}
          >
            Apply Resolutions
          </button>
        </div>
      </div>
    </div>
  );
}

// ── internal components ───────────────────────────────────────────────────────

interface ConflictRowProps {
  conflict: FieldConflict;
  localValue: unknown;
  choice: Resolution;
  onChoose: (c: Resolution) => void;
}

function ConflictRow({
  conflict,
  localValue,
  choice,
  onChoose,
}: ConflictRowProps): React.ReactElement {
  return (
    <div
      style={{
        marginBottom: "1rem",
        borderRadius: "0.375rem",
        border: "1px solid #e5e7eb",
        overflow: "hidden",
      }}
    >
      <div
        style={{
          padding: "0.5rem 0.75rem",
          backgroundColor: "#f9fafb",
          borderBottom: "1px solid #e5e7eb",
          display: "flex",
          alignItems: "center",
          gap: "0.5rem",
        }}
      >
        <code style={{ fontSize: "0.8125rem", flex: 1 }}>{conflict.path}</code>
        <span
          style={{
            fontSize: "0.75rem",
            padding: "0.125rem 0.375rem",
            borderRadius: "0.25rem",
            backgroundColor:
              conflict.strategy === "TRANSACTIONAL" ? "#fde8e8" : "#e0f2fe",
            color:
              conflict.strategy === "TRANSACTIONAL" ? "#991b1b" : "#0369a1",
          }}
        >
          {conflict.strategy}
        </span>
      </div>

      <div
        style={{
          display: "grid",
          gridTemplateColumns: "1fr 1fr",
          gap: 0,
        }}
      >
        {/* Mine */}
        <OptionPanel
          label="My change"
          value={localValue}
          selected={choice === "mine"}
          onClick={() => onChoose("mine")}
          accentColor="#2563eb"
        />
        {/* Server */}
        <OptionPanel
          label="Server (authoritative)"
          value={conflict.resolvedValue}
          selected={choice === "server"}
          onClick={() => onChoose("server")}
          accentColor="#16a34a"
          bordered
        />
      </div>
    </div>
  );
}

interface OptionPanelProps {
  label: string;
  value: unknown;
  selected: boolean;
  onClick: () => void;
  accentColor: string;
  bordered?: boolean;
}

function OptionPanel({
  label,
  value,
  selected,
  onClick,
  accentColor,
  bordered,
}: OptionPanelProps): React.ReactElement {
  return (
    <button
      type="button"
      onClick={onClick}
      style={{
        padding: "0.75rem",
        textAlign: "left",
        cursor: "pointer",
        border: "none",
        borderLeft: bordered ? "1px solid #e5e7eb" : undefined,
        backgroundColor: selected ? `${accentColor}10` : "#ffffff",
        outline: selected ? `2px solid ${accentColor}` : "none",
        outlineOffset: "-2px",
        transition: "background-color 100ms",
        width: "100%",
      }}
    >
      <div
        style={{
          fontSize: "0.75rem",
          fontWeight: 600,
          color: selected ? accentColor : "#6b7280",
          marginBottom: "0.25rem",
        }}
      >
        {selected && "✔ "}
        {label}
      </div>
      <pre
        style={{
          margin: 0,
          fontSize: "0.75rem",
          fontFamily: "monospace",
          whiteSpace: "pre-wrap",
          wordBreak: "break-all",
          color: "#1f2937",
        }}
      >
        {JSON.stringify(value, null, 2)}
      </pre>
    </button>
  );
}

// ── helpers ───────────────────────────────────────────────────────────────────

/** Extract the value at a JSON-pointer path (e.g. `/amount`) from an object. */
function getFieldValue(obj: unknown, pointer: string): unknown {
  if (typeof obj !== "object" || obj === null) return undefined;
  // Strip leading `/` and split on `/`
  const segments = pointer.replace(/^\//, "").split("/");
  let current: unknown = obj;
  for (const seg of segments) {
    if (typeof current !== "object" || current === null) return undefined;
    current = (current as Record<string, unknown>)[seg];
  }
  return current;
}
