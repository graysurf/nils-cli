use std::fs;
use std::path::Path;

use nils_test_support::cmd::{CmdOptions, CmdOutput, run_resolved};
use nils_test_support::git::{git, init_repo_main};
use pretty_assertions::assert_eq;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

fn run(dir: &Path, args: &[&str], envs: &[(&str, &str)]) -> CmdOutput {
    run_with_env_remove(dir, args, envs, &[])
}

fn run_with_env_remove(
    dir: &Path,
    args: &[&str],
    envs: &[(&str, &str)],
    remove_envs: &[&str],
) -> CmdOutput {
    let options = CmdOptions::new()
        .with_cwd(dir)
        .with_env_remove_many(remove_envs)
        .with_envs(envs);
    run_resolved("agent-out", args, &options)
}

#[derive(Deserialize, Serialize)]
struct TestCleanupPlan {
    agent_home: String,
    out_root: String,
    out_root_exists: bool,
    include_projects: bool,
    items: Vec<TestCleanupItem>,
    summary: TestCleanupSummary,
    plan_digest: String,
}

#[derive(Deserialize, Serialize)]
struct TestCleanupItem {
    name: String,
    path: String,
    kind: String,
    category: String,
    action: String,
    reason: String,
    size_bytes: u64,
    mtime_unix: Option<i64>,
    contains_skill_usage: bool,
    contains_test_first_evidence: bool,
}

#[derive(Deserialize, Serialize)]
struct TestCleanupSummary {
    total: usize,
    delete: usize,
    preserve: usize,
    needs_policy: usize,
    delete_bytes: u64,
    preserve_bytes: u64,
    needs_policy_bytes: u64,
}

#[derive(Serialize)]
struct TestCleanupPlanDigestInput<'a> {
    agent_home: &'a str,
    out_root: &'a str,
    out_root_exists: bool,
    include_projects: bool,
    items: &'a [TestCleanupItem],
    summary: &'a TestCleanupSummary,
}

fn recompute_cleanup_plan_digest(envelope: &mut Value) -> String {
    let mut plan: TestCleanupPlan =
        serde_json::from_value(envelope["result"].clone()).expect("cleanup plan");
    let digest_input = TestCleanupPlanDigestInput {
        agent_home: &plan.agent_home,
        out_root: &plan.out_root,
        out_root_exists: plan.out_root_exists,
        include_projects: plan.include_projects,
        items: &plan.items,
        summary: &plan.summary,
    };
    let bytes = serde_json::to_vec(&digest_input).expect("digest input");
    let digest = Sha256::digest(bytes);
    let hex = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    plan.plan_digest = format!("sha256:{hex}");
    envelope["result"] = serde_json::to_value(&plan).expect("plan value");
    plan.plan_digest
}

#[test]
fn project_outputs_path_without_creating_directory_by_default() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let agent_home = tmp.path().join("agent-home");
    let repo = init_repo_main();
    git(
        repo.path(),
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/sympoies/nils-cli.git",
        ],
    );

    let agent_home_arg = agent_home.to_string_lossy().to_string();
    let repo_arg = repo.path().to_string_lossy().to_string();
    let output = run(
        tmp.path(),
        &[
            "project",
            "--topic",
            "Bug Sweep!",
            "--repo",
            &repo_arg,
            "--agent-home",
            &agent_home_arg,
        ],
        &[],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let path = output.stdout_text();
    let path = path.trim();
    assert!(
        path.starts_with(&format!(
            "{}/out/projects/sympoies__nils-cli/",
            agent_home.display()
        )),
        "unexpected path: {path}"
    );
    assert!(
        path.ends_with("-bug-sweep"),
        "topic was not sanitized in path: {path}"
    );
    assert!(
        !Path::new(path).exists(),
        "--mkdir was not passed, path should not exist"
    );
}

