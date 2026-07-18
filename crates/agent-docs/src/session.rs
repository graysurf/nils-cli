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
use crate::model::{
    ConfigErrorKind, Context, FallbackMode, OutputFormat, PreflightReport, Product,
};
use crate::resolver;

const RECORD_SCHEMA: &str = "agent-docs.session.v2";
const LEGACY_RECORD_SCHEMA: &str = "agent-docs.session.v1";
const INTENT_FINGERPRINT_SCHEMA: &str = "agent-docs.session-intent-fingerprint.v1";
const EXIT_OK: i32 = 0;
const EXIT_RUNTIME: i32 = 4;
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

#[derive(Debug, Deserialize)]
struct SessionRecordEnvelope {
    schema: String,
}

enum DecodedRecord {
    Current(SessionRecord),
    LegacyV1,
}

#[derive(Debug, Serialize)]
struct SessionData {
    product: String,
    active_intents: Vec<String>,
    record_file: String,
    verified: bool,
}

#[derive(Serialize)]
struct IntentFingerprintInput<'a> {
    schema_version: &'static str,
    trusted_producer: TrustedProducerInput<'a>,
    fallback: &'static str,
    report_schema_version: &'static str,
    intent: &'a str,
    product: Option<&'static str>,
    strict: bool,
    docs_home: &'a Path,
    project_path: &'a Path,
    is_linked_worktree: bool,
    documents: Vec<ResolvedDocumentFingerprintInput<'a>>,
    validation: ValidationFingerprintInput<'a>,
    required_total: usize,
    satisfied_required: usize,
    missing_required: usize,
    invalid_required: usize,
}

#[derive(Serialize)]
struct TrustedProducerInput<'a> {
    kind: &'static str,
    binary: &'static str,
    package_version: &'static str,
    integration_fingerprint: Option<&'a str>,
    private_project_catalog: bool,
    private_allowed_roots: Vec<PathBuf>,
    catalogs: Vec<CatalogProducerInput<'a>>,
}

#[derive(Serialize)]
struct CatalogProducerInput<'a> {
    source_scope: &'static str,
    root: &'a Path,
    file_path: &'a Path,
    documents: Vec<CatalogDocumentFingerprintInput<'a>>,
    validations: Vec<CatalogValidationFingerprintInput<'a>>,
}

#[derive(Serialize)]
struct CatalogDocumentFingerprintInput<'a> {
    context: &'a str,
    scope: &'static str,
    path: &'a Path,
    products: Vec<&'static str>,
    required: bool,
    when: &'a str,
    marker: Option<&'a str>,
    freshness_days: Option<u64>,
}

#[derive(Serialize)]
struct CatalogValidationFingerprintInput<'a> {
    context: &'a str,
    products: Vec<&'static str>,
    commands: &'a [String],
    marker: Option<&'a str>,
}

#[derive(Serialize)]
struct ResolvedDocumentFingerprintInput<'a> {
    context: &'a str,
    scope: &'static str,
    path: &'a Path,
    products: Vec<&'static str>,
    declared_required: bool,
    required: bool,
    when: &'a str,
    when_satisfied: bool,
    status: &'static str,
    exists: bool,
    non_empty: bool,
    marker_present: Option<bool>,
    freshness: &'static str,
    valid: bool,
    source: &'static str,
    content_digest: Option<String>,
}

