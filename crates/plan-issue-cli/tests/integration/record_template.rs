//! `plan-issue record template` integration coverage (Task 3.1).
//!
//! The non-mutating preview command must render the visible Markdown
//! skeleton and JSON payload skeleton for every lifecycle role using the
//! same vNext registry that drives the renderer.
//!
//! Source: `docs/source/plan-issue-redesign/plan-tracking-issue-cli-redesign-v1.md`
//! Workstream 3.

use pretty_assertions::assert_eq;
use serde_json::Value;

use crate::common;

fn json_stdout(out: &common::CmdOut) -> Value {
    serde_json::from_str(&out.stdout).expect("json stdout")
}

#[test]
fn record_template_help_lists_template_under_record() {
    let out = common::run_plan_issue(&["record", "--help"]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("template"),
        "record --help missing template subcommand: {}",
        out.stdout
    );
}

#[test]
fn record_template_markdown_renders_every_role() {
    for role in ["source", "plan", "state", "session", "validation", "review", "closeout"] {
        let out = common::run_plan_issue(&[
            "--format",
            "json",
            "record",
            "template",
            "--profile",
            "tracking",
            "--kind",
            role,
            "--shape",
            "markdown",
        ]);
        assert_eq!(out.code, 0, "role {role} stderr: {}", out.stderr);
        let envelope = json_stdout(&out);
        let template = envelope["payload"]["result"]["template"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            template.contains("<!-- plan-issue-record:v2"),
            "role {role} markdown missing marker: {template}"
        );
        assert!(
            template.contains("<!-- plan-issue-record-payload:hex:"),
            "role {role} markdown missing payload placeholder: {template}"
        );
        // Templates must not render the pre-v2 visible fenced payload.
        assert!(
            !template.contains("```plan-issue-record-payload"),
            "role {role} markdown contains pre-v2 fenced payload: {template}"
        );
    }
}

#[test]
fn record_template_json_renders_every_role() {
    for role in ["source", "plan", "state", "session", "validation", "review", "closeout"] {
        let out = common::run_plan_issue(&[
            "--format",
            "json",
            "record",
            "template",
            "--profile",
            "tracking",
            "--kind",
            role,
            "--shape",
            "json",
        ]);
        assert_eq!(out.code, 0, "role {role} stderr: {}", out.stderr);
        let envelope = json_stdout(&out);
        let template = envelope["payload"]["result"]["template"]
            .as_str()
            .unwrap_or_default();
        let parsed: Value = serde_json::from_str(template).expect("template is valid JSON");
        assert_eq!(parsed["schema"], "plan-issue-record.payload.v2");
        assert_eq!(parsed["role"], role);
        assert_eq!(parsed["profile"], "tracking");
        assert!(parsed["data"].is_object(), "role {role} data missing");
    }
}

#[test]
fn record_template_state_markdown_includes_task_ledger() {
    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "record",
        "template",
        "--kind",
        "state",
        "--shape",
        "markdown",
    ]);
    assert_eq!(out.code, 0);
    let envelope = json_stdout(&out);
    let template = envelope["payload"]["result"]["template"]
        .as_str()
        .unwrap_or_default();
    assert!(
        template.contains("## Task Ledger"),
        "state template missing ## Task Ledger heading: {template}"
    );
}

#[test]
fn record_template_envelope_shape_is_stable() {
    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "record",
        "template",
        "--kind",
        "validation",
        "--shape",
        "markdown",
    ]);
    assert_eq!(out.code, 0);
    let envelope = json_stdout(&out);
    assert_eq!(envelope["schema_version"], "plan-issue-cli.record.template.v2");
    assert_eq!(envelope["command"], "record.template");
    assert_eq!(envelope["status"], "ok");
    let result = &envelope["payload"]["result"];
    assert_eq!(result["operation"], "template");
    assert_eq!(result["profile"], "tracking");
    assert_eq!(result["role"], "validation");
    assert_eq!(result["shape"], "markdown");
}

#[test]
fn record_template_dispatch_profile_renders_for_every_role() {
    for role in ["source", "plan", "state", "session", "validation", "review", "closeout"] {
        let out = common::run_plan_issue(&[
            "--format",
            "json",
            "record",
            "template",
            "--profile",
            "dispatch",
            "--kind",
            role,
            "--shape",
            "markdown",
        ]);
        assert_eq!(out.code, 0, "dispatch role {role} stderr: {}", out.stderr);
        let envelope = json_stdout(&out);
        let template = envelope["payload"]["result"]["template"]
            .as_str()
            .unwrap_or_default();
        assert!(
            template.contains("profile=dispatch"),
            "dispatch role {role} marker profile drift: {template}"
        );
    }
}
