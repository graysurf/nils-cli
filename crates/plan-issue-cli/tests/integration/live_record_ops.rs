use std::fs;
use std::path::Path;

use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use tempfile::TempDir;

use nils_test_support::StubBinDir;
use nils_test_support::cmd::CmdOptions;

use crate::common;

const PAYLOAD_SCHEMA_V2: &str = "plan-issue-record.payload.v2";
const PAYLOAD_FENCE_INFO: &str = "plan-issue-record-payload";

/// Build an older `plan-issue-record:v2` comment body with a visible payload
/// fence carrying `data` for the given role/profile.
fn v2_comment_body(role: &str, profile: &str, data: Value) -> String {
    let envelope = json!({
        "schema": PAYLOAD_SCHEMA_V2,
        "role": role,
        "profile": profile,
        "data": data,
    });
    let payload = serde_json::to_string(&envelope).expect("payload json");
    format!(
        "<!-- plan-issue-record:v2 role={role} profile={profile} -->\n\n```{PAYLOAD_FENCE_INFO}\n{payload}\n```\n",
    )
}

fn parse_json(stdout: &str) -> Value {
    serde_json::from_str(stdout).expect("stdout is valid JSON")
}

fn write_fixture_files(dir: &Path, body: &str, comments: &Value) {
    fs::write(dir.join("issue-body.md"), body).expect("write fixture body");
    fs::write(
        dir.join("comments.json"),
        serde_json::to_string(comments).expect("comments json"),
    )
    .expect("write fixture comments");
}

fn write_pr_fixture(dir: &Path, repo: &str, pr: u64, value: Value) {
    let prs = dir.join("prs");
    fs::create_dir_all(&prs).expect("create prs dir");
    let slug = repo.replace('/', "__");
    fs::write(
        prs.join(format!("{slug}__{pr}.json")),
        serde_json::to_string(&value).expect("pr json"),
    )
    .expect("write pr fixture");
}

fn audit_single_comment_body(body: &str) -> Value {
    let tmp = TempDir::new().expect("tempdir");
    let comments_json = tmp.path().join("comments.json");
    fs::write(
        &comments_json,
        json!({
            "comments": [
                {"body": body, "url": "https://github.com/owner/repo/issues/1#issuecomment-record"}
            ]
        })
        .to_string(),
    )
    .expect("write comments json");

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "audit",
        "--comments-json",
        comments_json.to_str().expect("comments path"),
        "--profile",
        "tracking",
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    parse_json(&out.stdout)["payload"]["result"]["audit"].clone()
}

#[test]
fn record_post_state_with_payload_file_renders_v2_marker_in_dry_run() {
    let tmp = TempDir::new().expect("tempdir");
    let payload = tmp.path().join("state.json");
    fs::write(
        &payload,
        json!({
            "status": "in-progress",
            "target_scope": "scope",
            "tasks": [{"id": "1.1", "status": "done", "title": "x"}],
            "prs": [],
            "blockers": [],
            "links": {}
        })
        .to_string(),
    )
    .expect("write payload");

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "post",
        "--issue",
        "448",
        "--kind",
        "state",
        "--payload-file",
        payload.to_str().expect("payload str"),
    ]);

    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let parsed = parse_json(&out.stdout);
    let result = &parsed["payload"]["result"];
    assert_eq!(result["operation"], "record.post");
    assert_eq!(result["kind"], "state");
    let body = result["comment_body"]
        .as_str()
        .expect("comment_body in dry-run");
    assert!(
        body.starts_with("<!-- plan-issue-record:v2 role=state profile=tracking -->"),
        "{body}"
    );
    assert!(
        !body.contains(&format!("```{PAYLOAD_FENCE_INFO}")),
        "{body}"
    );
    assert!(
        body.contains("<!-- plan-issue-record-payload:hex:"),
        "{body}"
    );
}

#[test]
fn record_post_state_summary_file_is_rendered_in_dry_run() {
    let tmp = TempDir::new().expect("tempdir");
    let payload = tmp.path().join("state.json");
    let summary = tmp.path().join("summary.md");
    fs::write(
        &payload,
        json!({
            "status": "in-progress",
            "target_scope": "summary surface",
            "tasks": [],
            "prs": [],
            "blockers": [],
            "links": {}
        })
        .to_string(),
    )
    .expect("write payload");
    fs::write(
        &summary,
        "- Updated runtime-kit skills to the v3 surface.\n",
    )
    .expect("write summary");

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "post",
        "--issue",
        "448",
        "--kind",
        "state",
        "--payload-file",
        payload.to_str().expect("payload str"),
        "--summary-file",
        summary.to_str().expect("summary str"),
    ]);

    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let parsed = parse_json(&out.stdout);
    let body = parsed["payload"]["result"]["comment_body"]
        .as_str()
        .expect("comment body");
    assert!(
        body.contains("- Updated runtime-kit skills to the v3 surface."),
        "{body}"
    );
    assert!(
        body.starts_with("<!-- plan-issue-record:v2 role=state profile=tracking -->"),
        "{body}"
    );
}

#[test]
fn record_post_state_rejects_payload_that_cannot_drive_dashboard() {
    let tmp = TempDir::new().expect("tempdir");
    let payload = tmp.path().join("state.json");
    fs::write(
        &payload,
        json!({
            "status": "in-progress",
            "target_scope": "schema drift",
            "current": "PRs are open as drafts",
            "next_action": "review draft PRs",
            "tasks": [{"id": "1.1", "status": "done", "title": "x"}],
            "prs": [
                {"ref": "owner/repo#9", "url": "https://github.com/owner/repo/pull/9", "status": "draft-open"}
            ],
            "blockers": [
                {"code": "live-home-drift", "status": "open", "detail": "extra surface"}
            ],
            "links": {}
        })
        .to_string(),
    )
    .expect("write payload");

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "post",
        "--issue",
        "448",
        "--kind",
        "state",
        "--payload-file",
        payload.to_str().expect("payload str"),
    ]);

    assert_ne!(out.code, 0, "invalid state payload must fail");
    assert!(
        out.stderr.contains("record-post-payload-schema-invalid")
            || out.stdout.contains("record-post-payload-schema-invalid"),
        "expected schema-invalid error: stdout={} stderr={}",
        out.stdout,
        out.stderr
    );
}

#[test]
fn record_post_rejects_source_plan_and_closeout_kinds() {
    for kind in ["source", "plan", "closeout"] {
        let out = common::run_plan_issue_local(&[
            "--format", "json", "record", "post", "--issue", "1", "--kind", kind,
        ]);
        assert_ne!(out.code, 0, "kind {kind} should be rejected");
        assert!(
            out.stderr.contains("record-post-") || out.stdout.contains("record-post-"),
            "expected record-post error for kind {kind}: stdout={} stderr={}",
            out.stdout,
            out.stderr
        );
    }
}

