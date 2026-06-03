use std::fs;
use std::path::Path;

use nils_test_support::cmd::{CmdOptions, run_resolved};
use nils_test_support::fs::{write_executable_in_dir, write_text_in_dir};
use nils_test_support::git::{InitRepoOptions, git, init_repo_at_with};
use pretty_assertions::assert_eq;
use serde_json::Value;

fn fixture_repo(root: &Path) {
    fs::create_dir_all(root).expect("repo dir");
    init_repo_at_with(
        root,
        InitRepoOptions::new()
            .with_branch("main")
            .with_initial_commit(),
    );
    write_executable_in_dir(
        root,
        "bootstrap/zsh-kit-setup.zsh",
        r#"#!/usr/bin/env zsh
set -euo pipefail
print -r -- "$*" > hook-args.txt
print -r -- "ran" > hook-ran.txt
"#,
    );
    git(root, &["add", "bootstrap/zsh-kit-setup.zsh"]);
    git(root, &["commit", "-m", "add zsh-kit setup hook"]);
}

fn run_zsh_kit(args: &[&str], home: &Path) -> nils_test_support::cmd::CmdOutput {
    run_resolved(
        "zsh-kit",
        args,
        &CmdOptions::new().with_env("HOME", &home.to_string_lossy()),
    )
}

#[test]
fn dry_run_reports_plan_without_mutating_destination() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = temp.path().join("repo");
    let dest = temp.path().join("dest");
    fixture_repo(&repo);

    let output = run_zsh_kit(
        &[
            "setup",
            "--repo",
            &repo.to_string_lossy(),
            "--dest",
            &dest.to_string_lossy(),
            "--dry-run",
            "--features",
            "core,tools",
            "--format",
            "json",
        ],
        temp.path(),
    );

    assert_eq!(output.code, 0, "stderr: {}", output.stderr_text());
    assert!(!dest.exists(), "dry-run must not create destination");
    let json: Value = serde_json::from_str(&output.stdout_text()).expect("json");
    assert_eq!(json["schema_version"], "cli.zsh-kit.setup.v1");
    assert_eq!(json["ok"], true);
    assert_eq!(json["data"]["mode"], "dry-run");
    assert_eq!(json["data"]["mutation_status"], "planned");
    assert_eq!(
        json["data"]["features"],
        serde_json::json!(["core", "tools"])
    );
    assert_eq!(json["data"]["changed_paths"], serde_json::json!([]));
}

#[test]
fn apply_clones_repo_and_dispatches_hook() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = temp.path().join("repo");
    let dest = temp.path().join("dest");
    fixture_repo(&repo);

    let output = run_zsh_kit(
        &[
            "setup",
            "--repo",
            &repo.to_string_lossy(),
            "--dest",
            &dest.to_string_lossy(),
            "--apply",
            "--features",
            "core",
            "--install-tools",
            "repo",
            "--format",
            "json",
        ],
        temp.path(),
    );

    assert_eq!(output.code, 0, "stderr: {}", output.stderr_text());
    assert!(dest.join("bootstrap/zsh-kit-setup.zsh").is_file());
    assert_eq!(
        fs::read_to_string(dest.join("hook-ran.txt")).unwrap(),
        "ran\n"
    );
    assert_eq!(
        fs::read_to_string(dest.join("hook-args.txt")).unwrap(),
        "--features core --install-tools repo\n"
    );
    let json: Value = serde_json::from_str(&output.stdout_text()).expect("json");
    assert_eq!(json["data"]["mode"], "apply");
    assert_eq!(json["data"]["mutation_status"], "applied");
    assert!(
        json["data"]["hook_path"]
            .as_str()
            .unwrap()
            .contains("zsh-kit-setup.zsh")
    );
}

#[test]
fn missing_hook_refuses_local_source_before_clone() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).expect("repo dir");
    init_repo_at_with(
        &repo,
        InitRepoOptions::new()
            .with_branch("main")
            .with_initial_commit(),
    );
    let dest = temp.path().join("dest");

    let output = run_zsh_kit(
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

    assert_eq!(output.code, 65);
    assert!(!dest.exists(), "refusal must happen before clone");
    assert!(!output.stdout_text().contains("ghp_"));
    let json: Value = serde_json::from_str(&output.stdout_text()).expect("json");
    assert_eq!(json["error"]["code"], "missing-setup-hook");
}

#[test]
fn dirty_destination_refuses_without_force() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = temp.path().join("repo");
    let dest = temp.path().join("dest");
    fixture_repo(&repo);
    git(
        temp.path(),
        &["clone", &repo.to_string_lossy(), &dest.to_string_lossy()],
    );
    write_text_in_dir(&dest, "dirty.txt", "dirty\n");

    let output = run_zsh_kit(
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

    assert_eq!(output.code, 65);
    let json: Value = serde_json::from_str(&output.stdout_text()).expect("json");
    assert_eq!(json["error"]["code"], "destination-dirty");
}

#[test]
fn path_conflict_refuses() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = temp.path().join("repo");
    let dest = temp.path().join("dest-file");
    fixture_repo(&repo);
    fs::write(&dest, "not a dir").expect("dest file");

    let output = run_zsh_kit(
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

    assert_eq!(output.code, 65);
    let json: Value = serde_json::from_str(&output.stdout_text()).expect("json");
    assert_eq!(json["error"]["code"], "destination-conflict");
}

#[test]
fn credential_url_refuses_and_redacts() {
    let temp = tempfile::tempdir().expect("tempdir");
    let output = run_zsh_kit(
        &[
            "setup",
            "--repo",
            "https://ghp_abcdefghijklmnop@github.com/example/private.git",
            "--dry-run",
            "--format",
            "json",
        ],
        temp.path(),
    );

    assert_eq!(output.code, 65);
    assert!(!output.stdout_text().contains("ghp_abcdefghijklmnop"));
    let json: Value = serde_json::from_str(&output.stdout_text()).expect("json");
    assert_eq!(json["error"]["code"], "credential-bearing-repo-url");
}

#[test]
fn ssh_url_userinfo_is_not_treated_as_embedded_credentials() {
    let temp = tempfile::tempdir().expect("tempdir");
    let output = run_zsh_kit(
        &[
            "setup",
            "--repo",
            "ssh://git@github.com/example/private.git",
            "--dry-run",
            "--format",
            "json",
        ],
        temp.path(),
    );

    assert_eq!(output.code, 0, "stderr: {}", output.stderr_text());
    let json: Value = serde_json::from_str(&output.stdout_text()).expect("json");
    assert_eq!(
        json["data"]["repo"],
        "ssh://git@github.com/example/private.git"
    );
    assert_eq!(json["data"]["mutation_status"], "planned");
}

#[test]
fn completion_export_contains_setup_flags() {
    let output = run_resolved("zsh-kit", &["completion", "bash"], &CmdOptions::new());

    assert_eq!(output.code, 0, "stderr: {}", output.stderr_text());
    let stdout = output.stdout_text();
    assert!(stdout.contains("--repo"));
    assert!(stdout.contains("--dry-run"));
    assert!(stdout.contains("--apply"));
}
