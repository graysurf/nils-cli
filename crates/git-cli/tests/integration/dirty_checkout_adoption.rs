use crate::common::{GitCliHarness, git, init_repo};
use git_cli::worktree::dirty_checkout_adoption::{
    DirtyCheckoutError, DirtyCheckoutErrorKind, DirtySnapshot, adopt_dirty, dirty_snapshot,
    revoke_dirty,
};
use nils_test_support::cmd::{CmdOutput, run_with};
#[cfg(target_os = "linux")]
use nils_test_support::git::{InitRepoOptions, init_repo_at_with};
use pretty_assertions::assert_eq;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
#[cfg(target_os = "linux")]
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

const CHALLENGE_TOKEN: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const CHALLENGE_TOKEN_DIGEST: &str =
    "ffe054fe7ae0cb6dc65c3af9b61d5209f439851db43d0ba5997337df154668eb";
const SESSION_KEY: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const AUTHORIZATION_TURN_DIGEST: &str =
    "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

fn private_state_home() -> tempfile::TempDir {
    let parent = fs::canonicalize(std::env::temp_dir()).expect("canonicalize platform temp root");
    tempfile::Builder::new()
        .prefix("git-cli-dirty-checkout-state-")
        .tempdir_in(parent)
        .expect("create private state home")
}

#[test]
fn dirty_checkout_state_home_fixture_is_canonical() {
    let state_home = private_state_home();

    assert_eq!(
        fs::canonicalize(state_home.path()).expect("canonical state home"),
        state_home.path()
    );
}

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
    let state_dir = checkout_state_dir(state_root, snapshot);
    let repository_dir = state_dir.parent().expect("repository state directory");
    let challenge_dir = state_dir.join("challenges");
    for directory in [
        state_root,
        repository_dir,
        state_dir.as_path(),
        challenge_dir.as_path(),
    ] {
        fs::create_dir_all(directory).expect("create challenge directory hierarchy");
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700))
            .expect("make challenge directory hierarchy private");
    }
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
    let first_name = std::ffi::OsString::from_vec(b"native-\xfe-path".to_vec());
    let second_name = std::ffi::OsString::from_vec(b"native-\xff-path".to_vec());
    let first_path = repo.path().join(first_name);
    let second_path = repo.path().join(second_name);
    fs::write(&first_path, b"native path content\n").expect("write first native path");
    let first = dirty_snapshot(repo.path()).expect("snapshot first non-UTF-8 path");

    fs::rename(&first_path, &second_path).expect("replace native path bytes");
    let second = dirty_snapshot(repo.path()).expect("snapshot second non-UTF-8 path");

    assert_eq!(first.untracked_entries, 1);
    assert_eq!(first.hashed_bytes, 20);
    assert_eq!(second.untracked_entries, 1);
    assert_eq!(second.hashed_bytes, 20);
    assert_ne!(
        first.snapshot_id, second.snapshot_id,
        "snapshot identity must bind the authoritative native path bytes"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn adoption_supports_a_non_utf8_checkout_root_on_linux() {
    let checkout_parent = tempfile::TempDir::new().expect("checkout parent");
    let checkout_name = std::ffi::OsString::from_vec(b"checkout-\xff".to_vec());
    let checkout = checkout_parent.path().join(checkout_name);
    fs::create_dir(&checkout).expect("create non-UTF-8 checkout root");
    assert!(
        checkout.to_str().is_none(),
        "fixture root must not be UTF-8"
    );
    init_repo_at_with(
        &checkout,
        InitRepoOptions::new()
            .with_branch("main")
            .with_initial_commit(),
    );
    fs::write(checkout.join("dirty.txt"), "dirty checkout state\n")
        .expect("write dirty checkout fixture");
    let status_before = git(
        &checkout,
        &["status", "--porcelain=v1", "--untracked-files=all"],
    );
    let snapshot = dirty_snapshot(&checkout).expect("snapshot non-UTF-8 checkout root");
    let state_home = private_state_home();
    let reason_file = state_home.path().join("reason.txt");
    fs::write(&reason_file, "Preserve the dirty checkout.\n").expect("write reason");
    let _challenge_path = write_challenge(state_home.path(), &snapshot);

    let receipt = adopt_dirty(&checkout, state_home.path(), CHALLENGE_TOKEN, &reason_file)
        .expect("adopt non-UTF-8 checkout root");
    let lease_path = checkout_state_dir(state_home.path(), &snapshot).join("lease.json");
    let lease: serde_json::Value =
        serde_json::from_slice(&fs::read(&lease_path).expect("read non-UTF-8 checkout lease"))
            .expect("parse non-UTF-8 checkout lease");
    assert_eq!(lease["checkout_root_bytes"], path_hex(&checkout));
    assert_eq!(
        lease["checkout_git_dir_bytes"],
        path_hex(&checkout.join(".git"))
    );
    revoke_dirty(&checkout, state_home.path(), &receipt.receipt_id)
        .expect("revoke non-UTF-8 checkout adoption");

    assert_eq!(
        git(
            &checkout,
            &["status", "--porcelain=v1", "--untracked-files=all"]
        ),
        status_before,
        "adoption lifecycle must not mutate the checkout"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn real_cli_worker_lifecycle_supports_a_non_utf8_checkout_root() {
    let checkout_parent = tempfile::TempDir::new().expect("CLI checkout parent");
    let checkout = checkout_parent
        .path()
        .join(std::ffi::OsString::from_vec(b"cli-checkout-\xff".to_vec()));
    fs::create_dir(&checkout).expect("create CLI non-UTF-8 checkout root");
    init_repo_at_with(
        &checkout,
        InitRepoOptions::new()
            .with_branch("main")
            .with_initial_commit(),
    );
    fs::write(checkout.join("dirty.txt"), b"dirty CLI checkout\n")
        .expect("write CLI dirty checkout fixture");
    let status_before = git(
        &checkout,
        &["status", "--porcelain=v1", "--untracked-files=all"],
    );
    let harness = GitCliHarness::new();
    let state_home = private_state_home();
    let reason_file = state_home.path().join("reason.txt");
    fs::write(&reason_file, b"Preserve the native checkout.\n").expect("write CLI reason");

    let snapshot_output = run_governed_command(
        &harness,
        &checkout,
        state_home.path(),
        false,
        &["worktree", "dirty-snapshot", "--format=json"],
    );
    assert_eq!(
        snapshot_output.code,
        0,
        "stderr: {}",
        snapshot_output.stderr_text()
    );
    let snapshot_json: serde_json::Value =
        serde_json::from_str(snapshot_output.stdout_text().trim()).expect("CLI snapshot JSON");
    let data = &snapshot_json["data"];
    let snapshot = DirtySnapshot {
        schema: "agent-runtime.dirty-checkout-snapshot.v1",
        repository_key: data["repository_key"]
            .as_str()
            .expect("repository key")
            .to_string(),
        checkout_key: data["checkout_key"]
            .as_str()
            .expect("checkout key")
            .to_string(),
        checkout_instance: data["checkout_instance"]
            .as_str()
            .expect("checkout instance")
            .to_string(),
        snapshot_id: data["snapshot_id"]
            .as_str()
            .expect("snapshot ID")
            .to_string(),
        head_oid: data["head_oid"]
            .as_str()
            .expect("HEAD identity")
            .to_string(),
        branch_ref_digest: data["branch_ref_digest"]
            .as_str()
            .expect("branch digest")
            .to_string(),
        tracked_entries: data["tracked_entries"].as_u64().expect("tracked count") as usize,
        untracked_entries: data["untracked_entries"].as_u64().expect("untracked count") as usize,
        hashed_bytes: data["hashed_bytes"].as_u64().expect("hashed bytes"),
    };
    write_challenge(state_home.path(), &snapshot);
    let reason_arg = reason_file.to_string_lossy().to_string();
    let adopted = run_governed_command(
        &harness,
        &checkout,
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
    let adopted_json: serde_json::Value =
        serde_json::from_str(adopted.stdout_text().trim()).expect("CLI adoption JSON");
    let receipt_id = adopted_json["data"]["receipt_id"]
        .as_str()
        .expect("CLI receipt ID");
    let lease_path = checkout_state_dir(state_home.path(), &snapshot).join("lease.json");
    let lease: serde_json::Value =
        serde_json::from_slice(&fs::read(&lease_path).expect("read CLI native lease"))
            .expect("parse CLI native lease");
    assert_eq!(lease["checkout_root_bytes"], path_hex(&checkout));
    assert_eq!(
        lease["checkout_git_dir_bytes"],
        path_hex(&checkout.join(".git"))
    );

    let revoked = run_governed_command(
        &harness,
        &checkout,
        state_home.path(),
        false,
        &[
            "worktree",
            "revoke-dirty",
            "--receipt",
            receipt_id,
            "--format=json",
        ],
    );
    assert_eq!(revoked.code, 0, "stderr: {}", revoked.stderr_text());
    assert_eq!(
        git(
            &checkout,
            &["status", "--porcelain=v1", "--untracked-files=all"]
        ),
        status_before,
        "real CLI lifecycle must preserve the non-UTF-8 checkout"
    );
}

#[test]
fn public_snapshot_api_does_not_require_a_colocated_git_cli_binary() {
    const CHILD_ENV: &str = "NILS_GIT_CLI_PUBLIC_SNAPSHOT_CONSUMER";
    const CHECKOUT_ENV: &str = "NILS_GIT_CLI_PUBLIC_SNAPSHOT_CHECKOUT";
    if std::env::var_os(CHILD_ENV).is_some() {
        let checkout = std::env::var_os(CHECKOUT_ENV).expect("standalone checkout path");
        dirty_snapshot(std::path::Path::new(&checkout))
            .expect("public snapshot API works without a colocated CLI binary");
        return;
    }

    let repo = init_repo();
    fs::write(repo.path().join("dirty.txt"), b"dirty\n").expect("write dirty file");
    let isolated = tempfile::TempDir::new().expect("standalone consumer directory");
    let consumer = isolated.path().join("public-api-consumer");
    fs::copy(
        std::env::current_exe().expect("current test executable"),
        &consumer,
    )
    .expect("copy standalone consumer");
    fs::set_permissions(&consumer, fs::Permissions::from_mode(0o755))
        .expect("make standalone consumer executable");

    let launch_deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
    let status = loop {
        match Command::new(&consumer)
            .arg("public_snapshot_api_does_not_require_a_colocated_git_cli_binary")
            .arg("--nocapture")
            .env(CHILD_ENV, "1")
            .env(CHECKOUT_ENV, repo.path())
            .env("RUST_TEST_THREADS", "1")
            .status()
        {
            Ok(status) => break status,
            Err(error)
                if error.raw_os_error() == Some(libc::ETXTBSY)
                    && std::time::Instant::now() < launch_deadline =>
            {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(error) => panic!("run standalone public API consumer: {error}"),
        }
    };

    assert!(
        status.success(),
        "standalone public API consumer failed: {status}"
    );
}

#[test]
fn snapshot_cli_does_not_resolve_git_from_an_injected_path_entry() {
    let repo = init_repo();
    fs::write(repo.path().join("dirty.txt"), b"dirty\n").expect("write dirty file");
    let root = tempfile::TempDir::new().expect("injected Git root");
    let bin = root.path().join("bin");
    fs::create_dir(&bin).expect("create injected Git bin directory");
    let marker = root.path().join("git-invoked");
    let fake_git = bin.join("git");
    fs::write(
        &fake_git,
        format!(
            "#!/bin/sh\nprintf invoked > '{}'\nexit 99\n",
            marker.display()
        ),
    )
    .expect("write injected Git executable");
    fs::set_permissions(&fake_git, fs::Permissions::from_mode(0o755))
        .expect("make injected Git executable runnable");
    let harness = GitCliHarness::new();
    let options = harness.cmd_options(repo.path()).with_path_prepend(&bin);

    let output = run_with(
        &harness.git_cli_bin(),
        &["worktree", "dirty-snapshot", "--format", "json"],
        &options,
    );

    assert_eq!(output.code, 0, "stderr: {}", output.stderr_text());
    assert!(
        !marker.exists(),
        "snapshot executed a PATH-injected Git binary"
    );
}

fn prepare_command_filter_fixture(
    repo: &std::path::Path,
    helper_root: &std::path::Path,
) -> (std::path::PathBuf, std::path::PathBuf) {
    fs::write(repo.join("tracked.txt"), b"base\n").expect("write tracked file");
    fs::write(
        repo.join(".gitattributes"),
        b"tracked.txt filter=probe diff=probe\n",
    )
    .expect("write attributes");
    git(repo, &["add", "tracked.txt", ".gitattributes"]);
    git(repo, &["commit", "-qm", "add filtered file"]);
    let marker = helper_root.join("filter-invoked");
    let helper = helper_root.join("filter-helper");
    fs::write(
        &helper,
        format!("#!/bin/sh\ncat\nprintf invoked > '{}'\n", marker.display()),
    )
    .expect("write filter helper");
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o755))
        .expect("make filter helper executable");
    fs::write(repo.join("tracked.txt"), b"changed\n").expect("dirty filtered file");
    (marker, helper)
}

#[test]
fn dirty_snapshot_rejects_command_bearing_filters_without_launching_them() {
    for key in [
        "filter.probe.clean",
        "filter.probe.smudge",
        "filter.probe.process",
        "diff.probe.command",
        "diff.probe.textconv",
    ] {
        let repo = init_repo();
        let helper_root = tempfile::TempDir::new().expect("command helper root");
        let (marker, helper) = prepare_command_filter_fixture(repo.path(), helper_root.path());
        git(
            repo.path(),
            &["config", key, helper.to_str().expect("UTF-8 helper path")],
        );

        let error = dirty_snapshot(repo.path())
            .expect_err("command-bearing configuration must fail closed");
        assert!(!marker.exists(), "snapshot launched helper for {key}");
        assert_eq!(
            error
                .downcast_ref::<DirtyCheckoutError>()
                .unwrap_or_else(|| panic!("typed rejection for {key}"))
                .kind(),
            DirtyCheckoutErrorKind::UnsupportedGitState,
            "unexpected rejection for {key}"
        );
    }
}

#[test]
fn dirty_snapshot_rejects_canonical_conditional_includes_without_helper_execution() {
    let repo = init_repo();
    let helper_root = tempfile::TempDir::new().expect("conditional include helper root");
    let (marker, helper) = prepare_command_filter_fixture(repo.path(), helper_root.path());
    let included = helper_root.path().join("included.gitconfig");
    fs::write(
        &included,
        format!("[filter \"probe\"]\n\tclean = {}\n", helper.display()),
    )
    .expect("write conditionally included config");
    let branch = git(repo.path(), &["branch", "--show-current"]);
    let key = format!("includeif.onbranch:{}.path", branch.trim());
    git(
        repo.path(),
        &[
            "config",
            &key,
            included.to_str().expect("UTF-8 include path"),
        ],
    );

    let result = dirty_snapshot(repo.path());
    let kind = result
        .as_ref()
        .err()
        .and_then(|error| error.downcast_ref::<DirtyCheckoutError>())
        .map(DirtyCheckoutError::kind);

    assert_eq!(
        (kind, marker.exists()),
        (Some(DirtyCheckoutErrorKind::UnsupportedGitState), false),
        "canonical conditional include must be rejected without helper execution"
    );
}

#[test]
fn dirty_snapshot_rejects_effective_worktree_config_without_helper_execution() {
    let repo = init_repo();
    let helper_root = tempfile::TempDir::new().expect("worktree config helper root");
    let (marker, helper) = prepare_command_filter_fixture(repo.path(), helper_root.path());
    git(
        repo.path(),
        &["config", "extensions.worktreeConfig", "true"],
    );
    git(
        repo.path(),
        &[
            "config",
            "--worktree",
            "filter.probe.clean",
            helper.to_str().expect("UTF-8 helper path"),
        ],
    );

    let result = dirty_snapshot(repo.path());
    let kind = result
        .as_ref()
        .err()
        .and_then(|error| error.downcast_ref::<DirtyCheckoutError>())
        .map(DirtyCheckoutError::kind);

    assert_eq!(
        (kind, marker.exists()),
        (Some(DirtyCheckoutErrorKind::UnsupportedGitState), false),
        "effective config.worktree command must be rejected without helper execution"
    );
}

#[test]
fn dirty_snapshot_rejects_submodule_local_commands_without_helper_execution() {
    let child = init_repo();
    fs::write(child.path().join("tracked.txt"), b"base\n").expect("write child tracked file");
    fs::write(
        child.path().join(".gitattributes"),
        b"tracked.txt filter=probe\n",
    )
    .expect("write child attributes");
    git(child.path(), &["add", "tracked.txt", ".gitattributes"]);
    git(child.path(), &["commit", "-qm", "add filtered child file"]);

    let repo = init_repo();
    let child_path = child.path().to_string_lossy().to_string();
    git(
        repo.path(),
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            &child_path,
            "modules/child",
        ],
    );
    git(repo.path(), &["commit", "-qam", "add child submodule"]);
    let child_checkout = repo.path().join("modules/child");
    let helper_root = tempfile::TempDir::new().expect("submodule helper root");
    let marker = helper_root.path().join("filter-invoked");
    let helper = helper_root.path().join("filter-helper");
    fs::write(
        &helper,
        format!("#!/bin/sh\ncat\nprintf invoked > '{}'\n", marker.display()),
    )
    .expect("write child filter helper");
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o755))
        .expect("make child helper executable");
    git(
        &child_checkout,
        &[
            "config",
            "filter.probe.clean",
            helper.to_str().expect("UTF-8 helper path"),
        ],
    );
    fs::write(child_checkout.join("tracked.txt"), b"changed\n").expect("dirty filtered child file");

    let result = dirty_snapshot(repo.path());
    let kind = result
        .as_ref()
        .err()
        .and_then(|error| error.downcast_ref::<DirtyCheckoutError>())
        .map(DirtyCheckoutError::kind);

    assert_eq!(
        (kind, marker.exists()),
        (Some(DirtyCheckoutErrorKind::UnsupportedGitState), false),
        "submodule command-bearing config must be rejected without helper execution"
    );
}

