use std::fs;

use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use tempfile::TempDir;

use plan_issue_cli::lifecycle_record::{
    self, CheckStatus, FindingDisposition, FindingSeverity, PAYLOAD_SCHEMA_V2, PayloadErrorKind,
    PayloadProfile, PayloadRole, PrLifecycleStatus, ReviewDecision, StateStatus, TaskRowStatus,
    ValidationCommandStatus, ValidationOverall,
};

use crate::common;

/// Build a `plan-issue-record:v2` lifecycle comment body whose payload
/// fence carries the given `data` value. Used by audit + closeout-gate
/// tests in this file to produce comment JSON without re-deriving the
/// fenced payload by hand.
fn v2_comment_body(role: &str, profile: &str, data: Value) -> String {
    let envelope = json!({
        "schema": PAYLOAD_SCHEMA_V2,
        "role": role,
        "profile": profile,
        "data": data,
    });
    let payload = serde_json::to_string(&envelope).expect("serialize payload");
    format!(
        "<!-- plan-issue-record:v2 role={role} profile={profile} -->\n\n```plan-issue-record-payload\n{payload}\n```\n",
    )
}

fn json_stdout(out: &common::CmdOut) -> Value {
    serde_json::from_str(&out.stdout).expect("json stdout")
}

#[test]
fn issue_backed_lifecycle_render_comment_emits_v2_canonical_marker() {
    let tmp = TempDir::new().expect("tmp");
    let content = tmp.path().join("source.md");
    let rendered = tmp.path().join("comment.md");
    fs::write(
        &content,
        "# Source\n\nQuoted markers inside details are ignored by the v2 parser:\n<!-- plan-issue-record:v2 role=state profile=tracking -->\n",
    )
    .expect("write content");

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "render-comment",
        "--kind",
        "source",
        "--path",
        "docs/source.md",
        "--commit",
        "abc123",
        "--content-file",
        content.to_str().expect("content path"),
        "--out",
        rendered.to_str().expect("rendered path"),
    ]);

    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let body = fs::read_to_string(&rendered).expect("read rendered");
    assert!(
        body.starts_with("<!-- plan-issue-record:v2 role=source profile=tracking -->"),
        "{body}"
    );
    assert!(body.contains("## Source Snapshot"), "{body}");
    assert!(body.contains("<details>"), "{body}");
    assert!(!body.contains("## Task Decomposition"), "{body}");
    // The v3 lifecycle no longer emits any pre-v2 marker family from
    // `record render-comment`, even when the rendered content contains
    // quoted v1 markers.
    assert!(
        !body.contains("plan-tracking-issue:snapshot:v1"),
        "v2 render must not emit v1 outer marker: {body}"
    );

    let payload = json_stdout(&out);
    assert_eq!(payload["command"], "record.render-comment");
    assert_eq!(payload["payload"]["result"]["kind"], "source");
}

