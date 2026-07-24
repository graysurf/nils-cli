use std::fs;
use std::path::PathBuf;

use nils_test_support::cmd::{self, CmdOptions};
use nils_test_support::fs as test_fs;
use nils_test_support::git as test_git;
use nils_test_support::{GlobalStateLock, bin};

fn codex_cli_bin() -> PathBuf {
    bin::resolve("codex-cli")
}

#[test]
fn agent_prompt_defaults_to_ephemeral_isolated_runtime_without_dangerous_gate() {
    let _lock = GlobalStateLock::new();
    let temp = tempfile::tempdir().expect("tempdir");
    let stub_dir = temp.path().join("bin");
    fs::create_dir_all(&stub_dir).expect("stub dir");
    let log = temp.path().join("codex.log");
    test_fs::write_executable(
        &stub_dir.join("codex"),
        r#"#!/bin/sh
set -eu
if [ "${1:-}" = "exec" ] && [ "${2:-}" = "--help" ]; then
  printf '%s\n' '--ignore-user-config --ignore-rules --ephemeral --skip-git-repo-check --disable'
  exit 0
fi
if [ "${1:-}" = "features" ] && [ "${2:-}" = "list" ]; then
  printf '%s\n' 'hooks plugins remote_plugin apps memories goals multi_agent workspace_dependencies shell_tool unified_exec'
  exit 0
fi
{
  printf 'HOME=%s\n' "${HOME:-}"
  printf 'CODEX_HOME=%s\n' "${CODEX_HOME:-}"
  printf 'AGENT_DOCS_HOME=%s\n' "${AGENT_DOCS_HOME:-missing}"
  printf 'AUTH_KIND=%s\n' "$(test -L "$CODEX_HOME/auth.json" && printf symlink || printf other)"
  for entry in "$CODEX_HOME"/*; do
    test -e "$entry" || test -L "$entry" || continue
    printf 'HOME_ENTRY=%s\n' "${entry##*/}"
  done
  i=0
  for arg in "$@"; do
    printf 'ARG_%s=%s\n' "$i" "$arg"
    i=$((i+1))
  done
} > "$CODEX_ISOLATION_LOG"
"#,
    );
    let real_home = temp.path().join("real-codex-home");
    fs::create_dir_all(&real_home).expect("real home");
    fs::write(real_home.join("AGENTS.md"), "must not load").expect("agents");
    fs::write(real_home.join("auth.json"), "secret-auth").expect("auth");
    let path = stub_dir.to_string_lossy().into_owned();
    let log_path = log.to_string_lossy().into_owned();
    let home = real_home.to_string_lossy().into_owned();
    let options = CmdOptions::default()
        .with_cwd(temp.path())
        .with_env("PATH", &path)
        .with_env("CODEX_HOME", &home)
        .with_env("CODEX_ISOLATION_LOG", &log_path)
        .with_env("AGENT_DOCS_HOME", "/must/not/inherit")
        .with_env_remove("CODEX_ALLOW_DANGEROUS_ENABLED");

    let output = cmd::run_with(
        &codex_cli_bin(),
        &["agent", "prompt", "hello", "world"],
        &options,
    );

    assert_eq!(
        output.code,
        0,
        "stdout={} stderr={}",
        output.stdout_text(),
        output.stderr_text()
    );
    let log = fs::read_to_string(&log).expect("isolation log");
    assert!(log.contains("AGENT_DOCS_HOME=missing"));
    assert!(log.contains("AUTH_KIND=symlink"));
    assert!(log.contains("HOME_ENTRY=auth.json"));
    assert!(!log.contains("HOME_ENTRY=AGENTS.md"));
    // The shared child-home refactor must not give the isolated prompt runtime
    // the capsule supervisor's governance projection.
    assert!(!log.contains("HOME_ENTRY=config.toml"));
    assert!(!log.contains("HOME_ENTRY=hooks.json"));
    assert!(log.contains("ARG_0=--ask-for-approval"));
    assert!(log.contains("--ignore-user-config"));
    assert!(log.contains("--ignore-rules"));
    assert!(log.contains("--ephemeral"));
    assert!(log.contains("project_doc_max_bytes=0"));
    assert!(log.contains("ARG_"));
    assert!(!log.contains("--dangerously-bypass-approvals-and-sandbox"));
    let child_home = log
        .lines()
        .find_map(|line| line.strip_prefix("CODEX_HOME="))
        .expect("child home");
    assert_ne!(child_home, home);
    assert!(!std::path::Path::new(child_home).exists());
}

