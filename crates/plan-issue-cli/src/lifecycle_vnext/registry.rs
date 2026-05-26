//! Lifecycle role registry.
//!
//! Single source of truth for the seven lifecycle comment roles defined in
//! `docs/source/plan-issue-redesign/plan-tracking-issue-comment-taxonomy-v1.md`:
//!
//! 1. `source` — durable plan source snapshot (open/attach only)
//! 2. `plan` — durable plan snapshot (open/attach only)
//! 3. `state` — execution-state checkpoint, requires visible Task Ledger
//! 4. `session` — execution session summary, requires non-empty summary
//! 5. `validation` — validation evidence, requires overall + commands/waiver
//! 6. `review` — review evidence, requires decision and finding disposition
//! 7. `closeout` — final closeout, owned by `record close`
//!
//! Each [`RoleSpec`] carries the metadata needed to drive
//! [`super::templates`], [`super::visible_lint`], [`super::render`], and the
//! `tracking::checkpoint` macro from one structured table.

use crate::lifecycle_record::PayloadRole;

/// Heading text used in the visible body for a lifecycle role. The headings
/// match `docs/source/plan-issue-redesign/plan-tracking-issue-comment-taxonomy-v1.md`.
pub const HEADING_SOURCE: &str = "## Source Snapshot";
pub const HEADING_PLAN: &str = "## Plan Snapshot";
pub const HEADING_STATE: &str = "## Execution State";
pub const HEADING_SESSION: &str = "## Execution Session";
pub const HEADING_VALIDATION: &str = "## Validation Evidence";
pub const HEADING_REVIEW: &str = "## Review Evidence";
pub const HEADING_CLOSEOUT: &str = "## Tracking Issue Closeout";

/// Required visible sections expressed as canonical Markdown headings. The
/// visible-completeness lint asserts that each heading appears verbatim in
/// the rendered comment body.
pub type VisibleSections = &'static [&'static str];

/// Direct-post permission for a lifecycle role.
///
/// `record post --kind <role>` accepts roles where this is
/// [`DirectPostPolicy::Allowed`]. Roles marked
/// [`DirectPostPolicy::OpenAttachOnly`] are owned by `record open` /
/// `record attach`. Roles marked [`DirectPostPolicy::RecordCloseOwned`] are
/// owned by `record close`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectPostPolicy {
    /// Skills may post directly via `record post --kind <role>`.
    Allowed,
    /// Posted only when `record open` or `record attach` snapshots the plan.
    OpenAttachOnly,
    /// Posted only when `record close` finishes the strict closeout gate.
    RecordCloseOwned,
}

impl DirectPostPolicy {
    pub fn allows_direct_post(self) -> bool {
        matches!(self, Self::Allowed)
    }
}

/// Dashboard repair expectation following a successful post for the role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DashboardRepair {
    /// Caller should repair the dashboard body after the post.
    Expected,
    /// Dashboard repair is optional for this role.
    Optional,
    /// Dashboard repair is owned by `record close` only.
    OwnedByCloseout,
}

/// Closeout ownership for a lifecycle role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseoutOwnership {
    /// Role appears before closeout and is not the closeout post.
    PreCloseout,
    /// Role is the closeout post itself.
    Closeout,
}

/// Structured payload schema declared for a lifecycle role. The strings are
/// machine-stable identifiers used by `record template --format json` and
/// the audit surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayloadSchema {
    Snapshot,
    State,
    Session,
    Validation,
    Review,
    Closeout,
}

impl PayloadSchema {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Snapshot => "snapshot.v1",
            Self::State => "state.v1",
            Self::Session => "session.v1",
            Self::Validation => "validation.v1",
            Self::Review => "review.v1",
            Self::Closeout => "closeout.v1",
        }
    }
}

/// Complete lifecycle role specification.
#[derive(Debug, Clone, Copy)]
pub struct RoleSpec {
    pub role: PayloadRole,
    pub marker_role: &'static str,
    pub default_heading: &'static str,
    pub required_visible_sections: VisibleSections,
    pub payload_schema: PayloadSchema,
    pub direct_post: DirectPostPolicy,
    pub dashboard_repair: DashboardRepair,
    pub closeout_ownership: CloseoutOwnership,
}

impl RoleSpec {
    pub fn allows_direct_post(&self) -> bool {
        self.direct_post.allows_direct_post()
    }
}

const SOURCE_REQUIRED: VisibleSections = &[HEADING_SOURCE];
const PLAN_REQUIRED: VisibleSections = &[HEADING_PLAN];
const STATE_REQUIRED: VisibleSections = &[HEADING_STATE, "## Task Ledger"];
const SESSION_REQUIRED: VisibleSections = &[HEADING_SESSION];
const VALIDATION_REQUIRED: VisibleSections = &[HEADING_VALIDATION];
const REVIEW_REQUIRED: VisibleSections = &[HEADING_REVIEW];
const CLOSEOUT_REQUIRED: VisibleSections = &[HEADING_CLOSEOUT];