#[test]
fn project_mkdir_creates_generated_directory() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let agent_home = tmp.path().join("agent-home");
    let agent_home_arg = agent_home.to_string_lossy().to_string();

    let output = run(
        tmp.path(),
        &[
            "project",
            "--topic",
            "Artifacts",
            "--repo-slug",
            "owner/repo",
            "--agent-home",
            &agent_home_arg,
            "--mkdir",
        ],
        &[],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let path = output.stdout_text();
    let path = Path::new(path.trim());
    assert!(path.is_dir(), "expected --mkdir to create {path:?}");
}

#[test]
fn project_json_uses_versioned_envelope() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let agent_home = tmp.path().join("agent-home");
    let agent_home_arg = agent_home.to_string_lossy().to_string();

    let output = run(
        tmp.path(),
        &[
            "project",
            "--topic",
            "API Smoke",
            "--repo-slug",
            "sympoies/nils-cli",
            "--agent-home",
            &agent_home_arg,
            "--format",
            "json",
        ],
        &[],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let value = output.stdout_json();
    assert_eq!(value["schema_version"], "cli.agent-out.project.v1");
    assert_eq!(value["command"], "agent-out project");
    assert_eq!(value["ok"], true);
    assert_eq!(value["result"]["project_slug"], "sympoies__nils-cli");
    assert_eq!(value["result"]["topic"], "api-smoke");
    assert_eq!(value["result"]["created"], false);
    let run_id = value["result"]["run_id"].as_str().expect("run_id");
    assert!(
        run_id.starts_with("20") && run_id.ends_with("-api-smoke") && run_id.len() == 15 + 1 + 9,
        "unexpected run_id shape: {run_id}"
    );
}

#[test]
fn project_env_outputs_shell_assignments() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let agent_home = tmp.path().join("agent home");
    let agent_home_arg = agent_home.to_string_lossy().to_string();

    let output = run(
        tmp.path(),
        &[
            "project",
            "--topic",
            "Env Mode",
            "--repo-slug",
            "owner/repo",
            "--agent-home",
            &agent_home_arg,
            "--format",
            "env",
        ],
        &[],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let stdout = output.stdout_text();
    assert!(
        stdout.contains("AGENT_OUT_PATH='"),
        "missing path assignment: {stdout}"
    );
    assert!(
        stdout.contains("AGENT_OUT_PROJECT_SLUG='owner__repo'"),
        "missing slug assignment: {stdout}"
    );
    assert!(
        stdout.contains("AGENT_OUT_TOPIC='env-mode'"),
        "missing topic assignment: {stdout}"
    );
}

#[test]
fn project_requires_agent_home_from_flag_or_env() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let output = run_with_env_remove(
        tmp.path(),
        &[
            "project",
            "--topic",
            "missing home",
            "--repo-slug",
            "owner/repo",
            "--format",
            "json",
        ],
        &[],
        &["AGENT_HOME"],
    );

    assert_eq!(output.code, 64, "stderr={}", output.stderr_text());
    let value = output.stdout_json();
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"]["code"], "missing-agent-home");
    assert!(
        !output.stdout_text().contains("sk-"),
        "json error should not contain secret-like material"
    );
}

#[test]
fn project_uses_agent_home_environment_when_flag_is_absent() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let agent_home = tmp.path().join("env-home");
    let agent_home_env = agent_home.to_string_lossy().to_string();
    let envs = [("AGENT_HOME", agent_home_env.as_str())];
    let output = run(
        tmp.path(),
        &[
            "project",
            "--topic",
            "env home",
            "--repo-slug",
            "owner/repo",
        ],
        &envs,
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    assert!(
        output
            .stdout_text()
            .starts_with(&format!("{}/out/projects/", agent_home.display())),
        "AGENT_HOME env was not used: {}",
        output.stdout_text()
    );
}