#[test]
fn lifecycle_record_audit_returns_typed_v2_evidence_and_ignores_v1_markers() {
    let tmp = TempDir::new().expect("tmp");
    let body = tmp.path().join("body.md");
    let comments = tmp.path().join("comments.json");
    fs::write(
        &body,
        "## Current Dashboard\n\n## Durable Record\n\nNo task table here.\n",
    )
    .expect("write body");

    let payload = json!({
        "comments": [
            {
                "url": "https://github.com/owner/repo/issues/1#issuecomment-source",
                "body": v2_comment_body(
                    "source",
                    "tracking",
                    json!({"path": "docs/plans/example/example-discussion-source.md", "commit": "abc1234"}),
                ),
            },
            {
                "url": "https://github.com/owner/repo/issues/1#issuecomment-plan",
                "body": v2_comment_body(
                    "plan",
                    "tracking",
                    json!({"path": "docs/plans/example/example-plan.md", "commit": "abc1234"}),
                ),
            },
            {
                "url": "https://github.com/owner/repo/issues/1#issuecomment-state",
                "body": v2_comment_body(
                    "state",
                    "tracking",
                    json!({"status": "complete", "target_scope": "v3", "tasks": [], "prs": []}),
                ),
            },
            {
                "url": "https://github.com/owner/repo/issues/1#issuecomment-v1-marker",
                "body": "<!-- execute-from-tracking-issue:state:v1 -->\n\n## Execution State\n\n- Status: complete\n",
            }
        ]
    });
    fs::write(&comments, payload.to_string()).expect("write comments");

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "audit",
        "--profile",
        "tracking",
        "--body-file",
        body.to_str().expect("body path"),
        "--comments-json",
        comments.to_str().expect("comments path"),
    ]);

    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let payload = json_stdout(&out);
    let audit = &payload["payload"]["result"]["audit"];
    assert_eq!(audit["recognized_count"], 3);
    assert_eq!(
        audit["evidence"]["source"]["url"],
        "https://github.com/owner/repo/issues/1#issuecomment-source"
    );
    assert_eq!(
        audit["evidence"]["plan"]["url"],
        "https://github.com/owner/repo/issues/1#issuecomment-plan"
    );
    assert_eq!(audit["evidence"]["state"]["status"], "complete");
    assert_eq!(
        audit["evidence"]["state"]["payload"]["schema"],
        PAYLOAD_SCHEMA_V2
    );
    assert!(audit["evidence"]["validation"].is_null());
    assert_eq!(audit["missing_required"].as_array().unwrap().len(), 0);
    assert_eq!(
        audit["unsupported_markers"][0]["marker_prefix"],
        "execute-from-tracking-issue"
    );
    assert_eq!(audit["body_sections"]["task_decomposition"], false);
}

#[test]
fn lifecycle_record_closeout_gate_passes_with_v2_payloads() {
    let tmp = TempDir::new().expect("tmp");
    let body = tmp.path().join("body.md");
    let comments = tmp.path().join("comments.json");
    fs::write(
        &body,
        "## Final Dashboard\n\n## Durable Record\n\n## Closeout Checks\n",
    )
    .expect("write body");

    let payload = json!([
        {
            "url": "https://github.com/owner/repo/issues/1#issuecomment-source",
            "body": v2_comment_body(
                "source",
                "tracking",
                json!({"path": "docs/plans/example/example-discussion-source.md", "commit": "abc1234"}),
            ),
        },
        {
            "url": "https://github.com/owner/repo/issues/1#issuecomment-plan",
            "body": v2_comment_body(
                "plan",
                "tracking",
                json!({"path": "docs/plans/example/example-plan.md", "commit": "abc1234"}),
            ),
        },
        {
            "url": "https://github.com/owner/repo/issues/1#issuecomment-state",
            "body": v2_comment_body(
                "state",
                "tracking",
                json!({"status": "complete", "target_scope": "v3", "tasks": [], "prs": []}),
            ),
        },
        {
            "url": "https://github.com/owner/repo/issues/1#issuecomment-session",
            "body": v2_comment_body("session", "tracking", json!({"summary": "Final session"})),
        },
        {
            "url": "https://github.com/owner/repo/issues/1#issuecomment-validation",
            "body": v2_comment_body("validation", "tracking", json!({"overall": "pass", "commands": []})),
        }
    ]);
    fs::write(&comments, payload.to_string()).expect("write comments");

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "closeout-gate",
        "--profile",
        "tracking",
        "--require-complete",
        "--require-session",
        "--require-validation",
        "--approval",
        "explicit close approval flag",
        "--body-file",
        body.to_str().expect("body path"),
        "--comments-json",
        comments.to_str().expect("comments path"),
    ]);

    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let payload = json_stdout(&out);
    let result = &payload["payload"]["result"];
    assert_eq!(result["ready"], true);
    assert!(
        result["checks_markdown"]
            .as_str()
            .unwrap()
            .contains("| close approval | pass | explicit close approval flag |"),
        "{}",
        result["checks_markdown"]
    );
}

