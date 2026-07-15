use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use tempfile::TempDir;

use crate::common;

#[test]
fn ssh_exec_uses_stdin_protocol_preserves_argv_and_cleans_remote_state() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("cwd");
    let fake_peekaboo = cwd.path().join("peekaboo");
    write_executable(
        &fake_peekaboo,
        "#!/bin/sh\nprintf '%s\\n' '{\"success\":true,\"data\":{\"transport\":\"fixture\"}}'\n",
    );
    let ssh_log = cwd.path().join("ssh-argv.log");
    let remote_error_log = cwd.path().join("remote-error.log");
    let fake_ssh = cwd.path().join("ssh");
    write_executable(
        &fake_ssh,
        r#"#!/bin/sh
while [ "$1" = "-o" ]; do shift 2; done
[ "$1" = "--" ] && shift
shift
printf '%s\n' "$@" >> "$NILS_MACOS_AGENT_SSH_LOG"
shift
"$NILS_MACOS_AGENT_REMOTE_BIN" "$@" 2>> "$NILS_MACOS_AGENT_REMOTE_ERROR_LOG"
"#,
    );
    let out_dir = cwd.path().join("received-journal");
    let remote_root = cwd.path().join("remote-sessions");
    let options = harness
        .cmd_options(cwd.path())
        .with_env(
            "NILS_MACOS_AGENT_PEEKABOO_BIN",
            fake_peekaboo.to_str().expect("peekaboo"),
        )
        .with_env("NILS_MACOS_AGENT_SSH_BIN", fake_ssh.to_str().expect("ssh"))
        .with_env(
            "NILS_MACOS_AGENT_REMOTE_BIN",
            harness.macos_agent_bin().to_str().expect("agent"),
        )
        .with_env(
            "NILS_MACOS_AGENT_REMOTE_ROOT",
            remote_root.to_str().expect("remote"),
        )
        .with_env("NILS_MACOS_AGENT_SSH_LOG", ssh_log.to_str().expect("log"))
        .with_env(
            "NILS_MACOS_AGENT_REMOTE_ERROR_LOG",
            remote_error_log.to_str().expect("remote error log"),
        );
    let out = harness.run_with_options(
        cwd.path(),
        &[
            "--format",
            "json",
            "exec",
            "--host",
            "fixture-role",
            "--out-dir",
            out_dir.to_str().expect("out"),
            "--intent",
            "Unicode fixture Ω",
            "--",
            "see",
            "--app",
            "Name with spaces Ω",
            "--json",
        ],
        options,
    );
    assert_eq!(
        out.code,
        0,
        "stderr: {}; ssh argv: {}; remote stderr: {}",
        out.stderr_text(),
        fs::read_to_string(&ssh_log).unwrap_or_default(),
        fs::read_to_string(&remote_error_log).unwrap_or_default()
    );
    let payload = out.stdout_json();
    assert_eq!(payload["result"]["transport"], "ssh");
    assert_eq!(payload["result"]["upstream"]["json"]["success"], true);
    assert!(!out.stdout_text().contains("fixture-role"));
    let ssh_argv = fs::read_to_string(&ssh_log).expect("ssh log");
    assert!(ssh_argv.contains("macos-agent\n__remote\n"));
    assert!(!ssh_argv.contains("Name with spaces"));
    assert!(!ssh_argv.contains("Unicode fixture"));
    let steps = fs::read_to_string(out_dir.join("steps.jsonl")).expect("steps");
    assert!(steps.contains("Name with spaces Ω"));
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(out_dir.join("manifest.json")).expect("manifest"))
            .expect("manifest json");
    assert_eq!(manifest["transport"], "ssh");
    assert_eq!(
        fs::read_dir(&remote_root)
            .map(|rows| rows.count())
            .unwrap_or(0),
        0
    );

    let replay_plan = harness.run_with_options(
        cwd.path(),
        &[
            "--format",
            "json",
            "journal",
            "replay-plan",
            "--out-dir",
            out_dir.to_str().expect("out"),
        ],
        harness.cmd_options(cwd.path()).with_env(
            "NILS_MACOS_AGENT_PEEKABOO_BIN",
            fake_peekaboo.to_str().expect("peekaboo"),
        ),
    );
    assert_eq!(replay_plan.code, 0, "{}", replay_plan.stderr_text());
    assert_eq!(
        replay_plan.stdout_json()["result"]["steps"][0]["eligible"],
        false
    );
    assert!(
        replay_plan.stdout_json()["result"]["steps"][0]["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("remote"))
    );

    let replay = harness.run_with_options(
        cwd.path(),
        &[
            "--error-format",
            "json",
            "journal",
            "replay-step",
            "--out-dir",
            out_dir.to_str().expect("out"),
            "--step",
            "step-000001",
        ],
        harness.cmd_options(cwd.path()).with_env(
            "NILS_MACOS_AGENT_PEEKABOO_BIN",
            fake_peekaboo.to_str().expect("peekaboo"),
        ),
    );
    assert_eq!(replay.code, 78, "{}", replay.stdout_text());
    assert_eq!(replay.stderr_json()["error"]["class"], "policy");
}

