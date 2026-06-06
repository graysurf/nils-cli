use crate::common;
use std::fs;
use std::path::Path;
use tempfile::TempDir;

fn touch(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

#[test]
fn open_changed_files_dry_run_groups_by_git_workspace() {
    let temp = TempDir::new().unwrap();
    let pwd_workspace = temp.path().join("pwd-workspace");
    fs::create_dir_all(&pwd_workspace).unwrap();

    let git1 = temp.path().join("git1");
    let git2 = temp.path().join("git2");
    fs::create_dir_all(git1.join(".git")).unwrap();
    fs::create_dir_all(git2.join(".git")).unwrap();

    let file1 = git1.join("sub/a.txt");
    let file2 = git2.join("sub/b.txt");
    touch(&file1, "a");
    touch(&file2, "b");

    let file1_s = file1.to_string_lossy().to_string();
    let file2_s = file2.to_string_lossy().to_string();
    let envs = [("OPEN_CHANGED_FILES_CODE_PATH", "code")];
    let out = common::run_fzf_cli(
        &pwd_workspace,
        &[
            "open-changed-files",
            "--dry-run",
            "--workspace-mode",
            "git",
            &file1_s,
            &file2_s,
        ],
        &envs,
        None,
    );

    assert_eq!(out.code, 0);
    assert!(out.stderr.is_empty(), "unexpected stderr: {}", out.stderr);
    let invocations: Vec<&str> = out.stdout.lines().collect();
    assert_eq!(invocations.len(), 2, "stdout was:\n{}", out.stdout);
    assert!(invocations[0].contains("--new-window"));
    assert!(invocations[0].contains(&git1.to_string_lossy().to_string()));
    assert!(invocations[0].contains(&file1.to_string_lossy().to_string()));
    assert!(invocations[1].contains("--new-window"));
    assert!(invocations[1].contains(&git2.to_string_lossy().to_string()));
    assert!(invocations[1].contains(&file2.to_string_lossy().to_string()));
}

#[test]
fn open_changed_files_code_disabled_is_silent_noop() {
    let temp = TempDir::new().unwrap();
    let file = temp.path().join("file.txt");
    touch(&file, "one");

    let file_s = file.to_string_lossy().to_string();
    let out = common::run_fzf_cli(
        temp.path(),
        &["open-changed-files", &file_s],
        &[("OPEN_CHANGED_FILES_CODE_PATH", "none")],
        None,
    );

    assert_eq!(out.code, 0);
    assert_eq!(out.stdout, "");
    assert_eq!(out.stderr, "");
}

#[test]
fn open_changed_files_invalid_code_override_logs_when_verbose() {
    let temp = TempDir::new().unwrap();
    let file = temp.path().join("file.txt");
    let missing_code = temp.path().join("missing-code");
    touch(&file, "one");

    let file_s = file.to_string_lossy().to_string();
    let missing_code_s = missing_code.to_string_lossy().to_string();
    let out = common::run_fzf_cli(
        temp.path(),
        &["open-changed-files", "--verbose", &file_s],
        &[("OPEN_CHANGED_FILES_CODE_PATH", &missing_code_s)],
        None,
    );

    assert_eq!(out.code, 0);
    assert_eq!(out.stdout, "");
    assert!(
        out.stderr.contains("no-op: code override not found:"),
        "stderr was: {}",
        out.stderr
    );
}

#[test]
fn open_changed_files_invokes_code_in_batches() {
    let temp = TempDir::new().unwrap();
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).unwrap();

    let stub = common::make_stub_dir();
    let code_log = temp.path().join("code.log");
    fs::write(&code_log, "").unwrap();
    common::write_exe(
        stub.path(),
        "code",
        r#"#!/bin/bash
set -euo pipefail
printf '%s\n' "$*" >> "${CODE_LOG:?}"
"#,
    );

    let mut args = vec![
        "open-changed-files".to_string(),
        "--max-files".to_string(),
        "55".to_string(),
    ];
    for i in 1..=55 {
        let file = temp.path().join(format!("many/{i}.txt"));
        touch(&file, &i.to_string());
        args.push(file.to_string_lossy().to_string());
    }
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let code_log_s = code_log.to_string_lossy().to_string();

    let out = common::run_fzf_cli_with_stub_path(
        &workspace,
        stub.path(),
        &arg_refs,
        &[
            ("OPEN_CHANGED_FILES_CODE_PATH", "code"),
            ("CODE_LOG", &code_log_s),
        ],
        None,
    );

    assert_eq!(out.code, 0);
    assert_eq!(out.stdout, "");
    assert_eq!(out.stderr, "");
    let log = fs::read_to_string(&code_log).unwrap();
    let invocations: Vec<&str> = log.lines().collect();
    assert_eq!(invocations.len(), 2, "code log was:\n{log}");
    assert!(invocations[0].contains("--new-window"));
    assert!(invocations[0].contains(&workspace.to_string_lossy().to_string()));
    assert!(invocations[1].contains("--reuse-window"));
    assert!(invocations[1].contains(&workspace.to_string_lossy().to_string()));
}

#[test]
fn open_changed_files_git_source_collects_staged_unstaged_and_untracked() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();

    let staged = repo.join("staged.txt");
    let unstaged = repo.join("unstaged.txt");
    let untracked = repo.join("untracked.txt");
    touch(&staged, "staged");
    touch(&unstaged, "unstaged");
    touch(&untracked, "untracked");

    let stub = common::make_stub_dir();
    common::write_exe(
        stub.path(),
        "git",
        r#"#!/bin/bash
set -euo pipefail
case "$*" in
  "rev-parse --is-inside-work-tree")
    echo true
    ;;
  "rev-parse --show-toplevel")
    echo "${REPO_ROOT:?}"
    ;;
  "diff --name-only --cached")
    echo staged.txt
    ;;
  "diff --name-only")
    echo unstaged.txt
    ;;
  "ls-files --others --exclude-standard")
    echo untracked.txt
    ;;
  *)
    exit 1
    ;;
esac
"#,
    );

    let repo_s = repo.to_string_lossy().to_string();
    let out = common::run_fzf_cli_with_stub_path(
        &repo,
        stub.path(),
        &["open-changed-files", "--git", "--dry-run"],
        &[
            ("OPEN_CHANGED_FILES_CODE_PATH", "code"),
            ("REPO_ROOT", &repo_s),
        ],
        None,
    );

    assert_eq!(out.code, 0);
    assert!(out.stderr.is_empty(), "unexpected stderr: {}", out.stderr);
    assert!(out.stdout.contains(&staged.to_string_lossy().to_string()));
    assert!(out.stdout.contains(&unstaged.to_string_lossy().to_string()));
    assert!(
        out.stdout
            .contains(&untracked.to_string_lossy().to_string())
    );
}
