//! Visible-completeness lint coverage (Task 2.2).
//!
//! For every lifecycle role, exercise one passing fixture and one or more
//! failing fixtures so failure codes stay stable and Hidden-payload success
//! alone cannot satisfy visible completeness.
//!
//! Source: `docs/source/plan-issue-redesign/plan-tracking-issue-comment-taxonomy-v1.md`.

use plan_issue::commands::record::{LifecycleCommentKind, RecordProfile};
use plan_issue::lifecycle_record::{PayloadRole, render_record_post_comment};
use plan_issue::lifecycle_vnext::visible_lint::{LintHints, codes, lint_visible};
use serde_json::json;

fn body_pass_state_non_final() -> String {
    "<!-- plan-issue-record:v2 role=state profile=tracking -->\n\n\
     ## Execution State\n\n\
     - Profile: tracking\n\
     - Status: in-progress\n\
     - Target scope: vNext sprint 2\n\
     - Current task: 2.2\n\n\
     ## Task Ledger\n\n\
     <details><summary>Show task ledger</summary>\n\n\
     | ID | Status | Task |\n\
     | --- | --- | --- |\n\
     | 2.1 | done | registry |\n\
     | 2.2 | in-progress | visible-lint |\n\n\
     </details>\n"
        .to_string()
}

fn body_pass_state_final_expanded() -> String {
    "## Execution State\n\n\
     - Profile: tracking\n\
     - Status: complete\n\
     - Target scope: vNext sprint 2\n\n\
     ## Task Ledger\n\n\
     | ID | Status | Task |\n\
     | --- | --- | --- |\n\
     | 2.1 | done | registry |\n\
     | 2.2 | done | visible-lint |\n"
        .to_string()
}

#[test]
fn visible_lint_state_passing_non_final() {
    let report = lint_visible(
        PayloadRole::State,
        &body_pass_state_non_final(),
        LintHints::default(),
    );
    assert!(
        report.is_pass(),
        "non-final state should lint clean; findings={:?}",
        report.findings
    );
}

#[test]
fn visible_lint_state_final_must_expand_task_ledger() {
    let hints = LintHints {
        state_is_final: true,
        ..LintHints::default()
    };
    let collapsed = body_pass_state_non_final();
    let report = lint_visible(PayloadRole::State, &collapsed, hints);
    assert!(
        report
            .codes()
            .contains(&codes::STATE_FINAL_TASK_LEDGER_NOT_EXPANDED),
        "final state with collapsed ledger should fail; codes={:?}",
        report.codes()
    );
}

#[test]
fn visible_lint_state_final_anchors_to_rendered_task_ledger() {
    let hints = LintHints {
        state_is_final: true,
        ..LintHints::default()
    };
    let body = "## Execution State\n\n\
        - Profile: tracking\n\
        - Status: complete\n\n\
        ## Task Ledger\n\n\
        <details>\n\
        <summary>Show task ledger</summary>\n\n\
        | ID | Status | Task |\n\
        | --- | --- | --- |\n\
        | 2.1 | done | implementation |\n\n\
        </details>\n\n\
        ## Validation Notes\n\n\
        A reviewer quoted a later heading from a prior state comment:\n\n\
        ## Task Ledger\n\n\
        | ID | Status | Task |\n\
        | --- | --- | --- |\n\
        | quoted | done | not the rendered ledger |\n";

    let report = lint_visible(PayloadRole::State, body, hints);

    assert!(
        report
            .codes()
            .contains(&codes::STATE_FINAL_TASK_LEDGER_NOT_EXPANDED),
        "final state must inspect the rendered Task Ledger, not a later quote; codes={:?}",
        report.codes()
    );
}