#[test]
fn dirty_snapshot_rejects_nested_submodule_commands_without_helper_execution() {
    let nested = init_repo();
    fs::write(nested.path().join("tracked.txt"), b"base\n").expect("write nested tracked file");
    fs::write(
        nested.path().join(".gitattributes"),
        b"tracked.txt filter=probe\n",
    )
    .expect("write nested attributes");
    git(nested.path(), &["add", "tracked.txt", ".gitattributes"]);
    git(
        nested.path(),
        &["commit", "-qm", "add nested filtered file"],
    );

    let child = init_repo();
    let nested_path = nested.path().to_string_lossy().to_string();
    git(
        child.path(),
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            &nested_path,
            "modules/nested",
        ],
    );
    git(child.path(), &["commit", "-qam", "add nested submodule"]);

    let repo = init_repo();
    let child_path = child.path().to_string_lossy().to_string();
    git(
        repo.path(),
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            &child_path,
            "modules/child",
        ],
    );
    git(repo.path(), &["commit", "-qam", "add child submodule"]);
    git(
        repo.path(),
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "update",
            "--init",
            "--recursive",
        ],
    );

    let nested_checkout = repo.path().join("modules/child/modules/nested");
    let helper_root = tempfile::TempDir::new().expect("nested submodule helper root");
    let marker = helper_root.path().join("filter-invoked");
    let helper = helper_root.path().join("filter-helper");
    fs::write(
        &helper,
        format!("#!/bin/sh\ncat\nprintf invoked > '{}'\n", marker.display()),
    )
    .expect("write nested filter helper");
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o755))
        .expect("make nested helper executable");
    git(
        &nested_checkout,
        &[
            "config",
            "filter.probe.clean",
            helper.to_str().expect("UTF-8 helper path"),
        ],
    );
    fs::write(nested_checkout.join("tracked.txt"), b"changed\n")
        .expect("dirty nested filtered file");

    let result = dirty_snapshot(repo.path());
    let kind = result
        .as_ref()
        .err()
        .and_then(|error| error.downcast_ref::<DirtyCheckoutError>())
        .map(DirtyCheckoutError::kind);

    assert_eq!(
        (kind, marker.exists()),
        (Some(DirtyCheckoutErrorKind::UnsupportedGitState), false),
        "nested submodule command-bearing config must be rejected without helper execution"
    );
}

