use std::fs;
use std::path::Path;

use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;

use nils_test_support::StubBinDir;
use nils_test_support::cmd::CmdOptions;

use crate::common;
const PLAN_PATH: &str =
    "crates/plan-issue/tests/fixtures/plans/plan-issue-rust-cli-full-delivery-plan.md";

fn forge_cmd_options(stub_dir: &Path, envs: &[(&str, &str)]) -> CmdOptions {
    common::plan_issue_cmd_options()
        // Keep the stubbed forge-cli behavior deterministic even when the outer
        // shell exports FORGE_CLI_STUB_* variables.
        .with_env_remove_prefix("FORGE_CLI_STUB_")
        .with_path_prepend(stub_dir)
        .with_envs(envs)
}

fn issue_body_with_preface(task_rows: &str) -> String {
    format!(
        r#"# Plan: Rust Plan-Issue CLI Full Delivery

## Overview

- This plan delivers a shell-free Rust implementation for the current plan-issue orchestration workflow.
- The issue body keeps pre-sprint context and uses Task Decomposition as runtime truth.

## Scope

- Maintain one plan issue for the full multi-sprint workflow.
- Keep pre-sprint sections stable while sprint commands read/validate runtime-truth rows.

## Task Decomposition

| Task | Summary | Owner | Branch | Worktree | Execution Mode | PR | Status | Notes |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
{task_rows}
"#
    )
}

fn issue_body_sprint4_planned() -> String {
    issue_body_with_preface(
        r#"| S3T1 | Implement task-spec generation core using `plan-tooling` | subagent-s3-t1 | feat/s3-t1-implement-task-spec-generation-core-using-plan-t | feat-s3-t1 | per-sprint | #221 | done | sprint=S3; plan-task:Task 3.1 |
| S3T2 | Implement issue-body and sprint-comment rendering engine | subagent-s3-t1 | feat/s3-t1-implement-task-spec-generation-core-using-plan-t | feat-s3-t1 | per-sprint | #221 | done | sprint=S3; plan-task:Task 3.2 |
| S3T3 | Implement independent local dry-run workflow | subagent-s3-t1 | feat/s3-t1-implement-task-spec-generation-core-using-plan-t | feat-s3-t1 | per-sprint | #221 | done | sprint=S3; plan-task:Task 3.3 |
| S4T1 | Implement GitHub adapter abstraction and `gh` backend | subagent-s4-t1 | feat/s4-t1-implement-github-adapter-abstraction-and-gh-back | feat-s4-t1 | per-sprint | TBD | planned | sprint=S4; plan-task:Task 4.1; deps=Task 3.3; validate=cargo test -p nils-plan-issue github_adapter; pr-grouping=per-sprint; pr-group=s4; shared-pr-anchor=S4T1 |
| S4T2 | Implement live plan-level commands | subagent-s4-t1 | feat/s4-t1-implement-github-adapter-abstraction-and-gh-back | feat-s4-t1 | per-sprint | TBD | planned | sprint=S4; plan-task:Task 4.2; deps=Task 4.1; validate=cargo test -p nils-plan-issue live_plan_commands; pr-grouping=per-sprint; pr-group=s4; shared-pr-anchor=S4T1 |
| S4T3 | Implement live sprint-level commands and guide output | subagent-s4-t1 | feat/s4-t1-implement-github-adapter-abstraction-and-gh-back | feat-s4-t1 | per-sprint | TBD | planned | sprint=S4; plan-task:Task 4.3; deps=Task 4.1; validate=cargo test -p nils-plan-issue live_sprint_commands; pr-grouping=per-sprint; pr-group=s4; shared-pr-anchor=S4T1 |
"#,
    )
}

