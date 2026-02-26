/**
 * GP2F React Example App – Agent Proposal Dashboard
 *
 * Phase 10 requirement 6: React example demonstrating the AI agent
 * proposal pipeline (POST /agent/propose) with canary rollout display.
 */

import React, { useCallback, useEffect, useState } from "react";

// ── types ─────────────────────────────────────────────────────────────────────

interface ProposalResult {
  id: string;
  tenantId: string;
  workflowId: string;
  status: "accepted" | "rejected" | "no_tool" | "rate_limited" | "error";
  toolId?: string;
  reason?: string;
  latencyMs: number;
  timestamp: Date;
}

interface AiFeatureStatus {
  tenantId: string;
  workflowId: string;
  enabled: boolean;
  failureRatePercent: number;
}

// ── component ─────────────────────────────────────────────────────────────────

/**
 * AgentDashboard – shows live AI proposal results and canary status.
 */
export function AgentDashboard(): React.JSX.Element {
  const SERVER_URL =
    (typeof process !== "undefined" && process.env?.REACT_APP_GP2F_URL) ||
    "http://localhost:3000";

  const [proposals, setProposals] = useState<ProposalResult[]>([]);
  const [loading, setLoading] = useState(false);
  const [tenantId, setTenantId] = useState("demo-tenant");
  const [workflowId, setWorkflowId] = useState("intake-form");
  const [prompt, setPrompt] = useState("What is the best next action?");
  const [featureStatus, setFeatureStatus] = useState<AiFeatureStatus | null>(
    null
  );

  // Simulate canary status refresh every 5 seconds.
  useEffect(() => {
    const interval = setInterval(() => {
      setFeatureStatus({
        tenantId,
        workflowId,
        enabled: true,
        failureRatePercent:
          proposals.length === 0
            ? 0
            : (proposals.filter((p) => p.status === "rejected").length /
                proposals.length) *
              100,
      });
    }, 5000);
    return () => clearInterval(interval);
  }, [tenantId, workflowId, proposals]);

  const sendProposal = useCallback(async () => {
    setLoading(true);
    const start = Date.now();
    const id = `proposal-${Date.now()}-${Math.random().toString(36).slice(2, 6)}`;

    try {
      const resp = await fetch(`${SERVER_URL}/agent/propose`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          tenantId,
          workflowId,
          instanceId: `inst-${Math.floor(Math.random() * 100)}`,
          astVersion: "1.0.0",
          prompt,
          vibe: {
            intent: "focused",
            confidence: 0.85,
            bottleneck: "current_step",
          },
        }),
      });

      const latencyMs = Date.now() - start;
      const data = await resp.json();

      let status: ProposalResult["status"] = "error";
      let toolId: string | undefined;
      let reason: string | undefined;

      if (resp.status === 429) {
        status = "rate_limited";
        reason = "Rate limit exceeded";
      } else if (data.type === "ACCEPT") {
        status = "accepted";
        toolId = data.opId;
      } else if (data.status === "proposal_rejected") {
        status = "rejected";
        reason = data.reason;
      } else if (data.status === "no_tool_chosen") {
        status = "no_tool";
      }

      setProposals((prev) => [
        {
          id,
          tenantId,
          workflowId,
          status,
          toolId,
          reason,
          latencyMs,
          timestamp: new Date(),
        },
        ...prev.slice(0, 49), // keep last 50
      ]);
    } catch (err) {
      const latencyMs = Date.now() - start;
      setProposals((prev) => [
        {
          id,
          tenantId,
          workflowId,
          status: "error",
          reason: err instanceof Error ? err.message : "Unknown error",
          latencyMs,
          timestamp: new Date(),
        },
        ...prev.slice(0, 49),
      ]);
    } finally {
      setLoading(false);
    }
  }, [SERVER_URL, tenantId, workflowId, prompt]);

  const statusColor: Record<ProposalResult["status"], string> = {
    accepted: "#16a34a",
    rejected: "#dc2626",
    no_tool: "#ca8a04",
    rate_limited: "#7c3aed",
    error: "#9ca3af",
  };

  const p95 =
    proposals.length === 0
      ? 0
      : (() => {
          const sorted = [...proposals]
            .map((p) => p.latencyMs)
            .sort((a, b) => a - b);
          const idx = Math.floor(sorted.length * 0.95);
          return sorted[idx] ?? sorted[sorted.length - 1];
        })();

  return (
    <div
      style={{ maxWidth: 800, margin: "2rem auto", fontFamily: "sans-serif" }}
    >
      <h1>GP2F Agent Proposal Dashboard</h1>

      {/* Canary status */}
      {featureStatus && (
        <div
          style={{
            marginBottom: "1rem",
            padding: "10px 14px",
            background: featureStatus.enabled ? "#f0fdf4" : "#fef2f2",
            border: `1px solid ${featureStatus.enabled ? "#86efac" : "#fca5a5"}`,
            borderRadius: 6,
          }}
        >
          <strong>Canary Status</strong>{" "}
          <span
            style={{
              color: featureStatus.enabled ? "#16a34a" : "#dc2626",
              fontWeight: 600,
            }}
          >
            {featureStatus.enabled ? "ENABLED" : "ROLLED BACK"}
          </span>
          {" · "}
          Failure rate:{" "}
          <strong>{featureStatus.failureRatePercent.toFixed(2)}%</strong>
          {featureStatus.failureRatePercent > 0.1 && (
            <span style={{ color: "#dc2626", marginLeft: 8 }}>
              ⚠ Approaching rollback threshold (0.1%)
            </span>
          )}
        </div>
      )}

      {/* Configuration */}
      <div
        style={{
          display: "grid",
          gridTemplateColumns: "1fr 1fr",
          gap: "0.75rem",
          marginBottom: "1rem",
        }}
      >
        <label>
          Tenant ID
          <br />
          <input
            value={tenantId}
            onChange={(e) => setTenantId(e.target.value)}
            style={{ width: "100%", padding: "4px 8px", marginTop: 4 }}
          />
        </label>
        <label>
          Workflow ID
          <br />
          <input
            value={workflowId}
            onChange={(e) => setWorkflowId(e.target.value)}
            style={{ width: "100%", padding: "4px 8px", marginTop: 4 }}
          />
        </label>
      </div>

      <div style={{ marginBottom: "1rem" }}>
        <label>
          Prompt
          <br />
          <input
            value={prompt}
            onChange={(e) => setPrompt(e.target.value)}
            style={{ width: "100%", padding: "4px 8px", marginTop: 4 }}
          />
        </label>
      </div>

      <button
        onClick={sendProposal}
        disabled={loading}
        style={{
          padding: "8px 20px",
          background: "#4f46e5",
          color: "white",
          border: "none",
          borderRadius: 4,
          cursor: loading ? "wait" : "pointer",
          marginBottom: "1.5rem",
        }}
      >
        {loading ? "Sending…" : "Send AI Proposal"}
      </button>

      {/* Stats */}
      {proposals.length > 0 && (
        <div
          style={{
            display: "flex",
            gap: "1.5rem",
            marginBottom: "1rem",
            fontSize: 14,
          }}
        >
          <span>
            <strong>Total:</strong> {proposals.length}
          </span>
          <span>
            <strong>Accepted:</strong>{" "}
            {proposals.filter((p) => p.status === "accepted").length}
          </span>
          <span>
            <strong>Rejected:</strong>{" "}
            {proposals.filter((p) => p.status === "rejected").length}
          </span>
          <span>
            <strong>p95 latency:</strong> {p95}ms
            {p95 > 800 && (
              <span style={{ color: "#dc2626" }}> ⚠ &gt; 800ms SLO</span>
            )}
          </span>
        </div>
      )}

      {/* Proposal log */}
      <table style={{ width: "100%", borderCollapse: "collapse", fontSize: 13 }}>
        <thead>
          <tr style={{ textAlign: "left", borderBottom: "1px solid #e5e7eb" }}>
            <th style={{ padding: "6px 8px" }}>Time</th>
            <th style={{ padding: "6px 8px" }}>Status</th>
            <th style={{ padding: "6px 8px" }}>Tool / Reason</th>
            <th style={{ padding: "6px 8px" }}>Latency</th>
          </tr>
        </thead>
        <tbody>
          {proposals.map((p) => (
            <tr
              key={p.id}
              style={{ borderBottom: "1px solid #f3f4f6" }}
            >
              <td style={{ padding: "5px 8px", color: "#6b7280" }}>
                {p.timestamp.toLocaleTimeString()}
              </td>
              <td
                style={{
                  padding: "5px 8px",
                  color: statusColor[p.status],
                  fontWeight: 600,
                }}
              >
                {p.status.toUpperCase().replace("_", " ")}
              </td>
              <td style={{ padding: "5px 8px" }}>
                {p.toolId ?? p.reason ?? "–"}
              </td>
              <td
                style={{
                  padding: "5px 8px",
                  color: p.latencyMs > 800 ? "#dc2626" : "#374151",
                }}
              >
                {p.latencyMs}ms
              </td>
            </tr>
          ))}
        </tbody>
      </table>

      {proposals.length === 0 && (
        <p style={{ color: "#9ca3af", textAlign: "center", marginTop: "2rem" }}>
          No proposals yet. Click "Send AI Proposal" to start.
        </p>
      )}
    </div>
  );
}

export default AgentDashboard;
