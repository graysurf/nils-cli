use super::{CliError, detect_format, emit_error, emit_success, take_format};
use anyhow::{Context, Result, bail, ensure};
use nils_common::cli_contract::OutputFormat;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::env;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const SNAPSHOT_SCHEMA: &str = "agent-runtime.dirty-checkout-snapshot.v1";
const CHALLENGE_SCHEMA: &str = "agent-runtime.dirty-checkout-challenge.v1";
const RECEIPT_SCHEMA: &str = "agent-runtime.dirty-checkout-receipt.v1";
const LEASE_V1_SCHEMA: &str = "agent-runtime.checkout-lease.v1";
const LEASE_V2_SCHEMA: &str = "agent-runtime.checkout-lease.v2";
const ADOPTION_SCHEMA: &str = "agent-runtime.dirty-checkout-adoption.v1";
const INSTANCE_FILE: &str = ".agent-runtime-checkout-instance";
const MAX_GIT_OUTPUT_BYTES: usize = 64 * 1024 * 1024;
const MAX_GIT_STDERR_BYTES: usize = 1024 * 1024;
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

#[derive(Debug)]
struct CheckoutIdentity {
    root: PathBuf,
    git_dir: PathBuf,
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

#[derive(Debug, Deserialize)]
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

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AdoptionRecord {
    schema: String,
    receipt_id: String,
    snapshot_id: String,
    authorization_turn_digest: String,
    reason_digest: String,
    adopted_at: u64,
    challenge_issued_at: u64,
    challenge_digest: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct LeaseRecord {
    schema: String,
    session_key: String,
    checkout_instance: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    checkout_root: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    checkout_git_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    checkout_root_bytes: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    checkout_git_dir_bytes: Option<String>,
    acquired_at: u64,
    refreshed_at: u64,
    expires_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    adoption: Option<AdoptionRecord>,
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

#[derive(Debug)]
struct SnapshotPass {
    snapshot: DirtySnapshot,
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
    let deadline = Instant::now() + SNAPSHOT_TIMEOUT;
    let first = snapshot_once(checkout, deadline)?;
    let second = snapshot_once(checkout, deadline)?;
    ensure!(
        first.snapshot == second.snapshot,
        "checkout changed while the dirty snapshot was calculated"
    );
    Ok(first.snapshot)
}

pub fn adopt_dirty(
    checkout: &Path,
    state_root: &Path,
    challenge_token: &str,
    reason_file: &Path,
) -> Result<AdoptionReceipt> {
    ensure_lower_hex(challenge_token, 64, "challenge token")?;
    let challenge_digest = sha256_hex(challenge_token.as_bytes());
    let identity = resolve_checkout(checkout, true, Instant::now() + SNAPSHOT_TIMEOUT)?;
    let checkout_dir = checkout_state_dir(state_root, &identity, true)?;
    let _lock = LeaseLock::acquire(&checkout_dir)?;

    let challenge_dir = checkout_dir.join("challenges");
    verify_private_directory(&challenge_dir)?;
    let challenge_path = challenge_dir.join(format!("{challenge_digest}.json"));
    let challenge_bytes = read_private_regular(&challenge_path, MAX_STATE_FILE_BYTES, true)?;
    let challenge: ChallengeRecord = serde_json::from_slice(&challenge_bytes)
        .context("dirty-checkout challenge state is malformed")?;
    validate_challenge(&challenge, &identity, &challenge_digest)?;

    let reason = read_regular_bounded(reason_file, MAX_REASON_BYTES)?;
    ensure!(
        !reason.iter().all(u8::is_ascii_whitespace),
        "adoption reason file is empty"
    );
    let reason_digest = sha256_hex(&reason);
    let now = unix_time()?;

    let lease_path = checkout_dir.join("lease.json");
    let existing = load_lease(&lease_path)?;
    if let Some(lease) = &existing {
        validate_lease(lease)?;
        if lease.checkout_instance == identity.checkout_instance
            && lease.session_key != challenge.session_key
            && lease.expires_at > now
        {
            bail!("another session owns the active checkout lease");
        }
        if lease.checkout_instance == identity.checkout_instance
            && lease.session_key == challenge.session_key
            && let Some(adoption) = &lease.adoption
        {
            if adoption.challenge_digest == challenge_digest {
                bail!("dirty-checkout challenge has already been consumed");
            }
            bail!("checkout already has a current-session dirty adoption");
        }
    }

    let snapshot = dirty_snapshot(&identity.root)?;
    ensure!(
        snapshot.repository_key == challenge.repository_key
            && snapshot.checkout_key == challenge.checkout_key
            && snapshot.checkout_instance == challenge.checkout_instance
            && snapshot.snapshot_id == challenge.snapshot_id
            && snapshot.head_oid == challenge.head_oid
            && snapshot.branch_ref_digest == challenge.branch_ref_digest,
        "dirty-checkout challenge no longer matches the current checkout snapshot"
    );

    let receipt_id = random_hex(32)?;
    let receipt_record = ReceiptRecord {
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
        adopted_at: now,
    };
    let receipts_dir = checkout_dir.join("receipts");
    private_directory(&receipts_dir)?;
    let receipt_path = receipts_dir.join(format!("{receipt_id}.json"));
    write_json_atomic(&receipt_path, &receipt_record, false)?;
    let spent_challenge_path = receipts_dir.join(format!(".challenge-{receipt_id}.json"));
    if let Err(error) = fs::rename(&challenge_path, &spent_challenge_path) {
        let _ = fs::remove_file(&receipt_path);
        return Err(error).context("failed to consume dirty-checkout challenge");
    }
    if let Err(error) = sync_directory(&challenge_dir).and_then(|()| sync_directory(&receipts_dir))
    {
        let _ = fs::rename(&spent_challenge_path, &challenge_path);
        let _ = fs::remove_file(&receipt_path);
        let _ = sync_directory(&challenge_dir);
        let _ = sync_directory(&receipts_dir);
        return Err(error);
    }

    let acquired_at = existing
        .as_ref()
        .filter(|lease| {
            lease.checkout_instance == identity.checkout_instance
                && lease.session_key == challenge.session_key
        })
        .map_or(now, |lease| lease.acquired_at);
    let lease = LeaseRecord {
        schema: LEASE_V2_SCHEMA.to_string(),
        session_key: challenge.session_key,
        checkout_instance: identity.checkout_instance.clone(),
        checkout_root: identity.root.to_str().map(str::to_string),
        checkout_git_dir: identity.git_dir.to_str().map(str::to_string),
        checkout_root_bytes: Some(hex_bytes(identity.root.as_os_str().as_bytes())),
        checkout_git_dir_bytes: Some(hex_bytes(identity.git_dir.as_os_str().as_bytes())),
        acquired_at,
        refreshed_at: now,
        expires_at: now.saturating_add(lease_ttl_seconds()),
        adoption: Some(AdoptionRecord {
            schema: ADOPTION_SCHEMA.to_string(),
            receipt_id: receipt_id.clone(),
            snapshot_id: snapshot.snapshot_id.clone(),
            authorization_turn_digest: receipt_record.authorization_turn_digest.clone(),
            reason_digest,
            adopted_at: now,
            challenge_issued_at: challenge.issued_at,
            challenge_digest,
        }),
    };
    if let Err(error) = write_json_atomic(&lease_path, &lease, true) {
        let committed = load_lease(&lease_path)
            .ok()
            .flatten()
            .is_some_and(|actual| {
                actual.schema == LEASE_V2_SCHEMA
                    && actual.session_key == lease.session_key
                    && actual.checkout_instance == lease.checkout_instance
                    && actual.adoption.as_ref().is_some_and(|adoption| {
                        adoption.receipt_id == receipt_id
                            && adoption.snapshot_id == snapshot.snapshot_id
                    })
            });
        if !committed {
            let _ = fs::rename(&spent_challenge_path, &challenge_path);
            let _ = fs::remove_file(&receipt_path);
            let _ = sync_directory(&challenge_dir);
            let _ = sync_directory(&receipts_dir);
            return Err(error);
        }
    }

    Ok(AdoptionReceipt {
        receipt_id,
        snapshot_id: snapshot.snapshot_id,
    })
}

pub fn revoke_dirty(checkout: &Path, state_root: &Path, receipt_id: &str) -> Result<()> {
    ensure_lower_hex(receipt_id, 64, "receipt ID")?;
    let identity = resolve_checkout(checkout, false, Instant::now() + GIT_TIMEOUT)?;
    let checkout_dir = checkout_state_dir(state_root, &identity, false)?;
    let _lock = LeaseLock::acquire(&checkout_dir)?;
    let receipts_dir = checkout_dir.join("receipts");
    verify_private_directory(&receipts_dir)?;
    let receipt_path = receipts_dir.join(format!("{receipt_id}.json"));
    let receipt_bytes = read_private_regular(&receipt_path, MAX_STATE_FILE_BYTES, true)?;
    let receipt: ReceiptRecord = serde_json::from_slice(&receipt_bytes)
        .context("dirty-checkout receipt state is malformed")?;
    validate_receipt(&receipt, &identity, receipt_id)?;

    let lease_path = checkout_dir.join("lease.json");
    let lease = load_lease(&lease_path)?.context("dirty-checkout lease is missing")?;
    validate_lease(&lease)?;
    let adoption = lease
        .adoption
        .as_ref()
        .context("checkout lease has no dirty adoption")?;
    ensure!(
        lease.schema == LEASE_V2_SCHEMA
            && lease.session_key == receipt.session_key
            && lease.checkout_instance == receipt.checkout_instance
            && adoption.schema == ADOPTION_SCHEMA
            && adoption.receipt_id == receipt.receipt_id
            && adoption.snapshot_id == receipt.snapshot_id
            && adoption.authorization_turn_digest == receipt.authorization_turn_digest
            && adoption.reason_digest == receipt.reason_digest
            && adoption.adopted_at == receipt.adopted_at
            && adoption.challenge_digest == receipt.challenge_digest,
        "receipt does not match the active dirty-checkout adoption"
    );

    let revoked_lease_path = checkout_dir.join(format!(".revoked-{receipt_id}.json"));
    fs::rename(&lease_path, &revoked_lease_path)
        .context("failed to revoke adopted checkout lease")?;
    if let Err(error) = sync_directory(&checkout_dir) {
        let committed = !lease_path.exists() && revoked_lease_path.exists();
        if !committed {
            return Err(error);
        }
    }

    let challenge_path = checkout_dir
        .join("challenges")
        .join(format!("{}.json", receipt.challenge_digest));
    let spent_challenge_path = receipts_dir.join(format!(".challenge-{receipt_id}.json"));
    for path in [
        &receipt_path,
        &challenge_path,
        &spent_challenge_path,
        &revoked_lease_path,
    ] {
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(_) => {
                // The lease rename is the revocation commit point. Private,
                // receipt-bound metadata may remain for later cleanup, but it
                // cannot restore or authorize an adoption.
            }
        }
    }
    let _ = sync_directory(&checkout_dir);
    let _ = sync_directory(&receipts_dir);
    if let Some(challenge_dir) = challenge_path.parent() {
        let _ = sync_directory(challenge_dir);
    }
    Ok(())
}

fn snapshot_once(checkout: &Path, deadline: Instant) -> Result<SnapshotPass> {
    ensure_deadline(deadline)?;
    let identity = resolve_checkout(checkout, true, deadline)?;
    reject_active_git_operation(&identity)?;

    let status = git_output(
        &identity.root,
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
    ensure!(status.status.success(), "Git dirty-state probe failed");
    ensure!(!status.stdout.is_empty(), "checkout is clean");

    let head_output = git_output(
        &identity.root,
        &["rev-parse", "--verify", "HEAD"],
        256,
        deadline,
    )?;
    ensure!(
        head_output.status.success(),
        "Git HEAD identity is unavailable"
    );
    let head_raw = strip_git_line(&head_output.stdout)?;
    ensure!(
        (40..=64).contains(&head_raw.len()) && head_raw.iter().all(u8::is_ascii_hexdigit),
        "Git HEAD identity is malformed"
    );
    let head_oid =
        String::from_utf8(head_raw.to_vec()).context("Git HEAD identity is malformed")?;

    let branch_output = git_output(
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
        bail!("Git branch identity is unavailable");
    };

    let index_output = git_output(
        &identity.root,
        &["ls-files", "--stage", "-z"],
        MAX_GIT_OUTPUT_BYTES,
        deadline,
    )?;
    ensure!(index_output.status.success(), "Git index listing failed");
    let mut tracked = parse_index(&index_output.stdout)?;
    let unstaged_output = git_output(
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
    ensure!(
        unstaged_output.status.success(),
        "Git unstaged-path listing failed"
    );
    let mut unstaged = parse_nul_paths(&unstaged_output.stdout)?;
    let untracked_output = git_output(
        &identity.root,
        &["ls-files", "--others", "--exclude-standard", "-z"],
        MAX_GIT_OUTPUT_BYTES,
        deadline,
    )?;
    ensure!(
        untracked_output.status.success(),
        "Git untracked listing failed"
    );
    let mut untracked = parse_nul_paths(&untracked_output.stdout)?;
    ensure!(
        tracked.len().saturating_add(untracked.len()) <= MAX_ENTRY_COUNT,
        "dirty snapshot entry count exceeds the supported limit"
    );
    tracked.sort_by(|left, right| left.path.cmp(&right.path));
    unstaged.sort();
    untracked.sort();
    ensure_unique_paths(tracked.iter().map(|entry| entry.path.as_slice()))?;
    ensure_unique_paths(unstaged.iter().map(Vec::as_slice))?;
    ensure_unique_paths(untracked.iter().map(Vec::as_slice))?;
    let submodule_paths: HashSet<Vec<u8>> = tracked
        .iter()
        .filter(|entry| entry.mode.as_slice() == b"160000")
        .map(|entry| entry.path.clone())
        .collect();
    reject_special_filesystem_objects(&identity.root, &submodule_paths, deadline)?;
    let tracked_paths: HashSet<Vec<u8>> = tracked.iter().map(|entry| entry.path.clone()).collect();
    ensure!(
        unstaged.iter().all(|path| tracked_paths.contains(path)),
        "Git unstaged-path listing does not match the index"
    );
    let unstaged_count = unstaged.len();
    let unstaged_paths: HashSet<Vec<u8>> = unstaged.into_iter().collect();

    let mut hasher = FramedHasher::new();
    hasher.field(b"schema", SNAPSHOT_SCHEMA.as_bytes());
    hasher.field(b"repository_key", identity.repository_key.as_bytes());
    hasher.field(b"checkout_key", identity.checkout_key.as_bytes());
    hasher.field(b"checkout_instance", identity.checkout_instance.as_bytes());
    hasher.field(b"head_oid", head_oid.as_bytes());
    hasher.field(b"branch_ref", &branch_raw);
    let mut hashed_bytes = 0_u64;

    for entry in &tracked {
        ensure_deadline(deadline)?;
        hasher.field(b"index_mode", &entry.mode);
        hasher.field(b"index_oid", &entry.oid);
        hasher.field(b"index_path", &entry.path);
        if unstaged_paths.contains(&entry.path) {
            hasher.field(b"unstaged_path", &entry.path);
            hash_worktree_object(
                &mut hasher,
                &identity.root,
                &entry.path,
                entry.mode.as_slice() == b"160000",
                Some(&entry.oid),
                &mut hashed_bytes,
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
            &mut hashed_bytes,
            deadline,
        )?;
    }
    hasher.field(b"tracked_count", &(tracked.len() as u64).to_be_bytes());
    hasher.field(b"unstaged_count", &(unstaged_count as u64).to_be_bytes());
    hasher.field(b"untracked_count", &(untracked.len() as u64).to_be_bytes());
    hasher.field(b"hashed_bytes", &hashed_bytes.to_be_bytes());

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
            hashed_bytes,
        },
    })
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
        Ok(_) => bail!("checkout instance sentinel is empty"),
        Err(error)
            if error
                .downcast_ref::<io::Error>()
                .is_some_and(|io| io.kind() == io::ErrorKind::NotFound) => {}
        Err(error) => return Err(error),
    }
    ensure!(create, "checkout instance sentinel is missing");
    let value = random_hex(16)?;
    atomic_create_once(&path, format!("{value}\n").as_bytes())?;
    let raw = read_private_regular(&path, 128, true)?;
    parse_instance(&raw)
}

fn parse_instance(raw: &[u8]) -> Result<String> {
    let value = std::str::from_utf8(raw)
        .context("checkout instance sentinel is malformed")?
        .trim();
    ensure_lower_hex(value, 32, "checkout instance sentinel")?;
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
            Ok(_) => bail!("checkout has an active or ambiguous Git operation"),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("Git operation state could not be verified"),
        }
    }
    Ok(())
}

