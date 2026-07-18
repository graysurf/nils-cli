//! Shared provider session-resume resolution for the `nils-cli` workspace.
//!
//! Codex and Claude Code both record the original working directory in their
//! local session history, so resuming a known session id only needs a bounded
//! scan of that history to recover the cwd. This crate owns that scan, the
//! `session_meta` / transcript parsing, the bounded-scan budgets, and the
//! missing / ambiguous / truncated outcome handling so that `agent-session`,
//! `codex-cli`, and `claude-cli` do not each maintain an independent copy.
//!
//! Callers keep their own user-facing error text, exit-code mapping, and final
//! provider command composition; this crate returns structured results only.
//!
//! It deliberately lives outside `nils-common`: the bounded scan needs
//! monotonic time (`Instant::now`) to cap its wall-clock cost, which the
//! `nils-common` render-path determinism gate forbids.

use std::collections::BTreeSet;
use std::fs;
use std::io::{self, BufRead, Read};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use jiff::Timestamp;
use serde_json::Value;

use nils_common::fs::home_dir;

/// Maximum directory depth walked when scanning Codex session history.
///
/// Public because `agent-session` reuses it for its own post-launch capture
/// scan, which shares the same bounded-depth guarantee.
pub const CODEX_RESUME_SCAN_MAX_DEPTH: usize = 6;
const CODEX_RESUME_SCAN_MAX_ENTRIES: usize = 5000;
const CODEX_RESUME_SCAN_SLICE_MS: u64 = 250;
const CODEX_SESSION_META_MAX_LINE_BYTES: u64 = 64 * 1024;
const CLAUDE_RESUME_SCAN_MAX_ENTRIES: usize = 5000;
const CLAUDE_RESUME_SCAN_SLICE_MS: u64 = 250;
const CLAUDE_SESSION_SCAN_MAX_LINES: usize = 64;
/// Per-line byte cap when scanning Claude Code transcripts. Public so callers
/// (and their tests) can build fixtures at the boundary.
pub const CLAUDE_SESSION_META_MAX_LINE_BYTES: usize = 1024 * 1024;

/// The provider whose local session history should be scanned.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ResumeProvider {
    /// Codex CLI rollout histories under `$CODEX_HOME/sessions`.
    Codex,
    /// Claude Code project transcripts under `$CLAUDE_CONFIG_DIR/projects`.
    Claude,
}

impl ResumeProvider {
    /// Stable machine-readable label describing how a match was captured.
    /// Mirrors the `capture_method` recorded by `agent-session` provider-resume
    /// imports so the two stay in lockstep. Surfaced to callers through
    /// [`ResolvedResume::capture_method`] rather than called directly.
    pub(crate) fn capture_method(self) -> &'static str {
        match self {
            ResumeProvider::Codex => "codex-session-meta-import",
            ResumeProvider::Claude => "claude-project-transcript-import",
        }
    }
}

/// A resume id resolved to exactly one recorded working directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedResume {
    /// The recorded original working directory for the session.
    pub cwd: PathBuf,
    /// Stable capture-method label (see [`ResumeProvider::capture_method`]).
    pub capture_method: &'static str,
}

/// Structured reason a resume id could not be resolved to a single cwd.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResumeResolveError {
    /// No CLI-origin session history contained the id.
    NotFound,
    /// The id resolved to more than one distinct recorded cwd.
    Ambiguous {
        /// Number of distinct recorded cwds the id matched.
        cwd_count: usize,
    },
    /// The bounded scan hit its entry/time budget before resolving the id.
    Truncated,
}

/// Reason a raw resume id was rejected before any scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResumeIdError {
    /// The id was empty (after trimming surrounding whitespace).
    Empty,
    /// The id contained control characters.
    ControlChar,
}

/// Trim and validate a raw resume id.
///
/// Rejects empty and control-character ids so a caller never launches a
/// provider with a malformed session identifier. Callers map the returned
/// [`ResumeIdError`] to their own user-facing text.
pub fn normalize_resume_id(session_id: &str) -> Result<String, ResumeIdError> {
    let session_id = session_id.trim();
    if session_id.is_empty() {
        return Err(ResumeIdError::Empty);
    }
    if session_id.chars().any(char::is_control) {
        return Err(ResumeIdError::ControlChar);
    }
    Ok(session_id.to_string())
}

/// Resolve `session_id` to the single working directory recorded for it in the
/// provider's default local history root.
///
/// The default root honors `CODEX_HOME` / `CLAUDE_CONFIG_DIR` and falls back to
/// the standard `$HOME` locations. Returns [`ResumeResolveError`] when the id is
/// missing, matches more than one distinct cwd, or the bounded scan was
/// truncated before it could decide. A canonical Codex UUIDv7 match in its
/// timestamp-derived history day is authoritative; non-canonical layouts are
/// scanned only when that day contains no valid match.
pub fn resolve_resume_source(
    provider: ResumeProvider,
    session_id: &str,
) -> Result<ResolvedResume, ResumeResolveError> {
    let root = match provider {
        ResumeProvider::Codex => codex_sessions_root(),
        ResumeProvider::Claude => claude_projects_root(),
    };
    let Some(root) = root else {
        return Err(ResumeResolveError::NotFound);
    };
    resolve_resume_source_in(provider, &root, session_id)
}

