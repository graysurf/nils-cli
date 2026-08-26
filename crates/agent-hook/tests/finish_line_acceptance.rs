#![cfg(target_os = "linux")]

mod support;

use std::collections::BTreeSet;
use std::io::Write;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

use pretty_assertions::{assert_eq, assert_ne};
use serde_json::{Value, json};

use support::Fixture;

const POLICY: &str = r#"schema_version = "agent-hook.policy.v1"
bundle_id = "finish-line-acceptance-test"
version = "2026.08.25.1"
"#;
const VALIDATOR_DEFINITION: &str =
    "sha256:1111111111111111111111111111111111111111111111111111111111111111";
const MUTATION_DEFINITION: &str =
    "sha256:2222222222222222222222222222222222222222222222222222222222222222";
const SECOND_VALIDATOR_DEFINITION: &str =
    "sha256:3333333333333333333333333333333333333333333333333333333333333333";

fn fixture() -> Fixture {
    let fixture = Fixture::new(POLICY);
    let status = Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(&fixture.root)
        .status()
        .expect("git init");
    assert!(status.success(), "git init failed");
    fixture
}

fn common(fixture: &Fixture, session_id: &str, turn_id: &str) -> Value {
    json!({
        "product": "dsh",
        "session_id": session_id,
        "turn_id": turn_id,
        "cwd": fixture.root,
    })
}

fn call(fixture: &Fixture, operation: &str, request: &Value) -> (i32, Value) {
    let output = fixture.run(
        &["finish-line", operation, "--format", "json"],
        Some(&request.to_string()),
    );
    (output.code, output.stdout_json())
}

fn install_bash_contract(fixture: &Fixture, command: &str) {
    std::fs::write(
        fixture.root.join("AGENT_DOCS.toml"),
        format!(
            "[[validation]]\ncontext = \"project-dev\"\nproduct = \"dsh\"\ncommands = [{command:?}]\ndescription = \"acceptance test\"\n"
        ),
    )
    .expect("agent-docs contract");
}

fn finish_line_state_paths(fixture: &Fixture) -> (PathBuf, PathBuf) {
    let repo_state_dir = fixture.state_home.join("agent-hook/finish-line/repos");
    let mut main = None;
    let mut acceptance = None;
    for entry in std::fs::read_dir(repo_state_dir).expect("finish-line state directory") {
        let path = entry.expect("finish-line state entry").path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if name.ends_with(".acceptance.json") {
            acceptance = Some(path);
        } else if name.ends_with(".json") {
            main = Some(path);
        }
    }
    (
        main.expect("main finish-line state"),
        acceptance.expect("acceptance finish-line state"),
    )
}

fn read_json(path: &PathBuf) -> Value {
    serde_json::from_slice(&std::fs::read(path).expect("read JSON state"))
        .expect("parse JSON state")
}

fn write_json(path: &PathBuf, value: &Value) {
    std::fs::write(
        path,
        serde_json::to_vec(value).expect("serialize JSON state"),
    )
    .expect("write JSON state");
}

fn open(fixture: &Fixture, session_id: &str) -> String {
    let mut request = common(fixture, session_id, "turn-open");
    request["schema_version"] = json!("agent-hook.finish-line.open.v1");
    request["attempt_token"] = json!(format!("private-open:{session_id}"));
    let (code, envelope) = call(fixture, "open", &request);
    assert_eq!(code, 0, "envelope={envelope}");
    envelope["data"]["runner_capability"]
        .as_str()
        .expect("runner capability")
        .to_string()
}

fn register(fixture: &Fixture, session_id: &str, capability: &str) -> (i32, Value) {
    let mut request = common(fixture, session_id, "turn-register");
    request["schema_version"] = json!("agent-hook.finish-line.register.v1");
    request["runner_capability"] = json!(capability);
    request["requirements"] = json!([{
        "name": "unit",
        "validators": [{
            "id": "runtime-plus-one",
            "tool_name": "runtime_kit_plus_one",
            "definition_digest": VALIDATOR_DEFINITION,
            "execution": {"kind": "host-observed"},
        }],
    }]);
    request["invalidators"] = json!([{
        "tool_name": "edit",
        "definition_digest": MUTATION_DEFINITION,
    }]);
    call(fixture, "register", &request)
}

