# GP2F From Scratch: A Junior Developer's Guide

This guide explains exactly what happens when a user clicks a button in a GP2F application. We will follow a single click from the moment the mouse button is pressed to the moment the UI updates—touching every component in the system along the way.

This guide assumes you are comfortable with React and JavaScript, and that you have some familiarity with how web applications talk to servers. We will build up the more complex concepts step by step.

---

## The Setup: What We Are Building

Imagine a document review application. A user named Alex can view documents assigned to them. They can click a "Submit for Review" button to send a document to their manager. There is one rule: Alex can only submit a document if the document's status is "draft" and Alex has the role "contributor".

In a traditional application, when Alex clicks "Submit for Review", the button is grayed out or a spinner appears while the browser sends a request to the server, the server checks the database, the server mutates the database, and the server sends back a response. This takes 50–300ms, and the UI is frozen the entire time.

In GP2F, the moment Alex clicks the button, the UI updates instantly. The document visually moves to "pending review" status before any network request has been sent. This is called an **optimistic update**. The network request happens in the background, and if something goes wrong, the UI gracefully rolls back.

Let's trace how this works.

---

## Part 1: Before the Click — The AST Policy Engine

Before Alex can even see the "Submit for Review" button, something interesting has already happened. When Alex's browser first loaded the application, it received a special file alongside the normal JavaScript: a WebAssembly (WASM) binary called the **policy evaluator**.

WebAssembly is like a mini-program that runs inside the browser, separate from JavaScript but able to communicate with it. The GP2F policy evaluator is written in Rust (a systems programming language known for being fast and reliable) and compiled to WASM so it can run anywhere.

The policy evaluator does one thing: it looks at the current state of the application and a description of what the user wants to do, and it says "yes, this is allowed" or "no, this is not allowed". It does this in zero milliseconds because it runs entirely in the browser, with no network connection needed.

The rules that the evaluator enforces are stored in a structure called an **Abstract Syntax Tree** (AST). An AST is just a way of representing a rule as data (specifically, a tree of nodes) rather than as code. For our example rule, the AST looks something like this:

```json
{
  "nodeType": "AND",
  "children": [
    {
      "nodeType": "EQUALS",
      "field": "document.status",
      "value": "draft"
    },
    {
      "nodeType": "EQUALS",
      "field": "user.role",
      "value": "contributor"
    }
  ]
}
```

The evaluator reads this tree and checks: is `document.status` equal to `draft`? And is `user.role` equal to `contributor`? If both are true, the action is permitted.

**Why use an AST instead of just writing the rule in code?**

Because the AST is data. It can be sent from the server to the browser, stored in the browser's local storage, versioned with a unique hash, and evaluated by the same logic on both the server and the browser. If the server and browser are both evaluating the same AST, they will always reach the same conclusion.

---

## Part 2: The Click — Local Evaluation

Alex clicks "Submit for Review". Here is what happens in the browser in the first millisecond.

The button's click handler calls the policy evaluator (the WASM binary) with two pieces of information: the current state of the document (its status, Alex's role, etc.) and the intent (what Alex wants to do: "submit_document").

The evaluator runs in about 0.1 milliseconds and returns:

```json
{
  "permitted": true,
  "trace": [
    "Checking AND node",
    "document.status == 'draft': true",
    "user.role == 'contributor': true",
    "AND result: true"
  ],
  "snapshotHash": "a3f9bc..."
}
```

The `permitted: true` result means the action is allowed. The `trace` is a log of every check the evaluator performed—this is stored for auditing later. The `snapshotHash` is a fingerprint of the exact state the evaluator saw when it made this decision.

Because the evaluation happened locally in the browser, the UI can update immediately. The document's status changes to "pending review" on screen. Alex sees the update happen instantly.

This is the **optimistic update**: we are optimistically assuming the server will agree with our local evaluation. In practice, the server will agree the vast majority of the time, because both the browser and the server are evaluating the same AST.

---

## Part 3: Signing the Operation — The op_id

At the same time as the optimistic update, the browser creates a unique, tamper-proof identifier for this operation. This is called an **op_id**.

Think of the `op_id` like a wax seal on a medieval letter. It proves that this specific operation was created by Alex's specific browser session at this specific moment with this specific state. If anyone tampers with the operation (changes the intent, the timestamp, or the state hash), the seal breaks.

The `op_id` is created using a cryptographic technique called HMAC-SHA256. It combines Alex's secret session key with the details of the operation (the intent, the time, the state fingerprint) and produces a 32-byte "seal". Without Alex's session key, no one can create a valid `op_id` for Alex's session.

```
op_id = HMAC-SHA256(
  key  = Alex's secret session key,
  data = (intent + timestamp + state_fingerprint + sequence_number)
)
```

---

## Part 4: Writing to the Encrypted Queue — IndexedDB

Here is something important: the browser writes the `op_id` to local storage **before** sending it to the server.

This local storage is called **IndexedDB**—it is a small database built into every browser. GP2F encrypts everything written to IndexedDB using AES-GCM encryption (a strong symmetric encryption algorithm), so that even if someone gains access to the browser's local storage, they cannot read the pending operations.

Why write to local storage before sending to the server? Because networks are unreliable. If Alex's Wi-Fi drops at the exact moment she clicks the button, the `op_id` is not lost. It sits in the encrypted queue, waiting. When connectivity is restored, the queue automatically replays all pending operations.