fn parse_index(raw: &[u8]) -> Result<Vec<IndexEntry>> {
    let mut entries = Vec::new();
    for record in split_nul_records(raw)? {
        let tab = record
            .iter()
            .position(|byte| *byte == b'\t')
            .context("Git index output is malformed")?;
        let metadata = &record[..tab];
        let path = &record[tab + 1..];
        let fields: Vec<&[u8]> = metadata.split(|byte| *byte == b' ').collect();
        ensure!(fields.len() == 3, "Git index output is malformed");
        let mode = fields[0];
        let oid = fields[1];
        let stage = fields[2];
        ensure!(
            mode.len() == 6 && mode.iter().all(u8::is_ascii_digit),
            "Git index mode is malformed"
        );
        ensure!(
            (40..=64).contains(&oid.len()) && oid.iter().all(u8::is_ascii_hexdigit),
            "Git index object identity is malformed"
        );
        ensure!(stage == b"0", "unmerged index stages are unsupported");
        validate_repo_relative_path(path)?;
        entries.push(IndexEntry {
            mode: mode.to_vec(),
            oid: oid.to_vec(),
            path: path.to_vec(),
        });
    }
    Ok(entries)
}

fn parse_nul_paths(raw: &[u8]) -> Result<Vec<Vec<u8>>> {
    split_nul_records(raw)?
        .into_iter()
        .map(|path| {
            validate_repo_relative_path(path)?;
            Ok(path.to_vec())
        })
        .collect()
}

