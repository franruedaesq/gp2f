# The Four Pillars of GP2F

## Why GP2F? The Problem with REST + CRUD + Hardcoded FSMs

Traditional web applications are built on a stack that was designed for a simpler era: a browser sends an HTTP request, a server mutates a database row, and the client polls or waits for confirmation. This architecture creates four compounding failures at enterprise scale.

**The Latency Problem.** Every user action requires a round-trip before the UI can reflect the change. Even on a low-latency connection, this means 50–300ms of frozen UI per interaction. Multiply this across thousands of concurrent users and the perceived performance degrades into a crawl.

**The Business Logic Duplication Problem.** Frontend validation is written in TypeScript; backend validation is written in whatever language runs the API server. These two implementations diverge. A rule change requires a coordinated, multi-team deployment. Until that deployment lands, a class of user interactions is either incorrectly blocked or incorrectly permitted.

**The Auditability Problem.** A `PATCH /resource/123` request destroys the prior state. If an audit reveals a dispute at timestamp T, the only source of truth is whatever was in the database at that moment—if it was even logged. Reconstructing the chain of decisions is forensically expensive or impossible.

**The AI Safety Problem.** LLMs integrated into applications via raw tool-calling have no awareness of business rules. They can propose mutations that violate policies, exceed authorization boundaries, or conflict with in-flight operations from other users. There is no structural guardrail.

GP2F addresses all four problems with a single, unified architecture. It is not a library—it is an architectural contract enforced at the binary level.

---

## Pillar 1: Isomorphic AST Policy Engine

The core insight is that business rules should not exist as code. They should exist as data—specifically, as a versioned, serialized Abstract Syntax Tree (AST) expressed in Protobuf format.

A rule like "a user in role `viewer` cannot submit a document that is in state `archived`" is not a Rust `if` statement. It is an `AstNode` with a structure that can be traversed, versioned, diffed, and evaluated by any conforming runtime.

The `policy-core` crate is compiled to two targets. The first is native Rust, which runs inside the Axum server and Temporal worker processes via Wasmtime. The second is WebAssembly via `wasm-pack`, which is shipped to the browser and runs directly inside the React application. The evaluator function is pure: it accepts a state document and an intent, and it returns an `EvalResult`. It performs no I/O and has no side effects.

```rust
pub struct EvalResult {
    pub permitted: bool,
    pub trace: Vec<String>,
    pub snapshot_hash: String, // BLAKE3 hex of the input state
}
```

**Why Not XState or a Traditional FSM?**

Finite State Machines are expressive for sequential state transitions but fail under the following conditions:

A state space that grows proportionally with product complexity requires a combinatorial explosion of states and transitions. A system with 12 independent boolean properties requires up to 4,096 explicit states in a pure FSM. An AST evaluator requires 12 nodes.

FSMs are code artifacts. They live in a specific language runtime, cannot be stored in a database and diffed over time, and cannot be versioned with a content-addressable hash. An `AstNode` tree is a data artifact. It can be stored in Postgres, replicated to a CDN, loaded from IndexedDB, and transmitted over a WebSocket. Its SHA-256 hash constitutes a verifiable snapshot of the policy state at a point in time.

FSMs cannot be co-evaluated. Two FSMs running in different processes (browser and server) will diverge unless every state transition is explicitly synchronized. Two instances of the same AST evaluator running the same tree against the same state document will produce identical, deterministic results by definition.

**The Trade-off: Payload Size vs. Zero-Latency**

Shipping the WASM evaluator to the browser costs approximately 280–320KB compressed. This is a real cost, paid once on initial load. The return on this investment is every subsequent user action becoming a local computation with zero network dependency. For applications where the P50 interaction frequency exceeds one action per two seconds, the amortized cost of the initial payload is negligible compared to the cumulative latency savings.

**Single Point of Failure Analysis**

