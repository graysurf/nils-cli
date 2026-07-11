use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::cli::{
    SessionActivateArgs, SessionArgs, SessionCommand, SessionCommonArgs, SessionVerifyArgs,
};
use crate::config::load_catalog_from_roots;
use crate::env::{PathOverrides, resolve_roots};
use crate::model::{Context, FallbackMode, OutputFormat};
use crate::resolver;

const RECORD_SCHEMA: &str = "agent-docs.session.v1";
const EXIT_OK: i32 = 0;
const EXIT_RUNTIME: i32 = 1;
const EXIT_DATA: i32 = 65;
const SECRET_MODE: u32 = 0o600;

#[derive(Debug, Serialize, Deserialize)]
struct SessionRecord {
    schema: String,
    session_hash: String,
    project_hash: String,
    product: String,
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

pub fn run(args: SessionArgs, overrides: PathOverrides, fallback: FallbackMode) -> i32 {
    match args.command {
        SessionCommand::Activate(args) => activate(args, overrides, fallback),
        SessionCommand::Status(args) => status(args, overrides),
        SessionCommand::Verify(args) => verify(args, overrides, fallback),
    }
}

fn activate(args: SessionActivateArgs, overrides: PathOverrides, fallback: FallbackMode) -> i32 {
    let common = &args.common;
    let result = (|| -> Result<SessionData, SessionFailure> {
        validate_common(common)?;
        let roots = resolve_roots(&overrides)
            .map_err(|err| SessionFailure::runtime("root-resolution-failed", err.to_string()))?;
        let catalog = load_catalog_from_roots(&roots)
            .map_err(|err| SessionFailure::runtime("catalog-load-failed", err.to_string()))?;
        let available = resolver::declared_intents(&roots, fallback, &catalog);
        let path = record_path(common, &roots.project_path)?;
        let _lock = RecordLock::acquire(&path)?;
        let mut record = if path.exists() {
            read_record(&path)?
        } else {
            SessionRecord {
                schema: RECORD_SCHEMA.to_string(),
                session_hash: digest(common.session_id.trim().as_bytes()),
                project_hash: project_hash(&roots.project_path),
                product: common.product.as_str().to_string(),
                active_intents: BTreeMap::new(),
                activated_at: jiff::Timestamp::now().to_string(),
                producer_version: env!("CARGO_PKG_VERSION").to_string(),
            }
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
            let report = resolver::resolve_intent_with_catalog_for_product(
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
                .insert(intent.to_string(), fingerprint(&report, &catalog)?);
        }
        record.activated_at = jiff::Timestamp::now().to_string();
        write_record(&path, &record)?;
        Ok(data(&path, &record, true))
    })();
    render(common.format, "activate", result)
}

fn status(common: SessionCommonArgs, overrides: PathOverrides) -> i32 {
    let result = (|| -> Result<SessionData, SessionFailure> {
        validate_common(&common)?;
        let roots = resolve_roots(&overrides)
            .map_err(|err| SessionFailure::runtime("root-resolution-failed", err.to_string()))?;
        let path = record_path(&common, &roots.project_path)?;
        let record = read_record(&path)?;
        Ok(data(&path, &record, false))
    })();
    render(common.format, "status", result)
}

fn verify(args: SessionVerifyArgs, overrides: PathOverrides, fallback: FallbackMode) -> i32 {
    let common = &args.common;
    let result = (|| -> Result<SessionData, SessionFailure> {
        validate_common(common)?;
        let roots = resolve_roots(&overrides)
            .map_err(|err| SessionFailure::runtime("root-resolution-failed", err.to_string()))?;
        let catalog = load_catalog_from_roots(&roots)
            .map_err(|err| SessionFailure::runtime("catalog-load-failed", err.to_string()))?;
        let path = record_path(common, &roots.project_path)?;
        let record = read_record(&path)?;
        for raw in &args.require_intent {
            let intent =
                Context::parse(raw).map_err(|err| SessionFailure::data("invalid-intent", err))?;
            let Some(stored) = record.active_intents.get(intent.as_str()) else {
                return Err(SessionFailure::data(
                    "missing-intent",
                    format!("intent `{intent}` is not active"),
                ));
            };
            let report = resolver::resolve_intent_with_catalog_for_product(
                &intent,
                &roots,
                Some(common.product),
                true,
                fallback,
                true,
                &catalog,
            );
            if report.has_unsatisfied_required() || *stored != fingerprint(&report, &catalog)? {
                return Err(SessionFailure::data(
                    "stale-activation",
                    format!("activation for `{intent}` no longer matches the resolved catalog"),
                ));
            }
        }
        Ok(data(&path, &record, true))
    })();
    render(common.format, "verify", result)
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

fn data(path: &Path, record: &SessionRecord, verified: bool) -> SessionData {
    SessionData {
        product: record.product.clone(),
        active_intents: record.active_intents.keys().cloned().collect(),
        record_file: path.to_string_lossy().into_owned(),
        verified,
    }
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
                Ok(()) => return Ok(Self(lock)),
                Err(err)
                    if err.kind() == std::io::ErrorKind::AlreadyExists
                        && started.elapsed() < Duration::from_secs(30) =>
                {
                    thread::sleep(Duration::from_millis(10))
                }
                Err(err) => return Err(SessionFailure::runtime("lock-failed", err.to_string())),
            }
        }
    }
}

impl Drop for RecordLock {
    fn drop(&mut self) {
        let _ = fs::remove_dir(&self.0);
    }
}
