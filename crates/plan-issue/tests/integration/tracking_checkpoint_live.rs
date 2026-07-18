//! `plan-issue tracking checkpoint --live` posting coverage.
//!
//! Sister file to `tracking_checkpoint_dry_run.rs` and
//! `tracking_checkpoint_refusals.rs`. These tests cover the fixture-mode
//! posting hop that landed when the
//! `tracking-checkpoint-live-not-implemented` blocked code was retired from
//! the live branch (inbox: `tracking-closeout-review-state-complete-gap`).
//!
//! True provider-bound live mode (real `gh`/`forge-cli` adapter calls) is
//! covered by the live integration suites against fixture provider
//! adapters; the deterministic behavior under `--fixture` is what runtime
//! smoke and the runtime-kit happy-path probe depend on, so it is the
//! primary coverage target here.

use std::fs;
use std::path::{Path, PathBuf};

use pretty_assertions::{assert_eq, assert_ne};
use serde_json::{Value, json};
use tempfile::TempDir;

use nils_test_support::StubBinDir;
use nils_test_support::cmd::CmdOptions;
use plan_issue::commands::record::RecordProfile;
use plan_issue::lifecycle_record::PAYLOAD_SCHEMA_V2;

use crate::common;

fn v2_comment(role: &str, profile: &str, data: Value, visible: &str) -> String {
    let envelope = json!({
        "schema": PAYLOAD_SCHEMA_V2,
        "role": role,
        "profile": profile,
        "data": data,
    });
    let payload = serde_json::to_string(&envelope).expect("serialize");
    format!(
        "<!-- plan-issue-record:v2 role={role} profile={profile} -->\n\n{visible}\n\n```plan-issue-record-payload\n{payload}\n```\n",
    )
}

fn write_fixture(roles: &[(&str, Value, &str, &str)]) -> TempDir {
    let tmp = TempDir::new().expect("tmp");
    fs::write(tmp.path().join("body.md"), "## Current Dashboard\n").expect("body");
    let comments: Vec<Value> = roles
        .iter()
        .enumerate()
        .map(|(idx, (role, data, visible, at))| {
            json!({
                "url": format!("https://example.com/c{idx}"),
                "created_at": at,
                "body": v2_comment(role, "tracking", data.clone(), visible),
            })
        })
        .collect();
    fs::write(
        tmp.path().join("comments.json"),
        json!({"comments": comments}).to_string(),
    )
    .expect("comments");
    tmp
}

fn write_run_state(path: &std::path::Path, phase: &str) {
    let body = json!({
        "schema": "plan-issue.execution-run.v1",
        "run_id": "run-1",
        "repo": "owner/repo",
        "issue": 999,
        "profile": "tracking",
        "phase": phase,
        "created_at": "2026-05-26T00:00:00Z",
        "updated_at": "2026-05-26T01:00:00Z",
        "selected_scope": {
            "task": "1.2",
            "title": "demo"
        },
        "branch": "feat/x",
        "review": {
            "decision": "approve",
            "lenses": ["testing"],
            "evidence": null
        }
    });
    fs::write(path, body.to_string()).expect("run-state");
}

fn write_provider_bound_run_state(path: &Path, repo: &str, provider: &str, host: &str, issue: u64) {
    let body = json!({
        "schema": "plan-issue.execution-run.v1",
        "run_id": "run-provider-bound",
        "repo": repo,
        "repo_provider": provider,
        "repo_host": host,
        "issue": issue,
        "profile": "tracking",
        "phase": "ready_for_close",
        "created_at": "2026-05-26T00:00:00Z",
        "updated_at": "2026-05-26T01:00:00Z",
        "selected_scope": {
            "task": "1.2",
            "title": "demo"
        },
        "branch": "feat/x",
        "review": {
            "decision": "approve",
            "lenses": ["testing"],
            "evidence": null
        }
    });
    fs::write(path, body.to_string()).expect("provider-bound run-state");
}

fn write_bound_execution_state(
    tmp: &TempDir,
    repo: &str,
    provider: &str,
    host: &str,
    execution_state_contents: &str,
) -> (PathBuf, PathBuf) {
    let checkout = tmp.path().join("checkout");
    fs::create_dir(&checkout).expect("checkout");
    assert!(
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&checkout)
            .status()
            .expect("git init")
            .success()
    );
    let remote = format!("https://{host}/{repo}.git");
    assert!(
        std::process::Command::new("git")
            .args(["remote", "add", "origin", &remote])
            .current_dir(&checkout)
            .status()
            .expect("git remote add")
            .success()
    );
    let execution_state = checkout.join("provider-execution-state.md");
    fs::write(&execution_state, execution_state_contents).expect("execution state");
    let run_state = tmp.path().join("run-state.json");
    write_provider_bound_run_state(&run_state, repo, provider, host, 999);
    let mut run: Value = serde_json::from_str(&fs::read_to_string(&run_state).expect("run state"))
        .expect("run json");
    run["execution_state_file"] = json!(execution_state);
    fs::write(&run_state, run.to_string()).expect("bound run state");
    (run_state, execution_state)
}

fn provider_stub_options(stub_dir: &Path, envs: &[(&str, &str)]) -> CmdOptions {
    common::plan_issue_cmd_options()
        .with_env_remove_prefix("FORGE_CLI_STUB_")
        .with_path_prepend(stub_dir)
        .with_envs(envs)
}

fn provider_evidence_env(fixture: &TempDir) -> (String, String) {
    let body = fs::read_to_string(fixture.path().join("body.md")).expect("fixture body");
    let comments: Value = serde_json::from_str(
        &fs::read_to_string(fixture.path().join("comments.json")).expect("fixture comments"),
    )
    .expect("comments json");
    (json!(body).to_string(), comments["comments"].to_string())
}

/// Pre-closeout fixture with source/plan present plus a `status=in-progress`
/// state comment — mirrors the shape `record open` writes initially. The
/// run-state's `phase=ready_for_close` then drives the controller to render
/// a final `status=complete` state and an `approve` review.
fn pre_closeout_fixture() -> TempDir {
    write_fixture(&[
        (
            "source",
            json!({"path": "p", "commit": "c"}),
            "## Source Snapshot\n\n- Profile: tracking\n- Path: `p`",
            "2026-05-26T00:00:00Z",
        ),
        (
            "plan",
            json!({"path": "p", "commit": "c"}),
            "## Plan Snapshot\n\n- Profile: tracking\n- Path: `p`",
            "2026-05-26T00:00:01Z",
        ),
        (
            "state",
            json!({"status": "in-progress", "target_scope": "x", "tasks": [], "prs": []}),
            "## Execution State\n\n- Profile: tracking\n- Status: in-progress\n\n## Task Ledger\n\n| ID | Status |\n| --- | --- |\n| 1.1 | done |",
            "2026-05-26T00:00:02Z",
        ),
    ])
}

#[test]
fn tracking_checkpoint_live_fixture_returns_posted_state_role_with_synthesized_url() {
    let fixture = pre_closeout_fixture();
    let tmp = TempDir::new().expect("tmp");
    let rs_path = tmp.path().join("run-state.json");
    write_run_state(&rs_path, "ready_for_close");

    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "checkpoint",
        "--run-state",
        rs_path.to_str().expect("rs"),
        "--post",
        "state",
        "--fixture",
        fixture.path().to_str().expect("fixture"),
        "--issue",
        "144",
        "--live",
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());

    let result = out.stdout_json()["payload"]["result"].clone();
    assert_eq!(result["mode"], "fixture");
    // The retired refusal code must not surface on the live path.
    let blocked_codes: Vec<&str> = result["blocked"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["code"].as_str().unwrap())
        .collect();
    assert!(
        !blocked_codes.contains(&"tracking-checkpoint-live-not-implemented"),
        "blocked codes unexpectedly include the retired refusal: {blocked_codes:?}"
    );

    let posted = result["posted"].as_array().expect("posted array");
    assert_eq!(posted.len(), 1, "one comment per role: {posted:?}");
    assert_eq!(posted[0]["role"], "state");
    let url = posted[0]["comment_url"].as_str().expect("url");
    assert_eq!(url, "fixture://issue/144/state");
}

#[test]
fn tracking_checkpoint_live_fixture_refuses_when_lifecycle_lock_is_busy() {
    let fixture = pre_closeout_fixture();
    let tmp = TempDir::new().expect("tmp");
    let state_dir = TempDir::new().expect("state-dir");
    let rs_path = tmp.path().join("run-state.json");
    write_run_state(&rs_path, "ready_for_close");

    plan_issue::state::set_state_dir_override(Some(state_dir.path().to_path_buf()));
    let _busy_lock = plan_issue::lifecycle_lock::acquire_for_identity(
        "local",
        None,
        "owner/repo",
        144,
        RecordProfile::Tracking,
    )
    .expect("pre-acquire lifecycle lock");
    plan_issue::state::set_state_dir_override(None);

    let state_dir_arg = state_dir.path().to_string_lossy().to_string();
    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "--state-dir",
        &state_dir_arg,
        "tracking",
        "checkpoint",
        "--run-state",
        rs_path.to_str().expect("rs"),
        "--post",
        "state",
        "--fixture",
        fixture.path().to_str().expect("fixture"),
        "--issue",
        "144",
        "--live",
    ]);
    assert_eq!(
        out.code,
        1,
        "stdout={} stderr={}",
        out.stdout_text(),
        out.stderr_text()
    );

    let parsed = out.stdout_json();
    assert_eq!(parsed["status"], "error");
    assert_eq!(parsed["error"]["code"], "plan-issue-lifecycle-lock-busy");
    let message = parsed["error"]["message"].as_str().expect("message");
    assert!(message.contains("issue=144"), "{message}");
    assert!(message.contains("profile=tracking"), "{message}");
}