#[test]
fn record_repair_dashboard_rejects_malformed_state_payload_instead_of_pending() {
    let tmp = TempDir::new().expect("tempdir");
    let body_path = tmp.path().join("body.md");
    let comments_path = tmp.path().join("comments.json");
    fs::write(&body_path, "## Current Dashboard\n\n- Status: pending\n").expect("write body");

    fs::write(
        &comments_path,
        json!({
            "comments": [
                {
                    "url": "https://github.com/owner/repo/issues/9#issuecomment-state",
                    "body": v2_comment_body(
                        "state",
                        "tracking",
                        json!({
                            "status": "in-progress",
                            "target_scope": "schema drift",
                            "current": "PRs are open as drafts",
                            "next_action": "review draft PRs",
                            "tasks": [{"id": "1.1", "status": "done", "title": "x"}],
                            "prs": [
                                {"ref": "owner/repo#9", "url": "https://github.com/owner/repo/pull/9", "status": "draft-open-green"}
                            ],
                            "blockers": [
                                {"code": "live-home-drift", "status": "open", "detail": "extra surface"}
                            ],
                            "links": {}
                        }),
                    ),
                    "created_at": "2026-05-23T10:00:00Z"
                }
            ]
        })
        .to_string(),
    )
    .expect("write comments");

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "repair-dashboard",
        "--body-file",
        body_path.to_str().expect("body path"),
        "--comments-json",
        comments_path.to_str().expect("comments path"),
    ]);

    assert_ne!(out.code, 0, "malformed state payload must fail repair");
    assert!(
        out.stderr.contains("malformed payload") || out.stdout.contains("malformed payload"),
        "expected malformed payload error: stdout={} stderr={}",
        out.stdout,
        out.stderr
    );
}

#[test]
fn record_repair_dashboard_allows_new_valid_state_to_supersede_old_malformed_state() {
    let tmp = TempDir::new().expect("tempdir");
    let body_path = tmp.path().join("body.md");
    let comments_path = tmp.path().join("comments.json");
    fs::write(&body_path, "## Current Dashboard\n\n- Status: pending\n").expect("write body");

    fs::write(
        &comments_path,
        json!({
            "comments": [
                {
                    "url": "https://github.com/owner/repo/issues/9#issuecomment-state-old",
                    "body": v2_comment_body(
                        "state",
                        "tracking",
                        json!({
                            "status": "in-progress",
                            "target_scope": "old schema drift",
                            "prs": [{"ref": "owner/repo#9", "status": "draft-open"}],
                            "blockers": [{"code": "x"}],
                            "links": {}
                        }),
                    ),
                    "created_at": "2026-05-23T10:00:00Z"
                },
                {
                    "url": "https://github.com/owner/repo/issues/9#issuecomment-state-new",
                    "body": v2_comment_body(
                        "state",
                        "tracking",
                        json!({
                            "status": "in-progress",
                            "target_scope": "schema repaired",
                            "current": "latest valid state",
                            "next_action": "continue",
                            "tasks": [{"id": "1.1", "status": "done", "title": "x"}],
                            "prs": [{"ref": "owner/repo#9", "url": "https://github.com/owner/repo/pull/9", "status": "open"}],
                            "blockers": ["older malformed state superseded"],
                            "links": {}
                        }),
                    ),
                    "created_at": "2026-05-23T11:00:00Z"
                }
            ]
        })
        .to_string(),
    )
    .expect("write comments");

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "repair-dashboard",
        "--body-file",
        body_path.to_str().expect("body path"),
        "--comments-json",
        comments_path.to_str().expect("comments path"),
    ]);

    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let parsed = parse_json(&out.stdout);
    let dashboard = parsed["payload"]["result"]["dashboard_markdown"]
        .as_str()
        .expect("dashboard markdown");
    assert!(dashboard.contains("- Status: in-progress"), "{dashboard}");
    assert!(
        dashboard.contains("- Target scope: schema repaired"),
        "{dashboard}"
    );
    assert!(
        dashboard.contains("https://github.com/owner/repo/issues/9#issuecomment-state-new"),
        "{dashboard}"
    );
}

#[test]
fn record_repair_dashboard_renders_canonical_dashboard_from_body_and_comments() {
    let tmp = TempDir::new().expect("tempdir");
    let body_path = tmp.path().join("body.md");
    let comments_path = tmp.path().join("comments.json");
    fs::write(
        &body_path,
        "## Current Dashboard\n\n- Status: in-progress\n",
    )
    .expect("write body");

    let comments = json!({
        "comments": [
            {
                "url": "https://github.com/owner/repo/issues/9#issuecomment-state-1",
                "body": v2_comment_body(
                    "state",
                    "tracking",
                    json!({
                        "status": "in-progress",
                        "target_scope": "sample plan",
                        "current": "Sprint 2 in progress",
                        "next_action": "land Sprint 2",
                        "tasks": [{"id": "1.1", "status": "done", "title": "x"}],
                        "prs": [{"ref": "owner/repo#1", "url": "https://github.com/owner/repo/pull/1", "status": "merged"}],
                        "blockers": [],
                        "links": {}
                    }),
                ),
                "created_at": "2026-05-23T10:00:00Z"
            },
            {
                "url": "https://github.com/owner/repo/issues/9#issuecomment-source",
                "body": v2_comment_body(
                    "source",
                    "tracking",
                    json!({"path": "docs/plans/sample/sample-discussion-source.md", "commit": "abc"}),
                ),
                "created_at": "2026-05-23T09:00:00Z"
            }
        ]
    });
    fs::write(
        &comments_path,
        serde_json::to_string(&comments).expect("json"),
    )
    .expect("write comments");

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "repair-dashboard",
        "--body-file",
        body_path.to_str().expect("body path"),
        "--comments-json",
        comments_path.to_str().expect("comments path"),
    ]);

    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let parsed = parse_json(&out.stdout);
    let dashboard = parsed["payload"]["result"]["dashboard_markdown"]
        .as_str()
        .expect("dashboard markdown");
    assert!(dashboard.starts_with("## Current Dashboard"), "{dashboard}");
    assert!(dashboard.contains("- Status: in-progress"), "{dashboard}");
    // Source URL from latest audit evidence should appear in Durable Record.
    assert!(
        dashboard.contains("https://github.com/owner/repo/issues/9#issuecomment-source"),
        "{dashboard}"
    );
}

#[test]
fn record_repair_dashboard_out_writes_local_dashboard_file() {
    let tmp = TempDir::new().expect("tempdir");
    let body_path = tmp.path().join("body.md");
    let comments_path = tmp.path().join("comments.json");
    let out_path = tmp.path().join("dashboard.md");
    fs::write(&body_path, "## Current Dashboard\n\n- Status: stale\n").expect("write body");
    fs::write(
        &comments_path,
        json!({
            "comments": [
                {
                    "url": "https://github.com/owner/repo/issues/9#issuecomment-state",
                    "body": v2_comment_body(
                        "state",
                        "tracking",
                        json!({
                            "status": "in-progress",
                            "target_scope": "repair out",
                            "current": "refresh dashboard",
                            "next_action": "continue",
                            "tasks": [],
                            "prs": [],
                            "blockers": [],
                            "links": {}
                        }),
                    ),
                    "created_at": "2026-05-23T10:00:00Z"
                }
            ]
        })
        .to_string(),
    )
    .expect("write comments");

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "repair-dashboard",
        "--body-file",
        body_path.to_str().expect("body path"),
        "--comments-json",
        comments_path.to_str().expect("comments path"),
        "--out",
        out_path.to_str().expect("out path"),
    ]);

    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let parsed = parse_json(&out.stdout);
    let result = &parsed["payload"]["result"];
    assert_eq!(result["mode"], "local");
    assert_eq!(
        result["out_path"],
        out_path.to_string_lossy().as_ref(),
        "{result}"
    );
    let dashboard = fs::read_to_string(&out_path).expect("read dashboard");
    assert!(dashboard.starts_with("## Current Dashboard"), "{dashboard}");
    assert!(dashboard.contains("- Status: in-progress"), "{dashboard}");
}

