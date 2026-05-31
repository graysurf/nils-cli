//! `plan-issue record restore` integration coverage.
//!
//! Source: `docs/plans/plan-issue-record-restore/plan-issue-record-restore-plan.md`
//! (Sprint 1 — snapshot parser + restore command).
//!
//! These tests drive the real `render_record_snapshot_comment` renderer to
//! build the issue comments, then restore from them, so they pin the
//! `open`->`restore` round-trip against the canonical snapshot format.

use std::fs;

use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use tempfile::TempDir;

use plan_issue::commands::record::{LifecycleCommentKind, RecordProfile};
use plan_issue::lifecycle_record::{self, SnapshotData};

use crate::common;

fn json_stdout(out: &common::CmdOut) -> Value {
    serde_json::from_str(&out.stdout).expect("json stdout")
}

fn snapshot_comment(kind: LifecycleCommentKind, path: &str, commit: &str, content: &str) -> String {
    let snapshot = SnapshotData {
        path: path.to_string(),
        commit: commit.to_string(),
        title: None,
        summary: None,
    };
    lifecycle_record::render_record_snapshot_comment(
        RecordProfile::Tracking,
        kind,
        &snapshot,
        content,
        None,
    )
    .expect("render snapshot comment")
}

fn comment_obj(idx: usize, body: String) -> Value {
    json!({
        "url": format!("https://example.com/c{idx}"),
        "created_at": format!("2026-05-26T08:00:0{idx}Z"),
        "body": body,
    })
}

fn write_comments(tmp: &TempDir, comments: Vec<Value>) -> std::path::PathBuf {
    let path = tmp.path().join("comments.json");
    fs::write(&path, json!({ "comments": comments }).to_string()).expect("write comments");
    path
}

#[test]
fn record_restore_round_trips_source_and_plan() {
    let tmp = TempDir::new().expect("tmp");
    // Content exercises an inline `<details>` mention (must not be treated as a
    // nested fold) and an `->` arrow that earlier tooling escaping would mangle.
    let source_content =
        "# Source Doc\n\nLine with an inline `<details>` mention and a -> arrow.\nMore text.\n";
    let plan_content = "# Plan Doc\n\n## Sprint 1\n\n- task one\n- task two\n";
    let comments = vec![
        comment_obj(
            0,
            snapshot_comment(
                LifecycleCommentKind::Source,
                "docs/plans/foo/foo-discussion-source.md",
                "abc123",
                source_content,
            ),
        ),
        comment_obj(
            1,
            snapshot_comment(
                LifecycleCommentKind::Plan,
                "docs/plans/foo/foo-plan.md",
                "abc123",
                plan_content,
            ),
        ),
    ];
    let comments_path = write_comments(&tmp, comments);
    let out = tmp.path().join("restore-out");

    let result = common::run_plan_issue(&[
        "--format",
        "json",
        "record",
        "restore",
        "--comments-json",
        comments_path.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
    ]);
    assert_eq!(result.code, 0, "stderr: {}", result.stderr);

    let restored_source =
        fs::read_to_string(out.join("docs/plans/foo/foo-discussion-source.md")).expect("source");
    let restored_plan = fs::read_to_string(out.join("docs/plans/foo/foo-plan.md")).expect("plan");
    assert_eq!(
        restored_source, source_content,
        "source round-trips verbatim"
    );
    assert_eq!(restored_plan, plan_content, "plan round-trips verbatim");

    let envelope = json_stdout(&result);
    let restored = envelope["payload"]["result"]["restored"]
        .as_array()
        .expect("restored array");
    assert_eq!(restored.len(), 2, "{envelope}");
    let by_role: std::collections::BTreeMap<&str, &Value> = restored
        .iter()
        .map(|entry| (entry["role"].as_str().unwrap(), entry))
        .collect();
    assert_eq!(by_role["source"]["commit"], "abc123");
    assert_eq!(by_role["plan"]["path"], "docs/plans/foo/foo-plan.md");
}

#[test]
fn record_restore_preserves_nested_details_in_content() {
    let tmp = TempDir::new().expect("tmp");
    // The content itself contains a block-level <details> fold; depth tracking
    // must stop only at the snapshot wrapper's matching close.
    let content = "# Doc\n\nBefore.\n\n<details>\n<summary>inner</summary>\n\nnested body\n\n</details>\n\nAfter.\n";
    let comments = vec![comment_obj(
        0,
        snapshot_comment(
            LifecycleCommentKind::Source,
            "docs/plans/foo/foo-discussion-source.md",
            "abc",
            content,
        ),
    )];
    // A plan role is required, so add a minimal one.
    let mut comments = comments;
    comments.push(comment_obj(
        1,
        snapshot_comment(
            LifecycleCommentKind::Plan,
            "docs/plans/foo/foo-plan.md",
            "abc",
            "# Plan\n",
        ),
    ));
    let comments_path = write_comments(&tmp, comments);
    let out = tmp.path().join("restore-out");

    let result = common::run_plan_issue(&[
        "--format",
        "json",
        "record",
        "restore",
        "--comments-json",
        comments_path.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
    ]);
    assert_eq!(result.code, 0, "stderr: {}", result.stderr);

    let restored =
        fs::read_to_string(out.join("docs/plans/foo/foo-discussion-source.md")).expect("source");
    assert_eq!(restored, content, "nested <details> content round-trips");
}

