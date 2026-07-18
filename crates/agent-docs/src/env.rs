use std::env;
#[cfg(unix)]
use std::ffi::OsString;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::Serialize;

use crate::paths::normalize_root_path;

/// Install symlinks the harness creates; the docs-home is the directory that
/// holds the file each symlink resolves to (`AGENT_HOME.md`).
const INSTALL_SYMLINKS: [&str; 2] = [".claude/CLAUDE.md", ".codex/AGENTS.md"];

#[derive(Debug, Clone, Default)]
pub struct PathOverrides {
    pub docs_home: Option<PathBuf>,
    pub project_path: Option<PathBuf>,
}

/// How the docs-home was located.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum DocsHomeSource {
    /// Explicit `--docs-home` flag.
    Flag,
    /// Derived from the install symlink (`dirname(readlink ~/.claude/CLAUDE.md)`).
    Symlink,
    /// The `AGENT_DOCS_HOME` environment variable (lowest-precedence fallback).
    Env,
}

impl DocsHomeSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Flag => "flag",
            Self::Symlink => "symlink",
            Self::Env => "env",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RootIdentity {
    pub canonical_path: PathBuf,
    pub git_common_dir: Option<PathBuf>,
}

impl RootIdentity {
    /// Match only exact roots or two roots positively identified as the same
    /// Git repository. A Git/non-Git or non-Git/non-Git mismatch fails closed.
    pub fn matches(&self, other: &Self) -> bool {
        self.canonical_path == other.canonical_path
            || matches!(
                (&self.git_common_dir, &other.git_common_dir),
                (Some(left), Some(right)) if left == right
            )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PrimaryWorktreeSource {
    /// A non-bare primary root verified from `git worktree list --porcelain`.
    VerifiedWorktreeRecord,
}

impl PrimaryWorktreeSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::VerifiedWorktreeRecord => "verified-worktree-record",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PrimaryWorktreeFallback {
    /// The primary path equivalent to the requested project path. For a
    /// project in a linked-worktree subdirectory, this includes that suffix.
    pub path: PathBuf,
    pub source: PrimaryWorktreeSource,
}

#[derive(Debug, Clone)]
pub struct ResolvedRoots {
    pub docs_home: PathBuf,
    pub docs_home_source: DocsHomeSource,
    pub project_path: PathBuf,
    pub is_linked_worktree: bool,
    pub git_common_dir: Option<PathBuf>,
    pub primary_worktree_path: Option<PathBuf>,
    pub worktree_roots: Vec<PathBuf>,
    pub docs_home_identity: RootIdentity,
    pub project_identity: RootIdentity,
}

impl ResolvedRoots {
    /// Construct roots for explicit paths (used by tests and internal callers).
    pub fn for_paths(docs_home: PathBuf, project_path: PathBuf) -> Self {
        let docs_home_identity = root_identity_without_git(&docs_home);
        let project_identity = root_identity_without_git(&project_path);
        Self {
            docs_home,
            docs_home_source: DocsHomeSource::Flag,
            project_path,
            is_linked_worktree: false,
            git_common_dir: None,
            primary_worktree_path: None,
            worktree_roots: Vec::new(),
            docs_home_identity,
            project_identity,
        }
    }

    pub fn docs_home_matches_project(&self) -> bool {
        self.docs_home_identity.matches(&self.project_identity)
    }

    pub fn primary_worktree_fallback(&self) -> Option<PrimaryWorktreeFallback> {
        self.primary_worktree_path
            .clone()
            .map(|path| PrimaryWorktreeFallback {
                path,
                source: PrimaryWorktreeSource::VerifiedWorktreeRecord,
            })
    }
}

#[derive(Debug, Clone)]
pub struct ProjectIdentity {
    pub project_path: PathBuf,
    pub is_linked_worktree: bool,
    pub git_common_dir: Option<PathBuf>,
    pub primary_worktree_path: Option<PathBuf>,
    /// Every non-bare worktree reported by Git, including normalized paths for
    /// prunable records whose destinations no longer exist.
    pub worktree_roots: Vec<PathBuf>,
}

impl ProjectIdentity {
    pub fn root_identity(&self) -> RootIdentity {
        RootIdentity {
            canonical_path: self.project_path.clone(),
            git_common_dir: self.git_common_dir.clone(),
        }
    }

    pub fn primary_worktree_fallback(&self) -> Option<PrimaryWorktreeFallback> {
        self.primary_worktree_path
            .clone()
            .map(|path| PrimaryWorktreeFallback {
                path,
                source: PrimaryWorktreeSource::VerifiedWorktreeRecord,
            })
    }
}

pub fn resolve_project_identity(project_override: Option<&Path>) -> Result<ProjectIdentity> {
    let cwd = env::current_dir().context("failed to read current directory")?;
    let project_path = resolve_project_path(project_override, &cwd)?;
    let project_path = std::fs::canonicalize(&project_path).with_context(|| {
        format!(
            "failed to canonicalize project path {}",
            project_path.display()
        )
    })?;
    let metadata = resolve_linked_worktree_metadata(&project_path)?;
    Ok(ProjectIdentity {
        project_path,
        is_linked_worktree: metadata.is_linked_worktree,
        git_common_dir: metadata.git_common_dir,
        primary_worktree_path: metadata.primary_worktree_path,
        worktree_roots: metadata.worktree_roots,
    })
}

pub fn resolve_roots(overrides: &PathOverrides) -> Result<ResolvedRoots> {
    let cwd = env::current_dir().context("failed to read current directory")?;
    let (docs_home, docs_home_source) = resolve_docs_home(overrides.docs_home.as_deref(), &cwd)?;
    let identity = resolve_project_identity(overrides.project_path.as_deref())?;
    let project_identity = identity.root_identity();
    let canonical_docs_home = std::fs::canonicalize(&docs_home).with_context(|| {
        format!(
            "failed to canonicalize docs-home path {}",
            docs_home.display()
        )
    })?;
    let docs_home_identity = if canonical_docs_home == project_identity.canonical_path {
        project_identity.clone()
    } else {
        resolve_root_identity(&canonical_docs_home)?
    };

    Ok(ResolvedRoots {
        docs_home,
        docs_home_source,
        project_path: identity.project_path,
        is_linked_worktree: identity.is_linked_worktree,
        git_common_dir: identity.git_common_dir,
        primary_worktree_path: identity.primary_worktree_path,
        worktree_roots: identity.worktree_roots,
        docs_home_identity,
        project_identity,
    })
}

fn resolve_docs_home(cli_value: Option<&Path>, cwd: &Path) -> Result<(PathBuf, DocsHomeSource)> {
    if let Some(path) = cli_value {
        return Ok((normalize_root_path(path, cwd), DocsHomeSource::Flag));
    }

    if let Some(path) = derive_docs_home_from_symlinks(home_dir().as_deref()) {
        return Ok((normalize_root_path(&path, cwd), DocsHomeSource::Symlink));
    }

    if let Some(path) = read_env_path("AGENT_DOCS_HOME") {
        return Ok((normalize_root_path(&path, cwd), DocsHomeSource::Env));
    }

    bail!(
        "cannot locate docs-home: no --docs-home, the install symlink \
         (~/.claude/CLAUDE.md or ~/.codex/AGENTS.md) does not resolve, and \
         AGENT_DOCS_HOME is unset; pass --docs-home <path>"
    )
}

/// The user's home directory, honoring `HOME` (so it can be overridden in
/// subprocess tests) with a `USERPROFILE` fallback for Windows.
pub fn home_dir() -> Option<PathBuf> {
    nils_common::fs::home_dir().or_else(|| read_env_path("USERPROFILE"))
}

/// Derive the docs-home from the harness install symlinks: the docs-home is the
/// directory that contains the file `~/.claude/CLAUDE.md` (or the Codex
/// equivalent) resolves to.
pub fn derive_docs_home_from_symlinks(home: Option<&Path>) -> Option<PathBuf> {
    let home = home?;
    for relative in INSTALL_SYMLINKS {
        let link = home.join(relative);
        if let Some(docs_home) = docs_home_from_symlink(&link) {
            return Some(docs_home);
        }
    }
    None
}

fn docs_home_from_symlink(link: &Path) -> Option<PathBuf> {
    // The link must itself be a symlink; a plain file is not the install wiring.
    let meta = std::fs::symlink_metadata(link).ok()?;
    if !meta.file_type().is_symlink() {
        return None;
    }
    let resolved = std::fs::canonicalize(link).ok()?;
    resolved.parent().map(Path::to_path_buf)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SymlinkWiring {
    /// A symlink is present and resolves into the docs-home.
    Intact,
    /// A symlink is present but resolves elsewhere.
    Mismatch,
    /// No install symlink is present.
    Missing,
}

/// Inspect the install symlink wiring relative to a resolved docs-home, for
/// `audit` reporting.
pub fn inspect_symlink_wiring(home: Option<&Path>, docs_home: &Path) -> (SymlinkWiring, String) {
    let Some(home) = home else {
        return (
            SymlinkWiring::Missing,
            "HOME is unset; cannot inspect install symlink".to_string(),
        );
    };
    let canonical_docs_home =
        std::fs::canonicalize(docs_home).unwrap_or_else(|_| docs_home.to_path_buf());
    for relative in INSTALL_SYMLINKS {
        let link = home.join(relative);
        let Ok(meta) = std::fs::symlink_metadata(&link) else {
            continue;
        };
        if !meta.file_type().is_symlink() {
            continue;
        }
        let Ok(resolved) = std::fs::canonicalize(&link) else {
            return (
                SymlinkWiring::Mismatch,
                format!("{} is a broken symlink", link.display()),
            );
        };
        let resolved_home = resolved.parent().map(Path::to_path_buf).unwrap_or(resolved);
        if resolved_home == canonical_docs_home {
            return (
                SymlinkWiring::Intact,
                format!("{} -> {}", link.display(), resolved_home.display()),
            );
        }
        return (
            SymlinkWiring::Mismatch,
            format!(
                "{} resolves to {} but docs-home is {}",
                link.display(),
                resolved_home.display(),
                canonical_docs_home.display()
            ),
        );
    }
    (
        SymlinkWiring::Missing,
        format!(
            "no install symlink found under {} ({})",
            home.display(),
            INSTALL_SYMLINKS.join(", ")
        ),
    )
}

fn resolve_project_path(cli_value: Option<&Path>, cwd: &Path) -> Result<PathBuf> {
    if let Some(path) = cli_value {
        return Ok(normalize_root_path(path, cwd));
    }

    if let Some(path) = read_env_path("PROJECT_PATH") {
        return Ok(normalize_root_path(&path, cwd));
    }

    if let Some(path) = git_top_level(cwd)? {
        return Ok(normalize_root_path(&path, cwd));
    }

    Ok(normalize_root_path(cwd, cwd))
}

fn read_env_path(name: &str) -> Option<PathBuf> {
    let raw = env::var_os(name)?;
    if raw.is_empty() {
        None
    } else {
        Some(PathBuf::from(raw))
    }
}

fn git_top_level(cwd: &Path) -> Result<Option<PathBuf>> {
    git_rev_parse_path(cwd, "--show-toplevel")
}

#[derive(Debug, Default)]
struct LinkedWorktreeMetadata {
    is_linked_worktree: bool,
    git_common_dir: Option<PathBuf>,
    primary_worktree_path: Option<PathBuf>,
    worktree_roots: Vec<PathBuf>,
}

fn root_identity_without_git(path: &Path) -> RootIdentity {
    RootIdentity {
        canonical_path: std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()),
        git_common_dir: None,
    }
}

fn resolve_root_identity(path: &Path) -> Result<RootIdentity> {
    let canonical_path = std::fs::canonicalize(path)
        .with_context(|| format!("failed to canonicalize root path {}", path.display()))?;
    let git_common_dir = resolve_git_common_dir(&canonical_path)?;
    Ok(RootIdentity {
        canonical_path,
        git_common_dir,
    })
}

fn resolve_git_common_dir(path: &Path) -> Result<Option<PathBuf>> {
    if git_rev_parse_path(path, "--absolute-git-dir")?.is_none() {
        return Ok(None);
    }
    let common = git_required_rev_parse_path(path, "--git-common-dir")?;
    Ok(Some(canonicalize_git_path(&common, "Git common dir")?))
}

fn resolve_linked_worktree_metadata(project: &Path) -> Result<LinkedWorktreeMetadata> {
    let Some(absolute_git_dir) = git_rev_parse_path(project, "--absolute-git-dir")? else {
        return Ok(LinkedWorktreeMetadata::default());
    };
    let git_common_dir = git_required_rev_parse_path(project, "--git-common-dir")?;
    let absolute_git_dir = canonicalize_git_path(&absolute_git_dir, "Git directory")?;
    let git_common_dir = canonicalize_git_path(&git_common_dir, "Git common dir")?;
    let worktrees = git_worktree_records(project)?;
    let worktree_roots = worktrees
        .iter()
        .filter(|record| !record.bare)
        .map(|record| record.path.clone())
        .collect();

    if git_rev_parse_bool(project, "--is-bare-repository")? {
        return Ok(LinkedWorktreeMetadata {
            git_common_dir: Some(git_common_dir),
            worktree_roots,
            ..LinkedWorktreeMetadata::default()
        });
    }

    let top_level = git_required_rev_parse_path(project, "--show-toplevel")?;
    let top_level = canonicalize_git_path(&top_level, "Git top-level")?;
    let relative_suffix = project.strip_prefix(&top_level).with_context(|| {
        format!(
            "project path {} is outside its Git worktree root {}",
            project.display(),
            top_level.display()
        )
    })?;
    if !worktrees
        .iter()
        .any(|record| !record.prunable && record.path == top_level)
    {
        bail!(
            "Git common dir {} does not list project worktree {}",
            git_common_dir.display(),
            top_level.display()
        );
    }

    let is_linked_worktree = absolute_git_dir != git_common_dir;
    let primary_worktree_path = is_linked_worktree
        .then(|| verified_primary_worktree_root(&worktrees, &top_level, &git_common_dir))
        .flatten()
        .map(|root| root.join(relative_suffix));

    Ok(LinkedWorktreeMetadata {
        is_linked_worktree,
        git_common_dir: Some(git_common_dir),
        primary_worktree_path,
        worktree_roots,
    })
}

fn canonicalize_git_path(path: &Path, label: &str) -> Result<PathBuf> {
    std::fs::canonicalize(path)
        .with_context(|| format!("failed to canonicalize {label} {}", path.display()))
}

fn verified_primary_worktree_root(
    records: &[GitWorktreeRecord],
    current_root: &Path,
    git_common_dir: &Path,
) -> Option<PathBuf> {
    records.iter().find_map(|record| {
        if record.bare || record.prunable || record.path == current_root {
            return None;
        }
        let dot_git = record.path.join(".git");
        let metadata = std::fs::symlink_metadata(&dot_git).ok()?;
        if !metadata.file_type().is_dir() {
            return None;
        }
        let candidate_common = std::fs::canonicalize(dot_git).ok()?;
        (candidate_common == git_common_dir).then(|| record.path.clone())
    })
}

fn git_rev_parse_path(cwd: &Path, arg: &str) -> Result<Option<PathBuf>> {
    let output = run_git_output_sanitized(cwd, &["rev-parse", arg])
        .with_context(|| format!("failed to execute git rev-parse {arg}"))?;
    if !output.status.success() {
        if git_reports_non_repository(&output.stderr)
            || (arg == "--show-toplevel" && git_reports_no_worktree(&output.stderr))
        {
            return Ok(None);
        }
        bail!(
            "git rev-parse {arg} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let raw = trim_git_line_ending(&output.stdout);
    if raw.is_empty() {
        bail!("git rev-parse {arg} returned an empty path");
    }
    let path = path_from_git_bytes(raw)?;
    Ok(Some(normalize_root_path(&path, cwd)))
}

fn git_required_rev_parse_path(cwd: &Path, arg: &str) -> Result<PathBuf> {
    git_rev_parse_path(cwd, arg)?.with_context(|| {
        format!(
            "Git repository at {} omitted rev-parse result for {arg}",
            cwd.display()
        )
    })
}

fn git_rev_parse_bool(cwd: &Path, arg: &str) -> Result<bool> {
    let output = run_git_output_sanitized(cwd, &["rev-parse", arg])
        .with_context(|| format!("failed to execute git rev-parse {arg}"))?;
    if !output.status.success() {
        bail!(
            "git rev-parse {arg} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    match trim_ascii_whitespace(&output.stdout) {
        b"true" => Ok(true),
        b"false" => Ok(false),
        value => bail!(
            "git rev-parse {arg} returned unexpected value {:?}",
            String::from_utf8_lossy(value)
        ),
    }
}

fn git_reports_non_repository(stderr: &[u8]) -> bool {
    stderr
        .windows(b"not a git repository".len())
        .any(|window| window == b"not a git repository")
}

fn git_reports_no_worktree(stderr: &[u8]) -> bool {
    stderr
        .windows(b"must be run in a work tree".len())
        .any(|window| window == b"must be run in a work tree")
}

#[derive(Debug)]
struct GitWorktreeRecord {
    path: PathBuf,
    bare: bool,
    prunable: bool,
}

fn git_worktree_records(project: &Path) -> Result<Vec<GitWorktreeRecord>> {
    const MAX_WORKTREE_RECORDS: usize = 1024;

    let output = run_git_output_sanitized(project, &["worktree", "list", "--porcelain", "-z"])
        .context("failed to list Git worktrees")?;
    if !output.status.success() {
        bail!(
            "git worktree list failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    let mut records = Vec::new();
    let mut fields = Vec::new();
    let mut record_count = 0_usize;
    for field in output.stdout.split(|byte| *byte == 0) {
        if field.is_empty() {
            if fields.is_empty() {
                continue;
            }
            record_count += 1;
            if record_count > MAX_WORKTREE_RECORDS {
                bail!("git worktree list exceeded the {MAX_WORKTREE_RECORDS}-record safety limit");
            }
            if let Some(record) = parse_worktree_record(&fields, project)? {
                records.push(record);
            }
            fields.clear();
        } else {
            fields.push(field);
        }
    }
    if !fields.is_empty() {
        record_count += 1;
        if record_count > MAX_WORKTREE_RECORDS {
            bail!("git worktree list exceeded the {MAX_WORKTREE_RECORDS}-record safety limit");
        }
        if let Some(record) = parse_worktree_record(&fields, project)? {
            records.push(record);
        }
    }
    Ok(records)
}

fn parse_worktree_record(fields: &[&[u8]], project: &Path) -> Result<Option<GitWorktreeRecord>> {
    let prunable = fields
        .iter()
        .any(|field| *field == b"prunable" || field.starts_with(b"prunable "));
    let raw_path = fields
        .iter()
        .find_map(|field| field.strip_prefix(b"worktree "))
        .context("git worktree list returned a record without a worktree path")?;
    let raw_path = path_from_git_bytes(raw_path)?;
    let path = if prunable {
        normalize_root_path(&raw_path, project)
    } else {
        std::fs::canonicalize(&raw_path).with_context(|| {
            format!("failed to canonicalize Git worktree {}", raw_path.display())
        })?
    };
    Ok(Some(GitWorktreeRecord {
        path,
        bare: fields.contains(&b"bare".as_slice()),
        prunable,
    }))
}

const GIT_COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
const GIT_STDOUT_LIMIT: usize = 1024 * 1024;
const GIT_STDERR_LIMIT: usize = 256 * 1024;

pub(crate) fn run_git_output_sanitized(cwd: &Path, args: &[&str]) -> io::Result<Output> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(cwd)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("LC_ALL", "C");
    for (name, _) in env::vars_os() {
        if name
            .as_encoded_bytes()
            .get(..4)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"GIT_"))
        {
            command.env_remove(name);
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }

    let mut child = command.spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("failed to capture Git stdout"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("failed to capture Git stderr"))?;
    let stdout_reader = thread::spawn(move || read_bounded(stdout, GIT_STDOUT_LIMIT));
    let stderr_reader = thread::spawn(move || read_bounded(stderr, GIT_STDERR_LIMIT));
    let deadline = Instant::now() + GIT_COMMAND_TIMEOUT;

    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                kill_and_reap(&mut child);
                let _ = join_bounded_reader(stdout_reader);
                let _ = join_bounded_reader(stderr_reader);
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!(
                        "Git command exceeded the {}-second timeout",
                        GIT_COMMAND_TIMEOUT.as_secs()
                    ),
                ));
            }
            Err(err) => {
                kill_and_reap(&mut child);
                let _ = join_bounded_reader(stdout_reader);
                let _ = join_bounded_reader(stderr_reader);
                return Err(err);
            }
        }
    };

