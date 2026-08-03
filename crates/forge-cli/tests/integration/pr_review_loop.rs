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

/// The same stub, plus one privileged review-state comment page carrying
/// `comment_bodies`, so a preflight can reach a real chain instead of only a
/// resolvable pull request.
fn gh_stub_with_ledger(stub: &StubEnv, head: &str, comment_bodies: &[String]) -> String {
    let nodes = comment_bodies
        .iter()
        .map(|body| {
            serde_json::json!({
                "author": {"login": "forge-bot"},
                "authorAssociation": "OWNER",
                "body": body,
                "createdAt": "2026-07-20T12:00:00Z"
            })
        })
        .collect::<Vec<_>>();
    let page = serde_json::json!({"data": {
        "viewer": {"login": "forge-bot"},
        "repository": {"pullRequest": {"comments": {
            "nodes": nodes,
            "pageInfo": {"hasNextPage": false, "endCursor": null}
        }}}
    }});
    gh_stub_with_head(stub, head).replace(
        r#"esac
echo "stub: unscripted gh args: $*" >&2"#,
        &format!(
            r#"  "api graphql")
    cat <<'JSON'
{page}
JSON
    exit 0
    ;;
esac
echo "stub: unscripted gh args: $*" >&2"#
        ),
    )
}

/// An empty ledger, so a genesis observation can reach a clean preflight.
fn gh_stub_with_empty_ledger(stub: &StubEnv, head: &str) -> String {
    gh_stub_with_ledger(stub, head, &[])
}

/// A stateful stub for the LIVE append: the ledger reads empty until the comment
/// POST lands, then returns `appended_body`. That ordering is what makes the
/// post-write read-back — and therefore `outcome_posted` — run for real.
fn gh_stub_that_accepts_one_append(stub: &StubEnv, head: &str, appended_body: &str) -> String {
    let posted_flag = stub.tempdir.path().join("posted.flag");
    let page = |nodes: serde_json::Value| {
        serde_json::json!({"data": {
            "viewer": {"login": "forge-bot"},
            "repository": {"pullRequest": {"comments": {
                "nodes": nodes,
                "pageInfo": {"hasNextPage": false, "endCursor": null}
            }}}
        }})
        .to_string()
    };
    let after = page(serde_json::json!([{
        "author": {"login": "forge-bot"},
        "authorAssociation": "OWNER",
        "body": appended_body,
        "createdAt": "2026-07-20T12:00:01Z"
    }]));
    let before = page(serde_json::json!([]));
    gh_stub_with_head(stub, head).replace(
        r#"esac
echo "stub: unscripted gh args: $*" >&2"#,
        &format!(
            r#"  "api graphql")
    if [ -f "{flag}" ]; then
      cat <<'AFTER'
{after}
AFTER
    else
      cat <<'BEFORE'
{before}
BEFORE
    fi
    exit 0
    ;;
esac
case "$1" in
  api)
    : > "{flag}"
    echo "https://github.com/acme/widgets/pull/7#issuecomment-9"
    exit 0
    ;;
esac
echo "stub: unscripted gh args: $*" >&2"#,
            flag = posted_flag.display(),
        ),
    )
}

/// The exact comment body a genesis `VALID_FINDINGS` observation at `head`
/// produces, plus its record digest, so a stub can seed a chain that is already
/// current and a test can assert the exact planned write.
///
/// `outcome` mirrors what `--body` would carry; `None` is the marker-only form.
fn ledger_comment(head: &str, outcome: Option<&str>) -> (String, String) {
    use forge_cli::ops::review_state::{
        ReviewFindingObservation, ReviewFindingStatus, ReviewStatePayload, ReviewStateRecord,
        observe_review_loop, render_state_comment_body,
    };

    let state = observe_review_loop(
        None,
        head,
        &[ReviewFindingObservation {
            fingerprint: "correctness:review-loop:head-cas".to_string(),
            root_cause_fingerprint: None,
            blocking: true,
            status: ReviewFindingStatus::Open,
            threads: Vec::new(),
        }],
    )
    .expect("genesis transition")
    .state;
    let record = ReviewStateRecord::new(
        "acme/widgets",
        7,
        head,
        0,
        None,
        ReviewStatePayload::ReviewLoop { state },
    )
    .expect("genesis record");
    let body = render_state_comment_body(&record, outcome).expect("rendered body");
    (body, record.record_digest)
}

fn current_ledger_comment(head: &str) -> (String, String) {
    ledger_comment(head, None)
}

