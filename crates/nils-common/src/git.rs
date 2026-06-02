use crate::process;
use std::collections::BTreeSet;
use std::error::Error;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Output};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitContextError {
    GitNotFound,
    NotRepository,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameStatusParseError {
    MalformedOutput,
}

impl fmt::Display for NameStatusParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NameStatusParseError::MalformedOutput => {
                write!(f, "error: malformed name-status output")
            }
        }
    }
}

impl Error for NameStatusParseError {}

/// Conventional-commit-style PR / MR kind shared by `forge-cli`
/// (`pr deliver` / `pr create --kind` and the `branch_kind` preflight) and
/// `git-cli` (`worktree add --kind` branch-prefix derivation). The kind set
/// and its branch-prefix mapping live here so the two tools cannot disagree
/// about which branch a given kind expects: a worktree opened for one kind
/// then delivers cleanly under the same kind. The set tracks the
/// Conventional Commits type whitelist (`feature`, `bug`, `chore`, `docs`,
/// `ci`, `refactor`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrKind {
    Feature,
    Bug,
    Chore,
    Docs,
    Ci,
    Refactor,
}

impl PrKind {
    /// Render the kind to the lower-case enum literal used in `--kind` argv
    /// and JSON envelopes.
    pub fn as_str(self) -> &'static str {
        match self {
            PrKind::Feature => "feature",
            PrKind::Bug => "bug",
            PrKind::Chore => "chore",
            PrKind::Docs => "docs",
            PrKind::Ci => "ci",
            PrKind::Refactor => "refactor",
        }
    }

    /// The branch-name prefix this kind requires, without the trailing `/`.
    /// `feature -> feat` and `bug -> fix`; every other kind maps to its own
    /// literal. This is the single source of truth for the mapping
    /// `forge-cli`'s `branch_kind` rule enforces.
    pub fn branch_prefix(self) -> &'static str {
        match self {
            PrKind::Feature => "feat",
            PrKind::Bug => "fix",
            PrKind::Chore => "chore",
            PrKind::Docs => "docs",
            PrKind::Ci => "ci",
            PrKind::Refactor => "refactor",
        }
    }

    /// Parse the lower-case `--kind` flag value. Returns `None` for anything
    /// outside the spec's enum so each caller can emit its own usage error.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "feature" => Some(PrKind::Feature),
            "bug" => Some(PrKind::Bug),
            "chore" => Some(PrKind::Chore),
            "docs" => Some(PrKind::Docs),
            "ci" => Some(PrKind::Ci),
            "refactor" => Some(PrKind::Refactor),
            _ => None,
        }
    }

    /// Every kind in declaration order, for help text and exhaustive tests.
    pub fn all() -> [PrKind; 6] {
        [
            PrKind::Feature,
            PrKind::Bug,
            PrKind::Chore,
            PrKind::Docs,
            PrKind::Ci,
            PrKind::Refactor,
        ]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NameStatusZEntry<'a> {
    pub status_raw: &'a [u8],
    pub path: &'a [u8],
    pub old_path: Option<&'a [u8]>,
}

pub fn parse_name_status_z(buf: &[u8]) -> Result<Vec<NameStatusZEntry<'_>>, NameStatusParseError> {
    let parts: Vec<&[u8]> = buf
        .split(|b| *b == 0)
        .filter(|part| !part.is_empty())
        .collect();
    let mut out: Vec<NameStatusZEntry<'_>> = Vec::new();
    let mut i = 0;

    while i < parts.len() {
        let status_raw = parts[i];
        i += 1;

        if matches!(status_raw.first(), Some(b'R' | b'C')) {
            let old = *parts.get(i).ok_or(NameStatusParseError::MalformedOutput)?;
            let new = *parts
                .get(i + 1)
                .ok_or(NameStatusParseError::MalformedOutput)?;
            i += 2;
            out.push(NameStatusZEntry {
                status_raw,
                path: new,
                old_path: Some(old),
            });
        } else {
            let file = *parts.get(i).ok_or(NameStatusParseError::MalformedOutput)?;
            i += 1;
            out.push(NameStatusZEntry {
                status_raw,
                path: file,
                old_path: None,
            });
        }
    }

    Ok(out)
}

