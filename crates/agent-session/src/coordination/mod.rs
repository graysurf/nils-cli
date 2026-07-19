pub(crate) mod broker;
pub(crate) mod claims;
pub(crate) mod context;
pub(crate) mod mailbox;
mod notification;
pub(crate) mod server;

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use jiff::Timestamp;
use nils_common::fs::{SECRET_FILE_MODE, write_atomic};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::cli::{self, BrokerCommand, MessageCommand, WorkContextCommand};
use crate::{
    CliContext, CliError, SessionRecord, load_session_record, render_error, render_single_success,
    session_dir,
};

const REGISTRY_VERSION: &str = "agent-session.coordination-registry.v1";
const REGISTRY_FILE: &str = "registry.json";
const REGISTRY_LOCK: &str = "registry.lock";
const LOCK_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_REGISTRY_BYTES: u64 = 68 * 1024 * 1024;
const RECEIPT_TTL_SECS: i64 = 24 * 60 * 60;
const TERMINAL_RETENTION_SECS: i64 = 5 * 60;
pub(crate) const CAPABILITY_ENV: &str = "AGENT_SESSION_CAPABILITY_FILE";

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub(crate) struct Registry {
    schema_version: String,
    fingerprint_epoch: u64,
    fingerprint_key: String,
    brokers: BTreeMap<String, BrokerRecord>,
    claims: Vec<context::WorkContextRecord>,
    operations: Vec<claims::OperationLease>,
    messages: Vec<mailbox::StoredMessage>,
    receipts: BTreeMap<String, IdempotencyReceipt>,
    notifications: BTreeMap<String, notification::NotificationReceipt>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct BrokerRecord {
    pub session_id: String,
    pub incarnation: String,
    pub capability_digest: String,
    pub generation: u64,
    pub state: String,
    pub heartbeat_at: String,
    pub heartbeat_epoch: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct IdempotencyReceipt {
    principal: String,
    incarnation: String,
    operation: String,
    digest: String,
    outcome: Value,
    expires_at_epoch: i64,
}

pub(crate) struct LockedRegistry {
    _lock: File,
    path: PathBuf,
    pub registry: Registry,
}

impl Drop for LockedRegistry {
    fn drop(&mut self) {
        // SAFETY: the descriptor remains owned by `_lock` for this call.
        unsafe {
            libc::flock(self._lock.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

impl LockedRegistry {
    pub fn save(&mut self) -> Result<(), CliError> {
        self.registry.schema_version = REGISTRY_VERSION.to_string();
        let bytes = serde_json::to_vec_pretty(&self.registry).map_err(|_| store_corrupt())?;
        if bytes.len() as u64 > MAX_REGISTRY_BYTES {
            return Err(CliError::data(
                "quota-exceeded",
                "coordination registry byte limit exceeded",
                None,
            ));
        }
        write_atomic(&self.path, &bytes, SECRET_FILE_MODE).map_err(|_| store_unavailable())
    }
}

pub(crate) fn run_work_context(context: &CliContext, args: cli::WorkContextArgs) -> i32 {
    let (command, format, result) = match args.command {
        WorkContextCommand::Claim(args) => (
            "work-context-claim",
            args.format,
            claims::claim(context, args),
        ),
        WorkContextCommand::Show(args) => (
            "work-context-show",
            args.format,
            claims::show(context, args),
        ),
        WorkContextCommand::Check(args) => (
            "work-context-check",
            args.format,
            claims::check(context, args),
        ),
        WorkContextCommand::Renew(args) => (
            "work-context-renew",
            args.format,
            claims::renew(context, args),
        ),
        WorkContextCommand::Release(args) => (
            "work-context-release",
            args.format,
            claims::release(context, args),
        ),
        WorkContextCommand::Admit(args) => (
            "work-context-admit",
            args.format,
            claims::admit(context, args),
        ),
        WorkContextCommand::Complete(args) => (
            "work-context-complete",
            args.format,
            claims::complete(context, args),
        ),
        WorkContextCommand::Reconcile(args) => (
            "work-context-reconcile",
            args.format,
            claims::reconcile(context, args),
        ),
    };
    render_coordination(command, format, result)
}

pub(crate) fn run_broker(context: &CliContext, args: cli::BrokerArgs) -> i32 {
    let (command, format, result) = match args.command {
        BrokerCommand::Status(args) => {
            ("broker-status", args.format, broker::status(context, args))
        }
        BrokerCommand::Adopt(args) => (
            "broker-adopt",
            args.format,
            broker::recover(context, args, false),
        ),
        BrokerCommand::Reconcile(args) => (
            "broker-reconcile",
            args.format,
            broker::recover(context, args, true),
        ),
    };
    render_coordination(command, format, result)
}

pub(crate) fn run_message(context: &CliContext, args: cli::MessageArgs) -> i32 {
    let (command, format, result) = match args.command {
        MessageCommand::Send(args) => ("message-send", args.format, mailbox::send(context, args)),
        MessageCommand::Inbox(args) => {
            ("message-inbox", args.format, mailbox::inbox(context, args))
        }
        MessageCommand::Show(args) => ("message-show", args.format, mailbox::show(context, args)),
        MessageCommand::Ack(args) => ("message-ack", args.format, mailbox::ack(context, args)),
        MessageCommand::Reply(args) => {
            ("message-reply", args.format, mailbox::reply(context, args))
        }
        MessageCommand::Wait(args) => ("message-wait", args.format, mailbox::wait(context, args)),
    };
    render_coordination(command, format, result)
}

fn render_coordination(
    command: &'static str,
    format: nils_common::cli_contract::OutputFormat,
    result: Result<Value, CliError>,
) -> i32 {
    match result {
        Ok(value) => render_single_success(command, format, &value, render_value_text),
        Err(error) => render_error(command, format, error),
    }
}

fn render_value_text(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| "{}".to_string()) + "\n"
}

pub(crate) fn provision(context: &CliContext, record: &SessionRecord) -> Result<PathBuf, CliError> {
    broker::provision(context, record)
}

pub(crate) fn prepare(context: &CliContext, record: &SessionRecord) -> Result<(), CliError> {
    broker::prepare(context, record)
}

pub(crate) fn activate_ready(context: &CliContext, record: &SessionRecord) -> Result<(), CliError> {
    broker::activate_ready(context, record)
}

pub(crate) fn ensure_ready(context: &CliContext, record: &SessionRecord) -> Result<(), CliError> {
    broker::ensure_ready(context, record)
}

pub(crate) fn revoke(context: &CliContext, record: &SessionRecord) -> Result<(), CliError> {
    broker::revoke(context, record)
}

pub(crate) fn notification_candidate(
    context: &CliContext,
    message_id: &str,
) -> Result<Option<(String, String)>, CliError> {
    notification::candidate(context, message_id)
}

pub(crate) fn begin_notification_attempt(
    context: &CliContext,
    message_id: &str,
    target_session_id: &str,
    target_incarnation: &str,
) -> Result<bool, CliError> {
    notification::begin_attempt(context, message_id, target_session_id, target_incarnation)
}

pub(crate) fn notification_prompt(message_id: &str, session_id: &str) -> String {
    notification::fixed_prompt(message_id, session_id)
}

pub(crate) fn coordination_dir(context: &CliContext, session_id: &str) -> PathBuf {
    session_dir(context, session_id).join("coordination")
}

pub(crate) fn capability_path(
    context: &CliContext,
    session_id: &str,
    incarnation: &str,
) -> PathBuf {
    capability_path_for_state(&context.state_dir, session_id, incarnation)
}

pub(crate) fn capability_path_for_state(
    state_dir: &Path,
    session_id: &str,
    incarnation: &str,
) -> PathBuf {
    state_dir
        .join("sessions")
        .join(session_id)
        .join("coordination")
        .join(format!(
            "capability-{}",
            digest_bytes(incarnation.as_bytes())
        ))
}

pub(crate) fn heartbeat_path(state_dir: &Path, session_id: &str) -> PathBuf {
    state_dir
        .join("sessions")
        .join(session_id)
        .join("coordination")
        .join("heartbeat")
}

fn authenticate_from_file(
    context: &CliContext,
    session_id: &str,
    capability_file: Option<&Path>,
) -> Result<(SessionRecord, String), CliError> {
    let path = capability_file
        .map(PathBuf::from)
        .or_else(|| std::env::var_os(CAPABILITY_ENV).map(PathBuf::from))
        .ok_or_else(unauthorized)?;
    let metadata = fs::symlink_metadata(&path).map_err(|_| unauthorized())?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > 512
        || metadata.mode() & 0o077 != 0
        || metadata.uid() != unsafe { libc::geteuid() }
    {
        return Err(unauthorized());
    }
    let token = fs::read_to_string(&path).map_err(|_| unauthorized())?;
    authenticate_token(context, session_id, token.trim())
}

pub(crate) fn authenticate_token(
    context: &CliContext,
    session_id: &str,
    token: &str,
) -> Result<(SessionRecord, String), CliError> {
    if token.len() < 32 || token.len() > 256 || !token.is_ascii() {
        return Err(unauthorized());
    }
    let record = load_session_record(context, session_id).map_err(|_| unauthorized())?;
    let incarnation = incarnation(&record)?;
    let locked = lock_registry(context)?;
    let broker = locked
        .registry
        .brokers
        .get(&record.id)
        .ok_or_else(unauthorized)?;
    if broker.state != "ready"
        || broker.incarnation != incarnation
        || !digest_eq(&broker.capability_digest, &digest_bytes(token.as_bytes()))
        || !broker::capability_available(
            context,
            &record.id,
            &incarnation,
            &broker.capability_digest,
        )
    {
        return Err(unauthorized());
    }
    if !broker::heartbeat_fresh(context, &record.id, &incarnation, broker.heartbeat_epoch) {
        return Err(CliError::runtime(
            "coordination-broker-lost",
            "coordination broker heartbeat is stale",
            None,
        ));
    }
    Ok((record, incarnation))
}

pub(crate) fn incarnation(record: &SessionRecord) -> Result<String, CliError> {
    record
        .runtime
        .as_ref()
        .map(|runtime| runtime.launch_id.trim())
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            CliError::data(
                "session-incarnation-conflict",
                "session incarnation is unavailable",
                None,
            )
        })
}

pub(crate) fn lock_registry(context: &CliContext) -> Result<LockedRegistry, CliError> {
    let root = coordination_root(context)?;
    let lock_path = root.join(REGISTRY_LOCK);
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(SECRET_FILE_MODE)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&lock_path)
        .map_err(|error| {
            if error.raw_os_error() == Some(libc::ELOOP) {
                store_untrusted()
            } else {
                store_unavailable()
            }
        })?;
    lock.set_permissions(fs::Permissions::from_mode(SECRET_FILE_MODE))
        .map_err(|_| store_unavailable())?;
    let lock_metadata = lock.metadata().map_err(|_| store_unavailable())?;
    if !lock_metadata.is_file()
        || lock_metadata.uid() != unsafe { libc::geteuid() }
        || lock_metadata.mode() & 0o077 != 0
        || lock_metadata.nlink() != 1
    {
        return Err(store_untrusted());
    }
    let started = Instant::now();
    loop {
        // SAFETY: flock is called with a valid, owned file descriptor.
        let result = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if result == 0 {
            break;
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::EWOULDBLOCK) || started.elapsed() >= LOCK_TIMEOUT {
            return Err(CliError::runtime(
                "coordination-lock-timeout",
                "coordination registry lock could not be acquired",
                None,
            ));
        }
        thread::sleep(Duration::from_millis(10));
    }
    let path = root.join(REGISTRY_FILE);
    let mut registry = match fs::symlink_metadata(&path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink()
                || !metadata.is_file()
                || metadata.len() > MAX_REGISTRY_BYTES
                || metadata.mode() & 0o077 != 0
                || metadata.uid() != unsafe { libc::geteuid() }
            {
                return Err(store_untrusted());
            }
            let bytes = fs::read(&path).map_err(|_| store_unavailable())?;
            let registry: Registry = serde_json::from_slice(&bytes).map_err(|_| store_corrupt())?;
            if !registry.schema_version.is_empty() && registry.schema_version != REGISTRY_VERSION {
                return Err(store_corrupt());
            }
            registry
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Registry {
            schema_version: REGISTRY_VERSION.to_string(),
            ..Registry::default()
        },
        Err(_) => return Err(store_unavailable()),
    };
    let now = now_epoch();
    let mut renewed = false;
    for claim in &mut registry.claims {
        if claim.state != "active" || claim.expires_at_epoch > now.saturating_add(15 * 60) {
            continue;
        }
        let Some(broker) = registry.brokers.get(&claim.session_id) else {
            continue;
        };
        if broker.incarnation == claim.session_incarnation
            && broker.state == "ready"
            && broker::heartbeat_fresh(
                context,
                &claim.session_id,
                &claim.session_incarnation,
                broker.heartbeat_epoch,
            )
        {
            claim.revision = claim.revision.saturating_add(1);
            claim.updated_at = timestamp(now);
            claim.expires_at_epoch = now.saturating_add(30 * 60);
            claim.expires_at = timestamp(claim.expires_at_epoch);
            renewed = true;
        }
    }
    if renewed {
        let bytes = serde_json::to_vec_pretty(&registry).map_err(|_| store_corrupt())?;
        write_atomic(&path, &bytes, SECRET_FILE_MODE).map_err(|_| store_unavailable())?;
    }
    Ok(LockedRegistry {
        _lock: lock,
        path,
        registry,
    })
}

fn coordination_root(context: &CliContext) -> Result<PathBuf, CliError> {
    let root = context.state_dir.join("coordination");
    if let Ok(metadata) = fs::symlink_metadata(&root) {
        if metadata.file_type().is_symlink()
            || !metadata.is_dir()
            || metadata.uid() != unsafe { libc::geteuid() }
        {
            return Err(store_untrusted());
        }
    } else {
        fs::create_dir_all(&root).map_err(|_| store_unavailable())?;
    }
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700))
        .map_err(|_| store_unavailable())?;
    let canonical_state = fs::canonicalize(&context.state_dir).map_err(|_| store_unavailable())?;
    let canonical_root = fs::canonicalize(&root).map_err(|_| store_unavailable())?;
    if !canonical_root.starts_with(&canonical_state) {
        return Err(store_untrusted());
    }
    Ok(root)
}

