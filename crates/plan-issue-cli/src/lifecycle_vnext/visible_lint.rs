//! Visible-completeness lint for rendered lifecycle comments.
//!
//! The lint rejects Profile-only bodies and enforces role-specific visible
//! sections from
//! `docs/source/plan-issue-redesign/plan-tracking-issue-comment-taxonomy-v1.md`.
//! Full rules implemented in Task 2.2.
//!
//! Reusable surface:
//!
//! - [`lint_visible`] evaluates a rendered Markdown body against a role spec.
//! - [`VisibleFinding`] carries a stable role-specific failure code.
//! - [`VisibleReport`] aggregates findings into a pass/fail decision.

use crate::lifecycle_record::PayloadRole;
use crate::lifecycle_vnext::registry::{self, RoleSpec};

/// Stable, role-specific failure code emitted by the lint.
///
/// Codes carry a structured prefix per role so runtime-kit and skill smoke
/// can assert specific gaps (`state-missing-task-ledger`,
/// `validation-missing-overall`, …). The full catalog is finalized in
/// Task 2.2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisibleFinding {
    pub role: PayloadRole,
    pub code: &'static str,
    pub message: String,
}

impl VisibleFinding {
    pub fn new(role: PayloadRole, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            role,
            code,
            message: message.into(),
        }
    }
}

/// Aggregated visible-completeness report.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VisibleReport {
    pub findings: Vec<VisibleFinding>,
}

impl VisibleReport {
    pub fn is_pass(&self) -> bool {
        self.findings.is_empty()
    }

    pub fn push(&mut self, finding: VisibleFinding) {
        self.findings.push(finding);
    }
}

/// Hints that callers can pass to refine lint behavior. The full set is
/// expanded in Task 2.2 (Task Ledger display mode, presence of findings,
/// linked PRs, etc.).
#[derive(Debug, Clone, Copy, Default)]
pub struct LintHints {
    /// `true` when the caller is rendering the final state checkpoint and the
    /// Task Ledger must be expanded. Non-final state may collapse rows.
    pub state_is_final: bool,
    /// `true` when review findings are present and the review comment must
    /// include a disposition row.
    pub review_has_findings: bool,
}

/// Run the visible-completeness lint against a rendered Markdown body for the
/// supplied lifecycle role.
///
/// The Task 1.1 skeleton only checks that the required headings from the
/// registry appear verbatim. Task 2.2 expands the implementation to cover
/// Task Ledger structure, validation status, review disposition, session
/// summary, and closeout approval + linked PR evidence.
pub fn lint_visible(role: PayloadRole, body: &str, hints: LintHints) -> VisibleReport {
    let spec = registry::role(role);
    let mut report = VisibleReport::default();
    check_required_headings(spec, body, &mut report);
    // Task 2.2 adds the role-specific rule blocks below this line.
    let _ = hints;
    report
}

fn check_required_headings(spec: &RoleSpec, body: &str, report: &mut VisibleReport) {
    for heading in spec.required_visible_sections {
        if !body_contains_heading(body, heading) {
            report.push(VisibleFinding::new(
                spec.role,
                missing_heading_code(spec.role),
                format!(
                    "rendered {role} body is missing required heading `{heading}`",
                    role = spec.marker_role
                ),
            ));
        }
    }
}

fn body_contains_heading(body: &str, heading: &str) -> bool {
    body.lines().any(|line| line.trim_end() == heading)
}

fn missing_heading_code(role: PayloadRole) -> &'static str {
    match role {
        PayloadRole::Source => "source-missing-heading",
        PayloadRole::Plan => "plan-missing-heading",
        PayloadRole::State => "state-missing-heading",
        PayloadRole::Session => "session-missing-heading",
        PayloadRole::Validation => "validation-missing-heading",
        PayloadRole::Review => "review-missing-heading",
        PayloadRole::Closeout => "closeout-missing-heading",
    }
}
