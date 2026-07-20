//! End-to-end `pr review` integration tests. The command is intentionally a
//! provider posting primitive: it posts an already-rendered review outcome and
//! optionally mirrors a compact activity note to an issue.

use std::{fs, path::Path};

use pretty_assertions::assert_eq;

use super::support::{
    CmdOutput, StubEnv, parse_envelope, run_forge_cli, run_forge_cli_in, run_forge_cli_with_stdin,
};

fn assert_backend_not_invoked(capture: &Path) {
    let calls = fs::read_to_string(capture).unwrap_or_default();
    assert!(
        calls.trim().is_empty(),
        "backend should not be invoked before validation failure: {calls}"
    );
}

fn github_review_stub(capture: &str) -> String {
    format!(
        r#"#!/bin/sh
set -eu
printf '%s\n' "$*" >> {capture:?}
if [ "$1" != "api" ]; then
  echo "stub: unexpected gh command: $*" >&2
  exit 99
fi
case "$2" in
  repos/acme/widgets/pulls/44)
    echo "44"
    ;;
  repos/acme/widgets/issues/44/comments)
    echo "https://github.com/acme/widgets/pull/44#issuecomment-440"
    ;;
  repos/acme/widgets/issues/101/comments)
    echo "https://github.com/acme/widgets/issues/101#issuecomment-1010"
    ;;
  *)
    echo "stub: unexpected gh api endpoint: $2" >&2
    exit 99
    ;;
esac
"#
    )
}

// Modern glab: `mr note create` advertises `--resolvable`, so the live path
// probes it and posts the non-resolvable form.
fn gitlab_review_stub(capture: &str) -> String {
    format!(
        r#"#!/bin/sh
set -eu
printf '%s\n' "$*" >> {capture:?}
case "$*" in
  *"mr note create"*"--help"*)
    echo "  -m --message    Comment or note message."
    echo "  --resolvable    Create the note as a resolvable discussion thread. Set to false to create a non-resolvable note. (true)"
    ;;
  "mr note create "*)
    echo "https://gitlab.com/acme/widgets/-/merge_requests/44#note_440"
    ;;
  "issue note "*)
    echo "https://gitlab.com/acme/widgets/-/issues/101#note_1010"
    ;;
  *)
    echo "stub: unexpected glab command: $*" >&2
    exit 99
    ;;
esac
"#
    )
}

// Mid glab: `mr note create` exists (its `--help` usage names it) but does NOT
// advertise `--resolvable`, so the live path posts via `mr note create <id>
// --message` (dropping only `--resolvable=false`).
fn gitlab_create_no_resolvable_review_stub(capture: &str) -> String {
    format!(
        r#"#!/bin/sh
set -eu
printf '%s\n' "$*" >> {capture:?}
case "$*" in
  *"mr note create"*"--help"*)
    echo "  USAGE    glab mr note create [<id> | <branch>] [--flags]"
    echo "  -m --message    Comment or note message."
    echo "  --unique        Don't create a note if a note with the same body already exists."
    ;;
  "mr note create "*)
    echo "https://gitlab.com/acme/widgets/-/merge_requests/44#note_440"
    ;;
  "issue note "*)
    echo "https://gitlab.com/acme/widgets/-/issues/101#note_1010"
    ;;
  *)
    echo "stub: unexpected glab command: $*" >&2
    exit 99
    ;;
esac
"#
    )
}

// Old glab: `mr note` has NO `create` subcommand, so `mr note create --help`
// falls through to the parent `mr note` help (no `mr note create` usage line,
// no `--resolvable`). The live path must post via the bare `mr note <id>` form.
fn gitlab_no_create_review_stub(capture: &str) -> String {
    format!(
        r#"#!/bin/sh
set -eu
printf '%s\n' "$*" >> {capture:?}
case "$*" in
  *"mr note create"*"--help"*)
    echo "  Creates a comment by default. Use --resolve or --unresolve."
    echo "  USAGE    glab mr note [<id> | <branch>] [--flags]"
    echo "  -m --message    Comment or note message."
    ;;
  "mr note "*)
    echo "https://gitlab.com/acme/widgets/-/merge_requests/44#note_440"
    ;;
  "issue note "*)
    echo "https://gitlab.com/acme/widgets/-/issues/101#note_1010"
    ;;
  *)
    echo "stub: unexpected glab command: $*" >&2
    exit 99
    ;;
esac
"#
    )
}

#[test]
fn pr_review_posts_outcome_and_mirrors_issue_activity() {
    let stub = StubEnv::new();
    let review_file = stub.tempdir.path().join("review.md");
    let capture = stub.tempdir.path().join("gh-args.log");
    fs::write(
        &review_file,
        "## Testing Review\n\nStatus: pass\nFindings: none\n",
    )
    .expect("write review body");
    let stub = stub.gh_stub(&github_review_stub(&capture.to_string_lossy()));

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
            "review",
            "44",
            "--decision",
            "comments-only",
            "--comment-file",
            review_file.to_str().expect("utf8 path"),
            "--lens",
            "testing",
            "--lens",
            "red-team",
            "--issue",
            "101",
            "--mirror-issue",
        ],
    );

    assert_eq!(out.code, 0, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["schema_version"], "cli.forge-cli.pr.review.v1");
    assert_eq!(env["ok"], true);
    assert_eq!(env["data"]["provider"], "github");
    assert_eq!(env["data"]["number"], 44);
    assert_eq!(env["data"]["decision"], "comments-only");
    assert_eq!(
        env["data"]["pr_comment_url"],
        "https://github.com/acme/widgets/pull/44#issuecomment-440"
    );
    assert_eq!(env["data"]["issue_number"], 101);
    assert_eq!(
        env["data"]["issue_comment_url"],
        "https://github.com/acme/widgets/issues/101#issuecomment-1010"
    );
    assert_eq!(env["data"]["mirrored"], true);
    assert_eq!(env["data"]["lenses"][0], "testing");
    assert_eq!(env["data"]["lenses"][1], "red-team");

    let calls = fs::read_to_string(capture).expect("read captured calls");
    // The PR-existence guard verifies `<id>` is a pull request before posting.
    assert!(
        calls.contains("repos/acme/widgets/pulls/44"),
        "PR-existence lookup missing: {calls}"
    );
    assert!(
        calls.contains("repos/acme/widgets/issues/44/comments"),
        "PR comment call missing: {calls}"
    );
    assert!(
        calls.contains("repos/acme/widgets/issues/101/comments"),
        "issue mirror call missing: {calls}"
    );
    // GitHub PRs are referenced with `#<number>` in the mirror body.
    assert!(
        calls.contains("pr: #44"),
        "mirror body should reference the PR as #44: {calls}"
    );
    assert!(
        calls.contains("review_url=https://github.com/acme/widgets/pull/44#issuecomment-440"),
        "mirror body should link the PR review comment: {calls}"
    );
}

#[test]
fn pr_review_posts_gitlab_outcome_and_issue_mirror() {
    let stub = StubEnv::new();
    let capture = stub.tempdir.path().join("glab-args.log");
    let stub = stub.glab_stub(&gitlab_review_stub(&capture.to_string_lossy()));

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
            "review",
            "44",
            "--decision",
            "request-changes",
            "--comment",
            "Needs another pass.",
            "--issue",
            "101",
            "--mirror-issue",
        ],
    );

    assert_eq!(out.code, 0, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["schema_version"], "cli.forge-cli.pr.review.v1");
    assert_eq!(env["data"]["provider"], "gitlab");
    assert_eq!(env["data"]["decision"], "request-changes");
    assert_eq!(
        env["data"]["pr_comment_url"],
        "https://gitlab.com/acme/widgets/-/merge_requests/44#note_440"
    );
    assert_eq!(
        env["data"]["issue_comment_url"],
        "https://gitlab.com/acme/widgets/-/issues/101#note_1010"
    );

    let calls = fs::read_to_string(capture).expect("read captured calls");
    // The review outcome note must be created non-resolvable so a comments-only
    // or approve review does not leave an unresolved MR discussion that blocks
    // the next `forge-cli pr merge`.
    assert!(calls.contains("mr note create 44"), "{calls}");
    assert!(calls.contains("--resolvable=false"), "{calls}");
    assert!(calls.contains("issue note 101"), "{calls}");
    assert!(calls.contains("--repo acme/widgets"), "{calls}");
    // GitLab merge requests are referenced with `!<iid>`, not `#<iid>`.
    assert!(
        calls.contains("pr: !44"),
        "mirror body should reference the MR as !44: {calls}"
    );
}

#[test]
fn pr_review_rejects_non_pull_request_id_before_posting() {
    // When `<id>` is not a pull request (the GitHub issues-comments API would
    // otherwise silently post the outcome onto an unrelated issue), the guard
    // must reject before posting anything.
    let stub = StubEnv::new();
    let capture = stub.tempdir.path().join("gh-args.log");
    let body = format!(
        r#"#!/bin/sh
set -eu
printf '%s\n' "$*" >> {capture:?}
case "$2" in
  repos/acme/widgets/pulls/44)
    echo "gh: Not Found (HTTP 404)" >&2
    exit 1
    ;;
  *)
    echo "stub: should not post on a non-PR id: $*" >&2
    exit 99
    ;;
esac
"#,
        capture = capture
    );
    let stub = stub.gh_stub(&body);

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
            "review",
            "44",
            "--decision",
            "comments-only",
            "--comment",
            "Status: pass",
        ],
    );

    assert_eq!(out.code, 65, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["schema_version"], "cli.forge-cli.error.v1");
    assert_eq!(env["ok"], false);
    assert_eq!(env["error"]["code"], "id_not_pull_request");

    let calls = fs::read_to_string(capture).expect("read captured calls");
    assert!(
        calls.contains("repos/acme/widgets/pulls/44"),
        "PR-existence lookup should run: {calls}"
    );
    assert!(
        !calls.contains("issues/44/comments"),
        "no review outcome should be posted when the id is not a PR: {calls}"
    );
}

#[test]
fn pr_review_rejects_lens_local_path_in_mirror_before_backend_call() {
    // A `--lens` value carrying a machine-local path is embedded into the
    // generated issue mirror body, so it must hit the same `no_local_path`
    // guard the review body does — and before any backend mutation.
    let stub = StubEnv::new().gh_stub("#!/bin/sh\necho should-not-run >&2\nexit 99\n");

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
            "review",
            "44",
            "--decision",
            "comments-only",
            "--comment",
            "Status: pass",
            "--lens",
            "/Users/terry/project/secret.txt",
            "--issue",
            "101",
            "--mirror-issue",
        ],
    );

    assert_eq!(out.code, 65, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["schema_version"], "cli.forge-cli.error.v1");
    assert_eq!(env["ok"], false);
    assert_eq!(env["error"]["code"], "local_path_present");
}

#[test]
fn pr_review_rejects_lens_escaped_control_in_mirror_before_backend_call() {
    // A `--lens` value carrying a literal escaped control is embedded into the
    // generated issue mirror body. The escaped-control guard must run before
    // the PR comment is posted, so a bad lens never leaves a posted outcome
    // with no mirror.
    let stub = StubEnv::new().gh_stub("#!/bin/sh\necho should-not-run >&2\nexit 99\n");

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
            "review",
            "44",
            "--decision",
            "comments-only",
            "--comment",
            "Status: pass",
            "--lens",
            "foo\\nbar",
            "--issue",
            "101",
            "--mirror-issue",
        ],
    );

    assert_eq!(out.code, 65, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["schema_version"], "cli.forge-cli.error.v1");
    assert_eq!(env["ok"], false);
    assert_eq!(env["error"]["code"], "markdown_escaped_control");
}

#[test]
fn pr_review_rejects_local_paths_before_backend_call() {
    let stub = StubEnv::new().gh_stub("#!/bin/sh\necho should-not-run >&2\nexit 99\n");

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
            "review",
            "44",
            "--decision",
            "comments-only",
            "--comment",
            "local path: /Users/terry/secret.txt",
        ],
    );

    assert_eq!(out.code, 65, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["schema_version"], "cli.forge-cli.error.v1");
    assert_eq!(env["ok"], false);
    assert_eq!(env["error"]["code"], "local_path_present");
}

#[test]
fn pr_review_missing_comment_file_reports_matching_flag() {
    let stub = StubEnv::new().gh_stub("#!/bin/sh\necho should-not-run >&2\nexit 99\n");

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
            "review",
            "44",
            "--comment-file",
            "/this/path/does/not/exist",
        ],
    );

    assert_eq!(out.code, 70, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["schema_version"], "cli.forge-cli.error.v1");
    assert_eq!(env["ok"], false);
    assert_eq!(env["error"]["code"], "software_error");
    assert!(
        env["error"]["message"]
            .as_str()
            .expect("error message")
            .contains("--comment-file"),
        "{env:#}"
    );
}

