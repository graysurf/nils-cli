use std::ffi::{CString, OsStr};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const MANIFEST_SCHEMA: &str = "execution-capsule.v1";
const RECEIPT_SCHEMA: &str = "cli.codex-cli.execution-capsule.receipt.v1";
const ERROR_SCHEMA: &str = "cli.codex-cli.execution-capsule.error.v1";
const ATTESTATION_SCHEMA: &str = "cli.codex-cli.execution-capsule.attestation.v1";
const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
const MAX_ENTRYPOINT_BYTES: u64 = 1024 * 1024;
const MAX_FINAL_BYTES: u64 = 64 * 1024;
const MAX_EVENTS_BYTES: u64 = 32 * 1024 * 1024;
static TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug)]
pub struct RunOptions {
    pub capsule: PathBuf,
    pub allow_host_access: bool,
    pub json: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum Access {
    Workspace,
    Host,
}

impl Access {
    fn as_str(self) -> &'static str {
        match self {
            Self::Workspace => "workspace",
            Self::Host => "host",
        }
    }

    fn sandbox(self) -> &'static str {
        match self {
            Self::Workspace => "workspace-write",
            Self::Host => "danger-full-access",
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema_version: String,
    task: String,
    cwd: PathBuf,
    entrypoint: PathBuf,
    entrypoint_sha256: String,
    access: Access,
    allowed_paths: Vec<PathBuf>,
    #[serde(default)]
    expected_git: Option<ExpectedGit>,
    #[serde(default)]
    validation: Vec<ValidationStep>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExpectedGit {
    #[serde(default)]
    head: Option<String>,
    #[serde(default)]
    branch: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ValidationStep {
    #[serde(default)]
    name: Option<String>,
    argv: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct FinalReport {
    status: FinalStatus,
    summary: String,
    actions: Vec<String>,
    validation: Vec<String>,
    errors: Vec<String>,
    recommendations: Vec<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum FinalStatus {
    Succeeded,
    Failed,
    Blocked,
}

#[derive(Clone, Debug, Serialize)]
struct ValidationResult {
    name: Option<String>,
    argv: Vec<String>,
    exit_code: i32,
    passed: bool,
    command: String,
    events: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
struct ScriptRunResult {
    phase: String,
    terminal: bool,
    exit_code: i32,
    passed: bool,
    command: String,
    events: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
struct ArtifactPaths {
    result_schema: String,
    events: String,
    final_report: String,
    receipt: String,
}

#[derive(Clone, Debug, Serialize)]
struct ReceiptData {
    capsule: String,
    manifest_sha256: String,
    entrypoint_sha256: String,
    access: &'static str,
    codex_exit_code: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    codex_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    evidence_error: Option<String>,
    script_runs: Vec<ScriptRunResult>,
    script_passed: bool,
    validation: Vec<ValidationResult>,
    validations_passed: bool,
    helper_integrity_valid: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    helper_integrity_error: Option<String>,
    entrypoint_integrity_valid: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    entrypoint_integrity_error: Option<String>,
    final_report_valid: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    final_report_error: Option<String>,
    #[serde(rename = "final")]
    final_report: Option<FinalReport>,
    artifacts: ArtifactPaths,
    completed_at: String,
}

#[derive(Debug)]
struct HelperCommands {
    nonce: String,
    script: String,
    validation: Vec<String>,
    executable: File,
    executable_identity: FileIdentity,
    executable_digest: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ExecutionAttestation {
    schema_version: String,
    nonce: String,
    kind: String,
    #[serde(default)]
    validation_index: Option<usize>,
    entrypoint_sha256: String,
    exit_code: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Debug)]
struct CapsuleError {
    code: &'static str,
    message: String,
    exit_code: i32,
}

impl CapsuleError {
    fn invalid(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            exit_code: 65,
        }
    }

    fn runtime(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            exit_code: 1,
        }
    }
}

struct ValidatedCapsule {
    root: PathBuf,
    directory: CapsuleDirectory,
    manifest: Manifest,
    manifest_sha256: String,
    manifest_identity: FileIdentity,
    cwd: PathBuf,
    entrypoint: PathBuf,
    entrypoint_file: File,
    entrypoint_identity: FileIdentity,
    allowed_paths: Vec<PathBuf>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileIdentity {
    device: u64,
    inode: u64,
    uid: u32,
    mode: u32,
    links: u64,
    size: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

impl FileIdentity {
    fn from_metadata(metadata: &fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            uid: metadata.uid(),
            mode: metadata.mode(),
            links: metadata.nlink(),
            size: metadata.size(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        }
    }
}

struct CapsuleDirectory {
    file: File,
}

pub fn run(options: &RunOptions) -> i32 {
    match run_inner(options) {
        Ok(code) => code,
        Err(error) => {
            render_error(&error, options.json);
            error.exit_code
        }
    }
}

fn run_inner(options: &RunOptions) -> Result<i32, CapsuleError> {
    let capsule = validate_capsule(&options.capsule, true)?;
    if capsule.manifest.access == Access::Host && !options.allow_host_access {
        return Err(CapsuleError::invalid(
            "host-access-acknowledgement-required",
            "host access requires the operator to pass --allow-host-access",
        ));
    }

    let schema_path = capsule.root.join("result.schema.json");
    let events_path = capsule.root.join("events.jsonl");
    let final_path = capsule.root.join("final.json");
    let receipt_path = capsule.root.join("receipt.json");
    let receipt_recovery_name = format!(
        "receipt.recovery.{}.json",
        random_artifact_token("receipt-recovery-name-unavailable")?
    );
    capsule
        .directory
        .write_private_json("result.schema.json", &final_report_schema())?;
    drop(capsule.directory.private_output_file("events.jsonl")?);
    drop(capsule.directory.private_output_file("final.json")?);
    drop(capsule.directory.private_output_file("receipt.json")?);
    let mut helpers = helper_commands(&capsule)?;
    let mut final_capture = child_output_capture(&capsule.directory)?;
    let final_capture_path = format!("/dev/fd/{}", final_capture.as_raw_fd());

    crate::runtime::refresh_remote_auth_before_exec();
    let prompt = supervisor_prompt(&capsule, &helpers);
    let codex_spawn = Command::new("codex")
        .args([
            "--ask-for-approval",
            "never",
            "exec",
            "--skip-git-repo-check",
            "-C",
        ])
        .arg(&capsule.cwd)
        .args(["--sandbox", capsule.manifest.access.sandbox(), "--json"])
        .args(["--output-schema"])
        .arg(&schema_path)
        .args(["--output-last-message"])
        .arg(&final_capture_path)
        .args(["--", &prompt])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn();
    let (codex_exit_code, codex_error, events_bytes, mut evidence_error) = match codex_spawn {
        Ok(mut child) => {
            let (bytes, read_error) = match child.stdout.take() {
                Some(stdout) => read_bounded(
                    stdout,
                    MAX_EVENTS_BYTES,
                    "events-unreadable",
                    "events-too-large: Codex events exceed 32 MiB",
                ),
                None => (
                    Vec::new(),
                    Some("events-unreadable: Codex stdout pipe was unavailable".to_string()),
                ),
            };
            let status = child.wait().map_err(|error| {
                CapsuleError::runtime(
                    "codex-exec-failed",
                    format!("failed to wait for Codex supervisor: {error}"),
                )
            })?;
            (status.code().unwrap_or(1), None, bytes, read_error)
        }
        Err(error) => (
            127,
            Some(format!(
                "codex-exec-failed: failed to start Codex supervisor: {error}"
            )),
            Vec::new(),
            None,
        ),
    };
    let (final_bytes, mut final_capture_error) =
        if let Err(error) = final_capture.seek(SeekFrom::Start(0)) {
            (
                Vec::new(),
                Some(format!(
                    "final-report-unreadable: cannot rewind capture: {error}"
                )),
            )
        } else {
            read_bounded(
                &mut final_capture,
                MAX_FINAL_BYTES,
                "final-report-unreadable",
                "final-report-too-large: Codex final report exceeds 64 KiB",
            )
        };
    if let Err(error) = capsule
        .directory
        .write_private_bytes_replacing("events.jsonl", &events_bytes)
    {
        evidence_error = Some(format!("{}: {}", error.code, error.message));
    }
    if let Err(error) = capsule
        .directory
        .write_private_bytes_replacing("final.json", &final_bytes)
    {
        final_capture_error = Some(format!("{}: {}", error.code, error.message));
    }

    let (final_report, final_report_error) = if codex_error.is_some() {
        (None, codex_error.clone())
    } else if final_capture_error.is_some() {
        (None, final_capture_error)
    } else {
        match read_final_report_bytes(&final_bytes) {
            Ok(report) => (Some(report), None),
            Err(error) => (None, Some(format!("{}: {}", error.code, error.message))),
        }
    };
    let final_succeeded = final_report
        .as_ref()
        .is_some_and(|report| report.status == FinalStatus::Succeeded);

    let (script_runs, validation) = if evidence_error.is_none() {
        read_execution_evidence(&capsule, &helpers, &events_bytes)
    } else {
        missing_execution_evidence(
            &capsule,
            &helpers,
            evidence_error.as_deref().unwrap_or("events unavailable"),
        )
    };
    let script_passed = script_runs.last().is_some_and(|result| result.passed);
    let validations_passed = validation.iter().all(|result| result.passed);
    let (helper_integrity_valid, helper_integrity_error) =
        match verify_helper_integrity(&mut helpers) {
            Ok(()) => (true, None),
            Err(error) => (false, Some(format!("{}: {}", error.code, error.message))),
        };
    let (entrypoint_integrity_valid, entrypoint_integrity_error) =
        match verify_capsule_integrity(&capsule) {
            Ok(()) => (true, None),
            Err(error) => (false, Some(format!("{}: {}", error.code, error.message))),
        };
    let ok = codex_exit_code == 0
        && codex_error.is_none()
        && final_succeeded
        && evidence_error.is_none()
        && helper_integrity_valid
        && entrypoint_integrity_valid
        && script_passed
        && validations_passed;

    let result = ReceiptData {
        capsule: capsule.root.display().to_string(),
        manifest_sha256: capsule.manifest_sha256.clone(),
        entrypoint_sha256: capsule.manifest.entrypoint_sha256.clone(),
        access: capsule.manifest.access.as_str(),
        codex_exit_code,
        codex_error: codex_error.clone(),
        evidence_error: evidence_error.clone(),
        script_runs,
        script_passed,
        validation,
        validations_passed,
        helper_integrity_valid,
        helper_integrity_error,
        entrypoint_integrity_valid,
        entrypoint_integrity_error,
        final_report_valid: final_report.is_some(),
        final_report_error: final_report_error.clone(),
        final_report,
        artifacts: ArtifactPaths {
            result_schema: schema_path.display().to_string(),
            events: events_path.display().to_string(),
            final_report: final_path.display().to_string(),
            receipt: receipt_path.display().to_string(),
        },
        completed_at: Utc::now().to_rfc3339(),
    };
    let mut response = json!({
        "schema_version": RECEIPT_SCHEMA,
        "command": "agent run",
        "ok": ok,
        "result": result,
    });
    if !ok {
        response.as_object_mut().expect("receipt envelope").insert(
            "error".to_string(),
            execution_error(
                codex_exit_code,
                codex_error.as_deref(),
                evidence_error.as_deref(),
                final_succeeded,
                final_report_error.as_deref(),
                helper_integrity_valid,
                entrypoint_integrity_valid,
                script_passed,
                validations_passed,
                &receipt_path,
            ),
        );
    }
    let mut exit_code = if ok { 0 } else { 1 };
    if let Err(error) = capsule
        .directory
        .write_private_json_replacing("receipt.json", &response)
    {
        exit_code = 1;
        response["ok"] = json!(false);
        let receipt_error = format!("{}: {}", error.code, error.message);
        response["result"]["receipt_error"] = json!(receipt_error);
        let fallback_path = capsule.root.join(&receipt_recovery_name);
        response["result"]["artifacts"]["receipt"] = json!(fallback_path.display().to_string());
        if response.get("error").is_none() {
            response["error"] = json!({
                "code": "receipt-publish-failed",
                "message": "the primary receipt path was unsafe at closeout",
                "details": {
                    "receipt": fallback_path.display().to_string(),
                }
            });
        } else {
            response["error"]["details"]["receipt"] = json!(fallback_path.display().to_string());
            response["error"]["details"]["receipt_publish_error"] =
                json!(response["result"]["receipt_error"].clone());
        }
        capsule
            .directory
            .write_private_json_replacing(&receipt_recovery_name, &response)?;
    }
    render_receipt(&response, options.json);
    Ok(exit_code)
}

pub fn sandbox_exec(path: &Path, nonce: &str, validation_index: Option<usize>) -> i32 {
    let mut capsule = match validate_capsule(path, false) {
        Ok(capsule) => capsule,
        Err(error) => {
            render_error(&error, false);
            return error.exit_code;
        }
    };
    let (kind, outcome) = match validation_index {
        Some(index) => ("validation", run_validation_snapshot(&capsule, index)),
        None => ("script", run_entrypoint_snapshot(&mut capsule)),
    };
    let (exit_code, execution_error) = match outcome {
        Ok(exit_code) => (exit_code, None),
        Err(error) => {
            eprintln!("codex-cli agent capsule-exec: {}", error.message);
            (127, Some(format!("{}: {}", error.code, error.message)))
        }
    };
    println!(
        "\n{}",
        serde_json::to_string(&ExecutionAttestation {
            schema_version: ATTESTATION_SCHEMA.to_string(),
            nonce: nonce.to_string(),
            kind: kind.to_string(),
            validation_index,
            entrypoint_sha256: capsule.manifest.entrypoint_sha256.clone(),
            exit_code,
            error: execution_error,
        })
        .expect("attestation serialization")
    );
    exit_code
}

fn validate_capsule(path: &Path, verify_git: bool) -> Result<ValidatedCapsule, CapsuleError> {
    let input_metadata = fs::symlink_metadata(path).map_err(|error| {
        CapsuleError::invalid(
            "capsule-unreadable",
            format!("cannot inspect capsule {}: {error}", path.display()),
        )
    })?;
    if input_metadata.file_type().is_symlink() || !input_metadata.is_dir() {
        return Err(CapsuleError::invalid(
            "capsule-not-private-directory",
            "capsule must be a real directory, not a symlink",
        ));
    }
    require_owner_only(path, &input_metadata, "capsule-not-private-directory")?;
    let root = fs::canonicalize(path).map_err(|error| {
        CapsuleError::invalid(
            "capsule-unreadable",
            format!("cannot resolve capsule {}: {error}", path.display()),
        )
    })?;
    let directory = CapsuleDirectory::open(&root)?;
    let directory_metadata = directory.file.metadata().map_err(|error| {
        CapsuleError::invalid(
            "capsule-unreadable",
            format!("cannot inspect opened capsule {}: {error}", root.display()),
        )
    })?;
    require_owner_only(&root, &directory_metadata, "capsule-not-private-directory")?;
    if FileIdentity::from_metadata(&input_metadata)
        != FileIdentity::from_metadata(&directory_metadata)
    {
        return Err(CapsuleError::invalid(
            "capsule-changed",
            "capsule directory changed while it was being opened",
        ));
    }

    let manifest_path = root.join("manifest.json");
    let mut manifest_file = directory.open_private_regular(
        OsStr::new("manifest.json"),
        "manifest-unreadable",
        "manifest.json must be a regular file",
        "manifest-not-private",
    )?;
    let manifest_metadata = manifest_file.metadata().map_err(|error| {
        CapsuleError::invalid(
            "manifest-unreadable",
            format!("cannot inspect {}: {error}", manifest_path.display()),
        )
    })?;
    if manifest_metadata.len() > MAX_MANIFEST_BYTES {
        return Err(CapsuleError::invalid(
            "manifest-too-large",
            "manifest.json exceeds 64 KiB",
        ));
    }
    let mut manifest_bytes = Vec::with_capacity(manifest_metadata.len() as usize);
    manifest_file
        .read_to_end(&mut manifest_bytes)
        .map_err(|error| {
            CapsuleError::invalid(
                "manifest-unreadable",
                format!("cannot read {}: {error}", manifest_path.display()),
            )
        })?;
    let manifest: Manifest = serde_json::from_slice(&manifest_bytes).map_err(|error| {
        CapsuleError::invalid(
            "manifest-invalid",
            format!("invalid manifest.json: {error}"),
        )
    })?;
    validate_manifest_fields(&manifest)?;

    if manifest.access == Access::Host && manifest.allowed_paths.is_empty() {
        return Err(CapsuleError::invalid(
            "allowed-paths-missing",
            "host capsules must declare at least one allowed path",
        ));
    }
    let cwd = canonical_absolute_dir(&manifest.cwd, "cwd-invalid")?;
    if manifest.access == Access::Workspace && root.starts_with(&cwd) {
        return Err(CapsuleError::invalid(
            "workspace-capsule-inside-cwd",
            format!(
                "workspace capsule {} must be outside sandbox-writable cwd {}",
                root.display(),
                cwd.display()
            ),
        ));
    }
    let mut allowed_paths = Vec::with_capacity(manifest.allowed_paths.len());
    for path in &manifest.allowed_paths {
        let allowed = canonical_absolute(path, "allowed-path-invalid")?;
        if manifest.access == Access::Workspace && !allowed.starts_with(&cwd) {
            return Err(CapsuleError::invalid(
                "workspace-path-outside-cwd",
                format!(
                    "workspace capsule path {} is outside cwd {}",
                    allowed.display(),
                    cwd.display()
                ),
            ));
        }
        allowed_paths.push(allowed);
    }
    if !allowed_paths.iter().any(|path| cwd.starts_with(path)) {
        return Err(CapsuleError::invalid(
            "cwd-not-allowed",
            "cwd must be contained by one of allowed_paths",
        ));
    }

    let entrypoint_path = root.join(&manifest.entrypoint);
    let mut entrypoint_file = directory.open_private_regular(
        OsStr::new("run.sh"),
        "entrypoint-unreadable",
        "run.sh must be a regular file",
        "entrypoint-not-private",
    )?;
    let entrypoint_metadata = entrypoint_file.metadata().map_err(|error| {
        CapsuleError::invalid(
            "entrypoint-unreadable",
            format!("cannot inspect {}: {error}", entrypoint_path.display()),
        )
    })?;
    if entrypoint_metadata.len() > MAX_ENTRYPOINT_BYTES {
        return Err(CapsuleError::invalid(
            "entrypoint-too-large",
            "run.sh exceeds 1 MiB",
        ));
    }
    if entrypoint_metadata.mode() & 0o100 == 0 {
        return Err(CapsuleError::invalid(
            "entrypoint-not-executable",
            "run.sh must be executable by its owner",
        ));
    }
    let entrypoint = entrypoint_path;
    let actual_digest = sha256_open_file(&mut entrypoint_file, &entrypoint)?;
    if actual_digest != manifest.entrypoint_sha256 {
        return Err(CapsuleError::invalid(
            "entrypoint-digest-mismatch",
            format!(
                "run.sh digest mismatch: expected {}, found {actual_digest}",
                manifest.entrypoint_sha256
            ),
        ));
    }

    if verify_git && let Some(expected) = &manifest.expected_git {
        verify_git_preconditions(&cwd, expected)?;
    }

    Ok(ValidatedCapsule {
        root,
        directory,
        manifest,
        manifest_sha256: sha256_bytes(&manifest_bytes),
        manifest_identity: FileIdentity::from_metadata(&manifest_metadata),
        cwd,
        entrypoint,
        entrypoint_file,
        entrypoint_identity: FileIdentity::from_metadata(&entrypoint_metadata),
        allowed_paths,
    })
}

fn validate_manifest_fields(manifest: &Manifest) -> Result<(), CapsuleError> {
    if manifest.schema_version != MANIFEST_SCHEMA {
        return Err(CapsuleError::invalid(
            "manifest-schema-unsupported",
            format!(
                "expected schema_version {MANIFEST_SCHEMA}, found {}",
                manifest.schema_version
            ),
        ));
    }
    if manifest.task.trim().is_empty() {
        return Err(CapsuleError::invalid(
            "task-missing",
            "manifest task must not be empty",
        ));
    }
    if manifest.task.len() > 8 * 1024 {
        return Err(CapsuleError::invalid(
            "task-too-large",
            "manifest task exceeds 8 KiB",
        ));
    }
    if manifest.entrypoint != Path::new("run.sh")
        || manifest
            .entrypoint
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(CapsuleError::invalid(
            "entrypoint-invalid",
            "entrypoint must be the capsule-local run.sh",
        ));
    }
    if !manifest.entrypoint_sha256.starts_with("sha256:")
        || manifest.entrypoint_sha256.len() != "sha256:".len() + 64
        || !manifest
            .entrypoint_sha256
            .bytes()
            .skip("sha256:".len())
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(CapsuleError::invalid(
            "entrypoint-digest-invalid",
            "entrypoint_sha256 must be sha256 followed by 64 lowercase hex digits",
        ));
    }
    for step in &manifest.validation {
        if step.argv.is_empty() || step.argv.iter().any(|argument| argument.is_empty()) {
            return Err(CapsuleError::invalid(
                "validation-invalid",
                "each validation step must contain a non-empty argv array",
            ));
        }
    }
    Ok(())
}

fn require_owner_only(
    path: &Path,
    metadata: &fs::Metadata,
    code: &'static str,
) -> Result<(), CapsuleError> {
    let effective_uid = unsafe { libc::geteuid() };
    if metadata.uid() != effective_uid {
        return Err(CapsuleError::invalid(
            code,
            format!(
                "{} must be owned by effective uid {effective_uid}, found {}",
                path.display(),
                metadata.uid()
            ),
        ));
    }
    if metadata.mode() & 0o077 != 0 {
        return Err(CapsuleError::invalid(
            code,
            format!("{} must not grant group or other access", path.display()),
        ));
    }
    Ok(())
}

impl CapsuleDirectory {
    fn open(path: &Path) -> Result<Self, CapsuleError> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(path)
            .map_err(|error| {
                CapsuleError::invalid(
                    "capsule-unreadable",
                    format!("cannot securely open {}: {error}", path.display()),
                )
            })?;
        Ok(Self { file })
    }

    fn open_private_regular(
        &self,
        name: &OsStr,
        unreadable_code: &'static str,
        irregular_message: &'static str,
        private_code: &'static str,
    ) -> Result<File, CapsuleError> {
        let name = c_name(name, unreadable_code)?;
        let descriptor = unsafe {
            libc::openat(
                self.file.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if descriptor < 0 {
            return Err(CapsuleError::invalid(
                unreadable_code,
                format!(
                    "cannot securely open {}: {}",
                    name.to_string_lossy(),
                    std::io::Error::last_os_error()
                ),
            ));
        }
        let file = unsafe { File::from_raw_fd(descriptor) };
        let metadata = file.metadata().map_err(|error| {
            CapsuleError::invalid(
                unreadable_code,
                format!("cannot inspect {}: {error}", name.to_string_lossy()),
            )
        })?;
        if !metadata.is_file() {
            return Err(CapsuleError::invalid(unreadable_code, irregular_message));
        }
        require_owner_only(
            Path::new(OsStr::from_bytes(name.as_bytes())),
            &metadata,
            private_code,
        )?;
        if metadata.nlink() != 1 {
            return Err(CapsuleError::invalid(
                private_code,
                format!(
                    "{} must have exactly one filesystem link",
                    name.to_string_lossy()
                ),
            ));
        }
        Ok(file)
    }

    fn private_output_file(&self, name: &str) -> Result<File, CapsuleError> {
        let name = c_name(OsStr::new(name), "artifact-path-unsafe")?;
        self.prepare_artifact_target(&name)?;
        self.create_new_private(&name)
    }

    fn prepare_artifact_target(&self, name: &CString) -> Result<(), CapsuleError> {
        let mut stat = MaybeUninit::<libc::stat>::zeroed();
        let result = unsafe {
            libc::fstatat(
                self.file.as_raw_fd(),
                name.as_ptr(),
                stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if result == 0 {
            let stat = unsafe { stat.assume_init() };
            let file_type = stat.st_mode & libc::S_IFMT;
            let effective_uid = unsafe { libc::geteuid() };
            if file_type != libc::S_IFREG
                || stat.st_uid != effective_uid
                || stat.st_nlink != 1
                || stat.st_mode & 0o077 != 0
            {
                return Err(CapsuleError::invalid(
                    "artifact-path-unsafe",
                    format!(
                        "refusing unsafe pre-existing artifact {}",
                        name.to_string_lossy()
                    ),
                ));
            }
            if unsafe { libc::unlinkat(self.file.as_raw_fd(), name.as_ptr(), 0) } != 0 {
                return Err(CapsuleError::runtime(
                    "artifact-write-failed",
                    format!(
                        "cannot replace {}: {}",
                        name.to_string_lossy(),
                        std::io::Error::last_os_error()
                    ),
                ));
            }
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ENOENT) {
            return Ok(());
        }
        Err(CapsuleError::invalid(
            "artifact-path-unsafe",
            format!("cannot inspect {}: {error}", name.to_string_lossy()),
        ))
    }

    fn create_new_private(&self, name: &CString) -> Result<File, CapsuleError> {
        let descriptor = unsafe {
            libc::openat(
                self.file.as_raw_fd(),
                name.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                0o600,
            )
        };
        if descriptor < 0 {
            return Err(CapsuleError::runtime(
                "artifact-write-failed",
                format!(
                    "cannot create {}: {}",
                    name.to_string_lossy(),
                    std::io::Error::last_os_error()
                ),
            ));
        }
        let file = unsafe { File::from_raw_fd(descriptor) };
        let metadata = file.metadata().map_err(|error| {
            CapsuleError::runtime(
                "artifact-write-failed",
                format!("cannot inspect {}: {error}", name.to_string_lossy()),
            )
        })?;
        if metadata.uid() != unsafe { libc::geteuid() }
            || metadata.nlink() != 1
            || metadata.mode() & 0o077 != 0
        {
            return Err(CapsuleError::runtime(
                "artifact-write-failed",
                format!("new artifact {} is not private", name.to_string_lossy()),
            ));
        }
        Ok(file)
    }

    fn private_unlinked_output(&self, name: &str) -> Result<File, CapsuleError> {
        let name = c_name(OsStr::new(name), "artifact-path-unsafe")?;
        let descriptor = unsafe {
            libc::openat(
                self.file.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                0o600,
            )
        };
        if descriptor < 0 {
            return Err(CapsuleError::runtime(
                "final-capture-unavailable",
                format!(
                    "cannot create private final capture: {}",
                    std::io::Error::last_os_error()
                ),
            ));
        }
        let file = unsafe { File::from_raw_fd(descriptor) };
        let metadata = file.metadata().map_err(|error| {
            CapsuleError::runtime(
                "final-capture-unavailable",
                format!("cannot inspect private final capture: {error}"),
            )
        })?;
        if metadata.uid() != unsafe { libc::geteuid() }
            || metadata.nlink() != 1
            || metadata.mode() & 0o077 != 0
        {
            return Err(CapsuleError::runtime(
                "final-capture-unavailable",
                "private final capture is not owner-only",
            ));
        }
        if unsafe { libc::unlinkat(self.file.as_raw_fd(), name.as_ptr(), 0) } != 0 {
            return Err(CapsuleError::runtime(
                "final-capture-unavailable",
                format!(
                    "cannot unlink private final capture: {}",
                    std::io::Error::last_os_error()
                ),
            ));
        }
        Ok(file)
    }

    fn write_private_json(&self, name: &str, value: &Value) -> Result<(), CapsuleError> {
        let mut bytes = serde_json::to_vec_pretty(value).map_err(|error| {
            CapsuleError::runtime(
                "artifact-write-failed",
                format!("cannot encode {name}: {error}"),
            )
        })?;
        bytes.push(b'\n');
        self.write_private_bytes(name, &bytes)
    }

    fn write_private_json_replacing(&self, name: &str, value: &Value) -> Result<(), CapsuleError> {
        let mut bytes = serde_json::to_vec_pretty(value).map_err(|error| {
            CapsuleError::runtime(
                "artifact-write-failed",
                format!("cannot encode {name}: {error}"),
            )
        })?;
        bytes.push(b'\n');
        self.write_private_bytes_inner(name, &bytes, false)
    }

    fn write_private_bytes(&self, name: &str, bytes: &[u8]) -> Result<(), CapsuleError> {
        self.write_private_bytes_inner(name, bytes, true)
    }

    fn write_private_bytes_replacing(&self, name: &str, bytes: &[u8]) -> Result<(), CapsuleError> {
        self.write_private_bytes_inner(name, bytes, false)
    }

    fn write_private_bytes_inner(
        &self,
        name: &str,
        bytes: &[u8],
        inspect_target: bool,
    ) -> Result<(), CapsuleError> {
        let target = c_name(OsStr::new(name), "artifact-path-unsafe")?;
        if inspect_target {
            self.prepare_artifact_target(&target)?;
            let mut file = self.create_new_private(&target)?;
            return write_and_sync_artifact(&mut file, name, bytes);
        }
        self.write_private_bytes_replacing_platform(name, &target, bytes)
    }

    #[cfg(target_os = "linux")]
    fn write_private_bytes_replacing_platform(
        &self,
        name: &str,
        target: &CString,
        bytes: &[u8],
    ) -> Result<(), CapsuleError> {
        let mut file = self.create_unnamed_private()?;
        write_and_sync_artifact(&mut file, name, bytes)?;
        if unsafe { libc::unlinkat(self.file.as_raw_fd(), target.as_ptr(), 0) } != 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ENOENT) {
                return Err(CapsuleError::runtime(
                    "artifact-write-failed",
                    format!("cannot replace {name}: {error}"),
                ));
            }
        }
        let empty = c"";
        let direct_link = unsafe {
            libc::linkat(
                file.as_raw_fd(),
                empty.as_ptr(),
                self.file.as_raw_fd(),
                target.as_ptr(),
                libc::AT_EMPTY_PATH,
            )
        };
        let linked = if direct_link == 0 {
            true
        } else {
            let descriptor_path = CString::new(format!("/proc/self/fd/{}", file.as_raw_fd()))
                .expect("descriptor path");
            (unsafe {
                libc::linkat(
                    libc::AT_FDCWD,
                    descriptor_path.as_ptr(),
                    self.file.as_raw_fd(),
                    target.as_ptr(),
                    libc::AT_SYMLINK_FOLLOW,
                )
            }) == 0
        };
        if !linked {
            let error = std::io::Error::last_os_error();
            return Err(CapsuleError::runtime(
                "artifact-write-failed",
                format!("cannot publish {name}: {error}"),
            ));
        }
        self.file.sync_all().map_err(|error| {
            CapsuleError::runtime(
                "artifact-write-failed",
                format!("cannot sync capsule directory after publishing {name}: {error}"),
            )
        })?;
        Ok(())
    }

    #[cfg(not(target_os = "linux"))]
    fn write_private_bytes_replacing_platform(
        &self,
        name: &str,
        target: &CString,
        bytes: &[u8],
    ) -> Result<(), CapsuleError> {
        let temporary_name = format!(
            ".artifact.tmp.{}",
            random_artifact_token("artifact-temporary-name-unavailable")?
        );
        let temporary = c_name(OsStr::new(&temporary_name), "artifact-path-unsafe")?;
        let mut file = self.create_new_private(&temporary)?;
        let expected = FileIdentity::from_metadata(&file.metadata().map_err(|error| {
            CapsuleError::runtime(
                "artifact-write-failed",
                format!("cannot inspect temporary artifact: {error}"),
            )
        })?);
        if let Err(error) = write_and_sync_artifact(&mut file, name, bytes) {
            unsafe {
                libc::unlinkat(self.file.as_raw_fd(), temporary.as_ptr(), 0);
            }
            return Err(error);
        }
        if unsafe {
            libc::renameat(
                self.file.as_raw_fd(),
                temporary.as_ptr(),
                self.file.as_raw_fd(),
                target.as_ptr(),
            )
        } != 0
        {
            let error = std::io::Error::last_os_error();
            unsafe {
                libc::unlinkat(self.file.as_raw_fd(), temporary.as_ptr(), 0);
            }
            return Err(CapsuleError::runtime(
                "artifact-write-failed",
                format!("cannot publish {name}: {error}"),
            ));
        }
        let published = self.open_private_regular(
            OsStr::from_bytes(target.as_bytes()),
            "artifact-write-failed",
            "published artifact is not a regular file",
            "artifact-write-failed",
        )?;
        let actual = FileIdentity::from_metadata(&published.metadata().map_err(|error| {
            CapsuleError::runtime(
                "artifact-write-failed",
                format!("cannot inspect published {name}: {error}"),
            )
        })?);
        if expected != actual {
            return Err(CapsuleError::runtime(
                "artifact-write-failed",
                format!("published {name} does not match the private temporary file"),
            ));
        }
        self.file.sync_all().map_err(|error| {
            CapsuleError::runtime(
                "artifact-write-failed",
                format!("cannot sync capsule directory after publishing {name}: {error}"),
            )
        })
    }

    #[cfg(target_os = "linux")]
    fn create_unnamed_private(&self) -> Result<File, CapsuleError> {
        let directory = c".";
        let descriptor = unsafe {
            libc::openat(
                self.file.as_raw_fd(),
                directory.as_ptr(),
                libc::O_WRONLY | libc::O_CLOEXEC | libc::O_TMPFILE,
                0o600,
            )
        };
        if descriptor < 0 {
            return Err(CapsuleError::runtime(
                "artifact-write-failed",
                format!(
                    "cannot create private unnamed artifact: {}",
                    std::io::Error::last_os_error()
                ),
            ));
        }
        let file = unsafe { File::from_raw_fd(descriptor) };
        let metadata = file.metadata().map_err(|error| {
            CapsuleError::runtime(
                "artifact-write-failed",
                format!("cannot inspect private unnamed artifact: {error}"),
            )
        })?;
        if metadata.uid() != unsafe { libc::geteuid() }
            || metadata.nlink() != 0
            || metadata.mode() & 0o077 != 0
        {
            return Err(CapsuleError::runtime(
                "artifact-write-failed",
                "new unnamed artifact is not private",
            ));
        }
        Ok(file)
    }
}

fn write_and_sync_artifact(file: &mut File, name: &str, bytes: &[u8]) -> Result<(), CapsuleError> {
    file.write_all(bytes).map_err(|error| {
        CapsuleError::runtime(
            "artifact-write-failed",
            format!("cannot write {name}: {error}"),
        )
    })?;
    file.sync_all().map_err(|error| {
        CapsuleError::runtime(
            "artifact-write-failed",
            format!("cannot sync {name}: {error}"),
        )
    })
}

fn c_name(name: &OsStr, code: &'static str) -> Result<CString, CapsuleError> {
    if name.as_bytes().contains(&b'/') || name.as_bytes().contains(&0) {
        return Err(CapsuleError::invalid(
            code,
            format!(
                "artifact name must be one path component: {}",
                name.to_string_lossy()
            ),
        ));
    }
    CString::new(name.as_bytes()).map_err(|_| {
        CapsuleError::invalid(
            code,
            format!("path contains a null byte: {}", name.to_string_lossy()),
        )
    })
}

fn canonical_absolute(path: &Path, code: &'static str) -> Result<PathBuf, CapsuleError> {
    if !path.is_absolute() {
        return Err(CapsuleError::invalid(
            code,
            format!("path must be absolute: {}", path.display()),
        ));
    }
    fs::canonicalize(path).map_err(|error| {
        CapsuleError::invalid(code, format!("cannot resolve {}: {error}", path.display()))
    })
}

fn canonical_absolute_dir(path: &Path, code: &'static str) -> Result<PathBuf, CapsuleError> {
    let path = canonical_absolute(path, code)?;
    if !path.is_dir() {
        return Err(CapsuleError::invalid(
            code,
            format!("path is not a directory: {}", path.display()),
        ));
    }
    Ok(path)
}

fn verify_git_preconditions(cwd: &Path, expected: &ExpectedGit) -> Result<(), CapsuleError> {
    if let Some(head) = &expected.head {
        let actual = git_value(cwd, &["rev-parse", "HEAD"])?;
        if &actual != head {
            return Err(CapsuleError::invalid(
                "git-head-mismatch",
                format!("expected Git HEAD {head}, found {actual}"),
            ));
        }
    }
    if let Some(branch) = &expected.branch {
        let actual = git_value(cwd, &["symbolic-ref", "--quiet", "--short", "HEAD"])?;
        if &actual != branch {
            return Err(CapsuleError::invalid(
                "git-branch-mismatch",
                format!("expected Git branch {branch}, found {actual}"),
            ));
        }
    }
    Ok(())
}

fn git_value(cwd: &Path, args: &[&str]) -> Result<String, CapsuleError> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|error| {
            CapsuleError::invalid(
                "git-precondition-unavailable",
                format!("failed to inspect Git state: {error}"),
            )
        })?;
    if !output.status.success() {
        return Err(CapsuleError::invalid(
            "git-precondition-unavailable",
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn sha256_open_file(file: &mut File, path: &Path) -> Result<String, CapsuleError> {
    file.seek(SeekFrom::Start(0)).map_err(|error| {
        CapsuleError::invalid(
            "entrypoint-unreadable",
            format!("cannot rewind {}: {error}", path.display()),
        )
    })?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let count = file.read(&mut buffer).map_err(|error| {
            CapsuleError::invalid(
                "entrypoint-unreadable",
                format!("cannot read {}: {error}", path.display()),
            )
        })?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    file.seek(SeekFrom::Start(0)).map_err(|error| {
        CapsuleError::invalid(
            "entrypoint-unreadable",
            format!("cannot rewind {}: {error}", path.display()),
        )
    })?;
    Ok(format!("sha256:{}", hex(&hasher.finalize())))
}

fn sha256_bytes(bytes: &[u8]) -> String {
    format!("sha256:{}", hex(&Sha256::digest(bytes)))
}

fn random_artifact_token(code: &'static str) -> Result<String, CapsuleError> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).map_err(|error| {
        CapsuleError::runtime(
            code,
            format!("cannot obtain operating-system randomness: {error}"),
        )
    })?;
    Ok(hex(&bytes))
}

fn hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        write!(&mut output, "{byte:02x}").expect("write to string");
    }
    output
}

fn final_report_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["status", "summary", "actions", "validation", "errors", "recommendations"],
        "properties": {
            "status": {"type": "string", "enum": ["succeeded", "failed", "blocked"]},
            "summary": {"type": "string"},
            "actions": {"type": "array", "items": {"type": "string"}},
            "validation": {"type": "array", "items": {"type": "string"}},
            "errors": {"type": "array", "items": {"type": "string"}},
            "recommendations": {"type": "array", "items": {"type": "string"}}
        }
    })
}

fn set_descriptor_inheritable(file: &File, code: &'static str) -> Result<(), CapsuleError> {
    let descriptor = file.as_raw_fd();
    let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFD) };
    if flags < 0 || unsafe { libc::fcntl(descriptor, libc::F_SETFD, flags & !libc::FD_CLOEXEC) } < 0
    {
        return Err(CapsuleError::runtime(
            code,
            format!(
                "cannot make descriptor {descriptor} inheritable: {}",
                std::io::Error::last_os_error()
            ),
        ));
    }
    Ok(())
}

