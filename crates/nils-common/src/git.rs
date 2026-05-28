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
}
