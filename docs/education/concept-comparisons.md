# GP2F Concept Comparisons

This guide maps GP2F's architectural concepts to patterns you are likely already familiar with from standard web development. Each section explains what the standard pattern does, what GP2F replaces it with, and why the trade-off is worth making.

---

## Comparison 1: Event Sourcing + CRDTs vs. Standard REST CRUD

### The Standard Pattern: REST CRUD

In a standard REST application, your data management works like this:

You have a `documents` table in a database. When a user edits a document, the frontend sends `PATCH /documents/123` with the changed fields. The server validates the request, runs `UPDATE documents SET title = 'New Title', updated_at = NOW() WHERE id = 123`, and returns the updated row. The client receives the response and updates its local copy.

This is familiar, simple to implement, and well-understood. Most developers learn it in their first job.

The limitations only become visible at scale and under adversarial conditions.

**Limitation 1: No history.** The `UPDATE` statement overwrites the previous value. If a bug caused incorrect data to be written on Tuesday, and you discover it on Friday, you cannot reconstruct what the data looked like on Monday without having separately implemented a change log. Most applications do not implement a comprehensive change log; they regret it when they need one.

**Limitation 2: Offline writes are impossible.** If a user is on a train with no internet connection and edits a document, the `PATCH` request will fail. The standard solution is to disable write interactions while offline, which is a poor user experience.

**Limitation 3: Concurrency conflicts are resolved by discarding.** If two users edit the same document simultaneously, one of their `PATCH` requests will arrive second and will overwrite the first. This is "last write wins" at the coarsest granularity. The first user's change is silently lost.

### The GP2F Pattern: Event Sourcing + CRDTs

GP2F never issues an `UPDATE` statement. Instead, every change is recorded as an **event**: an immutable, append-only record of what happened. The document's current state is computed by replaying all events from the beginning.

Think of it like a bank account. A bank does not store your current balance and update it on every transaction. It stores every transaction (deposit, withdrawal) and computes the balance by summing them. This means you can reproduce the balance at any point in the past, audit every change, and detect anomalies.

For concurrency, GP2F uses **CRDTs** (Conflict-free Replicated Data Types). A CRDT is a data structure that can be merged from multiple sources without losing any changes, regardless of the order in which changes arrive. Two users editing the same document simultaneously will have both of their changes merged automatically—no data is lost.

**What you gain:**
A complete, immutable audit trail of every change, automatically. Offline writes that sync automatically on reconnection. Conflict-free concurrent editing. The ability to reconstruct any historical state by replaying the event log.

**What you give up:**
Simplicity. Event sourcing requires more infrastructure (an event store, a replay mechanism, CRDT document management). The data model is more complex to reason about than a simple row in a table. Storage grows without bound unless you implement compaction.

**When to use GP2F's model:**
Use it when any of these are true: you need a complete audit trail for compliance reasons, you need offline write support, you have multiple users who can edit the same data concurrently, or you need to reconstruct historical state.

**When to stick with REST CRUD:**
Use it for simple internal tools, low-concurrency applications, or when offline support and complete auditability are not requirements. REST CRUD is not wrong—it is the right tool for many problems.

---

## Comparison 2: Tokenized AI Sandbox vs. Raw OpenAI API Calls

### The Standard Pattern: Raw API Calls with Function Calling

In a standard AI integration, you give the OpenAI API (or equivalent) a set of functions it can call:

```javascript
const tools = [
  {
    type: 'function',
    function: {
      name: 'submitDocument',
      description: 'Submit a document for review',
      parameters: {
        type: 'object',
        properties: {
          documentId: { type: 'string' }
        }
      }
    }
  }
];

const response = await openai.chat.completions.create({
  model: 'gpt-4o',
  messages: [{ role: 'user', content: userMessage }],
  tools,
});
```

If the model decides to call `submitDocument`, your application executes the function. This is simple to implement and gives the AI a lot of capability.

The problem is that the AI has no awareness of business rules. It will call `submitDocument` even if:
- The document is not in a submittable state.
- The user does not have permission to submit.
- Another operation is already in flight for this document.

You can add server-side validation that rejects the call after the fact, but this does not prevent the AI from making the call, and it does not prevent adversarial content in the user's input from manipulating the AI into calling the wrong function with the wrong arguments. This is called **prompt injection**, and it is a real attack that has been demonstrated against production AI integrations.

### The GP2F Pattern: Tokenized AI Sandbox

In GP2F's tokenized sandbox, the AI never receives callable functions. It receives **tokens**—ephemeral, single-use identifiers that represent specific actions the current user is currently permitted to perform.

The process works like this:

Step 1: The system evaluates the AST policy and determines exactly which actions the current user is permitted to take right now. For Alex in her current state, this might be: `["submit_document", "request_revision"]`.

Step 2: The system mints a unique, short-lived token for each permitted action. The token looks like: `tool_req_submit_document_7f3a9b2c`. It is stored in Redis with a 5-minute expiration. Without this token, no one can execute the `submit_document` action through the AI pathway.

Step 3: The AI receives a prompt that includes the tokens and their human-readable descriptions, but no callable functions. It can only select a token or say "no action is appropriate."

