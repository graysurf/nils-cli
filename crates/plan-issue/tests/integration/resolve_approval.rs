// Task 1.5: `plan-issue resolve-approval` — wrap the orchestrator pattern
// of finding the latest review-evidence comment whose body contains
// `Decision: merge` on a PR. Drives the live `plan-issue` binary with a
// stubbed `forge-cli` so we can pin the comment payload deterministically.
//
// Post-consolidation (`docs/plans/2026-06-19-plan-issue-forge-cli-consolidation`)
// `ForgeCliAdapter::pr_comments` runs `forge-cli pr comments <pr>` and reshapes
// each returned comment, renaming forge-cli's native `url` field to `html_url`
// for the resolve-approval consumer. The fixtures here therefore use `url`
// (the wire shape forge-cli emits); the adapter performs the rename.

use std::path::Path;

use nils_test_support::StubBinDir;
use nils_test_support::cmd::CmdOptions;
use pretty_assertions::assert_eq;
use serde_json::json;

use crate::common;

/// Build the test command options, prepending the stubbed `forge-cli` and
/// seeding the PR-comments array the stub will return for `pr comments`.
fn cmd_options(stub_dir: &Path, pr_comments_json: &str) -> CmdOptions {
    common::plan_issue_cmd_options()
        .with_env_remove_prefix("FORGE_CLI_STUB_")
        .with_path_prepend(stub_dir)
        .with_envs(&[("FORGE_CLI_STUB_PR_COMMENTS_JSON", pr_comments_json)])
}

#[test]
fn resolve_approval_text_prints_url_when_exactly_one_match() {
    let comments = json!([
        {
            "url": "https://github.com/owner/repo/pull/12#issuecomment-1",
            "author": "reviewer",
            "created_at": "2026-04-25T10:00:00Z",
            "body": "Triage: Pending\nDecision: pending\n"
        },
        {
            "url": "https://github.com/owner/repo/pull/12#issuecomment-2",
            "author": "reviewer",
            "created_at": "2026-04-25T11:00:00Z",
            "body": "Decision: merge\nVerdict: ready\n"
        }
    ])
    .to_string();

    let stub = StubBinDir::new();
    stub.write_exe("forge-cli", common::forge_cli_stub_script());

    let out = common::run_plan_issue_with_options(
        &["--repo", "owner/repo", "resolve-approval", "--pr", "12"],
        cmd_options(stub.path(), &comments),
    );

    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    assert_eq!(
        out.stdout_text().trim(),
        "https://github.com/owner/repo/pull/12#issuecomment-2",
        "single-match text output should print only the URL: stdout={:?} stderr={:?}",
        out.stdout_text(),
        out.stderr_text()
    );
}

#[test]
fn resolve_approval_text_fails_when_no_decision_merge_comment() {
    let comments = json!([
        {
            "url": "https://github.com/owner/repo/pull/12#issuecomment-1",
            "author": "reviewer",
            "created_at": "2026-04-25T10:00:00Z",
            "body": "Decision: pending\n"
        }
    ])
    .to_string();

    let stub = StubBinDir::new();
    stub.write_exe("forge-cli", common::forge_cli_stub_script());

    let out = common::run_plan_issue_with_options(
        &["--repo", "owner/repo", "resolve-approval", "--pr", "12"],
        cmd_options(stub.path(), &comments),
    );

    assert_ne!(out.code, 0, "expected non-zero exit on no-match");
    assert!(
        out.stderr_text()
            .contains("no merge-decision review-evidence comment found"),
        "stderr should describe the empty case: {:?}",
        out.stderr_text()
    );
    assert!(
        out.stdout_text().trim().is_empty(),
        "stdout must stay empty when text mode fails: {:?}",
        out.stdout_text()
    );
}

#[test]
fn resolve_approval_text_fails_when_multiple_decision_merge_comments() {
    let comments = json!([
        {
            "url": "https://github.com/owner/repo/pull/12#issuecomment-1",
            "author": "reviewer",
            "created_at": "2026-04-25T10:00:00Z",
            "body": "Decision: merge\n"
        },
        {
            "url": "https://github.com/owner/repo/pull/12#issuecomment-2",
            "author": "reviewer",
            "created_at": "2026-04-25T11:00:00Z",
            "body": "Decision: merge\n"
        }
    ])
    .to_string();

    let stub = StubBinDir::new();
    stub.write_exe("forge-cli", common::forge_cli_stub_script());

    let out = common::run_plan_issue_with_options(
        &["--repo", "owner/repo", "resolve-approval", "--pr", "12"],
        cmd_options(stub.path(), &comments),
    );

    assert_ne!(out.code, 0, "expected non-zero exit when ambiguous");
    assert!(
        out.stderr_text()
            .contains("found 2 merge-decision review-evidence comments"),
        "stderr should name the count: {:?}",
        out.stderr_text()
    );
    assert!(
        out.stdout_text().trim().is_empty(),
        "stdout must stay empty when text mode fails: {:?}",
        out.stdout_text()
    );
}

#[test]
fn resolve_approval_json_reports_zero_one_or_many_candidates() {
    // Many candidates: latest sorts first, all listed.
    let comments = json!([
        {
            "url": "https://github.com/owner/repo/pull/12#issuecomment-old",
            "author": "reviewer",
            "created_at": "2026-04-20T10:00:00Z",
            "body": "Decision: merge\n"
        },
        {
            "url": "https://github.com/owner/repo/pull/12#issuecomment-new",
            "author": "reviewer",
            "created_at": "2026-04-25T18:00:00Z",
            "body": "Decision: merge\n"
        }
    ])
    .to_string();
    let stub = StubBinDir::new();
    stub.write_exe("forge-cli", common::forge_cli_stub_script());

    let out = common::run_plan_issue_with_options(
        &[
            "--format",
            "json",
            "--repo",
            "owner/repo",
            "resolve-approval",
            "--pr",
            "12",
        ],
        cmd_options(stub.path(), &comments),
    );
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let payload = out.stdout_json();
    let result = &payload["payload"]["result"];
    assert_eq!(result["count"], 2);
    assert_eq!(
        result["url"], "https://github.com/owner/repo/pull/12#issuecomment-new",
        "result.url must be the latest html_url"
    );
    let candidates = result["candidates"].as_array().expect("candidates");
    assert_eq!(candidates.len(), 2);
    assert_eq!(
        candidates[0]["html_url"], "https://github.com/owner/repo/pull/12#issuecomment-new",
        "candidates[0] is the latest"
    );

    // Zero candidates: count == 0, url == null, exit still 0 in JSON mode.
    let empty = json!([]).to_string();
    let out = common::run_plan_issue_with_options(
        &[
            "--format",
            "json",
            "--repo",
            "owner/repo",
            "resolve-approval",
            "--pr",
            "12",
        ],
        cmd_options(stub.path(), &empty),
    );
    assert_eq!(out.code, 0, "JSON mode keeps exit 0: {}", out.stderr_text());
    let payload = out.stdout_json();
    let result = &payload["payload"]["result"];
    assert_eq!(result["count"], 0);
    assert!(result["url"].is_null());
    assert_eq!(result["candidates"].as_array().expect("array").len(), 0);
}
