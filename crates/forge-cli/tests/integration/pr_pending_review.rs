//! Recovery tests for an authenticated actor's provider-valid pending review.

use std::fs;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus};
use std::thread;
use std::time::Duration;

use forge_cli::ops::review_state::{
    ReviewCommentManifestItem, ReviewRunReceipt, ReviewStatePayload, ReviewStateRecord,
    compute_review_run_id, sha256_digest,
};
use pretty_assertions::assert_eq;

use super::support::{
    CmdOutput, StubEnv, forge_cli_bin, parse_envelope, run_forge_cli, run_forge_cli_with_stdin,
};

const INCOMPLETE_RECEIPT_RUN_ID: &str =
    "sha256:3de5f335496edb76e29ba96b787fbc9dc18d081f45a285b0e2ae877443f49405";

struct LeaseTestChild {
    child: Option<Child>,
    release: PathBuf,
}

impl LeaseTestChild {
    fn new(child: Child, release: PathBuf) -> Self {
        Self {
            child: Some(child),
            release,
        }
    }

    fn release_and_wait(mut self) -> ExitStatus {
        fs::write(&self.release, "release\n").expect("release first submit");
        let status = self
            .child
            .as_mut()
            .expect("lease child")
            .wait()
            .expect("wait for first submit");
        self.child.take();
        status
    }
}

impl Drop for LeaseTestChild {
    fn drop(&mut self) {
        let _ = fs::write(&self.release, "release\n");
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn incomplete_receipt_marker() -> String {
    let receipt = ReviewRunReceipt {
        review_run_id: INCOMPLETE_RECEIPT_RUN_ID.to_string(),
        route_lenses: Vec::new(),
        decision: "comments-only".to_string(),
        expected_head: "head-new".to_string(),
        round: 0,
        summary_digest: "sha256:summary".to_string(),
        inline_manifest: Vec::new(),
    };
    assert_eq!(
        compute_review_run_id(
            "acme/widgets",
            42,
            &receipt.expected_head,
            receipt.round,
            &receipt.route_lenses,
            &receipt.decision,
            &receipt.summary_digest,
            &receipt.inline_manifest,
        )
        .expect("review run id"),
        receipt.review_run_id
    );
    ReviewStateRecord::new(
        "acme/widgets",
        42,
        "head-new",
        0,
        None,
        ReviewStatePayload::ReviewRunReceipt { receipt },
    )
    .expect("review-state record")
    .marker()
    .expect("review-state marker")
}

fn receipt_marker(
    summary: &str,
    inline_manifest: Vec<ReviewCommentManifestItem>,
) -> (String, String) {
    let summary_digest = sha256_digest(summary.as_bytes());
    let review_run_id = compute_review_run_id(
        "acme/widgets",
        42,
        "head-new",
        0,
        &[],
        "comments-only",
        &summary_digest,
        &inline_manifest,
    )
    .expect("review run id");
    let receipt = ReviewRunReceipt {
        review_run_id: review_run_id.clone(),
        route_lenses: Vec::new(),
        decision: "comments-only".to_string(),
        expected_head: "head-new".to_string(),
        round: 0,
        summary_digest,
        inline_manifest,
    };
    let marker = ReviewStateRecord::new(
        "acme/widgets",
        42,
        "head-new",
        0,
        None,
        ReviewStatePayload::ReviewRunReceipt { receipt },
    )
    .expect("review-state record")
    .marker()
    .expect("review-state marker");
    (review_run_id, marker)
}

#[test]
fn pr_pending_review_catalog_uses_provider_native_viewer_guards() {
    let catalog = include_str!("../../docs/specs/forge-cli-ops-v1.yaml");
    for operation in [
        "pr.pending-review.inspect",
        "pr.pending-review.resume-submit",
        "pr.pending-review.submit",
        "pr.pending-review.discard",
        "pr.pending-review.delete",
    ] {
        assert!(
            catalog.contains(&format!("  - id: {operation}\n")),
            "missing operation {operation}"
        );
    }
    assert!(catalog.contains("schema: forge-cli.review-loop.v1"));
    assert!(catalog.contains("privacy_forbidden:"));
    let resume_submit = catalog
        .split_once("  - id: pr.pending-review.resume-submit\n")
        .expect("resume-submit operation")
        .1
        .split_once("  - id: pr.pending-review.submit\n")
        .expect("submit follows resume-submit")
        .0;
    assert!(
        resume_submit.contains("schema_version: cli.forge-cli.pr.pending-review.resume-submit.v2")
    );
    assert!(resume_submit.contains("snapshot_digest?"));
    assert!(resume_submit.contains("snapshot_provenance"));
    let direct_submit = catalog
        .split_once("  - id: pr.pending-review.submit\n")
        .expect("direct-submit operation")
        .1
        .split_once("  - id: pr.pending-review.discard\n")
        .expect("discard follows direct-submit")
        .0;
    assert!(direct_submit.contains("schema_version: cli.forge-cli.pr.pending-review.submit.v1"));
    assert!(direct_submit.contains("commit_sha,snapshot_digest,snapshot_provenance"));
    assert!(!direct_submit.contains("snapshot_digest?"));
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

fn pending_recovery_snapshot(marked: bool, inline_count: usize, viewer_owned: bool) -> String {
    let review_body = if marked {
        format!("Summary\n<!-- forge-cli:review-run:v1 run={INCOMPLETE_RECEIPT_RUN_ID} -->")
    } else {
        "Unmarked summary".to_string()
    };
    let comments = (0..inline_count)
        .map(|index| {
            let line = index + 1;
            let semantic = if marked {
                "first".to_string()
            } else {
                format!("unmarked finding {index}")
            };
            let body = if marked {
                format!(
                    "{semantic}\n<!-- forge-cli:review-finding:v1 run={INCOMPLETE_RECEIPT_RUN_ID} digest=sha256:a7937b64b8caa58f03721bb6bacf5c78cb235febe0e70b1b84cd99541461a08e -->"
                )
            } else {
                semantic
            };
            serde_json::json!({
                "id": format!("PRRC_{index}"),
                "url": format!("https://github.com/acme/widgets/pull/42#discussion_r{index}"),
                "author": {"login": "review-bot"},
                "body": body,
                "createdAt": format!("2026-07-20T12:00:{index:02}Z"),
                "path": format!("src/file-{index}.rs"),
                "diffHunk": format!("@@ -{line},0 +{line},1 @@\n+added line"),
                "line": line,
                "originalLine": line,
                "startLine": null,
                "originalStartLine": null,
                "subjectType": "LINE"
            })
        })
        .collect::<Vec<_>>();
    serde_json::json!({
        "data": {
            "node": {
                "id": "PRR_pending",
                "url": "https://github.com/acme/widgets/pull/42#pullrequestreview-102",
                "author": {"login": "review-bot"},
                "state": "PENDING",
                "commit": {"oid": "head-new"},
                "body": review_body,
                "viewerDidAuthor": viewer_owned,
                "viewerCanDelete": viewer_owned,
                "comments": {
                    "totalCount": inline_count,
                    "nodes": comments,
                    "pageInfo": {"hasNextPage": false, "endCursor": null}
                },
                "pullRequest": {
                    "number": 42,
                    "url": "https://github.com/acme/widgets/pull/42",
                    "headRefOid": "head-new"
                }
            }
        }
    })
    .to_string()
}

fn pending_recovery_script(capture: &str, snapshot: &str, marker: &str) -> String {
    let state_marker = serde_json::to_string(marker).expect("serialize review-state marker");
    let mut submitted: serde_json::Value =
        serde_json::from_str(snapshot).expect("pending snapshot fixture");
    submitted["data"]["node"]["state"] = "COMMENTED".into();
    submitted["data"]["node"]["viewerCanDelete"] = false.into();
    let submitted = submitted.to_string();
    let submitted_flag = format!("{capture}.submitted");
    let deleted_flag = format!("{capture}.deleted");
    r#"#!/bin/sh
set -eu
printf '%s\n' "$*" >> "@CAPTURE@"
case "$1 $2" in
  "pr view")
    printf '%s\n' '{"number":42,"url":"https://github.com/acme/widgets/pull/42","state":"OPEN","isDraft":false,"baseRefName":"main","headRefName":"feat/reviews","headRefOid":"head-new","title":"feat: reviews","body":""}'
    ;;
  "api graphql")
    case "$*" in
      *"pullRequest(number: \$pr) { comments(first: 100"*)
        printf '%s\n' '{"data":{"viewer":{"login":"review-bot"},"repository":{"pullRequest":{"comments":{"nodes":[{"author":{"login":"review-bot"},"body":@STATE_MARKER@}],"pageInfo":{"hasNextPage":false,"endCursor":"state-tip"}}}}}}'
        ;;
      *"submitPullRequestReview(input:"*)
        : > "@SUBMITTED_FLAG@"
        printf '%s\n' '{"data":{"submitPullRequestReview":{"pullRequestReview":{"url":"https://github.com/acme/widgets/pull/42#pullrequestreview-102"}}}}'
        ;;
      *"deletePullRequestReview(input:"*)
        : > "@DELETED_FLAG@"
        printf '%s\n' '{"data":{"deletePullRequestReview":{"pullRequestReview":{"id":"PRR_pending","url":"https://github.com/acme/widgets/pull/42#pullrequestreview-102"}}}}'
        ;;
      *"comments(first: 100"*)
        if [ -e "@DELETED_FLAG@" ]; then
          printf '%s\n' '{"data":{"node":null}}'
        elif [ -e "@SUBMITTED_FLAG@" ]; then
          printf '%s\n' '@SUBMITTED_SNAPSHOT@'
        else
          printf '%s\n' '@SNAPSHOT@'
        fi
        ;;
      *)
        echo "unexpected graphql args: $*" >&2
        exit 99
        ;;
    esac
    ;;
  *)
    echo "unexpected gh args: $*" >&2
    exit 99
    ;;
