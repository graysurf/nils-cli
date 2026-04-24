use crate::common;
use common::{GitCliHarness, git, init_bare_remote, init_repo};
use nils_test_support::git::commit_file;
use std::fs;
use std::path::Path;
use tempfile::TempDir;

fn add_remote(repo: &Path, name: &str, remote: &TempDir) {
    let remote_path = remote.path().to_string_lossy().to_string();
    git(repo, &["remote", "add", name, &remote_path]);
}

fn push_main(repo: &Path, remote: &str) {
    git(repo, &["push", "-u", remote, "main"]);
}

fn git_path(repo: &Path, path: &str) -> std::path::PathBuf {
    let raw = git(repo, &["rev-parse", "--git-path", path]);
    let candidate = std::path::PathBuf::from(raw.trim_end_matches(['\n', '\r']));
    if candidate.is_absolute() {
        candidate
    } else {
        repo.join(candidate)
    }
}

#[test]
fn ci_pick_usage_missing_args() {
    let harness = GitCliHarness::new();
    let dir = init_repo();

    let output = harness.run(dir.path(), &["ci", "pick"]);

    assert_eq!(output.code, 2);
    assert_eq!(output.stdout_text(), "");
    assert_eq!(
        output.stderr_text(),
        "❌ Usage: git-pick <target> <commit-or-range> <name>\n   Try: git-pick --help\n"
    );
}

#[test]
fn ci_pick_help_shows_usage() {
    let harness = GitCliHarness::new();
    let dir = tempfile::TempDir::new().expect("tempdir");

    let output = harness.run(dir.path(), &["ci", "pick", "--help"]);

    assert_eq!(output.code, 0);
    assert!(
        output
            .stdout_text()
            .contains("git-pick: create and push a CI branch")
    );
    assert!(
        output
            .stdout_text()
            .contains("Usage:\n  git-pick <target> <commit-or-range> <name>\n")
    );
    assert_eq!(output.stderr_text(), "");
}

#[test]
fn ci_pick_usage_unknown_flag() {
    let harness = GitCliHarness::new();
    let dir = init_repo();

    let output = harness.run(dir.path(), &["ci", "pick", "--bogus"]);

    assert_eq!(output.code, 2);
    assert_eq!(output.stdout_text(), "");
    assert_eq!(
        output.stderr_text(),
        "❌ Usage: git-pick <target> <commit-or-range> <name>\n   Try: git-pick --help\n"
    );
}

#[test]
fn ci_pick_reports_not_in_repo() {
    let harness = GitCliHarness::new();
    let dir = tempfile::TempDir::new().expect("tempdir");

    let output = harness.run(dir.path(), &["ci", "pick", "main", "HEAD", "test"]);

    assert_eq!(output.code, 1);
    assert_eq!(output.stdout_text(), "");
    assert_eq!(output.stderr_text(), "❌ Not inside a Git repository.\n");
}

#[test]
fn ci_pick_refuses_in_progress_op() {
    let harness = GitCliHarness::new();
    let dir = init_repo();

    let cherry_pick = git_path(dir.path(), "CHERRY_PICK_HEAD");
    fs::write(&cherry_pick, "conflict").expect("write cherry pick head");

    let output = harness.run(dir.path(), &["ci", "pick", "main", "HEAD", "test"]);

    assert_eq!(output.code, 1);
    assert_eq!(output.stdout_text(), "");
    assert_eq!(
        output.stderr_text(),
        "❌ Refusing to run during an in-progress Git operation:\n   - cherry-pick in progress\n"
    );
}

#[test]
fn ci_pick_errors_without_remotes() {
    let harness = GitCliHarness::new();
    let dir = init_repo();

    let output = harness.run(dir.path(), &["ci", "pick", "main", "HEAD", "test"]);

    assert_eq!(output.code, 1);
    assert_eq!(output.stdout_text(), "");
    assert_eq!(
        output.stderr_text(),
        "❌ No git remotes found (need a remote to push CI branches).\n"
    );
}