#[test]
fn unsafe_host_and_ssh_failure_are_typed_without_echoing_private_identity() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("cwd");
    let unsafe_host = harness.run(
        cwd.path(),
        &[
            "--error-format",
            "json",
            "exec",
            "--host",
            "-oProxyCommand=touch-owned",
            "--out-dir",
            cwd.path().join("unsafe").to_str().expect("out"),
            "--",
            "see",
        ],
    );
    assert_eq!(unsafe_host.code, 64);
    assert_eq!(unsafe_host.stderr_json()["error"]["class"], "usage");

    let fake_ssh = cwd.path().join("ssh-fail");
    write_executable(
        &fake_ssh,
        "#!/bin/sh\nprintf 'private-user@private-host failed\\n' >&2\nexit 42\n",
    );
    let options = harness
        .cmd_options(cwd.path())
        .with_env("NILS_MACOS_AGENT_SSH_BIN", fake_ssh.to_str().expect("ssh"));
    let failed = harness.run_with_options(
        cwd.path(),
        &[
            "--error-format",
            "json",
            "exec",
            "--host",
            "private-user@private-host",
            "--out-dir",
            cwd.path().join("failed").to_str().expect("out"),
            "--",
            "see",
        ],
        options,
    );
    assert_eq!(failed.code, 75);
    let stderr = failed.stderr_text();
    assert!(!stderr.contains("private-user"));
    assert!(!stderr.contains("private-host"));
    assert_eq!(failed.stderr_json()["error"]["class"], "transport");
}

#[test]
fn ssh_scenario_stages_by_digest_retains_partial_result_and_preserves_source() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("cwd");
    let fake_peekaboo = cwd.path().join("peekaboo");
    write_executable(
        &fake_peekaboo,
        "#!/bin/sh\nprintf '%s\\n' '{\"success\":false,\"steps\":[{\"status\":\"passed\"},{\"status\":\"failed\"}]}'\nexit 8\n",
    );
    let fake_ssh = cwd.path().join("ssh");
    write_executable(
        &fake_ssh,
        r#"#!/bin/sh
while [ "$1" = "-o" ]; do shift 2; done
[ "$1" = "--" ] && shift
shift
shift
exec "$NILS_MACOS_AGENT_REMOTE_BIN" "$@"
"#,
    );
    let source = cwd.path().join("flow.peekaboo.json");
    let source_body = br#"{"steps":[{"command":"see"},{"command":"click","query":"fixture"}]}"#;
    fs::write(&source, source_body).expect("scenario");
    let out_dir = cwd.path().join("scenario-journal");
    let remote_root = cwd.path().join("scenario-remote");
    let options = harness
        .cmd_options(cwd.path())
        .with_env(
            "NILS_MACOS_AGENT_PEEKABOO_BIN",
            fake_peekaboo.to_str().expect("peekaboo"),
        )
        .with_env("NILS_MACOS_AGENT_SSH_BIN", fake_ssh.to_str().expect("ssh"))
        .with_env(
            "NILS_MACOS_AGENT_REMOTE_BIN",
            harness.macos_agent_bin().to_str().expect("agent"),
        )
        .with_env(
            "NILS_MACOS_AGENT_REMOTE_ROOT",
            remote_root.to_str().expect("remote"),
        );
    let out = harness.run_with_options(
        cwd.path(),
        &[
            "--format",
            "json",
            "scenario",
            "--host",
            "fixture-role",
            "--out-dir",
            out_dir.to_str().expect("out"),
            "--file",
            source.to_str().expect("source"),
        ],
        options,
    );
    assert_eq!(out.code, 70, "stderr: {}", out.stderr_text());
    let payload = out.stdout_json();
    assert_eq!(payload["result"]["transport"], "ssh");
    assert_eq!(
        payload["result"]["upstream"]["json"]["steps"][0]["status"],
        "passed"
    );
    assert_eq!(fs::read(&source).expect("source"), source_body);
    let steps = fs::read_to_string(out_dir.join("steps.jsonl")).expect("steps");
    assert!(steps.contains("scenario-sha256-"));
    assert!(!steps.contains(source.to_str().expect("source")));
    assert_eq!(
        fs::read_dir(&remote_root)
            .map(|entries| entries.count())
            .unwrap_or(0),
        0
    );
}

#[test]
fn local_scenario_executes_only_a_private_immutable_staged_copy() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("cwd");
    let fake_peekaboo = cwd.path().join("peekaboo");
    let launch_log = cwd.path().join("scenario-launch.log");
    write_executable(
        &fake_peekaboo,
        r#"#!/bin/sh
case "$(uname -s)" in
  Darwin) mode=$(stat -f '%Lp' "$2") || exit 91 ;;
  *) mode=$(stat -c '%a' "$2") || exit 91 ;;
