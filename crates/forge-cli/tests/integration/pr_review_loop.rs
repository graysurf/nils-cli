//! `pr review-loop observe --dry-run` must be a faithful, non-mutating
//! preflight.
//!
//! The ledger's `observe` is a provider-mutating call with a multi-field input
//! schema. Before this suite, its dry-run announced "read the chain, evaluate
//! one observation, and append with tip/head CAS" and then returned before
//! reading anything, so it predicted nothing about whether the real call would
//! be accepted — and the only way to discover the observation schema was to run
//! a live `observe`, which appends durable provider-visible state on success.

use std::fs;

use pretty_assertions::assert_eq;

use super::support::{StubEnv, parse_envelope, run_forge_cli};

/// A `gh` stub that answers `pr view` with a fixed head and records every
/// invocation, so a test can prove no mutation was attempted.
fn gh_stub_with_head(stub: &StubEnv, head: &str) -> String {
    let args_log = stub.tempdir.path().join("gh-args.log");
    format!(
        r#"#!/bin/sh
echo "$*" >> "{args_log}"
case "$1 $2" in
  "pr view")
    cat <<'JSON'
{{
  "number": 7,
  "url": "https://github.com/acme/widgets/pull/7",
  "state": "OPEN",
  "isDraft": false,
  "title": "example",
  "headRefName": "feat/example",
  "headRefOid": "{head}",
  "headRepository": {{"name": "widgets"}},
  "baseRefName": "main",
  "mergeable": "MERGEABLE",
  "mergedAt": "",
  "mergeCommit": null,
  "labels": [],
  "body": "",
  "closingIssuesReferences": []
}}
JSON
    exit 0
    ;;
esac
echo "stub: unscripted gh args: $*" >&2
exit 99
"#,
        args_log = args_log.display(),
        head = head,
    )
}

/// The same stub, plus an empty privileged review-state comment page, so a
/// genesis observation can reach a completely clean preflight.
fn gh_stub_with_empty_ledger(stub: &StubEnv, head: &str) -> String {
    gh_stub_with_head(stub, head).replace(
        r#"esac
echo "stub: unscripted gh args: $*" >&2"#,
        r#"  "api graphql")
    cat <<'JSON'
{"data":{"viewer":{"login":"forge-bot"},"repository":{"pullRequest":{"comments":{"nodes":[],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}}
JSON
    exit 0
    ;;
esac
echo "stub: unscripted gh args: $*" >&2"#,
    )
}

fn gh_args_log(stub: &StubEnv) -> String {
    fs::read_to_string(stub.tempdir.path().join("gh-args.log")).unwrap_or_default()
}

fn write_findings(stub: &StubEnv, name: &str, body: &str) -> String {
    let path = stub.tempdir.path().join(name);
    fs::write(&path, body).expect("write findings file");
    path.to_string_lossy().to_string()
}

fn preflight_verdict<'a>(envelope: &'a serde_json::Value, rule: &str) -> &'a serde_json::Value {
    envelope["data"]["preflight"]
        .as_array()
        .unwrap_or_else(|| panic!("data.preflight array, got: {envelope}"))
        .iter()
        .find(|verdict| verdict["rule"] == rule)
        .unwrap_or_else(|| panic!("no preflight verdict for {rule}, got: {envelope}"))
}

const VALID_FINDINGS: &str = r#"[
  {
    "lifecycle_fingerprint": "correctness:review-loop:head-cas",
    "blocking": true,
    "disposition": "open"
  }
]"#;

#[test]
fn observe_dry_run_performs_the_head_cas_it_advertises() {
    let stub = StubEnv::new();
    let body = gh_stub_with_head(&stub, "provider-head");
    let findings = write_findings(&stub, "findings.json", VALID_FINDINGS);
    let stub = stub.gh_stub(&body);

    let out = run_forge_cli(
        &stub,
        &[
            "--format",
            "json",
            "--dry-run",
            "--provider",
            "github",
            "--repo",
            "acme/widgets",
            "pr",
            "review-loop",
            "observe",
            "7",
            "--expected-head",
            "stale-head",
            "--findings-file",
            &findings,
        ],
    );

    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(
        envelope["schema_version"],
        "cli.forge-cli.pr.review-loop.observe.v1"
    );

    // The whole point: a dry run against a drifted head must predict the
    // `review_state_conflict` the real call would return, instead of reporting
    // a plan it never evaluated.
    let verdict = preflight_verdict(&envelope, "expected_head");
    assert_eq!(verdict["ok"], false, "verdict={verdict}");
    assert_eq!(verdict["code"], "review_state_conflict");
    assert_eq!(envelope["data"]["preflight_ok"], false);

    // Reads are expected — the CAS needs them. Writes are not. The chain read
    // is a read-only GraphQL `query(...)`, so the write signal is a mutating
    // HTTP verb, a GraphQL `mutation`, or a comment-posting subcommand.
    let log = gh_args_log(&stub);
    for write_marker in [
        "-X POST",
        "-X PATCH",
        "-X PUT",
        "-X DELETE",
        "mutation",
        "pr comment",
    ] {
        assert!(
            !log.contains(write_marker),
            "a dry run must not attempt a provider write ({write_marker}), log={log}"
        );
    }
}

