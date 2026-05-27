//! `plan-archive refresh` — fetch provider payloads through forge-cli
//! and append scrubbed, append-only snapshots to `_index/`.
//!
//! Sprint 4 of Plan 1. Refresh writes and scrubs but never commits:
//! the snapshot constraint requires that any emitted `.scrub.log` is
//! reviewed before the snapshot is committed, so the commit is left
//! to the wrapping skill / user.

use std::fs;
use std::path::{Path, PathBuf};

use nils_common::cli_contract::{Envelope, EnvelopeError, OutputFormat, exit, schema_version_for};
use serde::Serialize;

use crate::scrub;
use crate::validate;
use crate::validate::hosts::{HostsConfig, validate_hosts_yaml};

pub mod forge;
pub mod refparse;

pub use forge::{ForgeFetcher, RealForge};
pub use refparse::{RefKind, RefTarget, parse_ref_url};

const COMMAND: &str = "refresh";
const BINARY: &str = "plan-archive";

/// Args forwarded from `cli::run`.
pub struct DispatchArgs {
    pub r#ref: Option<String>,
    pub repo: Option<String>,
    pub since: Option<String>,
    pub archive: Option<PathBuf>,
    pub hosts: Option<PathBuf>,
    pub format: OutputFormat,
}

/// Per-ref refresh outcome.
#[derive(Debug, Clone, Serialize)]
pub struct SnapshotResult {
    pub r#ref: String,
    pub snapshot_path: String,
    pub scrub_log_path: Option<String>,
    pub redaction_count: usize,
    pub requires_review: bool,
}

/// One ref that failed inside a batch without aborting the batch.
#[derive(Debug, Clone, Serialize)]
pub struct FailedRef {
    pub r#ref: String,
    pub code: String,
    pub message: String,
}

/// Aggregate refresh report.
#[derive(Debug, Clone, Serialize)]
pub struct RefreshReport {
    pub snapshots: Vec<SnapshotResult>,
    pub failed: Vec<FailedRef>,
    pub requires_review: bool,
    pub catalog_path: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum RefreshError {
    #[error("one of `--ref` or `--repo` is required")]
    NoSelector,
    #[error("`--ref` value `{0}` is not a parseable issue/PR/MR URL")]
    UnparseableRef(String),
    #[error("`--repo` value `{0}` must be `host/org/repo`")]
    UnparseableRepo(String),
    #[error("archive clone path not found at `{0}`")]
    ArchiveCloneMissing(PathBuf),
    #[error("`{0}` is not a recognised provider host in the archive `config/hosts.yaml`")]
    UnknownHost(String),
    #[error("failed to load archive `config/hosts.yaml`: {0}")]
    HostsLoadFailed(String),
    #[error("failed to parse archive `config/hosts.yaml`: {0}")]
    HostsParseFailed(String),
    #[error("forge-cli fetch failed for `{0}`: {1}")]
    ForgeFetchFailed(String, String),
    #[error("invalid `--since` value `{0}`; expected YYYY-MM-DD")]
    InvalidSince(String),
    #[error("io error during refresh: {0}")]
    Io(String),
}

impl RefreshError {
    pub fn code(&self) -> &'static str {
        match self {
            RefreshError::NoSelector => "refresh-no-selector",
            RefreshError::UnparseableRef(_) => "refresh-unparseable-ref",
            RefreshError::UnparseableRepo(_) => "refresh-unparseable-repo",
            RefreshError::ArchiveCloneMissing(_) => "refresh-archive-clone-missing",
            RefreshError::UnknownHost(_) => "refresh-unknown-host",
            RefreshError::HostsLoadFailed(_) => "refresh-hosts-load-failed",
            RefreshError::HostsParseFailed(_) => "refresh-hosts-parse-failed",
            RefreshError::ForgeFetchFailed(_, _) => "refresh-forge-fetch-failed",
            RefreshError::InvalidSince(_) => "refresh-invalid-since",
            RefreshError::Io(_) => "refresh-io-error",
        }
    }
}

/// Entry point from `cli::run`. Uses the real forge-cli backend.
pub fn dispatch(args: DispatchArgs) -> i32 {
    let format = args.format;
    let fetcher = RealForge::from_env();
    let clock = SystemClock;
    match run(&args, &fetcher, &clock) {
        Ok(report) => emit(format, report),
        Err(err) => emit_error(format, err.code(), &err.to_string()),
    }
}

/// Abstracted clock so snapshot file names are deterministic in
/// tests.
pub trait Clock {
    /// Basic-format UTC ISO8601 with no colons, e.g.
    /// `20260527T013045Z`.
    fn now_basic_utc(&self) -> String;
}

