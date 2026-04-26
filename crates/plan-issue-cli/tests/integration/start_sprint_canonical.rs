use std::fs;
use std::path::PathBuf;

use pretty_assertions::assert_eq;
use serde_json::Value;
use tempfile::TempDir;

use crate::common;

const PLAN_PATH: &str =
    "crates/plan-issue-cli/tests/fixtures/plans/plan-issue-rust-cli-full-delivery-plan.md";

fn parse_json(stdout: &str) -> Value {
    serde_json::from_str(stdout).expect("stdout should be valid JSON")
}

struct StartSprintRun {
    payload: Value,
    sprint_root: PathBuf,
    issue_root: PathBuf,
}

fn run_local_start_sprint(
    agent_home: &str,
    sprint: &str,
    issue: &str,
    repo: &str,
) -> StartSprintRun {
    let out = common::run_plan_issue_local_with_env(
        &[
            "--format",
            "json",
            "--dry-run",
            "--repo",
            repo,
            "start-sprint",
            "--plan",
            PLAN_PATH,
            "--issue",
            issue,
            "--sprint",
            sprint,
            "--strategy",
            "auto",
            "--default-pr-grouping",
            "per-sprint",
            "--no-comment",
        ],
        &[("AGENT_HOME", agent_home)],
    );
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let payload = parse_json(&out.stdout);
    let sprint_root = PathBuf::from(
        payload["payload"]["result"]["sprint_root"]
            .as_str()
            .expect("sprint_root in result"),
    );
    let issue_root = sprint_root
        .parent()
        .map(|p| p.to_path_buf())
        .expect("issue_root parent");
    StartSprintRun {
        payload,
        sprint_root,
        issue_root,
    }
}

fn seeded_agent_home() -> (TempDir, PathBuf, String) {
    let tmp = TempDir::new().expect("tempdir");
    let agent_home = tmp.path().join("agent-home");
    fs::create_dir_all(&agent_home).expect("agent home");
    common::seed_agent_home_prompts(&agent_home);
    let agent_home_s = agent_home.to_string_lossy().to_string();
    (tmp, agent_home, agent_home_s)
}

#[test]
fn start_sprint_emits_plan_snapshot() {
    let (_tmp, _ah, ah_s) = seeded_agent_home();
    let run = run_local_start_sprint(&ah_s, "3", "217", "graysurf/plan-issue-smoke");

    let snapshot_path = PathBuf::from(
        run.payload["payload"]["result"]["plan_snapshot_path"]
            .as_str()
            .expect("plan_snapshot_path"),
    );
    let expected = run.issue_root.join("plan").join("plan.snapshot.md");
    assert_eq!(snapshot_path, expected);
    assert!(snapshot_path.is_file(), "plan snapshot not on disk");

    let snapshot = fs::read_to_string(&snapshot_path).expect("read snapshot");
    assert!(
        snapshot.contains("Plan: Rust Plan-Issue CLI Full Delivery"),
        "snapshot must mirror plan source: {snapshot}"
    );
}

#[test]
fn start_sprint_emits_subagent_init_snapshot() {
    let (_tmp, _ah, ah_s) = seeded_agent_home();
    let run = run_local_start_sprint(&ah_s, "3", "217", "graysurf/plan-issue-smoke");

    let snapshot_path = PathBuf::from(
        run.payload["payload"]["result"]["subagent_init_snapshot_path"]
            .as_str()
            .expect("subagent_init_snapshot_path"),
    );
    let expected = run
        .sprint_root
        .join("prompts")
        .join("plan-issue-delivery-subagent-init.snapshot.md");
    assert_eq!(snapshot_path, expected);
    assert!(snapshot_path.is_file());

    let snapshot = fs::read_to_string(&snapshot_path).expect("read snapshot");
    assert!(
        snapshot.contains("Subagent Init"),
        "snapshot must mirror source: {snapshot}"
    );
}