#[test]
fn dirty_snapshot_maps_file_size_limits_to_resource_unavailable() {
    let repo = init_repo();
    let oversized = repo.path().join("oversized.bin");
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&oversized)
        .expect("create sparse oversized file")
        .set_len(1024 * 1024 * 1024 + 1)
        .expect("size sparse oversized file");

    let error = dirty_snapshot(repo.path()).expect_err("oversized file must fail closed");

    assert_eq!(
        error
            .downcast_ref::<DirtyCheckoutError>()
            .expect("typed size-limit error")
            .kind(),
        DirtyCheckoutErrorKind::ResourceUnavailable
    );
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
    let state_home = private_state_home();
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
    let state_home = private_state_home();
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
    let state_home = private_state_home();
    let dirty_path = repo.path().join("dirty.txt");
    let reason_file = state_home.path().join("adoption-reason.txt");
    fs::write(&dirty_path, "user-owned dirty state\n").expect("write dirty file");
    fs::write(&reason_file, "Need to preserve user-owned changes.\n").expect("write reason file");
    let status_before = git(
        repo.path(),
        &["status", "--porcelain=v1", "--untracked-files=all"],
    );
    let snapshot = dirty_snapshot(repo.path()).expect("snapshot dirty checkout");
    let challenge_path = write_challenge(state_home.path(), &snapshot);
    let challenge_bytes = fs::read(&challenge_path).expect("read exact challenge artifact");
    let challenge_artifact_digest = sha256_hex(&challenge_bytes);
    assert_ne!(
        challenge_artifact_digest, CHALLENGE_TOKEN_DIGEST,
        "challenge artifact and bearer token require distinct digest domains"
    );

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
    let receipt_record: serde_json::Value =
        serde_json::from_str(&receipt_text).expect("parse receipt");
    let spent_challenge_path = state_dir
        .join("receipts")
        .join(format!(".challenge-{}.json", receipt.receipt_id));
    assert_eq!(
        fs::read(&spent_challenge_path).expect("read spent challenge artifact"),
        challenge_bytes,
        "challenge consumption must preserve the exact challenged artifact"
    );
    assert_eq!(
        receipt_record["challenge_digest"], challenge_artifact_digest,
        "receipt must bind the exact consumed challenge artifact"
    );
    assert_eq!(
        lease["adoption"]["challenge_digest"], challenge_artifact_digest,
        "lease adoption must bind the exact consumed challenge artifact"
    );
    assert_eq!(lease["schema"], "agent-runtime.checkout-lease.v2");
    assert_eq!(
        lease["adoption"]["schema"],
        "agent-runtime.dirty-checkout-adoption.v1"
    );
    assert_eq!(
        lease["adoption"]["receipt_schema"],
        "agent-runtime.dirty-checkout-receipt.v1"
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
    let retry = adopt_dirty(
        repo.path(),
        state_home.path(),
        CHALLENGE_TOKEN,
        &reason_file,
    )
    .expect("an identical lost-response retry must return the committed receipt");
    assert_eq!(retry, receipt);
    fs::write(&reason_file, "A different authorization reason.\n").expect("change retry reason");
    adopt_dirty(
        repo.path(),
        state_home.path(),
        CHALLENGE_TOKEN,
        &reason_file,
    )
    .expect_err("a changed retry must not alias the committed authorization");

    let mismatched_receipt_id = "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";
    let mut mismatched_receipt: serde_json::Value =
        serde_json::from_str(&receipt_text).expect("parse receipt fixture");
    mismatched_receipt["receipt_id"] = json!(mismatched_receipt_id);
    let mismatched_receipt_path = state_dir
        .join("receipts")
        .join(format!("{mismatched_receipt_id}.json"));
    fs::write(&mismatched_receipt_path, format!("{mismatched_receipt}\n"))
        .expect("write mismatched receipt");
    fs::set_permissions(&mismatched_receipt_path, fs::Permissions::from_mode(0o600))
        .expect("make mismatched receipt private");

    revoke_dirty(repo.path(), state_home.path(), mismatched_receipt_id)
        .expect_err("well-formed mismatched receipt must not revoke the adoption");
    assert_eq!(
        fs::read_to_string(&lease_path).expect("lease retained after mismatch"),
        lease_text,
        "mismatched revocation must preserve the complete lease"
    );
    assert!(receipt_path.exists(), "issued receipt must be preserved");

    revoke_dirty(repo.path(), state_home.path(), &receipt.receipt_id)
        .expect("issued receipt must revoke its adoption");
    assert!(
        !lease_path.exists(),
        "successful revocation removes the lease"
    );
    assert!(
        !receipt_path.exists(),
        "successful revocation removes the receipt"
    );
    revoke_dirty(repo.path(), state_home.path(), &receipt.receipt_id)
        .expect("repeating a committed receipt-bound revocation must be idempotent");
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
fn identical_lost_response_retry_returns_the_committed_receipt() {
    let repo = init_repo();
    let state_home = private_state_home();
    let reason_file = state_home.path().join("reason.txt");
    fs::write(repo.path().join("dirty.txt"), "dirty\n").expect("write dirty file");
    fs::write(&reason_file, "Preserve the dirty checkout.\n").expect("write reason");
    let snapshot = dirty_snapshot(repo.path()).expect("snapshot dirty checkout");
    let _challenge_path = write_challenge(state_home.path(), &snapshot);
    let committed = adopt_dirty(
        repo.path(),
        state_home.path(),
        CHALLENGE_TOKEN,
        &reason_file,
    )
    .expect("commit adoption before simulated lost response");

    let retried = adopt_dirty(
        repo.path(),
        state_home.path(),
        CHALLENGE_TOKEN,
        &reason_file,
    )
    .expect("identical lost-response retry must succeed");

    assert_eq!(retried, committed);
}

#[test]
fn committed_retry_revalidates_exact_artifacts_and_current_snapshot() {
    for tamper in ["receipt", "spent-challenge", "checkout"] {
        let repo = init_repo();
        let state_home = private_state_home();
        let reason_file = state_home.path().join("reason.txt");
        let dirty_path = repo.path().join("dirty.txt");
        fs::write(&dirty_path, "dirty\n").expect("write dirty file");
        fs::write(&reason_file, "Preserve the dirty checkout.\n").expect("write reason");
        let snapshot = dirty_snapshot(repo.path()).expect("snapshot dirty checkout");
        write_challenge(state_home.path(), &snapshot);
        let receipt = adopt_dirty(
            repo.path(),
            state_home.path(),
            CHALLENGE_TOKEN,
            &reason_file,
        )
        .expect("commit adoption before retry tamper");
        let state_dir = checkout_state_dir(state_home.path(), &snapshot);
        let lease_path = state_dir.join("lease.json");
        let receipt_path = state_dir
            .join("receipts")
            .join(format!("{}.json", receipt.receipt_id));
        let spent_path = state_dir
            .join("receipts")
            .join(format!(".challenge-{}.json", receipt.receipt_id));
        let pending_path =
            state_dir.join(format!(".pending-adoption-{CHALLENGE_TOKEN_DIGEST}.json"));
        assert!(
            !pending_path.exists(),
            "committed transition must be settled"
        );

        match tamper {
            "receipt" => {
                let mut record: serde_json::Value = serde_json::from_slice(
                    &fs::read(&receipt_path).expect("read committed receipt"),
                )
                .expect("parse committed receipt");
                record["reason_digest"] = json!("d".repeat(64));
                write_private_json(&receipt_path, &record);
            }
            "spent-challenge" => {
                let mut bytes = fs::read(&spent_path).expect("read exact spent challenge");
                bytes.push(b' ');
                fs::write(&spent_path, bytes).expect("tamper exact spent challenge bytes");
            }
            "checkout" => {
                fs::write(&dirty_path, "changed after commit\n")
                    .expect("change committed checkout snapshot");
            }
            _ => unreachable!(),
        }
        let lease_before = fs::read(&lease_path).expect("read lease before retry");
        let receipt_before = fs::read(&receipt_path).expect("read receipt before retry");
        let spent_before = fs::read(&spent_path).expect("read spent challenge before retry");

        let error = adopt_dirty(
            repo.path(),
            state_home.path(),
            CHALLENGE_TOKEN,
            &reason_file,
        )
        .expect_err("tampered committed retry must fail closed");
        assert!(
            error.downcast_ref::<DirtyCheckoutError>().is_some(),
            "{tamper} retry must preserve a typed domain error: {error}"
        );

        assert_eq!(
            fs::read(&lease_path).expect("read lease after rejected retry"),
            lease_before,
            "{tamper} retry must preserve the committed lease"
        );
        assert_eq!(
            fs::read(&receipt_path).expect("read receipt after rejected retry"),
            receipt_before,
            "{tamper} retry must preserve receipt state"
        );
        assert_eq!(
            fs::read(&spent_path).expect("read spent challenge after rejected retry"),
            spent_before,
            "{tamper} retry must preserve exact spent-challenge state"
        );
        assert!(
            !pending_path.exists(),
            "{tamper} retry must not start a new transition"
        );
    }
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
    let expired_state = private_state_home();
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
    let public_state = private_state_home();
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
    let state_home = private_state_home();
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
fn governed_cli_feature_gate_accepts_only_exact_one() {
    let harness = GitCliHarness::new();
    let repo = init_repo();
    let state_home = private_state_home();
    let state_root = state_home.path().to_string_lossy().to_string();
    let args = [
        "worktree",
        "adopt-dirty",
        "--challenge",
        CHALLENGE_TOKEN,
        "--reason-file",
        "/nonexistent/adoption-reason.txt",
        "--format=json",
    ];

    for rejected in ["true", "TRUE", "yes", "YES", "01", "1 ", " 1"] {
        let options = harness
            .cmd_options(repo.path())
            .with_env("AGENT_RUNTIME_CHECKOUT_LEASE_STATE_HOME", &state_root)
            .with_env("AGENT_RUNTIME_DIRTY_CHECKOUT_ADOPTION", rejected);
        let output = run_with(&harness.git_cli_bin(), &args, &options);
        assert_json_error(
            &output,
            "dirty-checkout-adoption-disabled",
            nils_common::cli_contract::exit::DATA,
        );
    }

    let options = harness
        .cmd_options(repo.path())
        .with_env("AGENT_RUNTIME_CHECKOUT_LEASE_STATE_HOME", &state_root)
        .with_env("AGENT_RUNTIME_DIRTY_CHECKOUT_ADOPTION", "1");
    let accepted = run_with(&harness.git_cli_bin(), &args, &options);
    let (_, code) = json_error_identity(&accepted);
    assert_ne!(code, "dirty-checkout-adoption-disabled");
}

#[test]
fn governed_cli_enforces_gate_and_returns_private_json_contracts() {
    let harness = GitCliHarness::new();
    let repo = init_repo();
    let state_home = private_state_home();
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
    assert_exact_json_keys(
        &adopted_json,
        &["schema_version", "ok", "data"],
        "adoption success envelope",
    );
    assert_exact_json_keys(
        &adopted_json["data"],
        &["receipt_id", "snapshot_id"],
        "adoption success payload",
    );
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
    assert_exact_json_keys(
        &revoked_json,
        &["schema_version", "ok", "data"],
        "revocation success envelope",
    );
    assert_exact_json_keys(
        &revoked_json["data"],
        &["receipt_id", "revoked"],
        "revocation success payload",
    );
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
    let state_home = private_state_home();
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
    assert_exact_json_keys(
        &value,
        &["schema_version", "ok", "data"],
        "snapshot success envelope",
    );
    assert_exact_json_keys(
        &value["data"],
        &[
            "schema",
            "repository_key",
            "checkout_key",
            "checkout_instance",
            "snapshot_id",
            "head_oid",
            "branch_ref_digest",
            "tracked_entries",
            "untracked_entries",
            "hashed_bytes",
        ],
        "snapshot success payload",
    );
    assert_eq!(
        value["schema_version"],
        "cli.git-cli.worktree.dirty-snapshot.v1"
    );
    assert_eq!(
        value["data"]["schema"],
        "agent-runtime.dirty-checkout-snapshot.v1"
    );
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time")
        .as_secs()
}

fn write_challenge_for_digest(
    state_root: &std::path::Path,
    snapshot: &DirtySnapshot,
    token_digest: &str,
) -> std::path::PathBuf {
    let state_dir = checkout_state_dir(state_root, snapshot);
    let repository_dir = state_dir.parent().expect("repository state directory");
    let challenge_dir = state_dir.join("challenges");
    for directory in [
        state_root,
        repository_dir,
        state_dir.as_path(),
        challenge_dir.as_path(),
    ] {
        fs::create_dir_all(directory).expect("create challenge hierarchy");
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700))
            .expect("make challenge hierarchy private");
    }
    let issued_at = unix_now();
    let challenge = json!({
        "schema": "agent-runtime.dirty-checkout-challenge.v1",
        "token_digest": token_digest,
        "session_key": SESSION_KEY,
        "repository_key": &snapshot.repository_key,
        "checkout_key": &snapshot.checkout_key,
        "checkout_instance": &snapshot.checkout_instance,
        "snapshot_id": &snapshot.snapshot_id,
        "head_oid": &snapshot.head_oid,
        "branch_ref_digest": &snapshot.branch_ref_digest,
        "authorization_turn_digest": AUTHORIZATION_TURN_DIGEST,
        "issued_at": issued_at,
        "expires_at": issued_at + 300,
    });
    let challenge_path = challenge_dir.join(format!("{token_digest}.json"));
    fs::write(&challenge_path, format!("{challenge}\n")).expect("write challenge fixture");
    fs::set_permissions(&challenge_path, fs::Permissions::from_mode(0o600))
        .expect("make challenge private");
    challenge_path
}

