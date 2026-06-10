use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use nils_common::cli_contract::exit;
use nils_test_support::cmd::{CmdOptions, CmdOutput, run_resolved};
use pretty_assertions::{assert_eq, assert_ne};

fn run(args: &[&str], options: &CmdOptions) -> CmdOutput {
    run_resolved("agent-run", args, options)
}

fn arg(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

#[test]
fn exec_runs_directly_without_env_file_and_preserves_child_output() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let cwd = tmp.path().join("repo");
    fs::create_dir(&cwd).expect("repo");
    let cwd_arg = arg(&cwd);

    let output = run(
        &[
            "exec",
            "--cwd",
            &cwd_arg,
            "--",
            "sh",
            "-c",
            "printf stdout; printf stderr >&2; exit 7",
        ],
        &CmdOptions::new(),
    );

    assert_eq!(output.code, 7);
    assert_eq!(output.stdout_text(), "stdout");
    assert_eq!(output.stderr_text(), "stderr");
}

#[test]
fn require_without_env_file_fails_before_child_starts() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let cwd = tmp.path().join("repo");
    fs::create_dir(&cwd).expect("repo");
    let marker = tmp.path().join("marker");
    let cwd_arg = arg(&cwd);
    let marker_arg = arg(&marker);

    let output = run(
        &[
            "exec",
            "--direnv",
            "require",
            "--cwd",
            &cwd_arg,
            "--",
            "sh",
            "-c",
            &format!("touch {marker_arg}"),
        ],
        &CmdOptions::new(),
    );

    assert_eq!(output.code, exit::UNAVAILABLE);
    assert!(
        !marker.exists(),
        "child command must not run when required env is absent"
    );
    assert!(output.stderr_text().contains("no .envrc or .env"));
}

#[test]
fn off_mode_bypasses_direnv_even_when_env_file_exists() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let cwd = tmp.path().join("repo");
    fs::create_dir(&cwd).expect("repo");
    fs::write(cwd.join(".envrc"), "export SHOULD_NOT_LOAD=1\n").expect("envrc");
    let fake_dir = tmp.path().join("bin");
    fs::create_dir(&fake_dir).expect("bin");
    let log = tmp.path().join("direnv.log");
    write_fake_direnv(&fake_dir, FakeDirenv::Success { log: log.clone() });
    let cwd_arg = arg(&cwd);

    let output = run(
        &[
            "exec",
            "--direnv",
            "off",
            "--cwd",
            &cwd_arg,
            "--",
            "sh",
            "-c",
            "printf direct",
        ],
        &CmdOptions::new().with_path_prepend(&fake_dir),
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    assert_eq!(output.stdout_text(), "direct");
    assert_eq!(output.stderr_text(), "");
    assert!(!log.exists(), "direnv must not be invoked in off mode");
}

#[test]
fn allowed_env_file_runs_through_direnv_exec() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let cwd = tmp.path().join("repo");
    fs::create_dir(&cwd).expect("repo");
    fs::write(cwd.join(".envrc"), "export FROM_DIRENV=1\n").expect("envrc");
    let fake_dir = tmp.path().join("bin");
    fs::create_dir(&fake_dir).expect("bin");
    let log = tmp.path().join("direnv.log");
    write_fake_direnv(&fake_dir, FakeDirenv::Success { log: log.clone() });
    let cwd_arg = arg(&cwd);

    let output = run(
        &[
            "exec",
            "--cwd",
            &cwd_arg,
            "--",
            "sh",
            "-c",
            "printf $FROM_DIRENV",
        ],
        &CmdOptions::new().with_path_prepend(&fake_dir),
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    assert_eq!(output.stdout_text(), "1");
    assert_eq!(output.stderr_text(), "");
    let log_text = fs::read_to_string(log).expect("direnv log");
    assert!(log_text.contains("exec"));
    assert!(log_text.contains(&cwd_arg));
}

