//! `plan-archive migrate` — dry-run and apply.
//!
//! Sprint 3 of Plan 1 ships the deterministic migration pipeline.
//! Dry-run is the default; `--apply` shells out to the released
//! `semantic-commit` binary for both the archive commit and the
//! source-repo deletion commit, and pushes the archive only.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command as ProcCommand;

use nils_common::cli_contract::{Envelope, EnvelopeError, OutputFormat, exit, schema_version_for};
use serde::Serialize;

use crate::validate;
use crate::validate::hosts::{HostClass, HostEntry, HostsConfig, validate_hosts_yaml};

pub mod identity;
pub mod path;

pub use identity::{SourceIdentity, derive_source_identity};
pub use path::{archive_target_path, parse_plan_folder};

const COMMAND: &str = "migrate";
const BINARY: &str = "plan-archive";

/// Args forwarded from `cli::run`.
pub struct DispatchArgs {
    pub plan: PathBuf,
    pub source_repo: Option<PathBuf>,
    pub archive: Option<PathBuf>,
    pub hosts: Option<PathBuf>,
    pub issue: Option<String>,
    pub pr: Option<String>,
    pub mr: Option<String>,
    pub apply: bool,
    pub format: OutputFormat,
}

/// Classification snapshot pulled out of `config/hosts.yaml`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ClassificationSnapshot {
    pub host: String,
    pub class: HostClass,
    pub employer: Option<String>,
    pub primary_identity: Option<String>,
    pub retention: Option<String>,
}

impl ClassificationSnapshot {
    fn from(host: &str, entry: &HostEntry) -> Self {
        Self {
            host: host.to_string(),
            class: entry.class,
            employer: entry.employer.clone(),
            primary_identity: entry.primary_identity.clone(),
            retention: entry.retention.clone(),
        }
    }
}

/// Metadata payload written under
/// `plans/<host>/<org>/<repo>/<folder>/metadata.yaml`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct MetadataPayload {
    pub version: u32,
    pub source: MetadataSource,
    pub captured_classification: ClassificationSnapshot,
    pub refs: MetadataRefs,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct MetadataSource {
    pub host: String,
    pub org_or_group_path: String,
    pub repo: String,
    pub branch: String,
    pub archive_commit: String,
    pub original_path: String,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct MetadataRefs {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mr: Option<String>,
}

impl MetadataRefs {
    fn any(&self) -> bool {
        self.issue.is_some() || self.pr.is_some() || self.mr.is_some()
    }
}

/// Result of a `migrate --dry-run` (also reused as the prelude of an
/// apply).
#[derive(Debug, Clone, Serialize)]
pub struct DryRunReport {
    pub plan_folder: String,
    pub source: SourceIdentity,
    pub classification: ClassificationSnapshot,
    pub archive_target: ArchiveTarget,
    pub files_to_copy: Vec<String>,
    pub metadata: MetadataPayload,
}

#[derive(Debug, Clone, Serialize)]
pub struct ArchiveTarget {
    pub absolute_path: String,
    pub relative_path: String,
    pub exists: bool,
}

/// Result of a successful `migrate --apply`.
#[derive(Debug, Clone, Serialize)]
pub struct ApplyReport {
    pub plan_folder: String,
    pub archive_commit: String,
    pub source_deletion_commit: String,
    pub archive_target: String,
    pub files_copied: usize,
    pub scrub_log: Option<String>,
}

/// Errors produced by the migration pipeline.
#[derive(Debug, thiserror::Error)]
pub enum MigrateError {
    #[error("source repo not found at `{0}`")]
    SourceRepoNotFound(PathBuf),
    #[error("plan folder `{0}` does not exist inside the source repo")]
    PlanFolderMissing(String),
    #[error(
        "archive clone path not found at `{0}` (set `--archive` or seed `archive_clone_path` in the local config)"
    )]
    ArchiveCloneMissing(PathBuf),
    #[error("`{0}` is not a recognised provider host in the archive `config/hosts.yaml`")]
    UnknownHost(String),
    #[error("failed to load archive `config/hosts.yaml`: {0}")]
    HostsLoadFailed(String),
    #[error("failed to parse archive `config/hosts.yaml`: {0}")]
    HostsParseFailed(String),
    #[error(
        "at least one of `--issue`, `--pr`, `--mr` must be supplied so `metadata.yaml` carries a provider reference"
    )]
    NoRefsSupplied,
    #[error("failed to read source repo identity: {0}")]
    IdentityFailed(String),
    #[error(
        "archive target `{0}` already exists; remove it or re-run after resolving the conflict"
    )]
    ArchiveTargetExists(String),
    #[error("source repo has uncommitted changes inside `{0}`; commit or stash them first")]
    SourceRepoDirty(String),
    #[error("io error during migration: {0}")]
    Io(String),
    #[error("subprocess `{0}` failed: {1}")]
    Subprocess(String, String),
}

