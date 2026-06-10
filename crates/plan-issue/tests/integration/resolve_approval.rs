// Task 1.5: `plan-issue resolve-approval` — wrap the orchestrator pattern
// of finding the latest review-evidence comment whose body contains
// `Decision: merge` on a PR. Drives the live `plan-issue` binary with a
// stubbed `gh` so we can pin the comment payload deterministically.

use std::fs;

use nils_test_support::StubBinDir;
use nils_test_support::cmd::CmdOptions;
use pretty_assertions::assert_eq;
use tempfile::TempDir;

use crate::common;

/// `gh api repos/.../issues/<pr>/comments` stub. Reads JSON from the path
/// in `RESOLVE_APPROVAL_GH_COMMENTS` and prints it back unchanged. Other
/// `gh` calls are rejected so we catch unintended live calls.
fn comments_stub_script() -> &'static str {
    r#"#!/usr/bin/env bash
set -euo pipefail

cmd="${1:-}"
if [[ "$cmd" != "api" ]]; then
  printf 'unsupported gh call: %s\n' "$*" >&2
  exit 1
fi

# Skip past `--paginate` if present.
shift 1
while [[ ${1:-} == "--paginate" ]]; do
  shift 1
done

endpoint="${1:-}"
case "$endpoint" in
  repos/*/issues/*/comments)
    src="${RESOLVE_APPROVAL_GH_COMMENTS:-}"
    if [[ -z "$src" ]]; then
      printf '[]\n'
    else
      cat "$src"
    fi
    ;;
  *)
    printf 'unsupported gh api endpoint: %s\n' "$endpoint" >&2
    exit 1
    ;;
esac
"#
}

fn cmd_options(stub: &StubBinDir, comments_file: &std::path::Path) -> CmdOptions {
    common::plan_issue_cmd_options()
        .with_env_remove_prefix("RESOLVE_APPROVAL_GH_")
        .with_path_prepend(stub.path())
        .with_envs(&[(
            "RESOLVE_APPROVAL_GH_COMMENTS",
            &comments_file.to_string_lossy(),
        )])
}

#[test]
fn resolve_approval_text_prints_url_when_exactly_one_match() {
    let tmp = TempDir::new().expect("tempdir");
    let comments = tmp.path().join("comments.json");
    fs::write(
        &comments,
        r#"[
  {
    "html_url": "https://github.com/owner/repo/pull/12#issuecomment-1",
    "created_at": "2026-04-25T10:00:00Z",
    "body": "Triage: Pending\nDecision: pending\n"
  },
  {
    "html_url": "https://github.com/owner/repo/pull/12#issuecomment-2",
    "created_at": "2026-04-25T11:00:00Z",
    "body": "Decision: merge\nVerdict: ready\n"
  }
]"#,
    )
    .expect("write comments");

    let stub = StubBinDir::new();
    stub.write_exe("gh", comments_stub_script());

    let out = common::run_plan_issue_with_options(
        &["--repo", "owner/repo", "resolve-approval", "--pr", "12"],
        cmd_options(&stub, &comments),
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
    let tmp = TempDir::new().expect("tempdir");
    let comments = tmp.path().join("comments.json");
    fs::write(
        &comments,
        r#"[
  {
    "html_url": "https://github.com/owner/repo/pull/12#issuecomment-1",
    "created_at": "2026-04-25T10:00:00Z",
    "body": "Decision: pending\n"
  }
]"#,
    )
    .expect("write comments");

    let stub = StubBinDir::new();
    stub.write_exe("gh", comments_stub_script());

    let out = common::run_plan_issue_with_options(
        &["--repo", "owner/repo", "resolve-approval", "--pr", "12"],
        cmd_options(&stub, &comments),
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
    let tmp = TempDir::new().expect("tempdir");
    let comments = tmp.path().join("comments.json");
    fs::write(
        &comments,
        r#"[
  {
    "html_url": "https://github.com/owner/repo/pull/12#issuecomment-1",
    "created_at": "2026-04-25T10:00:00Z",
    "body": "Decision: merge\n"
  },
  {
    "html_url": "https://github.com/owner/repo/pull/12#issuecomment-2",
    "created_at": "2026-04-25T11:00:00Z",
    "body": "Decision: merge\n"
  }
]"#,
    )
    .expect("write comments");

    let stub = StubBinDir::new();
    stub.write_exe("gh", comments_stub_script());

    let out = common::run_plan_issue_with_options(
        &["--repo", "owner/repo", "resolve-approval", "--pr", "12"],
        cmd_options(&stub, &comments),
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
    let tmp = TempDir::new().expect("tempdir");
    let comments = tmp.path().join("comments.json");
    fs::write(
        &comments,
        r#"[
  {
    "html_url": "https://github.com/owner/repo/pull/12#issuecomment-old",
    "created_at": "2026-04-20T10:00:00Z",
    "body": "Decision: merge\n"
  },
  {
    "html_url": "https://github.com/owner/repo/pull/12#issuecomment-new",
    "created_at": "2026-04-25T18:00:00Z",
    "body": "Decision: merge\n"
  }
]"#,
    )
    .expect("write comments");
    let stub = StubBinDir::new();
    stub.write_exe("gh", comments_stub_script());

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
        cmd_options(&stub, &comments),
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
    fs::write(&comments, "[]").expect("rewrite comments");
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
        cmd_options(&stub, &comments),
    );
    assert_eq!(out.code, 0, "JSON mode keeps exit 0: {}", out.stderr_text());
    let payload = out.stdout_json();
    let result = &payload["payload"]["result"];
    assert_eq!(result["count"], 0);
    assert!(result["url"].is_null());
    assert_eq!(result["candidates"].as_array().expect("array").len(), 0);
}