#[test]
fn record_restore_selects_latest_snapshot_per_role() {
    let tmp = TempDir::new().expect("tmp");
    let comments = vec![
        comment_obj(
            0,
            snapshot_comment(
                LifecycleCommentKind::Source,
                "docs/plans/foo/foo-discussion-source.md",
                "old",
                "OLD CONTENT\n",
            ),
        ),
        comment_obj(
            2,
            snapshot_comment(
                LifecycleCommentKind::Source,
                "docs/plans/foo/foo-discussion-source.md",
                "new",
                "NEW CONTENT\n",
            ),
        ),
        comment_obj(
            1,
            snapshot_comment(
                LifecycleCommentKind::Plan,
                "docs/plans/foo/foo-plan.md",
                "abc",
                "# Plan\n",
            ),
        ),
    ];
    let comments_path = write_comments(&tmp, comments);
    let out = tmp.path().join("restore-out");

    let result = common::run_plan_issue(&[
        "--format",
        "json",
        "record",
        "restore",
        "--comments-json",
        comments_path.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
    ]);
    assert_eq!(result.code, 0, "stderr: {}", result.stderr);

    let restored =
        fs::read_to_string(out.join("docs/plans/foo/foo-discussion-source.md")).expect("source");
    assert_eq!(restored, "NEW CONTENT\n", "latest source snapshot wins");

    let envelope = json_stdout(&result);
    let source_commit = envelope["payload"]["result"]["restored"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["role"] == "source")
        .map(|entry| entry["commit"].clone())
        .unwrap();
    assert_eq!(source_commit, "new", "latest commit provenance reported");
}

#[test]
fn record_restore_errors_on_missing_required_role() {
    let tmp = TempDir::new().expect("tmp");
    // Only a source snapshot; the plan role is absent.
    let comments = vec![comment_obj(
        0,
        snapshot_comment(
            LifecycleCommentKind::Source,
            "docs/plans/foo/foo-discussion-source.md",
            "abc",
            "# Source\n",
        ),
    )];
    let comments_path = write_comments(&tmp, comments);
    let out = tmp.path().join("restore-out");

    let result = common::run_plan_issue(&[
        "--format",
        "json",
        "record",
        "restore",
        "--comments-json",
        comments_path.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
    ]);
    assert_ne!(result.code, 0, "missing role must fail");
    let combined = format!("{}{}", result.stdout, result.stderr);
    assert!(
        combined.contains("record-restore-missing-role") && combined.contains("plan"),
        "expected missing-role error naming plan: stdout={} stderr={}",
        result.stdout,
        result.stderr
    );
    assert!(
        !out.join("docs/plans/foo/foo-discussion-source.md").exists(),
        "no files should be written when a required role is missing"
    );
}

#[test]
fn record_restore_refuses_overwrite_without_force() {
    let tmp = TempDir::new().expect("tmp");
    let comments = vec![
        comment_obj(
            0,
            snapshot_comment(
                LifecycleCommentKind::Source,
                "docs/plans/foo/foo-discussion-source.md",
                "abc",
                "RESTORED SOURCE\n",
            ),
        ),
        comment_obj(
            1,
            snapshot_comment(
                LifecycleCommentKind::Plan,
                "docs/plans/foo/foo-plan.md",
                "abc",
                "RESTORED PLAN\n",
            ),
        ),
    ];
    let comments_path = write_comments(&tmp, comments);
    let out = tmp.path().join("restore-out");

    // Pre-create one target file so restore would clobber it.
    let existing = out.join("docs/plans/foo/foo-plan.md");
    fs::create_dir_all(existing.parent().unwrap()).unwrap();
    fs::write(&existing, "ORIGINAL PLAN\n").unwrap();

    let refused = common::run_plan_issue(&[
        "--format",
        "json",
        "record",
        "restore",
        "--comments-json",
        comments_path.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
    ]);
    assert_ne!(refused.code, 0, "overwrite must be refused without --force");
    let combined = format!("{}{}", refused.stdout, refused.stderr);
    assert!(
        combined.contains("record-restore-would-overwrite"),
        "expected would-overwrite error: {combined}"
    );
    assert_eq!(
        fs::read_to_string(&existing).unwrap(),
        "ORIGINAL PLAN\n",
        "existing file untouched on refusal"
    );

    // With the global --force the restore overwrites.
    let forced = common::run_plan_issue(&[
        "--format",
        "json",
        "--force",
        "record",
        "restore",
        "--comments-json",
        comments_path.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
    ]);
    assert_eq!(forced.code, 0, "stderr: {}", forced.stderr);
    assert_eq!(
        fs::read_to_string(&existing).unwrap(),
        "RESTORED PLAN\n",
        "--force overwrites the existing file"
    );
}

#[test]
fn record_restore_help_mentions_out_and_offline_inputs() {
    let out = common::run_plan_issue(&["record", "restore", "--help"]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("--out"),
        "help missing --out: {}",
        out.stdout
    );
    assert!(
        out.stdout.contains("--comments-json"),
        "help missing --comments-json: {}",
        out.stdout
    );
}
