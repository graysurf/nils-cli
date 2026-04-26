use std::fs;

use pretty_assertions::assert_eq;
use serde_json::Value;
use tempfile::TempDir;

use crate::common;

const PLAN_PATH: &str =
    "crates/plan-issue-cli/tests/fixtures/plans/plan-issue-rust-cli-full-delivery-plan.md";

fn parse_json(stdout: &str) -> Value {
    serde_json::from_str(stdout).expect("stdout should be valid JSON")
}

fn run_local_start_plan(state_dir: &str, repo: &str) -> common::CmdOut {
    common::run_plan_issue_local_with_env(
        &[
            "--format",
            "json",
            "--dry-run",
            "--repo",
            repo,
            "start-plan",
            "--plan",
            PLAN_PATH,
            "--pr-grouping",
            "per-sprint",
        ],
        &[("PLAN_ISSUE_HOME", state_dir)],
    )
}

#[test]
fn start_plan_emits_canonical_artifacts() {
    let tmp = TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state-dir");
    fs::create_dir_all(&state_dir).expect("create agent home");
    let state_dir_s = state_dir.to_string_lossy().to_string();

    let out = run_local_start_plan(&state_dir_s, "graysurf/plan-issue-smoke");
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);

    let payload = parse_json(&out.stdout);
    let result = &payload["payload"]["result"];
    let issue_root = result["issue_root"].as_str().expect("issue_root in result");
    let expected_issue_root = state_dir
        .join("out")
        .join("plan-issue-delivery")
        .join("graysurf__plan-issue-smoke")
        .join("issue-999");
    assert_eq!(
        issue_root,
        expected_issue_root.to_string_lossy().to_string()
    );

    let task_spec_path = result["task_spec_path"]
        .as_str()
        .expect("task_spec_path in result");
    assert_eq!(
        task_spec_path,
        expected_issue_root
            .join("plan")
            .join("tasks.tsv")
            .to_string_lossy()
            .to_string()
    );
    assert!(
        std::path::Path::new(task_spec_path).is_file(),
        "task spec missing on disk: {task_spec_path}"
    );

    let issue_body_path = result["issue_body_path"]
        .as_str()
        .expect("issue_body_path in result");
    assert_eq!(
        issue_body_path,
        expected_issue_root
            .join("plan")
            .join("issue-body.md")
            .to_string_lossy()
            .to_string()
    );
    assert!(
        std::path::Path::new(issue_body_path).is_file(),
        "issue body missing on disk: {issue_body_path}"
    );

    // Init-snapshot machinery removed in 0.8: result must NOT include
    // `main_agent_init_snapshot_path` or `init_snapshot_skipped`.
    assert!(
        result.get("main_agent_init_snapshot_path").is_none(),
        "main_agent_init_snapshot_path must be absent post-0.8: {result}"
    );
    assert!(
        result.get("init_snapshot_skipped").is_none(),
        "init_snapshot_skipped must be absent post-0.8: {result}"
    );

    // No *-init.snapshot.md files should be written under the issue root.
    let issue_root_path = std::path::Path::new(issue_root);
    let stray = walk_for_init_snapshot(issue_root_path);
    assert!(
        stray.is_empty(),
        "no *-init.snapshot.md files should be written: {stray:?}"
    );
}

#[test]
fn start_plan_writes_plan_branch_ref() {
    let tmp = TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state-dir");
    fs::create_dir_all(&state_dir).expect("create agent home");
    let state_dir_s = state_dir.to_string_lossy().to_string();

    let out = run_local_start_plan(&state_dir_s, "graysurf/plan-issue-smoke");
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);

    let payload = parse_json(&out.stdout);
    let plan_branch_ref_path = payload["payload"]["result"]["plan_branch_ref_path"]
        .as_str()
        .expect("plan_branch_ref_path in result");

    let expected_path = state_dir
        .join("out")
        .join("plan-issue-delivery")
        .join("graysurf__plan-issue-smoke")
        .join("issue-999")
        .join("plan")
        .join("plan-branch.ref");
    assert_eq!(
        plan_branch_ref_path,
        expected_path.to_string_lossy().to_string()
    );

    let contents = fs::read_to_string(&expected_path).expect("read plan-branch.ref");
    assert_eq!(contents, "plan/issue-999");
    assert!(
        !contents.ends_with('\n'),
        "plan-branch.ref must not have a trailing newline: {contents:?}"
    );
}

