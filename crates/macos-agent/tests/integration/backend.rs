use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::json;
use sha2::{Digest, Sha256};
use tempfile::TempDir;

use crate::common;

#[test]
fn install_is_idempotent_and_rollback_selects_only_a_verified_previous_receipt() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("cwd");
    let backend_root = cwd.path().join("backend");
    let old = candidate(cwd.path(), "v3.9.2", '2');
    let new = candidate(cwd.path(), "v3.9.3", '3');
    authorize_rollback(&new, &old);

    let dry_run = run_backend(
        &harness,
        cwd.path(),
        &backend_root,
        &old,
        &["--format", "json", "backend", "install", "--dry-run"],
    );
    assert_eq!(dry_run.code, 0, "{}", dry_run.stderr_text());
    assert_eq!(dry_run.stdout_json()["result"]["dry_run"], true);
    assert!(!backend_root.exists(), "dry-run mutated backend storage");

    let old_install = run_backend(
        &harness,
        cwd.path(),
        &backend_root,
        &old,
        &["--format", "json", "backend", "install"],
    );
    assert_eq!(old_install.code, 0, "{}", old_install.stderr_text());
    assert_eq!(
        old_install.stdout_json()["result"]["current"]["tag"],
        "v3.9.2"
    );

    let new_install = run_backend(
        &harness,
        cwd.path(),
        &backend_root,
        &new,
        &["--format", "json", "backend", "install"],
    );
    assert_eq!(new_install.code, 0, "{}", new_install.stderr_text());
    assert_eq!(
        new_install.stdout_json()["result"]["current"]["tag"],
        "v3.9.3"
    );
    assert_eq!(
        new_install.stdout_json()["result"]["previous"]["tag"],
        "v3.9.2"
    );

    let new_receipt = fs::read(backend_root.join("receipts/current.json")).expect("new receipt");
    let old_receipt = fs::read(backend_root.join("receipts/previous.json")).expect("old receipt");
    fs::write(backend_root.join("receipts/current.json"), &old_receipt)
        .expect("simulate old current");
    fs::write(backend_root.join("receipts/pending.json"), &new_receipt)
        .expect("simulate pending activation");
    let recovered = run_backend(
        &harness,
        cwd.path(),
        &backend_root,
        &new,
        &["--format", "json", "backend", "install"],
    );
    assert_eq!(recovered.code, 0, "{}", recovered.stderr_text());
    assert_eq!(
        recovered.stdout_json()["result"]["current"]["tag"],
        "v3.9.3"
    );
    assert!(!backend_root.join("receipts/pending.json").exists());

    let receipt_before = fs::read(backend_root.join("receipts/current.json")).expect("receipt");
    let repeat = run_backend(
        &harness,
        cwd.path(),
        &backend_root,
        &new,
        &["--format", "json", "backend", "install"],
    );
    assert_eq!(repeat.code, 0, "{}", repeat.stderr_text());
    assert_eq!(
        fs::read(backend_root.join("receipts/current.json")).expect("receipt"),
        receipt_before,
        "idempotent install rewrote the receipt"
    );

    let doctor = run_backend(
        &harness,
        cwd.path(),
        &backend_root,
        &new,
        &["--format", "json", "doctor"],
    );
    assert_eq!(doctor.code, 0, "{}", doctor.stderr_text());
    let doctor = doctor.stdout_json();
    assert_eq!(doctor["result"]["ready"], true);
    assert_eq!(doctor["result"]["permissions"]["status"], "pass");
    assert_eq!(doctor["result"]["bridge"]["status"], "pass");
    assert_eq!(doctor["result"]["runtime"]["status"], "pass");

    let current_before_rollback =
        fs::read(backend_root.join("receipts/current.json")).expect("current receipt");
    let previous_before_rollback =
        fs::read(backend_root.join("receipts/previous.json")).expect("previous receipt");
    let rollback_dry_run = run_backend(
        &harness,
        cwd.path(),
        &backend_root,
        &new,
        &["--format", "json", "backend", "rollback", "--dry-run"],
    );
    assert_eq!(
        rollback_dry_run.code,
        0,
        "{}",
        rollback_dry_run.stderr_text()
    );
    assert_eq!(rollback_dry_run.stdout_json()["result"]["dry_run"], true);
    assert_eq!(
        fs::read(backend_root.join("receipts/current.json")).expect("current receipt"),
        current_before_rollback
    );
    assert_eq!(
        fs::read(backend_root.join("receipts/previous.json")).expect("previous receipt"),
        previous_before_rollback
    );

    let previous_cli = backend_root.join("versions/v3.9.2/cli/peekaboo");
    let previous_cli_body = fs::read(&previous_cli).expect("previous CLI");
    fs::write(&previous_cli, b"tampered").expect("tamper previous CLI");
    let invalid_rollback = run_backend(
        &harness,
        cwd.path(),
        &backend_root,
        &new,
        &["--error-format", "json", "backend", "rollback"],
    );
    assert_eq!(invalid_rollback.code, 69);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(
            &fs::read(backend_root.join("receipts/current.json")).expect("current receipt")
        )
        .expect("receipt JSON")["tag"],
        "v3.9.3"
    );
    fs::write(&previous_cli, previous_cli_body).expect("restore previous CLI");

    let rollback = run_backend(
        &harness,
        cwd.path(),
        &backend_root,
        &new,
        &["--format", "json", "backend", "rollback"],
    );
    assert_eq!(rollback.code, 0, "{}", rollback.stderr_text());
    assert_eq!(rollback.stdout_json()["result"]["current"]["tag"], "v3.9.2");

    let verify = run_backend(
        &harness,
        cwd.path(),
        &backend_root,
        &new,
        &["--format", "json", "backend", "verify"],
    );
    assert_eq!(verify.code, 0, "{}", verify.stderr_text());
    assert_eq!(verify.stdout_json()["result"]["active_tag"], "v3.9.2");
    assert_eq!(verify.stdout_json()["result"]["rollback_active"], true);

    let reinstall = run_backend(
        &harness,
        cwd.path(),
        &backend_root,
        &new,
        &["--format", "json", "backend", "install"],
    );
    assert_eq!(reinstall.code, 0, "{}", reinstall.stderr_text());
    assert_eq!(
        reinstall.stdout_json()["result"]["current"]["tag"],
        "v3.9.3"
    );
}