fn admit_validator(
    fixture: &Fixture,
    session_id: &str,
    capability: &str,
    contract_digest: &str,
    operation_id: &str,
) -> (i32, Value) {
    admit_validator_binding(
        fixture,
        session_id,
        capability,
        contract_digest,
        operation_id,
        "unit",
        "runtime-plus-one",
        "runtime_kit_plus_one",
        VALIDATOR_DEFINITION,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn admit_validator_binding(
    fixture: &Fixture,
    session_id: &str,
    capability: &str,
    contract_digest: &str,
    operation_id: &str,
    requirement: &str,
    validator_id: &str,
    tool_name: &str,
    definition_digest: &str,
    source_operation_id: Option<&str>,
) -> (i32, Value) {
    let mut request = common(fixture, session_id, "turn-validator");
    request["schema_version"] = json!("agent-hook.finish-line.admit.v1");
    request["runner_capability"] = json!(capability);
    request["contract_digest"] = json!(contract_digest);
    request["operation_id"] = json!(operation_id);
    request["attempt_token"] = json!(format!("validator-token:{operation_id}"));
    request["operation"] = json!({
        "kind": "validator",
        "requirement": requirement,
        "validator_id": validator_id,
        "tool_name": tool_name,
        "definition_digest": definition_digest,
    });
    if let Some(source_operation_id) = source_operation_id {
        request["operation"]["source_operation_id"] = json!(source_operation_id);
    }
    call(fixture, "admit", &request)
}

fn release(fixture: &Fixture, session_id: &str, capability: &str) -> (i32, Value) {
    let mut request = common(fixture, session_id, "turn-release");
    request["schema_version"] = json!("agent-hook.finish-line.release.v1");
    request["runner_capability"] = json!(capability);
    call(fixture, "release", &request)
}

fn quiesce(
    fixture: &Fixture,
    session_id: &str,
    turn_id: &str,
    capability: &str,
    operation_id: &str,
) -> (i32, Value) {
    let mut request = common(fixture, session_id, turn_id);
    request["schema_version"] = json!("agent-hook.finish-line.quiesce.v1");
    request["runner_capability"] = json!(capability);
    request["operation_id"] = json!(operation_id);
    call(fixture, "quiesce", &request)
}

fn admit_mutation(
    fixture: &Fixture,
    session_id: &str,
    capability: &str,
    contract_digest: &str,
    operation_id: &str,
) -> (i32, Value) {
    let mut request = common(fixture, session_id, "turn-mutation");
    request["schema_version"] = json!("agent-hook.finish-line.admit.v1");
    request["runner_capability"] = json!(capability);
    request["contract_digest"] = json!(contract_digest);
    request["operation_id"] = json!(operation_id);
    request["attempt_token"] = json!(format!("mutation-token:{operation_id}"));
    request["operation"] = json!({
        "kind": "mutation",
        "tool_name": "edit",
        "definition_digest": MUTATION_DEFINITION,
    });
    call(fixture, "admit", &request)
}

fn observe(
    fixture: &Fixture,
    session_id: &str,
    capability: &str,
    operation_id: &str,
    status: &str,
) -> (i32, Value) {
    let mut request = common(fixture, session_id, "turn-observe");
    request["schema_version"] = json!("agent-hook.finish-line.observe.v1");
    request["runner_capability"] = json!(capability);
    request["operation_id"] = json!(operation_id);
    request["observation"] = json!({"kind": "host-observed", "status": status});
    call(fixture, "observe", &request)
}

fn observe_contained(
    fixture: &Fixture,
    session_id: &str,
    capability: &str,
    operation_id: &str,
    contained_operation_id: &str,
) -> (i32, Value) {
    let mut request = common(fixture, session_id, "turn-observe-contained");
    request["schema_version"] = json!("agent-hook.finish-line.observe.v1");
    request["runner_capability"] = json!(capability);
    request["operation_id"] = json!(operation_id);
    request["observation"] = json!({
        "kind": "contained-bash",
        "operation_id": contained_operation_id,
    });
    call(fixture, "observe", &request)
}

fn verdict(
    fixture: &Fixture,
    session_id: &str,
    capability: &str,
    contract_digest: &str,
) -> (i32, Value) {
    let mut request = common(fixture, session_id, "turn-verdict");
    request["schema_version"] = json!("agent-hook.finish-line.verdict.v1");
    request["runner_capability"] = json!(capability);
    request["contract_digest"] = json!(contract_digest);
    call(fixture, "verdict", &request)
}

fn reserve_completion(
    fixture: &Fixture,
    session_id: &str,
    capability: &str,
    contract_digest: &str,
    operation_id: &str,
) -> (i32, Value) {
    let mut request = common(fixture, session_id, "turn-completion-reservation");
    request["schema_version"] = json!("agent-hook.finish-line.verdict.v1");
    request["runner_capability"] = json!(capability);
    request["contract_digest"] = json!(contract_digest);
    request["completion_reservation"] = json!({"operation_id": operation_id});
    call(fixture, "verdict", &request)
}

#[test]
fn named_host_validator_satisfies_only_its_exact_current_requirement() {
    let fixture = fixture();
    let capability = open(&fixture, "session-a");
    let (register_code, registered) = register(&fixture, "session-a", &capability);
    assert_eq!(register_code, 0, "envelope={registered}");
    assert_eq!(registered["data"]["status"], "registered");
    let contract_digest = registered["data"]["contract_digest"]
        .as_str()
        .expect("contract digest");

    let (missing_code, missing) = verdict(&fixture, "session-a", &capability, contract_digest);
    assert_eq!(missing_code, 1, "envelope={missing}");
    assert_eq!(missing["data"]["aggregate"], "missing");
    assert_eq!(missing["data"]["requirements"][0]["status"], "missing");

    let (admit_code, admitted) = admit_validator(
        &fixture,
        "session-a",
        &capability,
        contract_digest,
        "validator-1",
    );
    assert_eq!(admit_code, 0, "envelope={admitted}");
    assert_eq!(admitted["data"]["status"], "admitted");
    assert_eq!(admitted["data"]["generation"], 0);
    assert_eq!(
        verdict(&fixture, "session-a", &capability, contract_digest).1["data"]["aggregate"],
        "active"
    );
    let mut malformed = common(&fixture, "session-a", "turn-malformed");
    malformed["schema_version"] = json!("agent-hook.finish-line.observe.v1");
    malformed["runner_capability"] = json!(capability);
    malformed["operation_id"] = json!("validator-1");
    malformed["observation"] = json!({
        "kind": "host-observed",
        "status": "succeeded",
        "output": "caller-reported output is forbidden",
    });
    let (malformed_code, malformed_result) = call(&fixture, "observe", &malformed);
    assert_eq!(malformed_code, 65, "envelope={malformed_result}");
    assert_eq!(
        verdict(&fixture, "session-a", &capability, contract_digest).1["data"]["aggregate"],
        "active",
        "a malformed provider response must not mutate evidence"
    );
    let (busy_code, busy) = release(&fixture, "session-a", &capability);
    assert_eq!(busy_code, 75, "envelope={busy}");
    assert_eq!(busy["error"]["code"], "finish-line-session-busy");

    let (observe_code, observed) = observe(
        &fixture,
        "session-a",
        &capability,
        "validator-1",
        "succeeded",
    );
    assert_eq!(observe_code, 0, "envelope={observed}");
    assert_eq!(observed["data"]["status"], "applied");

    let (verdict_code, accepted) = verdict(&fixture, "session-a", &capability, contract_digest);
    assert_eq!(verdict_code, 0, "envelope={accepted}");
    assert_eq!(accepted["data"]["aggregate"], "satisfied");
    assert_eq!(accepted["data"]["requirements"][0]["name"], "unit");
    assert_eq!(accepted["data"]["requirements"][0]["status"], "satisfied");
}

#[test]
fn completion_reservation_atomically_blocks_repository_mutation_until_consumed() {
    let fixture = fixture();
    let capability_a = open(&fixture, "session-completion-a");
    let capability_b = open(&fixture, "session-completion-b");
    let registered_a = register(&fixture, "session-completion-a", &capability_a).1;
    let registered_b = register(&fixture, "session-completion-b", &capability_b).1;
    let contract_a = registered_a["data"]["contract_digest"]
        .as_str()
        .expect("contract A");
    let contract_b = registered_b["data"]["contract_digest"]
        .as_str()
        .expect("contract B");
    admit_validator(
        &fixture,
        "session-completion-a",
        &capability_a,
        contract_a,
        "validator-completion-a",
    );
    observe(
        &fixture,
        "session-completion-a",
        &capability_a,
        "validator-completion-a",
        "succeeded",
    );

    let (reserved_code, reserved) = reserve_completion(
        &fixture,
        "session-completion-a",
        &capability_a,
        contract_a,
        "goal-completion-a",
    );
    assert_eq!(reserved_code, 0, "envelope={reserved}");
    assert_eq!(reserved["data"]["action"], "allow");
    assert_eq!(
        reserved["data"]["completion_reservation"],
        json!({"operation_id": "goal-completion-a", "status": "reserved"})
    );
    let duplicate = reserve_completion(
        &fixture,
        "session-completion-a",
        &capability_a,
        contract_a,
        "goal-completion-a",
    );
    assert_eq!(duplicate.0, 0, "envelope={}", duplicate.1);
    assert_eq!(
        duplicate.1["data"]["completion_reservation"]["status"],
        "duplicate"
    );

    let blocked_mutation = admit_mutation(
        &fixture,
        "session-completion-b",
        &capability_b,
        contract_b,
        "mutation-blocked-by-completion",
    );
    assert_eq!(blocked_mutation.0, 75, "envelope={}", blocked_mutation.1);
    assert_eq!(
        blocked_mutation.1["error"]["code"],
        "finish-line-completion-reserved"
    );

    let mut shell = common(&fixture, "session-completion-b", "turn-shell-blocked");
    shell["schema_version"] = json!("agent-hook.finish-line.run.v1");
    shell["operation_id"] = json!("shell-blocked-by-completion");
    shell["intent"] = json!("project-dev");
    shell["command"] = json!("printf must-not-run > completion-race");
    shell["runner_capability"] = json!(capability_b);
    shell["timeout_ms"] = json!(5_000);
    shell["execution"] = json!({
        "kind": "bash-v1",
        "workdir": fixture.root,
        "output_max_bytes": 64 * 1024,
        "runner": {"kind": "danger-full-access"},
    });
    let blocked_shell = call(&fixture, "run", &shell);
    assert_eq!(blocked_shell.0, 75, "envelope={}", blocked_shell.1);
    assert_eq!(
        blocked_shell.1["error"]["code"],
        "finish-line-completion-reserved"
    );
    assert!(!fixture.root.join("completion-race").exists());

    let consumed = observe(
        &fixture,
        "session-completion-a",
        &capability_a,
        "goal-completion-a",
        "succeeded",
    );
    assert_eq!(consumed.0, 0, "envelope={}", consumed.1);
    assert_eq!(consumed.1["data"]["status"], "applied");
    let admitted = admit_mutation(
        &fixture,
        "session-completion-b",
        &capability_b,
        contract_b,
        "mutation-after-completion",
    );
    assert_eq!(admitted.0, 0, "envelope={}", admitted.1);

    observe(
        &fixture,
        "session-completion-b",
        &capability_b,
        "mutation-after-completion",
        "succeeded",
    );
    admit_validator(
        &fixture,
        "session-completion-a",
        &capability_a,
        contract_a,
        "validator-before-release",
    );
    observe(
        &fixture,
        "session-completion-a",
        &capability_a,
        "validator-before-release",
        "succeeded",
    );
    let reserved_for_release = reserve_completion(
        &fixture,
        "session-completion-a",
        &capability_a,
        contract_a,
        "goal-completion-release",
    );
    assert_eq!(
        reserved_for_release.0, 0,
        "envelope={}",
        reserved_for_release.1
    );
    let released = release(&fixture, "session-completion-a", &capability_a);
    assert_eq!(released.0, 0, "envelope={}", released.1);
    let after_release = admit_mutation(
        &fixture,
        "session-completion-b",
        &capability_b,
        contract_b,
        "mutation-after-reservation-release",
    );
    assert_eq!(after_release.0, 0, "envelope={}", after_release.1);
}

#[test]
fn completion_reservation_and_repository_mutation_are_serialized_by_one_lock() {
    let fixture = fixture();
    let capability_a = open(&fixture, "session-race-a");
    let capability_b = open(&fixture, "session-race-b");
    let registered_a = register(&fixture, "session-race-a", &capability_a).1;
    let registered_b = register(&fixture, "session-race-b", &capability_b).1;
    let contract_a = registered_a["data"]["contract_digest"]
        .as_str()
        .expect("contract A");
    let contract_b = registered_b["data"]["contract_digest"]
        .as_str()
        .expect("contract B");
    admit_validator(
        &fixture,
        "session-race-a",
        &capability_a,
        contract_a,
        "validator-race-a",
    );
    observe(
        &fixture,
        "session-race-a",
        &capability_a,
        "validator-race-a",
        "succeeded",
    );

    let barrier = Arc::new(Barrier::new(3));
    let (reserved, mutation) = thread::scope(|scope| {
        let reserve_barrier = Arc::clone(&barrier);
        let reserve_fixture = &fixture;
        let reserve_capability = &capability_a;
        let reserve = scope.spawn(move || {
            reserve_barrier.wait();
            reserve_completion(
                reserve_fixture,
                "session-race-a",
                reserve_capability,
                contract_a,
                "goal-completion-race",
            )
        });
        let mutation_barrier = Arc::clone(&barrier);
        let mutation_fixture = &fixture;
        let mutation_capability = &capability_b;
        let mutation = scope.spawn(move || {
            mutation_barrier.wait();
            admit_mutation(
                mutation_fixture,
                "session-race-b",
                mutation_capability,
                contract_b,
                "mutation-race-b",
            )
        });
        barrier.wait();
        (
            reserve.join().expect("reservation thread"),
            mutation.join().expect("mutation thread"),
        )
    });

    assert_ne!(
        reserved.0 == 0,
        mutation.0 == 0,
        "reserved={}; mutation={}",
        reserved.1,
        mutation.1
    );
    if reserved.0 == 0 {
        assert_eq!(mutation.0, 75, "envelope={}", mutation.1);
        assert_eq!(
            mutation.1["error"]["code"],
            "finish-line-completion-reserved"
        );
    } else {
        assert_eq!(mutation.0, 0, "envelope={}", mutation.1);
        assert_eq!(reserved.0, 1, "envelope={}", reserved.1);
        assert_eq!(reserved.1["data"]["aggregate"], "active");
        assert_eq!(reserved.1["data"]["completion_reservation"], Value::Null);
    }
}

#[test]
fn orphaned_completion_reservation_terminalizes_before_repository_mutation() {
    let fixture = fixture();
    let capability_a = open(&fixture, "session-orphaned-completion-a");
    let capability_b = open(&fixture, "session-orphaned-completion-b");
    let registered_a = register(&fixture, "session-orphaned-completion-a", &capability_a).1;
    let registered_b = register(&fixture, "session-orphaned-completion-b", &capability_b).1;
    let contract_a = registered_a["data"]["contract_digest"]
        .as_str()
        .expect("contract A");
    let contract_b = registered_b["data"]["contract_digest"]
        .as_str()
        .expect("contract B");
    admit_validator(
        &fixture,
        "session-orphaned-completion-a",
        &capability_a,
        contract_a,
        "validator-orphaned-completion",
    );
    observe(
        &fixture,
        "session-orphaned-completion-a",
        &capability_a,
        "validator-orphaned-completion",
        "succeeded",
    );
    let reserved = reserve_completion(
        &fixture,
        "session-orphaned-completion-a",
        &capability_a,
        contract_a,
        "goal-orphaned-completion",
    );
    assert_eq!(reserved.0, 0, "envelope={}", reserved.1);

    let (main_path, acceptance_path) = finish_line_state_paths(&fixture);
    let acceptance_before_orphan = read_json(&acceptance_path);
    let orphaned_session_key = acceptance_before_orphan["operations"]
        .as_object()
        .expect("acceptance operations")
        .values()
        .find(|operation| operation["kind"]["kind"] == "completion")
        .and_then(|operation| operation["session_key"].as_str())
        .expect("orphaned session key")
        .to_string();
    let mut main_state = read_json(&main_path);
    main_state["sessions"]
        .as_object_mut()
        .expect("main sessions")
        .remove(&orphaned_session_key);
    write_json(&main_path, &main_state);

    let admitted = admit_mutation(
        &fixture,
        "session-orphaned-completion-b",
        &capability_b,
        contract_b,
        "mutation-after-orphaned-completion",
    );
    assert_eq!(admitted.0, 0, "envelope={}", admitted.1);
    let acceptance_state = read_json(&acceptance_path);
    let terminal = acceptance_state["operations"]
        .as_object()
        .expect("acceptance operations")
        .values()
        .find(|operation| operation["kind"]["kind"] == "completion")
        .and_then(|operation| operation["terminal"].as_object())
        .expect("orphaned completion terminal");
    assert_eq!(terminal["observation"], "infrastructure-blocked");
    assert_eq!(terminal["source_digest"], "session-orphaned");
}

#[test]
fn admitted_mutation_invalidates_before_body_and_failure_never_restores_evidence() {
    let fixture = fixture();
    let capability = open(&fixture, "session-a");
    let registered = register(&fixture, "session-a", &capability).1;
    let contract_digest = registered["data"]["contract_digest"]
        .as_str()
        .expect("contract digest");
    admit_validator(
        &fixture,
        "session-a",
        &capability,
        contract_digest,
        "validator-1",
    );
    observe(
        &fixture,
        "session-a",
        &capability,
        "validator-1",
        "succeeded",
    );
    assert_eq!(
        verdict(&fixture, "session-a", &capability, contract_digest).0,
        0
    );

    let (admit_code, admitted) = admit_mutation(
        &fixture,
        "session-a",
        &capability,
        contract_digest,
        "mutation-1",
    );
    assert_eq!(admit_code, 0, "envelope={admitted}");
    assert_eq!(admitted["data"]["generation"], 1);
    assert_eq!(
        verdict(&fixture, "session-a", &capability, contract_digest).1["data"]["aggregate"],
        "active"
    );

    let (observe_code, observed) =
        observe(&fixture, "session-a", &capability, "mutation-1", "failed");
    assert_eq!(observe_code, 0, "envelope={observed}");
    let (blocked_code, blocked) = verdict(&fixture, "session-a", &capability, contract_digest);
    assert_eq!(blocked_code, 1, "envelope={blocked}");
    assert_eq!(blocked["data"]["generation"], 1);
    assert_eq!(blocked["data"]["aggregate"], "missing");

    admit_validator(
        &fixture,
        "session-a",
        &capability,
        contract_digest,
        "validator-after-failure",
    );
    observe(
        &fixture,
        "session-a",
        &capability,
        "validator-after-failure",
        "succeeded",
    );
    assert_eq!(
        verdict(&fixture, "session-a", &capability, contract_digest).0,
        0
    );

    admit_mutation(
        &fixture,
        "session-a",
        &capability,
        contract_digest,
        "mutation-cancelled",
    );
    observe(
        &fixture,
        "session-a",
        &capability,
        "mutation-cancelled",
        "cancelled",
    );
    assert_eq!(
        verdict(&fixture, "session-a", &capability, contract_digest).1["data"]["aggregate"],
        "uncertain"
    );
    admit_validator(
        &fixture,
        "session-a",
        &capability,
        contract_digest,
        "validator-after-cancellation",
    );
    observe(
        &fixture,
        "session-a",
        &capability,
        "validator-after-cancellation",
        "succeeded",
    );
    assert_eq!(
        verdict(&fixture, "session-a", &capability, contract_digest).0,
        0,
        "exact later validation must reconcile an uncertain mutation generation"
    );
}

#[test]
fn persisted_mutation_admission_crash_boundaries_reconcile_once_and_drift_blocks() {
    for main_generation_already_advanced in [false, true] {
        let fixture = fixture();
        let session_id = if main_generation_already_advanced {
            "session-crash-after-generation"
        } else {
            "session-crash-after-reservation"
        };
        let capability = open(&fixture, session_id);
        let registered = register(&fixture, session_id, &capability).1;
        let contract_digest = registered["data"]["contract_digest"]
            .as_str()
            .expect("contract digest");
        let admitted = admit_mutation(
            &fixture,
            session_id,
            &capability,
            contract_digest,
            "crash-mutation",
        );
        assert_eq!(admitted.0, 0, "envelope={}", admitted.1);
        let expected_generation = admitted.1["data"]["generation"]
            .as_u64()
            .expect("admitted generation");
        let (main_path, acceptance_path) = finish_line_state_paths(&fixture);
        let mut acceptance_state = read_json(&acceptance_path);
        let operation = acceptance_state["operations"]
            .as_object_mut()
            .expect("acceptance operations")
            .values_mut()
            .next()
            .expect("mutation operation");
        operation["admission"] = json!("reserved");
        for session in acceptance_state["sessions"]
            .as_object_mut()
            .expect("acceptance sessions")
            .values_mut()
        {
            session["claimed_sources"] = json!([support::sha256(b"pre-mutation-contained-source")]);
            session["claimed_sources_generation"] = json!(expected_generation - 1);
        }
        if !main_generation_already_advanced {
            let mut main_state = read_json(&main_path);
            main_state["generation"] = json!(expected_generation - 1);
            write_json(&main_path, &main_state);
        }
        write_json(&acceptance_path, &acceptance_state);

        let retried = admit_mutation(
            &fixture,
            session_id,
            &capability,
            contract_digest,
            "crash-mutation",
        );
        assert_eq!(retried.0, 0, "envelope={}", retried.1);
        assert_eq!(retried.1["data"]["status"], "duplicate");
        assert_eq!(retried.1["data"]["generation"], expected_generation);
        assert_eq!(read_json(&main_path)["generation"], expected_generation);
        let reconciled_state = read_json(&acceptance_path);
        assert!(
            reconciled_state["operations"]
                .as_object()
                .expect("acceptance operations")
                .values()
                .all(|operation| operation["admission"] == "admitted")
        );
        assert!(
            reconciled_state["sessions"]
                .as_object()
                .expect("acceptance sessions")
                .values()
                .all(|session| session["claimed_sources"]
                    .as_array()
                    .expect("claimed sources")
                    .is_empty()),
            "reconciliation must discard claims from the prior generation"
        );
        let duplicate = admit_mutation(
            &fixture,
            session_id,
            &capability,
            contract_digest,
            "crash-mutation",
        );
        assert_eq!(duplicate.0, 0, "envelope={}", duplicate.1);
        assert_eq!(read_json(&main_path)["generation"], expected_generation);
    }

    let fixture = fixture();
    let capability = open(&fixture, "session-crash-drift");
    let registered = register(&fixture, "session-crash-drift", &capability).1;
    let contract_digest = registered["data"]["contract_digest"]
        .as_str()
        .expect("contract digest");
    let admitted = admit_mutation(
        &fixture,
        "session-crash-drift",
        &capability,
        contract_digest,
        "drifted-mutation",
    );
    let reserved_generation = admitted.1["data"]["generation"]
        .as_u64()
        .expect("reserved generation");
    let (main_path, acceptance_path) = finish_line_state_paths(&fixture);
    let mut acceptance_state = read_json(&acceptance_path);
    acceptance_state["operations"]
        .as_object_mut()
        .expect("acceptance operations")
        .values_mut()
        .next()
        .expect("mutation operation")["admission"] = json!("reserved");
    write_json(&acceptance_path, &acceptance_state);
    let mut main_state = read_json(&main_path);
    main_state["generation"] = json!(reserved_generation + 2);
    write_json(&main_path, &main_state);
    let retry = admit_mutation(
        &fixture,
        "session-crash-drift",
        &capability,
        contract_digest,
        "drifted-mutation",
    );
    assert_eq!(retry.0, 69, "envelope={}", retry.1);
    assert_eq!(
        retry.1["error"]["code"],
        "finish-line-acceptance-admission-uncertain"
    );
    let (_, blocked) = verdict(
        &fixture,
        "session-crash-drift",
        &capability,
        contract_digest,
    );
    assert_eq!(blocked["data"]["aggregate"], "infrastructure-blocked");
}

#[test]
fn compaction_and_restart_preserve_current_generation_mutation_blocker() {
    let fixture = fixture();
    let capability = open(&fixture, "session-compaction");
    let registered = register(&fixture, "session-compaction", &capability).1;
    let contract_digest = registered["data"]["contract_digest"]
        .as_str()
        .expect("contract digest");
    admit_mutation(
        &fixture,
        "session-compaction",
        &capability,
        contract_digest,
        "uncertain-mutation",
    );
    observe(
        &fixture,
        "session-compaction",
        &capability,
        "uncertain-mutation",
        "uncertain",
    );
    admit_validator(
        &fixture,
        "session-compaction",
        &capability,
        contract_digest,
        "failed-validator",
    );
    observe(
        &fixture,
        "session-compaction",
        &capability,
        "failed-validator",
        "failed",
    );
    assert_eq!(
        verdict(&fixture, "session-compaction", &capability, contract_digest,).1["data"]["aggregate"],
        "uncertain"
    );

    let (_, acceptance_path) = finish_line_state_paths(&fixture);
    let mut state = read_json(&acceptance_path);
    let validator = state["operations"]
        .as_object()
        .expect("acceptance operations")
        .values()
        .find(|operation| operation["kind"]["kind"] == "validator")
        .expect("terminal validator")
        .clone();
    let operations = state["operations"]
        .as_object_mut()
        .expect("acceptance operations");
    for index in 0..300_u64 {
        let mut filler = validator.clone();
        filler["sequence"] = json!(10_000 + index);
        operations.insert(
            support::sha256(format!("compaction-filler-{index}").as_bytes()),
            filler,
        );
    }
    state["next_sequence"] = json!(20_000);
    write_json(&acceptance_path, &state);

    let after_compaction = admit_validator(
        &fixture,
        "session-compaction",
        &capability,
        contract_digest,
        "validator-after-compaction",
    );
    assert_eq!(after_compaction.0, 0, "envelope={}", after_compaction.1);
    observe(
        &fixture,
        "session-compaction",
        &capability,
        "validator-after-compaction",
        "failed",
    );
    let (_, blocked) = verdict(&fixture, "session-compaction", &capability, contract_digest);
    assert_eq!(blocked["data"]["aggregate"], "uncertain");
    assert!(
        read_json(&acceptance_path)["operations"]
            .as_object()
            .expect("acceptance operations")
            .values()
            .any(|operation| {
                operation["kind"]["kind"] == "mutation"
                    && operation["terminal"]["observation"] == "uncertain"
            })
    );
}

#[test]
fn generation_advance_prunes_contained_source_claim_history() {
    let fixture = fixture();
    let capability = open(&fixture, "session-claim-rollover");
    let registered = register(&fixture, "session-claim-rollover", &capability).1;
    let contract_digest = registered["data"]["contract_digest"]
        .as_str()
        .expect("contract digest");
    let (_, acceptance_path) = finish_line_state_paths(&fixture);
    let mut state = read_json(&acceptance_path);
    let session = state["sessions"]
        .as_object_mut()
        .expect("acceptance sessions")
        .values_mut()
        .next()
        .expect("acceptance session");
    session["claimed_sources"] = json!(
        (0..300_u64)
            .map(|index| support::sha256(format!("claimed-source-{index}").as_bytes()))
            .collect::<BTreeSet<_>>()
    );
    write_json(&acceptance_path, &state);

    let admitted = admit_mutation(
        &fixture,
        "session-claim-rollover",
        &capability,
        contract_digest,
        "claim-rollover-mutation",
    );
    assert_eq!(admitted.0, 0, "envelope={}", admitted.1);

    let state = read_json(&acceptance_path);
    let claimed_sources = state["sessions"]
        .as_object()
        .expect("acceptance sessions")
        .values()
        .next()
        .expect("acceptance session")["claimed_sources"]
        .as_array()
        .expect("claimed sources");
    assert!(
        claimed_sources.is_empty(),
        "a confirmed generation advance must discard prior-generation claims"
    );
}

#[test]
fn future_claim_generation_is_corrupt_and_never_pruned() {
    let fixture = fixture();
    let capability = open(&fixture, "session-future-claim-generation");
    let registered = register(&fixture, "session-future-claim-generation", &capability).1;
    let contract_digest = registered["data"]["contract_digest"]
        .as_str()
        .expect("contract digest");
    let (main_path, acceptance_path) = finish_line_state_paths(&fixture);
    let current_generation = read_json(&main_path)["generation"]
        .as_u64()
        .expect("main generation");
    let mut state = read_json(&acceptance_path);
    let session = state["sessions"]
        .as_object_mut()
        .expect("acceptance sessions")
        .values_mut()
        .next()
        .expect("acceptance session");
    session["claimed_sources"] = json!([support::sha256(b"reserved-future-source")]);
    session["claimed_sources_generation"] = json!(current_generation + 1);
    write_json(&acceptance_path, &state);
    let before = std::fs::read(&acceptance_path).expect("read corrupt acceptance state");

    let rejected = verdict(
        &fixture,
        "session-future-claim-generation",
        &capability,
        contract_digest,
    );
    assert_eq!(rejected.0, 65, "envelope={}", rejected.1);
    assert_eq!(
        rejected.1["error"]["code"],
        "finish-line-acceptance-state-invalid"
    );
    assert_eq!(
        std::fs::read(&acceptance_path).expect("reread corrupt acceptance state"),
        before,
        "future-generation corruption must not erase single-use claim evidence"
    );
}

#[test]
fn repository_wide_mutation_barrier_covers_other_sessions_and_ordinary_shell() {
    let fixture = fixture();
    install_bash_contract(&fixture, ":");
    let capability_a = open(&fixture, "session-barrier-a");
    let capability_b = open(&fixture, "session-barrier-b");
    let registered_a = register(&fixture, "session-barrier-a", &capability_a).1;
    let registered_b = register(&fixture, "session-barrier-b", &capability_b).1;
    let contract_a = registered_a["data"]["contract_digest"]
        .as_str()
        .expect("contract A");
    let contract_b = registered_b["data"]["contract_digest"]
        .as_str()
        .expect("contract B");
    let mutation = admit_mutation(
        &fixture,
        "session-barrier-a",
        &capability_a,
        contract_a,
        "mutation-a",
    );
    assert_eq!(mutation.0, 0, "envelope={}", mutation.1);

    let (cross_session_verdict_code, cross_session_verdict) =
        verdict(&fixture, "session-barrier-b", &capability_b, contract_b);
    assert_eq!(cross_session_verdict_code, 1);
    assert_eq!(
        cross_session_verdict["data"]["aggregate"], "active",
        "a live repository mutation must remain retryable from another session: {cross_session_verdict}"
    );

    let blocked_validator = admit_validator(
        &fixture,
        "session-barrier-b",
        &capability_b,
        contract_b,
        "validator-b-blocked",
    );
    assert_eq!(blocked_validator.0, 75, "envelope={}", blocked_validator.1);
    assert_eq!(
        blocked_validator.1["error"]["code"],
        "finish-line-acceptance-mutation-active"
    );
    let blocked_mutation = admit_mutation(
        &fixture,
        "session-barrier-b",
        &capability_b,
        contract_b,
        "mutation-b-blocked",
    );
    assert_eq!(blocked_mutation.0, 75, "envelope={}", blocked_mutation.1);

    let run_request = |operation_id: &str, command: &str, turn: &str| {
        let mut request = common(&fixture, "session-barrier-b", turn);
        request["schema_version"] = json!("agent-hook.finish-line.run.v1");
        request["operation_id"] = json!(operation_id);
        request["intent"] = json!("project-dev");
        request["command"] = json!(command);
        request["runner_capability"] = json!(capability_b);
        request["timeout_ms"] = json!(5_000);
        request["execution"] = json!({
            "kind": "bash-v1",
            "workdir": fixture.root,
            "output_max_bytes": 64 * 1024,
            "runner": {"kind": "danger-full-access"},
        });
        request
    };
    let blocked_validation = call(
        &fixture,
        "run",
        &run_request("validation-b-blocked", ":", "turn-validation-blocked"),
    );
    assert_eq!(
        blocked_validation.0, 75,
        "envelope={}",
        blocked_validation.1
    );
    assert_eq!(
        blocked_validation.1["error"]["code"],
        "finish-line-repository-mutation-active"
    );
    let blocked_shell = call(
        &fixture,
        "run",
        &run_request(
            "shell-b-blocked",
            "printf blocked > should-not-run",
            "turn-shell-blocked",
        ),
    );
    assert_eq!(blocked_shell.0, 75, "envelope={}", blocked_shell.1);
    assert!(!fixture.root.join("should-not-run").exists());

    observe(
        &fixture,
        "session-barrier-a",
        &capability_a,
        "mutation-a",
        "succeeded",
    );
    let ordinary = run_request(
        "ordinary-shell-active",
        "touch ordinary-started; sleep 0.4",
        "turn-ordinary-active",
    );
    let ordinary_result = thread::scope(|scope| {
        let handle = scope.spawn(|| call(&fixture, "run", &ordinary));
        let deadline = Instant::now() + Duration::from_secs(2);
        while !fixture.root.join("ordinary-started").exists() {
            assert!(Instant::now() < deadline, "ordinary shell did not start");
            thread::sleep(Duration::from_millis(5));
        }
        let validator = admit_validator(
            &fixture,
            "session-barrier-b",
            &capability_b,
            contract_b,
            "validator-during-shell",
        );
        assert_eq!(validator.0, 75, "envelope={}", validator.1);
        let mutation = admit_mutation(
            &fixture,
            "session-barrier-a",
            &capability_a,
            contract_a,
            "mutation-during-shell",
        );
        assert_eq!(mutation.0, 75, "envelope={}", mutation.1);
        let (_, blocked) = verdict(&fixture, "session-barrier-b", &capability_b, contract_b);
        assert_eq!(blocked["data"]["aggregate"], "active");
        handle.join().expect("ordinary shell thread")
    });
    assert_eq!(ordinary_result.0, 0, "envelope={}", ordinary_result.1);
}

#[test]
fn stale_duplicate_forged_and_drifting_provider_messages_fail_closed() {
    let fixture = fixture();
    let capability = open(&fixture, "session-a");
    let registered = register(&fixture, "session-a", &capability).1;
    let contract_digest = registered["data"]["contract_digest"]
        .as_str()
        .expect("contract digest");

    let (duplicate_code, duplicate) = register(&fixture, "session-a", &capability);
    assert_eq!(duplicate_code, 0, "envelope={duplicate}");
    assert_eq!(duplicate["data"]["status"], "duplicate");

    admit_validator(
        &fixture,
        "session-a",
        &capability,
        contract_digest,
        "validator-stale",
    );
    admit_mutation(
        &fixture,
        "session-a",
        &capability,
        contract_digest,
        "mutation-newer",
    );
    let (stale_code, stale) = observe(
        &fixture,
        "session-a",
        &capability,
        "validator-stale",
        "succeeded",
    );
    assert_eq!(stale_code, 0, "envelope={stale}");
    assert_eq!(stale["data"]["status"], "stale");
    let (replay_code, replay) = observe(
        &fixture,
        "session-a",
        &capability,
        "validator-stale",
        "succeeded",
    );
    assert_eq!(replay_code, 0, "envelope={replay}");
    assert_eq!(replay["data"]["status"], "duplicate");

    let (forged_code, forged) = observe(
        &fixture,
        "session-a",
        "finish-line-runner:forged",
        "mutation-newer",
        "succeeded",
    );
    assert_eq!(forged_code, 65, "envelope={forged}");
    assert_eq!(
        forged["error"]["code"],
        "finish-line-runner-capability-invalid"
    );

    let mut drift = common(&fixture, "session-a", "turn-drift");
    drift["schema_version"] = json!("agent-hook.finish-line.register.v1");
    drift["runner_capability"] = json!(capability);
    drift["requirements"] = json!([{
        "name": "different",
        "validators": [{
            "id": "runtime-plus-one",
            "tool_name": "runtime_kit_plus_one",
            "definition_digest": VALIDATOR_DEFINITION,
            "execution": {"kind": "host-observed"},
        }],
    }]);
    drift["invalidators"] = json!([]);
    let (drift_code, drifted) = call(&fixture, "register", &drift);
    assert_eq!(drift_code, 65, "envelope={drifted}");
    assert_eq!(
        drifted["error"]["code"],
        "finish-line-acceptance-contract-drift"
    );
}

#[test]
fn multiple_requirements_supersede_and_recover_deterministically() {
    let fixture = fixture();
    let capability = open(&fixture, "session-multi");
    let mut registration = common(&fixture, "session-multi", "turn-register");
    registration["schema_version"] = json!("agent-hook.finish-line.register.v1");
    registration["runner_capability"] = json!(capability);
    registration["requirements"] = json!([
        {
            "name": "alpha",
            "validators": [{
                "id": "alpha-validator",
                "tool_name": "alpha_tool",
                "definition_digest": VALIDATOR_DEFINITION,
                "execution": {"kind": "host-observed"},
            }],
        },
        {
            "name": "beta",
            "validators": [{
                "id": "beta-validator",
                "tool_name": "beta_tool",
                "definition_digest": SECOND_VALIDATOR_DEFINITION,
                "execution": {"kind": "host-observed"},
            }],
        },
    ]);
    registration["invalidators"] = json!([]);
    let (register_code, registered) = call(&fixture, "register", &registration);
    assert_eq!(register_code, 0, "envelope={registered}");
    let contract_digest = registered["data"]["contract_digest"]
        .as_str()
        .expect("contract digest");

    admit_validator_binding(
        &fixture,
        "session-multi",
        &capability,
        contract_digest,
        "alpha-old",
        "alpha",
        "alpha-validator",
        "alpha_tool",
        VALIDATOR_DEFINITION,
        None,
    );
    admit_validator_binding(
        &fixture,
        "session-multi",
        &capability,
        contract_digest,
        "alpha-current",
        "alpha",
        "alpha-validator",
        "alpha_tool",
        VALIDATOR_DEFINITION,
        None,
    );
    let (_, superseded) = observe(
        &fixture,
        "session-multi",
        &capability,
        "alpha-old",
        "succeeded",
    );
    assert_eq!(superseded["data"]["status"], "superseded");
    observe(
        &fixture,
        "session-multi",
        &capability,
        "alpha-current",
        "failed",
    );
    let (_, failed) = verdict(&fixture, "session-multi", &capability, contract_digest);
    assert_eq!(failed["data"]["aggregate"], "failed");
    assert_eq!(failed["data"]["requirements"][0]["name"], "alpha");
    assert_eq!(failed["data"]["requirements"][1]["name"], "beta");

    admit_validator_binding(
        &fixture,
        "session-multi",
        &capability,
        contract_digest,
        "beta-success",
        "beta",
        "beta-validator",
        "beta_tool",
        SECOND_VALIDATOR_DEFINITION,
        None,
    );
    observe(
        &fixture,
        "session-multi",
        &capability,
        "beta-success",
        "succeeded",
    );
    admit_validator_binding(
        &fixture,
        "session-multi",
        &capability,
        contract_digest,
        "alpha-uncertain",
        "alpha",
        "alpha-validator",
        "alpha_tool",
        VALIDATOR_DEFINITION,
        None,
    );
    observe(
        &fixture,
        "session-multi",
        &capability,
        "alpha-uncertain",
        "timed-out",
    );
    assert_eq!(
        verdict(&fixture, "session-multi", &capability, contract_digest).1["data"]["aggregate"],
        "uncertain"
    );
    admit_validator_binding(
        &fixture,
        "session-multi",
        &capability,
        contract_digest,
        "alpha-infrastructure",
        "alpha",
        "alpha-validator",
        "alpha_tool",
        VALIDATOR_DEFINITION,
        None,
    );
    observe(
        &fixture,
        "session-multi",
        &capability,
        "alpha-infrastructure",
        "infrastructure-blocked",
    );
    assert_eq!(
        verdict(&fixture, "session-multi", &capability, contract_digest).1["data"]["aggregate"],
        "infrastructure-blocked"
    );
    admit_validator_binding(
        &fixture,
        "session-multi",
        &capability,
        contract_digest,
        "alpha-recovered",
        "alpha",
        "alpha-validator",
        "alpha_tool",
        VALIDATOR_DEFINITION,
        None,
    );
    observe(
        &fixture,
        "session-multi",
        &capability,
        "alpha-recovered",
        "succeeded",
    );
    assert_eq!(
        verdict(&fixture, "session-multi", &capability, contract_digest).0,
        0
    );
}

#[test]
fn contained_bash_success_is_derived_only_from_the_exact_nils_run() {
    let fixture = fixture();
    let command = "printf 'contained\\n' > contained.marker";
    install_bash_contract(&fixture, command);
    let capability = open(&fixture, "session-contained");
    let mut registration = common(&fixture, "session-contained", "turn-register");
    registration["schema_version"] = json!("agent-hook.finish-line.register.v1");
    registration["runner_capability"] = json!(capability);
    registration["requirements"] = json!([{
        "name": "contained",
        "validators": [{
            "id": "contained-validator",
            "tool_name": "Bash",
            "definition_digest": VALIDATOR_DEFINITION,
            "execution": {
                "kind": "contained-bash",
                "intent": "project-dev",
                "command": command,
            },
        }],
    }]);
    registration["invalidators"] = json!([]);
    let (register_code, registered) = call(&fixture, "register", &registration);
    assert_eq!(register_code, 0, "envelope={registered}");
    let contract_digest = registered["data"]["contract_digest"]
        .as_str()
        .expect("contract digest");
    let (admit_code, admitted) = admit_validator_binding(
        &fixture,
        "session-contained",
        &capability,
        contract_digest,
        "acceptance-contained",
        "contained",
        "contained-validator",
        "Bash",
        VALIDATOR_DEFINITION,
        Some("nils-contained-run"),
    );
    assert_eq!(admit_code, 0, "envelope={admitted}");

    let (forged_code, forged) = observe(
        &fixture,
        "session-contained",
        &capability,
        "acceptance-contained",
        "succeeded",
    );
    assert_eq!(forged_code, 65, "envelope={forged}");
    assert_eq!(
        forged["error"]["code"],
        "finish-line-acceptance-observation-source-invalid"
    );

    let mut run = common(&fixture, "session-contained", "turn-contained-run");
    run["schema_version"] = json!("agent-hook.finish-line.run.v1");
    run["operation_id"] = json!("nils-contained-run");
    run["intent"] = json!("project-dev");
    run["command"] = json!(command);
    run["runner_capability"] = json!(capability);
    run["timeout_ms"] = json!(5_000);
    run["execution"] = json!({
        "kind": "bash-v1",
        "workdir": fixture.root,
        "output_max_bytes": 64 * 1024,
        "runner": {"kind": "danger-full-access"},
    });
    let (run_code, executed) = call(&fixture, "run", &run);
    assert_eq!(run_code, 0, "envelope={executed}");
    assert_eq!(executed["data"]["status"], "applied");

    let (observe_code, observed) = observe_contained(
        &fixture,
        "session-contained",
        &capability,
        "acceptance-contained",
        "nils-contained-run",
    );
    assert_eq!(observe_code, 0, "envelope={observed}");
    assert_eq!(observed["data"]["observation"], "succeeded");
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("contained.marker")).expect("contained marker"),
        "contained\n"
    );
    assert_eq!(
        verdict(&fixture, "session-contained", &capability, contract_digest,).0,
        0
    );
}

