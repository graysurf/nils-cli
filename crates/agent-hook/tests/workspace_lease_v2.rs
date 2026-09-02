//! WorkspaceLease v2: target-scoped repository authority.
//!
//! v1 bound one immutable session cwd, so a dirty anchor denied every tool. v2
//! classifies each exact operation into zero or more canonical repository
//! targets and binds them lazily and independently.

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use pretty_assertions::{assert_eq, assert_ne};
use serde_json::{Value, json};

use support::Fixture;

const POLICY: &str = r#"schema_version = "agent-hook.policy.v1"
bundle_id = "workspace-lease-v2-test"
version = "2026.09.02.1"
"#;

const RESOLVE_RESULT: &str = "agent-hook.workspace-lease.resolve-result.v2";
const BIND_RESULT: &str = "agent-hook.workspace-lease.bind-result.v2";
const BEGIN_RESULT: &str = "agent-hook.workspace-lease.begin-result.v2";

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

fn repo(root: &Path) -> PathBuf {
    fs::create_dir_all(root).expect("repository directory");
    git(root, &["init", "--quiet"]);
    git(root, &["config", "user.email", "workspace@example.com"]);
    git(root, &["config", "user.name", "Workspace Test"]);
    fs::write(root.join("tracked.txt"), "base\n").expect("tracked file");
    git(root, &["add", "--all"]);
    git(root, &["commit", "--quiet", "-m", "test: initial"]);
    fs::canonicalize(root).expect("canonical repository root")
}

fn invoke(fixture: &Fixture, operation: &str, request: Value) -> (i32, Value) {
    let output = fixture.run(
        &["workspace-lease", operation, "--format", "json"],
        Some(&request.to_string()),
    );
    (output.code, output.stdout_json())
}

fn ok(fixture: &Fixture, operation: &str, request: Value) -> Value {
    let (code, envelope) = invoke(fixture, operation, request);
    assert_eq!(code, 0, "envelope={envelope}");
    envelope["data"].clone()
}

fn resolve_request(
    session: &str,
    request_id: &str,
    anchor: Option<&Path>,
    tool: &str,
    arguments: Value,
) -> Value {
    let mut request = json!({
        "schema_version": "agent-hook.workspace-lease.resolve.v2",
        "version": 2,
        "request_id": request_id,
        "session_id": session,
        "call_id": format!("call:{request_id}"),
        "root_call_id": format!("root:{request_id}"),
        "tool_name": tool,
        "arguments": arguments,
        "nested": false
    });
    if let Some(anchor) = anchor {
        request["anchor_cwd"] = json!(anchor);
    }
    request
}

fn resolve(
    fixture: &Fixture,
    session: &str,
    request_id: &str,
    anchor: Option<&Path>,
    tool: &str,
    arguments: Value,
) -> Value {
    ok(
        fixture,
        "resolve",
        resolve_request(session, request_id, anchor, tool, arguments),
    )
}

fn write_targets(fixture: &Fixture, session: &str, request_id: &str, path: &Path) -> Value {
    let data = resolve(
        fixture,
        session,
        request_id,
        None,
        "write",
        json!({"file_path": path, "content": "next"}),
    );
    assert_eq!(data["schema_version"], RESOLVE_RESULT);
    data
}

fn bind_request(session: &str, request_id: &str, target: &Value) -> Value {
    json!({
        "schema_version": "agent-hook.workspace-lease.bind.v2",
        "version": 2,
        "request_id": request_id,
        "session_id": session,
        "target": target,
        "source": "startup"
    })
}

fn bind(fixture: &Fixture, session: &str, request_id: &str, target: &Value) -> Value {
    ok(fixture, "bind", bind_request(session, request_id, target))
}

fn anchor_bind_request(session: &str, request_id: &str, cwd: &Path, source: &str) -> Value {
    json!({
        "schema_version": "agent-hook.workspace-lease.bind.v2",
        "version": 2,
        "request_id": request_id,
        "session_id": session,
        "cwd": cwd,
        "source": source
    })
}

