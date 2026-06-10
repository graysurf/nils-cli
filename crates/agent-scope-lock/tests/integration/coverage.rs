//! Behavioral coverage for agent-scope-lock guard rails the happy-path `cli`
//! suite does not exercise: overwrite protection, lock-document validation
//! errors, path normalization rejections, git-failure surfacing, and the JSON
//! render arms for read/clear plus the bash completion contract.

use std::fs;
use std::path::Path;

use nils_test_support::cmd::{CmdOutput, run_resolved_in_dir};
use nils_test_support::git::{InitRepoOptions, init_repo_with};
use pretty_assertions::assert_eq;

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

fn lock_arg(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

#[test]
fn create_refuses_to_overwrite_existing_lock_without_force() {
    let repo = init_repo();
    let lock = repo.path().join("scope.json");
    let lock_arg = lock_arg(&lock);

    let first = run(
        repo.path(),
        &["create", "--path", "README.md", "--lock-file", &lock_arg],
    );
    assert_eq!(first.code, 0, "stderr={}", first.stderr_text());

    let second = run(
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
    assert_eq!(second.code, 1, "stdout={}", second.stdout_text());
    assert_eq!(second.stdout_json()["error"]["code"], "lock-exists");

    // --force overwrites in place.
    let forced = run(
        repo.path(),
        &[
            "create",
            "--path",
            "README.md",
            "--lock-file",
            &lock_arg,
            "--force",
        ],
    );
    assert_eq!(forced.code, 0, "stderr={}", forced.stderr_text());
}

#[test]
fn create_writes_lock_into_missing_parent_directories() {
    let repo = init_repo();
    let lock = repo.path().join("nested/dir/scope.json");
    let lock_arg = lock_arg(&lock);

    let output = run(
        repo.path(),
        &["create", "--path", "README.md", "--lock-file", &lock_arg],
    );
    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    assert!(lock.is_file(), "lock should be created with its parents");
}

#[test]
fn create_rejects_paths_outside_the_repository() {
    let repo = init_repo();
    let lock = repo.path().join("scope.json");

    let output = run(
        repo.path(),
        &[
            "create",
            "--path",
            "/etc/hosts",
            "--lock-file",
            &lock_arg(&lock),
            "--format",
            "json",
        ],
    );
    assert_eq!(output.code, 64, "stderr={}", output.stderr_text());
    assert_eq!(output.stdout_json()["error"]["code"], "path-outside-repo");
}

#[test]
fn create_rejects_targeting_the_git_metadata_directory() {
    let repo = init_repo();
    let lock = repo.path().join("scope.json");

    let output = run(
        repo.path(),
        &[
            "create",
            "--path",
            ".git/config",
            "--lock-file",
            &lock_arg(&lock),
            "--format",
            "json",
        ],
    );
    assert_eq!(output.code, 64, "stderr={}", output.stderr_text());
    assert_eq!(output.stdout_json()["error"]["code"], "git-dir-not-allowed");
}

#[test]
fn create_without_paths_is_usage_error() {
    let repo = init_repo();
    let lock = repo.path().join("scope.json");
    // clap allows zero --path, so the runtime guard must reject it.
    let output = run(
        repo.path(),
        &[
            "create",
            "--lock-file",
            &lock_arg(&lock),
            "--format",
            "json",
        ],
    );
    assert_eq!(output.code, 64, "stderr={}", output.stderr_text());
    assert_eq!(output.stdout_json()["error"]["code"], "missing-path");
}

#[test]
fn create_outside_git_repository_surfaces_git_failure() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let lock = tmp.path().join("scope.json");

    let output = run(
        tmp.path(),
        &[
            "create",
            "--path",
            "README.md",
            "--lock-file",
            &lock_arg(&lock),
            "--format",
            "json",
        ],
    );
    assert_eq!(output.code, 1, "stdout={}", output.stdout_text());
    let value = output.stdout_json();
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"]["code"], "git-command-failed");
}

#[test]
fn read_reports_invalid_lock_json() {
    let repo = init_repo();
    let lock = repo.path().join("broken.json");
    fs::write(&lock, "{ not valid json").expect("write lock");

    let output = run(
        repo.path(),
        &["read", "--lock-file", &lock_arg(&lock), "--format", "json"],
    );
    assert_eq!(output.code, 1, "stdout={}", output.stdout_text());
    assert_eq!(output.stdout_json()["error"]["code"], "invalid-lock-json");
}

#[test]
fn read_reports_unsupported_lock_version() {
    let repo = init_repo();
    let lock = repo.path().join("old.json");
    fs::write(
        &lock,
        r#"{"schema_version":"agent-scope-lock.v0","allowed_paths":["src"]}"#,
    )
    .expect("write lock");

    let output = run(
        repo.path(),
        &["read", "--lock-file", &lock_arg(&lock), "--format", "json"],
    );
    assert_eq!(output.code, 1, "stdout={}", output.stdout_text());
    assert_eq!(
        output.stdout_json()["error"]["code"],
        "unsupported-lock-version"
    );
}

#[test]
fn read_reports_lock_without_allowed_paths() {
    let repo = init_repo();
    let lock = repo.path().join("empty.json");
    fs::write(
        &lock,
        r#"{"schema_version":"agent-scope-lock.v1","allowed_paths":[]}"#,
    )
    .expect("write lock");

    let output = run(
        repo.path(),
        &["read", "--lock-file", &lock_arg(&lock), "--format", "json"],
    );
    assert_eq!(output.code, 1, "stdout={}", output.stdout_text());
    assert_eq!(output.stdout_json()["error"]["code"], "invalid-lock");
}

#[test]
fn read_missing_lock_is_a_runtime_error() {
    let repo = init_repo();
    let missing = repo.path().join("nope.json");

    let output = run(
        repo.path(),
        &[
            "read",
            "--lock-file",
            &lock_arg(&missing),
            "--format",
            "json",
        ],
    );
    assert_eq!(output.code, 1, "stdout={}", output.stdout_text());
    let value = output.stdout_json();
    assert_eq!(value["schema_version"], "cli.agent-scope-lock.read.v1");
    assert_eq!(value["error"]["code"], "missing-lock");
}

#[test]
fn read_and_clear_render_json_envelopes() {
    let repo = init_repo();
    let lock = repo.path().join("scope.json");
    let lock_arg = lock_arg(&lock);

    let create = run(
        repo.path(),
        &["create", "--path", "README.md", "--lock-file", &lock_arg],
    );
    assert_eq!(create.code, 0, "stderr={}", create.stderr_text());

    let read = run(
        repo.path(),
        &["read", "--lock-file", &lock_arg, "--format", "json"],
    );
    assert_eq!(read.code, 0, "stderr={}", read.stderr_text());
    let read_value = read.stdout_json();
    assert_eq!(read_value["schema_version"], "cli.agent-scope-lock.read.v1");
    assert_eq!(
        read_value["result"]["lock"]["allowed_paths"][0],
        "README.md"
    );

    let clear = run(
        repo.path(),
        &["clear", "--lock-file", &lock_arg, "--format", "json"],
    );
    assert_eq!(clear.code, 0, "stderr={}", clear.stderr_text());
    let clear_value = clear.stdout_json();
    assert_eq!(
        clear_value["schema_version"],
        "cli.agent-scope-lock.clear.v1"
    );
    assert_eq!(clear_value["result"]["removed"], true);
}

#[test]
fn completion_bash_exports_script() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let output = run(tmp.path(), &["completion", "bash"]);
    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    assert!(
        output.stdout_text().contains("agent-scope-lock"),
        "bash completion should mention the binary"
    );
}