#[test]
fn backend_transition_retires_owned_daemons_before_later_locks_drop_authority() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("cwd");
    let backend_root = cwd.path().join("backend");
    let release_a = candidate(cwd.path(), "v3.9.2", '2');
    let release_b = candidate(cwd.path(), "v3.9.3", '3');
    let release_c = candidate(cwd.path(), "v3.9.4", '4');
    authorize_rollback(&release_b, &release_a);
    authorize_rollback(&release_c, &release_b);

    let installed_a = run_backend(
        &harness,
        cwd.path(),
        &backend_root,
        &release_a,
        &["--format", "json", "backend", "install"],
    );
    assert_eq!(installed_a.code, 0, "{}", installed_a.stderr_text());

    let release_a_identity =
        &sha256(&release_a.source.join("peekaboo-macos-universal/peekaboo"))[..16];
    let socket_dir = harness
        .home_dir()
        .join("Library/Application Support/Peekaboo");
    fs::create_dir_all(&socket_dir).expect("socket directory");
    let owned_daemon = socket_dir.join(format!("daemon-{release_a_identity}.sock"));
    let owned_auto = socket_dir.join(format!("auto-{release_a_identity}.sock"));
    let unrelated = socket_dir.join("daemon-unrelated.sock");
    for socket in [&owned_daemon, &owned_auto, &unrelated] {
        fs::write(socket, b"fixture").expect("socket fixture");
    }

    for candidate in [&release_b, &release_c] {
        let installed = run_backend(
            &harness,
            cwd.path(),
            &backend_root,
            candidate,
            &["--format", "json", "backend", "install"],
        );
        assert_eq!(installed.code, 0, "{}", installed.stderr_text());
    }

    assert!(!owned_daemon.exists());
    assert!(!owned_auto.exists());
    assert!(unrelated.exists());

    let release_c_identity =
        &sha256(&release_c.source.join("peekaboo-macos-universal/peekaboo"))[..16];
    let rollback_daemon = socket_dir.join(format!("daemon-{release_c_identity}.sock"));
    let rollback_auto = socket_dir.join(format!("auto-{release_c_identity}.sock"));
    for socket in [&rollback_daemon, &rollback_auto] {
        fs::write(socket, b"fixture").expect("rollback socket fixture");
    }
    let rolled_back = run_backend(
        &harness,
        cwd.path(),
        &backend_root,
        &release_c,
        &["--format", "json", "backend", "rollback"],
    );
    assert_eq!(rolled_back.code, 0, "{}", rolled_back.stderr_text());
    assert!(!rollback_daemon.exists());
    assert!(!rollback_auto.exists());
    assert!(unrelated.exists());
}

#[test]
fn failed_transition_retirement_keeps_the_verified_outgoing_state_intact() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("cwd");
    let backend_root = cwd.path().join("backend");
    let old = candidate(cwd.path(), "v3.9.2", '2');
    let new = candidate(cwd.path(), "v3.9.3", '3');
    authorize_rollback(&new, &old);
    let installed = run_backend(
        &harness,
        cwd.path(),
        &backend_root,
        &old,
        &["--format", "json", "backend", "install"],
    );
    assert_eq!(installed.code, 0, "{}", installed.stderr_text());

    let old_identity = &sha256(&old.source.join("peekaboo-macos-universal/peekaboo"))[..16];
    let socket_dir = harness
        .home_dir()
        .join("Library/Application Support/Peekaboo");
    fs::create_dir_all(&socket_dir).expect("socket directory");
    let socket = socket_dir.join(format!("daemon-{old_identity}.sock"));
    fs::write(&socket, b"fixture").expect("socket fixture");
    let current_path = backend_root.join("receipts/current.json");
    let current_before = fs::read(&current_path).expect("current receipt");
    let stable_app = backend_root.join("stable/Peekaboo.app/Contents/MacOS/Peekaboo");
    let stable_before = fs::read(&stable_app).expect("stable app");

    let rejected = run_backend_probe_mode(
        &harness,
        cwd.path(),
        &backend_root,
        &new,
        &["--error-format", "json", "backend", "install"],
        "daemon_stop_failed",
    );
    assert_eq!(rejected.code, 69, "{}", rejected.stderr_text());
    assert!(socket.exists());
    assert_eq!(
        fs::read(&current_path).expect("current receipt"),
        current_before
    );
    assert_eq!(fs::read(&stable_app).expect("stable app"), stable_before);
    assert!(!backend_root.join("receipts/pending.json").exists());
}

#[test]
fn rollback_rejects_malformed_current_receipt_before_transition_side_effects() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("cwd");
    let backend_root = cwd.path().join("backend");
    let old = candidate(cwd.path(), "v3.9.2", '2');
    let new = candidate(cwd.path(), "v3.9.3", '3');
    authorize_rollback(&new, &old);
    for candidate in [&old, &new] {
        let installed = run_backend(
            &harness,
            cwd.path(),
            &backend_root,
            candidate,
            &["--format", "json", "backend", "install"],
        );
        assert_eq!(installed.code, 0, "{}", installed.stderr_text());
    }

    let receipt_path = backend_root.join("receipts/current.json");
    let mut current: serde_json::Value =
        serde_json::from_slice(&fs::read(&receipt_path).expect("current receipt"))
            .expect("receipt JSON");
    current["cli_binary_sha256"] = json!("../short");
    let tampered_receipt = serde_json::to_vec_pretty(&current).expect("tampered receipt");
    fs::write(&receipt_path, &tampered_receipt).expect("write tampered receipt");
    let stable_app = backend_root.join("stable/Peekaboo.app/Contents/MacOS/Peekaboo");
    let stable_before = fs::read(&stable_app).expect("stable app");

    let rejected = run_backend(
        &harness,
        cwd.path(),
        &backend_root,
        &new,
        &["--error-format", "json", "backend", "rollback"],
    );
    assert_eq!(rejected.code, 69, "{}", rejected.stderr_text());
    assert_eq!(
        fs::read(&receipt_path).expect("current receipt"),
        tampered_receipt
    );
    assert_eq!(fs::read(&stable_app).expect("stable app"), stable_before);
    assert!(!backend_root.join("receipts/pending.json").exists());
}