#[test]
fn path_for_supports_state_out_projects_topic_contract() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let agent_home = tmp.path().join("agent-home");
    let repo = init_repo_main();
    git(
        repo.path(),
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/sympoies/nils-cli.git",
        ],
    );

    let agent_home_arg = agent_home.to_string_lossy().to_string();
    let repo_arg = repo.path().to_string_lossy().to_string();
    let output = run(
        tmp.path(),
        &[
            "path-for",
            "--domain",
            "projects",
            "--topic",
            "daily-brief",
            "--repo",
            &repo_arg,
            "--agent-home",
            &agent_home_arg,
        ],
        &[],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let path = output.stdout_text();
    let path = path.trim();
    assert!(
        path.starts_with(&format!(
            "{}/out/projects/sympoies__nils-cli/",
            agent_home.display()
        )),
        "unexpected path: {path}"
    );
    assert!(
        path.ends_with("-daily-brief"),
        "topic was not preserved in path: {path}"
    );
    assert!(
        !Path::new(path).exists(),
        "--mkdir was not passed, path should not exist"
    );
}

#[test]
fn path_for_accepts_repo_slug_in_repo_flag_and_creates_directory() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let agent_home = tmp.path().join("agent-home");
    let agent_home_arg = agent_home.to_string_lossy().to_string();

    let output = run(
        tmp.path(),
        &[
            "path-for",
            "--domain",
            "tools",
            "--repo",
            "sympoies/nils-cli",
            "--agent-home",
            &agent_home_arg,
            "--mkdir",
            "--format",
            "json",
        ],
        &[],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let value = output.stdout_json();
    assert_eq!(value["schema_version"], "cli.agent-out.path-for.v1");
    assert_eq!(value["command"], "agent-out path-for");
    assert_eq!(value["ok"], true);
    assert_eq!(value["result"]["domain"], "tools");
    assert_eq!(value["result"]["project_slug"], "sympoies__nils-cli");
    assert_eq!(value["result"]["topic"], "tools");
    assert_eq!(value["result"]["created"], true);
    let path = value["result"]["path"].as_str().expect("path");
    assert!(
        Path::new(path).is_dir(),
        "expected --mkdir to create {path}"
    );
}

#[test]
fn audit_separates_allowlisted_roots_from_violations() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let agent_home = tmp.path().join("agent-home");
    let out = agent_home.join("out");
    fs::create_dir_all(out.join("projects")).expect("projects");
    fs::create_dir_all(out.join("plan-issue-delivery")).expect("plan root");
    fs::create_dir_all(out.join("random-scratch")).expect("scratch");
    fs::write(out.join("loose.txt"), "debug").expect("loose file");

    let agent_home_arg = agent_home.to_string_lossy().to_string();
    let output = run(
        tmp.path(),
        &["audit", "--agent-home", &agent_home_arg, "--format", "json"],
        &[],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let value = output.stdout_json();
    assert_eq!(value["schema_version"], "cli.agent-out.audit.v1");
    assert_eq!(value["ok"], true);
    assert_eq!(value["result"]["summary"]["allowed_roots"], 2);
    assert_eq!(value["result"]["summary"]["violations"], 2);
}

#[test]
fn audit_strict_exits_nonzero_with_json_error_details() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let agent_home = tmp.path().join("agent-home");
    let out = agent_home.join("out");
    fs::create_dir_all(out.join("scratch")).expect("scratch");

    let agent_home_arg = agent_home.to_string_lossy().to_string();
    let output = run(
        tmp.path(),
        &[
            "audit",
            "--agent-home",
            &agent_home_arg,
            "--strict",
            "--format",
            "json",
        ],
        &[],
    );

    assert_eq!(output.code, 1, "stderr={}", output.stderr_text());
    let value = output.stdout_json();
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"]["code"], "audit-violations");
    assert_eq!(value["error"]["details"]["summary"]["violations"], 1);
}

#[test]
fn audit_missing_out_root_is_ok() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let agent_home = tmp.path().join("agent-home");
    let agent_home_arg = agent_home.to_string_lossy().to_string();

    let output = run(
        tmp.path(),
        &[
            "audit",
            "--agent-home",
            &agent_home_arg,
            "--strict",
            "--format",
            "json",
        ],
        &[],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let value = output.stdout_json();
    assert_eq!(value["result"]["out_root_exists"], false);
    assert_eq!(value["result"]["summary"]["violations"], 0);
}

