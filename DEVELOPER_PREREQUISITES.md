# Developer Prerequisites & Skills Matrix

This guide outlines the technical knowledge required to **use and implement** the GP2F framework in a new project. It distinguishes between the roles of a Backend/Workflow Engineer (Rust) and a Frontend/Client Engineer (TypeScript).

---

## Quick Answer: Do I need to know Rust?

**Yes.** To implement this framework in a new project, you must have a developer who knows Rust.

### Why?
Currently, the framework does not support loading workflow definitions from external configuration files (like JSON or YAML) at runtime. Instead, workflows are defined as Rust structs and must be registered in the server's source code (e.g., `server/src/pilot_workflows.rs`).

To add a new workflow, you must:
1.  Write a Rust function that builds a `WorkflowDefinition` struct.
2.  Register that function in the `WorkflowRegistry` in `server/src/main.rs`.
3.  Recompile the server binary.

### Can a TypeScript developer use it?
A TypeScript developer **cannot** implement the backend logic or define new workflows on their own. However, they can fully implement the **Frontend** application using the provided `client-sdk`. The client SDK abstracts away the WebSocket communication and cryptographic signing, allowing the frontend developer to work with familiar TypeScript interfaces.

---

## Knowledge Matrix

### 1. Backend / Workflow Engineer (Rust)
*Responsible for defining business logic, workflows, and hosting the server.*

| Area | specific Knowledge Required | Why? |
|------|-----------------------------|------|
| **Language** | **Rust (Intermediate)** | You must read and modify the server source code to add your own `WorkflowDefinition`s. You need to understand structs, enums, `Option`/`Result`, and basic ownership. |
| **Frameworks** | **Axum / Tokio** | The server is built on Axum. You may need to add custom HTTP endpoints or middleware. |
| **Concepts** | **Event Sourcing** | The system is an append-only log. You must understand that state is derived from a sequence of events, not a CRUD database. |
| **Concepts** | **CRDTs (Conflict-free Replicated Data Types)** | Understanding how concurrent edits are merged (using Yrs/Yjs) is crucial for designing data models that don't break under high concurrency. |
| **Infrastructure** | **Temporal (Optional but Recommended)** | For production durability, the system integrates with Temporal. Understanding Temporal's workflow/activity model is helpful for long-running processes. |

### 2. Frontend / Client Engineer (TypeScript)
*Responsible for building the user interface and connecting to the workflow engine.*

| Area | Specific Knowledge Required | Why? |
|------|-----------------------------|------|
| **Language** | **TypeScript (Intermediate)** | The `client-sdk` is written in TypeScript. You need to be comfortable with async/await, strict typing, and handling WebSocket events. |
| **Frameworks** | **React (or any UI lib)** | The SDK provides React hooks (like `useGp2fClient`), but the core client is framework-agnostic. |
| **Concepts** | **Optimistic UI** | The framework relies heavily on optimistic updates. You need to understand how to handle "tentative" state that might be rejected or modified by the server later. |
| **Concepts** | **WebSocket APIs** | The communication is primarily over WebSockets. Understanding connection lifecycles (connect, disconnect, reconnect) is important for building robust UIs. |

---

## Example Workflow Implementation Steps

To illustrate where each skill is used, here is the process for adding a new "Document Approval" workflow:

1.  **Backend (Rust):**
    *   Create a new file `server/src/workflows/document_approval.rs`.
    *   Define the `WorkflowDefinition` struct, including activities ("upload", "review", "sign") and their policies (AST nodes).
    *   Register the workflow in `server/src/main.rs`.
    *   Run `cargo build` and deploy the server.

2.  **Frontend (TypeScript):**
    *   Import `Gp2fClient` from `@gp2f/client-sdk`.
    *   Connect to the server with `tenantId="my-tenant"` and `workflowId="document_approval"`.
    *   Build UI components that call `client.send({ action: "upload", ... })`.
    *   Handle `onAccept` and `onReject` events to update the UI state.
