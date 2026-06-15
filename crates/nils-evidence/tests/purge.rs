//! Integration coverage for `evidence purge` dry-run and apply.
//!
//! Builds an archive clone populated with rollups under two hosts (one employer,
//! one personal), then drives `purge::run`. The apply path shells out to
//! `semantic-commit` and `git push`; both are stubbed (a local-commit stub on
//! PATH; the archive is given a bare push remote).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use evidence::purge::{self, PurgeArgs, PurgeError};
use evidence::validate::hosts::HostClass;
use nils_common::cli_contract::OutputFormat;

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
        "git {args:?} failed:\nstderr={}",
        String::from_utf8_lossy(&out.stderr),
    );
}

fn write_rollup(archive: &Path, host: &str, org: &str, repo: &str, id: &str) {
    let dir = archive
        .join("evidence")
        .join(host)
        .join(org)
        .join(repo)
        .join(id);
    fs::create_dir_all(&dir).unwrap();
    let body = format!(
        r#"{{"schema":"skill-usage.rollup.v1","id":"{id}","archived_at":"2026-06-14T10:00:00Z","skill":"deliver-pr","intent":"x","trigger":"user_explicit","repo":{{"host":"{host}","org":"{org}","repo":"{repo}"}},"cwd":"~/x","started_at":"2026-06-14T10:00:00Z","outcome":{{"status":"pass","summary":"done"}},"producer":{{"tool":"skill-usage"}},"counts":{{"validation":0,"failures":0}},"linked_evidence":[],"source_digest":"sha256:{id}"}}"#
    );
    fs::write(dir.join("skill-usage.rollup.json"), body).unwrap();
}

struct Archive {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    archive: PathBuf,
}

/// Archive with a multi-host config (one employer, one personal) and three
/// rollups: two under the employer host, one under the personal host.
fn build() -> Archive {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let archive = root.join("archive");
    fs::create_dir_all(archive.join("config")).unwrap();
    fs::create_dir_all(archive.join("evidence")).unwrap();
    fs::write(
        archive.join("config").join("hosts.yaml"),
        "version: 1\nhosts:\n  gitlab.gamania.com:\n    class: employer\n    employer: Gamania\n  github.com:\n    class: personal\n    primary_identity: graysurf\n",
    )
    .unwrap();
    write_rollup(
        &archive,
        "gitlab.gamania.com",
        "gim",
        "svc",
        "20260601T000000Z-a",
    );
    write_rollup(
        &archive,
        "gitlab.gamania.com",
        "gim",
        "svc",
        "20260602T000000Z-b",
    );
    write_rollup(
        &archive,
        "github.com",
        "graysurf",
        "kit",
        "20260603T000000Z-c",
    );
    git(&archive, &["init", "-q", "-b", "main"]);
    git(&archive, &["add", "-A"]);
    git(
        &archive,
        &[
            "-c",
            "user.name=tester",
            "-c",
            "user.email=tester@example.com",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "-m",
            "seed",
        ],
    );
    Archive {
        _tmp: tmp,
        root,
        archive,
    }
}

fn args(archive: &Path, host: Vec<String>, class: Option<HostClass>, apply: bool) -> PurgeArgs {
    PurgeArgs {
        archive: Some(archive.to_path_buf()),
        hosts: None,
        host,
        class,
        apply,
        format: OutputFormat::Json,
    }
}

fn git_count_commits(repo: &Path) -> usize {
    let out = Command::new("git")
        .args(["rev-list", "--count", "HEAD"])
        .current_dir(repo)
        .output()
        .expect("git rev-list");
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .unwrap_or(0)
}

fn configure_push_remote(a: &Archive) {
    let remote = a.root.join("archive-remote.git");
    let out = Command::new("git")
        .args(["init", "--bare", "-q"])
        .arg(&remote)
        .output()
        .expect("git init --bare");
    assert!(out.status.success());
    git(
        &a.archive,
        &["remote", "add", "origin", &remote.to_string_lossy()],
    );
    git(&a.archive, &["push", "-u", "origin", "main"]);
}

#[cfg(unix)]
fn install_semantic_commit_stub(a: &Archive) -> PathBuf {
    let bin_dir = a.root.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let stub = bin_dir.join("semantic-commit");
    fs::write(
        &stub,
        r#"#!/bin/sh
repo=
msg=
while [ "$#" -gt 0 ]; do
  case "$1" in
    --repo) repo="$2"; shift 2 ;;
    -m) msg="$2"; shift 2 ;;
    *) shift ;;
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