fn begin_request(
    session: &str,
    request_id: &str,
    binding: &Value,
    target: &Value,
    tool: &str,
    arguments: Value,
) -> Value {
    json!({
        "schema_version": "agent-hook.workspace-lease.begin.v2",
        "version": 2,
        "request_id": request_id,
        "session_id": session,
        "binding_id": binding["binding_id"],
        "workspace_id": binding["workspace_id"],
        "generation": binding["generation"],
        "binding_state": binding["state"],
        "call_id": format!("call:{request_id}"),
        "root_call_id": format!("root:{request_id}"),
        "tool_name": tool,
        "arguments": arguments,
        "nested": false,
        "target": target
    })
}

fn begin(
    fixture: &Fixture,
    session: &str,
    request_id: &str,
    binding: &Value,
    target: &Value,
) -> Value {
    ok(
        fixture,
        "begin",
        begin_request(
            session,
            request_id,
            binding,
            target,
            "write",
            json!({"file_path": "tracked.txt", "content": "next"}),
        ),
    )
}

fn complete(
    fixture: &Fixture,
    session: &str,
    request_id: &str,
    binding: &Value,
    operation: &Value,
    begin_request_id: &str,
) -> Value {
    ok(
        fixture,
        "complete",
        json!({
            "schema_version": "agent-hook.workspace-lease.complete.v2",
            "version": 2,
            "request_id": request_id,
            "session_id": session,
            "binding_id": binding["binding_id"],
            "workspace_id": binding["workspace_id"],
            "generation": binding["generation"],
            "operation_id": operation["operation_id"],
            "fence": operation["fence"],
            "call_id": format!("call:{begin_request_id}"),
            "root_call_id": format!("root:{begin_request_id}"),
            "tool_name": "write",
            "outcome": "succeeded"
        }),
    )
}

fn release(fixture: &Fixture, session: &str, request_id: &str, binding: &Value) -> Value {
    ok(
        fixture,
        "release",
        json!({
            "schema_version": "agent-hook.workspace-lease.release.v2",
            "version": 2,
            "request_id": request_id,
            "session_id": session,
            "binding_id": binding["binding_id"],
            "workspace_id": binding["workspace_id"],
            "generation": binding["generation"],
            "reason": "agent-disposed"
        }),
    )
}

fn only_target(data: &Value) -> Value {
    assert_eq!(data["kind"], "targets", "data={data}");
    let targets = data["targets"].as_array().expect("target array");
    assert_eq!(targets.len(), 1, "data={data}");
    targets[0].clone()
}

#[test]
fn unclassifiable_and_read_only_operations_need_no_repository_target() {
    let fixture = Fixture::new(POLICY);
    let root = repo(&fixture.root.join("repo-a"));

    for (tool, arguments) in [
        (
            "bash",
            json!({"command": "rm -rf ./build", "workdir": root}),
        ),
        ("read", json!({"file_path": root.join("tracked.txt")})),
        (
            "str_replace_editor",
            json!({"command": "view", "path": root.join("tracked.txt")}),
        ),
        ("some_future_tool", json!({"file_path": root})),
    ] {
        let data = resolve(
            &fixture,
            "session-a",
            &format!("r-{tool}"),
            None,
            tool,
            arguments,
        );
        assert_eq!(data["schema_version"], RESOLVE_RESULT);
        assert_eq!(data["kind"], "not-required", "tool={tool} data={data}");
    }
}

#[test]
fn non_repository_writes_need_no_repository_target() {
    let fixture = Fixture::new(POLICY);
    let plain = fixture.root.join("plain/nested");
    fs::create_dir_all(&plain).expect("plain directory");

    let data = write_targets(&fixture, "session-a", "r-plain", &plain.join("notes.txt"));
    assert_eq!(data["kind"], "not-required", "data={data}");
}

#[test]
fn path_spellings_converge_and_relative_paths_resolve_from_the_anchor() {
    let fixture = Fixture::new(POLICY);
    let root = repo(&fixture.root.join("repo-a"));
    fs::create_dir_all(root.join("nested")).expect("nested");
    let link = fixture.root.join("link-a");
    std::os::unix::fs::symlink(&root, &link).expect("symlink");

    let direct = only_target(&write_targets(
        &fixture,
        "session-a",
        "r-direct",
        &root.join("tracked.txt"),
    ));
    let dotted = only_target(&write_targets(
        &fixture,
        "session-a",
        "r-dotted",
        &root.join("nested/../tracked.txt"),
    ));
    let linked = only_target(&write_targets(
        &fixture,
        "session-a",
        "r-linked",
        &link.join("tracked.txt"),
    ));
    let relative = only_target(&resolve(
        &fixture,
        "session-a",
        "r-relative",
        Some(root.join("nested").as_path()),
        "write",
        json!({"file_path": "../tracked.txt", "content": "next"}),
    ));

    assert_eq!(direct["root"], json!(root));
    for other in [&dotted, &linked, &relative] {
        assert_eq!(other, &direct, "targets must converge");
    }
}