The SPOF for this pillar is AST distribution. If the server cannot push an updated AST to clients, clients will continue evaluating stale rules. GP2F mitigates this with a three-layer defense: clients cache the current AST in IndexedDB with a BLAKE3 hash, the server signs every AST version with an Ed25519 key and attaches a monotonic version counter, and clients refuse to evaluate if the local hash does not match the hash embedded in the most recently received signed `op_id` acknowledgment from the server.

---

## Pillar 2: Event-Sourced Synchronization

GP2F never issues a `PATCH` or `UPDATE` command against the canonical data store. Every mutation is an event: a cryptographically signed, immutable record of intent.

The `op_id` is the fundamental unit of synchronization. It is a 32-byte value constructed as:

```
op_id = HMAC-SHA256(key=device_session_key, data=CBOR(intent || timestamp || client_state_hash))
```

When a user triggers an action, the client immediately applies the predicted outcome to its local state (an optimistic update), writes the `op_id` to a local IndexedDB queue encrypted with AES-GCM, and emits the `op_id` over a WebSocket to the server. The UI reflects the outcome instantly—at zero latency—because the update is local.

The server receives the `op_id`, deserializes the intent, re-evaluates the AST against the authoritative state, and determines an outcome: `ACCEPT` or `REJECT`. It then broadcasts a 3-way patch: the original `op_id`, the outcome, and the CRDT diff required to converge all connected clients to the canonical state.

**CRDTs for Mergeable Fields**

Fields that support concurrent modification—such as collaborative text, tag lists, and set-typed collections—are stored as Yrs (Yjs) CRDT documents. A CRDT guarantees eventual consistency without coordination: two clients that apply the same set of operations in any order will converge to the same state. The trade-off is memory: Yrs documents retain a tombstone log of all deleted operations to maintain merge semantics. For a document with high churn, this log can grow to several megabytes. GP2F implements a compaction strategy: after a Temporal workflow confirms that all connected clients have acknowledged a vector clock checkpoint, the tombstone log prior to that checkpoint is pruned.

**LWW and Transactional Fields**

Not all fields require CRDT semantics. A numeric counter with a "last writer wins" semantic uses a simple LWW register with a Hybrid Logical Clock (HLC) timestamp. Fields that must not conflict—such as a workflow state transition—are marked `transactional` in the schema; the server will `REJECT` any `op_id` that targets a transactional field while another unacknowledged `op_id` for the same field is in flight.

**The Trade-off: CRDT Memory Bloat vs. Offline Capability**

The tombstone accumulation in Yrs is the primary operational cost of this pillar. A deployment that allows clients to go offline for extended periods (days, not hours) and then sync must provision more memory in its WebSocket server to hold the uncompacted CRDT state. GP2F's recommendation is to configure the compaction checkpoint interval in proportion to the expected maximum offline duration of clients in the deployment. The memory overhead is bounded and predictable; the alternative—requiring clients to be online to perform any write—is architecturally unacceptable for a local-first system.

**Single Point of Failure Analysis**

The SPOF for this pillar is the IndexedDB queue on the client. If the browser clears IndexedDB (e.g., a user clears site data), unsynced operations are permanently lost. GP2F mitigates this by writing each `op_id` to two durability targets before returning control to the UI: the in-memory CRDT document and the encrypted IndexedDB queue. A future hardening option is a secondary sync to a user-owned cloud storage bucket, which is outside the current scope of GP2F core.

---

## Pillar 3: Tokenized Agent Sandbox

LLMs are non-deterministic systems with no intrinsic awareness of application state, authorization rules, or concurrency constraints. Integrating them via raw tool-calling—giving an LLM the ability to call `submitDocument()` or `deleteRecord(id)` directly—creates an authorization bypass that is qualitatively different from a conventional bug. It is a structural gap in the trust boundary.

GP2F eliminates this gap with a tokenized proposal protocol. The LLM is never given a function it can call directly. Instead, it is given a constrained vocabulary of ephemeral tokens, each of which represents a single permitted action from the AST.

The interaction protocol is:

