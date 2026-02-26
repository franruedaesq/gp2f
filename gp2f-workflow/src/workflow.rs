//! Temporal-style workflow orchestration model.
//!
//! Every high-stakes workflow is modeled as a [`WorkflowDefinition`] that
//! contains ordered [`ActivityDef`]s.  The AST Policy Engine is the decision
//! point inside each activity.  When an op is *accepted*, any compensation
//! saga defined for that activity is automatically registered so the workflow
//! can be cleanly rolled back if a later activity fails.
//!
//! ## Lifecycle
//! ```text
//! WorkflowDefinition   →  WorkflowInstance::start()
//!       │                        │
//!       ▼                        ▼
//!  ActivityDef      →  execute_activity() → AST eval → ACCEPT/REJECT
//!       │                        │
//!       └──CompensationAction ←──┘  (registered on ACCEPT)
//!                                │
//!                          compensate()  ← called on failure / explicit cancel
//! ```

use policy_core::{AstNode, Evaluator};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

// ── activity definition ───────────────────────────────────────────────────────

/// A single step in a workflow.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityDef {
    /// Unique name within the workflow (e.g. `"verify_identity"`).
    pub name: String,
    /// AST policy evaluated to decide whether this activity is permitted.
    pub policy: AstNode,
    /// Optional reference to a [`CompensationAction`] in the parent
    /// workflow's `compensation_handlers` map.  Registered automatically on
    /// ACCEPT.
    pub compensation_ref: Option<String>,
    /// When `true` the activity is a *Local Activity*: it executes in the
    /// worker process without a Temporal server round-trip for persistence.
    /// Use for short-lived, low-risk operations where the extra latency of a
    /// full history event is not acceptable (see Phase 2.1 of the operational
    /// roadmap).  Local activities are NOT replayed by Temporal on worker
    /// restart, so they must be idempotent.
    #[serde(default)]
    pub is_local: bool,
}

// ── compensation ──────────────────────────────────────────────────────────────

/// A rollback action that undoes an accepted activity.
///
/// In a full Temporal deployment this would become a *compensation workflow*.
/// Here we store the rollback payload so the host application can apply it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompensationAction {
    /// Name matching the key in [`WorkflowDefinition::compensation_handlers`].
    pub name: String,
    /// JSON payload the host applies to undo the corresponding activity.
    pub rollback_payload: Value,
}

// ── workflow definition ───────────────────────────────────────────────────────

/// Immutable blueprint describing all activities and their compensations.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowDefinition {
    /// Stable identifier (e.g. `"medical_intake"`, `"contract_signing"`).
    pub workflow_id: String,
    /// Ordered list of activities in execution order.
    pub activities: Vec<ActivityDef>,
    /// Map from compensation name → [`CompensationAction`].
    pub compensation_handlers: HashMap<String, CompensationAction>,
    /// Optional AST policy that must pass for *any* access to this workflow.
    /// Evaluated against the caller's RBAC context before activity start.
    pub access_policy: Option<AstNode>,
    /// Monotonically increasing version number for this workflow definition.
    /// New workers increment this when logic changes; old workers continue
    /// draining the previous version's task queue (see Phase 3.2 Worker
    /// Versioning).
    #[serde(default = "default_workflow_version")]
    pub workflow_version: u32,
    /// Temporal task queue this workflow is assigned to.
    /// Use versioned queue names (e.g. `"gp2f-queue-v2"`) when deploying
    /// breaking changes so old and new workers can drain independently.
    #[serde(default = "default_task_queue")]
    pub task_queue: String,
}

fn default_workflow_version() -> u32 {
    1
}

fn default_task_queue() -> String {
    DEFAULT_TASK_QUEUE.into()
}

/// Default Temporal task queue name.  Override per-workflow when deploying
/// breaking changes (e.g. `"gp2f-queue-v2"`).
pub const DEFAULT_TASK_QUEUE: &str = "gp2f-queue-v1";

// ── workflow instance ─────────────────────────────────────────────────────────

/// Mutable runtime state of a single workflow execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowInstance {
    pub instance_id: String,
    pub tenant_id: String,
    pub workflow_id: String,
    /// Current phase of the execution.
    pub status: WorkflowStatus,
    /// Index of the next activity to execute.
    pub next_activity: usize,
    /// op_ids for all *accepted* activities, in order (used for audit/replay).
    pub accepted_op_ids: Vec<String>,
    /// Compensation actions pending execution (LIFO order for rollback).
    pub pending_compensations: Vec<CompensationAction>,
    /// Set of patch names that have been applied to this instance via
    /// [`WorkflowInstance::patched`].  Guards against double-application of
    /// the same patch (Phase 3.1 – Patching API).
    #[serde(default)]
    pub applied_patches: Vec<String>,
}

