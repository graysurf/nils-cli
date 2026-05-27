//! Integration coverage for `plan-archive discover` (read-only).
//!
//! Builds a throwaway source repo + archive clone + `hosts.yaml`, lays
//! down plan folders that exercise every classification path, and
//! drives `discover::scan` to assert the candidate model. Mirrors the
//! fixture style of `tests/migrate.rs` and reuses the shared
//! `archive_target_path` derivation so discover and migrate cannot
//! drift on archive targets.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use nils_common::cli_contract::OutputFormat;
use plan_archive::discover::{
    self, DiscoverCandidate, DiscoverReport, DiscoverStatus, DispatchArgs,
};
use plan_archive::migrate::archive_target_path;
use plan_archive::refresh::refparse::RefKind;

struct Scenario {
    _tmp: tempfile::TempDir,
    source_repo: PathBuf,
    archive: PathBuf,
    hosts: PathBuf,
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

/// Source repo (github.com/graysurf/agent-runtime-kit) + archive clone
/// with a `github.com: personal` hosts entry. No plan folders yet.
fn build_base() -> Scenario {
    build_with_host("github.com")
}

fn build_with_host(host: &str) -> Scenario {
    let tmp = tempfile::tempdir().unwrap();
    let source_repo = tmp.path().join("source");
    let archive = tmp.path().join("archive");
    fs::create_dir_all(&source_repo).unwrap();
    fs::create_dir_all(&archive).unwrap();

    git(&source_repo, &["init", "-q", "-b", "main"]);
    git(&source_repo, &["config", "commit.gpgsign", "false"]);
    git(
        &source_repo,
        &[
            "remote",
            "add",
            "origin",
            "git@github.com:graysurf/agent-runtime-kit.git",
        ],
    );
    // An initial commit so HEAD/branch resolve like a real repo.
    fs::write(source_repo.join("README.md"), "# source\n").unwrap();
    git(&source_repo, &["add", "README.md"]);
    git(&source_repo, &["commit", "-q", "-m", "init"]);

    git(&archive, &["init", "-q", "-b", "main"]);
    git(&archive, &["config", "commit.gpgsign", "false"]);
    let config_dir = archive.join("config");
    fs::create_dir_all(&config_dir).unwrap();
    let hosts = config_dir.join("hosts.yaml");
    fs::write(
        &hosts,
        format!(
            "version: 1\nhosts:\n  {host}:\n    class: personal\n    primary_identity: graysurf\n"
        ),
    )
    .unwrap();
    git(&archive, &["add", "config/hosts.yaml"]);
    git(&archive, &["commit", "-q", "-m", "seed hosts"]);

    Scenario {
        _tmp: tmp,
        source_repo,
        archive,
        hosts,
    }
}

/// Write a plan folder with the given top-level files (not committed).
fn write_plan(s: &Scenario, folder: &str, files: &[(&str, &str)]) {
    let dir = s.source_repo.join("docs/plans").join(folder);
    fs::create_dir_all(&dir).unwrap();
    for (name, body) in files {
        fs::write(dir.join(name), body).unwrap();
    }
}

fn commit_all(s: &Scenario, msg: &str) {
    git(&s.source_repo, &["add", "-A"]);
    git(&s.source_repo, &["commit", "-q", "-m", msg]);
}

fn args(s: &Scenario, include_unknown: bool) -> DispatchArgs {
    DispatchArgs {
        source_repo: Some(s.source_repo.clone()),
        plans_root: None,
        archive: Some(s.archive.clone()),
        hosts: Some(s.hosts.clone()),
        include_unknown,
        format: OutputFormat::Json,
    }
}

fn find<'a>(r: &'a DiscoverReport, folder: &str) -> &'a DiscoverCandidate {
    r.candidates
        .iter()
        .find(|c| c.plan_folder == folder)
        .unwrap_or_else(|| panic!("no candidate `{folder}` in {:?}", folder_names(r)))
}

