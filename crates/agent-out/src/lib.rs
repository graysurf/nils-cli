mod cli;
mod completion;

use std::collections::BTreeSet;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;

use chrono::Local;
use clap::Parser;
use clap::error::ErrorKind;
use serde::Serialize;
use serde_json::{Value, json};

use cli::{AuditArgs, AuditFormat, Cli, Command, ProjectArgs, ProjectFormat};

use nils_common::cli_contract::exit;

const EXIT_OK: i32 = exit::SUCCESS;
const EXIT_AUDIT_VIOLATIONS: i32 = exit::RUNTIME;
const EXIT_RUNTIME: i32 = exit::RUNTIME;
const EXIT_USAGE: i32 = exit::USAGE;

const PROJECT_SCHEMA_VERSION: &str = "cli.agent-out.project.v1";
const AUDIT_SCHEMA_VERSION: &str = "cli.agent-out.audit.v1";
const PROJECT_COMMAND: &str = "agent-out project";
const AUDIT_COMMAND: &str = "agent-out audit";

const CANONICAL_PROJECT_ROOT: &str = "projects";
const ALLOWLISTED_TOOL_ROOTS: &[&str] = &[
    "agent-browser",
    "api-test-runner",
    "delegate-parallel",
    "image-processing",
    "macos-agent-trace",
    "plan-issue-delivery",
    "plan-issue-sprint-pr",
    "playwright",
    "screen-record",
    "screenshot",
    "semgrep",
    "tests",
    "workspace-shared-audit",
    "workspace-test-cleanup",
];

pub fn run() -> i32 {
    run_with_args(env::args_os())
}

pub fn run_with_args<I, T>(args: I) -> i32
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(err) => {
            let code = match err.kind() {
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => err.exit_code(),
                _ => EXIT_USAGE,
            };
            let _ = err.print();
            return code;
        }
    };

    dispatch(cli)
}

pub fn project_dir_for_current_repo(topic: &str, mkdir: bool) -> Result<ProjectResult, String> {
    let agent_home = resolve_agent_home(None).map_err(|err| err.message)?;
    let repo = resolve_repo(None).map_err(|err| err.message)?;
    let input = ProjectPathInput {
        agent_home,
        repo,
        explicit_repo_slug: None,
        topic: topic.to_string(),
        timestamp: current_timestamp(),
        mkdir,
    };
    build_project_path(input).map_err(|err| err.message)
}

fn dispatch(cli: Cli) -> i32 {
    match cli.command {
        Command::Project(args) => run_project(args),
        Command::Audit(args) => run_audit(args),
        Command::Completion(args) => completion::run(args.shell),
    }
}

fn run_project(args: ProjectArgs) -> i32 {
    match build_project_result(&args) {
        Ok(result) => render_project_success(args.format, &result),
        Err(err) => render_error(PROJECT_SCHEMA_VERSION, PROJECT_COMMAND, args.format, err),
    }
}

fn run_audit(args: AuditArgs) -> i32 {
    let report = match build_audit_report(&args) {
        Ok(report) => report,
        Err(err) => return render_error(AUDIT_SCHEMA_VERSION, AUDIT_COMMAND, args.format, err),
    };

    match args.format {
        AuditFormat::Text => {
            println!("{}", render_audit_text(&report));
            if args.strict && report.summary.violations > 0 {
                EXIT_AUDIT_VIOLATIONS
            } else {
                EXIT_OK
            }
        }
        AuditFormat::Json => {
            if args.strict && report.summary.violations > 0 {
                let details = json!({
                    "agent_home": report.agent_home,
                    "out_root": report.out_root,
                    "summary": report.summary,
                    "violations": report.violations,
                });
                print_json_error(
                    AUDIT_SCHEMA_VERSION,
                    AUDIT_COMMAND,
                    "audit-violations",
                    "noncanonical AGENT_HOME/out entries found",
                    Some(details),
                    EXIT_AUDIT_VIOLATIONS,
                )
                .unwrap_or_else(render_json_failure)
            } else {
                print_json_success(AUDIT_SCHEMA_VERSION, AUDIT_COMMAND, &report)
                    .unwrap_or_else(render_json_failure)
            }
        }
    }
}

