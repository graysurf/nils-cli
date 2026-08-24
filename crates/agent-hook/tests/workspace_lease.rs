mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use pretty_assertions::{assert_eq, assert_ne};
use serde_json::{Value, json};

use support::Fixture;

const POLICY: &str = r#"schema_version = "agent-hook.policy.v1"
bundle_id = "workspace-lease-test"
version = "2026.08.24.1"
"#;

fn fixture() -> Fixture {
    let fixture = Fixture::new(POLICY);
    git(&fixture.root, &["init", "--quiet"]);
    git(
        &fixture.root,
        &["config", "user.email", "workspace@example.com"],
    );
    git(&fixture.root, &["config", "user.name", "Workspace Test"]);
    fs::write(
        fixture.root.join(".gitignore"),
        "/config/\n/data/\n/home/\n/state/\n/session-state/\n/linked/\n",
    )
    .expect("fixture ignores");
    fs::write(fixture.root.join("tracked.txt"), "base\n").expect("tracked file");
    git(&fixture.root, &["add", "--all"]);
    git(&fixture.root, &["commit", "--quiet", "-m", "test: initial"]);
    fixture
}

fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .expect("git command");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn invoke(fixture: &Fixture, operation: &str, request: Value) -> (i32, Value) {
    let output = fixture.run(
        &["workspace-lease", operation, "--format", "json"],
        Some(&request.to_string()),
    );
    (output.code, output.stdout_json())
}

fn bind_request(session: &str, request_id: &str, cwd: &Path) -> Value {
    json!({
        "schema_version": "agent-hook.workspace-lease.bind.v1",
        "version": 1,
        "request_id": request_id,
        "session_id": session,
        "cwd": cwd,
        "source": "startup"
    })
}

fn bind(fixture: &Fixture, session: &str, request_id: &str, cwd: &Path) -> Value {
    let (code, envelope) = invoke(fixture, "bind", bind_request(session, request_id, cwd));
    assert_eq!(code, 0, "envelope={envelope}");
    envelope["data"].clone()
}

fn begin_request(binding: &Value, session: &str, request_id: &str, tool: &str) -> Value {
    json!({
        "schema_version": "agent-hook.workspace-lease.begin.v1",
        "version": 1,
        "request_id": request_id,
        "session_id": session,
        "binding_id": binding["binding_id"],
        "workspace_id": binding["workspace_id"],
        "generation": binding["generation"],
        "binding_state": binding["state"],
        "call_id": format!("call:{request_id}"),
        "root_call_id": format!("root:{request_id}"),
        "tool_name": tool,
        "arguments": {"path": "tracked.txt", "content": "next"},
        "nested": false
    })
}

fn complete_request(binding: &Value, operation: &Value, session: &str, request_id: &str) -> Value {
    json!({
        "schema_version": "agent-hook.workspace-lease.complete.v1",
        "version": 1,
        "request_id": request_id,
        "session_id": session,
        "binding_id": binding["binding_id"],
        "workspace_id": binding["workspace_id"],
        "generation": binding["generation"],
        "operation_id": operation["operation_id"],
        "fence": operation["fence"],
        "call_id": "call:begin-1",
        "root_call_id": "root:begin-1",
        "tool_name": "write",
        "outcome": "succeeded"
    })
}

#[test]
fn canonical_binding_converges_path_spellings_but_distinguishes_linked_worktrees() {
    let fixture = fixture();
    let dotted = fixture.root.join("subdir/..");
    fs::create_dir_all(fixture.root.join("subdir")).expect("subdir");

    let first = bind(&fixture, "session-a", "bind-a", &dotted);
    assert_eq!(
        first["schema_version"],
        "agent-hook.workspace-lease.bind-result.v1"
    );
    assert_eq!(first["kind"], "bound");
    assert_eq!(first["state"], "owned");
    assert_eq!(first["renew_after_ms"], 10_000);

    let replay = bind(&fixture, "session-a", "bind-a", &fixture.root);
    assert_eq!(
        replay, first,
        "the exact request id must replay after canonicalization"
    );

    let linked = fixture.root.join("linked");
    git(
        &fixture.root,
        &[
            "worktree",
            "add",
            "--quiet",
            "--detach",
            linked.to_str().expect("linked UTF-8"),
            "HEAD",
        ],
    );
    let second = bind(&fixture, "session-b", "bind-b", &linked);
    assert_eq!(second["kind"], "bound");
    assert_ne!(second["workspace_id"], first["workspace_id"]);
    assert!(
        !second
            .to_string()
            .contains(fixture.root.to_str().expect("root UTF-8"))
    );
}

