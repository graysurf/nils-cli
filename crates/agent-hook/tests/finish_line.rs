#![cfg(target_os = "linux")]

mod support;

use std::fs;
use std::io::Write;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use pretty_assertions::{assert_eq, assert_ne};
use serde_json::{Value, json};

use support::Fixture;

const POLICY: &str = r#"schema_version = "agent-hook.policy.v1"
bundle_id = "finish-line-test"
version = "2026.08.18.2"
"#;

fn fixture() -> Fixture {
    let fixture = Fixture::new(POLICY);
    let status = std::process::Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(&fixture.root)
        .status()
        .expect("git init");
    assert!(status.success(), "git init failed");
    fixture
}

fn install_contracts(fixture: &Fixture, contracts: &[(&str, &[&str])]) {
    let mut body = String::new();
    for (intent, commands) in contracts {
        body.push_str(&format!(
            "[[validation]]\ncontext = {intent:?}\nproduct = \"dsh\"\ncommands = {commands:?}\ndescription = \"finish-line test\"\n\n"
        ));
    }
    fs::write(fixture.root.join("AGENT_DOCS.toml"), body).expect("agent-docs catalog");
}

fn one_contract(fixture: &Fixture, command: &str) {
    install_contracts(fixture, &[("project-dev", &[command])]);
}

fn common(fixture: &Fixture, session_id: &str, turn_id: &str) -> Value {
    json!({
        "product": "dsh",
        "session_id": session_id,
        "turn_id": turn_id,
        "cwd": fixture.root,
    })
}

fn begin_edit(fixture: &Fixture, session_id: &str, turn_id: &str, operation_id: &str) -> Value {
    let mut request = common(fixture, session_id, turn_id);
    request["schema_version"] = json!("agent-hook.finish-line.begin.v1");
    request["operation_id"] = json!(operation_id);
    request["attempt_token"] = json!(format!("edit-token:{session_id}:{operation_id}"));
    request["operation"] = json!({"kind": "edit"});
    let output = fixture.run(
        &["finish-line", "begin", "--format", "json"],
        Some(&request.to_string()),
    );
    assert_eq!(
        output.code,
        0,
        "stdout={} stderr={}",
        output.stdout_text(),
        output.stderr_text(),
    );
    output.stdout_json()["data"].clone()
}

fn open_runner(fixture: &Fixture, session_id: &str, turn_id: &str) -> String {
    let (code, envelope) = open_runner_with_token(
        fixture,
        session_id,
        turn_id,
        &format!("open-token:{session_id}"),
    );
    assert_eq!(code, 0, "envelope={envelope}");
    envelope["data"]["runner_capability"]
        .as_str()
        .expect("runner capability")
        .to_string()
}

fn open_runner_with_token(
    fixture: &Fixture,
    session_id: &str,
    turn_id: &str,
    attempt_token: &str,
) -> (i32, Value) {
    let mut request = common(fixture, session_id, turn_id);
    request["schema_version"] = json!("agent-hook.finish-line.open.v1");
    request["attempt_token"] = json!(attempt_token);
    let output = fixture.run(
        &["finish-line", "open", "--format", "json"],
        Some(&request.to_string()),
    );
    (output.code, output.stdout_json())
}

fn release_runner(
    fixture: &Fixture,
    session_id: &str,
    turn_id: &str,
    runner_capability: &str,
) -> (i32, Value) {
    let mut request = common(fixture, session_id, turn_id);
    request["schema_version"] = json!("agent-hook.finish-line.release.v1");
    request["runner_capability"] = json!(runner_capability);
    let output = fixture.run(
        &["finish-line", "release", "--format", "json"],
        Some(&request.to_string()),
    );
    (output.code, output.stdout_json())
}

fn run_validation(
    fixture: &Fixture,
    session_id: &str,
    turn_id: &str,
    operation_id: &str,
    intent: &str,
    command: &str,
    timeout_ms: u64,
) -> (i32, Value) {
    run_validation_in_workdir(
        fixture,
        session_id,
        turn_id,
        operation_id,
        intent,
        command,
        timeout_ms,
        &fixture.root,
    )
}

#[allow(clippy::too_many_arguments)]
fn run_validation_in_workdir(
    fixture: &Fixture,
    session_id: &str,
    turn_id: &str,
    operation_id: &str,
    intent: &str,
    command: &str,
    timeout_ms: u64,
    workdir: &Path,
) -> (i32, Value) {
    let runner_capability = open_runner(fixture, session_id, turn_id);
    let mut request = common(fixture, session_id, turn_id);
    request["schema_version"] = json!("agent-hook.finish-line.run.v1");
    request["operation_id"] = json!(operation_id);
    request["intent"] = json!(intent);
    request["command"] = json!(command);
    request["runner_capability"] = json!(runner_capability);
    request["timeout_ms"] = json!(timeout_ms);
    request["execution"] = json!({
        "kind": "bash-v1",
        "workdir": workdir,
        "output_max_bytes": 64 * 1024,
        "runner": {"kind": "danger-full-access"},
    });
    let output = fixture.run(
        &["finish-line", "run", "--format", "json"],
        Some(&request.to_string()),
    );
    (output.code, output.stdout_json())
}

fn stop(fixture: &Fixture, session_id: &str) -> (i32, Value) {
    let mut request = common(fixture, session_id, "turn-stop");
    request["schema_version"] = json!("agent-hook.finish-line.stop.v1");
    let output = fixture.run(
        &["finish-line", "stop", "--format", "json"],
        Some(&request.to_string()),
    );
    (output.code, output.stdout_json())
}

fn status(fixture: &Fixture, session_id: &str) -> (i32, Value) {
    let mut request = common(fixture, session_id, "turn-status");
    request["schema_version"] = json!("agent-hook.finish-line.status.v1");
    let output = fixture.run(
        &["finish-line", "status", "--format", "json"],
        Some(&request.to_string()),
    );
    (output.code, output.stdout_json())
}

fn reason_codes(envelope: &Value) -> Vec<&str> {
    envelope["data"]["reason_codes"]
        .as_array()
        .expect("reason codes")
        .iter()
        .map(|value| value.as_str().expect("reason code"))
        .collect()
}

