//! Recovery tests for an authenticated actor's provider-valid pending review.

use std::fs;

use pretty_assertions::assert_eq;

use super::support::{CmdOutput, StubEnv, parse_envelope, run_forge_cli, run_forge_cli_with_stdin};

#[test]
fn pr_pending_review_catalog_uses_provider_native_viewer_guards() {
    let catalog = include_str!("../../docs/specs/forge-cli-ops-v1.yaml");
    let operation = catalog
        .split_once("  - id: pr.pending-review.delete\n")
        .expect("pending-review operation")
        .1
        .split_once("  - id: pr.tasks\n")
        .expect("pr.tasks follows pending-review operation")
        .0;

    assert!(operation.contains("viewerDidAuthor"));
    assert!(operation.contains("viewerCanDelete"));
    assert!(operation.contains("--expected-head"));
    assert!(operation.contains("--expected-commit"));
    assert!(operation.contains("--expected-body-file"));
    assert!(operation.contains("--confirm-abandoned"));
    assert!(operation.contains("comments(first: 1)"));
    assert!(operation.contains("pending_review_inline_comments_present"));
    assert!(operation.contains("pending_review_pr_mismatch"));
    assert!(!operation.contains("gh api user"));
}

fn run_pending_delete_with_script(script: &str, review: &str) -> CmdOutput {
    let stub = StubEnv::new().gh_stub(script);
    run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "--repo",
            "acme/widgets",
            "--format",
            "json",
            "pr",
            "pending-review",
            "delete",
            "42",
            "--review",
            review,
            "--expected-head",
            "head-new",
            "--expected-commit",
            "head-new",
            "--expected-body",
            "Pending review summary",
            "--confirm-abandoned",
        ],
    )
}

#[test]
fn pr_pending_review_delete_verifies_and_deletes_the_exact_pending_node() {
    let stub = StubEnv::new();
    let capture = stub.tempdir.path().join("gh-calls.log");
    let script = format!(
        r#"#!/bin/sh
set -eu
printf '%s\n' "$*" >> {capture:?}
case "$1 $2" in
  "pr view")
    printf '%s\n' '{{"number":42,"url":"https://github.com/acme/widgets/pull/42","state":"OPEN","isDraft":false,"baseRefName":"main","headRefName":"feat/reviews","headRefOid":"head-new","title":"feat: reviews","body":""}}'
    ;;
  "api graphql")
    case "$*" in
      *"deletePullRequestReview(input:"*)
        printf '%s\n' '{{"data":{{"deletePullRequestReview":{{"pullRequestReview":{{"id":"PRR_pending","url":"https://github.com/acme/widgets/pull/42#pullrequestreview-102"}}}}}}}}'
        ;;
      *"comments(first: 1)"*)
        printf '%s\n' '{{"data":{{"node":{{"id":"PRR_pending","url":"https://github.com/acme/widgets/pull/42#pullrequestreview-102","author":{{"login":"example-review-bot[bot]"}},"state":"PENDING","commit":{{"oid":"head-new"}},"body":"Pending review summary","viewerDidAuthor":true,"viewerCanDelete":true,"comments":{{"totalCount":0}},"pullRequest":{{"number":42,"url":"https://github.com/acme/widgets/pull/42","headRefOid":"head-new"}}}}}}}}'
        ;;
      *)
        printf '%s\n' '{{"data":{{"repository":{{"pullRequest":{{"headRefOid":"head-new","reviews":{{"nodes":[{{"id":"PRR_pending","databaseId":102,"url":"https://github.com/acme/widgets/pull/42#pullrequestreview-102","author":{{"login":"example-review-bot[bot]"}},"state":"PENDING","commit":{{"oid":"head-new"}},"viewerDidAuthor":true,"viewerCanDelete":true,"body":"Pending review summary"}}],"pageInfo":{{"hasNextPage":false,"endCursor":null}}}}}}}}}}}}'
        ;;
    esac
    ;;
  *)
    echo "unexpected gh args: $*" >&2
    exit 99
    ;;
esac
"#
    );
    let stub = stub.gh_stub(&script);

    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "--repo",
            "acme/widgets",
            "--format",
            "json",
            "pr",
            "pending-review",
            "delete",
            "42",
            "--review",
            "PRR_pending",
            "--expected-head",
            "head-new",
            "--expected-commit",
            "head-new",
            "--expected-body",
            "Pending review summary",
            "--confirm-abandoned",
        ],
    );

    assert_eq!(out.code, 0, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(
        env["schema_version"],
        "cli.forge-cli.pr.pending-review.delete.v1"
    );
    assert_eq!(env["data"]["number"], 42);
    assert_eq!(env["data"]["review_id"], "PRR_pending");
    assert_eq!(env["data"]["author"], "example-review-bot[bot]");
    assert_eq!(env["data"]["head_sha"], "head-new");

    let calls = fs::read_to_string(capture).expect("read gh calls");
    assert!(!calls.contains("api user"), "{calls}");
    assert!(calls.contains("states: [PENDING]"), "{calls}");
    assert!(calls.contains("deletePullRequestReview(input:"), "{calls}");
    assert!(calls.contains("reviewId=PRR_pending"), "{calls}");
}

#[test]
fn pr_pending_review_delete_rejects_confirmed_body_drift_without_mutating() {
    let stub = StubEnv::new();
    let capture = stub.tempdir.path().join("gh-calls.log");
    let expected_body = stub.tempdir.path().join("expected-review.md");
    fs::write(&expected_body, "Expected pending review body\n").expect("expected body");
    let script = format!(
        r#"#!/bin/sh
set -eu
printf '%s\n' "$*" >> {capture:?}
case "$1 $2" in
  "pr view")
    printf '%s\n' '{{"number":42,"url":"https://github.com/acme/widgets/pull/42","state":"OPEN","isDraft":false,"baseRefName":"main","headRefName":"feat/reviews","headRefOid":"head-new","title":"feat: reviews","body":""}}'
    ;;
  "api graphql")
    case "$*" in
      *"deletePullRequestReview(input:"*)
        echo "delete must not run after body drift" >&2
        exit 99
        ;;
      *)
        printf '%s\n' '{{"data":{{"repository":{{"pullRequest":{{"headRefOid":"head-new","reviews":{{"nodes":[{{"id":"PRR_pending","databaseId":102,"url":"https://github.com/acme/widgets/pull/42#pullrequestreview-102","author":{{"login":"example-review-bot[bot]"}},"state":"PENDING","commit":{{"oid":"head-new"}},"viewerDidAuthor":true,"viewerCanDelete":true,"body":"Concurrently changed body"}}],"pageInfo":{{"hasNextPage":false,"endCursor":null}}}}}}}}}}}}'
        ;;
    esac
    ;;
  *)
    echo "unexpected gh args: $*" >&2
    exit 99
    ;;