#[test]
fn lifecycle_record_closeout_gate_filters_linked_prs_by_profile_v2() {
    let tmp = TempDir::new().expect("tmp");
    let body = tmp.path().join("body.md");
    let comments = tmp.path().join("comments.json");
    fs::write(
        &body,
        "## Final Dashboard\n\n## Durable Record\n\n## Closeout Checks\n",
    )
    .expect("write body");

    let payload = json!([
        {
            "url": "https://github.com/owner/repo/issues/1#issuecomment-source",
            "body": v2_comment_body(
                "source",
                "dispatch",
                json!({"path": "docs/plans/example/example-discussion-source.md", "commit": "abc1234"}),
            ),
        },
        {
            "url": "https://github.com/owner/repo/issues/1#issuecomment-plan",
            "body": v2_comment_body(
                "plan",
                "dispatch",
                json!({"path": "docs/plans/example/example-plan.md", "commit": "abc1234"}),
            ),
        },
        {
            "url": "https://github.com/owner/repo/issues/1#issuecomment-state",
            "body": v2_comment_body(
                "state",
                "dispatch",
                json!({"status": "complete", "target_scope": "v3", "tasks": [], "prs": []}),
            ),
        },
        {
            "url": "https://github.com/owner/repo/issues/1#issuecomment-session",
            "body": v2_comment_body("session", "dispatch", json!({"summary": "Session"})),
        },
        {
            "url": "https://github.com/owner/repo/issues/1#issuecomment-validation",
            "body": v2_comment_body("validation", "dispatch", json!({"overall": "pass", "commands": []})),
        },
        {
            "url": "https://github.com/owner/repo/issues/1#issuecomment-review",
            "body": v2_comment_body("review", "dispatch", json!({"decision": "approve", "lenses": ["testing"], "findings": []})),
        },
        {
            "url": "https://github.com/owner/repo/issues/1#issuecomment-tracking",
            "body": v2_comment_body("session", "tracking", json!({"summary": "PR #999"})),
        }
    ]);
    fs::write(&comments, payload.to_string()).expect("write comments");

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "closeout-gate",
        "--profile",
        "dispatch",
        "--require-complete",
        "--require-session",
        "--require-validation",
        "--require-review",
        "--approval",
        "explicit dispatch approval",
        "--linked-pr",
        "#999",
        "--body-file",
        body.to_str().expect("body path"),
        "--comments-json",
        comments.to_str().expect("comments path"),
    ]);

    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let payload = json_stdout(&out);
    let result = &payload["payload"]["result"];
    // The dispatch-profile filter rejects the tracking-profile session
    // payload, so `#999` (which appears only in the tracking session
    // payload) is not present in any dispatch-profile evidence text.
    assert_eq!(result["ready"], false);
    assert!(
        result["checks_markdown"]
            .as_str()
            .unwrap()
            .contains("| linked PRs | fail | linked PRs not found in lifecycle evidence: #999 |"),
        "{}",
        result["checks_markdown"]
    );
}

#[test]
fn issue_backed_lifecycle_build_dispatch_ledger_uses_plan_tooling_without_task_decomposition() {
    let tmp = TempDir::new().expect("tmp");
    let rendered = tmp.path().join("dispatch-ledger.md");
    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "build-dispatch-ledger",
        "--plan",
        "crates/plan-issue-cli/tests/fixtures/plans/plan-issue-rust-cli-full-delivery-plan.md",
        "--pr-grouping",
        "per-sprint",
        "--out",
        rendered.to_str().expect("rendered path"),
    ]);

    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let ledger = fs::read_to_string(&rendered).expect("read ledger");
    assert!(ledger.starts_with("## Dispatch Ledger"), "{ledger}");
    assert!(
        ledger.contains("| Task | Summary | Sprint | Owner/Subagent |"),
        "{ledger}"
    );
    assert!(!ledger.contains("## Task Decomposition"), "{ledger}");

    let payload = json_stdout(&out);
    assert_eq!(payload["command"], "record.build-dispatch-ledger");
    assert!(
        payload["payload"]["result"]["record_count"]
            .as_u64()
            .unwrap()
            > 0
    );
}