fn split_nul_records(raw: &[u8]) -> Result<Vec<&[u8]>> {
    if raw.is_empty() {
        return Ok(Vec::new());
    }
    ensure!(
        raw.last() == Some(&0),
        "NUL-delimited Git output is malformed"
    );
    let records: Vec<&[u8]> = raw[..raw.len() - 1].split(|byte| *byte == 0).collect();
    ensure!(
        records.iter().all(|record| !record.is_empty()),
        "NUL-delimited Git output is malformed"
    );
    Ok(records)
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
    submodule_paths: &HashSet<Vec<u8>>,
    deadline: Instant,
) -> Result<()> {
    let mut directories = vec![root.to_path_buf()];
    let mut entry_count = 0_usize;
    while let Some(directory) = directories.pop() {
        ensure_deadline(deadline)?;
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
            if directory == root && entry.file_name().as_bytes() == b".git" {
                continue;
            }
            entry_count = entry_count.saturating_add(1);
            ensure!(
                entry_count <= MAX_ENTRY_COUNT,
                "checkout filesystem entry count exceeds the supported limit"
            );
            let path = entry.path();
            let relative = path
                .strip_prefix(root)
                .context("checkout directory entry escapes the checkout")?;
            let relative_bytes = relative.as_os_str().as_bytes();
            if submodule_paths.contains(relative_bytes) {
                continue;
            }
            let file_type = entry
                .file_type()
                .context("checkout directory entry type is unavailable")?;
            if file_type.is_dir() {
                directories.push(path);
            } else {
                ensure!(
                    file_type.is_file() || file_type.is_symlink(),
                    "special filesystem objects are unsupported"
                );
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
    total_bytes: &mut u64,
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
        let head = git_output(
            &canonical,
            &["rev-parse", "--verify", "HEAD"],
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
        let status = git_output(
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
    ensure!(
        before.len() <= MAX_FILE_BYTES,
        "file exceeds the dirty snapshot size limit"
    );
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
    *total_bytes = total_bytes
        .checked_add(before.len())
        .context("dirty snapshot byte count overflowed")?;
    ensure!(
        *total_bytes <= MAX_TOTAL_BYTES,
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
}

fn validate_challenge(
    record: &ChallengeRecord,
    identity: &CheckoutIdentity,
    digest: &str,
) -> Result<()> {
    ensure!(
        record.schema == CHALLENGE_SCHEMA,
        "dirty-checkout challenge schema is unsupported"
    );
    ensure_lower_hex(&record.token_digest, 64, "challenge digest")?;
    ensure_lower_hex(&record.session_key, 64, "challenge session identity")?;
    ensure_lower_hex(&record.repository_key, 64, "challenge repository identity")?;
    ensure_lower_hex(&record.checkout_key, 64, "challenge checkout identity")?;
    ensure_lower_hex(&record.checkout_instance, 32, "challenge checkout instance")?;
    ensure_lower_hex(&record.snapshot_id, 64, "challenge snapshot identity")?;
    ensure_lower_hex_range(&record.head_oid, 40, 64, "challenge HEAD identity")?;
    ensure_lower_hex(&record.branch_ref_digest, 64, "challenge branch identity")?;
    ensure_lower_hex(
        &record.authorization_turn_digest,
        64,
        "challenge authorization identity",
    )?;
    ensure!(
        record.token_digest == digest,
        "dirty-checkout challenge token does not match"
    );
    ensure!(
        record.repository_key == identity.repository_key
            && record.checkout_key == identity.checkout_key
            && record.checkout_instance == identity.checkout_instance,
        "dirty-checkout challenge belongs to another checkout instance"
    );
    let now = unix_time()?;
    ensure!(
        record.issued_at <= now && now < record.expires_at,
        "dirty-checkout challenge is expired or not yet valid"
    );
    ensure!(
        record.expires_at > record.issued_at
            && record.expires_at.saturating_sub(record.issued_at) <= 300,
        "dirty-checkout challenge lifetime is invalid"
    );
    Ok(())
}

fn validate_receipt(
    record: &ReceiptRecord,
    identity: &CheckoutIdentity,
    receipt_id: &str,
) -> Result<()> {
    ensure!(
        record.schema == RECEIPT_SCHEMA,
        "dirty-checkout receipt schema is unsupported"
    );
    for (value, length, label) in [
        (record.receipt_id.as_str(), 64, "receipt ID"),
        (record.session_key.as_str(), 64, "receipt session identity"),
        (
            record.repository_key.as_str(),
            64,
            "receipt repository identity",
        ),
        (
            record.checkout_key.as_str(),
            64,
            "receipt checkout identity",
        ),
        (
            record.checkout_instance.as_str(),
            32,
            "receipt checkout instance",
        ),
        (record.snapshot_id.as_str(), 64, "receipt snapshot identity"),
        (
            record.authorization_turn_digest.as_str(),
            64,
            "receipt authorization identity",
        ),
        (record.reason_digest.as_str(), 64, "receipt reason digest"),
        (
            record.challenge_digest.as_str(),
            64,
            "receipt challenge digest",
        ),
    ] {
        ensure_lower_hex(value, length, label)?;
    }
    ensure!(
        record.receipt_id == receipt_id
            && record.repository_key == identity.repository_key
            && record.checkout_key == identity.checkout_key
            && record.checkout_instance == identity.checkout_instance,
        "receipt belongs to another checkout instance"
    );
    Ok(())
}

fn load_lease(path: &Path) -> Result<Option<LeaseRecord>> {
    let bytes = match read_private_regular(path, MAX_STATE_FILE_BYTES, true) {
        Ok(bytes) if bytes.is_empty() => return Ok(None),
        Ok(bytes) => bytes,
        Err(error)
            if error
                .downcast_ref::<io::Error>()
                .is_some_and(|io| io.kind() == io::ErrorKind::NotFound) =>
        {
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    let lease: LeaseRecord =
        serde_json::from_slice(&bytes).context("checkout lease state is malformed")?;
    Ok(Some(lease))
}

fn validate_lease(lease: &LeaseRecord) -> Result<()> {
    ensure!(
        lease.schema == LEASE_V1_SCHEMA || lease.schema == LEASE_V2_SCHEMA,
        "checkout lease schema is unsupported"
    );
    ensure_lower_hex(&lease.session_key, 64, "checkout lease session identity")?;
    ensure_lower_hex(
        &lease.checkout_instance,
        32,
        "checkout lease instance identity",
    )?;
    ensure!(
        lease.acquired_at <= lease.refreshed_at && lease.refreshed_at <= lease.expires_at,
        "checkout lease timestamps are malformed"
    );
    for (value, label) in [
        (lease.checkout_root.as_deref(), "checkout lease root"),
        (
            lease.checkout_git_dir.as_deref(),
            "checkout lease Git directory",
        ),
    ] {
        if let Some(value) = value {
            ensure!(
                !value.is_empty() && Path::new(value).is_absolute(),
                "{label} is malformed"
            );
        }
    }
    if lease.schema == LEASE_V1_SCHEMA {
        ensure!(
            lease.adoption.is_none()
                && lease.checkout_root_bytes.is_none()
                && lease.checkout_git_dir_bytes.is_none(),
            "v1 checkout lease contains v2 state"
        );
    } else {
        for (value, label) in [
            (
                lease.checkout_root_bytes.as_deref(),
                "v2 checkout root identity",
            ),
            (
                lease.checkout_git_dir_bytes.as_deref(),
                "v2 checkout Git-dir identity",
            ),
        ] {
            ensure!(
                value.is_some_and(|value| {
                    !value.is_empty() && value.len() % 2 == 0 && is_lower_hex(value, value.len())
                }),
                "{label} is malformed"
            );
        }
        let adoption = lease
            .adoption
            .as_ref()
            .context("v2 checkout lease has no dirty adoption")?;
        ensure!(
            adoption.schema == ADOPTION_SCHEMA,
            "dirty-checkout adoption schema is unsupported"
        );
        for (value, label) in [
            (adoption.receipt_id.as_str(), "adoption receipt ID"),
            (adoption.snapshot_id.as_str(), "adoption snapshot identity"),
            (
                adoption.authorization_turn_digest.as_str(),
                "adoption authorization identity",
            ),
            (adoption.reason_digest.as_str(), "adoption reason digest"),
            (
                adoption.challenge_digest.as_str(),
                "adoption challenge digest",
            ),
        ] {
            ensure_lower_hex(value, 64, label)?;
        }
        ensure!(
            adoption.challenge_issued_at <= adoption.adopted_at
                && adoption.adopted_at <= lease.refreshed_at,
            "dirty-checkout adoption timestamps are malformed"
        );
    }
    Ok(())
}

fn checkout_state_dir(
    state_root: &Path,
    identity: &CheckoutIdentity,
    create: bool,
) -> Result<PathBuf> {
    let directory = state_root
        .join(&identity.repository_key)
        .join(&identity.checkout_key);
    if create {
        private_directory(state_root)?;
        private_directory(&state_root.join(&identity.repository_key))?;
        private_directory(&directory)?;
    } else {
        verify_private_directory(&directory)?;
    }
    Ok(directory)
}

fn private_directory(path: &Path) -> Result<()> {
    fs::create_dir_all(path).context("dirty-checkout state directory is unavailable")?;
    let metadata =
        fs::symlink_metadata(path).context("dirty-checkout state directory is unavailable")?;
    ensure!(
        metadata.file_type().is_dir() && !metadata.file_type().is_symlink(),
        "dirty-checkout state path is not a trusted directory"
    );
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .context("dirty-checkout state permissions could not be set")?;
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
            .context("checkout lease lock is unavailable")?;
        ensure!(
            file.metadata()?.file_type().is_file(),
            "checkout lease lock is not a regular file"
        );
        let started = Instant::now();
        loop {
            let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if result == 0 {
                return Ok(Self(file));
            }
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::EWOULDBLOCK) || started.elapsed() >= LOCK_WAIT {
                return Err(error).context("checkout lease lock timed out or failed");
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

fn ensure_lower_hex(value: &str, length: usize, label: &str) -> Result<()> {
    ensure!(is_lower_hex(value, length), "{label} is malformed");
    Ok(())
}

fn ensure_lower_hex_range(value: &str, min: usize, max: usize, label: &str) -> Result<()> {
    ensure!(
        (min..=max).contains(&value.len()) && is_lower_hex(value, value.len()),
        "{label} is malformed"
    );
    Ok(())
}

fn strip_git_line(raw: &[u8]) -> Result<&[u8]> {
    ensure!(
        !raw.contains(&0),
        "Git output contains an unexpected NUL byte"
    );
    let without_lf = raw.strip_suffix(b"\n").unwrap_or(raw);
    Ok(without_lf.strip_suffix(b"\r").unwrap_or(without_lf))
}

fn ensure_deadline(deadline: Instant) -> Result<()> {
    ensure!(
        Instant::now() < deadline,
        "dirty snapshot exceeded the supported time limit"
    );
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
    let remaining = deadline.saturating_duration_since(Instant::now());
    let timeout = remaining.min(GIT_TIMEOUT);
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(cwd)
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env_remove("GIT_EXTERNAL_DIFF")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    output_with_limits(&mut command, timeout, limit, MAX_GIT_STDERR_BYTES)
}

#[derive(Clone, Copy)]
enum Stream {
    Stdout,
    Stderr,
}

struct Captured {
    bytes: Vec<u8>,
    exceeded: bool,
}

fn output_with_limits(
    command: &mut Command,
    timeout: Duration,
    stdout_limit: usize,
    stderr_limit: usize,
) -> Result<std::process::Output> {
    ensure!(
        !timeout.is_zero(),
        "Git command exceeded the supported time limit"
    );
    let mut child = command
        .spawn()
        .context("Git command could not be started")?;
    let stdout = child.stdout.take().context("Git stdout was not captured")?;
    let stderr = child.stderr.take().context("Git stderr was not captured")?;
    let (sender, receiver) = mpsc::channel();
    let stdout_reader = spawn_reader(stdout, Stream::Stdout, stdout_limit, sender.clone());
    let stderr_reader = spawn_reader(stderr, Stream::Stderr, stderr_limit, sender);
    let started = Instant::now();
    loop {
        if receiver.try_recv().is_ok() {
            kill_child(&mut child);
            let status = child.wait().context("Git command wait failed")?;
            let _ = collect_output(status, stdout_reader, stderr_reader)?;
            bail!("Git output exceeds the supported size limit");
        }
        if let Some(status) = child.try_wait().context("Git command status failed")? {
            let (output, exceeded) = collect_output(status, stdout_reader, stderr_reader)?;
            ensure!(!exceeded, "Git output exceeds the supported size limit");
            return Ok(output);
        }
        if started.elapsed() >= timeout {
            kill_child(&mut child);
            let status = child.wait().context("Git command wait failed")?;
            let _ = collect_output(status, stdout_reader, stderr_reader)?;
            bail!("Git command exceeded the supported time limit");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn spawn_reader<R: Read + Send + 'static>(
    mut reader: R,
    stream: Stream,
    limit: usize,
    sender: mpsc::Sender<Stream>,
) -> JoinHandle<io::Result<Captured>> {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 8192];
        loop {
            let read = reader.read(&mut buffer)?;
            if read == 0 {
                return Ok(Captured {
                    bytes,
                    exceeded: false,
                });
            }
            let remaining = limit.saturating_sub(bytes.len());
            if read > remaining {
                bytes.extend_from_slice(&buffer[..remaining]);
                let _ = sender.send(stream);
                return Ok(Captured {
                    bytes,
                    exceeded: true,
                });
            }
            bytes.extend_from_slice(&buffer[..read]);
        }
    })
}

fn collect_output(
    status: ExitStatus,
    stdout_reader: JoinHandle<io::Result<Captured>>,
    stderr_reader: JoinHandle<io::Result<Captured>>,
) -> Result<(std::process::Output, bool)> {
    let stdout = stdout_reader
        .join()
        .map_err(|_| anyhow::anyhow!("Git stdout reader failed"))??;
    let stderr = stderr_reader
        .join()
        .map_err(|_| anyhow::anyhow!("Git stderr reader failed"))??;
    let exceeded = stdout.exceeded || stderr.exceeded;
    Ok((
        std::process::Output {
            status,
            stdout: stdout.bytes,
            stderr: stderr.bytes,
        },
        exceeded,
    ))
}

fn kill_child(child: &mut Child) {
    unsafe {
        let _ = libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL);
    }
    let _ = child.kill();
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
        .and_then(|path| dirty_snapshot(&path))
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
            adopt_dirty(
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
    let format = take_format(&mut args)?;
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
            value => {
                return Err(CliError::usage(
                    "unexpected-argument",
                    format!("unexpected adopt-dirty argument: {value}"),
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
    CliError::data("dirty-checkout-adoption-failed", error.to_string())
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