fn render_project_success(format: ProjectFormat, result: &ProjectResult) -> i32 {
    match format {
        ProjectFormat::Path => {
            println!("{}", result.path);
            EXIT_OK
        }
        ProjectFormat::Json => print_json_success(PROJECT_SCHEMA_VERSION, PROJECT_COMMAND, result)
            .unwrap_or_else(render_json_failure),
        ProjectFormat::Env => {
            println!("{}", render_project_env(result));
            EXIT_OK
        }
    }
}

fn render_error<T>(
    schema_version: &'static str,
    command: &'static str,
    format: T,
    err: CliError,
) -> i32
where
    T: JsonFormat,
{
    if format.is_json() {
        return print_json_error(
            schema_version,
            command,
            err.code,
            &err.message,
            err.details,
            err.exit_code,
        )
        .unwrap_or_else(render_json_failure);
    }

    eprintln!("agent-out: error: {}", err.message);
    err.exit_code
}

trait JsonFormat {
    fn is_json(&self) -> bool;
}

impl JsonFormat for ProjectFormat {
    fn is_json(&self) -> bool {
        *self == ProjectFormat::Json
    }
}

impl JsonFormat for AuditFormat {
    fn is_json(&self) -> bool {
        *self == AuditFormat::Json
    }
}

#[derive(Debug)]
struct CliError {
    code: &'static str,
    message: String,
    details: Option<Value>,
    exit_code: i32,
}

impl CliError {
    fn usage(code: &'static str, message: impl Into<String>, details: Option<Value>) -> Self {
        Self {
            code,
            message: message.into(),
            details,
            exit_code: EXIT_USAGE,
        }
    }

