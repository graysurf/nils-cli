use std::fs;
use std::path::{Path, PathBuf};

use nils_test_support::cmd::{CmdOutput, run_resolved_in_dir};
use nils_test_support::git::{InitRepoOptions, git, init_repo_with};
use pretty_assertions::assert_eq;
use serde_json::Value;

fn init_repo() -> tempfile::TempDir {
    init_repo_with(
        InitRepoOptions::new()
            .with_branch("main")
            .with_initial_commit(),
    )
}

fn run(dir: &Path, args: &[&str]) -> CmdOutput {
    run_resolved_in_dir("agent-scope-lock", dir, args, &[], None)
}

fn json_stdout(output: &CmdOutput) -> Value {
    serde_json::from_str(&output.stdout_text()).expect("stdout should be json")
}

fn lock_arg(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

fn combined_output(output: &CmdOutput) -> String {
    format!("{}{}", output.stdout_text(), output.stderr_text())
}

#[test]
fn help_includes_version_flag_and_examples() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let output = run(tmp.path(), &["--help"]);

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let stdout = output.stdout_text();
    assert!(
        stdout.contains("-V, --version"),
        "missing version flag: {stdout}"
    );
    assert!(stdout.contains("Examples:"), "missing examples: {stdout}");
}

#[test]
fn create_read_clear_lifecycle_uses_git_path_default() {
    let repo = init_repo();

    let create = run(
        repo.path(),
        &[
            "create",
            "--path",
            "README.md",
            "--owner",
            "T3",
            "--note",
            "focused lock",
        ],
    );
    assert_eq!(create.code, 0, "stderr={}", create.stderr_text());
    assert!(
        repo.path().join(".git/agent-scope-lock.json").is_file(),
        "default lock should be stored under .git"
    );

    let read = run(repo.path(), &["read"]);
    assert_eq!(read.code, 0, "stderr={}", read.stderr_text());
    let stdout = read.stdout_text();
    assert!(stdout.contains("owner: T3"), "missing owner: {stdout}");
    assert!(
        stdout.contains("README.md"),
        "missing allowed path: {stdout}"
    );

    let clear = run(repo.path(), &["clear"]);
    assert_eq!(clear.code, 0, "stderr={}", clear.stderr_text());
    assert!(
        !repo.path().join(".git/agent-scope-lock.json").exists(),
        "clear should remove default lock"
    );

    let clear_again = run(repo.path(), &["clear"]);
    assert_eq!(clear_again.code, 0, "stderr={}", clear_again.stderr_text());
    assert!(
        clear_again.stdout_text().contains("already clear"),
        "clear should be idempotent: {}",
        clear_again.stdout_text()
    );
}