esac
"#
    );
    let stub = stub.gh_stub(&script);

    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "--repo",
            "acme/widgets",
            "--format",
            "json",
            "pr",
            "pending-review",
            "delete",
            "42",
            "--review",
            "PRR_pending",
            "--expected-head",
            "head-new",
            "--expected-commit",
            "head-new",
            "--expected-body-file",
            expected_body.to_str().expect("body path"),
            "--confirm-abandoned",
        ],
    );

    assert_eq!(out.code, 65, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["error"]["code"], "pending_review_body_mismatch");
    let calls = fs::read_to_string(capture).expect("read gh calls");
    assert!(!calls.contains("deletePullRequestReview(input:"), "{calls}");
}

#[test]
fn pr_pending_review_delete_rejects_expected_head_drift_without_mutating() {
    let stub = StubEnv::new();
    let capture = stub.tempdir.path().join("gh-calls.log");
    let script = format!(
        r#"#!/bin/sh
set -eu
printf '%s\n' "$*" >> {capture:?}
case "$1 $2" in
  "pr view")
    printf '%s\n' '{{"number":42,"url":"https://github.com/acme/widgets/pull/42","state":"OPEN","isDraft":false,"baseRefName":"main","headRefName":"feat/reviews","headRefOid":"head-new","title":"feat: reviews","body":""}}'
    ;;
  "api graphql")
    case "$*" in
      *"deletePullRequestReview(input:"*)
        echo "delete must not run after head drift" >&2
        exit 99
        ;;
      *)
        printf '%s\n' '{{"data":{{"repository":{{"pullRequest":{{"headRefOid":"head-changed","reviews":{{"nodes":[],"pageInfo":{{"hasNextPage":false,"endCursor":null}}}}}}}}}}}}'
        ;;
    esac
    ;;
  *)
    echo "unexpected gh args: $*" >&2
    exit 99
    ;;
esac
"#
    );
    let stub = stub.gh_stub(&script);

    let out = run_pending_delete_with_script_for_stub(&stub, "PRR_pending", "Pending");

    assert_eq!(out.code, 65, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["error"]["code"], "pending_review_head_mismatch");
    let calls = fs::read_to_string(capture).expect("read gh calls");
    assert!(!calls.contains("deletePullRequestReview(input:"), "{calls}");
}

#[test]
fn pr_pending_review_delete_rejects_expected_commit_drift_without_mutating() {
    let stub = StubEnv::new();
    let capture = stub.tempdir.path().join("gh-calls.log");
    let script = format!(
        r#"#!/bin/sh
set -eu
printf '%s\n' "$*" >> {capture:?}
case "$1 $2" in
  "pr view")
    printf '%s\n' '{{"number":42,"url":"https://github.com/acme/widgets/pull/42","state":"OPEN","isDraft":false,"baseRefName":"main","headRefName":"feat/reviews","headRefOid":"head-new","title":"feat: reviews","body":""}}'
    ;;
  "api graphql")
    case "$*" in
      *"deletePullRequestReview(input:"*)
        echo "delete must not run after commit drift" >&2
        exit 99
        ;;
      *)
        printf '%s\n' '{{"data":{{"repository":{{"pullRequest":{{"headRefOid":"head-new","reviews":{{"nodes":[{{"id":"PRR_pending","url":"https://github.com/acme/widgets/pull/42#pullrequestreview-102","author":{{"login":"reviewer"}},"state":"PENDING","commit":{{"oid":"head-changed"}},"body":"Pending","viewerDidAuthor":true,"viewerCanDelete":true}}],"pageInfo":{{"hasNextPage":false,"endCursor":null}}}}}}}}}}}}'
        ;;
    esac
    ;;
  *)
    echo "unexpected gh args: $*" >&2
    exit 99
    ;;
esac
"#
    );
    let stub = stub.gh_stub(&script);

    let out = run_pending_delete_with_script_for_stub(&stub, "PRR_pending", "Pending");

    assert_eq!(out.code, 65, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["error"]["code"], "pending_review_commit_mismatch");
    let calls = fs::read_to_string(capture).expect("read gh calls");
    assert!(!calls.contains("deletePullRequestReview(input:"), "{calls}");
}

fn run_pending_delete_with_script_for_stub(
    stub: &StubEnv,
    review: &str,
    expected_body: &str,
) -> CmdOutput {
    run_forge_cli(
        stub,
        &[
            "--provider",
            "github",
            "--repo",
            "acme/widgets",
            "--format",
            "json",
            "pr",
            "pending-review",
            "delete",
            "42",
            "--review",
            review,
            "--expected-head",
            "head-new",
            "--expected-commit",
            "head-new",
            "--expected-body",
            expected_body,
            "--confirm-abandoned",
        ],
    )
}

#[test]
fn pr_pending_review_delete_requires_explicit_abandoned_confirmation() {
    let stub = StubEnv::new();
    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "--repo",
            "acme/widgets",
            "--format",
            "json",
            "pr",
            "pending-review",
            "delete",
            "42",
            "--review",
            "PRR_pending",
            "--expected-head",
            "head-new",
            "--expected-commit",
            "head-new",
            "--expected-body",
            "Pending",
        ],
    );

    assert_eq!(out.code, 64, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["error"]["code"], "parse-error");
}

#[test]
fn pr_pending_review_delete_requires_every_content_guard() {
    let cases: [(&str, &[&str]); 3] = [
        (
            "missing-head",
            &[
                "--expected-commit",
                "head-new",
                "--expected-body",
                "Pending",
            ],
        ),
        (
            "missing-commit",
            &["--expected-head", "head-new", "--expected-body", "Pending"],
        ),
        (
            "missing-body",
            &[
                "--expected-head",
                "head-new",
                "--expected-commit",
                "head-new",
            ],
        ),
    ];

    for (name, guards) in cases {
        let stub = StubEnv::new();
        let mut argv = vec![
            "--provider",
            "github",
            "--repo",
            "acme/widgets",
            "--format",
            "json",
            "pr",
            "pending-review",
            "delete",
            "42",
            "--review",
            "PRR_pending",
        ];
        argv.extend_from_slice(guards);
        argv.push("--confirm-abandoned");
        let out = run_forge_cli(&stub, &argv);

        assert_eq!(
            out.code, 64,
            "case={name}\nstdout={}\nstderr={}",
            out.stdout, out.stderr
        );
        let env = parse_envelope(&out.stdout);
        assert_eq!(env["error"]["code"], "parse-error", "case={name}");
    }
}