const REGISTRY: &[RoleSpec] = &[
    RoleSpec {
        role: PayloadRole::Source,
        marker_role: "source",
        default_heading: HEADING_SOURCE,
        required_visible_sections: SOURCE_REQUIRED,
        payload_schema: PayloadSchema::Snapshot,
        direct_post: DirectPostPolicy::OpenAttachOnly,
        dashboard_repair: DashboardRepair::Optional,
        closeout_ownership: CloseoutOwnership::PreCloseout,
    },
    RoleSpec {
        role: PayloadRole::Plan,
        marker_role: "plan",
        default_heading: HEADING_PLAN,
        required_visible_sections: PLAN_REQUIRED,
        payload_schema: PayloadSchema::Snapshot,
        direct_post: DirectPostPolicy::OpenAttachOnly,
        dashboard_repair: DashboardRepair::Optional,
        closeout_ownership: CloseoutOwnership::PreCloseout,
    },
    RoleSpec {
        role: PayloadRole::State,
        marker_role: "state",
        default_heading: HEADING_STATE,
        required_visible_sections: STATE_REQUIRED,
        payload_schema: PayloadSchema::State,
        direct_post: DirectPostPolicy::Allowed,
        dashboard_repair: DashboardRepair::Expected,
        closeout_ownership: CloseoutOwnership::PreCloseout,
    },
    RoleSpec {
        role: PayloadRole::Session,
        marker_role: "session",
        default_heading: HEADING_SESSION,
        required_visible_sections: SESSION_REQUIRED,
        payload_schema: PayloadSchema::Session,
        direct_post: DirectPostPolicy::Allowed,
        dashboard_repair: DashboardRepair::Optional,
        closeout_ownership: CloseoutOwnership::PreCloseout,
    },
    RoleSpec {
        role: PayloadRole::Validation,
        marker_role: "validation",
        default_heading: HEADING_VALIDATION,
        required_visible_sections: VALIDATION_REQUIRED,
        payload_schema: PayloadSchema::Validation,
        direct_post: DirectPostPolicy::Allowed,
        dashboard_repair: DashboardRepair::Expected,
        closeout_ownership: CloseoutOwnership::PreCloseout,
    },
    RoleSpec {
        role: PayloadRole::Review,
        marker_role: "review",
        default_heading: HEADING_REVIEW,
        required_visible_sections: REVIEW_REQUIRED,
        payload_schema: PayloadSchema::Review,
        direct_post: DirectPostPolicy::Allowed,
        dashboard_repair: DashboardRepair::Expected,
        closeout_ownership: CloseoutOwnership::PreCloseout,
    },
    RoleSpec {
        role: PayloadRole::Closeout,
        marker_role: "closeout",
        default_heading: HEADING_CLOSEOUT,
        required_visible_sections: CLOSEOUT_REQUIRED,
        payload_schema: PayloadSchema::Closeout,
        direct_post: DirectPostPolicy::RecordCloseOwned,
        dashboard_repair: DashboardRepair::OwnedByCloseout,
        closeout_ownership: CloseoutOwnership::Closeout,
    },
];

/// All lifecycle role specifications in canonical order (source → closeout).
pub fn all_roles() -> &'static [RoleSpec] {
    REGISTRY
}

/// Look up the role specification for a [`PayloadRole`].
pub fn role(role: PayloadRole) -> &'static RoleSpec {
    for spec in REGISTRY {
        if spec.role == role {
            return spec;
        }
    }
    // The registry covers every PayloadRole variant; unreachable in practice.
    panic!("lifecycle_vnext::registry missing entry for {role:?}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_has_every_role() {
        let roles: Vec<PayloadRole> = all_roles().iter().map(|spec| spec.role).collect();
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
    fn source_and_plan_are_open_attach_only() {
        assert_eq!(
            role(PayloadRole::Source).direct_post,
            DirectPostPolicy::OpenAttachOnly
        );
        assert_eq!(
            role(PayloadRole::Plan).direct_post,
            DirectPostPolicy::OpenAttachOnly
        );
    }

    #[test]
    fn closeout_is_record_close_owned() {
        let closeout = role(PayloadRole::Closeout);
        assert_eq!(closeout.direct_post, DirectPostPolicy::RecordCloseOwned);
        assert_eq!(closeout.dashboard_repair, DashboardRepair::OwnedByCloseout);
        assert_eq!(closeout.closeout_ownership, CloseoutOwnership::Closeout);
    }

    #[test]
    fn state_requires_task_ledger_heading() {
        let state = role(PayloadRole::State);
        assert!(
            state
                .required_visible_sections
                .contains(&"## Task Ledger")
        );
    }

    #[test]
    fn registry_headings_match_taxonomy_doc() {
        // Canonical headings come from
        // docs/source/plan-issue-redesign/plan-tracking-issue-comment-taxonomy-v1.md
        // — keep in lockstep with the taxonomy table to prevent silent drift.
        let expected: Vec<(PayloadRole, &'static str)> = vec![
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
                role(role_id).default_heading,
                heading,
                "role {role_id:?} heading drift"
            );
        }
    }

    #[test]
    fn every_role_has_visible_section_and_payload_schema() {
        for spec in all_roles() {
            assert!(
                !spec.required_visible_sections.is_empty(),
                "role {:?} has no required visible sections",
                spec.role
            );
            // Every role advertises a payload schema identifier.
            assert!(
                !spec.payload_schema.as_str().is_empty(),
                "role {:?} payload schema is empty",
                spec.role
            );
        }
    }

    #[test]
    fn direct_post_policy_matches_taxonomy() {
        // source/plan are owned by record open|attach. closeout is owned by
        // record close. All other roles allow direct record post.
        for spec in all_roles() {
            let expected = match spec.role {
                PayloadRole::Source | PayloadRole::Plan => DirectPostPolicy::OpenAttachOnly,
                PayloadRole::Closeout => DirectPostPolicy::RecordCloseOwned,
                _ => DirectPostPolicy::Allowed,
            };
            assert_eq!(
                spec.direct_post, expected,
                "role {:?} direct-post policy drift",
                spec.role
            );
        }
    }
}
