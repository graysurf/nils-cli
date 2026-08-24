use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

use nils_common::cli_contract::{Envelope, EnvelopeError};

use crate::cli::{
    SessionActivateArgs, SessionArgs, SessionCommand, SessionCommitPrerequisiteArgs,
    SessionCommonArgs, SessionContextArgs, SessionPrerequisiteArgs, SessionPrerequisiteBindingArgs,
    SessionVerifyArgs,
};
use crate::config::{load_catalog_from_roots, load_dsh_catalog_from_roots};
use crate::env::{PathOverrides, ResolvedRoots, resolve_roots};
use crate::integration;
use crate::model::{
    ConfigErrorKind, Context, FallbackMode, OutputFormat, Phase, PreflightReport, Product,
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
const MAX_AVAILABLE_INTENTS: usize = 32;
const MAX_RECOVERY_INTENTS: usize = 16;
const MAX_RECOVERY_IDENTIFIER_BYTES: usize = 128;
const BOUNDED_RETRY_ATTEMPTS: u8 = 3;
const MAX_CONTEXT_REQUEST_ID_BYTES: usize = 128;
const MAX_CONTEXT_BYTES: usize = 64 * 1024;
const CONTEXT_DECISION_SCHEMA: &str = "decision.context.v1";
const CONTEXT_FINGERPRINT_PREFIX: &str = "dsh-context-v1:";
const PREREQUISITE_DECISION_SCHEMA: &str = "decision.prerequisite.v1";
const PREREQUISITE_RECEIPT_SCHEMA: &str = "agent-docs.prerequisite-receipt.v1";
const MAX_PREREQUISITE_BINDING_BYTES: usize = 256;
const MAX_PREREQUISITE_RECEIPT_BYTES: usize = 4096;

#[derive(Debug, Serialize, Deserialize)]
struct SessionRecord {
    schema: String,
    session_hash: String,
    project_hash: String,
    product: String,
    #[serde(default)]
    integration_fingerprint: Option<String>,
    active_intents: BTreeMap<String, String>,
    /// Phase-scoped activations: intent -> { phase -> fingerprint }. Skipped
    /// when empty so a no-phase record serializes byte-identically to a
    /// pre-phase record (and keeps its fingerprint stable across the upgrade).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    active_phase_intents: BTreeMap<String, BTreeMap<String, String>>,
    activated_at: String,
    producer_version: String,
}

#[derive(Debug, Deserialize)]
struct SessionRecordEnvelope {
    schema: String,
}

enum DecodedRecord {
    Current(Box<SessionRecord>),
    LegacyV1,
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

#[derive(Debug, Serialize)]
struct ContextData {
    decision: ContextDecision,
}

#[derive(Debug, Serialize)]
struct ContextDecision {
    schema_version: &'static str,
    request_id: String,
    product: &'static str,
    intent: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    phase: Option<String>,
    reason: &'static str,
    verified: bool,
    documents: Vec<ContextDocument>,
    document_count: usize,
    total_bytes: usize,
}

#[derive(Debug, Serialize)]
struct ContextDocument {
    source: &'static str,
    scope: &'static str,
    content: String,
}

#[derive(Debug, Serialize)]
struct PrerequisiteData {
    decision: PrerequisiteDecision,
}

#[derive(Debug, Serialize)]
struct PrerequisiteDecision {
    schema_version: &'static str,
    request_id: String,
    product: &'static str,
    intent: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    phase: Option<String>,
    reason: &'static str,
    verified: bool,
    documents: Vec<ContextDocument>,
    document_count: usize,
    total_bytes: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    receipt: Option<String>,
}

#[derive(Debug, Serialize)]
struct PrerequisiteCommitData {
    product: &'static str,
    intent: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    phase: Option<String>,
    reason: &'static str,
    verified: bool,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PrerequisiteReceipt {
    schema_version: String,
    session_hash: String,
    state_home_hash: String,
    project_hash: String,
    product: String,
    intent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    phase: Option<String>,
    agent_hash: String,
    workspace_generation_hash: String,
    call_hash: String,
    turn: u64,
    step: u64,
    tool_hash: String,
    definition_hash: String,
    context_fingerprint: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum NextAction {
    FixArguments,
    ListDeclaredIntents,
    InspectPreflight,
    PrepareIntent,
    RefreshIntegrationDecision,
    RepairCatalog,
    RetryBounded,
    InspectSessionState,
    UpgradeAgentDocs,
    ReportInvariant,
}

impl NextAction {
    const fn as_str(self) -> &'static str {
        match self {
            Self::FixArguments => "fix-arguments",
            Self::ListDeclaredIntents => "list-declared-intents",
            Self::InspectPreflight => "inspect-preflight",
            Self::PrepareIntent => "prepare-intent",
            Self::RefreshIntegrationDecision => "refresh-integration-decision",
            Self::RepairCatalog => "repair-catalog",
            Self::RetryBounded => "retry-bounded",
            Self::InspectSessionState => "inspect-session-state",
            Self::UpgradeAgentDocs => "upgrade-agent-docs",
            Self::ReportInvariant => "report-invariant",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
enum RecoveryCommand {
    #[serde(rename = "audit")]
    Audit,
    #[serde(rename = "integration.resolve")]
    IntegrationResolve,
    #[serde(rename = "list")]
    List,
    #[serde(rename = "preflight")]
    Preflight,
    #[serde(rename = "session.prepare")]
    SessionPrepare,
    #[serde(rename = "session.context")]
    SessionContext,
    #[serde(rename = "session.prerequisite")]
    SessionPrerequisite,
    #[serde(rename = "session.status")]
    SessionStatus,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "kebab-case")]
enum RecoveryAction {
    FixArguments,
    RepairCatalog,
    ReportInvariant,
    RetryCommand,
    UpgradeAgentDocs,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum ReuseField {
    SessionId,
    Product,
    StateHome,
    DocsHome,
    ProjectPath,
    UserConfig,
}

#[derive(Debug, Serialize)]
struct SessionRecovery {
    #[serde(skip_serializing_if = "Option::is_none")]
    command: Option<RecoveryCommand>,
    #[serde(skip_serializing_if = "Option::is_none")]
    action: Option<RecoveryAction>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    reuse_scope: Vec<ReuseField>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    intents: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    phase: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    refresh_integration_fingerprint: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    then: Option<RecoveryCommand>,
    #[serde(skip_serializing_if = "Option::is_none")]
    retry_original: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_attempts: Option<u8>,
}

#[derive(Debug, Serialize)]
struct PreflightDiagnostics {
    required_total: usize,
    satisfied_required: usize,
    missing_required: usize,
    invalid_required: usize,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "kebab-case")]
enum RecordRelation {
    PriorVersionReplaceable,
    Future,
    Unrecognized,
}

#[derive(Debug, Serialize)]
struct SessionFailureDetails {
    retryable: bool,
    next_action: NextAction,
    recovery: SessionRecovery,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    available_intents: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    diagnostics: Option<PreflightDiagnostics>,
    #[serde(skip_serializing_if = "Option::is_none")]
    record_relation: Option<RecordRelation>,
}

#[derive(Serialize)]
struct IntentFingerprintInput<'a> {
    schema_version: &'static str,
    trusted_producer: TrustedProducerInput<'a>,
    fallback: &'static str,
    report_schema_version: &'static str,
    intent: &'a str,
    product: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    phase: Option<&'a str>,
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
    phases: Vec<&'a str>,
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
        SessionCommand::Prepare(args) => prepare(
            args,
            overrides,
            fallback,
            use_user_config,
            expected_integration_fingerprint.as_deref(),
        ),
        SessionCommand::Context(args) => context(
            args,
            overrides,
            fallback,
            use_user_config,
            expected_integration_fingerprint.as_deref(),
        ),
        SessionCommand::Prerequisite(args) => prerequisite(
            args,
            overrides,
            fallback,
            use_user_config,
            expected_integration_fingerprint.as_deref(),
        ),
        SessionCommand::CommitPrerequisite(args) => commit_prerequisite(
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

/// Resolve one DSH intent, validate the complete response budget, and only
/// then persist its activation while the same per-scope record lock is held.
/// The success DTO deliberately excludes catalog metadata, filesystem paths,
/// validation commands, session state paths, and the raw session identifier.
fn context(
    args: SessionContextArgs,
    overrides: PathOverrides,
    fallback: FallbackMode,
    use_user_config: bool,
    expected_integration_fingerprint: Option<&str>,
) -> i32 {
    let product_label = args.product.as_str();
    let result = (|| -> Result<ContextData, SessionFailure> {
        validate_session_scope(&args.session_id, &args.state_home)?;
        validate_context_args(&args)?;
        let phase = parse_phase_arg(args.phase.as_deref())?;
        let roots = resolve_session_roots(&overrides)?;
        let path = record_path_for(
            &args.session_id,
            &args.state_home,
            product_label,
            &roots.project_path,
        )?;
        let _lock = RecordLock::acquire(&path)?;
        let existing = if path.exists() {
            match decode_record(&path)? {
                DecodedRecord::Current(record) => {
                    validate_record_identity(
                        &args.session_id,
                        product_label,
                        &roots.project_path,
                        &record,
                    )?;
                    Some(*record)
                }
                DecodedRecord::LegacyV1 => None,
            }
        } else {
            None
        };
        let (catalog, integration_fingerprint) = load_dsh_session_catalog(
            &roots,
            fallback,
            use_user_config,
            expected_integration_fingerprint,
        )
        .map_err(|failure| failure.for_dsh_context(&args.intent, phase.as_ref()))?;
        let available = resolver::declared_intents(&roots, fallback, &catalog.catalog);
        let intent = Context::parse(&args.intent).map_err(|err| {
            SessionFailure::data("invalid-intent", err).with_available_intents(&available)
        })?;
        if !available.iter().any(|item| item == intent.as_str()) {
            return Err(SessionFailure::data(
                "undeclared-intent",
                format!("intent `{intent}` is not declared"),
            )
            .with_available_intents(&available)
            .for_dsh_context(intent.as_str(), phase.as_ref()));
        }
        let report = resolver::resolve_dsh_context_with_effective_catalog_for_scope(
            &intent,
            &roots,
            phase.clone(),
            true,
            fallback,
            args.max_bytes,
            &catalog,
        )
        .map_err(context_resolve_failure)?;
        if report.has_unsatisfied_required() {
            return Err(SessionFailure::data(
                unsatisfied_code(phase.as_ref()),
                format!("strict preflight failed for `{intent}`"),
            )
            .with_preflight_context(&intent, phase.as_ref(), &report)
            .for_dsh_context(intent.as_str(), phase.as_ref()));
        }

        let mut total_bytes = 0usize;
        let mut documents = Vec::with_capacity(report.summary.satisfied_required);
        for document in report
            .documents
            .iter()
            .filter(|document| document.required && document.satisfied())
        {
            let content = document.content.clone().ok_or_else(|| {
                SessionFailure::runtime(
                    "context-content-missing",
                    "a satisfied required document omitted its content",
                )
            })?;
            total_bytes = total_bytes.checked_add(content.len()).ok_or_else(|| {
                SessionFailure::data(
                    "context-budget-exceeded",
                    "required policy content exceeds the requested response budget",
                )
            })?;
            if total_bytes > args.max_bytes {
                return Err(SessionFailure::data(
                    "context-budget-exceeded",
                    "required policy content exceeds the requested response budget",
                ));
            }
            documents.push(ContextDocument {
                source: document.source.as_str(),
                scope: document.scope.as_str(),
                content,
            });
        }

        let had_existing = existing.is_some();
        let mut record = existing.unwrap_or_else(|| {
            new_record_for(
                &args.session_id,
                product_label,
                &roots.project_path,
                integration_fingerprint.clone(),
            )
        });
        let mut changed = !had_existing;
        if record.integration_fingerprint != integration_fingerprint {
            record.active_intents.clear();
            record.active_phase_intents.clear();
            record.integration_fingerprint = integration_fingerprint.clone();
            changed = true;
        }
        changed |= store_activation(
            &mut record,
            &intent,
            phase.as_ref(),
            context_fingerprint(
                &report,
                &catalog,
                fallback,
                integration_fingerprint.as_deref(),
            )?,
        );
        let reason = if changed {
            record.activated_at = jiff::Timestamp::now().to_string();
            write_record(&path, &record)?;
            "prepared"
        } else {
            "already-current"
        };
        let document_count = documents.len();
        Ok(ContextData {
            decision: ContextDecision {
                schema_version: CONTEXT_DECISION_SCHEMA,
                request_id: args.request_id.clone(),
                product: product_label,
                intent: intent.to_string(),
                phase: phase.as_ref().map(ToString::to_string),
                reason,
                verified: true,
                documents,
                document_count,
                total_bytes,
            },
        })
    })();
    render_context(args.format, result)
}

/// Resolve one exact DSH prerequisite without mutating activation state. The
/// returned receipt is a bounded, replay-bound compare-and-swap input; it is
/// useful only with the same execution binding and current policy fingerprint.
fn prerequisite(
    args: SessionPrerequisiteArgs,
    overrides: PathOverrides,
    fallback: FallbackMode,
    use_user_config: bool,
    expected_integration_fingerprint: Option<&str>,
) -> i32 {
    let SessionPrerequisiteArgs {
        session_id,
        product,
        state_home,
        format,
        intent,
        phase,
        request_id,
        max_bytes,
        binding,
    } = args;
    let context = SessionContextArgs {
        session_id,
        product,
        state_home,
        format,
        intent,
        phase: Some(phase),
        request_id,
        max_bytes,
    };
    let result = (|| -> Result<PrerequisiteData, SessionFailure> {
        validate_prerequisite_selection(
            fallback,
            use_user_config,
            expected_integration_fingerprint,
        )?;
        validate_session_scope(&context.session_id, &context.state_home)?;
        validate_context_args(&context)?;
        validate_prerequisite_binding(&binding)?;
        let phase = parse_phase_arg(context.phase.as_deref())?;
        let roots = resolve_session_roots(&overrides)?;
        let path = record_path_for(
            &context.session_id,
            &context.state_home,
            context.product.as_str(),
            &roots.project_path,
        )?;
        let existing = if path.exists() {
            match decode_record(&path)? {
                DecodedRecord::Current(record) => {
                    validate_record_identity(
                        &context.session_id,
                        context.product.as_str(),
                        &roots.project_path,
                        &record,
                    )?;
                    Some(*record)
                }
                DecodedRecord::LegacyV1 => None,
            }
        } else {
            None
        };
        let (catalog, integration_fingerprint) = load_dsh_session_catalog(
            &roots,
            fallback,
            use_user_config,
            expected_integration_fingerprint,
        )
        .map_err(|failure| failure.for_dsh_context(&context.intent, phase.as_ref()))?;
        let available = resolver::declared_intents(&roots, fallback, &catalog.catalog);
        let intent = Context::parse(&context.intent).map_err(|err| {
            SessionFailure::data("invalid-intent", err).with_available_intents(&available)
        })?;
        if !available.iter().any(|item| item == intent.as_str()) {
            return Err(SessionFailure::data(
                "undeclared-intent",
                format!("intent `{intent}` is not declared"),
            )
            .with_available_intents(&available)
            .for_dsh_context(intent.as_str(), phase.as_ref()));
        }
        let report = resolver::resolve_dsh_context_with_effective_catalog_for_scope(
            &intent,
            &roots,
            phase.clone(),
            true,
            fallback,
            context.max_bytes,
            &catalog,
        )
        .map_err(context_resolve_failure)?;
        if report.has_unsatisfied_required() {
            return Err(SessionFailure::data(
                unsatisfied_code(phase.as_ref()),
                format!("strict preflight failed for `{intent}`"),
            )
            .with_preflight_context(&intent, phase.as_ref(), &report)
            .for_dsh_context(intent.as_str(), phase.as_ref()));
        }
        let (documents, total_bytes) = context_documents(&report, context.max_bytes)?;
        let fingerprint = context_fingerprint(
            &report,
            &catalog,
            fallback,
            integration_fingerprint.as_deref(),
        )?;
        let already_current = existing.as_ref().is_some_and(|record| {
            record.integration_fingerprint == integration_fingerprint
                && exact_context_activation_is_current(
                    &intent,
                    phase.as_ref(),
                    record,
                    &fingerprint,
                )
        });
        // Every call gets an execution-bound receipt, including reuse. A
        // prerequisite can wait behind downstream approval after this read;
        // commit-prerequisite must therefore re-resolve the policy immediately
        // before execution instead of trusting an earlier activation alone.
        let receipt = Some(encode_prerequisite_receipt(PrerequisiteReceipt {
            schema_version: PREREQUISITE_RECEIPT_SCHEMA.to_string(),
            session_hash: digest(context.session_id.trim().as_bytes()),
            state_home_hash: digest(context.state_home.to_string_lossy().as_bytes()),
            project_hash: project_hash(&roots.project_path),
            product: context.product.as_str().to_string(),
            intent: intent.to_string(),
            phase: phase.as_ref().map(ToString::to_string),
            agent_hash: digest(binding.agent_id.as_bytes()),
            workspace_generation_hash: digest(binding.workspace_generation.as_bytes()),
            call_hash: digest(binding.call_id.as_bytes()),
            turn: binding.turn,
            step: binding.step,
            tool_hash: digest(binding.tool_name.as_bytes()),
            definition_hash: digest(binding.definition_id.as_bytes()),
            context_fingerprint: fingerprint,
        })?);
        let document_count = documents.len();
        Ok(PrerequisiteData {
            decision: PrerequisiteDecision {
                schema_version: PREREQUISITE_DECISION_SCHEMA,
                request_id: context.request_id.clone(),
                product: context.product.as_str(),
                intent: intent.to_string(),
                phase: phase.as_ref().map(ToString::to_string),
                reason: if already_current {
                    "already-current"
                } else {
                    "pending"
                },
                verified: true,
                documents,
                document_count,
                total_bytes,
                receipt,
            },
        })
    })();
    render_prerequisite(format, result)
}

/// Commit an exact prerequisite receipt only if its execution binding and
/// freshly resolved policy fingerprint still match.
fn commit_prerequisite(
    args: SessionCommitPrerequisiteArgs,
    overrides: PathOverrides,
    fallback: FallbackMode,
    use_user_config: bool,
    expected_integration_fingerprint: Option<&str>,
) -> i32 {
    let result = (|| -> Result<PrerequisiteCommitData, SessionFailure> {
        validate_prerequisite_selection(
            fallback,
            use_user_config,
            expected_integration_fingerprint,
        )?;
        validate_session_scope(&args.session_id, &args.state_home)?;
        validate_prerequisite_binding(&args.binding)?;
        let receipt = decode_prerequisite_receipt(&args.receipt)?;
        let roots = resolve_session_roots(&overrides)?;
        validate_prerequisite_receipt_scope(
            &receipt,
            &args.session_id,
            &args.state_home,
            args.product.as_str(),
            &roots.project_path,
            &args.binding,
        )?;
        let intent =
            Context::parse(&receipt.intent).map_err(|_| prerequisite_receipt_mismatch())?;
        let phase = parse_phase_arg(receipt.phase.as_deref())
            .map_err(|_| prerequisite_receipt_mismatch())?;
        let path = record_path_for(
            &args.session_id,
            &args.state_home,
            args.product.as_str(),
            &roots.project_path,
        )?;
        let _lock = RecordLock::acquire(&path)?;
        let (catalog, integration_fingerprint) = load_dsh_session_catalog(
            &roots,
            fallback,
            use_user_config,
            expected_integration_fingerprint,
        )?;
        let available = resolver::declared_intents(&roots, fallback, &catalog.catalog);
        if !available.iter().any(|item| item == intent.as_str()) {
            return Err(SessionFailure::data(
                "prerequisite-stale",
                "the prerequisite intent is no longer declared",
            )
            .with_prepare_intent(&intent, phase.as_ref()));
        }
        let report = resolver::resolve_dsh_context_with_effective_catalog_for_scope(
            &intent,
            &roots,
            phase.clone(),
            true,
            fallback,
            MAX_CONTEXT_BYTES,
            &catalog,
        )
        .map_err(|_| {
            SessionFailure::data(
                "prerequisite-stale",
                "the prerequisite policy can no longer be resolved",
            )
            .with_prepare_intent(&intent, phase.as_ref())
        })?;
        if report.has_unsatisfied_required() {
            return Err(SessionFailure::data(
                "prerequisite-stale",
                "the prerequisite policy is no longer satisfied",
            )
            .with_prepare_intent(&intent, phase.as_ref()));
        }
        let current_fingerprint = context_fingerprint(
            &report,
            &catalog,
            fallback,
            integration_fingerprint.as_deref(),
        )?;
        if receipt.context_fingerprint != current_fingerprint {
            return Err(SessionFailure::data(
                "prerequisite-stale",
                "the prerequisite policy changed before commit",
            )
            .with_prepare_intent(&intent, phase.as_ref()));
        }

        let existing = if path.exists() {
            match decode_record(&path)? {
                DecodedRecord::Current(record) => {
                    validate_record_identity(
                        &args.session_id,
                        args.product.as_str(),
                        &roots.project_path,
                        &record,
                    )?;
                    Some(*record)
                }
                DecodedRecord::LegacyV1 => None,
            }
        } else {
            None
        };
        let had_existing = existing.is_some();
        let mut record = existing.unwrap_or_else(|| {
            new_record_for(
                &args.session_id,
                args.product.as_str(),
                &roots.project_path,
                integration_fingerprint.clone(),
            )
        });
        let mut changed = !had_existing;
        if record.integration_fingerprint != integration_fingerprint {
            record.active_intents.clear();
            record.active_phase_intents.clear();
            record.integration_fingerprint = integration_fingerprint;
            changed = true;
        }
        changed |= store_activation(&mut record, &intent, phase.as_ref(), current_fingerprint);
        let reason = if changed {
            record.activated_at = jiff::Timestamp::now().to_string();
            write_record(&path, &record)?;
            "prepared"
        } else {
            "already-current"
        };
        Ok(PrerequisiteCommitData {
            product: args.product.as_str(),
            intent: intent.to_string(),
            phase: phase.as_ref().map(ToString::to_string),
            reason,
            verified: true,
        })
    })();
    render_prerequisite_commit(args.format, result)
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
        let phase = parse_phase_arg(args.phase.as_deref())?;
        let roots = resolve_session_roots(&overrides)?;
        let path = record_path(common, &roots.project_path)?;
        let _lock = RecordLock::acquire(&path)?;
        let existing = if path.exists() {
            match decode_record(&path)? {
                DecodedRecord::Current(record) => {
                    validate_record_context(common, &roots.project_path, &record)?;
                    Some(*record)
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
            record.active_phase_intents.clear();
            record.integration_fingerprint = integration_fingerprint.clone();
        }
        for raw in &args.intent {
            let intent = Context::parse(raw).map_err(|err| {
                SessionFailure::data("invalid-intent", err).with_available_intents(&available)
            })?;
            if !available.iter().any(|item| item == intent.as_str()) {
                return Err(SessionFailure::data(
                    "undeclared-intent",
                    format!("intent `{intent}` is not declared"),
                )
                .with_available_intents(&available));
            }
            let report = resolver::resolve_intent_with_effective_catalog_for_scope(
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
                )
                .with_preflight_context(&intent, phase.as_ref(), &report));
            }
            store_activation(
                &mut record,
                &intent,
                phase.as_ref(),
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
fn prepare(
    args: SessionActivateArgs,
    overrides: PathOverrides,
    fallback: FallbackMode,
    use_user_config: bool,
    expected_integration_fingerprint: Option<&str>,
) -> i32 {
    let common = &args.common;
    let result = (|| -> Result<SessionData, SessionFailure> {
        validate_common(common)?;
        let phase = parse_phase_arg(args.phase.as_deref())?;
        let roots = resolve_session_roots(&overrides)?;
        let path = record_path(common, &roots.project_path)?;
        let _lock = RecordLock::acquire(&path)?;
        let existing = if path.exists() {
            match decode_record(&path)? {
                DecodedRecord::Current(record) => {
                    validate_record_context(common, &roots.project_path, &record)?;
                    Some(*record)
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
            record.active_phase_intents.clear();
            record.integration_fingerprint = integration_fingerprint.clone();
        }
        let mut prepared: Vec<String> = Vec::new();
        for raw in &args.intent {
            let intent = Context::parse(raw).map_err(|err| {
                SessionFailure::data("invalid-intent", err).with_available_intents(&available)
            })?;
            if !available.iter().any(|item| item == intent.as_str()) {
                return Err(SessionFailure::data(
                    "undeclared-intent",
                    format!("intent `{intent}` is not declared"),
                )
                .with_available_intents(&available));
            }
            let report = resolver::resolve_intent_with_effective_catalog_for_scope(
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
                )
                .with_preflight_context(&intent, phase.as_ref(), &report));
            }
            let fingerprint = fingerprint(
                &report,
                &catalog,
                fallback,
                integration_fingerprint.as_deref(),
            )?;
            if store_activation(&mut record, &intent, phase.as_ref(), fingerprint) {
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
        let phase = parse_phase_arg(args.phase.as_deref())?;
        let roots = resolve_session_roots(&overrides)?;
        let (catalog, integration_fingerprint) = load_session_catalog(
            &roots,
            common.product,
            fallback,
            use_user_config,
            expected_integration_fingerprint,
        )?;
        let available = resolver::declared_intents(&roots, fallback, &catalog.catalog);
        let recovery_intents = declared_requested_intents(&args.require_intent, &available);
        let path = record_path(common, &roots.project_path)?;
        let record = read_record(&path)
            .map_err(|failure| failure.with_prepare_context(&recovery_intents, phase.as_ref()))?;
        validate_record_context(common, &roots.project_path, &record)?;
        if record.integration_fingerprint != integration_fingerprint {
            return Err(SessionFailure::data(
                "stale-integration-decision",
                "session activation does not match the current integration decision",
            )
            .with_refresh_context(&recovery_intents, phase.as_ref()));
        }
        for raw in &args.require_intent {
            let intent = Context::parse(raw).map_err(|err| {
                SessionFailure::data("invalid-intent", err).with_available_intents(&available)
            })?;
            if !available.iter().any(|item| item == intent.as_str()) {
                return Err(SessionFailure::data(
                    "undeclared-intent",
                    "the required intent is not declared",
                )
                .with_available_intents(&available));
            }
            verify_intent(
                &intent,
                phase.as_ref(),
                &record,
                &roots,
                common.product,
                fallback,
                &catalog,
                integration_fingerprint.as_deref(),
            )?;
        }
        let mut out = data(&path, &common.state_home, &record, true)?;
        out.phase = phase.as_ref().map(|p| p.to_string());
        Ok(out)
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

fn load_dsh_session_catalog(
    roots: &ResolvedRoots,
    fallback: FallbackMode,
    use_user_config: bool,
    expected_integration_fingerprint: Option<&str>,
) -> Result<(integration::EffectiveCatalog, Option<String>), SessionFailure> {
    if !use_user_config {
        let catalog = load_dsh_catalog_from_roots(roots).map_err(|err| match err.kind {
            ConfigErrorKind::Parse | ConfigErrorKind::Validation => {
                SessionFailure::config("catalog-load-failed", err.to_string())
            }
            ConfigErrorKind::Io => SessionFailure::runtime("catalog-load-failed", err.to_string()),
        })?;
        return Ok((
            integration::EffectiveCatalog {
                catalog: catalog.into_loaded(),
                private_project_catalog: false,
                private_allowed_roots: Vec::new(),
            },
            None,
        ));
    }
    let (catalog, current) = integration::load_dsh_bound_catalog_with_fingerprint(
        roots,
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

pub(crate) fn dsh_session_intent_is_current(
    roots: &ResolvedRoots,
    session_id: &str,
    state_home: &Path,
    intent: &Context,
    phase: Option<&Phase>,
    fallback: FallbackMode,
) -> bool {
    (|| -> Result<bool, SessionFailure> {
        validate_session_scope(session_id, state_home)?;
        let path = record_path_for(session_id, state_home, "dsh", &roots.project_path)?;
        let _lock = RecordLock::acquire(&path)?;
        let record = read_record(&path)?;
        validate_record_identity(session_id, "dsh", &roots.project_path, &record)?;
        let (catalog, integration_fingerprint) =
            load_dsh_session_catalog(roots, fallback, false, None)?;
        if record.integration_fingerprint != integration_fingerprint {
            return Ok(false);
        }
        let available = resolver::declared_intents(roots, fallback, &catalog.catalog);
        if !available.iter().any(|item| item == intent.as_str()) {
            return Ok(false);
        }
        dsh_verify_intent(
            intent,
            phase,
            &record,
            roots,
            fallback,
            &catalog,
            integration_fingerprint.as_deref(),
        )
    })()
    .unwrap_or(false)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn dsh_prerequisite_receipt_is_current(
    roots: &ResolvedRoots,
    session_id: &str,
    state_home: &Path,
    intent: &Context,
    phase: Option<&Phase>,
    fallback: FallbackMode,
    receipt_raw: &str,
    agent_id: &str,
    workspace_generation: &str,
    call_id: &str,
    turn: u64,
    step: u64,
    tool_name: &str,
    definition_id: &str,
) -> bool {
    (|| -> Result<bool, SessionFailure> {
        validate_session_scope(session_id, state_home)?;
        let binding = SessionPrerequisiteBindingArgs {
            agent_id: agent_id.to_string(),
            workspace_generation: workspace_generation.to_string(),
            call_id: call_id.to_string(),
            turn,
            step,
            tool_name: tool_name.to_string(),
            definition_id: definition_id.to_string(),
        };
        validate_prerequisite_binding(&binding)?;
        let receipt = decode_prerequisite_receipt(receipt_raw)?;
        validate_prerequisite_receipt_scope(
            &receipt,
            session_id,
            state_home,
            "dsh",
            &roots.project_path,
            &binding,
        )?;
        if receipt.intent != intent.as_str() || receipt.phase.as_deref() != phase.map(Phase::as_str)
        {
            return Ok(false);
        }
        let (catalog, integration_fingerprint) =
            load_dsh_session_catalog(roots, fallback, false, None)?;
        let available = resolver::declared_intents(roots, fallback, &catalog.catalog);
        if !available.iter().any(|item| item == intent.as_str()) {
            return Ok(false);
        }
        let report = resolver::resolve_dsh_context_with_effective_catalog_for_scope(
            intent,
            roots,
            phase.cloned(),
            true,
            fallback,
            MAX_CONTEXT_BYTES,
            &catalog,
        )
        .map_err(verification_context_failure)?;
        if report.has_unsatisfied_required() {
            return Ok(false);
        }
        Ok(receipt.context_fingerprint
            == context_fingerprint(
                &report,
                &catalog,
                fallback,
                integration_fingerprint.as_deref(),
            )?)
    })()
    .unwrap_or(false)
}

fn dsh_verify_intent(
    intent: &Context,
    phase: Option<&Phase>,
    record: &SessionRecord,
    roots: &ResolvedRoots,
    fallback: FallbackMode,
    catalog: &integration::EffectiveCatalog,
    integration_fingerprint: Option<&str>,
) -> Result<bool, SessionFailure> {
    let matches = |stored: &str, phase: Option<&Phase>| {
        dsh_activation_matches(
            intent,
            phase,
            stored,
            roots,
            fallback,
            catalog,
            integration_fingerprint,
        )
    };
    let Some(phase) = phase else {
        return match record.active_intents.get(intent.as_str()) {
            Some(stored) => matches(stored, None),
            None => Ok(false),
        };
    };
    if let Some(stored) = record
        .active_phase_intents
        .get(intent.as_str())
        .and_then(|phases| phases.get(phase.as_str()))
        && matches(stored, Some(phase))?
    {
        return Ok(true);
    }
    match record.active_intents.get(intent.as_str()) {
        Some(stored) => matches(stored, None),
        None => Ok(false),
    }
}

/// Decide prerequisite reuse from the exact report fingerprint already
/// returned to the caller. A full activation still covers phase verification,
/// but it cannot prove that separately returned phase-scoped content came from
/// the same filesystem snapshot. The first phase prerequisite therefore
/// materializes its own phase activation and later calls reuse it directly.
fn exact_context_activation_is_current(
    intent: &Context,
    phase: Option<&Phase>,
    record: &SessionRecord,
    fingerprint: &str,
) -> bool {
    match phase {
        Some(phase) => record
            .active_phase_intents
            .get(intent.as_str())
            .and_then(|phases| phases.get(phase.as_str()))
            .is_some_and(|stored| stored == fingerprint),
        None => record
            .active_intents
            .get(intent.as_str())
            .is_some_and(|stored| stored == fingerprint),
    }
}

#[allow(clippy::too_many_arguments)]
fn dsh_activation_matches(
    intent: &Context,
    phase: Option<&Phase>,
    stored: &str,
    roots: &ResolvedRoots,
    fallback: FallbackMode,
    catalog: &integration::EffectiveCatalog,
    integration_fingerprint: Option<&str>,
) -> Result<bool, SessionFailure> {
    if !stored.starts_with(CONTEXT_FINGERPRINT_PREFIX) {
        return Ok(false);
    }
    let report = resolver::resolve_dsh_context_with_effective_catalog_for_scope(
        intent,
        roots,
        phase.cloned(),
        true,
        fallback,
        MAX_CONTEXT_BYTES,
        catalog,
    )
    .map_err(verification_context_failure)?;
    Ok(!report.has_unsatisfied_required()
        && stored == context_fingerprint(&report, catalog, fallback, integration_fingerprint)?)
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
    catalog: &integration::EffectiveCatalog,
    integration_fingerprint: Option<&str>,
) -> Result<(), SessionFailure> {
    let Some(phase) = phase else {
        let Some(stored) = record.active_intents.get(intent.as_str()) else {
            return Err(SessionFailure::data(
                "missing-intent",
                format!("intent `{intent}` is not active"),
            )
            .with_prepare_intent(intent, None));
        };
        if !activation_matches(
            intent,
            None,
            stored,
            roots,
            product,
            fallback,
            catalog,
            integration_fingerprint,
        )? {
            return Err(SessionFailure::data(
                "stale-activation",
                format!("activation for `{intent}` no longer matches the resolved catalog"),
            )
            .with_prepare_intent(intent, None));
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
        )
        .with_prepare_intent(intent, Some(phase)));
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
            integration_fingerprint,
        )?
    {
        return Ok(());
    }
    if let Some(stored) = full_stored
        && activation_matches(
            intent,
            None,
            stored,
            roots,
            product,
            fallback,
            catalog,
            integration_fingerprint,
        )?
    {
        return Ok(());
    }
    Err(SessionFailure::data(
        "stale-activation",
        format!(
            "activation for `{intent}` no longer matches the resolved catalog for phase `{phase}`"
        ),
    )
    .with_prepare_intent(intent, Some(phase)))
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
    catalog: &integration::EffectiveCatalog,
    integration_fingerprint: Option<&str>,
) -> Result<bool, SessionFailure> {
    if stored.starts_with(CONTEXT_FINGERPRINT_PREFIX) {
        return Ok(false);
    }
    let report = resolver::resolve_intent_with_effective_catalog_for_scope(
        intent,
        roots,
        Some(product),
        phase.cloned(),
        true,
        fallback,
        true,
        catalog,
    );
    Ok(!report.has_unsatisfied_required()
        && stored == fingerprint(&report, catalog, fallback, integration_fingerprint)?)
}

fn context_resolve_failure(error: resolver::ContextResolveError) -> SessionFailure {
    SessionFailure::data(error.code(), error.message())
}

fn context_documents(
    report: &PreflightReport,
    max_bytes: usize,
) -> Result<(Vec<ContextDocument>, usize), SessionFailure> {
    let mut total_bytes = 0usize;
    let mut documents = Vec::with_capacity(report.summary.satisfied_required);
    for document in report
        .documents
        .iter()
        .filter(|document| document.required && document.satisfied())
    {
        let content = document.content.clone().ok_or_else(|| {
            SessionFailure::runtime(
                "context-content-missing",
                "a satisfied required document omitted its content",
            )
        })?;
        total_bytes = total_bytes.checked_add(content.len()).ok_or_else(|| {
            SessionFailure::data(
                "context-budget-exceeded",
                "required policy content exceeds the requested response budget",
            )
        })?;
        if total_bytes > max_bytes {
            return Err(SessionFailure::data(
                "context-budget-exceeded",
                "required policy content exceeds the requested response budget",
            ));
        }
        documents.push(ContextDocument {
            source: document.source.as_str(),
            scope: document.scope.as_str(),
            content,
        });
    }
    Ok((documents, total_bytes))
}

fn encode_prerequisite_receipt(receipt: PrerequisiteReceipt) -> Result<String, SessionFailure> {
    let encoded = serde_json::to_string(&receipt).map_err(|_| {
        SessionFailure::runtime(
            "prerequisite-receipt-render-failed",
            "the prerequisite receipt could not be serialized",
        )
    })?;
    if encoded.len() > MAX_PREREQUISITE_RECEIPT_BYTES {
        return Err(SessionFailure::runtime(
            "prerequisite-receipt-render-failed",
            "the prerequisite receipt exceeded its bounded contract",
        ));
    }
    Ok(encoded)
}

fn decode_prerequisite_receipt(raw: &str) -> Result<PrerequisiteReceipt, SessionFailure> {
    if raw.is_empty() || raw.len() > MAX_PREREQUISITE_RECEIPT_BYTES {
        return Err(SessionFailure::data(
            "prerequisite-receipt-invalid",
            "the prerequisite receipt is missing or oversized",
        ));
    }
    let receipt: PrerequisiteReceipt = serde_json::from_str(raw).map_err(|_| {
        SessionFailure::data(
            "prerequisite-receipt-invalid",
            "the prerequisite receipt is malformed",
        )
    })?;
    let digests = [
        receipt.session_hash.as_str(),
        receipt.state_home_hash.as_str(),
        receipt.project_hash.as_str(),
        receipt.agent_hash.as_str(),
        receipt.workspace_generation_hash.as_str(),
        receipt.call_hash.as_str(),
        receipt.tool_hash.as_str(),
        receipt.definition_hash.as_str(),
    ];
    if receipt.schema_version != PREREQUISITE_RECEIPT_SCHEMA
        || receipt.product != "dsh"
        || Context::parse(&receipt.intent).is_err()
        || receipt
            .phase
            .as_deref()
            .is_some_and(|phase| Phase::parse(phase).is_err())
        || receipt.turn == 0
        || receipt.step == 0
        || !digests.into_iter().all(valid_sha256_digest)
        || !receipt
            .context_fingerprint
            .starts_with(CONTEXT_FINGERPRINT_PREFIX)
        || !valid_sha256_digest(
            receipt
                .context_fingerprint
                .strip_prefix(CONTEXT_FINGERPRINT_PREFIX)
                .unwrap_or_default(),
        )
    {
        return Err(SessionFailure::data(
            "prerequisite-receipt-invalid",
            "the prerequisite receipt fields are invalid",
        ));
    }
    Ok(receipt)
}

fn valid_sha256_digest(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("sha256:")
        && value[7..].bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn validate_prerequisite_receipt_scope(
    receipt: &PrerequisiteReceipt,
    session_id: &str,
    state_home: &Path,
    product: &str,
    project: &Path,
    binding: &SessionPrerequisiteBindingArgs,
) -> Result<(), SessionFailure> {
    let matches = receipt.session_hash == digest(session_id.trim().as_bytes())
        && receipt.state_home_hash == digest(state_home.to_string_lossy().as_bytes())
        && receipt.project_hash == project_hash(project)
        && receipt.product == product
        && receipt.agent_hash == digest(binding.agent_id.as_bytes())
        && receipt.workspace_generation_hash == digest(binding.workspace_generation.as_bytes())
        && receipt.call_hash == digest(binding.call_id.as_bytes())
        && receipt.turn == binding.turn
        && receipt.step == binding.step
        && receipt.tool_hash == digest(binding.tool_name.as_bytes())
        && receipt.definition_hash == digest(binding.definition_id.as_bytes());
    if matches {
        Ok(())
    } else {
        Err(prerequisite_receipt_mismatch())
    }
}

fn prerequisite_receipt_mismatch() -> SessionFailure {
    SessionFailure::data(
        "prerequisite-receipt-mismatch",
        "the prerequisite receipt belongs to a different execution scope",
    )
}

fn verification_context_failure(_error: resolver::ContextResolveError) -> SessionFailure {
    SessionFailure::data(
        "context-policy-invalid",
        "the prepared DSH context can no longer be resolved within its safety limits",
    )
}

fn parse_phase_arg(raw: Option<&str>) -> Result<Option<Phase>, SessionFailure> {
    match raw {
        None => Ok(None),
        Some(raw) => Phase::parse(raw)
            .map(Some)
            .map_err(|err| SessionFailure::data("invalid-phase", err)),
    }
}

fn new_record(
    common: &SessionCommonArgs,
    project: &Path,
    integration_fingerprint: Option<String>,
) -> SessionRecord {
    new_record_for(
        &common.session_id,
        common.product.as_str(),
        project,
        integration_fingerprint,
    )
}

fn new_record_for(
    session_id: &str,
    product_label: &str,
    project: &Path,
    integration_fingerprint: Option<String>,
) -> SessionRecord {
    SessionRecord {
        schema: RECORD_SCHEMA.to_string(),
        session_hash: digest(session_id.trim().as_bytes()),
        project_hash: project_hash(project),
        product: product_label.to_string(),
        integration_fingerprint,
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
    validate_session_scope(&common.session_id, &common.state_home)
}

fn validate_session_scope(session_id: &str, state_home: &Path) -> Result<(), SessionFailure> {
    if session_id.trim().is_empty() {
        return Err(SessionFailure::data(
            "invalid-session-id",
            "--session-id must not be empty",
        ));
    }
    if !state_home.is_absolute() {
        return Err(SessionFailure::data(
            "invalid-state-home",
            "--state-home must be absolute",
        ));
    }
    Ok(())
}

fn validate_context_args(args: &SessionContextArgs) -> Result<(), SessionFailure> {
    let request_id = args.request_id.as_bytes();
    let valid_request_id = !request_id.is_empty()
        && request_id.len() <= MAX_CONTEXT_REQUEST_ID_BYTES
        && request_id[0].is_ascii_alphanumeric()
        && request_id
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'));
    if !valid_request_id {
        return Err(SessionFailure::data(
            "invalid-request-id",
            "--request-id must be a bounded argv-safe identifier",
        ));
    }
    if args.max_bytes == 0 || args.max_bytes > MAX_CONTEXT_BYTES {
        return Err(SessionFailure::data(
            "invalid-max-bytes",
            "--max-bytes must be between 1 and 65536",
        ));
    }
    Ok(())
}

fn validate_prerequisite_binding(
    binding: &SessionPrerequisiteBindingArgs,
) -> Result<(), SessionFailure> {
    let valid = [
        binding.agent_id.as_str(),
        binding.workspace_generation.as_str(),
        binding.call_id.as_str(),
        binding.tool_name.as_str(),
        binding.definition_id.as_str(),
    ]
    .into_iter()
    .all(|value| {
        !value.is_empty()
            && value.len() <= MAX_PREREQUISITE_BINDING_BYTES
            && !value.bytes().any(|byte| byte.is_ascii_control())
    }) && binding.turn > 0
        && binding.step > 0;
    if valid {
        Ok(())
    } else {
        Err(SessionFailure::data(
            "invalid-prerequisite-binding",
            "the prerequisite execution binding is invalid",
        ))
    }
}

fn validate_prerequisite_selection(
    fallback: FallbackMode,
    use_user_config: bool,
    expected_integration_fingerprint: Option<&str>,
) -> Result<(), SessionFailure> {
    if fallback == FallbackMode::Auto
        && !use_user_config
        && expected_integration_fingerprint.is_none()
    {
        Ok(())
    } else {
        Err(SessionFailure::data(
            "unsupported-prerequisite-selection",
            "prerequisites require the verifier's default catalog selection",
        ))
    }
}

fn validate_record_context(
    common: &SessionCommonArgs,
    project: &Path,
    record: &SessionRecord,
) -> Result<(), SessionFailure> {
    validate_record_identity(&common.session_id, common.product.as_str(), project, record)
}

fn validate_record_identity(
    session_id: &str,
    product_label: &str,
    project: &Path,
    record: &SessionRecord,
) -> Result<(), SessionFailure> {
    let context_matches = record.session_hash == digest(session_id.trim().as_bytes())
        && record.project_hash == project_hash(project)
        && record.product == product_label;
    if !context_matches {
        return Err(SessionFailure::data(
            "context-mismatch",
            "session activation does not match the requested session, project, and product",
        ));
    }
    Ok(())
}

fn record_path(common: &SessionCommonArgs, project: &Path) -> Result<PathBuf, SessionFailure> {
    record_path_for(
        &common.session_id,
        &common.state_home,
        common.product.as_str(),
        project,
    )
}

fn record_path_for(
    session_id: &str,
    state_home: &Path,
    product_label: &str,
    project: &Path,
) -> Result<PathBuf, SessionFailure> {
    Ok(state_home
        .join("agent-docs/sessions")
        .join(digest(session_id.trim().as_bytes()))
        .join(product_label)
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
                    phases: document.phases.iter().map(Phase::as_str).collect(),
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
        phase: report.phase.as_ref().map(Phase::as_str),
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

fn context_fingerprint(
    report: &PreflightReport,
    catalog: &integration::EffectiveCatalog,
    fallback: FallbackMode,
    integration_fingerprint: Option<&str>,
) -> Result<String, SessionFailure> {
    Ok(format!(
        "{CONTEXT_FINGERPRINT_PREFIX}{}",
        fingerprint(report, catalog, fallback, integration_fingerprint)?
    ))
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
        DecodedRecord::Current(record) => Ok(*record),
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
            .map(|record| DecodedRecord::Current(Box::new(record)))
            .map_err(|err| SessionFailure::runtime("record-parse-failed", err.to_string())),
        LEGACY_RECORD_SCHEMA => Ok(DecodedRecord::LegacyV1),
        unsupported => Err(unsupported_record(unsupported)),
    }
}

fn unsupported_record(schema: &str) -> SessionFailure {
    let mut failure = SessionFailure::data("unsupported-record", "unsupported session schema");
    if schema == LEGACY_RECORD_SCHEMA {
        failure.message = "the v1 session record must be replaced".to_string();
        failure.details = failure_details("missing-activation");
        failure.details.record_relation = Some(RecordRelation::PriorVersionReplaceable);
    } else if session_record_version(schema).is_some_and(|version| version > 2) {
        failure.message = "the session record requires a newer agent-docs version".to_string();
        failure.details.record_relation = Some(RecordRelation::Future);
    } else {
        failure.message = "the session record schema is unrecognized".to_string();
        failure.details = failure_details("record-parse-failed");
        failure.details.record_relation = Some(RecordRelation::Unrecognized);
    }
    failure.hint = Some(format!(
        "Next action: `{}`.",
        failure.details.next_action.as_str()
    ));
    failure
}

fn session_record_version(schema: &str) -> Option<u64> {
    schema
        .strip_prefix("agent-docs.session.v")
        .and_then(|version| version.parse().ok())
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
                let details = serde_json::to_value(&failure.details)
                    .expect("session failure details serialize");
                let mut error =
                    EnvelopeError::new(failure.code, &failure.message).with_details(details);
                if let Some(hint) = &failure.hint {
                    error = error.with_hint(hint);
                }
                let envelope: Envelope<SessionData> =
                    Envelope::failure(format!("cli.agent-docs.session.{command}.v1"), error);
                println!(
                    "{}",
                    serde_json::to_string_pretty(&envelope).expect("session error serializes")
                );
            } else {
                eprintln!(
                    "agent-docs session {command}: {}; next action: {}",
                    failure.message,
                    failure.details.next_action.as_str()
                );
            }
            failure.exit_code
        }
    }
}

fn render_context(format: OutputFormat, result: Result<ContextData, SessionFailure>) -> i32 {
    const SCHEMA: &str = "cli.agent-docs.session.context.v1";
    match result {
        Ok(data) => {
            if format == OutputFormat::Json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "schema_version": SCHEMA,
                        "ok": true,
                        "data": data,
                    }))
                    .expect("session context output serializes")
                );
            } else {
                println!(
                    "agent-docs session context: product={} intent={} documents={} bytes={} verified={}",
                    data.decision.product,
                    data.decision.intent,
                    data.decision.document_count,
                    data.decision.total_bytes,
                    data.decision.verified
                );
            }
            EXIT_OK
        }
        Err(failure) => {
            if format == OutputFormat::Json {
                let details = serde_json::to_value(&failure.details)
                    .expect("session failure details serialize");
                let mut error =
                    EnvelopeError::new(failure.code, &failure.message).with_details(details);
                if let Some(hint) = &failure.hint {
                    error = error.with_hint(hint);
                }
                let envelope: Envelope<ContextData> = Envelope::failure(SCHEMA, error);
                println!(
                    "{}",
                    serde_json::to_string_pretty(&envelope).expect("session error serializes")
                );
            } else {
                eprintln!(
                    "agent-docs session context: {}; next action: {}",
                    failure.message,
                    failure.details.next_action.as_str()
                );
            }
            failure.exit_code
        }
    }
}

fn render_prerequisite(
    format: OutputFormat,
    result: Result<PrerequisiteData, SessionFailure>,
) -> i32 {
    const SCHEMA: &str = "cli.agent-docs.session.prerequisite.v1";
    match result {
        Ok(data) => {
            if format == OutputFormat::Json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "schema_version": SCHEMA,
                        "ok": true,
                        "data": data,
                    }))
                    .expect("session prerequisite output serializes")
                );
            } else {
                println!(
                    "agent-docs session prerequisite: product={} intent={} reason={} verified={}",
                    data.decision.product,
                    data.decision.intent,
                    data.decision.reason,
                    data.decision.verified
                );
            }
            EXIT_OK
        }
        Err(failure) => render_prerequisite_failure(SCHEMA, "prerequisite", format, failure),
    }
}

fn render_prerequisite_commit(
    format: OutputFormat,
    result: Result<PrerequisiteCommitData, SessionFailure>,
) -> i32 {
    const SCHEMA: &str = "cli.agent-docs.session.commit-prerequisite.v1";
    match result {
        Ok(data) => {
            if format == OutputFormat::Json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "schema_version": SCHEMA,
                        "ok": true,
                        "data": data,
                    }))
                    .expect("session prerequisite commit output serializes")
                );
            } else {
                println!(
                    "agent-docs session commit-prerequisite: product={} intent={} reason={} verified={}",
                    data.product, data.intent, data.reason, data.verified
                );
            }
            EXIT_OK
        }
        Err(failure) => render_prerequisite_failure(SCHEMA, "commit-prerequisite", format, failure),
    }
}

fn render_prerequisite_failure(
    schema: &'static str,
    command: &'static str,
    format: OutputFormat,
    failure: SessionFailure,
) -> i32 {
    if format == OutputFormat::Json {
        let details =
            serde_json::to_value(&failure.details).expect("session failure details serialize");
        let mut error = EnvelopeError::new(failure.code, &failure.message).with_details(details);
        if let Some(hint) = &failure.hint {
            error = error.with_hint(hint);
        }
        let envelope: Envelope<serde_json::Value> = Envelope::failure(schema, error);
        println!(
            "{}",
            serde_json::to_string_pretty(&envelope).expect("session error serializes")
        );
    } else {
        eprintln!(
            "agent-docs session {command}: {}; next action: {}",
            failure.message,
            failure.details.next_action.as_str()
        );
    }
    failure.exit_code
}

struct SessionFailure {
    code: &'static str,
    message: String,
    hint: Option<String>,
    details: Box<SessionFailureDetails>,
    exit_code: i32,
}

impl SessionFailure {
    fn data(code: &'static str, _message: impl Into<String>) -> Self {
        Self::classified(code, EXIT_DATA)
    }

    fn config(code: &'static str, _message: impl Into<String>) -> Self {
        Self::classified(code, EXIT_CONFIG)
    }

    fn runtime(code: &'static str, _message: impl Into<String>) -> Self {
        Self::classified(code, EXIT_RUNTIME)
    }

    fn classified(code: &'static str, exit_code: i32) -> Self {
        let details = failure_details(code);
        let hint = Some(format!("Next action: `{}`.", details.next_action.as_str()));
        Self {
            code,
            message: failure_message(code).to_string(),
            hint,
            details,
            exit_code,
        }
    }

    fn with_available_intents(mut self, intents: &[String]) -> Self {
        let mut bounded: Vec<String> = intents
            .iter()
            .filter(|intent| intent.len() <= MAX_RECOVERY_IDENTIFIER_BYTES)
            .take(MAX_AVAILABLE_INTENTS)
            .cloned()
            .collect();
        bounded.sort();
        bounded.dedup();
        self.details.available_intents = bounded;
        self
    }

    fn with_prepare_context(mut self, intents: &[String], phase: Option<&Phase>) -> Self {
        if self.details.next_action == NextAction::PrepareIntent {
            self.details.recovery.intents = bounded_intents(intents);
            self.details.recovery.phase =
                phase.and_then(|phase| bounded_identifier(phase.as_str()));
        }
        self
    }

    fn with_prepare_intent(self, intent: &Context, phase: Option<&Phase>) -> Self {
        self.with_prepare_context(&[intent.to_string()], phase)
    }

    fn with_refresh_context(mut self, intents: &[String], phase: Option<&Phase>) -> Self {
        if self.details.next_action == NextAction::RefreshIntegrationDecision {
            self.details.recovery.intents = bounded_intents(intents);
            self.details.recovery.phase =
                phase.and_then(|phase| bounded_identifier(phase.as_str()));
        }
        self
    }

    fn with_preflight_context(
        mut self,
        intent: &Context,
        phase: Option<&Phase>,
        report: &PreflightReport,
    ) -> Self {
        self.details.recovery.intents = bounded_intents(&[intent.to_string()]);
        self.details.recovery.phase = phase.and_then(|phase| bounded_identifier(phase.as_str()));
        self.details.diagnostics = Some(PreflightDiagnostics {
            required_total: report.summary.required_total,
            satisfied_required: report.summary.satisfied_required,
            missing_required: report.summary.missing_required,
            invalid_required: report.summary.invalid_required,
        });
        self
    }

    fn for_dsh_context(mut self, intent: &str, phase: Option<&Phase>) -> Self {
        match self.code {
            "undeclared-intent" => {
                self.details.next_action = NextAction::FixArguments;
                self.details.recovery = recovery_action(RecoveryAction::FixArguments);
            }
            "preflight-unsatisfied" | "phase-unsatisfied" | "context-policy-invalid" => {
                self.details.next_action = NextAction::RepairCatalog;
                self.details.recovery = SessionRecovery {
                    retry_original: Some(true),
                    ..recovery_action(RecoveryAction::RepairCatalog)
                };
            }
            "stale-integration-decision" => {
                self.details.recovery.then = Some(RecoveryCommand::SessionContext);
            }
            _ => return self,
        }
        if let Ok(intent) = Context::parse(intent) {
            self.details.recovery.intents = bounded_intents(&[intent.to_string()]);
        }
        self.details.recovery.phase = phase.and_then(|phase| bounded_identifier(phase.as_str()));
        self.hint = Some(format!(
            "Next action: `{}`.",
            self.details.next_action.as_str()
        ));
        self
    }
}

fn failure_details(code: &str) -> Box<SessionFailureDetails> {
    let (retryable, next_action, recovery) = match code {
        "invalid-session-id"
        | "invalid-state-home"
        | "invalid-intent"
        | "invalid-phase"
        | "invalid-product"
        | "invalid-request-id"
        | "invalid-max-bytes"
        | "invalid-prerequisite-binding"
        | "unsupported-prerequisite-selection"
        | "prerequisite-receipt-invalid"
        | "prerequisite-receipt-mismatch"
        | "context-budget-exceeded"
        | "root-resolution-failed" => (
            false,
            NextAction::FixArguments,
            recovery_action(RecoveryAction::FixArguments),
        ),
        "undeclared-intent" => (
            false,
            NextAction::ListDeclaredIntents,
            recovery_command(RecoveryCommand::List, catalog_reuse_scope()),
        ),
        "preflight-unsatisfied" | "phase-unsatisfied" => (
            false,
            NextAction::InspectPreflight,
            recovery_command(RecoveryCommand::Preflight, catalog_reuse_scope()),
        ),
        "missing-intent" | "stale-activation" | "missing-activation" => (
            true,
            NextAction::PrepareIntent,
            SessionRecovery {
                command: Some(RecoveryCommand::SessionPrepare),
                action: None,
                reuse_scope: session_reuse_scope(),
                intents: Vec::new(),
                phase: None,
                refresh_integration_fingerprint: Some(false),
                then: None,
                retry_original: Some(true),
                max_attempts: None,
            },
        ),
        "prerequisite-stale" => (
            true,
            NextAction::PrepareIntent,
            SessionRecovery {
                command: Some(RecoveryCommand::SessionPrerequisite),
                action: None,
                reuse_scope: session_reuse_scope(),
                intents: Vec::new(),
                phase: None,
                refresh_integration_fingerprint: Some(false),
                then: None,
                retry_original: Some(true),
                max_attempts: None,
            },
        ),
        "stale-integration-decision" => (
            true,
            NextAction::RefreshIntegrationDecision,
            SessionRecovery {
                command: Some(RecoveryCommand::IntegrationResolve),
                action: None,
                reuse_scope: session_reuse_scope(),
                intents: Vec::new(),
                phase: None,
                refresh_integration_fingerprint: Some(true),
                then: Some(RecoveryCommand::SessionPrepare),
                retry_original: Some(true),
                max_attempts: None,
            },
        ),
        "catalog-load-failed"
        | "integration-catalog-not-selected"
        | "integration-resolution-failed"
        | "context-policy-invalid" => (
            false,
            NextAction::RepairCatalog,
            recovery_command(RecoveryCommand::Audit, catalog_reuse_scope()),
        ),
        "lock-timeout" => (
            true,
            NextAction::RetryBounded,
            SessionRecovery {
                max_attempts: Some(BOUNDED_RETRY_ATTEMPTS),
                ..recovery_action(RecoveryAction::RetryCommand)
            },
        ),
        "fingerprint-producer-missing"
        | "fingerprint-failed"
        | "context-content-missing"
        | "record-render-failed"
        | "record-path-not-portable"
        | "prerequisite-receipt-render-failed"
        | "lock-owner-render-failed"
        | "integration-catalog-invariant-failed" => (
            false,
            NextAction::ReportInvariant,
            recovery_action(RecoveryAction::ReportInvariant),
        ),
        "unsupported-record" => (
            false,
            NextAction::UpgradeAgentDocs,
            recovery_action(RecoveryAction::UpgradeAgentDocs),
        ),
        _ => (
            false,
            NextAction::InspectSessionState,
            recovery_command(RecoveryCommand::SessionStatus, session_reuse_scope()),
        ),
    };
    Box::new(SessionFailureDetails {
        retryable,
        next_action,
        recovery,
        available_intents: Vec::new(),
        diagnostics: None,
        record_relation: None,
    })
}

fn failure_message(code: &str) -> &'static str {
    match code {
        "invalid-session-id" => "the session identifier is invalid",
        "invalid-state-home" => "the session state location is invalid",
        "invalid-intent" => "the intent identifier is invalid",
        "invalid-phase" => "the phase identifier is invalid",
        "invalid-product" => "the selected product is not supported for session context",
        "invalid-request-id" => "the context request identifier is invalid",
        "invalid-max-bytes" => "the context response budget is invalid",
        "invalid-prerequisite-binding" => "the prerequisite execution binding is invalid",
        "unsupported-prerequisite-selection" => "the prerequisite catalog selection is unsupported",
        "prerequisite-receipt-invalid" => "the prerequisite receipt is invalid",
        "prerequisite-receipt-mismatch" => {
            "the prerequisite receipt belongs to a different execution scope"
        }
        "prerequisite-stale" => "the prerequisite policy changed before commit",
        "context-budget-exceeded" => "required policy content exceeds the response budget",
        "context-policy-invalid" => {
            "the prepared DSH context no longer satisfies the bounded policy contract"
        }
        "context-content-missing" => "required policy content was unavailable after validation",
        "undeclared-intent" => "the requested intent is not declared for this project",
        "preflight-unsatisfied" => "required policy documents are not satisfied",
        "phase-unsatisfied" => "required policy documents are not satisfied for this phase",
        "missing-intent" => "the required intent has not been prepared",
        "stale-activation" => "the prepared intent no longer matches current policy",
        "missing-activation" => "no session activation exists for this scope",
        "stale-integration-decision" => "the integration decision must be refreshed",
        "unsupported-record" => "the session record schema is unsupported",
        "context-mismatch" => "the session record belongs to a different scope",
        "catalog-load-failed" => "the policy catalog could not be loaded",
        "integration-catalog-not-selected" => "the integration decision does not select a catalog",
        "integration-resolution-failed" => "the integration decision could not be resolved",
        "root-resolution-failed" => "the configured documentation roots could not be resolved",
        "record-read-failed" => "the session record could not be read",
        "record-parse-failed" => "the session record is corrupt or unreadable",
        "record-write-failed" => "the session record could not be written",
        "lock-timeout" => "the session record remained locked",
        "lock-parent-failed"
        | "lock-failed"
        | "lock-owner-write-failed"
        | "stale-lock-remove-failed"
        | "stale-lock-reclaim-failed" => "the session record lock could not be managed safely",
        "fingerprint-producer-missing" | "fingerprint-failed" => {
            "the session policy fingerprint could not be produced"
        }
        "record-render-failed" => "the session record could not be serialized",
        "record-path-not-portable" => "the session record location violated an invariant",
        "prerequisite-receipt-render-failed" => "the prerequisite receipt could not be serialized",
        "lock-owner-render-failed" => "the session lock owner could not be serialized",
        "integration-catalog-invariant-failed" => {
            "the integration catalog violated an internal invariant"
        }
        _ => "the session operation could not be completed safely",
    }
}

