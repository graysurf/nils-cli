//! Integration coverage for `plan-archive migrate` dry-run.
//!
//! Builds a throwaway git repo + archive clone + `hosts.yaml`, then
//! drives `migrate::prepare` to assert the dry-run report.
//!
//! Apply-path coverage is intentionally minimal: it exercises the
//! semantic-commit-driven commit pipeline, which is gated on the
//! released `semantic-commit` binary being present in `$PATH`. The
//! shipped tests cover the pure prepare/dispatch logic; the apply
//! path is exercised through the runtime-smoke fixtures in the
//! agent-runtime-kit Plan 3 work.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use nils_common::cli_contract::OutputFormat;
use plan_archive::migrate::{self, DispatchArgs, MetadataPayload};
use plan_archive::validate::hosts::HostClass;

struct Scenario {
    _tmp: tempfile::TempDir,
    source_repo: PathBuf,
    archive: PathBuf,
    hosts: PathBuf,
    plan_path: PathBuf,
}

#[derive(Debug)]
struct CmdOutput {
    code: i32,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

impl CmdOutput {
    fn stdout_text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).to_string()
    }

    fn stderr_text(&self) -> String {
        String::from_utf8_lossy(&self.stderr).to_string()
    }
}

fn plan_archive_bin() -> PathBuf {
    nils_test_support::bin::resolve("plan-archive")
}

fn run_plan_archive_in(dir: &Path, args: &[String], envs: &[(&str, String)]) -> CmdOutput {
    let mut command = Command::new(plan_archive_bin());
    command.args(args).current_dir(dir);
    for (key, value) in envs {
        command.env(key, value);
    }
    let output = command.output().expect("plan-archive command");
    CmdOutput {
        code: output.status.code().unwrap_or(1),
        stdout: output.stdout,
        stderr: output.stderr,
    }
}

fn git(repo: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(repo)
        .env("GIT_AUTHOR_NAME", "tester")
        .env("GIT_AUTHOR_EMAIL", "tester@example.com")
        .env("GIT_COMMITTER_NAME", "tester")
        .env("GIT_COMMITTER_EMAIL", "tester@example.com")
        .output()
        .unwrap_or_else(|e| panic!("spawn git {args:?}: {e}"));
    assert!(
        out.status.success(),
        "git {args:?} failed:\nstderr={}\nstdout={}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout),
    );
}