#[test]
fn pr_pending_review_delete_rejects_an_author_mismatch_without_mutating() {
    let stub = StubEnv::new();
    let capture = stub.tempdir.path().join("gh-calls.log");
    let script = format!(
        r#"#!/bin/sh
set -eu
printf '%s\n' "$*" >> {capture:?}
case "$1 $2" in
  "pr view")
    printf '%s\n' '{{"number":42,"url":"https://github.com/acme/widgets/pull/42","state":"OPEN","isDraft":false,"baseRefName":"main","headRefName":"feat/reviews","headRefOid":"head-new","title":"feat: reviews","body":""}}'
    ;;
  "api graphql")
    case "$*" in
      *"deletePullRequestReview(input:"*)
        echo "delete must not run after an author mismatch" >&2
        exit 99
        ;;
      *)
        printf '%s\n' '{{"data":{{"repository":{{"pullRequest":{{"headRefOid":"head-new","reviews":{{"nodes":[{{"id":"PRR_pending","databaseId":102,"url":"https://github.com/acme/widgets/pull/42#pullrequestreview-102","author":{{"login":"example-review-bot[bot]"}},"state":"PENDING","commit":null,"viewerDidAuthor":false,"viewerCanDelete":true,"body":"Pending review summary"}}],"pageInfo":{{"hasNextPage":false,"endCursor":null}}}}}}}}}}}}'
        ;;
    esac
    ;;
  *)
    echo "unexpected gh args: $*" >&2
    exit 99
    ;;
esac
"#
    );
    let stub = stub.gh_stub(&script);

    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "--repo",
            "acme/widgets",
            "--format",
            "json",
            "pr",
            "pending-review",
            "delete",
            "42",
            "--review",
            "PRR_pending",
            "--expected-head",
            "head-new",
            "--expected-commit",
            "head-new",
            "--expected-body",
            "Pending review summary",
            "--confirm-abandoned",
        ],
    );

    assert_eq!(out.code, 65, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["error"]["code"], "pending_review_author_mismatch");
    let calls = fs::read_to_string(capture).expect("read gh calls");
    assert!(!calls.contains("deletePullRequestReview(input:"), "{calls}");
}

#[test]
fn pr_pending_review_delete_rejects_a_non_deletable_owned_review_without_mutating() {
    let stub = StubEnv::new();
    let capture = stub.tempdir.path().join("gh-calls.log");
    let script = format!(
        r#"#!/bin/sh
set -eu
printf '%s\n' "$*" >> {capture:?}
case "$1 $2" in
  "pr view")
    printf '%s\n' '{{"number":42,"url":"https://github.com/acme/widgets/pull/42","state":"OPEN","isDraft":false,"baseRefName":"main","headRefName":"feat/reviews","headRefOid":"head-new","title":"feat: reviews","body":""}}'
    ;;
  "api graphql")
    case "$*" in
      *"deletePullRequestReview(input:"*)
        echo "delete must not run when viewerCanDelete is false" >&2
        exit 99
        ;;
      *)
        printf '%s\n' '{{"data":{{"repository":{{"pullRequest":{{"headRefOid":"head-new","reviews":{{"nodes":[{{"id":"PRR_pending","databaseId":102,"url":"https://github.com/acme/widgets/pull/42#pullrequestreview-102","author":{{"login":"example-review-bot[bot]"}},"state":"PENDING","commit":null,"viewerDidAuthor":true,"viewerCanDelete":false,"body":"Pending review summary"}}],"pageInfo":{{"hasNextPage":false,"endCursor":null}}}}}}}}}}}}'
        ;;
    esac
    ;;
  *)
    echo "unexpected gh args: $*" >&2
    exit 99
    ;;
esac
"#
    );
    let stub = stub.gh_stub(&script);

    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "--repo",
            "acme/widgets",
            "--format",
            "json",
            "pr",
            "pending-review",
            "delete",
            "42",
            "--review",
            "PRR_pending",
            "--expected-head",
            "head-new",
            "--expected-commit",
            "head-new",
            "--expected-body",
            "Pending review summary",
            "--confirm-abandoned",
        ],
    );

    assert_eq!(out.code, 65, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["error"]["code"], "pending_review_not_deletable");
    let calls = fs::read_to_string(capture).expect("read gh calls");
    assert!(!calls.contains("deletePullRequestReview(input:"), "{calls}");
}

#[test]
fn pr_pending_review_delete_rejects_a_non_pending_review_without_mutating() {
    let stub = StubEnv::new();
    let capture = stub.tempdir.path().join("gh-calls.log");
    let script = format!(
        r#"#!/bin/sh
set -eu
printf '%s\n' "$*" >> {capture:?}
case "$1 $2" in
  "pr view")
    printf '%s\n' '{{"number":42,"url":"https://github.com/acme/widgets/pull/42","state":"OPEN","isDraft":false,"baseRefName":"main","headRefName":"feat/reviews","headRefOid":"head-new","title":"feat: reviews","body":""}}'
    ;;
  "api graphql")
    printf '%s\n' '{{"data":{{"repository":{{"pullRequest":{{"headRefOid":"head-new","reviews":{{"nodes":[],"pageInfo":{{"hasNextPage":false,"endCursor":null}}}}}}}}}}}}'
    ;;
  *)
    echo "unexpected gh args: $*" >&2
    exit 99
    ;;
esac
"#
    );
    let stub = stub.gh_stub(&script);

    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "--repo",
            "acme/widgets",
            "--format",
            "json",
            "pr",
            "pending-review",
            "delete",
            "42",
            "--review",
            "PRR_submitted",
            "--expected-head",
            "head-new",
            "--expected-commit",
            "head-new",
            "--expected-body",
            "Pending review summary",
            "--confirm-abandoned",
        ],
    );

    assert_eq!(out.code, 65, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["error"]["code"], "pending_review_not_found");
    let calls = fs::read_to_string(capture).expect("read gh calls");
    assert!(!calls.contains("api user"), "{calls}");
    assert!(!calls.contains("deletePullRequestReview(input:"), "{calls}");
}