1. The Semantic Vibe Engine evaluates the user's current context and produces a vibe vector: `{ intent, confidence, bottleneck }`.
2. The vibe vector is passed to the policy engine, which evaluates the AST and returns the set of permitted actions for this user in this state.
3. For each permitted action, the server mints an ephemeral token with a 5-minute Redis TTL: `tool_req_<action_id>_<uuid>`.
4. The LLM receives a system prompt that contains only the vibe context and the list of valid tokens. It cannot see the action implementations; it can only select a token.
5. When the LLM selects a token, the application constructs the `op_id` and runs it through the standard synchronization pipeline. The token is consumed atomically with the `op_id` emission.

**Why This Guarantees Compliance**

The LLM is constrained to a finite, policy-derived action set at the moment of invocation. It cannot propose an action that is not in the permitted set because no token exists for it. It cannot replay a token because tokens are consumed on first use. It cannot forge a token because tokens are generated server-side with a secret key and verified on redemption. The auditability is complete: every AI action is traceable to a specific token mint event, which is itself traceable to a specific AST evaluation result, which is itself traceable to a specific vibe vector computation.

**The Trade-off: AI Creativity vs. Safety**

The tokenized sandbox intentionally limits the LLM's action space to what is permitted at the moment of invocation. This means an LLM cannot propose a "creative" solution that requires an action not currently permitted by the AST. For enterprise deployments, this is a feature, not a limitation—business rules exist precisely to constrain what is permitted. For research or highly exploratory workflows, policy authors can define broader "draft mode" rules that expand the permitted action set, which then expands the token vocabulary available to the LLM.

**Single Point of Failure Analysis**

The SPOF for this pillar is Redis token storage. If the Redis instance is unavailable, no AI-assisted actions can be proposed. GP2F mitigates this by treating AI assistance as a progressive enhancement: if the token minting service is unavailable, the UI falls back to direct user interaction, which continues to function normally through the standard `op_id` pipeline.

---

## Pillar 4: Semantic Vibe Engine

Context management is the unsolved problem in human-computer interaction at scale. A user staring at a form with a blocked submit button has no idea whether the block is due to a missing field, a policy restriction, a concurrency conflict, or a system error. This ambiguity is resolved manually—by reading error messages, consulting documentation, or asking a colleague.

The Semantic Vibe Engine is a lightweight, on-device neural classifier that continuously evaluates the user's interaction context and produces a three-dimensional output:

```typescript
interface VibeVector {
  intent: string;       // e.g., "submit_document", "navigate_back"
  confidence: number;   // 0.0 to 1.0
  bottleneck: string;   // e.g., "missing_required_field", "policy_restriction"
}
```

The classifier is an ONNX model of approximately 4MB, loaded once and evaluated locally using the ONNX Runtime WASM backend. It consumes a feature vector derived from the current form state, recent user interaction events, and the current AST evaluation trace. It runs in a Web Worker to avoid blocking the main thread.

The vibe vector serves two consumers. First, the UI uses it to render proactive assistance: if `bottleneck` is `policy_restriction` and `confidence` exceeds 0.85, the UI renders an AI assistant prompt that explains the restriction and offers permitted alternatives. Second, the Tokenized Agent Sandbox uses it to scope the token vocabulary to the user's inferred intent, reducing noise in the LLM's action space.

**The Trade-off: Model Accuracy vs. Binary Size**

The 4MB ONNX model is a significant addition to the initial payload. A quantized INT8 version reduces this to approximately 1.2MB with a confidence degradation of roughly 3 percentage points at the P50. For deployments on high-latency or metered connections, the quantized model is recommended. The model is loaded lazily—it does not block the initial render of the application.

**Single Point of Failure Analysis**

The SPOF for this pillar is model freshness. If user interaction patterns shift and the model is not retrained, vibe accuracy degrades, leading to incorrect bottleneck attribution and poor AI suggestions. GP2F ships the model with the application bundle and provides a model versioning protocol: the server can push a new model version identifier via the WebSocket channel, and the client downloads the updated model in the background.
