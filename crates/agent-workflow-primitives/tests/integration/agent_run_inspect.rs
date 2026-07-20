#![cfg(target_os = "linux")]

use std::fs::{self, OpenOptions};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use nils_common::cli_contract::exit;
use nils_test_support::cmd::{CmdOptions, CmdOutput, run_resolved};
use pretty_assertions::assert_eq;
use serde_json::Value;

fn run(args: &[&str], options: &CmdOptions) -> CmdOutput {
    run_resolved("agent-run", args, options)
}

fn arg(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

fn shell_quote(value: &Path) -> String {
    format!("'{}'", value.to_string_lossy().replace('\'', "'\\''"))
}

#[test]
fn inspect_runs_compound_reads_in_the_exact_cwd() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let cwd = temp.path().join("repo");
    fs::create_dir(&cwd).expect("repo");
    fs::write(cwd.join("input.txt"), "read only\n").expect("fixture");
    let cwd_arg = arg(&cwd);
    let output = run(
        &[
            "inspect",
            "--cwd",
            &cwd_arg,
            "--",
            "sh",
            "-c",
            "printf '%s:' \"$PWD\"; cat input.txt | tr '[:lower:]' '[:upper:]'",
        ],
        &CmdOptions::new(),
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    assert_eq!(
        output.stdout_text(),
        format!(
            "{}:READ ONLY\n",
            cwd.canonicalize().expect("canonical cwd").display()
        )
    );
}

#[test]
fn inspect_denies_writes_to_checkout_linked_git_home_and_agent_state() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let cwd = temp.path().join("repo");
    let git_dir = temp.path().join("linked-git");
    let host_home = temp.path().join("host-home");
    let agent_state = temp.path().join("agent-state");
    for directory in [&cwd, &git_dir, &host_home, &agent_state] {
        fs::create_dir(directory).expect("durable directory");
    }
    fs::write(cwd.join(".git"), format!("gitdir: {}\n", git_dir.display())).expect("gitfile");
    let blocked = [
        cwd.join("blocked"),
        git_dir.join("blocked"),
        host_home.join("blocked"),
        agent_state.join("blocked"),
    ];
    let script = format!(
        "for path in {}; do if printf changed >\"$path\" 2>/dev/null; then exit 23; fi; done; printf denied",
        blocked
            .iter()
            .map(|path| shell_quote(path))
            .collect::<Vec<_>>()
            .join(" ")
    );
    let cwd_arg = arg(&cwd);
    let output = run(
        &["inspect", "--cwd", &cwd_arg, "--", "sh", "-c", &script],
        &CmdOptions::new()
            .with_env("HOME", &arg(&host_home))
            .with_env("AGENT_HOME", &arg(&agent_state)),
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    assert_eq!(output.stdout_text(), "denied");
    for path in blocked {
        assert!(
            !path.exists(),
            "durable write escaped to {}",
            path.display()
        );
    }
}

#[test]
fn inspect_uses_private_ephemeral_roots_and_clears_credentials() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let cwd = temp.path().join("repo");
    let host_home = temp.path().join("host-home");
    let host_state = temp.path().join("host-state");
    fs::create_dir(&cwd).expect("repo");
    fs::create_dir(&host_home).expect("home");
    fs::create_dir(&host_state).expect("state");
    let cwd_arg = arg(&cwd);
    let output = run(
        &[
            "inspect",
            "--cwd",
            &cwd_arg,
            "--",
            "sh",
            "-c",
            "test -z \"${INSPECT_SECRET+x}\"; test \"$HOME\" = /run/agent-scratch/home; test \"$XDG_STATE_HOME\" = /run/agent-scratch/xdg-state; printf x >\"$HOME/file\"; printf y >\"$XDG_STATE_HOME/file\"; printf private",
        ],
        &CmdOptions::new()
            .with_env("INSPECT_SECRET", "must-not-be-inherited")
            .with_env("HOME", &arg(&host_home))
            .with_env("XDG_STATE_HOME", &arg(&host_state)),
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    assert_eq!(output.stdout_text(), "private");
    assert!(!host_home.join("file").exists());
    assert!(!host_state.join("file").exists());
}

#[test]
fn inspect_denies_host_network_even_when_a_listener_exists() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let cwd = temp.path().join("repo");
    fs::create_dir(&cwd).expect("repo");
    let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
    let port = listener.local_addr().expect("listener addr").port();
    let script = format!(
        "if exec 3<>/dev/tcp/127.0.0.1/{port}; then printf shared-network; exit 31; else printf denied; fi"
    );
    let cwd_arg = arg(&cwd);
    let output = run(
        &["inspect", "--cwd", &cwd_arg, "--", "bash", "-c", &script],
        &CmdOptions::new(),
    );

    drop(listener);
    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    assert_eq!(output.stdout_text(), "denied");
}

#[test]
fn inspect_scratch_is_new_for_every_invocation() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let cwd = temp.path().join("repo");
    fs::create_dir(&cwd).expect("repo");
    let cwd_arg = arg(&cwd);
    let first = run(
        &[
            "inspect",
            "--cwd",
            &cwd_arg,
            "--",
            "sh",
            "-c",
            "printf token >\"$HOME/token\"",
        ],
        &CmdOptions::new(),
    );
    assert_eq!(first.code, 0, "stderr={}", first.stderr_text());

    let second = run(
        &[
            "inspect",
            "--cwd",
            &cwd_arg,
            "--",
            "sh",
            "-c",
            "test ! -e \"$HOME/token\" && printf clean",
        ],
        &CmdOptions::new(),
    );
    assert_eq!(second.code, 0, "stderr={}", second.stderr_text());
    assert_eq!(second.stdout_text(), "clean");
}