#[test]
fn visible_lint_state_final_prefers_later_execution_anchor_over_summary_current_state() {
    let hints = LintHints {
        state_is_final: true,
        ..LintHints::default()
    };
    let body = "Summary quotes an earlier state section:\n\n\
        ## Current State\n\n\
        ## Task Ledger\n\n\
        | ID | Status | Task |\n\
        | --- | --- | --- |\n\
        | quoted | done | old expanded ledger |\n\n\
        ## Execution State\n\n\
        - Profile: tracking\n\
        - Status: complete\n\n\
        ## Task Ledger\n\n\
        <details>\n\
        <summary>Show task ledger</summary>\n\n\
        | ID | Status | Task |\n\
        | --- | --- | --- |\n\
        | 2.1 | done | implementation |\n\n\
        </details>\n";

    let report = lint_visible(PayloadRole::State, body, hints);

    assert!(
        report
            .codes()
            .contains(&codes::STATE_FINAL_TASK_LEDGER_NOT_EXPANDED),
        "final state must prefer the later rendered Execution State anchor; codes={:?}",
        report.codes()
    );
}

#[test]
fn visible_lint_state_final_expanded_passes() {
    let hints = LintHints {
        state_is_final: true,
        ..LintHints::default()
    };
    let report = lint_visible(PayloadRole::State, &body_pass_state_final_expanded(), hints);
    assert!(
        report.is_pass(),
        "expanded final state should lint clean; findings={:?}",
        report.findings
    );
}

#[test]
fn visible_lint_state_missing_task_ledger_is_blocked() {
    let body = "## Execution State\n\n- Profile: tracking\n- Status: in-progress\n";
    let report = lint_visible(PayloadRole::State, body, LintHints::default());
    assert!(
        report.codes().contains(&codes::STATE_MISSING_TASK_LEDGER),
        "codes={:?}",
        report.codes()
    );
}

#[test]
fn visible_lint_session_requires_non_empty_summary() {
    let pass =
        "## Execution Session\n\n- Profile: tracking\n- Summary: implemented vNext registry\n";
    let fail = "## Execution Session\n\n- Profile: tracking\n- Summary: \n";

    let ok = lint_visible(PayloadRole::Session, pass, LintHints::default());
    assert!(ok.is_pass(), "{:?}", ok.findings);

    let bad = lint_visible(PayloadRole::Session, fail, LintHints::default());
    assert!(
        bad.codes().contains(&codes::SESSION_MISSING_SUMMARY),
        "codes={:?}",
        bad.codes()
    );
}

#[test]
fn visible_lint_validation_requires_overall_and_evidence() {
    let pass = "## Validation Evidence\n\n\
                - Profile: tracking\n\
                - Overall: pass\n\n\
                | Command | Status | Evidence |\n\
                | --- | --- | --- |\n\
                | `cargo test` | pass | log.txt |\n";
    let no_overall =
        "## Validation Evidence\n\n- Profile: tracking\n\n| Command |\n|---|\n| `cargo test` |\n";
    let no_evidence = "## Validation Evidence\n\n- Profile: tracking\n- Overall: pass\n";

    let ok = lint_visible(PayloadRole::Validation, pass, LintHints::default());
    assert!(ok.is_pass(), "{:?}", ok.findings);

    let bad1 = lint_visible(PayloadRole::Validation, no_overall, LintHints::default());
    assert!(
        bad1.codes().contains(&codes::VALIDATION_MISSING_OVERALL),
        "codes={:?}",
        bad1.codes()
    );

    let bad2 = lint_visible(PayloadRole::Validation, no_evidence, LintHints::default());
    assert!(
        bad2.codes()
            .contains(&codes::VALIDATION_MISSING_COMMANDS_OR_WAIVER),
        "codes={:?}",
        bad2.codes()
    );
}

