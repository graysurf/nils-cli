use std::fs;
use std::path::PathBuf;

use pretty_assertions::assert_eq;
use serde_json::Value;
use tempfile::TempDir;

use crate::common;
const DUCK_PLAN: &str = "crates/plan-tooling/tests/fixtures/split_prs/duck-plan.md";
const SHELL_PARITY_DIR: &str = "tests/fixtures/shell_parity";

fn parse_json(stdout: &str) -> Value {
    serde_json::from_str(stdout).expect("stdout should be valid JSON")
}

fn shell_fixture(name: &str) -> String {
    let path = PathBuf::from(SHELL_PARITY_DIR).join(name);
    fs::read_to_string(&path).unwrap_or_else(|err| {
        panic!("failed to read fixture {}: {err}", path.display());
    })
}

fn normalize_shell_text(text: &str, state_dir: &str) -> String {
    text.trim()
        .replace(state_dir, "$PLAN_ISSUE_HOME")
        .replace("\\<", "<")
        .replace("\\>", ">")
}

#[test]
fn parity_shell_help_surface_tracks_current_command_contract() {
    let out = common::run_plan_issue_local(&["--help"]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);

    for token in [
        "Commands:",
        "build-task-spec       Build sprint-scoped task-spec TSV from a plan",
        "build-plan-task-spec  Build plan-scoped task-spec TSV (all sprints) for the single plan issue",
        "start-plan            Open one plan issue with all plan tasks in Task Decomposition",
        "status-plan           Wrapper of issue-delivery-loop status for the plan issue",
        "link-pr               Link PR to task rows and set runtime status (default: in-progress)",
        "ready-plan            Wrapper of issue-delivery-loop ready-for-review for final plan review",
        "close-plan            Close the single plan issue after final approval + merged PR gates, then enforce worktree cleanup",
        "cleanup-worktrees     Enforce cleanup of all issue-assigned task worktrees",
        "start-sprint          Start sprint from Task Decomposition runtime truth after previous sprint merge+done gate passes",
        "ready-sprint          Post sprint-ready comment for main-agent review before merge",
        "accept-sprint         Enforce merged-PR gate, sync sprint status=done, then post accepted comment",
        "multi-sprint-guide    Print the full repeated command flow for a plan (1 plan = 1 issue)",
        "resolve-approval      Resolve the URL of the most recent `Decision: merge` review-evidence comment on a PR, suitable for `accept-sprint --approved-comment-url`",
        "record                Render and audit issue-backed plan record dashboards and comments",
        "completion            Export shell completion script",
    ] {
        assert!(
            out.stdout.contains(token),
            "help output missing token `{token}`\n{}",
            out.stdout
        );
    }

    for token in [
        "--repo <owner/repo>",
        "Pass-through repository target for GitHub operations",
        "--dry-run",
        "Print write actions without mutating GitHub state",
        "--force",
        "Bypass markdown payload guard for GitHub body/comment writes",
    ] {
        assert!(
            out.stdout.contains(token),
            "help output missing token `{token}`\n{}",
            out.stdout
        );
    }
}

#[test]
fn parity_shell_completion_scripts_emit_expected_headers() {
    let bash_out = common::run_plan_issue(&["completion", "bash"]);
    assert_eq!(bash_out.code, 0, "stderr: {}", bash_out.stderr);
    assert!(
        bash_out.stdout.contains("complete -F"),
        "{}",
        bash_out.stdout
    );
    assert!(
        bash_out.stdout.contains("plan-issue"),
        "{}",
        bash_out.stdout
    );

    let zsh_out = common::run_plan_issue_local(&["completion", "zsh"]);
    assert_eq!(zsh_out.code, 0, "stderr: {}", zsh_out.stderr);
    assert!(
        zsh_out.stdout.contains("#compdef plan-issue-local"),
        "{}",
        zsh_out.stdout
    );
}