#[test]
fn observe_dry_run_accepts_a_matching_head() {
    let stub = StubEnv::new();
    let body = gh_stub_with_head(&stub, "provider-head");
    let findings = write_findings(&stub, "findings.json", VALID_FINDINGS);
    let stub = stub.gh_stub(&body);

    let out = run_forge_cli(
        &stub,
        &[
            "--format",
            "json",
            "--dry-run",
            "--provider",
            "github",
            "--repo",
            "acme/widgets",
            "pr",
            "review-loop",
            "observe",
            "7",
            "--expected-head",
            "provider-head",
            "--findings-file",
            &findings,
        ],
    );

    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let envelope = parse_envelope(&out.stdout);
    let verdict = preflight_verdict(&envelope, "expected_head");
    assert_eq!(verdict["ok"], true, "verdict={verdict}");
}

#[test]
fn observe_dry_run_validates_the_findings_payload() {
    let stub = StubEnv::new();
    let body = gh_stub_with_head(&stub, "provider-head");
    // A merge-envelope row with no lifecycle fingerprint: the exact shape whose
    // rejection previously could only be discovered by a live, state-appending
    // `observe`.
    let findings = write_findings(
        &stub,
        "findings.json",
        r#"{"data": {"findings": [{"category": "correctness", "primary": {"severity": "high"}}]}}"#,
    );
    let stub = stub.gh_stub(&body);

    let out = run_forge_cli(
        &stub,
        &[
            "--format",
            "json",
            "--dry-run",
            "--provider",
            "github",
            "--repo",
            "acme/widgets",
            "pr",
            "review-loop",
            "observe",
            "7",
            "--expected-head",
            "provider-head",
            "--findings-file",
            &findings,
        ],
    );

    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let envelope = parse_envelope(&out.stdout);
    let verdict = preflight_verdict(&envelope, "findings_file");
    assert_eq!(verdict["ok"], false, "verdict={verdict}");
    assert_eq!(verdict["code"], "review_fingerprint_required");
}

#[test]
fn observe_dry_run_reports_the_payload_verdict_when_the_provider_is_unreachable() {
    // The sweep does not short-circuit, so the local payload check is reported
    // even when no provider call can succeed. That is what makes `--dry-run`
    // usable as a schema check without writing durable state.
    let stub = StubEnv::new();
    let findings = write_findings(&stub, "findings.json", VALID_FINDINGS);
    let stub = stub.gh_stub("#!/bin/sh\necho 'provider unreachable' >&2\nexit 99\n");

    let out = run_forge_cli(
        &stub,
        &[
            "--format",
            "json",
            "--dry-run",
            "--provider",
            "github",
            "--repo",
            "acme/widgets",
            "pr",
            "review-loop",
            "observe",
            "7",
            "--expected-head",
            "any-head",
            "--findings-file",
            &findings,
        ],
    );

    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(preflight_verdict(&envelope, "findings_file")["ok"], true);
    assert_eq!(
        preflight_verdict(&envelope, "provider_pull_request")["ok"],
        false
    );
    // Rules that depend on a failed lookup are reported as unevaluated rather
    // than silently omitted or guessed.
    let head_rule = preflight_verdict(&envelope, "expected_head");
    assert_eq!(head_rule["ok"], false);
    assert!(
        head_rule["message"]
            .as_str()
            .unwrap_or_default()
            .starts_with("not evaluated:"),
        "verdict={head_rule}"
    );
}