esac
"#
    .replace("@CAPTURE@", capture)
    .replace("@SNAPSHOT@", snapshot)
    .replace("@SUBMITTED_SNAPSHOT@", &submitted)
    .replace("@SUBMITTED_FLAG@", &submitted_flag)
    .replace("@DELETED_FLAG@", &deleted_flag)
    .replace("@STATE_MARKER@", &state_marker)
}

fn two_page_pending_snapshot_script(capture: &str, second_body: &str) -> String {
    let page = |id: &str, body: &str, has_next_page: bool, end_cursor: Option<&str>| {
        serde_json::json!({
            "data": {"node": {
                "id": "PRR_pending",
                "url": "https://github.com/acme/widgets/pull/42#pullrequestreview-102",
                "author": {"login": "review-bot"},
                "state": "PENDING",
                "commit": {"oid": "head-new"},
                "body": "Summary",
                "viewerDidAuthor": true,
                "viewerCanDelete": true,
                "comments": {
                    "totalCount": 2,
                    "nodes": [{
                        "id": id,
                        "url": format!("https://github.com/acme/widgets/pull/42#discussion_{id}"),
                        "author": {"login": "review-bot"},
                        "body": body,
                        "createdAt": "2026-07-20T12:00:00Z",
                        "path": format!("src/{id}.rs"),
                        "line": 10,
                        "originalLine": 10,
                        "diffSide": "RIGHT",
                        "startLine": null,
                        "originalStartLine": null,
                        "startDiffSide": null,
                        "subjectType": "LINE"
                    }],
                    "pageInfo": {
                        "hasNextPage": has_next_page,
                        "endCursor": end_cursor
                    }
                },
                "pullRequest": {
                    "number": 42,
                    "url": "https://github.com/acme/widgets/pull/42",
                    "headRefOid": "head-new"
                }
            }}
        })
        .to_string()
    };
    let first_page = page("PRRC_1", "first-page finding", true, Some("cursor-1"));
    let second_page = page("PRRC_2", second_body, false, None);
    r#"#!/bin/sh
set -eu
printf '%s\n' "$*" >> "@CAPTURE@"
case "$1 $2" in
  "pr view")
    printf '%s\n' '{"number":42,"url":"https://github.com/acme/widgets/pull/42","state":"OPEN","isDraft":false,"baseRefName":"main","headRefName":"feat/reviews","headRefOid":"head-new","title":"feat: reviews","body":""}'
    ;;
  "api graphql")
    case "$*" in
      *"after=cursor-1"*)
        printf '%s\n' '@SECOND_PAGE@'
        ;;
      *)
        printf '%s\n' '@FIRST_PAGE@'
        ;;
    esac
    ;;
  *)
    echo "unexpected gh args: $*" >&2
    exit 99
    ;;