#[test]
fn cleanup_plan_classifies_cache_temp_evidence_and_project_artifacts() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let agent_home = tmp.path().join("agent-home");
    let out = agent_home.join("out");
    fs::create_dir_all(out.join("nils-versions/v1.19.2")).expect("cache");
    fs::write(out.join("nils-versions/v1.19.2/agent-out"), "binary").expect("cache file");
    fs::write(out.join("loose.log"), "debug").expect("loose");
    fs::create_dir_all(out.join("scratch")).expect("scratch");
    fs::create_dir_all(out.join("evidence-run")).expect("evidence dir");
    fs::write(out.join("evidence-run/test-first-evidence.json"), "{}").expect("evidence marker");
    fs::create_dir_all(out.join("projects/owner__repo/20260627-report")).expect("project run");
    fs::write(
        out.join("projects/owner__repo/20260627-report/report.md"),
        "report",
    )
    .expect("project artifact");
    fs::create_dir_all(out.join("projects/owner__repo/20260627-evidence"))
        .expect("project evidence run");
    fs::write(
        out.join("projects/owner__repo/20260627-evidence/skill-usage.record.json"),
        "{}",
    )
    .expect("skill marker");

    let agent_home_arg = agent_home.to_string_lossy().to_string();
    let output = run(
        tmp.path(),
        &[
            "cleanup",
            "plan",
            "--agent-home",
            &agent_home_arg,
            "--include-projects",
            "--format",
            "json",
        ],
        &[],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let value = output.stdout_json();
    assert_eq!(value["schema_version"], "cli.agent-out.cleanup.plan.v1");
    assert_eq!(value["command"], "agent-out cleanup plan");
    assert_eq!(value["ok"], true);
    assert_eq!(value["result"]["summary"]["delete"], 3);
    assert_eq!(value["result"]["summary"]["needs_policy"], 1);
    assert!(
        value["result"]["plan_digest"]
            .as_str()
            .expect("plan_digest")
            .starts_with("sha256:")
    );

    let items = value["result"]["items"].as_array().expect("items");
    assert!(items.iter().any(|item| {
        item["name"] == "nils-versions" && item["category"] == "cache" && item["action"] == "delete"
    }));
    assert!(items.iter().any(|item| {
        item["name"] == "evidence-run"
            && item["action"] == "preserve"
            && item["contains_test_first_evidence"] == true
    }));
    assert!(items.iter().any(|item| {
        item["path"]
            .as_str()
            .expect("path")
            .ends_with("projects/owner__repo/20260627-report")
            && item["category"] == "project-artifact"
            && item["action"] == "needs-policy"
    }));
}

#[test]
fn cleanup_plan_preserves_evidence_markers_before_cache_classification() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let agent_home = tmp.path().join("agent-home");
    let out = agent_home.join("out");
    fs::create_dir_all(out.join("nils-versions/v1.19.2")).expect("cache");
    fs::write(
        out.join("nils-versions/v1.19.2/skill-usage.record.json"),
        "{}",
    )
    .expect("skill marker");

    let agent_home_arg = agent_home.to_string_lossy().to_string();
    let output = run(
        tmp.path(),
        &[
            "cleanup",
            "plan",
            "--agent-home",
            &agent_home_arg,
            "--format",
            "json",
        ],
        &[],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let value = output.stdout_json();
    assert_eq!(value["result"]["summary"]["delete"], 0);
    assert_eq!(value["result"]["summary"]["preserve"], 1);
    assert_eq!(value["result"]["summary"]["delete_bytes"], 0);
    let items = value["result"]["items"].as_array().expect("items");
    assert!(items.iter().any(|item| {
        item["name"] == "nils-versions"
            && item["category"] == "evidence-source"
            && item["action"] == "preserve"
            && item["contains_skill_usage"] == true
    }));
}

