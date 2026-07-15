use std::fs;
use std::os::unix::fs::PermissionsExt;

use tempfile::TempDir;

use crate::common;

#[test]
fn cli_errors_and_exit_classes_are_stable() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("cwd");

    let parse = harness.run(cwd.path(), &["--error-format", "json", "retired-command"]);
    assert_eq!(parse.code, 64);
    assert!(parse.stdout.is_empty());
    assert_eq!(parse.stderr_json()["error"]["class"], "usage");

    let policy = harness.run(
        cwd.path(),
        &[
            "--error-format",
            "json",
            "exec",
            "--out-dir",
            cwd.path().join("denied").to_str().expect("path"),
            "--",
            "browser",
            "status",
        ],
    );
    assert_eq!(policy.code, 78);
    assert_eq!(policy.stderr_json()["error"]["class"], "policy");
    assert!(cwd.path().join("denied/steps.jsonl").is_file());

    let missing_postcondition = harness.run(
        cwd.path(),
        &[
            "--error-format",
            "json",
            "exec",
            "--out-dir",
            cwd.path().join("mutation").to_str().expect("path"),
            "--",
            "click",
            "--id",
            "B1",
        ],
    );
    assert_eq!(missing_postcondition.code, 78);
}

#[test]
fn backend_subcommands_reject_flags_that_have_no_declared_effect() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("cwd");
    for args in [
        &["backend", "status", "--strict"][..],
        &["backend", "status", "--dry-run"][..],
        &["backend", "verify", "--dry-run"][..],
    ] {
        let out = harness.run(cwd.path(), args);
        assert_eq!(out.code, 64, "{args:?}: {}", out.stderr_text());
    }
}

#[test]
fn capabilities_publish_every_enforced_hard_denial() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("cwd");
    let out = harness.run(cwd.path(), &["--format", "json", "capabilities"]);
    assert_eq!(out.code, 0, "{}", out.stderr_text());
    let response = out.stdout_json();
    let disabled = response["result"]["disabled"]
        .as_array()
        .expect("disabled")
        .iter()
        .filter_map(serde_json::Value::as_str)
        .collect::<std::collections::BTreeSet<_>>();
    for required in [
        "agent",
        "analyze",
        "audio",
        "browser",
        "clipboard",
        "config",
        "credentials",
        "image",
        "mcp_agent",
        "permission_mutation",
        "shell",
        "http_mcp",
        "sse_mcp",
    ] {
        assert!(
            disabled.contains(required),
            "missing {required}: {disabled:?}"
        );
    }
}

#[test]
fn local_and_ssh_capabilities_publish_the_same_contract() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("cwd");
    let local = harness.run(cwd.path(), &["--format", "json", "capabilities"]);
    assert_eq!(local.code, 0, "{}", local.stderr_text());

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
    let remote_root = cwd.path().join("remote-sessions");
    let options = harness
        .cmd_options(cwd.path())
        .with_env("NILS_MACOS_AGENT_SSH_BIN", fake_ssh.to_str().expect("ssh"))
        .with_env(
            "NILS_MACOS_AGENT_REMOTE_BIN",
            harness.macos_agent_bin().to_str().expect("agent"),
        )
        .with_env(
            "NILS_MACOS_AGENT_REMOTE_ROOT",
            remote_root.to_str().expect("remote root"),
        );
    let remote = harness.run_with_options(
        cwd.path(),
        &["--format", "json", "capabilities", "--host", "fixture-role"],
        options,
    );
    assert_eq!(remote.code, 0, "{}", remote.stderr_text());
    assert_eq!(
        local.stdout_json()["result"],
        remote.stdout_json()["result"]
    );
}

#[test]
fn exec_preserves_upstream_json_and_writes_structural_journal() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("cwd");
    let fake = cwd.path().join("peekaboo");
    write_executable(
        &fake,
        "#!/bin/sh\nprintf '%s\\n' '{\"success\":true,\"data\":{\"count\":1}}'\n",
    );
    let out_dir = cwd.path().join("journal");
    let options = harness.cmd_options(cwd.path()).with_env(
        "NILS_MACOS_AGENT_PEEKABOO_BIN",
        fake.to_str().expect("fake"),
    );
    let out = harness.run_with_options(
        cwd.path(),
        &[
            "--format",
            "json",
            "exec",
            "--out-dir",
            out_dir.to_str().expect("out"),
            "--intent",
            "inspect fixture",
            "--",
            "see",
            "--app",
            "Calculator",
            "--json",
        ],
        options,
    );
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    assert!(out.stderr.is_empty());
    let payload = out.stdout_json();
    assert_eq!(payload["schema_version"], "macos-agent.adapter.v2");
    assert_eq!(payload["command"], "exec");
    assert_eq!(payload["result"]["upstream"]["json"]["success"], true);
    for required in [
        "manifest.json",
        "steps.jsonl",
        "summary.json",
        "redaction.json",
        "artifacts/index.json",
    ] {
        assert!(out_dir.join(required).is_file(), "missing {required}");
    }
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(out_dir.join("manifest.json")).expect("manifest"))
            .expect("manifest json");
    assert_eq!(manifest["schema_version"], "macos-agent.journal.v2");
    assert_eq!(manifest["transport"], "local");
}

