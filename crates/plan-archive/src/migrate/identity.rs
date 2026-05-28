//! Derive the working-repo identity (host, org/group path, repo,
//! branch, commit SHA) for the migration metadata payload.
//!
//! Uses git plumbing exclusively — no provider auth required. The
//! provider identity comes from the configured `origin` remote URL.

use std::path::Path;

use serde::Serialize;

use nils_common::git::{self as gitio, parse_git_remote_url};

/// Source-repo identity captured at migration time.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SourceIdentity {
    pub host: String,
    pub org_or_group_path: String,
    pub repo: String,
    pub branch: String,
    pub commit: String,
}

#[derive(Debug, thiserror::Error)]
pub enum IdentityError {
    #[error("git command failed: {0}")]
    Io(String),
    #[error("origin remote not configured")]
    NoOriginRemote,
    #[error("origin remote URL `{0}` does not match a known provider shape")]
    UnparseableRemote(String),
    #[error("could not determine current branch (detached HEAD?)")]
    NoBranch,
    #[error("could not resolve HEAD")]
    NoCommit,
}

/// Top-level helper used from `migrate::prepare`. Reads remote +
/// branch + HEAD and parses out the host triple.
pub fn derive_source_identity(repo: &Path) -> Result<SourceIdentity, IdentityError> {
    let remote = origin_url(repo)?;
    let (host, org_or_group_path, repo_name) =
        parse_remote_url(&remote).ok_or(IdentityError::UnparseableRemote(remote.clone()))?;
    let branch = current_branch(repo)?;
    let commit = head_commit(repo)?;
    Ok(SourceIdentity {
        host,
        org_or_group_path,
        repo: repo_name,
        branch,
        commit,
    })
}

fn origin_url(repo: &Path) -> Result<String, IdentityError> {
    let out = gitio::run_output_in(repo, &["remote", "get-url", "origin"])
        .map_err(|e| IdentityError::Io(e.to_string()))?;
    if !out.status.success() {
        return Err(IdentityError::NoOriginRemote);
    }
    let url = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if url.is_empty() {
        return Err(IdentityError::NoOriginRemote);
    }
    Ok(url)
}

fn current_branch(repo: &Path) -> Result<String, IdentityError> {
    let out = gitio::run_output_in(repo, &["symbolic-ref", "--short", "HEAD"])
        .map_err(|e| IdentityError::Io(e.to_string()))?;
    if !out.status.success() {
        return Err(IdentityError::NoBranch);
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn head_commit(repo: &Path) -> Result<String, IdentityError> {
    let out = gitio::run_output_in(repo, &["rev-parse", "HEAD"])
        .map_err(|e| IdentityError::Io(e.to_string()))?;
    if !out.status.success() {
        return Err(IdentityError::NoCommit);
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Parse a git remote URL into `(host, org_or_group_path, repo)`.
/// Supports SSH (`git@github.com:org/repo.git`), HTTPS
/// (`https://github.com/org/repo.git`), nested GitLab groups, and
/// optional `.git` suffix.
pub fn parse_remote_url(url: &str) -> Option<(String, String, String)> {
    let parsed = parse_git_remote_url(url)?;
    let mut segments: Vec<&str> = parsed.path.split('/').filter(|s| !s.is_empty()).collect();
    if segments.len() < 2 {
        return None;
    }
    let repo = segments.pop()?.to_string();
    let org_or_group_path = segments.join("/");
    if org_or_group_path.is_empty() {
        return None;
    }
    Some((parsed.host, org_or_group_path, repo))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_github_ssh() {
        let r = parse_remote_url("git@github.com:graysurf/agent-runtime-kit.git").unwrap();
        assert_eq!(r.0, "github.com");
        assert_eq!(r.1, "graysurf");
        assert_eq!(r.2, "agent-runtime-kit");
    }

    #[test]
    fn parses_github_https_with_dotgit() {
        let r = parse_remote_url("https://github.com/sympoies/nils-cli.git").unwrap();
        assert_eq!(
            r,
            ("github.com".into(), "sympoies".into(), "nils-cli".into())
        );
    }

    #[test]
    fn parses_github_https_without_dotgit() {
        let r = parse_remote_url("https://github.com/sympoies/nils-cli").unwrap();
        assert_eq!(
            r,
            ("github.com".into(), "sympoies".into(), "nils-cli".into())
        );
    }

    #[test]
    fn parses_nested_gitlab_groups_ssh() {
        let r =
            parse_remote_url("git@gitlab.example.com:acme/platform/backend/ingest.git").unwrap();
        assert_eq!(
            r,
            (
                "gitlab.example.com".into(),
                "acme/platform/backend".into(),
                "ingest".into()
            )
        );
    }

    #[test]
    fn parses_nested_gitlab_groups_https() {
        let r =
            parse_remote_url("https://gitlab.example.com/acme/platform/backend/ingest").unwrap();
        assert_eq!(
            r,
            (
                "gitlab.example.com".into(),
                "acme/platform/backend".into(),
                "ingest".into()
            )
        );
    }

    #[test]
    fn rejects_url_without_org() {
        assert!(parse_remote_url("git@github.com:repo.git").is_none());
    }

    #[test]
    fn rejects_unknown_scheme() {
        assert!(parse_remote_url("file:///tmp/x.git").is_none());
    }

    #[test]
    fn parses_https_with_basic_auth_credentials() {
        let r = parse_remote_url("https://user:pass@github.com/org/repo.git").unwrap();
        assert_eq!(r, ("github.com".into(), "org".into(), "repo".into()));
    }

    #[test]
    fn parses_ssh_with_non_git_user() {
        let r = parse_remote_url("ssh://deploy@gitlab.example.com/group/proj.git").unwrap();
        assert_eq!(
            r,
            ("gitlab.example.com".into(), "group".into(), "proj".into())
        );
    }

    #[test]
    fn parses_ssh_with_userinfo_and_port() {
        let r = parse_remote_url("ssh://deploy@gitlab.example.com:2222/group/proj.git").unwrap();
        assert_eq!(
            r,
            ("gitlab.example.com".into(), "group".into(), "proj".into())
        );
    }
}