/// Wall-clock implementation.
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_basic_utc(&self) -> String {
        // Avoid a chrono dependency: derive a UTC basic timestamp
        // from the system clock.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        format_unix_basic_utc(now)
    }
}

/// Core, testable refresh routine. The fetcher and clock are
/// injected so unit tests can stub forge-cli and freeze time.
pub fn run<F: ForgeFetcher, C: Clock>(
    args: &DispatchArgs,
    fetcher: &F,
    clock: &C,
) -> Result<RefreshReport, RefreshError> {
    let archive = resolve_archive(args.archive.as_deref())?;
    let hosts_path = args
        .hosts
        .clone()
        .unwrap_or_else(|| archive.join("config").join("hosts.yaml"));
    let hosts = load_hosts(&hosts_path)?;

    let targets = resolve_targets(args, fetcher, &hosts)?;

    let mut snapshots = Vec::new();
    let mut failed = Vec::new();
    for target in targets {
        match refresh_one(&archive, &hosts, fetcher, clock, &target) {
            Ok(snap) => snapshots.push(snap),
            Err(err) => failed.push(FailedRef {
                r#ref: target.canonical_url(),
                code: err.code().to_string(),
                message: err.to_string(),
            }),
        }
    }

    let catalog_path = crate::catalog::write_catalog(&archive)
        .map_err(|e| RefreshError::Io(e.to_string()))?
        .display()
        .to_string();
    let requires_review = snapshots.iter().any(|s| s.requires_review);
    Ok(RefreshReport {
        snapshots,
        failed,
        requires_review,
        catalog_path: Some(catalog_path),
    })
}

fn resolve_targets<F: ForgeFetcher>(
    args: &DispatchArgs,
    fetcher: &F,
    hosts: &HostsConfig,
) -> Result<Vec<RefTarget>, RefreshError> {
    if let Some(ref_url) = &args.r#ref {
        let target =
            parse_ref_url(ref_url).ok_or_else(|| RefreshError::UnparseableRef(ref_url.clone()))?;
        ensure_known_host(hosts, &target.host)?;
        return Ok(vec![target]);
    }

    if let Some(repo) = &args.repo {
        let (host, org, repo_name) =
            parse_repo_triple(repo).ok_or_else(|| RefreshError::UnparseableRepo(repo.clone()))?;
        ensure_known_host(hosts, &host)?;
        if let Some(since) = &args.since {
            validate_since(since)?;
        }
        let listed = fetcher
            .list_open_refs(&host, &org, &repo_name, args.since.as_deref())
            .map_err(|e| RefreshError::ForgeFetchFailed(repo.clone(), e))?;
        return Ok(listed);
    }

    Err(RefreshError::NoSelector)
}

fn refresh_one<F: ForgeFetcher, C: Clock>(
    archive: &Path,
    _hosts: &HostsConfig,
    fetcher: &F,
    clock: &C,
    target: &RefTarget,
) -> Result<SnapshotResult, RefreshError> {
    let payload = fetcher
        .fetch_payload(target)
        .map_err(|e| RefreshError::ForgeFetchFailed(target.canonical_url(), e))?;

    let scrubbed = scrub::scrub_text(&payload);

    let dir = archive.join(target.index_dir());
    fs::create_dir_all(&dir).map_err(|e| RefreshError::Io(e.to_string()))?;
    let stamp = clock.now_basic_utc();
    let snapshot_path = dir.join(format!("{stamp}.json"));
    fs::write(&snapshot_path, scrubbed.redacted.as_bytes())
        .map_err(|e| RefreshError::Io(e.to_string()))?;

    let scrub_log_path = if scrubbed.matches.is_empty() {
        None
    } else {
        let log_path = dir.join(format!("{stamp}.scrub.log"));
        scrub::write_log_if_any(&log_path, &scrubbed.matches)
            .map_err(|e| RefreshError::Io(e.to_string()))?;
        Some(log_path.display().to_string())
    };

    let redaction_count = scrubbed.matches.len();
    Ok(SnapshotResult {
        r#ref: target.canonical_url(),
        snapshot_path: snapshot_path.display().to_string(),
        scrub_log_path,
        redaction_count,
        requires_review: redaction_count > 0,
    })
}

fn ensure_known_host(hosts: &HostsConfig, host: &str) -> Result<(), RefreshError> {
    if hosts.hosts.contains_key(host) {
        Ok(())
    } else {
        Err(RefreshError::UnknownHost(host.to_string()))
    }
}

fn parse_repo_triple(s: &str) -> Option<(String, String, String)> {
    let mut segments: Vec<&str> = s.split('/').filter(|p| !p.is_empty()).collect();
    if segments.len() < 3 {
        return None;
    }
    let repo = segments.pop()?.to_string();
    let host = segments.remove(0).to_string();
    let org = segments.join("/");
    if host.is_empty() || org.is_empty() {
        return None;
    }
    Some((host, org, repo))
}