#[test]
fn parity_shell_multi_sprint_guide_matches_shell_fixture_after_normalization() {
    let tmp = TempDir::new().expect("temp dir");
    let state_dir = tmp.path().join("state-dir");
    fs::create_dir_all(&state_dir).expect("agent home");
    let state_dir_s = state_dir.to_string_lossy().to_string();

    let out = common::run_plan_issue_local_with_env(
        &[
            "--format",
            "json",
            "--dry-run",
            "multi-sprint-guide",
            "--plan",
            DUCK_PLAN,
            "--from-sprint",
            "1",
            "--to-sprint",
            "2",
        ],
        &[("PLAN_ISSUE_HOME", &state_dir_s)],
    );
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);

    let payload = parse_json(&out.stdout);
    let actual = payload["payload"]["result"]["guide"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let expected = shell_fixture("multi_sprint_guide_dry_run.txt");

    assert_eq!(
        normalize_shell_text(&actual, &state_dir_s),
        normalize_shell_text(&expected, &state_dir_s),
    );
}

#[test]
fn command_guardrails_local_issue_path_error_contains_subcommand_specific_guidance() {
    let out = common::run_plan_issue_local(&["--format", "json", "status-plan", "--issue", "217"]);
    assert_eq!(out.code, 64, "stderr: {}", out.stderr);

    let payload = parse_json(&out.stdout);
    assert_eq!(payload["status"], "error");
    assert_eq!(payload["error"]["code"], "live-command-unavailable");

    let message = payload["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("status-plan --issue <number>"),
        "{message}"
    );
    assert!(
        message.contains("status-plan --body-file <path> --dry-run"),
        "{message}"
    );
}

#[test]
fn parity_shell_start_comment_template_matches_shell_fixture() {
    let tmp = TempDir::new().expect("temp dir");
    let state_dir = tmp.path().join("state-dir");
    fs::create_dir_all(&state_dir).expect("agent home");
    let state_dir_s = state_dir.to_string_lossy().to_string();

    let out = common::run_plan_issue_local_with_env(
        &[
            "--format",
            "json",
            "--dry-run",
            "start-sprint",
            "--plan",
            DUCK_PLAN,
            "--issue",
            "217",
            "--sprint",
            "1",
            "--pr-grouping",
            "group",
            "--pr-group",
            "S1T1=s1-foundation",
            "--pr-group",
            "S1T2=s1-fixtures",
            "--pr-group",
            "S1T3=s1-fixtures",
            "--no-comment",
        ],
        &[("PLAN_ISSUE_HOME", &state_dir_s)],
    );
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);

    let payload = parse_json(&out.stdout);
    let comment_path = payload["payload"]["result"]["comment_path"]
        .as_str()
        .expect("comment path");
    let actual = fs::read_to_string(comment_path).expect("read rendered comment");
    let expected = shell_fixture("comment_template_start.md");

    assert!(actual.contains("pr-isolated"), "{actual}");
    assert!(actual.contains("pr-shared"), "{actual}");
    assert_eq!(actual.trim(), expected.trim());
}

#[test]
fn command_guardrails_close_plan_requires_body_file_in_dry_run_mode() {
    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "--dry-run",
        "close-plan",
        "--issue",
        "217",
        "--approved-comment-url",
        "https://github.com/sympoies/nils-cli/issues/217#issuecomment-5000000001",
    ]);

    assert_eq!(out.code, 1, "stderr: {}", out.stderr);
    let payload = parse_json(&out.stdout);
    assert_eq!(payload["status"], "error");
    assert_eq!(payload["error"]["code"], "missing-body-file");
    assert_eq!(
        payload["error"]["message"],
        "--body-file is required for close-plan --dry-run"
    );
}

#[test]
fn command_guardrails_close_plan_rejects_body_file_without_dry_run() {
    let tmp = TempDir::new().expect("temp dir");
    let body_file = tmp.path().join("body.md");
    fs::write(&body_file, "## Task Decomposition\n").expect("body file");
    let body_file_s = body_file.to_string_lossy().to_string();

    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "close-plan",
        "--issue",
        "217",
        "--body-file",
        &body_file_s,
        "--approved-comment-url",
        "https://github.com/sympoies/nils-cli/issues/217#issuecomment-5000000002",
    ]);

    assert_eq!(out.code, 1, "stderr: {}", out.stderr);
    let payload = parse_json(&out.stdout);
    assert_eq!(payload["status"], "error");
    assert_eq!(payload["error"]["code"], "conflicting-issue-source");
    assert_eq!(
        payload["error"]["message"],
        "use either --issue or --body-file for close-plan, not both"
    );
}

