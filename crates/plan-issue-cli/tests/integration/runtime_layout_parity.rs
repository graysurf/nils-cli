use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use pretty_assertions::assert_eq;
use serde_json::Value;
use tempfile::TempDir;

use crate::common;

const FIXTURE_PLAN_FROM_REPO: &str = "crates/plan-issue-cli/tests/fixtures/runtime_layout/plan.md";
const FIXTURE_PLAN_FROM_CRATE: &str = "tests/fixtures/runtime_layout/plan.md";
const FIXTURE_REPO: &str = "graysurf/runtime-layout-fixture";
const FIXTURE_REPO_SLUG: &str = "graysurf__runtime-layout-fixture";
const PLACEHOLDER_ISSUE: u64 = 999;

fn parse_json(stdout: &str) -> Value {
    serde_json::from_str(stdout).expect("stdout should be valid JSON")
}

fn relativize(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .map(|stripped| stripped.to_string_lossy().to_string())
        .unwrap_or_else(|_| path.to_string_lossy().to_string())
}

fn collect_relative_paths(root: &Path) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    walk_recursive(root, root, &mut out);
    out
}

fn walk_recursive(root: &Path, current: &Path, sink: &mut BTreeSet<String>) {
    let Ok(entries) = fs::read_dir(current) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let path = entry.path();
        if metadata.is_dir() {
            walk_recursive(root, &path, sink);
        } else if metadata.is_file() {
            sink.insert(relativize(root, &path));
        }
    }
}