#[test]
fn observe_dry_run_renders_the_exact_combined_comment_it_would_post() {
    // A marker-only ledger comment renders blank on GitHub, and the final
    // outcome used to need a second comment. The dry run must show the one
    // comment that now carries both, and still write nothing.
    let stub = StubEnv::new();
    let body = gh_stub_with_empty_ledger(&stub, "provider-head");
    let findings = write_findings(&stub, "findings.json", "[]");
    let outcome = stub.tempdir.path().join("outcome.md");
    fs::write(
        &outcome,
        "## Delivery outcome\n\nApproved: bounded review converged.\n",
    )
    .expect("write outcome body");
    let outcome = outcome.to_string_lossy().to_string();
    let stub = stub.gh_stub(&body);

    let out = run_forge_cli(
        &stub,
        &[
            "--format",
            "json",
            "--dry-run",
            "--provider",
            "github",
            "--repo",
            "acme/widgets",
            "pr",
            "review-loop",
            "observe",
            "7",
            "--expected-head",
            "provider-head",
            "--findings-file",
            &findings,
            "--body-file",
            &outcome,
        ],
    );

    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["data"]["preflight_ok"], true, "{envelope}");
    assert_eq!(envelope["data"]["would_append"], true);
    assert_eq!(preflight_verdict(&envelope, "outcome_body")["ok"], true);
    assert_eq!(
        preflight_verdict(&envelope, "state_comment_body")["ok"],
        true
    );

    let planned = &envelope["data"]["planned_comment"];
    assert_eq!(planned["includes_outcome_body"], true, "{envelope}");
    assert_eq!(
        planned["visible_metadata"],
        "forge-cli review ledger · generation 0 · review-loop · head provider-hea"
    );
    // The reported size is the complete body that would be posted, which is what
    // the provider limit is checked against.
    assert!(
        planned["bytes"].as_u64().unwrap_or_default() > 0,
        "{envelope}"
    );
    assert!(
        envelope["data"]["plan"][0]
            .as_str()
            .unwrap_or_default()
            .contains("in one comment"),
        "{envelope}"
    );

    let log = gh_args_log(&stub);
    for write_marker in ["-X POST", "-X PATCH", "-X PUT", "mutation", "pr comment"] {
        assert!(
            !log.contains(write_marker),
            "a combined dry run must not write ({write_marker}), log={log}"
        );
    }
}

#[test]
fn observe_dry_run_omits_a_planned_comment_when_the_ledger_is_already_current() {
    // Nothing is appended, so nothing is planned — and the supplied outcome is
    // therefore not posted either. This is the retry-deduplication signal the
    // caller reads instead of guessing.
    let stub = StubEnv::new();
    let body = gh_stub_with_head(&stub, "provider-head");
    let findings = write_findings(&stub, "findings.json", VALID_FINDINGS);
    let stub = stub.gh_stub(&body);

    let out = run_forge_cli(
        &stub,
        &[
            "--format",
            "json",
            "--dry-run",
            "--provider",
            "github",
            "--repo",
            "acme/widgets",
            "pr",
            "review-loop",
            "observe",
            "7",
            "--expected-head",
            "stale-head",
            "--findings-file",
            &findings,
            "--body",
            "## Delivery outcome",
        ],
    );

    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert!(
        envelope["data"].get("planned_comment").is_none(),
        "no write is planned, so no comment shape is reported: {envelope}"
    );
    // The outcome body itself is still validated, independently of the provider.
    assert_eq!(preflight_verdict(&envelope, "outcome_body")["ok"], true);
}

#[test]
fn observe_rejects_both_outcome_body_forms_at_once() {
    let stub = StubEnv::new();
    let findings = write_findings(&stub, "findings.json", VALID_FINDINGS);
    let stub = stub.gh_stub("#!/bin/sh\nexit 99\n");

    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "--repo",
            "acme/widgets",
            "pr",
            "review-loop",
            "observe",
            "7",
            "--expected-head",
            "provider-head",
            "--findings-file",
            &findings,
            "--body",
            "inline",
            "--body-file",
            "outcome.md",
        ],
    );

    assert_eq!(out.code, 64, "stdout={} stderr={}", out.stdout, out.stderr);
    assert!(
        out.stderr.contains("--body-file"),
        "stderr should name the conflicting flag: {}",
        out.stderr
    );
}

#[test]
fn observe_help_documents_both_findings_file_shapes_and_the_combined_outcome() {
    let stub = StubEnv::new();
    let out = run_forge_cli(&stub, &["pr", "review-loop", "observe", "--help"]);
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    for expected in [
        "lifecycle_fingerprint",
        "<category>:<component>:<invariant>",
        "disposition",
        "--dry-run",
        "--body-file",
        "forge-cli review ledger",
        "outcome_posted",
    ] {
        assert!(
            out.stdout.contains(expected),
            "observe --help missing {expected:?}: stdout={}",
            out.stdout
        );
    }
}
