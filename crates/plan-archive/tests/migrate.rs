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