fn issue_body_sprint4_in_progress() -> String {
    issue_body_with_preface(
        r#"| S3T1 | Implement task-spec generation core using `plan-tooling` | subagent-s3-t1 | feat/s3-t1-implement-task-spec-generation-core-using-plan-t | feat-s3-t1 | per-sprint | #221 | done | sprint=S3; plan-task:Task 3.1 |
| S3T2 | Implement issue-body and sprint-comment rendering engine | subagent-s3-t1 | feat/s3-t1-implement-task-spec-generation-core-using-plan-t | feat-s3-t1 | per-sprint | #221 | done | sprint=S3; plan-task:Task 3.2 |
| S3T3 | Implement independent local dry-run workflow | subagent-s3-t1 | feat/s3-t1-implement-task-spec-generation-core-using-plan-t | feat-s3-t1 | per-sprint | #221 | done | sprint=S3; plan-task:Task 3.3 |
| S4T1 | Implement GitHub adapter abstraction and `gh` backend | subagent-s4-t1 | feat/s4-t1-implement-github-adapter-abstraction-and-gh-back | feat-s4-t1 | per-sprint | #222 | in-progress | sprint=S4; plan-task:Task 4.1; deps=Task 3.3; validate=cargo test -p nils-plan-issue github_adapter; pr-grouping=per-sprint; pr-group=s4; shared-pr-anchor=S4T1 |
| S4T2 | Implement live plan-level commands | subagent-s4-t1 | feat/s4-t1-implement-github-adapter-abstraction-and-gh-back | feat-s4-t1 | per-sprint | #222 | in-progress | sprint=S4; plan-task:Task 4.2; deps=Task 4.1; validate=cargo test -p nils-plan-issue live_plan_commands; pr-grouping=per-sprint; pr-group=s4; shared-pr-anchor=S4T1 |
| S4T3 | Implement live sprint-level commands and guide output | subagent-s4-t1 | feat/s4-t1-implement-github-adapter-abstraction-and-gh-back | feat-s4-t1 | per-sprint | #222 | in-progress | sprint=S4; plan-task:Task 4.3; deps=Task 4.1; validate=cargo test -p nils-plan-issue live_sprint_commands; pr-grouping=per-sprint; pr-group=s4; shared-pr-anchor=S4T1 |
"#,
    )
}

fn issue_body_plan_done() -> String {
    issue_body_with_preface(
        r#"| S4T1 | Implement GitHub adapter abstraction and `gh` backend | subagent-s4-t1 | feat/s4-t1-implement-github-adapter-abstraction-and-gh-back | feat-s4-t1 | per-sprint | #222 | done | sprint=S4; plan-task:Task 4.1; deps=Task 3.3; validate=cargo test -p nils-plan-issue github_adapter; pr-grouping=per-sprint; pr-group=s4; shared-pr-anchor=S4T1 |
| S4T2 | Implement live plan-level commands | subagent-s4-t1 | feat/s4-t1-implement-github-adapter-abstraction-and-gh-back | feat-s4-t1 | per-sprint | #222 | done | sprint=S4; plan-task:Task 4.2; deps=Task 4.1; validate=cargo test -p nils-plan-issue live_plan_commands; pr-grouping=per-sprint; pr-group=s4; shared-pr-anchor=S4T1 |
| S4T3 | Implement live sprint-level commands and guide output | subagent-s4-t1 | feat/s4-t1-implement-github-adapter-abstraction-and-gh-back | feat-s4-t1 | per-sprint | #222 | done | sprint=S4; plan-task:Task 4.3; deps=Task 4.1; validate=cargo test -p nils-plan-issue live_sprint_commands; pr-grouping=per-sprint; pr-group=s4; shared-pr-anchor=S4T1 |
"#,
    )
}

