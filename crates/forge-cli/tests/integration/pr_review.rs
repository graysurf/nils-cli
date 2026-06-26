//! End-to-end `pr review` integration tests. The command is intentionally a
//! provider posting primitive: it posts an already-rendered review outcome and
//! optionally mirrors a compact activity note to an issue.

use std::fs;

use pretty_assertions::assert_eq;

use super::support::{StubEnv, parse_envelope, run_forge_cli};

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
        capture = capture.to_string_lossy()
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
        capture = capture.to_string_lossy()
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

fn github_review_thread_submit_stub(capture: &str) -> String {
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
      *"repository(owner:"*)
        printf '%s\n' '{{"data":{{"repository":{{"pullRequest":{{"id":"PR_kwDOabc","url":"https://github.com/acme/widgets/pull/44"}}}}}}}}'
        ;;
      *"addPullRequestReview(input:"*)
        printf '%s\n' '{{"data":{{"addPullRequestReview":{{"pullRequestReview":{{"id":"PRR_kwDOpending","url":"https://github.com/acme/widgets/pull/44#pullrequestreview-9900"}}}}}}}}'
        ;;
      *"addPullRequestReviewThread(input:"*)
        printf '%s\n' '{{"data":{{"addPullRequestReviewThread":{{"thread":{{"id":"PRRT_kwDOthread","path":"src/lib.rs","line":42,"subjectType":"LINE","comments":{{"nodes":[{{"url":"https://github.com/acme/widgets/pull/44#discussion_r42"}}]}}}}}}}}}}'
        ;;
      *"submitPullRequestReview(input:"*)
        printf '%s\n' '{{"data":{{"submitPullRequestReview":{{"pullRequestReview":{{"url":"https://github.com/acme/widgets/pull/44#pullrequestreview-9900"}}}}}}}}'
        ;;
      *)
        echo "stub: unexpected graphql payload: $*" >&2
        exit 99
        ;;
    esac
    ;;
  *)
    echo "stub: unexpected gh api endpoint: $2" >&2
    exit 99
    ;;
esac
"#
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
        calls.contains("addPullRequestReview(input:"),
        "pending review mutation missing: {calls}"
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
        calls.contains("body=Add coverage for the rejected profile URL path."),
        "{calls}"
    );
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
}
