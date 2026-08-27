mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

#[cfg(target_os = "linux")]
use std::ffi::OsString;
#[cfg(target_os = "linux")]
use std::os::unix::ffi::OsStringExt;

use pretty_assertions::assert_eq;
use serde_json::{Value, json};

use nils_test_support::tempdir::ScopedTempDir;
use support::Fixture;

const POLICY: &str = r#"schema_version = "agent-hook.policy.v1"
bundle_id = "workspace-recovery-test"
version = "2026.08.27.1"
"#;

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

fn repo_key(repo_root: &Path) -> String {
    let basename = repo_root
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_ascii_lowercase();
    let basename = basename.trim_matches(['-', '_', '.']);
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in repo_root.as_os_str().as_encoded_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{basename}-{:08x}", hash as u32)
}

fn fixture() -> (Fixture, PathBuf) {
    let fixture = Fixture::new(POLICY);
    git(&fixture.root, &["init", "--quiet"]);
    git(
        &fixture.root,
        &["config", "user.email", "workspace@example.com"],
    );
    git(&fixture.root, &["config", "user.name", "Workspace Test"]);
    fs::write(
        fixture.root.join(".gitignore"),
        "/config/\n/data/\n/home/\n/state/\nsession-state/\n",
    )
    .expect("fixture ignores");
    fs::write(fixture.root.join("tracked.txt"), "base\n").expect("tracked file");
    git(&fixture.root, &["add", "--all"]);
    git(&fixture.root, &["commit", "--quiet", "-m", "test: initial"]);
    let managed = fixture
        .state_home
        .join("agent-runtime-kit/worktrees")
        .join(repo_key(&fixture.root))
        .join("handoff");
    fs::create_dir_all(managed.parent().expect("managed parent")).expect("managed parent");
    git(
        &fixture.root,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "fix/handoff",
            managed.to_str().expect("managed UTF-8"),
            "HEAD",
        ],
    );
    (fixture, managed)
}

fn inspect(fixture: &Fixture) -> (i32, Value) {
    let request = json!({
        "schema_version": "agent-hook.workspace-recovery.inspect.v1",
        "version": 1,
        "cwd": fixture.root,
    });
    let agent_home = fixture.state_home.join("agent-runtime-kit");
    let output = fixture.run_with_env(
        &["workspace-recovery", "inspect", "--format", "json"],
        Some(&request.to_string()),
        &[("AGENT_HOME", agent_home.to_str().expect("agent home UTF-8"))],
    );
    (output.code, output.stdout_json())
}

fn verify(fixture: &Fixture, handoff: &Path) -> (i32, Value) {
    let request = json!({
        "schema_version": "agent-hook.workspace-recovery.verify-handoff.v1",
        "version": 1,
        "cwd": fixture.root,
        "handoff_path": handoff,
    });
    let agent_home = fixture.state_home.join("agent-runtime-kit");
    let output = fixture.run_with_env(
        &["workspace-recovery", "verify-handoff", "--format", "json"],
        Some(&request.to_string()),
        &[("AGENT_HOME", agent_home.to_str().expect("agent home UTF-8"))],
    );
    (output.code, output.stdout_json())
}

#[test]
fn inspect_projects_dirty_path_names_and_managed_worktrees_without_mutation() {
    let (fixture, managed) = fixture();
    fs::write(fixture.root.join("notes.txt"), "private contents\n").expect("dirty file");
    let notes_before = fs::read(fixture.root.join("notes.txt")).expect("dirty file bytes");
    let head_before = fs::read(fixture.root.join(".git/HEAD")).expect("HEAD bytes");
    let index_before = fs::read(fixture.root.join(".git/index")).expect("index bytes");
    let config_before = fs::read(fixture.root.join(".git/config")).expect("config bytes");

    let (code, envelope) = inspect(&fixture);

    assert_eq!(code, 0, "envelope={envelope}");
    assert_eq!(
        envelope["schema_version"],
        "cli.agent-hook.workspace-recovery-inspect.v1"
    );
    let data = &envelope["data"];
    assert_eq!(
        data["schema_version"],
        "agent-hook.workspace-recovery.result.v1"
    );
    assert_eq!(data["action"], "inspect");
    assert_eq!(data["state"], "dirty");
    assert_eq!(data["checkout"]["dirty_entries"][0]["path"], "notes.txt");
    assert_eq!(data["checkout"]["dirty_entries"][0]["lossy"], false);
    let projected = data["worktrees"].as_array().expect("worktrees");
    let handoff = projected
        .iter()
        .find(|entry| entry["path"] == managed.to_string_lossy().as_ref())
        .expect("managed handoff");
    assert_eq!(handoff["managed"], true, "data={data}");
    assert_eq!(handoff["branch"], "fix/handoff");
    assert!(!envelope.to_string().contains("private contents"));
    assert_eq!(
        fs::read(fixture.root.join("notes.txt")).unwrap(),
        notes_before
    );
    assert_eq!(
        fs::read(fixture.root.join(".git/HEAD")).unwrap(),
        head_before
    );
    assert_eq!(
        fs::read(fixture.root.join(".git/index")).unwrap(),
        index_before
    );
    assert_eq!(
        fs::read(fixture.root.join(".git/config")).unwrap(),
        config_before
    );
    assert!(
        !fixture
            .state_home
            .join("agent-hook/workspace-leases")
            .exists()
    );
}

