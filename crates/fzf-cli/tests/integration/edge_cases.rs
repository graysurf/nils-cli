use crate::common;
use std::fs;

use pretty_assertions::assert_eq;

#[test]
fn file_unknown_flag_exits_2() {
    let temp = tempfile::TempDir::new().unwrap();
    let out = common::run_fzf_cli(temp.path(), &["file", "--nope"], &[], None);
    assert_eq!(out.code, 2);
    assert!(
        out.stderr.contains("❌ Unknown flag: --nope"),
        "missing error: {}",
        out.stderr
    );
}

#[test]
fn file_mutually_exclusive_flags_exit_2() {
    let temp = tempfile::TempDir::new().unwrap();
    let out = common::run_fzf_cli(temp.path(), &["file", "--vi", "--vscode"], &[], None);
    assert_eq!(out.code, 2);
    assert!(
        out.stderr
            .contains("❌ Flags are mutually exclusive: --vi and --vscode"),
        "missing mutual exclusion error: {}",
        out.stderr
    );
}

#[test]
fn env_requires_delimiters() {
    let temp = tempfile::TempDir::new().unwrap();
    let envs = [("FZF_DEF_DELIM", ""), ("FZF_DEF_DELIM_END", "")];
    let out = common::run_fzf_cli(temp.path(), &["env"], &envs, None);
    assert_eq!(out.code, 1);
    assert!(
        out.stdout
            .contains("❌ Error: FZF_DEF_DELIM or FZF_DEF_DELIM_END is not set."),
        "missing delimiter error: {}",
        out.stdout
    );
    assert!(
        out.stdout
            .contains("💡 Please export FZF_DEF_DELIM and FZF_DEF_DELIM_END before running."),
        "missing delimiter help: {}",
        out.stdout
    );
}

#[test]
fn history_parsing_strips_icon_prefix() {
    let dir = tempfile::TempDir::new().unwrap();

    let hist = dir.path().join("histfile");
    fs::write(
        &hist,
        ": 1700000000:0;   \n: 1700000001:0;!!!\n: 1700000002:0;🧪   echo hi\n",
    )
    .unwrap();

    let stub = common::make_stub_dir();
    let out_dir = stub.path().join("fzf-out");
    fs::create_dir_all(&out_dir).unwrap();
    fs::write(
        out_dir.join("1.out"),
        "enter\n1700000002 |    3 | 🧪   echo hi\n",
    )
    .unwrap();

    common::write_exe(stub.path(), "fzf", common::fzf_stub_script());

    let hist_s = hist.to_string_lossy().to_string();
    let out_dir_s = out_dir.to_string_lossy().to_string();
    let envs = [
        ("HISTFILE", hist_s.as_str()),
        ("FZF_STUB_OUT_DIR", out_dir_s.as_str()),
    ];

    let out =
        common::run_fzf_cli_with_stub_path(dir.path(), stub.path(), &["history"], &envs, None);
    assert_eq!(out.code, 0);
    assert_eq!(out.stdout.trim(), "echo hi");
}

#[test]
fn directory_ctrl_d_emits_cd_command() {
    let dir = tempfile::TempDir::new().unwrap();
    fs::create_dir_all(dir.path().join("subdir")).unwrap();
    fs::write(dir.path().join("subdir/file.txt"), "hi").unwrap();

    let stub = common::make_stub_dir();
    let out_dir = stub.path().join("fzf-out");
    fs::create_dir_all(&out_dir).unwrap();
    fs::write(out_dir.join("1.out"), "\nsubdir\n").unwrap();
    fs::write(out_dir.join("2.out"), "ctrl-d\n").unwrap();

    common::write_exe(stub.path(), "fzf", common::fzf_stub_script());

    let out_dir_s = out_dir.to_string_lossy().to_string();
    let envs = [("FZF_STUB_OUT_DIR", out_dir_s.as_str())];

    let out =
        common::run_fzf_cli_with_stub_path(dir.path(), stub.path(), &["directory"], &envs, None);
    assert_eq!(out.code, 0);
    assert!(
        out.stdout.contains("cd "),
        "expected cd output, got: {}",
        out.stdout
    );
    assert!(
        out.stdout.contains("subdir"),
        "expected subdir in cd output, got: {}",
        out.stdout
    );
}

#[test]
fn process_kill_now_calls_kill() {
    let dir = tempfile::TempDir::new().unwrap();
    let stub = common::make_stub_dir();
    let out_dir = stub.path().join("fzf-out");
    fs::create_dir_all(&out_dir).unwrap();
    fs::write(out_dir.join("1.out"), "u 123 1 0 0 S now 0 cmd\n").unwrap();

    common::write_exe(stub.path(), "fzf", common::fzf_stub_script());
    common::write_exe(
        stub.path(),
        "ps",
        r#"#!/bin/bash
echo "USER PID PPID PCPU PMEM STAT LSTART TIME ARGS"
echo "u 123 1 0 0 S now 0 cmd"
"#,
    );
    common::write_exe(
        stub.path(),
        "kill",
        r#"#!/bin/bash
set -euo pipefail
echo "$@" >> "${KILL_LOG:?}"
"#,
    );

    let kill_log = dir.path().join("kill.log");
    fs::write(&kill_log, "").unwrap();

    let out_dir_s = out_dir.to_string_lossy().to_string();
    let kill_log_s = kill_log.to_string_lossy().to_string();
    let envs = [
        ("FZF_STUB_OUT_DIR", out_dir_s.as_str()),
        ("KILL_LOG", kill_log_s.as_str()),
    ];

    let out = common::run_fzf_cli_with_stub_only_path(
        dir.path(),
        stub.path(),
        &["process", "-k"],
        &envs,
        None,
    );
    assert_eq!(out.code, 0);
    assert!(
        out.stdout.contains("SIGTERM"),
        "expected SIGTERM output, got: {}",
        out.stdout
    );
    let log = fs::read_to_string(&kill_log).unwrap();
    assert!(log.contains("123"), "kill log missing pid: {log}");
}