/// The genesis body for an EMPTY findings envelope, optionally carrying
/// `outcome`. This is the record a clean review round appends, so it is what the
/// combined dry-run and live-append tests predict.
fn empty_findings_ledger_comment(head: &str, outcome: Option<&str>) -> (String, String) {
    use forge_cli::ops::review_state::{
        ReviewStatePayload, ReviewStateRecord, observe_review_loop, render_state_comment_body,
    };

    let state = observe_review_loop(None, head, &[])
        .expect("genesis transition")
        .state;
    let record = ReviewStateRecord::new(
        "acme/widgets",
        7,
        head,
        0,
        None,
        ReviewStatePayload::ReviewLoop { state },
    )
    .expect("genesis record");
    let body = render_state_comment_body(&record, outcome).expect("rendered body");
    (body, record.record_digest)
}

fn combined_ledger_comment(head: &str, outcome: &str) -> (String, String) {
    empty_findings_ledger_comment(head, Some(outcome))
}

fn ledger_comment_for_empty_findings(head: &str) -> (String, String) {
    empty_findings_ledger_comment(head, None)
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

/// Argv fragments that would mean a provider write was attempted.
///
/// These must match the shapes this crate actually emits, not generic `curl`
/// idioms: the ledger append is
/// `gh api repos/<repo>/issues/<n>/comments --method POST --raw-field body=…`
/// (`--method`, never `-X`), so a guard listing only `-X POST` can never fail.
/// The chain read is a read-only GraphQL `query(...)`, so `mutation` is the
/// GraphQL write signal.
const PROVIDER_WRITE_MARKERS: [&str; 7] = [
    "--method POST",
    "--method PATCH",
    "--method PUT",
    "--method DELETE",
    "--raw-field body=",
    "mutation",
    "pr comment",
];

const OUTCOME_BODY: &str = "## Delivery outcome\n\nApproved: bounded review converged.";

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
    for write_marker in PROVIDER_WRITE_MARKERS {
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
    fs::write(&outcome, format!("{OUTCOME_BODY}\n")).expect("write outcome body");
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
    // the provider limit is checked against — so it must equal the byte length of
    // the exact rendered body, not the marker or the outcome alone.
    let (expected_body, _) = combined_ledger_comment("provider-head", OUTCOME_BODY);
    assert_eq!(
        planned["bytes"].as_u64().unwrap_or_default(),
        expected_body.len() as u64,
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
    for write_marker in PROVIDER_WRITE_MARKERS {
        assert!(
            !log.contains(write_marker),
            "a combined dry run must not write ({write_marker}), log={log}"
        );
    }
}

#[test]
fn observe_dry_run_plans_nothing_when_the_transition_could_not_be_evaluated() {
    // With a drifted head the chain read still runs and fails, so the transition
    // is never evaluated. Nothing may be planned on the strength of a rule that
    // did not run, and the outcome body must still be validated locally.
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
            OUTCOME_BODY,
        ],
    );

    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert!(
        envelope["data"].get("planned_comment").is_none(),
        "no write is planned, so no comment shape is reported: {envelope}"
    );
    assert!(
        envelope["data"].get("would_append").is_none(),
        "an unevaluated transition must not predict an append: {envelope}"
    );
    // The outcome body itself is still validated, independently of the provider.
    assert_eq!(preflight_verdict(&envelope, "outcome_body")["ok"], true);
}

#[test]
fn observe_dry_run_fails_the_outcome_verdict_for_a_body_a_live_run_would_reject() {
    // Delivery gates on `preflight_ok`, so a body the live run refuses must fail
    // here too. All three rules are enforced before the first provider call, which
    // is why an unreachable provider does not hide them.
    let stub = StubEnv::new();
    let findings = write_findings(&stub, "findings.json", VALID_FINDINGS);
    let missing = stub
        .tempdir
        .path()
        .join("absent-outcome.md")
        .to_string_lossy()
        .to_string();
    let stub = stub.gh_stub("#!/bin/sh\necho 'provider unreachable' >&2\nexit 99\n");

    for (label, args, expected_code) in [
        (
            "marker injection",
            vec!["--body", "<!-- forge-cli:review-state:v1 deadbeef -->"],
            Some("review_state_comment_invalid"),
        ),
        (
            "hidden html comment",
            vec!["--body", "Approved.\n\n<!--"],
            Some("review_state_comment_invalid"),
        ),
        (
            "local path leak",
            vec!["--body", "outcome at /home/operator/report.md"],
            Some("local_path_present"),
        ),
        (
            "unreadable body file",
            vec!["--body-file", missing.as_str()],
            None,
        ),
    ] {
        let mut argv = vec![
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
        ];
        argv.extend(args);

        let out = run_forge_cli(&stub, &argv);

        assert_eq!(out.code, 0, "{label}: stderr={}", out.stderr);
        let envelope = parse_envelope(&out.stdout);
        let verdict = preflight_verdict(&envelope, "outcome_body");
        assert_eq!(verdict["ok"], false, "{label}: verdict={verdict}");
        assert_eq!(envelope["data"]["preflight_ok"], false, "{label}");
        if let Some(code) = expected_code {
            assert_eq!(verdict["code"], code, "{label}: verdict={verdict}");
        }
        // The findings payload verdict still survives an unreachable provider.
        assert_eq!(
            preflight_verdict(&envelope, "findings_file")["ok"],
            true,
            "{label}"
        );
    }
}

