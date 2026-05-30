//! CLI-boundary coverage for the `no_local_path` lock-down rule (spec
//! §"Lock-down policy" item 11, `error.kind = local_path_present`).
//!
//! These tests drive real ops through the binary with a backend stub that
//! aborts if invoked, proving the guard rejects machine-local home paths
//! *before* any provider call — the egress path the repo-side
//! `portable-paths-scan.py` file hook never covers.

use pretty_assertions::assert_eq;

use super::support::{StubEnv, parse_envelope, run_forge_cli};

/// Exit class for lock-down violations (BSD sysexits `EX_DATAERR`).
const DATA: i32 = 65;

/// A backend stub that fails loudly: a blocked-before-backend op must never
/// reach it.
const NEVER_RUN: &str =
    "#!/bin/sh\necho 'local-path guard must block before backend' >&2\nexit 99\n";

#[test]
fn issue_comment_with_macos_home_path_exits_data_65() {
    let stub = StubEnv::new().gh_stub(NEVER_RUN);
    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "--format",
            "json",
            "issue",
            "comment",
            "1",
            "--body",
            "logs are under /Users/dev/Project/secret",
        ],
    );
    assert_eq!(out.code, DATA, "stderr={}", out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["ok"], false);
    assert_eq!(env["error"]["code"], "local_path_present");
    let detail = env["error"]["details"]["detail"]
        .as_str()
        .expect("detail string");
    assert!(detail.contains("/Users/dev/Project/secret"), "{detail}");
    assert!(detail.contains("use $HOME/Project/secret"), "{detail}");
}

#[test]
fn issue_create_with_local_path_in_title_exits_data_65() {
    let stub = StubEnv::new().gh_stub(NEVER_RUN);
    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "--format",
            "json",
            "issue",
            "create",
            "--title",
            "broken under /home/alice/x",
        ],
    );
    assert_eq!(out.code, DATA, "stderr={}", out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["error"]["code"], "local_path_present");
    assert!(
        env["error"]["message"]
            .as_str()
            .unwrap_or("")
            .starts_with("title contains"),
        "{}",
        env["error"]["message"]
    );
}

#[test]
fn text_format_surfaces_kind_and_home_fix_on_stderr() {
    let stub = StubEnv::new().gh_stub(NEVER_RUN);
    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "--format",
            "text",
            "issue",
            "comment",
            "1",
            "--body",
            "see /Users/dev/notes",
        ],
    );
    assert_eq!(out.code, DATA);
    assert!(out.stderr.contains("local_path_present"), "{}", out.stderr);
    assert!(out.stderr.contains("use $HOME/notes"), "{}", out.stderr);
}

#[test]
fn allowlisted_ci_runner_path_is_not_blocked() {
    // `/home/runner/...` is an allowlisted CI-runner root, so a comment that
    // pastes a CI log path dry-runs cleanly without tripping the guard.
    let stub = StubEnv::new().gh_stub(NEVER_RUN);
    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "--dry-run",
            "--format",
            "json",
            "issue",
            "comment",
            "1",
            "--body",
            "CI artifact at /home/runner/work/repo/out.log",
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["ok"], true);
}

#[test]
fn escape_hatch_env_bypasses_the_guard() {
    // FORGE_CLI_ALLOW_LOCAL_PATH=1 disables the scan for a verified false
    // positive; the same body that would otherwise be blocked dry-runs cleanly.
    let stub = StubEnv::new()
        .gh_stub(NEVER_RUN)
        .env("FORGE_CLI_ALLOW_LOCAL_PATH", "1");
    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "--dry-run",
            "--format",
            "json",
            "issue",
            "comment",
            "1",
            "--body",
            "deliberately /Users/dev/x for a verified reason",
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["ok"], true);
}
