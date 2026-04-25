use crate::common;
use std::fs;
use std::path::Path;

use common::{GitCliHarness, write_context_json_git_stub};
use nils_test_support::StubBinDir;
use nils_test_support::cmd::run_with;
use nils_test_support::git::{git, git_with_env};
use nils_test_support::stubs::STUB_LOG_ENV;
use pretty_assertions::assert_eq;
use serde_json::Value;
use tempfile::TempDir;

const GIT_SCOPE_STUB: &str = r#"#!/bin/bash
set -euo pipefail
cat <<'EOF'
.
├── hello.txt
├── package-lock.json
└── yarn.lock
EOF
"#;

const FILE_STUB: &str = r#"#!/usr/bin/env bash
set -euo pipefail

data="$(cat || true)"

if [[ "$data" == *"GIT_CLI_BINARY_MARKER"* ]]; then
  printf "%s\n" "application/octet-stream; charset=binary"
else
  printf "%s\n" "text/plain; charset=us-ascii"
fi
"#;

fn write_scope_stubs() -> StubBinDir {
    let stubs = StubBinDir::new();
    stubs.write_exe("git-scope", GIT_SCOPE_STUB);
    stubs.write_exe("file", FILE_STUB);
    stubs
}

fn git_init_repo(dir: &Path) {
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.name", "Fixture Bot"]);
    git(dir, &["config", "user.email", "fixture@example.invalid"]);
    git(dir, &["config", "commit.gpgsign", "false"]);
    git(dir, &["config", "advice.detachedHead", "false"]);
}

fn git_commit_all(dir: &Path, msg: &str, date: &str) {
    git(dir, &["add", "-A"]);
    let envs = [
        ("GIT_AUTHOR_NAME", "Fixture Bot"),
        ("GIT_AUTHOR_EMAIL", "fixture@example.invalid"),
        ("GIT_COMMITTER_NAME", "Fixture Bot"),
        ("GIT_COMMITTER_EMAIL", "fixture@example.invalid"),
        ("GIT_AUTHOR_DATE", date),
        ("GIT_COMMITTER_DATE", date),
    ];
    git_with_env(
        dir,
        &["-c", "commit.gpgsign=false", "commit", "-q", "-m", msg],
        &envs,
    );
}

fn assert_exit_code(id: &str, output: &nils_test_support::cmd::CmdOutput, expected: i32) {
    assert_eq!(output.code, expected, "exit code mismatch for {id}");
}

fn assert_contains(id: &str, stream: &str, haystack: &str, needle: &str) {
    assert!(
        haystack.contains(needle),
        "expected {id} {stream} to contain {needle:?}\n---\n{haystack}"
    );
}

fn assert_stdout_contains_all(id: &str, stdout: &str, needles: &[&str]) {
    for needle in needles {
        assert_contains(id, "stdout", stdout, needle);
    }
}

fn assert_stderr_contains_all(id: &str, stderr: &str, needles: &[&str]) {
    for needle in needles {
        assert_contains(id, "stderr", stderr, needle);
    }
}

#[test]
fn commit_context_fixture_f030() {
    let repo = TempDir::new().expect("tempdir");
    git_init_repo(repo.path());

    fs::write(repo.path().join("hello.txt"), "base\n").expect("write hello");
    git_commit_all(repo.path(), "init", "2000-01-01T00:00:00+0000");

    fs::write(repo.path().join("hello.txt"), "GIT_CLI_BINARY_MARKER\n").expect("write hello");
    fs::write(repo.path().join("yarn.lock"), "yarn-lock\n").expect("write yarn");
    fs::write(
        repo.path().join("package-lock.json"),
        "{\"name\":\"pkg\"}\n",
    )
    .expect("write pkg");
    git(
        repo.path(),
        &["add", "hello.txt", "yarn.lock", "package-lock.json"],
    );

    let harness = GitCliHarness::new();
    let stubs = write_scope_stubs();
    let options = harness
        .cmd_options(repo.path())
        .with_path_prepend(stubs.path())
        .with_env("GIT_CLI_FIXTURE_FILE_MODE", "stub");

    let output = run_with(
        &harness.git_cli_bin(),
        &[
            "commit",
            "context",
            "--both",
            "--no-color",
            "--include",
            "yarn.lock",
        ],
        &options,
    );

    assert_exit_code("F030", &output, 0);
    let stdout = output.stdout_text();
    let stderr = output.stderr_text();
    assert!(
        stderr.trim().is_empty(),
        "expected F030 stderr to be empty, got:\n{stderr}"
    );
    assert!(
        stdout.starts_with("# Commit Context\n"),
        "expected F030 stdout to start with commit context header\n---\n{stdout}"
    );
    assert_stdout_contains_all(
        "F030",
        &stdout,
        &[
            "## 📂 Scope and file tree:",
            "## 📄 Git staged diff:",
            "## 📚 Staged file contents (index version):",
            ".\n├── hello.txt\n├── package-lock.json\n└── yarn.lock",
            "diff --git a/hello.txt b/hello.txt",
            "+GIT_CLI_BINARY_MARKER",
            "### hello.txt (M)",
            "[Binary file content hidden]",
            "Type: application/octet-stream; charset=binary",
            "### yarn.lock (A)",
            "yarn-lock\n",
            "### package-lock.json (A)",
            "[Lockfile content hidden]",
            "Tip: use --include package-lock.json",
        ],
    );
}