#[test]
fn verify_handoff_accepts_only_a_different_clean_managed_worktree() {
    let (fixture, managed) = fixture();
    fs::write(fixture.root.join("notes.txt"), "dirty\n").expect("dirty file");

    let (code, envelope) = verify(&fixture, &managed);
    assert_eq!(code, 0, "envelope={envelope}");
    assert_eq!(envelope["data"]["handoff"]["status"], "verified");
    assert_eq!(envelope["data"]["handoff"]["branch"], "fix/handoff");

    fs::write(managed.join("unfinished.txt"), "dirty\n").expect("dirty handoff");
    let (code, envelope) = verify(&fixture, &managed);
    assert_eq!(code, 65, "envelope={envelope}");
    assert_eq!(
        envelope["error"]["code"],
        "workspace-recovery-handoff-dirty"
    );

    let (code, envelope) = verify(&fixture, &fixture.root);
    assert_eq!(code, 65, "envelope={envelope}");
    assert_eq!(
        envelope["error"]["code"],
        "workspace-recovery-handoff-ineligible"
    );
}

#[test]
fn inspect_never_executes_repository_filters() {
    let (fixture, _managed) = fixture();
    fs::write(
        fixture.root.join(".gitattributes"),
        "tracked.txt filter=hostile\n",
    )
    .expect("hostile attributes");
    git(&fixture.root, &["add", ".gitattributes"]);
    git(
        &fixture.root,
        &["commit", "--quiet", "-m", "test: hostile filter fixture"],
    );
    let marker = fixture.root.join("filter-executed");
    let filter = fixture.root.join("hostile-filter.sh");
    fs::write(
        &filter,
        format!("#!/bin/sh\n: > {}\n/bin/cat\n", marker.to_string_lossy()),
    )
    .expect("hostile filter");
    fs::set_permissions(&filter, fs::Permissions::from_mode(0o700)).expect("filter mode");
    git(
        &fixture.root,
        &[
            "config",
            "filter.hostile.clean",
            filter.to_str().expect("filter UTF-8"),
        ],
    );
    fs::write(fixture.root.join("tracked.txt"), "next\n").expect("dirty tracked file");

    let (code, envelope) = inspect(&fixture);

    assert_eq!(code, 0, "envelope={envelope}");
    assert_eq!(envelope["data"]["state"], "dirty");
    assert!(!marker.exists(), "inspection executed a repository filter");
}

#[test]
#[cfg(target_os = "linux")]
fn inspect_marks_lossy_dirty_paths_without_exposing_file_contents() {
    let (fixture, _managed) = fixture();
    let path = fixture
        .root
        .join(OsString::from_vec(b"opaque-\xff.txt".to_vec()));
    fs::write(&path, "must not appear in output\n").expect("non-UTF-8 dirty path");

    let (code, envelope) = inspect(&fixture);

    assert_eq!(code, 0, "envelope={envelope}");
    let entries = envelope["data"]["checkout"]["dirty_entries"]
        .as_array()
        .expect("dirty entries");
    let projected = entries
        .iter()
        .find(|entry| {
            entry["path"]
                .as_str()
                .is_some_and(|value| value.starts_with("opaque-"))
        })
        .expect("lossy path projection");
    assert_eq!(projected["lossy"], true);
    assert!(!envelope.to_string().contains("must not appear in output"));
}

