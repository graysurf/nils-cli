use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use nils_test_support::bin;
use nils_test_support::cmd::{CmdOptions, CmdOutput, run_resolved};
use pretty_assertions::assert_eq;
use serde_json::json;
use sha2::{Digest, Sha256};

use super::cli::{fake_agent, fake_tmux, tmux_calls};

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

fn run_main_agent(dir: &Path, args: &[&str], envs: &[(&str, &str)]) -> CmdOutput {
    run_resolved(
        "main-agent",
        args,
        &CmdOptions::new().with_cwd(dir).with_envs(envs),
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
        let capability_dir = state_dir.join("sessions").join(id).join("coordination");
        fs::create_dir(&capability_dir).expect("capability directory");
        fs::set_permissions(&capability_dir, fs::Permissions::from_mode(0o700))
            .expect("capability dir mode");
        let capability_path = capability_dir.join(format!("capability-{}", digest(incarnation)));
        fs::write(&capability_path, capability).expect("capability");
        fs::set_permissions(&capability_path, fs::Permissions::from_mode(0o600))
            .expect("capability mode");
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
}

#[test]
fn main_agent_help_documents_safe_lifecycle_revision_fences_and_retry_keys() {
    let tmp = tempfile::tempdir().expect("tempdir");

    let root = run_main_agent(tmp.path(), &["--help"], &[]);
    assert_eq!(root.code, 0, "stderr={}", root.stderr_text());
    let root_help = root.stdout_text();
    for lifecycle_step in [
        "SAFE LIFECYCLE",
        "init -> rehydrate/status -> worker start -> worker self/checkpoint",
        "accept -> release -> delete -> close",
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
            &["worker", "start", "--help"][..],
            &["expected current run revision", "same idempotency key"][..],
        ),
        (
            &["worker", "accept", "--help"][..],
            &[
                "expected current assignment revision",
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
    let recovered = run(
        tmp.path(),
        &[
            "--state-dir",
            state_dir.to_str().expect("state"),
            "broker",
            "adopt",
            "--session",
            "alpha",
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
}

#[test]
fn main_agent_worker_start_replay_converges_a_persisted_start_without_duplicate_launch() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let checkout = tmp.path().join("checkout");
    fs::create_dir(&state_dir).expect("state");
    init_checkout(&checkout, "https://example.invalid/example/repository.git");
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
    let worker_prompt = state_dir.join(format!("sessions/{worker_id}/prompt.md"));
    fs::write(
        &worker_prompt,
        format!(
            "You are a managed worker for assignment {assignment_id}. Run `main-agent self show --format json`, then checkpoint state `working` before mutations."
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
    rewrite_orchestration_registry(&state_dir, |registry| {
        registry["receipts"][format!("main-one:main-incarnation-one:{idempotency_key}")] = json!({
            "principal_session_id": "main-one",
            "principal_incarnation": "main-incarnation-one",
            "operation": "worker-start",
            "request_digest": request_digest,
            "outcome": {
                "schema_version": "main-agent.worker-start-result.v1",
                "assignment_id": assignment_id,
                "state": "starting",
                "acceptance": "pending"
            },
            "created_at_epoch": 1
        });
    });
    let assignment_path = tmp.path().join("assignment.json");
    write_private_json(&assignment_path, &assignment_input);
    let args = [
        "--state-dir",
        state_dir.to_str().expect("state dir"),
        "worker",
        "start",
        "--assignment-file",
        assignment_path.to_str().expect("assignment path"),
        "--if-run-revision",
        "1",
        "--idempotency-key",
        idempotency_key,
        "--format",
        "json",
    ];
    let resumed = run_main_agent(
        &checkout,
        &args,
        &[("AGENT_SESSION_CAPABILITY_FILE", &main_capability)],
    );
    assert_eq!(resumed.code, 0, "stderr={}", resumed.stderr_text());
    assert_eq!(data(&resumed)["assignment"]["assignment_id"], assignment_id);
    assert_eq!(data(&resumed)["assignment"]["revision"], 2);
    assert_eq!(data(&resumed)["worker"]["session_id"], worker_id);
    assert_eq!(
        data(&resumed)["acceptance"]["state"],
        "pending-worker-checkpoint"
    );

    let replay = run_main_agent(
        &checkout,
        &args,
        &[("AGENT_SESSION_CAPABILITY_FILE", &main_capability)],
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
    let started = run_main_agent(
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
    for assignment_id in ["handoff-one", "adopt-one"] {
        insert_orchestration_assignment(
            &state_dir,
            assignment_id,
            json!({
                "schema_version": "agent-session.orchestration-assignment.v1",
                "assignment_id": assignment_id,
                "run_id": "run-one",
                "revision": 1,
                "state": "assigned",
                "task_summary": "Exercise relationship lifecycle",
                "private_packet_digest": "replaced-by-fixture",
                "primary_manager": main_one_controller,
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
