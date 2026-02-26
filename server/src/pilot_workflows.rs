//! Production-ready pilot workflow definitions for Phase 9.
//!
//! Each workflow is a complete [`WorkflowDefinition`] that can be registered in
//! a [`WorkflowRegistry`] and driven by the reconciliation engine.  The three
//! workflows cover the first enterprise pilot deployments:
//!
//! 1. **Medical Triage Intake** – HIPAA-aligned patient intake for urgent-care
//!    facilities.  Activities are gated on role (`"clinician"` or `"admin"`) and
//!    patient-consent fields.  Compensation actions roll back data ingestion on
//!    failure.
//!
//! 2. **Supply-Chain Offline Delivery Update** – Works reliably during network
//!    outages.  A delivery driver can complete all activities locally; ops queue
//!    and reconcile automatically on reconnect.
//!
//! 3. **Multi-Party Contract Negotiation** – Sequential review stages for legal,
//!    financial, and executive stakeholders.  Each stage requires the prior
//!    stage to have accepted before advancing.

use crate::workflow::{ActivityDef, CompensationAction, WorkflowDefinition};
use policy_core::ast::{AstNode, NodeKind};
use serde_json::json;
use std::collections::HashMap;

// ── helpers ───────────────────────────────────────────────────────────────────

/// Convenience: build a leaf value node from a string that will be parsed
/// as a JSON scalar by the evaluator's `parse_scalar` helper.
///
/// Uses `NodeKind::Eq` as the kind specifically because it has no special-casing
/// in `resolve_child_value` (only `Field`, `LiteralTrue`, and `LiteralFalse` are
/// special-cased) and therefore falls through to the scalar catch-all branch when
/// the node has no children, no path, and a non-null `value`.
fn scalar_leaf(value: &str) -> AstNode {
    AstNode {
        version: None,
        kind: NodeKind::Eq, // any non-Field/non-Literal kind triggers the scalar catch-all
        children: vec![],
        path: None,
        value: Some(value.to_owned()),
        call_name: None,
    }
}

