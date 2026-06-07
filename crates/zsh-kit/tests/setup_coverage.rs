//! Coverage for zsh-kit argument-parse handling and the higher-value `setup`
//! behaviors the happy-path suite skips: text-format rendering, destination
//! guard rails (not-git / repo-mismatch / missing-hook), `.zshenv` clobber
//! protection, the update-existing-repo path, hook-dispatch failure, and
//! home/`file://` path expansion.

use std::path::Path;

use nils_test_support::cmd::{CmdOptions, CmdOutput, run_resolved};
use nils_test_support::fs::write_executable_in_dir;
use nils_test_support::git::{InitRepoOptions, git, init_repo_at_with};
use pretty_assertions::assert_eq;
use serde_json::Value;
use std::fs;

fn run(args: &[&str]) -> CmdOutput {
    run_resolved("zsh-kit", args, &CmdOptions::new())
}

fn run_home(args: &[&str], home: &Path) -> CmdOutput {
    run_resolved(
        "zsh-kit",
        args,
        &CmdOptions::new().with_env("HOME", &home.to_string_lossy()),
    )
}

/// A git repo carrying a committed setup hook. When `hook_body` is provided it
/// replaces the default success hook (used to exercise dispatch failure).
fn fixture_repo_with_hook(root: &Path, hook_body: &str) {
    fs::create_dir_all(root).expect("repo dir");
    init_repo_at_with(
        root,
        InitRepoOptions::new()
            .with_branch("main")
            .with_initial_commit(),
    );
    write_executable_in_dir(root, "bootstrap/zsh-kit-setup.zsh", hook_body);
    git(root, &["add", "bootstrap/zsh-kit-setup.zsh"]);
    git(root, &["commit", "-m", "add zsh-kit setup hook"]);
}

fn fixture_repo(root: &Path) {
    fixture_repo_with_hook(
        root,
        "#!/usr/bin/env zsh\nset -euo pipefail\nprint -r -- \"ran\" > hook-ran.txt\n",
    );
}

fn json(output: &CmdOutput) -> Value {
    serde_json::from_str(&output.stdout_text()).expect("stdout should be json")
}

#[test]
fn unknown_subcommand_emits_json_parse_error() {
    let output = run(&["definitely-not-a-subcommand", "--format", "json"]);
    assert_eq!(output.code, 64, "stderr={}", output.stderr_text());
    let value: Value = serde_json::from_str(&output.stdout_text()).expect("json");
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"]["code"], "unknown-subcommand");
}

#[test]
fn unknown_subcommand_detects_format_equals_form() {
    let output = run(&["definitely-not-a-subcommand", "--format=json"]);
    assert_eq!(output.code, 64, "stderr={}", output.stderr_text());
    let value: Value = serde_json::from_str(&output.stdout_text()).expect("json");
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"]["code"], "unknown-subcommand");
}

#[test]
fn missing_required_repo_is_text_parse_error_by_default() {
    // `setup` requires --repo; without --format the error renders as text.
    let output = run(&["setup", "--dry-run"]);
    assert_eq!(output.code, 64, "stdout={}", output.stdout_text());
    assert!(
        output.stderr_text().contains("error:"),
        "stderr={}",
        output.stderr_text()
    );
}

#[test]
fn missing_required_repo_renders_json_parse_error_code() {
    let output = run(&["setup", "--dry-run", "--format", "json"]);
    assert_eq!(output.code, 64, "stderr={}", output.stderr_text());
    let value: Value = serde_json::from_str(&output.stdout_text()).expect("json");
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"]["code"], "parse-error");
}

#[test]
fn help_flag_exits_success() {
    let output = run(&["--help"]);
    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    assert!(output.stdout_text().contains("zsh-kit"));
}

#[test]
fn version_flag_exits_success() {
    let output = run(&["--version"]);
    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
}

// ---- setup: text-format rendering ----

#[test]
fn setup_dry_run_renders_text_summary() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = temp.path().join("repo");
    let dest = temp.path().join("dest");
    fixture_repo(&repo);

    // No --format -> default text rendering.
    let output = run_home(
        &[
            "setup",
            "--repo",
            &repo.to_string_lossy(),
            "--dest",
            &dest.to_string_lossy(),
            "--dry-run",
        ],
        temp.path(),
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let stdout = output.stdout_text();
    assert!(stdout.contains("zsh-kit setup planned"), "stdout={stdout}");
    assert!(stdout.contains("mode: dry-run"), "stdout={stdout}");
    assert!(stdout.contains("actions:"), "stdout={stdout}");
    assert!(!dest.exists(), "dry-run must not create destination");
}