#[test]
fn pr_review_propagates_non_404_guard_failure() {
    // Only a genuine 404 means `<id>` is not a pull request. A transient backend
    // failure (rate limit, 5xx, forbidden/SSO) on the guard call could hit a
    // perfectly valid PR, so it must surface as a retryable `backend_error`, not
    // a permanent `id_not_pull_request` validation failure — and must not post.
    let stub = StubEnv::new();
    let capture = stub.tempdir.path().join("gh-args.log");
    let body = format!(
        r#"#!/bin/sh
set -eu
printf '%s\n' "$*" >> {capture:?}
case "$2" in
  repos/acme/widgets/pulls/44)
    echo "gh: HTTP 502 Bad Gateway" >&2
    exit 1
    ;;
  *)
    echo "stub: should not post when the guard fails: $*" >&2
    exit 99
    ;;
esac
"#,
        capture = capture
    );
    let stub = stub.gh_stub(&body);

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
            "review",
            "44",
            "--decision",
            "comments-only",
            "--comment",
            "Status: pass",
        ],
    );

    assert_eq!(out.code, 1, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["schema_version"], "cli.forge-cli.error.v1");
    assert_eq!(env["ok"], false);
    assert_eq!(env["error"]["code"], "backend_error");

    let calls = fs::read_to_string(capture).expect("read captured calls");
    assert!(
        !calls.contains("issues/44/comments"),
        "no review outcome should be posted when the guard fails: {calls}"
    );
}

#[test]
fn pr_review_dry_run_includes_pull_request_guard() {
    // dry-run must render every backend command the live run performs, including
    // the GitHub PR-existence guard read.
    let stub = StubEnv::new().gh_stub("#!/bin/sh\necho should-not-run >&2\nexit 99\n");

    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "--repo",
            "acme/widgets",
            "--format",
            "json",
            "--dry-run",
            "pr",
            "review",
            "44",
            "--decision",
            "comments-only",
            "--comment",
            "Status: pass",
        ],
    );

    assert_eq!(out.code, 0, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["schema_version"], "cli.forge-cli.pr.review.v1");
    assert_eq!(env["ok"], true);
    let guard_plan = env["data"]["guard_plan"]
        .as_array()
        .expect("guard_plan present in github dry-run");
    let joined = guard_plan
        .iter()
        .map(|v| v.as_str().unwrap_or_default())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        joined.contains("repos/acme/widgets/pulls/44"),
        "guard_plan should render the PR-existence lookup: {joined}"
    );
}

#[test]
fn pr_review_mirror_issue_without_issue_reports_issue_required() {
    // `--mirror-issue` without `--issue` reaches the runtime guard (clap no
    // longer rejects it at parse time) and returns the documented DATA 65
    // `issue_required` envelope before any backend mutation.
    let stub = StubEnv::new().gh_stub("#!/bin/sh\necho should-not-run >&2\nexit 99\n");

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
            "review",
            "44",
            "--decision",
            "comments-only",
            "--comment",
            "Status: pass",
            "--mirror-issue",
        ],
    );

    assert_eq!(out.code, 65, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["schema_version"], "cli.forge-cli.error.v1");
    assert_eq!(env["ok"], false);
    assert_eq!(env["error"]["code"], "issue_required");
}

#[test]
fn pr_review_gitlab_uses_create_without_flag_when_resolvable_unsupported() {
    // glab where `mr note create` exists but lacks `--resolvable`: post via
    // `mr note create <id> --message`, dropping only `--resolvable=false`.
    let stub = StubEnv::new();
    let capture = stub.tempdir.path().join("glab-args.log");
    let stub = stub.glab_stub(&gitlab_create_no_resolvable_review_stub(
        &capture.to_string_lossy(),
    ));

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
            "review",
            "44",
            "--decision",
            "comments-only",
            "--comment",
            "Status: pass",
        ],
    );

    assert_eq!(out.code, 0, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["data"]["provider"], "gitlab");

    let calls = fs::read_to_string(capture).expect("read captured calls");
    assert!(
        calls.contains("mr note create --help"),
        "should probe glab note form: {calls}"
    );
    assert!(
        calls.contains("mr note create 44"),
        "should post via the `mr note create <id>` form: {calls}"
    );
    assert!(
        !calls.contains("--resolvable"),
        "must not pass --resolvable on a glab build that lacks it: {calls}"
    );
}

#[test]
fn pr_review_gitlab_uses_bare_form_when_no_create_subcommand() {
    // Old glab without a `mr note create` subcommand: the probe sees no
    // `mr note create` usage, so the command posts via the bare `mr note <id>`
    // form rather than invoking a `create` subcommand that does not exist.
    let stub = StubEnv::new();
    let capture = stub.tempdir.path().join("glab-args.log");
    let stub = stub.glab_stub(&gitlab_no_create_review_stub(&capture.to_string_lossy()));

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
            "review",
            "44",
            "--decision",
            "comments-only",
            "--comment",
            "Status: pass",
        ],
    );

    assert_eq!(out.code, 0, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["data"]["provider"], "gitlab");

    let calls = fs::read_to_string(capture).expect("read captured calls");
    assert!(
        calls.contains("mr note create --help"),
        "should probe glab note form: {calls}"
    );
    assert!(
        calls.contains("mr note 44"),
        "should post via the bare `mr note <id>` form: {calls}"
    );
    assert!(
        !calls.contains("mr note create 44"),
        "must not invoke a `create` subcommand that does not exist: {calls}"
    );
}

#[test]
fn pr_review_mirror_issue_without_issue_checks_before_reading_body() {
    // `issue_required` must fire before the comment body is read, so a missing
    // `--issue` fails fast with DATA 65 instead of blocking on stdin
    // (`--comment-file -`) or surfacing a file-read `software_error` first.
    let stub = StubEnv::new().gh_stub("#!/bin/sh\necho should-not-run >&2\nexit 99\n");

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
            "review",
            "44",
            "--mirror-issue",
            "--comment-file",
            "/this/path/does/not/exist",
        ],
    );

    assert_eq!(out.code, 65, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["schema_version"], "cli.forge-cli.error.v1");
    assert_eq!(env["ok"], false);
    assert_eq!(env["error"]["code"], "issue_required");
}

// `--submit-review`: create a native GitHub review object via
// POST .../pulls/<id>/reviews (the #pullrequestreview- artifact) instead of an
// issue comment. The guard call returns the PR number; the reviews endpoint
// returns the review html_url.
fn github_review_submit_stub(capture: &str) -> String {
    format!(
        r#"#!/bin/sh
set -eu
printf '%s\n' "$*" >> {capture:?}
if [ "$1" != "api" ]; then
  echo "stub: unexpected gh command: $*" >&2
  exit 99
fi
case "$2" in
  repos/acme/widgets/pulls/44)
    echo "44"
    ;;
  graphql)
    case "$*" in
      *"states: [PENDING]"*)
        printf '%s\n' '{{"data":{{"repository":{{"pullRequest":{{"headRefOid":"head-44","reviews":{{"nodes":[{{"id":"PRR_other_pending","url":"https://github.com/acme/widgets/pull/44#pullrequestreview-9901","author":null,"state":"PENDING","commit":null,"body":"","viewerDidAuthor":false,"viewerCanDelete":false}}],"pageInfo":{{"hasNextPage":false,"endCursor":null}}}}}}}}}}}}'
        ;;
      *)
        echo "stub: unexpected graphql payload: $*" >&2
        exit 99
        ;;
    esac
    ;;
  repos/acme/widgets/pulls/44/reviews)
    echo "https://github.com/acme/widgets/pull/44#pullrequestreview-9900"
    ;;
  repos/acme/widgets/issues/101/comments)
    echo "https://github.com/acme/widgets/issues/101#issuecomment-1010"
    ;;
  *)
    echo "stub: unexpected gh api endpoint: $2" >&2
    exit 99
    ;;
esac
"#
    )
}

fn github_review_submit_approval_422_stub(capture: &str) -> String {
    format!(
        r#"#!/bin/sh
set -eu
printf '%s\n' "$*" >> {capture:?}
if [ "$1" != "api" ]; then
  echo "stub: unexpected gh command: $*" >&2
  exit 99
fi
case "$2" in
  repos/acme/widgets/pulls/44)
    echo "44"
    ;;
  graphql)
    case "$*" in
      *"reviewThreads(first: 100"*)
        printf '%s\n' '{{"data":{{"repository":{{"pullRequest":{{"headRefOid":"head-44","reviewThreads":{{"nodes":[],"pageInfo":{{"hasNextPage":false,"endCursor":null}}}}}}}}}}}}'
        ;;
      *"states: [PENDING]"*)
        printf '%s\n' '{{"data":{{"repository":{{"pullRequest":{{"headRefOid":"head-44","reviews":{{"nodes":[],"pageInfo":{{"hasNextPage":false,"endCursor":null}}}}}}}}}}}}'
        ;;
      *)
        echo "stub: unexpected graphql payload: $*" >&2
        exit 99
        ;;
    esac
    ;;
  repos/acme/widgets/pulls/44/reviews)
    cat >&2 <<'ERR'
gh: Unprocessable Entity (HTTP 422)
{{"message":"Validation Failed","errors":[{{"resource":"PullRequestReview","code":"custom","message":"Only users with explicit access can approve pull requests"}}]}}
ERR
    exit 1
    ;;
  *)
    echo "stub: unexpected gh api endpoint: $2" >&2
    exit 99
    ;;
esac
"#
    )
}

fn github_review_submit_pending_conflict_stub(capture: &str) -> String {
    format!(
        r#"#!/bin/sh
set -eu
printf '%s\n' "$*" >> {capture:?}
if [ "$1" != "api" ]; then
  echo "stub: unexpected gh command: $*" >&2
  exit 99
fi
case "$2" in
  repos/acme/widgets/pulls/44)
    echo "44"
    ;;
  graphql)
    case "$*" in
      *"states: [PENDING]"*)
        printf '%s\n' '{{"data":{{"repository":{{"pullRequest":{{"headRefOid":"head-44","reviews":{{"nodes":[{{"id":"PRR_pending","url":"https://github.com/acme/widgets/pull/44#pullrequestreview-9911","author":{{"login":"review-bot"}},"state":"PENDING","commit":{{"oid":"head-44"}},"body":"Pending review","viewerDidAuthor":true,"viewerCanDelete":false}}],"pageInfo":{{"hasNextPage":false,"endCursor":null}}}}}}}}}}}}'
        ;;
      *)
        echo "stub: unexpected graphql payload: $*" >&2
        exit 99
        ;;
    esac
    ;;
  repos/acme/widgets/pulls/44/reviews)
    echo "native review mutation must not run while a viewer-owned pending review exists" >&2
    exit 99
    ;;
  *)
    echo "stub: unexpected gh api endpoint: $2" >&2
    exit 99
    ;;
esac
"#
    )
}

fn github_resumable_thread_stub(
    capture: &str,
    existing_thread: bool,
    existing_resolved: bool,
    existing_outdated: bool,
    failure: &str,
    unmarked_pending: bool,
) -> String {
    r#"#!/bin/sh
set -eu
capture="@CAPTURE@"
receipt="$capture.receipt"
pending_body="$capture.pending-body"
finding_body_0="$capture.finding-body-0"
finding_path_0="$capture.finding-path-0"
finding_line_0="$capture.finding-line-0"
finding_body_1="$capture.finding-body-1"
finding_path_1="$capture.finding-path-1"
finding_line_1="$capture.finding-line-1"
submitted="$capture.submitted"
failed_once="$capture.failed-once"
failure="@FAILURE@"

extract_field() {
  name=$1
  shift
  for arg in "$@"; do
    case "$arg" in
      "$name="*) printf '%s' "${arg#*=}"; return 0 ;;
    esac
  done
  return 1
}

json_escape() {
  awk 'BEGIN { ORS=""; first=1 } {
    if (!first) printf "\\n";
    printf "%s", $0;
    first=0
  }' "$1"
}

if [ "@UNMARKED_PENDING@" = "true" ] && [ ! -f "$pending_body" ]; then
  printf '%s' 'Pending review' > "$pending_body"
fi

printf '%s\n' "$*" >> "$capture"
if [ "$1" != "api" ]; then
  echo "stub: unexpected gh command: $*" >&2
  exit 99
fi

