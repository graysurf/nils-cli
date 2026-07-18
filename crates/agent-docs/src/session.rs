use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::cli::{
    SessionActivateArgs, SessionArgs, SessionCommand, SessionCommonArgs, SessionVerifyArgs,
};
use crate::config::load_catalog_from_roots;
use crate::env::{PathOverrides, ResolvedRoots, resolve_roots};
use crate::model::{Context, FallbackMode, LoadedCatalog, OutputFormat, Phase, Product};
use crate::resolver;

const RECORD_SCHEMA: &str = "agent-docs.session.v1";
const EXIT_OK: i32 = 0;
const EXIT_RUNTIME: i32 = 1;
const EXIT_DATA: i32 = 65;
const SECRET_MODE: u32 = 0o600;
const LOCK_OWNER_FILE: &str = "owner.json";
const LOCK_STALE_AFTER: Duration = Duration::from_secs(300);
const LOCK_WAIT_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Serialize, Deserialize)]
struct SessionRecord {
    schema: String,
    session_hash: String,
    project_hash: String,
    product: String,
    active_intents: BTreeMap<String, String>,
    /// Phase-scoped activations: intent -> { phase -> fingerprint }. Skipped
    /// when empty so a no-phase record serializes byte-identically to a
    /// pre-phase record (and keeps its fingerprint stable across the upgrade).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    active_phase_intents: BTreeMap<String, BTreeMap<String, String>>,
    activated_at: String,
    producer_version: String,
}