#[test]
fn agent_prompt_inherited_is_explicit_and_preserves_existing_gate_and_home() {
    let _lock = GlobalStateLock::new();
    let temp = tempfile::tempdir().expect("tempdir");
    let stub_dir = temp.path().join("bin");
    fs::create_dir_all(&stub_dir).expect("stub dir");
    let log = temp.path().join("codex.log");
    test_fs::write_executable(
        &stub_dir.join("codex"),
        r#"#!/bin/sh
set -eu
printf 'CODEX_HOME=%s\n' "${CODEX_HOME:-}" > "$CODEX_ISOLATION_LOG"
for arg in "$@"; do printf 'ARG=%s\n' "$arg" >> "$CODEX_ISOLATION_LOG"; done
"#,
    );
    let home = temp.path().join("real-home");
    fs::create_dir_all(&home).expect("home");
    let options = CmdOptions::default()
        .with_cwd(temp.path())
        .with_path_prepend(&stub_dir)
        .with_env("CODEX_HOME", &home.to_string_lossy())
        .with_env("CODEX_ISOLATION_LOG", &log.to_string_lossy())
        .with_env("CODEX_ALLOW_DANGEROUS_ENABLED", "true");
    let output = cmd::run_with(
        &codex_cli_bin(),
        &["agent", "prompt", "--runtime", "inherited", "existing"],
        &options,
    );
    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let log = fs::read_to_string(log).expect("log");
    assert!(log.contains(&format!("CODEX_HOME={}", home.display())));
    assert!(log.contains("ARG=--dangerously-bypass-approvals-and-sandbox"));
    assert!(!log.contains("ARG=--ignore-user-config"));
}

#[test]
fn agent_doctor_json_is_secret_free_and_requires_no_api_call() {
    let _lock = GlobalStateLock::new();
    let temp = tempfile::tempdir().expect("tempdir");
    let stub_dir = temp.path().join("bin");
    fs::create_dir_all(&stub_dir).expect("stub dir");
    test_fs::write_executable(
        &stub_dir.join("codex"),
        r#"#!/bin/sh
set -eu
if [ "${1:-}" = "exec" ] && [ "${2:-}" = "--help" ]; then
  printf '%s\n' '--ignore-user-config --ignore-rules --ephemeral --skip-git-repo-check --disable'
  exit 0
fi
if [ "${1:-}" = "features" ] && [ "${2:-}" = "list" ]; then
  printf '%s\n' 'hooks plugins remote_plugin apps memories goals multi_agent workspace_dependencies shell_tool unified_exec'
  exit 0
fi
case " $* " in
  *" debug prompt-input "*)
    test -f "$HOME/.codex/AGENTS.md"
    test -f "$PWD/AGENTS.md"
    test -f "$CODEX_HOME/config.toml"
    test -n "${CODEX_CLI_DOCTOR_HOOK_FILE:-}"
    printf '%s\n' '[]'
    exit 0
    ;;
esac
printf '%s\n' 'unexpected model invocation' >&2
exit 91
"#,
    );
    let output = cmd::run_with(
        &codex_cli_bin(),
        &["agent", "doctor", "--format", "json"],
        &CmdOptions::default()
            .with_cwd(temp.path())
            .with_path_prepend(&stub_dir),
    );
    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let value: serde_json::Value =
        serde_json::from_str(output.stdout_text().trim()).expect("doctor JSON");
    assert_eq!(value["schema_version"], "cli.codex-cli.agent.doctor.v1");
    assert_eq!(value["data"]["ready"], true);
    assert_eq!(value["data"]["instruction_isolation"], true);
    assert_eq!(value["data"]["hook_isolation"], true);
    assert!(!output.stdout_text().contains("auth.json"));
}

#[test]
fn agent_doctor_fails_closed_on_instruction_or_hook_sentinel_leak() {
    let _lock = GlobalStateLock::new();
    let temp = tempfile::tempdir().expect("tempdir");
    let stub_dir = temp.path().join("bin");
    fs::create_dir_all(&stub_dir).expect("stub dir");
    test_fs::write_executable(
        &stub_dir.join("codex"),
        r#"#!/bin/sh
set -eu
if [ "${1:-}" = "exec" ] && [ "${2:-}" = "--help" ]; then
  printf '%s\n' '--ignore-user-config --ignore-rules --ephemeral --skip-git-repo-check --disable'
  exit 0
fi
if [ "${1:-}" = "features" ] && [ "${2:-}" = "list" ]; then
  printf '%s\n' 'hooks plugins remote_plugin apps memories goals multi_agent workspace_dependencies shell_tool unified_exec'
  exit 0
fi
case " $* " in
  *" debug prompt-input "*)
    if [ "${CODEX_TEST_LEAK:-}" = instruction ]; then
      cat "$PWD/AGENTS.md"
    else
      : > "$CODEX_CLI_DOCTOR_HOOK_FILE"
      printf '%s\n' '[]'
    fi
    exit 0
    ;;
esac
exit 91
"#,
    );
    for (leak, field) in [
        ("instruction", "instruction_isolation"),
        ("hook", "hook_isolation"),
    ] {
        let output = cmd::run_with(
            &codex_cli_bin(),
            &["agent", "doctor", "--format", "json"],
            &CmdOptions::default()
                .with_cwd(temp.path())
                .with_path_prepend(&stub_dir)
                .with_env("CODEX_TEST_LEAK", leak),
        );
        assert_eq!(
            output.code,
            1,
            "leak={leak} stderr={}",
            output.stderr_text()
        );
        let value: serde_json::Value =
            serde_json::from_str(output.stdout_text().trim()).expect("doctor JSON");
        assert_eq!(value["data"]["ready"], false);
        assert_eq!(value["data"][field], false);
    }
}