pub fn is_lockfile_path(path: &str) -> bool {
    let name = Path::new(path)
        .file_name()
        .and_then(|segment| segment.to_str())
        .unwrap_or("");
    matches!(
        name,
        "yarn.lock"
            | "package-lock.json"
            | "pnpm-lock.yaml"
            | "bun.lockb"
            | "bun.lock"
            | "npm-shrinkwrap.json"
    )
}

pub fn trim_trailing_newlines(input: &str) -> String {
    input.trim_end_matches(['\n', '\r']).to_string()
}

/// Return the substring after the last `@`, or the input unchanged when no
/// `@` is present. Used by git remote URL parsers to drop the `userinfo@`
/// prefix from a host segment (or a host+path string, since `/` cannot appear
/// inside userinfo, splitting at the last `@` is safe for both shapes).
pub fn strip_userinfo(host: &str) -> &str {
    host.rsplit_once('@').map(|(_, tail)| tail).unwrap_or(host)
}

/// Host + path split out of a parsed git remote URL.
///
/// `host` carries the bare hostname (port and userinfo removed). `path` carries
/// the URL path with leading/trailing slashes and a single optional trailing
/// `.git` removed; callers split it into owner/repo or group/project segments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitRemoteUrl {
    pub host: String,
    pub path: String,
}

/// Parse a git remote URL into a `host`/`path` pair, accepting the four shapes
/// git itself accepts:
///
/// - `git@<host>:<path>` (SCP-style; `<path>` carries the slashes)
/// - `ssh://[userinfo@]<host>[:<port>]/<path>`
/// - `https://[userinfo@]<host>[:<port>]/<path>`
/// - `http://[userinfo@]<host>[:<port>]/<path>`
///
/// Userinfo (`user@`, `user:pass@`, …) is stripped, ports are stripped from
/// `host`, surrounding slashes and a single trailing `.git` are trimmed from
/// `path`. Returns `None` for unknown schemes, empty hosts, or empty paths.
pub fn parse_git_remote_url(url: &str) -> Option<GitRemoteUrl> {
    let trimmed = url.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return None;
    }

    // SCP-style: [user@]host:path (host never contains `/`)
    if !trimmed.contains("://")
        && let Some((host_with_user, path)) = trimmed.split_once(':')
        && !host_with_user.contains('/')
        && !path.contains("://")
    {
        let host = strip_userinfo(host_with_user);
        return finalize(host, path);
    }

    // ssh://[userinfo@]host[:port]/path
    if let Some(rest) = trimmed.strip_prefix("ssh://") {
        let after_user = strip_userinfo(rest);
        let (host_port, path) = after_user.split_once('/')?;
        let host = host_port
            .split_once(':')
            .map(|(h, _)| h)
            .unwrap_or(host_port);
        return finalize(host, path);
    }

    // https:// / http:// [userinfo@]host[:port]/path
    for prefix in ["https://", "http://"] {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            let (host_with_user, path) = rest.split_once('/')?;
            let host_no_user = strip_userinfo(host_with_user);
            let host = host_no_user
                .split_once(':')
                .map(|(h, _)| h)
                .unwrap_or(host_no_user);
            return finalize(host, path);
        }
    }

    None
}

fn finalize(host: &str, path: &str) -> Option<GitRemoteUrl> {
    let host = host.trim();
    let path = path.trim_matches('/').trim_end_matches(".git");
    if host.is_empty() || path.is_empty() {
        return None;
    }
    Some(GitRemoteUrl {
        host: host.to_string(),
        path: path.to_string(),
    })
}