#[test]
fn setup_error_renders_text_to_stderr() {
    let temp = tempfile::tempdir().expect("tempdir");
    // Credential-bearing URL fails in prepare_repo; default text format.
    let output = run_home(
        &[
            "setup",
            "--repo",
            "https://ghp_secrettoken@github.com/example/private.git",
            "--dry-run",
        ],
        temp.path(),
    );

    assert_ne!(output.code, 0);
    let stderr = output.stderr_text();
    assert!(stderr.contains("error:"), "stderr={stderr}");
    assert!(
        !stderr.contains("ghp_secrettoken"),
        "stderr must be redacted: {stderr}"
    );
}

// ---- setup: destination guard rails ----

#[test]
fn destination_that_is_not_a_repo_is_refused() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = temp.path().join("repo");
    let dest = temp.path().join("dest");
    fixture_repo(&repo);
    fs::create_dir_all(&dest).expect("dest dir"); // exists, not a git repo

    let output = run_home(
        &[
            "setup",
            "--repo",
            &repo.to_string_lossy(),
            "--dest",
            &dest.to_string_lossy(),
            "--dry-run",
            "--format",
            "json",
        ],
        temp.path(),
    );

    assert_eq!(output.code, 65, "stderr={}", output.stderr_text());
    assert_eq!(json(&output)["error"]["code"], "destination-not-git");
}

#[test]
fn destination_with_mismatched_origin_is_refused_without_force() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = temp.path().join("repo");
    let dest = temp.path().join("dest");
    fixture_repo(&repo);
    git(
        temp.path(),
        &["clone", &repo.to_string_lossy(), &dest.to_string_lossy()],
    );

    // Point --repo at a different path than the dest's origin (repo).
    let other = temp.path().join("other-repo");
    let output = run_home(
        &[
            "setup",
            "--repo",
            &other.to_string_lossy(),
            "--dest",
            &dest.to_string_lossy(),
            "--dry-run",
            "--format",
            "json",
        ],
        temp.path(),
    );

    assert_eq!(output.code, 65, "stderr={}", output.stderr_text());
    assert_eq!(json(&output)["error"]["code"], "destination-repo-mismatch");
}

#[test]
fn existing_repo_without_hook_is_refused() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = temp.path().join("repo");
    let dest = temp.path().join("dest");
    // A repo with no setup hook.
    fs::create_dir_all(&repo).expect("repo dir");
    init_repo_at_with(
        &repo,
        InitRepoOptions::new()
            .with_branch("main")
            .with_initial_commit(),
    );
    git(
        temp.path(),
        &["clone", &repo.to_string_lossy(), &dest.to_string_lossy()],
    );

    let output = run_home(
        &[
            "setup",
            "--repo",
            &repo.to_string_lossy(),
            "--dest",
            &dest.to_string_lossy(),
            "--dry-run",
            "--format",
            "json",
        ],
        temp.path(),
    );

    assert_eq!(output.code, 65, "stderr={}", output.stderr_text());
    assert_eq!(json(&output)["error"]["code"], "missing-setup-hook");
}

// ---- setup: .zshenv clobber protection ----

#[test]
fn write_zshenv_refuses_unmanaged_file_without_force() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = temp.path().join("repo");
    let dest = temp.path().join("dest");
    let home = temp.path().join("home");
    fs::create_dir_all(&home).expect("home dir");
    fs::write(home.join(".zshenv"), "export FOO=unmanaged\n").expect("seed zshenv");
    fixture_repo(&repo);

    let output = run_home(
        &[
            "setup",
            "--repo",
            &repo.to_string_lossy(),
            "--dest",
            &dest.to_string_lossy(),
            "--apply",
            "--write-zshenv",
            "--format",
            "json",
        ],
        &home,
    );

    assert_eq!(output.code, 65, "stderr={}", output.stderr_text());
    assert_eq!(json(&output)["error"]["code"], "zshenv-conflict");
    // The unmanaged file is left untouched.
    assert_eq!(
        fs::read_to_string(home.join(".zshenv")).expect("zshenv"),
        "export FOO=unmanaged\n"
    );
}