#[test]
fn pr_pending_review_delete_dry_run_is_offline_and_renders_every_plan() {
    let stub = StubEnv::new();
    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "--repo",
            "acme/widgets",
            "--dry-run",
            "--format",
            "json",
            "pr",
            "pending-review",
            "delete",
            "42",
            "--review",
            "PRR_pending",
            "--expected-head",
            "head-new",
            "--expected-commit",
            "head-new",
            "--expected-body",
            "Pending review summary",
            "--confirm-abandoned",
        ],
    );

    assert_eq!(out.code, 0, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["data"]["review_id"], "PRR_pending");
    assert_eq!(env["data"]["expected_head"], "head-new");
    assert_eq!(env["data"]["expected_commit"], "head-new");
    assert_eq!(env["data"]["expected_inline_comment_count"], 0);
    assert_eq!(env["data"]["confirmed_abandoned"], true);
    assert!(env["data"].get("expected_body").is_none());
    assert!(env["data"].get("expected_body_file").is_none());
    assert!(env["data"]["guard_plan"].as_array().is_some());
    assert!(env["data"]["snapshot_plan"].as_array().is_some());
    assert!(
        env["data"]["target_plan"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item
                .as_str()
                .is_some_and(|item| item.contains("comments(first: 1)")))
    );
    assert!(
        env["data"]["target_plan"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item == "review=PRR_pending")
    );
    assert!(
        env["data"]["snapshot_plan"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item
                .as_str()
                .is_some_and(|item| item.contains("states: [PENDING]")))
    );
    assert!(
        env["data"]["delete_plan"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item == "reviewId=PRR_pending")
    );
}

#[test]
fn pr_pending_review_delete_dry_run_validates_body_file_without_disclosing_it() {
    let stub = StubEnv::new();
    let expected_body = stub
        .tempdir
        .path()
        .join("sensitive-review-body-location.md");
    let sentinel = "private pending review body sentinel";
    fs::write(&expected_body, sentinel).expect("write expected review body");
    let expected_body_path = expected_body.to_str().expect("body path");

    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "--repo",
            "acme/widgets",
            "--dry-run",
            "--format",
            "json",
            "pr",
            "pending-review",
            "delete",
            "42",
            "--review",
            "PRR_pending",
            "--expected-head",
            "head-new",
            "--expected-commit",
            "head-new",
            "--expected-body-file",
            expected_body_path,
            "--confirm-abandoned",
        ],
    );

    assert_eq!(out.code, 0, "stdout={}\nstderr={}", out.stdout, out.stderr);
    assert!(!out.stdout.contains(sentinel), "{}", out.stdout);
    assert!(!out.stderr.contains(sentinel), "{}", out.stderr);
    assert!(!out.stdout.contains(expected_body_path), "{}", out.stdout);
    assert!(!out.stderr.contains(expected_body_path), "{}", out.stderr);
    let env = parse_envelope(&out.stdout);
    assert!(env["data"].get("expected_body").is_none());
    assert!(env["data"].get("expected_body_file").is_none());
}

#[test]
fn pr_pending_review_delete_dry_run_redacts_an_unreadable_body_file_path() {
    let stub = StubEnv::new();
    let missing_body = stub.tempdir.path().join("sensitive-missing-review-body.md");
    let missing_body_path = missing_body.to_str().expect("body path");

    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "--repo",
            "acme/widgets",
            "--dry-run",
            "--format",
            "json",
            "pr",
            "pending-review",
            "delete",
            "42",
            "--review",
            "PRR_pending",
            "--expected-head",
            "head-new",
            "--expected-commit",
            "head-new",
            "--expected-body-file",
            missing_body_path,
            "--confirm-abandoned",
        ],
    );

    assert_eq!(out.code, 70, "stdout={}\nstderr={}", out.stdout, out.stderr);
    assert!(!out.stdout.contains(missing_body_path), "{}", out.stdout);
    assert!(!out.stderr.contains(missing_body_path), "{}", out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["error"]["code"], "software_error");
    assert_eq!(
        env["error"]["message"],
        "failed to read --expected-body-file"
    );
}

#[test]
fn pr_pending_review_delete_rejects_an_oversized_body_file_without_disclosing_it() {
    let stub = StubEnv::new();
    let expected_body = stub
        .tempdir
        .path()
        .join("sensitive-oversized-review-body.md");
    let sentinel = "private oversized pending review body sentinel";
    let body = format!("{sentinel}{}", "x".repeat(64 * 1024));
    fs::write(&expected_body, &body).expect("write oversized review body");
    let expected_body_path = expected_body.to_str().expect("body path");

    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "--repo",
            "acme/widgets",
            "--dry-run",
            "--format",
            "json",
            "pr",
            "pending-review",
            "delete",
            "42",
            "--review",
            "PRR_pending",
            "--expected-head",
            "head-new",
            "--expected-commit",
            "head-new",
            "--expected-body-file",
            expected_body_path,
            "--confirm-abandoned",
        ],
    );

    assert_eq!(out.code, 65, "stdout={}\nstderr={}", out.stdout, out.stderr);
    assert!(!out.stdout.contains(sentinel), "{}", out.stdout);
    assert!(!out.stderr.contains(sentinel), "{}", out.stderr);
    assert!(!out.stdout.contains(expected_body_path), "{}", out.stdout);
    assert!(!out.stderr.contains(expected_body_path), "{}", out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["error"]["code"], "pending_review_body_too_large");
}

