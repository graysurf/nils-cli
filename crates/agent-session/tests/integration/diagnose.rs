//! Integration coverage for the control-plane diagnostic entry point
//! (`sympoies/nils-cli#1409`, observability workstream).
//!
//! The command exists because no surface could answer "why did this session
//! degrade?". It must therefore read the shared observation spool, project health
//! from it, name the available typed recovery, and keep working with no serve
//! daemon, no broker, and no coordination registry present.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

use nils_test_support::cmd::{CmdOptions, CmdOutput, run_resolved};
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

fn diagnose(state_dir: &Path, args: &[&str]) -> CmdOutput {
    let mut full = vec!["--state-dir", state_dir.to_str().expect("state dir UTF-8")];
    full.push("diagnose");
    full.extend_from_slice(args);
    run_resolved("agent-session", &full, &CmdOptions::new())
}

/// Append one already-validated observation event to the bounded spool.
fn append_event(state_dir: &Path, event: &Value) {
    let spool = state_dir.join("observation/spool");
    fs::create_dir_all(&spool).expect("spool directory");
    fs::set_permissions(&spool, fs::Permissions::from_mode(0o700)).expect("spool mode");
    let segment = spool.join("segment-000000000001.jsonl");
    let mut file = OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .open(&segment)
        .expect("spool segment");
    file.write_all(serde_json::to_string(event).expect("event JSON").as_bytes())
        .expect("write event");
    file.write_all(b"\n").expect("terminate event");
}

fn event(code: &str, severity: &str, epoch: i64, recovery: Option<&str>) -> Value {
    let mut value = json!({
        "schema_version": "agent-session.observation.v1",
        "recorded_at": "2026-08-03T00:00:00Z",
        "recorded_at_epoch": epoch,
        "component": "agent-hook",
        "stage": "dispatch",
        "code": code,
        "severity": severity,
        "binary_version": "1.25.13"
    });
    if let Some(recovery) = recovery {
        value["recovery_action"] = json!(recovery);
    }
    value
}

/// With no spool at all the command still succeeds. An operator running it on a
/// cold host must get an answer, not an error they then have to diagnose.
#[test]
fn diagnose_reports_a_healthy_empty_plane_without_any_control_plane_state() {
    let temporary = tempfile::TempDir::new().expect("temporary state");
    let state_dir = temporary.path();

    let json_output = diagnose(state_dir, &["--format", "json"]);
    assert_eq!(json_output.code, 0, "stderr={}", json_output.stderr_text());
    let envelope = json_output.stdout_json();
    assert_eq!(envelope["schema_version"], "cli.agent-session.diagnose.v1");
    assert_eq!(envelope["ok"], true);
    assert_eq!(
        envelope["data"]["schema_version"],
        "agent-session.diagnostic-bundle.v1"
    );
    assert_eq!(envelope["data"]["health"], "healthy");
    assert_eq!(envelope["data"]["observation"]["event_count"], 0);
    assert_eq!(envelope["data"]["runtime"]["release_skew"], json!([]));

    let text_output = diagnose(state_dir, &[]);
    assert_eq!(text_output.code, 0, "stderr={}", text_output.stderr_text());
    assert!(
        text_output.stdout_text().contains("health: healthy"),
        "{}",
        text_output.stdout_text()
    );
}

/// The bundle aggregates the plane into per-code counters with a seen window, and
/// it carries the recovery action forward so the operator gets one next step.
#[test]
fn diagnose_aggregates_codes_and_projects_degraded_health() {
    let temporary = tempfile::TempDir::new().expect("temporary state");
    let state_dir = temporary.path();
    append_event(state_dir, &event("dispatch-completed", "info", 100, None));
    append_event(state_dir, &event("dispatch-completed", "info", 110, None));
    append_event(
        state_dir,
        &event(
            "coordination-degraded-read-only",
            "warn",
            120,
            Some("agent-session broker status --session <id>"),
        ),
    );

    let output = diagnose(state_dir, &["--format", "json"]);
    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let bundle = output.stdout_json()["data"].clone();
    assert_eq!(bundle["health"], "degraded");
    assert_eq!(bundle["observation"]["event_count"], 3);
    assert_eq!(bundle["observation"]["first_seen_epoch"], 100);
    assert_eq!(bundle["observation"]["last_seen_epoch"], 120);

    let summary = bundle["observation"]["summary"]
        .as_array()
        .expect("summary array");
    // Most severe first, so the operator reads the actionable code before volume.
    assert_eq!(summary[0]["code"], "coordination-degraded-read-only");
    assert_eq!(summary[0]["severity"], "warn");
    assert_eq!(
        summary[0]["recovery_action"],
        "agent-session broker status --session <id>"
    );
    assert_eq!(summary[1]["code"], "dispatch-completed");
    assert_eq!(summary[1]["count"], 2);
    assert_eq!(summary[1]["first_seen_epoch"], 100);
    assert_eq!(summary[1]["last_seen_epoch"], 110);

    let rendered = diagnose(state_dir, &[]).stdout_text();
    assert!(rendered.contains("health: degraded"), "{rendered}");
    assert!(
        rendered.contains("-> agent-session broker status --session <id>"),
        "{rendered}"
    );
}

