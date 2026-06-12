//! Visible-completeness lint for rendered lifecycle comments.
//!
//! The lint rejects Profile-only bodies and enforces role-specific visible
//! sections from
//! `docs/source/plan-issue-redesign/plan-tracking-issue-comment-taxonomy-v1.md`.
//!
//! Reusable surface:
//!
//! - [`lint_visible`] evaluates a rendered Markdown body against a role spec.
//! - [`VisibleFinding`] carries a stable role-specific failure code.
//! - [`VisibleReport`] aggregates findings into a pass/fail decision.

use crate::lifecycle_record::PayloadRole;
use crate::lifecycle_vnext::registry::{self, RoleSpec};

/// Stable, role-specific failure codes emitted by the lint. Runtime-kit
/// smoke and skill flows pattern-match on these strings, so each code is
/// documented next to where it can be returned.
pub mod codes {
    // Generic.
    pub const PROFILE_ONLY: &str = "profile-only";

    // Heading presence (registry-driven).
    pub const SOURCE_MISSING_HEADING: &str = "source-missing-heading";
    pub const PLAN_MISSING_HEADING: &str = "plan-missing-heading";
    pub const STATE_MISSING_HEADING: &str = "state-missing-heading";
    pub const SESSION_MISSING_HEADING: &str = "session-missing-heading";
    pub const VALIDATION_MISSING_HEADING: &str = "validation-missing-heading";
    pub const REVIEW_MISSING_HEADING: &str = "review-missing-heading";
    pub const CLOSEOUT_MISSING_HEADING: &str = "closeout-missing-heading";

    // State.
    pub const STATE_MISSING_TASK_LEDGER: &str = "state-missing-task-ledger";
    pub const STATE_FINAL_TASK_LEDGER_NOT_EXPANDED: &str = "state-final-task-ledger-not-expanded";

    // Session.
    pub const SESSION_MISSING_SUMMARY: &str = "session-missing-summary";

    // Validation.
    pub const VALIDATION_MISSING_OVERALL: &str = "validation-missing-overall";
    pub const VALIDATION_MISSING_COMMANDS_OR_WAIVER: &str = "validation-missing-commands-or-waiver";

    // Review.
    pub const REVIEW_MISSING_DECISION: &str = "review-missing-decision";
    pub const REVIEW_MISSING_CONTEXT: &str = "review-missing-context";
    pub const REVIEW_MISSING_DISPOSITION: &str = "review-missing-disposition";

    // Closeout.
    pub const CLOSEOUT_MISSING_FINAL_STATUS: &str = "closeout-missing-final-status";
    pub const CLOSEOUT_MISSING_APPROVAL: &str = "closeout-missing-approval";
    pub const CLOSEOUT_MISSING_LINKED_PR: &str = "closeout-missing-linked-pr";
}

/// Single lint finding.
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

    pub fn codes(&self) -> Vec<&'static str> {
        self.findings.iter().map(|f| f.code).collect()
    }
}

/// Hints that callers pass to refine lint behavior beyond the registry
/// defaults. `state_is_final` and `review_has_findings` mirror the taxonomy
/// rules from the comment taxonomy doc.
#[derive(Debug, Clone, Copy, Default)]
pub struct LintHints {
    /// `true` when the caller is rendering the final state checkpoint and
    /// the Task Ledger must be expanded. Non-final state may collapse rows
    /// but must keep the `## Task Ledger` heading visible.
    pub state_is_final: bool,
    /// `true` when review findings are present and the review comment must
    /// include a disposition row.
    pub review_has_findings: bool,
    /// `true` when closeout intentionally has no linked PR (docs-only or
    /// admin closeout). The closeout body must then carry an explicit
    /// `Linked PRs: none` note instead of a PR row.
    pub closeout_has_no_linked_pr_ok: bool,
}