fn child_output_capture(directory: &CapsuleDirectory) -> Result<File, CapsuleError> {
    let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let name = format!(".final.capture.{}.{}", std::process::id(), sequence);
    let file = directory.private_unlinked_output(&name)?;
    set_descriptor_inheritable(&file, "final-capture-unavailable")?;
    Ok(file)
}

fn read_bounded<R: Read>(
    mut reader: R,
    limit: u64,
    unreadable_code: &str,
    too_large_message: &str,
) -> (Vec<u8>, Option<String>) {
    let mut bytes = Vec::new();
    let mut limited = reader.by_ref().take(limit + 1);
    let read_error = limited
        .read_to_end(&mut bytes)
        .err()
        .map(|error| format!("{unreadable_code}: {error}"));
    let oversized = (bytes.len() as u64 > limit).then(|| too_large_message.to_string());
    (bytes, read_error.or(oversized))
}

fn helper_commands(capsule: &ValidatedCapsule) -> Result<HelperCommands, CapsuleError> {
    let executable_path = std::env::current_exe().map_err(|error| {
        CapsuleError::runtime(
            "helper-unavailable",
            format!("cannot resolve the running codex-cli executable: {error}"),
        )
    })?;
    let mut executable = OpenOptions::new()
        .read(true)
        .open(&executable_path)
        .map_err(|error| {
            CapsuleError::runtime(
                "helper-unavailable",
                format!(
                    "cannot pin running codex-cli executable {}: {error}",
                    executable_path.display()
                ),
            )
        })?;
    let metadata = executable.metadata().map_err(|error| {
        CapsuleError::runtime(
            "helper-unavailable",
            format!(
                "cannot inspect running codex-cli executable {}: {error}",
                executable_path.display()
            ),
        )
    })?;
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o111 == 0
    {
        return Err(CapsuleError::runtime(
            "helper-untrusted",
            format!(
                "running codex-cli executable {} must be owner-controlled and executable",
                executable_path.display()
            ),
        ));
    }
    let executable_identity = FileIdentity::from_metadata(&metadata);
    let executable_digest = sha256_open_file(&mut executable, &executable_path)?;
    set_descriptor_inheritable(&executable, "helper-unavailable")?;
    let executable_command = format!("/dev/fd/{}", executable.as_raw_fd());
    if !Path::new(&executable_command).exists() {
        return Err(CapsuleError::runtime(
            "helper-unavailable",
            format!("inherited descriptor path {executable_command} is unavailable"),
        ));
    }
    let nonce_seed = format!(
        "{}:{}:{}:{}",
        capsule.manifest_sha256,
        std::process::id(),
        Utc::now().timestamp_nanos_opt().unwrap_or_default(),
        TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed),
    );
    let nonce = sha256_bytes(nonce_seed.as_bytes())
        .trim_start_matches("sha256:")
        .to_string();
    let base = vec![
        executable_command,
        "agent".to_string(),
        "capsule-exec".to_string(),
        "--capsule".to_string(),
        capsule.root.display().to_string(),
        "--nonce".to_string(),
        nonce.clone(),
    ];
    let script = shell_display(&base);
    let validation = capsule
        .manifest
        .validation
        .iter()
        .enumerate()
        .map(|(index, _)| {
            let mut argv = base.clone();
            argv.push("--validation-index".to_string());
            argv.push(index.to_string());
            shell_display(&argv)
        })
        .collect();
    Ok(HelperCommands {
        nonce,
        script,
        validation,
        executable,
        executable_identity,
        executable_digest,
    })
}