#[test]
fn record_close_requires_non_empty_approval() {
    let out =
        common::run_plan_issue_local(&["--format", "json", "record", "close", "--issue", "9"]);
    assert_ne!(out.code, 0, "missing --approval should fail");
    assert!(
        out.stderr.contains("record-close-missing-approval")
            || out.stdout.contains("record-close-missing-approval"),
        "stderr: {} stdout: {}",
        out.stderr,
        out.stdout
    );
}

fn build_closeout_evidence(linked_pr_ref: &str) -> Value {
    json!({
        "comments": [
            {
                "url": "https://github.com/owner/repo/issues/9#issuecomment-source",
                "body": v2_comment_body(
                    "source",
                    "tracking",
                    json!({"path": "docs/plans/sample/sample-discussion-source.md", "commit": "src1234"}),
                ),
                "created_at": "2026-05-23T09:00:00Z"
            },
            {
                "url": "https://github.com/owner/repo/issues/9#issuecomment-plan",
                "body": v2_comment_body(
                    "plan",
                    "tracking",
                    json!({"path": "docs/plans/sample/sample-plan.md", "commit": "pln1234"}),
                ),
                "created_at": "2026-05-23T09:01:00Z"
            },
            {
                "url": "https://github.com/owner/repo/issues/9#issuecomment-state",
                "body": v2_comment_body(
                    "state",
                    "tracking",
                    json!({
                        "status": "complete",
                        "target_scope": "sample plan",
                        "current": "complete",
                        "next_action": "closeout",
                        "tasks": [
                            {"id": "1.1", "status": "done", "title": "x"},
                            {"id": "1.2", "status": "deferred", "title": "y"},
                        ],
                        "prs": [{"ref": linked_pr_ref, "url": "https://github.com/owner/repo/pull/1", "status": "merged"}],
                        "blockers": [],
                        "links": {}
                    }),
                ),
                "created_at": "2026-05-23T10:00:00Z"
            },
            {
                "url": "https://github.com/owner/repo/issues/9#issuecomment-validation",
                "body": v2_comment_body(
                    "validation",
                    "tracking",
                    json!({
                        "overall": "pass",
                        "commands": [{"command": "cargo test", "status": "pass"}],
                        "waivers": []
                    }),
                ),
                "created_at": "2026-05-23T11:00:00Z"
            },
            {
                "url": "https://github.com/owner/repo/issues/9#issuecomment-review",
                "body": v2_comment_body(
                    "review",
                    "tracking",
                    json!({
                        "decision": "approve",
                        "lenses": ["testing", "maintainability"],
                        "findings": [],
                    }),
                ),
                "created_at": "2026-05-23T12:00:00Z"
            }
        ]
    })
}

#[test]
fn record_close_body_file_mode_blocks_unresolved_linked_pr() {
    let tmp = TempDir::new().expect("tempdir");
    let body_path = tmp.path().join("body.md");
    let comments_path = tmp.path().join("comments.json");
    fs::write(&body_path, "## Current Dashboard\n").expect("write body");
    fs::write(
        &comments_path,
        build_closeout_evidence("owner/repo#1").to_string(),
    )
    .expect("write comments");

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "close",
        "--issue",
        "9",
        "--linked-pr",
        "owner/repo#1",
        "--approval",
        "https://github.com/owner/repo/issues/9#issuecomment-approval",
        "--body-file",
        body_path.to_str().expect("body path"),
        "--comments-json",
        comments_path.to_str().expect("comments path"),
    ]);

    assert_ne!(out.code, 0, "missing provider PR evidence should block");
    let joined = format!("{}\n{}", out.stderr, out.stdout);
    assert!(
        joined.contains("linked-pr-not-merged"),
        "expected linked-pr-not-merged without PR evidence: {joined}"
    );
}

#[test]
fn record_close_fixture_passes_strict_gate_with_complete_v2_evidence() {
    let tmp = TempDir::new().expect("tempdir");
    let fixture = tmp.path().join("fixture");
    fs::create_dir_all(&fixture).expect("create fixture");

    let body = "## Current Dashboard\n\n- Status: in-progress\n";
    write_fixture_files(&fixture, body, &build_closeout_evidence("owner/repo#1"));
    write_pr_fixture(
        &fixture,
        "owner/repo",
        1,
        json!({
            "state": "MERGED",
            "mergeCommit": {"oid": "deadbeefcafebabe"},
            "statusCheckRollup": {"state": "success"},
            "url": "https://github.com/owner/repo/pull/1"
        }),
    );

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "close",
        "--issue",
        "9",
        "--linked-pr",
        "owner/repo#1",
        "--approval",
        "https://github.com/owner/repo/issues/9#issuecomment-approval",
        "--fixture",
        fixture.to_str().expect("fixture path"),
    ]);

    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let parsed = parse_json(&out.stdout);
    let result = &parsed["payload"]["result"];
    assert_eq!(result["operation"], "record.close");
    assert_eq!(result["mode"], "fixture");
    assert_eq!(result["dry_run"], true);
    let preview = &result["preview"];
    let body = preview["closeout_comment_body"]
        .as_str()
        .expect("closeout body");
    assert!(
        body.starts_with("<!-- plan-issue-record:v2 role=closeout profile=tracking -->"),
        "{body}"
    );
    assert!(
        !body.contains(&format!("```{PAYLOAD_FENCE_INFO}")),
        "{body}"
    );
    assert!(
        body.contains("<!-- plan-issue-record-payload:hex:"),
        "{body}"
    );
    let audit = audit_single_comment_body(body);
    let closeout = &audit["evidence"]["closeout"]["payload"]["data"];
    assert_eq!(closeout["final_status"], "complete");
    assert_eq!(closeout["linked_prs"][0]["merge_sha"], "deadbeefcafebabe");
    let final_dashboard = preview["final_dashboard"]
        .as_str()
        .expect("final dashboard");
    assert!(
        final_dashboard.starts_with("## Final Dashboard"),
        "{final_dashboard}"
    );
}