#[test]
fn contained_validator_terminalizes_stale_after_a_generation_race() {
    let fixture = fixture();
    let command = ":";
    install_bash_contract(&fixture, command);
    let capability = open(&fixture, "session-contained-stale");
    let mut registration = common(&fixture, "session-contained-stale", "turn-register");
    registration["schema_version"] = json!("agent-hook.finish-line.register.v1");
    registration["runner_capability"] = json!(capability);
    registration["requirements"] = json!([{
        "name": "contained",
        "validators": [{
            "id": "contained-validator",
            "tool_name": "Bash",
            "definition_digest": VALIDATOR_DEFINITION,
            "execution": {
                "kind": "contained-bash",
                "intent": "project-dev",
                "command": command,
            },
        }],
    }]);
    registration["invalidators"] = json!([{
        "tool_name": "edit",
        "definition_digest": MUTATION_DEFINITION,
    }]);
    let registered = call(&fixture, "register", &registration).1;
    let contract_digest = registered["data"]["contract_digest"]
        .as_str()
        .expect("contract digest");
    let admitted = admit_validator_binding(
        &fixture,
        "session-contained-stale",
        &capability,
        contract_digest,
        "acceptance-contained-stale",
        "contained",
        "contained-validator",
        "Bash",
        VALIDATOR_DEFINITION,
        Some("nils-contained-stale-run"),
    );
    assert_eq!(admitted.0, 0, "envelope={}", admitted.1);
    let mutation = admit_mutation(
        &fixture,
        "session-contained-stale",
        &capability,
        contract_digest,
        "mutation-after-contained-admission",
    );
    assert_eq!(mutation.0, 0, "envelope={}", mutation.1);
    let mutation_terminal = observe(
        &fixture,
        "session-contained-stale",
        &capability,
        "mutation-after-contained-admission",
        "succeeded",
    );
    assert_eq!(mutation_terminal.0, 0, "envelope={}", mutation_terminal.1);

    let mut run = common(
        &fixture,
        "session-contained-stale",
        "turn-contained-stale-run",
    );
    run["schema_version"] = json!("agent-hook.finish-line.run.v1");
    run["operation_id"] = json!("nils-contained-stale-run");
    run["intent"] = json!("project-dev");
    run["command"] = json!(command);
    run["runner_capability"] = json!(capability);
    run["timeout_ms"] = json!(5_000);
    run["execution"] = json!({
        "kind": "bash-v1",
        "workdir": fixture.root,
        "output_max_bytes": 64 * 1024,
        "runner": {"kind": "danger-full-access"},
    });
    let executed = call(&fixture, "run", &run);
    assert_eq!(executed.0, 0, "envelope={}", executed.1);

    let observed = observe_contained(
        &fixture,
        "session-contained-stale",
        &capability,
        "acceptance-contained-stale",
        "nils-contained-stale-run",
    );
    assert_eq!(observed.0, 0, "envelope={}", observed.1);
    assert_eq!(observed.1["data"]["status"], "stale");
    assert_eq!(observed.1["data"]["observation"], "uncertain");
    let released = release(&fixture, "session-contained-stale", &capability);
    assert_eq!(released.0, 0, "envelope={}", released.1);
}