/// Resolve `session_id` beneath an explicit provider configuration directory.
///
/// The configuration directory is the daemon-owned provider context (for
/// example `CODEX_HOME` or `CLAUDE_CONFIG_DIR`), not the nested history
/// directory. This keeps profile-aware callers from accidentally falling back
/// to the process-wide provider root while preserving the same bounded scan.
pub fn resolve_resume_source_in_config_dir(
    provider: ResumeProvider,
    config_dir: &Path,
    session_id: &str,
) -> Result<ResolvedResume, ResumeResolveError> {
    let history_root = match provider {
        ResumeProvider::Codex => config_dir.join("sessions"),
        ResumeProvider::Claude => config_dir.join("projects"),
    };
    resolve_resume_source_in(provider, &history_root, session_id)
}

/// Resolve `session_id` against an explicit provider history `root`.
///
/// Same contract as [`resolve_resume_source`] but with the root supplied by the
/// caller, so it can be exercised without touching the process environment.
pub fn resolve_resume_source_in(
    provider: ResumeProvider,
    root: &Path,
    session_id: &str,
) -> Result<ResolvedResume, ResumeResolveError> {
    if provider == ResumeProvider::Codex {
        let mut budget = CodexResumeScanBudget::from_env();
        return resolve_codex_resume_source_in_with_budget(root, session_id, &mut budget);
    }

    let mut matches = BTreeSet::new();
    let mut budget = ClaudeResumeScanBudget::from_env();
    collect_claude_provider_resume_matches(root, session_id, &mut matches, &mut budget);
    if budget.truncated {
        return Err(ResumeResolveError::Truncated);
    }
    resolved_from_matches(matches, provider.capture_method())
}

fn resolve_codex_resume_source_in_with_budget(
    root: &Path,
    session_id: &str,
    budget: &mut CodexResumeScanBudget,
) -> Result<ResolvedResume, ResumeResolveError> {
    let mut matches = BTreeSet::new();
    if let Some(day) = codex_uuid_v7_session_day(root, session_id) {
        collect_codex_canonical_day_matches(&day, session_id, &mut matches, budget);
        if budget.truncated {
            return Err(ResumeResolveError::Truncated);
        }
        if !matches.is_empty() {
            return resolved_from_matches(matches, ResumeProvider::Codex.capture_method());
        }
    }

    collect_codex_provider_resume_matches(root, 0, session_id, &mut matches, budget);
    if budget.truncated {
        return Err(ResumeResolveError::Truncated);
    }
    resolved_from_matches(matches, ResumeProvider::Codex.capture_method())
}

fn codex_uuid_v7_session_day(root: &Path, session_id: &str) -> Option<PathBuf> {
    let compact: String = session_id
        .chars()
        .filter(|character| *character != '-')
        .collect();
    if compact.len() != 32
        || !compact.bytes().all(|byte| byte.is_ascii_hexdigit())
        || compact.as_bytes()[12] != b'7'
        || !matches!(
            compact.as_bytes()[16],
            b'8' | b'9' | b'a' | b'b' | b'A' | b'B'
        )
    {
        return None;
    }
    let milliseconds = i64::try_from(u64::from_str_radix(&compact[..12], 16).ok()?).ok()?;
    let utc = Timestamp::from_millisecond(milliseconds)
        .ok()?
        .in_tz("UTC")
        .ok()?;
    Some(
        root.join(format!("{:04}", utc.year()))
            .join(format!("{:02}", utc.month()))
            .join(format!("{:02}", utc.day())),
    )
}

fn collect_codex_canonical_day_matches(
    day: &Path,
    session_id: &str,
    matches: &mut BTreeSet<ProviderHistoryMatch>,
    budget: &mut CodexResumeScanBudget,
) {
    let Ok(entries) = fs::read_dir(day) else {
        return;
    };
    for entry in entries.flatten() {
        if !budget.visit_entry() {
            return;
        }
        let file_name = entry.file_name();
        let Some(file_name) = file_name.to_str() else {
            continue;
        };
        let Some(stem) = file_name.strip_suffix(".jsonl") else {
            continue;
        };
        if stem != session_id
            && !stem
                .strip_suffix(session_id)
                .is_some_and(|prefix| prefix.ends_with('-'))
        {
            continue;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_file() {
            continue;
        }
        let path = entry.path();
        if let Some(meta) = read_codex_resumable_session_meta(&path)
            && meta.session_id == session_id
        {
            matches.insert(ProviderHistoryMatch {
                path,
                cwd: meta.cwd,
            });
        }
    }
}

fn resolved_from_matches(
    matches: BTreeSet<ProviderHistoryMatch>,
    capture_method: &'static str,
) -> Result<ResolvedResume, ResumeResolveError> {
    let cwds: BTreeSet<String> = matches.into_iter().map(|candidate| candidate.cwd).collect();
    match cwds.len() {
        0 => Err(ResumeResolveError::NotFound),
        1 => Ok(ResolvedResume {
            cwd: PathBuf::from(cwds.into_iter().next().expect("single match")),
            capture_method,
        }),
        len => Err(ResumeResolveError::Ambiguous { cwd_count: len }),
    }
}

/// One session-history file that matched a resume id, with its recorded cwd.
///
/// Ordered by `(path, cwd)` so callers can dedupe deterministically.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ProviderHistoryMatch {
    /// The matched session-history file.
    pub path: PathBuf,
    /// The recorded working directory for the session.
    pub cwd: String,
}

