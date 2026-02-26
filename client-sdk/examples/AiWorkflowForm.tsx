/**
 * GP2F React Example App – AI-Assisted Workflow Form
 *
 * Phase 10 requirement 6: React example app demonstrating @gp2f/client-sdk@1.0.0.
 *
 * Shows:
 * - Connecting to the GP2F server via WebSocket
 * - Submitting optimistic ops and handling ACCEPT / REJECT responses
 * - Triggering AI assistance via the /agent/propose endpoint
 * - Displaying the ReconciliationBanner and UndoButton components
 */

import React, { useCallback, useEffect, useRef, useState } from "react";
import {
  Gp2fClient,
  ReconciliationBanner,
  UndoButton,
} from "@gp2f/client-sdk";
import type {
  ServerMessage,
  AcceptResponse,
  RejectResponse,
} from "@gp2f/client-sdk";

// ── types ─────────────────────────────────────────────────────────────────────

interface WorkflowFormState {
  symptoms: string;
  severity: string;
  notes: string;
}

interface AiSuggestion {
  toolId: string;
  arguments: Record<string, unknown>;
}

// ── component ─────────────────────────────────────────────────────────────────

/**
 * AiWorkflowForm – demonstrates full GP2F integration.
 *
 * 1. Connects to the GP2F server via WebSocket on mount.
 * 2. Submits form changes as optimistic ops.
 * 3. Shows ReconciliationBanner on REJECT with a merge patch.
 * 4. Lets the user request AI assistance (POST /agent/propose).
 */