pub fn staged_name_only() -> io::Result<String> {
    staged_name_only_inner(None)
}

pub fn staged_name_only_in(cwd: &Path) -> io::Result<String> {
    staged_name_only_inner(Some(cwd))
}

pub fn suggested_scope_from_staged_paths(staged: &str) -> String {
    let mut top: BTreeSet<String> = BTreeSet::new();
    for line in staged.lines() {
        let file = line.trim();
        if file.is_empty() {
            continue;
        }
        if let Some((first, _rest)) = file.split_once('/') {
            top.insert(first.to_string());
        } else {
            top.insert(String::new());
        }
    }

    if top.len() == 1 {
        return top.iter().next().cloned().unwrap_or_default();
    }

    if top.len() == 2 && top.contains("") {
        for part in top {
            if !part.is_empty() {
                return part;
            }
        }
    }

    String::new()
}

pub fn run_output(args: &[&str]) -> io::Result<Output> {
    run_output_inner(None, args, &[])
}

pub fn run_output_in(cwd: &Path, args: &[&str]) -> io::Result<Output> {
    run_output_inner(Some(cwd), args, &[])
}

pub fn run_output_with_env(
    args: &[&str],
    env: &[process::ProcessEnvPair<'_>],
) -> io::Result<Output> {
    run_output_inner(None, args, env)
}

pub fn run_output_in_with_env(
    cwd: &Path,
    args: &[&str],
    env: &[process::ProcessEnvPair<'_>],
) -> io::Result<Output> {
    run_output_inner(Some(cwd), args, env)
}

pub fn run_status_quiet(args: &[&str]) -> io::Result<ExitStatus> {
    run_status_quiet_inner(None, args, &[])
}

pub fn run_status_quiet_in(cwd: &Path, args: &[&str]) -> io::Result<ExitStatus> {
    run_status_quiet_inner(Some(cwd), args, &[])
}

pub fn run_status_inherit(args: &[&str]) -> io::Result<ExitStatus> {
    run_status_inherit_inner(None, args, &[])
}

pub fn run_status_inherit_in(cwd: &Path, args: &[&str]) -> io::Result<ExitStatus> {
    run_status_inherit_inner(Some(cwd), args, &[])
}

pub fn run_status_inherit_with_env(
    args: &[&str],
    env: &[process::ProcessEnvPair<'_>],
) -> io::Result<ExitStatus> {
    run_status_inherit_inner(None, args, env)
}

pub fn run_status_inherit_in_with_env(
    cwd: &Path,
    args: &[&str],
    env: &[process::ProcessEnvPair<'_>],
) -> io::Result<ExitStatus> {
    run_status_inherit_inner(Some(cwd), args, env)
}

pub fn is_git_available() -> bool {
    run_status_quiet(&["--version"])
        .map(|status| status.success())
        .unwrap_or(false)
}

pub fn require_repo() -> Result<(), GitContextError> {
    require_context(None, &["rev-parse", "--git-dir"])
}

pub fn require_repo_in(cwd: &Path) -> Result<(), GitContextError> {
    require_context(Some(cwd), &["rev-parse", "--git-dir"])
}

pub fn require_work_tree() -> Result<(), GitContextError> {
    require_context(None, &["rev-parse", "--is-inside-work-tree"])
}

pub fn require_work_tree_in(cwd: &Path) -> Result<(), GitContextError> {
    require_context(Some(cwd), &["rev-parse", "--is-inside-work-tree"])
}

pub fn is_inside_work_tree() -> io::Result<bool> {
    Ok(run_status_quiet(&["rev-parse", "--is-inside-work-tree"])?.success())
}

pub fn is_inside_work_tree_in(cwd: &Path) -> io::Result<bool> {
    Ok(run_status_quiet_in(cwd, &["rev-parse", "--is-inside-work-tree"])?.success())
}

pub fn has_staged_changes() -> io::Result<bool> {
    let status = run_status_quiet(&["diff", "--cached", "--quiet", "--"])?;
    Ok(has_staged_changes_from_status(status))
}

pub fn has_staged_changes_in(cwd: &Path) -> io::Result<bool> {
    let status = run_status_quiet_in(cwd, &["diff", "--cached", "--quiet", "--"])?;
    Ok(has_staged_changes_from_status(status))
}

pub fn is_git_repo() -> io::Result<bool> {
    Ok(run_status_quiet(&["rev-parse", "--git-dir"])?.success())
}

pub fn is_git_repo_in(cwd: &Path) -> io::Result<bool> {
    Ok(run_status_quiet_in(cwd, &["rev-parse", "--git-dir"])?.success())
}

pub fn repo_root() -> io::Result<Option<PathBuf>> {
    let output = run_output(&["rev-parse", "--show-toplevel"])?;
    Ok(trimmed_stdout_if_success(&output).map(PathBuf::from))
}

pub fn repo_root_in(cwd: &Path) -> io::Result<Option<PathBuf>> {
    let output = run_output_in(cwd, &["rev-parse", "--show-toplevel"])?;
    Ok(trimmed_stdout_if_success(&output).map(PathBuf::from))
}

pub fn repo_root_or_cwd() -> PathBuf {
    repo_root()
        .ok()
        .flatten()
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn rev_parse(args: &[&str]) -> io::Result<Option<String>> {
    let output = run_output(&rev_parse_args(args))?;
    Ok(trimmed_stdout_if_success(&output))
}

pub fn rev_parse_in(cwd: &Path, args: &[&str]) -> io::Result<Option<String>> {
    let output = run_output_in(cwd, &rev_parse_args(args))?;
    Ok(trimmed_stdout_if_success(&output))
}

fn run_output_inner(
    cwd: Option<&Path>,
    args: &[&str],
    env: &[process::ProcessEnvPair<'_>],
) -> io::Result<Output> {
    process::run_output_with("git", args, cwd, env).map(|output| output.into_std_output())
}

fn run_status_quiet_inner(
    cwd: Option<&Path>,
    args: &[&str],
    env: &[process::ProcessEnvPair<'_>],
) -> io::Result<ExitStatus> {
    process::run_status_quiet_with("git", args, cwd, env)
}

fn run_status_inherit_inner(
    cwd: Option<&Path>,
    args: &[&str],
    env: &[process::ProcessEnvPair<'_>],
) -> io::Result<ExitStatus> {
    process::run_status_inherit_with("git", args, cwd, env)
}

fn require_context(cwd: Option<&Path>, probe_args: &[&str]) -> Result<(), GitContextError> {
    if !is_git_available() {
        return Err(GitContextError::GitNotFound);
    }

    let in_context = match cwd {
        Some(cwd) => run_status_quiet_in(cwd, probe_args),
        None => run_status_quiet(probe_args),
    }
    .map(|status| status.success())
    .unwrap_or(false);

    if in_context {
        Ok(())
    } else {
        Err(GitContextError::NotRepository)
    }
}

fn rev_parse_args<'a>(args: &'a [&'a str]) -> Vec<&'a str> {
    let mut full = Vec::with_capacity(args.len() + 1);
    full.push("rev-parse");
    full.extend_from_slice(args);
    full
}

fn staged_name_only_inner(cwd: Option<&Path>) -> io::Result<String> {
    let args = [
        "-c",
        "core.quotepath=false",
        "diff",
        "--cached",
        "--name-only",
        "--diff-filter=ACMRTUXBD",
    ];
    let output = match cwd {
        Some(cwd) => run_output_in(cwd, &args)?,
        None => run_output(&args)?,
    };
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn has_staged_changes_from_status(status: ExitStatus) -> bool {
    match status.code() {
        Some(0) => false,
        Some(1) => true,
        _ => !status.success(),
    }
}

fn trimmed_stdout_if_success(output: &Output) -> Option<String> {
    if !output.status.success() {
        return None;
    }

    let trimmed = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nils_test_support::git::{InitRepoOptions, git as run_git, init_repo_with};
    use nils_test_support::{CwdGuard, EnvGuard, GlobalStateLock};
    use pretty_assertions::assert_eq;
    use tempfile::TempDir;

    #[test]
    fn run_output_in_preserves_nonzero_status() {
        let repo = init_repo_with(InitRepoOptions::new());

        let output = run_output_in(repo.path(), &["rev-parse", "--verify", "HEAD"])
            .expect("run output in repo");

        assert!(!output.status.success());
        assert!(!output.stderr.is_empty());
    }

    #[test]
    fn pr_kind_branch_prefix_mapping_is_canonical() {
        assert_eq!(PrKind::Feature.branch_prefix(), "feat");
        assert_eq!(PrKind::Bug.branch_prefix(), "fix");
        assert_eq!(PrKind::Chore.branch_prefix(), "chore");
        assert_eq!(PrKind::Docs.branch_prefix(), "docs");
        assert_eq!(PrKind::Ci.branch_prefix(), "ci");
        assert_eq!(PrKind::Refactor.branch_prefix(), "refactor");
    }

    #[test]
    fn pr_kind_parse_round_trips_every_kind_and_rejects_unknown() {
        for kind in PrKind::all() {
            assert_eq!(PrKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(PrKind::parse("nope"), None);
        assert_eq!(PrKind::parse("feat"), None);
    }

    #[test]
    fn run_status_quiet_in_returns_success_and_failure_statuses() {
        let repo = init_repo_with(InitRepoOptions::new());

        let ok =
            run_status_quiet_in(repo.path(), &["rev-parse", "--git-dir"]).expect("status success");
        let bad = run_status_quiet_in(repo.path(), &["rev-parse", "--verify", "HEAD"])
            .expect("status failure");

        assert!(ok.success());
        assert!(!bad.success());
    }

    #[test]
    fn run_output_with_env_passes_environment_variables_to_git() {
        let output = run_output_with_env(
            &["config", "--get", "nils.test-env"],
            &[
                ("GIT_CONFIG_COUNT", "1"),
                ("GIT_CONFIG_KEY_0", "nils.test-env"),
                ("GIT_CONFIG_VALUE_0", "ready"),
            ],
        )
        .expect("run git output with env");

        assert!(output.status.success());
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "ready");
    }

    #[test]
    fn run_status_inherit_in_with_env_applies_cwd_and_environment() {
        let repo = init_repo_with(InitRepoOptions::new().with_initial_commit());
        let status = run_status_inherit_in_with_env(
            repo.path(),
            &["config", "--get", "nils.test-status"],
            &[
                ("GIT_CONFIG_COUNT", "1"),
                ("GIT_CONFIG_KEY_0", "nils.test-status"),
                ("GIT_CONFIG_VALUE_0", "ok"),
            ],
        )
        .expect("run git status in with env");

        assert!(status.success());
    }

    #[test]
    fn is_git_repo_in_and_is_inside_work_tree_in_match_repo_context() {
        let repo = init_repo_with(InitRepoOptions::new());
        let outside = TempDir::new().expect("tempdir");

        assert!(is_git_repo_in(repo.path()).expect("is_git_repo in repo"));
        assert!(is_inside_work_tree_in(repo.path()).expect("is_inside_work_tree in repo"));
        assert!(!is_git_repo_in(outside.path()).expect("is_git_repo outside repo"));
        assert!(!is_inside_work_tree_in(outside.path()).expect("is_inside_work_tree outside repo"));
    }

    #[test]
    fn repo_root_in_returns_root_or_none() {
        let repo = init_repo_with(InitRepoOptions::new());
        let outside = TempDir::new().expect("tempdir");
        let expected_root = run_git(repo.path(), &["rev-parse", "--show-toplevel"])
            .trim()
            .to_string();

        assert_eq!(
            repo_root_in(repo.path()).expect("repo_root_in repo"),
            Some(expected_root.into())
        );
        assert_eq!(
            repo_root_in(outside.path()).expect("repo_root_in outside"),
            None
        );
    }

    #[test]
    fn rev_parse_in_returns_value_or_none() {
        let repo = init_repo_with(InitRepoOptions::new().with_initial_commit());
        let head = run_git(repo.path(), &["rev-parse", "HEAD"])
            .trim()
            .to_string();

        assert_eq!(
            rev_parse_in(repo.path(), &["HEAD"]).expect("rev_parse head"),
            Some(head)
        );
        assert_eq!(
            rev_parse_in(repo.path(), &["--verify", "refs/heads/does-not-exist"])
                .expect("rev_parse missing ref"),
            None
        );
    }

    #[test]
    fn cwd_wrappers_delegate_to_in_variants() {
        let lock = GlobalStateLock::new();
        let repo = init_repo_with(InitRepoOptions::new().with_initial_commit());
        let _cwd = CwdGuard::set(&lock, repo.path()).expect("set cwd");
        let head = run_git(repo.path(), &["rev-parse", "HEAD"])
            .trim()
            .to_string();
        let root = run_git(repo.path(), &["rev-parse", "--show-toplevel"])
            .trim()
            .to_string();

        assert!(is_git_repo().expect("is_git_repo"));
        assert!(is_inside_work_tree().expect("is_inside_work_tree"));
        assert!(!has_staged_changes().expect("has_staged_changes"));
        assert_eq!(require_repo(), Ok(()));
        assert_eq!(require_work_tree(), Ok(()));
        assert_eq!(repo_root().expect("repo_root"), Some(root.into()));
        assert_eq!(rev_parse(&["HEAD"]).expect("rev_parse"), Some(head));
    }

    #[test]
    fn has_staged_changes_in_reports_index_state() {
        let repo = init_repo_with(InitRepoOptions::new().with_initial_commit());

        assert!(!has_staged_changes_in(repo.path()).expect("no staged changes"));

        std::fs::write(repo.path().join("a.txt"), "hello\n").expect("write staged file");
        run_git(repo.path(), &["add", "a.txt"]);

        assert!(has_staged_changes_in(repo.path()).expect("staged changes present"));
    }

    #[test]
    fn repo_root_or_cwd_prefers_repo_root_when_available() {
        let lock = GlobalStateLock::new();
        let repo = init_repo_with(InitRepoOptions::new().with_initial_commit());
        let _cwd = CwdGuard::set(&lock, repo.path()).expect("set cwd");
        let expected_root = run_git(repo.path(), &["rev-parse", "--show-toplevel"])
            .trim()
            .to_string();

        assert_eq!(repo_root_or_cwd(), PathBuf::from(expected_root));
    }

    #[test]
    fn repo_root_or_cwd_falls_back_to_current_dir_outside_repo() {
        let lock = GlobalStateLock::new();
        let outside = TempDir::new().expect("tempdir");
        let _cwd = CwdGuard::set(&lock, outside.path()).expect("set cwd");

        let resolved = repo_root_or_cwd()
            .canonicalize()
            .expect("canonicalize resolved path");
        let expected = outside
            .path()
            .canonicalize()
            .expect("canonicalize expected path");

        assert_eq!(resolved, expected);
    }

    #[test]
    fn require_work_tree_in_reports_missing_git_or_repo_state() {
        let lock = GlobalStateLock::new();
        let outside = TempDir::new().expect("tempdir");
        let empty = TempDir::new().expect("tempdir");
        let _path = EnvGuard::set(&lock, "PATH", &empty.path().to_string_lossy());

        assert_eq!(
            require_work_tree_in(outside.path()),
            Err(GitContextError::GitNotFound)
        );
    }

    #[test]
    fn require_repo_and_work_tree_in_report_context_readiness() {
        let repo = init_repo_with(InitRepoOptions::new());
        let outside = TempDir::new().expect("tempdir");

        assert_eq!(require_repo_in(repo.path()), Ok(()));
        assert_eq!(require_work_tree_in(repo.path()), Ok(()));
        assert_eq!(
            require_repo_in(outside.path()),
            Err(GitContextError::NotRepository)
        );
        assert_eq!(
            require_work_tree_in(outside.path()),
            Err(GitContextError::NotRepository)
        );
    }

    #[test]
    fn parse_name_status_z_handles_rename_copy_and_modify() {
        let bytes = b"R100\0old.txt\0new.txt\0C90\0src.rs\0dst.rs\0M\0file.txt\0";
        let entries = parse_name_status_z(bytes).expect("parse name-status");

        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].status_raw, b"R100");
        assert_eq!(entries[0].path, b"new.txt");
        assert_eq!(entries[0].old_path, Some(&b"old.txt"[..]));
        assert_eq!(entries[1].status_raw, b"C90");
        assert_eq!(entries[1].path, b"dst.rs");
        assert_eq!(entries[1].old_path, Some(&b"src.rs"[..]));
        assert_eq!(entries[2].status_raw, b"M");
        assert_eq!(entries[2].path, b"file.txt");
        assert_eq!(entries[2].old_path, None);
    }

    #[test]
    fn parse_name_status_z_errors_on_malformed_output() {
        let err = parse_name_status_z(b"R100\0old.txt\0").expect_err("expected parse error");
        assert_eq!(err, NameStatusParseError::MalformedOutput);
        assert_eq!(err.to_string(), "error: malformed name-status output");
    }

    #[test]
    fn is_lockfile_path_matches_known_package_manager_lockfiles() {
        for path in [
            "yarn.lock",
            "frontend/package-lock.json",
            "subdir/pnpm-lock.yaml",
            "bun.lockb",
            "bun.lock",
            "npm-shrinkwrap.json",
        ] {
            assert!(is_lockfile_path(path), "expected {path} to be a lockfile");
        }

        assert!(!is_lockfile_path("Cargo.lock"));
        assert!(!is_lockfile_path("package-lock.json.bak"));
    }

    #[test]
    fn trim_trailing_newlines_drops_lf_and_crlf_suffixes() {
        assert_eq!(trim_trailing_newlines("value\n"), "value");
        assert_eq!(trim_trailing_newlines("value\r\n"), "value");
        assert_eq!(trim_trailing_newlines("value"), "value");
    }

    #[test]
    fn suggested_scope_from_staged_paths_matches_single_top_level_dir() {
        let staged = "src/main.rs\nsrc/lib.rs\n";
        assert_eq!(suggested_scope_from_staged_paths(staged), "src");
    }

    #[test]
    fn suggested_scope_from_staged_paths_ignores_root_file_when_single_dir_exists() {
        let staged = "README.md\nsrc/main.rs\n";
        assert_eq!(suggested_scope_from_staged_paths(staged), "src");
    }

    #[test]
    fn suggested_scope_from_staged_paths_returns_empty_when_multiple_dirs_exist() {
        let staged = "src/main.rs\ncrates/a.rs\n";
        assert_eq!(suggested_scope_from_staged_paths(staged), "");
    }

    #[test]
    fn staged_name_only_in_lists_cached_paths() {
        let repo = init_repo_with(InitRepoOptions::new().with_initial_commit());
        std::fs::write(repo.path().join("src.txt"), "hi\n").expect("write file");
        run_git(repo.path(), &["add", "src.txt"]);

        let staged = staged_name_only_in(repo.path()).expect("staged names");
        assert!(staged.contains("src.txt"));
    }

    #[test]
    fn staged_name_only_wrapper_uses_current_working_repo() {
        let lock = GlobalStateLock::new();
        let repo = init_repo_with(InitRepoOptions::new().with_initial_commit());
        std::fs::write(repo.path().join("docs.md"), "hello\n").expect("write file");
        run_git(repo.path(), &["add", "docs.md"]);
        let _cwd = CwdGuard::set(&lock, repo.path()).expect("set cwd");

        let staged = staged_name_only().expect("staged names");
        assert!(staged.contains("docs.md"));
    }

    #[test]
    fn strip_userinfo_passthrough_when_no_at() {
        assert_eq!(strip_userinfo("github.com"), "github.com");
        assert_eq!(
            strip_userinfo("gitlab.example.com:2222"),
            "gitlab.example.com:2222"
        );
        assert_eq!(strip_userinfo(""), "");
    }

    #[test]
    fn strip_userinfo_drops_user_only_prefix() {
        assert_eq!(strip_userinfo("git@github.com"), "github.com");
        assert_eq!(strip_userinfo("x-access-token@gitlab.com"), "gitlab.com");
    }

    #[test]
    fn strip_userinfo_drops_user_password_prefix() {
        assert_eq!(strip_userinfo("user:pass@github.com"), "github.com");
        assert_eq!(strip_userinfo("user:p@ss@gitlab.com"), "gitlab.com");
    }

    #[test]
    fn parse_git_remote_url_handles_scp_form() {
        let r = parse_git_remote_url("git@github.com:sympoies/nils-cli.git").expect("scp");
        assert_eq!(r.host, "github.com");
        assert_eq!(r.path, "sympoies/nils-cli");
    }

    #[test]
    fn parse_git_remote_url_handles_scp_form_nested_gitlab_group() {
        let r = parse_git_remote_url("git@gitlab.example.com:acme/platform/backend/ingest.git")
            .expect("scp nested");
        assert_eq!(r.host, "gitlab.example.com");
        assert_eq!(r.path, "acme/platform/backend/ingest");
    }

    #[test]
    fn parse_git_remote_url_handles_ssh_with_userinfo_and_port() {
        let r = parse_git_remote_url("ssh://deploy@gitlab.example.com:2222/group/proj.git")
            .expect("ssh");
        assert_eq!(r.host, "gitlab.example.com");
        assert_eq!(r.path, "group/proj");
    }

    #[test]
    fn parse_git_remote_url_handles_https_with_basic_auth() {
        let r = parse_git_remote_url("https://user:pass@github.com/sympoies/nils-cli.git")
            .expect("https");
        assert_eq!(r.host, "github.com");
        assert_eq!(r.path, "sympoies/nils-cli");
    }

    #[test]
    fn parse_git_remote_url_handles_https_with_port() {
        let r = parse_git_remote_url("https://gitlab.example.com:8443/group/proj").expect("port");
        assert_eq!(r.host, "gitlab.example.com");
        assert_eq!(r.path, "group/proj");
    }

    #[test]
    fn parse_git_remote_url_handles_http() {
        let r = parse_git_remote_url("http://gitlab.example.com/group/proj.git").expect("http");
        assert_eq!(r.host, "gitlab.example.com");
        assert_eq!(r.path, "group/proj");
    }

    #[test]
    fn parse_git_remote_url_rejects_unknown_schemes_and_empty() {
        assert!(parse_git_remote_url("").is_none());
        assert!(parse_git_remote_url("file:///tmp/x.git").is_none());
        assert!(parse_git_remote_url("ftp://host/path").is_none());
    }

    #[test]
    fn parse_git_remote_url_rejects_empty_host_or_path() {
        assert!(parse_git_remote_url("https://user:pass@/owner/repo").is_none());
        assert!(parse_git_remote_url("ssh://deploy@/owner/repo").is_none());
        assert!(parse_git_remote_url("https://github.com/").is_none());
    }
}