#[test]
fn rollback_rejects_replaced_current_cli_before_transition_side_effects() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("cwd");
    let backend_root = cwd.path().join("backend");
    let old = candidate(cwd.path(), "v3.9.2", '2');
    let new = candidate(cwd.path(), "v3.9.3", '3');
    authorize_rollback(&new, &old);
    for candidate in [&old, &new] {
        let installed = run_backend(
            &harness,
            cwd.path(),
            &backend_root,
            candidate,
            &["--format", "json", "backend", "install"],
        );
        assert_eq!(installed.code, 0, "{}", installed.stderr_text());
    }

    let receipt_path = backend_root.join("receipts/current.json");
    let current_receipt = fs::read(&receipt_path).expect("current receipt");
    let current: serde_json::Value =
        serde_json::from_slice(&current_receipt).expect("receipt JSON");
    let identity = &current["cli_binary_sha256"].as_str().expect("CLI digest")[..16];
    let socket_dir = harness
        .home_dir()
        .join("Library/Application Support/Peekaboo");
    fs::create_dir_all(&socket_dir).expect("socket directory");
    let socket = socket_dir.join(format!("daemon-{identity}.sock"));
    fs::write(&socket, b"fixture").expect("socket fixture");
    let marker = cwd.path().join("untrusted-cli-invoked");
    let current_cli = backend_root.join("versions/v3.9.3/cli/peekaboo");
    write_executable(
        &current_cli,
        &format!(
            "#!/bin/sh\nprintf invoked > '{}'\nprintf '%s\\n' '{{\"success\":true,\"data\":{{\"selected\":{{\"source\":\"remote\",\"handshake\":{{\"hostKind\":\"onDemand\",\"build\":\"3.9.3 (3.9.3)\"}}}}}}}}'\n",
            marker.display()
        ),
    );
    let stable_app = backend_root.join("stable/Peekaboo.app/Contents/MacOS/Peekaboo");
    let stable_before = fs::read(&stable_app).expect("stable app");

    let rejected = run_backend(
        &harness,
        cwd.path(),
        &backend_root,
        &new,
        &["--error-format", "json", "backend", "rollback"],
    );
    assert_eq!(rejected.code, 69, "{}", rejected.stderr_text());
    assert!(!marker.exists(), "unverified current CLI was executed");
    assert!(
        socket.exists(),
        "socket changed before current verification"
    );
    assert_eq!(
        fs::read(&receipt_path).expect("current receipt"),
        current_receipt
    );
    assert_eq!(fs::read(&stable_app).expect("stable app"), stable_before);
    assert!(!backend_root.join("receipts/pending.json").exists());
}

#[test]
fn rollback_dry_run_rejects_the_same_unowned_stable_app_as_execution() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("cwd");
    let backend_root = cwd.path().join("backend");
    let old = candidate(cwd.path(), "v3.9.2", '2');
    let new = candidate(cwd.path(), "v3.9.3", '3');
    authorize_rollback(&new, &old);
    for candidate in [&old, &new] {
        let installed = run_backend(
            &harness,
            cwd.path(),
            &backend_root,
            candidate,
            &["--format", "json", "backend", "install"],
        );
        assert_eq!(installed.code, 0, "{}", installed.stderr_text());
    }

    let current_path = backend_root.join("receipts/current.json");
    let previous_path = backend_root.join("receipts/previous.json");
    let current_before = fs::read(&current_path).expect("current receipt");
    let previous_before = fs::read(&previous_path).expect("previous receipt");
    let stable_app = backend_root.join("stable/Peekaboo.app/Contents/MacOS/Peekaboo");
    fs::write(&stable_app, b"unowned stable app").expect("drift stable app");
    let stable_before = fs::read(&stable_app).expect("drifted stable app");

    for args in [
        &["--error-format", "json", "backend", "rollback", "--dry-run"][..],
        &["--error-format", "json", "backend", "rollback"][..],
    ] {
        let rejected = run_backend(&harness, cwd.path(), &backend_root, &new, args);
        assert_eq!(rejected.code, 69, "{args:?}: {}", rejected.stderr_text());
        assert_eq!(rejected.stderr_json()["error"]["class"], "backend");
        assert_eq!(fs::read(&current_path).expect("current"), current_before);
        assert_eq!(fs::read(&previous_path).expect("previous"), previous_before);
        assert_eq!(fs::read(&stable_app).expect("stable app"), stable_before);
        assert!(!backend_root.join("receipts/pending.json").exists());
    }
}

#[test]
fn rollback_recovers_an_interruption_after_the_stable_app_swap() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("cwd");
    let backend_root = cwd.path().join("backend");
    let old = candidate(cwd.path(), "v3.9.2", '2');
    let new = candidate(cwd.path(), "v3.9.3", '3');
    authorize_rollback(&new, &old);
    for candidate in [&old, &new] {
        let installed = run_backend(
            &harness,
            cwd.path(),
            &backend_root,
            candidate,
            &["--format", "json", "backend", "install"],
        );
        assert_eq!(installed.code, 0, "{}", installed.stderr_text());
    }

    fs::copy(
        backend_root.join("versions/v3.9.2/app/Peekaboo.app/Contents/MacOS/Peekaboo"),
        backend_root.join("stable/Peekaboo.app/Contents/MacOS/Peekaboo"),
    )
    .expect("simulate rollback app swap before receipt commit");

    let recovered = run_backend(
        &harness,
        cwd.path(),
        &backend_root,
        &new,
        &["--format", "json", "backend", "rollback"],
    );
    assert_eq!(recovered.code, 0, "{}", recovered.stderr_text());
    assert_eq!(
        recovered.stdout_json()["result"]["current"]["tag"],
        "v3.9.2"
    );
    assert_eq!(
        recovered.stdout_json()["result"]["previous"]["tag"],
        "v3.9.3"
    );
}