#[derive(Debug, Serialize)]
struct SessionData {
    product: String,
    /// The phase this result was scoped to, when `--phase` was supplied. Absent
    /// for no-phase calls so their stable output shape is unchanged.
    #[serde(skip_serializing_if = "Option::is_none")]
    phase: Option<String>,
    active_intents: Vec<String>,
    record_file: String,
    verified: bool,
    /// Intents newly prepared (or refreshed) by this `prepare` call. Absent for
    /// activate/status/verify so their stable output shape is unchanged.
    #[serde(skip_serializing_if = "Option::is_none")]
    prepared_intents: Option<Vec<String>>,
    /// Stable reason code for a `prepare` result (`prepared` / `already-current`).
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

pub fn run(args: SessionArgs, overrides: PathOverrides, fallback: FallbackMode) -> i32 {
    match args.command {
        SessionCommand::Activate(args) => activate(args, overrides, fallback),
        SessionCommand::Prepare(args) => prepare(args, overrides, fallback),
        SessionCommand::Status(args) => status(args, overrides),
        SessionCommand::Verify(args) => verify(args, overrides, fallback),
    }
}

fn activate(args: SessionActivateArgs, overrides: PathOverrides, fallback: FallbackMode) -> i32 {
    let common = &args.common;
    let result = (|| -> Result<SessionData, SessionFailure> {
        validate_common(common)?;
        let phase = parse_phase_arg(args.phase.as_deref())?;
        let roots = resolve_roots(&overrides)
            .map_err(|err| SessionFailure::runtime("root-resolution-failed", err.to_string()))?;
        let catalog = load_catalog_from_roots(&roots)
            .map_err(|err| SessionFailure::runtime("catalog-load-failed", err.to_string()))?;
        let available = resolver::declared_intents(&roots, fallback, &catalog);
        let path = record_path(common, &roots.project_path)?;
        let _lock = RecordLock::acquire(&path)?;
        let mut record = if path.exists() {
            let record = read_record(&path)?;
            validate_record_context(common, &roots.project_path, &record)?;
            record
        } else {
            new_record(common, &roots.project_path)
        };
        for raw in &args.intent {
            let intent =
                Context::parse(raw).map_err(|err| SessionFailure::data("invalid-intent", err))?;
            if !available.iter().any(|item| item == intent.as_str()) {
                return Err(SessionFailure::data(
                    "undeclared-intent",
                    format!("intent `{intent}` is not declared"),
                ));
            }
            let report = resolver::resolve_intent_with_catalog_for_scope(
                &intent,
                &roots,
                Some(common.product),
                phase.clone(),
                true,
                fallback,
                true,
                &catalog,
            );
            if report.has_unsatisfied_required() {
                return Err(SessionFailure::data(
                    unsatisfied_code(phase.as_ref()),
                    format!("strict preflight failed for `{intent}`"),
                ));
            }
            store_activation(
                &mut record,
                &intent,
                phase.as_ref(),
                fingerprint(&report, &catalog)?,
            );
        }
        record.activated_at = jiff::Timestamp::now().to_string();
        write_record(&path, &record)?;
        let mut out = data(&path, &common.state_home, &record, true)?;
        out.phase = phase.as_ref().map(|p| p.to_string());
        Ok(out)
    })();
    render(common.format, "activate", result)
}

/// Prepare one or more declared intents atomically and report a stable JSON
/// result. `prepare` performs the same strict preflight + activation as
/// `activate`, but additionally reports which intents were newly prepared this
/// call and a stable `reason` code, so a runtime hook can drive intent
/// preparation with a single trusted invocation instead of a separate
/// activate + preflight round-trip.
fn prepare(args: SessionActivateArgs, overrides: PathOverrides, fallback: FallbackMode) -> i32 {
    let common = &args.common;
    let result = (|| -> Result<SessionData, SessionFailure> {
        validate_common(common)?;
        let phase = parse_phase_arg(args.phase.as_deref())?;
        let roots = resolve_roots(&overrides)
            .map_err(|err| SessionFailure::runtime("root-resolution-failed", err.to_string()))?;
        let catalog = load_catalog_from_roots(&roots)
            .map_err(|err| SessionFailure::runtime("catalog-load-failed", err.to_string()))?;
        let available = resolver::declared_intents(&roots, fallback, &catalog);
        let path = record_path(common, &roots.project_path)?;
        let _lock = RecordLock::acquire(&path)?;
        let mut record = if path.exists() {
            let record = read_record(&path)?;
            validate_record_context(common, &roots.project_path, &record)?;
            record
        } else {
            new_record(common, &roots.project_path)
        };
        let mut prepared: Vec<String> = Vec::new();
        for raw in &args.intent {
            let intent =
                Context::parse(raw).map_err(|err| SessionFailure::data("invalid-intent", err))?;
            if !available.iter().any(|item| item == intent.as_str()) {
                return Err(SessionFailure::data(
                    "undeclared-intent",
                    format!("intent `{intent}` is not declared"),
                ));
            }
            let report = resolver::resolve_intent_with_catalog_for_scope(
                &intent,
                &roots,
                Some(common.product),
                phase.clone(),
                true,
                fallback,
                true,
                &catalog,
            );
            if report.has_unsatisfied_required() {
                return Err(SessionFailure::data(
                    unsatisfied_code(phase.as_ref()),
                    format!("strict preflight failed for `{intent}`"),
                ));
            }
            let fingerprint = fingerprint(&report, &catalog)?;
            let is_new = store_activation(&mut record, &intent, phase.as_ref(), fingerprint);
            if is_new {
                prepared.push(intent.to_string());
            }
        }
        record.activated_at = jiff::Timestamp::now().to_string();
        write_record(&path, &record)?;
        let mut out = data(&path, &common.state_home, &record, true)?;
        out.phase = phase.as_ref().map(|p| p.to_string());
        out.reason = Some(
            if prepared.is_empty() {
                "already-current"
            } else {
                "prepared"
            }
            .to_string(),
        );
        out.prepared_intents = Some(prepared);
        Ok(out)
    })();
    render(common.format, "prepare", result)
}

fn status(common: SessionCommonArgs, overrides: PathOverrides) -> i32 {
    let result = (|| -> Result<SessionData, SessionFailure> {
        validate_common(&common)?;
        let roots = resolve_roots(&overrides)
            .map_err(|err| SessionFailure::runtime("root-resolution-failed", err.to_string()))?;
        let path = record_path(&common, &roots.project_path)?;
        let record = read_record(&path)?;
        validate_record_context(&common, &roots.project_path, &record)?;
        data(&path, &common.state_home, &record, false)
    })();
    render(common.format, "status", result)
}

fn verify(args: SessionVerifyArgs, overrides: PathOverrides, fallback: FallbackMode) -> i32 {
    let common = &args.common;
    let result = (|| -> Result<SessionData, SessionFailure> {
        validate_common(common)?;
        let phase = parse_phase_arg(args.phase.as_deref())?;
        let roots = resolve_roots(&overrides)
            .map_err(|err| SessionFailure::runtime("root-resolution-failed", err.to_string()))?;
        let catalog = load_catalog_from_roots(&roots)
            .map_err(|err| SessionFailure::runtime("catalog-load-failed", err.to_string()))?;
        let path = record_path(common, &roots.project_path)?;
        let record = read_record(&path)?;
        validate_record_context(common, &roots.project_path, &record)?;
        for raw in &args.require_intent {
            let intent =
                Context::parse(raw).map_err(|err| SessionFailure::data("invalid-intent", err))?;
            verify_intent(
                &intent,
                phase.as_ref(),
                &record,
                &roots,
                common.product,
                fallback,
                &catalog,
            )?;
        }
        let mut out = data(&path, &common.state_home, &record, true)?;
        out.phase = phase.as_ref().map(|p| p.to_string());
        Ok(out)
    })();
    render(common.format, "verify", result)
}

/// Verify a single required intent.
///
/// With no phase, the intent must have a matching full activation (today's
/// behavior). With a phase, verification passes on a matching phase-scoped
/// activation OR a matching full (no-phase) activation, since a full prepare
/// covers every phase's subset.
#[allow(clippy::too_many_arguments)]
fn verify_intent(
    intent: &Context,
    phase: Option<&Phase>,
    record: &SessionRecord,
    roots: &ResolvedRoots,
    product: Product,
    fallback: FallbackMode,
    catalog: &LoadedCatalog,
) -> Result<(), SessionFailure> {
    let Some(phase) = phase else {
        let Some(stored) = record.active_intents.get(intent.as_str()) else {
            return Err(SessionFailure::data(
                "missing-intent",
                format!("intent `{intent}` is not active"),
            ));
        };
        if !activation_matches(intent, None, stored, roots, product, fallback, catalog)? {
            return Err(SessionFailure::data(
                "stale-activation",
                format!("activation for `{intent}` no longer matches the resolved catalog"),
            ));
        }
        return Ok(());
    };

    let phase_stored = record
        .active_phase_intents
        .get(intent.as_str())
        .and_then(|phases| phases.get(phase.as_str()));
    let full_stored = record.active_intents.get(intent.as_str());
    if phase_stored.is_none() && full_stored.is_none() {
        return Err(SessionFailure::data(
            "missing-intent",
            format!("intent `{intent}` is not active for phase `{phase}`"),
        ));
    }

    if let Some(stored) = phase_stored
        && activation_matches(
            intent,
            Some(phase),
            stored,
            roots,
            product,
            fallback,
            catalog,
        )?
    {
        return Ok(());
    }
    if let Some(stored) = full_stored
        && activation_matches(intent, None, stored, roots, product, fallback, catalog)?
    {
        return Ok(());
    }
    Err(SessionFailure::data(
        "stale-activation",
        format!(
            "activation for `{intent}` no longer matches the resolved catalog for phase `{phase}`"
        ),
    ))
}

/// Whether a stored fingerprint still matches a freshly resolved, satisfied
/// report for the intent at the given phase scope.
#[allow(clippy::too_many_arguments)]
fn activation_matches(
    intent: &Context,
    phase: Option<&Phase>,
    stored: &str,
    roots: &ResolvedRoots,
    product: Product,
    fallback: FallbackMode,
    catalog: &LoadedCatalog,
) -> Result<bool, SessionFailure> {
    let report = resolver::resolve_intent_with_catalog_for_scope(
        intent,
        roots,
        Some(product),
        phase.cloned(),
        true,
        fallback,
        true,
        catalog,
    );
    Ok(!report.has_unsatisfied_required() && stored == fingerprint(&report, catalog)?)
}

fn parse_phase_arg(raw: Option<&str>) -> Result<Option<Phase>, SessionFailure> {
    match raw {
        None => Ok(None),
        Some(raw) => Phase::parse(raw)
            .map(Some)
            .map_err(|err| SessionFailure::data("invalid-phase", err)),
    }
}

fn new_record(common: &SessionCommonArgs, project: &Path) -> SessionRecord {
    SessionRecord {
        schema: RECORD_SCHEMA.to_string(),
        session_hash: digest(common.session_id.trim().as_bytes()),
        project_hash: project_hash(project),
        product: common.product.as_str().to_string(),
        active_intents: BTreeMap::new(),
        active_phase_intents: BTreeMap::new(),
        activated_at: jiff::Timestamp::now().to_string(),
        producer_version: env!("CARGO_PKG_VERSION").to_string(),
    }
}

/// Store an activation fingerprint for `intent` at the given phase scope and
/// return whether it was newly added or refreshed. A no-phase activation lives
/// in `active_intents` (byte-compatible with pre-phase records); a phase-scoped
/// activation lives in `active_phase_intents[intent][phase]`.
fn store_activation(
    record: &mut SessionRecord,
    intent: &Context,
    phase: Option<&Phase>,
    fingerprint: String,
) -> bool {
    let key = intent.to_string();
    match phase {
        Some(phase) => {
            let phases = record.active_phase_intents.entry(key).or_default();
            let is_new = phases.get(phase.as_str()) != Some(&fingerprint);
            phases.insert(phase.as_str().to_string(), fingerprint);
            is_new
        }
        None => {
            let is_new = record.active_intents.get(&key) != Some(&fingerprint);
            record.active_intents.insert(key, fingerprint);
            is_new
        }
    }
}

fn unsatisfied_code(phase: Option<&Phase>) -> &'static str {
    if phase.is_some() {
        "phase-unsatisfied"
    } else {
        "preflight-unsatisfied"
    }
}