#[test]
fn dotenv_file_runs_with_direnv_dotenv_parser_when_status_has_no_rc() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let cwd = tmp.path().join("repo");
    fs::create_dir(&cwd).expect("repo");
    fs::write(cwd.join(".env"), "FROM_DOTENV=dotenv-value\n").expect("env");
    let fake_dir = tmp.path().join("bin");
    fs::create_dir(&fake_dir).expect("bin");
    let log = tmp.path().join("direnv.log");
    write_fake_direnv(&fake_dir, FakeDirenv::Dotenv { log: log.clone() });
    let cwd_arg = arg(&cwd);

    let output = run(
        &[
            "exec",
            "--cwd",
            &cwd_arg,
            "--",
            "sh",
            "-c",
            "printf $FROM_DOTENV",
        ],
        &CmdOptions::new().with_path_prepend(&fake_dir),
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    assert_eq!(output.stdout_text(), "dotenv-value");
    assert_eq!(output.stderr_text(), "");
    let log_text = fs::read_to_string(log).expect("direnv log");
    assert!(log_text.contains("status --json"));
    assert!(log_text.contains("dotenv json"));
    assert!(
        !log_text.contains("exec"),
        ".env fallback must not call direnv exec"
    );
}

#[test]
fn blocked_env_file_fails_without_running_child() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let cwd = tmp.path().join("repo");
    fs::create_dir(&cwd).expect("repo");
    fs::write(cwd.join(".envrc"), "export BLOCKED=1\n").expect("envrc");
    let marker = tmp.path().join("marker");
    let fake_dir = tmp.path().join("bin");
    fs::create_dir(&fake_dir).expect("bin");
    write_fake_direnv(&fake_dir, FakeDirenv::Blocked);
    let cwd_arg = arg(&cwd);
    let marker_arg = arg(&marker);

    let output = run(
        &[
            "exec",
            "--cwd",
            &cwd_arg,
            "--",
            "sh",
            "-c",
            &format!("touch {marker_arg}"),
        ],
        &CmdOptions::new().with_path_prepend(&fake_dir),
    );

    assert_eq!(output.code, exit::UNAVAILABLE);
    assert!(!marker.exists(), "blocked envrc must stop before child");
    assert!(output.stderr_text().contains(".envrc"));
}

#[test]
fn env_json_reports_status_without_environment_diff() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let cwd = tmp.path().join("repo");
    fs::create_dir(&cwd).expect("repo");
    fs::write(cwd.join(".envrc"), "export SECRET_VALUE=hidden\n").expect("envrc");
    let fake_dir = tmp.path().join("bin");
    fs::create_dir(&fake_dir).expect("bin");
    let log = tmp.path().join("direnv.log");
    write_fake_direnv(&fake_dir, FakeDirenv::Success { log: log.clone() });
    let cwd_arg = arg(&cwd);

    let output = run(
        &["env", "--cwd", &cwd_arg, "--format", "json"],
        &CmdOptions::new().with_path_prepend(&fake_dir),
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let value = output.stdout_json();
    assert_eq!(value["schema_version"], "cli.agent-run.env.v1");
    assert_eq!(value["data"]["schema"], "agent-run.env.v1");
    assert_eq!(value["data"]["env_file"]["kind"], ".envrc");
    assert_eq!(value["data"]["decision"]["kind"], "direnv");
    assert_eq!(value["data"]["decision"]["status"], "active");
    let rendered = output.stdout_text();
    assert_ne!(
        rendered.contains("SECRET_VALUE"),
        true,
        "env JSON must not include an environment diff or values"
    );
    let log_text = fs::read_to_string(log).expect("direnv log");
    assert!(log_text.contains("status --json"));
    assert!(
        !log_text.contains("exec"),
        "env status must not execute direnv env loading"
    );
}