#[test]
fn runtime_layout_parity() {
    let tmp = TempDir::new().expect("tempdir");
    let agent_home = tmp.path().join("agent-home");
    fs::create_dir_all(&agent_home).expect("agent home");
    common::seed_agent_home_prompts(&agent_home);
    let agent_home_s = agent_home.to_string_lossy().to_string();

    let start_plan_out = common::run_plan_issue_local_with_env(
        &[
            "--format",
            "json",
            "--dry-run",
            "--repo",
            FIXTURE_REPO,
            "start-plan",
            "--plan",
            FIXTURE_PLAN_FROM_REPO,
            "--pr-grouping",
            "per-sprint",
        ],
        &[("AGENT_HOME", &agent_home_s)],
    );
    assert_eq!(start_plan_out.code, 0, "stderr: {}", start_plan_out.stderr);

    let start_sprint_out = common::run_plan_issue_local_with_env(
        &[
            "--format",
            "json",
            "--dry-run",
            "--repo",
            FIXTURE_REPO,
            "start-sprint",
            "--plan",
            FIXTURE_PLAN_FROM_REPO,
            "--issue",
            "999",
            "--sprint",
            "1",
            "--pr-grouping",
            "per-sprint",
            "--no-comment",
        ],
        &[("AGENT_HOME", &agent_home_s)],
    );
    assert_eq!(
        start_sprint_out.code, 0,
        "stderr: {}",
        start_sprint_out.stderr
    );

    let runtime_root = agent_home
        .join("out")
        .join("plan-issue-delivery")
        .join(FIXTURE_REPO_SLUG)
        .join(format!("issue-{PLACEHOLDER_ISSUE}"));
    let actual_files = collect_relative_paths(&runtime_root);
    let expected_files: BTreeSet<String> = [
        "plan/issue-body.md",
        "plan/plan-branch.ref",
        "plan/plan.snapshot.md",
        "plan/tasks.tsv",
        "prompts/plan-issue-delivery-main-agent-init.snapshot.md",
        "sprint-1/manifests/dispatch-S1T1.json",
        "sprint-1/manifests/prompt-manifest.tsv",
        "sprint-1/prompts/S1T1.md",
        "sprint-1/prompts/plan-issue-delivery-subagent-init.snapshot.md",
        "sprint-1/specs/sprint-task-spec.tsv",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    assert_eq!(
        actual_files,
        expected_files,
        "canonical artifact tree mismatch under {}",
        runtime_root.display()
    );

    let plan_branch = fs::read_to_string(runtime_root.join("plan/plan-branch.ref"))
        .expect("read plan-branch.ref");
    assert_eq!(plan_branch, format!("plan/issue-{PLACEHOLDER_ISSUE}"));

    let plan_snapshot =
        fs::read_to_string(runtime_root.join("plan/plan.snapshot.md")).expect("read plan snapshot");
    let plan_source = fs::read_to_string(FIXTURE_PLAN_FROM_CRATE).expect("read fixture plan");
    assert_eq!(
        plan_snapshot, plan_source,
        "plan snapshot must mirror source byte-for-byte"
    );

    let main_init = fs::read_to_string(
        runtime_root.join("prompts/plan-issue-delivery-main-agent-init.snapshot.md"),
    )
    .expect("read main-init snapshot");
    let main_source =
        fs::read_to_string(agent_home.join("prompts/plan-issue-delivery-main-agent-init.md"))
            .expect("read main-init source");
    assert_eq!(main_init, main_source);

    let subagent_init = fs::read_to_string(
        runtime_root.join("sprint-1/prompts/plan-issue-delivery-subagent-init.snapshot.md"),
    )
    .expect("read subagent-init snapshot");
    let subagent_source =
        fs::read_to_string(agent_home.join("prompts/plan-issue-delivery-subagent-init.md"))
            .expect("read subagent-init source");
    assert_eq!(subagent_init, subagent_source);

    let dispatch_path = runtime_root.join("sprint-1/manifests/dispatch-S1T1.json");
    let dispatch_text = fs::read_to_string(&dispatch_path).expect("read dispatch");
    let dispatch: Value = serde_json::from_str(&dispatch_text).expect("dispatch parses");
    assert_eq!(dispatch["task_id"], "S1T1");
    assert_eq!(dispatch["execution_mode"], "per-sprint");
    assert_eq!(dispatch["base_branch"], "plan/issue-999");
    assert_eq!(dispatch["workflow_role"], "implementation");
    assert_eq!(
        dispatch["task_prompt_path"]
            .as_str()
            .expect("task_prompt_path string"),
        runtime_root
            .join("sprint-1/prompts/S1T1.md")
            .to_string_lossy()
            .to_string()
    );
    assert_eq!(
        dispatch["plan_snapshot_path"]
            .as_str()
            .expect("plan_snapshot_path string"),
        runtime_root
            .join("plan/plan.snapshot.md")
            .to_string_lossy()
            .to_string()
    );
    for adapter_key in [
        "runtime_name",
        "runtime_role",
        "runtime_role_fallback_reason",
    ] {
        assert!(
            dispatch.get(adapter_key).is_none(),
            "binary must not emit {adapter_key}: {dispatch_text}"
        );
    }

    let manifest_path = runtime_root.join("sprint-1/manifests/prompt-manifest.tsv");
    let manifest = fs::read_to_string(&manifest_path).expect("read prompt manifest");
    let mut lines = manifest.lines();
    assert_eq!(
        lines.next(),
        Some("task_id\tprompt_path\texecution_mode\tworkflow_role"),
        "manifest header"
    );
    let body_lines: Vec<&str> = lines.filter(|line| !line.is_empty()).collect();
    assert_eq!(body_lines.len(), 1, "one row per task");
    let cells: Vec<&str> = body_lines[0].split('\t').collect();
    assert_eq!(
        cells,
        vec![
            "S1T1",
            runtime_root
                .join("sprint-1/prompts/S1T1.md")
                .to_string_lossy()
                .as_ref(),
            "per-sprint",
            "implementation",
        ]
    );

    let plan_payload = parse_json(&start_plan_out.stdout);
    let sprint_payload = parse_json(&start_sprint_out.stdout);
    assert_eq!(
        plan_payload["payload"]["result"]["issue_number"], 999,
        "plan-issue-local placeholder issue"
    );
    assert_eq!(sprint_payload["payload"]["result"]["record_count"], 1);
    let sprint_root = sprint_payload["payload"]["result"]["sprint_root"]
        .as_str()
        .expect("sprint_root in result");
    assert_eq!(PathBuf::from(sprint_root), runtime_root.join("sprint-1"));
}