esac
printf '%s|%s\n' "$2" "$mode" > "$NILS_MACOS_AGENT_SCENARIO_LAUNCH_LOG"
printf '%s\n' '{"success":true,"steps":[]}'
"#,
    );
    let source = cwd.path().join("reviewed.peekaboo.json");
    let source_body = br#"{"steps":[{"command":"see"}]}"#;
    fs::write(&source, source_body).expect("source");
    let out_dir = cwd.path().join("local-scenario-journal");
    let options = harness
        .cmd_options(cwd.path())
        .with_env(
            "NILS_MACOS_AGENT_PEEKABOO_BIN",
            fake_peekaboo.to_str().expect("fake"),
        )
        .with_env(
            "NILS_MACOS_AGENT_SCENARIO_LAUNCH_LOG",
            launch_log.to_str().expect("log"),
        );
    let out = harness.run_with_options(
        cwd.path(),
        &[
            "--format",
            "json",
            "scenario",
            "--out-dir",
            out_dir.to_str().expect("out"),
            "--file",
            source.to_str().expect("source"),
        ],
        options,
    );
    assert_eq!(out.code, 0, "{}", out.stderr_text());
    let launch = fs::read_to_string(&launch_log).expect("launch log");
    let (executed, mode) = launch.trim().rsplit_once('|').expect("path and mode");
    assert_ne!(Path::new(executed), source);
    assert_eq!(mode, "600");
    assert!(
        !Path::new(executed).exists(),
        "staged source was not removed"
    );
    assert_eq!(fs::read(&source).expect("source preserved"), source_body);
}

#[test]
fn signaled_scenario_is_an_unknown_never_replayable_mutation() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("cwd");
    let fake_peekaboo = cwd.path().join("peekaboo");
    write_executable(&fake_peekaboo, "#!/bin/sh\nkill -TERM $$\n");
    let source = cwd.path().join("signaled.peekaboo.json");
    fs::write(&source, br#"{"steps":[{"command":"see"}]}"#).expect("scenario");
    let out_dir = cwd.path().join("signaled-scenario-journal");
    let out = harness.run_with_options(
        cwd.path(),
        &[
            "--format",
            "json",
            "scenario",
            "--out-dir",
            out_dir.to_str().expect("out"),
            "--file",
            source.to_str().expect("source"),
        ],
        harness.cmd_options(cwd.path()).with_env(
            "NILS_MACOS_AGENT_PEEKABOO_BIN",
            fake_peekaboo.to_str().expect("fake"),
        ),
    );
    assert_eq!(out.code, 70);
    assert_eq!(out.stdout_json()["result"]["upstream"]["signal"], 15);
    let step: serde_json::Value = serde_json::from_str(
        fs::read_to_string(out_dir.join("steps.jsonl"))
            .expect("journal")
            .trim(),
    )
    .expect("step JSON");
    assert_eq!(step["status"], "unknown");
    assert_eq!(step["failure_class"], "unknown_mutation");
    assert_eq!(step["replay_class"], "never");
}

#[test]
fn cleanup_failure_is_typed_journaled_and_retains_remote_state_for_audit() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("cwd");
    let fake_peekaboo = cwd.path().join("peekaboo");
    write_executable(
        &fake_peekaboo,
        "#!/bin/sh\nprintf '%s\\n' '{\"success\":true}'\n",
    );
    let fake_ssh = cwd.path().join("ssh");
    write_executable(
        &fake_ssh,
        r#"#!/bin/sh
while [ "$1" = "-o" ]; do shift 2; done
[ "$1" = "--" ] && shift
shift
shift
exec "$NILS_MACOS_AGENT_REMOTE_BIN" "$@"
"#,
    );
    let out_dir = cwd.path().join("cleanup-journal");
    let remote_root = cwd.path().join("cleanup-remote");
    let options = harness
        .cmd_options(cwd.path())
        .with_env(
            "NILS_MACOS_AGENT_PEEKABOO_BIN",
            fake_peekaboo.to_str().expect("peekaboo"),
        )
        .with_env("NILS_MACOS_AGENT_SSH_BIN", fake_ssh.to_str().expect("ssh"))
        .with_env(
            "NILS_MACOS_AGENT_REMOTE_BIN",
            harness.macos_agent_bin().to_str().expect("agent"),
        )
        .with_env(
            "NILS_MACOS_AGENT_REMOTE_ROOT",
            remote_root.to_str().expect("remote"),
        )
        .with_env("NILS_MACOS_AGENT_TEST_CLEANUP_FAIL", "1");
    let out = harness.run_with_options(
        cwd.path(),
        &[
            "--error-format",
            "json",
            "exec",
            "--host",
            "fixture-role",
            "--out-dir",
            out_dir.to_str().expect("out"),
            "--",
            "see",
            "--json",
        ],
        options,
    );
    assert_eq!(out.code, 75, "stderr: {}", out.stderr_text());
    assert_eq!(out.stderr_json()["error"]["class"], "transport");
    let steps = fs::read_to_string(out_dir.join("steps.jsonl")).expect("transferred journal");
    assert!(steps.contains("remote_cleanup"), "{steps}");
    assert_eq!(
        fs::read_dir(&remote_root)
            .expect("retained remote root")
            .count(),
        1,
        "failed cleanup must not be reported as successful deletion"
    );
}

fn write_executable(path: &std::path::Path, body: &str) {
    fs::write(path, body).expect("write executable");
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("chmod executable");
}