/// Run `f` with `dir` prepended to PATH. Serialized via a process-global mutex
/// because PATH is process-wide.
#[cfg(unix)]
fn with_path<T>(dir: &Path, f: impl FnOnce() -> T) -> T {
    use std::sync::Mutex;
    static LOCK: Mutex<()> = Mutex::new(());
    let _guard = LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let original = std::env::var("PATH").unwrap_or_default();
    let new = format!("{}:{original}", dir.display());
    // SAFETY: serialized under LOCK; restored before returning.
    unsafe { std::env::set_var("PATH", &new) };
    let result = f();
    unsafe { std::env::set_var("PATH", original) };
    result
}

#[test]
fn purge_requires_a_scope() {
    // Safety: never an implicit whole-archive purge.
    let a = build();
    let err = purge::run(&args(&a.archive, vec![], None, false)).unwrap_err();
    assert!(matches!(err, PurgeError::NoScope));
    // Nothing was touched.
    assert!(a.archive.join("evidence/gitlab.gamania.com").exists());
    assert!(a.archive.join("evidence/github.com").exists());
}

#[test]
fn purge_dry_run_by_class_scopes_employer_hosts_only() {
    let a = build();
    let report = purge::run(&args(&a.archive, vec![], Some(HostClass::Employer), false)).unwrap();
    assert_eq!(report.scope_hosts, vec!["gitlab.gamania.com".to_string()]);
    assert_eq!(
        report.total_records, 2,
        "two employer-host records in scope"
    );
    assert!(!report.applied);
    assert!(report.archive_commit.is_none());
    // Dry-run deletes nothing.
    assert!(a.archive.join("evidence/gitlab.gamania.com").exists());
    assert!(a.archive.join("evidence/github.com").exists());
}

#[cfg(unix)]
#[test]
fn purge_apply_by_host_deletes_only_that_host_and_commits() {
    let a = build();
    configure_push_remote(&a);
    let stub = install_semantic_commit_stub(&a);
    let before = git_count_commits(&a.archive);

    let report = with_path(&stub, || {
        purge::run(&args(
            &a.archive,
            vec!["gitlab.gamania.com".to_string()],
            None,
            true,
        ))
        .expect("apply must succeed")
    });

    assert!(report.applied);
    assert_eq!(report.total_records, 2);
    assert!(report.archive_commit.is_some());

    // Only the named host tree is gone; the other host is untouched.
    assert!(
        !a.archive.join("evidence/gitlab.gamania.com").exists(),
        "purged host tree removed"
    );
    assert!(
        a.archive.join("evidence/github.com").exists(),
        "out-of-scope host left intact"
    );

    // Exactly one purge commit.
    assert_eq!(git_count_commits(&a.archive) - before, 1);

    // Catalog regenerated to only the surviving record.
    let cat = fs::read_to_string(a.archive.join("catalog.json")).unwrap();
    assert!(cat.contains("github.com"), "survivor is catalogued");
    assert!(
        !cat.contains("gitlab.gamania.com"),
        "purged host no longer catalogued"
    );
}

fn seed_commit(archive: &Path, msg: &str) {
    git(archive, &["add", "-A"]);
    git(
        archive,
        &[
            "-c",
            "user.name=tester",
            "-c",
            "user.email=tester@example.com",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "-m",
            msg,
        ],
    );
}

#[test]
fn purge_rejects_path_like_host() {
    // A `--host` value that is not a plain host label could escape
    // `archive/evidence` once joined and `remove_dir_all`-ed. It must be
    // refused before any path join or deletion.
    let a = build();
    for bad in ["../escape", "/etc", "..", ".", "a/b", ""] {
        let err = purge::run(&args(&a.archive, vec![bad.to_string()], None, true)).unwrap_err();
        assert!(
            matches!(err, PurgeError::UnsafeHost(_)),
            "host {bad:?} must be rejected as unsafe, got {err:?}"
        );
    }
    // Real host trees are untouched.
    assert!(a.archive.join("evidence/gitlab.gamania.com").exists());
    assert!(a.archive.join("evidence/github.com").exists());
}