#[test]
fn github_adapter_live_commands_use_forge_cli_backend_for_issue_and_pr_state() {
    let tmp = TempDir::new().expect("temp dir");
    let stub = StubBinDir::new();
    stub.write_exe("forge-cli", common::forge_cli_stub_script());

    let log_path = tmp.path().join("forge-cli.log");
    let log_s = log_path.to_string_lossy().to_string();

    let state_dir = tmp.path().join("state-dir");
    fs::create_dir_all(&state_dir).expect("agent home");
    let state_dir_s = state_dir.to_string_lossy().to_string();

    let body_json = json!(issue_body_sprint4_in_progress()).to_string();

    let out = common::run_plan_issue_with_options(
        &[
            "--format",
            "json",
            "--dry-run",
            "--repo",
            "sympoies/nils-cli",
            "accept-sprint",
            "--plan",
            PLAN_PATH,
            "--issue",
            "217",
            "--sprint",
            "4",
            "--approved-comment-url",
            "https://github.com/sympoies/nils-cli/issues/217#issuecomment-4000000000",
            "--pr-grouping",
            "per-sprint",
            "--no-comment",
        ],
        forge_cmd_options(
            stub.path(),
            &[
                ("FORGE_CLI_STUB_LOG", &log_s),
                ("FORGE_CLI_STUB_VIEW_BODY_JSON", &body_json),
                ("PLAN_ISSUE_HOME", &state_dir_s),
            ],
        ),
    );

    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let payload = out.stdout_json();
    assert_eq!(payload["command"], "accept-sprint");
    assert_eq!(payload["status"], "ok");

    let log = fs::read_to_string(&log_path).expect("read log");
    assert!(log.contains("issue view 217"), "{log}");
    assert!(log.contains("pr view 222"), "{log}");
}

#[test]
fn live_plan_commands_ready_and_close_follow_gate_contracts() {
    let tmp = TempDir::new().expect("temp dir");
    let stub = StubBinDir::new();
    stub.write_exe("forge-cli", common::forge_cli_stub_script());

    let log_path = tmp.path().join("forge-cli.log");
    let log_s = log_path.to_string_lossy().to_string();

    let state_dir = tmp.path().join("state-dir");
    fs::create_dir_all(&state_dir).expect("agent home");
    let state_dir_s = state_dir.to_string_lossy().to_string();

    let comment_capture = tmp.path().join("ready-plan-comment.md");
    let comment_capture_s = comment_capture.to_string_lossy().to_string();

    let body_json = json!(issue_body_plan_done()).to_string();

    let ready_out = common::run_plan_issue_with_options(
        &[
            "--format",
            "json",
            "--repo",
            "sympoies/nils-cli",
            "ready-plan",
            "--issue",
            "217",
            "--summary",
            "Final plan review",
        ],
        forge_cmd_options(
            stub.path(),
            &[
                ("FORGE_CLI_STUB_LOG", &log_s),
                ("FORGE_CLI_STUB_VIEW_BODY_JSON", &body_json),
                ("FORGE_CLI_STUB_CAPTURE_COMMENT_FILE", &comment_capture_s),
                ("PLAN_ISSUE_HOME", &state_dir_s),
            ],
        ),
    );

    assert_eq!(ready_out.code, 0, "stderr: {}", ready_out.stderr_text());
    let ready_payload = ready_out.stdout_json();
    assert_eq!(ready_payload["command"], "ready-plan");
    assert_eq!(
        ready_payload["payload"]["result"]["label_update_applied"],
        false
    );

    let close_body_path = tmp.path().join("close-body.md");
    fs::write(&close_body_path, issue_body_plan_done()).expect("write close body");
    let close_body_s = close_body_path.to_string_lossy().to_string();

    let close_out = common::run_plan_issue_with_options(
        &[
            "--format",
            "json",
            "--dry-run",
            "--repo",
            "sympoies/nils-cli",
            "close-plan",
            "--body-file",
            &close_body_s,
            "--approved-comment-url",
            "https://github.com/sympoies/nils-cli/issues/217#issuecomment-4000000001",
        ],
        forge_cmd_options(
            stub.path(),
            &[
                ("FORGE_CLI_STUB_LOG", &log_s),
                ("FORGE_CLI_STUB_VIEW_BODY_JSON", &body_json),
                ("PLAN_ISSUE_HOME", &state_dir_s),
            ],
        ),
    );

    assert_eq!(close_out.code, 0, "stderr: {}", close_out.stderr_text());
    let close_payload = close_out.stdout_json();
    assert_eq!(close_payload["command"], "close-plan");
    assert_eq!(close_payload["payload"]["result"]["issue_closed"], false);

    // forge-cli argv ordering: the `--repo <slug>` prefix precedes the
    // subcommand (the stub log strips only `--format json --provider <p>`).
    let log = fs::read_to_string(&log_path).expect("read log");
    assert!(!log.contains("--add-label needs-review"), "{log}");
    assert!(
        log.contains("--repo sympoies/nils-cli issue comment 217 --body-file"),
        "{log}"
    );
    assert!(
        log.contains("--repo sympoies/nils-cli pr view 222"),
        "{log}"
    );
}