fn validate_common(common: &SessionCommonArgs) -> Result<(), SessionFailure> {
    if common.session_id.trim().is_empty() {
        return Err(SessionFailure::data(
            "invalid-session-id",
            "--session-id must not be empty",
        ));
    }
    if !common.state_home.is_absolute() {
        return Err(SessionFailure::data(
            "invalid-state-home",
            "--state-home must be absolute",
        ));
    }
    Ok(())
}

fn validate_record_context(
    common: &SessionCommonArgs,
    project: &Path,
    record: &SessionRecord,
) -> Result<(), SessionFailure> {
    let context_matches = record.session_hash == digest(common.session_id.trim().as_bytes())
        && record.project_hash == project_hash(project)
        && record.product == common.product.as_str();
    if !context_matches {
        return Err(SessionFailure::data(
            "context-mismatch",
            "session activation does not match the requested session, project, and product",
        ));
    }
    Ok(())
}

fn record_path(common: &SessionCommonArgs, project: &Path) -> Result<PathBuf, SessionFailure> {
    Ok(common
        .state_home
        .join("agent-docs/sessions")
        .join(digest(common.session_id.trim().as_bytes()))
        .join(common.product.as_str())
        .join(format!("{}.json", project_hash(project))))
}

fn project_hash(project: &Path) -> String {
    let canonical = fs::canonicalize(project).unwrap_or_else(|_| project.to_path_buf());
    digest(canonical.to_string_lossy().as_bytes())
}