fn write_private_json(path: &std::path::Path, value: &serde_json::Value) {
    fs::write(path, format!("{value}\n")).expect("write private JSON fixture");
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .expect("make JSON fixture private");
}

fn path_hex(path: &std::path::Path) -> String {
    path.as_os_str()
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn assert_exact_json_keys(value: &serde_json::Value, expected: &[&str], label: &str) {
    let mut actual: Vec<_> = value
        .as_object()
        .unwrap_or_else(|| panic!("{label} must be a JSON object"))
        .keys()
        .map(String::as_str)
        .collect();
    actual.sort_unstable();
    let mut expected = expected.to_vec();
    expected.sort_unstable();
    assert_eq!(actual, expected, "{label} key set");
}

fn json_error_identity(output: &CmdOutput) -> (i32, String) {
    assert_eq!(output.stderr_text(), "");
    let value: serde_json::Value =
        serde_json::from_str(output.stdout_text().trim()).expect("JSON error envelope");
    assert_eq!(value["ok"], false);
    (
        output.code,
        value["error"]["code"]
            .as_str()
            .expect("JSON error code")
            .to_string(),
    )
}

fn assert_json_error(output: &CmdOutput, expected_code: &str, expected_exit: i32) {
    assert_eq!(
        output.code,
        expected_exit,
        "stderr: {}",
        output.stderr_text()
    );
    assert_eq!(output.stderr_text(), "");
    let value: serde_json::Value =
        serde_json::from_str(output.stdout_text().trim()).expect("JSON error envelope");
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"]["code"], expected_code);
}

#[test]
fn dirty_checkout_cli_uses_stable_domain_error_codes_and_exits() {
    let harness = GitCliHarness::new();
    let clean_repo = init_repo();
    let clean = harness.run(
        clean_repo.path(),
        &["worktree", "dirty-snapshot", "--format=json"],
    );
    assert_json_error(
        &clean,
        "dirty-checkout-clean",
        nils_common::cli_contract::exit::DATA,
    );

    let operation_repo = init_repo();
    fs::write(operation_repo.path().join("dirty.txt"), "dirty\n").expect("write dirty file");
    let git_dir = git(operation_repo.path(), &["rev-parse", "--absolute-git-dir"]);
    fs::write(std::path::Path::new(git_dir.trim()).join("index.lock"), "")
        .expect("write active operation marker");
    let unsupported = harness.run(
        operation_repo.path(),
        &["worktree", "dirty-snapshot", "--format=json"],
    );
    assert_json_error(
        &unsupported,
        "dirty-checkout-unsupported-git-state",
        nils_common::cli_contract::exit::DATA,
    );
}

#[test]
fn dirty_checkout_cli_maps_resource_limits_to_runtime_exit_class() {
    let harness = GitCliHarness::new();
    let repo = init_repo();
    let oversized = fs::File::create(repo.path().join("oversized-sparse.bin"))
        .expect("create sparse oversized fixture");
    oversized
        .set_len(1024 * 1024 * 1024 + 1)
        .expect("size sparse oversized fixture");

    let output = harness.run(
        repo.path(),
        &["worktree", "dirty-snapshot", "--format=json"],
    );

    assert_json_error(
        &output,
        "dirty-checkout-resource-unavailable",
        nils_common::cli_contract::exit::RUNTIME,
    );
}

#[test]
fn dirty_checkout_cli_maps_expected_fail_closed_boundaries_to_domain_errors() {
    let harness = GitCliHarness::new();

    let link_repo = init_repo();
    symlink("/etc/passwd", link_repo.path().join("escape")).expect("create escaping link fixture");
    let link_output = harness.run(
        link_repo.path(),
        &["worktree", "dirty-snapshot", "--format=json"],
    );
    assert_json_error(
        &link_output,
        "dirty-checkout-unsupported-git-state",
        nils_common::cli_contract::exit::DATA,
    );

    let reason_repo = init_repo();
    let reason_state = private_state_home();
    fs::write(reason_repo.path().join("dirty.txt"), "dirty\n").expect("write dirty fixture");
    let reason_snapshot = dirty_snapshot(reason_repo.path()).expect("reason snapshot");
    let reason_challenge = write_challenge(reason_state.path(), &reason_snapshot);
    let reason_target = reason_state.path().join("reason-target.txt");
    let reason_link = reason_state.path().join("reason-link.txt");
    fs::write(&reason_target, "Preserve changes.\n").expect("write reason target");
    symlink(&reason_target, &reason_link).expect("create reason symlink");
    let reason_arg = reason_link.to_string_lossy().to_string();
    let reason_output = run_governed_command(
        &harness,
        reason_repo.path(),
        reason_state.path(),
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
    assert_json_error(
        &reason_output,
        "dirty-checkout-invalid-input",
        nils_common::cli_contract::exit::DATA,
    );
    assert!(reason_challenge.exists());

    let challenge_repo = init_repo();
    let challenge_state = private_state_home();
    let challenge_reason = challenge_state.path().join("reason.txt");
    fs::write(challenge_repo.path().join("dirty.txt"), "dirty\n").expect("write dirty fixture");
    fs::write(&challenge_reason, "Preserve changes.\n").expect("write reason");
    let challenge_snapshot = dirty_snapshot(challenge_repo.path()).expect("challenge snapshot");
    let challenge_path = write_challenge(challenge_state.path(), &challenge_snapshot);
    fs::set_permissions(&challenge_path, fs::Permissions::from_mode(0o644))
        .expect("make challenge public");
    let challenge_reason_arg = challenge_reason.to_string_lossy().to_string();
    let challenge_output = run_governed_command(
        &harness,
        challenge_repo.path(),
        challenge_state.path(),
        true,
        &[
            "worktree",
            "adopt-dirty",
            "--challenge",
            CHALLENGE_TOKEN,
            "--reason-file",
            &challenge_reason_arg,
            "--format=json",
        ],
    );
    assert_json_error(
        &challenge_output,
        "dirty-checkout-malformed-state",
        nils_common::cli_contract::exit::DATA,
    );

    let root_repo = init_repo();
    let root_state = private_state_home();
    let root_reason = root_state.path().join("reason.txt");
    fs::write(root_repo.path().join("dirty.txt"), "dirty\n").expect("write dirty fixture");
    fs::write(&root_reason, "Preserve changes.\n").expect("write reason");
    let root_snapshot = dirty_snapshot(root_repo.path()).expect("root snapshot");
    let root_challenge = write_challenge(root_state.path(), &root_snapshot);
    fs::set_permissions(root_state.path(), fs::Permissions::from_mode(0o755))
        .expect("make state root public");
    let root_reason_arg = root_reason.to_string_lossy().to_string();
    let root_output = run_governed_command(
        &harness,
        root_repo.path(),
        root_state.path(),
        true,
        &[
            "worktree",
            "adopt-dirty",
            "--challenge",
            CHALLENGE_TOKEN,
            "--reason-file",
            &root_reason_arg,
            "--format=json",
        ],
    );
    assert_json_error(
        &root_output,
        "dirty-checkout-invalid-input",
        nils_common::cli_contract::exit::DATA,
    );
    assert!(root_challenge.exists());
    fs::set_permissions(root_state.path(), fs::Permissions::from_mode(0o700))
        .expect("restore state root mode");
}

#[test]
fn adopt_dirty_argument_errors_never_reflect_bearer_or_path_values() {
    let harness = GitCliHarness::new();
    let repo = init_repo();
    let bearer = "bearer-value-that-must-not-leak";
    let reason_path = "/private/reasons/customer-secret.txt";
    let stray = "secret-positional-value";

    for format in [None, Some("--format=json")] {
        let mut args = vec![
            "worktree",
            "adopt-dirty",
            "--challenge",
            bearer,
            "--reason-file",
            reason_path,
            stray,
        ];
        if let Some(format) = format {
            args.push(format);
        }
        let output = harness.run(repo.path(), &args);
        assert_ne!(output.code, 0);
        let rendered = format!("{}{}", output.stdout_text(), output.stderr_text());
        for secret in [bearer, reason_path, stray] {
            assert!(
                !rendered.contains(secret),
                "malformed argument output reflected {secret:?}: {rendered}"
            );
        }
    }
}

#[test]
fn revoke_dirty_argument_errors_never_reflect_receipt_format_or_stray_values() {
    let harness = GitCliHarness::new();
    let repo = init_repo();
    let receipt = "receipt-value-that-must-not-leak";
    let stray = "secret-positional-value";

    for format in [None, Some("--format=json")] {
        let mut args = vec!["worktree", "revoke-dirty", "--receipt", receipt, stray];
        if let Some(format) = format {
            args.push(format);
        }
        let output = harness.run(repo.path(), &args);
        assert_ne!(output.code, 0);
        let rendered = format!("{}{}", output.stdout_text(), output.stderr_text());
        for secret in [receipt, stray] {
            assert!(
                !rendered.contains(secret),
                "malformed argument output reflected {secret:?}: {rendered}"
            );
        }
    }

    let invalid_format = "secret-output-format";
    let output = harness.run(
        repo.path(),
        &[
            "worktree",
            "revoke-dirty",
            "--receipt",
            receipt,
            "--format",
            invalid_format,
        ],
    );
    assert_ne!(output.code, 0);
    let rendered = format!("{}{}", output.stdout_text(), output.stderr_text());
    for secret in [receipt, invalid_format] {
        assert!(
            !rendered.contains(secret),
            "malformed format output reflected {secret:?}: {rendered}"
        );
    }
}

#[test]
fn adopt_dirty_requires_nonempty_utf8_reason_before_state_transition() {
    let repo = init_repo();
    let state_home = private_state_home();
    fs::write(repo.path().join("dirty.txt"), "dirty\n").expect("write dirty file");
    let snapshot = dirty_snapshot(repo.path()).expect("snapshot dirty checkout");
    let challenge_path = write_challenge(state_home.path(), &snapshot);
    let reason_file = state_home.path().join("reason.bin");
    fs::write(&reason_file, [0xff, 0xfe]).expect("write non-UTF-8 reason");

    adopt_dirty(
        repo.path(),
        state_home.path(),
        CHALLENGE_TOKEN,
        &reason_file,
    )
    .expect_err("non-UTF-8 reason must be rejected");

    let state_dir = checkout_state_dir(state_home.path(), &snapshot);
    assert!(
        challenge_path.exists(),
        "invalid reason must not be consumed"
    );
    assert!(!state_dir.join("lease.json").exists());
    assert!(!state_dir.join("receipts").exists());
}

#[test]
fn adopt_dirty_treats_an_empty_existing_lease_as_malformed_state() {
    let harness = GitCliHarness::new();
    let repo = init_repo();
    let state_home = private_state_home();
    let reason_file = state_home.path().join("reason.txt");
    fs::write(repo.path().join("dirty.txt"), "dirty\n").expect("write dirty file");
    fs::write(&reason_file, "Preserve the dirty checkout.\n").expect("write reason");
    let snapshot = dirty_snapshot(repo.path()).expect("snapshot dirty checkout");
    let challenge_path = write_challenge(state_home.path(), &snapshot);
    let lease_path = checkout_state_dir(state_home.path(), &snapshot).join("lease.json");
    fs::write(&lease_path, "").expect("write empty lease");
    fs::set_permissions(&lease_path, fs::Permissions::from_mode(0o600))
        .expect("make empty lease private");
    let reason_arg = reason_file.to_string_lossy().to_string();

    let output = run_governed_command(
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

    assert_json_error(
        &output,
        "dirty-checkout-malformed-state",
        nils_common::cli_contract::exit::DATA,
    );
    assert!(
        challenge_path.exists(),
        "malformed lease must preserve challenge"
    );
    assert_eq!(fs::read(&lease_path).expect("read empty lease"), b"");
}

#[test]
fn adopt_dirty_strictly_rejects_unknown_v1_lease_fields() {
    let repo = init_repo();
    let state_home = private_state_home();
    let reason_file = state_home.path().join("reason.txt");
    fs::write(repo.path().join("dirty.txt"), "dirty\n").expect("write dirty file");
    fs::write(&reason_file, "Preserve the dirty checkout.\n").expect("write reason");
    let snapshot = dirty_snapshot(repo.path()).expect("snapshot dirty checkout");
    let challenge_path = write_challenge(state_home.path(), &snapshot);
    let now = unix_now();
    let lease = json!({
        "schema": "agent-runtime.checkout-lease.v1",
        "session_key": SESSION_KEY,
        "checkout_instance": &snapshot.checkout_instance,
        "checkout_root": repo.path(),
        "checkout_git_dir": repo.path().join(".git"),
        "acquired_at": now,
        "refreshed_at": now,
        "expires_at": now + 60,
        "unexpected": "field",
    });
    let lease_path = checkout_state_dir(state_home.path(), &snapshot).join("lease.json");
    write_private_json(&lease_path, &lease);

    adopt_dirty(
        repo.path(),
        state_home.path(),
        CHALLENGE_TOKEN,
        &reason_file,
    )
    .expect_err("unknown v1 lease field must be rejected");

    assert!(challenge_path.exists());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&fs::read(&lease_path).expect("read lease"))
            .expect("parse retained lease"),
        lease
    );
}

#[test]
fn revoke_dirty_rejects_v2_raw_paths_that_disagree_with_text_paths() {
    let repo = init_repo();
    let state_home = private_state_home();
    let reason_file = state_home.path().join("reason.txt");
    fs::write(repo.path().join("dirty.txt"), "dirty\n").expect("write dirty file");
    fs::write(&reason_file, "Preserve the dirty checkout.\n").expect("write reason");
    let snapshot = dirty_snapshot(repo.path()).expect("snapshot dirty checkout");
    let _challenge_path = write_challenge(state_home.path(), &snapshot);
    let receipt = adopt_dirty(
        repo.path(),
        state_home.path(),
        CHALLENGE_TOKEN,
        &reason_file,
    )
    .expect("adopt dirty checkout");
    let lease_path = checkout_state_dir(state_home.path(), &snapshot).join("lease.json");
    let mut lease: serde_json::Value =
        serde_json::from_slice(&fs::read(&lease_path).expect("read lease")).expect("parse lease");
    lease["checkout_root_bytes"] = json!(path_hex(std::path::Path::new("/different")));
    write_private_json(&lease_path, &lease);

    revoke_dirty(repo.path(), state_home.path(), &receipt.receipt_id)
        .expect_err("mismatched v2 raw path must be rejected");

    assert!(lease_path.exists(), "malformed lease must be preserved");
}

#[test]
fn replacing_an_expired_adoption_cleans_its_exact_predecessor_state() {
    const SECOND_TOKEN: &str = "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";
    const SECOND_DIGEST: &str = "d91323a5298f3b9f814db29efaa271f24fbdccedfdd062491b8abc8e07b7fb69";

    let repo = init_repo();
    let state_home = private_state_home();
    let reason_file = state_home.path().join("reason.txt");
    let dirty_path = repo.path().join("dirty.txt");
    fs::write(&dirty_path, "first dirty state\n").expect("write dirty file");
    fs::write(&reason_file, "Preserve the dirty checkout.\n").expect("write reason");
    let first_snapshot = dirty_snapshot(repo.path()).expect("first snapshot");
    let _first_challenge = write_challenge(state_home.path(), &first_snapshot);
    let first_receipt = adopt_dirty(
        repo.path(),
        state_home.path(),
        CHALLENGE_TOKEN,
        &reason_file,
    )
    .expect("first adoption");
    let state_dir = checkout_state_dir(state_home.path(), &first_snapshot);
    let lease_path = state_dir.join("lease.json");
    let mut expired_lease: serde_json::Value =
        serde_json::from_slice(&fs::read(&lease_path).expect("read first lease"))
            .expect("parse first lease");
    let adopted_at = expired_lease["adoption"]["adopted_at"]
        .as_u64()
        .expect("adopted_at");
    expired_lease["refreshed_at"] = json!(adopted_at);
    expired_lease["expires_at"] = json!(adopted_at);
    write_private_json(&lease_path, &expired_lease);

    fs::write(&dirty_path, "second dirty state\n").expect("change dirty file");
    let second_snapshot = dirty_snapshot(repo.path()).expect("second snapshot");
    let _second_challenge =
        write_challenge_for_digest(state_home.path(), &second_snapshot, SECOND_DIGEST);

    let second_receipt = adopt_dirty(repo.path(), state_home.path(), SECOND_TOKEN, &reason_file)
        .expect("replace expired adoption");

    assert_ne!(second_receipt.receipt_id, first_receipt.receipt_id);
    let receipts_dir = state_dir.join("receipts");
    assert!(
        !receipts_dir
            .join(format!("{}.json", first_receipt.receipt_id))
            .exists(),
        "expired predecessor receipt must be removed after replacement"
    );
    assert!(
        !receipts_dir
            .join(format!(".challenge-{}.json", first_receipt.receipt_id))
            .exists(),
        "expired predecessor spent challenge must be removed after replacement"
    );
    assert!(
        receipts_dir
            .join(format!("{}.json", second_receipt.receipt_id))
            .exists(),
        "current receipt must be retained"
    );
}

fn assert_expired_predecessor_cleanup_resumes(missing_artifact: &str) {
    const SECOND_TOKEN: &str = "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";
    const SECOND_DIGEST: &str = "d91323a5298f3b9f814db29efaa271f24fbdccedfdd062491b8abc8e07b7fb69";

    let repo = init_repo();
    let state_home = private_state_home();
    let reason_file = state_home.path().join("reason.txt");
    let dirty_path = repo.path().join("dirty.txt");
    fs::write(&dirty_path, "first dirty state\n").expect("write dirty file");
    fs::write(&reason_file, "Preserve the dirty checkout.\n").expect("write reason");
    let first_snapshot = dirty_snapshot(repo.path()).expect("first snapshot");
    let _first_challenge = write_challenge(state_home.path(), &first_snapshot);
    let first_receipt = adopt_dirty(
        repo.path(),
        state_home.path(),
        CHALLENGE_TOKEN,
        &reason_file,
    )
    .expect("first adoption");
    let state_dir = checkout_state_dir(state_home.path(), &first_snapshot);
    let lease_path = state_dir.join("lease.json");
    let mut expired_lease: serde_json::Value =
        serde_json::from_slice(&fs::read(&lease_path).expect("read first lease"))
            .expect("parse first lease");
    let adopted_at = expired_lease["adoption"]["adopted_at"]
        .as_u64()
        .expect("adopted_at");
    expired_lease["refreshed_at"] = json!(adopted_at);
    expired_lease["expires_at"] = json!(adopted_at);
    write_private_json(&lease_path, &expired_lease);

    let receipts_dir = state_dir.join("receipts");
    let predecessor_receipt_path = receipts_dir.join(format!("{}.json", first_receipt.receipt_id));
    let predecessor_spent_path =
        receipts_dir.join(format!(".challenge-{}.json", first_receipt.receipt_id));
    match missing_artifact {
        "receipt" => fs::remove_file(&predecessor_receipt_path)
            .expect("simulate durable predecessor receipt cleanup"),
        "spent-challenge" => fs::remove_file(&predecessor_spent_path)
            .expect("simulate durable predecessor challenge cleanup"),
        other => panic!("unsupported predecessor fixture {other}"),
    }
    fs::File::open(&receipts_dir)
        .expect("open predecessor receipt directory")
        .sync_all()
        .expect("make partial predecessor cleanup durable");

    fs::write(&dirty_path, "second dirty state\n").expect("change dirty file");
    let second_snapshot = dirty_snapshot(repo.path()).expect("second snapshot");
    let _second_challenge =
        write_challenge_for_digest(state_home.path(), &second_snapshot, SECOND_DIGEST);

    let second_receipt = adopt_dirty(repo.path(), state_home.path(), SECOND_TOKEN, &reason_file)
        .unwrap_or_else(|error| {
            panic!("replacement must resume after predecessor {missing_artifact} cleanup: {error}")
        });

    assert_ne!(second_receipt.receipt_id, first_receipt.receipt_id);
    assert!(!predecessor_receipt_path.exists());
    assert!(!predecessor_spent_path.exists());
}

#[test]
fn predecessor_cleanup_resumes_after_receipt_deletion() {
    assert_expired_predecessor_cleanup_resumes("receipt");
}

#[test]
fn predecessor_cleanup_resumes_after_spent_challenge_deletion() {
    assert_expired_predecessor_cleanup_resumes("spent-challenge");
}

#[test]
fn concurrent_adoption_consumes_a_challenge_exactly_once() {
    let repo = init_repo();
    let state_home = private_state_home();
    let reason_file = state_home.path().join("reason.txt");
    fs::write(repo.path().join("dirty.txt"), "dirty\n").expect("write dirty file");
    fs::write(&reason_file, "Preserve the dirty checkout.\n").expect("write reason");
    let snapshot = dirty_snapshot(repo.path()).expect("snapshot dirty checkout");
    let _challenge_path = write_challenge(state_home.path(), &snapshot);
    let state_dir = checkout_state_dir(state_home.path(), &snapshot);
    let lease_lock_path = state_dir.join("lease.lock");
    fs::write(&lease_lock_path, b"").expect("create real lease lock boundary");
    fs::set_permissions(&lease_lock_path, fs::Permissions::from_mode(0o600))
        .expect("make lease lock private");
    let lease_lock = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&lease_lock_path)
        .expect("open real lease lock boundary");
    assert_eq!(
        unsafe { libc::flock(lease_lock.as_raw_fd(), libc::LOCK_EX) },
        0,
        "acquire real lease lock boundary"
    );
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
    let mut workers = Vec::new();
    for _ in 0..2 {
        let checkout = repo.path().to_path_buf();
        let state_root = state_home.path().to_path_buf();
        let reason = reason_file.clone();
        let barrier = std::sync::Arc::clone(&barrier);
        workers.push(std::thread::spawn(move || {
            barrier.wait();
            adopt_dirty(&checkout, &state_root, CHALLENGE_TOKEN, &reason)
        }));
    }
    barrier.wait();
    std::thread::sleep(std::time::Duration::from_millis(100));
    assert_eq!(
        unsafe { libc::flock(lease_lock.as_raw_fd(), libc::LOCK_UN) },
        0,
        "release real lease lock boundary"
    );
    let results: Vec<_> = workers
        .into_iter()
        .map(|worker| worker.join().expect("adoption worker"))
        .collect();

    assert_eq!(
        results.iter().filter(|result| result.is_ok()).count(),
        2,
        "concurrent adoption results: {results:?}"
    );
    let receipts: Vec<_> = results
        .iter()
        .map(|result| {
            result
                .as_ref()
                .expect("identical concurrent retry succeeds")
        })
        .collect();
    assert_eq!(
        receipts[0], receipts[1],
        "identical concurrent retries must observe one committed receipt"
    );
    let winner = receipts[0];

    let challenge_path = state_dir
        .join("challenges")
        .join(format!("{CHALLENGE_TOKEN_DIGEST}.json"));
    assert!(
        !challenge_path.exists(),
        "winning adoption consumes challenge"
    );
    let lease_path = state_dir.join("lease.json");
    assert!(lease_path.exists(), "exactly one lease must be installed");
    let lease: serde_json::Value =
        serde_json::from_slice(&fs::read(&lease_path).expect("read winning lease"))
            .expect("parse winning lease");
    assert_eq!(lease["adoption"]["receipt_id"], winner.receipt_id);
    let receipt_entries: Vec<_> = fs::read_dir(state_dir.join("receipts"))
        .expect("list receipts")
        .map(|entry| entry.expect("receipt entry").file_name())
        .collect();
    assert_eq!(
        receipt_entries
            .iter()
            .filter(|name| !name.as_bytes().starts_with(b"."))
            .count(),
        1,
        "exactly one ordinary receipt must exist"
    );
    assert_eq!(
        receipt_entries
            .iter()
            .filter(|name| name.as_bytes().starts_with(b".challenge-"))
            .count(),
        1,
        "exactly one spent challenge must exist"
    );
    let replay = adopt_dirty(
        repo.path(),
        state_home.path(),
        CHALLENGE_TOKEN,
        &reason_file,
    )
    .expect("a later identical retry must return the committed receipt");
    assert_eq!(replay, *winner);
}