fn recovery_command(command: RecoveryCommand, reuse_scope: Vec<ReuseField>) -> SessionRecovery {
    SessionRecovery {
        command: Some(command),
        action: None,
        reuse_scope,
        intents: Vec::new(),
        phase: None,
        refresh_integration_fingerprint: None,
        then: None,
        retry_original: None,
        max_attempts: None,
    }
}

fn recovery_action(action: RecoveryAction) -> SessionRecovery {
    SessionRecovery {
        command: None,
        action: Some(action),
        reuse_scope: Vec::new(),
        intents: Vec::new(),
        phase: None,
        refresh_integration_fingerprint: None,
        then: None,
        retry_original: None,
        max_attempts: None,
    }
}

fn catalog_reuse_scope() -> Vec<ReuseField> {
    vec![
        ReuseField::Product,
        ReuseField::DocsHome,
        ReuseField::ProjectPath,
        ReuseField::UserConfig,
    ]
}

fn session_reuse_scope() -> Vec<ReuseField> {
    vec![
        ReuseField::SessionId,
        ReuseField::Product,
        ReuseField::StateHome,
        ReuseField::DocsHome,
        ReuseField::ProjectPath,
        ReuseField::UserConfig,
    ]
}

fn bounded_intents(intents: &[String]) -> Vec<String> {
    let mut bounded: Vec<String> = intents
        .iter()
        .filter(|intent| intent.len() <= MAX_RECOVERY_IDENTIFIER_BYTES)
        .take(MAX_RECOVERY_INTENTS)
        .cloned()
        .collect();
    bounded.sort();
    bounded.dedup();
    bounded
}