This is what **offline-first** means: the application continues to function even when the network is unavailable, and it catches up automatically when the connection returns.

---

## Part 5: The Network — WebSocket Emission

With the optimistic update applied and the `op_id` durably stored, the browser sends the `op_id` to the server over a **WebSocket** connection.

A WebSocket is different from a normal HTTP request. Instead of opening a new connection for each message, a WebSocket maintains a single persistent, bidirectional connection between the browser and the server. This is much faster for the kind of high-frequency, real-time communication that GP2F needs.

The browser sends a message that looks like this:

```json
{
  "type": "OP_ID",
  "payload": {
    "opId": "a3f9bc...",
    "sessionId": "sess_xyz789",
    "intent": { "actionId": "submit_document", "documentId": "doc_456" },
    "clientStateHash": "b7e2aa...",
    "timestampMs": 1706745600000,
    "sequenceNumber": 42
  }
}
```

---

## Part 6: The Server — Re-evaluation and the Temporal Workflow

The server receives the `op_id`. The first thing it does is verify the HMAC seal: does this `op_id` actually match the claimed session key and payload? If not, the operation is rejected immediately and logged as a suspicious event.

If the seal is valid, the server passes the operation to a **Temporal workflow**. Temporal is a system for running durable, fault-tolerant background processes. If the server crashes in the middle of processing this operation, Temporal will resume from where it left off when the server restarts. Nothing is lost.

The Temporal workflow performs four steps:

**Step 1:** Fetch the authoritative state of the document from the database. The server does not trust the client's `clientStateHash` as the ground truth—it uses it to look up the exact snapshot the client was working from, so it can detect if the client had stale data.

**Step 2:** Re-evaluate the AST policy. The server runs the same policy evaluator against the same state and the same intent. Because the evaluator is deterministic (given the same inputs, it always produces the same output), the server will reach the same conclusion as the browser—unless the state changed between when the client sent the operation and when the server processed it.

**Step 3:** Persist the outcome. The server writes the `op_id`, the evaluation result, and the outcome (`ACCEPT` or `REJECT`) to the database's event log. This record is immutable—it can never be changed or deleted. This is the **audit trail**.

**Step 4:** Broadcast the result. The server publishes a message to a **Redis PubSub** channel for this document. Redis is a very fast in-memory data store that GP2F uses for real-time messaging. Every browser that has this document open is subscribed to this channel.

---

## Part 7: The CRDT Merge — Handling Concurrent Edits

What if two users (Alex and Sam) both made changes to the document at the same time? When Sam's browser receives Alex's operation, it needs to merge Alex's change with Sam's change without losing either.

GP2F uses a technology called a **CRDT** (Conflict-free Replicated Data Type) for this. A CRDT is a special kind of data structure that can always be merged without conflicts—no matter what order the operations arrive in, they will always produce the same final result.

Think of it like Google Docs: if two people type in the same document simultaneously, their changes are merged automatically. CRDTs are the mathematical mechanism that makes this possible.

The server computes the merge and broadcasts a **3-way patch**: here is what Alex's operation changed, here is what Sam's operation changed, and here is the merged result. Every browser applies this patch to synchronize to the canonical state.

---

## Part 8: Back to the UI — Finalizing the Optimistic Update

Alex's browser receives the broadcast from the server:

```json
{
  "type": "OP_ACK",
  "payload": {
    "opId": "a3f9bc...",
    "outcome": "ACCEPT",
    "canonicalPatch": { ... }
  }
}
```

Because the outcome is `ACCEPT`, two things happen:

1. The `op_id` is removed from the encrypted IndexedDB queue (it has been successfully processed).
2. The optimistic update is finalized—the document's "pending review" status, which was applied speculatively, is now confirmed as canonical.

From Alex's perspective, nothing changed—the document has been in "pending review" status since the moment she clicked the button. The entire round-trip to the server happened invisibly, in the background, in approximately 10–15ms.

---

## What Happens When an Operation is Rejected?

If the server sends `"outcome": "REJECT"`, the browser performs a **rollback**. The optimistic update is reverted, the document returns to its previous state, and an error message is shown to Alex explaining why the action was not permitted.

This can happen if Alex's policy data was stale (perhaps her role changed on the server after she loaded the page) or if there was a concurrency conflict that the client-side evaluation could not have detected.

Rejections are rare in a well-functioning system, but they are handled gracefully without data loss or user confusion.

---

## The Complete Journey: A Summary

A single button click travels the following path:

1. Browser evaluates the AST policy locally → instant result (0ms).
2. UI applies optimistic update → user sees the change immediately.
3. Browser generates a signed `op_id`.
4. Browser encrypts and writes the `op_id` to IndexedDB.
5. Browser emits the `op_id` over WebSocket to the server.
6. Server verifies the HMAC seal on the `op_id`.
7. Server starts a Temporal workflow.
8. Workflow re-evaluates the AST against the authoritative state.
9. Workflow persists the outcome to the immutable event log.
10. Workflow broadcasts `ACCEPT`/`REJECT` + CRDT patch via Redis PubSub.
11. All connected browsers receive the broadcast and synchronize.
12. Originating browser dequeues the `op_id` and finalizes (or reverts) the optimistic update.

Steps 1–4 happen before any network communication. Steps 5–12 happen asynchronously in the background. The user experiences steps 1–4 as the complete interaction: instant, seamless, and reliable.
