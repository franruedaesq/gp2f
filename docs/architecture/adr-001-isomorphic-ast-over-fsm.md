# ADR-001: Isomorphic AST Policy Engine over Finite State Machines

**Status:** Accepted
**Date:** 2025-01-15
**Deciders:** Principal Engineering, Platform Architecture, Security

---

## Context

GP2F requires a mechanism for expressing, evaluating, and distributing business rules across a heterogeneous runtime environment that includes a browser (WASM), a Rust application server (Axum), and an async workflow engine (Temporal). The rules themselves are subject to continuous change by product and compliance teams, and every evaluation must be fully auditable with a tamper-evident hash.

The incumbent approach in the codebase was a collection of hardcoded conditional blocks, duplicated across the TypeScript frontend and the Rust backend. A partial migration to XState had been explored to address the duplication, but it introduced its own set of constraints. The fundamental question this ADR addresses is: what is the correct abstraction for encoding and distributing business rules in a system with these properties?

---

## Decision

We will replace all hardcoded conditional logic and any FSM-based rule encoding with a versioned, Protobuf-serialized Abstract Syntax Tree (AST). The evaluator for this AST will be a single Rust crate (`policy-core`) compiled to two targets: native Rust (for the server) and WebAssembly (for the browser and Wasmtime). The AST will be the single source of truth for all business rules.

---

## Alternatives Considered

**Alternative 1: XState v5**

XState is a mature, well-documented statechart library with TypeScript bindings and a visual debugging tool. We prototyped a subset of GP2F's rules as an XState machine and evaluated it against the following requirements.

The first requirement is polyglot evaluation. XState is a JavaScript/TypeScript library. Achieving evaluation parity in the Rust backend required either running a Node.js process inside the server (unacceptable operational overhead) or reimplementing the state machine in Rust (which reintroduces the duplication problem). A TypeScript-to-Rust transpiler approach was prototyped and found to be brittle—the generated Rust code was not idiomatic and failed to handle all XState transition semantics.

The second requirement is content-addressable versioning. XState machines are code objects. Versioning them requires a code deployment. There is no native mechanism to compute a deterministic hash of an XState machine configuration and compare it across runtimes. Attempts to serialize the configuration to JSON introduced ambiguity around guard functions, which are JavaScript closures and are not serializable.

The third requirement is auditability of evaluation traces. XState provides event history, but it does not produce a structured trace of which conditions were evaluated and why an outcome was reached. Implementing this required wrapping every guard function with logging middleware, which was invasive and fragile.

**Verdict:** XState was eliminated because it cannot satisfy the polyglot evaluation requirement without reintroducing duplication, and it cannot satisfy the content-addressable versioning requirement due to its code-centric model.

**Alternative 2: Open Policy Agent (OPA) with Rego**

OPA is the industry standard for policy-as-code in cloud-native environments. It is mature, has a large ecosystem, and supports a declarative policy language (Rego) that is designed for this use case.

OPA evaluation requires a running OPA server or the OPA Go library embedded in the application. The Go library cannot be compiled to WebAssembly in a manner that integrates cleanly with the browser runtime. OPA does provide a WASM compilation target for Rego bundles, but it requires the OPA toolchain to be present at build time and produces a bundle that is tightly coupled to the Rego source—it is not a general-purpose evaluator that can accept a dynamically loaded policy graph.

Rego as a language is powerful but has a steep learning curve. For a team where policy authoring will be done by product managers and compliance officers (not solely engineers), Rego's Datalog-like semantics were found to be a significant barrier to adoption. Internal user testing showed that the target authors could not produce correct Rego policies without significant engineering assistance.

**Verdict:** OPA was eliminated because the WASM compilation model is incompatible with our dynamic policy loading requirement, and the Rego language is not accessible to non-engineering policy authors.

**Alternative 3: JSON Schema + Ajv Validation**

JSON Schema is a widely-used standard for data validation. Ajv is a high-performance JSON Schema validator available in JavaScript and, via bindings, in other environments.

JSON Schema is designed for structural validation, not semantic policy evaluation. It can express that a field must be present or must match a pattern, but it cannot express stateful rules like "this action is only permitted if the user's role includes `approver` AND the document is in state `pending_review`". Expressing such rules requires JSON Schema extensions (like `if/then/else`) that quickly become unreadable for complex policies.

More critically, JSON Schema has no mechanism for producing an evaluation trace that explains why a document failed validation. The output is a list of validation errors, not a structured decision log. This is insufficient for audit requirements.

**Verdict:** JSON Schema was eliminated because it cannot express stateful, contextual business rules and provides insufficient auditability.

**Alternative 4: Drools (Java Rule Engine)**

Drools is a mature, enterprise-grade rule engine with decades of production history. It supports complex forward-chaining inference and has a declarative rule syntax (DRL).

Drools is a JVM technology. Integrating it into a Rust/WASM stack requires a JVM sidecar process, which adds substantial operational complexity, increases container footprint, and introduces a cross-process latency penalty for every rule evaluation. WASM compilation of Drools is not a viable path.

**Verdict:** Drools was eliminated due to JVM runtime incompatibility with the target stack.

---

## Consequences

**Positive Consequences**

The single `policy-core` crate compiled to native and WASM targets guarantees evaluation parity: the same AST evaluated against the same state will produce the same result in the browser and on the server. This eliminates the class of bugs caused by logic divergence between frontend and backend validation.

ASTs are data artifacts. They can be stored in Postgres, versioned with a monotonic counter and a BLAKE3 content hash, distributed via CDN, cached in IndexedDB, and compared across clients. Policy changes do not require a code deployment; they require an AST update and distribution event.

The `EvalResult` structure contains a full evaluation trace: a `Vec<String>` of condition evaluations with their outcomes. This trace is stored alongside the `op_id` in the event log and provides a complete, machine-readable audit record for every business decision made by the system.

**Negative Consequences**

The `policy-core` WASM binary adds approximately 300KB compressed to the initial client payload. This cost must be accepted as the price of zero-latency local evaluation.

Policy authoring currently requires knowledge of the `AstNode` Protobuf schema. A visual policy authoring tool is on the roadmap but not yet available. In the interim, a YAML-to-AST compiler is provided to reduce the authoring barrier.

The AST evaluator is pure and stateless. It cannot make decisions based on external data sources (e.g., database lookups). Policies that require external data must pre-load that data into the state document before evaluation. This is a deliberate design constraint that preserves determinism and testability.

---

## Compliance Notes

This decision satisfies the following compliance requirements:

SOC 2 Type II requires evidence that access controls are evaluated consistently and that evaluation results are logged. The AST evaluator's deterministic output and the mandatory evaluation trace in every `EvalResult` directly satisfy this requirement.

GDPR Article 22 requires that automated decision-making systems provide meaningful information about the logic involved. The evaluation trace stored in the event log provides exactly this: a human-readable explanation of which conditions were evaluated and why an outcome was reached.