#[test]
fn tracking_checkpoint_live_provider_refuses_busy_lock_before_evidence_fetch() {
    let tmp = TempDir::new().expect("tmp");
    let state_dir = TempDir::new().expect("state-dir");
    let stub = StubBinDir::new();
    stub.write_exe("forge-cli", common::forge_cli_stub_script());
    let rs_path = tmp.path().join("run-state.json");
    let log_path = tmp.path().join("forge-cli.log");
    write_provider_bound_run_state(&rs_path, "group/project", "gitlab", "gitlab.com", 999);

    plan_issue::state::set_state_dir_override(Some(state_dir.path().to_path_buf()));
    let _busy_lock = plan_issue::lifecycle_lock::acquire_for_identity(
        "gitlab",
        Some("gitlab.com"),
        "group/project",
        999,
        RecordProfile::Tracking,
    )
    .expect("pre-acquire lifecycle lock");
    plan_issue::state::set_state_dir_override(None);

    let state_dir_arg = state_dir.path().to_string_lossy().to_string();
    let log_s = log_path.to_string_lossy().to_string();
    let out = common::run_plan_issue_with_options(
        &[
            "--format",
            "json",
            "--state-dir",
            &state_dir_arg,
            "tracking",
            "checkpoint",
            "--run-state",
            rs_path.to_str().expect("run state"),
            "--post",
            "state",
            "--live",
        ],
        provider_stub_options(stub.path(), &[("FORGE_CLI_STUB_LOG", &log_s)]).with_cwd(tmp.path()),
    );
    assert_eq!(out.code, 1, "stderr={}", out.stderr_text());
    assert_eq!(
        out.stdout_json()["error"]["code"],
        "plan-issue-lifecycle-lock-busy"
    );
    assert_eq!(fs::read_to_string(log_path).unwrap_or_default(), "");
}

#[test]
fn tracking_checkpoint_live_refuses_busy_run_state_lock_before_provider_access() {
    let fixture = pre_closeout_fixture();
    let (body_json, comments_json) = provider_evidence_env(&fixture);
    let tmp = TempDir::new().expect("tmp");
    let stub = StubBinDir::new();
    stub.write_exe("forge-cli", common::forge_cli_stub_script());
    let rs_path = tmp.path().join("run-state.json");
    let run_lock = tmp.path().join(".run-state.json.update.lock");
    let log_path = tmp.path().join("forge-cli.log");
    write_provider_bound_run_state(&rs_path, "owner/repo", "github", "github.com", 999);
    let _active_run_lock = plan_tooling::mutation_lock::OwnedFileLock::acquire(&run_lock)
        .expect("hold run-state lock");

    let log_s = log_path.to_string_lossy().to_string();
    let out = common::run_plan_issue_with_options(
        &[
            "--format",
            "json",
            "tracking",
            "checkpoint",
            "--run-state",
            rs_path.to_str().expect("run state"),
            "--post",
            "state",
            "--live",
        ],
        provider_stub_options(
            stub.path(),
            &[
                ("FORGE_CLI_STUB_VIEW_BODY_JSON", &body_json),
                ("FORGE_CLI_STUB_VIEW_COMMENTS_JSON", &comments_json),
                ("FORGE_CLI_STUB_LOG", &log_s),
            ],
        )
        .with_cwd(tmp.path()),
    );

    assert_eq!(
        out.code,
        1,
        "stdout={} stderr={}",
        out.stdout_text(),
        out.stderr_text()
    );
    assert_eq!(
        out.stdout_json()["error"]["code"],
        "tracking-run-update-lock-busy"
    );
    assert_eq!(fs::read_to_string(log_path).unwrap_or_default(), "");
    assert!(run_lock.exists(), "stable advisory run lock path missing");
}

#[test]
fn tracking_checkpoint_live_refuses_busy_execution_state_lock_before_provider_access() {
    let fixture = pre_closeout_fixture();
    let (body_json, comments_json) = provider_evidence_env(&fixture);
    let tmp = TempDir::new().expect("tmp");
    let stub = StubBinDir::new();
    stub.write_exe("forge-cli", common::forge_cli_stub_script());
    assert!(
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(tmp.path())
            .status()
            .expect("git init")
            .success()
    );
    assert!(
        std::process::Command::new("git")
            .args([
                "remote",
                "add",
                "origin",
                "https://github.com/owner/repo.git",
            ])
            .current_dir(tmp.path())
            .status()
            .expect("git remote add")
            .success()
    );
    let execution_state = tmp.path().join("execution-state.md");
    fs::write(
        &execution_state,
        "## Execution State\n\n- Target scope: provider-bound target\n- Tracking issue: <https://github.com/owner/repo/issues/999>\n\n## Task Ledger\n\n| ID | Status | Task | Evidence |\n| --- | --- | --- | --- |\n| 1.2 | done | demo | test |\n",
    )
    .expect("execution state");
    let exec_lock = tmp.path().join("execution-state.md.lock");
    let _active_exec_lock = plan_tooling::mutation_lock::OwnedFileLock::acquire(&exec_lock)
        .expect("hold execution-state lock");
    let rs_path = tmp.path().join("run-state.json");
    let run_lock = tmp.path().join(".run-state.json.update.lock");
    let log_path = tmp.path().join("forge-cli.log");
    write_provider_bound_run_state(&rs_path, "owner/repo", "github", "github.com", 999);
    let mut run: Value =
        serde_json::from_str(&fs::read_to_string(&rs_path).expect("run state")).expect("run json");
    run["execution_state_file"] = json!(execution_state);
    fs::write(&rs_path, run.to_string()).expect("bound run state");

    let log_s = log_path.to_string_lossy().to_string();
    let out = common::run_plan_issue_with_options(
        &[
            "--format",
            "json",
            "tracking",
            "checkpoint",
            "--run-state",
            rs_path.to_str().expect("run state"),
            "--post",
            "state",
            "--live",
        ],
        provider_stub_options(
            stub.path(),
            &[
                ("FORGE_CLI_STUB_VIEW_BODY_JSON", &body_json),
                ("FORGE_CLI_STUB_VIEW_COMMENTS_JSON", &comments_json),
                ("FORGE_CLI_STUB_LOG", &log_s),
            ],
        )
        .with_cwd(tmp.path()),
    );

    assert_eq!(
        out.code,
        1,
        "stdout={} stderr={}",
        out.stdout_text(),
        out.stderr_text()
    );
    assert_eq!(
        out.stdout_json()["error"]["code"],
        "exec-state-mutation-lock-busy"
    );
    assert_eq!(fs::read_to_string(log_path).unwrap_or_default(), "");
    assert!(run_lock.exists(), "stable advisory run lock path missing");
    assert!(
        exec_lock.exists(),
        "stable advisory execution-state lock path missing"
    );
}

#[cfg(unix)]
#[test]
fn tracking_checkpoint_live_rejects_hard_linked_execution_state_before_provider_access() {
    let fixture = pre_closeout_fixture();
    let (body_json, comments_json) = provider_evidence_env(&fixture);
    let tmp = TempDir::new().expect("tmp");
    let stub = StubBinDir::new();
    stub.write_exe("forge-cli", common::forge_cli_stub_script());
    let (run_state, execution_state) = write_bound_execution_state(
        &tmp,
        "owner/repo",
        "github",
        "github.com",
        "## Execution State\n\n- Target scope: provider-bound target\n- Tracking issue: <https://github.com/owner/repo/issues/999>\n\n## Task Ledger\n\n| ID | Status | Task | Evidence |\n| --- | --- | --- | --- |\n| 1.2 | done | demo | test |\n",
    );
    fs::hard_link(&execution_state, tmp.path().join("external-alias.md")).expect("hard-link alias");
    let log_path = tmp.path().join("forge-cli.log");
    let log_s = log_path.to_string_lossy().to_string();

    let out = common::run_plan_issue_with_options(
        &[
            "--format",
            "json",
            "tracking",
            "checkpoint",
            "--run-state",
            run_state.to_str().expect("run state"),
            "--post",
            "state",
            "--live",
        ],
        provider_stub_options(
            stub.path(),
            &[
                ("FORGE_CLI_STUB_VIEW_BODY_JSON", &body_json),
                ("FORGE_CLI_STUB_VIEW_COMMENTS_JSON", &comments_json),
                ("FORGE_CLI_STUB_LOG", &log_s),
            ],
        )
        .with_cwd(execution_state.parent().expect("checkout")),
    );

    assert_eq!(
        out.code,
        1,
        "stdout={} stderr={}",
        out.stdout_text(),
        out.stderr_text()
    );
    assert_eq!(
        out.stdout_json()["error"]["code"],
        "exec-state-unsafe-file-alias"
    );
    assert_eq!(fs::read_to_string(log_path).unwrap_or_default(), "");
}

