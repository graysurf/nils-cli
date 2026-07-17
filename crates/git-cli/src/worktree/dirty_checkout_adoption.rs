use super::{CliError, detect_format, emit_error, emit_success, take_format};
use anyhow::{Context, Result, bail, ensure};
use nils_common::cli_contract::{OutputFormat, exit};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::env;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
#[cfg(test)]
use std::os::unix::fs::PermissionsExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use thiserror::Error;

const SNAPSHOT_SCHEMA: &str = "agent-runtime.dirty-checkout-snapshot.v1";
const CHALLENGE_SCHEMA: &str = "agent-runtime.dirty-checkout-challenge.v1";
const RECEIPT_SCHEMA: &str = "agent-runtime.dirty-checkout-receipt.v1";
const LEASE_V1_SCHEMA: &str = "agent-runtime.checkout-lease.v1";
const LEASE_V2_SCHEMA: &str = "agent-runtime.checkout-lease.v2";
const ADOPTION_SCHEMA: &str = "agent-runtime.dirty-checkout-adoption.v1";
const PENDING_ADOPTION_SCHEMA: &str = "agent-runtime.dirty-checkout-pending.v1";
const INSTANCE_FILE: &str = ".agent-runtime-checkout-instance";
const MAX_GIT_OUTPUT_BYTES: usize = 64 * 1024 * 1024;
const MAX_GIT_STDERR_BYTES: usize = 1024 * 1024;
const MAX_CAPTURE_BYTES: usize = 64 * 1024 * 1024;
const MAX_PATH_BYTES: usize = 32 * 1024 * 1024;
const MAX_STATE_FILE_BYTES: usize = 16 * 1024;
const MAX_REASON_BYTES: usize = 2_000;
const MAX_ENTRY_COUNT: usize = 100_000;
const MAX_FILE_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(30);
const GIT_TIMEOUT: Duration = Duration::from_secs(10);
const LOCK_WAIT: Duration = Duration::from_secs(2);
const LOCK_POLL: Duration = Duration::from_millis(50);
const DEFAULT_LEASE_TTL_SECONDS: u64 = 8 * 60 * 60;
const MAX_LEASE_TTL_SECONDS: u64 = 7 * 24 * 60 * 60;
const UNBORN_HEAD_PREFIX: &str = "unborn:";
const TRUSTED_GIT_PATHS: &[&str] = &["/usr/bin/git", "/bin/git"];
const SNAPSHOT_WORKER_ENV: &str = "NILS_GIT_CLI_INTERNAL_DIRTY_SNAPSHOT_WORKER";
const SNAPSHOT_WORKER_CHECKOUT_ENV: &str = "NILS_GIT_CLI_INTERNAL_DIRTY_SNAPSHOT_CHECKOUT";
const SNAPSHOT_WORKER_SCHEMA: &str = "nils-git-cli.dirty-snapshot-worker.v1";

#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirtyCheckoutErrorKind {
    CleanCheckout,
    UnsupportedGitState,
    ChallengeExpired,
    ChallengeReused,
    ChallengeDrift,
    ForeignLease,
    MalformedState,
    InvalidInput,
    Timeout,
    ResourceUnavailable,
    SnapshotFailed,
    AdoptionFailed,
    RevocationFailed,
}

impl DirtyCheckoutErrorKind {
    pub const fn code(self) -> &'static str {
        match self {
            Self::CleanCheckout => "dirty-checkout-clean",
            Self::UnsupportedGitState => "dirty-checkout-unsupported-git-state",
            Self::ChallengeExpired => "dirty-checkout-challenge-expired",
            Self::ChallengeReused => "dirty-checkout-challenge-reused",
            Self::ChallengeDrift => "dirty-checkout-challenge-drift",
            Self::ForeignLease => "dirty-checkout-foreign-lease",
            Self::MalformedState => "dirty-checkout-malformed-state",
            Self::InvalidInput => "dirty-checkout-invalid-input",
            Self::Timeout => "dirty-checkout-timeout",
            Self::ResourceUnavailable => "dirty-checkout-resource-unavailable",
            Self::SnapshotFailed => "dirty-checkout-snapshot-failed",
            Self::AdoptionFailed => "dirty-checkout-adoption-failed",
            Self::RevocationFailed => "dirty-checkout-revoke-failed",
        }
    }

    const fn exit_code(self) -> i32 {
        match self {
            Self::Timeout
            | Self::ResourceUnavailable
            | Self::SnapshotFailed
            | Self::AdoptionFailed
            | Self::RevocationFailed => exit::RUNTIME,
            Self::CleanCheckout
            | Self::UnsupportedGitState
            | Self::ChallengeExpired
            | Self::ChallengeReused
            | Self::ChallengeDrift
            | Self::ForeignLease
            | Self::MalformedState
            | Self::InvalidInput => exit::DATA,
        }
    }
}

#[derive(Debug, Error)]
#[error("{message}")]
pub struct DirtyCheckoutError {
    kind: DirtyCheckoutErrorKind,
    message: Box<str>,
}

impl DirtyCheckoutError {
    fn new(kind: DirtyCheckoutErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into().into_boxed_str(),
        }
    }

    pub const fn kind(&self) -> DirtyCheckoutErrorKind {
        self.kind
    }

    pub const fn code(&self) -> &'static str {
        self.kind.code()
    }
}

fn domain_error(kind: DirtyCheckoutErrorKind, message: impl Into<String>) -> anyhow::Error {
    DirtyCheckoutError::new(kind, message).into()
}