#[test]
fn contained_source_must_be_future_exact_and_single_use() {
    let fixture = fixture();
    let command = ":";
    install_bash_contract(&fixture, command);
    let capability = open(&fixture, "session-contained-source");
    let mut registration = common(&fixture, "session-contained-source", "turn-register");
    registration["schema_version"] = json!("agent-hook.finish-line.register.v1");
    registration["runner_capability"] = json!(capability);
    registration["requirements"] = json!([
        {
            "name": "alpha",
            "validators": [{
                "id": "alpha-validator",
                "tool_name": "Bash",
                "definition_digest": VALIDATOR_DEFINITION,
                "execution": {"kind": "contained-bash", "intent": "project-dev", "command": command},
            }],
        },
        {
            "name": "beta",
            "validators": [{
                "id": "beta-validator",
                "tool_name": "Bash",
                "definition_digest": VALIDATOR_DEFINITION,
                "execution": {"kind": "contained-bash", "intent": "project-dev", "command": command},
            }],
        },
    ]);
    registration["invalidators"] = json!([]);
    let registered = call(&fixture, "register", &registration).1;
    let contract_digest = registered["data"]["contract_digest"]
        .as_str()
        .expect("contract digest");
    let run = |operation_id: &str, turn_id: &str| {
        let mut request = common(&fixture, "session-contained-source", turn_id);
        request["schema_version"] = json!("agent-hook.finish-line.run.v1");
        request["operation_id"] = json!(operation_id);
        request["intent"] = json!("project-dev");
        request["command"] = json!(command);
        request["runner_capability"] = json!(capability);
        request["timeout_ms"] = json!(5_000);
        request["execution"] = json!({
            "kind": "bash-v1",
            "workdir": fixture.root,
            "output_max_bytes": 64 * 1024,
            "runner": {"kind": "danger-full-access"},
        });
        call(&fixture, "run", &request)
    };

    let old_run = run("source-before-admission", "turn-old-run");
    assert_eq!(old_run.0, 0, "envelope={}", old_run.1);
    let replay = admit_validator_binding(
        &fixture,
        "session-contained-source",
        &capability,
        contract_digest,
        "alpha-replay",
        "alpha",
        "alpha-validator",
        "Bash",
        VALIDATOR_DEFINITION,
        Some("source-before-admission"),
    );
    assert_eq!(replay.0, 65, "envelope={}", replay.1);
    assert_eq!(
        replay.1["error"]["code"],
        "finish-line-acceptance-source-operation-exists"
    );

    let alpha = admit_validator_binding(
        &fixture,
        "session-contained-source",
        &capability,
        contract_digest,
        "alpha-current",
        "alpha",
        "alpha-validator",
        "Bash",
        VALIDATOR_DEFINITION,
        Some("shared-source"),
    );
    assert_eq!(alpha.0, 0, "envelope={}", alpha.1);
    let reused = admit_validator_binding(
        &fixture,
        "session-contained-source",
        &capability,
        contract_digest,
        "beta-current",
        "beta",
        "beta-validator",
        "Bash",
        VALIDATOR_DEFINITION,
        Some("shared-source"),
    );
    assert_eq!(reused.0, 65, "envelope={}", reused.1);
    assert_eq!(
        reused.1["error"]["code"],
        "finish-line-acceptance-source-operation-claimed"
    );

    let current_run = run("shared-source", "turn-current-run");
    assert_eq!(current_run.0, 0, "envelope={}", current_run.1);
    let duplicate = admit_validator_binding(
        &fixture,
        "session-contained-source",
        &capability,
        contract_digest,
        "alpha-current",
        "alpha",
        "alpha-validator",
        "Bash",
        VALIDATOR_DEFINITION,
        Some("shared-source"),
    );
    assert_eq!(duplicate.0, 0, "envelope={}", duplicate.1);
    assert_eq!(duplicate.1["data"]["status"], "duplicate");
    let observed = observe_contained(
        &fixture,
        "session-contained-source",
        &capability,
        "alpha-current",
        "shared-source",
    );
    assert_eq!(observed.0, 0, "envelope={}", observed.1);
    assert_eq!(observed.1["data"]["observation"], "succeeded");
}