esac
"#
    .replace("@CAPTURE@", capture)
    .replace("@FIRST_PAGE@", &first_page)
    .replace("@SECOND_PAGE@", &second_page)
}

#[test]
fn pr_pending_review_inspect_digests_later_comment_pages() {
    let mut digests = Vec::new();
    for second_body in ["second-page finding A", "second-page finding B"] {
        let stub = StubEnv::new();
        let capture = stub.tempdir.path().join("inspect-two-page-calls.log");
        let script = two_page_pending_snapshot_script(&capture.to_string_lossy(), second_body);
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
                "inspect",
                "42",
                "--review",
                "PRR_pending",
            ],
        );

        assert_eq!(out.code, 0, "stdout={}\nstderr={}", out.stdout, out.stderr);
        let envelope = parse_envelope(&out.stdout);
        assert_eq!(
            envelope["data"]["snapshot"]["inline_comments"]
                .as_array()
                .expect("inline comment manifest")
                .len(),
            2
        );
        digests.push(
            envelope["data"]["snapshot"]["snapshot_digest"]
                .as_str()
                .expect("snapshot digest")
                .to_string(),
        );
        let calls = fs::read_to_string(capture).expect("read gh calls");
        assert!(calls.contains("after=cursor-1"), "{calls}");
    }

    assert_ne!(
        digests[0], digests[1],
        "changing only a later-page inline comment must change the snapshot digest"
    );
}

#[test]
fn pr_pending_review_inspect_returns_a_complete_receipt_aware_snapshot() {
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
    printf '%s\n' '{{"data":{{"node":{{"id":"PRR_pending","url":"https://github.com/acme/widgets/pull/42#pullrequestreview-102","author":{{"login":"review-bot"}},"state":"PENDING","commit":{{"oid":"head-new"}},"body":"Summary\n<!-- forge-cli:review-run:v1 run=run-123 -->","viewerDidAuthor":true,"viewerCanDelete":true,"comments":{{"totalCount":1,"nodes":[{{"id":"PRRC_1","url":"https://github.com/acme/widgets/pull/42#discussion_r1","author":{{"login":"review-bot"}},"body":"first\n<!-- forge-cli:review-finding:v1 run=run-123 digest=sha256:a7937b64b8caa58f03721bb6bacf5c78cb235febe0e70b1b84cd99541461a08e -->","createdAt":"2026-07-20T12:00:00Z","path":"src/lib.rs","diffHunk":"@@ -10,0 +10,1 @@\n+new","line":10,"originalLine":10,"startLine":null,"originalStartLine":null,"subjectType":"LINE"}}],"pageInfo":{{"hasNextPage":false,"endCursor":null}}}},"pullRequest":{{"number":42,"url":"https://github.com/acme/widgets/pull/42","headRefOid":"head-new"}}}}}}}}'
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
            "inspect",
            "42",
            "--review",
            "PRR_pending",
        ],
    );

    assert_eq!(out.code, 0, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(
        env["schema_version"],
        "cli.forge-cli.pr.pending-review.inspect.v1"
    );
    assert_eq!(env["data"]["snapshot"]["review_id"], "PRR_pending");
    assert_eq!(env["data"]["snapshot"]["review_run_id"], "run-123");
    assert_eq!(env["data"]["snapshot"]["provenance"], "receipt-bound");
    assert_eq!(
        env["data"]["snapshot"]["inline_comments"]
            .as_array()
            .expect("inline comment manifest")
            .len(),
        1
    );
    assert_eq!(
        env["data"]["snapshot"]["inline_comments"][0]["diff_side"],
        "RIGHT"
    );
    assert!(
        env["data"]["snapshot"]["snapshot_digest"]
            .as_str()
            .is_some_and(|digest| digest.starts_with("sha256:"))
    );
}

#[test]
fn pr_pending_review_resume_submit_rejects_an_incomplete_receipt_manifest() {
    let stub = StubEnv::new();
    let capture = stub.tempdir.path().join("resume-submit-calls.log");
    let snapshot = pending_recovery_snapshot(true, 1, true);
    let script = pending_recovery_script(
        &capture.to_string_lossy(),
        &snapshot,
        &incomplete_receipt_marker(),
    );
    let stub = stub.gh_stub(&script);

    let inspect = run_forge_cli(
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
            "inspect",
            "42",
            "--review",
            "PRR_pending",
        ],
    );
    assert_eq!(inspect.code, 0, "{}", inspect.stdout);
    let inspect_envelope = parse_envelope(&inspect.stdout);
    let digest = inspect_envelope["data"]["snapshot"]["snapshot_digest"]
        .as_str()
        .expect("snapshot digest")
        .to_string();

    let output = run_forge_cli(
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
            "resume-submit",
            "42",
            "--review",
            "PRR_pending",
            "--review-run-id",
            INCOMPLETE_RECEIPT_RUN_ID,
            "--expected-head",
            "head-new",
            "--expected-commit",
            "head-new",
            "--expected-snapshot",
            &digest,
            "--decision",
            "comments-only",
        ],
    );

    assert_ne!(
        output.code, 0,
        "stdout={}\nstderr={}",
        output.stdout, output.stderr
    );
    let envelope = parse_envelope(&output.stdout);
    assert_eq!(envelope["schema_version"], "cli.forge-cli.error.v1");
    assert_eq!(
        envelope["error"]["code"], "pending_review_manifest_mismatch",
        "{}",
        output.stdout
    );
    let calls = fs::read_to_string(capture).expect("read calls");
    assert!(!calls.contains("submitPullRequestReview(input:"), "{calls}");
    assert!(!calls.contains("deletePullRequestReview(input:"), "{calls}");
}