#[test]
fn json_create_uses_versioned_envelope() {
    let repo = init_repo();
    let lock = repo.path().join("custom-lock.json");
    let lock_arg = lock_arg(&lock);

    let output = run(
        repo.path(),
        &[
            "create",
            "--path",
            "README.md",
            "--lock-file",
            &lock_arg,
            "--format",
            "json",
        ],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let value = json_stdout(&output);
    assert_eq!(value["schema_version"], "cli.agent-scope-lock.create.v1");
    assert_eq!(value["command"], "agent-scope-lock create");
    assert_eq!(value["ok"], true);
    assert_eq!(
        value["result"]["lock"]["schema_version"],
        "agent-scope-lock.v1"
    );
    assert_eq!(value["result"]["lock"]["allowed_paths"][0], "README.md");
    assert_eq!(value["result"]["lock_file"], lock_arg);
}

#[test]
fn validate_succeeds_when_changed_paths_are_allowed() {
    let repo = init_repo();
    fs::create_dir_all(repo.path().join("src")).expect("src dir");
    fs::write(repo.path().join("src/lib.rs"), "pub fn ok() {}\n").expect("write src");

    let create = run(repo.path(), &["create", "--path", "src"]);
    assert_eq!(create.code, 0, "stderr={}", create.stderr_text());

    let validate = run(repo.path(), &["validate", "--format", "json"]);
    assert_eq!(validate.code, 0, "stderr={}", validate.stderr_text());
    let value = json_stdout(&validate);
    assert_eq!(value["schema_version"], "cli.agent-scope-lock.validate.v1");
    assert_eq!(value["command"], "agent-scope-lock validate");
    assert_eq!(value["ok"], true);
    assert_eq!(value["result"]["violations"].as_array().unwrap().len(), 0);
    assert_eq!(value["result"]["changed_paths"][0], "src/lib.rs");
}

#[test]
fn validate_fails_with_concise_violation_list() {
    let repo = init_repo();
    fs::create_dir_all(repo.path().join("src")).expect("src dir");
    fs::write(repo.path().join("other.txt"), "outside\n").expect("write outside");

    let create = run(repo.path(), &["create", "--path", "src"]);
    assert_eq!(create.code, 0, "stderr={}", create.stderr_text());

    let validate = run(repo.path(), &["validate"]);
    assert_eq!(validate.code, 1, "stdout={}", validate.stdout_text());
    let stderr = validate.stderr_text();
    assert!(
        stderr.contains("scope violations"),
        "missing header: {stderr}"
    );
    assert!(stderr.contains("other.txt"), "missing path: {stderr}");
    assert!(
        stderr.contains("allowed paths:"),
        "missing allowed paths: {stderr}"
    );
}

#[test]
fn missing_lock_fails_with_json_error() {
    let repo = init_repo();
    let missing = repo.path().join("missing-lock.json");
    let missing_arg = lock_arg(&missing);

    let output = run(
        repo.path(),
        &["validate", "--lock-file", &missing_arg, "--format", "json"],
    );

    assert_eq!(output.code, 1, "stderr={}", output.stderr_text());
    let value = json_stdout(&output);
    assert_eq!(value["schema_version"], "cli.agent-scope-lock.validate.v1");
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"]["code"], "missing-lock");
    assert_eq!(value["error"]["details"]["lock_file"], missing_arg);
}

#[test]
fn secret_like_file_contents_are_not_emitted_on_violation() {
    let repo = init_repo();
    fs::create_dir_all(repo.path().join("src")).expect("src dir");
    fs::create_dir_all(repo.path().join("secrets")).expect("secrets dir");
    fs::write(
        repo.path().join("secrets/api-key.txt"),
        "sk-proj-secret-token\n",
    )
    .expect("write secret-like content");

    let create = run(repo.path(), &["create", "--path", "src"]);
    assert_eq!(create.code, 0, "stderr={}", create.stderr_text());

    let validate = run(repo.path(), &["validate", "--format", "json"]);
    assert_eq!(validate.code, 1, "stdout={}", validate.stdout_text());
    let combined = combined_output(&validate);
    assert!(
        combined.contains("secrets/api-key.txt"),
        "violation should include path: {combined}"
    );
    assert!(
        !combined.contains("sk-proj-secret-token"),
        "violation output must not include file contents: {combined}"
    );
}

#[test]
fn validate_modes_split_staged_and_unstaged_paths() {
    let repo = init_repo();
    fs::create_dir_all(repo.path().join("src")).expect("src dir");
    fs::write(repo.path().join("src/staged.rs"), "staged\n").expect("staged");
    git(repo.path(), &["add", "src/staged.rs"]);
    fs::write(repo.path().join("outside.txt"), "unstaged\n").expect("unstaged");

    let create = run(repo.path(), &["create", "--path", "src"]);
    assert_eq!(create.code, 0, "stderr={}", create.stderr_text());

    let staged = run(repo.path(), &["validate", "--changes", "staged"]);
    assert_eq!(staged.code, 0, "stderr={}", staged.stderr_text());

    let unstaged = run(repo.path(), &["validate", "--changes", "unstaged"]);
    assert_eq!(unstaged.code, 1, "stdout={}", unstaged.stdout_text());
    assert!(
        unstaged.stderr_text().contains("outside.txt"),
        "missing unstaged violation: {}",
        unstaged.stderr_text()
    );
}

#[test]
fn create_treats_relative_paths_as_repo_relative_from_subdirectories() {
    let repo = init_repo();
    fs::create_dir_all(repo.path().join("nested")).expect("nested dir");
    let lock = repo.path().join("scope.json");
    let lock_arg = lock_arg(&lock);

    let output = run(
        &repo.path().join("nested"),
        &[
            "create",
            "--path",
            "README.md",
            "--lock-file",
            &lock_arg,
            "--format",
            "json",
        ],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let value = json_stdout(&output);
    assert_eq!(value["result"]["lock"]["allowed_paths"][0], "README.md");
}

#[test]
fn completion_export_succeeds_outside_git_repo() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let output = run(tmp.path(), &["completion", "zsh"]);

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    assert!(
        output.stdout_text().contains("#compdef agent-scope-lock"),
        "missing completion header: {}",
        output.stdout_text()
    );
}

#[test]
fn lock_file_override_accepts_relative_path() {
    let repo = init_repo();
    let lock_rel = PathBuf::from("tmp-lock.json");

    let output = run(
        repo.path(),
        &[
            "create",
            "--path",
            "README.md",
            "--lock-file",
            lock_rel.to_str().unwrap(),
        ],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    assert!(repo.path().join(lock_rel).is_file());
}