    fn runtime(code: &'static str, message: impl Into<String>, details: Option<Value>) -> Self {
        Self {
            code,
            message: message.into(),
            details,
            exit_code: EXIT_RUNTIME,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ProjectResult {
    pub path: String,
    pub agent_home: String,
    pub out_root: String,
    pub repo: String,
    pub project_slug: String,
    pub topic: String,
    pub run_id: String,
    pub created: bool,
}

#[derive(Debug)]
struct ProjectPathInput {
    agent_home: PathBuf,
    repo: PathBuf,
    explicit_repo_slug: Option<String>,
    topic: String,
    timestamp: String,
    mkdir: bool,
}

fn build_project_result(args: &ProjectArgs) -> Result<ProjectResult, CliError> {
    let agent_home = resolve_agent_home(args.agent_home.as_deref())?;
    let repo = resolve_repo(args.repo.as_deref())?;
    let input = ProjectPathInput {
        agent_home,
        repo,
        explicit_repo_slug: args.repo_slug.clone(),
        topic: args.topic.clone(),
        timestamp: current_timestamp(),
        mkdir: args.mkdir,
    };
    build_project_path(input)
}

fn build_project_path(input: ProjectPathInput) -> Result<ProjectResult, CliError> {
    let project_slug = resolve_project_slug(input.explicit_repo_slug.as_deref(), &input.repo)?;
    let topic = sanitize_topic(&input.topic);
    let run_id = format!("{}-{}", input.timestamp, topic);
    let out_root = input.agent_home.join("out");
    let path = out_root
        .join(CANONICAL_PROJECT_ROOT)
        .join(&project_slug)
        .join(&run_id);

    if input.mkdir {
        fs::create_dir_all(&path).map_err(|err| {
            CliError::runtime(
                "mkdir-failed",
                format!("failed to create {}: {err}", path.display()),
                Some(json!({ "path": display_path(&path) })),
            )
        })?;
    }

    Ok(ProjectResult {
        path: display_path(&path),
        agent_home: display_path(&input.agent_home),
        out_root: display_path(&out_root),
        repo: display_path(&input.repo),
        project_slug,
        topic,
        run_id,
        created: input.mkdir,
    })
}

fn current_timestamp() -> String {
    Local::now().format("%Y%m%d-%H%M%S").to_string()
}

fn resolve_agent_home(explicit: Option<&Path>) -> Result<PathBuf, CliError> {
    if let Some(path) = explicit {
        return absolute_path(path);
    }

    match env::var_os("AGENT_HOME") {
        Some(value) if !value.is_empty() => absolute_path(Path::new(&value)),
        _ => Err(CliError::usage(
            "missing-agent-home",
            "AGENT_HOME is required; pass --agent-home or set AGENT_HOME",
            Some(json!({ "env": "AGENT_HOME", "flag": "--agent-home" })),
        )),
    }
}

fn resolve_repo(explicit: Option<&Path>) -> Result<PathBuf, CliError> {
    match explicit {
        Some(path) => absolute_path(path),
        None => env::current_dir().map_err(|err| {
            CliError::runtime(
                "cwd-unavailable",
                format!("failed to read current directory: {err}"),
                None,
            )
        }),
    }
}

fn absolute_path(path: &Path) -> Result<PathBuf, CliError> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        env::current_dir()
            .map_err(|err| {
                CliError::runtime(
                    "cwd-unavailable",
                    format!("failed to read current directory: {err}"),
                    None,
                )
            })?
            .join(path)
    };

    Ok(fs::canonicalize(&absolute).unwrap_or(absolute))
}

fn resolve_project_slug(explicit: Option<&str>, repo: &Path) -> Result<String, CliError> {
    if let Some(slug) = explicit {
        return project_slug_from_owner_repo(slug).ok_or_else(|| {
            CliError::usage(
                "invalid-repo-slug",
                "--repo-slug must include at least one path-safe segment",
                Some(json!({ "repo_slug": slug })),
            )
        });
    }

    if let Some(remote) = git_origin_url(repo)
        && let Some(slug) = project_slug_from_remote_url(&remote)
    {
        return Ok(slug);
    }

    Ok(local_project_slug(repo))
}

fn git_origin_url(repo: &Path) -> Option<String> {
    let output = ProcessCommand::new("git")
        .arg("-C")
        .arg(repo)
        .args(["remote", "get-url", "origin"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let remote = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if remote.is_empty() {
        None
    } else {
        Some(remote)
    }
}

pub fn project_slug_from_remote_url(remote: &str) -> Option<String> {
    let trimmed = remote.trim().trim_end_matches(".git");
    let path = if let Some((_, after_scheme)) = trimmed.split_once("://") {
        after_scheme.split_once('/').map(|(_, rest)| rest)?
    } else if let Some((_, scp_path)) = trimmed.split_once(':') {
        scp_path
    } else {
        trimmed
    };

    project_slug_from_owner_repo(path)
}

pub fn project_slug_from_owner_repo(value: &str) -> Option<String> {
    let mut parts: Vec<&str> = value
        .trim()
        .trim_end_matches(".git")
        .split('/')
        .filter(|part| !part.trim().is_empty())
        .collect();

    if parts.len() >= 2 {
        let repo = parts.pop().expect("repo segment");
        let owner = parts.pop().expect("owner segment");
        let owner = sanitize_path_label(owner, "");
        let repo = sanitize_path_label(repo, "");
        if owner.is_empty() || repo.is_empty() {
            return None;
        }
        return Some(format!("{owner}__{repo}"));
    }

    let slug = sanitize_path_label(value, "");
    if slug.is_empty() { None } else { Some(slug) }
}

fn local_project_slug(repo: &Path) -> String {
    let basename = repo
        .file_name()
        .and_then(|value| value.to_str())
        .map(|value| sanitize_path_label(value, "repo"))
        .unwrap_or_else(|| "repo".to_string());
    let hash = stable_short_hash(&display_path(repo));
    format!("local__{basename}-{hash}")
}

pub fn sanitize_topic(topic: &str) -> String {
    sanitize_path_label(topic, "untitled")
}

fn sanitize_path_label(value: &str, fallback: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;

    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }

    let trimmed = out.trim_matches('-');
    let mut sanitized: String = trimmed.chars().take(80).collect();
    sanitized = sanitized.trim_matches('-').to_string();

    if sanitized.is_empty() {
        fallback.to_string()
    } else {
        sanitized
    }
}

fn stable_short_hash(value: &str) -> String {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{:08x}", hash as u32)
}

#[derive(Debug, Serialize)]
pub struct AuditReport {
    pub agent_home: String,
    pub out_root: String,
    pub out_root_exists: bool,
    pub allowed_roots: Vec<AuditEntry>,
    pub violations: Vec<AuditEntry>,
    pub summary: AuditSummary,
}

#[derive(Debug, Serialize)]
pub struct AuditSummary {
    pub allowed_roots: usize,
    pub violations: usize,
}

#[derive(Debug, Serialize)]
pub struct AuditEntry {
    pub name: String,
    pub path: String,
    pub kind: String,
    pub classification: String,
    pub reason: String,
}

fn build_audit_report(args: &AuditArgs) -> Result<AuditReport, CliError> {
    let agent_home = resolve_agent_home(args.agent_home.as_deref())?;
    let out_root = agent_home.join("out");
    let mut allowed_roots = Vec::new();
    let mut violations = Vec::new();

    if !out_root.exists() {
        return Ok(AuditReport {
            agent_home: display_path(&agent_home),
            out_root: display_path(&out_root),
            out_root_exists: false,
            allowed_roots,
            violations,
            summary: AuditSummary {
                allowed_roots: 0,
                violations: 0,
            },
        });
    }

    if !out_root.is_dir() {
        return Err(CliError::runtime(
            "out-root-not-directory",
            format!("{} is not a directory", out_root.display()),
            Some(json!({ "out_root": display_path(&out_root) })),
        ));
    }

    let allowlisted: BTreeSet<&str> = ALLOWLISTED_TOOL_ROOTS.iter().copied().collect();
    let mut entries = Vec::new();
    for entry in fs::read_dir(&out_root).map_err(|err| {
        CliError::runtime(
            "audit-read-failed",
            format!("failed to read {}: {err}", out_root.display()),
            Some(json!({ "out_root": display_path(&out_root) })),
        )
    })? {
        let entry = entry.map_err(|err| {
            CliError::runtime(
                "audit-read-failed",
                format!("failed to read entry under {}: {err}", out_root.display()),
                Some(json!({ "out_root": display_path(&out_root) })),
            )
        })?;
        entries.push(entry.path());
    }
    entries.sort();

    for path in entries {
        let name = path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("")
            .to_string();
        let kind = entry_kind(&path);

        if name == CANONICAL_PROJECT_ROOT && path.is_dir() {
            allowed_roots.push(AuditEntry {
                name,
                path: display_path(&path),
                kind,
                classification: "canonical-project-root".to_string(),
                reason: "canonical project-scoped artifact root".to_string(),
            });
        } else if allowlisted.contains(name.as_str()) && path.is_dir() {
            allowed_roots.push(AuditEntry {
                name,
                path: display_path(&path),
                kind,
                classification: "allowlisted-tool-root".to_string(),
                reason: "preserved tool/workflow artifact root".to_string(),
            });
        } else {
            violations.push(AuditEntry {
                name,
                path: display_path(&path),
                kind,
                classification: "noncanonical".to_string(),
                reason: "top-level out entry is not projects/ or an allowlisted tool/workflow root"
                    .to_string(),
            });
        }
    }

    let summary = AuditSummary {
        allowed_roots: allowed_roots.len(),
        violations: violations.len(),
    };

    Ok(AuditReport {
        agent_home: display_path(&agent_home),
        out_root: display_path(&out_root),
        out_root_exists: true,
        allowed_roots,
        violations,
        summary,
    })
}

fn entry_kind(path: &Path) -> String {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => "symlink".to_string(),
        Ok(metadata) if metadata.is_dir() => "directory".to_string(),
        Ok(metadata) if metadata.is_file() => "file".to_string(),
        Ok(_) => "other".to_string(),
        Err(_) => "unknown".to_string(),
    }
}

fn render_audit_text(report: &AuditReport) -> String {
    let mut lines = vec![
        format!("agent_home: {}", report.agent_home),
        format!("out_root: {}", report.out_root),
        format!("out_root_exists: {}", report.out_root_exists),
        "allowlisted:".to_string(),
    ];

    if report.allowed_roots.is_empty() {
        lines.push("  none".to_string());
    } else {
        for entry in &report.allowed_roots {
            lines.push(format!(
                "  - {} ({}, {}): {}",
                entry.name, entry.kind, entry.classification, entry.reason
            ));
        }
    }

    lines.push("violations:".to_string());
    if report.violations.is_empty() {
        lines.push("  none".to_string());
    } else {
        for entry in &report.violations {
            lines.push(format!(
                "  - {} ({}, {}): {}",
                entry.name, entry.kind, entry.classification, entry.reason
            ));
        }
    }

    lines.push(format!(
        "summary: allowed_roots={} violations={}",
        report.summary.allowed_roots, report.summary.violations
    ));
    lines.join("\n")
}

fn render_project_env(result: &ProjectResult) -> String {
    [
        ("AGENT_OUT_PATH", result.path.as_str()),
        ("AGENT_OUT_ROOT", result.out_root.as_str()),
        ("AGENT_OUT_PROJECT_SLUG", result.project_slug.as_str()),
        ("AGENT_OUT_TOPIC", result.topic.as_str()),
        ("AGENT_OUT_RUN_ID", result.run_id.as_str()),
    ]
    .into_iter()
    .map(|(key, value)| format!("{key}={}", shell_quote(value)))
    .collect::<Vec<_>>()
    .join("\n")
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn print_json_success<T: Serialize>(
    schema_version: &'static str,
    command: &'static str,
    result: &T,
) -> Result<i32, serde_json::Error> {
    let envelope = SuccessEnvelope {
        schema_version,
        command,
        ok: true,
        result,
    };
    println!("{}", serde_json::to_string_pretty(&envelope)?);
    Ok(EXIT_OK)
}

fn print_json_error(
    schema_version: &'static str,
    command: &'static str,
    code: &'static str,
    message: &str,
    details: Option<Value>,
    exit_code: i32,
) -> Result<i32, serde_json::Error> {
    let envelope = ErrorEnvelope {
        schema_version,
        command,
        ok: false,
        error: ErrorBody {
            code,
            message,
            details,
        },
    };
    println!("{}", serde_json::to_string_pretty(&envelope)?);
    Ok(exit_code)
}

fn render_json_failure(err: serde_json::Error) -> i32 {
    eprintln!("agent-out: error: failed to render json: {err}");
    EXIT_RUNTIME
}

#[derive(Serialize)]
struct SuccessEnvelope<'a, T: Serialize> {
    schema_version: &'static str,
    command: &'static str,
    ok: bool,
    result: &'a T,
}

#[derive(Serialize)]
struct ErrorEnvelope<'a> {
    schema_version: &'static str,
    command: &'static str,
    ok: bool,
    error: ErrorBody<'a>,
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    code: &'static str,
    message: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    details: Option<Value>,
}