#[test]
fn a_relative_target_without_an_anchor_fails_closed() {
    let fixture = Fixture::new(POLICY);
    repo(&fixture.root.join("repo-a"));

    let (code, envelope) = invoke(
        &fixture,
        "resolve",
        resolve_request(
            "session-a",
            "r-relative",
            None,
            "write",
            json!({"file_path": "tracked.txt", "content": "next"}),
        ),
    );
    assert_ne!(code, 0, "envelope={envelope}");
    assert_eq!(envelope["error"]["code"], "workspace-target-unresolvable");
}

#[test]
fn distinct_repositories_bind_independently_for_one_session() {
    let fixture = Fixture::new(POLICY);
    let first = repo(&fixture.root.join("repo-a"));
    let second = repo(&fixture.root.join("repo-b"));

    let target_a = only_target(&write_targets(
        &fixture,
        "session-a",
        "r-a",
        &first.join("tracked.txt"),
    ));
    let target_b = only_target(&write_targets(
        &fixture,
        "session-a",
        "r-b",
        &second.join("tracked.txt"),
    ));
    assert_ne!(target_a["workspace_key"], target_b["workspace_key"]);

    let binding_a = bind(&fixture, "session-a", "b-a", &target_a);
    let binding_b = bind(&fixture, "session-a", "b-b", &target_b);
    assert_eq!(binding_a["schema_version"], BIND_RESULT);
    assert_eq!(binding_a["kind"], "bound");
    assert_eq!(binding_a["state"], "owned");
    assert_eq!(binding_b["kind"], "bound");
    assert_ne!(binding_a["binding_id"], binding_b["binding_id"]);
    assert_ne!(binding_a["workspace_id"], binding_b["workspace_id"]);

    // A binding for A grants no authority over B and vice versa.
    let (code, envelope) = invoke(
        &fixture,
        "begin",
        begin_request(
            "session-a",
            "x-cross",
            &binding_a,
            &target_b,
            "write",
            json!({"file_path": "tracked.txt", "content": "next"}),
        ),
    );
    assert_ne!(code, 0, "envelope={envelope}");
    assert_eq!(envelope["error"]["code"], "workspace-target-invalid");

    let operation_a = begin(&fixture, "session-a", "g-a", &binding_a, &target_a);
    let operation_b = begin(&fixture, "session-a", "g-b", &binding_b, &target_b);
    assert_eq!(operation_a["schema_version"], BEGIN_RESULT);
    assert_eq!(operation_a["kind"], "granted");
    assert_eq!(operation_b["kind"], "granted");
    assert_ne!(operation_a["fence"], operation_b["fence"]);

    assert_eq!(
        complete(
            &fixture,
            "session-a",
            "c-a",
            &binding_a,
            &operation_a,
            "g-a"
        )["kind"],
        "completed"
    );
    assert_eq!(
        complete(
            &fixture,
            "session-a",
            "c-b",
            &binding_b,
            &operation_b,
            "g-b"
        )["kind"],
        "completed"
    );
    assert_eq!(
        release(&fixture, "session-a", "rel-a", &binding_a)["kind"],
        "released"
    );
    assert_eq!(
        release(&fixture, "session-a", "rel-b", &binding_b)["kind"],
        "released"
    );
}

#[test]
fn repeated_resolution_of_one_target_set_is_stable() {
    let fixture = Fixture::new(POLICY);
    let root = repo(&fixture.root.join("repo-a"));

    // Acquisition order is the sorted keyed workspace digest, so repeated
    // resolution of the same operation projects a byte-stable target set and
    // multi-target acquisition cannot deadlock between sessions.
    let first = write_targets(&fixture, "session-a", "r-set-1", &root.join("tracked.txt"));
    let second = write_targets(
        &fixture,
        "session-a",
        "r-set-2",
        &root.join("nested/new.txt"),
    );
    assert_eq!(only_target(&first), only_target(&second));
}