/// The default Codex session-history root: `$CODEX_HOME/sessions`, falling back
/// to `$HOME/.codex/sessions`.
pub fn codex_sessions_root() -> Option<PathBuf> {
    if let Some(codex_home) = non_empty_env("CODEX_HOME") {
        return Some(PathBuf::from(codex_home).join("sessions"));
    }
    home_dir().map(|home| home.join(".codex/sessions"))
}

/// The default Claude Code project-transcript root:
/// `$CLAUDE_CONFIG_DIR/projects`, falling back to `$HOME/.claude/projects`.
pub fn claude_projects_root() -> Option<PathBuf> {
    if let Some(claude_config_dir) = non_empty_env("CLAUDE_CONFIG_DIR") {
        return Some(PathBuf::from(claude_config_dir).join("projects"));
    }
    home_dir().map(|home| home.join(".claude/projects"))
}

/// Bounded-scan budget for Codex session-history walks.
///
/// Fields are public so callers can construct a fixed budget in tests; the
/// [`Self::from_env`] constructor reads the canonical tuning env vars.
#[derive(Debug)]
pub struct CodexResumeScanBudget {
    /// Entries visited so far.
    pub visited: usize,
    /// Maximum entries to visit before truncating.
    pub max_entries: usize,
    /// Monotonic deadline after which the scan truncates.
    pub deadline: Instant,
    /// Set once the entry or time budget is exhausted.
    pub truncated: bool,
}

impl CodexResumeScanBudget {
    /// Construct a budget from the canonical Codex scan-tuning env vars,
    /// falling back to the built-in defaults.
    pub fn from_env() -> Self {
        let max_entries = non_empty_env("AGENT_SESSION_CODEX_RESUME_SCAN_MAX_ENTRIES")
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or_else(|| {
                env_usize(
                    "AGENT_SESSION_CODEX_CAPTURE_MAX_ENTRIES",
                    CODEX_RESUME_SCAN_MAX_ENTRIES,
                )
            })
            .max(1);
        let slice = Duration::from_millis(env_u64(
            "AGENT_SESSION_CODEX_SCAN_SLICE_MS",
            CODEX_RESUME_SCAN_SLICE_MS,
        ));
        Self {
            visited: 0,
            max_entries,
            deadline: Instant::now() + slice,
            truncated: false,
        }
    }

    /// Account for one entry visit; returns `false` once the budget is spent.
    pub fn visit_entry(&mut self) -> bool {
        if self.visited >= self.max_entries || Instant::now() >= self.deadline {
            self.truncated = true;
            return false;
        }
        self.visited += 1;
        true
    }
}

/// Recursively collect Codex session-history files whose `session_meta` id
/// matches `session_id`, honoring the bounded scan budget.
pub fn collect_codex_provider_resume_matches(
    dir: &Path,
    depth: usize,
    session_id: &str,
    matches: &mut BTreeSet<ProviderHistoryMatch>,
    budget: &mut CodexResumeScanBudget,
) {
    if depth > CODEX_RESUME_SCAN_MAX_DEPTH {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if !budget.visit_entry() {
            return;
        }
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            collect_codex_provider_resume_matches(&path, depth + 1, session_id, matches, budget);
            if budget.truncated {
                return;
            }
            continue;
        }
        if !file_type.is_file()
            || path.extension().and_then(|value| value.to_str()) != Some("jsonl")
        {
            continue;
        }
        if let Some(meta) = read_codex_resumable_session_meta(&path)
            && meta.session_id == session_id
        {
            matches.insert(ProviderHistoryMatch {
                path,
                cwd: meta.cwd,
            });
        }
    }
}

/// Bounded-scan budget for Claude Code project-transcript walks.
///
/// Fields are public so callers can construct a fixed budget in tests; the
/// [`Self::from_env`] constructor reads the canonical tuning env vars.
#[derive(Debug)]
pub struct ClaudeResumeScanBudget {
    /// Entries visited so far.
    pub visited: usize,
    /// Maximum entries to visit before truncating.
    pub max_entries: usize,
    /// Monotonic deadline after which the scan truncates.
    pub deadline: Instant,
    /// Set once the entry or time budget is exhausted.
    pub truncated: bool,
}