#[test]
fn inspect_closes_inherited_writable_file_descriptors() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let cwd = temp.path().join("repo");
    fs::create_dir(&cwd).expect("repo");
    let marker = temp.path().join("fd-marker");
    let stdin_marker = temp.path().join("stdin-marker");
    let inherited_stdin = OpenOptions::new()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .open(&stdin_marker)
        .expect("writable stdin fixture");
    let agent_run = nils_test_support::bin::resolve("agent-run");
    let command = format!(
        "exec 9>{}; exec {} inspect --cwd {} -- sh -c 'printf stdin >&0 2>/dev/null; if printf escaped >&9 2>/dev/null; then exit 37; else printf closed; fi'",
        shell_quote(&marker),
        shell_quote(&agent_run),
        shell_quote(&cwd)
    );
    let output = Command::new("sh")
        .args(["-c", &command])
        .stdin(Stdio::from(inherited_stdin))
        .output()
        .expect("run agent-run with inherited fd");

    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "closed");
    assert_eq!(fs::read(&marker).expect("marker exists"), b"");
    assert_eq!(fs::read(&stdin_marker).expect("stdin marker exists"), b"");
}

#[test]
fn inspect_reaps_background_descendants_before_returning() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let cwd = temp.path().join("repo");
    fs::create_dir(&cwd).expect("repo");
    let cwd_arg = arg(&cwd);
    let started = Instant::now();
    let output = run(
        &[
            "inspect",
            "--cwd",
            &cwd_arg,
            "--",
            "sh",
            "-c",
            "(sleep 5) & printf contained",
        ],
        &CmdOptions::new(),
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    assert_eq!(output.stdout_text(), "contained");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "background child kept the sandbox alive: {:?}",
        started.elapsed()
    );
}

#[test]
fn inspect_rejects_missing_command_without_backend_fallback() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let cwd_arg = arg(temp.path());
    let output = run(&["inspect", "--cwd", &cwd_arg, "--"], &CmdOptions::new());

    assert_eq!(output.code, exit::USAGE);
    assert!(output.stderr_text().contains("required arguments"));
    assert!(!output.stderr_text().contains("agent-run exec"));
}

#[test]
fn inspect_enforces_the_shared_output_limit() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let cwd_arg = arg(temp.path());
    let output = run(
        &[
            "inspect",
            "--cwd",
            &cwd_arg,
            "--",
            "head",
            "-c",
            "9000000",
            "/dev/zero",
        ],
        &CmdOptions::new(),
    );

    assert_eq!(output.code, exit::SOFTWARE);
    assert!(
        output
            .stderr_text()
            .contains("sandbox-output-limit-exceeded"),
        "stderr={}",
        output.stderr_text()
    );
    assert!(output.stdout.is_empty());
}

#[test]
fn inspect_enforces_the_shared_process_limit() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let cwd_arg = arg(temp.path());
    let script = r#"import os, signal, sys, time
children = []
try:
    for _ in range(80):
        pid = os.fork()
        if pid == 0:
            time.sleep(5)
            os._exit(0)
        children.append(pid)
except OSError:
    print("bounded", end="")
finally:
    for pid in children:
        try:
            os.kill(pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
    for pid in children:
        try:
            os.waitpid(pid, 0)
        except ChildProcessError:
            pass
if len(children) >= 80:
    print("escaped", end="")
    sys.exit(41)
"#;
    let output = run(
        &[
            "inspect",
            "--cwd",
            &cwd_arg,
            "--",
            "/usr/bin/python3",
            "-c",
            script,
        ],
        &CmdOptions::new(),
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    assert_eq!(output.stdout_text(), "bounded");
}

#[test]
fn inspect_operation_effect_reports_strict_os_enforcement() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let cwd_arg = arg(temp.path());
    let output = run(
        &[
            "operation-effect",
            "--format",
            "json",
            "--",
            "inspect",
            "--cwd",
            &cwd_arg,
            "--",
            "rg",
            "TODO",
        ],
        &CmdOptions::new(),
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let value: Value = serde_json::from_str(&output.stdout_text()).expect("descriptor JSON");
    assert_eq!(value["schema_version"], "cli.agent-run.operation-effect.v1");
    assert_eq!(value["data"]["capability_class"], "os_enforced");
    assert_eq!(value["data"]["operation"], "inspect");
    assert_eq!(value["data"]["effect"], "read_only");
    assert_eq!(value["data"]["provider_effect"], "none");
    assert_eq!(
        value["data"]["os_enforcement"]["backend"]["kind"],
        "linux.bubblewrap-systemd.v1"
    );
    assert_eq!(
        value["data"]["os_enforcement"]["limits"]["wall_time_ms"],
        30_000
    );
    assert_eq!(
        value["data"]["os_enforcement"]["limits"]["process_count"],
        64
    );
    assert_eq!(
        value["data"]["os_enforcement"]["limits"]["output_bytes"],
        8 * 1024 * 1024
    );
}

#[test]
fn non_inspection_operation_effect_is_not_read_only() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let cwd_arg = arg(temp.path());
    let output = run(
        &[
            "operation-effect",
            "--format",
            "json",
            "--",
            "exec",
            "--cwd",
            &cwd_arg,
            "--",
            "rg",
            "TODO",
        ],
        &CmdOptions::new(),
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let value: Value = serde_json::from_str(&output.stdout_text()).expect("descriptor JSON");
    assert_eq!(value["data"]["capability_class"], "tool_contract");
    assert_eq!(value["data"]["effect"], "mutation");
    assert!(value["data"].get("os_enforcement").is_none());
}