fn fingerprint<T: Serialize, C: Serialize>(
    report: &T,
    catalog: &C,
) -> Result<String, SessionFailure> {
    let bytes = serde_json::to_vec(&(report, catalog))
        .map_err(|err| SessionFailure::runtime("fingerprint-failed", err.to_string()))?;
    Ok(digest(&bytes))
}

fn digest(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    format!(
        "sha256:{}",
        digest
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    )
}

fn read_record(path: &Path) -> Result<SessionRecord, SessionFailure> {
    let raw = fs::read_to_string(path).map_err(|err| {
        let code = if err.kind() == std::io::ErrorKind::NotFound {
            "missing-activation"
        } else {
            "record-read-failed"
        };
        SessionFailure::data(code, format!("session activation is unavailable: {err}"))
    })?;
    let record: SessionRecord = serde_json::from_str(&raw)
        .map_err(|err| SessionFailure::runtime("record-parse-failed", err.to_string()))?;
    if record.schema != RECORD_SCHEMA {
        return Err(SessionFailure::data(
            "unsupported-record",
            format!("unsupported session schema `{}`", record.schema),
        ));
    }
    Ok(record)
}

fn write_record(path: &Path, record: &SessionRecord) -> Result<(), SessionFailure> {
    let mut bytes = serde_json::to_vec_pretty(record)
        .map_err(|err| SessionFailure::runtime("record-render-failed", err.to_string()))?;
    bytes.push(b'\n');
    nils_common::fs::write_atomic(path, &bytes, SECRET_MODE)
        .map_err(|err| SessionFailure::runtime("record-write-failed", err.to_string()))
}