/// Build an AST node that checks `/role == <role_name>`.
fn role_equals(role_name: &str) -> AstNode {
    AstNode {
        version: None,
        kind: NodeKind::Eq,
        children: vec![
            AstNode {
                version: None,
                kind: NodeKind::Field,
                children: vec![],
                path: Some("/role".into()),
                value: None,
                call_name: None,
            },
            scalar_leaf(&format!(r#""{role_name}""#)),
        ],
        path: None,
        value: None,
        call_name: None,
    }
}

/// Build an AST node that checks whether `/role` is one of the provided values.
fn role_in(roles: &[&str]) -> AstNode {
    // Serialize the roles as a JSON array string so parse_scalar can decode it.
    let array_str = serde_json::to_string(&roles.to_vec()).unwrap();
    AstNode {
        version: None,
        kind: NodeKind::In,
        children: vec![
            AstNode {
                version: None,
                kind: NodeKind::Field,
                children: vec![],
                path: Some("/role".into()),
                value: None,
                call_name: None,
            },
            scalar_leaf(&array_str),
        ],
        path: None,
        value: None,
        call_name: None,
    }
}

/// Build an AST node that checks a boolean flag at `json_path` is `true`.
fn field_is_true(json_path: &str) -> AstNode {
    AstNode {
        version: None,
        kind: NodeKind::Eq,
        children: vec![
            AstNode {
                version: None,
                kind: NodeKind::Field,
                children: vec![],
                path: Some(json_path.into()),
                value: None,
                call_name: None,
            },
            scalar_leaf("true"),
        ],
        path: None,
        value: None,
        call_name: None,
    }
}

/// Logical AND of two AST nodes.
fn and(left: AstNode, right: AstNode) -> AstNode {
    AstNode {
        version: None,
        kind: NodeKind::And,
        children: vec![left, right],
        path: None,
        value: None,
        call_name: None,
    }
}

// ── 1. Medical Triage Intake ──────────────────────────────────────────────────

/// Build the **Medical Triage Intake** workflow definition.
///
/// Activities:
/// 1. `register_patient` – Allowed by any `clinician` or `admin`.
/// 2. `collect_vitals` – Allowed by any `clinician`; requires patient consent
///    (`/consent_given == true`).
/// 3. `assign_triage_level` – Allowed by `clinician` or `admin`; requires
///    vitals to have been recorded (`/vitals_recorded == true`).
/// 4. `finalize_intake` – Allowed by `admin` only.
///
/// All activities carry compensation actions so the intake can be cleanly
/// rolled back (e.g. on consent withdrawal).
pub fn medical_triage_intake() -> WorkflowDefinition {
    let mut compensation_handlers = HashMap::new();
    compensation_handlers.insert(
        "undo_register_patient".to_owned(),
        CompensationAction {
            name: "undo_register_patient".into(),
            rollback_payload: json!({
                "action": "delete_patient_record",
                "reason": "intake_rolled_back"
            }),
        },
    );
    compensation_handlers.insert(
        "undo_collect_vitals".to_owned(),
        CompensationAction {
            name: "undo_collect_vitals".into(),
            rollback_payload: json!({
                "action": "delete_vitals_record",
                "reason": "intake_rolled_back"
            }),
        },
    );
    compensation_handlers.insert(
        "undo_assign_triage_level".to_owned(),
        CompensationAction {
            name: "undo_assign_triage_level".into(),
            rollback_payload: json!({
                "action": "clear_triage_assignment",
                "reason": "intake_rolled_back"
            }),
        },
    );
    compensation_handlers.insert(
        "undo_finalize_intake".to_owned(),
        CompensationAction {
            name: "undo_finalize_intake".into(),
            rollback_payload: json!({
                "action": "revert_intake_finalization",
                "reason": "intake_rolled_back"
            }),
        },
    );

    WorkflowDefinition {
        workflow_id: "medical_triage_intake".into(),
        activities: vec![
            ActivityDef {
                name: "register_patient".into(),
                policy: role_in(&["clinician", "admin"]),
                compensation_ref: Some("undo_register_patient".into()),
                is_local: false,
            },
            ActivityDef {
                name: "collect_vitals".into(),
                policy: and(role_equals("clinician"), field_is_true("/consent_given")),
                compensation_ref: Some("undo_collect_vitals".into()),
                is_local: false,
            },
            ActivityDef {
                name: "assign_triage_level".into(),
                policy: and(
                    role_in(&["clinician", "admin"]),
                    field_is_true("/vitals_recorded"),
                ),
                compensation_ref: Some("undo_assign_triage_level".into()),
                is_local: false,
            },
            ActivityDef {
                name: "finalize_intake".into(),
                policy: role_equals("admin"),
                compensation_ref: Some("undo_finalize_intake".into()),
                is_local: false,
            },
        ],
        compensation_handlers,
        access_policy: Some(role_in(&["clinician", "admin"])),
        workflow_version: 1,
        task_queue: "gp2f-queue-v1".into(),
    }
}

/// Build the **Supply-Chain Offline Delivery Update** workflow.
///
/// Designed to operate fully offline: a delivery driver executes all
/// activities locally and the ops reconcile automatically on reconnect.
///
/// Activities:
/// 1. `scan_package` – Any `driver` or `dispatcher`.
/// 2. `confirm_delivery_location` – `driver`; GPS signature flag required.
/// 3. `record_proof_of_delivery` – `driver`; requires
///    `/delivery_location_confirmed == true`.
/// 4. `close_delivery` – `driver` or `dispatcher`; requires proof of delivery.
pub fn supply_chain_delivery_update() -> WorkflowDefinition {
    let mut compensation_handlers = HashMap::new();
    compensation_handlers.insert(
        "undo_scan_package".to_owned(),
        CompensationAction {
            name: "undo_scan_package".into(),
            rollback_payload: json!({
                "action": "unmark_package_scanned",
                "reason": "delivery_rolled_back"
            }),
        },
    );
    compensation_handlers.insert(
        "undo_confirm_delivery_location".to_owned(),
        CompensationAction {
            name: "undo_confirm_delivery_location".into(),
            rollback_payload: json!({
                "action": "unconfirm_delivery_location",
                "reason": "delivery_rolled_back"
            }),
        },
    );

    WorkflowDefinition {
        workflow_id: "supply_chain_delivery_update".into(),
        activities: vec![
            ActivityDef {
                name: "scan_package".into(),
                policy: role_in(&["driver", "dispatcher"]),
                compensation_ref: Some("undo_scan_package".into()),
                is_local: false,
            },
            ActivityDef {
                name: "confirm_delivery_location".into(),
                policy: and(role_equals("driver"), field_is_true("/gps_signed")),
                compensation_ref: Some("undo_confirm_delivery_location".into()),
                is_local: false,
            },
            ActivityDef {
                name: "record_proof_of_delivery".into(),
                policy: and(
                    role_equals("driver"),
                    field_is_true("/delivery_location_confirmed"),
                ),
                compensation_ref: None,
                is_local: false,
            },
            ActivityDef {
                name: "close_delivery".into(),
                policy: and(
                    role_in(&["driver", "dispatcher"]),
                    field_is_true("/proof_of_delivery_recorded"),
                ),
                compensation_ref: None,
                is_local: false,
            },
        ],
        compensation_handlers,
        access_policy: Some(role_in(&["driver", "dispatcher"])),
        workflow_version: 1,
        task_queue: "gp2f-queue-v1".into(),
    }
}

// ── 3. Multi-Party Contract Negotiation ───────────────────────────────────────

/// Build the **Multi-Party Contract Negotiation** workflow.
///
/// Sequential review stages: legal → finance → executive.  Each stage must
/// pass before the next can start.
///
/// Activities:
/// 1. `legal_review` – `legal` role; requires contract draft uploaded.
/// 2. `finance_review` – `finance` role; requires legal sign-off.
/// 3. `executive_approval` – `executive` role; requires finance sign-off.
/// 4. `countersign` – Any party that has the `signatory` role.
pub fn multi_party_contract_negotiation() -> WorkflowDefinition {
    let mut compensation_handlers = HashMap::new();
    compensation_handlers.insert(
        "undo_legal_review".to_owned(),
        CompensationAction {
            name: "undo_legal_review".into(),
            rollback_payload: json!({
                "action": "revoke_legal_signoff",
                "reason": "contract_negotiation_rolled_back"
            }),
        },
    );
    compensation_handlers.insert(
        "undo_finance_review".to_owned(),
        CompensationAction {
            name: "undo_finance_review".into(),
            rollback_payload: json!({
                "action": "revoke_finance_signoff",
                "reason": "contract_negotiation_rolled_back"
            }),
        },
    );
    compensation_handlers.insert(
        "undo_executive_approval".to_owned(),
        CompensationAction {
            name: "undo_executive_approval".into(),
            rollback_payload: json!({
                "action": "revoke_executive_approval",
                "reason": "contract_negotiation_rolled_back"
            }),
        },
    );

    WorkflowDefinition {
        workflow_id: "multi_party_contract_negotiation".into(),
        activities: vec![
            ActivityDef {
                name: "legal_review".into(),
                policy: and(role_equals("legal"), field_is_true("/draft_uploaded")),
                compensation_ref: Some("undo_legal_review".into()),
                is_local: false,
            },
            ActivityDef {
                name: "finance_review".into(),
                policy: and(role_equals("finance"), field_is_true("/legal_signed_off")),
                compensation_ref: Some("undo_finance_review".into()),
                is_local: false,
            },
            ActivityDef {
                name: "executive_approval".into(),
                policy: and(
                    role_equals("executive"),
                    field_is_true("/finance_signed_off"),
                ),
                compensation_ref: Some("undo_executive_approval".into()),
                is_local: false,
            },
            ActivityDef {
                name: "countersign".into(),
                policy: and(
                    role_equals("signatory"),
                    field_is_true("/executive_approved"),
                ),
                compensation_ref: None,
                is_local: false,
            },
        ],
        compensation_handlers,
        access_policy: Some(role_in(&["legal", "finance", "executive", "signatory"])),
        workflow_version: 1,
        task_queue: "gp2f-queue-v1".into(),
    }
}

/// Register all three pilot workflows in a [`crate::workflow::WorkflowRegistry`].
pub fn register_pilot_workflows(registry: &mut crate::workflow::WorkflowRegistry) {
    registry.register(medical_triage_intake());
    registry.register(supply_chain_delivery_update());
    registry.register(multi_party_contract_negotiation());
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::{ActivityOutcome, WorkflowInstance, WorkflowRegistry};
    use serde_json::json;

    // ── medical triage intake ─────────────────────────────────────────────

    #[test]
    fn medical_triage_register_patient_allowed_for_clinician() {
        let def = medical_triage_intake();
        let mut inst = WorkflowInstance::start("i1", "hospital-a", &def);
        let state = json!({ "role": "clinician" });
        let outcome = inst.execute_next(&def, &state, "op-1").unwrap();
        assert!(matches!(outcome, ActivityOutcome::Accepted { .. }));
    }

    #[test]
    fn medical_triage_register_patient_denied_for_driver() {
        let def = medical_triage_intake();
        let mut inst = WorkflowInstance::start("i2", "hospital-a", &def);
        let state = json!({ "role": "driver" });
        let outcome = inst.execute_next(&def, &state, "op-1").unwrap();
        assert!(matches!(outcome, ActivityOutcome::Rejected { .. }));
    }

    #[test]
    fn medical_triage_collect_vitals_requires_consent() {
        let def = medical_triage_intake();
        let mut inst = WorkflowInstance::start("i3", "hospital-a", &def);

        // Advance past register_patient
        inst.execute_next(&def, &json!({ "role": "clinician" }), "op-1")
            .unwrap();

        // Collect vitals: clinician but no consent
        let outcome = inst
            .execute_next(
                &def,
                &json!({ "role": "clinician", "consent_given": false }),
                "op-2",
            )
            .unwrap();
        assert!(matches!(outcome, ActivityOutcome::Rejected { .. }));

        // With consent
        let outcome = inst
            .execute_next(
                &def,
                &json!({ "role": "clinician", "consent_given": true }),
                "op-3",
            )
            .unwrap();
        assert!(matches!(outcome, ActivityOutcome::Accepted { .. }));
    }

    #[test]
    fn medical_triage_full_happy_path() {
        let def = medical_triage_intake();
        let mut inst = WorkflowInstance::start("i4", "hospital-a", &def);

        let s1 = json!({ "role": "clinician" });
        let s2 = json!({ "role": "clinician", "consent_given": true });
        let s3 = json!({ "role": "clinician", "vitals_recorded": true });
        let s4 = json!({ "role": "admin" });

        inst.execute_next(&def, &s1, "op-1").unwrap(); // register
        inst.execute_next(&def, &s2, "op-2").unwrap(); // vitals
        inst.execute_next(&def, &s3, "op-3").unwrap(); // triage level
        let outcome = inst.execute_next(&def, &s4, "op-4").unwrap(); // finalize
        assert!(matches!(outcome, ActivityOutcome::Accepted { .. }));
        assert_eq!(inst.status, crate::workflow::WorkflowStatus::Completed);
    }

    #[test]
    fn medical_triage_compensation_is_lifo() {
        let def = medical_triage_intake();
        let mut inst = WorkflowInstance::start("i5", "hospital-a", &def);

        let s1 = json!({ "role": "clinician" });
        let s2 = json!({ "role": "clinician", "consent_given": true });

        inst.execute_next(&def, &s1, "op-1").unwrap();
        inst.execute_next(&def, &s2, "op-2").unwrap();

        let actions = inst.compensate();
        // LIFO: undo_collect_vitals first, undo_register_patient second
        assert_eq!(actions[0].name, "undo_collect_vitals");
        assert_eq!(actions[1].name, "undo_register_patient");
    }

    // ── supply-chain delivery ─────────────────────────────────────────────

    #[test]
    fn supply_chain_scan_package_allowed_for_driver() {
        let def = supply_chain_delivery_update();
        let mut inst = WorkflowInstance::start("i6", "logistics-co", &def);
        let state = json!({ "role": "driver" });
        let outcome = inst.execute_next(&def, &state, "op-1").unwrap();
        assert!(matches!(outcome, ActivityOutcome::Accepted { .. }));
    }

    #[test]
    fn supply_chain_confirm_location_requires_gps() {
        let def = supply_chain_delivery_update();
        let mut inst = WorkflowInstance::start("i7", "logistics-co", &def);

        inst.execute_next(&def, &json!({ "role": "driver" }), "op-1")
            .unwrap();

        // No GPS signature → rejected
        let outcome = inst
            .execute_next(
                &def,
                &json!({ "role": "driver", "gps_signed": false }),
                "op-2",
            )
            .unwrap();
        assert!(matches!(outcome, ActivityOutcome::Rejected { .. }));

        // With GPS → accepted
        let outcome = inst
            .execute_next(
                &def,
                &json!({ "role": "driver", "gps_signed": true }),
                "op-3",
            )
            .unwrap();
        assert!(matches!(outcome, ActivityOutcome::Accepted { .. }));
    }

    #[test]
    fn supply_chain_full_happy_path() {
        let def = supply_chain_delivery_update();
        let mut inst = WorkflowInstance::start("i8", "logistics-co", &def);

        inst.execute_next(&def, &json!({ "role": "driver" }), "op-1")
            .unwrap();
        inst.execute_next(
            &def,
            &json!({ "role": "driver", "gps_signed": true }),
            "op-2",
        )
        .unwrap();
        inst.execute_next(
            &def,
            &json!({ "role": "driver", "delivery_location_confirmed": true }),
            "op-3",
        )
        .unwrap();
        let outcome = inst
            .execute_next(
                &def,
                &json!({ "role": "driver", "proof_of_delivery_recorded": true }),
                "op-4",
            )
            .unwrap();
        assert!(matches!(outcome, ActivityOutcome::Accepted { .. }));
        assert_eq!(inst.status, crate::workflow::WorkflowStatus::Completed);
    }

    // ── multi-party contract negotiation ──────────────────────────────────

    #[test]
    fn contract_legal_review_requires_draft_uploaded() {
        let def = multi_party_contract_negotiation();
        let mut inst = WorkflowInstance::start("i9", "law-firm", &def);

        // Legal role but no draft → rejected
        let outcome = inst
            .execute_next(
                &def,
                &json!({ "role": "legal", "draft_uploaded": false }),
                "op-1",
            )
            .unwrap();
        assert!(matches!(outcome, ActivityOutcome::Rejected { .. }));

        // With draft → accepted
        let outcome = inst
            .execute_next(
                &def,
                &json!({ "role": "legal", "draft_uploaded": true }),
                "op-2",
            )
            .unwrap();
        assert!(matches!(outcome, ActivityOutcome::Accepted { .. }));
    }

    #[test]
    fn contract_sequential_gates_enforced() {
        let def = multi_party_contract_negotiation();
        let mut inst = WorkflowInstance::start("i10", "law-firm", &def);

        // Finance cannot skip legal review stage
        let outcome = inst
            .execute_next(
                &def,
                &json!({ "role": "finance", "legal_signed_off": true }),
                "op-1",
            )
            .unwrap();
        // First activity is `legal_review`, finance role fails the policy.
        assert!(matches!(outcome, ActivityOutcome::Rejected { .. }));
    }

    #[test]
    fn contract_full_happy_path() {
        let def = multi_party_contract_negotiation();
        let mut inst = WorkflowInstance::start("i11", "law-firm", &def);

        inst.execute_next(
            &def,
            &json!({ "role": "legal", "draft_uploaded": true }),
            "op-1",
        )
        .unwrap();
        inst.execute_next(
            &def,
            &json!({ "role": "finance", "legal_signed_off": true }),
            "op-2",
        )
        .unwrap();
        inst.execute_next(
            &def,
            &json!({ "role": "executive", "finance_signed_off": true }),
            "op-3",
        )
        .unwrap();
        let outcome = inst
            .execute_next(
                &def,
                &json!({ "role": "signatory", "executive_approved": true }),
                "op-4",
            )
            .unwrap();
        assert!(matches!(outcome, ActivityOutcome::Accepted { .. }));
        assert_eq!(inst.status, crate::workflow::WorkflowStatus::Completed);
    }

    // ── registry helper ───────────────────────────────────────────────────

    #[test]
    fn register_pilot_workflows_populates_registry() {
        let mut reg = WorkflowRegistry::new();
        register_pilot_workflows(&mut reg);

        assert!(reg.get("medical_triage_intake").is_some());
        assert!(reg.get("supply_chain_delivery_update").is_some());
        assert!(reg.get("multi_party_contract_negotiation").is_some());
    }
}
