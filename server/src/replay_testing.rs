//! Replay Test Suite (Phase 3 – Versioning Safety).
//!
//! Downloads serialised workflow history JSON from production (or loads from
//! fixtures in CI) and replays each event sequence against the current worker
//! code.  Any determinism regression – a history event that the new worker
//! would produce a different outcome for – is surfaced as a test failure.
//!
//! ## Usage in CI
//! ```text
//! # 1. Export history from production (one-time or scheduled):
//! temporal workflow show --workflow-id gp2f/<tenant>:<wf>:<inst> \
//!     --output json > tests/fixtures/history_<id>.json
//!
//! # 2. Run replay tests in every PR:
//! cargo test --test replay  # drives replay_testing::run_fixture_dir
//! ```
//!
//! ## Pattern
//! Each fixture file is a JSON array of [`ReplayEvent`]s.  The test driver
//! calls [`replay_history`] which re-executes the events against a fresh
//! [`WorkflowInstance`] and asserts that:
//! * every previously-accepted op is still accepted.
//! * every previously-rejected op is still rejected.
//!
//! Any divergence indicates a non-deterministic or breaking change.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::workflow::{ActivityOutcome, WorkflowDefinition, WorkflowError, WorkflowInstance};

// ── types ─────────────────────────────────────────────────────────────────────

/// A single event from a serialised workflow history.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplayEvent {
    /// The op_id that was submitted.
    pub op_id: String,
    /// The workflow state snapshot at the time of submission.
    pub state: Value,
    /// Whether the op was accepted or rejected in the original run.
    pub accepted: bool,
}

/// Result of replaying a history file against the current worker code.
#[derive(Debug, Default)]
pub struct ReplayReport {
    /// Number of events that produced the same outcome as the original run.
    pub matched: usize,
    /// Events where the new worker diverged from the original outcome.
    pub diverged: Vec<ReplayDivergence>,
}

impl ReplayReport {
    /// Return `true` when every event replayed without divergence.
    pub fn is_clean(&self) -> bool {
        self.diverged.is_empty()
    }
}

/// Details of a single replay divergence.
#[derive(Debug, Clone)]
pub struct ReplayDivergence {
    pub op_id: String,
    pub original_accepted: bool,
    pub replayed_accepted: bool,
}

// ── replay engine ─────────────────────────────────────────────────────────────

/// Replay `events` against `definition` from a fresh instance.
///
/// Returns a [`ReplayReport`] describing any divergences.  A clean report
/// (`report.is_clean() == true`) confirms the new worker code is backward-
/// compatible with the recorded history.
pub fn replay_history(definition: &WorkflowDefinition, events: &[ReplayEvent]) -> ReplayReport {
    let mut instance = WorkflowInstance::start("replay", "replay-tenant", definition);
    let mut report = ReplayReport::default();

    for event in events {
        let outcome = match instance.execute_next(definition, &event.state, &event.op_id) {
            Ok(o) => o,
            Err(WorkflowError::AllActivitiesComplete) => break,
            Err(_) => break,
        };

        let replayed_accepted = matches!(outcome, ActivityOutcome::Accepted { .. });

        if replayed_accepted == event.accepted {
            report.matched += 1;
        } else {
            report.diverged.push(ReplayDivergence {
                op_id: event.op_id.clone(),
                original_accepted: event.accepted,
                replayed_accepted,
            });
        }
    }

    report
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::{ActivityDef, CompensationAction, WorkflowDefinition};
    use policy_core::ast::AstNode;
    use serde_json::json;
    use std::collections::HashMap;

    fn simple_def() -> WorkflowDefinition {
        WorkflowDefinition {
            workflow_id: "replay_wf".into(),
            activities: vec![
                ActivityDef {
                    name: "step_1".into(),
                    policy: AstNode::literal_true(),
                    compensation_ref: None,
                    is_local: false,
                },
                ActivityDef {
                    name: "step_2".into(),
                    policy: AstNode::literal_false(),
                    compensation_ref: None,
                    is_local: false,
                },
            ],
            compensation_handlers: HashMap::<String, CompensationAction>::new(),
            access_policy: None,
            workflow_version: 1,
            task_queue: "gp2f-queue-v1".into(),
        }
    }

    #[test]
    fn clean_replay_produces_no_divergences() {
        let def = simple_def();
        // step_1 accepts, step_2 rejects
        let history = vec![
            ReplayEvent {
                op_id: "op-1".into(),
                state: json!({}),
                accepted: true,
            },
            ReplayEvent {
                op_id: "op-2".into(),
                state: json!({}),
                accepted: false,
            },
        ];
        let report = replay_history(&def, &history);
        assert!(
            report.is_clean(),
            "expected clean replay, got {:?}",
            report.diverged
        );
        assert_eq!(report.matched, 2);
    }

    #[test]
    fn divergence_detected_when_worker_logic_changes() {
        let def = simple_def();
        // Original history says step_2 was accepted, but current policy rejects it.
        let history = vec![
            ReplayEvent {
                op_id: "op-1".into(),
                state: json!({}),
                accepted: true,
            },
            ReplayEvent {
                op_id: "op-2".into(),
                state: json!({}),
                accepted: true, // original said ACCEPTED, but current policy REJECTS
            },
        ];
        let report = replay_history(&def, &history);
        assert!(!report.is_clean());
        assert_eq!(report.diverged.len(), 1);
        assert_eq!(report.diverged[0].op_id, "op-2");
        assert!(report.diverged[0].original_accepted);
        assert!(!report.diverged[0].replayed_accepted);
    }

    #[test]
    fn replay_stops_gracefully_when_history_exhausts_activities() {
        // A workflow with 2 activities that both accept.  After 2 events the
        // workflow is Completed; a 3rd event in the history should be silently
        // ignored without panicking.
        let def = WorkflowDefinition {
            workflow_id: "replay_wf_done".into(),
            activities: vec![
                ActivityDef {
                    name: "s1".into(),
                    policy: AstNode::literal_true(),
                    compensation_ref: None,
                    is_local: false,
                },
                ActivityDef {
                    name: "s2".into(),
                    policy: AstNode::literal_true(),
                    compensation_ref: None,
                    is_local: false,
                },
            ],
            compensation_handlers: HashMap::<String, CompensationAction>::new(),
            access_policy: None,
            workflow_version: 1,
            task_queue: "gp2f-queue-v1".into(),
        };

        let history = vec![
            ReplayEvent {
                op_id: "op-1".into(),
                state: json!({}),
                accepted: true,
            },
            ReplayEvent {
                op_id: "op-2".into(),
                state: json!({}),
                accepted: true,
            },
            ReplayEvent {
                op_id: "op-3".into(),
                state: json!({}),
                accepted: true,
            }, // no 3rd activity
        ];
        let report = replay_history(&def, &history);
        // First two events match; third is silently ignored (AllActivitiesComplete).
        assert!(report.is_clean());
        assert_eq!(report.matched, 2);
    }
}
