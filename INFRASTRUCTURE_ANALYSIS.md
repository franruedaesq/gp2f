# Infrastructure & Installation Analysis for GP2F

## Executive Summary

**Is it "too much"?**
- **For a simple blog or CRUD app:** Yes. The complexity of Conflict-Free Replicated Data Types (CRDTs) and event sourcing is overkill.
- **For a "Google Docs"-style collaborative app or offline-first tool:** No. It is actually *very lightweight* compared to building these features from scratch.
- **For an Enterprise Workflow System:** It is "just right" in architecture but **currently incomplete** in implementation (persistence is stubbed).

## 1. Installation Requirements

### Mandatory (Minimum to Run Locally)
To run the framework in "Demo Mode" (which is the default), you need very little:

1.  **Rust Toolchain (1.78+):** Required to compile and run the backend server.
    -   *Command:* `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
2.  **Node.js (v18+):** Required to build the client SDK and run the demo frontend.
3.  **No Database Required:** By default, the system runs entirely in-memory.
    -   *Note:* Use caution! If you restart the server, **all data is lost** in the default configuration.

### Optional (Production Grade)
To run this in a real production environment, the requirements increase significantly. **Crucially, some of these integrations are currently "stubs" in the code and require you to finish writing the integration logic.**

1.  **Redis (Highly Recommended):**
    -   *Purpose:* Handles real-time updates (PubSub) across multiple server instances.
    -   *Status:* Supported via the `redis-broadcast` feature flag in `Cargo.toml`.
2.  **Temporal.io Cluster (Required for Persistence):**
    -   *Purpose:* Stores the long-term history of all events and workflows.
    -   *Status:* **Stubbed.** The code in `server/src/temporal_store.rs` contains the *logic* for how to connect, but the actual SDK calls are commented out or replaced with logging. You cannot use this out-of-the-box without writing Rust code to finish the integration.
3.  **Postgres (via Temporal):**
    -   *Purpose:* Temporal uses Postgres as its own backing store. You don't interact with Postgres directly; Temporal manages it.
4.  **LLM API Keys (Optional):**
    -   *Purpose:* OpenAI, Anthropic, or Groq keys for AI features.
    -   *Status:* Optional. If missing, the system falls back to a "Mock" provider that returns dummy responses.

## 2. Framework Comparison

| Feature | **Next.js / Remix** | **Temporal.io** | **GP2F (This Framework)** |
| :--- | :--- | :--- | :--- |
| **Primary Goal** | Web UI & API Routes | Durable Workflow Orchestration | **Offline-First Policy Engine** |
| **State Management** | Database (Postgres/MySQL) | Event History (in DB) | **CRDTs + Event Sourcing** |
| **Offline Support** | None (requires manual fetch/sync) | None (server-side only) | **Built-in (Queue + Reconcile)** |
| **Conflict Resolution**| Last-Write-Wins (usually) | Deterministic Replay | **3-Way Merge / Custom Policy** |
| **Complexity** | Low / Medium | High (requires cluster) | **Medium (Backend is Rust)** |
| **Infrastructure** | Node.js + DB | Node/Go/Java + DB + ES | **Rust Binary + (Optional) Temporal** |

### vs. Next.js
-   **Next.js** is a full-stack web framework. You build your UI and simple APIs with it.
-   **GP2F** is a *backend engine*. You would likely use **Next.js** to build the frontend of your application, and use the **GP2F Client SDK** inside your Next.js components to talk to the GP2F server.
-   *Verdict:* They are complementary, not competitors.

### vs. Temporal
-   **Temporal** is a "Workflow-as-Code" platform. It guarantees code runs to completion even if servers crash.
-   **GP2F** is designed to sit *on top* of Temporal. It adds the "Client-Side" intelligence (optimistic updates, offline support) that Temporal lacks.
-   *Verdict:* GP2F uses Temporal as a "hard drive" for reliability.

## 3. Cost/Value Analysis

### Is it worth it?

**YES, IF:**
-   You need **Offline Support**: Your users (e.g., delivery drivers, field medics) act in areas with bad internet.
-   You need **Real-time Collaboration**: Multiple users editing the same "ticket" or "document" at once.
-   You need **Audit Trails**: You need to prove exactly *why* a state changed (cryptographically signed logs).
-   You need **AI Safety**: You want LLMs to propose actions, but need a strict "Policy Guardrail" to prevent them from doing dangerous things.

**NO, IF:**
-   You are building a simple dashboard or e-commerce site.
-   You don't know Rust and don't want to learn it (the backend is 100% Rust).
-   You need a "finished product" today. (The persistence layer requires dev work).

## 4. Summary Recommendation

**Current Status:**
The framework is in a **"High-Fidelity Prototype"** state. It is excellent for demonstrating the *concept* of local-first, policy-driven workflows.

**To use it today:**
1.  **For Demo:** Install Rust & Node. Run `cargo run`. It works perfectly in memory.
2.  **For Production:** You must be willing to:
    -   Deploy a Temporal cluster.
    -   Uncomment and finish the `TemporalStore` implementation in `server/src/temporal_store.rs`.
    -   Set up Redis.

**Conclusion:** It is not "too much" infrastructure-wise *if* you need the advanced features it offers. However, it *is* a significant engineering commitment to finish the integration code required for a persistent production deployment.