    let stdout_result = join_bounded_reader(stdout_reader);
    let stderr_result = join_bounded_reader(stderr_reader);
    let stdout = stdout_result?;
    let stderr = stderr_result?;
    if stdout.exceeded {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Git stdout exceeded the {GIT_STDOUT_LIMIT}-byte safety limit"),
        ));
    }
    if stderr.exceeded {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Git stderr exceeded the {GIT_STDERR_LIMIT}-byte safety limit"),
        ));
    }
    Ok(Output {
        status,
        stdout: stdout.bytes,
        stderr: stderr.bytes,
    })
}

#[derive(Debug)]
struct BoundedRead {
    bytes: Vec<u8>,
    exceeded: bool,
}

fn read_bounded(mut pipe: impl Read, limit: usize) -> io::Result<BoundedRead> {
    let mut bytes = Vec::new();
    let mut exceeded = false;
    let mut buffer = [0_u8; 8192];
    loop {
        let count = pipe.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        let remaining = limit.saturating_sub(bytes.len());
        let retained = count.min(remaining);
        bytes.extend_from_slice(&buffer[..retained]);
        exceeded |= retained < count;
    }
    Ok(BoundedRead { bytes, exceeded })
}

fn join_bounded_reader(
    reader: thread::JoinHandle<io::Result<BoundedRead>>,
) -> io::Result<BoundedRead> {
    reader
        .join()
        .map_err(|_| io::Error::other("Git output reader thread panicked"))?
}

