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

fn run_local_start_plan(agent_home: &str, repo: &str) -> common::CmdOut {
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
        &[("AGENT_HOME", agent_home)],
    )
}

#[test]
fn start_plan_emits_canonical_artifacts() {
    let tmp = TempDir::new().expect("tempdir");
    let agent_home = tmp.path().join("agent-home");
    fs::create_dir_all(&agent_home).expect("create agent home");
    common::seed_agent_home_prompts(&agent_home);
    let agent_home_s = agent_home.to_string_lossy().to_string();

    let out = run_local_start_plan(&agent_home_s, "graysurf/plan-issue-smoke");
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);

    let payload = parse_json(&out.stdout);
    let result = &payload["payload"]["result"];
    let issue_root = result["issue_root"].as_str().expect("issue_root in result");
    let expected_issue_root = agent_home
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

    let main_init_path = result["main_agent_init_snapshot_path"]
        .as_str()
        .expect("main_agent_init_snapshot_path in result");
    assert_eq!(
        main_init_path,
        expected_issue_root
            .join("prompts")
            .join("plan-issue-delivery-main-agent-init.snapshot.md")
            .to_string_lossy()
            .to_string()
    );
    let snapshot = fs::read_to_string(main_init_path).expect("read main-init snapshot");
    assert!(
        snapshot.contains("Main Agent Init"),
        "snapshot must mirror source content: {snapshot}"
    );
}

#[test]
fn start_plan_writes_plan_branch_ref() {
    let tmp = TempDir::new().expect("tempdir");
    let agent_home = tmp.path().join("agent-home");
    fs::create_dir_all(&agent_home).expect("create agent home");
    common::seed_agent_home_prompts(&agent_home);
    let agent_home_s = agent_home.to_string_lossy().to_string();

    let out = run_local_start_plan(&agent_home_s, "graysurf/plan-issue-smoke");
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);

    let payload = parse_json(&out.stdout);
    let plan_branch_ref_path = payload["payload"]["result"]["plan_branch_ref_path"]
        .as_str()
        .expect("plan_branch_ref_path in result");

    let expected_path = agent_home
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
    let agent_home = tmp.path().join("agent-home");
    fs::create_dir_all(&agent_home).expect("create agent home");
    common::seed_agent_home_prompts(&agent_home);
    let agent_home_s = agent_home.to_string_lossy().to_string();

    let out = run_local_start_plan(&agent_home_s, "graysurf/plan-issue-smoke");
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
fn start_plan_fails_on_missing_main_agent_init_source() {
    let tmp = TempDir::new().expect("tempdir");
    let agent_home = tmp.path().join("agent-home");
    fs::create_dir_all(&agent_home).expect("create agent home");
    let agent_home_s = agent_home.to_string_lossy().to_string();

    let out = run_local_start_plan(&agent_home_s, "graysurf/plan-issue-smoke");
    assert_eq!(out.code, 1, "stdout: {} stderr: {}", out.stdout, out.stderr);
    assert!(
        out.stderr
            .contains("main-agent-init-snapshot-source-missing")
            || out
                .stdout
                .contains("main-agent-init-snapshot-source-missing"),
        "missing-source error not surfaced; stdout={} stderr={}",
        out.stdout,
        out.stderr
    );
}