pub(crate) fn clean_expired(registry: &mut Registry, now: i64) -> bool {
    let mut changed = false;
    for operation in &mut registry.operations {
        if operation.state == "active" && operation.expires_at_epoch <= now {
            operation.state = "completing".to_string();
            operation.revision = operation.revision.saturating_add(1);
            changed = true;
        }
        if matches!(
            operation.state.as_str(),
            "completed" | "failed" | "abandoned"
        ) && operation.terminal_at_epoch.is_none()
        {
            operation.terminal_at_epoch = Some(now);
            changed = true;
        }
    }
    for claim in &mut registry.claims {
        let bound_operation = registry.operations.iter().any(|operation| {
            operation.claim_id == claim.claim_id
                && matches!(operation.state.as_str(), "active" | "completing")
        });
        if claim.state == "active" && claim.expires_at_epoch <= now && !bound_operation {
            claim.state = "expired".to_string();
            claim.revision = claim.revision.saturating_add(1);
            claim.terminal_at_epoch = Some(now);
            changed = true;
        } else if matches!(claim.state.as_str(), "released" | "expired" | "stale")
            && claim.terminal_at_epoch.is_none()
        {
            claim.terminal_at_epoch = Some(now);
            changed = true;
        }
    }
    for message in &mut registry.messages {
        if !matches!(
            message.state.as_str(),
            "acknowledged" | "expired" | "deleted"
        ) && message.expires_at_epoch <= now
        {
            message.state = "expired".to_string();
            message.revision = message.revision.saturating_add(1);
            message.terminal_at_epoch = Some(now);
            changed = true;
        } else if matches!(
            message.state.as_str(),
            "acknowledged" | "expired" | "deleted"
        ) && message.terminal_at_epoch.is_none()
        {
            message.terminal_at_epoch = Some(now);
            changed = true;
        }
    }
    let removed_messages: std::collections::BTreeSet<_> = registry
        .messages
        .iter()
        .filter(|message| {
            message
                .terminal_at_epoch
                .is_some_and(|terminal| terminal <= now.saturating_sub(TERMINAL_RETENTION_SECS))
        })
        .map(|message| message.message_id.clone())
        .collect();
    let message_count = registry.messages.len();
    let notification_count = registry.notifications.len();
    let operation_count = registry.operations.len();
    let claim_count = registry.claims.len();
    let receipt_count = registry.receipts.len();
    registry
        .messages
        .retain(|message| !removed_messages.contains(&message.message_id));
    registry
        .notifications
        .retain(|message_id, _| !removed_messages.contains(message_id));
    registry.operations.retain(|operation| {
        operation
            .terminal_at_epoch
            .is_none_or(|terminal| terminal > now.saturating_sub(TERMINAL_RETENTION_SECS))
    });
    registry.claims.retain(|claim| {
        claim
            .terminal_at_epoch
            .is_none_or(|terminal| terminal > now.saturating_sub(TERMINAL_RETENTION_SECS))
    });
    registry
        .receipts
        .retain(|_, receipt| receipt.expires_at_epoch > now);
    changed
        || registry.messages.len() != message_count
        || registry.notifications.len() != notification_count
        || registry.operations.len() != operation_count
        || registry.claims.len() != claim_count
        || registry.receipts.len() != receipt_count
}

