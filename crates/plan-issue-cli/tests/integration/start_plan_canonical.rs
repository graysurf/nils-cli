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
fn status_plan_emits_repo_slug_and_v2_schema_version() {
    // Task 1.1: status-plan exposes the runtime repo slug and bumps to v2.
    // Drive it via a body-file flow with an explicit --repo so the slug is
    // derivable without contacting GitHub.
    let tmp = TempDir::new().expect("tempdir");
    let agent_home = tmp.path().join("agent-home");
    fs::create_dir_all(&agent_home).expect("create agent home");
    common::seed_agent_home_prompts(&agent_home);
    let agent_home_s = agent_home.to_string_lossy().to_string();

    // First run start-plan to produce an issue body we can feed into
    // status-plan.
    let start = run_local_start_plan(&agent_home_s, "graysurf/plan-issue-smoke");
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
        &[("AGENT_HOME", &agent_home_s)],
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
    let agent_home = tmp.path().join("agent-home");
    fs::create_dir_all(&agent_home).expect("create agent home");
    common::seed_agent_home_prompts(&agent_home);
    let agent_home_s = agent_home.to_string_lossy().to_string();

    let out = run_local_start_plan(&agent_home_s, "graysurf/plan-issue-smoke");
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
    let out2 = run_local_start_plan(&agent_home_s, "graysurf/plan-issue-smoke");
    assert_eq!(out2.code, 0, "stderr: {}", out2.stderr);
    let payload2 = parse_json(&out2.stdout);
    assert_eq!(
        payload2["payload"]["result"]["repo_slug"], result["repo_slug"],
        "repo_slug must round-trip identically across runs"
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

#[test]
fn start_plan_skips_init_snapshot_when_env_set() {
    // Adapter opt-out: setting PLAN_ISSUE_SKIP_INIT_SNAPSHOT=1 makes
    // the binary skip both the existence check on the canonical
    // main-agent init prompt and the copy into the runtime workspace,
    // even if `$AGENT_HOME/prompts/` is empty.
    let tmp = TempDir::new().expect("tempdir");
    let agent_home = tmp.path().join("agent-home");
    fs::create_dir_all(&agent_home).expect("create agent home");
    let agent_home_s = agent_home.to_string_lossy().to_string();

    let out = common::run_plan_issue_local_with_env(
        &[
            "--format",
            "json",
            "--dry-run",
            "--repo",
            "graysurf/plan-issue-smoke",
            "start-plan",
            "--plan",
            PLAN_PATH,
            "--pr-grouping",
            "per-sprint",
        ],
        &[
            ("AGENT_HOME", &agent_home_s),
            ("PLAN_ISSUE_SKIP_INIT_SNAPSHOT", "1"),
        ],
    );
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);

    let payload = parse_json(&out.stdout);
    let result = &payload["payload"]["result"];
    assert_eq!(
        result["init_snapshot_skipped"], true,
        "result must flag init_snapshot_skipped when env is set"
    );

    let main_init_path = result["main_agent_init_snapshot_path"]
        .as_str()
        .expect("main_agent_init_snapshot_path string");
    assert!(
        !std::path::Path::new(main_init_path).exists(),
        "main-agent init snapshot must NOT be written when env is set: {main_init_path}"
    );

    let issue_root = result["issue_root"].as_str().expect("issue_root in result");
    let issue_root_path = std::path::Path::new(issue_root);
    let stray = walk_for_init_snapshot(issue_root_path);
    assert!(
        stray.is_empty(),
        "no *-init.snapshot.md files allowed under issue_root when env is set: {stray:?}"
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