#[test]
fn pr_pending_review_delete_enforces_body_byte_limit_across_utf8_boundaries() {
    let cases = [
        ("exact-limit", "x".repeat(64 * 1024), 0, None),
        (
            "one-byte-over",
            "x".repeat(64 * 1024 + 1),
            65,
            Some("pending_review_body_too_large"),
        ),
        (
            "split-multibyte-boundary",
            format!("{}€", "x".repeat(64 * 1024 - 1)),
            65,
            Some("pending_review_body_too_large"),
        ),
    ];

    for (name, body, expected_exit, expected_error) in cases {
        let stub = StubEnv::new();
        let expected_body = stub.tempdir.path().join(format!("{name}-body.md"));
        fs::write(&expected_body, &body).expect("write boundary review body");
        let expected_body_path = expected_body.to_str().expect("body path");
        let out = run_forge_cli(
            &stub,
            &[
                "--provider",
                "github",
                "--repo",
                "acme/widgets",
                "--dry-run",
                "--format",
                "json",
                "pr",
                "pending-review",
                "delete",
                "42",
                "--review",
                "PRR_pending",
                "--expected-head",
                "head-new",
                "--expected-commit",
                "head-new",
                "--expected-body-file",
                expected_body_path,
                "--confirm-abandoned",
            ],
        );

        assert_eq!(
            out.code, expected_exit,
            "case={name}\nstdout={}\nstderr={}",
            out.stdout, out.stderr
        );
        assert!(!out.stdout.contains(expected_body_path), "{}", out.stdout);
        assert!(!out.stderr.contains(expected_body_path), "{}", out.stderr);
        assert!(!out.stdout.contains('€'), "{}", out.stdout);
        assert!(!out.stderr.contains('€'), "{}", out.stderr);
        if let Some(expected_error) = expected_error {
            let env = parse_envelope(&out.stdout);
            assert_eq!(env["error"]["code"], expected_error, "case={name}");
        }
    }
}

#[test]
fn pr_pending_review_delete_bounds_live_stdin_before_provider_calls() {
    let stub = StubEnv::new();
    let sentinel = "private oversized stdin review body sentinel";
    let body = format!("{sentinel}{}", "x".repeat(64 * 1024));

    let out = run_forge_cli_with_stdin(
        &stub,
        &[
            "--provider",
            "github",
            "--repo",
            "acme/widgets",
            "--format",
            "json",
            "pr",
            "pending-review",
            "delete",
            "42",
            "--review",
            "PRR_pending",
            "--expected-head",
            "head-new",
            "--expected-commit",
            "head-new",
            "--expected-body-file",
            "-",
            "--confirm-abandoned",
        ],
        &body,
    );

    assert_eq!(out.code, 65, "stdout={}\nstderr={}", out.stdout, out.stderr);
    assert!(!out.stdout.contains(sentinel), "{}", out.stdout);
    assert!(!out.stderr.contains(sentinel), "{}", out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["error"]["code"], "pending_review_body_too_large");
}

#[test]
fn pr_pending_review_delete_revalidates_the_exact_target_before_mutating() {
    let cases = [
        (
            "body-drift",
            r#"{"data":{"node":{"id":"PRR_pending","url":"https://github.com/acme/widgets/pull/42#pullrequestreview-102","author":{"login":"reviewer"},"state":"PENDING","commit":{"oid":"head-new"},"body":"Changed after snapshot","viewerDidAuthor":true,"viewerCanDelete":true,"comments":{"totalCount":0},"pullRequest":{"number":42,"url":"https://github.com/acme/widgets/pull/42","headRefOid":"head-new"}}}}"#,
            "pending_review_body_mismatch",
        ),
        (
            "inline-comments",
            r#"{"data":{"node":{"id":"PRR_pending","url":"https://github.com/acme/widgets/pull/42#pullrequestreview-102","author":{"login":"reviewer"},"state":"PENDING","commit":{"oid":"head-new"},"body":"Pending review summary","viewerDidAuthor":true,"viewerCanDelete":true,"comments":{"totalCount":1},"pullRequest":{"number":42,"url":"https://github.com/acme/widgets/pull/42","headRefOid":"head-new"}}}}"#,
            "pending_review_inline_comments_present",
        ),
        (
            "pr-mismatch",
            r#"{"data":{"node":{"id":"PRR_pending","url":"https://github.com/acme/widgets/pull/43#pullrequestreview-102","author":{"login":"reviewer"},"state":"PENDING","commit":{"oid":"head-new"},"body":"Pending review summary","viewerDidAuthor":true,"viewerCanDelete":true,"comments":{"totalCount":0},"pullRequest":{"number":43,"url":"https://github.com/acme/widgets/pull/43","headRefOid":"head-new"}}}}"#,
            "pending_review_pr_mismatch",
        ),
        (
            "partial-graphql",
            r#"{"errors":[{"message":"partial"}],"data":{"node":{"id":"PRR_pending"}}}"#,
            "review_snapshot_incomplete",
        ),
        (
            "missing-inline-comment-count",
            r#"{"data":{"node":{"id":"PRR_pending","url":"https://github.com/acme/widgets/pull/42#pullrequestreview-102","author":{"login":"reviewer"},"state":"PENDING","commit":{"oid":"head-new"},"body":"Pending review summary","viewerDidAuthor":true,"viewerCanDelete":true,"comments":{},"pullRequest":{"number":42,"url":"https://github.com/acme/widgets/pull/42","headRefOid":"head-new"}}}}"#,
            "review_snapshot_incomplete",
        ),
    ];

    for (name, target, expected_code) in cases {
        let script = r#"#!/bin/sh
set -eu
case "$1 $2" in
  "pr view")
    printf '%s\n' '{"number":42,"url":"https://github.com/acme/widgets/pull/42","state":"OPEN","isDraft":false,"baseRefName":"main","headRefName":"feat/reviews","headRefOid":"head-new","title":"feat: reviews","body":""}'
    ;;
  "api graphql")
    case "$*" in
      *"deletePullRequestReview(input:"*)
        echo "delete must not run after exact-target drift" >&2
        exit 99
        ;;
      *"comments(first: 1)"*)
        printf '%s\n' '__TARGET__'
        ;;
      *)
        printf '%s\n' '{"data":{"repository":{"pullRequest":{"headRefOid":"head-new","reviews":{"nodes":[{"id":"PRR_pending","url":"https://github.com/acme/widgets/pull/42#pullrequestreview-102","author":{"login":"reviewer"},"state":"PENDING","commit":{"oid":"head-new"},"body":"Pending review summary","viewerDidAuthor":true,"viewerCanDelete":true}],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}}'
        ;;
    esac
    ;;
  *)
    echo "unexpected gh args: $*" >&2
    exit 99
    ;;
esac
"#
        .replace("__TARGET__", target);
        let out = run_pending_delete_with_script(&script, "PRR_pending");

        assert_eq!(
            out.code, 65,
            "case={name}\nstdout={}\nstderr={}",
            out.stdout, out.stderr
        );
        let env = parse_envelope(&out.stdout);
        assert_eq!(env["error"]["code"], expected_code, "case={name}");
    }
}