#[test]
fn start_sprint_emits_dispatch_record_per_task() {
    let (_tmp, _ah, ah_s) = seeded_agent_home();
    let run = run_local_start_sprint(&ah_s, "3", "217", "graysurf/plan-issue-smoke");

    let dispatch_paths = run.payload["payload"]["result"]["dispatch_record_paths"]
        .as_array()
        .expect("dispatch_record_paths");
    let record_count = run.payload["payload"]["result"]["record_count"]
        .as_u64()
        .expect("record_count");
    assert_eq!(
        dispatch_paths.len() as u64,
        record_count,
        "one dispatch record per task: {dispatch_paths:?}"
    );

    for value in dispatch_paths {
        let raw = value.as_str().expect("dispatch path string");
        assert!(
            raw.contains("/manifests/dispatch-"),
            "dispatch path under manifests/: {raw}"
        );
        let record_text = fs::read_to_string(raw).expect("read dispatch record");
        let record: Value = serde_json::from_str(&record_text).expect("dispatch JSON parses");
        for key in [
            "task_id",
            "task_prompt_path",
            "subagent_init_snapshot_path",
            "plan_snapshot_path",
            "worktree",
            // Task 1.4: explicit absolute worktree path for orchestrators.
            "worktree_abs_path",
            "branch",
            "execution_mode",
            "pr_group",
            "base_branch",
            "workflow_role",
        ] {
            assert!(
                record.get(key).is_some(),
                "missing key {key} in {record_text}"
            );
        }
        assert_eq!(record["workflow_role"], "implementation");
        assert_eq!(record["base_branch"], "plan/issue-217");
        // Task 1.4: worktree_abs_path is absolute and lives under the
        // canonical $AGENT_HOME/out/plan-issue-delivery/<slug>/issue-N/worktrees/ tree.
        let worktree_abs = record["worktree_abs_path"]
            .as_str()
            .expect("worktree_abs_path string");
        assert!(
            std::path::Path::new(worktree_abs).is_absolute(),
            "worktree_abs_path must be absolute: {worktree_abs}"
        );
        assert!(
            worktree_abs.contains("/out/plan-issue-delivery/")
                && worktree_abs.contains("/issue-217/worktrees/"),
            "worktree_abs_path must live under canonical runtime root: {worktree_abs}"
        );
        // For backwards compatibility v1 readers expect `worktree` to be
        // identical to the new field.
        assert_eq!(record["worktree"], record["worktree_abs_path"]);
    }
}

#[test]
fn start_sprint_emits_prompt_manifest() {
    let (_tmp, _ah, ah_s) = seeded_agent_home();
    let run = run_local_start_sprint(&ah_s, "3", "217", "graysurf/plan-issue-smoke");

    let manifest_path = PathBuf::from(
        run.payload["payload"]["result"]["prompt_manifest_path"]
            .as_str()
            .expect("prompt_manifest_path"),
    );
    let expected = run
        .sprint_root
        .join("manifests")
        .join("prompt-manifest.tsv");
    assert_eq!(manifest_path, expected);
    let manifest = fs::read_to_string(&manifest_path).expect("read manifest");
    let mut lines = manifest.lines();
    assert_eq!(
        lines.next(),
        Some("task_id\tprompt_path\texecution_mode\tworkflow_role"),
        "header"
    );

    let record_count = run.payload["payload"]["result"]["record_count"]
        .as_u64()
        .expect("record_count");
    let body_lines: Vec<&str> = lines.filter(|line| !line.is_empty()).collect();
    assert_eq!(body_lines.len() as u64, record_count, "one row per task");
    for line in &body_lines {
        let cells: Vec<&str> = line.split('\t').collect();
        assert_eq!(cells.len(), 4, "TSV row shape: {line}");
        assert!(cells[1].contains("/prompts/"), "prompt path: {line}");
        assert_eq!(cells[3], "implementation");
    }
}