case "$*" in
  *"repos/acme/widgets/pulls/44 --jq .number"*)
    echo "44"
    ;;
  *"pullRequest(number: \$pr) { comments(first: 100"*)
    if [ -f "$receipt" ]; then
      body=$(json_escape "$receipt")
      printf '{"data":{"viewer":{"login":"review-bot"},"repository":{"pullRequest":{"comments":{"nodes":[{"author":{"login":"review-bot"},"body":"%s"}],"pageInfo":{"hasNextPage":false,"endCursor":"state-tip"}}}}}}\n' "$body"
    else
      printf '%s\n' '{"data":{"viewer":{"login":"review-bot"},"repository":{"pullRequest":{"comments":{"nodes":[],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}}'
    fi
    ;;
  *"repos/acme/widgets/issues/44/comments --method POST"*)
    extract_field body "$@" > "$receipt"
    printf '%s\n' 'https://github.com/acme/widgets/pull/44#issuecomment-receipt'
    ;;
  *"reviewThreads(first: 100"*)
    if [ -f "$submitted" ] && [ -f "$finding_body_0" ]; then
      body=$(json_escape "$finding_body_0")
      path=$(json_escape "$finding_path_0")
      line=$(sed -n '1p' "$finding_line_0")
      [ -n "$line" ] || line=null
      printf '{"data":{"repository":{"pullRequest":{"headRefOid":"head-44","reviewThreads":{"nodes":[{"id":"PRRT_created","isResolved":false,"isOutdated":false,"path":"%s","diffSide":"RIGHT","line":%s,"originalLine":%s,"originalStartLine":null,"startDiffSide":null,"startLine":null,"subjectType":"LINE","comments":{"nodes":[{"id":"PRRC_created","author":{"login":"review-bot"},"body":"%s","createdAt":"2026-07-20T12:00:00Z","url":"https://github.com/acme/widgets/pull/44#discussion_r42"}],"pageInfo":{"hasNextPage":false,"endCursor":null}}}],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}}\n' "$path" "$line" "$line" "$body"
    elif [ "@EXISTING_THREAD@" = "true" ]; then
      printf '%s\n' '{"data":{"repository":{"pullRequest":{"headRefOid":"head-44","reviewThreads":{"nodes":[{"id":"PRRT_existing","isResolved":@EXISTING_RESOLVED@,"isOutdated":@EXISTING_OUTDATED@,"path":"src/lib.rs","diffSide":"RIGHT","line":42,"originalLine":42,"originalStartLine":null,"startDiffSide":null,"startLine":null,"subjectType":"LINE","comments":{"nodes":[{"id":"PRRC_existing","author":{"login":"quality-bot"},"body":"Duplicate finding body.","createdAt":"2026-07-14T04:00:00Z","url":"https://github.com/acme/widgets/pull/44#discussion_r1"}],"pageInfo":{"hasNextPage":false,"endCursor":null}}}],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}}'
    else
      printf '%s\n' '{"data":{"repository":{"pullRequest":{"headRefOid":"head-44","reviewThreads":{"nodes":[],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}}'
    fi
    ;;
  *"states: [PENDING]"*)
    if [ -f "$pending_body" ] && [ ! -f "$submitted" ]; then
      body=$(json_escape "$pending_body")
      printf '{"data":{"repository":{"pullRequest":{"headRefOid":"head-44","reviews":{"nodes":[{"id":"PRR_kwDOpending","url":"https://github.com/acme/widgets/pull/44#pullrequestreview-9900","author":{"login":"review-bot"},"state":"PENDING","commit":{"oid":"head-44"},"body":"%s","viewerDidAuthor":true,"viewerCanDelete":true}],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}}\n' "$body"
    else
      printf '%s\n' '{"data":{"repository":{"pullRequest":{"headRefOid":"head-44","reviews":{"nodes":[],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}}'
    fi
    ;;
  *"reviews(first: 100"*)
    if [ -f "$submitted" ]; then
      body=$(json_escape "$pending_body")
      printf '{"data":{"viewer":{"login":"review-bot"},"repository":{"pullRequest":{"headRefOid":"head-44","reviews":{"nodes":[{"id":"PRR_kwDOpending","databaseId":9900,"url":"https://github.com/acme/widgets/pull/44#pullrequestreview-9900","author":{"login":"review-bot"},"state":"COMMENTED","commit":{"oid":"head-44"},"submittedAt":"2026-07-20T12:00:00Z","body":"%s","viewerDidAuthor":true}],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}}\n' "$body"
    else
      printf '%s\n' '{"data":{"repository":{"pullRequest":{"headRefOid":"head-44","reviews":{"nodes":[],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}}'
    fi
    ;;
  *"query(\$review: ID!"*)
    if [ ! -f "$pending_body" ] || [ -f "$submitted" ]; then
      printf '%s\n' '{"data":{"node":null}}'
      exit 0
    fi
    body=$(json_escape "$pending_body")
    comments=''
    total=0
    if [ -f "$finding_body_0" ]; then
      comment=$(json_escape "$finding_body_0")
      path=$(json_escape "$finding_path_0")
      line=$(sed -n '1p' "$finding_line_0")
      [ -n "$line" ] || line=null
      comments=$(printf '{"id":"PRRC_created_0","url":"https://github.com/acme/widgets/pull/44#discussion_r42","author":{"login":"review-bot"},"body":"%s","createdAt":"2026-07-20T12:00:00Z","path":"%s","line":%s,"originalLine":%s,"diffSide":"RIGHT","startLine":null,"originalStartLine":null,"startDiffSide":null,"subjectType":"LINE"}' "$comment" "$path" "$line" "$line")
      total=1
    fi
    if [ -f "$finding_body_1" ]; then
      comment=$(json_escape "$finding_body_1")
      path=$(json_escape "$finding_path_1")
      line=$(sed -n '1p' "$finding_line_1")
      [ -n "$line" ] || line=null
      next=$(printf '{"id":"PRRC_created_1","url":"https://github.com/acme/widgets/pull/44#discussion_r43","author":{"login":"review-bot"},"body":"%s","createdAt":"2026-07-20T12:00:01Z","path":"%s","line":%s,"originalLine":%s,"diffSide":"RIGHT","startLine":null,"originalStartLine":null,"startDiffSide":null,"subjectType":"LINE"}' "$comment" "$path" "$line" "$line")
      if [ -n "$comments" ]; then
        comments="$comments,$next"
      else
        comments="$next"
      fi
      total=2
    fi
    printf '{"data":{"node":{"id":"PRR_kwDOpending","url":"https://github.com/acme/widgets/pull/44#pullrequestreview-9900","author":{"login":"review-bot"},"state":"PENDING","commit":{"oid":"head-44"},"body":"%s","viewerDidAuthor":true,"viewerCanDelete":true,"comments":{"totalCount":%s,"nodes":[%s],"pageInfo":{"hasNextPage":false,"endCursor":null}},"pullRequest":{"number":44,"url":"https://github.com/acme/widgets/pull/44","headRefOid":"head-44"}}}}\n' "$body" "$total" "$comments"
    ;;
  *"query(\$owner: String!, \$name: String!, \$number: Int!"*)
    printf '%s\n' '{"data":{"repository":{"pullRequest":{"id":"PR_kwDOabc","url":"https://github.com/acme/widgets/pull/44"}}}}'
    ;;
  *"addPullRequestReview(input:"*)
    extract_field body "$@" > "$pending_body"
    if [ "$failure" = "pending-create-once" ] && [ ! -f "$failed_once" ]; then
      : > "$failed_once"
      echo "connection lost after pending review creation" >&2
      exit 42
    fi
    printf '%s\n' '{"data":{"addPullRequestReview":{"pullRequestReview":{"id":"PRR_kwDOpending","url":"https://github.com/acme/widgets/pull/44#pullrequestreview-9900"}}}}'
    ;;
  *"addPullRequestReviewThread(input:"*)
    if [ ! -f "$finding_body_0" ]; then
      finding_body="$finding_body_0"
      finding_path="$finding_path_0"
      finding_line="$finding_line_0"
      finding_index=0
    else
      finding_body="$finding_body_1"
      finding_path="$finding_path_1"
      finding_line="$finding_line_1"
      finding_index=1
    fi
    extract_field body "$@" > "$finding_body"
    extract_field path "$@" > "$finding_path"
    extract_field line "$@" > "$finding_line" || printf '' > "$finding_line"
    if [ "$failure" = "lost-thread-$finding_index-once" ] && [ ! -f "$failed_once" ]; then
      : > "$failed_once"
      echo "connection lost after inline comment index $finding_index" >&2
      exit 42
    fi
    if [ "$failure" = "lost-thread-once" ] && [ ! -f "$failed_once" ]; then
      : > "$failed_once"
      echo "connection lost after inline comment creation" >&2
      exit 42
    fi
    if [ "$failure" = "thread-error" ]; then
      echo "could not map review thread to diff" >&2
      exit 42
    fi
    if [ "$failure" = "thread-422" ]; then
      echo "gh: line must be part of the diff (HTTP 422)" >&2
      exit 1
    fi
    path=$(sed -n '1p' "$finding_path")
    line=$(sed -n '1p' "$finding_line")
    [ -n "$line" ] || line=null
    printf '{"data":{"addPullRequestReviewThread":{"thread":{"id":"PRRT_kwDOthread","path":"%s","line":%s,"subjectType":"LINE","comments":{"nodes":[{"url":"https://github.com/acme/widgets/pull/44#discussion_r42"}]}}}}}\n' "$path" "$line"
    ;;
  *"submitPullRequestReview(input:"*)
    if [ "$failure" = "before-submit-once" ] && [ ! -f "$failed_once" ]; then
      : > "$failed_once"
      echo "connection lost before submit confirmation" >&2
      exit 42
    fi
    if [ "$failure" = "submit-error" ]; then
      echo "review submit failed" >&2
      exit 42
    fi
    if [ "$failure" = "submit-422" ]; then
      echo "gh: Only users with explicit access can approve pull requests (HTTP 422)" >&2
      exit 1
    fi
    : > "$submitted"
    if [ "$failure" = "lost-submit" ]; then
      echo "connection lost after submit" >&2
      exit 42
    fi
    printf '%s\n' '{"data":{"submitPullRequestReview":{"pullRequestReview":{"url":"https://github.com/acme/widgets/pull/44#pullrequestreview-9900"}}}}'
    ;;
  *)
    echo "stub: unexpected gh api endpoint: $*" >&2
    exit 99
    ;;
esac
"#
    .replace("@CAPTURE@", capture)
    .replace("@FAILURE@", failure)
    .replace("@EXISTING_THREAD@", if existing_thread { "true" } else { "false" })
    .replace(
        "@EXISTING_RESOLVED@",
        if existing_resolved { "true" } else { "false" },
    )
    .replace(
        "@EXISTING_OUTDATED@",
        if existing_outdated { "true" } else { "false" },
    )
    .replace("@UNMARKED_PENDING@", if unmarked_pending { "true" } else { "false" })
}

fn github_review_thread_submit_stub(capture: &str) -> String {
    github_resumable_thread_stub(capture, false, false, false, "none", false)
}

/// Like [`github_review_thread_submit_stub`] but the `reviewThreads` read
/// returns one live (non-resolved, non-outdated) thread whose body matches the
/// spec body, so cross-run idempotency must skip re-creating it. The
/// `reviewThreads(first: 100)` case is matched before `repository(owner:`
/// because the threads query contains both substrings.
fn github_review_thread_idempotent_stub(
    capture: &str,
    existing_resolved: bool,
    existing_outdated: bool,
) -> String {
    github_resumable_thread_stub(
        capture,
        true,
        existing_resolved,
        existing_outdated,
        "none",
        false,
    )
}

fn github_review_thread_fail_after_pending_stub(capture: &str) -> String {
    github_resumable_thread_stub(capture, false, false, false, "thread-error", false)
}

fn github_review_thread_diff_mapping_stub(capture: &str) -> String {
    format!(
        r#"#!/bin/sh
set -eu
printf '%s\n' "$*" >> {capture:?}
if [ "$1" != "api" ]; then
  echo "stub: unexpected gh command: $*" >&2
  exit 99
fi
case "$2" in
  repos/acme/widgets/pulls/44/files)
    cat <<'JSON'
[
  {{
    "filename": "src/lib.rs",
    "patch": "@@ -10,2 +10,3 @@\n unchanged\n+added\n unchanged tail"
  }}
]
JSON
    ;;
  *)
    echo "stub: unexpected gh api endpoint: $2" >&2
    exit 99
    ;;
esac
"#,
        capture = capture
    )
}

fn github_review_thread_diff_mapping_paginated_stub(capture: &str) -> String {
    format!(
        r#"#!/bin/sh
set -eu
printf '%s\n' "$*" >> {capture:?}
if [ "$1" != "api" ]; then
  echo "stub: unexpected gh command: $*" >&2
  exit 99
fi
case "$2" in
  repos/acme/widgets/pulls/44/files)
    printf '%s\n' '[{{"filename":"README.md","patch":"@@ -1 +1 @@\n-old\n+new"}}]'
    printf '%s\n' '[{{"filename":"src/lib.rs","patch":"@@ -10,2 +10,3 @@\n unchanged\n+added\n unchanged tail"}}]'
    ;;
  *)
    echo "stub: unexpected gh api endpoint: $2" >&2
    exit 99
    ;;
esac
"#,
        capture = capture
    )
}