fn kill_and_reap(child: &mut Child) {
    #[cfg(unix)]
    {
        let process_group = -(child.id() as i32);
        // SAFETY: the child was placed in a new process group with its pid as
        // the group id immediately before spawn.
        if unsafe { libc::kill(process_group, libc::SIGKILL) } != 0 {
            let _ = child.kill();
        }
    }
    #[cfg(not(unix))]
    let _ = child.kill();
    let _ = child.wait();
}

fn trim_git_line_ending(mut bytes: &[u8]) -> &[u8] {
    if let Some(stripped) = bytes.strip_suffix(b"\n") {
        bytes = stripped;
    }
    if let Some(stripped) = bytes.strip_suffix(b"\r") {
        bytes = stripped;
    }
    bytes
}

fn trim_ascii_whitespace(mut bytes: &[u8]) -> &[u8] {
    while bytes.first().is_some_and(u8::is_ascii_whitespace) {
        bytes = &bytes[1..];
    }
    while bytes.last().is_some_and(u8::is_ascii_whitespace) {
        bytes = &bytes[..bytes.len() - 1];
    }
    bytes
}

#[cfg(unix)]
fn path_from_git_bytes(bytes: &[u8]) -> Result<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    Ok(PathBuf::from(OsString::from_vec(bytes.to_vec())))
}

#[cfg(not(unix))]
fn path_from_git_bytes(bytes: &[u8]) -> Result<PathBuf> {
    let value = String::from_utf8(bytes.to_vec())
        .map_err(|_| anyhow::anyhow!("Git returned a non-UTF-8 path on this platform"))?;
    Ok(PathBuf::from(value))
}
