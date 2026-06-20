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

// Older glab: `mr note create --help` does NOT list `--resolvable`, so the live
// path must fall back to the bare `mr note <id>` form rather than passing an
// unknown flag.
fn gitlab_older_glab_review_stub(capture: &str) -> String {
    format!(
        r#"#!/bin/sh
set -eu
printf '%s\n' "$*" >> {capture:?}
case "$*" in
  *"mr note create"*"--help"*)
    echo "  -m --message    Comment or note message."
    echo "  --unique        Don't create a note if a note with the same body already exists."
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
fn pr_review_gitlab_falls_back_when_resolvable_unsupported() {
    // On a glab build whose `mr note create` lacks `--resolvable`, the command
    // must fall back to the bare `mr note <id>` form instead of passing an
    // unknown flag that would fail every GitLab `pr review`.
    let stub = StubEnv::new();
    let capture = stub.tempdir.path().join("glab-args.log");
    let stub = stub.glab_stub(&gitlab_older_glab_review_stub(&capture.to_string_lossy()));

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
        "should probe glab --resolvable support: {calls}"
    );
    assert!(
        calls.contains("mr note 44"),
        "should post via the bare `mr note <id>` fallback: {calls}"
    );
    assert!(
        !calls.contains("mr note create 44"),
        "must not post via the create form when --resolvable is unsupported: {calls}"
    );
    assert!(
        !calls.contains("--resolvable"),
        "must not pass --resolvable on a glab build that lacks it: {calls}"
    );
}
