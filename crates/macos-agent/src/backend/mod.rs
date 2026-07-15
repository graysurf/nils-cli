use std::fs::{self, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt, symlink};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use nils_common::fs::{SECRET_FILE_MODE, sha256_file, write_atomic};
use serde::{Deserialize, Serialize};

use crate::error::CliError;
use crate::lock::{AssetLock, PeekabooLock, RollbackReleaseLock};
use crate::process;
use crate::test_mode;

const RECEIPT_SCHEMA: &str = "macos-agent.backend-receipt.v1";
const LIFECYCLE_LOCK_FILE: &str = ".backend-lifecycle.lock";

#[derive(Debug, Clone, Copy)]
enum LifecycleLockMode {
    Shared,
    Exclusive,
}

#[derive(Debug)]
struct LifecycleLock(fs::File);

impl LifecycleLock {
    fn acquire(root: &Path, mode: LifecycleLockMode) -> Result<Self, CliError> {
        let path = root.join(LIFECYCLE_LOCK_FILE);
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .mode(SECRET_FILE_MODE)
            .open(&path)
            .map_err(|_| backend_error("failed to open the backend lifecycle lock"))?;
        fs::set_permissions(&path, fs::Permissions::from_mode(SECRET_FILE_MODE))
            .map_err(|_| backend_error("failed to secure the backend lifecycle lock"))?;
        let operation = match mode {
            LifecycleLockMode::Shared => libc::LOCK_SH,
            LifecycleLockMode::Exclusive => libc::LOCK_EX,
        };
        // SAFETY: `flock` observes the valid file descriptor owned by `file`.
        if unsafe { libc::flock(file.as_raw_fd(), operation) } != 0 {
            return Err(backend_error(
                "failed to acquire the backend lifecycle lock",
            ));
        }
        Ok(Self(file))
    }
}

