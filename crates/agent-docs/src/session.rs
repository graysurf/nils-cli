use std::collections::BTreeMap;
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
use crate::integration;
use crate::model::{Context, FallbackMode, OutputFormat, Product};
use crate::resolver;

const RECORD_SCHEMA: &str = "agent-docs.session.v2";
const EXIT_OK: i32 = 0;
const EXIT_RUNTIME: i32 = 1;
const EXIT_CONFIG: i32 = 3;
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
    #[serde(default)]
    integration_fingerprint: Option<String>,
    active_intents: BTreeMap<String, String>,
    activated_at: String,
    producer_version: String,
}

#[derive(Debug, Serialize)]
struct SessionData {
    product: String,
    active_intents: Vec<String>,
    record_file: String,
    verified: bool,
}

pub fn run(
    args: SessionArgs,
    overrides: PathOverrides,
    fallback: FallbackMode,
    use_user_config: bool,
    expected_integration_fingerprint: Option<String>,
) -> i32 {
    match args.command {
        SessionCommand::Activate(args) => activate(
            args,
            overrides,
            fallback,
            use_user_config,
            expected_integration_fingerprint.as_deref(),
        ),
        SessionCommand::Status(args) => {
            status(args, overrides, expected_integration_fingerprint.as_deref())
        }
        SessionCommand::Verify(args) => verify(
            args,
            overrides,
            fallback,
            use_user_config,
            expected_integration_fingerprint.as_deref(),
        ),
    }
}

fn activate(
    args: SessionActivateArgs,
    overrides: PathOverrides,
    fallback: FallbackMode,
    use_user_config: bool,
    expected_integration_fingerprint: Option<&str>,
) -> i32 {
    let common = &args.common;
    let result = (|| -> Result<SessionData, SessionFailure> {
        validate_common(common)?;
        let roots = resolve_roots(&overrides)
            .map_err(|err| SessionFailure::runtime("root-resolution-failed", err.to_string()))?;
        let (catalog, integration_fingerprint) = load_session_catalog(
            &roots,
            common.product,
            fallback,
            use_user_config,
            expected_integration_fingerprint,
        )?;
        let available = resolver::declared_intents(&roots, fallback, &catalog.catalog);
        let path = record_path(common, &roots.project_path)?;
        let _lock = RecordLock::acquire(&path)?;
        let mut record = if path.exists() {
            match read_record(&path) {
                Ok(record) => {
                    validate_record_context(common, &roots.project_path, &record)?;
                    record
                }
                Err(failure) if failure.code == "unsupported-record" => {
                    new_record(common, &roots.project_path, integration_fingerprint.clone())
                }
                Err(failure) => return Err(failure),
            }
        } else {
            new_record(common, &roots.project_path, integration_fingerprint.clone())
        };
        if record.integration_fingerprint != integration_fingerprint {
            record.active_intents.clear();
            record.integration_fingerprint = integration_fingerprint.clone();
        }
        for raw in &args.intent {
            let intent =
                Context::parse(raw).map_err(|err| SessionFailure::data("invalid-intent", err))?;
            if !available.iter().any(|item| item == intent.as_str()) {
                return Err(SessionFailure::data(
                    "undeclared-intent",
                    format!("intent `{intent}` is not declared"),
                ));
            }
            let report = resolver::resolve_intent_with_effective_catalog_for_product(
                &intent,
                &roots,
                Some(common.product),
                true,
                fallback,
                true,
                &catalog,
            );
            if report.has_unsatisfied_required() {
                return Err(SessionFailure::data(
                    "preflight-unsatisfied",
                    format!("strict preflight failed for `{intent}`"),
                ));
            }
            record
                .active_intents
                .insert(intent.to_string(), fingerprint(&report, &catalog.catalog)?);
        }
        record.activated_at = jiff::Timestamp::now().to_string();
        write_record(&path, &record)?;
        data(&path, &common.state_home, &record, true)
    })();
    render(common.format, "activate", result)
}

fn status(
    common: SessionCommonArgs,
    overrides: PathOverrides,
    expected_integration_fingerprint: Option<&str>,
) -> i32 {
    let result = (|| -> Result<SessionData, SessionFailure> {
        validate_common(&common)?;
        let roots = resolve_roots(&overrides)
            .map_err(|err| SessionFailure::runtime("root-resolution-failed", err.to_string()))?;
        let path = record_path(&common, &roots.project_path)?;
        let record = read_record(&path)?;
        validate_record_context(&common, &roots.project_path, &record)?;
        if let Some(expected) = expected_integration_fingerprint
            && record.integration_fingerprint.as_deref() != Some(expected)
        {
            return Err(SessionFailure::data(
                "stale-integration-decision",
                "session activation does not match the requested integration fingerprint",
            ));
        }
        data(&path, &common.state_home, &record, false)
    })();
    render(common.format, "status", result)
}

