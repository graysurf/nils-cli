use nils_test_support::git;
use pretty_assertions::assert_eq;
use std::fs;

#[test]
fn init_repo_with_default_branch_and_config() {
    let repo = git::init_repo_with(git::InitRepoOptions::default());
    let branch = git::git(repo.path(), &["symbolic-ref", "--short", "HEAD"]);
    assert_eq!(branch.trim_end(), "main");

    let email = git::git(repo.path(), &["config", "user.email"]);
    assert_eq!(email.trim_end(), "test@example.com");
}

#[test]
fn init_repo_main_sets_main_branch() {
    let repo = git::init_repo_main();
    let branch = git::git(repo.path(), &["symbolic-ref", "--short", "HEAD"]);
    assert_eq!(branch.trim_end(), "main");
}

#[test]
fn init_repo_main_with_initial_commit_sets_main_and_commits() {
    let repo = git::init_repo_main_with_initial_commit();
    let branch = git::git(repo.path(), &["symbolic-ref", "--short", "HEAD"]);
    assert_eq!(branch.trim_end(), "main");

    let head = git::git(repo.path(), &["rev-parse", "HEAD"]);
    assert_eq!(head.trim().len(), 40);
}

#[test]
fn init_repo_with_initial_commit_creates_commit() {
    let repo = git::init_repo_with(git::InitRepoOptions::new().with_initial_commit());
    let head = git::git(repo.path(), &["rev-parse", "HEAD"]);
    let head = head.trim();
    assert_eq!(head.len(), 40);
}

#[test]
fn init_repo_at_with_initializes_existing_directory() {
    let workspace = tempfile::TempDir::new().expect("tempdir");
    let repo = workspace.path().join("repo");
    fs::create_dir_all(&repo).expect("create repo dir");

    git::init_repo_at_with(&repo, git::InitRepoOptions::new().with_initial_commit());

    let head = git::git(&repo, &["rev-parse", "HEAD"]);
    assert_eq!(head.trim().len(), 40);
}

#[test]
fn worktree_add_branch_creates_linked_worktree() {
    let repo = git::init_repo_with(git::InitRepoOptions::new().with_initial_commit());
    let workspace = tempfile::TempDir::new().expect("tempdir");
    let linked = workspace.path().join("linked");

    git::worktree_add_branch(repo.path(), &linked, "linked-worktree");

    let branch = git::git(&linked, &["symbolic-ref", "--short", "HEAD"]);
    assert_eq!(branch.trim_end(), "linked-worktree");
}

#[test]
fn commit_file_creates_commit_and_returns_hash() {
    let repo = git::init_repo_with(git::InitRepoOptions::default());
    let hash = git::commit_file(repo.path(), "hello.txt", "hello", "hello");
    let head = git::git(repo.path(), &["rev-parse", "HEAD"]);
    assert_eq!(hash, head.trim());
}

#[test]
fn git_with_env_applies_env_vars() {
    let repo = git::init_repo_with(git::InitRepoOptions::default());
    let ident = git::git_with_env(
        repo.path(),
        &["var", "GIT_AUTHOR_IDENT"],
        &[
            ("GIT_AUTHOR_NAME", "Env Name"),
            ("GIT_AUTHOR_EMAIL", "env@example.com"),
        ],
    );
    assert!(ident.contains("Env Name"));
    assert!(ident.contains("env@example.com"));
}

#[test]
fn git_output_returns_status() {
    let repo = git::init_repo_with(git::InitRepoOptions::default());
    let output = git::git_output(repo.path(), &["status", "--porcelain"]);
    assert_eq!(output.status.success(), true);
}

#[test]
fn repo_id_matches_directory_name() {
    let repo = git::init_repo_with(git::InitRepoOptions::default());
    let expected = repo
        .path()
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("");
    assert_eq!(git::repo_id(repo.path()), expected.to_string());
}