#[test]
fn visible_lint_review_decision_and_disposition() {
    let pass_with_findings = "## Review Evidence\n\n\
        - Profile: tracking\n\
        - Decision: approve\n\n\
        | ID | Severity | Disposition | Summary |\n\
        | --- | --- | --- | --- |\n\
        | F1 | minor | fixed | tiny nit |\n";
    let pass_no_findings = "## Review Evidence\n\n\
        - Profile: tracking\n\
        - Decision: comments-only\n\
        - Lenses: testing\n";
    let no_review_context = "## Review Evidence\n\n- Profile: tracking\n- Decision: approve\n";
    let no_decision = "## Review Evidence\n\n- Profile: tracking\n";
    let with_findings_no_disposition = "## Review Evidence\n\n\
        - Profile: tracking\n\
        - Decision: approve\n\n\
        | ID | Severity | Disposition | Summary |\n\
        | --- | --- | --- | --- |\n\
        | F1 | minor | TBD | tiny nit |\n";

    let hints_findings = LintHints {
        review_has_findings: true,
        ..LintHints::default()
    };

    let ok = lint_visible(PayloadRole::Review, pass_with_findings, hints_findings);
    assert!(ok.is_pass(), "{:?}", ok.findings);

    let ok_empty = lint_visible(PayloadRole::Review, pass_no_findings, LintHints::default());
    assert!(ok_empty.is_pass(), "{:?}", ok_empty.findings);

    let bad_no_context = lint_visible(PayloadRole::Review, no_review_context, LintHints::default());
    assert!(
        bad_no_context
            .codes()
            .contains(&codes::REVIEW_MISSING_CONTEXT),
        "codes={:?}",
        bad_no_context.codes()
    );

    let bad_no_decision = lint_visible(PayloadRole::Review, no_decision, LintHints::default());
    assert!(
        bad_no_decision
            .codes()
            .contains(&codes::REVIEW_MISSING_DECISION),
        "codes={:?}",
        bad_no_decision.codes()
    );

    let bad_missing_disposition = lint_visible(
        PayloadRole::Review,
        with_findings_no_disposition,
        hints_findings,
    );
    assert!(
        bad_missing_disposition
            .codes()
            .contains(&codes::REVIEW_MISSING_DISPOSITION),
        "codes={:?}",
        bad_missing_disposition.codes()
    );
}

#[test]
fn visible_lint_review_accepts_disposition_word_in_summary() {
    let body = "## Review Evidence\n\n\
        - Profile: tracking\n\
        - Decision: approve\n\n\
        | ID | Severity | Disposition | Summary |\n\
        | --- | --- | --- | --- |\n\
        | F1 | minor | fixed | Disposition schema is documented |\n";
    let hints = LintHints {
        review_has_findings: true,
        ..LintHints::default()
    };

    let report = lint_visible(PayloadRole::Review, body, hints);

    assert!(report.is_pass(), "{:?}", report.findings);
}

#[test]
fn visible_lint_review_accepts_escaped_pipes_in_cells() {
    let body = render_record_post_comment(
        RecordProfile::Tracking,
        LifecycleCommentKind::Review,
        json!({
            "decision": "approve",
            "lenses": ["testing"],
            "findings": [{
                "id": "F|1",
                "severity": "minor",
                "disposition": "fixed",
                "summary": "API | CLI Disposition schema",
            }],
            "outcome_comment_url": "https://example.test/review",
        }),
        None,
        Some("2026-07-12T00:00:00Z"),
    )
    .expect("render review body");
    let hints = LintHints {
        review_has_findings: true,
        ..LintHints::default()
    };

    let report = lint_visible(PayloadRole::Review, &body, hints);

    assert!(report.is_pass(), "{:?}", report.findings);
}

