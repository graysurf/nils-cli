//! Plan Issue vNext compatibility baseline (Task 1.2).
//!
//! Locks the released `record` command surface, envelope, and error shape.
//! The baseline guards the current active contract; it must not grow into a
//! permanent old-state-payload compatibility suite.
//!
//! Source: `docs/plans/plan-issue-vnext-implementation/plan-issue-vnext-implementation-plan.md`
//! Task 1.2; design constraints from
//! `docs/source/plan-issue-redesign/plan-tracking-issue-cli-redesign-v1.md`
//! ("Preserve" table).

use std::fs;

use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use tempfile::TempDir;

use plan_issue::commands::record::{LifecycleCommentKind, RecordProfile};
use plan_issue::lifecycle_record::PAYLOAD_SCHEMA_V2;
use plan_issue::lifecycle_vnext::registry;
use plan_issue::tracking;

use crate::common;

fn json_stdout(out: &common::CmdOut) -> Value {
    serde_json::from_str(&out.stdout).expect("json stdout")
}

fn v2_comment_body(role: &str, profile: &str, data: Value) -> String {
    let envelope = json!({
        "schema": PAYLOAD_SCHEMA_V2,
        "role": role,
        "profile": profile,
        "data": data,
    });
    let payload = serde_json::to_string(&envelope).expect("serialize payload");
    format!(
        "<!-- plan-issue-record:v2 role={role} profile={profile} -->\n\n```plan-issue-record-payload\n{payload}\n```\n",
    )
}

/// Lock the released `record` subcommand names so vNext rewrites cannot
/// accidentally rename, merge, or drop a subcommand without surfacing here.
#[test]
fn record_subcommand_help_keeps_released_command_surface() {
    for sub in [
        "open",
        "attach",
        "post",
        "repair-dashboard",
        "close",
        "audit",
    ] {
        let out = common::run_plan_issue(&["record", sub, "--help"]);
        assert_eq!(
            out.code, 0,
            "record {sub} --help should exit success; stderr: {}",
            out.stderr
        );
        assert!(
            out.stdout.contains(sub),
            "record {sub} --help stdout missing subcommand name: {}",
            out.stdout
        );
    }
}

#[test]
fn lifecycle_reader_help_declares_no_old_state_payload_contract() {
    for args in [
        &["record", "audit", "--help"][..],
        &["record", "repair-dashboard", "--help"][..],
        &["tracking", "status", "--help"][..],
        &["tracking", "close-ready", "--help"][..],
    ] {
        let out = common::run_plan_issue(args);
        assert_eq!(
            out.code, 0,
            "help should exit success for {:?}; stderr: {}",
            args, out.stderr
        );
        assert!(
            out.stdout.contains("active payload contract"),
            "help should name the active payload contract for {:?}: {}",
            args,
            out.stdout
        );
        assert!(
            out.stdout.contains("one-off migration/repair"),
            "help should route old state payloads to one-off migration/repair for {:?}: {}",
            args,
            out.stdout
        );
        assert!(
            out.stdout.contains("no long-term v2 reader"),
            "help should reject long-term v2 reader scope for {:?}: {}",
            args,
            out.stdout
        );
    }
}

/// Lock the LifecycleCommentKind value-enum so downstream runtime-kit
/// usage of `--kind <role>` keeps the same set of accepted roles during
/// the rewrite.
#[test]
fn lifecycle_comment_kind_value_enum_keeps_seven_roles() {
    use LifecycleCommentKind::*;
    let roles = [Source, Plan, State, Session, Validation, Review, Closeout];
    let strs: Vec<&'static str> = roles.iter().map(|k| k.as_str()).collect();
    assert_eq!(
        strs,
        vec![
            "source",
            "plan",
            "state",
            "session",
            "validation",
            "review",
            "closeout"
        ]
    );

    // Profile surface must stay binary tracking|dispatch.
    assert_eq!(RecordProfile::Tracking.as_str(), "tracking");
    assert_eq!(RecordProfile::Dispatch.as_str(), "dispatch");
}