#[test]
fn command_guardrails_close_plan_requires_github_comment_url_format() {
    let tmp = TempDir::new().expect("temp dir");
    let body_file = tmp.path().join("body.md");
    fs::write(&body_file, "placeholder body").expect("body file");
    let body_file_s = body_file.to_string_lossy().to_string();

    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "--dry-run",
        "close-plan",
        "--body-file",
        &body_file_s,
        "--approved-comment-url",
        "https://example.com/not-github-comment",
    ]);

    assert_eq!(out.code, 64, "stderr: {}", out.stderr);
    let payload = parse_json(&out.stdout);
    assert_eq!(payload["status"], "error");
    assert_eq!(payload["error"]["code"], "invalid-approval-comment-url");
    assert_eq!(
        payload["error"]["message"],
        "--approved-comment-url must be a GitHub issue/pull comment URL"
    );
}

#[test]
fn command_guardrails_multi_sprint_guide_rejects_reverse_range() {
    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "multi-sprint-guide",
        "--plan",
        DUCK_PLAN,
        "--from-sprint",
        "3",
        "--to-sprint",
        "1",
    ]);

    assert_eq!(out.code, 1, "stderr: {}", out.stderr);
    let payload = parse_json(&out.stdout);
    assert_eq!(payload["status"], "error");
    assert_eq!(payload["error"]["code"], "invalid-sprint-range");
    assert_eq!(
        payload["error"]["message"],
        "--from-sprint must be <= --to-sprint"
    );
}

#[test]
fn json_contract_multi_sprint_guide_success_envelope_is_stable() {
    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "--dry-run",
        "multi-sprint-guide",
        "--plan",
        DUCK_PLAN,
        "--from-sprint",
        "1",
        "--to-sprint",
        "2",
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);

    let payload = parse_json(&out.stdout);
    assert_eq!(
        payload["schema_version"],
        "plan-issue-cli.multi.sprint.guide.v1"
    );
    assert_eq!(payload["command"], "multi-sprint-guide");
    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["payload"]["binary"], "plan-issue");
    assert_eq!(payload["payload"]["execution_mode"], "live");
    assert_eq!(payload["payload"]["dry_run"], true);
    assert_eq!(payload["payload"]["arguments"]["from_sprint"], 1);
    assert_eq!(payload["payload"]["arguments"]["to_sprint"], 2);
    assert!(
        payload["payload"]["result"]["guide"]
            .as_str()
            .is_some_and(|guide| guide.contains("MULTI_SPRINT_GUIDE_BEGIN")),
        "{}",
        out.stdout
    );
    assert!(
        payload["payload"]["result"]["guide"]
            .as_str()
            .is_some_and(|guide| guide.contains("MODE=DRY_RUN_LIVE_BINARY")),
        "{}",
        out.stdout
    );
}

#[test]
fn json_contract_guardrail_error_envelope_is_stable() {
    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "--dry-run",
        "close-plan",
        "--issue",
        "217",
        "--approved-comment-url",
        "https://github.com/sympoies/nils-cli/issues/217#issuecomment-5000000003",
    ]);
    assert_eq!(out.code, 1, "stderr: {}", out.stderr);

    let payload = parse_json(&out.stdout);
    assert_eq!(payload["schema_version"], "plan-issue-cli.close.plan.v1");
    assert_eq!(payload["command"], "close-plan");
    assert_eq!(payload["status"], "error");
    assert_eq!(payload["error"]["code"], "missing-body-file");
    assert!(payload["error"]["message"].is_string(), "{}", out.stdout);
}

#[test]
fn json_contract_local_binary_success_envelope_is_stable() {
    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "build-task-spec",
        "--plan",
        "crates/plan-issue-cli/tests/fixtures/plans/plan-issue-rust-cli-full-delivery-plan.md",
        "--sprint",
        "2",
        "--pr-grouping",
        "per-sprint",
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);

    let payload = parse_json(&out.stdout);
    assert_eq!(payload["command"], "build-task-spec");
    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["payload"]["binary"], "plan-issue-local");
    assert_eq!(payload["payload"]["execution_mode"], "local");
}