#[cfg(unix)]
#[test]
fn purge_apply_refuses_foreign_staged_changes() {
    // The dirty guard only covers evidence/ + catalog.json, but semantic-commit
    // commits the whole index. A pre-staged change elsewhere in the archive must
    // be refused so purge never folds it into the purge commit.
    let a = build();
    configure_push_remote(&a);
    let stub = install_semantic_commit_stub(&a);
    let before = git_count_commits(&a.archive);

    let hosts_yaml = a.archive.join("config").join("hosts.yaml");
    let edited = fs::read_to_string(&hosts_yaml).unwrap() + "\n# pre-staged unrelated edit\n";
    fs::write(&hosts_yaml, edited).unwrap();
    git(&a.archive, &["add", "config/hosts.yaml"]);

    let err = with_path(&stub, || {
        purge::run(&args(
            &a.archive,
            vec!["gitlab.gamania.com".to_string()],
            None,
            true,
        ))
        .unwrap_err()
    });
    assert!(
        matches!(err, PurgeError::StagedArchiveChanges),
        "foreign staged change must be refused, got {err:?}"
    );

    // Nothing deleted, nothing committed.
    assert!(a.archive.join("evidence/gitlab.gamania.com").exists());
    assert_eq!(git_count_commits(&a.archive), before);
}

#[cfg(unix)]
#[test]
fn purge_apply_deletes_host_tree_without_rollups() {
    // A scoped host tree that holds only legacy/orphaned files (no
    // skill-usage.rollup.json) must still be removed; the no-op decision is
    // based on whether scoped host roots exist, not on the rollup count.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let archive = root.join("archive");
    fs::create_dir_all(archive.join("config")).unwrap();
    fs::create_dir_all(archive.join("evidence")).unwrap();
    fs::write(
        archive.join("config").join("hosts.yaml"),
        "version: 1\nhosts:\n  gitlab.gamania.com:\n    class: employer\n    employer: Gamania\n",
    )
    .unwrap();
    let legacy = archive.join("evidence/gitlab.gamania.com/orphan");
    fs::create_dir_all(&legacy).unwrap();
    fs::write(legacy.join("secret.txt"), "sensitive").unwrap();
    git(&archive, &["init", "-q", "-b", "main"]);
    let a = Archive {
        _tmp: tmp,
        root,
        archive,
    };
    seed_commit(&a.archive, "seed");
    configure_push_remote(&a);
    let stub = install_semantic_commit_stub(&a);
    let before = git_count_commits(&a.archive);

    let report = with_path(&stub, || {
        purge::run(&args(&a.archive, vec![], Some(HostClass::Employer), true)).expect("apply")
    });

    assert!(report.applied);
    assert_eq!(report.total_records, 0, "no rollups discovered");
    assert!(
        report.archive_commit.is_some(),
        "legacy-only host tree deletion is still committed"
    );
    assert!(
        !a.archive.join("evidence/gitlab.gamania.com").exists(),
        "legacy host tree without rollups is removed"
    );
    assert_eq!(git_count_commits(&a.archive) - before, 1);
}

#[cfg(unix)]
#[test]
fn purge_apply_rolls_back_on_catalog_failure() {
    // If catalog regeneration fails on a malformed surviving (out-of-scope)
    // rollup after the scoped host tree is deleted, the deletion must be rolled
    // back so apply never leaves uncommitted destructive state.
    let a = build();
    configure_push_remote(&a);
    let stub = install_semantic_commit_stub(&a);

    let survivor = a
        .archive
        .join("evidence/github.com/graysurf/kit/20260603T000000Z-c/skill-usage.rollup.json");
    fs::write(&survivor, "{ not valid json").unwrap();
    seed_commit(&a.archive, "corrupt surviving rollup");
    let before = git_count_commits(&a.archive);

    let err = with_path(&stub, || {
        purge::run(&args(
            &a.archive,
            vec!["gitlab.gamania.com".to_string()],
            None,
            true,
        ))
        .unwrap_err()
    });
    assert!(
        matches!(err, PurgeError::Catalog(_)),
        "catalog failure must surface, got {err:?}"
    );

    // Rolled back: scoped host tree restored, no purge commit.
    assert!(
        a.archive.join("evidence/gitlab.gamania.com").exists(),
        "scoped host tree deletion rolled back after catalog failure"
    );
    assert_eq!(
        git_count_commits(&a.archive),
        before,
        "no purge commit on catalog failure"
    );
}