#[test]
fn write_zshenv_backs_up_unmanaged_file_with_force() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = temp.path().join("repo");
    let dest = temp.path().join("dest");
    let home = temp.path().join("home");
    fs::create_dir_all(&home).expect("home dir");
    fs::write(home.join(".zshenv"), "export FOO=unmanaged\n").expect("seed zshenv");
    fixture_repo(&repo);

    let output = run_home(
        &[
            "setup",
            "--repo",
            &repo.to_string_lossy(),
            "--dest",
            &dest.to_string_lossy(),
            "--apply",
            "--write-zshenv",
            "--force",
            "--format",
            "json",
        ],
        &home,
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    assert_eq!(
        fs::read_to_string(home.join(".zshenv.zsh-kit.bak")).expect("backup"),
        "export FOO=unmanaged\n"
    );
    assert!(
        fs::read_to_string(home.join(".zshenv"))
            .expect("zshenv")
            .contains("# Managed by zsh-kit."),
        "managed marker should be written"
    );
}

// ---- setup: update an already-cloned destination ----

#[test]
fn apply_updates_existing_clone_via_fast_forward() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = temp.path().join("repo");
    let dest = temp.path().join("dest");
    fixture_repo(&repo);
    git(
        temp.path(),
        &["clone", &repo.to_string_lossy(), &dest.to_string_lossy()],
    );

    let output = run_home(
        &[
            "setup",
            "--repo",
            &repo.to_string_lossy(),
            "--dest",
            &dest.to_string_lossy(),
            "--apply",
            "--format",
            "json",
        ],
        temp.path(),
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    assert_eq!(json(&output)["data"]["mutation_status"], "applied");
    assert!(dest.join("hook-ran.txt").is_file(), "hook should dispatch");
}

#[test]
fn apply_updates_existing_clone_on_named_branch() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = temp.path().join("repo");
    let dest = temp.path().join("dest");
    fixture_repo(&repo);
    git(
        temp.path(),
        &["clone", &repo.to_string_lossy(), &dest.to_string_lossy()],
    );

    let output = run_home(
        &[
            "setup",
            "--repo",
            &repo.to_string_lossy(),
            "--dest",
            &dest.to_string_lossy(),
            "--apply",
            "--branch",
            "main",
            "--format",
            "json",
        ],
        temp.path(),
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    assert_eq!(json(&output)["data"]["mutation_status"], "applied");
}

// ---- setup: hook dispatch failure ----

#[test]
fn apply_reports_hook_dispatch_failure() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = temp.path().join("repo");
    let dest = temp.path().join("dest");
    fixture_repo_with_hook(
        &repo,
        "#!/usr/bin/env zsh\nprint -r -- 'boom' >&2\nexit 1\n",
    );

    let output = run_home(
        &[
            "setup",
            "--repo",
            &repo.to_string_lossy(),
            "--dest",
            &dest.to_string_lossy(),
            "--apply",
            "--format",
            "json",
        ],
        temp.path(),
    );

    assert_eq!(output.code, 1, "stderr={}", output.stderr_text());
    assert_eq!(json(&output)["error"]["code"], "hook-command-failed");
}

// ---- setup: path expansion ----

#[test]
fn dest_tilde_is_expanded_against_home() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = temp.path().join("repo");
    let home = temp.path().join("home");
    fs::create_dir_all(&home).expect("home dir");
    fixture_repo(&repo);

    let output = run_home(
        &[
            "setup",
            "--repo",
            &repo.to_string_lossy(),
            "--dest",
            "~/zsh-config",
            "--dry-run",
            "--format",
            "json",
        ],
        &home,
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    assert_eq!(
        json(&output)["data"]["dest"].as_str().unwrap(),
        home.join("zsh-config").to_string_lossy()
    );
}

#[test]
fn file_url_repo_is_validated_against_local_hook() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = temp.path().join("repo");
    let dest = temp.path().join("dest");
    fixture_repo(&repo);

    let file_url = format!("file://{}", repo.to_string_lossy());
    let output = run_home(
        &[
            "setup",
            "--repo",
            &file_url,
            "--dest",
            &dest.to_string_lossy(),
            "--dry-run",
            "--format",
            "json",
        ],
        temp.path(),
    );

    // The local source hook is found via the file:// path, so the plan succeeds.
    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    assert_eq!(json(&output)["data"]["mutation_status"], "planned");
}