fn assert_revocation_resumes_after_tombstone(
    retained_receipt: bool,
    retained_spent_challenge: bool,
    fault_boundary: &str,
) {
    let repo = init_repo();
    let state_home = private_state_home();
    let reason_file = state_home.path().join("reason.txt");
    fs::write(repo.path().join("dirty.txt"), "dirty\n").expect("write dirty file");
    fs::write(&reason_file, "Preserve the dirty checkout.\n").expect("write reason");
    let snapshot = dirty_snapshot(repo.path()).expect("snapshot dirty checkout");
    let _challenge_path = write_challenge(state_home.path(), &snapshot);
    let receipt = adopt_dirty(
        repo.path(),
        state_home.path(),
        CHALLENGE_TOKEN,
        &reason_file,
    )
    .expect("adopt dirty checkout");
    let state_dir = checkout_state_dir(state_home.path(), &snapshot);
    let receipts_dir = state_dir.join("receipts");
    let lease_path = state_dir.join("lease.json");
    let receipt_path = receipts_dir.join(format!("{}.json", receipt.receipt_id));
    let spent_path = receipts_dir.join(format!(".challenge-{}.json", receipt.receipt_id));
    let tombstone_path = state_dir.join(format!(".revoked-{}.json", receipt.receipt_id));

    fs::rename(&lease_path, &tombstone_path).expect("commit simulated revocation tombstone");
    fs::File::open(&state_dir)
        .expect("open checkout state directory")
        .sync_all()
        .expect("make simulated revocation tombstone durable");
    if !retained_receipt {
        fs::remove_file(&receipt_path).expect("simulate receipt cleanup");
    }
    if !retained_spent_challenge {
        fs::remove_file(&spent_path).expect("simulate spent challenge cleanup");
    }
    if !retained_receipt || !retained_spent_challenge {
        fs::File::open(&receipts_dir)
            .expect("open revocation receipt directory")
            .sync_all()
            .expect("make partial revocation cleanup durable");
    }
    if !retained_spent_challenge {
        for directory in [state_dir.join("challenges"), state_dir.clone()] {
            fs::File::open(directory)
                .expect("open revocation cleanup directory")
                .sync_all()
                .expect("make revocation cleanup boundary durable");
        }
    }

    revoke_dirty(repo.path(), state_home.path(), &receipt.receipt_id).unwrap_or_else(|error| {
        panic!("revocation retry must resume after {fault_boundary}: {error}")
    });

    assert!(tombstone_path.exists(), "durable tombstone must remain");
    assert!(!lease_path.exists(), "revoked lease must not be restored");
    assert!(!receipt_path.exists(), "receipt cleanup must converge");
    assert!(
        !spent_path.exists(),
        "spent challenge cleanup must converge"
    );
}