// --- Sprint 1 Task 1.3: lifecycle_record_structured_payloads ---

fn payload_comment(body: &str) -> String {
    format!(
        "<!-- plan-issue-record:v2 role=state profile=tracking -->\n\n## State\n\n```plan-issue-record-payload\n{body}\n```\n",
    )
}

#[test]
fn lifecycle_record_structured_payloads_state_round_trip() {
    let body = format!(
        "{{\n  \"schema\": \"{PAYLOAD_SCHEMA_V2}\",\n  \"role\": \"state\",\n  \"profile\": \"tracking\",\n  \"updated_at\": \"2026-05-23T09:00:00Z\",\n  \"data\": {{\n    \"status\": \"in-progress\",\n    \"target_scope\": \"Plan-Issue v3 sprint 1\",\n    \"current\": \"Sprint 1 implementation\",\n    \"next_action\": \"Land spec PR\",\n    \"tasks\": [\n      {{\"id\": \"1.1\", \"status\": \"done\", \"title\": \"Spec\"}},\n      {{\"id\": \"1.2\", \"status\": \"in-progress\"}}\n    ],\n    \"prs\": [\n      {{\"ref\": \"sympoies/nils-cli#500\", \"url\": \"https://github.com/sympoies/nils-cli/pull/500\", \"status\": \"open\"}}\n    ],\n    \"blockers\": [],\n    \"links\": {{\"source\": \"https://github.com/sympoies/nils-cli/issues/448#issuecomment-100\"}}\n  }}\n}}"
    );
    let comment = payload_comment(&body);

    let envelope = lifecycle_record::extract_payload(&comment).expect("extract state payload");
    assert_eq!(envelope.role, PayloadRole::State);
    assert_eq!(envelope.profile, PayloadProfile::Tracking);
    assert_eq!(envelope.updated_at.as_deref(), Some("2026-05-23T09:00:00Z"));

    let state = envelope.parse_state().expect("parse state data");
    assert_eq!(state.status, Some(StateStatus::InProgress));
    assert_eq!(
        state.target_scope.as_deref(),
        Some("Plan-Issue v3 sprint 1")
    );
    assert_eq!(state.tasks.len(), 2);
    assert_eq!(state.tasks[0].id, "1.1");
    assert_eq!(state.tasks[0].status, TaskRowStatus::Done);
    assert_eq!(state.tasks[1].status, TaskRowStatus::InProgress);
    assert_eq!(state.prs.len(), 1);
    assert_eq!(state.prs[0].pr_ref, "sympoies/nils-cli#500");
    assert_eq!(state.prs[0].status, PrLifecycleStatus::Open);
    assert_eq!(
        state.links.get("source").map(String::as_str),
        Some("https://github.com/sympoies/nils-cli/issues/448#issuecomment-100"),
    );
}

#[test]
fn lifecycle_record_structured_payloads_validation_command_rows() {
    let body = format!(
        "{{\n  \"schema\": \"{PAYLOAD_SCHEMA_V2}\",\n  \"role\": \"validation\",\n  \"profile\": \"tracking\",\n  \"data\": {{\n    \"overall\": \"pass\",\n    \"commands\": [\n      {{\"command\": \"cargo test -p nils-plan-issue-cli\", \"status\": \"pass\"}},\n      {{\"command\": \"bash scripts/ci/nils-cli-checks-entrypoint.sh\", \"status\": \"pass\", \"evidence\": \"out/log.txt\"}}\n    ],\n    \"waivers\": []\n  }}\n}}"
    );
    let comment = payload_comment(&body);

    let envelope = lifecycle_record::extract_payload(&comment).expect("extract validation payload");
    let validation = envelope.parse_validation().expect("parse validation data");
    assert_eq!(validation.overall, ValidationOverall::Pass);
    assert_eq!(validation.commands.len(), 2);
    for cmd in &validation.commands {
        assert_eq!(cmd.status, ValidationCommandStatus::Pass);
    }
    assert_eq!(
        validation.commands[1].evidence.as_deref(),
        Some("out/log.txt")
    );
    assert!(validation.waivers.is_empty());
}