fn github_review_thread_diff_mapping_multihunk_stub(capture: &str) -> String {
    format!(
        r#"#!/bin/sh
set -eu
printf '%s\n' "$*" >> {capture:?}
if [ "$1" != "api" ]; then
  echo "stub: unexpected gh command: $*" >&2
  exit 99
fi
case "$2" in
  repos/acme/widgets/pulls/44/files)
    cat <<'JSON'
[
  {{
    "filename": "src/lib.rs",
    "patch": "@@ -10,2 +10,3 @@\n unchanged\n+added\n unchanged tail\n@@ -40,2 +40,3 @@\n unchanged\n+second\n unchanged tail"
  }}
]
JSON
    ;;
  *)
    echo "stub: unexpected gh api endpoint: $2" >&2
    exit 99
    ;;
esac
"#,
        capture = capture
    )
}

fn github_review_thread_rejected_stub(capture: &str) -> String {
    github_resumable_thread_stub(capture, false, false, false, "thread-422", false)
}

fn github_review_thread_fail_on_submit_stub(capture: &str) -> String {
    github_resumable_thread_stub(capture, false, false, false, "submit-error", false)
}

fn github_review_thread_approval_422_on_submit_stub(capture: &str) -> String {
    github_resumable_thread_stub(capture, false, false, false, "submit-422", false)
}

fn run_resumable_thread_review(stub: &StubEnv, thread_file: &Path) -> CmdOutput {
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
            "review",
            "44",
            "--decision",
            "comments-only",
            "--submit-review",
            "--expected-head",
            "head-44",
            "--comment",
            "Summary body",
            "--thread-file",
            thread_file.to_str().expect("utf8 path"),
        ],
    )
}

#[test]
fn pr_review_thread_file_creates_resolvable_github_review_thread() {
    let stub = StubEnv::new();
    let capture = stub.tempdir.path().join("gh-args.log");
    let review_file = stub.tempdir.path().join("review.md");
    let thread_file = stub.tempdir.path().join("review-threads.json");
    fs::write(
        &review_file,
        "## Testing Review\n\nOne actionable finding.\n",
    )
    .expect("write review");
    fs::write(
        &thread_file,
        r#"[{"path":"src/lib.rs","line":42,"side":"RIGHT","body":"Add coverage for the rejected profile URL path."}]"#,
    )
    .expect("write thread specs");
    let stub = stub.gh_stub(&github_review_thread_submit_stub(
        &capture.to_string_lossy(),
    ));

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
            "review",
            "44",
            "--decision",
            "comments-only",
            "--submit-review",
            "--expected-head",
            "head-44",
            "--comment-file",
            review_file.to_str().expect("utf8 path"),
            "--thread-file",
            thread_file.to_str().expect("utf8 path"),
        ],
    );

    assert_eq!(out.code, 0, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["schema_version"], "cli.forge-cli.pr.review.v1");
    assert_eq!(env["ok"], true);
    assert_eq!(env["data"]["submitted_review"], true);
    assert_eq!(
        env["data"]["pr_comment_url"],
        "https://github.com/acme/widgets/pull/44#pullrequestreview-9900"
    );
    assert_eq!(env["data"]["review_threads"][0]["id"], "PRRT_kwDOthread");
    assert_eq!(env["data"]["review_threads"][0]["path"], "src/lib.rs");
    assert_eq!(env["data"]["review_threads"][0]["line"], 42);
    assert_eq!(
        env["data"]["review_threads"][0]["url"],
        "https://github.com/acme/widgets/pull/44#discussion_r42"
    );

    let calls = fs::read_to_string(capture).expect("read captured calls");
    assert!(
        calls.contains("repos/acme/widgets/pulls/44 --jq .number"),
        "PR-existence guard missing: {calls}"
    );
    assert!(
        calls.contains("states: [PENDING]"),
        "viewer-owned pending-review guard missing: {calls}"
    );
    assert!(
        calls.contains("addPullRequestReview(input:"),
        "pending review mutation missing: {calls}"
    );
    assert!(
        calls.contains("commitOID=head-44"),
        "pending review mutation must bind the reviewed head: {calls}"
    );
    assert!(
        calls.contains("addPullRequestReviewThread(input:"),
        "thread mutation missing: {calls}"
    );
    assert!(
        calls.contains("submitPullRequestReview(input:"),
        "submit review mutation missing: {calls}"
    );
    assert!(calls.contains("path=src/lib.rs"), "{calls}");
    assert!(calls.contains("line=42"), "{calls}");
    assert!(calls.contains("side=RIGHT"), "{calls}");
    assert!(
        calls.contains("forge-cli:review-finding:v1")
            && calls.contains("Add coverage for the rejected profile URL path."),
        "{calls}"
    );
}

#[test]
fn pr_review_transaction_resumes_after_each_interruption_point() {
    for failure in [
        "pending-create-once",
        "lost-thread-once",
        "before-submit-once",
    ] {
        let stub = StubEnv::new();
        let capture = stub.tempdir.path().join(format!("{failure}-gh-args.log"));
        let thread_file = stub.tempdir.path().join(format!("{failure}-threads.json"));
        fs::write(
            &thread_file,
            r#"[{"path":"src/lib.rs","line":42,"body":"Thread body"}]"#,
        )
        .expect("write thread specs");
        let stub = stub.gh_stub(&github_resumable_thread_stub(
            &capture.to_string_lossy(),
            false,
            false,
            false,
            failure,
            false,
        ));

        let interrupted = run_resumable_thread_review(&stub, &thread_file);
        assert_eq!(
            interrupted.code, 65,
            "failure={failure}; stdout={}; stderr={}",
            interrupted.stdout, interrupted.stderr
        );
        let interrupted_envelope = parse_envelope(&interrupted.stdout);
        assert_eq!(
            interrupted_envelope["error"]["code"], "pending_review_transaction_incomplete",
            "failure={failure}"
        );

        let resumed = run_resumable_thread_review(&stub, &thread_file);
        assert_eq!(
            resumed.code, 0,
            "failure={failure}; stdout={}; stderr={}",
            resumed.stdout, resumed.stderr
        );
        let resumed_envelope = parse_envelope(&resumed.stdout);
        assert_eq!(resumed_envelope["data"]["submitted_review"], true);

        let calls = fs::read_to_string(&capture).expect("read captured calls");
        assert_eq!(
            calls.matches("addPullRequestReview(input:").count(),
            1,
            "the same pending review must be reused after {failure}: {calls}"
        );
        assert_eq!(
            calls.matches("addPullRequestReviewThread(input:").count(),
            1,
            "the same inline comment must be reused after {failure}: {calls}"
        );
        assert!(
            !calls.contains("deletePullRequestReview(input:"),
            "interrupted content must never be deleted after {failure}: {calls}"
        );
    }
}

#[test]
fn pr_review_transaction_resumes_after_every_inline_comment_index() {
    for failure in ["lost-thread-0-once", "lost-thread-1-once"] {
        let stub = StubEnv::new();
        let capture = stub.tempdir.path().join(format!("{failure}-gh-args.log"));
        let thread_file = stub.tempdir.path().join(format!("{failure}-threads.json"));
        fs::write(
            &thread_file,
            r#"[{"path":"src/lib.rs","line":42,"body":"First finding"},{"path":"src/other.rs","line":7,"body":"Second finding"}]"#,
        )
        .expect("write thread specs");
        let stub = stub.gh_stub(&github_resumable_thread_stub(
            &capture.to_string_lossy(),
            false,
            false,
            false,
            failure,
            false,
        ));

        let interrupted = run_resumable_thread_review(&stub, &thread_file);
        assert_eq!(
            interrupted.code, 65,
            "failure={failure}; stdout={}; stderr={}",
            interrupted.stdout, interrupted.stderr
        );
        let resumed = run_resumable_thread_review(&stub, &thread_file);
        assert_eq!(
            resumed.code, 0,
            "failure={failure}; stdout={}; stderr={}",
            resumed.stdout, resumed.stderr
        );

        let calls = fs::read_to_string(&capture).expect("read captured calls");
        assert_eq!(
            calls.matches("addPullRequestReviewThread(input:").count(),
            2,
            "each manifest entry must be created exactly once after {failure}: {calls}"
        );
        assert!(
            !calls.contains("deletePullRequestReview(input:"),
            "inline progress must remain intact after {failure}: {calls}"
        );
    }
}

#[test]
fn pr_review_transaction_recovers_a_lost_submit_response() {
    let stub = StubEnv::new();
    let capture = stub.tempdir.path().join("lost-submit-gh-args.log");
    let thread_file = stub.tempdir.path().join("lost-submit-threads.json");
    fs::write(
        &thread_file,
        r#"[{"path":"src/lib.rs","line":42,"body":"Thread body"}]"#,
    )
    .expect("write thread specs");
    let stub = stub.gh_stub(&github_resumable_thread_stub(
        &capture.to_string_lossy(),
        false,
        false,
        false,
        "lost-submit",
        false,
    ));

    let output = run_resumable_thread_review(&stub, &thread_file);
    assert_eq!(
        output.code, 0,
        "stdout={}\nstderr={}",
        output.stdout, output.stderr
    );
    let envelope = parse_envelope(&output.stdout);
    assert_eq!(envelope["data"]["submitted_review"], true);
    assert_eq!(
        envelope["data"]["pr_comment_url"],
        "https://github.com/acme/widgets/pull/44#pullrequestreview-9900"
    );
    let calls = fs::read_to_string(capture).expect("read captured calls");
    assert_eq!(calls.matches("submitPullRequestReview(input:").count(), 1);
    assert!(
        calls.matches("reviews(first: 100").count() >= 2,
        "the lost response must be reconciled through submitted-review read-back: {calls}"
    );
}

#[test]
fn pr_review_transaction_is_idempotent_across_sessions() {
    let stub = StubEnv::new();
    let capture = stub.tempdir.path().join("idempotent-gh-args.log");
    let thread_file = stub.tempdir.path().join("idempotent-threads.json");
    fs::write(
        &thread_file,
        r#"[{"path":"src/lib.rs","line":42,"body":"Thread body"}]"#,
    )
    .expect("write thread specs");
    let stub = stub.gh_stub(&github_resumable_thread_stub(
        &capture.to_string_lossy(),
        false,
        false,
        false,
        "none",
        false,
    ));

    let first = run_resumable_thread_review(&stub, &thread_file);
    let first_calls = fs::read_to_string(&capture).expect("read first-run calls");
    assert!(
        !first_calls.contains("submittedAt body viewerDidAuthor"),
        "a fresh transaction without a pre-existing receipt must not scan submitted-review history: {first_calls}"
    );
    let second = run_resumable_thread_review(&stub, &thread_file);
    assert_eq!(first.code, 0, "{}", first.stdout);
    assert_eq!(second.code, 0, "{}", second.stdout);
    let first_envelope = parse_envelope(&first.stdout);
    let second_envelope = parse_envelope(&second.stdout);
    assert_eq!(second_envelope["data"]["submitted_review"], true);
    assert_eq!(
        second_envelope["data"]["pr_comment_url"], first_envelope["data"]["pr_comment_url"],
        "an exact rerun must preserve the submitted review identity"
    );

    let calls = fs::read_to_string(capture).expect("read captured calls");
    assert_eq!(calls.matches("addPullRequestReview(input:").count(), 1);
    assert_eq!(
        calls.matches("addPullRequestReviewThread(input:").count(),
        1
    );
    assert_eq!(calls.matches("submitPullRequestReview(input:").count(), 1);
    assert_eq!(
        calls.matches("issues/44/comments --method POST").count(),
        1,
        "the immutable receipt must not be duplicated: {calls}"
    );
}

#[test]
fn pr_review_skips_duplicate_thread_on_unchanged_head() {
    // Cross-run idempotency: when a finding already has a live (non-resolved,
    // non-outdated) thread on the current head, a re-run must not create a
    // duplicate thread, and an all-duplicate review must not be re-submitted.
    let stub = StubEnv::new();
    let capture = stub.tempdir.path().join("gh-args.log");
    let review_file = stub.tempdir.path().join("review.md");
    let thread_file = stub.tempdir.path().join("review-threads.json");
    fs::write(
        &review_file,
        "## Testing Review\n\nOne finding, already threaded.\n",
    )
    .expect("write review");
    fs::write(
        &thread_file,
        r#"[{"path":"src/lib.rs","line":42,"side":"RIGHT","body":"Duplicate finding body."}]"#,
    )
    .expect("write thread specs");
    let stub = stub.gh_stub(&github_review_thread_idempotent_stub(
        &capture.to_string_lossy(),
        false,
        false,
    ));

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
            "review",
            "44",
            "--decision",
            "comments-only",
            "--submit-review",
            "--expected-head",
            "head-44",
            "--comment-file",
            review_file.to_str().expect("utf8 path"),
            "--thread-file",
            thread_file.to_str().expect("utf8 path"),
        ],
    );

    assert_eq!(out.code, 0, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let calls = fs::read_to_string(&capture).expect("read captured calls");
    assert!(
        calls.contains("reviewThreads(first: 100"),
        "existing threads must be read to dedup: {calls}"
    );
    assert!(
        !calls.contains("addPullRequestReviewThread(input:"),
        "a duplicate thread must not be created on an unchanged head: {calls}"
    );
    assert!(
        !calls.contains("submitPullRequestReview(input:"),
        "an all-duplicate review must not be re-submitted: {calls}"
    );
    let env = parse_envelope(&out.stdout);
    assert_eq!(
        env["data"]["threads_skipped_idempotent"], 1,
        "the duplicate finding must be reported as skipped"
    );
    assert_eq!(
        env["data"]["review_threads"]
            .as_array()
            .map(|a| a.len())
            .unwrap_or(0),
        0,
        "no new threads should be posted (the field is omitted when empty)"
    );
    assert_eq!(
        env["data"]["submitted_review"], false,
        "an all-duplicate review is a no-op, so no native review event was submitted"
    );
    assert_eq!(
        env["data"]["pr_comment_url"], "",
        "no review event means no review URL on the idempotent-skip path"
    );
}

