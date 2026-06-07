use std::fs;
use std::io::{BufReader, Cursor};
use std::path::{Path, PathBuf};
use std::process::Command;

use nils_test_support::cmd::{self, CmdOptions, CmdOutput};
use nils_test_support::fs as test_fs;
use nils_test_support::git::{InitRepoOptions, git, init_repo_at_with};
use nils_test_support::{EnvGuard, GlobalStateLock, StubBinDir, bin, prepend_path};
use opencode_cli::agent;
use pretty_assertions::assert_eq;

fn opencode_cli_bin() -> PathBuf {
    bin::resolve("opencode-cli")
}

fn run(args: &[&str], options: &CmdOptions) -> CmdOutput {
    cmd::run_with(&opencode_cli_bin(), args, options)
}

fn clean_options() -> CmdOptions {
    CmdOptions::default()
        .with_env_remove("OPENCODE_CLI_MODEL")
        .with_env_remove("OPENCODE_CLI_VARIANT")
}

fn assert_exit(output: &CmdOutput, code: i32) {
    assert_eq!(
        output.code,
        code,
        "unexpected exit code\nstdout:\n{}\nstderr:\n{}",
        output.stdout_text(),
        output.stderr_text()
    );
}

fn write_opencode_arg_file_stub(stub_dir: &Path, out_dir: &Path) {
    fs::create_dir_all(stub_dir).expect("stub dir");
    fs::create_dir_all(out_dir).expect("out dir");
    test_fs::write_executable(
        &stub_dir.join("opencode"),
        r#"#!/usr/bin/env bash
set -euo pipefail
out_dir="${OPENCODE_STUB_OUT_DIR:?}"
mkdir -p "$out_dir"
i=0
for arg in "$@"; do
  printf '%s' "$arg" > "$out_dir/arg-$i"
  i=$((i+1))
done
exit 0
"#,
    );
}

fn read_arg(out_dir: &Path, index: usize) -> String {
    fs::read_to_string(out_dir.join(format!("arg-{index}"))).expect("read arg")
}

fn read_args(out_dir: &Path) -> Vec<String> {
    let mut args = Vec::new();
    for index in 0.. {
        let path = out_dir.join(format!("arg-{index}"));
        if !path.is_file() {
            break;
        }
        args.push(fs::read_to_string(path).expect("read arg"));
    }
    args
}

#[test]
fn agent_prompt_execs_opencode_with_expected_args() {
    let lock = GlobalStateLock::new();
    let dir = tempfile::TempDir::new().expect("tempdir");
    let stubs = StubBinDir::new();
    let out_dir = dir.path().join("out");
    write_opencode_arg_file_stub(stubs.path(), &out_dir);

    let out_dir_value = out_dir.to_string_lossy().to_string();
    let _path = prepend_path(&lock, stubs.path());
    let _model = EnvGuard::set(&lock, "OPENCODE_CLI_MODEL", "m-test");
    let _variant = EnvGuard::set(&lock, "OPENCODE_CLI_VARIANT", "v-test");
    let _out = EnvGuard::set(&lock, "OPENCODE_STUB_OUT_DIR", &out_dir_value);

    let mut stdin = BufReader::new(Cursor::new(""));
    let mut stdout: Vec<u8> = Vec::new();
    let mut stderr: Vec<u8> = Vec::new();
    let code = agent::prompt_with_io(
        &["hello".into(), "world".into()],
        &mut stdin,
        &mut stdout,
        &mut stderr,
    );

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    assert_eq!(read_arg(&out_dir, 0), "run");
    assert_eq!(read_arg(&out_dir, 1), "-m");
    assert_eq!(read_arg(&out_dir, 2), "m-test");
    assert_eq!(read_arg(&out_dir, 3), "--variant");
    assert_eq!(read_arg(&out_dir, 4), "v-test");
    assert_eq!(read_arg(&out_dir, 5), "--title");
    assert_eq!(read_arg(&out_dir, 6), "opencode-tools:prompt");
    assert_eq!(read_arg(&out_dir, 7), "--");
    assert_eq!(read_arg(&out_dir, 8), "hello world");
}

