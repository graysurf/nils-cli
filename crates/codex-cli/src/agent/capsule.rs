use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const MANIFEST_SCHEMA: &str = "execution-capsule.v1";
const RECEIPT_SCHEMA: &str = "cli.codex-cli.execution-capsule.receipt.v1";
const ERROR_SCHEMA: &str = "cli.codex-cli.execution-capsule.error.v1";
const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
const MAX_FINAL_BYTES: u64 = 64 * 1024;

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
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
struct ArtifactPaths {
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
    validation: Vec<ValidationResult>,
    final_report_valid: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    final_report_error: Option<String>,
    #[serde(rename = "final")]
    final_report: Option<FinalReport>,
    artifacts: ArtifactPaths,
    completed_at: String,
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
    manifest: Manifest,
    manifest_sha256: String,
    cwd: PathBuf,
    entrypoint: PathBuf,
    allowed_paths: Vec<PathBuf>,
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
    let capsule = validate_capsule(&options.capsule)?;
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
    write_private_json(&schema_path, &final_report_schema())?;
    drop(private_output_file(&final_path)?);
    drop(private_output_file(&receipt_path)?);
    let events = private_output_file(&events_path)?;

    crate::runtime::refresh_remote_auth_before_exec();
    let prompt = supervisor_prompt(&capsule);
    let status = Command::new("codex")
        .args(["--ask-for-approval", "never", "exec", "-C"])
        .arg(&capsule.cwd)
        .args(["--sandbox", capsule.manifest.access.sandbox(), "--json"])
        .args(["--output-schema"])
        .arg(&schema_path)
        .args(["--output-last-message"])
        .arg(&final_path)
        .args(["--", &prompt])
        .stdin(Stdio::null())
        .stdout(Stdio::from(events))
        .stderr(Stdio::inherit())
        .status()
        .map_err(|error| {
            CapsuleError::runtime(
                "codex-exec-failed",
                format!("failed to start Codex supervisor: {error}"),
            )
        })?;
    let codex_exit_code = status.code().unwrap_or(1);

    let (final_report, final_report_error) = match read_final_report(&final_path) {
        Ok(report) => (Some(report), None),
        Err(error) => (None, Some(format!("{}: {}", error.code, error.message))),
    };
    let validation = run_validation(&capsule);
    let validations_passed = validation.iter().all(|result| result.passed);
    let final_succeeded = final_report
        .as_ref()
        .is_some_and(|report| report.status == FinalStatus::Succeeded);
    let ok = codex_exit_code == 0 && final_succeeded && validations_passed;

    let response = json!({
        "schema_version": RECEIPT_SCHEMA,
        "ok": ok,
        "data": ReceiptData {
            capsule: capsule.root.display().to_string(),
            manifest_sha256: capsule.manifest_sha256,
            entrypoint_sha256: capsule.manifest.entrypoint_sha256,
            access: capsule.manifest.access.as_str(),
            codex_exit_code,
            validation,
            final_report_valid: final_report.is_some(),
            final_report_error,
            final_report,
            artifacts: ArtifactPaths {
                events: events_path.display().to_string(),
                final_report: final_path.display().to_string(),
                receipt: receipt_path.display().to_string(),
            },
            completed_at: Utc::now().to_rfc3339(),
        }
    });
    write_private_json(&receipt_path, &response)?;
    render_receipt(&response, options.json);
    Ok(if ok { 0 } else { 1 })
}