#[test]
fn pr_pending_review_resume_submit_accepts_an_exact_receipt_manifest() {
    let stub = StubEnv::new();
    let capture = stub.tempdir.path().join("valid-resume-submit-calls.log");
    let manifest = vec![ReviewCommentManifestItem {
        index: 0,
        path: "src/file-0.rs".to_string(),
        line: Some(1),
        side: "RIGHT".to_string(),
        start_line: None,
        start_side: None,
        subject_type: "LINE".to_string(),
        body_digest: sha256_digest(b"first"),
    }];
    let (review_run_id, marker) = receipt_marker("Summary", manifest);
    let snapshot =
        pending_recovery_snapshot(true, 1, true).replace(INCOMPLETE_RECEIPT_RUN_ID, &review_run_id);
    let script = pending_recovery_script(&capture.to_string_lossy(), &snapshot, &marker);
    let stub = stub.gh_stub(&script);

    let inspect = run_forge_cli(
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
            "inspect",
            "42",
            "--review",
            "PRR_pending",
        ],
    );
    assert_eq!(inspect.code, 0, "{}", inspect.stdout);
    let digest = parse_envelope(&inspect.stdout)["data"]["snapshot"]["snapshot_digest"]
        .as_str()
        .expect("snapshot digest")
        .to_string();
    let output = run_forge_cli(
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
            "resume-submit",
            "42",
            "--review",
            "PRR_pending",
            "--review-run-id",
            &review_run_id,
            "--expected-head",
            "head-new",
            "--expected-commit",
            "head-new",
            "--expected-snapshot",
            &digest,
            "--decision",
            "comments-only",
        ],
    );

    assert_eq!(output.code, 0, "{}", output.stdout);
    let envelope = parse_envelope(&output.stdout);
    assert_eq!(
        envelope["schema_version"],
        "cli.forge-cli.pr.pending-review.resume-submit.v2"
    );
    assert_eq!(envelope["data"]["submitted"], true);
    assert_eq!(envelope["data"]["review_run_id"], review_run_id);
    assert_eq!(
        envelope["data"]["snapshot_provenance"],
        "pending-cas+submitted-reconciled"
    );
    let calls = fs::read_to_string(capture).expect("read calls");
    assert_eq!(calls.matches("submitPullRequestReview(input:").count(), 1);
    assert!(!calls.contains("deletePullRequestReview(input:"), "{calls}");
}

#[test]
fn pr_pending_review_direct_submit_v1_remains_deserializable_with_a_required_digest() {
    #[derive(serde::Deserialize)]
    struct PriorV1Envelope {
        schema_version: String,
        data: PriorV1SubmitPayload,
    }

    #[derive(serde::Deserialize)]
    struct PriorV1SubmitPayload {
        snapshot_digest: String,
    }

    let stub = StubEnv::new();
    let capture = stub.tempdir.path().join("direct-submit-v1-calls.log");
    let snapshot = pending_recovery_snapshot(false, 1, true);
    let script = pending_recovery_script(&capture.to_string_lossy(), &snapshot, "unused");
    let stub = stub.gh_stub(&script);
    let inspect = run_forge_cli(
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
            "inspect",
            "42",
            "--review",
            "PRR_pending",
        ],
    );
    assert_eq!(inspect.code, 0, "{}", inspect.stdout);
    let digest = parse_envelope(&inspect.stdout)["data"]["snapshot"]["snapshot_digest"]
        .as_str()
        .expect("snapshot digest")
        .to_string();

    let output = run_forge_cli(
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
            "submit",
            "42",
            "--review",
            "PRR_pending",
            "--expected-head",
            "head-new",
            "--expected-commit",
            "head-new",
            "--expected-snapshot",
            &digest,
            "--decision",
            "comments-only",
            "--confirm-unmarked-submit",
        ],
    );

    assert_eq!(output.code, 0, "{}", output.stdout);
    let prior: PriorV1Envelope =
        serde_json::from_str(&output.stdout).expect("prior v1 consumer shape");
    assert_eq!(
        prior.schema_version,
        "cli.forge-cli.pr.pending-review.submit.v1"
    );
    assert_eq!(prior.data.snapshot_digest, digest);
}

#[test]
fn pr_pending_review_submit_fails_when_provider_state_cannot_be_reconciled() {
    let stub = StubEnv::new();
    let capture = stub.tempdir.path().join("unreconciled-submit-calls.log");
    let manifest = vec![ReviewCommentManifestItem {
        index: 0,
        path: "src/file-0.rs".to_string(),
        line: Some(1),
        side: "RIGHT".to_string(),
        start_line: None,
        start_side: None,
        subject_type: "LINE".to_string(),
        body_digest: sha256_digest(b"first"),
    }];
    let (review_run_id, marker) = receipt_marker("Summary", manifest);
    let snapshot =
        pending_recovery_snapshot(true, 1, true).replace(INCOMPLETE_RECEIPT_RUN_ID, &review_run_id);
    let script = pending_recovery_script(&capture.to_string_lossy(), &snapshot, &marker).replace(
        &format!(": > \"{}.submitted\"", capture.to_string_lossy()),
        ":",
    );
    let stub = stub.gh_stub(&script);
    let inspect = run_forge_cli(
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
            "inspect",
            "42",
            "--review",
            "PRR_pending",
        ],
    );
    let digest = parse_envelope(&inspect.stdout)["data"]["snapshot"]["snapshot_digest"]
        .as_str()
        .expect("snapshot digest")
        .to_string();

    let output = run_forge_cli(
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
            "resume-submit",
            "42",
            "--review",
            "PRR_pending",
            "--review-run-id",
            &review_run_id,
            "--expected-head",
            "head-new",
            "--expected-commit",
            "head-new",
            "--expected-snapshot",
            &digest,
            "--decision",
            "comments-only",
        ],
    );

    assert_eq!(
        output.code, 65,
        "stdout={}\nstderr={}",
        output.stdout, output.stderr
    );
    assert_eq!(
        parse_envelope(&output.stdout)["error"]["code"],
        "pending_review_reconciliation_failed"
    );
    let calls = fs::read_to_string(capture).expect("read calls");
    assert_eq!(calls.matches("submitPullRequestReview(input:").count(), 1);
}