/// Run the visible-completeness lint against a rendered Markdown body for
/// the supplied lifecycle role.
pub fn lint_visible(role: PayloadRole, body: &str, hints: LintHints) -> VisibleReport {
    let spec = registry::role(role);
    let mut report = VisibleReport::default();

    check_profile_only(spec, body, &mut report);
    check_required_headings(spec, body, &mut report);

    match role {
        PayloadRole::Source | PayloadRole::Plan => {
            // Heading presence + non-Profile-only check is enough; snapshot
            // bodies carry the `<details>` element from the taxonomy template.
        }
        PayloadRole::State => check_state(body, hints, &mut report),
        PayloadRole::Session => check_session(body, &mut report),
        PayloadRole::Validation => check_validation(body, &mut report),
        PayloadRole::Review => check_review(body, hints, &mut report),
        PayloadRole::Closeout => check_closeout(body, hints, &mut report),
    }

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

fn check_profile_only(spec: &RoleSpec, body: &str, report: &mut VisibleReport) {
    if is_profile_only_body(body, spec.default_heading) {
        report.push(VisibleFinding::new(
            spec.role,
            codes::PROFILE_ONLY,
            format!(
                "rendered {role} body has nothing beyond the role heading and `Profile:` line",
                role = spec.marker_role
            ),
        ));
    }
}

fn check_state(body: &str, hints: LintHints, report: &mut VisibleReport) {
    let task_ledger = task_ledger_section(body);
    if task_ledger.is_none() {
        report.push(VisibleFinding::new(
            PayloadRole::State,
            codes::STATE_MISSING_TASK_LEDGER,
            "state body must include a visible `## Task Ledger` heading",
        ));
    }
    if hints.state_is_final && task_ledger.is_some_and(|section| section.appears_collapsed) {
        report.push(VisibleFinding::new(
            PayloadRole::State,
            codes::STATE_FINAL_TASK_LEDGER_NOT_EXPANDED,
            "final state must expand the Task Ledger rows (no `<details>` wrapper)",
        ));
    }
}

fn check_session(body: &str, report: &mut VisibleReport) {
    let summary_present = body.lines().any(|line| {
        let trimmed = line.trim();
        let prefix = "- Summary:";
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            !rest.trim().is_empty()
        } else {
            false
        }
    });
    if !summary_present {
        report.push(VisibleFinding::new(
            PayloadRole::Session,
            codes::SESSION_MISSING_SUMMARY,
            "session body must contain a non-empty `- Summary:` line",
        ));
    }
}

fn check_validation(body: &str, report: &mut VisibleReport) {
    let overall = body.lines().any(|line| {
        let trimmed = line.trim();
        let prefix = "- Overall:";
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            !rest.trim().is_empty()
        } else {
            false
        }
    });
    if !overall {
        report.push(VisibleFinding::new(
            PayloadRole::Validation,
            codes::VALIDATION_MISSING_OVERALL,
            "validation body must contain a `- Overall: pass|partial|fail` line",
        ));
    }
    // Commands table OR a Waivers section is required to back the overall.
    let has_command_row = body_contains_validation_command_row(body);
    let has_waiver = body_contains_heading(body, "### Waivers")
        || body
            .lines()
            .any(|line| line.trim_start().starts_with("- `"));
    if !has_command_row && !has_waiver {
        report.push(VisibleFinding::new(
            PayloadRole::Validation,
            codes::VALIDATION_MISSING_COMMANDS_OR_WAIVER,
            "validation body must include at least one command row or an explicit waiver",
        ));
    }
}

fn check_review(body: &str, hints: LintHints, report: &mut VisibleReport) {
    let decision_present = body.lines().any(|line| {
        let trimmed = line.trim();
        let prefix = "- Decision:";
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            !rest.trim().is_empty()
        } else {
            false
        }
    });
    if !decision_present {
        report.push(VisibleFinding::new(
            PayloadRole::Review,
            codes::REVIEW_MISSING_DECISION,
            "review body must contain a `- Decision:` line with approve|request-changes|comments-only",
        ));
    }
    if decision_present
        && !body_contains_review_context(body)
        && !body_contains_review_disposition_row(body)
    {
        report.push(VisibleFinding::new(
            PayloadRole::Review,
            codes::REVIEW_MISSING_CONTEXT,
            "review body must include lenses, outcome evidence, finding rows, or an explicit review context marker",
        ));
    }
    if hints.review_has_findings && !body_contains_review_disposition_row(body) {
        report.push(VisibleFinding::new(
            PayloadRole::Review,
            codes::REVIEW_MISSING_DISPOSITION,
            "review body has findings but no disposition row in the findings table",
        ));
    }
}

