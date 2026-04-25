use crate::common;
use nils_test_support::fs as test_fs;
use pretty_assertions::assert_eq;
use std::fs;

#[test]
fn git_branch_checkout_confirmed() {
    let temp = tempfile::TempDir::new().unwrap();
    let stub = common::make_stub_dir();
    let out_dir = stub.path().join("fzf-out");
    test_fs::write_text(&out_dir.join("1.out"), "main\n");

    let git_log = temp.path().join("git.log");
    test_fs::write_text(&git_log, "");

    common::write_exe(stub.path(), "fzf", common::fzf_stub_script());
    common::write_exe(
        stub.path(),
        "git",
        r#"#!/bin/bash
set -euo pipefail
cmd="$1"; shift || true
case "$cmd" in
  rev-parse)
    if [[ "${1:-}" == "--is-inside-work-tree" ]]; then
      echo "true"
      exit 0
    fi
    ;;
  branch)
    echo "* main"
    echo "  dev"
    exit 0
    ;;
  checkout)
    echo "checkout $1" >> "${GIT_LOG:?}"
    exit 0
    ;;
esac
exit 0
"#,
    );

    let out_dir_s = out_dir.to_string_lossy().to_string();
    let git_log_s = git_log.to_string_lossy().to_string();
    let envs = [
        ("FZF_STUB_OUT_DIR", out_dir_s.as_str()),
        ("GIT_LOG", git_log_s.as_str()),
    ];

    let out = common::run_fzf_cli_with_stub_path(
        temp.path(),
        stub.path(),
        &["git-branch"],
        &envs,
        Some("y\n"),
    );
    assert_eq!(out.code, 0);
    assert!(out.stdout.contains("✅ Checked out to main"));
    let log = fs::read_to_string(&git_log).unwrap();
    assert!(log.contains("checkout main"));
}

#[test]
fn git_tag_checkout_success() {
    let temp = tempfile::TempDir::new().unwrap();
    let stub = common::make_stub_dir();
    let out_dir = stub.path().join("fzf-out");
    test_fs::write_text(&out_dir.join("1.out"), "v1.0.0\n");

    let git_log = temp.path().join("git.log");
    test_fs::write_text(&git_log, "");

    common::write_exe(stub.path(), "fzf", common::fzf_stub_script());
    common::write_exe(
        stub.path(),
        "git",
        r#"#!/bin/bash
set -euo pipefail
cmd="$1"; shift || true
case "$cmd" in
  rev-parse)
    if [[ "${1:-}" == "--is-inside-work-tree" ]]; then
      echo "true"
      exit 0
    fi
    if [[ "${1:-}" == "--verify" ]]; then
      echo "abc123"
      exit 0
    fi
    ;;
  tag)
    echo "v1.0.0"
    exit 0
    ;;
  checkout)
    echo "checkout $1" >> "${GIT_LOG:?}"
    exit 0
    ;;
esac
exit 0
"#,
    );

    let out_dir_s = out_dir.to_string_lossy().to_string();
    let git_log_s = git_log.to_string_lossy().to_string();
    let envs = [
        ("FZF_STUB_OUT_DIR", out_dir_s.as_str()),
        ("GIT_LOG", git_log_s.as_str()),
    ];

    let out = common::run_fzf_cli_with_stub_path(
        temp.path(),
        stub.path(),
        &["git-tag"],
        &envs,
        Some("y\n"),
    );
    assert_eq!(out.code, 0);
    assert!(
        out.stdout
            .contains("✅ Checked out to tag v1.0.0 (commit abc123)")
    );
    let log = fs::read_to_string(&git_log).unwrap();
    assert!(log.contains("checkout abc123"));
}

#[test]
fn git_checkout_stashes_and_retries() {
    let temp = tempfile::TempDir::new().unwrap();
    let stub = common::make_stub_dir();
    let out_dir = stub.path().join("fzf-out");
    test_fs::write_text(
        &out_dir.join("1.out"),
        "query\nabc123 01-01 00:00 User subject\n",
    );

    let checkout_count = temp.path().join("checkout.count");
    test_fs::write_text(&checkout_count, "0");

    common::write_exe(stub.path(), "fzf", common::fzf_stub_script());
    common::write_exe(
        stub.path(),
        "date",
        r#"#!/bin/bash
echo "2024-01-01_0000"
"#,
    );
    common::write_exe(
        stub.path(),
        "git",
        r#"#!/bin/bash
set -euo pipefail
cmd="$1"; shift || true
case "$cmd" in
  rev-parse)
    if [[ "${1:-}" == "--is-inside-work-tree" ]]; then
      echo "true"
      exit 0
    fi
    ;;
  log)
    for arg in "$@"; do
      if [[ "$arg" == "-1" ]]; then
        echo "Initial commit"
        exit 0
      fi
    done
    echo "abc123 01-01 00:00 User subject"
    exit 0
    ;;
  checkout)
    count_file="${CHECKOUT_COUNT:?}"
    count=$(cat "$count_file")
    count=$((count + 1))
    echo "$count" > "$count_file"
    if [[ "$count" -eq 1 ]]; then
      exit 1
    fi
    exit 0
    ;;
  stash)
    exit 0
    ;;
