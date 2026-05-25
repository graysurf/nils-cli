//! Integration coverage for the lifecycle role registry (Task 2.1).
//!
//! Mirrors the unit tests in `lifecycle_vnext::registry::tests` so the plan
//! validation command
//! `cargo test -p nils-plan-issue-cli lifecycle_vnext_registry --no-fail-fast`
//! finds at least one matching test name regardless of how cargo filters
//! the inner module paths.
//!
//! Sources:
//!
//! - `docs/source/plan-issue-redesign/plan-tracking-issue-comment-taxonomy-v1.md`
//! - `docs/source/plan-issue-redesign/plan-tracking-issue-cli-redesign-v1.md`

use plan_issue_cli::lifecycle_record::PayloadRole;
use plan_issue_cli::lifecycle_vnext::registry::{
    self, DashboardRepair, DirectPostPolicy, PayloadSchema,
};

#[test]
fn lifecycle_vnext_registry_covers_every_payload_role() {
    let roles: Vec<PayloadRole> = registry::all_roles().iter().map(|spec| spec.role).collect();
    assert_eq!(
        roles,
        vec![
            PayloadRole::Source,
            PayloadRole::Plan,
            PayloadRole::State,
            PayloadRole::Session,
            PayloadRole::Validation,
            PayloadRole::Review,
            PayloadRole::Closeout,
        ]
    );
}

#[test]
fn lifecycle_vnext_registry_headings_match_taxonomy() {
    let expected: Vec<(PayloadRole, &str)> = vec![
        (PayloadRole::Source, "## Source Snapshot"),
        (PayloadRole::Plan, "## Plan Snapshot"),
        (PayloadRole::State, "## Execution State"),
        (PayloadRole::Session, "## Execution Session"),
        (PayloadRole::Validation, "## Validation Evidence"),
        (PayloadRole::Review, "## Review Evidence"),
        (PayloadRole::Closeout, "## Tracking Issue Closeout"),
    ];
    for (role_id, heading) in expected {
        assert_eq!(
            registry::role(role_id).default_heading,
            heading,
            "role {role_id:?} heading drift vs taxonomy doc"
        );
    }
}

#[test]
fn lifecycle_vnext_registry_state_requires_task_ledger() {
    let state = registry::role(PayloadRole::State);
    assert!(
        state
            .required_visible_sections
            .iter()
            .any(|h| *h == "## Task Ledger"),
        "state visible sections missing ## Task Ledger: {:?}",
        state.required_visible_sections
    );
}

#[test]
fn lifecycle_vnext_registry_source_and_plan_open_attach_only() {
    assert_eq!(
        registry::role(PayloadRole::Source).direct_post,
        DirectPostPolicy::OpenAttachOnly
    );
    assert_eq!(
        registry::role(PayloadRole::Plan).direct_post,
        DirectPostPolicy::OpenAttachOnly
    );
}

#[test]
fn lifecycle_vnext_registry_closeout_is_record_close_owned() {
    let closeout = registry::role(PayloadRole::Closeout);
    assert_eq!(closeout.direct_post, DirectPostPolicy::RecordCloseOwned);
    assert_eq!(closeout.dashboard_repair, DashboardRepair::OwnedByCloseout);
}

#[test]
fn lifecycle_vnext_registry_payload_schemas_declared_for_every_role() {
    let expected: Vec<(PayloadRole, PayloadSchema)> = vec![
        (PayloadRole::Source, PayloadSchema::Snapshot),
        (PayloadRole::Plan, PayloadSchema::Snapshot),
        (PayloadRole::State, PayloadSchema::State),
        (PayloadRole::Session, PayloadSchema::Session),
        (PayloadRole::Validation, PayloadSchema::Validation),
        (PayloadRole::Review, PayloadSchema::Review),
        (PayloadRole::Closeout, PayloadSchema::Closeout),
    ];
    for (role_id, schema) in expected {
        assert_eq!(
            registry::role(role_id).payload_schema,
            schema,
            "role {role_id:?} payload schema drift"
        );
    }
}

#[test]
fn lifecycle_vnext_registry_iteration_proves_visible_template_and_schema() {
    for spec in registry::all_roles() {
        assert!(
            !spec.required_visible_sections.is_empty(),
            "role {:?} missing visible template",
            spec.role
        );
        assert!(
            !spec.payload_schema.as_str().is_empty(),
            "role {:?} missing payload schema id",
            spec.role
        );
    }
}
