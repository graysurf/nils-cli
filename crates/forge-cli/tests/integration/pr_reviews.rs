//! Native pull-request review read-contract tests.

use pretty_assertions::assert_eq;

use super::support::{StubEnv, parse_envelope, run_forge_cli};

#[test]
fn pr_reviews_catalog_requires_commit_sha() {
    let catalog = include_str!("../../docs/specs/forge-cli-ops-v1.yaml");
    let reviews = catalog
        .split_once("  - id: pr.reviews\n")
        .expect("pr.reviews catalog entry")
        .1
        .split_once("  - id: pr.tasks\n")
        .expect("pr.tasks follows pr.reviews")
        .0;

    assert_eq!(reviews.matches("commit_sha").count(), 3);
    assert!(
        !reviews.contains("commit_sha?"),
        "successful review entries require commit_sha"
    );
}

#[test]
fn pr_reviews_classifies_current_head_and_stale_native_reviews() {
    let stub = StubEnv::new().gh_stub(
        r#"#!/bin/sh
set -eu
case "$1 $2" in
  "pr view")
    cat <<'JSON'
{"number":42,"url":"https://github.com/acme/widgets/pull/42","state":"OPEN","isDraft":false,"baseRefName":"main","headRefName":"feat/reviews","headRefOid":"head-new","title":"feat: reviews","body":""}
JSON
    ;;
  "api graphql")
    cat <<'JSON'
{"data":{"repository":{"pullRequest":{"headRefOid":"head-new","reviews":{"nodes":[{"id":"PRR_current","databaseId":101,"url":"https://github.com/acme/widgets/pull/42#pullrequestreview-101","author":{"login":"example-review-bot[bot]"},"state":"COMMENTED","commit":{"oid":"head-new"},"submittedAt":"2026-07-14T04:00:00Z","body":"Current-head summary"},{"id":"PRR_stale","databaseId":99,"url":"https://github.com/acme/widgets/pull/42#pullrequestreview-99","author":{"login":"example-review-bot[bot]"},"state":"CHANGES_REQUESTED","commit":{"oid":"head-old"},"submittedAt":"2026-07-14T03:00:00Z","body":"Old-head summary"}],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}}
JSON
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
            "reviews",
            "42",
        ],
    );
    assert_eq!(out.code, 0, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["schema_version"], "cli.forge-cli.pr.reviews.v1");
    assert_eq!(env["data"]["head_sha"], "head-new");
    assert_eq!(env["data"]["current_head_reviews"][0]["id"], "PRR_current");
    assert_eq!(env["data"]["current_head_reviews"][0]["database_id"], 101);
    assert_eq!(
        env["data"]["current_head_reviews"][0]["summary"],
        "Current-head summary"
    );
    assert_eq!(env["data"]["stale_reviews"][0]["id"], "PRR_stale");
    assert_eq!(env["data"]["stale_reviews"][0]["commit_sha"], "head-old");
}

#[test]
fn pr_reviews_paginates_until_the_native_review_snapshot_is_complete() {
    let stub = StubEnv::new().gh_stub(
        r#"#!/bin/sh
set -eu
case "$1 $2" in
  "pr view")
    printf '%s\n' '{"number":42,"url":"https://github.com/acme/widgets/pull/42","state":"OPEN","isDraft":false,"baseRefName":"main","headRefName":"feat/reviews","headRefOid":"head-new","title":"feat: reviews","body":""}'
    ;;
  "api graphql")
    case "$*" in
      *"after=cursor-1"*)
        printf '%s\n' '{"data":{"repository":{"pullRequest":{"headRefOid":"head-new","reviews":{"nodes":[{"id":"PRR_page_2","databaseId":101,"url":"https://github.com/acme/widgets/pull/42#pullrequestreview-101","author":{"login":"reviewer"},"state":"CHANGES_REQUESTED","commit":{"oid":"head-new"},"submittedAt":"2026-07-14T04:01:00Z","body":"page two"}],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}}'
        ;;
      *)
        printf '%s\n' '{"data":{"repository":{"pullRequest":{"headRefOid":"head-new","reviews":{"nodes":[{"id":"PRR_page_1","databaseId":1,"url":"https://github.com/acme/widgets/pull/42#pullrequestreview-1","author":{"login":"reviewer"},"state":"COMMENTED","commit":{"oid":"head-new"},"submittedAt":"2026-07-14T04:00:00Z","body":"page one"}],"pageInfo":{"hasNextPage":true,"endCursor":"cursor-1"}}}}}}'
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
            "reviews",
            "42",
        ],
    );
    assert_eq!(out.code, 0, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["data"]["current_head_reviews"][0]["id"], "PRR_page_1");
    assert_eq!(env["data"]["current_head_reviews"][1]["id"], "PRR_page_2");
}

#[test]
fn pr_reviews_rejects_a_review_without_commit_oid() {
    let stub = StubEnv::new().gh_stub(
        r#"#!/bin/sh
set -eu
case "$1 $2" in
  "pr view")
    printf '%s\n' '{"number":42,"url":"https://github.com/acme/widgets/pull/42","state":"OPEN","isDraft":false,"baseRefName":"main","headRefName":"feat/reviews","headRefOid":"head-new","title":"feat: reviews","body":""}'
    ;;
  "api graphql")
    printf '%s\n' '{"data":{"repository":{"pullRequest":{"headRefOid":"head-new","reviews":{"nodes":[{"id":"PRR_missing_commit","databaseId":8,"url":"https://github.com/acme/widgets/pull/42#pullrequestreview-8","author":{"login":"reviewer"},"state":"COMMENTED","commit":null,"submittedAt":"2026-07-14T04:00:00Z","body":"observed activity"}],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}}'
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
            "reviews",
            "42",
        ],
    );
    assert_eq!(out.code, 65, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["error"]["code"], "review_snapshot_incomplete");
    assert!(
        env["error"]["details"]["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("review.commit.oid"))
    );
}

#[test]
fn pr_reviews_fails_explicitly_for_gitlab() {
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
            "reviews",
            "42",
        ],
    );
    assert_eq!(out.code, 64, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["ok"], false);
    assert_eq!(env["error"]["code"], "provider_unsupported");
}

#[test]
fn pr_reviews_dry_run_renders_the_github_graphql_plan() {
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
            "reviews",
            "42",
        ],
    );
    assert_eq!(out.code, 0, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["schema_version"], "cli.forge-cli.pr.reviews.v1");
    let plan = env["data"]["plan"].as_array().expect("dry-run plan");
    assert!(plan.iter().any(|item| item == "graphql"));
    assert!(plan.iter().any(|item| item == "owner=acme"));
    assert!(plan.iter().any(|item| item == "name=widgets"));
    assert!(plan.iter().any(|item| item == "pr=42"));
}

#[test]
fn pr_close_does_not_accept_review_convergence() {
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
            "close",
            "42",
            "--review-convergence",
        ],
    );
    assert_eq!(out.code, 64, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["error"]["code"], "unknown-subcommand");
}