#[test]
fn cross_process_contention_release_and_duplicate_lifecycle_are_fenced() {
    let fixture = fixture();
    let owner = bind(&fixture, "session-owner", "bind-owner", &fixture.root);

    let denied = bind(&fixture, "session-peer", "bind-peer", &fixture.root);
    assert_eq!(denied["kind"], "denied");
    assert_eq!(denied["state"], "foreign-active");
    assert_eq!(denied["code"], "WORKSPACE_FOREIGN_ACTIVE");

    let first_begin_request = begin_request(&owner, "session-owner", "begin-1", "write");
    let (code, first_begin) = invoke(&fixture, "begin", first_begin_request.clone());
    assert_eq!(code, 0, "envelope={first_begin}");
    assert_eq!(first_begin["data"]["kind"], "granted");
    let (code, replayed_begin) = invoke(&fixture, "begin", first_begin_request);
    assert_eq!(code, 0, "envelope={replayed_begin}");
    assert_eq!(replayed_begin["data"], first_begin["data"]);

    let operation = &first_begin["data"];
    let completion = complete_request(&owner, operation, "session-owner", "complete-1");
    let (code, first_complete) = invoke(&fixture, "complete", completion.clone());
    assert_eq!(code, 0, "envelope={first_complete}");
    assert_eq!(first_complete["data"]["kind"], "completed");
    let (code, duplicate_complete) = invoke(&fixture, "complete", completion);
    assert_eq!(code, 0, "envelope={duplicate_complete}");
    assert_eq!(duplicate_complete["data"]["kind"], "duplicate");

    let release = json!({
        "schema_version": "agent-hook.workspace-lease.release.v1",
        "version": 1,
        "request_id": "release-1",
        "session_id": "session-owner",
        "binding_id": owner["binding_id"],
        "workspace_id": owner["workspace_id"],
        "generation": owner["generation"],
        "reason": "agent-disposed"
    });
    let (code, first_release) = invoke(&fixture, "release", release.clone());
    assert_eq!(code, 0, "envelope={first_release}");
    assert_eq!(first_release["data"]["kind"], "released");
    let (code, duplicate_release) = invoke(&fixture, "release", release);
    assert_eq!(code, 0, "envelope={duplicate_release}");
    assert_eq!(duplicate_release["data"]["kind"], "duplicate");

    let peer = bind(&fixture, "session-peer", "bind-peer-2", &fixture.root);
    assert_eq!(peer["kind"], "bound");
    assert_ne!(peer["generation"], owner["generation"]);

    let stale = begin_request(&owner, "session-owner", "stale-begin", "write");
    let (code, envelope) = invoke(&fixture, "begin", stale);
    assert_eq!(code, 0, "envelope={envelope}");
    assert_eq!(envelope["data"]["kind"], "denied");
    assert_eq!(envelope["data"]["state"], "foreign-active");
}

#[test]
fn simultaneous_processes_publish_exactly_one_workspace_owner() {
    let fixture = fixture();
    let (first, second) = std::thread::scope(|scope| {
        let first = scope.spawn(|| bind(&fixture, "session-a", "bind-a", &fixture.root));
        let second = scope.spawn(|| bind(&fixture, "session-b", "bind-b", &fixture.root));
        (
            first.join().expect("first bind process"),
            second.join().expect("second bind process"),
        )
    });
    let mut kinds = [
        first["kind"].as_str().expect("first kind"),
        second["kind"].as_str().expect("second kind"),
    ];
    kinds.sort_unstable();
    assert_eq!(kinds, ["bound", "denied"]);
    let denial = if first["kind"] == "denied" {
        first
    } else {
        second
    };
    assert_eq!(denial["state"], "foreign-active");
}