fn folder_names(r: &DiscoverReport) -> Vec<String> {
    r.candidates.iter().map(|c| c.plan_folder.clone()).collect()
}

fn reason_codes(c: &DiscoverCandidate) -> Vec<&str> {
    c.reasons.iter().map(|r| r.code.as_str()).collect()
}

const ISSUE_URL: &str = "https://github.com/graysurf/agent-runtime-kit/issues/10";
const CROSSREPO_PR: &str = "https://github.com/sympoies/nils-cli/pull/42";

/// Build the full classification matrix in one source repo.
fn build_matrix() -> Scenario {
    let s = build_base();
    write_plan(
        &s,
        "2026-05-01-eligible",
        &[
            ("plan.md", &format!("# plan\n\nTracking: {ISSUE_URL}\n")),
            (
                "state.md",
                "# state\n\n- Status: complete; all sprints delivered\n",
            ),
        ],
    );
    write_plan(
        &s,
        "2026-05-02-target-exists",
        &[
            ("plan.md", &format!("Tracking: {ISSUE_URL}\n")),
            ("state.md", "- Status: done\n"),
        ],
    );
    write_plan(
        &s,
        "2026-05-03-no-refs",
        &[("state.md", "- Status: complete; shipped\n")],
    );
    write_plan(
        &s,
        "2026-05-04-dirty",
        &[
            ("plan.md", &format!("Tracking: {ISSUE_URL}\n")),
            ("state.md", "- Status: done\n"),
        ],
    );
    write_plan(
        &s,
        "2026-05-05-uncertain",
        &[
            ("plan.md", &format!("Tracking: {ISSUE_URL}\n")),
            ("state.md", "- Status: in progress\n"),
        ],
    );
    write_plan(
        &s,
        "2026-05-06-sprint-active",
        &[
            ("plan.md", &format!("Tracking: {ISSUE_URL}\n")),
            (
                "state.md",
                "- Status: Sprint 2 Task 2.4 complete; Sprint 2 closed; Sprint 3 active\n",
            ),
        ],
    );
    write_plan(
        &s,
        "2026-05-07-crossrepo",
        &[
            ("plan.md", &format!("Implementation PR: {CROSSREPO_PR}\n")),
            ("state.md", "## Closeout\n\nAll lanes merged.\n"),
        ],
    );
    commit_all(&s, "seed plan matrix");

    // Pre-create the archive target for the collision case.
    let collision = s
        .archive
        .join("plans/github.com/graysurf/agent-runtime-kit/2026-05-02-target-exists");
    fs::create_dir_all(&collision).unwrap();

    // Make the dirty folder dirty with an untracked file.
    fs::write(
        s.source_repo.join("docs/plans/2026-05-04-dirty/scratch.md"),
        "wip\n",
    )
    .unwrap();

    s
}