impl MigrateError {
    pub fn code(&self) -> &'static str {
        match self {
            MigrateError::SourceRepoNotFound(_) => "migrate-source-repo-not-found",
            MigrateError::PlanFolderMissing(_) => "migrate-plan-folder-missing",
            MigrateError::ArchiveCloneMissing(_) => "migrate-archive-clone-missing",
            MigrateError::UnknownHost(_) => "migrate-unknown-host",
            MigrateError::HostsLoadFailed(_) => "migrate-hosts-load-failed",
            MigrateError::HostsParseFailed(_) => "migrate-hosts-parse-failed",
            MigrateError::NoRefsSupplied => "migrate-no-refs-supplied",
            MigrateError::IdentityFailed(_) => "migrate-identity-failed",
            MigrateError::ArchiveTargetExists(_) => "migrate-archive-target-exists",
            MigrateError::SourceRepoDirty(_) => "migrate-source-repo-dirty",
            MigrateError::Io(_) => "migrate-io-error",
            MigrateError::Subprocess(_, _) => "migrate-subprocess-failed",
        }
    }
}

/// Entry point called from `cli::run`.
pub fn dispatch(args: DispatchArgs) -> i32 {
    let format = args.format;
    match prepare(&args) {
        Ok(report) => {
            if args.apply {
                match apply(args, report) {
                    Ok(applied) => emit_apply(format, applied),
                    Err(err) => emit_error(format, err.code(), &err.to_string()),
                }
            } else {
                emit_dry_run(format, report)
            }
        }
        Err(err) => emit_error(format, err.code(), &err.to_string()),
    }
}

/// Pure(ish) preparation. Resolves identities, validates host
/// classification, enumerates files, and assembles the metadata
/// payload. Performs no writes.
pub fn prepare(args: &DispatchArgs) -> Result<DryRunReport, MigrateError> {
    let source_repo = resolve_source_repo(args.source_repo.as_deref())?;
    let archive = resolve_archive(args.archive.as_deref())?;
    let plan_path_in_repo = normalise_plan_arg(&args.plan);

    let absolute_plan = source_repo.join(&plan_path_in_repo);
    if !absolute_plan.is_dir() {
        return Err(MigrateError::PlanFolderMissing(
            plan_path_in_repo.to_string_lossy().to_string(),
        ));
    }

    let identity = derive_source_identity(&source_repo)
        .map_err(|e| MigrateError::IdentityFailed(e.to_string()))?;

    let hosts_path = args
        .hosts
        .clone()
        .unwrap_or_else(|| archive.join("config").join("hosts.yaml"));
    let hosts_config = load_hosts(&hosts_path)?;

    let host_entry = hosts_config
        .hosts
        .get(&identity.host)
        .ok_or_else(|| MigrateError::UnknownHost(identity.host.clone()))?;
    let classification = ClassificationSnapshot::from(&identity.host, host_entry);

    let folder_name = absolute_plan
        .file_name()
        .ok_or_else(|| MigrateError::PlanFolderMissing(plan_path_in_repo.display().to_string()))?
        .to_string_lossy()
        .to_string();

    let archive_target_rel = archive_target_path(
        &identity.host,
        &identity.org_or_group_path,
        &identity.repo,
        &folder_name,
    );
    let archive_target_abs = archive.join(&archive_target_rel);
    let target = ArchiveTarget {
        absolute_path: archive_target_abs.display().to_string(),
        relative_path: archive_target_rel.display().to_string(),
        exists: archive_target_abs.exists(),
    };

    let files_to_copy = enumerate_plan_files(&source_repo, &plan_path_in_repo)?;

    let refs = MetadataRefs {
        issue: args.issue.clone(),
        pr: args.pr.clone(),
        mr: args.mr.clone(),
    };
    if !refs.any() {
        return Err(MigrateError::NoRefsSupplied);
    }

    let metadata = MetadataPayload {
        version: 1,
        source: MetadataSource {
            host: identity.host.clone(),
            org_or_group_path: identity.org_or_group_path.clone(),
            repo: identity.repo.clone(),
            branch: identity.branch.clone(),
            archive_commit: identity.commit.clone(),
            original_path: format!(
                "{}/",
                plan_path_in_repo
                    .display()
                    .to_string()
                    .trim_end_matches('/')
            ),
        },
        captured_classification: classification.clone(),
        refs,
    };

    Ok(DryRunReport {
        plan_folder: folder_name,
        source: identity,
        classification,
        archive_target: target,
        files_to_copy: files_to_copy
            .iter()
            .map(|p| p.display().to_string())
            .collect(),
        metadata,
    })
}