#[test]
fn contained_source_claims_fail_closed_at_a_bounded_generation_limit() {
    let fixture = fixture();
    let command = ":";
    install_bash_contract(&fixture, command);
    let capability = open(&fixture, "session-contained-claim-limit");
    let mut registration = common(&fixture, "session-contained-claim-limit", "turn-register");
    registration["schema_version"] = json!("agent-hook.finish-line.register.v1");
    registration["runner_capability"] = json!(capability);
    registration["requirements"] = json!([{
        "name": "contained",
        "validators": [{
            "id": "contained-validator",
            "tool_name": "Bash",
            "definition_digest": VALIDATOR_DEFINITION,
            "execution": {"kind": "contained-bash", "intent": "project-dev", "command": command},
        }],
    }]);
    registration["invalidators"] = json!([]);
    let registered = call(&fixture, "register", &registration).1;
    let contract_digest = registered["data"]["contract_digest"]
        .as_str()
        .expect("contract digest");
    let (_, acceptance_path) = finish_line_state_paths(&fixture);
    let mut state = read_json(&acceptance_path);
    let session = state["sessions"]
        .as_object_mut()
        .expect("acceptance sessions")
        .values_mut()
        .next()
        .expect("acceptance session");
    session["claimed_sources"] = json!(
        (0..512_u64)
            .map(|index| support::sha256(format!("bounded-claim-{index}").as_bytes()))
            .collect::<BTreeSet<_>>()
    );
    write_json(&acceptance_path, &state);

    let rejected = admit_validator_binding(
        &fixture,
        "session-contained-claim-limit",
        &capability,
        contract_digest,
        "validator-over-claim-limit",
        "contained",
        "contained-validator",
        "Bash",
        VALIDATOR_DEFINITION,
        Some("future-source-over-limit"),
    );
    assert_eq!(rejected.0, 65, "envelope={}", rejected.1);
    assert_eq!(rejected.1["error"]["code"], "finish-line-state-limit");
}