#[test]
fn lifecycle_record_structured_payloads_review_findings_and_decision() {
    let body = format!(
        "{{\n  \"schema\": \"{PAYLOAD_SCHEMA_V2}\",\n  \"role\": \"review\",\n  \"profile\": \"tracking\",\n  \"data\": {{\n    \"decision\": \"approve\",\n    \"lenses\": [\"testing\", \"maintainability\"],\n    \"findings\": [\n      {{\"id\": \"F1\", \"severity\": \"minor\", \"disposition\": \"fixed\", \"summary\": \"Fence parser handles whitespace\"}},\n      {{\"id\": \"F2\", \"severity\": \"nit\", \"disposition\": \"no-action\", \"summary\": \"Comment wording\"}}\n    ],\n    \"outcome_comment_url\": \"https://github.com/sympoies/nils-cli/pull/500#issuecomment-200\"\n  }}\n}}"
    );
    let comment = payload_comment(&body);

    let envelope = lifecycle_record::extract_payload(&comment).expect("extract review payload");
    let review = envelope.parse_review().expect("parse review data");
    assert_eq!(review.decision, ReviewDecision::Approve);
    assert_eq!(review.lenses, vec!["testing", "maintainability"]);
    assert_eq!(review.findings.len(), 2);
    assert_eq!(review.findings[0].severity, FindingSeverity::Minor);
    assert_eq!(review.findings[0].disposition, FindingDisposition::Fixed);
    assert_eq!(review.findings[1].severity, FindingSeverity::Nit);
    assert_eq!(review.findings[1].disposition, FindingDisposition::NoAction);
    assert_eq!(
        review.outcome_comment_url.as_deref(),
        Some("https://github.com/sympoies/nils-cli/pull/500#issuecomment-200"),
    );
}

#[test]
fn lifecycle_record_structured_payloads_closeout_final_checks_and_merge_evidence() {
    let body = format!(
        "{{\n  \"schema\": \"{PAYLOAD_SCHEMA_V2}\",\n  \"role\": \"closeout\",\n  \"profile\": \"tracking\",\n  \"data\": {{\n    \"final_status\": \"complete\",\n    \"approval\": {{\"comment_url\": \"https://example/approve\", \"approver\": \"graysurf\"}},\n    \"linked_prs\": [\n      {{\"ref\": \"sympoies/nils-cli#500\", \"url\": \"https://github.com/sympoies/nils-cli/pull/500\", \"merge_sha\": \"abcd1234\", \"checks\": \"pass\"}},\n      {{\"ref\": \"sympoies/nils-cli#501\", \"merge_sha\": \"5678ef\", \"checks\": \"pass\"}}\n    ],\n    \"final_validation_url\": \"https://example/validation\"\n  }}\n}}"
    );
    let comment = payload_comment(&body);

    let envelope = lifecycle_record::extract_payload(&comment).expect("extract closeout payload");
    let closeout = envelope.parse_closeout().expect("parse closeout data");
    assert_eq!(closeout.final_status, "complete");
    assert_eq!(closeout.approval.approver.as_deref(), Some("graysurf"));
    assert_eq!(closeout.linked_prs.len(), 2);
    for pr in &closeout.linked_prs {
        assert!(pr.merge_sha.is_some(), "merge_sha required: {pr:?}");
        assert_eq!(pr.checks, CheckStatus::Pass);
    }
    assert_eq!(
        closeout.final_validation_url.as_deref(),
        Some("https://example/validation"),
    );
}

#[test]
fn lifecycle_record_structured_payloads_reject_schema_mismatch() {
    let body = "{\n  \"schema\": \"plan-issue-record.payload.v1\",\n  \"role\": \"state\",\n  \"profile\": \"tracking\",\n  \"data\": {}\n}";
    let comment = payload_comment(body);

    let err = lifecycle_record::extract_payload(&comment).expect_err("schema mismatch");
    assert_eq!(err.kind, PayloadErrorKind::SchemaMismatch);
    assert!(
        err.message.contains("plan-issue-record.payload.v2"),
        "{}",
        err.message,
    );
}