fn bounded_identifier(identifier: &str) -> Option<String> {
    (identifier.len() <= MAX_RECOVERY_IDENTIFIER_BYTES).then(|| identifier.to_string())
}

fn declared_requested_intents(requested: &[String], available: &[String]) -> Vec<String> {
    let available: BTreeSet<&str> = available.iter().map(String::as_str).collect();
    let declared: Vec<String> = requested
        .iter()
        .filter(|intent| available.contains(intent.as_str()))
        .cloned()
        .collect();
    bounded_intents(&declared)
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

    #[test]
    fn recovery_policy_distinguishes_bounded_retry_state_inspection_and_invariants() {
        for (code, retryable, next_action) in [
            ("lock-timeout", true, NextAction::RetryBounded),
            (
                "record-parse-failed",
                false,
                NextAction::InspectSessionState,
            ),
            ("fingerprint-failed", false, NextAction::ReportInvariant),
        ] {
            let failure = SessionFailure::runtime(code, "private diagnostic");
            assert_eq!(failure.details.retryable, retryable, "code={code}");
            assert_eq!(failure.details.next_action, next_action, "code={code}");
        }
        let lock_timeout = SessionFailure::runtime("lock-timeout", "private diagnostic");
        assert_eq!(
            lock_timeout.details.recovery.max_attempts,
            Some(BOUNDED_RETRY_ATTEMPTS)
        );
    }

    #[test]
    fn recovery_arrays_are_bounded() {
        let intents: Vec<String> = (0..64).map(|index| format!("intent-{index:02}")).collect();
        let undeclared = SessionFailure::data("undeclared-intent", "private diagnostic")
            .with_available_intents(&intents);
        assert_eq!(
            undeclared.details.available_intents.len(),
            MAX_AVAILABLE_INTENTS
        );

        let missing = SessionFailure::data("missing-intent", "private diagnostic")
            .with_prepare_context(&intents, None);
        assert_eq!(missing.details.recovery.intents.len(), MAX_RECOVERY_INTENTS);

        let oversized = vec!["x".repeat(MAX_RECOVERY_IDENTIFIER_BYTES + 1)];
        let oversized_failure = SessionFailure::data("undeclared-intent", "private diagnostic")
            .with_available_intents(&oversized);
        assert!(oversized_failure.details.available_intents.is_empty());
    }

    #[test]
    fn unrecognized_record_schema_is_inspected_without_echoing_private_content() {
        let private_schema = "PRIVATE_RECORD_CONTENT_SENTINEL";
        let failure = unsupported_record(private_schema);

        assert_eq!(failure.details.next_action, NextAction::InspectSessionState);
        assert!(matches!(
            failure.details.record_relation,
            Some(RecordRelation::Unrecognized)
        ));
        assert!(!failure.message.contains(private_schema));
        assert!(
            !failure
                .hint
                .as_deref()
                .unwrap_or_default()
                .contains(private_schema)
        );
    }
}