fn verify(
    args: SessionVerifyArgs,
    overrides: PathOverrides,
    fallback: FallbackMode,
    use_user_config: bool,
    expected_integration_fingerprint: Option<&str>,
) -> i32 {
    let common = &args.common;
    let result = (|| -> Result<SessionData, SessionFailure> {
        validate_common(common)?;
        let roots = resolve_roots(&overrides)
            .map_err(|err| SessionFailure::runtime("root-resolution-failed", err.to_string()))?;
        let (catalog, integration_fingerprint) = load_session_catalog(
            &roots,
            common.product,
            fallback,
            use_user_config,
            expected_integration_fingerprint,
        )?;
        let path = record_path(common, &roots.project_path)?;
        let record = read_record(&path)?;
        validate_record_context(common, &roots.project_path, &record)?;
        if record.integration_fingerprint != integration_fingerprint {
            return Err(SessionFailure::data(
                "stale-integration-decision",
                "session activation does not match the current integration decision",
            ));
        }
        for raw in &args.require_intent {
            let intent =
                Context::parse(raw).map_err(|err| SessionFailure::data("invalid-intent", err))?;
            let Some(stored) = record.active_intents.get(intent.as_str()) else {
                return Err(SessionFailure::data(
                    "missing-intent",
                    format!("intent `{intent}` is not active"),
                ));
            };
            let report = resolver::resolve_intent_with_effective_catalog_for_product(
                &intent,
                &roots,
                Some(common.product),
                true,
                fallback,
                true,
                &catalog,
            );
            if report.has_unsatisfied_required()
                || *stored != fingerprint(&report, &catalog.catalog)?
            {
                return Err(SessionFailure::data(
                    "stale-activation",
                    format!("activation for `{intent}` no longer matches the resolved catalog"),
                ));
            }
        }
        data(&path, &common.state_home, &record, true)
    })();
    render(common.format, "verify", result)
}

fn load_session_catalog(
    roots: &ResolvedRoots,
    product: Product,
    fallback: FallbackMode,
    use_user_config: bool,
    expected_integration_fingerprint: Option<&str>,
) -> Result<(integration::EffectiveCatalog, Option<String>), SessionFailure> {
    if !use_user_config {
        let catalog = load_catalog_from_roots(roots)
            .map_err(|err| SessionFailure::runtime("catalog-load-failed", err.to_string()))?;
        return Ok((
            integration::EffectiveCatalog {
                catalog,
                private_project_catalog: false,
            },
            None,
        ));
    }
    let (catalog, current) = integration::load_bound_catalog_with_fingerprint(
        roots,
        product,
        fallback,
        expected_integration_fingerprint,
    )
    .map_err(|err| match err.kind() {
        integration::BoundCatalogErrorKind::Data => {
            SessionFailure::data(err.code(), err.to_string())
        }
        integration::BoundCatalogErrorKind::Config => {
            SessionFailure::config(err.code(), err.to_string())
        }
        integration::BoundCatalogErrorKind::Runtime => {
            SessionFailure::runtime(err.code(), err.to_string())
        }
    })?;
    Ok((catalog, Some(current)))
}

fn new_record(
    common: &SessionCommonArgs,
    project: &Path,
    integration_fingerprint: Option<String>,
) -> SessionRecord {
    SessionRecord {
        schema: RECORD_SCHEMA.to_string(),
        session_hash: digest(common.session_id.trim().as_bytes()),
        project_hash: project_hash(project),
        product: common.product.as_str().to_string(),
        integration_fingerprint,
        active_intents: BTreeMap::new(),
        activated_at: jiff::Timestamp::now().to_string(),
        producer_version: env!("CARGO_PKG_VERSION").to_string(),
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
    Ok(SessionData {
        product: record.product.clone(),
        active_intents: record.active_intents.keys().cloned().collect(),
        record_file: record_file.to_string_lossy().replace('\\', "/"),
        verified,
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
    fn config(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            exit_code: EXIT_CONFIG,
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
