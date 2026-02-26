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
}

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
        }
    }

    /// Execute the next activity against `state`.
    ///
    /// Returns `Ok(true)` when the activity's AST policy allows the op,
    /// `Ok(false)` when the policy denies it, and `Err` when all activities
    /// have already been completed or the workflow is not in `Running` state.
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
                trace: eval_result.trace,
            })
        } else {
            Ok(ActivityOutcome::Rejected {
                activity_name: activity.name.clone(),
                trace: eval_result.trace,
            })
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
        trace: Vec<String>,
    },
    Rejected {
        activity_name: String,
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
                },
                ActivityDef {
                    name: "step_2".into(),
                    policy: AstNode::literal_true(),
                    compensation_ref: None,
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
        }
    }

    fn deny_all_def(workflow_id: &str) -> WorkflowDefinition {
        WorkflowDefinition {
            workflow_id: workflow_id.to_owned(),
            activities: vec![ActivityDef {
                name: "gated_step".into(),
                policy: AstNode::literal_false(),
                compensation_ref: None,
            }],
            compensation_handlers: HashMap::new(),
            access_policy: None,
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
                },
                ActivityDef {
                    name: "b".into(),
                    policy: AstNode::literal_true(),
                    compensation_ref: Some("undo_b".into()),
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
            }],
            compensation_handlers: HashMap::new(),
            access_policy: None,
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
}