#[test]
fn dirty_state_refuses_takeover_and_clean_expiry_recovers_with_a_new_generation() {
    let fixture = fixture();
    fs::write(fixture.root.join("dirty.txt"), "dirty\n").expect("dirty file");
    let dirty = bind(&fixture, "session-a", "dirty-bind", &fixture.root);
    assert_eq!(dirty["kind"], "denied");
    assert_eq!(dirty["state"], "dirty");
    assert_eq!(dirty["code"], "WORKSPACE_DIRTY");

    fs::remove_file(fixture.root.join("dirty.txt")).expect("clean checkout");
    let owner = bind(&fixture, "session-a", "clean-bind", &fixture.root);
    assert_eq!(owner["kind"], "bound");

    let state = workspace_state_file(&fixture);
    let mut persisted: Value =
        serde_json::from_slice(&fs::read(&state).expect("state bytes")).expect("state JSON");
    persisted["binding"]["refreshed_at_epoch"] = json!(1);
    persisted["binding"]["expires_at_epoch"] = json!(1);
    fs::write(
        &state,
        serde_json::to_vec_pretty(&persisted).expect("state render"),
    )
    .expect("expire state");
    Fixture::set_private(&state);

    let recovered = bind(&fixture, "session-b", "recover-bind", &fixture.root);
    assert_eq!(recovered["kind"], "bound");
    assert_ne!(recovered["generation"], owner["generation"]);
    assert_ne!(recovered["binding_id"], owner["binding_id"]);

    fs::write(fixture.root.join("dirty-again.txt"), "dirty\n").expect("dirty again");
    let state = workspace_state_file(&fixture);
    let mut persisted: Value =
        serde_json::from_slice(&fs::read(&state).expect("state bytes")).expect("state JSON");
    persisted["binding"]["refreshed_at_epoch"] = json!(1);
    persisted["binding"]["expires_at_epoch"] = json!(1);
    fs::write(
        &state,
        serde_json::to_vec_pretty(&persisted).expect("state render"),
    )
    .expect("expire dirty state");
    Fixture::set_private(&state);

    let refused = bind(&fixture, "session-c", "dirty-takeover", &fixture.root);
    assert_eq!(refused["kind"], "denied");
    assert_eq!(refused["state"], "dirty");
}

#[test]
fn read_only_tools_need_no_operation_and_copied_fences_fail_closed() {
    let fixture = fixture();
    let owner = bind(&fixture, "session-owner", "bind-owner", &fixture.root);

    let read = begin_request(&owner, "session-owner", "read-1", "read");
    let (code, envelope) = invoke(&fixture, "begin", read);
    assert_eq!(code, 0, "envelope={envelope}");
    assert_eq!(envelope["data"]["kind"], "not-required");

    let mutation = begin_request(&owner, "session-owner", "begin-1", "write");
    let (code, granted) = invoke(&fixture, "begin", mutation);
    assert_eq!(code, 0, "envelope={granted}");
    let mut forged = complete_request(&owner, &granted["data"], "session-owner", "complete-x");
    forged["fence"] = json!("wlf1.copied");
    let (code, denied) = invoke(&fixture, "complete", forged);
    assert_eq!(code, 65, "envelope={denied}");
    assert_eq!(denied["error"]["code"], "workspace-operation-fence-invalid");
    assert!(
        !denied
            .to_string()
            .contains(fixture.root.to_str().expect("root UTF-8")),
        "diagnostics must not leak checkout paths: {denied}"
    );
}