#[test]
fn tracking_checkpoint_live_holds_local_snapshot_locks_through_provider_post() {
    let fixture = pre_closeout_fixture();
    let (body_json, comments_json) = provider_evidence_env(&fixture);
    let tmp = TempDir::new().expect("tmp");
    let stub = StubBinDir::new();
    stub.write_exe("forge-cli", common::forge_cli_stub_script());
    assert!(
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(tmp.path())
            .status()
            .expect("git init")
            .success()
    );
    assert!(
        std::process::Command::new("git")
            .args([
                "remote",
                "add",
                "origin",
                "https://github.com/owner/repo.git",
            ])
            .current_dir(tmp.path())
            .status()
            .expect("git remote add")
            .success()
    );
    let execution_state = tmp.path().join("execution-state.md");
    fs::write(
        &execution_state,
        "## Execution State\n\n- Target scope: provider-bound target\n- Tracking issue: <https://github.com/owner/repo/issues/999>\n\n## Task Ledger\n\n| ID | Status | Task | Evidence |\n| --- | --- | --- | --- |\n| 1.2 | done | demo | test |\n",
    )
    .expect("execution state");
    let exec_lock = tmp.path().join("execution-state.md.lock");
    let rs_path = tmp.path().join("run-state.json");
    let run_lock = tmp.path().join(".run-state.json.update.lock");
    write_provider_bound_run_state(&rs_path, "owner/repo", "github", "github.com", 999);
    let mut run: Value =
        serde_json::from_str(&fs::read_to_string(&rs_path).expect("run state")).expect("run json");
    run["execution_state_file"] = json!(execution_state);
    fs::write(&rs_path, run.to_string()).expect("bound run state");

    let entered_path = tmp.path().join("comment-entered");
    let continue_path = tmp.path().join("comment-continue");
    let entered_s = entered_path.to_string_lossy().to_string();
    let continue_s = continue_path.to_string_lossy().to_string();
    let stub_path = stub.path().to_path_buf();
    let cwd = tmp.path().to_path_buf();
    let run_state_s = rs_path.to_string_lossy().to_string();
    let command = std::thread::spawn(move || {
        common::run_plan_issue_with_options(
            &[
                "--format",
                "json",
                "tracking",
                "checkpoint",
                "--run-state",
                &run_state_s,
                "--post",
                "state",
                "--live",
            ],
            provider_stub_options(
                &stub_path,
                &[
                    ("FORGE_CLI_STUB_VIEW_BODY_JSON", &body_json),
                    ("FORGE_CLI_STUB_VIEW_COMMENTS_JSON", &comments_json),
                    ("FORGE_CLI_STUB_COMMENT_ENTERED_PATH", &entered_s),
                    ("FORGE_CLI_STUB_COMMENT_CONTINUE_PATH", &continue_s),
                ],
            )
            .with_cwd(&cwd),
        )
    });

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !entered_path.exists() && std::time::Instant::now() < deadline && !command.is_finished() {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    if !entered_path.exists() {
        fs::write(&continue_path, b"continue").expect("release provider callback");
        let out = command.join().expect("checkpoint command thread");
        panic!(
            "provider callback was not reached: stdout={} stderr={}",
            out.stdout_text(),
            out.stderr_text()
        );
    }

    let run_probe = plan_tooling::mutation_lock::OwnedFileLock::acquire(&run_lock);
    let run_busy = matches!(
        run_probe,
        Err(plan_tooling::mutation_lock::OwnedFileLockError::Busy)
    );
    let exec_probe = plan_tooling::mutation_lock::OwnedFileLock::acquire(&exec_lock);
    let exec_busy = matches!(
        exec_probe,
        Err(plan_tooling::mutation_lock::OwnedFileLockError::Busy)
    );
    fs::write(&continue_path, b"continue").expect("release provider callback");
    let out = command.join().expect("checkpoint command thread");

    assert!(run_busy, "run-state lock was not held during provider post");
    assert!(
        exec_busy,
        "execution-state lock was not held during provider post"
    );

    assert_eq!(
        out.code,
        0,
        "stdout={} stderr={}",
        out.stdout_text(),
        out.stderr_text()
    );
    let result = &out.stdout_json()["payload"]["result"];
    assert_eq!(result["blocked"], json!([]), "{result}");
    assert_eq!(result["posted"].as_array().expect("posted").len(), 1);
    assert!(run_lock.exists(), "stable advisory run lock path missing");
    drop(
        plan_tooling::mutation_lock::OwnedFileLock::acquire(&run_lock)
            .expect("run lock released after checkpoint"),
    );
    assert!(
        exec_lock.exists(),
        "stable advisory execution-state lock path missing"
    );
    drop(
        plan_tooling::mutation_lock::OwnedFileLock::acquire(&exec_lock)
            .expect("execution-state lock released after checkpoint"),
    );
}

#[test]
fn tracking_checkpoint_offline_uses_persisted_authority_for_issue_reconciliation() {
    let tmp = TempDir::new().expect("tmp");
    let (run_state, _) = write_bound_execution_state(
        &tmp,
        "group/project",
        "gitlab",
        "gitlab.example.test",
        "## Execution State\n\n- Target scope: provider-bound target\n- Tracking issue: <https://github.com/group/project/issues/999>\n\n## Task Ledger\n\n| ID | Status | Task | Evidence |\n| --- | --- | --- | --- |\n| 1.2 | done | demo | test |\n",
    );

    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "checkpoint",
        "--run-state",
        run_state.to_str().expect("run state"),
        "--post",
        "state",
    ]);

    assert_eq!(
        out.code,
        0,
        "stdout={} stderr={}",
        out.stdout_text(),
        out.stderr_text()
    );
    let result = &out.stdout_json()["payload"]["result"];
    assert_eq!(result["execution_state_reconcile"]["status"], "mismatch");
    assert!(
        result["blocked"]
            .as_array()
            .expect("blocked")
            .iter()
            .any(|entry| entry["code"] == "execution-state-issue-mismatch"),
        "{result}"
    );
}

#[test]
fn tracking_checkpoint_fixture_self_heals_with_persisted_self_hosted_authority() {
    let fixture = pre_closeout_fixture();
    let tmp = TempDir::new().expect("tmp");
    let (run_state, execution_state) = write_bound_execution_state(
        &tmp,
        "group/project",
        "gitlab",
        "gitlab.example.test",
        "## Execution State\n\n- Target scope: provider-bound target\n\n## Task Ledger\n\n| ID | Status | Task | Evidence |\n| --- | --- | --- | --- |\n| 1.2 | done | demo | test |\n",
    );

    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "checkpoint",
        "--run-state",
        run_state.to_str().expect("run state"),
        "--post",
        "state",
        "--fixture",
        fixture.path().to_str().expect("fixture"),
        "--live",
    ]);

    assert_eq!(
        out.code,
        0,
        "stdout={} stderr={}",
        out.stdout_text(),
        out.stderr_text()
    );
    let result = &out.stdout_json()["payload"]["result"];
    assert_eq!(result["execution_state_reconcile"]["status"], "self-healed");
    assert_eq!(
        result["execution_state_reconcile"]["issue_url"],
        "https://gitlab.example.test/group/project/-/issues/999"
    );
    let written = fs::read_to_string(execution_state).expect("healed execution state");
    assert!(
        written
            .contains("- Tracking issue: <https://gitlab.example.test/group/project/-/issues/999>"),
        "{written}"
    );
}

#[test]
fn tracking_checkpoint_rejects_invalid_tracking_issue_without_reflecting_credentials() {
    let tmp = TempDir::new().expect("tmp");
    let (run_state, _) = write_bound_execution_state(
        &tmp,
        "group/project",
        "gitlab",
        "gitlab.example.test",
        "## Execution State\n\n- Target scope: provider-bound target\n- Tracking issue: <https://operator:secret@gitlab.example.test/group/project/pull/7>\n\n## Task Ledger\n\n| ID | Status | Task | Evidence |\n| --- | --- | --- | --- |\n| 1.2 | done | demo | test |\n",
    );

    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "checkpoint",
        "--run-state",
        run_state.to_str().expect("run state"),
        "--post",
        "state",
    ]);

    assert_eq!(
        out.code,
        0,
        "stdout={} stderr={}",
        out.stdout_text(),
        out.stderr_text()
    );
    let result = &out.stdout_json()["payload"]["result"];
    assert_eq!(result["execution_state_reconcile"]["status"], "invalid");
    assert!(
        result["blocked"]
            .as_array()
            .expect("blocked")
            .iter()
            .any(|entry| entry["code"] == "execution-state-issue-invalid"),
        "{result}"
    );
    assert!(!out.stdout_text().contains("operator"));
    assert!(!out.stdout_text().contains("secret"));
    assert!(!out.stderr_text().contains("operator"));
    assert!(!out.stderr_text().contains("secret"));
}

#[test]
fn tracking_checkpoint_surfaces_self_heal_failure_without_posting() {
    let fixture = pre_closeout_fixture();
    let (body_json, comments_json) = provider_evidence_env(&fixture);
    let tmp = TempDir::new().expect("tmp");
    let stub = StubBinDir::new();
    stub.write_exe("forge-cli", common::forge_cli_stub_script());
    let (run_state, execution_state) = write_bound_execution_state(
        &tmp,
        "group/project",
        "gitlab",
        "gitlab.example.test",
        "## Execution State\n\n- Target scope: provider-bound target\n\n## Task Ledger\n\n| ID | Status | Task | Evidence |\n| --- | --- | --- | --- |\n| 1.2 | done | demo | test |\n",
    );
    let replacement = "# Missing execution state section\n\n## Task Ledger\n\n| ID | Status | Task | Evidence |\n| --- | --- | --- | --- |\n| 1.2 | done | demo | test |\n";
    let execution_state_s = execution_state.to_string_lossy().to_string();
    let log_path = tmp.path().join("forge-cli.log");
    let log_s = log_path.to_string_lossy().to_string();

    let out = common::run_plan_issue_with_options(
        &[
            "--format",
            "json",
            "tracking",
            "checkpoint",
            "--run-state",
            run_state.to_str().expect("run state"),
            "--post",
            "state",
            "--provider-repo",
            "https://gitlab.example.test/group/project",
            "--live",
        ],
        provider_stub_options(
            stub.path(),
            &[
                ("FORGE_CLI_STUB_VIEW_BODY_JSON", &body_json),
                ("FORGE_CLI_STUB_VIEW_COMMENTS_JSON", &comments_json),
                ("FORGE_CLI_STUB_VIEW_REPLACE_PATH", &execution_state_s),
                ("FORGE_CLI_STUB_VIEW_REPLACE_CONTENTS", replacement),
                ("FORGE_CLI_STUB_LOG", &log_s),
            ],
        ),
    );

    assert_eq!(
        out.code,
        0,
        "stdout={} stderr={}",
        out.stdout_text(),
        out.stderr_text()
    );
    let result = &out.stdout_json()["payload"]["result"];
    assert_eq!(
        result["execution_state_reconcile"]["status"],
        "self-heal-failed"
    );
    assert!(
        result["blocked"]
            .as_array()
            .expect("blocked")
            .iter()
            .any(|entry| entry["code"] == "exec-state-section-missing"),
        "{result}"
    );
    assert_eq!(result["posted"], json!([]));
    let log = fs::read_to_string(log_path).expect("provider log");
    assert!(log.contains("issue view 999"), "{log}");
    assert!(!log.contains("issue comment 999"), "{log}");
}