impl ClaudeResumeScanBudget {
    /// Construct a budget from the canonical Claude scan-tuning env vars,
    /// falling back to the built-in defaults.
    pub fn from_env() -> Self {
        let max_entries = env_usize(
            "AGENT_SESSION_CLAUDE_RESUME_SCAN_MAX_ENTRIES",
            CLAUDE_RESUME_SCAN_MAX_ENTRIES,
        )
        .max(1);
        let slice = Duration::from_millis(env_u64(
            "AGENT_SESSION_CLAUDE_RESUME_SCAN_SLICE_MS",
            CLAUDE_RESUME_SCAN_SLICE_MS,
        ));
        Self {
            visited: 0,
            max_entries,
            deadline: Instant::now() + slice,
            truncated: false,
        }
    }

    /// Account for one entry visit; returns `false` once the budget is spent.
    pub fn visit_entry(&mut self) -> bool {
        if self.visited >= self.max_entries || Instant::now() >= self.deadline {
            self.truncated = true;
            return false;
        }
        self.visited += 1;
        true
    }
}

/// Collect Claude Code project transcripts whose leading records carry
/// `session_id` and a non-empty `cwd`, honoring the bounded scan budget.
pub fn collect_claude_provider_resume_matches(
    projects_root: &Path,
    session_id: &str,
    matches: &mut BTreeSet<ProviderHistoryMatch>,
    budget: &mut ClaudeResumeScanBudget,
) {
    let Ok(projects) = fs::read_dir(projects_root) else {
        return;
    };
    for project in projects.flatten() {
        if !budget.visit_entry() {
            return;
        }
        let Ok(project_type) = project.file_type() else {
            continue;
        };
        if !project_type.is_dir() {
            continue;
        }
        let Ok(entries) = fs::read_dir(project.path()) else {
            continue;
        };
        for entry in entries.flatten() {
            if !budget.visit_entry() {
                return;
            }
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if !file_type.is_file()
                || path.extension().and_then(|value| value.to_str()) != Some("jsonl")
            {
                continue;
            }
            if let Some(cwd) = read_claude_session_cwd(&path, session_id) {
                matches.insert(ProviderHistoryMatch { path, cwd });
            }
        }
    }
}

/// The `session_meta` fields a Codex rollout file exposes for resume matching.
#[derive(Debug, PartialEq)]
pub struct CodexSessionMeta {
    /// The Codex session id (`payload.id` or `payload.session_id`).
    pub session_id: String,
    /// The recorded original working directory.
    pub cwd: String,
    /// When the session was created (parsed from the record timestamp).
    pub created_at: SystemTime,
}

/// Read the leading `session_meta` record from a Codex rollout file.
///
/// Returns `None` unless the first line is a CLI-origin `session_meta` record
/// with a non-empty id, a cwd, and a parseable timestamp. The first line is
/// size-bounded so a pathological file cannot exhaust memory.
pub fn read_codex_session_meta(path: &Path) -> Option<CodexSessionMeta> {
    read_codex_session_meta_from_sources(path, &["cli"])
}

/// Read Codex session metadata eligible for explicit provider-ID resume.
///
/// Unlike [`read_codex_session_meta`], this accepts both the `cli` and
/// `vscode` scalar source labels used by resumable interactive histories.
pub fn read_codex_resumable_session_meta(path: &Path) -> Option<CodexSessionMeta> {
    read_codex_session_meta_from_sources(path, &["cli", "vscode"])
}

fn read_codex_session_meta_from_sources(
    path: &Path,
    accepted_sources: &[&str],
) -> Option<CodexSessionMeta> {
    let file = fs::File::open(path).ok()?;
    let mut reader = io::BufReader::new(file).take(CODEX_SESSION_META_MAX_LINE_BYTES + 1);
    let mut first_line = Vec::new();
    let read = reader.read_until(b'\n', &mut first_line).ok()?;
    if read == 0 || first_line.len() as u64 > CODEX_SESSION_META_MAX_LINE_BYTES {
        return None;
    }
    if first_line.last() == Some(&b'\n') {
        first_line.pop();
        if first_line.last() == Some(&b'\r') {
            first_line.pop();
        }
    }
    let first_line = std::str::from_utf8(&first_line).ok()?;
    let value: Value = serde_json::from_str(first_line).ok()?;
    if value.get("type").and_then(Value::as_str)? != "session_meta" {
        return None;
    }
    let payload = value.get("payload")?;
    let cwd = payload.get("cwd").and_then(Value::as_str)?.to_string();
    let source = payload.get("source").and_then(Value::as_str)?;
    if !accepted_sources.contains(&source) {
        return None;
    }
    let session_id = payload
        .get("id")
        .or_else(|| payload.get("session_id"))
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .map(str::to_string)?;
    let timestamp = payload
        .get("timestamp")
        .or_else(|| value.get("timestamp"))
        .and_then(Value::as_str)?;
    let created_at: SystemTime = timestamp.parse::<Timestamp>().ok()?.into();
    Some(CodexSessionMeta {
        session_id,
        cwd,
        created_at,
    })
}