fn resolve_source_repo(arg: Option<&Path>) -> Result<PathBuf, MigrateError> {
    let candidate = match arg {
        Some(p) => p.to_path_buf(),
        None => std::env::current_dir().map_err(|e| MigrateError::Io(e.to_string()))?,
    };
    let root = nils_common::git::repo_root_in(&candidate)
        .map_err(|e| MigrateError::Io(e.to_string()))?
        .ok_or_else(|| MigrateError::SourceRepoNotFound(candidate.clone()))?;
    if !root.is_dir() {
        return Err(MigrateError::SourceRepoNotFound(root));
    }
    Ok(root)
}

fn resolve_archive(arg: Option<&Path>) -> Result<PathBuf, MigrateError> {
    let candidate = match arg {
        Some(p) => p.to_path_buf(),
        None => default_archive_clone_path()?,
    };
    if !candidate.is_dir() {
        return Err(MigrateError::ArchiveCloneMissing(candidate));
    }
    Ok(candidate)
}

fn default_archive_clone_path() -> Result<PathBuf, MigrateError> {
    let local = validate::local::validate_local_path(&local_config_path())
        .map_err(|e| MigrateError::Io(e.to_string()))?;
    Ok(local.data.config.archive_clone_path)
}

fn local_config_path() -> PathBuf {
    if let Some(p) = std::env::var_os("PLAN_ARCHIVE_LOCAL_CONFIG") {
        return PathBuf::from(p);
    }
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        return PathBuf::from(xdg)
            .join("agent-plan-archive")
            .join("config.yaml");
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home)
            .join(".config")
            .join("agent-plan-archive")
            .join("config.yaml");
    }
    PathBuf::from("/nonexistent/agent-plan-archive/config.yaml")
}

fn normalise_plan_arg(arg: &Path) -> PathBuf {
    let s = arg.to_string_lossy().trim_end_matches('/').to_string();
    PathBuf::from(s)
}

fn load_hosts(path: &Path) -> Result<HostsConfig, MigrateError> {
    let raw = fs::read_to_string(path).map_err(|e| MigrateError::HostsLoadFailed(e.to_string()))?;
    let v = validate_hosts_yaml(&raw).map_err(|e| MigrateError::HostsParseFailed(e.to_string()))?;
    Ok(v.data.config)
}

fn enumerate_plan_files(
    source_repo: &Path,
    plan_path_in_repo: &Path,
) -> Result<Vec<PathBuf>, MigrateError> {
    let output = nils_common::git::run_output_in(
        source_repo,
        &["ls-files", "-z", &plan_path_in_repo.to_string_lossy()],
    )
    .map_err(|e| MigrateError::Io(e.to_string()))?;
    if !output.status.success() {
        return Err(MigrateError::Subprocess(
            "git ls-files".to_string(),
            String::from_utf8_lossy(&output.stderr).to_string(),
        ));
    }
    let mut files: Vec<PathBuf> = output
        .stdout
        .split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| PathBuf::from(std::str::from_utf8(s).unwrap_or("").to_string()))
        .collect();
    files.sort();
    Ok(files)
}

fn emit_dry_run(format: OutputFormat, report: DryRunReport) -> i32 {
    match format {
        OutputFormat::Json => emit_json(&report),
        OutputFormat::Text => {
            println!("plan-archive migrate (dry-run)");
            println!("  plan folder       : {}", report.plan_folder);
            println!(
                "  source repo       : {}/{}/{} @ {}",
                report.source.host,
                report.source.org_or_group_path,
                report.source.repo,
                report.source.branch
            );
            println!(
                "  archive target    : {}",
                report.archive_target.relative_path
            );
            println!(
                "  archive exists?   : {}",
                if report.archive_target.exists {
                    "yes (would refuse on --apply)"
                } else {
                    "no"
                }
            );
            println!(
                "  classification    : {}{}",
                match report.classification.class {
                    HostClass::Personal => "personal",
                    HostClass::Employer => "employer",
                },
                report
                    .classification
                    .employer
                    .as_deref()
                    .map(|e| format!(" ({e})"))
                    .unwrap_or_default()
            );
            println!("  files to copy     : {}", report.files_to_copy.len());
            for f in &report.files_to_copy {
                println!("    - {f}");
            }
            println!("  refs              :");
            if let Some(i) = &report.metadata.refs.issue {
                println!("    issue : {i}");
            }
            if let Some(p) = &report.metadata.refs.pr {
                println!("    pr    : {p}");
            }
            if let Some(m) = &report.metadata.refs.mr {
                println!("    mr    : {m}");
            }
            println!("  (no files modified; pass --apply to commit)");
            exit::SUCCESS
        }
    }
}