#[test]
fn record_close_fixture_blocks_when_linked_pr_not_merged() {
    let tmp = TempDir::new().expect("tempdir");
    let fixture = tmp.path().join("fixture");
    fs::create_dir_all(&fixture).expect("create fixture");

    let body = "## Current Dashboard\n";
    write_fixture_files(&fixture, body, &build_closeout_evidence("owner/repo#1"));
    write_pr_fixture(
        &fixture,
        "owner/repo",
        1,
        json!({
            "state": "OPEN",
            "mergeCommit": null,
            "statusCheckRollup": {"state": "pending"},
            "url": "https://github.com/owner/repo/pull/1"
        }),
    );

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "close",
        "--issue",
        "9",
        "--linked-pr",
        "owner/repo#1",
        "--approval",
        "ok",
        "--fixture",
        fixture.to_str().expect("fixture path"),
    ]);

    assert_ne!(out.code, 0, "unmerged PR should block strict gate");
    let joined = format!("{}\n{}", out.stderr, out.stdout);
    assert!(
        joined.contains("linked-pr-not-merged"),
        "expected linked-pr-not-merged code, got: {joined}"
    );
}

#[test]
fn record_close_fixture_blocks_when_review_request_changes() {
    let tmp = TempDir::new().expect("tempdir");
    let fixture = tmp.path().join("fixture");
    fs::create_dir_all(&fixture).expect("create fixture");

    // Replace the review entry in the evidence stack with a
    // request-changes decision.
    let mut comments = build_closeout_evidence("owner/repo#1");
    let comments_list = comments["comments"].as_array_mut().expect("comments array");
    let last_index = comments_list.len() - 1;
    comments_list[last_index] = json!({
        "url": "https://github.com/owner/repo/issues/9#issuecomment-review-rej",
        "body": v2_comment_body(
            "review",
            "tracking",
            json!({"decision": "request-changes", "findings": []}),
        ),
        "created_at": "2026-05-23T12:00:00Z"
    });
    write_fixture_files(&fixture, "## Current Dashboard\n", &comments);
    write_pr_fixture(
        &fixture,
        "owner/repo",
        1,
        json!({
            "state": "MERGED",
            "mergeCommit": {"oid": "abc"},
            "statusCheckRollup": {"state": "success"},
            "url": "https://github.com/owner/repo/pull/1"
        }),
    );

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "close",
        "--issue",
        "9",
        "--linked-pr",
        "owner/repo#1",
        "--approval",
        "ok",
        "--fixture",
        fixture.to_str().expect("fixture path"),
    ]);
    assert_ne!(out.code, 0);
    let joined = format!("{}\n{}", out.stderr, out.stdout);
    assert!(
        joined.contains("review-rejected"),
        "expected review-rejected: {joined}"
    );
}

#[test]
fn record_close_fixture_passes_with_non_required_failure_when_zero_required() {
    // Regression for sympoies/nils-cli#502:
    // PR merged, zero required checks, one non-required check failed.
    // Strict closeout gate must not block on non-required failures.
    let tmp = TempDir::new().expect("tempdir");
    let fixture = tmp.path().join("fixture");
    fs::create_dir_all(&fixture).expect("create fixture");

    let body = "## Current Dashboard\n\n- Status: in-progress\n";
    write_fixture_files(&fixture, body, &build_closeout_evidence("owner/repo#1"));
    write_pr_fixture(
        &fixture,
        "owner/repo",
        1,
        json!({
            "state": "MERGED",
            "mergeCommit": {"oid": "deadbeefcafebabe"},
            "statusCheckRollup": {"state": "failure"},
            "requiredCheckRollup": {"state": "success", "count": 0},
            "nonRequiredFailures": ["scripts/ci/all.sh"],
            "url": "https://github.com/owner/repo/pull/1"
        }),
    );

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "close",
        "--issue",
        "9",
        "--linked-pr",
        "owner/repo#1",
        "--approval",
        "https://github.com/owner/repo/issues/9#issuecomment-approval",
        "--fixture",
        fixture.to_str().expect("fixture path"),
    ]);

    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let parsed = parse_json(&out.stdout);
    let result = &parsed["payload"]["result"];
    assert_eq!(result["operation"], "record.close");
    let preview = &result["preview"];
    assert!(
        preview["blocked_codes"]
            .as_array()
            .expect("array")
            .is_empty(),
        "blocked_codes should be empty: {}",
        preview["blocked_codes"]
    );
    let linked = &result["linked_prs"][0];
    assert_eq!(linked["required_count"], 0);
    assert_eq!(linked["required_state"], "pass");
    assert_eq!(linked["non_required_failures"][0], "scripts/ci/all.sh");
}

#[test]
fn record_close_fixture_passes_with_non_required_failure_when_required_pass() {
    let tmp = TempDir::new().expect("tempdir");
    let fixture = tmp.path().join("fixture");
    fs::create_dir_all(&fixture).expect("create fixture");

    let body = "## Current Dashboard\n";
    write_fixture_files(&fixture, body, &build_closeout_evidence("owner/repo#1"));
    write_pr_fixture(
        &fixture,
        "owner/repo",
        1,
        json!({
            "state": "MERGED",
            "mergeCommit": {"oid": "abc"},
            "statusCheckRollup": {"state": "failure"},
            "requiredCheckRollup": {"state": "success", "count": 3},
            "nonRequiredFailures": ["lint-experimental"],
            "url": "https://github.com/owner/repo/pull/1"
        }),
    );

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "close",
        "--issue",
        "9",
        "--linked-pr",
        "owner/repo#1",
        "--approval",
        "ok",
        "--fixture",
        fixture.to_str().expect("fixture path"),
    ]);

    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
}

#[test]
fn record_close_fixture_blocks_with_linked_pr_checks_failed_when_required_fail() {
    let tmp = TempDir::new().expect("tempdir");
    let fixture = tmp.path().join("fixture");
    fs::create_dir_all(&fixture).expect("create fixture");

    let body = "## Current Dashboard\n";
    write_fixture_files(&fixture, body, &build_closeout_evidence("owner/repo#1"));
    write_pr_fixture(
        &fixture,
        "owner/repo",
        1,
        json!({
            "state": "MERGED",
            "mergeCommit": {"oid": "abc"},
            "statusCheckRollup": {"state": "failure"},
            "requiredCheckRollup": {"state": "failure", "count": 2},
            "nonRequiredFailures": [],
            "url": "https://github.com/owner/repo/pull/1"
        }),
    );

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "close",
        "--issue",
        "9",
        "--linked-pr",
        "owner/repo#1",
        "--approval",
        "ok",
        "--fixture",
        fixture.to_str().expect("fixture path"),
    ]);

    assert_ne!(out.code, 0, "required-check failure must block");
    let joined = format!("{}\n{}", out.stderr, out.stdout);
    assert!(
        joined.contains("linked-pr-checks-failed"),
        "expected linked-pr-checks-failed: {joined}"
    );
    assert!(
        !joined.contains("linked-pr-not-merged"),
        "must not collapse into linked-pr-not-merged: {joined}"
    );
}