#[test]
fn pr_pending_review_cross_process_lease_excludes_a_second_mutation() {
    let stub = StubEnv::new();
    let capture = stub.tempdir.path().join("concurrent-submit-calls.log");
    let started = stub.tempdir.path().join("submit-started.flag");
    let release = stub.tempdir.path().join("submit-release.flag");
    let manifest = vec![ReviewCommentManifestItem {
        index: 0,
        path: "src/file-0.rs".to_string(),
        line: Some(1),
        side: "RIGHT".to_string(),
        start_line: None,
        start_side: None,
        subject_type: "LINE".to_string(),
        body_digest: sha256_digest(b"first"),
    }];
    let (review_run_id, marker) = receipt_marker("Summary", manifest);
    let snapshot =
        pending_recovery_snapshot(true, 1, true).replace(INCOMPLETE_RECEIPT_RUN_ID, &review_run_id);
    let submitted_flag = format!("{}.submitted", capture.to_string_lossy());
    let transition = format!(
        ": > {started:?}\n        while [ ! -e {release:?} ]; do sleep 0.01; done\n        : > \"{submitted_flag}\""
    );
    let script = pending_recovery_script(&capture.to_string_lossy(), &snapshot, &marker)
        .replace(&format!(": > \"{submitted_flag}\""), &transition);
    let stub = stub.gh_stub(&script);
    let inspect = run_forge_cli(
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
            "inspect",
            "42",
            "--review",
            "PRR_pending",
        ],
    );
    let digest = parse_envelope(&inspect.stdout)["data"]["snapshot"]["snapshot_digest"]
        .as_str()
        .expect("snapshot digest")
        .to_string();
    let args = [
        "--provider",
        "github",
        "--repo",
        "acme/widgets",
        "--format",
        "json",
        "pr",
        "pending-review",
        "resume-submit",
        "42",
        "--review",
        "PRR_pending",
        "--review-run-id",
        &review_run_id,
        "--expected-head",
        "head-new",
        "--expected-commit",
        "head-new",
        "--expected-snapshot",
        &digest,
        "--decision",
        "comments-only",
    ];
    let mut first = Command::new(forge_cli_bin());
    first
        .args(args)
        .env("XDG_CONFIG_HOME", stub.tempdir.path().join("xdg-config"))
        .env("XDG_STATE_HOME", stub.tempdir.path().join("xdg-state"))
        .env("FORGE_CLI_RATE_LIMIT_GATE", "off")
        .current_dir(stub.tempdir.path());
    for (key, value) in &stub.envs {
        first.env(key, value);
    }
    let first = LeaseTestChild::new(first.spawn().expect("spawn first submit"), release.clone());
    for _ in 0..500 {
        if started.exists() {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert!(started.exists(), "first submit did not reach the mutation");

    let second = run_forge_cli(&stub, &args);
    assert_eq!(second.code, 69, "{}", second.stdout);
    assert_eq!(
        parse_envelope(&second.stdout)["error"]["code"],
        "pending_review_lease_busy"
    );
    let calls = fs::read_to_string(&capture).expect("read calls");
    assert_eq!(calls.matches("submitPullRequestReview(input:").count(), 1);

    let status = first.release_and_wait();
    assert!(status.success(), "first submit exited with {status}");
}

#[test]
fn pr_pending_review_resume_submit_accepts_provider_null_file_anchors() {
    let stub = StubEnv::new();
    let capture = stub
        .tempdir
        .path()
        .join("file-anchor-resume-submit-calls.log");
    let bodies = [
        "first file finding",
        "second file finding",
        "third file finding",
    ];
    let manifest = bodies
        .iter()
        .enumerate()
        .map(|(index, body)| ReviewCommentManifestItem {
            index,
            path: format!("src/file-{index}.rs"),
            line: None,
            side: "RIGHT".to_string(),
            start_line: None,
            start_side: None,
            subject_type: "FILE".to_string(),
            body_digest: sha256_digest(body.as_bytes()),
        })
        .collect::<Vec<_>>();
    let (review_run_id, marker) = receipt_marker("Summary", manifest);
    let comments = bodies
        .iter()
        .enumerate()
        .map(|(index, body)| serde_json::json!({
            "id": format!("PRRC_{index}"),
            "url": format!("https://github.com/acme/widgets/pull/42#discussion_file_{index}"),
            "author": {"login": "review-bot"},
            "body": format!("{body}\n<!-- forge-cli:review-finding:v1 run={review_run_id} digest={} -->", sha256_digest(body.as_bytes())),
            "createdAt": format!("2026-07-20T12:00:{index:02}Z"),
            "path": format!("src/file-{index}.rs"),
            "diffHunk": "",
            "line": null,
            "originalLine": null,
            "startLine": null,
            "originalStartLine": null,
            "subjectType": "FILE"
        }))
        .collect::<Vec<_>>();
    let snapshot = serde_json::json!({
        "data": {"node": {
            "id": "PRR_pending",
            "url": "https://github.com/acme/widgets/pull/42#pullrequestreview-102",
            "author": {"login": "review-bot"},
            "state": "PENDING",
            "commit": {"oid": "head-new"},
            "body": format!("Summary\n<!-- forge-cli:review-run:v1 run={review_run_id} -->"),
            "viewerDidAuthor": true,
            "viewerCanDelete": true,
            "comments": {
                "totalCount": 3,
                "nodes": comments,
                "pageInfo": {"hasNextPage": false, "endCursor": null}
            },
            "pullRequest": {
                "number": 42,
                "url": "https://github.com/acme/widgets/pull/42",
                "headRefOid": "head-new"
            }
        }}
    })
    .to_string();
    let script = pending_recovery_script(&capture.to_string_lossy(), &snapshot, &marker);
    let stub = stub.gh_stub(&script);

    let inspect = run_forge_cli(
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
            "inspect",
            "42",
            "--review",
            "PRR_pending",
        ],
    );
    assert_eq!(inspect.code, 0, "{}", inspect.stdout);
    let digest = parse_envelope(&inspect.stdout)["data"]["snapshot"]["snapshot_digest"]
        .as_str()
        .expect("snapshot digest")
        .to_string();
    let output = run_forge_cli(
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
            "resume-submit",
            "42",
            "--review",
            "PRR_pending",
            "--review-run-id",
            &review_run_id,
            "--expected-head",
            "head-new",
            "--expected-commit",
            "head-new",
            "--expected-snapshot",
            &digest,
            "--decision",
            "comments-only",
        ],
    );

    assert_eq!(
        output.code, 0,
        "stdout={}\nstderr={}",
        output.stdout, output.stderr
    );
    let calls = fs::read_to_string(capture).expect("read calls");
    assert_eq!(calls.matches("submitPullRequestReview(input:").count(), 1);
}

