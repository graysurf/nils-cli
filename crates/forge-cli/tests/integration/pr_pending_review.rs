//! Recovery tests for an authenticated actor's provider-valid pending review.

use std::fs;

use pretty_assertions::assert_eq;

use super::support::{StubEnv, parse_envelope, run_forge_cli};

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
  "api user")
    printf '%s\n' 'example-review-bot[bot]'
    ;;
  "api graphql")
    case "$*" in
      *"deletePullRequestReview(input:"*)
        printf '%s\n' '{{"data":{{"deletePullRequestReview":{{"pullRequestReview":{{"id":"PRR_pending","url":"https://github.com/acme/widgets/pull/42#pullrequestreview-102"}}}}}}}}'
        ;;
      *)
        printf '%s\n' '{{"data":{{"repository":{{"pullRequest":{{"headRefOid":"head-new","reviews":{{"nodes":[{{"id":"PRR_pending","databaseId":102,"url":"https://github.com/acme/widgets/pull/42#pullrequestreview-102","author":{{"login":"example-review-bot[bot]"}},"state":"PENDING","commit":{{"oid":"head-new"}},"submittedAt":null,"body":"Pending review summary"}}],"pageInfo":{{"hasNextPage":false,"endCursor":null}}}}}}}}}}}}'
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
    assert!(
        calls.contains("api user"),
        "identity guard missing: {calls}"
    );
    assert!(calls.contains("deletePullRequestReview(input:"), "{calls}");
    assert!(calls.contains("reviewId=PRR_pending"), "{calls}");
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
  "api user")
    printf '%s\n' 'different-reviewer[bot]'
    ;;
  "api graphql")
    case "$*" in
      *"deletePullRequestReview(input:"*)
        echo "delete must not run after an author mismatch" >&2
        exit 99
        ;;
      *)
        printf '%s\n' '{{"data":{{"repository":{{"pullRequest":{{"headRefOid":"head-new","reviews":{{"nodes":[{{"id":"PRR_pending","databaseId":102,"url":"https://github.com/acme/widgets/pull/42#pullrequestreview-102","author":{{"login":"example-review-bot[bot]"}},"state":"PENDING","commit":{{"oid":"head-new"}},"submittedAt":null,"body":"Pending review summary"}}],"pageInfo":{{"hasNextPage":false,"endCursor":null}}}}}}}}}}}}'
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
        ],
    );

    assert_eq!(out.code, 65, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["error"]["code"], "pending_review_author_mismatch");
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
    printf '%s\n' '{{"data":{{"repository":{{"pullRequest":{{"headRefOid":"head-new","reviews":{{"nodes":[{{"id":"PRR_submitted","databaseId":103,"url":"https://github.com/acme/widgets/pull/42#pullrequestreview-103","author":{{"login":"example-review-bot[bot]"}},"state":"COMMENTED","commit":{{"oid":"head-new"}},"submittedAt":"2026-07-16T04:00:00Z","body":"Submitted review summary"}}],"pageInfo":{{"hasNextPage":false,"endCursor":null}}}}}}}}}}}}'
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
        ],
    );

    assert_eq!(out.code, 0, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["data"]["review_id"], "PRR_pending");
    assert!(env["data"]["guard_plan"].as_array().is_some());
    assert!(env["data"]["snapshot_plan"].as_array().is_some());
    assert!(env["data"]["identity_plan"].as_array().is_some());
    assert!(
        env["data"]["delete_plan"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item == "reviewId=PRR_pending")
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
        ],
    );

    assert_eq!(out.code, 64, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["error"]["code"], "provider_unsupported");
}