#[test]
fn install_recovers_the_half_swap_after_the_stable_app_was_moved_to_backup() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("cwd");
    let backend_root = cwd.path().join("backend");
    let old = candidate(cwd.path(), "v3.9.2", '2');
    let new = candidate(cwd.path(), "v3.9.3", '3');
    authorize_rollback(&new, &old);
    for candidate in [&old, &new] {
        let installed = run_backend(
            &harness,
            cwd.path(),
            &backend_root,
            candidate,
            &["--format", "json", "backend", "install"],
        );
        assert_eq!(installed.code, 0, "{}", installed.stderr_text());
    }

    let current_new = fs::read(backend_root.join("receipts/current.json")).expect("new receipt");
    let previous_old = fs::read(backend_root.join("receipts/previous.json")).expect("old receipt");
    fs::write(backend_root.join("receipts/current.json"), &previous_old)
        .expect("restore old current receipt");
    fs::write(backend_root.join("receipts/pending.json"), &current_new)
        .expect("stage pending new receipt");

    let stable_parent = backend_root.join("stable");
    let stable = stable_parent.join("Peekaboo.app");
    fs::remove_dir_all(&stable).expect("remove active new app");
    copy_fixture_app(
        &backend_root.join("versions/v3.9.2/app/Peekaboo.app"),
        &stable,
    );
    let incoming = stable_parent.join(".nils-peekaboo-incoming");
    let backup = stable_parent.join(".nils-peekaboo-backup");
    copy_fixture_app(
        &backend_root.join("versions/v3.9.3/app/Peekaboo.app"),
        &incoming,
    );
    fs::rename(&stable, &backup).expect("simulate crash after stable-to-backup rename");

    let recovered = run_backend(
        &harness,
        cwd.path(),
        &backend_root,
        &new,
        &["--format", "json", "backend", "install"],
    );
    assert_eq!(recovered.code, 0, "{}", recovered.stderr_text());
    assert_eq!(
        recovered.stdout_json()["result"]["current"]["tag"],
        "v3.9.3"
    );
    assert!(stable.is_dir());
    assert!(!incoming.exists());
    assert!(!backup.exists());
    assert!(!backend_root.join("receipts/pending.json").exists());
}

#[test]
fn install_recovers_a_partial_first_install_incoming_app() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("cwd");
    let backend_root = cwd.path().join("backend");
    let candidate = candidate(cwd.path(), "v3.9.3", '3');
    let installed = run_backend(
        &harness,
        cwd.path(),
        &backend_root,
        &candidate,
        &["--format", "json", "backend", "install"],
    );
    assert_eq!(installed.code, 0, "{}", installed.stderr_text());

    let current = fs::read(backend_root.join("receipts/current.json")).expect("current receipt");
    fs::remove_dir_all(backend_root.join("stable/Peekaboo.app")).expect("remove stable app");
    fs::remove_file(backend_root.join("receipts/current.json")).expect("remove current receipt");
    fs::write(backend_root.join("receipts/pending.json"), current).expect("pending receipt");
    let partial = backend_root.join("stable/.nils-peekaboo-incoming/Contents/MacOS");
    fs::create_dir_all(&partial).expect("partial incoming tree");
    fs::write(partial.join("Peekaboo"), b"partial").expect("partial incoming executable");

    let recovered = run_backend(
        &harness,
        cwd.path(),
        &backend_root,
        &candidate,
        &["--format", "json", "backend", "install"],
    );
    assert_eq!(recovered.code, 0, "{}", recovered.stderr_text());
    assert!(backend_root.join("stable/Peekaboo.app").is_dir());
    assert!(!backend_root.join("stable/.nils-peekaboo-incoming").exists());
    assert!(!backend_root.join("receipts/pending.json").exists());
}

#[test]
fn doctor_parses_the_pinned_permissions_and_bridge_schemas_fail_closed() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("cwd");
    let backend_root = cwd.path().join("backend");
    let candidate = candidate(cwd.path(), "v3.9.3", '3');
    let installed = run_backend(
        &harness,
        cwd.path(),
        &backend_root,
        &candidate,
        &["--format", "json", "backend", "install"],
    );
    assert_eq!(installed.code, 0, "{}", installed.stderr_text());

    let ready = run_backend_probe_mode(
        &harness,
        cwd.path(),
        &backend_root,
        &candidate,
        &["--format", "json", "doctor"],
        "real_ready",
    );
    assert_eq!(ready.code, 0, "{}", ready.stderr_text());
    assert_eq!(ready.stdout_json()["result"]["ready"], true);

    for mode in [
        "permission_denied",
        "bridge_failed",
        "bridge_missing_build",
        "bridge_stale_build",
        "malformed_probe",
    ] {
        let blocked = run_backend_probe_mode(
            &harness,
            cwd.path(),
            &backend_root,
            &candidate,
            &["--format", "json", "doctor", "--strict"],
            mode,
        );
        assert_eq!(blocked.code, 77, "{mode}: {}", blocked.stderr_text());
        assert_eq!(blocked.stdout_json()["result"]["ready"], false, "{mode}");
    }
    let report_only = run_backend_probe_mode(
        &harness,
        cwd.path(),
        &backend_root,
        &candidate,
        &["--format", "json", "doctor"],
        "permission_denied",
    );
    assert_eq!(report_only.code, 0, "{}", report_only.stderr_text());
    assert_eq!(report_only.stdout_json()["result"]["ready"], false);
}

#[test]
fn lock_without_each_mandatory_capability_probe_is_rejected() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("cwd");
    for missing in ["version", "permissions", "bridge", "tools"] {
        let candidate = candidate(cwd.path(), &format!("v3.9.3-{missing}"), '3');
        let mut lock = read_lock(&candidate.lock);
        lock["required_capability_probes"]
            .as_array_mut()
            .expect("probes")
            .retain(|probe| probe["id"] != missing);
        write_lock(&candidate.lock, &lock);
        let out = run_backend(
            &harness,
            cwd.path(),
            &cwd.path().join(format!("backend-{missing}")),
            &candidate,
            &["--error-format", "json", "backend", "install"],
        );
        assert_eq!(out.code, 69, "missing {missing}: {}", out.stderr_text());
    }
}

#[test]
fn lock_rejects_archive_policy_that_weakens_any_required_guard() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("cwd");
    for field in [
        "reject_absolute_paths",
        "reject_parent_traversal",
        "allow_internal_symlinks",
        "reject_symlink_escape",
    ] {
        let candidate = candidate(cwd.path(), &format!("v3.9.3-{field}"), '3');
        let mut lock = read_lock(&candidate.lock);
        lock["archive_policy"][field] = json!(false);
        write_lock(&candidate.lock, &lock);
        let out = run_backend(
            &harness,
            cwd.path(),
            &cwd.path().join(format!("archive-policy-{field}")),
            &candidate,
            &["--error-format", "json", "backend", "install"],
        );
        assert_eq!(out.code, 69, "{field}: {}", out.stderr_text());
    }
}