/// Phase of a workflow instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum WorkflowStatus {
    Running,
    Completed,
    Compensating,
    Failed,
}

impl WorkflowInstance {
    /// Start a new instance from its definition.
    pub fn start(
        instance_id: impl Into<String>,
        tenant_id: impl Into<String>,
        definition: &WorkflowDefinition,
    ) -> Self {
        Self {
            instance_id: instance_id.into(),
            tenant_id: tenant_id.into(),
            workflow_id: definition.workflow_id.clone(),
            status: WorkflowStatus::Running,
            next_activity: 0,
            accepted_op_ids: Vec::new(),
            pending_compensations: Vec::new(),
            applied_patches: Vec::new(),
        }
    }

    /// Execute the next activity against `state`.
    ///
    /// Returns `Ok(true)` when the activity's AST policy allows the op,
    /// `Ok(false)` when the policy denies it, and `Err` when all activities
    /// have already been completed or the workflow is not in `Running` state.
    ///
    /// When the activity's `is_local` flag is set the outcome is identical but
    /// the caller is responsible for skipping the Temporal persistence
    /// round-trip (Phase 2.1 – Local Activities).
    pub fn execute_next(
        &mut self,
        definition: &WorkflowDefinition,
        state: &Value,
        op_id: &str,
    ) -> Result<ActivityOutcome, WorkflowError> {
        if self.status != WorkflowStatus::Running {
            return Err(WorkflowError::NotRunning(self.status.clone()));
        }

        let activity = definition
            .activities
            .get(self.next_activity)
            .ok_or(WorkflowError::AllActivitiesComplete)?;

        let eval_result = Evaluator::new()
            .evaluate(state, &activity.policy)
            .map_err(|e| WorkflowError::PolicyError(e.to_string()))?;

        if eval_result.result {
            // Op accepted: register compensation if one is defined.
            if let Some(ref comp_ref) = activity.compensation_ref {
                if let Some(comp) = definition.compensation_handlers.get(comp_ref) {
                    self.pending_compensations.push(comp.clone());
                }
            }
            self.accepted_op_ids.push(op_id.to_owned());
            self.next_activity += 1;

            // Mark completed when all activities are done.
            if self.next_activity >= definition.activities.len() {
                self.status = WorkflowStatus::Completed;
            }

            Ok(ActivityOutcome::Accepted {
                activity_name: activity.name.clone(),
                is_local: activity.is_local,
                trace: eval_result.trace,
            })
        } else {
            Ok(ActivityOutcome::Rejected {
                activity_name: activity.name.clone(),
                is_local: activity.is_local,
                trace: eval_result.trace,
            })
        }
    }

    /// Apply a named patch to this workflow instance (Phase 3.1 – Patching API).
    ///
    /// Returns `true` when the instance was **not** already running under the
    /// patched code path, so callers can execute new-path logic.  Returns
    /// `false` when the patch has already been applied (replay of old history).
    ///
    /// This mirrors Temporal's `workflow.patched()` / `workflow.get_version()`
    /// semantics: the first time a live execution reaches this call, the patch
    /// name is recorded in `applied_patches`.  During replay, the SDK detects
    /// that the patch marker is absent in the old history and returns `false`,
    /// so the worker can safely execute old-path logic for in-flight workflows
    /// while routing new executions through the new path.
    pub fn patched(&mut self, patch_name: &str) -> bool {
        if self.applied_patches.iter().any(|p| p == patch_name) {
            // Patch already applied – return false so callers use old-path logic
            // during replay.
            false
        } else {
            self.applied_patches.push(patch_name.to_owned());
            true
        }
    }

    /// Trigger compensating transactions in reverse order (LIFO).
    ///
    /// Returns the list of compensation payloads that should be applied.
    pub fn compensate(&mut self) -> Vec<CompensationAction> {
        self.status = WorkflowStatus::Compensating;
        // LIFO: take the last registered compensation first.
        let mut actions = self.pending_compensations.clone();
        actions.reverse();
        self.pending_compensations.clear();
        self.status = WorkflowStatus::Failed;
        actions
    }
}

// ── outcome types ─────────────────────────────────────────────────────────────

/// Result of executing one activity.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ActivityOutcome {
    Accepted {
        activity_name: String,
        /// `true` when the activity ran as a Local Activity (no Temporal
        /// persistence round-trip).  The caller should skip the Temporal
        /// signal and handle durability itself.
        is_local: bool,
        trace: Vec<String>,
    },
    Rejected {
        activity_name: String,
        /// `true` when the activity was evaluated as a Local Activity.
        is_local: bool,
        trace: Vec<String>,
    },
}