#[test]
fn revocation_retry_resumes_immediately_after_tombstone_commit() {
    assert_revocation_resumes_after_tombstone(true, true, "tombstone commit");
}

#[test]
fn revocation_retry_resumes_after_receipt_cleanup() {
    assert_revocation_resumes_after_tombstone(false, true, "receipt cleanup");
}

#[test]
fn revocation_retry_resumes_after_spent_challenge_cleanup() {
    assert_revocation_resumes_after_tombstone(true, false, "spent challenge cleanup");
}

#[test]
fn revocation_retry_is_idempotent_after_artifact_cleanup_and_sync_faults() {
    assert_revocation_resumes_after_tombstone(false, false, "artifact cleanup or directory sync");
}

#[test]
fn revocation_crash_state_never_restores_a_consumed_challenge() {
    let repo = init_repo();
    let state_home = private_state_home();
    let reason_file = state_home.path().join("reason.txt");
    fs::write(repo.path().join("dirty.txt"), "dirty\n").expect("write dirty file");
    fs::write(&reason_file, "Preserve the dirty checkout.\n").expect("write reason");
    let snapshot = dirty_snapshot(repo.path()).expect("snapshot dirty checkout");
    let _challenge_path = write_challenge(state_home.path(), &snapshot);
    let receipt = adopt_dirty(
        repo.path(),
        state_home.path(),
        CHALLENGE_TOKEN,
        &reason_file,
    )
    .expect("adopt dirty checkout");
    let state_dir = checkout_state_dir(state_home.path(), &snapshot);
    let receipts_dir = state_dir.join("receipts");
    let lease_path = state_dir.join("lease.json");
    let lease: serde_json::Value =
        serde_json::from_slice(&fs::read(&lease_path).expect("read active lease"))
            .expect("parse active lease");
    let spent_path = receipts_dir.join(format!(".challenge-{}.json", receipt.receipt_id));
    let spent: serde_json::Value =
        serde_json::from_slice(&fs::read(&spent_path).expect("read spent challenge"))
            .expect("parse spent challenge");
    let pending_path = state_dir.join(format!(
        ".pending-adoption-{}.json",
        spent["token_digest"].as_str().expect("token digest")
    ));
    let pending = json!({
        "schema": "agent-runtime.dirty-checkout-pending.v1",
        "receipt_id": &receipt.receipt_id,
        "token_digest": &spent["token_digest"],
        "challenge_digest": &lease["adoption"]["challenge_digest"],
        "session_key": &lease["session_key"],
        "checkout_instance": &lease["checkout_instance"],
        "snapshot_id": &lease["adoption"]["snapshot_id"],
        "predecessor_receipt_id": null,
        "predecessor_receipt_digest": null,
        "predecessor_spent_challenge_digest": null,
    });
    write_private_json(&pending_path, &pending);

    revoke_dirty(repo.path(), state_home.path(), &receipt.receipt_id)
        .expect("commit revocation before simulated cleanup crash");
    let tombstone_path = state_dir.join(format!(".revoked-{}.json", receipt.receipt_id));
    write_private_json(&tombstone_path, &lease);
    write_private_json(&spent_path, &spent);
    write_private_json(&pending_path, &pending);

    let replay = adopt_dirty(
        repo.path(),
        state_home.path(),
        CHALLENGE_TOKEN,
        &reason_file,
    )
    .expect_err("revoked challenge must remain consumed after crash recovery");

    assert_eq!(
        replay
            .downcast_ref::<DirtyCheckoutError>()
            .expect("typed revoked challenge replay")
            .kind(),
        DirtyCheckoutErrorKind::ChallengeReused
    );
    assert!(
        tombstone_path.exists(),
        "revocation tombstone must be retained"
    );
    assert!(
        !state_dir
            .join("challenges")
            .join(format!("{CHALLENGE_TOKEN_DIGEST}.json"))
            .exists(),
        "revocation recovery must not restore the live challenge"
    );
}

#[test]
fn state_root_rejection_is_verify_only_and_does_not_create_transition_files() {
    let repo = init_repo();
    let state_root = repo.path().join("runtime-state-inside-checkout");
    let reason_file = repo.path().join("reason.txt");
    fs::write(repo.path().join("dirty.txt"), "dirty\n").expect("write dirty file");
    fs::write(&reason_file, "Preserve the dirty checkout.\n").expect("write reason");
    let snapshot = dirty_snapshot(repo.path()).expect("snapshot dirty checkout");
    let challenge_path = write_challenge(&state_root, &snapshot);
    let state_dir = checkout_state_dir(&state_root, &snapshot);

    adopt_dirty(repo.path(), &state_root, CHALLENGE_TOKEN, &reason_file)
        .expect_err("state root inside checkout must be rejected");

    assert!(challenge_path.exists());
    for forbidden in ["lease.lock", "lease.json"] {
        assert!(
            !state_dir.join(forbidden).exists(),
            "state-root rejection created {forbidden}"
        );
    }
    assert!(!state_dir.join("receipts").exists());
}

#[test]
fn public_state_root_is_rejected_without_permission_repair_or_mutation() {
    let repo = init_repo();
    let state_home = private_state_home();
    let reason_file = state_home.path().join("reason.txt");
    fs::write(repo.path().join("dirty.txt"), "dirty\n").expect("write dirty file");
    fs::write(&reason_file, "Preserve the dirty checkout.\n").expect("write reason");
    let snapshot = dirty_snapshot(repo.path()).expect("snapshot dirty checkout");
    let challenge_path = write_challenge(state_home.path(), &snapshot);
    fs::set_permissions(state_home.path(), fs::Permissions::from_mode(0o755))
        .expect("make state root public");
    let state_dir = checkout_state_dir(state_home.path(), &snapshot);

    adopt_dirty(
        repo.path(),
        state_home.path(),
        CHALLENGE_TOKEN,
        &reason_file,
    )
    .expect_err("public state root must be rejected");

    assert_eq!(
        fs::metadata(state_home.path())
            .expect("state root metadata")
            .permissions()
            .mode()
            & 0o777,
        0o755,
        "verify-only lookup must not repair unsafe permissions"
    );
    assert!(challenge_path.exists());
    assert!(!state_dir.join("lease.lock").exists());
    fs::set_permissions(state_home.path(), fs::Permissions::from_mode(0o700))
        .expect("restore tempdir permissions");
}

