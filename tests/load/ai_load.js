/**
 * GP2F AI Gateway Load Test
 *
 * Phase 10 requirement 2: k6 script simulating 500 concurrent users + AI agents,
 * measuring p95 latency < 800 ms and zero policy violations under load.
 *
 * Usage:
 *   k6 run tests/load/ai_load.js
 *   k6 run --env BASE_URL=http://localhost:3000 tests/load/ai_load.js
 *
 * Pass/fail thresholds:
 *   - http_req_duration p(95) < 800ms
 *   - http_req_failed   rate < 0.001  (< 0.1 % HTTP errors)
 *   - policy_violations count == 0
 */

import http from "k6/http";
import { check, sleep } from "k6";
import { Counter, Trend } from "k6/metrics";

// ── configuration ─────────────────────────────────────────────────────────────

const BASE_URL = __ENV.BASE_URL || "http://localhost:3000";

/** Number of virtual users to ramp to. */
const PEAK_VUS = 500;

// ── custom metrics ────────────────────────────────────────────────────────────

/** Counts tool calls that were not in the allowed list (policy violations). */
const policyViolations = new Counter("policy_violations");

/** Tracks the latency of /agent/propose requests specifically. */
const agentProposeLatency = new Trend("agent_propose_latency", true);

// ── thresholds ────────────────────────────────────────────────────────────────

export const options = {
  scenarios: {
    /**
     * Human users sending reconciliation ops.
     * Ramps from 0 → 250 VUs over 30 s, holds for 2 min, then ramps down.
     */
    human_users: {
      executor: "ramping-vus",
      startVUs: 0,
      stages: [
        { duration: "30s", target: 250 },
        { duration: "2m", target: 250 },
        { duration: "10s", target: 0 },
      ],
      gracefulRampDown: "10s",
    },
    /**
     * AI agents sending /agent/propose requests.
     * Ramps from 0 → 250 VUs over 30 s, holds for 2 min, then ramps down.
     */
    ai_agents: {
      executor: "ramping-vus",
      startVUs: 0,
      stages: [
        { duration: "30s", target: 250 },
        { duration: "2m", target: 250 },
        { duration: "10s", target: 0 },
      ],
      gracefulRampDown: "10s",
      exec: "aiAgentScenario",
    },
  },
  thresholds: {
    /** p95 latency must be below 800 ms. */
    http_req_duration: ["p(95)<800"],
    /** HTTP error rate must be below 0.1 %. */
    http_req_failed: ["rate<0.001"],
    /** Zero policy violations (LLM choosing a disallowed tool). */
    policy_violations: ["count==0"],
    /** AI-specific: p95 of /agent/propose must be below 800 ms. */
    agent_propose_latency: ["p(95)<800"],
  },
};

// ── helpers ───────────────────────────────────────────────────────────────────

/** Generate a random tenant and workflow ID for isolation. */
function tenantContext() {
  const id = Math.floor(Math.random() * 100);
  return {
    tenantId: `load-tenant-${id}`,
    workflowId: `load-wf-${id % 10}`,
    instanceId: `load-inst-${__VU}-${__ITER}`,
  };
}

/** Serialise a vibe vector. */
function makeVibe(intent, confidence, bottleneck) {
  return { intent, confidence, bottleneck };
}

/** Pick a random allowed tool ID. */
function randomAllowedTool() {
  const tools = [
    "tool_req_extract_symptoms_8k2p9",
    "tool_req_summarize_workflow_3x7r1",
    "tool_req_suggest_next_action_9q4m2",
  ];
  return tools[Math.floor(Math.random() * tools.length)];
}

/** Pick a random vibe intent. */
function randomVibe() {
  const intents = ["focused", "frustrated", "confused", "stuck", "exploring"];
  const intent = intents[Math.floor(Math.random() * intents.length)];
  return makeVibe(intent, Math.random(), "current_step");
}

// ── human user scenario (default export) ─────────────────────────────────────

export default function () {
  const ctx = tenantContext();

  // 1. Health check
  const healthRes = http.get(`${BASE_URL}/health`);
  check(healthRes, { "health: status 200": (r) => r.status === 200 });

  // 2. Submit an optimistic op through /op
  const opPayload = JSON.stringify({
    opId: `op-${__VU}-${__ITER}-${Date.now()}`,
    astVersion: "1.0.0",
    action: "update",
    payload: { field: `value-${__ITER}` },
    clientSnapshotHash: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    tenantId: ctx.tenantId,
    workflowId: ctx.workflowId,
    instanceId: ctx.instanceId,
    role: "default",
  });

  const opRes = http.post(`${BASE_URL}/op`, opPayload, {
    headers: { "Content-Type": "application/json" },
  });

  check(opRes, {
    "op: status 200": (r) => r.status === 200,
    "op: has type field": (r) => {
      try {
        const body = JSON.parse(r.body);
        return body.type === "ACCEPT" || body.type === "REJECT";
      } catch {
        return false;
      }
    },
  });

  sleep(0.1);
}

// ── AI agent scenario ─────────────────────────────────────────────────────────

export function aiAgentScenario() {
  const ctx = tenantContext();

  const vibe = randomVibe();
  const body = JSON.stringify({
    tenantId: ctx.tenantId,
    workflowId: ctx.workflowId,
    instanceId: ctx.instanceId,
    astVersion: "1.0.0",
    prompt: "What is the most helpful next action for the user?",
    vibe,
  });

  const start = Date.now();
  const res = http.post(`${BASE_URL}/agent/propose`, body, {
    headers: { "Content-Type": "application/json" },
    tags: { name: "agent_propose" },
  });
  agentProposeLatency.add(Date.now() - start);

  check(res, {
    "agent/propose: status 200 or 429": (r) => r.status === 200 || r.status === 429,
  });

  if (res.status === 200) {
    try {
      const responseBody = JSON.parse(res.body);

      // Check for policy violations: if the response contains "disallowed tool",
      // that means the LLM tried to use a tool it shouldn't have.
      if (
        responseBody.status === "proposal_rejected" &&
        responseBody.reason === "disallowed tool"
      ) {
        policyViolations.add(1);
        console.error(
          `POLICY VIOLATION: tenant=${ctx.tenantId} workflow=${ctx.workflowId} reason=${responseBody.reason}`
        );
      }
    } catch {
      // Non-JSON response is acceptable (rate-limited plain text, etc.)
    }
  }

  sleep(0.05);
}