#[test]
fn ci_pick_reports_invalid_target_ref() {
    let harness = GitCliHarness::new();
    let dir = init_repo();
    let remote = init_bare_remote();

    add_remote(dir.path(), "origin", &remote);

    let output = harness.run(
        dir.path(),
        &["ci", "pick", "origin/missing", "HEAD", "test", "--no-fetch"],
    );

    assert_eq!(output.code, 1);
    assert_eq!(output.stdout_text(), "");
    assert_eq!(
        output.stderr_text(),
        "❌ Cannot resolve target ref: origin/missing\n"
    );
}

#[test]
fn ci_pick_refuses_remote_target_with_mismatched_remote() {
    let harness = GitCliHarness::new();
    let dir = init_repo();
    let origin = init_bare_remote();
    let upstream = init_bare_remote();

    add_remote(dir.path(), "origin", &origin);
    add_remote(dir.path(), "upstream", &upstream);
    push_main(dir.path(), "origin");

    let output = harness.run(
        dir.path(),
        &[
            "ci",
            "pick",
            "origin/main",
            "HEAD",
            "test",
            "--remote",
            "upstream",
        ],
    );

    assert_eq!(output.code, 2);
    assert_eq!(output.stdout_text(), "");
    assert!(output.stderr_text().contains(
        "❌ Target ref looks like 'origin/main' (remote 'origin') but --remote is 'upstream'."
    ));
}

#[test]
fn ci_pick_invalid_branch_name_errors() {
    let harness = GitCliHarness::new();
    let dir = init_repo();
    let origin = init_bare_remote();

    add_remote(dir.path(), "origin", &origin);
    push_main(dir.path(), "origin");

    let output = harness.run(dir.path(), &["ci", "pick", "main", "HEAD", "bad^name"]);

    assert_eq!(output.code, 2);
    assert_eq!(output.stdout_text(), "");
    assert_eq!(
        output.stderr_text(),
        "❌ Invalid CI branch name: ci/main/bad^name\n"
    );
}

#[test]
fn ci_pick_rejects_invalid_ci_branch_name() {
    let harness = GitCliHarness::new();
    let dir = init_repo();
    let remote = init_bare_remote();

    add_remote(dir.path(), "origin", &remote);

    let output = harness.run(
        dir.path(),
        &["ci", "pick", "main", "HEAD", "bad name", "--no-fetch"],
    );

    assert_eq!(output.code, 2);
    assert_eq!(output.stdout_text(), "");
    assert_eq!(
        output.stderr_text(),
        "❌ Invalid CI branch name: ci/main/bad name\n"
    );
}

#[test]
fn ci_pick_reports_invalid_base_ref() {
    let harness = GitCliHarness::new();
    let dir = init_repo();
    let remote = init_bare_remote();

    add_remote(dir.path(), "origin", &remote);

    let output = harness.run(
        dir.path(),
        &["ci", "pick", "missing-branch", "HEAD", "test", "--no-fetch"],
    );

    assert_eq!(output.code, 1);
    assert_eq!(output.stdout_text(), "");
    assert_eq!(
        output.stderr_text(),
        "❌ Cannot resolve target ref: missing-branch\n"
    );
}

#[test]
fn ci_pick_reports_empty_commit_range() {
    let harness = GitCliHarness::new();
    let dir = init_repo();
    let remote = init_bare_remote();

    add_remote(dir.path(), "origin", &remote);
    push_main(dir.path(), "origin");

    let output = harness.run(
        dir.path(),
        &["ci", "pick", "main", "HEAD..HEAD", "empty", "--no-fetch"],
    );

    assert_eq!(output.code, 1);
    assert_eq!(output.stdout_text(), "");
    assert_eq!(
        output.stderr_text(),
        "❌ No commits resolved from range: HEAD..HEAD\n"
    );
}