#[test]
fn isolated_agent_commit_uses_structured_model_output_and_semantic_commit_only() {
    let _lock = GlobalStateLock::new();
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).expect("repo");
    test_git::git(&repo, &["init"]);
    test_git::git(&repo, &["config", "user.name", "Test User"]);
    test_git::git(&repo, &["config", "user.email", "test@example.com"]);
    test_git::git(&repo, &["config", "commit.gpgsign", "false"]);
    fs::write(repo.join("base.txt"), "base\n").expect("base");
    test_git::git(&repo, &["add", "base.txt"]);
    test_git::git(&repo, &["commit", "-m", "chore: base"]);
    let old_head = test_git::git(&repo, &["rev-parse", "HEAD"])
        .trim()
        .to_string();
    fs::write(repo.join("change.txt"), "change\n").expect("change");
    test_git::git(&repo, &["add", "change.txt"]);

    let stub_dir = temp.path().join("bin");
    fs::create_dir_all(&stub_dir).expect("stub dir");
    let semantic_log = temp.path().join("semantic.log");
    test_fs::write_executable(
        &stub_dir.join("codex"),
        r#"#!/bin/sh
set -eu
if [ "${1:-}" = "exec" ] && [ "${2:-}" = "--help" ]; then
  printf '%s\n' '--ignore-user-config --ignore-rules --ephemeral --skip-git-repo-check --disable'
  exit 0
fi
if [ "${1:-}" = "features" ] && [ "${2:-}" = "list" ]; then
  printf '%s\n' 'hooks plugins remote_plugin apps memories goals multi_agent workspace_dependencies shell_tool unified_exec'
  exit 0
fi
last=''
for arg in "$@"; do
  if [ "$last" = '--output-last-message' ]; then
    printf '%s\n' '{"type":"fix","scope":"agent","subject":"isolate helper runtime","body_bullets":["Keep model output message-only"]}' > "$arg"
  fi
  last="$arg"
done
"#,
    );
    let real_git = nils_common::process::find_in_path("git").expect("git");
    test_fs::write_executable(
        &stub_dir.join("semantic-commit"),
        r#"#!/bin/sh
set -eu
printf '%s\n' "$*" >> "$SEMANTIC_LOG"
if [ "${1:-}" = 'staged-context' ]; then
  printf '%s\n' 'STAGED BUNDLE'
  exit 0
fi
repo=''
last=''
for arg in "$@"; do
  if [ "$last" = '--repo' ]; then repo="$arg"; fi
  last="$arg"
done
"$REAL_GIT" -C "$repo" commit -m 'fix(agent): isolate helper runtime' >/dev/null
"#,
    );
    let output = cmd::run_with(
        &codex_cli_bin(),
        &["agent", "commit"],
        &CmdOptions::default()
            .with_cwd(&repo)
            .with_path_prepend(&stub_dir)
            .with_env("SEMANTIC_LOG", &semantic_log.to_string_lossy())
            .with_env("REAL_GIT", &real_git.to_string_lossy())
            .with_env_remove("CODEX_ALLOW_DANGEROUS_ENABLED"),
    );
    assert_eq!(
        output.code,
        0,
        "stdout={} stderr={}",
        output.stdout_text(),
        output.stderr_text()
    );
    let new_head = test_git::git(&repo, &["rev-parse", "HEAD"]);
    assert_ne!(new_head.trim(), old_head);
    let log = fs::read_to_string(semantic_log).expect("semantic log");
    assert!(log.contains("staged-context --format bundle --repo"));
    assert!(log.contains("commit --type fix --scope agent --subject isolate helper runtime"));
    assert!(log.contains("--body-bullet Keep model output message-only"));
    assert!(log.contains(&format!("--expect-head {old_head}")));
}