fn validate_since(s: &str) -> Result<(), RefreshError> {
    let parts: Vec<&str> = s.split('-').collect();
    let ok = parts.len() == 3
        && parts[0].len() == 4
        && parts[1].len() == 2
        && parts[2].len() == 2
        && parts.iter().all(|p| p.chars().all(|c| c.is_ascii_digit()));
    if ok {
        Ok(())
    } else {
        Err(RefreshError::InvalidSince(s.to_string()))
    }
}

fn resolve_archive(arg: Option<&Path>) -> Result<PathBuf, RefreshError> {
    let candidate = match arg {
        Some(p) => p.to_path_buf(),
        None => default_archive_clone_path()?,
    };
    if !candidate.is_dir() {
        return Err(RefreshError::ArchiveCloneMissing(candidate));
    }
    Ok(candidate)
}

fn default_archive_clone_path() -> Result<PathBuf, RefreshError> {
    let local = validate::local::validate_local_path(&local_config_path())
        .map_err(|e| RefreshError::Io(e.to_string()))?;
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

fn load_hosts(path: &Path) -> Result<HostsConfig, RefreshError> {
    let raw = fs::read_to_string(path).map_err(|e| RefreshError::HostsLoadFailed(e.to_string()))?;
    let v = validate_hosts_yaml(&raw).map_err(|e| RefreshError::HostsParseFailed(e.to_string()))?;
    Ok(v.data.config)
}

fn emit(format: OutputFormat, report: RefreshReport) -> i32 {
    match format {
        OutputFormat::Json => {
            let envelope = Envelope::success(schema_version_for(BINARY, COMMAND, 1), &report);
            match serde_json::to_string(&envelope) {
                Ok(s) => {
                    println!("{s}");
                    exit::SUCCESS
                }
                Err(_) => exit::SOFTWARE,
            }
        }
        OutputFormat::Text => {
            for s in &report.snapshots {
                println!("refreshed {} → {}", s.r#ref, s.snapshot_path);
                if let Some(log) = &s.scrub_log_path {
                    println!(
                        "  ⚠ {} redaction(s); review {} before committing",
                        s.redaction_count, log
                    );
                }
            }
            for f in &report.failed {
                eprintln!("failed {} [{}]: {}", f.r#ref, f.code, f.message);
            }
            if report.requires_review {
                println!(
                    "review required: at least one snapshot emitted a scrub log; commit only after acknowledging it"
                );
            }
            if let Some(path) = &report.catalog_path {
                println!("catalog regenerated: {path}");
            }
            exit::SUCCESS
        }
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

/// Format a Unix epoch seconds value as basic-format UTC ISO8601
/// (`YYYYMMDDThhmmssZ`). Civil-date conversion via the standard
/// days-from-epoch algorithm.
pub fn format_unix_basic_utc(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (hh, mm, ss) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}{m:02}{d:02}T{hh:02}{mm:02}{ss:02}Z")
}

// Howard Hinnant's days->civil algorithm.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_repo_triple_simple() {
        let r = parse_repo_triple("github.com/graysurf/agent-runtime-kit").unwrap();
        assert_eq!(
            r,
            (
                "github.com".into(),
                "graysurf".into(),
                "agent-runtime-kit".into()
            )
        );
    }

    #[test]
    fn parses_repo_triple_nested_group() {
        let r = parse_repo_triple("gitlab.example.com/acme/platform/ingest").unwrap();
        assert_eq!(
            r,
            (
                "gitlab.example.com".into(),
                "acme/platform".into(),
                "ingest".into()
            )
        );
    }

    #[test]
    fn rejects_repo_triple_too_short() {
        assert!(parse_repo_triple("github.com/org").is_none());
    }

    #[test]
    fn validates_since_format() {
        assert!(validate_since("2026-05-27").is_ok());
        assert!(validate_since("2026-5-27").is_err());
        assert!(validate_since("not-a-date").is_err());
    }

    #[test]
    fn unix_basic_utc_known_epoch() {
        // 2026-05-27T01:30:45Z = 1779annotated; verify round numbers
        // via a known timestamp: 2021-01-01T00:00:00Z = 1609459200.
        assert_eq!(format_unix_basic_utc(1_609_459_200), "20210101T000000Z");
        // 1970-01-01T00:00:00Z
        assert_eq!(format_unix_basic_utc(0), "19700101T000000Z");
        // 2000-02-29T12:34:56Z (leap day) = 951827696
        assert_eq!(format_unix_basic_utc(951_827_696), "20000229T123456Z");
    }
}