fn git_stdout(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap_or_else(|e| panic!("spawn git {args:?}: {e}"));
    assert!(
        out.status.success(),
        "git {args:?} failed:\nstderr={}\nstdout={}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout),
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn build_scenario() -> Scenario {
    let tmp = tempfile::tempdir().unwrap();
    let source_repo = tmp.path().join("source");
    let archive = tmp.path().join("archive");
    fs::create_dir_all(&source_repo).unwrap();
    fs::create_dir_all(&archive).unwrap();

    // Source repo with a plan folder that contains two files.
    git(&source_repo, &["init", "-q", "-b", "main"]);
    git(
        &source_repo,
        &[
            "remote",
            "add",
            "origin",
            "git@github.com:graysurf/agent-runtime-kit.git",
        ],
    );
    let plan = source_repo.join("docs/plans/2026-05-27-demo-plan");
    fs::create_dir_all(&plan).unwrap();
    fs::write(plan.join("PLAN.md"), "# demo plan\n").unwrap();
    fs::write(plan.join("notes.md"), "notes\n").unwrap();
    git(&source_repo, &["add", "docs/plans"]);
    git(&source_repo, &["commit", "-q", "-m", "seed plan"]);

    // Archive clone with config/hosts.yaml.
    git(&archive, &["init", "-q", "-b", "main"]);
    let config_dir = archive.join("config");
    fs::create_dir_all(&config_dir).unwrap();
    let hosts = config_dir.join("hosts.yaml");
    fs::write(
        &hosts,
        "version: 1\nhosts:\n  github.com:\n    class: personal\n    primary_identity: graysurf\n",
    )
    .unwrap();
    git(&archive, &["add", "config/hosts.yaml"]);
    git(&archive, &["commit", "-q", "-m", "seed hosts"]);

    Scenario {
        _tmp: tmp,
        source_repo,
        archive,
        hosts,
        plan_path: PathBuf::from("docs/plans/2026-05-27-demo-plan"),
    }
}

fn arg_path(path: &Path) -> String {
    path.display().to_string()
}

fn cli_migrate_args(scenario: &Scenario, apply: bool) -> Vec<String> {
    let mut args = vec![
        "migrate".to_string(),
        "--plan".to_string(),
        arg_path(&scenario.plan_path),
        "--source-repo".to_string(),
        arg_path(&scenario.source_repo),
        "--archive".to_string(),
        arg_path(&scenario.archive),
        "--hosts".to_string(),
        arg_path(&scenario.hosts),
        "--issue".to_string(),
        "https://github.com/sympoies/nils-cli/issues/571".to_string(),
    ];
    if apply {
        args.push("--apply".to_string());
    }
    args
}

fn configure_archive_push_remote(scenario: &Scenario) {
    let remote = scenario
        .archive
        .parent()
        .expect("scenario root")
        .join("archive-remote.git");
    let out = Command::new("git")
        .args(["init", "--bare", "-q"])
        .arg(&remote)
        .output()
        .expect("git init --bare");
    assert!(
        out.status.success(),
        "git init --bare failed:\nstderr={}\nstdout={}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout),
    );
    let remote_arg = arg_path(&remote);
    git(&scenario.archive, &["remote", "add", "origin", &remote_arg]);
    git(&scenario.archive, &["push", "-u", "origin", "main"]);
}

#[cfg(unix)]
fn install_semantic_commit_stub(scenario: &Scenario) -> PathBuf {
    let bin_dir = scenario
        .archive
        .parent()
        .expect("scenario root")
        .join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let stub = bin_dir.join("semantic-commit");
    fs::write(
        &stub,
        r#"#!/bin/sh
repo=
msg=
while [ "$#" -gt 0 ]; do
  case "$1" in
    --repo)
      repo="$2"
      shift 2
      ;;
    -m)
      msg="$2"
      shift 2
      ;;
    *)
      shift
      ;;
  esac
done
if [ -z "$repo" ] || [ -z "$msg" ]; then
  echo "missing repo or message" >&2
  exit 2
fi
git -C "$repo" -c user.name=tester -c user.email=tester@example.com -c commit.gpgsign=false commit -q -m "$msg"
"#,
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(&stub).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&stub, perms).unwrap();
    bin_dir
}

#[cfg(unix)]
fn path_with_prepend(dir: &Path) -> String {
    let current = std::env::var("PATH").unwrap_or_default();
    format!("{}:{current}", dir.display())
}

fn args_for(scenario: &Scenario) -> DispatchArgs {
    DispatchArgs {
        plan: scenario.plan_path.clone(),
        source_repo: Some(scenario.source_repo.clone()),
        archive: Some(scenario.archive.clone()),
        hosts: Some(scenario.hosts.clone()),
        issue: Some("https://github.com/sympoies/nils-cli/issues/571".to_string()),
        pr: None,
        mr: None,
        apply: false,
        format: OutputFormat::Json,
    }
}

#[test]
fn dry_run_resolves_identity_target_and_files() {
    let scenario = build_scenario();
    let args = args_for(&scenario);
    let report = migrate::prepare(&args).expect("dry-run prepare ok");

    assert_eq!(report.plan_folder, "2026-05-27-demo-plan");
    assert_eq!(report.source.host, "github.com");
    assert_eq!(report.source.org_or_group_path, "graysurf");
    assert_eq!(report.source.repo, "agent-runtime-kit");
    assert_eq!(report.source.branch, "main");
    assert_eq!(report.source.commit.len(), 40);
    assert!(matches!(report.classification.class, HostClass::Personal));
    assert_eq!(
        report.classification.primary_identity.as_deref(),
        Some("graysurf")
    );
    assert_eq!(
        report.archive_target.relative_path,
        "plans/github.com/graysurf/agent-runtime-kit/2026-05-27-demo-plan"
    );
    assert!(!report.archive_target.exists);

    let mut files = report.files_to_copy.clone();
    files.sort();
    assert_eq!(
        files,
        vec![
            "docs/plans/2026-05-27-demo-plan/PLAN.md".to_string(),
            "docs/plans/2026-05-27-demo-plan/notes.md".to_string(),
        ]
    );

    assert_serialized_metadata(&report.metadata);
}