fn emit_apply(format: OutputFormat, report: ApplyReport) -> i32 {
    match format {
        OutputFormat::Json => emit_json(&report),
        OutputFormat::Text => {
            println!("plan-archive migrate (applied)");
            println!("  plan folder            : {}", report.plan_folder);
            println!("  archive target         : {}", report.archive_target);
            println!("  files copied           : {}", report.files_copied);
            println!("  archive commit         : {}", report.archive_commit);
            println!(
                "  source deletion commit : {}",
                report.source_deletion_commit
            );
            exit::SUCCESS
        }
    }
}

fn emit_json<T: Serialize>(data: &T) -> i32 {
    let envelope = Envelope::success(schema_version_for(BINARY, COMMAND, 1), data);
    match serde_json::to_string(&envelope) {
        Ok(s) => {
            let stdout = std::io::stdout();
            let mut handle = stdout.lock();
            if writeln!(handle, "{s}").is_err() {
                return exit::SOFTWARE;
            }
            exit::SUCCESS
        }
        Err(_) => exit::SOFTWARE,
    }
}

fn emit_error(format: OutputFormat, code: &str, message: &str) -> i32 {
    match format {
        OutputFormat::Json => {
            let envelope: Envelope<()> = Envelope::failure(
                schema_version_for(BINARY, COMMAND, 1),
                EnvelopeError::new(code, message),
            );
            if let Ok(s) = serde_json::to_string(&envelope) {
                eprintln!("{s}");
            }
            exit::DATA
        }
        OutputFormat::Text => {
            eprintln!("error [{code}]: {message}");
            exit::DATA
        }
    }
}

// === Apply ===

/// Apply path. Copies files, writes metadata, commits archive,
/// pushes archive, and on success commits the source-repo deletion.
fn apply(args: DispatchArgs, report: DryRunReport) -> Result<ApplyReport, MigrateError> {
    let source_repo = resolve_source_repo(args.source_repo.as_deref())?;
    let archive = resolve_archive(args.archive.as_deref())?;
    let plan_path_in_repo = normalise_plan_arg(&args.plan);

    if report.archive_target.exists {
        return Err(MigrateError::ArchiveTargetExists(
            report.archive_target.relative_path,
        ));
    }

    if has_dirty_plan_folder(&source_repo, &plan_path_in_repo)? {
        return Err(MigrateError::SourceRepoDirty(
            plan_path_in_repo.display().to_string(),
        ));
    }

    let archive_abs_target = PathBuf::from(&report.archive_target.absolute_path);
    fs::create_dir_all(&archive_abs_target).map_err(|e| MigrateError::Io(e.to_string()))?;

    let mut copied = 0usize;
    for rel in &report.files_to_copy {
        let src = source_repo.join(rel);
        let rel_from_plan = pathdiff(rel, &plan_path_in_repo);
        let dest = archive_abs_target.join(rel_from_plan);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).map_err(|e| MigrateError::Io(e.to_string()))?;
        }
        fs::copy(&src, &dest).map_err(|e| MigrateError::Io(e.to_string()))?;
        copied += 1;
    }

    let metadata_yaml = serde_yaml_ng::to_string(&report.metadata)
        .map_err(|e| MigrateError::Io(format!("metadata serialize: {e}")))?;
    let metadata_path = archive_abs_target.join("metadata.yaml");
    fs::write(&metadata_path, metadata_yaml).map_err(|e| MigrateError::Io(e.to_string()))?;

    // Stage and commit in the archive repo.
    let stage_args = ["add", "--", &report.archive_target.relative_path];
    let stage_out = nils_common::git::run_output_in(&archive, &stage_args)
        .map_err(|e| MigrateError::Io(e.to_string()))?;
    if !stage_out.status.success() {
        return Err(MigrateError::Subprocess(
            "git add (archive)".to_string(),
            String::from_utf8_lossy(&stage_out.stderr).to_string(),
        ));
    }

    let archive_msg = archive_commit_message(&report.source.repo, &report.plan_folder);
    run_semantic_commit(&archive, &archive_msg)?;
    let archive_commit = head_sha(&archive)?;
    push_archive(&archive)?;

    // Source-repo deletion phase.
    let rm_args = [
        "rm",
        "-r",
        "--quiet",
        "--",
        &plan_path_in_repo.to_string_lossy(),
    ];
    let rm_out = nils_common::git::run_output_in(&source_repo, &rm_args)
        .map_err(|e| MigrateError::Io(e.to_string()))?;
    if !rm_out.status.success() {
        return Err(MigrateError::Subprocess(
            "git rm (source)".to_string(),
            String::from_utf8_lossy(&rm_out.stderr).to_string(),
        ));
    }
    let source_msg = format!(
        "chore(plans): archive {} → agent-plan-archive",
        report.plan_folder
    );
    run_semantic_commit(&source_repo, &source_msg)?;
    let source_commit = head_sha(&source_repo)?;

    Ok(ApplyReport {
        plan_folder: report.plan_folder,
        archive_commit,
        source_deletion_commit: source_commit,
        archive_target: report.archive_target.relative_path,
        files_copied: copied,
        scrub_log: None,
    })
}