#[test]
fn classification_matrix() {
    let s = build_matrix();
    let report = discover::scan(&args(&s, true)).expect("scan ok");

    assert_eq!(report.source.host, "github.com");
    assert_eq!(report.source.repo, "agent-runtime-kit");
    assert!(report.host_known);
    assert_eq!(report.plans_root, "docs/plans");

    assert_eq!(report.summary.scanned, 7);
    assert_eq!(report.summary.eligible, 2);
    assert_eq!(report.summary.blocked, 3);
    assert_eq!(report.summary.unknown, 2);

    // eligible: refs + closeout + free target + clean + known host
    let elig = find(&report, "2026-05-01-eligible");
    assert_eq!(elig.status, DiscoverStatus::Eligible);
    assert!(elig.reasons.is_empty());
    assert_eq!(elig.refs.len(), 1);
    assert_eq!(elig.refs[0].url, ISSUE_URL);
    assert_eq!(elig.refs[0].kind, RefKind::Issue);
    assert!(elig.refs[0].matches_source_repo);
    assert!(!elig.archive_target.exists);
    assert!(elig.closeout_evidence.is_some());
    assert!(!elig.dirty);
    let cmd = elig
        .suggested_migrate_command
        .as_deref()
        .expect("eligible has a suggested command");
    assert!(cmd.contains("--plan docs/plans/2026-05-01-eligible"));
    assert!(cmd.contains(&format!("--issue {ISSUE_URL}")));
    assert!(cmd.contains("--format json"));
    assert!(!cmd.contains("migrate --plan docs/plans/2026-05-01-eligible/")); // no trailing slash

    // blocked: archive target collision
    let coll = find(&report, "2026-05-02-target-exists");
    assert_eq!(coll.status, DiscoverStatus::Blocked);
    assert!(coll.archive_target.exists);
    assert!(reason_codes(coll).contains(&"archive-target-exists"));
    assert!(coll.suggested_migrate_command.is_none());

    // blocked: no provider refs
    let norefs = find(&report, "2026-05-03-no-refs");
    assert_eq!(norefs.status, DiscoverStatus::Blocked);
    assert!(norefs.refs.is_empty());
    assert!(reason_codes(norefs).contains(&"no-provider-refs"));

    // blocked: dirty plan folder
    let dirty = find(&report, "2026-05-04-dirty");
    assert_eq!(dirty.status, DiscoverStatus::Blocked);
    assert!(dirty.dirty);
    assert!(reason_codes(dirty).contains(&"source-plan-folder-dirty"));

    // unknown: refs present but no closeout evidence
    let uncertain = find(&report, "2026-05-05-uncertain");
    assert_eq!(uncertain.status, DiscoverStatus::Unknown);
    assert!(reason_codes(uncertain).contains(&"closeout-evidence-uncertain"));
    assert!(uncertain.closeout_evidence.is_none());

    // unknown: sprint-level "complete" alongside an active sprint must
    // NOT be promoted to eligible.
    let sprint = find(&report, "2026-05-06-sprint-active");
    assert_eq!(sprint.status, DiscoverStatus::Unknown);
    assert!(sprint.closeout_evidence.is_none());

    // eligible via a Closeout heading; the only ref is cross-repo.
    let cross = find(&report, "2026-05-07-crossrepo");
    assert_eq!(cross.status, DiscoverStatus::Eligible);
    assert_eq!(cross.refs.len(), 1);
    assert_eq!(cross.refs[0].kind, RefKind::Pull);
    assert!(!cross.refs[0].matches_source_repo);
    let ccmd = cross.suggested_migrate_command.as_deref().unwrap();
    assert!(ccmd.contains(&format!("--pr {CROSSREPO_PR}")));
    assert!(!ccmd.contains("--issue"));
}

#[test]
fn include_unknown_gates_listing_but_not_counts() {
    let s = build_matrix();

    let hidden = discover::scan(&args(&s, false)).expect("scan ok");
    assert_eq!(hidden.summary.scanned, 7);
    assert_eq!(hidden.summary.unknown, 2, "count still reports unknowns");
    assert!(!hidden.summary.included_unknown);
    assert_eq!(hidden.candidates.len(), 5, "unknowns omitted from listing");
    assert!(
        !hidden
            .candidates
            .iter()
            .any(|c| c.status == DiscoverStatus::Unknown)
    );

    let shown = discover::scan(&args(&s, true)).expect("scan ok");
    assert!(shown.summary.included_unknown);
    assert_eq!(shown.candidates.len(), 7);
}

#[test]
fn shared_archive_target_derivation_matches_migrate_helper() {
    let s = build_matrix();
    let report = discover::scan(&args(&s, true)).expect("scan ok");
    let elig = find(&report, "2026-05-01-eligible");
    let expected = archive_target_path(
        "github.com",
        "graysurf",
        "agent-runtime-kit",
        "2026-05-01-eligible",
    );
    assert_eq!(
        elig.archive_target.relative_path,
        expected.to_string_lossy()
    );
}