#[test]
fn agent_prompt_reads_stdin_when_no_args() {
    let lock = GlobalStateLock::new();
    let dir = tempfile::TempDir::new().expect("tempdir");
    let stubs = StubBinDir::new();
    let out_dir = dir.path().join("out");
    write_opencode_arg_file_stub(stubs.path(), &out_dir);

    let out_dir_value = out_dir.to_string_lossy().to_string();
    let _path = prepend_path(&lock, stubs.path());
    let _model = EnvGuard::remove(&lock, "OPENCODE_CLI_MODEL");
    let _variant = EnvGuard::remove(&lock, "OPENCODE_CLI_VARIANT");
    let _out = EnvGuard::set(&lock, "OPENCODE_STUB_OUT_DIR", &out_dir_value);

    let mut stdin = BufReader::new(Cursor::new("from stdin\n"));
    let mut stdout: Vec<u8> = Vec::new();
    let mut stderr: Vec<u8> = Vec::new();
    let code = agent::prompt_with_io(&[], &mut stdin, &mut stdout, &mut stderr);

    assert_eq!(code, 0);
    assert_eq!(String::from_utf8_lossy(&stdout), "Prompt: ");
    assert!(stderr.is_empty());
    assert_eq!(read_arg(&out_dir, 4), "from stdin");
}

#[test]
fn agent_advice_substitutes_arguments() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let zdotdir = dir.path().join("zdotdir");
    let prompts = zdotdir.join("prompts");
    fs::create_dir_all(&prompts).expect("prompts dir");
    fs::write(prompts.join("actionable-advice.md"), "Advice: $ARGUMENTS\n").expect("template");

    let stub_dir = dir.path().join("bin");
    let out_dir = dir.path().join("out");
    write_opencode_arg_file_stub(&stub_dir, &out_dir);

    let output = run(
        &["agent", "advice", "hello", "world"],
        &clean_options()
            .with_path_prepend(&stub_dir)
            .with_env("ZDOTDIR", &zdotdir.to_string_lossy())
            .with_env("OPENCODE_STUB_OUT_DIR", &out_dir.to_string_lossy()),
    );

    assert_exit(&output, 0);
    assert_eq!(read_arg(&out_dir, 4), "Advice: hello world\n");
}

#[test]
fn agent_knowledge_missing_template_prints_error_prefix() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let zdotdir = dir.path().join("zdotdir");
    fs::create_dir_all(zdotdir.join("prompts")).expect("prompts dir");

    let output = run(
        &["agent", "knowledge", "x"],
        &clean_options().with_env("ZDOTDIR", &zdotdir.to_string_lossy()),
    );

    assert_exit(&output, 1);
    assert!(
        output
            .stderr_text()
            .contains("opencode-tools: prompt template not found:")
    );
}

#[test]
fn agent_commit_uses_semantic_commit_prompt_and_extra_instructions() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let repo = dir.path().join("repo");
    fs::create_dir_all(&repo).expect("repo dir");
    init_repo_at_with(
        &repo,
        InitRepoOptions::new()
            .with_branch("main")
            .with_initial_commit(),
    );
    fs::write(repo.join("src.txt"), "changed\n").expect("change");
    git(&repo, &["add", "src.txt"]);

    let zdotdir = dir.path().join("zdotdir");
    let prompts = zdotdir.join("prompts");
    fs::create_dir_all(&prompts).expect("prompts dir");
    fs::write(prompts.join("semantic-commit-staged.md"), "Commit prompt\n").expect("template");

    let stub_dir = dir.path().join("bin");
    let out_dir = dir.path().join("out");
    write_opencode_arg_file_stub(&stub_dir, &out_dir);
    test_fs::write_executable(&stub_dir.join("semantic-commit"), "#!/bin/sh\nexit 0\n");

    let output = run(
        &["agent", "commit", "-p", "Prefer terse subject"],
        &clean_options()
            .with_cwd(&repo)
            .with_path_prepend(&stub_dir)
            .with_env("ZDOTDIR", &zdotdir.to_string_lossy())
            .with_env("OPENCODE_STUB_OUT_DIR", &out_dir.to_string_lossy()),
    );

    assert_exit(&output, 0);
    let args = read_args(&out_dir);
    assert!(args.contains(&"opencode-tools:commit-with-scope".to_string()));
    let prompt = args.last().expect("prompt arg");
    assert!(prompt.contains("Commit prompt"));
    assert!(prompt.contains("push the committed changes"));
    assert!(prompt.contains("Additional instructions from user:"));
    assert!(prompt.contains("Prefer terse subject"));
}

