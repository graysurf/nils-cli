use std::env;
#[cfg(unix)]
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

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

#[derive(Debug, Clone)]
pub struct ResolvedRoots {
    pub docs_home: PathBuf,
    pub docs_home_source: DocsHomeSource,
    pub project_path: PathBuf,
    pub is_linked_worktree: bool,
    pub git_common_dir: Option<PathBuf>,
    pub primary_worktree_path: Option<PathBuf>,
}

impl ResolvedRoots {
    /// Construct roots for explicit paths (used by tests and internal callers).
    pub fn for_paths(docs_home: PathBuf, project_path: PathBuf) -> Self {
        Self {
            docs_home,
            docs_home_source: DocsHomeSource::Flag,
            project_path,
            is_linked_worktree: false,
            git_common_dir: None,
            primary_worktree_path: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProjectIdentity {
    pub project_path: PathBuf,
    pub is_linked_worktree: bool,
    pub git_common_dir: Option<PathBuf>,
    pub primary_worktree_path: Option<PathBuf>,
}

pub fn resolve_project_identity(project_override: Option<&Path>) -> Result<ProjectIdentity> {
    let cwd = env::current_dir().context("failed to read current directory")?;
    let project_path = resolve_project_path(project_override, &cwd);
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
    })
}

pub fn resolve_roots(overrides: &PathOverrides) -> Result<ResolvedRoots> {
    let cwd = env::current_dir().context("failed to read current directory")?;
    let (docs_home, docs_home_source) = resolve_docs_home(overrides.docs_home.as_deref(), &cwd)?;
    let identity = resolve_project_identity(overrides.project_path.as_deref())?;

    Ok(ResolvedRoots {
        docs_home,
        docs_home_source,
        project_path: identity.project_path,
        is_linked_worktree: identity.is_linked_worktree,
        git_common_dir: identity.git_common_dir,
        primary_worktree_path: identity.primary_worktree_path,
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

fn resolve_project_path(cli_value: Option<&Path>, cwd: &Path) -> PathBuf {
    if let Some(path) = cli_value {
        return normalize_root_path(path, cwd);
    }

    if let Some(path) = read_env_path("PROJECT_PATH") {
        return normalize_root_path(&path, cwd);
    }

    if let Some(path) = git_top_level(cwd) {
        return normalize_root_path(&path, cwd);
    }

    normalize_root_path(cwd, cwd)
}

fn read_env_path(name: &str) -> Option<PathBuf> {
    let raw = env::var_os(name)?;
    if raw.is_empty() {
        None
    } else {
        Some(PathBuf::from(raw))
    }
}

fn git_top_level(cwd: &Path) -> Option<PathBuf> {
    git_rev_parse_path(cwd, "--show-toplevel").ok().flatten()
}

#[derive(Debug, Default)]
struct LinkedWorktreeMetadata {
    is_linked_worktree: bool,
    git_common_dir: Option<PathBuf>,
    primary_worktree_path: Option<PathBuf>,
}

fn resolve_linked_worktree_metadata(project: &Path) -> Result<LinkedWorktreeMetadata> {
    let Some(top_level) = git_rev_parse_path(project, "--show-toplevel")? else {
        return Ok(LinkedWorktreeMetadata::default());
    };
    let top_level = std::fs::canonicalize(&top_level).with_context(|| {
        format!(
            "failed to canonicalize Git top-level {}",
            top_level.display()
        )
    })?;
    if top_level != project {
        return Ok(LinkedWorktreeMetadata::default());
    }

    let Some(absolute_git_dir) = git_rev_parse_path(project, "--absolute-git-dir")? else {
        bail!("Git repository omitted its absolute Git directory");
    };
    let Some(git_common_dir) = git_rev_parse_path(project, "--git-common-dir")? else {
        bail!("Git repository omitted its common directory");
    };
    let absolute_git_dir = std::fs::canonicalize(&absolute_git_dir).with_context(|| {
        format!(
            "failed to canonicalize Git directory {}",
            absolute_git_dir.display()
        )
    })?;
    let git_common_dir = std::fs::canonicalize(&git_common_dir).with_context(|| {
        format!(
            "failed to canonicalize Git common dir {}",
            git_common_dir.display()
        )
    })?;
    let worktrees = git_worktree_roots(project)?;
    if !worktrees.iter().any(|root| root == project) {
        bail!(
            "Git common dir {} does not list project worktree {}",
            git_common_dir.display(),
            project.display()
        );
    }

    let is_linked_worktree = absolute_git_dir != git_common_dir;
    let primary_worktree_path = if is_linked_worktree {
        git_common_dir.parent().map(Path::to_path_buf)
    } else {
        None
    };

    Ok(LinkedWorktreeMetadata {
        is_linked_worktree,
        git_common_dir: Some(git_common_dir),
        primary_worktree_path,
    })
}

fn git_rev_parse_path(cwd: &Path, arg: &str) -> Result<Option<PathBuf>> {
    let output = run_git_output_sanitized(cwd, &["rev-parse", arg])
        .with_context(|| format!("failed to execute git rev-parse {arg}"))?;
    if !output.status.success() {
        return Ok(None);
    }
    let raw = trim_ascii_whitespace(&output.stdout);
    if raw.is_empty() {
        return Ok(None);
    }
    let path = path_from_git_bytes(raw)?;
    Ok(Some(normalize_root_path(&path, cwd)))
}

pub(crate) fn git_worktree_roots(project: &Path) -> Result<Vec<PathBuf>> {
    let output = run_git_output_sanitized(project, &["worktree", "list", "--porcelain", "-z"])
        .context("failed to list Git worktrees")?;
    if !output.status.success() {
        bail!(
            "git worktree list failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    let mut roots = Vec::new();
    for field in output.stdout.split(|byte| *byte == 0) {
        let Some(raw) = field.strip_prefix(b"worktree ") else {
            continue;
        };
        let path = path_from_git_bytes(raw)?;
        roots.push(
            std::fs::canonicalize(&path).with_context(|| {
                format!("failed to canonicalize Git worktree {}", path.display())
            })?,
        );
    }
    if roots.is_empty() {
        bail!("git worktree list returned no worktrees");
    }
    Ok(roots)
}

pub(crate) fn run_git_output_sanitized(cwd: &Path, args: &[&str]) -> io::Result<Output> {
    let mut command = Command::new("git");
    command.arg("-C").arg(cwd).args(args);
    for name in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_PREFIX",
        "GIT_CEILING_DIRECTORIES",
    ] {
        command.env_remove(name);
    }
    command.output()
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