#[test]
fn start_sprint_relocates_task_prompt() {
    let (_tmp, _ah, ah_s) = seeded_agent_home();
    let run = run_local_start_sprint(&ah_s, "3", "217", "graysurf/plan-issue-smoke");

    let prompt_files = run.payload["payload"]["result"]["subagent_prompt_files"]
        .as_array()
        .expect("subagent_prompt_files");
    assert!(
        !prompt_files.is_empty(),
        "expected at least one prompt file"
    );

    let canonical_dir = run.sprint_root.join("prompts");
    for value in prompt_files {
        let raw = value.as_str().expect("prompt path string");
        let path = std::path::Path::new(raw);
        assert!(
            path.starts_with(&canonical_dir),
            "prompt path must live under canonical prompts dir; path={raw}, expected_dir={}",
            canonical_dir.display()
        );
        let file_name = path
            .file_name()
            .and_then(|s| s.to_str())
            .expect("file name");
        assert!(
            file_name.ends_with(".md"),
            "prompt file should be <TASK_ID>.md: {file_name}"
        );
        let stem_chars: Vec<char> = file_name
            .strip_suffix(".md")
            .expect("md suffix")
            .chars()
            .collect();
        assert!(
            stem_chars
                .iter()
                .all(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-'),
            "task id stem should not embed retired-format suffixes: {file_name}"
        );
    }
}

#[test]
fn start_sprint_emits_repo_slug_and_v2_schema_version() {
    // Task 1.1: start-sprint result exposes the runtime repo slug;
    // schema_version bumps to v2.
    let (_tmp, _ah, ah_s) = seeded_agent_home();
    let run = run_local_start_sprint(&ah_s, "3", "217", "graysurf/plan-issue-smoke");
    assert_eq!(
        run.payload["schema_version"], "plan-issue-cli.start.sprint.v2",
        "schema_version should bump to v2"
    );
    assert_eq!(
        run.payload["payload"]["result"]["repo_slug"], "graysurf__plan-issue-smoke",
        "result.repo_slug should mirror runtime_layout::repo_slug derivation"
    );
}

#[test]
fn start_sprint_emits_pr_groups_array() {
    // Task 1.3: start-sprint payload includes `pr_groups` listing every
    // group actually created (name + task_ids).
    let (_tmp, _ah, ah_s) = seeded_agent_home();
    let run = run_local_start_sprint(&ah_s, "3", "217", "graysurf/plan-issue-smoke");

    let pr_groups = run.payload["payload"]["result"]["pr_groups"]
        .as_array()
        .expect("pr_groups in result");
    assert!(
        !pr_groups.is_empty(),
        "pr_groups must list at least one group: {pr_groups:?}"
    );

    let mut total_tasks = 0;
    for group in pr_groups {
        let name = group["name"].as_str().expect("group name");
        assert!(
            !name.is_empty(),
            "pr_group name must be non-empty: {group:?}"
        );
        let task_ids = group["task_ids"].as_array().expect("task_ids array");
        assert!(
            !task_ids.is_empty(),
            "pr_group task_ids must be non-empty: {group:?}"
        );
        total_tasks += task_ids.len();
    }

    let record_count = run.payload["payload"]["result"]["record_count"]
        .as_u64()
        .expect("record_count");
    assert_eq!(
        total_tasks as u64, record_count,
        "every task should appear in exactly one pr-group"
    );
}

#[test]
fn dispatch_record_omits_runtime_adapter_keys() {
    let (_tmp, _ah, ah_s) = seeded_agent_home();
    let run = run_local_start_sprint(&ah_s, "3", "217", "graysurf/plan-issue-smoke");

    let dispatch_paths = run.payload["payload"]["result"]["dispatch_record_paths"]
        .as_array()
        .expect("dispatch_record_paths");
    for value in dispatch_paths {
        let raw = value.as_str().expect("dispatch path string");
        let record_text = fs::read_to_string(raw).expect("read dispatch record");
        for adapter_key in [
            "runtime_name",
            "runtime_role",
            "runtime_role_fallback_reason",
        ] {
            assert!(
                !record_text.contains(adapter_key),
                "{adapter_key} must not be emitted by the binary: {record_text}"
            );
        }
    }
}

#[test]
fn start_sprint_skips_init_snapshot_when_env_set() {
    // Adapter opt-out: setting PLAN_ISSUE_SKIP_INIT_SNAPSHOT=1 makes
    // start-sprint skip the canonical subagent-init snapshot copy
    // entirely. Plan snapshot, prompt manifest, and dispatch records
    // are still produced (only the init snapshot is conditional).
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
            "start-sprint",
            "--plan",
            PLAN_PATH,
            "--issue",
            "217",
            "--sprint",
            "3",
            "--strategy",
            "auto",
            "--default-pr-grouping",
            "per-sprint",
            "--no-comment",
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

    let subagent_init_path = result["subagent_init_snapshot_path"]
        .as_str()
        .expect("subagent_init_snapshot_path string");
    assert!(
        !std::path::Path::new(subagent_init_path).exists(),
        "subagent init snapshot must NOT be written when env is set: {subagent_init_path}"
    );

    let plan_snapshot_path = result["plan_snapshot_path"]
        .as_str()
        .expect("plan_snapshot_path string");
    assert!(
        std::path::Path::new(plan_snapshot_path).is_file(),
        "plan snapshot must still be written: {plan_snapshot_path}"
    );

    let dispatch_paths = result["dispatch_record_paths"]
        .as_array()
        .expect("dispatch_record_paths");
    assert!(
        !dispatch_paths.is_empty(),
        "dispatch records must still be produced when env is set"
    );

    let sprint_root = PathBuf::from(
        result["sprint_root"]
            .as_str()
            .expect("sprint_root in result"),
    );
    let issue_root_path = sprint_root.parent().expect("issue_root parent");
    let stray = walk_init_snapshots(issue_root_path);
    assert!(
        stray.is_empty(),
        "no *-init.snapshot.md files allowed under issue_root when env is set: {stray:?}"
    );
}

fn walk_init_snapshots(root: &std::path::Path) -> Vec<String> {
    let mut hits = Vec::new();
    walk_init_recursive(root, &mut hits);
    hits
}

fn walk_init_recursive(dir: &std::path::Path, sink: &mut Vec<String>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        let path = entry.path();
        if meta.is_dir() {
            walk_init_recursive(&path, sink);
        } else if let Some(name) = path.file_name().and_then(|s| s.to_str())
            && name.ends_with("-init.snapshot.md")
        {
            sink.push(path.to_string_lossy().to_string());
        }
    }
}