#[test]
fn a_dirty_target_denies_only_that_repository_while_a_clean_worktree_binds() {
    let fixture = Fixture::new(POLICY);
    let dirty_root = repo(&fixture.root.join("repo-a"));
    let clean_root = repo(&fixture.root.join("repo-b"));
    let linked = fixture.root.join("linked-a");
    git(
        &dirty_root,
        &[
            "worktree",
            "add",
            "--quiet",
            linked.to_str().expect("linked path"),
            "-b",
            "linked",
        ],
    );
    let linked = fs::canonicalize(&linked).expect("canonical linked worktree");
    fs::write(dirty_root.join("tracked.txt"), "dirty\n").expect("dirty file");

    let dirty_target = only_target(&write_targets(
        &fixture,
        "session-a",
        "r-dirty",
        &dirty_root.join("tracked.txt"),
    ));
    let denial = bind(&fixture, "session-a", "b-dirty", &dirty_target);
    assert_eq!(denial["schema_version"], BIND_RESULT);
    assert_eq!(denial["kind"], "denied");
    assert_eq!(denial["state"], "dirty");
    assert_eq!(denial["code"], "WORKSPACE_DIRTY");

    // The same session keeps full authority over an unrelated repository and
    // over a distinct clean linked worktree of the same repository.
    for (request_id, root) in [("b-clean", &clean_root), ("b-linked", &linked)] {
        let target = only_target(&write_targets(
            &fixture,
            "session-a",
            &format!("r-{request_id}"),
            &root.join("tracked.txt"),
        ));
        assert_ne!(target["workspace_key"], dirty_target["workspace_key"]);
        let binding = bind(&fixture, "session-a", request_id, &target);
        assert_eq!(binding["kind"], "bound", "root={root:?}");
        assert_eq!(
            begin(
                &fixture,
                "session-a",
                &format!("g-{request_id}"),
                &binding,
                &target
            )["kind"],
            "granted"
        );
    }
}

#[test]
fn one_physical_worktree_still_contends_across_sessions() {
    let fixture = Fixture::new(POLICY);
    let root = repo(&fixture.root.join("repo-a"));
    let target = only_target(&write_targets(
        &fixture,
        "session-a",
        "r-a",
        &root.join("tracked.txt"),
    ));

    let owner = bind(&fixture, "session-a", "b-owner", &target);
    assert_eq!(owner["kind"], "bound");

    let contender = bind(&fixture, "session-b", "b-contender", &target);
    assert_eq!(contender["kind"], "denied");
    assert_eq!(contender["state"], "foreign-active");
    assert_eq!(contender["code"], "WORKSPACE_FOREIGN_ACTIVE");
}

#[test]
fn a_forged_or_drifted_target_cannot_bind() {
    let fixture = Fixture::new(POLICY);
    let first = repo(&fixture.root.join("repo-a"));
    let second = repo(&fixture.root.join("repo-b"));
    let target_a = only_target(&write_targets(
        &fixture,
        "session-a",
        "r-a",
        &first.join("tracked.txt"),
    ));

    // A model-shaped root swap keeps the authenticated digest of repository A.
    let mut forged = target_a.clone();
    forged["root"] = json!(second);
    let (code, envelope) = invoke(
        &fixture,
        "bind",
        bind_request("session-a", "b-forged", &forged),
    );
    assert_ne!(code, 0, "envelope={envelope}");
    assert_eq!(envelope["error"]["code"], "workspace-target-invalid");

    // A subdirectory is not a canonical repository root.
    let mut nested = target_a.clone();
    nested["root"] = json!(first.join("nested"));
    fs::create_dir_all(first.join("nested")).expect("nested");
    let (code, envelope) = invoke(
        &fixture,
        "bind",
        bind_request("session-a", "b-nested", &nested),
    );
    assert_ne!(code, 0, "envelope={envelope}");
    assert_eq!(envelope["error"]["code"], "workspace-target-invalid");
}