#[test]
fn visible_lint_review_requires_a_structural_disposition_column() {
    let missing_column = "## Review Evidence\n\n\
        - Profile: tracking\n\
        - Decision: approve\n\n\
        | ID | Severity | Summary |\n\
        | --- | --- | --- |\n\
        | F1 | minor | fixed wording in summary |\n";
    let invalid_value = "## Review Evidence\n\n\
        - Profile: tracking\n\
        - Decision: approve\n\n\
        | ID | Severity | Disposition | Summary |\n\
        | --- | --- | --- | --- |\n\
        | F1 | minor | TBD | fixed wording in summary |\n";
    let hints = LintHints {
        review_has_findings: true,
        ..LintHints::default()
    };

    for body in [missing_column, invalid_value] {
        let report = lint_visible(PayloadRole::Review, body, hints);
        assert!(
            report.codes().contains(&codes::REVIEW_MISSING_DISPOSITION),
            "body={body:?} codes={:?}",
            report.codes()
        );
    }
}

#[test]
fn visible_lint_review_rejects_malformed_findings_separator() {
    let body = "## Review Evidence\n\n\
        - Profile: tracking\n\
        - Decision: approve\n\n\
        | ID | Severity | Disposition | Summary |\n\
        | --- | | | |\n\
        | F1 | minor | fixed | ordinary summary |\n";
    let hints = LintHints {
        review_has_findings: true,
        ..LintHints::default()
    };

    let report = lint_visible(PayloadRole::Review, body, hints);

    assert!(
        report.codes().contains(&codes::REVIEW_MISSING_DISPOSITION),
        "codes={:?}",
        report.codes()
    );
}

#[test]
fn visible_lint_review_ignores_hidden_findings_tables() {
    let fenced = "## Review Evidence\n\n\
        - Profile: tracking\n\
        - Decision: approve\n\n\
        ```markdown\n\
        | ID | Severity | Disposition | Summary |\n\
        | --- | --- | --- | --- |\n\
        | F1 | minor | fixed | hidden example |\n\
        ```\n";
    let commented = "## Review Evidence\n\n\
        - Profile: tracking\n\
        - Decision: approve\n\n\
        <!--\n\
        | ID | Severity | Disposition | Summary |\n\
        | --- | --- | --- | --- |\n\
        | F1 | minor | fixed | hidden example |\n\
        -->\n";
    let indented = concat!(
        "## Review Evidence\n\n",
        "- Profile: tracking\n",
        "- Decision: approve\n\n",
        "    | ID | Severity | Disposition | Summary |\n",
        "    | --- | --- | --- | --- |\n",
        "    | F1 | minor | fixed | hidden example |\n",
    );
    let shorter_backtick_closer = concat!(
        "## Review Evidence\n\n",
        "- Profile: tracking\n",
        "- Decision: approve\n\n",
        "````markdown\n",
        "```\n",
        "| ID | Severity | Disposition | Summary |\n",
        "| --- | --- | --- | --- |\n",
        "| F1 | minor | fixed | hidden example |\n",
        "````\n",
    );
    let shorter_tilde_closer = concat!(
        "## Review Evidence\n\n",
        "- Profile: tracking\n",
        "- Decision: approve\n\n",
        "~~~~markdown\n",
        "~~~\n",
        "| ID | Severity | Disposition | Summary |\n",
        "| --- | --- | --- | --- |\n",
        "| F1 | minor | fixed | hidden example |\n",
        "~~~~\n",
    );
    let trailing_text_false_closer = concat!(
        "## Review Evidence\n\n",
        "- Profile: tracking\n",
        "- Decision: approve\n\n",
        "```markdown\n",
        "```not a closer\n",
        "| ID | Severity | Disposition | Summary |\n",
        "| --- | --- | --- | --- |\n",
        "| F1 | minor | fixed | hidden example |\n",
        "```\n",
    );
    let indented_false_closer = concat!(
        "## Review Evidence\n\n",
        "- Profile: tracking\n",
        "- Decision: approve\n\n",
        "```markdown\n",
        "    ```\n",
        "| ID | Severity | Disposition | Summary |\n",
        "| --- | --- | --- | --- |\n",
        "| F1 | minor | fixed | hidden example |\n",
        "```\n",
    );
    let tab_indented_false_closer = concat!(
        "## Review Evidence\n\n",
        "- Profile: tracking\n",
        "- Decision: approve\n\n",
        "~~~markdown\n",
        "\t~~~\n",
        "| ID | Severity | Disposition | Summary |\n",
        "| --- | --- | --- | --- |\n",
        "| F1 | minor | fixed | hidden example |\n",
        "~~~\n",
    );
    let chained_comments = concat!(
        "## Review Evidence\n\n",
        "- Profile: tracking\n",
        "- Decision: approve\n\n",
        "<!-- closed --> <!--\n",
        "| ID | Severity | Disposition | Summary |\n",
        "| --- | --- | --- | --- |\n",
        "| F1 | minor | fixed | hidden example |\n",
        "-->\n",
    );
    let hints = LintHints {
        review_has_findings: true,
        ..LintHints::default()
    };

    for body in [
        fenced,
        commented,
        indented,
        shorter_backtick_closer,
        shorter_tilde_closer,
        trailing_text_false_closer,
        indented_false_closer,
        tab_indented_false_closer,
        chained_comments,
    ] {
        let report = lint_visible(PayloadRole::Review, body, hints);
        assert!(
            report.codes().contains(&codes::REVIEW_MISSING_DISPOSITION),
            "body={body:?} codes={:?}",
            report.codes()
        );
    }
}