export function AiWorkflowForm(): React.JSX.Element {
  const SERVER_URL =
    (typeof process !== "undefined" && process.env?.REACT_APP_GP2F_URL) ||
    "http://localhost:3000";
  const WS_URL = SERVER_URL.replace(/^http/, "ws") + "/ws";

  const [form, setForm] = useState<WorkflowFormState>({
    symptoms: "",
    severity: "mild",
    notes: "",
  });
  const [lastResponse, setLastResponse] = useState<ServerMessage | null>(null);
  const [aiSuggestion, setAiSuggestion] = useState<AiSuggestion | null>(null);
  const [aiLoading, setAiLoading] = useState(false);
  const [aiError, setAiError] = useState<string | null>(null);
  const [connected, setConnected] = useState(false);

  const clientRef = useRef<Gp2fClient | null>(null);
  const snapshotHashRef = useRef<string>(
    "0000000000000000000000000000000000000000000000000000000000000000"
  );

  // ── connect on mount ───────────────────────────────────────────────────────

  useEffect(() => {
    const client = new Gp2fClient({
      url: WS_URL,
      onMessage: (msg: ServerMessage) => {
        setLastResponse(msg);
        if (msg.type === "ACCEPT") {
          snapshotHashRef.current = (msg as { type: "ACCEPT" } & AcceptResponse)
            .serverSnapshotHash;
        }
      },
      onError: (err: Error) => {
        console.error("GP2F WebSocket error:", err);
      },
    });

    clientRef.current = client;

    const socket = new WebSocket(WS_URL);
    socket.onopen = () => setConnected(true);
    socket.onclose = () => setConnected(false);

    return () => {
      socket.close();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // ── submit op ─────────────────────────────────────────────────────────────

  const submitOp = useCallback(
    (payload: Record<string, unknown>) => {
      const opId = `op-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
      const message = {
        opId,
        astVersion: "1.0.0",
        action: "update",
        payload,
        clientSnapshotHash: snapshotHashRef.current,
        tenantId: "demo-tenant",
        workflowId: "intake-form",
        instanceId: "session-001",
        role: "default",
      };

      fetch(`${SERVER_URL}/op`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(message),
      })
        .then((r) => r.json())
        .then((resp: ServerMessage) => {
          setLastResponse(resp);
          if (resp.type === "ACCEPT") {
            snapshotHashRef.current = (
              resp as { type: "ACCEPT" } & AcceptResponse
            ).serverSnapshotHash;
          }
        })
        .catch(console.error);
    },
    [SERVER_URL]
  );

  const handleFieldChange = useCallback(
    (field: keyof WorkflowFormState) =>
      (e: React.ChangeEvent<HTMLInputElement | HTMLSelectElement | HTMLTextAreaElement>) => {
        const value = e.target.value;
        setForm((prev) => ({ ...prev, [field]: value }));
        submitOp({ [field]: value });
      },
    [submitOp]
  );

  // ── AI assistance ─────────────────────────────────────────────────────────

  const requestAiAssist = useCallback(async () => {
    setAiLoading(true);
    setAiError(null);
    setAiSuggestion(null);

    try {
      const resp = await fetch(`${SERVER_URL}/agent/propose`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          tenantId: "demo-tenant",
          workflowId: "intake-form",
          instanceId: "session-001",
          astVersion: "1.0.0",
          prompt: `The user has entered symptoms: "${form.symptoms}". What is the best next action?`,
          vibe: {
            intent: "confused",
            confidence: 0.7,
            bottleneck: "symptom_entry",
          },
        }),
      });

      const data = await resp.json();
      if (data.type === "ACCEPT") {
        setAiSuggestion({
          toolId: data.opId ?? "unknown",
          arguments: data.payload ?? {},
        });
      } else {
        setAiError(data.reason ?? data.error ?? "AI proposal not accepted");
      }
    } catch (err) {
      setAiError(err instanceof Error ? err.message : "Request failed");
    } finally {
      setAiLoading(false);
    }
  }, [SERVER_URL, form.symptoms]);

  // ── undo ──────────────────────────────────────────────────────────────────

  const handleUndo = useCallback(() => {
    // In a full implementation this would submit a reverse-op or
    // replay the event log up to the previous snapshot.
    setForm({ symptoms: "", severity: "mild", notes: "" });
    setLastResponse(null);
    setAiSuggestion(null);
  }, []);

  // ── render ────────────────────────────────────────────────────────────────

  const isRejected = lastResponse?.type === "REJECT";
  const rejectedResponse = isRejected
    ? (lastResponse as { type: "REJECT" } & RejectResponse)
    : null;

  return (
    <div style={{ maxWidth: 640, margin: "2rem auto", fontFamily: "sans-serif" }}>
      <h1>GP2F AI-Assisted Intake Form</h1>

      <p style={{ color: connected ? "green" : "red" }}>
        {connected ? "● Connected to GP2F server" : "○ Disconnected"}
      </p>

      {isRejected && rejectedResponse && (
        <ReconciliationBanner
          reason={rejectedResponse.reason}
          patch={rejectedResponse.patch}
          onAcceptServerVersion={() => {
            // Apply the server's base snapshot as the new local state.
            const base = rejectedResponse.patch.baseSnapshot as WorkflowFormState;
            if (base && typeof base === "object") {
              setForm((prev) => ({ ...prev, ...base }));
            }
            setLastResponse(null);
          }}
          onKeepLocalVersion={() => setLastResponse(null)}
        />
      )}

      <form onSubmit={(e) => e.preventDefault()}>
        <div style={{ marginBottom: "1rem" }}>
          <label htmlFor="symptoms">
            <strong>Symptoms</strong>
          </label>
          <br />
          <textarea
            id="symptoms"
            value={form.symptoms}
            onChange={handleFieldChange("symptoms")}
            rows={3}
            style={{ width: "100%", marginTop: 4 }}
            placeholder="Describe the patient's symptoms…"
          />
        </div>

        <div style={{ marginBottom: "1rem" }}>
          <label htmlFor="severity">
            <strong>Severity</strong>
          </label>
          <br />
          <select
            id="severity"
            value={form.severity}
            onChange={handleFieldChange("severity")}
            style={{ marginTop: 4 }}
          >
            <option value="mild">Mild</option>
            <option value="moderate">Moderate</option>
            <option value="severe">Severe</option>
            <option value="critical">Critical</option>
          </select>
        </div>

        <div style={{ marginBottom: "1rem" }}>
          <label htmlFor="notes">
            <strong>Clinical Notes</strong>
          </label>
          <br />
          <textarea
            id="notes"
            value={form.notes}
            onChange={handleFieldChange("notes")}
            rows={4}
            style={{ width: "100%", marginTop: 4 }}
            placeholder="Additional clinical observations…"
          />
        </div>

        <div style={{ display: "flex", gap: "1rem", alignItems: "center" }}>
          <button
            type="button"
            onClick={requestAiAssist}
            disabled={aiLoading}
            style={{
              padding: "8px 16px",
              background: "#4f46e5",
              color: "white",
              border: "none",
              borderRadius: 4,
              cursor: aiLoading ? "wait" : "pointer",
            }}
          >
            {aiLoading ? "Asking AI…" : "✨ Get AI Suggestion"}
          </button>

          <UndoButton onUndo={handleUndo} label="Reset Form" />
        </div>
      </form>

      {aiError && (
        <div
          style={{
            marginTop: "1rem",
            padding: "8px 12px",
            background: "#fee2e2",
            border: "1px solid #fca5a5",
            borderRadius: 4,
          }}
        >
          <strong>AI Error:</strong> {aiError}
        </div>
      )}

      {aiSuggestion && (
        <div
          style={{
            marginTop: "1rem",
            padding: "12px",
            background: "#f0fdf4",
            border: "1px solid #86efac",
            borderRadius: 4,
          }}
        >
          <strong>AI Suggestion</strong>
          <pre style={{ marginTop: 8, fontSize: 13 }}>
            {JSON.stringify(aiSuggestion, null, 2)}
          </pre>
        </div>
      )}
    </div>
  );
}

export default AiWorkflowForm;
