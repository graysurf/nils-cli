use std::fs;
use std::path::{Path, PathBuf};

use nils_common::execution_effect::digest_parts;

use super::git::Git;
use super::receipt::verify_destination;

#[derive(Clone, Debug)]
pub(super) struct RemoteState {
    pub(super) configured_count: usize,
    pub(super) mode: &'static str,
    pub(super) upstream: Option<String>,
    pub(super) upstream_sha: Option<String>,
    pub(super) remote_name: Option<String>,
}

#[derive(Debug)]
pub(super) struct RepositoryState {
    pub(super) root: PathBuf,
    pub(super) default_branch: String,
    pub(super) head: String,
    pub(super) staged_file_count: usize,
    pub(super) fingerprint: String,
    pub(super) remote: RemoteState,
}

pub(super) fn inspect(
    repo: Option<&Path>,
    expect_head: &str,
    receipt_out: Option<&Path>,
) -> Result<RepositoryState, String> {
    if !valid_object_id(expect_head) {
        return Err("--expect-head must be a full lowercase object ID".to_string());
    }
    let start = match repo {
        Some(path) if !path.is_absolute() => {
            return Err("--repo must be an absolute repository path".to_string());
        }
        Some(path) => path.to_path_buf(),
        None => std::env::current_dir().map_err(|error| error.to_string())?,
    };
    let git = Git::discover(&start)?;
    if git.stdout(["rev-parse", "--is-bare-repository"])? != "false" {
        return Err("default-branch requires a non-bare repository".to_string());
    }
    verify_primary_worktree(&git)?;
    let default_branch = git
        .stdout(["symbolic-ref", "--quiet", "--short", "HEAD"])
        .map_err(|_| "default-branch requires an attached HEAD".to_string())?;
    git.run(["check-ref-format", "--branch", default_branch.as_str()])?;
    let head = git.stdout(["rev-parse", "--verify", "HEAD^{commit}"])?;
    if head != expect_head {
        return Err(format!(
            "HEAD mismatch (expected {expect_head}, found {head})"
        ));
    }
    verify_no_git_operation(&git)?;
    let staged = git.run([
        "diff",
        "--cached",
        "--name-only",
        "-z",
        "--diff-filter=ACDMRTUXB",
    ])?;
    let staged_file_count = staged
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .count();
    if staged_file_count == 0 {
        return Err("no staged changes (stage files with git add first)".to_string());
    }
    let status = git.stdout_allow_empty(["status", "--porcelain", "--untracked-files=all"])?;
    if status.lines().any(|line| {
        line.starts_with("??") || line.as_bytes().get(1).is_some_and(|byte| *byte != b' ')
    }) {
        return Err("unstaged or untracked changes present".to_string());
    }
    if let Some(receipt_out) = receipt_out {
        verify_destination(git.root(), receipt_out)?;
    }

    let remote = resolve_remote_state(&git, &default_branch, &head)?;
    let common = git.stdout(["rev-parse", "--git-common-dir"])?;
    let common = canonical_git_path(git.root(), &common)?;
    let object_format = git.stdout(["rev-parse", "--show-object-format"])?;
    let fingerprint = digest_parts([
        common.as_os_str().as_encoded_bytes(),
        object_format.as_bytes(),
    ]);
    Ok(RepositoryState {
        root: git.root().to_path_buf(),
        default_branch,
        head,
        staged_file_count,
        fingerprint,
        remote,
    })
}

pub(super) fn verify_identity_after(state: &RepositoryState) -> Result<(), String> {
    let git = Git::at(state.root.clone());
    verify_primary_worktree(&git)?;
    let branch = git.stdout(["symbolic-ref", "--quiet", "--short", "HEAD"])?;
    if branch != state.default_branch {
        return Err("default branch changed after commit".to_string());
    }
    let current_remote = resolve_remote_identity(&git, &branch)?;
    if current_remote.configured_count != state.remote.configured_count
        || current_remote.upstream != state.remote.upstream
        || current_remote.upstream_sha != state.remote.upstream_sha
        || current_remote.remote_name != state.remote.remote_name
    {
        return Err("cached default-branch identity changed after commit".to_string());
    }
    Ok(())
}

fn resolve_remote_state(git: &Git, branch: &str, head: &str) -> Result<RemoteState, String> {
    let remote = resolve_remote_identity(git, branch)?;
    if let Some(upstream_sha) = remote.upstream_sha.as_deref()
        && upstream_sha != head
    {
        let (behind, ahead) = cached_relation_counts(git, upstream_sha, head)?;
        let relation = match (behind > 0, ahead > 0) {
            (true, true) => "diverged",
            (true, false) => "behind",
            (false, true) => "already-ahead",
            (false, false) => "ambiguous",
        };
        return Err(format!(
            "current HEAD is {relation} relative to the cached default-branch upstream"
        ));
    }
    Ok(remote)
}