#[test]
fn live_ready_plan_label_update_flag_applies_review_label() {
    let tmp = TempDir::new().expect("temp dir");
    let stub = StubBinDir::new();
    stub.write_exe("forge-cli", common::forge_cli_stub_script());

    let log_path = tmp.path().join("forge-cli.log");
    let log_s = log_path.to_string_lossy().to_string();

    let state_dir = tmp.path().join("state-dir");
    fs::create_dir_all(&state_dir).expect("agent home");
    let state_dir_s = state_dir.to_string_lossy().to_string();

    let body_json = json!(issue_body_plan_done()).to_string();

    let ready_out = common::run_plan_issue_with_options(
        &[
            "--format",
            "json",
            "--repo",
            "sympoies/nils-cli",
            "ready-plan",
            "--issue",
            "217",
            "--summary",
            "Final plan review",
            "--label-update",
            "--no-comment",
        ],
        forge_cmd_options(
            stub.path(),
            &[
                ("FORGE_CLI_STUB_LOG", &log_s),
                ("FORGE_CLI_STUB_VIEW_BODY_JSON", &body_json),
                ("PLAN_ISSUE_HOME", &state_dir_s),
            ],
        ),
    );

    assert_eq!(ready_out.code, 0, "stderr: {}", ready_out.stderr_text());
    let payload = ready_out.stdout_json();
    assert_eq!(payload["command"], "ready-plan");
    assert_eq!(payload["payload"]["result"]["label_update_requested"], true);
    assert_eq!(payload["payload"]["result"]["label_update_applied"], true);

    // forge-cli argv: `--repo <slug>` prefix precedes the `issue edit` verb.
    let log = fs::read_to_string(&log_path).expect("read log");
    assert!(
        log.contains("--repo sympoies/nils-cli issue edit 217 --add-label needs-review"),
        "{log}"
    );
}