Step 4: If the AI selects a token, the application validates the token (it must exist in Redis, it must not have been used before, it must belong to the current session), then uses it to construct and execute the operation through the standard GP2F pipeline.

**What you gain:**
The AI's action space is structurally limited to what is currently permitted by the business rules. Prompt injection cannot cause the AI to perform an unauthorized action, because no token exists for unauthorized actions. Every AI action is traceable to a specific policy evaluation and a specific token mint event.

**What you give up:**
Flexibility. The AI cannot propose novel multi-step plans that involve actions not in the current permitted set. Token minting requires a server round-trip before the AI can be invoked, adding latency to the AI activation path.

**When to use GP2F's sandbox:**
Use it in any production enterprise application where AI has access to actions that affect real data. The constraint on action space is a feature in these environments, not a limitation.

**When raw tool-calling might be acceptable:**
In a purely informational assistant (read-only access), or in a research prototype with no real data, raw tool-calling is simpler and the risk is lower. Do not use raw tool-calling for any AI integration that can mutate data in a production system.

---

## Comparison 3: Isomorphic AST vs. Duplicated Frontend/Backend Validation

### The Standard Pattern: Duplicated Validation

In a standard application, validation logic is written twice: once in the frontend and once in the backend.

The frontend validation might look like this in TypeScript:

```typescript
function canSubmitDocument(document: Document, user: User): boolean {
  return document.status === 'draft' && user.role === 'contributor';
}
```

The backend validation might look like this in Python:

```python
def can_submit_document(document: dict, user: dict) -> bool:
    return document['status'] == 'draft' and user['role'] == 'contributor'
```

These two implementations are supposed to be equivalent. In practice, they diverge over time. A product manager updates the rule: "Contributors AND managers can submit drafts, but managers can also submit documents in 'revision_requested' state." The frontend developer updates the TypeScript. The backend developer is on vacation. The deployment goes out. For one week, the frontend correctly rejects a manager trying to submit a revision-requested document, while the backend correctly accepts it. Users are confused.

This class of bug is extremely common, extremely annoying, and structurally unavoidable when validation logic exists in two separate codebases.

**A second problem:** the frontend validation runs in JavaScript, which is visible to the user. A determined user can open the browser's developer tools, find the validation function, and replace it with one that always returns `true`. This bypasses the UI-level guard entirely. The backend validation is the real security boundary, but the frontend validation is the user experience—and they are not the same code.

### The GP2F Pattern: Isomorphic AST

GP2F solves this with a single policy evaluator that runs in both environments. The evaluator is compiled to WASM (for the browser) and run natively (for the server). The AST that defines the rules is the same data artifact in both environments.

When the rule changes ("managers can also submit revision-requested documents"), a policy author updates the AST. The server distributes the new AST to all connected clients. Both the browser's WASM evaluator and the server's native evaluator are now evaluating the same updated AST. There is one change, not two. There can be no divergence.

The equivalence is verified mathematically using property-based testing: thousands of random inputs are generated and evaluated by both the native and WASM targets, and the outputs must be identical. If they ever differ, a test fails immediately.

For security, the WASM evaluator running in the browser provides **accurate** feedback to the user (the button is disabled because the business rule says so), but the **authoritative** evaluation always happens on the server. A user who modifies the WASM evaluator in their browser will receive incorrect UI feedback, but every `op_id` they emit will be re-evaluated on the server before being accepted. The client-side evaluation is an optimization (zero-latency feedback), not a security control.

**What you gain:**
A single source of truth for business rules. Zero divergence between frontend and backend validation. A versioned, auditable history of every rule change. Accurate UI feedback that matches server-side enforcement.

**What you give up:**
The WASM binary adds approximately 300KB to the initial page load. Rule authoring requires knowledge of the AST schema (though tooling can abstract this). The evaluator is pure—it cannot make decisions based on real-time data that is not in the policy state document.

**When to use GP2F's isomorphic AST:**
Use it whenever business rules are complex enough to be worth centralizing, when rules change frequently, when compliance requires auditability of rule evaluations, or when the cost of frontend/backend divergence is high (e.g., in financial, healthcare, or legal applications).

**When duplicated validation might be acceptable:**
For a small application with very simple, stable rules and a single-language stack (e.g., a Next.js app where both frontend and backend are TypeScript), duplicated validation may be acceptable if the team is disciplined. The risk of divergence grows with team size, rule complexity, and the number of languages in the stack.

---

## Quick Reference Table

| Concept | Standard Web Dev | GP2F |
|---|---|---|
| State changes | `PATCH /resource/:id` | Signed `op_id` events |
| History | Manual audit log (if implemented) | Automatic immutable event log |
| Offline writes | Not supported | IndexedDB queue, auto-replay |
| Concurrency | Last write wins | CRDT merge, no data loss |
| AI actions | Raw function calling | Token-gated, policy-constrained |
| Validation | Duplicated TS + backend code | Single AST, isomorphic evaluator |
| Validation parity | Human discipline | Mathematical proof via proptest |
| UI latency | Network round-trip before update | Local evaluation, 0ms update |
| Audit trail | Optional, application-specific | Mandatory, built-in |