#[test]
fn ci_pick_refuses_unstaged_changes() {
    let harness = GitCliHarness::new();
    let dir = init_repo();

    fs::write(dir.path().join("README.md"), "dirty\n").expect("write change");

    let output = harness.run(dir.path(), &["ci", "pick", "main", "HEAD", "test"]);

    assert_eq!(output.code, 1);
    assert_eq!(output.stdout_text(), "");
    assert_eq!(
        output.stderr_text(),
        "❌ Unstaged changes detected. Commit or stash before running git-pick.\n"
    );
}

#[test]
fn ci_pick_refuses_staged_changes() {
    let harness = GitCliHarness::new();
    let dir = init_repo();

    fs::write(dir.path().join("README.md"), "staged\n").expect("write change");
    git(dir.path(), &["add", "README.md"]);

    let output = harness.run(dir.path(), &["ci", "pick", "main", "HEAD", "test"]);

    assert_eq!(output.code, 1);
    assert_eq!(output.stdout_text(), "");
    assert_eq!(
        output.stderr_text(),
        "❌ Staged changes detected. Commit or stash before running git-pick.\n"
    );
}

#[test]
fn ci_pick_no_fetch_path_uses_local_refs() {
    let harness = GitCliHarness::new();
    let dir = init_repo();
    let remote = init_bare_remote();

    add_remote(dir.path(), "origin", &remote);
    push_main(dir.path(), "origin");

    git(dir.path(), &["switch", "-q", "-c", "feature"]);
    let commit_sha = commit_file(dir.path(), "feature.txt", "feature\n", "feature");
    git(dir.path(), &["switch", "-q", "main"]);

    let output = harness.run(
        dir.path(),
        &["ci", "pick", "main", &commit_sha, "no-fetch", "--no-fetch"],
    );

    assert_eq!(output.code, 0);
    assert!(
        output
            .stdout_text()
            .contains("🌿 CI branch: ci/main/no-fetch\n")
    );
    assert!(output.stdout_text().contains("🔧 Base     : main\n"));
}

#[test]
fn ci_pick_creates_and_pushes_branch() {
    let harness = GitCliHarness::new();
    let dir = init_repo();
    let remote = init_bare_remote();

    add_remote(dir.path(), "origin", &remote);
    push_main(dir.path(), "origin");

    git(dir.path(), &["switch", "-q", "-c", "feature"]);
    let commit_sha = commit_file(dir.path(), "feature.txt", "feature\n", "feature");
    git(dir.path(), &["switch", "-q", "main"]);

    let output = harness.run(
        dir.path(),
        &["ci", "pick", "origin/main", &commit_sha, "try"],
    );

    assert_eq!(output.code, 0);
    assert!(output.stdout_text().contains("🌿 CI branch: ci/main/try\n"));
    assert!(output.stdout_text().contains("🔧 Base     : origin/main\n"));
    assert!(
        output
            .stdout_text()
            .contains(&format!("🍒 Pick     : {commit_sha} (1 commit(s))\n"))
    );
    assert!(
        output
            .stdout_text()
            .contains("✅ Pushed: origin/ci/main/try (CI should run on branch push)\n")
    );

    let remote_refs = git(remote.path(), &["show-ref", "--heads", "ci/main/try"]);
    assert!(remote_refs.contains("ci/main/try"));

    let head = git(dir.path(), &["rev-parse", "--abbrev-ref", "HEAD"]);
    assert_eq!(head.trim(), "main");
}

#[test]
fn ci_pick_refuses_existing_remote_branch_without_force() {
    let harness = GitCliHarness::new();
    let dir = init_repo();
    let remote = init_bare_remote();

    add_remote(dir.path(), "origin", &remote);
    push_main(dir.path(), "origin");

    git(dir.path(), &["switch", "-q", "-c", "feature"]);
    let commit_sha = commit_file(dir.path(), "feature.txt", "feature\n", "feature");
    git(dir.path(), &["switch", "-q", "main"]);

    git(
        dir.path(),
        &["switch", "-q", "-c", "ci/main/existing", "main"],
    );
    git(dir.path(), &["push", "-u", "origin", "ci/main/existing"]);
    git(dir.path(), &["switch", "-q", "main"]);
    git(dir.path(), &["branch", "-D", "ci/main/existing"]);

    let output = harness.run(
        dir.path(),
        &["ci", "pick", "main", &commit_sha, "existing", "--no-fetch"],
    );

    assert_eq!(output.code, 1);
    assert_eq!(output.stdout_text(), "");
    assert_eq!(
        output.stderr_text(),
        "❌ Remote branch already exists: origin/ci/main/existing\n   Use --force to reset/rebuild it.\n"
    );
}

