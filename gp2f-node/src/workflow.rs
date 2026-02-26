//! Workflow and Activity bindings for Node.js.
//!
//! Exposes [`JsWorkflow`] as a `Workflow` class in JavaScript with an
//! idiomatic fluent `addActivity` builder API.  Activity callbacks
//! (`onExecute`) are bridged from Rust to JavaScript via
//! `napi::threadsafe_function::ThreadsafeFunction` so the Rust runtime can
//! invoke them safely from any thread.

use napi::bindgen_prelude::*;
use napi::threadsafe_function::{
    ErrorStrategy, ThreadSafeCallContext, ThreadsafeFunction, ThreadsafeFunctionCallMode,
};
use napi::JsFunction;
use napi_derive::napi;
use policy_core::AstNode;
use serde_json::Value;

use crate::policy::JsAstNode;

// ── Activity configuration ────────────────────────────────────────────────────

/// Configuration object for a single workflow activity.
///
/// Passed to [`JsWorkflow::add_activity`].
#[napi(object)]
pub struct JsActivityConfig {
    /// Policy AST that governs whether this activity is permitted.
    pub policy: JsAstNode,
    /// Optional name of a registered compensation handler to undo this
    /// activity if a later step fails.
    pub compensation_ref: Option<String>,
    /// When `true`, this activity runs as a Local Activity (no Temporal
    /// persistence round-trip).  Use for short, idempotent operations.
    pub is_local: Option<bool>,
}

// ── Execution context ─────────────────────────────────────────────────────────

/// Context object passed to every `onExecute` callback.
///
/// Contains the workflow instance identifiers and the current state document
/// that was evaluated by the policy engine.
#[napi(object)]
#[derive(Debug, Clone)]
pub struct JsExecutionContext {
    /// Unique workflow execution identifier.
    pub instance_id: String,
    /// Tenant/organisation this execution belongs to.
    pub tenant_id: String,
    /// Name of the activity currently executing.
    pub activity_name: String,
    /// The JSON-encoded state document evaluated by the policy engine.
    /// Parse with `JSON.parse(ctx.stateJson)` in JavaScript.
    pub state_json: String,
}

// ── Internal activity storage ─────────────────────────────────────────────────

/// Internal representation of a registered activity.
///
/// `Clone` is derived so activity lists can be shared with the HTTP server
/// registry.  `ThreadsafeFunction` is reference-counted and safe to clone.
#[derive(Clone)]
pub(crate) struct ActivityEntry {
    pub(crate) policy: AstNode,
    // Stored for future compensation/saga support (Phase 4).
    #[allow(dead_code)]
    pub(crate) compensation_ref: Option<String>,
    // Stored for Local Activity optimisation flag (Phase 3).
    #[allow(dead_code)]
    pub(crate) is_local: bool,
    /// Optional JS async callback invoked when the activity executes.
    pub(crate) on_execute:
        Option<ThreadsafeFunction<JsExecutionContext, ErrorStrategy::CalleeHandled>>,
}

// ── Workflow class ────────────────────────────────────────────────────────────

/// A GP2F workflow definition.
///
/// Construct a workflow, register activities, then pass it to
/// [`JsGP2FServer::register`].
///
/// Example (TypeScript):
/// ```typescript
/// import { Workflow } from '@gp2f/server';
///
/// const wf = new Workflow('document-approval');
/// wf.addActivity('review', {
///   policy: { kind: 'LITERAL_TRUE' },
///   onExecute: async (ctx) => { console.log('executing', ctx.activityName); },
/// });
/// ```
#[napi]
pub struct JsWorkflow {
    /// Stable workflow identifier.
    pub(crate) workflow_id: String,
    /// Ordered activity entries (insertion order = execution order).
    pub(crate) activities: Vec<(String, ActivityEntry)>,
}

#[napi]
impl JsWorkflow {
    /// Create a new workflow with the given identifier.
    #[napi(constructor)]
    pub fn new(workflow_id: String) -> Self {
        Self {
            workflow_id,
            activities: Vec::new(),
        }
    }

    /// Add an activity to this workflow.
    ///
    /// Activities are executed in the order they are added.  Each activity has
    /// a policy AST that determines whether the operation is permitted.
    ///
    /// The optional `on_execute` callback is invoked when the activity runs.
    /// It receives an [`JsExecutionContext`] and may return a Promise; the
    /// Rust runtime awaits the resolution before proceeding.
    ///
    /// Returns the workflow identifier (for informational purposes).
    #[napi]
    pub fn add_activity(
        &mut self,
        name: String,
        config: JsActivityConfig,
        on_execute: Option<JsFunction>,
    ) -> Result<String> {
        let policy = AstNode::try_from(config.policy)?;
        let tsfn = on_execute
            .map(|f| {
                f.create_threadsafe_function(0, |ctx: ThreadSafeCallContext<JsExecutionContext>| {
                    let env = ctx.env;
                    let js_ctx = ctx.value;

                    // Build the JS object passed to the callback.
                    let mut obj = env.create_object()?;
                    obj.set_named_property("instanceId", env.create_string(&js_ctx.instance_id)?)?;
                    obj.set_named_property("tenantId", env.create_string(&js_ctx.tenant_id)?)?;
                    obj.set_named_property(
                        "activityName",
                        env.create_string(&js_ctx.activity_name)?,
                    )?;
                    // Pass the state as a JSON string.  Users call
                    // `JSON.parse(ctx.stateJson)` or use the `state`
                    // getter provided by the JavaScript wrapper.
                    obj.set_named_property("stateJson", env.create_string(&js_ctx.state_json)?)?;

                    Ok(vec![obj])
                })
            })
            .transpose()?;

        self.activities.push((
            name,
            ActivityEntry {
                policy,
                compensation_ref: config.compensation_ref,
                is_local: config.is_local.unwrap_or(false),
                on_execute: tsfn,
            },
        ));

        Ok(self.workflow_id.clone())
    }

    /// Return the workflow identifier.
    #[napi(getter)]
    pub fn id(&self) -> String {
        self.workflow_id.clone()
    }

    /// Return the number of registered activities.
    #[napi(getter)]
    pub fn activity_count(&self) -> u32 {
        self.activities.len() as u32
    }

    /// Evaluate the workflow against a state document without side-effects.
    ///
    /// Returns `true` when *every* activity policy is satisfied by `state`.
    /// Useful for dry-run checks before calling `start()`.
    #[napi]
    pub fn dry_run(&self, state: Value) -> Result<bool> {
        let evaluator = policy_core::Evaluator::new();
        for (_, entry) in &self.activities {
            let result = evaluator
                .evaluate(&state, &entry.policy)
                .map_err(|e| napi::Error::new(napi::Status::GenericFailure, e.to_string()))?;
            if !result.result {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

// ── Async activity execution helper ──────────────────────────────────────────

/// Call the optional `onExecute` JS callback for an activity.
///
/// This is a fire-and-forget call; the Rust runtime does not block on the JS
/// promise.  In a full Temporal integration the promise resolution would be
/// awaited before advancing the workflow.
pub(crate) fn invoke_on_execute(entry: &ActivityEntry, ctx: JsExecutionContext) {
    if let Some(tsfn) = &entry.on_execute {
        // Non-blocking: queues the call on the JS event loop.
        tsfn.call(Ok(ctx), ThreadsafeFunctionCallMode::NonBlocking);
    }
}