#[test]
fn tracking_checkpoint_comment_failures_stop_posting_skip_repair_and_release_locks() {
    let fixture = pre_closeout_fixture();
    let (body_json, comments_json) = provider_evidence_env(&fixture);

    for (fail_on, expected_posted, expected_role) in [(1, 0, "state"), (2, 1, "review")] {
        let tmp = TempDir::new().expect("tmp");
        let stub = StubBinDir::new();
        stub.write_exe("forge-cli", common::forge_cli_stub_script());
        let (run_state, execution_state) = write_bound_execution_state(
            &tmp,
            "owner/repo",
            "github",
            "github.com",
            "## Execution State\n\n- Target scope: provider-bound target\n- Tracking issue: <https://github.com/owner/repo/issues/999>\n\n## Task Ledger\n\n| ID | Status | Task | Evidence |\n| --- | --- | --- | --- |\n| 1.2 | done | demo | test |\n",
        );
        let log_path = tmp.path().join("forge-cli.log");
        let count_path = tmp.path().join("comment-count");
        let log_s = log_path.to_string_lossy().to_string();
        let count_s = count_path.to_string_lossy().to_string();
        let fail_on_s = fail_on.to_string();
        let args = [
            "--format",
            "json",
            "tracking",
            "checkpoint",
            "--run-state",
            run_state.to_str().expect("run state"),
            "--post",
            "state,review",
            "--provider-repo",
            "https://github.com/owner/repo",
            "--live",
            "--repair-dashboard",
        ];

        let failed = common::run_plan_issue_with_options(
            &args,
            provider_stub_options(
                stub.path(),
                &[
                    ("FORGE_CLI_STUB_VIEW_BODY_JSON", &body_json),
                    ("FORGE_CLI_STUB_VIEW_COMMENTS_JSON", &comments_json),
                    ("FORGE_CLI_STUB_COMMENT_COUNT_FILE", &count_s),
                    ("FORGE_CLI_STUB_FAIL_COMMENT_ON_CALL", &fail_on_s),
                    ("FORGE_CLI_STUB_LOG", &log_s),
                ],
            ),
        );

        assert_eq!(
            failed.code,
            0,
            "fail_on={fail_on} stdout={} stderr={}",
            failed.stdout_text(),
            failed.stderr_text()
        );
        let result = &failed.stdout_json()["payload"]["result"];
        assert_eq!(
            result["posted"].as_array().expect("posted").len(),
            expected_posted,
            "fail_on={fail_on}: {result}"
        );
        assert!(
            result["blocked"]
                .as_array()
                .expect("blocked")
                .iter()
                .any(|entry| {
                    entry["code"] == "tracking-checkpoint-live-post-failed"
                        && entry["role"] == expected_role
                }),
            "fail_on={fail_on}: {result}"
        );
        assert!(result["repair_dashboard_result"].is_null(), "{result}");
        let failed_log = fs::read_to_string(&log_path).expect("provider log");
        assert!(!failed_log.contains("issue edit 999"), "{failed_log}");
        let run_lock = run_state.with_file_name(".run-state.json.update.lock");
        let exec_lock = execution_state.with_file_name("provider-execution-state.md.lock");
        assert!(run_lock.exists(), "stable advisory run lock path missing");
        drop(
            plan_tooling::mutation_lock::OwnedFileLock::acquire(&run_lock)
                .expect("run lock released after failed post"),
        );
        assert!(
            exec_lock.exists(),
            "stable advisory execution-state lock path missing"
        );
        drop(
            plan_tooling::mutation_lock::OwnedFileLock::acquire(&exec_lock)
                .expect("execution-state lock released after failed post"),
        );

        let retry = common::run_plan_issue_with_options(
            &args,
            provider_stub_options(
                stub.path(),
                &[
                    ("FORGE_CLI_STUB_VIEW_BODY_JSON", &body_json),
                    ("FORGE_CLI_STUB_VIEW_COMMENTS_JSON", &comments_json),
                    ("FORGE_CLI_STUB_COMMENT_COUNT_FILE", &count_s),
                    ("FORGE_CLI_STUB_LOG", &log_s),
                ],
            ),
        );
        assert_eq!(
            retry.code,
            0,
            "retry fail_on={fail_on} stdout={} stderr={}",
            retry.stdout_text(),
            retry.stderr_text()
        );
        let retry_result = &retry.stdout_json()["payload"]["result"];
        assert_eq!(retry_result["blocked"], json!([]), "{retry_result}");
        assert_eq!(
            retry_result["posted"]
                .as_array()
                .expect("retry posted")
                .len(),
            2,
            "{retry_result}"
        );
        assert!(
            retry_result["repair_dashboard_result"].is_object(),
            "{retry_result}"
        );
        drop(
            plan_tooling::mutation_lock::OwnedFileLock::acquire(&run_lock)
                .expect("run lock released after retry"),
        );
        drop(
            plan_tooling::mutation_lock::OwnedFileLock::acquire(&exec_lock)
                .expect("execution-state lock released after retry"),
        );
    }
}

#[test]
fn tracking_checkpoint_live_fixture_posts_state_and_review_in_declaration_order() {
    let fixture = pre_closeout_fixture();
    let tmp = TempDir::new().expect("tmp");
    let rs_path = tmp.path().join("run-state.json");
    write_run_state(&rs_path, "ready_for_close");

    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "checkpoint",
        "--run-state",
        rs_path.to_str().expect("rs"),
        "--post",
        "state,review",
        "--fixture",
        fixture.path().to_str().expect("fixture"),
        "--issue",
        "144",
        "--live",
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());

    let result = out.stdout_json()["payload"]["result"].clone();
    assert_eq!(result["mode"], "fixture");

    let posted = result["posted"].as_array().expect("posted array");
    assert_eq!(posted.len(), 2, "one comment per role: {posted:?}");
    let posted_roles: Vec<&str> = posted
        .iter()
        .map(|entry| entry["role"].as_str().unwrap())
        .collect();
    assert_eq!(posted_roles, vec!["state", "review"]);

    // The state body must reflect the run-state-driven `status=complete`
    // derivation (phase=ready_for_close maps to status=complete via
    // `synthesize_state_payload`).
    let rendered = result["rendered"].as_array().expect("rendered array");
    let state_entry = rendered
        .iter()
        .find(|entry| entry["role"] == "state")
        .expect("state rendered");
    let state_body = state_entry["body"].as_str().expect("state body");
    assert!(
        state_body.contains("Status: complete"),
        "state body should reflect status=complete; got: {state_body}"
    );
}

#[test]
fn tracking_checkpoint_live_fixture_repair_dashboard_returns_fixture_repair_result() {
    let fixture = pre_closeout_fixture();
    let tmp = TempDir::new().expect("tmp");
    let rs_path = tmp.path().join("run-state.json");
    write_run_state(&rs_path, "ready_for_close");

    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "checkpoint",
        "--run-state",
        rs_path.to_str().expect("rs"),
        "--post",
        "state",
        "--fixture",
        fixture.path().to_str().expect("fixture"),
        "--issue",
        "144",
        "--live",
        "--repair-dashboard",
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());

    let result = out.stdout_json()["payload"]["result"].clone();
    let repair = &result["repair_dashboard_result"];
    assert_eq!(repair["operation"], "record.repair-dashboard");
    assert_eq!(repair["mode"], "fixture");
    assert_eq!(repair["dry_run"], true);
}

#[test]
fn tracking_checkpoint_live_visible_completeness_failure_short_circuits_before_posting() {
    // Force a real visible-completeness failure from provider-bound source
    // evidence. A closed state with a status token as its authored target scope
    // is structurally renderable but forbidden by the visible lint, so the live
    // branch must skip posting.
    let fixture = write_fixture(&[
        (
            "source",
            json!({"path": "p", "commit": "c"}),
            "## Source Snapshot\n\n- Profile: tracking\n- Path: `p`",
            "2026-05-26T00:00:00Z",
        ),
        (
            "plan",
            json!({"path": "p", "commit": "c"}),
            "## Plan Snapshot\n\n- Profile: tracking\n- Path: `p`",
            "2026-05-26T00:00:01Z",
        ),
    ]);
    let tmp = TempDir::new().expect("tmp");
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(tmp.path())
            .status()
            .expect("run git")
    };
    assert!(git(&["init", "-q"]).success());
    assert!(
        git(&[
            "remote",
            "add",
            "origin",
            "https://github.com/owner/repo.git",
        ])
        .success()
    );
    let execution_state = tmp.path().join("execution-state.md");
    fs::write(
        &execution_state,
        "## Execution State\n\n- Target scope: done\n- Tracking issue: <https://github.com/owner/repo/issues/1>\n\n## Task Ledger\n\n| ID | Status | Task | Evidence |\n| --- | --- | --- | --- |\n| 1.1 | done | selected | test |\n",
    )
    .expect("execution state");
    let rs_path = tmp.path().join("run-state.json");
    let minimal = json!({
        "schema": "plan-issue.execution-run.v1",
        "run_id": "run-1",
        "repo": "owner/repo",
        "repo_provider": "github",
        "repo_host": "github.com",
        "issue": 1,
        "profile": "tracking",
        "phase": "closed",
        "execution_state_file": execution_state,
        "created_at": "2026-05-26T00:00:00Z",
        "updated_at": "2026-05-26T01:00:00Z"
    });
    fs::write(&rs_path, minimal.to_string()).expect("rs");

    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "checkpoint",
        "--run-state",
        rs_path.to_str().expect("rs"),
        "--post",
        "state",
        "--fixture",
        fixture.path().to_str().expect("fixture"),
        "--issue",
        "1",
        "--live",
    ]);
    assert_eq!(
        out.code,
        0,
        "stdout: {}\nstderr: {}",
        out.stdout_text(),
        out.stderr_text()
    );
    let result = out.stdout_json()["payload"]["result"].clone();
    // The lint blocker must remove the role from the posting plan and prevent
    // any provider mutation.
    let blocked_codes: Vec<&str> = result["blocked"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["code"].as_str().unwrap())
        .collect();
    assert!(
        blocked_codes.contains(&"visible-completeness-failed"),
        "expected a real visible-completeness blocker: {result}"
    );
    assert_eq!(result["posted"], json!([]));
    assert_eq!(result["roles_planned"], json!([]));
}

/// Regression for finding #44: the documented dispatch entrypoint passes only
/// `--run-state` (no `--provider-repo`/`--issue`). The run-state already
/// carries the issue (written by `tracking run init --issue`), so the live
/// checkpoint must inherit it and post — not silently no-op with status=ok.
#[test]
fn tracking_checkpoint_live_inherits_issue_from_run_state() {
    let fixture = pre_closeout_fixture();
    let tmp = TempDir::new().expect("tmp");
    let rs_path = tmp.path().join("run-state.json");
    // `write_run_state` persists `issue: 999`.
    write_run_state(&rs_path, "ready_for_close");

    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "checkpoint",
        "--run-state",
        rs_path.to_str().expect("rs"),
        "--post",
        "state",
        "--fixture",
        fixture.path().to_str().expect("fixture"),
        "--live", // no --issue: must be inherited from run-state
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let result = out.stdout_json()["payload"]["result"].clone();
    let blocked_codes: Vec<&str> = result["blocked"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["code"].as_str().unwrap())
        .collect();
    assert!(
        !blocked_codes.contains(&"tracking-checkpoint-live-missing-issue"),
        "run-state issue must be inherited, not blocked: {blocked_codes:?}"
    );
    let posted = result["posted"].as_array().expect("posted array");
    assert_eq!(posted.len(), 1, "state role must post: {posted:?}");
    assert_eq!(posted[0]["role"], "state");
    assert_eq!(
        posted[0]["comment_url"].as_str().expect("url"),
        "fixture://issue/999/state",
        "posted URL must reflect the run-state issue (999)"
    );
}