#[test]
fn main_help_unknown_and_completion_paths_are_stable() {
    let output = run(&[], &CmdOptions::default());
    assert_exit(&output, 0);
    assert!(output.stdout_text().contains("opencode-cli"));

    let output = run(&["agent"], &clean_options());
    assert_exit(&output, 0);
    assert!(output.stdout_text().contains("Agent command group"));

    let output = run(&["not-a-real-command"], &clean_options());
    assert_exit(&output, 64);
    assert!(output.stderr_text().contains("unrecognized subcommand"));

    let zsh = run(&["completion", "zsh"], &clean_options());
    assert_exit(&zsh, 0);
    assert!(zsh.stdout_text().contains("#compdef opencode-cli"));

    let bash = run(&["completion", "bash"], &clean_options());
    assert_exit(&bash, 0);
    assert!(bash.stdout_text().contains("_opencode-cli()"));
    assert!(bash.stdout_text().contains("complete -F _opencode-cli"));
}

#[test]
fn published_binary_starts_without_opencode_for_help() {
    let status = Command::new(opencode_cli_bin())
        .arg("--help")
        .status()
        .expect("run help");
    assert!(status.success());
}

// ---- coverage: zdotdir / prompt-template resolution ----

/// Build a `PATH` value with every directory that contains one of `programs`
/// removed, so the child cannot discover those binaries regardless of how the
/// developer's machine is provisioned (e.g. installed nils binaries on PATH).
fn path_excluding(programs: &[&str]) -> String {
    let paths: Vec<PathBuf> = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .filter(|dir| !programs.iter().any(|program| dir.join(program).is_file()))
        .collect();
    std::env::join_paths(paths)
        .expect("join PATH")
        .to_string_lossy()
        .to_string()
}

fn last_commit_subject(repo: &Path) -> String {
    git(repo, &["log", "-1", "--pretty=%s"]).trim().to_string()
}

fn staged_repo(dir: &Path, name: &str, contents: &str) -> PathBuf {
    let repo = dir.join("repo");
    fs::create_dir_all(&repo).expect("repo dir");
    init_repo_at_with(
        &repo,
        InitRepoOptions::new()
            .with_branch("main")
            .with_initial_commit(),
    );
    let file = repo.join(name);
    if let Some(parent) = file.parent() {
        fs::create_dir_all(parent).expect("file parent");
    }
    fs::write(&file, contents).expect("write change");
    git(&repo, &["add", name]);
    repo
}

#[test]
fn resolve_zdotdir_prefers_explicit_env() {
    let lock = GlobalStateLock::new();
    let _zdot = EnvGuard::set(&lock, "ZDOTDIR", "/explicit/zdotdir");
    assert_eq!(
        opencode_cli::paths::resolve_zdotdir(),
        Some(PathBuf::from("/explicit/zdotdir"))
    );
}

#[test]
fn resolve_zdotdir_falls_back_to_script_dir_parent() {
    let lock = GlobalStateLock::new();
    // An empty ZDOTDIR is treated as unset, exercising the script-dir fallback.
    let _zdot = EnvGuard::set(&lock, "ZDOTDIR", "");
    let _script = EnvGuard::set(&lock, "ZSH_SCRIPT_DIR", "/home/user/zsh/scripts");
    assert_eq!(
        opencode_cli::paths::resolve_zdotdir(),
        Some(PathBuf::from("/home/user/zsh"))
    );
    assert_eq!(
        opencode_cli::paths::resolve_script_dir(),
        Some(PathBuf::from("/home/user/zsh/scripts"))
    );
    assert_eq!(
        opencode_cli::paths::resolve_feature_dir(),
        Some(PathBuf::from("/home/user/zsh/scripts/_features/opencode"))
    );
}

#[test]
fn resolve_zdotdir_falls_back_to_home_config_zsh() {
    let lock = GlobalStateLock::new();
    let _zdot = EnvGuard::remove(&lock, "ZDOTDIR");
    let _script = EnvGuard::remove(&lock, "ZSH_SCRIPT_DIR");
    let _home = EnvGuard::set(&lock, "HOME", "/home/tester");
    assert_eq!(
        opencode_cli::paths::resolve_zdotdir(),
        Some(PathBuf::from("/home/tester/.config/zsh"))
    );
}

#[test]
fn resolve_paths_are_none_without_any_env() {
    let lock = GlobalStateLock::new();
    let _zdot = EnvGuard::remove(&lock, "ZDOTDIR");
    let _script = EnvGuard::remove(&lock, "ZSH_SCRIPT_DIR");
    let _home = EnvGuard::remove(&lock, "HOME");
    assert_eq!(opencode_cli::paths::resolve_zdotdir(), None);
    assert_eq!(opencode_cli::paths::resolve_script_dir(), None);
    assert_eq!(opencode_cli::paths::resolve_feature_dir(), None);
}