#[test]
fn renew_is_exact_and_non_git_directories_remain_unmanaged() {
    let fixture = fixture();
    let non_git = tempfile::TempDir::new().expect("non-Git cwd");
    let binding = bind(
        &fixture,
        "unmanaged-session",
        "unmanaged-bind",
        non_git.path(),
    );
    assert_eq!(binding["kind"], "bound");
    assert_eq!(binding["state"], "unmanaged");

    let mutation = begin_request(&binding, "unmanaged-session", "unmanaged-write", "write");
    let (code, result) = invoke(&fixture, "begin", mutation);
    assert_eq!(code, 0, "envelope={result}");
    assert_eq!(result["data"]["kind"], "not-required");

    let renew = json!({
        "schema_version": "agent-hook.workspace-lease.renew.v1",
        "version": 1,
        "request_id": "unmanaged-renew",
        "session_id": "unmanaged-session",
        "binding_id": binding["binding_id"],
        "workspace_id": binding["workspace_id"],
        "generation": binding["generation"]
    });
    let (code, renewed) = invoke(&fixture, "renew", renew.clone());
    assert_eq!(code, 0, "envelope={renewed}");
    assert_eq!(renewed["data"]["kind"], "renewed");
    assert_eq!(renewed["data"]["renew_after_ms"], 10_000);

    let release = json!({
        "schema_version": "agent-hook.workspace-lease.release.v1",
        "version": 1,
        "request_id": "unmanaged-release",
        "session_id": "unmanaged-session",
        "binding_id": binding["binding_id"],
        "workspace_id": binding["workspace_id"],
        "generation": binding["generation"],
        "reason": "provider-disposed"
    });
    let (code, released) = invoke(&fixture, "release", release);
    assert_eq!(code, 0, "envelope={released}");
    let (code, lost) = invoke(&fixture, "renew", renew);
    assert_eq!(code, 0, "envelope={lost}");
    assert_eq!(lost["data"]["kind"], "lost");
    assert_eq!(lost["data"]["state"], "stale-clean");
    assert_eq!(lost["data"]["code"], "WORKSPACE_BINDING_RELEASED");
}

#[test]
fn strict_wire_rejects_duplicate_and_unknown_fields() {
    let fixture = fixture();
    let duplicate = format!(
        r#"{{"schema_version":"agent-hook.workspace-lease.bind.v1","version":1,"request_id":"a","request_id":"b","session_id":"s","cwd":{},"source":"startup"}}"#,
        serde_json::to_string(&fixture.root).expect("cwd JSON")
    );
    let output = fixture.run(
        &["workspace-lease", "bind", "--format", "json"],
        Some(&duplicate),
    );
    assert_eq!(output.code, 65, "stdout={}", output.stdout_text());
    assert_eq!(
        output.stdout_json()["error"]["code"],
        "workspace-wire-invalid"
    );

    let mut unknown = bind_request("session-a", "unknown", &fixture.root);
    unknown["repository"] = json!("forged/repository");
    let (code, envelope) = invoke(&fixture, "bind", unknown);
    assert_eq!(code, 65, "envelope={envelope}");
    assert_eq!(envelope["error"]["code"], "workspace-wire-invalid");
}

#[test]
fn expired_active_operation_blocks_recovery_until_exact_completion() {
    let fixture = fixture();
    let owner = bind(&fixture, "session-owner", "bind-owner", &fixture.root);
    let mutation = begin_request(&owner, "session-owner", "begin-1", "write");
    let (code, granted) = invoke(&fixture, "begin", mutation);
    assert_eq!(code, 0, "envelope={granted}");
    assert_eq!(granted["data"]["kind"], "granted");

    expire_workspace_state(&fixture);
    let refused = bind(&fixture, "session-peer", "bind-peer", &fixture.root);
    assert_eq!(refused["kind"], "denied");
    assert_eq!(refused["state"], "uncertain");
    assert_eq!(refused["code"], "WORKSPACE_OPERATION_UNCERTAIN");

    let release = json!({
        "schema_version": "agent-hook.workspace-lease.release.v1",
        "version": 1,
        "request_id": "release-active",
        "session_id": "session-owner",
        "binding_id": owner["binding_id"],
        "workspace_id": owner["workspace_id"],
        "generation": owner["generation"],
        "reason": "agent-disposed"
    });
    let (code, envelope) = invoke(&fixture, "release", release.clone());
    assert_eq!(code, 69, "envelope={envelope}");
    assert_eq!(envelope["error"]["code"], "workspace-release-uncertain");
    assert_eq!(envelope["error"]["details"]["retryable"], true);
    assert_eq!(
        envelope["error"]["details"]["recovery"]["kind"],
        "bounded-retry"
    );

    let completion = complete_request(&owner, &granted["data"], "session-owner", "complete-1");
    let (code, envelope) = invoke(&fixture, "complete", completion);
    assert_eq!(code, 0, "envelope={envelope}");
    let (code, envelope) = invoke(&fixture, "release", release);
    assert_eq!(code, 0, "envelope={envelope}");
    assert_eq!(envelope["data"]["kind"], "released");

    let peer = bind(&fixture, "session-peer", "bind-peer-2", &fixture.root);
    assert_eq!(peer["kind"], "bound");
    assert_ne!(peer["generation"], owner["generation"]);
}