/// The loud `tracking-checkpoint-live-missing-issue` blocker still fires when
/// neither `--issue` nor a run-state issue can be resolved (`issue: 0` is the
/// never-written sentinel). The fix in #44 only adds the run-state fallback; it
/// does not remove the guard against a genuinely unresolvable target.
#[test]
fn tracking_checkpoint_live_missing_issue_blocks_when_run_state_has_none() {
    let fixture = pre_closeout_fixture();
    let tmp = TempDir::new().expect("tmp");
    let rs_path = tmp.path().join("run-state.json");
    // Mirror `write_run_state` but with the `issue: 0` sentinel so there is
    // nothing to inherit.
    let body = json!({
        "schema": "plan-issue.execution-run.v1",
        "run_id": "run-1",
        "repo": "owner/repo",
        "issue": 0,
        "profile": "tracking",
        "phase": "ready_for_close",
        "created_at": "2026-05-26T00:00:00Z",
        "updated_at": "2026-05-26T01:00:00Z",
        "selected_scope": {"task": "1.2", "title": "demo"},
        "branch": "feat/x",
        "review": {"decision": "approve", "evidence": null}
    });
    fs::write(&rs_path, body.to_string()).expect("run-state");

    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "checkpoint",
        "--run-state",
        rs_path.to_str().expect("rs"),
        "--post",
        "state",
        "--fixture",
        fixture.path().to_str().expect("fixture"),
        "--live", // no --issue and run-state issue is the 0 sentinel
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let result = out.stdout_json()["payload"]["result"].clone();
    let blocked_codes: Vec<&str> = result["blocked"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["code"].as_str().unwrap())
        .collect();
    assert!(
        blocked_codes.contains(&"tracking-checkpoint-live-missing-issue"),
        "expected tracking-checkpoint-live-missing-issue, got {blocked_codes:?}"
    );
    let posted = result["posted"].as_array().expect("posted array");
    assert!(
        posted.is_empty(),
        "no posting may occur with no resolvable issue: {posted:?}"
    );
}

#[test]
fn tracking_checkpoint_live_uses_persisted_gitlab_target_for_every_provider_hop() {
    let fixture = pre_closeout_fixture();
    let (body_json, comments_json) = provider_evidence_env(&fixture);
    let tmp = TempDir::new().expect("tmp");
    let stub = StubBinDir::new();
    stub.write_exe("forge-cli", common::forge_cli_stub_script());
    let rs_path = tmp.path().join("run-state.json");
    let log_path = tmp.path().join("forge-cli.log");
    let checkout = tmp.path().join("checkout");
    fs::create_dir(&checkout).expect("checkout");
    assert!(
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&checkout)
            .status()
            .expect("git init")
            .success()
    );
    assert!(
        std::process::Command::new("git")
            .args([
                "remote",
                "add",
                "origin",
                "https://gitlab.com/group/project.git",
            ])
            .current_dir(&checkout)
            .status()
            .expect("git remote add")
            .success()
    );
    let execution_state = checkout.join("provider-execution-state.md");
    fs::write(
        &execution_state,
        "## Execution State\n\n- Target scope: provider-bound target\n\n## Task Ledger\n\n| ID | Status | Task | Evidence |\n| --- | --- | --- | --- |\n| 1.2 | done | demo | test |\n",
    )
    .expect("execution state");
    write_provider_bound_run_state(&rs_path, "group/project", "gitlab", "gitlab.com", 999);
    let mut run: Value =
        serde_json::from_str(&fs::read_to_string(&rs_path).expect("run state")).expect("run json");
    run["execution_state_file"] = json!(execution_state);
    fs::write(&rs_path, run.to_string()).expect("bound run state");

    let log_s = log_path.to_string_lossy().to_string();
    let out = common::run_plan_issue_with_options(
        &[
            "--format",
            "json",
            "tracking",
            "checkpoint",
            "--run-state",
            rs_path.to_str().expect("rs"),
            "--post",
            "state",
            "--live",
            "--repair-dashboard",
        ],
        provider_stub_options(
            stub.path(),
            &[
                ("FORGE_CLI_STUB_VIEW_BODY_JSON", &body_json),
                ("FORGE_CLI_STUB_VIEW_COMMENTS_JSON", &comments_json),
                ("FORGE_CLI_STUB_LOG", &log_s),
            ],
        ),
    );
    assert_eq!(
        out.code,
        0,
        "stdout={} stderr={}",
        out.stdout_text(),
        out.stderr_text()
    );

    let result = &out.stdout_json()["payload"]["result"];
    assert_eq!(result["blocked"], json!([]), "{result}");
    assert_eq!(result["posted"].as_array().expect("posted").len(), 1);
    assert_eq!(
        result["repair_dashboard_result"]["issue"]["url"],
        "https://gitlab.com/group/project/-/issues/999"
    );
    assert_eq!(result["execution_state_reconcile"]["status"], "self-healed");
    assert_eq!(
        result["execution_state_reconcile"]["issue_url"],
        "https://gitlab.com/group/project/-/issues/999"
    );
    assert!(
        fs::read_to_string(&execution_state)
            .expect("healed execution state")
            .contains("- Tracking issue: <https://gitlab.com/group/project/-/issues/999>")
    );
    let log = fs::read_to_string(log_path).expect("provider log");
    assert!(
        log.contains("--repo group/project issue view 999 --with-comments"),
        "initial evidence must use the persisted target:\n{log}"
    );
    assert!(
        log.contains("--repo group/project issue comment 999"),
        "posting must use the persisted target:\n{log}"
    );
    assert!(
        log.matches("--repo group/project issue view 999 --with-comments")
            .count()
            >= 2,
        "dashboard repair must refetch the same target:\n{log}"
    );
    assert!(
        log.contains("--repo group/project issue edit 999"),
        "dashboard repair must edit the same target:\n{log}"
    );
}

#[test]
fn tracking_checkpoint_live_rejects_explicit_target_mismatches_before_provider_access() {
    struct Case<'a> {
        name: &'a str,
        persisted_repo: &'a str,
        persisted_provider: &'a str,
        persisted_host: &'a str,
        persisted_issue: u64,
        explicit_repo: &'a str,
        explicit_issue: u64,
    }

    let cases = [
        Case {
            name: "provider",
            persisted_repo: "group/project",
            persisted_provider: "gitlab",
            persisted_host: "gitlab.com",
            persisted_issue: 999,
            explicit_repo: "https://github.com/group/project",
            explicit_issue: 999,
        },
        Case {
            name: "host",
            persisted_repo: "group/project",
            persisted_provider: "gitlab",
            persisted_host: "gitlab.example.com",
            persisted_issue: 999,
            explicit_repo: "https://gitlab.com/group/project",
            explicit_issue: 999,
        },
        Case {
            name: "slug",
            persisted_repo: "group/project",
            persisted_provider: "gitlab",
            persisted_host: "gitlab.com",
            persisted_issue: 999,
            explicit_repo: "https://gitlab.com/other/project",
            explicit_issue: 999,
        },
        Case {
            name: "issue",
            persisted_repo: "group/project",
            persisted_provider: "gitlab",
            persisted_host: "gitlab.com",
            persisted_issue: 999,
            explicit_repo: "https://gitlab.com/group/project",
            explicit_issue: 1000,
        },
    ];

    for case in cases {
        let tmp = TempDir::new().expect("tmp");
        let stub = StubBinDir::new();
        stub.write_exe("forge-cli", common::forge_cli_stub_script());
        let rs_path = tmp.path().join("run-state.json");
        let log_path = tmp.path().join("forge-cli.log");
        write_provider_bound_run_state(
            &rs_path,
            case.persisted_repo,
            case.persisted_provider,
            case.persisted_host,
            case.persisted_issue,
        );
        let explicit_issue = case.explicit_issue.to_string();
        let log_s = log_path.to_string_lossy().to_string();

        let out = common::run_plan_issue_with_options(
            &[
                "--format",
                "json",
                "tracking",
                "checkpoint",
                "--run-state",
                rs_path.to_str().expect("rs"),
                "--post",
                "state",
                "--provider-repo",
                case.explicit_repo,
                "--issue",
                &explicit_issue,
                "--live",
            ],
            provider_stub_options(stub.path(), &[("FORGE_CLI_STUB_LOG", &log_s)]),
        );
        assert_ne!(out.code, 0, "{} mismatch must fail", case.name);
        assert_eq!(
            out.stdout_json()["error"]["code"],
            "tracking-checkpoint-live-target-mismatch",
            "{} mismatch: stdout={} stderr={}",
            case.name,
            out.stdout_text(),
            out.stderr_text()
        );
        assert_eq!(
            fs::read_to_string(&log_path).unwrap_or_default(),
            "",
            "{} mismatch must fail before provider access",
            case.name
        );
    }
}

#[test]
fn tracking_checkpoint_live_matches_github_slug_case_insensitively() {
    let fixture = pre_closeout_fixture();
    let (body_json, comments_json) = provider_evidence_env(&fixture);
    let tmp = TempDir::new().expect("tmp");
    let stub = StubBinDir::new();
    stub.write_exe("forge-cli", common::forge_cli_stub_script());
    let rs_path = tmp.path().join("run-state.json");
    let log_path = tmp.path().join("forge-cli.log");
    write_provider_bound_run_state(&rs_path, "Owner/Repo", "github", "github.com", 999);
    let log_s = log_path.to_string_lossy().to_string();

    let out = common::run_plan_issue_with_options(
        &[
            "--format",
            "json",
            "tracking",
            "checkpoint",
            "--run-state",
            rs_path.to_str().expect("run state"),
            "--post",
            "state",
            "--provider-repo",
            "https://github.com/owner/repo",
            "--live",
        ],
        provider_stub_options(
            stub.path(),
            &[
                ("FORGE_CLI_STUB_VIEW_BODY_JSON", &body_json),
                ("FORGE_CLI_STUB_VIEW_COMMENTS_JSON", &comments_json),
                ("FORGE_CLI_STUB_LOG", &log_s),
            ],
        ),
    );

    assert_eq!(
        out.code,
        0,
        "stdout={} stderr={}",
        out.stdout_text(),
        out.stderr_text()
    );
    assert_eq!(out.stdout_json()["payload"]["result"]["blocked"], json!([]));
}

#[test]
fn tracking_checkpoint_live_rejects_qualified_global_override_masked_by_bare_checkpoint() {
    let tmp = TempDir::new().expect("tmp");
    let stub = StubBinDir::new();
    stub.write_exe("forge-cli", common::forge_cli_stub_script());
    let rs_path = tmp.path().join("run-state.json");
    let log_path = tmp.path().join("forge-cli.log");
    write_provider_bound_run_state(&rs_path, "group/project", "github", "github.com", 999);
    let log_s = log_path.to_string_lossy().to_string();

    let out = common::run_plan_issue_with_options(
        &[
            "--format",
            "json",
            "--repo",
            "https://gitlab.com/group/project",
            "tracking",
            "checkpoint",
            "--run-state",
            rs_path.to_str().expect("run state"),
            "--post",
            "state",
            "--provider-repo",
            "group/project",
            "--live",
        ],
        provider_stub_options(stub.path(), &[("FORGE_CLI_STUB_LOG", &log_s)]),
    );

    assert_ne!(out.code, 0);
    assert_eq!(
        out.stdout_json()["error"]["code"],
        "tracking-checkpoint-live-target-mismatch"
    );
    assert_eq!(fs::read_to_string(&log_path).unwrap_or_default(), "");
}

