//! End-to-end coverage for the GraphQL rate-limit gate (sympoies/nils-cli#1051).
//!
//! GraphQL-backed ops preflight the free `gh api rate_limit` endpoint and retry
//! once after a rate-limit failure. The timing/backoff policy is unit-tested
//! with a fake clock in `crate::rate_limit`; these tests pin the wiring through
//! the real binary with the gate enabled. Both use a healthy GraphQL budget so
//! no real sleeping occurs.

use pretty_assertions::assert_eq;

use super::support::{StubEnv, parse_envelope, run_forge_cli};

const PR_VIEW_JSON: &str = r#"{
  "number": 42,
  "url": "https://github.com/o/r/pull/42",
  "state": "OPEN",
  "isDraft": false,
  "title": "t",
  "headRefName": "feat/x",
  "baseRefName": "main",
  "mergeable": "MERGEABLE",
  "mergedAt": null,
  "labels": []
}"#;

const ISSUE_VIEW_JSON: &str = r#"{
  "number": 7,
  "url": "https://github.com/o/r/issues/7",
  "state": "OPEN",
  "title": "t",
  "body": "b",
  "labels": [],
  "assignees": []
}"#;

/// A `gh api rate_limit` document with a healthy GraphQL budget (well above the
/// default `min_remaining`), so the preflight proceeds without sleeping.
const HEALTHY_RATE_LIMIT: &str = r#"{"resources":{"core":{"remaining":4821},"graphql":{"limit":5000,"remaining":4999,"reset":1700000000}}}"#;

/// Enable the gate and clamp any accidental wait to a couple of seconds. A
/// healthy budget means these bounds are never exercised.
fn with_gate_on(stub: StubEnv) -> StubEnv {
    stub.env("FORGE_CLI_RATE_LIMIT_GATE", "on")
        .env("FORGE_CLI_RATE_LIMIT_POLL_SECS", "1")
        .env("FORGE_CLI_RATE_LIMIT_MAX_WAIT_SECS", "2")
}

#[test]
fn pr_view_preflights_rate_limit_when_gate_enabled() {
    let stub = StubEnv::new();
    let log = stub.tempdir.path().join("gh-calls.log");
    let body = format!(
        r#"#!/bin/sh
set -e
echo "$1 $2" >> "{log}"
case "$1 $2" in
  "api rate_limit")
    cat <<'EOF'
{rl}
EOF
    ;;
  "pr view")
    cat <<'EOF'
{view}
EOF
    ;;
  *)
    echo "unexpected gh args: $*" >&2
    exit 99
    ;;
esac
"#,
        log = log.display(),
        rl = HEALTHY_RATE_LIMIT,
        view = PR_VIEW_JSON,
    );
    let stub = with_gate_on(stub.gh_stub(&body));
    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "--format",
            "json",
            "pr",
            "view",
            "42",
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    assert_eq!(parse_envelope(&out.stdout)["data"]["number"], 42);

    let calls = std::fs::read_to_string(&log).expect("read gh call log");
    let lines: Vec<&str> = calls.lines().collect();
    assert_eq!(
        lines.first().copied(),
        Some("api rate_limit"),
        "the free rate-limit probe must run before the GraphQL-backed call; calls={lines:?}"
    );
    assert!(
        lines.contains(&"pr view"),
        "the gated pr view call still ran; calls={lines:?}"
    );
}

#[test]
fn issue_view_preflights_rate_limit_when_gate_enabled() {
    // A non-`pr view` GraphQL-backed op must also preflight the free
    // `gh api rate_limit` probe. This pins that gating is wired centrally for
    // every op, not hand-picked per PR-lifecycle command (sympoies/nils-cli#1063).
    let stub = StubEnv::new();
    let log = stub.tempdir.path().join("gh-calls.log");
    let body = format!(
        r#"#!/bin/sh
set -e
echo "$1 $2" >> "{log}"
case "$1 $2" in
  "api rate_limit")
    cat <<'EOF'
{rl}
EOF
    ;;
  "issue view")
    cat <<'EOF'
{view}
EOF
    ;;
  *)
    echo "unexpected gh args: $*" >&2
    exit 99
    ;;
esac
"#,
        log = log.display(),
        rl = HEALTHY_RATE_LIMIT,
        view = ISSUE_VIEW_JSON,
    );
    let stub = with_gate_on(stub.gh_stub(&body));
    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "--format",
            "json",
            "issue",
            "view",
            "7",
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    assert_eq!(parse_envelope(&out.stdout)["data"]["number"], 7);

    let calls = std::fs::read_to_string(&log).expect("read gh call log");
    let lines: Vec<&str> = calls.lines().collect();
    assert_eq!(
        lines.first().copied(),
        Some("api rate_limit"),
        "the free rate-limit probe must run before the GraphQL-backed issue view; calls={lines:?}"
    );
    assert!(
        lines.contains(&"issue view"),
        "the gated issue view call still ran; calls={lines:?}"
    );
}

#[test]
fn pr_view_retries_once_after_rate_limited_failure() {
    let stub = StubEnv::new();
    // First `pr view` fails with a GraphQL rate-limit stderr; the sentinel
    // flips it to success on the gate's retry.
    let sentinel = stub.tempdir.path().join("pr-view-failed-once");
    let body = format!(
        r#"#!/bin/sh
set -e
case "$1 $2" in
  "api rate_limit")
    cat <<'EOF'
{rl}
EOF
    ;;
  "pr view")
    if [ ! -e "{sentinel}" ]; then
      touch "{sentinel}"
      echo 'GraphQL: API rate limit exceeded' >&2
      exit 1
    fi
    cat <<'EOF'
{view}
EOF
    ;;
  *)
    echo "unexpected gh args: $*" >&2
    exit 99
    ;;
esac
"#,
        rl = HEALTHY_RATE_LIMIT,
        view = PR_VIEW_JSON,
        sentinel = sentinel.display(),
    );
    let stub = with_gate_on(stub.gh_stub(&body));
    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "--format",
            "json",
            "pr",
            "view",
            "42",
        ],
    );
    assert_eq!(
        out.code, 0,
        "the gate should retry past the first rate-limit failure; stderr={}",
        out.stderr
    );
    assert_eq!(parse_envelope(&out.stdout)["data"]["number"], 42);
    assert!(
        sentinel.exists(),
        "the first pr view attempt must have failed as designed"
    );
}