#[test]
fn pr_pending_review_resume_submit_rejects_diff_side_and_range_drift_without_mutation() {
    let stub = StubEnv::new();
    let capture = stub.tempdir.path().join("anchor-drift-calls.log");
    let manifest = vec![ReviewCommentManifestItem {
        index: 0,
        path: "src/file-0.rs".to_string(),
        line: Some(1),
        side: "LEFT".to_string(),
        start_line: Some(1),
        start_side: Some("LEFT".to_string()),
        subject_type: "LINE".to_string(),
        body_digest: sha256_digest(b"first"),
    }];
    let (review_run_id, marker) = receipt_marker("Summary", manifest);
    let snapshot =
        pending_recovery_snapshot(true, 1, true).replace(INCOMPLETE_RECEIPT_RUN_ID, &review_run_id);
    let script = pending_recovery_script(&capture.to_string_lossy(), &snapshot, &marker);
    let stub = stub.gh_stub(&script);
    let inspect = run_forge_cli(
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
            "inspect",
            "42",
            "--review",
            "PRR_pending",
        ],
    );
    let digest = parse_envelope(&inspect.stdout)["data"]["snapshot"]["snapshot_digest"]
        .as_str()
        .expect("snapshot digest")
        .to_string();
    let output = run_forge_cli(
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
            "resume-submit",
            "42",
            "--review",
            "PRR_pending",
            "--review-run-id",
            &review_run_id,
            "--expected-head",
            "head-new",
            "--expected-commit",
            "head-new",
            "--expected-snapshot",
            &digest,
            "--decision",
            "comments-only",
        ],
    );

    assert_eq!(output.code, 65, "{}", output.stdout);
    assert_eq!(
        parse_envelope(&output.stdout)["error"]["code"],
        "pending_review_manifest_mismatch"
    );
    let calls = fs::read_to_string(capture).expect("read calls");
    assert!(!calls.contains("submitPullRequestReview(input:"), "{calls}");
    assert!(!calls.contains("deletePullRequestReview(input:"), "{calls}");
}

#[test]
fn pr_pending_review_resume_submit_is_idempotent_after_submission() {
    let stub = StubEnv::new();
    let capture = stub.tempdir.path().join("already-submitted-calls.log");
    let summary_digest = sha256_digest(b"Summary");
    let review_run_id = compute_review_run_id(
        "acme/widgets",
        7,
        "head-abc",
        0,
        &[],
        "comments-only",
        &summary_digest,
        &[],
    )
    .expect("review run id");
    let receipt = ReviewRunReceipt {
        review_run_id: review_run_id.clone(),
        route_lenses: Vec::new(),
        decision: "comments-only".to_string(),
        expected_head: "head-abc".to_string(),
        round: 0,
        summary_digest,
        inline_manifest: Vec::new(),
    };
    let marker = ReviewStateRecord::new(
        "acme/widgets",
        7,
        "head-abc",
        0,
        None,
        ReviewStatePayload::ReviewRunReceipt { receipt },
    )
    .expect("review-state record")
    .marker()
    .expect("review-state marker");
    let ledger = serde_json::json!([{
        "author": {"login": "review-bot"},
        "body": marker
    }])
    .to_string();
    let script = format!(
        r#"#!/bin/sh
set -eu
printf '%s\n' "$*" >> {capture:?}
case "$1 $2" in
  "pr view")
    printf '%s\n' '{{"number":7,"url":"https://github.com/acme/widgets/pull/7","state":"OPEN","isDraft":false,"baseRefName":"main","headRefName":"feat/reviews","headRefOid":"head-abc","title":"feat: reviews","body":""}}'
    ;;
  "api graphql")
    case "$*" in
      *"pullRequest(number: \$pr) {{ comments(first: 100"*)
        printf '%s\n' '{{"data":{{"viewer":{{"login":"review-bot"}},"repository":{{"pullRequest":{{"comments":{{"nodes":{ledger},"pageInfo":{{"hasNextPage":false,"endCursor":"state-tip"}}}}}}}}}}}}'
        ;;
      *"node(id: \$review)"*)
        printf '%s\n' '{{"data":{{"node":{{"id":"PRR_pending","url":"https://github.com/acme/widgets/pull/7#pullrequestreview-102","author":{{"login":"review-bot"}},"state":"COMMENTED","commit":{{"oid":"head-abc"}},"body":"Summary\n<!-- forge-cli:review-run:v1 run={review_run_id} -->","viewerDidAuthor":true,"viewerCanDelete":false,"comments":{{"totalCount":0,"nodes":[],"pageInfo":{{"hasNextPage":false,"endCursor":null}}}},"pullRequest":{{"number":7,"url":"https://github.com/acme/widgets/pull/7","headRefOid":"head-abc"}}}}}}}}'
        ;;
      *"reviews(first: 100"*)
        printf '%s\n' '{{"data":{{"viewer":{{"login":"review-bot"}},"repository":{{"pullRequest":{{"headRefOid":"head-abc","reviews":{{"nodes":[{{"id":"PRR_pending","databaseId":102,"url":"https://github.com/acme/widgets/pull/7#pullrequestreview-102","author":{{"login":"review-bot"}},"state":"COMMENTED","commit":{{"oid":"head-abc"}},"submittedAt":"2026-07-20T12:05:00Z","body":"Summary\n<!-- forge-cli:review-run:v1 run={review_run_id} -->","viewerDidAuthor":true}}],"pageInfo":{{"hasNextPage":false,"endCursor":null}}}}}}}}}}}}'
        ;;
      *)
        echo "unexpected graphql args: $*" >&2
        exit 99
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

    let output = run_forge_cli(
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
            "resume-submit",
            "7",
            "--review",
            "PRR_pending",
            "--review-run-id",
            &review_run_id,
            "--expected-head",
            "head-abc",
            "--expected-commit",
            "head-abc",
            "--expected-snapshot",
            "sha256:inspected-before-submit",
            "--decision",
            "comments-only",
        ],
    );

    assert_eq!(
        output.code, 0,
        "stdout={}\nstderr={}",
        output.stdout, output.stderr
    );
    let envelope = parse_envelope(&output.stdout);
    assert_eq!(
        envelope["schema_version"],
        "cli.forge-cli.pr.pending-review.resume-submit.v2"
    );
    assert_eq!(envelope["data"]["review_id"], "PRR_pending");
    assert_eq!(envelope["data"]["review_run_id"], review_run_id);
    assert_eq!(envelope["data"]["snapshot_digest"], serde_json::Value::Null);
    assert_eq!(
        envelope["data"]["snapshot_provenance"],
        "pending-snapshot-unverified"
    );
    assert_eq!(envelope["data"]["submitted"], true);
    let calls = fs::read_to_string(capture).expect("read calls");
    assert!(!calls.contains("submitPullRequestReview(input:"), "{calls}");
    assert!(!calls.contains("deletePullRequestReview(input:"), "{calls}");
}