impl Drop for LifecycleLock {
    fn drop(&mut self) {
        // SAFETY: `flock` observes the valid file descriptor owned by this guard.
        let _ = unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

#[derive(Debug)]
pub struct VerifiedBackend {
    path: PathBuf,
    _lease: Option<LifecycleLock>,
}

impl VerifiedBackend {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn digest(&self) -> Result<String, CliError> {
        Ok(format!("sha256:{}", hash_file(&self.path)?))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Receipt {
    pub schema_version: String,
    pub tag: String,
    pub commit: String,
    pub installed_at: String,
    pub cli_archive_sha256: String,
    pub app_archive_sha256: String,
    pub cli_binary_sha256: String,
    pub app_binary_sha256: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PublicReceipt {
    pub tag: String,
    pub commit: String,
    pub installed_at: String,
}

impl From<&Receipt> for PublicReceipt {
    fn from(receipt: &Receipt) -> Self {
        Self {
            tag: receipt.tag.clone(),
            commit: receipt.commit.clone(),
            installed_at: receipt.installed_at.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct BackendStatus {
    pub locked_tag: String,
    pub locked_commit: String,
    pub installed: bool,
    pub verified: bool,
    pub current: Option<PublicReceipt>,
    pub previous: Option<PublicReceipt>,
    pub app_owned: bool,
    pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct VerificationReport {
    pub locked_tag: String,
    pub active_tag: String,
    pub rollback_active: bool,
    pub strict: bool,
    pub ready: bool,
    pub checks: Vec<CheckResult>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DoctorReport {
    pub locked_tag: String,
    pub strict: bool,
    pub ready: bool,
    pub backend: VerificationReport,
    pub runtime: CheckResult,
    pub permissions: CheckResult,
    pub bridge: CheckResult,
    pub capabilities: Vec<CheckResult>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CheckResult {
    pub id: String,
    pub status: &'static str,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct BackendPaths {
    root: PathBuf,
    stable_app: PathBuf,
}

impl BackendPaths {
    pub fn resolve() -> Result<Self, CliError> {
        if let Some(root) = test_mode::backend_root_override() {
            return Ok(Self {
                stable_app: root.join("stable").join("Peekaboo.app"),
                root,
            });
        }
        let home = std::env::var_os("HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .ok_or_else(|| backend_error("HOME is required to resolve backend storage"))?;
        Ok(Self {
            root: home
                .join("Library")
                .join("Application Support")
                .join("nils-cli")
                .join("macos-agent"),
            stable_app: home.join("Applications").join("Nils CLI Peekaboo.app"),
        })
    }

    fn receipts(&self) -> PathBuf {
        self.root.join("receipts")
    }

    fn current_receipt(&self) -> PathBuf {
        self.receipts().join("current.json")
    }

    fn previous_receipt(&self) -> PathBuf {
        self.receipts().join("previous.json")
    }

    fn pending_receipt(&self) -> PathBuf {
        self.receipts().join("pending.json")
    }

    fn version_root(&self, tag: &str) -> PathBuf {
        self.root.join("versions").join(tag)
    }

    pub fn cli_for(&self, tag: &str) -> PathBuf {
        self.version_root(tag).join("cli").join("peekaboo")
    }

    pub fn app_for(&self, tag: &str) -> PathBuf {
        self.version_root(tag).join("app").join("Peekaboo.app")
    }

    pub fn current_cli(&self) -> Result<PathBuf, CliError> {
        let receipt = read_receipt(&self.current_receipt())?
            .ok_or_else(|| backend_error("the locked Peekaboo backend is not installed"))?;
        Ok(self.cli_for(&receipt.tag))
    }

    pub fn stable_app(&self) -> &Path {
        &self.stable_app
    }
}

pub fn status(dry_run: bool) -> Result<BackendStatus, CliError> {
    let lock = PeekabooLock::embedded()?;
    let paths = BackendPaths::resolve()?;
    let _guard = paths
        .root
        .is_dir()
        .then(|| LifecycleLock::acquire(&paths.root, LifecycleLockMode::Shared))
        .transpose()?;
    status_unlocked(&lock, &paths, dry_run)
}

fn status_unlocked(
    lock: &PeekabooLock,
    paths: &BackendPaths,
    dry_run: bool,
) -> Result<BackendStatus, CliError> {
    let current = read_receipt(&paths.current_receipt())?;
    let previous = read_receipt(&paths.previous_receipt())?;
    let verified = current.as_ref().is_some_and(|receipt| {
        if receipt.tag == lock.tag && receipt.commit == lock.commit {
            verify_receipt(paths, lock, receipt, false).is_ok()
        } else {
            verify_receipt_any_version(paths, lock, receipt, false).is_ok()
        }
    });
    let app_owned = current
        .as_ref()
        .is_some_and(|receipt| stable_app_matches(paths, receipt).unwrap_or(false));
    Ok(BackendStatus {
        locked_tag: lock.tag.clone(),
        locked_commit: lock.commit.clone(),
        installed: current.is_some(),
        verified,
        current: current.as_ref().map(PublicReceipt::from),
        previous: previous.as_ref().map(PublicReceipt::from),
        app_owned,
        dry_run,
    })
}

pub fn install(dry_run: bool, strict: bool) -> Result<BackendStatus, CliError> {
    ensure_supported_platform()?;
    let lock = PeekabooLock::embedded()?;
    let paths = BackendPaths::resolve()?;
    let _guard = if dry_run {
        paths
            .root
            .is_dir()
            .then(|| LifecycleLock::acquire(&paths.root, LifecycleLockMode::Shared))
            .transpose()?
    } else {
        create_private_dir(&paths.root)?;
        Some(LifecycleLock::acquire(
            &paths.root,
            LifecycleLockMode::Exclusive,
        )?)
    };
    if paths.pending_receipt().exists() {
        if dry_run {
            return Err(backend_error(
                "an interrupted backend activation requires a non-dry-run recovery",
            ));
        }
        recover_pending(&paths, &lock, strict)?;
    }
    if let Some(current) = read_receipt(&paths.current_receipt())?
        && current.tag == lock.tag
        && current.commit == lock.commit
        && verify_receipt(&paths, &lock, &current, strict).is_ok()
        && stable_app_matches(&paths, &current).unwrap_or(false)
    {
        return status_unlocked(&lock, &paths, dry_run);
    }
    refuse_unowned_app(&paths)?;
    if dry_run {
        return Ok(BackendStatus {
            locked_tag: lock.tag.clone(),
            locked_commit: lock.commit.clone(),
            installed: false,
            verified: false,
            current: None,
            previous: read_receipt(&paths.previous_receipt())?
                .as_ref()
                .map(PublicReceipt::from),
            app_owned: false,
            dry_run: true,
        });
    }

    let staging = paths.root.join(format!(".staging-{}", std::process::id()));
    if staging.exists() {
        fs::remove_dir_all(&staging)
            .map_err(|_| backend_error("failed to clear an interrupted staging directory"))?;
    }
    create_private_dir(&staging)?;
    let result = install_into_staging(&paths, &lock, &staging, strict);
    let _ = fs::remove_dir_all(&staging);
    result?;
    status_unlocked(&lock, &paths, false)
}

fn install_into_staging(
    paths: &BackendPaths,
    lock: &PeekabooLock,
    staging: &Path,
    strict: bool,
) -> Result<(), CliError> {
    let downloads = staging.join("downloads");
    let extracted = staging.join("extracted");
    create_private_dir(&downloads)?;
    create_private_dir(&extracted)?;

    let cli_archive = download_asset(lock.cli_asset(), &downloads)?;
    let app_archive = download_asset(lock.app_asset(), &downloads)?;
    verify_archive_digest(&cli_archive, lock.cli_asset())?;
    verify_archive_digest(&app_archive, lock.app_asset())?;
    validate_archive_listing(&cli_archive, ArchiveKind::TarGz)?;
    validate_archive_listing(&app_archive, ArchiveKind::Zip)?;

    let cli_extract = extracted.join("cli");
    let app_extract = extracted.join("app");
    create_private_dir(&cli_extract)?;
    create_private_dir(&app_extract)?;
    extract_archive(&cli_archive, &cli_extract, ArchiveKind::TarGz)?;
    extract_archive(&app_archive, &app_extract, ArchiveKind::Zip)?;
    validate_symlink_tree(&cli_extract)?;
    validate_symlink_tree(&app_extract)?;

    let cli_source = cli_extract
        .join(&lock.cli_asset().archive_root)
        .join(&lock.cli_asset().executable);
    let app_source = app_extract.join(&lock.app_asset().archive_root);
    let app_binary = app_source.join(&lock.app_asset().executable);
    if !cli_source.is_file() || !app_binary.is_file() {
        return Err(backend_error(
            "verified archives do not contain the locked executables",
        ));
    }
    verify_version(&cli_source, &lock.tag)?;
    verify_architectures(&cli_source, &lock.cli_asset().architectures)?;
    verify_architectures(&app_binary, &lock.app_asset().architectures)?;
    verify_app_metadata(
        &app_source,
        lock.app_asset(),
        &lock.tag,
        &lock.minimum_macos,
    )?;
    verify_signature(&cli_source, lock.cli_asset(), false, strict)?;
    verify_signature(&app_source, lock.app_asset(), true, strict)?;

    let cli_binary_sha256 = hash_file(&cli_source)?;
    let app_binary_sha256 = hash_file(&app_binary)?;
    if cli_binary_sha256 != lock.cli_asset().executable_sha256
        || app_binary_sha256 != lock.app_asset().executable_sha256
    {
        return Err(backend_error(
            "verified archives do not match the locked executable digests",
        ));
    }
    let version_stage = staging.join("version-ready");
    create_private_dir(&version_stage)?;
    let cli_target = version_stage.join("cli").join("peekaboo");
    let app_target = version_stage.join("app").join("Peekaboo.app");
    copy_file(&cli_source, &cli_target, 0o755)?;
    copy_tree(&app_source, &app_target)?;

    let version_root = paths.version_root(&lock.tag);
    if version_root.exists() {
        let existing = Receipt {
            schema_version: RECEIPT_SCHEMA.into(),
            tag: lock.tag.clone(),
            commit: lock.commit.clone(),
            installed_at: test_mode::timestamp(),
            cli_archive_sha256: lock.cli_asset().sha256.clone(),
            app_archive_sha256: lock.app_asset().sha256.clone(),
            cli_binary_sha256: cli_binary_sha256.clone(),
            app_binary_sha256: app_binary_sha256.clone(),
        };
        verify_receipt(paths, lock, &existing, strict)?;
    } else {
        fs::create_dir_all(version_root.parent().expect("version parent"))
            .map_err(|_| backend_error("failed to create versioned backend storage"))?;
        fs::rename(&version_stage, &version_root)
            .map_err(|_| backend_error("failed to atomically activate versioned backend files"))?;
    }

    let new_receipt = Receipt {
        schema_version: RECEIPT_SCHEMA.into(),
        tag: lock.tag.clone(),
        commit: lock.commit.clone(),
        installed_at: test_mode::timestamp(),
        cli_archive_sha256: lock.cli_asset().sha256.clone(),
        app_archive_sha256: lock.app_asset().sha256.clone(),
        cli_binary_sha256,
        app_binary_sha256,
    };
    let old_current = read_receipt(&paths.current_receipt())?;
    write_receipt(&paths.pending_receipt(), &new_receipt)?;
    replace_stable_app(paths, &new_receipt)?;
    if let Some(old) = old_current.as_ref()
        && old.tag != new_receipt.tag
    {
        write_receipt(&paths.previous_receipt(), old)?;
    }
    write_receipt(&paths.current_receipt(), &new_receipt)?;
    fs::remove_file(paths.pending_receipt())
        .map_err(|_| backend_error("failed to finalize backend activation receipt"))?;
    Ok(())
}

fn recover_pending(
    paths: &BackendPaths,
    lock: &PeekabooLock,
    strict: bool,
) -> Result<(), CliError> {
    let pending = read_receipt(&paths.pending_receipt())?
        .ok_or_else(|| backend_error("pending backend activation receipt is unavailable"))?;
    if pending.tag == lock.tag && pending.commit == lock.commit {
        verify_receipt(paths, lock, &pending, strict)?;
    } else {
        verify_receipt_any_version(paths, lock, &pending, strict)?;
    }
    let current = read_receipt(&paths.current_receipt())?;
    if stable_app_matches(paths, &pending)? {
        if let Some(current) = current.as_ref()
            && current.tag != pending.tag
        {
            write_receipt(&paths.previous_receipt(), current)?;
        }
        write_receipt(&paths.current_receipt(), &pending)?;
    } else if !current
        .as_ref()
        .is_some_and(|receipt| stable_app_matches(paths, receipt).unwrap_or(false))
    {
        return Err(backend_error(
            "interrupted backend activation cannot prove stable app ownership",
        ));
    }
    fs::remove_file(paths.pending_receipt())
        .map_err(|_| backend_error("failed to clear recovered backend activation receipt"))
}

pub fn verify(strict: bool) -> Result<VerificationReport, CliError> {
    ensure_supported_platform()?;
    let lock = PeekabooLock::embedded()?;
    let paths = BackendPaths::resolve()?;
    let _guard = LifecycleLock::acquire(&paths.root, LifecycleLockMode::Shared)?;
    verify_unlocked(strict, &lock, &paths)
}

fn verify_unlocked(
    strict: bool,
    lock: &PeekabooLock,
    paths: &BackendPaths,
) -> Result<VerificationReport, CliError> {
    let receipt = read_receipt(&paths.current_receipt())?
        .ok_or_else(|| backend_error("the locked Peekaboo backend is not installed"))?;
    let rollback_active = receipt.tag != lock.tag || receipt.commit != lock.commit;
    if rollback_active {
        verify_receipt_any_version(paths, lock, &receipt, strict)?;
    } else {
        verify_receipt(paths, lock, &receipt, strict)?;
    }
    if !stable_app_matches(paths, &receipt)? {
        return Err(backend_error("the stable app identity has drifted"));
    }
    let (active_app_asset, minimum_macos) = app_contract_for_receipt(lock, &receipt)?;
    verify_signature(&paths.stable_app, active_app_asset, true, strict)?;
    verify_app_metadata(
        &paths.stable_app,
        active_app_asset,
        &receipt.tag,
        minimum_macos,
    )?;

    let mut checks = vec![
        passed("lock", "embedded lock is valid and immutable"),
        passed(
            "receipt",
            if rollback_active {
                "verified previous receipt is active for rollback"
            } else {
                "current receipt matches the locked release"
            },
        ),
        passed(
            "digests",
            "active CLI and app executable digests match the receipt",
        ),
        passed("version", "active CLI reports the locked version"),
        passed("stable_app", "stable app is owned by the current receipt"),
    ];
    if strict {
        checks.extend([
            passed(
                "architectures",
                "locked executable architectures are present",
            ),
            passed(
                "codesign",
                "CLI and app signatures match exact locked identities",
            ),
            passed("notary", "CLI passes Apple's notarization requirement"),
            passed(
                "gatekeeper",
                "app passes Gatekeeper as a notarized Developer ID release",
            ),
        ]);
    }
    Ok(VerificationReport {
        locked_tag: lock.tag.clone(),
        active_tag: receipt.tag,
        rollback_active,
        strict,
        ready: true,
        checks,
    })
}

pub fn doctor(strict: bool) -> Result<DoctorReport, CliError> {
    ensure_supported_platform()?;
    let lock = PeekabooLock::embedded()?;
    let paths = BackendPaths::resolve()?;
    let _guard = LifecycleLock::acquire(&paths.root, LifecycleLockMode::Shared)?;
    let backend = verify_unlocked(strict, &lock, &paths)?;
    let binary = paths.current_cli()?;
    let mut permissions = None;
    let mut bridge = None;
    let mut capabilities = Vec::new();
    for probe in &lock.required_capability_probes {
        let arguments = probe.argv.iter().map(String::as_str).collect::<Vec<_>>();
        let output = run_tool(&binary, &arguments);
        let (status, message) = evaluate_capability_probe(&probe.id, output, &lock.tag);
        let check = CheckResult {
            id: probe.id.clone(),
            status,
            message: message.into(),
        };
        if probe.id == "permissions" {
            permissions = Some(check.clone());
        } else if probe.id == "bridge" {
            bridge = Some(check.clone());
        } else {
            capabilities.push(check);
        }
    }
    let permissions = permissions
        .ok_or_else(|| backend_error("mandatory permissions probe result is unavailable"))?;
    let bridge =
        bridge.ok_or_else(|| backend_error("mandatory Bridge probe result is unavailable"))?;
    let runtime = if paths.stable_app().is_dir() {
        passed("runtime", "stable Peekaboo app path is ready")
    } else {
        CheckResult {
            id: "runtime".into(),
            status: "fail",
            message: "stable Peekaboo app path is unavailable".into(),
        }
    };
    let ready = backend.ready
        && runtime.status == "pass"
        && permissions.status == "pass"
        && bridge.status == "pass"
        && capabilities.iter().all(|check| check.status == "pass");
    Ok(DoctorReport {
        locked_tag: lock.tag,
        strict,
        ready,
        backend,
        runtime,
        permissions,
        bridge,
        capabilities,
    })
}

pub fn acquire_verified_backend() -> Result<VerifiedBackend, CliError> {
    if let Some(path) = test_mode::peekaboo_bin_override() {
        return Ok(VerifiedBackend { path, _lease: None });
    }
    ensure_supported_platform()?;
    let lock = PeekabooLock::embedded()?;
    let paths = BackendPaths::resolve()?;
    let lease = LifecycleLock::acquire(&paths.root, LifecycleLockMode::Shared)?;
    verify_unlocked(false, &lock, &paths)?;
    let path = paths.current_cli()?;
    Ok(VerifiedBackend {
        path,
        _lease: Some(lease),
    })
}

fn evaluate_capability_probe(
    id: &str,
    output: Result<process::ProcessOutput, CliError>,
    locked_tag: &str,
) -> (&'static str, &'static str) {
    let Ok(output) = output else {
        return ("fail", "required capability probe failed");
    };
    if output.exit_code != 0 || output.timed_out || output.stdout_truncated {
        return ("fail", "required capability probe failed");
    }
    if id == "version" {
        let text = String::from_utf8_lossy(&output.stdout);
        return if text.contains(locked_tag.trim_start_matches('v')) {
            ("pass", "locked version capability probe passed")
        } else {
            (
                "fail",
                "version capability probe returned an unexpected release",
            )
        };
    }
    if matches!(id, "observation" | "action" | "scenario" | "mcp_stdio") {
        return ("pass", "required adapter surface probe passed");
    }
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&output.stdout) else {
        return ("fail", "capability probe returned malformed JSON");
    };
    if value.get("success").and_then(serde_json::Value::as_bool) != Some(true) {
        return ("fail", "capability probe returned an unsuccessful envelope");
    }
    match id {
        "permissions" => {
            let Some(rows) = value
                .pointer("/data/permissions")
                .and_then(serde_json::Value::as_array)
            else {
                return ("fail", "permission capability schema is incompatible");
            };
            let required = ["Screen Recording", "Accessibility"];
            if required.iter().all(|name| {
                rows.iter().any(|row| {
                    row.get("name").and_then(serde_json::Value::as_str) == Some(name)
                        && row.get("isRequired").and_then(serde_json::Value::as_bool) == Some(true)
                        && row.get("isGranted").and_then(serde_json::Value::as_bool) == Some(true)
                })
            }) {
                ("pass", "required macOS permissions are granted")
            } else {
                ("blocked", "required macOS permissions are not ready")
            }
        }
        "bridge" => {
            let source = value
                .pointer("/data/selected/source")
                .and_then(serde_json::Value::as_str);
            let host_kind = value
                .pointer("/data/selected/handshake/hostKind")
                .and_then(serde_json::Value::as_str);
            if source == Some("remote") && host_kind == Some("gui") {
                ("pass", "Peekaboo GUI Bridge is selected and ready")
            } else if source.is_some() {
                ("blocked", "Peekaboo GUI Bridge is unavailable")
            } else {
                ("fail", "Bridge capability schema is incompatible")
            }
        }
        "tools" => {
            if value
                .pointer("/data/tools")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|tools| !tools.is_empty())
            {
                ("pass", "tool capability probe passed")
            } else {
                ("fail", "tool capability schema is incompatible")
            }
        }
        _ => ("fail", "unrecognized mandatory capability probe"),
    }
}

pub fn rollback(dry_run: bool, strict: bool) -> Result<BackendStatus, CliError> {
    ensure_supported_platform()?;
    let lock = PeekabooLock::embedded()?;
    let paths = BackendPaths::resolve()?;
    let _guard = LifecycleLock::acquire(
        &paths.root,
        if dry_run {
            LifecycleLockMode::Shared
        } else {
            LifecycleLockMode::Exclusive
        },
    )?;
    let current = read_receipt(&paths.current_receipt())?
        .ok_or_else(|| backend_error("no current backend receipt exists"))?;
    let previous = read_receipt(&paths.previous_receipt())?
        .ok_or_else(|| backend_error("no previous backend receipt exists"))?;
    verify_receipt_any_version(&paths, &lock, &previous, strict)?;
    if dry_run {
        return Ok(BackendStatus {
            locked_tag: lock.tag,
            locked_commit: lock.commit,
            installed: true,
            verified: true,
            current: Some(PublicReceipt::from(&previous)),
            previous: Some(PublicReceipt::from(&current)),
            app_owned: true,
            dry_run: true,
        });
    }
    write_receipt(&paths.pending_receipt(), &previous)?;
    if !stable_app_matches(&paths, &previous)? {
        replace_stable_app(&paths, &previous)?;
    }
    recover_pending(&paths, &lock, strict)?;
    status_unlocked(&lock, &paths, false)
}

fn verify_receipt(
    paths: &BackendPaths,
    lock: &PeekabooLock,
    receipt: &Receipt,
    strict: bool,
) -> Result<(), CliError> {
    if receipt.schema_version != RECEIPT_SCHEMA
        || receipt.tag != lock.tag
        || receipt.commit != lock.commit
        || receipt.cli_archive_sha256 != lock.cli_asset().sha256
        || receipt.app_archive_sha256 != lock.app_asset().sha256
        || receipt.cli_binary_sha256 != lock.cli_asset().executable_sha256
        || receipt.app_binary_sha256 != lock.app_asset().executable_sha256
    {
        return Err(backend_error(
            "current receipt does not match the embedded lock",
        ));
    }
    verify_receipt_files(
        paths,
        receipt,
        &lock.cli_asset().executable_sha256,
        &lock.app_asset().executable_sha256,
    )?;
    verify_version(&paths.cli_for(&receipt.tag), &receipt.tag)?;
    if strict {
        verify_architectures(
            &paths.cli_for(&receipt.tag),
            &lock.cli_asset().architectures,
        )?;
        verify_architectures(
            &paths
                .app_for(&receipt.tag)
                .join(&lock.app_asset().executable),
            &lock.app_asset().architectures,
        )?;
    }
    verify_signature(
        &paths.cli_for(&receipt.tag),
        lock.cli_asset(),
        false,
        strict,
    )?;
    verify_signature(&paths.app_for(&receipt.tag), lock.app_asset(), true, strict)?;
    verify_app_metadata(
        &paths.app_for(&receipt.tag),
        lock.app_asset(),
        &receipt.tag,
        &lock.minimum_macos,
    )?;
    Ok(())
}

fn verify_receipt_any_version(
    paths: &BackendPaths,
    lock: &PeekabooLock,
    receipt: &Receipt,
    strict: bool,
) -> Result<(), CliError> {
    if receipt.schema_version != RECEIPT_SCHEMA || !safe_tag(&receipt.tag) {
        return Err(backend_error("previous receipt is malformed"));
    }
    let release = lock
        .rollback_release(&receipt.tag, &receipt.commit)
        .ok_or_else(|| backend_error("previous release is not authorized by the embedded lock"))?;
    verify_rollback_receipt_identity(receipt, release)?;
    verify_receipt_files(
        paths,
        receipt,
        &release.cli_asset().executable_sha256,
        &release.app_asset().executable_sha256,
    )?;
    verify_version(&paths.cli_for(&receipt.tag), &receipt.tag)?;
    if strict {
        verify_architectures(
            &paths.cli_for(&receipt.tag),
            &release.cli_asset().architectures,
        )?;
        verify_architectures(
            &paths
                .app_for(&receipt.tag)
                .join(&release.app_asset().executable),
            &release.app_asset().architectures,
        )?;
    }
    verify_signature(
        &paths.cli_for(&receipt.tag),
        release.cli_asset(),
        false,
        strict,
    )?;
    verify_signature(
        &paths.app_for(&receipt.tag),
        release.app_asset(),
        true,
        strict,
    )?;
    verify_app_metadata(
        &paths.app_for(&receipt.tag),
        release.app_asset(),
        &receipt.tag,
        &release.minimum_macos,
    )?;
    Ok(())
}

fn verify_rollback_receipt_identity(
    receipt: &Receipt,
    release: &RollbackReleaseLock,
) -> Result<(), CliError> {
    if receipt.cli_archive_sha256 != release.cli_asset().sha256
        || receipt.app_archive_sha256 != release.app_asset().sha256
        || receipt.cli_binary_sha256 != release.cli_asset().executable_sha256
        || receipt.app_binary_sha256 != release.app_asset().executable_sha256
    {
        return Err(backend_error(
            "previous receipt does not match its reviewed rollback lock",
        ));
    }
    Ok(())
}

fn verify_receipt_files(
    paths: &BackendPaths,
    receipt: &Receipt,
    expected_cli_sha256: &str,
    expected_app_sha256: &str,
) -> Result<(), CliError> {
    let cli = paths.cli_for(&receipt.tag);
    let app_binary = paths
        .app_for(&receipt.tag)
        .join("Contents")
        .join("MacOS")
        .join("Peekaboo");
    if hash_file(&cli)? != expected_cli_sha256 || hash_file(&app_binary)? != expected_app_sha256 {
        return Err(backend_error(
            "active backend file digest does not match its receipt",
        ));
    }
    Ok(())
}

fn refuse_unowned_app(paths: &BackendPaths) -> Result<(), CliError> {
    if !paths.stable_app.exists() {
        return Ok(());
    }
    let Some(receipt) = read_receipt(&paths.current_receipt())? else {
        return Err(backend_error("refusing to replace an unowned Peekaboo app"));
    };
    let lock = PeekabooLock::embedded()?;
    if receipt.tag == lock.tag && receipt.commit == lock.commit {
        verify_receipt(paths, &lock, &receipt, false)?;
    } else {
        verify_receipt_any_version(paths, &lock, &receipt, false)?;
    }
    let (active_app_asset, minimum_macos) = app_contract_for_receipt(&lock, &receipt)?;
    if !stable_app_matches(paths, &receipt)? {
        return Err(backend_error(
            "refusing to replace a changed or unowned Peekaboo app",
        ));
    }
    verify_signature(&paths.stable_app, active_app_asset, true, false)?;
    verify_app_metadata(
        &paths.stable_app,
        active_app_asset,
        &receipt.tag,
        minimum_macos,
    )?;
    Ok(())
}

fn stable_app_matches(paths: &BackendPaths, receipt: &Receipt) -> Result<bool, CliError> {
    let binary = paths
        .stable_app
        .join("Contents")
        .join("MacOS")
        .join("Peekaboo");
    if !binary.is_file() {
        return Ok(false);
    }
    Ok(hash_file(&binary)? == receipt.app_binary_sha256)
}

fn replace_stable_app(paths: &BackendPaths, receipt: &Receipt) -> Result<(), CliError> {
    refuse_unowned_app(paths)?;
    let source = paths.app_for(&receipt.tag);
    let parent = paths
        .stable_app
        .parent()
        .ok_or_else(|| backend_error("stable app path has no parent"))?;
    fs::create_dir_all(parent)
        .map_err(|_| backend_error("failed to create the stable app parent directory"))?;
    let incoming = parent.join(format!(".nils-peekaboo-incoming-{}", std::process::id()));
    let backup = parent.join(format!(".nils-peekaboo-backup-{}", std::process::id()));
    let _ = fs::remove_dir_all(&incoming);
    let _ = fs::remove_dir_all(&backup);
    copy_tree(&source, &incoming)?;
    let incoming_hash = hash_file(&incoming.join("Contents/MacOS/Peekaboo"))?;
    if incoming_hash != receipt.app_binary_sha256 {
        let _ = fs::remove_dir_all(&incoming);
        return Err(backend_error("stable app staging digest mismatch"));
    }
    let lock = PeekabooLock::embedded()?;
    let (app_asset, minimum_macos) = app_contract_for_receipt(&lock, receipt)?;
    verify_signature(&incoming, app_asset, true, false)?;
    verify_app_metadata(&incoming, app_asset, &receipt.tag, minimum_macos)?;
    let had_current = paths.stable_app.exists();
    if had_current {
        fs::rename(&paths.stable_app, &backup)
            .map_err(|_| backend_error("failed to stage the previous stable app"))?;
    }
    if let Err(error) = fs::rename(&incoming, &paths.stable_app) {
        if had_current {
            let _ = fs::rename(&backup, &paths.stable_app);
        }
        return Err(backend_error(format!(
            "failed to activate the stable app: {error}"
        )));
    }
    if had_current {
        let _ = fs::remove_dir_all(&backup);
    }
    Ok(())
}

fn app_contract_for_receipt<'a>(
    lock: &'a PeekabooLock,
    receipt: &Receipt,
) -> Result<(&'a AssetLock, &'a str), CliError> {
    if receipt.tag == lock.tag && receipt.commit == lock.commit {
        return Ok((lock.app_asset(), &lock.minimum_macos));
    }
    let release = lock
        .rollback_release(&receipt.tag, &receipt.commit)
        .ok_or_else(|| backend_error("receipt release is not authorized by the embedded lock"))?;
    Ok((release.app_asset(), &release.minimum_macos))
}

fn download_asset(asset: &AssetLock, downloads: &Path) -> Result<PathBuf, CliError> {
    let destination = downloads.join(&asset.name);
    if test_mode::enabled()
        && let Some(fixtures) = std::env::var_os("NILS_MACOS_AGENT_TEST_ASSET_DIR")
    {
        let fixture = PathBuf::from(fixtures).join(&asset.name);
        fs::copy(&fixture, &destination)
            .map_err(|_| backend_error(format!("test asset `{}` is unavailable", asset.name)))?;
        return Ok(destination);
    }
    let args = vec![
        "--fail".into(),
        "--location".into(),
        "--silent".into(),
        "--show-error".into(),
        "--proto".into(),
        "=https".into(),
        "--tlsv1.2".into(),
        "--output".into(),
        destination.to_string_lossy().into_owned(),
        asset.url.clone(),
    ];
    let output = process::run(
        Path::new("curl"),
        &args,
        &[],
        &[],
        None,
        Duration::from_secs(300),
    )
    .map_err(|_| backend_error("failed to start the HTTPS downloader"))?;
    if output.timed_out || output.exit_code != 0 {
        return Err(backend_error(format!(
            "failed to download locked asset `{}`",
            asset.name
        )));
    }
    Ok(destination)
}

fn verify_archive_digest(path: &Path, asset: &AssetLock) -> Result<(), CliError> {
    if hash_file(path)? != asset.sha256 {
        return Err(backend_error(format!(
            "SHA256 mismatch for locked asset `{}`",
            asset.name
        )));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
enum ArchiveKind {
    TarGz,
    Zip,
}

fn validate_archive_listing(path: &Path, kind: ArchiveKind) -> Result<(), CliError> {
    let (program, args) = match kind {
        ArchiveKind::TarGz => (
            Path::new("tar"),
            vec!["-tzf".into(), path.to_string_lossy().into_owned()],
        ),
        ArchiveKind::Zip => (
            Path::new("unzip"),
            vec!["-Z1".into(), path.to_string_lossy().into_owned()],
        ),
    };
    let output = process::run(program, &args, &[], &[], None, Duration::from_secs(30))
        .map_err(|_| backend_error("failed to inspect a locked archive"))?;
    if output.exit_code != 0 || output.timed_out || output.stdout_truncated {
        return Err(backend_error(
            "locked archive listing failed or exceeded its bound",
        ));
    }
    let listing = String::from_utf8(output.stdout)
        .map_err(|_| backend_error("locked archive listing is not UTF-8"))?;
    let mut count = 0usize;
    for raw in listing.lines() {
        count += 1;
        if count > 20_000 {
            return Err(backend_error("locked archive contains too many entries"));
        }
        validate_archive_path(raw)?;
    }
    if count == 0 {
        return Err(backend_error("locked archive is empty"));
    }
    Ok(())
}

fn validate_archive_path(raw: &str) -> Result<(), CliError> {
    let path = Path::new(raw);
    if raw.is_empty()
        || path.is_absolute()
        || raw.contains('\0')
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::Prefix(_)))
    {
        return Err(backend_error("locked archive contains an unsafe path"));
    }
    Ok(())
}

fn extract_archive(path: &Path, destination: &Path, kind: ArchiveKind) -> Result<(), CliError> {
    let (program, args) = match kind {
        ArchiveKind::TarGz => (
            Path::new("tar"),
            vec![
                "-xzf".into(),
                path.to_string_lossy().into_owned(),
                "-C".into(),
                destination.to_string_lossy().into_owned(),
            ],
        ),
        ArchiveKind::Zip => (
            Path::new("unzip"),
            vec![
                "-q".into(),
                path.to_string_lossy().into_owned(),
                "-d".into(),
                destination.to_string_lossy().into_owned(),
            ],
        ),
    };
    let output = process::run(program, &args, &[], &[], None, Duration::from_secs(120))
        .map_err(|_| backend_error("failed to start archive extraction"))?;
    if output.exit_code != 0 || output.timed_out {
        return Err(backend_error("locked archive extraction failed"));
    }
    Ok(())
}

fn validate_symlink_tree(root: &Path) -> Result<(), CliError> {
    let canonical_root = root
        .canonicalize()
        .map_err(|_| backend_error("failed to resolve archive staging root"))?;
    visit_tree(root, &mut |path, file_type| {
        if !file_type.is_symlink() {
            return Ok(());
        }
        let target =
            fs::read_link(path).map_err(|_| backend_error("failed to inspect archive symlink"))?;
        if target.is_absolute() {
            return Err(backend_error("locked archive contains an absolute symlink"));
        }
        let resolved = path
            .canonicalize()
            .map_err(|_| backend_error("locked archive contains a broken symlink"))?;
        if !resolved.starts_with(&canonical_root) {
            return Err(backend_error(
                "locked archive symlink escapes the staging root",
            ));
        }
        Ok(())
    })
}

fn visit_tree(
    path: &Path,
    visitor: &mut impl FnMut(&Path, fs::FileType) -> Result<(), CliError>,
) -> Result<(), CliError> {
    for entry in
        fs::read_dir(path).map_err(|_| backend_error("failed to inspect extracted archive"))?
    {
        let entry =
            entry.map_err(|_| backend_error("failed to inspect extracted archive entry"))?;
        let file_type = entry
            .file_type()
            .map_err(|_| backend_error("failed to classify extracted archive entry"))?;
        let entry_path = entry.path();
        visitor(&entry_path, file_type)?;
        if file_type.is_dir() {
            visit_tree(&entry_path, visitor)?;
        }
    }
    Ok(())
}

fn verify_version(binary: &Path, tag: &str) -> Result<(), CliError> {
    let output = run_tool(binary, &["--version"])?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if output.exit_code != 0 || output.timed_out || !text.contains(tag.trim_start_matches('v')) {
        return Err(backend_error(
            "Peekaboo version probe does not match the receipt",
        ));
    }
    Ok(())
}

fn verify_architectures(binary: &Path, expected: &[String]) -> Result<(), CliError> {
    let args = ["-archs".into(), binary.to_string_lossy().into_owned()];
    let output = run_tool(
        Path::new("lipo"),
        &args.iter().map(String::as_str).collect::<Vec<_>>(),
    )?;
    let actual = String::from_utf8_lossy(&output.stdout);
    if output.exit_code != 0
        || expected
            .iter()
            .any(|arch| !actual.split_whitespace().any(|row| row == arch))
    {
        return Err(backend_error(
            "Peekaboo executable architecture does not match the lock",
        ));
    }
    Ok(())
}

fn verify_app_metadata(
    app: &Path,
    asset: &AssetLock,
    tag: &str,
    minimum_macos: &str,
) -> Result<(), CliError> {
    let raw = fs::read_to_string(app.join("Contents/Info.plist"))
        .map_err(|_| backend_error("Peekaboo app metadata is unavailable"))?;
    let document = roxmltree::Document::parse(&raw)
        .map_err(|_| backend_error("Peekaboo app metadata is malformed"))?;
    let dictionary = document
        .descendants()
        .find(|node| node.has_tag_name("dict"))
        .ok_or_else(|| backend_error("Peekaboo app metadata dictionary is unavailable"))?;
    for (key, expected) in [
        (
            "CFBundleIdentifier",
            asset.bundle_id.as_deref().unwrap_or_default(),
        ),
        ("CFBundleShortVersionString", tag.trim_start_matches('v')),
        ("LSMinimumSystemVersion", minimum_macos),
    ] {
        if expected.is_empty() || plist_string(dictionary, key).as_deref() != Some(expected) {
            return Err(backend_error(
                "Peekaboo app metadata does not match the lock",
            ));
        }
    }
    Ok(())
}

fn plist_string(dictionary: roxmltree::Node<'_, '_>, key: &str) -> Option<String> {
    let elements = dictionary
        .children()
        .filter(roxmltree::Node::is_element)
        .collect::<Vec<_>>();
    let mut matches = elements.chunks_exact(2).filter_map(|pair| {
        (pair[0].has_tag_name("key")
            && pair[0].text() == Some(key)
            && pair[1].has_tag_name("string"))
        .then(|| pair[1].text().map(str::to_string))
        .flatten()
    });
    let value = matches.next()?;
    matches.next().is_none().then_some(value)
}

fn verify_signature(
    path: &Path,
    asset: &AssetLock,
    app: bool,
    require_assessment: bool,
) -> Result<(), CliError> {
    let path_text = path.to_string_lossy().into_owned();
    let mut verify_args = vec!["--verify", "--deep", "--strict", "--verbose=2"];
    verify_args.push(&path_text);
    let verify = run_tool(Path::new("codesign"), &verify_args)?;
    if verify.exit_code != 0 || verify.timed_out {
        return Err(backend_error("Peekaboo code signature verification failed"));
    }
    let metadata_args = vec!["-dv", "--verbose=4", &path_text];
    let metadata = run_tool(Path::new("codesign"), &metadata_args)?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&metadata.stdout),
        String::from_utf8_lossy(&metadata.stderr)
    );
    if metadata.exit_code != 0
        || !text.contains(&asset.signing_authority)
        || !text.contains(&format!("TeamIdentifier={}", asset.team_id))
    {
        return Err(backend_error(
            "Peekaboo signing identity does not match the lock",
        ));
    }
    if app && require_assessment {
        let assessment = run_tool(Path::new("spctl"), &["-a", "-vv", &path_text])?;
        let assessment_text = format!(
            "{}{}",
            String::from_utf8_lossy(&assessment.stdout),
            String::from_utf8_lossy(&assessment.stderr)
        );
        if assessment.exit_code != 0
            || assessment.timed_out
            || !assessment_text.contains("source=Notarized Developer ID")
        {
            return Err(backend_error(
                "Peekaboo app Gatekeeper/notary assessment failed",
            ));
        }
    } else if !app && require_assessment {
        let assessment = run_tool(
            Path::new("codesign"),
            &["-vvvv", "-R=notarized", "--check-notarization", &path_text],
        )?;
        if assessment.exit_code != 0 || assessment.timed_out {
            return Err(backend_error("Peekaboo CLI notarization assessment failed"));
        }
    }
    Ok(())
}

fn run_tool(program: &Path, args: &[&str]) -> Result<process::ProcessOutput, CliError> {
    let program =
        test_mode::verification_tool_override(program).unwrap_or_else(|| program.to_path_buf());
    let args = args
        .iter()
        .map(|arg| (*arg).to_string())
        .collect::<Vec<_>>();
    process::run(&program, &args, &[], &[], None, Duration::from_secs(30))
        .map_err(|_| backend_error("required backend verification tool is unavailable"))
}

fn copy_tree(source: &Path, destination: &Path) -> Result<(), CliError> {
    let metadata = fs::symlink_metadata(source)
        .map_err(|_| backend_error("failed to inspect a backend file during activation"))?;
    if metadata.file_type().is_symlink() {
        let target = fs::read_link(source)
            .map_err(|_| backend_error("failed to read an internal app symlink"))?;
        if target.is_absolute() {
            return Err(backend_error("refusing to copy an absolute app symlink"));
        }
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)
                .map_err(|_| backend_error("failed to create app symlink parent"))?;
        }
        symlink(target, destination)
            .map_err(|_| backend_error("failed to copy an internal app symlink"))?;
        return Ok(());
    }
    if metadata.is_dir() {
        fs::create_dir_all(destination)
            .map_err(|_| backend_error("failed to create backend app directory"))?;
        fs::set_permissions(destination, metadata.permissions())
            .map_err(|_| backend_error("failed to preserve backend app permissions"))?;
        for entry in fs::read_dir(source)
            .map_err(|_| backend_error("failed to enumerate backend app directory"))?
        {
            let entry = entry.map_err(|_| backend_error("failed to read backend app entry"))?;
            copy_tree(&entry.path(), &destination.join(entry.file_name()))?;
        }
        return Ok(());
    }
    copy_file(source, destination, metadata.permissions().mode())
}

fn copy_file(source: &Path, destination: &Path, mode: u32) -> Result<(), CliError> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)
            .map_err(|_| backend_error("failed to create backend file parent"))?;
    }
    fs::copy(source, destination).map_err(|_| backend_error("failed to copy backend file"))?;
    fs::set_permissions(destination, fs::Permissions::from_mode(mode))
        .map_err(|_| backend_error("failed to set backend file permissions"))?;
    Ok(())
}

fn create_private_dir(path: &Path) -> Result<(), CliError> {
    fs::create_dir_all(path)
        .map_err(|_| backend_error("failed to create private backend storage"))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|_| backend_error("failed to secure private backend storage"))?;
    Ok(())
}

fn hash_file(path: &Path) -> Result<String, CliError> {
    sha256_file(path).map_err(|_| backend_error("failed to hash a backend file"))
}

fn read_receipt(path: &Path) -> Result<Option<Receipt>, CliError> {
    let raw = match fs::read(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(backend_error("failed to read a backend receipt")),
    };
    let receipt = serde_json::from_slice::<Receipt>(&raw)
        .map_err(|_| backend_error("backend receipt is malformed"))?;
    if receipt.schema_version != RECEIPT_SCHEMA || !safe_tag(&receipt.tag) {
        return Err(backend_error("backend receipt is unsupported or unsafe"));
    }
    Ok(Some(receipt))
}

fn write_receipt(path: &Path, receipt: &Receipt) -> Result<(), CliError> {
    let body = serde_json::to_vec_pretty(receipt)
        .map_err(|_| backend_error("failed to encode backend receipt"))?;
    write_atomic(path, &body, SECRET_FILE_MODE)
        .map_err(|_| backend_error("failed to atomically write backend receipt"))
}

fn safe_tag(tag: &str) -> bool {
    !tag.is_empty()
        && tag.len() <= 64
        && tag
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

fn ensure_supported_platform() -> Result<(), CliError> {
    if test_mode::enabled() {
        return Ok(());
    }
    if !cfg!(target_os = "macos") {
        return Err(backend_error("backend operations require macOS"));
    }
    let args = vec!["-productVersion".to_string()];
    let output = process::run(
        Path::new("/usr/bin/sw_vers"),
        &args,
        &[],
        &[],
        None,
        Duration::from_secs(5),
    )
    .map_err(|_| backend_error("failed to inspect the running macOS version"))?;
    let version = String::from_utf8_lossy(&output.stdout);
    if output.exit_code != 0
        || output.timed_out
        || output.stdout_truncated
        || !macos_version_supported(version.trim(), "15.0")
    {
        return Err(backend_error(
            "the running macOS version is below the locked 15.0 minimum",
        ));
    }
    Ok(())
}

fn macos_version_supported(running: &str, minimum: &str) -> bool {
    fn major_minor(value: &str) -> Option<(u64, u64)> {
        let mut components = value.split('.');
        let major = components.next()?.parse().ok()?;
        let minor = components.next().unwrap_or("0").parse().ok()?;
        Some((major, minor))
    }
    major_minor(running)
        .zip(major_minor(minimum))
        .is_some_and(|(running, minimum)| running >= minimum)
}

fn passed(id: &str, message: &str) -> CheckResult {
    CheckResult {
        id: id.into(),
        status: "pass",
        message: message.into(),
    }
}

fn backend_error(message: impl Into<String>) -> CliError {
    CliError::backend(message).with_operation("backend")
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    use tempfile::TempDir;

    use super::{
        LifecycleLock, LifecycleLockMode, RECEIPT_SCHEMA, Receipt, app_contract_for_receipt,
        macos_version_supported, safe_tag, validate_archive_path, validate_symlink_tree,
    };
    use crate::lock::PeekabooLock;

    #[test]
    fn rejects_archive_traversal_and_absolute_paths() {
        assert!(validate_archive_path("Peekaboo.app/Contents/MacOS/Peekaboo").is_ok());
        assert!(validate_archive_path("../escape").is_err());
        assert!(validate_archive_path("a/../../escape").is_err());
        assert!(validate_archive_path("/absolute").is_err());
    }

    #[test]
    fn accepts_internal_symlinks_and_rejects_escaping_links() {
        let root = TempDir::new().expect("root");
        fs::create_dir_all(root.path().join("Framework/Versions/B")).expect("tree");
        symlink("Versions/B", root.path().join("Framework/Current")).expect("internal link");
        assert!(validate_symlink_tree(root.path()).is_ok());

        symlink("/etc/passwd", root.path().join("escape")).expect("escaping link");
        assert!(validate_symlink_tree(root.path()).is_err());
    }

    #[test]
    fn receipt_tags_cannot_escape_version_storage() {
        assert!(safe_tag("v3.9.3"));
        assert!(!safe_tag("../v3.9.3"));
        assert!(!safe_tag("v3/9/3"));
    }

    #[test]
    fn shared_execution_lease_blocks_backend_mutation_until_release() {
        let root = TempDir::new().expect("root");
        let lease =
            LifecycleLock::acquire(root.path(), LifecycleLockMode::Shared).expect("shared lease");
        let lock_root = root.path().to_path_buf();
        let (acquired_tx, acquired_rx) = mpsc::channel();
        let mutation = thread::spawn(move || {
            let _guard = LifecycleLock::acquire(&lock_root, LifecycleLockMode::Exclusive)
                .expect("exclusive mutation lock");
            acquired_tx.send(()).expect("notify");
        });
        assert!(
            acquired_rx.recv_timeout(Duration::from_millis(50)).is_err(),
            "mutation acquired the lifecycle lock while execution still held a lease"
        );
        drop(lease);
        acquired_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("mutation proceeds after lease release");
        mutation.join().expect("mutation thread");
    }

    #[test]
    fn running_macos_must_satisfy_the_locked_minimum_floor() {
        assert!(!macos_version_supported("14.7.6", "15.0"));
        assert!(macos_version_supported("15.0", "15.0"));
        assert!(macos_version_supported("15.4.1", "15.0"));
        assert!(macos_version_supported("16.0", "15.0"));
        assert!(!macos_version_supported("unknown", "15.0"));
    }

    #[test]
    fn rollback_activation_selects_the_historical_app_identity() {
        let mut lock = PeekabooLock::embedded().expect("lock");
        let mut historical_app = lock.app_asset().clone();
        historical_app.signing_authority = "Historical App Authority".into();
        historical_app.team_id = "HISTORY".into();
        lock.rollback_releases
            .push(crate::lock::RollbackReleaseLock {
                tag: "v3.9.2".into(),
                commit: "2".repeat(40),
                minimum_macos: "15.0".into(),
                assets: vec![lock.cli_asset().clone(), historical_app],
            });
        let receipt = Receipt {
            schema_version: RECEIPT_SCHEMA.into(),
            tag: "v3.9.2".into(),
            commit: "2".repeat(40),
            installed_at: "fixture".into(),
            cli_archive_sha256: String::new(),
            app_archive_sha256: String::new(),
            cli_binary_sha256: String::new(),
            app_binary_sha256: String::new(),
        };
        let (asset, minimum) = app_contract_for_receipt(&lock, &receipt).expect("historical");
        assert_eq!(asset.signing_authority, "Historical App Authority");
        assert_eq!(asset.team_id, "HISTORY");
        assert_eq!(minimum, "15.0");
    }
}