#[test]
fn record_close_fixture_override_passes_when_required_unknown_aggregate_fails() {
    // When the adapter cannot resolve required-check state (`requiredCheckRollup`
    // absent), the gate stays conservative and blocks on aggregate failure.
    // The override flag unblocks it and records evidence in the closeout body.
    let tmp = TempDir::new().expect("tempdir");
    let fixture = tmp.path().join("fixture");
    fs::create_dir_all(&fixture).expect("create fixture");

    let body = "## Current Dashboard\n";
    write_fixture_files(&fixture, body, &build_closeout_evidence("owner/repo#1"));
    write_pr_fixture(
        &fixture,
        "owner/repo",
        1,
        json!({
            "state": "MERGED",
            "mergeCommit": {"oid": "abc"},
            "statusCheckRollup": {"state": "failure"},
            "nonRequiredFailures": ["legacy/lint"],
            "url": "https://github.com/owner/repo/pull/1"
        }),
    );

    // Without the override → blocked.
    let blocked = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "close",
        "--issue",
        "9",
        "--linked-pr",
        "owner/repo#1",
        "--approval",
        "ok",
        "--fixture",
        fixture.to_str().expect("fixture path"),
    ]);
    assert_ne!(blocked.code, 0, "conservative block expected");
    assert!(
        format!("{}\n{}", blocked.stderr, blocked.stdout).contains("linked-pr-checks-failed"),
        "expected linked-pr-checks-failed under unknown required state"
    );

    // With the override + reason → passes and records evidence.
    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "close",
        "--issue",
        "9",
        "--linked-pr",
        "owner/repo#1",
        "--approval",
        "ok",
        "--allow-non-required-check-failure",
        "--allow-non-required-check-failure-reason",
        "operator verified legacy/lint is non-required",
        "--fixture",
        fixture.to_str().expect("fixture path"),
    ]);

    assert_eq!(out.code, 0, "override should unblock: {}", out.stderr);
    let parsed = parse_json(&out.stdout);
    let body = parsed["payload"]["result"]["preview"]["closeout_comment_body"]
        .as_str()
        .expect("closeout body")
        .to_string();
    assert!(
        body.contains("non-required-check failure override"),
        "expected override summary in body: {body}"
    );
    let audit = audit_single_comment_body(&body);
    let closeout = &audit["evidence"]["closeout"]["payload"]["data"];
    let override_block = &closeout["non_required_check_override"];
    assert_eq!(
        override_block["reason"], "operator verified legacy/lint is non-required",
        "override block reason recorded"
    );
    assert!(
        override_block["observed_non_required_failures"]
            .as_array()
            .is_some_and(|arr| arr.iter().any(|item| item == "owner/repo#1: legacy/lint")),
        "expected observed failure list to include legacy/lint: {override_block}"
    );
}

#[test]
fn record_close_fixture_blocks_when_state_not_complete() {
    let tmp = TempDir::new().expect("tempdir");
    let fixture = tmp.path().join("fixture");
    fs::create_dir_all(&fixture).expect("create fixture");

    let mut comments = build_closeout_evidence("owner/repo#1");
    let comments_list = comments["comments"].as_array_mut().expect("comments array");
    // Replace the state entry (index 2) with status=in-progress.
    comments_list[2] = json!({
        "url": "https://github.com/owner/repo/issues/9#issuecomment-state",
        "body": v2_comment_body(
            "state",
            "tracking",
            json!({
                "status": "in-progress",
                "target_scope": "x",
                "tasks": [],
                "prs": [],
                "blockers": [],
                "links": {}
            }),
        ),
        "created_at": "2026-05-23T10:00:00Z"
    });
    write_fixture_files(&fixture, "## Current Dashboard\n", &comments);
    write_pr_fixture(
        &fixture,
        "owner/repo",
        1,
        json!({
            "state": "MERGED",
            "mergeCommit": {"oid": "abc"},
            "statusCheckRollup": {"state": "success"},
            "url": "https://github.com/owner/repo/pull/1"
        }),
    );

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "close",
        "--issue",
        "9",
        "--linked-pr",
        "owner/repo#1",
        "--approval",
        "ok",
        "--fixture",
        fixture.to_str().expect("fixture path"),
    ]);
    assert_ne!(out.code, 0);
    let joined = format!("{}\n{}", out.stderr, out.stdout);
    assert!(
        joined.contains("state-not-complete"),
        "expected state-not-complete: {joined}"
    );
}

#[test]
fn record_open_fixture_mode_returns_v2_evidence_urls() {
    let tmp = TempDir::new().expect("tempdir");
    let fixture = tmp.path().join("fixture");
    fs::create_dir_all(&fixture).expect("create fixture");

    let body = "## Current Dashboard\n";
    let comments = json!({
        "comments": [
            {
                "url": "https://github.com/owner/repo/issues/9#issuecomment-source",
                "body": v2_comment_body(
                    "source",
                    "tracking",
                    json!({"path": "docs/plans/sample/sample-discussion-source.md", "commit": "src1"}),
                ),
                "created_at": "2026-05-23T09:00:00Z"
            },
            {
                "url": "https://github.com/owner/repo/issues/9#issuecomment-plan",
                "body": v2_comment_body(
                    "plan",
                    "tracking",
                    json!({"path": "docs/plans/sample/sample-plan.md", "commit": "pln1"}),
                ),
                "created_at": "2026-05-23T09:01:00Z"
            },
            {
                "url": "https://github.com/owner/repo/issues/9#issuecomment-state",
                "body": v2_comment_body(
                    "state",
                    "tracking",
                    json!({"status": "in-progress", "tasks": [], "prs": [], "blockers": [], "links": {}}),
                ),
                "created_at": "2026-05-23T09:02:00Z"
            }
        ]
    });
    write_fixture_files(&fixture, body, &comments);

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "open",
        "--fixture",
        fixture.to_str().expect("fixture path"),
        "--title",
        "Sample Plan",
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let parsed = parse_json(&out.stdout);
    let result = &parsed["payload"]["result"];
    assert_eq!(result["operation"], "record.open");
    assert_eq!(result["mode"], "fixture");
    let comments_result = &result["comments"];
    assert_eq!(
        comments_result["source"],
        "https://github.com/owner/repo/issues/9#issuecomment-source"
    );
    assert_eq!(
        comments_result["plan"],
        "https://github.com/owner/repo/issues/9#issuecomment-plan"
    );
    assert_eq!(
        comments_result["state"],
        "https://github.com/owner/repo/issues/9#issuecomment-state"
    );
}

#[test]
fn record_post_state_fixture_returns_rendered_body_without_provider_call() {
    let tmp = TempDir::new().expect("tempdir");
    let fixture = tmp.path().join("fixture");
    fs::create_dir_all(&fixture).expect("create fixture");
    write_fixture_files(&fixture, "## Current Dashboard\n", &json!({"comments": []}));
    let payload = tmp.path().join("payload.json");
    fs::write(
        &payload,
        json!({"status": "in-progress", "tasks": [], "prs": [], "blockers": [], "links": {}})
            .to_string(),
    )
    .expect("write payload");

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "post",
        "--issue",
        "9",
        "--kind",
        "state",
        "--payload-file",
        payload.to_str().expect("payload str"),
        "--fixture",
        fixture.to_str().expect("fixture path"),
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let parsed = parse_json(&out.stdout);
    let result = &parsed["payload"]["result"];
    assert_eq!(result["mode"], "fixture");
    assert_eq!(result["kind"], "state");
    let body = result["comment_body"]
        .as_str()
        .expect("comment body in fixture mode");
    assert!(
        body.contains("<!-- plan-issue-record:v2 role=state profile=tracking -->"),
        "{body}"
    );
}