#[test]
fn pr_pending_review_already_submitted_rejects_a_missing_receipt_finding() {
    let stub = StubEnv::new();
    let capture = stub
        .tempdir
        .path()
        .join("already-submitted-missing-finding-calls.log");
    let summary_digest = sha256_digest(b"Summary");
    let manifest = vec![ReviewCommentManifestItem {
        index: 0,
        path: "src/lib.rs".to_string(),
        line: Some(42),
        side: "RIGHT".to_string(),
        start_line: None,
        start_side: None,
        subject_type: "LINE".to_string(),
        body_digest: sha256_digest(b"Required finding"),
    }];
    let review_run_id = compute_review_run_id(
        "acme/widgets",
        7,
        "head-abc",
        0,
        &[],
        "comments-only",
        &summary_digest,
        &manifest,
    )
    .expect("review run id");
    let receipt = ReviewRunReceipt {
        review_run_id: review_run_id.clone(),
        route_lenses: Vec::new(),
        decision: "comments-only".to_string(),
        expected_head: "head-abc".to_string(),
        round: 0,
        summary_digest,
        inline_manifest: manifest,
    };
    let marker = ReviewStateRecord::new(
        "acme/widgets",
        7,
        "head-abc",
        0,
        None,
        ReviewStatePayload::ReviewRunReceipt { receipt },
    )
    .expect("review-state record")
    .marker()
    .expect("review-state marker");
    let ledger = serde_json::json!([{
        "author": {"login": "review-bot"},
        "body": marker
    }])
    .to_string();
    let script = format!(
        r#"#!/bin/sh
set -eu
printf '%s\n' "$*" >> {capture:?}
case "$1 $2" in
  "pr view")
    printf '%s\n' '{{"number":7,"url":"https://github.com/acme/widgets/pull/7","state":"OPEN","isDraft":false,"baseRefName":"main","headRefName":"feat/reviews","headRefOid":"head-abc","title":"feat: reviews","body":""}}'
    ;;
  "api graphql")
    case "$*" in
      *"pullRequest(number: \$pr) {{ comments(first: 100"*)
        printf '%s\n' '{{"data":{{"viewer":{{"login":"review-bot"}},"repository":{{"pullRequest":{{"comments":{{"nodes":{ledger},"pageInfo":{{"hasNextPage":false,"endCursor":"state-tip"}}}}}}}}}}}}'
        ;;
      *"node(id: \$review)"*)
        printf '%s\n' '{{"data":{{"node":{{"id":"PRR_pending","url":"https://github.com/acme/widgets/pull/7#pullrequestreview-102","author":{{"login":"review-bot"}},"state":"COMMENTED","commit":{{"oid":"head-abc"}},"body":"Summary\n<!-- forge-cli:review-run:v1 run={review_run_id} -->","viewerDidAuthor":true,"viewerCanDelete":false,"comments":{{"totalCount":0,"nodes":[],"pageInfo":{{"hasNextPage":false,"endCursor":null}}}},"pullRequest":{{"number":7,"url":"https://github.com/acme/widgets/pull/7","headRefOid":"head-abc"}}}}}}}}'
        ;;
      *"reviews(first: 100"*)
        printf '%s\n' '{{"data":{{"viewer":{{"login":"review-bot"}},"repository":{{"pullRequest":{{"headRefOid":"head-abc","reviews":{{"nodes":[{{"id":"PRR_pending","databaseId":102,"url":"https://github.com/acme/widgets/pull/7#pullrequestreview-102","author":{{"login":"review-bot"}},"state":"COMMENTED","commit":{{"oid":"head-abc"}},"submittedAt":"2026-07-20T12:05:00Z","body":"Summary\n<!-- forge-cli:review-run:v1 run={review_run_id} -->","viewerDidAuthor":true}}],"pageInfo":{{"hasNextPage":false,"endCursor":null}}}}}}}}}}}}'
        ;;
      *)
        echo "unexpected graphql args: $*" >&2
        exit 99
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

    let output = run_forge_cli(
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
            "resume-submit",
            "7",
            "--review",
            "PRR_pending",
            "--review-run-id",
            &review_run_id,
            "--expected-head",
            "head-abc",
            "--expected-commit",
            "head-abc",
            "--expected-snapshot",
            "sha256:inspected-before-submit",
            "--decision",
            "comments-only",
        ],
    );

    assert_eq!(output.code, 65, "{}", output.stdout);
    assert_eq!(
        parse_envelope(&output.stdout)["error"]["code"],
        "pending_review_manifest_mismatch"
    );
    let calls = fs::read_to_string(capture).expect("read calls");
    assert!(!calls.contains("submitPullRequestReview(input:"), "{calls}");
    assert!(!calls.contains("deletePullRequestReview(input:"), "{calls}");
}

#[test]
fn pr_pending_review_resume_submit_rejects_identity_mismatch_without_mutation() {
    let stub = StubEnv::new();
    let capture = stub.tempdir.path().join("identity-mismatch-calls.log");
    let manifest = vec![ReviewCommentManifestItem {
        index: 0,
        path: "src/file-0.rs".to_string(),
        line: Some(1),
        side: "RIGHT".to_string(),
        start_line: None,
        start_side: None,
        subject_type: "LINE".to_string(),
        body_digest: sha256_digest(b"first"),
    }];
    let (review_run_id, marker) = receipt_marker("Summary", manifest);
    let snapshot = pending_recovery_snapshot(true, 1, false)
        .replace(INCOMPLETE_RECEIPT_RUN_ID, &review_run_id);
    let script = pending_recovery_script(&capture.to_string_lossy(), &snapshot, &marker);
    let stub = stub.gh_stub(&script);

    let inspect = run_forge_cli(
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
            "inspect",
            "42",
            "--review",
            "PRR_pending",
        ],
    );
    let digest = parse_envelope(&inspect.stdout)["data"]["snapshot"]["snapshot_digest"]
        .as_str()
        .expect("snapshot digest")
        .to_string();
    let output = run_forge_cli(
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
            "resume-submit",
            "42",
            "--review",
            "PRR_pending",
            "--review-run-id",
            &review_run_id,
            "--expected-head",
            "head-new",
            "--expected-commit",
            "head-new",
            "--expected-snapshot",
            &digest,
            "--decision",
            "comments-only",
        ],
    );

    assert_eq!(output.code, 65, "{}", output.stdout);
    assert_eq!(
        parse_envelope(&output.stdout)["error"]["code"],
        "pending_review_identity_mismatch"
    );
    let calls = fs::read_to_string(capture).expect("read calls");
    assert!(!calls.contains("submitPullRequestReview(input:"), "{calls}");
    assert!(!calls.contains("deletePullRequestReview(input:"), "{calls}");
}