pub(crate) fn idempotency_replay(
    registry: &Registry,
    key: &str,
    principal: &str,
    incarnation: &str,
    operation: &str,
    digest: &str,
) -> Result<Option<Value>, CliError> {
    validate_idempotency_key(key)?;
    let Some(receipt) = registry.receipts.get(key) else {
        return Ok(None);
    };
    if receipt.principal == principal
        && receipt.incarnation == incarnation
        && receipt.operation == operation
        && receipt.digest == digest
    {
        return Ok(Some(receipt.outcome.clone()));
    }
    Err(CliError::data(
        "idempotency-conflict",
        "idempotency key is already bound to another request",
        None,
    ))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn store_receipt(
    registry: &mut Registry,
    key: String,
    principal: String,
    incarnation: String,
    operation: String,
    digest: String,
    outcome: Value,
    now: i64,
) {
    registry.receipts.insert(
        key,
        IdempotencyReceipt {
            principal,
            incarnation,
            operation,
            digest,
            outcome,
            expires_at_epoch: now.saturating_add(RECEIPT_TTL_SECS),
        },
    );
}

fn validate_idempotency_key(key: &str) -> Result<(), CliError> {
    if !(8..=128).contains(&key.len())
        || !key.is_ascii()
        || key
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte == b' ')
    {
        return Err(CliError::usage(
            "invalid-idempotency-key",
            "idempotency key must be 8-128 printable non-space ASCII bytes",
            None,
        ));
    }
    Ok(())
}