#[test]
fn commit_context_fixture_f031_missing_clipboard() {
    let repo = TempDir::new().expect("tempdir");
    git_init_repo(repo.path());

    fs::write(repo.path().join("hello.txt"), "base\n").expect("write hello");
    git_commit_all(repo.path(), "init", "2000-01-01T00:00:00+0000");

    fs::write(repo.path().join("hello.txt"), "base\nchange\n").expect("write hello");
    git(repo.path(), &["add", "hello.txt"]);

    let harness = GitCliHarness::new();
    let stubs = write_scope_stubs();
    let options = harness
        .cmd_options(repo.path())
        .with_path_prepend(stubs.path())
        .with_env("GIT_CLI_FIXTURE_CLIPBOARD_MODE", "missing");

    let output = run_with(&harness.git_cli_bin(), &["commit", "context"], &options);

    assert_exit_code("F031", &output, 0);
    let stdout = output.stdout_text();
    let stderr = output.stderr_text();
    assert_stderr_contains_all(
        "F031",
        &stderr,
        &["⚠️  No clipboard tool found (requires pbcopy, xclip, or xsel)"],
    );
    assert_stdout_contains_all(
        "F031",
        &stdout,
        &[
            "✅ Commit context copied to clipboard with:",
            "• Diff",
            "• Scope summary (via git-scope staged)",
            "• Staged file contents (index version)",
        ],
    );
}

#[test]
fn commit_context_fixture_f032_missing_file() {
    let repo = TempDir::new().expect("tempdir");
    git_init_repo(repo.path());

    fs::write(repo.path().join("hello.txt"), "base\n").expect("write hello");
    git_commit_all(repo.path(), "init", "2000-01-01T00:00:00+0000");

    fs::write(repo.path().join("bin.dat"), b"\x00\x01\x02binary\x00data\n").expect("write bin");
    git(repo.path(), &["add", "bin.dat"]);

    let harness = GitCliHarness::new();
    let stubs = write_scope_stubs();
    let options = harness
        .cmd_options(repo.path())
        .with_path_prepend(stubs.path())
        .with_env("GIT_CLI_FIXTURE_FILE_MODE", "missing");

    let output = run_with(
        &harness.git_cli_bin(),
        &["commit", "context", "--stdout"],
        &options,
    );

    assert_exit_code("F032", &output, 0);
    let stdout = output.stdout_text();
    let stderr = output.stderr_text();
    assert!(
        stderr.trim().is_empty(),
        "expected F032 stderr to be empty, got:\n{stderr}"
    );
    assert_stdout_contains_all(
        "F032",
        &stdout,
        &[
            "# Commit Context",
            "### bin.dat (A)",
            "[Binary file content hidden]",
        ],
    );
}

#[test]
fn commit_context_no_staged_changes() {
    let repo = TempDir::new().expect("tempdir");
    git_init_repo(repo.path());

    fs::write(repo.path().join("hello.txt"), "base\n").expect("write hello");
    git_commit_all(repo.path(), "init", "2000-01-01T00:00:00+0000");

    let harness = GitCliHarness::new();
    let output = run_with(
        &harness.git_cli_bin(),
        &["commit", "context"],
        &harness.cmd_options(repo.path()),
    );

    assert_exit_code("F035", &output, 1);
    let stderr = output.stderr_text();
    assert_stderr_contains_all("F035", &stderr, &["⚠️  No staged changes to record"]);
}

#[test]
fn commit_context_missing_include_value() {
    let repo = TempDir::new().expect("tempdir");
    git_init_repo(repo.path());

    fs::write(repo.path().join("hello.txt"), "base\n").expect("write hello");
    git_commit_all(repo.path(), "init", "2000-01-01T00:00:00+0000");

    let harness = GitCliHarness::new();
    let output = run_with(
        &harness.git_cli_bin(),
        &["commit", "context", "--include"],
        &harness.cmd_options(repo.path()),
    );

    assert_exit_code("F036", &output, 2);
    let stderr = output.stderr_text();
    assert_stderr_contains_all("F036", &stderr, &["❌ Missing value for --include"]);
}

#[test]
fn commit_context_help_output() {
    let repo = TempDir::new().expect("tempdir");
    git_init_repo(repo.path());

    fs::write(repo.path().join("hello.txt"), "base\n").expect("write hello");
    git_commit_all(repo.path(), "init", "2000-01-01T00:00:00+0000");

    let harness = GitCliHarness::new();
    let output = run_with(
        &harness.git_cli_bin(),
        &["commit", "context", "--help"],
        &harness.cmd_options(repo.path()),
    );

    assert_exit_code("F037", &output, 0);
    let stdout = output.stdout_text();
    assert_stdout_contains_all(
        "F037",
        &stdout,
        &[
            "Usage: git-commit-context",
            "--stdout",
            "--both",
            "--no-color",
            "--include",
        ],
    );
}