#[test]
fn cleanup_apply_deletes_confirmed_candidates_and_preserves_evidence() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let agent_home = tmp.path().join("agent-home");
    let out = agent_home.join("out");
    fs::create_dir_all(out.join("nils-versions/v1.19.2")).expect("cache");
    fs::write(out.join("nils-versions/v1.19.2/agent-out"), "binary").expect("cache file");
    fs::write(out.join("loose.log"), "debug").expect("loose");
    fs::create_dir_all(out.join("evidence-run")).expect("evidence dir");
    fs::write(out.join("evidence-run/test-first-evidence.json"), "{}").expect("evidence marker");

    let agent_home_arg = agent_home.to_string_lossy().to_string();
    let plan = run(
        tmp.path(),
        &[
            "cleanup",
            "plan",
            "--agent-home",
            &agent_home_arg,
            "--format",
            "json",
        ],
        &[],
    );
    assert_eq!(plan.code, 0, "stderr={}", plan.stderr_text());
    let plan_value = plan.stdout_json();
    let digest = plan_value["result"]["plan_digest"]
        .as_str()
        .expect("plan_digest")
        .to_string();
    let plan_file = tmp.path().join("cleanup-plan.json");
    fs::write(&plan_file, plan.stdout_text()).expect("plan file");
    let plan_file_arg = plan_file.to_string_lossy().to_string();

    let apply = run(
        tmp.path(),
        &[
            "cleanup",
            "apply",
            "--plan-file",
            &plan_file_arg,
            "--confirm-digest",
            &digest,
            "--agent-home",
            &agent_home_arg,
            "--format",
            "json",
        ],
        &[],
    );

    assert_eq!(apply.code, 0, "stderr={}", apply.stderr_text());
    let value = apply.stdout_json();
    assert_eq!(value["schema_version"], "cli.agent-out.cleanup.apply.v1");
    assert_eq!(value["result"]["summary"]["deleted"], 2);
    assert_eq!(value["result"]["summary"]["skipped"], 0);
    assert!(
        !out.join("nils-versions").exists(),
        "cache should be deleted"
    );
    assert!(
        !out.join("loose.log").exists(),
        "loose file should be deleted"
    );
    assert!(
        out.join("evidence-run/test-first-evidence.json").is_file(),
        "evidence marker must be preserved"
    );
}

#[test]
fn cleanup_apply_skips_path_when_evidence_marker_appears_after_plan() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let agent_home = tmp.path().join("agent-home");
    let out = agent_home.join("out");
    fs::create_dir_all(out.join("scratch")).expect("scratch");
    fs::write(out.join("scratch/debug.log"), "debug").expect("scratch file");

    let agent_home_arg = agent_home.to_string_lossy().to_string();
    let plan = run(
        tmp.path(),
        &[
            "cleanup",
            "plan",
            "--agent-home",
            &agent_home_arg,
            "--format",
            "json",
        ],
        &[],
    );
    assert_eq!(plan.code, 0, "stderr={}", plan.stderr_text());
    let plan_value = plan.stdout_json();
    let digest = plan_value["result"]["plan_digest"]
        .as_str()
        .expect("plan_digest")
        .to_string();
    let plan_file = tmp.path().join("cleanup-plan.json");
    fs::write(&plan_file, plan.stdout_text()).expect("plan file");
    fs::write(out.join("scratch/skill-usage.record.json"), "{}").expect("late marker");
    let plan_file_arg = plan_file.to_string_lossy().to_string();

    let apply = run(
        tmp.path(),
        &[
            "cleanup",
            "apply",
            "--plan-file",
            &plan_file_arg,
            "--confirm-digest",
            &digest,
            "--agent-home",
            &agent_home_arg,
            "--format",
            "json",
        ],
        &[],
    );

    assert_eq!(apply.code, 0, "stderr={}", apply.stderr_text());
    let value = apply.stdout_json();
    assert_eq!(value["result"]["summary"]["deleted"], 0);
    assert_eq!(value["result"]["summary"]["skipped"], 1);
    assert_eq!(
        value["result"]["entries"][0]["reason"],
        "evidence marker appeared after the plan was created"
    );
    assert!(
        out.join("scratch/skill-usage.record.json").is_file(),
        "late evidence marker must be preserved"
    );
}