fn record_open_dry_run_gh_stub() -> &'static str {
    r#"#!/usr/bin/env bash
echo "record_open_dry_run_gh_stub should not be called" >&2
exit 1
"#
}

fn dry_run_cmd_options(stub_dir: &Path) -> CmdOptions {
    common::plan_issue_cmd_options()
        .with_env_remove_prefix("PLAN_ISSUE_GH_")
        .with_path_prepend(stub_dir)
}

#[test]
fn record_open_dry_run_returns_preview_without_gh_calls() {
    use nils_test_support::git::{InitRepoOptions, git, init_repo_with};

    let stub = StubBinDir::new();
    stub.write_exe("gh", record_open_dry_run_gh_stub());

    let repo = init_repo_with(InitRepoOptions::new().with_branch("main"));
    let bundle = repo.path().join("docs/plans/sample");
    fs::create_dir_all(&bundle).expect("create bundle dir");
    let source = bundle.join("sample-discussion-source.md");
    let plan = bundle.join("sample-plan.md");
    let execution_state = bundle.join("sample-execution-state.md");
    fs::write(&source, "# Source\n\n- Decision: implement v2 lifecycle.\n").expect("write source");
    fs::write(
        &plan,
        "# Plan: Sample Plan\n\n## Overview\n\n- Sample plan body.\n\n## Read First\n\n- Primary source: docs/plans/sample/sample-discussion-source.md\n- Source type: discussion-to-implementation-doc\n- Open questions carried into execution: none\n\n## Scope\n\n- In scope:\n  - Demo plan.\n- Out of scope:\n  - none.\n\n## Assumptions\n\n1. Demo only.\n\n## Sprint 1: Demo\n\n**Goal**: Demo the surface.\n\n**PR grouping intent**: group\n**Execution Profile**: serial\n\n### Task 1.1: Demo task\n\n- **Location**:\n  - `docs/plans/sample/sample-plan.md`\n- **Description**: Demo task description.\n- **Dependencies**:\n  - none\n- **Complexity**: 1\n- **Acceptance criteria**:\n  - The demo task is complete.\n- **Validation**:\n  - `true`\n",
    )
    .expect("write plan");
    fs::write(
        &execution_state,
        "# Sample Execution State\n\n<!-- plan-issue-record:v2 role=state profile=tracking -->\n\n## Execution State\n\n- Status: pending\n- Target scope: Sample Plan\n",
    )
    .expect("write execution state");
    git(repo.path(), &["add", "."]);
    git(
        repo.path(),
        &[
            "-c",
            "user.email=test@example.com",
            "-c",
            "user.name=Test",
            "commit",
            "-m",
            "seed bundle",
            "--no-gpg-sign",
        ],
    );

    let opts = dry_run_cmd_options(stub.path()).with_cwd(repo.path());
    let bundle_arg = bundle.to_string_lossy().to_string();
    let out = nils_test_support::cmd::run_resolved(
        "plan-issue-local",
        &[
            "--format",
            "json",
            "record",
            "open",
            "--bundle",
            &bundle_arg,
        ],
        &opts,
    );
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let parsed: Value = serde_json::from_str(&out.stdout_text()).expect("json");
    let result = &parsed["payload"]["result"];
    assert_eq!(result["operation"], "record.open");
    assert_eq!(result["mode"], "dry-run");
    assert_eq!(result["dry_run"], true);
    let preview = &result["preview"];
    assert_eq!(preview["plan_title"], "Plan: Sample Plan");
    let source_comment = preview["comments"]["source"]
        .as_str()
        .expect("source comment");
    assert!(
        source_comment.starts_with("<!-- plan-issue-record:v2 role=source profile=tracking -->"),
        "{source_comment}"
    );
    let plan_comment = preview["comments"]["plan"].as_str().expect("plan comment");
    let state_comment = preview["comments"]["state"]
        .as_str()
        .expect("state comment");
    for (label, comment) in [
        ("source", source_comment),
        ("plan", plan_comment),
        ("state", state_comment),
    ] {
        assert!(
            !comment.contains(&format!("```{PAYLOAD_FENCE_INFO}")),
            "{label} comment should not visibly leak payload JSON:\n{comment}"
        );
    }
    assert!(
        state_comment.contains("# Sample Execution State"),
        "{state_comment}"
    );
    assert!(
        state_comment.contains("- Status: pending"),
        "{state_comment}"
    );
    assert!(
        !state_comment.contains("Initial execution state seeded"),
        "{state_comment}"
    );

    let comments_json = repo.path().join("comments.json");
    fs::write(
        &comments_json,
        json!({
            "comments": [
                {"body": source_comment, "url": "https://github.com/owner/repo/issues/1#issuecomment-source"},
                {"body": plan_comment, "url": "https://github.com/owner/repo/issues/1#issuecomment-plan"},
                {"body": state_comment, "url": "https://github.com/owner/repo/issues/1#issuecomment-state"}
            ]
        })
        .to_string(),
    )
    .expect("write comments json");

    let audit = nils_test_support::cmd::run_resolved(
        "plan-issue-local",
        &[
            "--format",
            "json",
            "record",
            "audit",
            "--comments-json",
            comments_json.to_str().expect("comments path"),
            "--profile",
            "tracking",
        ],
        &opts,
    );
    assert_eq!(audit.code, 0, "stderr: {}", audit.stderr_text());
    let parsed_audit: Value = serde_json::from_str(&audit.stdout_text()).expect("audit json");
    let audit_result = &parsed_audit["payload"]["result"]["audit"];
    assert_eq!(audit_result["recognized_count"], 3);
    assert_eq!(
        audit_result["missing_required"],
        json!([]),
        "{audit_result}"
    );
}

