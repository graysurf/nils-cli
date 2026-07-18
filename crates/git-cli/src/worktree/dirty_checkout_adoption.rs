use super::{CliError, detect_format, emit_error, emit_success, take_format};
use anyhow::{Context, Result, bail, ensure};
use nils_common::cli_contract::{OutputFormat, exit};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
#[cfg(any(test, target_os = "macos"))]
use std::collections::HashMap;
use std::collections::{HashSet, VecDeque};
use std::env;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
#[cfg(test)]
use std::os::unix::fs::PermissionsExt;
use std::os::unix::fs::{DirBuilderExt, FileExt, MetadataExt, OpenOptionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, OnceLock};
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
const MAX_PENDING_STATE_FILE_BYTES: usize = MAX_STATE_FILE_BYTES * 4 + 4 * 1024;
const MAX_REVOCATION_TOMBSTONES: usize = 64;
const MAX_RECOVERY_DIRECTORY_ENTRIES: usize = 1_024;
const MAX_RECOVERY_STATE_ENTRIES: usize = 256;
const MAX_RECOVERY_AGGREGATE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_RECOVERY_NAME_BYTES: usize = 256 * 1024;
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
const PROCESS_SUPERVISOR_ENV: &str = "NILS_GIT_CLI_INTERNAL_PROCESS_SUPERVISOR";
const PROCESS_SUPERVISOR_CAPABILITY_FD_ENV: &str =
    "NILS_GIT_CLI_INTERNAL_PROCESS_SUPERVISOR_CAPABILITY_FD";