#[test]
fn visible_lint_review_rejects_non_ascii_fence_closers() {
    let hints = LintHints {
        review_has_findings: true,
        ..LintHints::default()
    };

    for suffix in ["\u{00a0}", "\t"] {
        let body = format!(
            "## Review Evidence\n\n\
             - Profile: tracking\n\
             - Decision: approve\n\n\
             ```markdown\n\
             ```{suffix}\n\
             | ID | Severity | Disposition | Summary |\n\
             | --- | --- | --- | --- |\n\
             | F1 | minor | fixed | hidden example |\n\
             ```\n"
        );
        let report = lint_visible(PayloadRole::Review, &body, hints);

        assert!(
            report.codes().contains(&codes::REVIEW_MISSING_DISPOSITION),
            "suffix={suffix:?} codes={:?}",
            report.codes()
        );
    }
}

#[test]
fn visible_lint_review_accepts_ascii_fence_closer_whitespace() {
    let hints = LintHints {
        review_has_findings: true,
        ..LintHints::default()
    };

    for closer in ["```\n", "```   \n"] {
        let body = format!(
            "## Review Evidence\n\n\
             - Profile: tracking\n\
             - Decision: approve\n\n\
             ```markdown\n\
             hidden example\n\
             {closer}\
             | ID | Severity | Disposition | Summary |\n\
             | --- | --- | --- | --- |\n\
             | F1 | minor | fixed | visible finding |\n"
        );

        let report = lint_visible(PayloadRole::Review, &body, hints);

        assert!(report.is_pass(), "closer={closer:?} {:?}", report.findings);
    }
}

#[test]
fn visible_lint_review_rejects_comment_prefixed_table_headers() {
    let body = concat!(
        "## Review Evidence\n\n",
        "- Profile: tracking\n",
        "- Decision: approve\n\n",
        "<!-- prefix -->| ID | Severity | Disposition | Summary |\n",
        "| --- | --- | --- | --- |\n",
        "| F1 | minor | fixed | synthetic table |\n",
    );
    let hints = LintHints {
        review_has_findings: true,
        ..LintHints::default()
    };

    let report = lint_visible(PayloadRole::Review, body, hints);

    assert!(
        report.codes().contains(&codes::REVIEW_MISSING_DISPOSITION),
        "codes={:?}",
        report.codes()
    );
}