fn verify_helper_integrity(helpers: &mut HelperCommands) -> Result<(), CapsuleError> {
    let metadata = helpers.executable.metadata().map_err(|error| {
        CapsuleError::runtime(
            "helper-integrity-failed",
            format!("cannot inspect pinned codex-cli helper after supervision: {error}"),
        )
    })?;
    if FileIdentity::from_metadata(&metadata) != helpers.executable_identity {
        return Err(CapsuleError::runtime(
            "helper-integrity-failed",
            "pinned codex-cli helper identity or permissions changed during supervision",
        ));
    }
    let digest = sha256_open_file(
        &mut helpers.executable,
        Path::new("pinned codex-cli helper"),
    )?;
    if digest != helpers.executable_digest {
        return Err(CapsuleError::runtime(
            "helper-integrity-failed",
            "pinned codex-cli helper content changed during supervision",
        ));
    }
    Ok(())
}

fn supervisor_prompt(capsule: &ValidatedCapsule, helpers: &HelperCommands) -> String {
    let allowed = capsule
        .allowed_paths
        .iter()
        .map(|path| format!("- {}", path.display()))
        .collect::<Vec<_>>()
        .join("\n");
    let validation = if helpers.validation.is_empty() {
        "- No additional wrapper validation commands were declared.".to_string()
    } else {
        helpers
            .validation
            .iter()
            .enumerate()
            .map(|(index, command)| format!("- Validation command {index}: {command}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    format!(
        "You are supervising an operator-authorized Execution Capsule.\n\
         Read and obey the active home and project instructions, hooks, and repository rules.\n\
         Do not bypass them. Do not modify anything outside the allowed paths below.\n\n\
         Task:\n{}\n\n\
         Capsule: {}\n\
         Working directory: {}\n\
         Access class: {}\n\
         Exact prepared script: {}\n\
         Exact script command: {}\n\n\
         Allowed paths:\n{}\n\n\
         Run the exact script command above as one shell command. It snapshots and verifies \
         run.sh inside the active Codex sandbox before executing those exact bytes. Do not replace, \
         wrap, imitate, or merely echo this command or its attestation. If it fails, diagnose the \
         failure, make only the smallest correction inside allowed_paths, and rerun the same exact \
         script command. After one script attempt succeeds, run every exact validation helper \
         command below as its own shell command:\n{}\n\n\
         Do not edit the capsule or run.sh. Do not silently reinterpret a permission, policy, hook, \
         or concurrent Git-state failure as authorization to bypass the guard.\n\n\
         Return the required structured result with a truthful status, actions, validation, \
         errors, and recommendations. The parent wrapper accepts execution only from matching \
         Codex command_execution events plus helper attestations, verifies final capsule integrity, \
         and writes the receipt.",
        capsule.manifest.task.trim(),
        capsule.root.display(),
        capsule.cwd.display(),
        capsule.manifest.access.as_str(),
        capsule.entrypoint.display(),
        helpers.script,
        allowed,
        validation
    )
}

fn shell_display(argv: &[String]) -> String {
    argv.iter()
        .map(|argument| {
            if argument
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"@%_+=:,./-".contains(&byte))
            {
                argument.clone()
            } else {
                format!("'{}'", argument.replace('\'', "'\"'\"'"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn run_entrypoint_snapshot(capsule: &mut ValidatedCapsule) -> Result<i32, CapsuleError> {
    capsule
        .entrypoint_file
        .seek(SeekFrom::Start(0))
        .map_err(|error| {
            CapsuleError::runtime(
                "entrypoint-unreadable",
                format!("cannot rewind prepared entrypoint: {error}"),
            )
        })?;
    let mut script = Vec::new();
    capsule
        .entrypoint_file
        .read_to_end(&mut script)
        .map_err(|error| {
            CapsuleError::runtime(
                "entrypoint-unreadable",
                format!("cannot snapshot prepared entrypoint: {error}"),
            )
        })?;
    let digest = sha256_bytes(&script);
    if digest != capsule.manifest.entrypoint_sha256 {
        return Err(CapsuleError::runtime(
            "entrypoint-changed",
            format!(
                "entrypoint changed before sandboxed execution: expected {}, found {digest}",
                capsule.manifest.entrypoint_sha256
            ),
        ));
    }
    let mut child = Command::new("bash")
        .args(["-c", "source /dev/stdin"])
        .arg(&capsule.entrypoint)
        .current_dir(&capsule.cwd)
        .env("EXECUTION_CAPSULE_DIR", &capsule.root)
        .env("EXECUTION_CAPSULE_ENTRYPOINT", &capsule.entrypoint)
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|error| {
            CapsuleError::runtime(
                "entrypoint-exec-failed",
                format!("failed to start prepared entrypoint: {error}"),
            )
        })?;
    let mut stdin = child.stdin.take().ok_or_else(|| {
        CapsuleError::runtime(
            "entrypoint-exec-failed",
            "failed to open prepared entrypoint stdin",
        )
    })?;
    stdin.write_all(&script).map_err(|error| {
        CapsuleError::runtime(
            "entrypoint-exec-failed",
            format!("failed to stream prepared entrypoint snapshot: {error}"),
        )
    })?;
    drop(stdin);
    let status = child.wait().map_err(|error| {
        CapsuleError::runtime(
            "entrypoint-exec-failed",
            format!("failed to wait for prepared entrypoint: {error}"),
        )
    })?;
    Ok(status.code().unwrap_or(1))
}

fn run_validation_snapshot(capsule: &ValidatedCapsule, index: usize) -> Result<i32, CapsuleError> {
    let step = capsule.manifest.validation.get(index).ok_or_else(|| {
        CapsuleError::invalid(
            "validation-index-invalid",
            format!("validation index {index} is out of range"),
        )
    })?;
    let (program, arguments) = step.argv.split_first().ok_or_else(|| {
        CapsuleError::invalid("validation-invalid", "validation argv must not be empty")
    })?;
    let status = Command::new(program)
        .args(arguments)
        .current_dir(&capsule.cwd)
        .stdin(Stdio::null())
        .status()
        .map_err(|error| {
            CapsuleError::runtime(
                "validation-exec-failed",
                format!(
                    "failed to run validation {}: {error}",
                    shell_display(&step.argv)
                ),
            )
        })?;
    Ok(status.code().unwrap_or(1))
}

fn read_execution_evidence(
    capsule: &ValidatedCapsule,
    helpers: &HelperCommands,
    events_bytes: &[u8],
) -> (Vec<ScriptRunResult>, Vec<ValidationResult>) {
    let events_path = capsule.root.join("events.jsonl");
    let events = events_path.display().to_string();
    let mut script_runs: Vec<ScriptRunResult> = Vec::new();
    let mut validations: Vec<Option<ValidationResult>> =
        vec![None; capsule.manifest.validation.len()];
    for line in String::from_utf8_lossy(events_bytes).lines() {
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if event["type"] != "item.completed" || event["item"]["type"] != "command_execution" {
            continue;
        }
        let Some(command) = event["item"]["command"].as_str() else {
            continue;
        };
        let event_exit = event["item"]["exit_code"]
            .as_i64()
            .and_then(|code| i32::try_from(code).ok())
            .unwrap_or(1);
        let output = event["item"]["aggregated_output"].as_str().unwrap_or("");
        if event_command_matches(command, &helpers.script) {
            let attestation = find_attestation(output, helpers, "script", None);
            let (exit_code, passed, error) =
                attest_event(attestation, event_exit, &capsule.manifest.entrypoint_sha256);
            for result in &mut script_runs {
                result.terminal = false;
            }
            script_runs.push(ScriptRunResult {
                phase: format!("attempt-{}", script_runs.len() + 1),
                terminal: true,
                exit_code,
                passed,
                command: helpers.script.clone(),
                events: events.clone(),
                error,
            });
            continue;
        }
        for (index, expected) in helpers.validation.iter().enumerate() {
            if !event_command_matches(command, expected) {
                continue;
            }
            let attestation = find_attestation(output, helpers, "validation", Some(index));
            let (exit_code, passed, error) =
                attest_event(attestation, event_exit, &capsule.manifest.entrypoint_sha256);
            let step = &capsule.manifest.validation[index];
            validations[index] = Some(ValidationResult {
                name: step.name.clone(),
                argv: step.argv.clone(),
                exit_code,
                passed,
                command: expected.clone(),
                events: events.clone(),
                error,
            });
        }
    }

    let validation = validations
        .into_iter()
        .enumerate()
        .map(|(index, result)| {
            result.unwrap_or_else(|| {
                let step = &capsule.manifest.validation[index];
                ValidationResult {
                    name: step.name.clone(),
                    argv: step.argv.clone(),
                    exit_code: 127,
                    passed: false,
                    command: helpers.validation[index].clone(),
                    events: events.clone(),
                    error: Some(
                        "no matching sandboxed validation command event and attestation"
                            .to_string(),
                    ),
                }
            })
        })
        .collect();
    (script_runs, validation)
}

fn missing_execution_evidence(
    capsule: &ValidatedCapsule,
    helpers: &HelperCommands,
    error: &str,
) -> (Vec<ScriptRunResult>, Vec<ValidationResult>) {
    let events = capsule.root.join("events.jsonl").display().to_string();
    let validation = capsule
        .manifest
        .validation
        .iter()
        .enumerate()
        .map(|(index, step)| ValidationResult {
            name: step.name.clone(),
            argv: step.argv.clone(),
            exit_code: 127,
            passed: false,
            command: helpers.validation[index].clone(),
            events: events.clone(),
            error: Some(error.to_string()),
        })
        .collect();
    (Vec::new(), validation)
}

fn event_command_matches(observed: &str, expected: &str) -> bool {
    observed == expected
        || observed
            .split_once(" -lc ")
            .is_some_and(|(_, command)| command == expected)
}

fn find_attestation(
    output: &str,
    helpers: &HelperCommands,
    kind: &str,
    validation_index: Option<usize>,
) -> Option<ExecutionAttestation> {
    output.lines().rev().find_map(|line| {
        let attestation = serde_json::from_str::<ExecutionAttestation>(line).ok()?;
        (attestation.schema_version == ATTESTATION_SCHEMA
            && attestation.nonce == helpers.nonce
            && attestation.kind == kind
            && attestation.validation_index == validation_index)
            .then_some(attestation)
    })
}

fn attest_event(
    attestation: Option<ExecutionAttestation>,
    event_exit: i32,
    expected_digest: &str,
) -> (i32, bool, Option<String>) {
    let Some(attestation) = attestation else {
        return (
            event_exit,
            false,
            Some("matching command event did not contain a valid helper attestation".to_string()),
        );
    };
    if attestation.entrypoint_sha256 != expected_digest {
        return (
            event_exit,
            false,
            Some("helper attestation entrypoint digest did not match the manifest".to_string()),
        );
    }
    if attestation.exit_code != event_exit {
        return (
            event_exit,
            false,
            Some(format!(
                "helper attestation exit {} did not match command event exit {event_exit}",
                attestation.exit_code
            )),
        );
    }
    (
        event_exit,
        event_exit == 0,
        attestation.error.filter(|_| event_exit != 0),
    )
}

fn read_final_report_bytes(bytes: &[u8]) -> Result<FinalReport, CapsuleError> {
    if bytes.len() as u64 > MAX_FINAL_BYTES {
        return Err(CapsuleError::runtime(
            "final-report-too-large",
            "Codex final report exceeds 64 KiB",
        ));
    }
    serde_json::from_slice(bytes).map_err(|error| {
        CapsuleError::runtime(
            "final-report-invalid",
            format!("invalid Codex final report: {error}"),
        )
    })
}

fn verify_capsule_integrity(capsule: &ValidatedCapsule) -> Result<(), CapsuleError> {
    let mut manifest = capsule.directory.open_private_regular(
        OsStr::new("manifest.json"),
        "manifest-changed",
        "manifest.json is no longer a regular file",
        "manifest-changed",
    )?;
    let manifest_metadata = manifest.metadata().map_err(|error| {
        CapsuleError::runtime(
            "manifest-changed",
            format!("cannot inspect manifest.json after supervision: {error}"),
        )
    })?;
    if FileIdentity::from_metadata(&manifest_metadata) != capsule.manifest_identity {
        return Err(CapsuleError::runtime(
            "manifest-changed",
            "manifest.json identity or permissions changed during supervision",
        ));
    }
    let mut manifest_bytes = Vec::with_capacity(manifest_metadata.len() as usize);
    manifest.read_to_end(&mut manifest_bytes).map_err(|error| {
        CapsuleError::runtime(
            "manifest-changed",
            format!("cannot reread manifest.json after supervision: {error}"),
        )
    })?;
    if sha256_bytes(&manifest_bytes) != capsule.manifest_sha256 {
        return Err(CapsuleError::runtime(
            "manifest-changed",
            "manifest.json content changed during supervision",
        ));
    }

    let mut entrypoint = capsule.directory.open_private_regular(
        OsStr::new("run.sh"),
        "entrypoint-changed",
        "run.sh is no longer a regular file",
        "entrypoint-changed",
    )?;
    let metadata = entrypoint.metadata().map_err(|error| {
        CapsuleError::runtime(
            "entrypoint-changed",
            format!("cannot inspect run.sh after supervision: {error}"),
        )
    })?;
    if FileIdentity::from_metadata(&metadata) != capsule.entrypoint_identity {
        return Err(CapsuleError::runtime(
            "entrypoint-changed",
            "entrypoint identity or permissions changed during supervision",
        ));
    }
    let digest = sha256_open_file(&mut entrypoint, &capsule.entrypoint)?;
    if digest != capsule.manifest.entrypoint_sha256 {
        return Err(CapsuleError::runtime(
            "entrypoint-changed",
            format!(
                "entrypoint digest changed during supervision: expected {}, found {digest}",
                capsule.manifest.entrypoint_sha256
            ),
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn execution_error(
    codex_exit_code: i32,
    codex_error: Option<&str>,
    evidence_error: Option<&str>,
    final_succeeded: bool,
    final_report_error: Option<&str>,
    helper_integrity_valid: bool,
    integrity_valid: bool,
    script_passed: bool,
    validations_passed: bool,
    receipt_path: &Path,
) -> Value {
    let (code, message) = if let Some(error) = codex_error {
        ("codex-exec-failed", error.to_string())
    } else if codex_exit_code != 0 {
        (
            "codex-exit-nonzero",
            format!("Codex supervisor exited with status {codex_exit_code}"),
        )
    } else if let Some(error) = evidence_error {
        ("execution-evidence-invalid", error.to_string())
    } else if let Some(error) = final_report_error {
        ("final-report-invalid", error.to_string())
    } else if !final_succeeded {
        (
            "supervisor-not-succeeded",
            "Codex supervisor did not report succeeded".to_string(),
        )
    } else if !helper_integrity_valid {
        (
            "helper-integrity-failed",
            "pinned codex-cli helper identity or content changed during supervision".to_string(),
        )
    } else if !integrity_valid {
        (
            "capsule-integrity-failed",
            "capsule identity, permissions, or digest changed during supervision".to_string(),
        )
    } else if !script_passed {
        (
            "script-attestation-failed",
            "no sandboxed prepared-script attempt completed successfully".to_string(),
        )
    } else if !validations_passed {
        (
            "validation-failed",
            "one or more sandboxed validation commands failed".to_string(),
        )
    } else {
        ("execution-failed", "execution capsule failed".to_string())
    };
    json!({
        "code": code,
        "message": message,
        "details": {
            "receipt": receipt_path.display().to_string(),
        }
    })
}

fn render_receipt(receipt: &Value, json_output: bool) {
    if json_output {
        println!(
            "{}",
            serde_json::to_string(receipt).expect("receipt serialization")
        );
        return;
    }
    let ok = receipt["ok"].as_bool().unwrap_or(false);
    let data = &receipt["result"];
    let status = if ok { "succeeded" } else { "failed" };
    println!("execution capsule {status}");
    if let Some(summary) = data["final"]["summary"].as_str() {
        println!("summary: {summary}");
    }
    if let Some(error) = data["codex_error"].as_str() {
        println!("error: {error}");
    }
    if let Some(error) = data["evidence_error"].as_str() {
        println!("execution evidence error: {error}");
    }
    if let Some(error) = data["final_report_error"].as_str() {
        println!("final report error: {error}");
    }
    if let Some(error) = data["helper_integrity_error"].as_str() {
        println!("helper integrity error: {error}");
    }
    if let Some(error) = data["entrypoint_integrity_error"].as_str() {
        println!("integrity error: {error}");
    }
    if let Some(errors) = data["final"]["errors"].as_array() {
        for error in errors.iter().filter_map(Value::as_str) {
            println!("error: {error}");
        }
    }
    if let Some(recommendations) = data["final"]["recommendations"].as_array() {
        for recommendation in recommendations.iter().filter_map(Value::as_str) {
            println!("recommendation: {recommendation}");
        }
    }
    if let Some(script_runs) = data["script_runs"].as_array() {
        for script_run in script_runs {
            if !script_run["passed"].as_bool().unwrap_or(false) {
                println!(
                    "script {} failed with exit {} (events: {})",
                    script_run["phase"].as_str().unwrap_or("unknown"),
                    script_run["exit_code"].as_i64().unwrap_or(1),
                    script_run["events"].as_str().unwrap_or("unknown"),
                );
            }
        }
    }
    if let Some(validation) = data["validation"].as_array() {
        for step in validation {
            if !step["passed"].as_bool().unwrap_or(false) {
                println!(
                    "validation failed with exit {} (events: {})",
                    step["exit_code"].as_i64().unwrap_or(1),
                    step["events"].as_str().unwrap_or("unknown"),
                );
            }
        }
    }
    if let Some(path) = data["artifacts"]["receipt"].as_str() {
        println!("receipt: {path}");
    }
    if let Some(path) = data["artifacts"]["events"].as_str() {
        println!("events: {path}");
    }
}

fn render_error(error: &CapsuleError, json_output: bool) {
    if json_output {
        println!(
            "{}",
            serde_json::to_string(&json!({
                "schema_version": ERROR_SCHEMA,
                "command": "agent run",
                "ok": false,
                "error": {
                    "code": error.code,
                    "message": error.message,
                }
            }))
            .expect("error serialization")
        );
    } else {
        eprintln!("codex-cli agent run: {}: {}", error.code, error.message);
    }
}
