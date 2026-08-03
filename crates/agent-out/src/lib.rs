mod cli;
mod completion;

use std::collections::BTreeSet;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::Command as ProcessCommand;
use std::time::UNIX_EPOCH;

use chrono::Local;
use clap::Parser;
use clap::error::ErrorKind;
// Project-slug normalization now lives in the shared crate so `nils-evidence`
// can reuse the exact same rule. Re-export the public helpers to preserve the
// `agent_out::project_slug_*` API.
use nils_common::slug::sanitize_path_label;
pub use nils_common::slug::{project_slug_from_owner_repo, project_slug_from_remote_url};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use cli::{
    AuditArgs, AuditFormat, CleanupApplyArgs, CleanupFormat, CleanupPlanArgs, Cli, Command,
    PathForArgs, ProjectArgs, ProjectFormat,
};

use nils_common::cli_contract::exit;
use nils_common::fs::display_path;

const EXIT_OK: i32 = exit::SUCCESS;
const EXIT_AUDIT_VIOLATIONS: i32 = exit::RUNTIME;
const EXIT_RUNTIME: i32 = exit::RUNTIME;
const EXIT_USAGE: i32 = exit::USAGE;
const EXIT_DATA: i32 = exit::DATA;

const PROJECT_SCHEMA_VERSION: &str = "cli.agent-out.project.v1";
const PATH_FOR_SCHEMA_VERSION: &str = "cli.agent-out.path-for.v1";
const AUDIT_SCHEMA_VERSION: &str = "cli.agent-out.audit.v1";
const CLEANUP_PLAN_SCHEMA_VERSION: &str = "cli.agent-out.cleanup.plan.v1";
const CLEANUP_APPLY_SCHEMA_VERSION: &str = "cli.agent-out.cleanup.apply.v1";
const PROJECT_COMMAND: &str = "agent-out project";
const PATH_FOR_COMMAND: &str = "agent-out path-for";
const AUDIT_COMMAND: &str = "agent-out audit";
const CLEANUP_PLAN_COMMAND: &str = "agent-out cleanup plan";
const CLEANUP_APPLY_COMMAND: &str = "agent-out cleanup apply";

const CANONICAL_PROJECT_ROOT: &str = "projects";
const RELEASE_CACHE_ROOT: &str = "nils-versions";
const SKILL_USAGE_MARKER: &str = "skill-usage.record.json";
const TEST_FIRST_MARKER: &str = "test-first-evidence.json";
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
        Command::PathFor(args) => run_path_for(args),
        Command::Project(args) => run_project(args),
        Command::Audit(args) => run_audit(args),
        Command::Cleanup(args) => match args.command {
            cli::CleanupCommand::Plan(args) => run_cleanup_plan(args),
            cli::CleanupCommand::Apply(args) => run_cleanup_apply(args),
        },
        Command::Completion(args) => completion::run(args.shell),
    }
}