#[test]
fn ci_pick_stays_on_ci_branch_when_requested() {
    let harness = GitCliHarness::new();
    let dir = init_repo();
    let remote = init_bare_remote();

    add_remote(dir.path(), "origin", &remote);
    push_main(dir.path(), "origin");

    git(dir.path(), &["switch", "-q", "-c", "feature"]);
    let commit_sha = commit_file(dir.path(), "feature.txt", "feature\n", "feature");
    git(dir.path(), &["switch", "-q", "main"]);

    let output = harness.run(
        dir.path(),
        &[
            "ci",
            "pick",
            "main",
            &commit_sha,
            "stay",
            "--stay",
            "--no-fetch",
        ],
    );

    assert_eq!(output.code, 0);
    let head = git(dir.path(), &["rev-parse", "--abbrev-ref", "HEAD"]);
    assert_eq!(head.trim(), "ci/main/stay");
}

#[test]
fn ci_pick_cleanup_switch_back_missing_original_branch() {
    let harness = GitCliHarness::new();
    let dir = init_repo();
    let remote = init_bare_remote();

    add_remote(dir.path(), "origin", &remote);
    push_main(dir.path(), "origin");

    git(dir.path(), &["switch", "-q", "-c", "feature"]);
    let commit_sha = commit_file(dir.path(), "feature.txt", "feature\n", "feature");
    git(dir.path(), &["switch", "-q", "main"]);

    git(dir.path(), &["read-tree", "--empty"]);
    for entry in fs::read_dir(dir.path()).expect("read repo root") {
        let entry = entry.expect("dir entry");
        if entry.file_name() == ".git" {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            fs::remove_dir_all(&path).expect("remove repo dir");
        } else {
            fs::remove_file(&path).expect("remove repo file");
        }
    }

    let head_path = git_path(dir.path(), "HEAD");
    fs::write(&head_path, "ref: refs/heads/missing\n").expect("write HEAD");

    let output = harness.run(
        dir.path(),
        &["ci", "pick", "main", &commit_sha, "missing", "--no-fetch"],
    );

    assert_eq!(output.code, 0);
    let head = git(dir.path(), &["rev-parse", "--abbrev-ref", "HEAD"]);
    assert_eq!(head.trim(), "ci/main/missing");
}

#[test]
fn ci_pick_reports_cherry_pick_failure() {
    let harness = GitCliHarness::new();
    let dir = init_repo();
    let remote = init_bare_remote();

    add_remote(dir.path(), "origin", &remote);
    push_main(dir.path(), "origin");

    git(dir.path(), &["switch", "-q", "-c", "feature"]);
    let commit_sha = commit_file(dir.path(), "conflict.txt", "feature\n", "feature");
    git(dir.path(), &["switch", "-q", "main"]);
    commit_file(dir.path(), "conflict.txt", "main\n", "main");

    let output = harness.run(dir.path(), &["ci", "pick", "main", &commit_sha, "conflict"]);

    assert_eq!(output.code, 1);
    assert!(
        output
            .stderr_text()
            .contains("❌ Cherry-pick failed on branch: ci/main/conflict\n")
    );
    assert!(
        output
            .stderr_text()
            .contains("🧠 Resolve conflicts then run: git cherry-pick --continue\n")
    );
    assert!(
        output
            .stderr_text()
            .contains("    Or abort and retry:        git cherry-pick --abort\n")
    );
}