#[test]
fn pr_pending_review_delete_finds_the_exact_node_on_a_later_page() {
    let stub = StubEnv::new();
    let capture = stub.tempdir.path().join("gh-calls.log");
    let script = format!(
        r#"#!/bin/sh
set -eu
printf '%s\n' "$*" >> {capture:?}
case "$1 $2" in
  "pr view")
    printf '%s\n' '{{"number":42,"url":"https://github.com/acme/widgets/pull/42","state":"OPEN","isDraft":false,"baseRefName":"main","headRefName":"feat/reviews","headRefOid":"head-new","title":"feat: reviews","body":""}}'
    ;;
  "api graphql")
    case "$*" in
      *"deletePullRequestReview(input:"*)
        printf '%s\n' '{{"data":{{"deletePullRequestReview":{{"pullRequestReview":{{"id":"PRR_page_2","url":"https://github.com/acme/widgets/pull/42#pullrequestreview-202"}}}}}}}}'
        ;;
      *"comments(first: 1)"*)
        printf '%s\n' '{{"data":{{"node":{{"id":"PRR_page_2","url":"https://github.com/acme/widgets/pull/42#pullrequestreview-202","author":{{"login":"example-review-bot[bot]"}},"state":"PENDING","commit":{{"oid":"head-new"}},"body":"target","viewerDidAuthor":true,"viewerCanDelete":true,"comments":{{"totalCount":0}},"pullRequest":{{"number":42,"url":"https://github.com/acme/widgets/pull/42","headRefOid":"head-new"}}}}}}}}'
        ;;
      *"after=cursor-1"*)
        printf '%s\n' '{{"data":{{"repository":{{"pullRequest":{{"headRefOid":"head-new","reviews":{{"nodes":[{{"id":"PRR_page_2","databaseId":202,"url":"https://github.com/acme/widgets/pull/42#pullrequestreview-202","author":{{"login":"example-review-bot[bot]"}},"state":"PENDING","commit":{{"oid":"head-new"}},"viewerDidAuthor":true,"viewerCanDelete":true,"body":"target"}}],"pageInfo":{{"hasNextPage":false,"endCursor":null}}}}}}}}}}}}'
        ;;
      *)
        printf '%s\n' '{{"data":{{"repository":{{"pullRequest":{{"headRefOid":"head-new","reviews":{{"nodes":[{{"id":"PRR_page_1","databaseId":201,"url":"https://github.com/acme/widgets/pull/42#pullrequestreview-201","author":{{"login":"other"}},"state":"PENDING","commit":null,"viewerDidAuthor":false,"viewerCanDelete":false,"body":"other"}}],"pageInfo":{{"hasNextPage":true,"endCursor":"cursor-1"}}}}}}}}}}}}'
        ;;
    esac
    ;;
  *)
    echo "unexpected gh args: $*" >&2
    exit 99
    ;;
esac
"#
    );
    let stub = stub.gh_stub(&script);

    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "--repo",
            "acme/widgets",
            "--format",
            "json",
            "pr",
            "pending-review",
            "delete",
            "42",
            "--review",
            "PRR_page_2",
            "--expected-head",
            "head-new",
            "--expected-commit",
            "head-new",
            "--expected-body",
            "target",
            "--confirm-abandoned",
        ],
    );

    assert_eq!(out.code, 0, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["data"]["review_id"], "PRR_page_2");
    let calls = fs::read_to_string(capture).expect("read gh calls");
    assert!(calls.contains("after=cursor-1"), "{calls}");
    assert!(calls.contains("reviewId=PRR_page_2"), "{calls}");
}

#[test]
fn pr_pending_review_delete_rejects_an_oversized_provider_body_without_disclosing_it() {
    let sentinel = "private oversized provider review body sentinel";
    let body = format!("{sentinel}{}", "x".repeat(64 * 1024));
    let snapshot = serde_json::json!({
        "data": {
            "repository": {
                "pullRequest": {
                    "headRefOid": "head-new",
                    "reviews": {
                        "nodes": [{
                            "id": "PRR_pending",
                            "url": "https://github.com/acme/widgets/pull/42#pullrequestreview-102",
                            "author": { "login": "reviewer" },
                            "state": "PENDING",
                            "commit": { "oid": "head-new" },
                            "body": body,
                            "viewerDidAuthor": true,
                            "viewerCanDelete": true
                        }],
                        "pageInfo": { "hasNextPage": false, "endCursor": null }
                    }
                }
            }
        }
    })
    .to_string();
    let script = r#"#!/bin/sh
set -eu
case "$1 $2" in
  "pr view")
    printf '%s\n' '{"number":42,"url":"https://github.com/acme/widgets/pull/42","state":"OPEN","isDraft":false,"baseRefName":"main","headRefName":"feat/reviews","headRefOid":"head-new","title":"feat: reviews","body":""}'
    ;;
  "api graphql")
    case "$*" in
      *"deletePullRequestReview(input:"*|*"comments(first: 1)"*)
        echo "mutation and exact-target read must not run for an oversized snapshot" >&2
        exit 99
        ;;
      *)
        printf '%s\n' '__SNAPSHOT__'
        ;;
    esac
    ;;
  *)
    echo "unexpected gh args: $*" >&2
    exit 99
    ;;
esac
"#
    .replace("__SNAPSHOT__", &snapshot);

    let out = run_pending_delete_with_script(&script, "PRR_pending");

    assert_eq!(out.code, 65, "stdout={}\nstderr={}", out.stdout, out.stderr);
    assert!(!out.stdout.contains(sentinel), "{}", out.stdout);
    assert!(!out.stderr.contains(sentinel), "{}", out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["error"]["code"], "review_snapshot_incomplete");
}

