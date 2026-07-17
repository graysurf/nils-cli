use crate::common::{GitCliHarness, git, init_repo};
use git_cli::worktree::dirty_checkout_adoption::{
    DirtySnapshot, adopt_dirty, dirty_snapshot, revoke_dirty,
};
use nils_test_support::cmd::{CmdOutput, run_with};
use pretty_assertions::assert_eq;
use serde_json::json;
use std::fs;
use std::os::unix::ffi::OsStrExt;
#[cfg(target_os = "linux")]
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::time::{SystemTime, UNIX_EPOCH};

const CHALLENGE_TOKEN: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const CHALLENGE_TOKEN_DIGEST: &str =
    "ffe054fe7ae0cb6dc65c3af9b61d5209f439851db43d0ba5997337df154668eb";
const SESSION_KEY: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const AUTHORIZATION_TURN_DIGEST: &str =
    "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

fn checkout_state_dir(
    state_root: &std::path::Path,
    snapshot: &DirtySnapshot,
) -> std::path::PathBuf {
    state_root
        .join(&snapshot.repository_key)
        .join(&snapshot.checkout_key)
}

fn write_challenge_window(
    state_root: &std::path::Path,
    snapshot: &DirtySnapshot,
    issued_at: u64,
    expires_at: u64,
) -> std::path::PathBuf {
    let challenge_dir = checkout_state_dir(state_root, snapshot).join("challenges");
    fs::create_dir_all(&challenge_dir).expect("create challenge directory");
    fs::set_permissions(&challenge_dir, fs::Permissions::from_mode(0o700))
        .expect("make challenge directory private");
    let challenge = json!({
        "schema": "agent-runtime.dirty-checkout-challenge.v1",
        "token_digest": CHALLENGE_TOKEN_DIGEST,
        "session_key": SESSION_KEY,
        "repository_key": &snapshot.repository_key,
        "checkout_key": &snapshot.checkout_key,
        "checkout_instance": &snapshot.checkout_instance,
        "snapshot_id": &snapshot.snapshot_id,
        "head_oid": &snapshot.head_oid,
        "branch_ref_digest": &snapshot.branch_ref_digest,
        "authorization_turn_digest": AUTHORIZATION_TURN_DIGEST,
        "issued_at": issued_at,
        "expires_at": expires_at,
    });
    let challenge_path = challenge_dir.join(format!("{CHALLENGE_TOKEN_DIGEST}.json"));
    fs::write(&challenge_path, format!("{challenge}\n")).expect("write challenge fixture");
    fs::set_permissions(&challenge_path, fs::Permissions::from_mode(0o600))
        .expect("make challenge private");
    challenge_path
}

fn write_challenge(state_root: &std::path::Path, snapshot: &DirtySnapshot) -> std::path::PathBuf {
    let issued_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time")
        .as_secs();
    write_challenge_window(state_root, snapshot, issued_at, issued_at + 300)
}

fn run_governed_command(
    harness: &GitCliHarness,
    cwd: &std::path::Path,
    state_root: &std::path::Path,
    enabled: bool,
    args: &[&str],
) -> CmdOutput {
    let state_root = state_root.to_string_lossy().to_string();
    let mut options = harness
        .cmd_options(cwd)
        .with_env("AGENT_RUNTIME_CHECKOUT_LEASE_STATE_HOME", &state_root)
        .with_env_remove("AGENT_RUNTIME_DIRTY_CHECKOUT_ADOPTION");
    if enabled {
        options = options.with_env("AGENT_RUNTIME_DIRTY_CHECKOUT_ADOPTION", "1");
    }
    run_with(&harness.git_cli_bin(), args, &options)
}

fn assert_help_contract(args: &[&str], expected_usage: &str) -> String {
    let harness = GitCliHarness::new();
    let repo = crate::common::init_repo();

    let output = harness.run(repo.path(), args);

    assert_eq!(output.code, 0, "stderr: {}", output.stderr_text());
    assert_eq!(output.stderr_text(), "");
    let stdout = output.stdout_text();
    assert!(
        stdout.contains(expected_usage),
        "expected help to contain {expected_usage:?}\n\n{stdout}"
    );
    stdout
}

#[test]
fn dirty_snapshot_help_exposes_frozen_command_surface() {
    assert_help_contract(
        &["worktree", "dirty-snapshot", "--help"],
        "Usage: git-cli worktree dirty-snapshot",
    );
}

#[test]
fn adopt_dirty_help_exposes_frozen_authorization_inputs() {
    assert_help_contract(
        &["worktree", "adopt-dirty", "--help"],
        "Usage: git-cli worktree adopt-dirty --challenge <token> --reason-file <path>",
    );
}

