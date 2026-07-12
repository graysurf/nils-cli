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
    let lines = body.lines().collect::<Vec<_>>();
    let rendered_start = lines
        .iter()
        .enumerate()
        .filter_map(|(idx, line)| {
            matches!(line.trim(), "## Current State" | "## Execution State").then_some(idx)
        })
        .next_back()
        .map_or(0, |idx| idx + 1);
    let start = lines
        .iter()
        .enumerate()
        .skip(rendered_start)
        .find_map(|(idx, line)| (line.trim() == "## Task Ledger").then_some(idx))?;
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
    let finding_headers = ["ID", "Severity", "Disposition", "Summary"];
    let mut pending_header = None;
    let mut disposition_column = None;
    let mut fence = None;
    let mut in_html_comment = false;
    let mut code_span_delimiter = None;
    let lines = body.lines().collect::<Vec<_>>();

    for (line_index, line) in lines.iter().enumerate() {
        if let Some((opening_marker, opening_length)) = fence {
            if is_markdown_fence_closer(line, opening_marker, opening_length) {
                fence = None;
            }
            continue;
        }

        if !in_html_comment && code_span_delimiter.is_none() {
            if line.starts_with("    ") || line.starts_with('\t') {
                pending_header = None;
                disposition_column = None;
                continue;
            }
            if let Some((marker, length)) = markdown_fence_opener(line) {
                fence = Some((marker, length));
                pending_header = None;
                disposition_column = None;
                continue;
            }
        }

        let comment_was_open = in_html_comment;
        let code_span_was_open = code_span_delimiter.is_some();
        let visible_line = strip_html_comments(
            line,
            &lines[line_index + 1..],
            &mut in_html_comment,
            &mut code_span_delimiter,
        );
        if comment_was_open
            || in_html_comment
            || code_span_was_open
            || code_span_delimiter.is_some()
        {
            pending_header = None;
            disposition_column = None;
            continue;
        }

        let trimmed = visible_line.trim();
        if visible_line.starts_with("    ") || visible_line.starts_with('\t') {
            pending_header = None;
            disposition_column = None;
            continue;
        }

        let Some(cells) = markdown_table_cells(trimmed) else {
            pending_header = None;
            disposition_column = None;
            continue;
        };

        if let Some((index, width)) = disposition_column {
            if cells.len() == width
                && cells
                    .get(index)
                    .is_some_and(|cell| dispositions.contains(cell))
            {
                return true;
            }
            continue;
        }

        if let Some((index, width)) = pending_header.take()
            && cells.len() == width
            && is_markdown_table_separator(&cells)
        {
            disposition_column = Some((index, width));
            continue;
        }

        if cells.len() == finding_headers.len()
            && finding_headers.iter().all(|header| cells.contains(header))
            && let Some(index) = cells.iter().position(|cell| *cell == "Disposition")
        {
            pending_header = Some((index, cells.len()));
        }
    }

    false
}

fn markdown_fence(line: &str) -> Option<(char, usize, &str)> {
    let marker = line.chars().next()?;
    if !matches!(marker, '`' | '~') {
        return None;
    }
    let length = line
        .chars()
        .take_while(|character| *character == marker)
        .count();
    (length >= 3).then_some((marker, length, &line[length..]))
}

fn markdown_fence_opener(line: &str) -> Option<(char, usize)> {
    let candidate = markdown_after_fence_indent(line)?;
    let (marker, length, remainder) = markdown_fence(candidate)?;
    if marker == '`' && remainder.contains('`') {
        return None;
    }
    Some((marker, length))
}

fn is_markdown_fence_closer(line: &str, opening_marker: char, opening_length: usize) -> bool {
    let Some(candidate) = markdown_after_fence_indent(line) else {
        return false;
    };
    markdown_fence(candidate).is_some_and(|(marker, length, remainder)| {
        marker == opening_marker && length >= opening_length && remainder.trim().is_empty()
    })
}