fn command_error(
    error: anyhow::Error,
    fallback: DirtyCheckoutErrorKind,
    message: impl AsRef<str>,
) -> anyhow::Error {
    if error.downcast_ref::<DirtyCheckoutError>().is_some() {
        error
    } else {
        domain_error(fallback, format!("{}: {error}", message.as_ref()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DirtySnapshot {
    pub schema: &'static str,
    pub repository_key: String,
    pub checkout_key: String,
    pub checkout_instance: String,
    pub snapshot_id: String,
    pub head_oid: String,
    pub branch_ref_digest: String,
    pub tracked_entries: usize,
    pub untracked_entries: usize,
    pub hashed_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AdoptionReceipt {
    pub receipt_id: String,
    pub snapshot_id: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotWorkerSnapshot {
    schema: String,
    repository_key: String,
    checkout_key: String,
    checkout_instance: String,
    snapshot_id: String,
    head_oid: String,
    branch_ref_digest: String,
    tracked_entries: usize,
    untracked_entries: usize,
    hashed_bytes: u64,
}

impl From<DirtySnapshot> for SnapshotWorkerSnapshot {
    fn from(snapshot: DirtySnapshot) -> Self {
        Self {
            schema: snapshot.schema.to_string(),
            repository_key: snapshot.repository_key,
            checkout_key: snapshot.checkout_key,
            checkout_instance: snapshot.checkout_instance,
            snapshot_id: snapshot.snapshot_id,
            head_oid: snapshot.head_oid,
            branch_ref_digest: snapshot.branch_ref_digest,
            tracked_entries: snapshot.tracked_entries,
            untracked_entries: snapshot.untracked_entries,
            hashed_bytes: snapshot.hashed_bytes,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotWorkerError {
    code: String,
    message: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotWorkerResponse {
    schema: String,
    snapshot: Option<SnapshotWorkerSnapshot>,
    error: Option<SnapshotWorkerError>,
}

#[derive(Debug)]
struct CheckoutIdentity {
    root: PathBuf,
    git_dir: PathBuf,
    common_dir: PathBuf,
    repository_key: String,
    checkout_key: String,
    checkout_instance: String,
}

#[derive(Debug, Clone)]
struct IndexEntry {
    mode: Vec<u8>,
    oid: Vec<u8>,
    path: Vec<u8>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ChallengeRecord {
    schema: String,
    token_digest: String,
    session_key: String,
    repository_key: String,
    checkout_key: String,
    checkout_instance: String,
    snapshot_id: String,
    head_oid: String,
    branch_ref_digest: String,
    authorization_turn_digest: String,
    issued_at: u64,
    expires_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AdoptionRecord {
    schema: String,
    receipt_schema: String,
    receipt_id: String,
    snapshot_id: String,
    authorization_turn_digest: String,
    reason_digest: String,
    adopted_at: u64,
    challenge_issued_at: u64,
    challenge_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
enum LeaseRecord {
    V1(LeaseV1Wire),
    V2(Box<LeaseV2Wire>),
}

impl LeaseRecord {
    fn schema(&self) -> &str {
        match self {
            Self::V1(lease) => &lease.schema,
            Self::V2(lease) => &lease.schema,
        }
    }

    fn session_key(&self) -> &str {
        match self {
            Self::V1(lease) => &lease.session_key,
            Self::V2(lease) => &lease.session_key,
        }
    }

    fn checkout_instance(&self) -> &str {
        match self {
            Self::V1(lease) => &lease.checkout_instance,
            Self::V2(lease) => &lease.checkout_instance,
        }
    }

    fn checkout_root(&self) -> &str {
        match self {
            Self::V1(lease) => &lease.checkout_root,
            Self::V2(lease) => &lease.checkout_root,
        }
    }

    fn checkout_git_dir(&self) -> &str {
        match self {
            Self::V1(lease) => &lease.checkout_git_dir,
            Self::V2(lease) => &lease.checkout_git_dir,
        }
    }

    fn acquired_at(&self) -> u64 {
        match self {
            Self::V1(lease) => lease.acquired_at,
            Self::V2(lease) => lease.acquired_at,
        }
    }

    fn refreshed_at(&self) -> u64 {
        match self {
            Self::V1(lease) => lease.refreshed_at,
            Self::V2(lease) => lease.refreshed_at,
        }
    }

    fn expires_at(&self) -> u64 {
        match self {
            Self::V1(lease) => lease.expires_at,
            Self::V2(lease) => lease.expires_at,
        }
    }

    fn adoption(&self) -> Option<&AdoptionRecord> {
        match self {
            Self::V1(_) => None,
            Self::V2(lease) => Some(&lease.adoption),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LeaseV1Wire {
    schema: String,
    session_key: String,
    checkout_instance: String,
    checkout_root: String,
    checkout_git_dir: String,
    acquired_at: u64,
    refreshed_at: u64,
    expires_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LeaseV2Wire {
    schema: String,
    session_key: String,
    checkout_instance: String,
    checkout_root: String,
    checkout_git_dir: String,
    checkout_root_bytes: String,
    checkout_git_dir_bytes: String,
    acquired_at: u64,
    refreshed_at: u64,
    expires_at: u64,
    adoption: AdoptionRecord,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum LeaseWire {
    V1(LeaseV1Wire),
    V2(Box<LeaseV2Wire>),
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReceiptRecord {
    schema: String,
    receipt_id: String,
    session_key: String,
    repository_key: String,
    checkout_key: String,
    checkout_instance: String,
    snapshot_id: String,
    authorization_turn_digest: String,
    reason_digest: String,
    challenge_digest: String,
    adopted_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PendingAdoptionRecord {
    schema: String,
    receipt_id: String,
    challenge_digest: String,
    session_key: String,
    checkout_instance: String,
    snapshot_id: String,
    predecessor_receipt_id: Option<String>,
    predecessor_receipt_digest: Option<String>,
    predecessor_spent_challenge_digest: Option<String>,
}

#[derive(Debug)]
struct SnapshotPass {
    snapshot: DirtySnapshot,
}

#[derive(Debug, Default)]
struct SnapshotBudget {
    capture_bytes: usize,
    path_bytes: usize,
    file_bytes: u64,
}

impl SnapshotBudget {
    fn output_limit(&self, command_limit: usize) -> Result<usize> {
        let remaining = MAX_CAPTURE_BYTES.saturating_sub(self.capture_bytes);
        if remaining == 0 {
            return Err(domain_error(
                DirtyCheckoutErrorKind::ResourceUnavailable,
                "dirty snapshot aggregate capture budget is exhausted",
            ));
        }
        Ok(command_limit.min(remaining))
    }

    fn charge_output(&mut self, output: &std::process::Output) -> Result<()> {
        let captured = output.stdout.len().saturating_add(output.stderr.len());
        self.capture_bytes = self.capture_bytes.checked_add(captured).ok_or_else(|| {
            domain_error(
                DirtyCheckoutErrorKind::ResourceUnavailable,
                "dirty snapshot aggregate capture budget overflowed",
            )
        })?;
        if self.capture_bytes > MAX_CAPTURE_BYTES {
            return Err(domain_error(
                DirtyCheckoutErrorKind::ResourceUnavailable,
                "dirty snapshot aggregate capture budget is exceeded",
            ));
        }
        Ok(())
    }

    fn charge_path(&mut self, bytes: usize) -> Result<()> {
        self.path_bytes = self.path_bytes.checked_add(bytes).ok_or_else(|| {
            domain_error(
                DirtyCheckoutErrorKind::ResourceUnavailable,
                "dirty snapshot path-byte budget overflowed",
            )
        })?;
        if self.path_bytes > MAX_PATH_BYTES {
            return Err(domain_error(
                DirtyCheckoutErrorKind::ResourceUnavailable,
                "dirty snapshot path-byte budget is exceeded",
            ));
        }
        Ok(())
    }
}

struct FramedHasher(Sha256);

impl FramedHasher {
    fn new() -> Self {
        Self(Sha256::new())
    }

    fn field(&mut self, tag: &[u8], value: &[u8]) {
        self.0.update((tag.len() as u32).to_be_bytes());
        self.0.update(tag);
        self.0.update((value.len() as u64).to_be_bytes());
        self.0.update(value);
    }

    fn finish(self) -> String {
        hex_bytes(&self.0.finalize())
    }
}

pub fn dirty_snapshot(checkout: &Path) -> Result<DirtySnapshot> {
    dirty_snapshot_worker(checkout, deadline_after(SNAPSHOT_TIMEOUT)?)
}

fn dirty_snapshot_cli(checkout: &Path) -> Result<DirtySnapshot> {
    let deadline = deadline_after(SNAPSHOT_TIMEOUT)?;
    let executable = snapshot_worker_executable()?;
    ensure_deadline(deadline)?;
    let mut command = Command::new(executable);
    command
        .env(SNAPSHOT_WORKER_ENV, "1")
        .env(SNAPSHOT_WORKER_CHECKOUT_ENV, checkout.as_os_str())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let output = run_snapshot_worker_command(&mut command, deadline)?;
    decode_snapshot_worker_output(output)
}

fn decode_snapshot_worker_output(output: std::process::Output) -> Result<DirtySnapshot> {
    if !output.status.success() || !output.stderr.is_empty() {
        return Err(domain_error(
            DirtyCheckoutErrorKind::SnapshotFailed,
            "dirty snapshot worker failed without a valid response",
        ));
    }
    let response: SnapshotWorkerResponse =
        serde_json::from_slice(&output.stdout).map_err(|_| {
            domain_error(
                DirtyCheckoutErrorKind::SnapshotFailed,
                "dirty snapshot worker response is malformed",
            )
        })?;
    if response.schema != SNAPSHOT_WORKER_SCHEMA {
        return Err(domain_error(
            DirtyCheckoutErrorKind::SnapshotFailed,
            "dirty snapshot worker response schema is unsupported",
        ));
    }
    match (response.snapshot, response.error) {
        (Some(snapshot), None) if snapshot.schema == SNAPSHOT_SCHEMA => Ok(DirtySnapshot {
            schema: SNAPSHOT_SCHEMA,
            repository_key: snapshot.repository_key,
            checkout_key: snapshot.checkout_key,
            checkout_instance: snapshot.checkout_instance,
            snapshot_id: snapshot.snapshot_id,
            head_oid: snapshot.head_oid,
            branch_ref_digest: snapshot.branch_ref_digest,
            tracked_entries: snapshot.tracked_entries,
            untracked_entries: snapshot.untracked_entries,
            hashed_bytes: snapshot.hashed_bytes,
        }),
        (None, Some(error)) => {
            let kind = error_kind_from_code(&error.code).ok_or_else(|| {
                domain_error(
                    DirtyCheckoutErrorKind::SnapshotFailed,
                    "dirty snapshot worker returned an unsupported error code",
                )
            })?;
            Err(domain_error(kind, error.message))
        }
        _ => Err(domain_error(
            DirtyCheckoutErrorKind::SnapshotFailed,
            "dirty snapshot worker response is inconsistent",
        )),
    }
}

pub(crate) fn run_internal_snapshot_worker() -> Option<i32> {
    if env::var_os(SNAPSHOT_WORKER_ENV).as_deref() != Some(OsStr::new("1")) {
        return None;
    }
    let result = env::var_os(SNAPSHOT_WORKER_CHECKOUT_ENV)
        .map(PathBuf::from)
        .context("dirty snapshot worker checkout is unavailable")
        .and_then(|checkout| dirty_snapshot_worker(&checkout, Instant::now() + SNAPSHOT_TIMEOUT));
    let response = match result {
        Ok(snapshot) => SnapshotWorkerResponse {
            schema: SNAPSHOT_WORKER_SCHEMA.to_string(),
            snapshot: Some(snapshot.into()),
            error: None,
        },
        Err(error) => {
            let domain = error.downcast_ref::<DirtyCheckoutError>();
            SnapshotWorkerResponse {
                schema: SNAPSHOT_WORKER_SCHEMA.to_string(),
                snapshot: None,
                error: Some(SnapshotWorkerError {
                    code: domain
                        .map_or(DirtyCheckoutErrorKind::SnapshotFailed.code(), |error| {
                            error.code()
                        })
                        .to_string(),
                    message: error.to_string(),
                }),
            }
        }
    };
    match serde_json::to_writer(io::stdout().lock(), &response) {
        Ok(()) => Some(0),
        Err(_) => Some(exit::RUNTIME),
    }
}

fn snapshot_worker_executable() -> Result<PathBuf> {
    let current = env::current_exe()
        .and_then(fs::canonicalize)
        .context("dirty snapshot worker executable could not be resolved")?;
    let candidate = if current.file_name() == Some(OsStr::new("git-cli")) {
        current
    } else {
        let executable_dir = current
            .parent()
            .context("current executable has no parent directory")?;
        let target_dir = if executable_dir.file_name() == Some(OsStr::new("deps")) {
            executable_dir
                .parent()
                .context("test executable directory has no parent")?
        } else {
            executable_dir
        };
        fs::canonicalize(target_dir.join("git-cli"))
            .context("dirty snapshot worker executable is unavailable")?
    };
    let metadata = fs::metadata(&candidate)
        .context("dirty snapshot worker executable metadata is unavailable")?;
    let owner = metadata.uid();
    let mode = metadata.mode();
    if !metadata.file_type().is_file()
        || (owner != 0 && owner != unsafe { libc::geteuid() })
        || mode & 0o002 != 0
        || (owner == 0 && mode & 0o020 != 0)
    {
        return Err(domain_error(
            DirtyCheckoutErrorKind::ResourceUnavailable,
            "dirty snapshot worker executable is not trusted",
        ));
    }
    Ok(candidate)
}

fn run_snapshot_worker_command(
    command: &mut Command,
    deadline: Instant,
) -> Result<std::process::Output> {
    output_with_aggregate_limit_until(
        command,
        deadline,
        MAX_STATE_FILE_BYTES,
        4096,
        MAX_STATE_FILE_BYTES,
    )
}

fn error_kind_from_code(code: &str) -> Option<DirtyCheckoutErrorKind> {
    [
        DirtyCheckoutErrorKind::CleanCheckout,
        DirtyCheckoutErrorKind::UnsupportedGitState,
        DirtyCheckoutErrorKind::ChallengeExpired,
        DirtyCheckoutErrorKind::ChallengeReused,
        DirtyCheckoutErrorKind::ChallengeDrift,
        DirtyCheckoutErrorKind::ForeignLease,
        DirtyCheckoutErrorKind::MalformedState,
        DirtyCheckoutErrorKind::InvalidInput,
        DirtyCheckoutErrorKind::Timeout,
        DirtyCheckoutErrorKind::ResourceUnavailable,
        DirtyCheckoutErrorKind::SnapshotFailed,
        DirtyCheckoutErrorKind::AdoptionFailed,
        DirtyCheckoutErrorKind::RevocationFailed,
    ]
    .into_iter()
    .find(|kind| kind.code() == code)
}

fn dirty_snapshot_worker(checkout: &Path, deadline: Instant) -> Result<DirtySnapshot> {
    let first = snapshot_once(checkout, deadline)?;
    let second = snapshot_once(checkout, deadline)?;
    if first.snapshot != second.snapshot {
        return Err(domain_error(
            DirtyCheckoutErrorKind::SnapshotFailed,
            "checkout changed while the dirty snapshot was calculated",
        ));
    }
    Ok(first.snapshot)
}

pub fn adopt_dirty(
    checkout: &Path,
    state_root: &Path,
    challenge_token: &str,
    reason_file: &Path,
) -> Result<AdoptionReceipt> {
    adopt_dirty_with_snapshot(
        checkout,
        state_root,
        challenge_token,
        reason_file,
        dirty_snapshot,
    )
}

fn adopt_dirty_cli(
    checkout: &Path,
    state_root: &Path,
    challenge_token: &str,
    reason_file: &Path,
) -> Result<AdoptionReceipt> {
    adopt_dirty_with_snapshot(
        checkout,
        state_root,
        challenge_token,
        reason_file,
        dirty_snapshot_cli,
    )
}

fn adopt_dirty_with_snapshot<F>(
    checkout: &Path,
    state_root: &Path,
    challenge_token: &str,
    reason_file: &Path,
    mut snapshotter: F,
) -> Result<AdoptionReceipt>
where
    F: FnMut(&Path) -> Result<DirtySnapshot>,
{
    adopt_dirty_inner(
        checkout,
        state_root,
        challenge_token,
        reason_file,
        &mut snapshotter,
    )
    .map_err(|error| {
        command_error(
            error,
            DirtyCheckoutErrorKind::AdoptionFailed,
            "dirty checkout adoption failed",
        )
    })
}

fn adopt_dirty_inner<F>(
    checkout: &Path,
    state_root: &Path,
    challenge_token: &str,
    reason_file: &Path,
    snapshotter: &mut F,
) -> Result<AdoptionReceipt>
where
    F: FnMut(&Path) -> Result<DirtySnapshot>,
{
    if !is_lower_hex(challenge_token, 64) {
        return Err(domain_error(
            DirtyCheckoutErrorKind::InvalidInput,
            "challenge token is malformed",
        ));
    }
    let challenge_digest = sha256_hex(challenge_token.as_bytes());
    let identity = resolve_checkout(checkout, false, deadline_after(SNAPSHOT_TIMEOUT)?)?;
    let checkout_dir = checkout_state_dir(state_root, &identity)?;
    let challenge_dir = checkout_dir.join("challenges");
    verify_private_directory(&challenge_dir).map_err(|error| {
        command_error(
            error,
            DirtyCheckoutErrorKind::MalformedState,
            "dirty-checkout challenge directory is untrusted",
        )
    })?;
    let _lock = LeaseLock::acquire(&checkout_dir)?;

    let challenge_path = challenge_dir.join(format!("{challenge_digest}.json"));
    if matches!(
        recover_pending_adoption(&checkout_dir, &challenge_dir, &identity, &challenge_digest)?,
        PendingRecovery::Committed | PendingRecovery::Revoked
    ) {
        return Err(domain_error(
            DirtyCheckoutErrorKind::ChallengeReused,
            "dirty-checkout challenge was consumed by a recovered adoption",
        ));
    }
    let challenge_bytes = match read_private_regular(&challenge_path, MAX_STATE_FILE_BYTES, true) {
        Ok(bytes) => bytes,
        Err(error)
            if error
                .downcast_ref::<io::Error>()
                .is_some_and(|error| error.kind() == io::ErrorKind::NotFound) =>
        {
            return Err(domain_error(
                DirtyCheckoutErrorKind::ChallengeReused,
                "dirty-checkout challenge is unavailable or already consumed",
            ));
        }
        Err(error) => {
            return Err(command_error(
                error,
                DirtyCheckoutErrorKind::MalformedState,
                "dirty-checkout challenge state is untrusted",
            ));
        }
    };
    let challenge: ChallengeRecord = serde_json::from_slice(&challenge_bytes).map_err(|_| {
        domain_error(
            DirtyCheckoutErrorKind::MalformedState,
            "dirty-checkout challenge state is malformed",
        )
    })?;
    validate_challenge(&challenge, &identity, &challenge_digest)?;

    let reason = read_regular_bounded(reason_file, MAX_REASON_BYTES).map_err(|error| {
        command_error(
            error,
            DirtyCheckoutErrorKind::InvalidInput,
            "adoption reason is invalid",
        )
    })?;
    let reason_text = std::str::from_utf8(&reason).map_err(|_| {
        domain_error(
            DirtyCheckoutErrorKind::InvalidInput,
            "adoption reason must be valid UTF-8",
        )
    })?;
    if reason_text.trim().is_empty() {
        return Err(domain_error(
            DirtyCheckoutErrorKind::InvalidInput,
            "adoption reason must not be empty",
        ));
    }
    let reason_digest = sha256_hex(&reason);
    let initial_now = unix_time()?;

    let lease_path = checkout_dir.join("lease.json");
    let existing = load_lease(&lease_path)?;
    if let Some(lease) = &existing {
        validate_lease(lease, &identity)?;
        if lease.expires_at() > initial_now && lease.session_key() != challenge.session_key {
            return Err(domain_error(
                DirtyCheckoutErrorKind::ForeignLease,
                "another session owns the active checkout lease",
            ));
        }
        if lease.expires_at() > initial_now
            && lease.session_key() == challenge.session_key
            && let Some(adoption) = lease.adoption()
        {
            if adoption.challenge_digest == challenge_digest {
                return Err(domain_error(
                    DirtyCheckoutErrorKind::ChallengeReused,
                    "dirty-checkout challenge has already been consumed",
                ));
            }
            return Err(domain_error(
                DirtyCheckoutErrorKind::AdoptionFailed,
                "checkout already has a current-session dirty adoption",
            ));
        }
    }
    let predecessor = match existing
        .as_ref()
        .filter(|lease| lease.expires_at() <= initial_now)
    {
        Some(lease) if lease.adoption().is_some() => {
            expired_predecessor(&checkout_dir, &identity, lease)?
                .ok_or_else(|| {
                    domain_error(
                        DirtyCheckoutErrorKind::MalformedState,
                        "expired adoption predecessor state is incomplete or malformed",
                    )
                })
                .map(Some)?
        }
        _ => None,
    };

    let initial_snapshot = snapshotter(&identity.root)?;
    validate_snapshot_matches_challenge(&initial_snapshot, &challenge)?;
    validate_adoption_boundary(&challenge, &identity, unix_time()?)?;

    let receipts_dir = checkout_dir.join("receipts");
    private_directory(&receipts_dir).map_err(|error| {
        command_error(
            error,
            DirtyCheckoutErrorKind::ResourceUnavailable,
            "dirty-checkout receipt directory is unavailable",
        )
    })?;
    let receipt_id = random_hex(32)?;
    let receipt_path = receipts_dir.join(format!("{receipt_id}.json"));
    let spent_challenge_path = receipts_dir.join(format!(".challenge-{receipt_id}.json"));
    let checkout_root_text = identity.root.to_str().ok_or_else(|| {
        domain_error(
            DirtyCheckoutErrorKind::InvalidInput,
            "checkout root is not valid UTF-8 for the lease-v2 wire format",
        )
    })?;
    let checkout_git_dir_text = identity.git_dir.to_str().ok_or_else(|| {
        domain_error(
            DirtyCheckoutErrorKind::InvalidInput,
            "checkout Git directory is not valid UTF-8 for the lease-v2 wire format",
        )
    })?;
    let pending = PendingAdoptionRecord {
        schema: PENDING_ADOPTION_SCHEMA.to_string(),
        receipt_id: receipt_id.clone(),
        challenge_digest: challenge_digest.clone(),
        session_key: challenge.session_key.clone(),
        checkout_instance: identity.checkout_instance.clone(),
        snapshot_id: challenge.snapshot_id.clone(),
        predecessor_receipt_id: predecessor
            .as_ref()
            .map(|predecessor| predecessor.receipt_id.clone()),
        predecessor_receipt_digest: predecessor
            .as_ref()
            .map(|predecessor| predecessor.receipt_digest.clone()),
        predecessor_spent_challenge_digest: predecessor
            .as_ref()
            .map(|predecessor| predecessor.spent_challenge_digest.clone()),
    };
    validate_pending_adoption(&pending, &identity, &challenge_digest)?;
    let pending_path = pending_adoption_path(&checkout_dir, &challenge_digest);
    if let Err(error) = write_json_atomic(&pending_path, &pending, false) {
        return Err(transition_failure_after_recovery(
            error,
            &checkout_dir,
            &challenge_dir,
            &identity,
            &challenge_digest,
        ));
    }

    let build_transition =
        |snapshot: &DirtySnapshot, transition_at: u64| -> Result<(ReceiptRecord, LeaseRecord)> {
            let receipt = ReceiptRecord {
                schema: RECEIPT_SCHEMA.to_string(),
                receipt_id: receipt_id.clone(),
                session_key: challenge.session_key.clone(),
                repository_key: identity.repository_key.clone(),
                checkout_key: identity.checkout_key.clone(),
                checkout_instance: identity.checkout_instance.clone(),
                snapshot_id: snapshot.snapshot_id.clone(),
                authorization_turn_digest: challenge.authorization_turn_digest.clone(),
                reason_digest: reason_digest.clone(),
                challenge_digest: challenge_digest.clone(),
                adopted_at: transition_at,
            };
            let acquired_at = existing
                .as_ref()
                .filter(|lease| lease.session_key() == challenge.session_key)
                .map_or(transition_at, LeaseRecord::acquired_at);
            let lease = LeaseRecord::V2(Box::new(LeaseV2Wire {
                schema: LEASE_V2_SCHEMA.to_string(),
                session_key: challenge.session_key.clone(),
                checkout_instance: identity.checkout_instance.clone(),
                checkout_root: checkout_root_text.to_string(),
                checkout_git_dir: checkout_git_dir_text.to_string(),
                checkout_root_bytes: hex_bytes(identity.root.as_os_str().as_bytes()),
                checkout_git_dir_bytes: hex_bytes(identity.git_dir.as_os_str().as_bytes()),
                acquired_at,
                refreshed_at: transition_at,
                expires_at: transition_at.saturating_add(lease_ttl_seconds()),
                adoption: AdoptionRecord {
                    schema: ADOPTION_SCHEMA.to_string(),
                    receipt_schema: RECEIPT_SCHEMA.to_string(),
                    receipt_id: receipt_id.clone(),
                    snapshot_id: snapshot.snapshot_id.clone(),
                    authorization_turn_digest: receipt.authorization_turn_digest.clone(),
                    reason_digest: reason_digest.clone(),
                    adopted_at: transition_at,
                    challenge_issued_at: challenge.issued_at,
                    challenge_digest: challenge_digest.clone(),
                },
            }));
            validate_lease(&lease, &identity)?;
            Ok((receipt, lease))
        };

    let prepared_snapshot = transition_result_after_recovery(
        snapshotter(&identity.root),
        &checkout_dir,
        &challenge_dir,
        &identity,
        &challenge_digest,
    )?;
    transition_result_after_recovery(
        validate_snapshot_matches_challenge(&prepared_snapshot, &challenge),
        &checkout_dir,
        &challenge_dir,
        &identity,
        &challenge_digest,
    )?;
    let prepared_at = transition_result_after_recovery(
        unix_time(),
        &checkout_dir,
        &challenge_dir,
        &identity,
        &challenge_digest,
    )?;
    transition_result_after_recovery(
        validate_adoption_boundary(&challenge, &identity, prepared_at),
        &checkout_dir,
        &challenge_dir,
        &identity,
        &challenge_digest,
    )?;
    let (prepared_receipt, _) = transition_result_after_recovery(
        build_transition(&prepared_snapshot, prepared_at),
        &checkout_dir,
        &challenge_dir,
        &identity,
        &challenge_digest,
    )?;
    if let Err(error) = write_json_atomic(&receipt_path, &prepared_receipt, false) {
        return Err(transition_failure_after_recovery(
            error,
            &checkout_dir,
            &challenge_dir,
            &identity,
            &challenge_digest,
        ));
    }

    let snapshot = transition_result_after_recovery(
        snapshotter(&identity.root),
        &checkout_dir,
        &challenge_dir,
        &identity,
        &challenge_digest,
    )?;
    transition_result_after_recovery(
        validate_snapshot_matches_challenge(&snapshot, &challenge),
        &checkout_dir,
        &challenge_dir,
        &identity,
        &challenge_digest,
    )?;
    let transition_at = transition_result_after_recovery(
        unix_time(),
        &checkout_dir,
        &challenge_dir,
        &identity,
        &challenge_digest,
    )?;
    transition_result_after_recovery(
        validate_adoption_boundary(&challenge, &identity, transition_at),
        &checkout_dir,
        &challenge_dir,
        &identity,
        &challenge_digest,
    )?;
    let (receipt_record, lease) = transition_result_after_recovery(
        build_transition(&snapshot, transition_at),
        &checkout_dir,
        &challenge_dir,
        &identity,
        &challenge_digest,
    )?;
    if let Err(error) = write_json_atomic(&receipt_path, &receipt_record, true) {
        return Err(transition_failure_after_recovery(
            error,
            &checkout_dir,
            &challenge_dir,
            &identity,
            &challenge_digest,
        ));
    }
    if let Err(error) =
        validate_adoption_precommit_with(&challenge, &identity, || Ok(()), unix_time)
    {
        return Err(transition_failure_after_recovery(
            error,
            &checkout_dir,
            &challenge_dir,
            &identity,
            &challenge_digest,
        ));
    }
    if let Err(error) = fs::rename(&challenge_path, &spent_challenge_path)
        .context("failed to consume dirty-checkout challenge")
    {
        return Err(transition_failure_after_recovery(
            error,
            &checkout_dir,
            &challenge_dir,
            &identity,
            &challenge_digest,
        ));
    }
    if let Err(error) = sync_directory(&challenge_dir).and_then(|()| sync_directory(&receipts_dir))
    {
        return Err(transition_failure_after_recovery(
            error,
            &checkout_dir,
            &challenge_dir,
            &identity,
            &challenge_digest,
        ));
    }

    match install_lease(&lease_path, &lease, existing.as_ref(), &identity) {
        LeaseInstallOutcome::Installed => {
            let post_install = snapshotter(&identity.root).and_then(|post_snapshot| {
                validate_snapshot_matches_challenge(&post_snapshot, &challenge)
            });
            if let Err(error) = post_install {
                if let Err(rollback_error) = rollback_installed_adoption(
                    &checkout_dir,
                    &challenge_dir,
                    &identity,
                    &challenge_digest,
                    &receipt_id,
                ) {
                    return Err(domain_error(
                        DirtyCheckoutErrorKind::AdoptionFailed,
                        format!(
                            "post-install snapshot verification failed and rollback is incomplete: {error}; rollback failed: {rollback_error}"
                        ),
                    ));
                }
                return Err(error);
            }
            if recover_pending_adoption(
                &checkout_dir,
                &challenge_dir,
                &identity,
                &challenge_digest,
            )? != PendingRecovery::Committed
            {
                return Err(domain_error(
                    DirtyCheckoutErrorKind::AdoptionFailed,
                    "installed adoption did not enter a committed recovery state",
                ));
            }
        }
        LeaseInstallOutcome::NotInstalled(error) => {
            return Err(transition_failure_after_recovery(
                error,
                &checkout_dir,
                &challenge_dir,
                &identity,
                &challenge_digest,
            ));
        }
        LeaseInstallOutcome::Ambiguous(error) => {
            return Err(domain_error(
                DirtyCheckoutErrorKind::AdoptionFailed,
                format!(
                    "checkout lease installation is ambiguous; recovery state was retained: {error}"
                ),
            ));
        }
    }

    Ok(AdoptionReceipt {
        receipt_id,
        snapshot_id: snapshot.snapshot_id,
    })
}

fn validate_snapshot_matches_challenge(
    snapshot: &DirtySnapshot,
    challenge: &ChallengeRecord,
) -> Result<()> {
    if snapshot.repository_key != challenge.repository_key
        || snapshot.checkout_key != challenge.checkout_key
        || snapshot.checkout_instance != challenge.checkout_instance
        || snapshot.snapshot_id != challenge.snapshot_id
        || snapshot.head_oid != challenge.head_oid
        || snapshot.branch_ref_digest != challenge.branch_ref_digest
    {
        return Err(domain_error(
            DirtyCheckoutErrorKind::ChallengeDrift,
            "dirty-checkout challenge no longer matches the current checkout snapshot",
        ));
    }
    Ok(())
}

fn rollback_installed_adoption(
    checkout_dir: &Path,
    challenge_dir: &Path,
    identity: &CheckoutIdentity,
    challenge_digest: &str,
    receipt_id: &str,
) -> Result<()> {
    let lease_path = checkout_dir.join("lease.json");
    let revoked_lease_path = checkout_dir.join(format!(".revoked-{receipt_id}.json"));
    fs::rename(&lease_path, &revoked_lease_path)
        .context("post-install adoption rollback could not revoke the lease")?;
    sync_directory(checkout_dir)
        .context("post-install adoption rollback tombstone could not be made durable")?;
    if recover_pending_adoption(checkout_dir, challenge_dir, identity, challenge_digest)?
        != PendingRecovery::Revoked
    {
        return Err(domain_error(
            DirtyCheckoutErrorKind::AdoptionFailed,
            "post-install adoption rollback did not enter revoked recovery state",
        ));
    }
    Ok(())
}

#[derive(Debug)]
struct ExpiredPredecessor {
    receipt_id: String,
    receipt_path: PathBuf,
    spent_challenge_path: PathBuf,
    receipt_digest: String,
    spent_challenge_digest: String,
}

fn expired_predecessor(
    checkout_dir: &Path,
    identity: &CheckoutIdentity,
    lease: &LeaseRecord,
) -> Result<Option<ExpiredPredecessor>> {
    let Some(adoption) = lease.adoption() else {
        return Ok(None);
    };
    let receipts_dir = checkout_dir.join("receipts");
    let receipt_path = receipts_dir.join(format!("{}.json", adoption.receipt_id));
    let spent_challenge_path =
        receipts_dir.join(format!(".challenge-{}.json", adoption.receipt_id));
    let receipt_bytes =
        read_private_regular(&receipt_path, MAX_STATE_FILE_BYTES, true).map_err(|error| {
            command_error(
                error,
                DirtyCheckoutErrorKind::MalformedState,
                "expired adoption predecessor receipt is unavailable",
            )
        })?;
    let receipt: ReceiptRecord = serde_json::from_slice(&receipt_bytes).map_err(|_| {
        domain_error(
            DirtyCheckoutErrorKind::MalformedState,
            "expired adoption predecessor receipt is malformed",
        )
    })?;
    validate_receipt(&receipt, identity, &adoption.receipt_id)?;
    if adoption.receipt_schema != receipt.schema
        || lease.session_key() != receipt.session_key
        || lease.checkout_instance() != receipt.checkout_instance
        || adoption.snapshot_id != receipt.snapshot_id
        || adoption.authorization_turn_digest != receipt.authorization_turn_digest
        || adoption.reason_digest != receipt.reason_digest
        || adoption.adopted_at != receipt.adopted_at
        || adoption.challenge_digest != receipt.challenge_digest
    {
        return Err(domain_error(
            DirtyCheckoutErrorKind::MalformedState,
            "expired adoption predecessor receipt identity is inconsistent",
        ));
    }
    let spent_bytes = read_private_regular(&spent_challenge_path, MAX_STATE_FILE_BYTES, true)
        .map_err(|error| {
            command_error(
                error,
                DirtyCheckoutErrorKind::MalformedState,
                "expired adoption predecessor challenge is unavailable",
            )
        })?;
    let spent: ChallengeRecord = serde_json::from_slice(&spent_bytes).map_err(|_| {
        domain_error(
            DirtyCheckoutErrorKind::MalformedState,
            "expired adoption predecessor challenge is malformed",
        )
    })?;
    validate_challenge_identity(&spent, identity, &adoption.challenge_digest)?;
    if spent.issued_at != adoption.challenge_issued_at || spent.snapshot_id != adoption.snapshot_id
    {
        return Err(domain_error(
            DirtyCheckoutErrorKind::MalformedState,
            "expired adoption predecessor challenge identity is inconsistent",
        ));
    }
    Ok(Some(ExpiredPredecessor {
        receipt_id: adoption.receipt_id.clone(),
        receipt_path,
        spent_challenge_path,
        receipt_digest: sha256_hex(&receipt_bytes),
        spent_challenge_digest: sha256_hex(&spent_bytes),
    }))
}

fn load_bound_predecessor(
    receipts_dir: &Path,
    receipt_id: &str,
    expected_receipt_digest: &str,
    expected_spent_challenge_digest: &str,
) -> Result<ExpiredPredecessor> {
    let receipt_path = receipts_dir.join(format!("{receipt_id}.json"));
    let spent_challenge_path = receipts_dir.join(format!(".challenge-{receipt_id}.json"));
    let receipt_bytes =
        read_private_regular(&receipt_path, MAX_STATE_FILE_BYTES, true).map_err(|error| {
            command_error(
                error,
                DirtyCheckoutErrorKind::MalformedState,
                "bound predecessor receipt is unavailable",
            )
        })?;
    let spent_bytes = read_private_regular(&spent_challenge_path, MAX_STATE_FILE_BYTES, true)
        .map_err(|error| {
            command_error(
                error,
                DirtyCheckoutErrorKind::MalformedState,
                "bound predecessor challenge is unavailable",
            )
        })?;
    let receipt_digest = sha256_hex(&receipt_bytes);
    let spent_challenge_digest = sha256_hex(&spent_bytes);
    if receipt_digest != expected_receipt_digest
        || spent_challenge_digest != expected_spent_challenge_digest
    {
        return Err(domain_error(
            DirtyCheckoutErrorKind::MalformedState,
            "bound predecessor artifacts do not match pending adoption state",
        ));
    }
    Ok(ExpiredPredecessor {
        receipt_id: receipt_id.to_string(),
        receipt_path,
        spent_challenge_path,
        receipt_digest,
        spent_challenge_digest,
    })
}

fn cleanup_expired_predecessor(predecessor: &ExpiredPredecessor) -> Result<()> {
    for path in [&predecessor.receipt_path, &predecessor.spent_challenge_path] {
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).context("expired adoption predecessor cleanup failed");
            }
        }
    }
    if let Some(parent) = predecessor.receipt_path.parent() {
        sync_directory(parent)?;
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingRecovery {
    None,
    RolledBack,
    Committed,
    Revoked,
}

fn pending_adoption_path(checkout_dir: &Path, challenge_digest: &str) -> PathBuf {
    checkout_dir.join(format!(".pending-adoption-{challenge_digest}.json"))
}

fn recover_pending_adoption(
    checkout_dir: &Path,
    challenge_dir: &Path,
    identity: &CheckoutIdentity,
    challenge_digest: &str,
) -> Result<PendingRecovery> {
    let pending_path = pending_adoption_path(checkout_dir, challenge_digest);
    let Some(pending_bytes) = read_optional_private(&pending_path, "pending adoption")? else {
        return Ok(PendingRecovery::None);
    };
    let pending: PendingAdoptionRecord = serde_json::from_slice(&pending_bytes).map_err(|_| {
        domain_error(
            DirtyCheckoutErrorKind::MalformedState,
            "pending dirty-checkout adoption state is malformed",
        )
    })?;
    validate_pending_adoption(&pending, identity, challenge_digest)?;

    let receipts_dir = checkout_dir.join("receipts");
    verify_private_directory(&receipts_dir).map_err(|error| {
        command_error(
            error,
            DirtyCheckoutErrorKind::MalformedState,
            "pending adoption receipt directory is untrusted",
        )
    })?;
    let receipt_path = receipts_dir.join(format!("{}.json", pending.receipt_id));
    let spent_challenge_path = receipts_dir.join(format!(".challenge-{}.json", pending.receipt_id));
    let challenge_path = challenge_dir.join(format!("{challenge_digest}.json"));
    let revoked_lease_path = checkout_dir.join(format!(".revoked-{}.json", pending.receipt_id));

    if let Some(revoked_bytes) = read_optional_private(&revoked_lease_path, "revoked adoption")? {
        let revoked = parse_lease(&revoked_bytes)?;
        validate_lease(&revoked, identity)?;
        let adoption = revoked.adoption().ok_or_else(|| {
            domain_error(
                DirtyCheckoutErrorKind::MalformedState,
                "revoked adoption tombstone has no adoption identity",
            )
        })?;
        if revoked.session_key() != pending.session_key
            || revoked.checkout_instance() != pending.checkout_instance
            || adoption.receipt_id != pending.receipt_id
            || adoption.snapshot_id != pending.snapshot_id
            || adoption.challenge_digest != pending.challenge_digest
        {
            return Err(domain_error(
                DirtyCheckoutErrorKind::MalformedState,
                "revoked adoption tombstone does not match pending state",
            ));
        }
        if let Some(challenge) =
            load_pending_challenge(&challenge_path, identity, challenge_digest)?
            && challenge.token_digest != pending.challenge_digest
        {
            return Err(domain_error(
                DirtyCheckoutErrorKind::MalformedState,
                "revoked adoption has an inconsistent live challenge",
            ));
        }
        if let Some(spent) =
            load_pending_challenge(&spent_challenge_path, identity, challenge_digest)?
            && (spent.token_digest != pending.challenge_digest
                || spent.snapshot_id != pending.snapshot_id)
        {
            return Err(domain_error(
                DirtyCheckoutErrorKind::MalformedState,
                "revoked adoption has an inconsistent spent challenge",
            ));
        }
        if let Some(receipt_bytes) =
            read_optional_private(&receipt_path, "revoked adoption receipt")?
        {
            let receipt: ReceiptRecord = serde_json::from_slice(&receipt_bytes).map_err(|_| {
                domain_error(
                    DirtyCheckoutErrorKind::MalformedState,
                    "revoked adoption receipt state is malformed",
                )
            })?;
            validate_receipt(&receipt, identity, &pending.receipt_id)?;
            if receipt.session_key != pending.session_key
                || receipt.snapshot_id != pending.snapshot_id
                || receipt.challenge_digest != pending.challenge_digest
            {
                return Err(domain_error(
                    DirtyCheckoutErrorKind::MalformedState,
                    "revoked adoption receipt does not match pending state",
                ));
            }
        }
        for path in [&challenge_path, &spent_challenge_path, &receipt_path] {
            match fs::remove_file(path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(error).context("revoked adoption recovery cleanup failed");
                }
            }
        }
        sync_directory(challenge_dir)?;
        sync_directory(&receipts_dir)?;
        fs::remove_file(&pending_path)
            .context("revoked adoption recovery marker cleanup failed")?;
        sync_directory(checkout_dir)?;
        return Ok(PendingRecovery::Revoked);
    }

    let lease = load_lease(&checkout_dir.join("lease.json"))?;
    if let Some(lease) = &lease {
        validate_lease(lease, identity)?;
    }
    let committed = lease.as_ref().is_some_and(|lease| {
        lease.session_key() == pending.session_key
            && lease.checkout_instance() == pending.checkout_instance
            && lease.adoption().is_some_and(|adoption| {
                adoption.receipt_id == pending.receipt_id
                    && adoption.snapshot_id == pending.snapshot_id
                    && adoption.challenge_digest == pending.challenge_digest
            })
    });
    if committed {
        let lease = lease.expect("checked committed lease");
        validate_lease(&lease, identity)?;
        let artifacts = expired_predecessor(checkout_dir, identity, &lease)?.ok_or_else(|| {
            domain_error(
                DirtyCheckoutErrorKind::MalformedState,
                "committed pending adoption artifacts are incomplete or malformed",
            )
        })?;
        if artifacts.receipt_id != pending.receipt_id {
            return Err(domain_error(
                DirtyCheckoutErrorKind::MalformedState,
                "committed pending adoption receipt identity is inconsistent",
            ));
        }
        if let (
            Some(predecessor_receipt_id),
            Some(predecessor_receipt_digest),
            Some(predecessor_spent_challenge_digest),
        ) = (
            &pending.predecessor_receipt_id,
            &pending.predecessor_receipt_digest,
            &pending.predecessor_spent_challenge_digest,
        ) {
            let predecessor = load_bound_predecessor(
                &receipts_dir,
                predecessor_receipt_id,
                predecessor_receipt_digest,
                predecessor_spent_challenge_digest,
            )?;
            cleanup_expired_predecessor(&predecessor)?;
        }
        fs::remove_file(&pending_path)
            .context("committed adoption recovery marker cleanup failed")?;
        sync_directory(checkout_dir)?;
        return Ok(PendingRecovery::Committed);
    }

    let current_challenge = load_pending_challenge(&challenge_path, identity, challenge_digest)?;
    let spent_challenge =
        load_pending_challenge(&spent_challenge_path, identity, challenge_digest)?;
    match (current_challenge.is_some(), spent_challenge.is_some()) {
        (true, false) => {}
        (false, true) => fs::rename(&spent_challenge_path, &challenge_path)
            .context("pending adoption challenge rollback failed")?,
        (true, true) => {
            return Err(domain_error(
                DirtyCheckoutErrorKind::MalformedState,
                "pending adoption has duplicate current and spent challenges",
            ));
        }
        (false, false) => {
            return Err(domain_error(
                DirtyCheckoutErrorKind::MalformedState,
                "pending adoption challenge state is missing",
            ));
        }
    }

    if let Some(receipt_bytes) = read_optional_private(&receipt_path, "pending adoption receipt")? {
        let receipt: ReceiptRecord = serde_json::from_slice(&receipt_bytes).map_err(|_| {
            domain_error(
                DirtyCheckoutErrorKind::MalformedState,
                "pending adoption receipt state is malformed",
            )
        })?;
        validate_receipt(&receipt, identity, &pending.receipt_id)?;
        if receipt.session_key != pending.session_key
            || receipt.snapshot_id != pending.snapshot_id
            || receipt.challenge_digest != pending.challenge_digest
        {
            return Err(domain_error(
                DirtyCheckoutErrorKind::MalformedState,
                "pending adoption receipt does not match its recovery marker",
            ));
        }
        fs::remove_file(&receipt_path).context("pending adoption receipt rollback failed")?;
    }
    sync_directory(challenge_dir)?;
    sync_directory(&receipts_dir)?;
    fs::remove_file(&pending_path).context("pending adoption recovery marker cleanup failed")?;
    sync_directory(checkout_dir)?;
    Ok(PendingRecovery::RolledBack)
}

fn read_optional_private(path: &Path, label: &str) -> Result<Option<Vec<u8>>> {
    match read_private_regular(path, MAX_STATE_FILE_BYTES, true) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error)
            if error
                .downcast_ref::<io::Error>()
                .is_some_and(|error| error.kind() == io::ErrorKind::NotFound) =>
        {
            Ok(None)
        }
        Err(error) => Err(command_error(
            error,
            DirtyCheckoutErrorKind::MalformedState,
            format!("{label} state is untrusted"),
        )),
    }
}

fn load_pending_challenge(
    path: &Path,
    identity: &CheckoutIdentity,
    challenge_digest: &str,
) -> Result<Option<ChallengeRecord>> {
    let Some(bytes) = read_optional_private(path, "pending adoption challenge")? else {
        return Ok(None);
    };
    let challenge: ChallengeRecord = serde_json::from_slice(&bytes).map_err(|_| {
        domain_error(
            DirtyCheckoutErrorKind::MalformedState,
            "pending adoption challenge state is malformed",
        )
    })?;
    validate_challenge_identity(&challenge, identity, challenge_digest)?;
    Ok(Some(challenge))
}

fn validate_pending_adoption(
    pending: &PendingAdoptionRecord,
    identity: &CheckoutIdentity,
    challenge_digest: &str,
) -> Result<()> {
    let valid = pending.schema == PENDING_ADOPTION_SCHEMA
        && is_lower_hex(&pending.receipt_id, 64)
        && is_lower_hex(&pending.challenge_digest, 64)
        && is_lower_hex(&pending.session_key, 64)
        && is_lower_hex(&pending.checkout_instance, 32)
        && is_lower_hex(&pending.snapshot_id, 64)
        && pending.challenge_digest == challenge_digest
        && pending.checkout_instance == identity.checkout_instance
        && match (
            &pending.predecessor_receipt_id,
            &pending.predecessor_receipt_digest,
            &pending.predecessor_spent_challenge_digest,
        ) {
            (None, None, None) => true,
            (Some(receipt_id), Some(receipt_digest), Some(spent_digest)) => {
                is_lower_hex(receipt_id, 64)
                    && receipt_id != &pending.receipt_id
                    && is_lower_hex(receipt_digest, 64)
                    && is_lower_hex(spent_digest, 64)
            }
            _ => false,
        };
    if !valid {
        return Err(domain_error(
            DirtyCheckoutErrorKind::MalformedState,
            "pending dirty-checkout adoption state is malformed",
        ));
    }
    Ok(())
}

fn transition_result_after_recovery<T>(
    result: Result<T>,
    checkout_dir: &Path,
    challenge_dir: &Path,
    identity: &CheckoutIdentity,
    challenge_digest: &str,
) -> Result<T> {
    result.map_err(|error| {
        transition_failure_after_recovery(
            error,
            checkout_dir,
            challenge_dir,
            identity,
            challenge_digest,
        )
    })
}

fn transition_failure_after_recovery(
    error: anyhow::Error,
    checkout_dir: &Path,
    challenge_dir: &Path,
    identity: &CheckoutIdentity,
    challenge_digest: &str,
) -> anyhow::Error {
    match recover_pending_adoption(checkout_dir, challenge_dir, identity, challenge_digest) {
        Ok(PendingRecovery::None | PendingRecovery::RolledBack) => error,
        Ok(PendingRecovery::Committed | PendingRecovery::Revoked) => domain_error(
            DirtyCheckoutErrorKind::AdoptionFailed,
            format!("adoption transition became committed while handling a failure: {error}"),
        ),
        Err(recovery_error) => domain_error(
            DirtyCheckoutErrorKind::AdoptionFailed,
            format!(
                "adoption transition failed and requires recovery: {error}; recovery failed: {recovery_error}"
            ),
        ),
    }
}
enum LeaseInstallOutcome {
    Installed,
    NotInstalled(anyhow::Error),
    Ambiguous(anyhow::Error),
}

fn install_lease(
    path: &Path,
    expected: &LeaseRecord,
    previous: Option<&LeaseRecord>,
    identity: &CheckoutIdentity,
) -> LeaseInstallOutcome {
    install_lease_with(path, expected, previous, identity, || {
        write_json_atomic(path, expected, true)
    })
}

fn install_lease_with<F>(
    path: &Path,
    expected: &LeaseRecord,
    previous: Option<&LeaseRecord>,
    identity: &CheckoutIdentity,
    writer: F,
) -> LeaseInstallOutcome
where
    F: FnOnce() -> Result<()>,
{
    let Err(error) = writer() else {
        return LeaseInstallOutcome::Installed;
    };
    match load_lease(path) {
        Ok(Some(actual)) if validate_lease(&actual, identity).is_ok() && actual == *expected => {
            LeaseInstallOutcome::Ambiguous(error)
        }
        Ok(actual) if actual.as_ref() == previous => LeaseInstallOutcome::NotInstalled(error),
        _ => LeaseInstallOutcome::Ambiguous(error),
    }
}

pub fn revoke_dirty(checkout: &Path, state_root: &Path, receipt_id: &str) -> Result<()> {
    revoke_dirty_inner(checkout, state_root, receipt_id).map_err(|error| {
        command_error(
            error,
            DirtyCheckoutErrorKind::RevocationFailed,
            "dirty checkout revocation failed",
        )
    })
}

fn revoke_dirty_inner(checkout: &Path, state_root: &Path, receipt_id: &str) -> Result<()> {
    if !is_lower_hex(receipt_id, 64) {
        return Err(domain_error(
            DirtyCheckoutErrorKind::InvalidInput,
            "receipt ID is malformed",
        ));
    }
    let identity = resolve_checkout(checkout, false, Instant::now() + GIT_TIMEOUT)?;
    let checkout_dir = checkout_state_dir(state_root, &identity)?;
    let receipts_dir = checkout_dir.join("receipts");
    verify_private_directory(&receipts_dir).map_err(|error| {
        command_error(
            error,
            DirtyCheckoutErrorKind::MalformedState,
            "dirty-checkout receipt directory is untrusted",
        )
    })?;
    let _lock = LeaseLock::acquire(&checkout_dir)?;
    let receipt_path = receipts_dir.join(format!("{receipt_id}.json"));
    let receipt_bytes =
        read_private_regular(&receipt_path, MAX_STATE_FILE_BYTES, true).map_err(|error| {
            command_error(
                error,
                DirtyCheckoutErrorKind::MalformedState,
                "dirty-checkout receipt state is untrusted",
            )
        })?;
    let receipt: ReceiptRecord = serde_json::from_slice(&receipt_bytes).map_err(|_| {
        domain_error(
            DirtyCheckoutErrorKind::MalformedState,
            "dirty-checkout receipt state is malformed",
        )
    })?;
    validate_receipt(&receipt, &identity, receipt_id)?;

    let lease_path = checkout_dir.join("lease.json");
    let lease = load_lease(&lease_path)?.ok_or_else(|| {
        domain_error(
            DirtyCheckoutErrorKind::MalformedState,
            "dirty-checkout lease is missing",
        )
    })?;
    validate_lease(&lease, &identity)?;
    let adoption = lease.adoption().ok_or_else(|| {
        domain_error(
            DirtyCheckoutErrorKind::MalformedState,
            "checkout lease has no dirty adoption",
        )
    })?;
    if lease.schema() != LEASE_V2_SCHEMA
        || lease.session_key() != receipt.session_key
        || lease.checkout_instance() != receipt.checkout_instance
        || adoption.schema != ADOPTION_SCHEMA
        || adoption.receipt_schema != receipt.schema
        || adoption.receipt_id != receipt.receipt_id
        || adoption.snapshot_id != receipt.snapshot_id
        || adoption.authorization_turn_digest != receipt.authorization_turn_digest
        || adoption.reason_digest != receipt.reason_digest
        || adoption.adopted_at != receipt.adopted_at
        || adoption.challenge_digest != receipt.challenge_digest
    {
        return Err(domain_error(
            DirtyCheckoutErrorKind::MalformedState,
            "receipt does not match the active dirty-checkout adoption",
        ));
    }

    let challenge_dir = checkout_dir.join("challenges");
    match recover_pending_adoption(
        &checkout_dir,
        &challenge_dir,
        &identity,
        &receipt.challenge_digest,
    )? {
        PendingRecovery::None | PendingRecovery::Committed => {}
        PendingRecovery::RolledBack | PendingRecovery::Revoked => {
            return Err(domain_error(
                DirtyCheckoutErrorKind::MalformedState,
                "active adoption changed while revocation was prepared",
            ));
        }
    }

    let revoked_lease_path = checkout_dir.join(format!(".revoked-{receipt_id}.json"));
    fs::rename(&lease_path, &revoked_lease_path)
        .context("failed to revoke adopted checkout lease")?;
    sync_directory(&checkout_dir).context("revoked lease tombstone could not be made durable")?;

    let challenge_path = challenge_dir.join(format!("{}.json", receipt.challenge_digest));
    let spent_challenge_path = receipts_dir.join(format!(".challenge-{receipt_id}.json"));
    for path in [&receipt_path, &challenge_path, &spent_challenge_path] {
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).context("revoked adoption artifact cleanup failed");
            }
        }
    }
    sync_directory(&receipts_dir)
        .context("revoked adoption receipt cleanup could not be made durable")?;
    sync_directory(&challenge_dir)
        .context("revoked adoption challenge cleanup could not be made durable")?;
    sync_directory(&checkout_dir)
        .context("revoked adoption tombstone could not be confirmed durable")?;
    Ok(())
}

fn snapshot_once(checkout: &Path, deadline: Instant) -> Result<SnapshotPass> {
    ensure_deadline(deadline)?;
    let identity = resolve_checkout(checkout, true, deadline)?;
    reject_command_bearing_git_config(&identity.root, deadline)?;
    reject_active_git_operation(&identity)?;
    let mut budget = SnapshotBudget::default();

    let branch_output = snapshot_git_output(
        &mut budget,
        &identity.root,
        &["symbolic-ref", "--quiet", "HEAD"],
        4096,
        deadline,
    )?;
    let branch_raw = if branch_output.status.success() {
        strip_git_line(&branch_output.stdout)?.to_vec()
    } else if branch_output.status.code() == Some(1) {
        Vec::new()
    } else {
        return Err(domain_error(
            DirtyCheckoutErrorKind::UnsupportedGitState,
            "Git branch identity is unavailable",
        ));
    };

    let head_output = snapshot_git_output(
        &mut budget,
        &identity.root,
        &["rev-parse", "--verify", "HEAD^{commit}"],
        256,
        deadline,
    )?;
    let head_oid = if head_output.status.success() {
        let head_raw = strip_git_line(&head_output.stdout)?;
        if !(40..=64).contains(&head_raw.len()) || !head_raw.iter().all(u8::is_ascii_hexdigit) {
            return Err(domain_error(
                DirtyCheckoutErrorKind::UnsupportedGitState,
                "Git HEAD identity is malformed",
            ));
        }
        String::from_utf8(head_raw.to_vec()).map_err(|_| {
            domain_error(
                DirtyCheckoutErrorKind::UnsupportedGitState,
                "Git HEAD identity is malformed",
            )
        })?
    } else if !branch_raw.is_empty() {
        validate_unborn_head(&mut budget, &identity.root, &branch_raw, deadline)?
    } else {
        return Err(domain_error(
            DirtyCheckoutErrorKind::UnsupportedGitState,
            "Git HEAD identity is unavailable",
        ));
    };

    let index_output = snapshot_git_output(
        &mut budget,
        &identity.root,
        &["ls-files", "--stage", "-z"],
        MAX_GIT_OUTPUT_BYTES,
        deadline,
    )?;
    if !index_output.status.success() {
        return Err(domain_error(
            DirtyCheckoutErrorKind::UnsupportedGitState,
            "Git index listing failed",
        ));
    }
    let mut tracked = parse_index_bounded(&index_output.stdout, MAX_ENTRY_COUNT, &mut budget)?;
    drop(index_output);

    let flags_output = snapshot_git_output(
        &mut budget,
        &identity.root,
        &["ls-files", "-v", "-z"],
        MAX_GIT_OUTPUT_BYTES,
        deadline,
    )?;
    if !flags_output.status.success() {
        return Err(domain_error(
            DirtyCheckoutErrorKind::UnsupportedGitState,
            "Git index flags are unavailable",
        ));
    }
    validate_index_flags(&flags_output.stdout, &tracked, &mut budget)?;
    drop(flags_output);

    let staged_output = snapshot_git_output(
        &mut budget,
        &identity.root,
        &[
            "diff",
            "--cached",
            "--quiet",
            "--no-ext-diff",
            "--ignore-submodules=none",
            "--",
        ],
        1,
        deadline,
    )?;
    let staged_dirty = match staged_output.status.code() {
        Some(0) => false,
        Some(1) => true,
        _ => {
            return Err(domain_error(
                DirtyCheckoutErrorKind::UnsupportedGitState,
                "Git staged-state probe failed",
            ));
        }
    };
    drop(staged_output);

    let unstaged_output = snapshot_git_output(
        &mut budget,
        &identity.root,
        &[
            "diff",
            "--name-only",
            "-z",
            "--no-ext-diff",
            "--ignore-submodules=none",
            "--",
        ],
        MAX_GIT_OUTPUT_BYTES,
        deadline,
    )?;
    if !unstaged_output.status.success() {
        return Err(domain_error(
            DirtyCheckoutErrorKind::UnsupportedGitState,
            "Git unstaged-path listing failed",
        ));
    }
    let mut unstaged =
        parse_nul_paths_bounded(&unstaged_output.stdout, MAX_ENTRY_COUNT, &mut budget)?;
    drop(unstaged_output);

    let untracked_output = snapshot_git_output(
        &mut budget,
        &identity.root,
        &["ls-files", "--others", "--exclude-standard", "-z"],
        MAX_GIT_OUTPUT_BYTES,
        deadline,
    )?;
    if !untracked_output.status.success() {
        return Err(domain_error(
            DirtyCheckoutErrorKind::UnsupportedGitState,
            "Git untracked listing failed",
        ));
    }
    let remaining_entries = MAX_ENTRY_COUNT.saturating_sub(tracked.len());
    let mut untracked =
        parse_nul_paths_bounded(&untracked_output.stdout, remaining_entries, &mut budget)?;
    drop(untracked_output);

    if !staged_dirty && unstaged.is_empty() && untracked.is_empty() {
        return Err(domain_error(
            DirtyCheckoutErrorKind::CleanCheckout,
            "checkout is clean",
        ));
    }

    tracked.sort_by(|left, right| left.path.cmp(&right.path));
    unstaged.sort();
    untracked.sort();
    ensure_unique_paths(tracked.iter().map(|entry| entry.path.as_slice()))?;
    ensure_unique_paths(unstaged.iter().map(Vec::as_slice))?;
    ensure_unique_paths(untracked.iter().map(Vec::as_slice))?;
    let submodule_paths: HashSet<&[u8]> = tracked
        .iter()
        .filter(|entry| entry.mode.as_slice() == b"160000")
        .map(|entry| entry.path.as_slice())
        .collect();
    reject_special_filesystem_objects(&identity.root, &submodule_paths, &mut budget, deadline)
        .map_err(|error| {
            command_error(
                error,
                DirtyCheckoutErrorKind::UnsupportedGitState,
                "checkout contains unsupported filesystem state",
            )
        })?;
    let tracked_paths: HashSet<&[u8]> = tracked.iter().map(|entry| entry.path.as_slice()).collect();
    if !unstaged
        .iter()
        .all(|path| tracked_paths.contains(path.as_slice()))
    {
        return Err(domain_error(
            DirtyCheckoutErrorKind::UnsupportedGitState,
            "Git unstaged-path listing does not match the index",
        ));
    }
    let unstaged_count = unstaged.len();
    let unstaged_paths: HashSet<Vec<u8>> = unstaged.into_iter().collect();

    let mut hasher = FramedHasher::new();
    hasher.field(b"schema", SNAPSHOT_SCHEMA.as_bytes());
    hasher.field(b"repository_key", identity.repository_key.as_bytes());
    hasher.field(b"checkout_key", identity.checkout_key.as_bytes());
    hasher.field(b"checkout_instance", identity.checkout_instance.as_bytes());
    hasher.field(b"head_oid", head_oid.as_bytes());
    hasher.field(b"branch_ref", &branch_raw);

    for entry in &tracked {
        ensure_deadline(deadline)?;
        hasher.field(b"index_mode", &entry.mode);
        hasher.field(b"index_oid", &entry.oid);
        hasher.field(b"index_path", &entry.path);
        let is_submodule = entry.mode.as_slice() == b"160000";
        if is_submodule || unstaged_paths.contains(entry.path.as_slice()) {
            if !is_submodule {
                hasher.field(b"unstaged_path", &entry.path);
            }
            hash_worktree_object(
                &mut hasher,
                &identity.root,
                &entry.path,
                is_submodule,
                Some(&entry.oid),
                &mut budget,
                deadline,
            )?;
        }
    }
    for path in &untracked {
        ensure_deadline(deadline)?;
        hasher.field(b"untracked_path", path);
        hash_worktree_object(
            &mut hasher,
            &identity.root,
            path,
            false,
            None,
            &mut budget,
            deadline,
        )?;
    }
    hasher.field(b"tracked_count", &(tracked.len() as u64).to_be_bytes());
    hasher.field(b"unstaged_count", &(unstaged_count as u64).to_be_bytes());
    hasher.field(b"untracked_count", &(untracked.len() as u64).to_be_bytes());
    hasher.field(b"hashed_bytes", &budget.file_bytes.to_be_bytes());

    Ok(SnapshotPass {
        snapshot: DirtySnapshot {
            schema: SNAPSHOT_SCHEMA,
            repository_key: identity.repository_key,
            checkout_key: identity.checkout_key,
            checkout_instance: identity.checkout_instance,
            snapshot_id: hasher.finish(),
            head_oid,
            branch_ref_digest: sha256_hex(&branch_raw),
            tracked_entries: tracked.len(),
            untracked_entries: untracked.len(),
            hashed_bytes: budget.file_bytes,
        },
    })
}

fn reject_command_bearing_git_config(cwd: &Path, deadline: Instant) -> Result<()> {
    const COMMAND_BEARING_CONFIG: &str = r"^(include\.path|includeIf\..*\.path|filter\..*\.(clean|smudge|process)|diff\..*\.(command|textconv))$";
    let output = git_output(
        cwd,
        &[
            "config",
            "--local",
            "--no-includes",
            "--name-only",
            "--null",
            "--get-regexp",
            COMMAND_BEARING_CONFIG,
        ],
        1024 * 1024,
        deadline,
    )?;
    match output.status.code() {
        Some(1) if output.stdout.is_empty() => Ok(()),
        Some(0) => Err(domain_error(
            DirtyCheckoutErrorKind::UnsupportedGitState,
            "repository command-bearing Git configuration is unsupported",
        )),
        _ => Err(domain_error(
            DirtyCheckoutErrorKind::UnsupportedGitState,
            "repository Git configuration could not be validated",
        )),
    }
}

fn snapshot_git_output(
    budget: &mut SnapshotBudget,
    cwd: &Path,
    args: &[&str],
    command_limit: usize,
    deadline: Instant,
) -> Result<std::process::Output> {
    let limit = budget.output_limit(command_limit)?;
    let output = git_output(cwd, args, limit, deadline)?;
    budget.charge_output(&output)?;
    Ok(output)
}

fn validate_unborn_head(
    budget: &mut SnapshotBudget,
    cwd: &Path,
    branch_ref: &[u8],
    deadline: Instant,
) -> Result<String> {
    if !branch_ref.starts_with(b"refs/heads/") {
        return Err(domain_error(
            DirtyCheckoutErrorKind::UnsupportedGitState,
            "symbolic HEAD does not identify a local branch",
        ));
    }
    let args = [
        OsString::from("show-ref"),
        OsString::from("--verify"),
        OsString::from("--quiet"),
        OsString::from_vec(branch_ref.to_vec()),
    ];
    let limit = budget.output_limit(1024)?;
    let output = git_output_os(cwd, &args, limit, deadline)?;
    budget.charge_output(&output)?;
    if output.status.code() == Some(1) {
        return Ok(unborn_head_identity(branch_ref));
    }
    Err(domain_error(
        DirtyCheckoutErrorKind::UnsupportedGitState,
        "symbolic HEAD is dangling, inaccessible, or does not identify a commit",
    ))
}

fn unborn_head_identity(branch_ref: &[u8]) -> String {
    let mut hasher = FramedHasher::new();
    hasher.field(b"domain", b"agent-runtime.git-head.unborn.v1");
    hasher.field(b"symbolic_ref", branch_ref);
    format!("{UNBORN_HEAD_PREFIX}{}", hasher.finish())
}

fn resolve_checkout(
    checkout: &Path,
    create_instance: bool,
    deadline: Instant,
) -> Result<CheckoutIdentity> {
    let requested =
        fs::canonicalize(checkout).context("checkout identity could not be resolved")?;
    let root_output = git_output_os(
        &requested,
        &[
            OsString::from("rev-parse"),
            OsString::from("--show-toplevel"),
        ],
        1024 * 1024,
        deadline,
    )?;
    ensure!(root_output.status.success(), "path is not a Git checkout");
    let root = canonical_git_path(&requested, strip_git_line(&root_output.stdout)?)?;
    let git_dir_output = git_output(
        &root,
        &["rev-parse", "--absolute-git-dir"],
        1024 * 1024,
        deadline,
    )?;
    let common_dir_output = git_output(
        &root,
        &["rev-parse", "--git-common-dir"],
        1024 * 1024,
        deadline,
    )?;
    ensure!(
        git_dir_output.status.success() && common_dir_output.status.success(),
        "Git checkout identity could not be resolved"
    );
    let git_dir = canonical_git_path(&root, strip_git_line(&git_dir_output.stdout)?)?;
    let common_dir = canonical_git_path(&root, strip_git_line(&common_dir_output.stdout)?)?;
    let repository_key = sha256_hex(common_dir.as_os_str().as_bytes());
    let checkout_key = sha256_hex(root.as_os_str().as_bytes());
    let checkout_instance = read_checkout_instance(&git_dir, create_instance)?;
    Ok(CheckoutIdentity {
        root,
        git_dir,
        common_dir,
        repository_key,
        checkout_key,
        checkout_instance,
    })
}

fn canonical_git_path(base: &Path, raw: &[u8]) -> Result<PathBuf> {
    ensure!(
        !raw.is_empty() && !raw.contains(&0),
        "Git path identity is malformed"
    );
    let path = PathBuf::from(OsString::from_vec(raw.to_vec()));
    let absolute = if path.is_absolute() {
        path
    } else {
        base.join(path)
    };
    fs::canonicalize(absolute).context("Git path identity could not be canonicalized")
}

fn read_checkout_instance(git_dir: &Path, create: bool) -> Result<String> {
    let path = git_dir.join(INSTANCE_FILE);
    match read_private_regular(&path, 128, false) {
        Ok(raw) if !raw.is_empty() => return parse_instance(&raw),
        Ok(_) => {
            return Err(domain_error(
                DirtyCheckoutErrorKind::MalformedState,
                "checkout instance sentinel is empty",
            ));
        }
        Err(error)
            if error
                .downcast_ref::<io::Error>()
                .is_some_and(|io| io.kind() == io::ErrorKind::NotFound) => {}
        Err(error) => {
            return Err(command_error(
                error,
                DirtyCheckoutErrorKind::MalformedState,
                "checkout instance sentinel is untrusted",
            ));
        }
    }
    if !create {
        return Err(domain_error(
            DirtyCheckoutErrorKind::MalformedState,
            "checkout instance sentinel is missing",
        ));
    }
    let value = random_hex(16)?;
    atomic_create_once(&path, format!("{value}\n").as_bytes())?;
    let raw = read_private_regular(&path, 128, true)?;
    parse_instance(&raw)
}

fn parse_instance(raw: &[u8]) -> Result<String> {
    let value = std::str::from_utf8(raw)
        .map_err(|_| {
            domain_error(
                DirtyCheckoutErrorKind::MalformedState,
                "checkout instance sentinel is malformed",
            )
        })?
        .trim();
    if !is_lower_hex(value, 32) {
        return Err(domain_error(
            DirtyCheckoutErrorKind::MalformedState,
            "checkout instance sentinel is malformed",
        ));
    }
    Ok(value.to_string())
}

fn reject_active_git_operation(identity: &CheckoutIdentity) -> Result<()> {
    for marker in [
        "MERGE_HEAD",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
        "rebase-merge",
        "rebase-apply",
        "sequencer",
        "BISECT_LOG",
        "index.lock",
    ] {
        match fs::symlink_metadata(identity.git_dir.join(marker)) {
            Ok(_) => {
                return Err(domain_error(
                    DirtyCheckoutErrorKind::UnsupportedGitState,
                    "checkout has an active or ambiguous Git operation",
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(domain_error(
                    DirtyCheckoutErrorKind::ResourceUnavailable,
                    format!("Git operation state could not be verified: {error}"),
                ));
            }
        }
    }
    Ok(())
}

fn parse_index_bounded(
    raw: &[u8],
    entry_limit: usize,
    budget: &mut SnapshotBudget,
) -> Result<Vec<IndexEntry>> {
    let payload = nul_payload(raw)?;
    if payload.is_empty() {
        return Ok(Vec::new());
    }
    let mut entries = Vec::new();
    for record in payload.split(|byte| *byte == 0) {
        if entries.len() >= entry_limit {
            return Err(domain_error(
                DirtyCheckoutErrorKind::ResourceUnavailable,
                "dirty snapshot tracked entry count exceeds the supported limit",
            ));
        }
        let tab = record
            .iter()
            .position(|byte| *byte == b'\t')
            .context("Git index output is malformed")?;
        let metadata = &record[..tab];
        let path = &record[tab + 1..];
        let mut fields = metadata.split(|byte| *byte == b' ');
        let mode = fields.next().context("Git index output is malformed")?;
        let oid = fields.next().context("Git index output is malformed")?;
        let stage = fields.next().context("Git index output is malformed")?;
        ensure!(fields.next().is_none(), "Git index output is malformed");
        ensure!(
            mode.len() == 6 && mode.iter().all(u8::is_ascii_digit),
            "Git index mode is malformed"
        );
        ensure!(
            (40..=64).contains(&oid.len()) && oid.iter().all(u8::is_ascii_hexdigit),
            "Git index object identity is malformed"
        );
        if oid.iter().all(|byte| *byte == b'0') {
            return Err(domain_error(
                DirtyCheckoutErrorKind::UnsupportedGitState,
                "intent-to-add index entries are unsupported",
            ));
        }
        if stage != b"0" {
            return Err(domain_error(
                DirtyCheckoutErrorKind::UnsupportedGitState,
                "unmerged index stages are unsupported",
            ));
        }
        validate_repo_relative_path(path)?;
        budget.charge_path(path.len())?;
        entries.push(IndexEntry {
            mode: mode.to_vec(),
            oid: oid.to_vec(),
            path: path.to_vec(),
        });
    }
    Ok(entries)
}

fn parse_nul_paths_bounded(
    raw: &[u8],
    entry_limit: usize,
    budget: &mut SnapshotBudget,
) -> Result<Vec<Vec<u8>>> {
    let payload = nul_payload(raw)?;
    if payload.is_empty() {
        return Ok(Vec::new());
    }
    let mut paths = Vec::new();
    for path in payload.split(|byte| *byte == 0) {
        if paths.len() >= entry_limit {
            return Err(domain_error(
                DirtyCheckoutErrorKind::ResourceUnavailable,
                "dirty snapshot path entry count exceeds the supported limit",
            ));
        }
        validate_repo_relative_path(path)?;
        budget.charge_path(path.len())?;
        paths.push(path.to_vec());
    }
    Ok(paths)
}

#[cfg(test)]
fn parse_nul_paths(raw: &[u8]) -> Result<Vec<Vec<u8>>> {
    parse_nul_paths_bounded(raw, MAX_ENTRY_COUNT, &mut SnapshotBudget::default())
}

fn validate_index_flags(
    raw: &[u8],
    tracked: &[IndexEntry],
    budget: &mut SnapshotBudget,
) -> Result<()> {
    let payload = nul_payload(raw)?;
    if payload.is_empty() {
        if tracked.is_empty() {
            return Ok(());
        }
        return Err(domain_error(
            DirtyCheckoutErrorKind::UnsupportedGitState,
            "Git index flag listing does not match the index",
        ));
    }
    let mut count = 0_usize;
    for record in payload.split(|byte| *byte == 0) {
        if count >= tracked.len() || record.len() < 3 || record[1] != b' ' {
            return Err(domain_error(
                DirtyCheckoutErrorKind::UnsupportedGitState,
                "Git index flag listing does not match the index",
            ));
        }
        let path = &record[2..];
        validate_repo_relative_path(path)?;
        budget.charge_path(path.len())?;
        if record[0] != b'H' {
            return Err(domain_error(
                DirtyCheckoutErrorKind::UnsupportedGitState,
                "tracked index flags that can hide worktree drift are unsupported",
            ));
        }
        if path != tracked[count].path.as_slice() {
            return Err(domain_error(
                DirtyCheckoutErrorKind::UnsupportedGitState,
                "Git index flag listing does not match the index",
            ));
        }
        count += 1;
    }
    if count != tracked.len() {
        return Err(domain_error(
            DirtyCheckoutErrorKind::UnsupportedGitState,
            "Git index flag listing does not match the index",
        ));
    }
    Ok(())
}

fn nul_payload(raw: &[u8]) -> Result<&[u8]> {
    if raw.is_empty() {
        return Ok(raw);
    }
    ensure!(
        raw.last() == Some(&0),
        "NUL-delimited Git output is malformed"
    );
    let payload = &raw[..raw.len() - 1];
    ensure!(
        !payload.is_empty()
            && !payload
                .split(|byte| *byte == 0)
                .any(|record| record.is_empty()),
        "NUL-delimited Git output is malformed"
    );
    Ok(payload)
}

fn ensure_unique_paths<'a>(paths: impl Iterator<Item = &'a [u8]>) -> Result<()> {
    let mut seen = HashSet::new();
    for path in paths {
        ensure!(
            seen.insert(path.to_vec()),
            "Git path listing contains duplicate entries"
        );
    }
    Ok(())
}

fn validate_repo_relative_path(raw: &[u8]) -> Result<()> {
    ensure!(
        !raw.is_empty() && !raw.contains(&0),
        "Git path is malformed"
    );
    let path = Path::new(OsStr::from_bytes(raw));
    ensure!(!path.is_absolute(), "absolute Git paths are unsupported");
    ensure!(
        path.components()
            .all(|component| matches!(component, Component::Normal(_))),
        "Git path escapes the checkout"
    );
    Ok(())
}

fn reject_special_filesystem_objects(
    root: &Path,
    submodule_paths: &HashSet<&[u8]>,
    budget: &mut SnapshotBudget,
    deadline: Instant,
) -> Result<()> {
    let mut directories = vec![PathBuf::new()];
    let mut entry_count = 0_usize;
    while let Some(relative_directory) = directories.pop() {
        ensure_deadline(deadline)?;
        let directory = root.join(&relative_directory);
        let metadata = fs::symlink_metadata(&directory)
            .context("checkout directory metadata is unavailable")?;
        ensure!(
            metadata.file_type().is_dir() && !metadata.file_type().is_symlink(),
            "checkout directory changed during snapshot"
        );
        let canonical = fs::canonicalize(&directory)
            .context("checkout directory identity could not be verified")?;
        ensure!(
            canonical.starts_with(root),
            "checkout directory escapes the checkout"
        );
        let entries =
            fs::read_dir(&directory).context("checkout directory could not be scanned")?;
        for entry in entries {
            ensure_deadline(deadline)?;
            let entry = entry.context("checkout directory entry is unavailable")?;
            if relative_directory.as_os_str().is_empty() && entry.file_name().as_bytes() == b".git"
            {
                continue;
            }
            entry_count = entry_count.saturating_add(1);
            if entry_count > MAX_ENTRY_COUNT {
                return Err(domain_error(
                    DirtyCheckoutErrorKind::ResourceUnavailable,
                    "checkout filesystem entry count exceeds the supported limit",
                ));
            }
            let path = entry.path();
            let relative = path
                .strip_prefix(root)
                .context("checkout directory entry escapes the checkout")?;
            let relative_bytes = relative.as_os_str().as_bytes();
            let metadata = fs::symlink_metadata(&path)
                .context("checkout directory entry metadata is unavailable")?;
            if submodule_paths.contains(relative_bytes) {
                ensure!(
                    metadata.file_type().is_dir() && !metadata.file_type().is_symlink(),
                    "submodule checkout path is not a directory"
                );
                continue;
            }
            if metadata.file_type().is_dir() {
                budget.charge_path(relative_bytes.len())?;
                directories.push(relative.to_path_buf());
            } else if metadata.file_type().is_symlink() {
                let target = fs::read_link(&path).context("symlink target could not be read")?;
                if target.as_os_str().as_bytes().len() > 64 * 1024 {
                    return Err(domain_error(
                        DirtyCheckoutErrorKind::ResourceUnavailable,
                        "symlink target exceeds the supported limit",
                    ));
                }
                ensure_symlink_stays_within_root(
                    root,
                    path.parent().expect("directory entry parent"),
                    &target,
                )?;
                let after = fs::symlink_metadata(&path)
                    .context("symlink changed during checkout traversal")?;
                ensure!(
                    same_metadata(&metadata, &after),
                    "symlink changed during checkout traversal"
                );
            } else if metadata.file_type().is_file() {
                ensure!(
                    metadata.nlink() == 1,
                    "multiply-linked files are unsupported"
                );
            } else {
                bail!("special filesystem objects are unsupported");
            }
        }
    }
    Ok(())
}

fn hash_worktree_object(
    hasher: &mut FramedHasher,
    root: &Path,
    raw_path: &[u8],
    submodule: bool,
    index_oid: Option<&Vec<u8>>,
    budget: &mut SnapshotBudget,
    deadline: Instant,
) -> Result<()> {
    let relative = PathBuf::from(OsString::from_vec(raw_path.to_vec()));
    let path = root.join(&relative);
    verify_parent_within_root(root, &path)?;
    let before = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            ensure!(!submodule, "submodule checkout is unavailable");
            hasher.field(b"worktree_kind", b"missing");
            return Ok(());
        }
        Err(error) => return Err(error).context("worktree object metadata is unavailable"),
    };

    if submodule {
        ensure!(
            before.file_type().is_dir(),
            "dirty or unavailable submodules are unsupported"
        );
        let canonical = fs::canonicalize(&path).context("submodule path could not be verified")?;
        ensure!(
            canonical.starts_with(root),
            "submodule path escapes the checkout"
        );
        reject_special_filesystem_objects(&canonical, &HashSet::new(), budget, deadline)?;
        let head = snapshot_git_output(
            budget,
            &canonical,
            &["rev-parse", "--verify", "HEAD^{commit}"],
            256,
            deadline,
        )?;
        ensure!(
            head.status.success(),
            "dirty or unavailable submodules are unsupported"
        );
        let submodule_head = strip_git_line(&head.stdout)?;
        ensure!(
            index_oid.is_some_and(|oid| oid.as_slice() == submodule_head),
            "dirty or unavailable submodules are unsupported"
        );
        let status = snapshot_git_output(
            budget,
            &canonical,
            &[
                "status",
                "--porcelain=v1",
                "-z",
                "--untracked-files=all",
                "--ignore-submodules=none",
            ],
            MAX_GIT_OUTPUT_BYTES,
            deadline,
        )?;
        ensure!(
            status.status.success() && status.stdout.is_empty(),
            "dirty submodules are unsupported"
        );
        hasher.field(b"worktree_kind", b"submodule");
        hasher.field(b"worktree_digest", submodule_head);
        return Ok(());
    }

    if before.file_type().is_symlink() {
        let target = fs::read_link(&path).context("symlink target could not be read")?;
        ensure_symlink_stays_within_root(root, path.parent().expect("path parent"), &target)?;
        let after = fs::symlink_metadata(&path).context("symlink changed during snapshot")?;
        ensure!(
            same_metadata(&before, &after),
            "symlink changed during snapshot"
        );
        let target_bytes = target.as_os_str().as_bytes();
        ensure!(
            target_bytes.len() <= 64 * 1024,
            "symlink target exceeds the supported limit"
        );
        hasher.field(b"worktree_kind", b"symlink");
        hasher.field(b"worktree_digest", &Sha256::digest(target_bytes));
        return Ok(());
    }
    ensure!(
        before.file_type().is_file(),
        "special filesystem objects are unsupported"
    );
    ensure!(before.nlink() == 1, "multiply-linked files are unsupported");
    if before.len() > MAX_FILE_BYTES {
        return Err(domain_error(
            DirtyCheckoutErrorKind::ResourceUnavailable,
            "file exceeds the dirty snapshot size limit",
        ));
    }
    let remaining_total = MAX_TOTAL_BYTES.saturating_sub(budget.file_bytes);
    if before.len() > remaining_total {
        return Err(domain_error(
            DirtyCheckoutErrorKind::ResourceUnavailable,
            "dirty snapshot total bytes exceed the supported limit",
        ));
    }
    let canonical = fs::canonicalize(&path).context("worktree file could not be verified")?;
    ensure!(
        canonical.starts_with(root),
        "worktree file escapes the checkout"
    );

    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(&path)
        .context("worktree file could not be opened safely")?;
    let opened = file
        .metadata()
        .context("worktree file metadata is unavailable")?;
    ensure!(
        same_metadata(&before, &opened),
        "worktree file changed before hashing"
    );
    let mut digest = Sha256::new();
    let mut remaining = before.len();
    let mut buffer = [0_u8; 64 * 1024];
    while remaining > 0 {
        ensure_deadline(deadline)?;
        let read_limit =
            usize::try_from(remaining.min(buffer.len() as u64)).expect("bounded read size");
        let read = file
            .read(&mut buffer[..read_limit])
            .context("worktree file read failed")?;
        ensure!(read > 0, "worktree file changed while hashing");
        digest.update(&buffer[..read]);
        remaining -= read as u64;
    }
    let mut extra = [0_u8; 1];
    ensure!(
        file.read(&mut extra)
            .context("worktree file final read failed")?
            == 0,
        "worktree file grew while hashing"
    );
    let after = file
        .metadata()
        .context("worktree file final metadata is unavailable")?;
    ensure!(
        same_metadata(&opened, &after),
        "worktree file changed while hashing"
    );
    let pathname_after =
        fs::symlink_metadata(&path).context("worktree file pathname changed while hashing")?;
    ensure!(
        same_metadata(&opened, &pathname_after),
        "worktree file pathname changed while hashing"
    );
    let canonical_after =
        fs::canonicalize(&path).context("worktree file pathname could not be revalidated")?;
    ensure!(
        canonical_after == canonical,
        "worktree file pathname changed while hashing"
    );
    budget.file_bytes = budget
        .file_bytes
        .checked_add(before.len())
        .context("dirty snapshot byte count overflowed")?;
    ensure!(
        budget.file_bytes <= MAX_TOTAL_BYTES,
        "dirty snapshot total bytes exceed the supported limit"
    );
    hasher.field(b"worktree_kind", b"regular");
    hasher.field(b"worktree_mode", &(before.mode() & 0o7777).to_be_bytes());
    hasher.field(b"worktree_digest", &digest.finalize());
    Ok(())
}

fn verify_parent_within_root(root: &Path, path: &Path) -> Result<()> {
    let parent = path.parent().context("worktree path has no parent")?;
    let canonical =
        fs::canonicalize(parent).context("worktree path parent could not be verified")?;
    ensure!(
        canonical.starts_with(root),
        "worktree path escapes the checkout"
    );
    Ok(())
}

fn ensure_symlink_stays_within_root(root: &Path, parent: &Path, target: &Path) -> Result<()> {
    ensure!(
        !target.is_absolute(),
        "absolute symlink targets are unsupported"
    );
    let relative_parent = parent
        .strip_prefix(root)
        .context("symlink parent escapes the checkout")?;
    let mut depth = relative_parent.components().count();
    for component in target.components() {
        match component {
            Component::Normal(_) => depth = depth.saturating_add(1),
            Component::CurDir => {}
            Component::ParentDir if depth > 0 => depth -= 1,
            Component::ParentDir => bail!("symlink target escapes the checkout"),
            Component::RootDir | Component::Prefix(_) => {
                bail!("symlink target escapes the checkout")
            }
        }
    }
    Ok(())
}

fn same_metadata(left: &Metadata, right: &Metadata) -> bool {
    left.dev() == right.dev()
        && left.ino() == right.ino()
        && left.mode() == right.mode()
        && left.nlink() == right.nlink()
        && left.len() == right.len()
        && left.mtime() == right.mtime()
        && left.mtime_nsec() == right.mtime_nsec()
        && left.ctime() == right.ctime()
        && left.ctime_nsec() == right.ctime_nsec()
}

fn validate_challenge(
    record: &ChallengeRecord,
    identity: &CheckoutIdentity,
    digest: &str,
) -> Result<()> {
    validate_challenge_identity(record, identity, digest)?;
    validate_challenge_at(record, unix_time()?)
}

fn validate_challenge_identity(
    record: &ChallengeRecord,
    identity: &CheckoutIdentity,
    digest: &str,
) -> Result<()> {
    let valid = record.schema == CHALLENGE_SCHEMA
        && is_lower_hex(&record.token_digest, 64)
        && is_lower_hex(&record.session_key, 64)
        && is_lower_hex(&record.repository_key, 64)
        && is_lower_hex(&record.checkout_key, 64)
        && is_lower_hex(&record.checkout_instance, 32)
        && is_lower_hex(&record.snapshot_id, 64)
        && valid_head_identity(&record.head_oid)
        && is_lower_hex(&record.branch_ref_digest, 64)
        && is_lower_hex(&record.authorization_turn_digest, 64)
        && record.expires_at > record.issued_at
        && record.expires_at.saturating_sub(record.issued_at) <= 300;
    if !valid {
        return Err(domain_error(
            DirtyCheckoutErrorKind::MalformedState,
            "dirty-checkout challenge state is malformed",
        ));
    }
    if record.token_digest != digest {
        return Err(domain_error(
            DirtyCheckoutErrorKind::InvalidInput,
            "dirty-checkout challenge token does not match",
        ));
    }
    if record.repository_key != identity.repository_key
        || record.checkout_key != identity.checkout_key
        || record.checkout_instance != identity.checkout_instance
    {
        return Err(domain_error(
            DirtyCheckoutErrorKind::ChallengeDrift,
            "dirty-checkout challenge belongs to another checkout instance",
        ));
    }
    Ok(())
}

fn validate_challenge_at(record: &ChallengeRecord, now: u64) -> Result<()> {
    if record.issued_at > now || now >= record.expires_at {
        return Err(domain_error(
            DirtyCheckoutErrorKind::ChallengeExpired,
            "dirty-checkout challenge is expired or not yet valid",
        ));
    }
    Ok(())
}

fn validate_adoption_precommit_with<B, N>(
    challenge: &ChallengeRecord,
    identity: &CheckoutIdentity,
    before_validation: B,
    now: N,
) -> Result<()>
where
    B: FnOnce() -> Result<()>,
    N: FnOnce() -> Result<u64>,
{
    before_validation()?;
    validate_adoption_boundary(challenge, identity, now()?)
}

fn validate_adoption_boundary(
    challenge: &ChallengeRecord,
    identity: &CheckoutIdentity,
    now: u64,
) -> Result<()> {
    reject_active_git_operation(identity)?;
    validate_challenge_at(challenge, now)
}

fn valid_head_identity(value: &str) -> bool {
    if let Some(digest) = value.strip_prefix(UNBORN_HEAD_PREFIX) {
        return is_lower_hex(digest, 64);
    }
    (40..=64).contains(&value.len()) && is_lower_hex(value, value.len())
}

fn validate_receipt(
    record: &ReceiptRecord,
    identity: &CheckoutIdentity,
    receipt_id: &str,
) -> Result<()> {
    let valid = record.schema == RECEIPT_SCHEMA
        && is_lower_hex(&record.receipt_id, 64)
        && is_lower_hex(&record.session_key, 64)
        && is_lower_hex(&record.repository_key, 64)
        && is_lower_hex(&record.checkout_key, 64)
        && is_lower_hex(&record.checkout_instance, 32)
        && is_lower_hex(&record.snapshot_id, 64)
        && is_lower_hex(&record.authorization_turn_digest, 64)
        && is_lower_hex(&record.reason_digest, 64)
        && is_lower_hex(&record.challenge_digest, 64)
        && record.receipt_id == receipt_id
        && record.repository_key == identity.repository_key
        && record.checkout_key == identity.checkout_key
        && record.checkout_instance == identity.checkout_instance;
    if !valid {
        return Err(domain_error(
            DirtyCheckoutErrorKind::MalformedState,
            "dirty-checkout receipt state is malformed or belongs to another checkout",
        ));
    }
    Ok(())
}

fn load_lease(path: &Path) -> Result<Option<LeaseRecord>> {
    let bytes = match read_private_regular(path, MAX_STATE_FILE_BYTES, true) {
        Ok(bytes) => bytes,
        Err(error)
            if error
                .downcast_ref::<io::Error>()
                .is_some_and(|io| io.kind() == io::ErrorKind::NotFound) =>
        {
            return Ok(None);
        }
        Err(error) => {
            return Err(command_error(
                error,
                DirtyCheckoutErrorKind::MalformedState,
                "checkout lease state is untrusted",
            ));
        }
    };
    Ok(Some(parse_lease(&bytes)?))
}

fn parse_lease(bytes: &[u8]) -> Result<LeaseRecord> {
    let wire: LeaseWire = serde_json::from_slice(bytes).map_err(|_| {
        domain_error(
            DirtyCheckoutErrorKind::MalformedState,
            "checkout lease state is malformed",
        )
    })?;
    match wire {
        LeaseWire::V1(wire) if wire.schema == LEASE_V1_SCHEMA => Ok(LeaseRecord::V1(wire)),
        LeaseWire::V2(wire) if wire.schema == LEASE_V2_SCHEMA => Ok(LeaseRecord::V2(wire)),
        LeaseWire::V1(_) | LeaseWire::V2(_) => Err(domain_error(
            DirtyCheckoutErrorKind::MalformedState,
            "checkout lease schema is unsupported",
        )),
    }
}

fn validate_lease(lease: &LeaseRecord, identity: &CheckoutIdentity) -> Result<()> {
    if !is_lower_hex(lease.session_key(), 64)
        || !is_lower_hex(lease.checkout_instance(), 32)
        || lease.acquired_at() > lease.refreshed_at()
        || lease.refreshed_at() > lease.expires_at()
        || lease.checkout_instance() != identity.checkout_instance
    {
        return Err(domain_error(
            DirtyCheckoutErrorKind::MalformedState,
            "checkout lease identity or timestamps are malformed",
        ));
    }
    validate_lease_text_path(lease.checkout_root(), &identity.root, "checkout lease root")?;
    validate_lease_text_path(
        lease.checkout_git_dir(),
        &identity.git_dir,
        "checkout lease Git directory",
    )?;

    match lease {
        LeaseRecord::V1(lease) => {
            if lease.schema != LEASE_V1_SCHEMA {
                return Err(domain_error(
                    DirtyCheckoutErrorKind::MalformedState,
                    "checkout lease schema is unsupported",
                ));
            }
        }
        LeaseRecord::V2(lease) => {
            if lease.schema != LEASE_V2_SCHEMA {
                return Err(domain_error(
                    DirtyCheckoutErrorKind::MalformedState,
                    "checkout lease schema is unsupported",
                ));
            }
            let root_bytes = validate_lease_raw_path(
                &lease.checkout_root_bytes,
                &lease.checkout_root,
                &identity.root,
                "v2 checkout root identity",
            )?;
            let git_dir_bytes = validate_lease_raw_path(
                &lease.checkout_git_dir_bytes,
                &lease.checkout_git_dir,
                &identity.git_dir,
                "v2 checkout Git-dir identity",
            )?;
            ensure!(
                !root_bytes.is_empty() && !git_dir_bytes.is_empty(),
                "v2 checkout paths are malformed"
            );
            let adoption = &lease.adoption;
            let valid_adoption = adoption.schema == ADOPTION_SCHEMA
                && adoption.receipt_schema == RECEIPT_SCHEMA
                && is_lower_hex(&adoption.receipt_id, 64)
                && is_lower_hex(&adoption.snapshot_id, 64)
                && is_lower_hex(&adoption.authorization_turn_digest, 64)
                && is_lower_hex(&adoption.reason_digest, 64)
                && is_lower_hex(&adoption.challenge_digest, 64)
                && adoption.challenge_issued_at <= adoption.adopted_at
                && adoption.adopted_at <= lease.refreshed_at;
            if !valid_adoption {
                return Err(domain_error(
                    DirtyCheckoutErrorKind::MalformedState,
                    "dirty-checkout adoption state is malformed",
                ));
            }
        }
    }
    Ok(())
}

fn validate_lease_text_path(value: &str, expected: &Path, label: &str) -> Result<()> {
    if value.is_empty()
        || value.as_bytes().contains(&0)
        || !Path::new(value).is_absolute()
        || value.as_bytes() != expected.as_os_str().as_bytes()
    {
        return Err(domain_error(
            DirtyCheckoutErrorKind::MalformedState,
            format!("{label} is malformed or does not match the current checkout"),
        ));
    }
    Ok(())
}

fn validate_lease_raw_path(
    value: &str,
    text: &str,
    expected: &Path,
    label: &str,
) -> Result<Vec<u8>> {
    let bytes = decode_lower_hex(value).ok_or_else(|| {
        domain_error(
            DirtyCheckoutErrorKind::MalformedState,
            format!("{label} is malformed"),
        )
    })?;
    let path = Path::new(OsStr::from_bytes(&bytes));
    if bytes.is_empty()
        || bytes.contains(&0)
        || !path.is_absolute()
        || bytes.as_slice() != text.as_bytes()
        || bytes.as_slice() != expected.as_os_str().as_bytes()
    {
        return Err(domain_error(
            DirtyCheckoutErrorKind::MalformedState,
            format!("{label} does not match the textual path or current checkout"),
        ));
    }
    Ok(bytes)
}

fn decode_lower_hex(value: &str) -> Option<Vec<u8>> {
    if value.is_empty() || !value.len().is_multiple_of(2) || !is_lower_hex(value, value.len()) {
        return None;
    }
    let mut bytes = Vec::with_capacity(value.len() / 2);
    for pair in value.as_bytes().chunks_exact(2) {
        let high = (pair[0] as char).to_digit(16)? as u8;
        let low = (pair[1] as char).to_digit(16)? as u8;
        bytes.push((high << 4) | low);
    }
    Some(bytes)
}

fn checkout_state_dir(state_root: &Path, identity: &CheckoutIdentity) -> Result<PathBuf> {
    validate_state_root(state_root, identity)?;
    let repository_dir = state_root.join(&identity.repository_key);
    let directory = repository_dir.join(&identity.checkout_key);
    verify_private_directory(&repository_dir).map_err(|error| {
        command_error(
            error,
            DirtyCheckoutErrorKind::MalformedState,
            "dirty-checkout repository state directory is untrusted",
        )
    })?;
    verify_private_directory(&directory).map_err(|error| {
        command_error(
            error,
            DirtyCheckoutErrorKind::MalformedState,
            "dirty-checkout checkout state directory is untrusted",
        )
    })?;
    verify_no_symlink_components(&directory)?;
    Ok(directory)
}

fn validate_state_root(state_root: &Path, identity: &CheckoutIdentity) -> Result<()> {
    if !state_root.is_absolute() {
        return Err(domain_error(
            DirtyCheckoutErrorKind::InvalidInput,
            "dirty-checkout state root must be absolute",
        ));
    }
    verify_no_symlink_components(state_root)?;
    let canonical = fs::canonicalize(state_root).map_err(|error| {
        domain_error(
            DirtyCheckoutErrorKind::ResourceUnavailable,
            format!("dirty-checkout state root is unavailable: {error}"),
        )
    })?;
    if canonical != state_root {
        return Err(domain_error(
            DirtyCheckoutErrorKind::InvalidInput,
            "dirty-checkout state root must be canonical",
        ));
    }
    verify_private_directory(state_root).map_err(|error| {
        command_error(
            error,
            DirtyCheckoutErrorKind::InvalidInput,
            "dirty-checkout state root is not private",
        )
    })?;
    if canonical.starts_with(&identity.root)
        || canonical.starts_with(&identity.git_dir)
        || canonical.starts_with(&identity.common_dir)
    {
        return Err(domain_error(
            DirtyCheckoutErrorKind::InvalidInput,
            "dirty-checkout state root must be outside the checkout and Git directories",
        ));
    }
    Ok(())
}

fn verify_no_symlink_components(path: &Path) -> Result<()> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        let metadata = fs::symlink_metadata(&current).map_err(|error| {
            domain_error(
                DirtyCheckoutErrorKind::ResourceUnavailable,
                format!("state path component is unavailable: {error}"),
            )
        })?;
        if metadata.file_type().is_symlink() {
            return Err(domain_error(
                DirtyCheckoutErrorKind::InvalidInput,
                "dirty-checkout state path contains a symlink component",
            ));
        }
    }
    Ok(())
}

fn private_directory(path: &Path) -> Result<()> {
    match fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => {
            return Err(error).context("dirty-checkout state directory is unavailable");
        }
    }
    verify_private_directory(path)
}

fn verify_private_directory(path: &Path) -> Result<()> {
    let metadata =
        fs::symlink_metadata(path).context("dirty-checkout state directory is unavailable")?;
    ensure!(
        metadata.file_type().is_dir() && !metadata.file_type().is_symlink(),
        "dirty-checkout state path is not a trusted directory"
    );
    ensure!(
        metadata.uid() == unsafe { libc::geteuid() },
        "dirty-checkout state directory has the wrong owner"
    );
    ensure!(
        metadata.mode() & 0o077 == 0,
        "dirty-checkout state directory is not private"
    );
    Ok(())
}

fn read_private_regular(path: &Path, max_bytes: usize, require_private: bool) -> Result<Vec<u8>> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
        .map_err(anyhow::Error::new)?;
    let metadata = file
        .metadata()
        .context("state file metadata is unavailable")?;
    ensure!(
        metadata.file_type().is_file() && metadata.len() <= max_bytes as u64,
        "state file is not a bounded regular file"
    );
    if require_private {
        ensure!(
            metadata.uid() == unsafe { libc::geteuid() },
            "state file has the wrong owner"
        );
        ensure!(metadata.mode() & 0o077 == 0, "state file is not private");
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(max_bytes as u64 + 1)
        .read_to_end(&mut bytes)
        .context("state file read failed")?;
    ensure!(
        bytes.len() <= max_bytes,
        "state file exceeds the supported limit"
    );
    Ok(bytes)
}

fn read_regular_bounded(path: &Path, max_bytes: usize) -> Result<Vec<u8>> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
        .context("adoption reason file could not be opened safely")?;
    let metadata = file
        .metadata()
        .context("adoption reason metadata is unavailable")?;
    ensure!(
        metadata.file_type().is_file() && metadata.len() <= max_bytes as u64,
        "adoption reason is not a bounded regular file"
    );
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(max_bytes as u64 + 1)
        .read_to_end(&mut bytes)
        .context("adoption reason read failed")?;
    ensure!(
        !bytes.is_empty() && bytes.len() <= max_bytes,
        "adoption reason file is empty or too large"
    );
    Ok(bytes)
}

fn write_json_atomic(path: &Path, value: &impl Serialize, replace: bool) -> Result<()> {
    let mut payload = serde_json::to_vec(value).context("state serialization failed")?;
    payload.push(b'\n');
    ensure!(
        payload.len() <= MAX_STATE_FILE_BYTES,
        "serialized state exceeds the supported limit"
    );
    let parent = path
        .parent()
        .context("state file has no parent directory")?;
    let temporary = parent.join(format!(".state-{}", random_hex(16)?));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(&temporary)
        .context("temporary state file could not be created")?;
    let result = (|| -> Result<()> {
        file.write_all(&payload)
            .context("state file write failed")?;
        file.sync_all().context("state file sync failed")?;
        drop(file);
        if replace {
            fs::rename(&temporary, path).context("state file replacement failed")?;
        } else {
            fs::hard_link(&temporary, path).context("state file creation failed")?;
            fs::remove_file(&temporary).context("temporary state cleanup failed")?;
        }
        sync_directory(parent)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn atomic_create_once(path: &Path, payload: &[u8]) -> Result<()> {
    let parent = path.parent().context("sentinel path has no parent")?;
    let temporary = parent.join(format!(".{INSTANCE_FILE}-{}", random_hex(8)?));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(&temporary)
        .context("checkout instance temporary file could not be created")?;
    file.write_all(payload)
        .context("checkout instance write failed")?;
    file.sync_all().context("checkout instance sync failed")?;
    drop(file);
    match fs::hard_link(&temporary, path) {
        Ok(()) => sync_directory(parent)?,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => {
            let _ = fs::remove_file(&temporary);
            return Err(error).context("checkout instance sentinel could not be installed");
        }
    }
    fs::remove_file(&temporary).context("checkout instance temporary cleanup failed")?;
    Ok(())
}

fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .context("state directory sync failed")
}

struct LeaseLock(File);

impl LeaseLock {
    fn acquire(directory: &Path) -> Result<Self> {
        let path = directory.join("lease.lock");
        let file = OpenOptions::new()
            .read(true)
            .append(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(path)
            .map_err(|error| {
                domain_error(
                    DirtyCheckoutErrorKind::ResourceUnavailable,
                    format!("checkout lease lock is unavailable: {error}"),
                )
            })?;
        let metadata = file.metadata().map_err(|error| {
            domain_error(
                DirtyCheckoutErrorKind::ResourceUnavailable,
                format!("checkout lease lock metadata is unavailable: {error}"),
            )
        })?;
        if !metadata.file_type().is_file()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o077 != 0
        {
            return Err(domain_error(
                DirtyCheckoutErrorKind::MalformedState,
                "checkout lease lock is not a private owner-controlled regular file",
            ));
        }
        let started = Instant::now();
        loop {
            let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if result == 0 {
                return Ok(Self(file));
            }
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::EWOULDBLOCK) {
                return Err(domain_error(
                    DirtyCheckoutErrorKind::ResourceUnavailable,
                    format!("checkout lease lock failed: {error}"),
                ));
            }
            if started.elapsed() >= LOCK_WAIT {
                return Err(domain_error(
                    DirtyCheckoutErrorKind::Timeout,
                    "checkout lease lock timed out",
                ));
            }
            std::thread::sleep(LOCK_POLL);
        }
    }
}

impl Drop for LeaseLock {
    fn drop(&mut self) {
        let _ = unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

use std::os::fd::AsRawFd;

fn unix_time() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock precedes the Unix epoch")?
        .as_secs())
}

fn lease_ttl_seconds() -> u64 {
    env::var("AGENT_RUNTIME_CHECKOUT_LEASE_TTL_SECONDS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(|value| value.clamp(60, MAX_LEASE_TTL_SECONDS))
        .unwrap_or(DEFAULT_LEASE_TTL_SECONDS)
}

fn random_hex(byte_count: usize) -> Result<String> {
    let mut bytes = vec![0_u8; byte_count];
    getrandom::fill(&mut bytes)
        .map_err(|error| anyhow::anyhow!("secure random generation failed: {error}"))?;
    Ok(hex_bytes(&bytes))
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex_bytes(&Sha256::digest(bytes))
}

fn hex_bytes(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn is_lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn strip_git_line(raw: &[u8]) -> Result<&[u8]> {
    ensure!(
        !raw.contains(&0),
        "Git output contains an unexpected NUL byte"
    );
    let without_lf = raw.strip_suffix(b"\n").unwrap_or(raw);
    Ok(without_lf.strip_suffix(b"\r").unwrap_or(without_lf))
}

fn deadline_after(timeout: Duration) -> Result<Instant> {
    if timeout.is_zero() {
        return Err(domain_error(
            DirtyCheckoutErrorKind::Timeout,
            "dirty snapshot exceeded the supported time limit",
        ));
    }
    Instant::now().checked_add(timeout).ok_or_else(|| {
        domain_error(
            DirtyCheckoutErrorKind::ResourceUnavailable,
            "dirty snapshot deadline could not be represented",
        )
    })
}

fn ensure_deadline(deadline: Instant) -> Result<()> {
    if Instant::now() >= deadline {
        return Err(domain_error(
            DirtyCheckoutErrorKind::Timeout,
            "dirty snapshot exceeded the supported time limit",
        ));
    }
    Ok(())
}

fn git_output(
    cwd: &Path,
    args: &[&str],
    limit: usize,
    deadline: Instant,
) -> Result<std::process::Output> {
    let owned: Vec<OsString> = args.iter().map(OsString::from).collect();
    git_output_os(cwd, &owned, limit, deadline)
}

fn git_output_os(
    cwd: &Path,
    args: &[OsString],
    limit: usize,
    deadline: Instant,
) -> Result<std::process::Output> {
    ensure_deadline(deadline)?;
    let command_deadline = deadline.min(deadline_after(GIT_TIMEOUT)?);
    let executable = trusted_git_executable()?;
    ensure_deadline(deadline)?;
    let mut command = Command::new(executable);
    sanitize_git_environment(&mut command);
    command
        .arg("-c")
        .arg("core.fsmonitor=false")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ATTR_NOSYSTEM", "1")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if env::var_os(SNAPSHOT_WORKER_ENV).as_deref() != Some(OsStr::new("1")) {
        command.process_group(0);
    }
    output_with_aggregate_limit_until(
        &mut command,
        command_deadline,
        limit,
        MAX_GIT_STDERR_BYTES,
        limit,
    )
}

fn trusted_git_executable() -> Result<PathBuf> {
    for candidate in TRUSTED_GIT_PATHS {
        let path = Path::new(candidate);
        let Ok(canonical) = fs::canonicalize(path) else {
            continue;
        };
        let Ok(metadata) = fs::metadata(&canonical) else {
            continue;
        };
        if metadata.file_type().is_file()
            && metadata.uid() == 0
            && metadata.mode() & 0o022 == 0
            && metadata.mode() & 0o111 != 0
        {
            return Ok(canonical);
        }
    }
    Err(domain_error(
        DirtyCheckoutErrorKind::ResourceUnavailable,
        "a trusted system Git executable is unavailable",
    ))
}

fn sanitize_git_environment(command: &mut Command) {
    const EXACT: &[&str] = &[
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_ASKPASS",
        "GIT_ATTR_NOSYSTEM",
        "GIT_CEILING_DIRECTORIES",
        "GIT_COMMON_DIR",
        "GIT_DIFF_OPTS",
        "GIT_DIR",
        "GIT_DISCOVERY_ACROSS_FILESYSTEM",
        "GIT_EXEC_PATH",
        "GIT_EXTERNAL_DIFF",
        "GIT_GRAFT_FILE",
        "GIT_INDEX_FILE",
        "GIT_NAMESPACE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_QUARANTINE_PATH",
        "GIT_REPLACE_REF_BASE",
        "GIT_SHALLOW_FILE",
        "GIT_SSH",
        "GIT_SSH_COMMAND",
        "GIT_WORK_TREE",
    ];
    for (key, _) in env::vars_os() {
        let name = key.to_string_lossy();
        if EXACT.contains(&name.as_ref())
            || name.starts_with("GIT_CONFIG")
            || name.starts_with("GIT_TRACE")
        {
            command.env_remove(key);
        }
    }
}

#[cfg(test)]
fn output_with_limits(
    command: &mut Command,
    timeout: Duration,
    stdout_limit: usize,
    stderr_limit: usize,
) -> Result<std::process::Output> {
    output_with_aggregate_limit(
        command,
        timeout,
        stdout_limit,
        stderr_limit,
        stdout_limit.max(stderr_limit),
    )
}

#[cfg(test)]
fn output_with_aggregate_limit(
    command: &mut Command,
    timeout: Duration,
    stdout_limit: usize,
    stderr_limit: usize,
    aggregate_limit: usize,
) -> Result<std::process::Output> {
    output_with_aggregate_limit_until(
        command,
        deadline_after(timeout)?,
        stdout_limit,
        stderr_limit,
        aggregate_limit,
    )
}

fn output_with_aggregate_limit_until(
    command: &mut Command,
    deadline: Instant,
    stdout_limit: usize,
    stderr_limit: usize,
    aggregate_limit: usize,
) -> Result<std::process::Output> {
    ensure_deadline(deadline)?;
    let mut child = command
        .spawn()
        .context("Git command could not be started")?;
    let process_group = child.id() as libc::pid_t;
    if Instant::now() >= deadline {
        terminate_child(&mut child, process_group);
        return Err(domain_error(
            DirtyCheckoutErrorKind::Timeout,
            "Git command exceeded the supported time limit",
        ));
    }
    let mut stdout = child.stdout.take().context("Git stdout was not captured")?;
    let mut stderr = child.stderr.take().context("Git stderr was not captured")?;
    if let Err(error) =
        set_nonblocking(stdout.as_raw_fd()).and_then(|()| set_nonblocking(stderr.as_raw_fd()))
    {
        terminate_child(&mut child, process_group);
        return Err(domain_error(
            DirtyCheckoutErrorKind::ResourceUnavailable,
            format!("Git output pipes could not be made nonblocking: {error}"),
        ));
    }

    let mut stdout_bytes = Vec::with_capacity(stdout_limit.min(64 * 1024));
    let mut stderr_bytes = Vec::with_capacity(stderr_limit.min(64 * 1024));
    let mut stdout_eof = false;
    let mut stderr_eof = false;
    loop {
        if stdout_eof && stderr_eof {
            match child.try_wait() {
                Ok(Some(status)) => {
                    unsafe {
                        let _ = libc::kill(-process_group, libc::SIGKILL);
                    }
                    return Ok(std::process::Output {
                        status,
                        stdout: stdout_bytes,
                        stderr: stderr_bytes,
                    });
                }
                Ok(None) => {}
                Err(error) => {
                    terminate_child(&mut child, process_group);
                    return Err(error).context("Git command status failed");
                }
            }
        }

        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            terminate_child(&mut child, process_group);
            return Err(domain_error(
                DirtyCheckoutErrorKind::Timeout,
                "Git command exceeded the supported time limit",
            ));
        }
        if stdout_eof && stderr_eof {
            std::thread::sleep(remaining.min(Duration::from_millis(10)));
            continue;
        }

        let mut poll_fds = [
            libc::pollfd {
                fd: if stdout_eof { -1 } else { stdout.as_raw_fd() },
                events: libc::POLLIN | libc::POLLHUP | libc::POLLERR,
                revents: 0,
            },
            libc::pollfd {
                fd: if stderr_eof { -1 } else { stderr.as_raw_fd() },
                events: libc::POLLIN | libc::POLLHUP | libc::POLLERR,
                revents: 0,
            },
        ];
        let poll_timeout = duration_to_poll_timeout(remaining.min(Duration::from_millis(10)));
        let polled = unsafe {
            libc::poll(
                poll_fds.as_mut_ptr(),
                poll_fds.len() as libc::nfds_t,
                poll_timeout,
            )
        };
        if polled < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            terminate_child(&mut child, process_group);
            return Err(error).context("Git output polling failed");
        }

        let aggregate_before = stdout_bytes.len().saturating_add(stderr_bytes.len());
        if !stdout_eof && poll_fds[0].revents != 0 {
            match drain_nonblocking(
                &mut stdout,
                &mut stdout_bytes,
                stdout_limit,
                aggregate_limit.saturating_sub(aggregate_before),
            ) {
                Ok(eof) => stdout_eof = eof,
                Err(error) => {
                    terminate_child(&mut child, process_group);
                    return Err(error);
                }
            }
        }
        let aggregate_before = stdout_bytes.len().saturating_add(stderr_bytes.len());
        if !stderr_eof && poll_fds[1].revents != 0 {
            match drain_nonblocking(
                &mut stderr,
                &mut stderr_bytes,
                stderr_limit,
                aggregate_limit.saturating_sub(aggregate_before),
            ) {
                Ok(eof) => stderr_eof = eof,
                Err(error) => {
                    terminate_child(&mut child, process_group);
                    return Err(error);
                }
            }
        }
    }
}

fn set_nonblocking(fd: libc::c_int) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn duration_to_poll_timeout(duration: Duration) -> libc::c_int {
    let millis = duration.as_millis().max(1);
    libc::c_int::try_from(millis).unwrap_or(libc::c_int::MAX)
}

fn drain_nonblocking<R: Read>(
    reader: &mut R,
    bytes: &mut Vec<u8>,
    stream_limit: usize,
    aggregate_remaining: usize,
) -> Result<bool> {
    let mut aggregate_remaining = aggregate_remaining;
    let mut buffer = [0_u8; 8192];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => return Ok(true),
            Ok(read) => {
                let stream_remaining = stream_limit.saturating_sub(bytes.len());
                if read > stream_remaining || read > aggregate_remaining {
                    return Err(domain_error(
                        DirtyCheckoutErrorKind::ResourceUnavailable,
                        "Git output exceeds the supported size limit",
                    ));
                }
                bytes.extend_from_slice(&buffer[..read]);
                aggregate_remaining -= read;
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(false),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error).context("Git output read failed"),
        }
    }
}

fn terminate_child(child: &mut Child, process_group: libc::pid_t) {
    unsafe {
        let _ = libc::kill(-process_group, libc::SIGKILL);
    }
    let _ = child.kill();
    let reap_deadline = Instant::now()
        .checked_add(Duration::from_millis(250))
        .unwrap_or_else(Instant::now);
    loop {
        match child.try_wait() {
            Ok(Some(_)) | Err(_) => return,
            Ok(None) if Instant::now() < reap_deadline => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Ok(None) => return,
        }
    }
}

pub(super) fn run_dirty_snapshot(args: &[String]) -> i32 {
    let requested_format = detect_format(args);
    let mut args = args.to_vec();
    let format = match take_format(&mut args) {
        Ok(format) => format,
        Err(error) => return emit_error("worktree.dirty-snapshot", requested_format, error),
    };
    if args
        .iter()
        .any(|arg| matches!(arg.as_str(), "-h" | "--help"))
    {
        println!("Usage: git-cli worktree dirty-snapshot [--format text|json]");
        return 0;
    }
    if !args.is_empty() {
        return emit_error(
            "worktree.dirty-snapshot",
            format,
            CliError::usage(
                "unexpected-argument",
                "worktree dirty-snapshot accepts no positional arguments",
            ),
        );
    }
    match env::current_dir()
        .context("current checkout could not be resolved")
        .and_then(|path| dirty_snapshot_cli(&path))
    {
        Ok(snapshot) => emit_success("worktree.dirty-snapshot", format, &snapshot, || {
            snapshot.snapshot_id.clone()
        }),
        Err(error) => emit_error("worktree.dirty-snapshot", format, adoption_cli_error(error)),
    }
}

pub(super) fn run_adopt_dirty(args: &[String]) -> i32 {
    let requested_format = detect_format(args);
    let parsed = match parse_adopt_args(args) {
        Ok(Some(parsed)) => parsed,
        Ok(None) => return 0,
        Err(error) => return emit_error("worktree.adopt-dirty", requested_format, error),
    };
    if !feature_enabled() {
        return emit_error(
            "worktree.adopt-dirty",
            parsed.format,
            CliError::data(
                "dirty-checkout-adoption-disabled",
                "dirty-checkout adoption is disabled",
            ),
        );
    }
    let result = env::current_dir()
        .context("current checkout could not be resolved")
        .and_then(|checkout| {
            let state_root = resolve_state_root()?;
            adopt_dirty_cli(
                &checkout,
                &state_root,
                &parsed.challenge,
                &parsed.reason_file,
            )
        });
    match result {
        Ok(receipt) => emit_success("worktree.adopt-dirty", parsed.format, &receipt, || {
            receipt.receipt_id.clone()
        }),
        Err(error) => emit_error(
            "worktree.adopt-dirty",
            parsed.format,
            adoption_cli_error(error),
        ),
    }
}

pub(super) fn run_revoke_dirty(args: &[String]) -> i32 {
    let requested_format = detect_format(args);
    let parsed = match parse_revoke_args(args) {
        Ok(Some(parsed)) => parsed,
        Ok(None) => return 0,
        Err(error) => return emit_error("worktree.revoke-dirty", requested_format, error),
    };
    let result = env::current_dir()
        .context("current checkout could not be resolved")
        .and_then(|checkout| {
            let state_root = resolve_state_root()?;
            revoke_dirty(&checkout, &state_root, &parsed.receipt)
        });
    match result {
        Ok(()) => emit_success(
            "worktree.revoke-dirty",
            parsed.format,
            &serde_json::json!({ "revoked": true, "receipt_id": parsed.receipt }),
            || "Revoked matching dirty-checkout adoption".to_string(),
        ),
        Err(error) => emit_error(
            "worktree.revoke-dirty",
            parsed.format,
            adoption_cli_error(error),
        ),
    }
}

struct AdoptArgs {
    challenge: String,
    reason_file: PathBuf,
    format: OutputFormat,
}

struct RevokeArgs {
    receipt: String,
    format: OutputFormat,
}

fn parse_adopt_args(raw: &[String]) -> std::result::Result<Option<AdoptArgs>, CliError> {
    let mut args = raw.to_vec();
    let format = take_format(&mut args).map_err(redact_adopt_parse_error)?;
    if args
        .iter()
        .any(|arg| matches!(arg.as_str(), "-h" | "--help"))
    {
        println!(
            "Usage: git-cli worktree adopt-dirty --challenge <token> --reason-file <path> [--format text|json]"
        );
        return Ok(None);
    }
    let mut challenge = None;
    let mut reason_file = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--challenge" => {
                let value = args.get(index + 1).ok_or_else(|| {
                    CliError::usage("missing-challenge", "--challenge requires a token")
                })?;
                if challenge.replace(value.clone()).is_some() {
                    return Err(CliError::usage(
                        "duplicate-challenge",
                        "--challenge may be supplied only once",
                    ));
                }
                index += 2;
            }
            value if value.starts_with("--challenge=") => {
                let value = value.trim_start_matches("--challenge=");
                if value.is_empty() || challenge.replace(value.to_string()).is_some() {
                    return Err(CliError::usage(
                        "invalid-challenge",
                        "--challenge requires one token",
                    ));
                }
                index += 1;
            }
            "--reason-file" => {
                let value = args.get(index + 1).ok_or_else(|| {
                    CliError::usage("missing-reason-file", "--reason-file requires a path")
                })?;
                if reason_file.replace(PathBuf::from(value)).is_some() {
                    return Err(CliError::usage(
                        "duplicate-reason-file",
                        "--reason-file may be supplied only once",
                    ));
                }
                index += 2;
            }
            value if value.starts_with("--reason-file=") => {
                let value = value.trim_start_matches("--reason-file=");
                if value.is_empty() || reason_file.replace(PathBuf::from(value)).is_some() {
                    return Err(CliError::usage(
                        "invalid-reason-file",
                        "--reason-file requires one path",
                    ));
                }
                index += 1;
            }
            _ => {
                return Err(CliError::usage(
                    "unexpected-argument",
                    "unexpected adopt-dirty argument",
                ));
            }
        }
    }
    Ok(Some(AdoptArgs {
        challenge: challenge
            .ok_or_else(|| CliError::usage("missing-challenge", "--challenge is required"))?,
        reason_file: reason_file
            .ok_or_else(|| CliError::usage("missing-reason-file", "--reason-file is required"))?,
        format,
    }))
}

fn redact_adopt_parse_error(error: CliError) -> CliError {
    let message = match error.code {
        "missing-format" | "invalid-format" => "--format requires text or json",
        _ => "invalid adopt-dirty arguments",
    };
    CliError::usage(error.code, message)
}

fn parse_revoke_args(raw: &[String]) -> std::result::Result<Option<RevokeArgs>, CliError> {
    let mut args = raw.to_vec();
    let format = take_format(&mut args)?;
    if args
        .iter()
        .any(|arg| matches!(arg.as_str(), "-h" | "--help"))
    {
        println!("Usage: git-cli worktree revoke-dirty --receipt <id> [--format text|json]");
        return Ok(None);
    }
    let mut receipt = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--receipt" => {
                let value = args.get(index + 1).ok_or_else(|| {
                    CliError::usage("missing-receipt", "--receipt requires an ID")
                })?;
                if receipt.replace(value.clone()).is_some() {
                    return Err(CliError::usage(
                        "duplicate-receipt",
                        "--receipt may be supplied only once",
                    ));
                }
                index += 2;
            }
            value if value.starts_with("--receipt=") => {
                let value = value.trim_start_matches("--receipt=");
                if value.is_empty() || receipt.replace(value.to_string()).is_some() {
                    return Err(CliError::usage(
                        "invalid-receipt",
                        "--receipt requires one ID",
                    ));
                }
                index += 1;
            }
            value => {
                return Err(CliError::usage(
                    "unexpected-argument",
                    format!("unexpected revoke-dirty argument: {value}"),
                ));
            }
        }
    }
    Ok(Some(RevokeArgs {
        receipt: receipt
            .ok_or_else(|| CliError::usage("missing-receipt", "--receipt is required"))?,
        format,
    }))
}

fn adoption_cli_error(error: anyhow::Error) -> CliError {
    if let Some(domain) = error.downcast_ref::<DirtyCheckoutError>() {
        return if domain.kind.exit_code() == exit::RUNTIME {
            CliError::runtime(domain.code(), domain.to_string())
        } else {
            CliError::data(domain.code(), domain.to_string())
        };
    }
    CliError::runtime("dirty-checkout-command-failed", error.to_string())
}

fn feature_enabled() -> bool {
    matches!(
        env::var("AGENT_RUNTIME_DIRTY_CHECKOUT_ADOPTION").as_deref(),
        Ok("1" | "true" | "TRUE" | "yes" | "YES")
    )
}

fn resolve_state_root() -> Result<PathBuf> {
    if let Some(value) =
        env::var_os("AGENT_RUNTIME_CHECKOUT_LEASE_STATE_HOME").filter(|value| !value.is_empty())
    {
        return Ok(PathBuf::from(value));
    }
    if let Some(value) = env::var_os("AGENT_RUNTIME_STATE_HOME").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(value).join("checkout-leases"));
    }
    if let Some(value) = env::var_os("XDG_STATE_HOME").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(value).join("agent-runtime-kit/checkout-leases"));
    }
    let home = env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .context("runtime state root is unavailable")?;
    Ok(PathBuf::from(home).join(".local/state/agent-runtime-kit/checkout-leases"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_process_absent(pid: libc::pid_t, label: &str) {
        let deadline = Instant::now() + Duration::from_millis(500);
        loop {
            let status = unsafe { libc::kill(pid, 0) };
            if status == -1 && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "{label} process {pid} is still present"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn snapshot_worker_process_is_reaped_at_the_hard_deadline() {
        let root = tempfile::TempDir::new().expect("snapshot worker root");
        let pid_path = root.path().join("worker.pid");
        let child_pid_path = root.path().join("child.pid");
        let script = format!(
            "printf %s $$ > '{}'; sleep 60 & printf %s $! > '{}'; while :; do :; done",
            pid_path.display(),
            child_pid_path.display()
        );
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", &script])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);

        let error = run_snapshot_worker_command(
            &mut command,
            deadline_after(Duration::from_millis(100)).expect("worker deadline"),
        )
        .expect_err("blocked snapshot worker must hit the hard deadline");

        assert_eq!(
            error
                .downcast_ref::<DirtyCheckoutError>()
                .expect("typed snapshot timeout")
                .kind(),
            DirtyCheckoutErrorKind::Timeout
        );
        let pid: libc::pid_t = fs::read_to_string(pid_path)
            .expect("read worker pid")
            .parse()
            .expect("parse worker pid");
        let child_pid: libc::pid_t = fs::read_to_string(child_pid_path)
            .expect("read worker child pid")
            .parse()
            .expect("parse worker child pid");
        assert_process_absent(pid, "timed-out snapshot worker");
        assert_process_absent(child_pid, "timed-out snapshot worker child");
    }

    #[test]
    fn successful_git_output_reaps_a_surviving_process_group_child() {
        let root = tempfile::TempDir::new().expect("surviving child root");
        let pid_path = root.path().join("child.pid");
        let script = format!(
            "sleep 60 </dev/null >/dev/null 2>&1 & printf %s $! > '{}'",
            pid_path.display()
        );
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", &script])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);

        let output = output_with_limits(&mut command, Duration::from_secs(1), 1024, 1024)
            .expect("successful leader must return after reaping its process group");

        assert!(output.status.success());
        let pid: libc::pid_t = fs::read_to_string(pid_path)
            .expect("read surviving child pid")
            .parse()
            .expect("parse surviving child pid");
        assert_process_absent(pid, "successful command child");
    }

    #[test]
    fn git_output_deadline_does_not_join_a_descendant_held_pipe() {
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "while :; do :; done & exit 0"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        let started = Instant::now();

        let error = output_with_limits(&mut command, Duration::from_millis(100), 1024, 1024)
            .expect_err("descendant-held pipe must hit the hard deadline");

        assert!(
            started.elapsed() < Duration::from_secs(2),
            "hard deadline was not enforced: {:?}",
            started.elapsed()
        );
        assert!(error.to_string().contains("time limit"));
    }

    #[test]
    fn git_output_collection_enforces_an_aggregate_stream_budget() {
        let mut command = Command::new("/bin/sh");
        command
            .args([
                "-c",
                "i=0; while [ $i -lt 700 ]; do printf x; i=$((i + 1)); done; i=0; while [ $i -lt 700 ]; do printf y >&2; i=$((i + 1)); done",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);

        let error = output_with_limits(&mut command, Duration::from_secs(1), 1024, 1024)
            .expect_err("combined stdout and stderr must share an aggregate budget");

        assert!(error.to_string().contains("size limit"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn detached_pipe_holder_does_not_leave_reader_threads() {
        let root = tempfile::TempDir::new().expect("detached child root");
        let pid_path = root.path().join("detached.pid");
        let script = format!(
            "/usr/bin/python3 -c 'import os,time; os.setsid(); open(\"{}\", \"w\").write(str(os.getpid())); time.sleep(60)'",
            pid_path.display()
        );
        let reader_threads = || {
            fs::read_dir("/proc/self/task")
                .expect("list process threads")
                .filter_map(|entry| entry.ok())
                .filter(|entry| {
                    fs::read_to_string(entry.path().join("comm"))
                        .is_ok_and(|name| name.starts_with("git-std"))
                })
                .count()
        };
        let baseline = reader_threads();
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", &script])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);

        output_with_limits(&mut command, Duration::from_millis(100), 1024, 1024)
            .expect_err("detached pipe holder must hit the deadline");
        std::thread::sleep(Duration::from_millis(20));
        let after = reader_threads();
        let pid: libc::pid_t = fs::read_to_string(&pid_path)
            .expect("read detached child pid")
            .parse()
            .expect("parse detached child pid");
        unsafe {
            libc::kill(pid, libc::SIGKILL);
        }

        assert_eq!(after, baseline, "timeout leaked output reader threads");
    }

    #[test]
    fn nul_path_parser_enforces_entry_cap_before_owned_allocation() {
        let mut raw = Vec::new();
        for index in 0..=MAX_ENTRY_COUNT {
            raw.extend_from_slice(format!("path-{index}").as_bytes());
            raw.push(0);
        }

        parse_nul_paths(&raw).expect_err("path parser must enforce the entry cap");
    }

    #[test]
    fn aggregate_file_budget_is_checked_before_opening_the_file() {
        let root = tempfile::TempDir::new().expect("snapshot root");
        let path = root.path().join("unreadable");
        fs::write(&path, b"x").expect("write fixture");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o000))
            .expect("make fixture unreadable");
        let mut hasher = FramedHasher::new();
        let mut budget = SnapshotBudget {
            file_bytes: MAX_TOTAL_BYTES,
            ..SnapshotBudget::default()
        };

        let error = hash_worktree_object(
            &mut hasher,
            root.path(),
            b"unreadable",
            false,
            None,
            &mut budget,
            Instant::now() + Duration::from_secs(1),
        )
        .expect_err("remaining aggregate budget must be checked before open");

        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
            .expect("restore fixture permissions");
        assert!(
            error.to_string().contains("total bytes"),
            "unexpected pre-open failure: {error}"
        );
    }

    fn test_git(checkout: &Path, args: &[&str]) {
        let status = Command::new("/usr/bin/git")
            .args(args)
            .current_dir(checkout)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .status()
            .expect("run test Git");
        assert!(status.success(), "test Git failed: {args:?}");
    }

    fn adoption_transaction_fixture() -> (
        tempfile::TempDir,
        tempfile::TempDir,
        PathBuf,
        DirtySnapshot,
        PathBuf,
    ) {
        let checkout = tempfile::TempDir::new().expect("adoption checkout");
        test_git(checkout.path(), &["init", "-q"]);
        test_git(checkout.path(), &["config", "user.name", "Test User"]);
        test_git(
            checkout.path(),
            &["config", "user.email", "test@example.com"],
        );
        fs::write(checkout.path().join("tracked.txt"), b"base\n").expect("write tracked file");
        test_git(checkout.path(), &["add", "tracked.txt"]);
        test_git(checkout.path(), &["commit", "-qm", "base"]);
        fs::write(checkout.path().join("dirty.txt"), b"initial dirty state\n")
            .expect("write dirty file");
        let snapshot = dirty_snapshot(checkout.path()).expect("snapshot adoption fixture");

        let state_root = tempfile::TempDir::new().expect("adoption state root");
        fs::set_permissions(state_root.path(), fs::Permissions::from_mode(0o700))
            .expect("make state root private");
        let checkout_dir = state_root
            .path()
            .join(&snapshot.repository_key)
            .join(&snapshot.checkout_key);
        private_directory(checkout_dir.parent().expect("repository state directory"))
            .expect("create repository state directory");
        private_directory(&checkout_dir).expect("create checkout state directory");
        let challenge_dir = checkout_dir.join("challenges");
        private_directory(&challenge_dir).expect("create challenge directory");

        let token = "a".repeat(64);
        let challenge_digest = sha256_hex(token.as_bytes());
        let now = unix_time().expect("challenge time");
        let challenge = ChallengeRecord {
            schema: CHALLENGE_SCHEMA.to_string(),
            token_digest: challenge_digest.clone(),
            session_key: "b".repeat(64),
            repository_key: snapshot.repository_key.clone(),
            checkout_key: snapshot.checkout_key.clone(),
            checkout_instance: snapshot.checkout_instance.clone(),
            snapshot_id: snapshot.snapshot_id.clone(),
            head_oid: snapshot.head_oid.clone(),
            branch_ref_digest: snapshot.branch_ref_digest.clone(),
            authorization_turn_digest: "c".repeat(64),
            issued_at: now,
            expires_at: now + 300,
        };
        let challenge_path = challenge_dir.join(format!("{challenge_digest}.json"));
        write_json_atomic(&challenge_path, &challenge, false).expect("write challenge");
        let reason_path = state_root.path().join("reason.txt");
        fs::write(&reason_path, b"Preserve user-owned dirty state.\n").expect("write reason");
        fs::set_permissions(&reason_path, fs::Permissions::from_mode(0o600))
            .expect("make reason private");

        (checkout, state_root, reason_path, snapshot, challenge_path)
    }

    #[test]
    fn adoption_rechecks_the_complete_snapshot_after_durable_preparation() {
        let (checkout, state_root, reason_path, snapshot, challenge_path) =
            adoption_transaction_fixture();
        let checkout_dir = checkout_state_dir(
            state_root.path(),
            &resolve_checkout(
                checkout.path(),
                false,
                deadline_after(SNAPSHOT_TIMEOUT).expect("identity deadline"),
            )
            .expect("resolve identity"),
        )
        .expect("resolve checkout state");
        let mut calls = 0;

        let error = adopt_dirty_with_snapshot(
            checkout.path(),
            state_root.path(),
            &"a".repeat(64),
            &reason_path,
            |path| {
                calls += 1;
                if calls == 3 {
                    fs::write(path.join("dirty.txt"), b"drift before acceptance\n")
                        .expect("introduce pre-acceptance drift");
                }
                dirty_snapshot(path)
            },
        )
        .expect_err("pre-acceptance snapshot drift must reject adoption");

        assert_eq!(calls, 3);
        assert_eq!(
            error
                .downcast_ref::<DirtyCheckoutError>()
                .expect("typed pre-acceptance drift")
                .kind(),
            DirtyCheckoutErrorKind::ChallengeDrift
        );
        assert!(challenge_path.exists(), "challenge must remain reusable");
        assert!(!checkout_dir.join("lease.json").exists());
        assert!(
            fs::read_dir(checkout_dir.join("receipts"))
                .expect("list receipt directory")
                .next()
                .is_none(),
            "failed preparation must roll back transition artifacts"
        );
        assert_eq!(snapshot.snapshot_id.len(), 64);
    }

    #[test]
    fn adoption_rolls_back_when_the_post_install_snapshot_drifts() {
        let (checkout, state_root, reason_path, snapshot, challenge_path) =
            adoption_transaction_fixture();
        let checkout_dir = state_root
            .path()
            .join(&snapshot.repository_key)
            .join(&snapshot.checkout_key);
        let mut calls = 0;

        let error = adopt_dirty_with_snapshot(
            checkout.path(),
            state_root.path(),
            &"a".repeat(64),
            &reason_path,
            |path| {
                calls += 1;
                if calls == 4 {
                    fs::write(path.join("dirty.txt"), b"drift after lease install\n")
                        .expect("introduce post-install drift");
                }
                dirty_snapshot(path)
            },
        )
        .expect_err("post-install snapshot drift must roll back adoption");

        assert_eq!(calls, 4);
        assert_eq!(
            error
                .downcast_ref::<DirtyCheckoutError>()
                .expect("typed post-install drift")
                .kind(),
            DirtyCheckoutErrorKind::ChallengeDrift
        );
        assert!(
            !challenge_path.exists(),
            "consumed challenge must stay spent"
        );
        assert!(!checkout_dir.join("lease.json").exists());
        let tombstones = fs::read_dir(&checkout_dir)
            .expect("list checkout state")
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_name().as_bytes().starts_with(b".revoked-"))
            .count();
        assert_eq!(tombstones, 1, "rollback must retain one tombstone");
        assert!(
            fs::read_dir(checkout_dir.join("receipts"))
                .expect("list receipt directory")
                .next()
                .is_none(),
            "revoked recovery must remove receipt and spent challenge"
        );

        let replay = adopt_dirty(
            checkout.path(),
            state_root.path(),
            &"a".repeat(64),
            &reason_path,
        )
        .expect_err("rolled-back challenge must not be replayable");
        assert_eq!(
            replay
                .downcast_ref::<DirtyCheckoutError>()
                .expect("typed challenge replay")
                .kind(),
            DirtyCheckoutErrorKind::ChallengeReused
        );
    }

    #[test]
    fn adoption_boundary_rechecks_expiry_and_git_operation_markers() {
        let root = tempfile::TempDir::new().expect("boundary root");
        let checkout = root.path().join("checkout");
        let git_dir = root.path().join("git-dir");
        fs::create_dir(&checkout).expect("create checkout");
        fs::create_dir(&git_dir).expect("create git dir");
        let identity = CheckoutIdentity {
            root: checkout,
            git_dir: git_dir.clone(),
            common_dir: git_dir.clone(),
            repository_key: "a".repeat(64),
            checkout_key: "b".repeat(64),
            checkout_instance: "c".repeat(32),
        };
        let challenge = ChallengeRecord {
            schema: CHALLENGE_SCHEMA.to_string(),
            token_digest: "d".repeat(64),
            session_key: "e".repeat(64),
            repository_key: identity.repository_key.clone(),
            checkout_key: identity.checkout_key.clone(),
            checkout_instance: identity.checkout_instance.clone(),
            snapshot_id: "f".repeat(64),
            head_oid: "1".repeat(40),
            branch_ref_digest: "2".repeat(64),
            authorization_turn_digest: "3".repeat(64),
            issued_at: 100,
            expires_at: 101,
        };

        validate_adoption_boundary(&challenge, &identity, 100)
            .expect("challenge starts valid at the final boundary");
        let expired = validate_adoption_boundary(&challenge, &identity, 101)
            .expect_err("challenge must be rechecked at transition time");
        assert_eq!(
            expired
                .downcast_ref::<DirtyCheckoutError>()
                .expect("typed expiry")
                .kind(),
            DirtyCheckoutErrorKind::ChallengeExpired
        );

        let marker = validate_adoption_precommit_with(
            &challenge,
            &identity,
            || {
                fs::write(git_dir.join("index.lock"), b"")
                    .context("write operation marker at precommit barrier")?;
                Ok(())
            },
            || Ok(100),
        )
        .expect_err("operation marker introduced at the barrier must fail closed");
        assert_eq!(
            marker
                .downcast_ref::<DirtyCheckoutError>()
                .expect("typed operation error")
                .kind(),
            DirtyCheckoutErrorKind::UnsupportedGitState
        );
        fs::remove_file(git_dir.join("index.lock")).expect("remove operation marker");

        let expired_at_barrier =
            validate_adoption_precommit_with(&challenge, &identity, || Ok(()), || Ok(101))
                .expect_err("challenge expiring at the precommit barrier must fail closed");
        assert_eq!(
            expired_at_barrier
                .downcast_ref::<DirtyCheckoutError>()
                .expect("typed barrier expiry")
                .kind(),
            DirtyCheckoutErrorKind::ChallengeExpired
        );
    }

    #[test]
    fn predecessor_cleanup_failure_is_reported_for_recovery() {
        let root = tempfile::TempDir::new().expect("predecessor cleanup root");
        let receipt_path = root.path().join("receipt.json");
        let spent_challenge_path = root.path().join("spent.json");
        fs::create_dir(&receipt_path).expect("create non-removable receipt fixture");
        fs::write(&spent_challenge_path, b"spent").expect("write spent fixture");
        let predecessor = ExpiredPredecessor {
            receipt_id: "a".repeat(64),
            receipt_path,
            spent_challenge_path,
            receipt_digest: "b".repeat(64),
            spent_challenge_digest: "c".repeat(64),
        };

        cleanup_expired_predecessor(&predecessor)
            .expect_err("predecessor cleanup failure must not be silent");
        assert!(
            predecessor.spent_challenge_path.exists(),
            "cleanup must stop without forgetting the remaining predecessor state"
        );
    }

    #[test]
    fn pending_adoption_recovery_covers_preinstall_commit_and_rollback_faults() {
        let root = tempfile::TempDir::new().expect("pending recovery root");
        let checkout = root.path().join("checkout");
        let git_dir = root.path().join("git-dir");
        let checkout_dir = root.path().join("state");
        let challenge_dir = checkout_dir.join("challenges");
        let receipts_dir = checkout_dir.join("receipts");
        for directory in [
            checkout.as_path(),
            git_dir.as_path(),
            checkout_dir.as_path(),
            challenge_dir.as_path(),
            receipts_dir.as_path(),
        ] {
            fs::create_dir(directory).expect("create recovery directory");
            fs::set_permissions(directory, fs::Permissions::from_mode(0o700))
                .expect("make recovery directory private");
        }
        let identity = CheckoutIdentity {
            root: checkout,
            git_dir: git_dir.clone(),
            common_dir: git_dir,
            repository_key: "a".repeat(64),
            checkout_key: "b".repeat(64),
            checkout_instance: "c".repeat(32),
        };
        let challenge_digest = "d".repeat(64);
        let receipt_id = "e".repeat(64);
        let predecessor_receipt_id = "f".repeat(64);
        let challenge = ChallengeRecord {
            schema: CHALLENGE_SCHEMA.to_string(),
            token_digest: challenge_digest.clone(),
            session_key: "1".repeat(64),
            repository_key: identity.repository_key.clone(),
            checkout_key: identity.checkout_key.clone(),
            checkout_instance: identity.checkout_instance.clone(),
            snapshot_id: "2".repeat(64),
            head_oid: "3".repeat(40),
            branch_ref_digest: "4".repeat(64),
            authorization_turn_digest: "5".repeat(64),
            issued_at: 10,
            expires_at: 20,
        };
        let receipt = ReceiptRecord {
            schema: RECEIPT_SCHEMA.to_string(),
            receipt_id: receipt_id.clone(),
            session_key: challenge.session_key.clone(),
            repository_key: identity.repository_key.clone(),
            checkout_key: identity.checkout_key.clone(),
            checkout_instance: identity.checkout_instance.clone(),
            snapshot_id: challenge.snapshot_id.clone(),
            authorization_turn_digest: challenge.authorization_turn_digest.clone(),
            reason_digest: "6".repeat(64),
            challenge_digest: challenge_digest.clone(),
            adopted_at: 15,
        };
        let pending_path = pending_adoption_path(&checkout_dir, &challenge_digest);
        let challenge_path = challenge_dir.join(format!("{challenge_digest}.json"));
        let receipt_path = receipts_dir.join(format!("{receipt_id}.json"));
        let spent_path = receipts_dir.join(format!(".challenge-{receipt_id}.json"));
        let pending = PendingAdoptionRecord {
            schema: PENDING_ADOPTION_SCHEMA.to_string(),
            receipt_id: receipt_id.clone(),
            challenge_digest: challenge_digest.clone(),
            session_key: challenge.session_key.clone(),
            checkout_instance: identity.checkout_instance.clone(),
            snapshot_id: challenge.snapshot_id.clone(),
            predecessor_receipt_id: None,
            predecessor_receipt_digest: None,
            predecessor_spent_challenge_digest: None,
        };

        write_json_atomic(&challenge_path, &challenge, false).expect("write challenge");
        write_json_atomic(&receipt_path, &receipt, false).expect("write prepared receipt");
        write_json_atomic(&pending_path, &pending, false).expect("write pending marker");
        fs::rename(&challenge_path, &spent_path).expect("simulate consumed challenge");
        assert_eq!(
            recover_pending_adoption(&checkout_dir, &challenge_dir, &identity, &challenge_digest,)
                .expect("roll back preinstall transition"),
            PendingRecovery::RolledBack
        );
        assert!(challenge_path.exists());
        assert!(!spent_path.exists());
        assert!(!receipt_path.exists());
        assert!(!pending_path.exists());

        write_json_atomic(&receipt_path, &receipt, false).expect("rewrite committed receipt");
        write_json_atomic(&pending_path, &pending, false).expect("rewrite pending marker");
        fs::rename(&challenge_path, &spent_path).expect("consume committed challenge");
        let lease = LeaseRecord::V2(Box::new(LeaseV2Wire {
            schema: LEASE_V2_SCHEMA.to_string(),
            session_key: challenge.session_key.clone(),
            checkout_instance: identity.checkout_instance.clone(),
            checkout_root: identity.root.to_str().expect("UTF-8 root").to_string(),
            checkout_git_dir: identity
                .git_dir
                .to_str()
                .expect("UTF-8 Git dir")
                .to_string(),
            checkout_root_bytes: hex_bytes(identity.root.as_os_str().as_bytes()),
            checkout_git_dir_bytes: hex_bytes(identity.git_dir.as_os_str().as_bytes()),
            acquired_at: 15,
            refreshed_at: 15,
            expires_at: 30,
            adoption: AdoptionRecord {
                schema: ADOPTION_SCHEMA.to_string(),
                receipt_schema: RECEIPT_SCHEMA.to_string(),
                receipt_id: receipt_id.clone(),
                snapshot_id: challenge.snapshot_id.clone(),
                authorization_turn_digest: challenge.authorization_turn_digest.clone(),
                reason_digest: receipt.reason_digest.clone(),
                adopted_at: 15,
                challenge_issued_at: challenge.issued_at,
                challenge_digest: challenge_digest.clone(),
            },
        }));
        write_json_atomic(&checkout_dir.join("lease.json"), &lease, false)
            .expect("write committed lease");
        assert_eq!(
            recover_pending_adoption(&checkout_dir, &challenge_dir, &identity, &challenge_digest,)
                .expect("finish committed transition"),
            PendingRecovery::Committed
        );
        assert!(!pending_path.exists());
        assert!(receipt_path.exists());
        assert!(spent_path.exists());

        let committed_pending = PendingAdoptionRecord {
            predecessor_receipt_id: Some(predecessor_receipt_id.clone()),
            predecessor_receipt_digest: Some("7".repeat(64)),
            predecessor_spent_challenge_digest: Some("8".repeat(64)),
            ..pending
        };
        let predecessor_receipt_path = receipts_dir.join(format!("{predecessor_receipt_id}.json"));
        let predecessor_spent_path =
            receipts_dir.join(format!(".challenge-{predecessor_receipt_id}.json"));
        fs::write(&predecessor_receipt_path, b"unbound predecessor receipt")
            .expect("write unbound predecessor receipt");
        fs::write(&predecessor_spent_path, b"unbound predecessor challenge")
            .expect("write unbound predecessor challenge");
        write_json_atomic(&pending_path, &committed_pending, false)
            .expect("write unbound predecessor marker");

        let unbound =
            recover_pending_adoption(&checkout_dir, &challenge_dir, &identity, &challenge_digest)
                .expect_err("unbound predecessor artifacts must fail closed");
        assert_eq!(
            unbound
                .downcast_ref::<DirtyCheckoutError>()
                .expect("typed unbound predecessor error")
                .kind(),
            DirtyCheckoutErrorKind::MalformedState
        );
        assert!(pending_path.exists());
        assert!(predecessor_receipt_path.exists());
        assert!(predecessor_spent_path.exists());
        fs::remove_file(&pending_path).expect("remove unbound predecessor marker");
        fs::remove_file(&predecessor_receipt_path).expect("remove unbound predecessor receipt");
        fs::remove_file(&predecessor_spent_path).expect("remove unbound predecessor challenge");

        fs::remove_file(checkout_dir.join("lease.json")).expect("remove committed lease");
        write_json_atomic(&pending_path, &committed_pending, false)
            .expect("write rollback-failure marker");
        write_json_atomic(&challenge_path, &challenge, false)
            .expect("create duplicate current challenge");
        let error =
            recover_pending_adoption(&checkout_dir, &challenge_dir, &identity, &challenge_digest)
                .expect_err("duplicate challenge state must retain recovery marker");
        assert_eq!(
            error
                .downcast_ref::<DirtyCheckoutError>()
                .expect("typed rollback failure")
                .kind(),
            DirtyCheckoutErrorKind::MalformedState
        );
        assert!(pending_path.exists());
    }

    #[test]
    fn lease_install_fault_classification_compares_complete_validated_state() {
        let root = tempfile::TempDir::new().expect("lease state root");
        let checkout = root.path().join("checkout");
        let git_dir = root.path().join("git-dir");
        fs::create_dir(&checkout).expect("create checkout");
        fs::create_dir(&git_dir).expect("create git dir");
        let identity = CheckoutIdentity {
            root: checkout,
            git_dir: git_dir.clone(),
            common_dir: git_dir,
            repository_key: "a".repeat(64),
            checkout_key: "b".repeat(64),
            checkout_instance: "c".repeat(32),
        };
        let expected = LeaseRecord::V2(Box::new(LeaseV2Wire {
            schema: LEASE_V2_SCHEMA.to_string(),
            session_key: "d".repeat(64),
            checkout_instance: identity.checkout_instance.clone(),
            checkout_root: identity.root.to_str().expect("UTF-8 root").to_string(),
            checkout_git_dir: identity
                .git_dir
                .to_str()
                .expect("UTF-8 Git dir")
                .to_string(),
            checkout_root_bytes: hex_bytes(identity.root.as_os_str().as_bytes()),
            checkout_git_dir_bytes: hex_bytes(identity.git_dir.as_os_str().as_bytes()),
            acquired_at: 10,
            refreshed_at: 20,
            expires_at: 30,
            adoption: AdoptionRecord {
                schema: ADOPTION_SCHEMA.to_string(),
                receipt_schema: RECEIPT_SCHEMA.to_string(),
                receipt_id: "e".repeat(64),
                snapshot_id: "f".repeat(64),
                authorization_turn_digest: "1".repeat(64),
                reason_digest: "2".repeat(64),
                adopted_at: 20,
                challenge_issued_at: 10,
                challenge_digest: "3".repeat(64),
            },
        }));
        let lease_path = root.path().join("lease.json");

        assert!(matches!(
            install_lease_with(&lease_path, &expected, None, &identity, || Err(
                anyhow::anyhow!("injected pre-install failure")
            )),
            LeaseInstallOutcome::NotInstalled(_)
        ));

        assert!(matches!(
            install_lease_with(&lease_path, &expected, None, &identity, || {
                write_json_atomic(&lease_path, &expected, true)?;
                Err(anyhow::anyhow!("injected post-install durability failure"))
            }),
            LeaseInstallOutcome::Ambiguous(_)
        ));

        let mut different = expected.clone();
        let LeaseRecord::V2(different) = &mut different else {
            panic!("expected v2 lease");
        };
        different.adoption.reason_digest = "4".repeat(64);
        assert!(matches!(
            install_lease_with(&lease_path, &expected, None, &identity, || {
                write_json_atomic(&lease_path, &different, true)?;
                Err(anyhow::anyhow!("injected ambiguous durability failure"))
            }),
            LeaseInstallOutcome::Ambiguous(_)
        ));
    }

    #[test]
    fn strict_wire_round_trips_and_rejects_duplicate_or_unknown_fields() {
        let root = tempfile::TempDir::new().expect("wire root");
        let checkout = root.path().join("checkout");
        let git_dir = root.path().join("git-dir");
        fs::create_dir(&checkout).expect("create checkout");
        fs::create_dir(&git_dir).expect("create git dir");
        let identity = CheckoutIdentity {
            root: checkout,
            git_dir: git_dir.clone(),
            common_dir: git_dir,
            repository_key: "a".repeat(64),
            checkout_key: "b".repeat(64),
            checkout_instance: "c".repeat(32),
        };
        let adoption = AdoptionRecord {
            schema: ADOPTION_SCHEMA.to_string(),
            receipt_schema: RECEIPT_SCHEMA.to_string(),
            receipt_id: "d".repeat(64),
            snapshot_id: "e".repeat(64),
            authorization_turn_digest: "f".repeat(64),
            reason_digest: "1".repeat(64),
            adopted_at: 20,
            challenge_issued_at: 10,
            challenge_digest: "2".repeat(64),
        };
        let v2 = LeaseRecord::V2(Box::new(LeaseV2Wire {
            schema: LEASE_V2_SCHEMA.to_string(),
            session_key: "3".repeat(64),
            checkout_instance: identity.checkout_instance.clone(),
            checkout_root: identity.root.to_str().expect("UTF-8 root").to_string(),
            checkout_git_dir: identity
                .git_dir
                .to_str()
                .expect("UTF-8 Git dir")
                .to_string(),
            checkout_root_bytes: hex_bytes(identity.root.as_os_str().as_bytes()),
            checkout_git_dir_bytes: hex_bytes(identity.git_dir.as_os_str().as_bytes()),
            acquired_at: 10,
            refreshed_at: 20,
            expires_at: 30,
            adoption: adoption.clone(),
        }));
        let v2_path = root.path().join("v2.json");
        write_json_atomic(&v2_path, &v2, false).expect("write canonical v2");
        assert_eq!(load_lease(&v2_path).expect("load v2"), Some(v2));

        let v1 = LeaseRecord::V1(LeaseV1Wire {
            schema: LEASE_V1_SCHEMA.to_string(),
            session_key: "4".repeat(64),
            checkout_instance: identity.checkout_instance.clone(),
            checkout_root: identity.root.to_str().expect("UTF-8 root").to_string(),
            checkout_git_dir: identity
                .git_dir
                .to_str()
                .expect("UTF-8 Git dir")
                .to_string(),
            acquired_at: 10,
            refreshed_at: 20,
            expires_at: 30,
        });
        let v1_path = root.path().join("v1.json");
        write_json_atomic(&v1_path, &v1, false).expect("write canonical v1");
        assert_eq!(load_lease(&v1_path).expect("load v1"), Some(v1));

        let duplicate_path = root.path().join("duplicate.json");
        let duplicate = format!(
            "{{\"schema\":\"{LEASE_V1_SCHEMA}\",\"session_key\":\"{}\",\"session_key\":\"{}\",\"checkout_instance\":\"{}\",\"checkout_root\":{},\"checkout_git_dir\":{},\"acquired_at\":10,\"refreshed_at\":20,\"expires_at\":30}}\n",
            "4".repeat(64),
            "5".repeat(64),
            identity.checkout_instance,
            serde_json::to_string(identity.root.to_str().expect("UTF-8 root")).expect("root JSON"),
            serde_json::to_string(identity.git_dir.to_str().expect("UTF-8 Git dir"))
                .expect("Git-dir JSON")
        );
        fs::write(&duplicate_path, duplicate).expect("write duplicate-key lease");
        fs::set_permissions(&duplicate_path, fs::Permissions::from_mode(0o600))
            .expect("make duplicate-key lease private");
        load_lease(&duplicate_path).expect_err("duplicate known fields must be rejected");

        let mut unknown_adoption = serde_json::to_value(&adoption).expect("serialize adoption");
        unknown_adoption["unexpected"] = serde_json::json!(true);
        serde_json::from_value::<AdoptionRecord>(unknown_adoption)
            .expect_err("unknown adoption field must be rejected");
        let mut missing_receipt_schema =
            serde_json::to_value(&adoption).expect("serialize adoption");
        missing_receipt_schema
            .as_object_mut()
            .expect("adoption object")
            .remove("receipt_schema");
        serde_json::from_value::<AdoptionRecord>(missing_receipt_schema)
            .expect_err("receipt_schema must be required");

        let receipt = ReceiptRecord {
            schema: RECEIPT_SCHEMA.to_string(),
            receipt_id: adoption.receipt_id,
            session_key: "3".repeat(64),
            repository_key: identity.repository_key,
            checkout_key: identity.checkout_key,
            checkout_instance: identity.checkout_instance,
            snapshot_id: adoption.snapshot_id,
            authorization_turn_digest: adoption.authorization_turn_digest,
            reason_digest: adoption.reason_digest,
            challenge_digest: adoption.challenge_digest,
            adopted_at: adoption.adopted_at,
        };
        let encoded_receipt = serde_json::to_vec(&receipt).expect("serialize receipt");
        let decoded_receipt: ReceiptRecord =
            serde_json::from_slice(&encoded_receipt).expect("round-trip receipt");
        assert_eq!(decoded_receipt.receipt_id, receipt.receipt_id);
        let mut unknown_receipt = serde_json::to_value(&receipt).expect("serialize receipt");
        unknown_receipt["unexpected"] = serde_json::json!(true);
        serde_json::from_value::<ReceiptRecord>(unknown_receipt)
            .expect_err("unknown receipt field must be rejected");
    }

    #[test]
    fn canonical_json_fixtures_match_producers_and_cross_file_contracts() {
        const ROOT: &str = "/workspace/agent-runtime-kit";
        const GIT_DIR: &str = "/workspace/agent-runtime-kit/.git/worktrees/dirty-adoption";
        const ROOT_BYTES: &str = "2f776f726b73706163652f6167656e742d72756e74696d652d6b6974";
        const GIT_DIR_BYTES: &str = "2f776f726b73706163652f6167656e742d72756e74696d652d6b69742f2e6769742f776f726b74726565732f64697274792d61646f7074696f6e";

        let snapshot_fixture = include_str!(
            "../../tests/fixtures/dirty-checkout-adoption/dirty-checkout-snapshot-v1.json"
        );
        let challenge_fixture = include_str!(
            "../../tests/fixtures/dirty-checkout-adoption/dirty-checkout-challenge-v1.json"
        );
        let receipt_fixture = include_str!(
            "../../tests/fixtures/dirty-checkout-adoption/dirty-checkout-receipt-v1.json"
        );
        let lease_v1_fixture =
            include_str!("../../tests/fixtures/dirty-checkout-adoption/checkout-lease-v1.json");
        let lease_v2_fixture =
            include_str!("../../tests/fixtures/dirty-checkout-adoption/checkout-lease-v2.json");
        let adoption_fixture = include_str!(
            "../../tests/fixtures/dirty-checkout-adoption/dirty-checkout-adoption-v1.json"
        );
        let fixture_value = |source: &str| {
            serde_json::from_str::<serde_json::Value>(source).expect("parse canonical fixture")
        };

        let expected_snapshot = DirtySnapshot {
            schema: SNAPSHOT_SCHEMA,
            repository_key: "a".repeat(64),
            checkout_key: "b".repeat(64),
            checkout_instance: "c".repeat(32),
            snapshot_id: "d".repeat(64),
            head_oid: "e".repeat(40),
            branch_ref_digest: "f".repeat(64),
            tracked_entries: 2,
            untracked_entries: 1,
            hashed_bytes: 4096,
        };
        assert_eq!(
            serde_json::to_value(&expected_snapshot).expect("serialize snapshot producer"),
            fixture_value(snapshot_fixture)
        );
        let snapshot: SnapshotWorkerSnapshot =
            serde_json::from_str(snapshot_fixture).expect("strict snapshot fixture");

        let expected_challenge = ChallengeRecord {
            schema: CHALLENGE_SCHEMA.to_string(),
            token_digest: "1".repeat(64),
            session_key: "2".repeat(64),
            repository_key: "a".repeat(64),
            checkout_key: "b".repeat(64),
            checkout_instance: "c".repeat(32),
            snapshot_id: "d".repeat(64),
            head_oid: "e".repeat(40),
            branch_ref_digest: "f".repeat(64),
            authorization_turn_digest: "3".repeat(64),
            issued_at: 1_700_000_000,
            expires_at: 1_700_000_300,
        };
        assert_eq!(
            serde_json::to_value(&expected_challenge).expect("serialize challenge producer"),
            fixture_value(challenge_fixture)
        );
        let challenge: ChallengeRecord =
            serde_json::from_str(challenge_fixture).expect("strict challenge fixture");

        let expected_receipt = ReceiptRecord {
            schema: RECEIPT_SCHEMA.to_string(),
            receipt_id: "4".repeat(64),
            session_key: "2".repeat(64),
            repository_key: "a".repeat(64),
            checkout_key: "b".repeat(64),
            checkout_instance: "c".repeat(32),
            snapshot_id: "d".repeat(64),
            authorization_turn_digest: "3".repeat(64),
            reason_digest: "5".repeat(64),
            challenge_digest: "6".repeat(64),
            adopted_at: 1_700_000_100,
        };
        assert_eq!(
            serde_json::to_value(&expected_receipt).expect("serialize receipt producer"),
            fixture_value(receipt_fixture)
        );
        let receipt: ReceiptRecord =
            serde_json::from_str(receipt_fixture).expect("strict receipt fixture");

        let expected_adoption = AdoptionRecord {
            schema: ADOPTION_SCHEMA.to_string(),
            receipt_schema: RECEIPT_SCHEMA.to_string(),
            receipt_id: "4".repeat(64),
            snapshot_id: "d".repeat(64),
            authorization_turn_digest: "3".repeat(64),
            reason_digest: "5".repeat(64),
            adopted_at: 1_700_000_100,
            challenge_issued_at: 1_700_000_000,
            challenge_digest: "6".repeat(64),
        };
        assert_eq!(
            serde_json::to_value(&expected_adoption).expect("serialize adoption producer"),
            fixture_value(adoption_fixture)
        );
        let adoption: AdoptionRecord =
            serde_json::from_str(adoption_fixture).expect("strict adoption fixture");

        let expected_v1 = LeaseRecord::V1(LeaseV1Wire {
            schema: LEASE_V1_SCHEMA.to_string(),
            session_key: "2".repeat(64),
            checkout_instance: "c".repeat(32),
            checkout_root: ROOT.to_string(),
            checkout_git_dir: GIT_DIR.to_string(),
            acquired_at: 1_699_999_000,
            refreshed_at: 1_700_000_000,
            expires_at: 1_700_003_600,
        });
        assert_eq!(
            serde_json::to_value(&expected_v1).expect("serialize v1 lease producer"),
            fixture_value(lease_v1_fixture)
        );
        let lease_v1 = parse_lease(lease_v1_fixture.as_bytes()).expect("strict v1 lease fixture");
        assert_eq!(lease_v1, expected_v1);

        let expected_v2 = LeaseRecord::V2(Box::new(LeaseV2Wire {
            schema: LEASE_V2_SCHEMA.to_string(),
            session_key: "2".repeat(64),
            checkout_instance: "c".repeat(32),
            checkout_root: ROOT.to_string(),
            checkout_git_dir: GIT_DIR.to_string(),
            checkout_root_bytes: ROOT_BYTES.to_string(),
            checkout_git_dir_bytes: GIT_DIR_BYTES.to_string(),
            acquired_at: 1_700_000_100,
            refreshed_at: 1_700_000_100,
            expires_at: 1_700_003_700,
            adoption: expected_adoption,
        }));
        assert_eq!(
            serde_json::to_value(&expected_v2).expect("serialize v2 lease producer"),
            fixture_value(lease_v2_fixture)
        );
        let lease_v2 = parse_lease(lease_v2_fixture.as_bytes()).expect("strict v2 lease fixture");
        assert_eq!(lease_v2, expected_v2);

        let identity = CheckoutIdentity {
            root: PathBuf::from(ROOT),
            git_dir: PathBuf::from(GIT_DIR),
            common_dir: PathBuf::from(GIT_DIR),
            repository_key: snapshot.repository_key.clone(),
            checkout_key: snapshot.checkout_key.clone(),
            checkout_instance: snapshot.checkout_instance.clone(),
        };
        validate_challenge_identity(&challenge, &identity, &challenge.token_digest)
            .expect("validate challenge fixture");
        validate_receipt(&receipt, &identity, &receipt.receipt_id)
            .expect("validate receipt fixture");
        validate_lease(&lease_v1, &identity).expect("validate v1 lease fixture");
        validate_lease(&lease_v2, &identity).expect("validate v2 lease fixture");

        assert_eq!(challenge.repository_key, snapshot.repository_key);
        assert_eq!(challenge.checkout_key, snapshot.checkout_key);
        assert_eq!(challenge.checkout_instance, snapshot.checkout_instance);
        assert_eq!(challenge.snapshot_id, snapshot.snapshot_id);
        assert_eq!(challenge.head_oid, snapshot.head_oid);
        assert_eq!(challenge.branch_ref_digest, snapshot.branch_ref_digest);
        assert_eq!(receipt.session_key, challenge.session_key);
        assert_eq!(receipt.repository_key, challenge.repository_key);
        assert_eq!(receipt.checkout_key, challenge.checkout_key);
        assert_eq!(receipt.checkout_instance, challenge.checkout_instance);
        assert_eq!(receipt.snapshot_id, challenge.snapshot_id);
        assert_eq!(
            receipt.authorization_turn_digest,
            challenge.authorization_turn_digest
        );
        assert_eq!(adoption.receipt_id, receipt.receipt_id);
        assert_eq!(adoption.snapshot_id, receipt.snapshot_id);
        assert_eq!(
            adoption.authorization_turn_digest,
            receipt.authorization_turn_digest
        );
        assert_eq!(adoption.reason_digest, receipt.reason_digest);
        assert_eq!(adoption.challenge_digest, receipt.challenge_digest);
        assert_eq!(adoption.adopted_at, receipt.adopted_at);
        assert_eq!(adoption.challenge_issued_at, challenge.issued_at);
        assert_eq!(lease_v2.adoption(), Some(&adoption));
        assert_eq!(lease_v2.session_key(), receipt.session_key);
        assert_eq!(lease_v2.checkout_instance(), receipt.checkout_instance);
        assert_eq!(
            decode_lower_hex(ROOT_BYTES).as_deref(),
            Some(ROOT.as_bytes())
        );
        assert_eq!(
            decode_lower_hex(GIT_DIR_BYTES).as_deref(),
            Some(GIT_DIR.as_bytes())
        );
    }

    #[test]
    fn metadata_equality_includes_ctime() {
        let root = tempfile::TempDir::new().expect("metadata root");
        let path = root.path().join("file");
        fs::write(&path, b"content").expect("write fixture");
        let before = fs::metadata(&path).expect("metadata before");
        std::thread::sleep(Duration::from_millis(10));
        fs::set_permissions(&path, before.permissions()).expect("touch ctime with chmod");
        let after = fs::metadata(&path).expect("metadata after");

        assert_eq!(before.mode(), after.mode());
        assert_eq!(before.len(), after.len());
        assert_eq!(before.mtime(), after.mtime());
        assert!(
            !same_metadata(&before, &after),
            "ctime-only drift must invalidate metadata equality"
        );
    }
}