#[test]
fn lifecycle_record_structured_payloads_reject_missing_fence() {
    let comment = "<!-- plan-issue-record:v2 role=state profile=tracking -->\n\nNo payload here.\n";
    let err = lifecycle_record::extract_payload(comment).expect_err("no fence");
    assert_eq!(err.kind, PayloadErrorKind::NoFence);
}

#[test]
fn lifecycle_record_structured_payloads_reject_invalid_json() {
    let comment = "```plan-issue-record-payload\n{ not json }\n```\n";
    let err = lifecycle_record::extract_payload(comment).expect_err("invalid json");
    assert_eq!(err.kind, PayloadErrorKind::InvalidJson);
}

// --- Sprint 2 Tasks 2.1-2.3 acceptance tests ---

#[test]
fn lifecycle_record_audit_latest_marker_per_role_wins_by_timestamp() {
    let earlier = v2_comment_body(
        "state",
        "tracking",
        json!({"status": "in-progress", "target_scope": "v3"}),
    );
    let later = v2_comment_body(
        "state",
        "tracking",
        json!({"status": "complete", "target_scope": "v3"}),
    );
    let comments = json!([
        {
            "url": "https://github.com/owner/repo/issues/1#issuecomment-state-1",
            "body": earlier,
            "created_at": "2026-05-20T10:00:00Z",
        },
        {
            "url": "https://github.com/owner/repo/issues/1#issuecomment-state-2",
            "body": later,
            "created_at": "2026-05-23T10:00:00Z",
        }
    ]);

    let tmp = TempDir::new().expect("tmp");
    let comments_path = tmp.path().join("comments.json");
    fs::write(&comments_path, comments.to_string()).expect("write comments");

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "audit",
        "--comments-json",
        comments_path.to_str().expect("path"),
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let payload = json_stdout(&out);
    let audit = &payload["payload"]["result"]["audit"];
    assert_eq!(audit["evidence"]["state"]["status"], "complete");
    assert_eq!(
        audit["evidence"]["state"]["url"],
        "https://github.com/owner/repo/issues/1#issuecomment-state-2"
    );
}

#[test]
fn lifecycle_record_audit_reports_stable_missing_required_codes() {
    let tmp = TempDir::new().expect("tmp");
    let comments_path = tmp.path().join("comments.json");
    let comments = json!({
        "comments": [
            {
                "url": "https://github.com/owner/repo/issues/1#issuecomment-source",
                "body": v2_comment_body(
                    "source",
                    "tracking",
                    json!({"path": "docs/plans/example/example-discussion-source.md", "commit": "abc"}),
                ),
            }
        ]
    });
    fs::write(&comments_path, comments.to_string()).expect("write comments");

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "audit",
        "--comments-json",
        comments_path.to_str().expect("path"),
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let payload = json_stdout(&out);
    let audit = &payload["payload"]["result"]["audit"];
    let missing = audit["missing_required"]
        .as_array()
        .expect("missing_required array")
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    assert_eq!(missing, vec!["plan-missing", "state-missing"]);
}