#[test]
fn revoke_dirty_help_exposes_opaque_receipt_id_input() {
    let stdout = assert_help_contract(
        &["worktree", "revoke-dirty", "--help"],
        "Usage: git-cli worktree revoke-dirty --receipt <id>",
    );
    assert!(
        !stdout.contains("--receipt <path>"),
        "receipt must remain an opaque ID, not a file path:\n\n{stdout}"
    );
}

#[test]
fn dirty_snapshot_is_sensitive_without_mutating_checkout() {
    let repo = init_repo();
    let dirty_path = repo.path().join("dirty.txt");
    fs::write(&dirty_path, "first dirty state\n").expect("write dirty file");
    let first_status = git(
        repo.path(),
        &["status", "--porcelain=v1", "--untracked-files=all"],
    );

    let first_challenge = dirty_snapshot(repo.path()).expect("snapshot first dirty state");

    assert_eq!(
        git(
            repo.path(),
            &["status", "--porcelain=v1", "--untracked-files=all"]
        ),
        first_status,
        "snapshot must not mutate the checkout"
    );
    assert_eq!(
        fs::read_to_string(&dirty_path).expect("read dirty file"),
        "first dirty state\n"
    );

    fs::write(&dirty_path, "second dirty state\n").expect("change dirty file");
    let second_status = git(
        repo.path(),
        &["status", "--porcelain=v1", "--untracked-files=all"],
    );
    let second_challenge = dirty_snapshot(repo.path()).expect("snapshot changed dirty state");

    assert_ne!(
        first_challenge, second_challenge,
        "challenge must be sensitive to dirty content"
    );
    assert_eq!(
        git(
            repo.path(),
            &["status", "--porcelain=v1", "--untracked-files=all"]
        ),
        second_status,
        "snapshot must leave the changed checkout untouched"
    );
    assert_eq!(
        fs::read_to_string(&dirty_path).expect("read changed dirty file"),
        "second dirty state\n"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn dirty_snapshot_supports_non_utf8_native_paths() {
    let repo = init_repo();
    let filename = std::ffi::OsString::from_vec(b"native-\xff-path".to_vec());
    fs::write(repo.path().join(filename), b"native path content\n").expect("write native path");

    let snapshot = dirty_snapshot(repo.path()).expect("snapshot non-UTF-8 path");

    assert_eq!(snapshot.untracked_entries, 1);
    assert_eq!(snapshot.hashed_bytes, 20);
}

#[test]
fn dirty_snapshot_rejects_symlink_escape_without_reading_target() {
    let repo = init_repo();
    symlink("/etc/passwd", repo.path().join("escaped-link")).expect("create escaped symlink");

    let error = dirty_snapshot(repo.path()).expect_err("escaped symlink must be unsupported");

    assert!(
        error.to_string().contains("symlink"),
        "error should identify the unsupported object class: {error}"
    );
}

#[test]
fn dirty_snapshot_rejects_active_git_operation_state() {
    let repo = init_repo();
    fs::write(repo.path().join("dirty.txt"), "dirty\n").expect("write dirty file");
    let git_dir = git(repo.path(), &["rev-parse", "--absolute-git-dir"]);
    fs::write(std::path::Path::new(git_dir.trim()).join("index.lock"), "")
        .expect("write operation marker");

    let error = dirty_snapshot(repo.path()).expect_err("active operation must be unsupported");

    assert!(
        error.to_string().contains("Git operation"),
        "error should identify active operation state: {error}"
    );
}

#[test]
fn adopt_dirty_rejects_a_stale_snapshot_challenge() {
    let repo = init_repo();
    let state_home = tempfile::TempDir::new().expect("state home");
    let dirty_path = repo.path().join("dirty.txt");
    let reason_file = state_home.path().join("adoption-reason.txt");
    fs::write(&dirty_path, "first dirty state\n").expect("write dirty file");
    fs::write(&reason_file, "Need to preserve user-owned changes.\n").expect("write reason file");
    let snapshot = dirty_snapshot(repo.path()).expect("snapshot dirty checkout");
    let _challenge_path = write_challenge(state_home.path(), &snapshot);
    fs::write(&dirty_path, "changed after challenge\n").expect("change dirty file");

    let error = adopt_dirty(
        repo.path(),
        state_home.path(),
        CHALLENGE_TOKEN,
        &reason_file,
    )
    .expect_err("adoption must reject a challenge for an older dirty snapshot");

    assert!(
        error.to_string().contains("challenge"),
        "stale challenge error should identify the failed authorization: {error}"
    );
}

#[test]
fn adopt_dirty_rejects_a_live_foreign_v1_lease() {
    let repo = init_repo();
    let state_home = tempfile::TempDir::new().expect("state home");
    let reason_file = state_home.path().join("adoption-reason.txt");
    fs::write(repo.path().join("dirty.txt"), "user-owned dirty state\n").expect("write dirty file");
    fs::write(&reason_file, "Need to preserve user-owned changes.\n").expect("write reason file");
    let snapshot = dirty_snapshot(repo.path()).expect("snapshot dirty checkout");
    let _challenge_path = write_challenge(state_home.path(), &snapshot);
    let state_dir = checkout_state_dir(state_home.path(), &snapshot);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time")
        .as_secs();
    let foreign_lease = json!({
        "schema": "agent-runtime.checkout-lease.v1",
        "session_key": "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
        "checkout_instance": &snapshot.checkout_instance,
        "checkout_root": repo.path(),
        "checkout_git_dir": repo.path().join(".git"),
        "acquired_at": now,
        "refreshed_at": now,
        "expires_at": now + 3600,
    });
    let lease_path = state_dir.join("lease.json");
    fs::write(&lease_path, format!("{foreign_lease}\n")).expect("write foreign v1 lease");
    fs::set_permissions(&lease_path, fs::Permissions::from_mode(0o600))
        .expect("make foreign lease private");

    let error = adopt_dirty(
        repo.path(),
        state_home.path(),
        CHALLENGE_TOKEN,
        &reason_file,
    )
    .expect_err("live foreign v1 lease must block adoption");

    assert!(
        error.to_string().contains("another session"),
        "error should preserve foreign ownership: {error}"
    );
}

#[test]
fn adoption_returns_opaque_receipt_and_revocation_is_receipt_bound() {
    let repo = init_repo();
    let state_home = tempfile::TempDir::new().expect("state home");
    let dirty_path = repo.path().join("dirty.txt");
    let reason_file = state_home.path().join("adoption-reason.txt");
    fs::write(&dirty_path, "user-owned dirty state\n").expect("write dirty file");
    fs::write(&reason_file, "Need to preserve user-owned changes.\n").expect("write reason file");
    let status_before = git(
        repo.path(),
        &["status", "--porcelain=v1", "--untracked-files=all"],
    );
    let snapshot = dirty_snapshot(repo.path()).expect("snapshot dirty checkout");
    let _challenge_path = write_challenge(state_home.path(), &snapshot);

    let receipt = adopt_dirty(
        repo.path(),
        state_home.path(),
        CHALLENGE_TOKEN,
        &reason_file,
    )
    .expect("adopt matching dirty checkout");

    assert!(
        !receipt.receipt_id.trim().is_empty(),
        "receipt ID must be non-empty"
    );
    assert_eq!(receipt.snapshot_id, snapshot.snapshot_id);
    let state_dir = checkout_state_dir(state_home.path(), &snapshot);
    let lease_path = state_dir.join("lease.json");
    let receipt_path = state_dir
        .join("receipts")
        .join(format!("{}.json", receipt.receipt_id));
    let lease_text = fs::read_to_string(&lease_path).expect("read adopted lease");
    let receipt_text = fs::read_to_string(&receipt_path).expect("read adoption receipt");
    let lease: serde_json::Value = serde_json::from_str(&lease_text).expect("parse lease");
    assert_eq!(lease["schema"], "agent-runtime.checkout-lease.v2");
    assert_eq!(
        lease["adoption"]["schema"],
        "agent-runtime.dirty-checkout-adoption.v1"
    );
    assert_eq!(lease["adoption"]["snapshot_id"], snapshot.snapshot_id);
    for retained in [&lease_text, &receipt_text] {
        assert!(!retained.contains(CHALLENGE_TOKEN));
        assert!(!retained.contains("Need to preserve user-owned changes."));
    }
    assert_eq!(
        fs::metadata(&lease_path)
            .expect("lease metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(&receipt_path)
            .expect("receipt metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert!(
        adopt_dirty(
            repo.path(),
            state_home.path(),
            CHALLENGE_TOKEN,
            &reason_file,
        )
        .is_err(),
        "a consumed challenge must never authorize a second adoption"
    );
    assert!(
        revoke_dirty(repo.path(), state_home.path(), "not-the-issued-receipt").is_err(),
        "revocation must reject a receipt not issued for this adoption"
    );
    revoke_dirty(repo.path(), state_home.path(), &receipt.receipt_id)
        .expect("issued receipt must revoke its adoption");
    assert_eq!(
        git(
            repo.path(),
            &["status", "--porcelain=v1", "--untracked-files=all"]
        ),
        status_before,
        "adoption and revocation must not mutate the checkout"
    );
}

#[test]
fn dirty_snapshot_rejects_clean_and_special_object_states() {
    let clean_repo = init_repo();
    let clean_error =
        dirty_snapshot(clean_repo.path()).expect_err("clean checkout must be rejected");
    assert!(clean_error.to_string().contains("clean"));

    let special_repo = init_repo();
    fs::write(special_repo.path().join("dirty.txt"), "dirty\n").expect("write dirty fixture");
    let fifo_path = special_repo.path().join("unsupported-fifo");
    let fifo_path = std::ffi::CString::new(fifo_path.as_os_str().as_bytes()).expect("FIFO path");
    let result = unsafe { libc::mkfifo(fifo_path.as_ptr(), 0o600) };
    assert_eq!(result, 0, "create FIFO fixture");
    let special_error =
        dirty_snapshot(special_repo.path()).expect_err("special objects must be rejected");
    assert!(
        special_error.to_string().contains("special filesystem"),
        "unexpected error: {special_error}"
    );
}

#[test]
fn dirty_snapshot_binds_staged_index_content() {
    let repo = init_repo();
    let staged_path = repo.path().join("staged.txt");
    fs::write(&staged_path, "first staged state\n").expect("write staged file");
    git(repo.path(), &["add", "--", "staged.txt"]);
    let first = dirty_snapshot(repo.path()).expect("snapshot first staged state");

    fs::write(&staged_path, "second staged state\n").expect("change staged file");
    git(repo.path(), &["add", "--", "staged.txt"]);
    let second = dirty_snapshot(repo.path()).expect("snapshot second staged state");

    assert_ne!(first.snapshot_id, second.snapshot_id);
}

#[test]
fn adopt_dirty_rejects_expired_and_non_private_challenges() {
    let expired_repo = init_repo();
    let expired_state = tempfile::TempDir::new().expect("expired state home");
    let expired_reason = expired_state.path().join("reason.txt");
    fs::write(expired_repo.path().join("dirty.txt"), "dirty\n").expect("write dirty file");
    fs::write(&expired_reason, "Preserve these changes.\n").expect("write reason file");
    let expired_snapshot = dirty_snapshot(expired_repo.path()).expect("snapshot dirty checkout");
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time")
        .as_secs();
    let _expired_path = write_challenge_window(
        expired_state.path(),
        &expired_snapshot,
        now.saturating_sub(301),
        now.saturating_sub(1),
    );
    let expired_error = adopt_dirty(
        expired_repo.path(),
        expired_state.path(),
        CHALLENGE_TOKEN,
        &expired_reason,
    )
    .expect_err("expired challenge must be rejected");
    assert!(expired_error.to_string().contains("expired"));

    let public_repo = init_repo();
    let public_state = tempfile::TempDir::new().expect("public state home");
    let public_reason = public_state.path().join("reason.txt");
    fs::write(public_repo.path().join("dirty.txt"), "dirty\n").expect("write dirty file");
    fs::write(&public_reason, "Preserve these changes.\n").expect("write reason file");
    let public_snapshot = dirty_snapshot(public_repo.path()).expect("snapshot dirty checkout");
    let public_path = write_challenge(public_state.path(), &public_snapshot);
    fs::set_permissions(&public_path, fs::Permissions::from_mode(0o644))
        .expect("make challenge non-private");
    let public_error = adopt_dirty(
        public_repo.path(),
        public_state.path(),
        CHALLENGE_TOKEN,
        &public_reason,
    )
    .expect_err("non-private challenge must be rejected");
    assert!(public_error.to_string().contains("private"));
}

#[test]
fn adopt_dirty_rejects_symlink_reason_files() {
    let repo = init_repo();
    let state_home = tempfile::TempDir::new().expect("state home");
    fs::write(repo.path().join("dirty.txt"), "dirty\n").expect("write dirty file");
    let reason_target = state_home.path().join("reason-target.txt");
    let reason_link = state_home.path().join("reason-link.txt");
    fs::write(&reason_target, "Preserve these changes.\n").expect("write reason target");
    symlink(&reason_target, &reason_link).expect("create reason symlink");
    let snapshot = dirty_snapshot(repo.path()).expect("snapshot dirty checkout");
    let _challenge_path = write_challenge(state_home.path(), &snapshot);

    let error = adopt_dirty(
        repo.path(),
        state_home.path(),
        CHALLENGE_TOKEN,
        &reason_link,
    )
    .expect_err("reason symlink must be rejected");

    assert!(
        error.to_string().contains("opened safely"),
        "unexpected error: {error}"
    );
}

#[test]
fn governed_cli_enforces_gate_and_returns_private_json_contracts() {
    let harness = GitCliHarness::new();
    let repo = init_repo();
    let state_home = tempfile::TempDir::new().expect("state home");
    let reason_file = state_home.path().join("adoption-reason.txt");
    let reason_text = "Preserve these user-owned changes.\n";
    fs::write(repo.path().join("dirty.txt"), "dirty state\n").expect("write dirty file");
    fs::write(&reason_file, reason_text).expect("write reason file");
    let snapshot = dirty_snapshot(repo.path()).expect("snapshot dirty checkout");
    let challenge_path = write_challenge(state_home.path(), &snapshot);
    let reason_arg = reason_file.to_string_lossy().to_string();

    let disabled = run_governed_command(
        &harness,
        repo.path(),
        state_home.path(),
        false,
        &[
            "worktree",
            "adopt-dirty",
            "--challenge",
            CHALLENGE_TOKEN,
            "--reason-file",
            &reason_arg,
            "--format",
            "json",
        ],
    );
    assert_ne!(disabled.code, 0);
    assert_eq!(disabled.stderr_text(), "");
    let disabled_json: serde_json::Value =
        serde_json::from_str(disabled.stdout_text().trim()).expect("disabled JSON envelope");
    assert_eq!(
        disabled_json["schema_version"],
        "cli.git-cli.worktree.adopt-dirty.v1"
    );
    assert_eq!(disabled_json["ok"], false);
    assert_eq!(
        disabled_json["error"]["code"],
        "dirty-checkout-adoption-disabled"
    );
    assert!(
        challenge_path.exists(),
        "gate refusal must not consume challenge"
    );

    let adopted = run_governed_command(
        &harness,
        repo.path(),
        state_home.path(),
        true,
        &[
            "worktree",
            "adopt-dirty",
            "--challenge",
            CHALLENGE_TOKEN,
            "--reason-file",
            &reason_arg,
            "--format=json",
        ],
    );
    assert_eq!(adopted.code, 0, "stderr: {}", adopted.stderr_text());
    assert_eq!(adopted.stderr_text(), "");
    let adopted_text = adopted.stdout_text();
    assert!(!adopted_text.contains(CHALLENGE_TOKEN));
    assert!(!adopted_text.contains(reason_text.trim()));
    assert!(!adopted_text.contains(&repo.path().to_string_lossy().to_string()));
    let adopted_json: serde_json::Value =
        serde_json::from_str(adopted_text.trim()).expect("adoption JSON envelope");
    assert_eq!(
        adopted_json["schema_version"],
        "cli.git-cli.worktree.adopt-dirty.v1"
    );
    assert_eq!(adopted_json["ok"], true);
    let receipt = adopted_json["data"]["receipt_id"]
        .as_str()
        .expect("receipt ID");
    assert_eq!(receipt.len(), 64);
    assert!(
        !challenge_path.exists(),
        "successful adoption consumes challenge"
    );

    let revoked = run_governed_command(
        &harness,
        repo.path(),
        state_home.path(),
        false,
        &[
            "worktree",
            "revoke-dirty",
            "--receipt",
            receipt,
            "--format=json",
        ],
    );
    assert_eq!(revoked.code, 0, "stderr: {}", revoked.stderr_text());
    let revoked_json: serde_json::Value =
        serde_json::from_str(revoked.stdout_text().trim()).expect("revocation JSON envelope");
    assert_eq!(
        revoked_json["schema_version"],
        "cli.git-cli.worktree.revoke-dirty.v1"
    );
    assert_eq!(revoked_json["data"]["revoked"], true);
}

#[test]
fn dirty_snapshot_json_omits_raw_checkout_paths() {
    let harness = GitCliHarness::new();
    let repo = init_repo();
    let state_home = tempfile::TempDir::new().expect("state home");
    fs::write(repo.path().join("dirty.txt"), "dirty\n").expect("write dirty file");

    let output = run_governed_command(
        &harness,
        repo.path(),
        state_home.path(),
        false,
        &["worktree", "dirty-snapshot", "--format=json"],
    );

    assert_eq!(output.code, 0, "stderr: {}", output.stderr_text());
    let text = output.stdout_text();
    assert!(!text.contains(&repo.path().to_string_lossy().to_string()));
    let value: serde_json::Value =
        serde_json::from_str(text.trim()).expect("snapshot JSON envelope");
    assert_eq!(
        value["schema_version"],
        "cli.git-cli.worktree.dirty-snapshot.v1"
    );
    assert_eq!(
        value["data"]["schema"],
        "agent-runtime.dirty-checkout-snapshot.v1"
    );
}