#[test]
fn commit_context_missing_git_scope() {
    let repo = TempDir::new().expect("tempdir");
    git_init_repo(repo.path());

    fs::write(repo.path().join("hello.txt"), "base\n").expect("write hello");
    git_commit_all(repo.path(), "init", "2000-01-01T00:00:00+0000");

    fs::write(repo.path().join("hello.txt"), "base\nchange\n").expect("write hello");
    git(repo.path(), &["add", "hello.txt"]);

    let harness = GitCliHarness::new();
    let options = harness
        .cmd_options(repo.path())
        .with_env("GIT_CLI_FIXTURE_GIT_SCOPE_MODE", "missing");

    let output = run_with(&harness.git_cli_bin(), &["commit", "context"], &options);

    assert_exit_code("F038", &output, 1);
    let stderr = output.stderr_text();
    assert_stderr_contains_all(
        "F038",
        &stderr,
        &["❗ git-scope is required but was not found in PATH."],
    );
}

#[test]
fn commit_context_unknown_args_warning() {
    let repo = TempDir::new().expect("tempdir");
    git_init_repo(repo.path());

    fs::write(repo.path().join("hello.txt"), "base\n").expect("write hello");
    git_commit_all(repo.path(), "init", "2000-01-01T00:00:00+0000");

    fs::write(repo.path().join("hello.txt"), "base\nchange\n").expect("write hello");
    git(repo.path(), &["add", "hello.txt"]);

    let harness = GitCliHarness::new();
    let stubs = write_scope_stubs();
    let options = harness
        .cmd_options(repo.path())
        .with_path_prepend(stubs.path());

    let output = run_with(
        &harness.git_cli_bin(),
        &["commit", "context", "--stdout", "--bogus", "extra"],
        &options,
    );

    assert_exit_code("F043", &output, 0);
    let stdout = output.stdout_text();
    let stderr = output.stderr_text();
    assert_stderr_contains_all(
        "F043",
        &stderr,
        &["⚠️  Ignoring unknown arguments: --bogus extra"],
    );
    assert_stdout_contains_all("F043", &stdout, &["# Commit Context"]);
}

#[test]
fn commit_context_json_unknown_args_warning() {
    let repo = TempDir::new().expect("tempdir");
    git_init_repo(repo.path());

    fs::write(repo.path().join("hello.txt"), "base\n").expect("write hello");
    git_commit_all(repo.path(), "init", "2000-01-01T00:00:00+0000");

    fs::write(repo.path().join("hello.txt"), "base\nchange\n").expect("write hello");
    git(repo.path(), &["add", "hello.txt"]);

    let harness = GitCliHarness::new();
    let output = run_with(
        &harness.git_cli_bin(),
        &["commit", "context-json", "--stdout", "--bogus", "extra"],
        &harness.cmd_options(repo.path()),
    );

    assert_exit_code("F049", &output, 0);
    let stderr = output.stderr_text();
    assert_stderr_contains_all(
        "F049",
        &stderr,
        &["⚠️  Ignoring unknown arguments: --bogus extra"],
    );
    assert!(!output.stdout_text().trim().is_empty());
}

#[test]
fn commit_context_no_color_env_propagates() {
    let repo = TempDir::new().expect("tempdir");
    git_init_repo(repo.path());

    fs::write(repo.path().join("hello.txt"), "base\n").expect("write hello");
    git_commit_all(repo.path(), "init", "2000-01-01T00:00:00+0000");

    fs::write(repo.path().join("hello.txt"), "base\nchange\n").expect("write hello");
    git(repo.path(), &["add", "hello.txt"]);

    let scope_stub = r#"#!/bin/bash
set -euo pipefail
printf "SCOPE_ARGS: %s\n" "$*"
"#;
    let scope_bin = StubBinDir::new();
    scope_bin.write_exe("git-scope", scope_stub);

    let harness = GitCliHarness::new();
    let options = harness
        .cmd_options(repo.path())
        .with_path_prepend(scope_bin.path())
        .with_env("NO_COLOR", "1");

    let output = run_with(
        &harness.git_cli_bin(),
        &["commit", "context", "--stdout"],
        &options,
    );

    assert_exit_code("F044", &output, 0);
    let stdout = output.stdout_text();
    assert_stdout_contains_all("F044", &stdout, &["SCOPE_ARGS: staged --no-color"]);
}

#[test]
fn commit_context_binary_missing_file_command() {
    let repo = TempDir::new().expect("tempdir");
    git_init_repo(repo.path());

    fs::write(repo.path().join("hello.txt"), "base\n").expect("write hello");
    git_commit_all(repo.path(), "init", "2000-01-01T00:00:00+0000");

    fs::write(repo.path().join("bin.dat"), b"\x00\x01\x02binary\x00data\n").expect("write bin");
    git(repo.path(), &["add", "bin.dat"]);

    let harness = GitCliHarness::new();
    let stubs = write_scope_stubs();
    let options = harness
        .cmd_options(repo.path())
        .with_path_prepend(stubs.path())
        .with_env("GIT_CLI_FIXTURE_FILE_MODE", "missing");

    let output = run_with(
        &harness.git_cli_bin(),
        &["commit", "context", "--stdout"],
        &options,
    );

    assert_exit_code("F045", &output, 0);
    let stdout = output.stdout_text();
    assert_stdout_contains_all(
        "F045",
        &stdout,
        &["### bin.dat (A)", "[Binary file content hidden]"],
    );
    assert!(!stdout.contains("Type:"), "expected F045 no type line");
}