#[test]
fn digest_mismatch_and_unowned_stable_app_fail_closed() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("cwd");
    let backend_root = cwd.path().join("backend");
    let candidate = candidate(cwd.path(), "v3.9.3", 'a');
    let mut lock: serde_json::Value =
        serde_json::from_slice(&fs::read(&candidate.lock).expect("lock")).expect("lock json");
    lock["assets"][0]["sha256"] = json!("0".repeat(64));
    let bad_lock = cwd.path().join("bad-lock.json");
    fs::write(&bad_lock, serde_json::to_vec_pretty(&lock).expect("encode")).expect("bad lock");
    let bad = Candidate {
        lock: bad_lock,
        assets: candidate.assets.clone(),
        source: candidate.source.clone(),
        tools: candidate.tools.clone(),
    };
    let mismatch = run_backend(
        &harness,
        cwd.path(),
        &backend_root,
        &bad,
        &[
            "--format",
            "json",
            "--error-format",
            "json",
            "backend",
            "install",
        ],
    );
    assert_eq!(mismatch.code, 69);
    assert_eq!(mismatch.stderr_json()["error"]["class"], "backend");
    assert!(!backend_root.join("receipts/current.json").exists());

    let conflict_root = cwd.path().join("conflict-backend");
    fs::create_dir_all(conflict_root.join("stable/Peekaboo.app")).expect("stable");
    fs::write(
        conflict_root.join("stable/Peekaboo.app/unowned"),
        b"do not replace",
    )
    .expect("unowned");
    let conflict = run_backend(
        &harness,
        cwd.path(),
        &conflict_root,
        &candidate,
        &["--error-format", "json", "backend", "install"],
    );
    assert_eq!(conflict.code, 69);
    assert!(conflict_root.join("stable/Peekaboo.app/unowned").is_file());
}

#[test]
fn mutable_receipt_hashes_cannot_authorize_replaced_code_or_bundle_drift() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("cwd");
    let candidate = candidate(cwd.path(), "v3.9.3", '3');

    let cli_root = cwd.path().join("cli-backend");
    let installed = run_backend(
        &harness,
        cwd.path(),
        &cli_root,
        &candidate,
        &["--format", "json", "backend", "install"],
    );
    assert_eq!(installed.code, 0, "{}", installed.stderr_text());
    let replacement = cli_root.join("versions/v3.9.3/cli/peekaboo");
    fs::write(
        &replacement,
        "#!/bin/sh\n[ \"$1\" = --version ] && echo 'Peekaboo 3.9.3' && exit 0\necho '{\"success\":true}'\n",
    )
    .expect("replace CLI");
    fs::set_permissions(&replacement, fs::Permissions::from_mode(0o755)).expect("chmod");
    let mut receipt: serde_json::Value =
        serde_json::from_slice(&fs::read(cli_root.join("receipts/current.json")).expect("receipt"))
            .expect("receipt JSON");
    receipt["cli_binary_sha256"] = json!(sha256(&replacement));
    fs::write(
        cli_root.join("receipts/current.json"),
        serde_json::to_vec_pretty(&receipt).expect("receipt"),
    )
    .expect("rewrite receipt");
    let rejected = run_backend(
        &harness,
        cwd.path(),
        &cli_root,
        &candidate,
        &["--error-format", "json", "backend", "verify"],
    );
    assert_eq!(rejected.code, 69, "{}", rejected.stderr_text());

    let app_root = cwd.path().join("app-backend");
    let installed = run_backend(
        &harness,
        cwd.path(),
        &app_root,
        &candidate,
        &["--format", "json", "backend", "install"],
    );
    assert_eq!(installed.code, 0, "{}", installed.stderr_text());
    let wrong_metadata = b"<plist><dict><key>CFBundleIdentifier</key><string>boo.peekaboo.mac</string><key>CFBundleShortVersionString</key><string>3.9.3</string><key>CFBundleVersion</key><string>stale-build</string><key>LSMinimumSystemVersion</key><string>15.0</string></dict></plist>";
    for plist in [
        app_root.join("versions/v3.9.3/app/Peekaboo.app/Contents/Info.plist"),
        app_root.join("stable/Peekaboo.app/Contents/Info.plist"),
    ] {
        fs::write(plist, wrong_metadata).expect("tamper signed metadata fixture");
    }
    let rejected = run_backend(
        &harness,
        cwd.path(),
        &app_root,
        &candidate,
        &["--error-format", "json", "backend", "verify"],
    );
    assert_eq!(rejected.code, 69, "{}", rejected.stderr_text());
}

#[test]
fn rollback_rejects_a_forged_receipt_for_an_unreviewed_same_signer_release() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("cwd");
    let backend_root = cwd.path().join("backend");
    let unreviewed = candidate(cwd.path(), "v3.9.1", '1');
    let old = candidate(cwd.path(), "v3.9.2", '2');
    let new = candidate(cwd.path(), "v3.9.3", '3');
    authorize_rollback(&old, &unreviewed);
    authorize_rollback(&new, &old);

    let installed = run_backend(
        &harness,
        cwd.path(),
        &backend_root,
        &unreviewed,
        &["--format", "json", "backend", "install"],
    );
    assert_eq!(installed.code, 0, "{}", installed.stderr_text());
    let forged_receipt =
        fs::read(backend_root.join("receipts/current.json")).expect("unreviewed receipt");
    for release in [&old, &new] {
        let installed = run_backend(
            &harness,
            cwd.path(),
            &backend_root,
            release,
            &["--format", "json", "backend", "install"],
        );
        assert_eq!(installed.code, 0, "{}", installed.stderr_text());
    }

    fs::write(backend_root.join("receipts/previous.json"), forged_receipt)
        .expect("forge previous receipt");
    let rejected = run_backend(
        &harness,
        cwd.path(),
        &backend_root,
        &new,
        &["--error-format", "json", "backend", "rollback"],
    );
    assert_eq!(rejected.code, 69, "{}", rejected.stderr_text());
    assert_eq!(rejected.stderr_json()["error"]["class"], "backend");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(
            &fs::read(backend_root.join("receipts/current.json")).expect("current")
        )
        .expect("current JSON")["tag"],
        "v3.9.3"
    );
}