#[test]
fn env_json_reports_dotenv_status_without_environment_diff() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let cwd = tmp.path().join("repo");
    fs::create_dir(&cwd).expect("repo");
    fs::write(cwd.join(".env"), "SECRET_VALUE=hidden\n").expect("env");
    let fake_dir = tmp.path().join("bin");
    fs::create_dir(&fake_dir).expect("bin");
    let log = tmp.path().join("direnv.log");
    write_fake_direnv(&fake_dir, FakeDirenv::Dotenv { log: log.clone() });
    let cwd_arg = arg(&cwd);

    let output = run(
        &["env", "--cwd", &cwd_arg, "--format", "json"],
        &CmdOptions::new().with_path_prepend(&fake_dir),
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let value = output.stdout_json();
    assert_eq!(value["schema_version"], "cli.agent-run.env.v1");
    assert_eq!(value["data"]["env_file"]["kind"], ".env");
    assert_eq!(value["data"]["decision"]["kind"], "direnv-dotenv");
    assert_eq!(value["data"]["decision"]["status"], "active");
    let rendered = output.stdout_text();
    assert_ne!(
        rendered.contains("SECRET_VALUE"),
        true,
        "env JSON must not include an environment diff or values"
    );
    let log_text = fs::read_to_string(log).expect("direnv log");
    assert!(log_text.contains("status --json"));
    assert!(
        !log_text.contains("dotenv json"),
        "env status must not parse or emit .env values"
    );
    assert!(
        !log_text.contains("exec"),
        "env status must not execute direnv env loading"
    );
}

#[test]
fn doctor_json_reports_missing_direnv_when_env_file_applies() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let cwd = tmp.path().join("repo");
    fs::create_dir(&cwd).expect("repo");
    fs::write(cwd.join(".envrc"), "export NEEDS_DIRENV=1\n").expect("envrc");
    let empty_path = tmp.path().join("empty-bin");
    fs::create_dir(&empty_path).expect("empty path");
    let cwd_arg = arg(&cwd);
    let path_value = arg(&empty_path);

    let output = run(
        &["doctor", "--cwd", &cwd_arg, "--format", "json"],
        &CmdOptions::new().with_env("PATH", &path_value),
    );

    assert_eq!(output.code, 0, "doctor reports status without failing");
    let value = output.stdout_json();
    assert_eq!(value["schema_version"], "cli.agent-run.doctor.v1");
    assert_eq!(value["data"]["schema"], "agent-run.doctor.v1");
    assert_eq!(value["data"]["direnv"]["available"], false);
    assert_eq!(value["data"]["decision"]["status"], "missing-direnv");
}

enum FakeDirenv {
    Success { log: PathBuf },
    Dotenv { log: PathBuf },
    Blocked,
}

fn write_fake_direnv(dir: &Path, mode: FakeDirenv) {
    let script = dir.join("direnv");
    let body = match mode {
        FakeDirenv::Success { log } => format!(
            r#"#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> {}
if [[ "${{1:-}}" == "exec" ]]; then
  shift
  _cwd="${{1:-}}"
  shift
  export FROM_DIRENV=1
  exec "$@"
fi
if [[ "${{1:-}}" == "status" ]]; then
  printf '{{"state":{{"foundRC":{{"path":"fake","allowed":0}}}}}}\n'
  exit 0
fi
	exit 2
	"#,
            shell_quote(&log)
        ),
        FakeDirenv::Dotenv { log } => format!(
            r#"#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> {}
if [[ "${{1:-}}" == "status" ]]; then
  printf '{{"state":{{"foundRC":null,"loadedRC":null}}}}\n'
  exit 0
fi
if [[ "${{1:-}}" == "dotenv" && "${{2:-}}" == "json" ]]; then
  printf '{{"FROM_DOTENV":"dotenv-value"}}\n'
  exit 0
fi
if [[ "${{1:-}}" == "exec" ]]; then
  printf 'direnv exec should not run for dotenv fallback\n' >&2
  exit 12
fi
exit 2
"#,
            shell_quote(&log)
        ),
        FakeDirenv::Blocked => r#"#!/usr/bin/env bash
set -euo pipefail
if [[ "${1:-}" == "exec" ]]; then
  printf 'direnv: error .envrc is blocked\n' >&2
  exit 1
fi
if [[ "${1:-}" == "status" ]]; then
  printf '{"state":{"foundRC":{"path":"fake","allowed":1}}}\n'
  exit 0
fi
exit 2
"#
        .to_string(),
    };
    fs::write(&script, body).expect("fake direnv");
    let mut perms = fs::metadata(&script).expect("metadata").permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script, perms).expect("chmod");
}

fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}