/// Representative success envelope: `record audit` returns the v2 schema
/// envelope, `status=ok`, and a typed `audit` payload. Runtime-kit and
/// runtime smoke both depend on this envelope shape.
#[test]
fn record_audit_success_envelope_shape_is_stable() {
    let tmp = TempDir::new().expect("tmp");
    let body = tmp.path().join("body.md");
    let comments = tmp.path().join("comments.json");
    fs::write(&body, "## Current Dashboard\n\n## Durable Record\n").expect("write body");
    let payload = json!({
        "comments": [
            {
                "url": "https://github.com/owner/repo/issues/1#issuecomment-state",
                "body": v2_comment_body(
                    "state",
                    "tracking",
                    json!({
                        "status": "in-progress",
                        "target_scope": "compat baseline",
                        "tasks": [],
                        "prs": []
                    }),
                ),
            }
        ]
    });
    fs::write(&comments, payload.to_string()).expect("write comments");

    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "record",
        "audit",
        "--profile",
        "tracking",
        "--body-file",
        body.to_str().expect("body"),
        "--comments-json",
        comments.to_str().expect("comments"),
    ]);

    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let envelope = json_stdout(&out);
    assert_eq!(
        envelope["schema_version"], "plan-issue.record.audit.v2",
        "schema_version drift; full envelope: {envelope}"
    );
    assert_eq!(envelope["command"], "record.audit");
    assert_eq!(envelope["status"], "ok");
    assert_eq!(envelope["payload"]["execution_mode"], "live");
    assert!(
        envelope["payload"]["result"]["audit"].is_object(),
        "audit payload missing: {envelope}"
    );
}

/// Representative failure envelope: missing required `--comments-json`
/// returns the usage exit code (64) — locks parse-time error behavior.
#[test]
fn record_audit_failure_returns_usage_exit_for_missing_arg() {
    let out = common::run_plan_issue(&["--format", "json", "record", "audit"]);
    assert_eq!(
        out.code, 64,
        "record audit without args should exit USAGE (64); stderr: {}",
        out.stderr
    );
}

/// Representative runtime failure: `record audit` against a path that does
/// not exist returns the v2 error envelope with `status=error` and a stable
/// machine-readable code (not a panic, not an unbounded message).
#[test]
fn record_audit_runtime_failure_envelope_is_stable() {
    let tmp = TempDir::new().expect("tmp");
    let body = tmp.path().join("missing-body.md");
    let comments = tmp.path().join("missing-comments.json");
    // Intentionally do not create the files.

    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "record",
        "audit",
        "--body-file",
        body.to_str().expect("body"),
        "--comments-json",
        comments.to_str().expect("comments"),
    ]);

    assert_ne!(out.code, 0, "missing fixture must not silently succeed");
    let envelope = json_stdout(&out);
    assert_eq!(envelope["schema_version"], "plan-issue.record.audit.v2");
    assert_eq!(envelope["command"], "record.audit");
    assert_eq!(envelope["status"], "error");
    let code = envelope["error"]["code"]
        .as_str()
        .expect("error.code is a string");
    assert!(!code.is_empty(), "error code should be a non-empty string");
}

/// vNext registry must cover every released `LifecycleCommentKind`. This
/// guards against accidental drift where the rewrite drops or renames a
/// role compared to the released `record post --kind` surface.
#[test]
fn lifecycle_vnext_registry_matches_released_role_set() {
    let registered: Vec<&'static str> = registry::all_roles()
        .iter()
        .map(|spec| spec.marker_role)
        .collect();
    assert_eq!(
        registered,
        vec![
            "source",
            "plan",
            "state",
            "session",
            "validation",
            "review",
            "closeout"
        ]
    );
}

/// Runtime layout resolution must remain shared infrastructure. Tracking
/// modules read schema identifiers but rely on the canonical
/// `runtime_layout::runtime_root()` for the on-disk path math (Task 4.1
/// implementation; this baseline asserts the shapes line up at
/// compile/link time).
#[test]
fn tracking_run_state_schema_constants_are_stable() {
    assert_eq!(
        tracking::run_state::RUN_STATE_SCHEMA,
        "plan-issue.execution-run.v1"
    );
    assert_eq!(
        tracking::events::EVENT_SCHEMA,
        "plan-issue.execution-event.v1"
    );
}

/// Plan-issue-local must still expose the released `record` surface so
/// fixture-driven runtime smoke continues to work during the vNext rewrite.
#[test]
fn plan_issue_local_keeps_record_subcommand_help() {
    let out = common::run_plan_issue_local(&["record", "--help"]);
    assert_eq!(
        out.code, 0,
        "plan-issue-local record --help should exit success; stderr: {}",
        out.stderr
    );
    for sub in [
        "open",
        "attach",
        "post",
        "repair-dashboard",
        "close",
        "audit",
    ] {
        assert!(
            out.stdout.contains(sub),
            "plan-issue-local record --help missing `{sub}`: {}",
            out.stdout
        );
    }
}