#[test]
fn contained_bash_provider_failure_terminalizes_acceptance_as_infrastructure_blocked() {
    let fixture = fixture();
    let command = ":";
    install_bash_contract(&fixture, command);
    let capability = open(&fixture, "session-contained-failure");
    let mut registration = common(&fixture, "session-contained-failure", "turn-register");
    registration["schema_version"] = json!("agent-hook.finish-line.register.v1");
    registration["runner_capability"] = json!(capability);
    registration["requirements"] = json!([{
        "name": "contained",
        "validators": [{
            "id": "contained-validator",
            "tool_name": "Bash",
            "definition_digest": VALIDATOR_DEFINITION,
            "execution": {
                "kind": "contained-bash",
                "intent": "project-dev",
                "command": command,
            },
        }],
    }]);
    registration["invalidators"] = json!([]);
    let registered = call(&fixture, "register", &registration).1;
    let contract_digest = registered["data"]["contract_digest"]
        .as_str()
        .expect("contract digest");
    let admitted = admit_validator_binding(
        &fixture,
        "session-contained-failure",
        &capability,
        contract_digest,
        "acceptance-contained-failure",
        "contained",
        "contained-validator",
        "Bash",
        VALIDATOR_DEFINITION,
        Some("nils-contained-failure-run"),
    );
    assert_eq!(admitted.0, 0, "envelope={}", admitted.1);

    let provider = fixture.root.join("failing-provider-runner");
    std::fs::write(
        &provider,
        "#!/bin/sh\nprintf 'fake-runner: profile rejected\\n' >&2\nexit 125\n",
    )
    .expect("failing provider runner");
    std::fs::set_permissions(&provider, std::fs::Permissions::from_mode(0o700))
        .expect("provider permissions");
    let mut run = common(
        &fixture,
        "session-contained-failure",
        "turn-contained-failure-run",
    );
    run["schema_version"] = json!("agent-hook.finish-line.run.v1");
    run["operation_id"] = json!("nils-contained-failure-run");
    run["intent"] = json!("project-dev");
    run["command"] = json!(command);
    run["runner_capability"] = json!(capability);
    run["timeout_ms"] = json!(5_000);
    run["execution"] = json!({
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
    let failed = call(&fixture, "run", &run);
    assert_ne!(failed.0, 0, "envelope={}", failed.1);
    assert_eq!(
        failed.1["error"]["code"],
        "finish-line-sandbox-runner-failed"
    );
    assert_eq!(
        verdict(
            &fixture,
            "session-contained-failure",
            &capability,
            contract_digest,
        )
        .1["data"]["aggregate"],
        "infrastructure-blocked"
    );

    let quiesced = quiesce(
        &fixture,
        "session-contained-failure",
        "turn-contained-failure-run",
        &capability,
        "nils-contained-failure-run",
    );
    assert_eq!(quiesced.0, 0, "envelope={}", quiesced.1);
    let observed = observe_contained(
        &fixture,
        "session-contained-failure",
        &capability,
        "acceptance-contained-failure",
        "nils-contained-failure-run",
    );
    assert_eq!(observed.0, 0, "envelope={}", observed.1);
    assert_eq!(observed.1["data"]["status"], "duplicate");
    assert_eq!(observed.1["data"]["observation"], "infrastructure-blocked");
    let released = release(&fixture, "session-contained-failure", &capability);
    assert_eq!(released.0, 0, "envelope={}", released.1);
}

#[test]
fn killed_contained_supervisor_quiesces_to_infrastructure_blocked_terminal() {
    let fixture = fixture();
    let command = "touch acceptance-validation-started; sleep 5; touch acceptance-late-write";
    install_bash_contract(&fixture, command);
    let capability = open(&fixture, "session-contained-killed");
    let mut registration = common(&fixture, "session-contained-killed", "turn-register");
    registration["schema_version"] = json!("agent-hook.finish-line.register.v1");
    registration["runner_capability"] = json!(capability);
    registration["requirements"] = json!([{
        "name": "contained",
        "validators": [{
            "id": "contained-validator",
            "tool_name": "Bash",
            "definition_digest": VALIDATOR_DEFINITION,
            "execution": {
                "kind": "contained-bash",
                "intent": "project-dev",
                "command": command,
            },
        }],
    }]);
    registration["invalidators"] = json!([]);
    let registered = call(&fixture, "register", &registration).1;
    let contract_digest = registered["data"]["contract_digest"]
        .as_str()
        .expect("contract digest");
    let admitted = admit_validator_binding(
        &fixture,
        "session-contained-killed",
        &capability,
        contract_digest,
        "acceptance-contained-killed",
        "contained",
        "contained-validator",
        "Bash",
        VALIDATOR_DEFINITION,
        Some("nils-contained-killed-run"),
    );
    assert_eq!(admitted.0, 0, "envelope={}", admitted.1);

    let mut request = common(
        &fixture,
        "session-contained-killed",
        "turn-contained-killed-run",
    );
    request["schema_version"] = json!("agent-hook.finish-line.run.v1");
    request["operation_id"] = json!("nils-contained-killed-run");
    request["intent"] = json!("project-dev");
    request["command"] = json!(command);
    request["runner_capability"] = json!(capability);
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
        .expect("spawn contained validation supervisor");
    child
        .stdin
        .take()
        .expect("validation stdin")
        .write_all(request.to_string().as_bytes())
        .expect("write validation request");
    let deadline = Instant::now() + Duration::from_secs(2);
    while !fixture.root.join("acceptance-validation-started").exists() {
        assert!(
            Instant::now() < deadline,
            "contained validation did not start"
        );
        thread::sleep(Duration::from_millis(5));
    }
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
    assert!(!child.wait().expect("reap killed supervisor").success());

    let quiesced = quiesce(
        &fixture,
        "session-contained-killed",
        "turn-contained-killed-run",
        &capability,
        "nils-contained-killed-run",
    );
    assert_eq!(quiesced.0, 0, "envelope={}", quiesced.1);
    thread::sleep(Duration::from_millis(250));
    assert!(!fixture.root.join("acceptance-late-write").exists());
    let (_, blocked) = verdict(
        &fixture,
        "session-contained-killed",
        &capability,
        contract_digest,
    );
    assert_eq!(blocked["data"]["aggregate"], "infrastructure-blocked");
    let observed = observe_contained(
        &fixture,
        "session-contained-killed",
        &capability,
        "acceptance-contained-killed",
        "nils-contained-killed-run",
    );
    assert_eq!(observed.0, 0, "envelope={}", observed.1);
    assert_eq!(observed.1["data"]["status"], "duplicate");
    let released = release(&fixture, "session-contained-killed", &capability);
    assert_eq!(released.0, 0, "envelope={}", released.1);
}

#[test]
fn durable_sidecar_survives_release_without_leaking_provider_material() {
    let fixture = fixture();
    let capability = open(&fixture, "session-resume");
    let registered = register(&fixture, "session-resume", &capability).1;
    let contract_digest = registered["data"]["contract_digest"]
        .as_str()
        .expect("contract digest");
    admit_validator(
        &fixture,
        "session-resume",
        &capability,
        contract_digest,
        "validator-secret-operation",
    );
    observe(
        &fixture,
        "session-resume",
        &capability,
        "validator-secret-operation",
        "succeeded",
    );

    let repo_state_dir = fixture.state_home.join("agent-hook/finish-line/repos");
    let sidecar = std::fs::read_dir(&repo_state_dir)
        .expect("finish-line state directory")
        .map(|entry| entry.expect("finish-line state entry").path())
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(".acceptance.json"))
        })
        .expect("acceptance sidecar");
    let sidecar_text = std::fs::read_to_string(&sidecar).expect("acceptance state");
    let sidecar_metadata = std::fs::symlink_metadata(&sidecar).expect("acceptance metadata");
    assert!(sidecar_metadata.is_file());
    assert_eq!(sidecar_metadata.permissions().mode() & 0o077, 0);
    assert_eq!(sidecar_metadata.nlink(), 1);
    for protected in [
        "session-resume",
        "validator-secret-operation",
        "runtime_kit_plus_one",
        VALIDATOR_DEFINITION,
        "private-open:session-resume",
    ] {
        assert!(!sidecar_text.contains(protected), "leaked {protected}");
    }
    let main_state = std::fs::read_dir(&repo_state_dir)
        .expect("finish-line state directory")
        .map(|entry| entry.expect("finish-line state entry").path())
        .find(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
                && path != &sidecar
        })
        .expect("released finish-line state");
    assert!(
        !std::fs::read_to_string(main_state)
            .expect("released state")
            .contains("acceptance"),
        "released rollback state schema was changed"
    );

    let (release_code, released) = release(&fixture, "session-resume", &capability);
    assert_eq!(release_code, 0, "envelope={released}");
    let resumed_capability = open(&fixture, "session-resume");
    assert_ne!(resumed_capability, capability);
    let (register_code, duplicate) = register(&fixture, "session-resume", &resumed_capability);
    assert_eq!(register_code, 0, "envelope={duplicate}");
    assert_eq!(duplicate["data"]["status"], "duplicate");
    assert_eq!(
        verdict(
            &fixture,
            "session-resume",
            &resumed_capability,
            contract_digest,
        )
        .0,
        0
    );
}