#[test]
fn lifecycle_record_dashboard_renders_from_audit_without_explicit_urls() {
    use lifecycle_record::render_dashboard_from_audit;

    let comments = json!([
        {
            "url": "https://github.com/owner/repo/issues/1#issuecomment-source",
            "body": v2_comment_body(
                "source",
                "tracking",
                json!({"path": "docs/plans/example/example-discussion-source.md", "commit": "abc"}),
            ),
        },
        {
            "url": "https://github.com/owner/repo/issues/1#issuecomment-plan",
            "body": v2_comment_body(
                "plan",
                "tracking",
                json!({"path": "docs/plans/example/example-plan.md", "commit": "abc"}),
            ),
        },
        {
            "url": "https://github.com/owner/repo/issues/1#issuecomment-state",
            "body": v2_comment_body(
                "state",
                "tracking",
                json!({
                    "status": "in-progress",
                    "target_scope": "v3 lifecycle rewrite",
                    "current": "Sprint 2 PR",
                    "next_action": "Land marker collapse",
                    "tasks": [],
                    "prs": [{"ref": "owner/repo#500", "url": "https://example/pull/500", "status": "open"}]
                }),
            ),
        }
    ]);
    let audit =
        lifecycle_record::audit_record(None, &comments.to_string(), None).expect("audit ok");

    let rendered = render_dashboard_from_audit(
        &audit,
        Some("Plan-Issue Lifecycle v3"),
        Some("https://github.com/owner/repo/issues/1"),
    );

    assert!(rendered.contains("## Current Dashboard"), "{rendered}");
    assert!(
        rendered.contains("- Target scope: v3 lifecycle rewrite"),
        "{rendered}"
    );
    assert!(
        rendered.contains("- Linked PRs: https://example/pull/500"),
        "{rendered}"
    );
    assert!(
        rendered.contains(
            "- Source snapshot: [source snapshot](https://github.com/owner/repo/issues/1#issuecomment-source)"
        ),
        "{rendered}"
    );
    assert!(
        rendered.contains("- Closeout comment: pending"),
        "{rendered}"
    );
}

#[test]
fn lifecycle_record_dashboard_renders_final_when_state_complete() {
    use lifecycle_record::render_dashboard_from_audit;

    let comments = json!([
        {
            "url": "https://github.com/owner/repo/issues/1#issuecomment-source",
            "body": v2_comment_body("source", "tracking", json!({"path": "x", "commit": "abc"})),
        },
        {
            "url": "https://github.com/owner/repo/issues/1#issuecomment-plan",
            "body": v2_comment_body("plan", "tracking", json!({"path": "y", "commit": "abc"})),
        },
        {
            "url": "https://github.com/owner/repo/issues/1#issuecomment-state",
            "body": v2_comment_body(
                "state",
                "tracking",
                json!({"status": "complete", "target_scope": "v3", "tasks": [], "prs": []}),
            ),
        }
    ]);
    let audit =
        lifecycle_record::audit_record(None, &comments.to_string(), None).expect("audit ok");

    let rendered = render_dashboard_from_audit(&audit, None, None);
    assert!(
        rendered.contains("## Final Dashboard"),
        "expected Final Dashboard heading: {rendered}"
    );
    assert!(!rendered.contains("## Current Dashboard"), "{rendered}");
}

#[test]
fn lifecycle_record_dashboard_render_is_idempotent_for_same_audit() {
    use lifecycle_record::render_dashboard_from_audit;

    let comments = json!([
        {
            "url": "https://github.com/owner/repo/issues/1#issuecomment-state",
            "body": v2_comment_body(
                "state",
                "tracking",
                json!({"status": "in-progress", "target_scope": "v3", "current": "step", "next_action": "do", "tasks": [], "prs": []}),
            ),
        }
    ]);
    let audit =
        lifecycle_record::audit_record(None, &comments.to_string(), None).expect("audit ok");

    let first = render_dashboard_from_audit(&audit, None, None);
    let second = render_dashboard_from_audit(&audit, None, None);
    assert_eq!(first, second);
}

#[test]
fn lifecycle_record_structured_payloads_reject_role_decode_mismatch() {
    let body = format!(
        "{{\n  \"schema\": \"{PAYLOAD_SCHEMA_V2}\",\n  \"role\": \"session\",\n  \"profile\": \"tracking\",\n  \"data\": {{\"summary\": \"x\"}}\n}}"
    );
    let comment = payload_comment(&body);
    let envelope = lifecycle_record::extract_payload(&comment).expect("extract session payload");
    let err = envelope.parse_state().expect_err("state decode mismatch");
    assert_eq!(err.kind, PayloadErrorKind::SchemaMismatch);
    assert!(err.message.contains("state"), "{}", err.message);
}