fn display_path(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topic_sanitization_is_path_safe() {
        assert_eq!(
            sanitize_topic("Fix API/Test Output!!"),
            "fix-api-test-output"
        );
        assert_eq!(sanitize_topic("   "), "untitled");
        assert_eq!(sanitize_topic("../secret"), "secret");
    }

    #[test]
    fn explicit_owner_repo_slug_uses_double_underscore() {
        assert_eq!(
            project_slug_from_owner_repo("Sympoies/Nils CLI"),
            Some("sympoies__nils-cli".to_string())
        );
    }

    #[test]
    fn remote_url_slug_supports_https_and_scp_forms() {
        assert_eq!(
            project_slug_from_remote_url("https://github.com/sympoies/nils-cli.git"),
            Some("sympoies__nils-cli".to_string())
        );
        assert_eq!(
            project_slug_from_remote_url("git@github.com:sympoies/nils-cli.git"),
            Some("sympoies__nils-cli".to_string())
        );
    }

    #[test]
    fn local_slug_is_stable_and_namespaced() {
        let slug = local_project_slug(Path::new("/tmp/Nils CLI"));
        assert!(
            slug.starts_with("local__nils-cli-"),
            "unexpected local slug: {slug}"
        );
        assert_eq!(slug.len(), "local__nils-cli-".len() + 8);
    }

    #[test]
    fn project_path_uses_canonical_shape() {
        let input = ProjectPathInput {
            agent_home: PathBuf::from("/agent-home"),
            repo: PathBuf::from("/repo"),
            explicit_repo_slug: Some("owner/repo".to_string()),
            topic: "Bug Sweep".to_string(),
            timestamp: "20260511-121314".to_string(),
            mkdir: false,
        };

        let result = build_project_path(input).expect("project path");
        assert_eq!(result.project_slug, "owner__repo");
        assert_eq!(result.topic, "bug-sweep");
        assert_eq!(result.run_id, "20260511-121314-bug-sweep");
        assert_eq!(
            result.path,
            "/agent-home/out/projects/owner__repo/20260511-121314-bug-sweep"
        );
    }
}