#[test]
fn visible_lint_review_rejects_comment_synthesized_table_separators() {
    let body = concat!(
        "## Review Evidence\n\n",
        "- Profile: tracking\n",
        "- Decision: approve\n\n",
        "| ID | Severity | Disposition | Summary |\n",
        "| --- | <!-- hidden --> --- | --- | --- |\n",
        "| F1 | minor | fixed | synthetic table |\n",
    );
    let hints = LintHints {
        review_has_findings: true,
        ..LintHints::default()
    };

    let report = lint_visible(PayloadRole::Review, body, hints);

    assert!(
        report.codes().contains(&codes::REVIEW_MISSING_DISPOSITION),
        "codes={:?}",
        report.codes()
    );
}

#[test]
fn visible_lint_review_ignores_raw_html_block_tables() {
    let hints = LintHints {
        review_has_findings: true,
        ..LintHints::default()
    };

    for (opener, closer) in [("<pre>", "</pre>"), ("<script>", "</script>")] {
        let body = format!(
            "## Review Evidence\n\n\
             - Profile: tracking\n\
             - Decision: approve\n\n\
             {opener}\n\
             | ID | Severity | Disposition | Summary |\n\
             | --- | --- | --- | --- |\n\
             | F1 | minor | fixed | hidden example |\n\
             {closer}\n"
        );

        let report = lint_visible(PayloadRole::Review, &body, hints);

        assert!(
            report.codes().contains(&codes::REVIEW_MISSING_DISPOSITION),
            "opener={opener} codes={:?}",
            report.codes()
        );
    }
}

#[test]
fn visible_lint_review_ignores_comments_after_literal_backticks() {
    let unmatched = concat!(
        "## Review Evidence\n\n",
        "- Profile: tracking\n",
        "- Decision: approve\n\n",
        "` unmatched <!--\n",
        "| ID | Severity | Disposition | Summary |\n",
        "| --- | --- | --- | --- |\n",
        "| F1 | minor | fixed | hidden example |\n",
        "-->\n",
    );
    let escaped = concat!(
        "## Review Evidence\n\n",
        "- Profile: tracking\n",
        "- Decision: approve\n\n",
        "\\` literal <!--\n",
        "| ID | Severity | Disposition | Summary |\n",
        "| --- | --- | --- | --- |\n",
        "| F1 | minor | fixed | hidden example |\n",
        "-->\n",
    );
    let hints = LintHints {
        review_has_findings: true,
        ..LintHints::default()
    };

    for body in [unmatched, escaped] {
        let report = lint_visible(PayloadRole::Review, body, hints);
        assert!(
            report.codes().contains(&codes::REVIEW_MISSING_DISPOSITION),
            "body={body:?} codes={:?}",
            report.codes()
        );
    }
}

#[test]
fn visible_lint_review_stops_code_span_matching_at_comment_blocks() {
    let body = concat!(
        "## Review Evidence\n\n",
        "- Profile: tracking\n",
        "- Decision: approve\n\n",
        "` unmatched\n",
        "<!--\n",
        "hidden ` delimiter\n",
        "| ID | Severity | Disposition | Summary |\n",
        "| --- | --- | --- | --- |\n",
        "| F1 | minor | fixed | hidden example |\n",
        "-->\n",
    );
    let hints = LintHints {
        review_has_findings: true,
        ..LintHints::default()
    };

    let report = lint_visible(PayloadRole::Review, body, hints);

    assert!(
        report.codes().contains(&codes::REVIEW_MISSING_DISPOSITION),
        "codes={:?}",
        report.codes()
    );
}

#[test]
fn visible_lint_review_accepts_multiline_code_span_comment_token() {
    let body = concat!(
        "## Review Evidence\n\n",
        "- Profile: tracking\n",
        "- Decision: approve\n\n",
        "`code span\n",
        "literal <!-- token\n",
        "ends here`\n\n",
        "| ID | Severity | Disposition | Summary |\n",
        "| --- | --- | --- | --- |\n",
        "| F1 | minor | fixed | visible finding |\n",
    );
    let hints = LintHints {
        review_has_findings: true,
        ..LintHints::default()
    };

    let report = lint_visible(PayloadRole::Review, body, hints);

    assert!(report.is_pass(), "{:?}", report.findings);
}