#[test]
fn commit_context_deleted_file_content() {
    let repo = TempDir::new().expect("tempdir");
    git_init_repo(repo.path());

    fs::write(repo.path().join("deleted.txt"), "keep me\n").expect("write file");
    git_commit_all(repo.path(), "init", "2000-01-01T00:00:00+0000");

    fs::remove_file(repo.path().join("deleted.txt")).expect("remove file");
    git(repo.path(), &["add", "-A"]);

    let harness = GitCliHarness::new();
    let stubs = write_scope_stubs();
    let options = harness
        .cmd_options(repo.path())
        .with_path_prepend(stubs.path());

    let output = run_with(
        &harness.git_cli_bin(),
        &["commit", "context", "--stdout"],
        &options,
    );

    assert_exit_code("F046", &output, 0);
    let stdout = output.stdout_text();
    assert_stdout_contains_all(
        "F046",
        &stdout,
        &[
            "### deleted.txt (D)",
            "[Deleted file, showing HEAD version]",
            "keep me",
        ],
    );
}

#[test]
fn commit_context_lockfile_include_override() {
    let repo = TempDir::new().expect("tempdir");
    git_init_repo(repo.path());

    fs::write(repo.path().join("hello.txt"), "base\n").expect("write hello");
    git_commit_all(repo.path(), "init", "2000-01-01T00:00:00+0000");

    fs::write(
        repo.path().join("package-lock.json"),
        "{\"name\":\"pkg\",\"lock\":true}\n",
    )
    .expect("write lockfile");
    git(repo.path(), &["add", "package-lock.json"]);

    let harness = GitCliHarness::new();
    let stubs = write_scope_stubs();
    let options = harness
        .cmd_options(repo.path())
        .with_path_prepend(stubs.path());

    let output = run_with(
        &harness.git_cli_bin(),
        &[
            "commit",
            "context",
            "--stdout",
            "--include",
            "package-lock.json",
        ],
        &options,
    );

    assert_exit_code("F047", &output, 0);
    let stdout = output.stdout_text();
    assert_stdout_contains_all(
        "F047",
        &stdout,
        &["### package-lock.json (A)", "\"lock\":true"],
    );
    assert!(
        !stdout.contains("Lockfile content hidden"),
        "expected F047 lockfile to be included"
    );
}

#[test]
fn commit_context_json_fixture_f033() {
    let root = TempDir::new().expect("tempdir");
    let repo_path = root.path().join("f033-commit-context-json");
    fs::create_dir_all(&repo_path).expect("create repo dir");
    git_init_repo(&repo_path);

    fs::write(repo_path.join("hello.txt"), "base\n").expect("write hello");
    git_commit_all(&repo_path, "init", "2000-01-01T00:00:00+0000");

    fs::write(repo_path.join("hello.txt"), "base\nchange\n").expect("write hello");
    git(&repo_path, &["add", "hello.txt"]);

    let harness = GitCliHarness::new();
    let options = harness
        .cmd_options(&repo_path)
        .with_env("GIT_CLI_FIXTURE_DATE_MODE", "fixed");

    let output = run_with(
        &harness.git_cli_bin(),
        &[
            "commit",
            "context-json",
            "--stdout",
            "--pretty",
            "--bundle",
            "--out-dir",
            "./out/commit-context",
        ],
        &options,
    );

    assert_exit_code("F033", &output, 0);
    let stdout = output.stdout_text();
    let stderr = output.stderr_text();
    assert!(
        stderr.trim().is_empty(),
        "expected F033 stderr to be empty, got:\n{stderr}"
    );
    assert_stdout_contains_all(
        "F033",
        &stdout,
        &[
            "===== commit-context.json =====",
            "===== staged.patch =====",
        ],
    );

    let json_start = stdout
        .strip_prefix("===== commit-context.json =====\n")
        .expect("bundle json header");
    let (json_body, patch_body) = json_start
        .split_once("\n\n===== staged.patch =====\n")
        .expect("bundle patch split");

    let manifest_path = repo_path.join("out/commit-context/commit-context.json");
    let patch_path = repo_path.join("out/commit-context/staged.patch");

    let manifest = fs::read_to_string(manifest_path).expect("read manifest");
    let patch = fs::read_to_string(patch_path).expect("read patch");
    let parsed: Value = serde_json::from_str(json_body).expect("parse commit-context json");
    let generated_at = parsed
        .get("generatedAt")
        .and_then(|value| value.as_str())
        .expect("generatedAt field");

    assert_eq!(manifest, format!("{json_body}\n"));
    assert_eq!(patch, patch_body);
    assert_eq!(generated_at, "2000-01-02T03:04:05Z");
}

#[test]
fn commit_context_json_no_staged_changes() {
    let repo = TempDir::new().expect("tempdir");
    git_init_repo(repo.path());

    fs::write(repo.path().join("hello.txt"), "base\n").expect("write hello");
    git_commit_all(repo.path(), "init", "2000-01-01T00:00:00+0000");

    let harness = GitCliHarness::new();
    let output = run_with(
        &harness.git_cli_bin(),
        &["commit", "context-json"],
        &harness.cmd_options(repo.path()),
    );

    assert_exit_code("F039", &output, 1);
    let stderr = output.stderr_text();
    assert_stderr_contains_all("F039", &stderr, &["⚠️  No staged changes to record"]);
}