#[test]
fn pr_review_reposts_when_matching_thread_is_outdated() {
    // A finding whose only matching thread is outdated (the diff hunk moved) is
    // NOT a live duplicate: it must be posted fresh, not suppressed. This guards
    // the `!outdated` half of the idempotency fingerprint.
    let stub = StubEnv::new();
    let capture = stub.tempdir.path().join("gh-args.log");
    let review_file = stub.tempdir.path().join("review.md");
    let thread_file = stub.tempdir.path().join("review-threads.json");
    fs::write(
        &review_file,
        "## Testing Review\n\nOne finding, prior copy outdated.\n",
    )
    .expect("write review");
    fs::write(
        &thread_file,
        r#"[{"path":"src/lib.rs","line":42,"side":"RIGHT","body":"Duplicate finding body."}]"#,
    )
    .expect("write thread specs");
    let stub = stub.gh_stub(&github_review_thread_idempotent_stub(
        &capture.to_string_lossy(),
        false,
        true,
    ));

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
            "review",
            "44",
            "--decision",
            "comments-only",
            "--submit-review",
            "--expected-head",
            "head-44",
            "--comment-file",
            review_file.to_str().expect("utf8 path"),
            "--thread-file",
            thread_file.to_str().expect("utf8 path"),
        ],
    );

    assert_eq!(out.code, 0, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let calls = fs::read_to_string(&capture).expect("read captured calls");
    assert!(
        calls.contains("addPullRequestReviewThread(input:"),
        "an outdated match must be reposted fresh: {calls}"
    );
    assert!(
        calls.contains("submitPullRequestReview(input:"),
        "the review must be submitted when a fresh thread is posted: {calls}"
    );
    let env = parse_envelope(&out.stdout);
    assert_eq!(
        env["data"]["threads_skipped_idempotent"]
            .as_u64()
            .unwrap_or(0),
        0,
        "an outdated match is not a live duplicate"
    );
    assert_eq!(env["data"]["review_threads"][0]["id"], "PRRT_kwDOthread");
    assert_eq!(env["data"]["submitted_review"], true);
}

#[test]
fn pr_review_skips_only_the_duplicate_and_posts_the_rest() {
    // Partial-skip: one finding already has a live thread, another is new. Only
    // the duplicate is skipped; the survivor is posted and the review submitted.
    // The second spec reuses the duplicate's body on a DIFFERENT path, so it also
    // proves the fingerprint keys on (path, body), not body alone.
    let stub = StubEnv::new();
    let capture = stub.tempdir.path().join("gh-args.log");
    let review_file = stub.tempdir.path().join("review.md");
    let thread_file = stub.tempdir.path().join("review-threads.json");
    fs::write(&review_file, "## Testing Review\n\nOne dup, one new.\n").expect("write review");
    fs::write(
        &thread_file,
        r#"[{"path":"src/lib.rs","line":42,"side":"RIGHT","body":"Duplicate finding body."},{"path":"src/other.rs","line":7,"side":"RIGHT","body":"Duplicate finding body."}]"#,
    )
    .expect("write thread specs");
    let stub = stub.gh_stub(&github_review_thread_idempotent_stub(
        &capture.to_string_lossy(),
        false,
        false,
    ));

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
            "review",
            "44",
            "--decision",
            "comments-only",
            "--submit-review",
            "--expected-head",
            "head-44",
            "--comment-file",
            review_file.to_str().expect("utf8 path"),
            "--thread-file",
            thread_file.to_str().expect("utf8 path"),
        ],
    );

    assert_eq!(out.code, 0, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let calls = fs::read_to_string(&capture).expect("read captured calls");
    assert_eq!(
        calls.matches("addPullRequestReviewThread(input:").count(),
        1,
        "exactly one (survivor) thread must be created: {calls}"
    );
    assert!(
        calls.contains("path=src/other.rs"),
        "the new finding (different path, same body) must be posted: {calls}"
    );
    assert!(
        !calls.contains("path=src/lib.rs"),
        "the duplicate finding must not be re-posted: {calls}"
    );
    assert!(
        calls.contains("submitPullRequestReview(input:"),
        "the review must be submitted when a survivor is posted: {calls}"
    );
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["data"]["threads_skipped_idempotent"], 1);
    assert_eq!(
        env["data"]["review_threads"]
            .as_array()
            .map(|a| a.len())
            .unwrap_or(0),
        1,
        "only the survivor thread is in the payload"
    );
    assert_eq!(env["data"]["submitted_review"], true);
}

#[test]
fn pr_review_reposts_when_matching_thread_is_resolved() {
    // A resolved thread whose (path, body) matches a finding is NOT a live
    // duplicate — the reviewer resolved it, so a re-finding must be posted fresh.
    // Guards the `!resolved` half of the idempotency fingerprint.
    let stub = StubEnv::new();
    let capture = stub.tempdir.path().join("gh-args.log");
    let review_file = stub.tempdir.path().join("review.md");
    let thread_file = stub.tempdir.path().join("review-threads.json");
    fs::write(
        &review_file,
        "## Testing Review\n\nOne finding, prior copy resolved.\n",
    )
    .expect("write review");
    fs::write(
        &thread_file,
        r#"[{"path":"src/lib.rs","line":42,"side":"RIGHT","body":"Duplicate finding body."}]"#,
    )
    .expect("write thread specs");
    let stub = stub.gh_stub(&github_review_thread_idempotent_stub(
        &capture.to_string_lossy(),
        true,
        false,
    ));

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
            "review",
            "44",
            "--decision",
            "comments-only",
            "--submit-review",
            "--expected-head",
            "head-44",
            "--comment-file",
            review_file.to_str().expect("utf8 path"),
            "--thread-file",
            thread_file.to_str().expect("utf8 path"),
        ],
    );

    assert_eq!(out.code, 0, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let calls = fs::read_to_string(&capture).expect("read captured calls");
    assert!(
        calls.contains("addPullRequestReviewThread(input:"),
        "a resolved match must be reposted fresh: {calls}"
    );
    let env = parse_envelope(&out.stdout);
    assert_eq!(
        env["data"]["threads_skipped_idempotent"]
            .as_u64()
            .unwrap_or(0),
        0,
        "a resolved match is not a live duplicate"
    );
    assert_eq!(env["data"]["review_threads"][0]["id"], "PRRT_kwDOthread");
    assert_eq!(env["data"]["submitted_review"], true);
}

#[test]
fn pr_review_thread_file_dry_run_renders_thread_creation_plan() {
    let stub = StubEnv::new().gh_stub("#!/bin/sh\necho should-not-run >&2\nexit 99\n");
    let thread_file = stub.tempdir.path().join("review-threads.json");
    fs::write(
        &thread_file,
        r#"[{"path":"src/lib.rs","line":42,"body":"Thread body"}]"#,
    )
    .expect("write thread specs");

    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "--repo",
            "acme/widgets",
            "--format",
            "json",
            "--dry-run",
            "pr",
            "review",
            "44",
            "--decision",
            "comments-only",
            "--submit-review",
            "--expected-head",
            "head-44",
            "--comment",
            "Summary body",
            "--thread-file",
            thread_file.to_str().expect("utf8 path"),
        ],
    );

    assert_eq!(out.code, 0, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["schema_version"], "cli.forge-cli.pr.review.v1");
    assert_eq!(env["ok"], true);
    assert_eq!(env["data"]["submitted_review"], true);
    assert_eq!(env["data"]["planned_review_threads"], 1);
    let thread_plan = env["data"]["thread_plan"]
        .as_array()
        .expect("thread_plan present")
        .iter()
        .flat_map(|v| v.as_array().into_iter().flatten())
        .map(|v| v.as_str().unwrap_or_default())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        thread_plan.contains("addPullRequestReviewThread(input:"),
        "thread_plan should render thread mutation: {thread_plan}"
    );
    assert!(thread_plan.contains("path=src/lib.rs"), "{thread_plan}");
    assert!(thread_plan.contains("line=42"), "{thread_plan}");
    let pending_guard_plan = env["data"]["pending_review_guard_plan"]
        .as_array()
        .expect("pending_review_guard_plan present")
        .iter()
        .map(|v| v.as_str().unwrap_or_default())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        pending_guard_plan.contains("states: [PENDING]"),
        "threaded dry-run must render the pending-review guard: {pending_guard_plan}"
    );
    for field in [
        "review_state_plan",
        "review_receipt_plan",
        "review_state_verify_plan",
    ] {
        let plan = env["data"][field]
            .as_array()
            .unwrap_or_else(|| panic!("{field} present in threaded dry-run"))
            .iter()
            .map(|value| value.as_str().unwrap_or_default())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            plan.contains("graphql") || plan.contains("issues/44/comments"),
            "{field}: {plan}"
        );
    }
}

#[test]
fn pr_review_validate_thread_file_emits_normalized_preflight_without_backend() {
    let stub = StubEnv::new().gh_stub("#!/bin/sh\necho should-not-run >&2\nexit 99\n");
    let thread_file = stub.tempdir.path().join("review-threads.json");
    fs::write(
        &thread_file,
        r#"[{"path":"src/lib.rs","line":42,"body":"Thread body"}]"#,
    )
    .expect("write thread specs");

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
            "review",
            "validate",
            "--comment",
            "Summary body",
            "--thread-file",
            thread_file.to_str().expect("utf8 path"),
        ],
    );

    assert_eq!(out.code, 0, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["schema_version"], "cli.forge-cli.pr.review.validate.v1");
    assert_eq!(env["ok"], true);
    assert_eq!(env["data"]["provider"], "github");
    assert_eq!(env["data"]["comment"]["present"], true);
    assert_eq!(env["data"]["comment"]["bytes"], 12);
    assert_eq!(env["data"]["review_threads"]["count"], 1);
    assert_eq!(env["data"]["review_threads"]["specs"][0]["index"], 1);
    assert_eq!(
        env["data"]["review_threads"]["specs"][0]["path"],
        "src/lib.rs"
    );
    assert_eq!(env["data"]["review_threads"]["specs"][0]["line"], 42);
    assert_eq!(
        env["data"]["review_threads"]["specs"][0]["subject_type"],
        "LINE"
    );
}

#[test]
fn pr_review_validate_runs_on_local_provider_without_store_backend() {
    let stub = StubEnv::new().gh_stub("#!/bin/sh\necho should-not-run >&2\nexit 99\n");
    let thread_file = stub.tempdir.path().join("review-threads.json");
    fs::write(
        &thread_file,
        r#"[{"path":"src/lib.rs","line":42,"body":"Thread body"}]"#,
    )
    .expect("write thread specs");

    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "local",
            "--repo",
            "local:demo",
            "--format",
            "json",
            "pr",
            "review",
            "validate",
            "--comment",
            "Summary body",
            "--thread-file",
            thread_file.to_str().expect("utf8 path"),
        ],
    );

    assert_eq!(out.code, 0, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["schema_version"], "cli.forge-cli.pr.review.validate.v1");
    assert_eq!(env["ok"], true);
    assert_eq!(env["data"]["provider"], "local");
    assert_eq!(env["data"]["review_threads"]["count"], 1);
    assert_eq!(env["data"]["review_threads"]["diff_checked"], false);
}