#[test]
fn pr_pending_review_delete_rejects_a_mismatched_mutation_receipt() {
    let stub = StubEnv::new().gh_stub(
        r#"#!/bin/sh
set -eu
case "$1 $2" in
  "pr view")
    printf '%s\n' '{"number":42,"url":"https://github.com/acme/widgets/pull/42","state":"OPEN","isDraft":false,"baseRefName":"main","headRefName":"feat/reviews","headRefOid":"head-new","title":"feat: reviews","body":""}'
    ;;
  "api graphql")
    case "$*" in
      *"deletePullRequestReview(input:"*)
        printf '%s\n' '{"data":{"deletePullRequestReview":{"pullRequestReview":{"id":"PRR_wrong","url":"https://github.com/acme/widgets/pull/42#pullrequestreview-999"}}}}'
        ;;
      *"comments(first: 1)"*)
        printf '%s\n' '{"data":{"node":{"id":"PRR_pending","url":"https://github.com/acme/widgets/pull/42#pullrequestreview-102","author":{"login":"reviewer"},"state":"PENDING","commit":{"oid":"head-new"},"body":"Pending","viewerDidAuthor":true,"viewerCanDelete":true,"comments":{"totalCount":0},"pullRequest":{"number":42,"url":"https://github.com/acme/widgets/pull/42","headRefOid":"head-new"}}}}'
        ;;
      *)
        printf '%s\n' '{"data":{"repository":{"pullRequest":{"headRefOid":"head-new","reviews":{"nodes":[{"id":"PRR_pending","databaseId":102,"url":"https://github.com/acme/widgets/pull/42#pullrequestreview-102","author":{"login":"reviewer"},"state":"PENDING","commit":{"oid":"head-new"},"viewerDidAuthor":true,"viewerCanDelete":true,"body":"Pending"}],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}}'
        ;;
    esac
    ;;
  *)
    echo "unexpected gh args: $*" >&2
    exit 99
    ;;
esac
"#,
    );

    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "--repo",
            "acme/widgets",
            "--format",
            "json",
            "pr",
            "pending-review",
            "delete",
            "42",
            "--review",
            "PRR_pending",
            "--expected-head",
            "head-new",
            "--expected-commit",
            "head-new",
            "--expected-body",
            "Pending",
            "--confirm-abandoned",
        ],
    );

    assert_eq!(out.code, 70, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert!(!env["ok"].as_bool().unwrap_or(true));
    assert!(
        env["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("different review"))
    );
}

#[test]
fn pr_pending_review_delete_classifies_invalid_mutation_receipts() {
    let cases = [
        ("invalid-json", "not-json", 70, "software_error"),
        (
            "graphql-error",
            r#"{"errors":[{"message":"denied"}],"data":{"deletePullRequestReview":null}}"#,
            1,
            "backend_error",
        ),
        (
            "missing-url",
            r#"{"data":{"deletePullRequestReview":{"pullRequestReview":{"id":"PRR_pending"}}}}"#,
            70,
            "software_error",
        ),
    ];

    for (name, receipt, exit_code, error_code) in cases {
        let script = r#"#!/bin/sh
set -eu
case "$1 $2" in
  "pr view")
    printf '%s\n' '{"number":42,"url":"https://github.com/acme/widgets/pull/42","state":"OPEN","isDraft":false,"baseRefName":"main","headRefName":"feat/reviews","headRefOid":"head-new","title":"feat: reviews","body":""}'
    ;;
  "api graphql")
    case "$*" in
      *"deletePullRequestReview(input:"*)
        printf '%s\n' '__RECEIPT__'
        ;;
      *"comments(first: 1)"*)
        printf '%s\n' '{"data":{"node":{"id":"PRR_pending","url":"https://github.com/acme/widgets/pull/42#pullrequestreview-102","author":{"login":"reviewer"},"state":"PENDING","commit":{"oid":"head-new"},"body":"Pending","viewerDidAuthor":true,"viewerCanDelete":true,"comments":{"totalCount":0},"pullRequest":{"number":42,"url":"https://github.com/acme/widgets/pull/42","headRefOid":"head-new"}}}}'
        ;;
      *)
        printf '%s\n' '{"data":{"repository":{"pullRequest":{"headRefOid":"head-new","reviews":{"nodes":[{"id":"PRR_pending","url":"https://github.com/acme/widgets/pull/42#pullrequestreview-102","author":{"login":"reviewer"},"state":"PENDING","commit":{"oid":"head-new"},"body":"Pending","viewerDidAuthor":true,"viewerCanDelete":true}],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}}'
        ;;
    esac
    ;;
  *)
    echo "unexpected gh args: $*" >&2
    exit 99
    ;;
esac
"#
        .replace("__RECEIPT__", receipt);
        let stub = StubEnv::new().gh_stub(&script);

        let out = run_forge_cli(
            &stub,
            &[
                "--provider",
                "github",
                "--repo",
                "acme/widgets",
                "--format",
                "json",
                "pr",
                "pending-review",
                "delete",
                "42",
                "--review",
                "PRR_pending",
                "--expected-head",
                "head-new",
                "--expected-commit",
                "head-new",
                "--expected-body",
                "Pending",
                "--confirm-abandoned",
            ],
        );

        assert_eq!(
            out.code, exit_code,
            "case={name}\nstdout={}\nstderr={}",
            out.stdout, out.stderr
        );
        let env = parse_envelope(&out.stdout);
        assert_eq!(env["ok"], false, "case={name}");
        assert_eq!(env["error"]["code"], error_code, "case={name}");
    }
}

#[test]
fn pr_pending_review_delete_rejects_incomplete_or_non_pending_snapshots() {
    let cases = [
        (
            "graphql-error",
            r#"{"errors":[{"message":"partial"}],"data":{"repository":{"pullRequest":null}}}"#,
            "review_snapshot_incomplete",
        ),
        (
            "missing-viewer-guard",
            r#"{"data":{"repository":{"pullRequest":{"headRefOid":"head-new","reviews":{"nodes":[{"id":"PRR_pending","url":"https://github.com/acme/widgets/pull/42#pullrequestreview-102","author":{"login":"reviewer"},"state":"PENDING","viewerDidAuthor":true}],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}}"#,
            "review_snapshot_incomplete",
        ),
        (
            "non-pending-node",
            r#"{"data":{"repository":{"pullRequest":{"headRefOid":"head-new","reviews":{"nodes":[{"id":"PRR_pending","url":"https://github.com/acme/widgets/pull/42#pullrequestreview-102","author":{"login":"reviewer"},"state":"COMMENTED","viewerDidAuthor":true,"viewerCanDelete":true}],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}}"#,
            "review_snapshot_incomplete",
        ),
        (
            "missing-commit",
            r#"{"data":{"repository":{"pullRequest":{"headRefOid":"head-new","reviews":{"nodes":[{"id":"PRR_pending","url":"https://github.com/acme/widgets/pull/42#pullrequestreview-102","author":{"login":"reviewer"},"state":"PENDING","commit":null,"body":"Pending review summary","viewerDidAuthor":true,"viewerCanDelete":true}],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}}"#,
            "pending_review_commit_mismatch",
        ),
        (
            "missing-body",
            r#"{"data":{"repository":{"pullRequest":{"headRefOid":"head-new","reviews":{"nodes":[{"id":"PRR_pending","url":"https://github.com/acme/widgets/pull/42#pullrequestreview-102","author":{"login":"reviewer"},"state":"PENDING","commit":{"oid":"head-new"},"viewerDidAuthor":true,"viewerCanDelete":true}],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}}"#,
            "review_snapshot_incomplete",
        ),
    ];

    for (name, snapshot, expected_code) in cases {
        let script = r#"#!/bin/sh
set -eu
case "$1 $2" in
  "pr view")
    printf '%s\n' '{"number":42,"url":"https://github.com/acme/widgets/pull/42","state":"OPEN","isDraft":false,"baseRefName":"main","headRefName":"feat/reviews","headRefOid":"head-new","title":"feat: reviews","body":""}'
    ;;
  "api graphql")
    case "$*" in
      *"deletePullRequestReview(input:"*)
        echo "delete must not run for an invalid snapshot" >&2
        exit 99
        ;;
      *)
        printf '%s\n' '__SNAPSHOT__'
        ;;
    esac
    ;;
  *)
    echo "unexpected gh args: $*" >&2
    exit 99
    ;;