#[test]
fn malformed_durable_state_fails_closed_without_protected_values() {
    let fixture = fixture();
    let _owner = bind(&fixture, "session-owner", "bind-owner", &fixture.root);
    let state = workspace_state_file(&fixture);
    fs::write(&state, b"{").expect("malformed state");
    Fixture::set_private(&state);

    let (code, envelope) = invoke(
        &fixture,
        "bind",
        bind_request("session-peer", "bind-peer", &fixture.root),
    );
    assert_eq!(code, 65, "envelope={envelope}");
    assert_eq!(envelope["error"]["code"], "workspace-lease-state-invalid");
    assert!(
        !envelope
            .to_string()
            .contains(fixture.root.to_str().expect("root UTF-8")),
        "diagnostics must not expose protected paths: {envelope}"
    );
    assert!(!envelope.to_string().contains("session-owner"));
}

#[test]
fn durable_state_hashes_session_call_and_tool_facts_and_keeps_its_key_private() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = fixture();
    let owner = bind(&fixture, "private-session", "private-bind", &fixture.root);
    let mutation = begin_request(&owner, "private-session", "private-begin", "write");
    let (code, granted) = invoke(&fixture, "begin", mutation);
    assert_eq!(code, 0, "envelope={granted}");

    let state_text = fs::read_to_string(workspace_state_file(&fixture)).expect("state text");
    for protected in [
        "private-session",
        "private-bind",
        "private-begin",
        "call:private-begin",
        "root:private-begin",
        "tracked.txt",
        "next",
    ] {
        assert!(
            !state_text.contains(protected),
            "durable state retained protected value {protected:?}"
        );
    }

    let key_path = fixture
        .state_home
        .join("agent-hook/workspace-leases/fingerprint.key");
    let key = fs::read_to_string(&key_path).expect("fingerprint key");
    assert_eq!(key.len(), 64);
    assert_eq!(
        fs::metadata(&key_path)
            .expect("fingerprint key metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert!(!owner.to_string().contains(&key));
    assert!(!granted.to_string().contains(&key));
}

#[test]
fn expired_exact_bind_request_never_replays_stale_authority() {
    let fixture = fixture();
    let owner = bind(&fixture, "session-owner", "bind-owner", &fixture.root);
    expire_workspace_state(&fixture);

    let rebound = bind(&fixture, "session-owner", "bind-owner", &fixture.root);
    assert_eq!(rebound["kind"], "bound");
    assert_ne!(rebound["binding_id"], owner["binding_id"]);
    assert_ne!(rebound["generation"], owner["generation"]);
}

fn workspace_state_file(fixture: &Fixture) -> PathBuf {
    let root = fixture.state_home.join("agent-hook/workspace-leases");
    fs::read_dir(root)
        .expect("workspace lease root")
        .filter_map(Result::ok)
        .map(|entry| entry.path().join("state.json"))
        .find(|path| path.is_file())
        .expect("workspace lease state")
}

fn expire_workspace_state(fixture: &Fixture) {
    let state = workspace_state_file(fixture);
    let mut persisted: Value =
        serde_json::from_slice(&fs::read(&state).expect("state bytes")).expect("state JSON");
    persisted["binding"]["refreshed_at_epoch"] = json!(1);
    persisted["binding"]["expires_at_epoch"] = json!(1);
    fs::write(
        &state,
        serde_json::to_vec_pretty(&persisted).expect("state render"),
    )
    .expect("expire state");
    Fixture::set_private(&state);
}