#[test]
fn read_template_errors_when_prompts_dir_missing() {
    let lock = GlobalStateLock::new();
    let dir = tempfile::TempDir::new().expect("tempdir");
    let _zdot = EnvGuard::set(&lock, "ZDOTDIR", &dir.path().to_string_lossy());
    let _script = EnvGuard::remove(&lock, "ZSH_SCRIPT_DIR");
    assert!(matches!(
        opencode_cli::prompts::read_template("anything"),
        Err(opencode_cli::prompts::PromptTemplateError::PromptsDirNotFound)
    ));
}

#[test]
fn read_template_reports_missing_template_file() {
    let lock = GlobalStateLock::new();
    let dir = tempfile::TempDir::new().expect("tempdir");
    fs::create_dir_all(dir.path().join("prompts")).expect("prompts dir");
    let _zdot = EnvGuard::set(&lock, "ZDOTDIR", &dir.path().to_string_lossy());
    let _script = EnvGuard::remove(&lock, "ZSH_SCRIPT_DIR");
    match opencode_cli::prompts::read_template("missing") {
        Err(opencode_cli::prompts::PromptTemplateError::TemplateMissing { path }) => {
            assert!(path.ends_with("prompts/missing.md"), "path={path:?}");
        }
        other => panic!("expected TemplateMissing, got {other:?}"),
    }
}

#[test]
fn read_template_reads_from_zdotdir_prompts() {
    let lock = GlobalStateLock::new();
    let dir = tempfile::TempDir::new().expect("tempdir");
    let prompts = dir.path().join("prompts");
    fs::create_dir_all(&prompts).expect("prompts dir");
    fs::write(prompts.join("advice.md"), "Body $ARGUMENTS\n").expect("template");
    let _zdot = EnvGuard::set(&lock, "ZDOTDIR", &dir.path().to_string_lossy());
    let (path, content) = opencode_cli::prompts::read_template("advice").expect("template");
    assert!(path.ends_with("prompts/advice.md"), "path={path:?}");
    assert_eq!(content, "Body $ARGUMENTS\n");
}

#[test]
fn read_template_uses_feature_dir_fallback_when_zdotdir_has_no_prompts() {
    let lock = GlobalStateLock::new();
    let dir = tempfile::TempDir::new().expect("tempdir");
    let zdotdir = dir.path().join("zdotdir");
    fs::create_dir_all(&zdotdir).expect("zdotdir");
    let script_dir = dir.path().join("scripts");
    let feature_prompts = script_dir.join("_features/opencode/prompts");
    fs::create_dir_all(&feature_prompts).expect("feature prompts");
    fs::write(feature_prompts.join("knowledge.md"), "Feature body\n").expect("template");
    let _zdot = EnvGuard::set(&lock, "ZDOTDIR", &zdotdir.to_string_lossy());
    let _script = EnvGuard::set(&lock, "ZSH_SCRIPT_DIR", &script_dir.to_string_lossy());
    let (path, content) = opencode_cli::prompts::read_template("knowledge").expect("template");
    assert!(
        path.ends_with("_features/opencode/prompts/knowledge.md"),
        "path={path:?}"
    );
    assert_eq!(content, "Feature body\n");
}

// ---- coverage: commit fallback flow (no semantic-commit on PATH) ----

#[test]
fn agent_commit_fallback_uses_plain_git_when_semantic_commit_missing() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let repo = staged_repo(dir.path(), "feature.txt", "added\n");

    let output = run(
        &["agent", "commit", "note-me"],
        &clean_options()
            .with_cwd(&repo)
            .with_env(
                "PATH",
                &path_excluding(&["semantic-commit", "git-scope", "opencode"]),
            )
            .with_stdin_str("feat\ncore\nadd feature\ny\n"),
    );

    assert_exit(&output, 0);
    let stderr = output.stderr_text();
    assert!(
        stderr.contains("semantic-commit not found"),
        "stderr={stderr}"
    );
    assert!(
        stderr.contains("extra prompt is ignored in fallback mode"),
        "stderr={stderr}"
    );
    assert_eq!(last_commit_subject(&repo), "feat(core): add feature");
}