fn pathdiff(file_rel_to_repo: &str, plan_path: &Path) -> PathBuf {
    let prefix = format!("{}/", plan_path.display());
    PathBuf::from(
        file_rel_to_repo
            .strip_prefix(&prefix)
            .unwrap_or(file_rel_to_repo),
    )
}

/// Build the archive commit header. Keeps only `<repo>/<folder>` so the
/// header stays within semantic-commit's 100-char limit even for long
/// date-prefixed plan folder names; the full host/org/repo path is already
/// recorded in the archive path and `metadata.yaml`.
fn archive_commit_message(repo: &str, plan_folder: &str) -> String {
    format!("archive(plan): {repo}/{plan_folder}")
}

fn has_dirty_plan_folder(repo: &Path, plan_path: &Path) -> Result<bool, MigrateError> {
    let out = nils_common::git::run_output_in(
        repo,
        &["status", "--porcelain", "--", &plan_path.to_string_lossy()],
    )
    .map_err(|e| MigrateError::Io(e.to_string()))?;
    if !out.status.success() {
        return Err(MigrateError::Subprocess(
            "git status".to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        ));
    }
    Ok(!out.stdout.is_empty())
}

fn run_semantic_commit(repo: &Path, message: &str) -> Result<(), MigrateError> {
    let mut child = ProcCommand::new("semantic-commit")
        .arg("commit")
        .arg("-m")
        .arg(message)
        .arg("--quiet")
        .arg("--no-summary")
        .arg("--repo")
        .arg(repo)
        .spawn()
        .map_err(|e| MigrateError::Subprocess("semantic-commit".to_string(), e.to_string()))?;
    let status = child
        .wait()
        .map_err(|e| MigrateError::Subprocess("semantic-commit".to_string(), e.to_string()))?;
    if !status.success() {
        return Err(MigrateError::Subprocess(
            "semantic-commit".to_string(),
            format!("exit code {:?}", status.code()),
        ));
    }
    Ok(())
}

fn head_sha(repo: &Path) -> Result<String, MigrateError> {
    let out = nils_common::git::run_output_in(repo, &["rev-parse", "HEAD"])
        .map_err(|e| MigrateError::Io(e.to_string()))?;
    if !out.status.success() {
        return Err(MigrateError::Subprocess(
            "git rev-parse".to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn push_archive(repo: &Path) -> Result<(), MigrateError> {
    let out = nils_common::git::run_output_in(repo, &["push"])
        .map_err(|e| MigrateError::Io(e.to_string()))?;
    if !out.status.success() {
        return Err(MigrateError::Subprocess(
            "git push (archive)".to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::archive_commit_message;

    #[test]
    fn archive_commit_message_format() {
        assert_eq!(
            archive_commit_message("agent-runtime-kit", "2026-05-27-plan-archive-nils-cli"),
            "archive(plan): agent-runtime-kit/2026-05-27-plan-archive-nils-cli"
        );
    }

    #[test]
    fn archive_commit_header_within_semantic_commit_limit() {
        // The longest dated plan folder migrated to date; the header must stay
        // within semantic-commit's 100-char header limit.
        let msg = archive_commit_message(
            "agent-runtime-kit",
            "2026-05-26-plan-issue-lifecycle-ordering-regression",
        );
        assert!(
            msg.len() <= 100,
            "archive commit header is {} chars: {msg}",
            msg.len()
        );
    }
}