fn resolve_remote_identity(git: &Git, branch: &str) -> Result<RemoteState, String> {
    let remotes = git.stdout_allow_empty(["remote"])?;
    let configured_count = remotes
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count();
    let branch_remote = git.config_optional(&format!("branch.{branch}.remote"))?;
    let branch_merge = git.config_optional(&format!("branch.{branch}.merge"))?;
    if configured_count == 0 {
        if branch_remote.is_some() || branch_merge.is_some() {
            return Err(
                "remote-free default branch must not have configured upstream metadata".to_string(),
            );
        }
        return Ok(RemoteState {
            configured_count,
            mode: "remote-free",
            upstream: None,
            upstream_sha: None,
            remote_name: None,
        });
    }

    let remote_name = branch_remote
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            "configured remotes require authoritative cached default/upstream state".to_string()
        })?;
    if remote_name == "." || !remotes.lines().any(|remote| remote == remote_name) {
        return Err("configured upstream remote is not an authoritative named remote".to_string());
    }
    let merge_ref = branch_merge
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            "configured remotes require authoritative cached default/upstream state".to_string()
        })?;
    let upstream_branch = merge_ref
        .strip_prefix("refs/heads/")
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "configured upstream merge ref is invalid".to_string())?;
    if upstream_branch != branch {
        return Err(format!(
            "primary checkout branch '{branch}' is not its configured upstream branch '{upstream_branch}'"
        ));
    }
    let expected_upstream = format!("{remote_name}/{branch}");
    let upstream = git
        .stdout([
            "rev-parse",
            "--abbrev-ref",
            "--symbolic-full-name",
            "@{upstream}",
        ])
        .map_err(|_| "configured upstream cached ref cannot be resolved".to_string())?;
    if upstream != expected_upstream {
        return Err("configured upstream identity is ambiguous".to_string());
    }
    let cached_default_ref = format!("refs/remotes/{remote_name}/HEAD");
    let cached_default = git
        .stdout([
            "symbolic-ref",
            "--quiet",
            "--short",
            cached_default_ref.as_str(),
        ])
        .map_err(|_| "authoritative cached default branch cannot be resolved".to_string())?;
    if cached_default != upstream {
        return Err(format!(
            "checked-out branch '{branch}' is not the authoritative cached default branch"
        ));
    }
    let upstream_sha = git
        .stdout(["rev-parse", "--verify", "@{upstream}^{commit}"])
        .map_err(|_| "configured upstream cached ref cannot be resolved".to_string())?;
    Ok(RemoteState {
        configured_count,
        mode: "cached-upstream",
        upstream: Some(upstream),
        upstream_sha: Some(upstream_sha),
        remote_name: Some(remote_name),
    })
}

pub(super) fn cached_relation_counts(
    git: &Git,
    upstream_sha: &str,
    head: &str,
) -> Result<(usize, usize), String> {
    let range = format!("{upstream_sha}...{head}");
    let counts = git.stdout(["rev-list", "--left-right", "--count", range.as_str()])?;
    let mut values = counts.split_whitespace();
    let behind = values
        .next()
        .and_then(|value| value.parse::<usize>().ok())
        .ok_or_else(|| "failed to parse cached upstream behind count".to_string())?;
    let ahead = values
        .next()
        .and_then(|value| value.parse::<usize>().ok())
        .ok_or_else(|| "failed to parse cached upstream ahead count".to_string())?;
    if values.next().is_some() {
        return Err("failed to parse cached upstream relation counts".to_string());
    }
    Ok((behind, ahead))
}

fn verify_primary_worktree(git: &Git) -> Result<(), String> {
    let listing = git.stdout(["worktree", "list", "--porcelain"])?;
    let primary = listing
        .lines()
        .find_map(|line| line.strip_prefix("worktree "))
        .ok_or_else(|| "failed to resolve primary worktree".to_string())?;
    let primary = fs::canonicalize(primary)
        .map_err(|error| format!("failed to canonicalize primary worktree: {error}"))?;
    if primary != git.root() {
        return Err("default-branch may run only in the repository primary checkout".to_string());
    }
    Ok(())
}

fn verify_no_git_operation(git: &Git) -> Result<(), String> {
    for marker in [
        "MERGE_HEAD",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
        "BISECT_LOG",
        "rebase-merge",
        "rebase-apply",
    ] {
        let path = git.stdout(["rev-parse", "--git-path", marker])?;
        if canonical_git_path_unchecked(git.root(), &path).exists() {
            return Err(format!("Git operation in progress ({marker})"));
        }
    }
    Ok(())
}

fn canonical_git_path(root: &Path, value: &str) -> Result<PathBuf, String> {
    fs::canonicalize(canonical_git_path_unchecked(root, value))
        .map_err(|error| format!("failed to resolve Git path: {error}"))
}

fn canonical_git_path_unchecked(root: &Path, value: &str) -> PathBuf {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        path
    } else {
        root.join(path)
    }
}

fn valid_object_id(value: &str) -> bool {
    matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}