#[test]
fn commit_context_json_bad_out_dir() {
    let repo = TempDir::new().expect("tempdir");
    git_init_repo(repo.path());

    fs::write(repo.path().join("hello.txt"), "base\n").expect("write hello");
    git_commit_all(repo.path(), "init", "2000-01-01T00:00:00+0000");

    fs::write(repo.path().join("hello.txt"), "base\nchange\n").expect("write hello");
    git(repo.path(), &["add", "hello.txt"]);

    let out_dir = repo.path().join("out-file");
    fs::write(&out_dir, "not a dir\n").expect("write out dir file");

    let harness = GitCliHarness::new();
    let output = run_with(
        &harness.git_cli_bin(),
        &[
            "commit",
            "context-json",
            "--out-dir",
            out_dir.to_string_lossy().as_ref(),
        ],
        &harness.cmd_options(repo.path()),
    );

    assert_exit_code("F040", &output, 1);
    let stderr = output.stderr_text();
    assert_stderr_contains_all(
        "F040",
        &stderr,
        &[&format!(
            "❌ Failed to create output directory: {}",
            out_dir.to_string_lossy()
        )],
    );
}

#[test]
fn commit_context_json_stdout_non_bundle() {
    let repo = TempDir::new().expect("tempdir");
    git_init_repo(repo.path());

    fs::write(repo.path().join("hello.txt"), "base\n").expect("write hello");
    git_commit_all(repo.path(), "init", "2000-01-01T00:00:00+0000");

    fs::write(repo.path().join("hello.txt"), "base\nchange\n").expect("write hello");
    git(repo.path(), &["add", "hello.txt"]);

    let out_dir = repo.path().join("out/commit-context");
    let harness = GitCliHarness::new();
    let output = run_with(
        &harness.git_cli_bin(),
        &[
            "commit",
            "context-json",
            "--stdout",
            "--out-dir",
            out_dir.to_string_lossy().as_ref(),
        ],
        &harness.cmd_options(repo.path()),
    );

    assert_exit_code("F041", &output, 0);
    let stdout = output.stdout_text();
    assert!(!stdout.contains("===== commit-context.json ====="));
    let parsed: Value = serde_json::from_str(stdout.trim()).expect("parse json stdout");
    assert_eq!(
        parsed.get("schemaVersion").and_then(|value| value.as_i64()),
        Some(1)
    );
}

#[test]
fn commit_context_json_help_output() {
    let repo = TempDir::new().expect("tempdir");
    git_init_repo(repo.path());

    fs::write(repo.path().join("hello.txt"), "base\n").expect("write hello");
    git_commit_all(repo.path(), "init", "2000-01-01T00:00:00+0000");

    let harness = GitCliHarness::new();
    let output = run_with(
        &harness.git_cli_bin(),
        &["commit", "context-json", "--help"],
        &harness.cmd_options(repo.path()),
    );

    assert_exit_code("F042", &output, 0);
    let stdout = output.stdout_text();
    assert_stdout_contains_all(
        "F042",
        &stdout,
        &[
            "Usage: git-commit-context-json",
            "--stdout",
            "--both",
            "--pretty",
            "--bundle",
            "--out-dir",
        ],
    );
}

#[test]
fn commit_context_json_missing_out_dir_value() {
    let repo = TempDir::new().expect("tempdir");
    git_init_repo(repo.path());

    let harness = GitCliHarness::new();
    let output = run_with(
        &harness.git_cli_bin(),
        &["commit", "context-json", "--out-dir"],
        &harness.cmd_options(repo.path()),
    );

    assert_exit_code("F050", &output, 2);
    let stderr = output.stderr_text();
    assert_stderr_contains_all("F050", &stderr, &["❌ Missing value for --out-dir"]);
    assert_eq!(output.stdout_text(), "");
}