#[test]
fn upstream_failure_remains_inside_successfully_encoded_adapter_envelope() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("cwd");
    let fake = cwd.path().join("peekaboo");
    write_executable(
        &fake,
        "#!/bin/sh\nprintf 'fixture failure\\n' >&2\nexit 9\n",
    );
    let options = harness.cmd_options(cwd.path()).with_env(
        "NILS_MACOS_AGENT_PEEKABOO_BIN",
        fake.to_str().expect("fake"),
    );
    let out = harness.run_with_options(
        cwd.path(),
        &[
            "--format",
            "json",
            "exec",
            "--out-dir",
            cwd.path().join("failed").to_str().expect("out"),
            "--",
            "see",
        ],
        options,
    );
    assert_eq!(out.code, 70);
    assert!(out.stderr.is_empty());
    let payload = out.stdout_json();
    assert_eq!(payload["ok"], true);
    assert_eq!(payload["result"]["upstream"]["exit_code"], 9);
}

#[test]
fn malformed_json_is_distinct_from_a_successful_text_command() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("cwd");
    let fake = cwd.path().join("peekaboo");
    write_executable(&fake, "#!/bin/sh\nprintf 'not-json\\n'\n");
    let options = harness.cmd_options(cwd.path()).with_env(
        "NILS_MACOS_AGENT_PEEKABOO_BIN",
        fake.to_str().expect("fake"),
    );
    let out_dir = cwd.path().join("malformed");
    let out = harness.run_with_options(
        cwd.path(),
        &[
            "--format",
            "json",
            "exec",
            "--out-dir",
            out_dir.to_str().expect("out"),
            "--",
            "see",
            "--json",
        ],
        options,
    );
    assert_eq!(out.code, 70);
    assert_eq!(out.stdout_json()["result"]["upstream"]["exit_code"], 0);
    let journal = fs::read_to_string(out_dir.join("steps.jsonl")).expect("journal");
    assert!(journal.contains("upstream_malformed_json"));
}

#[test]
fn signal_and_timeout_preserve_distinct_upstream_and_mutation_state() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("cwd");

    let signaled = cwd.path().join("peekaboo-signal");
    write_executable(&signaled, "#!/bin/sh\nkill -TERM $$\n");
    let signal_dir = cwd.path().join("signal-journal");
    let signal = harness.run_with_options(
        cwd.path(),
        &[
            "--format",
            "json",
            "exec",
            "--out-dir",
            signal_dir.to_str().expect("out"),
            "--",
            "see",
        ],
        harness.cmd_options(cwd.path()).with_env(
            "NILS_MACOS_AGENT_PEEKABOO_BIN",
            signaled.to_str().expect("signal fixture"),
        ),
    );
    assert_eq!(signal.code, 70);
    assert_eq!(signal.stdout_json()["result"]["upstream"]["signal"], 15);
    assert!(
        fs::read_to_string(signal_dir.join("steps.jsonl"))
            .expect("signal journal")
            .contains("upstream_signal")
    );

    let sleeping = cwd.path().join("peekaboo-timeout");
    write_executable(&sleeping, "#!/bin/sh\nsleep 3\n");
    let read_dir = cwd.path().join("read-timeout-journal");
    let read_timeout = harness.run_with_options(
        cwd.path(),
        &[
            "--format",
            "json",
            "exec",
            "--timeout-seconds",
            "1",
            "--out-dir",
            read_dir.to_str().expect("out"),
            "--",
            "see",
        ],
        harness.cmd_options(cwd.path()).with_env(
            "NILS_MACOS_AGENT_PEEKABOO_BIN",
            sleeping.to_str().expect("timeout fixture"),
        ),
    );
    assert_eq!(read_timeout.code, 70);
    assert_eq!(
        read_timeout.stdout_json()["result"]["upstream"]["timed_out"],
        true
    );
    assert!(
        fs::read_to_string(read_dir.join("steps.jsonl"))
            .expect("read timeout journal")
            .contains("upstream_timeout")
    );

    let mutation_dir = cwd.path().join("mutation-timeout-journal");
    let mutation_timeout = harness.run_with_options(
        cwd.path(),
        &[
            "--format",
            "json",
            "exec",
            "--timeout-seconds",
            "1",
            "--out-dir",
            mutation_dir.to_str().expect("out"),
            "--expected",
            "fixture state changed",
            "--",
            "click",
            "--id",
            "B1",
        ],
        harness.cmd_options(cwd.path()).with_env(
            "NILS_MACOS_AGENT_PEEKABOO_BIN",
            sleeping.to_str().expect("timeout fixture"),
        ),
    );
    assert_eq!(mutation_timeout.code, 70);
    let step: serde_json::Value = serde_json::from_str(
        fs::read_to_string(mutation_dir.join("steps.jsonl"))
            .expect("mutation timeout journal")
            .trim(),
    )
    .expect("step JSON");
    assert_eq!(step["status"], "unknown");
    assert_eq!(step["failure_class"], "unknown_mutation");
    assert_eq!(step["replay_class"], "never");
}

fn write_executable(path: &std::path::Path, body: &str) {
    fs::write(path, body).expect("write executable");
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("chmod executable");
}