#[test]
fn cleanup_apply_rejects_parent_component_delete_path_with_matching_digest() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let agent_home = tmp.path().join("agent-home");
    let out = agent_home.join("out");
    fs::create_dir_all(&out).expect("out root");
    fs::write(out.join("loose.log"), "debug").expect("loose");
    fs::write(agent_home.join("victim.log"), "do not delete").expect("victim");

    let agent_home_arg = agent_home.to_string_lossy().to_string();
    let plan = run(
        tmp.path(),
        &[
            "cleanup",
            "plan",
            "--agent-home",
            &agent_home_arg,
            "--format",
            "json",
        ],
        &[],
    );
    assert_eq!(plan.code, 0, "stderr={}", plan.stderr_text());
    let mut envelope = plan.stdout_json();
    envelope["result"]["items"][0]["path"] =
        Value::String(out.join("../victim.log").to_string_lossy().to_string());
    let digest = recompute_cleanup_plan_digest(&mut envelope);
    let plan_file = tmp.path().join("cleanup-plan.json");
    fs::write(
        &plan_file,
        serde_json::to_string_pretty(&envelope).expect("plan json"),
    )
    .expect("plan file");
    let plan_file_arg = plan_file.to_string_lossy().to_string();

    let apply = run(
        tmp.path(),
        &[
            "cleanup",
            "apply",
            "--plan-file",
            &plan_file_arg,
            "--confirm-digest",
            &digest,
            "--agent-home",
            &agent_home_arg,
            "--format",
            "json",
        ],
        &[],
    );

    assert_eq!(apply.code, 65, "stderr={}", apply.stderr_text());
    let value = apply.stdout_json();
    assert_eq!(value["error"]["code"], "cleanup-path-outside-out-root");
    assert!(
        agent_home.join("victim.log").is_file(),
        "crafted parent-component path must not delete outside out"
    );
    assert!(
        out.join("loose.log").is_file(),
        "apply must stop before delete"
    );
}

#[cfg(unix)]
#[test]
fn cleanup_apply_rejects_intermediate_symlink_delete_path_outside_out_root() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let agent_home = tmp.path().join("agent-home");
    let out = agent_home.join("out");
    let outside = tmp.path().join("outside");
    fs::create_dir_all(&out).expect("out root");
    fs::create_dir_all(&outside).expect("outside");
    fs::write(out.join("loose.log"), "debug").expect("loose");
    fs::write(outside.join("secret.log"), "do not delete").expect("outside file");
    std::os::unix::fs::symlink(&outside, out.join("linkdir")).expect("symlink");

    let agent_home_arg = agent_home.to_string_lossy().to_string();
    let plan = run(
        tmp.path(),
        &[
            "cleanup",
            "plan",
            "--agent-home",
            &agent_home_arg,
            "--format",
            "json",
        ],
        &[],
    );
    assert_eq!(plan.code, 0, "stderr={}", plan.stderr_text());
    let mut envelope = plan.stdout_json();
    envelope["result"]["items"][0]["path"] =
        Value::String(out.join("linkdir/secret.log").to_string_lossy().to_string());
    let digest = recompute_cleanup_plan_digest(&mut envelope);
    let plan_file = tmp.path().join("cleanup-plan.json");
    fs::write(
        &plan_file,
        serde_json::to_string_pretty(&envelope).expect("plan json"),
    )
    .expect("plan file");
    let plan_file_arg = plan_file.to_string_lossy().to_string();

    let apply = run(
        tmp.path(),
        &[
            "cleanup",
            "apply",
            "--plan-file",
            &plan_file_arg,
            "--confirm-digest",
            &digest,
            "--agent-home",
            &agent_home_arg,
            "--format",
            "json",
        ],
        &[],
    );

    assert_eq!(apply.code, 65, "stderr={}", apply.stderr_text());
    let value = apply.stdout_json();
    assert_eq!(value["error"]["code"], "cleanup-path-outside-out-root");
    assert!(
        outside.join("secret.log").is_file(),
        "intermediate symlink must not delete outside out"
    );
}