esac
exit 0
"#,
    );

    let out_dir_s = out_dir.to_string_lossy().to_string();
    let checkout_count_s = checkout_count.to_string_lossy().to_string();
    let envs = [
        ("FZF_STUB_OUT_DIR", out_dir_s.as_str()),
        ("CHECKOUT_COUNT", checkout_count_s.as_str()),
    ];

    let out = common::run_fzf_cli_with_stub_path(
        temp.path(),
        stub.path(),
        &["git-checkout"],
        &envs,
        Some("y\ny\n"),
    );
    assert_eq!(out.code, 0);
    assert!(out.stdout.contains("📦 Changes stashed"));
    assert!(out.stdout.contains("✅ Checked out to abc123"));
}

#[test]
fn git_status_runs_with_stubbed_fzf() {
    let temp = tempfile::TempDir::new().unwrap();
    let stub = common::make_stub_dir();
    let out_dir = stub.path().join("fzf-out");
    test_fs::write_text(&out_dir.join("1.out"), "");

    common::write_exe(stub.path(), "fzf", common::fzf_stub_script());
    common::write_exe(
        stub.path(),
        "git",
        r#"#!/bin/bash
set -euo pipefail
cmd="$1"; shift || true
case "$cmd" in
  rev-parse)
    echo "true"
    exit 0
    ;;
  status)
    echo " M file.txt"
    exit 0
    ;;
esac
exit 0
"#,
    );

    let out_dir_s = out_dir.to_string_lossy().to_string();
    let envs = [("FZF_STUB_OUT_DIR", out_dir_s.as_str())];

    let out =
        common::run_fzf_cli_with_stub_path(temp.path(), stub.path(), &["git-status"], &envs, None);
    assert_eq!(out.code, 0);
}

#[test]
fn git_commit_opens_selected_worktree_file() {
    let temp = tempfile::TempDir::new().unwrap();
    let repo_root = temp.path().join("repo");
    fs::create_dir_all(&repo_root).unwrap();
    fs::write(repo_root.join("file.txt"), "hi").unwrap();

    let stub = common::make_stub_dir();
    let out_dir = stub.path().join("fzf-out");
    test_fs::write_text(
        &out_dir.join("1.out"),
        "query\nabcdef1 01-01 00:00 User subject\n",
    );
    test_fs::write_text(&out_dir.join("2.out"), "\n[M] file.txt  [+1 / -0]\n");

    let vi_log = temp.path().join("vi.log");
    test_fs::write_text(&vi_log, "");

    common::write_exe(stub.path(), "fzf", common::fzf_stub_script());
    common::write_exe(
        stub.path(),
        "vi",
        r#"#!/bin/bash
echo "$@" >> "${VI_LOG:?}"
exit 0
"#,
    );
    common::write_exe(
        stub.path(),
        "git",
        r#"#!/bin/bash
set -euo pipefail
cmd="$1"; shift || true
case "$cmd" in
  rev-parse)
    if [[ "${1:-}" == "--is-inside-work-tree" ]]; then
      echo "true"
      exit 0
    fi
    if [[ "${1:-}" == "--show-toplevel" ]]; then
      echo "${REPO_ROOT:?}"
      exit 0
    fi
    ;;
  log)
    echo "abcdef1 01-01 00:00 User subject"
    exit 0
    ;;
  diff-tree)
    echo -e "M\tfile.txt"
    exit 0
    ;;
  show)
    echo -e "1\t0\tfile.txt"
    exit 0
    ;;
esac
exit 0
"#,
    );

    let out_dir_s = out_dir.to_string_lossy().to_string();
    let repo_root_s = repo_root.to_string_lossy().to_string();
    let vi_log_s = vi_log.to_string_lossy().to_string();
    let envs = [
        ("FZF_STUB_OUT_DIR", out_dir_s.as_str()),
        ("FZF_FILE_OPEN_WITH", "vi"),
        ("REPO_ROOT", repo_root_s.as_str()),
        ("VI_LOG", vi_log_s.as_str()),
    ];

    let out =
        common::run_fzf_cli_with_stub_path(temp.path(), stub.path(), &["git-commit"], &envs, None);
    assert_eq!(out.code, 0);
    let log = fs::read_to_string(&vi_log).unwrap();
    assert!(log.contains("file.txt"));
}
