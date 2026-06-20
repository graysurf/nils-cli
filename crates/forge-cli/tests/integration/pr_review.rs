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

fn gitlab_review_stub(capture: &str) -> String {
    format!(
        r#"#!/bin/sh
set -eu
printf '%s\n' "$*" >> {capture:?}
case "$1 $2" in
  "mr note")
    echo "https://gitlab.com/acme/widgets/-/merge_requests/44#note_440"
    ;;
  "issue note")
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
    assert!(
        calls.contains("repos/acme/widgets/issues/44/comments"),
        "PR comment call missing: {calls}"
    );
    assert!(
        calls.contains("repos/acme/widgets/issues/101/comments"),
        "issue mirror call missing: {calls}"
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
    assert!(calls.contains("mr note 44"), "{calls}");
    assert!(calls.contains("issue note 101"), "{calls}");
    assert!(calls.contains("--repo acme/widgets"), "{calls}");
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