#[test]
fn cleanup_apply_rejects_nested_preserved_root_descendant_with_matching_digest() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let agent_home = tmp.path().join("agent-home");
    let out = agent_home.join("out");
    fs::create_dir_all(out.join("projects/owner__repo/20260627-report")).expect("project run");
    fs::write(
        out.join("projects/owner__repo/20260627-report/report.md"),
        "report",
    )
    .expect("report");
    fs::write(out.join("loose.log"), "debug").expect("loose");

    let agent_home_arg = agent_home.to_string_lossy().to_string();
    let plan = run(
        tmp.path(),
        &[
            "cleanup",
            "plan",
            "--agent-home",
            &agent_home_arg,
            "--format",
            "json",
        ],
        &[],
    );
    assert_eq!(plan.code, 0, "stderr={}", plan.stderr_text());
    let mut envelope = plan.stdout_json();
    let plan_out = Path::new(
        envelope["result"]["out_root"]
            .as_str()
            .expect("plan out_root"),
    );
    envelope["result"]["items"][0]["path"] = Value::String(
        plan_out
            .join("projects/owner__repo/20260627-report/report.md")
            .to_string_lossy()
            .to_string(),
    );
    envelope["result"]["items"][0]["category"] =
        Value::String("top-level-noncanonical".to_string());
    let digest = recompute_cleanup_plan_digest(&mut envelope);
    let plan_file = tmp.path().join("cleanup-plan.json");
    fs::write(
        &plan_file,
        serde_json::to_string_pretty(&envelope).expect("plan json"),
    )
    .expect("plan file");
    let plan_file_arg = plan_file.to_string_lossy().to_string();

    let apply = run(
        tmp.path(),
        &[
            "cleanup",
            "apply",
            "--plan-file",
            &plan_file_arg,
            "--confirm-digest",
            &digest,
            "--agent-home",
            &agent_home_arg,
            "--format",
            "json",
        ],
        &[],
    );

    assert_eq!(apply.code, 65, "stderr={}", apply.stderr_text());
    let value = apply.stdout_json();
    assert_eq!(value["error"]["code"], "cleanup-delete-shape-invalid");
    assert!(
        out.join("projects/owner__repo/20260627-report/report.md")
            .is_file(),
        "crafted nested project path must not be deleted"
    );
}

#[test]
fn cleanup_apply_rejects_digest_mismatch() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let agent_home = tmp.path().join("agent-home");
    let out = agent_home.join("out");
    fs::create_dir_all(&out).expect("out root");
    fs::write(out.join("loose.log"), "debug").expect("loose");

    let agent_home_arg = agent_home.to_string_lossy().to_string();
    let plan = run(
        tmp.path(),
        &[
            "cleanup",
            "plan",
            "--agent-home",
            &agent_home_arg,
            "--format",
            "json",
        ],
        &[],
    );
    assert_eq!(plan.code, 0, "stderr={}", plan.stderr_text());
    let plan_file = tmp.path().join("cleanup-plan.json");
    fs::write(&plan_file, plan.stdout_text()).expect("plan file");
    let plan_file_arg = plan_file.to_string_lossy().to_string();

    let apply = run(
        tmp.path(),
        &[
            "cleanup",
            "apply",
            "--plan-file",
            &plan_file_arg,
            "--confirm-digest",
            "sha256:not-the-plan",
            "--agent-home",
            &agent_home_arg,
            "--format",
            "json",
        ],
        &[],
    );

    assert_eq!(apply.code, 65, "stderr={}", apply.stderr_text());
    let value = apply.stdout_json();
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"]["code"], "cleanup-digest-mismatch");
    assert!(out.join("loose.log").is_file(), "apply must not delete");
}

#[test]
fn completion_export_succeeds_outside_git_repo() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let output = run(tmp.path(), &["completion", "zsh"], &[]);

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    assert!(
        output.stdout_text().contains("#compdef agent-out"),
        "missing completion header: {}",
        output.stdout_text()
    );
}