fn check_closeout(body: &str, hints: LintHints, report: &mut VisibleReport) {
    let final_status_present = body.lines().any(|line| {
        let trimmed = line.trim();
        let prefix = "- Final status:";
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            !rest.trim().is_empty()
        } else {
            false
        }
    });
    if !final_status_present {
        report.push(VisibleFinding::new(
            PayloadRole::Closeout,
            codes::CLOSEOUT_MISSING_FINAL_STATUS,
            "closeout body must contain a `- Final status:` line",
        ));
    }
    let approval_present = body.lines().any(|line| {
        let trimmed = line.trim();
        let prefix = "- Approval:";
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            !rest.trim().is_empty()
        } else {
            false
        }
    });
    if !approval_present {
        report.push(VisibleFinding::new(
            PayloadRole::Closeout,
            codes::CLOSEOUT_MISSING_APPROVAL,
            "closeout body must contain a `- Approval:` line",
        ));
    }
    if !hints.closeout_has_no_linked_pr_ok && !body_contains_closeout_linked_pr_evidence(body) {
        report.push(VisibleFinding::new(
            PayloadRole::Closeout,
            codes::CLOSEOUT_MISSING_LINKED_PR,
            "closeout body must include linked PR evidence or an explicit no-PR note",
        ));
    }
}

fn body_contains_heading(body: &str, heading: &str) -> bool {
    body.lines().any(|line| line.trim_end() == heading)
}

#[derive(Clone, Copy)]
struct TaskLedgerSection {
    appears_collapsed: bool,
}

fn task_ledger_section(body: &str) -> Option<TaskLedgerSection> {
    // Free-form summaries may quote lifecycle headings above the generated
    // state body. The renderer's Task Ledger is the final ledger section.
    let lines = body.lines().collect::<Vec<_>>();
    let start = lines
        .iter()
        .rposition(|line| line.trim() == "## Task Ledger")?;
    let mut appears_collapsed = false;
    for line in &lines[start + 1..] {
        let trimmed = line.trim();
        if trimmed.starts_with("## ") {
            break;
        }
        if trimmed.starts_with("<details") {
            appears_collapsed = true;
            break;
        }
        if trimmed.starts_with("| ") {
            break;
        }
    }
    Some(TaskLedgerSection { appears_collapsed })
}

fn body_contains_validation_command_row(body: &str) -> bool {
    // Walk lines and find table data rows under a validation-shaped header.
    // The renderer in `lifecycle_record` does not necessarily wrap the
    // command in backticks, so the lint accepts any non-empty data row
    // beneath the `| Command | Status | Evidence |` header.
    let mut in_validation_table = false;
    let mut saw_separator = false;
    for line in body.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with('|') {
            in_validation_table = false;
            saw_separator = false;
            continue;
        }
        if trimmed.contains("Command") && trimmed.contains("Status") {
            in_validation_table = true;
            saw_separator = false;
            continue;
        }
        if in_validation_table && is_table_separator(trimmed) {
            saw_separator = true;
            continue;
        }
        if in_validation_table && saw_separator {
            return true;
        }
    }
    false
}

fn is_table_separator(line: &str) -> bool {
    line.chars().all(|c| matches!(c, '|' | '-' | ' ' | ':')) && line.contains("---")
}

fn body_contains_review_disposition_row(body: &str) -> bool {
    let dispositions = ["fixed", "residual", "follow-up", "deferred", "no-action"];
    body.lines().any(|line| {
        let trimmed = line.trim();
        if !trimmed.starts_with('|') {
            return false;
        }
        if trimmed.contains("Disposition") || trimmed.contains("---") {
            return false;
        }
        dispositions.iter().any(|d| trimmed.contains(d))
    })
}

fn body_contains_review_context(body: &str) -> bool {
    ["- Lenses:", "- Outcome comment:"]
        .iter()
        .any(|prefix| body_contains_non_empty_line_value(body, prefix))
}