pub(crate) fn request_digest<T: Serialize>(operation: &str, value: &T) -> String {
    let mut digest = Sha256::new();
    digest.update(operation.as_bytes());
    digest.update([0]);
    digest.update(serde_json::to_vec(value).unwrap_or_default());
    hex(&digest.finalize())
}

pub(crate) fn ensure_fingerprint_key(registry: &mut Registry) {
    if registry.fingerprint_epoch == 0 {
        registry.fingerprint_epoch = 1;
    }
    if registry.fingerprint_key.is_empty() {
        registry.fingerprint_key = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
    }
}

pub(crate) fn worktree_fingerprint(
    registry: &Registry,
    checkout: &Path,
) -> Result<String, CliError> {
    if registry.fingerprint_epoch == 0 || registry.fingerprint_key.len() < 32 {
        return Err(store_corrupt());
    }
    let canonical = fs::canonicalize(checkout).unwrap_or_else(|_| checkout.to_path_buf());
    let digest = hmac_sha256(
        registry.fingerprint_key.as_bytes(),
        canonical.as_os_str().as_encoded_bytes(),
    );
    Ok(format!(
        "hmac-sha256:{}:{}",
        registry.fingerprint_epoch,
        hex(&digest)
    ))
}

fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut normalized = [0_u8; BLOCK];
    if key.len() > BLOCK {
        normalized[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        normalized[..key.len()].copy_from_slice(key);
    }
    let mut inner_pad = [0x36_u8; BLOCK];
    let mut outer_pad = [0x5c_u8; BLOCK];
    for index in 0..BLOCK {
        inner_pad[index] ^= normalized[index];
        outer_pad[index] ^= normalized[index];
    }
    let mut inner = Sha256::new();
    inner.update(inner_pad);
    inner.update(message);
    let inner = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(outer_pad);
    outer.update(inner);
    outer.finalize().into()
}

