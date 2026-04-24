use nils_common::process as shared_process;
use nils_test_support::bin;
use nils_test_support::cmd::{self, CmdOptions, CmdOutput};
use nils_test_support::fs as test_fs;
use nils_test_support::git as test_git;
use pretty_assertions::assert_eq;
use std::fs;
use std::path::{Path, PathBuf};

fn gemini_cli_bin() -> PathBuf {
    bin::resolve("gemini-cli")
}

fn run(repo: &Path, args: &[&str], path_env: &str, stdin: Option<&str>) -> CmdOutput {
    let mut options = CmdOptions::default()
        .with_cwd(repo)
        .with_env("PATH", path_env);
    if let Some(input) = stdin {
        options = options.with_stdin_str(input);
    }
    cmd::run_with(&gemini_cli_bin(), args, &options)
}

fn git(repo: &Path, args: &[&str]) {
    test_git::git(repo, args);
}

fn git_stdout(repo: &Path, args: &[&str]) -> String {
    test_git::git(repo, args).trim().to_string()
}

fn init_repo(repo: &Path) {
    test_git::init_repo_at_with(repo, test_git::InitRepoOptions::new().without_branch());
}

fn real_git_path() -> String {
    shared_process::find_in_path("git")
        .map(|path| path.to_string_lossy().to_string())
        .unwrap_or_else(|| panic!("git not found in PATH for tests"))
}

fn write_git_proxy(dir: &Path) {
    let git = real_git_path();
    let proxy = dir.join("git");
    let script = format!("#!/bin/sh\nexec \"{git}\" \"$@\"\n");
    test_fs::write_executable(&proxy, &script);
}

#[test]
fn agent_commit_fallback_commits_with_prompted_header() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let repo = dir.path().join("repo");
    fs::create_dir_all(&repo).expect("repo dir");
    init_repo(&repo);

    fs::create_dir_all(repo.join("src")).expect("src dir");
    fs::write(repo.join("src/lib.rs"), "pub fn hello() {}\n").expect("write file");
    git(&repo, &["add", "src/lib.rs"]);

    let bin_dir = dir.path().join("bin");
    fs::create_dir_all(&bin_dir).expect("bin dir");
    write_git_proxy(&bin_dir);
    let path_env = bin_dir.display().to_string();

    let output = run(
        &repo,
        &["agent", "commit"],
        &path_env,
        Some("feat\ncore\nAdd fallback commit\ny\n"),
    );
    assert_eq!(output.code, 0);
    assert!(String::from_utf8_lossy(&output.stderr).contains("fallback mode"));

    let subject = git_stdout(&repo, &["log", "-1", "--pretty=%s"]);
    assert_eq!(subject, "feat(core): Add fallback commit");
}

#[test]
fn agent_commit_fallback_defaults_type_and_scope() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let repo = dir.path().join("repo");
    fs::create_dir_all(&repo).expect("repo dir");
    init_repo(&repo);

    fs::create_dir_all(repo.join("src")).expect("src dir");
    fs::write(repo.join("src/main.rs"), "fn main() {}\n").expect("write file");
    git(&repo, &["add", "src/main.rs"]);

    let bin_dir = dir.path().join("bin");
    fs::create_dir_all(&bin_dir).expect("bin dir");
    write_git_proxy(&bin_dir);
    let path_env = bin_dir.display().to_string();

    let output = run(
        &repo,
        &["agent", "commit"],
        &path_env,
        Some("\n\nUse defaults\ny\n"),
    );
    assert_eq!(output.code, 0);

    let subject = git_stdout(&repo, &["log", "-1", "--pretty=%s"]);
    assert_eq!(subject, "chore(src): Use defaults");
}

#[test]
fn agent_commit_fallback_aborts_on_confirmation_reject() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let repo = dir.path().join("repo");
    fs::create_dir_all(&repo).expect("repo dir");
    init_repo(&repo);

    fs::write(repo.join("README.md"), "hello\n").expect("write file");
    git(&repo, &["add", "README.md"]);

    let bin_dir = dir.path().join("bin");
    fs::create_dir_all(&bin_dir).expect("bin dir");
    write_git_proxy(&bin_dir);
    let path_env = bin_dir.display().to_string();

    let output = run(
        &repo,
        &["agent", "commit"],
        &path_env,
        Some("fix\nrepo\nAbort this\nn\n"),
    );
    assert_eq!(output.code, 1);
    assert!(String::from_utf8_lossy(&output.stderr).contains("Aborted."));
}

#[test]
fn agent_commit_fallback_push_flag_returns_1_when_push_fails() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let repo = dir.path().join("repo");
    fs::create_dir_all(&repo).expect("repo dir");
    init_repo(&repo);

    fs::write(repo.join("README.md"), "hello\n").expect("write file");
    git(&repo, &["add", "README.md"]);

    let bin_dir = dir.path().join("bin");
    fs::create_dir_all(&bin_dir).expect("bin dir");
    write_git_proxy(&bin_dir);
    let path_env = bin_dir.display().to_string();

    let output = run(
        &repo,
        &["agent", "commit", "--push"],
        &path_env,
        Some("fix\nrepo\nCommit then fail push\ny\n"),
    );
    assert_eq!(output.code, 1);

    // Commit still succeeded before push failed.
    let subject = git_stdout(&repo, &["log", "-1", "--pretty=%s"]);
    assert_eq!(subject, "fix(repo): Commit then fail push");
}

#[test]
fn agent_commit_without_staged_changes_returns_1() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let repo = dir.path().join("repo");
    fs::create_dir_all(&repo).expect("repo dir");
    init_repo(&repo);

    let bin_dir = dir.path().join("bin");
    fs::create_dir_all(&bin_dir).expect("bin dir");
    write_git_proxy(&bin_dir);
    let path_env = bin_dir.display().to_string();

    let output = run(&repo, &["agent", "commit"], &path_env, None);
    assert_eq!(output.code, 1);
    assert!(String::from_utf8_lossy(&output.stderr).contains("no staged changes"));
}

#[test]
fn agent_commit_auto_stage_in_fallback_stages_and_commits() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let repo = dir.path().join("repo");
    fs::create_dir_all(&repo).expect("repo dir");
    init_repo(&repo);

    fs::write(repo.join("README.md"), "hello\n").expect("write file");

    let bin_dir = dir.path().join("bin");
    fs::create_dir_all(&bin_dir).expect("bin dir");
    write_git_proxy(&bin_dir);
    let path_env = bin_dir.display().to_string();

    let output = run(
        &repo,
        &["agent", "commit", "--auto-stage"],
        &path_env,
        Some("chore\nrepo\nAuto stage commit\ny\n"),
    );
    assert_eq!(output.code, 0);

    let subject = git_stdout(&repo, &["log", "-1", "--pretty=%s"]);
    assert_eq!(subject, "chore(repo): Auto stage commit");
}