#[test]
fn inspect_projects_stale_worktrees_as_prunable_and_never_as_handoffs() {
    let (fixture, managed) = fixture();
    fs::remove_dir_all(&managed).expect("remove linked worktree checkout only");

    let (code, envelope) = inspect(&fixture);

    assert_eq!(code, 0, "envelope={envelope}");
    let projected = envelope["data"]["worktrees"].as_array().expect("worktrees");
    let stale = projected
        .iter()
        .find(|entry| entry["path"] == managed.to_string_lossy().as_ref())
        .expect("prunable managed worktree");
    assert_eq!(stale["managed"], true);
    assert_eq!(stale["prunable"], true);
    assert_eq!(stale["branch"], Value::Null);
    assert_eq!(stale["head"], Value::Null);

    let (verify_code, denied) = verify(&fixture, &managed);
    assert_eq!(verify_code, 65, "envelope={denied}");
    assert_eq!(
        denied["error"]["code"],
        "workspace-recovery-handoff-invalid"
    );
}

#[test]
fn strict_wire_rejects_ambiguous_and_duplicate_requests() {
    let (fixture, _managed) = fixture();
    let unknown = json!({
        "schema_version": "agent-hook.workspace-recovery.inspect.v1",
        "version": 1,
        "cwd": fixture.root,
        "handoff_path": fixture.root,
    });
    let output = fixture.run(
        &["workspace-recovery", "inspect", "--format", "json"],
        Some(&unknown.to_string()),
    );
    assert_eq!(output.code, 65);
    assert_eq!(
        output.stdout_json()["error"]["code"],
        "workspace-recovery-wire-invalid"
    );

    let duplicate = format!(
        r#"{{"schema_version":"agent-hook.workspace-recovery.inspect.v1","version":1,"version":1,"cwd":{}}}"#,
        serde_json::to_string(&fixture.root).unwrap()
    );
    let output = fixture.run(
        &["workspace-recovery", "inspect", "--format", "json"],
        Some(&duplicate),
    );
    assert_eq!(output.code, 65);
    assert_eq!(
        output.stdout_json()["error"]["code"],
        "workspace-recovery-wire-invalid"
    );
}

#[test]
fn unavailable_checkout_has_bounded_typed_recovery_details_without_path_leakage() {
    let (fixture, _) = fixture();
    let missing = ScopedTempDir::outside_git_ancestry("workspace-recovery-missing-");
    let request = json!({
        "schema_version": "agent-hook.workspace-recovery.inspect.v1",
        "version": 1,
        "cwd": missing.path().join("missing-checkout"),
    });

    let output = fixture.run(
        &["workspace-recovery", "inspect", "--format", "json"],
        Some(&request.to_string()),
    );
    let envelope = output.stdout_json();

    assert_eq!(output.code, 69, "envelope={envelope}");
    assert_eq!(
        envelope["error"]["code"],
        "workspace-recovery-checkout-unavailable"
    );
    assert_eq!(envelope["error"]["details"]["retryable"], true);
    assert_eq!(
        envelope["error"]["details"]["next_action"],
        "verify-checkout-and-retry"
    );
    assert_eq!(
        envelope["error"]["details"]["recovery"]["kind"],
        "bounded-retry"
    );
    assert_eq!(envelope["error"]["details"]["recovery"]["max_attempts"], 1);
    assert!(!envelope.to_string().contains("missing-checkout"));
}

#[test]
fn inspect_truncates_large_projections_with_typed_omitted_counts() {
    let (fixture, _) = fixture();
    for index in 0..1_400 {
        let name = format!("{index:04}-{}", "x".repeat(180));
        fs::write(fixture.root.join(name), []).expect("large dirty projection fixture");
    }
    let request = json!({
        "schema_version": "agent-hook.workspace-recovery.inspect.v1",
        "version": 1,
        "cwd": fixture.root,
    });
    let agent_home = fixture.state_home.join("agent-runtime-kit");

    let output = fixture.run_with_env(
        &["workspace-recovery", "inspect", "--format", "json"],
        Some(&request.to_string()),
        &[("AGENT_HOME", agent_home.to_str().expect("agent home UTF-8"))],
    );
    let envelope = output.stdout_json();
    let data = &envelope["data"];
    let displayed = data["checkout"]["dirty_entries"]
        .as_array()
        .expect("dirty entries")
        .len();
    let omitted = data["checkout"]["dirty_entries_omitted"]
        .as_u64()
        .expect("dirty entries omitted") as usize;

    assert_eq!(output.code, 0, "envelope={envelope}");
    assert!(omitted > 0, "large projection must be truncated");
    assert_eq!(displayed + omitted, 1_400);
    assert!(serde_json::to_vec(data).unwrap().len() <= 192 * 1024);
    assert!(output.stdout_text().len() <= 256 * 1024);
    assert!(data["worktrees_omitted"].is_u64());
}