#[test]
#[ignore = "requires NILS_AGENT_HOOK_1279_BIN pointing to the exact 1.27.9 baseline binary"]
fn exact_1279_reader_round_trip_preserves_fail_closed_upgrade_semantics() {
    let baseline = std::env::var_os("NILS_AGENT_HOOK_1279_BIN")
        .map(PathBuf::from)
        .expect("NILS_AGENT_HOOK_1279_BIN");
    let fixture = fixture();
    let capability = open(&fixture, "session-exact-rollback");
    let registered = register(&fixture, "session-exact-rollback", &capability).1;
    let contract_digest = registered["data"]["contract_digest"]
        .as_str()
        .expect("contract digest");
    admit_mutation(
        &fixture,
        "session-exact-rollback",
        &capability,
        contract_digest,
        "rollback-mutation",
    );
    observe(
        &fixture,
        "session-exact-rollback",
        &capability,
        "rollback-mutation",
        "succeeded",
    );
    admit_validator(
        &fixture,
        "session-exact-rollback",
        &capability,
        contract_digest,
        "rollback-validator",
    );
    observe(
        &fixture,
        "session-exact-rollback",
        &capability,
        "rollback-validator",
        "succeeded",
    );
    assert_eq!(
        verdict(
            &fixture,
            "session-exact-rollback",
            &capability,
            contract_digest,
        )
        .0,
        0
    );

    let run_baseline = |command: &str, request: Value| {
        let mut child = Command::new(&baseline)
            .args(["finish-line", command, "--format", "json"])
            .current_dir(&fixture.root)
            .env("HOME", &fixture.home)
            .env("XDG_CONFIG_HOME", &fixture.config_home)
            .env("XDG_STATE_HOME", &fixture.state_home)
            .env("AGENT_SESSION_STATE_DIR", &fixture.session_state)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn exact baseline agent-hook");
        child
            .stdin
            .take()
            .expect("baseline stdin")
            .write_all(request.to_string().as_bytes())
            .expect("write baseline request");
        let output = child.wait_with_output().expect("baseline output");
        let envelope: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
            panic!(
                "baseline JSON: {error}; stdout={} stderr={}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
        });
        (output.status.code().unwrap_or(255), envelope)
    };

    let mut stop = common(&fixture, "session-exact-rollback", "turn-baseline-stop");
    stop["schema_version"] = json!("agent-hook.finish-line.stop.v1");
    let baseline_stop = run_baseline("stop", stop);
    assert_eq!(baseline_stop.0, 1, "envelope={}", baseline_stop.1);
    assert_eq!(baseline_stop.1["data"]["action"], "block");

    let mut status = common(&fixture, "session-exact-rollback", "turn-baseline-status");
    status["schema_version"] = json!("agent-hook.finish-line.status.v1");
    let baseline_status = run_baseline("status", status);
    assert_eq!(baseline_status.0, 0, "envelope={}", baseline_status.1);
    assert_eq!(baseline_status.1["data"]["generation"], 1);

    let mut edit = common(&fixture, "session-exact-rollback", "turn-baseline-edit");
    edit["schema_version"] = json!("agent-hook.finish-line.begin.v1");
    edit["operation_id"] = json!("baseline-edit");
    edit["attempt_token"] = json!("baseline-edit-private-token");
    edit["operation"] = json!({"kind": "edit"});
    let baseline_edit = run_baseline("begin", edit);
    assert_eq!(baseline_edit.0, 0, "envelope={}", baseline_edit.1);
    assert_eq!(baseline_edit.1["data"]["generation"], 2);

    let upgraded = verdict(
        &fixture,
        "session-exact-rollback",
        &capability,
        contract_digest,
    );
    assert_eq!(upgraded.0, 1, "envelope={}", upgraded.1);
    assert_eq!(upgraded.1["data"]["aggregate"], "missing");
}