#[test]
fn start_plan_local_uses_placeholder_issue() {
    let tmp = TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state-dir");
    fs::create_dir_all(&state_dir).expect("create agent home");
    let state_dir_s = state_dir.to_string_lossy().to_string();

    let out = run_local_start_plan(&state_dir_s, "graysurf/plan-issue-smoke");
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);

    let payload = parse_json(&out.stdout);
    let result = &payload["payload"]["result"];
    assert_eq!(result["issue_number"], 999);
    let issue_root = result["issue_root"].as_str().expect("issue_root in result");
    assert!(
        issue_root.contains("graysurf__plan-issue-smoke/issue-999"),
        "issue_root should embed placeholder: {issue_root}"
    );
}

#[test]
fn status_plan_emits_repo_slug_and_v2_schema_version() {
    // Task 1.1: status-plan exposes the runtime repo slug and bumps to v2.
    // Drive it via a body-file flow with an explicit --repo so the slug is
    // derivable without contacting GitHub.
    let tmp = TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state-dir");
    fs::create_dir_all(&state_dir).expect("create agent home");
    let state_dir_s = state_dir.to_string_lossy().to_string();

    // First run start-plan to produce an issue body we can feed into
    // status-plan.
    let start = run_local_start_plan(&state_dir_s, "graysurf/plan-issue-smoke");
    assert_eq!(start.code, 0, "start-plan stderr: {}", start.stderr);
    let start_payload = parse_json(&start.stdout);
    let issue_body_path = start_payload["payload"]["result"]["issue_body_path"]
        .as_str()
        .expect("issue_body_path")
        .to_string();

    let status = common::run_plan_issue_local_with_env(
        &[
            "--format",
            "json",
            "--dry-run",
            "--repo",
            "graysurf/plan-issue-smoke",
            "status-plan",
            "--body-file",
            &issue_body_path,
        ],
        &[("PLAN_ISSUE_HOME", &state_dir_s)],
    );
    assert_eq!(status.code, 0, "status-plan stderr: {}", status.stderr);

    let payload = parse_json(&status.stdout);
    assert_eq!(
        payload["schema_version"], "plan-issue-cli.status.plan.v2",
        "schema_version should bump to v2"
    );
    assert_eq!(
        payload["payload"]["result"]["repo_slug"], "graysurf__plan-issue-smoke",
        "result.repo_slug should mirror runtime_layout::repo_slug derivation"
    );
}

#[test]
fn start_plan_emits_repo_slug_and_v2_schema_version() {
    // Task 1.1: result payload exposes the runtime repo slug; schema_version
    // bumps to v2 so consumers know the new field is present.
    let tmp = TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state-dir");
    fs::create_dir_all(&state_dir).expect("create agent home");
    let state_dir_s = state_dir.to_string_lossy().to_string();

    let out = run_local_start_plan(&state_dir_s, "graysurf/plan-issue-smoke");
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);

    let payload = parse_json(&out.stdout);
    assert_eq!(
        payload["schema_version"], "plan-issue-cli.start.plan.v2",
        "schema_version should bump to v2: {}",
        out.stdout
    );
    let result = &payload["payload"]["result"];
    assert_eq!(
        result["repo_slug"], "graysurf__plan-issue-smoke",
        "result.repo_slug should mirror runtime_layout::repo_slug derivation"
    );

    // Round-trip: a second run produces an identical repo_slug.
    let out2 = run_local_start_plan(&state_dir_s, "graysurf/plan-issue-smoke");
    assert_eq!(out2.code, 0, "stderr: {}", out2.stderr);
    let payload2 = parse_json(&out2.stdout);
    assert_eq!(
        payload2["payload"]["result"]["repo_slug"], result["repo_slug"],
        "repo_slug must round-trip identically across runs"
    );
}

fn walk_for_init_snapshot(root: &std::path::Path) -> Vec<String> {
    let mut hits = Vec::new();
    walk_recursive(root, &mut hits);
    hits
}

fn walk_recursive(dir: &std::path::Path, sink: &mut Vec<String>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        let path = entry.path();
        if meta.is_dir() {
            walk_recursive(&path, sink);
        } else if let Some(name) = path.file_name().and_then(|s| s.to_str())
            && name.ends_with("-init.snapshot.md")
        {
            sink.push(path.to_string_lossy().to_string());
        }
    }
}