#[test]
fn visible_lint_review_accepts_inline_comment_tokens_in_summary() {
    let hints = LintHints {
        review_has_findings: true,
        ..LintHints::default()
    };

    for summary in [
        "Handle <!-- marker --> safely",
        "Keep the `<!--` literal visible",
    ] {
        let body = render_record_post_comment(
            RecordProfile::Tracking,
            LifecycleCommentKind::Review,
            json!({
                "decision": "approve",
                "lenses": ["testing"],
                "findings": [{
                    "id": "F1",
                    "severity": "minor",
                    "disposition": "fixed",
                    "summary": summary,
                }],
                "outcome_comment_url": "https://example.test/review",
            }),
            None,
            Some("2026-07-12T00:00:00Z"),
        )
        .expect("render review body");

        let report = lint_visible(PayloadRole::Review, &body, hints);
        assert!(report.is_pass(), "{summary}: {:?}", report.findings);
    }
}

#[test]
fn visible_lint_review_rejects_unrelated_disposition_table() {
    let body = "## Review Evidence\n\n\
        - Profile: tracking\n\
        - Decision: approve\n\n\
        | Component | Disposition | Notes |\n\
        | --- | --- | --- |\n\
        | parser | fixed | unrelated status |\n";
    let hints = LintHints {
        review_has_findings: true,
        ..LintHints::default()
    };

    let report = lint_visible(PayloadRole::Review, body, hints);

    assert!(
        report.codes().contains(&codes::REVIEW_MISSING_DISPOSITION),
        "codes={:?}",
        report.codes()
    );
}

#[test]
fn visible_lint_review_accepts_all_supported_disposition_values() {
    let hints = LintHints {
        review_has_findings: true,
        ..LintHints::default()
    };

    for disposition in ["fixed", "residual", "follow-up", "deferred", "no-action"] {
        let body = format!(
            "## Review Evidence\n\n\
             - Profile: tracking\n\
             - Decision: approve\n\n\
             | ID | Severity | Disposition | Summary |\n\
             | --- | --- | --- | --- |\n\
             | F1 | minor | {disposition} | ordinary summary |\n"
        );

        let report = lint_visible(PayloadRole::Review, &body, hints);
        assert!(report.is_pass(), "{disposition}: {:?}", report.findings);
    }
}

#[test]
fn visible_lint_closeout_requires_approval_and_linked_pr() {
    let pass = "## Tracking Issue Closeout\n\n\
        - Profile: tracking\n\
        - Final status: complete\n\
        - Approval: https://example.com/approval\n\n\
        | PR | Merge SHA | Checks |\n\
        | --- | --- | --- |\n\
        | owner/repo#123 | abc123 | pass |\n";
    let no_approval = "## Tracking Issue Closeout\n\n\
        - Profile: tracking\n\
        - Final status: complete\n\n\
        | PR | Merge SHA |\n\
        | --- | --- |\n\
        | owner/repo#1 | x |\n";
    let no_pr = "## Tracking Issue Closeout\n\n\
        - Profile: tracking\n\
        - Final status: complete\n\
        - Approval: approver text\n";
    let no_pr_with_note = "## Tracking Issue Closeout\n\n\
        - Profile: tracking\n\
        - Final status: complete\n\
        - Approval: approver text\n\
        - Linked PRs: none (docs-only)\n";

    let ok = lint_visible(PayloadRole::Closeout, pass, LintHints::default());
    assert!(ok.is_pass(), "{:?}", ok.findings);

    let bad1 = lint_visible(PayloadRole::Closeout, no_approval, LintHints::default());
    assert!(
        bad1.codes().contains(&codes::CLOSEOUT_MISSING_APPROVAL),
        "codes={:?}",
        bad1.codes()
    );

    let bad2 = lint_visible(PayloadRole::Closeout, no_pr, LintHints::default());
    assert!(
        bad2.codes().contains(&codes::CLOSEOUT_MISSING_LINKED_PR),
        "codes={:?}",
        bad2.codes()
    );

    let allow_no_pr = LintHints {
        closeout_has_no_linked_pr_ok: true,
        ..LintHints::default()
    };
    let bad_should_pass = lint_visible(PayloadRole::Closeout, no_pr_with_note, allow_no_pr);
    assert!(
        bad_should_pass.is_pass(),
        "explicit no-PR note should satisfy linked-PR rule: {:?}",
        bad_should_pass.findings
    );
}

