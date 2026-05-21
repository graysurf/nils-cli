use crate::common;
use common::{GitCliHarness, init_repo};
use nils_test_support::git::{commit_file, git};
use std::fs;
use std::path::Path;

fn copy_staged_help() -> &'static str {
    "Usage: git-cli utils copy-staged [-p|--stdout|--both]\n  -p, --stdout, --print   Print staged diff to stdout (no status message)\n  --both                  Print to stdout and copy to clipboard\n"
}

fn zip_help() -> &'static str {
    "Usage: git-cli utils zip\nCreate backup-<short-sha>.zip from HEAD in the current directory.\n"
}

fn trim_trailing_newlines(input: &str) -> &str {
    input.trim_end_matches(['\n', '\r'])
}

fn staged_diff(repo: &Path) -> String {
    trim_trailing_newlines(&git(repo, &["diff", "--cached", "--no-color"])).to_string()
}

fn repo_root(repo: &Path) -> String {
    trim_trailing_newlines(&git(repo, &["rev-parse", "--show-toplevel"])).to_string()
}

fn shell_escape(value: &str) -> String {
    if value.is_empty() {
        return "''".to_string();
    }
    let mut out = String::from("'");
    for ch in value.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

fn setup_repo_with_staged_change() -> tempfile::TempDir {
    let dir = init_repo();
    commit_file(dir.path(), "hello.txt", "base\n", "add hello");
    fs::write(dir.path().join("hello.txt"), "base\nchange\n").expect("write staged file");
    git(dir.path(), &["add", "hello.txt"]);
    dir
}

#[test]
fn utils_zip_creates_backup_zip() {
    let harness = GitCliHarness::new();
    let dir = init_repo();

    let short_raw = git(dir.path(), &["rev-parse", "--short", "HEAD"]);
    let short = trim_trailing_newlines(&short_raw);
    let output = harness.run(dir.path(), &["utils", "zip"]);

    assert_eq!(output.code, 0);
    assert_eq!(output.stdout_text(), "");
    assert_eq!(output.stderr_text(), "");

    let zip_path = dir.path().join(format!("backup-{short}.zip"));
    assert!(zip_path.exists(), "expected zip archive to exist");
}

#[test]
fn utils_zip_help_does_not_create_backup_zip() {
    let harness = GitCliHarness::new();
    let dir = init_repo();

    let output = harness.run(dir.path(), &["utils", "zip", "--help"]);

    assert_eq!(output.code, 0);
    assert_eq!(output.stderr_text(), "");
    assert_eq!(output.stdout_text(), zip_help());

    let entries: Vec<_> = fs::read_dir(dir.path())
        .expect("read repo")
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name.starts_with("backup-") && name.ends_with(".zip"))
        .collect();
    assert!(entries.is_empty(), "unexpected backup files: {entries:?}");
}

#[test]
fn utils_copy_staged_both_outputs_diff_and_status() {
    let harness = GitCliHarness::new();
    let dir = setup_repo_with_staged_change();
    let diff = staged_diff(dir.path());

    let output = harness.run(dir.path(), &["utils", "copy-staged", "--both"]);

    assert_eq!(output.code, 0);
    assert_eq!(output.stderr_text(), "");
    assert_eq!(
        output.stdout_text(),
        format!("{diff}\n✅ Staged diff copied to clipboard\n")
    );
}

#[test]
fn utils_copy_staged_stdout_outputs_diff_only() {
    let harness = GitCliHarness::new();
    let dir = setup_repo_with_staged_change();
    let diff = staged_diff(dir.path());

    let output = harness.run(dir.path(), &["utils", "copy-staged", "--stdout"]);

    assert_eq!(output.code, 0);
    assert_eq!(output.stderr_text(), "");
    assert_eq!(output.stdout_text(), format!("{diff}\n"));
}

#[test]
fn utils_copy_staged_no_changes_warns_and_exits_1() {
    let harness = GitCliHarness::new();
    let dir = init_repo();

    let output = harness.run(dir.path(), &["utils", "copy-staged"]);

    assert_eq!(output.code, 1);
    assert_eq!(output.stderr_text(), "");
    assert_eq!(output.stdout_text(), "⚠️  No staged changes to copy\n");
}

#[test]
fn utils_copy_staged_help_prints_usage() {
    let harness = GitCliHarness::new();
    let dir = init_repo();

    let output = harness.run(dir.path(), &["utils", "copy-staged", "--help"]);

    assert_eq!(output.code, 0);
    assert_eq!(output.stderr_text(), "");
    assert_eq!(output.stdout_text(), copy_staged_help());
}