#[test]
fn tracking_checkpoint_live_historical_bare_repo_does_not_inherit_matching_checkout_identity() {
    let fixture = pre_closeout_fixture();
    let (body_json, comments_json) = provider_evidence_env(&fixture);
    let tmp = TempDir::new().expect("tmp");
    let stub = StubBinDir::new();
    stub.write_exe("forge-cli", common::forge_cli_stub_script());
    let checkout = tmp.path().join("historical-checkout");
    fs::create_dir(&checkout).expect("checkout");
    assert!(
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&checkout)
            .status()
            .expect("git init")
            .success()
    );
    assert!(
        std::process::Command::new("git")
            .args([
                "remote",
                "add",
                "origin",
                "https://github.com/owner/repo.git",
            ])
            .current_dir(&checkout)
            .status()
            .expect("git remote add")
            .success()
    );
    let execution_state = checkout.join("historical-execution-state.md");
    fs::write(
        &execution_state,
        "## Execution State\n\n- Target scope: historical target\n\n## Task Ledger\n\n| ID | Status | Task | Evidence |\n| --- | --- | --- | --- |\n| 1.2 | done | demo | test |\n",
    )
    .expect("execution state");
    let rs_path = tmp.path().join("run-state.json");
    let log_path = tmp.path().join("forge-cli.log");
    write_run_state(&rs_path, "ready_for_close");
    let mut run: Value =
        serde_json::from_str(&fs::read_to_string(&rs_path).expect("run state")).expect("run json");
    run["repo"] = json!("owner/repo");
    run["execution_state_file"] = json!(execution_state);
    fs::write(&rs_path, run.to_string()).expect("historical run state");
    let log_s = log_path.to_string_lossy().to_string();

    let out = common::run_plan_issue_with_options(
        &[
            "--format",
            "json",
            "tracking",
            "checkpoint",
            "--run-state",
            rs_path.to_str().expect("run state"),
            "--post",
            "state",
            "--live",
        ],
        provider_stub_options(
            stub.path(),
            &[
                ("FORGE_CLI_STUB_VIEW_BODY_JSON", &body_json),
                ("FORGE_CLI_STUB_VIEW_COMMENTS_JSON", &comments_json),
                ("FORGE_CLI_STUB_LOG", &log_s),
            ],
        )
        .with_cwd(&checkout),
    );

    assert_ne!(
        out.code,
        0,
        "stdout={} stderr={}",
        out.stdout_text(),
        out.stderr_text()
    );
    assert_eq!(
        out.stdout_json()["error"]["code"],
        "tracking-checkpoint-live-repo-identity-required"
    );
    assert_eq!(
        fs::read_to_string(log_path).unwrap_or_default(),
        "",
        "historical bare identities must fail before provider access"
    );
}

#[test]
fn tracking_checkpoint_live_historical_bare_repo_requires_qualified_identity() {
    let fixture = pre_closeout_fixture();
    let (body_json, comments_json) = provider_evidence_env(&fixture);
    let tmp = TempDir::new().expect("tmp");
    let stub = StubBinDir::new();
    stub.write_exe("forge-cli", common::forge_cli_stub_script());
    let rs_path = tmp.path().join("run-state.json");
    let log_path = tmp.path().join("forge-cli.log");
    write_run_state(&rs_path, "ready_for_close");
    let checkout = tmp.path().join("pre-binding-checkout");
    fs::create_dir(&checkout).expect("pre-binding checkout");
    assert!(
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&checkout)
            .status()
            .expect("git init")
            .success()
    );
    assert!(
        std::process::Command::new("git")
            .args([
                "remote",
                "add",
                "origin",
                "https://gitlab.com/owner/repo.git",
            ])
            .current_dir(&checkout)
            .status()
            .expect("git remote add")
            .success()
    );
    let execution_state = checkout.join("pre-binding-execution-state.md");
    fs::write(
        &execution_state,
        "## Execution State\n\n- Target scope: pre-binding target\n\n## Task Ledger\n\n| ID | Status | Task | Evidence |\n| --- | --- | --- | --- |\n| 1.2 | done | demo | test |\n",
    )
    .expect("execution state");
    let mut run: Value =
        serde_json::from_str(&fs::read_to_string(&rs_path).expect("run state")).expect("run json");
    run["execution_state_file"] = json!(execution_state);
    fs::write(&rs_path, run.to_string()).expect("pre-binding run state");
    let log_s = log_path.to_string_lossy().to_string();

    let missing_identity = common::run_plan_issue_with_options(
        &[
            "--format",
            "json",
            "tracking",
            "checkpoint",
            "--run-state",
            rs_path.to_str().expect("rs"),
            "--post",
            "state",
            "--live",
        ],
        provider_stub_options(stub.path(), &[("FORGE_CLI_STUB_LOG", &log_s)]),
    );
    assert_ne!(missing_identity.code, 0);
    assert_eq!(
        missing_identity.stdout_json()["error"]["code"],
        "tracking-checkpoint-live-repo-identity-required"
    );
    assert_eq!(fs::read_to_string(&log_path).unwrap_or_default(), "");

    let qualified = common::run_plan_issue_with_options(
        &[
            "--format",
            "json",
            "tracking",
            "checkpoint",
            "--run-state",
            rs_path.to_str().expect("rs"),
            "--post",
            "state",
            "--provider-repo",
            "https://gitlab.com/owner/repo",
            "--live",
        ],
        provider_stub_options(
            stub.path(),
            &[
                ("FORGE_CLI_STUB_VIEW_BODY_JSON", &body_json),
                ("FORGE_CLI_STUB_VIEW_COMMENTS_JSON", &comments_json),
                ("FORGE_CLI_STUB_LOG", &log_s),
            ],
        ),
    );
    assert_eq!(
        qualified.code,
        0,
        "stdout={} stderr={}",
        qualified.stdout_text(),
        qualified.stderr_text()
    );
    let result = &qualified.stdout_json()["payload"]["result"];
    assert_eq!(result["blocked"], json!([]), "{result}");
    assert_eq!(result["posted"].as_array().expect("posted").len(), 1);
    let log = fs::read_to_string(log_path).expect("provider log");
    assert!(log.contains("--repo owner/repo issue view 999 --with-comments"));
    assert!(log.contains("--repo owner/repo issue comment 999"));
}

#[test]
fn tracking_checkpoint_live_rejects_malformed_persisted_identity_before_provider_access() {
    struct Case<'a> {
        name: &'a str,
        repo: &'a str,
        provider: Option<&'a str>,
        host: Option<&'a str>,
    }

    let cases = [
        Case {
            name: "unknown-provider",
            repo: "group/project",
            provider: Some("bogus"),
            host: Some("gitlab.com"),
        },
        Case {
            name: "unknown-host",
            repo: "group/project",
            provider: Some("gitlab"),
            host: Some("unknown.example"),
        },
        Case {
            name: "partial-binding",
            repo: "group/project",
            provider: Some("gitlab"),
            host: None,
        },
        Case {
            name: "contradictory-qualified-fields",
            repo: "https://github.com/group/project",
            provider: Some("gitlab"),
            host: Some("gitlab.com"),
        },
    ];

    for case in cases {
        let tmp = TempDir::new().expect("tmp");
        let stub = StubBinDir::new();
        stub.write_exe("forge-cli", common::forge_cli_stub_script());
        let rs_path = tmp.path().join("run-state.json");
        let log_path = tmp.path().join("forge-cli.log");
        let mut run = json!({
            "schema": "plan-issue.execution-run.v1",
            "run_id": case.name,
            "repo": case.repo,
            "issue": 999,
            "profile": "tracking",
            "phase": "ready_for_close",
            "created_at": "2026-05-26T00:00:00Z",
            "updated_at": "2026-05-26T01:00:00Z",
            "selected_scope": {"task": "1.2", "title": "demo"}
        });
        if let Some(provider) = case.provider {
            run["repo_provider"] = json!(provider);
        }
        if let Some(host) = case.host {
            run["repo_host"] = json!(host);
        }
        fs::write(&rs_path, run.to_string()).expect("run state");
        let log_s = log_path.to_string_lossy().to_string();

        let out = common::run_plan_issue_with_options(
            &[
                "--format",
                "json",
                "tracking",
                "checkpoint",
                "--run-state",
                rs_path.to_str().expect("run state"),
                "--post",
                "state",
                "--provider-repo",
                "https://gitlab.com/group/project",
                "--live",
            ],
            provider_stub_options(stub.path(), &[("FORGE_CLI_STUB_LOG", &log_s)]),
        );
        assert_ne!(out.code, 0, "{} must fail", case.name);
        assert_eq!(
            out.stdout_json()["error"]["code"],
            "tracking-checkpoint-live-repo-identity-invalid",
            "{}: stdout={} stderr={}",
            case.name,
            out.stdout_text(),
            out.stderr_text()
        );
        assert_eq!(
            fs::read_to_string(&log_path).unwrap_or_default(),
            "",
            "{} must fail before provider access",
            case.name
        );
    }
}

#[test]
fn tracking_checkpoint_live_rejects_unconfirmed_self_hosted_persisted_identity() {
    let tmp = TempDir::new().expect("tmp");
    let stub = StubBinDir::new();
    stub.write_exe("forge-cli", common::forge_cli_stub_script());
    let rs_path = tmp.path().join("run-state.json");
    let log_path = tmp.path().join("forge-cli.log");
    write_provider_bound_run_state(
        &rs_path,
        "group/project",
        "gitlab",
        "gitlab.example.com",
        999,
    );
    let log_s = log_path.to_string_lossy().to_string();

    let out = common::run_plan_issue_with_options(
        &[
            "--format",
            "json",
            "tracking",
            "checkpoint",
            "--run-state",
            rs_path.to_str().expect("run state"),
            "--post",
            "state",
            "--live",
        ],
        provider_stub_options(stub.path(), &[("FORGE_CLI_STUB_LOG", &log_s)]).with_cwd(tmp.path()),
    );
    assert_eq!(out.code, 64, "stderr={}", out.stderr_text());
    assert_eq!(
        out.stdout_json()["error"]["code"],
        "tracking-checkpoint-live-repo-identity-required"
    );
    assert_eq!(fs::read_to_string(log_path).unwrap_or_default(), "");
}