#[test]
fn visible_lint_source_and_plan_reject_profile_only() {
    let source_pass = "## Source Snapshot\n\n\
        - Profile: tracking\n\
        - Path: `docs/plans/foo/foo.md`\n\
        - Commit: `abc1234`\n";
    let source_profile_only = "## Source Snapshot\n\n- Profile: tracking\n";

    let ok = lint_visible(PayloadRole::Source, source_pass, LintHints::default());
    assert!(ok.is_pass(), "{:?}", ok.findings);

    let bad = lint_visible(
        PayloadRole::Source,
        source_profile_only,
        LintHints::default(),
    );
    assert!(
        bad.codes().contains(&codes::PROFILE_ONLY),
        "codes={:?}",
        bad.codes()
    );

    let plan_pass = "## Plan Snapshot\n\n\
        - Profile: tracking\n\
        - Path: `docs/plans/foo/foo-plan.md`\n";
    let plan_profile_only = "## Plan Snapshot\n\n- Profile: tracking\n";

    let ok_plan = lint_visible(PayloadRole::Plan, plan_pass, LintHints::default());
    assert!(ok_plan.is_pass(), "{:?}", ok_plan.findings);

    let bad_plan = lint_visible(PayloadRole::Plan, plan_profile_only, LintHints::default());
    assert!(
        bad_plan.codes().contains(&codes::PROFILE_ONLY),
        "codes={:?}",
        bad_plan.codes()
    );
}

#[test]
fn visible_lint_rejects_profile_only_state_body() {
    let body = "## Execution State\n\n- Profile: tracking\n";
    let report = lint_visible(PayloadRole::State, body, LintHints::default());
    // Profile-only state also missing Task Ledger; both codes should be
    // reported so callers can act on either path.
    assert!(report.codes().contains(&codes::PROFILE_ONLY));
    assert!(report.codes().contains(&codes::STATE_MISSING_TASK_LEDGER));
}

#[test]
fn visible_lint_missing_heading_returns_role_specific_code() {
    let bodies: Vec<(PayloadRole, &str, &str)> = vec![
        (
            PayloadRole::Source,
            "- Profile: tracking\n",
            codes::SOURCE_MISSING_HEADING,
        ),
        (
            PayloadRole::Plan,
            "- Profile: tracking\n",
            codes::PLAN_MISSING_HEADING,
        ),
        (
            PayloadRole::State,
            "- Profile: tracking\n",
            codes::STATE_MISSING_HEADING,
        ),
        (
            PayloadRole::Session,
            "- Profile: tracking\n",
            codes::SESSION_MISSING_HEADING,
        ),
        (
            PayloadRole::Validation,
            "- Profile: tracking\n",
            codes::VALIDATION_MISSING_HEADING,
        ),
        (
            PayloadRole::Review,
            "- Profile: tracking\n",
            codes::REVIEW_MISSING_HEADING,
        ),
        (
            PayloadRole::Closeout,
            "- Profile: tracking\n",
            codes::CLOSEOUT_MISSING_HEADING,
        ),
    ];
    for (role_id, body, expected) in bodies {
        let report = lint_visible(role_id, body, LintHints::default());
        assert!(
            report.codes().contains(&expected),
            "role {role_id:?} missing-heading code drift: {:?}",
            report.codes()
        );
    }
}