#[test]
fn pr_review_validate_check_diff_dry_run_renders_diff_plan_without_backend() {
    let stub = StubEnv::new();
    let capture = stub.tempdir.path().join("gh-args.log");
    let thread_file = stub.tempdir.path().join("review-threads.json");
    fs::write(
        &thread_file,
        r#"[{"path":"src/lib.rs","line":42,"body":"Thread body"}]"#,
    )
    .expect("write thread specs");
    let stub = stub.gh_stub(&format!(
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {capture:?}\necho should-not-run >&2\nexit 99\n",
        capture = capture.to_string_lossy()
    ));

    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "--repo",
            "acme/widgets",
            "--format",
            "json",
            "--dry-run",
            "pr",
            "review",
            "validate",
            "44",
            "--comment",
            "Summary body",
            "--thread-file",
            thread_file.to_str().expect("utf8 path"),
            "--check-diff",
        ],
    );

    assert_eq!(out.code, 0, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["schema_version"], "cli.forge-cli.pr.review.validate.v1");
    assert_eq!(env["ok"], true);
    assert_eq!(env["data"]["check_diff"], true);
    assert_eq!(env["data"]["review_threads"]["diff_checked"], false);
    let diff_plan = env["data"]["diff_plan"]
        .as_array()
        .expect("diff_plan rendered")
        .iter()
        .map(|value| value.as_str().unwrap_or_default())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        diff_plan.contains("repos/acme/widgets/pulls/44/files"),
        "{diff_plan}"
    );
    assert!(diff_plan.contains("--paginate"), "{diff_plan}");
    assert!(
        !capture.exists(),
        "dry-run validate must not invoke gh; captured {}",
        capture.display()
    );
}

#[test]
fn pr_review_validate_check_diff_uses_parent_pr_id() {
    let stub = StubEnv::new();
    let capture = stub.tempdir.path().join("gh-args.log");
    let thread_file = stub.tempdir.path().join("review-threads.json");
    fs::write(
        &thread_file,
        r#"[{"path":"src/lib.rs","line":42,"body":"Thread body"}]"#,
    )
    .expect("write thread specs");
    let stub = stub.gh_stub(&format!(
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {capture:?}\necho should-not-run >&2\nexit 99\n",
        capture = capture.to_string_lossy()
    ));

    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "--repo",
            "acme/widgets",
            "--format",
            "json",
            "--dry-run",
            "pr",
            "review",
            "44",
            "validate",
            "--thread-file",
            thread_file.to_str().expect("utf8 path"),
            "--check-diff",
        ],
    );

    assert_eq!(out.code, 0, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["schema_version"], "cli.forge-cli.pr.review.validate.v1");
    assert_eq!(env["ok"], true);
    assert_eq!(env["data"]["number"], 44);
    let diff_plan = env["data"]["diff_plan"]
        .as_array()
        .expect("diff_plan rendered")
        .iter()
        .map(|value| value.as_str().unwrap_or_default())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        diff_plan.contains("repos/acme/widgets/pulls/44/files"),
        "{diff_plan}"
    );
    assert!(
        !capture.exists(),
        "dry-run validate must not invoke gh; captured {}",
        capture.display()
    );
}

#[test]
fn pr_review_validate_inherits_parent_comment_and_thread_files() {
    let stub = StubEnv::new().gh_stub("#!/bin/sh\necho should-not-run >&2\nexit 99\n");
    let review_file = stub.tempdir.path().join("review.md");
    let thread_file = stub.tempdir.path().join("review-threads.json");
    fs::write(&review_file, "Summary body").expect("write review");
    fs::write(
        &thread_file,
        r#"[{"path":"src/lib.rs","line":42,"body":"Thread body"}]"#,
    )
    .expect("write thread specs");

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
            "review",
            "--comment-file",
            review_file.to_str().expect("utf8 path"),
            "--thread-file",
            thread_file.to_str().expect("utf8 path"),
            "validate",
        ],
    );

    assert_eq!(out.code, 0, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["schema_version"], "cli.forge-cli.pr.review.validate.v1");
    assert_eq!(env["ok"], true);
    assert_eq!(env["data"]["comment"]["present"], true);
    assert_eq!(env["data"]["comment"]["bytes"], 12);
    assert_eq!(env["data"]["review_threads"]["count"], 1);
    assert_eq!(
        env["data"]["review_threads"]["specs"][0]["path"],
        "src/lib.rs"
    );
}

#[test]
fn pr_review_validate_check_diff_dry_run_requires_repo_before_diff_plan() {
    let stub = StubEnv::new();
    let capture = stub.tempdir.path().join("gh-args.log");
    let thread_file = stub.tempdir.path().join("review-threads.json");
    fs::write(
        &thread_file,
        r#"[{"path":"src/lib.rs","line":42,"body":"Thread body"}]"#,
    )
    .expect("write thread specs");
    let stub = stub.gh_stub(&format!(
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {capture:?}\necho should-not-run >&2\nexit 99\n",
        capture = capture.to_string_lossy()
    ));

    let out = run_forge_cli_in(
        &stub,
        &[
            "--provider",
            "github",
            "--format",
            "json",
            "--dry-run",
            "pr",
            "review",
            "validate",
            "44",
            "--thread-file",
            thread_file.to_str().expect("utf8 path"),
            "--check-diff",
        ],
        Some(stub.tempdir.path()),
    );

    assert_eq!(out.code, 65, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["schema_version"], "cli.forge-cli.error.v1");
    assert_eq!(env["ok"], false);
    assert_eq!(env["error"]["code"], "repo_required");
    assert!(
        !capture.exists(),
        "repo validation must fail before invoking gh; captured {}",
        capture.display()
    );
}

#[test]
fn pr_review_validate_check_diff_requires_id() {
    let stub = StubEnv::new().gh_stub("#!/bin/sh\necho should-not-run >&2\nexit 99\n");

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
            "review",
            "validate",
            "--comment",
            "Summary body",
            "--check-diff",
        ],
    );

    assert_eq!(out.code, 65, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["schema_version"], "cli.forge-cli.error.v1");
    assert_eq!(env["ok"], false);
    assert_eq!(env["error"]["code"], "review_validate_id_required");
}

#[test]
fn pr_review_validate_check_diff_requires_id_before_reading_thread_file_stdin() {
    let stub = StubEnv::new().gh_stub("#!/bin/sh\necho should-not-run >&2\nexit 99\n");

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
            "review",
            "validate",
            "--thread-file",
            "-",
            "--check-diff",
        ],
        "not json",
    );

    assert_eq!(out.code, 65, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["schema_version"], "cli.forge-cli.error.v1");
    assert_eq!(env["ok"], false);
    assert_eq!(env["error"]["code"], "review_validate_id_required");
}

#[test]
fn pr_review_validate_check_diff_rejects_reversed_range() {
    let stub = StubEnv::new();
    let capture = stub.tempdir.path().join("gh-args.log");
    let thread_file = stub.tempdir.path().join("review-threads.json");
    fs::write(
        &thread_file,
        r#"[{"path":"src/lib.rs","startLine":12,"line":11,"body":"Thread body"}]"#,
    )
    .expect("write thread specs");
    let stub = stub.gh_stub(&github_review_thread_diff_mapping_stub(
        &capture.to_string_lossy(),
    ));

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
            "review",
            "validate",
            "44",
            "--thread-file",
            thread_file.to_str().expect("utf8 path"),
            "--check-diff",
        ],
    );

    assert_eq!(out.code, 65, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["schema_version"], "cli.forge-cli.error.v1");
    assert_eq!(env["ok"], false);
    assert_eq!(env["error"]["code"], "review_thread_range_not_in_diff");
}

#[test]
fn pr_review_validate_check_diff_rejects_range_across_hunks() {
    let stub = StubEnv::new();
    let capture = stub.tempdir.path().join("gh-args.log");
    let thread_file = stub.tempdir.path().join("review-threads.json");
    fs::write(
        &thread_file,
        r#"[{"path":"src/lib.rs","startLine":11,"line":41,"body":"Thread body"}]"#,
    )
    .expect("write thread specs");
    let stub = stub.gh_stub(&github_review_thread_diff_mapping_multihunk_stub(
        &capture.to_string_lossy(),
    ));

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
            "review",
            "validate",
            "44",
            "--thread-file",
            thread_file.to_str().expect("utf8 path"),
            "--check-diff",
        ],
    );

    assert_eq!(out.code, 65, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["schema_version"], "cli.forge-cli.error.v1");
    assert_eq!(env["ok"], false);
    assert_eq!(env["error"]["code"], "review_thread_range_not_in_diff");
}

#[test]
fn pr_review_validate_check_diff_rejects_uncommentable_line() {
    let stub = StubEnv::new();
    let capture = stub.tempdir.path().join("gh-args.log");
    let thread_file = stub.tempdir.path().join("review-threads.json");
    fs::write(
        &thread_file,
        r#"[{"path":"src/lib.rs","line":99,"body":"Thread body"}]"#,
    )
    .expect("write thread specs");
    let stub = stub.gh_stub(&github_review_thread_diff_mapping_stub(
        &capture.to_string_lossy(),
    ));

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
            "review",
            "validate",
            "44",
            "--comment",
            "Summary body",
            "--thread-file",
            thread_file.to_str().expect("utf8 path"),
            "--check-diff",
        ],
    );

    assert_eq!(out.code, 65, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["schema_version"], "cli.forge-cli.error.v1");
    assert_eq!(env["ok"], false);
    assert_eq!(env["error"]["code"], "review_thread_line_not_in_diff");
    assert!(
        env["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("thread spec #1"),
        "{}",
        env["error"]["message"]
    );
    let calls = fs::read_to_string(capture).expect("read captured calls");
    assert!(
        calls.contains("repos/acme/widgets/pulls/44/files"),
        "{calls}"
    );
}

#[test]
fn pr_review_validate_check_diff_rejects_left_side_context_line() {
    let stub = StubEnv::new();
    let capture = stub.tempdir.path().join("gh-args.log");
    let thread_file = stub.tempdir.path().join("review-threads.json");
    fs::write(
        &thread_file,
        r#"[{"path":"src/lib.rs","line":10,"side":"LEFT","body":"Thread body"}]"#,
    )
    .expect("write thread specs");
    let stub = stub.gh_stub(&github_review_thread_diff_mapping_stub(
        &capture.to_string_lossy(),
    ));

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
            "review",
            "validate",
            "44",
            "--thread-file",
            thread_file.to_str().expect("utf8 path"),
            "--check-diff",
        ],
    );

    assert_eq!(out.code, 65, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["schema_version"], "cli.forge-cli.error.v1");
    assert_eq!(env["ok"], false);
    assert_eq!(env["error"]["code"], "review_thread_line_not_in_diff");
}

#[test]
fn pr_review_validate_check_diff_accepts_paginated_file_arrays() {
    let stub = StubEnv::new();
    let capture = stub.tempdir.path().join("gh-args.log");
    let thread_file = stub.tempdir.path().join("review-threads.json");
    fs::write(
        &thread_file,
        r#"[{"path":"src/lib.rs","line":11,"body":"Thread body"}]"#,
    )
    .expect("write thread specs");
    let stub = stub.gh_stub(&github_review_thread_diff_mapping_paginated_stub(
        &capture.to_string_lossy(),
    ));

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
            "review",
            "validate",
            "44",
            "--comment",
            "Summary body",
            "--thread-file",
            thread_file.to_str().expect("utf8 path"),
            "--check-diff",
        ],
    );

    assert_eq!(out.code, 0, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["schema_version"], "cli.forge-cli.pr.review.validate.v1");
    assert_eq!(env["ok"], true);
    assert_eq!(env["data"]["review_threads"]["diff_checked"], true);
    let calls = fs::read_to_string(capture).expect("read captured calls");
    assert!(
        calls.contains("repos/acme/widgets/pulls/44/files"),
        "{calls}"
    );
}

#[test]
fn pr_review_thread_file_rejects_malformed_json() {
    let stub = StubEnv::new().gh_stub("#!/bin/sh\necho should-not-run >&2\nexit 99\n");
    let thread_file = stub.tempdir.path().join("review-threads.json");
    fs::write(&thread_file, r#"[{"path":"src/lib.rs","body":42}]"#)
        .expect("write bad thread specs");

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
            "review",
            "44",
            "--decision",
            "comments-only",
            "--submit-review",
            "--comment",
            "Summary body",
            "--thread-file",
            thread_file.to_str().expect("utf8 path"),
        ],
    );

    assert_eq!(out.code, 65, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["schema_version"], "cli.forge-cli.error.v1");
    assert_eq!(env["ok"], false);
    assert_eq!(env["error"]["code"], "invalid_review_thread_spec");
}

#[test]
fn pr_review_thread_file_mirror_issue_without_issue_checks_before_reading_thread_file() {
    let stub = StubEnv::new();
    let capture = stub.tempdir.path().join("gh-args.log");
    let stub = stub.gh_stub(&github_review_thread_submit_stub(
        &capture.to_string_lossy(),
    ));

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
            "review",
            "44",
            "--decision",
            "comments-only",
            "--submit-review",
            "--comment",
            "Summary body",
            "--mirror-issue",
            "--thread-file",
            "/this/path/does/not/exist",
        ],
    );

    assert_eq!(out.code, 65, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["schema_version"], "cli.forge-cli.error.v1");
    assert_eq!(env["ok"], false);
    assert_eq!(env["error"]["code"], "issue_required");
    assert_backend_not_invoked(&capture);
}