fn validate_capsule(path: &Path) -> Result<ValidatedCapsule, CapsuleError> {
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

    let manifest_path = root.join("manifest.json");
    let manifest_metadata = regular_file_metadata(
        &manifest_path,
        "manifest-unreadable",
        "manifest.json must be a regular file",
    )?;
    require_owner_only(&manifest_path, &manifest_metadata, "manifest-not-private")?;
    if manifest_metadata.len() > MAX_MANIFEST_BYTES {
        return Err(CapsuleError::invalid(
            "manifest-too-large",
            "manifest.json exceeds 64 KiB",
        ));
    }
    let manifest_bytes = fs::read(&manifest_path).map_err(|error| {
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
    let entrypoint_metadata = regular_file_metadata(
        &entrypoint_path,
        "entrypoint-unreadable",
        "run.sh must be a regular file",
    )?;
    require_owner_only(
        &entrypoint_path,
        &entrypoint_metadata,
        "entrypoint-not-private",
    )?;
    if entrypoint_metadata.mode() & 0o100 == 0 {
        return Err(CapsuleError::invalid(
            "entrypoint-not-executable",
            "run.sh must be executable by its owner",
        ));
    }
    let entrypoint = fs::canonicalize(&entrypoint_path).map_err(|error| {
        CapsuleError::invalid(
            "entrypoint-unreadable",
            format!("cannot resolve {}: {error}", entrypoint_path.display()),
        )
    })?;
    if !entrypoint.starts_with(&root) {
        return Err(CapsuleError::invalid(
            "entrypoint-escape",
            "entrypoint resolves outside the capsule",
        ));
    }
    let actual_digest = sha256_file(&entrypoint)?;
    if actual_digest != manifest.entrypoint_sha256 {
        return Err(CapsuleError::invalid(
            "entrypoint-digest-mismatch",
            format!(
                "run.sh digest mismatch: expected {}, found {actual_digest}",
                manifest.entrypoint_sha256
            ),
        ));
    }

    if let Some(expected) = &manifest.expected_git {
        verify_git_preconditions(&cwd, expected)?;
    }

    Ok(ValidatedCapsule {
        root,
        manifest,
        manifest_sha256: sha256_bytes(&manifest_bytes),
        cwd,
        entrypoint,
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

fn regular_file_metadata(
    path: &Path,
    code: &'static str,
    message: &'static str,
) -> Result<fs::Metadata, CapsuleError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        CapsuleError::invalid(code, format!("cannot inspect {}: {error}", path.display()))
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(CapsuleError::invalid(code, message));
    }
    Ok(metadata)
}

fn require_owner_only(
    path: &Path,
    metadata: &fs::Metadata,
    code: &'static str,
) -> Result<(), CapsuleError> {
    if metadata.mode() & 0o077 != 0 {
        return Err(CapsuleError::invalid(
            code,
            format!("{} must not grant group or other access", path.display()),
        ));
    }
    Ok(())
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

fn sha256_file(path: &Path) -> Result<String, CapsuleError> {
    let mut file = File::open(path).map_err(|error| {
        CapsuleError::invalid(
            "entrypoint-unreadable",
            format!("cannot read {}: {error}", path.display()),
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
    Ok(format!("sha256:{}", hex(&hasher.finalize())))
}

fn sha256_bytes(bytes: &[u8]) -> String {
    format!("sha256:{}", hex(&Sha256::digest(bytes)))
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

fn supervisor_prompt(capsule: &ValidatedCapsule) -> String {
    let allowed = capsule
        .allowed_paths
        .iter()
        .map(|path| format!("- {}", path.display()))
        .collect::<Vec<_>>()
        .join("\n");
    let validation = if capsule.manifest.validation.is_empty() {
        "- No additional wrapper validation commands were declared.".to_string()
    } else {
        capsule
            .manifest
            .validation
            .iter()
            .map(|step| format!("- {}", shell_display(&step.argv)))
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
         Exact prepared script: {}\n\n\
         Allowed paths:\n{}\n\n\
         Run the prepared script with `bash {}`. If it fails, diagnose the failure, make only the \
         smallest in-scope correction, and rerun it. Do not silently reinterpret a permission, \
         policy, hook, or concurrent Git-state failure as authorization to bypass the guard. \
         Then run or confirm these validations:\n{}\n\n\
         Return the required structured result with a truthful status, actions, validation, \
         errors, and recommendations. The parent wrapper will independently rerun declared \
         validation commands and write the receipt.",
        capsule.manifest.task.trim(),
        capsule.root.display(),
        capsule.cwd.display(),
        capsule.manifest.access.as_str(),
        capsule.entrypoint.display(),
        allowed,
        capsule.entrypoint.display(),
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

fn read_final_report(path: &Path) -> Result<FinalReport, CapsuleError> {
    let metadata = fs::metadata(path).map_err(|error| {
        CapsuleError::runtime(
            "final-report-missing",
            format!("Codex did not write {}: {error}", path.display()),
        )
    })?;
    if metadata.len() > MAX_FINAL_BYTES {
        return Err(CapsuleError::runtime(
            "final-report-too-large",
            "Codex final report exceeds 64 KiB",
        ));
    }
    let bytes = fs::read(path).map_err(|error| {
        CapsuleError::runtime(
            "final-report-unreadable",
            format!("cannot read {}: {error}", path.display()),
        )
    })?;
    serde_json::from_slice(&bytes).map_err(|error| {
        CapsuleError::runtime(
            "final-report-invalid",
            format!("invalid Codex final report: {error}"),
        )
    })
}

fn run_validation(capsule: &ValidatedCapsule) -> Vec<ValidationResult> {
    let mut results = Vec::with_capacity(capsule.manifest.validation.len());
    for step in &capsule.manifest.validation {
        let Some((program, arguments)) = step.argv.split_first() else {
            continue;
        };
        let status = Command::new(program)
            .args(arguments)
            .current_dir(&capsule.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status();
        match status {
            Ok(status) => {
                let exit_code = status.code().unwrap_or(1);
                results.push(ValidationResult {
                    name: step.name.clone(),
                    argv: step.argv.clone(),
                    exit_code,
                    passed: status.success(),
                    error: None,
                });
            }
            Err(error) => results.push(ValidationResult {
                name: step.name.clone(),
                argv: step.argv.clone(),
                exit_code: 127,
                passed: false,
                error: Some(format!(
                    "failed to run validation {}: {error}",
                    shell_display(&step.argv)
                )),
            }),
        }
    }
    results
}

fn prepare_private_output(path: &Path) -> Result<(), CapsuleError> {
    if let Ok(metadata) = fs::symlink_metadata(path)
        && (!metadata.is_file() || metadata.file_type().is_symlink())
    {
        return Err(CapsuleError::invalid(
            "artifact-path-unsafe",
            format!("refusing non-regular artifact path {}", path.display()),
        ));
    }
    Ok(())
}

fn private_output_file(path: &Path) -> Result<File, CapsuleError> {
    prepare_private_output(path)?;
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .map_err(|error| {
            CapsuleError::runtime(
                "artifact-write-failed",
                format!("cannot open {}: {error}", path.display()),
            )
        })?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|error| {
        CapsuleError::runtime(
            "artifact-write-failed",
            format!("cannot secure {}: {error}", path.display()),
        )
    })?;
    Ok(file)
}

fn write_private_json(path: &Path, value: &Value) -> Result<(), CapsuleError> {
    let mut file = private_output_file(path)?;
    serde_json::to_writer_pretty(&mut file, value).map_err(|error| {
        CapsuleError::runtime(
            "artifact-write-failed",
            format!("cannot encode {}: {error}", path.display()),
        )
    })?;
    file.write_all(b"\n").map_err(|error| {
        CapsuleError::runtime(
            "artifact-write-failed",
            format!("cannot write {}: {error}", path.display()),
        )
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
    let data = &receipt["data"];
    let status = if ok { "succeeded" } else { "failed" };
    println!("execution capsule {status}");
    if let Some(summary) = data["final"]["summary"].as_str() {
        println!("summary: {summary}");
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
        eprintln!(
            "{}",
            serde_json::to_string(&json!({
                "schema_version": ERROR_SCHEMA,
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