fn markdown_after_fence_indent(line: &str) -> Option<&str> {
    let indentation = line.bytes().take_while(|byte| *byte == b' ').count();
    if indentation > 3 {
        return None;
    }
    let candidate = &line[indentation..];
    (!candidate.starts_with('\t')).then_some(candidate)
}

fn strip_html_comments(
    line: &str,
    future_lines: &[&str],
    in_comment: &mut bool,
    code_span_delimiter: &mut Option<usize>,
) -> String {
    let mut visible = String::with_capacity(line.len());
    let mut index = 0;

    while index < line.len() {
        let remainder = &line[index..];
        if *in_comment {
            if remainder.starts_with("-->") {
                *in_comment = false;
                index += 3;
            } else {
                index += remainder
                    .chars()
                    .next()
                    .expect("non-empty remainder")
                    .len_utf8();
            }
            continue;
        }

        if remainder.starts_with('`') {
            let length = remainder.bytes().take_while(|byte| *byte == b'`').count();
            visible.push_str(&remainder[..length]);
            if code_span_delimiter.is_some_and(|opening_length| opening_length == length) {
                *code_span_delimiter = None;
            } else if code_span_delimiter.is_none()
                && !is_backslash_escaped(line, index)
                && has_matching_code_span_delimiter(&remainder[length..], future_lines, length)
            {
                *code_span_delimiter = Some(length);
            }
            index += length;
            continue;
        }
        if code_span_delimiter.is_none() && remainder.starts_with("<!--") {
            *in_comment = true;
            index += 4;
            continue;
        }

        let character = remainder.chars().next().expect("non-empty remainder");
        visible.push(character);
        index += character.len_utf8();
    }

    visible
}

fn is_backslash_escaped(line: &str, index: usize) -> bool {
    line[..index]
        .bytes()
        .rev()
        .take_while(|byte| *byte == b'\\')
        .count()
        % 2
        == 1
}

fn has_matching_code_span_delimiter(
    current_remainder: &str,
    future_lines: &[&str],
    delimiter_length: usize,
) -> bool {
    if contains_backtick_run(current_remainder, delimiter_length) {
        return true;
    }
    for line in future_lines {
        if line.trim().is_empty() || markdown_fence_opener(line).is_some() {
            break;
        }
        if contains_backtick_run(line, delimiter_length) {
            return true;
        }
    }
    false
}

fn contains_backtick_run(text: &str, delimiter_length: usize) -> bool {
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'`' {
            index += 1;
            continue;
        }
        let start = index;
        while index < bytes.len() && bytes[index] == b'`' {
            index += 1;
        }
        if index - start == delimiter_length {
            return true;
        }
    }
    false
}

fn is_markdown_table_separator(cells: &[&str]) -> bool {
    !cells.is_empty()
        && cells.iter().all(|cell| {
            let without_leading_alignment = cell.strip_prefix(':').unwrap_or(cell);
            let hyphens = without_leading_alignment
                .strip_suffix(':')
                .unwrap_or(without_leading_alignment);
            hyphens.len() >= 3 && hyphens.bytes().all(|byte| byte == b'-')
        })
}

fn markdown_table_cells(line: &str) -> Option<Vec<&str>> {
    let inner = line.strip_prefix('|')?.strip_suffix('|')?;
    let mut cells = Vec::new();
    let mut start = 0;
    let mut preceding_backslashes = 0;

    // Markdown treats a pipe as escaped only after an odd-length backslash
    // run. The lifecycle renderer uses this form for literal pipes in IDs and
    // summaries, so those bytes must remain inside their original cell.
    for (index, character) in inner.char_indices() {
        if character == '|' && preceding_backslashes % 2 == 0 {
            cells.push(inner[start..index].trim());
            start = index + character.len_utf8();
            preceding_backslashes = 0;
        } else if character == '\\' {
            preceding_backslashes += 1;
        } else {
            preceding_backslashes = 0;
        }
    }
    cells.push(inner[start..].trim());

    Some(cells)
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