#[test]
fn an_already_current_ledger_still_reports_a_clean_combined_preflight() {
    // Delivery gates on `preflight_ok == true` before a live append. A rerun
    // whose ledger is already current must therefore stay clean while still
    // reporting that nothing — including the outcome — would be posted.
    let stub = StubEnv::new();
    let (seeded, tip) = current_ledger_comment("provider-head");
    let body = gh_stub_with_ledger(&stub, "provider-head", &[seeded]);
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
            "--expected-state",
            &tip,
            "--findings-file",
            &findings,
            "--body",
            "## Delivery outcome",
        ],
    );

    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["data"]["preflight_ok"], true, "{envelope}");
    assert_eq!(envelope["data"]["would_append"], false, "{envelope}");
    assert_eq!(preflight_verdict(&envelope, "outcome_body")["ok"], true);
    assert_eq!(
        preflight_verdict(&envelope, "expected_state_tip")["ok"],
        true
    );
    assert_eq!(
        preflight_verdict(&envelope, "observation_transition")["ok"],
        true
    );
    // Nothing is written, so no comment shape is reported and the conditional
    // `state_comment_body` rule is absent rather than a spurious failure.
    assert!(
        envelope["data"].get("planned_comment").is_none(),
        "{envelope}"
    );
    assert!(
        envelope["data"]["preflight"]
            .as_array()
            .expect("preflight array")
            .iter()
            .all(|verdict| verdict["rule"] != "state_comment_body"),
        "{envelope}"
    );

    let log = gh_args_log(&stub);
    for write_marker in PROVIDER_WRITE_MARKERS {
        assert!(
            !log.contains(write_marker),
            "a dry run must not write ({write_marker}), log={log}"
        );
    }
}

#[test]
fn a_live_combined_observe_reports_appended_and_outcome_posted() {
    // The envelope is the delivery contract: `outcome_posted` must reflect a
    // confirmed provider write, and the posted comment must carry the visible
    // label, the exact marker, and the outcome in ONE body.
    let stub = StubEnv::new();
    let (combined, tip) = combined_ledger_comment("provider-head", OUTCOME_BODY);
    let body = gh_stub_that_accepts_one_append(&stub, "provider-head", &combined);
    let findings = write_findings(&stub, "findings.json", "[]");
    let stub = stub.gh_stub(&body);

    let out = run_forge_cli(
        &stub,
        &[
            "--format",
            "json",
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
            OUTCOME_BODY,
        ],
    );

    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(
        envelope["schema_version"],
        "cli.forge-cli.pr.review-loop.observe.v1"
    );
    assert_eq!(envelope["data"]["appended"], true, "{envelope}");
    assert_eq!(envelope["data"]["outcome_posted"], true, "{envelope}");
    assert_eq!(envelope["data"]["state_tip_digest"], tip, "{envelope}");

    // Exactly one comment mutation, and its body carries all three parts.
    let log = gh_args_log(&stub);
    assert_eq!(
        log.matches("--method POST").count(),
        1,
        "one comment mutation only, log={log}"
    );
    for expected in [
        "forge-cli review ledger \u{b7} generation 0 \u{b7} review-loop \u{b7} head provider-hea",
        "<!-- forge-cli:review-state:v1 ",
        "Approved: bounded review converged.",
    ] {
        assert!(log.contains(expected), "missing {expected:?} in log={log}");
    }
}

#[test]
fn a_live_observe_without_an_outcome_body_reports_outcome_posted_false() {
    let stub = StubEnv::new();
    let (marker_only, _) = combined_ledger_comment("provider-head", OUTCOME_BODY);
    // Seed the read-back with the marker-only body the run actually writes.
    let (appended, _) = ledger_comment_for_empty_findings("provider-head");
    assert_ne!(marker_only, appended);
    let body = gh_stub_that_accepts_one_append(&stub, "provider-head", &appended);
    let findings = write_findings(&stub, "findings.json", "[]");
    let stub = stub.gh_stub(&body);

    let out = run_forge_cli(
        &stub,
        &[
            "--format",
            "json",
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
    assert_eq!(envelope["data"]["appended"], true, "{envelope}");
    assert_eq!(envelope["data"]["outcome_posted"], false, "{envelope}");
    // Even with no outcome, the ledger comment is never a bare marker.
    let log = gh_args_log(&stub);
    assert!(
        log.contains("forge-cli review ledger \u{b7} generation 0"),
        "{log}"
    );
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