/// Errors that can occur during workflow execution.
#[derive(Debug, thiserror::Error)]
pub enum WorkflowError {
    #[error("workflow is not in Running state (current: {0:?})")]
    NotRunning(WorkflowStatus),
    #[error("all activities have already been completed")]
    AllActivitiesComplete,
    #[error("policy evaluation error: {0}")]
    PolicyError(String),
}

// ── in-memory registry ────────────────────────────────────────────────────────

/// In-memory registry of workflow definitions.
///
/// In production replace the inner `HashMap` with a database-backed store.
#[derive(Debug, Default)]
pub struct WorkflowRegistry {
    definitions: HashMap<String, WorkflowDefinition>,
}

impl WorkflowRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a workflow definition.
    pub fn register(&mut self, def: WorkflowDefinition) {
        self.definitions.insert(def.workflow_id.clone(), def);
    }

    /// Look up a workflow definition by ID.
    pub fn get(&self, workflow_id: &str) -> Option<&WorkflowDefinition> {
        self.definitions.get(workflow_id)
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use policy_core::ast::AstNode;
    use serde_json::json;

    fn allow_all_def(workflow_id: &str) -> WorkflowDefinition {
        WorkflowDefinition {
            workflow_id: workflow_id.to_owned(),
            activities: vec![
                ActivityDef {
                    name: "step_1".into(),
                    policy: AstNode::literal_true(),
                    compensation_ref: Some("undo_step_1".into()),
                    is_local: false,
                },
                ActivityDef {
                    name: "step_2".into(),
                    policy: AstNode::literal_true(),
                    compensation_ref: None,
                    is_local: false,
                },
            ],
            compensation_handlers: {
                let mut m = HashMap::new();
                m.insert(
                    "undo_step_1".to_owned(),
                    CompensationAction {
                        name: "undo_step_1".into(),
                        rollback_payload: json!({"action": "rollback_step_1"}),
                    },
                );
                m
            },
            access_policy: None,
            workflow_version: 1,
            task_queue: DEFAULT_TASK_QUEUE.into(),
        }
    }

    fn deny_all_def(workflow_id: &str) -> WorkflowDefinition {
        WorkflowDefinition {
            workflow_id: workflow_id.to_owned(),
            activities: vec![ActivityDef {
                name: "gated_step".into(),
                policy: AstNode::literal_false(),
                compensation_ref: None,
                is_local: false,
            }],
            compensation_handlers: HashMap::new(),
            access_policy: None,
            workflow_version: 1,
            task_queue: DEFAULT_TASK_QUEUE.into(),
        }
    }

    #[test]
    fn two_activities_complete_successfully() {
        let def = allow_all_def("wf1");
        let mut inst = WorkflowInstance::start("i1", "t1", &def);

        let outcome1 = inst.execute_next(&def, &json!({}), "op-1").unwrap();
        assert!(matches!(outcome1, ActivityOutcome::Accepted { .. }));
        assert_eq!(inst.status, WorkflowStatus::Running);

        let outcome2 = inst.execute_next(&def, &json!({}), "op-2").unwrap();
        assert!(matches!(outcome2, ActivityOutcome::Accepted { .. }));
        assert_eq!(inst.status, WorkflowStatus::Completed);
    }

    #[test]
    fn denied_activity_does_not_advance() {
        let def = deny_all_def("wf2");
        let mut inst = WorkflowInstance::start("i2", "t1", &def);

        let outcome = inst.execute_next(&def, &json!({}), "op-1").unwrap();
        assert!(matches!(outcome, ActivityOutcome::Rejected { .. }));
        // Instance stays Running and activity index is unchanged.
        assert_eq!(inst.status, WorkflowStatus::Running);
        assert_eq!(inst.next_activity, 0);
    }

    #[test]
    fn compensation_registered_on_accept() {
        let def = allow_all_def("wf3");
        let mut inst = WorkflowInstance::start("i3", "t1", &def);

        // step_1 has a compensation_ref
        inst.execute_next(&def, &json!({}), "op-1").unwrap();
        assert_eq!(inst.pending_compensations.len(), 1);
        assert_eq!(inst.pending_compensations[0].name, "undo_step_1");
    }

    #[test]
    fn compensation_is_lifo() {
        // Build a definition where both steps have compensations.
        let def = WorkflowDefinition {
            workflow_id: "wf_lifo".into(),
            activities: vec![
                ActivityDef {
                    name: "a".into(),
                    policy: AstNode::literal_true(),
                    compensation_ref: Some("undo_a".into()),
                    is_local: false,
                },
                ActivityDef {
                    name: "b".into(),
                    policy: AstNode::literal_true(),
                    compensation_ref: Some("undo_b".into()),
                    is_local: false,
                },
            ],
            compensation_handlers: {
                let mut m = HashMap::new();
                m.insert(
                    "undo_a".into(),
                    CompensationAction {
                        name: "undo_a".into(),
                        rollback_payload: json!({}),
                    },
                );
                m.insert(
                    "undo_b".into(),
                    CompensationAction {
                        name: "undo_b".into(),
                        rollback_payload: json!({}),
                    },
                );
                m
            },
            access_policy: None,
            workflow_version: 1,
            task_queue: DEFAULT_TASK_QUEUE.into(),
        };

        let mut inst = WorkflowInstance::start("i_lifo", "t1", &def);
        inst.execute_next(&def, &json!({}), "op-1").unwrap();
        inst.execute_next(&def, &json!({}), "op-2").unwrap();

        let actions = inst.compensate();
        assert_eq!(actions.len(), 2);
        // LIFO: undo_b first, undo_a second
        assert_eq!(actions[0].name, "undo_b");
        assert_eq!(actions[1].name, "undo_a");
    }

    #[test]
    fn execute_on_completed_workflow_is_error() {
        let def = WorkflowDefinition {
            workflow_id: "wf_single".into(),
            activities: vec![ActivityDef {
                name: "only_step".into(),
                policy: AstNode::literal_true(),
                compensation_ref: None,
                is_local: false,
            }],
            compensation_handlers: HashMap::new(),
            access_policy: None,
            workflow_version: 1,
            task_queue: DEFAULT_TASK_QUEUE.into(),
        };
        let mut inst = WorkflowInstance::start("i_done", "t1", &def);
        inst.execute_next(&def, &json!({}), "op-1").unwrap();
        assert_eq!(inst.status, WorkflowStatus::Completed);

        let err = inst.execute_next(&def, &json!({}), "op-2").unwrap_err();
        assert!(matches!(err, WorkflowError::NotRunning(_)));
    }

    #[test]
    fn workflow_registry_stores_and_retrieves() {
        let mut reg = WorkflowRegistry::new();
        reg.register(allow_all_def("my_workflow"));
        assert!(reg.get("my_workflow").is_some());
        assert!(reg.get("nonexistent").is_none());
    }

    // ── Phase 2.1: Local Activities ───────────────────────────────────────

    #[test]
    fn local_activity_outcome_carries_flag() {
        let def = WorkflowDefinition {
            workflow_id: "wf_local".into(),
            activities: vec![ActivityDef {
                name: "fast_check".into(),
                policy: AstNode::literal_true(),
                compensation_ref: None,
                is_local: true,
            }],
            compensation_handlers: HashMap::new(),
            access_policy: None,
            workflow_version: 1,
            task_queue: DEFAULT_TASK_QUEUE.into(),
        };

        let mut inst = WorkflowInstance::start("il1", "t1", &def);
        let outcome = inst.execute_next(&def, &json!({}), "op-local").unwrap();
        match outcome {
            ActivityOutcome::Accepted { is_local, .. } => assert!(is_local),
            ActivityOutcome::Rejected { .. } => panic!("expected Accepted"),
        }
    }

    // ── Phase 3.1: Patching API ───────────────────────────────────────────

    #[test]
    fn patched_returns_true_first_call_false_on_replay() {
        let def = allow_all_def("wf_patch");
        let mut inst = WorkflowInstance::start("ip1", "t1", &def);

        // First call: new-path logic should execute.
        assert!(inst.patched("add-extra-validation-v2"));
        // Second call with same name: replay of old history – use old path.
        assert!(!inst.patched("add-extra-validation-v2"));
    }

    #[test]
    fn different_patch_names_are_independent() {
        let def = allow_all_def("wf_patch2");
        let mut inst = WorkflowInstance::start("ip2", "t1", &def);

        assert!(inst.patched("patch-a"));
        assert!(inst.patched("patch-b")); // different name → first time
        assert!(!inst.patched("patch-a")); // already applied
    }

    // ── Phase 3.2: Worker Versioning ─────────────────────────────────────

    #[test]
    fn workflow_definition_default_version_and_queue() {
        let def = allow_all_def("wf_ver");
        assert_eq!(def.workflow_version, 1);
        assert_eq!(def.task_queue, "gp2f-queue-v1");
    }

    #[test]
    fn workflow_definition_custom_version_and_queue() {
        let def = WorkflowDefinition {
            workflow_id: "wf_v2".into(),
            activities: vec![],
            compensation_handlers: HashMap::new(),
            access_policy: None,
            workflow_version: 2,
            task_queue: "gp2f-queue-v2".into(),
        };
        assert_eq!(def.workflow_version, 2);
        assert_eq!(def.task_queue, "gp2f-queue-v2");
    }
}
