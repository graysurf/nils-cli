//! Terminal-state reconciliation for archived execution-state plan docs.
//!
//! A plan's `*-execution-state.md` header is committed during PR delivery,
//! *before* the PR merges and the tracking issue is closed, so left alone it
//! freezes at a mid-flight status (e.g. "repo PR delivery pending"). Because
//! `migrate` only ever archives a *closed* plan, it is the deterministic,
//! offline chokepoint at which that header can be reconciled to a terminal
//! status before the bundle enters the archive — so the archived,
//! human-readable state never reads as if the work were still in flight.
//!
//! Reconciliation is purely textual and label-driven: only the `Status`,
//! `Current task`, and `Next task` bullets inside the `## Execution State`
//! section are rewritten (along with any wrapped continuation lines of those
//! bullets). Everything else — the task ledger, validation table, session
//! log, snapshot links — is preserved verbatim. The authoritative live state
//! still lives in the provider issue snapshot (`_index/`) and `catalog.json`
//! refs; this only stops the bundle's own header from lying.

/// Heading that opens the reconciled section.
const SECTION_HEADING: &str = "## Execution State";

/// If `file_rel` names an execution-state plan doc whose `## Execution State`
/// header should be reconciled, return its rewritten content. Returns `None`
/// for any other file (the caller then copies it verbatim).
///
/// `primary_ref` is the tracking reference the terminal status should defer to
/// (the issue ref when present, else the PR/MR ref).
pub fn reconcile_archived_execution_state(
    file_rel: &str,
    content: &str,
    primary_ref: Option<&str>,
) -> Option<String> {
    if !is_execution_state_doc(file_rel, content) {
        return None;
    }

    let status_line = match primary_ref {
        Some(r) => format!(
            "- Status: archived — plan bundle migrated to agent-plan-archive; final state tracked in {r}"
        ),
        None => "- Status: archived — plan bundle migrated to agent-plan-archive".to_string(),
    };

    let mut out: Vec<String> = Vec::new();
    let mut in_section = false;
    let mut lines = content.lines().peekable();

    while let Some(line) = lines.next() {
        if is_heading(line, SECTION_HEADING) {
            in_section = true;
            out.push(line.to_string());
            continue;
        }
        // A later top-level `## ` heading closes the section.
        if in_section && line.starts_with("## ") {
            in_section = false;
            out.push(line.to_string());
            continue;
        }

        if in_section && let Some(replacement) = terminal_replacement(line, &status_line) {
            out.push(replacement);
            // Drop the original bullet's wrapped continuation lines so a
            // multi-line "Status:" value does not leave an orphaned tail.
            while lines.peek().is_some_and(|peek| is_continuation(peek)) {
                lines.next();
            }
            continue;
        }

        out.push(line.to_string());
    }

    let mut result = out.join("\n");
    if content.ends_with('\n') {
        result.push('\n');
    }
    Some(result)
}

/// True when the file is an execution-state plan doc: its name ends with
/// `execution-state.md` *and* it actually carries a `## Execution State`
/// section. Both conditions guard against rewriting unrelated markdown.
fn is_execution_state_doc(file_rel: &str, content: &str) -> bool {
    file_rel.ends_with("execution-state.md")
        && content.lines().any(|l| is_heading(l, SECTION_HEADING))
}

/// Exact (trimmed) heading match, e.g. `## Execution State`.
fn is_heading(line: &str, heading: &str) -> bool {
    line.trim_end() == heading
}

/// Map a section bullet to its terminal replacement, or `None` to keep it.
fn terminal_replacement(line: &str, status_line: &str) -> Option<String> {
    if line.starts_with("- Status:") {
        Some(status_line.to_string())
    } else if line.starts_with("- Current task:") {
        Some("- Current task: none — archived".to_string())
    } else if line.starts_with("- Next task:") {
        Some("- Next task: none — archived".to_string())
    } else {
        None
    }
}

/// A wrapped continuation line of the preceding bullet: indented and
/// non-empty. A blank line or a fresh `- ` bullet ends the continuation.
fn is_continuation(line: &str) -> bool {
    (line.starts_with(' ') || line.starts_with('\t')) && !line.trim().is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "<!-- execute-from-tracking-issue:state:v1 -->\n\
# Demo Execution State\n\
\n\
## Execution State\n\
\n\
- Status: implementation complete — all tasks done; repo PR\n  \
delivery pending\n\
- Target scope: whole issue\n\
- Current task: all tasks done; delivering the repo PR\n\
- Next task: `forge-cli pr deliver` → close-ready handoff\n\
- Last updated: 2026-05-30\n\
\n\
## Task Ledger\n\
\n\
- Status: this bullet is outside the section and must be kept\n\
- Current task: also kept verbatim\n";

    fn reconcile(content: &str) -> String {
        reconcile_archived_execution_state(
            "docs/plans/demo/demo-execution-state.md",
            content,
            Some("https://github.com/org/repo/issues/2"),
        )
        .expect("execution-state doc should reconcile")
    }

    #[test]
    fn rewrites_status_current_and_next_inside_section() {
        let out = reconcile(SAMPLE);
        assert!(out.contains(
            "- Status: archived — plan bundle migrated to agent-plan-archive; \
final state tracked in https://github.com/org/repo/issues/2"
        ));
        assert!(out.contains("- Current task: none — archived"));
        assert!(out.contains("- Next task: none — archived"));
    }

    #[test]
    fn drops_wrapped_continuation_of_status_bullet() {
        let out = reconcile(SAMPLE);
        // The original "delivery pending" wrap must not survive.
        assert!(!out.contains("delivery pending"));
        assert!(!out.contains("implementation complete"));
    }

    #[test]
    fn preserves_lines_outside_the_section() {
        let out = reconcile(SAMPLE);
        assert!(out.contains("- Target scope: whole issue"));
        assert!(out.contains("- Last updated: 2026-05-30"));
        assert!(out.contains("## Task Ledger"));
        // Bullets after the section heading are left untouched, even when they
        // share a label with a reconciled bullet.
        assert!(out.contains("- Status: this bullet is outside the section and must be kept"));
        assert!(out.contains("- Current task: also kept verbatim"));
    }

    #[test]
    fn omits_ref_clause_when_no_ref_supplied() {
        let out = reconcile_archived_execution_state(
            "docs/plans/demo/demo-execution-state.md",
            SAMPLE,
            None,
        )
        .expect("reconciles without a ref");
        assert!(out.contains("- Status: archived — plan bundle migrated to agent-plan-archive\n"));
        assert!(!out.contains("final state tracked in"));
    }

    #[test]
    fn is_idempotent() {
        let once = reconcile(SAMPLE);
        let twice = reconcile(&once);
        assert_eq!(once, twice);
    }

    #[test]
    fn preserves_trailing_newline_presence() {
        assert!(reconcile(SAMPLE).ends_with('\n'));
        let no_trailing = SAMPLE.trim_end_matches('\n');
        let out = reconcile(no_trailing);
        assert!(!out.ends_with('\n'));
    }

    #[test]
    fn skips_non_execution_state_files() {
        assert!(
            reconcile_archived_execution_state("docs/plans/demo/PLAN.md", SAMPLE, Some("x"))
                .is_none()
        );
    }

    #[test]
    fn skips_execution_state_named_file_without_section() {
        let body = "# Title\n\nNo execution state heading here.\n";
        assert!(
            reconcile_archived_execution_state(
                "docs/plans/demo/demo-execution-state.md",
                body,
                Some("x")
            )
            .is_none()
        );
    }
}