#[test]
fn an_anchor_bind_is_optional_and_a_non_repository_anchor_needs_no_binding() {
    let fixture = Fixture::new(POLICY);
    let root = repo(&fixture.root.join("repo-a"));
    let plain = fixture.root.join("plain");
    fs::create_dir_all(&plain).expect("plain directory");

    let anchored = ok(
        &fixture,
        "bind",
        anchor_bind_request("session-a", "b-anchor", &root, "startup"),
    );
    assert_eq!(anchored["kind"], "bound");
    assert_eq!(anchored["state"], "owned");

    let unmanaged = ok(
        &fixture,
        "bind",
        anchor_bind_request("session-b", "b-plain", &plain, "startup"),
    );
    assert_eq!(unmanaged["schema_version"], BIND_RESULT);
    assert_eq!(unmanaged["kind"], "not-required");

    // The eager anchor binding is the same durable authority a later lazy
    // acquisition of that repository observes.
    let target = only_target(&write_targets(
        &fixture,
        "session-c",
        "r-a",
        &root.join("tracked.txt"),
    ));
    let contender = bind(&fixture, "session-c", "b-contender", &target);
    assert_eq!(contender["kind"], "denied");
    assert_eq!(contender["code"], "WORKSPACE_FOREIGN_ACTIVE");
}

#[test]
fn same_session_resume_recovers_its_own_dirty_target() {
    let fixture = Fixture::new(POLICY);
    let root = repo(&fixture.root.join("repo-a"));
    let target = only_target(&write_targets(
        &fixture,
        "session-a",
        "r-a",
        &root.join("tracked.txt"),
    ));

    let binding = bind(&fixture, "session-a", "b-a", &target);
    assert_eq!(binding["kind"], "bound");
    fs::write(root.join("tracked.txt"), "dirty\n").expect("dirty file");
    assert_eq!(
        release(&fixture, "session-a", "rel-a", &binding)["kind"],
        "released"
    );

    let resumed = ok(
        &fixture,
        "bind",
        json!({
            "schema_version": "agent-hook.workspace-lease.bind.v2",
            "version": 2,
            "request_id": "b-resume",
            "session_id": "session-a",
            "target": target,
            "source": "resume"
        }),
    );
    assert_eq!(resumed["kind"], "bound", "resumed={resumed}");

    let foreign = ok(
        &fixture,
        "bind",
        json!({
            "schema_version": "agent-hook.workspace-lease.bind.v2",
            "version": 2,
            "request_id": "b-foreign",
            "session_id": "session-b",
            "target": target,
            "source": "resume"
        }),
    );
    assert_eq!(foreign["kind"], "denied");
    assert_eq!(foreign["state"], "foreign-active");
}

#[test]
fn mixed_protocol_generations_are_rejected_rather_than_reinterpreted() {
    let fixture = Fixture::new(POLICY);
    let root = repo(&fixture.root.join("repo-a"));
    let target = only_target(&write_targets(
        &fixture,
        "session-a",
        "r-a",
        &root.join("tracked.txt"),
    ));
    let binding = bind(&fixture, "session-a", "b-a", &target);

    // A v2 schema declaring version 1.
    let mut mixed = bind_request("session-a", "b-mixed", &target);
    mixed["version"] = json!(1);
    let (code, envelope) = invoke(&fixture, "bind", mixed);
    assert_ne!(code, 0, "envelope={envelope}");
    assert_eq!(envelope["error"]["code"], "workspace-protocol-unsupported");

    // A v1 schema carrying a v2 target.
    let (code, envelope) = invoke(
        &fixture,
        "bind",
        json!({
            "schema_version": "agent-hook.workspace-lease.bind.v1",
            "version": 1,
            "request_id": "b-v1-target",
            "session_id": "session-a",
            "target": target,
            "source": "startup"
        }),
    );
    assert_ne!(code, 0, "envelope={envelope}");
    assert_eq!(envelope["error"]["code"], "workspace-wire-invalid");

    // A v2 begin without an exact target owns no honest coverage claim.
    let mut untargeted = begin_request(
        "session-a",
        "g-untargeted",
        &binding,
        &target,
        "write",
        json!({"file_path": "tracked.txt", "content": "next"}),
    );
    untargeted.as_object_mut().expect("object").remove("target");
    let (code, envelope) = invoke(&fixture, "begin", untargeted);
    assert_ne!(code, 0, "envelope={envelope}");
    assert_eq!(envelope["error"]["code"], "workspace-wire-invalid");
}