#[test]
fn pr_review_thread_file_rejects_privacy_guard_violations_before_backend() {
    let cases = [
        (
            r#"[{"path":"/Users/terry/project/secret.rs","line":42,"body":"Thread body"}]"#,
            "local_path_present",
        ),
        (
            r#"[{"path":"src/lib.rs","line":42,"body":"See /Users/terry/secret.txt"}]"#,
            "local_path_present",
        ),
        (
            r#"[{"path":"src/lib.rs\\n","line":42,"body":"Thread body"}]"#,
            "markdown_escaped_control",
        ),
        (
            r#"[{"path":"src/lib.rs","line":42,"body":"Line one\\nline two"}]"#,
            "markdown_escaped_control",
        ),
    ];

    for (idx, (thread_spec, expected_kind)) in cases.iter().enumerate() {
        let stub = StubEnv::new();
        let capture = stub.tempdir.path().join("gh-args.log");
        let thread_file = stub
            .tempdir
            .path()
            .join(format!("review-threads-{idx}.json"));
        fs::write(&thread_file, thread_spec).expect("write thread specs");
        let stub = stub.gh_stub(&github_review_thread_submit_stub(
            &capture.to_string_lossy(),
        ));

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
                "review",
                "44",
                "--decision",
                "comments-only",
                "--submit-review",
                "--comment",
                "Summary body",
                "--thread-file",
                thread_file.to_str().expect("utf8 path"),
            ],
        );

        assert_eq!(
            out.code, 65,
            "case {idx} stdout={}\nstderr={}",
            out.stdout, out.stderr
        );
        let env = parse_envelope(&out.stdout);
        assert_eq!(env["schema_version"], "cli.forge-cli.error.v1");
        assert_eq!(env["ok"], false);
        assert_eq!(env["error"]["code"], *expected_kind);
        assert_backend_not_invoked(&capture);
    }
}

#[test]
fn pr_review_thread_file_rejects_too_many_specs_before_backend() {
    let stub = StubEnv::new();
    let capture = stub.tempdir.path().join("gh-args.log");
    let thread_file = stub.tempdir.path().join("review-threads.json");
    let specs = (0..51)
        .map(|idx| {
            format!(
                r#"{{"path":"src/lib.rs","line":{},"body":"Thread body {}"}}"#,
                idx + 1,
                idx + 1
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    fs::write(&thread_file, format!("[{specs}]")).expect("write thread specs");
    let stub = stub.gh_stub(&github_review_thread_submit_stub(
        &capture.to_string_lossy(),
    ));

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
            "review",
            "44",
            "--decision",
            "comments-only",
            "--submit-review",
            "--comment",
            "Summary body",
            "--thread-file",
            thread_file.to_str().expect("utf8 path"),
        ],
    );

    assert_eq!(out.code, 65, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["schema_version"], "cli.forge-cli.error.v1");
    assert_eq!(env["ok"], false);
    assert_eq!(env["error"]["code"], "invalid_review_thread_spec");
    assert_backend_not_invoked(&capture);
}

#[test]
fn pr_review_thread_file_rejects_oversized_body_before_backend() {
    let stub = StubEnv::new();
    let capture = stub.tempdir.path().join("gh-args.log");
    let thread_file = stub.tempdir.path().join("review-threads.json");
    let body = "a".repeat(16 * 1024 + 1);
    let spec = serde_json::json!([
        {
            "path": "src/lib.rs",
            "line": 42,
            "body": body,
        }
    ]);
    fs::write(&thread_file, serde_json::to_string(&spec).unwrap()).expect("write thread specs");
    let stub = stub.gh_stub(&github_review_thread_submit_stub(
        &capture.to_string_lossy(),
    ));

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
            "review",
            "44",
            "--decision",
            "comments-only",
            "--submit-review",
            "--comment",
            "Summary body",
            "--thread-file",
            thread_file.to_str().expect("utf8 path"),
        ],
    );

    assert_eq!(out.code, 65, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["schema_version"], "cli.forge-cli.error.v1");
    assert_eq!(env["ok"], false);
    assert_eq!(env["error"]["code"], "invalid_review_thread_spec");
    assert_backend_not_invoked(&capture);
}

#[test]
fn pr_review_thread_file_rejects_oversized_stdin_before_backend() {
    let stub = StubEnv::new();
    let capture = stub.tempdir.path().join("gh-args.log");
    let stdin = "x".repeat(256 * 1024 + 2);
    let stub = stub.gh_stub(&github_review_thread_submit_stub(
        &capture.to_string_lossy(),
    ));

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
            "review",
            "44",
            "--decision",
            "comments-only",
            "--submit-review",
            "--comment",
            "Summary body",
            "--thread-file",
            "-",
        ],
        &stdin,
    );

    assert_eq!(out.code, 65, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["schema_version"], "cli.forge-cli.error.v1");
    assert_eq!(env["ok"], false);
    assert_eq!(env["error"]["code"], "invalid_review_thread_spec");
    assert_backend_not_invoked(&capture);
}

#[test]
fn pr_review_thread_file_preserves_pending_review_after_thread_failure() {
    let stub = StubEnv::new();
    let capture = stub.tempdir.path().join("gh-args.log");
    let thread_file = stub.tempdir.path().join("review-threads.json");
    fs::write(
        &thread_file,
        r#"[{"path":"src/lib.rs","line":42,"body":"Thread body"}]"#,
    )
    .expect("write thread specs");
    let stub = stub.gh_stub(&github_review_thread_fail_after_pending_stub(
        &capture.to_string_lossy(),
    ));

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
            "review",
            "44",
            "--decision",
            "comments-only",
            "--submit-review",
            "--expected-head",
            "head-44",
            "--comment",
            "Summary body",
            "--thread-file",
            thread_file.to_str().expect("utf8 path"),
        ],
    );

    assert_eq!(out.code, 65, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["schema_version"], "cli.forge-cli.error.v1");
    assert_eq!(env["ok"], false);
    assert_eq!(
        env["error"]["code"],
        "pending_review_transaction_incomplete"
    );

    let calls = fs::read_to_string(capture).expect("read captured calls");
    assert!(calls.contains("addPullRequestReview(input:"), "{calls}");
    assert!(
        calls.contains("addPullRequestReviewThread(input:"),
        "{calls}"
    );
    assert!(
        !calls.contains("deletePullRequestReview(input:"),
        "interrupted inline review content must remain resumable: {calls}"
    );
    assert!(
        !calls.contains("submitPullRequestReview(input:"),
        "failed pending review must not be submitted: {calls}"
    );
}

#[test]
fn pr_review_thread_file_preserves_pending_review_after_github_diff_failure() {
    let stub = StubEnv::new();
    let capture = stub.tempdir.path().join("gh-args.log");
    let thread_file = stub.tempdir.path().join("review-threads.json");
    fs::write(
        &thread_file,
        r#"[{"path":"src/lib.rs","line":99,"body":"Thread body"}]"#,
    )
    .expect("write thread specs");
    let stub = stub.gh_stub(&github_review_thread_rejected_stub(
        &capture.to_string_lossy(),
    ));

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
            "review",
            "44",
            "--decision",
            "comments-only",
            "--submit-review",
            "--expected-head",
            "head-44",
            "--comment",
            "Summary body",
            "--thread-file",
            thread_file.to_str().expect("utf8 path"),
        ],
    );

    assert_eq!(out.code, 65, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["schema_version"], "cli.forge-cli.error.v1");
    assert_eq!(env["ok"], false);
    assert_eq!(
        env["error"]["code"],
        "pending_review_transaction_incomplete"
    );
    let detail = env["error"]["details"]["detail"]
        .as_str()
        .expect("detail is preserved");
    assert!(detail.contains("thread_spec_index=1"), "{detail}");
    assert!(detail.contains("thread_spec_path=src/lib.rs"), "{detail}");
    assert!(detail.contains("thread_spec_line=99"), "{detail}");
    assert!(detail.contains("line must be part of the diff"), "{detail}");

    let calls = fs::read_to_string(capture).expect("read captured calls");
    assert!(calls.contains("addPullRequestReview(input:"), "{calls}");
    assert!(
        calls.contains("addPullRequestReviewThread(input:"),
        "{calls}"
    );
    assert!(
        !calls.contains("deletePullRequestReview(input:"),
        "rejected inline content must remain available for recovery: {calls}"
    );
    assert!(
        !calls.contains("submitPullRequestReview(input:"),
        "failed pending review must not be submitted: {calls}"
    );
}

#[test]
fn pr_review_thread_file_preserves_pending_review_after_submit_failure() {
    let stub = StubEnv::new();
    let capture = stub.tempdir.path().join("gh-args.log");
    let thread_file = stub.tempdir.path().join("review-threads.json");
    fs::write(
        &thread_file,
        r#"[{"path":"src/lib.rs","line":42,"body":"Thread body"}]"#,
    )
    .expect("write thread specs");
    let stub = stub.gh_stub(&github_review_thread_fail_on_submit_stub(
        &capture.to_string_lossy(),
    ));

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
            "review",
            "44",
            "--decision",
            "comments-only",
            "--submit-review",
            "--expected-head",
            "head-44",
            "--comment",
            "Summary body",
            "--thread-file",
            thread_file.to_str().expect("utf8 path"),
        ],
    );

    assert_eq!(out.code, 65, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["schema_version"], "cli.forge-cli.error.v1");
    assert_eq!(env["ok"], false);
    assert_eq!(
        env["error"]["code"],
        "pending_review_transaction_incomplete"
    );

    let calls = fs::read_to_string(capture).expect("read captured calls");
    assert!(calls.contains("addPullRequestReview(input:"), "{calls}");
    assert!(
        calls.contains("addPullRequestReviewThread(input:"),
        "{calls}"
    );
    assert!(
        calls.contains("submitPullRequestReview(input:"),
        "submit should have been attempted after thread creation: {calls}"
    );
    assert!(
        !calls.contains("deletePullRequestReview(input:"),
        "an unknown submit result must preserve the pending review: {calls}"
    );
}

#[test]
fn pr_review_thread_file_submit_422_is_actionable_and_preserves_content() {
    let stub = StubEnv::new();
    let capture = stub.tempdir.path().join("gh-args.log");
    let thread_file = stub.tempdir.path().join("review-threads.json");
    fs::write(
        &thread_file,
        r#"[{"path":"src/lib.rs","line":42,"body":"Thread body"}]"#,
    )
    .expect("write thread specs");
    let stub = stub.gh_stub(&github_review_thread_approval_422_on_submit_stub(
        &capture.to_string_lossy(),
    ));

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
            "review",
            "44",
            "--decision",
            "approve",
            "--submit-review",
            "--expected-head",
            "head-44",
            "--comment",
            "Summary body",
            "--thread-file",
            thread_file.to_str().expect("utf8 path"),
        ],
    );

    assert_eq!(out.code, 65, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["schema_version"], "cli.forge-cli.error.v1");
    assert_eq!(env["ok"], false);
    assert_eq!(
        env["error"]["code"],
        "pending_review_transaction_incomplete"
    );
    let detail = env["error"]["details"]["detail"]
        .as_str()
        .expect("detail is preserved");
    assert!(detail.contains("HTTP 422"), "{detail}");
    assert!(detail.contains("raw_backend_error_detail="), "{detail}");
    assert!(
        detail.contains("Only users with explicit access can approve pull requests"),
        "{detail}"
    );

    let calls = fs::read_to_string(capture).expect("read captured calls");
    assert!(calls.contains("addPullRequestReview(input:"), "{calls}");
    assert!(
        calls.contains("addPullRequestReviewThread(input:"),
        "{calls}"
    );
    assert!(
        calls.contains("submitPullRequestReview(input:"),
        "submit should have been attempted after thread creation: {calls}"
    );
    assert!(
        !calls.contains("deletePullRequestReview(input:"),
        "a rejected submit must preserve the pending review: {calls}"
    );
}

#[test]
fn pr_review_thread_file_rejects_without_submit_review() {
    let stub = StubEnv::new().gh_stub("#!/bin/sh\necho should-not-run >&2\nexit 99\n");
    let thread_file = stub.tempdir.path().join("review-threads.json");
    fs::write(
        &thread_file,
        r#"[{"path":"src/lib.rs","line":42,"body":"Thread body"}]"#,
    )
    .expect("write thread specs");

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
            "review",
            "44",
            "--decision",
            "comments-only",
            "--comment",
            "Summary body",
            "--thread-file",
            thread_file.to_str().expect("utf8 path"),
        ],
    );

    assert_eq!(out.code, 65, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["schema_version"], "cli.forge-cli.error.v1");
    assert_eq!(env["ok"], false);
    assert_eq!(env["error"]["code"], "thread_file_requires_submit_review");
}