fn run_path_for(args: PathForArgs) -> i32 {
    match build_path_for_result(&args) {
        Ok(result) => render_path_for_success(args.format, &result),
        Err(err) => render_error(PATH_FOR_SCHEMA_VERSION, PATH_FOR_COMMAND, args.format, err),
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

fn run_cleanup_plan(args: CleanupPlanArgs) -> i32 {
    let plan = match build_cleanup_plan(&args) {
        Ok(plan) => plan,
        Err(err) => {
            return render_error(
                CLEANUP_PLAN_SCHEMA_VERSION,
                CLEANUP_PLAN_COMMAND,
                args.format,
                err,
            );
        }
    };

    match args.format {
        CleanupFormat::Text => {
            println!("{}", render_cleanup_plan_text(&plan));
            EXIT_OK
        }
        CleanupFormat::Json => {
            print_json_success(CLEANUP_PLAN_SCHEMA_VERSION, CLEANUP_PLAN_COMMAND, &plan)
                .unwrap_or_else(render_json_failure)
        }
    }
}

fn run_cleanup_apply(args: CleanupApplyArgs) -> i32 {
    let report = match apply_cleanup_plan(&args) {
        Ok(report) => report,
        Err(err) => {
            return render_error(
                CLEANUP_APPLY_SCHEMA_VERSION,
                CLEANUP_APPLY_COMMAND,
                args.format,
                err,
            );
        }
    };

    match args.format {
        CleanupFormat::Text => {
            println!("{}", render_cleanup_apply_text(&report));
            EXIT_OK
        }
        CleanupFormat::Json => {
            print_json_success(CLEANUP_APPLY_SCHEMA_VERSION, CLEANUP_APPLY_COMMAND, &report)
                .unwrap_or_else(render_json_failure)
        }
    }
}

fn render_path_for_success(format: ProjectFormat, result: &PathForResult) -> i32 {
    match format {
        ProjectFormat::Path => {
            println!("{}", result.path);
            EXIT_OK
        }
        ProjectFormat::Json => {
            print_json_success(PATH_FOR_SCHEMA_VERSION, PATH_FOR_COMMAND, result)
                .unwrap_or_else(render_json_failure)
        }
        ProjectFormat::Env => {
            println!("{}", render_path_for_env(result));
            EXIT_OK
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

impl JsonFormat for CleanupFormat {
    fn is_json(&self) -> bool {
        *self == CleanupFormat::Json
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

    fn data(code: &'static str, message: impl Into<String>, details: Option<Value>) -> Self {
        Self {
            code,
            message: message.into(),
            details,
            exit_code: EXIT_DATA,
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

#[derive(Debug, Serialize)]
pub struct PathForResult {
    pub path: String,
    pub agent_home: String,
    pub out_root: String,
    pub repo: String,
    pub project_slug: String,
    pub domain: String,
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

fn build_path_for_result(args: &PathForArgs) -> Result<PathForResult, CliError> {
    let agent_home = resolve_agent_home(args.agent_home.as_deref())?;
    let (repo, explicit_repo_slug) =
        resolve_path_for_repo(args.repo.as_deref(), args.repo_slug.as_deref())?;
    let domain = sanitize_topic(&args.domain);
    let topic = path_for_topic(&domain, args.topic.as_deref());
    let project = build_project_path(ProjectPathInput {
        agent_home,
        repo,
        explicit_repo_slug,
        topic,
        timestamp: current_timestamp(),
        mkdir: args.mkdir,
    })?;

    Ok(PathForResult {
        path: project.path,
        agent_home: project.agent_home,
        out_root: project.out_root,
        repo: project.repo,
        project_slug: project.project_slug,
        domain,
        topic: project.topic,
        run_id: project.run_id,
        created: project.created,
    })
}

fn resolve_path_for_repo(
    repo_arg: Option<&str>,
    repo_slug_arg: Option<&str>,
) -> Result<(PathBuf, Option<String>), CliError> {
    if let Some(slug) = repo_slug_arg {
        let repo = resolve_repo(repo_arg.map(Path::new))?;
        return Ok((repo, Some(slug.to_string())));
    }

    if let Some(repo) = repo_arg {
        let path = Path::new(repo);
        if path.exists() {
            return Ok((absolute_path(path)?, None));
        }

        if project_slug_from_owner_repo(repo).is_some() {
            return Ok((resolve_repo(None)?, Some(repo.to_string())));
        }

        return Ok((absolute_path(path)?, None));
    }

    Ok((resolve_repo(None)?, None))
}

fn path_for_topic(domain: &str, topic: Option<&str>) -> String {
    match topic {
        Some(topic) if domain == CANONICAL_PROJECT_ROOT => sanitize_topic(topic),
        Some(topic) => sanitize_topic(&format!("{domain}-{topic}")),
        None => domain.to_string(),
    }
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

fn local_project_slug(repo: &Path) -> String {
    let basename = repo
        .file_name()
        .and_then(|value| value.to_str())
        .map(|value| sanitize_path_label(value, "repo"))
        .unwrap_or_else(|| "repo".to_string());
    let hash = stable_short_hash(&display_path(repo));
    let prefix = nils_common::slug::LOCAL_FALLBACK_SLUG_PREFIX;
    format!("{prefix}{basename}-{hash}")
}

pub fn sanitize_topic(topic: &str) -> String {
    sanitize_path_label(topic, "untitled")
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

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CleanupCategory {
    AllowedRoot,
    Cache,
    TopLevelNoncanonical,
    EvidenceSource,
    ProjectArtifact,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CleanupAction {
    Delete,
    Preserve,
    NeedsPolicy,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CleanupItem {
    pub name: String,
    pub path: String,
    pub kind: String,
    pub category: CleanupCategory,
    pub action: CleanupAction,
    pub reason: String,
    pub size_bytes: u64,
    pub mtime_unix: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_digest: Option<String>,
    pub contains_skill_usage: bool,
    pub contains_test_first_evidence: bool,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct CleanupPlan {
    pub agent_home: String,
    pub out_root: String,
    pub out_root_exists: bool,
    pub include_projects: bool,
    pub items: Vec<CleanupItem>,
    pub summary: CleanupSummary,
    pub plan_digest: String,
}

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct CleanupSummary {
    pub total: usize,
    pub delete: usize,
    pub preserve: usize,
    pub needs_policy: usize,
    pub delete_bytes: u64,
    pub preserve_bytes: u64,
    pub needs_policy_bytes: u64,
}

#[derive(Debug, Serialize)]
pub struct CleanupApplyReport {
    pub agent_home: String,
    pub out_root: String,
    pub plan_digest: String,
    pub applied: bool,
    pub entries: Vec<CleanupApplyEntry>,
    pub summary: CleanupApplySummary,
}

#[derive(Debug, Serialize)]
pub struct CleanupApplyEntry {
    pub path: String,
    pub action: String,
    pub status: String,
    pub reason: String,
}

#[derive(Debug, Default, Serialize)]
pub struct CleanupApplySummary {
    pub deleted: usize,
    pub skipped: usize,
    pub delete_bytes: u64,
}

struct CleanupValidatedDelete {
    path: PathBuf,
    display_path: String,
    reason: String,
    size_bytes: u64,
    metadata: fs::Metadata,
}

enum CleanupApplyDecision {
    Delete(Box<CleanupValidatedDelete>),
    Skip { path: String, reason: String },
}

#[derive(Deserialize)]
struct CleanupPlanEnvelope {
    schema_version: String,
    command: String,
    ok: bool,
    result: CleanupPlan,
}

#[derive(Serialize)]
struct CleanupPlanDigestInput<'a> {
    agent_home: &'a str,
    out_root: &'a str,
    out_root_exists: bool,
    include_projects: bool,
    items: &'a [CleanupItem],
    summary: &'a CleanupSummary,
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

fn build_cleanup_plan(args: &CleanupPlanArgs) -> Result<CleanupPlan, CliError> {
    let agent_home = resolve_agent_home(args.agent_home.as_deref())?;
    let out_root = agent_home.join("out");
    let mut items = Vec::new();

    reject_cleanup_out_root_symlink_if_exists(&out_root)?;
    if !out_root.exists() {
        let mut plan = CleanupPlan {
            agent_home: display_path(&agent_home),
            out_root: display_path(&out_root),
            out_root_exists: false,
            include_projects: args.include_projects,
            items,
            summary: CleanupSummary::default(),
            plan_digest: String::new(),
        };
        plan.plan_digest = compute_cleanup_plan_digest(&plan)?;
        return Ok(plan);
    }

    if !out_root.is_dir() {
        return Err(CliError::runtime(
            "out-root-not-directory",
            format!("{} is not a directory", out_root.display()),
            Some(json!({ "out_root": display_path(&out_root) })),
        ));
    }

    let allowlisted: BTreeSet<&str> = ALLOWLISTED_TOOL_ROOTS.iter().copied().collect();
    let mut entries = read_sorted_children(&out_root, "cleanup-read-failed")?;

    for path in entries.drain(..) {
        let name = path_name(&path);
        let kind = entry_kind(&path);

        if name == CANONICAL_PROJECT_ROOT && path.is_dir() {
            items.push(CleanupItem {
                name,
                path: display_path(&path),
                kind,
                category: CleanupCategory::AllowedRoot,
                action: CleanupAction::Preserve,
                reason: "canonical project-scoped artifact root".to_string(),
                size_bytes: path_shallow_size_bytes(&path)?,
                mtime_unix: path_mtime_unix(&path),
                content_digest: None,
                contains_skill_usage: false,
                contains_test_first_evidence: false,
            });
            if args.include_projects {
                items.extend(build_project_cleanup_items(&path)?);
            }
        } else if allowlisted.contains(name.as_str()) && path.is_dir() {
            items.push(CleanupItem {
                name,
                path: display_path(&path),
                kind,
                category: CleanupCategory::AllowedRoot,
                action: CleanupAction::Preserve,
                reason: "preserved tool/workflow artifact root".to_string(),
                size_bytes: path_shallow_size_bytes(&path)?,
                mtime_unix: path_mtime_unix(&path),
                content_digest: None,
                contains_skill_usage: false,
                contains_test_first_evidence: false,
            });
        } else {
            let markers = marker_flags(&path)?;
            let size_bytes = path_size_bytes(&path)?;
            let mtime_unix = path_mtime_unix(&path);

            if markers.has_evidence() {
                items.push(CleanupItem {
                    name,
                    path: display_path(&path),
                    kind,
                    category: CleanupCategory::EvidenceSource,
                    action: CleanupAction::Preserve,
                    reason: "contains retained evidence markers; use evidence migrate/prune-source"
                        .to_string(),
                    size_bytes,
                    mtime_unix,
                    content_digest: None,
                    contains_skill_usage: markers.skill_usage,
                    contains_test_first_evidence: markers.test_first_evidence,
                });
            } else if name == RELEASE_CACHE_ROOT {
                items.push(CleanupItem {
                    name,
                    path: display_path(&path),
                    kind,
                    category: CleanupCategory::Cache,
                    action: CleanupAction::Delete,
                    reason: "released nils-cli binary cache; safe to recreate from release assets"
                        .to_string(),
                    size_bytes,
                    mtime_unix,
                    content_digest: Some(path_content_digest(&path)?),
                    contains_skill_usage: markers.skill_usage,
                    contains_test_first_evidence: markers.test_first_evidence,
                });
            } else {
                items.push(CleanupItem {
                    name,
                    path: display_path(&path),
                    kind,
                    category: CleanupCategory::TopLevelNoncanonical,
                    action: CleanupAction::NeedsPolicy,
                    reason: "top-level noncanonical entry requires review before deletion"
                        .to_string(),
                    size_bytes,
                    mtime_unix,
                    content_digest: None,
                    contains_skill_usage: false,
                    contains_test_first_evidence: false,
                });
            }
        }
    }

    let summary = cleanup_summary(&items);
    let mut plan = CleanupPlan {
        agent_home: display_path(&agent_home),
        out_root: display_path(&out_root),
        out_root_exists: true,
        include_projects: args.include_projects,
        items,
        summary,
        plan_digest: String::new(),
    };
    plan.plan_digest = compute_cleanup_plan_digest(&plan)?;
    Ok(plan)
}

fn build_project_cleanup_items(projects_root: &Path) -> Result<Vec<CleanupItem>, CliError> {
    let mut items = Vec::new();
    if !projects_root.is_dir() {
        return Ok(items);
    }

    for project_dir in read_sorted_children(projects_root, "cleanup-read-failed")? {
        if !project_dir.is_dir() {
            continue;
        }
        for run_dir in read_sorted_children(&project_dir, "cleanup-read-failed")? {
            let markers = marker_flags(&run_dir)?;
            let action = if markers.has_evidence() {
                CleanupAction::Preserve
            } else {
                CleanupAction::NeedsPolicy
            };
            let (category, reason) = if markers.has_evidence() {
                (
                    CleanupCategory::EvidenceSource,
                    "contains retained evidence markers; use evidence migrate/prune-source"
                        .to_string(),
                )
            } else {
                (
                    CleanupCategory::ProjectArtifact,
                    "canonical project artifact without a retention policy; review before deleting"
                        .to_string(),
                )
            };

            items.push(CleanupItem {
                name: path_name(&run_dir),
                path: display_path(&run_dir),
                kind: entry_kind(&run_dir),
                category,
                action,
                reason,
                size_bytes: path_size_bytes(&run_dir)?,
                mtime_unix: path_mtime_unix(&run_dir),
                content_digest: None,
                contains_skill_usage: markers.skill_usage,
                contains_test_first_evidence: markers.test_first_evidence,
            });
        }
    }
    Ok(items)
}

fn cleanup_summary(items: &[CleanupItem]) -> CleanupSummary {
    let mut summary = CleanupSummary {
        total: items.len(),
        ..CleanupSummary::default()
    };
    for item in items {
        match item.action {
            CleanupAction::Delete => {
                summary.delete += 1;
                summary.delete_bytes = summary.delete_bytes.saturating_add(item.size_bytes);
            }
            CleanupAction::Preserve => {
                summary.preserve += 1;
                summary.preserve_bytes = summary.preserve_bytes.saturating_add(item.size_bytes);
            }
            CleanupAction::NeedsPolicy => {
                summary.needs_policy += 1;
                summary.needs_policy_bytes =
                    summary.needs_policy_bytes.saturating_add(item.size_bytes);
            }
        }
    }
    summary
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

fn path_name(path: &Path) -> String {
    path.file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_string()
}

fn read_sorted_children(path: &Path, code: &'static str) -> Result<Vec<PathBuf>, CliError> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(path).map_err(|err| {
        CliError::runtime(
            code,
            format!("failed to read {}: {err}", path.display()),
            Some(json!({ "path": display_path(path) })),
        )
    })? {
        let entry = entry.map_err(|err| {
            CliError::runtime(
                code,
                format!("failed to read entry under {}: {err}", path.display()),
                Some(json!({ "path": display_path(path) })),
            )
        })?;
        entries.push(entry.path());
    }
    entries.sort();
    Ok(entries)
}

#[derive(Default)]
struct MarkerFlags {
    skill_usage: bool,
    test_first_evidence: bool,
}

impl MarkerFlags {
    fn has_evidence(&self) -> bool {
        self.skill_usage || self.test_first_evidence
    }

    fn merge(&mut self, other: MarkerFlags) {
        self.skill_usage |= other.skill_usage;
        self.test_first_evidence |= other.test_first_evidence;
    }
}

fn marker_flags(path: &Path) -> Result<MarkerFlags, CliError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(MarkerFlags::default()),
        Err(err) => {
            return Err(CliError::runtime(
                "cleanup-stat-failed",
                format!("failed to inspect {}: {err}", path.display()),
                Some(json!({ "path": display_path(path) })),
            ));
        }
    };

    let mut flags = MarkerFlags::default();
    match path.file_name().and_then(|value| value.to_str()) {
        Some(SKILL_USAGE_MARKER) => flags.skill_usage = true,
        Some(TEST_FIRST_MARKER) => flags.test_first_evidence = true,
        _ => {}
    }

    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        for child in read_sorted_children(path, "cleanup-read-failed")? {
            flags.merge(marker_flags(&child)?);
            if flags.has_evidence() && flags.skill_usage && flags.test_first_evidence {
                break;
            }
        }
    }

    Ok(flags)
}

fn path_size_bytes(path: &Path) -> Result<u64, CliError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(err) => {
            return Err(CliError::runtime(
                "cleanup-stat-failed",
                format!("failed to inspect {}: {err}", path.display()),
                Some(json!({ "path": display_path(path) })),
            ));
        }
    };

    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        let mut total = 0_u64;
        for child in read_sorted_children(path, "cleanup-read-failed")? {
            total = total.saturating_add(path_size_bytes(&child)?);
        }
        Ok(total)
    } else {
        Ok(metadata.len())
    }
}

fn path_shallow_size_bytes(path: &Path) -> Result<u64, CliError> {
    let metadata = fs::symlink_metadata(path).map_err(|err| {
        CliError::runtime(
            "cleanup-stat-failed",
            format!("failed to inspect {}: {err}", path.display()),
            Some(json!({ "path": display_path(path) })),
        )
    })?;
    Ok(metadata.len())
}

fn path_content_digest(path: &Path) -> Result<String, CliError> {
    let mut hasher = Sha256::new();
    update_content_digest_record(
        &mut hasher,
        b"domain",
        b"agent-out.cleanup.content_digest.v2",
    );
    update_path_content_digest(&mut hasher, path, path)?;
    let digest = hasher.finalize();
    let hex = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Ok(format!("sha256:{hex}"))
}

fn update_content_digest_record(hasher: &mut Sha256, tag: &[u8], payload: &[u8]) {
    hasher.update((tag.len() as u64).to_le_bytes());
    hasher.update(tag);
    hasher.update((payload.len() as u64).to_le_bytes());
    hasher.update(payload);
}

fn update_content_digest_u64(hasher: &mut Sha256, tag: &[u8], value: u64) {
    update_content_digest_record(hasher, tag, &value.to_le_bytes());
}

fn update_path_content_digest(
    hasher: &mut Sha256,
    root: &Path,
    path: &Path,
) -> Result<(), CliError> {
    let metadata = fs::symlink_metadata(path).map_err(|err| {
        CliError::runtime(
            "cleanup-stat-failed",
            format!("failed to inspect {}: {err}", path.display()),
            Some(json!({ "path": display_path(path) })),
        )
    })?;
    let relative = path.strip_prefix(root).unwrap_or(path);
    update_content_digest_record(hasher, b"path", &path_digest_bytes(relative));

    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        update_content_digest_record(hasher, b"type", b"dir");
        let children = read_sorted_children(path, "cleanup-read-failed")?;
        update_content_digest_u64(hasher, b"children", children.len() as u64);
        for child in children {
            update_path_content_digest(hasher, root, &child)?;
        }
    } else if metadata.file_type().is_symlink() {
        update_content_digest_record(hasher, b"type", b"symlink");
        let target = fs::read_link(path).map_err(|err| {
            CliError::runtime(
                "cleanup-readlink-failed",
                format!("failed to read symlink {}: {err}", path.display()),
                Some(json!({ "path": display_path(path) })),
            )
        })?;
        update_content_digest_record(hasher, b"target", &path_digest_bytes(&target));
    } else {
        update_content_digest_record(hasher, b"type", b"file");
        update_content_digest_u64(hasher, b"content_len", metadata.len());
        let mut file = fs::File::open(path).map_err(|err| {
            CliError::runtime(
                "cleanup-read-failed",
                format!("failed to read {}: {err}", path.display()),
                Some(json!({ "path": display_path(path) })),
            )
        })?;
        let mut buffer = [0_u8; 8192];
        loop {
            let read = file.read(&mut buffer).map_err(|err| {
                CliError::runtime(
                    "cleanup-read-failed",
                    format!("failed to read {}: {err}", path.display()),
                    Some(json!({ "path": display_path(path) })),
                )
            })?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
    }

    Ok(())
}

fn path_digest_bytes(path: &Path) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes().to_vec()
    }

    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        path.as_os_str()
            .encode_wide()
            .flat_map(u16::to_le_bytes)
            .collect()
    }

    #[cfg(not(any(unix, windows)))]
    {
        display_path(path).into_bytes()
    }
}

fn path_mtime_unix(path: &Path) -> Option<i64> {
    let metadata = fs::symlink_metadata(path).ok()?;
    let modified = metadata.modified().ok()?;
    match modified.duration_since(UNIX_EPOCH) {
        Ok(duration) => Some(duration.as_secs() as i64),
        Err(err) => Some(-(err.duration().as_secs() as i64)),
    }
}

fn compute_cleanup_plan_digest(plan: &CleanupPlan) -> Result<String, CliError> {
    let digest_input = CleanupPlanDigestInput {
        agent_home: &plan.agent_home,
        out_root: &plan.out_root,
        out_root_exists: plan.out_root_exists,
        include_projects: plan.include_projects,
        items: &plan.items,
        summary: &plan.summary,
    };
    let bytes = serde_json::to_vec(&digest_input).map_err(|err| {
        CliError::runtime(
            "cleanup-digest-failed",
            format!("failed to compute cleanup plan digest: {err}"),
            None,
        )
    })?;
    let digest = Sha256::digest(bytes);
    let hex = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Ok(format!("sha256:{hex}"))
}

fn apply_cleanup_plan(args: &CleanupApplyArgs) -> Result<CleanupApplyReport, CliError> {
    let plan_text = fs::read_to_string(&args.plan_file).map_err(|err| {
        CliError::data(
            "cleanup-plan-read-failed",
            format!("failed to read {}: {err}", args.plan_file.display()),
            Some(json!({ "plan_file": display_path(&args.plan_file) })),
        )
    })?;
    let envelope: CleanupPlanEnvelope = serde_json::from_str(&plan_text).map_err(|err| {
        CliError::data(
            "cleanup-plan-invalid-json",
            format!("failed to parse cleanup plan json: {err}"),
            Some(json!({ "plan_file": display_path(&args.plan_file) })),
        )
    })?;
    if envelope.schema_version != CLEANUP_PLAN_SCHEMA_VERSION
        || envelope.command != CLEANUP_PLAN_COMMAND
        || !envelope.ok
    {
        return Err(CliError::data(
            "cleanup-plan-invalid",
            "plan file is not a successful agent-out cleanup plan envelope",
            Some(json!({
                "schema_version": envelope.schema_version,
                "command": envelope.command,
                "ok": envelope.ok
            })),
        ));
    }

    let plan = envelope.result;
    let computed_digest = compute_cleanup_plan_digest(&plan)?;
    if computed_digest != plan.plan_digest {
        return Err(CliError::data(
            "cleanup-plan-digest-invalid",
            "plan digest does not match plan contents",
            Some(json!({
                "plan_digest": plan.plan_digest,
                "computed_digest": computed_digest
            })),
        ));
    }
    if args.confirm_digest != plan.plan_digest {
        return Err(CliError::data(
            "cleanup-digest-mismatch",
            "confirmed digest does not match the cleanup plan",
            Some(json!({
                "confirm_digest": args.confirm_digest,
                "plan_digest": plan.plan_digest
            })),
        ));
    }

    let agent_home = resolve_agent_home(args.agent_home.as_deref())?;
    let expected_agent_home = display_path(&agent_home);
    if expected_agent_home != plan.agent_home {
        return Err(CliError::data(
            "cleanup-agent-home-mismatch",
            "plan agent_home does not match the resolved agent home",
            Some(json!({
                "agent_home": expected_agent_home,
                "plan_agent_home": plan.agent_home
            })),
        ));
    }
    let expected_out_root = display_path(&agent_home.join("out"));
    if expected_out_root != plan.out_root {
        return Err(CliError::data(
            "cleanup-out-root-mismatch",
            "plan out_root does not match the resolved agent home",
            Some(json!({
                "out_root": expected_out_root,
                "plan_out_root": plan.out_root
            })),
        ));
    }
    let out_root = PathBuf::from(&plan.out_root);
    validate_cleanup_plan_path(&out_root, &plan.out_root)?;
    reject_cleanup_out_root_symlink_if_exists(&out_root)?;
    let mut decisions = Vec::new();
    let mut delete_paths = BTreeSet::new();

    for item in plan
        .items
        .iter()
        .filter(|item| item.action == CleanupAction::Delete)
    {
        let path = PathBuf::from(&item.path);
        validate_cleanup_plan_path(&path, &item.path)?;
        if !path.starts_with(&out_root) {
            return Err(CliError::data(
                "cleanup-path-outside-out-root",
                "plan contains a delete path outside out_root",
                Some(json!({ "path": item.path, "out_root": plan.out_root })),
            ));
        }
        validate_cleanup_delete_eligibility(item, &path, &out_root)?;

        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                decisions.push(CleanupApplyDecision::Skip {
                    path: item.path.clone(),
                    reason: "path no longer exists".to_string(),
                });
                continue;
            }
            Err(err) => {
                return Err(CliError::runtime(
                    "cleanup-stat-failed",
                    format!("failed to inspect {}: {err}", path.display()),
                    Some(json!({ "path": display_path(&path) })),
                ));
            }
        };
        let delete_identity = validate_cleanup_delete_target(&path, &out_root, &metadata)?;
        if !delete_paths.insert(delete_identity) {
            return Err(CliError::data(
                "cleanup-delete-duplicate",
                "cleanup plan contains duplicate delete paths",
                Some(json!({ "path": item.path })),
            ));
        }

        let markers = marker_flags(&path)?;
        if markers.has_evidence() {
            decisions.push(CleanupApplyDecision::Skip {
                path: item.path.clone(),
                reason: "evidence marker appeared after the plan was created".to_string(),
            });
            continue;
        }

        if !cleanup_item_metadata_matches(item, &path)? {
            decisions.push(CleanupApplyDecision::Skip {
                path: item.path.clone(),
                reason: "path metadata changed after the plan was created".to_string(),
            });
            continue;
        }

        decisions.push(CleanupApplyDecision::Delete(Box::new(
            CleanupValidatedDelete {
                path,
                display_path: item.path.clone(),
                reason: item.reason.clone(),
                size_bytes: item.size_bytes,
                metadata,
            },
        )));
    }

    let mut entries = Vec::new();
    let mut summary = CleanupApplySummary::default();

    for decision in decisions {
        match decision {
            CleanupApplyDecision::Skip { path, reason } => {
                entries.push(CleanupApplyEntry {
                    path,
                    action: "delete".to_string(),
                    status: "skipped".to_string(),
                    reason,
                });
                summary.skipped += 1;
            }
            CleanupApplyDecision::Delete(delete) => {
                if delete.metadata.is_dir() && !delete.metadata.file_type().is_symlink() {
                    fs::remove_dir_all(&delete.path).map_err(|err| {
                        CliError::runtime(
                            "cleanup-delete-failed",
                            format!("failed to delete {}: {err}", delete.path.display()),
                            Some(json!({ "path": display_path(&delete.path) })),
                        )
                    })?;
                } else {
                    fs::remove_file(&delete.path).map_err(|err| {
                        CliError::runtime(
                            "cleanup-delete-failed",
                            format!("failed to delete {}: {err}", delete.path.display()),
                            Some(json!({ "path": display_path(&delete.path) })),
                        )
                    })?;
                }

                entries.push(CleanupApplyEntry {
                    path: delete.display_path,
                    action: "delete".to_string(),
                    status: "deleted".to_string(),
                    reason: delete.reason,
                });
                summary.deleted += 1;
                summary.delete_bytes = summary.delete_bytes.saturating_add(delete.size_bytes);
            }
        }
    }

    Ok(CleanupApplyReport {
        agent_home: plan.agent_home,
        out_root: plan.out_root,
        plan_digest: plan.plan_digest,
        applied: true,
        entries,
        summary,
    })
}

fn validate_cleanup_plan_path(path: &Path, raw: &str) -> Result<(), CliError> {
    if !path.is_absolute() {
        return Err(CliError::data(
            "cleanup-path-outside-out-root",
            "cleanup plan paths must be absolute",
            Some(json!({ "path": raw })),
        ));
    }
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(CliError::data(
            "cleanup-path-outside-out-root",
            "cleanup plan paths must not contain parent-directory components",
            Some(json!({ "path": raw })),
        ));
    }
    Ok(())
}

fn reject_cleanup_out_root_symlink_if_exists(out_root: &Path) -> Result<(), CliError> {
    let Ok(metadata) = fs::symlink_metadata(out_root) else {
        return Ok(());
    };
    if metadata.file_type().is_symlink() {
        return Err(CliError::data(
            "cleanup-out-root-symlink-unsupported",
            "cleanup out_root must be a real directory, not a symlink",
            Some(json!({ "out_root": display_path(out_root) })),
        ));
    }
    Ok(())
}

fn validate_cleanup_delete_target(
    path: &Path,
    out_root: &Path,
    metadata: &fs::Metadata,
) -> Result<PathBuf, CliError> {
    let canonical_out_root = fs::canonicalize(out_root).map_err(|err| {
        CliError::runtime(
            "cleanup-out-root-canonicalize-failed",
            format!("failed to resolve {}: {err}", out_root.display()),
            Some(json!({ "out_root": display_path(out_root) })),
        )
    })?;
    let (containment_path, delete_identity) = if metadata.file_type().is_symlink() {
        let parent = path.parent().ok_or_else(|| {
            CliError::data(
                "cleanup-path-outside-out-root",
                "cleanup plan path has no parent directory",
                Some(json!({ "path": display_path(path) })),
            )
        })?;
        let canonical_parent = fs::canonicalize(parent).map_err(|err| {
            CliError::runtime(
                "cleanup-parent-canonicalize-failed",
                format!("failed to resolve {}: {err}", parent.display()),
                Some(json!({ "path": display_path(path), "parent": display_path(parent) })),
            )
        })?;
        let file_name = path.file_name().ok_or_else(|| {
            CliError::data(
                "cleanup-path-outside-out-root",
                "cleanup plan path has no file name",
                Some(json!({ "path": display_path(path) })),
            )
        })?;
        (canonical_parent.clone(), canonical_parent.join(file_name))
    } else {
        let canonical_path = fs::canonicalize(path).map_err(|err| {
            CliError::runtime(
                "cleanup-target-canonicalize-failed",
                format!("failed to resolve {}: {err}", path.display()),
                Some(json!({ "path": display_path(path) })),
            )
        })?;
        (canonical_path.clone(), canonical_path)
    };

    if !containment_path.starts_with(&canonical_out_root) {
        return Err(CliError::data(
            "cleanup-path-outside-out-root",
            "plan contains a delete path that resolves outside out_root",
            Some(json!({
                "path": display_path(path),
                "resolved_path": display_path(&containment_path),
                "out_root": display_path(out_root),
                "resolved_out_root": display_path(&canonical_out_root)
            })),
        ));
    }

    Ok(delete_identity)
}

fn validate_cleanup_delete_eligibility(
    item: &CleanupItem,
    path: &Path,
    out_root: &Path,
) -> Result<(), CliError> {
    let relative = path.strip_prefix(out_root).map_err(|_| {
        CliError::data(
            "cleanup-path-outside-out-root",
            "plan contains a delete path outside out_root",
            Some(json!({ "path": item.path, "out_root": display_path(out_root) })),
        )
    })?;
    if relative.components().count() != 1 {
        return Err(CliError::data(
            "cleanup-delete-shape-invalid",
            "cleanup apply only deletes direct children of out_root",
            Some(json!({
                "path": item.path,
                "out_root": display_path(out_root),
                "category": item.category
            })),
        ));
    }

    let name = relative
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("");
    match item.category {
        CleanupCategory::Cache if name == RELEASE_CACHE_ROOT => {
            if item.content_digest.is_none() {
                return Err(CliError::data(
                    "cleanup-delete-content-digest-required",
                    "cache delete candidates require content_digest; regenerate the cleanup plan",
                    Some(json!({ "path": item.path, "category": item.category })),
                ));
            }
        }
        CleanupCategory::Cache => {
            return Err(CliError::data(
                "cleanup-delete-shape-invalid",
                "cache delete candidates must be the nils-versions top-level root",
                Some(json!({ "path": item.path, "category": item.category })),
            ));
        }
        CleanupCategory::TopLevelNoncanonical => {
            return Err(CliError::data(
                "cleanup-delete-shape-invalid",
                "top-level noncanonical delete candidates require a policy decision before apply",
                Some(json!({ "path": item.path, "category": item.category })),
            ));
        }
        _ => {
            return Err(CliError::data(
                "cleanup-delete-shape-invalid",
                "cleanup apply refuses delete actions outside reviewed delete categories",
                Some(json!({ "path": item.path, "category": item.category })),
            ));
        }
    }

    Ok(())
}

fn cleanup_item_metadata_matches(item: &CleanupItem, path: &Path) -> Result<bool, CliError> {
    if path_size_bytes(path)? != item.size_bytes || path_mtime_unix(path) != item.mtime_unix {
        return Ok(false);
    }

    if let Some(expected_digest) = &item.content_digest {
        return Ok(path_content_digest(path)? == *expected_digest);
    }

    Ok(true)
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

fn render_cleanup_plan_text(plan: &CleanupPlan) -> String {
    let mut lines = vec![
        format!("agent_home: {}", plan.agent_home),
        format!("out_root: {}", plan.out_root),
        format!("out_root_exists: {}", plan.out_root_exists),
        format!("include_projects: {}", plan.include_projects),
        format!("plan_digest: {}", plan.plan_digest),
        "items:".to_string(),
    ];

    if plan.items.is_empty() {
        lines.push("  none".to_string());
    } else {
        for item in &plan.items {
            lines.push(format!(
                "  - {} ({}, category={}, action={}, size={}): {}",
                item.path,
                item.kind,
                cleanup_category_label(item.category),
                cleanup_action_label(item.action),
                item.size_bytes,
                item.reason
            ));
        }
    }

    lines.push(format!(
        "summary: total={} delete={} preserve={} needs_policy={} delete_bytes={} preserve_bytes={} needs_policy_bytes={}",
        plan.summary.total,
        plan.summary.delete,
        plan.summary.preserve,
        plan.summary.needs_policy,
        plan.summary.delete_bytes,
        plan.summary.preserve_bytes,
        plan.summary.needs_policy_bytes
    ));
    lines.join("\n")
}

fn render_cleanup_apply_text(report: &CleanupApplyReport) -> String {
    let mut lines = vec![
        format!("agent_home: {}", report.agent_home),
        format!("out_root: {}", report.out_root),
        format!("plan_digest: {}", report.plan_digest),
        format!("applied: {}", report.applied),
        "entries:".to_string(),
    ];

    if report.entries.is_empty() {
        lines.push("  none".to_string());
    } else {
        for entry in &report.entries {
            lines.push(format!(
                "  - {} ({}, {}): {}",
                entry.path, entry.action, entry.status, entry.reason
            ));
        }
    }

    lines.push(format!(
        "summary: deleted={} skipped={} delete_bytes={}",
        report.summary.deleted, report.summary.skipped, report.summary.delete_bytes
    ));
    lines.join("\n")
}

fn cleanup_action_label(action: CleanupAction) -> &'static str {
    match action {
        CleanupAction::Delete => "delete",
        CleanupAction::Preserve => "preserve",
        CleanupAction::NeedsPolicy => "needs-policy",
    }
}

fn cleanup_category_label(category: CleanupCategory) -> &'static str {
    match category {
        CleanupCategory::AllowedRoot => "allowed-root",
        CleanupCategory::Cache => "cache",
        CleanupCategory::TopLevelNoncanonical => "top-level-noncanonical",
        CleanupCategory::EvidenceSource => "evidence-source",
        CleanupCategory::ProjectArtifact => "project-artifact",
    }
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

fn render_path_for_env(result: &PathForResult) -> String {
    [
        ("AGENT_OUT_PATH", result.path.as_str()),
        ("AGENT_OUT_ROOT", result.out_root.as_str()),
        ("AGENT_OUT_PROJECT_SLUG", result.project_slug.as_str()),
        ("AGENT_OUT_TOPIC", result.topic.as_str()),
        ("AGENT_OUT_RUN_ID", result.run_id.as_str()),
        ("AGENT_OUT_DOMAIN", result.domain.as_str()),
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
        // The recognizer must agree with the producer, so identity matching
        // treats this as a local fallback (no authoritative owner/repo).
        assert!(
            nils_common::slug::is_local_fallback_slug(&slug),
            "is_local_fallback_slug must recognize local_project_slug output: {slug}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn path_digest_bytes_preserves_non_utf8_unix_names() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let first = PathBuf::from(OsString::from_vec(vec![0xff]));
        let second = PathBuf::from(OsString::from_vec(vec![0xfe]));

        assert_eq!(first.to_string_lossy(), second.to_string_lossy());
        assert_ne!(path_digest_bytes(&first), path_digest_bytes(&second));
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

    // -----------------------------------------------------------------
    // Cleanup apply is the only destructive surface in this binary. Every
    // guard below is what keeps a reviewed plan from deleting something the
    // reviewer never saw, so each one is pinned to its exact error code.
    // -----------------------------------------------------------------

    fn cache_item(path: &Path, size_bytes: u64, digest: Option<String>) -> CleanupItem {
        CleanupItem {
            name: path_name(path),
            path: display_path(path),
            kind: entry_kind(path),
            category: CleanupCategory::Cache,
            action: CleanupAction::Delete,
            reason: "release cache is reproducible".to_string(),
            size_bytes,
            mtime_unix: path_mtime_unix(path),
            content_digest: digest,
            contains_skill_usage: false,
            contains_test_first_evidence: false,
        }
    }

    /// Build an `agent_home` with `out/nils-versions/<file>` populated, plus a
    /// digest-consistent plan envelope on disk ready for `cleanup apply`.
    struct ApplyFixture {
        _tmp: tempfile::TempDir,
        agent_home: PathBuf,
        out_root: PathBuf,
        cache_root: PathBuf,
        plan_file: PathBuf,
        plan_digest: String,
    }

    fn apply_fixture(mutate: impl FnOnce(&mut CleanupPlan)) -> ApplyFixture {
        let tmp = tempfile::tempdir().unwrap();
        // `fs::canonicalize` is used by the delete guard, so start from a
        // canonical root or macOS `/var` -> `/private/var` breaks containment.
        let agent_home = fs::canonicalize(tmp.path()).unwrap().join("agent-home");
        let out_root = agent_home.join("out");
        let cache_root = out_root.join(RELEASE_CACHE_ROOT);
        fs::create_dir_all(cache_root.join("1.0.0")).unwrap();
        fs::write(cache_root.join("1.0.0").join("bin"), b"payload").unwrap();

        let item = cache_item(
            &cache_root,
            path_size_bytes(&cache_root).unwrap(),
            Some(path_content_digest(&cache_root).unwrap()),
        );
        let mut plan = CleanupPlan {
            agent_home: display_path(&agent_home),
            out_root: display_path(&out_root),
            out_root_exists: true,
            include_projects: false,
            summary: cleanup_summary(std::slice::from_ref(&item)),
            items: vec![item],
            plan_digest: String::new(),
        };
        plan.plan_digest = compute_cleanup_plan_digest(&plan).expect("digest");
        mutate(&mut plan);
        let plan_digest = plan.plan_digest.clone();

        let plan_file = agent_home.join("plan.json");
        fs::write(
            &plan_file,
            serde_json::to_string(&serde_json::json!({
                "schema_version": CLEANUP_PLAN_SCHEMA_VERSION,
                "command": CLEANUP_PLAN_COMMAND,
                "ok": true,
                "result": plan,
            }))
            .unwrap(),
        )
        .unwrap();

        ApplyFixture {
            _tmp: tmp,
            agent_home,
            out_root,
            cache_root,
            plan_file,
            plan_digest,
        }
    }

    impl ApplyFixture {
        fn args(&self, confirm_digest: &str) -> CleanupApplyArgs {
            CleanupApplyArgs {
                plan_file: self.plan_file.clone(),
                confirm_digest: confirm_digest.to_string(),
                agent_home: Some(self.agent_home.clone()),
                format: CleanupFormat::Json,
            }
        }
    }

    #[test]
    fn cleanup_apply_deletes_a_digest_confirmed_release_cache() {
        let fixture = apply_fixture(|_| {});

        let report = apply_cleanup_plan(&fixture.args(&fixture.plan_digest)).expect("apply");

        assert!(report.applied);
        assert_eq!(report.summary.deleted, 1);
        assert_eq!(report.summary.skipped, 0);
        assert!(report.summary.delete_bytes > 0);
        assert_eq!(report.entries.len(), 1);
        assert_eq!(report.entries[0].status, "deleted");
        assert!(
            !fixture.cache_root.exists(),
            "the reviewed cache root must be gone"
        );
        assert!(
            fixture.out_root.is_dir(),
            "out_root itself is never deleted"
        );
    }

    #[test]
    fn cleanup_apply_rejects_a_plan_file_that_cannot_be_read() {
        let fixture = apply_fixture(|_| {});
        let mut args = fixture.args(&fixture.plan_digest);
        args.plan_file = fixture.agent_home.join("missing-plan.json");

        let err = apply_cleanup_plan(&args).expect_err("missing plan file");

        assert_eq!(err.code, "cleanup-plan-read-failed");
        assert_eq!(err.exit_code, EXIT_DATA);
    }

    #[test]
    fn cleanup_apply_rejects_a_plan_file_that_is_not_json() {
        let fixture = apply_fixture(|_| {});
        fs::write(&fixture.plan_file, "{ not json").unwrap();

        let err =
            apply_cleanup_plan(&fixture.args(&fixture.plan_digest)).expect_err("invalid json");

        assert_eq!(err.code, "cleanup-plan-invalid-json");
    }

    #[test]
    fn cleanup_apply_rejects_an_envelope_from_another_command() {
        let fixture = apply_fixture(|_| {});
        let raw = fs::read_to_string(&fixture.plan_file).unwrap();
        let mut value: Value = serde_json::from_str(&raw).unwrap();
        value["command"] = serde_json::json!("agent-out audit");
        fs::write(&fixture.plan_file, value.to_string()).unwrap();

        let err =
            apply_cleanup_plan(&fixture.args(&fixture.plan_digest)).expect_err("wrong envelope");

        assert_eq!(err.code, "cleanup-plan-invalid");
    }

    #[test]
    fn cleanup_apply_rejects_a_plan_whose_digest_does_not_cover_its_contents() {
        // Tamper after the digest was computed: the recorded digest no longer
        // describes the item list an operator would have reviewed.
        let fixture = apply_fixture(|plan| {
            plan.items[0].reason = "tampered".to_string();
        });

        let err = apply_cleanup_plan(&fixture.args(&fixture.plan_digest))
            .expect_err("digest must cover contents");

        assert_eq!(err.code, "cleanup-plan-digest-invalid");
        assert!(
            fixture.cache_root.exists(),
            "nothing may be deleted once the digest check fails"
        );
    }

    #[test]
    fn cleanup_apply_requires_the_operator_to_confirm_the_exact_digest() {
        let fixture = apply_fixture(|_| {});

        let err = apply_cleanup_plan(&fixture.args("sha256:not-the-plan"))
            .expect_err("confirmation mismatch");

        assert_eq!(err.code, "cleanup-digest-mismatch");
        assert!(fixture.cache_root.exists());
    }

    #[test]
    fn cleanup_apply_rejects_a_plan_written_for_another_agent_home() {
        let fixture = apply_fixture(|_| {});
        let mut args = fixture.args(&fixture.plan_digest);
        let other = fixture.agent_home.parent().unwrap().join("other-home");
        fs::create_dir_all(other.join("out")).unwrap();
        args.agent_home = Some(other);

        let err = apply_cleanup_plan(&args).expect_err("agent home mismatch");

        assert_eq!(err.code, "cleanup-agent-home-mismatch");
        assert!(fixture.cache_root.exists());
    }

    #[test]
    fn cleanup_apply_skips_a_path_that_disappeared_after_the_plan() {
        let fixture = apply_fixture(|_| {});
        fs::remove_dir_all(&fixture.cache_root).unwrap();

        let report = apply_cleanup_plan(&fixture.args(&fixture.plan_digest)).expect("apply");

        assert_eq!(report.summary.deleted, 0);
        assert_eq!(report.summary.skipped, 1);
        assert_eq!(report.entries[0].status, "skipped");
        assert_eq!(report.entries[0].reason, "path no longer exists");
    }

    #[test]
    fn cleanup_apply_skips_a_path_that_grew_evidence_after_the_plan() {
        let fixture = apply_fixture(|_| {});
        fs::write(fixture.cache_root.join(SKILL_USAGE_MARKER), b"{}").unwrap();

        let report = apply_cleanup_plan(&fixture.args(&fixture.plan_digest)).expect("apply");

        assert_eq!(report.summary.deleted, 0);
        assert_eq!(report.summary.skipped, 1);
        assert_eq!(
            report.entries[0].reason,
            "evidence marker appeared after the plan was created"
        );
        assert!(fixture.cache_root.exists());
    }

    #[test]
    fn cleanup_apply_skips_a_path_whose_contents_changed_after_the_plan() {
        let fixture = apply_fixture(|_| {});
        fs::write(fixture.cache_root.join("1.0.0").join("bin"), b"changed!!!").unwrap();

        let report = apply_cleanup_plan(&fixture.args(&fixture.plan_digest)).expect("apply");

        assert_eq!(report.summary.deleted, 0);
        assert_eq!(report.summary.skipped, 1);
        assert_eq!(
            report.entries[0].reason,
            "path metadata changed after the plan was created"
        );
        assert!(fixture.cache_root.exists());
    }

    #[test]
    fn cleanup_plan_digest_is_stable_and_content_sensitive() {
        let fixture = apply_fixture(|_| {});
        let raw = fs::read_to_string(&fixture.plan_file).unwrap();
        let envelope: CleanupPlanEnvelope = serde_json::from_str(&raw).unwrap();
        let mut plan = envelope.result;

        assert_eq!(
            compute_cleanup_plan_digest(&plan).unwrap(),
            fixture.plan_digest
        );
        plan.include_projects = !plan.include_projects;
        assert_ne!(
            compute_cleanup_plan_digest(&plan).unwrap(),
            fixture.plan_digest
        );
    }

    #[test]
    fn cleanup_plan_paths_must_be_absolute_and_free_of_parent_components() {
        validate_cleanup_plan_path(Path::new("/out/cache"), "/out/cache").expect("absolute path");

        let relative = validate_cleanup_plan_path(Path::new("out/cache"), "out/cache")
            .expect_err("relative path");
        assert_eq!(relative.code, "cleanup-path-outside-out-root");

        let traversal = validate_cleanup_plan_path(Path::new("/out/../etc"), "/out/../etc")
            .expect_err("parent component");
        assert_eq!(traversal.code, "cleanup-path-outside-out-root");
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_refuses_a_symlinked_out_root() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real-out");
        fs::create_dir_all(&real).unwrap();
        let link = tmp.path().join("out");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        // A missing out_root is not an error here; only a symlink is.
        reject_cleanup_out_root_symlink_if_exists(&tmp.path().join("absent")).expect("absent ok");
        reject_cleanup_out_root_symlink_if_exists(&real).expect("real directory ok");
        let err = reject_cleanup_out_root_symlink_if_exists(&link).expect_err("symlink out_root");
        assert_eq!(err.code, "cleanup-out-root-symlink-unsupported");
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_delete_target_must_resolve_inside_out_root() {
        let tmp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(tmp.path()).unwrap();
        let out_root = root.join("out");
        let outside = root.join("outside");
        fs::create_dir_all(&out_root).unwrap();
        fs::create_dir_all(&outside).unwrap();

        let inside = out_root.join("cache");
        fs::create_dir_all(&inside).unwrap();
        let identity = validate_cleanup_delete_target(
            &inside,
            &out_root,
            &fs::symlink_metadata(&inside).unwrap(),
        )
        .expect("contained path");
        assert_eq!(identity, inside);

        // A symlink is identified by its own parent, never by its target, so
        // the symlink entry itself is what gets removed.
        let link = out_root.join("link");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        let link_identity =
            validate_cleanup_delete_target(&link, &out_root, &fs::symlink_metadata(&link).unwrap())
                .expect("symlink inside out_root");
        assert_eq!(link_identity, link);

        // A real directory that lives outside out_root must be refused.
        let err = validate_cleanup_delete_target(
            &outside,
            &out_root,
            &fs::symlink_metadata(&outside).unwrap(),
        )
        .expect_err("escapes out_root");
        assert_eq!(err.code, "cleanup-path-outside-out-root");
    }

    #[test]
    fn cleanup_delete_eligibility_is_restricted_to_reviewed_categories() {
        let out_root = Path::new("/home/tester/agent/out");
        let cache = out_root.join(RELEASE_CACHE_ROOT);

        let mut item = cache_item(&cache, 10, Some("sha256:x".to_string()));
        validate_cleanup_delete_eligibility(&item, &cache, out_root).expect("reviewed cache root");

        item.content_digest = None;
        assert_eq!(
            validate_cleanup_delete_eligibility(&item, &cache, out_root)
                .expect_err("cache needs a digest")
                .code,
            "cleanup-delete-content-digest-required"
        );

        // A nested path is never a direct child of out_root.
        let nested = cache.join("1.0.0");
        let mut nested_item = cache_item(&nested, 10, Some("sha256:x".to_string()));
        assert_eq!(
            validate_cleanup_delete_eligibility(&nested_item, &nested, out_root)
                .expect_err("nested delete")
                .code,
            "cleanup-delete-shape-invalid"
        );

        // A cache row that is not the release-cache root is refused outright.
        let other_cache = out_root.join("something-else");
        nested_item = cache_item(&other_cache, 10, Some("sha256:x".to_string()));
        assert_eq!(
            validate_cleanup_delete_eligibility(&nested_item, &other_cache, out_root)
                .expect_err("non-release cache")
                .code,
            "cleanup-delete-shape-invalid"
        );

        for category in [
            CleanupCategory::TopLevelNoncanonical,
            CleanupCategory::AllowedRoot,
            CleanupCategory::EvidenceSource,
            CleanupCategory::ProjectArtifact,
        ] {
            let mut row = cache_item(&other_cache, 10, None);
            row.category = category;
            assert_eq!(
                validate_cleanup_delete_eligibility(&row, &other_cache, out_root)
                    .expect_err("category needs policy")
                    .code,
                "cleanup-delete-shape-invalid",
                "category {category:?} must not be auto-deletable"
            );
        }

        let elsewhere = Path::new("/tmp/elsewhere");
        let stray = cache_item(elsewhere, 10, None);
        assert_eq!(
            validate_cleanup_delete_eligibility(&stray, elsewhere, out_root)
                .expect_err("outside out_root")
                .code,
            "cleanup-path-outside-out-root"
        );
    }

    #[cfg(unix)]
    #[test]
    fn content_digest_distinguishes_files_dirs_and_symlink_targets() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        let file = root.join("file");
        fs::write(&file, b"payload").unwrap();
        let same = root.join("same");
        fs::write(&same, b"payload").unwrap();
        let different = root.join("different");
        fs::write(&different, b"payload!").unwrap();

        // The digest is content-addressed relative to the digested root, so
        // two identical files hash the same and a changed byte does not.
        assert_eq!(
            path_content_digest(&file).unwrap(),
            path_content_digest(&same).unwrap()
        );
        assert_ne!(
            path_content_digest(&file).unwrap(),
            path_content_digest(&different).unwrap()
        );

        let link_a = root.join("link-a");
        let link_b = root.join("link-b");
        std::os::unix::fs::symlink("target-a", &link_a).unwrap();
        std::os::unix::fs::symlink("target-b", &link_b).unwrap();
        assert_ne!(
            path_content_digest(&link_a).unwrap(),
            path_content_digest(&link_b).unwrap(),
            "a symlink digest must cover its target"
        );

        let dir = root.join("dir");
        fs::create_dir_all(dir.join("nested")).unwrap();
        fs::write(dir.join("nested").join("f"), b"x").unwrap();
        let before = path_content_digest(&dir).unwrap();
        fs::write(dir.join("nested").join("g"), b"y").unwrap();
        assert_ne!(
            before,
            path_content_digest(&dir).unwrap(),
            "an added child must change the directory digest"
        );

        let missing = path_content_digest(&root.join("absent")).expect_err("missing path");
        assert_eq!(missing.code, "cleanup-stat-failed");
    }

    #[cfg(unix)]
    #[test]
    fn size_and_kind_helpers_describe_the_tree_without_following_symlinks() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let dir = root.join("dir");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("a"), b"1234").unwrap();
        fs::write(dir.join("b"), b"567").unwrap();
        let link = root.join("link");
        std::os::unix::fs::symlink(&dir, &link).unwrap();

        assert_eq!(path_size_bytes(&dir).unwrap(), 7);
        assert_eq!(path_size_bytes(&root.join("absent")).unwrap(), 0);
        assert!(path_shallow_size_bytes(&dir).is_ok());
        assert_eq!(
            path_shallow_size_bytes(&root.join("absent"))
                .expect_err("missing")
                .code,
            "cleanup-stat-failed"
        );

        assert_eq!(entry_kind(&dir), "directory");
        assert_eq!(entry_kind(&dir.join("a")), "file");
        assert_eq!(entry_kind(&link), "symlink");
        assert_eq!(entry_kind(&root.join("absent")), "unknown");
        assert_eq!(path_name(&dir), "dir");
        assert_eq!(path_name(Path::new("/")), "");

        assert!(path_mtime_unix(&dir).is_some());
        assert_eq!(path_mtime_unix(&root.join("absent")), None);

        let children = read_sorted_children(&dir, "cleanup-read-failed").unwrap();
        assert_eq!(children, vec![dir.join("a"), dir.join("b")]);
        assert_eq!(
            read_sorted_children(&root.join("absent"), "cleanup-read-failed")
                .expect_err("missing dir")
                .code,
            "cleanup-read-failed"
        );
    }

    #[test]
    fn marker_flags_find_evidence_anywhere_below_a_candidate() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("run");
        fs::create_dir_all(root.join("deep").join("deeper")).unwrap();

        assert!(!marker_flags(&root).unwrap().has_evidence());
        assert!(
            !marker_flags(&tmp.path().join("absent"))
                .unwrap()
                .has_evidence(),
            "a missing path carries no evidence"
        );

        fs::write(
            root.join("deep").join("deeper").join(TEST_FIRST_MARKER),
            b"{}",
        )
        .unwrap();
        let flags = marker_flags(&root).unwrap();
        assert!(flags.test_first_evidence);
        assert!(!flags.skill_usage);
        assert!(flags.has_evidence());

        fs::write(root.join(SKILL_USAGE_MARKER), b"{}").unwrap();
        let both = marker_flags(&root).unwrap();
        assert!(both.skill_usage && both.test_first_evidence);
    }

    #[test]
    fn text_renderers_cover_empty_and_populated_reports() {
        let empty_audit = AuditReport {
            agent_home: "/home".to_string(),
            out_root: "/home/out".to_string(),
            out_root_exists: false,
            allowed_roots: Vec::new(),
            violations: Vec::new(),
            summary: AuditSummary {
                allowed_roots: 0,
                violations: 0,
            },
        };
        let rendered = render_audit_text(&empty_audit);
        assert!(rendered.contains("allowlisted:\n  none"), "{rendered}");
        assert!(rendered.contains("violations:\n  none"), "{rendered}");
        assert!(
            rendered.ends_with("summary: allowed_roots=0 violations=0"),
            "{rendered}"
        );

        let entry = AuditEntry {
            name: "projects".to_string(),
            path: "/home/out/projects".to_string(),
            kind: "directory".to_string(),
            classification: "canonical".to_string(),
            reason: "canonical project root".to_string(),
        };
        let populated = AuditReport {
            allowed_roots: vec![entry],
            violations: vec![AuditEntry {
                name: "stray".to_string(),
                path: "/home/out/stray".to_string(),
                kind: "file".to_string(),
                classification: "noncanonical".to_string(),
                reason: "not allowlisted".to_string(),
            }],
            summary: AuditSummary {
                allowed_roots: 1,
                violations: 1,
            },
            ..empty_audit
        };
        let rendered = render_audit_text(&populated);
        assert!(
            rendered.contains("  - projects (directory, canonical): canonical project root"),
            "{rendered}"
        );
        assert!(
            rendered.contains("  - stray (file, noncanonical): not allowlisted"),
            "{rendered}"
        );

        let mut plan = CleanupPlan {
            agent_home: "/home".to_string(),
            out_root: "/home/out".to_string(),
            out_root_exists: true,
            include_projects: false,
            items: Vec::new(),
            summary: CleanupSummary::default(),
            plan_digest: "sha256:abc".to_string(),
        };
        assert!(render_cleanup_plan_text(&plan).contains("items:\n  none"));
        plan.items.push(cache_item(
            Path::new("/home/out/nils-versions"),
            42,
            Some("sha256:x".to_string()),
        ));
        let rendered = render_cleanup_plan_text(&plan);
        assert!(
            rendered.contains("category=cache, action=delete, size=42"),
            "{rendered}"
        );

        let mut report = CleanupApplyReport {
            agent_home: "/home".to_string(),
            out_root: "/home/out".to_string(),
            plan_digest: "sha256:abc".to_string(),
            applied: true,
            entries: Vec::new(),
            summary: CleanupApplySummary::default(),
        };
        assert!(render_cleanup_apply_text(&report).contains("entries:\n  none"));
        report.entries.push(CleanupApplyEntry {
            path: "/home/out/nils-versions".to_string(),
            action: "delete".to_string(),
            status: "deleted".to_string(),
            reason: "release cache is reproducible".to_string(),
        });
        assert!(
            render_cleanup_apply_text(&report)
                .contains("  - /home/out/nils-versions (delete, deleted): release cache"),
        );
    }

    #[test]
    fn cleanup_labels_are_exhaustive_and_kebab_cased() {
        assert_eq!(cleanup_action_label(CleanupAction::Delete), "delete");
        assert_eq!(cleanup_action_label(CleanupAction::Preserve), "preserve");
        assert_eq!(
            cleanup_action_label(CleanupAction::NeedsPolicy),
            "needs-policy"
        );
        assert_eq!(
            cleanup_category_label(CleanupCategory::AllowedRoot),
            "allowed-root"
        );
        assert_eq!(cleanup_category_label(CleanupCategory::Cache), "cache");
        assert_eq!(
            cleanup_category_label(CleanupCategory::TopLevelNoncanonical),
            "top-level-noncanonical"
        );
        assert_eq!(
            cleanup_category_label(CleanupCategory::EvidenceSource),
            "evidence-source"
        );
        assert_eq!(
            cleanup_category_label(CleanupCategory::ProjectArtifact),
            "project-artifact"
        );
    }

    #[test]
    fn env_renderers_quote_every_value_for_shell_eval() {
        let project = ProjectResult {
            path: "/home/out/projects/o__r/run".to_string(),
            agent_home: "/home".to_string(),
            out_root: "/home/out".to_string(),
            repo: "/repo".to_string(),
            project_slug: "o__r".to_string(),
            topic: "it's-tricky".to_string(),
            run_id: "run".to_string(),
            created: false,
        };
        let rendered = render_project_env(&project);
        assert!(rendered.starts_with("AGENT_OUT_PATH='/home/out/projects/o__r/run'"));
        assert!(
            rendered.contains(r"AGENT_OUT_TOPIC='it'\''s-tricky'"),
            "a single quote must be escaped for `eval`: {rendered}"
        );
        assert_eq!(rendered.lines().count(), 5);

        let path_for = PathForResult {
            path: project.path.clone(),
            agent_home: project.agent_home.clone(),
            out_root: project.out_root.clone(),
            repo: project.repo.clone(),
            project_slug: project.project_slug.clone(),
            domain: "review".to_string(),
            topic: project.topic.clone(),
            run_id: project.run_id.clone(),
            created: false,
        };
        let rendered = render_path_for_env(&path_for);
        assert_eq!(rendered.lines().count(), 6);
        assert!(rendered.contains("AGENT_OUT_DOMAIN='review'"));

        assert_eq!(shell_quote("plain"), "'plain'");
        assert_eq!(shell_quote(""), "''");
    }
}