#[test]
fn a_v2_mutation_target_always_receives_a_fence() {
    let fixture = Fixture::new(POLICY);
    let root = repo(&fixture.root.join("repo-a"));
    let target = only_target(&write_targets(
        &fixture,
        "session-a",
        "r-a",
        &root.join("tracked.txt"),
    ));
    let binding = bind(&fixture, "session-a", "b-a", &target);

    // v1 reclassified read-only tool names inside begin. v2 classification is
    // owned by resolve, so an admitted v2 target is always fenced.
    let granted = ok(
        &fixture,
        "begin",
        begin_request(
            "session-a",
            "g-read",
            &binding,
            &target,
            "read",
            json!({"file_path": "tracked.txt"}),
        ),
    );
    assert_eq!(granted["kind"], "granted");
    assert_eq!(
        resolve(
            &fixture,
            "session-a",
            "r-read",
            None,
            "read",
            json!({"file_path": root.join("tracked.txt")})
        )["kind"],
        "not-required"
    );
}

#[test]
fn v2_operation_replay_and_release_ordering_stay_fenced() {
    let fixture = Fixture::new(POLICY);
    let root = repo(&fixture.root.join("repo-a"));
    let target = only_target(&write_targets(
        &fixture,
        "session-a",
        "r-a",
        &root.join("tracked.txt"),
    ));
    let binding = bind(&fixture, "session-a", "b-a", &target);

    // An active retry of the exact request is idempotent.
    let first = begin(&fixture, "session-a", "g-a", &binding, &target);
    let retry = begin(&fixture, "session-a", "g-a", &binding, &target);
    assert_eq!(first, retry);

    // A release cannot retire authority while an operation lacks an outcome.
    let (code, envelope) = invoke(
        &fixture,
        "release",
        json!({
            "schema_version": "agent-hook.workspace-lease.release.v2",
            "version": 2,
            "request_id": "rel-early",
            "session_id": "session-a",
            "binding_id": binding["binding_id"],
            "workspace_id": binding["workspace_id"],
            "generation": binding["generation"],
            "reason": "agent-disposed"
        }),
    );
    assert_ne!(code, 0, "envelope={envelope}");
    assert_eq!(envelope["error"]["code"], "workspace-release-uncertain");

    assert_eq!(
        complete(&fixture, "session-a", "c-a", &binding, &first, "g-a")["kind"],
        "completed"
    );
    assert_eq!(
        complete(&fixture, "session-a", "c-a", &binding, &first, "g-a")["kind"],
        "duplicate"
    );

    // A terminal operation identity can never be replayed into new authority.
    let replayed = begin(&fixture, "session-a", "g-a", &binding, &target);
    assert_eq!(replayed["kind"], "denied");
    assert_eq!(replayed["code"], "WORKSPACE_OPERATION_REPLAYED");

    assert_eq!(
        release(&fixture, "session-a", "rel-a", &binding)["kind"],
        "released"
    );
    let (code, envelope) = invoke(
        &fixture,
        "begin",
        begin_request(
            "session-a",
            "g-released",
            &binding,
            &target,
            "write",
            json!({"file_path": "tracked.txt", "content": "next"}),
        ),
    );
    assert_eq!(code, 0, "envelope={envelope}");
    assert_eq!(envelope["data"]["kind"], "denied");
    assert_eq!(envelope["data"]["code"], "WORKSPACE_BINDING_RELEASED");
}

#[test]
fn every_v2_binding_names_its_exact_repository_target() {
    let fixture = Fixture::new(POLICY);
    let root = repo(&fixture.root.join("repo-a"));
    let target = only_target(&write_targets(
        &fixture,
        "session-a",
        "r-a",
        &root.join("tracked.txt"),
    ));

    let lazy = bind(&fixture, "session-a", "b-a", &target);
    assert_eq!(lazy["target"], target);

    // The eager anchor binding converges on the same canonical target, so one
    // session never contends with itself over its own anchor repository.
    let anchored = ok(
        &fixture,
        "bind",
        anchor_bind_request("session-b", "b-anchor", &root, "startup"),
    );
    assert_eq!(anchored["kind"], "denied");

    let solo = Fixture::new(POLICY);
    let solo_root = repo(&solo.root.join("repo-a"));
    let eager = ok(
        &solo,
        "bind",
        anchor_bind_request("session-a", "b-anchor", &solo_root, "startup"),
    );
    assert_eq!(eager["kind"], "bound");
    let resolved = only_target(&write_targets(
        &solo,
        "session-a",
        "r-a",
        &solo_root.join("tracked.txt"),
    ));
    assert_eq!(eager["target"], resolved);
}