#[test]
fn older_incarnation_active_acceptance_blocks_release_after_earlier_client_reopen() {
    let fixture = fixture();
    let capability = open(&fixture, "session-earlier-client-reopen");
    let registered = register(&fixture, "session-earlier-client-reopen", &capability).1;
    let contract_digest = registered["data"]["contract_digest"]
        .as_str()
        .expect("contract digest");
    admit_validator(
        &fixture,
        "session-earlier-client-reopen",
        &capability,
        contract_digest,
        "earlier-client-active-validator",
    );

    let repo_state_dir = fixture.state_home.join("agent-hook/finish-line/repos");
    let main_state = std::fs::read_dir(&repo_state_dir)
        .expect("finish-line state directory")
        .map(|entry| entry.expect("finish-line state entry").path())
        .find(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
                && !path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.ends_with(".acceptance.json"))
        })
        .expect("main finish-line state");
    let mut earlier_state: Value =
        serde_json::from_slice(&std::fs::read(&main_state).expect("read main finish-line state"))
            .expect("parse main finish-line state");
    earlier_state["sessions"] = json!({});
    std::fs::write(
        &main_state,
        serde_json::to_vec(&earlier_state).expect("serialize earlier state"),
    )
    .expect("simulate earlier-client release");

    let reopened_capability = open(&fixture, "session-earlier-client-reopen");
    assert_ne!(reopened_capability, capability);
    let duplicate = register(
        &fixture,
        "session-earlier-client-reopen",
        &reopened_capability,
    );
    assert_eq!(duplicate.0, 0, "envelope={}", duplicate.1);
    assert_eq!(duplicate.1["data"]["status"], "duplicate");
    assert_eq!(
        verdict(
            &fixture,
            "session-earlier-client-reopen",
            &reopened_capability,
            contract_digest,
        )
        .1["data"]["aggregate"],
        "infrastructure-blocked"
    );
    let blocked = release(
        &fixture,
        "session-earlier-client-reopen",
        &reopened_capability,
    );
    assert_ne!(blocked.0, 0, "envelope={}", blocked.1);
    assert_eq!(blocked.1["error"]["code"], "finish-line-session-busy");
}

#[test]
fn concurrent_validator_admissions_serialize_and_only_the_newest_attempt_applies() {
    let fixture = fixture();
    let capability = open(&fixture, "session-concurrent");
    let registered = register(&fixture, "session-concurrent", &capability).1;
    let contract_digest = registered["data"]["contract_digest"]
        .as_str()
        .expect("contract digest")
        .to_string();

    let request = |operation_id: &str| {
        let mut request = common(&fixture, "session-concurrent", "turn-concurrent");
        request["schema_version"] = json!("agent-hook.finish-line.admit.v1");
        request["runner_capability"] = json!(capability);
        request["contract_digest"] = json!(contract_digest);
        request["operation_id"] = json!(operation_id);
        request["attempt_token"] = json!(format!("concurrent-token:{operation_id}"));
        request["operation"] = json!({
            "kind": "validator",
            "requirement": "unit",
            "validator_id": "runtime-plus-one",
            "tool_name": "runtime_kit_plus_one",
            "definition_digest": VALIDATOR_DEFINITION,
        });
        request
    };
    let first = request("concurrent-a");
    let second = request("concurrent-b");
    let barrier = Arc::new(Barrier::new(3));
    let (first_result, second_result) = thread::scope(|scope| {
        let first_barrier = Arc::clone(&barrier);
        let first_fixture = &fixture;
        let first_handle = scope.spawn(move || {
            first_barrier.wait();
            call(first_fixture, "admit", &first)
        });
        let second_barrier = Arc::clone(&barrier);
        let second_fixture = &fixture;
        let second_handle = scope.spawn(move || {
            second_barrier.wait();
            call(second_fixture, "admit", &second)
        });
        barrier.wait();
        (
            first_handle.join().expect("first admission thread"),
            second_handle.join().expect("second admission thread"),
        )
    });
    assert_eq!(first_result.0, 0, "envelope={}", first_result.1);
    assert_eq!(second_result.0, 0, "envelope={}", second_result.1);

    let first_observed = observe(
        &fixture,
        "session-concurrent",
        &capability,
        "concurrent-a",
        "succeeded",
    );
    let second_observed = observe(
        &fixture,
        "session-concurrent",
        &capability,
        "concurrent-b",
        "succeeded",
    );
    assert_eq!(first_observed.0, 0, "envelope={}", first_observed.1);
    assert_eq!(second_observed.0, 0, "envelope={}", second_observed.1);
    let statuses = [
        first_observed.1["data"]["status"]
            .as_str()
            .expect("first disposition"),
        second_observed.1["data"]["status"]
            .as_str()
            .expect("second disposition"),
    ]
    .into_iter()
    .collect::<BTreeSet<_>>();
    assert_eq!(statuses, BTreeSet::from(["applied", "superseded"]));
    assert_eq!(
        verdict(
            &fixture,
            "session-concurrent",
            &capability,
            &contract_digest,
        )
        .0,
        0
    );
}