#[test]
fn discovery_never_mutates_source_or_archive() {
    let s = build_matrix();

    // Capture archive listing before scanning.
    let before = archive_listing(&s.archive);
    let report = discover::scan(&args(&s, true)).expect("scan ok");
    let after = archive_listing(&s.archive);
    assert_eq!(before, after, "discover must not write to the archive");

    // The eligible folder's archive target must not have been created.
    let elig_target = s
        .archive
        .join("plans/github.com/graysurf/agent-runtime-kit/2026-05-01-eligible");
    assert!(
        !elig_target.exists(),
        "discover created an archive target it should only have previewed"
    );

    // The only working-tree change in source is the intentionally-dirty
    // folder's untracked file — scan added nothing.
    let status = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(&s.source_repo)
        .output()
        .unwrap();
    let dirty_lines: Vec<String> = String::from_utf8_lossy(&status.stdout)
        .lines()
        .map(|l| l.to_string())
        .collect();
    assert_eq!(
        dirty_lines.len(),
        1,
        "unexpected source changes: {dirty_lines:?}"
    );
    assert!(dirty_lines[0].contains("2026-05-04-dirty/scratch.md"));

    // sanity: the scan still classified something.
    assert_eq!(report.summary.scanned, 7);
}

fn archive_listing(archive: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        if let Ok(entries) = fs::read_dir(dir) {
            for e in entries.flatten() {
                let p = e.path();
                if p.file_name().and_then(|n| n.to_str()) == Some(".git") {
                    continue;
                }
                if p.is_dir() {
                    walk(&p, out);
                } else {
                    out.push(p);
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(archive, &mut out);
    out.sort();
    out
}

#[test]
fn unknown_host_blocks_every_folder() {
    let s = build_with_host("gitlab.com"); // github.com absent
    write_plan(
        &s,
        "2026-05-01-eligible-shape",
        &[
            ("plan.md", &format!("Tracking: {ISSUE_URL}\n")),
            ("state.md", "- Status: done\n"),
        ],
    );
    commit_all(&s, "seed");

    let report = discover::scan(&args(&s, true)).expect("scan ok");
    assert!(!report.host_known);
    assert_eq!(report.summary.scanned, 1);
    assert_eq!(report.summary.blocked, 1);
    let c = find(&report, "2026-05-01-eligible-shape");
    assert_eq!(c.status, DiscoverStatus::Blocked);
    assert!(reason_codes(c).contains(&"unknown-host"));
}

#[test]
fn empty_plans_root_yields_zero_candidates() {
    let s = build_base(); // no docs/plans created
    let report = discover::scan(&args(&s, true)).expect("scan ok");
    assert_eq!(report.summary.scanned, 0);
    assert!(report.candidates.is_empty());
}

#[test]
fn plans_root_outside_repo_is_rejected() {
    let s = build_base();
    let mut a = args(&s, true);
    a.plans_root = Some(PathBuf::from("/definitely/not/under/the/source/repo"));
    let err = discover::scan(&a).unwrap_err();
    assert_eq!(err.code(), "discover-plans-root-outside-repo");
}

#[test]
fn duplicate_refs_are_deduplicated_across_files() {
    let s = build_base();
    write_plan(
        &s,
        "2026-05-01-dupes",
        &[
            (
                "plan.md",
                &format!("see {ISSUE_URL} and again {ISSUE_URL}\n"),
            ),
            ("state.md", &format!("- Status: done\n\nref {ISSUE_URL}\n")),
        ],
    );
    commit_all(&s, "seed");
    let report = discover::scan(&args(&s, true)).expect("scan ok");
    let c = find(&report, "2026-05-01-dupes");
    assert_eq!(c.refs.len(), 1, "same URL inferred once");
    assert_eq!(c.status, DiscoverStatus::Eligible);
}