#[test]
fn pr_review_thread_file_rejects_gitlab() {
    let stub = StubEnv::new().glab_stub("#!/bin/sh\necho should-not-run >&2\nexit 99\n");
    let thread_file = stub.tempdir.path().join("review-threads.json");
    fs::write(
        &thread_file,
        r#"[{"path":"src/lib.rs","line":42,"body":"Thread body"}]"#,
    )
    .expect("write thread specs");

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
            "review",
            "44",
            "--decision",
            "comments-only",
            "--submit-review",
            "--comment",
            "Summary body",
            "--thread-file",
            thread_file.to_str().expect("utf8 path"),
        ],
    );

    assert_eq!(out.code, 64, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["schema_version"], "cli.forge-cli.error.v1");
    assert_eq!(env["ok"], false);
    assert_eq!(env["error"]["code"], "provider_unsupported");
}

#[test]
fn pr_review_submit_native_review_event_on_github() {
    // `--submit-review` must create a native GitHub review object via
    // POST .../pulls/<id>/reviews, mapping --decision to the review `event`,
    // and must NOT post an issue comment.
    let stub = StubEnv::new();
    let capture = stub.tempdir.path().join("gh-args.log");
    let stub = stub.gh_stub(&github_review_submit_stub(&capture.to_string_lossy()));

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
            "review",
            "44",
            "--decision",
            "request-changes",
            "--submit-review",
            "--expected-head",
            "head-44",
            "--comment",
            "Needs another pass.",
        ],
    );

    assert_eq!(out.code, 0, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["schema_version"], "cli.forge-cli.pr.review.v1");
    assert_eq!(env["ok"], true);
    assert_eq!(env["data"]["provider"], "github");
    assert_eq!(env["data"]["number"], 44);
    assert_eq!(env["data"]["decision"], "request-changes");
    assert_eq!(env["data"]["submitted_review"], true);
    assert_eq!(
        env["data"]["pr_comment_url"],
        "https://github.com/acme/widgets/pull/44#pullrequestreview-9900"
    );

    let calls = fs::read_to_string(capture).expect("read captured calls");
    // The PR-existence guard still runs before the native submit.
    assert!(
        calls.contains("repos/acme/widgets/pulls/44 --jq .number"),
        "PR-existence guard missing: {calls}"
    );
    // Native review submission to the reviews endpoint, with the mapped event.
    assert!(
        calls.contains("repos/acme/widgets/pulls/44/reviews"),
        "native review POST missing: {calls}"
    );
    assert!(calls.contains("--method POST"), "{calls}");
    assert!(
        calls.contains("commit_id=head-44"),
        "native review POST must bind the reviewed head: {calls}"
    );
    assert!(
        calls.contains("event=REQUEST_CHANGES"),
        "decision must map to the review event: {calls}"
    );
    assert!(calls.contains("body=Needs another pass."), "{calls}");
    // It must NOT fall back to the issue-comment posting path.
    assert!(
        !calls.contains("issues/44/comments"),
        "native review must not post an issue comment: {calls}"
    );
}

#[test]
fn pr_review_submit_native_requires_expected_head_before_backend() {
    let stub = StubEnv::new();
    let capture = stub.tempdir.path().join("gh-args.log");
    let stub = stub.gh_stub(&github_review_submit_stub(&capture.to_string_lossy()));

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
            "review",
            "44",
            "--decision",
            "approve",
            "--submit-review",
        ],
    );

    assert_eq!(out.code, 65, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["error"]["code"], "expected_review_head_required");
    assert_backend_not_invoked(&capture);
}

#[test]
fn pr_review_expected_head_requires_submit_review_before_backend() {
    let stub = StubEnv::new();
    let capture = stub.tempdir.path().join("gh-args.log");
    let stub = stub.gh_stub(&github_review_submit_stub(&capture.to_string_lossy()));

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
            "review",
            "44",
            "--decision",
            "comments-only",
            "--comment",
            "Summary body",
            "--expected-head",
            "head-44",
        ],
    );

    assert_eq!(out.code, 65, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(
        env["error"]["code"],
        "expected_review_head_requires_submit_review"
    );
    assert_backend_not_invoked(&capture);
}

#[test]
fn pr_review_submit_native_rejects_expected_head_mismatch_before_mutation() {
    let stub = StubEnv::new();
    let capture = stub.tempdir.path().join("gh-args.log");
    let stub = stub.gh_stub(&github_review_submit_stub(&capture.to_string_lossy()));

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
            "review",
            "44",
            "--decision",
            "approve",
            "--submit-review",
            "--expected-head",
            "head-old",
        ],
    );

    assert_eq!(out.code, 65, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["error"]["code"], "github_review_head_changed");
    let detail = env["error"]["details"]["detail"]
        .as_str()
        .expect("detail is preserved");
    assert!(detail.contains("expected_head=head-old"), "{detail}");
    assert!(detail.contains("provider_head=head-44"), "{detail}");
    let calls = fs::read_to_string(capture).expect("read captured calls");
    assert!(calls.contains("states: [PENDING]"), "{calls}");
    assert!(
        !calls.contains("repos/acme/widgets/pulls/44/reviews"),
        "head mismatch must stop before native review mutation: {calls}"
    );
}

#[test]
fn pr_review_submit_native_rejects_viewer_owned_pending_review_before_mutation() {
    let stub = StubEnv::new();
    let capture = stub.tempdir.path().join("gh-args.log");
    let stub = stub.gh_stub(&github_review_submit_pending_conflict_stub(
        &capture.to_string_lossy(),
    ));

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
            "review",
            "44",
            "--decision",
            "comments-only",
            "--submit-review",
            "--expected-head",
            "head-44",
            "--comment",
            "Summary body",
        ],
    );

    assert_eq!(out.code, 1, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["schema_version"], "cli.forge-cli.error.v1");
    assert_eq!(env["ok"], false);
    assert_eq!(env["error"]["code"], "github_pending_review_exists");
    let detail = env["error"]["details"]["detail"]
        .as_str()
        .expect("detail is preserved");
    assert!(detail.contains("head_sha=head-44"), "{detail}");
    assert!(detail.contains("pending_review_count=1"), "{detail}");
    assert!(
        detail.contains("deletable_pending_review_count=0"),
        "viewer ownership must block independently of delete capability: {detail}"
    );

    let calls = fs::read_to_string(capture).expect("read captured calls");
    assert!(calls.contains("states: [PENDING]"), "{calls}");
    assert!(
        !calls.contains("repos/acme/widgets/pulls/44/reviews"),
        "native review mutation must not run after a pending-review conflict: {calls}"
    );
}

#[test]
fn pr_review_thread_file_preserves_unmarked_pending_review_before_mutation() {
    let stub = StubEnv::new();
    let capture = stub.tempdir.path().join("gh-args.log");
    let thread_file = stub.tempdir.path().join("review-threads.json");
    fs::write(
        &thread_file,
        r#"[{"path":"src/lib.rs","line":42,"body":"Thread body"}]"#,
    )
    .expect("write thread specs");
    let stub = stub.gh_stub(&github_resumable_thread_stub(
        &capture.to_string_lossy(),
        false,
        false,
        false,
        "none",
        true,
    ));

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
            "review",
            "44",
            "--decision",
            "comments-only",
            "--submit-review",
            "--expected-head",
            "head-44",
            "--comment",
            "Summary body",
            "--thread-file",
            thread_file.to_str().expect("utf8 path"),
        ],
    );

    assert_eq!(out.code, 65, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["error"]["code"], "pending_review_manifest_mismatch");
    let calls = fs::read_to_string(capture).expect("read captured calls");
    assert!(calls.contains("states: [PENDING]"), "{calls}");
    assert!(
        !calls.contains("addPullRequestReview(input:"),
        "threaded review must not create a second pending review: {calls}"
    );
    assert!(
        !calls.contains("issues/44/comments --method POST"),
        "a unmarked mismatch must not append a receipt: {calls}"
    );
}

#[test]
fn pr_review_submit_native_approve_allows_empty_body() {
    // GitHub permits a body-less APPROVE review, so `--submit-review --decision
    // approve` with no comment must submit event=APPROVE with no body field —
    // the empty-body guard is relaxed only for native approve.
    let stub = StubEnv::new();
    let capture = stub.tempdir.path().join("gh-args.log");
    let stub = stub.gh_stub(&github_review_submit_stub(&capture.to_string_lossy()));

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
            "review",
            "44",
            "--decision",
            "approve",
            "--submit-review",
            "--expected-head",
            "head-44",
        ],
    );

    assert_eq!(out.code, 0, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["data"]["decision"], "approve");
    assert_eq!(env["data"]["submitted_review"], true);

    let calls = fs::read_to_string(capture).expect("read captured calls");
    assert!(
        calls.contains("repos/acme/widgets/pulls/44/reviews"),
        "{calls}"
    );
    assert!(calls.contains("event=APPROVE"), "{calls}");
    assert!(
        !calls.contains("body="),
        "a body-less approve must not send a body field: {calls}"
    );
}

#[test]
fn pr_review_submit_native_approve_422_is_actionable() {
    // GitHub can reject native approval for identities that can comment but are
    // not eligible reviewers, including GitHub App bot identities. Surface that
    // as a typed native-review failure instead of an opaque backend_error.
    let stub = StubEnv::new();
    let capture = stub.tempdir.path().join("gh-args.log");
    let stub = stub.gh_stub(&github_review_submit_approval_422_stub(
        &capture.to_string_lossy(),
    ));

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
            "review",
            "44",
            "--decision",
            "approve",
            "--submit-review",
            "--expected-head",
            "head-44",
            "--comment",
            "Looks good.",
        ],
    );

    assert_eq!(out.code, 1, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["schema_version"], "cli.forge-cli.error.v1");
    assert_eq!(env["ok"], false);
    assert_eq!(env["error"]["code"], "github_native_review_rejected");
    assert!(
        env["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("native approve review"),
        "{}",
        env["error"]["message"]
    );
    let detail = env["error"]["details"]["detail"]
        .as_str()
        .expect("detail is preserved");
    assert!(detail.contains("HTTP 422"), "{detail}");
    assert!(detail.contains("GitHub App bot"), "{detail}");
    assert!(detail.contains("omit --submit-review"), "{detail}");
    assert!(detail.contains("switch reviewer identity"), "{detail}");
    assert!(detail.contains("raw_backend_error_detail="), "{detail}");
    assert!(
        detail.contains("Only users with explicit access can approve pull requests"),
        "{detail}"
    );

    let calls = fs::read_to_string(capture).expect("read captured calls");
    assert!(
        calls.contains("repos/acme/widgets/pulls/44/reviews"),
        "{calls}"
    );
    assert!(
        !calls.contains("issues/44/comments"),
        "typed failure must not silently post a fallback comment: {calls}"
    );
}

#[test]
fn pr_review_submit_review_rejects_gitlab() {
    // Native review submission is GitHub-only in v1; GitLab must surface
    // provider_unsupported (USAGE 64) before touching any backend.
    let stub = StubEnv::new().glab_stub("#!/bin/sh\necho should-not-run >&2\nexit 99\n");

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
            "review",
            "44",
            "--decision",
            "approve",
            "--submit-review",
            "--comment",
            "looks good",
        ],
    );

    assert_eq!(out.code, 64, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["schema_version"], "cli.forge-cli.error.v1");
    assert_eq!(env["ok"], false);
    assert_eq!(env["error"]["code"], "provider_unsupported");
}

#[test]
fn pr_review_submit_native_dry_run_renders_review_submit() {
    // dry-run must render the native review-submit POST (not the issue-comment
    // post) and still include the GitHub PR-existence guard read.
    let stub = StubEnv::new().gh_stub("#!/bin/sh\necho should-not-run >&2\nexit 99\n");

    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "--repo",
            "acme/widgets",
            "--format",
            "json",
            "--dry-run",
            "pr",
            "review",
            "44",
            "--decision",
            "request-changes",
            "--submit-review",
            "--expected-head",
            "head-44",
            "--comment",
            "Status: needs work",
        ],
    );

    assert_eq!(out.code, 0, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["schema_version"], "cli.forge-cli.pr.review.v1");
    assert_eq!(env["ok"], true);
    assert_eq!(env["data"]["submitted_review"], true);
    let plan = env["data"]["plan"]
        .as_array()
        .expect("plan present")
        .iter()
        .map(|v| v.as_str().unwrap_or_default())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        plan.contains("repos/acme/widgets/pulls/44/reviews"),
        "plan should render the reviews POST: {plan}"
    );
    assert!(
        plan.contains("event=REQUEST_CHANGES"),
        "plan should render the mapped review event: {plan}"
    );
    let guard_plan = env["data"]["guard_plan"]
        .as_array()
        .expect("guard_plan present")
        .iter()
        .map(|v| v.as_str().unwrap_or_default())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        guard_plan.contains("repos/acme/widgets/pulls/44"),
        "guard_plan should render the PR-existence lookup: {guard_plan}"
    );
    let pending_guard_plan = env["data"]["pending_review_guard_plan"]
        .as_array()
        .expect("pending_review_guard_plan present")
        .iter()
        .map(|v| v.as_str().unwrap_or_default())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        pending_guard_plan.contains("states: [PENDING]"),
        "pending_review_guard_plan should render the pending-only snapshot: {pending_guard_plan}"
    );
}