#[test]
fn malformed_archives_metadata_and_escaping_symlinks_fail_before_activation() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("cwd");

    let truncated = candidate(cwd.path(), "v3.9.3", '3');
    let app_name = asset_name(&truncated.lock, "app");
    fs::write(truncated.assets.join(&app_name), b"truncated zip").expect("truncate app archive");
    refresh_asset_digest(&truncated.lock, &truncated.assets, "app");
    let truncated_out = run_backend(
        &harness,
        cwd.path(),
        &cwd.path().join("truncated-backend"),
        &truncated,
        &["--error-format", "json", "backend", "install"],
    );
    assert_eq!(truncated_out.code, 69);

    let mismatched = candidate(cwd.path(), "v3.9.4", '4');
    let mut lock = read_lock(&mismatched.lock);
    lock["assets"][1]["bundle_id"] = json!("invalid.bundle.identity");
    write_lock(&mismatched.lock, &lock);
    let mismatched_out = run_backend(
        &harness,
        cwd.path(),
        &cwd.path().join("mismatched-backend"),
        &mismatched,
        &["--error-format", "json", "backend", "install"],
    );
    assert_eq!(mismatched_out.code, 69);

    let malicious = candidate(cwd.path(), "v3.9.5", '5');
    symlink(
        "/etc/passwd",
        malicious
            .source
            .join("peekaboo-macos-universal/escaping-link"),
    )
    .expect("escaping symlink");
    let cli_name = asset_name(&malicious.lock, "cli");
    let archive = malicious.assets.join(&cli_name);
    fs::remove_file(&archive).expect("replace CLI archive");
    let tar = Command::new("tar")
        .args(["-czf", archive.to_str().expect("tar"), "-C"])
        .arg(&malicious.source)
        .arg("peekaboo-macos-universal")
        .status()
        .expect("tar command");
    assert!(tar.success());
    refresh_asset_digest(&malicious.lock, &malicious.assets, "cli");
    let malicious_out = run_backend(
        &harness,
        cwd.path(),
        &cwd.path().join("malicious-backend"),
        &malicious,
        &["--error-format", "json", "backend", "install"],
    );
    assert_eq!(malicious_out.code, 69);
    assert!(
        !cwd.path()
            .join("malicious-backend/receipts/current.json")
            .exists()
    );
}

#[test]
fn strict_architecture_signature_notary_and_gatekeeper_checks_are_independently_enforced() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("cwd");
    let candidate = candidate(cwd.path(), "v3.9.3", '3');

    let strict_ok = run_backend(
        &harness,
        cwd.path(),
        &cwd.path().join("strict-ok"),
        &candidate,
        &["--format", "json", "backend", "install", "--strict"],
    );
    assert_eq!(strict_ok.code, 0, "{}", strict_ok.stderr_text());

    for failure in [
        "architecture",
        "signature",
        "notary",
        "gatekeeper",
        "gatekeeper_source",
    ] {
        let out = run_backend_with_failure(
            &harness,
            cwd.path(),
            &cwd.path().join(format!("strict-{failure}")),
            &candidate,
            &["--error-format", "json", "backend", "install", "--strict"],
            failure,
        );
        assert_eq!(out.code, 69, "{failure}: {}", out.stderr_text());
        assert_eq!(out.stderr_json()["error"]["class"], "backend");
    }
}

struct Candidate {
    lock: PathBuf,
    assets: PathBuf,
    source: PathBuf,
    tools: PathBuf,
}