fn body_contains_non_empty_line_value(body: &str, prefix: &str) -> bool {
    body.lines().any(|line| {
        line.trim()
            .strip_prefix(prefix)
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false)
    })
}

fn body_contains_closeout_linked_pr_evidence(body: &str) -> bool {
    // Accept either a Markdown table row with a PR-looking cell or an
    // explicit `- Linked PRs:` line.
    let table_row = body.lines().any(|line| {
        let trimmed = line.trim();
        if !trimmed.starts_with('|') || trimmed.contains("---") || trimmed.contains("| PR ") {
            return false;
        }
        trimmed.contains('#') || trimmed.contains("http")
    });
    let linked_line = body.lines().any(|line| {
        let trimmed = line.trim();
        trimmed.starts_with("- Linked PR")
            || trimmed.starts_with("- PR:")
            || trimmed.starts_with("- Linked PRs:")
    });
    table_row || linked_line
}

fn is_profile_only_body(body: &str, default_heading: &str) -> bool {
    let mut saw_heading = false;
    let mut saw_profile = false;
    let mut saw_other_content = false;
    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with("<!--") || trimmed.ends_with("-->") {
            continue;
        }
        if trimmed == default_heading {
            saw_heading = true;
            continue;
        }
        if trimmed.starts_with("- Profile:") {
            saw_profile = true;
            continue;
        }
        saw_other_content = true;
    }
    saw_heading && saw_profile && !saw_other_content
}

fn missing_heading_code(role: PayloadRole) -> &'static str {
    match role {
        PayloadRole::Source => codes::SOURCE_MISSING_HEADING,
        PayloadRole::Plan => codes::PLAN_MISSING_HEADING,
        PayloadRole::State => codes::STATE_MISSING_HEADING,
        PayloadRole::Session => codes::SESSION_MISSING_HEADING,
        PayloadRole::Validation => codes::VALIDATION_MISSING_HEADING,
        PayloadRole::Review => codes::REVIEW_MISSING_HEADING,
        PayloadRole::Closeout => codes::CLOSEOUT_MISSING_HEADING,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn final_state_hints() -> LintHints {
        LintHints {
            state_is_final: true,
            ..LintHints::default()
        }
    }

    #[test]
    fn final_state_uses_real_task_ledger_after_summary_heading() {
        let body = concat!(
            "## Execution State\n\n",
            "- Profile: tracking\n",
            "- Status: complete\n\n",
            "Summary quotes a task ledger heading and table:\n\n",
            "## Task Ledger\n\n",
            "| Quoted | Row |\n",
            "| --- | --- |\n",
            "| old | content |\n\n",
            "## Current State\n\n",
            "- Current task: done\n\n",
            "## Task Ledger\n\n",
            "<details>\n",
            "<summary>Show task ledger</summary>\n\n",
            "| ID | Status | Task |\n",
            "| --- | --- | --- |\n",
            "| 1.1 | done | Ship |\n\n",
            "</details>\n"
        );

        let report = lint_visible(PayloadRole::State, body, final_state_hints());

        assert!(
            report
                .codes()
                .contains(&codes::STATE_FINAL_TASK_LEDGER_NOT_EXPANDED),
            "{report:?}"
        );
    }

    #[test]
    fn final_state_ignores_summary_quoted_collapsed_ledger() {
        let body = concat!(
            "## Execution State\n\n",
            "- Profile: tracking\n",
            "- Status: complete\n\n",
            "Summary quotes a prior collapsed task ledger:\n\n",
            "## Task Ledger\n\n",
            "<details>\n",
            "<summary>Show task ledger</summary>\n\n",
            "| ID | Status | Task |\n",
            "| --- | --- | --- |\n",
            "| 1.1 | done | Old |\n\n",
            "</details>\n\n",
            "## Current State\n\n",
            "- Current task: done\n\n",
            "## Task Ledger\n\n",
            "| ID | Status | Task |\n",
            "| --- | --- | --- |\n",
            "| 1.1 | done | Ship |\n"
        );

        let report = lint_visible(PayloadRole::State, body, final_state_hints());

        assert!(report.is_pass(), "{report:?}");
    }
}