#[test]
fn pr_pending_review_unmarked_fourteen_comment_draft_submits_without_data_loss() {
    let stub = StubEnv::new();
    let capture = stub.tempdir.path().join("unmarked-submit-calls.log");
    let snapshot = pending_recovery_snapshot(false, 14, true);
    let script = pending_recovery_script(
        &capture.to_string_lossy(),
        &snapshot,
        &incomplete_receipt_marker(),
    );
    let stub = stub.gh_stub(&script);

    let inspect = run_forge_cli(
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
            "inspect",
            "42",
            "--review",
            "PRR_pending",
        ],
    );
    assert_eq!(inspect.code, 0, "{}", inspect.stdout);
    let inspected = parse_envelope(&inspect.stdout);
    assert_eq!(
        inspected["data"]["snapshot"]["inline_comments"]
            .as_array()
            .expect("comments")
            .len(),
        14
    );
    assert_eq!(inspected["data"]["snapshot"]["provenance"], "unmarked");
    let digest = inspected["data"]["snapshot"]["snapshot_digest"]
        .as_str()
        .expect("snapshot digest")
        .to_string();

    let output = run_forge_cli(
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
            "submit",
            "42",
            "--review",
            "PRR_pending",
            "--expected-head",
            "head-new",
            "--expected-commit",
            "head-new",
            "--expected-snapshot",
            &digest,
            "--decision",
            "comments-only",
            "--confirm-unmarked-submit",
        ],
    );

    assert_eq!(
        output.code, 0,
        "stdout={}\nstderr={}",
        output.stdout, output.stderr
    );
    let calls = fs::read_to_string(capture).expect("read calls");
    assert!(calls.contains("submitPullRequestReview(input:"), "{calls}");
    assert!(!calls.contains("deletePullRequestReview(input:"), "{calls}");
}

#[test]
fn pr_pending_review_discard_requires_distinct_inline_content_approval() {
    let stub = StubEnv::new();
    let capture = stub.tempdir.path().join("discard-calls.log");
    let snapshot = pending_recovery_snapshot(false, 14, true);
    let script = pending_recovery_script(
        &capture.to_string_lossy(),
        &snapshot,
        &incomplete_receipt_marker(),
    );
    let stub = stub.gh_stub(&script);

    let inspect = run_forge_cli(
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
            "inspect",
            "42",
            "--review",
            "PRR_pending",
        ],
    );
    let digest = parse_envelope(&inspect.stdout)["data"]["snapshot"]["snapshot_digest"]
        .as_str()
        .expect("snapshot digest")
        .to_string();
    let base_args = [
        "--provider",
        "github",
        "--repo",
        "acme/widgets",
        "--format",
        "json",
        "pr",
        "pending-review",
        "discard",
        "42",
        "--review",
        "PRR_pending",
        "--expected-head",
        "head-new",
        "--expected-commit",
        "head-new",
        "--expected-snapshot",
        digest.as_str(),
        "--confirm-discard",
    ];
    let rejected = run_forge_cli(&stub, &base_args);
    assert_eq!(rejected.code, 65, "{}", rejected.stdout);
    assert_eq!(
        parse_envelope(&rejected.stdout)["error"]["code"],
        "pending_review_inline_discard_approval_required"
    );
    let calls = fs::read_to_string(&capture).expect("read calls");
    assert!(!calls.contains("deletePullRequestReview(input:"), "{calls}");

    let mut approved_args = base_args.to_vec();
    approved_args.push("--confirm-inline-content-loss");
    let approved = run_forge_cli(&stub, &approved_args);
    assert_eq!(
        approved.code, 0,
        "stdout={}\nstderr={}",
        approved.stdout, approved.stderr
    );
    let approved_envelope = parse_envelope(&approved.stdout);
    assert_eq!(approved_envelope["data"]["inline_comment_count"], 14);
    assert_eq!(approved_envelope["data"]["discarded"], true);
}

#[test]
fn pr_pending_review_delete_verifies_and_deletes_the_exact_pending_node() {
    let stub = StubEnv::new();
    let capture = stub.tempdir.path().join("gh-calls.log");
    let deleted = stub.tempdir.path().join("deleted.flag");
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
        : > {deleted:?}
        printf '%s\n' '{{"data":{{"deletePullRequestReview":{{"pullRequestReview":{{"id":"PRR_pending","url":"https://github.com/acme/widgets/pull/42#pullrequestreview-102"}}}}}}}}'
        ;;
      *"comments(first: 1)"*)
        if [ -e {deleted:?} ]; then
          printf '%s\n' '{{"data":{{"node":null}}}}'
        else
          printf '%s\n' '{{"data":{{"node":{{"id":"PRR_pending","url":"https://github.com/acme/widgets/pull/42#pullrequestreview-102","author":{{"login":"example-review-bot[bot]"}},"state":"PENDING","commit":{{"oid":"head-new"}},"body":"Pending review summary","viewerDidAuthor":true,"viewerCanDelete":true,"comments":{{"totalCount":0}},"pullRequest":{{"number":42,"url":"https://github.com/acme/widgets/pull/42","headRefOid":"head-new"}}}}}}}}'
        fi
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
    let deleted = stub.tempdir.path().join("deleted.flag");
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
        : > {deleted:?}
        printf '%s\n' '{{"data":{{"deletePullRequestReview":{{"pullRequestReview":{{"id":"PRR_page_2","url":"https://github.com/acme/widgets/pull/42#pullrequestreview-202"}}}}}}}}'
        ;;
      *"comments(first: 1)"*)
        if [ -e {deleted:?} ]; then
          printf '%s\n' '{{"data":{{"node":null}}}}'
        else
          printf '%s\n' '{{"data":{{"node":{{"id":"PRR_page_2","url":"https://github.com/acme/widgets/pull/42#pullrequestreview-202","author":{{"login":"example-review-bot[bot]"}},"state":"PENDING","commit":{{"oid":"head-new"}},"body":"target","viewerDidAuthor":true,"viewerCanDelete":true,"comments":{{"totalCount":0}},"pullRequest":{{"number":42,"url":"https://github.com/acme/widgets/pull/42","headRefOid":"head-new"}}}}}}}}'
        fi
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