fn candidate(root: &Path, tag: &str, commit_char: char) -> Candidate {
    let token = tag.replace('.', "-");
    let source = root.join(format!("source-{token}"));
    let assets = root.join(format!("assets-{token}"));
    let tools = root.join(format!("tools-{token}"));
    fs::create_dir_all(source.join("peekaboo-macos-universal")).expect("cli source");
    fs::create_dir_all(source.join("Peekaboo.app/Contents/MacOS")).expect("app source");
    fs::create_dir_all(&assets).expect("assets");
    fs::create_dir_all(&tools).expect("tools");
    write_executable(
        &tools.join("lipo"),
        "#!/bin/sh\n[ \"${NILS_MACOS_AGENT_TEST_VERIFY_FAIL:-}\" = architecture ] && exit 1\nprintf '%s\\n' 'arm64 x86_64'\n",
    );
    write_executable(
        &tools.join("codesign"),
        "#!/bin/sh\n[ \"${NILS_MACOS_AGENT_TEST_VERIFY_FAIL:-}\" = signature ] && exit 1\nfor arg in \"$@\"; do candidate=$arg; done\n[ -f \"$candidate\" ] && grep -q MALICIOUS_MARKER \"$candidate\" && exit 1\n[ -d \"$candidate\" ] && grep -q 'tampered sealed resource' \"$candidate/Contents/Info.plist\" 2>/dev/null && exit 1\ncase \" $* \" in\n  *\" --check-notarization \"*) [ \"${NILS_MACOS_AGENT_TEST_VERIFY_FAIL:-}\" = notary ] && exit 1 ;;\nesac\nprintf '%s\\n' 'Authority=Fixture CLI' 'Authority=Fixture App' 'TeamIdentifier=FIXTURE' >&2\n",
    );
    write_executable(
        &tools.join("spctl"),
        "#!/bin/sh\n[ \"${NILS_MACOS_AGENT_TEST_VERIFY_FAIL:-}\" = gatekeeper ] && exit 1\n[ \"${NILS_MACOS_AGENT_TEST_VERIFY_FAIL:-}\" = gatekeeper_source ] && printf '%s\\n' 'source=Developer ID' >&2 && exit 0\nprintf '%s\\n' 'source=Notarized Developer ID' >&2\n",
    );
    let version = tag.trim_start_matches('v');
    let cli = source.join("peekaboo-macos-universal/peekaboo");
    fs::write(
        &cli,
        format!(
            r#"#!/bin/sh
if [ "$1" = "--version" ]; then
  echo 'Peekaboo {version}'
  exit 0
fi
mode="${{NILS_MACOS_AGENT_TEST_PROBE_MODE:-real_ready}}"
case " $* " in
  *" permissions status "*)
    [ "$mode" = malformed_probe ] && echo 'not-json' && exit 0
    if [ "$mode" = permission_denied ]; then
      cat <<'JSON'
{{
  "success": true,
  "data": {{
    "source": "bridge",
    "permissions": [
      {{"name":"Screen Recording","isRequired":true,"isGranted":false,"grantInstructions":"System Settings"}},
      {{"name":"Accessibility","isRequired":true,"isGranted":true,"grantInstructions":"System Settings"}}
    ]
  }}
}}
JSON
    else
      cat <<'JSON'
{{"success":true,"data":{{"source":"bridge","permissions":[{{"name":"Screen Recording","isRequired":true,"isGranted":true,"grantInstructions":"System Settings"}},{{"name":"Accessibility","isRequired":true,"isGranted":true,"grantInstructions":"System Settings"}}]}}}}
JSON
    fi
    ;;
  *" bridge status "*)
    bridge_socket=''
    previous=''
    for argument in "$@"; do
      [ "$previous" = --bridge-socket ] && bridge_socket=$argument
      previous=$argument
    done
    if [ -n "$bridge_socket" ]; then
      cat <<'JSON'
{{"success":true,"data":{{"selected":{{"source":"remote","handshake":{{"hostKind":"onDemand","build":"{version} ({version})"}}}}}}}}
JSON
      exit 0
    fi
    [ "$mode" = malformed_probe ] && echo 'not-json' && exit 0
    if [ "$mode" = bridge_failed ]; then
      cat <<'JSON'
{{"success":true,"data":{{"remoteSkipped":false,"selected":{{"source":"local","socketPath":null,"handshake":null}},"candidates":[{{"socketPath":"/private/bridge.sock","result":{{"failure":{{"kind":"system","message":"connection refused"}}}}}}],"client":{{"processIdentifier":1}}}}}}
JSON
    elif [ "$mode" = bridge_missing_build ]; then
      cat <<'JSON'
{{"success":true,"data":{{"remoteSkipped":false,"selected":{{"source":"remote","socketPath":"/private/bridge.sock","handshake":{{"hostKind":"gui"}}}}}}}}
JSON
    elif [ "$mode" = bridge_stale_build ]; then
      cat <<'JSON'
{{"success":true,"data":{{"remoteSkipped":false,"selected":{{"source":"remote","socketPath":"/private/bridge.sock","handshake":{{"hostKind":"gui","build":"{version} (stale)"}}}}}}}}
JSON
    else
      cat <<'JSON'
{{"success":true,"data":{{"remoteSkipped":false,"selected":{{"source":"remote","socketPath":"/private/bridge.sock","handshake":{{"hostKind":"gui","build":"{version} (fixture-{token})"}}}},"candidates":[{{"socketPath":"/private/bridge.sock","result":{{"success":{{"hostKind":"gui"}}}}}}],"client":{{"processIdentifier":1}}}}}}
JSON
    fi
    ;;
  *" daemon stop "*)
    [ "$mode" = daemon_stop_failed ] && exit 1
    for socket do :; done
    rm -- "$socket"
    ;;
  *" tools "*) echo '{{"success":true,"data":{{"tools":[{{"name":"see"}}]}}}}' ;;
  *) echo '{{"success":true}}' ;;
esac
"#
        ),
    )
    .expect("cli");
    fs::set_permissions(&cli, fs::Permissions::from_mode(0o755)).expect("chmod cli");
    let app_binary = source.join("Peekaboo.app/Contents/MacOS/Peekaboo");
    fs::write(&app_binary, format!("#!/bin/sh\n# {tag}\nexit 0\n")).expect("app binary");
    fs::set_permissions(&app_binary, fs::Permissions::from_mode(0o755)).expect("chmod app");
    fs::write(
        source.join("Peekaboo.app/Contents/Info.plist"),
        format!(
            "<plist><dict><key>CFBundleIdentifier</key><string>boo.peekaboo.mac</string><key>CFBundleShortVersionString</key><string>{version}</string><key>CFBundleVersion</key><string>fixture-{token}</string><key>LSMinimumSystemVersion</key><string>15.0</string></dict></plist>"
        ),
    )
    .expect("plist");
    let cli_name = format!("peekaboo-{token}.tar.gz");
    let app_name = format!("Peekaboo-{token}.app.zip");
    let tar = Command::new("tar")
        .args(["-czf", assets.join(&cli_name).to_str().expect("tar"), "-C"])
        .arg(&source)
        .arg("peekaboo-macos-universal")
        .status()
        .expect("tar command");
    assert!(tar.success());
    let zip = Command::new("zip")
        .current_dir(&source)
        .args([
            "-qr",
            assets.join(&app_name).to_str().expect("zip"),
            "Peekaboo.app",
        ])
        .status()
        .expect("zip command");
    assert!(zip.success());
    let commit = commit_char.to_string().repeat(40);
    let lock = json!({
        "schema_version": 1,
        "repository": "https://github.com/openclaw/Peekaboo",
        "tag": tag,
        "commit": commit,
        "released_at": "2026-07-15T02:05:06Z",
        "license": {"spdx":"MIT","source":format!("https://github.com/openclaw/Peekaboo/blob/{tag}/LICENSE")},
        "minimum_macos": "15.0",
        "assets": [
            {
                "kind":"cli","name":cli_name,
                "url":format!("https://github.com/openclaw/Peekaboo/releases/download/{tag}/fixture-cli"),
                "sha256":sha256(&assets.join(&cli_name)),"archive_root":"peekaboo-macos-universal","executable":"peekaboo",
                "executable_sha256":sha256(&cli),
                "bridge_build":format!("{version} ({version})"),
                "architectures":["arm64","x86_64"],"signing_authority":"Fixture CLI","team_id":"FIXTURE"
            },
            {
                "kind":"app","name":app_name,
                "url":format!("https://github.com/openclaw/Peekaboo/releases/download/{tag}/fixture-app"),
                "sha256":sha256(&assets.join(&app_name)),"archive_root":"Peekaboo.app","executable":"Contents/MacOS/Peekaboo",
                "executable_sha256":sha256(&app_binary),
                "bridge_build":format!("{version} (fixture-{token})"),
                "architectures":["arm64"],"bundle_id":"boo.peekaboo.mac","signing_authority":"Fixture App","team_id":"FIXTURE"
            }
        ],
        "archive_policy":{"reject_absolute_paths":true,"reject_parent_traversal":true,"allow_internal_symlinks":true,"reject_symlink_escape":true},
        "required_capability_probes":[
            {"id":"version","argv":["--version"]},
            {"id":"permissions","argv":["permissions","status","--json"]},
            {"id":"bridge","argv":["bridge","status","--json"]},
            {"id":"tools","argv":["tools","--json"]}
        ],
        "rollback_releases":[]
    });
    let lock_path = root.join(format!("lock-{token}.json"));
    fs::write(
        &lock_path,
        serde_json::to_vec_pretty(&lock).expect("encode lock"),
    )
    .expect("lock");
    Candidate {
        lock: lock_path,
        assets,
        source,
        tools,
    }
}