fn data(
    path: &Path,
    state_home: &Path,
    record: &SessionRecord,
    verified: bool,
) -> Result<SessionData, SessionFailure> {
    let record_file = path.strip_prefix(state_home).map_err(|_| {
        SessionFailure::runtime(
            "record-path-not-portable",
            "session record path is outside the configured state home",
        )
    })?;
    // The active-intent list is the union of full and phase-scoped activations.
    // For a no-phase record `active_phase_intents` is empty, so the set reduces
    // to the sorted `active_intents` keys and the output stays byte-identical.
    let mut active: BTreeSet<String> = record.active_intents.keys().cloned().collect();
    active.extend(record.active_phase_intents.keys().cloned());
    Ok(SessionData {
        product: record.product.clone(),
        phase: None,
        active_intents: active.into_iter().collect(),
        record_file: record_file.to_string_lossy().replace('\\', "/"),
        verified,
        prepared_intents: None,
        reason: None,
    })
}

fn render(format: OutputFormat, command: &str, result: Result<SessionData, SessionFailure>) -> i32 {
    match result {
        Ok(data) => {
            if format == OutputFormat::Json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "schema_version": format!("cli.agent-docs.session.{command}.v1"),
                        "ok": true,
                        "data": data,
                    }))
                    .expect("session output serializes")
                );
            } else {
                println!(
                    "agent-docs session {command}: product={} intents={} verified={}",
                    data.product,
                    data.active_intents.join(","),
                    data.verified
                );
            }
            EXIT_OK
        }
        Err(failure) => {
            if format == OutputFormat::Json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "schema_version": format!("cli.agent-docs.session.{command}.v1"),
                        "ok": false,
                        "error": {"code": failure.code, "message": failure.message},
                    }))
                    .expect("session error serializes")
                );
            } else {
                eprintln!("agent-docs session {command}: {}", failure.message);
            }
            failure.exit_code
        }
    }
}

struct SessionFailure {
    code: &'static str,
    message: String,
    exit_code: i32,
}