/// Read the recorded `cwd` for `session_id` from a Claude Code transcript.
///
/// Scans a bounded number of leading lines for a non-sidechain record whose
/// `sessionId`/`session_id` matches and that carries a non-empty `cwd`.
pub fn read_claude_session_cwd(path: &Path, session_id: &str) -> Option<String> {
    let file = fs::File::open(path).ok()?;
    let mut reader = io::BufReader::new(file);
    let mut line = Vec::new();
    for _ in 0..CLAUDE_SESSION_SCAN_MAX_LINES {
        let within_limit =
            read_bounded_line(&mut reader, &mut line, CLAUDE_SESSION_META_MAX_LINE_BYTES).ok()?;
        let within_limit = within_limit?;
        if !within_limit {
            return None;
        }
        let Ok(line) = std::str::from_utf8(trim_line_ending(&line)) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let id_matches = value
            .get("sessionId")
            .or_else(|| value.get("session_id"))
            .and_then(Value::as_str)
            == Some(session_id);
        if !id_matches || value.get("isSidechain").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        if let Some(cwd) = value
            .get("cwd")
            .and_then(Value::as_str)
            .filter(|cwd| !cwd.trim().is_empty())
        {
            return Some(cwd.to_string());
        }
    }
    None
}

/// Read one newline-terminated line into `buffer`, bounded to `max_bytes`.
///
/// Returns `Ok(None)` at EOF with nothing buffered, `Ok(Some(true))` for a line
/// within the limit, and `Ok(Some(false))` when the line exceeded `max_bytes`
/// (the overflow is discarded without growing the buffer past the limit).
fn read_bounded_line<R: BufRead>(
    reader: &mut R,
    buffer: &mut Vec<u8>,
    max_bytes: usize,
) -> io::Result<Option<bool>> {
    buffer.clear();
    let mut total = 0usize;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok((total > 0).then_some(true));
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let take_len = newline.map_or(available.len(), |index| index + 1);
        let remaining = max_bytes.saturating_sub(total);
        if take_len > remaining {
            buffer.extend_from_slice(&available[..remaining]);
            let consume_len = remaining.saturating_add(1).min(take_len);
            reader.consume(consume_len);
            return Ok(Some(false));
        }
        buffer.extend_from_slice(&available[..take_len]);
        total = total.saturating_add(take_len);
        reader.consume(take_len);
        if newline.is_some() {
            return Ok(Some(true));
        }
    }
}

fn trim_line_ending(bytes: &[u8]) -> &[u8] {
    let bytes = bytes.strip_suffix(b"\n").unwrap_or(bytes);
    bytes.strip_suffix(b"\r").unwrap_or(bytes)
}