fn run_backend(
    harness: &common::MacosAgentHarness,
    cwd: &Path,
    backend_root: &Path,
    candidate: &Candidate,
    args: &[&str],
) -> nils_test_support::cmd::CmdOutput {
    let options = harness
        .cmd_options(cwd)
        .with_env(
            "NILS_MACOS_AGENT_BACKEND_ROOT",
            backend_root.to_str().expect("backend root"),
        )
        .with_env(
            "NILS_MACOS_AGENT_TEST_ASSET_DIR",
            candidate.assets.to_str().expect("assets"),
        )
        .with_env(
            "NILS_MACOS_AGENT_LOCK_PATH",
            candidate.lock.to_str().expect("lock"),
        )
        .with_env(
            "NILS_MACOS_AGENT_TEST_TOOL_DIR",
            candidate.tools.to_str().expect("tools"),
        );
    harness.run_with_options(cwd, args, options)
}

fn run_backend_with_failure(
    harness: &common::MacosAgentHarness,
    cwd: &Path,
    backend_root: &Path,
    candidate: &Candidate,
    args: &[&str],
    failure: &str,
) -> nils_test_support::cmd::CmdOutput {
    let options = harness
        .cmd_options(cwd)
        .with_env(
            "NILS_MACOS_AGENT_BACKEND_ROOT",
            backend_root.to_str().expect("backend root"),
        )
        .with_env(
            "NILS_MACOS_AGENT_TEST_ASSET_DIR",
            candidate.assets.to_str().expect("assets"),
        )
        .with_env(
            "NILS_MACOS_AGENT_LOCK_PATH",
            candidate.lock.to_str().expect("lock"),
        )
        .with_env(
            "NILS_MACOS_AGENT_TEST_TOOL_DIR",
            candidate.tools.to_str().expect("tools"),
        )
        .with_env("NILS_MACOS_AGENT_TEST_VERIFY_FAIL", failure);
    harness.run_with_options(cwd, args, options)
}

fn run_backend_probe_mode(
    harness: &common::MacosAgentHarness,
    cwd: &Path,
    backend_root: &Path,
    candidate: &Candidate,
    args: &[&str],
    mode: &str,
) -> nils_test_support::cmd::CmdOutput {
    let options = harness
        .cmd_options(cwd)
        .with_env(
            "NILS_MACOS_AGENT_BACKEND_ROOT",
            backend_root.to_str().expect("backend root"),
        )
        .with_env(
            "NILS_MACOS_AGENT_TEST_ASSET_DIR",
            candidate.assets.to_str().expect("assets"),
        )
        .with_env(
            "NILS_MACOS_AGENT_LOCK_PATH",
            candidate.lock.to_str().expect("lock"),
        )
        .with_env(
            "NILS_MACOS_AGENT_TEST_TOOL_DIR",
            candidate.tools.to_str().expect("tools"),
        )
        .with_env("NILS_MACOS_AGENT_TEST_PROBE_MODE", mode);
    harness.run_with_options(cwd, args, options)
}

fn read_lock(path: &Path) -> serde_json::Value {
    serde_json::from_slice(&fs::read(path).expect("lock")).expect("lock JSON")
}

fn write_lock(path: &Path, value: &serde_json::Value) {
    fs::write(path, serde_json::to_vec_pretty(value).expect("encode lock")).expect("write lock");
}

fn authorize_rollback(current: &Candidate, previous: &Candidate) {
    let mut current_lock = read_lock(&current.lock);
    let previous_lock = read_lock(&previous.lock);
    current_lock["rollback_releases"] = json!([{
        "tag": previous_lock["tag"],
        "commit": previous_lock["commit"],
        "minimum_macos": previous_lock["minimum_macos"],
        "assets": previous_lock["assets"],
    }]);
    write_lock(&current.lock, &current_lock);
}

fn asset_name(lock: &Path, kind: &str) -> String {
    read_lock(lock)["assets"]
        .as_array()
        .expect("assets")
        .iter()
        .find(|asset| asset["kind"] == kind)
        .and_then(|asset| asset["name"].as_str())
        .expect("asset name")
        .to_string()
}

fn refresh_asset_digest(lock_path: &Path, assets: &Path, kind: &str) {
    let mut lock = read_lock(lock_path);
    let asset = lock["assets"]
        .as_array_mut()
        .expect("assets")
        .iter_mut()
        .find(|asset| asset["kind"] == kind)
        .expect("asset");
    let name = asset["name"].as_str().expect("name");
    asset["sha256"] = json!(sha256(&assets.join(name)));
    write_lock(lock_path, &lock);
}

fn write_executable(path: &Path, body: &str) {
    fs::write(path, body).expect("write executable");
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("chmod executable");
}

fn copy_fixture_app(source: &Path, destination: &Path) {
    fs::create_dir_all(destination.join("Contents/MacOS")).expect("app destination");
    fs::copy(
        source.join("Contents/Info.plist"),
        destination.join("Contents/Info.plist"),
    )
    .expect("copy app metadata");
    let executable = destination.join("Contents/MacOS/Peekaboo");
    fs::copy(source.join("Contents/MacOS/Peekaboo"), &executable).expect("copy app executable");
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o755))
        .expect("app executable mode");
}

fn sha256(path: &Path) -> String {
    let body = fs::read(path).expect("hash source");
    let digest = Sha256::digest(body);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}