#[test]
fn tracking_checkpoint_live_binds_qualified_self_hosted_provider_transport() {
    let fixture = pre_closeout_fixture();
    let (body_json, comments_json) = provider_evidence_env(&fixture);
    let tmp = TempDir::new().expect("tmp");
    let stub = StubBinDir::new();
    stub.write_exe("forge-cli", common::forge_cli_stub_script());
    let rs_path = tmp.path().join("run-state.json");
    let log_path = tmp.path().join("forge-cli.log");
    write_provider_bound_run_state(
        &rs_path,
        "group/project",
        "gitlab",
        "gitlab.example.com",
        999,
    );
    let log_s = log_path.to_string_lossy().to_string();

    let out = common::run_plan_issue_with_options(
        &[
            "--format",
            "json",
            "tracking",
            "checkpoint",
            "--run-state",
            rs_path.to_str().expect("run state"),
            "--post",
            "state",
            "--provider-repo",
            "https://gitlab.example.com/group/project",
            "--live",
        ],
        provider_stub_options(
            stub.path(),
            &[
                ("FORGE_CLI_STUB_VIEW_BODY_JSON", &body_json),
                ("FORGE_CLI_STUB_VIEW_COMMENTS_JSON", &comments_json),
                ("FORGE_CLI_STUB_LOG", &log_s),
            ],
        )
        .with_cwd(tmp.path()),
    );
    assert_eq!(
        out.code,
        0,
        "stdout={} stderr={}",
        out.stdout_text(),
        out.stderr_text()
    );
    assert_eq!(out.stdout_json()["payload"]["result"]["blocked"], json!([]));
    let log = fs::read_to_string(log_path).expect("provider log");
    assert!(
        log.contains(
            "--host gitlab.example.com --repo group/project issue view 999 --with-comments"
        ),
        "bound host must reach every forge-cli call:\n{log}"
    );
    assert!(
        log.contains("--host gitlab.example.com --repo group/project issue comment 999"),
        "bound host must reach mutation calls:\n{log}"
    );
}

#[test]
fn tracking_checkpoint_live_preserves_non_default_issue_url_authority_port() {
    let fixture = pre_closeout_fixture();
    let (body_json, comments_json) = provider_evidence_env(&fixture);
    let tmp = TempDir::new().expect("tmp");
    let stub = StubBinDir::new();
    stub.write_exe("forge-cli", common::forge_cli_stub_script());
    let checkout = tmp.path().join("checkout");
    fs::create_dir(&checkout).expect("checkout");
    assert!(
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&checkout)
            .status()
            .expect("git init")
            .success()
    );
    assert!(
        std::process::Command::new("git")
            .args([
                "remote",
                "add",
                "origin",
                "https://internal.ghe.com:8443/acme/widgets.git",
            ])
            .current_dir(&checkout)
            .status()
            .expect("git remote add")
            .success()
    );
    let execution_state = checkout.join("port-execution-state.md");
    fs::write(
        &execution_state,
        "## Execution State\n\n- Target scope: port-preserving checkpoint\n- Tracking issue: <https://internal.ghe.com:8443/acme/widgets/issues/999>\n\n## Task Ledger\n\n| ID | Status | Task | Evidence |\n| --- | --- | --- | --- |\n| 1.2 | done | demo | test |\n",
    )
    .expect("execution state");
    let rs_path = checkout.join("run-state.json");
    write_provider_bound_run_state(
        &rs_path,
        "acme/widgets",
        "github",
        "internal.ghe.com:8443",
        999,
    );
    let mut run: Value =
        serde_json::from_str(&fs::read_to_string(&rs_path).expect("run state")).expect("run json");
    run["execution_state_file"] = json!(execution_state);
    fs::write(&rs_path, run.to_string()).expect("bound run state");
    let log_path = tmp.path().join("forge-cli.log");
    let log_s = log_path.to_string_lossy().to_string();

    let out = common::run_plan_issue_with_options(
        &[
            "--format",
            "json",
            "tracking",
            "checkpoint",
            "--run-state",
            rs_path.to_str().expect("run state"),
            "--post",
            "state",
            "--provider-repo",
            "https://internal.ghe.com:8443/acme/widgets",
            "--live",
        ],
        provider_stub_options(
            stub.path(),
            &[
                ("FORGE_CLI_STUB_VIEW_BODY_JSON", &body_json),
                ("FORGE_CLI_STUB_VIEW_COMMENTS_JSON", &comments_json),
                ("FORGE_CLI_STUB_LOG", &log_s),
            ],
        )
        .with_cwd(&checkout),
    );

    assert_eq!(
        out.code,
        0,
        "stdout={} stderr={}",
        out.stdout_text(),
        out.stderr_text()
    );
    let result = &out.stdout_json()["payload"]["result"];
    assert_eq!(
        result["execution_state_reconcile"]["status"], "consistent",
        "matching non-default authority port must not mismatch: {result}"
    );
    assert_eq!(result["blocked"], json!([]), "{result}");
    let log = fs::read_to_string(log_path).expect("provider log");
    assert!(
        log.contains("--host internal.ghe.com:8443 --repo acme/widgets issue view 999"),
        "provider transport must retain the authority port:\n{log}"
    );
}

#[test]
fn tracking_checkpoint_live_accepts_self_hosted_identity_from_matching_checkout() {
    let fixture = pre_closeout_fixture();
    let (body_json, comments_json) = provider_evidence_env(&fixture);
    let tmp = TempDir::new().expect("tmp");
    let stub = StubBinDir::new();
    stub.write_exe("forge-cli", common::forge_cli_stub_script());
    let rs_path = tmp.path().join("run-state.json");
    let log_path = tmp.path().join("forge-cli.log");
    let checkout = tmp.path().join("checkout");
    fs::create_dir(&checkout).expect("checkout");
    assert!(
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&checkout)
            .status()
            .expect("git init")
            .success()
    );
    assert!(
        std::process::Command::new("git")
            .args([
                "remote",
                "add",
                "origin",
                "https://gitlab.example.com/group/project.git",
            ])
            .current_dir(&checkout)
            .status()
            .expect("git remote add")
            .success()
    );
    write_provider_bound_run_state(
        &rs_path,
        "group/project",
        "gitlab",
        "gitlab.example.com",
        1000,
    );
    let log_s = log_path.to_string_lossy().to_string();

    let out = common::run_plan_issue_with_options(
        &[
            "--format",
            "json",
            "tracking",
            "checkpoint",
            "--run-state",
            rs_path.to_str().expect("run state"),
            "--post",
            "state",
            "--live",
        ],
        provider_stub_options(
            stub.path(),
            &[
                ("FORGE_CLI_STUB_VIEW_BODY_JSON", &body_json),
                ("FORGE_CLI_STUB_VIEW_COMMENTS_JSON", &comments_json),
                ("FORGE_CLI_STUB_LOG", &log_s),
            ],
        )
        .with_cwd(&checkout),
    );
    assert_eq!(
        out.code,
        0,
        "stdout={} stderr={}",
        out.stdout_text(),
        out.stderr_text()
    );
    assert_eq!(out.stdout_json()["payload"]["result"]["blocked"], json!([]));
    let log = fs::read_to_string(log_path).expect("provider log");
    assert!(
        log.contains(
            "--host gitlab.example.com --repo group/project issue view 1000 --with-comments"
        ),
        "bound host must reach every forge-cli call:\n{log}"
    );
    assert!(
        log.contains("--host gitlab.example.com --repo group/project issue comment 1000"),
        "bound host must reach mutation calls:\n{log}"
    );
}

#[test]
fn tracking_checkpoint_live_accepts_historical_qualified_run_repository() {
    let fixture = pre_closeout_fixture();
    let (body_json, comments_json) = provider_evidence_env(&fixture);
    let tmp = TempDir::new().expect("tmp");
    let stub = StubBinDir::new();
    stub.write_exe("forge-cli", common::forge_cli_stub_script());
    let rs_path = tmp.path().join("run-state.json");
    let log_path = tmp.path().join("forge-cli.log");
    write_run_state(&rs_path, "ready_for_close");
    let mut run: Value =
        serde_json::from_str(&fs::read_to_string(&rs_path).expect("run state")).expect("run json");
    run["repo"] = json!("https://gitlab.com/owner/repo.git");
    fs::write(&rs_path, run.to_string()).expect("qualified pre-binding run");
    let log_s = log_path.to_string_lossy().to_string();

    let out = common::run_plan_issue_with_options(
        &[
            "--format",
            "json",
            "tracking",
            "checkpoint",
            "--run-state",
            rs_path.to_str().expect("run state"),
            "--post",
            "state",
            "--live",
        ],
        provider_stub_options(
            stub.path(),
            &[
                ("FORGE_CLI_STUB_VIEW_BODY_JSON", &body_json),
                ("FORGE_CLI_STUB_VIEW_COMMENTS_JSON", &comments_json),
                ("FORGE_CLI_STUB_LOG", &log_s),
            ],
        ),
    );
    assert_eq!(
        out.code,
        0,
        "stdout={} stderr={}",
        out.stdout_text(),
        out.stderr_text()
    );
    assert_eq!(out.stdout_json()["payload"]["result"]["blocked"], json!([]));
    let log = fs::read_to_string(log_path).expect("provider log");
    assert!(log.contains("--host gitlab.com --repo owner/repo issue view 999 --with-comments"));
    assert!(log.contains("--host gitlab.com --repo owner/repo issue comment 999"));
}