const PROCESS_SUPERVISOR_CAPABILITY: &[u8] = b"nils-git-cli-process-supervisor-v1";
const PROCESS_SUPERVISOR_COMPLETION: &[u8] = b"nils-git-cli-process-supervisor-completion-v1";
const PROCESS_SUPERVISOR_COMPLETION_BYTES: usize = PROCESS_SUPERVISOR_COMPLETION.len() + 6;
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
    token_digest: String,
    challenge_digest: String,
    session_key: String,
    checkout_instance: String,
    snapshot_id: String,
    predecessor_receipt_id: Option<String>,
    predecessor_receipt_digest: Option<String>,
    predecessor_spent_challenge_digest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    predecessor_lease_digest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    predecessor_lease_bytes: Option<Vec<u8>>,
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
    traversal_entries: usize,
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

    fn charge_traversal_entry(&mut self) -> Result<()> {
        self.traversal_entries = self.traversal_entries.checked_add(1).ok_or_else(|| {
            domain_error(
                DirtyCheckoutErrorKind::ResourceUnavailable,
                "checkout filesystem entry count overflowed",
            )
        })?;
        if self.traversal_entries > MAX_ENTRY_COUNT {
            return Err(domain_error(
                DirtyCheckoutErrorKind::ResourceUnavailable,
                "checkout filesystem entry count exceeds the supported limit",
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
    dirty_snapshot_cli_until(checkout, deadline_after(SNAPSHOT_TIMEOUT)?)
}

fn dirty_snapshot_cli_until(checkout: &Path, deadline: Instant) -> Result<DirtySnapshot> {
    let executable = snapshot_worker_executable()?;
    executable.revalidate()?;
    ensure_deadline(deadline)?;
    let mut command = Command::new(executable.command_path());
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
        (Some(snapshot), None) if valid_worker_snapshot(&snapshot) => Ok(DirtySnapshot {
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
        (Some(_), None) => Err(domain_error(
            DirtyCheckoutErrorKind::SnapshotFailed,
            "dirty snapshot worker success response is semantically invalid",
        )),
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

fn valid_worker_snapshot(snapshot: &SnapshotWorkerSnapshot) -> bool {
    snapshot.schema == SNAPSHOT_SCHEMA
        && is_lower_hex(&snapshot.repository_key, 64)
        && is_lower_hex(&snapshot.checkout_key, 64)
        && is_lower_hex(&snapshot.checkout_instance, 32)
        && is_lower_hex(&snapshot.snapshot_id, 64)
        && valid_head_identity(&snapshot.head_oid)
        && is_lower_hex(&snapshot.branch_ref_digest, 64)
        && snapshot
            .tracked_entries
            .checked_add(snapshot.untracked_entries)
            .is_some_and(|entries| entries <= MAX_ENTRY_COUNT)
        && snapshot.hashed_bytes <= MAX_TOTAL_BYTES
}

pub(crate) fn run_internal_snapshot_worker() -> Option<i32> {
    if env::var_os(SNAPSHOT_WORKER_ENV).as_deref() != Some(OsStr::new("1")) {
        return None;
    }
    let result = env::var_os(SNAPSHOT_WORKER_CHECKOUT_ENV)
        .map(PathBuf::from)
        .context("dirty snapshot worker checkout is unavailable")
        .and_then(|checkout| {
            let deadline = Instant::now() + SNAPSHOT_TIMEOUT;
            dirty_snapshot_worker(&checkout, deadline)
        });
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

const PROCESS_SUPERVISOR_POLL_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProcessSupervisorCompletionKind {
    Target = 1,
    Terminated = 2,
    InternalFailure = 3,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ProcessSupervisorCompletion {
    kind: ProcessSupervisorCompletionKind,
    cleanup_complete: bool,
    exit_code: i32,
}

fn write_process_supervisor_completion<W: Write>(
    writer: &mut W,
    completion: ProcessSupervisorCompletion,
) -> io::Result<()> {
    let mut frame = Vec::with_capacity(PROCESS_SUPERVISOR_COMPLETION_BYTES);
    frame.extend_from_slice(PROCESS_SUPERVISOR_COMPLETION);
    frame.push(completion.kind as u8);
    frame.push(u8::from(completion.cleanup_complete));
    frame.extend_from_slice(&completion.exit_code.to_be_bytes());
    writer.write_all(&frame)
}

fn read_process_supervisor_completion<R: Read>(
    reader: &mut R,
) -> Result<ProcessSupervisorCompletion> {
    let mut frame = vec![0_u8; PROCESS_SUPERVISOR_COMPLETION_BYTES];
    reader
        .read_exact(&mut frame)
        .context("process supervisor completion is unavailable")?;
    let mut trailing = [0_u8; 1];
    if reader
        .read(&mut trailing)
        .context("process supervisor completion could not be sealed")?
        != 0
        || &frame[..PROCESS_SUPERVISOR_COMPLETION.len()] != PROCESS_SUPERVISOR_COMPLETION
    {
        return Err(process_scan_resource_error());
    }
    let kind_offset = PROCESS_SUPERVISOR_COMPLETION.len();
    let kind = match frame[kind_offset] {
        1 => ProcessSupervisorCompletionKind::Target,
        2 => ProcessSupervisorCompletionKind::Terminated,
        3 => ProcessSupervisorCompletionKind::InternalFailure,
        _ => return Err(process_scan_resource_error()),
    };
    let cleanup_complete = match frame[kind_offset + 1] {
        0 => false,
        1 => true,
        _ => return Err(process_scan_resource_error()),
    };
    let exit_code = i32::from_be_bytes(
        frame[kind_offset + 2..]
            .try_into()
            .map_err(|_| process_scan_resource_error())?,
    );
    Ok(ProcessSupervisorCompletion {
        kind,
        cleanup_complete,
        exit_code,
    })
}

static PROCESS_SUPERVISOR_TERMINATE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

extern "C" fn request_process_supervisor_termination(_signal: libc::c_int) {
    PROCESS_SUPERVISOR_TERMINATE.store(true, std::sync::atomic::Ordering::SeqCst);
}

fn supervisor_parent_is_trusted() -> bool {
    let Ok(current) = env::current_exe() else {
        return false;
    };
    let Ok(current_metadata) = fs::metadata(current) else {
        return false;
    };
    let parent = unsafe { libc::getppid() };

    #[cfg(target_os = "linux")]
    let parent_metadata = fs::metadata(format!("/proc/{parent}/exe"));

    #[cfg(target_os = "macos")]
    let parent_metadata = {
        let mut bytes = vec![0_u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
        let length =
            unsafe { libc::proc_pidpath(parent, bytes.as_mut_ptr().cast(), bytes.len() as u32) };
        if length <= 0 {
            return false;
        }
        bytes.truncate(length as usize);
        fs::metadata(PathBuf::from(OsString::from_vec(bytes)))
    };

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let parent_metadata: io::Result<Metadata> = Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "process parent identity is unsupported",
    ));

    parent_metadata.is_ok_and(|metadata| same_metadata(&current_metadata, &metadata))
}

#[cfg(target_os = "linux")]
fn linux_peer_credentials_match(
    credentials: libc::ucred,
    parent: libc::pid_t,
    effective_uid: libc::uid_t,
) -> bool {
    credentials.pid == parent && credentials.uid == effective_uid
}

#[derive(Debug)]
struct AuthenticatedProcessSupervisor {
    channel: std::os::unix::net::UnixStream,
    deadline: Instant,
}

fn monotonic_clock_nanos() -> Result<u64> {
    let mut value = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut value) } != 0 {
        return Err(process_scan_resource_error());
    }
    u64::try_from(value.tv_sec)
        .ok()
        .and_then(|seconds| seconds.checked_mul(1_000_000_000))
        .and_then(|nanos| {
            u64::try_from(value.tv_nsec)
                .ok()
                .and_then(|fraction| nanos.checked_add(fraction))
        })
        .ok_or_else(process_scan_resource_error)
}

fn monotonic_deadline_nanos(deadline: Instant) -> Result<u64> {
    let now_nanos = monotonic_clock_nanos()?;
    let now = Instant::now();
    now_nanos
        .checked_add(
            u64::try_from(deadline.saturating_duration_since(now).as_nanos())
                .map_err(|_| process_scan_resource_error())?,
        )
        .ok_or_else(process_scan_resource_error)
}

fn instant_from_monotonic_deadline(deadline_nanos: u64) -> Result<Instant> {
    let now_nanos = monotonic_clock_nanos()?;
    let remaining = Duration::from_nanos(deadline_nanos.saturating_sub(now_nanos));
    if remaining > SNAPSHOT_TIMEOUT {
        return Err(process_scan_resource_error());
    }
    Instant::now()
        .checked_add(remaining)
        .ok_or_else(process_scan_resource_error)
}

fn process_supervisor_owner_is_lost(channel: &std::os::unix::net::UnixStream) -> Result<bool> {
    let mut byte = [0_u8; 1];
    let result = unsafe {
        libc::recv(
            channel.as_raw_fd(),
            byte.as_mut_ptr().cast(),
            byte.len(),
            libc::MSG_DONTWAIT | libc::MSG_PEEK,
        )
    };
    if result == 0 {
        return Ok(true);
    }
    if result > 0 {
        return Err(process_scan_resource_error());
    }
    let error = io::Error::last_os_error();
    if matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
    ) {
        Ok(false)
    } else {
        Err(process_scan_resource_error())
    }
}

fn validate_process_supervisor_capability() -> Result<AuthenticatedProcessSupervisor> {
    validate_process_supervisor_capability_with(supervisor_parent_is_trusted)
}

fn validate_process_supervisor_capability_with<F>(
    parent_is_trusted: F,
) -> Result<AuthenticatedProcessSupervisor>
where
    F: FnOnce() -> bool,
{
    use std::os::fd::FromRawFd;
    use std::os::unix::net::UnixStream;

    if !parent_is_trusted() {
        return Err(process_scan_resource_error());
    }
    let descriptor = env::var_os(PROCESS_SUPERVISOR_CAPABILITY_FD_ENV)
        .and_then(|value| {
            value
                .to_str()
                .and_then(|value| value.parse::<libc::c_int>().ok())
        })
        .filter(|descriptor| *descriptor > libc::STDERR_FILENO)
        .ok_or_else(process_scan_resource_error)?;

    #[cfg(target_os = "linux")]
    {
        let mut credentials = std::mem::MaybeUninit::<libc::ucred>::zeroed();
        let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        let result = unsafe {
            libc::getsockopt(
                descriptor,
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                credentials.as_mut_ptr().cast(),
                &mut length,
            )
        };
        if result != 0 || length as usize != std::mem::size_of::<libc::ucred>() {
            return Err(process_scan_resource_error());
        }
        let credentials = unsafe { credentials.assume_init() };
        if !linux_peer_credentials_match(credentials, unsafe { libc::getppid() }, unsafe {
            libc::geteuid()
        }) {
            return Err(process_scan_resource_error());
        }
    }

    #[cfg(target_os = "macos")]
    {
        let mut uid = 0;
        let mut gid = 0;
        if unsafe { libc::getpeereid(descriptor, &mut uid, &mut gid) } != 0
            || uid != unsafe { libc::geteuid() }
        {
            return Err(process_scan_resource_error());
        }
    }

    let mut request = vec![0_u8; PROCESS_SUPERVISOR_CAPABILITY.len() + 8];
    let mut stream = unsafe { UnixStream::from_raw_fd(descriptor) };
    stream
        .read_exact(&mut request)
        .context("process supervisor capability is unavailable")?;
    if &request[..PROCESS_SUPERVISOR_CAPABILITY.len()] != PROCESS_SUPERVISOR_CAPABILITY {
        return Err(process_scan_resource_error());
    }
    let deadline_nanos = u64::from_be_bytes(
        request[PROCESS_SUPERVISOR_CAPABILITY.len()..]
            .try_into()
            .map_err(|_| process_scan_resource_error())?,
    );
    let deadline = instant_from_monotonic_deadline(deadline_nanos)?;
    let descriptor_flags = unsafe { libc::fcntl(descriptor, libc::F_GETFD) };
    if descriptor_flags < 0
        || unsafe {
            libc::fcntl(
                descriptor,
                libc::F_SETFD,
                descriptor_flags | libc::FD_CLOEXEC,
            )
        } < 0
    {
        return Err(process_scan_resource_error());
    }
    Ok(AuthenticatedProcessSupervisor {
        channel: stream,
        deadline,
    })
}

pub(crate) fn run_internal_process_supervisor() -> Option<i32> {
    if env::var_os(PROCESS_SUPERVISOR_ENV).as_deref() != Some(OsStr::new("1")) {
        return None;
    }
    let authenticated = match validate_process_supervisor_capability() {
        Ok(authenticated) => authenticated,
        Err(_) => return Some(exit::RUNTIME),
    };
    let mut arguments = env::args_os().skip(1);
    let Some(program) = arguments.next() else {
        return Some(exit::RUNTIME);
    };
    let arguments: Vec<OsString> = arguments.collect();
    Some(
        supervise_process_with_cleanup_proof(
            program,
            &arguments,
            Some(authenticated.channel),
            authenticated.deadline,
        )
        .unwrap_or(exit::RUNTIME),
    )
}

fn with_process_supervisor_cleanup_proof<F>(
    mut completion_channel: Option<std::os::unix::net::UnixStream>,
    run: F,
) -> Result<i32>
where
    F: FnOnce(Option<&mut std::os::unix::net::UnixStream>) -> Result<i32>,
{
    PROCESS_SUPERVISOR_TERMINATE.store(false, std::sync::atomic::Ordering::SeqCst);
    let previous = unsafe {
        libc::signal(
            libc::SIGTERM,
            request_process_supervisor_termination as *const () as libc::sighandler_t,
        )
    };
    if previous == libc::SIG_ERR {
        return Err(process_scan_resource_error());
    }
    let result = run(completion_channel.as_mut());
    if result.is_err()
        && let Some(channel) = &mut completion_channel
    {
        let _ = write_process_supervisor_completion(
            channel,
            ProcessSupervisorCompletion {
                kind: ProcessSupervisorCompletionKind::InternalFailure,
                cleanup_complete: false,
                exit_code: exit::RUNTIME,
            },
        );
    }
    result
}

fn supervise_process_with_cleanup_proof(
    program: OsString,
    arguments: &[OsString],
    completion_channel: Option<std::os::unix::net::UnixStream>,
    deadline: Instant,
) -> Result<i32> {
    with_process_supervisor_cleanup_proof(completion_channel, |completion_channel| {
        supervise_process_with(program, arguments, completion_channel, deadline, || {
            PROCESS_SUPERVISOR_TERMINATE.load(std::sync::atomic::Ordering::SeqCst)
        })
    })
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
fn supervise_process_with_cleanup_proof_and_owner<T>(
    program: OsString,
    arguments: &[OsString],
    completion_channel: Option<std::os::unix::net::UnixStream>,
    deadline: Instant,
    owner: &mut T,
) -> Result<i32>
where
    T: OwnedProcessTracker,
{
    with_process_supervisor_cleanup_proof(completion_channel, |completion_channel| {
        supervise_process_with_owner(
            program,
            arguments,
            owner,
            completion_channel,
            deadline,
            || PROCESS_SUPERVISOR_TERMINATE.load(std::sync::atomic::Ordering::SeqCst),
        )
    })
}

fn supervise_process_with<F>(
    program: OsString,
    arguments: &[OsString],
    completion_channel: Option<&mut std::os::unix::net::UnixStream>,
    deadline: Instant,
    should_terminate: F,
) -> Result<i32>
where
    F: FnMut() -> bool,
{
    let mut owner = ProcessOwner::new()?;
    supervise_process_with_owner(
        program,
        arguments,
        &mut owner,
        completion_channel,
        deadline,
        should_terminate,
    )
}

fn supervise_process_with_owner<T, F>(
    program: OsString,
    arguments: &[OsString],
    owner: &mut T,
    mut completion_channel: Option<&mut std::os::unix::net::UnixStream>,
    deadline: Instant,
    mut should_terminate: F,
) -> Result<i32>
where
    T: OwnedProcessTracker,
    F: FnMut() -> bool,
{
    let mut command = Command::new(program);
    command
        .args(arguments)
        .env_remove(PROCESS_SUPERVISOR_ENV)
        .env_remove(PROCESS_SUPERVISOR_CAPABILITY_FD_ENV)
        .stdin(Stdio::null());
    let mut child = command
        .spawn()
        .context("supervised process could not be started")?;
    loop {
        let owner_lost = match completion_channel.as_deref() {
            Some(channel) => process_supervisor_owner_is_lost(channel)?,
            None => false,
        };
        let deadline_expired = Instant::now() >= deadline;
        let signal_requested = should_terminate();
        if owner_lost || deadline_expired || signal_requested {
            terminate_owned_processes(owner, &mut child)?;
            if !owner_lost && let Some(channel) = &mut completion_channel {
                write_process_supervisor_completion(
                    channel,
                    ProcessSupervisorCompletion {
                        kind: ProcessSupervisorCompletionKind::Terminated,
                        cleanup_complete: true,
                        exit_code: exit::RUNTIME,
                    },
                )
                .context("process supervisor completion could not be delivered")?;
            }
            return Ok(exit::RUNTIME);
        }
        let refresh_deadline = deadline.min(
            Instant::now()
                .checked_add(PROCESS_CLEANUP_TIMEOUT)
                .unwrap_or(deadline),
        );
        if let Err(error) = owner.refresh(refresh_deadline) {
            let deadline_expired = Instant::now() >= deadline;
            return match terminate_owned_processes(owner, &mut child) {
                Ok(()) if deadline_expired => {
                    if let Some(channel) = &mut completion_channel {
                        write_process_supervisor_completion(
                            channel,
                            ProcessSupervisorCompletion {
                                kind: ProcessSupervisorCompletionKind::Terminated,
                                cleanup_complete: true,
                                exit_code: exit::RUNTIME,
                            },
                        )
                        .context("process supervisor completion could not be delivered")?;
                    }
                    Ok(exit::RUNTIME)
                }
                Ok(()) => Err(error),
                Err(cleanup_error) => Err(cleanup_error),
            };
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                terminate_owned_processes(owner, &mut child)?;
                let Some(exit_code) = status.code() else {
                    return Err(domain_error(
                        DirtyCheckoutErrorKind::ResourceUnavailable,
                        "supervised process terminated without an exit status",
                    ));
                };
                if let Some(channel) = &mut completion_channel {
                    write_process_supervisor_completion(
                        channel,
                        ProcessSupervisorCompletion {
                            kind: ProcessSupervisorCompletionKind::Target,
                            cleanup_complete: true,
                            exit_code,
                        },
                    )
                    .context("process supervisor completion could not be delivered")?;
                }
                return Ok(exit_code);
            }
            Ok(None) => std::thread::sleep(PROCESS_SUPERVISOR_POLL_INTERVAL),
            Err(error) => {
                return match terminate_owned_processes(owner, &mut child) {
                    Ok(()) => Err(error).context("supervised process status failed"),
                    Err(cleanup_error) => Err(cleanup_error),
                };
            }
        }
    }
}

struct SnapshotWorkerExecutable {
    source_path: PathBuf,
    command_path: PathBuf,
    file: File,
    metadata: Metadata,
    digest: [u8; 32],
    #[cfg(test)]
    _private_root: Option<tempfile::TempDir>,
}

struct WorkerDigestCache {
    file: File,
    metadata: Metadata,
    digest: [u8; 32],
}

static WORKER_DIGEST_CACHE: OnceLock<Mutex<Option<WorkerDigestCache>>> = OnceLock::new();

impl SnapshotWorkerExecutable {
    fn command_path(&self) -> &Path {
        &self.command_path
    }

    fn revalidate(&self) -> Result<()> {
        let file_metadata = self
            .file
            .metadata()
            .context("dirty snapshot worker executable descriptor is unavailable")?;
        validate_worker_file_metadata(&file_metadata)?;
        if !same_metadata(&self.metadata, &file_metadata)
            && (!same_worker_descriptor_metadata(&self.metadata, &file_metadata)
                || worker_file_digest(&self.file, file_metadata.len())? != self.digest)
        {
            return Err(domain_error(
                DirtyCheckoutErrorKind::ResourceUnavailable,
                "dirty snapshot worker executable changed after validation",
            ));
        }
        if self.command_path == self.source_path {
            validate_worker_path_permissions(&self.source_path)?;
            let path_metadata = fs::metadata(&self.source_path)
                .context("dirty snapshot worker executable could not be revalidated")?;
            if !same_metadata(&self.metadata, &path_metadata) {
                return Err(domain_error(
                    DirtyCheckoutErrorKind::ResourceUnavailable,
                    "dirty snapshot worker executable changed after validation",
                ));
            }
        }
        Ok(())
    }
}

fn same_worker_descriptor_metadata(left: &Metadata, right: &Metadata) -> bool {
    left.dev() == right.dev()
        && left.ino() == right.ino()
        && left.mode() == right.mode()
        && left.uid() == right.uid()
        && left.gid() == right.gid()
        && left.nlink() == right.nlink()
        && left.len() == right.len()
        && left.mtime() == right.mtime()
        && left.mtime_nsec() == right.mtime_nsec()
}

fn worker_file_digest(file: &File, length: u64) -> Result<[u8; 32]> {
    ensure!(
        length <= MAX_CAPTURE_BYTES as u64,
        "dirty snapshot worker executable exceeds the supported size limit"
    );
    let mut digest = Sha256::new();
    let mut offset = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    while offset < length {
        let read_limit = usize::try_from((length - offset).min(buffer.len() as u64))
            .expect("bounded worker executable read size");
        let read = file
            .read_at(&mut buffer[..read_limit], offset)
            .context("dirty snapshot worker executable could not be hashed")?;
        ensure!(
            read > 0,
            "dirty snapshot worker executable changed while hashing"
        );
        digest.update(&buffer[..read]);
        offset += read as u64;
    }
    let mut trailing = [0_u8; 1];
    ensure!(
        file.read_at(&mut trailing, length)
            .context("dirty snapshot worker executable final read failed")?
            == 0,
        "dirty snapshot worker executable changed while hashing"
    );
    Ok(digest.finalize().into())
}

fn cached_worker_file_digest(file: &File, metadata: &Metadata) -> Result<[u8; 32]> {
    let cache = WORKER_DIGEST_CACHE.get_or_init(|| Mutex::new(None));
    let mut cache = cache.lock().map_err(|_| {
        domain_error(
            DirtyCheckoutErrorKind::ResourceUnavailable,
            "dirty snapshot worker executable digest cache is unavailable",
        )
    })?;
    if let Some(cached) = cache.as_ref() {
        let cached_metadata = cached
            .file
            .metadata()
            .context("cached dirty snapshot worker executable is unavailable")?;
        if same_metadata(&cached.metadata, &cached_metadata)
            && same_metadata(&cached_metadata, metadata)
        {
            return Ok(cached.digest);
        }
    }

    let digest = worker_file_digest(file, metadata.len())?;
    let hashed_metadata = file
        .metadata()
        .context("dirty snapshot worker executable could not be revalidated after hashing")?;
    if !same_metadata(metadata, &hashed_metadata) {
        return Err(domain_error(
            DirtyCheckoutErrorKind::ResourceUnavailable,
            "dirty snapshot worker executable changed during validation",
        ));
    }
    *cache = Some(WorkerDigestCache {
        file: file
            .try_clone()
            .context("dirty snapshot worker executable descriptor could not be retained")?,
        metadata: hashed_metadata,
        digest,
    });
    Ok(digest)
}

#[cfg(test)]
fn private_test_worker_executable(
    candidate: PathBuf,
) -> Result<(PathBuf, Option<tempfile::TempDir>)> {
    let source = File::open(&candidate)
        .context("dirty snapshot worker test executable could not be opened")?;
    let source_metadata = source
        .metadata()
        .context("dirty snapshot worker test executable metadata is unavailable")?;
    if source_metadata.mode() & 0o022 == 0 || source_metadata.uid() != unsafe { libc::geteuid() } {
        return Ok((candidate, None));
    }

    let private_root = tempfile::TempDir::new()
        .context("private dirty snapshot worker test root could not be created")?;
    let private_candidate = private_root.path().join("git-cli");
    let mut private_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o700)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(&private_candidate)
        .context("private dirty snapshot worker test executable could not be created")?;
    io::copy(&mut &source, &mut private_file)
        .context("dirty snapshot worker test executable could not be copied")?;
    private_file
        .sync_all()
        .context("private dirty snapshot worker test executable could not be synced")?;
    private_file
        .set_permissions(fs::Permissions::from_mode(0o500))
        .context("private dirty snapshot worker test executable could not be hardened")?;
    let current_source_metadata = source
        .metadata()
        .context("dirty snapshot worker test executable could not be revalidated")?;
    let source_path_metadata = fs::metadata(&candidate)
        .context("dirty snapshot worker test executable path is unavailable")?;
    if !same_metadata(&source_metadata, &current_source_metadata)
        || !same_metadata(&current_source_metadata, &source_path_metadata)
    {
        return Err(domain_error(
            DirtyCheckoutErrorKind::ResourceUnavailable,
            "dirty snapshot worker test executable changed during private installation",
        ));
    }
    Ok((private_candidate, Some(private_root)))
}

fn snapshot_worker_executable() -> Result<SnapshotWorkerExecutable> {
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
    #[cfg(test)]
    let (candidate, private_root) = private_test_worker_executable(candidate)?;
    let file = File::open(&candidate)
        .context("dirty snapshot worker executable descriptor could not be opened")?;
    let metadata = file
        .metadata()
        .context("dirty snapshot worker executable metadata is unavailable")?;
    validate_worker_file_metadata(&metadata)?;
    let digest = cached_worker_file_digest(&file, &metadata)?;
    let command_path = if cfg!(target_os = "linux") {
        PathBuf::from(format!("/proc/self/fd/{}", file.as_raw_fd()))
    } else if cfg!(target_os = "macos") {
        PathBuf::from(format!("/dev/fd/{}", file.as_raw_fd()))
    } else {
        candidate.clone()
    };
    if command_path == candidate {
        validate_worker_path_permissions(&candidate)?;
        let path_metadata = fs::metadata(&candidate)
            .context("dirty snapshot worker executable path is unavailable")?;
        if !same_metadata(&metadata, &path_metadata) {
            return Err(domain_error(
                DirtyCheckoutErrorKind::ResourceUnavailable,
                "dirty snapshot worker executable changed during validation",
            ));
        }
    }
    Ok(SnapshotWorkerExecutable {
        source_path: candidate,
        command_path,
        file,
        metadata,
        digest,
        #[cfg(test)]
        _private_root: private_root,
    })
}

fn validate_worker_file_metadata(metadata: &Metadata) -> Result<()> {
    let owner = metadata.uid();
    let mode = metadata.mode();
    if !metadata.file_type().is_file()
        || mode & 0o111 == 0
        || (owner != 0 && owner != unsafe { libc::geteuid() })
        || mode & 0o022 != 0
    {
        return Err(domain_error(
            DirtyCheckoutErrorKind::ResourceUnavailable,
            "dirty snapshot worker executable is not trusted",
        ));
    }
    Ok(())
}

fn validate_worker_path_permissions(candidate: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(candidate)
        .context("dirty snapshot worker executable metadata is unavailable")?;
    validate_worker_file_metadata(&metadata)?;
    let euid = unsafe { libc::geteuid() };
    let mut component = candidate.parent();
    while let Some(directory) = component {
        let metadata = fs::symlink_metadata(directory)
            .context("dirty snapshot worker parent metadata is unavailable")?;
        let owner = metadata.uid();
        let mode = metadata.mode();
        if !metadata.file_type().is_dir() || (owner != 0 && owner != euid) || mode & 0o022 != 0 {
            return Err(domain_error(
                DirtyCheckoutErrorKind::ResourceUnavailable,
                "dirty snapshot worker executable path is not trusted",
            ));
        }
        component = directory.parent();
    }
    Ok(())
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
    let deadline = deadline_after(SNAPSHOT_TIMEOUT)?;
    let mut previous = None;
    let mut snapshotter = |path: &Path, barrier_deadline: Instant| {
        snapshot_reusing(path, barrier_deadline, &mut previous)
    };
    let mut now = unix_time;
    adopt_dirty_inner(
        checkout,
        state_root,
        challenge_token,
        reason_file,
        deadline,
        &mut snapshotter,
        &mut now,
    )
    .map_err(|error| {
        command_error(
            error,
            DirtyCheckoutErrorKind::AdoptionFailed,
            "dirty checkout adoption failed",
        )
    })
}

fn adopt_dirty_cli(
    checkout: &Path,
    state_root: &Path,
    challenge_token: &str,
    reason_file: &Path,
) -> Result<AdoptionReceipt> {
    let deadline = deadline_after(SNAPSHOT_TIMEOUT)?;
    let mut previous: Option<DirtySnapshot> = None;
    let mut snapshotter = |path: &Path, barrier_deadline: Instant| {
        let snapshot = dirty_snapshot_cli_until(path, barrier_deadline)?;
        if previous.as_ref().is_some_and(|prior| prior != &snapshot) {
            return Err(domain_error(
                DirtyCheckoutErrorKind::SnapshotFailed,
                "checkout changed between adoption verification barriers",
            ));
        }
        previous = Some(snapshot.clone());
        Ok(snapshot)
    };
    let mut now = unix_time;
    adopt_dirty_inner(
        checkout,
        state_root,
        challenge_token,
        reason_file,
        deadline,
        &mut snapshotter,
        &mut now,
    )
    .map_err(|error| {
        command_error(
            error,
            DirtyCheckoutErrorKind::AdoptionFailed,
            "dirty checkout adoption failed",
        )
    })
}

fn snapshot_reusing(
    checkout: &Path,
    deadline: Instant,
    previous: &mut Option<DirtySnapshot>,
) -> Result<DirtySnapshot> {
    let current = dirty_snapshot_worker(checkout, deadline)?;
    if previous.as_ref().is_some_and(|prior| prior != &current) {
        return Err(domain_error(
            DirtyCheckoutErrorKind::SnapshotFailed,
            "checkout changed between adoption verification barriers",
        ));
    }
    *previous = Some(current.clone());
    Ok(current)
}

#[cfg(test)]
fn adopt_dirty_with_snapshot<F>(
    checkout: &Path,
    state_root: &Path,
    challenge_token: &str,
    reason_file: &Path,
    snapshotter: F,
) -> Result<AdoptionReceipt>
where
    F: FnMut(&Path) -> Result<DirtySnapshot>,
{
    adopt_dirty_with_snapshot_and_clock(
        checkout,
        state_root,
        challenge_token,
        reason_file,
        snapshotter,
        unix_time,
    )
}

#[cfg(test)]
fn adopt_dirty_with_snapshot_and_clock<F, G>(
    checkout: &Path,
    state_root: &Path,
    challenge_token: &str,
    reason_file: &Path,
    snapshotter: F,
    now: G,
) -> Result<AdoptionReceipt>
where
    F: FnMut(&Path) -> Result<DirtySnapshot>,
    G: FnMut() -> Result<u64>,
{
    let deadline = deadline_after(SNAPSHOT_TIMEOUT)?;
    let mut snapshotter = snapshotter;
    adopt_dirty_with_snapshot_clock_and_deadline(
        checkout,
        state_root,
        challenge_token,
        reason_file,
        |path, _| snapshotter(path),
        now,
        deadline,
    )
}

#[cfg(test)]
fn adopt_dirty_with_snapshot_clock_and_deadline<F, G>(
    checkout: &Path,
    state_root: &Path,
    challenge_token: &str,
    reason_file: &Path,
    mut snapshotter: F,
    mut now: G,
    deadline: Instant,
) -> Result<AdoptionReceipt>
where
    F: FnMut(&Path, Instant) -> Result<DirtySnapshot>,
    G: FnMut() -> Result<u64>,
{
    adopt_dirty_inner(
        checkout,
        state_root,
        challenge_token,
        reason_file,
        deadline,
        &mut snapshotter,
        &mut now,
    )
    .map_err(|error| {
        command_error(
            error,
            DirtyCheckoutErrorKind::AdoptionFailed,
            "dirty checkout adoption failed",
        )
    })
}

fn adopt_dirty_inner<F, G>(
    checkout: &Path,
    state_root: &Path,
    challenge_token: &str,
    reason_file: &Path,
    deadline: Instant,
    snapshotter: &mut F,
    now: &mut G,
) -> Result<AdoptionReceipt>
where
    F: FnMut(&Path, Instant) -> Result<DirtySnapshot>,
    G: FnMut() -> Result<u64>,
{
    if !is_lower_hex(challenge_token, 64) {
        return Err(domain_error(
            DirtyCheckoutErrorKind::InvalidInput,
            "challenge token is malformed",
        ));
    }
    ensure_deadline(deadline)?;
    let token_digest = sha256_hex(challenge_token.as_bytes());
    let identity = resolve_checkout(checkout, false, deadline)?;
    let checkout_dir = checkout_state_dir(state_root, &identity)?;
    let challenge_dir = checkout_dir.join("challenges");
    verify_private_directory(&challenge_dir).map_err(|error| {
        command_error(
            error,
            DirtyCheckoutErrorKind::MalformedState,
            "dirty-checkout challenge directory is untrusted",
        )
    })?;
    let _lock = LeaseLock::acquire_until(&checkout_dir, deadline)?;
    recover_checkout_pending_adoptions(
        &checkout_dir,
        &challenge_dir,
        &identity,
        &token_digest,
        deadline,
    )?;

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

    let pending_recovery = inspect_pending_adoption_until(
        &checkout_dir,
        &challenge_dir,
        &identity,
        &token_digest,
        deadline,
    )?;
    match pending_recovery {
        PendingRecovery::Staged | PendingRecovery::Committed => {
            let staged_pending = pending_recovery == PendingRecovery::Staged;
            return committed_adoption_retry(
                CommittedAdoptionRetry {
                    checkout_dir: &checkout_dir,
                    challenge_dir: &challenge_dir,
                    identity: &identity,
                    token_digest: &token_digest,
                    reason_digest: &reason_digest,
                    provisional_pending: true,
                    staged_pending,
                    deadline,
                },
                snapshotter,
                &mut *now,
            )?
            .ok_or_else(|| {
                domain_error(
                    DirtyCheckoutErrorKind::MalformedState,
                    "recovered adoption does not match its committed artifacts",
                )
            });
        }
        PendingRecovery::Revoked => {
            return Err(domain_error(
                DirtyCheckoutErrorKind::ChallengeReused,
                "dirty-checkout challenge was consumed by a revoked adoption",
            ));
        }
        PendingRecovery::None | PendingRecovery::RolledBack => {}
    }

    let challenge_path = challenge_dir.join(format!("{token_digest}.json"));
    let challenge_bytes = match read_private_regular(&challenge_path, MAX_STATE_FILE_BYTES, true) {
        Ok(bytes) => bytes,
        Err(error)
            if error
                .downcast_ref::<io::Error>()
                .is_some_and(|error| error.kind() == io::ErrorKind::NotFound) =>
        {
            if let Some(receipt) = committed_adoption_retry(
                CommittedAdoptionRetry {
                    checkout_dir: &checkout_dir,
                    challenge_dir: &challenge_dir,
                    identity: &identity,
                    token_digest: &token_digest,
                    reason_digest: &reason_digest,
                    provisional_pending: false,
                    staged_pending: false,
                    deadline,
                },
                snapshotter,
                &mut *now,
            )? {
                return Ok(receipt);
            }
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
    let challenge_digest = sha256_hex(&challenge_bytes);
    let challenge: ChallengeRecord = serde_json::from_slice(&challenge_bytes).map_err(|_| {
        domain_error(
            DirtyCheckoutErrorKind::MalformedState,
            "dirty-checkout challenge state is malformed",
        )
    })?;
    validate_challenge_identity(&challenge, &identity, &token_digest)?;
    validate_challenge_at(&challenge, now()?)?;
    let initial_now = now()?;

    let lease_path = checkout_dir.join("lease.json");
    let existing_bytes = read_optional_private(&lease_path, "checkout lease")?;
    let existing = existing_bytes.as_deref().map(parse_lease).transpose()?;
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

    let predecessor_lease_bytes =
        preserved_predecessor_lease_bytes(existing.as_ref(), existing_bytes.clone());

    let initial_snapshot = snapshot_before_deadline(snapshotter, &identity.root, deadline)?;
    validate_snapshot_matches_challenge(&initial_snapshot, &challenge)?;
    validate_adoption_boundary(&challenge, &identity, now()?)?;

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
    let checkout_root_text = lease_path_text(&identity.root);
    let checkout_git_dir_text = lease_path_text(&identity.git_dir);
    let pending = PendingAdoptionRecord {
        schema: PENDING_ADOPTION_SCHEMA.to_string(),
        receipt_id: receipt_id.clone(),
        token_digest: token_digest.clone(),
        challenge_digest: challenge_digest.clone(),
        session_key: challenge.session_key.clone(),
        checkout_instance: identity.checkout_instance.clone(),
        snapshot_id: challenge.snapshot_id.clone(),
        predecessor_receipt_id: predecessor
            .as_ref()
            .map(|predecessor| predecessor.receipt_id.clone()),
        predecessor_receipt_digest: predecessor
            .as_ref()
            .and_then(|predecessor| predecessor.receipt_digest.clone()),
        predecessor_spent_challenge_digest: predecessor
            .as_ref()
            .and_then(|predecessor| predecessor.spent_challenge_digest.clone()),
        predecessor_lease_digest: predecessor_lease_bytes.as_deref().map(sha256_hex),
        predecessor_lease_bytes,
    };
    validate_pending_adoption(&pending, &identity, &token_digest)?;
    let pending_path = pending_adoption_path(&checkout_dir, &token_digest);
    if let Err(error) =
        write_json_atomic_with_limit(&pending_path, &pending, false, MAX_PENDING_STATE_FILE_BYTES)
    {
        return Err(transition_failure_after_recovery(
            error,
            &checkout_dir,
            &challenge_dir,
            &identity,
            &token_digest,
            deadline,
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
        snapshot_before_deadline(snapshotter, &identity.root, deadline),
        &checkout_dir,
        &challenge_dir,
        &identity,
        &token_digest,
        deadline,
    )?;
    transition_result_after_recovery(
        validate_snapshot_matches_challenge(&prepared_snapshot, &challenge),
        &checkout_dir,
        &challenge_dir,
        &identity,
        &token_digest,
        deadline,
    )?;
    let prepared_at = transition_result_after_recovery(
        now(),
        &checkout_dir,
        &challenge_dir,
        &identity,
        &token_digest,
        deadline,
    )?;
    transition_result_after_recovery(
        validate_adoption_boundary(&challenge, &identity, prepared_at),
        &checkout_dir,
        &challenge_dir,
        &identity,
        &token_digest,
        deadline,
    )?;
    let (prepared_receipt, _) = transition_result_after_recovery(
        build_transition(&prepared_snapshot, prepared_at),
        &checkout_dir,
        &challenge_dir,
        &identity,
        &token_digest,
        deadline,
    )?;
    if let Err(error) = write_json_atomic(&receipt_path, &prepared_receipt, false) {
        return Err(transition_failure_after_recovery(
            error,
            &checkout_dir,
            &challenge_dir,
            &identity,
            &token_digest,
            deadline,
        ));
    }

    let snapshot = transition_result_after_recovery(
        snapshot_before_deadline(snapshotter, &identity.root, deadline),
        &checkout_dir,
        &challenge_dir,
        &identity,
        &token_digest,
        deadline,
    )?;
    transition_result_after_recovery(
        validate_snapshot_matches_challenge(&snapshot, &challenge),
        &checkout_dir,
        &challenge_dir,
        &identity,
        &token_digest,
        deadline,
    )?;
    let transition_at = transition_result_after_recovery(
        now(),
        &checkout_dir,
        &challenge_dir,
        &identity,
        &token_digest,
        deadline,
    )?;
    transition_result_after_recovery(
        validate_adoption_boundary(&challenge, &identity, transition_at),
        &checkout_dir,
        &challenge_dir,
        &identity,
        &token_digest,
        deadline,
    )?;
    let (receipt_record, lease) = transition_result_after_recovery(
        build_transition(&snapshot, transition_at),
        &checkout_dir,
        &challenge_dir,
        &identity,
        &token_digest,
        deadline,
    )?;
    if let Err(error) = write_json_atomic(&receipt_path, &receipt_record, true) {
        return Err(transition_failure_after_recovery(
            error,
            &checkout_dir,
            &challenge_dir,
            &identity,
            &token_digest,
            deadline,
        ));
    }
    if let Err(error) =
        validate_adoption_precommit_with(&challenge, &identity, || Ok(()), &mut *now)
    {
        return Err(transition_failure_after_recovery(
            error,
            &checkout_dir,
            &challenge_dir,
            &identity,
            &token_digest,
            deadline,
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
            &token_digest,
            deadline,
        ));
    }
    if let Err(error) = sync_directory(&challenge_dir).and_then(|()| sync_directory(&receipts_dir))
    {
        return Err(transition_failure_after_recovery(
            error,
            &checkout_dir,
            &challenge_dir,
            &identity,
            &token_digest,
            deadline,
        ));
    }

    let staged_path = staged_lease_path(&checkout_dir, &receipt_id);
    match install_lease(&staged_path, &lease, None, &identity) {
        LeaseInstallOutcome::Installed => {
            let post_stage = snapshot_before_deadline(snapshotter, &identity.root, deadline)
                .and_then(|post_snapshot| {
                    validate_snapshot_matches_challenge(&post_snapshot, &challenge)
                })
                .and_then(|()| validate_adoption_boundary(&challenge, &identity, now()?));
            if let Err(error) = post_stage {
                if let Err(rollback_error) = rollback_provisional_adoption(
                    &staged_path,
                    &checkout_dir,
                    &challenge_dir,
                    &identity,
                    &token_digest,
                    &receipt_id,
                    deadline,
                ) {
                    return Err(domain_error(
                        DirtyCheckoutErrorKind::AdoptionFailed,
                        format!(
                            "post-stage snapshot verification failed and rollback is incomplete: {error}; rollback failed: {rollback_error}"
                        ),
                    ));
                }
                return Err(error);
            }
            match publish_staged_lease(
                &staged_path,
                &lease_path,
                &lease,
                existing_bytes.as_deref(),
                &identity,
            ) {
                LeaseInstallOutcome::Installed => {}
                LeaseInstallOutcome::NotInstalled(error) => {
                    return Err(transition_failure_after_recovery(
                        error,
                        &checkout_dir,
                        &challenge_dir,
                        &identity,
                        &token_digest,
                        deadline,
                    ));
                }
                LeaseInstallOutcome::Ambiguous(error) => {
                    return Err(domain_error(
                        DirtyCheckoutErrorKind::AdoptionFailed,
                        format!(
                            "checkout lease publication is ambiguous; recovery state was retained: {error}"
                        ),
                    ));
                }
            }
            if recover_pending_adoption_until(
                &checkout_dir,
                &challenge_dir,
                &identity,
                &token_digest,
                deadline,
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
                &token_digest,
                deadline,
            ));
        }
        LeaseInstallOutcome::Ambiguous(error) => {
            return Err(domain_error(
                DirtyCheckoutErrorKind::AdoptionFailed,
                format!(
                    "staged checkout lease installation is ambiguous; recovery state was retained: {error}"
                ),
            ));
        }
    }

    Ok(AdoptionReceipt {
        receipt_id,
        snapshot_id: snapshot.snapshot_id,
    })
}

fn preserved_predecessor_lease_bytes(
    existing: Option<&LeaseRecord>,
    existing_bytes: Option<Vec<u8>>,
) -> Option<Vec<u8>> {
    existing.and(existing_bytes)
}

fn snapshot_before_deadline<F>(
    snapshotter: &mut F,
    checkout: &Path,
    deadline: Instant,
) -> Result<DirtySnapshot>
where
    F: FnMut(&Path, Instant) -> Result<DirtySnapshot>,
{
    ensure_deadline(deadline)?;
    let snapshot = snapshotter(checkout, deadline)?;
    ensure_deadline(deadline)?;
    Ok(snapshot)
}

struct CommittedAdoptionRetry<'a> {
    checkout_dir: &'a Path,
    challenge_dir: &'a Path,
    identity: &'a CheckoutIdentity,
    token_digest: &'a str,
    reason_digest: &'a str,
    provisional_pending: bool,
    staged_pending: bool,
    deadline: Instant,
}

fn committed_adoption_retry<F, G>(
    retry: CommittedAdoptionRetry<'_>,
    snapshotter: &mut F,
    now: &mut G,
) -> Result<Option<AdoptionReceipt>>
where
    F: FnMut(&Path, Instant) -> Result<DirtySnapshot>,
    G: FnMut() -> Result<u64>,
{
    let CommittedAdoptionRetry {
        checkout_dir,
        challenge_dir,
        identity,
        token_digest,
        reason_digest,
        provisional_pending,
        staged_pending,
        deadline,
    } = retry;
    let staged_pending_record = if staged_pending {
        let pending_path = pending_adoption_path(checkout_dir, token_digest);
        let pending_bytes =
            read_private_regular(&pending_path, MAX_PENDING_STATE_FILE_BYTES, true)?;
        let pending: PendingAdoptionRecord =
            serde_json::from_slice(&pending_bytes).map_err(|_| {
                domain_error(
                    DirtyCheckoutErrorKind::MalformedState,
                    "staged adoption recovery marker is malformed",
                )
            })?;
        validate_pending_adoption(&pending, identity, token_digest)?;
        Some(pending)
    } else {
        None
    };
    let authoritative_lease_path = checkout_dir.join("lease.json");
    let lease_path = staged_pending_record.as_ref().map_or_else(
        || authoritative_lease_path.clone(),
        |pending| staged_lease_path(checkout_dir, &pending.receipt_id),
    );
    let Some(lease) = load_lease(&lease_path)? else {
        return Ok(None);
    };
    validate_lease(&lease, identity)?;
    let Some(adoption) = lease.adoption() else {
        return Ok(None);
    };
    let receipt_id = adoption.receipt_id.clone();
    let receipts_dir = checkout_dir.join("receipts");
    verify_private_directory(&receipts_dir).map_err(|error| {
        command_error(
            error,
            DirtyCheckoutErrorKind::MalformedState,
            "committed adoption receipt directory is untrusted",
        )
    })?;
    let spent_path = receipts_dir.join(format!(".challenge-{receipt_id}.json"));
    let Some(spent_bytes) = read_optional_private(&spent_path, "committed adoption challenge")?
    else {
        return Err(domain_error(
            DirtyCheckoutErrorKind::MalformedState,
            "committed adoption challenge artifact is missing",
        ));
    };
    let spent: ChallengeRecord = serde_json::from_slice(&spent_bytes).map_err(|_| {
        domain_error(
            DirtyCheckoutErrorKind::MalformedState,
            "committed adoption challenge artifact is malformed",
        )
    })?;
    validate_challenge_identity(&spent, identity, &spent.token_digest)?;
    if spent.token_digest != token_digest {
        return Ok(None);
    }
    let receipt_path = receipts_dir.join(format!("{receipt_id}.json"));
    let receipt_bytes =
        read_private_regular(&receipt_path, MAX_STATE_FILE_BYTES, true).map_err(|error| {
            command_error(
                error,
                DirtyCheckoutErrorKind::MalformedState,
                "committed adoption receipt is unavailable",
            )
        })?;
    let receipt: ReceiptRecord = serde_json::from_slice(&receipt_bytes).map_err(|_| {
        domain_error(
            DirtyCheckoutErrorKind::MalformedState,
            "committed adoption receipt is malformed",
        )
    })?;
    validate_receipt(&receipt, identity, &receipt_id)?;
    validate_receipt_matches_lease(&receipt, &lease)?;
    validate_spent_challenge_matches_lease(&spent_bytes, &spent, identity, &lease)?;
    if adoption.reason_digest != reason_digest {
        return Err(domain_error(
            DirtyCheckoutErrorKind::ChallengeReused,
            "committed adoption retry inputs or artifacts do not match",
        ));
    }

    let verification = (|| -> Result<()> {
        let before_snapshot = now()?;
        if lease.expires_at() <= before_snapshot {
            return Err(domain_error(
                DirtyCheckoutErrorKind::ChallengeReused,
                "committed adoption lease expired during retry validation",
            ));
        }
        if provisional_pending {
            validate_adoption_boundary(&spent, identity, before_snapshot)?;
        }
        let snapshot = snapshot_before_deadline(snapshotter, &identity.root, deadline)?;
        validate_snapshot_matches_challenge(&snapshot, &spent)?;
        let after_snapshot = now()?;
        if lease.expires_at() <= after_snapshot {
            return Err(domain_error(
                DirtyCheckoutErrorKind::ChallengeReused,
                "committed adoption lease expired during retry validation",
            ));
        }
        if provisional_pending {
            validate_adoption_boundary(&spent, identity, after_snapshot)?;
        }
        Ok(())
    })();

    if let Err(error) = verification {
        if provisional_pending
            && let Err(rollback_error) = rollback_provisional_adoption(
                &lease_path,
                checkout_dir,
                challenge_dir,
                identity,
                token_digest,
                &receipt_id,
                deadline,
            )
        {
            return Err(domain_error(
                DirtyCheckoutErrorKind::AdoptionFailed,
                format!(
                    "provisional adoption verification failed and rollback is incomplete: {error}; rollback failed: {rollback_error}"
                ),
            ));
        }
        return Err(error);
    }

    if let Some(pending) = &staged_pending_record {
        match publish_staged_lease(
            &lease_path,
            &authoritative_lease_path,
            &lease,
            pending.predecessor_lease_bytes.as_deref(),
            identity,
        ) {
            LeaseInstallOutcome::Installed => {}
            LeaseInstallOutcome::NotInstalled(error) => return Err(error),
            LeaseInstallOutcome::Ambiguous(error) => {
                return Err(domain_error(
                    DirtyCheckoutErrorKind::AdoptionFailed,
                    format!(
                        "checkout lease publication is ambiguous; recovery state was retained: {error}"
                    ),
                ));
            }
        }
    }

    if provisional_pending
        && recover_pending_adoption_until(
            checkout_dir,
            challenge_dir,
            identity,
            token_digest,
            deadline,
        )? != PendingRecovery::Committed
    {
        return Err(domain_error(
            DirtyCheckoutErrorKind::AdoptionFailed,
            "verified provisional adoption did not finalize committed recovery",
        ));
    }
    Ok(Some(AdoptionReceipt {
        receipt_id: receipt.receipt_id,
        snapshot_id: receipt.snapshot_id,
    }))
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

fn rollback_provisional_adoption(
    provisional_lease_path: &Path,
    checkout_dir: &Path,
    challenge_dir: &Path,
    identity: &CheckoutIdentity,
    token_digest: &str,
    receipt_id: &str,
    deadline: Instant,
) -> Result<()> {
    rollback_provisional_adoption_with_pruning(ProvisionalRollback {
        provisional_lease_path,
        checkout_dir,
        challenge_dir,
        identity,
        token_digest,
        receipt_id,
        prune_revocations: true,
        deadline,
    })
}

fn rollback_provisional_adoption_without_pruning(
    provisional_lease_path: &Path,
    checkout_dir: &Path,
    challenge_dir: &Path,
    identity: &CheckoutIdentity,
    token_digest: &str,
    receipt_id: &str,
    deadline: Instant,
) -> Result<()> {
    rollback_provisional_adoption_with_pruning(ProvisionalRollback {
        provisional_lease_path,
        checkout_dir,
        challenge_dir,
        identity,
        token_digest,
        receipt_id,
        prune_revocations: false,
        deadline,
    })
}

struct ProvisionalRollback<'a> {
    provisional_lease_path: &'a Path,
    checkout_dir: &'a Path,
    challenge_dir: &'a Path,
    identity: &'a CheckoutIdentity,
    token_digest: &'a str,
    receipt_id: &'a str,
    prune_revocations: bool,
    deadline: Instant,
}

fn rollback_provisional_adoption_with_pruning(rollback: ProvisionalRollback<'_>) -> Result<()> {
    let ProvisionalRollback {
        provisional_lease_path,
        checkout_dir,
        challenge_dir,
        identity,
        token_digest,
        receipt_id,
        prune_revocations,
        deadline,
    } = rollback;
    let revoked_lease_path = checkout_dir.join(format!(".revoked-{receipt_id}.json"));
    fs::rename(provisional_lease_path, &revoked_lease_path)
        .context("provisional adoption rollback could not revoke the lease")?;
    sync_directory(checkout_dir)
        .context("provisional adoption rollback tombstone could not be made durable")?;
    let recovery = if prune_revocations {
        recover_pending_adoption_until(
            checkout_dir,
            challenge_dir,
            identity,
            token_digest,
            deadline,
        )?
    } else {
        recover_pending_adoption_without_pruning(
            checkout_dir,
            challenge_dir,
            identity,
            token_digest,
            deadline,
        )?
    };
    if recovery != PendingRecovery::Revoked {
        return Err(domain_error(
            DirtyCheckoutErrorKind::AdoptionFailed,
            "provisional adoption rollback did not enter revoked recovery state",
        ));
    }
    Ok(())
}

#[derive(Debug)]
struct ExpiredPredecessor {
    receipt_id: String,
    receipt_path: PathBuf,
    spent_challenge_path: PathBuf,
    receipt_digest: Option<String>,
    spent_challenge_digest: Option<String>,
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
        read_optional_private(&receipt_path, "expired adoption predecessor receipt")?;
    if let Some(receipt_bytes) = &receipt_bytes {
        let receipt: ReceiptRecord = serde_json::from_slice(receipt_bytes).map_err(|_| {
            domain_error(
                DirtyCheckoutErrorKind::MalformedState,
                "expired adoption predecessor receipt is malformed",
            )
        })?;
        validate_receipt(&receipt, identity, &adoption.receipt_id)?;
        validate_receipt_matches_lease(&receipt, lease)?;
    }
    let spent_bytes = read_optional_private(
        &spent_challenge_path,
        "expired adoption predecessor challenge",
    )?;
    if let Some(spent_bytes) = &spent_bytes {
        let spent: ChallengeRecord = serde_json::from_slice(spent_bytes).map_err(|_| {
            domain_error(
                DirtyCheckoutErrorKind::MalformedState,
                "expired adoption predecessor challenge is malformed",
            )
        })?;
        validate_spent_challenge_matches_lease(spent_bytes, &spent, identity, lease)?;
    }
    Ok(Some(ExpiredPredecessor {
        receipt_id: adoption.receipt_id.clone(),
        receipt_path,
        spent_challenge_path,
        receipt_digest: receipt_bytes.as_deref().map(sha256_hex),
        spent_challenge_digest: spent_bytes.as_deref().map(sha256_hex),
    }))
}

fn load_bound_predecessor(
    receipts_dir: &Path,
    receipt_id: &str,
    expected_receipt_digest: Option<&str>,
    expected_spent_challenge_digest: Option<&str>,
) -> Result<ExpiredPredecessor> {
    let receipt_path = receipts_dir.join(format!("{receipt_id}.json"));
    let spent_challenge_path = receipts_dir.join(format!(".challenge-{receipt_id}.json"));
    let receipt_bytes = read_optional_private(&receipt_path, "bound predecessor receipt")?;
    let spent_bytes = read_optional_private(&spent_challenge_path, "bound predecessor challenge")?;
    let receipt_matches = match (&receipt_bytes, expected_receipt_digest) {
        (Some(bytes), Some(expected)) => sha256_hex(bytes) == expected,
        (None, _) => true,
        (Some(_), None) => false,
    };
    let spent_matches = match (&spent_bytes, expected_spent_challenge_digest) {
        (Some(bytes), Some(expected)) => sha256_hex(bytes) == expected,
        (None, _) => true,
        (Some(_), None) => false,
    };
    if !receipt_matches || !spent_matches {
        return Err(domain_error(
            DirtyCheckoutErrorKind::MalformedState,
            "bound predecessor artifacts do not match pending adoption state",
        ));
    }
    Ok(ExpiredPredecessor {
        receipt_id: receipt_id.to_string(),
        receipt_path,
        spent_challenge_path,
        receipt_digest: expected_receipt_digest.map(str::to_string),
        spent_challenge_digest: expected_spent_challenge_digest.map(str::to_string),
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

struct RecoveryScanBudget {
    deadline: Instant,
    directory_entries: usize,
    state_entries: usize,
    aggregate_bytes: u64,
    name_bytes: usize,
}

impl RecoveryScanBudget {
    fn new(deadline: Instant) -> Self {
        Self {
            deadline,
            directory_entries: 0,
            state_entries: 0,
            aggregate_bytes: 0,
            name_bytes: 0,
        }
    }

    fn charge(&mut self, entry: &fs::DirEntry, state_entry: bool) -> Result<()> {
        ensure_deadline(self.deadline)?;
        self.directory_entries = self.directory_entries.saturating_add(1);
        self.name_bytes = self
            .name_bytes
            .saturating_add(entry.file_name().as_bytes().len());
        if self.directory_entries > MAX_RECOVERY_DIRECTORY_ENTRIES
            || self.name_bytes > MAX_RECOVERY_NAME_BYTES
        {
            return Err(domain_error(
                DirtyCheckoutErrorKind::ResourceUnavailable,
                "checkout recovery directory exceeds the supported scan limit",
            ));
        }
        if state_entry {
            self.state_entries = self.state_entries.saturating_add(1);
            let metadata = fs::symlink_metadata(entry.path())
                .context("checkout recovery state metadata is unavailable")?;
            self.aggregate_bytes = self.aggregate_bytes.saturating_add(metadata.len());
            if self.state_entries > MAX_RECOVERY_STATE_ENTRIES
                || self.aggregate_bytes > MAX_RECOVERY_AGGREGATE_BYTES
            {
                return Err(domain_error(
                    DirtyCheckoutErrorKind::ResourceUnavailable,
                    "checkout recovery state exceeds the supported aggregate limit",
                ));
            }
        }
        Ok(())
    }

    fn check_deadline(&self) -> Result<()> {
        ensure_deadline(self.deadline)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingRecovery {
    None,
    RolledBack,
    Staged,
    Committed,
    Revoked,
}

fn pending_adoption_path(checkout_dir: &Path, token_digest: &str) -> PathBuf {
    checkout_dir.join(format!(".pending-adoption-{token_digest}.json"))
}

fn staged_lease_path(checkout_dir: &Path, receipt_id: &str) -> PathBuf {
    checkout_dir.join(format!(".pending-lease-{receipt_id}.json"))
}

fn recover_pending_batch_with<I, R, B, P>(
    records: I,
    current_token_digest: &str,
    mut recover: R,
    mut rollback_staged: B,
    prune: P,
) -> Result<()>
where
    I: IntoIterator<Item = (String, String)>,
    R: FnMut(&str) -> Result<PendingRecovery>,
    B: FnMut(&str, &str) -> Result<()>,
    P: FnOnce(Option<&str>) -> Result<()>,
{
    let mut failed_receipt_id = None;
    let recovery_result = (|| {
        for (token_digest, receipt_id) in records {
            if token_digest == current_token_digest {
                continue;
            }
            let recovery = match recover(&token_digest) {
                Ok(recovery) => recovery,
                Err(error) => {
                    failed_receipt_id = Some(receipt_id);
                    return Err(error);
                }
            };
            if recovery == PendingRecovery::Staged
                && let Err(error) = rollback_staged(&token_digest, &receipt_id)
            {
                failed_receipt_id = Some(receipt_id);
                return Err(error);
            }
        }
        Ok(())
    })();
    let prune_result = prune(failed_receipt_id.as_deref());

    match (recovery_result, prune_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(primary), Ok(())) => Err(primary),
        (Ok(()), Err(prune_error)) => Err(prune_error),
        (Err(primary), Err(prune_error)) => {
            let context = format!(
                "checkout-wide pending-adoption recovery failed: {primary:#}; deferred revocation tombstone pruning also failed: {prune_error:#}"
            );
            Err(primary.context(context))
        }
    }
}

fn recover_checkout_pending_adoptions(
    checkout_dir: &Path,
    challenge_dir: &Path,
    identity: &CheckoutIdentity,
    current_token_digest: &str,
    deadline: Instant,
) -> Result<()> {
    let mut budget = RecoveryScanBudget::new(deadline);
    let mut pending_records = Vec::new();
    let mut protected_receipts = HashSet::new();
    let mut tombstone_candidates = HashSet::new();
    for entry in fs::read_dir(checkout_dir).context("pending adoption scan is unavailable")? {
        let entry = entry.context("pending adoption scan entry is unreadable")?;
        let name = entry.file_name();
        let token_digest = name
            .to_str()
            .and_then(|name| name.strip_prefix(".pending-adoption-"))
            .and_then(|name| name.strip_suffix(".json"))
            .filter(|digest| is_lower_hex(digest, 64));
        let tombstone_receipt = name
            .to_str()
            .and_then(|name| name.strip_prefix(".revoked-"))
            .and_then(|name| name.strip_suffix(".json"))
            .filter(|receipt_id| is_lower_hex(receipt_id, 64));
        budget.charge(
            &entry,
            token_digest.is_some() || tombstone_receipt.is_some(),
        )?;
        if let Some(receipt_id) = tombstone_receipt {
            tombstone_candidates.insert(receipt_id.to_string());
        }
        let Some(token_digest) = token_digest else {
            continue;
        };
        let pending_bytes =
            read_private_regular(&entry.path(), MAX_PENDING_STATE_FILE_BYTES, true)?;
        let pending: PendingAdoptionRecord =
            serde_json::from_slice(&pending_bytes).map_err(|_| {
                domain_error(
                    DirtyCheckoutErrorKind::MalformedState,
                    "checkout-wide recovery marker is malformed",
                )
            })?;
        validate_pending_adoption(&pending, identity, token_digest)?;
        tombstone_candidates.insert(pending.receipt_id.clone());
        protected_receipts.insert(pending.receipt_id.clone());
        pending_records.push((token_digest.to_string(), pending.receipt_id));
    }
    budget.check_deadline()?;

    recover_pending_batch_with(
        pending_records,
        current_token_digest,
        |token_digest| {
            recover_pending_adoption_without_pruning(
                checkout_dir,
                challenge_dir,
                identity,
                token_digest,
                deadline,
            )
        },
        |token_digest, receipt_id| {
            rollback_provisional_adoption_without_pruning(
                &staged_lease_path(checkout_dir, receipt_id),
                checkout_dir,
                challenge_dir,
                identity,
                token_digest,
                receipt_id,
                deadline,
            )
        },
        |failed_receipt_id| {
            if let Some(receipt_id) = failed_receipt_id {
                protected_receipts.insert(receipt_id.to_string());
            }
            prune_revocation_tombstone_candidates(
                checkout_dir,
                identity,
                &protected_receipts,
                tombstone_candidates,
                deadline,
            )
        },
    )?;
    budget.check_deadline()
}

#[cfg(test)]
fn recover_pending_adoption(
    checkout_dir: &Path,
    challenge_dir: &Path,
    identity: &CheckoutIdentity,
    token_digest: &str,
) -> Result<PendingRecovery> {
    recover_pending_adoption_until(
        checkout_dir,
        challenge_dir,
        identity,
        token_digest,
        deadline_after(SNAPSHOT_TIMEOUT)?,
    )
}

fn recover_pending_adoption_until(
    checkout_dir: &Path,
    challenge_dir: &Path,
    identity: &CheckoutIdentity,
    token_digest: &str,
    deadline: Instant,
) -> Result<PendingRecovery> {
    recover_pending_adoption_with(
        checkout_dir,
        challenge_dir,
        identity,
        token_digest,
        true,
        true,
        deadline,
    )
}

fn recover_pending_adoption_without_pruning(
    checkout_dir: &Path,
    challenge_dir: &Path,
    identity: &CheckoutIdentity,
    token_digest: &str,
    deadline: Instant,
) -> Result<PendingRecovery> {
    recover_pending_adoption_with(
        checkout_dir,
        challenge_dir,
        identity,
        token_digest,
        true,
        false,
        deadline,
    )
}

fn inspect_pending_adoption_until(
    checkout_dir: &Path,
    challenge_dir: &Path,
    identity: &CheckoutIdentity,
    token_digest: &str,
    deadline: Instant,
) -> Result<PendingRecovery> {
    recover_pending_adoption_with(
        checkout_dir,
        challenge_dir,
        identity,
        token_digest,
        false,
        false,
        deadline,
    )
}

fn recover_pending_adoption_with(
    checkout_dir: &Path,
    challenge_dir: &Path,
    identity: &CheckoutIdentity,
    token_digest: &str,
    finalize_committed: bool,
    prune_revocations: bool,
    deadline: Instant,
) -> Result<PendingRecovery> {
    ensure_deadline(deadline)?;
    let pending_path = pending_adoption_path(checkout_dir, token_digest);
    let Some(pending_bytes) = read_optional_private_with_limit(
        &pending_path,
        "pending adoption",
        MAX_PENDING_STATE_FILE_BYTES,
    )?
    else {
        return Ok(PendingRecovery::None);
    };
    let pending: PendingAdoptionRecord = serde_json::from_slice(&pending_bytes).map_err(|_| {
        domain_error(
            DirtyCheckoutErrorKind::MalformedState,
            "pending dirty-checkout adoption state is malformed",
        )
    })?;
    validate_pending_adoption(&pending, identity, token_digest)?;

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
    let challenge_path = challenge_dir.join(format!("{token_digest}.json"));
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
        if let Some(challenge) = load_pending_challenge(
            &challenge_path,
            identity,
            token_digest,
            &pending.challenge_digest,
        )? && challenge.token_digest != pending.token_digest
        {
            return Err(domain_error(
                DirtyCheckoutErrorKind::MalformedState,
                "revoked adoption has an inconsistent live challenge",
            ));
        }
        if let Some(spent) = load_pending_challenge(
            &spent_challenge_path,
            identity,
            token_digest,
            &pending.challenge_digest,
        )? && (spent.token_digest != pending.token_digest
            || spent.session_key != revoked.session_key()
            || spent.snapshot_id != pending.snapshot_id
            || spent.authorization_turn_digest != adoption.authorization_turn_digest
            || spent.issued_at != adoption.challenge_issued_at
            || adoption.adopted_at < spent.issued_at
            || adoption.adopted_at >= spent.expires_at)
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
            validate_receipt_matches_lease(&receipt, &revoked)?;
        }
        restore_pending_predecessor_lease(checkout_dir, &pending, identity)?;
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
        if prune_revocations {
            prune_revocation_tombstones(checkout_dir, identity, &pending.receipt_id, deadline)?;
        }
        fs::remove_file(&pending_path)
            .context("revoked adoption recovery marker cleanup failed")?;
        sync_directory(checkout_dir)?;
        return Ok(PendingRecovery::Revoked);
    }

    let staged_path = staged_lease_path(checkout_dir, &pending.receipt_id);
    if let Some(staged_bytes) = read_optional_private(&staged_path, "staged adoption lease")? {
        let staged = parse_lease(&staged_bytes)?;
        validate_lease(&staged, identity)?;
        let adoption = staged.adoption().ok_or_else(|| {
            domain_error(
                DirtyCheckoutErrorKind::MalformedState,
                "staged adoption lease has no adoption identity",
            )
        })?;
        if staged.session_key() != pending.session_key
            || staged.checkout_instance() != pending.checkout_instance
            || adoption.receipt_id != pending.receipt_id
            || adoption.snapshot_id != pending.snapshot_id
            || adoption.challenge_digest != pending.challenge_digest
        {
            return Err(domain_error(
                DirtyCheckoutErrorKind::MalformedState,
                "staged adoption lease does not match pending state",
            ));
        }
        if load_pending_challenge(
            &challenge_path,
            identity,
            token_digest,
            &pending.challenge_digest,
        )?
        .is_some()
        {
            return Err(domain_error(
                DirtyCheckoutErrorKind::MalformedState,
                "staged adoption has an inconsistent live challenge",
            ));
        }
        let artifacts = expired_predecessor(checkout_dir, identity, &staged)?.ok_or_else(|| {
            domain_error(
                DirtyCheckoutErrorKind::MalformedState,
                "staged pending adoption artifacts are incomplete or malformed",
            )
        })?;
        if artifacts.receipt_id != pending.receipt_id
            || artifacts.receipt_digest.is_none()
            || artifacts.spent_challenge_digest.is_none()
        {
            return Err(domain_error(
                DirtyCheckoutErrorKind::MalformedState,
                "staged pending adoption artifacts are incomplete or inconsistent",
            ));
        }
        return Ok(PendingRecovery::Staged);
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
        if artifacts.receipt_id != pending.receipt_id
            || artifacts.receipt_digest.is_none()
            || artifacts.spent_challenge_digest.is_none()
        {
            return Err(domain_error(
                DirtyCheckoutErrorKind::MalformedState,
                "committed pending adoption artifacts are incomplete or inconsistent",
            ));
        }
        if !finalize_committed {
            return Ok(PendingRecovery::Committed);
        }
        if let Some(predecessor_receipt_id) = &pending.predecessor_receipt_id {
            let predecessor = load_bound_predecessor(
                &receipts_dir,
                predecessor_receipt_id,
                pending.predecessor_receipt_digest.as_deref(),
                pending.predecessor_spent_challenge_digest.as_deref(),
            )?;
            cleanup_expired_predecessor(&predecessor)?;
        }
        fs::remove_file(&pending_path)
            .context("committed adoption recovery marker cleanup failed")?;
        sync_directory(checkout_dir)?;
        return Ok(PendingRecovery::Committed);
    }

    let current_challenge = load_pending_challenge(
        &challenge_path,
        identity,
        token_digest,
        &pending.challenge_digest,
    )?;
    let spent_challenge = load_pending_challenge(
        &spent_challenge_path,
        identity,
        token_digest,
        &pending.challenge_digest,
    )?;
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
    read_optional_private_with_limit(path, label, MAX_STATE_FILE_BYTES)
}

fn read_optional_private_with_limit(
    path: &Path,
    label: &str,
    max_bytes: usize,
) -> Result<Option<Vec<u8>>> {
    match read_private_regular(path, max_bytes, true) {
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
    token_digest: &str,
    challenge_digest: &str,
) -> Result<Option<ChallengeRecord>> {
    let Some(bytes) = read_optional_private(path, "pending adoption challenge")? else {
        return Ok(None);
    };
    if sha256_hex(&bytes) != challenge_digest {
        return Err(domain_error(
            DirtyCheckoutErrorKind::MalformedState,
            "pending adoption challenge artifact does not match its recovery marker",
        ));
    }
    let challenge: ChallengeRecord = serde_json::from_slice(&bytes).map_err(|_| {
        domain_error(
            DirtyCheckoutErrorKind::MalformedState,
            "pending adoption challenge state is malformed",
        )
    })?;
    validate_challenge_identity(&challenge, identity, token_digest)?;
    Ok(Some(challenge))
}

fn validate_pending_adoption(
    pending: &PendingAdoptionRecord,
    identity: &CheckoutIdentity,
    token_digest: &str,
) -> Result<()> {
    let valid = pending.schema == PENDING_ADOPTION_SCHEMA
        && is_lower_hex(&pending.receipt_id, 64)
        && is_lower_hex(&pending.token_digest, 64)
        && is_lower_hex(&pending.challenge_digest, 64)
        && is_lower_hex(&pending.session_key, 64)
        && is_lower_hex(&pending.checkout_instance, 32)
        && is_lower_hex(&pending.snapshot_id, 64)
        && pending.token_digest == token_digest
        && pending.checkout_instance == identity.checkout_instance
        && match (
            &pending.predecessor_receipt_id,
            &pending.predecessor_receipt_digest,
            &pending.predecessor_spent_challenge_digest,
        ) {
            (None, None, None) => true,
            (Some(receipt_id), receipt_digest, spent_digest) => {
                is_lower_hex(receipt_id, 64)
                    && receipt_id != &pending.receipt_id
                    && receipt_digest
                        .as_ref()
                        .is_none_or(|digest| is_lower_hex(digest, 64))
                    && spent_digest
                        .as_ref()
                        .is_none_or(|digest| is_lower_hex(digest, 64))
            }
            (None, _, _) => false,
        };
    if !valid {
        return Err(domain_error(
            DirtyCheckoutErrorKind::MalformedState,
            "pending dirty-checkout adoption state is malformed",
        ));
    }
    validate_pending_predecessor_lease(pending, identity)?;
    Ok(())
}

fn validate_pending_predecessor_lease<'a>(
    pending: &'a PendingAdoptionRecord,
    identity: &CheckoutIdentity,
) -> Result<Option<&'a [u8]>> {
    let (digest, bytes) = match (
        pending.predecessor_lease_digest.as_deref(),
        pending.predecessor_lease_bytes.as_deref(),
    ) {
        (None, None) => return Ok(None),
        (Some(digest), Some(bytes)) => (digest, bytes),
        _ => {
            return Err(domain_error(
                DirtyCheckoutErrorKind::MalformedState,
                "pending predecessor lease binding is incomplete",
            ));
        }
    };
    let lease = parse_lease(bytes)?;
    validate_lease(&lease, identity)?;
    let predecessor_matches = match (lease.adoption(), pending.predecessor_receipt_id.as_deref()) {
        (None, None) => lease.schema() == LEASE_V1_SCHEMA,
        (Some(adoption), Some(receipt_id)) => adoption.receipt_id == receipt_id,
        _ => false,
    };
    if !is_lower_hex(digest, 64)
        || sha256_hex(bytes) != digest
        || lease.checkout_instance() != pending.checkout_instance
        || !predecessor_matches
    {
        return Err(domain_error(
            DirtyCheckoutErrorKind::MalformedState,
            "pending predecessor lease does not match its recovery marker",
        ));
    }
    Ok(Some(bytes))
}

fn restore_pending_predecessor_lease(
    checkout_dir: &Path,
    pending: &PendingAdoptionRecord,
    identity: &CheckoutIdentity,
) -> Result<()> {
    let expected = validate_pending_predecessor_lease(pending, identity)?;
    let lease_path = checkout_dir.join("lease.json");
    match (
        read_optional_private(&lease_path, "restored predecessor lease")?,
        expected,
    ) {
        (None, Some(bytes)) => write_state_atomic(&lease_path, bytes, false)
            .context("exact predecessor lease restoration failed"),
        (Some(actual), Some(expected)) if actual == expected => Ok(()),
        (None, None) => Ok(()),
        _ => Err(domain_error(
            DirtyCheckoutErrorKind::MalformedState,
            "revoked adoption predecessor lease state is inconsistent",
        )),
    }
}

fn transition_result_after_recovery<T>(
    result: Result<T>,
    checkout_dir: &Path,
    challenge_dir: &Path,
    identity: &CheckoutIdentity,
    token_digest: &str,
    deadline: Instant,
) -> Result<T> {
    result.map_err(|error| {
        transition_failure_after_recovery(
            error,
            checkout_dir,
            challenge_dir,
            identity,
            token_digest,
            deadline,
        )
    })
}

fn transition_failure_after_recovery(
    error: anyhow::Error,
    checkout_dir: &Path,
    challenge_dir: &Path,
    identity: &CheckoutIdentity,
    token_digest: &str,
    deadline: Instant,
) -> anyhow::Error {
    match inspect_pending_adoption_until(
        checkout_dir,
        challenge_dir,
        identity,
        token_digest,
        deadline,
    ) {
        Ok(PendingRecovery::None | PendingRecovery::RolledBack) => error,
        Ok(PendingRecovery::Staged | PendingRecovery::Committed | PendingRecovery::Revoked) => {
            domain_error(
                DirtyCheckoutErrorKind::AdoptionFailed,
                format!(
                    "adoption transition retained recovery state while handling a failure: {error}"
                ),
            )
        }
        Err(recovery_error) => domain_error(
            DirtyCheckoutErrorKind::AdoptionFailed,
            format!(
                "adoption transition failed and requires recovery: {error}; recovery failed: {recovery_error}"
            ),
        ),
    }
}
fn publish_staged_lease(
    staged_path: &Path,
    authoritative_path: &Path,
    expected: &LeaseRecord,
    expected_previous_bytes: Option<&[u8]>,
    identity: &CheckoutIdentity,
) -> LeaseInstallOutcome {
    let actual_previous_bytes =
        match read_optional_private(authoritative_path, "authoritative lease") {
            Ok(actual) => actual,
            Err(error) => return LeaseInstallOutcome::NotInstalled(error),
        };
    if actual_previous_bytes.as_deref() != expected_previous_bytes {
        let error = match actual_previous_bytes.as_deref() {
            Some(bytes) => {
                match parse_lease(bytes).and_then(|lease| validate_lease(&lease, identity)) {
                    Ok(()) => domain_error(
                        DirtyCheckoutErrorKind::ForeignLease,
                        "authoritative checkout lease changed before staged publication",
                    ),
                    Err(error) => error,
                }
            }
            None => domain_error(
                DirtyCheckoutErrorKind::AdoptionFailed,
                "authoritative checkout lease disappeared before staged publication",
            ),
        };
        return LeaseInstallOutcome::NotInstalled(error);
    }
    let previous = match expected_previous_bytes.map(parse_lease).transpose() {
        Ok(previous) => previous,
        Err(error) => return LeaseInstallOutcome::NotInstalled(error),
    };
    install_lease_with(
        authoritative_path,
        expected,
        previous.as_ref(),
        identity,
        || {
            let staged = load_lease(staged_path)?.ok_or_else(|| {
                domain_error(
                    DirtyCheckoutErrorKind::MalformedState,
                    "staged checkout lease is missing",
                )
            })?;
            validate_lease(&staged, identity)?;
            ensure!(
                staged == *expected,
                "staged checkout lease changed before publication"
            );
            fs::rename(staged_path, authoritative_path)
                .context("staged checkout lease could not be published")?;
            sync_directory(
                authoritative_path
                    .parent()
                    .context("checkout lease has no parent directory")?,
            )
        },
    )
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
            LeaseInstallOutcome::Installed
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
    revoke_dirty_inner_until(
        checkout,
        state_root,
        receipt_id,
        deadline_after(SNAPSHOT_TIMEOUT)?,
    )
}

fn revoke_dirty_inner_until(
    checkout: &Path,
    state_root: &Path,
    receipt_id: &str,
    deadline: Instant,
) -> Result<()> {
    if !is_lower_hex(receipt_id, 64) {
        return Err(domain_error(
            DirtyCheckoutErrorKind::InvalidInput,
            "receipt ID is malformed",
        ));
    }
    let identity = resolve_checkout(checkout, false, deadline)?;
    let checkout_dir = checkout_state_dir(state_root, &identity)?;
    let receipts_dir = checkout_dir.join("receipts");
    verify_private_directory(&receipts_dir).map_err(|error| {
        command_error(
            error,
            DirtyCheckoutErrorKind::MalformedState,
            "dirty-checkout receipt directory is untrusted",
        )
    })?;
    let challenge_dir = checkout_dir.join("challenges");
    verify_private_directory(&challenge_dir).map_err(|error| {
        command_error(
            error,
            DirtyCheckoutErrorKind::MalformedState,
            "dirty-checkout challenge directory is untrusted",
        )
    })?;
    let _lock = LeaseLock::acquire_until(&checkout_dir, deadline)?;
    if recover_revocation_tombstone(
        &checkout_dir,
        &challenge_dir,
        &receipts_dir,
        &identity,
        receipt_id,
        deadline,
    )? {
        return Ok(());
    }

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
    validate_receipt_matches_lease(&receipt, &lease)?;

    let spent_path = receipts_dir.join(format!(".challenge-{receipt_id}.json"));
    let spent_bytes =
        read_private_regular(&spent_path, MAX_STATE_FILE_BYTES, true).map_err(|error| {
            command_error(
                error,
                DirtyCheckoutErrorKind::MalformedState,
                "dirty-checkout spent challenge is unavailable",
            )
        })?;
    let spent: ChallengeRecord = serde_json::from_slice(&spent_bytes).map_err(|_| {
        domain_error(
            DirtyCheckoutErrorKind::MalformedState,
            "dirty-checkout spent challenge is malformed",
        )
    })?;
    validate_spent_challenge_matches_lease(&spent_bytes, &spent, &identity, &lease)?;

    match recover_pending_adoption_until(
        &checkout_dir,
        &challenge_dir,
        &identity,
        &spent.token_digest,
        deadline,
    )? {
        PendingRecovery::None | PendingRecovery::Committed => {}
        PendingRecovery::RolledBack | PendingRecovery::Staged | PendingRecovery::Revoked => {
            return Err(domain_error(
                DirtyCheckoutErrorKind::MalformedState,
                "active adoption changed while revocation was prepared",
            ));
        }
    }

    ensure_deadline(deadline)?;
    let revoked_path = checkout_dir.join(format!(".revoked-{receipt_id}.json"));
    fs::rename(&lease_path, &revoked_path).context("failed to revoke adopted checkout lease")?;
    sync_directory(&checkout_dir).context("revoked lease tombstone could not be made durable")?;
    if !recover_revocation_tombstone(
        &checkout_dir,
        &challenge_dir,
        &receipts_dir,
        &identity,
        receipt_id,
        deadline,
    )? {
        return Err(domain_error(
            DirtyCheckoutErrorKind::RevocationFailed,
            "durable revocation tombstone could not be recovered",
        ));
    }
    Ok(())
}

fn validate_receipt_matches_lease(receipt: &ReceiptRecord, lease: &LeaseRecord) -> Result<()> {
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
            "receipt does not match the dirty-checkout adoption",
        ));
    }
    Ok(())
}

fn validate_spent_challenge_matches_lease(
    bytes: &[u8],
    spent: &ChallengeRecord,
    identity: &CheckoutIdentity,
    lease: &LeaseRecord,
) -> Result<()> {
    validate_challenge_identity(spent, identity, &spent.token_digest)?;
    let adoption = lease.adoption().ok_or_else(|| {
        domain_error(
            DirtyCheckoutErrorKind::MalformedState,
            "checkout lease has no dirty adoption",
        )
    })?;
    if lease.schema() != LEASE_V2_SCHEMA
        || sha256_hex(bytes) != adoption.challenge_digest
        || spent.session_key != lease.session_key()
        || spent.snapshot_id != adoption.snapshot_id
        || spent.authorization_turn_digest != adoption.authorization_turn_digest
        || spent.issued_at != adoption.challenge_issued_at
        || adoption.adopted_at < spent.issued_at
        || adoption.adopted_at >= spent.expires_at
    {
        return Err(domain_error(
            DirtyCheckoutErrorKind::MalformedState,
            "spent challenge does not match the dirty-checkout adoption",
        ));
    }
    Ok(())
}

fn recover_revocation_tombstone(
    checkout_dir: &Path,
    challenge_dir: &Path,
    receipts_dir: &Path,
    identity: &CheckoutIdentity,
    receipt_id: &str,
    deadline: Instant,
) -> Result<bool> {
    ensure_deadline(deadline)?;
    let tombstone_path = checkout_dir.join(format!(".revoked-{receipt_id}.json"));
    let Some(tombstone_bytes) = read_optional_private(&tombstone_path, "revoked adoption")? else {
        return Ok(false);
    };
    let tombstone = parse_lease(&tombstone_bytes)?;
    validate_lease(&tombstone, identity)?;
    let adoption = tombstone.adoption().ok_or_else(|| {
        domain_error(
            DirtyCheckoutErrorKind::MalformedState,
            "revoked adoption tombstone has no adoption identity",
        )
    })?;
    if adoption.receipt_id != receipt_id {
        return Err(domain_error(
            DirtyCheckoutErrorKind::MalformedState,
            "revoked adoption tombstone receipt identity is inconsistent",
        ));
    }

    let mut budget = RecoveryScanBudget::new(deadline);
    let mut token_digests = Vec::new();
    for entry in fs::read_dir(checkout_dir).context("revocation recovery state is unavailable")? {
        let entry = entry.context("revocation recovery state is unreadable")?;
        let name = entry.file_name();
        let token_digest = name
            .to_str()
            .and_then(|name| name.strip_prefix(".pending-adoption-"))
            .and_then(|name| name.strip_suffix(".json"))
            .filter(|digest| is_lower_hex(digest, 64));
        budget.charge(&entry, token_digest.is_some())?;
        if let Some(token_digest) = token_digest {
            token_digests.push(token_digest.to_string());
        }
    }
    budget.check_deadline()?;

    for token_digest in token_digests {
        budget.check_deadline()?;
        let Some(pending_bytes) = read_optional_private_with_limit(
            &pending_adoption_path(checkout_dir, &token_digest),
            "pending adoption",
            MAX_PENDING_STATE_FILE_BYTES,
        )?
        else {
            continue;
        };
        let pending: PendingAdoptionRecord =
            serde_json::from_slice(&pending_bytes).map_err(|_| {
                domain_error(
                    DirtyCheckoutErrorKind::MalformedState,
                    "pending dirty-checkout adoption state is malformed",
                )
            })?;
        validate_pending_adoption(&pending, identity, &token_digest)?;
        if pending.receipt_id == receipt_id {
            if pending.challenge_digest != adoption.challenge_digest
                || recover_pending_adoption_until(
                    checkout_dir,
                    challenge_dir,
                    identity,
                    &token_digest,
                    deadline,
                )? != PendingRecovery::Revoked
            {
                return Err(domain_error(
                    DirtyCheckoutErrorKind::MalformedState,
                    "revoked adoption pending state is inconsistent",
                ));
            }
            break;
        }
    }
    budget.check_deadline()?;

    let receipt_path = receipts_dir.join(format!("{receipt_id}.json"));
    if let Some(bytes) = read_optional_private(&receipt_path, "revoked adoption receipt")? {
        let receipt: ReceiptRecord = serde_json::from_slice(&bytes).map_err(|_| {
            domain_error(
                DirtyCheckoutErrorKind::MalformedState,
                "revoked adoption receipt is malformed",
            )
        })?;
        validate_receipt(&receipt, identity, receipt_id)?;
        validate_receipt_matches_lease(&receipt, &tombstone)?;
    }
    let spent_path = receipts_dir.join(format!(".challenge-{receipt_id}.json"));
    let mut token_digest = None;
    if let Some(bytes) = read_optional_private(&spent_path, "revoked adoption challenge")? {
        let spent: ChallengeRecord = serde_json::from_slice(&bytes).map_err(|_| {
            domain_error(
                DirtyCheckoutErrorKind::MalformedState,
                "revoked adoption challenge is malformed",
            )
        })?;
        validate_spent_challenge_matches_lease(&bytes, &spent, identity, &tombstone)?;
        token_digest = Some(spent.token_digest);
    }
    budget.check_deadline()?;
    for path in [&receipt_path, &spent_path] {
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("revoked adoption artifact cleanup failed"),
        }
    }
    if let Some(token_digest) = token_digest {
        let challenge_path = challenge_dir.join(format!("{token_digest}.json"));
        match fs::remove_file(&challenge_path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("revoked adoption challenge cleanup failed"),
        }
    }
    sync_directory(receipts_dir)
        .context("revoked adoption receipt cleanup could not be made durable")?;
    sync_directory(challenge_dir)
        .context("revoked adoption challenge cleanup could not be made durable")?;
    sync_directory(checkout_dir)
        .context("revoked adoption tombstone could not be confirmed durable")?;
    prune_revocation_tombstones(checkout_dir, identity, receipt_id, deadline)?;
    Ok(true)
}

fn prune_revocation_tombstones(
    checkout_dir: &Path,
    identity: &CheckoutIdentity,
    current_receipt_id: &str,
    deadline: Instant,
) -> Result<()> {
    let mut protected = HashSet::from([current_receipt_id.to_string()]);
    let mut candidates = HashSet::new();
    let mut budget = RecoveryScanBudget::new(deadline);
    for entry in fs::read_dir(checkout_dir).context("revocation retention state is unavailable")? {
        let entry = entry.context("revocation retention state entry is unreadable")?;
        let name = entry.file_name();
        let token_digest = name
            .to_str()
            .and_then(|name| name.strip_prefix(".pending-adoption-"))
            .and_then(|name| name.strip_suffix(".json"))
            .filter(|digest| is_lower_hex(digest, 64));
        let tombstone_receipt = name
            .to_str()
            .and_then(|name| name.strip_prefix(".revoked-"))
            .and_then(|name| name.strip_suffix(".json"))
            .filter(|receipt_id| is_lower_hex(receipt_id, 64));
        budget.charge(
            &entry,
            token_digest.is_some() || tombstone_receipt.is_some(),
        )?;
        if let Some(receipt_id) = tombstone_receipt {
            candidates.insert(receipt_id.to_string());
        }
        let Some(token_digest) = token_digest else {
            continue;
        };
        let bytes = read_private_regular(&entry.path(), MAX_PENDING_STATE_FILE_BYTES, true)?;
        let pending: PendingAdoptionRecord = serde_json::from_slice(&bytes).map_err(|_| {
            domain_error(
                DirtyCheckoutErrorKind::MalformedState,
                "pending adoption tombstone reference is malformed",
            )
        })?;
        validate_pending_adoption(&pending, identity, token_digest)?;
        candidates.insert(pending.receipt_id.clone());
        protected.insert(pending.receipt_id);
    }
    budget.check_deadline()?;
    prune_revocation_tombstone_candidates(checkout_dir, identity, &protected, candidates, deadline)
}

fn prune_revocation_tombstone_candidates<I>(
    checkout_dir: &Path,
    identity: &CheckoutIdentity,
    protected: &HashSet<String>,
    candidates: I,
    deadline: Instant,
) -> Result<()>
where
    I: IntoIterator<Item = String>,
{
    let mut tombstones = Vec::new();
    for receipt_id in candidates {
        ensure_deadline(deadline)?;
        let path = checkout_dir.join(format!(".revoked-{receipt_id}.json"));
        let Some(bytes) = read_optional_private(&path, "revocation tombstone")? else {
            continue;
        };
        let lease = parse_lease(&bytes)?;
        validate_lease(&lease, identity)?;
        let adoption = lease.adoption().ok_or_else(|| {
            domain_error(
                DirtyCheckoutErrorKind::MalformedState,
                "revocation tombstone has no adoption identity",
            )
        })?;
        if adoption.receipt_id != receipt_id {
            return Err(domain_error(
                DirtyCheckoutErrorKind::MalformedState,
                "revocation tombstone filename does not match its adoption identity",
            ));
        }
        tombstones.push((
            protected.contains(&receipt_id),
            adoption.adopted_at,
            receipt_id,
        ));
    }
    tombstones.sort_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then_with(|| left.1.cmp(&right.1))
            .then_with(|| left.2.cmp(&right.2))
    });
    let remove_count = tombstones.len().saturating_sub(MAX_REVOCATION_TOMBSTONES);
    let mut removed = false;
    for (_, _, receipt_id) in tombstones
        .into_iter()
        .filter(|(protected, _, _)| !protected)
        .take(remove_count)
    {
        ensure_deadline(deadline)?;
        fs::remove_file(checkout_dir.join(format!(".revoked-{receipt_id}.json")))
            .context("expired revocation tombstone cleanup failed")?;
        removed = true;
    }
    if removed {
        sync_directory(checkout_dir)
            .context("revocation tombstone retention could not be made durable")?;
    }
    ensure_deadline(deadline)
}