#[test]
fn process_decline_confirmation_aborts() {
    let dir = tempfile::TempDir::new().unwrap();
    let stub = common::make_stub_dir();
    let out_dir = stub.path().join("fzf-out");
    fs::create_dir_all(&out_dir).unwrap();
    fs::write(out_dir.join("1.out"), "u 123 1 0 0 S now 0 cmd\n").unwrap();

    common::write_exe(stub.path(), "fzf", common::fzf_stub_script());
    common::write_exe(
        stub.path(),
        "ps",
        r#"#!/bin/bash
echo "USER PID PPID PCPU PMEM STAT LSTART TIME ARGS"
echo "u 123 1 0 0 S now 0 cmd"
"#,
    );
    common::write_exe(
        stub.path(),
        "kill",
        r#"#!/bin/bash
set -euo pipefail
echo "$@" >> "${KILL_LOG:?}"
"#,
    );

    let kill_log = dir.path().join("kill.log");
    fs::write(&kill_log, "").unwrap();

    let out_dir_s = out_dir.to_string_lossy().to_string();
    let kill_log_s = kill_log.to_string_lossy().to_string();
    let envs = [
        ("FZF_STUB_OUT_DIR", out_dir_s.as_str()),
        ("KILL_LOG", kill_log_s.as_str()),
    ];

    let out = common::run_fzf_cli_with_stub_only_path(
        dir.path(),
        stub.path(),
        &["process"],
        &envs,
        Some("n\n"),
    );
    assert_eq!(out.code, 1);
    assert!(
        out.stdout.contains("🚫 Aborted."),
        "missing abort message: {}",
        out.stdout
    );
    let log = fs::read_to_string(&kill_log).unwrap();
    assert!(log.trim().is_empty(), "kill should not run, got: {log}");
}

#[test]
fn port_netstat_fallback_is_view_only() {
    let dir = tempfile::TempDir::new().unwrap();
    let stub = common::make_stub_dir();
    let out_dir = stub.path().join("fzf-out");
    fs::create_dir_all(&out_dir).unwrap();
    fs::write(
        out_dir.join("1.out"),
        "tcp4 0 0 127.0.0.1.1234 *.* LISTEN\n",
    )
    .unwrap();

    common::write_exe(stub.path(), "fzf", common::fzf_stub_script());
    common::write_exe(
        stub.path(),
        "netstat",
        r#"#!/bin/bash
echo "tcp4 0 0 127.0.0.1.1234 *.* LISTEN"
"#,
    );

    let out_dir_s = out_dir.to_string_lossy().to_string();
    let envs = [("FZF_STUB_OUT_DIR", out_dir_s.as_str())];

    let out =
        common::run_fzf_cli_with_stub_only_path(dir.path(), stub.path(), &["port"], &envs, None);
    assert_eq!(out.code, 0);
}

#[test]
fn port_lsof_kill_now_calls_kill() {
    let dir = tempfile::TempDir::new().unwrap();
    let stub = common::make_stub_dir();
    let out_dir = stub.path().join("fzf-out");
    fs::create_dir_all(&out_dir).unwrap();
    fs::write(out_dir.join("1.out"), "cmd 999 user TCP 127.0.0.1:1234\n").unwrap();

    common::write_exe(stub.path(), "fzf", common::fzf_stub_script());
    common::write_exe(
        stub.path(),
        "lsof",
        r#"#!/bin/bash
echo "COMMAND PID USER"
echo "cmd 999 user TCP 127.0.0.1:1234"
"#,
    );
    common::write_exe(
        stub.path(),
        "kill",
        r#"#!/bin/bash
set -euo pipefail
echo "$@" >> "${KILL_LOG:?}"
"#,
    );

    let kill_log = dir.path().join("kill.log");
    fs::write(&kill_log, "").unwrap();

    let out_dir_s = out_dir.to_string_lossy().to_string();
    let kill_log_s = kill_log.to_string_lossy().to_string();
    let envs = [
        ("FZF_STUB_OUT_DIR", out_dir_s.as_str()),
        ("KILL_LOG", kill_log_s.as_str()),
    ];

    let out = common::run_fzf_cli_with_stub_only_path(
        dir.path(),
        stub.path(),
        &["port", "-k"],
        &envs,
        None,
    );
    assert_eq!(out.code, 0);
    let log = fs::read_to_string(&kill_log).unwrap();
    assert!(log.contains("999"), "kill log missing pid: {log}");
}

#[test]
fn git_commands_outside_repo_abort() {
    let dir = tempfile::TempDir::new().unwrap();
    for cmd in [
        "git-status",
        "git-branch",
        "git-tag",
        "git-commit",
        "git-checkout",
    ] {
        let out = common::run_fzf_cli(dir.path(), &[cmd], &[], None);
        assert_eq!(out.code, 1, "{cmd} should exit 1");
        assert!(
            out.stderr
                .contains("❌ Not inside a Git repository. Aborting."),
            "{cmd} missing abort message: {}",
            out.stderr
        );
    }
}