esac
"#
        .replace("__SNAPSHOT__", snapshot);
        let out = run_pending_delete_with_script(&script, "PRR_pending");

        assert_eq!(
            out.code, 65,
            "case={name}\nstdout={}\nstderr={}",
            out.stdout, out.stderr
        );
        let env = parse_envelope(&out.stdout);
        assert_eq!(env["error"]["code"], expected_code, "case={name}");
    }
}

#[test]
fn pr_pending_review_delete_rejects_head_drift_while_paginating() {
    let script = r#"#!/bin/sh
set -eu
case "$1 $2" in
  "pr view")
    printf '%s\n' '{"number":42,"url":"https://github.com/acme/widgets/pull/42","state":"OPEN","isDraft":false,"baseRefName":"main","headRefName":"feat/reviews","headRefOid":"head-new","title":"feat: reviews","body":""}'
    ;;
  "api graphql")
    case "$*" in
      *"deletePullRequestReview(input:"*)
        echo "delete must not run after head drift" >&2
        exit 99
        ;;
      *"after=cursor-1"*)
        printf '%s\n' '{"data":{"repository":{"pullRequest":{"headRefOid":"head-changed","reviews":{"nodes":[],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}}'
        ;;
      *)
        printf '%s\n' '{"data":{"repository":{"pullRequest":{"headRefOid":"head-new","reviews":{"nodes":[],"pageInfo":{"hasNextPage":true,"endCursor":"cursor-1"}}}}}}'
        ;;
    esac
    ;;
  *)
    echo "unexpected gh args: $*" >&2
    exit 99
    ;;
esac
"#;

    let out = run_pending_delete_with_script(script, "PRR_pending");
    assert_eq!(out.code, 65, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["error"]["code"], "review_snapshot_incomplete");
    assert!(
        env["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("head changed"))
    );
}

#[test]
fn pr_pending_review_delete_continues_after_target_and_rejects_later_head_drift() {
    let script = r#"#!/bin/sh
set -eu
case "$1 $2" in
  "pr view")
    printf '%s\n' '{"number":42,"url":"https://github.com/acme/widgets/pull/42","state":"OPEN","isDraft":false,"baseRefName":"main","headRefName":"feat/reviews","headRefOid":"head-new","title":"feat: reviews","body":""}'
    ;;
  "api graphql")
    case "$*" in
      *"deletePullRequestReview(input:"*|*"comments(first: 1)"*)
        echo "exact-target read and delete must not run after later head drift" >&2
        exit 99
        ;;
      *"after=cursor-1"*)
        printf '%s\n' '{"data":{"repository":{"pullRequest":{"headRefOid":"head-changed","reviews":{"nodes":[],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}}'
        ;;
      *)
        printf '%s\n' '{"data":{"repository":{"pullRequest":{"headRefOid":"head-new","reviews":{"nodes":[{"id":"PRR_pending","url":"https://github.com/acme/widgets/pull/42#pullrequestreview-102","author":{"login":"reviewer"},"state":"PENDING","commit":{"oid":"head-new"},"body":"Pending review summary","viewerDidAuthor":true,"viewerCanDelete":true}],"pageInfo":{"hasNextPage":true,"endCursor":"cursor-1"}}}}}}'
        ;;
    esac
    ;;
  *)
    echo "unexpected gh args: $*" >&2
    exit 99
    ;;
esac
"#;

    let out = run_pending_delete_with_script(script, "PRR_pending");
    assert_eq!(out.code, 65, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["error"]["code"], "review_snapshot_incomplete");
    assert!(
        env["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("head changed"))
    );
}

#[test]
fn pr_pending_review_delete_rejects_a_repeated_pagination_cursor() {
    let script = r#"#!/bin/sh
set -eu
case "$1 $2" in
  "pr view")
    printf '%s\n' '{"number":42,"url":"https://github.com/acme/widgets/pull/42","state":"OPEN","isDraft":false,"baseRefName":"main","headRefName":"feat/reviews","headRefOid":"head-new","title":"feat: reviews","body":""}'
    ;;
  "api graphql")
    case "$*" in
      *"deletePullRequestReview(input:"*)
        echo "delete must not run after a repeated cursor" >&2
        exit 99
        ;;
      *)
        printf '%s\n' '{"data":{"repository":{"pullRequest":{"headRefOid":"head-new","reviews":{"nodes":[],"pageInfo":{"hasNextPage":true,"endCursor":"cursor-1"}}}}}}'
        ;;
    esac
    ;;
  *)
    echo "unexpected gh args: $*" >&2
    exit 99
    ;;
esac
"#;

    let out = run_pending_delete_with_script(script, "PRR_pending");
    assert_eq!(out.code, 65, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["error"]["code"], "review_snapshot_incomplete");
    assert!(
        env["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("repeated a cursor"))
    );
}

#[test]
fn pr_pending_review_delete_fails_explicitly_for_gitlab() {
    let stub = StubEnv::new();
    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "gitlab",
            "--repo",
            "acme/widgets",
            "--format",
            "json",
            "pr",
            "pending-review",
            "delete",
            "42",
            "--review",
            "PRR_pending",
            "--expected-head",
            "head-new",
            "--expected-commit",
            "head-new",
            "--expected-body",
            "Pending review summary",
            "--confirm-abandoned",
        ],
    );

    assert_eq!(out.code, 64, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["error"]["code"], "provider_unsupported");
}
