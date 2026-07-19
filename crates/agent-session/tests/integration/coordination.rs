use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};

use nils_test_support::bin;
use nils_test_support::cmd::{CmdOptions, CmdOutput, run_resolved};
use pretty_assertions::assert_eq;
use serde_json::json;
use sha2::{Digest, Sha256};

fn run(dir: &Path, args: &[&str]) -> CmdOutput {
    run_resolved("agent-session", args, &CmdOptions::new().with_cwd(dir))
}

fn seed_session(state_dir: &Path, id: &str, incarnation: &str) {
    let session_dir = state_dir.join("sessions").join(id);
    fs::create_dir_all(&session_dir).expect("session directory");
    fs::set_permissions(state_dir, fs::Permissions::from_mode(0o700)).expect("state mode");
    fs::set_permissions(
        state_dir.join("sessions"),
        fs::Permissions::from_mode(0o700),
    )
    .expect("sessions mode");
    fs::set_permissions(&session_dir, fs::Permissions::from_mode(0o700)).expect("session mode");
    let record = json!({
        "schema_version": "agent-session.session.v1",
        "id": id,
        "agent": "codex",
        "mode": "interactive",
        "title": "coordination fixture",
        "title_revision": 0,
        "cwd": "/fixture/repository",
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
    let path = session_dir.join("session.json");
    fs::write(
        &path,
        serde_json::to_vec_pretty(&record).expect("session json"),
    )
    .expect("write session");
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).expect("record mode");
}

fn digest(value: &str) -> String {
    Sha256::digest(value.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn seed_brokers(state_dir: &Path, sessions: &[(&str, &str, &str)]) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs();
    let mut brokers = serde_json::Map::new();
    for (id, incarnation, capability) in sessions {
        seed_session(state_dir, id, incarnation);
        let capability_dir = state_dir.join("sessions").join(id).join("coordination");
        fs::create_dir(&capability_dir).expect("capability directory");
        fs::set_permissions(&capability_dir, fs::Permissions::from_mode(0o700))
            .expect("capability dir mode");
        let capability_path = capability_dir.join("capability");
        fs::write(&capability_path, capability).expect("capability");
        fs::set_permissions(&capability_path, fs::Permissions::from_mode(0o600))
            .expect("capability mode");
        brokers.insert(
            (*id).to_string(),
            json!({
                "session_id": id,
                "incarnation": incarnation,
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
    state_dir
        .join("sessions")
        .join(id)
        .join("coordination/capability")
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
                "value": prefix
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
            "--execution-token",
            "execution-token-alpha",
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
