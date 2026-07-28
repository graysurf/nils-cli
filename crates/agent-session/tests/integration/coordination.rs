use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use nils_test_support::bin;
use nils_test_support::cmd::{CmdOptions, CmdOutput, run_resolved};
use pretty_assertions::assert_eq;
use serde_json::json;
use sha2::{Digest, Sha256};

use super::cli::{
    TestProcessGroup, fake_agent, fake_tmux, spawn_scoped_test_process_group, tmux_calls,
};

fn run(dir: &Path, args: &[&str]) -> CmdOutput {
    run_resolved("agent-session", args, &CmdOptions::new().with_cwd(dir))
}

fn run_with_env(dir: &Path, args: &[&str], envs: &[(&str, &str)]) -> CmdOutput {
    run_resolved(
        "agent-session",
        args,
        &CmdOptions::new().with_cwd(dir).with_envs(envs),
    )
}

fn write_trusted_codex_config(codex_home: &Path, projects: &[&Path]) {
    let canonical_projects = projects
        .iter()
        .map(|project| fs::canonicalize(project).expect("canonical test checkout"))
        .collect::<BTreeSet<_>>();
    let mut config = String::new();
    for project in canonical_projects {
        let project_key = toml_edit::Value::from(
            project
                .to_str()
                .expect("test checkout must have a UTF-8 path"),
        );
        config.push_str(&format!(
            "[projects.{project_key}]\ntrust_level = \"trusted\"\n"
        ));
    }
    fs::create_dir_all(codex_home).expect("test Codex home");
    let config_path = codex_home.join("config.toml");
    fs::write(&config_path, config).expect("test Codex config");
    fs::set_permissions(&config_path, fs::Permissions::from_mode(0o600))
        .expect("test Codex config mode");
}

fn run_main_agent(dir: &Path, args: &[&str], envs: &[(&str, &str)]) -> CmdOutput {
    let mut options = CmdOptions::new().with_cwd(dir).with_envs(envs);
    if !envs
        .iter()
        .any(|(key, _)| *key == "AGENT_SESSION_CHECKPOINT_FILE")
        && let Some((_, capability_file)) = envs
            .iter()
            .find(|(key, value)| *key == "AGENT_SESSION_CAPABILITY_FILE" && !value.is_empty())
    {
        let capability_path = Path::new(capability_file);
        if let Some(name) = capability_path
            .file_name()
            .and_then(|value| value.to_str())
            .and_then(|value| value.strip_prefix("capability-"))
        {
            let checkpoint_path =
                capability_path.with_file_name(format!("main-agent-checkpoint-{name}.json"));
            options = options.with_env(
                "AGENT_SESSION_CHECKPOINT_FILE",
                checkpoint_path.to_str().expect("checkpoint path"),
            );
        }
    }
    run_resolved("main-agent", args, &options)
}

fn run_main_agent_with_codex_trust(
    dir: &Path,
    args: &[&str],
    envs: &[(&str, &str)],
    codex_home: &Path,
    projects: &[&Path],
) -> CmdOutput {
    assert!(
        !envs.iter().any(|(key, _)| *key == "CODEX_HOME"),
        "explicit Codex trust helper owns CODEX_HOME"
    );
    write_trusted_codex_config(codex_home, projects);
    let codex_home_arg = codex_home.to_string_lossy().into_owned();
    let mut trusted_envs = envs.to_vec();
    trusted_envs.push(("CODEX_HOME", codex_home_arg.as_str()));
    run_main_agent(dir, args, &trusted_envs)
}

fn run_main_agent_without_checkpoint(
    dir: &Path,
    args: &[&str],
    envs: &[(&str, &str)],
) -> CmdOutput {
    run_resolved(
        "main-agent",
        args,
        &CmdOptions::new().with_cwd(dir).with_envs(envs),
    )
}

fn run_managed_account_handoff(
    dir: &Path,
    state_dir: &Path,
    capability_file: &str,
    account: &str,
    idempotency_key: &str,
    apply_result: &str,
) -> CmdOutput {
    let registry = orchestration_registry(state_dir);
    let assignment = &registry["assignments"]["assignment-matrix"];
    let revision = assignment["account_handoff"]
        .as_object()
        .filter(|reservation| {
            reservation
                .get("account")
                .and_then(serde_json::Value::as_str)
                == Some(account)
        })
        .and_then(|reservation| reservation.get("reserved_revision"))
        .and_then(serde_json::Value::as_u64)
        .unwrap_or_else(|| {
            assignment["revision"]
                .as_u64()
                .expect("assignment revision")
        })
        .to_string();
    run_main_agent(
        dir,
        &[
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "worker",
            "account-handoff",
            "assignment-matrix",
            "--account",
            account,
            "--if-revision",
            &revision,
            "--authorize-account-change",
            "--timeout",
            "1s",
            "--idempotency-key",
            idempotency_key,
            "--format",
            "json",
        ],
        &[
            ("AGENT_SESSION_CAPABILITY_FILE", capability_file),
            ("AGENT_SESSION_CODEX_ACCOUNT_BROKER", r#"["/bin/false"]"#),
            (
                "NILS_AGENT_SESSION_TEST_ACCOUNT_HANDOFF_APPLY_RESULT",
                apply_result,
            ),
        ],
    )
}

fn run_managed_account_handoff_cancel(
    dir: &Path,
    state_dir: &Path,
    capability_file: &str,
    idempotency_key: &str,
) -> CmdOutput {
    let registry = orchestration_registry(state_dir);
    let assignment = &registry["assignments"]["assignment-matrix"];
    let reservation = &assignment["account_handoff"];
    let revision = assignment["revision"]
        .as_u64()
        .expect("assignment revision")
        .to_string();
    let reservation_id = reservation["reservation_id"]
        .as_str()
        .or_else(|| reservation["request_digest"].as_str())
        .expect("reservation identity")
        .to_string();
    let account = reservation["account"]
        .as_str()
        .expect("reserved account")
        .to_string();
    let intent_id = reservation["account_intent_id"]
        .as_str()
        .map(str::to_string);
    run_managed_account_handoff_cancel_with_identity(
        dir,
        state_dir,
        capability_file,
        idempotency_key,
        &revision,
        &reservation_id,
        &account,
        intent_id.as_deref(),
    )
}

#[allow(clippy::too_many_arguments)]
fn run_managed_account_handoff_cancel_with_identity(
    dir: &Path,
    state_dir: &Path,
    capability_file: &str,
    idempotency_key: &str,
    revision: &str,
    reservation_id: &str,
    account: &str,
    intent_id: Option<&str>,
) -> CmdOutput {
    let mut owned_args = vec![
        "--state-dir".to_string(),
        state_dir.to_str().expect("state dir").to_string(),
        "worker".to_string(),
        "account-handoff-cancel".to_string(),
        "assignment-matrix".to_string(),
        "--reservation-id".to_string(),
        reservation_id.to_string(),
        "--account".to_string(),
        account.to_string(),
    ];
    if let Some(intent_id) = intent_id {
        owned_args.push("--intent-id".to_string());
        owned_args.push(intent_id.to_string());
    }
    owned_args.extend([
        "--if-revision".to_string(),
        revision.to_string(),
        "--authorize-account-change".to_string(),
        "--idempotency-key".to_string(),
        idempotency_key.to_string(),
        "--format".to_string(),
        "json".to_string(),
    ]);
    let args = owned_args.iter().map(String::as_str).collect::<Vec<_>>();
    run_main_agent(
        dir,
        &args,
        &[
            ("AGENT_SESSION_CAPABILITY_FILE", capability_file),
            ("AGENT_SESSION_CODEX_ACCOUNT_BROKER", r#"["/bin/false"]"#),
        ],
    )
}

fn seed_session(state_dir: &Path, id: &str, incarnation: &str) {
    seed_session_at(
        state_dir,
        id,
        incarnation,
        Path::new("/fixture/repository"),
        None,
    );
}

fn seed_session_at(
    state_dir: &Path,
    id: &str,
    incarnation: &str,
    cwd: &Path,
    coordination_mode: Option<&str>,
) {
    let session_dir = state_dir.join("sessions").join(id);
    fs::create_dir_all(&session_dir).expect("session directory");
    fs::set_permissions(state_dir, fs::Permissions::from_mode(0o700)).expect("state mode");
    fs::set_permissions(
        state_dir.join("sessions"),
        fs::Permissions::from_mode(0o700),
    )
    .expect("sessions mode");
    fs::set_permissions(&session_dir, fs::Permissions::from_mode(0o700)).expect("session mode");
    let mut record = json!({
        "schema_version": "agent-session.session.v1",
        "id": id,
        "agent": "codex",
        "mode": "interactive",
        "title": "coordination fixture",
        "title_revision": 0,
        "cwd": cwd,
        "tmux_session": format!("hs-codex-{id}"),
        "prompt_file": null,
        "log_file": null,
        "created_at": "2030-01-01T00:00:00Z",
        "updated_at": "2030-01-01T00:00:00Z",
        "runtime": {
            "kind": "tmux",
            "tmux_session": format!("hs-codex-{id}"),
            "generation": 1,
            "started_at": "2030-01-01T00:00:00Z",
            "launch_id": incarnation
        }
    });
    if let Some(mode) = coordination_mode {
        record["coordination_mode"] = json!(mode);
    }
    let path = session_dir.join("session.json");
    fs::write(
        &path,
        serde_json::to_vec_pretty(&record).expect("session json"),
    )
    .expect("write session");
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).expect("record mode");
}

fn seed_activity_state(
    state_dir: &Path,
    id: &str,
    incarnation: &str,
    phase: &str,
    current_turn: serde_json::Value,
    last_turn: serde_json::Value,
) {
    let path = state_dir.join("sessions").join(id).join("activity.json");
    write_private_json(
        &path,
        &json!({
            "schema_version": "agent-session.activity.v1",
            "runtime_id": incarnation,
            "runtime_generation": 1,
            "state": {
                "schema_version": "agent-session.turn-state.v1",
                "phase": phase,
                "phase_changed_at": "2030-01-01T00:00:00Z",
                "revision": 1,
                "source": {
                    "kind": "runtime",
                    "provider": null,
                    "confidence": "authoritative"
                },
                "current_turn": current_turn,
                "last_turn": last_turn
            },
            "pending_attention": [],
            "seen_event_count": 0
        }),
    );
}

fn seed_live_runtime_identity(
    state_dir: &Path,
    id: &str,
    incarnation: &str,
    tmux_slot: u32,
) -> TestProcessGroup {
    let runtime = spawn_scoped_test_process_group().expect("live runtime identity");
    let runtime_pid = runtime.pid() as libc::pid_t;
    let runtime_identity = json!({
        "launch_id": incarnation,
        "session_id": format!("${tmux_slot}"),
        "pane_id": format!("%{tmux_slot}"),
        "pane_pid": runtime_pid,
        "process_group_id": runtime_pid,
        "process_session_id": runtime_pid
    });
    let session_path = state_dir.join("sessions").join(id).join("session.json");
    let mut session: serde_json::Value =
        serde_json::from_slice(&fs::read(&session_path).expect("session record"))
            .expect("session json");
    session["delete_tmux_identity"] = runtime_identity;
    write_private_json(&session_path, &session);
    runtime
}

fn init_checkout(path: &Path, remote: &str) {
    fs::create_dir_all(path).expect("checkout directory");
    let init = Command::new("git")
        .current_dir(path)
        .args(["init", "--quiet", "--initial-branch", "main"])
        .status()
        .expect("git init");
    assert!(init.success());
    let remote_add = Command::new("git")
        .current_dir(path)
        .args(["remote", "add", "origin", remote])
        .status()
        .expect("git remote add");
    assert!(remote_add.success());
}

fn git_stdout(path: &Path, args: &[&str]) -> Vec<u8> {
    let output = Command::new("git")
        .current_dir(path)
        .args(args)
        .output()
        .expect("run git fixture command");
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn digest(value: &str) -> String {
    Sha256::digest(value.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn seed_brokers(state_dir: &Path, sessions: &[(&str, &str, &str)]) {
    let sessions = sessions
        .iter()
        .map(|(id, incarnation, capability)| {
            (
                *id,
                *incarnation,
                *capability,
                Path::new("/fixture/repository"),
                None,
            )
        })
        .collect::<Vec<_>>();
    seed_brokers_at(state_dir, &sessions);
}

fn seed_brokers_at(state_dir: &Path, sessions: &[(&str, &str, &str, &Path, Option<&str>)]) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs();
    let mut brokers = serde_json::Map::new();
    for (id, incarnation, capability, cwd, coordination_mode) in sessions {
        seed_session_at(state_dir, id, incarnation, cwd, *coordination_mode);
        seed_activity_state(
            state_dir,
            id,
            incarnation,
            "starting",
            serde_json::Value::Null,
            serde_json::Value::Null,
        );
        let capability_dir = state_dir.join("sessions").join(id).join("coordination");
        fs::create_dir(&capability_dir).expect("capability directory");
        fs::set_permissions(&capability_dir, fs::Permissions::from_mode(0o700))
            .expect("capability dir mode");
        let capability_path = capability_dir.join(format!("capability-{}", digest(incarnation)));
        fs::write(&capability_path, capability).expect("capability");
        fs::set_permissions(&capability_path, fs::Permissions::from_mode(0o600))
            .expect("capability mode");
        let checkpoint_path = capability_dir.join(format!(
            "main-agent-checkpoint-{}.json",
            digest(incarnation)
        ));
        fs::write(&checkpoint_path, []).expect("checkpoint");
        fs::set_permissions(&checkpoint_path, fs::Permissions::from_mode(0o600))
            .expect("checkpoint mode");
        let heartbeat_path = capability_dir.join("heartbeat");
        fs::write(&heartbeat_path, format!("{incarnation}:{now}\n")).expect("heartbeat");
        fs::set_permissions(&heartbeat_path, fs::Permissions::from_mode(0o600))
            .expect("heartbeat mode");
        brokers.insert(
            (*id).to_string(),
            json!({
                "session_id": id,
                "incarnation": incarnation,
                "coordination_mode": coordination_mode.unwrap_or("advisory"),
                "capability_digest": digest(capability),
                "generation": 1,
                "state": "ready",
                "heartbeat_at": "2030-01-01T00:00:00Z",
                "heartbeat_epoch": now
            }),
        );
    }
    let coordination = state_dir.join("coordination");
    fs::create_dir(&coordination).expect("coordination root");
    fs::set_permissions(&coordination, fs::Permissions::from_mode(0o700))
        .expect("coordination mode");
    let registry = coordination.join("registry.json");
    fs::write(
        &registry,
        serde_json::to_vec_pretty(&json!({
            "schema_version": "agent-session.coordination-registry.v1",
            "fingerprint_epoch": 1,
            "fingerprint_key": "fixture-private-fingerprint-key-material-0000000001",
            "brokers": brokers,
            "claims": [],
            "operations": [],
            "messages": [],
            "receipts": {},
            "notifications": {}
        }))
        .expect("registry json"),
    )
    .expect("registry");
    fs::set_permissions(&registry, fs::Permissions::from_mode(0o600)).expect("registry mode");
}

fn grant_checkout_shell(state_dir: &Path, session_ids: &[&str]) {
    rewrite_registry(state_dir, |registry| {
        for claim in registry["claims"].as_array_mut().expect("claims") {
            if session_ids
                .iter()
                .any(|session_id| claim["session_id"] == *session_id)
                && claim["state"] == "active"
            {
                claim["checkout_shell_grant"] = json!(true);
            }
        }
    });
}

fn capability(state_dir: &Path, id: &str) -> String {
    let record: serde_json::Value = serde_json::from_slice(
        &fs::read(state_dir.join("sessions").join(id).join("session.json"))
            .expect("session record"),
    )
    .expect("session json");
    let incarnation = record["runtime"]["launch_id"]
        .as_str()
        .expect("session incarnation");
    state_dir
        .join("sessions")
        .join(id)
        .join(format!("coordination/capability-{}", digest(incarnation)))
        .to_string_lossy()
        .to_string()
}

fn candidate(path: &Path, prefix: &str, summary: &str) {
    fs::write(
        path,
        serde_json::to_vec_pretty(&json!({
            "schema_version": "agent-session.work-context-input.v1",
            "intent": "implementation",
            "tier": "L2",
            "repositories": ["example/repository"],
            "worktrees": [],
            "provider_refs": [],
            "plan_refs": [],
            "scopes": [{
                "kind": "path-prefix",
                "repository": "example/repository",
                "value": prefix.trim_end_matches('/')
            }],
            "summary": summary
        }))
        .expect("candidate json"),
    )
    .expect("candidate");
}

fn data(output: &CmdOutput) -> serde_json::Value {
    output.stdout_json()["data"].clone()
}

fn orchestration_request_digest(operation: &str, value: &serde_json::Value) -> String {
    let mut digest = Sha256::new();
    digest.update(operation.as_bytes());
    digest.update([0]);
    digest.update(serde_json::to_vec(value).expect("request json"));
    digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[derive(serde::Deserialize, serde::Serialize)]
struct AssignmentDigestFixture {
    schema_version: String,
    assignment_id: Option<String>,
    task_summary: String,
    task: serde_json::Value,
    launch: AssignmentLaunchDigestFixture,
    repository: Option<String>,
    worktree: Option<String>,
    base_ref: Option<String>,
    scopes: Vec<String>,
    durable_refs: Vec<String>,
}

#[derive(serde::Deserialize, serde::Serialize)]
struct AssignmentLaunchDigestFixture {
    agent: String,
    cwd: String,
    title: Option<String>,
    session_id: Option<String>,
    coordination_mode: String,
    agent_args: Vec<String>,
}

fn assignment_request_digest(value: &serde_json::Value) -> String {
    let input: AssignmentDigestFixture =
        serde_json::from_value(value.clone()).expect("assignment digest fixture");
    let mut digest = Sha256::new();
    digest.update(b"main-agent-worker-start");
    digest.update([0]);
    digest.update(serde_json::to_vec(&input).expect("assignment request json"));
    digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn write_private_json(path: &Path, value: &serde_json::Value) {
    fs::write(
        path,
        serde_json::to_vec_pretty(value).expect("private json"),
    )
    .expect("write private json");
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).expect("private json mode");
}

fn init_main_run(
    tmp: &Path,
    state_dir: &Path,
    checkout: &Path,
    session_id: &str,
    run_id: &str,
) -> String {
    let objective_path = tmp.join(format!("objective-{session_id}.json"));
    write_private_json(
        &objective_path,
        &json!({
            "schema_version": "main-agent.objective-packet.v1",
            "run_id": run_id,
            "tier": "L0",
            "objective_summary": "Exercise orchestration recovery",
            "objective": {},
            "done_criteria": ["Recovery converges"],
            "constraints": ["Do not duplicate lifecycle effects"],
            "durable_refs": [],
            "work_context": {
                "schema_version": "agent-session.work-context-input.v1",
                "intent": "implementation",
                "tier": "L0",
                "repositories": ["example/repository"],
                "worktrees": [],
                "provider_refs": [],
                "plan_refs": [],
                "scopes": [{
                    "kind": "path-prefix",
                    "repository": "example/repository",
                    "value": "crates/agent-session"
                }],
                "summary": "Exercise orchestration recovery"
            },
            "next_action": null
        }),
    );
    let capability_file = capability(state_dir, session_id);
    let initialized = run_main_agent(
        checkout,
        &[
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "init",
            "--packet-file",
            objective_path.to_str().expect("objective path"),
            "--if-absent",
            "--idempotency-key",
            &format!("init-{session_id}-0001"),
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_CAPABILITY_FILE", &capability_file)],
    );
    assert_eq!(initialized.code, 0, "stderr={}", initialized.stderr_text());
    capability_file
}

fn insert_orchestration_assignment(
    state_dir: &Path,
    assignment_id: &str,
    mut assignment: serde_json::Value,
    private_packet: &serde_json::Value,
) {
    let packet_bytes = serde_json::to_vec(private_packet).expect("assignment packet");
    let packet_digest = format!(
        "sha256:{}",
        Sha256::digest(&packet_bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    );
    let packets = state_dir.join("orchestration/packets");
    fs::create_dir_all(&packets).expect("packet directory");
    fs::set_permissions(&packets, fs::Permissions::from_mode(0o700)).expect("packet dir mode");
    let packet_path = packets.join(packet_digest.trim_start_matches("sha256:"));
    fs::write(&packet_path, packet_bytes).expect("assignment packet");
    fs::set_permissions(&packet_path, fs::Permissions::from_mode(0o600))
        .expect("assignment packet mode");
    assignment["private_packet_digest"] = json!(packet_digest);
    rewrite_orchestration_registry(state_dir, |registry| {
        registry["assignments"][assignment_id] = assignment;
    });
}

fn rewrite_orchestration_registry(state_dir: &Path, mutate: impl FnOnce(&mut serde_json::Value)) {
    let path = state_dir.join("orchestration/registry.json");
    let mut registry: serde_json::Value =
        serde_json::from_slice(&fs::read(&path).expect("orchestration registry"))
            .expect("orchestration registry json");
    mutate(&mut registry);
    write_private_json(&path, &registry);
}

fn seed_active_claim(state_dir: &Path, session_id: &str, incarnation: &str, claim_id: &str) {
    rewrite_registry(state_dir, |registry| {
        registry["claims"]
            .as_array_mut()
            .expect("claims")
            .push(json!({
                "schema_version": "agent-session.work-context.v1",
                "session_id": session_id,
                "session_incarnation": incarnation,
                "claim_id": claim_id,
                "revision": 1,
                "state": "active",
                "intent": "implementation",
                "tier": "L0",
                "repositories": [],
                "worktrees": [],
                "provider_refs": [],
                "plan_refs": [],
                "scopes": [],
                "summary": "Main Agent lifecycle fixture",
                "updated_at": "2030-01-01T00:00:00Z",
                "expires_at": "9999-12-31T23:59:59Z",
                "expires_at_epoch": i64::MAX
            }));
    });
}

fn seed_operation(
    state_dir: &Path,
    session_id: &str,
    incarnation: &str,
    lease_id: &str,
    state: &str,
) {
    rewrite_registry(state_dir, |registry| {
        registry["operations"]
            .as_array_mut()
            .expect("operations")
            .push(json!({
                "schema_version": "agent-session.operation-lease.v1",
                "lease_id": lease_id,
                "session_id": session_id,
                "session_incarnation": incarnation,
                "claim_id": format!("{lease_id}-claim"),
                "claim_revision": 1,
                "operation": "test mutation",
                "targets": [],
                "provider_targets": [],
                "state": state,
                "revision": 1,
                "started_at": "2030-01-01T00:00:00Z",
                "expires_at": "9999-12-31T23:59:59Z",
                "expires_at_epoch": i64::MAX,
                "terminal_at_epoch": null,
                "execution_token_digest": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "activity_revision": 1,
                "activity_identity_digest": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                "runtime_identity_digest": "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
                "descendant": null,
                "reconcile_observed_at_epoch": null,
                "outcome": null
            }));
    });
}

fn rewrite_registry(state_dir: &Path, mutate: impl FnOnce(&mut serde_json::Value)) {
    let path = state_dir.join("coordination/registry.json");
    let mut registry: serde_json::Value =
        serde_json::from_slice(&fs::read(&path).expect("registry")).expect("registry json");
    mutate(&mut registry);
    fs::write(
        &path,
        serde_json::to_vec_pretty(&registry).expect("registry json"),
    )
    .expect("write registry");
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).expect("registry mode");
}

fn orchestration_registry(state_dir: &Path) -> serde_json::Value {
    serde_json::from_slice(
        &fs::read(state_dir.join("orchestration/registry.json")).expect("orchestration registry"),
    )
    .expect("orchestration registry json")
}

fn load_coordination_registry(state_dir: &Path) -> serde_json::Value {
    serde_json::from_slice(
        &fs::read(state_dir.join("coordination/registry.json")).expect("coordination registry"),
    )
    .expect("coordination registry json")
}

fn coordination_authority_snapshot(
    registry: &serde_json::Value,
    session_id: &str,
) -> serde_json::Value {
    json!({
        "broker": registry["brokers"][session_id],
        "claims": registry["claims"]
            .as_array()
            .expect("claims")
            .iter()
            .filter(|claim| claim["session_id"] == session_id)
            .cloned()
            .collect::<Vec<_>>()
    })
}

fn assert_frozen_base_v3_registry_compatible(registry: &serde_json::Value) {
    assert_eq!(
        registry["schema_version"], "agent-session.orchestration-registry.v3",
        "B2 must keep the released v3 registry envelope"
    );
    let released_assignment_fields = [
        "schema_version",
        "assignment_id",
        "run_id",
        "revision",
        "state",
        "task_summary",
        "private_packet_digest",
        "primary_manager",
        "worker",
        "previous_worker",
        "collaborators",
        "borrowed_by",
        "repository",
        "worktree",
        "base_ref",
        "scopes",
        "durable_refs",
        "depends_on",
        "checkpoint",
        "result_summary",
        "blocker_summary",
        "submit_recovery",
        "worker_quarantine",
        "account_handoff",
        "created_at",
        "updated_at",
    ];
    for assignment in registry["assignments"]
        .as_object()
        .expect("assignments")
        .values()
    {
        for field in assignment.as_object().expect("assignment").keys() {
            assert!(
                released_assignment_fields.contains(&field.as_str()),
                "released v3 assignment reader rejects unknown field {field}"
            );
        }
        if !assignment["worker_quarantine"].is_null() {
            let recovery = &assignment["submit_recovery"];
            assert_eq!(
                recovery["state"], "reconciled",
                "released v3 requires every assignment quarantine to be backed by reconciled submit recovery"
            );
            assert_eq!(
                recovery["session_incarnation"],
                assignment["worker_quarantine"]["worker"]["session_incarnation"],
                "released v3 binds quarantine to the reconciled worker incarnation"
            );
        }
    }
}

#[test]
fn coordination_help_exposes_closed_work_context_and_mailbox_command_families() {
    let tmp = tempfile::TempDir::new().expect("tempdir");

    let work_context = run(tmp.path(), &["work-context", "--help"]);
    assert_eq!(
        work_context.code,
        0,
        "stderr={}",
        work_context.stderr_text()
    );
    let work_context_help = work_context.stdout_text();
    for command in [
        "status",
        "set",
        "clear",
        "advise",
        "acknowledge",
        "claim",
        "show",
        "check",
        "renew",
        "release",
        "admit",
        "complete",
        "reconcile",
    ] {
        assert!(
            work_context_help.contains(command),
            "missing work-context command {command}: {work_context_help}"
        );
    }

    let start = run(tmp.path(), &["start", "--help"]);
    assert_eq!(start.code, 0, "stderr={}", start.stderr_text());
    assert!(start.stdout_text().contains("--coordination-mode"));
    assert!(start.stdout_text().contains("advisory"));
    assert!(start.stdout_text().contains("enforce"));
    assert!(start.stdout_text().contains("off"));

    let broker = run(tmp.path(), &["broker", "--help"]);
    assert_eq!(broker.code, 0, "stderr={}", broker.stderr_text());
    let broker_help = broker.stdout_text();
    for command in ["status", "adopt", "reconcile"] {
        assert!(
            broker_help.contains(command),
            "missing broker command {command}: {broker_help}"
        );
    }

    let message = run(tmp.path(), &["message", "--help"]);
    assert_eq!(message.code, 0, "stderr={}", message.stderr_text());
    let message_help = message.stdout_text();
    for command in ["send", "inbox", "show", "ack", "reply", "wait"] {
        assert!(
            message_help.contains(command),
            "missing message command {command}: {message_help}"
        );
    }

    let send = run(tmp.path(), &["message", "send", "--help"]);
    assert_eq!(send.code, 0, "stderr={}", send.stderr_text());
    assert!(send.stdout_text().contains("eventual fixed notification"));
    assert!(
        send.stdout_text()
            .contains("defaults to AGENT_SESSION_CAPABILITY_FILE")
    );

    let inbox = run(tmp.path(), &["message", "inbox", "--help"]);
    assert_eq!(inbox.code, 0, "stderr={}", inbox.stderr_text());
    assert!(
        inbox
            .stdout_text()
            .contains("defaults to AGENT_SESSION_CAPABILITY_FILE")
    );

    let reply = run(tmp.path(), &["message", "reply", "--help"]);
    assert_eq!(reply.code, 0, "stderr={}", reply.stderr_text());
    assert!(reply.stdout_text().contains("eventual fixed notification"));
}

#[test]
fn main_agent_help_documents_safe_lifecycle_revision_fences_and_retry_keys() {
    let tmp = tempfile::tempdir().expect("tempdir");

    let root = run_main_agent(tmp.path(), &["--help"], &[]);
    assert_eq!(root.code, 0, "stderr={}", root.stderr_text());
    let root_help = root.stdout_text();
    for lifecycle_step in [
        "SAFE LIFECYCLE",
        "init -> rehydrate/status -> worker start --await-ready -> worker bootstrap",
        "worker supervise -> accept -> retire -> close",
        "MACRO-FIRST RECOVERY",
        "never resend a prompt or inject an unbounded/manual Enter",
    ] {
        assert!(
            root_help.contains(lifecycle_step),
            "main-agent root help omitted {lifecycle_step:?}: {root_help}"
        );
    }

    for (args, expected_guidance) in [
        (
            &["init", "--help"][..],
            &[
                "absence fence",
                "same idempotency key",
                "same logical request",
            ][..],
        ),
        (
            &["checkpoint", "--help"][..],
            &["expected current revision", "same idempotency key"][..],
        ),
        (
            &["self", "recover", "--help"][..],
            &["exact main agent controller", "same idempotency key"][..],
        ),
        (
            &["worker", "start", "--help"][..],
            // --if-run-revision is now optional (T2 decouple), so the help reads
            // "optional expected run revision" rather than "current".
            &["expected run revision", "same idempotency key"][..],
        ),
        (
            &["worker", "request-changes", "--help"][..],
            &[
                "return a submitted assignment",
                "bounded durable reason",
                "expected current assignment revision",
                "same idempotency key",
            ][..],
        ),
        (
            &["worker", "accept", "--help"][..],
            &[
                "expected current assignment revision",
                "same idempotency key",
            ][..],
        ),
        (
            &["worker", "reassign", "--help"][..],
            &[
                "distinct replacement assignment",
                "clean worktree",
                "expected current assignment revision",
                "same idempotency key",
            ][..],
        ),
        (
            &["worker", "submit-recovery", "--help"][..],
            &[
                "single guarded enter",
                "expected current assignment revision",
                "1-30s",
            ][..],
        ),
        (
            &["worker", "stop-runtime", "--help"][..],
            &[
                "exact live runtime",
                "durably exhausted",
                "preserving the assignment, session, and worktree",
                "exact worker incarnation",
                "expected current assignment revision",
                "same idempotency key",
            ][..],
        ),
        (
            &["worker", "guidance-reconcile", "--help"][..],
            &[
                "unread guidance",
                "expected current assignment revision",
                "same idempotency key",
            ][..],
        ),
        (
            &["worker", "guidance-quarantine", "--help"][..],
            &[
                "unretained stale worker incarnations",
                "expected current assignment revision",
                "same idempotency key",
            ][..],
        ),
        (
            &["worker", "account-handoff", "--help"][..],
            &[
                "explicitly authorized account handoff",
                "allowlisted account nickname",
                "expected current assignment revision",
                "same idempotency key",
            ][..],
        ),
        (
            &["worker", "account-handoff-cancel", "--help"][..],
            &[
                "failed, superseded, or queued",
                "--reservation-id",
                "--account",
                "--intent-id",
                "expected current assignment revision",
                "authorize-account-change",
                "same idempotency key",
            ][..],
        ),
    ] {
        let output = run_main_agent(tmp.path(), args, &[]);
        assert_eq!(
            output.code,
            0,
            "args={args:?} stderr={}",
            output.stderr_text()
        );
        let help = output.stdout_text().to_ascii_lowercase();
        for guidance in expected_guidance {
            assert!(
                help.contains(guidance),
                "main-agent {args:?} help omitted {guidance:?}: {help}"
            );
        }
    }
}

#[test]
fn main_agent_account_handoff_cancel_docs_publish_every_required_identity_selector() {
    let crate_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let spec = fs::read_to_string(crate_root.join("docs/specs/main-agent-orchestration-v1.md"))
        .expect("read orchestration spec");
    let runbook = fs::read_to_string(crate_root.join("docs/runbooks/main-agent-orchestration.md"))
        .expect("read orchestration runbook");
    for (name, document) in [("spec", spec), ("runbook", runbook)] {
        for selector in ["--reservation-id", "--account", "--intent-id"] {
            assert!(
                document.contains(selector),
                "{name} omits required cancellation selector {selector}"
            );
        }
        assert!(
            document.contains("released-v1"),
            "{name} must retain the frozen released-v1 intent-id exception"
        );
    }
}

#[cfg(target_os = "linux")]
#[test]
fn main_agent_self_recover_is_exact_controller_macro_first_and_claim_preserving() {
    use std::os::unix::process::CommandExt;

    #[derive(serde::Serialize)]
    struct RuntimeIdentityFixture<'a> {
        launch_id: &'a str,
        session_id: &'a str,
        pane_id: &'a str,
        pane_pid: libc::pid_t,
        process_group_id: libc::pid_t,
        process_session_id: libc::pid_t,
    }

    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let checkout = tmp.path().join("checkout");
    fs::create_dir(&state_dir).expect("state");
    init_checkout(&checkout, "https://example.invalid/example/repository.git");
    seed_brokers_at(
        &state_dir,
        &[(
            "main-one",
            "main-incarnation-one",
            "main-private-capability-material-0000000001",
            checkout.as_path(),
            Some("enforce"),
        )],
    );
    let main_capability = init_main_run(tmp.path(), &state_dir, &checkout, "main-one", "run-one");
    let mut runtime_process = Command::new("sleep");
    runtime_process.arg("30").stdin(Stdio::null());
    // SAFETY: the child performs only the async-signal-safe `setsid` before exec.
    unsafe {
        runtime_process.pre_exec(|| {
            if libc::setsid() < 0 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
    let mut runtime_process = runtime_process.spawn().expect("live runtime fixture");
    let runtime_pid = runtime_process.id() as libc::pid_t;
    let identity_fixture = RuntimeIdentityFixture {
        launch_id: "main-incarnation-one",
        session_id: "$77",
        pane_id: "%77",
        pane_pid: runtime_pid,
        process_group_id: runtime_pid,
        process_session_id: runtime_pid,
    };
    let identity_bytes = serde_json::to_vec(&identity_fixture).expect("runtime identity bytes");
    let identity: serde_json::Value =
        serde_json::from_slice(&identity_bytes).expect("runtime identity");
    let identity_digest = Sha256::digest(&identity_bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let session_path = state_dir.join("sessions/main-one/session.json");
    let mut session: serde_json::Value =
        serde_json::from_slice(&fs::read(&session_path).expect("main session"))
            .expect("main session json");
    session["delete_tmux_identity"] = identity.clone();
    write_private_json(&session_path, &session);
    rewrite_registry(&state_dir, |registry| {
        registry["brokers"]["main-one"]["runtime_identity"] = identity;
        registry["brokers"]["main-one"]["runtime_identity_digest"] = json!(identity_digest);
    });

    let recover = |key: &str| {
        run_main_agent(
            &checkout,
            &[
                "--state-dir",
                state_dir.to_str().expect("state dir"),
                "self",
                "recover",
                "--idempotency-key",
                key,
                "--format",
                "json",
            ],
            &[
                ("AGENT_SESSION_ID", "main-one"),
                ("AGENT_SESSION_CAPABILITY_FILE", &main_capability),
            ],
        )
    };
    let healthy = recover("controller-recover-healthy-0001");
    assert_eq!(
        healthy.code,
        0,
        "stdout={} stderr={}",
        healthy.stdout_text(),
        healthy.stderr_text()
    );
    assert_eq!(data(&healthy)["recovery"], "healthy_noop");
    assert_eq!(data(&healthy)["claim"]["active"], true);
    assert_eq!(data(&healthy)["claim"]["retained"], true);
    assert_eq!(data(&healthy)["run"]["rebind_required"], false);
    assert!(
        data(&healthy)["forbidden_side_effects"]
            .as_object()
            .expect("side-effect proof")
            .values()
            .all(|value| value == false),
        "controller recovery must not impersonate the provider or clear a mutation fence"
    );

    let baseline_coordination = load_coordination_registry(&state_dir);
    let baseline_orchestration = orchestration_registry(&state_dir);
    let heartbeat_path = state_dir.join("sessions/main-one/coordination/heartbeat");
    let heartbeat_bytes = fs::read(&heartbeat_path).expect("healthy heartbeat");
    let main_claim_id = baseline_coordination["claims"]
        .as_array()
        .expect("claims")
        .iter()
        .find(|claim| claim["session_id"] == "main-one" && claim["state"] == "active")
        .and_then(|claim| claim["claim_id"].as_str())
        .expect("main claim")
        .to_string();

    fs::remove_file(&heartbeat_path).expect("remove heartbeat before replay");
    rewrite_registry(&state_dir, |registry| {
        registry["brokers"]["main-one"]["heartbeat_epoch"] = json!(0);
        registry["brokers"]["main-one"]["lost_since_epoch"] = json!(1);
    });
    let stale_broker_before_replay =
        load_coordination_registry(&state_dir)["brokers"]["main-one"].clone();
    let healthy_replay = recover("controller-recover-healthy-0001");
    assert_eq!(
        healthy_replay.code,
        0,
        "stdout={} stderr={}",
        healthy_replay.stdout_text(),
        healthy_replay.stderr_text()
    );
    assert_eq!(
        data(&healthy_replay),
        data(&healthy),
        "an accepted healthy-noop recovery key must replay its original result even after the broker becomes adoptable"
    );
    assert_eq!(
        load_coordination_registry(&state_dir)["brokers"]["main-one"],
        stale_broker_before_replay,
        "replaying the healthy result must not adopt or mutate the broker"
    );
    fs::write(&heartbeat_path, heartbeat_bytes).expect("restore healthy heartbeat");
    rewrite_registry(&state_dir, |registry| {
        *registry = baseline_coordination.clone();
    });
    rewrite_orchestration_registry(&state_dir, |registry| {
        *registry = baseline_orchestration.clone();
    });

    for (case, expected_code) in [
        ("active-operation", "controller-recovery-operation-fenced"),
        (
            "reconcile-pending-operation",
            "controller-recovery-operation-fenced",
        ),
        ("missing-claim", "controller-recovery-claim-unavailable"),
        ("released-claim", "controller-recovery-claim-unavailable"),
        ("worker-principal", "controller-recovery-role"),
        (
            "runtime-identity-mismatch",
            "controller-recovery-runtime-uncertain",
        ),
        ("runtime-stopped", "controller-recovery-runtime-uncertain"),
        ("runtime-unknown", "controller-recovery-runtime-uncertain"),
    ] {
        rewrite_registry(&state_dir, |registry| {
            *registry = baseline_coordination.clone();
            match case {
                "active-operation" | "reconcile-pending-operation" => {
                    registry["operations"]
                        .as_array_mut()
                        .expect("operations")
                        .push(json!({
                            "schema_version": "agent-session.operation-lease.v1",
                            "lease_id": format!("controller-recovery-{case}"),
                            "session_id": "main-one",
                            "session_incarnation": "main-incarnation-one",
                            "claim_id": main_claim_id.clone(),
                            "claim_revision": 1,
                            "operation": "test mutation",
                            "targets": [],
                            "provider_targets": [],
                            "state": if case == "active-operation" {
                                "active"
                            } else {
                                "reconcile_pending"
                            },
                            "revision": 1,
                            "started_at": "2030-01-01T00:00:00Z",
                            "expires_at": "9999-12-31T23:59:59Z",
                            "expires_at_epoch": i64::MAX,
                            "terminal_at_epoch": null,
                            "execution_token_digest": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                            "activity_revision": 1,
                            "activity_identity_digest": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                            "runtime_identity_digest": "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
                            "descendant": null,
                            "reconcile_observed_at_epoch": null,
                            "outcome": null
                        }));
                }
                "missing-claim" => registry["claims"]
                    .as_array_mut()
                    .expect("claims")
                    .retain(|claim| claim["session_id"] != "main-one"),
                "released-claim" => {
                    let claim = registry["claims"]
                        .as_array_mut()
                        .expect("claims")
                        .iter_mut()
                        .find(|claim| claim["session_id"] == "main-one")
                        .expect("main claim");
                    claim["state"] = json!("released");
                }
                "runtime-identity-mismatch" => {
                    registry["brokers"]["main-one"]["runtime_identity_digest"] =
                        json!("ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff");
                }
                "worker-principal" | "runtime-stopped" | "runtime-unknown" => {}
                _ => unreachable!(),
            }
        });
        rewrite_orchestration_registry(&state_dir, |registry| {
            *registry = baseline_orchestration.clone();
            if case == "worker-principal" {
                registry["runs"]["run-one"]["controller"] = json!({
                    "session_id": "other-main",
                    "session_incarnation": "other-main-incarnation",
                    "session_created_at": "2030-01-01T00:00:00Z"
                });
                registry["assignments"]["controller-as-worker"] = json!({
                    "schema_version": "agent-session.orchestration-assignment.v1",
                    "assignment_id": "controller-as-worker",
                    "run_id": "run-one",
                    "revision": 1,
                    "state": "working",
                    "task_summary": "Controller recovery role fence",
                    "private_packet_digest": format!("sha256:{}", "d".repeat(64)),
                    "primary_manager": {
                        "session_id": "other-main",
                        "session_incarnation": "other-main-incarnation",
                        "session_created_at": "2030-01-01T00:00:00Z"
                    },
                    "worker": {
                        "session_id": "main-one",
                        "session_incarnation": "main-incarnation-one",
                        "session_created_at": "2030-01-01T00:00:00Z"
                    },
                    "collaborators": [],
                    "borrowed_by": [],
                    "scopes": [],
                    "durable_refs": [],
                    "depends_on": [],
                    "created_at": "2030-01-01T00:00:00Z",
                    "updated_at": "2030-01-01T00:00:00Z"
                });
            }
        });
        let before_coordination = load_coordination_registry(&state_dir);
        let before_run = orchestration_registry(&state_dir)["runs"]["run-one"].clone();
        let coordination_bytes =
            fs::read(state_dir.join("coordination/registry.json")).expect("coordination bytes");
        let orchestration_bytes =
            fs::read(state_dir.join("orchestration/registry.json")).expect("orchestration bytes");
        let key = format!("controller-recover-{case}-0001");
        let refused = if let Some(runtime_status) = case.strip_prefix("runtime-") {
            run_main_agent(
                &checkout,
                &[
                    "--state-dir",
                    state_dir.to_str().expect("state dir"),
                    "self",
                    "recover",
                    "--idempotency-key",
                    &key,
                    "--format",
                    "json",
                ],
                &[
                    ("AGENT_SESSION_ID", "main-one"),
                    ("AGENT_SESSION_CAPABILITY_FILE", &main_capability),
                    (
                        "NILS_AGENT_SESSION_TEST_CONTROLLER_RUNTIME_STATUS",
                        runtime_status,
                    ),
                ],
            )
        } else {
            recover(&key)
        };
        assert_ne!(refused.code, 0, "case={case}");
        assert_eq!(
            refused.stdout_json()["error"]["code"],
            expected_code,
            "case={case}"
        );
        let after_coordination = load_coordination_registry(&state_dir);
        assert_eq!(
            after_coordination["brokers"]["main-one"], before_coordination["brokers"]["main-one"],
            "broker generation/authority changed for {case}"
        );
        assert_eq!(
            after_coordination["claims"], before_coordination["claims"],
            "claim revision/state changed for {case}"
        );
        assert_eq!(
            orchestration_registry(&state_dir)["runs"]["run-one"],
            before_run,
            "orchestration run changed for {case}"
        );
        assert_eq!(
            fs::read(state_dir.join("coordination/registry.json")).unwrap(),
            coordination_bytes,
            "coordination registry bytes changed for {case}"
        );
        assert_eq!(
            fs::read(state_dir.join("orchestration/registry.json")).unwrap(),
            orchestration_bytes,
            "orchestration registry bytes changed for {case}"
        );
    }
    rewrite_registry(&state_dir, |registry| {
        *registry = baseline_coordination.clone();
    });
    rewrite_orchestration_registry(&state_dir, |registry| {
        *registry = baseline_orchestration.clone();
    });

    fs::remove_file(state_dir.join("sessions/main-one/coordination/heartbeat"))
        .expect("remove stale heartbeat");
    rewrite_registry(&state_dir, |registry| {
        registry["brokers"]["main-one"]["heartbeat_epoch"] = json!(0);
        registry["brokers"]["main-one"]["lost_since_epoch"] = json!(1);
    });
    let adopted = recover("controller-recover-adopt-0001");
    assert_eq!(
        adopted.code,
        0,
        "stdout={} stderr={}",
        adopted.stdout_text(),
        adopted.stderr_text()
    );
    assert_eq!(data(&adopted)["recovery"], "adopted");
    assert_eq!(data(&adopted)["broker"]["authoritative"], true);
    assert_eq!(data(&adopted)["claim"]["active"], true);
    assert_eq!(
        data(&adopted)["claim"]["revision"],
        data(&healthy)["claim"]["revision"],
        "macro recovery must retain the exact active controller claim"
    );
    assert_eq!(data(&adopted)["run"]["run_id"], "run-one");
    assert_eq!(
        data(&adopted)["run"]["revision"],
        data(&healthy)["run"]["revision"],
        "broker recovery must not mutate the durable objective"
    );

    let _ = runtime_process.kill();
    let _ = runtime_process.wait();
}

#[test]
fn advisory_presence_defaults_for_unclaimed_sessions_and_classifies_overlap() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    fs::create_dir(&state_dir).expect("state");
    let checkout = tmp.path().join("checkout");
    init_checkout(&checkout, "https://github.com/example/repository.git");
    seed_brokers_at(
        &state_dir,
        &[
            (
                "alpha",
                "incarnation-alpha",
                "alpha-private-capability-material",
                checkout.as_path(),
                None,
            ),
            (
                "beta",
                "incarnation-beta",
                "beta-private-capability-material",
                checkout.as_path(),
                Some("advisory"),
            ),
        ],
    );
    let state = state_dir.to_string_lossy();
    let alpha_cap = capability(&state_dir, "alpha");
    let managed_env = [
        ("AGENT_SESSION_ID", "alpha"),
        ("AGENT_SESSION_CAPABILITY_FILE", alpha_cap.as_str()),
        ("AGENT_SESSION_STATE_DIR", state.as_ref()),
    ];

    let status = run_with_env(
        tmp.path(),
        &["work-context", "status", "--format", "json"],
        &managed_env,
    );
    assert_eq!(status.code, 0, "stderr={}", status.stderr_text());
    assert_eq!(data(&status)["managed"], true);
    assert_eq!(data(&status)["mode"], "advisory");
    assert_eq!(data(&status)["presence"]["state"], "active");
    assert!(data(&status)["context"].is_null());

    let advised = run_with_env(
        tmp.path(),
        &["work-context", "advise", "--format", "json"],
        &managed_env,
    );
    assert_eq!(advised.code, 0, "stderr={}", advised.stderr_text());
    assert_eq!(data(&advised)["mode"], "advisory");
    assert_eq!(data(&advised)["severity"], "warning");
    assert_eq!(data(&advised)["suppressed"], false);
    assert_eq!(data(&advised)["reasons"][0]["code"], "same-worktree");
    assert_eq!(data(&advised)["peers"][0]["session_id"], "beta");
    assert!(
        !advised
            .stdout_text()
            .contains(checkout.to_string_lossy().as_ref())
    );
    assert!(
        !advised
            .stdout_text()
            .contains("beta-private-capability-material")
    );
}

#[test]
fn advisory_presence_distinguishes_same_repository_from_same_worktree() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    fs::create_dir(&state_dir).expect("state");
    let alpha_checkout = tmp.path().join("alpha-checkout");
    let beta_checkout = tmp.path().join("beta-checkout");
    init_checkout(&alpha_checkout, "git@github.com:example/repository.git");
    init_checkout(&beta_checkout, "https://github.com/example/repository.git");
    seed_brokers_at(
        &state_dir,
        &[
            (
                "alpha",
                "incarnation-alpha",
                "alpha-private-capability-material",
                alpha_checkout.as_path(),
                Some("advisory"),
            ),
            (
                "beta",
                "incarnation-beta",
                "beta-private-capability-material",
                beta_checkout.as_path(),
                Some("advisory"),
            ),
        ],
    );
    let state = state_dir.to_string_lossy();
    let alpha_cap = capability(&state_dir, "alpha");
    let managed_env = [
        ("AGENT_SESSION_ID", "alpha"),
        ("AGENT_SESSION_CAPABILITY_FILE", alpha_cap.as_str()),
        ("AGENT_SESSION_STATE_DIR", state.as_ref()),
    ];
    let advised = run_with_env(
        tmp.path(),
        &["work-context", "advise", "--format", "json"],
        &managed_env,
    );
    assert_eq!(advised.code, 0, "stderr={}", advised.stderr_text());
    assert_eq!(data(&advised)["severity"], "info");
    assert_eq!(data(&advised)["reasons"][0]["code"], "same-repository");
}

#[test]
fn unmanaged_and_off_sessions_are_explicit_nonparticipants() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let unmanaged = run_with_env(
        tmp.path(),
        &["work-context", "advise", "--format", "json"],
        &[
            ("AGENT_SESSION_ID", ""),
            ("AGENT_SESSION_CAPABILITY_FILE", ""),
            ("AGENT_SESSION_STATE_DIR", ""),
        ],
    );
    assert_eq!(unmanaged.code, 0, "stderr={}", unmanaged.stderr_text());
    assert_eq!(data(&unmanaged)["managed"], false);
    assert_eq!(data(&unmanaged)["mode"], "off");
    assert_eq!(data(&unmanaged)["severity"], "none");

    let state_dir = tmp.path().join("state");
    fs::create_dir(&state_dir).expect("state");
    let checkout = tmp.path().join("checkout");
    init_checkout(&checkout, "https://github.com/example/repository.git");
    seed_brokers_at(
        &state_dir,
        &[(
            "alpha",
            "incarnation-alpha",
            "alpha-private-capability-material",
            checkout.as_path(),
            Some("off"),
        )],
    );
    let state = state_dir.to_string_lossy();
    let alpha_cap = capability(&state_dir, "alpha");
    let off = run_with_env(
        &checkout,
        &["work-context", "advise", "--format", "json"],
        &[
            ("AGENT_SESSION_ID", "alpha"),
            ("AGENT_SESSION_CAPABILITY_FILE", alpha_cap.as_str()),
            ("AGENT_SESSION_STATE_DIR", state.as_ref()),
        ],
    );
    assert_eq!(off.code, 0, "stderr={}", off.stderr_text());
    assert_eq!(data(&off)["managed"], true);
    assert_eq!(data(&off)["mode"], "off");
    assert_eq!(data(&off)["severity"], "none");
}

#[test]
fn advisory_targets_require_the_exact_v1_schema() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    fs::create_dir(&state_dir).expect("state");
    let checkout = tmp.path().join("checkout");
    init_checkout(&checkout, "https://github.com/example/repository.git");
    seed_brokers_at(
        &state_dir,
        &[(
            "alpha",
            "incarnation-alpha",
            "alpha-private-capability-material",
            checkout.as_path(),
            Some("advisory"),
        )],
    );
    let state = state_dir.to_string_lossy();
    let alpha_cap = capability(&state_dir, "alpha");
    let env = [
        ("AGENT_SESSION_ID", "alpha"),
        ("AGENT_SESSION_CAPABILITY_FILE", alpha_cap.as_str()),
        ("AGENT_SESSION_STATE_DIR", state.as_ref()),
    ];
    let cases = [
        (
            "valid",
            json!({
                "schema_version": "agent-session.operation-targets.v1",
                "targets": [],
                "provider_refs": [],
                "checkouts": [],
                "descendant": null
            }),
            true,
        ),
        (
            "missing",
            json!({ "targets": [], "provider_refs": [] }),
            false,
        ),
        (
            "future",
            json!({
                "schema_version": "agent-session.operation-targets.v2",
                "targets": [],
                "provider_refs": []
            }),
            false,
        ),
        (
            "misspelled",
            json!({
                "schema_version": "agent-session.operation-targets.v1",
                "tragets": [],
                "provider_refs": []
            }),
            false,
        ),
    ];
    for (name, body, succeeds) in cases {
        let path = tmp.path().join(format!("{name}.json"));
        fs::write(
            &path,
            serde_json::to_vec_pretty(&body).expect("targets json"),
        )
        .expect("write targets");
        let output = run_with_env(
            &checkout,
            &[
                "work-context",
                "advise",
                "--targets-file",
                path.to_str().expect("target path"),
                "--format",
                "json",
            ],
            &env,
        );
        assert_eq!(
            output.code == 0,
            succeeds,
            "case={name} stdout={} stderr={}",
            output.stdout_text(),
            output.stderr_text()
        );
        if !succeeds {
            assert_eq!(
                output.stdout_json()["schema_version"],
                "cli.agent-session.work-context-advise.v1"
            );
            assert_eq!(
                output.stdout_json()["error"]["code"],
                "invalid-operation-targets"
            );
        }
    }
}

#[test]
fn clear_advisories_do_not_rewrite_the_registry_for_target_churn() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    fs::create_dir(&state_dir).expect("state");
    let checkout = tmp.path().join("checkout");
    init_checkout(&checkout, "https://github.com/example/repository.git");
    seed_brokers_at(
        &state_dir,
        &[(
            "alpha",
            "incarnation-alpha",
            "alpha-private-capability-material",
            checkout.as_path(),
            Some("advisory"),
        )],
    );
    let state = state_dir.to_string_lossy();
    let alpha_cap = capability(&state_dir, "alpha");
    let env = [
        ("AGENT_SESSION_ID", "alpha"),
        ("AGENT_SESSION_CAPABILITY_FILE", alpha_cap.as_str()),
        ("AGENT_SESSION_STATE_DIR", state.as_ref()),
    ];
    let registry = state_dir.join("coordination/registry.json");
    let before = fs::read(&registry).expect("registry before");
    for target in ["src/one.rs", "src/two.rs"] {
        let path = tmp.path().join(target.replace('/', "-"));
        fs::write(
            &path,
            serde_json::to_vec_pretty(&json!({
                "schema_version": "agent-session.operation-targets.v1",
                "targets": [{
                    "kind": "path-exact",
                    "repository": "example/repository",
                    "value": target
                }],
                "provider_refs": [],
                "checkouts": []
            }))
            .expect("targets json"),
        )
        .expect("write targets");
        let advised = run_with_env(
            &checkout,
            &[
                "work-context",
                "advise",
                "--targets-file",
                path.to_str().expect("target path"),
                "--format",
                "json",
            ],
            &env,
        );
        assert_eq!(advised.code, 0, "stderr={}", advised.stderr_text());
        assert_eq!(data(&advised)["severity"], "none");
    }
    assert_eq!(fs::read(&registry).expect("registry after"), before);
}

#[test]
fn self_targeting_context_set_clear_and_acknowledge_hide_mechanical_inputs() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    fs::create_dir(&state_dir).expect("state");
    let checkout = tmp.path().join("checkout");
    init_checkout(&checkout, "https://github.com/example/repository.git");
    seed_brokers_at(
        &state_dir,
        &[
            (
                "alpha",
                "incarnation-alpha",
                "alpha-private-capability-material",
                checkout.as_path(),
                Some("advisory"),
            ),
            (
                "beta",
                "incarnation-beta",
                "beta-private-capability-material",
                checkout.as_path(),
                Some("advisory"),
            ),
        ],
    );
    let state = state_dir.to_string_lossy();
    let alpha_cap = capability(&state_dir, "alpha");
    let beta_cap = capability(&state_dir, "beta");
    let alpha_env = [
        ("AGENT_SESSION_ID", "alpha"),
        ("AGENT_SESSION_CAPABILITY_FILE", alpha_cap.as_str()),
        ("AGENT_SESSION_STATE_DIR", state.as_ref()),
    ];
    let beta_env = [
        ("AGENT_SESSION_ID", "beta"),
        ("AGENT_SESSION_CAPABILITY_FILE", beta_cap.as_str()),
        ("AGENT_SESSION_STATE_DIR", state.as_ref()),
    ];

    for (envs, summary) in [(&alpha_env, "alpha task"), (&beta_env, "beta task")] {
        let set = run_with_env(
            &checkout,
            &[
                "work-context",
                "set",
                "--tier",
                "L2",
                "--summary",
                summary,
                "--issue",
                "1318",
                "--pr",
                "42",
                "--plan-ref",
                "issue:1318",
                "--path",
                "src/",
                "--format",
                "json",
            ],
            envs,
        );
        assert_eq!(
            set.code,
            0,
            "stdout={} stderr={}",
            set.stdout_text(),
            set.stderr_text()
        );
        assert_eq!(data(&set)["mode"], "advisory");
        assert_eq!(
            data(&set)["context"]["repositories"][0],
            "example/repository"
        );
        assert_eq!(data(&set)["context"]["provider_refs"][0]["kind"], "issue");
        assert_eq!(data(&set)["context"]["provider_refs"][1]["kind"], "pr");
        assert_eq!(data(&set)["context"]["plan_refs"][0], "issue:1318");
        assert_eq!(data(&set)["context"]["scopes"][0]["kind"], "path-prefix");
    }

    let acknowledged = run_with_env(
        &checkout,
        &[
            "work-context",
            "acknowledge",
            "--for",
            "30m",
            "--format",
            "json",
        ],
        &alpha_env,
    );
    assert_eq!(
        acknowledged.code,
        0,
        "stderr={}",
        acknowledged.stderr_text()
    );
    let advised = run_with_env(
        &checkout,
        &["work-context", "advise", "--format", "json"],
        &alpha_env,
    );
    assert_eq!(advised.code, 0, "stderr={}", advised.stderr_text());
    assert_eq!(data(&advised)["severity"], "warning");
    assert_eq!(data(&advised)["suppressed"], true);

    let cleared = run_with_env(
        &checkout,
        &["work-context", "clear", "--format", "json"],
        &alpha_env,
    );
    assert_eq!(cleared.code, 0, "stderr={}", cleared.stderr_text());
    assert_eq!(data(&cleared)["released"], true);
    let status = run_with_env(
        &checkout,
        &["work-context", "status", "--format", "json"],
        &alpha_env,
    );
    assert_eq!(status.code, 0, "stderr={}", status.stderr_text());
    assert!(data(&status)["context"].is_null());
}

#[test]
fn advisory_lifecycle_skips_stopped_and_off_peers_but_preserves_known_overlap() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    fs::create_dir(&state_dir).expect("state");
    let checkout = tmp.path().join("checkout");
    init_checkout(&checkout, "https://github.com/example/repository.git");
    seed_brokers_at(
        &state_dir,
        &[
            (
                "alpha",
                "incarnation-alpha",
                "alpha-private-capability-material",
                checkout.as_path(),
                Some("advisory"),
            ),
            (
                "beta",
                "incarnation-beta",
                "beta-private-capability-material",
                checkout.as_path(),
                Some("advisory"),
            ),
            (
                "gamma",
                "incarnation-gamma",
                "gamma-private-capability-material",
                checkout.as_path(),
                Some("off"),
            ),
        ],
    );
    let state = state_dir.to_string_lossy();
    let alpha_cap = capability(&state_dir, "alpha");
    let alpha_env = [
        ("AGENT_SESSION_ID", "alpha"),
        ("AGENT_SESSION_CAPABILITY_FILE", alpha_cap.as_str()),
        ("AGENT_SESSION_STATE_DIR", state.as_ref()),
    ];

    rewrite_registry(&state_dir, |registry| {
        registry["brokers"]["gamma"]["state"] = json!("starting");
    });
    let initial = run_with_env(
        &checkout,
        &["work-context", "advise", "--format", "json"],
        &alpha_env,
    );
    assert_eq!(initial.code, 0, "stderr={}", initial.stderr_text());
    assert_eq!(data(&initial)["available"], true);
    assert_eq!(data(&initial)["severity"], "warning");
    assert_eq!(data(&initial)["peers"].as_array().expect("peers").len(), 1);

    let gamma_record = state_dir.join("sessions/gamma/session.json");
    let mut gamma: serde_json::Value =
        serde_json::from_slice(&fs::read(&gamma_record).expect("gamma record"))
            .expect("gamma json");
    gamma["coordination_mode"] = json!("advisory");
    fs::write(
        &gamma_record,
        serde_json::to_vec_pretty(&gamma).expect("gamma json"),
    )
    .expect("write gamma");
    let mixed = run_with_env(
        &checkout,
        &["work-context", "advise", "--format", "json"],
        &alpha_env,
    );
    assert_eq!(mixed.code, 0, "stderr={}", mixed.stderr_text());
    assert_eq!(data(&mixed)["available"], false);
    assert_eq!(data(&mixed)["severity"], "warning");
    assert_eq!(data(&mixed)["reasons"][0]["peer_session_id"], "beta");

    rewrite_registry(&state_dir, |registry| {
        registry["brokers"]["beta"]["state"] = json!("stopped");
        registry["brokers"]["gamma"]["state"] = json!("stopped");
    });
    let stopped = run_with_env(
        &checkout,
        &["work-context", "advise", "--format", "json"],
        &alpha_env,
    );
    assert_eq!(stopped.code, 0, "stderr={}", stopped.stderr_text());
    assert_eq!(data(&stopped)["available"], true);
    assert_eq!(data(&stopped)["severity"], "none");
    assert!(
        data(&stopped)["reasons"]
            .as_array()
            .expect("reasons")
            .is_empty()
    );
}

#[test]
fn advisory_commit_preserves_a_replacement_incarnation_observation() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    fs::create_dir(&state_dir).expect("state");
    let checkout = tmp.path().join("checkout");
    init_checkout(&checkout, "https://github.com/example/repository.git");
    seed_brokers_at(
        &state_dir,
        &[
            (
                "alpha",
                "incarnation-alpha",
                "alpha-private-capability-material",
                checkout.as_path(),
                Some("advisory"),
            ),
            (
                "beta",
                "incarnation-beta",
                "beta-private-capability-material",
                checkout.as_path(),
                Some("advisory"),
            ),
        ],
    );
    let fake_bin = tmp.path().join("fake-bin");
    fs::create_dir(&fake_bin).expect("fake bin");
    let started = tmp.path().join("git-started");
    let release = tmp.path().join("git-release");
    let git = fake_bin.join("git");
    fs::write(
        &git,
        "#!/usr/bin/env bash\nset -euo pipefail\n: >\"$GIT_PROBE_STARTED\"\nwhile [ ! -e \"$GIT_PROBE_RELEASE\" ]; do sleep 0.01; done\nprintf '%s\\n' https://github.com/example/repository.git\n",
    )
    .expect("fake git");
    fs::set_permissions(&git, fs::Permissions::from_mode(0o755)).expect("git mode");
    let path = format!(
        "{}:{}",
        fake_bin.display(),
        std::env::var("PATH").expect("PATH")
    );
    let capability = capability(&state_dir, "alpha");
    let child = Command::new(bin::resolve("agent-session"))
        .current_dir(&checkout)
        .args(["work-context", "advise", "--format", "json"])
        .env("AGENT_SESSION_ID", "alpha")
        .env("AGENT_SESSION_CAPABILITY_FILE", &capability)
        .env("AGENT_SESSION_STATE_DIR", &state_dir)
        .env("GIT_PROBE_STARTED", &started)
        .env("GIT_PROBE_RELEASE", &release)
        .env("PATH", path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn advise");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while !started.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(
        started.exists(),
        "advisory evaluation did not reach git probe"
    );
    let observed_at_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("current time")
        .as_secs();
    rewrite_registry(&state_dir, |registry| {
        registry["brokers"]["alpha"]["incarnation"] = json!("incarnation-replacement");
        registry["advisory_observations"]["alpha"] = json!({
            "session_incarnation": "incarnation-replacement",
            "advisory_digest": "replacement-observation-digest",
            "observed_at_epoch": observed_at_epoch
        });
    });
    fs::write(&release, b"release\n").expect("release git probe");
    let output = child.wait_with_output().expect("wait advise");
    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let registry: serde_json::Value = serde_json::from_slice(
        &fs::read(state_dir.join("coordination/registry.json")).expect("registry"),
    )
    .expect("registry json");
    assert_eq!(
        registry["advisory_observations"]["alpha"]["session_incarnation"],
        "incarnation-replacement"
    );
    assert_eq!(
        registry["advisory_observations"]["alpha"]["advisory_digest"],
        "replacement-observation-digest"
    );
}

#[test]
fn advisory_reuses_checkout_resolution_across_many_peers() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    fs::create_dir(&state_dir).expect("state");
    let checkout = tmp.path().join("checkout");
    init_checkout(&checkout, "https://github.com/example/repository.git");
    let owned = (0..12)
        .map(|index| {
            let id = if index == 0 {
                "alpha".to_string()
            } else {
                format!("peer-{index:02}")
            };
            (
                id.clone(),
                format!("incarnation-{id}"),
                format!("{id}-private-capability-material-0123456789"),
            )
        })
        .collect::<Vec<_>>();
    let sessions = owned
        .iter()
        .map(|(id, incarnation, capability)| {
            (
                id.as_str(),
                incarnation.as_str(),
                capability.as_str(),
                checkout.as_path(),
                Some("advisory"),
            )
        })
        .collect::<Vec<_>>();
    seed_brokers_at(&state_dir, &sessions);
    let fake_bin = tmp.path().join("fake-bin");
    fs::create_dir(&fake_bin).expect("fake bin");
    let counter = tmp.path().join("git-probe-count");
    let git = fake_bin.join("git");
    fs::write(
        &git,
        "#!/usr/bin/env bash\nset -euo pipefail\ncount=0\nif [ -f \"$GIT_PROBE_COUNT\" ]; then IFS= read -r count <\"$GIT_PROBE_COUNT\"; fi\nprintf '%s\\n' \"$((count + 1))\" >\"$GIT_PROBE_COUNT\"\nsleep 0.15\nprintf '%s\\n' https://github.com/example/repository.git\n",
    )
    .expect("fake git");
    fs::set_permissions(&git, fs::Permissions::from_mode(0o755)).expect("git mode");
    let path = format!(
        "{}:{}",
        fake_bin.display(),
        std::env::var("PATH").expect("PATH")
    );
    let capability = capability(&state_dir, "alpha");
    let state = state_dir.to_string_lossy();
    let started = std::time::Instant::now();
    let advised = run_with_env(
        &checkout,
        &["work-context", "advise", "--format", "json"],
        &[
            ("AGENT_SESSION_ID", "alpha"),
            ("AGENT_SESSION_CAPABILITY_FILE", capability.as_str()),
            ("AGENT_SESSION_STATE_DIR", state.as_ref()),
            ("GIT_PROBE_COUNT", counter.to_str().expect("counter path")),
            ("PATH", path.as_str()),
        ],
    );
    assert_eq!(advised.code, 0, "stderr={}", advised.stderr_text());
    assert!(
        started.elapsed() < std::time::Duration::from_secs(1),
        "advisory evaluation took {:?}",
        started.elapsed()
    );
    assert_eq!(
        fs::read_to_string(counter).expect("probe count").trim(),
        "1"
    );
}

#[test]
fn advisory_budget_exhaustion_marks_later_repository_resolution_incomplete() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    fs::create_dir(&state_dir).expect("state");
    let alpha_checkout = tmp.path().join("alpha-checkout");
    let slow_checkout = tmp.path().join("peer-a-slow-checkout");
    let overlap_checkout = tmp.path().join("peer-z-overlap-checkout");
    init_checkout(&alpha_checkout, "https://github.com/example/shared.git");
    init_checkout(&slow_checkout, "https://github.com/example/unrelated.git");
    init_checkout(&overlap_checkout, "https://github.com/example/shared.git");
    seed_brokers_at(
        &state_dir,
        &[
            (
                "alpha",
                "incarnation-alpha",
                "alpha-private-capability-material",
                alpha_checkout.as_path(),
                Some("advisory"),
            ),
            (
                "peer-a-slow",
                "incarnation-peer-a-slow",
                "peer-a-slow-private-capability-material",
                slow_checkout.as_path(),
                Some("advisory"),
            ),
            (
                "peer-z-overlap",
                "incarnation-peer-z-overlap",
                "peer-z-overlap-private-capability-material",
                overlap_checkout.as_path(),
                Some("advisory"),
            ),
        ],
    );
    let fake_bin = tmp.path().join("fake-bin");
    fs::create_dir(&fake_bin).expect("fake bin");
    let git = fake_bin.join("git");
    fs::write(
        &git,
        "#!/usr/bin/env bash\nset -euo pipefail\ncase \"$2\" in\n  *peer-a-slow-checkout) sleep 1; printf '%s\\n' https://github.com/example/unrelated.git ;;\n  *) printf '%s\\n' https://github.com/example/shared.git ;;\nesac\n",
    )
    .expect("fake git");
    fs::set_permissions(&git, fs::Permissions::from_mode(0o755)).expect("git mode");
    let path = format!(
        "{}:{}",
        fake_bin.display(),
        std::env::var("PATH").expect("PATH")
    );
    let capability = capability(&state_dir, "alpha");
    let state = state_dir.to_string_lossy();
    let advised = run_with_env(
        &alpha_checkout,
        &["work-context", "advise", "--format", "json"],
        &[
            ("AGENT_SESSION_ID", "alpha"),
            ("AGENT_SESSION_CAPABILITY_FILE", capability.as_str()),
            ("AGENT_SESSION_STATE_DIR", state.as_ref()),
            ("PATH", path.as_str()),
        ],
    );
    assert_eq!(advised.code, 0, "stderr={}", advised.stderr_text());
    let body = data(&advised);
    assert_eq!(body["available"], false);
    assert_eq!(body["severity"], "degraded");
}

#[test]
fn acknowledgement_is_bound_to_the_observed_overlap_and_expiry() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    fs::create_dir(&state_dir).expect("state");
    let checkout = tmp.path().join("checkout");
    init_checkout(&checkout, "https://github.com/example/repository.git");
    seed_brokers_at(
        &state_dir,
        &[
            (
                "alpha",
                "incarnation-alpha",
                "alpha-private-capability-material",
                checkout.as_path(),
                Some("advisory"),
            ),
            (
                "beta",
                "incarnation-beta",
                "beta-private-capability-material",
                checkout.as_path(),
                Some("advisory"),
            ),
            (
                "gamma",
                "incarnation-gamma",
                "gamma-private-capability-material",
                checkout.as_path(),
                Some("off"),
            ),
        ],
    );
    let state = state_dir.to_string_lossy();
    let alpha_cap = capability(&state_dir, "alpha");
    let alpha_env = [
        ("AGENT_SESSION_ID", "alpha"),
        ("AGENT_SESSION_CAPABILITY_FILE", alpha_cap.as_str()),
        ("AGENT_SESSION_STATE_DIR", state.as_ref()),
    ];
    let advise = || {
        run_with_env(
            &checkout,
            &["work-context", "advise", "--format", "json"],
            &alpha_env,
        )
    };

    let first = advise();
    assert_eq!(data(&first)["suppressed"], false);
    let acknowledged = run_with_env(
        &checkout,
        &[
            "work-context",
            "acknowledge",
            "--for",
            "30m",
            "--format",
            "json",
        ],
        &alpha_env,
    );
    assert_eq!(
        acknowledged.code,
        0,
        "stderr={}",
        acknowledged.stderr_text()
    );
    assert_eq!(data(&advise())["suppressed"], true);

    for target in ["src/one.rs", "src/two.rs"] {
        let path = tmp.path().join(target.replace('/', "-"));
        fs::write(
            &path,
            serde_json::to_vec_pretty(&json!({
                "schema_version": "agent-session.operation-targets.v1",
                "targets": [{
                    "kind": "path-exact",
                    "repository": "example/repository",
                    "value": target
                }],
                "provider_refs": [],
                "checkouts": []
            }))
            .expect("targets json"),
        )
        .expect("write targets");
        let targeted = run_with_env(
            &checkout,
            &[
                "work-context",
                "advise",
                "--targets-file",
                path.to_str().expect("target path"),
                "--format",
                "json",
            ],
            &alpha_env,
        );
        assert_eq!(targeted.code, 0, "stderr={}", targeted.stderr_text());
        assert_eq!(data(&targeted)["suppressed"], true);
    }

    let gamma_record = state_dir.join("sessions/gamma/session.json");
    let mut gamma: serde_json::Value =
        serde_json::from_slice(&fs::read(&gamma_record).expect("gamma record"))
            .expect("gamma json");
    gamma["coordination_mode"] = json!("advisory");
    fs::write(
        &gamma_record,
        serde_json::to_vec_pretty(&gamma).expect("gamma json"),
    )
    .expect("write gamma");
    let changed = advise();
    assert_eq!(data(&changed)["severity"], "warning");
    assert_eq!(data(&changed)["suppressed"], false);
    assert_eq!(data(&changed)["peers"].as_array().expect("peers").len(), 2);

    let acknowledged_again = run_with_env(
        &checkout,
        &["work-context", "acknowledge", "--format", "json"],
        &alpha_env,
    );
    assert_eq!(
        acknowledged_again.code,
        0,
        "stderr={}",
        acknowledged_again.stderr_text()
    );
    assert_eq!(data(&advise())["suppressed"], true);
    rewrite_registry(&state_dir, |registry| {
        registry["advisory_acknowledgements"]["alpha"]["expires_at_epoch"] = json!(0);
    });
    assert_eq!(data(&advise())["suppressed"], false);
}

#[test]
fn raw_claim_and_high_level_set_share_the_checkout_root_fingerprint() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    fs::create_dir(&state_dir).expect("state");
    let checkout = tmp.path().join("checkout");
    let nested = checkout.join("nested");
    init_checkout(&checkout, "https://github.com/example/repository.git");
    fs::create_dir(&nested).expect("nested checkout directory");
    seed_brokers_at(
        &state_dir,
        &[(
            "alpha",
            "incarnation-alpha",
            "alpha-private-capability-material",
            nested.as_path(),
            Some("enforce"),
        )],
    );
    let context_file = tmp.path().join("context.json");
    candidate(&context_file, "src/", "raw claim");
    let state = state_dir.to_string_lossy();
    let alpha_cap = capability(&state_dir, "alpha");
    let raw = run(
        &nested,
        &[
            "--state-dir",
            &state,
            "work-context",
            "claim",
            "--session",
            "alpha",
            "--file",
            context_file.to_str().expect("context path"),
            "--capability-file",
            &alpha_cap,
            "--idempotency-key",
            "fingerprint-parity-raw",
            "--format",
            "json",
        ],
    );
    assert_eq!(raw.code, 0, "stderr={}", raw.stderr_text());
    let raw_fingerprint = data(&raw)["context"]["worktrees"][0].clone();
    let alpha_env = [
        ("AGENT_SESSION_ID", "alpha"),
        ("AGENT_SESSION_CAPABILITY_FILE", alpha_cap.as_str()),
        ("AGENT_SESSION_STATE_DIR", state.as_ref()),
    ];
    let declared = run_with_env(
        &nested,
        &[
            "work-context",
            "set",
            "--summary",
            "high-level declaration",
            "--format",
            "json",
        ],
        &alpha_env,
    );
    assert_eq!(declared.code, 0, "stderr={}", declared.stderr_text());
    assert_eq!(data(&declared)["context"]["worktrees"][0], raw_fingerprint);
}

#[test]
fn coordination_public_identifiers_do_not_authorize_a_claim_or_echo_peer_data() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    fs::create_dir(&state_dir).expect("state directory");
    seed_session(&state_dir, "alpha", "incarnation-alpha");
    let candidate = tmp.path().join("candidate.json");
    let private_canary = "PRIVATE-COORDINATION-SUMMARY-CANARY";
    fs::write(
        &candidate,
        serde_json::to_vec_pretty(&json!({
            "schema_version": "agent-session.work-context-input.v1",
            "intent": "implementation",
            "tier": "L2",
            "repositories": ["example/repository"],
            "worktrees": [],
            "provider_refs": [],
            "plan_refs": [],
            "scopes": [{
                "kind": "path-prefix",
                "repository": "example/repository",
                "value": "src/"
            }],
            "summary": private_canary
        }))
        .expect("candidate json"),
    )
    .expect("write candidate");

    let output = run(
        tmp.path(),
        &[
            "--state-dir",
            state_dir.to_str().expect("state path"),
            "work-context",
            "claim",
            "--session",
            "alpha",
            "--file",
            candidate.to_str().expect("candidate path"),
            "--idempotency-key",
            "claim-without-capability",
            "--format",
            "json",
        ],
    );

    assert_ne!(output.code, 0);
    assert_eq!(
        output.stdout_json()["error"]["code"],
        "coordination-unauthorized"
    );
    assert!(
        output.stdout_json()["error"]["hint"]
            .as_str()
            .is_some_and(|hint| hint.contains("self show")),
        "coordination-unauthorized must carry a remedy hint"
    );
    let combined = format!("{}{}", output.stdout_text(), output.stderr_text());
    assert!(!combined.contains(private_canary), "{combined}");
    assert!(!combined.contains("incarnation-alpha"), "{combined}");
}

#[test]
fn atomic_claim_conflict_idempotency_and_uncovered_mutation_are_fenced() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    fs::create_dir(&state_dir).expect("state");
    seed_brokers(
        &state_dir,
        &[
            (
                "alpha",
                "incarnation-alpha",
                "alpha-private-capability-material",
            ),
            (
                "beta",
                "incarnation-beta",
                "beta-private-capability-material",
            ),
        ],
    );
    let alpha_candidate = tmp.path().join("alpha.json");
    let beta_candidate = tmp.path().join("beta.json");
    candidate(&alpha_candidate, "src/", "alpha context");
    candidate(&beta_candidate, "src/", "beta context");
    let state = state_dir.to_string_lossy();
    let alpha_cap = capability(&state_dir, "alpha");
    let beta_cap = capability(&state_dir, "beta");

    let alpha = run(
        tmp.path(),
        &[
            "--state-dir",
            &state,
            "work-context",
            "claim",
            "--session",
            "alpha",
            "--file",
            alpha_candidate.to_str().expect("candidate"),
            "--capability-file",
            &alpha_cap,
            "--idempotency-key",
            "claim-alpha-0001",
            "--format",
            "json",
        ],
    );
    assert_eq!(
        alpha.code,
        0,
        "stdout={} stderr={}",
        alpha.stdout_text(),
        alpha.stderr_text()
    );
    assert_eq!(
        data(&alpha)["evaluation"]["classification"],
        "unknown",
        "the unclaimed live beta peer prevents clear"
    );
    let alpha_claim = data(&alpha)["context"].clone();

    let retry = run(
        tmp.path(),
        &[
            "--state-dir",
            &state,
            "work-context",
            "claim",
            "--session",
            "alpha",
            "--file",
            alpha_candidate.to_str().expect("candidate"),
            "--capability-file",
            &alpha_cap,
            "--idempotency-key",
            "claim-alpha-0001",
            "--format",
            "json",
        ],
    );
    assert_eq!(retry.code, 0, "stderr={}", retry.stderr_text());
    assert_eq!(data(&retry)["context"]["claim_id"], alpha_claim["claim_id"]);

    let beta = run(
        tmp.path(),
        &[
            "--state-dir",
            &state,
            "work-context",
            "claim",
            "--session",
            "beta",
            "--file",
            beta_candidate.to_str().expect("candidate"),
            "--capability-file",
            &beta_cap,
            "--idempotency-key",
            "claim-beta-0001",
            "--format",
            "json",
        ],
    );
    assert_ne!(beta.code, 0);
    assert_eq!(beta.stdout_json()["error"]["code"], "claim-conflict");
    assert_eq!(
        beta.stdout_json()["error"]["details"]["evaluation"]["reasons"][0]["code"],
        "overlapping-scope"
    );

    let targets = tmp.path().join("targets.json");
    fs::write(
        &targets,
        serde_json::to_vec_pretty(&json!({
            "schema_version": "agent-session.operation-targets.v1",
            "targets": [{
                "kind": "path-exact",
                "repository": "example/repository",
                "value": "tests/outside.rs"
            }]
        }))
        .expect("targets"),
    )
    .expect("targets file");
    let execution_token = tmp.path().join("execution-token");
    fs::write(&execution_token, "execution-token-alpha").expect("execution token");
    fs::set_permissions(&execution_token, fs::Permissions::from_mode(0o600))
        .expect("execution token mode");
    let uncovered = run(
        tmp.path(),
        &[
            "--state-dir",
            &state,
            "work-context",
            "admit",
            "--session",
            "alpha",
            "--claim",
            alpha_claim["claim_id"].as_str().expect("claim id"),
            "--if-revision",
            "1",
            "--targets-file",
            targets.to_str().expect("targets"),
            "--operation",
            "edit",
            "--execution-token-file",
            execution_token.to_str().expect("execution token"),
            "--capability-file",
            &alpha_cap,
            "--idempotency-key",
            "admit-alpha-0001",
            "--format",
            "json",
        ],
    );
    assert_ne!(uncovered.code, 0);
    assert_eq!(
        uncovered.stdout_json()["error"]["code"],
        "uncovered-mutation-scope"
    );
}

#[test]
fn checkout_bound_shell_is_covered_by_the_claim_worktree() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let checkout = tmp.path().join("checkout");
    let other_checkout = tmp.path().join("other-checkout");
    fs::create_dir(&state_dir).expect("state");
    init_checkout(&checkout, "https://example.invalid/example/repository.git");
    init_checkout(
        &other_checkout,
        "https://example.invalid/example/repository.git",
    );
    seed_brokers_at(
        &state_dir,
        &[(
            "alpha",
            "incarnation-alpha",
            "alpha-private-capability-material",
            checkout.as_path(),
            Some("enforce"),
        )],
    );
    seed_activity_state(
        &state_dir,
        "alpha",
        "incarnation-alpha",
        "working",
        json!({
            "provider_turn_id": "turn-checkout-shell",
            "started_at": "2030-01-01T00:00:01Z"
        }),
        serde_json::Value::Null,
    );
    let _runtime = seed_live_runtime_identity(&state_dir, "alpha", "incarnation-alpha", 91);
    let candidate_file = tmp.path().join("candidate.json");
    candidate(&candidate_file, "src/owned/", "checkout shell context");
    let state = state_dir.to_string_lossy();
    let alpha_cap = capability(&state_dir, "alpha");
    let claimed = run(
        &checkout,
        &[
            "--state-dir",
            &state,
            "work-context",
            "claim",
            "--session",
            "alpha",
            "--file",
            candidate_file.to_str().expect("candidate"),
            "--capability-file",
            &alpha_cap,
            "--idempotency-key",
            "claim-checkout-shell-0001",
            "--format",
            "json",
        ],
    );
    assert_eq!(
        claimed.code,
        0,
        "stdout={} stderr={}",
        claimed.stdout_text(),
        claimed.stderr_text()
    );
    let claim = data(&claimed)["context"].clone();
    assert_eq!(claim["scopes"][0]["kind"], "path-prefix");

    let targets = tmp.path().join("checkout-shell-targets.json");
    fs::write(
        &targets,
        serde_json::to_vec_pretty(&json!({
            "schema_version": "agent-session.operation-targets.v1",
            "targets": [{
                "kind": "repository",
                "repository": "example/repository",
                "value": "."
            }],
            "provider_refs": [],
            "checkouts": [{
                "repository": "example/repository",
                "path": checkout
            }]
        }))
        .expect("targets"),
    )
    .expect("targets file");
    let execution_token = tmp.path().join("checkout-shell-token");
    fs::write(&execution_token, "execution-token-checkout-shell").expect("execution token");
    fs::set_permissions(&execution_token, fs::Permissions::from_mode(0o600))
        .expect("execution token mode");

    let other_targets = tmp.path().join("other-checkout-shell-targets.json");
    fs::write(
        &other_targets,
        serde_json::to_vec_pretty(&json!({
            "schema_version": "agent-session.operation-targets.v1",
            "targets": [{
                "kind": "repository",
                "repository": "example/repository",
                "value": "."
            }],
            "provider_refs": [],
            "checkouts": [{
                "repository": "example/repository",
                "path": other_checkout
            }]
        }))
        .expect("targets"),
    )
    .expect("targets file");
    let wrong_checkout = run(
        &checkout,
        &[
            "--state-dir",
            &state,
            "work-context",
            "admit",
            "--session",
            "alpha",
            "--claim",
            claim["claim_id"].as_str().expect("claim id"),
            "--if-revision",
            "1",
            "--targets-file",
            other_targets.to_str().expect("targets"),
            "--operation",
            "shell",
            "--execution-token-file",
            execution_token.to_str().expect("execution token"),
            "--capability-file",
            &alpha_cap,
            "--idempotency-key",
            "admit-other-checkout-shell-0001",
            "--format",
            "json",
        ],
    );
    assert_ne!(wrong_checkout.code, 0);
    assert_eq!(
        wrong_checkout.stdout_json()["error"]["code"],
        "uncovered-mutation-scope"
    );

    let outside_edit_targets = tmp.path().join("outside-edit-targets.json");
    fs::write(
        &outside_edit_targets,
        serde_json::to_vec_pretty(&json!({
            "schema_version": "agent-session.operation-targets.v1",
            "targets": [{
                "kind": "path-exact",
                "repository": "example/repository",
                "value": "tests/outside.rs"
            }]
        }))
        .expect("targets"),
    )
    .expect("targets file");
    let outside_edit = run(
        &checkout,
        &[
            "--state-dir",
            &state,
            "work-context",
            "admit",
            "--session",
            "alpha",
            "--claim",
            claim["claim_id"].as_str().expect("claim id"),
            "--if-revision",
            "1",
            "--targets-file",
            outside_edit_targets.to_str().expect("targets"),
            "--operation",
            "edit",
            "--execution-token-file",
            execution_token.to_str().expect("execution token"),
            "--capability-file",
            &alpha_cap,
            "--idempotency-key",
            "admit-outside-edit-0001",
            "--format",
            "json",
        ],
    );
    assert_ne!(outside_edit.code, 0);
    assert_eq!(
        outside_edit.stdout_json()["error"]["code"],
        "uncovered-mutation-scope"
    );

    let unassigned_shell = run(
        &checkout,
        &[
            "--state-dir",
            &state,
            "work-context",
            "admit",
            "--session",
            "alpha",
            "--claim",
            claim["claim_id"].as_str().expect("claim id"),
            "--if-revision",
            "1",
            "--targets-file",
            targets.to_str().expect("targets"),
            "--operation",
            "shell",
            "--execution-token-file",
            execution_token.to_str().expect("execution token"),
            "--capability-file",
            &alpha_cap,
            "--idempotency-key",
            "admit-unassigned-checkout-shell-0001",
            "--format",
            "json",
        ],
    );
    assert_ne!(unassigned_shell.code, 0);
    assert_eq!(
        unassigned_shell.stdout_json()["error"]["code"],
        "uncovered-mutation-scope"
    );
    grant_checkout_shell(&state_dir, &["alpha"]);
    let missing_binding_targets = tmp.path().join("missing-binding-targets.json");
    write_private_json(
        &missing_binding_targets,
        &json!({
            "schema_version": "agent-session.operation-targets.v1",
            "targets": [{
                "kind": "repository",
                "repository": "example/repository",
                "value": "."
            }]
        }),
    );
    let additional_binding_targets = tmp.path().join("additional-binding-targets.json");
    write_private_json(
        &additional_binding_targets,
        &json!({
            "schema_version": "agent-session.operation-targets.v1",
            "targets": [{
                "kind": "repository",
                "repository": "example/repository",
                "value": "."
            }],
            "checkouts": [{
                "repository": "example/repository",
                "path": checkout
            }, {
                "repository": "example/repository",
                "path": other_checkout
            }]
        }),
    );
    let mismatched_repository_targets = tmp.path().join("mismatched-repository-targets.json");
    write_private_json(
        &mismatched_repository_targets,
        &json!({
            "schema_version": "agent-session.operation-targets.v1",
            "targets": [{
                "kind": "repository",
                "repository": "example/repository",
                "value": "."
            }],
            "checkouts": [{
                "repository": "other/repository",
                "path": checkout
            }]
        }),
    );
    let multiple_targets = tmp.path().join("multiple-shell-targets.json");
    write_private_json(
        &multiple_targets,
        &json!({
            "schema_version": "agent-session.operation-targets.v1",
            "targets": [{
                "kind": "repository",
                "repository": "example/repository",
                "value": "."
            }, {
                "kind": "path-exact",
                "repository": "example/repository",
                "value": "src/owned/generated.rs"
            }],
            "checkouts": [{
                "repository": "example/repository",
                "path": checkout
            }]
        }),
    );
    for (operation, targets_file, key) in [
        ("edit", targets.as_path(), "admit-repository-edit-0001"),
        (
            "shell",
            missing_binding_targets.as_path(),
            "admit-missing-binding-0001",
        ),
        (
            "shell",
            additional_binding_targets.as_path(),
            "admit-additional-binding-0001",
        ),
        (
            "shell",
            mismatched_repository_targets.as_path(),
            "admit-mismatched-repository-0001",
        ),
        (
            "shell",
            multiple_targets.as_path(),
            "admit-multiple-shell-targets-0001",
        ),
        (
            "shell",
            other_targets.as_path(),
            "admit-other-checkout-assigned-0001",
        ),
    ] {
        let rejected = run(
            &checkout,
            &[
                "--state-dir",
                &state,
                "work-context",
                "admit",
                "--session",
                "alpha",
                "--claim",
                claim["claim_id"].as_str().expect("claim id"),
                "--if-revision",
                "1",
                "--targets-file",
                targets_file.to_str().expect("targets"),
                "--operation",
                operation,
                "--execution-token-file",
                execution_token.to_str().expect("execution token"),
                "--capability-file",
                &alpha_cap,
                "--idempotency-key",
                key,
                "--format",
                "json",
            ],
        );
        assert_ne!(rejected.code, 0, "operation={operation} key={key}");
        assert_eq!(
            rejected.stdout_json()["error"]["code"],
            "uncovered-mutation-scope",
            "operation={operation} key={key}"
        );
    }

    let admitted = run(
        &checkout,
        &[
            "--state-dir",
            &state,
            "work-context",
            "admit",
            "--session",
            "alpha",
            "--claim",
            claim["claim_id"].as_str().expect("claim id"),
            "--if-revision",
            "1",
            "--targets-file",
            targets.to_str().expect("targets"),
            "--operation",
            "shell",
            "--execution-token-file",
            execution_token.to_str().expect("execution token"),
            "--capability-file",
            &alpha_cap,
            "--idempotency-key",
            "admit-checkout-shell-0001",
            "--format",
            "json",
        ],
    );
    assert_eq!(
        admitted.code,
        0,
        "stdout={} stderr={}",
        admitted.stdout_text(),
        admitted.stderr_text()
    );
    assert_eq!(data(&admitted)["operation"], "shell");
    assert_eq!(data(&admitted)["targets"][0]["kind"], "repository");
}

#[test]
fn checkout_bound_shells_in_distinct_worktrees_can_run_concurrently() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let alpha_checkout = tmp.path().join("alpha-checkout");
    let beta_checkout = tmp.path().join("beta-checkout");
    fs::create_dir(&state_dir).expect("state");
    init_checkout(
        &alpha_checkout,
        "https://example.invalid/example/repository.git",
    );
    init_checkout(
        &beta_checkout,
        "https://example.invalid/example/repository.git",
    );
    seed_brokers_at(
        &state_dir,
        &[
            (
                "alpha",
                "incarnation-alpha",
                "alpha-private-capability-material",
                alpha_checkout.as_path(),
                Some("enforce"),
            ),
            (
                "beta",
                "incarnation-beta",
                "beta-private-capability-material",
                beta_checkout.as_path(),
                Some("enforce"),
            ),
        ],
    );
    for (id, incarnation, turn) in [
        ("alpha", "incarnation-alpha", "turn-alpha-shell"),
        ("beta", "incarnation-beta", "turn-beta-shell"),
    ] {
        seed_activity_state(
            &state_dir,
            id,
            incarnation,
            "working",
            json!({
                "provider_turn_id": turn,
                "started_at": "2030-01-01T00:00:01Z"
            }),
            serde_json::Value::Null,
        );
    }
    let _alpha_runtime = seed_live_runtime_identity(&state_dir, "alpha", "incarnation-alpha", 92);
    let _beta_runtime = seed_live_runtime_identity(&state_dir, "beta", "incarnation-beta", 93);
    let state = state_dir.to_string_lossy();

    let mut claims = serde_json::Map::new();
    for (id, checkout, prefix) in [
        ("alpha", alpha_checkout.as_path(), "src/alpha/"),
        ("beta", beta_checkout.as_path(), "src/beta/"),
    ] {
        let candidate_file = tmp.path().join(format!("{id}-candidate.json"));
        candidate(&candidate_file, prefix, &format!("{id} checkout shell"));
        let capability_file = capability(&state_dir, id);
        let claimed = run(
            checkout,
            &[
                "--state-dir",
                &state,
                "work-context",
                "claim",
                "--session",
                id,
                "--file",
                candidate_file.to_str().expect("candidate"),
                "--capability-file",
                &capability_file,
                "--idempotency-key",
                &format!("claim-{id}-checkout-shell-0001"),
                "--format",
                "json",
            ],
        );
        assert_eq!(
            claimed.code,
            0,
            "id={id} stdout={} stderr={}",
            claimed.stdout_text(),
            claimed.stderr_text()
        );
        claims.insert(id.to_string(), data(&claimed)["context"].clone());
    }
    grant_checkout_shell(&state_dir, &["alpha", "beta"]);

    let mut admitted_leases = Vec::new();
    for (id, checkout) in [
        ("alpha", alpha_checkout.as_path()),
        ("beta", beta_checkout.as_path()),
    ] {
        let targets_file = tmp.path().join(format!("{id}-shell-targets.json"));
        fs::write(
            &targets_file,
            serde_json::to_vec_pretty(&json!({
                "schema_version": "agent-session.operation-targets.v1",
                "targets": [{
                    "kind": "repository",
                    "repository": "example/repository",
                    "value": "."
                }],
                "provider_refs": [],
                "checkouts": [{
                    "repository": "example/repository",
                    "path": checkout
                }]
            }))
            .expect("targets"),
        )
        .expect("targets file");
        let execution_token = tmp.path().join(format!("{id}-shell-token"));
        fs::write(&execution_token, format!("execution-token-{id}-shell"))
            .expect("execution token");
        fs::set_permissions(&execution_token, fs::Permissions::from_mode(0o600))
            .expect("execution token mode");
        let capability_file = capability(&state_dir, id);
        let claim = &claims[id];
        let admitted = run(
            checkout,
            &[
                "--state-dir",
                &state,
                "work-context",
                "admit",
                "--session",
                id,
                "--claim",
                claim["claim_id"].as_str().expect("claim id"),
                "--if-revision",
                "1",
                "--targets-file",
                targets_file.to_str().expect("targets"),
                "--operation",
                "shell",
                "--execution-token-file",
                execution_token.to_str().expect("execution token"),
                "--capability-file",
                &capability_file,
                "--idempotency-key",
                &format!("admit-{id}-checkout-shell-0001"),
                "--format",
                "json",
            ],
        );
        assert_eq!(
            admitted.code,
            0,
            "id={id} stdout={} stderr={}",
            admitted.stdout_text(),
            admitted.stderr_text()
        );
        admitted_leases.push(data(&admitted)["lease_id"].clone());
    }
    assert_ne!(admitted_leases[0], admitted_leases[1]);
}

#[test]
fn concurrent_definite_contenders_admit_exactly_one_claim() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    fs::create_dir(&state_dir).expect("state");
    seed_brokers(
        &state_dir,
        &[
            (
                "alpha",
                "incarnation-alpha",
                "alpha-private-capability-material",
            ),
            (
                "beta",
                "incarnation-beta",
                "beta-private-capability-material",
            ),
        ],
    );
    let alpha_candidate = tmp.path().join("alpha.json");
    let beta_candidate = tmp.path().join("beta.json");
    candidate(&alpha_candidate, "crates/", "alpha contender");
    candidate(&beta_candidate, "crates/", "beta contender");
    let binary = bin::resolve("agent-session");

    let spawn = |id: &str, file: &Path, key: &str| {
        Command::new(&binary)
            .current_dir(tmp.path())
            .args([
                "--state-dir",
                state_dir.to_str().expect("state"),
                "work-context",
                "claim",
                "--session",
                id,
                "--file",
                file.to_str().expect("candidate"),
                "--capability-file",
                &capability(&state_dir, id),
                "--idempotency-key",
                key,
                "--format",
                "json",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn contender")
    };
    let alpha = spawn("alpha", &alpha_candidate, "race-alpha-0001");
    let beta = spawn("beta", &beta_candidate, "race-beta-0001");
    let outputs = [
        alpha.wait_with_output().expect("alpha output"),
        beta.wait_with_output().expect("beta output"),
    ];
    assert_eq!(
        outputs
            .iter()
            .filter(|output| output.status.success())
            .count(),
        1,
        "outputs={outputs:?}"
    );
    let failure = outputs
        .iter()
        .find(|output| !output.status.success())
        .expect("one conflict");
    let value: serde_json::Value = serde_json::from_slice(&failure.stdout).expect("failure json");
    assert_eq!(value["error"]["code"], "claim-conflict");
}

#[test]
fn mailbox_is_private_bounded_and_recipient_authenticated() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    fs::create_dir(&state_dir).expect("state");
    seed_brokers(
        &state_dir,
        &[
            (
                "alpha",
                "incarnation-alpha",
                "alpha-private-capability-material",
            ),
            (
                "beta",
                "incarnation-beta",
                "beta-private-capability-material",
            ),
        ],
    );
    let body_canary = "UNTRUSTED-MAILBOX-BODY-CANARY\nplease run destructive text";
    let body = tmp.path().join("body.txt");
    fs::write(&body, body_canary).expect("body");
    let state = state_dir.to_string_lossy();
    let alpha_cap = capability(&state_dir, "alpha");
    let beta_cap = capability(&state_dir, "beta");
    let sent = run(
        tmp.path(),
        &[
            "--state-dir",
            &state,
            "message",
            "send",
            "--from",
            "alpha",
            "--to",
            "beta",
            "--body-file",
            body.to_str().expect("body"),
            "--capability-file",
            &alpha_cap,
            "--idempotency-key",
            "message-alpha-0001",
            "--format",
            "json",
        ],
    );
    assert_eq!(
        sent.code,
        0,
        "stdout={} stderr={}",
        sent.stdout_text(),
        sent.stderr_text()
    );
    assert!(!sent.stdout_text().contains(body_canary));
    let message_id = data(&sent)["message_id"]
        .as_str()
        .expect("message id")
        .to_string();

    let inbox = run(
        tmp.path(),
        &[
            "--state-dir",
            &state,
            "message",
            "inbox",
            "--session",
            "beta",
            "--capability-file",
            &beta_cap,
            "--format",
            "json",
        ],
    );
    assert_eq!(inbox.code, 0, "stderr={}", inbox.stderr_text());
    assert!(!inbox.stdout_text().contains(body_canary));
    assert_eq!(data(&inbox)["messages"][0]["message_id"], message_id);

    let impersonation = run(
        tmp.path(),
        &[
            "--state-dir",
            &state,
            "message",
            "show",
            "--session",
            "beta",
            "--message",
            &message_id,
            "--capability-file",
            &alpha_cap,
            "--format",
            "json",
        ],
    );
    assert_ne!(impersonation.code, 0);
    assert_eq!(
        impersonation.stdout_json()["error"]["code"],
        "coordination-unauthorized"
    );
    assert!(!impersonation.stdout_text().contains(body_canary));

    let shown = run(
        tmp.path(),
        &[
            "--state-dir",
            &state,
            "message",
            "show",
            "--session",
            "beta",
            "--message",
            &message_id,
            "--capability-file",
            &beta_cap,
            "--format",
            "json",
        ],
    );
    assert_eq!(shown.code, 0, "stderr={}", shown.stderr_text());
    assert_eq!(
        data(&shown)["body"]["classification"],
        "untrusted_peer_data"
    );
    assert_eq!(data(&shown)["body"]["text"], body_canary);

    let registry = state_dir.join("coordination/registry.json");
    assert_eq!(
        fs::metadata(registry)
            .expect("registry")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(state_dir.join("coordination"))
            .expect("coordination root")
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
}

#[test]
fn cli_send_and_reply_share_recipient_generation_scheduling() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    fs::create_dir(&state_dir).expect("state");
    seed_brokers(
        &state_dir,
        &[
            (
                "alpha",
                "incarnation-alpha",
                "alpha-private-capability-material",
            ),
            (
                "beta",
                "incarnation-beta",
                "beta-private-capability-material",
            ),
        ],
    );
    let first_body = tmp.path().join("first.txt");
    let second_body = tmp.path().join("second.txt");
    let reply_body = tmp.path().join("reply.txt");
    fs::write(&first_body, "first private body").expect("first body");
    fs::write(&second_body, "second private body").expect("second body");
    fs::write(&reply_body, "private reply").expect("reply body");
    let state = state_dir.to_string_lossy();
    let alpha_cap = capability(&state_dir, "alpha");
    let beta_cap = capability(&state_dir, "beta");

    let send = |body: &Path, key: &str| {
        run(
            tmp.path(),
            &[
                "--state-dir",
                &state,
                "message",
                "send",
                "--from",
                "alpha",
                "--to",
                "beta",
                "--body-file",
                body.to_str().expect("body"),
                "--capability-file",
                &alpha_cap,
                "--idempotency-key",
                key,
                "--format",
                "json",
            ],
        )
    };
    let first = send(&first_body, "generation-send-0001");
    assert_eq!(first.code, 0, "stderr={}", first.stderr_text());
    assert_eq!(data(&first)["notification"]["state"], "queued");
    assert_eq!(data(&first)["notification"]["generation"], 1);
    assert_eq!(data(&first)["notification"]["notified_generation"], 0);
    assert_eq!(
        data(&first)["notification"]["last_reason"],
        "notification-pending"
    );
    assert_eq!(data(&first)["notification"]["controller_available"], false);
    assert!(!first.stdout_text().contains("first private body"));
    let message_id = data(&first)["message_id"]
        .as_str()
        .expect("message id")
        .to_string();
    let replay = send(&first_body, "generation-send-0001");
    assert_eq!(replay.code, 0, "stderr={}", replay.stderr_text());
    assert_eq!(data(&replay)["message_id"], message_id);
    assert_eq!(data(&replay)["notification"]["generation"], 1);
    let second = send(&second_body, "generation-send-0002");
    assert_eq!(second.code, 0, "stderr={}", second.stderr_text());
    assert_eq!(data(&second)["notification"]["generation"], 2);

    let reply = run(
        tmp.path(),
        &[
            "--state-dir",
            &state,
            "message",
            "reply",
            "--session",
            "beta",
            "--message",
            &message_id,
            "--if-revision",
            "1",
            "--body-file",
            reply_body.to_str().expect("reply body"),
            "--capability-file",
            &beta_cap,
            "--idempotency-key",
            "generation-reply-0001",
            "--format",
            "json",
        ],
    );
    assert_eq!(reply.code, 0, "stderr={}", reply.stderr_text());
    assert_eq!(data(&reply)["notification"]["state"], "queued");
    assert_eq!(data(&reply)["notification"]["generation"], 1);
    assert_eq!(data(&reply)["notification"]["controller_available"], false);
    assert!(!reply.stdout_text().contains("private reply"));

    let registry: serde_json::Value = serde_json::from_slice(
        &fs::read(state_dir.join("coordination/registry.json")).expect("registry"),
    )
    .expect("registry json");
    let notifications = registry["notifications"]
        .as_object()
        .expect("notifications");
    assert_eq!(notifications.len(), 2);
    let beta = notifications
        .values()
        .find(|receipt| receipt["target_session_id"] == "beta")
        .expect("beta generation");
    assert_eq!(
        beta["schema_version"],
        "agent-session.notification-generation.v1"
    );
    assert_eq!(beta["target_incarnation"], "incarnation-beta");
    assert_eq!(beta["generation"], 2);
    assert_eq!(beta["state"], "queued");
    let alpha = notifications
        .values()
        .find(|receipt| receipt["target_session_id"] == "alpha")
        .expect("alpha generation");
    assert_eq!(alpha["target_incarnation"], "incarnation-alpha");
    assert_eq!(alpha["generation"], 1);
    assert_eq!(alpha["state"], "queued");
}

#[test]
fn coordination_review_envelopes_identify_the_exact_operation() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    fs::create_dir(&state_dir).expect("state");
    seed_brokers(
        &state_dir,
        &[(
            "alpha",
            "incarnation-alpha",
            "alpha-private-capability-material",
        )],
    );
    let output = run(
        tmp.path(),
        &[
            "--state-dir",
            state_dir.to_str().expect("state"),
            "message",
            "inbox",
            "--session",
            "alpha",
            "--capability-file",
            &capability(&state_dir, "alpha"),
            "--format",
            "json",
        ],
    );
    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    assert_eq!(
        output.stdout_json()["schema_version"],
        "cli.agent-session.message-inbox.v1"
    );
}

#[test]
fn coordination_review_wait_processes_message_expiry() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    fs::create_dir(&state_dir).expect("state");
    seed_brokers(
        &state_dir,
        &[
            (
                "alpha",
                "incarnation-alpha",
                "alpha-private-capability-material",
            ),
            (
                "beta",
                "incarnation-beta",
                "beta-private-capability-material",
            ),
        ],
    );
    let body = tmp.path().join("body.txt");
    fs::write(&body, "expires shortly").expect("body");
    let sent = run(
        tmp.path(),
        &[
            "--state-dir",
            state_dir.to_str().expect("state"),
            "message",
            "send",
            "--from",
            "alpha",
            "--to",
            "beta",
            "--body-file",
            body.to_str().expect("body"),
            "--expires-in",
            "1s",
            "--capability-file",
            &capability(&state_dir, "alpha"),
            "--idempotency-key",
            "message-expiry-review-0001",
            "--format",
            "json",
        ],
    );
    assert_eq!(sent.code, 0, "stderr={}", sent.stderr_text());
    let message_id = data(&sent)["message_id"]
        .as_str()
        .expect("message id")
        .to_string();
    std::thread::sleep(std::time::Duration::from_millis(1_100));
    let waited = run(
        tmp.path(),
        &[
            "--state-dir",
            state_dir.to_str().expect("state"),
            "message",
            "wait",
            "--session",
            "beta",
            "--message",
            &message_id,
            "--if-revision",
            "1",
            "--timeout",
            "1s",
            "--capability-file",
            &capability(&state_dir, "beta"),
            "--format",
            "json",
        ],
    );
    assert_ne!(waited.code, 0);
    assert_eq!(waited.stdout_json()["error"]["code"], "message-expired");
}

#[test]
fn coordination_review_registry_lock_rejects_symlinks_without_chmod() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    fs::create_dir(&state_dir).expect("state");
    seed_brokers(
        &state_dir,
        &[(
            "alpha",
            "incarnation-alpha",
            "alpha-private-capability-material",
        )],
    );
    let lock_path = state_dir.join("coordination/registry.lock");
    let sentinel = tmp.path().join("sentinel");
    fs::write(&sentinel, "do not touch").expect("sentinel");
    fs::set_permissions(&sentinel, fs::Permissions::from_mode(0o644)).expect("sentinel mode");
    std::os::unix::fs::symlink(&sentinel, &lock_path).expect("lock symlink");

    let output = run(
        tmp.path(),
        &[
            "--state-dir",
            state_dir.to_str().expect("state"),
            "message",
            "inbox",
            "--session",
            "alpha",
            "--capability-file",
            &capability(&state_dir, "alpha"),
            "--format",
            "json",
        ],
    );
    assert_ne!(output.code, 0);
    assert_eq!(
        output.stdout_json()["error"]["code"],
        "coordination-store-untrusted"
    );
    assert_eq!(
        fs::metadata(&sentinel)
            .expect("sentinel metadata")
            .permissions()
            .mode()
            & 0o777,
        0o644
    );
}

#[test]
fn coordination_review_bound_operation_blocks_claim_release_and_replacement() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    fs::create_dir(&state_dir).expect("state");
    seed_brokers(
        &state_dir,
        &[(
            "alpha",
            "incarnation-alpha",
            "alpha-private-capability-material",
        )],
    );
    let context_file = tmp.path().join("context.json");
    candidate(&context_file, "src/", "claim with active operation");
    let common = state_dir.to_string_lossy();
    let cap = capability(&state_dir, "alpha");
    let claimed = run(
        tmp.path(),
        &[
            "--state-dir",
            &common,
            "work-context",
            "claim",
            "--session",
            "alpha",
            "--file",
            context_file.to_str().expect("context"),
            "--capability-file",
            &cap,
            "--idempotency-key",
            "review-bound-claim-0001",
            "--format",
            "json",
        ],
    );
    assert_eq!(claimed.code, 0, "stderr={}", claimed.stderr_text());
    let claim_id = data(&claimed)["context"]["claim_id"]
        .as_str()
        .expect("claim id")
        .to_string();
    let registry_path = state_dir.join("coordination/registry.json");
    let mut registry: serde_json::Value =
        serde_json::from_slice(&fs::read(&registry_path).expect("registry")).expect("json");
    registry["operations"]
        .as_array_mut()
        .expect("operations")
        .push(json!({
            "schema_version": "agent-session.operation-lease.v1",
            "lease_id": "review-lease",
            "session_id": "alpha",
            "session_incarnation": "incarnation-alpha",
            "claim_id": claim_id,
            "claim_revision": 1,
            "operation": "edit",
            "targets": [{"kind": "path-exact", "repository": "example/repository", "value": "src/lib.rs"}],
            "state": "active",
            "revision": 1,
            "started_at": "2030-01-01T00:00:00Z",
            "expires_at": "2030-01-08T00:00:00Z",
            "expires_at_epoch": i64::MAX,
            "execution_token_digest": "digest",
            "activity_revision": 1,
            "runtime_identity_digest": "runtime"
        }));
    fs::write(
        &registry_path,
        serde_json::to_vec_pretty(&registry).expect("registry json"),
    )
    .expect("write registry");
    fs::set_permissions(&registry_path, fs::Permissions::from_mode(0o600)).expect("registry mode");

    let released = run(
        tmp.path(),
        &[
            "--state-dir",
            &common,
            "work-context",
            "release",
            "--session",
            "alpha",
            "--claim",
            &claim_id,
            "--if-revision",
            "1",
            "--capability-file",
            &cap,
            "--idempotency-key",
            "review-bound-release-0001",
            "--format",
            "json",
        ],
    );
    assert_ne!(released.code, 0);
    assert_eq!(
        released.stdout_json()["error"]["code"],
        "operation-in-progress"
    );

    let replaced = run(
        tmp.path(),
        &[
            "--state-dir",
            &common,
            "work-context",
            "claim",
            "--session",
            "alpha",
            "--file",
            context_file.to_str().expect("context"),
            "--if-revision",
            "1",
            "--capability-file",
            &cap,
            "--idempotency-key",
            "review-bound-replace-0001",
            "--format",
            "json",
        ],
    );
    assert_ne!(replaced.code, 0);
    assert_eq!(
        replaced.stdout_json()["error"]["code"],
        "operation-in-progress"
    );
}

#[test]
fn coordination_review_recovery_rejects_a_healthy_exact_broker() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    fs::create_dir(&state_dir).expect("state");
    seed_brokers(
        &state_dir,
        &[(
            "alpha",
            "incarnation-alpha",
            "alpha-private-capability-material",
        )],
    );
    let proof = tmp.path().join("proof.json");
    fs::write(
        &proof,
        serde_json::to_vec_pretty(&json!({
            "schema_version": "agent-session.coordination-recovery-proof.v1",
            "session_incarnation": "incarnation-alpha",
            "generation": 1
        }))
        .expect("proof json"),
    )
    .expect("proof");
    let alpha_capability = capability(&state_dir, "alpha");
    let recovered = run(
        tmp.path(),
        &[
            "--state-dir",
            state_dir.to_str().expect("state"),
            "broker",
            "adopt",
            "--session",
            "alpha",
            "--capability-file",
            &alpha_capability,
            "--proof-file",
            proof.to_str().expect("proof"),
            "--idempotency-key",
            "review-healthy-recovery-0001",
            "--format",
            "json",
        ],
    );
    assert_ne!(recovered.code, 0);
    assert_eq!(
        recovered.stdout_json()["error"]["code"],
        "coordination-broker-not-lost"
    );
}

#[test]
fn broker_recovery_rejects_cross_session_capabilities_without_state_change() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    fs::create_dir(&state_dir).expect("state");
    seed_brokers(
        &state_dir,
        &[
            (
                "alpha",
                "incarnation-alpha",
                "alpha-private-capability-material",
            ),
            (
                "beta",
                "incarnation-beta",
                "beta-private-capability-material",
            ),
        ],
    );
    let proof = tmp.path().join("proof.json");
    fs::write(
        &proof,
        serde_json::to_vec_pretty(&json!({
            "schema_version": "agent-session.coordination-recovery-proof.v1",
            "session_incarnation": "incarnation-alpha",
            "generation": 1
        }))
        .expect("proof json"),
    )
    .expect("proof");
    let before = fs::read(state_dir.join("coordination/registry.json"))
        .expect("coordination registry before unauthorized recovery");
    let beta_capability = capability(&state_dir, "beta");

    for (subcommand, extra) in [
        ("adopt", Vec::<&str>::new()),
        (
            "reconcile",
            vec![
                "--operation",
                "lease-alpha",
                "--if-revision",
                "1",
                "--attest-inactive",
            ],
        ),
    ] {
        let mut args = vec![
            "--state-dir",
            state_dir.to_str().expect("state"),
            "broker",
            subcommand,
            "--session",
            "alpha",
            "--capability-file",
            &beta_capability,
            "--proof-file",
            proof.to_str().expect("proof"),
            "--idempotency-key",
            if subcommand == "adopt" {
                "cross-session-adopt-0001"
            } else {
                "cross-session-reconcile-0001"
            },
        ];
        args.extend(extra);
        args.extend(["--format", "json"]);
        let recovered = run(tmp.path(), &args);
        assert_ne!(recovered.code, 0);
        assert_eq!(
            recovered.stdout_json()["error"]["code"],
            "coordination-unauthorized",
            "cross-session {subcommand} must fail at capability authentication"
        );
        assert_eq!(
            fs::read(state_dir.join("coordination/registry.json"))
                .expect("coordination registry after unauthorized recovery"),
            before,
            "unauthorized {subcommand} must not change broker or operation state"
        );
    }
}

#[test]
fn broker_recovery_rejects_copied_capability_from_replaced_same_id_incarnation() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    fs::create_dir(&state_dir).expect("state");
    seed_brokers(
        &state_dir,
        &[(
            "alpha",
            "incarnation-old",
            "alpha-old-private-capability-material",
        )],
    );
    let copied_old_capability = tmp.path().join("copied-old-capability");
    fs::copy(capability(&state_dir, "alpha"), &copied_old_capability)
        .expect("copy prior incarnation capability");
    fs::set_permissions(&copied_old_capability, fs::Permissions::from_mode(0o600))
        .expect("copied capability mode");
    fs::remove_dir_all(state_dir.join("sessions/alpha"))
        .expect("remove prior incarnation session state");
    fs::remove_dir_all(state_dir.join("coordination"))
        .expect("remove prior incarnation coordination registry");

    seed_brokers(
        &state_dir,
        &[(
            "alpha",
            "incarnation-new",
            "alpha-new-private-capability-material",
        )],
    );
    let proof = tmp.path().join("proof.json");
    fs::write(
        &proof,
        serde_json::to_vec_pretty(&json!({
            "schema_version": "agent-session.coordination-recovery-proof.v1",
            "session_incarnation": "incarnation-new",
            "generation": 1
        }))
        .expect("proof json"),
    )
    .expect("proof");
    let before = fs::read(state_dir.join("coordination/registry.json"))
        .expect("coordination registry before replaced-incarnation recovery");
    let recovered = run(
        tmp.path(),
        &[
            "--state-dir",
            state_dir.to_str().expect("state"),
            "broker",
            "adopt",
            "--session",
            "alpha",
            "--capability-file",
            copied_old_capability.to_str().expect("copied capability"),
            "--proof-file",
            proof.to_str().expect("proof"),
            "--idempotency-key",
            "replaced-incarnation-adopt-0001",
            "--format",
            "json",
        ],
    );
    assert_ne!(recovered.code, 0);
    assert_eq!(
        recovered.stdout_json()["error"]["code"],
        "coordination-unauthorized"
    );
    assert_eq!(
        fs::read(state_dir.join("coordination/registry.json"))
            .expect("coordination registry after replaced-incarnation recovery"),
        before,
        "a copied prior-incarnation capability must not mutate recovery state"
    );
}

#[test]
fn coordination_review_target_exit_revokes_copied_capability_without_hiding_public_status() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    fs::create_dir(&state_dir).expect("state");
    seed_brokers(
        &state_dir,
        &[(
            "alpha",
            "incarnation-alpha",
            "alpha-private-capability-material",
        )],
    );
    let live_capability = capability(&state_dir, "alpha");
    let copied_capability = tmp.path().join("copied-capability");
    fs::copy(&live_capability, &copied_capability).expect("copy capability");
    fs::set_permissions(&copied_capability, fs::Permissions::from_mode(0o600))
        .expect("copied capability mode");
    fs::remove_file(&live_capability).expect("simulate target exit revocation");

    let status = run(
        tmp.path(),
        &[
            "--state-dir",
            state_dir.to_str().expect("state"),
            "broker",
            "status",
            "--session",
            "alpha",
            "--capability-file",
            copied_capability.to_str().expect("copied capability"),
            "--format",
            "json",
        ],
    );
    assert_eq!(status.code, 0, "stderr={}", status.stderr_text());
    assert_eq!(data(&status)["capability_available"], false);
}

#[test]
fn coordination_review_round2_half_ttl_renew_does_not_self_conflict() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    fs::create_dir(&state_dir).expect("state");
    seed_brokers(
        &state_dir,
        &[(
            "alpha",
            "incarnation-alpha",
            "alpha-private-capability-material",
        )],
    );
    let candidate_file = tmp.path().join("candidate.json");
    candidate(&candidate_file, "src/", "renewable claim");
    let claimed = run(
        tmp.path(),
        &[
            "--state-dir",
            state_dir.to_str().expect("state"),
            "work-context",
            "claim",
            "--session",
            "alpha",
            "--file",
            candidate_file.to_str().expect("candidate"),
            "--capability-file",
            &capability(&state_dir, "alpha"),
            "--idempotency-key",
            "round2-renew-claim-0001",
            "--format",
            "json",
        ],
    );
    assert_eq!(claimed.code, 0, "stderr={}", claimed.stderr_text());
    let claim_id = data(&claimed)["context"]["claim_id"]
        .as_str()
        .expect("claim id")
        .to_string();
    let near_expiry = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs() as i64
        + 10;
    rewrite_registry(&state_dir, |registry| {
        registry["claims"][0]["expires_at_epoch"] = json!(near_expiry);
    });

    let renewed = run(
        tmp.path(),
        &[
            "--state-dir",
            state_dir.to_str().expect("state"),
            "work-context",
            "renew",
            "--session",
            "alpha",
            "--claim",
            &claim_id,
            "--if-revision",
            "1",
            "--capability-file",
            &capability(&state_dir, "alpha"),
            "--idempotency-key",
            "round2-renew-0001",
            "--format",
            "json",
        ],
    );
    assert_eq!(
        renewed.code,
        0,
        "stdout={} stderr={}",
        renewed.stdout_text(),
        renewed.stderr_text()
    );
    assert_eq!(data(&renewed)["revision"], 2);
}

#[test]
fn coordination_review_round2_send_rejects_recipient_after_capability_revocation() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    fs::create_dir(&state_dir).expect("state");
    seed_brokers(
        &state_dir,
        &[
            (
                "alpha",
                "incarnation-alpha",
                "alpha-private-capability-material",
            ),
            (
                "beta",
                "incarnation-beta",
                "beta-private-capability-material",
            ),
        ],
    );
    fs::remove_file(capability(&state_dir, "beta")).expect("revoke recipient capability");
    let body = tmp.path().join("body.txt");
    fs::write(&body, "must remain unsent").expect("body");
    let sent = run(
        tmp.path(),
        &[
            "--state-dir",
            state_dir.to_str().expect("state"),
            "message",
            "send",
            "--from",
            "alpha",
            "--to",
            "beta",
            "--body-file",
            body.to_str().expect("body"),
            "--capability-file",
            &capability(&state_dir, "alpha"),
            "--idempotency-key",
            "round2-revoked-recipient-0001",
            "--format",
            "json",
        ],
    );
    assert_ne!(sent.code, 0);
    assert_eq!(
        sent.stdout_json()["error"]["code"],
        "coordination-unavailable"
    );
}

#[test]
fn coordination_review_round2_unknown_fingerprint_epoch_is_not_a_definite_conflict() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    fs::create_dir(&state_dir).expect("state");
    seed_brokers(
        &state_dir,
        &[
            (
                "alpha",
                "incarnation-alpha",
                "alpha-private-capability-material",
            ),
            (
                "beta",
                "incarnation-beta",
                "beta-private-capability-material",
            ),
        ],
    );
    let fingerprint = format!("hmac-sha256:999:{}", "a".repeat(64));
    let write_candidate = |path: &Path, summary: &str| {
        fs::write(
            path,
            serde_json::to_vec_pretty(&json!({
                "schema_version": "agent-session.work-context-input.v1",
                "intent": "implementation",
                "tier": "L2",
                "repositories": [],
                "worktrees": [fingerprint],
                "provider_refs": [],
                "plan_refs": [],
                "scopes": [],
                "summary": summary
            }))
            .expect("candidate"),
        )
        .expect("write candidate");
    };
    let alpha_file = tmp.path().join("alpha.json");
    let beta_file = tmp.path().join("beta.json");
    write_candidate(&alpha_file, "alpha unknown epoch");
    write_candidate(&beta_file, "beta unknown epoch");
    for (id, file, key) in [
        ("alpha", &alpha_file, "round2-epoch-alpha-0001"),
        ("beta", &beta_file, "round2-epoch-beta-0001"),
    ] {
        let claimed = run(
            tmp.path(),
            &[
                "--state-dir",
                state_dir.to_str().expect("state"),
                "work-context",
                "claim",
                "--session",
                id,
                "--file",
                file.to_str().expect("candidate"),
                "--capability-file",
                &capability(&state_dir, id),
                "--idempotency-key",
                key,
                "--format",
                "json",
            ],
        );
        assert_eq!(
            claimed.code,
            0,
            "id={id} stdout={} stderr={}",
            claimed.stdout_text(),
            claimed.stderr_text()
        );
        assert_ne!(data(&claimed)["evaluation"]["classification"], "conflict");
    }
}

#[test]
fn coordination_review_round2_cli_declares_reply_cas_and_file_backed_execution_tokens() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let reply = run(tmp.path(), &["message", "reply", "--help"]);
    assert_eq!(reply.code, 0);
    assert!(reply.stdout_text().contains("--if-revision"));

    for leaf in ["admit", "complete"] {
        let help = run(tmp.path(), &["work-context", leaf, "--help"]);
        assert_eq!(help.code, 0);
        assert!(help.stdout_text().contains("--execution-token-file"));
        assert!(!help.stdout_text().contains("--execution-token <"));
    }
}

#[test]
fn coordination_review_round2_parse_errors_keep_exact_leaf_envelope_identity() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let output = run(
        tmp.path(),
        &[
            "message",
            "inbox",
            "--format",
            "json",
            "--unknown-review-flag",
        ],
    );
    assert_ne!(output.code, 0);
    assert_eq!(
        output.stdout_json()["schema_version"],
        "cli.agent-session.message-inbox.v1"
    );
}

#[test]
fn coordination_review_round2_reply_revalidates_parent_revision_in_final_transaction() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    fs::create_dir(&state_dir).expect("state");
    seed_brokers(
        &state_dir,
        &[
            (
                "alpha",
                "incarnation-alpha",
                "alpha-private-capability-material",
            ),
            (
                "beta",
                "incarnation-beta",
                "beta-private-capability-material",
            ),
        ],
    );
    let body = tmp.path().join("body.txt");
    fs::write(&body, "original").expect("body");
    let sent = run(
        tmp.path(),
        &[
            "--state-dir",
            state_dir.to_str().expect("state"),
            "message",
            "send",
            "--from",
            "alpha",
            "--to",
            "beta",
            "--body-file",
            body.to_str().expect("body"),
            "--capability-file",
            &capability(&state_dir, "alpha"),
            "--idempotency-key",
            "round2-reply-parent-0001",
            "--format",
            "json",
        ],
    );
    assert_eq!(sent.code, 0, "stderr={}", sent.stderr_text());
    let message_id = data(&sent)["message_id"]
        .as_str()
        .expect("message id")
        .to_string();
    let reply_body = tmp.path().join("reply.txt");
    fs::write(&reply_body, "reply").expect("reply");
    let replied = run(
        tmp.path(),
        &[
            "--state-dir",
            state_dir.to_str().expect("state"),
            "message",
            "reply",
            "--session",
            "beta",
            "--message",
            &message_id,
            "--if-revision",
            "2",
            "--body-file",
            reply_body.to_str().expect("reply"),
            "--capability-file",
            &capability(&state_dir, "beta"),
            "--idempotency-key",
            "round2-reply-cas-0001",
            "--format",
            "json",
        ],
    );
    assert_ne!(replied.code, 0);
    assert_eq!(
        replied.stdout_json()["error"]["code"],
        "message-revision-conflict"
    );
}

#[test]
fn coordination_review_round3_frozen_v1_scope_grammar_and_limits_are_exact() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    fs::create_dir(&state_dir).expect("state");
    seed_brokers(
        &state_dir,
        &[
            ("alpha", "inc-alpha", "alpha-private-capability-material"),
            ("beta", "inc-beta", "beta-private-capability-material"),
            ("gamma", "inc-gamma", "gamma-private-capability-material"),
        ],
    );
    let write_context = |path: &Path, repositories: Vec<String>, scopes: serde_json::Value| {
        fs::write(
            path,
            serde_json::to_vec_pretty(&json!({
                "schema_version": "agent-session.work-context-input.v1",
                "intent": "implementation",
                "tier": "L2",
                "repositories": repositories,
                "worktrees": [],
                "provider_refs": [],
                "plan_refs": [],
                "scopes": scopes,
                "summary": "round three frozen contract"
            }))
            .expect("context json"),
        )
        .expect("write context");
    };
    let capability_scope = tmp.path().join("capability.json");
    write_context(
        &capability_scope,
        vec!["example/repository".to_string()],
        json!([{"kind":"capability","repository":"example/repository","value":"deploy"}]),
    );
    let too_many = tmp.path().join("too-many.json");
    write_context(
        &too_many,
        (0..9)
            .map(|index| format!("example/repository-{index}"))
            .collect(),
        json!([]),
    );
    let glob = tmp.path().join("glob.json");
    write_context(
        &glob,
        vec!["example/repository".to_string()],
        json!([{"kind":"path-exact","repository":"example/repository","value":"src/*.rs"}]),
    );
    let attempt = |session: &str, file: &Path, key: &str| {
        run(
            tmp.path(),
            &[
                "--state-dir",
                state_dir.to_str().expect("state"),
                "work-context",
                "claim",
                "--session",
                session,
                "--file",
                file.to_str().expect("context"),
                "--capability-file",
                &capability(&state_dir, session),
                "--idempotency-key",
                key,
                "--format",
                "json",
            ],
        )
    };
    let results = [
        attempt("alpha", &capability_scope, "round3-scope-capability-0001"),
        attempt("beta", &too_many, "round3-scope-limits-0001"),
        attempt("gamma", &glob, "round3-scope-glob-0001"),
    ];
    assert!(results.iter().all(|output| output.code != 0));
    assert_eq!(
        results[0].stdout_json()["error"]["code"],
        "invalid-work-context"
    );
    assert_eq!(
        results[1].stdout_json()["error"]["code"],
        "invalid-work-context"
    );
    assert_eq!(results[2].stdout_json()["error"]["code"], "invalid-scope");
}

#[test]
fn coordination_review_round3_public_check_selectors_do_not_suppress_candidates() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    fs::create_dir(&state_dir).expect("state");
    seed_brokers(
        &state_dir,
        &[("alpha", "inc-alpha", "alpha-private-capability-material")],
    );
    let context_file = tmp.path().join("context.json");
    candidate(&context_file, "src", "public context");
    let claimed = run(
        tmp.path(),
        &[
            "--state-dir",
            state_dir.to_str().expect("state"),
            "work-context",
            "claim",
            "--session",
            "alpha",
            "--file",
            context_file.to_str().expect("context"),
            "--capability-file",
            &capability(&state_dir, "alpha"),
            "--idempotency-key",
            "round3-public-claim-0001",
            "--format",
            "json",
        ],
    );
    assert_eq!(claimed.code, 0, "stderr={}", claimed.stderr_text());
    let shown = run(
        tmp.path(),
        &[
            "--state-dir",
            state_dir.to_str().expect("state"),
            "work-context",
            "show",
            "--session",
            "alpha",
            "--format",
            "json",
        ],
    );
    let candidate_check = run(
        tmp.path(),
        &[
            "--state-dir",
            state_dir.to_str().expect("state"),
            "work-context",
            "check",
            "--candidate",
            context_file.to_str().expect("context"),
            "--format",
            "json",
        ],
    );
    let selected_check = run(
        tmp.path(),
        &[
            "--state-dir",
            state_dir.to_str().expect("state"),
            "work-context",
            "check",
            "--session",
            "alpha",
            "--format",
            "json",
        ],
    );
    assert_eq!(shown.code, 0, "stderr={}", shown.stderr_text());
    assert_eq!(
        candidate_check.code,
        0,
        "stderr={}",
        candidate_check.stderr_text()
    );
    assert_eq!(
        data(&candidate_check)["classification"],
        "conflict",
        "candidate must compare against every persisted record"
    );
    assert_eq!(
        selected_check.code,
        0,
        "stderr={}",
        selected_check.stderr_text()
    );
    assert_eq!(data(&selected_check)["classification"], "clear");
}

#[test]
fn coordination_review_round3_idempotency_keys_are_principal_scoped() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    fs::create_dir(&state_dir).expect("state");
    seed_brokers(
        &state_dir,
        &[
            ("alpha", "inc-alpha", "alpha-private-capability-material"),
            ("beta", "inc-beta", "beta-private-capability-material"),
        ],
    );
    rewrite_registry(&state_dir, |_| {});
    let beta_record_path = state_dir.join("sessions/beta/session.json");
    let mut beta_record: serde_json::Value =
        serde_json::from_slice(&fs::read(&beta_record_path).expect("beta record")).expect("json");
    beta_record["cwd"] = json!("/fixture/repository-beta");
    fs::write(
        &beta_record_path,
        serde_json::to_vec_pretty(&beta_record).expect("json"),
    )
    .expect("write beta");
    fs::set_permissions(&beta_record_path, fs::Permissions::from_mode(0o600)).expect("mode");
    let write_context = |path: &Path, repository: &str| {
        fs::write(
            path,
            serde_json::to_vec_pretty(&json!({
                "schema_version": "agent-session.work-context-input.v1",
                "intent": "implementation",
                "tier": "L2",
                "repositories": [repository],
                "worktrees": [],
                "provider_refs": [],
                "plan_refs": [],
                "scopes": [{"kind":"path-prefix","repository":repository,"value":"src"}],
                "summary": repository
            }))
            .expect("context"),
        )
        .expect("write context");
    };
    let alpha_file = tmp.path().join("alpha.json");
    let beta_file = tmp.path().join("beta.json");
    write_context(&alpha_file, "example/alpha");
    write_context(&beta_file, "example/beta");
    for (session, file) in [("alpha", alpha_file), ("beta", beta_file)] {
        let output = run(
            tmp.path(),
            &[
                "--state-dir",
                state_dir.to_str().expect("state"),
                "work-context",
                "claim",
                "--session",
                session,
                "--file",
                file.to_str().expect("context"),
                "--capability-file",
                &capability(&state_dir, session),
                "--idempotency-key",
                "round3-shared-idempotency-key",
                "--format",
                "json",
            ],
        );
        assert_eq!(
            output.code,
            0,
            "session={session} stderr={}",
            output.stderr_text()
        );
    }
}

#[test]
fn coordination_review_round3_reply_binding_and_revision_are_in_the_receipt() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    fs::create_dir(&state_dir).expect("state");
    seed_brokers(
        &state_dir,
        &[
            ("alpha", "inc-alpha", "alpha-private-capability-material"),
            ("beta", "inc-beta", "beta-private-capability-material"),
            ("gamma", "inc-gamma", "gamma-private-capability-material"),
        ],
    );
    let body = tmp.path().join("body.txt");
    fs::write(&body, "body").expect("body");
    let sent = run(
        tmp.path(),
        &[
            "--state-dir",
            state_dir.to_str().expect("state"),
            "message",
            "send",
            "--from",
            "alpha",
            "--to",
            "beta",
            "--body-file",
            body.to_str().expect("body"),
            "--capability-file",
            &capability(&state_dir, "alpha"),
            "--idempotency-key",
            "round3-parent-send-0001",
            "--format",
            "json",
        ],
    );
    assert_eq!(sent.code, 0, "stderr={}", sent.stderr_text());
    let parent = data(&sent)["message_id"].as_str().expect("id").to_string();
    let forged = run(
        tmp.path(),
        &[
            "--state-dir",
            state_dir.to_str().expect("state"),
            "message",
            "send",
            "--from",
            "beta",
            "--to",
            "gamma",
            "--reply-to",
            &parent,
            "--body-file",
            body.to_str().expect("body"),
            "--capability-file",
            &capability(&state_dir, "beta"),
            "--idempotency-key",
            "round3-forged-reply-0001",
            "--format",
            "json",
        ],
    );
    assert_ne!(forged.code, 0);
    let first_reply = run(
        tmp.path(),
        &[
            "--state-dir",
            state_dir.to_str().expect("state"),
            "message",
            "reply",
            "--session",
            "beta",
            "--message",
            &parent,
            "--if-revision",
            "1",
            "--body-file",
            body.to_str().expect("body"),
            "--capability-file",
            &capability(&state_dir, "beta"),
            "--idempotency-key",
            "round3-reply-cas-0001",
            "--format",
            "json",
        ],
    );
    assert_eq!(first_reply.code, 0, "stderr={}", first_reply.stderr_text());
    let changed_revision = run(
        tmp.path(),
        &[
            "--state-dir",
            state_dir.to_str().expect("state"),
            "message",
            "reply",
            "--session",
            "beta",
            "--message",
            &parent,
            "--if-revision",
            "2",
            "--body-file",
            body.to_str().expect("body"),
            "--capability-file",
            &capability(&state_dir, "beta"),
            "--idempotency-key",
            "round3-reply-cas-0001",
            "--format",
            "json",
        ],
    );
    assert_ne!(changed_revision.code, 0);
    assert_eq!(
        changed_revision.stdout_json()["error"]["code"],
        "idempotency-key-reused"
    );
    rewrite_registry(&state_dir, |registry| {
        registry["messages"]
            .as_array_mut()
            .expect("messages")
            .retain(|message| message["message_id"] != parent);
    });
    let replayed = run(
        tmp.path(),
        &[
            "--state-dir",
            state_dir.to_str().expect("state"),
            "message",
            "reply",
            "--session",
            "beta",
            "--message",
            &parent,
            "--if-revision",
            "1",
            "--body-file",
            body.to_str().expect("body"),
            "--capability-file",
            &capability(&state_dir, "beta"),
            "--idempotency-key",
            "round3-reply-cas-0001",
            "--format",
            "json",
        ],
    );
    assert_eq!(replayed.code, 0, "stderr={}", replayed.stderr_text());
    assert_eq!(
        data(&replayed)["message_id"],
        data(&first_reply)["message_id"]
    );
}

#[test]
fn coordination_review_round3_mailbox_burst_and_cursor_are_bounded() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    fs::create_dir(&state_dir).expect("state");
    seed_brokers(
        &state_dir,
        &[
            ("alpha", "inc-alpha", "alpha-private-capability-material"),
            ("beta", "inc-beta", "beta-private-capability-material"),
        ],
    );
    let body = tmp.path().join("body.txt");
    fs::write(&body, "body").expect("body");
    let mut sent_ids = Vec::new();
    let mut eleventh = None;
    for index in 0..11 {
        let key = format!("round3-burst-{index:04}");
        let output = run(
            tmp.path(),
            &[
                "--state-dir",
                state_dir.to_str().expect("state"),
                "message",
                "send",
                "--from",
                "alpha",
                "--to",
                "beta",
                "--body-file",
                body.to_str().expect("body"),
                "--capability-file",
                &capability(&state_dir, "alpha"),
                "--idempotency-key",
                &key,
                "--format",
                "json",
            ],
        );
        if index < 10 {
            assert_eq!(
                output.code,
                0,
                "index={index} stderr={}",
                output.stderr_text()
            );
            sent_ids.push(
                data(&output)["message_id"]
                    .as_str()
                    .expect("id")
                    .to_string(),
            );
        } else {
            eleventh = Some(output);
        }
    }
    let eleventh = eleventh.expect("eleventh");
    assert_ne!(eleventh.code, 0);
    assert_eq!(eleventh.stdout_json()["error"]["code"], "rate-limited");
    let inbox = run(
        tmp.path(),
        &[
            "--state-dir",
            state_dir.to_str().expect("state"),
            "message",
            "inbox",
            "--session",
            "beta",
            "--capability-file",
            &capability(&state_dir, "beta"),
            "--limit",
            "1",
            "--format",
            "json",
        ],
    );
    assert_eq!(inbox.code, 0, "stderr={}", inbox.stderr_text());
    let inbox_data = data(&inbox);
    let cursor = inbox_data["next_cursor"].as_str().expect("cursor");
    assert!(!sent_ids.iter().any(|message_id| message_id == cursor));
}

#[test]
fn coordination_review_round4_completion_can_close_an_uncertain_lease() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    fs::create_dir(&state_dir).expect("state");
    seed_brokers(
        &state_dir,
        &[("alpha", "inc-alpha", "alpha-private-capability-material")],
    );
    let execution_token = tmp.path().join("execution-token");
    fs::write(&execution_token, "round-four-execution-token").expect("token");
    fs::set_permissions(&execution_token, fs::Permissions::from_mode(0o600)).expect("token mode");
    let registry_path = state_dir.join("coordination/registry.json");
    let mut registry: serde_json::Value =
        serde_json::from_slice(&fs::read(&registry_path).expect("registry")).expect("json");
    registry["operations"]
        .as_array_mut()
        .expect("operations")
        .push(json!({
            "schema_version": "agent-session.operation-lease.v1",
            "lease_id": "round-four-lease",
            "session_id": "alpha",
            "session_incarnation": "inc-alpha",
            "claim_id": "round-four-claim",
            "claim_revision": 1,
            "operation": "edit",
            "targets": [{"kind":"path-exact","repository":"example/repository","value":"src/lib.rs"}],
            "state": "completing",
            "revision": 2,
            "started_at": "2030-01-01T00:00:00Z",
            "expires_at": "2030-01-01T00:30:00Z",
            "expires_at_epoch": i64::MAX,
            "execution_token_digest": digest("round-four-execution-token"),
            "activity_revision": 1,
            "runtime_identity_digest": "runtime"
        }));
    fs::write(
        &registry_path,
        serde_json::to_vec_pretty(&registry).expect("registry json"),
    )
    .expect("write registry");
    fs::set_permissions(&registry_path, fs::Permissions::from_mode(0o600)).expect("registry mode");

    let completed = run(
        tmp.path(),
        &[
            "--state-dir",
            state_dir.to_str().expect("state"),
            "work-context",
            "complete",
            "--session",
            "alpha",
            "--lease",
            "round-four-lease",
            "--if-revision",
            "2",
            "--execution-token-file",
            execution_token.to_str().expect("token"),
            "--outcome",
            "pass",
            "--capability-file",
            &capability(&state_dir, "alpha"),
            "--idempotency-key",
            "round4-complete-0001",
            "--format",
            "json",
        ],
    );
    assert_eq!(completed.code, 0, "stderr={}", completed.stderr_text());
    assert_eq!(data(&completed)["state"], "completed");
}

#[test]
fn main_agent_init_rehydrate_and_checkpoint_are_private_revision_fenced_and_idempotent() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let checkout = tmp.path().join("checkout");
    fs::create_dir(&state_dir).expect("state");
    init_checkout(&checkout, "https://example.invalid/example/repository.git");
    seed_brokers_at(
        &state_dir,
        &[(
            "main-one",
            "main-incarnation-one",
            "main-private-capability-material-0000000001",
            checkout.as_path(),
            Some("enforce"),
        )],
    );
    let capability_file = capability(&state_dir, "main-one");
    let state_arg = state_dir.to_string_lossy().into_owned();
    let objective_path = tmp.path().join("objective.json");
    let privacy_canary = "private-objective-canary-must-not-project";
    fs::write(
        &objective_path,
        serde_json::to_vec_pretty(&json!({
            "schema_version": "main-agent.objective-packet.v1",
            "run_id": "run-one",
            "tier": "L0",
            "objective_summary": "Deliver durable orchestration",
            "objective": { "private_note": privacy_canary },
            "done_criteria": ["Focused acceptance passes"],
            "constraints": ["Do not leak private packets"],
            "durable_refs": ["local:contract"],
            "work_context": {
                "schema_version": "agent-session.work-context-input.v1",
                "intent": "implementation",
                "tier": "L0",
                "repositories": ["example/repository"],
                "worktrees": [],
                "provider_refs": [],
                "plan_refs": [],
                "scopes": [{
                    "kind": "path-prefix",
                    "repository": "example/repository",
                    "value": "crates/agent-session"
                }],
                "summary": "Deliver durable orchestration"
            },
            "next_action": "Record the first checkpoint"
        }))
        .expect("objective json"),
    )
    .expect("objective packet");
    fs::set_permissions(&objective_path, fs::Permissions::from_mode(0o600))
        .expect("objective mode");

    let init = run_main_agent(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "--host",
            "sympoies",
            "init",
            "--packet-file",
            objective_path.to_str().expect("objective path"),
            "--if-absent",
            "--idempotency-key",
            "main-init-0001",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_CAPABILITY_FILE", &capability_file)],
    );
    assert_eq!(init.code, 0, "stderr={}", init.stderr_text());
    assert_eq!(data(&init)["run"]["run_id"], "run-one");
    let init_coordination = load_coordination_registry(&state_dir);
    let init_controller_claim = init_coordination["claims"]
        .as_array()
        .expect("claims")
        .iter()
        .find(|claim| claim["session_id"] == "main-one" && claim["state"] == "active")
        .expect("controller claim");
    assert!(
        !init_controller_claim["checkout_shell_grant"]
            .as_bool()
            .unwrap_or(false),
        "Main Agent controller init must not mint a worker checkout-shell grant"
    );
    assert_eq!(data(&init)["run"]["revision"], 1);

    let replay = run_main_agent(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "init",
            "--packet-file",
            objective_path.to_str().expect("objective path"),
            "--if-absent",
            "--idempotency-key",
            "main-init-0001",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_CAPABILITY_FILE", &capability_file)],
    );
    assert_eq!(replay.code, 0, "stderr={}", replay.stderr_text());
    assert_eq!(data(&replay), data(&init));

    let status = run_main_agent(
        tmp.path(),
        &["--state-dir", &state_arg, "status", "--format", "json"],
        &[("AGENT_SESSION_CAPABILITY_FILE", &capability_file)],
    );
    assert_eq!(status.code, 0, "stderr={}", status.stderr_text());
    assert!(!status.stdout_text().contains(privacy_canary));
    assert!(
        status
            .stdout_text()
            .contains("Deliver durable orchestration")
    );

    let first_rehydrate = run_main_agent(
        tmp.path(),
        &["--state-dir", &state_arg, "rehydrate", "--format", "json"],
        &[("AGENT_SESSION_CAPABILITY_FILE", &capability_file)],
    );
    let second_rehydrate = run_main_agent(
        tmp.path(),
        &["--state-dir", &state_arg, "rehydrate", "--format", "json"],
        &[("AGENT_SESSION_CAPABILITY_FILE", &capability_file)],
    );
    assert_eq!(
        first_rehydrate.code,
        0,
        "stderr={}",
        first_rehydrate.stderr_text()
    );
    assert_eq!(
        second_rehydrate.code,
        0,
        "stderr={}",
        second_rehydrate.stderr_text()
    );
    assert_eq!(
        data(&first_rehydrate)["durable"],
        data(&second_rehydrate)["durable"]
    );
    assert!(first_rehydrate.stdout_text().contains(privacy_canary));

    let checkpoint_path = tmp.path().join("checkpoint.json");
    fs::write(
        &checkpoint_path,
        serde_json::to_vec_pretty(&json!({
            "schema_version": "main-agent.checkpoint-input.v1",
            "summary": "CLI contract implemented",
            "next_action": "Run focused acceptance",
            "state": "active"
        }))
        .expect("checkpoint json"),
    )
    .expect("checkpoint packet");
    fs::set_permissions(&checkpoint_path, fs::Permissions::from_mode(0o600))
        .expect("checkpoint mode");
    let checkpoint = run_main_agent(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "checkpoint",
            "--file",
            checkpoint_path.to_str().expect("checkpoint path"),
            "--if-revision",
            "1",
            "--idempotency-key",
            "main-checkpoint-0001",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_CAPABILITY_FILE", &capability_file)],
    );
    assert_eq!(checkpoint.code, 0, "stderr={}", checkpoint.stderr_text());
    assert_eq!(data(&checkpoint)["run"]["revision"], 2);

    let stale = run_main_agent(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "checkpoint",
            "--file",
            checkpoint_path.to_str().expect("checkpoint path"),
            "--if-revision",
            "1",
            "--idempotency-key",
            "main-checkpoint-0002",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_CAPABILITY_FILE", &capability_file)],
    );
    assert_eq!(stale.code, 65);
    assert_eq!(
        stale.stdout_json()["error"]["code"],
        "orchestration-revision-conflict"
    );
    assert_eq!(
        stale.stdout_json()["error"]["details"]["current_revision"],
        2
    );

    let orchestration_root = state_dir.join("orchestration");
    assert_eq!(
        fs::metadata(&orchestration_root)
            .expect("root metadata")
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(orchestration_root.join("registry.json"))
            .expect("registry metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    for entry in fs::read_dir(orchestration_root.join("packets")).expect("packet directory") {
        assert_eq!(
            entry
                .expect("packet entry")
                .metadata()
                .expect("packet metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    let new_incarnation = "main-incarnation-two";
    let new_capability = "main-private-capability-material-0000000002";
    let session_path = state_dir.join("sessions/main-one/session.json");
    let mut session: serde_json::Value =
        serde_json::from_slice(&fs::read(&session_path).expect("session record"))
            .expect("session json");
    session["runtime"]["launch_id"] = json!(new_incarnation);
    fs::write(
        &session_path,
        serde_json::to_vec_pretty(&session).expect("session json"),
    )
    .expect("updated session record");
    fs::set_permissions(&session_path, fs::Permissions::from_mode(0o600)).expect("session mode");
    let coordination_dir = state_dir.join("sessions/main-one/coordination");
    let new_capability_file =
        coordination_dir.join(format!("capability-{}", digest(new_incarnation)));
    fs::write(&new_capability_file, new_capability).expect("new capability");
    fs::set_permissions(&new_capability_file, fs::Permissions::from_mode(0o600))
        .expect("new capability mode");
    let new_checkpoint_file = coordination_dir.join(format!(
        "main-agent-checkpoint-{}.json",
        digest(new_incarnation)
    ));
    fs::write(&new_checkpoint_file, []).expect("new checkpoint");
    fs::set_permissions(&new_checkpoint_file, fs::Permissions::from_mode(0o600))
        .expect("new checkpoint mode");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs();
    fs::write(
        coordination_dir.join("heartbeat"),
        format!("{new_incarnation}:{now}\n"),
    )
    .expect("new heartbeat");
    rewrite_registry(&state_dir, |registry| {
        registry["brokers"]["main-one"]["incarnation"] = json!(new_incarnation);
        registry["brokers"]["main-one"]["capability_digest"] = json!(digest(new_capability));
        registry["brokers"]["main-one"]["heartbeat_epoch"] = json!(now);
        for claim in registry["claims"].as_array_mut().expect("claims") {
            claim["state"] = json!("released");
        }
    });
    let new_capability_arg = new_capability_file.to_string_lossy().into_owned();

    let unfenced_rebind = run_main_agent(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "init",
            "--packet-file",
            objective_path.to_str().expect("objective path"),
            "--if-absent",
            "--idempotency-key",
            "main-rebind-0001",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_CAPABILITY_FILE", &new_capability_arg)],
    );
    assert_eq!(unfenced_rebind.code, 65);
    assert_eq!(
        unfenced_rebind.stdout_json()["error"]["code"],
        "orchestration-revision-required"
    );

    let rebound = run_main_agent(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "init",
            "--packet-file",
            objective_path.to_str().expect("objective path"),
            "--if-absent",
            "--if-revision",
            "2",
            "--idempotency-key",
            "main-rebind-0002",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_CAPABILITY_FILE", &new_capability_arg)],
    );
    assert_eq!(rebound.code, 0, "stderr={}", rebound.stderr_text());
    assert_eq!(data(&rebound)["rebound"], true);
    assert_eq!(data(&rebound)["run"]["revision"], 3);
    assert_eq!(
        data(&rebound)["run"]["controller"]["session_incarnation"],
        new_incarnation
    );
    let rebound_coordination = load_coordination_registry(&state_dir);
    let rebound_controller_claim = rebound_coordination["claims"]
        .as_array()
        .expect("claims")
        .iter()
        .find(|claim| {
            claim["session_id"] == "main-one"
                && claim["session_incarnation"] == new_incarnation
                && claim["state"] == "active"
        })
        .expect("rebound controller claim");
    assert!(
        !rebound_controller_claim["checkout_shell_grant"]
            .as_bool()
            .unwrap_or(false),
        "Main Agent controller rebind must not mint a worker checkout-shell grant"
    );
}

#[test]
fn main_agent_worker_start_rejects_untrusted_or_unverifiable_codex_checkout_before_durable_side_effects()
 {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let checkout = tmp.path().join("checkout");
    fs::create_dir(&state_dir).expect("state");
    init_checkout(&checkout, "https://example.invalid/example/repository.git");
    seed_brokers_at(
        &state_dir,
        &[(
            "main-one",
            "main-incarnation-one",
            "main-private-capability-material-0000000001",
            checkout.as_path(),
            Some("enforce"),
        )],
    );
    let main_capability = init_main_run(tmp.path(), &state_dir, &checkout, "main-one", "run-one");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let codex_bin = fake_agent(tmp.path(), "codex-worker");
    let codex_home = tmp.path().join("codex-home");
    write_trusted_codex_config(&codex_home, &[tmp.path()]);
    let codex_config = codex_home.join("config.toml");

    let assignment_path = tmp.path().join("assignment-untrusted-codex.json");
    write_private_json(
        &assignment_path,
        &json!({
            "schema_version": "main-agent.assignment-input.v1",
            "assignment_id": "assignment-untrusted-codex",
            "task_summary": "Refuse an untrusted Codex checkout before launch",
            "task": {},
            "launch": {
                "agent": "codex",
                "cwd": checkout,
                "title": null,
                "session_id": "worker-untrusted-codex",
                "coordination_mode": "enforce",
                "agent_args": []
            },
            "repository": "example/repository",
            "worktree": checkout,
            "base_ref": "main",
            "scopes": ["crates/agent-session"],
            "durable_refs": []
        }),
    );
    let state_arg = state_dir.to_string_lossy().into_owned();
    let tmux_arg = tmux_bin.to_string_lossy().into_owned();
    let tmux_log_arg = tmux_log.to_string_lossy().into_owned();
    let codex_arg = codex_bin.to_string_lossy().into_owned();
    let codex_home_arg = codex_home.to_string_lossy().into_owned();
    let refused = run_main_agent(
        &checkout,
        &[
            "--state-dir",
            &state_arg,
            "worker",
            "start",
            "--assignment-file",
            assignment_path.to_str().expect("assignment path"),
            "--await-ready",
            "0",
            "--idempotency-key",
            "worker-start-untrusted-codex-0001",
            "--format",
            "json",
        ],
        &[
            ("AGENT_SESSION_CAPABILITY_FILE", main_capability.as_str()),
            ("AGENT_SESSION_TMUX_BIN", tmux_arg.as_str()),
            ("AGENT_SESSION_CODEX_BIN", codex_arg.as_str()),
            ("AGENT_SESSION_FAKE_TMUX_LOG", tmux_log_arg.as_str()),
            ("CODEX_HOME", codex_home_arg.as_str()),
        ],
    );

    assert_eq!(refused.code, 65, "outcome={}", refused.stdout_text());
    assert_eq!(
        refused.stdout_json()["error"]["code"],
        "provider-trust-required"
    );
    assert_eq!(
        refused.stdout_json()["error"]["details"]["next_action"],
        "review-and-trust-project"
    );
    assert!(
        orchestration_registry(&state_dir)["assignments"]
            .get("assignment-untrusted-codex")
            .is_none(),
        "trust refusal must not persist an assignment"
    );
    assert!(
        !state_dir.join("sessions/worker-untrusted-codex").exists(),
        "trust refusal must not create a worker session"
    );
    assert!(
        tmux_calls(&tmux_log).is_empty(),
        "trust refusal must happen before tmux launch"
    );

    fs::write(&codex_config, "[projects.").expect("malformed Codex config");
    let unverifiable = run_main_agent(
        &checkout,
        &[
            "--state-dir",
            &state_arg,
            "worker",
            "start",
            "--assignment-file",
            assignment_path.to_str().expect("assignment path"),
            "--await-ready",
            "0",
            "--idempotency-key",
            "worker-start-untrusted-codex-0002",
            "--format",
            "json",
        ],
        &[
            ("AGENT_SESSION_CAPABILITY_FILE", main_capability.as_str()),
            ("AGENT_SESSION_TMUX_BIN", tmux_arg.as_str()),
            ("AGENT_SESSION_CODEX_BIN", codex_arg.as_str()),
            ("AGENT_SESSION_FAKE_TMUX_LOG", tmux_log_arg.as_str()),
            ("CODEX_HOME", codex_home_arg.as_str()),
        ],
    );
    assert_eq!(
        unverifiable.code,
        65,
        "outcome={}",
        unverifiable.stdout_text()
    );
    assert_eq!(
        unverifiable.stdout_json()["error"]["code"],
        "provider-trust-unverified"
    );
    assert_eq!(
        unverifiable.stdout_json()["error"]["details"]["next_action"],
        "repair-provider-config"
    );
    assert!(
        orchestration_registry(&state_dir)["assignments"]
            .get("assignment-untrusted-codex")
            .is_none(),
        "unverifiable trust must not persist an assignment"
    );
    assert!(
        !state_dir.join("sessions/worker-untrusted-codex").exists(),
        "unverifiable trust must not create a worker session"
    );
    assert!(
        tmux_calls(&tmux_log).is_empty(),
        "unverifiable trust must happen before tmux launch"
    );

    fs::write(&codex_config, format!("#{}\n", "x".repeat(1024 * 1024 + 1)))
        .expect("oversized Codex config");
    let oversized = run_main_agent(
        &checkout,
        &[
            "--state-dir",
            &state_arg,
            "worker",
            "start",
            "--assignment-file",
            assignment_path.to_str().expect("assignment path"),
            "--await-ready",
            "0",
            "--idempotency-key",
            "worker-start-untrusted-codex-0003",
            "--format",
            "json",
        ],
        &[
            ("AGENT_SESSION_CAPABILITY_FILE", main_capability.as_str()),
            ("AGENT_SESSION_TMUX_BIN", tmux_arg.as_str()),
            ("AGENT_SESSION_CODEX_BIN", codex_arg.as_str()),
            ("AGENT_SESSION_FAKE_TMUX_LOG", tmux_log_arg.as_str()),
            ("CODEX_HOME", codex_home_arg.as_str()),
        ],
    );
    assert_eq!(oversized.code, 65, "outcome={}", oversized.stdout_text());
    assert_eq!(
        oversized.stdout_json()["error"]["code"],
        "provider-trust-unverified"
    );
    assert!(
        orchestration_registry(&state_dir)["assignments"]
            .get("assignment-untrusted-codex")
            .is_none(),
        "oversized trust config must not persist an assignment"
    );
    assert!(
        tmux_calls(&tmux_log).is_empty(),
        "oversized trust config must happen before tmux launch"
    );

    fs::remove_file(&codex_config).expect("remove oversized Codex config");
    let fifo_path = std::ffi::CString::new(
        codex_config
            .to_str()
            .expect("test Codex config path must be UTF-8"),
    )
    .expect("FIFO path");
    assert_eq!(
        unsafe { libc::mkfifo(fifo_path.as_ptr(), 0o600) },
        0,
        "create Codex config FIFO"
    );
    let mut fifo_command = Command::new(bin::resolve("main-agent"));
    fifo_command
        .current_dir(&checkout)
        .args([
            "--state-dir",
            &state_arg,
            "worker",
            "start",
            "--assignment-file",
            assignment_path.to_str().expect("assignment path"),
            "--await-ready",
            "0",
            "--idempotency-key",
            "worker-start-untrusted-codex-0004",
            "--format",
            "json",
        ])
        .env("AGENT_SESSION_CAPABILITY_FILE", &main_capability)
        .env("AGENT_SESSION_TMUX_BIN", &tmux_bin)
        .env("AGENT_SESSION_CODEX_BIN", &codex_bin)
        .env("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log)
        .env("CODEX_HOME", &codex_home)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut fifo = fifo_command.spawn().expect("spawn FIFO preflight");
    let fifo_deadline = Instant::now() + Duration::from_secs(3);
    while fifo.try_wait().expect("poll FIFO preflight").is_none() {
        if Instant::now() >= fifo_deadline {
            fifo.kill().expect("kill blocked FIFO preflight");
            let _ = fifo.wait();
            panic!("Codex trust preflight blocked while opening a config.toml FIFO");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let fifo = fifo.wait_with_output().expect("FIFO preflight output");
    assert_eq!(
        fifo.status.code(),
        Some(65),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&fifo.stdout),
        String::from_utf8_lossy(&fifo.stderr)
    );
    let fifo_json: serde_json::Value =
        serde_json::from_slice(&fifo.stdout).expect("FIFO error json");
    assert_eq!(fifo_json["error"]["code"], "provider-trust-unverified");
    assert!(
        orchestration_registry(&state_dir)["assignments"]
            .get("assignment-untrusted-codex")
            .is_none(),
        "FIFO trust config must not persist an assignment"
    );
    assert!(
        tmux_calls(&tmux_log).is_empty(),
        "FIFO trust config must fail without blocking before tmux launch"
    );
    fs::remove_file(&codex_config).expect("remove Codex config FIFO");

    let linked_config_home = tmp.path().join("linked-config-home");
    write_trusted_codex_config(&linked_config_home, &[&checkout]);
    std::os::unix::fs::symlink(linked_config_home.join("config.toml"), &codex_config)
        .expect("linked Codex config");
    let linked_config = run_main_agent(
        &checkout,
        &[
            "--state-dir",
            &state_arg,
            "worker",
            "start",
            "--assignment-file",
            assignment_path.to_str().expect("assignment path"),
            "--await-ready",
            "0",
            "--idempotency-key",
            "worker-start-untrusted-codex-0005",
            "--format",
            "json",
        ],
        &[
            ("AGENT_SESSION_CAPABILITY_FILE", main_capability.as_str()),
            ("AGENT_SESSION_TMUX_BIN", tmux_arg.as_str()),
            ("AGENT_SESSION_CODEX_BIN", codex_arg.as_str()),
            ("AGENT_SESSION_FAKE_TMUX_LOG", tmux_log_arg.as_str()),
            ("CODEX_HOME", codex_home_arg.as_str()),
        ],
    );
    assert_eq!(
        linked_config.code,
        65,
        "outcome={}",
        linked_config.stdout_text()
    );
    assert_eq!(
        linked_config.stdout_json()["error"]["code"],
        "provider-trust-unverified"
    );
    assert!(
        orchestration_registry(&state_dir)["assignments"]
            .get("assignment-untrusted-codex")
            .is_none()
    );
    assert!(tmux_calls(&tmux_log).is_empty());
    fs::remove_file(&codex_config).expect("remove linked Codex config");

    write_trusted_codex_config(&codex_home, &[&checkout]);
    let linked_checkout = tmp.path().join("linked-checkout");
    std::os::unix::fs::symlink(&checkout, &linked_checkout).expect("linked checkout");
    let trusted_assignment_path = tmp.path().join("assignment-trusted-symlink.json");
    write_private_json(
        &trusted_assignment_path,
        &json!({
            "schema_version": "main-agent.assignment-input.v1",
            "assignment_id": "assignment-trusted-symlink",
            "task_summary": "Launch the exact canonical trusted checkout",
            "task": {},
            "launch": {
                "agent": "codex",
                "cwd": linked_checkout,
                "title": null,
                "session_id": "worker-trusted-symlink",
                "coordination_mode": "enforce",
                "agent_args": []
            },
            "repository": "example/repository",
            "worktree": linked_checkout,
            "base_ref": "main",
            "scopes": ["crates/agent-session"],
            "durable_refs": []
        }),
    );
    let trusted = run_main_agent(
        &checkout,
        &[
            "--state-dir",
            &state_arg,
            "worker",
            "start",
            "--assignment-file",
            trusted_assignment_path
                .to_str()
                .expect("trusted assignment path"),
            "--await-ready",
            "0",
            "--idempotency-key",
            "worker-start-trusted-symlink-0001",
            "--format",
            "json",
        ],
        &[
            ("AGENT_SESSION_CAPABILITY_FILE", main_capability.as_str()),
            ("AGENT_SESSION_TMUX_BIN", tmux_arg.as_str()),
            ("AGENT_SESSION_CODEX_BIN", codex_arg.as_str()),
            ("AGENT_SESSION_FAKE_TMUX_LOG", tmux_log_arg.as_str()),
            ("CODEX_HOME", codex_home_arg.as_str()),
        ],
    );
    assert_eq!(trusted.code, 0, "outcome={}", trusted.stdout_text());
    let worker: serde_json::Value = serde_json::from_slice(
        &fs::read(state_dir.join("sessions/worker-trusted-symlink/session.json"))
            .expect("trusted worker session"),
    )
    .expect("trusted worker session json");
    assert_eq!(
        worker["cwd"],
        fs::canonicalize(&checkout)
            .expect("canonical checkout")
            .to_string_lossy()
            .as_ref(),
        "the launched session must retain the exact canonical path that passed trust preflight"
    );
    let canonical_codex_home = fs::canonicalize(&codex_home).expect("canonical Codex home");
    assert_eq!(
        worker["runtime"]["agent_profile_provider_config_dir"],
        canonical_codex_home.to_string_lossy().as_ref(),
        "the session record must retain the verified provider configuration root without requiring a named profile"
    );
    assert!(
        tmux_calls(&tmux_log).iter().any(|call| {
            call.iter()
                .any(|arg| arg == &format!("CODEX_HOME={}", canonical_codex_home.to_string_lossy()))
        }),
        "the tmux child environment must receive the verified Codex configuration root"
    );
}

#[test]
fn main_agent_worker_start_rejects_missing_cwd_before_durable_side_effects() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let checkout = tmp.path().join("checkout");
    let missing_checkout = tmp.path().join("missing-checkout");
    fs::create_dir(&state_dir).expect("state");
    init_checkout(&checkout, "https://example.invalid/example/repository.git");
    seed_brokers_at(
        &state_dir,
        &[(
            "main-one",
            "main-incarnation-one",
            "main-private-capability-material-0000000001",
            checkout.as_path(),
            Some("enforce"),
        )],
    );
    let main_capability = init_main_run(tmp.path(), &state_dir, &checkout, "main-one", "run-one");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let codex_bin = fake_agent(tmp.path(), "codex-worker");
    let codex_home = tmp.path().join("codex-home");
    fs::create_dir(&codex_home).expect("Codex home");
    fs::write(codex_home.join("config.toml"), "[projects.").expect("malformed Codex config");
    let assignment_path = tmp.path().join("assignment-missing-cwd.json");
    write_private_json(
        &assignment_path,
        &json!({
            "schema_version": "main-agent.assignment-input.v1",
            "assignment_id": "assignment-missing-cwd",
            "task_summary": "Refuse a missing launch cwd before persistence",
            "task": {},
            "launch": {
                "agent": "codex",
                "cwd": missing_checkout,
                "title": null,
                "session_id": "worker-missing-cwd",
                "coordination_mode": "enforce",
                "agent_args": []
            },
            "repository": "example/repository",
            "worktree": missing_checkout,
            "base_ref": "main",
            "scopes": ["crates/agent-session"],
            "durable_refs": []
        }),
    );
    let state_arg = state_dir.to_string_lossy().into_owned();
    let tmux_arg = tmux_bin.to_string_lossy().into_owned();
    let tmux_log_arg = tmux_log.to_string_lossy().into_owned();
    let codex_arg = codex_bin.to_string_lossy().into_owned();
    let codex_home_arg = codex_home.to_string_lossy().into_owned();
    let refused = run_main_agent(
        &checkout,
        &[
            "--state-dir",
            &state_arg,
            "worker",
            "start",
            "--assignment-file",
            assignment_path.to_str().expect("assignment path"),
            "--await-ready",
            "0",
            "--idempotency-key",
            "worker-start-missing-cwd-0001",
            "--format",
            "json",
        ],
        &[
            ("AGENT_SESSION_CAPABILITY_FILE", main_capability.as_str()),
            ("AGENT_SESSION_TMUX_BIN", tmux_arg.as_str()),
            ("AGENT_SESSION_CODEX_BIN", codex_arg.as_str()),
            ("AGENT_SESSION_FAKE_TMUX_LOG", tmux_log_arg.as_str()),
            ("CODEX_HOME", codex_home_arg.as_str()),
        ],
    );

    assert_eq!(refused.code, 65, "outcome={}", refused.stdout_text());
    assert_eq!(
        refused.stdout_json()["error"]["code"],
        "assignment-launch-cwd-unavailable"
    );
    assert_eq!(
        refused.stdout_json()["error"]["details"]["next_action"],
        "create-managed-worktree"
    );
    assert!(
        orchestration_registry(&state_dir)["assignments"]
            .get("assignment-missing-cwd")
            .is_none(),
        "cwd refusal must not persist an assignment"
    );
    assert!(
        !state_dir.join("sessions/worker-missing-cwd").exists(),
        "cwd refusal must not create a worker session"
    );
    assert!(
        tmux_calls(&tmux_log).is_empty(),
        "cwd refusal must happen before tmux launch"
    );

    let file_checkout = tmp.path().join("file-checkout");
    fs::write(&file_checkout, "not a directory").expect("file checkout");
    let file_assignment_path = tmp.path().join("assignment-file-cwd.json");
    write_private_json(
        &file_assignment_path,
        &json!({
            "schema_version": "main-agent.assignment-input.v1",
            "assignment_id": "assignment-file-cwd",
            "task_summary": "Refuse a non-directory launch cwd before persistence",
            "task": {},
            "launch": {
                "agent": "codex",
                "cwd": file_checkout,
                "title": null,
                "session_id": "worker-file-cwd",
                "coordination_mode": "enforce",
                "agent_args": []
            },
            "repository": "example/repository",
            "worktree": file_checkout,
            "base_ref": "main",
            "scopes": ["crates/agent-session"],
            "durable_refs": []
        }),
    );
    let file_refused = run_main_agent(
        &checkout,
        &[
            "--state-dir",
            &state_arg,
            "worker",
            "start",
            "--assignment-file",
            file_assignment_path.to_str().expect("assignment path"),
            "--await-ready",
            "0",
            "--idempotency-key",
            "worker-start-file-cwd-0001",
            "--format",
            "json",
        ],
        &[
            ("AGENT_SESSION_CAPABILITY_FILE", main_capability.as_str()),
            ("AGENT_SESSION_TMUX_BIN", tmux_arg.as_str()),
            ("AGENT_SESSION_CODEX_BIN", codex_arg.as_str()),
            ("AGENT_SESSION_FAKE_TMUX_LOG", tmux_log_arg.as_str()),
            ("CODEX_HOME", codex_home_arg.as_str()),
        ],
    );
    assert_eq!(
        file_refused.code,
        65,
        "outcome={}",
        file_refused.stdout_text()
    );
    assert_eq!(
        file_refused.stdout_json()["error"]["code"],
        "assignment-launch-cwd-unavailable"
    );
    assert!(
        orchestration_registry(&state_dir)["assignments"]
            .get("assignment-file-cwd")
            .is_none(),
        "non-directory cwd refusal must not persist an assignment"
    );
    assert!(
        !state_dir.join("sessions/worker-file-cwd").exists(),
        "non-directory cwd refusal must not create a worker session"
    );
    assert!(
        tmux_calls(&tmux_log).is_empty(),
        "non-directory cwd refusal must happen before tmux launch"
    );
}

#[test]
fn main_agent_worker_start_replay_converges_a_persisted_start_without_duplicate_launch() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let checkout = tmp.path().join("checkout");
    let replacement_checkout = tmp.path().join("replacement-checkout");
    let linked_checkout = tmp.path().join("linked-checkout");
    fs::create_dir(&state_dir).expect("state");
    init_checkout(&checkout, "https://example.invalid/example/repository.git");
    init_checkout(
        &replacement_checkout,
        "https://example.invalid/example/repository.git",
    );
    std::os::unix::fs::symlink(&checkout, &linked_checkout).expect("linked checkout");
    let assignment_id = "assignment-auto-stable";
    let worker_id = "worker-assignment-auto-stable";
    seed_brokers_at(
        &state_dir,
        &[
            (
                "main-one",
                "main-incarnation-one",
                "main-private-capability-material-0000000001",
                checkout.as_path(),
                Some("enforce"),
            ),
            (
                worker_id,
                "worker-incarnation-one",
                "worker-private-capability-material-0000000001",
                checkout.as_path(),
                Some("enforce"),
            ),
        ],
    );
    let main_capability = init_main_run(tmp.path(), &state_dir, &checkout, "main-one", "run-one");
    let bootstrap_digest = orchestration_request_digest(
        "main-agent-worker-bootstrap-idempotency",
        &json!(assignment_id),
    );
    let main_agent_bin = bin::resolve("main-agent");
    let main_agent_bin = main_agent_bin.to_string_lossy();
    let main_agent_bin = shell_words::quote(&main_agent_bin);
    let worker_prompt = state_dir.join(format!("sessions/{worker_id}/prompt.md"));
    fs::write(
        &worker_prompt,
        format!(
            "You are a managed worker for assignment {assignment_id}. First run `{main_agent_bin} bootstrap --idempotency-key bootstrap-{} --format json`. Use the returned private assignment packet as your task and the returned literal `checkpoint_file` as the only checkpoint JSON write target; do not mutate before bootstrap succeeds. Write the final checkpoint payload there, then run `{main_agent_bin} checkpoint --file <returned-checkpoint_file> --if-revision <current-revision> --idempotency-key <stable-key> --format json`. After the checkpoint succeeds, release your work-context claim before reporting completion.",
            &bootstrap_digest[..32]
        ),
    )
    .expect("worker prompt");
    fs::set_permissions(&worker_prompt, fs::Permissions::from_mode(0o600))
        .expect("worker prompt mode");
    let worker_record_path = state_dir.join(format!("sessions/{worker_id}/session.json"));
    let mut worker_record: serde_json::Value =
        serde_json::from_slice(&fs::read(&worker_record_path).expect("worker record"))
            .expect("worker record json");
    worker_record["prompt_file"] = json!(worker_prompt);
    write_private_json(&worker_record_path, &worker_record);
    let assignment_input = json!({
        "schema_version": "main-agent.assignment-input.v1",
        "assignment_id": null,
        "task_summary": "Resume a durable worker launch",
        "task": {},
        "launch": {
            "agent": "codex",
            "cwd": linked_checkout,
            "title": null,
            "session_id": null,
            "coordination_mode": "enforce",
            "agent_args": []
        },
        "repository": "example/repository",
        "worktree": null,
        "base_ref": "main",
        "scopes": ["crates/agent-session"],
        "durable_refs": []
    });
    insert_orchestration_assignment(
        &state_dir,
        assignment_id,
        json!({
            "schema_version": "agent-session.orchestration-assignment.v1",
            "assignment_id": assignment_id,
            "run_id": "run-one",
            "revision": 1,
            "state": "starting",
            "task_summary": "Resume a durable worker launch",
            "private_packet_digest": "replaced-by-fixture",
            "primary_manager": {
                "session_id": "main-one",
                "session_incarnation": "main-incarnation-one",
                "session_created_at": "2030-01-01T00:00:00Z"
            },
            "worker": null,
            "collaborators": [],
            "borrowed_by": [],
            "repository": "example/repository",
            "worktree": null,
            "base_ref": "main",
            "scopes": ["crates/agent-session"],
            "durable_refs": [],
            "checkpoint": null,
            "result_summary": null,
            "blocker_summary": null,
            "created_at": "2030-01-01T00:00:01Z",
            "updated_at": "2030-01-01T00:00:01Z"
        }),
        &assignment_input,
    );
    let idempotency_key = "worker-start-crash-0001";
    let request_digest = assignment_request_digest(&assignment_input);
    let codex_home = tmp.path().join("codex-home");
    rewrite_orchestration_registry(&state_dir, |registry| {
        registry["receipts"][format!("main-one:main-incarnation-one:{idempotency_key}")] = json!({
            "principal_session_id": "main-one",
            "principal_incarnation": "main-incarnation-one",
            "operation": "worker-start",
            "request_digest": request_digest,
            "outcome": {
                "schema_version": "main-agent.worker-start-result.v1",
                "assignment_id": assignment_id,
                "worker_session_id": worker_id,
                "canonical_launch_cwd": fs::canonicalize(&checkout)
                    .expect("canonical checkout"),
                "provider_config_dir": codex_home,
                "state": "starting",
                "acceptance": "pending"
            },
            "created_at_epoch": 1
        });
    });
    let assignment_path = tmp.path().join("assignment.json");
    write_private_json(&assignment_path, &assignment_input);
    fs::remove_file(&linked_checkout).expect("remove original checkout link");
    std::os::unix::fs::symlink(&replacement_checkout, &linked_checkout)
        .expect("retarget checkout link");
    fs::create_dir(&codex_home).expect("Codex home");
    fs::write(codex_home.join("config.toml"), "[projects.").expect("malformed Codex config");
    let codex_home_arg = codex_home.to_string_lossy().into_owned();
    let args = [
        "--state-dir",
        state_dir.to_str().expect("state dir"),
        "worker",
        "start",
        "--assignment-file",
        assignment_path.to_str().expect("assignment path"),
        "--if-run-revision",
        "1",
        "--await-ready",
        "0",
        "--idempotency-key",
        idempotency_key,
        "--format",
        "json",
    ];
    let resumed = run_main_agent(
        &checkout,
        &args,
        &[
            ("AGENT_SESSION_CAPABILITY_FILE", &main_capability),
            ("CODEX_HOME", &codex_home_arg),
        ],
    );
    assert_eq!(resumed.code, 0, "stderr={}", resumed.stderr_text());
    assert_eq!(data(&resumed)["assignment"]["assignment_id"], assignment_id);
    assert_eq!(data(&resumed)["assignment"]["revision"], 2);
    assert_eq!(data(&resumed)["worker"]["session_id"], worker_id);
    let resumed_worker: serde_json::Value =
        serde_json::from_slice(&fs::read(&worker_record_path).expect("resumed worker record"))
            .expect("resumed worker record json");
    assert_eq!(
        fs::canonicalize(resumed_worker["cwd"].as_str().expect("worker cwd"))
            .expect("canonical resumed worker cwd"),
        fs::canonicalize(&checkout).expect("canonical original checkout"),
        "pending replay must use the durably approved canonical path, not a retargeted packet symlink"
    );
    assert_eq!(
        data(&resumed)["acceptance"]["state"],
        "pending-worker-checkpoint"
    );

    let replay = run_main_agent(
        &checkout,
        &args,
        &[
            ("AGENT_SESSION_CAPABILITY_FILE", &main_capability),
            ("CODEX_HOME", &codex_home_arg),
        ],
    );
    assert_eq!(replay.code, 0, "stderr={}", replay.stderr_text());
    assert_eq!(replay.stdout_text(), resumed.stdout_text());
    assert_eq!(
        fs::read_dir(state_dir.join("sessions"))
            .expect("sessions")
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name() == worker_id)
            .count(),
        1,
        "replay must not launch a duplicate worker"
    );
}

#[test]
fn main_agent_worker_start_preserves_ambiguous_initial_enter_without_redelivery() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let checkout = tmp.path().join("checkout");
    fs::create_dir(&state_dir).expect("state");
    init_checkout(&checkout, "https://example.invalid/example/repository.git");
    seed_brokers_at(
        &state_dir,
        &[(
            "main-one",
            "main-incarnation-one",
            "main-private-capability-material-0000000001",
            checkout.as_path(),
            Some("enforce"),
        )],
    );
    let main_capability = init_main_run(tmp.path(), &state_dir, &checkout, "main-one", "run-one");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let codex_bin = fake_agent(tmp.path(), "codex-worker");
    let codex_home = tmp.path().join("codex-home");
    let codex_session = codex_home.join("sessions/2026/07/26/session.jsonl");
    let assignment_path = tmp.path().join("assignment-ambiguous-enter.json");
    write_private_json(
        &assignment_path,
        &json!({
            "schema_version": "main-agent.assignment-input.v1",
            "assignment_id": "assignment-ambiguous-enter",
            "task_summary": "Preserve an ambiguous initial Enter",
            "task": {},
            "launch": {
                "agent": "codex",
                "cwd": checkout,
                "title": null,
                "session_id": "worker-ambiguous-enter",
                "coordination_mode": "enforce",
                "agent_args": []
            },
            "repository": "example/repository",
            "worktree": null,
            "base_ref": "main",
            "scopes": ["crates/agent-session"],
            "durable_refs": []
        }),
    );
    write_trusted_codex_config(&codex_home, &[&checkout]);
    let state_arg = state_dir.to_string_lossy().into_owned();
    let tmux_arg = tmux_bin.to_string_lossy().into_owned();
    let tmux_log_arg = tmux_log.to_string_lossy().into_owned();
    let codex_arg = codex_bin.to_string_lossy().into_owned();
    let codex_home_arg = codex_home.to_string_lossy().into_owned();
    let codex_session_arg = codex_session.to_string_lossy().into_owned();
    let checkout_arg = checkout.to_string_lossy().into_owned();
    let fail_once_dir = tmp.path().join("fail-after-initial-enter-once");
    let fail_once_arg = fail_once_dir.to_string_lossy().into_owned();
    let args = [
        "--state-dir",
        state_arg.as_str(),
        "worker",
        "start",
        "--assignment-file",
        assignment_path.to_str().expect("assignment path"),
        "--await-ready",
        "0",
        "--idempotency-key",
        "worker-start-ambiguous-enter-0001",
        "--format",
        "json",
    ];
    let failed = run_main_agent(
        &checkout,
        &args,
        &[
            ("AGENT_SESSION_CAPABILITY_FILE", main_capability.as_str()),
            ("AGENT_SESSION_TMUX_BIN", tmux_arg.as_str()),
            ("AGENT_SESSION_CODEX_BIN", codex_arg.as_str()),
            ("AGENT_SESSION_FAKE_TMUX_LOG", tmux_log_arg.as_str()),
            ("CODEX_HOME", codex_home_arg.as_str()),
            (
                "AGENT_SESSION_FAKE_CODEX_SESSION_FILE",
                codex_session_arg.as_str(),
            ),
            (
                "AGENT_SESSION_FAKE_CODEX_SESSION_ID",
                "codex-ambiguous-enter-resume",
            ),
            ("AGENT_SESSION_FAKE_CODEX_CWD", checkout_arg.as_str()),
            ("AGENT_SESSION_CODEX_CAPTURE_TIMEOUT_MS", "250"),
            ("AGENT_SESSION_CODEX_CAPTURE_POLL_MS", "10"),
            ("AGENT_SESSION_CODEX_AMBIGUITY_WINDOW_MS", "40"),
            ("AGENT_SESSION_FAKE_TMUX_FAIL_AFTER", "send-keys"),
            (
                "AGENT_SESSION_FAKE_TMUX_FAIL_AFTER_ONCE_DIR",
                fail_once_arg.as_str(),
            ),
        ],
    );
    assert_eq!(failed.code, 1, "outcome={}", failed.stdout_text());
    assert_eq!(
        failed.stdout_json()["error"]["code"],
        "managed-worker-prompt-delivery-outcome-unknown"
    );
    assert!(
        state_dir
            .join("sessions/worker-ambiguous-enter/session.json")
            .is_file(),
        "an ambiguous initial Enter must retain the exact worker record"
    );

    let resumed = run_main_agent(
        &checkout,
        &args,
        &[
            ("AGENT_SESSION_CAPABILITY_FILE", main_capability.as_str()),
            ("AGENT_SESSION_TMUX_BIN", tmux_arg.as_str()),
            ("AGENT_SESSION_CODEX_BIN", codex_arg.as_str()),
            ("AGENT_SESSION_FAKE_TMUX_LOG", tmux_log_arg.as_str()),
            ("CODEX_HOME", codex_home_arg.as_str()),
        ],
    );
    assert_eq!(resumed.code, 0, "stderr={}", resumed.stderr_text());
    assert_eq!(
        data(&resumed)["worker"]["session_id"],
        "worker-ambiguous-enter"
    );
    let calls = tmux_calls(&tmux_log);
    for (operation, expected) in [
        ("new-session", 1),
        ("load-buffer", 1),
        ("paste-buffer", 1),
        ("send-keys", 1),
    ] {
        assert_eq!(
            calls
                .iter()
                .filter(|call| call.first().is_some_and(|arg| arg == operation))
                .count(),
            expected,
            "retry must preserve the original worker without another {operation}: {calls:?}"
        );
    }
    assert!(
        calls.iter().all(|call| {
            call.first().is_none_or(|arg| arg != "if-shell")
                || call.iter().all(|arg| !arg.starts_with("kill-session -t "))
        }),
        "an outcome-unknown Enter must never tear down its worker: {calls:?}"
    );
    let worker_record: serde_json::Value = serde_json::from_slice(
        &fs::read(state_dir.join("sessions/worker-ambiguous-enter/session.json"))
            .expect("retained worker record"),
    )
    .expect("worker record json");
    assert_eq!(
        worker_record["provider_resume"]["session_id"], "codex-ambiguous-enter-resume",
        "preserving an ambiguous Enter must still finalize durable resume metadata"
    );
    assert!(
        state_dir
            .join("sessions/worker-ambiguous-enter/resume.json")
            .is_file(),
        "the retained worker must remain resumable after a later runtime loss"
    );
}

#[test]
fn main_agent_worker_start_binds_broker_to_same_release_agent_session_sibling() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let checkout = tmp.path().join("checkout");
    fs::create_dir(&state_dir).expect("state");
    init_checkout(&checkout, "https://example.invalid/example/repository.git");
    seed_brokers_at(
        &state_dir,
        &[(
            "main-one",
            "main-incarnation-one",
            "main-private-capability-material-0000000001",
            checkout.as_path(),
            Some("enforce"),
        )],
    );
    let main_capability = init_main_run(tmp.path(), &state_dir, &checkout, "main-one", "run-one");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let codex_bin = fake_agent(tmp.path(), "codex-worker");
    let path_bin = tmp.path().join("path-bin");
    fs::create_dir(&path_bin).expect("PATH fixture");
    let path_agent_session =
        path_bin.join(format!("agent-session{}", std::env::consts::EXE_SUFFIX));
    fs::write(&path_agent_session, "#!/bin/sh\nexit 97\n").expect("PATH trap");
    fs::set_permissions(&path_agent_session, fs::Permissions::from_mode(0o700))
        .expect("PATH trap mode");
    let inherited_path = std::env::var_os("PATH").unwrap_or_default();
    let joined_path = std::env::join_paths(
        std::iter::once(path_bin.as_path()).chain(
            std::env::split_paths(&inherited_path)
                .collect::<Vec<_>>()
                .iter()
                .map(PathBuf::as_path),
        ),
    )
    .expect("joined PATH");

    let assignment_path = tmp.path().join("assignment-facade-broker.json");
    write_private_json(
        &assignment_path,
        &json!({
            "schema_version": "main-agent.assignment-input.v1",
            "assignment_id": "assignment-facade-broker",
            "task_summary": "Exercise facade broker launch",
            "task": {},
            "launch": {
                "agent": "codex",
                "cwd": checkout,
                "title": null,
                "session_id": "worker-facade-broker",
                "coordination_mode": "enforce",
                "agent_args": []
            },
            "repository": "example/repository",
            "worktree": null,
            "base_ref": "main",
            "scopes": ["crates/agent-session"],
            "durable_refs": []
        }),
    );
    let tmux_arg = tmux_bin.to_string_lossy().into_owned();
    let tmux_log_arg = tmux_log.to_string_lossy().into_owned();
    let codex_arg = codex_bin.to_string_lossy().into_owned();
    let path_arg = joined_path.to_string_lossy().into_owned();
    let codex_home = tmp.path().join("codex-home");
    let started = run_main_agent_with_codex_trust(
        &checkout,
        &[
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "worker",
            "start",
            "--assignment-file",
            assignment_path.to_str().expect("assignment path"),
            "--if-run-revision",
            "1",
            "--await-ready",
            "0",
            "--idempotency-key",
            "worker-start-facade-broker-0001",
            "--format",
            "json",
        ],
        &[
            ("AGENT_SESSION_CAPABILITY_FILE", &main_capability),
            ("AGENT_SESSION_TMUX_BIN", &tmux_arg),
            ("AGENT_SESSION_CODEX_BIN", &codex_arg),
            ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
            ("PATH", &path_arg),
        ],
        &codex_home,
        &[&checkout],
    );
    assert_eq!(started.code, 0, "stderr={}", started.stderr_text());

    let main_agent = bin::resolve("main-agent");
    let expected_broker = bin::resolve("agent-session");
    assert_eq!(expected_broker.parent(), main_agent.parent());
    let launch = tmux_calls(&tmux_log)
        .into_iter()
        .find(|call| call.first().is_some_and(|arg| arg == "new-session"))
        .expect("worker tmux launch");
    let held_launch = launch
        .iter()
        .position(|arg| arg.contains("gate=$1; broker_gate=$2"))
        .expect("held launch script");
    let broker = Path::new(
        launch
            .get(held_launch + 8)
            .expect("held launch broker executable"),
    );
    assert_eq!(broker, expected_broker);
    assert_ne!(broker, main_agent);
    assert_ne!(broker, path_agent_session);
}

#[test]
fn main_agent_worker_delete_replay_converges_post_tombstone_and_is_exact() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let checkout = tmp.path().join("checkout");
    fs::create_dir(&state_dir).expect("state");
    init_checkout(&checkout, "https://example.invalid/example/repository.git");
    seed_brokers_at(
        &state_dir,
        &[
            (
                "main-one",
                "main-incarnation-one",
                "main-private-capability-material-0000000001",
                checkout.as_path(),
                Some("enforce"),
            ),
            (
                "worker-one",
                "worker-incarnation-one",
                "worker-private-capability-material-0000000001",
                checkout.as_path(),
                Some("enforce"),
            ),
        ],
    );
    let main_capability = init_main_run(tmp.path(), &state_dir, &checkout, "main-one", "run-one");
    let private_packet = json!({
        "schema_version": "main-agent.assignment-input.v1",
        "assignment_id": "assignment-one",
        "task_summary": "Delete a released worker",
        "task": {},
        "launch": {
            "agent": "codex",
            "cwd": checkout,
            "title": null,
            "session_id": "worker-one",
            "coordination_mode": "enforce",
            "agent_args": []
        },
        "repository": "example/repository",
        "worktree": null,
        "base_ref": "main",
        "scopes": ["crates/agent-session"],
        "durable_refs": []
    });
    insert_orchestration_assignment(
        &state_dir,
        "assignment-one",
        json!({
            "schema_version": "agent-session.orchestration-assignment.v1",
            "assignment_id": "assignment-one",
            "run_id": "run-one",
            "revision": 2,
            "state": "submitted",
            "task_summary": "Delete a released worker",
            "private_packet_digest": "replaced-by-fixture",
            "primary_manager": {
                "session_id": "main-one",
                "session_incarnation": "main-incarnation-one",
                "session_created_at": "2030-01-01T00:00:00Z"
            },
            "worker": {
                "session_id": "worker-one",
                "session_incarnation": "worker-incarnation-one",
                "session_created_at": "2030-01-01T00:00:00Z"
            },
            "collaborators": [],
            "borrowed_by": [],
            "repository": "example/repository",
            "worktree": null,
            "base_ref": "main",
            "scopes": ["crates/agent-session"],
            "durable_refs": [],
            "checkpoint": null,
            "result_summary": null,
            "blocker_summary": null,
            "created_at": "2030-01-01T00:00:01Z",
            "updated_at": "2030-01-01T00:00:02Z"
        }),
        &private_packet,
    );
    let accepted = run_main_agent(
        &checkout,
        &[
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "worker",
            "accept",
            "assignment-one",
            "--if-revision",
            "2",
            "--idempotency-key",
            "worker-accept-lifecycle-0001",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_CAPABILITY_FILE", &main_capability)],
    );
    assert_eq!(accepted.code, 0, "stderr={}", accepted.stderr_text());
    assert_eq!(data(&accepted)["assignment"]["state"], "accepted");
    assert_eq!(data(&accepted)["assignment"]["revision"], 3);
    let released = run_main_agent(
        &checkout,
        &[
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "worker",
            "release",
            "assignment-one",
            "--if-revision",
            "3",
            "--idempotency-key",
            "worker-release-lifecycle-0001",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_CAPABILITY_FILE", &main_capability)],
    );
    assert_eq!(released.code, 0, "stderr={}", released.stderr_text());
    assert_eq!(data(&released)["assignment"]["state"], "released");
    assert_eq!(data(&released)["assignment"]["revision"], 4);
    let idempotency_key = "worker-delete-crash-0001";
    let request = json!({ "assignment_id": "assignment-one", "if_revision": 4 });
    let request_digest = orchestration_request_digest("worker-delete", &request);
    rewrite_orchestration_registry(&state_dir, |registry| {
        registry["receipts"][format!("main-one:main-incarnation-one:{idempotency_key}")] = json!({
            "principal_session_id": "main-one",
            "principal_incarnation": "main-incarnation-one",
            "operation": "worker-delete",
            "request_digest": request_digest,
            "outcome": {
                "schema_version": "main-agent.worker-delete-pending.v1",
                "assignment_id": "assignment-one",
                "worker": {
                    "session_id": "worker-one",
                    "session_incarnation": "worker-incarnation-one",
                    "session_created_at": "2030-01-01T00:00:00Z"
                }
            },
            "created_at_epoch": 1
        });
    });
    let tombstone_root = state_dir.join("session-delete-tombstones");
    fs::create_dir(&tombstone_root).expect("tombstone root");
    fs::set_permissions(&tombstone_root, fs::Permissions::from_mode(0o700))
        .expect("tombstone root mode");
    let stale_tombstone = tombstone_root.join("worker-one-000-stale");
    fs::create_dir(&stale_tombstone).expect("stale tombstone");
    fs::set_permissions(&stale_tombstone, fs::Permissions::from_mode(0o700))
        .expect("stale tombstone mode");
    let mut stale_record: serde_json::Value = serde_json::from_slice(
        &fs::read(state_dir.join("sessions/worker-one/session.json")).expect("worker record"),
    )
    .expect("worker record json");
    stale_record["runtime"]["launch_id"] = json!("worker-incarnation-stale");
    write_private_json(&stale_tombstone.join("session.json"), &stale_record);
    let tombstone = tombstone_root.join("worker-one-fixture");
    fs::rename(state_dir.join("sessions/worker-one"), &tombstone)
        .expect("persist logical delete tombstone");

    let args = [
        "--state-dir",
        state_dir.to_str().expect("state dir"),
        "worker",
        "delete",
        "assignment-one",
        "--if-revision",
        "4",
        "--idempotency-key",
        idempotency_key,
        "--format",
        "json",
    ];
    let recovered = run_main_agent(
        &checkout,
        &args,
        &[("AGENT_SESSION_CAPABILITY_FILE", &main_capability)],
    );
    assert_eq!(recovered.code, 0, "stderr={}", recovered.stderr_text());
    assert_eq!(data(&recovered)["assignment"]["revision"], 5);
    assert_eq!(data(&recovered)["deleted"], true);
    assert_eq!(data(&recovered)["cleanup_pending"], true);

    let replay = run_main_agent(
        &checkout,
        &args,
        &[("AGENT_SESSION_CAPABILITY_FILE", &main_capability)],
    );
    assert_eq!(replay.code, 0, "stderr={}", replay.stderr_text());
    assert_eq!(replay.stdout_text(), recovered.stdout_text());
    assert!(
        tombstone.exists(),
        "replay must not repeat physical cleanup"
    );
    assert!(!state_dir.join("sessions/worker-one").exists());
}

#[test]
fn main_agent_borrow_prunes_all_expired_relationships_before_enforcing_the_limit() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let checkout = tmp.path().join("checkout");
    fs::create_dir(&state_dir).expect("state");
    init_checkout(&checkout, "https://example.invalid/example/repository.git");
    seed_brokers_at(
        &state_dir,
        &[
            (
                "main-one",
                "main-incarnation-one",
                "main-private-capability-material-0000000001",
                checkout.as_path(),
                Some("enforce"),
            ),
            (
                "borrower-new",
                "borrower-incarnation-new",
                "borrower-private-capability-material-0000000001",
                checkout.as_path(),
                Some("advisory"),
            ),
        ],
    );
    let main_capability = init_main_run(tmp.path(), &state_dir, &checkout, "main-one", "run-one");
    let expired = (0..16)
        .map(|index| {
            json!({
                "session": {
                    "session_id": format!("expired-borrower-{index:02}"),
                    "session_incarnation": format!("expired-incarnation-{index:02}"),
                    "session_created_at": "2030-01-01T00:00:00Z"
                },
                "expires_at": "1970-01-01T00:00:01Z",
                "expires_at_epoch": 1
            })
        })
        .collect::<Vec<_>>();
    let private_packet = json!({
        "schema_version": "main-agent.assignment-input.v1",
        "assignment_id": "assignment-one",
        "task_summary": "Prune expired borrowers",
        "task": {},
        "launch": {
            "agent": "codex",
            "cwd": checkout,
            "title": null,
            "session_id": null,
            "coordination_mode": "enforce",
            "agent_args": []
        },
        "repository": "example/repository",
        "worktree": null,
        "base_ref": "main",
        "scopes": ["crates/agent-session"],
        "durable_refs": []
    });
    insert_orchestration_assignment(
        &state_dir,
        "assignment-one",
        json!({
            "schema_version": "agent-session.orchestration-assignment.v1",
            "assignment_id": "assignment-one",
            "run_id": "run-one",
            "revision": 1,
            "state": "assigned",
            "task_summary": "Prune expired borrowers",
            "private_packet_digest": "replaced-by-fixture",
            "primary_manager": {
                "session_id": "main-one",
                "session_incarnation": "main-incarnation-one",
                "session_created_at": "2030-01-01T00:00:00Z"
            },
            "worker": null,
            "collaborators": [],
            "borrowed_by": expired,
            "repository": "example/repository",
            "worktree": null,
            "base_ref": "main",
            "scopes": ["crates/agent-session"],
            "durable_refs": [],
            "checkpoint": null,
            "result_summary": null,
            "blocker_summary": null,
            "created_at": "2030-01-01T00:00:01Z",
            "updated_at": "2030-01-01T00:00:01Z"
        }),
        &private_packet,
    );
    let borrowed = run_main_agent(
        &checkout,
        &[
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "borrow",
            "assignment-one",
            "--session",
            "borrower-new@borrower-incarnation-new",
            "--duration",
            "1h",
            "--if-revision",
            "1",
            "--idempotency-key",
            "borrow-prune-0001",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_CAPABILITY_FILE", &main_capability)],
    );
    assert_eq!(borrowed.code, 0, "stderr={}", borrowed.stderr_text());
    let projected_borrowers = data(&borrowed)["assignment"]["borrowed_by"]
        .as_array()
        .expect("projected borrowers")
        .clone();
    assert_eq!(projected_borrowers.len(), 1);
    assert_eq!(projected_borrowers[0]["session_id"], "borrower-new");
    assert_eq!(
        projected_borrowers[0]["session_incarnation"],
        "borrower-incarnation-new"
    );
    let registry: serde_json::Value = serde_json::from_slice(
        &fs::read(state_dir.join("orchestration/registry.json")).expect("registry"),
    )
    .expect("registry json");
    assert_eq!(
        registry["assignments"]["assignment-one"]["borrowed_by"]
            .as_array()
            .expect("borrowers")
            .len(),
        1
    );
}

#[test]
fn main_agent_worker_request_changes_rejects_invalid_reasons_as_public_input() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let checkout = tmp.path().join("checkout");
    fs::create_dir(&state_dir).expect("state");
    fs::create_dir(&checkout).expect("checkout");
    let state = state_dir.to_str().expect("state dir");
    let over_limit = "x".repeat(241);
    for (index, reason) in ["", "line\nbreak", over_limit.as_str()]
        .into_iter()
        .enumerate()
    {
        for equals_form in [false, true] {
            let revision = "1";
            let idempotency_key = format!("request-changes-invalid-{index}-{equals_form}");
            let mut args = vec![
                "--state-dir",
                state,
                "worker",
                "request-changes",
                "assignment-review",
                "--if-revision",
                revision,
                "--reason",
                reason,
                "--idempotency-key",
                &idempotency_key,
            ];
            if equals_form {
                args.push("--format=json");
            } else {
                args.extend(["--format", "json"]);
            }
            let invalid = run_main_agent(&checkout, &args, &[]);
            assert_eq!(invalid.code, 65, "stderr={}", invalid.stderr_text());
            assert_eq!(
                invalid.stdout_json()["schema_version"],
                "cli.main-agent.worker-request-changes.v1"
            );
            assert_eq!(
                invalid.stdout_json()["error"]["code"],
                "invalid-orchestration-input"
            );
            assert_eq!(invalid.stderr_text(), "");
        }
    }
}

#[test]
fn main_agent_worker_request_changes_reopens_only_the_submitted_assignment() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let checkout = tmp.path().join("checkout");
    fs::create_dir(&state_dir).expect("state");
    init_checkout(&checkout, "https://example.invalid/example/repository.git");
    seed_brokers_at(
        &state_dir,
        &[
            (
                "main-one",
                "main-incarnation-one",
                "main-private-capability-material-0000000001",
                checkout.as_path(),
                Some("enforce"),
            ),
            (
                "worker-one",
                "worker-incarnation-one",
                "worker-private-capability-material-0000000001",
                checkout.as_path(),
                Some("enforce"),
            ),
        ],
    );
    let main_capability = init_main_run(
        tmp.path(),
        &state_dir,
        &checkout,
        "main-one",
        "run-request-changes",
    );
    let private_packet = json!({
        "schema_version": "main-agent.assignment-input.v1",
        "assignment_id": "assignment-review",
        "task_summary": "Repair reviewed candidate",
        "task": {"private_note": "packet-preservation-canary"},
        "launch": {
            "agent": "codex",
            "cwd": checkout,
            "title": null,
            "session_id": "worker-one",
            "coordination_mode": "enforce",
            "agent_args": []
        },
        "repository": "example/repository",
        "worktree": null,
        "base_ref": "main",
        "scopes": ["crates/agent-session"],
        "durable_refs": []
    });
    insert_orchestration_assignment(
        &state_dir,
        "assignment-review",
        json!({
            "schema_version": "agent-session.orchestration-assignment.v1",
            "assignment_id": "assignment-review",
            "run_id": "run-request-changes",
            "revision": 5,
            "state": "submitted",
            "task_summary": "Repair reviewed candidate",
            "private_packet_digest": "replaced-by-fixture",
            "primary_manager": {
                "session_id": "main-one",
                "session_incarnation": "main-incarnation-one",
                "session_created_at": "2030-01-01T00:00:00Z"
            },
            "worker": {
                "session_id": "worker-one",
                "session_incarnation": "worker-incarnation-one",
                "session_created_at": "2030-01-01T00:00:00Z"
            },
            "collaborators": [],
            "borrowed_by": [],
            "repository": "example/repository",
            "worktree": null,
            "base_ref": "main",
            "scopes": ["crates/agent-session"],
            "durable_refs": [],
            "checkpoint": {
                "revision": 5,
                "summary": "Submitted stale candidate",
                "next_action": "Await acceptance",
                "updated_at": "2030-01-01T00:00:02Z"
            },
            "result_summary": "Stale candidate result",
            "blocker_summary": "Stale blocker",
            "created_at": "2030-01-01T00:00:01Z",
            "updated_at": "2030-01-01T00:00:02Z"
        }),
        &private_packet,
    );
    let before = orchestration_registry(&state_dir);
    let worker_before = before["assignments"]["assignment-review"]["worker"].clone();
    let packet_before = before["assignments"]["assignment-review"]["private_packet_digest"].clone();
    seed_active_claim(
        &state_dir,
        "worker-one",
        "worker-incarnation-one",
        "worker-request-changes-claim",
    );
    let worker_capability = capability(&state_dir, "worker-one");
    let wrong_role = run_main_agent(
        &checkout,
        &[
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "worker",
            "request-changes",
            "assignment-review",
            "--if-revision",
            "5",
            "--reason",
            "Worker cannot reopen its own submission",
            "--idempotency-key",
            "request-changes-wrong-role-0001",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_CAPABILITY_FILE", &worker_capability)],
    );
    assert_ne!(wrong_role.code, 0);
    assert_eq!(
        wrong_role.stdout_json()["error"]["code"],
        "controller-rebind-required"
    );
    assert_eq!(
        orchestration_registry(&state_dir)["assignments"]["assignment-review"]["state"],
        "submitted"
    );

    let args = [
        "--state-dir",
        state_dir.to_str().expect("state dir"),
        "worker",
        "request-changes",
        "assignment-review",
        "--if-revision",
        "5",
        "--reason",
        "Address the exact review findings",
        "--idempotency-key",
        "request-changes-review-0001",
        "--format",
        "json",
    ];
    let requested = run_main_agent(
        &checkout,
        &args,
        &[("AGENT_SESSION_CAPABILITY_FILE", &main_capability)],
    );
    assert_eq!(requested.code, 0, "stderr={}", requested.stderr_text());
    let assignment = &data(&requested)["assignment"];
    assert_eq!(assignment["state"], "working");
    assert_eq!(assignment["revision"], 6);
    assert_eq!(assignment["worker"], worker_before);
    assert_eq!(assignment["result_summary"], serde_json::Value::Null);
    assert_eq!(assignment["blocker_summary"], serde_json::Value::Null);
    assert_eq!(
        assignment["checkpoint"]["summary"],
        "Main Agent requested revisions"
    );
    assert_eq!(
        assignment["checkpoint"]["next_action"],
        "Address the exact review findings"
    );
    let after = orchestration_registry(&state_dir);
    assert_eq!(
        after["assignments"]["assignment-review"]["worker"],
        worker_before
    );
    assert_eq!(
        after["assignments"]["assignment-review"]["private_packet_digest"],
        packet_before
    );

    let replay = run_main_agent(
        &checkout,
        &args,
        &[("AGENT_SESSION_CAPABILITY_FILE", &main_capability)],
    );
    assert_eq!(replay.code, 0, "stderr={}", replay.stderr_text());
    assert_eq!(replay.stdout_text(), requested.stdout_text());

    let stale = run_main_agent(
        &checkout,
        &[
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "worker",
            "request-changes",
            "assignment-review",
            "--if-revision",
            "5",
            "--reason",
            "A different stale request",
            "--idempotency-key",
            "request-changes-stale-0001",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_CAPABILITY_FILE", &main_capability)],
    );
    assert_ne!(stale.code, 0);
    assert_eq!(
        stale.stdout_json()["error"]["code"],
        "orchestration-revision-conflict"
    );

    let invalid_state = run_main_agent(
        &checkout,
        &[
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "worker",
            "request-changes",
            "assignment-review",
            "--if-revision",
            "6",
            "--reason",
            "Cannot reopen working state",
            "--idempotency-key",
            "request-changes-invalid-state-0001",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_CAPABILITY_FILE", &main_capability)],
    );
    assert_ne!(invalid_state.code, 0);
    assert_eq!(
        invalid_state.stdout_json()["error"]["code"],
        "assignment-state-conflict"
    );

    rewrite_orchestration_registry(&state_dir, |registry| {
        let assignment = &mut registry["assignments"]["assignment-review"];
        assignment["revision"] = json!(7);
        assignment["state"] = json!("submitted");
        assignment["primary_manager"]["session_id"] = json!("worker-one");
        assignment["primary_manager"]["session_incarnation"] = json!("worker-incarnation-one");
    });
    let wrong_manager = run_main_agent(
        &checkout,
        &[
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "worker",
            "request-changes",
            "assignment-review",
            "--if-revision",
            "7",
            "--reason",
            "Wrong manager must fail closed",
            "--idempotency-key",
            "request-changes-wrong-manager-0001",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_CAPABILITY_FILE", &main_capability)],
    );
    assert_ne!(wrong_manager.code, 0);
    assert_eq!(
        wrong_manager.stdout_json()["error"]["code"],
        "primary-manager-conflict"
    );

    rewrite_orchestration_registry(&state_dir, |registry| {
        let assignment = &mut registry["assignments"]["assignment-review"];
        assignment["revision"] = json!(8);
        assignment["state"] = json!("accepted");
        assignment["primary_manager"]["session_id"] = json!("main-one");
        assignment["primary_manager"]["session_incarnation"] = json!("main-incarnation-one");
    });
    let terminal = run_main_agent(
        &checkout,
        &[
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "worker",
            "request-changes",
            "assignment-review",
            "--if-revision",
            "8",
            "--reason",
            "Terminal state must remain immutable",
            "--idempotency-key",
            "request-changes-terminal-0001",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_CAPABILITY_FILE", &main_capability)],
    );
    assert_ne!(terminal.code, 0);
    assert_eq!(
        terminal.stdout_json()["error"]["code"],
        "assignment-state-conflict"
    );

    rewrite_orchestration_registry(&state_dir, |registry| {
        let assignment = &mut registry["assignments"]["assignment-review"];
        assignment["revision"] = json!(u64::MAX);
        assignment["state"] = json!("submitted");
    });
    let before_overflow =
        fs::read(state_dir.join("orchestration/registry.json")).expect("registry before overflow");
    let overflow = run_main_agent(
        &checkout,
        &[
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "worker",
            "request-changes",
            "assignment-review",
            "--if-revision",
            "18446744073709551615",
            "--reason",
            "Revision overflow must fail without mutation",
            "--idempotency-key",
            "request-changes-overflow-0001",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_CAPABILITY_FILE", &main_capability)],
    );
    assert_eq!(overflow.code, 65, "stderr={}", overflow.stderr_text());
    assert_eq!(
        overflow.stdout_json()["error"]["code"],
        "orchestration-revision-capacity"
    );
    assert_eq!(
        fs::read(state_dir.join("orchestration/registry.json")).expect("registry after overflow"),
        before_overflow,
        "revision overflow must leave the registry byte-for-byte unchanged"
    );
}

#[test]
fn main_agent_worker_self_checkpoint_and_collaborator_visibility_are_durable() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let checkout = tmp.path().join("checkout");
    fs::create_dir(&state_dir).expect("state");
    init_checkout(&checkout, "https://example.invalid/example/repository.git");
    seed_brokers_at(
        &state_dir,
        &[
            (
                "main-one",
                "main-incarnation-one",
                "main-private-capability-material-0000000001",
                checkout.as_path(),
                Some("enforce"),
            ),
            (
                "worker-one",
                "worker-incarnation-one",
                "worker-private-capability-material-0000000001",
                checkout.as_path(),
                Some("enforce"),
            ),
            (
                "collaborator-one",
                "collaborator-incarnation-one",
                "collaborator-private-capability-material-0000001",
                checkout.as_path(),
                Some("advisory"),
            ),
        ],
    );
    let main_capability = init_main_run(tmp.path(), &state_dir, &checkout, "main-one", "run-one");
    seed_active_claim(
        &state_dir,
        "worker-one",
        "worker-incarnation-one",
        "worker-claim-one",
    );
    let private_packet = json!({
        "schema_version": "main-agent.assignment-input.v1",
        "assignment_id": "assignment-one",
        "task_summary": "Checkpoint worker acceptance",
        "task": {"private_note": "worker-private-canary"},
        "launch": {
            "agent": "codex",
            "cwd": checkout,
            "title": null,
            "session_id": "worker-one",
            "coordination_mode": "enforce",
            "agent_args": []
        },
        "repository": "example/repository",
        "worktree": null,
        "base_ref": "main",
        "scopes": ["crates/agent-session"],
        "durable_refs": []
    });
    insert_orchestration_assignment(
        &state_dir,
        "assignment-one",
        json!({
            "schema_version": "agent-session.orchestration-assignment.v1",
            "assignment_id": "assignment-one",
            "run_id": "run-one",
            "revision": 2,
            "state": "starting",
            "task_summary": "Checkpoint worker acceptance",
            "private_packet_digest": "replaced-by-fixture",
            "primary_manager": {
                "session_id": "main-one",
                "session_incarnation": "main-incarnation-one",
                "session_created_at": "2030-01-01T00:00:00Z"
            },
            "worker": {
                "session_id": "worker-one",
                "session_incarnation": "worker-incarnation-one",
                "session_created_at": "2030-01-01T00:00:00Z"
            },
            "collaborators": [],
            "borrowed_by": [],
            "repository": "example/repository",
            "worktree": null,
            "base_ref": "main",
            "scopes": ["crates/agent-session"],
            "durable_refs": [],
            "checkpoint": null,
            "result_summary": null,
            "blocker_summary": null,
            "created_at": "2030-01-01T00:00:01Z",
            "updated_at": "2030-01-01T00:00:02Z"
        }),
        &private_packet,
    );
    let worker_capability = capability(&state_dir, "worker-one");
    let self_show = run_main_agent(
        &checkout,
        &[
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "self",
            "show",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_CAPABILITY_FILE", &worker_capability)],
    );
    assert_eq!(self_show.code, 0, "stderr={}", self_show.stderr_text());
    assert_eq!(data(&self_show)["role"], "worker");
    assert_eq!(
        data(&self_show)["assignment"]["record"]["state"],
        "starting"
    );
    assert!(self_show.stdout_text().contains("worker-private-canary"));

    let collaborated = run_main_agent(
        &checkout,
        &[
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "collaborate",
            "assignment-one",
            "--session",
            "collaborator-one@collaborator-incarnation-one",
            "--if-revision",
            "2",
            "--idempotency-key",
            "collaborate-lifecycle-0001",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_CAPABILITY_FILE", &main_capability)],
    );
    assert_eq!(
        collaborated.code,
        0,
        "stderr={}",
        collaborated.stderr_text()
    );
    assert_eq!(
        data(&collaborated)["assignment"]["collaborators"][0]["session_id"],
        "collaborator-one"
    );

    let checkpoint_path = tmp.path().join("worker-checkpoint.json");
    write_private_json(
        &checkpoint_path,
        &json!({
            "schema_version": "main-agent.checkpoint-input.v1",
            "summary": "Worker authenticated and working",
            "next_action": "Implement the bounded assignment",
            "state": "working",
            "result_summary": null,
            "blocker_summary": null
        }),
    );
    let checkpoint_args = [
        "--state-dir",
        state_dir.to_str().expect("state dir"),
        "checkpoint",
        "--file",
        checkpoint_path.to_str().expect("checkpoint path"),
        "--if-revision",
        "3",
        "--idempotency-key",
        "worker-checkpoint-0001",
        "--format",
        "json",
    ];
    let checkpointed = run_main_agent(
        &checkout,
        &checkpoint_args,
        &[("AGENT_SESSION_CAPABILITY_FILE", &worker_capability)],
    );
    assert_eq!(
        checkpointed.code,
        0,
        "stderr={}",
        checkpointed.stderr_text()
    );
    assert_eq!(data(&checkpointed)["assignment"]["state"], "working");
    assert_eq!(data(&checkpointed)["assignment"]["revision"], 4);
    assert_eq!(
        data(&checkpointed)["assignment"]["collaborators"][0]["session_id"],
        "collaborator-one"
    );
    let replay = run_main_agent(
        &checkout,
        &checkpoint_args,
        &[("AGENT_SESSION_CAPABILITY_FILE", &worker_capability)],
    );
    assert_eq!(replay.code, 0, "stderr={}", replay.stderr_text());
    assert_eq!(replay.stdout_text(), checkpointed.stdout_text());

    let submitted_checkpoint_path = tmp.path().join("worker-submitted-checkpoint.json");
    write_private_json(
        &submitted_checkpoint_path,
        &json!({
            "schema_version": "main-agent.checkpoint-input.v1",
            "summary": "Worker submitted the bounded assignment",
            "next_action": "Await Main Agent acceptance",
            "state": "submitted",
            "result_summary": "Bounded assignment complete",
            "blocker_summary": null
        }),
    );
    let submitted = run_main_agent(
        &checkout,
        &[
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "checkpoint",
            "--file",
            submitted_checkpoint_path
                .to_str()
                .expect("submitted checkpoint path"),
            "--if-revision",
            "4",
            "--idempotency-key",
            "worker-checkpoint-submitted-0001",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_CAPABILITY_FILE", &worker_capability)],
    );
    assert_eq!(submitted.code, 0, "stderr={}", submitted.stderr_text());
    assert_eq!(data(&submitted)["assignment"]["state"], "submitted");
    assert_eq!(data(&submitted)["assignment"]["revision"], 5);

    let resumed_incarnation = "worker-incarnation-two";
    let resumed_capability = "worker-private-capability-material-0000000002";
    let worker_session_path = state_dir.join("sessions/worker-one/session.json");
    let mut worker_session: serde_json::Value =
        serde_json::from_slice(&fs::read(&worker_session_path).expect("worker session record"))
            .expect("worker session json");
    worker_session["runtime"]["launch_id"] = json!(resumed_incarnation);
    write_private_json(&worker_session_path, &worker_session);
    let worker_coordination_dir = state_dir.join("sessions/worker-one/coordination");
    let resumed_capability_file =
        worker_coordination_dir.join(format!("capability-{}", digest(resumed_incarnation)));
    fs::write(&resumed_capability_file, resumed_capability).expect("resumed capability");
    fs::set_permissions(&resumed_capability_file, fs::Permissions::from_mode(0o600))
        .expect("resumed capability mode");
    let resumed_checkpoint_file = worker_coordination_dir.join(format!(
        "main-agent-checkpoint-{}.json",
        digest(resumed_incarnation)
    ));
    fs::write(&resumed_checkpoint_file, []).expect("resumed checkpoint");
    fs::set_permissions(&resumed_checkpoint_file, fs::Permissions::from_mode(0o600))
        .expect("resumed checkpoint mode");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs();
    fs::write(
        worker_coordination_dir.join("heartbeat"),
        format!("{resumed_incarnation}:{now}\n"),
    )
    .expect("resumed heartbeat");
    rewrite_registry(&state_dir, |registry| {
        registry["brokers"]["worker-one"]["incarnation"] = json!(resumed_incarnation);
        registry["brokers"]["worker-one"]["capability_digest"] = json!(digest(resumed_capability));
        registry["brokers"]["worker-one"]["heartbeat_epoch"] = json!(now);
        for claim in registry["claims"].as_array_mut().expect("claims") {
            if claim["session_id"] == "worker-one" {
                claim["state"] = json!("released");
            }
        }
    });
    seed_active_claim(
        &state_dir,
        "worker-one",
        resumed_incarnation,
        "worker-claim-two",
    );
    let resumed_capability_arg = resumed_capability_file.to_string_lossy().into_owned();

    let resumed_self = run_main_agent(
        &checkout,
        &[
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "self",
            "show",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_CAPABILITY_FILE", &resumed_capability_arg)],
    );
    assert_eq!(
        resumed_self.code,
        0,
        "stderr={}",
        resumed_self.stderr_text()
    );
    assert_eq!(data(&resumed_self)["role"], "worker");
    assert_eq!(data(&resumed_self)["rebind_required"], true);

    let resumed_list = run(
        &checkout,
        &[
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "list",
            "--format",
            "json",
        ],
    );
    assert_eq!(
        resumed_list.code,
        0,
        "stderr={}",
        resumed_list.stderr_text()
    );
    let resumed_sessions = data(&resumed_list);
    let resumed_worker = resumed_sessions
        .as_array()
        .expect("session list")
        .iter()
        .find(|session| session["id"] == "worker-one")
        .expect("resumed worker session");
    assert_eq!(resumed_worker["orchestration"]["role"], "worker");
    assert_eq!(
        resumed_worker["orchestration"]["assignment_id"],
        "assignment-one"
    );
    assert_eq!(
        resumed_worker["orchestration"]["relationship_state"],
        "rebind_required"
    );

    let rebound = run_main_agent(
        &checkout,
        &[
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "checkpoint",
            "--file",
            checkpoint_path.to_str().expect("checkpoint path"),
            "--if-revision",
            "5",
            "--idempotency-key",
            "worker-checkpoint-rebind-0001",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_CAPABILITY_FILE", &resumed_capability_arg)],
    );
    assert_eq!(rebound.code, 0, "stderr={}", rebound.stderr_text());
    assert_eq!(data(&rebound)["assignment"]["revision"], 6);
    assert_eq!(
        data(&rebound)["assignment"]["state"],
        "submitted",
        "continuity rebind must not regress a submitted assignment"
    );
    assert_eq!(
        data(&rebound)["assignment"]["worker"]["session_incarnation"],
        resumed_incarnation
    );

    let rebound_self = run_main_agent(
        &checkout,
        &[
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "self",
            "show",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_CAPABILITY_FILE", &resumed_capability_arg)],
    );
    assert_eq!(
        rebound_self.code,
        0,
        "stderr={}",
        rebound_self.stderr_text()
    );
    assert_eq!(data(&rebound_self)["rebind_required"], false);

    let rebound_list = run(
        &checkout,
        &[
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "list",
            "--format",
            "json",
        ],
    );
    assert_eq!(
        rebound_list.code,
        0,
        "stderr={}",
        rebound_list.stderr_text()
    );
    let rebound_sessions = data(&rebound_list);
    let rebound_worker = rebound_sessions
        .as_array()
        .expect("session list")
        .iter()
        .find(|session| session["id"] == "worker-one")
        .expect("rebound worker session");
    assert_eq!(rebound_worker["orchestration"]["role"], "worker");
    assert_eq!(
        rebound_worker["orchestration"]["relationship_state"],
        "cross_managed"
    );

    let accepted = run_main_agent(
        &checkout,
        &[
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "worker",
            "accept",
            "assignment-one",
            "--if-revision",
            "6",
            "--idempotency-key",
            "worker-accept-rebound-0001",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_CAPABILITY_FILE", &main_capability)],
    );
    assert_eq!(accepted.code, 0, "stderr={}", accepted.stderr_text());
    assert_eq!(data(&accepted)["assignment"]["state"], "accepted");
    assert_eq!(data(&accepted)["assignment"]["revision"], 7);

    let terminal_checkpoint = run_main_agent(
        &checkout,
        &[
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "checkpoint",
            "--file",
            checkpoint_path.to_str().expect("checkpoint path"),
            "--if-revision",
            "7",
            "--idempotency-key",
            "worker-checkpoint-after-accept-0001",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_CAPABILITY_FILE", &resumed_capability_arg)],
    );
    assert_eq!(terminal_checkpoint.code, 65);
    assert_eq!(
        terminal_checkpoint.stdout_json()["error"]["code"],
        "assignment-terminal"
    );
    assert_eq!(
        orchestration_registry(&state_dir)["assignments"]["assignment-one"]["revision"],
        7,
        "terminal worker checkpoint rejection must preserve the manager revision"
    );

    let released = run_main_agent(
        &checkout,
        &[
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "worker",
            "release",
            "assignment-one",
            "--if-revision",
            "7",
            "--idempotency-key",
            "worker-release-rebound-0001",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_CAPABILITY_FILE", &main_capability)],
    );
    assert_eq!(released.code, 0, "stderr={}", released.stderr_text());
    assert_eq!(data(&released)["assignment"]["state"], "released");
    assert_eq!(data(&released)["assignment"]["revision"], 8);
    assert_eq!(
        data(&released)["assignment"]["worker"]["session_incarnation"],
        resumed_incarnation
    );

    rewrite_registry(&state_dir, |registry| {
        for claim in registry["claims"].as_array_mut().expect("claims") {
            if claim["session_id"] == "worker-one" {
                claim["state"] = json!("released");
            }
        }
    });
    let mut deletable_worker: serde_json::Value = serde_json::from_slice(
        &fs::read(&worker_session_path).expect("rebound worker session record"),
    )
    .expect("rebound worker session json");
    deletable_worker["tmux_runtime_never_launched"] = json!(resumed_incarnation);
    write_private_json(&worker_session_path, &deletable_worker);
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let tmux_arg = tmux_bin.to_string_lossy().into_owned();
    let tmux_log_arg = tmux_log.to_string_lossy().into_owned();
    let deleted = run_main_agent(
        &checkout,
        &[
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "worker",
            "delete",
            "assignment-one",
            "--if-revision",
            "8",
            "--idempotency-key",
            "worker-delete-rebound-0001",
            "--format",
            "json",
        ],
        &[
            ("AGENT_SESSION_CAPABILITY_FILE", &main_capability),
            ("AGENT_SESSION_TMUX_BIN", &tmux_arg),
            ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
            ("AGENT_SESSION_FAKE_TMUX_ABSENT", "1"),
        ],
    );
    assert_eq!(deleted.code, 0, "stderr={}", deleted.stderr_text());
    assert_eq!(data(&deleted)["assignment"]["revision"], 9);
    assert_eq!(data(&deleted)["assignment"]["state"], "released");
    assert_eq!(
        data(&deleted)["assignment"]["worker"]["session_incarnation"],
        resumed_incarnation
    );
    assert_eq!(data(&deleted)["deleted"], true);
    assert_eq!(data(&deleted)["cleanup_pending"], false);
    assert!(!worker_session_path.exists());
    assert!(
        tmux_calls(&tmux_log)
            .iter()
            .all(|call| call.first().is_none_or(|arg| arg != "kill-session")),
        "the exact rebound record is proven never launched and needs no tmux kill"
    );
}

#[test]
fn main_agent_worker_bootstrap_rejects_assignment_checkout_mismatch_before_grant() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let main_checkout = tmp.path().join("main-checkout");
    let worker_checkout = tmp.path().join("worker-checkout");
    let declared_checkout = tmp.path().join("declared-checkout");
    fs::create_dir(&state_dir).expect("state");
    for checkout in [&main_checkout, &worker_checkout, &declared_checkout] {
        init_checkout(checkout, "https://example.invalid/example/repository.git");
    }
    seed_brokers_at(
        &state_dir,
        &[
            (
                "main-one",
                "main-incarnation-one",
                "main-private-capability-material-0000000001",
                main_checkout.as_path(),
                Some("enforce"),
            ),
            (
                "worker-one",
                "worker-incarnation-one",
                "worker-private-capability-material-0000000001",
                worker_checkout.as_path(),
                Some("enforce"),
            ),
        ],
    );
    let main_capability = init_main_run(
        tmp.path(),
        &state_dir,
        &main_checkout,
        "main-one",
        "run-one",
    );
    let private_packet = json!({
        "schema_version": "main-agent.assignment-input.v1",
        "assignment_id": "assignment-checkout-mismatch",
        "task_summary": "Reject mismatched assignment checkout",
        "task": {},
        "launch": {
            "agent": "codex",
            "cwd": worker_checkout,
            "title": null,
            "session_id": "worker-one",
            "coordination_mode": "enforce",
            "agent_args": []
        },
        "repository": "example/repository",
        "worktree": declared_checkout,
        "base_ref": "main",
        "scopes": ["docs/bootstrap-canary"],
        "durable_refs": []
    });
    insert_orchestration_assignment(
        &state_dir,
        "assignment-checkout-mismatch",
        json!({
            "schema_version": "agent-session.orchestration-assignment.v1",
            "assignment_id": "assignment-checkout-mismatch",
            "run_id": "run-one",
            "revision": 2,
            "state": "starting",
            "task_summary": "Reject mismatched assignment checkout",
            "private_packet_digest": "replaced-by-fixture",
            "primary_manager": {
                "session_id": "main-one",
                "session_incarnation": "main-incarnation-one",
                "session_created_at": "2030-01-01T00:00:00Z"
            },
            "worker": {
                "session_id": "worker-one",
                "session_incarnation": "worker-incarnation-one",
                "session_created_at": "2030-01-01T00:00:00Z"
            },
            "collaborators": [],
            "borrowed_by": [],
            "repository": "example/repository",
            "worktree": declared_checkout,
            "base_ref": "main",
            "scopes": ["docs/bootstrap-canary"],
            "durable_refs": [],
            "checkpoint": null,
            "result_summary": null,
            "blocker_summary": null,
            "created_at": "2030-01-01T00:00:01Z",
            "updated_at": "2030-01-01T00:00:02Z"
        }),
        &private_packet,
    );
    let worker_capability = capability(&state_dir, "worker-one");
    let rejected = run_main_agent(
        &worker_checkout,
        &[
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "bootstrap",
            "--idempotency-key",
            "worker-bootstrap-checkout-mismatch-0001",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_CAPABILITY_FILE", &worker_capability)],
    );
    assert_eq!(rejected.code, 65, "stderr={}", rejected.stderr_text());
    assert_eq!(
        rejected.stdout_json()["error"]["code"],
        "worker-bootstrap-checkout-mismatch"
    );
    let coordination = load_coordination_registry(&state_dir);
    assert!(
        coordination["claims"]
            .as_array()
            .expect("claims")
            .iter()
            .all(|claim| claim["session_id"] != "worker-one"),
        "checkout mismatch must fail before a worker claim or grant is persisted"
    );
    let orchestration = orchestration_registry(&state_dir);
    assert_eq!(
        orchestration["assignments"]["assignment-checkout-mismatch"]["state"],
        "blocked"
    );
    assert_eq!(
        orchestration["assignments"]["assignment-checkout-mismatch"]["revision"],
        3
    );
    assert_eq!(
        orchestration["assignments"]["assignment-checkout-mismatch"]["blocker_summary"],
        "[pre-claim:worker-bootstrap-checkout-mismatch] worker bootstrap failed"
    );

    let diagnosed = run_main_agent(
        &main_checkout,
        &[
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "worker",
            "diagnose",
            "assignment-checkout-mismatch",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_CAPABILITY_FILE", &main_capability)],
    );
    assert_eq!(diagnosed.code, 0, "stderr={}", diagnosed.stderr_text());
    assert_eq!(data(&diagnosed)["classification"], "pre_claim_failure");
    assert_eq!(data(&diagnosed)["failed_preclaim"], true);

    let cancelled = run_main_agent(
        &main_checkout,
        &[
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "worker",
            "cancel",
            "assignment-checkout-mismatch",
            "--if-revision",
            "3",
            "--reason",
            "replace checkout-mismatched worker",
            "--idempotency-key",
            "worker-cancel-checkout-mismatch-0001",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_CAPABILITY_FILE", &main_capability)],
    );
    assert_eq!(cancelled.code, 0, "stderr={}", cancelled.stderr_text());
    assert_eq!(data(&cancelled)["assignment"]["state"], "cancelled");
    assert_eq!(data(&cancelled)["assignment"]["revision"], 4);
    assert_eq!(data(&cancelled)["claim_absent"], true);
    assert_eq!(data(&cancelled)["operation_quiescent"], true);
}

#[test]
fn main_agent_worker_bootstrap_rejects_an_existing_ungranted_claim() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let main_checkout = tmp.path().join("main-checkout");
    let worker_checkout = tmp.path().join("worker-checkout");
    fs::create_dir(&state_dir).expect("state");
    init_checkout(
        &main_checkout,
        "https://example.invalid/example/repository.git",
    );
    init_checkout(
        &worker_checkout,
        "https://example.invalid/example/repository.git",
    );
    seed_brokers_at(
        &state_dir,
        &[
            (
                "main-one",
                "main-incarnation-one",
                "main-private-capability-material-0000000001",
                main_checkout.as_path(),
                Some("enforce"),
            ),
            (
                "worker-one",
                "worker-incarnation-one",
                "worker-private-capability-material-0000000001",
                worker_checkout.as_path(),
                Some("enforce"),
            ),
        ],
    );
    let _main_capability = init_main_run(
        tmp.path(),
        &state_dir,
        &main_checkout,
        "main-one",
        "run-one",
    );
    let private_packet = json!({
        "schema_version": "main-agent.assignment-input.v1",
        "assignment_id": "assignment-bootstrap-mismatch",
        "task_summary": "Reject an ungranted pre-bootstrap claim",
        "task": {},
        "launch": {
            "agent": "codex",
            "cwd": worker_checkout,
            "title": null,
            "session_id": "worker-one",
            "coordination_mode": "enforce",
            "agent_args": []
        },
        "repository": "example/repository",
        "worktree": worker_checkout,
        "base_ref": "main",
        "scopes": ["docs/bootstrap-canary"],
        "durable_refs": []
    });
    insert_orchestration_assignment(
        &state_dir,
        "assignment-bootstrap-mismatch",
        json!({
            "schema_version": "agent-session.orchestration-assignment.v1",
            "assignment_id": "assignment-bootstrap-mismatch",
            "run_id": "run-one",
            "revision": 2,
            "state": "starting",
            "task_summary": "Reject an ungranted pre-bootstrap claim",
            "private_packet_digest": "replaced-by-fixture",
            "primary_manager": {
                "session_id": "main-one",
                "session_incarnation": "main-incarnation-one",
                "session_created_at": "2030-01-01T00:00:00Z"
            },
            "worker": {
                "session_id": "worker-one",
                "session_incarnation": "worker-incarnation-one",
                "session_created_at": "2030-01-01T00:00:00Z"
            },
            "collaborators": [],
            "borrowed_by": [],
            "repository": "example/repository",
            "worktree": worker_checkout,
            "base_ref": "main",
            "scopes": ["docs/bootstrap-canary"],
            "durable_refs": [],
            "checkpoint": null,
            "result_summary": null,
            "blocker_summary": null,
            "created_at": "2030-01-01T00:00:01Z",
            "updated_at": "2030-01-01T00:00:02Z"
        }),
        &private_packet,
    );
    let worker_capability = capability(&state_dir, "worker-one");
    let candidate_file = tmp.path().join("worker-ordinary-claim.json");
    candidate(
        &candidate_file,
        "docs/bootstrap-canary/",
        "Reject an ungranted pre-bootstrap claim",
    );
    let ordinary_claim = run(
        &worker_checkout,
        &[
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "work-context",
            "claim",
            "--session",
            "worker-one",
            "--file",
            candidate_file.to_str().expect("candidate"),
            "--capability-file",
            &worker_capability,
            "--idempotency-key",
            "worker-ordinary-claim-0001",
            "--format",
            "json",
        ],
    );
    assert_eq!(
        ordinary_claim.code,
        0,
        "stderr={}",
        ordinary_claim.stderr_text()
    );

    let rejected = run_main_agent(
        &worker_checkout,
        &[
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "bootstrap",
            "--idempotency-key",
            "worker-bootstrap-mismatch-0001",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_CAPABILITY_FILE", &worker_capability)],
    );
    assert_eq!(rejected.code, 65, "stderr={}", rejected.stderr_text());
    assert_eq!(
        rejected.stdout_json()["error"]["code"],
        "worker-bootstrap-claim-mismatch"
    );
    let coordination = load_coordination_registry(&state_dir);
    let retained_claim = coordination["claims"]
        .as_array()
        .expect("claims")
        .iter()
        .find(|claim| claim["session_id"] == "worker-one" && claim["state"] == "active")
        .expect("ordinary worker claim");
    assert!(
        !retained_claim["checkout_shell_grant"]
            .as_bool()
            .unwrap_or(false),
        "failed bootstrap must not upgrade an existing ordinary claim"
    );
    let orchestration = orchestration_registry(&state_dir);
    assert_eq!(
        orchestration["assignments"]["assignment-bootstrap-mismatch"]["state"],
        "starting"
    );
    assert_eq!(
        orchestration["assignments"]["assignment-bootstrap-mismatch"]["revision"],
        2
    );
}

#[test]
fn main_agent_worker_bootstrap_acquires_claim_and_checkpoints_from_packet() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let main_checkout = tmp.path().join("main-checkout");
    let worker_checkout = tmp.path().join("worker-checkout");
    fs::create_dir(&state_dir).expect("state");
    init_checkout(
        &main_checkout,
        "https://example.invalid/example/repository.git",
    );
    init_checkout(
        &worker_checkout,
        "https://example.invalid/example/repository.git",
    );
    seed_brokers_at(
        &state_dir,
        &[
            (
                "main-one",
                "main-incarnation-one",
                "main-private-capability-material-0000000001",
                main_checkout.as_path(),
                Some("enforce"),
            ),
            (
                "main-two",
                "main-incarnation-two",
                "main-private-capability-material-bootstrap-race",
                main_checkout.as_path(),
                Some("enforce"),
            ),
            (
                "worker-one",
                "worker-incarnation-one",
                "worker-private-capability-material-0000000001",
                worker_checkout.as_path(),
                Some("enforce"),
            ),
        ],
    );
    let main_capability = init_main_run(
        tmp.path(),
        &state_dir,
        &main_checkout,
        "main-one",
        "run-one",
    );
    seed_active_claim(
        &state_dir,
        "main-two",
        "main-incarnation-two",
        "main-two-bootstrap-race-claim",
    );
    rewrite_orchestration_registry(&state_dir, |registry| {
        let mut run_two = registry["runs"]["run-one"].clone();
        run_two["run_id"] = json!("run-two");
        run_two["revision"] = json!(1);
        run_two["objective_summary"] = json!("Receive bootstrap race handoff");
        run_two["controller"]["session_id"] = json!("main-two");
        run_two["controller"]["session_incarnation"] = json!("main-incarnation-two");
        registry["runs"]["run-two"] = run_two;
    });
    let private_packet = json!({
        "schema_version": "main-agent.assignment-input.v1",
        "assignment_id": "assignment-bootstrap",
        "task_summary": "Bootstrap worker acceptance",
        "task": {"private_note": "bootstrap-private-canary"},
        "launch": {
            "agent": "codex",
            "cwd": worker_checkout,
            "title": null,
            "session_id": "worker-one",
            "coordination_mode": "enforce",
            "agent_args": []
        },
        "repository": "example/repository",
        "worktree": worker_checkout,
        "base_ref": "main",
        "scopes": ["docs/bootstrap-canary"],
        "durable_refs": []
    });
    insert_orchestration_assignment(
        &state_dir,
        "assignment-bootstrap",
        json!({
            "schema_version": "agent-session.orchestration-assignment.v1",
            "assignment_id": "assignment-bootstrap",
            "run_id": "run-one",
            "revision": 2,
            "state": "starting",
            "task_summary": "Bootstrap worker acceptance",
            "private_packet_digest": "replaced-by-fixture",
            "primary_manager": {
                "session_id": "main-one",
                "session_incarnation": "main-incarnation-one",
                "session_created_at": "2030-01-01T00:00:00Z"
            },
            "worker": {
                "session_id": "worker-one",
                "session_incarnation": "worker-incarnation-one",
                "session_created_at": "2030-01-01T00:00:00Z"
            },
            "collaborators": [],
            "borrowed_by": [],
            "repository": "example/repository",
            "worktree": worker_checkout,
            "base_ref": "main",
            "scopes": ["docs/bootstrap-canary"],
            "durable_refs": [],
            "checkpoint": null,
            "result_summary": null,
            "blocker_summary": null,
            "created_at": "2030-01-01T00:00:01Z",
            "updated_at": "2030-01-01T00:00:02Z"
        }),
        &private_packet,
    );
    let worker_capability = capability(&state_dir, "worker-one");
    let args = [
        "--state-dir",
        state_dir.to_str().expect("state dir"),
        "bootstrap",
        "--idempotency-key",
        "worker-bootstrap-0001",
        "--format",
        "json",
    ];
    let missing_checkpoint_env = run_main_agent_without_checkpoint(
        &worker_checkout,
        &args,
        &[("AGENT_SESSION_CAPABILITY_FILE", &worker_capability)],
    );
    assert_ne!(
        missing_checkpoint_env.code,
        0,
        "a pre-B6 incarnation without its runtime-issued checkpoint environment must fail before bootstrap mutates: {}",
        missing_checkpoint_env.stdout_text()
    );
    assert_eq!(
        missing_checkpoint_env.stdout_json()["error"]["code"],
        "runtime-checkpoint-unavailable"
    );
    let coordination_registry: serde_json::Value = serde_json::from_slice(
        &fs::read(state_dir.join("coordination/registry.json")).expect("coordination registry"),
    )
    .expect("coordination registry json");
    assert!(
        coordination_registry["claims"]
            .as_array()
            .expect("claims")
            .iter()
            .all(|claim| claim["session_id"] != "worker-one" || claim["state"] != "active"),
        "checkpoint readiness must fail before the worker acquires a claim"
    );
    let worker_checkpoint = state_dir.join(format!(
        "sessions/worker-one/coordination/main-agent-checkpoint-{}.json",
        digest("worker-incarnation-one")
    ));
    let bootstrapped = run_main_agent(
        &worker_checkout,
        &args,
        &[
            ("AGENT_SESSION_CAPABILITY_FILE", &worker_capability),
            (
                "AGENT_SESSION_CHECKPOINT_FILE",
                worker_checkpoint.to_str().expect("checkpoint path"),
            ),
        ],
    );
    assert_eq!(
        bootstrapped.code,
        0,
        "stdout={} stderr={}",
        bootstrapped.stdout_text(),
        bootstrapped.stderr_text()
    );
    assert_eq!(
        data(&bootstrapped)["assignment"]["record"]["state"],
        "working"
    );
    assert_eq!(data(&bootstrapped)["assignment"]["record"]["revision"], 3);
    assert_eq!(
        data(&bootstrapped)["assignment"]["assignment_packet"]["task"]["private_note"],
        "bootstrap-private-canary"
    );
    assert_eq!(
        data(&bootstrapped)["assignment"]["record"]["worktree"],
        worker_checkout.to_string_lossy().as_ref(),
        "the absolute managed-worktree path remains durable routing metadata"
    );
    assert_eq!(
        data(&bootstrapped)["checkpoint_file"],
        state_dir
            .join(format!(
                "sessions/worker-one/coordination/main-agent-checkpoint-{}.json",
                digest("worker-incarnation-one")
            ))
            .to_string_lossy()
            .as_ref(),
        "bootstrap must return the runtime-issued checkpoint path for later authenticated checkpoints"
    );
    let coordination_registry: serde_json::Value = serde_json::from_slice(
        &fs::read(state_dir.join("coordination/registry.json")).expect("coordination registry"),
    )
    .expect("coordination registry json");
    let worker_claim = coordination_registry["claims"]
        .as_array()
        .expect("claims")
        .iter()
        .find(|claim| claim["session_id"] == "worker-one" && claim["state"] == "active")
        .expect("worker claim");
    assert_eq!(
        worker_claim["checkout_shell_grant"], true,
        "authenticated Main Agent bootstrap must mint the private checkout-shell grant"
    );
    assert!(
        worker_claim["worktrees"]
            .as_array()
            .expect("worker claim worktrees")
            .iter()
            .all(|value| value.as_str().is_some_and(|value| {
                value.starts_with("hmac-sha256:")
                    && !value.contains(worker_checkout.to_string_lossy().as_ref())
            })),
        "claim worktrees must contain only HMAC fingerprints: {worker_claim}"
    );
    let shown_claim = run(
        &worker_checkout,
        &[
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "work-context",
            "show",
            "--session",
            "worker-one",
            "--capability-file",
            &worker_capability,
            "--format",
            "json",
        ],
    );
    assert_eq!(shown_claim.code, 0, "stderr={}", shown_claim.stderr_text());
    assert!(
        data(&shown_claim).get("checkout_shell_grant").is_none(),
        "private admission grants must not enter public work-context output"
    );
    let diagnosed = run_main_agent(
        &main_checkout,
        &[
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "worker",
            "diagnose",
            "assignment-bootstrap",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_CAPABILITY_FILE", &main_capability)],
    );
    assert_eq!(diagnosed.code, 0, "stderr={}", diagnosed.stderr_text());
    assert_eq!(data(&diagnosed)["classification"], "healthy_progress");
    assert_eq!(
        data(&diagnosed)["coordination"]["claim_active"],
        true,
        "authenticated bootstrap claim is supervision evidence"
    );

    let replay = run_main_agent(
        &worker_checkout,
        &args,
        &[("AGENT_SESSION_CAPABILITY_FILE", &worker_capability)],
    );
    assert_eq!(replay.code, 0, "stderr={}", replay.stderr_text());
    assert_eq!(replay.stdout_text(), bootstrapped.stdout_text());

    let continuity_body = tmp.path().join("resume-guidance.md");
    fs::write(
        &continuity_body,
        "Unread exact-controller guidance must follow the resumed worker incarnation.",
    )
    .expect("resume guidance");
    let queued_guidance = run_main_agent(
        &main_checkout,
        &[
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "worker",
            "message",
            "assignment-bootstrap",
            "--body-file",
            continuity_body.to_str().expect("guidance body"),
            "--idempotency-key",
            "bootstrap-resume-guidance-0001",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_CAPABILITY_FILE", &main_capability)],
    );
    assert_eq!(
        queued_guidance.code,
        0,
        "stderr={}",
        queued_guidance.stderr_text()
    );
    let guidance_message_id = data(&queued_guidance)["message_id"]
        .as_str()
        .expect("guidance message id")
        .to_string();
    rewrite_registry(&state_dir, |registry| {
        let messages = registry["messages"].as_array_mut().expect("messages");
        let original = messages
            .iter()
            .find(|message| message["message_id"] == guidance_message_id)
            .expect("original guidance")
            .clone();
        let mut wrong_sender = original.clone();
        wrong_sender["message_id"] = json!("resume-guidance-wrong-sender");
        wrong_sender["sender_session_id"] = json!("unrelated-controller");
        wrong_sender["sender_incarnation"] = json!("unrelated-controller-incarnation");
        wrong_sender["recipient_incarnation"] = json!("worker-incarnation-two");
        messages.push(wrong_sender);
        let mut expired = original.clone();
        expired["message_id"] = json!("resume-guidance-expired");
        expired["expires_at_epoch"] = json!(0);
        messages.push(expired);
        let mut consumed = original;
        consumed["message_id"] = json!("resume-guidance-consumed");
        consumed["state"] = json!("read");
        messages.push(consumed);
    });

    let resumed_incarnation = "worker-incarnation-two";
    let resumed_capability = "worker-private-capability-material-0000000002";
    let worker_session_path = state_dir.join("sessions/worker-one/session.json");
    let mut worker_session: serde_json::Value =
        serde_json::from_slice(&fs::read(&worker_session_path).expect("worker session record"))
            .expect("worker session json");
    worker_session["runtime"]["launch_id"] = json!(resumed_incarnation);
    write_private_json(&worker_session_path, &worker_session);
    let worker_coordination_dir = state_dir.join("sessions/worker-one/coordination");
    let resumed_capability_file =
        worker_coordination_dir.join(format!("capability-{}", digest(resumed_incarnation)));
    fs::write(&resumed_capability_file, resumed_capability).expect("resumed capability");
    fs::set_permissions(&resumed_capability_file, fs::Permissions::from_mode(0o600))
        .expect("resumed capability mode");
    let resumed_checkpoint_file = worker_coordination_dir.join(format!(
        "main-agent-checkpoint-{}.json",
        digest(resumed_incarnation)
    ));
    fs::write(&resumed_checkpoint_file, []).expect("resumed checkpoint");
    fs::set_permissions(&resumed_checkpoint_file, fs::Permissions::from_mode(0o600))
        .expect("resumed checkpoint mode");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs();
    fs::write(
        worker_coordination_dir.join("heartbeat"),
        format!("{resumed_incarnation}:{now}\n"),
    )
    .expect("resumed heartbeat");
    rewrite_registry(&state_dir, |registry| {
        registry["brokers"]["worker-one"]["incarnation"] = json!(resumed_incarnation);
        registry["brokers"]["worker-one"]["capability_digest"] = json!(digest(resumed_capability));
        registry["brokers"]["worker-one"]["heartbeat_epoch"] = json!(now);
    });
    let resumed_capability_arg = resumed_capability_file.to_string_lossy().into_owned();
    let resumed_args = [
        "--state-dir",
        state_dir.to_str().expect("state dir"),
        "bootstrap",
        "--idempotency-key",
        "worker-bootstrap-resume-0001",
        "--format",
        "json",
    ];
    let before_resume = run_main_agent(
        &worker_checkout,
        &[
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "self",
            "show",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_CAPABILITY_FILE", &resumed_capability_arg)],
    );
    assert_eq!(data(&before_resume)["rebind_required"], true);
    let resumed = run_main_agent(
        &worker_checkout,
        &resumed_args,
        &[
            ("AGENT_SESSION_CAPABILITY_FILE", &resumed_capability_arg),
            (
                "AGENT_SESSION_CHECKPOINT_FILE",
                resumed_checkpoint_file.to_str().expect("checkpoint path"),
            ),
        ],
    );
    assert_eq!(
        resumed.code,
        0,
        "stdout={} stderr={}",
        resumed.stdout_text(),
        resumed.stderr_text()
    );
    assert_eq!(data(&resumed)["assignment"]["record"]["revision"], 4);
    assert_eq!(
        data(&resumed)["assignment"]["record"]["worker"]["session_incarnation"],
        resumed_incarnation
    );
    assert_eq!(
        data(&resumed)["assignment"]["record"]["previous_worker"]["session_incarnation"],
        "worker-incarnation-one",
        "resume bootstrap must preserve the superseded worker identity for continuity auditing"
    );
    let resumed_registry: serde_json::Value = serde_json::from_slice(
        &fs::read(state_dir.join("coordination/registry.json")).expect("coordination registry"),
    )
    .expect("coordination registry json");
    let claims = resumed_registry["claims"].as_array().expect("claims");
    assert!(claims.iter().any(|claim| {
        claim["session_incarnation"] == "worker-incarnation-one" && claim["state"] == "released"
    }));
    assert!(claims.iter().any(|claim| {
        claim["session_incarnation"] == resumed_incarnation && claim["state"] == "active"
    }));
    let carried_guidance = resumed_registry["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .find(|message| message["message_id"] == guidance_message_id)
        .expect("carried guidance");
    assert_eq!(carried_guidance["state"], "unread");
    assert_eq!(
        carried_guidance["recipient_incarnation"],
        resumed_incarnation
    );
    assert_eq!(
        carried_guidance["forwarded_from_incarnation"],
        "worker-incarnation-one"
    );
    assert!(
        carried_guidance["forwarded_at_epoch"].as_i64().is_some(),
        "carried guidance retains bounded provenance"
    );
    assert_eq!(
        carried_guidance["body"],
        "Unread exact-controller guidance must follow the resumed worker incarnation."
    );
    assert_eq!(
        carried_guidance["revision"], 2,
        "the retained message identity advances exactly once"
    );
    for message_id in ["resume-guidance-expired", "resume-guidance-consumed"] {
        let untouched = resumed_registry["messages"]
            .as_array()
            .expect("messages")
            .iter()
            .find(|message| message["message_id"] == message_id)
            .expect("non-forwardable message");
        assert_eq!(
            untouched["recipient_incarnation"], "worker-incarnation-one",
            "{message_id} must not cross the incarnation boundary"
        );
        assert!(
            untouched["forwarded_from_incarnation"].is_null(),
            "{message_id} must retain no false forwarding provenance"
        );
    }
    let unrelated = resumed_registry["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .find(|message| message["message_id"] == "resume-guidance-wrong-sender")
        .expect("unrelated message");
    assert_eq!(
        unrelated["recipient_incarnation"], resumed_incarnation,
        "fixture remains current-incarnation so diagnosis must filter by exact controller"
    );
    let resumed_diagnosis = run_main_agent(
        &main_checkout,
        &[
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "worker",
            "diagnose",
            "assignment-bootstrap",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_CAPABILITY_FILE", &main_capability)],
    );
    assert_eq!(
        resumed_diagnosis.code,
        0,
        "stdout={} stderr={}",
        resumed_diagnosis.stdout_text(),
        resumed_diagnosis.stderr_text()
    );
    assert_eq!(
        data(&resumed_diagnosis)["guidance"]["state"],
        "queued_unread",
        "only the exact controller's retained unread message is actionable guidance"
    );
    let reconcile_args = [
        "--state-dir",
        state_dir.to_str().expect("state dir"),
        "worker",
        "guidance-reconcile",
        "assignment-bootstrap",
        "--if-revision",
        "4",
        "--idempotency-key",
        "bootstrap-guidance-reconcile-0001",
        "--format",
        "json",
    ];
    let reconciled = run_main_agent(
        &main_checkout,
        &reconcile_args,
        &[("AGENT_SESSION_CAPABILITY_FILE", &main_capability)],
    );
    assert_eq!(
        reconciled.code,
        0,
        "stdout={} stderr={}",
        reconciled.stdout_text(),
        reconciled.stderr_text()
    );
    assert_eq!(data(&reconciled)["state"], "reconciled");
    assert_eq!(data(&reconciled)["message_identity_retained"], true);
    assert_eq!(data(&reconciled)["message_body_exposed"], false);
    assert_eq!(data(&reconciled)["message_marked_consumed"], false);
    let reconcile_replay = run_main_agent(
        &main_checkout,
        &reconcile_args,
        &[("AGENT_SESSION_CAPABILITY_FILE", &main_capability)],
    );
    assert_eq!(reconcile_replay.code, 0);
    assert_eq!(
        reconcile_replay.stdout_text(),
        reconciled.stdout_text(),
        "guidance reconciliation must be idempotent"
    );
    let after_reconcile_registry: serde_json::Value = serde_json::from_slice(
        &fs::read(state_dir.join("coordination/registry.json")).expect("coordination registry"),
    )
    .expect("coordination registry json");
    let retained_guidance = after_reconcile_registry["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .find(|message| message["message_id"] == guidance_message_id)
        .expect("retained guidance");
    assert_eq!(
        retained_guidance["state"], "unread",
        "reconciliation must not claim worker consumption"
    );
    let after_resume = run_main_agent(
        &worker_checkout,
        &[
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "self",
            "show",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_CAPABILITY_FILE", &resumed_capability_arg)],
    );
    assert_eq!(data(&after_resume)["rebind_required"], false);
    let resumed_replay = run_main_agent(
        &worker_checkout,
        &resumed_args,
        &[("AGENT_SESSION_CAPABILITY_FILE", &resumed_capability_arg)],
    );
    assert_eq!(resumed_replay.code, 0);
    assert_eq!(resumed_replay.stdout_text(), resumed.stdout_text());

    let raced_guidance_body = tmp.path().join("raced-resume-guidance.md");
    fs::write(
        &raced_guidance_body,
        "This old-controller message must remain on the prior incarnation after handoff.",
    )
    .expect("raced guidance");
    let raced_guidance = run_main_agent(
        &main_checkout,
        &[
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "worker",
            "message",
            "assignment-bootstrap",
            "--body-file",
            raced_guidance_body.to_str().expect("raced guidance body"),
            "--idempotency-key",
            "bootstrap-raced-guidance-0001",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_CAPABILITY_FILE", &main_capability)],
    );
    assert_eq!(raced_guidance.code, 0);
    let raced_message_id = data(&raced_guidance)["message_id"]
        .as_str()
        .expect("raced guidance message id")
        .to_string();
    let third_incarnation = "worker-incarnation-three";
    let third_capability = "worker-private-capability-material-0000000003";
    let mut third_worker_session: serde_json::Value =
        serde_json::from_slice(&fs::read(&worker_session_path).expect("worker session record"))
            .expect("worker session json");
    third_worker_session["runtime"]["launch_id"] = json!(third_incarnation);
    write_private_json(&worker_session_path, &third_worker_session);
    let third_capability_file =
        worker_coordination_dir.join(format!("capability-{}", digest(third_incarnation)));
    fs::write(&third_capability_file, third_capability).expect("third capability");
    fs::set_permissions(&third_capability_file, fs::Permissions::from_mode(0o600))
        .expect("third capability mode");
    let third_checkpoint_file = worker_coordination_dir.join(format!(
        "main-agent-checkpoint-{}.json",
        digest(third_incarnation)
    ));
    fs::write(&third_checkpoint_file, []).expect("third checkpoint");
    fs::set_permissions(&third_checkpoint_file, fs::Permissions::from_mode(0o600))
        .expect("third checkpoint mode");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs();
    fs::write(
        worker_coordination_dir.join("heartbeat"),
        format!("{third_incarnation}:{now}\n"),
    )
    .expect("third heartbeat");
    rewrite_registry(&state_dir, |registry| {
        registry["brokers"]["worker-one"]["incarnation"] = json!(third_incarnation);
        registry["brokers"]["worker-one"]["capability_digest"] = json!(digest(third_capability));
        registry["brokers"]["worker-one"]["heartbeat_epoch"] = json!(now);
    });
    let third_capability_arg = third_capability_file.to_string_lossy().into_owned();
    let guidance_barrier = tmp.path().join("bootstrap-guidance-race-barrier");
    fs::create_dir(&guidance_barrier).expect("guidance barrier");
    let raced_bootstrap = Command::new(bin::resolve("main-agent"))
        .current_dir(&worker_checkout)
        .args([
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "bootstrap",
            "--idempotency-key",
            "worker-bootstrap-race-0001",
            "--format",
            "json",
        ])
        .env("AGENT_SESSION_CAPABILITY_FILE", &third_capability_arg)
        .env("AGENT_SESSION_CHECKPOINT_FILE", &third_checkpoint_file)
        .env(
            "NILS_AGENT_SESSION_TEST_BOOTSTRAP_GUIDANCE_BARRIER_DIR",
            &guidance_barrier,
        )
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn raced bootstrap");
    let guidance_deadline = Instant::now() + Duration::from_secs(10);
    while !guidance_barrier.join("ready").is_file() {
        assert!(
            Instant::now() < guidance_deadline,
            "resumed bootstrap did not pause before its checkpoint"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let handed_off_during_bootstrap = run_main_agent(
        &main_checkout,
        &[
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "handoff",
            "assignment-bootstrap",
            "--to",
            "main-two@main-incarnation-two",
            "--if-revision",
            "4",
            "--idempotency-key",
            "bootstrap-guidance-race-handoff-0001",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_CAPABILITY_FILE", &main_capability)],
    );
    assert_eq!(
        handed_off_during_bootstrap.code,
        0,
        "stderr={}",
        handed_off_during_bootstrap.stderr_text()
    );
    fs::write(guidance_barrier.join("release"), b"continue").expect("release guidance barrier");
    let raced_bootstrap = raced_bootstrap
        .wait_with_output()
        .expect("raced bootstrap output");
    assert!(!raced_bootstrap.status.success());
    let raced_registry = load_coordination_registry(&state_dir);
    let raced_message = raced_registry["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .find(|message| message["message_id"] == raced_message_id)
        .expect("raced guidance message");
    assert_eq!(
        raced_message["recipient_incarnation"], resumed_incarnation,
        "the former controller's guidance must not cross the handoff boundary"
    );
    assert!(
        raced_message["forwarded_from_incarnation"].is_null(),
        "the losing bootstrap must leave no forwarding provenance"
    );
}

#[test]
fn main_agent_failed_preclaim_worker_is_cancelled_retired_and_reassigned_in_isolation() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let main_checkout = tmp.path().join("main-checkout");
    let failed_checkout = tmp.path().join("failed-checkout");
    let replacement_checkout = tmp.path().join("replacement-checkout");
    let unrelated_checkout = tmp.path().join("unrelated-checkout");
    fs::create_dir(&state_dir).expect("state");
    for checkout in [
        &main_checkout,
        &failed_checkout,
        &replacement_checkout,
        &unrelated_checkout,
    ] {
        init_checkout(checkout, "https://example.invalid/example/repository.git");
    }
    seed_brokers_at(
        &state_dir,
        &[
            (
                "main-one",
                "main-incarnation-one",
                "main-private-capability-material-0000000001",
                main_checkout.as_path(),
                Some("enforce"),
            ),
            (
                "worker-failed",
                "worker-failed-incarnation",
                "worker-failed-private-capability-0000000001",
                failed_checkout.as_path(),
                Some("enforce"),
            ),
            (
                "worker-unrelated",
                "worker-unrelated-incarnation",
                "worker-unrelated-private-capability-00000001",
                unrelated_checkout.as_path(),
                Some("enforce"),
            ),
        ],
    );
    let main_capability = init_main_run(
        tmp.path(),
        &state_dir,
        &main_checkout,
        "main-one",
        "run-one",
    );
    let failed_packet = json!({
        "schema_version": "main-agent.assignment-input.v1",
        "assignment_id": "assignment-failed",
        "task_summary": "Conflict before claim acquisition",
        "task": {},
        "launch": {
            "agent": "codex",
            "cwd": failed_checkout,
            "title": null,
            "session_id": "worker-failed",
            "coordination_mode": "enforce",
            "agent_args": []
        },
        "repository": "example/repository",
        "worktree": failed_checkout,
        "base_ref": "main",
        "scopes": ["crates/agent-session"],
        "durable_refs": []
    });
    insert_orchestration_assignment(
        &state_dir,
        "assignment-failed",
        json!({
            "schema_version": "agent-session.orchestration-assignment.v1",
            "assignment_id": "assignment-failed",
            "run_id": "run-one",
            "revision": 2,
            "state": "starting",
            "task_summary": "Conflict before claim acquisition",
            "private_packet_digest": "replaced-by-fixture",
            "primary_manager": {
                "session_id": "main-one",
                "session_incarnation": "main-incarnation-one",
                "session_created_at": "2030-01-01T00:00:00Z"
            },
            "worker": {
                "session_id": "worker-failed",
                "session_incarnation": "worker-failed-incarnation",
                "session_created_at": "2030-01-01T00:00:00Z"
            },
            "collaborators": [],
            "borrowed_by": [],
            "repository": "example/repository",
            "worktree": failed_checkout,
            "base_ref": "main",
            "scopes": ["crates/agent-session"],
            "durable_refs": [],
            "checkpoint": null,
            "result_summary": null,
            "blocker_summary": null,
            "created_at": "2030-01-01T00:00:01Z",
            "updated_at": "2030-01-01T00:00:02Z"
        }),
        &failed_packet,
    );
    let unrelated_packet = json!({
        "schema_version": "main-agent.assignment-input.v1",
        "assignment_id": "assignment-unrelated",
        "task_summary": "Unrelated active worker",
        "task": {},
        "launch": {
            "agent": "codex",
            "cwd": unrelated_checkout,
            "title": null,
            "session_id": "worker-unrelated",
            "coordination_mode": "enforce",
            "agent_args": []
        },
        "repository": "example/repository",
        "worktree": unrelated_checkout,
        "base_ref": "main",
        "scopes": ["docs/unrelated"],
        "durable_refs": []
    });
    insert_orchestration_assignment(
        &state_dir,
        "assignment-unrelated",
        json!({
            "schema_version": "agent-session.orchestration-assignment.v1",
            "assignment_id": "assignment-unrelated",
            "run_id": "run-one",
            "revision": 4,
            "state": "working",
            "task_summary": "Unrelated active worker",
            "private_packet_digest": "replaced-by-fixture",
            "primary_manager": {
                "session_id": "main-one",
                "session_incarnation": "main-incarnation-one",
                "session_created_at": "2030-01-01T00:00:00Z"
            },
            "worker": {
                "session_id": "worker-unrelated",
                "session_incarnation": "worker-unrelated-incarnation",
                "session_created_at": "2030-01-01T00:00:00Z"
            },
            "collaborators": [],
            "borrowed_by": [],
            "repository": "example/repository",
            "worktree": unrelated_checkout,
            "base_ref": "main",
            "scopes": ["docs/unrelated"],
            "durable_refs": [],
            "checkpoint": {
                "revision": 4,
                "summary": "Unrelated work continues",
                "next_action": "Continue",
                "updated_at": "2030-01-01T00:00:03Z"
            },
            "result_summary": null,
            "blocker_summary": null,
            "created_at": "2030-01-01T00:00:01Z",
            "updated_at": "2030-01-01T00:00:03Z"
        }),
        &unrelated_packet,
    );

    let worker_capability = capability(&state_dir, "worker-failed");
    let bootstrap = run_main_agent(
        &failed_checkout,
        &[
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "bootstrap",
            "--idempotency-key",
            "failed-bootstrap-0001",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_CAPABILITY_FILE", &worker_capability)],
    );
    assert_eq!(bootstrap.code, 65);
    assert_eq!(bootstrap.stdout_json()["error"]["code"], "claim-conflict");
    let migrated = orchestration_registry(&state_dir);
    assert_eq!(
        migrated["schema_version"],
        "agent-session.orchestration-registry.v3"
    );
    assert_eq!(
        migrated["assignments"]["assignment-failed"]["schema_version"],
        "agent-session.orchestration-assignment.v3"
    );

    let failed_worker_record_path = state_dir.join("sessions/worker-failed/session.json");
    let mut failed_worker_record: serde_json::Value = serde_json::from_slice(
        &fs::read(&failed_worker_record_path).expect("failed worker record"),
    )
    .expect("failed worker record json");
    failed_worker_record["provider_resume"] = json!({
        "provider": "codex",
        "session_id": "provider-resume-private-sentinel",
        "captured_at": "2030-01-01T00:00:00Z",
        "capture_method": "codex-session-meta",
        "resume_args": ["resume", "provider-resume-private-sentinel"],
        "private_extra": "provider-resume-extra-sentinel"
    });
    write_private_json(&failed_worker_record_path, &failed_worker_record);

    let supervised = run_main_agent(
        &main_checkout,
        &[
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "worker",
            "supervise",
            "assignment-failed",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_CAPABILITY_FILE", &main_capability)],
    );
    assert_eq!(supervised.code, 0, "stderr={}", supervised.stderr_text());
    assert_eq!(data(&supervised)["classification"], "pre_claim_failure");
    assert_eq!(
        data(&supervised)["last_proven_safe_state"]["reassignment_safe"],
        true
    );
    assert_eq!(
        data(&supervised)["last_proven_safe_state"]["provider_resume_preserved"],
        true
    );
    assert!(
        !supervised
            .stdout_text()
            .contains("provider-resume-private-sentinel")
            && !supervised
                .stdout_text()
                .contains("provider-resume-extra-sentinel"),
        "supervision must not expose raw provider resume metadata"
    );

    let stale_cancel = run_main_agent(
        &main_checkout,
        &[
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "worker",
            "cancel",
            "assignment-failed",
            "--if-revision",
            "2",
            "--reason",
            "replace failed pre-claim worker",
            "--idempotency-key",
            "stale-cancel-0001",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_CAPABILITY_FILE", &main_capability)],
    );
    assert_eq!(stale_cancel.code, 65);
    assert_eq!(
        stale_cancel.stdout_json()["error"]["code"],
        "orchestration-revision-conflict"
    );
    let coordination_path = state_dir.join("coordination/registry.json");
    let coordination_registry: serde_json::Value =
        serde_json::from_slice(&fs::read(&coordination_path).expect("coordination registry"))
            .expect("coordination registry json");
    let worker_broker = coordination_registry["brokers"]["worker-failed"].clone();
    for (case, expected_code) in [
        ("absent", "coordination-broker-unavailable"),
        ("mismatched", "coordination-broker-incarnation-conflict"),
        ("stopped", "coordination-broker-unavailable"),
        ("capability-missing", "coordination-broker-unavailable"),
    ] {
        rewrite_registry(&state_dir, |registry| match case {
            "absent" => {
                registry["brokers"]
                    .as_object_mut()
                    .expect("brokers")
                    .remove("worker-failed");
            }
            "mismatched" => {
                registry["brokers"]["worker-failed"]["incarnation"] =
                    json!("replacement-incarnation");
            }
            "stopped" => {
                registry["brokers"]["worker-failed"]["state"] = json!("stopped");
            }
            "capability-missing" => {
                registry["brokers"]["worker-failed"]["capability_digest"] = json!("");
            }
            _ => unreachable!(),
        });
        let refused = run_main_agent(
            &main_checkout,
            &[
                "--state-dir",
                state_dir.to_str().expect("state dir"),
                "worker",
                "cancel",
                "assignment-failed",
                "--if-revision",
                "3",
                "--reason",
                "broker evidence is not authoritative",
                "--idempotency-key",
                &format!("cancel-broker-{case}-0001"),
                "--format",
                "json",
            ],
            &[("AGENT_SESSION_CAPABILITY_FILE", &main_capability)],
        );
        assert_ne!(refused.code, 0, "outcome={}", refused.stdout_text());
        assert_eq!(refused.stdout_json()["error"]["code"], expected_code);
        rewrite_registry(&state_dir, |registry| {
            registry["brokers"]["worker-failed"] = worker_broker.clone();
        });
        let orchestration = orchestration_registry(&state_dir);
        assert_eq!(
            orchestration["assignments"]["assignment-failed"]["state"],
            "blocked"
        );
        assert_eq!(
            orchestration["assignments"]["assignment-failed"]["revision"],
            3
        );
    }

    let replacement_path = tmp.path().join("replacement.json");
    write_private_json(
        &replacement_path,
        &json!({
            "schema_version": "main-agent.assignment-input.v1",
            "assignment_id": "assignment-replacement",
            "task_summary": "Continue in a distinct clean worktree",
            "task": {},
            "launch": {
                "agent": "codex",
                "cwd": replacement_checkout,
                "title": null,
                "session_id": null,
                "coordination_mode": "enforce",
                "agent_args": []
            },
            "repository": "example/repository",
            "worktree": replacement_checkout,
            "base_ref": "main",
            "scopes": ["docs/replacement"],
            "durable_refs": []
        }),
    );
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let codex_bin = fake_agent(tmp.path(), "codex-worker");
    let tmux_arg = tmux_bin.to_string_lossy().into_owned();
    let tmux_log_arg = tmux_log.to_string_lossy().into_owned();
    let codex_arg = codex_bin.to_string_lossy().into_owned();
    let reassign_args = [
        "--state-dir",
        state_dir.to_str().expect("state dir"),
        "worker",
        "reassign",
        "assignment-failed",
        "--assignment-file",
        replacement_path.to_str().expect("replacement packet"),
        "--if-revision",
        "3",
        "--reason",
        "replace failed pre-claim worker",
        "--await-ready",
        "0",
        "--idempotency-key",
        "worker-reassign-0001",
        "--format",
        "json",
    ];
    let codex_home = tmp.path().join("codex-home");
    write_trusted_codex_config(&codex_home, &[&replacement_checkout]);
    let codex_home_arg = codex_home.to_string_lossy().into_owned();
    let reassign_env = [
        ("AGENT_SESSION_CAPABILITY_FILE", main_capability.as_str()),
        ("AGENT_SESSION_TMUX_BIN", tmux_arg.as_str()),
        ("AGENT_SESSION_CODEX_BIN", codex_arg.as_str()),
        ("AGENT_SESSION_FAKE_TMUX_LOG", tmux_log_arg.as_str()),
        ("CODEX_HOME", codex_home_arg.as_str()),
    ];
    let stale_reassign = run_main_agent(
        &main_checkout,
        &[
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "worker",
            "reassign",
            "assignment-failed",
            "--assignment-file",
            replacement_path.to_str().expect("replacement packet"),
            "--if-revision",
            "2",
            "--reason",
            "replace failed pre-claim worker",
            "--await-ready",
            "0",
            "--idempotency-key",
            "worker-reassign-stale-cancel-0001",
            "--format",
            "json",
        ],
        &reassign_env,
    );
    assert_eq!(
        stale_reassign.code,
        0,
        "stderr={}",
        stale_reassign.stderr_text()
    );
    assert_eq!(data(&stale_reassign)["state"], "failed");
    assert_eq!(data(&stale_reassign)["failed_stage"], "cancel");
    assert_eq!(
        data(&stale_reassign)["error"]["code"],
        "orchestration-revision-conflict"
    );
    let pane_process = match spawn_scoped_test_process_group() {
        Ok(process) => process,
        Err(reason) => {
            eprintln!("SKIP: Linux scoped-process integration capability unavailable: {reason}");
            return;
        }
    };
    let pane_pid_arg = pane_process.pid().to_string();
    let state_runtime_arg = state_dir.to_string_lossy().into_owned();
    let delete_failure_marker = tmp.path().join("delete-kill-failure-once");
    let delete_failure_marker_arg = delete_failure_marker.to_string_lossy().into_owned();
    let retire_failure_env = [
        ("AGENT_SESSION_CAPABILITY_FILE", main_capability.as_str()),
        ("AGENT_SESSION_TMUX_BIN", tmux_arg.as_str()),
        ("AGENT_SESSION_CODEX_BIN", codex_arg.as_str()),
        ("AGENT_SESSION_FAKE_TMUX_LOG", tmux_log_arg.as_str()),
        ("CODEX_HOME", codex_home_arg.as_str()),
        ("AGENT_SESSION_FAKE_TMUX_FAIL", "kill-session"),
        (
            "AGENT_SESSION_FAKE_TMUX_FAIL_ONCE_DIR",
            delete_failure_marker_arg.as_str(),
        ),
        ("AGENT_SESSION_FAKE_TMUX_PANE_PID", pane_pid_arg.as_str()),
        (
            "AGENT_SESSION_FAKE_TMUX_PROCESS_GROUP_ID",
            pane_pid_arg.as_str(),
        ),
        ("AGENT_SESSION_FAKE_TMUX_KEEP_PROCESS_GROUP", "1"),
        ("AGENT_SESSION_FAKE_TMUX_SESSION_ID", "$77"),
        ("AGENT_SESSION_FAKE_TMUX_AGENT_SESSION_ID", "worker-failed"),
        (
            "AGENT_SESSION_FAKE_TMUX_STATE_DIR",
            state_runtime_arg.as_str(),
        ),
        (
            "AGENT_SESSION_FAKE_TMUX_RUNTIME_ID",
            "worker-failed-incarnation",
        ),
    ];
    let retire_failed = run_main_agent(&main_checkout, &reassign_args, &retire_failure_env);
    assert_eq!(
        retire_failed.code,
        0,
        "stderr={}",
        retire_failed.stderr_text()
    );
    assert_eq!(data(&retire_failed)["state"], "failed");
    assert_eq!(data(&retire_failed)["failed_stage"], "retire");
    assert_eq!(
        data(&retire_failed)["last_proven_safe_state"]["assignment"]["state"],
        "cancelled"
    );
    assert_eq!(
        data(&retire_failed)["error"]["details"]["reason"],
        "kill-failed"
    );
    assert!(
        state_dir.join("sessions/worker-failed").exists(),
        "transient retirement failure must retain the exact worker fixture"
    );
    for retained in ["session.json", "activity.json"] {
        assert!(
            state_dir
                .join("sessions/worker-failed")
                .join(retained)
                .exists(),
            "the failed kill boundary must retain {retained}"
        );
    }
    assert!(
        Path::new(&worker_capability).exists(),
        "the failed kill boundary must retain the worker capability"
    );
    assert!(
        delete_failure_marker.is_dir(),
        "the one-shot kill failure injection must be consumed once"
    );
    assert!(
        pane_process.is_running(),
        "the failed kill-session invocation must retain the live runtime"
    );
    let reassign_receipt = orchestration_registry(&state_dir)["receipts"]
        ["main-one:main-incarnation-one:worker-reassign-0001"]
        .clone();
    assert!(
        !reassign_receipt
            .to_string()
            .contains("provider-resume-private-sentinel")
            && !reassign_receipt
                .to_string()
                .contains("provider-resume-extra-sentinel"),
        "durable reassign progress must not copy raw provider resume metadata"
    );
    let kill_attempts_after_failure = tmux_calls(&tmux_log)
        .into_iter()
        .filter(|call| {
            call.first().is_some_and(|arg| arg == "if-shell")
                && call.iter().any(|arg| arg.starts_with("kill-session -t "))
        })
        .count();
    assert_eq!(
        kill_attempts_after_failure, 1,
        "the first retirement must reach and fail one real kill-session boundary"
    );

    let retire_retry_env = [
        ("AGENT_SESSION_CAPABILITY_FILE", main_capability.as_str()),
        ("AGENT_SESSION_TMUX_BIN", tmux_arg.as_str()),
        ("AGENT_SESSION_CODEX_BIN", codex_arg.as_str()),
        ("AGENT_SESSION_FAKE_TMUX_LOG", tmux_log_arg.as_str()),
        ("CODEX_HOME", codex_home_arg.as_str()),
        ("AGENT_SESSION_FAKE_TMUX_PANE_PID", pane_pid_arg.as_str()),
        (
            "AGENT_SESSION_FAKE_TMUX_PROCESS_GROUP_ID",
            pane_pid_arg.as_str(),
        ),
        ("AGENT_SESSION_FAKE_TMUX_KEEP_PROCESS_GROUP", "1"),
        ("AGENT_SESSION_FAKE_TMUX_SESSION_ID", "$77"),
        ("AGENT_SESSION_FAKE_TMUX_AGENT_SESSION_ID", "worker-failed"),
        (
            "AGENT_SESSION_FAKE_TMUX_STATE_DIR",
            state_runtime_arg.as_str(),
        ),
        (
            "AGENT_SESSION_FAKE_TMUX_RUNTIME_ID",
            "worker-failed-incarnation",
        ),
    ];
    let reassigned = run_main_agent(&main_checkout, &reassign_args, &retire_retry_env);
    assert_eq!(reassigned.code, 0, "stderr={}", reassigned.stderr_text());
    assert_eq!(
        data(&reassigned)["state"],
        "reassigned",
        "outcome={}",
        data(&reassigned)
    );
    assert!(
        !state_dir.join("sessions/worker-failed").exists(),
        "the retry must retire the retained fixture through the real delete path"
    );
    assert!(
        !pane_process.is_running(),
        "the successful kill-session retry must terminate the exact runtime"
    );
    let kill_attempts_after_retry = tmux_calls(&tmux_log)
        .into_iter()
        .filter(|call| {
            call.first().is_some_and(|arg| arg == "if-shell")
                && call.iter().any(|arg| arg.starts_with("kill-session -t "))
        })
        .count();
    assert_eq!(
        kill_attempts_after_retry, 2,
        "the exact retry must invoke kill-session once more and then converge"
    );
    assert_eq!(
        data(&reassigned)["replacement_assignment_id"],
        "assignment-replacement"
    );
    assert!(!state_dir.join("sessions/worker-failed").exists());
    let replacement_session = data(&reassigned)["start"]["worker"]["session_id"]
        .as_str()
        .expect("derived replacement session")
        .to_string();
    assert!(
        state_dir
            .join("sessions")
            .join(&replacement_session)
            .exists()
    );
    assert!(
        state_dir.join("sessions/worker-unrelated").exists(),
        "reassignment must not touch an unrelated worker"
    );
    let calls_before_replay = tmux_calls(&tmux_log);
    let load_count_before_replay = calls_before_replay
        .iter()
        .filter(|call| call.first().is_some_and(|arg| arg == "load-buffer"))
        .count();
    let paste_count_before_replay = calls_before_replay
        .iter()
        .filter(|call| call.first().is_some_and(|arg| arg == "paste-buffer"))
        .count();
    let enter_count_before_replay = calls_before_replay
        .iter()
        .filter(|call| {
            call.first().is_some_and(|arg| arg == "send-keys")
                && call.last().is_some_and(|arg| arg == "Enter")
        })
        .count();
    assert_eq!(
        load_count_before_replay, 1,
        "the replacement prompt must be loaded exactly once"
    );
    assert_eq!(
        paste_count_before_replay, 1,
        "the replacement prompt must be pasted exactly once"
    );
    assert_eq!(
        enter_count_before_replay, 1,
        "the replacement prompt must be submitted exactly once"
    );
    fs::write(
        replacement_checkout.join("legitimate-worker-progress"),
        "dirty after start",
    )
    .expect("replacement progress");
    let replay = run_main_agent(&main_checkout, &reassign_args, &reassign_env);
    assert_eq!(replay.code, 0, "stderr={}", replay.stderr_text());
    assert_eq!(data(&replay)["state"], "reassigned");
    let calls_after_replay = tmux_calls(&tmux_log);
    let load_count_after_replay = calls_after_replay
        .iter()
        .filter(|call| call.first().is_some_and(|arg| arg == "load-buffer"))
        .count();
    let paste_count_after_replay = calls_after_replay
        .iter()
        .filter(|call| call.first().is_some_and(|arg| arg == "paste-buffer"))
        .count();
    let enter_count_after_replay = calls_after_replay
        .iter()
        .filter(|call| {
            call.first().is_some_and(|arg| arg == "send-keys")
                && call.last().is_some_and(|arg| arg == "Enter")
        })
        .count();
    assert_eq!(
        load_count_after_replay, load_count_before_replay,
        "an idempotent reassign retry must not reload the prompt"
    );
    assert_eq!(
        paste_count_after_replay, paste_count_before_replay,
        "an idempotent reassign retry must not repaste the prompt"
    );
    assert_eq!(
        enter_count_after_replay, enter_count_before_replay,
        "an idempotent reassign retry must not resend the prompt or Enter"
    );
    let registry: serde_json::Value = serde_json::from_slice(
        &fs::read(state_dir.join("orchestration/registry.json")).expect("registry"),
    )
    .expect("registry json");
    assert_eq!(
        registry["assignments"]["assignment-failed"]["state"],
        "cancelled"
    );
    assert!(
        registry["assignments"]["assignment-failed"]["blocker_summary"]
            .as_str()
            .unwrap_or_default()
            .contains("replace failed pre-claim worker")
    );
    assert_eq!(
        registry["assignments"]["assignment-unrelated"]["state"],
        "working"
    );
}

struct StoppedPostClaimFixture {
    _tmp: tempfile::TempDir,
    state_dir: PathBuf,
    checkout: PathBuf,
    main_capability: String,
    worker_capability: String,
    tmux_bin: PathBuf,
    tmux_log: PathBuf,
}

impl StoppedPostClaimFixture {
    fn new() -> Self {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let state_dir = tmp.path().join("state");
        let checkout = tmp.path().join("main-checkout");
        let worker_checkout = tmp.path().join("worker-checkout");
        fs::create_dir(&state_dir).expect("state");
        init_checkout(&checkout, "https://example.invalid/example/repository.git");
        init_checkout(
            &worker_checkout,
            "https://example.invalid/example/repository.git",
        );
        seed_brokers_at(
            &state_dir,
            &[
                (
                    "main-one",
                    "main-incarnation-one",
                    "main-private-capability-postclaim-focused",
                    checkout.as_path(),
                    Some("enforce"),
                ),
                (
                    "worker-stopped",
                    "worker-stopped-incarnation",
                    "worker-private-capability-postclaim-focused",
                    worker_checkout.as_path(),
                    Some("enforce"),
                ),
            ],
        );
        let main_capability =
            init_main_run(tmp.path(), &state_dir, &checkout, "main-one", "run-one");
        let packet = json!({
            "schema_version": "main-agent.assignment-input.v1",
            "assignment_id": "assignment-stopped",
            "task_summary": "Focused stopped post-claim worker",
            "task": {},
            "launch": {
                "agent": "codex",
                "cwd": worker_checkout,
                "title": null,
                "session_id": "worker-stopped",
                "coordination_mode": "enforce",
                "agent_args": []
            },
            "repository": "example/repository",
            "worktree": worker_checkout,
            "base_ref": "main",
            "scopes": ["docs/stopped-focused"],
            "durable_refs": []
        });
        insert_orchestration_assignment(
            &state_dir,
            "assignment-stopped",
            json!({
                "schema_version": "agent-session.orchestration-assignment.v1",
                "assignment_id": "assignment-stopped",
                "run_id": "run-one",
                "revision": 2,
                "state": "starting",
                "task_summary": "Focused stopped post-claim worker",
                "private_packet_digest": "replaced-by-fixture",
                "primary_manager": {
                    "session_id": "main-one",
                    "session_incarnation": "main-incarnation-one",
                    "session_created_at": "2030-01-01T00:00:00Z"
                },
                "worker": {
                    "session_id": "worker-stopped",
                    "session_incarnation": "worker-stopped-incarnation",
                    "session_created_at": "2030-01-01T00:00:00Z"
                },
                "collaborators": [],
                "borrowed_by": [],
                "repository": "example/repository",
                "worktree": worker_checkout,
                "base_ref": "main",
                "scopes": ["docs/stopped-focused"],
                "durable_refs": [],
                "checkpoint": null,
                "result_summary": null,
                "blocker_summary": null,
                "created_at": "2030-01-01T00:00:01Z",
                "updated_at": "2030-01-01T00:00:02Z"
            }),
            &packet,
        );
        let worker_capability = capability(&state_dir, "worker-stopped");
        let bootstrapped = run_main_agent(
            &worker_checkout,
            &[
                "--state-dir",
                state_dir.to_str().expect("state dir"),
                "bootstrap",
                "--idempotency-key",
                "focused-postclaim-bootstrap-0001",
                "--format",
                "json",
            ],
            &[("AGENT_SESSION_CAPABILITY_FILE", &worker_capability)],
        );
        assert_eq!(
            bootstrapped.code,
            0,
            "stderr={}",
            bootstrapped.stderr_text()
        );
        let worker_record_path = state_dir.join("sessions/worker-stopped/session.json");
        let mut record: serde_json::Value =
            serde_json::from_slice(&fs::read(&worker_record_path).expect("worker record"))
                .expect("worker json");
        record["delete_tmux_identity"] = json!({
            "launch_id": "worker-stopped-incarnation",
            "session_id": "$91",
            "pane_id": "%91",
            "pane_pid": 2_000_000_000u32,
            "process_group_id": 2_000_000_000u32
        });
        write_private_json(&worker_record_path, &record);
        let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
        Self {
            _tmp: tmp,
            state_dir,
            checkout,
            main_capability,
            worker_capability,
            tmux_bin,
            tmux_log,
        }
    }

    fn reconcile_args(&self) -> [&str; 13] {
        [
            "--state-dir",
            self.state_dir.to_str().expect("state dir"),
            "worker",
            "reconcile-stopped",
            "assignment-stopped",
            "--if-revision",
            "3",
            "--reason",
            "focused stopped post-claim worker",
            "--idempotency-key",
            "focused-postclaim-terminalize-0001",
            "--format",
            "json",
        ]
    }

    fn envs(&self) -> [(&str, &str); 5] {
        [
            (
                "AGENT_SESSION_CAPABILITY_FILE",
                self.main_capability.as_str(),
            ),
            (
                "AGENT_SESSION_TMUX_BIN",
                self.tmux_bin.to_str().expect("tmux bin"),
            ),
            (
                "AGENT_SESSION_FAKE_TMUX_LOG",
                self.tmux_log.to_str().expect("tmux log"),
            ),
            ("AGENT_SESSION_FAKE_TMUX_ABSENT", "1"),
            ("AGENT_SESSION_CODEX_BIN", "/bin/false"),
        ]
    }

    fn run_reconcile(&self) -> CmdOutput {
        run_main_agent(&self.checkout, &self.reconcile_args(), &self.envs())
    }

    fn run_reconcile_with_extra_env(&self, extra: &[(&str, &str)]) -> CmdOutput {
        let mut env = self.envs().to_vec();
        env.extend_from_slice(extra);
        run_main_agent(&self.checkout, &self.reconcile_args(), &env)
    }

    fn spawn_reconcile_at_barrier(&self, barrier: &Path, stage: &str) -> Child {
        let mut command = Command::new(bin::resolve("main-agent"));
        command
            .current_dir(&self.checkout)
            .args(self.reconcile_args())
            .envs(self.envs())
            .env(
                "NILS_AGENT_SESSION_TEST_RECONCILE_STOPPED_BARRIER_STAGE",
                stage,
            )
            .env(
                "NILS_AGENT_SESSION_TEST_RECONCILE_STOPPED_BARRIER_DIR",
                barrier,
            )
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command.spawn().expect("spawn reconcile-stopped")
    }
}

fn wait_for_barrier(barrier: &Path) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !barrier.join("ready").is_file() {
        assert!(
            Instant::now() < deadline,
            "reconcile-stopped did not reach its stage barrier"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

struct KillChild(Option<Child>);

impl Drop for KillChild {
    fn drop(&mut self) {
        if let Some(child) = self.0.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn terminate_at_barrier(child: Child, label: &str) {
    let mut child = KillChild(Some(child));
    child
        .0
        .as_mut()
        .expect(label)
        .kill()
        .unwrap_or_else(|error| panic!("kill {label}: {error}"));
    child
        .0
        .as_mut()
        .expect(label)
        .wait()
        .unwrap_or_else(|error| panic!("wait {label}: {error}"));
    child.0 = None;
}

fn post_json_over_http(address: &str, path: &str, token: &str) -> serde_json::Value {
    let mut stream = TcpStream::connect(address).expect("connect HTTP server");
    let request = format!(
        "POST {path} HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}"
    );
    stream
        .write_all(request.as_bytes())
        .expect("write HTTP request");
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .expect("read HTTP response");
    let body = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| &response[index + 4..])
        .expect("HTTP response body");
    serde_json::from_slice(body).expect("HTTP JSON response")
}

struct ExhaustedReadinessRuntimeStopFixture {
    tmp: tempfile::TempDir,
    state_dir: PathBuf,
    state_arg: String,
    main_checkout: PathBuf,
    worker_checkout: PathBuf,
    main_capability: String,
    main_two_capability: String,
    main_three_capability: String,
    main_four_capability: String,
    worker_start_receipt: serde_json::Value,
    runtime: TestProcessGroup,
    tmux_log: PathBuf,
    tmux_arg: String,
    tmux_log_arg: String,
    runtime_pid_arg: String,
    state_runtime_arg: String,
}

impl ExhaustedReadinessRuntimeStopFixture {
    fn new() -> Self {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let state_dir = tmp.path().join("state");
        let state_arg = state_dir.to_string_lossy().into_owned();
        let main_checkout = tmp.path().join("main-checkout");
        let worker_checkout = tmp.path().join("worker-checkout");
        fs::create_dir(&state_dir).expect("state");
        init_checkout(
            &main_checkout,
            "https://example.invalid/example/repository.git",
        );
        init_checkout(
            &worker_checkout,
            "https://example.invalid/example/repository.git",
        );
        seed_brokers_at(
            &state_dir,
            &[
                (
                    "main-one",
                    "main-incarnation-one",
                    "main-private-capability-b3-runtime-stop",
                    main_checkout.as_path(),
                    Some("enforce"),
                ),
                (
                    "worker-exhausted",
                    "worker-exhausted-incarnation",
                    "worker-private-capability-b3-runtime-stop",
                    worker_checkout.as_path(),
                    Some("enforce"),
                ),
                (
                    "main-two",
                    "main-incarnation-two",
                    "main-private-capability-b3-runtime-stop-two",
                    main_checkout.as_path(),
                    Some("enforce"),
                ),
                (
                    "main-three",
                    "main-incarnation-three",
                    "main-private-capability-b3-runtime-stop-three",
                    main_checkout.as_path(),
                    Some("enforce"),
                ),
                (
                    "main-four",
                    "main-incarnation-four",
                    "main-private-capability-b3-runtime-stop-four",
                    main_checkout.as_path(),
                    Some("enforce"),
                ),
            ],
        );
        let main_capability = init_main_run(
            tmp.path(),
            &state_dir,
            &main_checkout,
            "main-one",
            "run-one",
        );
        seed_active_claim(
            &state_dir,
            "main-two",
            "main-incarnation-two",
            "main-two-claim",
        );
        let main_two_capability = capability(&state_dir, "main-two");
        seed_active_claim(
            &state_dir,
            "main-three",
            "main-incarnation-three",
            "main-three-claim",
        );
        let main_three_capability = capability(&state_dir, "main-three");
        seed_active_claim(
            &state_dir,
            "main-four",
            "main-incarnation-four",
            "main-four-claim",
        );
        let main_four_capability = capability(&state_dir, "main-four");
        rewrite_orchestration_registry(&state_dir, |registry| {
            let mut controller = registry["runs"]["run-one"]["controller"].clone();
            controller["session_id"] = json!("main-two");
            controller["session_incarnation"] = json!("main-incarnation-two");
            registry["runs"]["run-two"] = json!({
                "schema_version": "agent-session.orchestration-run.v1",
                "run_id": "run-two",
                "revision": 1,
                "state": "active",
                "tier": "L0",
                "objective_summary": "Runtime stop ownership transfer fence",
                "objective_packet_digest":
                    registry["runs"]["run-one"]["objective_packet_digest"].clone(),
                "controller": controller,
                "durable_refs": [],
                "checkpoint": null,
                "created_at": "2030-01-01T00:00:00Z",
                "updated_at": "2030-01-01T00:00:00Z"
            });
            controller["session_id"] = json!("main-three");
            controller["session_incarnation"] = json!("main-incarnation-three");
            registry["runs"]["run-three"] = json!({
                "schema_version": "agent-session.orchestration-run.v1",
                "run_id": "run-three",
                "revision": 1,
                "state": "active",
                "tier": "L0",
                "objective_summary": "Repeated runtime stop ownership transfer fence",
                "objective_packet_digest":
                    registry["runs"]["run-one"]["objective_packet_digest"].clone(),
                "controller": controller,
                "durable_refs": [],
                "checkpoint": null,
                "created_at": "2030-01-01T00:00:00Z",
                "updated_at": "2030-01-01T00:00:00Z"
            });
            controller["session_id"] = json!("main-four");
            controller["session_incarnation"] = json!("main-incarnation-four");
            registry["runs"]["run-four"] = json!({
                "schema_version": "agent-session.orchestration-run.v1",
                "run_id": "run-four",
                "revision": 1,
                "state": "active",
                "tier": "L0",
                "objective_summary": "Partial runtime stop transfer recovery",
                "objective_packet_digest":
                    registry["runs"]["run-one"]["objective_packet_digest"].clone(),
                "controller": controller,
                "durable_refs": [],
                "checkpoint": null,
                "created_at": "2030-01-01T00:00:00Z",
                "updated_at": "2030-01-01T00:00:00Z"
            });
        });
        let assignment_packet = json!({
            "schema_version": "main-agent.assignment-input.v1",
            "assignment_id": "assignment-exhausted",
            "task_summary": "Readiness exhausted before worker bootstrap",
            "task": {},
            "launch": {
                "agent": "codex",
                "cwd": worker_checkout,
                "title": null,
                "session_id": "worker-exhausted",
                "coordination_mode": "enforce",
                "agent_args": []
            },
            "repository": "example/repository",
            "worktree": worker_checkout,
            "base_ref": "main",
            "scopes": ["docs/exhausted"],
            "durable_refs": []
        });
        insert_orchestration_assignment(
            &state_dir,
            "assignment-exhausted",
            json!({
                "schema_version": "agent-session.orchestration-assignment.v1",
                "assignment_id": "assignment-exhausted",
                "run_id": "run-one",
                "revision": 3,
                "state": "starting",
                "task_summary": "Readiness exhausted before worker bootstrap",
                "private_packet_digest": "replaced-by-fixture",
                "primary_manager": {
                    "session_id": "main-one",
                    "session_incarnation": "main-incarnation-one",
                    "session_created_at": "2030-01-01T00:00:00Z"
                },
                "worker": {
                    "session_id": "worker-exhausted",
                    "session_incarnation": "worker-exhausted-incarnation",
                    "session_created_at": "2030-01-01T00:00:00Z"
                },
                "collaborators": [],
                "borrowed_by": [],
                "repository": "example/repository",
                "worktree": worker_checkout,
                "base_ref": "main",
                "scopes": ["docs/exhausted"],
                "durable_refs": [],
                "checkpoint": null,
                "result_summary": null,
                "blocker_summary": null,
                "submit_recovery": {
                    "schema_version": "main-agent.submit-recovery.v1",
                    "attempt_id": "b3-exhausted-attempt",
                    "origin": "automatic",
                    "run_id": "run-one",
                    "controller": {
                        "session_id": "main-one",
                        "session_incarnation": "main-incarnation-one",
                        "session_created_at": "2030-01-01T00:00:00Z"
                    },
                    "session_incarnation": "worker-exhausted-incarnation",
                    "reserved_revision": 3,
                    "state": "failed",
                    "attempt_count": 1,
                    "result": "checkpoint-timeout",
                    "attempted_at": "2030-01-01T00:00:01Z",
                    "updated_at": "2030-01-01T00:00:02Z"
                },
                "created_at": "2030-01-01T00:00:00Z",
                "updated_at": "2030-01-01T00:00:02Z"
            }),
            &assignment_packet,
        );
        let unrelated_packet = json!({
            "schema_version": "main-agent.assignment-input.v1",
            "assignment_id": "assignment-unrelated",
            "task_summary": "Unrelated submitted lane",
            "task": {},
            "launch": {
                "agent": "codex",
                "cwd": worker_checkout,
                "title": null,
                "session_id": null,
                "coordination_mode": "enforce",
                "agent_args": []
            },
            "repository": "example/repository",
            "worktree": worker_checkout,
            "base_ref": "main",
            "scopes": ["docs/unrelated"],
            "durable_refs": []
        });
        insert_orchestration_assignment(
            &state_dir,
            "assignment-unrelated",
            json!({
                "schema_version": "agent-session.orchestration-assignment.v1",
                "assignment_id": "assignment-unrelated",
                "run_id": "run-one",
                "revision": 1,
                "state": "submitted",
                "task_summary": "Unrelated submitted lane",
                "private_packet_digest": "replaced-by-fixture",
                "primary_manager": {
                    "session_id": "main-one",
                    "session_incarnation": "main-incarnation-one",
                    "session_created_at": "2030-01-01T00:00:00Z"
                },
                "worker": null,
                "collaborators": [],
                "borrowed_by": [],
                "repository": "example/repository",
                "worktree": worker_checkout,
                "base_ref": "main",
                "scopes": ["docs/unrelated"],
                "durable_refs": [],
                "checkpoint": null,
                "result_summary": "Reviewed unrelated result",
                "blocker_summary": null,
                "submit_recovery": null,
                "created_at": "2030-01-01T00:00:00Z",
                "updated_at": "2030-01-01T00:00:00Z"
            }),
            &unrelated_packet,
        );
        let worker_start_receipt = json!({
            "principal_session_id": "main-one",
            "principal_incarnation": "main-incarnation-one",
            "operation": "worker-start",
            "request_digest": "b3-worker-start-request",
            "outcome": {
                "schema_version": "main-agent.worker-start-result.v1",
                "assignment": {
                    "assignment_id": "assignment-exhausted",
                    "revision": 3,
                    "state": "starting"
                },
                "worker": {
                    "session_id": "worker-exhausted",
                    "session_incarnation": "worker-exhausted-incarnation"
                },
                "readiness": {
                    "state": "readiness_failed",
                    "assignment_state": "starting",
                    "worker_launched": true,
                    "delivery": {
                        "state": "unverified",
                        "transport_state": "submit-key-recovery-succeeded",
                        "proof": "worker-checkpoint-timeout"
                    },
                    "submit_key_recovery": {
                        "eligible": true,
                        "attempted": true,
                        "attempt_count": 1,
                        "result": "checkpoint-timeout"
                    },
                    "automatic_retry_safe": false
                }
            },
            "created_at_epoch": 1
        });
        rewrite_orchestration_registry(&state_dir, |registry| {
            registry["receipts"]["main-one:main-incarnation-one:b3-worker-start"] =
                worker_start_receipt.clone();
        });

        let runtime = seed_live_runtime_identity(
            &state_dir,
            "worker-exhausted",
            "worker-exhausted-incarnation",
            77,
        );
        let runtime_pid_arg = runtime.pid().to_string();
        let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
        let tmux_arg = tmux_bin.to_string_lossy().into_owned();
        let tmux_log_arg = tmux_log.to_string_lossy().into_owned();
        let state_runtime_arg = state_dir.to_string_lossy().into_owned();
        Self {
            tmp,
            state_dir,
            state_arg,
            main_checkout,
            worker_checkout,
            main_capability,
            main_two_capability,
            main_three_capability,
            main_four_capability,
            worker_start_receipt,
            runtime,
            tmux_log,
            tmux_arg,
            tmux_log_arg,
            runtime_pid_arg,
            state_runtime_arg,
        }
    }

    fn envs(&self) -> [(&str, &str); 10] {
        [
            (
                "AGENT_SESSION_CAPABILITY_FILE",
                self.main_capability.as_str(),
            ),
            ("AGENT_SESSION_TMUX_BIN", self.tmux_arg.as_str()),
            ("AGENT_SESSION_FAKE_TMUX_LOG", self.tmux_log_arg.as_str()),
            (
                "AGENT_SESSION_FAKE_TMUX_PANE_PID",
                self.runtime_pid_arg.as_str(),
            ),
            (
                "AGENT_SESSION_FAKE_TMUX_PROCESS_GROUP_ID",
                self.runtime_pid_arg.as_str(),
            ),
            ("AGENT_SESSION_FAKE_TMUX_KEEP_PROCESS_GROUP", "1"),
            ("AGENT_SESSION_FAKE_TMUX_SESSION_ID", "$77"),
            (
                "AGENT_SESSION_FAKE_TMUX_AGENT_SESSION_ID",
                "worker-exhausted",
            ),
            (
                "AGENT_SESSION_FAKE_TMUX_STATE_DIR",
                self.state_runtime_arg.as_str(),
            ),
            (
                "AGENT_SESSION_FAKE_TMUX_RUNTIME_ID",
                "worker-exhausted-incarnation",
            ),
        ]
    }

    fn stop_args(&self) -> [&str; 13] {
        [
            "--state-dir",
            self.state_arg.as_str(),
            "worker",
            "stop-runtime",
            "assignment-exhausted",
            "--worker-incarnation",
            "worker-exhausted-incarnation",
            "--if-revision",
            "3",
            "--idempotency-key",
            "b3-stop-runtime-0001",
            "--format",
            "json",
        ]
    }

    fn spawn_stop_at(&self, stage: &str, barrier: &Path) -> Child {
        let mut command = Command::new(bin::resolve("main-agent"));
        command
            .args(self.stop_args())
            .current_dir(&self.main_checkout)
            .env("NILS_AGENT_SESSION_TEST_RUNTIME_STOP_BARRIER_STAGE", stage)
            .env("NILS_AGENT_SESSION_TEST_RUNTIME_STOP_BARRIER_DIR", barrier)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in self.envs() {
            command.env(key, value);
        }
        command.spawn().expect("spawn runtime stop")
    }
}

#[test]
fn runtime_stop_projects_typed_executable_action() {
    let fixture = ExhaustedReadinessRuntimeStopFixture::new();
    let supervised = run_main_agent(
        &fixture.main_checkout,
        &[
            "--state-dir",
            fixture.state_arg.as_str(),
            "worker",
            "supervise",
            "assignment-exhausted",
            "--format",
            "json",
        ],
        &fixture.envs(),
    );
    assert_eq!(supervised.code, 0, "outcome={}", supervised.stdout_text());
    assert_eq!(
        data(&supervised)["schema_version"],
        "main-agent.worker-supervise-result.v3"
    );
    assert_eq!(
        data(&supervised)["classification"],
        "readiness_stop_required"
    );
    assert_eq!(
        data(&supervised)["recovery_action"]["kind"],
        "exact_worker_runtime_stop"
    );
    assert_eq!(data(&supervised)["recovery_action"]["executable"], true);
    assert_eq!(
        data(&supervised)["last_proven_safe_state"]["runtime_stop"]["in_flight"],
        false
    );
}

#[test]
fn runtime_stop_marker_first_crash_fences_worker_claim_and_is_adoptable() {
    let fixture = ExhaustedReadinessRuntimeStopFixture::new();
    let barrier = fixture.tmp.path().join("runtime-stop-marker-first");
    let child = fixture.spawn_stop_at("after_session_fence", &barrier);
    wait_for_barrier(&barrier);
    terminate_at_barrier(child, "marker-first runtime stop");

    let registry = orchestration_registry(&fixture.state_dir);
    assert!(
        registry["assignments"]["assignment-exhausted"]["runtime_stop"].is_null(),
        "the session fence must commit before the orchestration reservation"
    );
    assert!(
        registry["receipts"]["main-one:main-incarnation-one:b3-stop-runtime-0001"].is_null(),
        "the session fence must commit before the progress receipt"
    );
    let handoff = run_main_agent(
        &fixture.main_checkout,
        &[
            "--state-dir",
            fixture.state_arg.as_str(),
            "handoff",
            "assignment-exhausted",
            "--to",
            "main-two@main-incarnation-two",
            "--if-revision",
            "3",
            "--idempotency-key",
            "marker-first-handoff-0001",
            "--format",
            "json",
        ],
        &fixture.envs(),
    );
    assert_eq!(handoff.code, 69);
    assert_eq!(
        handoff.stdout_json()["error"]["code"],
        "worker-runtime-stop-in-flight"
    );

    let main_session = fixture.state_dir.join("sessions/main-one");
    let stale_main_session = fixture.state_dir.join("stale-main-one");
    fs::rename(&main_session, &stale_main_session).expect("make prior manager orphaned");
    let adopt = run_main_agent(
        &fixture.main_checkout,
        &[
            "--state-dir",
            fixture.state_arg.as_str(),
            "adopt",
            "assignment-exhausted",
            "--if-revision",
            "3",
            "--idempotency-key",
            "marker-first-adopt-0001",
            "--format",
            "json",
        ],
        &[(
            "AGENT_SESSION_CAPABILITY_FILE",
            fixture.main_two_capability.as_str(),
        )],
    );
    assert_eq!(adopt.code, 69);
    assert_eq!(
        adopt.stdout_json()["error"]["code"],
        "worker-runtime-stop-in-flight"
    );
    fs::rename(&stale_main_session, &main_session).expect("restore original manager");

    let worker_claim_candidate = fixture.tmp.path().join("marker-first-worker-claim.json");
    candidate(
        &worker_claim_candidate,
        "docs/exhausted",
        "Marker-first runtime stop must reject worker mutation authority",
    );
    let claim = run(
        &fixture.worker_checkout,
        &[
            "--state-dir",
            fixture.state_arg.as_str(),
            "work-context",
            "claim",
            "--session",
            "worker-exhausted",
            "--file",
            worker_claim_candidate.to_str().expect("candidate path"),
            "--capability-file",
            &capability(&fixture.state_dir, "worker-exhausted"),
            "--idempotency-key",
            "marker-first-worker-claim-0001",
            "--format",
            "json",
        ],
    );
    assert_eq!(claim.code, 65);
    assert_eq!(
        claim.stdout_json()["error"]["code"],
        "worker-runtime-stop-fenced"
    );

    let stopped = run_main_agent(
        &fixture.main_checkout,
        &fixture.stop_args(),
        &fixture.envs(),
    );
    assert_eq!(stopped.code, 0, "outcome={}", stopped.stdout_text());
    assert_eq!(data(&stopped)["runtime_stopped"], true);
}

#[test]
fn runtime_stop_releases_global_registries_after_authority_seal() {
    let fixture = ExhaustedReadinessRuntimeStopFixture::new();
    let barrier = fixture.tmp.path().join("runtime-stop-authority-seal");
    let child = fixture.spawn_stop_at("after_authority_seal", &barrier);
    wait_for_barrier(&barrier);

    let fenced = run_main_agent(
        &fixture.main_checkout,
        &[
            "--state-dir",
            fixture.state_arg.as_str(),
            "worker",
            "accept",
            "assignment-exhausted",
            "--if-revision",
            "3",
            "--idempotency-key",
            "fenced-accept-0001",
            "--format",
            "json",
        ],
        &fixture.envs(),
    );
    assert_eq!(fenced.code, 69);
    assert_eq!(
        fenced.stdout_json()["error"]["code"],
        "worker-runtime-stop-in-flight"
    );
    let unrelated = run_main_agent(
        &fixture.main_checkout,
        &[
            "--state-dir",
            fixture.state_arg.as_str(),
            "worker",
            "accept",
            "assignment-unrelated",
            "--if-revision",
            "1",
            "--idempotency-key",
            "unrelated-accept-0001",
            "--format",
            "json",
        ],
        &fixture.envs(),
    );
    assert_eq!(unrelated.code, 0, "outcome={}", unrelated.stdout_text());
    terminate_at_barrier(child, "authority-sealed runtime stop");
}

#[test]
fn runtime_stop_crash_after_authority_seal_replays_exactly() {
    let fixture = ExhaustedReadinessRuntimeStopFixture::new();
    let barrier = fixture.tmp.path().join("runtime-stop-sealed-crash");
    let child = fixture.spawn_stop_at("after_authority_seal", &barrier);
    wait_for_barrier(&barrier);
    assert!(fixture.runtime.is_running());
    terminate_at_barrier(child, "authority-sealed runtime stop");

    let supervised = run_main_agent(
        &fixture.main_checkout,
        &[
            "--state-dir",
            fixture.state_arg.as_str(),
            "worker",
            "supervise",
            "assignment-exhausted",
            "--format",
            "json",
        ],
        &fixture.envs(),
    );
    assert_eq!(
        data(&supervised)["classification"],
        "readiness_stop_in_progress"
    );
    assert_eq!(
        data(&supervised)["recovery_action"]["argv"][9],
        "b3-stop-runtime-0001"
    );
    let stopped = run_main_agent(
        &fixture.main_checkout,
        &fixture.stop_args(),
        &fixture.envs(),
    );
    assert_eq!(stopped.code, 0, "outcome={}", stopped.stdout_text());
    assert!(!fixture.runtime.is_running());
}

fn assert_runtime_stop_orphan_recovery(stage: &str) {
    let fixture = ExhaustedReadinessRuntimeStopFixture::new();
    let barrier = fixture
        .tmp
        .path()
        .join(format!("runtime-stop-orphan-{stage}"));
    let child = fixture.spawn_stop_at(stage, &barrier);
    wait_for_barrier(&barrier);
    terminate_at_barrier(child, "orphaned runtime stop");

    fs::rename(
        fixture.state_dir.join("sessions/main-one"),
        fixture.state_dir.join("stale-main-one"),
    )
    .expect("make original runtime-stop controller unavailable");
    let successor_env = [(
        "AGENT_SESSION_CAPABILITY_FILE",
        fixture.main_two_capability.as_str(),
    )];
    let adopted = run_main_agent(
        &fixture.main_checkout,
        &[
            "--state-dir",
            fixture.state_arg.as_str(),
            "adopt",
            "assignment-exhausted",
            "--if-revision",
            "3",
            "--idempotency-key",
            &format!("orphan-runtime-stop-adopt-{stage}"),
            "--format",
            "json",
        ],
        &successor_env,
    );
    assert_eq!(adopted.code, 0, "outcome={}", adopted.stdout_text());
    assert_eq!(data(&adopted)["assignment"]["revision"], 4);
    assert_eq!(
        data(&adopted)["assignment"]["primary_manager"]["session_id"],
        "main-two"
    );

    let mut replay_env = fixture.envs().to_vec();
    replay_env[0] = (
        "AGENT_SESSION_CAPABILITY_FILE",
        fixture.main_two_capability.as_str(),
    );
    if stage == "after_runtime_stop" {
        replay_env.push(("AGENT_SESSION_FAKE_TMUX_ABSENT", "1"));
    }
    let replay = run_main_agent(&fixture.main_checkout, &fixture.stop_args(), &replay_env);
    assert_eq!(replay.code, 0, "outcome={}", replay.stdout_text());
    assert_eq!(data(&replay)["runtime_stopped"], true);
    assert_eq!(data(&replay)["assignment"]["revision"], 4);
    assert_eq!(
        data(&replay)["assignment"]["primary_manager"]["session_id"],
        "main-two"
    );
}

#[test]
fn runtime_stop_orphan_after_authority_seal_transfers_exact_replay() {
    assert_runtime_stop_orphan_recovery("after_authority_seal");
}

#[test]
fn runtime_stop_orphan_after_process_stop_transfers_finalization_only() {
    assert_runtime_stop_orphan_recovery("after_runtime_stop");
}

#[test]
fn runtime_stop_partial_fence_rebind_survives_pending_successor_loss() {
    let fixture = ExhaustedReadinessRuntimeStopFixture::new();
    let stop_barrier = fixture.tmp.path().join("runtime-stop-repeat-orphan");
    let child = fixture.spawn_stop_at("after_authority_seal", &stop_barrier);
    wait_for_barrier(&stop_barrier);
    terminate_at_barrier(child, "initial orphaned runtime stop");

    fs::rename(
        fixture.state_dir.join("sessions/main-one"),
        fixture.state_dir.join("stale-main-one"),
    )
    .expect("make initial runtime-stop controller unavailable");
    let first_adopt = run_main_agent(
        &fixture.main_checkout,
        &[
            "--state-dir",
            fixture.state_arg.as_str(),
            "adopt",
            "assignment-exhausted",
            "--if-revision",
            "3",
            "--idempotency-key",
            "runtime-stop-adopt-main-two",
            "--format",
            "json",
        ],
        &[(
            "AGENT_SESSION_CAPABILITY_FILE",
            fixture.main_two_capability.as_str(),
        )],
    );
    assert_eq!(first_adopt.code, 0, "outcome={}", first_adopt.stdout_text());
    assert_eq!(data(&first_adopt)["assignment"]["revision"], 4);

    fs::rename(
        fixture.state_dir.join("sessions/main-two"),
        fixture.state_dir.join("stale-main-two"),
    )
    .expect("make first successor unavailable");
    let adopt_barrier = fixture.tmp.path().join("runtime-stop-adopt-rebind");
    let second_adopt_args = [
        "--state-dir",
        fixture.state_arg.as_str(),
        "adopt",
        "assignment-exhausted",
        "--if-revision",
        "4",
        "--idempotency-key",
        "runtime-stop-adopt-main-three",
        "--format",
        "json",
    ];
    let mut second_adopt = Command::new(bin::resolve("main-agent"));
    second_adopt
        .current_dir(&fixture.main_checkout)
        .args(second_adopt_args)
        .env(
            "AGENT_SESSION_CAPABILITY_FILE",
            fixture.main_three_capability.as_str(),
        )
        .env(
            "NILS_AGENT_SESSION_TEST_RUNTIME_STOP_ADOPT_BARRIER_STAGE",
            "after_fence_rebind",
        )
        .env(
            "NILS_AGENT_SESSION_TEST_RUNTIME_STOP_ADOPT_BARRIER_DIR",
            &adopt_barrier,
        )
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = second_adopt
        .spawn()
        .expect("spawn second runtime-stop adopt");
    wait_for_barrier(&adopt_barrier);
    terminate_at_barrier(child, "partially rebound runtime-stop adopt");

    let final_adopt_args = [
        "--state-dir",
        fixture.state_arg.as_str(),
        "adopt",
        "assignment-exhausted",
        "--if-revision",
        "4",
        "--idempotency-key",
        "runtime-stop-adopt-main-four",
        "--format",
        "json",
    ];
    let pending_successor_live = run_main_agent(
        &fixture.main_checkout,
        &final_adopt_args,
        &[(
            "AGENT_SESSION_CAPABILITY_FILE",
            fixture.main_four_capability.as_str(),
        )],
    );
    assert_eq!(pending_successor_live.code, 65);
    assert_eq!(
        pending_successor_live.stdout_json()["error"]["code"],
        "worker-runtime-stop-fence-conflict"
    );

    fs::rename(
        fixture.state_dir.join("sessions/main-three"),
        fixture.state_dir.join("stale-main-three"),
    )
    .expect("make pending successor unavailable");
    let final_adopt = run_main_agent(
        &fixture.main_checkout,
        &final_adopt_args,
        &[(
            "AGENT_SESSION_CAPABILITY_FILE",
            fixture.main_four_capability.as_str(),
        )],
    );
    assert_eq!(final_adopt.code, 0, "outcome={}", final_adopt.stdout_text());
    assert_eq!(data(&final_adopt)["assignment"]["revision"], 5);
    assert_eq!(
        data(&final_adopt)["assignment"]["primary_manager"]["session_id"],
        "main-four"
    );

    let mut replay_env = fixture.envs().to_vec();
    replay_env[0] = (
        "AGENT_SESSION_CAPABILITY_FILE",
        fixture.main_four_capability.as_str(),
    );
    let replay = run_main_agent(&fixture.main_checkout, &fixture.stop_args(), &replay_env);
    assert_eq!(replay.code, 0, "outcome={}", replay.stdout_text());
    assert_eq!(data(&replay)["runtime_stopped"], true);
    assert_eq!(data(&replay)["assignment"]["revision"], 5);
    assert_eq!(
        data(&replay)["assignment"]["primary_manager"]["session_id"],
        "main-four"
    );
}

#[test]
fn runtime_stop_crash_after_process_stop_finalizes_without_second_kill() {
    let fixture = ExhaustedReadinessRuntimeStopFixture::new();
    let barrier = fixture.tmp.path().join("runtime-stop-process-stopped");
    let child = fixture.spawn_stop_at("after_runtime_stop", &barrier);
    wait_for_barrier(&barrier);
    assert!(!fixture.runtime.is_running());
    terminate_at_barrier(child, "process-stopped runtime stop");
    let kill_count = tmux_calls(&fixture.tmux_log)
        .iter()
        .filter(|call| {
            call.first().is_some_and(|arg| arg == "if-shell")
                && call.iter().any(|arg| arg.starts_with("kill-session -t "))
        })
        .count();

    let mut replay_env = fixture.envs().to_vec();
    replay_env.push(("AGENT_SESSION_FAKE_TMUX_ABSENT", "1"));
    let stopped = run_main_agent(&fixture.main_checkout, &fixture.stop_args(), &replay_env);
    assert_eq!(stopped.code, 0, "outcome={}", stopped.stdout_text());
    assert_eq!(data(&stopped)["runtime_stopped"], true);
    assert_eq!(
        tmux_calls(&fixture.tmux_log)
            .iter()
            .filter(|call| {
                call.first().is_some_and(|arg| arg == "if-shell")
                    && call.iter().any(|arg| arg.starts_with("kill-session -t "))
            })
            .count(),
        kill_count,
        "stopped-runtime replay must not issue another runtime kill"
    );
}

#[test]
fn runtime_stop_then_cancel_keeps_completed_replay_stable() {
    let fixture = ExhaustedReadinessRuntimeStopFixture::new();
    let stopped = run_main_agent(
        &fixture.main_checkout,
        &fixture.stop_args(),
        &fixture.envs(),
    );
    assert_eq!(stopped.code, 0, "outcome={}", stopped.stdout_text());
    let mut stopped_env = fixture.envs().to_vec();
    stopped_env.push(("AGENT_SESSION_FAKE_TMUX_ABSENT", "1"));
    let cancelled = run_main_agent(
        &fixture.main_checkout,
        &[
            "--state-dir",
            fixture.state_arg.as_str(),
            "worker",
            "cancel",
            "assignment-exhausted",
            "--if-revision",
            "3",
            "--reason",
            "typed runtime stop proved exhausted pre-claim worker stopped",
            "--idempotency-key",
            "cancel-after-stop-0001",
            "--format",
            "json",
        ],
        &stopped_env,
    );
    assert_eq!(cancelled.code, 0, "outcome={}", cancelled.stdout_text());
    assert_eq!(data(&cancelled)["assignment"]["state"], "cancelled");
    let calls_after_cancel = tmux_calls(&fixture.tmux_log);
    let replay = run_main_agent(&fixture.main_checkout, &fixture.stop_args(), &stopped_env);
    assert_eq!(replay.code, 0, "outcome={}", replay.stdout_text());
    assert_eq!(data(&replay), data(&stopped));
    assert_eq!(tmux_calls(&fixture.tmux_log), calls_after_cancel);
}

/// B3 admission rejects every ambiguous or already-owned precondition before
/// advertising the typed stop action.
#[test]
fn runtime_stop_admission_guards_are_fail_closed() {
    let ExhaustedReadinessRuntimeStopFixture {
        tmp: _tmp,
        state_dir,
        state_arg,
        main_checkout,
        worker_checkout: _,
        main_capability,
        main_two_capability: _,
        main_three_capability: _,
        main_four_capability: _,
        worker_start_receipt,
        runtime,
        tmux_log: _,
        tmux_arg,
        tmux_log_arg,
        runtime_pid_arg,
        state_runtime_arg,
    } = ExhaustedReadinessRuntimeStopFixture::new();
    let envs = [
        ("AGENT_SESSION_CAPABILITY_FILE", main_capability.as_str()),
        ("AGENT_SESSION_TMUX_BIN", tmux_arg.as_str()),
        ("AGENT_SESSION_FAKE_TMUX_LOG", tmux_log_arg.as_str()),
        ("AGENT_SESSION_FAKE_TMUX_PANE_PID", runtime_pid_arg.as_str()),
        (
            "AGENT_SESSION_FAKE_TMUX_PROCESS_GROUP_ID",
            runtime_pid_arg.as_str(),
        ),
        ("AGENT_SESSION_FAKE_TMUX_KEEP_PROCESS_GROUP", "1"),
        ("AGENT_SESSION_FAKE_TMUX_SESSION_ID", "$77"),
        (
            "AGENT_SESSION_FAKE_TMUX_AGENT_SESSION_ID",
            "worker-exhausted",
        ),
        (
            "AGENT_SESSION_FAKE_TMUX_STATE_DIR",
            state_runtime_arg.as_str(),
        ),
        (
            "AGENT_SESSION_FAKE_TMUX_RUNTIME_ID",
            "worker-exhausted-incarnation",
        ),
    ];
    let wrong_incarnation = run_main_agent(
        &main_checkout,
        &[
            "--state-dir",
            state_arg.as_str(),
            "worker",
            "stop-runtime",
            "assignment-exhausted",
            "--worker-incarnation",
            "worker-replacement-incarnation",
            "--if-revision",
            "3",
            "--idempotency-key",
            "b3-stop-runtime-wrong-incarnation",
            "--format",
            "json",
        ],
        &envs,
    );
    assert_eq!(wrong_incarnation.code, 65);
    assert_eq!(
        wrong_incarnation.stdout_json()["error"]["code"],
        "worker-incarnation-changed"
    );
    assert!(runtime.is_running());

    let stale_revision = run_main_agent(
        &main_checkout,
        &[
            "--state-dir",
            state_arg.as_str(),
            "worker",
            "stop-runtime",
            "assignment-exhausted",
            "--worker-incarnation",
            "worker-exhausted-incarnation",
            "--if-revision",
            "2",
            "--idempotency-key",
            "b3-stop-runtime-stale-revision",
            "--format",
            "json",
        ],
        &envs,
    );
    assert_eq!(stale_revision.code, 65);
    assert_eq!(
        stale_revision.stdout_json()["error"]["code"],
        "orchestration-revision-conflict"
    );
    assert!(runtime.is_running());

    rewrite_orchestration_registry(&state_dir, |registry| {
        registry["receipts"]
            .as_object_mut()
            .expect("receipts")
            .remove("main-one:main-incarnation-one:b3-worker-start");
    });
    let missing_final_proof = run_main_agent(
        &main_checkout,
        &[
            "--state-dir",
            state_arg.as_str(),
            "worker",
            "stop-runtime",
            "assignment-exhausted",
            "--worker-incarnation",
            "worker-exhausted-incarnation",
            "--if-revision",
            "3",
            "--idempotency-key",
            "b3-stop-runtime-submit-failure-alone",
            "--format",
            "json",
        ],
        &envs,
    );
    assert_eq!(missing_final_proof.code, 65);
    assert_eq!(
        missing_final_proof.stdout_json()["error"]["code"],
        "worker-readiness-not-exhausted",
        "submit_recovery.state=failed alone must never authorize a stop"
    );
    assert!(runtime.is_running());
    rewrite_orchestration_registry(&state_dir, |registry| {
        registry["receipts"]["main-one:main-incarnation-one:b3-worker-start"] =
            worker_start_receipt.clone();
        registry["assignments"]["assignment-exhausted"]["submit_recovery"]["state"] =
            json!("attempting");
    });
    let recovery_in_flight = run_main_agent(
        &main_checkout,
        &[
            "--state-dir",
            state_arg.as_str(),
            "worker",
            "stop-runtime",
            "assignment-exhausted",
            "--worker-incarnation",
            "worker-exhausted-incarnation",
            "--if-revision",
            "3",
            "--idempotency-key",
            "b3-stop-runtime-recovery-in-flight",
            "--format",
            "json",
        ],
        &envs,
    );
    assert_eq!(recovery_in_flight.code, 65);
    assert_eq!(
        recovery_in_flight.stdout_json()["error"]["code"],
        "submit-recovery-in-flight"
    );
    assert!(runtime.is_running());
    rewrite_orchestration_registry(&state_dir, |registry| {
        registry["assignments"]["assignment-exhausted"]["submit_recovery"]["state"] =
            json!("failed");
    });

    seed_active_claim(
        &state_dir,
        "worker-exhausted",
        "worker-exhausted-incarnation",
        "b3-worker-live-claim",
    );
    let active_worker_claim = run_main_agent(
        &main_checkout,
        &[
            "--state-dir",
            state_arg.as_str(),
            "worker",
            "stop-runtime",
            "assignment-exhausted",
            "--worker-incarnation",
            "worker-exhausted-incarnation",
            "--if-revision",
            "3",
            "--idempotency-key",
            "b3-stop-runtime-active-worker-claim",
            "--format",
            "json",
        ],
        &envs,
    );
    assert_eq!(active_worker_claim.code, 65);
    assert_eq!(
        active_worker_claim.stdout_json()["error"]["code"],
        "worker-not-quiescent"
    );
    assert!(runtime.is_running());
    rewrite_registry(&state_dir, |registry| {
        registry["claims"]
            .as_array_mut()
            .expect("claims")
            .retain(|claim| claim["session_id"] != "worker-exhausted");
    });

    seed_operation(
        &state_dir,
        "worker-exhausted",
        "worker-exhausted-incarnation",
        "b3-worker-active-operation",
        "active",
    );
    let active_operation = run_main_agent(
        &main_checkout,
        &[
            "--state-dir",
            state_arg.as_str(),
            "worker",
            "stop-runtime",
            "assignment-exhausted",
            "--worker-incarnation",
            "worker-exhausted-incarnation",
            "--if-revision",
            "3",
            "--idempotency-key",
            "b3-stop-runtime-active-operation",
            "--format",
            "json",
        ],
        &envs,
    );
    assert_eq!(active_operation.code, 65);
    assert_eq!(
        active_operation.stdout_json()["error"]["code"],
        "worker-not-quiescent"
    );
    assert!(runtime.is_running());
    rewrite_registry(&state_dir, |registry| {
        registry["operations"]
            .as_array_mut()
            .expect("operations")
            .retain(|operation| operation["session_id"] != "worker-exhausted");
    });

    rewrite_orchestration_registry(&state_dir, |registry| {
        let assignment = &mut registry["assignments"]["assignment-exhausted"];
        assignment["account_handoff"] = json!({
            "schema_version": "main-agent.account-handoff-reservation.v3",
            "request_digest": "a".repeat(64),
            "reservation_id": "b3-handoff-reservation",
            "account_intent_id": "b3-handoff-intent",
            "run_id": "run-one",
            "controller": assignment["primary_manager"].clone(),
            "worker": assignment["worker"].clone(),
            "reserved_revision": 2,
            "account": "fallback",
            "created_at": "2030-01-01T00:00:03Z",
            "updated_at": "2030-01-01T00:00:03Z"
        });
    });
    let handoff_fenced = run_main_agent(
        &main_checkout,
        &[
            "--state-dir",
            state_arg.as_str(),
            "worker",
            "supervise",
            "assignment-exhausted",
            "--format",
            "json",
        ],
        &envs,
    );
    assert_eq!(
        handoff_fenced.code,
        0,
        "outcome={}",
        handoff_fenced.stdout_text()
    );
    assert_eq!(
        data(&handoff_fenced)["schema_version"],
        "main-agent.worker-supervise-result.v3"
    );
    assert_eq!(
        data(&handoff_fenced)["classification"],
        "account_handoff_in_flight"
    );
    assert_eq!(
        data(&handoff_fenced)["recovery_action"]["schema_version"],
        "main-agent.worker-recovery-action.v3"
    );
    assert_eq!(
        data(&handoff_fenced)["recovery_action"]["kind"],
        "managed_account_handoff_cancel"
    );
    assert_eq!(
        data(&handoff_fenced)["recovery_action"]["executable"],
        false
    );
    assert_eq!(
        data(&handoff_fenced)["recovery_action"]["argv_template"][2],
        "account-handoff-cancel"
    );
    rewrite_orchestration_registry(&state_dir, |registry| {
        registry["assignments"]["assignment-exhausted"]
            .as_object_mut()
            .expect("assignment")
            .remove("account_handoff");
    });

    let supervised = run_main_agent(
        &main_checkout,
        &[
            "--state-dir",
            state_arg.as_str(),
            "worker",
            "supervise",
            "assignment-exhausted",
            "--format",
            "json",
        ],
        &envs,
    );
    assert_eq!(supervised.code, 0, "outcome={}", supervised.stdout_text());
    assert_eq!(
        data(&supervised)["schema_version"],
        "main-agent.worker-supervise-result.v3"
    );
    assert_eq!(
        data(&supervised)["classification"],
        "readiness_stop_required"
    );
    assert_eq!(
        data(&supervised)["recovery_action"]["kind"],
        "exact_worker_runtime_stop"
    );
    assert_eq!(data(&supervised)["recovery_action"]["executable"], true);
    assert_eq!(
        data(&supervised)["recovery_action"]["argv"][2],
        "stop-runtime"
    );
    assert_eq!(
        data(&supervised)["recovery_action"]["argv"][5],
        "worker-exhausted-incarnation"
    );
}

/// B2: a worker that dies *after* bootstrap acquired its assignment-derived
/// claim leaves the assignment `working` with a claim alive on TTL. That is not
/// a pre-claim failure, so `worker cancel` and `worker reassign` both refuse and
/// the only remaining tool used to delete the Main Agent session itself. This
/// exercises the post-claim classification plus the guarded terminalization that
/// closes such a lane while preserving the worker worktree, the run, and the
/// Main Agent session.
#[test]
fn main_agent_post_claim_stopped_worker_is_terminalized_without_deleting_the_run() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let state_arg = state_dir.to_string_lossy().into_owned();
    let main_checkout = tmp.path().join("main-checkout");
    let worker_checkout = tmp.path().join("worker-checkout");
    let unrelated_checkout = tmp.path().join("unrelated-checkout");
    fs::create_dir(&state_dir).expect("state");
    for checkout in [&main_checkout, &worker_checkout, &unrelated_checkout] {
        init_checkout(checkout, "https://example.invalid/example/repository.git");
    }
    seed_brokers_at(
        &state_dir,
        &[
            (
                "main-one",
                "main-incarnation-one",
                "main-private-capability-material-0000000001",
                main_checkout.as_path(),
                Some("enforce"),
            ),
            (
                "main-two",
                "main-incarnation-two",
                "main-private-capability-material-postclaim-other",
                main_checkout.as_path(),
                Some("enforce"),
            ),
            (
                "worker-stopped",
                "worker-stopped-incarnation",
                "worker-stopped-private-capability-0000000001",
                worker_checkout.as_path(),
                Some("enforce"),
            ),
            (
                "worker-unrelated",
                "worker-unrelated-incarnation",
                "worker-unrelated-private-capability-00000001",
                unrelated_checkout.as_path(),
                Some("enforce"),
            ),
        ],
    );
    let main_capability = init_main_run(
        tmp.path(),
        &state_dir,
        &main_checkout,
        "main-one",
        "run-one",
    );
    let other_main_capability = capability(&state_dir, "main-two");
    seed_active_claim(
        &state_dir,
        "main-two",
        "main-incarnation-two",
        "main-two-postclaim-claim",
    );
    rewrite_orchestration_registry(&state_dir, |registry| {
        let mut run_two = registry["runs"]["run-one"].clone();
        run_two["run_id"] = json!("run-two");
        run_two["revision"] = json!(1);
        run_two["objective_summary"] = json!("Unrelated Main Agent run");
        run_two["controller"]["session_id"] = json!("main-two");
        run_two["controller"]["session_incarnation"] = json!("main-incarnation-two");
        registry["runs"]["run-two"] = run_two;
    });

    let stopped_packet = json!({
        "schema_version": "main-agent.assignment-input.v1",
        "assignment_id": "assignment-stopped",
        "task_summary": "Worker runtime dies after claim acquisition",
        "task": {},
        "launch": {
            "agent": "codex",
            "cwd": worker_checkout,
            "title": null,
            "session_id": "worker-stopped",
            "coordination_mode": "enforce",
            "agent_args": []
        },
        "repository": "example/repository",
        "worktree": worker_checkout,
        "base_ref": "main",
        "scopes": ["docs/stopped-lane"],
        "durable_refs": []
    });
    insert_orchestration_assignment(
        &state_dir,
        "assignment-stopped",
        json!({
            "schema_version": "agent-session.orchestration-assignment.v1",
            "assignment_id": "assignment-stopped",
            "run_id": "run-one",
            "revision": 2,
            "state": "starting",
            "task_summary": "Worker runtime dies after claim acquisition",
            "private_packet_digest": "replaced-by-fixture",
            "primary_manager": {
                "session_id": "main-one",
                "session_incarnation": "main-incarnation-one",
                "session_created_at": "2030-01-01T00:00:00Z"
            },
            "worker": {
                "session_id": "worker-stopped",
                "session_incarnation": "worker-stopped-incarnation",
                "session_created_at": "2030-01-01T00:00:00Z"
            },
            "collaborators": [],
            "borrowed_by": [],
            "repository": "example/repository",
            "worktree": worker_checkout,
            "base_ref": "main",
            "scopes": ["docs/stopped-lane"],
            "durable_refs": [],
            "checkpoint": null,
            "result_summary": null,
            "blocker_summary": null,
            "created_at": "2030-01-01T00:00:01Z",
            "updated_at": "2030-01-01T00:00:02Z"
        }),
        &stopped_packet,
    );
    let unrelated_packet = json!({
        "schema_version": "main-agent.assignment-input.v1",
        "assignment_id": "assignment-unrelated",
        "task_summary": "Unrelated live worker",
        "task": {},
        "launch": {
            "agent": "codex",
            "cwd": unrelated_checkout,
            "title": null,
            "session_id": "worker-unrelated",
            "coordination_mode": "enforce",
            "agent_args": []
        },
        "repository": "example/repository",
        "worktree": unrelated_checkout,
        "base_ref": "main",
        "scopes": ["docs/unrelated"],
        "durable_refs": []
    });
    insert_orchestration_assignment(
        &state_dir,
        "assignment-unrelated",
        json!({
            "schema_version": "agent-session.orchestration-assignment.v1",
            "assignment_id": "assignment-unrelated",
            "run_id": "run-one",
            "revision": 4,
            "state": "working",
            "task_summary": "Unrelated live worker",
            "private_packet_digest": "replaced-by-fixture",
            "primary_manager": {
                "session_id": "main-one",
                "session_incarnation": "main-incarnation-one",
                "session_created_at": "2030-01-01T00:00:00Z"
            },
            "worker": {
                "session_id": "worker-unrelated",
                "session_incarnation": "worker-unrelated-incarnation",
                "session_created_at": "2030-01-01T00:00:00Z"
            },
            "collaborators": [],
            "borrowed_by": [],
            "repository": "example/repository",
            "worktree": unrelated_checkout,
            "base_ref": "main",
            "scopes": ["docs/unrelated"],
            "durable_refs": [],
            "checkpoint": {
                "revision": 4,
                "summary": "Unrelated work continues",
                "next_action": "Continue",
                "updated_at": "2030-01-01T00:00:03Z"
            },
            "result_summary": null,
            "blocker_summary": null,
            "created_at": "2030-01-01T00:00:01Z",
            "updated_at": "2030-01-01T00:00:03Z"
        }),
        &unrelated_packet,
    );
    seed_active_claim(
        &state_dir,
        "worker-unrelated",
        "worker-unrelated-incarnation",
        "worker-unrelated-active-claim",
    );

    // The worker reaches `working` through the ordinary authenticated bootstrap,
    // so the claim under test is the real assignment-derived claim.
    let worker_capability = capability(&state_dir, "worker-stopped");
    let bootstrapped = run_main_agent(
        &worker_checkout,
        &[
            "--state-dir",
            &state_arg,
            "bootstrap",
            "--idempotency-key",
            "postclaim-bootstrap-0001",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_CAPABILITY_FILE", &worker_capability)],
    );
    assert_eq!(
        bootstrapped.code,
        0,
        "stdout={} stderr={}",
        bootstrapped.stdout_text(),
        bootstrapped.stderr_text()
    );
    assert_eq!(
        data(&bootstrapped)["assignment"]["record"]["state"],
        "working"
    );
    assert_eq!(data(&bootstrapped)["assignment"]["record"]["revision"], 3);

    // Unaccepted worker output that terminalization must preserve.
    let retained_progress = worker_checkout.join("worker-progress-to-preserve");
    fs::write(&retained_progress, "unaccepted worker output").expect("worker progress");

    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let tmux_arg = tmux_bin.to_string_lossy().into_owned();
    let tmux_log_arg = tmux_log.to_string_lossy().into_owned();
    let stopped_env = [
        ("AGENT_SESSION_CAPABILITY_FILE", main_capability.as_str()),
        ("AGENT_SESSION_TMUX_BIN", tmux_arg.as_str()),
        ("AGENT_SESSION_FAKE_TMUX_LOG", tmux_log_arg.as_str()),
        ("AGENT_SESSION_FAKE_TMUX_ABSENT", "1"),
    ];
    let worker_record_path = state_dir.join("sessions/worker-stopped/session.json");
    let set_runtime_identity = |identity: serde_json::Value| {
        let mut record: serde_json::Value =
            serde_json::from_slice(&fs::read(&worker_record_path).expect("worker session record"))
                .expect("worker session json");
        record["delete_tmux_identity"] = identity;
        write_private_json(&worker_record_path, &record);
    };
    let stopped_identity = json!({
        "launch_id": "worker-stopped-incarnation",
        "session_id": "$91",
        "pane_id": "%91",
        "pane_pid": 2_000_000_000,
        "process_group_id": 2_000_000_000
    });
    set_runtime_identity(stopped_identity.clone());

    // A durably stopped post-claim runtime must not read as healthy progress,
    // and must not be classified as the pre-claim failure `worker cancel` owns.
    let supervised = run_main_agent(
        &main_checkout,
        &[
            "--state-dir",
            &state_arg,
            "worker",
            "supervise",
            "assignment-stopped",
            "--format",
            "json",
        ],
        &stopped_env,
    );
    assert_eq!(supervised.code, 0, "stderr={}", supervised.stderr_text());
    assert_eq!(
        data(&supervised)["schema_version"],
        "main-agent.worker-supervise-result.v2"
    );
    assert_eq!(data(&supervised)["classification"], "post_claim_failure");
    assert_eq!(data(&supervised)["automatic_retry_safe"], false);
    assert_eq!(
        data(&supervised)["last_proven_safe_state"]["post_claim_terminalization_safe"],
        true
    );
    assert_eq!(
        data(&supervised)["last_proven_safe_state"]["failed_preclaim"],
        false,
        "a post-claim failure must not be routed through pre_claim_failure"
    );
    assert_eq!(
        data(&supervised)["recovery_action"]["kind"],
        "stopped_worker_terminalization"
    );
    assert_eq!(
        data(&supervised)["recovery_action"]["schema_version"],
        "main-agent.worker-recovery-action.v2"
    );
    assert_eq!(
        data(&supervised)["recovery_action"]["argv_template"][2],
        "reconcile-stopped"
    );
    assert_eq!(
        data(&supervised)["last_proven_safe_state"]["coordination"]["claim_active"],
        true,
        "the post-claim lane still holds its assignment-derived claim"
    );
    let diagnosed = run_main_agent(
        &main_checkout,
        &[
            "--state-dir",
            &state_arg,
            "worker",
            "diagnose",
            "assignment-stopped",
            "--format",
            "json",
        ],
        &stopped_env,
    );
    assert_eq!(diagnosed.code, 0, "stderr={}", diagnosed.stderr_text());
    assert_eq!(
        data(&diagnosed)["schema_version"],
        "main-agent.worker-diagnose-result.v2"
    );
    assert_eq!(data(&diagnosed)["classification"], "post_claim_failure");
    assert_eq!(
        data(&diagnosed)["recovery_action"]["kind"],
        "stopped_worker_terminalization"
    );
    assert_eq!(
        data(&diagnosed)["recovery_action"]["schema_version"],
        "main-agent.worker-recovery-action.v2"
    );

    // The pre-claim command remains unusable, which is what made the lane
    // uncloseable before the guarded post-claim transition existed.
    let refused_cancel = run_main_agent(
        &main_checkout,
        &[
            "--state-dir",
            &state_arg,
            "worker",
            "cancel",
            "assignment-stopped",
            "--if-revision",
            "3",
            "--reason",
            "worker runtime died after claim acquisition",
            "--idempotency-key",
            "postclaim-cancel-refused-0001",
            "--format",
            "json",
        ],
        &stopped_env,
    );
    assert_eq!(refused_cancel.code, 65);
    assert_eq!(
        refused_cancel.stdout_json()["error"]["code"],
        "assignment-not-preclaim-failed"
    );

    let terminalize_args = [
        "--state-dir",
        state_arg.as_str(),
        "worker",
        "reconcile-stopped",
        "assignment-stopped",
        "--if-revision",
        "3",
        "--reason",
        "worker runtime died after claim acquisition",
        "--idempotency-key",
        "postclaim-terminalize-0001",
        "--format",
        "json",
    ];

    // A live runtime and unverifiable runtime evidence both fail closed.
    let live_runtime = seed_live_runtime_identity(
        &state_dir,
        "worker-stopped",
        "worker-stopped-incarnation",
        91,
    );
    let live_refusal = run_main_agent(&main_checkout, &terminalize_args, &stopped_env);
    assert_eq!(
        live_refusal.code,
        65,
        "outcome={}",
        live_refusal.stdout_text()
    );
    assert_eq!(
        live_refusal.stdout_json()["error"]["code"],
        "worker-runtime-still-live"
    );
    drop(live_runtime);

    set_runtime_identity(json!({
        "launch_id": "worker-stopped-incarnation",
        "session_id": "$91",
        "pane_id": "%91",
        "pane_pid": 2_000_000_000
    }));
    let unverified_runtime = run_main_agent(&main_checkout, &terminalize_args, &stopped_env);
    assert_eq!(unverified_runtime.code, 1);
    assert_eq!(
        unverified_runtime.stdout_json()["error"]["code"],
        "coordination-runtime-unverified"
    );
    set_runtime_identity(stopped_identity.clone());

    // Any active or uncertain worker operation preserves the lane.
    for (operation_case, lease_state) in [
        ("active-operation", "active"),
        ("uncertain-operation", "reconcile_pending"),
    ] {
        seed_operation(
            &state_dir,
            "worker-stopped",
            "worker-stopped-incarnation",
            &format!("worker-stopped-{operation_case}"),
            lease_state,
        );
        let refused = run_main_agent(&main_checkout, &terminalize_args, &stopped_env);
        assert_ne!(refused.code, 0, "case={operation_case}");
        assert_eq!(
            refused.stdout_json()["error"]["code"],
            "worker-not-quiescent",
            "case={operation_case}"
        );
        let supervised_operation = run_main_agent(
            &main_checkout,
            &[
                "--state-dir",
                &state_arg,
                "worker",
                "supervise",
                "assignment-stopped",
                "--format",
                "json",
            ],
            &stopped_env,
        );
        assert_eq!(
            data(&supervised_operation)["classification"],
            "uncertain_mutation",
            "an operation lease must dominate the terminalization classification: case={operation_case}"
        );
        assert_eq!(
            orchestration_registry(&state_dir)["assignments"]["assignment-stopped"]["state"],
            "working",
            "case={operation_case}"
        );
        rewrite_registry(&state_dir, |registry| {
            registry["operations"]
                .as_array_mut()
                .expect("operations")
                .retain(|operation| operation["session_id"] != "worker-stopped");
        });
    }

    // A changed worker incarnation is identity-mismatched evidence, never a
    // licence to terminalize the assignment it no longer describes.
    let mut replaced_record: serde_json::Value =
        serde_json::from_slice(&fs::read(&worker_record_path).expect("worker session record"))
            .expect("worker session json");
    let original_runtime = replaced_record["runtime"].clone();
    replaced_record["runtime"]["launch_id"] = json!("worker-stopped-replacement");
    write_private_json(&worker_record_path, &replaced_record);
    let incarnation_refusal = run_main_agent(&main_checkout, &terminalize_args, &stopped_env);
    assert_ne!(incarnation_refusal.code, 0);
    assert_eq!(
        incarnation_refusal.stdout_json()["error"]["code"],
        "worker-incarnation-changed"
    );
    let supervised_mismatch = run_main_agent(
        &main_checkout,
        &[
            "--state-dir",
            &state_arg,
            "worker",
            "supervise",
            "assignment-stopped",
            "--format",
            "json",
        ],
        &stopped_env,
    );
    assert_eq!(
        data(&supervised_mismatch)["classification"],
        "evidence_unavailable",
        "identity-mismatched worker evidence must not classify as terminalizable"
    );
    replaced_record["runtime"] = original_runtime;
    write_private_json(&worker_record_path, &replaced_record);

    // A stale assignment revision and a Main Agent that does not own the run
    // are both refused before any mutation.
    let stale_revision = run_main_agent(
        &main_checkout,
        &[
            "--state-dir",
            &state_arg,
            "worker",
            "reconcile-stopped",
            "assignment-stopped",
            "--if-revision",
            "2",
            "--reason",
            "worker runtime died after claim acquisition",
            "--idempotency-key",
            "postclaim-terminalize-stale-0001",
            "--format",
            "json",
        ],
        &stopped_env,
    );
    assert_eq!(stale_revision.code, 65);
    assert_eq!(
        stale_revision.stdout_json()["error"]["code"],
        "orchestration-revision-conflict"
    );
    let foreign_main = run_main_agent(
        &main_checkout,
        &terminalize_args,
        &[
            ("AGENT_SESSION_CAPABILITY_FILE", &other_main_capability),
            ("AGENT_SESSION_TMUX_BIN", &tmux_arg),
            ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
            ("AGENT_SESSION_FAKE_TMUX_ABSENT", "1"),
        ],
    );
    assert_ne!(
        foreign_main.code,
        0,
        "outcome={}",
        foreign_main.stdout_text()
    );
    assert_eq!(
        foreign_main.stdout_json()["error"]["code"],
        "assignment-not-found"
    );

    // A pre-claim `starting` assignment stays on the pre-claim cancellation
    // path; this command must not absorb it.
    rewrite_orchestration_registry(&state_dir, |registry| {
        registry["assignments"]["assignment-stopped"]["state"] = json!("starting");
    });
    let preclaim_refusal = run_main_agent(&main_checkout, &terminalize_args, &stopped_env);
    assert_ne!(preclaim_refusal.code, 0);
    assert_eq!(
        preclaim_refusal.stdout_json()["error"]["code"],
        "assignment-state-conflict"
    );
    rewrite_orchestration_registry(&state_dir, |registry| {
        registry["assignments"]["assignment-stopped"]["state"] = json!("working");
    });

    let terminalized = run_main_agent(&main_checkout, &terminalize_args, &stopped_env);
    assert_eq!(
        terminalized.code,
        0,
        "stdout={} stderr={}",
        terminalized.stdout_text(),
        terminalized.stderr_text()
    );
    assert_eq!(data(&terminalized)["terminalized"], true);
    assert_eq!(data(&terminalized)["worker_claim_active_after"], false);
    assert_eq!(data(&terminalized)["input_sent"], false);
    assert_eq!(data(&terminalized)["automatic_retry_safe"], false);
    assert_eq!(data(&terminalized)["proof"]["worker_runtime"], "stopped");
    assert_eq!(data(&terminalized)["proof"]["coordination"], "quiescent");
    assert_eq!(
        data(&terminalized)["proof"]["worker_claim"],
        json!({
            "active_disposition": "absent",
            "release_provenance": "not_attributed_to_attempt",
            "observed_at_stage1": true
        })
    );
    assert_eq!(data(&terminalized)["assignment"]["state"], "cancelled");
    assert_eq!(data(&terminalized)["assignment"]["revision"], 4);
    assert!(
        data(&terminalized)["assignment"]["blocker_summary"]
            .as_str()
            .unwrap_or_default()
            .contains("worker runtime died after claim acquisition")
    );
    assert!(
        data(&terminalized)["proof"]["runtime_identity_digest"]
            .as_str()
            .is_some_and(
                |digest| digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
            ),
        "runtime proof must be a bounded digest, not raw process identity: {}",
        data(&terminalized)["proof"]
    );
    assert!(
        !terminalized.stdout_text().contains(&worker_capability),
        "the typed result must not leak the private worker capability path"
    );
    let terminal_orchestration = orchestration_registry(&state_dir);
    assert!(
        terminal_orchestration["assignments"]["assignment-stopped"]["worker_quarantine"].is_null(),
        "B2 must keep the released v3 assignment quarantine projection empty"
    );
    assert_frozen_base_v3_registry_compatible(&terminal_orchestration);
    assert!(
        state_dir
            .join("sessions/worker-stopped/authority-quarantine.json")
            .is_file(),
        "session-owned authority quarantine must be durable before public success"
    );

    // The session-owned quarantine is incarnation-independent authority:
    // neither the direct CLI nor the HTTP handler may provision a replacement
    // launch, broker, or capability before guarded retirement removes the
    // retained worker record.
    let runtime_before_resume =
        serde_json::from_slice::<serde_json::Value>(&fs::read(&worker_record_path).unwrap())
            .unwrap()["runtime"]
            .clone();
    let quarantined_resume = run_with_env(
        &worker_checkout,
        &[
            "--state-dir",
            &state_arg,
            "resume",
            "worker-stopped",
            "--format",
            "json",
        ],
        &[
            ("AGENT_SESSION_TMUX_BIN", &tmux_arg),
            ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
            ("AGENT_SESSION_FAKE_TMUX_ABSENT", "1"),
        ],
    );
    assert_eq!(quarantined_resume.code, 65);
    assert_eq!(
        quarantined_resume.stdout_json()["error"]["code"],
        "worker-quarantined"
    );
    let runtime_after_resume =
        serde_json::from_slice::<serde_json::Value>(&fs::read(&worker_record_path).unwrap())
            .unwrap()["runtime"]
            .clone();
    assert_eq!(
        runtime_after_resume, runtime_before_resume,
        "quarantined resume must fail before rotating the runtime incarnation"
    );
    assert!(
        !Path::new(&worker_capability).exists(),
        "quarantined resume must fail before recreating a capability"
    );

    // Only the target worker's coordination authority changed.
    let coordination = load_coordination_registry(&state_dir);
    let claim_states = |session_id: &str| {
        coordination["claims"]
            .as_array()
            .expect("claims")
            .iter()
            .filter(|claim| claim["session_id"] == session_id)
            .map(|claim| claim["state"].as_str().unwrap_or_default().to_string())
            .collect::<Vec<_>>()
    };
    assert!(
        !claim_states("worker-stopped")
            .iter()
            .any(|state| state == "active"),
        "the stopped worker claim must be revoked"
    );
    assert!(
        claim_states("worker-unrelated")
            .iter()
            .any(|state| state == "active"),
        "an unrelated worker claim must survive"
    );
    assert!(
        claim_states("main-one")
            .iter()
            .any(|state| state == "active"),
        "the controlling Main Agent claim must survive"
    );
    assert_eq!(
        coordination["brokers"]["worker-stopped"]["state"],
        "stopped"
    );
    assert_eq!(
        coordination["brokers"]["worker-unrelated"]["state"],
        "ready"
    );
    assert!(
        !Path::new(&worker_capability).exists(),
        "the stopped worker capability must be sealed"
    );

    // The worktree, the run, and the Main Agent session all survive.
    assert!(
        retained_progress.is_file(),
        "terminalization must preserve unaccepted worker output"
    );
    let orchestration = orchestration_registry(&state_dir);
    assert_eq!(orchestration["runs"]["run-one"]["state"], "active");
    assert_eq!(
        orchestration["assignments"]["assignment-unrelated"]["state"],
        "working"
    );
    assert!(
        state_dir.join("sessions/main-one").exists(),
        "terminalization must never remove the Main Agent session"
    );
    assert!(
        state_dir.join("sessions/worker-stopped").exists(),
        "the stopped worker record is retained until ordinary retirement"
    );

    let replay = run_main_agent(&main_checkout, &terminalize_args, &stopped_env);
    assert_eq!(replay.code, 0, "stderr={}", replay.stderr_text());
    assert_eq!(
        replay.stdout_text(),
        terminalized.stdout_text(),
        "exact replay must return the original terminal receipt"
    );
    assert_eq!(
        orchestration_registry(&state_dir)["assignments"]["assignment-stopped"]["revision"],
        4,
        "exact replay must not advance the assignment again"
    );

    // The ordinary retirement path is now reachable.
    let retired = run_main_agent(
        &main_checkout,
        &[
            "--state-dir",
            &state_arg,
            "worker",
            "retire",
            "assignment-stopped",
            "--if-revision",
            "4",
            "--idempotency-key",
            "postclaim-retire-0001",
            "--format",
            "json",
        ],
        &stopped_env,
    );
    assert_eq!(
        retired.code,
        0,
        "stdout={} stderr={}",
        retired.stdout_text(),
        retired.stderr_text()
    );
    assert!(
        !state_dir.join("sessions/worker-stopped").exists(),
        "ordinary retirement must remove the stopped worker session"
    );
    assert!(
        retained_progress.is_file(),
        "retirement must not touch the managed worktree"
    );
    let after_retire = orchestration_registry(&state_dir);
    assert_eq!(after_retire["runs"]["run-one"]["state"], "active");
    assert_eq!(
        after_retire["assignments"]["assignment-unrelated"]["state"],
        "working"
    );
    assert!(state_dir.join("sessions/main-one").exists());
}

#[test]
fn reconcile_stopped_identical_replay_converges_from_typed_progress() {
    let fixture = StoppedPostClaimFixture::new();
    let barrier = fixture._tmp.path().join("reconcile-progress-barrier");
    fs::create_dir(&barrier).expect("barrier");
    let first = fixture.spawn_reconcile_at_barrier(&barrier, "after_terminal_commit");
    wait_for_barrier(&barrier);

    let progress = orchestration_registry(&fixture.state_dir);
    let progress_receipt = &progress["receipts"]["main-one:main-incarnation-one:focused-postclaim-terminalize-0001"]
        ["outcome"];
    assert_eq!(
        progress_receipt["schema_version"],
        "main-agent.worker-reconcile-stopped-progress.v1"
    );
    assert_eq!(progress_receipt["stage"], "authority_quarantined");
    assert_eq!(
        progress["assignments"]["assignment-stopped"]["state"],
        "cancelled"
    );
    assert!(
        progress["assignments"]["assignment-stopped"]["worker_quarantine"].is_null(),
        "interrupted B2 progress must remain readable by the released v3 assignment contract"
    );
    assert_frozen_base_v3_registry_compatible(&progress);
    assert!(
        fixture
            .state_dir
            .join("sessions/worker-stopped/authority-quarantine.json")
            .is_file()
    );
    assert!(
        Path::new(&fixture.worker_capability).exists(),
        "stage 1 must not report success or seal target authority"
    );

    let replay = fixture.run_reconcile();
    assert_eq!(replay.code, 0, "stderr={}", replay.stderr_text());
    assert_eq!(
        data(&replay)["schema_version"],
        "main-agent.worker-reconcile-stopped-result.v2"
    );
    assert_ne!(
        data(&replay)["schema_version"],
        "main-agent.worker-reconcile-stopped-progress.v1",
        "durable progress is never a public success"
    );
    assert!(
        !Path::new(&fixture.worker_capability).exists(),
        "a successful replay must have sealed target authority"
    );
    assert!(
        !load_coordination_registry(&fixture.state_dir)["claims"]
            .as_array()
            .expect("claims")
            .iter()
            .any(|claim| { claim["session_id"] == "worker-stopped" && claim["state"] == "active" })
    );

    fs::write(barrier.join("release"), b"release").expect("release barrier");
    let first = first.wait_with_output().expect("first reconcile output");
    assert!(
        first.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&first.stdout),
        String::from_utf8_lossy(&first.stderr)
    );
    let first: serde_json::Value =
        serde_json::from_slice(&first.stdout).expect("first reconcile json");
    assert_eq!(first["data"], data(&replay));
}

#[test]
fn reconcile_stopped_observational_path_skips_operation_renewal_probes() {
    let fixture = StoppedPostClaimFixture::new();
    seed_operation(
        &fixture.state_dir,
        "main-one",
        "main-incarnation-one",
        "b2-observational-operation",
        "active",
    );
    rewrite_registry(&fixture.state_dir, |registry| {
        let operation = registry["operations"]
            .as_array_mut()
            .expect("operations")
            .iter_mut()
            .find(|operation| operation["lease_id"] == "b2-observational-operation")
            .expect("operation");
        operation["expires_at"] = json!("1970-01-01T00:00:00Z");
        operation["expires_at_epoch"] = json!(0);
    });
    let operation_before = load_coordination_registry(&fixture.state_dir)["operations"][0].clone();
    let probe_log = fixture._tmp.path().join("operation-renewal-probes.log");
    let probe_log_arg = probe_log.to_string_lossy().into_owned();

    let reconciled = fixture.run_reconcile_with_extra_env(&[(
        "NILS_AGENT_SESSION_TEST_COORDINATION_OPERATION_PROBE_LOG",
        probe_log_arg.as_str(),
    )]);
    assert_eq!(
        reconciled.code,
        0,
        "stdout={} stderr={}",
        reconciled.stdout_text(),
        reconciled.stderr_text()
    );
    assert!(
        !probe_log.exists(),
        "B2 observational locks must not enter operation-renewal probes"
    );
    let operation_after = load_coordination_registry(&fixture.state_dir)["operations"][0].clone();
    assert_eq!(
        json!({
            "state": operation_after["state"],
            "revision": operation_after["revision"],
            "expires_at": operation_after["expires_at"],
            "expires_at_epoch": operation_after["expires_at_epoch"],
            "reconcile_observed_at_epoch": operation_after["reconcile_observed_at_epoch"]
        }),
        json!({
            "state": operation_before["state"],
            "revision": operation_before["revision"],
            "expires_at": operation_before["expires_at"],
            "expires_at_epoch": operation_before["expires_at_epoch"],
            "reconcile_observed_at_epoch": operation_before["reconcile_observed_at_epoch"]
        }),
        "B2 observational locks must not rewrite unrelated operation leases"
    );
}

#[test]
fn reconcile_stopped_normal_and_after_seal_retry_share_stable_claim_truth() {
    let normal_fixture = StoppedPostClaimFixture::new();
    let normal = normal_fixture.run_reconcile();
    assert_eq!(normal.code, 0, "stderr={}", normal.stderr_text());

    let fixture = StoppedPostClaimFixture::new();
    let barrier = fixture
        ._tmp
        .path()
        .join("reconcile-after-authority-seal-barrier");
    fs::create_dir(&barrier).expect("barrier");
    let before = load_coordination_registry(&fixture.state_dir);
    let main_broker_before = before["brokers"]["main-one"].clone();
    let main_claims_before = before["claims"]
        .as_array()
        .expect("claims")
        .iter()
        .filter(|claim| claim["session_id"] == "main-one")
        .cloned()
        .collect::<Vec<_>>();
    let first = fixture.spawn_reconcile_at_barrier(&barrier, "after_authority_seal");
    wait_for_barrier(&barrier);
    let mut first = KillChild(Some(first));
    first
        .0
        .as_mut()
        .expect("child")
        .kill()
        .expect("kill child after authority seal");
    first
        .0
        .as_mut()
        .expect("child")
        .wait()
        .expect("wait for killed child");
    first.0 = None;

    let after_crash = load_coordination_registry(&fixture.state_dir);
    assert_eq!(after_crash["brokers"]["worker-stopped"]["state"], "stopped");
    assert_eq!(
        after_crash["brokers"]["worker-stopped"]["capability_digest"],
        ""
    );
    assert!(
        !after_crash["claims"]
            .as_array()
            .expect("claims")
            .iter()
            .any(|claim| { claim["session_id"] == "worker-stopped" && claim["state"] == "active" }),
        "the durable target-only seal must release the stopped worker claim"
    );
    assert!(
        !Path::new(&fixture.worker_capability).exists(),
        "the durable target-only seal must remove the stopped worker capability"
    );
    assert_eq!(
        after_crash["brokers"]["main-one"], main_broker_before,
        "the target-only seal must preserve the controller broker"
    );
    let main_claims_after_crash = after_crash["claims"]
        .as_array()
        .expect("claims")
        .iter()
        .filter(|claim| claim["session_id"] == "main-one")
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(
        main_claims_after_crash, main_claims_before,
        "the target-only seal must preserve controller claims"
    );
    assert!(
        Path::new(&fixture.main_capability).exists(),
        "the target-only seal must preserve the controller capability"
    );

    let progress_registry = orchestration_registry(&fixture.state_dir);
    let receipt_key = "main-one:main-incarnation-one:focused-postclaim-terminalize-0001";
    let progress = &progress_registry["receipts"][receipt_key]["outcome"];
    assert_eq!(
        progress["schema_version"],
        "main-agent.worker-reconcile-stopped-progress.v1"
    );
    assert_eq!(progress["state"], "in_progress");
    assert_eq!(progress["stage"], "authority_quarantined");
    assert_eq!(
        progress_registry["assignments"]["assignment-stopped"]["state"],
        "cancelled"
    );

    let retry = fixture.run_reconcile();
    assert_eq!(retry.code, 0, "stderr={}", retry.stderr_text());
    assert_eq!(
        data(&retry)["schema_version"],
        "main-agent.worker-reconcile-stopped-result.v2"
    );
    for outcome in [data(&normal), data(&retry)] {
        assert_eq!(outcome["worker_claim_active_after"], false);
        assert_eq!(
            outcome["proof"]["worker_claim"],
            json!({
                "active_disposition": "absent",
                "release_provenance": "not_attributed_to_attempt",
                "observed_at_stage1": true
            })
        );
    }
    assert_eq!(
        json!({
            "worker_claim_active_after": data(&normal)["worker_claim_active_after"],
            "worker_claim": data(&normal)["proof"]["worker_claim"]
        }),
        json!({
            "worker_claim_active_after": data(&retry)["worker_claim_active_after"],
            "worker_claim": data(&retry)["proof"]["worker_claim"]
        }),
        "normal completion and after-seal recovery must report the same terminal claim truth"
    );
    let coordination_after_retry = load_coordination_registry(&fixture.state_dir);
    let orchestration_after_retry = orchestration_registry(&fixture.state_dir);
    assert_eq!(
        orchestration_after_retry["receipts"][receipt_key]["outcome"],
        data(&retry)
    );

    let replay = fixture.run_reconcile();
    assert_eq!(replay.code, 0, "stderr={}", replay.stderr_text());
    assert_eq!(
        replay.stdout_text(),
        retry.stdout_text(),
        "a completed after-seal recovery must replay byte-stably"
    );
    assert_eq!(
        load_coordination_registry(&fixture.state_dir),
        coordination_after_retry,
        "a completed replay must not mutate coordination state"
    );
    assert_eq!(
        orchestration_registry(&fixture.state_dir),
        orchestration_after_retry,
        "a completed replay must not mutate orchestration state"
    );
}

#[test]
fn reconcile_stopped_same_main_successor_claim_authorizes_roll_forward() {
    let fixture = StoppedPostClaimFixture::new();
    let barrier = fixture
        ._tmp
        .path()
        .join("reconcile-controller-replacement-barrier");
    fs::create_dir(&barrier).expect("barrier");
    let first = fixture.spawn_reconcile_at_barrier(&barrier, "after_terminal_commit");
    wait_for_barrier(&barrier);
    let mut first = KillChild(Some(first));
    first.0.as_mut().expect("child").kill().expect("kill child");
    first.0.as_mut().expect("child").wait().expect("wait child");
    first.0 = None;

    let coordination_before = load_coordination_registry(&fixture.state_dir);
    let target_before = coordination_authority_snapshot(&coordination_before, "worker-stopped");
    let progress_registry = orchestration_registry(&fixture.state_dir);
    let receipt_key = "main-one:main-incarnation-one:focused-postclaim-terminalize-0001";
    let progress = &progress_registry["receipts"][receipt_key]["outcome"];
    let original_claim_id = progress["controller_claim_id"]
        .as_str()
        .expect("controller claim id");
    let original_claim_revision = progress["controller_claim_revision"]
        .as_u64()
        .expect("controller claim revision");

    let released = run(
        &fixture.checkout,
        &[
            "--state-dir",
            fixture.state_dir.to_str().expect("state dir"),
            "work-context",
            "release",
            "--session",
            "main-one",
            "--claim",
            original_claim_id,
            "--if-revision",
            &original_claim_revision.to_string(),
            "--capability-file",
            &fixture.main_capability,
            "--idempotency-key",
            "replace-controller-release-0001",
            "--format",
            "json",
        ],
    );
    assert_eq!(released.code, 0, "stderr={}", released.stderr_text());
    let candidate_file = fixture._tmp.path().join("replacement-controller.json");
    candidate(
        &candidate_file,
        "crates/agent-session/",
        "Same Main successor claim explicitly reauthorizes terminalization",
    );
    let replacement = run(
        &fixture.checkout,
        &[
            "--state-dir",
            fixture.state_dir.to_str().expect("state dir"),
            "work-context",
            "claim",
            "--session",
            "main-one",
            "--file",
            candidate_file.to_str().expect("candidate"),
            "--capability-file",
            &fixture.main_capability,
            "--idempotency-key",
            "replace-controller-claim-0001",
            "--format",
            "json",
        ],
    );
    assert_eq!(replacement.code, 0, "stderr={}", replacement.stderr_text());
    assert_ne!(
        data(&replacement)["context"]["claim_id"],
        original_claim_id,
        "the test requires a distinct replacement claim"
    );

    let successor_claim_id = data(&replacement)["context"]["claim_id"]
        .as_str()
        .expect("successor claim id")
        .to_string();
    let successor_claim_revision = data(&replacement)["context"]["revision"]
        .as_u64()
        .expect("successor claim revision");
    let successor_claim_expiry = load_coordination_registry(&fixture.state_dir)["claims"]
        .as_array()
        .expect("claims")
        .iter()
        .find(|claim| claim["claim_id"] == successor_claim_id)
        .expect("successor claim")["expires_at_epoch"]
        .as_i64()
        .expect("successor claim expiry");

    let replay = fixture.run_reconcile();
    assert_eq!(
        replay.code,
        0,
        "stdout={} stderr={}",
        replay.stdout_text(),
        replay.stderr_text()
    );
    let result = data(&replay);
    assert_eq!(
        result["proof"]["controller_authorization"]["mode"],
        "successor"
    );
    assert_eq!(
        result["proof"]["controller_authorization"]["original"]["claim_id"],
        original_claim_id
    );
    assert_eq!(
        result["proof"]["controller_authorization"]["original"]["revision"],
        original_claim_revision
    );
    assert_eq!(
        result["proof"]["controller_authorization"]["continuation"]["claim_id"],
        successor_claim_id
    );
    assert_eq!(
        result["proof"]["controller_authorization"]["continuation"]["revision"],
        successor_claim_revision
    );
    assert_eq!(
        result["proof"]["controller_authorization"]["continuation"]["expires_at_epoch"],
        successor_claim_expiry
    );
    assert_ne!(
        coordination_authority_snapshot(
            &load_coordination_registry(&fixture.state_dir),
            "worker-stopped"
        ),
        target_before,
        "an explicitly bound same-Main successor must seal target authority"
    );
    assert!(!Path::new(&fixture.worker_capability).exists());
    assert_frozen_base_v3_registry_compatible(&orchestration_registry(&fixture.state_dir));

    let retired = run_main_agent(
        &fixture.checkout,
        &[
            "--state-dir",
            fixture.state_dir.to_str().expect("state dir"),
            "worker",
            "retire",
            "assignment-stopped",
            "--if-revision",
            "4",
            "--idempotency-key",
            "successor-roll-forward-retire-0001",
            "--format",
            "json",
        ],
        &fixture.envs(),
    );
    assert_eq!(retired.code, 0, "stderr={}", retired.stderr_text());
    assert!(
        !fixture.state_dir.join("sessions/worker-stopped").exists(),
        "ordinary retirement must be reachable after successor roll-forward"
    );
}

#[test]
fn reconcile_stopped_controller_claim_revision_change_cannot_authorize_seal() {
    let fixture = StoppedPostClaimFixture::new();
    let barrier = fixture
        ._tmp
        .path()
        .join("reconcile-controller-revision-barrier");
    fs::create_dir(&barrier).expect("barrier");
    let first = fixture.spawn_reconcile_at_barrier(&barrier, "after_terminal_commit");
    wait_for_barrier(&barrier);

    let coordination_before = load_coordination_registry(&fixture.state_dir);
    let target_before = coordination_authority_snapshot(&coordination_before, "worker-stopped");
    let progress_registry = orchestration_registry(&fixture.state_dir);
    let receipt_key = "main-one:main-incarnation-one:focused-postclaim-terminalize-0001";
    let progress = &progress_registry["receipts"][receipt_key]["outcome"];
    let claim_id = progress["controller_claim_id"]
        .as_str()
        .expect("controller claim id");
    let claim_revision = progress["controller_claim_revision"]
        .as_u64()
        .expect("controller claim revision");
    let renewed = run(
        &fixture.checkout,
        &[
            "--state-dir",
            fixture.state_dir.to_str().expect("state dir"),
            "work-context",
            "renew",
            "--session",
            "main-one",
            "--claim",
            claim_id,
            "--if-revision",
            &claim_revision.to_string(),
            "--capability-file",
            &fixture.main_capability,
            "--idempotency-key",
            "change-controller-revision-0001",
            "--format",
            "json",
        ],
    );
    assert_eq!(renewed.code, 0, "stderr={}", renewed.stderr_text());
    assert_ne!(data(&renewed)["revision"], claim_revision);

    fs::write(barrier.join("release"), b"release").expect("release barrier");
    let first = first.wait_with_output().expect("first reconcile output");
    assert_eq!(first.status.code(), Some(65));
    let first: serde_json::Value =
        serde_json::from_slice(&first.stdout).expect("first reconcile json");
    assert_eq!(first["error"]["code"], "claim-not-active");
    assert_eq!(
        coordination_authority_snapshot(
            &load_coordination_registry(&fixture.state_dir),
            "worker-stopped"
        ),
        target_before,
        "a changed controller claim revision must not mutate target authority"
    );
    assert!(Path::new(&fixture.worker_capability).exists());
}

#[test]
fn reconcile_stopped_expired_original_accepts_same_main_successor_claim() {
    let fixture = StoppedPostClaimFixture::new();
    let barrier = fixture
        ._tmp
        .path()
        .join("reconcile-expired-controller-successor-barrier");
    fs::create_dir(&barrier).expect("barrier");
    let first = fixture.spawn_reconcile_at_barrier(&barrier, "after_terminal_commit");
    wait_for_barrier(&barrier);
    let mut first = KillChild(Some(first));
    first.0.as_mut().expect("child").kill().expect("kill child");
    first.0.as_mut().expect("child").wait().expect("wait child");
    first.0 = None;

    let receipt_key = "main-one:main-incarnation-one:focused-postclaim-terminalize-0001";
    let progress_registry = orchestration_registry(&fixture.state_dir);
    let original_claim_id =
        progress_registry["receipts"][receipt_key]["outcome"]["controller_claim_id"]
            .as_str()
            .expect("original claim id")
            .to_string();
    rewrite_registry(&fixture.state_dir, |registry| {
        let original = registry["claims"]
            .as_array_mut()
            .expect("claims")
            .iter_mut()
            .find(|claim| claim["claim_id"] == original_claim_id)
            .expect("original controller claim");
        original["state"] = json!("expired");
        original["revision"] = json!(
            original["revision"]
                .as_u64()
                .expect("claim revision")
                .saturating_add(1)
        );
        original["expires_at"] = json!("1970-01-01T00:00:00Z");
        original["expires_at_epoch"] = json!(0);
        original["terminal_at_epoch"] = json!(0);
    });
    let candidate_file = fixture
        ._tmp
        .path()
        .join("expired-successor-controller.json");
    candidate(
        &candidate_file,
        "crates/agent-session/",
        "Same Main successor replaces expired terminalization authority",
    );
    let successor = run(
        &fixture.checkout,
        &[
            "--state-dir",
            fixture.state_dir.to_str().expect("state dir"),
            "work-context",
            "claim",
            "--session",
            "main-one",
            "--file",
            candidate_file.to_str().expect("candidate"),
            "--capability-file",
            &fixture.main_capability,
            "--idempotency-key",
            "expired-controller-successor-0001",
            "--format",
            "json",
        ],
    );
    assert_eq!(successor.code, 0, "stderr={}", successor.stderr_text());
    let successor_claim_id = data(&successor)["context"]["claim_id"]
        .as_str()
        .expect("successor claim id")
        .to_string();
    assert_ne!(successor_claim_id, original_claim_id);

    let replay = fixture.run_reconcile();
    assert_eq!(replay.code, 0, "stderr={}", replay.stderr_text());
    assert_eq!(
        data(&replay)["proof"]["controller_authorization"]["mode"],
        "successor"
    );
    assert_eq!(
        data(&replay)["proof"]["controller_authorization"]["original"]["claim_id"],
        original_claim_id
    );
    assert_eq!(
        data(&replay)["proof"]["controller_authorization"]["continuation"]["claim_id"],
        successor_claim_id
    );
    assert!(!Path::new(&fixture.worker_capability).exists());
}

#[test]
fn reconcile_stopped_different_session_cannot_take_over_progress() {
    let fixture = StoppedPostClaimFixture::new();
    let barrier = fixture
        ._tmp
        .path()
        .join("reconcile-different-controller-barrier");
    fs::create_dir(&barrier).expect("barrier");
    let first = fixture.spawn_reconcile_at_barrier(&barrier, "after_terminal_commit");
    wait_for_barrier(&barrier);

    let target_before = coordination_authority_snapshot(
        &load_coordination_registry(&fixture.state_dir),
        "worker-stopped",
    );
    let mut foreign_env = fixture.envs();
    foreign_env[0] = (
        "AGENT_SESSION_CAPABILITY_FILE",
        fixture.worker_capability.as_str(),
    );
    let foreign = run_main_agent(&fixture.checkout, &fixture.reconcile_args(), &foreign_env);
    assert_eq!(foreign.code, 65, "stdout={}", foreign.stdout_text());
    assert_eq!(
        foreign.stdout_json()["error"]["code"],
        "controller-rebind-required"
    );
    assert_eq!(
        coordination_authority_snapshot(
            &load_coordination_registry(&fixture.state_dir),
            "worker-stopped"
        ),
        target_before,
        "a different session/incarnation/owner must not touch target authority"
    );

    fs::write(barrier.join("release"), b"release").expect("release barrier");
    let first = first.wait_with_output().expect("first reconcile output");
    assert!(
        first.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&first.stdout),
        String::from_utf8_lossy(&first.stderr)
    );
}

#[test]
fn reconcile_stopped_prior_external_release_reports_stable_terminal_claim_truth() {
    let fixture = StoppedPostClaimFixture::new();
    let barrier = fixture
        ._tmp
        .path()
        .join("reconcile-actual-claim-release-outcome-barrier");
    fs::create_dir(&barrier).expect("barrier");
    let first = fixture.spawn_reconcile_at_barrier(&barrier, "after_terminal_commit");
    wait_for_barrier(&barrier);
    let mut first = KillChild(Some(first));
    first.0.as_mut().expect("child").kill().expect("kill child");
    first.0.as_mut().expect("child").wait().expect("wait child");
    first.0 = None;

    rewrite_registry(&fixture.state_dir, |registry| {
        for claim in registry["claims"].as_array_mut().expect("claims") {
            if claim["session_id"] == "worker-stopped" && claim["state"] == "active" {
                claim["state"] = json!("released");
                claim["revision"] = json!(
                    claim["revision"]
                        .as_u64()
                        .expect("claim revision")
                        .saturating_add(1)
                );
                claim["terminal_at_epoch"] = json!(0);
            }
        }
    });

    let replay = fixture.run_reconcile();
    assert_eq!(replay.code, 0, "stderr={}", replay.stderr_text());
    assert_eq!(data(&replay)["worker_claim_active_after"], false);
    assert_eq!(
        data(&replay)["proof"]["worker_claim"],
        json!({
            "active_disposition": "absent",
            "release_provenance": "not_attributed_to_attempt",
            "observed_at_stage1": true
        })
    );
    assert!(!Path::new(&fixture.worker_capability).exists());
}

#[test]
fn reconcile_stopped_completed_v1_receipt_replays_byte_stably() {
    let fixture = StoppedPostClaimFixture::new();
    let completed = fixture.run_reconcile();
    assert_eq!(completed.code, 0, "stderr={}", completed.stderr_text());

    let receipt_key = "main-one:main-incarnation-one:focused-postclaim-terminalize-0001";
    let mut prior = data(&completed).clone();
    prior["schema_version"] = json!("main-agent.worker-reconcile-stopped-result.v1");
    prior
        .as_object_mut()
        .expect("result object")
        .remove("worker_claim_active_after");
    prior["worker_claim_revoked"] = json!(true);
    prior["proof"]["worker_claim"] = json!("revoked");
    rewrite_orchestration_registry(&fixture.state_dir, |registry| {
        registry["receipts"][receipt_key]["outcome"] = prior.clone();
    });

    let replay = fixture.run_reconcile();
    assert_eq!(replay.code, 0, "stderr={}", replay.stderr_text());
    assert_eq!(
        data(&replay),
        prior,
        "an already-completed v1 receipt must remain byte-stable on exact replay"
    );
    let replay_again = fixture.run_reconcile();
    assert_eq!(
        replay_again.code,
        0,
        "stderr={}",
        replay_again.stderr_text()
    );
    assert_eq!(
        replay_again.stdout_text(),
        replay.stdout_text(),
        "completed v1 replays must remain byte-stable"
    );
}

#[test]
fn reconcile_stopped_fresh_replay_does_not_renew_expired_controller_claim() {
    let fixture = StoppedPostClaimFixture::new();
    let barrier = fixture
        ._tmp
        .path()
        .join("reconcile-expired-controller-fresh-replay-barrier");
    fs::create_dir(&barrier).expect("barrier");
    let first = fixture.spawn_reconcile_at_barrier(&barrier, "after_terminal_commit");
    wait_for_barrier(&barrier);
    let mut first = KillChild(Some(first));
    first.0.as_mut().expect("child").kill().expect("kill child");
    first.0.as_mut().expect("child").wait().expect("wait child");
    first.0 = None;

    let coordination_before = load_coordination_registry(&fixture.state_dir);
    assert_eq!(
        coordination_before["brokers"]["main-one"]["state"], "ready",
        "the controller broker must remain healthy for the renewal regression"
    );
    assert!(Path::new(&fixture.main_capability).exists());
    let target_before = coordination_authority_snapshot(&coordination_before, "worker-stopped");
    let progress_registry = orchestration_registry(&fixture.state_dir);
    let receipt_key = "main-one:main-incarnation-one:focused-postclaim-terminalize-0001";
    let claim_id = progress_registry["receipts"][receipt_key]["outcome"]["controller_claim_id"]
        .as_str()
        .expect("controller claim id")
        .to_string();
    rewrite_registry(&fixture.state_dir, |registry| {
        let controller = registry["claims"]
            .as_array_mut()
            .expect("claims")
            .iter_mut()
            .find(|claim| claim["claim_id"] == claim_id)
            .expect("controller claim");
        controller["expires_at"] = json!("1970-01-01T00:00:00Z");
        controller["expires_at_epoch"] = json!(0);
    });

    let replay = fixture.run_reconcile();
    assert_eq!(replay.code, 65, "stdout={}", replay.stdout_text());
    assert_eq!(replay.stdout_json()["error"]["code"], "claim-not-active");
    let after = load_coordination_registry(&fixture.state_dir);
    assert_eq!(
        coordination_authority_snapshot(&after, "worker-stopped"),
        target_before,
        "an expired controller claim must fail before target cleanup"
    );
    assert_eq!(
        after["claims"]
            .as_array()
            .expect("claims")
            .iter()
            .find(|claim| claim["claim_id"] == claim_id)
            .expect("controller claim")["expires_at_epoch"],
        0,
        "fresh replay must not opportunistically renew the expired claim"
    );
    assert!(Path::new(&fixture.worker_capability).exists());
}

#[test]
fn reconcile_stopped_controller_claim_release_before_seal_preserves_target_authority() {
    let fixture = StoppedPostClaimFixture::new();
    let barrier = fixture
        ._tmp
        .path()
        .join("reconcile-controller-release-barrier");
    fs::create_dir(&barrier).expect("barrier");
    let first = fixture.spawn_reconcile_at_barrier(&barrier, "after_terminal_commit");
    wait_for_barrier(&barrier);

    let before = load_coordination_registry(&fixture.state_dir);
    let target_broker_before = before["brokers"]["worker-stopped"].clone();
    let target_claims_before = before["claims"]
        .as_array()
        .expect("claims")
        .iter()
        .filter(|claim| claim["session_id"] == "worker-stopped")
        .cloned()
        .collect::<Vec<_>>();
    assert!(Path::new(&fixture.worker_capability).exists());

    let shown = run(
        &fixture.checkout,
        &[
            "--state-dir",
            fixture.state_dir.to_str().expect("state dir"),
            "work-context",
            "show",
            "--session",
            "main-one",
            "--capability-file",
            &fixture.main_capability,
            "--format",
            "json",
        ],
    );
    assert_eq!(shown.code, 0, "stderr={}", shown.stderr_text());
    let main_claim = data(&shown);
    let released = run(
        &fixture.checkout,
        &[
            "--state-dir",
            fixture.state_dir.to_str().expect("state dir"),
            "work-context",
            "release",
            "--session",
            "main-one",
            "--claim",
            main_claim["claim_id"].as_str().expect("claim id"),
            "--if-revision",
            &main_claim["revision"].to_string(),
            "--capability-file",
            &fixture.main_capability,
            "--idempotency-key",
            "release-controller-before-target-seal-0001",
            "--format",
            "json",
        ],
    );
    assert_eq!(released.code, 0, "stderr={}", released.stderr_text());

    fs::write(barrier.join("release"), b"release").expect("release barrier");
    let first = first.wait_with_output().expect("first reconcile output");
    assert_eq!(first.status.code(), Some(65));
    let first: serde_json::Value =
        serde_json::from_slice(&first.stdout).expect("first reconcile json");
    assert_eq!(first["error"]["code"], "claim-not-active");

    let after = load_coordination_registry(&fixture.state_dir);
    assert_eq!(
        after["brokers"]["worker-stopped"], target_broker_before,
        "controller authority loss must not stop the target broker"
    );
    let target_claims_after = after["claims"]
        .as_array()
        .expect("claims")
        .iter()
        .filter(|claim| claim["session_id"] == "worker-stopped")
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(
        target_claims_after, target_claims_before,
        "controller authority loss must not mutate the target claim"
    );
    assert!(
        Path::new(&fixture.worker_capability).exists(),
        "controller authority loss must not remove the target capability"
    );
    assert!(
        fixture
            .state_dir
            .join("sessions/worker-stopped/authority-quarantine.json")
            .is_file(),
        "the already-committed quarantine keeps resume fail-closed"
    );
}

#[test]
fn reconcile_stopped_controller_claim_expiry_before_seal_preserves_target_authority() {
    let fixture = StoppedPostClaimFixture::new();
    let barrier = fixture
        ._tmp
        .path()
        .join("reconcile-controller-expiry-barrier");
    fs::create_dir(&barrier).expect("barrier");
    let first = fixture.spawn_reconcile_at_barrier(&barrier, "after_terminal_commit");
    wait_for_barrier(&barrier);

    let before = load_coordination_registry(&fixture.state_dir);
    let target_broker_before = before["brokers"]["worker-stopped"].clone();
    let target_claims_before = before["claims"]
        .as_array()
        .expect("claims")
        .iter()
        .filter(|claim| claim["session_id"] == "worker-stopped")
        .cloned()
        .collect::<Vec<_>>();
    rewrite_registry(&fixture.state_dir, |registry| {
        let controller = registry["claims"]
            .as_array_mut()
            .expect("claims")
            .iter_mut()
            .find(|claim| {
                claim["session_id"] == "main-one"
                    && claim["session_incarnation"] == "main-incarnation-one"
                    && claim["state"] == "active"
            })
            .expect("active controller claim");
        controller["expires_at"] = json!("1970-01-01T00:00:00Z");
        controller["expires_at_epoch"] = json!(0);
    });

    fs::write(barrier.join("release"), b"release").expect("release barrier");
    let first = first.wait_with_output().expect("first reconcile output");
    assert_eq!(first.status.code(), Some(65));
    let first: serde_json::Value =
        serde_json::from_slice(&first.stdout).expect("first reconcile json");
    assert_eq!(first["error"]["code"], "claim-not-active");

    let after = load_coordination_registry(&fixture.state_dir);
    assert_eq!(after["brokers"]["worker-stopped"], target_broker_before);
    let target_claims_after = after["claims"]
        .as_array()
        .expect("claims")
        .iter()
        .filter(|claim| claim["session_id"] == "worker-stopped")
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(target_claims_after, target_claims_before);
    assert!(Path::new(&fixture.worker_capability).exists());
}

#[test]
fn reconcile_stopped_malformed_progress_fails_before_target_cleanup() {
    let fixture = StoppedPostClaimFixture::new();
    let barrier = fixture._tmp.path().join("reconcile-malformed-barrier");
    fs::create_dir(&barrier).expect("barrier");
    let first = fixture.spawn_reconcile_at_barrier(&barrier, "after_terminal_commit");
    wait_for_barrier(&barrier);
    let mut first = KillChild(Some(first));
    first.0.as_mut().expect("child").kill().expect("kill child");
    first.0.as_mut().expect("child").wait().expect("wait child");
    first.0 = None;

    let receipt_key = "main-one:main-incarnation-one:focused-postclaim-terminalize-0001";
    let valid =
        orchestration_registry(&fixture.state_dir)["receipts"][receipt_key]["outcome"].clone();
    let coordination_before = load_coordination_registry(&fixture.state_dir);
    let mut cases = Vec::new();
    for field in [
        "controller_claim_id",
        "controller_claim_revision",
        "controller_claim_expires_at_epoch",
        "runtime_identity_digest",
        "worker_claim_observed",
    ] {
        let mut missing = valid.clone();
        missing
            .as_object_mut()
            .expect("progress object")
            .remove(field);
        cases.push((format!("missing-{field}"), missing));
    }
    for (name, field, value) in [
        ("empty-digest", "runtime_identity_digest", json!("")),
        ("bad-digest", "runtime_identity_digest", json!("invalid")),
        (
            "claim-flag-not-bool",
            "worker_claim_observed",
            json!("true"),
        ),
        ("empty-controller-claim", "controller_claim_id", json!("")),
        (
            "zero-controller-revision",
            "controller_claim_revision",
            json!(0),
        ),
        (
            "invalid-controller-expiry",
            "controller_claim_expires_at_epoch",
            json!(0),
        ),
        (
            "assignment-mismatch",
            "assignment_id",
            json!("another-assignment"),
        ),
        ("invalid-stage", "stage", json!("authority_sealed")),
    ] {
        let mut malformed = valid.clone();
        malformed[field] = value;
        cases.push((name.to_string(), malformed));
    }

    for (name, malformed) in cases {
        rewrite_orchestration_registry(&fixture.state_dir, |registry| {
            registry["receipts"][receipt_key]["outcome"] = malformed;
        });
        let refused = fixture.run_reconcile();
        assert_eq!(refused.code, 65, "case={name}: {}", refused.stdout_text());
        assert_eq!(
            refused.stdout_json()["error"]["code"],
            "orchestration-store-invalid",
            "case={name}"
        );
        assert_eq!(
            load_coordination_registry(&fixture.state_dir),
            coordination_before,
            "malformed progress must fail before target cleanup: case={name}"
        );
        assert!(
            Path::new(&fixture.worker_capability).exists(),
            "malformed progress must preserve the target capability: case={name}"
        );
    }
}

#[test]
fn reconcile_stopped_quarantine_blocks_http_resume_before_authority_provisioning() {
    let fixture = StoppedPostClaimFixture::new();
    let terminalized = fixture.run_reconcile();
    assert_eq!(
        terminalized.code,
        0,
        "stderr={}",
        terminalized.stderr_text()
    );
    let worker_record_path = fixture
        .state_dir
        .join("sessions/worker-stopped/session.json");
    let runtime_before =
        serde_json::from_slice::<serde_json::Value>(&fs::read(&worker_record_path).unwrap())
            .unwrap()["runtime"]
            .clone();

    let listener = TcpListener::bind("127.0.0.1:0").expect("reserve HTTP port");
    let address = listener.local_addr().expect("HTTP address");
    drop(listener);
    let token = "postclaim-http-test-token";
    let mut server = KillChild(Some(
        Command::new(bin::resolve("agent-session"))
            .current_dir(&fixture.checkout)
            .args([
                "serve",
                "--bind",
                &address.to_string(),
                "--state-dir",
                fixture.state_dir.to_str().expect("state dir"),
                "--token",
                token,
                "--tmux-bin",
                fixture.tmux_bin.to_str().expect("tmux bin"),
            ])
            .env("AGENT_SESSION_FAKE_TMUX_ABSENT", "1")
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn HTTP server"),
    ));
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match TcpStream::connect(address) {
            Ok(stream) => {
                drop(stream);
                break;
            }
            Err(_) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("HTTP server did not start: {error}"),
        }
    }
    let response = post_json_over_http(
        &address.to_string(),
        "/sessions/worker-stopped/resume",
        token,
    );
    assert_eq!(response["ok"], false);
    assert_eq!(response["error"]["code"], "worker-quarantined");
    let runtime_after =
        serde_json::from_slice::<serde_json::Value>(&fs::read(&worker_record_path).unwrap())
            .unwrap()["runtime"]
            .clone();
    assert_eq!(runtime_after, runtime_before);
    assert!(!Path::new(&fixture.worker_capability).exists());
    let _ = server.0.as_mut().expect("server").kill();
}

#[test]
fn main_agent_supervise_exposes_the_fail_closed_classification_matrix_without_mutation() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let main_checkout = tmp.path().join("main-checkout");
    let worker_checkout = tmp.path().join("worker-checkout");
    fs::create_dir(&state_dir).expect("state");
    init_checkout(
        &main_checkout,
        "https://example.invalid/example/repository.git",
    );
    init_checkout(
        &worker_checkout,
        "https://example.invalid/example/repository.git",
    );
    seed_brokers_at(
        &state_dir,
        &[
            (
                "main-one",
                "main-incarnation-one",
                "main-private-capability-material-0000000001",
                main_checkout.as_path(),
                Some("enforce"),
            ),
            (
                "main-two",
                "main-incarnation-two",
                "main-private-capability-material-0000000002",
                main_checkout.as_path(),
                Some("enforce"),
            ),
            (
                "worker-matrix",
                "worker-matrix-incarnation",
                "worker-matrix-private-capability-000000001",
                worker_checkout.as_path(),
                Some("enforce"),
            ),
        ],
    );
    let main_capability = init_main_run(
        tmp.path(),
        &state_dir,
        &main_checkout,
        "main-one",
        "run-one",
    );
    seed_active_claim(
        &state_dir,
        "main-two",
        "main-incarnation-two",
        "main-two-claim",
    );
    rewrite_orchestration_registry(&state_dir, |registry| {
        let mut run_two = registry["runs"]["run-one"].clone();
        run_two["run_id"] = json!("run-two");
        run_two["revision"] = json!(1);
        run_two["objective_summary"] = json!("Receive raced assignment handoff");
        run_two["controller"]["session_id"] = json!("main-two");
        run_two["controller"]["session_incarnation"] = json!("main-incarnation-two");
        registry["runs"]["run-two"] = run_two;
    });
    let packet = json!({
        "schema_version": "main-agent.assignment-input.v1",
        "assignment_id": "assignment-matrix",
        "task_summary": "Exercise supervisor classifications",
        "task": {},
        "launch": {
            "agent": "codex",
            "cwd": worker_checkout,
            "title": null,
            "session_id": "worker-matrix",
            "coordination_mode": "enforce",
            "agent_args": []
        },
        "repository": "example/repository",
        "worktree": worker_checkout,
        "base_ref": "main",
        "scopes": ["docs/matrix"],
        "durable_refs": []
    });
    insert_orchestration_assignment(
        &state_dir,
        "assignment-matrix",
        json!({
            "schema_version": "agent-session.orchestration-assignment.v1",
            "assignment_id": "assignment-matrix",
            "run_id": "run-one",
            "revision": 2,
            "state": "working",
            "task_summary": "Exercise supervisor classifications",
            "private_packet_digest": "replaced-by-fixture",
            "primary_manager": {
                "session_id": "main-one",
                "session_incarnation": "main-incarnation-one",
                "session_created_at": "2030-01-01T00:00:00Z"
            },
            "worker": {
                "session_id": "worker-matrix",
                "session_incarnation": "worker-matrix-incarnation",
                "session_created_at": "2030-01-01T00:00:00Z"
            },
            "collaborators": [],
            "borrowed_by": [],
            "repository": "example/repository",
            "worktree": worker_checkout,
            "base_ref": "main",
            "scopes": ["docs/matrix"],
            "durable_refs": [],
            "checkpoint": {
                "revision": 2,
                "summary": "Worker active",
                "next_action": "Continue",
                "updated_at": "2030-01-01T00:00:00Z"
            },
            "result_summary": null,
            "blocker_summary": null,
            "created_at": "2030-01-01T00:00:00Z",
            "updated_at": "2030-01-01T00:00:00Z"
        }),
        &packet,
    );
    seed_active_claim(
        &state_dir,
        "worker-matrix",
        "worker-matrix-incarnation",
        "worker-matrix-claim",
    );
    rewrite_registry(&state_dir, |registry| {
        let claim = registry["claims"]
            .as_array_mut()
            .expect("claims")
            .iter_mut()
            .find(|claim| claim["claim_id"] == "worker-matrix-claim")
            .expect("worker matrix claim");
        claim["repositories"] = json!(["example/repository"]);
        claim["scopes"] = json!([{
            "kind": "path-prefix",
            "repository": "example/repository",
            "value": "docs/matrix"
        }]);
    });
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let tmux_arg = tmux_bin.to_string_lossy().into_owned();
    let tmux_log_arg = tmux_log.to_string_lossy().into_owned();
    let account_broker = r#"["/bin/false"]"#;
    let observe = |command| {
        run_main_agent(
            &main_checkout,
            &[
                "--state-dir",
                state_dir.to_str().expect("state dir"),
                "worker",
                command,
                "assignment-matrix",
                "--format",
                "json",
            ],
            &[
                ("AGENT_SESSION_CAPABILITY_FILE", &main_capability),
                ("AGENT_SESSION_TMUX_BIN", &tmux_arg),
                ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
                ("AGENT_SESSION_CODEX_ACCOUNT_BROKER", account_broker),
            ],
        )
    };
    let supervise = || observe("supervise");

    let diagnosed = observe("diagnose");
    assert_eq!(diagnosed.code, 0, "stderr={}", diagnosed.stderr_text());
    let diagnosed = data(&diagnosed);
    assert_eq!(
        diagnosed["schema_version"],
        "main-agent.worker-diagnose-result.v2"
    );
    let expected_diagnose_keys = BTreeSet::from([
        "account",
        "activity",
        "assignment_id",
        "assignment_revision",
        "assignment_state",
        "auto_resume",
        "automatic_retry_safe",
        "cancel_then_reassign_safe",
        "classification",
        "coordination",
        "evidence",
        "failed_preclaim",
        "guidance",
        "new_assignment_safe",
        "next_action",
        "post_claim_terminalization_safe",
        "progress",
        "provider_resume_preserved",
        "quota_or_credit_evidence",
        "raw_rate_limit_diagnostic",
        "reassignment_safe",
        "recovery_action",
        "schema_version",
        "submit_recovery",
        "worker",
        "worktree_progress",
    ]);
    assert_eq!(
        diagnosed
            .as_object()
            .expect("v2 diagnosis object")
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>(),
        expected_diagnose_keys
    );
    let healthy = supervise();
    assert_eq!(healthy.code, 0, "stderr={}", healthy.stderr_text());
    let healthy = data(&healthy);
    assert_eq!(healthy["classification"], "healthy_progress");
    assert_eq!(
        healthy["schema_version"],
        "main-agent.worker-supervise-result.v2"
    );
    assert_eq!(
        healthy
            .as_object()
            .expect("v2 supervise object")
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([
            "assignment_id",
            "automatic_retry_safe",
            "classification",
            "last_proven_safe_state",
            "next_action",
            "recovery_action",
            "schema_version",
        ])
    );
    assert_eq!(healthy["last_proven_safe_state"]["assignment_revision"], 2);
    assert_eq!(
        healthy["last_proven_safe_state"]
            .as_object()
            .expect("nested v2 diagnosis")
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>(),
        expected_diagnose_keys
    );

    let progress_file = worker_checkout.join("large-progress.bin");
    let progress_len = 512 * 1024 + 64 * 1024;
    let mut base_state = 0x6a09_e667_f3bc_c909_u64;
    let mut base_material = Vec::with_capacity(progress_len);
    for _ in 0..progress_len {
        base_state ^= base_state << 13;
        base_state ^= base_state >> 7;
        base_state ^= base_state << 17;
        base_material.push(base_state as u8);
    }
    fs::write(&progress_file, &base_material).expect("large staged progress base");
    git_stdout(&worker_checkout, &["add", "large-progress.bin"]);
    let mut rewritten_material = base_material;
    for byte in &mut rewritten_material {
        *byte ^= 0xa5;
    }
    fs::write(&progress_file, &rewritten_material).expect("large unstaged progress rewrite");
    assert!(
        git_stdout(
            &worker_checkout,
            &["diff", "--cached", "--binary", "--full-index", "--"]
        )
        .len()
            > 512 * 1024,
        "public supervision regression requires staged output above the former cap"
    );
    assert!(
        git_stdout(
            &worker_checkout,
            &["diff", "--binary", "--full-index", "--"]
        )
        .len()
            > 512 * 1024,
        "public supervision regression requires unstaged output above the former cap"
    );
    let dirty_first = supervise();
    assert_eq!(dirty_first.code, 0, "stderr={}", dirty_first.stderr_text());
    assert_ne!(data(&dirty_first)["classification"], "evidence_unavailable");
    let dirty_fingerprint =
        data(&dirty_first)["last_proven_safe_state"]["worktree_progress"]["material_fingerprint"]
            .clone();
    let dirty_repeat = supervise();
    assert_eq!(
        data(&dirty_repeat)["last_proven_safe_state"]["worktree_progress"]["material_fingerprint"],
        dirty_fingerprint,
        "repeated public supervision must retain the exact unchanged fingerprint"
    );
    rewritten_material[progress_len / 2] ^= 0xff;
    fs::write(&progress_file, &rewritten_material).expect("same-size public progress rewrite");
    let dirty_rewritten = supervise();
    assert_ne!(
        data(&dirty_rewritten)["classification"],
        "evidence_unavailable"
    );
    assert_ne!(
        data(&dirty_rewritten)["last_proven_safe_state"]["worktree_progress"]["material_fingerprint"],
        dirty_fingerprint,
        "same-size material rewrites must advance public supervision evidence"
    );
    git_stdout(
        &worker_checkout,
        &["rm", "--cached", "--force", "large-progress.bin"],
    );
    fs::remove_file(progress_file).expect("remove public progress fixture");

    seed_activity_state(
        &state_dir,
        "worker-matrix",
        "worker-matrix-incarnation",
        "needs_input",
        json!({
            "provider_turn_id": "turn-matrix",
            "started_at": "2030-01-01T00:00:01Z",
            "attention": {
                "kind": "permission",
                "requested_at": "2030-01-01T00:00:02Z",
                "pending_count": 1
            }
        }),
        serde_json::Value::Null,
    );
    let dialog = supervise();
    assert_eq!(data(&dialog)["classification"], "startup_dialog_failure");
    assert_eq!(data(&dialog)["automatic_retry_safe"], false);

    let worker_record_path = state_dir.join("sessions/worker-matrix/session.json");
    let mut worker_record: serde_json::Value =
        serde_json::from_slice(&fs::read(&worker_record_path).expect("worker record"))
            .expect("worker record json");
    worker_record["provider_resume"] = json!({
        "provider": "codex",
        "session_id": "provider-thread-matrix",
        "captured_at": "2030-01-01T00:00:00Z",
        "capture_method": "codex-session-meta",
        "resume_args": ["resume", "provider-thread-matrix"],
        "private_extra": "provider-resume-matrix-extra"
    });
    fs::write(
        &worker_record_path,
        serde_json::to_vec_pretty(&worker_record).expect("worker record bytes"),
    )
    .expect("write worker record");
    seed_activity_state(
        &state_dir,
        "worker-matrix",
        "worker-matrix-incarnation",
        "needs_input",
        json!({
            "provider_turn_id": "turn-quota",
            "started_at": "2020-01-01T00:00:01Z",
            "attention": {
                "kind": "quota_credits_exhausted",
                "requested_at": "2020-01-01T00:00:02Z",
                "pending_count": 1
            }
        }),
        serde_json::Value::Null,
    );
    let quota = supervise();
    assert_eq!(
        data(&quota)["classification"],
        "account_handoff_capability_gap"
    );
    assert_eq!(
        data(&quota)["last_proven_safe_state"]["provider_resume_preserved"],
        true,
        "diagnosis may expose preservation state without continuation metadata"
    );
    assert!(
        !quota.stdout_text().contains("provider-thread-matrix")
            && !quota.stdout_text().contains("provider-resume-matrix-extra"),
        "diagnosis must not expose provider resume metadata"
    );
    assert!(
        data(&quota)["next_action"]
            .as_str()
            .unwrap_or_default()
            .contains("agent-session.codex-managed-account-handoff.v1"),
        "the capability gap must name the stable managed handoff contract"
    );
    assert!(
        data(&quota)["next_action"]
            .as_str()
            .unwrap_or_default()
            .contains("terminal capability gap")
    );
    assert!(
        data(&quota)["next_action"]
            .as_str()
            .unwrap_or_default()
            .contains("cannot be added by worker reassign"),
        "diagnosis must make the reassignment boundary explicit"
    );
    let raw_worker_before = fs::read(&worker_record_path).expect("raw worker record");
    let raw_handoff = run_main_agent(
        &main_checkout,
        &[
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "worker",
            "account-handoff",
            "assignment-matrix",
            "--account",
            "beta",
            "--if-revision",
            "2",
            "--authorize-account-change",
            "--idempotency-key",
            "matrix-raw-handoff-0001",
            "--format",
            "json",
        ],
        &[
            ("AGENT_SESSION_CAPABILITY_FILE", &main_capability),
            ("AGENT_SESSION_CODEX_ACCOUNT_BROKER", account_broker),
        ],
    );
    assert_eq!(raw_handoff.code, 65);
    assert_eq!(
        raw_handoff.stdout_json()["error"]["code"],
        "account-handoff-capability-unavailable"
    );
    assert_eq!(
        raw_handoff.stdout_json()["error"]["details"]["required_capability"],
        "agent-session.codex-managed-account-handoff.v1"
    );
    assert_eq!(
        raw_handoff.stdout_json()["error"]["details"]["capability_gap_terminal_for_assignment"],
        true
    );
    assert_eq!(
        raw_handoff.stdout_json()["error"]["details"]["lifecycle_boundary"],
        "accept|release|cancel|retire"
    );
    assert_eq!(
        raw_handoff.stdout_json()["error"]["details"]["public_raw_fallback_advertised"],
        false
    );
    assert_eq!(
        fs::read(&worker_record_path).expect("raw worker record after handoff"),
        raw_worker_before,
        "unsupported raw handoff must not switch accounts, restart, or rewrite the session"
    );

    let progress_snapshot = fs::read_dir(state_dir.join("sessions/worker-matrix/coordination"))
        .expect("progress directory")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("main-agent-progress-"))
        })
        .expect("progress snapshot");
    let mut stale_snapshot: serde_json::Value =
        serde_json::from_slice(&fs::read(&progress_snapshot).expect("progress snapshot bytes"))
            .expect("progress snapshot json");
    stale_snapshot["changed_at_epoch"] = json!(1);
    write_private_json(&progress_snapshot, &stale_snapshot);
    seed_activity_state(
        &state_dir,
        "worker-matrix",
        "worker-matrix-incarnation",
        "working",
        json!({
            "provider_turn_id": "turn-raw-real-shape",
            "started_at": "2020-01-01T00:00:01Z"
        }),
        serde_json::Value::Null,
    );
    let real_raw_stall = supervise();
    assert_eq!(
        data(&real_raw_stall)["classification"],
        "evidence_unavailable",
        "a true stale raw stall without exact account provenance must fail closed"
    );
    assert_eq!(
        data(&real_raw_stall)["last_proven_safe_state"]["quota_or_credit_evidence"],
        false
    );
    assert_eq!(
        data(&real_raw_stall)["last_proven_safe_state"]["raw_rate_limit_diagnostic"]["state"],
        "unavailable"
    );
    assert_eq!(
        data(&real_raw_stall)["last_proven_safe_state"]["raw_rate_limit_diagnostic"]["reason_code"],
        "selected-raw-account-unavailable",
        "ambient authentication must never invent provenance for a raw worker"
    );

    let mut managed_worker: serde_json::Value =
        serde_json::from_slice(&fs::read(&worker_record_path).expect("worker record"))
            .expect("worker record json");
    managed_worker["runtime"]["kind"] = json!("codex_app_server");
    managed_worker["runtime"]["codex_app_server_protocol"] = json!("v2");
    managed_worker["runtime"]["codex_app_server_socket"] = json!("/tmp/matrix.sock");
    managed_worker["runtime"]["codex_app_server_proxy"] = json!("/tmp/matrix.proxy");
    managed_worker["runtime"]["codex_app_server_thread_handoff"] = json!("/tmp/matrix.thread");
    managed_worker["runtime"]["codex_app_server_thread_attached"] = json!("/tmp/matrix.attached");
    managed_worker["runtime"]["managed_account_handoff_capability"] =
        json!("agent-session.codex-managed-account-handoff.v1");
    managed_worker["codex_account_binding"] = json!({
        "schema_version": "agent-session.codex-account-binding.v1",
        "selected_account": "alpha",
        "revision": 1,
        "state": "bound",
        "applied_runtime_id": "worker-matrix-incarnation",
        "updated_at": "2030-01-01T00:00:00Z"
    });
    write_private_json(&worker_record_path, &managed_worker);
    seed_activity_state(
        &state_dir,
        "worker-matrix",
        "worker-matrix-incarnation",
        "needs_input",
        json!({
            "provider_turn_id": "turn-managed-quota",
            "started_at": "2020-01-01T00:00:01Z",
            "attention": {
                "kind": "quota_credits_exhausted",
                "requested_at": "2020-01-01T00:00:02Z",
                "pending_count": 1
            }
        }),
        json!({
            "provider_turn_id": "turn-managed-quota",
            "started_at": "2020-01-01T00:00:01Z",
            "completed_at": "2020-01-01T00:00:02Z",
            "outcome": "quota_credits_exhausted"
        }),
    );
    let managed_quota = supervise();
    assert_eq!(
        data(&managed_quota)["classification"],
        "account_handoff_required"
    );
    assert!(
        data(&managed_quota)["next_action"]
            .as_str()
            .unwrap_or_default()
            .contains("main-agent worker account-handoff"),
        "managed quota recovery must advertise the executable typed macro"
    );
    let invalid_account = run_managed_account_handoff(
        &main_checkout,
        &state_dir,
        &main_capability,
        "bad/account",
        "matrix-managed-invalid-account-0001",
        "success",
    );
    assert_eq!(invalid_account.code, 64);
    assert_eq!(
        invalid_account.stdout_json()["error"]["code"],
        "invalid-codex-account"
    );
    assert!(
        orchestration_registry(&state_dir)["assignments"]["assignment-matrix"]["account_handoff"]
            .is_null(),
        "invalid account input must fail before reserving the assignment"
    );
    let managed_handoff_args = [
        "--state-dir",
        state_dir.to_str().expect("state dir"),
        "worker",
        "account-handoff",
        "assignment-matrix",
        "--account",
        "beta",
        "--if-revision",
        "2",
        "--authorize-account-change",
        "--timeout",
        "1s",
        "--idempotency-key",
        "matrix-managed-handoff-0001",
        "--format",
        "json",
    ];
    let managed_handoff = run_main_agent(
        &main_checkout,
        &managed_handoff_args,
        &[
            ("AGENT_SESSION_CAPABILITY_FILE", &main_capability),
            ("AGENT_SESSION_CODEX_ACCOUNT_BROKER", account_broker),
            (
                "NILS_AGENT_SESSION_TEST_ACCOUNT_HANDOFF_APPLY_RESULT",
                "success",
            ),
        ],
    );
    assert_eq!(
        managed_handoff.code,
        0,
        "stdout={} stderr={}",
        managed_handoff.stdout_text(),
        managed_handoff.stderr_text()
    );
    assert_eq!(data(&managed_handoff)["state"], "bound");
    assert_eq!(data(&managed_handoff)["account"], "beta");
    assert_eq!(data(&managed_handoff)["auto_resume_rearmed"], true);
    assert_eq!(data(&managed_handoff)["provider_resume_preserved"], true);
    assert!(
        !managed_handoff
            .stdout_text()
            .contains("provider-thread-matrix"),
        "public handoff output must not expose provider resume metadata"
    );
    assert_eq!(
        data(&managed_handoff)["forbidden_side_effects"],
        json!({
            "logout_used": false,
            "prompt_resent": false,
            "blind_enter_sent": false,
            "duplicate_worker_created": false,
            "provider_conversation_replaced": false
        })
    );
    let applied_worker: serde_json::Value = serde_json::from_slice(
        &fs::read(&worker_record_path).expect("managed worker after handoff"),
    )
    .expect("managed worker json");
    assert_eq!(
        applied_worker["codex_account_binding"]["selected_account"],
        "beta"
    );
    assert_eq!(
        applied_worker["provider_resume"]["session_id"],
        "provider-thread-matrix"
    );
    let auto_resume: serde_json::Value = serde_json::from_slice(
        &fs::read(
            state_dir
                .join("sessions/worker-matrix")
                .join("auto-resume.json"),
        )
        .expect("durable auto resume"),
    )
    .expect("auto resume json");
    assert_eq!(auto_resume["blocked_turn_id"], "turn-managed-quota");
    assert_eq!(auto_resume["blocked_revision"], 1);
    let handoff_receipt =
        orchestration_registry(&state_dir)["receipts"]
            ["main-one:main-incarnation-one:matrix-managed-handoff-0001"]
            .clone();
    assert_eq!(handoff_receipt["operation"], "worker-account-handoff");
    assert!(
        !handoff_receipt
            .to_string()
            .contains("provider-thread-matrix"),
        "durable handoff receipts must not copy provider resume metadata"
    );
    let managed_replay = run_main_agent(
        &main_checkout,
        &managed_handoff_args,
        &[
            ("AGENT_SESSION_CAPABILITY_FILE", &main_capability),
            ("AGENT_SESSION_CODEX_ACCOUNT_BROKER", account_broker),
        ],
    );
    assert_eq!(managed_replay.code, 0);
    assert_eq!(managed_replay.stdout_json(), managed_handoff.stdout_json());

    let rearm_barrier = tmp.path().join("account-rearm-incarnation-barrier");
    fs::create_dir(&rearm_barrier).expect("account rearm incarnation barrier");
    let rearm_handoff_revision =
        orchestration_registry(&state_dir)["assignments"]["assignment-matrix"]["revision"]
            .as_u64()
            .expect("rearm handoff revision")
            .to_string();
    let worker_before_rearm_race =
        fs::read(&worker_record_path).expect("worker before rearm incarnation race");
    let rearm_race = Command::new(bin::resolve("main-agent"))
        .current_dir(&main_checkout)
        .args([
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "worker",
            "account-handoff",
            "assignment-matrix",
            "--account",
            "replacement-race",
            "--if-revision",
            &rearm_handoff_revision,
            "--authorize-account-change",
            "--timeout",
            "5s",
            "--idempotency-key",
            "matrix-managed-rearm-incarnation-0001",
            "--format",
            "json",
        ])
        .env("AGENT_SESSION_CAPABILITY_FILE", &main_capability)
        .env("AGENT_SESSION_CODEX_ACCOUNT_BROKER", account_broker)
        .env(
            "NILS_AGENT_SESSION_TEST_ACCOUNT_HANDOFF_APPLY_RESULT",
            "success",
        )
        .env(
            "NILS_AGENT_SESSION_TEST_ACCOUNT_HANDOFF_BARRIER_STAGE",
            "before_auto_resume_rearm",
        )
        .env(
            "NILS_AGENT_SESSION_TEST_ACCOUNT_HANDOFF_BARRIER_DIR",
            &rearm_barrier,
        )
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn account rearm incarnation race");
    let rearm_deadline = Instant::now() + Duration::from_secs(10);
    while !rearm_barrier.join("ready").is_file() {
        assert!(
            Instant::now() < rearm_deadline,
            "account handoff did not pause before auto-resume rearm"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let auto_resume_path = state_dir
        .join("sessions/worker-matrix")
        .join("auto-resume.json");
    let auto_resume_before_replacement =
        fs::read(&auto_resume_path).expect("auto resume before replacement");
    let mut replacement_worker: serde_json::Value =
        serde_json::from_slice(&fs::read(&worker_record_path).expect("worker at rearm barrier"))
            .expect("worker at rearm barrier json");
    replacement_worker["runtime"]["launch_id"] = json!("worker-matrix-replacement-incarnation");
    write_private_json(&worker_record_path, &replacement_worker);
    let replacement_worker_bytes = fs::read(&worker_record_path).expect("replacement worker bytes");
    fs::write(rearm_barrier.join("release"), b"continue").expect("release account rearm barrier");
    let rearm_race = rearm_race
        .wait_with_output()
        .expect("account rearm incarnation race output");
    assert_eq!(rearm_race.status.code(), Some(65));
    let rearm_error: serde_json::Value =
        serde_json::from_slice(&rearm_race.stdout).expect("rearm race json");
    assert_eq!(
        rearm_error["error"]["code"],
        "account-handoff-worker-incarnation-conflict"
    );
    assert_eq!(
        fs::read(&auto_resume_path).expect("auto resume after replacement"),
        auto_resume_before_replacement,
        "the old handoff must not rearm auto-resume for a replacement runtime"
    );
    assert_eq!(
        fs::read(&worker_record_path).expect("replacement worker after rejected rearm"),
        replacement_worker_bytes,
        "the rejected old handoff must not mutate the replacement session record"
    );
    fs::write(&worker_record_path, worker_before_rearm_race)
        .expect("restore worker after rearm incarnation race");
    let rearm_race_cancel = run_managed_account_handoff_cancel(
        &main_checkout,
        &state_dir,
        &main_capability,
        "matrix-managed-rearm-incarnation-cancel-0001",
    );
    assert_eq!(
        rearm_race_cancel.code,
        0,
        "rearm race reservation cleanup failed: {}",
        rearm_race_cancel.stdout_text()
    );

    let failed = run_managed_account_handoff(
        &main_checkout,
        &state_dir,
        &main_capability,
        "gamma",
        "matrix-managed-failed-0001",
        "failed",
    );
    assert_eq!(failed.code, 65);
    assert_eq!(
        failed.stdout_json()["error"]["code"],
        "account-handoff-apply-failed"
    );
    assert!(
        orchestration_registry(&state_dir)["assignments"]["assignment-matrix"]["account_handoff"]
            .is_object(),
        "a failed apply remains durably fenced"
    );
    let transition_revision =
        orchestration_registry(&state_dir)["assignments"]["assignment-matrix"]["revision"]
            .as_u64()
            .expect("reserved assignment revision")
            .to_string();
    let transition_during_handoff = run_main_agent(
        &main_checkout,
        &[
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "handoff",
            "assignment-matrix",
            "--to",
            "main-two@main-incarnation-two",
            "--if-revision",
            &transition_revision,
            "--idempotency-key",
            "matrix-account-reservation-handoff-0001",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_CAPABILITY_FILE", &main_capability)],
    );
    assert_eq!(transition_during_handoff.code, 65);
    assert_eq!(
        transition_during_handoff.stdout_json()["error"]["code"],
        "account-handoff-in-flight"
    );
    let mut worker_without_reserved_intent: serde_json::Value = serde_json::from_slice(
        &fs::read(&worker_record_path).expect("worker before missing-intent replay"),
    )
    .expect("worker before missing-intent replay json");
    worker_without_reserved_intent
        .as_object_mut()
        .expect("worker record object")
        .remove("codex_account_next");
    write_private_json(&worker_record_path, &worker_without_reserved_intent);
    let missing_intent_replay = run_managed_account_handoff(
        &main_checkout,
        &state_dir,
        &main_capability,
        "gamma",
        "matrix-managed-failed-0001",
        "success",
    );
    assert_eq!(missing_intent_replay.code, 65);
    assert_eq!(
        missing_intent_replay.stdout_json()["error"]["code"],
        "account-handoff-superseded"
    );
    let worker_after_missing_intent_replay: serde_json::Value = serde_json::from_slice(
        &fs::read(&worker_record_path).expect("worker after missing-intent replay"),
    )
    .expect("worker after missing-intent replay json");
    assert!(
        worker_after_missing_intent_replay["codex_account_next"].is_null(),
        "an exact replay must not resurrect the missing reservation-owned intent"
    );
    let failed_cancel_registry = orchestration_registry(&state_dir);
    let failed_cancel_assignment = &failed_cancel_registry["assignments"]["assignment-matrix"];
    let failed_cancel_reservation = &failed_cancel_assignment["account_handoff"];
    let failed_cancel_revision = failed_cancel_assignment["revision"]
        .as_u64()
        .expect("failed cancel revision")
        .to_string();
    let failed_cancel_reservation_id = failed_cancel_reservation["reservation_id"]
        .as_str()
        .expect("failed cancel reservation")
        .to_string();
    let failed_cancel_account = failed_cancel_reservation["account"]
        .as_str()
        .expect("failed cancel account")
        .to_string();
    let failed_cancel_intent = failed_cancel_reservation["account_intent_id"]
        .as_str()
        .expect("failed cancel intent")
        .to_string();
    let mismatched_cancel = run_managed_account_handoff_cancel_with_identity(
        &main_checkout,
        &state_dir,
        &main_capability,
        "matrix-managed-mismatched-cancel-0001",
        &failed_cancel_revision,
        &failed_cancel_reservation_id,
        "different-account",
        Some(&failed_cancel_intent),
    );
    assert_eq!(mismatched_cancel.code, 65);
    assert_eq!(
        mismatched_cancel.stdout_json()["error"]["code"],
        "account-handoff-cancel-reservation-conflict"
    );
    assert!(
        orchestration_registry(&state_dir)["assignments"]["assignment-matrix"]["account_handoff"]
            .is_object(),
        "mismatched cancellation authorization must preserve the exact reservation"
    );
    let failed_cancel = run_managed_account_handoff_cancel_with_identity(
        &main_checkout,
        &state_dir,
        &main_capability,
        "matrix-managed-failed-cancel-0001",
        &failed_cancel_revision,
        &failed_cancel_reservation_id,
        &failed_cancel_account,
        Some(&failed_cancel_intent),
    );
    assert_eq!(
        failed_cancel.code,
        0,
        "stdout={} stderr={}",
        failed_cancel.stdout_text(),
        failed_cancel.stderr_text()
    );
    assert_eq!(data(&failed_cancel)["state"], "cancelled");
    assert_eq!(data(&failed_cancel)["account_changed"], false);
    assert_eq!(data(&failed_cancel)["auto_resume_rearmed"], false);
    let failed_cancel_replay = run_managed_account_handoff_cancel_with_identity(
        &main_checkout,
        &state_dir,
        &main_capability,
        "matrix-managed-failed-cancel-0001",
        &failed_cancel_revision,
        &failed_cancel_reservation_id,
        &failed_cancel_account,
        Some(&failed_cancel_intent),
    );
    assert_eq!(
        failed_cancel_replay.stdout_json(),
        failed_cancel.stdout_json()
    );

    let superseded = run_managed_account_handoff(
        &main_checkout,
        &state_dir,
        &main_capability,
        "gamma",
        "matrix-managed-superseded-0001",
        "superseded",
    );
    assert_eq!(superseded.code, 65);
    assert_eq!(
        superseded.stdout_json()["error"]["code"],
        "account-handoff-superseded"
    );
    assert!(
        orchestration_registry(&state_dir)["assignments"]["assignment-matrix"]["account_handoff"]
            .is_object(),
        "a superseded apply remains durably fenced"
    );
    let superseded_cancel = run_managed_account_handoff_cancel(
        &main_checkout,
        &state_dir,
        &main_capability,
        "matrix-managed-superseded-cancel-0001",
    );
    assert_eq!(superseded_cancel.code, 0);
    assert_eq!(data(&superseded_cancel)["state"], "cancelled");

    let timeout = run_managed_account_handoff(
        &main_checkout,
        &state_dir,
        &main_capability,
        "gamma",
        "matrix-managed-timeout-0001",
        "timeout",
    );
    assert_eq!(timeout.code, 1);
    assert_eq!(
        timeout.stdout_json()["error"]["code"],
        "account-handoff-binding-timeout"
    );
    assert!(
        orchestration_registry(&state_dir)["assignments"]["assignment-matrix"]["account_handoff"]
            .is_object(),
        "a timed-out apply remains durably fenced"
    );
    let timeout_cancel = run_managed_account_handoff_cancel(
        &main_checkout,
        &state_dir,
        &main_capability,
        "matrix-managed-timeout-cancel-0001",
    );
    assert_eq!(timeout_cancel.code, 0);
    assert_eq!(data(&timeout_cancel)["state"], "cancelled");
    assert_eq!(
        orchestration_registry(&state_dir)["assignments"]["assignment-matrix"]["account_handoff"],
        serde_json::Value::Null
    );

    let operation_barrier = tmp.path().join("account-operation-barrier");
    fs::create_dir(&operation_barrier).expect("operation barrier");
    let operation_handoff_revision =
        orchestration_registry(&state_dir)["assignments"]["assignment-matrix"]["revision"]
            .as_u64()
            .expect("operation handoff revision")
            .to_string();
    let account_with_operation_race = Command::new(bin::resolve("main-agent"))
        .current_dir(&main_checkout)
        .args([
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "worker",
            "account-handoff",
            "assignment-matrix",
            "--account",
            "gamma",
            "--if-revision",
            &operation_handoff_revision,
            "--authorize-account-change",
            "--timeout",
            "5s",
            "--idempotency-key",
            "matrix-managed-operation-race-0001",
            "--format",
            "json",
        ])
        .env("AGENT_SESSION_CAPABILITY_FILE", &main_capability)
        .env("AGENT_SESSION_CODEX_ACCOUNT_BROKER", account_broker)
        .env(
            "NILS_AGENT_SESSION_TEST_ACCOUNT_HANDOFF_APPLY_RESULT",
            "success",
        )
        .env(
            "NILS_AGENT_SESSION_TEST_ACCOUNT_HANDOFF_BARRIER_STAGE",
            "after_reservation",
        )
        .env(
            "NILS_AGENT_SESSION_TEST_ACCOUNT_HANDOFF_BARRIER_DIR",
            &operation_barrier,
        )
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn account handoff operation race");
    let operation_deadline = Instant::now() + Duration::from_secs(10);
    while !operation_barrier.join("ready").is_file() {
        assert!(
            Instant::now() < operation_deadline,
            "account handoff did not persist its reservation"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let operation_targets = tmp.path().join("account-operation-targets.json");
    write_private_json(
        &operation_targets,
        &json!({
            "schema_version": "agent-session.operation-targets.v1",
            "targets": [{
                "kind": "path-exact",
                "repository": "example/repository",
                "value": "docs/matrix/during-handoff.md"
            }]
        }),
    );
    let operation_token = tmp.path().join("account-operation-token");
    fs::write(&operation_token, "account-operation-token").expect("operation token");
    fs::set_permissions(&operation_token, fs::Permissions::from_mode(0o600))
        .expect("operation token mode");
    let worker_capability = capability(&state_dir, "worker-matrix");
    let operation_runtime =
        spawn_scoped_test_process_group().expect("operation race runtime identity");
    let runtime_pid = operation_runtime.pid() as libc::pid_t;
    let operation_runtime_identity = json!({
        "launch_id": "worker-matrix-incarnation",
        "session_id": "$88",
        "pane_id": "%88",
        "pane_pid": runtime_pid,
        "process_group_id": runtime_pid,
        "process_session_id": runtime_pid
    });
    let operation_runtime_bytes =
        serde_json::to_vec(&operation_runtime_identity).expect("operation runtime identity");
    let operation_runtime_digest = Sha256::digest(&operation_runtime_bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let mut operation_worker: serde_json::Value =
        serde_json::from_slice(&fs::read(&worker_record_path).expect("operation worker"))
            .expect("operation worker json");
    operation_worker["delete_tmux_identity"] = operation_runtime_identity.clone();
    write_private_json(&worker_record_path, &operation_worker);
    rewrite_registry(&state_dir, |registry| {
        registry["brokers"]["worker-matrix"]["runtime_identity"] =
            operation_runtime_identity.clone();
        registry["brokers"]["worker-matrix"]["runtime_identity_digest"] =
            json!(operation_runtime_digest);
    });
    seed_activity_state(
        &state_dir,
        "worker-matrix",
        "worker-matrix-incarnation",
        "working",
        json!({
            "provider_turn_id": "turn-operation-race",
            "started_at": "2020-01-01T00:00:20Z"
        }),
        serde_json::Value::Null,
    );
    let operation_attempt = run(
        tmp.path(),
        &[
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "work-context",
            "admit",
            "--session",
            "worker-matrix",
            "--claim",
            "worker-matrix-claim",
            "--if-revision",
            "1",
            "--targets-file",
            operation_targets.to_str().expect("operation targets"),
            "--operation",
            "edit",
            "--execution-token-file",
            operation_token.to_str().expect("operation token"),
            "--capability-file",
            &worker_capability,
            "--idempotency-key",
            "account-operation-race-admit-0001",
            "--format",
            "json",
        ],
    );
    assert_eq!(
        operation_attempt.code,
        0,
        "handoff must not hold coordination while waiting at a session mutation boundary: {}",
        operation_attempt.stdout_text()
    );
    let operation_registry = load_coordination_registry(&state_dir);
    let admitted_operation = operation_registry["operations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|operation| operation["session_id"] == "worker-matrix")
        .expect("admitted reciprocal operation");
    let admitted_lease = admitted_operation["lease_id"].as_str().unwrap().to_string();
    let admitted_revision = admitted_operation["revision"].as_u64().unwrap().to_string();
    fs::write(operation_barrier.join("release"), b"continue").expect("release operation barrier");
    let account_with_operation_race = account_with_operation_race
        .wait_with_output()
        .expect("account handoff operation race output");
    assert_eq!(
        account_with_operation_race.status.code(),
        Some(1),
        "reciprocal overlap must converge to a typed authority fence, stdout={} stderr={}",
        String::from_utf8_lossy(&account_with_operation_race.stdout),
        String::from_utf8_lossy(&account_with_operation_race.stderr)
    );
    let overlap_error: serde_json::Value =
        serde_json::from_slice(&account_with_operation_race.stdout).unwrap();
    assert_eq!(
        overlap_error["error"]["code"],
        "account-handoff-worker-authority-changed"
    );
    let operation_complete = run(
        tmp.path(),
        &[
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "work-context",
            "complete",
            "--session",
            "worker-matrix",
            "--lease",
            &admitted_lease,
            "--if-revision",
            &admitted_revision,
            "--execution-token-file",
            operation_token.to_str().expect("operation token"),
            "--outcome",
            "pass",
            "--capability-file",
            &worker_capability,
            "--idempotency-key",
            "account-operation-race-complete-0001",
            "--format",
            "json",
        ],
    );
    assert_eq!(
        operation_complete.code,
        0,
        "reciprocal operation completion failed: {}",
        operation_complete.stdout_text()
    );
    let overlap_retry = run_main_agent(
        &main_checkout,
        &[
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "worker",
            "account-handoff",
            "assignment-matrix",
            "--account",
            "gamma",
            "--if-revision",
            &operation_handoff_revision,
            "--authorize-account-change",
            "--timeout",
            "5s",
            "--idempotency-key",
            "matrix-managed-operation-race-0001",
            "--format",
            "json",
        ],
        &[
            ("AGENT_SESSION_CAPABILITY_FILE", &main_capability),
            ("AGENT_SESSION_CODEX_ACCOUNT_BROKER", account_broker),
            (
                "NILS_AGENT_SESSION_TEST_ACCOUNT_HANDOFF_APPLY_RESULT",
                "success",
            ),
        ],
    );
    assert_eq!(
        overlap_retry.code,
        0,
        "exact retry must converge without a stranded reservation: {}",
        overlap_retry.stdout_text()
    );

    let snapshot_barrier = tmp.path().join("account-snapshot-cas-barrier");
    fs::create_dir(&snapshot_barrier).expect("account snapshot barrier");
    let snapshot_handoff_revision =
        orchestration_registry(&state_dir)["assignments"]["assignment-matrix"]["revision"]
            .as_u64()
            .expect("snapshot handoff revision")
            .to_string();
    let stale_snapshot_handoff = Command::new(bin::resolve("main-agent"))
        .current_dir(&main_checkout)
        .args([
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "worker",
            "account-handoff",
            "assignment-matrix",
            "--account",
            "delta",
            "--if-revision",
            &snapshot_handoff_revision,
            "--authorize-account-change",
            "--timeout",
            "5s",
            "--idempotency-key",
            "matrix-managed-snapshot-cas-0001",
            "--format",
            "json",
        ])
        .env("AGENT_SESSION_CAPABILITY_FILE", &main_capability)
        .env("AGENT_SESSION_CODEX_ACCOUNT_BROKER", account_broker)
        .env(
            "NILS_AGENT_SESSION_TEST_ACCOUNT_HANDOFF_APPLY_RESULT",
            "success",
        )
        .env(
            "NILS_AGENT_SESSION_TEST_ACCOUNT_HANDOFF_BARRIER_STAGE",
            "after_reservation",
        )
        .env(
            "NILS_AGENT_SESSION_TEST_ACCOUNT_HANDOFF_BARRIER_DIR",
            &snapshot_barrier,
        )
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn stale snapshot handoff");
    let snapshot_deadline = Instant::now() + Duration::from_secs(10);
    while !snapshot_barrier.join("ready").is_file() {
        assert!(
            Instant::now() < snapshot_deadline,
            "account handoff did not pause after its durable snapshot"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let mut worker_after_snapshot: serde_json::Value = serde_json::from_slice(
        &fs::read(&worker_record_path).expect("worker after account snapshot"),
    )
    .expect("worker after account snapshot json");
    worker_after_snapshot["codex_account_next"] = json!({
        "schema_version": "agent-session.codex-account-next.v1",
        "account": "epsilon",
        "revision": 1,
        "intent_id": "normal-account-control-intent-0001",
        "state": "queued",
        "updated_at": "2030-01-01T00:00:10Z"
    });
    write_private_json(&worker_record_path, &worker_after_snapshot);
    fs::write(snapshot_barrier.join("release"), b"continue")
        .expect("release account snapshot barrier");
    let stale_snapshot_handoff = stale_snapshot_handoff
        .wait_with_output()
        .expect("stale snapshot handoff output");
    assert_eq!(stale_snapshot_handoff.status.code(), Some(65));
    let stale_snapshot_envelope: serde_json::Value =
        serde_json::from_slice(&stale_snapshot_handoff.stdout)
            .expect("stale snapshot handoff json");
    assert_eq!(
        stale_snapshot_envelope["error"]["code"],
        "account-handoff-superseded"
    );
    let worker_after_stale_handoff: serde_json::Value =
        serde_json::from_slice(&fs::read(&worker_record_path).expect("worker after stale handoff"))
            .expect("worker after stale handoff json");
    assert_eq!(
        worker_after_stale_handoff["codex_account_next"]["account"], "epsilon",
        "the stale handoff must preserve the newer account intent"
    );
    assert_eq!(
        worker_after_stale_handoff["codex_account_next"]["intent_id"],
        "normal-account-control-intent-0001"
    );
    let stale_snapshot_retry = run_main_agent(
        &main_checkout,
        &[
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "worker",
            "account-handoff",
            "assignment-matrix",
            "--account",
            "delta",
            "--if-revision",
            &snapshot_handoff_revision,
            "--authorize-account-change",
            "--timeout",
            "5s",
            "--idempotency-key",
            "matrix-managed-snapshot-cas-0001",
            "--format",
            "json",
        ],
        &[
            ("AGENT_SESSION_CAPABILITY_FILE", &main_capability),
            ("AGENT_SESSION_CODEX_ACCOUNT_BROKER", account_broker),
            (
                "NILS_AGENT_SESSION_TEST_ACCOUNT_HANDOFF_APPLY_RESULT",
                "success",
            ),
        ],
    );
    assert_eq!(stale_snapshot_retry.code, 65);
    assert_eq!(
        stale_snapshot_retry.stdout_json()["error"]["code"],
        "account-handoff-superseded"
    );
    let worker_after_stale_retry: serde_json::Value = serde_json::from_slice(
        &fs::read(&worker_record_path).expect("worker after stale handoff retry"),
    )
    .expect("worker after stale handoff retry json");
    assert_eq!(
        worker_after_stale_retry["codex_account_next"]["intent_id"],
        "normal-account-control-intent-0001",
        "an exact reservation retry must not adopt and replace its superseding intent"
    );
    let snapshot_cancel = run_managed_account_handoff_cancel(
        &main_checkout,
        &state_dir,
        &main_capability,
        "matrix-managed-snapshot-cas-cancel-0001",
    );
    assert_eq!(snapshot_cancel.code, 0);
    assert_eq!(
        data(&snapshot_cancel)["newer_account_intent_preserved"],
        true,
        "typed recovery must release only the stale reservation"
    );
    let worker_after_snapshot_cancel: serde_json::Value = serde_json::from_slice(
        &fs::read(&worker_record_path).expect("worker after superseded reservation recovery"),
    )
    .expect("worker after superseded reservation recovery json");
    assert_eq!(
        worker_after_snapshot_cancel["codex_account_next"]["intent_id"],
        "normal-account-control-intent-0001",
        "reservation recovery must not clear the superseding account intent"
    );

    let unrelated_run_before = orchestration_registry(&state_dir)["runs"]["run-one"].clone();
    rewrite_orchestration_registry(&state_dir, |registry| {
        let assignment = &registry["assignments"]["assignment-matrix"];
        let assignment_revision = assignment["revision"].clone();
        registry["assignments"]["assignment-matrix"]["account_handoff"] = json!({
            "schema_version": "main-agent.account-handoff-reservation.v1",
            "request_digest": "abababababababababababababababababababababababababababababababab",
            "run_id": "run-one",
            "controller": assignment["primary_manager"].clone(),
            "worker": assignment["worker"].clone(),
            "reserved_revision": assignment_revision,
            "account": "delta",
            "created_at": "2030-01-01T00:00:00Z",
            "updated_at": "2030-01-01T00:00:00Z"
        });
    });
    let legacy_cancel = run_managed_account_handoff_cancel(
        &main_checkout,
        &state_dir,
        &main_capability,
        "matrix-managed-v1-reservation-cancel-0001",
    );
    assert_eq!(
        legacy_cancel.code,
        0,
        "v1 serialized reservation must load and recover: {}",
        legacy_cancel.stdout_text()
    );
    assert_eq!(data(&legacy_cancel)["legacy_reservation_recovered"], true);
    assert_eq!(data(&legacy_cancel)["newer_account_intent_preserved"], true);
    let legacy_registry = orchestration_registry(&state_dir);
    assert!(
        legacy_registry["assignments"]["assignment-matrix"]["account_handoff"].is_null(),
        "v1 recovery clears only the assignment-local unbound reservation"
    );
    assert_eq!(
        legacy_registry["runs"]["run-one"], unrelated_run_before,
        "v1 recovery must preserve unrelated run state"
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(
            &fs::read(&worker_record_path).expect("worker after v1 cancellation")
        )
        .unwrap()["codex_account_next"]["intent_id"],
        "normal-account-control-intent-0001",
        "v1 recovery must never guess ownership of a newer intent"
    );

    let failed_before_cancel_race = run_managed_account_handoff(
        &main_checkout,
        &state_dir,
        &main_capability,
        "delta",
        "matrix-managed-cancel-race-handoff-0001",
        "failed",
    );
    assert_eq!(failed_before_cancel_race.code, 65);
    assert_eq!(
        failed_before_cancel_race.stdout_json()["error"]["code"],
        "account-handoff-apply-failed"
    );
    let cancel_barrier = tmp.path().join("account-cancel-race-barrier");
    fs::create_dir(&cancel_barrier).expect("account cancel barrier");
    let cancel_registry = orchestration_registry(&state_dir);
    let cancel_assignment = &cancel_registry["assignments"]["assignment-matrix"];
    let cancel_reservation = &cancel_assignment["account_handoff"];
    let cancel_revision = cancel_assignment["revision"]
        .as_u64()
        .expect("cancel assignment revision")
        .to_string();
    let cancel_reservation_id = cancel_reservation["reservation_id"]
        .as_str()
        .or_else(|| cancel_reservation["request_digest"].as_str())
        .expect("cancel reservation id")
        .to_string();
    let cancel_account = cancel_reservation["account"]
        .as_str()
        .expect("cancel account")
        .to_string();
    let cancel_intent = cancel_reservation["account_intent_id"]
        .as_str()
        .expect("cancel intent")
        .to_string();
    let cancel_race_args = vec![
        "--state-dir".to_string(),
        state_dir.to_str().expect("state dir").to_string(),
        "worker".to_string(),
        "account-handoff-cancel".to_string(),
        "assignment-matrix".to_string(),
        "--reservation-id".to_string(),
        cancel_reservation_id,
        "--account".to_string(),
        cancel_account,
        "--intent-id".to_string(),
        cancel_intent,
        "--if-revision".to_string(),
        cancel_revision,
        "--authorize-account-change".to_string(),
        "--idempotency-key".to_string(),
        "matrix-managed-cancel-race-0001".to_string(),
        "--format".to_string(),
        "json".to_string(),
    ];
    let cancel_race = Command::new(bin::resolve("main-agent"))
        .current_dir(&main_checkout)
        .args(cancel_race_args)
        .env("AGENT_SESSION_CAPABILITY_FILE", &main_capability)
        .env("AGENT_SESSION_CODEX_ACCOUNT_BROKER", account_broker)
        .env(
            "NILS_AGENT_SESSION_TEST_ACCOUNT_HANDOFF_BARRIER_STAGE",
            "before_cancel",
        )
        .env(
            "NILS_AGENT_SESSION_TEST_ACCOUNT_HANDOFF_BARRIER_DIR",
            &cancel_barrier,
        )
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn account cancel race");
    let cancel_deadline = Instant::now() + Duration::from_secs(10);
    while !cancel_barrier.join("ready").is_file() {
        assert!(
            Instant::now() < cancel_deadline,
            "account cancellation did not reach its exact-intent boundary"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let mut worker_during_cancel: serde_json::Value = serde_json::from_slice(
        &fs::read(&worker_record_path).expect("worker during account cancel"),
    )
    .expect("worker during account cancel json");
    let original_cancel_revision = worker_during_cancel["codex_account_next"]["revision"]
        .as_u64()
        .expect("pending account revision");
    worker_during_cancel["codex_account_next"]["account"] = json!("delta");
    worker_during_cancel["codex_account_next"]["revision"] = json!(original_cancel_revision);
    worker_during_cancel["codex_account_next"]["intent_id"] =
        json!("replacement-same-account-intent-0001");
    worker_during_cancel["codex_account_next"]["state"] = json!("queued");
    worker_during_cancel["codex_account_next"]["applying_runtime_id"] = serde_json::Value::Null;
    worker_during_cancel["codex_account_next"]["failure_reason"] = serde_json::Value::Null;
    write_private_json(&worker_record_path, &worker_during_cancel);
    fs::write(cancel_barrier.join("release"), b"continue").expect("release account cancel barrier");
    let cancel_race = cancel_race
        .wait_with_output()
        .expect("account cancel race output");
    assert!(!cancel_race.status.success());
    let cancel_race_envelope: serde_json::Value =
        serde_json::from_slice(&cancel_race.stdout).expect("account cancel race json");
    assert_eq!(
        cancel_race_envelope["error"]["code"],
        "codex-account-next-superseded"
    );
    let worker_after_cancel_race: serde_json::Value = serde_json::from_slice(
        &fs::read(&worker_record_path).expect("worker after account cancel race"),
    )
    .expect("worker after account cancel race json");
    assert_eq!(
        worker_after_cancel_race["codex_account_next"]["account"], "delta",
        "a same-account replacement must survive a stale cancellation attempt"
    );
    assert_eq!(
        worker_after_cancel_race["codex_account_next"]["revision"], original_cancel_revision,
        "the ABA fixture deliberately reuses the old account/revision tuple"
    );
    assert_eq!(
        worker_after_cancel_race["codex_account_next"]["intent_id"],
        "replacement-same-account-intent-0001",
        "the durable intent identity must fence stale cancellation"
    );
    assert!(
        orchestration_registry(&state_dir)["assignments"]["assignment-matrix"]["account_handoff"]
            .is_object(),
        "a stale cancellation must preserve the exact reservation"
    );
    let converged_cancel = run_managed_account_handoff_cancel(
        &main_checkout,
        &state_dir,
        &main_capability,
        "matrix-managed-cancel-race-0001",
    );
    assert_eq!(
        converged_cancel.code,
        0,
        "stdout={} stderr={}",
        converged_cancel.stdout_text(),
        converged_cancel.stderr_text()
    );
    assert_eq!(
        data(&converged_cancel)["newer_account_intent_preserved"],
        true,
        "a fresh recovery attempt must still distinguish the replacement from the reservation"
    );
    let worker_after_converged_cancel: serde_json::Value = serde_json::from_slice(
        &fs::read(&worker_record_path).expect("worker after converged cancellation"),
    )
    .expect("worker after converged cancellation json");
    assert_eq!(
        worker_after_converged_cancel["codex_account_next"]["intent_id"],
        "replacement-same-account-intent-0001"
    );
    let mut worker_after_converged_cancel = worker_after_converged_cancel;
    worker_after_converged_cancel
        .as_object_mut()
        .expect("worker record object")
        .remove("codex_account_next");
    write_private_json(&worker_record_path, &worker_after_converged_cancel);

    let guidance_body = tmp.path().join("matrix-guidance.md");
    fs::write(
        &guidance_body,
        "Continue with the focused validation after the active turn.",
    )
    .expect("guidance");
    let queued = run_main_agent(
        &main_checkout,
        &[
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "worker",
            "message",
            "assignment-matrix",
            "--body-file",
            guidance_body.to_str().expect("guidance path"),
            "--idempotency-key",
            "matrix-stale-guidance-0001",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_CAPABILITY_FILE", &main_capability)],
    );
    assert_eq!(queued.code, 0, "stderr={}", queued.stderr_text());
    let current_guidance_id = data(&queued)["message_id"]
        .as_str()
        .expect("current guidance id")
        .to_string();
    rewrite_registry(&state_dir, |registry| {
        let messages = registry["messages"].as_array_mut().expect("messages");
        let original = messages
            .iter()
            .find(|message| message["message_id"] == current_guidance_id)
            .expect("current guidance")
            .clone();
        for index in 0..3 {
            let mut stale = original.clone();
            stale["message_id"] = json!(format!("orphan-stale-guidance-{index}"));
            stale["recipient_incarnation"] = json!(format!("orphan-worker-incarnation-{index}"));
            messages.push(stale);
        }
        let mut unrelated = original;
        unrelated["message_id"] = json!("orphan-stale-guidance-unrelated");
        unrelated["recipient_incarnation"] = json!("orphan-worker-incarnation-unrelated");
        unrelated["sender_session_id"] = json!("unrelated-controller");
        unrelated["sender_incarnation"] = json!("unrelated-controller-incarnation");
        messages.push(unrelated);
    });
    assert!(
        orchestration_registry(&state_dir)["assignments"]["assignment-matrix"]["previous_worker"]
            .is_null(),
        "the contradictory fixture intentionally has no retained previous worker"
    );
    let orphaned_guidance = supervise();
    assert_eq!(
        data(&orphaned_guidance)["classification"],
        "orphan_guidance_quarantine_required"
    );
    assert!(
        data(&orphaned_guidance)["next_action"]
            .as_str()
            .unwrap_or_default()
            .contains("worker guidance-quarantine")
    );
    let quarantine_revision =
        orchestration_registry(&state_dir)["assignments"]["assignment-matrix"]["revision"]
            .as_u64()
            .expect("guidance quarantine revision")
            .to_string();
    let quarantine_args = [
        "--state-dir",
        state_dir.to_str().expect("state dir"),
        "worker",
        "guidance-quarantine",
        "assignment-matrix",
        "--if-revision",
        &quarantine_revision,
        "--idempotency-key",
        "matrix-guidance-quarantine-0001",
        "--format",
        "json",
    ];
    let quarantined = run_main_agent(
        &main_checkout,
        &quarantine_args,
        &[("AGENT_SESSION_CAPABILITY_FILE", &main_capability)],
    );
    assert_eq!(
        quarantined.code,
        0,
        "stdout={} stderr={}",
        quarantined.stdout_text(),
        quarantined.stderr_text()
    );
    assert_eq!(data(&quarantined)["state"], "quarantined");
    assert_eq!(data(&quarantined)["quarantined_count"], 3);
    let quarantine_replay = run_main_agent(
        &main_checkout,
        &quarantine_args,
        &[("AGENT_SESSION_CAPABILITY_FILE", &main_capability)],
    );
    assert_eq!(quarantine_replay.stdout_json(), quarantined.stdout_json());
    let messages_after_quarantine = load_coordination_registry(&state_dir)["messages"]
        .as_array()
        .expect("messages")
        .clone();
    for index in 0..3 {
        let message_id = format!("orphan-stale-guidance-{index}");
        let message = messages_after_quarantine
            .iter()
            .find(|message| message["message_id"] == message_id)
            .expect("quarantined guidance");
        assert_eq!(message["state"], "quarantined");
    }
    for message_id in [
        current_guidance_id.as_str(),
        "orphan-stale-guidance-unrelated",
    ] {
        let message = messages_after_quarantine
            .iter()
            .find(|message| message["message_id"] == message_id)
            .expect("preserved guidance");
        assert_eq!(
            message["state"], "unread",
            "current-incarnation and unrelated guidance must be preserved"
        );
    }
    let converged_guidance = supervise();
    assert_ne!(
        data(&converged_guidance)["classification"],
        "orphan_guidance_quarantine_required"
    );
    assert_ne!(
        data(&converged_guidance)["classification"],
        "guidance_continuity_required"
    );
    assert!(
        !data(&converged_guidance)["next_action"]
            .as_str()
            .unwrap_or_default()
            .contains("guidance-")
    );
    seed_activity_state(
        &state_dir,
        "worker-matrix",
        "worker-matrix-incarnation",
        "working",
        json!({
            "provider_turn_id": "turn-matrix",
            "started_at": "2020-01-01T00:00:01Z"
        }),
        serde_json::Value::Null,
    );
    let stale = supervise();
    assert_eq!(data(&stale)["classification"], "stale_provider_activity");
    assert_eq!(
        data(&stale)["last_proven_safe_state"]["guidance"]["state"],
        "queued_unread"
    );
    assert_eq!(
        data(&stale)["last_proven_safe_state"]["progress"]["material_worktree_changes"],
        0,
        "provider activity alone is not material worktree progress"
    );
    assert!(
        data(&stale)["next_action"]
            .as_str()
            .unwrap_or_default()
            .contains("turn boundary")
    );
    rewrite_registry(&state_dir, |registry| {
        for message in registry["messages"]
            .as_array_mut()
            .expect("messages")
            .iter_mut()
        {
            if message["recipient_session_id"] == "worker-matrix" {
                message["state"] = json!("read");
            }
        }
    });
    seed_activity_state(
        &state_dir,
        "worker-matrix",
        "worker-matrix-incarnation",
        "working",
        json!({
            "provider_turn_id": "turn-matrix-current",
            "started_at": jiff::Timestamp::now().to_string()
        }),
        serde_json::Value::Null,
    );
    let consumed = supervise();
    assert_eq!(data(&consumed)["classification"], "healthy_progress");
    assert_eq!(
        data(&consumed)["last_proven_safe_state"]["guidance"]["state"],
        "consumed"
    );

    rewrite_registry(&state_dir, |registry| {
        registry["operations"]
            .as_array_mut()
            .expect("operations")
            .push(json!({
                "schema_version": "agent-session.operation-lease.v1",
                "lease_id": "worker-matrix-lease",
                "session_id": "worker-matrix",
                "session_incarnation": "worker-matrix-incarnation",
                "claim_id": "worker-matrix-claim",
                "claim_revision": 1,
                "operation": "test mutation",
                "targets": [],
                "provider_targets": [],
                "state": "reconcile_pending",
                "revision": 2,
                "started_at": "2030-01-01T00:00:00Z",
                "expires_at": "9999-12-31T23:59:59Z",
                "expires_at_epoch": i64::MAX,
                "terminal_at_epoch": null,
                "execution_token_digest": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "activity_revision": 1,
                "activity_identity_digest": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                "runtime_identity_digest": "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
                "descendant": null,
                "reconcile_observed_at_epoch": null,
                "outcome": null
            }));
    });
    let uncertain = supervise();
    assert_eq!(data(&uncertain)["classification"], "uncertain_mutation");
    assert_eq!(data(&uncertain)["automatic_retry_safe"], false);

    rewrite_registry(&state_dir, |registry| {
        registry["operations"] = json!([]);
    });
    fs::write(
        state_dir.join("sessions/worker-matrix/session.json"),
        "{not-json",
    )
    .expect("corrupt worker record");
    let unavailable = supervise();
    assert_eq!(data(&unavailable)["classification"], "evidence_unavailable");
    assert_eq!(data(&unavailable)["automatic_retry_safe"], false);
    assert_eq!(
        data(&unavailable)["last_proven_safe_state"]["evidence"]["session"]["state"],
        "unavailable"
    );

    seed_session_at(
        &state_dir,
        "worker-matrix",
        "worker-matrix-incarnation",
        &worker_checkout,
        Some("enforce"),
    );
    seed_activity_state(
        &state_dir,
        "worker-matrix",
        "worker-matrix-incarnation",
        "starting",
        serde_json::Value::Null,
        serde_json::Value::Null,
    );
    rewrite_registry(&state_dir, |registry| {
        registry["brokers"]["worker-matrix"]["incarnation"] =
            json!("worker-matrix-other-incarnation");
    });
    let broker_mismatch = supervise();
    assert_eq!(
        data(&broker_mismatch)["classification"],
        "evidence_unavailable"
    );
    assert_eq!(
        data(&broker_mismatch)["last_proven_safe_state"]["evidence"]["coordination"]["state"],
        "identity_mismatch"
    );
    rewrite_registry(&state_dir, |registry| {
        registry["brokers"]["worker-matrix"]["incarnation"] = json!("worker-matrix-incarnation");
    });

    fs::write(
        state_dir.join("sessions/worker-matrix/activity.json"),
        "{not-json",
    )
    .expect("corrupt activity");
    let activity_unavailable = supervise();
    assert_eq!(
        data(&activity_unavailable)["classification"],
        "evidence_unavailable"
    );
    assert_eq!(
        data(&activity_unavailable)["last_proven_safe_state"]["evidence"]["activity"]["state"],
        "unavailable"
    );
    seed_activity_state(
        &state_dir,
        "worker-matrix",
        "worker-matrix-incarnation",
        "starting",
        serde_json::Value::Null,
        serde_json::Value::Null,
    );
    let registry: serde_json::Value = serde_json::from_slice(
        &fs::read(state_dir.join("orchestration/registry.json")).expect("registry"),
    )
    .expect("registry json");
    let packet_digest = registry["assignments"]["assignment-matrix"]["private_packet_digest"]
        .as_str()
        .expect("packet digest")
        .trim_start_matches("sha256:");
    let packet_path = state_dir.join("orchestration/packets").join(packet_digest);
    let packet_bytes = fs::read(&packet_path).expect("packet bytes");
    fs::write(&packet_path, "{not-json").expect("corrupt packet");
    let packet_unavailable = supervise();
    assert_eq!(
        data(&packet_unavailable)["classification"],
        "evidence_unavailable"
    );
    assert_eq!(
        data(&packet_unavailable)["last_proven_safe_state"]["evidence"]["packet"]["state"],
        "unavailable"
    );
    fs::write(&packet_path, packet_bytes).expect("restore packet");

    fs::remove_dir_all(state_dir.join("sessions/worker-matrix")).expect("remove worker session");
    let unreachable = supervise();
    assert_eq!(data(&unreachable)["classification"], "worker_unreachable");
    assert_eq!(data(&unreachable)["automatic_retry_safe"], false);

    rewrite_orchestration_registry(&state_dir, |registry| {
        let assignment = &mut registry["assignments"]["assignment-matrix"];
        assignment["state"] = json!("cancelled");
        assignment["revision"] = json!(3);
        assignment["worker"] = serde_json::Value::Null;
        assignment["checkpoint"] = serde_json::Value::Null;
    });
    let safe = supervise();
    assert_eq!(data(&safe)["classification"], "safe_reassignment");
    assert_eq!(data(&safe)["automatic_retry_safe"], true);
    let send_calls = tmux_calls(&tmux_log)
        .into_iter()
        .filter(|call| call.first().is_some_and(|arg| arg == "send-keys"))
        .count();
    assert_eq!(send_calls, 0, "supervision must never send terminal input");
}

#[test]
fn main_agent_handoff_requires_operation_quiescence_and_adopt_requires_an_orphan() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let checkout = tmp.path().join("checkout");
    fs::create_dir(&state_dir).expect("state");
    init_checkout(&checkout, "https://example.invalid/example/repository.git");
    seed_brokers_at(
        &state_dir,
        &[
            (
                "main-one",
                "main-incarnation-one",
                "main-private-capability-material-0000000001",
                checkout.as_path(),
                Some("enforce"),
            ),
            (
                "main-two",
                "main-incarnation-two",
                "main-private-capability-material-0000000002",
                checkout.as_path(),
                Some("enforce"),
            ),
            (
                "worker-handoff",
                "worker-handoff-incarnation",
                "worker-private-capability-material-handoff",
                checkout.as_path(),
                Some("enforce"),
            ),
            (
                "worker-account-race",
                "worker-account-race-incarnation",
                "worker-private-capability-account-race",
                checkout.as_path(),
                Some("enforce"),
            ),
        ],
    );
    let main_one_capability =
        init_main_run(tmp.path(), &state_dir, &checkout, "main-one", "run-one");
    seed_active_claim(
        &state_dir,
        "main-two",
        "main-incarnation-two",
        "main-two-claim",
    );
    let main_two_capability = capability(&state_dir, "main-two");
    let mut orchestration_registry: serde_json::Value = serde_json::from_slice(
        &fs::read(state_dir.join("orchestration/registry.json")).expect("registry"),
    )
    .expect("registry json");
    let main_one_controller = orchestration_registry["runs"]["run-one"]["controller"].clone();
    let mut main_two_controller = main_one_controller.clone();
    main_two_controller["session_id"] = json!("main-two");
    main_two_controller["session_incarnation"] = json!("main-incarnation-two");
    let objective_digest =
        orchestration_registry["runs"]["run-one"]["objective_packet_digest"].clone();
    orchestration_registry["runs"]["run-two"] = json!({
        "schema_version": "agent-session.orchestration-run.v1",
        "run_id": "run-two",
        "revision": 1,
        "state": "active",
        "tier": "L0",
        "objective_summary": "Adopt orphaned assignments",
        "objective_packet_digest": objective_digest,
        "controller": main_two_controller,
        "durable_refs": [],
        "checkpoint": null,
        "created_at": "2030-01-01T00:00:00Z",
        "updated_at": "2030-01-01T00:00:00Z"
    });
    write_private_json(
        &state_dir.join("orchestration/registry.json"),
        &orchestration_registry,
    );
    let private_packet = json!({
        "schema_version": "main-agent.assignment-input.v1",
        "assignment_id": "fixture",
        "task_summary": "Exercise relationship lifecycle",
        "task": {},
        "launch": {
            "agent": "codex",
            "cwd": checkout,
            "title": null,
            "session_id": null,
            "coordination_mode": "enforce",
            "agent_args": []
        },
        "repository": "example/repository",
        "worktree": null,
        "base_ref": "main",
        "scopes": ["crates/agent-session"],
        "durable_refs": []
    });
    for assignment_id in ["handoff-one", "adopt-one", "account-handoff-race"] {
        insert_orchestration_assignment(
            &state_dir,
            assignment_id,
            json!({
                "schema_version": "agent-session.orchestration-assignment.v1",
                "assignment_id": assignment_id,
                "run_id": "run-one",
                "revision": 1,
                "state": if assignment_id == "account-handoff-race" {
                    "working"
                } else {
                    "assigned"
                },
                "task_summary": "Exercise relationship lifecycle",
                "private_packet_digest": "replaced-by-fixture",
                "primary_manager": main_one_controller,
                "worker": if assignment_id == "handoff-one" {
                    json!({
                        "session_id": "worker-handoff",
                        "session_incarnation": "worker-handoff-incarnation",
                        "session_created_at": "2030-01-01T00:00:00Z"
                    })
                } else if assignment_id == "account-handoff-race" {
                    json!({
                        "session_id": "worker-account-race",
                        "session_incarnation": "worker-account-race-incarnation",
                        "session_created_at": "2030-01-01T00:00:00Z"
                    })
                } else {
                    serde_json::Value::Null
                },
                "collaborators": [],
                "borrowed_by": [],
                "repository": "example/repository",
                "worktree": null,
                "base_ref": "main",
                "scopes": ["crates/agent-session"],
                "durable_refs": [],
                "checkpoint": null,
                "result_summary": null,
                "blocker_summary": null,
                "created_at": "2030-01-01T00:00:01Z",
                "updated_at": "2030-01-01T00:00:01Z"
            }),
            &private_packet,
        );
    }
    let coordination_registry: serde_json::Value = serde_json::from_slice(
        &fs::read(state_dir.join("coordination/registry.json")).expect("coordination registry"),
    )
    .expect("coordination registry json");
    let main_one_claim = coordination_registry["claims"]
        .as_array()
        .expect("claims")
        .iter()
        .find(|claim| claim["session_id"] == "main-one" && claim["state"] == "active")
        .expect("main one claim");
    let main_one_claim_id = main_one_claim["claim_id"].as_str().expect("claim id");
    rewrite_registry(&state_dir, |registry| {
        registry["operations"]
            .as_array_mut()
            .expect("operations")
            .push(json!({
                "schema_version": "agent-session.operation-lease.v1",
                "lease_id": "handoff-operation",
                "session_id": "main-one",
                "session_incarnation": "main-incarnation-one",
                "claim_id": main_one_claim_id,
                "claim_revision": 1,
                "operation": "edit",
                "targets": [],
                "state": "active",
                "revision": 1,
                "started_at": "2030-01-01T00:00:00Z",
                "expires_at": "9999-12-31T23:59:59Z",
                "expires_at_epoch": i64::MAX,
                "execution_token_digest": digest("handoff-operation-token"),
                "activity_revision": 1,
                "runtime_identity_digest": "runtime"
            }));
    });
    let blocked = run_main_agent(
        &checkout,
        &[
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "handoff",
            "handoff-one",
            "--to",
            "main-two@main-incarnation-two",
            "--if-revision",
            "1",
            "--idempotency-key",
            "handoff-blocked-0001",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_CAPABILITY_FILE", &main_one_capability)],
    );
    assert_eq!(blocked.code, 65);
    assert_eq!(
        blocked.stdout_json()["error"]["code"],
        "handoff-not-quiescent"
    );
    rewrite_registry(&state_dir, |registry| {
        registry["operations"] = json!([]);
    });
    seed_active_claim(
        &state_dir,
        "worker-account-race",
        "worker-account-race-incarnation",
        "worker-account-race-claim",
    );
    let account_worker_path = state_dir.join("sessions/worker-account-race/session.json");
    let mut account_worker: serde_json::Value =
        serde_json::from_slice(&fs::read(&account_worker_path).expect("account race worker"))
            .expect("account race worker json");
    account_worker["runtime"]["kind"] = json!("codex_app_server");
    account_worker["runtime"]["codex_app_server_protocol"] = json!("v2");
    account_worker["runtime"]["managed_account_handoff_capability"] =
        json!("agent-session.codex-managed-account-handoff.v1");
    account_worker["runtime"]["codex_app_server_socket"] = json!("/tmp/account-race.sock");
    account_worker["runtime"]["codex_app_server_proxy"] = json!("/tmp/account-race.proxy");
    account_worker["runtime"]["codex_app_server_thread_handoff"] =
        json!("/tmp/account-race.thread");
    account_worker["runtime"]["codex_app_server_thread_attached"] =
        json!("/tmp/account-race.attached");
    account_worker["provider_resume"] = json!({
        "provider": "codex",
        "session_id": "provider-account-race",
        "captured_at": "2030-01-01T00:00:00Z",
        "capture_method": "codex-session-meta",
        "resume_args": ["resume", "provider-account-race"]
    });
    account_worker["codex_account_binding"] = json!({
        "schema_version": "agent-session.codex-account-binding.v1",
        "selected_account": "alpha",
        "revision": 1,
        "state": "bound",
        "applied_runtime_id": "worker-account-race-incarnation",
        "updated_at": "2030-01-01T00:00:00Z"
    });
    write_private_json(&account_worker_path, &account_worker);
    let account_barrier = tmp.path().join("account-handoff-race-barrier");
    fs::create_dir(&account_barrier).expect("account handoff barrier");
    let old_account_handoff = Command::new(bin::resolve("main-agent"))
        .current_dir(&checkout)
        .args([
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "worker",
            "account-handoff",
            "account-handoff-race",
            "--account",
            "beta",
            "--if-revision",
            "1",
            "--authorize-account-change",
            "--timeout",
            "1s",
            "--idempotency-key",
            "former-manager-account-handoff-0001",
            "--format",
            "json",
        ])
        .env("AGENT_SESSION_CAPABILITY_FILE", &main_one_capability)
        .env("AGENT_SESSION_CODEX_ACCOUNT_BROKER", r#"["/bin/false"]"#)
        .env(
            "NILS_AGENT_SESSION_TEST_ACCOUNT_HANDOFF_APPLY_RESULT",
            "success",
        )
        .env(
            "NILS_AGENT_SESSION_TEST_ACCOUNT_HANDOFF_BARRIER_STAGE",
            "after_initial_ownership_read",
        )
        .env(
            "NILS_AGENT_SESSION_TEST_ACCOUNT_HANDOFF_BARRIER_DIR",
            &account_barrier,
        )
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn former manager account handoff");
    let account_deadline = Instant::now() + Duration::from_secs(10);
    while !account_barrier.join("ready").is_file() {
        assert!(
            Instant::now() < account_deadline,
            "account handoff did not pause after the initial ownership read"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let account_assignment_handoff = run_main_agent(
        &checkout,
        &[
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "handoff",
            "account-handoff-race",
            "--to",
            "main-two@main-incarnation-two",
            "--if-revision",
            "1",
            "--idempotency-key",
            "account-assignment-handoff-0001",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_CAPABILITY_FILE", &main_one_capability)],
    );
    assert_eq!(
        account_assignment_handoff.code,
        0,
        "stderr={}",
        account_assignment_handoff.stderr_text()
    );
    fs::write(account_barrier.join("release"), b"continue")
        .expect("release account handoff barrier");
    let old_account_handoff = old_account_handoff
        .wait_with_output()
        .expect("former manager account handoff output");
    assert!(!old_account_handoff.status.success());
    let old_account_envelope: serde_json::Value =
        serde_json::from_slice(&old_account_handoff.stdout).expect("former manager json");
    assert!(
        matches!(
            old_account_envelope["error"]["code"].as_str(),
            Some("assignment-not-found" | "account-handoff-assignment-conflict")
        ),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&old_account_handoff.stdout),
        String::from_utf8_lossy(&old_account_handoff.stderr)
    );
    let account_after_race: serde_json::Value =
        serde_json::from_slice(&fs::read(&account_worker_path).expect("account worker after race"))
            .expect("account worker after race json");
    assert_eq!(
        account_after_race["codex_account_binding"]["selected_account"], "alpha",
        "the former manager must not queue or apply an account"
    );
    assert!(
        account_after_race.get("codex_account_next").is_none(),
        "the former manager must leave no queued account intent"
    );
    assert!(
        !state_dir
            .join("sessions/worker-account-race/auto-resume.json")
            .exists(),
        "the former manager must not re-arm auto-resume"
    );
    rewrite_orchestration_registry(&state_dir, |registry| {
        registry["assignments"]["handoff-one"]["depends_on"] = json!(["adopt-one"]);
    });
    let own_dependency_blocked = run_main_agent(
        &checkout,
        &[
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "handoff",
            "handoff-one",
            "--to",
            "main-two@main-incarnation-two",
            "--if-revision",
            "1",
            "--idempotency-key",
            "handoff-own-dependency-blocked-0001",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_CAPABILITY_FILE", &main_one_capability)],
    );
    assert_eq!(own_dependency_blocked.code, 65);
    assert_eq!(
        own_dependency_blocked.stdout_json()["error"]["code"],
        "handoff-dependency-conflict"
    );
    rewrite_orchestration_registry(&state_dir, |registry| {
        registry["assignments"]["handoff-one"]["depends_on"] = json!([]);
        registry["assignments"]["adopt-one"]["depends_on"] = json!(["handoff-one"]);
    });
    let reverse_dependency_blocked = run_main_agent(
        &checkout,
        &[
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "handoff",
            "handoff-one",
            "--to",
            "main-two@main-incarnation-two",
            "--if-revision",
            "1",
            "--idempotency-key",
            "handoff-reverse-dependency-blocked-0001",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_CAPABILITY_FILE", &main_one_capability)],
    );
    assert_eq!(reverse_dependency_blocked.code, 65);
    assert_eq!(
        reverse_dependency_blocked.stdout_json()["error"]["code"],
        "handoff-dependency-conflict"
    );
    rewrite_orchestration_registry(&state_dir, |registry| {
        registry["assignments"]["adopt-one"]["depends_on"] = json!([]);
    });
    let message_body = tmp.path().join("handoff-message.txt");
    fs::write(&message_body, "target-owned routing only").expect("message body");
    fs::set_permissions(&message_body, fs::Permissions::from_mode(0o600))
        .expect("message body mode");
    let routing_barrier = tmp.path().join("message-routing-barrier");
    fs::create_dir(&routing_barrier).expect("routing barrier");
    let racing_message = Command::new(bin::resolve("main-agent"))
        .current_dir(&checkout)
        .args([
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "worker",
            "message",
            "handoff-one",
            "--body-file",
            message_body.to_str().expect("message body"),
            "--idempotency-key",
            "handoff-racing-source-message-0001",
            "--format",
            "json",
        ])
        .env("AGENT_SESSION_CAPABILITY_FILE", &main_one_capability)
        .env(
            "NILS_AGENT_SESSION_TEST_MESSAGE_ROUTING_BARRIER_DIR",
            &routing_barrier,
        )
        .env(
            "NILS_AGENT_SESSION_TEST_MESSAGE_ROUTING_ASSIGNMENT",
            "handoff-one",
        )
        .env(
            "NILS_AGENT_SESSION_TEST_MESSAGE_ROUTING_WORKER",
            "worker-handoff@worker-handoff-incarnation",
        )
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn racing source message");
    let routing_deadline = Instant::now() + Duration::from_secs(2);
    while !routing_barrier.join("ready").is_file() && Instant::now() < routing_deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        routing_barrier.join("ready").is_file(),
        "message did not pause between its routing read and commit authorization"
    );
    let handed_off = run_main_agent(
        &checkout,
        &[
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "handoff",
            "handoff-one",
            "--to",
            "main-two@main-incarnation-two",
            "--if-revision",
            "1",
            "--idempotency-key",
            "handoff-success-0001",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_CAPABILITY_FILE", &main_one_capability)],
    );
    assert_eq!(handed_off.code, 0, "stderr={}", handed_off.stderr_text());
    assert_eq!(
        data(&handed_off)["assignment"]["primary_manager"]["session_id"],
        "main-two"
    );
    assert_eq!(data(&handed_off)["assignment"]["run_id"], "run-two");
    fs::write(routing_barrier.join("release"), b"continue").expect("release routing barrier");
    let racing_message_output = racing_message
        .wait_with_output()
        .expect("racing source message output");
    assert_eq!(racing_message_output.status.code(), Some(65));
    let racing_message_envelope: serde_json::Value =
        serde_json::from_slice(&racing_message_output.stdout).expect("racing message json");
    assert_eq!(
        racing_message_envelope["error"]["code"], "assignment-not-found",
        "handoff must commit before the paused old routing view is authorized"
    );
    let after_race: serde_json::Value = serde_json::from_slice(
        &fs::read(state_dir.join("coordination/registry.json")).expect("coordination registry"),
    )
    .expect("coordination registry json");
    assert!(
        after_race["messages"]
            .as_array()
            .expect("messages")
            .iter()
            .all(|message| message["recipient_session_id"] != "worker-handoff"),
        "the losing old routing view must not create a mailbox record"
    );
    let source_status = run_main_agent(
        &checkout,
        &[
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "status",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_CAPABILITY_FILE", &main_one_capability)],
    );
    assert!(
        data(&source_status)["assignments"]
            .as_array()
            .expect("source assignments")
            .iter()
            .all(|assignment| assignment["assignment_id"] != "handoff-one"),
        "the source run must lose all routing authority after handoff"
    );
    let target_status = run_main_agent(
        &checkout,
        &[
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "status",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_CAPABILITY_FILE", &main_two_capability)],
    );
    assert!(
        data(&target_status)["assignments"]
            .as_array()
            .expect("target assignments")
            .iter()
            .any(|assignment| {
                assignment["assignment_id"] == "handoff-one"
                    && assignment["primary_manager"]["session_id"] == "main-two"
            }),
        "the target run must gain coherent routing authority after handoff"
    );
    let stale_source_message = run_main_agent(
        &checkout,
        &[
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "worker",
            "message",
            "handoff-one",
            "--body-file",
            message_body.to_str().expect("message body"),
            "--idempotency-key",
            "handoff-source-message-0001",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_CAPABILITY_FILE", &main_one_capability)],
    );
    assert_eq!(stale_source_message.code, 65);
    assert_eq!(
        stale_source_message.stdout_json()["error"]["code"],
        "assignment-not-found"
    );
    let target_message = run_main_agent(
        &checkout,
        &[
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "worker",
            "message",
            "handoff-one",
            "--body-file",
            message_body.to_str().expect("message body"),
            "--idempotency-key",
            "handoff-target-message-0001",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_CAPABILITY_FILE", &main_two_capability)],
    );
    assert_eq!(
        target_message.code,
        0,
        "stderr={}",
        target_message.stderr_text()
    );
    let coordination_registry: serde_json::Value = serde_json::from_slice(
        &fs::read(state_dir.join("coordination/registry.json")).expect("coordination registry"),
    )
    .expect("coordination registry json");
    let handoff_messages = coordination_registry["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .filter(|message| message["recipient_session_id"] == "worker-handoff")
        .collect::<Vec<_>>();
    assert_eq!(handoff_messages.len(), 1);
    assert_eq!(handoff_messages[0]["sender_session_id"], "main-two");

    rewrite_orchestration_registry(&state_dir, |registry| {
        registry["assignments"]["adopt-one"]["submit_recovery"] = json!({
            "schema_version": "main-agent.submit-recovery.v1",
            "attempt_id": "adopt-recovery-attempt",
            "origin": "automatic",
            "session_incarnation": "worker-incarnation",
            "reserved_revision": 1,
            "state": "attempting",
            "attempt_count": 1,
            "result": "single guarded Enter reserved",
            "attempted_at": "2030-01-01T00:00:02Z",
            "updated_at": "2030-01-01T00:00:02Z"
        });
    });
    let adopt_during_recovery = run_main_agent(
        &checkout,
        &[
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "adopt",
            "adopt-one",
            "--if-revision",
            "1",
            "--idempotency-key",
            "adopt-recovery-fenced-0001",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_CAPABILITY_FILE", &main_two_capability)],
    );
    assert_eq!(adopt_during_recovery.code, 65);
    assert_eq!(
        adopt_during_recovery.stdout_json()["error"]["code"],
        "submit-recovery-in-flight"
    );
    rewrite_orchestration_registry(&state_dir, |registry| {
        registry["assignments"]["adopt-one"]["submit_recovery"] = serde_json::Value::Null;
    });

    fs::rename(
        state_dir.join("sessions/main-one"),
        state_dir.join("stale-main-one"),
    )
    .expect("make prior manager stale");
    let adopted = run_main_agent(
        &checkout,
        &[
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "adopt",
            "adopt-one",
            "--if-revision",
            "1",
            "--idempotency-key",
            "adopt-orphan-0001",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_CAPABILITY_FILE", &main_two_capability)],
    );
    assert_eq!(adopted.code, 0, "stderr={}", adopted.stderr_text());
    assert_eq!(data(&adopted)["assignment"]["run_id"], "run-two");
    assert_eq!(
        data(&adopted)["assignment"]["primary_manager"]["session_id"],
        "main-two"
    );
}

#[test]
fn main_agent_worker_wait_transitions_times_out_and_validates_target() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let checkout = tmp.path().join("checkout");
    fs::create_dir(&state_dir).expect("state");
    init_checkout(&checkout, "https://example.invalid/example/repository.git");
    seed_brokers_at(
        &state_dir,
        &[(
            "main-one",
            "main-incarnation-one",
            "main-private-capability-material-0000000001",
            checkout.as_path(),
            Some("enforce"),
        )],
    );
    let main_capability = init_main_run(tmp.path(), &state_dir, &checkout, "main-one", "run-one");
    let private_packet = json!({
        "schema_version": "main-agent.assignment-input.v1",
        "assignment_id": "assignment-one",
        "task_summary": "Worker wait fixture",
        "task": {},
        "launch": {
            "agent": "codex",
            "cwd": checkout,
            "title": null,
            "session_id": null,
            "coordination_mode": "enforce",
            "agent_args": []
        },
        "repository": "example/repository",
        "worktree": null,
        "base_ref": "main",
        "scopes": ["crates/agent-session"],
        "durable_refs": []
    });
    insert_orchestration_assignment(
        &state_dir,
        "assignment-one",
        json!({
            "schema_version": "agent-session.orchestration-assignment.v1",
            "assignment_id": "assignment-one",
            "run_id": "run-one",
            "revision": 2,
            "state": "submitted",
            "task_summary": "Worker wait fixture",
            "private_packet_digest": "replaced-by-fixture",
            "primary_manager": {
                "session_id": "main-one",
                "session_incarnation": "main-incarnation-one",
                "session_created_at": "2030-01-01T00:00:00Z"
            },
            "worker": null,
            "collaborators": [],
            "borrowed_by": [],
            "repository": "example/repository",
            "worktree": null,
            "base_ref": "main",
            "scopes": ["crates/agent-session"],
            "durable_refs": [],
            "checkpoint": null,
            "result_summary": null,
            "blocker_summary": null,
            "created_at": "2030-01-01T00:00:01Z",
            "updated_at": "2030-01-01T00:00:02Z"
        }),
        &private_packet,
    );
    let state = state_dir.to_str().expect("state dir");

    // Level-triggered: an already-submitted assignment returns immediately.
    let transitioned = run_main_agent(
        &checkout,
        &[
            "--state-dir",
            state,
            "worker",
            "wait",
            "assignment-one",
            "--until",
            "submitted",
            "--timeout",
            "5s",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_CAPABILITY_FILE", &main_capability)],
    );
    assert_eq!(
        transitioned.code,
        0,
        "stderr={}",
        transitioned.stderr_text()
    );
    assert_eq!(data(&transitioned)["outcome"], "transitioned");
    assert_eq!(data(&transitioned)["until"], "submitted");
    assert_eq!(
        data(&transitioned)["assignment"]["assignment_id"],
        "assignment-one"
    );
    assert_eq!(data(&transitioned)["assignment"]["state"], "submitted");

    // --any resolves the same assignment without naming it.
    let any = run_main_agent(
        &checkout,
        &[
            "--state-dir",
            state,
            "worker",
            "wait",
            "--any",
            "--until",
            "submitted",
            "--timeout",
            "5s",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_CAPABILITY_FILE", &main_capability)],
    );
    assert_eq!(any.code, 0, "stderr={}", any.stderr_text());
    assert_eq!(data(&any)["outcome"], "transitioned");
    assert_eq!(data(&any)["assignment"]["assignment_id"], "assignment-one");

    // Waiting for a state it will not reach within the bound reports timeout.
    let timed_out = run_main_agent(
        &checkout,
        &[
            "--state-dir",
            state,
            "worker",
            "wait",
            "assignment-one",
            "--until",
            "terminal",
            "--timeout",
            "1s",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_CAPABILITY_FILE", &main_capability)],
    );
    assert_eq!(timed_out.code, 0, "stderr={}", timed_out.stderr_text());
    assert_eq!(data(&timed_out)["outcome"], "timeout");
    assert_eq!(data(&timed_out)["until"], "terminal");

    // An unknown assignment id is an error, not a silent wait.
    let missing = run_main_agent(
        &checkout,
        &[
            "--state-dir",
            state,
            "worker",
            "wait",
            "missing-assignment",
            "--until",
            "submitted",
            "--timeout",
            "1s",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_CAPABILITY_FILE", &main_capability)],
    );
    assert_ne!(missing.code, 0);
    assert_eq!(
        missing.stdout_json()["error"]["code"],
        "assignment-not-found"
    );

    // Neither an id nor --any is rejected before any polling begins.
    let no_target = run_main_agent(
        &checkout,
        &[
            "--state-dir",
            state,
            "worker",
            "wait",
            "--until",
            "submitted",
            "--timeout",
            "1s",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_CAPABILITY_FILE", &main_capability)],
    );
    assert_ne!(no_target.code, 0);
    assert_eq!(
        no_target.stdout_json()["error"]["code"],
        "worker-wait-target"
    );
}

#[test]
fn main_agent_worker_start_decouples_run_revision_and_gates_on_dependencies() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let checkout = tmp.path().join("checkout");
    fs::create_dir(&state_dir).expect("state");
    init_checkout(&checkout, "https://example.invalid/example/repository.git");
    seed_brokers_at(
        &state_dir,
        &[(
            "main-one",
            "main-incarnation-one",
            "main-private-capability-material-0000000001",
            checkout.as_path(),
            Some("enforce"),
        )],
    );
    let main_capability = init_main_run(tmp.path(), &state_dir, &checkout, "main-one", "run-one");

    // Seed an upstream dependency that has NOT been accepted yet.
    let dep_packet = json!({
        "schema_version": "main-agent.assignment-input.v1",
        "assignment_id": "assignment-dep",
        "task_summary": "Upstream dependency",
        "task": {},
        "launch": {
            "agent": "codex",
            "cwd": checkout,
            "title": null,
            "session_id": null,
            "coordination_mode": "enforce",
            "agent_args": []
        },
        "repository": "example/repository",
        "worktree": null,
        "base_ref": "main",
        "scopes": ["crates/agent-session"],
        "durable_refs": []
    });
    insert_orchestration_assignment(
        &state_dir,
        "assignment-dep",
        json!({
            "schema_version": "agent-session.orchestration-assignment.v1",
            "assignment_id": "assignment-dep",
            "run_id": "run-one",
            "revision": 2,
            "state": "working",
            "task_summary": "Upstream dependency",
            "private_packet_digest": "replaced-by-fixture",
            "primary_manager": {
                "session_id": "main-one",
                "session_incarnation": "main-incarnation-one",
                "session_created_at": "2030-01-01T00:00:00Z"
            },
            "worker": null,
            "collaborators": [],
            "borrowed_by": [],
            "repository": "example/repository",
            "worktree": null,
            "base_ref": "main",
            "scopes": ["crates/agent-session"],
            "durable_refs": [],
            "checkpoint": null,
            "result_summary": null,
            "blocker_summary": null,
            "created_at": "2030-01-01T00:00:01Z",
            "updated_at": "2030-01-01T00:00:02Z"
        }),
        &dep_packet,
    );
    let state = state_dir.to_str().expect("state dir");

    // A dependent packet naming the not-yet-accepted dependency and a missing one.
    let dependent_path = tmp.path().join("dependent.json");
    write_private_json(
        &dependent_path,
        &json!({
            "schema_version": "main-agent.assignment-input.v1",
            "assignment_id": "assignment-dependent",
            "task_summary": "Downstream work",
            "task": {},
            "launch": {
                "agent": "codex",
                "cwd": checkout,
                "title": null,
                "session_id": null,
                "coordination_mode": "enforce",
                "agent_args": []
            },
            "repository": "example/repository",
            "worktree": null,
            "base_ref": "main",
            "scopes": ["crates/agent-session"],
            "durable_refs": [],
            "depends_on": ["assignment-dep", "assignment-missing"]
        }),
    );

    // T2 decouple + T5 gate: OMITTING --if-run-revision is accepted (no run
    // fence), and the dependent is refused until its dependencies are accepted.
    let refused = run_main_agent(
        &checkout,
        &[
            "--state-dir",
            state,
            "worker",
            "start",
            "--assignment-file",
            dependent_path.to_str().expect("dependent path"),
            "--idempotency-key",
            "start-dependent-0001",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_CAPABILITY_FILE", &main_capability)],
    );
    assert_ne!(
        refused.code,
        0,
        "dependent must be refused; stdout={}",
        refused.stdout_text()
    );
    assert_eq!(
        refused.stdout_json()["error"]["code"],
        "dependency-not-satisfied"
    );
    let blocked_on = refused.stdout_json()["error"]["details"]["blocked_on"].clone();
    let ids: Vec<String> = blocked_on
        .as_array()
        .expect("blocked_on array")
        .iter()
        .map(|entry| entry["assignment_id"].as_str().expect("id").to_string())
        .collect();
    assert!(
        ids.contains(&"assignment-dep".to_string()),
        "pre-terminal dependency is reported: {ids:?}"
    );
    assert!(
        ids.contains(&"assignment-missing".to_string()),
        "missing dependency is reported: {ids:?}"
    );

    // Refusal must not create the assignment (nothing to clean up).
    let missing_after = run_main_agent(
        &checkout,
        &[
            "--state-dir",
            state,
            "worker",
            "show",
            "assignment-dependent",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_CAPABILITY_FILE", &main_capability)],
    );
    assert_ne!(missing_after.code, 0);
    assert_eq!(
        missing_after.stdout_json()["error"]["code"],
        "assignment-not-found"
    );

    // A supplied but stale --if-run-revision still fences (honored when present,
    // and it fires before any launch).
    let stale = run_main_agent(
        &checkout,
        &[
            "--state-dir",
            state,
            "worker",
            "start",
            "--assignment-file",
            dependent_path.to_str().expect("dependent path"),
            "--if-run-revision",
            "999",
            "--idempotency-key",
            "start-dependent-0002",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_CAPABILITY_FILE", &main_capability)],
    );
    assert_ne!(stale.code, 0);
    assert_eq!(
        stale.stdout_json()["error"]["code"],
        "orchestration-revision-conflict"
    );
}

#[test]
fn main_agent_worker_start_batch_isolates_per_lane_results() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let checkout = tmp.path().join("checkout");
    fs::create_dir(&state_dir).expect("state");
    init_checkout(&checkout, "https://example.invalid/example/repository.git");
    seed_brokers_at(
        &state_dir,
        &[(
            "main-one",
            "main-incarnation-one",
            "main-private-capability-material-0000000001",
            checkout.as_path(),
            Some("enforce"),
        )],
    );
    let main_capability = init_main_run(tmp.path(), &state_dir, &checkout, "main-one", "run-one");

    // An unaccepted dependency so dependent lanes refuse before any launch.
    let dep_packet = json!({
        "schema_version": "main-agent.assignment-input.v1",
        "assignment_id": "assignment-dep",
        "task_summary": "Batch upstream",
        "task": {},
        "launch": {
            "agent": "codex", "cwd": checkout, "title": null, "session_id": null,
            "coordination_mode": "enforce", "agent_args": []
        },
        "repository": "example/repository", "worktree": null, "base_ref": "main",
        "scopes": ["crates/agent-session"], "durable_refs": []
    });
    insert_orchestration_assignment(
        &state_dir,
        "assignment-dep",
        json!({
            "schema_version": "agent-session.orchestration-assignment.v1",
            "assignment_id": "assignment-dep",
            "run_id": "run-one",
            "revision": 2,
            "state": "working",
            "task_summary": "Batch upstream",
            "private_packet_digest": "replaced-by-fixture",
            "primary_manager": {
                "session_id": "main-one",
                "session_incarnation": "main-incarnation-one",
                "session_created_at": "2030-01-01T00:00:00Z"
            },
            "worker": null,
            "collaborators": [],
            "borrowed_by": [],
            "repository": "example/repository",
            "worktree": null,
            "base_ref": "main",
            "scopes": ["crates/agent-session"],
            "durable_refs": [],
            "checkpoint": null,
            "result_summary": null,
            "blocker_summary": null,
            "created_at": "2030-01-01T00:00:01Z",
            "updated_at": "2030-01-01T00:00:02Z"
        }),
        &dep_packet,
    );
    let state = state_dir.to_str().expect("state dir");

    // Batch dir: two lanes depend on the unaccepted dependency (refuse), one
    // lane is schema-invalid — all fail before launch, exercising per-lane
    // isolation across distinct error kinds.
    let batch_dir = tmp.path().join("batch");
    fs::create_dir(&batch_dir).expect("batch dir");
    for name in ["lane-a", "lane-b"] {
        write_private_json(
            &batch_dir.join(format!("{name}.json")),
            &json!({
                "schema_version": "main-agent.assignment-input.v1",
                "assignment_id": format!("assignment-{name}"),
                "task_summary": "Batch lane",
                "task": {},
                "launch": {
                    "agent": "codex", "cwd": checkout, "title": null, "session_id": null,
                    "coordination_mode": "enforce", "agent_args": []
                },
                "repository": "example/repository", "worktree": null, "base_ref": "main",
                "scopes": ["crates/agent-session"], "durable_refs": [],
                "depends_on": ["assignment-dep"]
            }),
        );
    }
    write_private_json(
        &batch_dir.join("lane-bad.json"),
        &json!({
            "schema_version": "wrong.schema.v1",
            "assignment_id": "assignment-bad",
            "task_summary": "Invalid lane",
            "task": {},
            "launch": {
                "agent": "codex", "cwd": checkout, "title": null, "session_id": null,
                "coordination_mode": "enforce", "agent_args": []
            }
        }),
    );

    let batch = run_main_agent(
        &checkout,
        &[
            "--state-dir",
            state,
            "worker",
            "start",
            "--batch",
            batch_dir.to_str().expect("batch dir"),
            "--idempotency-key",
            "batch-0001",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_CAPABILITY_FILE", &main_capability)],
    );
    assert_eq!(
        batch.code,
        0,
        "batch succeeds; stderr={}",
        batch.stderr_text()
    );
    assert_eq!(
        data(&batch)["schema_version"],
        "main-agent.worker-start-batch.v1"
    );
    let lanes = data(&batch)["lanes"].as_array().expect("lanes").clone();
    assert_eq!(lanes.len(), 3, "one lane per packet: {lanes:?}");
    assert!(
        lanes.iter().all(|lane| lane["ok"] == json!(false)),
        "every lane failed before launch: {lanes:?}"
    );
    let codes: Vec<String> = lanes
        .iter()
        .map(|lane| {
            lane["error"]["code"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect();
    assert_eq!(
        codes
            .iter()
            .filter(|code| *code == "dependency-not-satisfied")
            .count(),
        2,
        "two dependent lanes refuse: {codes:?}"
    );
    assert_eq!(
        codes
            .iter()
            .filter(|code| *code == "invalid-orchestration-input")
            .count(),
        1,
        "one invalid lane: {codes:?}"
    );
    let launched = fs::read_dir(state_dir.join("sessions"))
        .map(|dir| {
            dir.filter_map(Result::ok)
                .filter(|entry| entry.file_name().to_string_lossy().starts_with("worker-"))
                .count()
        })
        .unwrap_or(0);
    assert_eq!(launched, 0, "refused/invalid lanes must not launch workers");

    let replay_batch = || {
        run_main_agent(
            &checkout,
            &[
                "--state-dir",
                state,
                "worker",
                "start",
                "--batch",
                batch_dir.to_str().expect("batch dir"),
                "--idempotency-key",
                "batch-0001",
                "--format",
                "json",
            ],
            &[("AGENT_SESSION_CAPABILITY_FILE", &main_capability)],
        )
    };
    let exact_replay = replay_batch();
    assert_eq!(
        exact_replay.code,
        0,
        "stderr={}",
        exact_replay.stderr_text()
    );
    assert_eq!(
        data(&exact_replay),
        data(&batch),
        "exact batch replay must resume the immutable manifest"
    );

    let lane_a = batch_dir.join("lane-a.json");
    let lane_b = batch_dir.join("lane-b.json");
    let lane_bad = batch_dir.join("lane-bad.json");
    let lane_a_bytes = fs::read(&lane_a).expect("lane-a bytes");
    let lane_b_bytes = fs::read(&lane_b).expect("lane-b bytes");

    fs::copy(&lane_a, batch_dir.join("lane-extra.json")).expect("add lane");
    let added = replay_batch();
    assert_eq!(added.code, 65, "outcome={}", added.stdout_text());
    assert_eq!(added.stdout_json()["error"]["code"], "idempotency-conflict");
    fs::remove_file(batch_dir.join("lane-extra.json")).expect("remove added lane");

    let renamed_lane = batch_dir.join("lane-renamed.json");
    fs::rename(&lane_a, &renamed_lane).expect("rename lane");
    let renamed = replay_batch();
    assert_eq!(renamed.code, 65, "outcome={}", renamed.stdout_text());
    assert_eq!(
        renamed.stdout_json()["error"]["code"],
        "idempotency-conflict"
    );
    fs::rename(&renamed_lane, &lane_a).expect("restore lane name");

    let mut edited_lane = lane_a_bytes.clone();
    edited_lane.extend_from_slice(b"\n ");
    fs::write(&lane_a, edited_lane).expect("edit lane bytes");
    let edited = replay_batch();
    assert_eq!(edited.code, 65, "outcome={}", edited.stdout_text());
    assert_eq!(
        edited.stdout_json()["error"]["code"],
        "idempotency-conflict"
    );
    fs::write(&lane_a, &lane_a_bytes).expect("restore lane bytes");

    fs::remove_file(&lane_b).expect("remove lane");
    let removed = replay_batch();
    assert_eq!(removed.code, 65, "outcome={}", removed.stdout_text());
    assert_eq!(
        removed.stdout_json()["error"]["code"],
        "idempotency-conflict"
    );
    fs::write(&lane_b, lane_b_bytes).expect("restore removed lane");
    assert!(lane_bad.is_file());

    let launched_after_conflicts = fs::read_dir(state_dir.join("sessions"))
        .map(|dir| {
            dir.filter_map(Result::ok)
                .filter(|entry| entry.file_name().to_string_lossy().starts_with("worker-"))
                .count()
        })
        .unwrap_or(0);
    assert_eq!(
        launched_after_conflicts, launched,
        "manifest conflicts must fail before any new worker launch"
    );

    // Neither source flag is a usage error.
    let neither = run_main_agent(
        &checkout,
        &[
            "--state-dir",
            state,
            "worker",
            "start",
            "--idempotency-key",
            "batch-0002",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_CAPABILITY_FILE", &main_capability)],
    );
    assert_ne!(neither.code, 0);
    assert_eq!(
        neither.stdout_json()["error"]["code"],
        "worker-start-source"
    );

    // Batch is deliberately transport-only so a bounded lane count cannot
    // multiply the single-assignment readiness deadline.
    let batch_readiness = run_main_agent(
        &checkout,
        &[
            "--state-dir",
            state,
            "worker",
            "start",
            "--batch",
            batch_dir.to_str().expect("batch dir"),
            "--await-ready",
            "1s",
            "--idempotency-key",
            "batch-readiness-conflict-0001",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_CAPABILITY_FILE", &main_capability)],
    );
    assert_ne!(batch_readiness.code, 0);
    assert_eq!(
        batch_readiness.stdout_json()["error"]["code"],
        "parse-error"
    );
    assert!(
        batch_readiness.stdout_json()["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("cannot be used with"),
        "unexpected clap conflict: {}",
        batch_readiness.stdout_text()
    );

    // An empty batch directory is rejected.
    let empty_dir = tmp.path().join("empty");
    fs::create_dir(&empty_dir).expect("empty dir");
    let empty = run_main_agent(
        &checkout,
        &[
            "--state-dir",
            state,
            "worker",
            "start",
            "--batch",
            empty_dir.to_str().expect("empty dir"),
            "--idempotency-key",
            "batch-0003",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_CAPABILITY_FILE", &main_capability)],
    );
    assert_ne!(empty.code, 0);
    assert_eq!(
        empty.stdout_json()["error"]["code"],
        "invalid-orchestration-input"
    );
}

#[test]
fn main_agent_worker_start_batch_replays_repaired_cwd_without_duplicate_lanes() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let checkout = tmp.path().join("checkout");
    let missing_checkout = tmp.path().join("missing-checkout");
    fs::create_dir(&state_dir).expect("state");
    init_checkout(&checkout, "https://example.invalid/example/repository.git");
    seed_brokers_at(
        &state_dir,
        &[(
            "main-one",
            "main-incarnation-one",
            "main-private-capability-material-0000000001",
            checkout.as_path(),
            Some("enforce"),
        )],
    );
    let main_capability = init_main_run(tmp.path(), &state_dir, &checkout, "main-one", "run-one");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let claude_bin = fake_agent(tmp.path(), "claude-worker");
    let codex_bin = fake_agent(tmp.path(), "codex-worker");
    let codex_home = tmp.path().join("codex-home");
    fs::create_dir(&codex_home).expect("Codex home");
    fs::write(codex_home.join("config.toml"), "[projects.").expect("malformed Codex config");
    let batch_dir = tmp.path().join("repairable-batch");
    fs::create_dir(&batch_dir).expect("batch dir");
    for (name, assignment_id, worker_id, agent, cwd) in [
        (
            "a-missing.json",
            "assignment-batch-missing",
            "worker-batch-missing",
            "codex",
            missing_checkout.as_path(),
        ),
        (
            "b-valid.json",
            "assignment-batch-valid",
            "worker-batch-valid",
            "claude",
            checkout.as_path(),
        ),
    ] {
        write_private_json(
            &batch_dir.join(name),
            &json!({
                "schema_version": "main-agent.assignment-input.v1",
                "assignment_id": assignment_id,
                "task_summary": "Resume a manually repaired batch lane",
                "task": {},
                "launch": {
                    "agent": agent,
                    "cwd": cwd,
                    "title": null,
                    "session_id": worker_id,
                    "coordination_mode": "enforce",
                    "agent_args": []
                },
                "repository": "example/repository",
                "worktree": cwd,
                "base_ref": "main",
                "scopes": ["crates/agent-session"],
                "durable_refs": []
            }),
        );
    }
    let tmux_arg = tmux_bin.to_string_lossy().into_owned();
    let tmux_log_arg = tmux_log.to_string_lossy().into_owned();
    let claude_arg = claude_bin.to_string_lossy().into_owned();
    let codex_arg = codex_bin.to_string_lossy().into_owned();
    let codex_home_arg = codex_home.to_string_lossy().into_owned();
    let args = [
        "--state-dir",
        state_dir.to_str().expect("state dir"),
        "worker",
        "start",
        "--batch",
        batch_dir.to_str().expect("batch dir"),
        "--idempotency-key",
        "batch-repairable-cwd-0001",
        "--format",
        "json",
    ];
    let envs = [
        ("AGENT_SESSION_CAPABILITY_FILE", main_capability.as_str()),
        ("AGENT_SESSION_TMUX_BIN", tmux_arg.as_str()),
        ("AGENT_SESSION_CLAUDE_BIN", claude_arg.as_str()),
        ("AGENT_SESSION_CODEX_BIN", codex_arg.as_str()),
        ("AGENT_SESSION_FAKE_TMUX_LOG", tmux_log_arg.as_str()),
        ("CODEX_HOME", codex_home_arg.as_str()),
    ];
    let blocked = run_main_agent(&checkout, &args, &envs);
    assert_eq!(blocked.code, 0, "outcome={}", blocked.stdout_text());
    let blocked_data = data(&blocked);
    let blocked_lanes = blocked_data["lanes"].as_array().expect("blocked lanes");
    assert_eq!(
        blocked_lanes[0]["error"]["code"],
        "assignment-launch-cwd-unavailable"
    );
    assert_eq!(blocked_lanes[0]["resumable"], true);
    assert_eq!(blocked_lanes[0]["error"]["details"]["retryable"], true);
    assert_eq!(blocked_lanes[1]["ok"], true);
    assert_eq!(
        tmux_calls(&tmux_log)
            .iter()
            .filter(|call| call.first().is_some_and(|arg| arg == "new-session"))
            .count(),
        1,
        "the independent valid lane must launch on the first call"
    );

    fs::create_dir(&missing_checkout).expect("repaired checkout");
    let unverified = run_main_agent(&checkout, &args, &envs);
    assert_eq!(unverified.code, 0, "outcome={}", unverified.stdout_text());
    assert_eq!(
        data(&unverified)["lanes"][0]["error"]["code"],
        "provider-trust-unverified"
    );
    assert_eq!(data(&unverified)["lanes"][0]["resumable"], true);
    assert_eq!(
        tmux_calls(&tmux_log)
            .iter()
            .filter(|call| call.first().is_some_and(|arg| arg == "new-session"))
            .count(),
        1,
        "unverifiable trust replay must not duplicate the completed lane"
    );

    fs::write(codex_home.join("config.toml"), "").expect("untrusted Codex config");
    let trust_required = run_main_agent(&checkout, &args, &envs);
    assert_eq!(
        trust_required.code,
        0,
        "outcome={}",
        trust_required.stdout_text()
    );
    assert_eq!(
        data(&trust_required)["lanes"][0]["error"]["code"],
        "provider-trust-required"
    );
    assert_eq!(data(&trust_required)["lanes"][0]["resumable"], true);
    assert_eq!(
        tmux_calls(&tmux_log)
            .iter()
            .filter(|call| call.first().is_some_and(|arg| arg == "new-session"))
            .count(),
        1,
        "required trust replay must not duplicate the completed lane"
    );

    write_trusted_codex_config(&codex_home, &[&missing_checkout]);
    let repaired = run_main_agent(&checkout, &args, &envs);
    assert_eq!(repaired.code, 0, "outcome={}", repaired.stdout_text());
    assert!(
        data(&repaired)["lanes"]
            .as_array()
            .expect("repaired lanes")
            .iter()
            .all(|lane| lane["ok"] == true),
        "the exact replay must converge every lane: {}",
        repaired.stdout_text()
    );
    assert_eq!(
        tmux_calls(&tmux_log)
            .iter()
            .filter(|call| call.first().is_some_and(|arg| arg == "new-session"))
            .count(),
        2,
        "repair replay must launch only the formerly blocked lane"
    );
    let replay = run_main_agent(&checkout, &args, &envs);
    assert_eq!(data(&replay), data(&repaired));
    assert_eq!(
        tmux_calls(&tmux_log)
            .iter()
            .filter(|call| call.first().is_some_and(|arg| arg == "new-session"))
            .count(),
        2,
        "completed replay must not duplicate either lane"
    );
}

#[test]
fn main_agent_worker_start_batch_stale_lane_owner_cannot_create_the_child_session() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let checkout = tmp.path().join("checkout");
    fs::create_dir(&state_dir).expect("state");
    init_checkout(&checkout, "https://example.invalid/example/repository.git");
    seed_brokers_at(
        &state_dir,
        &[(
            "main-one",
            "main-incarnation-one",
            "main-private-capability-material-0000000001",
            checkout.as_path(),
            Some("enforce"),
        )],
    );
    let main_capability = init_main_run(tmp.path(), &state_dir, &checkout, "main-one", "run-one");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let codex_bin = fake_agent(tmp.path(), "codex-worker");
    let batch_dir = tmp.path().join("stale-owner-batch");
    fs::create_dir(&batch_dir).expect("batch dir");
    write_private_json(
        &batch_dir.join("lane.json"),
        &json!({
            "schema_version": "main-agent.assignment-input.v1",
            "assignment_id": "assignment-stale-batch-owner",
            "task_summary": "Fence a stale batch lane owner",
            "task": {},
            "launch": {
                "agent": "codex", "cwd": checkout, "title": null,
                "session_id": "worker-stale-batch-owner",
                "coordination_mode": "enforce", "agent_args": []
            },
            "repository": "example/repository", "worktree": null, "base_ref": "main",
            "scopes": ["crates/agent-session"], "durable_refs": []
        }),
    );
    let codex_home = tmp.path().join("codex-home");
    write_trusted_codex_config(&codex_home, &[&checkout]);
    let barrier = tmp.path().join("batch-lane-barrier");
    fs::create_dir(&barrier).expect("barrier");
    let idempotency_key = "batch-stale-owner-0001";
    let spawn_batch = |pause: bool| {
        let mut command = Command::new(bin::resolve("main-agent"));
        command
            .current_dir(&checkout)
            .args([
                "--state-dir",
                state_dir.to_str().expect("state dir"),
                "worker",
                "start",
                "--batch",
                batch_dir.to_str().expect("batch dir"),
                "--idempotency-key",
                idempotency_key,
                "--format",
                "json",
            ])
            .env("AGENT_SESSION_CAPABILITY_FILE", &main_capability)
            .env("AGENT_SESSION_TMUX_BIN", &tmux_bin)
            .env("AGENT_SESSION_CODEX_BIN", &codex_bin)
            .env("CODEX_HOME", &codex_home)
            .env("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log)
            .env("NILS_AGENT_SESSION_TEST_BATCH_LANE_LEASE_SECS", "1")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if pause {
            command
                .env(
                    "NILS_AGENT_SESSION_TEST_BATCH_LANE_BARRIER_STAGE",
                    "before_session_create",
                )
                .env("NILS_AGENT_SESSION_TEST_BATCH_LANE_BARRIER_DIR", &barrier);
        }
        command.spawn().expect("spawn batch")
    };

    let stale = spawn_batch(true);
    let barrier_deadline = Instant::now() + Duration::from_secs(10);
    while !barrier.join("ready").is_file() {
        assert!(
            Instant::now() < barrier_deadline,
            "stale lane owner never reached the session create boundary"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let receipt_key = format!("main-one:main-incarnation-one:{idempotency_key}");
    let first_owner = orchestration_registry(&state_dir)["receipts"][&receipt_key]["outcome"]
        ["lanes"][0]["owner_id"]
        .as_str()
        .expect("first lane owner")
        .to_string();
    std::thread::sleep(Duration::from_secs(2));
    let successor = spawn_batch(false);
    let takeover_deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let owner = orchestration_registry(&state_dir)["receipts"][&receipt_key]["outcome"]["lanes"]
            [0]["owner_id"]
            .as_str()
            .map(str::to_string);
        if owner.as_deref().is_some_and(|owner| owner != first_owner) {
            break;
        }
        assert!(
            Instant::now() < takeover_deadline,
            "successor did not take over the expired batch lane"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    fs::write(barrier.join("release"), b"release").expect("release stale lane owner");

    let stale = stale.wait_with_output().expect("stale batch");
    assert!(!stale.status.success(), "the stale owner must be fenced");
    let stale_json: serde_json::Value =
        serde_json::from_slice(&stale.stdout).expect("stale owner json");
    assert_eq!(
        stale_json["error"]["code"],
        "worker-start-batch-lane-owner-changed"
    );
    let successor = successor.wait_with_output().expect("successor batch");
    assert!(
        successor.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&successor.stdout),
        String::from_utf8_lossy(&successor.stderr)
    );
    let successor_json: serde_json::Value =
        serde_json::from_slice(&successor.stdout).expect("successor json");
    assert_eq!(successor_json["data"]["lanes"][0]["ok"], true);
    assert_eq!(
        tmux_calls(&tmux_log)
            .iter()
            .filter(|call| call.first().is_some_and(|arg| arg == "new-session"))
            .count(),
        1,
        "the post-lock owner recheck must allow exactly one child session create"
    );

    let replay = spawn_batch(false)
        .wait_with_output()
        .expect("immediate exact replay");
    assert!(replay.status.success());
    let replay_json: serde_json::Value =
        serde_json::from_slice(&replay.stdout).expect("replay json");
    assert_eq!(replay_json["data"], successor_json["data"]);
}

#[test]
fn main_agent_worker_start_revalidates_controller_claim_immediately_before_session_create() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let checkout = tmp.path().join("checkout");
    fs::create_dir(&state_dir).expect("state");
    init_checkout(&checkout, "https://example.invalid/example/repository.git");
    seed_brokers_at(
        &state_dir,
        &[(
            "main-one",
            "main-incarnation-one",
            "main-private-capability-material-0000000001",
            checkout.as_path(),
            Some("enforce"),
        )],
    );
    let main_capability = init_main_run(tmp.path(), &state_dir, &checkout, "main-one", "run-one");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let codex_bin = fake_agent(tmp.path(), "codex-worker");
    let codex_home = tmp.path().join("codex-home");
    write_trusted_codex_config(&codex_home, &[&checkout]);
    let assignment_path = tmp.path().join("assignment-controller-fence.json");
    write_private_json(
        &assignment_path,
        &json!({
            "schema_version": "main-agent.assignment-input.v1",
            "assignment_id": "assignment-controller-fence",
            "task_summary": "Fence a controller whose claim ended before launch",
            "task": {},
            "launch": {
                "agent": "codex", "cwd": checkout, "title": null,
                "session_id": "worker-controller-fence",
                "coordination_mode": "enforce", "agent_args": []
            },
            "repository": "example/repository", "worktree": checkout, "base_ref": "main",
            "scopes": ["crates/agent-session"], "durable_refs": []
        }),
    );
    let barrier = tmp.path().join("controller-claim-barrier");
    fs::create_dir(&barrier).expect("barrier");
    let mut command = Command::new(bin::resolve("main-agent"));
    command
        .current_dir(&checkout)
        .args([
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "worker",
            "start",
            "--assignment-file",
            assignment_path.to_str().expect("assignment path"),
            "--await-ready",
            "0",
            "--idempotency-key",
            "worker-start-controller-fence-0001",
            "--format",
            "json",
        ])
        .env("AGENT_SESSION_CAPABILITY_FILE", &main_capability)
        .env("AGENT_SESSION_TMUX_BIN", &tmux_bin)
        .env("AGENT_SESSION_CODEX_BIN", &codex_bin)
        .env("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log)
        .env("CODEX_HOME", &codex_home)
        .env(
            "NILS_AGENT_SESSION_TEST_BATCH_LANE_BARRIER_STAGE",
            "before_session_create",
        )
        .env("NILS_AGENT_SESSION_TEST_BATCH_LANE_BARRIER_DIR", &barrier)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = command.spawn().expect("spawn worker start");
    let barrier_deadline = Instant::now() + Duration::from_secs(10);
    while !barrier.join("ready").is_file() {
        assert!(
            Instant::now() < barrier_deadline,
            "worker start never reached the session create boundary"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    rewrite_registry(&state_dir, |registry| {
        let claim = registry["claims"]
            .as_array_mut()
            .expect("claims")
            .iter_mut()
            .find(|claim| claim["session_id"] == "main-one" && claim["state"] == "active")
            .expect("active controller claim");
        claim["state"] = json!("released");
        claim["revision"] = json!(claim["revision"].as_u64().expect("claim revision") + 1);
    });
    fs::write(barrier.join("release"), b"release").expect("release worker start");
    let output = child.wait_with_output().expect("worker start output");
    assert!(
        !output.status.success(),
        "claimless controller must be fenced"
    );
    let outcome: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("worker start error json");
    assert_eq!(outcome["error"]["code"], "claim-not-active");
    assert!(
        tmux_calls(&tmux_log).is_empty(),
        "the final claim fence must run before tmux new-session"
    );
    assert!(
        !state_dir.join("sessions/worker-controller-fence").exists(),
        "the fenced controller must not create a child session record"
    );
    let registry = orchestration_registry(&state_dir);
    assert_eq!(
        registry["assignments"]["assignment-controller-fence"]["state"], "starting",
        "the durable pending assignment remains resumable by an authorized controller"
    );
}

#[test]
fn main_agent_worker_start_fences_claim_release_through_attachment_and_pins_codex_home() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let checkout = tmp.path().join("checkout");
    fs::create_dir(&state_dir).expect("state");
    init_checkout(&checkout, "https://example.invalid/example/repository.git");
    seed_brokers_at(
        &state_dir,
        &[(
            "main-one",
            "main-incarnation-one",
            "main-private-capability-material-0000000001",
            checkout.as_path(),
            Some("enforce"),
        )],
    );
    let main_capability = init_main_run(tmp.path(), &state_dir, &checkout, "main-one", "run-one");
    let coordination = load_coordination_registry(&state_dir);
    let claim = coordination["claims"]
        .as_array()
        .expect("claims")
        .iter()
        .find(|claim| claim["session_id"] == "main-one" && claim["state"] == "active")
        .expect("active main claim");
    let claim_id = claim["claim_id"].as_str().expect("claim id").to_string();
    let claim_revision = claim["revision"]
        .as_u64()
        .expect("claim revision")
        .to_string();
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let codex_bin = fake_agent(tmp.path(), "codex-worker");
    let codex_home_a = tmp.path().join("codex-home-a");
    let codex_home_b = tmp.path().join("codex-home-b");
    write_trusted_codex_config(&codex_home_a, &[&checkout]);
    write_trusted_codex_config(&codex_home_b, &[&checkout]);
    let codex_home_link = tmp.path().join("codex-home-current");
    std::os::unix::fs::symlink(&codex_home_a, &codex_home_link).expect("Codex home link");
    let assignment_path = tmp.path().join("assignment-authority-fence.json");
    write_private_json(
        &assignment_path,
        &json!({
            "schema_version": "main-agent.assignment-input.v1",
            "assignment_id": "assignment-authority-fence",
            "task_summary": "Hold controller authority through worker attachment",
            "task": {},
            "launch": {
                "agent": "codex", "cwd": checkout, "title": null,
                "session_id": "worker-authority-fence",
                "coordination_mode": "enforce", "agent_args": []
            },
            "repository": "example/repository", "worktree": checkout, "base_ref": "main",
            "scopes": ["crates/agent-session"], "durable_refs": []
        }),
    );
    let barrier = tmp.path().join("authority-fence-barrier");
    fs::create_dir(&barrier).expect("barrier");
    let mut command = Command::new(bin::resolve("main-agent"));
    command
        .current_dir(&checkout)
        .args([
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "worker",
            "start",
            "--assignment-file",
            assignment_path.to_str().expect("assignment path"),
            "--await-ready",
            "0",
            "--idempotency-key",
            "worker-start-authority-fence-0001",
            "--format",
            "json",
        ])
        .env("AGENT_SESSION_CAPABILITY_FILE", &main_capability)
        .env("AGENT_SESSION_TMUX_BIN", &tmux_bin)
        .env("AGENT_SESSION_CODEX_BIN", &codex_bin)
        .env("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log)
        .env("CODEX_HOME", &codex_home_link)
        .env(
            "NILS_AGENT_SESSION_TEST_BATCH_LANE_BARRIER_STAGE",
            "after_authority_fence",
        )
        .env("NILS_AGENT_SESSION_TEST_BATCH_LANE_BARRIER_DIR", &barrier)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = command.spawn().expect("spawn worker start");
    let barrier_deadline = Instant::now() + Duration::from_secs(10);
    while !barrier.join("ready").is_file() {
        assert!(
            Instant::now() < barrier_deadline,
            "worker start never established the authority fence"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    fs::remove_file(&codex_home_link).expect("remove original Codex home link");
    std::os::unix::fs::symlink(&codex_home_b, &codex_home_link).expect("retarget Codex home link");
    let state_arg = state_dir.to_string_lossy().into_owned();
    let release_while_fenced = run_with_env(
        &checkout,
        &[
            "--state-dir",
            &state_arg,
            "work-context",
            "release",
            "--session",
            "main-one",
            "--claim",
            &claim_id,
            "--if-revision",
            &claim_revision,
            "--idempotency-key",
            "release-controller-while-worker-starting-0001",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_CAPABILITY_FILE", &main_capability)],
    );
    assert_ne!(release_while_fenced.code, 0);
    assert_eq!(
        release_while_fenced.stdout_json()["error"]["code"],
        "operation-in-progress"
    );
    fs::write(barrier.join("release"), b"release").expect("release worker start");
    let output = child.wait_with_output().expect("worker start output");
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let worker: serde_json::Value = serde_json::from_slice(
        &fs::read(state_dir.join("sessions/worker-authority-fence/session.json"))
            .expect("worker session"),
    )
    .expect("worker session json");
    let canonical_codex_home_a =
        fs::canonicalize(&codex_home_a).expect("canonical original Codex home");
    assert_eq!(
        worker["runtime"]["agent_profile_provider_config_dir"],
        canonical_codex_home_a.to_string_lossy().as_ref()
    );
    assert!(
        tmux_calls(&tmux_log).iter().any(|call| {
            call.iter().any(|arg| {
                arg == &format!("CODEX_HOME={}", canonical_codex_home_a.to_string_lossy())
            })
        }),
        "retargeting the input symlink must not change the provider configuration root"
    );
    assert_eq!(
        orchestration_registry(&state_dir)["assignments"]["assignment-authority-fence"]["worker"]["session_id"],
        "worker-authority-fence",
        "the fence must remain active until the worker is durably attached"
    );
    let release_after_attach = run_with_env(
        &checkout,
        &[
            "--state-dir",
            &state_arg,
            "work-context",
            "release",
            "--session",
            "main-one",
            "--claim",
            &claim_id,
            "--if-revision",
            &claim_revision,
            "--idempotency-key",
            "release-controller-after-worker-start-0001",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_CAPABILITY_FILE", &main_capability)],
    );
    assert_eq!(
        release_after_attach.code,
        0,
        "stderr={}",
        release_after_attach.stderr_text()
    );
}

#[test]
fn main_agent_worker_start_replay_reuses_the_crash_retained_authority_fence() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let checkout = tmp.path().join("checkout");
    fs::create_dir(&state_dir).expect("state");
    init_checkout(&checkout, "https://example.invalid/example/repository.git");
    seed_brokers_at(
        &state_dir,
        &[(
            "main-one",
            "main-incarnation-one",
            "main-private-capability-material-0000000001",
            checkout.as_path(),
            Some("enforce"),
        )],
    );
    let main_capability = init_main_run(tmp.path(), &state_dir, &checkout, "main-one", "run-one");
    let coordination = load_coordination_registry(&state_dir);
    let claim = coordination["claims"]
        .as_array()
        .expect("claims")
        .iter()
        .find(|claim| claim["session_id"] == "main-one" && claim["state"] == "active")
        .expect("active main claim");
    let claim_id = claim["claim_id"].as_str().expect("claim id").to_string();
    let claim_revision = claim["revision"]
        .as_u64()
        .expect("claim revision")
        .to_string();
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let codex_bin = fake_agent(tmp.path(), "codex-worker");
    let codex_home = tmp.path().join("codex-home");
    write_trusted_codex_config(&codex_home, &[&checkout]);
    let assignment_path = tmp.path().join("assignment-fence-replay.json");
    write_private_json(
        &assignment_path,
        &json!({
            "schema_version": "main-agent.assignment-input.v1",
            "assignment_id": "assignment-fence-replay",
            "task_summary": "Reuse a crash-retained worker start fence",
            "task": {},
            "launch": {
                "agent": "codex", "cwd": checkout, "title": null,
                "session_id": "worker-fence-replay",
                "coordination_mode": "enforce", "agent_args": []
            },
            "repository": "example/repository", "worktree": checkout, "base_ref": "main",
            "scopes": ["crates/agent-session"], "durable_refs": []
        }),
    );
    let barrier = tmp.path().join("fence-replay-barrier");
    fs::create_dir(&barrier).expect("barrier");
    let contender_ready = tmp.path().join("fence-contender-ready");
    let make_command = |pause: bool| {
        let mut command = Command::new(bin::resolve("main-agent"));
        command
            .current_dir(&checkout)
            .args([
                "--state-dir",
                state_dir.to_str().expect("state dir"),
                "worker",
                "start",
                "--assignment-file",
                assignment_path.to_str().expect("assignment path"),
                "--await-ready",
                "0",
                "--idempotency-key",
                "worker-start-fence-replay-0001",
                "--format",
                "json",
            ])
            .env("AGENT_SESSION_CAPABILITY_FILE", &main_capability)
            .env("AGENT_SESSION_TMUX_BIN", &tmux_bin)
            .env("AGENT_SESSION_CODEX_BIN", &codex_bin)
            .env("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log)
            .env("CODEX_HOME", &codex_home)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if pause {
            command
                .env(
                    "NILS_AGENT_SESSION_TEST_BATCH_LANE_BARRIER_STAGE",
                    "before_worker_attachment",
                )
                .env("NILS_AGENT_SESSION_TEST_BATCH_LANE_BARRIER_DIR", &barrier);
        } else {
            command.env(
                "NILS_AGENT_SESSION_TEST_FENCE_CONTENDER_READY",
                &contender_ready,
            );
        }
        command
    };
    let mut interrupted = make_command(true).spawn().expect("spawn interrupted start");
    let barrier_deadline = Instant::now() + Duration::from_secs(10);
    while !barrier.join("ready").is_file() {
        assert!(
            Instant::now() < barrier_deadline,
            "interrupted start never reached durable pre-attachment state"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let retained_registry = load_coordination_registry(&state_dir);
    let retained_fences = retained_registry["operations"]
        .as_array()
        .expect("operations")
        .iter()
        .filter(|operation| operation["operation"] == "main-agent-worker-start")
        .collect::<Vec<_>>();
    assert_eq!(retained_fences.len(), 1);
    let retained_lease_id = retained_fences[0]["lease_id"]
        .as_str()
        .expect("fence lease id")
        .to_string();
    let retained_token_digest = retained_fences[0]["execution_token_digest"]
        .as_str()
        .expect("fence token digest")
        .to_string();
    let mut joined = make_command(false).spawn().expect("spawn exact join");
    let contender_deadline = Instant::now() + Duration::from_secs(10);
    while !contender_ready.is_file() {
        assert!(
            Instant::now() < contender_deadline,
            "exact replay never observed the live fence owner"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        joined.try_wait().expect("poll exact join").is_none(),
        "a live exact replay must wait for the current fence owner"
    );
    let live_registry = load_coordination_registry(&state_dir);
    let live_fence = live_registry["operations"]
        .as_array()
        .expect("operations")
        .iter()
        .find(|operation| operation["lease_id"] == retained_lease_id)
        .expect("live worker start fence");
    assert_eq!(
        live_fence["execution_token_digest"], retained_token_digest,
        "a live exact replay must not rotate the current owner's fence token"
    );
    let state_arg = state_dir.to_string_lossy().into_owned();
    let release_while_unattached = run_with_env(
        &checkout,
        &[
            "--state-dir",
            &state_arg,
            "work-context",
            "release",
            "--session",
            "main-one",
            "--claim",
            &claim_id,
            "--if-revision",
            &claim_revision,
            "--idempotency-key",
            "release-controller-before-replay-attachment-0001",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_CAPABILITY_FILE", &main_capability)],
    );
    assert_ne!(release_while_unattached.code, 0);
    assert_eq!(
        release_while_unattached.stdout_json()["error"]["code"],
        "operation-in-progress"
    );
    interrupted.kill().expect("interrupt worker start fixture");
    let interrupted = interrupted.wait().expect("wait interrupted start");
    assert!(!interrupted.success());
    assert!(
        state_dir.join("sessions/worker-fence-replay").exists(),
        "the fixture interruption occurs after durable session creation"
    );

    let replay = joined.wait_with_output().expect("joined worker start");
    assert!(
        replay.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&replay.stdout),
        String::from_utf8_lossy(&replay.stderr)
    );
    let completed_registry = load_coordination_registry(&state_dir);
    let completed_fences = completed_registry["operations"]
        .as_array()
        .expect("operations")
        .iter()
        .filter(|operation| operation["operation"] == "main-agent-worker-start")
        .collect::<Vec<_>>();
    assert_eq!(
        completed_fences.len(),
        1,
        "exact replay must reuse rather than stack the crash-retained fence"
    );
    assert_eq!(completed_fences[0]["lease_id"], retained_lease_id);
    assert_eq!(completed_fences[0]["state"], "completed");
    let receipt_key = "main-one:main-incarnation-one:worker-start-fence-replay-0001".to_string();
    rewrite_orchestration_registry(&state_dir, |registry| {
        let terminal = registry["receipts"][&receipt_key]["outcome"].clone();
        registry["receipts"][&receipt_key]["outcome"] = json!({
            "schema_version": "main-agent.worker-start-readiness-progress.v1",
            "state": "awaiting_readiness",
            "deadline_at_epoch": 1,
            "finalizer_id": "fixture-retained-fence-finalizer",
            "finalizer_lease_until_epoch": 1,
            "submit_key_recovery_eligible": false,
            "recovery_continuation": null,
            "outcome": terminal
        });
    });
    rewrite_registry(&state_dir, |registry| {
        let fence = registry["operations"]
            .as_array_mut()
            .expect("operations")
            .iter_mut()
            .find(|operation| operation["lease_id"] == retained_lease_id)
            .expect("worker start fence");
        fence["state"] = json!("active");
        fence["terminal_at_epoch"] = serde_json::Value::Null;
        fence["outcome"] = serde_json::Value::Null;
    });
    let terminal_replay = make_command(false)
        .output()
        .expect("readiness receipt fence cleanup replay");
    assert!(
        terminal_replay.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&terminal_replay.stdout),
        String::from_utf8_lossy(&terminal_replay.stderr)
    );
    let cleaned_registry = load_coordination_registry(&state_dir);
    let cleaned_fence = cleaned_registry["operations"]
        .as_array()
        .expect("operations")
        .iter()
        .find(|operation| operation["lease_id"] == retained_lease_id)
        .expect("cleaned worker start fence");
    assert_eq!(
        cleaned_fence["state"], "completed",
        "nested readiness receipt replay must finish a fence retained after attachment commit"
    );
    rewrite_registry(&state_dir, |registry| {
        let fence = registry["operations"]
            .as_array_mut()
            .expect("operations")
            .iter_mut()
            .find(|operation| operation["lease_id"] == retained_lease_id)
            .expect("worker start fence");
        fence["state"] = json!("active");
        fence["terminal_at_epoch"] = serde_json::Value::Null;
        fence["outcome"] = serde_json::Value::Null;
    });
    let final_replay = make_command(false)
        .output()
        .expect("terminal receipt fence cleanup replay");
    assert!(
        final_replay.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&final_replay.stdout),
        String::from_utf8_lossy(&final_replay.stderr)
    );
    let final_registry = load_coordination_registry(&state_dir);
    let final_fence = final_registry["operations"]
        .as_array()
        .expect("operations")
        .iter()
        .find(|operation| operation["lease_id"] == retained_lease_id)
        .expect("final worker start fence");
    assert_eq!(final_fence["state"], "completed");
    assert_eq!(
        tmux_calls(&tmux_log)
            .iter()
            .filter(|call| call.first().is_some_and(|arg| arg == "new-session"))
            .count(),
        1
    );
    let release_after_replay = run_with_env(
        &checkout,
        &[
            "--state-dir",
            &state_arg,
            "work-context",
            "release",
            "--session",
            "main-one",
            "--claim",
            &claim_id,
            "--if-revision",
            &claim_revision,
            "--idempotency-key",
            "release-controller-after-replay-attachment-0001",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_CAPABILITY_FILE", &main_capability)],
    );
    assert_eq!(
        release_after_replay.code,
        0,
        "stderr={}",
        release_after_replay.stderr_text()
    );
}

#[test]
fn main_agent_worker_start_distinct_null_id_requests_hold_distinct_authority_fences() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let checkout = tmp.path().join("checkout");
    fs::create_dir(&state_dir).expect("state");
    init_checkout(&checkout, "https://example.invalid/example/repository.git");
    seed_brokers_at(
        &state_dir,
        &[(
            "main-one",
            "main-incarnation-one",
            "main-private-capability-material-0000000001",
            checkout.as_path(),
            Some("enforce"),
        )],
    );
    let main_capability = init_main_run(tmp.path(), &state_dir, &checkout, "main-one", "run-one");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let codex_bin = fake_agent(tmp.path(), "codex-worker");
    let codex_home = tmp.path().join("codex-home");
    write_trusted_codex_config(&codex_home, &[&checkout]);
    let assignment_path = tmp.path().join("assignment-null-ids.json");
    write_private_json(
        &assignment_path,
        &json!({
            "schema_version": "main-agent.assignment-input.v1",
            "assignment_id": null,
            "task_summary": "Launch distinct workers from the same null-id packet",
            "task": {},
            "launch": {
                "agent": "codex", "cwd": checkout, "title": null,
                "session_id": null, "coordination_mode": "enforce", "agent_args": []
            },
            "repository": "example/repository", "worktree": checkout, "base_ref": "main",
            "scopes": ["crates/agent-session"], "durable_refs": []
        }),
    );
    let first_barrier = tmp.path().join("null-id-first-barrier");
    let second_barrier = tmp.path().join("null-id-second-barrier");
    fs::create_dir(&first_barrier).expect("first barrier");
    fs::create_dir(&second_barrier).expect("second barrier");
    let spawn = |idempotency_key: &str, barrier: &Path| {
        let mut command = Command::new(bin::resolve("main-agent"));
        command
            .current_dir(&checkout)
            .args([
                "--state-dir",
                state_dir.to_str().expect("state dir"),
                "worker",
                "start",
                "--assignment-file",
                assignment_path.to_str().expect("assignment path"),
                "--await-ready",
                "0",
                "--idempotency-key",
                idempotency_key,
                "--format",
                "json",
            ])
            .env("AGENT_SESSION_CAPABILITY_FILE", &main_capability)
            .env("AGENT_SESSION_TMUX_BIN", &tmux_bin)
            .env("AGENT_SESSION_CODEX_BIN", &codex_bin)
            .env("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log)
            .env("CODEX_HOME", &codex_home)
            .env(
                "NILS_AGENT_SESSION_TEST_BATCH_LANE_BARRIER_STAGE",
                "before_session_create",
            )
            .env("NILS_AGENT_SESSION_TEST_BATCH_LANE_BARRIER_DIR", barrier)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn worker start")
    };
    let first = spawn("worker-start-null-ids-0001", &first_barrier);
    let second = spawn("worker-start-null-ids-0002", &second_barrier);
    let overlap_deadline = Instant::now() + Duration::from_secs(10);
    while !first_barrier.join("ready").is_file() || !second_barrier.join("ready").is_file() {
        assert!(
            Instant::now() < overlap_deadline,
            "both null-id contenders did not reach the pre-create boundary concurrently"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    fs::write(first_barrier.join("release"), b"release").expect("release first contender");
    fs::write(second_barrier.join("release"), b"release").expect("release second contender");
    let first = first.wait_with_output().expect("first worker start");
    let second = second.wait_with_output().expect("second worker start");
    for output in [&first, &second] {
        assert!(
            output.status.success(),
            "stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let first: serde_json::Value = serde_json::from_slice(&first.stdout).expect("first json");
    let second: serde_json::Value = serde_json::from_slice(&second.stdout).expect("second json");
    assert_ne!(
        first["data"]["assignment"]["assignment_id"],
        second["data"]["assignment"]["assignment_id"]
    );
    assert_ne!(
        first["data"]["worker"]["session_id"],
        second["data"]["worker"]["session_id"]
    );
    let coordination = load_coordination_registry(&state_dir);
    let fences = coordination["operations"]
        .as_array()
        .expect("operations")
        .iter()
        .filter(|operation| operation["operation"] == "main-agent-worker-start")
        .collect::<Vec<_>>();
    assert_eq!(fences.len(), 2);
    assert_ne!(fences[0]["lease_id"], fences[1]["lease_id"]);
    assert!(fences.iter().all(|fence| fence["state"] == "completed"));
    assert_eq!(
        tmux_calls(&tmux_log)
            .iter()
            .filter(|call| call.first().is_some_and(|arg| arg == "new-session"))
            .count(),
        2
    );
}

#[test]
fn main_agent_worker_start_batch_converges_concurrently_from_historical_lane_receipts() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let released_fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../fixtures/orchestration/released-v2-lane-authority.json"
    ))
    .expect("released-v2 lane fixture");
    assert_eq!(
        released_fixture["source_commit"],
        "89fae89403782b7caec965c614dd1516d903a1e0"
    );
    let state_dir = tmp.path().join("state");
    let checkout = tmp.path().join("checkout");
    fs::create_dir(&state_dir).expect("state");
    init_checkout(&checkout, "https://example.invalid/example/repository.git");
    seed_brokers_at(
        &state_dir,
        &[(
            "main-one",
            "main-incarnation-one",
            "main-private-capability-material-0000000001",
            checkout.as_path(),
            Some("enforce"),
        )],
    );
    let main_capability = init_main_run(tmp.path(), &state_dir, &checkout, "main-one", "run-one");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let codex_bin = fake_agent(tmp.path(), "codex-worker");
    let batch_dir = tmp.path().join("historical-batch");
    fs::create_dir(&batch_dir).expect("batch dir");
    let completed_input = json!({
        "schema_version": "main-agent.assignment-input.v1",
        "assignment_id": "assignment-historical-completed",
        "task_summary": "Replay a completed historical lane",
        "task": {},
        "launch": {
            "agent": "codex", "cwd": checkout, "title": null,
            "session_id": "worker-historical-completed",
            "coordination_mode": "enforce", "agent_args": []
        },
        "repository": "example/repository", "worktree": null, "base_ref": "main",
        "scopes": ["crates/agent-session"], "durable_refs": []
    });
    let pending_input = json!({
        "schema_version": "main-agent.assignment-input.v1",
        "assignment_id": "assignment-historical-pending",
        "task_summary": "Resume a pending historical lane",
        "task": {},
        "launch": {
            "agent": "codex", "cwd": checkout, "title": null,
            "session_id": "worker-historical-pending",
            "coordination_mode": "enforce", "agent_args": []
        },
        "repository": "example/repository", "worktree": null, "base_ref": "main",
        "scopes": ["crates/agent-session"], "durable_refs": []
    });
    write_private_json(&batch_dir.join("lane-completed.json"), &completed_input);
    write_private_json(&batch_dir.join("lane-pending.json"), &pending_input);
    insert_orchestration_assignment(
        &state_dir,
        "assignment-historical-pending",
        json!({
            "schema_version": "agent-session.orchestration-assignment.v1",
            "assignment_id": "assignment-historical-pending",
            "run_id": "run-one",
            "revision": 1,
            "state": "starting",
            "task_summary": "Resume a pending historical lane",
            "private_packet_digest": "replaced-by-fixture",
            "primary_manager": {
                "session_id": "main-one",
                "session_incarnation": "main-incarnation-one",
                "session_created_at": "2030-01-01T00:00:00Z"
            },
            "worker": null,
            "collaborators": [],
            "borrowed_by": [],
            "repository": "example/repository",
            "worktree": null,
            "base_ref": "main",
            "scopes": ["crates/agent-session"],
            "durable_refs": [],
            "checkpoint": null,
            "result_summary": null,
            "blocker_summary": null,
            "created_at": "2030-01-01T00:00:01Z",
            "updated_at": "2030-01-01T00:00:01Z"
        }),
        &pending_input,
    );
    let parent_key = released_fixture["batch"]["parent_key"]
        .as_str()
        .expect("released-v2 batch parent key");
    assert_eq!(
        format!("{parent_key}-0"),
        released_fixture["batch"]["child_key"]
            .as_str()
            .expect("released-v2 first child key")
    );
    rewrite_orchestration_registry(&state_dir, |registry| {
        registry["receipts"][format!("main-one:main-incarnation-one:{parent_key}-0")] = json!({
            "principal_session_id": "main-one",
            "principal_incarnation": "main-incarnation-one",
            "operation": "worker-start",
            "request_digest": assignment_request_digest(&completed_input),
            "outcome": {
                "schema_version": "main-agent.worker-start-result.v1",
                "state": "started",
                "assignment": {
                    "assignment_id": "assignment-historical-completed"
                },
                "worker": {
                    "session_id": "worker-historical-completed"
                },
                "fresh_launch": false
            },
            "created_at_epoch": 1
        });
        registry["receipts"][format!("main-one:main-incarnation-one:{parent_key}-1")] = json!({
            "principal_session_id": "main-one",
            "principal_incarnation": "main-incarnation-one",
            "operation": "worker-start",
            "request_digest": assignment_request_digest(&pending_input),
            "outcome": {
                "schema_version": "main-agent.worker-start-result.v1",
                "assignment_id": "assignment-historical-pending",
                "worker_session_id": "worker-historical-pending",
                "state": "starting",
                "acceptance": "pending"
            },
            "created_at_epoch": 1
        });
    });
    let codex_home = tmp.path().join("codex-home");
    fs::create_dir(&codex_home).expect("Codex home");
    fs::write(codex_home.join("config.toml"), "[projects.").expect("malformed Codex config");
    let barrier = tmp.path().join("historical-batch-barrier");
    fs::create_dir(&barrier).expect("barrier");
    let spawn_batch = |pause: bool| {
        let mut command = Command::new(bin::resolve("main-agent"));
        command
            .current_dir(&checkout)
            .args([
                "--state-dir",
                state_dir.to_str().expect("state dir"),
                "worker",
                "start",
                "--batch",
                batch_dir.to_str().expect("batch dir"),
                "--idempotency-key",
                parent_key,
                "--format",
                "json",
            ])
            .env("AGENT_SESSION_CAPABILITY_FILE", &main_capability)
            .env("AGENT_SESSION_TMUX_BIN", &tmux_bin)
            .env("AGENT_SESSION_CODEX_BIN", &codex_bin)
            .env("CODEX_HOME", &codex_home)
            .env("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if pause {
            command
                .env(
                    "NILS_AGENT_SESSION_TEST_BATCH_LANE_BARRIER_STAGE",
                    "before_session_create",
                )
                .env("NILS_AGENT_SESSION_TEST_BATCH_LANE_BARRIER_DIR", &barrier);
        }
        command.spawn().expect("spawn batch")
    };
    let blocked = spawn_batch(false)
        .wait_with_output()
        .expect("blocked historical batch");
    assert!(
        blocked.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&blocked.stdout),
        String::from_utf8_lossy(&blocked.stderr)
    );
    let blocked: serde_json::Value =
        serde_json::from_slice(&blocked.stdout).expect("blocked historical batch json");
    assert_eq!(
        blocked["data"]["lanes"][1]["error"]["code"],
        "provider-trust-unverified"
    );
    assert_eq!(blocked["data"]["lanes"][1]["resumable"], true);
    assert!(
        tmux_calls(&tmux_log).is_empty(),
        "historical trust preflight failure must not duplicate the completed lane or launch the pending lane"
    );

    write_trusted_codex_config(&codex_home, &[&checkout]);
    let first = spawn_batch(true);
    let barrier_deadline = Instant::now() + Duration::from_secs(10);
    while !barrier.join("ready").is_file() {
        assert!(
            Instant::now() < barrier_deadline,
            "historical lane owner never reached the child side-effect boundary"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let receipt_key = format!("main-one:main-incarnation-one:{parent_key}");
    let owner_before = orchestration_registry(&state_dir)["receipts"][&receipt_key]["outcome"]
        ["lanes"][1]["owner_id"]
        .as_str()
        .expect("historical pending lane owner")
        .to_string();

    let mut second = spawn_batch(false);
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        second.try_wait().expect("poll second batch").is_none(),
        "the concurrent replay must wait on the live durable lane owner"
    );
    assert_eq!(
        orchestration_registry(&state_dir)["receipts"][&receipt_key]["outcome"]["lanes"][1]["owner_id"],
        owner_before,
        "the live durable lane owner must not be replaced"
    );
    fs::write(barrier.join("release"), b"release").expect("release historical lane owner");
    let first = first.wait_with_output().expect("first batch");
    let second = second.wait_with_output().expect("second batch");
    for output in [&first, &second] {
        assert!(
            output.status.success(),
            "stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let first: serde_json::Value = serde_json::from_slice(&first.stdout).expect("first json");
    let second: serde_json::Value = serde_json::from_slice(&second.stdout).expect("second json");
    assert_eq!(first["data"], second["data"]);
    assert!(
        first["data"]["lanes"]
            .as_array()
            .is_some_and(|lanes| lanes.iter().all(|lane| lane["ok"] == true))
    );
    assert_eq!(
        tmux_calls(&tmux_log)
            .iter()
            .filter(|call| call.first().is_some_and(|arg| arg == "new-session"))
            .count(),
        1,
        "the completed historical lane replays and the pending lane launches once"
    );

    fs::write(
        batch_dir.join("lane-completed.json"),
        serde_json::to_vec_pretty(&completed_input)
            .expect("completed bytes")
            .into_iter()
            .chain(b"\n ".iter().copied())
            .collect::<Vec<_>>(),
    )
    .expect("edit completed lane bytes");
    let drift = run_main_agent(
        &checkout,
        &[
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "worker",
            "start",
            "--batch",
            batch_dir.to_str().expect("batch dir"),
            "--idempotency-key",
            parent_key,
            "--format",
            "json",
        ],
        &[
            ("AGENT_SESSION_CAPABILITY_FILE", &main_capability),
            (
                "AGENT_SESSION_TMUX_BIN",
                tmux_bin.to_str().expect("tmux bin"),
            ),
            (
                "AGENT_SESSION_CODEX_BIN",
                codex_bin.to_str().expect("codex bin"),
            ),
            (
                "AGENT_SESSION_FAKE_TMUX_LOG",
                tmux_log.to_str().expect("tmux log"),
            ),
        ],
    );
    assert_eq!(drift.code, 65, "outcome={}", drift.stdout_text());
    assert_eq!(drift.stdout_json()["error"]["code"], "idempotency-conflict");
}

#[test]
fn main_agent_worker_start_await_ready_folds_timeout_into_readiness_failed() {
    // T1 readiness fold (Part C acceptance): a bounded `--await-ready` that never
    // observes the worker's checkpoint advance past `starting` must return a typed
    // `readiness_failed` carrying the last assignment state and an actionable
    // `safe_state`, not a bare error. The fake worker never checkpoints, so the
    // bound always elapses — this deterministically exercises the timeout branch
    // and verifies the diagnostic payload (Part F / F5).
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let checkout = tmp.path().join("checkout");
    fs::create_dir(&state_dir).expect("state");
    init_checkout(&checkout, "https://example.invalid/example/repository.git");
    seed_brokers_at(
        &state_dir,
        &[(
            "main-one",
            "main-incarnation-one",
            "main-private-capability-material-0000000001",
            checkout.as_path(),
            Some("enforce"),
        )],
    );
    let main_capability = init_main_run(tmp.path(), &state_dir, &checkout, "main-one", "run-one");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let codex_bin = fake_agent(tmp.path(), "codex-worker");

    let assignment_path = tmp.path().join("assignment-await-ready.json");
    write_private_json(
        &assignment_path,
        &json!({
            "schema_version": "main-agent.assignment-input.v1",
            "assignment_id": "assignment-await-ready",
            "task_summary": "Exercise the await-ready readiness fold",
            "task": {},
            "launch": {
                "agent": "codex",
                "cwd": checkout,
                "title": null,
                "session_id": "worker-await-ready",
                "coordination_mode": "enforce",
                "agent_args": []
            },
            "repository": "example/repository",
            "worktree": null,
            "base_ref": "main",
            "scopes": ["crates/agent-session"],
            "durable_refs": []
        }),
    );
    let codex_home = tmp.path().join("codex-home");
    write_trusted_codex_config(&codex_home, &[&checkout]);
    let tmux_arg = tmux_bin.to_string_lossy().into_owned();
    let tmux_log_arg = tmux_log.to_string_lossy().into_owned();
    let codex_arg = codex_bin.to_string_lossy().into_owned();
    let explicit_hook = tmp.path().join("explicit-recovery-contender");
    let explicit_hook_output = tmp.path().join("explicit-recovery-contender.json");
    fs::write(
        &explicit_hook,
        format!(
            "#!/usr/bin/env sh\nset -eu\nAGENT_SESSION_CAPABILITY_FILE='{capability}' '{main_agent}' --state-dir '{state_dir}' worker submit-recovery assignment-await-ready --if-revision 2 --timeout 1s --idempotency-key explicit-race-automatic-0001 --format json > '{output}'\n",
            capability = main_capability,
            main_agent = bin::resolve("main-agent").display(),
            state_dir = state_dir.display(),
            output = explicit_hook_output.display(),
        ),
    )
    .expect("explicit recovery hook");
    fs::set_permissions(&explicit_hook, fs::Permissions::from_mode(0o700))
        .expect("explicit recovery hook mode");
    let explicit_hook_arg = explicit_hook.to_string_lossy().into_owned();
    let enter_count_file = tmp.path().join("automatic-explicit-enter-count");
    let enter_count_arg = enter_count_file.to_string_lossy().into_owned();
    let started = run_main_agent_with_codex_trust(
        &checkout,
        &[
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "worker",
            "start",
            "--assignment-file",
            assignment_path.to_str().expect("assignment path"),
            "--if-run-revision",
            "1",
            "--await-ready",
            "12s",
            "--idempotency-key",
            "worker-start-await-ready-0001",
            "--format",
            "json",
        ],
        &[
            ("AGENT_SESSION_CAPABILITY_FILE", &main_capability),
            ("AGENT_SESSION_TMUX_BIN", &tmux_arg),
            ("AGENT_SESSION_CODEX_BIN", &codex_arg),
            ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
            ("AGENT_SESSION_FAKE_TMUX_ENTER_HOOK", &explicit_hook_arg),
            ("AGENT_SESSION_FAKE_TMUX_ENTER_COUNT_FILE", &enter_count_arg),
            ("AGENT_SESSION_FAKE_TMUX_ENTER_HOOK_AT", "2"),
        ],
        &codex_home,
        &[&checkout],
    );
    assert_eq!(started.code, 0, "stderr={}", started.stderr_text());
    let readiness = data(&started)["readiness"].clone();
    assert_eq!(readiness["state"], "readiness_failed");
    assert_eq!(readiness["assignment_state"], "starting");
    assert_eq!(readiness["worker_launched"], true);
    assert_eq!(readiness["delivery"]["state"], "unverified");
    assert_eq!(
        readiness["delivery"]["transport_state"],
        "submit-key-recovery-succeeded"
    );
    assert_eq!(readiness["delivery"]["proof"], "worker-checkpoint-timeout");
    assert_eq!(readiness["submit_key_recovery"]["eligible"], true);
    assert_eq!(readiness["submit_key_recovery"]["attempted"], true);
    assert_eq!(readiness["submit_key_recovery"]["attempt_count"], 1);
    assert_eq!(
        readiness["submit_key_recovery"]["result"],
        "checkpoint-timeout"
    );
    assert_eq!(readiness["automatic_retry_safe"], false);
    assert!(
        readiness["safe_state"]
            .as_str()
            .unwrap_or_default()
            .contains("single-Enter recovery is exhausted"),
        "readiness_failed safe_state should bound recovery: {}",
        readiness["safe_state"]
    );
    assert!(
        !readiness["safe_state"]
            .as_str()
            .unwrap_or_default()
            .contains("worker retire"),
        "starting is not a retireable assignment state: {}",
        readiness["safe_state"]
    );
    let enter_calls = tmux_calls(&tmux_log)
        .into_iter()
        .filter(|call| {
            call.first().is_some_and(|arg| arg == "send-keys")
                && call.last().is_some_and(|arg| arg == "Enter")
        })
        .count();
    assert_eq!(
        enter_calls, 2,
        "initial submission plus exactly one recovery Enter"
    );
    let hook_deadline = Instant::now() + Duration::from_secs(10);
    while fs::metadata(&explicit_hook_output).map_or(true, |metadata| metadata.len() == 0)
        && Instant::now() < hook_deadline
    {
        std::thread::sleep(Duration::from_millis(10));
    }
    let explicit_contender: serde_json::Value = serde_json::from_slice(
        &fs::read(&explicit_hook_output).expect("explicit recovery contender output"),
    )
    .expect("explicit recovery contender json");
    assert_eq!(explicit_contender["ok"], true);
    assert!(
        matches!(
            explicit_contender["data"]["result"].as_str(),
            Some("submit-recovery-send-outcome-unknown" | "checkpoint-timeout")
        ),
        "the observer may see the sender before or after its durable sent transition: {explicit_contender}"
    );
    let start_replay = run_main_agent(
        &checkout,
        &[
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "worker",
            "start",
            "--assignment-file",
            assignment_path.to_str().expect("assignment path"),
            "--if-run-revision",
            "1",
            "--await-ready",
            "12s",
            "--idempotency-key",
            "worker-start-await-ready-0001",
            "--format",
            "json",
        ],
        &[
            ("AGENT_SESSION_CAPABILITY_FILE", &main_capability),
            ("AGENT_SESSION_TMUX_BIN", &tmux_arg),
            ("AGENT_SESSION_CODEX_BIN", &codex_arg),
            ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
        ],
    );
    assert_eq!(
        start_replay.code,
        0,
        "stderr={}",
        start_replay.stderr_text()
    );
    assert_eq!(
        data(&start_replay),
        data(&started),
        "an exact start retry must replay the durable readiness decision"
    );
    let prompt_loads = tmux_calls(&tmux_log)
        .into_iter()
        .filter(|call| call.first().is_some_and(|arg| arg == "load-buffer"))
        .count();
    let prompt_pastes = tmux_calls(&tmux_log)
        .into_iter()
        .filter(|call| call.first().is_some_and(|arg| arg == "paste-buffer"))
        .count();
    assert_eq!(
        prompt_loads, 1,
        "startup loads the prompt once and recovery must not add another"
    );
    assert_eq!(
        prompt_pastes, 1,
        "startup pastes the prompt once and recovery must not add another"
    );

    let registry: serde_json::Value = serde_json::from_slice(
        &fs::read(state_dir.join("orchestration/registry.json")).expect("registry"),
    )
    .expect("registry json");
    let assignment = &registry["assignments"]["assignment-await-ready"];
    assert_eq!(assignment["submit_recovery"]["attempt_count"], 1);
    assert_eq!(assignment["submit_recovery"]["state"], "failed");
    assert_eq!(
        assignment["submit_recovery"]["result"],
        "checkpoint-timeout"
    );
    assert_eq!(
        assignment["submit_recovery"]["session_incarnation"],
        data(&started)["worker"]["session_incarnation"]
    );
    assert_eq!(
        assignment["readiness_stop_proof"],
        json!({
            "schema_version": "main-agent.worker-readiness-stop-proof.v1",
            "worker": assignment["worker"],
            "readiness_state": "readiness_failed",
            "delivery_proof": "worker-checkpoint-timeout",
            "automatic_retry_safe": false,
            "recorded_at": assignment["readiness_stop_proof"]["recorded_at"]
        }),
        "the real readiness finalizer must persist the direct stop proof"
    );
    let revision = assignment["revision"]
        .as_u64()
        .expect("assignment revision");
    let revision_arg = revision.to_string();
    rewrite_orchestration_registry(&state_dir, |registry| {
        registry["receipts"]
            .as_object_mut()
            .expect("receipts")
            .remove("main-one:main-incarnation-one:worker-start-await-ready-0001");
    });
    let supervised_after_receipt_eviction = run_main_agent(
        &checkout,
        &[
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "worker",
            "supervise",
            "assignment-await-ready",
            "--format",
            "json",
        ],
        &[
            ("AGENT_SESSION_CAPABILITY_FILE", &main_capability),
            ("AGENT_SESSION_TMUX_BIN", &tmux_arg),
            ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
        ],
    );
    assert_eq!(
        supervised_after_receipt_eviction.code,
        0,
        "outcome={}",
        supervised_after_receipt_eviction.stdout_text()
    );
    assert_eq!(
        data(&supervised_after_receipt_eviction)["classification"],
        "readiness_stop_required",
        "the direct assignment proof must authorize stop supervision after receipt eviction"
    );
    let explicit = run_main_agent(
        &checkout,
        &[
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "worker",
            "submit-recovery",
            "assignment-await-ready",
            "--if-revision",
            &revision_arg,
            "--timeout",
            "1s",
            "--idempotency-key",
            "worker-explicit-after-automatic-0001",
            "--format",
            "json",
        ],
        &[
            ("AGENT_SESSION_CAPABILITY_FILE", &main_capability),
            ("AGENT_SESSION_TMUX_BIN", &tmux_arg),
            ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
        ],
    );
    assert_eq!(explicit.code, 0, "stderr={}", explicit.stderr_text());
    assert_eq!(data(&explicit)["checkpoint_confirmed"], false);
    assert_eq!(
        data(&explicit)["result"],
        assignment["submit_recovery"]["result"]
    );
    let explicit_replay = run_main_agent(
        &checkout,
        &[
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "worker",
            "submit-recovery",
            "assignment-await-ready",
            "--if-revision",
            &revision_arg,
            "--timeout",
            "1s",
            "--idempotency-key",
            "worker-explicit-after-automatic-0001",
            "--format",
            "json",
        ],
        &[
            ("AGENT_SESSION_CAPABILITY_FILE", &main_capability),
            ("AGENT_SESSION_TMUX_BIN", &tmux_arg),
            ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
        ],
    );
    assert_eq!(data(&explicit_replay), data(&explicit));
    let enter_calls_after_explicit = tmux_calls(&tmux_log)
        .into_iter()
        .filter(|call| {
            call.first().is_some_and(|arg| arg == "send-keys")
                && call.last().is_some_and(|arg| arg == "Enter")
        })
        .count();
    assert_eq!(
        enter_calls_after_explicit, 2,
        "automatic and explicit recovery share one durable attempt"
    );
}

#[test]
fn main_agent_worker_start_concurrent_replays_join_one_readiness_finalizer() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let checkout = tmp.path().join("checkout");
    fs::create_dir(&state_dir).expect("state");
    init_checkout(&checkout, "https://example.invalid/example/repository.git");
    seed_brokers_at(
        &state_dir,
        &[(
            "main-one",
            "main-incarnation-one",
            "main-private-capability-material-0000000001",
            checkout.as_path(),
            Some("enforce"),
        )],
    );
    let main_capability = init_main_run(tmp.path(), &state_dir, &checkout, "main-one", "run-one");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let codex_bin = fake_agent(tmp.path(), "codex-worker");
    let assignment_path = tmp.path().join("assignment-concurrent-readiness.json");
    write_private_json(
        &assignment_path,
        &json!({
            "schema_version": "main-agent.assignment-input.v1",
            "assignment_id": "assignment-concurrent-readiness",
            "task_summary": "Join one durable readiness finalizer",
            "task": {},
            "launch": {
                "agent": "codex",
                "cwd": checkout,
                "title": null,
                "session_id": "worker-concurrent-readiness",
                "coordination_mode": "enforce",
                "agent_args": []
            },
            "repository": "example/repository",
            "worktree": null,
            "base_ref": "main",
            "scopes": ["crates/agent-session"],
            "durable_refs": []
        }),
    );
    let codex_home = tmp.path().join("codex-home");
    write_trusted_codex_config(&codex_home, &[&checkout]);
    let state_arg = state_dir.to_string_lossy().into_owned();
    let assignment_arg = assignment_path.to_string_lossy().into_owned();
    let tmux_arg = tmux_bin.to_string_lossy().into_owned();
    let tmux_log_arg = tmux_log.to_string_lossy().into_owned();
    let codex_arg = codex_bin.to_string_lossy().into_owned();
    let spawn_start = || {
        Command::new(bin::resolve("main-agent"))
            .current_dir(&checkout)
            .args([
                "--state-dir",
                state_arg.as_str(),
                "worker",
                "start",
                "--assignment-file",
                assignment_arg.as_str(),
                "--await-ready",
                "3s",
                "--idempotency-key",
                "worker-start-concurrent-readiness-0001",
                "--format",
                "json",
            ])
            .env("AGENT_SESSION_CAPABILITY_FILE", &main_capability)
            .env("AGENT_SESSION_TMUX_BIN", &tmux_arg)
            .env("AGENT_SESSION_CODEX_BIN", &codex_arg)
            .env("CODEX_HOME", &codex_home)
            .env("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn worker start")
    };
    let owner = spawn_start();
    let progress_deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let registry = orchestration_registry(&state_dir);
        let pending = registry["receipts"].as_object().is_some_and(|receipts| {
            receipts.values().any(|receipt| {
                receipt["outcome"]["schema_version"]
                    == "main-agent.worker-start-readiness-progress.v1"
            })
        });
        if pending {
            break;
        }
        assert!(
            Instant::now() < progress_deadline,
            "worker start never persisted readiness progress"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let observer = spawn_start();
    let owner_output = owner.wait_with_output().expect("owner output");
    let observer_output = observer.wait_with_output().expect("observer output");
    assert!(
        owner_output.status.success(),
        "owner stderr={}",
        String::from_utf8_lossy(&owner_output.stderr)
    );
    assert!(
        observer_output.status.success(),
        "observer stderr={}",
        String::from_utf8_lossy(&observer_output.stderr)
    );
    let owner_envelope: serde_json::Value =
        serde_json::from_slice(&owner_output.stdout).expect("owner json");
    let observer_envelope: serde_json::Value =
        serde_json::from_slice(&observer_output.stdout).expect("observer json");
    assert_eq!(
        owner_envelope["data"], observer_envelope["data"],
        "concurrent exact retries must return one committed readiness outcome"
    );
    assert_eq!(
        owner_envelope["data"]["readiness"]["state"],
        "readiness_failed"
    );
    let calls = tmux_calls(&tmux_log);
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.first().is_some_and(|arg| arg == "load-buffer"))
            .count(),
        1
    );
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.first().is_some_and(|arg| arg == "paste-buffer"))
            .count(),
        1
    );
    assert_eq!(
        calls
            .iter()
            .filter(|call| {
                call.first().is_some_and(|arg| arg == "send-keys")
                    && call.last().is_some_and(|arg| arg == "Enter")
            })
            .count(),
        2,
        "one prompt Enter plus one durable recovery Enter"
    );
}

#[test]
fn main_agent_worker_start_finalizer_takeover_resumes_the_same_recovery_attempt() {
    for crash_stage in ["before_reserve", "reserved", "sending", "sent"] {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let state_dir = tmp.path().join("state");
        let checkout = tmp.path().join("checkout");
        fs::create_dir(&state_dir).expect("state");
        init_checkout(&checkout, "https://example.invalid/example/repository.git");
        seed_brokers_at(
            &state_dir,
            &[(
                "main-one",
                "main-incarnation-one",
                "main-private-capability-material-0000000001",
                checkout.as_path(),
                Some("enforce"),
            )],
        );
        let main_capability =
            init_main_run(tmp.path(), &state_dir, &checkout, "main-one", "run-one");
        let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
        let codex_bin = fake_agent(tmp.path(), "codex-worker");
        let assignment_id = format!("assignment-readiness-takeover-{crash_stage}");
        let worker_id = format!("worker-readiness-takeover-{crash_stage}");
        let assignment_path = tmp.path().join(format!("{assignment_id}.json"));
        write_private_json(
            &assignment_path,
            &json!({
                "schema_version": "main-agent.assignment-input.v1",
                "assignment_id": assignment_id,
                "task_summary": "Resume one automatic recovery after finalizer takeover",
                "task": {},
                "launch": {
                    "agent": "codex",
                    "cwd": checkout,
                    "title": null,
                    "session_id": worker_id,
                    "coordination_mode": "enforce",
                    "agent_args": []
                },
                "repository": "example/repository",
                "worktree": null,
                "base_ref": "main",
                "scopes": ["crates/agent-session"],
                "durable_refs": []
            }),
        );
        let codex_home = tmp.path().join("codex-home");
        write_trusted_codex_config(&codex_home, &[&checkout]);
        let barrier = tmp.path().join(format!("readiness-{crash_stage}-barrier"));
        fs::create_dir(&barrier).expect("barrier");
        let takeover_barrier = tmp
            .path()
            .join(format!("readiness-{crash_stage}-takeover-barrier"));
        fs::create_dir(&takeover_barrier).expect("takeover barrier");
        let state_arg = state_dir.to_string_lossy().into_owned();
        let assignment_arg = assignment_path.to_string_lossy().into_owned();
        let tmux_arg = tmux_bin.to_string_lossy().into_owned();
        let tmux_log_arg = tmux_log.to_string_lossy().into_owned();
        let codex_arg = codex_bin.to_string_lossy().into_owned();
        let barrier_arg = barrier.to_string_lossy().into_owned();
        let takeover_barrier_arg = takeover_barrier.to_string_lossy().into_owned();
        let idempotency_key = format!("readiness-takeover-{crash_stage}-0001");
        let spawn_start = |pause: bool, pause_takeover: bool| {
            let mut command = Command::new(bin::resolve("main-agent"));
            command
                .current_dir(&checkout)
                .args([
                    "--state-dir",
                    state_arg.as_str(),
                    "worker",
                    "start",
                    "--assignment-file",
                    assignment_arg.as_str(),
                    "--await-ready",
                    "20s",
                    "--idempotency-key",
                    idempotency_key.as_str(),
                    "--format",
                    "json",
                ])
                .env("AGENT_SESSION_CAPABILITY_FILE", &main_capability)
                .env("AGENT_SESSION_TMUX_BIN", &tmux_arg)
                .env("AGENT_SESSION_CODEX_BIN", &codex_arg)
                .env("CODEX_HOME", &codex_home)
                .env("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg)
                .env(
                    "NILS_AGENT_SESSION_TEST_READINESS_FINALIZER_LEASE_SECS",
                    "7",
                )
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            if pause {
                command
                    .env(
                        "NILS_AGENT_SESSION_TEST_READINESS_RECOVERY_BARRIER_DIR",
                        &barrier_arg,
                    )
                    .env(
                        "NILS_AGENT_SESSION_TEST_READINESS_RECOVERY_BARRIER_STAGE",
                        crash_stage,
                    );
            }
            if pause_takeover {
                command.env(
                    "NILS_AGENT_SESSION_TEST_READINESS_TAKEOVER_BARRIER_DIR",
                    &takeover_barrier_arg,
                );
            }
            command.spawn().expect("spawn worker start")
        };

        let mut owner = spawn_start(true, false);
        let barrier_deadline = Instant::now() + Duration::from_secs(30);
        while !barrier.join("ready").is_file() {
            assert!(
                Instant::now() < barrier_deadline,
                "readiness finalizer never paused after {crash_stage}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let successor = if matches!(crash_stage, "before_reserve" | "sending") {
            let receipt_key = format!("main-one:main-incarnation-one:{idempotency_key}");
            let original_finalizer = orchestration_registry(&state_dir)["receipts"][&receipt_key]
                ["outcome"]["finalizer_id"]
                .as_str()
                .expect("original readiness finalizer")
                .to_string();
            let successor = spawn_start(false, true);
            let takeover_deadline = Instant::now() + Duration::from_secs(25);
            while !takeover_barrier.join("ready").is_file() {
                assert!(
                    Instant::now() < takeover_deadline,
                    "successor did not reach its post-takeover barrier at {crash_stage}"
                );
                std::thread::sleep(Duration::from_millis(25));
            }
            let successor_finalizer =
                fs::read_to_string(takeover_barrier.join("ready")).expect("successor finalizer id");
            {
                let registry = orchestration_registry(&state_dir);
                let current = registry["receipts"][&receipt_key]["outcome"]["finalizer_id"]
                    .as_str()
                    .map(str::to_string);
                assert_ne!(successor_finalizer, original_finalizer);
                assert_eq!(
                    current.as_deref(),
                    Some(successor_finalizer.as_str()),
                    "the successor-only barrier must observe its durable finalizer identity before the stale owner is released"
                );
            }
            fs::write(barrier.join("release"), b"release").expect("release stale finalizer");
            let stale_owner = owner.wait_with_output().expect("stale owner result");
            if crash_stage == "sending" {
                assert!(
                    !stale_owner.status.success(),
                    "a displaced sender must be fenced before Enter"
                );
                let stale: serde_json::Value =
                    serde_json::from_slice(&stale_owner.stdout).expect("stale owner json");
                assert_eq!(
                    stale["error"]["code"], "worker-start-finalizer-changed",
                    "the stale sending-stage owner must fail at the pre-send authority boundary"
                );
            } else {
                assert!(
                    stale_owner.status.success(),
                    "stale owner stdout={} stderr={}",
                    String::from_utf8_lossy(&stale_owner.stdout),
                    String::from_utf8_lossy(&stale_owner.stderr)
                );
            }
            fs::write(takeover_barrier.join("release"), b"release")
                .expect("release successor finalizer");
            successor
                .wait_with_output()
                .expect("successor worker start")
        } else {
            owner.kill().expect("terminate owning readiness finalizer");
            let _ = owner.wait().expect("reap owning readiness finalizer");
            spawn_start(false, false)
                .wait_with_output()
                .expect("successor worker start")
        };
        assert!(
            successor.status.success(),
            "stage={crash_stage} stdout={} stderr={}",
            String::from_utf8_lossy(&successor.stdout),
            String::from_utf8_lossy(&successor.stderr)
        );
        let successor: serde_json::Value =
            serde_json::from_slice(&successor.stdout).expect("successor json");
        assert_eq!(
            successor["data"]["readiness"]["submit_key_recovery"]["attempt_count"], 1,
            "successor must resume the persisted recovery reservation"
        );
        assert_eq!(
            successor["data"]["readiness"]["submit_key_recovery"]["attempted"],
            true
        );
        let registry = orchestration_registry(&state_dir);
        assert_eq!(
            registry["assignments"][&assignment_id]["submit_recovery"]["attempt_count"],
            1
        );
        let expected_enter_count = if crash_stage == "sending" { 1 } else { 2 };
        assert_eq!(
            tmux_calls(&tmux_log)
                .iter()
                .filter(|call| {
                    call.first().is_some_and(|arg| arg == "send-keys")
                        && call.last().is_some_and(|arg| arg == "Enter")
                })
                .count(),
            expected_enter_count,
            "a displaced finalizer must not send Enter during {crash_stage} takeover"
        );
    }
}

#[test]
fn main_agent_submit_recovery_rechecks_authority_inside_the_serialized_send_boundary() {
    for (case, expected_result) in [
        (
            "activity-dialog",
            "worker-activity-not-authoritative-starting",
        ),
        ("broker-absent", "coordination-broker-unavailable"),
        (
            "broker-mismatch",
            "coordination-broker-incarnation-conflict",
        ),
        ("broker-stopped", "coordination-broker-unavailable"),
        (
            "broker-capability-missing",
            "coordination-broker-unavailable",
        ),
        ("broker-heartbeat-stale", "coordination-broker-unavailable"),
        ("active-claim", "worker-not-quiescent"),
        ("active-operation", "worker-not-quiescent"),
        ("uncertain-operation", "worker-not-quiescent"),
        ("main-claim-revoked", "claim-not-active"),
        (
            "worker-checkpoint-before-enter",
            "authenticated worker checkpoint confirmed",
        ),
        ("account-next-queued", "codex-account-next-pending"),
        ("account-bound-control", "checkpoint-timeout"),
        ("account-bound-active-operation", "worker-not-quiescent"),
        ("tmux-timeout", "submit-recovery-send-outcome-unknown"),
        (
            "tmux-timeout-terminal-reconcile",
            "submit-recovery-send-outcome-unknown",
        ),
    ] {
        if std::env::var("NILS_AGENT_SESSION_TEST_RECOVERY_CASE")
            .ok()
            .is_some_and(|selected| selected != case)
        {
            continue;
        }
        let recovery_timeout = if case == "tmux-timeout-terminal-reconcile" {
            "4s"
        } else {
            "1s"
        };
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let state_dir = tmp.path().join("state");
        let checkout = tmp.path().join("checkout");
        let worker_checkout = tmp.path().join("worker-checkout");
        fs::create_dir(&state_dir).expect("state");
        init_checkout(&checkout, "https://example.invalid/example/repository.git");
        init_checkout(
            &worker_checkout,
            "https://example.invalid/example/repository.git",
        );
        seed_brokers_at(
            &state_dir,
            &[(
                "main-one",
                "main-incarnation-one",
                "main-private-capability-material-0000000001",
                checkout.as_path(),
                Some("enforce"),
            )],
        );
        let main_capability =
            init_main_run(tmp.path(), &state_dir, &checkout, "main-one", "run-one");
        let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
        let codex_bin = fake_agent(tmp.path(), "codex-worker");
        let assignment_path = tmp.path().join("assignment-race.json");
        write_private_json(
            &assignment_path,
            &json!({
                "schema_version": "main-agent.assignment-input.v1",
                "assignment_id": "assignment-race",
                "task_summary": "Race a startup dialog against submit recovery",
                "task": {},
                "launch": {
                    "agent": "codex",
                    "cwd": worker_checkout,
                    "title": null,
                    "session_id": "worker-race",
                    "coordination_mode": "enforce",
                    "agent_args": []
                },
                "repository": "example/repository",
                "worktree": worker_checkout,
                "base_ref": "main",
                "scopes": ["crates/worker-race"],
                "durable_refs": []
            }),
        );
        let state_arg = state_dir.to_string_lossy().into_owned();
        let tmux_arg = tmux_bin.to_string_lossy().into_owned();
        let tmux_log_arg = tmux_log.to_string_lossy().into_owned();
        let codex_arg = codex_bin.to_string_lossy().into_owned();
        let codex_home = tmp.path().join("codex-home");
        write_trusted_codex_config(&codex_home, &[&worker_checkout]);
        let codex_home_arg = codex_home.to_string_lossy().into_owned();
        let envs = [
            ("AGENT_SESSION_CAPABILITY_FILE", main_capability.as_str()),
            ("AGENT_SESSION_TMUX_BIN", tmux_arg.as_str()),
            ("AGENT_SESSION_CODEX_BIN", codex_arg.as_str()),
            ("AGENT_SESSION_FAKE_TMUX_LOG", tmux_log_arg.as_str()),
            ("CODEX_HOME", codex_home_arg.as_str()),
        ];
        let started = run_main_agent(
            &checkout,
            &[
                "--state-dir",
                &state_arg,
                "worker",
                "start",
                "--assignment-file",
                assignment_path.to_str().expect("assignment path"),
                "--await-ready",
                "0",
                "--idempotency-key",
                "worker-start-race-0001",
                "--format",
                "json",
            ],
            &envs,
        );
        assert_eq!(started.code, 0, "stderr={}", started.stderr_text());
        let worker_incarnation = data(&started)["worker"]["session_incarnation"]
            .as_str()
            .expect("worker incarnation")
            .to_string();

        let record_lock_path = state_dir.join("session-locks/worker-race.lock");
        let record_lock = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&record_lock_path)
            .expect("worker record lock");
        // SAFETY: the test owns this descriptor and releases it before dropping it.
        assert_eq!(
            unsafe { libc::flock(record_lock.as_raw_fd(), libc::LOCK_EX) },
            0
        );
        let recovery = Command::new(bin::resolve("main-agent"))
            .current_dir(&checkout)
            .args([
                "--state-dir",
                state_arg.as_str(),
                "worker",
                "submit-recovery",
                "assignment-race",
                "--if-revision",
                "2",
                "--timeout",
                recovery_timeout,
                "--idempotency-key",
                "worker-recovery-race-0001",
                "--format",
                "json",
            ])
            .env("AGENT_SESSION_CAPABILITY_FILE", &main_capability)
            .env("AGENT_SESSION_TMUX_BIN", &tmux_arg)
            .env("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg)
            .env(
                "AGENT_SESSION_CODEX_ACCOUNT_BROKER",
                if matches!(
                    case,
                    "account-bound-control" | "account-bound-active-operation"
                ) {
                    r#"["/configured/broker"]"#
                } else {
                    ""
                },
            )
            .env(
                "AGENT_SESSION_FAKE_TMUX_SEND_KEYS_SLEEP",
                if case.starts_with("tmux-timeout") {
                    "2"
                } else {
                    ""
                },
            )
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn submit recovery");
        let reservation_deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let registry: serde_json::Value = serde_json::from_slice(
                &fs::read(state_dir.join("orchestration/registry.json")).expect("registry"),
            )
            .expect("registry json");
            if !registry["assignments"]["assignment-race"]["submit_recovery"].is_null() {
                break;
            }
            assert!(
                Instant::now() < reservation_deadline,
                "submit recovery did not reserve its attempt"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let mut account_state_before = None;
        let mut auto_resume_state_before = None;
        let mut proxy_capability_lease = None;
        match case {
            "activity-dialog" => seed_activity_state(
                &state_dir,
                "worker-race",
                &worker_incarnation,
                "needs_input",
                json!({
                    "provider_turn_id": "turn-race",
                    "started_at": "2030-01-01T00:00:01Z",
                    "last_progress_at": null,
                    "attention": {
                        "kind": "trust",
                        "requested_at": "2030-01-01T00:00:02Z",
                        "pending_count": 1
                    }
                }),
                serde_json::Value::Null,
            ),
            "broker-absent" => rewrite_registry(&state_dir, |registry| {
                registry["brokers"]
                    .as_object_mut()
                    .expect("brokers")
                    .remove("worker-race");
            }),
            "broker-mismatch" => rewrite_registry(&state_dir, |registry| {
                registry["brokers"]["worker-race"]["incarnation"] =
                    json!("replacement-incarnation");
            }),
            "broker-stopped" => rewrite_registry(&state_dir, |registry| {
                registry["brokers"]["worker-race"]["state"] = json!("stopped");
            }),
            "broker-capability-missing" => rewrite_registry(&state_dir, |registry| {
                registry["brokers"]["worker-race"]["capability_digest"] = json!("");
            }),
            "broker-heartbeat-stale" => {
                fs::remove_file(state_dir.join("sessions/worker-race/coordination/heartbeat"))
                    .expect("remove heartbeat");
            }
            "active-claim" => seed_active_claim(
                &state_dir,
                "worker-race",
                &worker_incarnation,
                "worker-race-claim",
            ),
            "active-operation" => seed_operation(
                &state_dir,
                "worker-race",
                &worker_incarnation,
                "worker-race-active-operation",
                "active",
            ),
            "uncertain-operation" => seed_operation(
                &state_dir,
                "worker-race",
                &worker_incarnation,
                "worker-race-uncertain-operation",
                "reconcile_pending",
            ),
            "main-claim-revoked" => {
                let shown = run(
                    &checkout,
                    &[
                        "--state-dir",
                        &state_arg,
                        "work-context",
                        "show",
                        "--session",
                        "main-one",
                        "--capability-file",
                        &main_capability,
                        "--format",
                        "json",
                    ],
                );
                assert_eq!(shown.code, 0, "stderr={}", shown.stderr_text());
                let claim = data(&shown);
                let released = run(
                    &checkout,
                    &[
                        "--state-dir",
                        &state_arg,
                        "work-context",
                        "release",
                        "--session",
                        "main-one",
                        "--claim",
                        claim["claim_id"].as_str().expect("main claim id"),
                        "--if-revision",
                        &claim["revision"].to_string(),
                        "--capability-file",
                        &main_capability,
                        "--idempotency-key",
                        "release-main-authority-before-recovery-enter-0001",
                        "--format",
                        "json",
                    ],
                );
                assert_eq!(released.code, 0, "stderr={}", released.stderr_text());
            }
            "worker-checkpoint-before-enter" => {
                let worker_capability = capability(&state_dir, "worker-race");
                let worker_candidate = tmp.path().join("worker-race-enter-race-claim.json");
                candidate(
                    &worker_candidate,
                    "crates/worker-race",
                    "Checkpoint before recovery Enter authorization",
                );
                let claimed = run(
                    &worker_checkout,
                    &[
                        "--state-dir",
                        &state_arg,
                        "work-context",
                        "claim",
                        "--session",
                        "worker-race",
                        "--file",
                        worker_candidate.to_str().expect("worker candidate"),
                        "--capability-file",
                        &worker_capability,
                        "--idempotency-key",
                        "claim-worker-before-recovery-enter-0001",
                        "--format",
                        "json",
                    ],
                );
                assert_eq!(claimed.code, 0, "stderr={}", claimed.stderr_text());
                let checkpoint_path = tmp.path().join("worker-race-enter-race-checkpoint.json");
                write_private_json(
                    &checkpoint_path,
                    &json!({
                        "schema_version": "main-agent.checkpoint-input.v1",
                        "summary": "Worker checkpoint won the recovery Enter race",
                        "next_action": "Continue without recovery input",
                        "state": "working",
                        "result_summary": null,
                        "blocker_summary": null
                    }),
                );
                let checkpointed = run_main_agent(
                    &worker_checkout,
                    &[
                        "--state-dir",
                        &state_arg,
                        "checkpoint",
                        "--file",
                        checkpoint_path.to_str().expect("checkpoint path"),
                        "--if-revision",
                        "3",
                        "--idempotency-key",
                        "worker-checkpoint-before-recovery-enter-0001",
                        "--format",
                        "json",
                    ],
                    &[("AGENT_SESSION_CAPABILITY_FILE", &worker_capability)],
                );
                assert_eq!(
                    checkpointed.code,
                    0,
                    "stderr={}",
                    checkpointed.stderr_text()
                );
                let released = run(
                    &worker_checkout,
                    &[
                        "--state-dir",
                        &state_arg,
                        "work-context",
                        "release",
                        "--session",
                        "worker-race",
                        "--claim",
                        data(&claimed)["context"]["claim_id"]
                            .as_str()
                            .expect("worker claim id"),
                        "--if-revision",
                        &data(&claimed)["context"]["revision"].to_string(),
                        "--capability-file",
                        &worker_capability,
                        "--idempotency-key",
                        "release-worker-before-recovery-enter-0001",
                        "--format",
                        "json",
                    ],
                );
                assert_eq!(released.code, 0, "stderr={}", released.stderr_text());
            }
            "account-next-queued" => {
                let worker_record_path = state_dir.join("sessions/worker-race/session.json");
                let mut worker_record: serde_json::Value = serde_json::from_slice(
                    &fs::read(&worker_record_path).expect("worker session record"),
                )
                .expect("worker session json");
                let queued = json!({
                    "schema_version": "agent-session.codex-account-next.v1",
                    "account": "poies",
                    "revision": 1,
                    "state": "queued",
                    "updated_at": "2030-01-01T00:00:03Z"
                });
                worker_record["codex_account_next"] = queued.clone();
                write_private_json(&worker_record_path, &worker_record);
                account_state_before = Some(queued);
            }
            "account-bound-control" | "account-bound-active-operation" => {
                let worker_record_path = state_dir.join("sessions/worker-race/session.json");
                let mut worker_record: serde_json::Value = serde_json::from_slice(
                    &fs::read(&worker_record_path).expect("worker session record"),
                )
                .expect("worker session json");
                let runtime_root = tmp.path().join("bound-control-runtime");
                fs::create_dir(&runtime_root).expect("bound control runtime");
                worker_record["runtime"]["kind"] = json!("codex_app_server");
                worker_record["runtime"]["codex_app_server_protocol"] = json!("v2");
                worker_record["runtime"]["codex_app_server_socket"] =
                    json!(runtime_root.join("codex.sock"));
                worker_record["runtime"]["codex_app_server_proxy"] =
                    json!(runtime_root.join("codex-proxy.sock"));
                worker_record["runtime"]["codex_app_server_thread_handoff"] =
                    json!(runtime_root.join("thread-handoff"));
                worker_record["runtime"]["codex_app_server_thread_attached"] =
                    json!(runtime_root.join("thread-attached"));
                let binding = json!({
                    "schema_version": "agent-session.codex-account-binding.v1",
                    "selected_account": "gamania",
                    "revision": 1,
                    "state": "bound",
                    "applied_runtime_id": worker_incarnation,
                    "updated_at": "2030-01-01T00:00:03Z"
                });
                worker_record["codex_account_binding"] = binding.clone();
                write_private_json(&worker_record_path, &worker_record);
                account_state_before = Some(binding);

                let now_ms = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .expect("system time")
                    .as_millis() as u64;
                let capability_path =
                    state_dir.join("sessions/worker-race/.codex-app-server-proxy-capability");
                write_private_json(
                    &capability_path,
                    &json!({
                        "schema_version": "agent-session.codex-manual-input-proxy.v1",
                        "launch_id": worker_incarnation,
                        "token": "00000000-0000-4000-8000-000000000001",
                        "owner_pid": std::process::id(),
                        "expires_at_epoch_ms": now_ms + 60_000
                    }),
                );
                let lease = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&capability_path)
                    .expect("proxy capability lease");
                // SAFETY: the test owns this descriptor until recovery exits.
                assert_eq!(unsafe { libc::flock(lease.as_raw_fd(), libc::LOCK_SH) }, 0);
                proxy_capability_lease = Some(lease);
                if case == "account-bound-active-operation" {
                    seed_operation(
                        &state_dir,
                        "worker-race",
                        &worker_incarnation,
                        "worker-race-bound-active-operation",
                        "active",
                    );
                    let auto_resume = json!({
                        "schema_version": "agent-session.auto-resume.v1",
                        "enabled": true,
                        "state": "armed",
                        "updated_at": "2030-01-01T00:00:03Z",
                        "scheduled_at": null,
                        "next_check_at": null,
                        "failure_reason": null,
                        "blocked_turn_id": "turn-before-recovery",
                        "blocked_revision": 1,
                        "attempt": 0,
                        "ever_scheduled": false,
                        "fallback_schedules": 0
                    });
                    write_private_json(
                        &state_dir.join("sessions/worker-race/auto-resume.json"),
                        &auto_resume,
                    );
                    auto_resume_state_before = Some(auto_resume);
                }
            }
            "tmux-timeout" | "tmux-timeout-terminal-reconcile" => {}
            _ => unreachable!(),
        }
        // SAFETY: the test owns the locked descriptor.
        assert_eq!(
            unsafe { libc::flock(record_lock.as_raw_fd(), libc::LOCK_UN) },
            0
        );
        let output = recovery.wait_with_output().expect("submit recovery output");
        assert!(
            output.status.success(),
            "stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let envelope: serde_json::Value =
            serde_json::from_slice(&output.stdout).expect("submit recovery json");
        assert_eq!(
            envelope["data"]["checkpoint_confirmed"],
            case == "worker-checkpoint-before-enter"
        );
        assert_eq!(envelope["data"]["result"], expected_result, "case={case}");
        let enter_calls = tmux_calls(&tmux_log)
            .into_iter()
            .filter(|call| {
                call.first().is_some_and(|arg| arg == "send-keys")
                    && call.last().is_some_and(|arg| arg == "Enter")
            })
            .count();
        assert_eq!(
            enter_calls,
            if case.starts_with("tmux-timeout") || case == "account-bound-control" {
                2
            } else {
                1
            },
            "{case} must preserve exactly-once recovery input"
        );
        if case == "account-next-queued" {
            let registry = orchestration_registry(&state_dir);
            assert_eq!(
                registry["assignments"]["assignment-race"]["submit_recovery"]["state"],
                "failed"
            );
            assert_eq!(
                registry["assignments"]["assignment-race"]["submit_recovery"]["result"],
                "codex-account-next-pending"
            );
            let replay = run_main_agent(
                &checkout,
                &[
                    "--state-dir",
                    &state_arg,
                    "worker",
                    "submit-recovery",
                    "assignment-race",
                    "--if-revision",
                    "2",
                    "--timeout",
                    "1s",
                    "--idempotency-key",
                    "worker-recovery-race-0001",
                    "--format",
                    "json",
                ],
                &envs,
            );
            assert_eq!(replay.code, 0, "outcome={}", replay.stdout_text());
            assert_eq!(
                data(&replay),
                envelope["data"],
                "the fenced recovery reservation must replay without input"
            );
            assert_eq!(
                tmux_calls(&tmux_log)
                    .into_iter()
                    .filter(|call| {
                        call.first().is_some_and(|arg| arg == "send-keys")
                            && call.last().is_some_and(|arg| arg == "Enter")
                    })
                    .count(),
                1,
                "a queued account intent must fence every recovery replay"
            );
        }
        if let Some(expected_account_state) = account_state_before {
            let worker_record: serde_json::Value = serde_json::from_slice(
                &fs::read(state_dir.join("sessions/worker-race/session.json"))
                    .expect("worker session record"),
            )
            .expect("worker session json");
            let actual = if case == "account-next-queued" {
                &worker_record["codex_account_next"]
            } else {
                &worker_record["codex_account_binding"]
            };
            assert_eq!(
                actual, &expected_account_state,
                "submit recovery must not mutate durable account intent"
            );
            if case == "account-bound-active-operation" {
                assert!(
                    worker_record.get("codex_account_input_fence").is_none(),
                    "a rejected pre-send recovery must not persist an input fence"
                );
            }
        }
        if let Some(expected_auto_resume) = auto_resume_state_before {
            let actual: serde_json::Value = serde_json::from_slice(
                &fs::read(state_dir.join("sessions/worker-race/auto-resume.json"))
                    .expect("auto-resume state"),
            )
            .expect("auto-resume json");
            assert_eq!(
                actual, expected_auto_resume,
                "a rejected pre-send recovery must not cancel auto-resume"
            );
        }
        drop(proxy_capability_lease);
        if case.starts_with("tmux-timeout") {
            let registry = orchestration_registry(&state_dir);
            assert_eq!(
                registry["assignments"]["assignment-race"]["submit_recovery"]["state"],
                "attempting",
                "an ambiguous tmux outcome must preserve the mutation fence"
            );
            let relationship = run_main_agent(
                &checkout,
                &[
                    "--state-dir",
                    &state_arg,
                    "collaborate",
                    "assignment-race",
                    "--session",
                    "main-one@main-incarnation-one",
                    "--if-revision",
                    "3",
                    "--idempotency-key",
                    "collaborate-after-unknown-send-0001",
                    "--format",
                    "json",
                ],
                &[("AGENT_SESSION_CAPABILITY_FILE", &main_capability)],
            );
            assert_eq!(relationship.code, 65);
            assert_eq!(
                relationship.stdout_json()["error"]["code"],
                "submit-recovery-in-flight"
            );
            let replay_args = [
                "--state-dir",
                state_arg.as_str(),
                "worker",
                "submit-recovery",
                "assignment-race",
                "--if-revision",
                "2",
                "--timeout",
                recovery_timeout,
                "--idempotency-key",
                "worker-recovery-race-0001",
                "--format",
                "json",
            ];
            let unknown_replay = run_main_agent(
                &checkout,
                &replay_args,
                &[
                    ("AGENT_SESSION_CAPABILITY_FILE", &main_capability),
                    ("AGENT_SESSION_TMUX_BIN", &tmux_arg),
                    ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
                ],
            );
            assert_eq!(unknown_replay.code, 0);
            assert_eq!(
                data(&unknown_replay)["result"],
                "submit-recovery-send-outcome-unknown"
            );
            assert_eq!(
                tmux_calls(&tmux_log)
                    .into_iter()
                    .filter(|call| {
                        call.first().is_some_and(|arg| arg == "send-keys")
                            && call.last().is_some_and(|arg| arg == "Enter")
                    })
                    .count(),
                2,
                "observer replay cannot send another Enter"
            );
            if case == "tmux-timeout-terminal-reconcile" {
                let worker_record_path = state_dir.join("sessions/worker-race/session.json");
                let mut worker_record: serde_json::Value = serde_json::from_slice(
                    &fs::read(&worker_record_path).expect("worker session record"),
                )
                .expect("worker session json");
                worker_record["delete_tmux_identity"] = json!({
                    "launch_id": worker_incarnation,
                    "session_id": "$77",
                    "pane_id": "%77",
                    "pane_pid": 2_000_000_000,
                    "process_group_id": 2_000_000_000
                });
                write_private_json(&worker_record_path, &worker_record);
                for quiescence_case in ["active-claim", "active-operation", "uncertain-operation"] {
                    match quiescence_case {
                        "active-claim" => seed_active_claim(
                            &state_dir,
                            "worker-race",
                            &worker_incarnation,
                            "worker-reconcile-active-claim",
                        ),
                        "active-operation" => seed_operation(
                            &state_dir,
                            "worker-race",
                            &worker_incarnation,
                            "worker-reconcile-active-operation",
                            "active",
                        ),
                        "uncertain-operation" => seed_operation(
                            &state_dir,
                            "worker-race",
                            &worker_incarnation,
                            "worker-reconcile-uncertain-operation",
                            "reconcile_pending",
                        ),
                        _ => unreachable!(),
                    }
                    let refused = run_main_agent(
                        &checkout,
                        &[
                            "--state-dir",
                            &state_arg,
                            "worker",
                            "reconcile-recovery",
                            "assignment-race",
                            "--if-revision",
                            "3",
                            "--idempotency-key",
                            &format!("refuse-{quiescence_case}-recovery-0001"),
                            "--format",
                            "json",
                        ],
                        &[
                            ("AGENT_SESSION_CAPABILITY_FILE", &main_capability),
                            ("AGENT_SESSION_TMUX_BIN", &tmux_arg),
                            ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
                            ("AGENT_SESSION_FAKE_TMUX_ABSENT", "1"),
                        ],
                    );
                    assert_eq!(refused.code, 65, "case={quiescence_case}");
                    assert_eq!(
                        refused.stdout_json()["error"]["code"],
                        "worker-not-quiescent"
                    );
                    assert_eq!(
                        orchestration_registry(&state_dir)["assignments"]["assignment-race"]["revision"],
                        3,
                        "quiescence refusal must not terminalize recovery"
                    );
                    rewrite_registry(&state_dir, |registry| {
                        registry["claims"]
                            .as_array_mut()
                            .expect("claims")
                            .retain(|claim| claim["session_id"] != "worker-race");
                        registry["operations"]
                            .as_array_mut()
                            .expect("operations")
                            .retain(|operation| operation["session_id"] != "worker-race");
                    });
                }
                worker_record["delete_tmux_identity"] = json!({
                    "launch_id": worker_incarnation,
                    "session_id": "$77",
                    "pane_id": "%77",
                    "pane_pid": 2_000_000_000
                });
                write_private_json(&worker_record_path, &worker_record);
                let incomplete_runtime = run_main_agent(
                    &checkout,
                    &[
                        "--state-dir",
                        &state_arg,
                        "worker",
                        "reconcile-recovery",
                        "assignment-race",
                        "--if-revision",
                        "3",
                        "--idempotency-key",
                        "refuse-incomplete-runtime-recovery-0001",
                        "--format",
                        "json",
                    ],
                    &[
                        ("AGENT_SESSION_CAPABILITY_FILE", &main_capability),
                        ("AGENT_SESSION_TMUX_BIN", &tmux_arg),
                        ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
                        ("AGENT_SESSION_FAKE_TMUX_ABSENT", "1"),
                    ],
                );
                assert_eq!(incomplete_runtime.code, 1);
                assert_eq!(
                    incomplete_runtime.stdout_json()["error"]["code"],
                    "coordination-runtime-unverified"
                );
                worker_record["delete_tmux_identity"] = json!({
                    "launch_id": worker_incarnation,
                    "session_id": "$77",
                    "pane_id": "%77",
                    "pane_pid": std::process::id(),
                    "process_group_id": unsafe { libc::getpgrp() }
                });
                write_private_json(&worker_record_path, &worker_record);
                let live_runtime = run_main_agent(
                    &checkout,
                    &[
                        "--state-dir",
                        &state_arg,
                        "worker",
                        "reconcile-recovery",
                        "assignment-race",
                        "--if-revision",
                        "3",
                        "--idempotency-key",
                        "refuse-live-unknown-recovery-0001",
                        "--format",
                        "json",
                    ],
                    &[
                        ("AGENT_SESSION_CAPABILITY_FILE", &main_capability),
                        ("AGENT_SESSION_TMUX_BIN", &tmux_arg),
                        ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
                        ("AGENT_SESSION_FAKE_TMUX_ABSENT", "1"),
                    ],
                );
                assert_eq!(live_runtime.code, 65);
                assert_eq!(
                    live_runtime.stdout_json()["error"]["code"],
                    "submit-recovery-runtime-still-live"
                );
                worker_record["delete_tmux_identity"] = json!({
                    "launch_id": worker_incarnation,
                    "session_id": "$77",
                    "pane_id": "%77",
                    "pane_pid": 2_000_000_000,
                    "process_group_id": 2_000_000_000
                });
                write_private_json(&worker_record_path, &worker_record);
                let reconciled = run_main_agent(
                    &checkout,
                    &[
                        "--state-dir",
                        &state_arg,
                        "worker",
                        "reconcile-recovery",
                        "assignment-race",
                        "--if-revision",
                        "3",
                        "--idempotency-key",
                        "reconcile-stopped-unknown-recovery-0001",
                        "--format",
                        "json",
                    ],
                    &[
                        ("AGENT_SESSION_CAPABILITY_FILE", &main_capability),
                        ("AGENT_SESSION_TMUX_BIN", &tmux_arg),
                        ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
                        ("AGENT_SESSION_FAKE_TMUX_ABSENT", "1"),
                    ],
                );
                assert_eq!(reconciled.code, 0, "outcome={}", reconciled.stdout_text());
                assert_eq!(data(&reconciled)["reconciled"], true);
                assert_eq!(data(&reconciled)["input_sent"], false);
                assert_eq!(
                    data(&reconciled)["assignment"]["submit_recovery"]["state"],
                    "reconciled"
                );
                assert_eq!(data(&reconciled)["proof"]["worker_runtime"], "stopped");
                assert_eq!(data(&reconciled)["proof"]["coordination"], "quiescent");
                let worker_capability = capability(&state_dir, "worker-race");
                let reconciled_registry = orchestration_registry(&state_dir);
                assert_eq!(
                    reconciled_registry["assignments"]["assignment-race"]["worker_quarantine"]["worker"]
                        ["session_incarnation"],
                    worker_incarnation,
                    "reconciliation must durably quarantine the exact worker incarnation"
                );

                let quarantined_resume = run_with_env(
                    &worker_checkout,
                    &[
                        "--state-dir",
                        &state_arg,
                        "resume",
                        "worker-race",
                        "--format",
                        "json",
                    ],
                    &[
                        ("AGENT_SESSION_TMUX_BIN", &tmux_arg),
                        ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
                        ("AGENT_SESSION_FAKE_TMUX_ABSENT", "1"),
                    ],
                );
                assert_eq!(quarantined_resume.code, 65);
                assert_eq!(
                    quarantined_resume.stdout_json()["error"]["code"],
                    "worker-quarantined"
                );

                let quarantined_claim_path = tmp.path().join("claim-after-terminal-reconcile.json");
                candidate(
                    &quarantined_claim_path,
                    "crates/worker-race",
                    "Quarantined worker must not restore execution authority",
                );
                let quarantined_claim = run(
                    &worker_checkout,
                    &[
                        "--state-dir",
                        &state_arg,
                        "work-context",
                        "claim",
                        "--session",
                        "worker-race",
                        "--file",
                        quarantined_claim_path.to_str().expect("quarantined claim"),
                        "--capability-file",
                        &worker_capability,
                        "--idempotency-key",
                        "claim-after-terminal-reconcile-0001",
                        "--format",
                        "json",
                    ],
                );
                assert_eq!(quarantined_claim.code, 65);
                assert_eq!(
                    quarantined_claim.stdout_json()["error"]["code"],
                    "worker-quarantined"
                );

                let quarantined_bootstrap = run_main_agent(
                    &worker_checkout,
                    &[
                        "--state-dir",
                        &state_arg,
                        "bootstrap",
                        "--idempotency-key",
                        "bootstrap-after-terminal-reconcile-0001",
                        "--format",
                        "json",
                    ],
                    &[("AGENT_SESSION_CAPABILITY_FILE", &worker_capability)],
                );
                assert_eq!(quarantined_bootstrap.code, 65);
                assert_eq!(
                    quarantined_bootstrap.stdout_json()["error"]["code"],
                    "worker-quarantined"
                );

                let quarantined_checkpoint_path =
                    tmp.path().join("checkpoint-after-terminal-reconcile.json");
                write_private_json(
                    &quarantined_checkpoint_path,
                    &json!({
                        "schema_version": "main-agent.checkpoint-input.v1",
                        "summary": "Quarantined worker must not checkpoint",
                        "next_action": "Remain stopped for guarded cancellation",
                        "state": "working",
                        "result_summary": null,
                        "blocker_summary": null
                    }),
                );
                let quarantined_checkpoint = run_main_agent(
                    &worker_checkout,
                    &[
                        "--state-dir",
                        &state_arg,
                        "checkpoint",
                        "--file",
                        quarantined_checkpoint_path
                            .to_str()
                            .expect("quarantined checkpoint"),
                        "--if-revision",
                        "4",
                        "--idempotency-key",
                        "checkpoint-after-terminal-reconcile-0001",
                        "--format",
                        "json",
                    ],
                    &[("AGENT_SESSION_CAPABILITY_FILE", &worker_capability)],
                );
                assert_eq!(quarantined_checkpoint.code, 65);
                assert_eq!(
                    quarantined_checkpoint.stdout_json()["error"]["code"],
                    "worker-quarantined"
                );
                let replay_started = Instant::now();
                let terminal_replay = run_main_agent(
                    &checkout,
                    &replay_args,
                    &[
                        ("AGENT_SESSION_CAPABILITY_FILE", &main_capability),
                        ("AGENT_SESSION_TMUX_BIN", &tmux_arg),
                        ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
                    ],
                );
                assert_eq!(
                    terminal_replay.code,
                    0,
                    "outcome={}",
                    terminal_replay.stdout_text()
                );
                assert!(
                    replay_started.elapsed() < Duration::from_millis(1_500),
                    "reconciled replay must not wait for the original timeout"
                );
                assert_eq!(
                    data(&terminal_replay)["result"],
                    "worker-runtime-stopped-without-checkpoint"
                );
                assert_eq!(
                    data(&terminal_replay)["assignment"]["submit_recovery"]["state"],
                    "reconciled"
                );
                assert_eq!(data(&terminal_replay)["assignment"]["revision"], 4);
                assert_eq!(
                    tmux_calls(&tmux_log)
                        .into_iter()
                        .filter(|call| {
                            call.first().is_some_and(|arg| arg == "send-keys")
                                && call.last().is_some_and(|arg| arg == "Enter")
                        })
                        .count(),
                    2,
                    "terminal reconciliation must never resend Enter"
                );
                let stopped_broker = run(
                    &worker_checkout,
                    &[
                        "--state-dir",
                        &state_arg,
                        "broker",
                        "stop",
                        "--session",
                        "worker-race",
                        "--capability-file",
                        &worker_capability,
                        "--format",
                        "json",
                    ],
                );
                assert_eq!(
                    stopped_broker.code,
                    0,
                    "stderr={}",
                    stopped_broker.stderr_text()
                );
                assert_eq!(data(&stopped_broker)["state"], "stopped");
                let stopped_registry: serde_json::Value = serde_json::from_slice(
                    &fs::read(state_dir.join("coordination/registry.json"))
                        .expect("coordination registry"),
                )
                .expect("coordination registry json");
                assert_eq!(
                    stopped_registry["brokers"]["worker-race"]["state"],
                    "stopped"
                );
                assert_eq!(
                    stopped_registry["brokers"]["worker-race"]["capability_digest"],
                    ""
                );
                let cancel_barrier = tmp.path().join("cancel-authority-barrier");
                fs::create_dir(&cancel_barrier).expect("cancel barrier");
                let cancel_barrier_arg = cancel_barrier.to_string_lossy().into_owned();
                let mut raced_cancel = Command::new(bin::resolve("main-agent"));
                raced_cancel
                    .current_dir(&checkout)
                    .args([
                        "--state-dir",
                        state_arg.as_str(),
                        "worker",
                        "cancel",
                        "assignment-race",
                        "--if-revision",
                        "4",
                        "--reason",
                        "race terminal cancellation against controller claim release",
                        "--idempotency-key",
                        "cancel-after-terminal-reconcile-claim-race-0001",
                        "--format",
                        "json",
                    ])
                    .env("AGENT_SESSION_CAPABILITY_FILE", &main_capability)
                    .env("AGENT_SESSION_TMUX_BIN", &tmux_arg)
                    .env("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg)
                    .env("AGENT_SESSION_FAKE_TMUX_ABSENT", "1")
                    .env(
                        "NILS_AGENT_SESSION_TEST_WORKER_CANCEL_BARRIER_DIR",
                        &cancel_barrier_arg,
                    )
                    .env(
                        "NILS_AGENT_SESSION_TEST_WORKER_CANCEL_ASSIGNMENT",
                        "assignment-race",
                    )
                    .env(
                        "NILS_AGENT_SESSION_TEST_WORKER_CANCEL_WORKER",
                        format!("worker-race@{worker_incarnation}"),
                    )
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped());
                let raced_cancel = raced_cancel.spawn().expect("spawn raced cancel");
                let cancel_ready_deadline = Instant::now() + Duration::from_secs(10);
                while !cancel_barrier.join("ready").is_file() {
                    assert!(
                        Instant::now() < cancel_ready_deadline,
                        "worker cancel did not reach its admission barrier"
                    );
                    std::thread::sleep(Duration::from_millis(10));
                }
                let shown = run(
                    &checkout,
                    &[
                        "--state-dir",
                        &state_arg,
                        "work-context",
                        "show",
                        "--session",
                        "main-one",
                        "--capability-file",
                        &main_capability,
                        "--format",
                        "json",
                    ],
                );
                assert_eq!(shown.code, 0, "stderr={}", shown.stderr_text());
                let main_claim = data(&shown);
                let released_main = run(
                    &checkout,
                    &[
                        "--state-dir",
                        &state_arg,
                        "work-context",
                        "release",
                        "--session",
                        "main-one",
                        "--claim",
                        main_claim["claim_id"].as_str().expect("main claim id"),
                        "--if-revision",
                        &main_claim["revision"].to_string(),
                        "--capability-file",
                        &main_capability,
                        "--idempotency-key",
                        "release-main-during-terminal-cancel-0001",
                        "--format",
                        "json",
                    ],
                );
                assert_eq!(
                    released_main.code,
                    0,
                    "stderr={}",
                    released_main.stderr_text()
                );
                fs::write(cancel_barrier.join("release"), b"release")
                    .expect("release cancel barrier");
                let raced_cancel = raced_cancel
                    .wait_with_output()
                    .expect("raced cancel output");
                assert_eq!(raced_cancel.status.code(), Some(65));
                let raced_cancel_envelope: serde_json::Value =
                    serde_json::from_slice(&raced_cancel.stdout).expect("raced cancel json");
                assert_eq!(raced_cancel_envelope["error"]["code"], "claim-not-active");
                assert_eq!(
                    orchestration_registry(&state_dir)["assignments"]["assignment-race"]["revision"],
                    4,
                    "claim-loss race must not mutate the assignment"
                );
                assert_eq!(
                    orchestration_registry(&state_dir)["assignments"]["assignment-race"]["state"],
                    "starting",
                    "claim-loss race must preserve the assignment lifecycle state"
                );
                let main_reclaim = tmp.path().join("main-reclaim-after-cancel-race.json");
                candidate(
                    &main_reclaim,
                    "crates/agent-session",
                    "Reclaim Main Agent authority after cancellation race",
                );
                let reclaimed_main = run(
                    &checkout,
                    &[
                        "--state-dir",
                        &state_arg,
                        "work-context",
                        "claim",
                        "--session",
                        "main-one",
                        "--file",
                        main_reclaim.to_str().expect("main reclaim"),
                        "--capability-file",
                        &main_capability,
                        "--idempotency-key",
                        "reclaim-main-after-terminal-cancel-race-0001",
                        "--format",
                        "json",
                    ],
                );
                assert_eq!(
                    reclaimed_main.code,
                    0,
                    "stderr={}",
                    reclaimed_main.stderr_text()
                );
                let cancelled = run_main_agent(
                    &checkout,
                    &[
                        "--state-dir",
                        &state_arg,
                        "worker",
                        "cancel",
                        "assignment-race",
                        "--if-revision",
                        "4",
                        "--reason",
                        "terminally reconciled pre-claim runtime stopped",
                        "--idempotency-key",
                        "cancel-after-terminal-reconcile-0001",
                        "--format",
                        "json",
                    ],
                    &[
                        ("AGENT_SESSION_CAPABILITY_FILE", &main_capability),
                        ("AGENT_SESSION_TMUX_BIN", &tmux_arg),
                        ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
                        ("AGENT_SESSION_FAKE_TMUX_ABSENT", "1"),
                    ],
                );
                assert_eq!(cancelled.code, 0, "outcome={}", cancelled.stdout_text());
                assert_eq!(data(&cancelled)["assignment"]["state"], "cancelled");
                let retired = run_main_agent(
                    &checkout,
                    &[
                        "--state-dir",
                        &state_arg,
                        "worker",
                        "retire",
                        "assignment-race",
                        "--if-revision",
                        "5",
                        "--idempotency-key",
                        "retire-after-terminal-reconcile-0001",
                        "--format",
                        "json",
                    ],
                    &[
                        ("AGENT_SESSION_CAPABILITY_FILE", &main_capability),
                        ("AGENT_SESSION_TMUX_BIN", &tmux_arg),
                        ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
                        ("AGENT_SESSION_FAKE_TMUX_ABSENT", "1"),
                    ],
                );
                assert_eq!(retired.code, 0, "outcome={}", retired.stdout_text());
                assert_eq!(data(&retired)["retired"], true);
                continue;
            }
            let worker_capability = capability(&state_dir, "worker-race");
            let worker_candidate = tmp.path().join("worker-race-claim.json");
            candidate(
                &worker_candidate,
                "crates/worker-race",
                "Reconcile the ambiguous recovery through an authenticated checkpoint",
            );
            let claimed = run(
                &worker_checkout,
                &[
                    "--state-dir",
                    &state_arg,
                    "work-context",
                    "claim",
                    "--session",
                    "worker-race",
                    "--file",
                    worker_candidate.to_str().expect("worker candidate"),
                    "--capability-file",
                    &worker_capability,
                    "--idempotency-key",
                    "claim-worker-after-ambiguous-recovery-0001",
                    "--format",
                    "json",
                ],
            );
            assert_eq!(claimed.code, 0, "stderr={}", claimed.stderr_text());
            let checkpoint_path = tmp.path().join("worker-race-checkpoint.json");
            write_private_json(
                &checkpoint_path,
                &json!({
                    "schema_version": "main-agent.checkpoint-input.v1",
                    "summary": "Authenticated worker checkpoint after ambiguous Enter",
                    "next_action": "Continue without resending input",
                    "state": "working",
                    "result_summary": null,
                    "blocker_summary": null
                }),
            );
            let checkpointed = run_main_agent(
                &worker_checkout,
                &[
                    "--state-dir",
                    &state_arg,
                    "checkpoint",
                    "--file",
                    checkpoint_path.to_str().expect("checkpoint path"),
                    "--if-revision",
                    "3",
                    "--idempotency-key",
                    "worker-checkpoint-after-ambiguous-recovery-0001",
                    "--format",
                    "json",
                ],
                &[("AGENT_SESSION_CAPABILITY_FILE", &worker_capability)],
            );
            assert_eq!(
                checkpointed.code,
                0,
                "stderr={}",
                checkpointed.stderr_text()
            );
            let confirmed = run_main_agent(
                &checkout,
                &replay_args,
                &[
                    ("AGENT_SESSION_CAPABILITY_FILE", &main_capability),
                    ("AGENT_SESSION_TMUX_BIN", &tmux_arg),
                    ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
                ],
            );
            assert_eq!(confirmed.code, 0, "stderr={}", confirmed.stderr_text());
            assert_eq!(data(&confirmed)["checkpoint_confirmed"], true);
            assert_eq!(
                data(&confirmed)["assignment"]["submit_recovery"]["state"],
                "checkpoint_confirmed"
            );
            let confirmed_replay = run_main_agent(
                &checkout,
                &replay_args,
                &[
                    ("AGENT_SESSION_CAPABILITY_FILE", &main_capability),
                    ("AGENT_SESSION_TMUX_BIN", &tmux_arg),
                    ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
                ],
            );
            assert_eq!(data(&confirmed_replay), data(&confirmed));
        }
    }
}

#[test]
fn main_agent_submit_recovery_fences_concurrent_manager_mutations_until_resolved() {
    for transition in ["cancel", "handoff", "collaborate", "borrow"] {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let state_dir = tmp.path().join("state");
        let checkout = tmp.path().join("checkout");
        let worker_checkout = tmp.path().join("worker-checkout");
        fs::create_dir(&state_dir).expect("state");
        init_checkout(&checkout, "https://example.invalid/example/repository.git");
        init_checkout(
            &worker_checkout,
            "https://example.invalid/example/repository.git",
        );
        seed_brokers_at(
            &state_dir,
            &[
                (
                    "main-one",
                    "main-incarnation-one",
                    "main-private-capability-material-0000000001",
                    checkout.as_path(),
                    Some("enforce"),
                ),
                (
                    "main-two",
                    "main-incarnation-two",
                    "main-private-capability-material-0000000002",
                    checkout.as_path(),
                    Some("enforce"),
                ),
            ],
        );
        let main_capability =
            init_main_run(tmp.path(), &state_dir, &checkout, "main-one", "run-one");
        if transition == "handoff" {
            rewrite_orchestration_registry(&state_dir, |registry| {
                let mut run_two = registry["runs"]["run-one"].clone();
                let mut controller = run_two["controller"].clone();
                run_two["run_id"] = json!("run-two");
                controller["session_id"] = json!("main-two");
                controller["session_incarnation"] = json!("main-incarnation-two");
                run_two["controller"] = controller;
                registry["runs"]["run-two"] = run_two;
            });
        }
        let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
        let codex_bin = fake_agent(tmp.path(), "codex-worker");
        let assignment_path = tmp.path().join("assignment-transition.json");
        write_private_json(
            &assignment_path,
            &json!({
                "schema_version": "main-agent.assignment-input.v1",
                "assignment_id": "assignment-transition",
                "task_summary": "Reject manager transitions as worker checkpoints",
                "task": {},
                "launch": {
                    "agent": "codex",
                    "cwd": worker_checkout,
                    "title": null,
                    "session_id": "worker-transition",
                    "coordination_mode": "enforce",
                    "agent_args": []
                },
                "repository": "example/repository",
                "worktree": worker_checkout,
                "base_ref": "main",
                "scopes": ["docs/transition"],
                "durable_refs": []
            }),
        );
        let state_arg = state_dir.to_string_lossy().into_owned();
        let tmux_arg = tmux_bin.to_string_lossy().into_owned();
        let tmux_log_arg = tmux_log.to_string_lossy().into_owned();
        let codex_arg = codex_bin.to_string_lossy().into_owned();
        let codex_home = tmp.path().join("codex-home");
        write_trusted_codex_config(&codex_home, &[&worker_checkout]);
        let codex_home_arg = codex_home.to_string_lossy().into_owned();
        let started = run_main_agent(
            &checkout,
            &[
                "--state-dir",
                &state_arg,
                "worker",
                "start",
                "--assignment-file",
                assignment_path.to_str().expect("assignment path"),
                "--await-ready",
                "0",
                "--idempotency-key",
                "worker-start-transition-0001",
                "--format",
                "json",
            ],
            &[
                ("AGENT_SESSION_CAPABILITY_FILE", &main_capability),
                ("AGENT_SESSION_TMUX_BIN", &tmux_arg),
                ("AGENT_SESSION_CODEX_BIN", &codex_arg),
                ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
                ("CODEX_HOME", &codex_home_arg),
            ],
        );
        assert_eq!(started.code, 0, "stderr={}", started.stderr_text());
        if transition == "cancel" {
            rewrite_orchestration_registry(&state_dir, |registry| {
                registry["assignments"]["assignment-transition"]["blocker_summary"] =
                    json!("[pre-claim:test] deterministic concurrent cancellation");
            });
        }

        let hook = tmp.path().join("transition-on-recovery-enter");
        let hook_output = tmp.path().join("transition-output.json");
        let transition_args = match transition {
            "cancel" => {
                "worker cancel assignment-transition --if-revision 3 --reason concurrent-cancel --idempotency-key concurrent-cancel-0001"
            }
            "handoff" => {
                "handoff assignment-transition --to main-two@main-incarnation-two --if-revision 3 --idempotency-key concurrent-handoff-0001"
            }
            "collaborate" => {
                "collaborate assignment-transition --session main-two@main-incarnation-two --if-revision 3 --idempotency-key concurrent-collaborate-0001"
            }
            "borrow" => {
                "borrow assignment-transition --session main-two@main-incarnation-two --duration 5m --if-revision 3 --idempotency-key concurrent-borrow-0001"
            }
            _ => unreachable!(),
        };
        fs::write(
            &hook,
            format!(
                "#!/usr/bin/env sh\nset -eu\nAGENT_SESSION_CAPABILITY_FILE='{main_capability}' '{main_agent}' --state-dir '{state_dir}' {transition_args} --format json > '{output}'\n",
                main_capability = main_capability,
                main_agent = bin::resolve("main-agent").display(),
                state_dir = state_dir.display(),
                transition_args = transition_args,
                output = hook_output.display(),
            ),
        )
        .expect("transition hook");
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o700)).expect("hook mode");
        let hook_arg = hook.to_string_lossy().into_owned();
        let enter_count = tmp.path().join("transition-enter-count");
        let enter_count_arg = enter_count.to_string_lossy().into_owned();
        let recovered = run_main_agent(
            &checkout,
            &[
                "--state-dir",
                &state_arg,
                "worker",
                "submit-recovery",
                "assignment-transition",
                "--if-revision",
                "2",
                "--timeout",
                "1s",
                "--idempotency-key",
                "submit-recovery-transition-0001",
                "--format",
                "json",
            ],
            &[
                ("AGENT_SESSION_CAPABILITY_FILE", &main_capability),
                ("AGENT_SESSION_TMUX_BIN", &tmux_arg),
                ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
                ("AGENT_SESSION_FAKE_TMUX_ENTER_HOOK", &hook_arg),
                ("AGENT_SESSION_FAKE_TMUX_ENTER_COUNT_FILE", &enter_count_arg),
                ("AGENT_SESSION_FAKE_TMUX_ENTER_HOOK_AT", "1"),
            ],
        );
        assert_eq!(recovered.code, 0, "stderr={}", recovered.stderr_text());
        let hook_deadline = Instant::now() + Duration::from_secs(10);
        while fs::metadata(&hook_output).map_or(true, |metadata| metadata.len() == 0)
            && Instant::now() < hook_deadline
        {
            std::thread::sleep(Duration::from_millis(10));
        }
        let hook_envelope: serde_json::Value =
            serde_json::from_slice(&fs::read(&hook_output).expect("transition hook output"))
                .expect("transition hook json");
        if transition == "cancel" {
            assert_eq!(
                hook_envelope["ok"], true,
                "a typed pre-claim failure terminalizes recovery before cancellation: {hook_envelope}"
            );
            assert_eq!(hook_envelope["data"]["assignment"]["state"], "cancelled");
            assert_eq!(data(&recovered)["checkpoint_confirmed"], false);
            assert_eq!(
                data(&recovered)["result"],
                "worker-bootstrap-preclaim-failed"
            );
            assert_eq!(
                data(&recovered)["assignment"]["state"],
                "starting",
                "the recovery caller may snapshot before the concurrent cancellation commits"
            );
            assert_eq!(
                data(&recovered)["assignment"]["submit_recovery"]["state"],
                "failed"
            );
            assert_eq!(
                data(&recovered)["assignment"]["submit_recovery"]["result"],
                "worker-bootstrap-preclaim-failed"
            );
            assert_eq!(
                data(&recovered)["assignment"]["primary_manager"]["session_id"],
                "main-one"
            );
            assert_eq!(
                orchestration_registry(&state_dir)["assignments"]["assignment-transition"]["state"],
                "cancelled",
                "the successful hook result proves the terminal cancellation eventually committed"
            );
            continue;
        }
        assert_eq!(
            hook_envelope["ok"], false,
            "transition={transition}, envelope={hook_envelope}"
        );
        assert_eq!(
            hook_envelope["error"]["code"], "submit-recovery-in-flight",
            "transition={transition}, envelope={hook_envelope}"
        );
        assert_eq!(data(&recovered)["checkpoint_confirmed"], false);
        assert_eq!(data(&recovered)["result"], "checkpoint-timeout");
        assert_eq!(data(&recovered)["assignment"]["state"], "starting");
        assert_eq!(
            data(&recovered)["assignment"]["primary_manager"]["session_id"],
            "main-one",
            "a losing transition cannot change authority"
        );
    }
}

#[test]
fn main_agent_worker_start_await_ready_stops_on_authoritative_terminal_turns() {
    for kind in ["turn_completed", "turn_failed"] {
        assert_main_agent_worker_start_stops_on_authoritative_turn(kind);
    }
}

fn assert_main_agent_worker_start_stops_on_authoritative_turn(kind: &str) {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let checkout = tmp.path().join("checkout");
    let worker_checkout = tmp.path().join("worker-checkout");
    fs::create_dir(&state_dir).expect("state");
    init_checkout(&checkout, "https://example.invalid/example/repository.git");
    init_checkout(
        &worker_checkout,
        "https://example.invalid/example/repository.git",
    );
    seed_brokers_at(
        &state_dir,
        &[(
            "main-one",
            "main-incarnation-one",
            "main-private-capability-material-0000000001",
            checkout.as_path(),
            Some("enforce"),
        )],
    );
    let main_capability = init_main_run(tmp.path(), &state_dir, &checkout, "main-one", "run-one");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let codex_bin = fake_agent(tmp.path(), "codex-worker");
    let worker_id = "worker-authoritative-complete";
    let assignment_path = tmp.path().join("assignment-authoritative-complete.json");
    write_private_json(
        &assignment_path,
        &json!({
            "schema_version": "main-agent.assignment-input.v1",
            "assignment_id": "assignment-authoritative-complete",
            "task_summary": "Stop readiness after the provider turn terminates",
            "task": {},
            "launch": {
                "agent": "codex",
                "cwd": worker_checkout,
                "title": null,
                "session_id": worker_id,
                "coordination_mode": "enforce",
                "agent_args": []
            },
            "repository": "example/repository",
            "worktree": worker_checkout,
            "base_ref": "main",
            "scopes": ["crates/worker-lane"],
            "durable_refs": []
        }),
    );
    let codex_home = tmp.path().join("codex-home");
    write_trusted_codex_config(&codex_home, &[&worker_checkout]);

    let state_arg = state_dir.to_string_lossy().into_owned();
    let tmux_arg = tmux_bin.to_string_lossy().into_owned();
    let tmux_log_arg = tmux_log.to_string_lossy().into_owned();
    let codex_arg = codex_bin.to_string_lossy().into_owned();
    let start = Command::new(bin::resolve("main-agent"))
        .current_dir(&checkout)
        .args([
            "--state-dir",
            state_arg.as_str(),
            "worker",
            "start",
            "--assignment-file",
            assignment_path.to_str().expect("assignment path"),
            "--await-ready",
            "4s",
            "--idempotency-key",
            "worker-start-authoritative-complete-0001",
            "--format",
            "json",
        ])
        .env("AGENT_SESSION_CAPABILITY_FILE", &main_capability)
        .env("AGENT_SESSION_TMUX_BIN", &tmux_arg)
        .env("AGENT_SESSION_CODEX_BIN", &codex_arg)
        .env("CODEX_HOME", &codex_home)
        .env("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn worker start");

    let worker_record_path = state_dir.join(format!("sessions/{worker_id}/session.json"));
    let wait_deadline = Instant::now() + Duration::from_secs(10);
    while !worker_record_path.is_file() && Instant::now() < wait_deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let worker_record: serde_json::Value =
        serde_json::from_slice(&fs::read(&worker_record_path).expect("worker session record"))
            .expect("worker session json");
    let runtime_id = worker_record["runtime"]["launch_id"]
        .as_str()
        .expect("worker runtime id");
    let event = json!({
        "schema_version": "agent-session.turn-event.v1",
        "event_id": "worker-authoritative-complete-event",
        "runtime_id": runtime_id,
        "provider": "codex",
        "provider_session_id": "provider-session",
        "kind": kind,
        "confidence": "authoritative"
    })
    .to_string();
    let mut event_command = Command::new(bin::resolve("agent-session"))
        .current_dir(tmp.path())
        .args([
            "--state-dir",
            state_arg.as_str(),
            "activity",
            "event",
            worker_id,
            "--stdin",
            "--format",
            "json",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn authoritative completion event");
    event_command
        .stdin
        .as_mut()
        .expect("event stdin")
        .write_all(event.as_bytes())
        .expect("write completion event");
    let event_output = event_command.wait_with_output().expect("completion event");
    assert!(
        event_output.status.success(),
        "event stdout={} stderr={}",
        String::from_utf8_lossy(&event_output.stdout),
        String::from_utf8_lossy(&event_output.stderr)
    );

    let event_ingested_at = Instant::now();
    let output = start.wait_with_output().expect("worker start output");
    let completion_after_ingest = event_ingested_at.elapsed();
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        completion_after_ingest < Duration::from_millis(1_500),
        "{kind} should end readiness well before the 4s outer timeout; elapsed={completion_after_ingest:?}"
    );
    let envelope: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("worker start json");
    let readiness = &envelope["data"]["readiness"];
    assert_eq!(readiness["state"], "readiness_failed");
    assert_eq!(
        readiness["classification"],
        "submitted_or_waiting_without_checkpoint"
    );
    assert_eq!(
        readiness["delivery"]["proof"],
        "authoritative-provider-turn-terminated"
    );
    let enter_calls = tmux_calls(&tmux_log)
        .into_iter()
        .filter(|call| {
            call.first().is_some_and(|arg| arg == "send-keys")
                && call.last().is_some_and(|arg| arg == "Enter")
        })
        .count();
    assert_eq!(
        enter_calls, 1,
        "provider termination forbids the recovery Enter"
    );
    let supervised = run_main_agent(
        &checkout,
        &[
            "--state-dir",
            state_arg.as_str(),
            "worker",
            "supervise",
            "assignment-authoritative-complete",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_CAPABILITY_FILE", &main_capability)],
    );
    assert_eq!(supervised.code, 0, "stderr={}", supervised.stderr_text());
    assert_eq!(
        data(&supervised)["classification"],
        "submitted_or_waiting_without_checkpoint"
    );
    assert_eq!(
        data(&supervised)["last_proven_safe_state"]["reassignment_safe"],
        true
    );
}

#[test]
fn main_agent_worker_start_single_enter_recovery_survives_immediate_acceptance() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let checkout = tmp.path().join("checkout");
    let worker_checkout = tmp.path().join("worker-checkout");
    fs::create_dir(&state_dir).expect("state");
    init_checkout(&checkout, "https://example.invalid/example/repository.git");
    init_checkout(
        &worker_checkout,
        "https://example.invalid/example/repository.git",
    );
    seed_brokers_at(
        &state_dir,
        &[(
            "main-one",
            "main-incarnation-one",
            "main-private-capability-material-0000000001",
            checkout.as_path(),
            Some("enforce"),
        )],
    );
    let main_capability = init_main_run(tmp.path(), &state_dir, &checkout, "main-one", "run-one");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let codex_bin = fake_agent(tmp.path(), "codex-worker");
    let assignment_id = "assignment-enter-recovery";
    let worker_id = "worker-enter-recovery";
    let assignment_path = tmp.path().join("assignment-enter-recovery.json");
    write_private_json(
        &assignment_path,
        &json!({
            "schema_version": "main-agent.assignment-input.v1",
            "assignment_id": assignment_id,
            "task_summary": "Recover one dropped submit key",
            "task": {},
            "launch": {
                "agent": "codex",
                "cwd": worker_checkout,
                "title": null,
                "session_id": worker_id,
                "coordination_mode": "enforce",
                "agent_args": []
            },
            "repository": "example/repository",
            "worktree": worker_checkout,
            "base_ref": "main",
            "scopes": ["crates/worker-lane"],
            "durable_refs": []
        }),
    );

    let bootstrap_digest = orchestration_request_digest(
        "main-agent-worker-bootstrap-idempotency",
        &json!(assignment_id),
    );
    let bootstrap_key = format!("bootstrap-{}", &bootstrap_digest[..32]);
    let enter_hook = tmp.path().join("bootstrap-on-second-enter");
    let enter_hook_output = tmp.path().join("bootstrap-on-second-enter.json");
    let submitted_checkpoint = tmp.path().join("submitted-on-second-enter.json");
    let checkpoint_output = tmp.path().join("checkpoint-on-second-enter.json");
    let accept_output = tmp.path().join("accept-on-second-enter.json");
    fs::write(
        &enter_hook,
        format!(
            r#"#!/usr/bin/env sh
set -eu
capability_file=""
for candidate in {state_dir}/sessions/{worker_id}/coordination/capability-*; do
  [ -f "$candidate" ] || continue
  capability_file="$candidate"
  break
done
[ -n "$capability_file" ]
checkpoint_file=""
for candidate in {state_dir}/sessions/{worker_id}/coordination/main-agent-checkpoint-*; do
  [ -f "$candidate" ] || continue
  checkpoint_file="$candidate"
  break
done
[ -n "$checkpoint_file" ]
cd {worker_checkout}
AGENT_SESSION_CAPABILITY_FILE="$capability_file" \
AGENT_SESSION_CHECKPOINT_FILE="$checkpoint_file" \
  {main_agent} --state-dir {state_dir} bootstrap \
  --idempotency-key {bootstrap_key} --format json > {output}
cat > {submitted_checkpoint} <<'JSON'
{{"schema_version":"main-agent.checkpoint-input.v1","summary":"Worker submitted immediately after recovery","next_action":"Manager acceptance","state":"submitted","result_summary":"ready for acceptance","blocker_summary":null}}
JSON
chmod 600 {submitted_checkpoint}
AGENT_SESSION_CAPABILITY_FILE="$capability_file" \
AGENT_SESSION_CHECKPOINT_FILE="$checkpoint_file" \
  {main_agent} --state-dir {state_dir} checkpoint \
  --file {submitted_checkpoint} --if-revision 4 \
  --idempotency-key checkpoint-after-recovery-0001 --format json > {checkpoint_output}
AGENT_SESSION_CAPABILITY_FILE={main_capability} \
  {main_agent} --state-dir {state_dir} worker accept {assignment_id} \
  --if-revision 5 --idempotency-key accept-after-recovery-0001 \
  --format json > {accept_output}
"#,
            state_dir = state_dir.display(),
            worker_checkout = worker_checkout.display(),
            worker_id = worker_id,
            main_agent = bin::resolve("main-agent").display(),
            bootstrap_key = bootstrap_key,
            output = enter_hook_output.display(),
            submitted_checkpoint = submitted_checkpoint.display(),
            checkpoint_output = checkpoint_output.display(),
            main_capability = main_capability,
            assignment_id = assignment_id,
            accept_output = accept_output.display(),
        ),
    )
    .expect("enter hook");
    fs::set_permissions(&enter_hook, fs::Permissions::from_mode(0o700)).expect("enter hook mode");
    let enter_count = tmp.path().join("enter-count");

    let tmux_arg = tmux_bin.to_string_lossy().into_owned();
    let tmux_log_arg = tmux_log.to_string_lossy().into_owned();
    let codex_arg = codex_bin.to_string_lossy().into_owned();
    let enter_hook_arg = enter_hook.to_string_lossy().into_owned();
    let enter_count_arg = enter_count.to_string_lossy().into_owned();
    let codex_home = tmp.path().join("codex-home");
    let started = run_main_agent_with_codex_trust(
        &checkout,
        &[
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "worker",
            "start",
            "--assignment-file",
            assignment_path.to_str().expect("assignment path"),
            "--if-run-revision",
            "1",
            "--await-ready",
            "2s",
            "--idempotency-key",
            "worker-start-enter-recovery-0001",
            "--format",
            "json",
        ],
        &[
            ("AGENT_SESSION_CAPABILITY_FILE", &main_capability),
            ("AGENT_SESSION_TMUX_BIN", &tmux_arg),
            ("AGENT_SESSION_CODEX_BIN", &codex_arg),
            ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
            ("AGENT_SESSION_FAKE_TMUX_ENTER_HOOK", &enter_hook_arg),
            ("AGENT_SESSION_FAKE_TMUX_ENTER_COUNT_FILE", &enter_count_arg),
        ],
        &codex_home,
        &[&worker_checkout],
    );
    assert_eq!(started.code, 0, "stderr={}", started.stderr_text());
    let readiness = data(&started)["readiness"].clone();
    let enter_hook_result = fs::read_to_string(&enter_hook_output).unwrap_or_else(|error| {
        format!(
            "missing hook output ({error}); enter-count={}",
            fs::read_to_string(&enter_count).unwrap_or_else(|_| "missing".to_string())
        )
    });
    assert!(
        enter_hook_output.is_file(),
        "second Enter did not run bootstrap: {enter_hook_result}"
    );
    assert_eq!(
        readiness["state"],
        "ready",
        "readiness={readiness} hook={enter_hook_result} stderr={}",
        started.stderr_text()
    );
    assert!(
        matches!(
            readiness["assignment_state"].as_str(),
            Some("working" | "submitted" | "accepted")
        ),
        "readiness may observe any authenticated checkpoint before acceptance: {readiness}"
    );
    assert_eq!(readiness["delivery"]["state"], "confirmed");
    assert_eq!(
        readiness["delivery"]["transport_state"],
        "submit-key-recovery-succeeded"
    );
    assert_eq!(
        readiness["delivery"]["proof"],
        "authenticated-worker-checkpoint"
    );
    assert_eq!(readiness["submit_key_recovery"]["eligible"], true);
    assert_eq!(readiness["submit_key_recovery"]["attempted"], true);
    assert_eq!(readiness["submit_key_recovery"]["attempt_count"], 1);
    assert_eq!(
        readiness["submit_key_recovery"]["result"],
        "checkpoint-confirmed"
    );
    let acceptance_deadline = Instant::now() + Duration::from_secs(10);
    while fs::metadata(&accept_output).map_or(true, |metadata| metadata.len() == 0)
        && Instant::now() < acceptance_deadline
    {
        std::thread::sleep(Duration::from_millis(10));
    }
    let acceptance: serde_json::Value =
        serde_json::from_slice(&fs::read(&accept_output).unwrap_or_else(|error| {
            panic!(
                "acceptance hook did not finish: {error}; checkpoint={}",
                fs::read_to_string(&checkpoint_output)
                    .unwrap_or_else(|_| "missing checkpoint output".to_string())
            )
        }))
        .expect("accept output json");
    assert_eq!(acceptance["ok"], true, "acceptance={acceptance}");
    let registry = orchestration_registry(&state_dir);
    assert_eq!(registry["assignments"][assignment_id]["state"], "accepted");
    assert_eq!(
        registry["assignments"][assignment_id]["submit_recovery"]["state"],
        "checkpoint_confirmed"
    );
    assert_eq!(
        registry["assignments"][assignment_id]["submit_recovery"]["origin"],
        "automatic"
    );
    let reconciled = run_main_agent(
        &checkout,
        &[
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "worker",
            "submit-recovery",
            assignment_id,
            "--if-revision",
            "6",
            "--timeout",
            "1s",
            "--idempotency-key",
            "reconcile-accepted-recovery-0001",
            "--format",
            "json",
        ],
        &[
            ("AGENT_SESSION_CAPABILITY_FILE", &main_capability),
            ("AGENT_SESSION_TMUX_BIN", &tmux_arg),
            ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
        ],
    );
    assert_eq!(reconciled.code, 0, "stderr={}", reconciled.stderr_text());
    assert_eq!(data(&reconciled)["checkpoint_confirmed"], true);
    let replay = run_main_agent(
        &checkout,
        &[
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "worker",
            "submit-recovery",
            assignment_id,
            "--if-revision",
            "6",
            "--timeout",
            "1s",
            "--idempotency-key",
            "reconcile-accepted-recovery-0001",
            "--format",
            "json",
        ],
        &[
            ("AGENT_SESSION_CAPABILITY_FILE", &main_capability),
            ("AGENT_SESSION_TMUX_BIN", &tmux_arg),
            ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
        ],
    );
    assert_eq!(data(&replay), data(&reconciled));
    let enter_calls = tmux_calls(&tmux_log)
        .into_iter()
        .filter(|call| {
            call.first().is_some_and(|arg| arg == "send-keys")
                && call.last().is_some_and(|arg| arg == "Enter")
        })
        .count();
    assert_eq!(
        enter_calls, 2,
        "initial submission plus exactly one recovery Enter"
    );
}

#[test]
fn main_agent_worker_start_reports_checkout_preclaim_failure_as_not_ready() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let checkout = tmp.path().join("checkout");
    let worker_checkout = tmp.path().join("worker-checkout");
    let declared_checkout = tmp.path().join("declared-checkout");
    fs::create_dir(&state_dir).expect("state");
    for path in [&checkout, &worker_checkout, &declared_checkout] {
        init_checkout(path, "https://example.invalid/example/repository.git");
    }
    seed_brokers_at(
        &state_dir,
        &[(
            "main-one",
            "main-incarnation-one",
            "main-private-capability-material-0000000001",
            checkout.as_path(),
            Some("enforce"),
        )],
    );
    let main_capability = init_main_run(tmp.path(), &state_dir, &checkout, "main-one", "run-one");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let codex_bin = fake_agent(tmp.path(), "codex-worker");
    let assignment_id = "assignment-checkout-preclaim";
    let worker_id = "worker-checkout-preclaim";
    let assignment_path = tmp.path().join("assignment-checkout-preclaim.json");
    write_private_json(
        &assignment_path,
        &json!({
            "schema_version": "main-agent.assignment-input.v1",
            "assignment_id": assignment_id,
            "task_summary": "Reject a mismatched checkout before readiness",
            "task": {},
            "launch": {
                "agent": "codex",
                "cwd": worker_checkout,
                "title": null,
                "session_id": worker_id,
                "coordination_mode": "enforce",
                "agent_args": []
            },
            "repository": "example/repository",
            "worktree": declared_checkout,
            "base_ref": "main",
            "scopes": ["crates/worker-lane"],
            "durable_refs": []
        }),
    );

    let bootstrap_digest = orchestration_request_digest(
        "main-agent-worker-bootstrap-idempotency",
        &json!(assignment_id),
    );
    let bootstrap_key = format!("bootstrap-{}", &bootstrap_digest[..32]);
    let enter_hook = tmp.path().join("bootstrap-mismatch-on-second-enter");
    let enter_hook_output = tmp.path().join("bootstrap-mismatch-on-second-enter.json");
    fs::write(
        &enter_hook,
        format!(
            r#"#!/usr/bin/env sh
set -u
capability_file=""
for candidate in {state_dir}/sessions/{worker_id}/coordination/capability-*; do
  [ -f "$candidate" ] || continue
  capability_file="$candidate"
  break
done
[ -n "$capability_file" ]
checkpoint_file=""
for candidate in {state_dir}/sessions/{worker_id}/coordination/main-agent-checkpoint-*; do
  [ -f "$candidate" ] || continue
  checkpoint_file="$candidate"
  break
done
[ -n "$checkpoint_file" ]
cd {worker_checkout}
set +e
AGENT_SESSION_CAPABILITY_FILE="$capability_file" \
AGENT_SESSION_CHECKPOINT_FILE="$checkpoint_file" \
  {main_agent} --state-dir {state_dir} bootstrap \
  --idempotency-key {bootstrap_key} --format json > {output}
status=$?
set -e
[ "$status" -eq 65 ]
grep -q worker-bootstrap-checkout-mismatch {output}
"#,
            state_dir = state_dir.display(),
            worker_checkout = worker_checkout.display(),
            worker_id = worker_id,
            main_agent = bin::resolve("main-agent").display(),
            bootstrap_key = bootstrap_key,
            output = enter_hook_output.display(),
        ),
    )
    .expect("enter hook");
    fs::set_permissions(&enter_hook, fs::Permissions::from_mode(0o700)).expect("enter hook mode");
    let enter_count = tmp.path().join("enter-count");

    let tmux_arg = tmux_bin.to_string_lossy().into_owned();
    let tmux_log_arg = tmux_log.to_string_lossy().into_owned();
    let codex_arg = codex_bin.to_string_lossy().into_owned();
    let enter_hook_arg = enter_hook.to_string_lossy().into_owned();
    let enter_count_arg = enter_count.to_string_lossy().into_owned();
    let codex_home = tmp.path().join("codex-home");
    let started = run_main_agent_with_codex_trust(
        &checkout,
        &[
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "worker",
            "start",
            "--assignment-file",
            assignment_path.to_str().expect("assignment path"),
            "--if-run-revision",
            "1",
            "--await-ready",
            "2s",
            "--idempotency-key",
            "worker-start-checkout-preclaim-0001",
            "--format",
            "json",
        ],
        &[
            ("AGENT_SESSION_CAPABILITY_FILE", &main_capability),
            ("AGENT_SESSION_TMUX_BIN", &tmux_arg),
            ("AGENT_SESSION_CODEX_BIN", &codex_arg),
            ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
            ("AGENT_SESSION_FAKE_TMUX_ENTER_HOOK", &enter_hook_arg),
            ("AGENT_SESSION_FAKE_TMUX_ENTER_COUNT_FILE", &enter_count_arg),
        ],
        &codex_home,
        &[&worker_checkout],
    );
    assert_eq!(started.code, 0, "stderr={}", started.stderr_text());
    let readiness = &data(&started)["readiness"];
    assert_eq!(readiness["state"], "readiness_failed");
    assert_eq!(readiness["classification"], "checkpoint_proof_failed");
    assert_eq!(readiness["assignment_state"], "blocked");
    assert_eq!(readiness["delivery"]["state"], "unverified");
    assert_eq!(
        readiness["delivery"]["transport_state"],
        "submit-key-recovery-succeeded"
    );
    assert_eq!(
        readiness["delivery"]["proof"],
        "worker-bootstrap-preclaim-failed"
    );
    assert_eq!(
        readiness["submit_key_recovery"]["result"],
        "worker-bootstrap-preclaim-failed"
    );
    assert_eq!(readiness["automatic_retry_safe"], false);

    let registry = orchestration_registry(&state_dir);
    let assignment = &registry["assignments"][assignment_id];
    assert_eq!(assignment["state"], "blocked");
    assert_eq!(
        assignment["blocker_summary"],
        "[pre-claim:worker-bootstrap-checkout-mismatch] worker bootstrap failed"
    );
    assert_eq!(assignment["submit_recovery"]["state"], "failed");
    assert_eq!(
        assignment["submit_recovery"]["result"],
        "worker-bootstrap-preclaim-failed"
    );
    let enter_calls = tmux_calls(&tmux_log)
        .into_iter()
        .filter(|call| {
            call.first().is_some_and(|arg| arg == "send-keys")
                && call.last().is_some_and(|arg| arg == "Enter")
        })
        .count();
    assert_eq!(
        enter_calls, 2,
        "initial submission plus exactly one recovery Enter"
    );
}

#[test]
fn main_agent_submit_recovery_is_bounded_idempotent_and_never_sends_a_second_enter() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let checkout = tmp.path().join("checkout");
    let worker_checkout = tmp.path().join("worker-checkout");
    fs::create_dir(&state_dir).expect("state");
    init_checkout(&checkout, "https://example.invalid/example/repository.git");
    init_checkout(
        &worker_checkout,
        "https://example.invalid/example/repository.git",
    );
    seed_brokers_at(
        &state_dir,
        &[(
            "main-one",
            "main-incarnation-one",
            "main-private-capability-material-0000000001",
            checkout.as_path(),
            Some("enforce"),
        )],
    );
    let main_capability = init_main_run(tmp.path(), &state_dir, &checkout, "main-one", "run-one");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let codex_bin = fake_agent(tmp.path(), "codex-worker");
    let assignment_path = tmp.path().join("assignment-submit-recovery.json");
    write_private_json(
        &assignment_path,
        &json!({
            "schema_version": "main-agent.assignment-input.v1",
            "assignment_id": "assignment-submit-recovery",
            "task_summary": "Guard one submit recovery",
            "task": {},
            "launch": {
                "agent": "codex",
                "cwd": worker_checkout,
                "title": null,
                "session_id": "worker-submit-recovery",
                "coordination_mode": "enforce",
                "agent_args": []
            },
            "repository": "example/repository",
            "worktree": worker_checkout,
            "base_ref": "main",
            "scopes": ["docs/submit-recovery"],
            "durable_refs": []
        }),
    );
    let tmux_arg = tmux_bin.to_string_lossy().into_owned();
    let tmux_log_arg = tmux_log.to_string_lossy().into_owned();
    let codex_arg = codex_bin.to_string_lossy().into_owned();
    let codex_home = tmp.path().join("codex-home");
    write_trusted_codex_config(&codex_home, &[&worker_checkout]);
    let codex_home_arg = codex_home.to_string_lossy().into_owned();
    let envs = [
        ("AGENT_SESSION_CAPABILITY_FILE", main_capability.as_str()),
        ("AGENT_SESSION_TMUX_BIN", tmux_arg.as_str()),
        ("AGENT_SESSION_CODEX_BIN", codex_arg.as_str()),
        ("AGENT_SESSION_FAKE_TMUX_LOG", tmux_log_arg.as_str()),
        ("CODEX_HOME", codex_home_arg.as_str()),
    ];
    let started = run_main_agent(
        &checkout,
        &[
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "worker",
            "start",
            "--assignment-file",
            assignment_path.to_str().expect("assignment path"),
            "--await-ready",
            "0",
            "--idempotency-key",
            "worker-start-submit-recovery-0001",
            "--format",
            "json",
        ],
        &envs,
    );
    assert_eq!(started.code, 0, "stderr={}", started.stderr_text());
    assert_eq!(data(&started)["assignment"]["revision"], 2);

    let recover_args = [
        "--state-dir",
        state_dir.to_str().expect("state dir"),
        "worker",
        "submit-recovery",
        "assignment-submit-recovery",
        "--if-revision",
        "2",
        "--timeout",
        "1s",
        "--idempotency-key",
        "submit-recovery-0001",
        "--format",
        "json",
    ];
    let owner_barrier = tmp.path().join("submit-recovery-owner-barrier");
    let contender_barrier = tmp.path().join("submit-recovery-contender-barrier");
    fs::create_dir(&owner_barrier).expect("owner barrier");
    fs::create_dir(&contender_barrier).expect("contender barrier");
    let spawn_recovery = |barrier: &Path, stage: &str| {
        Command::new(bin::resolve("main-agent"))
            .current_dir(&checkout)
            .args(recover_args)
            .envs(envs)
            .env(
                "NILS_AGENT_SESSION_TEST_SUBMIT_RECOVERY_BARRIER_STAGE",
                stage,
            )
            .env(
                "NILS_AGENT_SESSION_TEST_SUBMIT_RECOVERY_BARRIER_DIR",
                barrier,
            )
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn submit recovery contender")
    };
    let owner = spawn_recovery(&owner_barrier, "owner_reserved");
    let owner_deadline = Instant::now() + Duration::from_secs(10);
    while !owner_barrier.join("ready").is_file() {
        assert!(
            Instant::now() < owner_deadline,
            "submit recovery owner did not persist its reservation"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        orchestration_registry(&state_dir)["assignments"]["assignment-submit-recovery"]["submit_recovery"]
            ["state"],
        "attempting",
        "the barrier must prove the owner persisted the in-progress attempt"
    );
    let contender = spawn_recovery(&contender_barrier, "joined_in_progress");
    let contender_deadline = Instant::now() + Duration::from_secs(10);
    while !contender_barrier.join("ready").is_file() {
        assert!(
            Instant::now() < contender_deadline,
            "same-key contender did not join the persisted attempt"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    fs::write(owner_barrier.join("release"), b"continue").expect("release recovery owner");
    fs::write(contender_barrier.join("release"), b"continue").expect("release recovery contender");
    let recovered = owner.wait_with_output().expect("recovery owner output");
    let concurrent_replay = contender
        .wait_with_output()
        .expect("recovery contender output");
    assert!(
        recovered.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&recovered.stdout),
        String::from_utf8_lossy(&recovered.stderr)
    );
    assert!(
        concurrent_replay.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&concurrent_replay.stdout),
        String::from_utf8_lossy(&concurrent_replay.stderr)
    );
    let recovered: serde_json::Value =
        serde_json::from_slice(&recovered.stdout).expect("recovery owner json");
    let concurrent_replay: serde_json::Value =
        serde_json::from_slice(&concurrent_replay.stdout).expect("recovery contender json");
    assert_eq!(
        concurrent_replay["data"], recovered["data"],
        "same-key contenders must converge on one durable attempt"
    );
    assert_eq!(recovered["data"]["attempt_count"], 1);
    assert_eq!(recovered["data"]["checkpoint_confirmed"], false);
    assert_eq!(recovered["data"]["automatic_retry_safe"], false);
    assert_eq!(
        recovered["data"]["assignment"]["submit_recovery"]["state"],
        "failed"
    );
    assert_eq!(recovered["data"]["result"], "checkpoint-timeout");
    let enter_count = || {
        tmux_calls(&tmux_log)
            .iter()
            .filter(|call| {
                call.first().is_some_and(|arg| arg == "send-keys")
                    && call.last().is_some_and(|arg| arg == "Enter")
            })
            .count()
    };
    assert_eq!(
        enter_count(),
        2,
        "initial submission plus one guarded recovery Enter"
    );

    let replay = run_main_agent(&checkout, &recover_args, &envs);
    assert_eq!(replay.code, 0, "stderr={}", replay.stderr_text());
    assert_eq!(data(&replay), recovered["data"]);
    assert_eq!(enter_count(), 2, "idempotent replay cannot send Enter");

    let before_late_checkpoint = orchestration_registry(&state_dir);
    rewrite_orchestration_registry(&state_dir, |registry| {
        let assignment = &mut registry["assignments"]["assignment-submit-recovery"];
        let revision = assignment["revision"]
            .as_u64()
            .expect("assignment revision")
            + 1;
        assignment["revision"] = json!(revision);
        assignment["state"] = json!("working");
        assignment["checkpoint"] = json!({
            "revision": revision,
            "summary": "Late authenticated worker checkpoint",
            "next_action": "Continue",
            "updated_at": "2030-01-01T00:00:10Z"
        });
        assignment["updated_at"] = json!("2030-01-01T00:00:10Z");
    });
    let late_checkpoint_replay = run_main_agent(&checkout, &recover_args, &envs);
    assert_eq!(
        late_checkpoint_replay.code,
        0,
        "stderr={}",
        late_checkpoint_replay.stderr_text()
    );
    assert_eq!(data(&late_checkpoint_replay)["checkpoint_confirmed"], true);
    assert_eq!(
        data(&late_checkpoint_replay)["result"],
        "authenticated worker checkpoint confirmed"
    );
    assert_eq!(
        enter_count(),
        2,
        "upgrading a final negative receipt from a late checkpoint must never send another Enter"
    );
    rewrite_orchestration_registry(&state_dir, |registry| {
        *registry = before_late_checkpoint.clone();
    });

    // Simulate termination after automatic readiness recovery reserved its
    // one attempt but before the sender could record whether Enter was sent.
    rewrite_orchestration_registry(&state_dir, |registry| {
        let recovery =
            &mut registry["assignments"]["assignment-submit-recovery"]["submit_recovery"];
        recovery["origin"] = json!("automatic");
        recovery["state"] = json!("attempting");
        recovery["result"] = json!("recovery reserved before guarded input");
    });
    let automatic_reconciliation = run_main_agent(
        &checkout,
        &[
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "worker",
            "submit-recovery",
            "assignment-submit-recovery",
            "--if-revision",
            "3",
            "--timeout",
            "1s",
            "--idempotency-key",
            "submit-recovery-0003",
            "--format",
            "json",
        ],
        &envs,
    );
    assert_eq!(
        automatic_reconciliation.code,
        0,
        "stderr={}",
        automatic_reconciliation.stderr_text()
    );
    assert_eq!(
        data(&automatic_reconciliation)["result"],
        "submit-recovery-send-outcome-unknown"
    );
    assert_eq!(
        enter_count(),
        2,
        "reconciling an automatic reservation cannot send another Enter"
    );
    let automatic_replay = run_main_agent(
        &checkout,
        &[
            "--state-dir",
            state_dir.to_str().expect("state dir"),
            "worker",
            "submit-recovery",
            "assignment-submit-recovery",
            "--if-revision",
            "3",
            "--timeout",
            "1s",
            "--idempotency-key",
            "submit-recovery-0003",
            "--format",
            "json",
        ],
        &envs,
    );
    assert_eq!(
        data(&automatic_replay),
        data(&automatic_reconciliation),
        "automatic recovery reconciliation must be exactly replayable"
    );
}

#[test]
fn main_agent_quick_validates_before_launch() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let checkout = tmp.path().join("checkout");
    fs::create_dir(&state_dir).expect("state");
    init_checkout(&checkout, "https://example.invalid/example/repository.git");
    seed_brokers_at(
        &state_dir,
        &[(
            "main-one",
            "main-incarnation-one",
            "main-private-capability-material-0000000001",
            checkout.as_path(),
            Some("enforce"),
        )],
    );
    let main_capability = capability(&state_dir, "main-one");
    let state = state_dir.to_str().expect("state dir");

    let with_repo = tmp.path().join("with-repo.json");
    write_private_json(
        &with_repo,
        &json!({
            "schema_version": "main-agent.assignment-input.v1",
            "assignment_id": "assignment-quick",
            "task_summary": "Quick delegate",
            "task": {},
            "launch": {
                "agent": "codex", "cwd": checkout, "title": null, "session_id": null,
                "coordination_mode": "enforce", "agent_args": []
            },
            "repository": "example/repository", "worktree": null, "base_ref": "main",
            "scopes": ["crates/agent-session"], "durable_refs": []
        }),
    );

    // (a) An invalid tier is rejected before any run or claim work.
    let bad_tier = run_main_agent(
        &checkout,
        &[
            "--state-dir",
            state,
            "quick",
            "--assignment-file",
            with_repo.to_str().expect("with-repo"),
            "--tier",
            "L9",
            "--idempotency-key",
            "quick-0001",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_CAPABILITY_FILE", &main_capability)],
    );
    assert_ne!(bad_tier.code, 0);
    assert_eq!(
        bad_tier.stdout_json()["error"]["code"],
        "invalid-orchestration-input"
    );
    assert!(
        bad_tier.stdout_json()["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("tier")
    );

    // (b) A packet without a repository cannot synthesize an ephemeral run.
    let no_repo = tmp.path().join("no-repo.json");
    write_private_json(
        &no_repo,
        &json!({
            "schema_version": "main-agent.assignment-input.v1",
            "assignment_id": "assignment-quick",
            "task_summary": "Quick delegate",
            "task": {},
            "launch": {
                "agent": "codex", "cwd": checkout, "title": null, "session_id": null,
                "coordination_mode": "enforce", "agent_args": []
            },
            "repository": null, "worktree": null, "base_ref": "main",
            "scopes": ["crates/agent-session"], "durable_refs": []
        }),
    );
    let missing_repo = run_main_agent(
        &checkout,
        &[
            "--state-dir",
            state,
            "quick",
            "--assignment-file",
            no_repo.to_str().expect("no-repo"),
            "--idempotency-key",
            "quick-0002",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_CAPABILITY_FILE", &main_capability)],
    );
    assert_ne!(missing_repo.code, 0);
    assert_eq!(
        missing_repo.stdout_json()["error"]["code"],
        "invalid-orchestration-input"
    );
    assert!(
        missing_repo.stdout_json()["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("repository")
    );

    // (c) quick refuses when the session already controls a run.
    let _ = init_main_run(tmp.path(), &state_dir, &checkout, "main-one", "run-one");
    let exists = run_main_agent(
        &checkout,
        &[
            "--state-dir",
            state,
            "quick",
            "--assignment-file",
            with_repo.to_str().expect("with-repo"),
            "--idempotency-key",
            "quick-0003",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_CAPABILITY_FILE", &main_capability)],
    );
    assert_ne!(exists.code, 0);
    assert_eq!(exists.stdout_json()["error"]["code"], "quick-run-exists");
}

#[test]
fn main_agent_quick_idempotency_binds_the_canonical_readiness_wait() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let checkout = tmp.path().join("checkout");
    fs::create_dir(&state_dir).expect("state");
    init_checkout(&checkout, "https://example.invalid/example/repository.git");
    seed_brokers_at(
        &state_dir,
        &[(
            "main-one",
            "main-incarnation-one",
            "main-private-capability-material-0000000001",
            checkout.as_path(),
            Some("enforce"),
        )],
    );
    let main_capability = capability(&state_dir, "main-one");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let codex_bin = fake_agent(tmp.path(), "codex-worker");
    let assignment_path = tmp.path().join("quick-await.json");
    write_private_json(
        &assignment_path,
        &json!({
            "schema_version": "main-agent.assignment-input.v1",
            "assignment_id": "assignment-quick-await",
            "task_summary": "Bind quick readiness semantics",
            "task": {},
            "launch": {
                "agent": "codex",
                "cwd": checkout,
                "title": null,
                "session_id": "worker-quick-await",
                "coordination_mode": "enforce",
                "agent_args": []
            },
            "repository": "example/repository",
            "worktree": null,
            "base_ref": "main",
            "scopes": ["crates/agent-session"],
            "durable_refs": []
        }),
    );
    let codex_home = tmp.path().join("codex-home");
    let run = |await_ready: &str| {
        run_main_agent_with_codex_trust(
            &checkout,
            &[
                "--state-dir",
                state_dir.to_str().expect("state dir"),
                "quick",
                "--assignment-file",
                assignment_path.to_str().expect("assignment path"),
                "--tier",
                "L0",
                "--await-ready",
                await_ready,
                "--idempotency-key",
                "quick-await-contract-0001",
                "--format",
                "json",
            ],
            &[
                ("AGENT_SESSION_CAPABILITY_FILE", &main_capability),
                (
                    "AGENT_SESSION_TMUX_BIN",
                    tmux_bin.to_str().expect("tmux bin"),
                ),
                (
                    "AGENT_SESSION_CODEX_BIN",
                    codex_bin.to_str().expect("codex bin"),
                ),
                (
                    "AGENT_SESSION_FAKE_TMUX_LOG",
                    tmux_log.to_str().expect("tmux log"),
                ),
            ],
            &codex_home,
            &[&checkout],
        )
    };

    let launched = run("0");
    assert_eq!(launched.code, 0, "stderr={}", launched.stderr_text());
    let parent_receipt = "main-one:main-incarnation-one:quick-await-contract-0001";
    let pending_run = data(&launched)["run"].clone();
    rewrite_orchestration_registry(&state_dir, |registry| {
        registry["receipts"][parent_receipt]["outcome"] = json!({
            "schema_version": "main-agent.quick-pending.v1",
            "run": pending_run
        });
    });
    let equivalent_wait = run("0s");
    assert_eq!(
        equivalent_wait.code,
        0,
        "equivalent canonical wait must resume: {}",
        equivalent_wait.stdout_text()
    );
    assert_eq!(data(&equivalent_wait), data(&launched));
    let changed_wait = run("1s");
    assert_ne!(changed_wait.code, 0);
    assert_eq!(
        changed_wait.stdout_json()["error"]["code"],
        "idempotency-conflict"
    );
    assert_eq!(
        tmux_calls(&tmux_log)
            .iter()
            .filter(|call| call.first().is_some_and(|arg| arg == "new-session"))
            .count(),
        1,
        "conflicting quick replay must not launch another worker"
    );
}

#[test]
fn main_agent_worker_retire_rejects_non_terminal_and_missing() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let checkout = tmp.path().join("checkout");
    fs::create_dir(&state_dir).expect("state");
    init_checkout(&checkout, "https://example.invalid/example/repository.git");
    seed_brokers_at(
        &state_dir,
        &[
            (
                "main-one",
                "main-incarnation-one",
                "main-private-capability-material-0000000001",
                checkout.as_path(),
                Some("enforce"),
            ),
            (
                "worker-retire",
                "worker-retire-incarnation",
                "worker-retire-private-capability-0000000001",
                checkout.as_path(),
                Some("enforce"),
            ),
        ],
    );
    let main_capability = init_main_run(tmp.path(), &state_dir, &checkout, "main-one", "run-one");
    let private_packet = json!({
        "schema_version": "main-agent.assignment-input.v1",
        "assignment_id": "assignment-one",
        "task_summary": "Retire fixture",
        "task": {},
        "launch": {
            "agent": "codex", "cwd": checkout, "title": null, "session_id": null,
            "coordination_mode": "enforce", "agent_args": []
        },
        "repository": "example/repository", "worktree": null, "base_ref": "main",
        "scopes": ["crates/agent-session"], "durable_refs": []
    });
    insert_orchestration_assignment(
        &state_dir,
        "assignment-one",
        json!({
            "schema_version": "agent-session.orchestration-assignment.v1",
            "assignment_id": "assignment-one",
            "run_id": "run-one",
            "revision": 2,
            "state": "working",
            "task_summary": "Retire fixture",
            "private_packet_digest": "replaced-by-fixture",
            "primary_manager": {
                "session_id": "main-one",
                "session_incarnation": "main-incarnation-one",
                "session_created_at": "2030-01-01T00:00:00Z"
            },
            "worker": null,
            "collaborators": [],
            "borrowed_by": [],
            "repository": "example/repository",
            "worktree": null,
            "base_ref": "main",
            "scopes": ["crates/agent-session"],
            "durable_refs": [],
            "checkpoint": null,
            "result_summary": null,
            "blocker_summary": null,
            "created_at": "2030-01-01T00:00:01Z",
            "updated_at": "2030-01-01T00:00:02Z"
        }),
        &private_packet,
    );
    let state = state_dir.to_str().expect("state dir");

    // A still-working assignment cannot be retired (guard fires before teardown).
    let working = run_main_agent(
        &checkout,
        &[
            "--state-dir",
            state,
            "worker",
            "retire",
            "assignment-one",
            "--if-revision",
            "2",
            "--idempotency-key",
            "retire-0001",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_CAPABILITY_FILE", &main_capability)],
    );
    assert_ne!(working.code, 0);
    assert_eq!(
        working.stdout_json()["error"]["code"],
        "assignment-not-retireable"
    );

    rewrite_orchestration_registry(&state_dir, |registry| {
        let assignment = &mut registry["assignments"]["assignment-one"];
        assignment["state"] = json!("cancelled");
        assignment["revision"] = json!(3);
        assignment["worker"] = json!({
            "session_id": "worker-proven-absent",
            "session_incarnation": "worker-proven-absent-incarnation",
            "session_created_at": "2030-01-01T00:00:00Z"
        });
    });
    let absent = run_main_agent(
        &checkout,
        &[
            "--state-dir",
            state,
            "worker",
            "retire",
            "assignment-one",
            "--if-revision",
            "3",
            "--idempotency-key",
            "retire-absent-0001",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_CAPABILITY_FILE", &main_capability)],
    );
    assert_eq!(absent.code, 0, "stderr={}", absent.stderr_text());
    assert_eq!(data(&absent)["retired"], true);
    assert_eq!(data(&absent)["deleted"], true);
    assert_eq!(data(&absent)["cleanup_pending"], false);

    // An accepted assignment can resume the same retire request after release
    // commits and deletion fails. The original revision remains attached to
    // the top-level progress receipt rather than being re-applied to the
    // already-released assignment.
    rewrite_orchestration_registry(&state_dir, |registry| {
        let assignment = &mut registry["assignments"]["assignment-one"];
        assignment["state"] = json!("accepted");
        assignment["revision"] = json!(5);
        assignment["worker"] = json!({
            "session_id": "worker-retire",
            "session_incarnation": "worker-retire-incarnation",
            "session_created_at": "2030-01-01T00:00:00Z"
        });
    });
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let tmux_arg = tmux_bin.to_string_lossy().into_owned();
    let tmux_log_arg = tmux_log.to_string_lossy().into_owned();
    let retire_args = [
        "--state-dir",
        state,
        "worker",
        "retire",
        "assignment-one",
        "--if-revision",
        "5",
        "--idempotency-key",
        "retire-resume-0001",
        "--format",
        "json",
    ];
    let failed = run_main_agent(
        &checkout,
        &retire_args,
        &[
            ("AGENT_SESSION_CAPABILITY_FILE", &main_capability),
            ("AGENT_SESSION_TMUX_BIN", &tmux_arg),
            ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
            ("AGENT_SESSION_FAKE_TMUX_FAIL", "kill-session"),
        ],
    );
    assert_ne!(failed.code, 0, "outcome={}", failed.stdout_text());
    let after_release = orchestration_registry(&state_dir);
    assert_eq!(
        after_release["assignments"]["assignment-one"]["state"],
        "released"
    );
    assert_eq!(
        after_release["assignments"]["assignment-one"]["revision"],
        6
    );
    fs::remove_dir_all(state_dir.join("sessions/worker-retire"))
        .expect("simulate confirmed absence before retire retry");
    let resumed = run_main_agent(
        &checkout,
        &retire_args,
        &[
            ("AGENT_SESSION_CAPABILITY_FILE", &main_capability),
            ("AGENT_SESSION_TMUX_BIN", &tmux_arg),
            ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
        ],
    );
    assert_eq!(resumed.code, 0, "outcome={}", resumed.stdout_text());
    assert_eq!(data(&resumed)["released"], true);
    assert_eq!(data(&resumed)["retired"], true);
    let replay = run_main_agent(
        &checkout,
        &retire_args,
        &[("AGENT_SESSION_CAPABILITY_FILE", &main_capability)],
    );
    assert_eq!(replay.code, 0, "outcome={}", replay.stdout_text());
    assert_eq!(data(&replay), data(&resumed));

    // A rolling-upgrade retry can encounter the historical release child
    // receipt after the assignment transition committed but before the old
    // parent retire receipt was persisted. Adopt that exact child authority
    // instead of reapplying the stale top-level revision.
    insert_orchestration_assignment(
        &state_dir,
        "assignment-one",
        json!({
            "schema_version": "agent-session.orchestration-assignment.v1",
            "assignment_id": "assignment-one",
            "run_id": "run-one",
            "revision": 11,
            "state": "released",
            "task_summary": "Retire fixture",
            "private_packet_digest": "replaced-by-fixture",
            "primary_manager": {
                "session_id": "main-one",
                "session_incarnation": "main-incarnation-one",
                "session_created_at": "2030-01-01T00:00:00Z"
            },
            "worker": {
                "session_id": "worker-historical-release-proven-absent",
                "session_incarnation": "worker-historical-release-incarnation",
                "session_created_at": "2030-01-01T00:00:00Z"
            },
            "collaborators": [],
            "borrowed_by": [],
            "repository": "example/repository",
            "worktree": null,
            "base_ref": "main",
            "scopes": ["crates/agent-session"],
            "durable_refs": [],
            "checkpoint": null,
            "result_summary": null,
            "blocker_summary": null,
            "created_at": "2030-01-01T00:00:01Z",
            "updated_at": "2030-01-01T00:00:11Z"
        }),
        &private_packet,
    );
    rewrite_orchestration_registry(&state_dir, |registry| {
        let parent_key = "retire-historical-release-0001";
        let child_key = format!("{parent_key}-release");
        let request = json!({
            "assignment_id": "assignment-one",
            "if_revision": 10
        });
        registry["receipts"][format!("main-one:main-incarnation-one:{child_key}")] = json!({
            "principal_session_id": "main-one",
            "principal_incarnation": "main-incarnation-one",
            "operation": "worker-release",
            "request_digest": orchestration_request_digest("worker-release", &request),
            "outcome": {
                "schema_version": "main-agent.assignment-mutation-result.v1",
                "assignment": {
                    "assignment_id": "assignment-one",
                    "revision": 11,
                    "state": "released"
                }
            },
            "created_at_epoch": 1
        });
    });
    let historical_release = run_main_agent(
        &checkout,
        &[
            "--state-dir",
            state,
            "worker",
            "retire",
            "assignment-one",
            "--if-revision",
            "10",
            "--idempotency-key",
            "retire-historical-release-0001",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_CAPABILITY_FILE", &main_capability)],
    );
    assert_eq!(
        historical_release.code,
        0,
        "outcome={}",
        historical_release.stdout_text()
    );
    assert_eq!(data(&historical_release)["released"], true);
    assert_eq!(data(&historical_release)["retired"], true);

    // A missing assignment is a clean not-found.
    let missing = run_main_agent(
        &checkout,
        &[
            "--state-dir",
            state,
            "worker",
            "retire",
            "missing-assignment",
            "--if-revision",
            "1",
            "--idempotency-key",
            "retire-0002",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_CAPABILITY_FILE", &main_capability)],
    );
    assert_ne!(missing.code, 0);
    assert_eq!(
        missing.stdout_json()["error"]["code"],
        "assignment-not-found"
    );
}

// F4: the objective-packet schema is discoverable via `main-agent packet-schema`
// (an example naming both nested schema_version constants), and a schema
// mismatch names the expected version and points back at the printer.
#[test]
fn main_agent_capabilities_exposes_the_runtime_checkpoint_contract() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let output = run_resolved(
        "main-agent",
        &["capabilities", "--provider", "codex", "--format", "json"],
        &CmdOptions::new()
            .with_cwd(tmp.path())
            .with_env_remove_many(&[
                "HOME",
                "XDG_STATE_HOME",
                "AGENT_SESSION_STATE_DIR",
                "AGENT_SESSION_HOST",
            ]),
    );
    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    assert_eq!(
        data(&output)["schema_version"],
        "main-agent.capabilities.v1"
    );
    assert_eq!(
        data(&output)["capabilities"]["runtime_checkpoint_file"],
        "main-agent.runtime-checkpoint-file.v1"
    );
}

#[test]
fn main_agent_self_readiness_is_bound_to_the_exact_runtime_checkpoint() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let checkout = tmp.path().join("checkout");
    fs::create_dir(&state_dir).expect("state");
    init_checkout(&checkout, "https://example.invalid/example/repository.git");
    seed_brokers_at(
        &state_dir,
        &[(
            "main-ready",
            "main-ready-incarnation",
            "main-ready-private-capability-material-0001",
            checkout.as_path(),
            Some("enforce"),
        )],
    );
    let capability_file = capability(&state_dir, "main-ready");
    let checkpoint_file = state_dir.join(format!(
        "sessions/main-ready/coordination/main-agent-checkpoint-{}.json",
        digest("main-ready-incarnation")
    ));
    let args = [
        "--state-dir",
        state_dir.to_str().expect("state"),
        "self",
        "readiness",
        "--format",
        "json",
    ];

    let missing = run_main_agent_without_checkpoint(
        &checkout,
        &args,
        &[("AGENT_SESSION_CAPABILITY_FILE", &capability_file)],
    );
    assert_eq!(
        missing.stdout_json()["error"]["code"],
        "runtime-checkpoint-unavailable"
    );

    let wrong_checkpoint = tmp.path().join("wrong-checkpoint.json");
    let mismatched = run_main_agent(
        &checkout,
        &args,
        &[
            ("AGENT_SESSION_CAPABILITY_FILE", &capability_file),
            (
                "AGENT_SESSION_CHECKPOINT_FILE",
                wrong_checkpoint.to_str().expect("wrong checkpoint"),
            ),
        ],
    );
    assert_eq!(
        mismatched.stdout_json()["error"]["code"],
        "runtime-checkpoint-unavailable"
    );

    let ready = run_main_agent(
        &checkout,
        &args,
        &[
            ("AGENT_SESSION_CAPABILITY_FILE", &capability_file),
            (
                "AGENT_SESSION_CHECKPOINT_FILE",
                checkpoint_file.to_str().expect("checkpoint"),
            ),
        ],
    );
    assert_eq!(ready.code, 0, "stdout={}", ready.stdout_text());
    assert_eq!(data(&ready)["ready"], true);
    assert_eq!(
        data(&ready)["checkpoint_file"],
        checkpoint_file.to_string_lossy().as_ref()
    );
}

#[test]
fn main_agent_packet_schema_prints_the_example_packet() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    fs::create_dir(&state_dir).expect("state");
    let state_arg = state_dir.to_str().expect("state path").to_string();
    let output = run_main_agent(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "packet-schema",
            "--format",
            "json",
        ],
        &[],
    );
    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    assert_eq!(
        data(&output)["schema_version"],
        "main-agent.objective-packet.v1"
    );
    assert_eq!(
        data(&output)["work_context"]["schema_version"],
        "agent-session.work-context-input.v1"
    );
    assert_eq!(
        data(&output)["work_context"]["repositories"][0],
        "owner/name"
    );
}

#[test]
fn main_agent_init_unsupported_schema_names_expected_version_and_hints_packet_schema() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    fs::create_dir(&state_dir).expect("state");
    let state_arg = state_dir.to_str().expect("state path").to_string();
    let packet = tmp.path().join("objective.json");
    fs::write(
        &packet,
        serde_json::to_vec(&json!({
            "schema_version": "main-agent.objective-packet.v0",
            "tier": "L0",
            "objective_summary": "demo",
            "work_context": {
                "schema_version": "agent-session.work-context-input.v1",
                "intent": "implementation",
                "tier": "L0",
                "repositories": ["owner/name"],
                "summary": "demo"
            }
        }))
        .expect("packet json"),
    )
    .expect("write packet");
    fs::set_permissions(&packet, fs::Permissions::from_mode(0o600)).expect("packet mode");
    let output = run_main_agent(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "init",
            "--packet-file",
            packet.to_str().expect("packet path"),
            "--if-absent",
            "--idempotency-key",
            "init-bad-schema-0001",
            "--format",
            "json",
        ],
        &[],
    );
    assert_ne!(output.code, 0);
    let error = output.stdout_json();
    let error = &error["error"];
    assert_eq!(error["code"], "invalid-orchestration-input");
    assert!(
        error["message"]
            .as_str()
            .expect("message")
            .contains("main-agent.objective-packet.v1"),
        "message must name the expected schema_version: {error}"
    );
    assert!(
        error["hint"]
            .as_str()
            .expect("hint")
            .contains("packet-schema"),
        "hint must point at packet-schema: {error}"
    );
}

// F3: a `worker start` parse error names the actual missing argument instead of
// collapsing to an unnamed "required arguments were not provided" line.
#[test]
fn main_agent_worker_start_parse_error_names_the_missing_idempotency_key() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    fs::create_dir(&state_dir).expect("state");
    let state_arg = state_dir.to_str().expect("state path").to_string();
    let output = run_main_agent(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "worker",
            "start",
            "--assignment-file",
            "/nonexistent/assignment.json",
            "--if-run-revision",
            "1",
            "--format",
            "json",
        ],
        &[],
    );
    assert_ne!(output.code, 0);
    let error = output.stdout_json();
    let error = &error["error"];
    assert_eq!(error["code"], "parse-error");
    assert!(
        error["message"]
            .as_str()
            .expect("message")
            .contains("--idempotency-key"),
        "parse error must name the missing --idempotency-key: {error}"
    );
}

#[test]
fn main_agent_account_handoff_parse_errors_honor_equals_json_format() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    fs::create_dir(&state_dir).expect("state");
    let state_arg = state_dir.to_str().expect("state path").to_string();
    for command in ["account-handoff", "account-handoff-cancel"] {
        let mut args = vec![
            "--state-dir",
            &state_arg,
            "worker",
            command,
            "assignment-1",
            "--if-revision",
            "1",
            "--idempotency-key",
            "parse-error-equals-json",
            "--format=json",
        ];
        if command == "account-handoff-cancel" {
            args.extend(["--reservation-id", "reservation-1"]);
        }
        let output = run_main_agent(tmp.path(), &args, &[]);
        assert_eq!(output.code, 64, "{command}: {}", output.stderr_text());
        assert_eq!(output.stderr_text(), "", "{command} must keep stderr empty");
        let error = output.stdout_json();
        assert_eq!(error["schema_version"], "cli.main-agent.error.v1");
        assert_eq!(error["ok"], false);
        assert_eq!(error["error"]["code"], "parse-error");
        assert!(
            error["error"]["message"]
                .as_str()
                .unwrap_or_default()
                .contains("--account"),
            "{command} must name the missing account: {error}"
        );
    }
}

// F1: `quick` no longer requires --idempotency-key (it defaults from a digest of
// the assignment packet), so clap only reports the still-required
// --assignment-file when both are omitted.
#[test]
fn main_agent_quick_no_longer_requires_an_idempotency_key() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    fs::create_dir(&state_dir).expect("state");
    let state_arg = state_dir.to_str().expect("state path").to_string();
    let output = run_main_agent(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "quick",
            "--tier",
            "L0",
            "--format",
            "json",
        ],
        &[],
    );
    assert_ne!(output.code, 0);
    let error = output.stdout_json();
    let error = &error["error"];
    assert_eq!(error["code"], "parse-error");
    let message = error["message"].as_str().expect("message");
    assert!(
        message.contains("--assignment-file"),
        "must still require --assignment-file: {error}"
    );
    assert!(
        !message.contains("--idempotency-key"),
        "must no longer require --idempotency-key: {error}"
    );
}