#[test]
fn live_sprint_commands_start_ready_accept_and_guide_are_deterministic() {
    let tmp = TempDir::new().expect("temp dir");
    let stub = StubBinDir::new();
    stub.write_exe("forge-cli", common::forge_cli_stub_script());

    let log_path = tmp.path().join("forge-cli.log");
    let log_s = log_path.to_string_lossy().to_string();

    let state_dir = tmp.path().join("state-dir");
    fs::create_dir_all(&state_dir).expect("agent home");
    let state_dir_s = state_dir.to_string_lossy().to_string();

    let start_body_json = json!(issue_body_sprint4_planned()).to_string();

    let start_out = common::run_plan_issue_with_options(
        &[
            "--format",
            "json",
            "--repo",
            "sympoies/nils-cli",
            "start-sprint",
            "--plan",
            PLAN_PATH,
            "--issue",
            "217",
            "--sprint",
            "4",
            "--pr-grouping",
            "per-sprint",
            "--no-comment",
        ],
        forge_cmd_options(
            stub.path(),
            &[
                ("FORGE_CLI_STUB_LOG", &log_s),
                ("FORGE_CLI_STUB_VIEW_BODY_JSON", &start_body_json),
                ("PLAN_ISSUE_HOME", &state_dir_s),
            ],
        ),
    );

    assert_eq!(start_out.code, 0, "stderr: {}", start_out.stderr_text());
    let start_payload = start_out.stdout_json();
    assert_eq!(start_payload["command"], "start-sprint");
    assert_eq!(start_payload["payload"]["result"]["synced_issue_rows"], 3);
    assert_eq!(
        start_payload["payload"]["result"]["live_mutations_performed"],
        false
    );
    let start_spec_path = start_payload["payload"]["result"]["task_spec_path"]
        .as_str()
        .expect("start task-spec path");
    let start_spec = fs::read_to_string(start_spec_path).expect("read start task-spec");
    assert!(start_spec.contains("subagent-s4-t1"), "{start_spec}");
    assert!(
        start_spec.contains("feat/s4-t1-implement-github-adapter-abstraction-and-gh-back"),
        "{start_spec}"
    );
    assert!(
        start_spec.contains("pr-grouping=per-sprint"),
        "{start_spec}"
    );

    let ready_out = common::run_plan_issue_with_options(
        &[
            "--format",
            "json",
            "--dry-run",
            "--repo",
            "sympoies/nils-cli",
            "ready-sprint",
            "--plan",
            PLAN_PATH,
            "--issue",
            "217",
            "--sprint",
            "4",
            "--pr-grouping",
            "per-sprint",
            "--summary",
            "Sprint 4 ready",
            "--no-comment",
        ],
        forge_cmd_options(
            stub.path(),
            &[
                ("FORGE_CLI_STUB_LOG", &log_s),
                ("FORGE_CLI_STUB_VIEW_BODY_JSON", &start_body_json),
                ("PLAN_ISSUE_HOME", &state_dir_s),
            ],
        ),
    );

    assert_eq!(ready_out.code, 0, "stderr: {}", ready_out.stderr_text());

    let accept_capture = tmp.path().join("accept-sprint-body.md");
    let accept_capture_s = accept_capture.to_string_lossy().to_string();
    let accept_body_json = json!(issue_body_sprint4_in_progress()).to_string();

    let accept_out = common::run_plan_issue_with_options(
        &[
            "--format",
            "json",
            "--repo",
            "sympoies/nils-cli",
            "accept-sprint",
            "--plan",
            PLAN_PATH,
            "--issue",
            "217",
            "--sprint",
            "4",
            "--approved-comment-url",
            "https://github.com/sympoies/nils-cli/issues/217#issuecomment-4000000002",
            "--pr-grouping",
            "per-sprint",
            "--no-comment",
        ],
        forge_cmd_options(
            stub.path(),
            &[
                ("FORGE_CLI_STUB_LOG", &log_s),
                ("FORGE_CLI_STUB_VIEW_BODY_JSON", &accept_body_json),
                ("FORGE_CLI_STUB_CAPTURE_BODY_FILE", &accept_capture_s),
                ("PLAN_ISSUE_HOME", &state_dir_s),
            ],
        ),
    );

    assert_eq!(accept_out.code, 0, "stderr: {}", accept_out.stderr_text());
    let accept_body = fs::read_to_string(&accept_capture).expect("captured accept body");
    assert!(
        accept_body.contains("## Overview"),
        "preface should be preserved\n{accept_body}"
    );
    assert!(accept_body.contains("| S4T1 |"), "{accept_body}");
    assert!(accept_body.contains("| done |"), "{accept_body}");

    let guide_out = common::run_plan_issue(&[
        "--format",
        "json",
        "--dry-run",
        "multi-sprint-guide",
        "--plan",
        PLAN_PATH,
        "--from-sprint",
        "3",
        "--to-sprint",
        "4",
    ]);
    assert_eq!(guide_out.code, 0, "stderr: {}", guide_out.stderr_text());
    let guide_payload = guide_out.stdout_json();
    let guide_text = guide_payload["payload"]["result"]["guide"]
        .as_str()
        .unwrap_or_default();
    assert!(
        guide_text.contains("MULTI_SPRINT_GUIDE_BEGIN"),
        "{guide_text}"
    );
    assert!(guide_text.contains("STEP_1="), "{guide_text}");
    assert!(
        guide_text.contains("MULTI_SPRINT_GUIDE_END"),
        "{guide_text}"
    );
}