#[test]
fn utils_copy_staged_rejects_conflicting_modes() {
    let harness = GitCliHarness::new();
    let dir = tempfile::TempDir::new().expect("tempdir");

    let output = harness.run(dir.path(), &["utils", "copy-staged", "--stdout", "--both"]);

    assert_eq!(output.code, 1);
    assert_eq!(output.stdout_text(), "");
    assert_eq!(
        output.stderr_text(),
        "❗ Only one output mode is allowed: --stdout or --both\n"
    );
}

#[test]
fn utils_copy_staged_rejects_unknown_arg() {
    let harness = GitCliHarness::new();
    let dir = tempfile::TempDir::new().expect("tempdir");

    let output = harness.run(dir.path(), &["utils", "copy-staged", "--nope"]);

    assert_eq!(output.code, 1);
    assert_eq!(output.stdout_text(), "");
    assert_eq!(
        output.stderr_text(),
        "❗ Unknown argument: --nope\nUsage: git-cli utils copy-staged [-p|--stdout|--both]\n"
    );
}

#[test]
fn utils_root_prints_message() {
    let harness = GitCliHarness::new();
    let dir = init_repo();
    let nested = dir.path().join("nested/dir");
    fs::create_dir_all(&nested).expect("create nested dir");
    let root = repo_root(dir.path());

    let output = harness.run(&nested, &["utils", "root"]);

    assert_eq!(output.code, 0);
    assert_eq!(output.stderr_text(), "");
    assert_eq!(
        output.stdout_text(),
        format!("\n📁 Jumped to Git root: {root}\n")
    );
}

#[test]
fn utils_root_not_in_repo_errors() {
    let harness = GitCliHarness::new();
    let dir = tempfile::TempDir::new().expect("tempdir");

    let output = harness.run(dir.path(), &["utils", "root"]);

    assert_eq!(output.code, 1);
    assert_eq!(output.stdout_text(), "");
    assert_eq!(output.stderr_text(), "❌ Not in a git repository\n");
}

#[test]
fn utils_root_shell_outputs_cd_command() {
    let harness = GitCliHarness::new();
    let base = tempfile::TempDir::new().expect("tempdir");
    let repo_dir = base.path().join("space repo");
    fs::create_dir_all(&repo_dir).expect("create repo dir");
    git(&repo_dir, &["init", "-q"]);

    let nested = repo_dir.join("nested dir");
    fs::create_dir_all(&nested).expect("create nested dir");
    let root = repo_root(&repo_dir);

    let output = harness.run(&nested, &["utils", "root", "--shell"]);

    assert_eq!(output.code, 0);
    assert_eq!(
        output.stdout_text(),
        format!("cd -- {}\n", shell_escape(&root))
    );
    assert_eq!(
        output.stderr_text(),
        format!("📁 Jumped to Git root: {root}\n")
    );
}

#[test]
fn utils_commit_hash_missing_ref_errors() {
    let harness = GitCliHarness::new();
    let dir = tempfile::TempDir::new().expect("tempdir");

    let output = harness.run(dir.path(), &["utils", "commit-hash"]);

    assert_eq!(output.code, 1);
    assert_eq!(output.stdout_text(), "");
    assert_eq!(output.stderr_text(), "❌ Missing git ref\n");
}

#[test]
fn utils_commit_hash_outputs_sha_for_head() {
    let harness = GitCliHarness::new();
    let dir = init_repo();
    let expected =
        trim_trailing_newlines(&git(dir.path(), &["rev-parse", "HEAD^{commit}"])).to_string();

    let output = harness.run(dir.path(), &["utils", "commit-hash", "HEAD"]);

    assert_eq!(output.code, 0);
    assert_eq!(output.stderr_text(), "");
    assert_eq!(output.stdout_text(), format!("{expected}\n"));
}

#[test]
fn utils_commit_hash_resolves_annotated_tag() {
    let harness = GitCliHarness::new();
    let dir = init_repo();
    git(dir.path(), &["tag", "-a", "-m", "v1", "v1"]);
    let expected =
        trim_trailing_newlines(&git(dir.path(), &["rev-parse", "HEAD^{commit}"])).to_string();

    let output = harness.run(dir.path(), &["utils", "commit-hash", "v1"]);

    assert_eq!(output.code, 0);
    assert_eq!(output.stderr_text(), "");
    assert_eq!(output.stdout_text(), format!("{expected}\n"));
}