pub(crate) fn digest_bytes(value: &[u8]) -> String {
    hex(&Sha256::digest(value))
}

fn hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

fn digest_eq(left: &str, right: &str) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.bytes()
        .zip(right.bytes())
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

pub(crate) fn now_epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .try_into()
        .unwrap_or(i64::MAX)
}

pub(crate) fn timestamp(epoch: i64) -> String {
    Timestamp::from_second(epoch)
        .map(|value| value.to_string())
        .unwrap_or_else(|_| Timestamp::now().to_string())
}

pub(crate) fn unauthorized() -> CliError {
    CliError::data(
        "coordination-unauthorized",
        "coordination authority could not be verified",
        None,
    )
}

fn store_untrusted() -> CliError {
    CliError::runtime(
        "coordination-store-untrusted",
        "coordination store ownership or path is untrusted",
        None,
    )
}

fn store_corrupt() -> CliError {
    CliError::runtime(
        "coordination-store-corrupt",
        "coordination store is corrupt or unsupported",
        None,
    )
}

fn store_unavailable() -> CliError {
    CliError::runtime(
        "coordination-unavailable",
        "coordination store is unavailable",
        None,
    )
}

pub(crate) fn public_summary(context: &CliContext, session_id: &str) -> CoordinationSummary {
    let registry_path = context.state_dir.join("coordination").join(REGISTRY_FILE);
    if matches!(
        fs::symlink_metadata(&registry_path),
        Err(error) if error.kind() == io::ErrorKind::NotFound
    ) {
        return CoordinationSummary::default();
    }
    let Ok(locked) = lock_registry(context) else {
        return CoordinationSummary::default();
    };
    let claim = locked
        .registry
        .claims
        .iter()
        .find(|claim| claim.session_id == session_id && claim.state == "active");
    CoordinationSummary {
        work_context_state: claim.map(|claim| claim.state.clone()),
        claim_id: claim.map(|claim| claim.claim_id.clone()),
        claim_expires_at: claim.map(|claim| claim.expires_at.clone()),
        unread_message_count: locked
            .registry
            .messages
            .iter()
            .filter(|message| {
                message.recipient_session_id == session_id && message.state == "unread"
            })
            .count(),
        coordination_conflict_severity: None,
        coordination_available: locked
            .registry
            .brokers
            .get(session_id)
            .is_some_and(|broker| {
                broker.state == "ready"
                    && broker::capability_available(
                        context,
                        session_id,
                        &broker.incarnation,
                        &broker.capability_digest,
                    )
                    && broker::heartbeat_fresh(
                        context,
                        session_id,
                        &broker.incarnation,
                        broker.heartbeat_epoch,
                    )
            }),
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub(crate) struct CoordinationSummary {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub work_context_state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claim_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claim_expires_at: Option<String>,
    pub unread_message_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coordination_conflict_severity: Option<String>,
    pub coordination_available: bool,
}

pub(crate) fn json_value<T: Serialize>(value: T) -> Result<Value, CliError> {
    serde_json::to_value(value).map_err(|_| {
        CliError::runtime(
            "coordination-store-corrupt",
            "coordination result could not be serialized",
            None,
        )
    })
}

pub(crate) fn read_bounded_json<T: for<'de> Deserialize<'de>>(
    path: &Path,
    max_bytes: u64,
    code: &'static str,
) -> Result<T, CliError> {
    let metadata = fs::metadata(path)
        .map_err(|_| CliError::data(code, "coordination input could not be read", None))?;
    if !metadata.is_file() || metadata.len() > max_bytes {
        return Err(CliError::data(code, "coordination input is invalid", None));
    }
    let bytes = fs::read(path)
        .map_err(|_| CliError::data(code, "coordination input could not be read", None))?;
    serde_json::from_slice(&bytes)
        .map_err(|_| CliError::data(code, "coordination input is invalid", None))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn digest_comparison_is_exact() {
        assert!(digest_eq("abc", "abc"));
        assert!(!digest_eq("abc", "abd"));
        assert!(!digest_eq("abc", "ab"));
    }

    #[test]
    fn hmac_sha256_matches_the_rfc_4231_vector() {
        let digest = hmac_sha256(&[0x0b; 20], b"Hi There");
        assert_eq!(
            hex(&digest),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
    }

    #[test]
    fn coordination_review_expired_operation_and_bound_claim_remain_fail_closed() {
        let mut registry: Registry = serde_json::from_value(json!({
            "schema_version": REGISTRY_VERSION,
            "claims": [{
                "schema_version": "agent-session.work-context.v1",
                "session_id": "session",
                "session_incarnation": "incarnation",
                "claim_id": "claim",
                "revision": 1,
                "state": "active",
                "intent": "implementation",
                "tier": "L2",
                "repositories": ["example/repository"],
                "worktrees": [],
                "provider_refs": [],
                "plan_refs": [],
                "scopes": [{"kind": "repository", "repository": "example/repository", "value": ""}],
                "summary": "fixture",
                "updated_at": "2030-01-01T00:00:00Z",
                "expires_at": "2030-01-01T00:00:01Z",
                "expires_at_epoch": 1
            }],
            "operations": [{
                "schema_version": "agent-session.operation-lease.v1",
                "lease_id": "lease",
                "session_id": "session",
                "session_incarnation": "incarnation",
                "claim_id": "claim",
                "claim_revision": 1,
                "operation": "edit",
                "targets": [{"kind": "repository", "repository": "example/repository", "value": ""}],
                "state": "active",
                "revision": 1,
                "started_at": "2030-01-01T00:00:00Z",
                "expires_at": "2030-01-01T00:00:01Z",
                "expires_at_epoch": 1,
                "execution_token_digest": "digest"
            }]
        })).expect("registry");
        clean_expired(&mut registry, 2);
        assert_eq!(registry.operations[0].state, "completing");
        assert_eq!(registry.claims[0].state, "active");
    }

    #[test]
    fn coordination_review_terminal_retention_reclaims_mail_and_notifications() {
        let mut registry: Registry = serde_json::from_value(json!({
            "schema_version": REGISTRY_VERSION,
            "messages": [{
                "schema_version": "agent-session.message.v1",
                "message_id": "message",
                "sender_session_id": "sender",
                "sender_incarnation": "sender-incarnation",
                "recipient_session_id": "recipient",
                "recipient_incarnation": "recipient-incarnation",
                "state": "acknowledged",
                "revision": 2,
                "reply_to": null,
                "reply_depth": 0,
                "created_at": "2030-01-01T00:00:00Z",
                "created_at_epoch": 0,
                "expires_at": "2030-01-01T00:00:01Z",
                "expires_at_epoch": 1,
                "terminal_at_epoch": 1,
                "body_bytes": 4,
                "body": "body"
            }],
            "notifications": {
                "message": {
                    "message_id": "message",
                    "target_session_id": "recipient",
                    "target_incarnation": "recipient-incarnation",
                    "state": "queued",
                    "attempted_at_epoch": 1
                }
            }
        }))
        .expect("registry");
        assert!(clean_expired(&mut registry, TERMINAL_RETENTION_SECS + 2));
        assert!(registry.messages.is_empty());
        assert!(registry.notifications.is_empty());
    }
}