#[test]
fn cli_dry_run_renders_plan_summary_without_mutating() {
    let scenario = build_scenario();
    let output = run_plan_archive_in(
        &scenario.source_repo,
        &cli_migrate_args(&scenario, false),
        &[],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let stdout = output.stdout_text();
    assert!(stdout.contains("plan-archive migrate (dry-run)"));
    assert!(stdout.contains("archive exists?   : no"));
    assert!(stdout.contains("classification    : personal"));
    assert!(stdout.contains("files to copy     : 2"));
    assert!(stdout.contains("issue : https://github.com/sympoies/nils-cli/issues/571"));
    assert!(stdout.contains("(no files modified; pass --apply to commit)"));
    assert!(scenario.source_repo.join(&scenario.plan_path).exists());
    assert_eq!(output.stderr_text(), "");
}

#[test]
#[cfg(unix)]
fn cli_apply_copies_plan_writes_metadata_pushes_archive_and_deletes_source() {
    let scenario = build_scenario();
    configure_archive_push_remote(&scenario);
    let stub_dir = install_semantic_commit_stub(&scenario);

    let output = run_plan_archive_in(
        &scenario.source_repo,
        &cli_migrate_args(&scenario, true),
        &[("PATH", path_with_prepend(&stub_dir))],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let stdout = output.stdout_text();
    assert!(stdout.contains("plan-archive migrate (applied)"));
    assert!(stdout.contains("files copied           : 2"));

    let target = scenario
        .archive
        .join("plans/github.com/graysurf/agent-runtime-kit/2026-05-27-demo-plan");
    assert!(target.join("PLAN.md").exists());
    assert!(target.join("notes.md").exists());
    let metadata = fs::read_to_string(target.join("metadata.yaml")).unwrap();
    assert!(metadata.contains("captured_classification:"));
    assert!(metadata.contains("issue: https://github.com/sympoies/nils-cli/issues/571"));
    assert!(scenario.archive.join("catalog.json").exists());
    assert!(!scenario.source_repo.join(&scenario.plan_path).exists());

    assert_eq!(
        git_stdout(&scenario.source_repo, &["status", "--porcelain"]),
        ""
    );
    assert_eq!(
        git_stdout(&scenario.archive, &["status", "--porcelain"]),
        ""
    );
    assert!(
        git_stdout(
            &scenario.archive,
            &["ls-remote", "--heads", "origin", "main"]
        )
        .contains("refs/heads/main")
    );
}

#[test]
#[cfg(unix)]
fn cli_apply_reconciles_execution_state_header_to_terminal() {
    let scenario = build_scenario();

    // Seed a mid-flight execution-state doc into the plan folder and commit it
    // so the apply path's clean-tree check passes.
    let exec_state = scenario
        .source_repo
        .join(&scenario.plan_path)
        .join("2026-05-27-demo-plan-execution-state.md");
    fs::write(
        &exec_state,
        "<!-- execute-from-tracking-issue:state:v1 -->\n\
# Demo Execution State\n\
\n\
## Execution State\n\
\n\
- Status: implementation complete — all tasks done; repo PR\n  \
delivery pending\n\
- Current task: delivering the repo PR\n\
- Next task: close-ready handoff\n\
- Last updated: 2026-05-30\n\
\n\
## Task Ledger\n\
\n\
- Task 1.1 | done | seed\n",
    )
    .unwrap();
    git(&scenario.source_repo, &["add", "docs/plans"]);
    git(
        &scenario.source_repo,
        &["commit", "-q", "-m", "seed execution state"],
    );

    configure_archive_push_remote(&scenario);
    let stub_dir = install_semantic_commit_stub(&scenario);

    let output = run_plan_archive_in(
        &scenario.source_repo,
        &cli_migrate_args(&scenario, true),
        &[("PATH", path_with_prepend(&stub_dir))],
    );
    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());

    let archived = scenario
        .archive
        .join("plans/github.com/graysurf/agent-runtime-kit/2026-05-27-demo-plan")
        .join("2026-05-27-demo-plan-execution-state.md");
    let body = fs::read_to_string(&archived).expect("archived execution-state doc");

    // Terminal header, deferring to the issue ref migrate carried.
    assert!(body.contains(
        "- Status: archived — plan bundle migrated to agent-plan-archive; \
final state tracked in https://github.com/sympoies/nils-cli/issues/571"
    ));
    assert!(body.contains("- Current task: none — archived"));
    assert!(body.contains("- Next task: none — archived"));
    // The mid-flight wording and its wrapped continuation are gone.
    assert!(!body.contains("delivery pending"));
    assert!(!body.contains("implementation complete"));
    // Everything outside the section is preserved verbatim.
    assert!(body.contains("- Last updated: 2026-05-30"));
    assert!(body.contains("## Task Ledger"));
    assert!(body.contains("- Task 1.1 | done | seed"));
    // Sibling plan files are still copied verbatim.
    let target_dir = scenario
        .archive
        .join("plans/github.com/graysurf/agent-runtime-kit/2026-05-27-demo-plan");
    assert!(target_dir.join("PLAN.md").exists());
    assert!(target_dir.join("notes.md").exists());
}

fn assert_serialized_metadata(m: &MetadataPayload) {
    assert_eq!(m.version, 1);
    assert_eq!(m.source.host, "github.com");
    assert_eq!(m.source.org_or_group_path, "graysurf");
    assert_eq!(m.source.repo, "agent-runtime-kit");
    assert_eq!(m.source.branch, "main");
    assert_eq!(m.source.original_path, "docs/plans/2026-05-27-demo-plan/");
    assert!(matches!(
        m.captured_classification.class,
        HostClass::Personal
    ));
    assert_eq!(
        m.refs.issue.as_deref(),
        Some("https://github.com/sympoies/nils-cli/issues/571")
    );
    assert!(m.refs.pr.is_none() && m.refs.mr.is_none());

    let yaml = serde_yaml_ng::to_string(m).unwrap();
    assert!(yaml.contains("version: 1"));
    assert!(yaml.contains("captured_classification:"));
    assert!(yaml.contains("class: personal"));
}

#[test]
fn dry_run_rejects_missing_plan_folder() {
    let scenario = build_scenario();
    let mut args = args_for(&scenario);
    args.plan = PathBuf::from("docs/plans/does-not-exist");
    let err = migrate::prepare(&args).unwrap_err();
    assert_eq!(err.code(), "migrate-plan-folder-missing");
}

#[test]
fn dry_run_rejects_unknown_host() {
    let scenario = build_scenario();
    // Rewrite hosts.yaml to remove github.com so the resolver can't
    // classify the source.
    fs::write(
        &scenario.hosts,
        "version: 1\nhosts:\n  gitlab.com:\n    class: personal\n",
    )
    .unwrap();
    let args = args_for(&scenario);
    let err = migrate::prepare(&args).unwrap_err();
    assert_eq!(err.code(), "migrate-unknown-host");
}

#[test]
fn dry_run_requires_at_least_one_ref() {
    let scenario = build_scenario();
    let mut args = args_for(&scenario);
    args.issue = None;
    args.pr = None;
    args.mr = None;
    let err = migrate::prepare(&args).unwrap_err();
    assert_eq!(err.code(), "migrate-no-refs-supplied");
}

#[test]
fn dry_run_flags_archive_target_collision() {
    let scenario = build_scenario();
    let target = scenario
        .archive
        .join("plans/github.com/graysurf/agent-runtime-kit/2026-05-27-demo-plan");
    fs::create_dir_all(&target).unwrap();
    let args = args_for(&scenario);
    let report = migrate::prepare(&args).expect("dry-run still completes");
    assert!(report.archive_target.exists);
}