/// Sprint 4 Task 4.3: exercise the v3 closeout end-to-end against a sanitized
/// agent-runtime-kit fixture. Asserts that one `record close` invocation can
/// audit the issue, verify provider PR merge evidence, render the closeout
/// comment + final dashboard, and that no v1 markers leak into the result.
#[test]
fn agent_runtime_kit_lifecycle_fixture_passes_strict_v2_closeout_end_to_end() {
    let fixture = Path::new("tests/fixtures/lifecycle/agent-runtime-kit-closeout").to_path_buf();
    assert!(fixture.exists(), "fixture missing: {}", fixture.display());

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "close",
        "--issue",
        "42",
        "--linked-pr",
        "sympoies/agent-runtime-kit#1",
        "--approval",
        "https://github.com/sympoies/agent-runtime-kit/issues/42#issuecomment-approval",
        "--fixture",
        fixture.to_str().expect("fixture path"),
    ]);

    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let parsed = parse_json(&out.stdout);
    let result = &parsed["payload"]["result"];
    assert_eq!(result["operation"], "record.close");
    assert_eq!(result["mode"], "fixture");
    assert_eq!(result["dry_run"], true);

    let preview = &result["preview"];
    let closeout_body = preview["closeout_comment_body"]
        .as_str()
        .expect("closeout body present");
    // Closeout comment uses the v2 marker and carries provider-verified
    // merge_sha from the fixture PR snapshot in the hidden payload.
    assert!(
        closeout_body.starts_with("<!-- plan-issue-record:v2 role=closeout profile=tracking -->"),
        "{closeout_body}"
    );
    assert!(
        !closeout_body.contains(&format!("```{PAYLOAD_FENCE_INFO}")),
        "{closeout_body}"
    );
    assert!(
        closeout_body.contains("<!-- plan-issue-record-payload:hex:"),
        "{closeout_body}"
    );
    let audit = audit_single_comment_body(closeout_body);
    let closeout = &audit["evidence"]["closeout"]["payload"]["data"];
    assert_eq!(closeout["final_status"], "complete");
    assert_eq!(
        closeout["linked_prs"][0]["merge_sha"], "merge1111111111111111111111111111111111",
        "merge_sha must come from PR fixture, not state payload: {closeout_body}"
    );
    // Sanity: no v1 marker bleed-through.
    assert!(
        !closeout_body.contains("execute-from-tracking-issue:")
            && !closeout_body.contains("plan-tracking-issue:"),
        "v1 markers must not appear in v2 closeout body: {closeout_body}"
    );

    let final_dashboard = preview["final_dashboard"]
        .as_str()
        .expect("final dashboard present");
    assert!(
        final_dashboard.starts_with("## Final Dashboard"),
        "complete state must render Final Dashboard: {final_dashboard}"
    );
    // Durable record links derive from audit, not caller-supplied URLs.
    assert!(
        final_dashboard.contains(
            "https://github.com/sympoies/agent-runtime-kit/issues/42#issuecomment-source"
        ),
        "dashboard must include source URL from audit: {final_dashboard}"
    );
    assert!(
        final_dashboard
            .contains("https://github.com/sympoies/agent-runtime-kit/issues/42#issuecomment-state"),
        "dashboard must include state URL from audit: {final_dashboard}"
    );
}

/// Issue sympoies/nils-cli#479: `record open --label` exposes labels in the
/// dry-run preview so downstream consumers can audit creation-time labels
/// without hitting the provider.
#[test]
fn record_open_dry_run_includes_labels_in_preview() {
    use nils_test_support::git::{InitRepoOptions, git, init_repo_with};

    let stub = StubBinDir::new();
    stub.write_exe("gh", record_open_dry_run_gh_stub());

    let repo = init_repo_with(InitRepoOptions::new().with_branch("main"));
    let bundle = repo.path().join("docs/plans/sample");
    fs::create_dir_all(&bundle).expect("create bundle dir");
    let source = bundle.join("sample-discussion-source.md");
    let plan = bundle.join("sample-plan.md");
    let execution_state = bundle.join("sample-execution-state.md");
    fs::write(&source, "# Source\n\n- Decision: implement v2 lifecycle.\n").expect("write source");
    fs::write(
        &plan,
        "# Plan: Sample Plan\n\n## Overview\n\n- Sample plan body.\n\n## Read First\n\n- Primary source: docs/plans/sample/sample-discussion-source.md\n- Source type: discussion-to-implementation-doc\n- Open questions carried into execution: none\n\n## Scope\n\n- In scope:\n  - Demo plan.\n- Out of scope:\n  - none.\n\n## Assumptions\n\n1. Demo only.\n\n## Sprint 1: Demo\n\n**Goal**: Demo the surface.\n\n**PR grouping intent**: group\n**Execution Profile**: serial\n\n### Task 1.1: Demo task\n\n- **Location**:\n  - `docs/plans/sample/sample-plan.md`\n- **Description**: Demo task description.\n- **Dependencies**:\n  - none\n- **Complexity**: 1\n- **Acceptance criteria**:\n  - The demo task is complete.\n- **Validation**:\n  - `true`\n",
    )
    .expect("write plan");
    fs::write(
        &execution_state,
        "# Sample Execution State\n\n<!-- plan-issue-record:v2 role=state profile=tracking -->\n\n## Execution State\n\n- Status: pending\n- Target scope: Sample Plan\n",
    )
    .expect("write execution state");
    git(repo.path(), &["add", "."]);
    git(
        repo.path(),
        &[
            "-c",
            "user.email=test@example.com",
            "-c",
            "user.name=Test",
            "commit",
            "-m",
            "seed bundle",
            "--no-gpg-sign",
        ],
    );

    let opts = dry_run_cmd_options(stub.path()).with_cwd(repo.path());
    let bundle_arg = bundle.to_string_lossy().to_string();
    let out = nils_test_support::cmd::run_resolved(
        "plan-issue-local",
        &[
            "--format",
            "json",
            "record",
            "open",
            "--bundle",
            &bundle_arg,
            "--label",
            "workflow::plan",
            "--label",
            " state::needs-triage ",
            "--label",
            "",
        ],
        &opts,
    );
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let parsed: Value = serde_json::from_str(&out.stdout_text()).expect("json");
    let result = &parsed["payload"]["result"];
    assert_eq!(result["mode"], "dry-run");
    let labels = result["preview"]["labels"]
        .as_array()
        .expect("preview.labels array");
    let labels: Vec<&str> = labels.iter().filter_map(Value::as_str).collect();
    assert_eq!(
        labels,
        vec!["workflow::plan", "state::needs-triage"],
        "empty/whitespace labels must be dropped and non-empty values trimmed"
    );
}

#[test]
fn record_attach_dry_run_renders_source_plan_and_state_comments() {
    use nils_test_support::git::{InitRepoOptions, git, init_repo_with};

    let repo = init_repo_with(InitRepoOptions::new().with_branch("main"));
    let bundle = repo.path().join("docs/plans/sample");
    fs::create_dir_all(&bundle).expect("create bundle dir");
    let source = bundle.join("sample-discussion-source.md");
    let plan = bundle.join("sample-plan.md");
    let execution_state = bundle.join("sample-execution-state.md");
    fs::write(&source, "# Source\n\n- Decision: attach existing issue.\n").expect("write source");
    fs::write(
        &plan,
        "# Plan: Existing Issue Attach\n\n## Overview\n\n- Attach v2 lifecycle comments.\n\n## Read First\n\n- Primary source: docs/plans/sample/sample-discussion-source.md\n- Source type: discussion-to-implementation-doc\n- Open questions carried into execution: none\n\n## Scope\n\n- In scope:\n  - Demo attach.\n- Out of scope:\n  - none.\n\n## Assumptions\n\n1. Demo only.\n\n## Sprint 1: Demo\n\n**Goal**: Demo the attach surface.\n\n**PR grouping intent**: group\n**Execution Profile**: serial\n\n### Task 1.1: Demo task\n\n- **Location**:\n  - `docs/plans/sample/sample-plan.md`\n- **Description**: Demo task description.\n- **Dependencies**:\n  - none\n- **Complexity**: 1\n- **Acceptance criteria**:\n  - The demo task is complete.\n- **Validation**:\n  - `true`\n",
    )
    .expect("write plan");
    fs::write(
        &execution_state,
        "# Sample Execution State\n\n<!-- plan-issue-record:v2 role=state profile=tracking -->\n\n## Execution State\n\n- Status: pending\n- Target scope: Existing Issue Attach\n",
    )
    .expect("write execution state");
    git(repo.path(), &["add", "."]);
    git(
        repo.path(),
        &[
            "-c",
            "user.email=test@example.com",
            "-c",
            "user.name=Test",
            "commit",
            "-m",
            "seed bundle",
            "--no-gpg-sign",
        ],
    );

    let bundle_arg = bundle.to_string_lossy().to_string();
    let out = nils_test_support::cmd::run_resolved(
        "plan-issue-local",
        &[
            "--format",
            "json",
            "--dry-run",
            "record",
            "attach",
            "--issue",
            "69",
            "--bundle",
            &bundle_arg,
        ],
        &nils_test_support::cmd::CmdOptions::new().with_cwd(repo.path()),
    );
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let parsed: Value = serde_json::from_str(&out.stdout_text()).expect("json");
    let result = &parsed["payload"]["result"];
    assert_eq!(result["mode"], "dry-run");
    assert_eq!(result["preview"]["issue_number"], 69);
    let comments = &result["preview"]["comments"];
    assert!(comments["source"].as_str().unwrap().contains("role=source"));
    assert!(comments["plan"].as_str().unwrap().contains("role=plan"));
    assert!(comments["state"].as_str().unwrap().contains("role=state"));
}

