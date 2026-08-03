//! CLI-boundary coverage for the `no_agent_attribution` lock-down rule (spec
//! §"Lock-down policy" item 17, `error.kind = agent_attribution_present`).
//!
//! These tests drive real ops through the binary with a backend stub that
//! aborts if invoked, proving attribution is rejected *before* any provider
//! call. The rule lives in the CLI rather than in an agent-harness hook, so it
//! holds whether or not the calling runtime declares a matching hook.

use pretty_assertions::assert_eq;

use super::support::{StubEnv, parse_envelope, run_forge_cli};

/// Exit class for lock-down violations (BSD sysexits `EX_DATAERR`).
const DATA: i32 = 65;

/// A backend stub that fails loudly: a blocked-before-backend op must never
/// reach it.
const NEVER_RUN: &str =
    "#!/bin/sh\necho 'agent-attribution guard must block before backend' >&2\nexit 99\n";

#[test]
fn issue_comment_with_generator_marker_exits_data_65() {
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
            "Done.\n\n🤖 Generated with [Claude Code](https://claude.com/claude-code)",
        ],
    );
    assert_eq!(out.code, DATA, "stderr={}", out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["ok"], false);
    assert_eq!(env["error"]["code"], "agent_attribution_present");
    let detail = env["error"]["details"]["detail"]
        .as_str()
        .expect("detail string");
    assert!(
        detail.contains("line 3: agent generator marker"),
        "{detail}"
    );
}

#[test]
fn pr_comment_with_coauthor_trailer_exits_data_65_before_backend() {
    let stub = StubEnv::new().glab_stub(NEVER_RUN);
    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "gitlab",
            "--format",
            "json",
            "pr",
            "comment",
            "3",
            "--body",
            "Ship it.\n\nCo-Authored-By: Claude Opus 5 <noreply@anthropic.com>",
        ],
    );
    assert_eq!(out.code, DATA, "stderr={}", out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["error"]["code"], "agent_attribution_present");
    let detail = env["error"]["details"]["detail"]
        .as_str()
        .expect("detail string");
    assert!(
        detail.contains("line 3: agent co-author trailer"),
        "{detail}"
    );
}

#[test]
fn issue_create_with_marker_in_title_exits_data_65() {
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
            "generated with claude code",
        ],
    );
    assert_eq!(out.code, DATA, "stderr={}", out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["error"]["code"], "agent_attribution_present");
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
fn text_format_surfaces_kind_and_fix_on_stderr() {
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
            "🤖 Generated with Claude Code",
        ],
    );
    assert_eq!(out.code, DATA);
    assert!(
        out.stderr.contains("agent_attribution_present"),
        "{}",
        out.stderr
    );
    assert!(
        out.stderr.contains("delete the generator marker line"),
        "{}",
        out.stderr
    );
}

#[test]
fn documenting_the_rule_in_a_code_span_is_not_blocked() {
    // The scan strips fenced blocks and inline code spans, so a comment that
    // documents the rule dry-runs cleanly.
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
            "The gate rejects `Co-Authored-By: Claude ...` trailers.",
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["ok"], true);
}

#[test]
fn escape_hatch_env_bypasses_the_guard() {
    // FORGE_CLI_ALLOW_AGENT_ATTRIBUTION=1 disables the scan for a verified
    // false positive; the same body dry-runs cleanly.
    let stub = StubEnv::new()
        .gh_stub(NEVER_RUN)
        .env("FORGE_CLI_ALLOW_AGENT_ATTRIBUTION", "1");
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
            "quoting a historical footer: 🤖 Generated with Claude Code",
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["ok"], true);
}