fn finish_line_state_path(fixture: &Fixture) -> std::path::PathBuf {
    fs::read_dir(fixture.state_home.join("agent-hook/finish-line/repos"))
        .expect("finish-line repo state")
        .map(|entry| entry.expect("repo state entry").path())
        .find(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .expect("finish-line state path")
}

#[test]
fn nils_executes_validation_and_removes_caller_reported_outcomes_and_waivers() {
    let fixture = fixture();
    let command = "printf 'executed\\n' > validation.marker";
    one_contract(&fixture, command);

    let edit = begin_edit(&fixture, "session-a", "turn-1", "edit-1");
    assert_eq!(edit["generation"], 1);
    let (code, envelope) = run_validation(
        &fixture,
        "session-a",
        "turn-1",
        "validation-1",
        "project-dev",
        command,
        5_000,
    );
    assert_eq!(code, 0, "envelope={envelope}");
    assert_eq!(
        envelope["schema_version"],
        "cli.agent-hook.finish-line-run.v1"
    );
    assert_eq!(envelope["data"]["status"], "applied");
    assert_eq!(
        envelope["data"]["execution"]["exit_code"], 0,
        "envelope={envelope}"
    );
    assert_eq!(
        fs::read_to_string(fixture.root.join("validation.marker"))
            .unwrap_or_else(|error| panic!("validation marker: {error}; envelope={envelope}")),
        "executed\n",
    );
    assert_eq!(stop(&fixture, "session-a").0, 0);

    let help = fixture.run(&["finish-line", "--help"], None);
    assert_eq!(help.code, 0);
    let help = help.stdout_text();
    assert!(help.contains("run"), "{help}");
    assert!(!help.contains("complete"), "{help}");
    assert!(!help.contains("waive"), "{help}");
    assert!(!help.contains("approve"), "{help}");
    assert!(!help.contains("revoke"), "{help}");
}

#[test]
fn durable_edit_generation_records_compact_instead_of_exhausting_the_state_limit() {
    let fixture = fixture();
    one_contract(&fixture, ":");

    for index in 0..513 {
        let data = begin_edit(
            &fixture,
            "session-a",
            &format!("turn-{index}"),
            &format!("edit-{index}"),
        );
        assert_eq!(data["generation"], index + 1);
    }

    let state: Value = serde_json::from_slice(
        &fs::read(finish_line_state_path(&fixture)).expect("finish-line state"),
    )
    .expect("finish-line state JSON");
    assert!(
        state["operations"].as_object().expect("operations").len() < 256,
        "state={state}",
    );
}

#[test]
fn released_sessions_do_not_exhaust_the_repository_session_limit() {
    let fixture = fixture();
    one_contract(&fixture, ":");

    for index in 0..80 {
        let session_id = format!("session-{index}");
        let capability = open_runner(&fixture, &session_id, "turn-1");
        let (code, envelope) = release_runner(&fixture, &session_id, "turn-1", &capability);
        assert_eq!(code, 0, "index={index} envelope={envelope}");
        assert_eq!(envelope["data"]["status"], "released");
        if index == 0 {
            let (retry_code, retry) = release_runner(&fixture, &session_id, "turn-1", &capability);
            assert_eq!(retry_code, 0, "retry={retry}");
            assert_eq!(retry["data"]["status"], "duplicate");
        }
    }

    let state: Value = serde_json::from_slice(
        &fs::read(finish_line_state_path(&fixture)).expect("finish-line state"),
    )
    .expect("finish-line state JSON");
    assert!(
        state["sessions"].as_object().expect("sessions").is_empty(),
        "state={state}",
    );
}

#[test]
fn tombstone_churn_never_resurrects_a_released_capability() {
    let fixture = fixture();
    one_contract(&fixture, ":");

    let (opened_code, opened) =
        open_runner_with_token(&fixture, "session-retired", "turn-1", "open-token:retired");
    assert_eq!(opened_code, 0, "envelope={opened}");
    let retired_capability = opened["data"]["runner_capability"]
        .as_str()
        .expect("retired runner capability")
        .to_string();
    let (release_code, released) =
        release_runner(&fixture, "session-retired", "turn-1", &retired_capability);
    assert_eq!(release_code, 0, "envelope={released}");

    for index in 0..80 {
        let session_id = format!("session-churn-{index}");
        let capability = open_runner(&fixture, &session_id, "turn-1");
        let (code, envelope) = release_runner(&fixture, &session_id, "turn-1", &capability);
        assert_eq!(code, 0, "index={index} envelope={envelope}");
    }

    let (replay_code, replayed) =
        open_runner_with_token(&fixture, "session-retired", "turn-2", "open-token:retired");
    assert_eq!(replay_code, 0, "envelope={replayed}");
    assert_eq!(replayed["data"]["status"], "opened");
    let replacement_capability = replayed["data"]["runner_capability"]
        .as_str()
        .expect("replacement runner capability")
        .to_string();
    assert_ne!(replacement_capability, retired_capability);

    let (old_release_code, old_release) =
        release_runner(&fixture, "session-retired", "turn-3", &retired_capability);
    assert_eq!(old_release_code, 65, "envelope={old_release}");
    assert_eq!(
        old_release["error"]["code"],
        "finish-line-runner-capability-invalid"
    );
    let (retry_code, retry) =
        open_runner_with_token(&fixture, "session-retired", "turn-3", "open-token:retired");
    assert_eq!(retry_code, 0, "envelope={retry}");
    assert_eq!(retry["data"]["status"], "duplicate");
    assert_eq!(retry["data"]["runner_capability"], replacement_capability);

    let (replacement_release_code, replacement_release) = release_runner(
        &fixture,
        "session-retired",
        "turn-4",
        &replacement_capability,
    );
    assert_eq!(
        replacement_release_code, 0,
        "envelope={replacement_release}"
    );
    assert_eq!(replacement_release["data"]["status"], "released");
}

#[test]
fn open_is_retry_safe_and_cannot_take_over_a_live_session() {
    let fixture = fixture();
    one_contract(&fixture, ":");

    let (code, opened) =
        open_runner_with_token(&fixture, "session-a", "turn-1", "open-token:owner-a");
    assert_eq!(code, 0, "envelope={opened}");
    assert_eq!(opened["data"]["status"], "opened");
    let capability = opened["data"]["runner_capability"]
        .as_str()
        .expect("runner capability")
        .to_string();

    let (retry_code, retry) =
        open_runner_with_token(&fixture, "session-a", "turn-2", "open-token:owner-a");
    assert_eq!(retry_code, 0, "envelope={retry}");
    assert_eq!(retry["data"]["status"], "duplicate");
    assert_eq!(retry["data"]["runner_capability"], capability);

    let (takeover_code, takeover) =
        open_runner_with_token(&fixture, "session-a", "turn-3", "open-token:attacker");
    assert_eq!(takeover_code, 65, "envelope={takeover}");
    assert_eq!(takeover["error"]["code"], "finish-line-session-active");

    let (release_code, released) = release_runner(&fixture, "session-a", "turn-4", &capability);
    assert_eq!(release_code, 0, "envelope={released}");
    assert_eq!(released["data"]["status"], "released");

    let resumed_edit = begin_edit(&fixture, "session-a", "turn-5", "edit-after-resume");
    assert_eq!(resumed_edit["status"], "registered");
    let (resume_code, resumed) =
        open_runner_with_token(&fixture, "session-a", "turn-5", "open-token:owner-a");
    assert_eq!(resume_code, 0, "envelope={resumed}");
    assert_eq!(resumed["data"]["status"], "opened");
    let resumed_capability = resumed["data"]["runner_capability"]
        .as_str()
        .expect("resumed runner capability")
        .to_string();
    assert_ne!(resumed_capability, capability);

    let (old_release_code, old_release) =
        release_runner(&fixture, "session-a", "turn-6", &capability);
    assert_eq!(old_release_code, 0, "envelope={old_release}");
    assert_eq!(old_release["data"]["status"], "duplicate");
    let (resume_retry_code, resume_retry) =
        open_runner_with_token(&fixture, "session-a", "turn-6", "open-token:owner-a");
    assert_eq!(resume_retry_code, 0, "envelope={resume_retry}");
    assert_eq!(resume_retry["data"]["status"], "duplicate");
    assert_eq!(
        resume_retry["data"]["runner_capability"],
        resumed_capability
    );

    let (second_takeover_code, second_takeover) =
        open_runner_with_token(&fixture, "session-a", "turn-7", "open-token:attacker-2");
    assert_eq!(second_takeover_code, 65, "envelope={second_takeover}");
    assert_eq!(
        second_takeover["error"]["code"],
        "finish-line-session-active"
    );

    let (resumed_release_code, resumed_release) =
        release_runner(&fixture, "session-a", "turn-8", &resumed_capability);
    assert_eq!(resumed_release_code, 0, "envelope={resumed_release}");
    assert_eq!(resumed_release["data"]["status"], "released");
}

#[test]
fn expired_quiescent_sessions_are_reclaimed_but_pending_sessions_remain_protected() {
    let fixture = fixture();
    one_contract(&fixture, ":");

    let (code, opened) =
        open_runner_with_token(&fixture, "session-pending", "turn-1", "open-token:pending");
    assert_eq!(code, 0, "envelope={opened}");
    let state_path = finish_line_state_path(&fixture);
    let pending_key =
        serde_json::from_slice::<Value>(&fs::read(&state_path).expect("finish-line state"))
            .expect("finish-line state JSON")["sessions"]
            .as_object()
            .expect("sessions")
            .keys()
            .next()
            .expect("pending session key")
            .clone();
    for index in 0..63 {
        let session_id = format!("session-expired-{index}");
        let (code, envelope) = open_runner_with_token(
            &fixture,
            &session_id,
            "turn-1",
            &format!("open-token:expired-{index}"),
        );
        assert_eq!(code, 0, "index={index} envelope={envelope}");
    }

    let mut state: Value =
        serde_json::from_slice(&fs::read(&state_path).expect("finish-line state"))
            .expect("finish-line state JSON");
    for session in state["sessions"]
        .as_object_mut()
        .expect("sessions")
        .values_mut()
    {
        session["capability_lease_expires_at_epoch"] = json!(0);
    }
    state["operations"]["pending-orphan-test"] = json!({
        "session_key": pending_key,
        "turn_key": "sha256:pending-turn",
        "token_digest": "sha256:pending-token",
        "generation": 0,
        "sequence": 1,
        "kind": "validation",
        "target_digest": null,
        "contract_digest": null,
        "terminal": null,
        "active_unit": "nils-finish-line-pending-orphan-test",
    });
    fs::write(
        &state_path,
        serde_json::to_vec(&state).expect("serialize expired state"),
    )
    .expect("write expired state");

    let (new_code, new_session) = open_runner_with_token(
        &fixture,
        "session-after-reclaim",
        "turn-1",
        "open-token:after-reclaim",
    );
    assert_eq!(new_code, 0, "envelope={new_session}");

    let compacted: Value =
        serde_json::from_slice(&fs::read(&state_path).expect("compacted finish-line state"))
            .expect("compacted finish-line state JSON");
    assert_eq!(
        compacted["sessions"].as_object().expect("sessions").len(),
        64,
        "capacity recovery must retire only the one session needed for admission"
    );
    assert!(compacted["sessions"].get(&pending_key).is_some());
    assert!(compacted["operations"].get("pending-orphan-test").is_some());
    assert_eq!(
        compacted["released_sessions"]
            .as_object()
            .expect("released sessions")
            .len(),
        1,
        "capacity recovery must leave an authenticated retirement receipt"
    );
}

#[test]
fn expired_crash_orphans_reclaim_only_after_trusted_unit_quiescence() {
    let fixture = fixture();
    one_contract(&fixture, ":");

    let mut capabilities = Vec::new();
    for index in 0..64 {
        let session_id = format!("session-orphan-{index}");
        let (code, opened) = open_runner_with_token(
            &fixture,
            &session_id,
            "turn-1",
            &format!("open-token:orphan-{index}"),
        );
        assert_eq!(code, 0, "index={index} envelope={opened}");
        capabilities.push(
            opened["data"]["runner_capability"]
                .as_str()
                .expect("runner capability")
                .to_string(),
        );
    }

    let state_path = finish_line_state_path(&fixture);
    let mut state: Value =
        serde_json::from_slice(&fs::read(&state_path).expect("finish-line state"))
            .expect("finish-line state JSON");
    let session_keys = state["sessions"]
        .as_object()
        .expect("sessions")
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    for (index, session_key) in session_keys.iter().enumerate() {
        state["sessions"][session_key]["capability_lease_expires_at_epoch"] = json!(0);
        state["operations"][format!("crash-orphan-{index}")] = json!({
            "session_key": session_key,
            "turn_key": format!("sha256:orphan-turn-{index}"),
            "token_digest": format!("sha256:orphan-token-{index}"),
            "generation": 0,
            "sequence": index + 1,
            "kind": "validation",
            "target_digest": null,
            "contract_digest": null,
            "terminal": null,
            "active_unit": format!("nils-finish-line-{index:032x}"),
        });
    }
    fs::write(
        &state_path,
        serde_json::to_vec(&state).expect("serialize crash-orphan state"),
    )
    .expect("write crash-orphan state");

    let edit = begin_edit(
        &fixture,
        "session-after-crash-orphans",
        "turn-1",
        "edit-after-crash-orphans",
    );
    assert_eq!(edit["status"], "registered");

    let compacted: Value =
        serde_json::from_slice(&fs::read(&state_path).expect("reclaimed finish-line state"))
            .expect("reclaimed finish-line state JSON");
    assert_eq!(
        compacted["sessions"].as_object().expect("sessions").len(),
        64
    );
    assert_eq!(
        compacted["operations"]
            .as_object()
            .expect("operations")
            .len(),
        64
    );
    assert_eq!(
        compacted["released_sessions"]
            .as_object()
            .expect("released sessions")
            .len(),
        1
    );

    let mut reclaimed_index = None;
    let mut still_active = 0;
    for (index, capability) in capabilities.iter().enumerate() {
        let (code, envelope) = release_runner(
            &fixture,
            &format!("session-orphan-{index}"),
            "turn-2",
            capability,
        );
        match (
            code,
            envelope["data"]["status"].as_str(),
            envelope["error"]["code"].as_str(),
        ) {
            (0, Some("duplicate"), None) => reclaimed_index = Some(index),
            (75, None, Some("finish-line-session-busy")) => still_active += 1,
            other => panic!("unexpected crash-orphan release result: {other:?}"),
        }
    }
    let reclaimed_index = reclaimed_index.expect("one reclaimed crash orphan");
    assert_eq!(still_active, 63);

    let resumed_token = format!("open-token:replacement-{reclaimed_index}");
    let (resume_code, resumed) = open_runner_with_token(
        &fixture,
        &format!("session-orphan-{reclaimed_index}"),
        "turn-3",
        &resumed_token,
    );
    assert_eq!(resume_code, 0, "envelope={resumed}");
    assert_eq!(resumed["data"]["status"], "opened");
    let (old_release_code, old_release) = release_runner(
        &fixture,
        &format!("session-orphan-{reclaimed_index}"),
        "turn-4",
        &capabilities[reclaimed_index],
    );
    assert_eq!(old_release_code, 0, "envelope={old_release}");
    assert_eq!(old_release["data"]["status"], "duplicate");
    let (retry_code, retry) = open_runner_with_token(
        &fixture,
        &format!("session-orphan-{reclaimed_index}"),
        "turn-4",
        &resumed_token,
    );
    assert_eq!(retry_code, 0, "envelope={retry}");
    assert_eq!(retry["data"]["status"], "duplicate");
}

#[test]
fn release_is_authenticated_and_cannot_reclaim_a_pending_session() {
    let fixture = fixture();
    one_contract(&fixture, ":");
    let capability = open_runner(&fixture, "session-a", "turn-1");

    let (code, rejected) =
        release_runner(&fixture, "session-a", "turn-1", "finish-line-runner:wrong");
    assert_eq!(code, 65, "envelope={rejected}");
    assert_eq!(
        rejected["error"]["code"],
        "finish-line-runner-capability-invalid"
    );

    let state_path = finish_line_state_path(&fixture);
    let mut state: Value =
        serde_json::from_slice(&fs::read(&state_path).expect("finish-line state"))
            .expect("finish-line state JSON");
    let session_key = state["sessions"]
        .as_object()
        .expect("sessions")
        .keys()
        .next()
        .expect("session key")
        .clone();
    state["operations"]["pending-release-test"] = json!({
        "session_key": session_key,
        "turn_key": "sha256:pending-turn",
        "token_digest": "sha256:pending-token",
        "generation": 0,
        "sequence": 1,
        "kind": "validation",
        "target_digest": null,
        "contract_digest": null,
        "terminal": null,
        "active_unit": "nils-finish-line-pending-test",
    });
    fs::write(
        &state_path,
        serde_json::to_vec(&state).expect("serialize pending state"),
    )
    .expect("write pending state");

    let (code, busy) = release_runner(&fixture, "session-a", "turn-1", &capability);
    assert_eq!(code, 75, "envelope={busy}");
    assert_eq!(busy["error"]["code"], "finish-line-session-busy");

    state["operations"]
        .as_object_mut()
        .expect("operations")
        .remove("pending-release-test");
    fs::write(
        &state_path,
        serde_json::to_vec(&state).expect("serialize quiescent state"),
    )
    .expect("write quiescent state");
    let (code, released) = release_runner(&fixture, "session-a", "turn-1", &capability);
    assert_eq!(code, 0, "envelope={released}");
    assert_eq!(released["data"]["status"], "released");
}

#[test]
fn deleting_initialized_repository_state_fails_closed_instead_of_resetting_generation() {
    let fixture = fixture();
    one_contract(&fixture, ":");
    begin_edit(&fixture, "session-a", "turn-1", "edit-1");
    assert_eq!(stop(&fixture, "session-a").0, 1);

    fs::remove_file(finish_line_state_path(&fixture)).expect("delete finish-line state");
    let (code, envelope) = stop(&fixture, "session-a");
    assert_eq!(code, 65, "envelope={envelope}");
    assert_eq!(
        envelope["error"]["code"], "finish-line-state-missing",
        "envelope={envelope}",
    );
}

#[test]
fn nils_executes_the_provider_confined_argv_and_rejects_command_substitution() {
    let fixture = fixture();
    let command = "printf 'provider-ran\\n' > provider.marker";
    one_contract(&fixture, command);
    begin_edit(&fixture, "session-a", "turn-1", "edit-1");
    let runner_capability = open_runner(&fixture, "session-a", "turn-1");
    let provider = fixture.root.join("provider-runner");
    fs::write(
        &provider,
        "#!/bin/sh\n[ \"$1\" = -- ] || exit 125\nshift\nexec \"$@\"\n",
    )
    .expect("provider runner");
    fs::set_permissions(&provider, fs::Permissions::from_mode(0o700))
        .expect("provider permissions");

    let mut request = common(&fixture, "session-a", "turn-1");
    request["schema_version"] = json!("agent-hook.finish-line.run.v1");
    request["operation_id"] = json!("validation-confined");
    request["intent"] = json!("project-dev");
    request["command"] = json!(command);
    request["runner_capability"] = json!(runner_capability);
    request["timeout_ms"] = json!(5_000);
    request["execution"] = json!({
        "kind": "bash-v1",
        "workdir": fixture.root,
        "output_max_bytes": 64 * 1024,
        "runner": {
            "kind": "confined",
            "argv": [provider, "--", "bash", "-c", command],
            "mode": "workspace-write",
            "enforcement": "full",
            "denial_signatures": ["read-only file system"],
            "runner_failure_rules": [],
        },
    });
    let output = fixture.run(
        &["finish-line", "run", "--format", "json"],
        Some(&request.to_string()),
    );
    assert_eq!(
        output.code,
        0,
        "stdout={} stderr={}",
        output.stdout_text(),
        output.stderr_text(),
    );
    assert_eq!(
        output.stdout_json()["data"]["execution"]["exit_code"],
        0,
        "envelope={}",
        output.stdout_json(),
    );
    assert_eq!(
        output.stdout_json()["data"]["execution"]["sandbox"],
        json!({"mode": "workspace-write", "denied": false, "enforcement": "full"}),
    );
    assert_eq!(
        fs::read_to_string(fixture.root.join("provider.marker")).expect("provider marker"),
        "provider-ran\n",
    );

    request["operation_id"] = json!("validation-substitution");
    request["execution"]["runner"]["argv"][4] = json!("touch substituted.marker");
    let output = fixture.run(
        &["finish-line", "run", "--format", "json"],
        Some(&request.to_string()),
    );
    assert_eq!(output.code, 65);
    assert_eq!(
        output.stdout_json()["error"]["code"],
        "finish-line-provider-argv-invalid",
    );
    assert!(!fixture.root.join("substituted.marker").exists());
}

#[test]
fn probe_is_non_executing_and_sandbox_runner_failure_is_not_validation_evidence() {
    let fixture = fixture();
    let command = ":";
    one_contract(&fixture, command);
    begin_edit(&fixture, "session-a", "turn-1", "edit-1");
    let runner_capability = open_runner(&fixture, "session-a", "turn-1");

    let mut request = common(&fixture, "session-a", "turn-1");
    request["schema_version"] = json!("agent-hook.finish-line.run.v1");
    request["operation_id"] = json!("validation-probe");
    request["intent"] = json!("project-dev");
    request["command"] = json!(command);
    request["runner_capability"] = json!(runner_capability);
    request["timeout_ms"] = json!(1);
    let probe = fixture.run(
        &["finish-line", "run", "--format", "json"],
        Some(&request.to_string()),
    );
    assert_eq!(probe.code, 0, "envelope={}", probe.stdout_json());
    assert_eq!(probe.stdout_json()["data"]["status"], "ready");
    assert_eq!(stop(&fixture, "session-a").0, 1);

    let provider = fixture.root.join("failing-provider-runner");
    fs::write(
        &provider,
        "#!/bin/sh\nprintf 'fake-runner: profile rejected\\n' >&2\nexit 125\n",
    )
    .expect("failing provider runner");
    fs::set_permissions(&provider, fs::Permissions::from_mode(0o700))
        .expect("provider permissions");
    request["operation_id"] = json!("validation-runner-failed");
    request["timeout_ms"] = json!(5_000);
    request["execution"] = json!({
        "kind": "bash-v1",
        "workdir": fixture.root,
        "output_max_bytes": 64 * 1024,
        "runner": {
            "kind": "confined",
            "argv": [provider, "--", "bash", "-c", command],
            "mode": "workspace-write",
            "enforcement": "full",
            "denial_signatures": ["permission denied"],
            "runner_failure_rules": [{
                "allowed_exit_codes": [125],
                "fatal_signatures": ["fake-runner: "],
                "informational_lines": [],
            }],
        },
    });
    let failed = fixture.run(
        &["finish-line", "run", "--format", "json"],
        Some(&request.to_string()),
    );
    assert_ne!(failed.code, 0);
    assert_eq!(
        failed.stdout_json()["error"]["code"],
        "finish-line-sandbox-runner-failed",
    );
    let (_, blocked) = stop(&fixture, "session-a");
    assert!(reason_codes(&blocked).contains(&"validation-pending"));
    assert!(!reason_codes(&blocked).contains(&"validation-failed"));

    let mut quiesce = common(&fixture, "session-a", "turn-1");
    quiesce["schema_version"] = json!("agent-hook.finish-line.quiesce.v1");
    quiesce["operation_id"] = json!("validation-runner-failed");
    quiesce["runner_capability"] = json!(runner_capability);
    let quiesced = fixture.run(
        &["finish-line", "quiesce", "--format", "json"],
        Some(&quiesce.to_string()),
    );
    assert_eq!(quiesced.code, 0, "envelope={}", quiesced.stdout_json());
    let (_, missing) = stop(&fixture, "session-a");
    assert!(reason_codes(&missing).contains(&"validation-missing"));

    let (_, recovered) = run_validation(
        &fixture,
        "session-a",
        "turn-2",
        "validation-recovered",
        "project-dev",
        command,
        5_000,
    );
    assert_eq!(recovered["data"]["execution"]["exit_code"], 0);
    assert_eq!(stop(&fixture, "session-a").0, 0);
}

#[test]
fn confined_denial_is_recorded_as_a_failed_validation_with_provider_facts() {
    let fixture = fixture();
    let command = "printf 'read-only file system\\n' >&2; exit 9";
    one_contract(&fixture, command);
    begin_edit(&fixture, "session-a", "turn-1", "edit-1");
    let runner_capability = open_runner(&fixture, "session-a", "turn-1");
    let provider = fixture.root.join("passing-provider-runner");
    fs::write(
        &provider,
        "#!/bin/sh\n[ \"$1\" = -- ] || exit 125\nshift\nexec \"$@\"\n",
    )
    .expect("provider runner");
    fs::set_permissions(&provider, fs::Permissions::from_mode(0o700))
        .expect("provider permissions");

    let mut request = common(&fixture, "session-a", "turn-1");
    request["schema_version"] = json!("agent-hook.finish-line.run.v1");
    request["operation_id"] = json!("validation-denied");
    request["intent"] = json!("project-dev");
    request["command"] = json!(command);
    request["runner_capability"] = json!(runner_capability);
    request["timeout_ms"] = json!(5_000);
    request["execution"] = json!({
        "kind": "bash-v1",
        "workdir": fixture.root,
        "output_max_bytes": 64 * 1024,
        "runner": {
            "kind": "confined",
            "argv": [provider, "--", "bash", "-c", command],
            "mode": "read-only",
            "enforcement": "partial",
            "denial_signatures": ["read-only file system"],
            "runner_failure_rules": [],
        },
    });
    let denied = fixture.run(
        &["finish-line", "run", "--format", "json"],
        Some(&request.to_string()),
    );
    assert_eq!(denied.code, 0, "envelope={}", denied.stdout_json());
    assert_eq!(
        denied.stdout_json()["data"]["execution"]["sandbox"],
        json!({"mode": "read-only", "denied": true, "enforcement": "partial"}),
    );
    let (code, blocked) = stop(&fixture, "session-a");
    assert_eq!(code, 1);
    assert!(reason_codes(&blocked).contains(&"validation-failed"));
}

#[test]
fn observed_exit_and_output_drive_failure_then_exact_retry_success() {
    let fixture = fixture();
    let failing = "printf 'out\\n'; printf 'err\\n' >&2; exit 202";
    one_contract(&fixture, failing);
    begin_edit(&fixture, "session-a", "turn-1", "edit-1");

    let (code, failed) = run_validation(
        &fixture,
        "session-a",
        "turn-1",
        "validation-failed",
        "project-dev",
        failing,
        5_000,
    );
    assert_eq!(code, 0, "envelope={failed}");
    assert_eq!(failed["data"]["execution"]["exit_code"], 202);
    assert_eq!(failed["data"]["execution"]["stdout"]["text"], "out\n");
    assert_eq!(failed["data"]["execution"]["stderr"]["text"], "err\n");
    let (code, blocked) = stop(&fixture, "session-a");
    assert_eq!(code, 1);
    assert!(reason_codes(&blocked).contains(&"validation-failed"));

    let passing = "printf 'recovered\\n'";
    one_contract(&fixture, passing);
    let (code, passed) = run_validation(
        &fixture,
        "session-a",
        "turn-2",
        "validation-passed",
        "project-dev",
        passing,
        5_000,
    );
    assert_eq!(code, 0, "envelope={passed}");
    assert_eq!(passed["data"]["execution"]["exit_code"], 0);
    assert_eq!(stop(&fixture, "session-a").0, 0);
}

#[test]
fn provider_exit_and_signal_facts_do_not_collide_with_runner_control() {
    let fixture = fixture();
    one_contract(&fixture, ":");
    for (index, command, expected_exit, expected_signal) in [
        (0, "exit 134", Some(134), None),
        (1, "exit 204", Some(204), None),
        (2, "kill -ABRT $$", None, Some("SIGABRT")),
        (3, "kill -SEGV $$", None, Some("SIGSEGV")),
        (4, "kill -PIPE $$", None, Some("SIGPIPE")),
    ] {
        let (code, envelope) = run_validation(
            &fixture,
            "session-a",
            &format!("turn-{index}"),
            &format!("provider-fact-{index}"),
            "project-dev",
            command,
            5_000,
        );
        assert_eq!(code, 0, "command={command:?} envelope={envelope}");
        assert_eq!(
            envelope["data"]["execution"]["exit_code"],
            expected_exit.map_or(Value::Null, Value::from),
            "command={command:?} envelope={envelope}",
        );
        assert_eq!(
            envelope["data"]["execution"]["signal"],
            expected_signal.map_or(Value::Null, Value::from),
            "command={command:?} envelope={envelope}",
        );
    }
}

#[test]
fn provider_cannot_reopen_finish_line_memfds_through_procfs() {
    let fixture = fixture();
    one_contract(&fixture, ":");
    let runner_capability = open_runner(&fixture, "session-a", "turn-1");
    let mut child = Command::new(env!("CARGO_BIN_EXE_agent-hook"))
        .args(["finish-line", "run", "--format", "json"])
        .current_dir(&fixture.root)
        .env("HOME", &fixture.home)
        .env("XDG_CONFIG_HOME", &fixture.config_home)
        .env("XDG_STATE_HOME", &fixture.state_home)
        .env("AGENT_SESSION_STATE_DIR", &fixture.session_state)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn validation supervisor");
    let command = format!(
        r#"for process in /proc/{} /proc/$PPID; do
  for descriptor in "$process"/fd/*; do
    target=$(readlink "$descriptor" 2>/dev/null) || continue
    case "$target" in
      *memfd:nils-finish-line-config*) if exec 9<"$descriptor"; then exit 78; fi ;;
      *memfd:nils-finish-line-control*) if exec 9<>"$descriptor"; then exit 77; fi ;;
    esac
  done
done
exit 23"#,
        child.id(),
    );
    let mut request = common(&fixture, "session-a", "turn-1");
    request["schema_version"] = json!("agent-hook.finish-line.run.v1");
    request["operation_id"] = json!("provider-control-reopen");
    request["intent"] = json!("project-dev");
    request["command"] = json!(command);
    request["runner_capability"] = json!(runner_capability);
    request["timeout_ms"] = json!(5_000);
    request["execution"] = json!({
        "kind": "bash-v1",
        "workdir": fixture.root,
        "output_max_bytes": 64 * 1024,
        "runner": {"kind": "danger-full-access"},
    });
    child
        .stdin
        .take()
        .expect("validation stdin")
        .write_all(request.to_string().as_bytes())
        .expect("write validation request");
    let output = child.wait_with_output().expect("validation output");
    let envelope: Value = serde_json::from_slice(&output.stdout).expect("validation envelope");
    assert!(
        output.status.success(),
        "envelope={envelope} stderr={}",
        String::from_utf8_lossy(&output.stderr),
    );
    assert_eq!(
        envelope["data"]["execution"]["exit_code"], 23,
        "provider reopened a trusted finish-line memfd: {envelope}",
    );
}

#[test]
fn non_contract_foreground_shell_is_supervised_once_and_invalidates_validation_evidence() {
    let fixture = fixture();
    one_contract(&fixture, ":");
    begin_edit(&fixture, "session-a", "turn-1", "edit-1");
    let nested = fixture.root.join("nested-workdir");
    fs::create_dir(&nested).expect("ordinary nested workdir");
    let unrelated = "printf x >> ordinary-executions";
    let (code, envelope) = run_validation_in_workdir(
        &fixture,
        "session-a",
        "turn-1",
        "unrelated",
        "project-dev",
        unrelated,
        5_000,
        &nested,
    );
    assert_eq!(code, 0, "envelope={envelope}");
    assert_eq!(envelope["data"]["status"], "ordinary-applied");
    assert_eq!(envelope["data"]["generation"], 2);
    assert_eq!(
        fs::read_to_string(nested.join("ordinary-executions")).expect("ordinary execution marker"),
        "x",
    );
    let (_, duplicate) = run_validation_in_workdir(
        &fixture,
        "session-a",
        "turn-1",
        "unrelated",
        "project-dev",
        unrelated,
        5_000,
        &nested,
    );
    assert_eq!(duplicate["data"]["status"], "duplicate");
    assert_eq!(
        fs::read_to_string(nested.join("ordinary-executions")).expect("ordinary execution marker"),
        "x",
    );
    let (code, blocked) = stop(&fixture, "session-a");
    assert_eq!(code, 1);
    assert!(reason_codes(&blocked).contains(&"validation-missing"));
}

#[test]
fn ordinary_shell_cannot_leave_a_same_group_descendant_to_mutate_after_return() {
    let fixture = fixture();
    one_contract(&fixture, ":");
    let command = "nohup sh -c 'sleep 0.2; printf late > late-mutation' >/dev/null 2>&1 &";
    let (code, envelope) = run_validation(
        &fixture,
        "session-a",
        "turn-1",
        "ordinary-descendant",
        "project-dev",
        command,
        5_000,
    );
    assert_eq!(code, 0, "envelope={envelope}");
    assert_eq!(envelope["data"]["status"], "ordinary-applied");

    std::thread::sleep(std::time::Duration::from_millis(400));
    assert!(
        !fixture.root.join("late-mutation").exists(),
        "ordinary shell descendant mutated the repository after nils returned",
    );
}

#[cfg(target_os = "linux")]
#[test]
fn ordinary_shell_cannot_escape_with_a_new_session_or_double_fork_after_return() {
    let fixture = fixture();
    one_contract(&fixture, ":");
    let command = "setsid sh -c \"sh -c 'sleep 0.2; printf escaped > escaped-mutation' >/dev/null 2>&1 &\" >/dev/null 2>&1 &";
    let (code, envelope) = run_validation(
        &fixture,
        "session-a",
        "turn-1",
        "ordinary-escaped-descendant",
        "project-dev",
        command,
        5_000,
    );
    assert_eq!(code, 0, "envelope={envelope}");
    assert_eq!(envelope["data"]["status"], "ordinary-applied");

    std::thread::sleep(std::time::Duration::from_millis(400));
    assert!(
        !fixture.root.join("escaped-mutation").exists(),
        "new-session descendant mutated the repository after nils returned",
    );
}

#[cfg(target_os = "linux")]
#[test]
fn confined_shell_cannot_delegate_a_late_mutation_to_the_user_manager() {
    let fixture = fixture();
    let command = "systemd-run --user --quiet --collect --unit=finish-line-confined-${BASHPID} -- sh -c 'sleep 0.2; printf escaped > manager-mutation'";
    one_contract(&fixture, command);
    begin_edit(&fixture, "session-a", "turn-1", "edit-1");
    let runner_capability = open_runner(&fixture, "session-a", "turn-1");
    let provider = fixture.root.join("provider-runner");
    fs::write(
        &provider,
        "#!/bin/sh\n[ \"$1\" = -- ] || exit 125\nshift\nexec \"$@\"\n",
    )
    .expect("provider runner");
    fs::set_permissions(&provider, fs::Permissions::from_mode(0o700))
        .expect("provider permissions");

    let mut request = common(&fixture, "session-a", "turn-1");
    request["schema_version"] = json!("agent-hook.finish-line.run.v1");
    request["operation_id"] = json!("validation-confined-manager-escape");
    request["intent"] = json!("project-dev");
    request["command"] = json!(command);
    request["runner_capability"] = json!(runner_capability);
    request["timeout_ms"] = json!(5_000);
    request["execution"] = json!({
        "kind": "bash-v1",
        "workdir": fixture.root,
        "output_max_bytes": 64 * 1024,
        "runner": {
            "kind": "confined",
            "argv": [provider, "--", "bash", "-c", command],
            "mode": "workspace-write",
            "enforcement": "full",
            "denial_signatures": ["address family not supported"],
            "runner_failure_rules": [],
        },
    });
    let output = fixture.run(
        &["finish-line", "run", "--format", "json"],
        Some(&request.to_string()),
    );
    assert_eq!(
        output.code,
        0,
        "stdout={} stderr={}",
        output.stdout_text(),
        output.stderr_text(),
    );
    let envelope = output.stdout_json();
    assert_ne!(envelope["data"]["execution"]["exit_code"], 0);

    std::thread::sleep(std::time::Duration::from_millis(400));
    assert!(
        !fixture.root.join("manager-mutation").exists(),
        "confined shell delegated a late mutation outside its execution unit",
    );
}

#[cfg(target_os = "linux")]
#[test]
fn contained_shell_cannot_migrate_itself_to_the_parent_cgroup() {
    let fixture = fixture();
    one_contract(&fixture, ":");
    let command = "current=$(awk -F: '$1 == 0 { print $3 }' /proc/self/cgroup); parent=${current%/*}; if (printf '%s\\n' $$ > \"/sys/fs/cgroup${parent}/cgroup.procs\") 2>/dev/null; then printf migrated; fi";
    let (code, envelope) = run_validation(
        &fixture,
        "session-a",
        "turn-1",
        "ordinary-control-path-probe",
        "project-dev",
        command,
        5_000,
    );
    assert_eq!(code, 0, "envelope={envelope}");
    assert_eq!(envelope["data"]["status"], "ordinary-applied");
    assert_eq!(envelope["data"]["execution"]["exit_code"], 0);
    assert_eq!(
        envelope["data"]["execution"]["stdout"]["text"], "",
        "contained shell migrated outside its execution unit: {envelope}",
    );
}

#[test]
fn exact_duplicate_is_durable_and_never_reexecutes_the_command() {
    let fixture = fixture();
    let command = "printf x >> executions";
    one_contract(&fixture, command);
    begin_edit(&fixture, "session-a", "turn-1", "edit-1");

    let (_, first) = run_validation(
        &fixture,
        "session-a",
        "turn-1",
        "validation-1",
        "project-dev",
        command,
        5_000,
    );
    assert_eq!(first["data"]["status"], "applied");
    let (_, duplicate) = run_validation(
        &fixture,
        "session-a",
        "turn-1",
        "validation-1",
        "project-dev",
        command,
        5_000,
    );
    assert_eq!(duplicate["data"]["status"], "duplicate");
    assert_eq!(duplicate["data"]["output_replayed"], false);
    assert_eq!(
        fs::read_to_string(fixture.root.join("executions")).unwrap(),
        "x"
    );
}

#[test]
fn edit_during_validation_makes_the_observed_success_stale() {
    let fixture = Arc::new(fixture());
    let command =
        "touch validation-started; while [ ! -e validation-release ]; do sleep 0.01; done";
    one_contract(&fixture, command);
    begin_edit(&fixture, "session-a", "turn-1", "edit-1");

    let runner = {
        let fixture = Arc::clone(&fixture);
        thread::spawn(move || {
            run_validation(
                &fixture,
                "session-a",
                "turn-1",
                "validation-stale",
                "project-dev",
                command,
                5_000,
            )
        })
    };
    let deadline = Instant::now() + Duration::from_secs(2);
    while !fixture.root.join("validation-started").exists() {
        assert!(Instant::now() < deadline, "validation did not start");
        thread::sleep(Duration::from_millis(10));
    }
    let edit = begin_edit(&fixture, "session-a", "turn-2", "edit-2");
    assert_eq!(edit["generation"], 2);
    fs::write(fixture.root.join("validation-release"), "release").unwrap();
    let (code, stale) = runner.join().expect("validation runner");
    assert_eq!(code, 0, "envelope={stale}");
    assert_eq!(stale["data"]["status"], "stale");
    assert_eq!(stop(&fixture, "session-a").0, 1);

    let (_, current) = run_validation(
        &fixture,
        "session-a",
        "turn-2",
        "validation-current",
        "project-dev",
        command,
        5_000,
    );
    assert_eq!(current["data"]["status"], "applied");
    assert_eq!(stop(&fixture, "session-a").0, 0);
}

#[test]
fn evidence_is_session_scoped_and_contract_drift_invalidates_it() {
    let fixture = fixture();
    one_contract(&fixture, ":");
    begin_edit(&fixture, "session-a", "turn-1", "edit-1");
    let (_, passed) = run_validation(
        &fixture,
        "session-a",
        "turn-1",
        "validation-1",
        "project-dev",
        ":",
        5_000,
    );
    assert_eq!(passed["data"]["status"], "applied");
    assert_eq!(stop(&fixture, "session-a").0, 0);
    assert_eq!(stop(&fixture, "session-b").0, 1);

    install_contracts(
        &fixture,
        &[("project-dev", &[":"]), ("browser-test", &["true"])],
    );
    let (code, drifted) = stop(&fixture, "session-a");
    assert_eq!(code, 1);
    assert!(reason_codes(&drifted).contains(&"validation-contract-drift"));
    assert!(reason_codes(&drifted).contains(&"validation-missing"));
}

#[test]
fn an_edit_in_another_session_invalidates_prior_repository_generation_evidence() {
    let fixture = fixture();
    one_contract(&fixture, ":");
    begin_edit(&fixture, "session-a", "turn-1", "edit-a");
    let (code, passed) = run_validation(
        &fixture,
        "session-a",
        "turn-1",
        "validation-a-1",
        "project-dev",
        ":",
        5_000,
    );
    assert_eq!(code, 0, "envelope={passed}");
    assert_eq!(stop(&fixture, "session-a").0, 0);

    let edit = begin_edit(&fixture, "session-b", "turn-2", "edit-b");
    assert_eq!(edit["generation"], 2);
    let (code, stale) = stop(&fixture, "session-a");
    assert_eq!(code, 1);
    assert!(
        reason_codes(&stale)
            .iter()
            .any(|reason| matches!(*reason, "validation-stale" | "validation-missing"))
    );

    let (code, refreshed) = run_validation(
        &fixture,
        "session-a",
        "turn-2",
        "validation-a-2",
        "project-dev",
        ":",
        5_000,
    );
    assert_eq!(code, 0, "envelope={refreshed}");
    assert_eq!(stop(&fixture, "session-a").0, 0);
}

#[test]
fn danger_full_access_preserves_the_host_user_systemd_bus() {
    let fixture = fixture();
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("localhost listener");
    let port = listener.local_addr().expect("listener address").port();
    let host_groups = Command::new("/usr/bin/id")
        .arg("-G")
        .output()
        .expect("host group inventory");
    assert!(host_groups.status.success(), "host group inventory failed");
    let host_groups = String::from_utf8(host_groups.stdout)
        .expect("host groups are utf8")
        .trim()
        .to_string();
    let command = "/usr/bin/systemctl --user show-environment >/dev/null";
    one_contract(&fixture, command);

    let (code, exact) = run_validation(
        &fixture,
        "session-a",
        "turn-1",
        "full-host-exact",
        "project-dev",
        command,
        5_000,
    );
    assert_eq!(code, 0, "envelope={exact}");
    assert_eq!(
        exact["data"]["execution"]["exit_code"], 0,
        "danger-full-access exact validation lost the host user bus: {exact}",
    );

    let ordinary_command = format!(
        "/usr/bin/systemctl --user show-environment >/dev/null; \
         : >/dev/tcp/127.0.0.1/{port}; /usr/bin/id -G",
    );
    let (code, ordinary) = run_validation(
        &fixture,
        "session-a",
        "turn-2",
        "full-host-ordinary",
        "project-dev",
        &ordinary_command,
        5_000,
    );
    assert_eq!(code, 0, "envelope={ordinary}");
    assert_eq!(
        ordinary["data"]["execution"]["exit_code"], 0,
        "danger-full-access ordinary Bash lost the host user bus: {ordinary}",
    );
    assert_eq!(
        ordinary["data"]["execution"]["stdout"]["text"]
            .as_str()
            .expect("ordinary stdout")
            .trim(),
        host_groups,
        "danger-full-access changed the host user's supplementary groups",
    );
}

#[test]
fn quiesce_recovers_a_killed_supervisor_and_prevents_late_mutation() {
    let fixture = fixture();
    let command = "if [ -e killed-once ]; then exit 0; fi; touch killed-once; printf '%s' \"$$\" > validation-child.pid.tmp; mv validation-child.pid.tmp validation-child.pid; touch validation-started; sleep 5; printf late > late-after-kill";
    one_contract(&fixture, command);
    begin_edit(&fixture, "session-a", "turn-1", "edit-1");
    let runner_capability = open_runner(&fixture, "session-a", "turn-1");
    let mut request = common(&fixture, "session-a", "turn-1");
    request["schema_version"] = json!("agent-hook.finish-line.run.v1");
    request["operation_id"] = json!("validation-killed");
    request["intent"] = json!("project-dev");
    request["command"] = json!(command);
    request["runner_capability"] = json!(runner_capability);
    request["timeout_ms"] = json!(10_000);
    request["execution"] = json!({
        "kind": "bash-v1",
        "workdir": fixture.root,
        "output_max_bytes": 64 * 1024,
        "runner": {"kind": "danger-full-access"},
    });

    let mut child = Command::new(env!("CARGO_BIN_EXE_agent-hook"))
        .args(["finish-line", "run", "--format", "json"])
        .current_dir(&fixture.root)
        .env("HOME", &fixture.home)
        .env("XDG_CONFIG_HOME", &fixture.config_home)
        .env("XDG_STATE_HOME", &fixture.state_home)
        .env("AGENT_SESSION_STATE_DIR", &fixture.session_state)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .expect("spawn validation supervisor");
    child
        .stdin
        .take()
        .expect("validation stdin")
        .write_all(request.to_string().as_bytes())
        .expect("write validation request");
    let deadline = Instant::now() + Duration::from_secs(2);
    while !fixture.root.join("validation-started").exists() {
        assert!(Instant::now() < deadline, "validation did not start");
        thread::sleep(Duration::from_millis(10));
    }
    let child_pid: i32 = fs::read_to_string(fixture.root.join("validation-child.pid"))
        .expect("validation child pid")
        .parse()
        .expect("numeric validation child pid");
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
    let status = child.wait().expect("reap killed supervisor");
    assert!(!status.success());

    let mut quiesce = common(&fixture, "session-a", "turn-1");
    quiesce["schema_version"] = json!("agent-hook.finish-line.quiesce.v1");
    quiesce["operation_id"] = json!("validation-killed");
    quiesce["runner_capability"] = json!(runner_capability);
    let output = fixture.run(
        &["finish-line", "quiesce", "--format", "json"],
        Some(&quiesce.to_string()),
    );
    assert_eq!(
        output.code,
        0,
        "stdout={} stderr={}",
        output.stdout_text(),
        output.stderr_text()
    );
    assert_eq!(output.stdout_json()["data"]["status"], "quiescent");
    thread::sleep(Duration::from_millis(300));
    assert!(!fixture.root.join("late-after-kill").exists());
    assert_ne!(unsafe { libc::kill(child_pid, 0) }, 0);
    let (_, blocked) = stop(&fixture, "session-a");
    assert!(!reason_codes(&blocked).contains(&"validation-pending"));

    let (code, retried) = run_validation(
        &fixture,
        "session-a",
        "turn-2",
        "validation-retry",
        "project-dev",
        command,
        5_000,
    );
    assert_eq!(code, 0, "envelope={retried}");
    assert_eq!(retried["data"]["execution"]["exit_code"], 0);
    assert_eq!(stop(&fixture, "session-a").0, 0);
}

#[test]
fn quiesce_stabilizes_a_unit_cancelled_during_submission() {
    let fixture = fixture();
    let command = "sleep 0.25; printf late > late-after-submission-cancel";
    one_contract(&fixture, command);
    begin_edit(&fixture, "session-a", "turn-1", "edit-1");
    let runner_capability = open_runner(&fixture, "session-a", "turn-1");
    let mut request = common(&fixture, "session-a", "turn-1");
    request["schema_version"] = json!("agent-hook.finish-line.run.v1");
    request["operation_id"] = json!("validation-submission-cancelled");
    request["intent"] = json!("project-dev");
    request["command"] = json!(command);
    request["runner_capability"] = json!(runner_capability);
    request["timeout_ms"] = json!(5_000);
    request["execution"] = json!({
        "kind": "bash-v1",
        "workdir": fixture.root,
        "output_max_bytes": 64 * 1024,
        "runner": {"kind": "danger-full-access"},
    });

    let mut child = Command::new(env!("CARGO_BIN_EXE_agent-hook"))
        .args(["finish-line", "run", "--format", "json"])
        .current_dir(&fixture.root)
        .env("HOME", &fixture.home)
        .env("XDG_CONFIG_HOME", &fixture.config_home)
        .env("XDG_STATE_HOME", &fixture.state_home)
        .env("AGENT_SESSION_STATE_DIR", &fixture.session_state)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .expect("spawn validation supervisor");
    child
        .stdin
        .take()
        .expect("validation stdin")
        .write_all(request.to_string().as_bytes())
        .expect("write validation request");

    let state_path = finish_line_state_path(&fixture);
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let state = fs::read_to_string(&state_path)
            .ok()
            .and_then(|state| serde_json::from_str::<Value>(&state).ok());
        let pending_unit = state
            .as_ref()
            .and_then(|state| state["operations"].as_object())
            .is_some_and(|operations| {
                operations.values().any(|operation| {
                    operation["active_unit"].is_string() && operation["terminal"].is_null()
                })
            });
        if pending_unit {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "validation did not persist its submitting unit"
        );
        thread::sleep(Duration::from_millis(2));
    }
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
    assert!(!child.wait().expect("reap killed supervisor").success());

    let mut quiesce = common(&fixture, "session-a", "turn-1");
    quiesce["schema_version"] = json!("agent-hook.finish-line.quiesce.v1");
    quiesce["operation_id"] = json!("validation-submission-cancelled");
    quiesce["runner_capability"] = json!(runner_capability);
    let output = fixture.run(
        &["finish-line", "quiesce", "--format", "json"],
        Some(&quiesce.to_string()),
    );
    assert_eq!(
        output.code,
        0,
        "stdout={} stderr={}",
        output.stdout_text(),
        output.stderr_text()
    );
    thread::sleep(Duration::from_millis(350));
    assert!(!fixture.root.join("late-after-submission-cancel").exists());
    let (_, blocked) = stop(&fixture, "session-a");
    assert!(!reason_codes(&blocked).contains(&"validation-pending"));
}

#[test]
fn quiesce_rejects_wrong_and_cross_session_capabilities_without_touching_the_unit() {
    let fixture = fixture();
    let command = "printf ready > quiesce-auth-ready; sleep 5";
    one_contract(&fixture, command);
    begin_edit(&fixture, "session-a", "turn-1", "edit-1");
    let runner_capability = open_runner(&fixture, "session-a", "turn-1");
    let cross_session_capability = open_runner(&fixture, "session-b", "turn-1");
    let mut request = common(&fixture, "session-a", "turn-1");
    request["schema_version"] = json!("agent-hook.finish-line.run.v1");
    request["operation_id"] = json!("validation-quiesce-auth");
    request["intent"] = json!("project-dev");
    request["command"] = json!(command);
    request["runner_capability"] = json!(runner_capability);
    request["timeout_ms"] = json!(10_000);
    request["execution"] = json!({
        "kind": "bash-v1",
        "workdir": fixture.root,
        "output_max_bytes": 64 * 1024,
        "runner": {"kind": "danger-full-access"},
    });

    let mut child = Command::new(env!("CARGO_BIN_EXE_agent-hook"))
        .args(["finish-line", "run", "--format", "json"])
        .current_dir(&fixture.root)
        .env("HOME", &fixture.home)
        .env("XDG_CONFIG_HOME", &fixture.config_home)
        .env("XDG_STATE_HOME", &fixture.state_home)
        .env("AGENT_SESSION_STATE_DIR", &fixture.session_state)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .expect("spawn validation supervisor");
    child
        .stdin
        .take()
        .expect("validation stdin")
        .write_all(request.to_string().as_bytes())
        .expect("write validation request");
    let deadline = Instant::now() + Duration::from_secs(2);
    while !fixture.root.join("quiesce-auth-ready").exists() {
        assert!(Instant::now() < deadline, "validation did not start");
        thread::sleep(Duration::from_millis(10));
    }

    for rejected_capability in [
        "finish-line-runner:wrong".to_string(),
        cross_session_capability,
    ] {
        let mut quiesce = common(&fixture, "session-a", "turn-1");
        quiesce["schema_version"] = json!("agent-hook.finish-line.quiesce.v1");
        quiesce["operation_id"] = json!("validation-quiesce-auth");
        quiesce["runner_capability"] = json!(rejected_capability);
        let rejected = fixture.run(
            &["finish-line", "quiesce", "--format", "json"],
            Some(&quiesce.to_string()),
        );
        assert_eq!(rejected.code, 65, "envelope={}", rejected.stdout_json());
        assert_eq!(
            rejected.stdout_json()["error"]["code"],
            "finish-line-runner-capability-invalid"
        );
        assert!(child.try_wait().expect("supervisor state").is_none());
        let (_, pending) = stop(&fixture, "session-a");
        assert!(reason_codes(&pending).contains(&"validation-pending"));
    }

    let mut quiesce = common(&fixture, "session-a", "turn-1");
    quiesce["schema_version"] = json!("agent-hook.finish-line.quiesce.v1");
    quiesce["operation_id"] = json!("validation-quiesce-auth");
    quiesce["runner_capability"] = json!(runner_capability);
    let quiesced = fixture.run(
        &["finish-line", "quiesce", "--format", "json"],
        Some(&quiesce.to_string()),
    );
    assert_eq!(quiesced.code, 0, "envelope={}", quiesced.stdout_json());
    let _ = child.wait().expect("reap quiesced supervisor");
    let (_, after) = stop(&fixture, "session-a");
    assert!(!reason_codes(&after).contains(&"validation-pending"));
}

#[test]
fn failed_contained_runner_stays_pending_until_authenticated_quiescence() {
    let fixture = fixture();
    let command = ":";
    one_contract(&fixture, command);
    begin_edit(&fixture, "session-a", "turn-1", "edit-1");
    let runner_capability = open_runner(&fixture, "session-a", "turn-1");
    let mut request = common(&fixture, "session-a", "turn-1");
    request["schema_version"] = json!("agent-hook.finish-line.run.v1");
    request["operation_id"] = json!("validation-runner-failed");
    request["intent"] = json!("project-dev");
    request["command"] = json!(command);
    request["runner_capability"] = json!(runner_capability);
    request["timeout_ms"] = json!(5_000);
    request["execution"] = json!({
        "kind": "bash-v1",
        "workdir": fixture.root,
        "output_max_bytes": 64 * 1024,
        "runner": {
            "kind": "confined",
            "argv": [
                "/definitely/missing/nils-finish-line-provider",
                "--",
                "bash",
                "-c",
                command,
            ],
            "mode": "workspace-write",
            "enforcement": "full",
            "denial_signatures": [],
            "runner_failure_rules": [],
        },
    });
    let failed = fixture.run(
        &["finish-line", "run", "--format", "json"],
        Some(&request.to_string()),
    );
    assert_ne!(failed.code, 0, "runner failure unexpectedly succeeded");

    let (code, pending) = stop(&fixture, "session-a");
    assert_eq!(code, 1, "envelope={pending}");
    assert!(
        reason_codes(&pending).contains(&"validation-pending"),
        "runner failure lost the active unit before quiescence: {pending}"
    );

    let mut quiesce = common(&fixture, "session-a", "turn-1");
    quiesce["schema_version"] = json!("agent-hook.finish-line.quiesce.v1");
    quiesce["operation_id"] = json!("validation-runner-failed");
    quiesce["runner_capability"] = json!(runner_capability);
    let quiesced = fixture.run(
        &["finish-line", "quiesce", "--format", "json"],
        Some(&quiesce.to_string()),
    );
    assert_eq!(
        quiesced.code,
        0,
        "stdout={} stderr={}",
        quiesced.stdout_text(),
        quiesced.stderr_text()
    );
    assert_eq!(quiesced.stdout_json()["data"]["status"], "quiescent");
    let (_, after) = stop(&fixture, "session-a");
    assert!(!reason_codes(&after).contains(&"validation-pending"));
}

#[test]
fn timeout_kills_the_validation_cgroup_and_records_failure() {
    let fixture = fixture();
    let command = "sleep 30 & child=$!; printf '%s' \"$child\" > child.pid; wait \"$child\"";
    one_contract(&fixture, command);
    begin_edit(&fixture, "session-a", "turn-1", "edit-1");
    let (code, timed_out) = run_validation(
        &fixture,
        "session-a",
        "turn-1",
        "validation-timeout",
        "project-dev",
        command,
        100,
    );
    assert_eq!(code, 0, "envelope={timed_out}");
    assert_eq!(timed_out["data"]["execution"]["timed_out"], true);
    let child: i32 = fs::read_to_string(fixture.root.join("child.pid"))
        .expect("child pid")
        .parse()
        .expect("numeric child pid");
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let alive = unsafe { libc::kill(child, 0) } == 0;
        if !alive {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "validation descendant survived timeout"
        );
        thread::sleep(Duration::from_millis(10));
    }
    let (code, blocked) = stop(&fixture, "session-a");
    assert_eq!(code, 1);
    assert!(reason_codes(&blocked).contains(&"validation-failed"));
}

#[test]
fn strict_run_schema_and_timeout_bounds_fail_closed() {
    let fixture = fixture();
    one_contract(&fixture, ":");
    begin_edit(&fixture, "session-a", "turn-1", "edit-1");
    let runner_capability = open_runner(&fixture, "session-a", "turn-1");
    let mut request = common(&fixture, "session-a", "turn-1");
    request["schema_version"] = json!("agent-hook.finish-line.run.v1");
    request["operation_id"] = json!("validation-1");
    request["intent"] = json!("project-dev");
    request["command"] = json!(":");
    request["runner_capability"] = json!(runner_capability);
    request["timeout_ms"] = json!(0);
    request["execution"] = json!({
        "kind": "bash-v1",
        "workdir": fixture.root,
        "output_max_bytes": 64 * 1024,
        "runner": {"kind": "danger-full-access"},
    });
    let output = fixture.run(
        &["finish-line", "run", "--format", "json"],
        Some(&request.to_string()),
    );
    assert_eq!(output.code, 65);
    assert_eq!(
        output.stdout_json()["error"]["code"],
        "finish-line-timeout-invalid"
    );

    request["timeout_ms"] = json!(5_000);
    request["outcome"] = json!({"status": "success", "exit_code": 0});
    let output = fixture.run(
        &["finish-line", "run", "--format", "json"],
        Some(&request.to_string()),
    );
    assert_eq!(output.code, 65);
    assert_eq!(
        output.stdout_json()["error"]["code"],
        "finish-line-request-invalid"
    );
}

#[test]
fn state_is_private_bounded_and_contains_no_raw_command_or_identity() {
    let fixture = fixture();
    let command = "printf private-output";
    one_contract(&fixture, command);
    begin_edit(&fixture, "raw-session-identifier", "turn-1", "edit-1");
    let (_, envelope) = run_validation(
        &fixture,
        "raw-session-identifier",
        "turn-1",
        "validation-1",
        "project-dev",
        command,
        5_000,
    );
    assert_eq!(
        envelope["data"]["execution"]["stdout"]["text"],
        "private-output"
    );
    let state_path = finish_line_state_path(&fixture);
    let metadata = fs::metadata(&state_path).expect("state metadata");
    assert_eq!(metadata.uid(), unsafe { libc::geteuid() });
    assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
    assert!(metadata.len() <= 384 * 1_024);
    let state = fs::read_to_string(state_path).expect("state");
    assert!(!state.contains("raw-session-identifier"));
    assert!(!state.contains(command));
    assert!(!state.contains("private-output"));

    let (code, inspected) = status(&fixture, "raw-session-identifier");
    assert_eq!(code, 0, "envelope={inspected}");
    assert_ne!(
        inspected["data"]["correlation_id"],
        "raw-session-identifier"
    );
}