#[test]
fn commit_context_json_resolve_out_dir_missing_git_dir() {
    let repo = TempDir::new().expect("tempdir");

    let stubs = StubBinDir::new();
    stubs.write_exe(
        "git",
        r#"#!/bin/bash
set -euo pipefail

args=("$@")

if [[ ${#args[@]} -ge 2 && "${args[0]}" == "rev-parse" && "${args[1]}" == "--is-inside-work-tree" ]]; then
  exit 0
fi

if [[ ${#args[@]} -ge 4 && "${args[0]}" == "diff" && "${args[1]}" == "--cached" && "${args[2]}" == "--quiet" && "${args[3]}" == "--exit-code" ]]; then
  exit 1
fi

if [[ ${#args[@]} -ge 2 && "${args[0]}" == "rev-parse" && "${args[1]}" == "--git-dir" ]]; then
  exit 0
fi

exit 0
"#,
    );

    let harness = GitCliHarness::new();
    let options = harness
        .cmd_options(repo.path())
        .with_path_prepend(stubs.path());

    let output = run_with(
        &harness.git_cli_bin(),
        &["commit", "context-json"],
        &options,
    );

    assert_exit_code("F053", &output, 1);
    let stderr = output.stderr_text();
    assert_stderr_contains_all("F053", &stderr, &["❌ Failed to resolve git dir."]);
}

#[test]
fn commit_context_json_patch_write_failure() {
    let repo = TempDir::new().expect("tempdir");

    let out_dir = repo.path().join("out/commit-context");
    fs::create_dir_all(&out_dir).expect("create out dir");
    fs::create_dir_all(out_dir.join("staged.patch")).expect("create staged.patch dir");

    let harness = GitCliHarness::new();
    let stubs = StubBinDir::new();
    write_context_json_git_stub(&stubs);

    let options = harness
        .cmd_options(repo.path())
        .with_path_prepend(stubs.path());

    let output = run_with(
        &harness.git_cli_bin(),
        &[
            "commit",
            "context-json",
            "--out-dir",
            out_dir.to_string_lossy().as_ref(),
        ],
        &options,
    );

    assert_exit_code("F051", &output, 1);
    let stderr = output.stderr_text();
    assert_stderr_contains_all(
        "F051",
        &stderr,
        &[&format!(
            "❌ Failed to write staged patch: {}",
            out_dir.join("staged.patch").to_string_lossy()
        )],
    );
}

#[test]
fn commit_context_json_manifest_write_failure() {
    let repo = TempDir::new().expect("tempdir");

    let out_dir = repo.path().join("out/commit-context");
    fs::create_dir_all(&out_dir).expect("create out dir");
    fs::create_dir_all(out_dir.join("commit-context.json")).expect("create manifest dir");

    let harness = GitCliHarness::new();
    let stubs = StubBinDir::new();
    write_context_json_git_stub(&stubs);

    let options = harness
        .cmd_options(repo.path())
        .with_path_prepend(stubs.path());

    let output = run_with(
        &harness.git_cli_bin(),
        &[
            "commit",
            "context-json",
            "--out-dir",
            out_dir.to_string_lossy().as_ref(),
        ],
        &options,
    );

    assert_exit_code("F052", &output, 1);
    let stderr = output.stderr_text();
    assert_stderr_contains_all(
        "F052",
        &stderr,
        &[&format!(
            "❌ Failed to write JSON manifest: {}",
            out_dir.join("commit-context.json").to_string_lossy()
        )],
    );
}

#[test]
fn commit_context_json_both_outputs_bundle_and_clipboard() {
    let repo = TempDir::new().expect("tempdir");
    git_init_repo(repo.path());

    fs::write(repo.path().join("hello.txt"), "base\n").expect("write hello");
    git_commit_all(repo.path(), "init", "2000-01-01T00:00:00+0000");

    fs::write(repo.path().join("hello.txt"), "base\nchange\n").expect("write hello");
    git(repo.path(), &["add", "hello.txt"]);

    let stubs = StubBinDir::new();
    let pbcopy_out = repo.path().join("pbcopy-bundle.out");
    stubs.write_exe(
        "pbcopy",
        r#"#!/bin/bash
set -euo pipefail
out="${PB_COPY_OUT:?PB_COPY_OUT is required}"
/bin/cat > "$out"
"#,
    );

    let harness = GitCliHarness::new();
    let options = harness
        .cmd_options(repo.path())
        .with_path_prepend(stubs.path())
        .with_env("PB_COPY_OUT", pbcopy_out.to_string_lossy().as_ref())
        .with_env("GIT_CLI_FIXTURE_DATE_MODE", "fixed");

    let output = run_with(
        &harness.git_cli_bin(),
        &["commit", "context-json", "--both", "--bundle"],
        &options,
    );

    assert_exit_code("F043", &output, 0);
    let stdout = output.stdout_text();
    assert_stdout_contains_all(
        "F043",
        &stdout,
        &[
            "===== commit-context.json =====",
            "===== staged.patch =====",
        ],
    );

    let clipboard = fs::read_to_string(pbcopy_out).expect("read pbcopy output");
    assert!(
        clipboard.contains("===== commit-context.json ====="),
        "expected F043 clipboard to contain bundle header"
    );
    assert!(
        clipboard.contains("===== staged.patch ====="),
        "expected F043 clipboard to contain patch header"
    );
}

#[test]
fn commit_context_json_both_outputs_json_and_clipboard() {
    let repo = TempDir::new().expect("tempdir");
    git_init_repo(repo.path());

    fs::write(repo.path().join("hello.txt"), "base\n").expect("write hello");
    git_commit_all(repo.path(), "init", "2000-01-01T00:00:00+0000");

    fs::write(repo.path().join("hello.txt"), "base\nchange\n").expect("write hello");
    git(repo.path(), &["add", "hello.txt"]);

    let stubs = StubBinDir::new();
    let pbcopy_out = repo.path().join("pbcopy-json.out");
    stubs.write_exe(
        "pbcopy",
        r#"#!/bin/bash
set -euo pipefail
out="${PB_COPY_OUT:?PB_COPY_OUT is required}"
/bin/cat > "$out"
"#,
    );

    let harness = GitCliHarness::new();
    let options = harness
        .cmd_options(repo.path())
        .with_path_prepend(stubs.path())
        .with_env("PB_COPY_OUT", pbcopy_out.to_string_lossy().as_ref());

    let output = run_with(
        &harness.git_cli_bin(),
        &["commit", "context-json", "--both"],
        &options,
    );

    assert_exit_code("F044", &output, 0);
    let stdout = output.stdout_text();
    assert!(
        !stdout.contains("===== commit-context.json ====="),
        "expected F044 stdout to be JSON only"
    );
    let parsed: Value = serde_json::from_str(stdout.trim()).expect("parse json stdout");
    assert_eq!(
        parsed.get("schemaVersion").and_then(|value| value.as_i64()),
        Some(1)
    );

    let clipboard = fs::read_to_string(pbcopy_out).expect("read pbcopy output");
    assert!(
        !clipboard.contains("===== commit-context.json ====="),
        "expected F044 clipboard to be JSON only"
    );
    let parsed_clipboard: Value =
        serde_json::from_str(clipboard.trim()).expect("parse json clipboard");
    assert_eq!(
        parsed_clipboard
            .get("schemaVersion")
            .and_then(|value| value.as_i64()),
        Some(1)
    );
}

#[test]
fn commit_context_json_default_out_dir_uses_git_dir() {
    let repo = TempDir::new().expect("tempdir");
    git_init_repo(repo.path());

    fs::write(repo.path().join("hello.txt"), "base\n").expect("write hello");
    git_commit_all(repo.path(), "init", "2000-01-01T00:00:00+0000");

    fs::write(repo.path().join("hello.txt"), "base\nchange\n").expect("write hello");
    git(repo.path(), &["add", "hello.txt"]);

    let harness = GitCliHarness::new();
    let output = run_with(
        &harness.git_cli_bin(),
        &["commit", "context-json", "--stdout"],
        &harness.cmd_options(repo.path()),
    );

    assert_exit_code("F045", &output, 0);
    let manifest_path = repo.path().join(".git/commit-context/commit-context.json");
    let patch_path = repo.path().join(".git/commit-context/staged.patch");
    assert!(manifest_path.exists(), "expected F045 manifest to exist");
    assert!(patch_path.exists(), "expected F045 patch to exist");
}

#[test]
fn commit_context_json_missing_git_repo() {
    let repo = TempDir::new().expect("tempdir");

    let harness = GitCliHarness::new();
    let output = run_with(
        &harness.git_cli_bin(),
        &["commit", "context-json"],
        &harness.cmd_options(repo.path()),
    );

    assert_exit_code("F046", &output, 1);
    let stderr = output.stderr_text();
    assert_stderr_contains_all("F046", &stderr, &["❌ Not a git repository."]);
}

#[test]
fn commit_context_json_diff_cached_error() {
    let repo = TempDir::new().expect("tempdir");

    let stubs = StubBinDir::new();
    stubs.write_exe(
        "git",
        r#"#!/bin/bash
set -euo pipefail

if [[ "$1" == "rev-parse" && "$2" == "--is-inside-work-tree" ]]; then
  exit 0
fi

if [[ "$1" == "diff" && "$2" == "--cached" && "$3" == "--quiet" && "$4" == "--exit-code" ]]; then
  exit 2
fi

exit 0
"#,
    );

    let harness = GitCliHarness::new();
    let options = harness
        .cmd_options(repo.path())
        .with_path_prepend(stubs.path())
        .with_env(
            STUB_LOG_ENV,
            repo.path().join("stub.log").to_string_lossy().as_ref(),
        );

    let output = run_with(
        &harness.git_cli_bin(),
        &["commit", "context-json"],
        &options,
    );

    assert_exit_code("F047", &output, 1);
    let stderr = output.stderr_text();
    assert_stderr_contains_all("F047", &stderr, &["❌ Failed to check staged changes."]);
}

#[test]
fn commit_to_stash_fixture_f034() {
    let repo = TempDir::new().expect("tempdir");
    git_init_repo(repo.path());

    fs::write(repo.path().join("hello.txt"), "one\n").expect("write hello");
    git_commit_all(repo.path(), "c1", "2000-01-01T00:00:00+0000");

    fs::write(repo.path().join("hello.txt"), "two\n").expect("write hello");
    git_commit_all(repo.path(), "c2", "2000-01-02T00:00:00+0000");

    let harness = GitCliHarness::new();
    let options = harness.cmd_options(repo.path()).with_stdin_str("y\nn\n");

    let output = run_with(
        &harness.git_cli_bin(),
        &["commit", "to-stash", "HEAD"],
        &options,
    );

    assert_exit_code("F034", &output, 0);
    let stdout = output.stdout_text();
    let stderr = output.stderr_text();
    assert!(
        stderr.trim().is_empty(),
        "expected F034 stderr to be empty, got:\n{stderr}"
    );
    assert_stdout_contains_all(
        "F034",
        &stdout,
        &[
            "🧾 Convert commit → stash",
            "✅ Stash created:",
            "✅ Done. Commit kept; stash saved.",
        ],
    );
}

#[test]
fn commit_to_stash_decline_prompt_aborts() {
    let repo = TempDir::new().expect("tempdir");
    git_init_repo(repo.path());

    fs::write(repo.path().join("hello.txt"), "one\n").expect("write hello");
    git_commit_all(repo.path(), "c1", "2000-01-01T00:00:00+0000");

    fs::write(repo.path().join("hello.txt"), "two\n").expect("write hello");
    git_commit_all(repo.path(), "c2", "2000-01-02T00:00:00+0000");

    let harness = GitCliHarness::new();
    let options = harness.cmd_options(repo.path()).with_stdin_str("n\n");

    let output = run_with(
        &harness.git_cli_bin(),
        &["commit", "to-stash", "HEAD"],
        &options,
    );

    assert_eq!(output.code, 1);
    assert!(
        output
            .stdout_text()
            .contains("❓ Proceed to create stash? [y/N] 🚫 Aborted"),
        "expected abort prompt"
    );
}

#[test]
fn commit_to_stash_merge_commit_prompt() {
    let repo = TempDir::new().expect("tempdir");
    git_init_repo(repo.path());

    fs::write(repo.path().join("hello.txt"), "base\n").expect("write hello");
    git_commit_all(repo.path(), "base", "2000-01-01T00:00:00+0000");

    git(repo.path(), &["checkout", "-q", "-b", "feature"]);
    fs::write(repo.path().join("hello.txt"), "feature\n").expect("write hello");
    git_commit_all(repo.path(), "feat", "2000-01-02T00:00:00+0000");

    git(repo.path(), &["checkout", "-q", "main"]);
    git_with_env(
        repo.path(),
        &[
            "-c",
            "commit.gpgsign=false",
            "merge",
            "-q",
            "--no-ff",
            "feature",
            "-m",
            "merge feature",
        ],
        &[
            ("GIT_AUTHOR_NAME", "Fixture Bot"),
            ("GIT_AUTHOR_EMAIL", "fixture@example.invalid"),
            ("GIT_COMMITTER_NAME", "Fixture Bot"),
            ("GIT_COMMITTER_EMAIL", "fixture@example.invalid"),
            ("GIT_AUTHOR_DATE", "2000-01-03T00:00:00+0000"),
            ("GIT_COMMITTER_DATE", "2000-01-03T00:00:00+0000"),
        ],
    );

    let harness = GitCliHarness::new();
    let options = harness.cmd_options(repo.path()).with_stdin_str("n\n");

    let output = run_with(
        &harness.git_cli_bin(),
        &["commit", "to-stash", "HEAD"],
        &options,
    );

    assert_eq!(output.code, 1);
    let stdout = output.stdout_text();
    assert!(stdout.contains("Target commit is a merge commit"));
    assert!(stdout.contains("❓ Proceed? [y/N] 🚫 Aborted"));
}

#[test]
fn commit_to_stash_fallback_refuses_dirty_tree() {
    let repo = TempDir::new().expect("tempdir");
    git_init_repo(repo.path());

    fs::write(repo.path().join("hello.txt"), "one\n").expect("write hello");
    git_commit_all(repo.path(), "c1", "2000-01-01T00:00:00+0000");

    fs::write(repo.path().join("hello.txt"), "two\n").expect("write hello");
    git_commit_all(repo.path(), "c2", "2000-01-02T00:00:00+0000");

    fs::write(repo.path().join("dirty.txt"), "dirty\n").expect("write dirty");

    let harness = GitCliHarness::new();
    let options = harness
        .cmd_options(repo.path())
        .with_env("GIT_CLI_FORCE_STASH_FALLBACK", "1")
        .with_stdin_str("y\ny\n");

    let output = run_with(
        &harness.git_cli_bin(),
        &["commit", "to-stash", "HEAD"],
        &options,
    );

    assert_eq!(output.code, 1);
    let stdout = output.stdout_text();
    assert!(stdout.contains("Fallback would require touching the working tree"));
    assert!(stdout.contains("Working tree is not clean; fallback requires clean state."));
}

#[test]
fn commit_to_stash_upstream_prompt_flow() {
    let repo = TempDir::new().expect("tempdir");
    git_init_repo(repo.path());

    fs::write(repo.path().join("hello.txt"), "one\n").expect("write hello");
    git_commit_all(repo.path(), "c1", "2000-01-01T00:00:00+0000");

    let remote = common::init_bare_remote();
    git(
        repo.path(),
        &[
            "remote",
            "add",
            "origin",
            remote.path().to_string_lossy().as_ref(),
        ],
    );
    git(repo.path(), &["push", "-u", "origin", "main"]);

    fs::write(repo.path().join("hello.txt"), "two\n").expect("write hello");
    git_commit_all(repo.path(), "c2", "2000-01-02T00:00:00+0000");
    git(repo.path(), &["push", "origin", "main"]);

    let harness = GitCliHarness::new();
    let options = harness.cmd_options(repo.path()).with_stdin_str("y\ny\nn\n");

    let output = run_with(
        &harness.git_cli_bin(),
        &["commit", "to-stash", "HEAD"],
        &options,
    );

    assert_exit_code("F048", &output, 0);
    let stdout = output.stdout_text();
    assert_stdout_contains_all(
        "F048",
        &stdout,
        &[
            "✅ Stash created:",
            "⚠️  This commit appears to be reachable from upstream",
            "❓ Still drop it? [y/N] ✅ Done. Commit kept; stash saved.",
        ],
    );
}