/// `record post --add-label / --remove-label` exposes the planned label
/// mutation in dry-run output and in fixture mode without touching gh.
#[test]
fn record_post_dry_run_includes_label_mutations() {
    let tmp = TempDir::new().expect("tempdir");
    let payload = tmp.path().join("state.json");
    fs::write(
        &payload,
        json!({"status": "blocked", "tasks": [], "prs": [], "blockers": [], "links": {}})
            .to_string(),
    )
    .expect("write payload");

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "--dry-run",
        "record",
        "post",
        "--issue",
        "448",
        "--kind",
        "state",
        "--payload-file",
        payload.to_str().expect("payload path"),
        "--add-label",
        "state::blocked",
        "--remove-label",
        "state::in-progress",
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let parsed = parse_json(&out.stdout);
    let result = &parsed["payload"]["result"];
    assert_eq!(result["mode"], "dry-run");
    assert_eq!(result["labels"]["add"][0], "state::blocked");
    assert_eq!(result["labels"]["remove"][0], "state::in-progress");
}

/// `record close --add-label / --remove-label` shows the planned closeout
/// label transition in fixture preview output.
#[test]
fn record_close_fixture_includes_label_mutations() {
    let fixture = Path::new("tests/fixtures/lifecycle/agent-runtime-kit-closeout").to_path_buf();
    assert!(fixture.exists(), "fixture missing: {}", fixture.display());

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "close",
        "--issue",
        "42",
        "--linked-pr",
        "sympoies/agent-runtime-kit#1",
        "--approval",
        "https://github.com/sympoies/agent-runtime-kit/issues/42#issuecomment-approval",
        "--fixture",
        fixture.to_str().expect("fixture path"),
        "--add-label",
        "state::closed",
        "--remove-label",
        "state::in-progress",
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let parsed = parse_json(&out.stdout);
    let labels = &parsed["payload"]["result"]["preview"]["labels"];
    assert_eq!(labels["add"][0], "state::closed");
    assert_eq!(labels["remove"][0], "state::in-progress");
}

/// Same label name in `--add-label` and `--remove-label` is incoherent — the
/// helper rejects it with a usage error so the live `gh issue edit` call is
/// never built.
#[test]
fn record_post_rejects_conflicting_label_mutations() {
    let tmp = TempDir::new().expect("tempdir");
    let payload = tmp.path().join("state.json");
    fs::write(
        &payload,
        json!({"status": "in-progress", "tasks": [], "prs": [], "blockers": [], "links": {}})
            .to_string(),
    )
    .expect("write payload");

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "--dry-run",
        "record",
        "post",
        "--issue",
        "448",
        "--kind",
        "state",
        "--payload-file",
        payload.to_str().expect("payload path"),
        "--add-label",
        "state::needs-triage",
        "--remove-label",
        "state::needs-triage",
    ]);
    assert_ne!(out.code, 0, "conflicting label mutation should fail");
    let joined = format!("{}\n{}", out.stderr, out.stdout);
    assert!(
        joined.contains("record-label-mutation-conflict"),
        "expected record-label-mutation-conflict code, got: {joined}"
    );
}

#[test]
fn record_close_rejects_conflicting_label_mutations() {
    let fixture = Path::new("tests/fixtures/lifecycle/agent-runtime-kit-closeout").to_path_buf();
    assert!(fixture.exists(), "fixture missing: {}", fixture.display());

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "close",
        "--issue",
        "42",
        "--linked-pr",
        "sympoies/agent-runtime-kit#1",
        "--approval",
        "ok",
        "--fixture",
        fixture.to_str().expect("fixture path"),
        "--add-label",
        "state::closed",
        "--remove-label",
        "state::closed",
    ]);
    assert_ne!(out.code, 0, "conflicting label mutation should fail");
    let joined = format!("{}\n{}", out.stderr, out.stdout);
    assert!(
        joined.contains("record-label-mutation-conflict"),
        "expected record-label-mutation-conflict code, got: {joined}"
    );
}

/// Sprint 4 Task 4.3: same fixture, but force the strict gate to fail by
/// flipping the PR snapshot to unmerged. Verifies the gate code surfaces.
#[test]
fn agent_runtime_kit_lifecycle_fixture_blocks_when_pr_unmerged() {
    let src = Path::new("tests/fixtures/lifecycle/agent-runtime-kit-closeout");
    let tmp = TempDir::new().expect("tmp");
    let fixture = tmp.path().join("fixture");
    fs::create_dir_all(fixture.join("prs")).expect("create fixture dirs");
    fs::copy(src.join("issue-body.md"), fixture.join("issue-body.md")).expect("copy body");
    fs::copy(src.join("comments.json"), fixture.join("comments.json")).expect("copy comments");
    // Replace the PR snapshot with an open PR so the strict gate fails.
    fs::write(
        fixture.join("prs/sympoies__agent-runtime-kit__1.json"),
        serde_json::to_string(&json!({
            "state": "OPEN",
            "mergeCommit": null,
            "statusCheckRollup": {"state": "pending"},
            "url": "https://github.com/sympoies/agent-runtime-kit/pull/1"
        }))
        .expect("pr json"),
    )
    .expect("write open pr fixture");

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "close",
        "--issue",
        "42",
        "--linked-pr",
        "sympoies/agent-runtime-kit#1",
        "--approval",
        "ok",
        "--fixture",
        fixture.to_str().expect("fixture path"),
    ]);

    assert_ne!(out.code, 0, "unmerged PR should block strict closeout");
    let joined = format!("{}\n{}", out.stderr, out.stdout);
    assert!(
        joined.contains("linked-pr-not-merged"),
        "expected linked-pr-not-merged code, got: {joined}"
    );
}