fn non_empty_env(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn env_u64(key: &str, default: u64) -> u64 {
    non_empty_env(key)
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(default)
}

fn env_usize(key: &str, default: usize) -> usize {
    non_empty_env(key)
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_session_meta_reader_accepts_resumable_interactive_sources() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("rollout.jsonl");

        for source in ["cli", "vscode"] {
            fs::write(
                &path,
                format!(
                    r#"{{"timestamp":"2099-01-01T00:00:00Z","type":"session_meta","payload":{{"id":"codex-id","session_id":"codex-id","cwd":"/repo","source":"{source}","originator":"codex-tui","timestamp":"2099-01-01T00:00:00Z"}}}}"#,
                ),
            )
            .unwrap();
            let meta = read_codex_resumable_session_meta(&path).expect("resumable session meta");
            assert_eq!(meta.session_id, "codex-id");
            assert_eq!(meta.cwd, "/repo");
        }

        fs::write(
            &path,
            r#"{"timestamp":"2099-01-01T00:00:00Z","type":"session_meta","payload":{"id":"exec-id","cwd":"/repo","source":"exec","originator":"codex_exec","timestamp":"2099-01-01T00:00:00Z"}}"#,
        )
        .unwrap();
        assert_eq!(read_codex_resumable_session_meta(&path), None);

        fs::write(
            &path,
            r#"{"timestamp":"2099-01-01T00:00:00Z","type":"session_meta","payload":{"id":"subagent-id","cwd":"/repo","source":{"subagent":{}},"timestamp":"2099-01-01T00:00:00Z"}}"#,
        )
        .unwrap();
        assert_eq!(read_codex_resumable_session_meta(&path), None);
    }

    #[test]
    fn codex_capture_session_meta_reader_remains_cli_only() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("rollout.jsonl");
        fs::write(
            &path,
            r#"{"timestamp":"2099-01-01T00:00:00Z","type":"session_meta","payload":{"id":"app-server-id","session_id":"app-server-id","cwd":"/repo","source":"vscode","originator":"agent-session","timestamp":"2099-01-01T00:00:00Z"}}"#,
        )
        .unwrap();

        assert_eq!(read_codex_session_meta(&path), None);
    }

    #[test]
    fn codex_session_meta_reader_rejects_oversized_first_line() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("rollout.jsonl");
        fs::write(
            &path,
            format!(
                r#"{{"timestamp":"2099-01-01T00:00:00Z","type":"session_meta","payload":{{"id":"oversized-codex-id","session_id":"oversized-codex-id","cwd":"/repo","source":"cli","timestamp":"2099-01-01T00:00:00Z","pad":"{}"}}}}"#,
                "x".repeat(2 * 1024 * 1024)
            ),
        )
        .unwrap();

        assert_eq!(read_codex_session_meta(&path), None);
    }

    #[test]
    fn bounded_line_reader_discards_oversized_lines_without_growing_buffer() {
        let mut reader = io::Cursor::new(format!("{}\nvalid\n", "x".repeat(32)).into_bytes());
        let mut line = Vec::new();

        assert_eq!(
            read_bounded_line(&mut reader, &mut line, 8).unwrap(),
            Some(false)
        );
        assert_eq!(line.len(), 8);
        assert_eq!(
            reader.position(),
            9,
            "oversized lines must not be drained to newline"
        );
    }

    #[test]
    fn resolve_codex_resume_source_returns_recorded_cwd() {
        let tmp = tempfile::TempDir::new().unwrap();
        let sessions = tmp.path().join("sessions");
        fs::create_dir_all(&sessions).unwrap();
        fs::write(
            sessions.join("rollout.jsonl"),
            r#"{"timestamp":"2099-01-01T00:00:00Z","type":"session_meta","payload":{"id":"target-id","session_id":"target-id","cwd":"/repo/one","source":"vscode","originator":"codex-tui","timestamp":"2099-01-01T00:00:00Z"}}"#,
        )
        .unwrap();

        let resolved =
            resolve_resume_source_in(ResumeProvider::Codex, &sessions, "target-id").unwrap();
        assert_eq!(resolved.cwd, PathBuf::from("/repo/one"));
        assert_eq!(resolved.capture_method, "codex-session-meta-import");
    }

    #[test]
    fn resolve_resume_source_in_config_dir_uses_provider_owned_history_child() {
        let tmp = tempfile::TempDir::new().unwrap();
        let profile_config = tmp.path().join("profile");
        let sessions = profile_config.join("sessions");
        fs::create_dir_all(&sessions).unwrap();
        fs::write(
            sessions.join("rollout.jsonl"),
            r#"{"timestamp":"2099-01-01T00:00:00Z","type":"session_meta","payload":{"id":"profile-id","session_id":"profile-id","cwd":"/repo/profile","source":"cli","timestamp":"2099-01-01T00:00:00Z"}}"#,
        )
        .unwrap();

        let resolved = resolve_resume_source_in_config_dir(
            ResumeProvider::Codex,
            &profile_config,
            "profile-id",
        )
        .unwrap();

        assert_eq!(resolved.cwd, PathBuf::from("/repo/profile"));
        assert_eq!(resolved.capture_method, "codex-session-meta-import");
    }

    #[test]
    fn resolve_canonical_codex_resume_source_before_broad_scan_budget() {
        let tmp = tempfile::TempDir::new().unwrap();
        let session_id = "019f5c96-65c0-7140-8657-0ce2ecb45aa5";
        let day = tmp.path().join("sessions/2026/07/13");
        fs::create_dir_all(&day).unwrap();
        fs::write(
            day.join(format!(
                "rollout-2026-07-13T17-46-28-{session_id}.jsonl"
            )),
            format!(
                r#"{{"timestamp":"2026-07-13T17:46:28.216Z","type":"session_meta","payload":{{"id":"{session_id}","session_id":"{session_id}","cwd":"/repo/exact","source":"cli","timestamp":"2026-07-13T17:46:28.216Z"}}}}"#
            ),
        )
        .unwrap();

        let mut one_entry_budget = CodexResumeScanBudget {
            visited: 0,
            max_entries: 1,
            deadline: Instant::now() + Duration::from_secs(60),
            truncated: false,
        };
        let resolved = resolve_codex_resume_source_in_with_budget(
            &tmp.path().join("sessions"),
            session_id,
            &mut one_entry_budget,
        )
        .unwrap();

        assert_eq!(resolved.cwd, PathBuf::from("/repo/exact"));
        assert_eq!(resolved.capture_method, "codex-session-meta-import");
    }

    #[test]
    fn resolve_uuid_v7_codex_resume_source_falls_back_for_noncanonical_history() {
        let tmp = tempfile::TempDir::new().unwrap();
        let session_id = "019f5c96-65c0-7140-8657-0ce2ecb45aa5";
        let fallback_layout = tmp.path().join("sessions/history");
        fs::create_dir_all(&fallback_layout).unwrap();
        fs::write(
            fallback_layout.join("rollout.jsonl"),
            format!(
                r#"{{"timestamp":"2026-07-13T17:46:28.216Z","type":"session_meta","payload":{{"id":"{session_id}","session_id":"{session_id}","cwd":"/repo/fallback","source":"cli","timestamp":"2026-07-13T17:46:28.216Z"}}}}"#
            ),
        )
        .unwrap();

        let resolved = resolve_resume_source_in(
            ResumeProvider::Codex,
            &tmp.path().join("sessions"),
            session_id,
        )
        .unwrap();

        assert_eq!(resolved.cwd, PathBuf::from("/repo/fallback"));
        assert_eq!(resolved.capture_method, "codex-session-meta-import");
    }

    #[test]
    fn resolve_uuid_v7_codex_resume_source_prefers_canonical_history() {
        let tmp = tempfile::TempDir::new().unwrap();
        let session_id = "019f5c96-65c0-7140-8657-0ce2ecb45aa5";
        let day = tmp.path().join("sessions/2026/07/13");
        let other_layout = tmp.path().join("sessions/history");
        fs::create_dir_all(&day).unwrap();
        fs::create_dir_all(&other_layout).unwrap();
        fs::write(
            day.join(format!("rollout-{session_id}.jsonl")),
            format!(
                r#"{{"timestamp":"2026-07-13T17:46:28.216Z","type":"session_meta","payload":{{"id":"{session_id}","session_id":"{session_id}","cwd":"/repo/canonical","source":"cli","timestamp":"2026-07-13T17:46:28.216Z"}}}}"#
            ),
        )
        .unwrap();
        fs::write(
            other_layout.join("rollout.jsonl"),
            format!(
                r#"{{"timestamp":"2026-07-13T17:46:28.216Z","type":"session_meta","payload":{{"id":"{session_id}","session_id":"{session_id}","cwd":"/repo/other","source":"cli","timestamp":"2026-07-13T17:46:28.216Z"}}}}"#
            ),
        )
        .unwrap();

        let resolved = resolve_resume_source_in(
            ResumeProvider::Codex,
            &tmp.path().join("sessions"),
            session_id,
        )
        .unwrap();

        assert_eq!(resolved.cwd, PathBuf::from("/repo/canonical"));
    }

    #[test]
    fn resolve_uuid_v7_codex_resume_source_shares_fast_and_fallback_budget() {
        let tmp = tempfile::TempDir::new().unwrap();
        let session_id = "019f5c96-65c0-7140-8657-0ce2ecb45aa5";
        let day = tmp.path().join("sessions/2026/07/13");
        let fallback_layout = tmp.path().join("sessions/history");
        fs::create_dir_all(&day).unwrap();
        fs::create_dir_all(&fallback_layout).unwrap();
        fs::write(day.join("unrelated.jsonl"), "{}\n").unwrap();
        fs::write(
            fallback_layout.join("rollout.jsonl"),
            format!(
                r#"{{"timestamp":"2026-07-13T17:46:28.216Z","type":"session_meta","payload":{{"id":"{session_id}","session_id":"{session_id}","cwd":"/repo/fallback","source":"cli","timestamp":"2026-07-13T17:46:28.216Z"}}}}"#
            ),
        )
        .unwrap();

        let mut one_entry_budget = CodexResumeScanBudget {
            visited: 0,
            max_entries: 1,
            deadline: Instant::now() + Duration::from_secs(60),
            truncated: false,
        };
        assert_eq!(
            resolve_codex_resume_source_in_with_budget(
                &tmp.path().join("sessions"),
                session_id,
                &mut one_entry_budget,
            ),
            Err(ResumeResolveError::Truncated)
        );
        assert_eq!(one_entry_budget.visited, 1);
        assert!(one_entry_budget.truncated);
    }

    #[test]
    fn resolve_codex_resume_source_missing_id_is_not_found() {
        let tmp = tempfile::TempDir::new().unwrap();
        let sessions = tmp.path().join("sessions");
        fs::create_dir_all(&sessions).unwrap();

        assert_eq!(
            resolve_resume_source_in(ResumeProvider::Codex, &sessions, "absent-id"),
            Err(ResumeResolveError::NotFound)
        );
    }

    #[test]
    fn resolve_codex_resume_source_multiple_cwds_is_ambiguous() {
        let tmp = tempfile::TempDir::new().unwrap();
        let sessions = tmp.path().join("sessions");
        fs::create_dir_all(&sessions).unwrap();
        fs::write(
            sessions.join("a.jsonl"),
            r#"{"timestamp":"2099-01-01T00:00:00Z","type":"session_meta","payload":{"id":"dup-id","session_id":"dup-id","cwd":"/repo/a","source":"cli","timestamp":"2099-01-01T00:00:00Z"}}"#,
        )
        .unwrap();
        fs::write(
            sessions.join("b.jsonl"),
            r#"{"timestamp":"2099-01-01T00:00:00Z","type":"session_meta","payload":{"id":"dup-id","session_id":"dup-id","cwd":"/repo/b","source":"cli","timestamp":"2099-01-01T00:00:00Z"}}"#,
        )
        .unwrap();

        assert_eq!(
            resolve_resume_source_in(ResumeProvider::Codex, &sessions, "dup-id"),
            Err(ResumeResolveError::Ambiguous { cwd_count: 2 })
        );
    }

    #[test]
    fn resolve_codex_resume_source_ignores_non_matching_id() {
        let tmp = tempfile::TempDir::new().unwrap();
        let sessions = tmp.path().join("sessions");
        fs::create_dir_all(&sessions).unwrap();
        fs::write(
            sessions.join("rollout.jsonl"),
            r#"{"timestamp":"2099-01-01T00:00:00Z","type":"session_meta","payload":{"id":"other-id","session_id":"other-id","cwd":"/repo/one","source":"cli","timestamp":"2099-01-01T00:00:00Z"}}"#,
        )
        .unwrap();

        assert_eq!(
            resolve_resume_source_in(ResumeProvider::Codex, &sessions, "target-id"),
            Err(ResumeResolveError::NotFound)
        );
    }

    #[test]
    fn resolve_claude_resume_source_returns_recorded_cwd() {
        let tmp = tempfile::TempDir::new().unwrap();
        let projects = tmp.path().join("projects");
        let project = projects.join("-repo-two");
        fs::create_dir_all(&project).unwrap();
        fs::write(
            project.join("session.jsonl"),
            "{\"sessionId\":\"claude-id\",\"cwd\":\"/repo/two\"}\n",
        )
        .unwrap();

        let resolved =
            resolve_resume_source_in(ResumeProvider::Claude, &projects, "claude-id").unwrap();
        assert_eq!(resolved.cwd, PathBuf::from("/repo/two"));
        assert_eq!(resolved.capture_method, "claude-project-transcript-import");
    }

    #[test]
    fn resolve_claude_resume_source_skips_sidechain_records() {
        let tmp = tempfile::TempDir::new().unwrap();
        let projects = tmp.path().join("projects");
        let project = projects.join("-repo-three");
        fs::create_dir_all(&project).unwrap();
        fs::write(
            project.join("session.jsonl"),
            "{\"sessionId\":\"claude-id\",\"isSidechain\":true,\"cwd\":\"/repo/three\"}\n",
        )
        .unwrap();

        assert_eq!(
            resolve_resume_source_in(ResumeProvider::Claude, &projects, "claude-id"),
            Err(ResumeResolveError::NotFound)
        );
    }

    #[test]
    fn codex_scan_truncates_by_entry_budget() {
        let tmp = tempfile::TempDir::new().unwrap();
        let sessions = tmp.path().join("sessions");
        fs::create_dir_all(&sessions).unwrap();
        for index in 0..3 {
            fs::write(
                sessions.join(format!("s{index}.jsonl")),
                r#"{"timestamp":"2099-01-01T00:00:00Z","type":"session_meta","payload":{"id":"dup","session_id":"dup","cwd":"/repo","source":"cli","timestamp":"2099-01-01T00:00:00Z"}}"#,
            )
            .unwrap();
        }

        let mut matches = BTreeSet::new();
        let mut budget = CodexResumeScanBudget {
            visited: 0,
            max_entries: 1,
            deadline: Instant::now() + Duration::from_secs(60),
            truncated: false,
        };
        collect_codex_provider_resume_matches(&sessions, 0, "dup", &mut matches, &mut budget);
        assert!(budget.truncated, "entry budget must truncate the scan");
    }

    #[test]
    fn claude_scan_truncates_by_entry_budget() {
        let tmp = tempfile::TempDir::new().unwrap();
        let projects = tmp.path().join("projects");
        for index in 0..3 {
            let project = projects.join(format!("-proj-{index}"));
            fs::create_dir_all(&project).unwrap();
            fs::write(
                project.join("session.jsonl"),
                "{\"sessionId\":\"dup\",\"cwd\":\"/repo\"}\n",
            )
            .unwrap();
        }

        let mut matches = BTreeSet::new();
        let mut budget = ClaudeResumeScanBudget {
            visited: 0,
            max_entries: 1,
            deadline: Instant::now() + Duration::from_secs(60),
            truncated: false,
        };
        collect_claude_provider_resume_matches(&projects, "dup", &mut matches, &mut budget);
        assert!(budget.truncated, "entry budget must truncate the scan");
    }

    #[test]
    fn normalize_resume_id_trims_and_rejects_bad_ids() {
        assert_eq!(normalize_resume_id("  abc  ").unwrap(), "abc");
        assert_eq!(normalize_resume_id("   "), Err(ResumeIdError::Empty));
        assert_eq!(normalize_resume_id("a\nb"), Err(ResumeIdError::ControlChar));
    }
}
