//! Recovery tests for an authenticated actor's provider-valid pending review.

use std::fs;

use pretty_assertions::assert_eq;

use super::support::{CmdOutput, StubEnv, parse_envelope, run_forge_cli};

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
      *)
        printf '%s\n' '{{"data":{{"repository":{{"pullRequest":{{"headRefOid":"head-new","reviews":{{"nodes":[{{"id":"PRR_pending","databaseId":102,"url":"https://github.com/acme/widgets/pull/42#pullrequestreview-102","author":{{"login":"example-review-bot[bot]"}},"state":"PENDING","commit":null,"viewerDidAuthor":true,"viewerCanDelete":true,"body":"Pending review summary"}}],"pageInfo":{{"hasNextPage":false,"endCursor":null}}}}}}}}}}}}'
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
    assert!(!calls.contains("api user"), "{calls}");
    assert!(calls.contains("states: [PENDING]"), "{calls}");
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
      *"after=cursor-1"*)
        printf '%s\n' '{{"data":{{"repository":{{"pullRequest":{{"headRefOid":"head-new","reviews":{{"nodes":[{{"id":"PRR_page_2","databaseId":202,"url":"https://github.com/acme/widgets/pull/42#pullrequestreview-202","author":{{"login":"example-review-bot[bot]"}},"state":"PENDING","commit":null,"viewerDidAuthor":true,"viewerCanDelete":true,"body":"target"}}],"pageInfo":{{"hasNextPage":false,"endCursor":null}}}}}}}}}}}}'
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
      *)
        printf '%s\n' '{"data":{"repository":{"pullRequest":{"headRefOid":"head-new","reviews":{"nodes":[{"id":"PRR_pending","databaseId":102,"url":"https://github.com/acme/widgets/pull/42#pullrequestreview-102","author":{"login":"reviewer"},"state":"PENDING","commit":null,"viewerDidAuthor":true,"viewerCanDelete":true,"body":"Pending"}],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}}'
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
      *)
        printf '%s\n' '{"data":{"repository":{"pullRequest":{"headRefOid":"head-new","reviews":{"nodes":[{"id":"PRR_pending","url":"https://github.com/acme/widgets/pull/42#pullrequestreview-102","author":{"login":"reviewer"},"state":"PENDING","viewerDidAuthor":true,"viewerCanDelete":true}],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}}'
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
        ),
        (
            "missing-viewer-guard",
            r#"{"data":{"repository":{"pullRequest":{"headRefOid":"head-new","reviews":{"nodes":[{"id":"PRR_pending","url":"https://github.com/acme/widgets/pull/42#pullrequestreview-102","author":{"login":"reviewer"},"state":"PENDING","viewerDidAuthor":true}],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}}"#,
        ),
        (
            "non-pending-node",
            r#"{"data":{"repository":{"pullRequest":{"headRefOid":"head-new","reviews":{"nodes":[{"id":"PRR_pending","url":"https://github.com/acme/widgets/pull/42#pullrequestreview-102","author":{"login":"reviewer"},"state":"COMMENTED","viewerDidAuthor":true,"viewerCanDelete":true}],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}}"#,
        ),
    ];

    for (name, snapshot) in cases {
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
        assert_eq!(env["error"]["code"], "review_snapshot_incomplete");
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
        ],
    );

    assert_eq!(out.code, 64, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["error"]["code"], "provider_unsupported");
}