#[test]
fn github_adapter_rejects_literal_escaped_newline_without_force() {
    let tmp = TempDir::new().expect("temp dir");
    let stub = StubBinDir::new();
    stub.write_exe("forge-cli", common::forge_cli_stub_script());

    let log_path = tmp.path().join("forge-cli.log");
    let log_s = log_path.to_string_lossy().to_string();

    let state_dir = tmp.path().join("state-dir");
    fs::create_dir_all(&state_dir).expect("agent home");
    let state_dir_s = state_dir.to_string_lossy().to_string();

    let body_json = json!(issue_body_plan_done()).to_string();

    let out = common::run_plan_issue_with_options(
        &[
            "--format",
            "json",
            "--repo",
            "sympoies/nils-cli",
            "ready-plan",
            "--issue",
            "217",
            "--summary",
            r"Final plan review\nPlease confirm",
        ],
        forge_cmd_options(
            stub.path(),
            &[
                ("FORGE_CLI_STUB_LOG", &log_s),
                ("FORGE_CLI_STUB_VIEW_BODY_JSON", &body_json),
                ("PLAN_ISSUE_HOME", &state_dir_s),
            ],
        ),
    );

    assert_eq!(out.code, 1, "stderr: {}", out.stderr_text());
    let payload = out.stdout_json();
    assert_eq!(payload["status"], "error");
    assert_eq!(payload["error"]["code"], "github-comment-failed");

    // Post-consolidation the escaped-control guard is enforced by forge-cli's
    // write ops (`markdown_escaped_control`), not a plan-issue-side guard. The
    // surfaced message carries the offending `\n` sequence and forge-cli's
    // "wrap them in a code span" fix hint.
    let message = payload["error"]["message"].as_str().unwrap_or_default();
    assert!(message.contains(r"\n"), "{message}");
    assert!(message.contains("code span"), "{message}");

    let log = fs::read_to_string(&log_path).expect("read log");
    assert!(
        log.contains("--repo sympoies/nils-cli issue view 217"),
        "{log}"
    );
    // The comment was attempted (forge-cli rejects it), so an `issue comment`
    // call IS logged — but it returns the validation error rather than posting.
    // The plan-issue run still fails with `github-comment-failed`.
}

#[test]
fn github_adapter_force_does_not_bypass_forge_cli_markdown_guard() {
    // Behavioral change from the consolidation: the escaped-control markdown
    // guard now lives in forge-cli's write ops, which has no plan-issue
    // `--force` bypass (plan-issue does not forward `--force` to forge-cli).
    // So `--force` no longer lets a literal escaped-control payload through —
    // it is rejected exactly as the non-force path is.
    let tmp = TempDir::new().expect("temp dir");
    let stub = StubBinDir::new();
    stub.write_exe("forge-cli", common::forge_cli_stub_script());

    let log_path = tmp.path().join("forge-cli.log");
    let log_s = log_path.to_string_lossy().to_string();

    let state_dir = tmp.path().join("state-dir");
    fs::create_dir_all(&state_dir).expect("agent home");
    let state_dir_s = state_dir.to_string_lossy().to_string();

    let body_json = json!(issue_body_plan_done()).to_string();

    let out = common::run_plan_issue_with_options(
        &[
            "--format",
            "json",
            "--force",
            "--repo",
            "sympoies/nils-cli",
            "ready-plan",
            "--issue",
            "217",
            "--summary",
            r"Final plan review\nPlease confirm",
        ],
        forge_cmd_options(
            stub.path(),
            &[
                ("FORGE_CLI_STUB_LOG", &log_s),
                ("FORGE_CLI_STUB_VIEW_BODY_JSON", &body_json),
                ("PLAN_ISSUE_HOME", &state_dir_s),
            ],
        ),
    );

    assert_eq!(out.code, 1, "stderr: {}", out.stderr_text());
    let payload = out.stdout_json();
    assert_eq!(payload["status"], "error");
    assert_eq!(payload["error"]["code"], "github-comment-failed");
    let message = payload["error"]["message"].as_str().unwrap_or_default();
    assert!(message.contains(r"\n"), "{message}");
}