/// The recent slice is bounded by `--limit` while the counters keep the whole
/// retained window, so a dominant failure shape stays visible without reprinting
/// every occurrence.
#[test]
fn diagnose_bounds_the_recent_slice_without_losing_the_counters() {
    let temporary = tempfile::TempDir::new().expect("temporary state");
    let state_dir = temporary.path();
    for epoch in 1..=10 {
        append_event(state_dir, &event("dispatch-completed", "info", epoch, None));
    }

    let bundle =
        diagnose(state_dir, &["--format", "json", "--limit", "3"]).stdout_json()["data"].clone();
    assert_eq!(bundle["observation"]["event_count"], 10);
    let recent = bundle["observation"]["recent"]
        .as_array()
        .expect("recent array");
    assert_eq!(recent.len(), 3);
    assert_eq!(recent[0]["recorded_at_epoch"], 8);
    assert_eq!(recent[2]["recorded_at_epoch"], 10);
    assert_eq!(bundle["observation"]["summary"][0]["count"], 10);
}

/// A live broker on another release generation is the upgrade condition. It has
/// to appear as its own distinct evidence with a bounded recovery, not merely as
/// a hook failure count.
#[test]
fn diagnose_reports_broker_release_skew_with_its_bounded_recovery() {
    let temporary = tempfile::TempDir::new().expect("temporary state");
    let state_dir = temporary.path();
    let coordination = state_dir.join("coordination");
    fs::create_dir_all(&coordination).expect("coordination directory");
    let registry = coordination.join("registry.json");
    fs::write(
        &registry,
        serde_json::to_vec(&json!({
            "schema_version": "agent-session.coordination-registry.v1",
            "fingerprint_epoch": 1,
            "fingerprint_key": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "brokers": {
                "old-worker": {
                    "session_id": "old-worker",
                    "incarnation": "inc-old",
                    "state": "ready",
                    "binary_version": "0.9.0"
                },
                "unpublished-worker": {
                    "session_id": "unpublished-worker",
                    "incarnation": "inc-unpublished",
                    "state": "ready"
                }
            },
            "claims": []
        }))
        .expect("registry JSON"),
    )
    .expect("registry");
    fs::set_permissions(&registry, fs::Permissions::from_mode(0o600)).expect("registry mode");

    let bundle = diagnose(state_dir, &["--format", "json"]).stdout_json()["data"].clone();
    assert_eq!(bundle["health"], "degraded");
    let skew = bundle["runtime"]["release_skew"]
        .as_array()
        .expect("release skew array");
    assert_eq!(
        skew.len(),
        1,
        "only a broker that published a crossing release is drift: {skew:?}"
    );
    assert_eq!(skew[0]["session_id"], "old-worker");
    assert_eq!(skew[0]["broker_release"], "0.9.0");
    assert_eq!(
        skew[0]["recovery_action"],
        "agent-session broker reconcile --session old-worker"
    );

    let rendered = diagnose(state_dir, &[]).stdout_text();
    assert!(
        rendered.contains("release skew: session old-worker"),
        "{rendered}"
    );
}

/// Coordination being unreadable is one of the states this command diagnoses, so
/// it must not become a reason the diagnosis itself fails.
#[test]
fn diagnose_survives_an_unreadable_coordination_registry() {
    let temporary = tempfile::TempDir::new().expect("temporary state");
    let state_dir = temporary.path();
    let coordination = state_dir.join("coordination");
    fs::create_dir_all(&coordination).expect("coordination directory");
    let registry = coordination.join("registry.json");
    fs::write(&registry, b"{").expect("malformed registry");
    fs::set_permissions(&registry, fs::Permissions::from_mode(0o600)).expect("registry mode");
    append_event(
        state_dir,
        &event(
            "coordination-invalid",
            "error",
            5,
            Some("agent-session diagnose"),
        ),
    );

    let output = diagnose(state_dir, &["--format", "json"]);
    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let bundle = output.stdout_json()["data"].clone();
    assert_eq!(bundle["health"], "degraded");
    assert_eq!(bundle["runtime"]["release_skew"], json!([]));
    assert_eq!(
        bundle["observation"]["summary"][0]["code"],
        "coordination-invalid"
    );
}