#[test]
fn tracking_checkpoint_live_rejects_dry_run_and_offline_evidence_before_provider_access() {
    let tmp = TempDir::new().expect("tmp");
    let stub = StubBinDir::new();
    stub.write_exe("forge-cli", common::forge_cli_stub_script());
    let rs_path = tmp.path().join("run-state.json");
    let log_path = tmp.path().join("forge-cli.log");
    let body_path = tmp.path().join("body.md");
    let comments_path = tmp.path().join("comments.json");
    write_provider_bound_run_state(&rs_path, "group/project", "gitlab", "gitlab.com", 999);
    fs::write(&body_path, "## Current Dashboard\n").expect("body");
    fs::write(&comments_path, "{\"comments\":[]}").expect("comments");
    let log_s = log_path.to_string_lossy().to_string();

    let cases = [
        (
            "dry-run",
            vec![
                "--format",
                "json",
                "--dry-run",
                "tracking",
                "checkpoint",
                "--run-state",
                rs_path.to_str().expect("run state"),
                "--post",
                "state",
                "--live",
            ],
            "tracking-checkpoint-dry-run-live-conflict",
        ),
        (
            "offline-evidence",
            vec![
                "--format",
                "json",
                "tracking",
                "checkpoint",
                "--run-state",
                rs_path.to_str().expect("run state"),
                "--post",
                "state",
                "--body-file",
                body_path.to_str().expect("body"),
                "--comments-json",
                comments_path.to_str().expect("comments"),
                "--live",
            ],
            "tracking-checkpoint-live-offline-evidence-conflict",
        ),
    ];

    for (name, args, expected_code) in cases {
        fs::write(&log_path, "").expect("reset log");
        let out = common::run_plan_issue_with_options(
            &args,
            provider_stub_options(stub.path(), &[("FORGE_CLI_STUB_LOG", &log_s)]),
        );
        assert_ne!(out.code, 0, "{name} must fail");
        assert_eq!(
            out.stdout_json()["error"]["code"],
            expected_code,
            "{name}: stdout={} stderr={}",
            out.stdout_text(),
            out.stderr_text()
        );
        assert_eq!(fs::read_to_string(&log_path).expect("provider log"), "");
    }
}

#[test]
fn tracking_checkpoint_live_rejects_zero_issue_and_conflicting_overrides() {
    let tmp = TempDir::new().expect("tmp");
    let stub = StubBinDir::new();
    stub.write_exe("forge-cli", common::forge_cli_stub_script());
    let rs_path = tmp.path().join("run-state.json");
    let log_path = tmp.path().join("forge-cli.log");
    write_provider_bound_run_state(&rs_path, "group/project", "gitlab", "gitlab.com", 0);
    let log_s = log_path.to_string_lossy().to_string();

    let zero = common::run_plan_issue_with_options(
        &[
            "--format",
            "json",
            "tracking",
            "checkpoint",
            "--run-state",
            rs_path.to_str().expect("run state"),
            "--issue",
            "0",
            "--post",
            "state",
            "--live",
        ],
        provider_stub_options(stub.path(), &[("FORGE_CLI_STUB_LOG", &log_s)]),
    );
    assert_ne!(zero.code, 0);
    assert_eq!(
        zero.stdout_json()["error"]["code"],
        "tracking-checkpoint-live-missing-issue"
    );
    assert_eq!(fs::read_to_string(&log_path).unwrap_or_default(), "");

    write_run_state(&rs_path, "ready_for_close");
    let conflict = common::run_plan_issue_with_options(
        &[
            "--format",
            "json",
            "--repo",
            "https://gitlab.com/owner/repo",
            "tracking",
            "checkpoint",
            "--run-state",
            rs_path.to_str().expect("run state"),
            "--provider-repo",
            "https://github.com/owner/repo",
            "--post",
            "state",
            "--live",
        ],
        provider_stub_options(stub.path(), &[("FORGE_CLI_STUB_LOG", &log_s)]),
    );
    assert_ne!(conflict.code, 0);
    assert_eq!(
        conflict.stdout_json()["error"]["code"],
        "tracking-checkpoint-live-target-mismatch"
    );
    assert_eq!(fs::read_to_string(&log_path).unwrap_or_default(), "");
}

#[test]
fn tracking_checkpoint_live_redacts_qualified_repository_credentials_in_errors() {
    let tmp = TempDir::new().expect("tmp");
    let stub = StubBinDir::new();
    stub.write_exe("forge-cli", common::forge_cli_stub_script());
    let rs_path = tmp.path().join("run-state.json");
    let log_path = tmp.path().join("forge-cli.log");
    write_provider_bound_run_state(&rs_path, "group/project", "gitlab", "gitlab.com", 999);
    let log_s = log_path.to_string_lossy().to_string();

    let out = common::run_plan_issue_with_options(
        &[
            "--format",
            "json",
            "tracking",
            "checkpoint",
            "--run-state",
            rs_path.to_str().expect("run state"),
            "--provider-repo",
            "https://user:secret@github.com/group/project",
            "--post",
            "state",
            "--live",
        ],
        provider_stub_options(stub.path(), &[("FORGE_CLI_STUB_LOG", &log_s)]),
    );
    assert_ne!(out.code, 0);
    assert_eq!(
        out.stdout_json()["error"]["code"],
        "tracking-checkpoint-live-target-mismatch"
    );
    assert!(
        !out.stdout_text().contains("secret"),
        "{}",
        out.stdout_text()
    );
    assert!(
        !out.stdout_text().contains("user:"),
        "{}",
        out.stdout_text()
    );
    assert_eq!(fs::read_to_string(&log_path).unwrap_or_default(), "");
}

#[test]
fn tracking_checkpoint_live_rejects_wrong_tracking_issue_identity_before_posting() {
    let fixture = pre_closeout_fixture();
    let (body_json, comments_json) = provider_evidence_env(&fixture);
    let tmp = TempDir::new().expect("tmp");
    let stub = StubBinDir::new();
    stub.write_exe("forge-cli", common::forge_cli_stub_script());
    let checkout = tmp.path().join("checkout");
    fs::create_dir(&checkout).expect("checkout");
    assert!(
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&checkout)
            .status()
            .expect("git init")
            .success()
    );
    assert!(
        std::process::Command::new("git")
            .args([
                "remote",
                "add",
                "origin",
                "https://gitlab.com/group/project.git",
            ])
            .current_dir(&checkout)
            .status()
            .expect("git remote add")
            .success()
    );
    let execution_state = checkout.join("provider-execution-state.md");
    fs::write(
        &execution_state,
        "## Execution State\n\n- Target scope: provider target\n- Tracking issue: <https://github.com/other/repo/issues/999>\n\n## Task Ledger\n\n| ID | Status | Task | Evidence |\n| --- | --- | --- | --- |\n| 1.2 | done | demo | test |\n",
    )
    .expect("execution state");
    let rs_path = tmp.path().join("run-state.json");
    let log_path = tmp.path().join("forge-cli.log");
    write_provider_bound_run_state(&rs_path, "group/project", "gitlab", "gitlab.com", 999);
    let mut run: Value =
        serde_json::from_str(&fs::read_to_string(&rs_path).expect("run state")).expect("run json");
    run["execution_state_file"] = json!(execution_state);
    fs::write(&rs_path, run.to_string()).expect("bound run state");
    let log_s = log_path.to_string_lossy().to_string();

    let out = common::run_plan_issue_with_options(
        &[
            "--format",
            "json",
            "tracking",
            "checkpoint",
            "--run-state",
            rs_path.to_str().expect("run state"),
            "--post",
            "state",
            "--live",
        ],
        provider_stub_options(
            stub.path(),
            &[
                ("FORGE_CLI_STUB_VIEW_BODY_JSON", &body_json),
                ("FORGE_CLI_STUB_VIEW_COMMENTS_JSON", &comments_json),
                ("FORGE_CLI_STUB_LOG", &log_s),
            ],
        ),
    );
    assert_eq!(
        out.code,
        0,
        "stdout={} stderr={}",
        out.stdout_text(),
        out.stderr_text()
    );
    let result = &out.stdout_json()["payload"]["result"];
    let blocked_codes: Vec<&str> = result["blocked"]
        .as_array()
        .expect("blocked")
        .iter()
        .filter_map(|entry| entry["code"].as_str())
        .collect();
    assert!(
        blocked_codes.contains(&"execution-state-issue-mismatch"),
        "{result}"
    );
    assert_eq!(result["posted"], json!([]));
    let log = fs::read_to_string(log_path).expect("provider log");
    assert!(log.contains("issue view 999 --with-comments"), "{log}");
    assert!(!log.contains("issue comment 999"), "{log}");
}

#[test]
fn tracking_checkpoint_live_local_provider_accepts_git_confined_source_ledger_without_origin() {
    let fixture = pre_closeout_fixture();
    let (body_json, comments_json) = provider_evidence_env(&fixture);
    let tmp = TempDir::new().expect("tmp");
    let stub = StubBinDir::new();
    stub.write_exe("forge-cli", common::forge_cli_stub_script());
    let checkout = tmp.path().join("source-checkout");
    fs::create_dir(&checkout).expect("checkout");
    assert!(
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&checkout)
            .status()
            .expect("git init")
            .success()
    );
    let execution_state = checkout.join("local-execution-state.md");
    fs::write(
        &execution_state,
        "## Execution State\n\n- Target scope: local provider target\n\n## Task Ledger\n\n| ID | Status | Task | Evidence |\n| --- | --- | --- | --- |\n| 1.2 | done | demo | test |\n",
    )
    .expect("execution state");
    let rs_path = tmp.path().join("run-state.json");
    let log_path = tmp.path().join("forge-cli.log");
    let run = json!({
        "schema": "plan-issue.execution-run.v1",
        "run_id": "local-provider-run",
        "repo": "demo",
        "repo_provider": "local",
        "issue": 999,
        "profile": "tracking",
        "phase": "ready_for_close",
        "created_at": "2026-05-26T00:00:00Z",
        "updated_at": "2026-05-26T01:00:00Z",
        "execution_state_file": execution_state,
        "selected_scope": {"task": "1.2", "title": "demo"}
    });
    fs::write(&rs_path, run.to_string()).expect("run state");
    let log_s = log_path.to_string_lossy().to_string();

    let out = common::run_plan_issue_with_options(
        &[
            "--format",
            "json",
            "tracking",
            "checkpoint",
            "--run-state",
            rs_path.to_str().expect("run state"),
            "--post",
            "state",
            "--live",
        ],
        provider_stub_options(
            stub.path(),
            &[
                ("FORGE_CLI_STUB_VIEW_BODY_JSON", &body_json),
                ("FORGE_CLI_STUB_VIEW_COMMENTS_JSON", &comments_json),
                ("FORGE_CLI_STUB_LOG", &log_s),
            ],
        ),
    );
    assert_eq!(
        out.code,
        0,
        "stdout={} stderr={}",
        out.stdout_text(),
        out.stderr_text()
    );
    let result = &out.stdout_json()["payload"]["result"];
    assert_eq!(result["blocked"], json!([]), "{result}");
    assert_eq!(result["posted"].as_array().map(Vec::len), Some(1));
    assert!(
        fs::read_to_string(&execution_state)
            .expect("healed execution state")
            .contains("local://demo/issues/999")
    );
    let log = fs::read_to_string(log_path).expect("provider log");
    assert!(
        log.contains("--repo demo issue view 999 --with-comments"),
        "{log}"
    );
    assert!(log.contains("--repo demo issue comment 999"), "{log}");
}
