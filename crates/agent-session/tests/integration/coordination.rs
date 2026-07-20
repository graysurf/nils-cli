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

fn run_with_env(dir: &Path, args: &[&str], envs: &[(&str, &str)]) -> CmdOutput {
    run_resolved(
        "agent-session",
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