#[test]
fn agent_commit_fallback_autostages_and_defaults_type_and_scope() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let repo = dir.path().join("repo");
    fs::create_dir_all(repo.join("src")).expect("src dir");
    init_repo_at_with(
        &repo,
        InitRepoOptions::new()
            .with_branch("main")
            .with_initial_commit(),
    );
    // Left unstaged on purpose: --auto-stage must `git add -A` first.
    fs::write(repo.join("src/lib.rs"), "pub fn x() {}\n").expect("change");

    let output = run(
        &["agent", "commit", "--auto-stage"],
        &clean_options()
            .with_cwd(&repo)
            .with_env(
                "PATH",
                &path_excluding(&["semantic-commit", "git-scope", "opencode"]),
            )
            // empty type -> chore, empty scope -> suggested "src".
            .with_stdin_str("\n\nadd lib\ny\n"),
    );

    assert_exit(&output, 0);
    assert_eq!(last_commit_subject(&repo), "chore(src): add lib");
}

#[test]
fn agent_commit_fallback_aborts_when_not_confirmed() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let repo = staged_repo(dir.path(), "feature.txt", "added\n");
    let head_before = git(&repo, &["rev-parse", "HEAD"]).trim().to_string();

    let output = run(
        &["agent", "commit"],
        &clean_options()
            .with_cwd(&repo)
            .with_env(
                "PATH",
                &path_excluding(&["semantic-commit", "git-scope", "opencode"]),
            )
            .with_stdin_str("feat\ncore\nsubject\nn\n"),
    );

    assert_exit(&output, 1);
    assert!(output.stderr_text().contains("Aborted."));
    assert_eq!(
        git(&repo, &["rev-parse", "HEAD"]).trim(),
        head_before,
        "an aborted commit must not advance HEAD"
    );
}

#[test]
fn agent_commit_reports_no_staged_changes() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let repo = dir.path().join("repo");
    fs::create_dir_all(&repo).expect("repo dir");
    init_repo_at_with(
        &repo,
        InitRepoOptions::new()
            .with_branch("main")
            .with_initial_commit(),
    );

    let output = run(
        &["agent", "commit"],
        &clean_options().with_cwd(&repo).with_stdin_str(""),
    );

    assert_exit(&output, 1);
    assert!(output.stderr_text().contains("no staged changes"));
}

#[test]
fn agent_commit_reports_missing_repository() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let plain = dir.path().join("plain");
    fs::create_dir_all(&plain).expect("plain dir");

    let output = run(
        &["agent", "commit"],
        &clean_options().with_cwd(&plain).with_stdin_str(""),
    );

    assert_exit(&output, 1);
    assert!(output.stderr_text().contains("not a git repository"));
}

#[test]
fn agent_commit_errors_when_prompts_dir_missing() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let repo = staged_repo(dir.path(), "feature.txt", "added\n");

    let stub_dir = dir.path().join("bin");
    fs::create_dir_all(&stub_dir).expect("stub dir");
    test_fs::write_executable(&stub_dir.join("semantic-commit"), "#!/bin/sh\nexit 0\n");

    let zdot = dir.path().join("zdot-no-prompts");
    fs::create_dir_all(&zdot).expect("zdot dir");

    let output = run(
        &["agent", "commit"],
        &clean_options()
            .with_cwd(&repo)
            .with_path_prepend(&stub_dir)
            .with_env("ZDOTDIR", &zdot.to_string_lossy())
            .with_env_remove("ZSH_SCRIPT_DIR")
            .with_stdin_str(""),
    );

    assert_exit(&output, 1);
    assert!(
        output.stderr_text().contains("prompts dir not found"),
        "stderr={}",
        output.stderr_text()
    );
}

#[test]
fn agent_commit_errors_when_prompt_template_missing() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let repo = staged_repo(dir.path(), "feature.txt", "added\n");

    let stub_dir = dir.path().join("bin");
    fs::create_dir_all(&stub_dir).expect("stub dir");
    test_fs::write_executable(&stub_dir.join("semantic-commit"), "#!/bin/sh\nexit 0\n");

    let zdot = dir.path().join("zdot");
    fs::create_dir_all(zdot.join("prompts")).expect("prompts dir without template");

    let output = run(
        &["agent", "commit"],
        &clean_options()
            .with_cwd(&repo)
            .with_path_prepend(&stub_dir)
            .with_env("ZDOTDIR", &zdot.to_string_lossy())
            .with_env_remove("ZSH_SCRIPT_DIR")
            .with_stdin_str(""),
    );

    assert_exit(&output, 1);
    assert!(
        output.stderr_text().contains("prompt template not found"),
        "stderr={}",
        output.stderr_text()
    );
}