#[derive(Serialize)]
struct ValidationFingerprintInput<'a> {
    context: &'a str,
    declared: bool,
    commands: &'a [String],
    marker: Option<&'a str>,
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
        SessionCommand::Status(args) => status(
            args,
            overrides,
            fallback,
            use_user_config,
            expected_integration_fingerprint.as_deref(),
        ),
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
        let roots = resolve_session_roots(&overrides)?;
        let path = record_path(common, &roots.project_path)?;
        let _lock = RecordLock::acquire(&path)?;
        let existing = if path.exists() {
            match decode_record(&path)? {
                DecodedRecord::Current(record) => {
                    validate_record_context(common, &roots.project_path, &record)?;
                    Some(record)
                }
                DecodedRecord::LegacyV1 => None,
            }
        } else {
            None
        };
        let (catalog, integration_fingerprint) = load_session_catalog(
            &roots,
            common.product,
            fallback,
            use_user_config,
            expected_integration_fingerprint,
        )?;
        let available = resolver::declared_intents(&roots, fallback, &catalog.catalog);
        let mut record = existing.unwrap_or_else(|| {
            new_record(common, &roots.project_path, integration_fingerprint.clone())
        });
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
            record.active_intents.insert(
                intent.to_string(),
                fingerprint(
                    &report,
                    &catalog,
                    fallback,
                    integration_fingerprint.as_deref(),
                )?,
            );
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
    fallback: FallbackMode,
    use_user_config: bool,
    expected_integration_fingerprint: Option<&str>,
) -> i32 {
    let result = (|| -> Result<SessionData, SessionFailure> {
        validate_common(&common)?;
        let roots = resolve_session_roots(&overrides)?;
        let path = record_path(&common, &roots.project_path)?;
        let record = read_record(&path)?;
        validate_record_context(&common, &roots.project_path, &record)?;
        if use_user_config {
            let (_, current) = load_session_catalog(&roots, common.product, fallback, true, None)?;
            validate_integration_fingerprints(
                record.integration_fingerprint.as_deref(),
                current.as_deref(),
                expected_integration_fingerprint,
            )?;
        } else if let Some(expected) = expected_integration_fingerprint
            && record.integration_fingerprint.as_deref() != Some(expected)
        {
            return Err(stale_integration_decision(
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
        let roots = resolve_session_roots(&overrides)?;
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
                || *stored
                    != fingerprint(
                        &report,
                        &catalog,
                        fallback,
                        integration_fingerprint.as_deref(),
                    )?
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

fn resolve_session_roots(overrides: &PathOverrides) -> Result<ResolvedRoots, SessionFailure> {
    resolve_roots(overrides)
        .map_err(|err| SessionFailure::runtime("root-resolution-failed", err.to_string()))
}

fn stale_integration_decision(message: impl Into<String>) -> SessionFailure {
    SessionFailure::data("stale-integration-decision", message)
}

fn validate_integration_fingerprints(
    stored: Option<&str>,
    current: Option<&str>,
    expected: Option<&str>,
) -> Result<(), SessionFailure> {
    if let Some(expected) = expected
        && current != Some(expected)
    {
        return Err(stale_integration_decision(
            "the current integration decision does not match the requested fingerprint",
        ));
    }
    if stored != current {
        return Err(stale_integration_decision(
            "session activation does not match the current integration decision",
        ));
    }
    Ok(())
}

fn load_session_catalog(
    roots: &ResolvedRoots,
    product: Product,
    fallback: FallbackMode,
    use_user_config: bool,
    expected_integration_fingerprint: Option<&str>,
) -> Result<(integration::EffectiveCatalog, Option<String>), SessionFailure> {
    if !use_user_config {
        let catalog = load_catalog_from_roots(roots).map_err(|err| match err.kind {
            ConfigErrorKind::Parse | ConfigErrorKind::Validation => {
                SessionFailure::config("catalog-load-failed", err.to_string())
            }
            ConfigErrorKind::Io => SessionFailure::runtime("catalog-load-failed", err.to_string()),
        })?;
        return Ok((
            integration::EffectiveCatalog {
                catalog,
                private_project_catalog: false,
                private_allowed_roots: Vec::new(),
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

fn fingerprint(
    report: &PreflightReport,
    catalog: &integration::EffectiveCatalog,
    fallback: FallbackMode,
    integration_fingerprint: Option<&str>,
) -> Result<String, SessionFailure> {
    if catalog.private_project_catalog && integration_fingerprint.is_none() {
        return Err(SessionFailure::runtime(
            "fingerprint-producer-missing",
            "private session activation is missing its trusted integration decision",
        ));
    }

    let mut private_allowed_roots = catalog.private_allowed_roots.clone();
    private_allowed_roots.sort();
    let catalogs = catalog
        .catalog
        .in_load_order()
        .into_iter()
        .map(|scope| CatalogProducerInput {
            source_scope: scope.source_scope.as_str(),
            root: &scope.root,
            file_path: &scope.file_path,
            documents: scope
                .documents
                .iter()
                .map(|document| CatalogDocumentFingerprintInput {
                    context: document.context.as_str(),
                    scope: document.scope.as_str(),
                    path: &document.path,
                    products: product_names(&document.products),
                    required: document.required,
                    when: &document.when_raw,
                    marker: document.marker.as_deref(),
                    freshness_days: document.freshness_days,
                })
                .collect(),
            validations: scope
                .validations
                .iter()
                .map(|validation| CatalogValidationFingerprintInput {
                    context: validation.context.as_str(),
                    products: product_names(&validation.products),
                    commands: &validation.commands,
                    marker: validation.marker.as_deref(),
                })
                .collect(),
        })
        .collect();
    let documents = report
        .documents
        .iter()
        .map(|document| ResolvedDocumentFingerprintInput {
            context: document.context.as_str(),
            scope: document.scope.as_str(),
            path: &document.path,
            products: product_names(&document.products),
            declared_required: document.declared_required,
            required: document.required,
            when: &document.when,
            when_satisfied: document.when_satisfied,
            status: document.status.as_str(),
            exists: document.validation.exists,
            non_empty: document.validation.non_empty,
            marker_present: document.validation.marker_present,
            freshness: document.validation.freshness.as_str(),
            valid: document.validation.valid,
            source: document.source.as_str(),
            content_digest: document
                .content
                .as_deref()
                .map(|content| digest(content.as_bytes())),
        })
        .collect();
    let input = IntentFingerprintInput {
        schema_version: INTENT_FINGERPRINT_SCHEMA,
        trusted_producer: TrustedProducerInput {
            kind: if integration_fingerprint.is_some() {
                "integration-decision"
            } else {
                "catalog-roots"
            },
            binary: "agent-docs",
            package_version: env!("CARGO_PKG_VERSION"),
            integration_fingerprint,
            private_project_catalog: catalog.private_project_catalog,
            private_allowed_roots,
            catalogs,
        },
        fallback: fallback.as_str(),
        report_schema_version: report.schema_version,
        intent: report.intent.as_str(),
        product: report.product.map(Product::as_str),
        strict: report.strict,
        docs_home: &report.docs_home,
        project_path: &report.project_path,
        is_linked_worktree: report.is_linked_worktree,
        documents,
        validation: ValidationFingerprintInput {
            context: report.validation.context.as_str(),
            declared: report.validation.declared,
            commands: &report.validation.commands,
            marker: report.validation.marker.as_deref(),
        },
        required_total: report.summary.required_total,
        satisfied_required: report.summary.satisfied_required,
        missing_required: report.summary.missing_required,
        invalid_required: report.summary.invalid_required,
    };
    let bytes = serde_json::to_vec(&input)
        .map_err(|err| SessionFailure::runtime("fingerprint-failed", err.to_string()))?;
    Ok(digest(&bytes))
}

fn product_names(products: &[Product]) -> Vec<&'static str> {
    products.iter().copied().map(Product::as_str).collect()
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
    match decode_record(path)? {
        DecodedRecord::Current(record) => Ok(record),
        DecodedRecord::LegacyV1 => Err(unsupported_record(LEGACY_RECORD_SCHEMA)),
    }
}

fn decode_record(path: &Path) -> Result<DecodedRecord, SessionFailure> {
    let raw = fs::read(path).map_err(|err| {
        let code = if err.kind() == std::io::ErrorKind::NotFound {
            "missing-activation"
        } else {
            "record-read-failed"
        };
        SessionFailure::data(code, format!("session activation is unavailable: {err}"))
    })?;
    let envelope: SessionRecordEnvelope = serde_json::from_slice(&raw)
        .map_err(|err| SessionFailure::runtime("record-parse-failed", err.to_string()))?;
    match envelope.schema.as_str() {
        RECORD_SCHEMA => serde_json::from_slice(&raw)
            .map(DecodedRecord::Current)
            .map_err(|err| SessionFailure::runtime("record-parse-failed", err.to_string())),
        LEGACY_RECORD_SCHEMA => Ok(DecodedRecord::LegacyV1),
        unsupported => Err(unsupported_record(unsupported)),
    }
}

fn unsupported_record(schema: &str) -> SessionFailure {
    SessionFailure::data(
        "unsupported-record",
        format!("unsupported session schema `{schema}`"),
    )
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