fn snapshot_once(checkout: &Path, deadline: Instant) -> Result<SnapshotPass> {
    ensure_deadline(deadline)?;
    let identity = resolve_checkout(checkout, true, deadline)?;
    let mut budget = SnapshotBudget::default();
    reject_command_bearing_git_config(&mut budget, &identity.root, deadline)?;
    reject_active_git_operation(&identity)?;

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

    reject_initialized_submodule_config(&identity.root, &tracked, &mut budget, deadline)?;

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

fn reject_command_bearing_git_config(
    budget: &mut SnapshotBudget,
    cwd: &Path,
    deadline: Instant,
) -> Result<()> {
    const COMMAND_BEARING_CONFIG: &str = r"^(include\.path|includeif\..*\.path|filter\..*\.(clean|smudge|process)|diff\..*\.(command|textconv))$";
    let output = snapshot_git_output(
        budget,
        cwd,
        &[
            "config",
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

fn validate_git_toplevel(
    budget: &mut SnapshotBudget,
    expected: &Path,
    deadline: Instant,
) -> Result<()> {
    let output = snapshot_git_output(
        budget,
        expected,
        &["rev-parse", "--show-toplevel"],
        1024 * 1024,
        deadline,
    )?;
    ensure!(
        output.status.success(),
        "submodule Git worktree identity is unavailable"
    );
    let resolved = canonical_git_path(expected, strip_git_line(&output.stdout)?)?;
    ensure!(
        resolved == expected,
        "submodule Git worktree identity does not match its index path"
    );
    Ok(())
}

fn reject_initialized_submodule_config(
    root: &Path,
    tracked: &[IndexEntry],
    budget: &mut SnapshotBudget,
    deadline: Instant,
) -> Result<()> {
    let mut visited = HashSet::new();
    reject_initialized_submodule_config_inner(root, tracked, budget, deadline, &mut visited)
}

fn reject_initialized_submodule_config_inner(
    root: &Path,
    tracked: &[IndexEntry],
    budget: &mut SnapshotBudget,
    deadline: Instant,
    visited: &mut HashSet<PathBuf>,
) -> Result<()> {
    for entry in tracked
        .iter()
        .filter(|entry| entry.mode.as_slice() == b"160000")
    {
        ensure_deadline(deadline)?;
        let path = root.join(OsStr::from_bytes(&entry.path));
        let result = (|| -> Result<()> {
            verify_parent_within_root(root, &path)?;
            let metadata = match fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
                Err(error) => {
                    return Err(error).context("submodule metadata is unavailable");
                }
            };
            ensure!(
                metadata.file_type().is_dir() && !metadata.file_type().is_symlink(),
                "submodule checkout path is not a directory"
            );
            let canonical =
                fs::canonicalize(&path).context("submodule path could not be verified")?;
            ensure!(
                canonical.starts_with(root),
                "submodule path escapes the checkout"
            );
            ensure!(
                visited.insert(canonical.clone()),
                "submodule checkout graph contains a cycle"
            );
            validate_git_toplevel(budget, &canonical, deadline)?;
            reject_command_bearing_git_config(budget, &canonical, deadline)?;
            let index_output = snapshot_git_output(
                budget,
                &canonical,
                &["ls-files", "--stage", "-z"],
                MAX_GIT_OUTPUT_BYTES,
                deadline,
            )?;
            ensure!(
                index_output.status.success(),
                "submodule index listing failed"
            );
            let nested = parse_index_bounded(&index_output.stdout, MAX_ENTRY_COUNT, budget)?;
            drop(index_output);
            let flags_output = snapshot_git_output(
                budget,
                &canonical,
                &["ls-files", "-v", "-z"],
                MAX_GIT_OUTPUT_BYTES,
                deadline,
            )?;
            ensure!(
                flags_output.status.success(),
                "submodule index flags are unavailable"
            );
            validate_index_flags(&flags_output.stdout, &nested, budget)?;
            reject_initialized_submodule_config_inner(
                &canonical, &nested, budget, deadline, visited,
            )
        })();
        if let Err(error) = result {
            return Err(command_error(
                error,
                DirtyCheckoutErrorKind::UnsupportedGitState,
                "submodule Git configuration is unsupported",
            ));
        }
    }
    Ok(())
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
    let requested_root = requested_checkout_root(&requested)?;
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
    if root != requested_root {
        return Err(domain_error(
            DirtyCheckoutErrorKind::UnsupportedGitState,
            "Git checkout top-level does not match the requested physical checkout",
        ));
    }
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

fn requested_checkout_root(requested: &Path) -> Result<PathBuf> {
    for ancestor in requested.ancestors() {
        let marker = ancestor.join(".git");
        match fs::symlink_metadata(&marker) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(domain_error(
                    DirtyCheckoutErrorKind::UnsupportedGitState,
                    "requested checkout has a symbolic Git identity marker",
                ));
            }
            Ok(metadata) if metadata.is_file() || metadata.is_dir() => {
                return fs::canonicalize(ancestor)
                    .context("requested checkout root could not be canonicalized");
            }
            Ok(_) => {
                return Err(domain_error(
                    DirtyCheckoutErrorKind::UnsupportedGitState,
                    "requested checkout has an unsupported Git identity marker",
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).context("requested checkout Git identity is unavailable");
            }
        }
    }
    Err(domain_error(
        DirtyCheckoutErrorKind::UnsupportedGitState,
        "requested path is not inside a physical Git checkout",
    ))
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
            budget.charge_traversal_entry()?;
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
            if submodule {
                return Err(domain_error(
                    DirtyCheckoutErrorKind::UnsupportedGitState,
                    "dirty or unavailable submodules are unsupported",
                ));
            }
            hasher.field(b"worktree_kind", b"missing");
            return Ok(());
        }
        Err(error) => return Err(error).context("worktree object metadata is unavailable"),
    };

    if submodule {
        let result = (|| -> Result<()> {
            ensure!(
                before.file_type().is_dir(),
                "dirty or unavailable submodules are unsupported"
            );
            let canonical =
                fs::canonicalize(&path).context("submodule path could not be verified")?;
            ensure!(
                canonical.starts_with(root),
                "submodule path escapes the checkout"
            );
            validate_git_toplevel(budget, &canonical, deadline)?;
            reject_command_bearing_git_config(budget, &canonical, deadline)?;
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
            Ok(())
        })();
        return result.map_err(|error| {
            command_error(
                error,
                DirtyCheckoutErrorKind::UnsupportedGitState,
                "dirty or unavailable submodules are unsupported",
            )
        });
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
    match lease {
        LeaseRecord::V1(lease) => {
            validate_lease_text_path(&lease.checkout_root, &identity.root, "checkout lease root")?;
            validate_lease_text_path(
                &lease.checkout_git_dir,
                &identity.git_dir,
                "checkout lease Git directory",
            )?;
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

fn lease_path_text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
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
        || text != lease_path_text(expected)
        || bytes.as_slice() != expected.as_os_str().as_bytes()
    {
        return Err(domain_error(
            DirtyCheckoutErrorKind::MalformedState,
            format!("{label} does not match the compatible text or current checkout"),
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
    write_json_atomic_with_limit(path, value, replace, MAX_STATE_FILE_BYTES)
}

fn write_json_atomic_with_limit(
    path: &Path,
    value: &impl Serialize,
    replace: bool,
    max_bytes: usize,
) -> Result<()> {
    let mut payload = serde_json::to_vec(value).context("state serialization failed")?;
    payload.push(b'\n');
    write_state_atomic_with_limit(path, &payload, replace, max_bytes)
}

fn write_state_atomic(path: &Path, payload: &[u8], replace: bool) -> Result<()> {
    write_state_atomic_with_limit(path, payload, replace, MAX_STATE_FILE_BYTES)
}

fn write_state_atomic_with_limit(
    path: &Path,
    payload: &[u8],
    replace: bool,
    max_bytes: usize,
) -> Result<()> {
    ensure!(
        !payload.is_empty() && payload.len() <= max_bytes,
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
        file.write_all(payload).context("state file write failed")?;
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
    fn acquire_until(directory: &Path, transaction_deadline: Instant) -> Result<Self> {
        ensure_deadline(transaction_deadline)?;
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
            if started.elapsed() >= LOCK_WAIT || Instant::now() >= transaction_deadline {
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
        .arg("-c")
        .arg("core.trustctime=true")
        .arg("-c")
        .arg("core.checkStat=default")
        .arg("-c")
        .arg("core.fileMode=true")
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

struct SpawnedOutputChild {
    child: Child,
    supervised: bool,
    cleanup_proof: Option<std::os::unix::net::UnixStream>,
    fallback_owner: Option<ProcessOwner>,
}

fn spawn_output_child(command: &mut Command, deadline: Instant) -> Result<SpawnedOutputChild> {
    let is_snapshot_worker = command.get_envs().any(|(name, value)| {
        name == OsStr::new(SNAPSHOT_WORKER_ENV) && value == Some(OsStr::new("1"))
    });
    if is_snapshot_worker {
        return command
            .spawn()
            .map(|child| SpawnedOutputChild {
                child,
                supervised: false,
                cleanup_proof: None,
                fallback_owner: None,
            })
            .context("Git command could not be started");
    }
    let running_git_cli = env::var_os(SNAPSHOT_WORKER_ENV).as_deref() == Some(OsStr::new("1"))
        || env::current_exe().is_ok_and(|path| path.file_name() == Some(OsStr::new("git-cli")));
    if !running_git_cli {
        return command
            .spawn()
            .map(|child| SpawnedOutputChild {
                child,
                supervised: false,
                cleanup_proof: None,
                fallback_owner: None,
            })
            .context("Git command could not be started");
    }

    let executable = snapshot_worker_executable()?;
    spawn_supervisor_output_child(command, &executable, deadline)
}

fn spawn_supervisor_output_child(
    command: &mut Command,
    executable: &SnapshotWorkerExecutable,
    deadline: Instant,
) -> Result<SpawnedOutputChild> {
    let program = command.get_program().to_os_string();
    let arguments: Vec<OsString> = command.get_args().map(OsString::from).collect();
    let environment: Vec<(OsString, Option<OsString>)> = command
        .get_envs()
        .map(|(name, value)| (name.to_os_string(), value.map(OsString::from)))
        .collect();
    let current_dir = command.get_current_dir().map(PathBuf::from);
    executable.revalidate()?;
    let deadline_nanos = monotonic_deadline_nanos(deadline)?;
    let (mut capability_sender, capability_receiver) =
        std::os::unix::net::UnixStream::pair().context("process supervisor capability failed")?;
    let capability_descriptor = capability_receiver.as_raw_fd();
    let descriptor_flags = unsafe { libc::fcntl(capability_descriptor, libc::F_GETFD) };
    if descriptor_flags < 0
        || unsafe {
            libc::fcntl(
                capability_descriptor,
                libc::F_SETFD,
                descriptor_flags & !libc::FD_CLOEXEC,
            )
        } < 0
    {
        return Err(process_scan_resource_error());
    }
    let mut supervisor = Command::new(executable.command_path());
    supervisor
        .arg(program)
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    if let Some(current_dir) = current_dir {
        supervisor.current_dir(current_dir);
    }
    for (name, value) in environment {
        match value {
            Some(value) => {
                supervisor.env(name, value);
            }
            None => {
                supervisor.env_remove(name);
            }
        }
    }
    supervisor.env(PROCESS_SUPERVISOR_ENV, "1").env(
        PROCESS_SUPERVISOR_CAPABILITY_FD_ENV,
        capability_descriptor.to_string(),
    );
    let mut child = supervisor
        .spawn()
        .context("Git command could not be started")?;
    let fallback_owner = match ProcessOwner::for_root(child.id() as libc::pid_t) {
        Ok(owner) => owner,
        Err(error) => {
            unsafe {
                let _ = libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL);
            }
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
    };
    drop(capability_receiver);
    if let Err(error) = capability_sender.write_all(PROCESS_SUPERVISOR_CAPABILITY) {
        unsafe {
            let _ = libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL);
        }
        let _ = child.kill();
        let _ = child.wait();
        return Err(error).context("process supervisor capability could not be delivered");
    }
    if let Err(error) = capability_sender.write_all(&deadline_nanos.to_be_bytes()) {
        unsafe {
            let _ = libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL);
        }
        let _ = child.kill();
        let _ = child.wait();
        return Err(error).context("process supervisor deadline could not be delivered");
    }
    Ok(SpawnedOutputChild {
        child,
        supervised: true,
        cleanup_proof: Some(capability_sender),
        fallback_owner: Some(fallback_owner),
    })
}

fn output_with_aggregate_limit_until(
    command: &mut Command,
    deadline: Instant,
    stdout_limit: usize,
    stderr_limit: usize,
    aggregate_limit: usize,
) -> Result<std::process::Output> {
    ensure_deadline(deadline)?;
    let child = spawn_output_child(command, deadline)?;
    collect_output_child_until(child, deadline, stdout_limit, stderr_limit, aggregate_limit)
}

fn collect_output_child_until(
    spawned: SpawnedOutputChild,
    deadline: Instant,
    stdout_limit: usize,
    stderr_limit: usize,
    aggregate_limit: usize,
) -> Result<std::process::Output> {
    let SpawnedOutputChild {
        mut child,
        supervised,
        mut cleanup_proof,
        mut fallback_owner,
    } = spawned;
    let process_group = child.id() as libc::pid_t;
    if Instant::now() >= deadline {
        terminate_child(
            &mut child,
            process_group,
            supervised,
            &mut cleanup_proof,
            &mut fallback_owner,
        );
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
        terminate_child(
            &mut child,
            process_group,
            supervised,
            &mut cleanup_proof,
            &mut fallback_owner,
        );
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
        if let Some(owner) = &mut fallback_owner
            && let Err(error) = owner.identities(deadline)
        {
            terminate_child(
                &mut child,
                process_group,
                supervised,
                &mut cleanup_proof,
                &mut fallback_owner,
            );
            return Err(error);
        }
        if stdout_eof && stderr_eof {
            match child.try_wait() {
                Ok(Some(status)) => {
                    if supervised {
                        let completion = cleanup_proof
                            .as_mut()
                            .ok_or_else(process_scan_resource_error)
                            .and_then(read_process_supervisor_completion);
                        let valid_target = completion.is_ok_and(|completion| {
                            completion.kind == ProcessSupervisorCompletionKind::Target
                                && completion.cleanup_complete
                                && status.code() == Some(completion.exit_code)
                        });
                        if !valid_target {
                            terminate_child(
                                &mut child,
                                process_group,
                                supervised,
                                &mut cleanup_proof,
                                &mut fallback_owner,
                            );
                            return Err(domain_error(
                                DirtyCheckoutErrorKind::ResourceUnavailable,
                                "process supervisor completion could not be authenticated",
                            ));
                        }
                    }
                    return Ok(std::process::Output {
                        status,
                        stdout: stdout_bytes,
                        stderr: stderr_bytes,
                    });
                }
                Ok(None) => {}
                Err(error) => {
                    terminate_child(
                        &mut child,
                        process_group,
                        supervised,
                        &mut cleanup_proof,
                        &mut fallback_owner,
                    );
                    return Err(error).context("Git command status failed");
                }
            }
        }

        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            terminate_child(
                &mut child,
                process_group,
                supervised,
                &mut cleanup_proof,
                &mut fallback_owner,
            );
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
            terminate_child(
                &mut child,
                process_group,
                supervised,
                &mut cleanup_proof,
                &mut fallback_owner,
            );
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
                    terminate_child(
                        &mut child,
                        process_group,
                        supervised,
                        &mut cleanup_proof,
                        &mut fallback_owner,
                    );
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
                    terminate_child(
                        &mut child,
                        process_group,
                        supervised,
                        &mut cleanup_proof,
                        &mut fallback_owner,
                    );
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

const PROCESS_CLEANUP_TIMEOUT: Duration = Duration::from_millis(250);
const PROCESS_SCAN_LIMITS: ProcessScanLimits = ProcessScanLimits {
    #[cfg(target_os = "linux")]
    process_entries: 32_768,
    #[cfg(target_os = "linux")]
    metadata_bytes: 32 * 1024 * 1024,
    descendants: 4_096,
};
const MAX_PROCESS_CLEANUP_GENERATIONS: usize = PROCESS_SCAN_LIMITS.descendants;
#[cfg(target_os = "linux")]
const MAX_PROC_STAT_BYTES: usize = 4 * 1024;
#[cfg(target_os = "linux")]
const MAX_PROC_CHILDREN_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct ProcessIdentity {
    pid: libc::pid_t,
    generation: u128,
}

#[derive(Clone, Copy, Debug)]
struct ProcessRecord {
    identity: ProcessIdentity,
    parent: libc::pid_t,
    #[cfg(all(test, target_os = "linux"))]
    metadata_bytes: usize,
}

impl ProcessRecord {
    fn new(pid: libc::pid_t, parent: libc::pid_t, generation: u128, metadata_bytes: usize) -> Self {
        #[cfg(not(all(test, target_os = "linux")))]
        let _ = metadata_bytes;
        Self {
            identity: ProcessIdentity { pid, generation },
            parent,
            #[cfg(all(test, target_os = "linux"))]
            metadata_bytes,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct ProcessScanLimits {
    #[cfg(target_os = "linux")]
    process_entries: usize,
    #[cfg(target_os = "linux")]
    metadata_bytes: usize,
    descendants: usize,
}

#[derive(Debug)]
struct ProcessScan {
    identities: Vec<ProcessIdentity>,
    #[cfg(all(test, target_os = "linux"))]
    edge_visits: usize,
}

fn process_scan_resource_error() -> anyhow::Error {
    domain_error(
        DirtyCheckoutErrorKind::ResourceUnavailable,
        "process cleanup exceeds the supported resource limit",
    )
}

#[cfg(all(test, target_os = "linux"))]
fn collect_descendant_processes_with<F>(
    root: libc::pid_t,
    deadline: Instant,
    limits: ProcessScanLimits,
    read_records: F,
) -> Result<ProcessScan>
where
    F: FnOnce(Instant) -> Result<Vec<ProcessRecord>>,
{
    ensure_deadline(deadline)?;
    let records = read_records(deadline)?;
    ensure_deadline(deadline)?;
    if records.len() > limits.process_entries {
        return Err(process_scan_resource_error());
    }

    let mut metadata_bytes = 0_usize;
    let mut children: std::collections::HashMap<libc::pid_t, Vec<ProcessIdentity>> =
        std::collections::HashMap::new();
    for record in records {
        ensure_deadline(deadline)?;
        metadata_bytes = metadata_bytes
            .checked_add(record.metadata_bytes)
            .ok_or_else(process_scan_resource_error)?;
        if metadata_bytes > limits.metadata_bytes {
            return Err(process_scan_resource_error());
        }
        children
            .entry(record.parent)
            .or_default()
            .push(record.identity);
    }

    let mut identities = Vec::new();
    let mut seen = HashSet::new();
    let mut frontier = vec![root];
    let mut edge_visits = 0_usize;
    while let Some(parent) = frontier.pop() {
        ensure_deadline(deadline)?;
        let Some(direct) = children.get(&parent) else {
            continue;
        };
        for identity in direct {
            edge_visits = edge_visits
                .checked_add(1)
                .ok_or_else(process_scan_resource_error)?;
            if seen.insert(*identity) {
                if identities.len() >= limits.descendants {
                    return Err(process_scan_resource_error());
                }
                identities.push(*identity);
                frontier.push(identity.pid);
            }
        }
    }
    Ok(ProcessScan {
        identities,
        #[cfg(all(test, target_os = "linux"))]
        edge_visits,
    })
}

#[cfg(target_os = "linux")]
fn pin_process_identity_result_with<H, O, V>(
    identity: ProcessIdentity,
    open: O,
    current_identity: V,
) -> Result<Option<H>>
where
    O: FnOnce(libc::pid_t) -> io::Result<H>,
    V: FnOnce(libc::pid_t) -> Result<Option<ProcessIdentity>>,
{
    let handle = match open(identity.pid) {
        Ok(handle) => handle,
        Err(error) if error.raw_os_error() == Some(libc::ESRCH) => return Ok(None),
        Err(_) => return Err(process_scan_resource_error()),
    };
    Ok((current_identity(identity.pid)? == Some(identity)).then_some(handle))
}

#[cfg(target_os = "linux")]
fn parse_linux_process_record(bytes: &[u8]) -> Option<ProcessRecord> {
    let open = bytes.iter().position(|byte| *byte == b' ')?;
    let close = bytes.iter().rposition(|byte| *byte == b')')?;
    let pid = std::str::from_utf8(bytes.get(..open)?)
        .ok()?
        .parse::<libc::pid_t>()
        .ok()?;
    let mut fields = bytes
        .get(close + 1..)?
        .split(|byte| byte.is_ascii_whitespace())
        .filter(|field| !field.is_empty());
    let parent = std::str::from_utf8(fields.nth(1)?)
        .ok()?
        .parse::<libc::pid_t>()
        .ok()?;
    let generation = std::str::from_utf8(fields.nth(17)?)
        .ok()?
        .parse::<u128>()
        .ok()?;
    Some(ProcessRecord::new(pid, parent, generation, bytes.len()))
}

#[cfg(target_os = "linux")]
fn read_linux_metadata(path: &Path, byte_limit: usize) -> Result<Option<Vec<u8>>> {
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(process_scan_resource_error()),
    };
    let mut bytes = Vec::with_capacity(256.min(byte_limit));
    file.take((byte_limit + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| process_scan_resource_error())?;
    if bytes.len() > byte_limit {
        return Err(process_scan_resource_error());
    }
    Ok(Some(bytes))
}

#[cfg(target_os = "linux")]
fn charge_process_scan(counter: &mut usize, amount: usize, limit: usize) -> Result<()> {
    *counter = counter
        .checked_add(amount)
        .ok_or_else(process_scan_resource_error)?;
    if *counter > limit {
        return Err(process_scan_resource_error());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn read_linux_process_record_at(
    proc_root: &Path,
    pid: libc::pid_t,
    deadline: Instant,
    metadata_bytes: &mut usize,
    limits: ProcessScanLimits,
) -> Result<Option<ProcessRecord>> {
    ensure_deadline(deadline)?;
    let Some(bytes) = read_linux_metadata(
        &proc_root.join(pid.to_string()).join("stat"),
        MAX_PROC_STAT_BYTES,
    )?
    else {
        return Ok(None);
    };
    charge_process_scan(metadata_bytes, bytes.len(), limits.metadata_bytes)?;
    Ok(parse_linux_process_record(&bytes).filter(|record| record.identity.pid == pid))
}

#[cfg(target_os = "linux")]
fn linux_process_identity_matches_at(
    proc_root: &Path,
    expected: ProcessIdentity,
    deadline: Instant,
    metadata_bytes: &mut usize,
    limits: ProcessScanLimits,
) -> Result<bool> {
    Ok(
        read_linux_process_record_at(proc_root, expected.pid, deadline, metadata_bytes, limits)?
            .is_some_and(|record| record.identity == expected),
    )
}

#[cfg(target_os = "linux")]
fn visit_linux_child_identities_at<F>(
    proc_root: &Path,
    parent: ProcessIdentity,
    deadline: Instant,
    process_entries: &mut usize,
    metadata_bytes: &mut usize,
    limits: ProcessScanLimits,
    mut visit: F,
) -> Result<()>
where
    F: FnMut(ProcessIdentity) -> Result<()>,
{
    ensure_deadline(deadline)?;
    if !linux_process_identity_matches_at(proc_root, parent, deadline, metadata_bytes, limits)? {
        return Ok(());
    }
    let task_path = proc_root.join(parent.pid.to_string()).join("task");
    let tasks = match fs::read_dir(task_path) {
        Ok(tasks) => tasks,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err(process_scan_resource_error()),
    };
    for task in tasks {
        ensure_deadline(deadline)?;
        let Ok(task) = task else {
            continue;
        };
        if task
            .file_name()
            .to_str()
            .and_then(|value| value.parse::<libc::pid_t>().ok())
            .is_none()
        {
            continue;
        }
        if !linux_process_identity_matches_at(proc_root, parent, deadline, metadata_bytes, limits)?
        {
            return Ok(());
        }
        charge_process_scan(process_entries, 1, limits.process_entries)?;
        let Some(bytes) =
            read_linux_metadata(&task.path().join("children"), MAX_PROC_CHILDREN_BYTES)?
        else {
            continue;
        };
        charge_process_scan(metadata_bytes, bytes.len(), limits.metadata_bytes)?;
        if !linux_process_identity_matches_at(proc_root, parent, deadline, metadata_bytes, limits)?
        {
            return Ok(());
        }
        let child_pids = bytes
            .split(|byte| byte.is_ascii_whitespace())
            .filter(|field| !field.is_empty())
            .map(|field| {
                ensure_deadline(deadline)?;
                let pid = std::str::from_utf8(field)
                    .ok()
                    .and_then(|field| field.parse::<libc::pid_t>().ok())
                    .ok_or_else(process_scan_resource_error)?;
                charge_process_scan(process_entries, 1, limits.process_entries)?;
                Ok(pid)
            });
        visit_identity_bound_children_with(
            parent,
            child_pids,
            |pid| read_linux_process_record_at(proc_root, pid, deadline, metadata_bytes, limits),
            &mut visit,
        )?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn descendant_processes_at<F>(
    proc_root: &Path,
    root: libc::pid_t,
    deadline: Instant,
    limits: ProcessScanLimits,
    mut observe: F,
) -> Result<ProcessScan>
where
    F: FnMut(ProcessIdentity),
{
    ensure_deadline(deadline)?;
    let mut identities = Vec::new();
    let mut seen = HashSet::new();
    let mut process_entries = 0_usize;
    let mut metadata_bytes = 0_usize;
    let mut edge_visits = 0_usize;
    let Some(root_identity) =
        read_linux_process_record_at(proc_root, root, deadline, &mut metadata_bytes, limits)?
            .map(|record| record.identity)
    else {
        return Ok(ProcessScan {
            identities,
            #[cfg(all(test, target_os = "linux"))]
            edge_visits,
        });
    };
    let mut frontier = vec![root_identity];
    while let Some(parent) = frontier.pop() {
        ensure_deadline(deadline)?;
        visit_linux_child_identities_at(
            proc_root,
            parent,
            deadline,
            &mut process_entries,
            &mut metadata_bytes,
            limits,
            |identity| {
                ensure_deadline(deadline)?;
                edge_visits = edge_visits
                    .checked_add(1)
                    .ok_or_else(process_scan_resource_error)?;
                if !seen.insert(identity) {
                    return Ok(());
                }
                if identities.len() >= limits.descendants {
                    return Err(process_scan_resource_error());
                }
                observe(identity);
                identities.push(identity);
                frontier.push(identity);
                Ok(())
            },
        )?;
    }
    Ok(ProcessScan {
        identities,
        #[cfg(all(test, target_os = "linux"))]
        edge_visits,
    })
}

#[cfg(target_os = "linux")]
fn descendant_processes_with<F>(
    root: libc::pid_t,
    deadline: Instant,
    observe: F,
) -> Result<ProcessScan>
where
    F: FnMut(ProcessIdentity),
{
    descendant_processes_at(
        Path::new("/proc"),
        root,
        deadline,
        PROCESS_SCAN_LIMITS,
        observe,
    )
}

#[cfg(target_os = "linux")]
fn process_identity(pid: libc::pid_t) -> Result<Option<ProcessIdentity>> {
    let Some(bytes) = read_linux_metadata(
        &Path::new("/proc").join(pid.to_string()).join("stat"),
        MAX_PROC_STAT_BYTES,
    )?
    else {
        return Ok(None);
    };
    let record = parse_linux_process_record(&bytes).ok_or_else(process_scan_resource_error)?;
    if record.identity.pid != pid {
        return Err(process_scan_resource_error());
    }
    Ok(Some(record.identity))
}

#[cfg(target_os = "linux")]
struct PinnedProcess {
    pidfd: File,
}

#[cfg(target_os = "linux")]
fn pin_process(identity: ProcessIdentity) -> Result<Option<PinnedProcess>> {
    use std::os::fd::FromRawFd;

    pin_process_identity_result_with(
        identity,
        |pid| {
            let pidfd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0_u32) };
            if pidfd < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(unsafe { File::from_raw_fd(pidfd as libc::c_int) })
        },
        process_identity,
    )
    .map(|pinned| pinned.map(|pidfd| PinnedProcess { pidfd }))
}

#[cfg(target_os = "linux")]
fn signal_pinned_process_result_with<F>(send: F) -> Result<bool>
where
    F: FnOnce() -> io::Result<()>,
{
    match send() {
        Ok(()) => Ok(true),
        Err(error) if error.raw_os_error() == Some(libc::ESRCH) => Ok(false),
        Err(_) => Err(process_scan_resource_error()),
    }
}

#[cfg(target_os = "linux")]
fn signal_pinned_process(process: &PinnedProcess, signal: libc::c_int) -> Result<bool> {
    signal_pinned_process_result_with(|| {
        let status = unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                process.pidfd.as_raw_fd(),
                signal,
                std::ptr::null::<libc::siginfo_t>(),
                0_u32,
            )
        };
        if status == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    })
}

#[cfg(target_os = "linux")]
fn wait_pinned_process_until(process: &PinnedProcess, deadline: Instant) -> Result<()> {
    loop {
        ensure_deadline(deadline)?;
        let remaining = deadline.saturating_duration_since(Instant::now());
        let timeout_millis = remaining.as_millis().min(libc::c_int::MAX as u128) as libc::c_int;
        let mut descriptor = libc::pollfd {
            fd: process.pidfd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let status = unsafe { libc::poll(&mut descriptor, 1, timeout_millis) };
        if status > 0 {
            if descriptor.revents & libc::POLLIN != 0 {
                return Ok(());
            }
            return Err(process_scan_resource_error());
        }
        if status == 0 {
            continue;
        }
        if io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
            return Err(process_scan_resource_error());
        }
    }
}

#[cfg(any(test, target_os = "linux", target_os = "macos"))]
fn visit_identity_bound_children_with<I, R, F>(
    parent: ProcessIdentity,
    children: I,
    mut read_record: R,
    mut visit: F,
) -> Result<()>
where
    I: IntoIterator<Item = Result<libc::pid_t>>,
    R: FnMut(libc::pid_t) -> Result<Option<ProcessRecord>>,
    F: FnMut(ProcessIdentity) -> Result<()>,
{
    for child in children {
        if read_record(parent.pid)?.map(|record| record.identity) != Some(parent) {
            return Ok(());
        }
        let pid = child?;
        let Some(record) = read_record(pid)? else {
            continue;
        };
        if record.parent != parent.pid {
            continue;
        }
        if read_record(parent.pid)?.map(|record| record.identity) != Some(parent) {
            return Ok(());
        }
        visit(record.identity)?;
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn macos_process_record(pid: libc::pid_t) -> Result<Option<ProcessRecord>> {
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let size = std::mem::size_of::<libc::proc_bsdinfo>();
    unsafe {
        *libc::__error() = 0;
    }
    let read = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size as libc::c_int,
        )
    };
    if read == size as libc::c_int {
        let info = unsafe { info.assume_init() };
        return Ok((info.pbi_pid == pid as u32).then(|| {
            ProcessRecord::new(
                pid,
                info.pbi_ppid as libc::pid_t,
                ((info.pbi_start_tvsec as u128) << 64) | info.pbi_start_tvusec as u128,
                size,
            )
        }));
    }
    let error = io::Error::last_os_error();
    if read == 0 && error.raw_os_error() == Some(libc::ESRCH) {
        Ok(None)
    } else {
        Err(process_scan_resource_error())
    }
}

#[cfg(target_os = "macos")]
fn process_identity(pid: libc::pid_t) -> Result<Option<ProcessIdentity>> {
    Ok(macos_process_record(pid)?.map(|record| record.identity))
}

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn task_suspend(target_task: libc::mach_port_t) -> libc::kern_return_t;
    fn task_resume(target_task: libc::mach_port_t) -> libc::kern_return_t;
    fn mach_port_deallocate(
        task: libc::mach_port_t,
        name: libc::mach_port_t,
    ) -> libc::kern_return_t;
    #[link_name = "mach_task_self_"]
    static CURRENT_MACH_TASK: libc::mach_port_t;
}

#[cfg(target_os = "macos")]
fn current_mach_task() -> libc::mach_port_t {
    unsafe { CURRENT_MACH_TASK }
}

#[cfg(target_os = "macos")]
struct MachTaskPort(libc::mach_port_t);

#[cfg(target_os = "macos")]
impl Drop for MachTaskPort {
    fn drop(&mut self) {
        unsafe {
            let _ = mach_port_deallocate(current_mach_task(), self.0);
        }
    }
}

#[cfg(target_os = "macos")]
struct PinnedProcess {
    identity: ProcessIdentity,
    task: MachTaskPort,
}

#[cfg(target_os = "macos")]
fn pin_process(identity: ProcessIdentity) -> Result<Option<PinnedProcess>> {
    let mut task = 0;
    let status = unsafe { libc::task_for_pid(current_mach_task(), identity.pid, &mut task) };
    if status != libc::KERN_SUCCESS {
        return match process_identity(identity.pid)? {
            None => Ok(None),
            Some(_) => Err(process_scan_resource_error()),
        };
    }
    let task = MachTaskPort(task);
    if process_identity(identity.pid)? != Some(identity) {
        return Ok(None);
    }
    Ok(Some(PinnedProcess { identity, task }))
}

#[cfg(target_os = "macos")]
fn signal_pinned_process(process: &PinnedProcess, signal: libc::c_int) -> Result<bool> {
    let status = match signal {
        libc::SIGSTOP => unsafe { task_suspend(process.task.0) },
        libc::SIGCONT => unsafe { task_resume(process.task.0) },
        libc::SIGKILL => unsafe { libc::task_terminate(process.task.0) },
        _ => return Err(process_scan_resource_error()),
    };
    if status == libc::KERN_SUCCESS {
        return Ok(true);
    }
    if process_identity(process.identity.pid)? != Some(process.identity) {
        Ok(false)
    } else {
        Err(process_scan_resource_error())
    }
}

#[cfg(target_os = "macos")]
fn wait_pinned_process_until(process: &PinnedProcess, deadline: Instant) -> Result<()> {
    loop {
        if process_identity(process.identity.pid)? != Some(process.identity) {
            return Ok(());
        }
        ensure_deadline(deadline)?;
        std::thread::sleep(
            deadline
                .saturating_duration_since(Instant::now())
                .min(Duration::from_millis(2)),
        );
    }
}

#[cfg(any(test, target_os = "macos"))]
fn macos_descendant_processes_from_identity_with<R, L, F>(
    root: ProcessIdentity,
    deadline: Instant,
    mut read_record: R,
    mut list_children: L,
    mut observe: F,
) -> Result<ProcessScan>
where
    R: FnMut(libc::pid_t) -> Result<Option<ProcessRecord>>,
    L: FnMut(libc::pid_t, &mut [libc::pid_t]) -> Result<usize>,
    F: FnMut(ProcessIdentity),
{
    let mut identities = Vec::new();
    let mut seen = HashSet::new();
    let mut edge_visits = 0_usize;
    let mut frontier = vec![root];
    let mut children = vec![0 as libc::pid_t; PROCESS_SCAN_LIMITS.descendants + 1];
    while let Some(parent) = frontier.pop() {
        ensure_deadline(deadline)?;
        if read_record(parent.pid)?.map(|record| record.identity) != Some(parent) {
            continue;
        }
        let count = list_children(parent.pid, &mut children)?;
        if read_record(parent.pid)?.map(|record| record.identity) != Some(parent) {
            continue;
        }
        if count == 0 {
            continue;
        }
        if count > PROCESS_SCAN_LIMITS.descendants {
            return Err(process_scan_resource_error());
        }
        visit_identity_bound_children_with(
            parent,
            children.iter().take(count).copied().map(Ok),
            |pid| {
                ensure_deadline(deadline)?;
                if pid != parent.pid {
                    edge_visits = edge_visits
                        .checked_add(1)
                        .ok_or_else(process_scan_resource_error)?;
                }
                read_record(pid)
            },
            |identity| {
                if seen.insert(identity) {
                    if identities.len() >= PROCESS_SCAN_LIMITS.descendants {
                        return Err(process_scan_resource_error());
                    }
                    observe(identity);
                    identities.push(identity);
                    frontier.push(identity);
                }
                Ok(())
            },
        )?;
    }
    Ok(ProcessScan {
        identities,
        #[cfg(all(test, target_os = "linux"))]
        edge_visits,
    })
}

#[cfg(target_os = "macos")]
fn descendant_processes_from_identity_with<F>(
    root: ProcessIdentity,
    deadline: Instant,
    observe: F,
) -> Result<ProcessScan>
where
    F: FnMut(ProcessIdentity),
{
    macos_descendant_processes_from_identity_with(
        root,
        deadline,
        macos_process_record,
        |parent, children| {
            unsafe {
                *libc::__error() = 0;
            }
            let count = unsafe {
                libc::proc_listchildpids(
                    parent,
                    children.as_mut_ptr().cast(),
                    std::mem::size_of_val(children) as libc::c_int,
                )
            };
            if count < 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::ESRCH) {
                    return Ok(0);
                }
                return Err(process_scan_resource_error());
            }
            Ok(count as usize)
        },
        observe,
    )
}

#[cfg(target_os = "macos")]
fn descendant_processes_from_identity(
    root: ProcessIdentity,
    deadline: Instant,
) -> Result<ProcessScan> {
    descendant_processes_from_identity_with(root, deadline, |_| {})
}

#[cfg(target_os = "macos")]
fn descendant_processes_with<F>(
    root: libc::pid_t,
    deadline: Instant,
    observe: F,
) -> Result<ProcessScan>
where
    F: FnMut(ProcessIdentity),
{
    let Some(root_identity) = process_identity(root)? else {
        return Ok(ProcessScan {
            identities: Vec::new(),
            #[cfg(all(test, target_os = "linux"))]
            edge_visits: 0,
        });
    };
    descendant_processes_from_identity_with(root_identity, deadline, observe)
}

#[cfg(any(test, target_os = "macos"))]
fn macos_fallback_descendants_with<R, S>(
    root: ProcessIdentity,
    deadline: Instant,
    mut current_identity: R,
    mut scan_descendants: S,
) -> Result<ProcessScan>
where
    R: FnMut(libc::pid_t) -> Result<Option<ProcessIdentity>>,
    S: FnMut(ProcessIdentity, Instant) -> Result<ProcessScan>,
{
    ensure_deadline(deadline)?;
    if current_identity(root.pid)? != Some(root) {
        return Ok(ProcessScan {
            identities: Vec::new(),
            #[cfg(all(test, target_os = "linux"))]
            edge_visits: 0,
        });
    }
    scan_descendants(root, deadline)
}

#[cfg(target_os = "linux")]
fn probe_pidfd_support() -> Result<()> {
    use std::os::fd::FromRawFd;

    let descriptor = unsafe { libc::syscall(libc::SYS_pidfd_open, libc::getpid(), 0_u32) };
    if descriptor < 0 {
        return Err(process_scan_resource_error());
    }
    let pidfd = unsafe { File::from_raw_fd(descriptor as libc::c_int) };
    let status = unsafe {
        libc::syscall(
            libc::SYS_pidfd_send_signal,
            pidfd.as_raw_fd(),
            0,
            std::ptr::null::<libc::siginfo_t>(),
            0_u32,
        )
    };
    if status != 0 {
        return Err(process_scan_resource_error());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
struct ProcessOwner {
    root: ProcessIdentity,
    reap_adopted: bool,
    known: HashSet<ProcessIdentity>,
}

#[cfg(target_os = "linux")]
impl ProcessOwner {
    fn new() -> Result<Self> {
        let root = unsafe { libc::getpid() };
        Self::with_root(root, true)
    }

    fn for_root(root: libc::pid_t) -> Result<Self> {
        Self::with_root(root, false)
    }

    fn with_root(root: libc::pid_t, reap_adopted: bool) -> Result<Self> {
        probe_pidfd_support()?;
        if reap_adopted && unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) } != 0 {
            return Err(process_scan_resource_error());
        }
        let root = process_identity(root)?.ok_or_else(process_scan_resource_error)?;
        Ok(Self {
            root,
            reap_adopted,
            known: HashSet::new(),
        })
    }

    fn refresh(&mut self, deadline: Instant) -> Result<()> {
        ensure_deadline(deadline)
    }

    fn identities(&mut self, deadline: Instant) -> Result<Vec<ProcessIdentity>> {
        if process_identity(self.root.pid)? != Some(self.root) {
            return Ok(Vec::new());
        }
        let known = &mut self.known;
        let mut generation_limit_exceeded = false;
        let scan = descendant_processes_at(
            Path::new("/proc"),
            self.root.pid,
            deadline,
            PROCESS_SCAN_LIMITS,
            |identity| {
                if known.contains(&identity) {
                    return;
                }
                if known.len() >= MAX_PROCESS_CLEANUP_GENERATIONS {
                    generation_limit_exceeded = true;
                    return;
                }
                known.insert(identity);
            },
        )?;
        if generation_limit_exceeded {
            return Err(process_scan_resource_error());
        }
        Ok(scan.identities)
    }

    fn known_identities(&self) -> Vec<ProcessIdentity> {
        self.known.iter().copied().collect()
    }

    fn reap_adopted_children(&self) -> bool {
        self.reap_adopted
    }
}

#[cfg(any(test, target_os = "macos"))]
#[derive(Clone, Copy, Debug)]
enum MacosProcessEvent {
    Child { pid: libc::pid_t },
    Exit { pid: libc::pid_t },
    Drained,
}

#[cfg(any(test, target_os = "macos"))]
fn reduce_macos_process_event_with<R>(
    _root: ProcessIdentity,
    tracked: &mut HashMap<libc::pid_t, ProcessIdentity>,
    pending: &mut HashMap<libc::pid_t, ProcessIdentity>,
    event: MacosProcessEvent,
    mut read_record: R,
) -> Result<()>
where
    R: FnMut(libc::pid_t) -> Result<Option<ProcessRecord>>,
{
    match event {
        MacosProcessEvent::Child { pid } => {
            let Some(record) = read_record(pid)? else {
                return Ok(());
            };
            let distinct_pending = pending
                .keys()
                .filter(|pending_pid| !tracked.contains_key(pending_pid))
                .count();
            if !tracked.contains_key(&pid)
                && !pending.contains_key(&pid)
                && tracked.len().saturating_add(distinct_pending) >= MAX_PROCESS_CLEANUP_GENERATIONS
            {
                return Err(process_scan_resource_error());
            }
            pending.insert(pid, record.identity);
        }
        MacosProcessEvent::Exit { pid } => {
            pending.remove(&pid);
            if read_record(pid)?.is_none() {
                tracked.remove(&pid);
            }
        }
        MacosProcessEvent::Drained => {
            let candidates: Vec<_> = pending
                .iter()
                .map(|(pid, identity)| (*pid, *identity))
                .collect();
            for (pid, identity) in candidates {
                let current = read_record(pid)?;
                pending.remove(&pid);
                if current.map(|record| record.identity) == Some(identity) {
                    tracked.insert(pid, identity);
                }
            }
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
struct ProcessOwner {
    root: ProcessIdentity,
    queue: File,
    tracked: HashMap<libc::pid_t, ProcessIdentity>,
    pending: HashMap<libc::pid_t, ProcessIdentity>,
}

#[cfg(target_os = "macos")]
impl ProcessOwner {
    fn new() -> Result<Self> {
        Self::for_root(unsafe { libc::getpid() })
    }

    fn for_root(root_pid: libc::pid_t) -> Result<Self> {
        use std::os::fd::FromRawFd;

        let descriptor = unsafe { libc::kqueue() };
        if descriptor < 0 {
            return Err(process_scan_resource_error());
        }
        let queue = unsafe { File::from_raw_fd(descriptor) };
        let root = process_identity(root_pid)?.ok_or_else(process_scan_resource_error)?;
        let change = libc::kevent {
            ident: root_pid as libc::uintptr_t,
            filter: libc::EVFILT_PROC,
            flags: libc::EV_ADD | libc::EV_ENABLE | libc::EV_CLEAR,
            fflags: libc::NOTE_TRACK | libc::NOTE_FORK | libc::NOTE_EXIT,
            data: 0,
            udata: std::ptr::null_mut(),
        };
        let changed = unsafe {
            libc::kevent(
                queue.as_raw_fd(),
                &change,
                1,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
            )
        };
        if changed < 0 {
            return Err(process_scan_resource_error());
        }
        Ok(Self {
            root,
            queue,
            tracked: HashMap::new(),
            pending: HashMap::new(),
        })
    }

    fn refresh(&mut self, deadline: Instant) -> Result<()> {
        let timeout = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        loop {
            ensure_deadline(deadline)?;
            let mut events: [libc::kevent; 64] = unsafe { std::mem::zeroed() };
            let count = unsafe {
                libc::kevent(
                    self.queue.as_raw_fd(),
                    std::ptr::null(),
                    0,
                    events.as_mut_ptr(),
                    events.len() as libc::c_int,
                    &timeout,
                )
            };
            if count < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(process_scan_resource_error());
            }
            if count == 0 {
                reduce_macos_process_event_with(
                    self.root,
                    &mut self.tracked,
                    &mut self.pending,
                    MacosProcessEvent::Drained,
                    |pid| {
                        ensure_deadline(deadline)?;
                        macos_process_record(pid)
                    },
                )?;
                return Ok(());
            }
            for event in &events[..count as usize] {
                ensure_deadline(deadline)?;
                let flags = unsafe { std::ptr::addr_of!(event.fflags).read_unaligned() };
                let pid =
                    unsafe { std::ptr::addr_of!(event.ident).read_unaligned() as libc::pid_t };
                if flags & libc::NOTE_TRACKERR != 0 {
                    return Err(process_scan_resource_error());
                }
                if flags & libc::NOTE_CHILD != 0 {
                    reduce_macos_process_event_with(
                        self.root,
                        &mut self.tracked,
                        &mut self.pending,
                        MacosProcessEvent::Child { pid },
                        macos_process_record,
                    )?;
                }
                if flags & libc::NOTE_EXIT != 0 {
                    reduce_macos_process_event_with(
                        self.root,
                        &mut self.tracked,
                        &mut self.pending,
                        MacosProcessEvent::Exit { pid },
                        macos_process_record,
                    )?;
                }
            }
        }
    }

    fn identities(&mut self, deadline: Instant) -> Result<Vec<ProcessIdentity>> {
        self.refresh(deadline)?;
        let mut identities = HashSet::new();
        let mut stale = Vec::new();
        for identity in self.tracked.values().copied() {
            ensure_deadline(deadline)?;
            if process_identity(identity.pid)? == Some(identity) {
                identities.insert(identity);
            } else {
                stale.push(identity);
            }
        }
        for identity in stale {
            if self.tracked.get(&identity.pid) == Some(&identity) {
                self.tracked.remove(&identity.pid);
            }
        }
        let fallback = macos_fallback_descendants_with(
            self.root,
            deadline,
            process_identity,
            descendant_processes_from_identity,
        )?;
        identities.extend(fallback.identities);
        if identities.len() > PROCESS_SCAN_LIMITS.descendants {
            return Err(process_scan_resource_error());
        }
        for identity in identities.iter().copied() {
            self.tracked.insert(identity.pid, identity);
        }
        Ok(identities.into_iter().collect())
    }

    fn known_identities(&self) -> Vec<ProcessIdentity> {
        self.tracked
            .values()
            .chain(self.pending.values())
            .copied()
            .collect::<HashSet<_>>()
            .into_iter()
            .collect()
    }

    fn reap_adopted_children(&self) -> bool {
        false
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
struct ProcessOwner {
    root: libc::pid_t,
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
impl ProcessOwner {
    fn new() -> Result<Self> {
        Self::for_root(unsafe { libc::getpid() })
    }

    fn for_root(root: libc::pid_t) -> Result<Self> {
        Ok(Self { root })
    }

    fn refresh(&mut self, deadline: Instant) -> Result<()> {
        ensure_deadline(deadline)
    }

    fn identities(&mut self, deadline: Instant) -> Result<Vec<ProcessIdentity>> {
        Ok(descendant_processes(self.root, deadline)?.identities)
    }

    fn known_identities(&self) -> Vec<ProcessIdentity> {
        Vec::new()
    }

    fn reap_adopted_children(&self) -> bool {
        false
    }
}

trait OwnedProcessTracker {
    fn refresh(&mut self, deadline: Instant) -> Result<()> {
        ensure_deadline(deadline)
    }
    fn identities(&mut self, deadline: Instant) -> Result<Vec<ProcessIdentity>>;
    fn known_identities(&self) -> Vec<ProcessIdentity>;
    fn reap_adopted_children(&self) -> bool {
        false
    }
}

impl OwnedProcessTracker for ProcessOwner {
    fn refresh(&mut self, deadline: Instant) -> Result<()> {
        ProcessOwner::refresh(self, deadline)
    }

    fn identities(&mut self, deadline: Instant) -> Result<Vec<ProcessIdentity>> {
        ProcessOwner::identities(self, deadline)
    }

    fn known_identities(&self) -> Vec<ProcessIdentity> {
        ProcessOwner::known_identities(self)
    }

    fn reap_adopted_children(&self) -> bool {
        ProcessOwner::reap_adopted_children(self)
    }
}

trait OwnedProcessControl {
    type Pinned;

    fn pin(&mut self, identity: ProcessIdentity) -> Result<Option<Self::Pinned>>;
    fn signal(&mut self, process: &Self::Pinned, signal: libc::c_int) -> Result<bool>;
    fn wait_for_exit(&mut self, _process: &Self::Pinned, _deadline: Instant) -> Result<()> {
        Ok(())
    }
}

struct SystemProcessControl;

impl OwnedProcessControl for SystemProcessControl {
    type Pinned = PinnedProcess;

    fn pin(&mut self, identity: ProcessIdentity) -> Result<Option<Self::Pinned>> {
        pin_process(identity)
    }

    fn signal(&mut self, process: &Self::Pinned, signal: libc::c_int) -> Result<bool> {
        signal_pinned_process(process, signal)
    }

    fn wait_for_exit(&mut self, process: &Self::Pinned, deadline: Instant) -> Result<()> {
        wait_pinned_process_until(process, deadline)
    }
}

fn terminate_owned_identity_until_with<C>(
    control: &mut C,
    identity: ProcessIdentity,
    deadline: Instant,
) -> Result<()>
where
    C: OwnedProcessControl,
{
    ensure_deadline(deadline)?;
    let Some(process) = control.pin(identity)? else {
        return Ok(());
    };
    ensure_deadline(deadline)?;
    if !control.signal(&process, libc::SIGSTOP)? {
        return Ok(());
    }
    if let Err(error) = ensure_deadline(deadline) {
        return match control.signal(&process, libc::SIGCONT) {
            Ok(_) => Err(error),
            Err(resume_error) => Err(resume_error),
        };
    }
    if let Err(error) = control.signal(&process, libc::SIGKILL) {
        return match control.signal(&process, libc::SIGCONT) {
            Ok(_) => Err(error),
            Err(resume_error) => Err(resume_error),
        };
    }
    control.wait_for_exit(&process, deadline)
}

fn terminate_owned_processes<T: OwnedProcessTracker>(
    owner: &mut T,
    child: &mut Child,
) -> Result<()> {
    let deadline = Instant::now()
        .checked_add(PROCESS_CLEANUP_TIMEOUT)
        .unwrap_or_else(Instant::now);
    terminate_owned_processes_until(owner, child, deadline)
}

fn terminate_owned_processes_until<T: OwnedProcessTracker>(
    owner: &mut T,
    child: &mut Child,
    deadline: Instant,
) -> Result<()> {
    terminate_owned_processes_until_with(owner, child, deadline, &mut SystemProcessControl)
}

fn terminate_owned_processes_until_with<T, C>(
    owner: &mut T,
    child: &mut Child,
    deadline: Instant,
    control: &mut C,
) -> Result<()>
where
    T: OwnedProcessTracker,
    C: OwnedProcessControl,
{
    let mut attempted = HashSet::new();
    let mut containment_error = None;
    if let Err(error) = owner.refresh(deadline) {
        containment_error.get_or_insert(error);
    }
    if let Err(error) = child.kill()
        && error.kind() != io::ErrorKind::InvalidInput
    {
        containment_error.get_or_insert_with(process_scan_resource_error);
    }
    'discovery: loop {
        if let Err(error) = ensure_deadline(deadline) {
            containment_error.get_or_insert(error);
            break;
        }
        let mut identities = match owner.identities(deadline) {
            Ok(identities) => identities,
            Err(error) => {
                containment_error.get_or_insert(error);
                Vec::new()
            }
        };
        identities.extend(owner.known_identities());
        let mut added = false;
        for identity in identities {
            if let Err(error) = ensure_deadline(deadline) {
                containment_error.get_or_insert(error);
                break 'discovery;
            }
            if attempted.contains(&identity) {
                continue;
            }
            if attempted.len() >= MAX_PROCESS_CLEANUP_GENERATIONS {
                containment_error.get_or_insert_with(process_scan_resource_error);
                break 'discovery;
            }
            attempted.insert(identity);
            added = true;
            if let Err(error) = terminate_owned_identity_until_with(control, identity, deadline) {
                containment_error.get_or_insert(error);
            }
            if let Err(error) = ensure_deadline(deadline) {
                containment_error.get_or_insert(error);
                break 'discovery;
            }
        }
        if containment_error.is_some() || !added {
            break;
        }
    }
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(2));
            }
            Ok(None) => {
                containment_error.get_or_insert_with(|| {
                    domain_error(
                        DirtyCheckoutErrorKind::Timeout,
                        "process cleanup exceeded its deadline",
                    )
                });
                break;
            }
            Err(_) => {
                containment_error.get_or_insert_with(process_scan_resource_error);
                break;
            }
        }
    }
    if owner.reap_adopted_children() {
        loop {
            if Instant::now() >= deadline {
                containment_error.get_or_insert_with(|| {
                    domain_error(
                        DirtyCheckoutErrorKind::Timeout,
                        "process cleanup exceeded its deadline",
                    )
                });
                break;
            }
            let reaped = unsafe { libc::waitpid(-1, std::ptr::null_mut(), libc::WNOHANG) };
            if reaped > 0 {
                continue;
            }
            if reaped < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                if error.raw_os_error() != Some(libc::ECHILD) {
                    containment_error.get_or_insert_with(process_scan_resource_error);
                }
                break;
            }
            if Instant::now() >= deadline {
                containment_error.get_or_insert_with(|| {
                    domain_error(
                        DirtyCheckoutErrorKind::Timeout,
                        "process cleanup exceeded its deadline",
                    )
                });
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    if let Some(error) = containment_error {
        Err(error)
    } else {
        Ok(())
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
struct PinnedProcess;

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn pin_process(_identity: ProcessIdentity) -> Result<Option<PinnedProcess>> {
    Ok(None)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn signal_pinned_process(_process: &PinnedProcess, _signal: libc::c_int) -> Result<bool> {
    Ok(false)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn wait_pinned_process_until(_process: &PinnedProcess, _deadline: Instant) -> Result<()> {
    Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn descendant_processes_with<F>(
    _root: libc::pid_t,
    _deadline: Instant,
    _observe: F,
) -> Result<ProcessScan>
where
    F: FnMut(ProcessIdentity),
{
    Ok(ProcessScan {
        identities: Vec::new(),
        #[cfg(all(test, target_os = "linux"))]
        edge_visits: 0,
    })
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn descendant_processes(root: libc::pid_t, deadline: Instant) -> Result<ProcessScan> {
    descendant_processes_with(root, deadline, |_| {})
}

fn read_supervisor_cleanup_proof_with<R: Read>(reader: &mut R) -> bool {
    read_process_supervisor_completion(reader).is_ok_and(|completion| {
        completion.cleanup_complete
            && completion.kind != ProcessSupervisorCompletionKind::InternalFailure
    })
}

fn signal_cleanup_leader_with<F>(pid: libc::pid_t, supervised: bool, mut send: F) -> io::Result<()>
where
    F: FnMut(libc::pid_t, libc::c_int) -> io::Result<()>,
{
    if supervised {
        send(pid, libc::SIGTERM)
    } else {
        send(-pid, libc::SIGSTOP)
    }
}

fn signal_cleanup_leader(pid: libc::pid_t, supervised: bool) -> io::Result<()> {
    signal_cleanup_leader_with(pid, supervised, |target, signal| {
        if unsafe { libc::kill(target, signal) } == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(())
        } else {
            Err(error)
        }
    })
}

fn should_use_unpinned_cleanup_signals(supervised: bool, leader_reaped: bool) -> bool {
    !supervised || !leader_reaped
}

#[cfg(all(test, target_os = "linux"))]
thread_local! {
    static UNPINNED_CLEANUP_SIGNAL_ATTEMPTS: std::cell::Cell<usize> = const {
        std::cell::Cell::new(0)
    };
}

#[cfg(all(test, target_os = "linux"))]
fn record_unpinned_cleanup_signal_attempt() {
    UNPINNED_CLEANUP_SIGNAL_ATTEMPTS.with(|attempts| attempts.set(attempts.get() + 1));
}

#[cfg(all(test, target_os = "linux"))]
fn reset_unpinned_cleanup_signal_attempts() {
    UNPINNED_CLEANUP_SIGNAL_ATTEMPTS.with(|attempts| attempts.set(0));
}

#[cfg(all(test, target_os = "linux"))]
fn unpinned_cleanup_signal_attempts() -> usize {
    UNPINNED_CLEANUP_SIGNAL_ATTEMPTS.with(std::cell::Cell::get)
}

fn terminate_child(
    child: &mut Child,
    process_group: libc::pid_t,
    supervised: bool,
    cleanup_proof: &mut Option<std::os::unix::net::UnixStream>,
    fallback_owner: &mut Option<ProcessOwner>,
) {
    let cleanup_started = Instant::now();
    let supervisor_grace = PROCESS_CLEANUP_TIMEOUT
        .checked_add(Duration::from_millis(25))
        .unwrap_or(PROCESS_CLEANUP_TIMEOUT);
    let cleanup_timeout = if supervised {
        supervisor_grace
            .checked_add(PROCESS_CLEANUP_TIMEOUT)
            .unwrap_or(supervisor_grace)
    } else {
        PROCESS_CLEANUP_TIMEOUT
    };
    let cleanup_deadline = cleanup_started
        .checked_add(cleanup_timeout)
        .unwrap_or(cleanup_started);
    let mut leader_reaped = child.try_wait().is_ok_and(|status| status.is_some());
    if should_use_unpinned_cleanup_signals(supervised, leader_reaped) {
        #[cfg(all(test, target_os = "linux"))]
        record_unpinned_cleanup_signal_attempt();
        let _ = signal_cleanup_leader(process_group, supervised);
    }

    if supervised {
        if leader_reaped
            && cleanup_proof
                .as_mut()
                .is_some_and(read_supervisor_cleanup_proof_with)
        {
            return;
        }
        let supervisor_deadline = cleanup_started
            .checked_add(supervisor_grace)
            .unwrap_or(cleanup_started)
            .min(cleanup_deadline);
        loop {
            match child.try_wait() {
                Ok(Some(_)) => {
                    leader_reaped = true;
                    if cleanup_proof
                        .as_mut()
                        .is_some_and(read_supervisor_cleanup_proof_with)
                    {
                        return;
                    }
                    break;
                }
                Ok(None) if Instant::now() < supervisor_deadline => {
                    std::thread::sleep(Duration::from_millis(2));
                }
                Ok(None) | Err(_) => break,
            }
        }
        if should_use_unpinned_cleanup_signals(supervised, leader_reaped) {
            #[cfg(all(test, target_os = "linux"))]
            record_unpinned_cleanup_signal_attempt();
            unsafe {
                let _ = libc::kill(-process_group, libc::SIGSTOP);
            }
        }
    }

    if let Some(owner) = fallback_owner {
        let _ = terminate_owned_processes_until(owner, child, cleanup_deadline);
    }

    if !supervised {
        let mut known = HashSet::new();
        let mut pending = VecDeque::new();
        let mut control = SystemProcessControl;
        while Instant::now() < cleanup_deadline {
            let mut added = false;
            let mut generation_limit_exceeded = false;
            let scan_result =
                descendant_processes_with(process_group, cleanup_deadline, |identity| {
                    if known.contains(&identity) {
                        return;
                    }
                    if known.len() >= MAX_PROCESS_CLEANUP_GENERATIONS {
                        generation_limit_exceeded = true;
                        return;
                    }
                    known.insert(identity);
                    pending.push_back(identity);
                    added = true;
                });
            while let Some(identity) = pending.pop_front() {
                if Instant::now() >= cleanup_deadline {
                    break;
                }
                let _ =
                    terminate_owned_identity_until_with(&mut control, identity, cleanup_deadline);
            }
            if scan_result.is_err() || generation_limit_exceeded || !added {
                break;
            }
        }
        #[cfg(all(test, target_os = "linux"))]
        record_unpinned_cleanup_signal_attempt();
        unsafe {
            if libc::kill(-process_group, libc::SIGKILL) != 0 {
                let _ = libc::kill(-process_group, libc::SIGCONT);
            }
        }
    }
    let _ = child.kill();
    loop {
        match child.try_wait() {
            Ok(Some(_)) | Err(_) => break,
            Ok(None) if Instant::now() < cleanup_deadline => {
                std::thread::sleep(Duration::from_millis(2));
            }
            Ok(None) => break,
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
    parse_revoke_args_inner(raw).map_err(redact_revoke_parse_error)
}

fn parse_revoke_args_inner(raw: &[String]) -> std::result::Result<Option<RevokeArgs>, CliError> {
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

fn redact_revoke_parse_error(error: CliError) -> CliError {
    let message = match error.code {
        "missing-format" | "invalid-format" => "--format requires text or json",
        _ => "invalid revoke-dirty arguments",
    };
    CliError::usage(error.code, message)
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
    env::var_os("AGENT_RUNTIME_DIRTY_CHECKOUT_ADOPTION").as_deref() == Some(OsStr::new("1"))
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
    use std::os::unix::process::ExitStatusExt;

    fn successful_worker_output(snapshot: serde_json::Value) -> std::process::Output {
        std::process::Output {
            status: std::process::ExitStatus::from_raw(0),
            stdout: serde_json::to_vec(&serde_json::json!({
                "schema": SNAPSHOT_WORKER_SCHEMA,
                "snapshot": snapshot,
                "error": null,
            }))
            .expect("serialize worker response"),
            stderr: Vec::new(),
        }
    }

    fn true_executable() -> &'static str {
        #[cfg(target_os = "macos")]
        {
            "/usr/bin/true"
        }
        #[cfg(not(target_os = "macos"))]
        {
            "/bin/true"
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    struct SupervisorFixture {
        child: Child,
        completion_channel: Option<std::os::unix::net::UnixStream>,
        reaped: bool,
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    impl SupervisorFixture {
        #[cfg(target_os = "linux")]
        fn id(&self) -> u32 {
            self.child.id()
        }

        fn abandon_owner(&mut self) {
            drop(self.completion_channel.take());
        }

        fn try_wait(&mut self) -> io::Result<Option<std::process::ExitStatus>> {
            let status = self.child.try_wait()?;
            if status.is_some() {
                self.reaped = true;
            }
            Ok(status)
        }

        fn wait(&mut self) -> io::Result<std::process::ExitStatus> {
            let status = self.child.wait()?;
            self.reaped = true;
            Ok(status)
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    impl Drop for SupervisorFixture {
        fn drop(&mut self) {
            if self.reaped {
                return;
            }
            unsafe {
                libc::kill(-(self.child.id() as libc::pid_t), libc::SIGKILL);
            }
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn pin_published_process_with<C: OwnedProcessControl>(
        control: &mut C,
        identity: ProcessIdentity,
    ) -> Result<Option<C::Pinned>> {
        control.pin(identity)
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn signal_published_process_with<C: OwnedProcessControl>(
        control: &mut C,
        process: &C::Pinned,
    ) -> Result<()> {
        control.signal(process, libc::SIGKILL).map(|_| ())
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    struct PublishedPidCleanup(Option<PinnedProcess>);

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    impl PublishedPidCleanup {
        fn new(identity: ProcessIdentity) -> Self {
            let mut control = SystemProcessControl;
            let process = pin_published_process_with(&mut control, identity)
                .ok()
                .flatten();
            Self(process)
        }

        #[cfg(target_os = "linux")]
        fn process(&self) -> Option<&PinnedProcess> {
            self.0.as_ref()
        }

        fn terminate_and_wait(&mut self) -> Result<()> {
            let Some(process) = &self.0 else {
                return Ok(());
            };
            let mut control = SystemProcessControl;
            signal_published_process_with(&mut control, process)?;
            wait_pinned_process_until(process, Instant::now() + Duration::from_secs(1))?;
            self.0 = None;
            Ok(())
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    impl Drop for PublishedPidCleanup {
        fn drop(&mut self) {
            let _ = self.terminate_and_wait();
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn fixture_monotonic_deadline(timeout: Duration) -> u64 {
        let mut now = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        assert_eq!(
            unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now) },
            0,
            "read fixture monotonic clock"
        );
        let now = u64::try_from(now.tv_sec)
            .expect("nonnegative monotonic seconds")
            .checked_mul(1_000_000_000)
            .and_then(|value| value.checked_add(u64::try_from(now.tv_nsec).ok()?))
            .expect("bounded monotonic clock");
        now.checked_add(u64::try_from(timeout.as_nanos()).expect("bounded fixture timeout"))
            .expect("bounded fixture deadline")
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn spawn_authenticated_supervisor_fixture(
        scenario: &str,
        pid_path: &Path,
    ) -> SupervisorFixture {
        spawn_authenticated_supervisor_fixture_with_timeout(
            scenario,
            pid_path,
            Duration::from_secs(5),
        )
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn spawn_authenticated_supervisor_fixture_with_timeout(
        scenario: &str,
        pid_path: &Path,
        timeout: Duration,
    ) -> SupervisorFixture {
        spawn_authenticated_supervisor_fixture_with_timeout_and_delay(
            scenario,
            pid_path,
            timeout,
            Duration::ZERO,
        )
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn spawn_authenticated_supervisor_fixture_with_timeout_and_delay(
        scenario: &str,
        pid_path: &Path,
        timeout: Duration,
        capability_delay: Duration,
    ) -> SupervisorFixture {
        use std::os::unix::net::UnixStream;

        let deadline_nanos = fixture_monotonic_deadline(timeout);
        let (mut sender, receiver) = UnixStream::pair().expect("fixture capability socket");
        let descriptor = receiver.as_raw_fd();
        let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFD) };
        assert!(flags >= 0, "read fixture descriptor flags");
        assert!(
            unsafe { libc::fcntl(descriptor, libc::F_SETFD, flags & !libc::FD_CLOEXEC) } >= 0,
            "make fixture descriptor inheritable"
        );
        let child = Command::new(env::current_exe().expect("current test executable"))
            .arg("authenticated_process_supervisor_test_fixture")
            .arg("--nocapture")
            .env(PROCESS_SUPERVISOR_ENV, "1")
            .env(PROCESS_SUPERVISOR_CAPABILITY_FD_ENV, descriptor.to_string())
            .env("NILS_PROCESS_SUPERVISOR_TEST_SCENARIO", scenario)
            .env("NILS_PROCESS_SUPERVISOR_TEST_PID_PATH", pid_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .expect("spawn authenticated supervisor fixture");
        let mut child = child;
        drop(receiver);
        std::thread::sleep(capability_delay);
        if let Err(error) = sender.write_all(PROCESS_SUPERVISOR_CAPABILITY) {
            let _ = child.kill();
            let _ = child.wait();
            panic!("deliver fixture capability: {error}");
        }
        if let Err(error) = sender.write_all(&deadline_nanos.to_be_bytes()) {
            unsafe {
                let _ = libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL);
            }
            let _ = child.kill();
            let _ = child.wait();
            panic!("deliver fixture deadline: {error}");
        }
        SupervisorFixture {
            child,
            completion_channel: Some(sender),
            reaped: false,
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn spawn_authenticated_supervisor_output_fixture(
        scenario: &str,
        pid_path: &Path,
    ) -> SpawnedOutputChild {
        use std::os::unix::net::UnixStream;

        let (mut sender, receiver) = UnixStream::pair().expect("fixture capability socket");
        let descriptor = receiver.as_raw_fd();
        let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFD) };
        assert!(flags >= 0, "read fixture descriptor flags");
        assert!(
            unsafe { libc::fcntl(descriptor, libc::F_SETFD, flags & !libc::FD_CLOEXEC) } >= 0,
            "make fixture descriptor inheritable"
        );
        let mut child = Command::new(env::current_exe().expect("current test executable"))
            .arg("authenticated_process_supervisor_test_fixture")
            .arg("--nocapture")
            .env(PROCESS_SUPERVISOR_ENV, "1")
            .env(PROCESS_SUPERVISOR_CAPABILITY_FD_ENV, descriptor.to_string())
            .env("NILS_PROCESS_SUPERVISOR_TEST_SCENARIO", scenario)
            .env("NILS_PROCESS_SUPERVISOR_TEST_PID_PATH", pid_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .expect("spawn authenticated supervisor output fixture");
        let fallback_owner = if matches!(scenario, "internal-failure" | "target-status-one") {
            None
        } else {
            Some(
                ProcessOwner::for_root(child.id() as libc::pid_t)
                    .expect("create scoped output fallback owner"),
            )
        };
        drop(receiver);
        if let Err(error) = sender.write_all(PROCESS_SUPERVISOR_CAPABILITY) {
            unsafe {
                let _ = libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL);
            }
            let _ = child.kill();
            let _ = child.wait();
            panic!("deliver output fixture capability: {error}");
        }
        if let Err(error) =
            sender.write_all(&fixture_monotonic_deadline(Duration::from_secs(5)).to_be_bytes())
        {
            unsafe {
                let _ = libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL);
            }
            let _ = child.kill();
            let _ = child.wait();
            panic!("deliver output fixture deadline: {error}");
        }
        SpawnedOutputChild {
            child,
            supervised: true,
            cleanup_proof: Some(sender),
            fallback_owner,
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn run_process_supervisor_capability_fixture(
        scenario: &str,
        capability: Option<&[u8]>,
    ) -> std::process::ExitStatus {
        use std::os::unix::net::UnixStream;

        let sockets = capability.map(|_| UnixStream::pair().expect("capability test socket"));
        let mut command = Command::new(env::current_exe().expect("current test executable"));
        command
            .arg("process_supervisor_capability_validation_test_fixture")
            .arg("--nocapture")
            .env("NILS_PROCESS_SUPERVISOR_CAPABILITY_TEST", scenario)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0);
        if scenario == "malformed-fd" {
            command.env(PROCESS_SUPERVISOR_CAPABILITY_FD_ENV, "not-a-descriptor");
        }
        if let Some((_, receiver)) = &sockets {
            let descriptor = receiver.as_raw_fd();
            let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFD) };
            assert!(flags >= 0, "read capability fixture descriptor flags");
            assert!(
                unsafe { libc::fcntl(descriptor, libc::F_SETFD, flags & !libc::FD_CLOEXEC) } >= 0,
                "make capability fixture descriptor inheritable"
            );
            command.env(PROCESS_SUPERVISOR_CAPABILITY_FD_ENV, descriptor.to_string());
        }
        let mut child = command
            .spawn()
            .expect("spawn capability validation fixture");
        if let (Some(bytes), Some((sender, receiver))) = (capability, sockets) {
            drop(receiver);
            (&sender)
                .write_all(bytes)
                .expect("deliver capability validation bytes");
            (&sender)
                .write_all(&fixture_monotonic_deadline(Duration::from_secs(1)).to_be_bytes())
                .expect("deliver capability validation deadline");
        }
        child
            .wait()
            .expect("wait for capability validation fixture")
    }

    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn process_supervisor_capability_validation_test_fixture() {
        let Some(scenario) = env::var_os("NILS_PROCESS_SUPERVISOR_CAPABILITY_TEST") else {
            return;
        };
        let result = if scenario == OsStr::new("valid-untrusted-parent") {
            validate_process_supervisor_capability_with(|| false)
        } else {
            validate_process_supervisor_capability()
        };
        if scenario == OsStr::new("valid") {
            result.expect("trusted parent with the exact capability must authenticate");
        } else {
            result.expect_err("invalid supervisor capability boundary must fail closed");
        }
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn process_supervisor_wrong_peer_sender_test_fixture() {
        use std::os::unix::net::UnixStream;

        let Some(socket_path) = env::var_os("NILS_PROCESS_SUPERVISOR_WRONG_PEER_SOCKET") else {
            return;
        };
        let mut sender = UnixStream::connect(socket_path).expect("connect wrong-peer sender");
        sender
            .write_all(PROCESS_SUPERVISOR_CAPABILITY)
            .expect("deliver wrong-peer capability");
        sender
            .write_all(&fixture_monotonic_deadline(Duration::from_secs(1)).to_be_bytes())
            .expect("deliver wrong-peer deadline");
    }

    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn process_supervisor_capability_boundaries_are_independent_and_fail_closed() {
        let mismatched = vec![b'x'; PROCESS_SUPERVISOR_CAPABILITY.len()];
        for (scenario, capability) in [
            ("missing-fd", None),
            ("malformed-fd", None),
            ("mismatched-capability", Some(mismatched.as_slice())),
            (
                "valid-untrusted-parent",
                Some(PROCESS_SUPERVISOR_CAPABILITY),
            ),
            ("valid", Some(PROCESS_SUPERVISOR_CAPABILITY)),
        ] {
            let status = run_process_supervisor_capability_fixture(scenario, capability);
            assert!(
                status.success(),
                "capability boundary fixture failed: {scenario}"
            );
        }
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn process_supervisor_rejects_capability_from_non_parent_peer() {
        use std::os::unix::net::UnixListener;

        let root = tempfile::TempDir::new().expect("wrong-peer capability root");
        let socket_path = root.path().join("capability.sock");
        let listener = UnixListener::bind(&socket_path).expect("bind wrong-peer capability socket");
        let mut sender = Command::new(env::current_exe().expect("current test executable"))
            .arg("process_supervisor_wrong_peer_sender_test_fixture")
            .arg("--nocapture")
            .env("NILS_PROCESS_SUPERVISOR_WRONG_PEER_SOCKET", &socket_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn wrong-peer capability sender");
        let (receiver, _) = listener.accept().expect("accept wrong-peer capability");
        let descriptor = receiver.as_raw_fd();
        let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFD) };
        assert!(flags >= 0, "read wrong-peer descriptor flags");
        assert!(
            unsafe { libc::fcntl(descriptor, libc::F_SETFD, flags & !libc::FD_CLOEXEC) } >= 0,
            "make wrong-peer descriptor inheritable"
        );
        let mut validator = Command::new(env::current_exe().expect("current test executable"))
            .arg("process_supervisor_capability_validation_test_fixture")
            .arg("--nocapture")
            .env("NILS_PROCESS_SUPERVISOR_CAPABILITY_TEST", "wrong-peer")
            .env(PROCESS_SUPERVISOR_CAPABILITY_FD_ENV, descriptor.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .expect("spawn wrong-peer capability validator");
        drop(receiver);

        assert!(
            validator
                .wait()
                .expect("wait for wrong-peer validator")
                .success(),
            "a non-parent peer must fail authentication"
        );
        assert!(
            sender.wait().expect("wait for wrong-peer sender").success(),
            "wrong-peer sender fixture failed"
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn process_supervisor_peer_credentials_reject_wrong_parent_or_user() {
        let expected_parent = 42;
        let expected_uid = 1_000;
        let credentials = libc::ucred {
            pid: expected_parent,
            uid: expected_uid,
            gid: 1_000,
        };

        assert!(linux_peer_credentials_match(
            credentials,
            expected_parent,
            expected_uid
        ));
        assert!(!linux_peer_credentials_match(
            libc::ucred {
                pid: expected_parent + 1,
                ..credentials
            },
            expected_parent,
            expected_uid,
        ));
        assert!(!linux_peer_credentials_match(
            libc::ucred {
                uid: expected_uid + 1,
                ..credentials
            },
            expected_parent,
            expected_uid,
        ));
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    struct SupervisorCompletionFixtureOwner;

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    impl OwnedProcessTracker for SupervisorCompletionFixtureOwner {
        fn identities(&mut self, deadline: Instant) -> Result<Vec<ProcessIdentity>> {
            ensure_deadline(deadline)?;
            Ok(Vec::new())
        }

        fn known_identities(&self) -> Vec<ProcessIdentity> {
            Vec::new()
        }
    }

    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn authenticated_process_supervisor_test_fixture() {
        let Some(scenario) = env::var_os("NILS_PROCESS_SUPERVISOR_TEST_SCENARIO") else {
            return;
        };
        let authenticated =
            validate_process_supervisor_capability().expect("authenticate fixture parent");
        let program = if scenario == OsStr::new("internal-failure") {
            PathBuf::from("/nils-supervisor-fixture/missing-target")
        } else {
            env::current_exe().expect("current test executable")
        };
        let arguments = vec![
            OsString::from("process_supervisor_target_test_fixture"),
            OsString::from("--nocapture"),
        ];
        let expected = match scenario.to_str() {
            Some("successful-child") => exit::SUCCESS,
            Some(
                "blocked-child"
                | "detached-double-fork"
                | "escaped-output-timeout"
                | "target-status-one",
            ) => exit::RUNTIME,
            Some("internal-failure") => {
                let mut owner = SupervisorCompletionFixtureOwner;
                supervise_process_with_cleanup_proof_and_owner(
                    program.into_os_string(),
                    &arguments,
                    Some(authenticated.channel),
                    authenticated.deadline,
                    &mut owner,
                )
                .expect_err("fixture target launch must fail internally");
                std::process::exit(exit::RUNTIME);
            }
            Some("target-signal") => {
                supervise_process_with_cleanup_proof(
                    program.into_os_string(),
                    &arguments,
                    Some(authenticated.channel),
                    authenticated.deadline,
                )
                .expect_err("fixture target launch must fail internally");
                std::process::exit(exit::RUNTIME);
            }
            _ => panic!("unsupported supervisor fixture scenario"),
        };
        let status = if scenario == OsStr::new("target-status-one") {
            let mut owner = SupervisorCompletionFixtureOwner;
            supervise_process_with_cleanup_proof_and_owner(
                program.into_os_string(),
                &arguments,
                Some(authenticated.channel),
                authenticated.deadline,
                &mut owner,
            )
        } else {
            supervise_process_with_cleanup_proof(
                program.into_os_string(),
                &arguments,
                Some(authenticated.channel),
                authenticated.deadline,
            )
        }
        .expect("run supervisor fixture");
        if scenario == OsStr::new("target-status-one") {
            std::process::exit(status);
        }
        assert_eq!(status, expected);
    }

    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[allow(clippy::zombie_processes)] // The supervisor, not this short-lived fixture parent, owns reaping.
    fn process_supervisor_target_test_fixture() {
        let Some(scenario) = env::var_os("NILS_PROCESS_SUPERVISOR_TEST_SCENARIO") else {
            return;
        };
        let pid_path =
            env::var_os("NILS_PROCESS_SUPERVISOR_TEST_PID_PATH").expect("fixture pid path");
        match scenario.to_str() {
            Some("target-signal") => {
                unsafe {
                    libc::kill(libc::getpid(), libc::SIGKILL);
                }
                unreachable!("SIGKILL target survived");
            }
            Some("target-status-one") => std::process::exit(exit::RUNTIME),
            Some("blocked-child") => {
                fs::write(pid_path, unsafe { libc::getpid() }.to_string())
                    .expect("publish blocked fixture PID");
                loop {
                    std::thread::park_timeout(Duration::from_secs(1));
                }
            }
            Some("successful-child") => {
                let mut child = Command::new(env::current_exe().expect("current test executable"))
                    .arg("process_supervisor_leaf_test_fixture")
                    .arg("--nocapture")
                    .env("NILS_PROCESS_SUPERVISOR_LEAF", "1")
                    .env("NILS_PROCESS_SUPERVISOR_TEST_PID_PATH", &pid_path)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .expect("spawn surviving fixture child");
                let deadline = Instant::now() + Duration::from_secs(1);
                while !Path::new(&pid_path).exists() {
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        panic!("surviving fixture child did not publish its PID");
                    }
                    std::thread::sleep(Duration::from_millis(2));
                }
            }
            Some(scenario @ ("detached-double-fork" | "escaped-output-timeout")) => {
                let mut intermediate =
                    Command::new(env::current_exe().expect("current test executable"));
                intermediate
                    .arg("process_supervisor_intermediate_test_fixture")
                    .arg("--nocapture")
                    .env("NILS_PROCESS_SUPERVISOR_INTERMEDIATE", "1")
                    .env("NILS_PROCESS_SUPERVISOR_TEST_PID_PATH", &pid_path)
                    .stdin(Stdio::null())
                    .process_group(0);
                if scenario == "escaped-output-timeout" {
                    intermediate.env("NILS_PROCESS_SUPERVISOR_LEAF_INHERIT_OUTPUT", "1");
                } else {
                    intermediate.stdout(Stdio::null()).stderr(Stdio::null());
                }
                intermediate
                    .spawn()
                    .expect("spawn detached fixture intermediate");
                while !Path::new(&pid_path).exists() {
                    std::thread::sleep(Duration::from_millis(2));
                }
                loop {
                    std::thread::park_timeout(Duration::from_secs(1));
                }
            }
            _ => panic!("unsupported target fixture scenario"),
        }
    }

    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[allow(clippy::zombie_processes)] // Exiting here deliberately reparents the leaf to the supervisor.
    fn process_supervisor_intermediate_test_fixture() {
        if env::var_os("NILS_PROCESS_SUPERVISOR_INTERMEDIATE").as_deref() != Some(OsStr::new("1")) {
            return;
        }
        let pid_path =
            env::var_os("NILS_PROCESS_SUPERVISOR_TEST_PID_PATH").expect("fixture pid path");
        let mut command = Command::new(env::current_exe().expect("current test executable"));
        command
            .arg("process_supervisor_leaf_test_fixture")
            .arg("--nocapture")
            .env("NILS_PROCESS_SUPERVISOR_LEAF", "1")
            .env("NILS_PROCESS_SUPERVISOR_TEST_PID_PATH", pid_path)
            .stdin(Stdio::null())
            .process_group(0);
        if env::var_os("NILS_PROCESS_SUPERVISOR_LEAF_INHERIT_OUTPUT").as_deref()
            != Some(OsStr::new("1"))
        {
            command.stdout(Stdio::null()).stderr(Stdio::null());
        }
        command.spawn().expect("spawn detached fixture leaf");
        if let Some(release_path) = env::var_os("NILS_PROCESS_SUPERVISOR_INTERMEDIATE_RELEASE_PATH")
        {
            while !Path::new(&release_path).exists() {
                std::thread::sleep(Duration::from_millis(2));
            }
        } else if let Some(delay) =
            env::var_os("NILS_PROCESS_SUPERVISOR_INTERMEDIATE_EXIT_DELAY_MS")
                .and_then(|value| value.to_str().and_then(|value| value.parse::<u64>().ok()))
        {
            std::thread::sleep(Duration::from_millis(delay));
        }
    }

    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn process_supervisor_leaf_test_fixture() {
        if env::var_os("NILS_PROCESS_SUPERVISOR_LEAF").as_deref() != Some(OsStr::new("1")) {
            return;
        }
        let pid_path =
            env::var_os("NILS_PROCESS_SUPERVISOR_TEST_PID_PATH").expect("fixture pid path");
        fs::write(pid_path, unsafe { libc::getpid() }.to_string())
            .expect("publish fixture leaf PID");
        loop {
            std::thread::park_timeout(Duration::from_secs(1));
        }
    }

    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[allow(clippy::zombie_processes)] // The collector must own the escaped leaf after this leader exits.
    fn output_cleanup_escaped_descendant_test_fixture() {
        if env::var_os("NILS_OUTPUT_CLEANUP_ESCAPED_DESCENDANT").as_deref() != Some(OsStr::new("1"))
        {
            return;
        }
        let pid_path =
            env::var_os("NILS_PROCESS_SUPERVISOR_TEST_PID_PATH").expect("fixture pid path");
        let exit_after_publish = env::var_os("NILS_OUTPUT_CLEANUP_EXIT_AFTER_PUBLISH").as_deref()
            == Some(OsStr::new("1"));
        let hard_exit_after_publish = env::var_os("NILS_OUTPUT_CLEANUP_HARD_EXIT_AFTER_PUBLISH")
            .as_deref()
            == Some(OsStr::new("1"));
        if let Some(start_path) = env::var_os("NILS_OUTPUT_CLEANUP_START_PATH") {
            while !Path::new(&start_path).exists() {
                std::thread::sleep(Duration::from_millis(2));
            }
        }
        let mut child = Command::new(env::current_exe().expect("current test executable"));
        child
            .arg("process_supervisor_intermediate_test_fixture")
            .arg("--nocapture")
            .env("NILS_PROCESS_SUPERVISOR_INTERMEDIATE", "1")
            .env("NILS_PROCESS_SUPERVISOR_TEST_PID_PATH", &pid_path)
            .stdin(Stdio::null())
            .process_group(0);
        if !exit_after_publish {
            child.env("NILS_PROCESS_SUPERVISOR_LEAF_INHERIT_OUTPUT", "1");
        } else {
            child.stdout(Stdio::null()).stderr(Stdio::null());
            if let Some(release_path) = env::var_os("NILS_OUTPUT_CLEANUP_RELEASE_PATH") {
                child.env(
                    "NILS_PROCESS_SUPERVISOR_INTERMEDIATE_RELEASE_PATH",
                    release_path,
                );
            } else {
                child.env("NILS_PROCESS_SUPERVISOR_INTERMEDIATE_EXIT_DELAY_MS", "500");
            }
        }
        let mut child = child
            .spawn()
            .expect("spawn escaped output fixture intermediate");
        let deadline = Instant::now() + Duration::from_secs(1);
        while !Path::new(&pid_path).exists() {
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("escaped output fixture leaf did not publish its PID");
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        child
            .wait()
            .expect("reap escaped output fixture intermediate");
        if exit_after_publish {
            std::thread::sleep(Duration::from_millis(100));
            if hard_exit_after_publish {
                unsafe { libc::_exit(exit::RUNTIME) };
            }
            return;
        }
        loop {
            std::thread::park_timeout(Duration::from_secs(1));
        }
    }

    #[test]
    fn dirty_checkout_error_contract_exhaustively_freezes_codes_exits_and_json_envelopes() {
        fn expected(kind: DirtyCheckoutErrorKind) -> (&'static str, i32) {
            match kind {
                DirtyCheckoutErrorKind::CleanCheckout => ("dirty-checkout-clean", exit::DATA),
                DirtyCheckoutErrorKind::UnsupportedGitState => {
                    ("dirty-checkout-unsupported-git-state", exit::DATA)
                }
                DirtyCheckoutErrorKind::ChallengeExpired => {
                    ("dirty-checkout-challenge-expired", exit::DATA)
                }
                DirtyCheckoutErrorKind::ChallengeReused => {
                    ("dirty-checkout-challenge-reused", exit::DATA)
                }
                DirtyCheckoutErrorKind::ChallengeDrift => {
                    ("dirty-checkout-challenge-drift", exit::DATA)
                }
                DirtyCheckoutErrorKind::ForeignLease => {
                    ("dirty-checkout-foreign-lease", exit::DATA)
                }
                DirtyCheckoutErrorKind::MalformedState => {
                    ("dirty-checkout-malformed-state", exit::DATA)
                }
                DirtyCheckoutErrorKind::InvalidInput => {
                    ("dirty-checkout-invalid-input", exit::DATA)
                }
                DirtyCheckoutErrorKind::Timeout => ("dirty-checkout-timeout", exit::RUNTIME),
                DirtyCheckoutErrorKind::ResourceUnavailable => {
                    ("dirty-checkout-resource-unavailable", exit::RUNTIME)
                }
                DirtyCheckoutErrorKind::SnapshotFailed => {
                    ("dirty-checkout-snapshot-failed", exit::RUNTIME)
                }
                DirtyCheckoutErrorKind::AdoptionFailed => {
                    ("dirty-checkout-adoption-failed", exit::RUNTIME)
                }
                DirtyCheckoutErrorKind::RevocationFailed => {
                    ("dirty-checkout-revoke-failed", exit::RUNTIME)
                }
            }
        }

        let kinds = [
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
        ];
        assert_eq!(kinds.len(), 13);

        for kind in kinds {
            let (code, exit_code) = expected(kind);
            assert_eq!(kind.code(), code);
            assert_eq!(kind.exit_code(), exit_code);
            assert_eq!(error_kind_from_code(code), Some(kind));

            let cli_error = adoption_cli_error(domain_error(kind, "contract message"));
            assert_eq!(cli_error.code, code);
            assert_eq!(cli_error.exit_code, exit_code);
            assert_eq!(cli_error.message.as_ref(), "contract message");
            assert!(cli_error.hint.is_none());
            assert!(cli_error.details.is_none());

            let value = serde_json::to_value(super::super::error_envelope(
                "worktree.dirty-snapshot",
                &cli_error,
            ))
            .expect("serialize exact error envelope");
            let mut envelope_keys = value
                .as_object()
                .expect("error envelope object")
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>();
            envelope_keys.sort_unstable();
            assert_eq!(envelope_keys, ["error", "ok", "schema_version"]);
            assert_eq!(
                value["schema_version"],
                "cli.git-cli.worktree.dirty-snapshot.v1"
            );
            assert_eq!(value["ok"], false);
            let mut error_keys = value["error"]
                .as_object()
                .expect("error object")
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>();
            error_keys.sort_unstable();
            assert_eq!(error_keys, ["code", "message"]);
            assert_eq!(value["error"]["code"], code);
            assert_eq!(value["error"]["message"], "contract message");
        }
    }

    #[test]
    fn snapshot_worker_success_rejects_semantically_invalid_identity_and_budgets() {
        let valid = serde_json::json!({
            "schema": SNAPSHOT_SCHEMA,
            "repository_key": "a".repeat(64),
            "checkout_key": "b".repeat(64),
            "checkout_instance": "c".repeat(32),
            "snapshot_id": "d".repeat(64),
            "head_oid": "e".repeat(40),
            "branch_ref_digest": "f".repeat(64),
            "tracked_entries": 1,
            "untracked_entries": 1,
            "hashed_bytes": 2,
        });
        let mut cases = Vec::new();
        for (label, field, value) in [
            ("malformed repository key", "repository_key", "a".repeat(63)),
            ("uppercase checkout key", "checkout_key", "B".repeat(64)),
            (
                "malformed checkout instance",
                "checkout_instance",
                "not-an-instance".to_string(),
            ),
            ("uppercase snapshot digest", "snapshot_id", "D".repeat(64)),
            (
                "malformed branch digest",
                "branch_ref_digest",
                "not-a-digest".to_string(),
            ),
        ] {
            let mut invalid = valid.clone();
            invalid[field] = serde_json::json!(value);
            cases.push((label, invalid));
        }
        let mut invalid_head = valid.clone();
        invalid_head["head_oid"] = serde_json::json!("not-a-head");
        cases.push(("malformed HEAD", invalid_head));
        let mut invalid_entries = valid.clone();
        invalid_entries["tracked_entries"] = serde_json::json!(MAX_ENTRY_COUNT);
        invalid_entries["untracked_entries"] = serde_json::json!(1);
        cases.push(("combined entry budget", invalid_entries));
        let mut invalid_bytes = valid;
        invalid_bytes["hashed_bytes"] = serde_json::json!(MAX_TOTAL_BYTES + 1);
        cases.push(("hashed byte budget", invalid_bytes));

        for (label, snapshot) in cases {
            let error = decode_snapshot_worker_output(successful_worker_output(snapshot))
                .expect_err("semantically invalid worker success must be rejected");
            assert_eq!(
                error
                    .downcast_ref::<DirtyCheckoutError>()
                    .expect("typed invalid worker response")
                    .kind(),
                DirtyCheckoutErrorKind::SnapshotFailed,
                "{label}"
            );
        }
    }

    #[test]
    fn snapshot_worker_executable_rejects_group_and_world_writable_files() {
        let root = tempfile::TempDir::new().expect("worker executable root");
        let executable = root.path().join("git-cli");
        fs::write(&executable, b"worker").expect("write worker fixture");

        for (mode, trusted) in [(0o700, true), (0o755, true), (0o775, false), (0o702, false)] {
            fs::set_permissions(&executable, fs::Permissions::from_mode(mode))
                .expect("set worker fixture mode");
            let metadata = fs::metadata(&executable).expect("worker fixture metadata");
            assert_eq!(
                validate_worker_file_metadata(&metadata).is_ok(),
                trusted,
                "unexpected worker trust result for mode {mode:o}"
            );
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn descriptor_backed_worker(path: &Path) -> SnapshotWorkerExecutable {
        let file = File::open(path).expect("open worker fixture descriptor");
        let metadata = file.metadata().expect("read worker fixture metadata");
        let digest = worker_file_digest(&file, metadata.len()).expect("hash worker fixture");
        let command_path = if cfg!(target_os = "linux") {
            PathBuf::from(format!("/proc/self/fd/{}", file.as_raw_fd()))
        } else {
            PathBuf::from(format!("/dev/fd/{}", file.as_raw_fd()))
        };
        SnapshotWorkerExecutable {
            source_path: path.to_path_buf(),
            command_path,
            file,
            metadata,
            digest,
            _private_root: None,
        }
    }

    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn descriptor_backed_worker_accepts_owner_controlled_group_writable_ancestry() {
        let root = tempfile::Builder::new()
            .prefix("worker-ancestor-")
            .tempdir_in(env::current_dir().expect("current test directory"))
            .expect("worker ancestor root");
        let executable = root.path().join("git-cli");
        fs::write(&executable, b"worker").expect("write worker fixture");
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700))
            .expect("set worker mode");
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o775))
            .expect("make owner-controlled ancestor group writable");

        descriptor_backed_worker(&executable)
            .revalidate()
            .expect("descriptor-backed launch must not depend on mutable ancestor path modes");
    }

    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn descriptor_backed_worker_remains_bound_after_path_replacement() {
        let root = tempfile::TempDir::new().expect("worker replacement root");
        let executable = root.path().join("git-cli");
        let displaced = root.path().join("git-cli.original");
        fs::write(&executable, b"trusted worker").expect("write worker fixture");
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700))
            .expect("set worker mode");
        let worker = descriptor_backed_worker(&executable);

        fs::rename(&executable, &displaced).expect("displace worker path");
        fs::write(&executable, b"replacement worker").expect("replace worker path");
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700))
            .expect("set replacement mode");

        worker
            .revalidate()
            .expect("held descriptor must remain bound to the originally validated worker inode");
        let held_metadata = worker.file.metadata().expect("held worker metadata");
        assert!(same_worker_descriptor_metadata(
            &worker.metadata,
            &held_metadata
        ));
        assert_eq!(
            worker_file_digest(&worker.file, held_metadata.len())
                .expect("held worker content digest"),
            worker.digest
        );
        assert!(!same_metadata(
            &worker.metadata,
            &fs::metadata(&executable).expect("replacement path metadata")
        ));
    }

    #[test]
    fn checkout_wide_recovery_prunes_once_after_processing_all_pending_records() {
        let records: Vec<_> = (0..MAX_RECOVERY_STATE_ENTRIES)
            .map(|index| (format!("token-{index}"), format!("receipt-{index}")))
            .collect();
        let recovered = std::cell::Cell::new(0_usize);
        let pruned = std::cell::Cell::new(0_usize);

        recover_pending_batch_with(
            records,
            "token-0",
            |_| {
                recovered.set(recovered.get() + 1);
                Ok(PendingRecovery::Revoked)
            },
            |_, _| panic!("revoked records do not require staged rollback"),
            |failed_receipt_id| {
                assert!(failed_receipt_id.is_none());
                pruned.set(pruned.get() + 1);
                Ok(())
            },
        )
        .expect("checkout-wide recovery succeeds");

        assert_eq!(recovered.get(), MAX_RECOVERY_STATE_ENTRIES - 1);
        assert_eq!(
            pruned.get(),
            1,
            "retention pruning must not rescan the checkout after every recovered marker"
        );
    }

    #[test]
    fn checkout_wide_recovery_prunes_after_failed_staged_rollback() {
        let state_entries = std::cell::Cell::new(MAX_RECOVERY_STATE_ENTRIES);
        let pruned = std::cell::Cell::new(0_usize);

        let error = recover_pending_batch_with(
            [("staged-token".to_string(), "staged-receipt".to_string())],
            "current-token",
            |_| Ok(PendingRecovery::Staged),
            |_, _| {
                state_entries.set(state_entries.get() + 1);
                Err(domain_error(
                    DirtyCheckoutErrorKind::AdoptionFailed,
                    "staged rollback failed after durable tombstone rename",
                ))
            },
            |failed_receipt_id| {
                assert_eq!(failed_receipt_id, Some("staged-receipt"));
                pruned.set(pruned.get() + 1);
                state_entries.set(state_entries.get() - 1);
                Ok(())
            },
        )
        .expect_err("the staged rollback failure remains primary");

        assert!(
            error
                .to_string()
                .contains("staged rollback failed after durable tombstone rename")
        );
        assert_eq!(pruned.get(), 1, "deferred pruning must still run");
        assert_eq!(state_entries.get(), MAX_RECOVERY_STATE_ENTRIES);
    }

    #[test]
    fn checkout_wide_recovery_preserves_primary_error_when_pruning_also_fails() {
        let error = recover_pending_batch_with(
            [("staged-token".to_string(), "staged-receipt".to_string())],
            "current-token",
            |_| Ok(PendingRecovery::Staged),
            |_, _| {
                Err(domain_error(
                    DirtyCheckoutErrorKind::AdoptionFailed,
                    "unique primary recovery failure",
                ))
            },
            |failed_receipt_id| {
                assert_eq!(failed_receipt_id, Some("staged-receipt"));
                Err(domain_error(
                    DirtyCheckoutErrorKind::ResourceUnavailable,
                    "unique deferred prune failure",
                ))
            },
        )
        .expect_err("both recovery and pruning fail");

        assert_eq!(
            error
                .downcast_ref::<DirtyCheckoutError>()
                .expect("primary typed error remains in the error chain")
                .kind(),
            DirtyCheckoutErrorKind::AdoptionFailed
        );
        let diagnostic = error.to_string();
        assert!(diagnostic.contains("unique primary recovery failure"));
        assert!(diagnostic.contains("unique deferred prune failure"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn process_identity_replacement_before_pinning_receives_no_signal() {
        let generation_a = ProcessIdentity {
            pid: 42_424,
            generation: 11,
        };
        let generation_b = ProcessIdentity {
            pid: generation_a.pid,
            generation: 12,
        };
        let current = std::cell::Cell::new(generation_a);
        let mut signals = Vec::new();

        let pinned = pin_process_identity_result_with(
            generation_a,
            |_| {
                current.set(generation_b);
                Ok(generation_a.pid)
            },
            |_| Ok(Some(current.get())),
        )
        .expect("post-open generation validation succeeds");
        if let Some(handle) = pinned {
            signals.push((handle, libc::SIGKILL));
        }

        assert!(pinned.is_none(), "generation B must not be pinned as A");
        assert!(signals.is_empty(), "generation B must receive zero signals");
    }

    #[test]
    fn delayed_cleanup_signals_use_only_the_successfully_pinned_generation() {
        struct SingleOwner(ProcessIdentity);

        impl OwnedProcessTracker for SingleOwner {
            fn identities(&mut self, _deadline: Instant) -> Result<Vec<ProcessIdentity>> {
                Ok(vec![self.0])
            }

            fn known_identities(&self) -> Vec<ProcessIdentity> {
                vec![self.0]
            }
        }

        struct ReplacingControl {
            replacement: ProcessIdentity,
            current: ProcessIdentity,
            signals: Vec<(ProcessIdentity, libc::c_int)>,
        }

        impl OwnedProcessControl for ReplacingControl {
            type Pinned = ProcessIdentity;

            fn pin(&mut self, identity: ProcessIdentity) -> Result<Option<Self::Pinned>> {
                self.current = self.replacement;
                Ok(Some(identity))
            }

            fn signal(&mut self, process: &Self::Pinned, signal: libc::c_int) -> Result<bool> {
                self.signals.push((*process, signal));
                Ok(true)
            }
        }

        let generation_a = ProcessIdentity {
            pid: 42_424,
            generation: 11,
        };
        let generation_b = ProcessIdentity {
            pid: generation_a.pid,
            generation: 12,
        };
        let mut owner = SingleOwner(generation_a);
        let mut control = ReplacingControl {
            replacement: generation_b,
            current: generation_a,
            signals: Vec::new(),
        };
        let mut child = Command::new(true_executable())
            .spawn()
            .expect("spawn completed cleanup child");

        terminate_owned_processes_until_with(
            &mut owner,
            &mut child,
            Instant::now() + Duration::from_secs(1),
            &mut control,
        )
        .expect("opaque-handle cleanup succeeds");

        assert_eq!(control.current, generation_b);
        assert_eq!(
            control.signals,
            vec![(generation_a, libc::SIGSTOP), (generation_a, libc::SIGKILL)],
            "cleanup must signal only the opaque handle acquired for generation A"
        );
        assert!(
            control
                .signals
                .iter()
                .all(|(handle, _)| *handle != generation_b),
            "the replacement generation must receive no cleanup signal"
        );
    }

    #[test]
    fn published_pid_cleanup_pins_the_original_generation_before_delayed_signal() {
        struct ReplacingControl {
            current: ProcessIdentity,
            replacement: ProcessIdentity,
            signals: Vec<(ProcessIdentity, libc::c_int)>,
        }

        impl OwnedProcessControl for ReplacingControl {
            type Pinned = ProcessIdentity;

            fn pin(&mut self, identity: ProcessIdentity) -> Result<Option<Self::Pinned>> {
                self.current = self.replacement;
                Ok(Some(identity))
            }

            fn signal(&mut self, process: &Self::Pinned, signal: libc::c_int) -> Result<bool> {
                self.signals.push((*process, signal));
                Ok(true)
            }
        }

        let generation_a = ProcessIdentity {
            pid: 54_321,
            generation: 7,
        };
        let generation_b = ProcessIdentity {
            pid: generation_a.pid,
            generation: 8,
        };
        let mut control = ReplacingControl {
            current: generation_a,
            replacement: generation_b,
            signals: Vec::new(),
        };

        let pinned = pin_published_process_with(&mut control, generation_a)
            .expect("pin published generation")
            .expect("published generation exists");
        signal_published_process_with(&mut control, &pinned).expect("clean published generation");

        assert_eq!(control.current, generation_b);
        assert_eq!(control.signals, vec![(generation_a, libc::SIGKILL)]);
        assert!(
            control
                .signals
                .iter()
                .all(|(identity, _)| *identity != generation_b),
            "delayed fixture cleanup must never signal the reused PID generation"
        );
    }

    #[test]
    fn supervisor_cleanup_completion_requires_authenticated_proof() {
        let mut valid = Vec::new();
        write_process_supervisor_completion(
            &mut valid,
            ProcessSupervisorCompletion {
                kind: ProcessSupervisorCompletionKind::Terminated,
                cleanup_complete: true,
                exit_code: exit::RUNTIME,
            },
        )
        .expect("serialize authenticated cleanup completion");
        let mut overlong = valid.clone();
        overlong.push(0);
        assert!(read_supervisor_cleanup_proof_with(&mut io::Cursor::new(
            valid
        )));
        assert!(!read_supervisor_cleanup_proof_with(&mut io::Cursor::new(
            overlong
        )));
        assert!(!read_supervisor_cleanup_proof_with(&mut io::Cursor::new(
            Vec::<u8>::new()
        )));
        assert!(!read_supervisor_cleanup_proof_with(&mut io::Cursor::new(
            b"wrong-proof".to_vec()
        )));
    }

    #[test]
    fn authenticated_target_status_one_remains_a_git_domain_result() {
        use std::os::unix::net::UnixStream;

        let (completion_reader, mut completion_writer) =
            UnixStream::pair().expect("target status completion socket");
        write_process_supervisor_completion(
            &mut completion_writer,
            ProcessSupervisorCompletion {
                kind: ProcessSupervisorCompletionKind::Target,
                cleanup_complete: true,
                exit_code: 1,
            },
        )
        .expect("deliver target status completion");
        drop(completion_writer);
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "exit 1"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        let child = command
            .spawn()
            .expect("spawn authenticated status-one fixture");
        let spawned = SpawnedOutputChild {
            child,
            supervised: true,
            cleanup_proof: Some(completion_reader),
            fallback_owner: None,
        };

        let output = collect_output_child_until(
            spawned,
            Instant::now() + Duration::from_secs(1),
            1024,
            1024,
            1024,
        )
        .expect("authenticated target status one remains available to Git consumers");

        assert_eq!(output.status.code(), Some(1));
    }

    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn authenticated_supervisor_producer_preserves_normal_target_status_one() {
        let root = tempfile::TempDir::new().expect("target status producer root");
        let spawned = spawn_authenticated_supervisor_output_fixture(
            "target-status-one",
            &root.path().join("unused.pid"),
        );

        let output = collect_output_child_until(
            spawned,
            Instant::now() + Duration::from_secs(5),
            1024,
            1024,
            1024,
        )
        .expect("authenticated target status one remains a Git-domain result");

        assert_eq!(output.status.code(), Some(exit::RUNTIME));
    }

    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn authenticated_supervisor_producer_internal_failure_is_not_git_status_one() {
        let root = tempfile::TempDir::new().expect("internal failure producer root");
        let spawned = spawn_authenticated_supervisor_output_fixture(
            "internal-failure",
            &root.path().join("unused.pid"),
        );

        let error = collect_output_child_until(
            spawned,
            Instant::now() + Duration::from_secs(5),
            1024,
            1024,
            1024,
        )
        .expect_err("authenticated internal failure must not enter the Git status domain");

        assert_eq!(
            error
                .downcast_ref::<DirtyCheckoutError>()
                .expect("typed authenticated internal failure")
                .kind(),
            DirtyCheckoutErrorKind::ResourceUnavailable
        );
    }

    #[test]
    fn authenticated_supervisor_internal_failure_cannot_become_git_status_one() {
        use std::os::unix::net::UnixStream;

        let (completion_reader, mut completion_writer) =
            UnixStream::pair().expect("internal failure completion socket");
        write_process_supervisor_completion(
            &mut completion_writer,
            ProcessSupervisorCompletion {
                kind: ProcessSupervisorCompletionKind::InternalFailure,
                cleanup_complete: false,
                exit_code: 1,
            },
        )
        .expect("deliver internal failure completion");
        drop(completion_writer);
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "exit 1"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        let child = command.spawn().expect("spawn internal status-one fixture");
        let spawned = SpawnedOutputChild {
            child,
            supervised: true,
            cleanup_proof: Some(completion_reader),
            fallback_owner: None,
        };

        let error = collect_output_child_until(
            spawned,
            Instant::now() + Duration::from_secs(1),
            1024,
            1024,
            1024,
        )
        .expect_err("authenticated internal failure must not become Git status one");

        assert_eq!(
            error
                .downcast_ref::<DirtyCheckoutError>()
                .expect("typed authenticated internal failure")
                .kind(),
            DirtyCheckoutErrorKind::ResourceUnavailable
        );
    }

    #[test]
    fn supervised_child_without_authenticated_completion_cannot_return_git_status_one() {
        use std::os::unix::net::UnixStream;

        let (completion_reader, completion_writer) =
            UnixStream::pair().expect("supervisor completion socket");
        drop(completion_writer);
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "exit 1"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        let child = command
            .spawn()
            .expect("spawn status-one supervisor fixture");
        let spawned = SpawnedOutputChild {
            child,
            supervised: true,
            cleanup_proof: Some(completion_reader),
            fallback_owner: None,
        };

        let error = collect_output_child_until(
            spawned,
            Instant::now() + Duration::from_secs(1),
            1024,
            1024,
            1024,
        )
        .expect_err("missing authenticated completion must not become Git status one");

        assert_eq!(
            error
                .downcast_ref::<DirtyCheckoutError>()
                .expect("typed missing supervisor completion error")
                .kind(),
            DirtyCheckoutErrorKind::ResourceUnavailable
        );
    }

    // macOS hosted runners can exhaust kqueue tracking while creating this fixture; see #1290.
    #[cfg(target_os = "linux")]
    #[test]
    fn missing_supervisor_completion_cleans_a_prescanned_escaped_descendant() {
        use std::os::unix::net::UnixStream;

        let root = tempfile::TempDir::new().expect("missing completion escaped root");
        let pid_path = root.path().join("escaped.pid");
        let start_path = root.path().join("start");
        let release_path = root.path().join("release");
        let (completion_reader, completion_writer) =
            UnixStream::pair().expect("missing completion socket");
        drop(completion_writer);
        let mut command = Command::new(env::current_exe().expect("current test executable"));
        command
            .arg("output_cleanup_escaped_descendant_test_fixture")
            .arg("--nocapture")
            .env("NILS_OUTPUT_CLEANUP_ESCAPED_DESCENDANT", "1")
            .env("NILS_OUTPUT_CLEANUP_EXIT_AFTER_PUBLISH", "1")
            .env("NILS_OUTPUT_CLEANUP_START_PATH", &start_path)
            .env("NILS_OUTPUT_CLEANUP_RELEASE_PATH", &release_path)
            .env("NILS_PROCESS_SUPERVISOR_TEST_PID_PATH", &pid_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        let child = command
            .spawn()
            .expect("spawn unexpected supervisor exit fixture");
        let mut fallback_owner = ProcessOwner::for_root(child.id() as libc::pid_t)
            .expect("create scoped escaped fallback owner");
        fs::write(&start_path, []).expect("release escaped fixture start gate");
        let publication_deadline = Instant::now() + Duration::from_secs(1);
        while !pid_path.exists() {
            assert!(
                Instant::now() < publication_deadline,
                "escaped fixture did not publish its PID"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        let pid = fs::read_to_string(&pid_path)
            .expect("read escaped fixture PID")
            .parse::<libc::pid_t>()
            .expect("parse escaped fixture PID");
        let identity = process_identity(pid)
            .expect("read escaped fixture identity")
            .expect("escaped fixture remains present before collection");
        let prescanned = fallback_owner
            .identities(Instant::now() + Duration::from_secs(1))
            .expect("prescan escaped supervisor descendants");
        assert!(
            prescanned.contains(&identity),
            "escaped descendant must be retained before unexpected supervisor exit"
        );
        let pinned = pin_process(identity)
            .expect("pin prescanned escaped descendant")
            .expect("prescanned escaped descendant remains present");
        fs::write(&release_path, []).expect("release escaped intermediate");
        let spawned = SpawnedOutputChild {
            child,
            supervised: true,
            cleanup_proof: Some(completion_reader),
            fallback_owner: Some(fallback_owner),
        };

        let error = collect_output_child_until(
            spawned,
            Instant::now() + Duration::from_secs(2),
            1024,
            1024,
            1024,
        )
        .expect_err("missing authenticated completion must fail supervision");
        assert_eq!(
            error
                .downcast_ref::<DirtyCheckoutError>()
                .expect("typed unexpected supervisor exit error")
                .kind(),
            DirtyCheckoutErrorKind::ResourceUnavailable
        );

        #[cfg(target_os = "linux")]
        let still_running = {
            let exit_deadline = Instant::now() + Duration::from_millis(500);
            loop {
                let state = fs::read(format!("/proc/{pid}/stat"))
                    .ok()
                    .and_then(|bytes| {
                        bytes
                            .iter()
                            .rposition(|byte| *byte == b')')
                            .and_then(|end| bytes.get(end + 2).copied())
                    });
                if state.is_none() || state == Some(b'Z') {
                    break false;
                }
                if Instant::now() >= exit_deadline {
                    break true;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
        };
        #[cfg(target_os = "macos")]
        let still_running = {
            let exit_deadline = Instant::now() + Duration::from_millis(500);
            loop {
                if process_identity(pid).expect("recheck escaped descendant") != Some(identity) {
                    break false;
                }
                if Instant::now() >= exit_deadline {
                    break true;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
        };
        if still_running {
            let _ = signal_pinned_process(&pinned, libc::SIGKILL);
            panic!("escaped descendant survived missing-completion fallback cleanup");
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn lost_trackers_fail_unavailable_without_signaling_an_unseen_descendant() {
        use std::os::unix::net::UnixStream;

        let root = tempfile::TempDir::new().expect("lost tracker characterization root");
        let pid_path = root.path().join("escaped.pid");
        let release_path = root.path().join("release");
        let (completion_reader, completion_writer) =
            UnixStream::pair().expect("lost tracker completion socket");
        drop(completion_writer);
        let mut command = Command::new(env::current_exe().expect("current test executable"));
        command
            .arg("output_cleanup_escaped_descendant_test_fixture")
            .arg("--nocapture")
            .env("NILS_OUTPUT_CLEANUP_ESCAPED_DESCENDANT", "1")
            .env("NILS_OUTPUT_CLEANUP_EXIT_AFTER_PUBLISH", "1")
            .env("NILS_OUTPUT_CLEANUP_HARD_EXIT_AFTER_PUBLISH", "1")
            .env("NILS_OUTPUT_CLEANUP_RELEASE_PATH", &release_path)
            .env("NILS_PROCESS_SUPERVISOR_TEST_PID_PATH", &pid_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        let mut child = command.spawn().expect("spawn hard-exit supervisor fixture");
        let supervisor_pid = child.id() as libc::pid_t;
        let fallback_owner =
            ProcessOwner::for_root(supervisor_pid).expect("create unscanned fallback owner");

        let mut canary_command = Command::new("/bin/sleep");
        canary_command
            .arg("60")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(supervisor_pid);
        let canary_child = canary_command
            .spawn()
            .expect("spawn unrelated process-group canary");
        let mut canary = SupervisorFixture {
            child: canary_child,
            completion_channel: None,
            reaped: false,
        };
        let canary_pid = canary.id() as libc::pid_t;
        let canary_identity = process_identity(canary_pid)
            .expect("read canary identity")
            .expect("canary remains present after spawn");
        let mut canary_cleanup = PublishedPidCleanup::new(canary_identity);
        assert!(
            canary_cleanup.process().is_some(),
            "test cleanup must pin the canary generation"
        );

        let publication_deadline = Instant::now() + Duration::from_secs(1);
        while !pid_path.exists() {
            assert!(
                Instant::now() < publication_deadline,
                "escaped fixture did not publish its PID"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        let leaf_pid = fs::read_to_string(&pid_path)
            .expect("read unseen leaf PID")
            .parse::<libc::pid_t>()
            .expect("parse unseen leaf PID");
        let leaf_identity = process_identity(leaf_pid)
            .expect("read unseen leaf identity")
            .expect("unseen leaf remains present before tracker loss");
        let mut test_only_leaf_reaper = PublishedPidCleanup::new(leaf_identity);
        assert!(
            test_only_leaf_reaper.process().is_some(),
            "test-only cleanup must pin the escaped leaf generation"
        );
        assert!(
            fallback_owner.known_identities().is_empty(),
            "the caller fallback owner must not prescan the escaped leaf"
        );

        fs::write(&release_path, []).expect("release hard-exit supervisor fixture");
        let supervisor_status = child.wait().expect("reap hard-exit supervisor fixture");
        assert_eq!(supervisor_status.code(), Some(exit::RUNTIME));
        assert_eq!(
            process_identity(supervisor_pid).expect("recheck supervisor identity"),
            None,
            "the supervisor generation must be gone before fallback collection"
        );

        reset_unpinned_cleanup_signal_attempts();
        let spawned = SpawnedOutputChild {
            child,
            supervised: true,
            cleanup_proof: Some(completion_reader),
            fallback_owner: Some(fallback_owner),
        };
        let collection_started = Instant::now();
        let error = collect_output_child_until(
            spawned,
            collection_started + Duration::from_secs(3),
            16 * 1024,
            16 * 1024,
            32 * 1024,
        )
        .expect_err("lost tracking authorities must fail unavailable");
        assert!(
            collection_started.elapsed() < Duration::from_secs(2),
            "missing cleanup proof must fail within the bounded cleanup window"
        );
        assert_eq!(
            error
                .downcast_ref::<DirtyCheckoutError>()
                .expect("typed lost tracker error")
                .kind(),
            DirtyCheckoutErrorKind::ResourceUnavailable
        );
        assert!(
            error
                .to_string()
                .contains("process supervisor completion could not be authenticated"),
            "the caller must not accept missing authenticated cleanup proof"
        );
        assert_eq!(
            unpinned_cleanup_signal_attempts(),
            0,
            "a reaped supervisor must disable every unpinned cleanup signal path"
        );

        assert_eq!(
            process_identity(canary_pid).expect("recheck canary identity"),
            Some(canary_identity),
            "cleanup must not replace or terminate the unrelated canary generation"
        );
        let canary_stat =
            fs::read(format!("/proc/{canary_pid}/stat")).expect("read canary process state");
        let canary_state = canary_stat
            .iter()
            .rposition(|byte| *byte == b')')
            .and_then(|end| canary_stat.get(end + 2).copied())
            .expect("parse canary process state");
        assert!(
            !matches!(canary_state, b'T' | b't' | b'Z'),
            "cleanup must not stop or terminate the unrelated process-group canary"
        );
        assert!(
            signal_pinned_process(canary_cleanup.process().expect("pinned canary handle"), 0,)
                .expect("probe pinned canary"),
            "the unrelated canary must remain alive"
        );
        assert_eq!(
            process_identity(leaf_pid).expect("recheck unseen leaf identity"),
            Some(leaf_identity),
            "no termination claim exists for a leaf unseen before every tracker was lost"
        );
        assert!(
            signal_pinned_process(
                test_only_leaf_reaper
                    .process()
                    .expect("pinned test-only leaf handle"),
                0,
            )
            .expect("probe pinned unseen leaf"),
            "the accepted residual leaves the unseen generation for test-only cleanup"
        );

        test_only_leaf_reaper
            .terminate_and_wait()
            .expect("terminate and observe the unseen leaf through its stable test handle");
        canary_cleanup
            .terminate_and_wait()
            .expect("terminate and observe the canary through its stable test handle");
        canary.wait().expect("reap unrelated canary");
    }

    #[test]
    fn reaped_supervisor_disables_unpinned_pid_and_process_group_signals() {
        assert!(!should_use_unpinned_cleanup_signals(true, true));
        assert!(should_use_unpinned_cleanup_signals(true, false));
        assert!(should_use_unpinned_cleanup_signals(false, true));
    }

    #[test]
    fn supervised_timeout_cleanup_signals_term_before_any_fallback_stop() {
        let mut signals = Vec::new();
        signal_cleanup_leader_with(42, true, |target, signal| {
            signals.push((target, signal));
            Ok(())
        })
        .expect("initial supervisor cleanup signal succeeds");

        assert_eq!(
            signals,
            vec![(42, libc::SIGTERM)],
            "the supervisor must retain execution so its ownership tracker can clean descendants"
        );
    }

    #[test]
    fn owned_cleanup_continues_until_a_quiescent_generation_pass() {
        struct WaveOwner {
            next_generation: u128,
            last_generation: u128,
        }

        impl OwnedProcessTracker for WaveOwner {
            fn identities(&mut self, _deadline: Instant) -> Result<Vec<ProcessIdentity>> {
                if self.next_generation > self.last_generation {
                    return Ok(Vec::new());
                }
                let identity = ProcessIdentity {
                    pid: 40_000 + self.next_generation as libc::pid_t,
                    generation: self.next_generation,
                };
                self.next_generation += 1;
                Ok(vec![identity])
            }

            fn known_identities(&self) -> Vec<ProcessIdentity> {
                Vec::new()
            }
        }

        struct RecordingControl(Vec<(ProcessIdentity, libc::c_int)>);

        impl OwnedProcessControl for RecordingControl {
            type Pinned = ProcessIdentity;

            fn pin(&mut self, identity: ProcessIdentity) -> Result<Option<Self::Pinned>> {
                Ok(Some(identity))
            }

            fn signal(&mut self, process: &Self::Pinned, signal: libc::c_int) -> Result<bool> {
                self.0.push((*process, signal));
                Ok(true)
            }
        }

        let mut owner = WaveOwner {
            next_generation: 1,
            last_generation: 5,
        };
        let mut control = RecordingControl(Vec::new());
        let mut child = Command::new(true_executable())
            .spawn()
            .expect("spawn completed cleanup child");

        terminate_owned_processes_until_with(
            &mut owner,
            &mut child,
            Instant::now() + Duration::from_secs(1),
            &mut control,
        )
        .expect("all escaping generations are cleaned");

        let killed: Vec<_> = control
            .0
            .iter()
            .filter_map(|(identity, signal)| (*signal == libc::SIGKILL).then_some(*identity))
            .collect();
        assert_eq!(
            killed.len(),
            5,
            "cleanup must not treat four discovery passes as a containment boundary"
        );
    }

    #[test]
    fn owned_cleanup_rejects_cumulative_generation_limit_plus_one() {
        struct ChurningOwner {
            next_generation: usize,
        }

        impl OwnedProcessTracker for ChurningOwner {
            fn identities(&mut self, _deadline: Instant) -> Result<Vec<ProcessIdentity>> {
                if self.next_generation > PROCESS_SCAN_LIMITS.descendants + 1 {
                    return Ok(Vec::new());
                }
                let identity = ProcessIdentity {
                    pid: 42_424,
                    generation: self.next_generation as u128,
                };
                self.next_generation += 1;
                Ok(vec![identity])
            }

            fn known_identities(&self) -> Vec<ProcessIdentity> {
                Vec::new()
            }
        }

        struct CountingControl {
            pinned: usize,
        }

        impl OwnedProcessControl for CountingControl {
            type Pinned = ProcessIdentity;

            fn pin(&mut self, identity: ProcessIdentity) -> Result<Option<Self::Pinned>> {
                self.pinned += 1;
                Ok(Some(identity))
            }

            fn signal(&mut self, _process: &Self::Pinned, _signal: libc::c_int) -> Result<bool> {
                Ok(true)
            }
        }

        let mut owner = ChurningOwner { next_generation: 1 };
        let mut control = CountingControl { pinned: 0 };
        let mut child = Command::new(true_executable())
            .spawn()
            .expect("spawn completed cleanup child");

        let error = terminate_owned_processes_until_with(
            &mut owner,
            &mut child,
            Instant::now() + Duration::from_secs(5),
            &mut control,
        )
        .expect_err("cumulative generation churn must fail closed");

        assert_eq!(
            error
                .downcast_ref::<DirtyCheckoutError>()
                .expect("typed generation-limit error")
                .kind(),
            DirtyCheckoutErrorKind::ResourceUnavailable
        );
        assert_eq!(control.pinned, PROCESS_SCAN_LIMITS.descendants);
    }

    #[test]
    fn failed_kill_resumes_a_stopped_pinned_process_before_cleanup_returns_error() {
        struct SingleOwner(ProcessIdentity);

        impl OwnedProcessTracker for SingleOwner {
            fn identities(&mut self, _deadline: Instant) -> Result<Vec<ProcessIdentity>> {
                Ok(vec![self.0])
            }

            fn known_identities(&self) -> Vec<ProcessIdentity> {
                vec![self.0]
            }
        }

        struct KillFailingControl {
            signals: Vec<libc::c_int>,
        }

        impl OwnedProcessControl for KillFailingControl {
            type Pinned = ProcessIdentity;

            fn pin(&mut self, identity: ProcessIdentity) -> Result<Option<Self::Pinned>> {
                Ok(Some(identity))
            }

            fn signal(&mut self, _process: &Self::Pinned, signal: libc::c_int) -> Result<bool> {
                self.signals.push(signal);
                if signal == libc::SIGKILL {
                    Err(process_scan_resource_error())
                } else {
                    Ok(true)
                }
            }
        }

        let identity = ProcessIdentity {
            pid: 42_424,
            generation: 11,
        };
        let mut owner = SingleOwner(identity);
        let mut control = KillFailingControl {
            signals: Vec::new(),
        };
        let mut child = Command::new(true_executable())
            .spawn()
            .expect("spawn completed cleanup child");

        terminate_owned_processes_until_with(
            &mut owner,
            &mut child,
            Instant::now() + Duration::from_secs(1),
            &mut control,
        )
        .expect_err("failed termination must fail containment");

        assert_eq!(
            control.signals,
            vec![libc::SIGSTOP, libc::SIGKILL, libc::SIGCONT],
            "cleanup must resume a process that it stopped but could not terminate"
        );
    }

    #[test]
    fn successful_kill_waits_for_pinned_process_termination_before_cleanup_returns() {
        struct TerminationControl {
            signals: Vec<libc::c_int>,
            waited: bool,
        }

        impl OwnedProcessControl for TerminationControl {
            type Pinned = ProcessIdentity;

            fn pin(&mut self, identity: ProcessIdentity) -> Result<Option<Self::Pinned>> {
                Ok(Some(identity))
            }

            fn signal(&mut self, _process: &Self::Pinned, signal: libc::c_int) -> Result<bool> {
                self.signals.push(signal);
                Ok(true)
            }

            fn wait_for_exit(&mut self, _process: &Self::Pinned, _deadline: Instant) -> Result<()> {
                self.waited = true;
                Ok(())
            }
        }

        let identity = ProcessIdentity {
            pid: 42_424,
            generation: 11,
        };
        let mut control = TerminationControl {
            signals: Vec::new(),
            waited: false,
        };

        terminate_owned_identity_until_with(
            &mut control,
            identity,
            Instant::now() + Duration::from_secs(1),
        )
        .expect("stable termination is observed");

        assert_eq!(control.signals, vec![libc::SIGSTOP, libc::SIGKILL]);
        assert!(
            control.waited,
            "cleanup completion must wait until the pinned generation terminates"
        );
    }

    #[test]
    fn cleanup_deadline_resumes_a_process_stopped_mid_phase_without_late_kill() {
        struct SingleOwner(ProcessIdentity);

        impl OwnedProcessTracker for SingleOwner {
            fn identities(&mut self, _deadline: Instant) -> Result<Vec<ProcessIdentity>> {
                Ok(vec![self.0])
            }

            fn known_identities(&self) -> Vec<ProcessIdentity> {
                vec![self.0]
            }
        }

        struct DeadlineCrossingControl {
            signals: Vec<libc::c_int>,
        }

        impl OwnedProcessControl for DeadlineCrossingControl {
            type Pinned = ProcessIdentity;

            fn pin(&mut self, identity: ProcessIdentity) -> Result<Option<Self::Pinned>> {
                Ok(Some(identity))
            }

            fn signal(&mut self, _process: &Self::Pinned, signal: libc::c_int) -> Result<bool> {
                self.signals.push(signal);
                if signal == libc::SIGSTOP {
                    std::thread::sleep(Duration::from_millis(75));
                }
                Ok(true)
            }
        }

        let identity = ProcessIdentity {
            pid: 42_424,
            generation: 11,
        };
        let mut owner = SingleOwner(identity);
        let mut control = DeadlineCrossingControl {
            signals: Vec::new(),
        };
        let mut child = Command::new(true_executable())
            .spawn()
            .expect("spawn completed cleanup child");

        terminate_owned_processes_until_with(
            &mut owner,
            &mut child,
            Instant::now() + Duration::from_millis(50),
            &mut control,
        )
        .expect_err("crossing the shared cleanup deadline must fail containment");

        assert_eq!(
            control.signals,
            vec![libc::SIGSTOP, libc::SIGCONT],
            "deadline expiry must resume the stopped process instead of signaling after expiry"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_process_pinning_propagates_post_open_identity_read_failures() {
        let identity = ProcessIdentity {
            pid: 42_424,
            generation: 11,
        };

        let error = pin_process_identity_result_with::<(), _, _>(
            identity,
            |_| Ok(()),
            |_| Err(process_scan_resource_error()),
        )
        .expect_err("post-pidfd identity read failures must fail containment");

        assert_eq!(
            error
                .downcast_ref::<DirtyCheckoutError>()
                .expect("typed identity-read containment error")
                .kind(),
            DirtyCheckoutErrorKind::ResourceUnavailable
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_process_pinning_treats_only_esrch_as_vanished() {
        let identity = ProcessIdentity {
            pid: 42_424,
            generation: 11,
        };
        let vanished = pin_process_identity_result_with::<(), _, _>(
            identity,
            |_| Err(io::Error::from_raw_os_error(libc::ESRCH)),
            |_| Ok(Some(identity)),
        )
        .expect("ESRCH means the discovered generation has exited");
        assert!(vanished.is_none());

        for error_code in [libc::ENOSYS, libc::EMFILE, libc::ENFILE, libc::EPERM] {
            let error = pin_process_identity_result_with::<(), _, _>(
                identity,
                |_| Err(io::Error::from_raw_os_error(error_code)),
                |_| Ok(Some(identity)),
            )
            .expect_err("pidfd resource and policy failures must fail containment");
            assert_eq!(
                error
                    .downcast_ref::<DirtyCheckoutError>()
                    .expect("typed pidfd containment error")
                    .kind(),
                DirtyCheckoutErrorKind::ResourceUnavailable
            );
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_pidfd_signaling_treats_only_esrch_as_vanished() {
        assert!(signal_pinned_process_result_with(|| Ok(())).expect("successful pidfd signal"));
        assert!(
            !signal_pinned_process_result_with(|| Err(io::Error::from_raw_os_error(libc::ESRCH)))
                .expect("ESRCH means the pinned generation has exited")
        );

        for error_code in [libc::ENOSYS, libc::EBADF, libc::EPERM, libc::EINVAL] {
            let error =
                signal_pinned_process_result_with(|| Err(io::Error::from_raw_os_error(error_code)))
                    .expect_err("pidfd signal failures other than ESRCH must fail containment");
            assert_eq!(
                error
                    .downcast_ref::<DirtyCheckoutError>()
                    .expect("typed pidfd signal containment error")
                    .kind(),
                DirtyCheckoutErrorKind::ResourceUnavailable
            );
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn cleanup_failure_terminates_known_processes_and_cannot_report_success() {
        struct FailingOwner {
            known: Vec<ProcessIdentity>,
        }

        impl OwnedProcessTracker for FailingOwner {
            fn identities(&mut self, _deadline: Instant) -> Result<Vec<ProcessIdentity>> {
                Err(process_scan_resource_error())
            }

            fn known_identities(&self) -> Vec<ProcessIdentity> {
                self.known.clone()
            }
        }

        let mut child = Command::new("/bin/sh")
            .args(["-c", "while :; do :; done"])
            .spawn()
            .expect("spawn cleanup failure fixture");
        let pid = child.id() as libc::pid_t;
        let identity = process_identity(pid)
            .expect("read cleanup fixture identity")
            .expect("cleanup fixture process exists");
        let mut owner = FailingOwner {
            known: vec![identity],
        };

        let error = terminate_owned_processes(&mut owner, &mut child)
            .expect_err("incomplete discovery must fail containment after cleanup");

        assert_eq!(
            error
                .downcast_ref::<DirtyCheckoutError>()
                .expect("typed cleanup failure")
                .kind(),
            DirtyCheckoutErrorKind::ResourceUnavailable
        );
        assert_process_absent(pid, "known process after incomplete discovery");
    }

    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn signal_terminated_target_cannot_be_authenticated_as_git_status_one() {
        let root = tempfile::TempDir::new().expect("signal target producer root");
        let spawned = spawn_authenticated_supervisor_output_fixture(
            "target-signal",
            &root.path().join("unused.pid"),
        );

        let error = collect_output_child_until(
            spawned,
            Instant::now() + Duration::from_secs(5),
            1024,
            1024,
            1024,
        )
        .expect_err("signal termination must remain outside the Git exit-status domain");

        assert_eq!(
            error
                .downcast_ref::<DirtyCheckoutError>()
                .expect("typed abnormal-target result")
                .kind(),
            DirtyCheckoutErrorKind::ResourceUnavailable
        );
    }

    #[test]
    fn successful_child_with_cleanup_failure_cannot_cross_supervisor_success_boundary() {
        struct CleanupFailingOwner;

        impl OwnedProcessTracker for CleanupFailingOwner {
            fn identities(&mut self, _deadline: Instant) -> Result<Vec<ProcessIdentity>> {
                Err(process_scan_resource_error())
            }

            fn known_identities(&self) -> Vec<ProcessIdentity> {
                Vec::new()
            }
        }

        let error = supervise_process_with_owner(
            OsString::from("/bin/true"),
            &[],
            &mut CleanupFailingOwner,
            None,
            Instant::now() + Duration::from_secs(1),
            || false,
        )
        .expect_err("cleanup failure must replace a successful child result");

        assert_eq!(
            error
                .downcast_ref::<DirtyCheckoutError>()
                .expect("typed supervisor cleanup failure")
                .kind(),
            DirtyCheckoutErrorKind::ResourceUnavailable
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn process_cleanup_deadline_bounds_every_cleanup_phase() {
        struct EmptyOwner;

        impl OwnedProcessTracker for EmptyOwner {
            fn identities(&mut self, _deadline: Instant) -> Result<Vec<ProcessIdentity>> {
                Ok(Vec::new())
            }

            fn known_identities(&self) -> Vec<ProcessIdentity> {
                Vec::new()
            }
        }

        let mut child = Command::new("/bin/sh")
            .args(["-c", "while :; do :; done"])
            .spawn()
            .expect("spawn expired cleanup fixture");
        let pid = child.id() as libc::pid_t;
        let mut owner = EmptyOwner;
        let error = terminate_owned_processes_until(
            &mut owner,
            &mut child,
            Instant::now() - Duration::from_millis(1),
        )
        .expect_err("an expired cleanup deadline must fail closed");

        assert_eq!(
            error
                .downcast_ref::<DirtyCheckoutError>()
                .expect("typed cleanup timeout")
                .kind(),
            DirtyCheckoutErrorKind::Timeout
        );
        let status = child.wait().expect("reap expired-deadline direct child");
        assert!(
            !status.success(),
            "the direct child must still be terminated"
        );
        assert_process_absent(pid, "expired-deadline direct child");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_process_scan_is_linear_bounded_and_deadline_first() {
        let limits = ProcessScanLimits {
            process_entries: 6,
            metadata_bytes: 600,
            descendants: 4,
        };
        let records = vec![
            ProcessRecord::new(10, 1, 10, 100),
            ProcessRecord::new(11, 10, 11, 100),
            ProcessRecord::new(12, 10, 12, 100),
            ProcessRecord::new(13, 11, 13, 100),
            ProcessRecord::new(14, 12, 14, 100),
            ProcessRecord::new(99, 1, 99, 100),
        ];
        let scan = collect_descendant_processes_with(
            10,
            Instant::now() + Duration::from_secs(1),
            limits,
            |_| Ok(records.clone()),
        )
        .expect("exact process, byte, and descendant caps must pass");
        assert_eq!(scan.identities.len(), 4);
        assert_eq!(scan.edge_visits, 4, "each descendant edge is visited once");

        let assert_resource = |result: Result<ProcessScan>| {
            assert_eq!(
                result
                    .expect_err("cap plus one must fail")
                    .downcast_ref::<DirtyCheckoutError>()
                    .expect("typed process scan error")
                    .kind(),
                DirtyCheckoutErrorKind::ResourceUnavailable
            );
        };
        assert_resource(collect_descendant_processes_with(
            10,
            Instant::now() + Duration::from_secs(1),
            ProcessScanLimits {
                process_entries: 5,
                ..limits
            },
            |_| Ok(records.clone()),
        ));
        assert_resource(collect_descendant_processes_with(
            10,
            Instant::now() + Duration::from_secs(1),
            ProcessScanLimits {
                metadata_bytes: 599,
                ..limits
            },
            |_| Ok(records.clone()),
        ));
        assert_resource(collect_descendant_processes_with(
            10,
            Instant::now() + Duration::from_secs(1),
            ProcessScanLimits {
                descendants: 3,
                ..limits
            },
            |_| Ok(records),
        ));

        let mut reads = 0;
        let expired = collect_descendant_processes_with(
            10,
            Instant::now() - Duration::from_millis(1),
            limits,
            |_| {
                reads += 1;
                Ok(Vec::new())
            },
        )
        .expect_err("an expired cleanup deadline must fail");
        assert_eq!(
            expired
                .downcast_ref::<DirtyCheckoutError>()
                .expect("typed cleanup timeout")
                .kind(),
            DirtyCheckoutErrorKind::Timeout
        );
        assert_eq!(reads, 0, "no process metadata may be read after expiry");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_targeted_reader_scans_only_reachable_processes_and_retains_a_bounded_prefix() {
        fn write_process(root: &Path, pid: libc::pid_t, parent: libc::pid_t, children: &str) {
            let task = root
                .join(pid.to_string())
                .join("task")
                .join(pid.to_string());
            fs::create_dir_all(&task).expect("create synthetic process task");
            fs::write(task.join("children"), children).expect("write synthetic children");
            let mut stat = format!("{pid} (process-{pid}) S {parent}").into_bytes();
            for _ in 0..17 {
                stat.extend_from_slice(b" 0");
            }
            stat.extend_from_slice(format!(" {pid}00\n").as_bytes());
            fs::write(root.join(pid.to_string()).join("stat"), stat).expect("write synthetic stat");
        }

        let proc_root = tempfile::TempDir::new().expect("synthetic proc root");
        write_process(proc_root.path(), 10, 1, "11\n");
        write_process(proc_root.path(), 11, 10, "12\n");
        write_process(proc_root.path(), 12, 11, "");
        write_process(proc_root.path(), 99, 1, "100\n");
        write_process(proc_root.path(), 100, 99, "");
        let limits = ProcessScanLimits {
            process_entries: 8,
            metadata_bytes: 4 * 1024,
            descendants: 2,
        };
        let mut observed = Vec::new();

        let scan = descendant_processes_at(
            proc_root.path(),
            10,
            Instant::now() + Duration::from_secs(1),
            limits,
            |identity| observed.push(identity),
        )
        .expect("reachable synthetic descendants must be discovered");

        assert_eq!(
            scan.identities
                .iter()
                .map(|identity| identity.pid)
                .collect::<Vec<_>>(),
            vec![11, 12]
        );
        assert_eq!(scan.edge_visits, 2);
        assert_eq!(observed, scan.identities);
        assert!(scan.identities.iter().all(|identity| identity.pid != 99));

        let mut bounded_prefix = Vec::new();
        let error = descendant_processes_at(
            proc_root.path(),
            10,
            Instant::now() + Duration::from_secs(1),
            ProcessScanLimits {
                descendants: 1,
                ..limits
            },
            |identity| bounded_prefix.push(identity),
        )
        .expect_err("descendant cap plus one must fail closed");
        assert_eq!(
            error
                .downcast_ref::<DirtyCheckoutError>()
                .expect("typed targeted scan error")
                .kind(),
            DirtyCheckoutErrorKind::ResourceUnavailable
        );
        assert_eq!(
            bounded_prefix
                .iter()
                .map(|identity| identity.pid)
                .collect::<Vec<_>>(),
            vec![11],
            "already discovered identities must remain available for cleanup"
        );

        let late_task = proc_root.path().join("10/task/20");
        fs::create_dir_all(&late_task).expect("create late synthetic task");
        fs::write(
            late_task.join("children"),
            vec![b' '; MAX_PROC_CHILDREN_BYTES + 1],
        )
        .expect("write oversized late-task metadata");
        let mut streamed_prefix = Vec::new();
        descendant_processes_at(
            proc_root.path(),
            10,
            Instant::now() + Duration::from_secs(1),
            ProcessScanLimits {
                metadata_bytes: MAX_PROC_CHILDREN_BYTES * 2,
                ..limits
            },
            |identity| streamed_prefix.push(identity),
        )
        .expect_err("a later task metadata overflow must fail closed");
        assert_eq!(
            streamed_prefix
                .iter()
                .map(|identity| identity.pid)
                .collect::<Vec<_>>(),
            vec![11],
            "a later task failure must preserve the earlier task's discovered identity"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_targeted_reader_rejects_recycled_child_pid_with_unrelated_parent() {
        fn write_process(root: &Path, pid: libc::pid_t, parent: libc::pid_t, children: &str) {
            let task = root
                .join(pid.to_string())
                .join("task")
                .join(pid.to_string());
            fs::create_dir_all(&task).expect("create synthetic process task");
            fs::write(task.join("children"), children).expect("write synthetic children");
            let mut stat = format!("{pid} (process-{pid}) S {parent}").into_bytes();
            for _ in 0..17 {
                stat.extend_from_slice(b" 0");
            }
            stat.extend_from_slice(format!(" {pid}00\n").as_bytes());
            fs::write(root.join(pid.to_string()).join("stat"), stat).expect("write synthetic stat");
        }

        let proc_root = tempfile::TempDir::new().expect("synthetic proc root");
        write_process(proc_root.path(), 10, 1, "11\n");
        write_process(proc_root.path(), 11, 99, "");

        let scan = descendant_processes_at(
            proc_root.path(),
            10,
            Instant::now() + Duration::from_secs(1),
            ProcessScanLimits {
                process_entries: 4,
                metadata_bytes: 1024,
                descendants: 2,
            },
            |_| {},
        )
        .expect("an unrelated PID generation must be skipped");

        assert!(
            scan.identities.is_empty(),
            "a recycled children entry whose current occupant has another parent is not owned"
        );
    }

    #[test]
    fn identity_bound_child_visiting_retains_safe_prefix_and_rejects_parent_swaps() {
        let parent = ProcessIdentity {
            pid: 10,
            generation: 1_000,
        };
        let child = ProcessIdentity {
            pid: 11,
            generation: 1_100,
        };
        let replacement = ProcessIdentity {
            pid: parent.pid,
            generation: 9_999,
        };
        let mut observed = Vec::new();
        let error = visit_identity_bound_children_with(
            parent,
            [Ok(11), Ok(12)],
            |pid| {
                if pid == parent.pid {
                    Ok(Some(ProcessRecord::new(pid, 1, parent.generation, 0)))
                } else if pid == child.pid {
                    Ok(Some(ProcessRecord::new(
                        pid,
                        parent.pid,
                        child.generation,
                        0,
                    )))
                } else {
                    Err(process_scan_resource_error())
                }
            },
            |identity| {
                observed.push(identity);
                Ok(())
            },
        )
        .expect_err("a later child metadata failure must fail containment");
        assert!(error.downcast_ref::<DirtyCheckoutError>().is_some());
        assert_eq!(
            observed,
            vec![child],
            "a child validated before a later failure remains available to cleanup"
        );

        let validations = std::cell::Cell::new(0_u8);
        observed.clear();
        visit_identity_bound_children_with(
            parent,
            [Ok(11)],
            |pid| {
                if pid == parent.pid {
                    let validation = validations.get();
                    validations.set(validation + 1);
                    let generation = if validation == 0 {
                        parent.generation
                    } else {
                        replacement.generation
                    };
                    Ok(Some(ProcessRecord::new(pid, 1, generation, 0)))
                } else {
                    Ok(Some(ProcessRecord::new(
                        pid,
                        parent.pid,
                        child.generation,
                        0,
                    )))
                }
            },
            |identity| {
                observed.push(identity);
                Ok(())
            },
        )
        .expect("a parent replacement is an absent edge, not a scan failure");
        assert!(
            observed.is_empty(),
            "a child edge must not publish across a parent generation change"
        );
    }

    #[test]
    fn macos_fallback_rejects_reused_root_generation_before_descendant_scan() {
        let root = ProcessIdentity {
            pid: 10,
            generation: 1_000,
        };
        let replacement = ProcessIdentity {
            pid: root.pid,
            generation: 9_999,
        };
        let unrelated = ProcessIdentity {
            pid: 11,
            generation: 11_000,
        };
        let scans = std::cell::Cell::new(0_usize);

        let scan = macos_fallback_descendants_with(
            root,
            Instant::now() + Duration::from_secs(1),
            |pid| {
                assert_eq!(pid, root.pid);
                Ok(Some(replacement))
            },
            |identity, _deadline| {
                assert_eq!(identity, root);
                scans.set(scans.get() + 1);
                Ok(ProcessScan {
                    identities: vec![unrelated],
                    #[cfg(all(test, target_os = "linux"))]
                    edge_visits: 1,
                })
            },
        )
        .expect("a reused root is an absent edge, not a scan failure");

        assert!(
            scan.identities.is_empty(),
            "fallback discovery must not adopt descendants through a reused root PID"
        );
        assert_eq!(
            scans.get(),
            0,
            "a stale root identity must suppress descendant discovery"
        );
    }

    #[test]
    fn macos_fallback_rejects_root_reuse_between_precheck_and_scan() {
        let root = ProcessIdentity {
            pid: 20,
            generation: 20_000,
        };
        let replacement = ProcessIdentity {
            pid: root.pid,
            generation: 29_999,
        };
        let unrelated = ProcessIdentity {
            pid: 21,
            generation: 21_000,
        };
        let child_lists = std::cell::Cell::new(0_usize);
        let observed = std::cell::RefCell::new(Vec::new());

        let scan = macos_fallback_descendants_with(
            root,
            Instant::now() + Duration::from_secs(1),
            |pid| {
                assert_eq!(pid, root.pid);
                Ok(Some(root))
            },
            |expected_root, deadline| {
                macos_descendant_processes_from_identity_with(
                    expected_root,
                    deadline,
                    |pid| match pid {
                        pid if pid == root.pid => Ok(Some(ProcessRecord::new(
                            replacement.pid,
                            1,
                            replacement.generation,
                            1,
                        ))),
                        pid if pid == unrelated.pid => Ok(Some(ProcessRecord::new(
                            unrelated.pid,
                            root.pid,
                            unrelated.generation,
                            1,
                        ))),
                        _ => Ok(None),
                    },
                    |pid, children| {
                        assert_eq!(pid, root.pid);
                        child_lists.set(child_lists.get() + 1);
                        children[0] = unrelated.pid;
                        Ok(1)
                    },
                    |identity| observed.borrow_mut().push(identity),
                )
            },
        )
        .expect("root reuse during fallback scanning is a vanished ownership edge");

        assert!(
            scan.identities.is_empty(),
            "fallback scanning must remain bound to the prevalidated root generation"
        );
        assert_eq!(child_lists.get(), 0);
        assert!(observed.borrow().is_empty());
    }

    #[test]
    fn macos_fallback_fail_closed_boundary_matrix() {
        let root = ProcessIdentity {
            pid: 30,
            generation: 30_000,
        };
        let child = ProcessIdentity {
            pid: 31,
            generation: 31_000,
        };

        let vanished_scans = std::cell::Cell::new(0_usize);
        let vanished = macos_fallback_descendants_with(
            root,
            Instant::now() + Duration::from_secs(1),
            |_| Ok(None),
            |_, _| {
                vanished_scans.set(vanished_scans.get() + 1);
                Ok(ProcessScan {
                    identities: vec![child],
                    #[cfg(all(test, target_os = "linux"))]
                    edge_visits: 1,
                })
            },
        )
        .expect("a vanished root is an absent ownership edge");
        assert!(vanished.identities.is_empty());
        assert_eq!(vanished_scans.get(), 0);

        let failed_scans = std::cell::Cell::new(0_usize);
        let identity_error = macos_fallback_descendants_with(
            root,
            Instant::now() + Duration::from_secs(1),
            |_| Err(process_scan_resource_error()),
            |_, _| {
                failed_scans.set(failed_scans.get() + 1);
                Ok(ProcessScan {
                    identities: vec![child],
                    #[cfg(all(test, target_os = "linux"))]
                    edge_visits: 1,
                })
            },
        )
        .expect_err("identity read failures must remain containment failures");
        assert_eq!(
            identity_error
                .downcast_ref::<DirtyCheckoutError>()
                .expect("typed identity failure")
                .kind(),
            DirtyCheckoutErrorKind::ResourceUnavailable
        );
        assert_eq!(failed_scans.get(), 0);

        let expired_identity_reads = std::cell::Cell::new(0_usize);
        let expired_scans = std::cell::Cell::new(0_usize);
        let expired = macos_fallback_descendants_with(
            root,
            Instant::now() - Duration::from_secs(1),
            |_| {
                expired_identity_reads.set(expired_identity_reads.get() + 1);
                Ok(Some(root))
            },
            |_, _| {
                expired_scans.set(expired_scans.get() + 1);
                Ok(ProcessScan {
                    identities: vec![child],
                    #[cfg(all(test, target_os = "linux"))]
                    edge_visits: 1,
                })
            },
        )
        .expect_err("an expired fallback deadline must fail before discovery");
        assert_eq!(
            expired
                .downcast_ref::<DirtyCheckoutError>()
                .expect("typed fallback timeout")
                .kind(),
            DirtyCheckoutErrorKind::Timeout
        );
        assert_eq!(expired_identity_reads.get(), 0);
        assert_eq!(expired_scans.get(), 0);

        let delegated_scans = std::cell::Cell::new(0_usize);
        let delegated = macos_fallback_descendants_with(
            root,
            Instant::now() + Duration::from_secs(1),
            |_| Ok(Some(root)),
            |_, _| {
                delegated_scans.set(delegated_scans.get() + 1);
                Ok(ProcessScan {
                    identities: vec![child],
                    #[cfg(all(test, target_os = "linux"))]
                    edge_visits: 1,
                })
            },
        )
        .expect("an exact live root delegates discovery");
        assert_eq!(delegated.identities, vec![child]);
        assert_eq!(delegated_scans.get(), 1);

        let scanner_error = macos_fallback_descendants_with(
            root,
            Instant::now() + Duration::from_secs(1),
            |_| Ok(Some(root)),
            |_, _| Err(process_scan_resource_error()),
        )
        .expect_err("scanner containment failures must propagate");
        assert_eq!(
            scanner_error
                .downcast_ref::<DirtyCheckoutError>()
                .expect("typed scanner failure")
                .kind(),
            DirtyCheckoutErrorKind::ResourceUnavailable
        );
    }

    #[test]
    fn macos_event_reducer_retains_reparented_child_after_parent_reap() {
        let root = ProcessIdentity {
            pid: 10,
            generation: 1_000,
        };
        let parent = ProcessIdentity {
            pid: 11,
            generation: 1_100,
        };
        let leaf = ProcessIdentity {
            pid: 12,
            generation: 1_200,
        };
        let mut tracked = std::collections::HashMap::from([(parent.pid, parent)]);
        let mut pending = std::collections::HashMap::new();

        reduce_macos_process_event_with(
            root,
            &mut tracked,
            &mut pending,
            MacosProcessEvent::Exit { pid: parent.pid },
            |_| Ok(None),
        )
        .expect("parent exit reduction succeeds");
        reduce_macos_process_event_with(
            root,
            &mut tracked,
            &mut pending,
            MacosProcessEvent::Child { pid: leaf.pid },
            |pid| Ok(Some(ProcessRecord::new(pid, 1, leaf.generation, 0))),
        )
        .expect("reparented child event reduction succeeds");
        assert_eq!(pending.get(&leaf.pid), Some(&leaf));
        assert!(!tracked.contains_key(&leaf.pid));

        reduce_macos_process_event_with(
            root,
            &mut tracked,
            &mut pending,
            MacosProcessEvent::Drained,
            |pid| Ok(Some(ProcessRecord::new(pid, 1, leaf.generation, 0))),
        )
        .expect("drained child generation revalidation succeeds");
        assert_eq!(
            tracked.get(&leaf.pid),
            Some(&leaf),
            "NOTE_TRACK ancestry must survive rapid parent reap and child reparenting"
        );
        assert!(pending.is_empty());
    }

    #[test]
    fn macos_event_reducer_rejects_reused_child_before_drain() {
        let root = ProcessIdentity {
            pid: 10,
            generation: 1_000,
        };
        let child = ProcessIdentity {
            pid: 11,
            generation: 1_100,
        };
        let replacement = ProcessIdentity {
            pid: child.pid,
            generation: 9_999,
        };
        let mut tracked = std::collections::HashMap::new();
        let mut pending = std::collections::HashMap::new();

        reduce_macos_process_event_with(
            root,
            &mut tracked,
            &mut pending,
            MacosProcessEvent::Child { pid: child.pid },
            |pid| Ok(Some(ProcessRecord::new(pid, root.pid, child.generation, 0))),
        )
        .expect("child event stages an exact generation");
        reduce_macos_process_event_with(
            root,
            &mut tracked,
            &mut pending,
            MacosProcessEvent::Exit { pid: child.pid },
            |pid| Ok(Some(ProcessRecord::new(pid, 1, replacement.generation, 0))),
        )
        .expect("exit cancels the pending generation");
        reduce_macos_process_event_with(
            root,
            &mut tracked,
            &mut pending,
            MacosProcessEvent::Drained,
            |pid| Ok(Some(ProcessRecord::new(pid, 1, replacement.generation, 0))),
        )
        .expect("drain after replacement succeeds");

        assert!(tracked.is_empty(), "the replacement PID is never published");
        assert!(pending.is_empty());
    }

    #[test]
    fn macos_event_reducer_discards_generation_change_before_drain() {
        let root = ProcessIdentity {
            pid: 10,
            generation: 1_000,
        };
        let child = ProcessIdentity {
            pid: 11,
            generation: 1_100,
        };
        let replacement = ProcessIdentity {
            pid: child.pid,
            generation: 9_999,
        };
        let mut tracked = std::collections::HashMap::new();
        let mut pending = std::collections::HashMap::new();

        reduce_macos_process_event_with(
            root,
            &mut tracked,
            &mut pending,
            MacosProcessEvent::Child { pid: child.pid },
            |pid| Ok(Some(ProcessRecord::new(pid, root.pid, child.generation, 0))),
        )
        .expect("child event stages generation A");
        reduce_macos_process_event_with(
            root,
            &mut tracked,
            &mut pending,
            MacosProcessEvent::Drained,
            |pid| Ok(Some(ProcessRecord::new(pid, 1, replacement.generation, 0))),
        )
        .expect("drain generation comparison succeeds");

        assert!(tracked.is_empty(), "neither generation is published");
        assert!(pending.is_empty());
    }

    #[test]
    fn macos_event_reducer_stale_exit_preserves_tracked_replacement() {
        let root = ProcessIdentity {
            pid: 10,
            generation: 1_000,
        };
        let replacement = ProcessIdentity {
            pid: 11,
            generation: 9_999,
        };
        let mut tracked = std::collections::HashMap::from([(replacement.pid, replacement)]);
        let mut pending = std::collections::HashMap::new();

        reduce_macos_process_event_with(
            root,
            &mut tracked,
            &mut pending,
            MacosProcessEvent::Exit {
                pid: replacement.pid,
            },
            |pid| {
                Ok(Some(ProcessRecord::new(
                    pid,
                    root.pid,
                    replacement.generation,
                    0,
                )))
            },
        )
        .expect("stale exit event reduction succeeds");
        assert_eq!(
            tracked.get(&replacement.pid),
            Some(&replacement),
            "an old exit event must not erase the current tracked generation"
        );
    }

    #[test]
    fn macos_child_resolution_discards_edges_when_parent_generation_changes() {
        let parent = ProcessIdentity {
            pid: 10,
            generation: 1_000,
        };
        let replacement = ProcessIdentity {
            pid: parent.pid,
            generation: 9_999,
        };
        let validations = std::cell::Cell::new(0_u8);
        let mut observed = Vec::new();

        visit_identity_bound_children_with(
            parent,
            [Ok(11)],
            |pid| {
                if pid == parent.pid {
                    let validation = validations.get();
                    validations.set(validation + 1);
                    let generation = if validation == 0 {
                        parent.generation
                    } else {
                        replacement.generation
                    };
                    Ok(Some(ProcessRecord::new(pid, 1, generation, 0)))
                } else {
                    Ok(Some(ProcessRecord::new(pid, parent.pid, 1_100, 0)))
                }
            },
            |identity| {
                observed.push(identity);
                Ok(())
            },
        )
        .expect("generation validation must remain available");

        assert!(
            observed.is_empty(),
            "children resolved across a parent generation change must be discarded"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_targeted_reader_revalidates_parent_generation_before_following_its_children() {
        fn write_process(
            root: &Path,
            pid: libc::pid_t,
            parent: libc::pid_t,
            generation: u128,
            children: &str,
        ) {
            let task = root
                .join(pid.to_string())
                .join("task")
                .join(pid.to_string());
            fs::create_dir_all(&task).expect("create synthetic process task");
            fs::write(task.join("children"), children).expect("write synthetic children");
            let mut stat = format!("{pid} (process-{pid}) S {parent}").into_bytes();
            for _ in 0..17 {
                stat.extend_from_slice(b" 0");
            }
            stat.extend_from_slice(format!(" {generation}\n").as_bytes());
            fs::write(root.join(pid.to_string()).join("stat"), stat).expect("write synthetic stat");
        }

        let proc_root = tempfile::TempDir::new().expect("synthetic proc root");
        write_process(proc_root.path(), 10, 1, 1_000, "11\n");
        write_process(proc_root.path(), 11, 10, 1_100, "12\n");
        write_process(proc_root.path(), 12, 11, 1_200, "");
        let mut observed = Vec::new();

        let scan = descendant_processes_at(
            proc_root.path(),
            10,
            Instant::now() + Duration::from_secs(1),
            ProcessScanLimits {
                process_entries: 8,
                metadata_bytes: 8 * 1024,
                descendants: 4,
            },
            |identity| {
                observed.push(identity);
                if identity.pid == 11 {
                    write_process(proc_root.path(), 11, 99, 9_999, "12\n");
                }
            },
        )
        .expect("a replaced frontier parent must be discarded rather than followed");

        assert_eq!(
            scan.identities
                .iter()
                .map(|identity| identity.pid)
                .collect::<Vec<_>>(),
            vec![11],
            "the replacement generation's child edge must never become owned"
        );
        assert_eq!(observed, scan.identities);
    }

    #[test]
    fn direct_supervisor_activation_cannot_execute_an_arbitrary_program() {
        let root = tempfile::TempDir::new().expect("supervisor activation root");
        let marker = root.path().join("executed");
        let executable = snapshot_worker_executable().expect("trusted git-cli executable");
        let status = Command::new(&executable.source_path)
            .env(PROCESS_SUPERVISOR_ENV, "1")
            .args([
                OsStr::new("/bin/sh"),
                OsStr::new("-c"),
                OsStr::new("printf executed > \"$NILS_SUPERVISOR_MARKER\""),
            ])
            .env("NILS_SUPERVISOR_MARKER", &marker)
            .status()
            .expect("invoke direct supervisor activation");

        assert!(
            !status.success(),
            "unauthenticated supervisor mode must fail"
        );
        assert!(!marker.exists(), "unauthenticated argv must never execute");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_process_stat_parser_accepts_non_utf8_process_names() {
        let mut stat = b"4242 (process-\xff-name) S 7".to_vec();
        for _ in 0..17 {
            stat.extend_from_slice(b" 0");
        }
        stat.extend_from_slice(b" 12345\n");

        let record = parse_linux_process_record(&stat)
            .expect("non-UTF-8 process names must not hide process identity");
        assert_eq!(record.identity.pid, 4242);
        assert_eq!(record.parent, 7);
        assert_eq!(record.identity.generation, 12345);
    }

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

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn supervisor_ownership_eof_terminates_the_target_before_the_original_deadline() {
        let root = tempfile::TempDir::new().expect("ownership EOF root");
        let pid_path = root.path().join("blocked.pid");
        let mut supervisor = spawn_authenticated_supervisor_fixture_with_timeout(
            "blocked-child",
            &pid_path,
            Duration::from_secs(5),
        );
        let publication_deadline = Instant::now() + Duration::from_secs(1);
        while !pid_path.exists() {
            assert!(
                Instant::now() < publication_deadline,
                "blocked target did not publish its PID"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        let pid = fs::read_to_string(&pid_path)
            .expect("read blocked target PID")
            .parse::<libc::pid_t>()
            .expect("parse blocked target PID");

        supervisor.abandon_owner();
        let owner_loss_deadline = Instant::now() + Duration::from_millis(500);
        let status = loop {
            if let Some(status) = supervisor.try_wait().expect("poll abandoned supervisor") {
                break status;
            }
            assert!(
                Instant::now() < owner_loss_deadline,
                "supervisor ignored authenticated ownership-channel EOF"
            );
            std::thread::sleep(Duration::from_millis(2));
        };

        assert!(status.success(), "ownership-loss supervisor fixture failed");
        assert_process_absent(pid, "target after supervisor ownership loss");
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn supervisor_retains_the_original_absolute_deadline_after_spawn() {
        let root = tempfile::TempDir::new().expect("supervisor deadline root");
        let pid_path = root.path().join("blocked.pid");
        let started = Instant::now();
        let mut supervisor = spawn_authenticated_supervisor_fixture_with_timeout_and_delay(
            "blocked-child",
            &pid_path,
            Duration::from_millis(500),
            Duration::from_millis(300),
        );
        let completion_deadline = Instant::now() + Duration::from_secs(1);
        let status = loop {
            if let Some(status) = supervisor.try_wait().expect("poll deadline supervisor") {
                break status;
            }
            assert!(
                Instant::now() < completion_deadline,
                "supervisor did not retain the original absolute deadline"
            );
            std::thread::sleep(Duration::from_millis(2));
        };

        assert!(status.success(), "deadline supervisor fixture failed");
        assert!(
            started.elapsed() < Duration::from_millis(700),
            "supervisor reset the deadline after delayed authentication: {:?}",
            started.elapsed()
        );
        let pid = fs::read_to_string(&pid_path)
            .expect("read deadline target PID")
            .parse::<libc::pid_t>()
            .expect("parse deadline target PID");
        assert_process_absent(pid, "target after supervisor deadline");
    }

    #[test]
    fn successful_git_output_reaps_a_surviving_process_group_child() {
        let root = tempfile::TempDir::new().expect("surviving child root");
        let pid_path = root.path().join("child.pid");
        let mut supervisor = spawn_authenticated_supervisor_fixture("successful-child", &pid_path);
        let publication_deadline = Instant::now() + Duration::from_secs(1);
        while !pid_path.exists() {
            assert!(
                Instant::now() < publication_deadline,
                "successful child did not publish its process identity"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        let pid: libc::pid_t = fs::read_to_string(&pid_path)
            .expect("read surviving child pid")
            .parse()
            .expect("parse surviving child pid");
        let identity = process_identity(pid)
            .expect("read surviving child identity")
            .expect("surviving child generation exists");
        let cleanup = PublishedPidCleanup::new(identity);
        let status = supervisor.wait().expect("wait for supervisor fixture");

        assert!(status.success(), "authenticated supervisor fixture failed");
        assert_process_absent(pid, "successful command child");
        drop(cleanup);
    }

    #[test]
    fn git_output_deadline_does_not_join_a_descendant_held_pipe() {
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "while :; do :; done & while :; do :; done"])
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

    // Hosted macOS coverage races kqueue ownership with the exiting group leader; see #1290.
    #[cfg(target_os = "linux")]
    #[test]
    fn output_cleanup_tracks_escaped_descendants_after_the_group_leader_exits() {
        let root = tempfile::TempDir::new().expect("escaped output child root");
        let pid_path = root.path().join("escaped.pid");
        let spawned =
            spawn_authenticated_supervisor_output_fixture("escaped-output-timeout", &pid_path);
        let publication_deadline = Instant::now() + Duration::from_secs(1);
        while !pid_path.exists() {
            assert!(
                Instant::now() < publication_deadline,
                "escaped output leaf did not publish its process identity"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        let pid: libc::pid_t = fs::read_to_string(&pid_path)
            .expect("read escaped output leaf PID")
            .parse()
            .expect("parse escaped output leaf PID");
        let identity = process_identity(pid)
            .expect("read escaped output leaf identity")
            .expect("escaped output leaf generation exists");
        let cleanup = PublishedPidCleanup::new(identity);

        let error = collect_output_child_until(
            spawned,
            Instant::now() + Duration::from_millis(100),
            1024,
            1024,
            1024,
        )
        .expect_err("the parked target must hit the supervised output deadline");
        assert_eq!(
            error
                .downcast_ref::<DirtyCheckoutError>()
                .expect("typed escaped output timeout")
                .kind(),
            DirtyCheckoutErrorKind::Timeout
        );
        assert_process_absent(pid, "escaped output descendant after leader exit");
        drop(cleanup);
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

    // macOS hosted runners cannot reliably start this four-process kqueue fixture; see #1290.
    #[cfg(target_os = "linux")]
    #[test]
    fn detached_double_fork_pipe_holder_is_owned_and_terminated() {
        let root = tempfile::TempDir::new().expect("detached child root");
        let pid_path = root.path().join("detached.pid");
        let mut supervisor =
            spawn_authenticated_supervisor_fixture("detached-double-fork", &pid_path);
        let started = Instant::now();
        while !pid_path.exists() {
            assert!(
                started.elapsed() < Duration::from_secs(1),
                "detached fixture did not publish its process identity"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        let pid: libc::pid_t = fs::read_to_string(&pid_path)
            .expect("read detached final pid")
            .parse()
            .expect("parse detached final pid");
        let identity = process_identity(pid)
            .expect("read detached final identity")
            .expect("detached final generation exists");
        let cleanup = PublishedPidCleanup::new(identity);
        unsafe {
            libc::kill(-(supervisor.id() as libc::pid_t), libc::SIGTERM);
        }
        let status = supervisor
            .wait()
            .expect("wait for detached supervisor fixture");
        assert!(status.success(), "detached supervisor fixture failed");

        assert_process_absent(pid, "detached double-fork timeout descendant");
        drop(cleanup);
    }

    #[test]
    fn snapshot_budget_counters_accept_exact_limits_and_reject_limit_plus_one() {
        let one_byte = std::process::Output {
            status: std::process::ExitStatus::from_raw(0),
            stdout: vec![0],
            stderr: Vec::new(),
        };
        let assert_resource = |error: anyhow::Error, label: &str| {
            assert_eq!(
                error
                    .downcast_ref::<DirtyCheckoutError>()
                    .unwrap_or_else(|| panic!("typed {label} budget error"))
                    .kind(),
                DirtyCheckoutErrorKind::ResourceUnavailable,
                "unexpected {label} budget error: {error}"
            );
        };

        let mut capture = SnapshotBudget {
            capture_bytes: MAX_CAPTURE_BYTES - 1,
            ..SnapshotBudget::default()
        };
        capture
            .charge_output(&one_byte)
            .expect("exact capture-byte limit is accepted");
        assert_resource(
            capture
                .output_limit(1)
                .expect_err("capture limit plus one must be rejected"),
            "capture",
        );

        let mut paths = SnapshotBudget {
            path_bytes: MAX_PATH_BYTES - 1,
            ..SnapshotBudget::default()
        };
        paths
            .charge_path(1)
            .expect("exact path-byte limit is accepted");
        assert_resource(
            paths
                .charge_path(1)
                .expect_err("path-byte limit plus one must be rejected"),
            "path",
        );

        let mut traversal = SnapshotBudget {
            traversal_entries: MAX_ENTRY_COUNT - 1,
            ..SnapshotBudget::default()
        };
        traversal
            .charge_traversal_entry()
            .expect("exact traversal-entry limit is accepted");
        assert_resource(
            traversal
                .charge_traversal_entry()
                .expect_err("traversal-entry limit plus one must be rejected"),
            "traversal",
        );
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
        const CHILD_ENV: &str = "NILS_GIT_CLI_TEST_PREOPEN_BUDGET";
        if env::var_os(CHILD_ENV).is_none() {
            let status = Command::new(env::current_exe().expect("current test executable"))
                .arg("aggregate_file_budget_is_checked_before_opening_the_file")
                .arg("--nocapture")
                .env(CHILD_ENV, "1")
                .env("RUST_TEST_THREADS", "1")
                .status()
                .expect("run isolated descriptor-budget test");
            assert!(status.success(), "isolated descriptor-budget test failed");
            return;
        }

        let root = tempfile::TempDir::new().expect("snapshot root");
        let canonical_root = fs::canonicalize(root.path()).expect("canonical snapshot root");
        let path = canonical_root.join("unopenable");
        fs::write(&path, b"x").expect("write fixture");
        let mut hasher = FramedHasher::new();
        let mut budget = SnapshotBudget {
            file_bytes: MAX_TOTAL_BYTES,
            ..SnapshotBudget::default()
        };
        let mut limits = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        assert_eq!(
            unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limits) },
            0,
            "read descriptor limit"
        );
        let original = limits;
        limits.rlim_cur = 0;
        assert_eq!(
            unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limits) },
            0,
            "exhaust descriptor budget"
        );

        let result = hash_worktree_object(
            &mut hasher,
            &canonical_root,
            b"unopenable",
            false,
            None,
            &mut budget,
            Instant::now() + Duration::from_secs(1),
        );

        assert_eq!(
            unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &original) },
            0,
            "restore descriptor limit"
        );
        let error = result.expect_err("aggregate budget must be checked before file open");
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
        let checkout_root =
            fs::canonicalize(checkout.path()).expect("canonicalize checkout fixture root");
        let state_root_path =
            fs::canonicalize(state_root.path()).expect("canonicalize state fixture root");
        let checkout_dir = checkout_state_dir(
            &state_root_path,
            &resolve_checkout(
                &checkout_root,
                false,
                deadline_after(SNAPSHOT_TIMEOUT).expect("identity deadline"),
            )
            .expect("resolve identity"),
        )
        .expect("resolve checkout state");
        let mut calls = 0;

        let error = adopt_dirty_with_snapshot(
            &checkout_root,
            &state_root_path,
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
        let checkout_root =
            fs::canonicalize(checkout.path()).expect("canonicalize checkout fixture root");
        let state_root_path =
            fs::canonicalize(state_root.path()).expect("canonicalize state fixture root");
        let checkout_dir = state_root_path
            .join(&snapshot.repository_key)
            .join(&snapshot.checkout_key);
        let mut calls = 0;

        let error = adopt_dirty_with_snapshot(
            &checkout_root,
            &state_root_path,
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
            &checkout_root,
            &state_root_path,
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
    fn post_install_rollback_prunes_revocation_tombstones() {
        let (checkout, state_root, reason_path, snapshot, _challenge_path) =
            adoption_transaction_fixture();
        let checkout_root =
            fs::canonicalize(checkout.path()).expect("canonicalize checkout fixture root");
        let state_root_path =
            fs::canonicalize(state_root.path()).expect("canonicalize state fixture root");
        let identity = resolve_checkout(
            &checkout_root,
            false,
            deadline_after(SNAPSHOT_TIMEOUT).expect("identity deadline"),
        )
        .expect("resolve checkout identity");
        let checkout_dir = state_root_path
            .join(&snapshot.repository_key)
            .join(&snapshot.checkout_key);
        let mut receipt_ids = Vec::new();
        for index in 0..MAX_REVOCATION_TOMBSTONES {
            let receipt_id = format!("{index:064x}");
            let adopted_at = index as u64 + 1;
            let lease = LeaseRecord::V2(Box::new(LeaseV2Wire {
                schema: LEASE_V2_SCHEMA.to_string(),
                session_key: "d".repeat(64),
                checkout_instance: identity.checkout_instance.clone(),
                checkout_root: identity.root.to_string_lossy().into_owned(),
                checkout_git_dir: identity.git_dir.to_string_lossy().into_owned(),
                checkout_root_bytes: hex_bytes(identity.root.as_os_str().as_bytes()),
                checkout_git_dir_bytes: hex_bytes(identity.git_dir.as_os_str().as_bytes()),
                acquired_at: adopted_at,
                refreshed_at: adopted_at,
                expires_at: adopted_at + 100,
                adoption: AdoptionRecord {
                    schema: ADOPTION_SCHEMA.to_string(),
                    receipt_schema: RECEIPT_SCHEMA.to_string(),
                    receipt_id: receipt_id.clone(),
                    snapshot_id: "e".repeat(64),
                    authorization_turn_digest: "f".repeat(64),
                    reason_digest: "1".repeat(64),
                    adopted_at,
                    challenge_issued_at: adopted_at - 1,
                    challenge_digest: "2".repeat(64),
                },
            }));
            write_json_atomic(
                &checkout_dir.join(format!(".revoked-{receipt_id}.json")),
                &lease,
                false,
            )
            .expect("write rollback tombstone fixture");
            receipt_ids.push(receipt_id);
        }
        let mut calls = 0;

        adopt_dirty_with_snapshot(
            &checkout_root,
            &state_root_path,
            &"a".repeat(64),
            &reason_path,
            |path| {
                calls += 1;
                if calls == 4 {
                    fs::write(path.join("dirty.txt"), b"drift for tombstone pruning\n")
                        .expect("introduce post-stage drift");
                }
                dirty_snapshot(path)
            },
        )
        .expect_err("post-stage drift must roll back adoption");

        let remaining = fs::read_dir(&checkout_dir)
            .expect("list rollback tombstones")
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_name().as_bytes().starts_with(b".revoked-"))
            .count();
        assert_eq!(remaining, MAX_REVOCATION_TOMBSTONES);
        assert!(
            !checkout_dir
                .join(format!(".revoked-{}.json", receipt_ids[0]))
                .exists(),
            "oldest unprotected tombstone must be pruned"
        );
    }

    #[test]
    fn adoption_expiry_is_checked_at_the_durable_authorization_boundary() {
        let (checkout, state_root, reason_path, snapshot, challenge_path) =
            adoption_transaction_fixture();
        let checkout_root =
            fs::canonicalize(checkout.path()).expect("canonicalize checkout fixture root");
        let state_root_path =
            fs::canonicalize(state_root.path()).expect("canonicalize state fixture root");
        let challenge: ChallengeRecord =
            serde_json::from_slice(&fs::read(&challenge_path).expect("read expiring challenge"))
                .expect("parse expiring challenge");
        let checkout_dir = state_root_path
            .join(&snapshot.repository_key)
            .join(&snapshot.checkout_key);
        let valid_at = challenge.issued_at;
        let expired_at = challenge.expires_at;
        let fourth_barrier_seen = std::cell::Cell::new(false);
        let authoritative_lease_visible = std::cell::Cell::new(false);
        let mut snapshot_calls = 0;

        let error = adopt_dirty_with_snapshot_and_clock(
            &checkout_root,
            &state_root_path,
            &"a".repeat(64),
            &reason_path,
            |path| {
                snapshot_calls += 1;
                if snapshot_calls == 4 {
                    authoritative_lease_visible.set(checkout_dir.join("lease.json").exists());
                    fourth_barrier_seen.set(true);
                }
                dirty_snapshot(path)
            },
            || {
                Ok(if fourth_barrier_seen.get() {
                    expired_at
                } else {
                    valid_at
                })
            },
        )
        .expect_err("authorization expiring before durable acceptance must be rejected");

        assert_eq!(snapshot_calls, 4);
        assert!(
            !authoritative_lease_visible.get(),
            "authoritative lease must remain unpublished through the fourth barrier"
        );
        assert_eq!(
            error
                .downcast_ref::<DirtyCheckoutError>()
                .expect("typed durable-boundary expiry")
                .kind(),
            DirtyCheckoutErrorKind::ChallengeExpired
        );
        assert!(
            !checkout_dir.join("lease.json").exists(),
            "expired authorization must not leave an accepted lease"
        );
    }

    #[test]
    fn crash_after_lease_staging_rechecks_authorization_before_commit_recovery() {
        let (checkout, state_root, reason_path, snapshot, challenge_path) =
            adoption_transaction_fixture();
        let checkout_root =
            fs::canonicalize(checkout.path()).expect("canonicalize checkout fixture root");
        let state_root_path =
            fs::canonicalize(state_root.path()).expect("canonicalize state fixture root");
        let challenge: ChallengeRecord =
            serde_json::from_slice(&fs::read(&challenge_path).expect("read crash challenge"))
                .expect("parse crash challenge");
        let checkout_dir = state_root_path
            .join(&snapshot.repository_key)
            .join(&snapshot.checkout_key);
        let mut snapshot_calls = 0;

        let crashed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = adopt_dirty_with_snapshot_and_clock(
                &checkout_root,
                &state_root_path,
                &"a".repeat(64),
                &reason_path,
                |path| {
                    snapshot_calls += 1;
                    if snapshot_calls == 4 {
                        panic!("injected crash after durable lease staging");
                    }
                    dirty_snapshot(path)
                },
                || Ok(challenge.issued_at),
            );
        }));
        assert!(crashed.is_err(), "fault injection must interrupt adoption");
        assert!(
            !checkout_dir.join("lease.json").exists(),
            "crash before barrier four must not publish authoritative authority"
        );
        assert_eq!(
            fs::read_dir(&checkout_dir)
                .expect("list staged crash state")
                .filter_map(|entry| entry.ok())
                .filter(|entry| entry.file_name().as_bytes().starts_with(b".pending-lease-"))
                .count(),
            1,
            "crash must retain one private staged lease"
        );
        assert!(
            fs::read_dir(&checkout_dir)
                .expect("list pending crash state")
                .filter_map(|entry| entry.ok())
                .any(|entry| entry
                    .file_name()
                    .as_bytes()
                    .starts_with(b".pending-adoption-")),
            "crash must retain provisional pending state"
        );

        let error = adopt_dirty_with_snapshot_and_clock(
            &checkout_root,
            &state_root_path,
            &"a".repeat(64),
            &reason_path,
            dirty_snapshot,
            || Ok(challenge.expires_at),
        )
        .expect_err("expired provisional authorization must not recover as committed");

        assert_eq!(
            error
                .downcast_ref::<DirtyCheckoutError>()
                .expect("typed crash-recovery expiry")
                .kind(),
            DirtyCheckoutErrorKind::ChallengeExpired
        );
        assert!(
            !checkout_dir.join("lease.json").exists(),
            "unverified provisional lease must be revoked"
        );
    }

    #[test]
    fn staged_retry_preserves_a_newer_authoritative_foreign_lease() {
        let (checkout, state_root, reason_path, snapshot, challenge_path) =
            adoption_transaction_fixture();
        let checkout_root =
            fs::canonicalize(checkout.path()).expect("canonicalize checkout fixture root");
        let state_root_path =
            fs::canonicalize(state_root.path()).expect("canonicalize state fixture root");
        let identity = resolve_checkout(
            &checkout_root,
            false,
            deadline_after(SNAPSHOT_TIMEOUT).expect("identity deadline"),
        )
        .expect("resolve checkout identity");
        let challenge: ChallengeRecord =
            serde_json::from_slice(&fs::read(&challenge_path).expect("read crash challenge"))
                .expect("parse crash challenge");
        let checkout_dir = state_root_path
            .join(&snapshot.repository_key)
            .join(&snapshot.checkout_key);
        let lease_path = checkout_dir.join("lease.json");
        let mut snapshot_calls = 0;

        let crashed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = adopt_dirty_with_snapshot_and_clock(
                &checkout_root,
                &state_root_path,
                &"a".repeat(64),
                &reason_path,
                |path| {
                    snapshot_calls += 1;
                    if snapshot_calls == 4 {
                        panic!("injected crash after durable lease staging");
                    }
                    dirty_snapshot(path)
                },
                || Ok(challenge.issued_at),
            );
        }));
        assert!(crashed.is_err(), "fault injection must interrupt adoption");

        let foreign = LeaseRecord::V1(LeaseV1Wire {
            schema: LEASE_V1_SCHEMA.to_string(),
            session_key: "b".repeat(64),
            checkout_instance: identity.checkout_instance.clone(),
            checkout_root: identity.root.to_string_lossy().into_owned(),
            checkout_git_dir: identity.git_dir.to_string_lossy().into_owned(),
            acquired_at: challenge.issued_at,
            refreshed_at: challenge.issued_at,
            expires_at: challenge.expires_at.saturating_add(3_600),
        });
        let mut foreign_bytes =
            serde_json::to_vec_pretty(&foreign).expect("serialize newer foreign lease");
        foreign_bytes.push(b'\n');
        fs::write(&lease_path, &foreign_bytes).expect("write newer foreign lease");
        fs::set_permissions(&lease_path, fs::Permissions::from_mode(0o600))
            .expect("make newer foreign lease private");
        sync_directory(&checkout_dir).expect("make newer foreign lease durable");

        let error = adopt_dirty_with_snapshot_and_clock(
            &checkout_root,
            &state_root_path,
            &"a".repeat(64),
            &reason_path,
            dirty_snapshot,
            || Ok(challenge.issued_at),
        )
        .expect_err("staged retry must not overwrite a newer foreign lease");

        assert_eq!(
            error
                .downcast_ref::<DirtyCheckoutError>()
                .expect("typed foreign staged-retry conflict")
                .kind(),
            DirtyCheckoutErrorKind::ForeignLease
        );
        assert_eq!(
            fs::read(&lease_path).expect("read preserved foreign lease"),
            foreign_bytes,
            "staged retry must preserve the exact newer foreign lease bytes"
        );
        assert!(
            fs::read_dir(&checkout_dir)
                .expect("list retained staged state")
                .filter_map(|entry| entry.ok())
                .any(|entry| entry.file_name().as_bytes().starts_with(b".pending-lease-")),
            "foreign conflict must retain staged recovery evidence"
        );
    }

    #[test]
    fn committed_retry_requires_lease_to_remain_active_through_snapshot() {
        let (checkout, state_root, reason_path, snapshot, _challenge_path) =
            adoption_transaction_fixture();
        let checkout_root =
            fs::canonicalize(checkout.path()).expect("canonicalize checkout fixture root");
        let state_root_path =
            fs::canonicalize(state_root.path()).expect("canonicalize state fixture root");
        let checkout_dir = state_root_path
            .join(&snapshot.repository_key)
            .join(&snapshot.checkout_key);

        adopt_dirty_with_snapshot_and_clock(
            &checkout_root,
            &state_root_path,
            &"a".repeat(64),
            &reason_path,
            dirty_snapshot,
            unix_time,
        )
        .expect("create committed adoption");
        let lease = load_lease(&checkout_dir.join("lease.json"))
            .expect("load committed lease")
            .expect("committed lease");
        let before_expiry = lease.expires_at() - 1;
        let at_expiry = lease.expires_at();
        let clock_calls = std::cell::Cell::new(0usize);

        let error = adopt_dirty_with_snapshot_and_clock(
            &checkout_root,
            &state_root_path,
            &"a".repeat(64),
            &reason_path,
            dirty_snapshot,
            || {
                let call = clock_calls.get();
                clock_calls.set(call + 1);
                Ok(if call == 0 { before_expiry } else { at_expiry })
            },
        )
        .expect_err("retry must fail when the lease expires during snapshot validation");

        assert_eq!(
            error
                .downcast_ref::<DirtyCheckoutError>()
                .expect("typed committed retry expiry")
                .kind(),
            DirtyCheckoutErrorKind::ChallengeReused
        );
        assert_eq!(clock_calls.get(), 2, "retry must check lease expiry twice");
    }

    #[test]
    fn pending_rollback_binds_expired_authoritative_lease_bytes() {
        let now = 1_000;
        let v1 = LeaseRecord::V1(LeaseV1Wire {
            schema: LEASE_V1_SCHEMA.to_string(),
            session_key: "a".repeat(64),
            checkout_instance: "b".repeat(64),
            checkout_root: "/checkout".to_string(),
            checkout_git_dir: "/checkout/.git".to_string(),
            acquired_at: 1,
            refreshed_at: 2,
            expires_at: now - 1,
        });
        let v2 = LeaseRecord::V2(Box::new(LeaseV2Wire {
            schema: LEASE_V2_SCHEMA.to_string(),
            session_key: "a".repeat(64),
            checkout_instance: "b".repeat(64),
            checkout_root: "/checkout".to_string(),
            checkout_git_dir: "/checkout/.git".to_string(),
            checkout_root_bytes: hex_bytes(OsStr::new("/checkout").as_bytes()),
            checkout_git_dir_bytes: hex_bytes(OsStr::new("/checkout/.git").as_bytes()),
            acquired_at: 1,
            refreshed_at: 2,
            expires_at: now - 1,
            adoption: AdoptionRecord {
                schema: ADOPTION_SCHEMA.to_string(),
                receipt_schema: RECEIPT_SCHEMA.to_string(),
                receipt_id: "c".repeat(64),
                snapshot_id: "d".repeat(64),
                authorization_turn_digest: "e".repeat(64),
                reason_digest: "f".repeat(64),
                adopted_at: 2,
                challenge_issued_at: 1,
                challenge_digest: "1".repeat(64),
            },
        }));

        for lease in [&v1, &v2] {
            let bytes = serde_json::to_vec(lease).expect("serialize expired predecessor");
            assert_eq!(
                preserved_predecessor_lease_bytes(Some(lease), Some(bytes.clone())),
                Some(bytes),
                "pre-publication rollback must retain the untouched expired predecessor"
            );
        }
    }

    #[test]
    fn adoption_rollback_restores_exact_max_size_active_same_session_v1_lease() {
        let (checkout, state_root, reason_path, snapshot, _challenge_path) =
            adoption_transaction_fixture();
        let checkout_root =
            fs::canonicalize(checkout.path()).expect("canonicalize checkout fixture root");
        let state_root_path =
            fs::canonicalize(state_root.path()).expect("canonicalize state fixture root");
        let identity = resolve_checkout(
            &checkout_root,
            false,
            deadline_after(SNAPSHOT_TIMEOUT).expect("identity deadline"),
        )
        .expect("resolve checkout identity");
        let checkout_dir = state_root_path
            .join(&snapshot.repository_key)
            .join(&snapshot.checkout_key);
        let lease_path = checkout_dir.join("lease.json");
        let now = unix_time().expect("predecessor lease time");
        let predecessor = LeaseRecord::V1(LeaseV1Wire {
            schema: LEASE_V1_SCHEMA.to_string(),
            session_key: "b".repeat(64),
            checkout_instance: identity.checkout_instance.clone(),
            checkout_root: identity.root.to_string_lossy().into_owned(),
            checkout_git_dir: identity.git_dir.to_string_lossy().into_owned(),
            acquired_at: now - 10,
            refreshed_at: now,
            expires_at: now + 3_600,
        });
        let mut predecessor_bytes =
            serde_json::to_vec_pretty(&predecessor).expect("serialize exact predecessor");
        predecessor_bytes.push(b'\n');
        predecessor_bytes.resize(MAX_STATE_FILE_BYTES, b' ');
        fs::write(&lease_path, &predecessor_bytes).expect("write exact predecessor lease");
        fs::set_permissions(&lease_path, fs::Permissions::from_mode(0o600))
            .expect("make predecessor lease private");
        sync_directory(&checkout_dir).expect("make predecessor durable");
        let mut snapshot_calls = 0;

        let error = adopt_dirty_with_snapshot(
            &checkout_root,
            &state_root_path,
            &"a".repeat(64),
            &reason_path,
            |path| {
                snapshot_calls += 1;
                if snapshot_calls == 4 {
                    fs::write(path.join("dirty.txt"), b"rollback exact predecessor\n")
                        .expect("introduce post-install drift");
                }
                dirty_snapshot(path)
            },
        )
        .expect_err("post-install drift must roll back over the v1 predecessor");

        assert_eq!(
            error
                .downcast_ref::<DirtyCheckoutError>()
                .expect("typed predecessor rollback")
                .kind(),
            DirtyCheckoutErrorKind::ChallengeDrift
        );
        assert_eq!(
            fs::read(&lease_path).expect("read restored predecessor"),
            predecessor_bytes,
            "rollback must restore the exact predecessor bytes"
        );
    }

    #[test]
    fn adoption_recovery_scan_rejects_the_entry_cap_plus_one_before_mutation() {
        const RECOVERY_ENTRY_CAP: usize = 1_024;
        let (checkout, state_root, reason_path, snapshot, challenge_path) =
            adoption_transaction_fixture();
        let checkout_root =
            fs::canonicalize(checkout.path()).expect("canonicalize checkout fixture root");
        let state_root_path =
            fs::canonicalize(state_root.path()).expect("canonicalize state fixture root");
        let checkout_dir = state_root_path
            .join(&snapshot.repository_key)
            .join(&snapshot.checkout_key);
        for index in 0..RECOVERY_ENTRY_CAP {
            fs::write(checkout_dir.join(format!("inert-{index:04}")), b"")
                .expect("write inert recovery scan entry");
        }
        fs::write(checkout_root.join("dirty.txt"), b"stale challenge\n")
            .expect("drift challenge fixture");

        let error = adopt_dirty(
            &checkout_root,
            &state_root_path,
            &"a".repeat(64),
            &reason_path,
        )
        .expect_err("oversized recovery scan must fail closed");

        assert_eq!(
            error
                .downcast_ref::<DirtyCheckoutError>()
                .expect("typed recovery scan limit")
                .kind(),
            DirtyCheckoutErrorKind::ResourceUnavailable
        );
        assert!(
            challenge_path.exists(),
            "scan refusal must not consume challenge"
        );
        assert!(
            !checkout_dir.join("receipts").exists(),
            "scan refusal must precede transition artifacts"
        );
    }

    #[test]
    fn adoption_recovers_checkout_wide_pending_predecessor_before_another_token() {
        let (checkout, state_root, reason_path, snapshot, challenge_path) =
            adoption_transaction_fixture();
        let checkout_root =
            fs::canonicalize(checkout.path()).expect("canonicalize checkout fixture root");
        let state_root_path =
            fs::canonicalize(state_root.path()).expect("canonicalize state fixture root");
        let identity = resolve_checkout(
            &checkout_root,
            false,
            deadline_after(SNAPSHOT_TIMEOUT).expect("identity deadline"),
        )
        .expect("resolve checkout identity");
        let checkout_dir = state_root_path
            .join(&snapshot.repository_key)
            .join(&snapshot.checkout_key);
        let challenge_dir = checkout_dir.join("challenges");
        let lease_path = checkout_dir.join("lease.json");
        let now = unix_time().expect("predecessor lease time");
        let predecessor = LeaseRecord::V1(LeaseV1Wire {
            schema: LEASE_V1_SCHEMA.to_string(),
            session_key: "b".repeat(64),
            checkout_instance: identity.checkout_instance.clone(),
            checkout_root: identity.root.to_string_lossy().into_owned(),
            checkout_git_dir: identity.git_dir.to_string_lossy().into_owned(),
            acquired_at: now - 10,
            refreshed_at: now,
            expires_at: now + 3_600,
        });
        let mut predecessor_bytes =
            serde_json::to_vec_pretty(&predecessor).expect("serialize predecessor");
        predecessor_bytes.push(b'\n');
        fs::write(&lease_path, &predecessor_bytes).expect("write predecessor lease");
        fs::set_permissions(&lease_path, fs::Permissions::from_mode(0o600))
            .expect("make predecessor private");
        sync_directory(&checkout_dir).expect("make predecessor durable");
        let original_challenge: ChallengeRecord =
            serde_json::from_slice(&fs::read(&challenge_path).expect("read first challenge"))
                .expect("parse first challenge");
        let first_token_digest = sha256_hex("a".repeat(64).as_bytes());
        let mut snapshot_calls = 0;

        let crashed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = adopt_dirty_with_snapshot(
                &checkout_root,
                &state_root_path,
                &"a".repeat(64),
                &reason_path,
                |path| {
                    snapshot_calls += 1;
                    if snapshot_calls == 4 {
                        panic!("injected crash after first-token lease staging");
                    }
                    dirty_snapshot(path)
                },
            );
        }));
        assert!(crashed.is_err(), "fault injection must interrupt adoption");
        let first_pending_path = pending_adoption_path(&checkout_dir, &first_token_digest);
        let pending_bytes = fs::read(&first_pending_path).expect("read first pending marker");
        let pending: PendingAdoptionRecord =
            serde_json::from_slice(&pending_bytes).expect("parse first pending marker");
        fs::rename(
            staged_lease_path(&checkout_dir, &pending.receipt_id),
            checkout_dir.join(format!(".revoked-{}.json", pending.receipt_id)),
        )
        .expect("simulate durable first-token rollback tombstone");
        sync_directory(&checkout_dir).expect("make first-token tombstone durable");

        let second_token = "d".repeat(64);
        let second_token_digest = sha256_hex(second_token.as_bytes());
        let mut second_challenge = original_challenge;
        second_challenge.token_digest = second_token_digest.clone();
        write_json_atomic(
            &challenge_dir.join(format!("{second_token_digest}.json")),
            &second_challenge,
            false,
        )
        .expect("write second challenge");
        fs::write(checkout_root.join("dirty.txt"), b"stale second challenge\n")
            .expect("drift second-token snapshot");

        let error = adopt_dirty(
            &checkout_root,
            &state_root_path,
            &second_token,
            &reason_path,
        )
        .expect_err("stale second challenge must be rejected");

        assert_eq!(
            error
                .downcast_ref::<DirtyCheckoutError>()
                .expect("typed second-token drift")
                .kind(),
            DirtyCheckoutErrorKind::ChallengeDrift
        );
        assert_eq!(
            fs::read(&lease_path).expect("read recovered predecessor"),
            predecessor_bytes,
            "checkout-wide recovery must preserve the exact predecessor"
        );
        assert!(
            !first_pending_path.exists(),
            "first-token recovery must complete before second-token validation"
        );
    }

    #[test]
    fn adoption_reuses_one_deadline_across_snapshot_barriers() {
        let (checkout, state_root, reason_path, snapshot, _challenge_path) =
            adoption_transaction_fixture();
        let checkout_root =
            fs::canonicalize(checkout.path()).expect("canonicalize checkout fixture root");
        let state_root_path =
            fs::canonicalize(state_root.path()).expect("canonicalize state fixture root");
        let checkout_dir = state_root_path
            .join(&snapshot.repository_key)
            .join(&snapshot.checkout_key);
        let transaction_deadline = deadline_after(SNAPSHOT_TIMEOUT).expect("transaction deadline");
        let mut observed_deadlines = Vec::new();

        adopt_dirty_with_snapshot_clock_and_deadline(
            &checkout_root,
            &state_root_path,
            &"a".repeat(64),
            &reason_path,
            |_, barrier_deadline| {
                observed_deadlines.push(barrier_deadline);
                Ok(snapshot.clone())
            },
            unix_time,
            transaction_deadline,
        )
        .expect("stable snapshot barriers must complete before the deadline");

        assert_eq!(
            observed_deadlines.len(),
            4,
            "adoption must execute every snapshot barrier"
        );
        assert!(
            observed_deadlines
                .iter()
                .all(|deadline| *deadline == transaction_deadline),
            "every snapshot barrier must receive the original transaction deadline"
        );
        assert!(
            checkout_dir.join("lease.json").exists(),
            "deadline observation must complete a real adoption transition"
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
    fn persisted_adoption_requires_transition_before_challenge_expiry() {
        let identity = CheckoutIdentity {
            root: PathBuf::from("/checkout"),
            git_dir: PathBuf::from("/checkout/.git"),
            common_dir: PathBuf::from("/checkout/.git"),
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
            issued_at: 10,
            expires_at: 20,
        };
        let mut challenge_bytes = serde_json::to_vec(&challenge).expect("serialize challenge");
        challenge_bytes.push(b'\n');
        let challenge_digest = sha256_hex(&challenge_bytes);

        for (adopted_at, valid) in [(19, true), (20, false), (21, false)] {
            let lease = LeaseRecord::V2(Box::new(LeaseV2Wire {
                schema: LEASE_V2_SCHEMA.to_string(),
                session_key: challenge.session_key.clone(),
                checkout_instance: identity.checkout_instance.clone(),
                checkout_root: "/checkout".to_string(),
                checkout_git_dir: "/checkout/.git".to_string(),
                checkout_root_bytes: hex_bytes(identity.root.as_os_str().as_bytes()),
                checkout_git_dir_bytes: hex_bytes(identity.git_dir.as_os_str().as_bytes()),
                acquired_at: adopted_at,
                refreshed_at: adopted_at,
                expires_at: 100,
                adoption: AdoptionRecord {
                    schema: ADOPTION_SCHEMA.to_string(),
                    receipt_schema: RECEIPT_SCHEMA.to_string(),
                    receipt_id: "4".repeat(64),
                    snapshot_id: challenge.snapshot_id.clone(),
                    authorization_turn_digest: challenge.authorization_turn_digest.clone(),
                    reason_digest: "5".repeat(64),
                    adopted_at,
                    challenge_issued_at: challenge.issued_at,
                    challenge_digest: challenge_digest.clone(),
                },
            }));
            assert_eq!(
                validate_spent_challenge_matches_lease(
                    &challenge_bytes,
                    &challenge,
                    &identity,
                    &lease,
                )
                .is_ok(),
                valid,
                "unexpected persisted authorization result at {adopted_at}"
            );
        }
    }

    #[test]
    fn revocation_pruning_accepts_the_scan_cap_and_rejects_cap_plus_one() {
        fn fixture(entry_count: usize) -> (tempfile::TempDir, PathBuf, CheckoutIdentity) {
            let root = tempfile::TempDir::new().expect("revocation scan root");
            let checkout = root.path().join("checkout");
            let git_dir = root.path().join("git-dir");
            let checkout_dir = root.path().join("state");
            for directory in [&checkout, &git_dir, &checkout_dir] {
                fs::create_dir(directory).expect("create revocation scan directory");
                fs::set_permissions(directory, fs::Permissions::from_mode(0o700))
                    .expect("make revocation scan directory private");
            }
            for index in 0..entry_count {
                fs::write(checkout_dir.join(format!("inert-{index:04}")), b"")
                    .expect("write revocation scan entry");
            }
            let identity = CheckoutIdentity {
                root: checkout,
                git_dir: git_dir.clone(),
                common_dir: git_dir,
                repository_key: "a".repeat(64),
                checkout_key: "b".repeat(64),
                checkout_instance: "c".repeat(32),
            };
            (root, checkout_dir, identity)
        }

        let (_exact_root, exact_dir, exact_identity) = fixture(MAX_RECOVERY_DIRECTORY_ENTRIES);
        prune_revocation_tombstones(
            &exact_dir,
            &exact_identity,
            &"d".repeat(64),
            deadline_after(SNAPSHOT_TIMEOUT).expect("exact scan deadline"),
        )
        .expect("the exact revocation scan entry cap must remain supported");

        let deadline_error = prune_revocation_tombstones(
            &exact_dir,
            &exact_identity,
            &"d".repeat(64),
            Instant::now(),
        )
        .expect_err("an expired revocation scan deadline must fail closed");
        assert_eq!(
            deadline_error
                .downcast_ref::<DirtyCheckoutError>()
                .expect("typed revocation scan timeout")
                .kind(),
            DirtyCheckoutErrorKind::Timeout
        );

        let (_overflow_root, overflow_dir, overflow_identity) =
            fixture(MAX_RECOVERY_DIRECTORY_ENTRIES + 1);
        let error = prune_revocation_tombstones(
            &overflow_dir,
            &overflow_identity,
            &"d".repeat(64),
            deadline_after(SNAPSHOT_TIMEOUT).expect("overflow scan deadline"),
        )
        .expect_err("revocation scan entry cap plus one must fail closed");
        assert_eq!(
            error
                .downcast_ref::<DirtyCheckoutError>()
                .expect("typed revocation scan limit")
                .kind(),
            DirtyCheckoutErrorKind::ResourceUnavailable
        );
    }

    #[test]
    fn revocation_tombstones_are_pruned_oldest_first_with_current_protected() {
        let root = tempfile::TempDir::new().expect("tombstone retention root");
        let checkout = root.path().join("checkout");
        let git_dir = root.path().join("git-dir");
        let checkout_dir = root.path().join("state");
        for directory in [&checkout, &git_dir, &checkout_dir] {
            fs::create_dir(directory).expect("create tombstone retention directory");
            fs::set_permissions(directory, fs::Permissions::from_mode(0o700))
                .expect("make tombstone retention directory private");
        }
        let identity = CheckoutIdentity {
            root: checkout,
            git_dir: git_dir.clone(),
            common_dir: git_dir,
            repository_key: "a".repeat(64),
            checkout_key: "b".repeat(64),
            checkout_instance: "c".repeat(32),
        };
        let mut receipt_ids = Vec::new();
        for index in 0..=MAX_REVOCATION_TOMBSTONES {
            let receipt_id = format!("{index:064x}");
            let adopted_at = 100 + index as u64;
            let lease = LeaseRecord::V2(Box::new(LeaseV2Wire {
                schema: LEASE_V2_SCHEMA.to_string(),
                session_key: "d".repeat(64),
                checkout_instance: identity.checkout_instance.clone(),
                checkout_root: identity.root.to_string_lossy().into_owned(),
                checkout_git_dir: identity.git_dir.to_string_lossy().into_owned(),
                checkout_root_bytes: hex_bytes(identity.root.as_os_str().as_bytes()),
                checkout_git_dir_bytes: hex_bytes(identity.git_dir.as_os_str().as_bytes()),
                acquired_at: adopted_at,
                refreshed_at: adopted_at,
                expires_at: adopted_at + 100,
                adoption: AdoptionRecord {
                    schema: ADOPTION_SCHEMA.to_string(),
                    receipt_schema: RECEIPT_SCHEMA.to_string(),
                    receipt_id: receipt_id.clone(),
                    snapshot_id: "e".repeat(64),
                    authorization_turn_digest: "f".repeat(64),
                    reason_digest: "1".repeat(64),
                    adopted_at,
                    challenge_issued_at: adopted_at - 1,
                    challenge_digest: "2".repeat(64),
                },
            }));
            write_json_atomic(
                &checkout_dir.join(format!(".revoked-{receipt_id}.json")),
                &lease,
                false,
            )
            .expect("write tombstone fixture");
            receipt_ids.push(receipt_id);
        }
        let protected_token_digest = "9".repeat(64);
        let protected_pending = PendingAdoptionRecord {
            schema: PENDING_ADOPTION_SCHEMA.to_string(),
            receipt_id: receipt_ids[0].clone(),
            token_digest: protected_token_digest.clone(),
            challenge_digest: "2".repeat(64),
            session_key: "d".repeat(64),
            checkout_instance: identity.checkout_instance.clone(),
            snapshot_id: "e".repeat(64),
            predecessor_receipt_id: None,
            predecessor_receipt_digest: None,
            predecessor_spent_challenge_digest: None,
            predecessor_lease_digest: None,
            predecessor_lease_bytes: None,
        };
        write_json_atomic(
            &pending_adoption_path(&checkout_dir, &protected_token_digest),
            &protected_pending,
            false,
        )
        .expect("write pending tombstone reference");
        let current = receipt_ids.last().expect("current tombstone");

        prune_revocation_tombstones(
            &checkout_dir,
            &identity,
            current,
            deadline_after(SNAPSHOT_TIMEOUT).expect("pruning deadline"),
        )
        .expect("prune tombstone retention set");

        let remaining = fs::read_dir(&checkout_dir)
            .expect("list retained tombstones")
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_name().as_bytes().starts_with(b".revoked-"))
            .count();
        assert_eq!(remaining, MAX_REVOCATION_TOMBSTONES);
        assert!(
            checkout_dir
                .join(format!(".revoked-{}.json", receipt_ids[0]))
                .exists(),
            "pending-referenced tombstone must remain durable"
        );
        assert!(
            !checkout_dir
                .join(format!(".revoked-{}.json", receipt_ids[1]))
                .exists(),
            "oldest unreferenced tombstone should be pruned first"
        );
        assert!(
            checkout_dir
                .join(format!(".revoked-{current}.json"))
                .exists()
        );
    }

    #[test]
    fn failed_checkout_recovery_protects_every_prescanned_pending_tombstone() {
        let root = tempfile::TempDir::new().expect("pending tombstone recovery root");
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
            fs::create_dir(directory).expect("create pending tombstone recovery directory");
            fs::set_permissions(directory, fs::Permissions::from_mode(0o700))
                .expect("make pending tombstone recovery directory private");
        }
        let identity = CheckoutIdentity {
            root: checkout,
            git_dir: git_dir.clone(),
            common_dir: git_dir,
            repository_key: "a".repeat(64),
            checkout_key: "b".repeat(64),
            checkout_instance: "c".repeat(32),
        };
        let mut receipt_ids = Vec::new();
        for index in 0..=MAX_REVOCATION_TOMBSTONES {
            let receipt_id = format!("{index:064x}");
            let adopted_at = 100 + index as u64;
            let lease = LeaseRecord::V2(Box::new(LeaseV2Wire {
                schema: LEASE_V2_SCHEMA.to_string(),
                session_key: "d".repeat(64),
                checkout_instance: identity.checkout_instance.clone(),
                checkout_root: identity.root.to_string_lossy().into_owned(),
                checkout_git_dir: identity.git_dir.to_string_lossy().into_owned(),
                checkout_root_bytes: hex_bytes(identity.root.as_os_str().as_bytes()),
                checkout_git_dir_bytes: hex_bytes(identity.git_dir.as_os_str().as_bytes()),
                acquired_at: adopted_at,
                refreshed_at: adopted_at,
                expires_at: adopted_at + 100,
                adoption: AdoptionRecord {
                    schema: ADOPTION_SCHEMA.to_string(),
                    receipt_schema: RECEIPT_SCHEMA.to_string(),
                    receipt_id: receipt_id.clone(),
                    snapshot_id: "e".repeat(64),
                    authorization_turn_digest: "f".repeat(64),
                    reason_digest: "1".repeat(64),
                    adopted_at,
                    challenge_issued_at: adopted_at - 1,
                    challenge_digest: "2".repeat(64),
                },
            }));
            write_json_atomic(
                &checkout_dir.join(format!(".revoked-{receipt_id}.json")),
                &lease,
                false,
            )
            .expect("write pending tombstone recovery fixture");
            receipt_ids.push(receipt_id);
        }
        for (token_digest, receipt_id) in [
            ("8".repeat(64), &receipt_ids[0]),
            ("9".repeat(64), &receipt_ids[1]),
        ] {
            let pending = PendingAdoptionRecord {
                schema: PENDING_ADOPTION_SCHEMA.to_string(),
                receipt_id: receipt_id.clone(),
                token_digest: token_digest.clone(),
                challenge_digest: "2".repeat(64),
                session_key: "7".repeat(64),
                checkout_instance: identity.checkout_instance.clone(),
                snapshot_id: "e".repeat(64),
                predecessor_receipt_id: None,
                predecessor_receipt_digest: None,
                predecessor_spent_challenge_digest: None,
                predecessor_lease_digest: None,
                predecessor_lease_bytes: None,
            };
            write_json_atomic(
                &pending_adoption_path(&checkout_dir, &token_digest),
                &pending,
                false,
            )
            .expect("write pending tombstone reference");
        }

        let error = recover_checkout_pending_adoptions(
            &checkout_dir,
            &challenge_dir,
            &identity,
            &"6".repeat(64),
            deadline_after(SNAPSHOT_TIMEOUT).expect("pending tombstone recovery deadline"),
        )
        .expect_err("one mismatched pending recovery must remain primary");

        assert_eq!(
            error
                .downcast_ref::<DirtyCheckoutError>()
                .expect("typed pending tombstone recovery failure")
                .kind(),
            DirtyCheckoutErrorKind::MalformedState
        );
        for receipt_id in &receipt_ids[..2] {
            assert!(
                checkout_dir
                    .join(format!(".revoked-{receipt_id}.json"))
                    .exists(),
                "every prescanned pending marker must protect its tombstone"
            );
        }
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
            receipt_digest: Some("b".repeat(64)),
            spent_challenge_digest: Some("c".repeat(64)),
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
        let challenge_artifact_digest = {
            let mut bytes = serde_json::to_vec(&challenge).expect("serialize recovery challenge");
            bytes.push(b'\n');
            sha256_hex(&bytes)
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
            challenge_digest: challenge_artifact_digest.clone(),
            adopted_at: 15,
        };
        let pending_path = pending_adoption_path(&checkout_dir, &challenge_digest);
        let challenge_path = challenge_dir.join(format!("{challenge_digest}.json"));
        let receipt_path = receipts_dir.join(format!("{receipt_id}.json"));
        let spent_path = receipts_dir.join(format!(".challenge-{receipt_id}.json"));
        let pending = PendingAdoptionRecord {
            schema: PENDING_ADOPTION_SCHEMA.to_string(),
            receipt_id: receipt_id.clone(),
            token_digest: challenge_digest.clone(),
            challenge_digest: challenge_artifact_digest.clone(),
            session_key: challenge.session_key.clone(),
            checkout_instance: identity.checkout_instance.clone(),
            snapshot_id: challenge.snapshot_id.clone(),
            predecessor_receipt_id: None,
            predecessor_receipt_digest: None,
            predecessor_spent_challenge_digest: None,
            predecessor_lease_digest: None,
            predecessor_lease_bytes: None,
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
                challenge_digest: challenge_artifact_digest.clone(),
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

        write_json_atomic(&pending_path, &pending, false)
            .expect("write missing-receipt recovery marker");
        fs::remove_file(&receipt_path).expect("remove current committed receipt");
        let missing_receipt =
            recover_pending_adoption(&checkout_dir, &challenge_dir, &identity, &challenge_digest)
                .expect_err("committed recovery must require the current receipt");
        assert_eq!(
            missing_receipt
                .downcast_ref::<DirtyCheckoutError>()
                .expect("typed missing receipt error")
                .kind(),
            DirtyCheckoutErrorKind::MalformedState
        );
        assert!(pending_path.exists());

        write_json_atomic(&receipt_path, &receipt, false).expect("restore current receipt");
        fs::remove_file(&spent_path).expect("remove current spent challenge");
        let missing_spent =
            recover_pending_adoption(&checkout_dir, &challenge_dir, &identity, &challenge_digest)
                .expect_err("committed recovery must require the exact spent challenge");
        assert_eq!(
            missing_spent
                .downcast_ref::<DirtyCheckoutError>()
                .expect("typed missing spent challenge error")
                .kind(),
            DirtyCheckoutErrorKind::MalformedState
        );
        assert!(pending_path.exists());
        write_json_atomic(&spent_path, &challenge, false).expect("restore spent challenge");
        assert_eq!(
            recover_pending_adoption(&checkout_dir, &challenge_dir, &identity, &challenge_digest)
                .expect("finish restored committed transition"),
            PendingRecovery::Committed
        );

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

        let predecessor_receipt_bytes = b"bound predecessor receipt";
        let predecessor_spent_bytes = b"bound predecessor challenge";
        let bound_pending = PendingAdoptionRecord {
            predecessor_receipt_digest: Some(sha256_hex(predecessor_receipt_bytes)),
            predecessor_spent_challenge_digest: Some(sha256_hex(predecessor_spent_bytes)),
            ..committed_pending.clone()
        };
        for missing_path in [&predecessor_receipt_path, &predecessor_spent_path] {
            fs::write(&predecessor_receipt_path, predecessor_receipt_bytes)
                .expect("write bound predecessor receipt");
            fs::write(&predecessor_spent_path, predecessor_spent_bytes)
                .expect("write bound predecessor challenge");
            for path in [&predecessor_receipt_path, &predecessor_spent_path] {
                fs::set_permissions(path, fs::Permissions::from_mode(0o600))
                    .expect("make bound predecessor artifact private");
            }
            write_json_atomic(&pending_path, &bound_pending, false)
                .expect("write bound predecessor marker");
            fs::remove_file(missing_path).expect("simulate partial predecessor cleanup");

            assert_eq!(
                recover_pending_adoption(
                    &checkout_dir,
                    &challenge_dir,
                    &identity,
                    &challenge_digest,
                )
                .expect("resume partial predecessor cleanup"),
                PendingRecovery::Committed
            );
            assert!(!pending_path.exists());
            assert!(!predecessor_receipt_path.exists());
            assert!(!predecessor_spent_path.exists());
        }

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
            LeaseInstallOutcome::Installed
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
        let challenge_artifact_digest = sha256_hex(challenge_fixture.as_bytes());

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
            challenge_digest: challenge_artifact_digest.clone(),
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
            challenge_digest: challenge_artifact_digest.clone(),
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
        assert_eq!(receipt.challenge_digest, challenge_artifact_digest);
        assert_ne!(receipt.challenge_digest, challenge.token_digest);
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