impl SessionFailure {
    fn data(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            exit_code: EXIT_DATA,
        }
    }
    fn runtime(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            exit_code: EXIT_RUNTIME,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct LockOwner {
    pid: u32,
    created_at_unix_seconds: u64,
}

struct RecordLock(PathBuf);

impl RecordLock {
    fn acquire(record: &Path) -> Result<Self, SessionFailure> {
        let lock = record.with_extension("json.lock");
        if let Some(parent) = lock.parent() {
            fs::create_dir_all(parent)
                .map_err(|err| SessionFailure::runtime("lock-parent-failed", err.to_string()))?;
        }
        let started = Instant::now();
        loop {
            match fs::create_dir(&lock) {
                Ok(()) => {
                    if let Err(err) = write_lock_owner(&lock) {
                        let _ = fs::remove_dir_all(&lock);
                        return Err(err);
                    }
                    return Ok(Self(lock));
                }
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                    if lock_is_stale(&lock) && reclaim_stale_lock(&lock)? {
                        continue;
                    }
                    if started.elapsed() >= LOCK_WAIT_TIMEOUT {
                        return Err(SessionFailure::runtime(
                            "lock-timeout",
                            "timed out waiting for the session activation lock",
                        ));
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                Err(err) => return Err(SessionFailure::runtime("lock-failed", err.to_string())),
            }
        }
    }
}

fn write_lock_owner(lock: &Path) -> Result<(), SessionFailure> {
    let owner = LockOwner {
        pid: std::process::id(),
        created_at_unix_seconds: unix_seconds(SystemTime::now()),
    };
    let bytes = serde_json::to_vec(&owner)
        .map_err(|err| SessionFailure::runtime("lock-owner-render-failed", err.to_string()))?;
    fs::write(lock.join(LOCK_OWNER_FILE), bytes)
        .map_err(|err| SessionFailure::runtime("lock-owner-write-failed", err.to_string()))
}

fn lock_is_stale(lock: &Path) -> bool {
    if let Ok(raw) = fs::read(lock.join(LOCK_OWNER_FILE))
        && let Ok(owner) = serde_json::from_slice::<LockOwner>(&raw)
    {
        let expired = unix_seconds(SystemTime::now()).saturating_sub(owner.created_at_unix_seconds)
            >= LOCK_STALE_AFTER.as_secs();
        return expired && owner_is_definitely_dead(owner.pid);
    }
    fs::metadata(lock)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .is_some_and(|age| age >= LOCK_STALE_AFTER)
}

fn owner_is_definitely_dead(pid: u32) -> bool {
    if pid == 0 {
        return true;
    }

    #[cfg(unix)]
    {
        let Ok(pid) = libc::pid_t::try_from(pid) else {
            return true;
        };
        // SAFETY: signal 0 does not deliver a signal; it only checks whether
        // the positive PID exists and whether this process may signal it.
        if unsafe { libc::kill(pid, 0) } == 0 {
            return false;
        }
        std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
    }

    #[cfg(not(unix))]
    {
        false
    }
}

fn reclaim_stale_lock(lock: &Path) -> Result<bool, SessionFailure> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let reclaimed =
        lock.with_extension(format!("json.lock.reclaim-{}-{nonce}", std::process::id()));
    match fs::rename(lock, &reclaimed) {
        Ok(()) => {
            fs::remove_dir_all(&reclaimed).map_err(|err| {
                SessionFailure::runtime("stale-lock-remove-failed", err.to_string())
            })?;
            Ok(true)
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(SessionFailure::runtime(
            "stale-lock-reclaim-failed",
            err.to_string(),
        )),
    }
}

fn unix_seconds(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

impl Drop for RecordLock {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;

    fn write_expired_lock(record: &Path, pid: u32) -> PathBuf {
        let lock = record.with_extension("json.lock");
        fs::create_dir(&lock).expect("create lock directory");
        let owner = LockOwner {
            pid,
            created_at_unix_seconds: 0,
        };
        fs::write(
            lock.join(LOCK_OWNER_FILE),
            serde_json::to_vec(&owner).expect("serialize owner"),
        )
        .expect("write owner");
        lock
    }

    #[test]
    fn expired_lock_for_live_process_is_not_stale() {
        let temp = tempfile::TempDir::new().expect("tempdir");
        let record = temp.path().join("session.json");
        let lock = write_expired_lock(&record, std::process::id());

        assert!(!lock_is_stale(&lock));
    }

    #[cfg(unix)]
    #[test]
    fn expired_lock_for_dead_process_is_reclaimed() {
        let mut child = Command::new("sh")
            .args(["-c", "exit 0"])
            .spawn()
            .expect("spawn short-lived owner");
        let dead_pid = child.id();
        assert!(child.wait().expect("reap owner").success());

        let temp = tempfile::TempDir::new().expect("tempdir");
        let record = temp.path().join("session.json");
        let lock = write_expired_lock(&record, dead_pid);

        let acquired = RecordLock::acquire(&record)
            .unwrap_or_else(|failure| panic!("recover dead owner's lock: {}", failure.message));
        let owner: LockOwner = serde_json::from_slice(
            &fs::read(lock.join(LOCK_OWNER_FILE)).expect("read replacement owner"),
        )
        .expect("parse replacement owner");
        assert_eq!(owner.pid, std::process::id());

        drop(acquired);
        assert!(!lock.exists());
    }
}
