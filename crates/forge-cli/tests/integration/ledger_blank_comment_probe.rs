//! Pre-fix probe for sympoies/nils-cli#1417.
//!
//! Captures the user-visible defect against the unmodified baseline: the body
//! `pr review-loop observe` posts for a ledger append is only an HTML comment,
//! which GitHub hides when it renders Markdown, so the PR timeline shows a blank
//! comment authored by the operator.
//!
//! Uses only pre-existing APIs so it compiles against the baseline. It asserts on
//! the recorded `gh` argv rather than the exit code, because the stub returns an
//! unchanged comment page and the append's read-back verification therefore fails
//! after the POST has already been recorded.

use std::fs;

use super::support::{StubEnv, run_forge_cli};

#[test]
fn a_posted_ledger_comment_body_is_not_only_a_hidden_html_comment() {
    let stub = StubEnv::new();
    let args_log = stub.tempdir.path().join("gh-args.log");
    let script = format!(
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
  "headRefOid": "provider-head",
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
  "api graphql")
    cat <<'JSON'
{{"data":{{"viewer":{{"login":"forge-bot"}},"repository":{{"pullRequest":{{"comments":{{"nodes":[],"pageInfo":{{"hasNextPage":false,"endCursor":null}}}}}}}}}}}}
JSON
    exit 0
    ;;
esac
case "$1" in
  api)
    echo "https://github.com/acme/widgets/pull/7#issuecomment-9"
    exit 0
    ;;
esac
echo "stub: unscripted gh args: $*" >&2
exit 99
"#,
        args_log = args_log.display(),
    );
    let findings = stub.tempdir.path().join("findings.json");
    fs::write(&findings, "[]").expect("write findings");
    let findings = findings.to_string_lossy().to_string();
    let stub = stub.gh_stub(&script);

    let _ = run_forge_cli(
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

    let log = fs::read_to_string(&args_log).unwrap_or_default();
    let posted = log
        .lines()
        .find_map(|line| line.split_once("body=").map(|(_, body)| body))
        .unwrap_or_else(|| panic!("no comment body was posted; log={log}"));

    // The defect: every visible byte of the posted body is inside an HTML
    // comment, so GitHub renders the comment as empty.
    assert!(
        !posted.trim_start().starts_with("<!--"),
        "a ledger comment must carry human-visible text, not only a hidden marker; \
         GitHub renders this body as a blank comment. posted={posted}"
    );
}