#[test]
fn symlinked_state_root_ancestor_is_rejected_without_transition_mutation() {
    let repo = init_repo();
    let state_home = private_state_home();
    let alias_home = private_state_home();
    let state_alias = alias_home.path().join("state-root");
    let reason_file = state_home.path().join("reason.txt");
    fs::write(repo.path().join("dirty.txt"), "dirty\n").expect("write dirty file");
    fs::write(&reason_file, "Preserve the dirty checkout.\n").expect("write reason");
    let snapshot = dirty_snapshot(repo.path()).expect("snapshot dirty checkout");
    let challenge_path = write_challenge(state_home.path(), &snapshot);
    let state_dir = checkout_state_dir(state_home.path(), &snapshot);
    symlink(state_home.path(), &state_alias).expect("create state-root symlink");

    let error = adopt_dirty(repo.path(), &state_alias, CHALLENGE_TOKEN, &reason_file)
        .expect_err("symlinked state root must be rejected");

    assert_eq!(
        error
            .downcast_ref::<DirtyCheckoutError>()
            .expect("typed state-root error")
            .kind(),
        DirtyCheckoutErrorKind::InvalidInput
    );
    assert!(error.to_string().contains("symlink component"));
    assert!(challenge_path.exists());
    for forbidden in ["lease.lock", "lease.json"] {
        assert!(
            !state_dir.join(forbidden).exists(),
            "state-root rejection created {forbidden}"
        );
    }
    assert!(!state_dir.join("receipts").exists());
}

#[test]
fn dirty_snapshot_rejects_hidden_index_flags() {
    for flag in ["--assume-unchanged", "--skip-worktree"] {
        let repo = init_repo();
        fs::write(repo.path().join("tracked.txt"), "tracked\n").expect("write tracked file");
        git(repo.path(), &["add", "--", "tracked.txt"]);
        git(repo.path(), &["commit", "-qm", "add tracked fixture"]);
        git(repo.path(), &["update-index", flag, "--", "tracked.txt"]);
        fs::write(repo.path().join("dirty.txt"), "dirty\n").expect("write dirty file");

        let error = dirty_snapshot(repo.path()).expect_err("hidden index flag must fail closed");
        assert!(
            error.to_string().contains("index") || error.to_string().contains("Git state"),
            "unexpected error for {flag}: {error}"
        );
    }
}

#[test]
fn dirty_snapshot_rejects_hidden_index_flags_in_recursive_submodules() {
    for flag in ["--assume-unchanged", "--skip-worktree"] {
        let child = init_repo();
        fs::write(child.path().join("tracked.txt"), "tracked\n").expect("write child file");
        git(child.path(), &["add", "--", "tracked.txt"]);
        git(child.path(), &["commit", "-qm", "add child fixture"]);

        let repo = init_repo();
        let child_path = child.path().to_string_lossy().to_string();
        git(
            repo.path(),
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                &child_path,
                "modules/child",
            ],
        );
        git(repo.path(), &["commit", "-qam", "add child submodule"]);
        let child_checkout = repo.path().join("modules/child");
        git(
            &child_checkout,
            &["update-index", flag, "--", "tracked.txt"],
        );
        fs::write(repo.path().join("dirty.txt"), "dirty\n").expect("write dirty anchor");

        let error = dirty_snapshot(repo.path())
            .expect_err("hidden recursive submodule index flag must fail closed");
        assert_eq!(
            error
                .downcast_ref::<DirtyCheckoutError>()
                .expect("typed recursive index-flag error")
                .kind(),
            DirtyCheckoutErrorKind::UnsupportedGitState,
            "unexpected error for {flag}: {error}"
        );
    }
}

#[test]
fn dirty_snapshot_overrides_repository_stat_cache_weakening() {
    let repo = init_repo();
    let tracked = repo.path().join("tracked.txt");
    fs::write(&tracked, "original\n").expect("write tracked fixture");
    git(repo.path(), &["add", "--", "tracked.txt"]);
    git(repo.path(), &["commit", "-qm", "add tracked fixture"]);
    git(repo.path(), &["config", "core.trustctime", "false"]);
    git(repo.path(), &["config", "core.checkStat", "minimal"]);
    fs::write(&tracked, "original\n").expect("refresh tracked fixture metadata");
    std::thread::sleep(std::time::Duration::from_millis(1_100));
    git(repo.path(), &["update-index", "--refresh"]);
    fs::write(repo.path().join("dirty.txt"), "dirty\n").expect("write dirty anchor");
    let before = dirty_snapshot(repo.path()).expect("snapshot before hidden edit");
    let metadata = fs::metadata(&tracked).expect("tracked metadata");

    fs::write(&tracked, "modified\n").expect("same-size tracked edit");
    let path = std::ffi::CString::new(tracked.as_os_str().as_bytes()).expect("NUL-free path");
    let times = [
        libc::timespec {
            tv_sec: metadata.atime(),
            tv_nsec: metadata.atime_nsec(),
        },
        libc::timespec {
            tv_sec: metadata.mtime(),
            tv_nsec: metadata.mtime_nsec(),
        },
    ];
    assert_eq!(
        unsafe { libc::utimensat(libc::AT_FDCWD, path.as_ptr(), times.as_ptr(), 0) },
        0,
        "restore tracked fixture timestamps: {}",
        std::io::Error::last_os_error()
    );

    let after = dirty_snapshot(repo.path()).expect("snapshot after hidden edit");
    assert_ne!(
        after.snapshot_id, before.snapshot_id,
        "same-size restored-mtime edit must change the snapshot"
    );
}

#[test]
fn dirty_snapshot_forces_file_mode_tracking_against_hostile_local_config() {
    let repo = init_repo();
    let tracked = repo.path().join("tracked.txt");
    fs::write(&tracked, "tracked\n").expect("write chmod fixture");
    git(repo.path(), &["add", "--", "tracked.txt"]);
    git(repo.path(), &["commit", "-qm", "add chmod fixture"]);
    git(repo.path(), &["config", "core.fileMode", "false"]);
    fs::write(repo.path().join("dirty.txt"), "dirty\n").expect("write dirty anchor");
    let before = dirty_snapshot(repo.path()).expect("snapshot before chmod-only drift");

    fs::set_permissions(&tracked, fs::Permissions::from_mode(0o755))
        .expect("change only tracked executable mode");
    assert_eq!(
        git(repo.path(), &["diff", "--name-only", "--"]),
        "",
        "hostile local config must hide the chmod from an unprotected Git invocation"
    );

    let after = dirty_snapshot(repo.path()).expect("snapshot after chmod-only drift");
    assert_ne!(
        after.snapshot_id, before.snapshot_id,
        "trusted snapshot must detect chmod-only drift despite local core.fileMode=false"
    );
}

#[test]
fn adopt_dirty_rejects_top_level_worktree_redirect_without_touching_target_state() {
    let source = init_repo();
    fs::write(source.path().join("source.txt"), "source\n").expect("write source fixture");
    git(source.path(), &["add", "--", "source.txt"]);
    git(source.path(), &["commit", "-qm", "add source fixture"]);

    let target = init_repo();
    fs::write(target.path().join("target.txt"), "target\n").expect("write target fixture");
    git(target.path(), &["add", "--", "target.txt"]);
    git(target.path(), &["commit", "-qm", "add target fixture"]);
    fs::write(target.path().join("dirty.txt"), "target dirty\n")
        .expect("write target dirty anchor");

    let state_home = private_state_home();
    let reason_file = state_home.path().join("reason.txt");
    fs::write(&reason_file, "Preserve the dirty checkout.\n").expect("write reason");
    let target_snapshot = dirty_snapshot(target.path()).expect("snapshot redirect target");
    let target_challenge = write_challenge(state_home.path(), &target_snapshot);
    let target_state = checkout_state_dir(state_home.path(), &target_snapshot);
    let target_instance = target.path().join(".git/.agent-runtime-checkout-instance");
    let instance_before = fs::read(&target_instance).expect("read target instance sentinel");
    let challenge_before = fs::read(&target_challenge).expect("read target challenge");
    let target_arg = target.path().to_string_lossy().to_string();
    git(
        source.path(),
        &["config", "core.worktree", target_arg.as_str()],
    );

    let error = adopt_dirty(
        source.path(),
        state_home.path(),
        CHALLENGE_TOKEN,
        &reason_file,
    )
    .expect_err("top-level worktree redirect must fail closed");

    assert_eq!(
        error
            .downcast_ref::<DirtyCheckoutError>()
            .expect("typed top-level redirect error")
            .kind(),
        DirtyCheckoutErrorKind::UnsupportedGitState
    );
    assert_eq!(
        fs::read(&target_instance).expect("read preserved target instance sentinel"),
        instance_before
    );
    assert_eq!(
        fs::read(&target_challenge).expect("read preserved target challenge"),
        challenge_before
    );
    assert!(!target_state.join("lease.json").exists());
    assert!(!target_state.join("receipts").exists());
}

#[test]
fn dirty_snapshot_rejects_submodule_worktree_identity_redirects() {
    let child = init_repo();
    fs::write(child.path().join("tracked.txt"), "tracked\n").expect("write child file");
    git(child.path(), &["add", "--", "tracked.txt"]);
    git(child.path(), &["commit", "-qm", "add child fixture"]);

    let repo = init_repo();
    let child_path = child.path().to_string_lossy().to_string();
    git(
        repo.path(),
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            &child_path,
            "modules/child",
        ],
    );
    git(repo.path(), &["commit", "-qam", "add child submodule"]);
    let redirect = tempfile::TempDir::new().expect("redirected submodule worktree");
    fs::write(redirect.path().join("README.md"), "init").expect("populate redirected initial file");
    fs::write(redirect.path().join("tracked.txt"), "tracked\n")
        .expect("populate redirected worktree");
    let redirect_arg = redirect.path().to_string_lossy().to_string();
    git(
        &repo.path().join("modules/child"),
        &["config", "core.worktree", &redirect_arg],
    );
    fs::write(repo.path().join("dirty.txt"), "dirty\n").expect("write dirty anchor");

    let error =
        dirty_snapshot(repo.path()).expect_err("submodule Git identity redirect must fail closed");
    assert_eq!(
        error
            .downcast_ref::<DirtyCheckoutError>()
            .expect("typed submodule identity error")
            .kind(),
        DirtyCheckoutErrorKind::UnsupportedGitState
    );
}

#[test]
fn complete_walk_rejects_ignored_symlink_escapes_and_clean_hardlinks() {
    let symlink_repo = init_repo();
    fs::write(symlink_repo.path().join(".gitignore"), "ignored-link\n").expect("write ignore file");
    git(symlink_repo.path(), &["add", "--", ".gitignore"]);
    git(
        symlink_repo.path(),
        &["commit", "-qm", "ignore symlink fixture"],
    );
    symlink("/etc/passwd", symlink_repo.path().join("ignored-link"))
        .expect("create ignored symlink escape");
    fs::write(symlink_repo.path().join("dirty.txt"), "dirty\n").expect("write dirty file");
    dirty_snapshot(symlink_repo.path())
        .expect_err("ignored symlink escape must be rejected by complete walk");

    let hardlink_repo = init_repo();
    let tracked = hardlink_repo.path().join("tracked.txt");
    fs::write(&tracked, "tracked\n").expect("write tracked file");
    fs::write(hardlink_repo.path().join(".gitignore"), "clean-hardlink\n")
        .expect("ignore clean hardlink fixture");
    git(
        hardlink_repo.path(),
        &["add", "--", "tracked.txt", ".gitignore"],
    );
    git(
        hardlink_repo.path(),
        &["commit", "-qm", "add hardlink fixture"],
    );
    fs::hard_link(&tracked, hardlink_repo.path().join("clean-hardlink"))
        .expect("create clean hardlink");
    git(
        hardlink_repo.path(),
        &["check-ignore", "-q", "--", "clean-hardlink"],
    );
    fs::write(hardlink_repo.path().join("dirty.txt"), "dirty\n").expect("write dirty file");
    dirty_snapshot(hardlink_repo.path())
        .expect_err("clean multiply-linked file must be rejected by complete walk");
}

