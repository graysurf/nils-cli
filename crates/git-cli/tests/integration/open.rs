use crate::common;
use common::{GitCliHarness, git, init_repo};
use nils_test_support::StubBinDir;
use nils_test_support::cmd::{CmdOutput, run_with};
use std::path::Path;

fn run_with_open_script(
    harness: &GitCliHarness,
    cwd: &Path,
    args: &[&str],
    open_script: &str,
) -> CmdOutput {
    let stubs = StubBinDir::new();
    stubs.write_exe("open", open_script);
    let options = harness.cmd_options(cwd).with_path_prepend(stubs.path());
    run_with(&harness.git_cli_bin(), args, &options)
}

fn run_with_open_stub(harness: &GitCliHarness, cwd: &Path, args: &[&str]) -> CmdOutput {
    run_with_open_script(
        harness,
        cwd,
        args,
        r#"#!/bin/bash
set -euo pipefail
exit 0
"#,
    )
}

#[test]
fn open_repo_opens_normalized_remote_homepage() {
    let harness = GitCliHarness::new();
    let dir = init_repo();
    git(
        dir.path(),
        &["remote", "add", "origin", "git@github.com:acme/repo.git"],
    );

    let output = run_with_open_stub(&harness, dir.path(), &["open", "repo"]);

    assert_eq!(output.code, 0);
    assert_eq!(output.stderr_text(), "");
    assert_eq!(
        output.stdout_text(),
        "🌐 Opened: https://github.com/acme/repo\n"
    );
}

#[test]
fn open_commit_opens_commit_page_for_ref() {
    let harness = GitCliHarness::new();
    let dir = init_repo();
    git(
        dir.path(),
        &["remote", "add", "origin", "git@github.com:acme/repo.git"],
    );
    let sha = git(dir.path(), &["rev-parse", "HEAD"])
        .trim_end_matches(['\n', '\r'])
        .to_string();

    let output = run_with_open_stub(&harness, dir.path(), &["open", "commit", "HEAD"]);

    assert_eq!(output.code, 0);
    assert_eq!(output.stderr_text(), "");
    assert_eq!(
        output.stdout_text(),
        format!("🔗 Opened: https://github.com/acme/repo/commit/{sha}\n")
    );
}

#[test]
fn open_file_encodes_path_spaces() {
    let harness = GitCliHarness::new();
    let dir = init_repo();
    git(
        dir.path(),
        &["remote", "add", "origin", "git@github.com:acme/repo.git"],
    );

    let output = run_with_open_stub(
        &harness,
        dir.path(),
        &["open", "file", "docs/my file.md", "main"],
    );

    assert_eq!(output.code, 0);
    assert_eq!(output.stderr_text(), "");
    assert_eq!(
        output.stdout_text(),
        "📄 Opened: https://github.com/acme/repo/blob/main/docs/my%20file.md\n"
    );
}

#[test]
fn open_leaf_help_does_not_launch_browser() {
    let harness = GitCliHarness::new();
    let dir = init_repo();
    git(
        dir.path(),
        &["remote", "add", "origin", "git@github.com:acme/repo.git"],
    );

    for args in [
        &["open", "compare", "--help"][..],
        &["open", "file", "--help"][..],
        &["open", "blame", "--help"][..],
    ] {
        let output = run_with_open_script(
            &harness,
            dir.path(),
            args,
            r#"#!/bin/bash
echo "open should not be called" >&2
exit 99
"#,
        );

        assert_eq!(output.code, 0, "args: {args:?}");
        assert_eq!(output.stderr_text(), "", "args: {args:?}");
        assert!(
            output.stdout_text().contains("Usage:\n  git-cli open"),
            "args: {args:?}, stdout: {}",
            output.stdout_text()
        );
    }
}

#[test]
fn open_actions_rejects_non_github_provider() {
    let harness = GitCliHarness::new();
    let dir = init_repo();
    git(
        dir.path(),
        &["remote", "add", "origin", "git@gitlab.com:acme/repo.git"],
    );

    let output = harness.run(dir.path(), &["open", "actions"]);

    assert_eq!(output.code, 1);
    assert_eq!(output.stdout_text(), "");
    assert_eq!(
        output.stderr_text(),
        "❗ actions is only supported for GitHub remotes.\n"
    );
}

#[test]
fn open_repo_headless_environment_prints_clear_manual_open_warning() {
    let harness = GitCliHarness::new();
    let dir = init_repo();
    git(
        dir.path(),
        &["remote", "add", "origin", "git@github.com:acme/repo.git"],
    );

    let output = run_with_open_script(
        &harness,
        dir.path(),
        &["open", "repo"],
        r#"#!/bin/bash
set -euo pipefail
echo "/usr/bin/open: 882: www-browser: not found" >&2
echo "xdg-open: no method available for opening '$1'" >&2
exit 3
"#,
    );

    assert_eq!(output.code, 0);
    assert_eq!(
        output.stdout_text(),
        "🔗 URL: https://github.com/acme/repo\n"
    );
    assert_eq!(
        output.stderr_text(),
        "⚠️  Could not launch a browser in this environment; open the URL manually.\n"
    );
}

#[test]
fn open_repo_non_headless_open_error_still_fails() {
    let harness = GitCliHarness::new();
    let dir = init_repo();
    git(
        dir.path(),
        &["remote", "add", "origin", "git@github.com:acme/repo.git"],
    );

    let output = run_with_open_script(
        &harness,
        dir.path(),
        &["open", "repo"],
        r#"#!/bin/bash
set -euo pipefail
echo "open: permission denied" >&2
exit 126
"#,
    );

    assert_eq!(output.code, 126);
    assert_eq!(output.stdout_text(), "");
    assert_eq!(output.stderr_text(), "open: permission denied\n");
}