#[test]
fn git_shaping_environment_cannot_validate_a_stale_virtual_index() {
    let harness = GitCliHarness::new();
    let repo = init_repo();
    let state_home = private_state_home();
    let tracked = repo.path().join("tracked.txt");
    let reason_file = state_home.path().join("reason.txt");
    fs::write(&tracked, "original\n").expect("write tracked file");
    git(repo.path(), &["add", "--", "tracked.txt"]);
    git(repo.path(), &["commit", "-qm", "add tracked fixture"]);
    fs::write(repo.path().join("dirty.txt"), "dirty\n").expect("write dirty file");
    fs::write(&reason_file, "Preserve the dirty checkout.\n").expect("write reason");
    let snapshot = dirty_snapshot(repo.path()).expect("snapshot dirty checkout");
    let challenge_path = write_challenge(state_home.path(), &snapshot);
    let git_dir = git(repo.path(), &["rev-parse", "--absolute-git-dir"]);
    let stale_index = state_home.path().join("stale-index");
    fs::copy(
        std::path::Path::new(git_dir.trim()).join("index"),
        &stale_index,
    )
    .expect("copy stale index");
    fs::write(&tracked, "staged replacement\n").expect("change tracked file");
    git(repo.path(), &["add", "--", "tracked.txt"]);
    fs::write(&tracked, "original\n").expect("restore worktree content");

    let state_root = state_home.path().to_string_lossy().to_string();
    let reason_arg = reason_file.to_string_lossy().to_string();
    let stale_index_arg = stale_index.to_string_lossy().to_string();
    let options = harness
        .cmd_options(repo.path())
        .with_env("AGENT_RUNTIME_CHECKOUT_LEASE_STATE_HOME", &state_root)
        .with_env("AGENT_RUNTIME_DIRTY_CHECKOUT_ADOPTION", "1")
        .with_env("GIT_INDEX_FILE", &stale_index_arg);
    let output = run_with(
        &harness.git_cli_bin(),
        &[
            "worktree",
            "adopt-dirty",
            "--challenge",
            CHALLENGE_TOKEN,
            "--reason-file",
            &reason_arg,
            "--format=json",
        ],
        &options,
    );

    assert_json_error(
        &output,
        "dirty-checkout-challenge-drift",
        nils_common::cli_contract::exit::DATA,
    );
    assert!(
        challenge_path.exists(),
        "drift rejection preserves challenge"
    );
}

#[test]
fn dirty_unborn_repository_snapshot_and_adoption_are_supported() {
    let repo = tempfile::TempDir::new().expect("unborn repository");
    git(repo.path(), &["init", "-q"]);
    fs::write(repo.path().join("staged.txt"), "staged\n").expect("write staged file");
    git(repo.path(), &["add", "--", "staged.txt"]);
    fs::write(repo.path().join("untracked.txt"), "untracked\n").expect("write untracked file");

    let snapshot = dirty_snapshot(repo.path()).expect("snapshot dirty unborn repository");
    assert!(
        snapshot.head_oid.starts_with("unborn:"),
        "unborn HEAD identity must be explicit and domain-separated: {}",
        snapshot.head_oid
    );
    assert_eq!(snapshot.tracked_entries, 1);
    assert_eq!(snapshot.untracked_entries, 1);

    let state_home = private_state_home();
    let reason_file = state_home.path().join("reason.txt");
    fs::write(&reason_file, "Preserve the dirty unborn checkout.\n").expect("write reason");
    let _challenge_path = write_challenge(state_home.path(), &snapshot);
    let receipt = adopt_dirty(
        repo.path(),
        state_home.path(),
        CHALLENGE_TOKEN,
        &reason_file,
    )
    .expect("adopt dirty unborn repository");
    assert_eq!(receipt.snapshot_id, snapshot.snapshot_id);
}

#[test]
fn dirty_unborn_snapshots_are_independently_sensitive_to_staged_and_untracked_content() {
    let staged_repo = tempfile::TempDir::new().expect("staged unborn repository");
    git(staged_repo.path(), &["init", "-q"]);
    let staged_path = staged_repo.path().join("staged.txt");
    fs::write(&staged_path, "first staged\n").expect("write staged fixture");
    git(staged_repo.path(), &["add", "--", "staged.txt"]);
    let first_staged = dirty_snapshot(staged_repo.path()).expect("first staged unborn snapshot");
    fs::write(&staged_path, "second staged\n").expect("change staged fixture");
    git(staged_repo.path(), &["add", "--", "staged.txt"]);
    let second_staged = dirty_snapshot(staged_repo.path()).expect("second staged unborn snapshot");
    assert_eq!(first_staged.head_oid, second_staged.head_oid);
    assert_ne!(first_staged.snapshot_id, second_staged.snapshot_id);

    let untracked_repo = tempfile::TempDir::new().expect("untracked unborn repository");
    git(untracked_repo.path(), &["init", "-q"]);
    let untracked_path = untracked_repo.path().join("untracked.txt");
    fs::write(&untracked_path, "first untracked\n").expect("write untracked fixture");
    let first_untracked =
        dirty_snapshot(untracked_repo.path()).expect("first untracked unborn snapshot");
    fs::write(&untracked_path, "second untracked\n").expect("change untracked fixture");
    let second_untracked =
        dirty_snapshot(untracked_repo.path()).expect("second untracked unborn snapshot");
    assert_eq!(first_untracked.head_oid, second_untracked.head_oid);
    assert_ne!(first_untracked.snapshot_id, second_untracked.snapshot_id);
}

#[test]
fn broken_symbolic_head_is_not_classified_as_unborn() {
    let repo = tempfile::TempDir::new().expect("broken HEAD repository");
    git(repo.path(), &["init", "-q"]);
    let git_dir = repo.path().join(".git");
    fs::create_dir_all(git_dir.join("refs/heads")).expect("create heads directory");
    fs::write(
        git_dir.join("refs/heads/broken"),
        "1111111111111111111111111111111111111111\n",
    )
    .expect("write dangling branch ref");
    git(repo.path(), &["symbolic-ref", "HEAD", "refs/heads/broken"]);
    fs::write(repo.path().join("dirty.txt"), "dirty\n").expect("write dirty fixture");

    let error = dirty_snapshot(repo.path()).expect_err("dangling symbolic HEAD must fail closed");

    assert_eq!(
        error
            .downcast_ref::<DirtyCheckoutError>()
            .expect("typed broken HEAD error")
            .kind(),
        DirtyCheckoutErrorKind::UnsupportedGitState
    );
}

#[test]
fn repository_local_fsmonitor_helper_is_never_launched_by_snapshot_probes() {
    let repo = init_repo();
    let marker = repo.path().join("fsmonitor-invoked");
    let hook = repo.path().join("fsmonitor-hook.sh");
    fs::write(
        &hook,
        format!(
            "#!/bin/sh\nprintf invoked > '{}'\nprintf 'token\\n'\n",
            marker.display()
        ),
    )
    .expect("write fsmonitor hook");
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o700))
        .expect("make fsmonitor hook executable");
    git(
        repo.path(),
        &[
            "config",
            "core.fsmonitor",
            hook.to_str().expect("UTF-8 hook"),
        ],
    );
    git(repo.path(), &["update-index", "--fsmonitor"]);
    let _ = fs::remove_file(&marker);
    fs::write(repo.path().join("dirty.txt"), "dirty\n").expect("write dirty anchor");

    dirty_snapshot(repo.path()).expect("snapshot with disabled repository fsmonitor");

    assert!(
        !marker.exists(),
        "snapshot launched repository fsmonitor helper"
    );
}

#[test]
fn initialized_and_unavailable_submodules_fail_closed_without_skipping_link_safety() {
    let harness = GitCliHarness::new();
    let child = init_repo();
    fs::write(child.path().join(".gitignore"), "ignored-link\n")
        .expect("write child ignore fixture");
    git(child.path(), &["add", "--", ".gitignore"]);
    git(child.path(), &["commit", "-qm", "add child ignore fixture"]);

    let super_repo = init_repo();
    let child_path = child.path().to_string_lossy().to_string();
    git(
        super_repo.path(),
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            &child_path,
            "modules/child",
        ],
    );
    git(
        super_repo.path(),
        &["commit", "-qam", "add child submodule"],
    );
    fs::write(super_repo.path().join("dirty.txt"), "dirty\n").expect("write dirty anchor");
    dirty_snapshot(super_repo.path()).expect("clean initialized submodule is supported");

    let child_checkout = super_repo.path().join("modules/child");
    let child_dirty_path = child_checkout.join("local-dirty.txt");
    fs::write(&child_dirty_path, "dirty submodule state\n").expect("dirty child checkout");
    let dirty_error =
        dirty_snapshot(super_repo.path()).expect_err("dirty submodule must fail closed");
    let dirty_kind = dirty_error
        .downcast_ref::<DirtyCheckoutError>()
        .map(DirtyCheckoutError::kind);
    let dirty_cli = harness.run(
        super_repo.path(),
        &["worktree", "dirty-snapshot", "--format=json"],
    );
    fs::remove_file(&child_dirty_path).expect("restore clean child checkout");

    symlink("/etc/passwd", child_checkout.join("ignored-link"))
        .expect("create ignored child symlink escape");
    dirty_snapshot(super_repo.path())
        .expect_err("initialized submodule link escape must fail closed");
    fs::remove_file(child_checkout.join("ignored-link")).expect("remove child symlink fixture");
    git(
        super_repo.path(),
        &["submodule", "deinit", "-f", "--", "modules/child"],
    );
    let unavailable_error =
        dirty_snapshot(super_repo.path()).expect_err("unavailable submodule must fail closed");
    let unavailable_kind = unavailable_error
        .downcast_ref::<DirtyCheckoutError>()
        .map(DirtyCheckoutError::kind);
    let unavailable_cli = harness.run(
        super_repo.path(),
        &["worktree", "dirty-snapshot", "--format=json"],
    );
    assert_eq!(
        (
            dirty_kind,
            json_error_identity(&dirty_cli),
            unavailable_kind,
            json_error_identity(&unavailable_cli),
        ),
        (
            Some(DirtyCheckoutErrorKind::UnsupportedGitState),
            (
                nils_common::cli_contract::exit::DATA,
                "dirty-checkout-unsupported-git-state".to_string(),
            ),
            Some(DirtyCheckoutErrorKind::UnsupportedGitState),
            (
                nils_common::cli_contract::exit::DATA,
                "dirty-checkout-unsupported-git-state".to_string(),
            ),
        ),
        "dirty and unavailable submodules must preserve one Rust/CLI domain classification"
    );
}
