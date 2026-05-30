//! Provider routing layer for plan-issue-cli (Sprint 2 Task 2.1).
//!
//! plan-issue-cli's lifecycle subcommands were originally hard-wired to `gh`
//! for every provider call. This module owns the abstraction that picks a
//! provider-aware adapter at runtime:
//!
//! - [`Provider`] discriminates GitHub vs GitLab.
//! - [`Repo`] carries the slug + host + provider needed to dispatch.
//! - [`resolve_repo`] derives a [`Repo`] from an explicit `--repo` flag and/or
//!   the cwd's git remote.
//! - [`select_adapter`] returns a `Box<dyn ProviderAdapter>` for the right
//!   backend. GitHub keeps using [`crate::github::GhCliAdapter`]; GitLab gets
//!   [`crate::forge_cli_adapter::ForgeCliAdapter`] which (in Sprint 2.1) is
//!   only a stub that returns `provider_not_implemented` for every call.
//!
//! Sprint 2.2 fills in the GitLab branch by routing through `forge-cli`'s
//! provider-neutral surface (`issue view --with-comments`, `pr view`,
//! `pr comments`, etc. — landed in #494/#495/#496).

use std::fmt;

use nils_common::git as common_git;

pub use crate::github::ProviderAdapter;

/// Provider discriminator. Carried alongside the slug so the right adapter
/// runs against the right backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    GitHub,
    GitLab,
    /// In-process, file-backed backend reached through `forge-cli --provider
    /// local`. Selected by an explicit `--repo local:<name>` (no host
    /// detection); routes through [`crate::forge_cli_adapter::ForgeCliAdapter`]
    /// like GitLab.
    Local,
}

impl Provider {
    pub fn as_str(self) -> &'static str {
        match self {
            Provider::GitHub => "github",
            Provider::GitLab => "gitlab",
            Provider::Local => "local",
        }
    }
}

impl fmt::Display for Provider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Resolved repository identity for one `plan-issue` invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repo {
    pub provider: Provider,
    /// `owner/repo` (GitHub) or `group[/subgroup]/project` (GitLab).
    pub slug: String,
    /// Provider host as it appears in the remote URL (e.g. `github.com`,
    /// `gitlab.com`). `None` when the provider was inferred without
    /// a host (e.g. bare `--repo owner/repo` default).
    pub host: Option<String>,
}

impl Repo {
    fn default_host(&self) -> &str {
        self.host.as_deref().unwrap_or(match self.provider {
            Provider::GitHub => "github.com",
            Provider::GitLab => "gitlab.com",
            // Local URLs use the `local://` scheme, not a host (see issue_url).
            Provider::Local => "local",
        })
    }

    /// Canonical issue URL hint used by lifecycle dashboards. GitHub uses
    /// `https://github.com/<slug>/issues/<n>`; GitLab uses
    /// `https://<host>/<slug>/-/issues/<n>` (GitLab redirects this to the
    /// Work Items surface when needed).
    pub fn issue_url(&self, issue_number: u64) -> String {
        match self.provider {
            Provider::GitHub => format!(
                "https://{host}/{slug}/issues/{issue_number}",
                host = self.default_host(),
                slug = self.slug,
            ),
            Provider::GitLab => format!(
                "https://{host}/{slug}/-/issues/{issue_number}",
                host = self.default_host(),
                slug = self.slug,
            ),
            // Synthetic scheme matching the forge-cli local backend
            // (`local://<slug>/issues/<n>`); this is a dashboard hint, the
            // authoritative URL comes from forge-cli's JSON.
            Provider::Local => {
                format!("local://{slug}/issues/{issue_number}", slug = self.slug)
            }
        }
    }

    /// Canonical PR / MR URL. GitHub uses `<host>/<slug>/pull/<n>`; GitLab
    /// uses `<host>/<slug>/-/merge_requests/<n>`.
    pub fn pr_url(&self, pr_number: u64) -> String {
        match self.provider {
            Provider::GitHub => format!(
                "https://{host}/{slug}/pull/{pr_number}",
                host = self.default_host(),
                slug = self.slug,
            ),
            Provider::GitLab => format!(
                "https://{host}/{slug}/-/merge_requests/{pr_number}",
                host = self.default_host(),
                slug = self.slug,
            ),
            Provider::Local => {
                format!("local://{slug}/pull/{pr_number}", slug = self.slug)
            }
        }
    }
}

/// Resolve a [`Repo`] from `--repo` and/or the cwd's git remote, mirroring
/// `forge-cli`'s detection ladder but kept local so plan-issue does not need
/// to subprocess-shell-out to `forge-cli repo view` just to learn its own
/// provider.
pub fn resolve_repo(repo_override: Option<&str>) -> Result<Repo, String> {
    if let Some(raw) = repo_override {
        // Explicit `--repo local:<name>` selects the in-process file-backed
        // backend; there is no remote host to detect.
        if let Some(name) = raw.strip_prefix("local:") {
            let slug = name.trim().trim_end_matches('/');
            if slug.is_empty() {
                return Err(format!("invalid --repo value: {raw}"));
            }
            return Ok(Repo {
                provider: Provider::Local,
                slug: slug.to_string(),
                host: None,
            });
        }
        if let Some(repo) = parse_repo_with_host(raw) {
            return Ok(repo);
        }
        if let Some(slug) = normalize_bare_slug(raw) {
            // Bare slug — fall back to the cwd remote to learn the host.
            let provider_from_remote = remote_provider().unwrap_or((Provider::GitHub, None));
            return Ok(Repo {
                provider: provider_from_remote.0,
                slug,
                host: provider_from_remote.1,
            });
        }
        return Err(format!("invalid --repo value: {raw}"));
    }

    let (slug, host, provider) = remote_repo()?;
    Ok(Repo {
        provider,
        slug,
        host: Some(host),
    })
}

/// Pick the right adapter implementation for [`Repo`].
///
/// In Sprint 2.1 the GitLab branch returns an [`crate::forge_cli_adapter::ForgeCliAdapter`]
/// whose methods all return a `provider_not_implemented` error. Sprint 2.2
/// fills in the actual GitLab calls (via `forge-cli` subprocesses) and wires
/// this factory into the per-dispatcher adapter construction sites.
pub fn select_adapter(repo: &Repo, force: bool) -> Box<dyn ProviderAdapter> {
    match repo.provider {
        Provider::GitHub => Box::new(crate::github::GhCliAdapter::new(force)),
        Provider::GitLab => Box::new(crate::forge_cli_adapter::ForgeCliAdapter::new(force)),
        Provider::Local => Box::new(crate::forge_cli_adapter::ForgeCliAdapter::new_local(force)),
    }
}

fn parse_repo_with_host(raw: &str) -> Option<Repo> {
    let parsed = common_git::parse_git_remote_url(raw)?;
    let provider = classify_host(&parsed.host)?;
    if !is_owner_repo(&parsed.path) && !is_group_project_path(&parsed.path) {
        return None;
    }
    Some(Repo {
        provider,
        slug: parsed.path,
        host: Some(parsed.host),
    })
}

fn normalize_bare_slug(raw: &str) -> Option<String> {
    let trimmed = raw.trim().trim_end_matches('/');
    if is_owner_repo(trimmed) || is_group_project_path(trimmed) {
        return Some(trimmed.to_string());
    }
    None
}

fn is_owner_repo(value: &str) -> bool {
    let mut parts = value.split('/');
    let owner = parts.next().unwrap_or_default().trim();
    let repo = parts.next().unwrap_or_default().trim();
    parts.next().is_none()
        && !owner.is_empty()
        && !repo.is_empty()
        && !value.contains(':')
        && !value.contains("://")
        && !value.ends_with(".git")
}

fn is_group_project_path(value: &str) -> bool {
    // Allow nested GitLab groups: at least two `/` separators.
    if value.contains(':') || value.contains("://") || value.ends_with(".git") {
        return false;
    }
    let segments: Vec<&str> = value.split('/').filter(|s| !s.is_empty()).collect();
    segments.len() >= 2 && segments.iter().all(|s| !s.trim().is_empty())
}

fn classify_host(host: &str) -> Option<Provider> {
    let host = host.trim().to_ascii_lowercase();
    if host == "github.com" || host.ends_with(".github.com") || host.ends_with(".ghe.com") {
        Some(Provider::GitHub)
    } else if host == "gitlab.com" || host.starts_with("gitlab.") || host.contains(".gitlab.") {
        Some(Provider::GitLab)
    } else {
        None
    }
}

fn remote_repo() -> Result<(String, String, Provider), String> {
    let output = common_git::run_output(&["remote", "get-url", "origin"])
        .map_err(|err| format!("failed to run `git remote get-url origin`: {err}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(format!(
            "failed to resolve repository from git remote: {}",
            if stderr.is_empty() {
                "unknown error"
            } else {
                &stderr
            }
        ));
    }
    let remote = String::from_utf8_lossy(&output.stdout).trim().to_string();
    parse_remote_url(&remote).ok_or_else(|| {
        format!(
            "unable to derive owner/repo from origin remote `{remote}`; pass --repo <owner/repo>"
        )
    })
}

fn remote_provider() -> Option<(Provider, Option<String>)> {
    let output = common_git::run_output(&["remote", "get-url", "origin"]).ok()?;
    if !output.status.success() {
        return None;
    }
    let remote = String::from_utf8_lossy(&output.stdout).trim().to_string();
    parse_remote_url(&remote).map(|(_, host, provider)| (provider, Some(host)))
}

fn parse_remote_url(remote: &str) -> Option<(String, String, Provider)> {
    let parsed = common_git::parse_git_remote_url(remote)?;
    let provider = classify_host(&parsed.host)?;
    if !is_owner_repo(&parsed.path) && !is_group_project_path(&parsed.path) {
        return None;
    }
    Some((parsed.path, parsed.host, provider))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_host_recognises_github_and_gitlab() {
        assert_eq!(classify_host("github.com"), Some(Provider::GitHub));
        assert_eq!(classify_host("gitlab.com"), Some(Provider::GitLab));
        assert_eq!(classify_host("bitbucket.org"), None);
    }

    #[test]
    fn classify_host_is_case_insensitive_and_trims() {
        assert_eq!(classify_host("GitHub.com"), Some(Provider::GitHub));
        assert_eq!(classify_host("  GitLab.com  "), Some(Provider::GitLab));
    }

    #[test]
    fn classify_host_recognises_github_enterprise_hosts() {
        assert_eq!(classify_host("internal.ghe.com"), Some(Provider::GitHub));
        assert_eq!(classify_host("corp.ghe.com"), Some(Provider::GitHub));
    }

    #[test]
    fn parse_repo_with_host_handles_https_and_ssh_forms() {
        let cases = [
            (
                "https://github.com/sympoies/nils-cli",
                Provider::GitHub,
                "sympoies/nils-cli",
                "github.com",
            ),
            (
                "git@gitlab.com:graysury/nils-cli-gitlab-sandbox.git",
                Provider::GitLab,
                "graysury/nils-cli-gitlab-sandbox",
                "gitlab.com",
            ),
            (
                "https://gitlab.com/group/sub/project",
                Provider::GitLab,
                "group/sub/project",
                "gitlab.com",
            ),
        ];
        for (raw, provider, slug, host) in cases {
            let repo = parse_repo_with_host(raw).unwrap_or_else(|| panic!("parse {raw}"));
            assert_eq!(repo.provider, provider);
            assert_eq!(repo.slug, slug);
            assert_eq!(repo.host.as_deref(), Some(host));
        }
    }

    #[test]
    fn parse_remote_url_handles_common_forms() {
        let cases = [
            (
                "git@github.com:sympoies/nils-cli.git",
                "sympoies/nils-cli",
                "github.com",
                Provider::GitHub,
            ),
            (
                "https://gitlab.com/graysury/nils-cli-gitlab-sandbox.git",
                "graysury/nils-cli-gitlab-sandbox",
                "gitlab.com",
                Provider::GitLab,
            ),
            (
                "ssh://git@gitlab.com/group/proj.git",
                "group/proj",
                "gitlab.com",
                Provider::GitLab,
            ),
        ];
        for (remote, slug, host, provider) in cases {
            let (s, h, p) = parse_remote_url(remote).unwrap_or_else(|| panic!("parse {remote}"));
            assert_eq!(s, slug);
            assert_eq!(h, host);
            assert_eq!(p, provider);
        }
    }

    #[test]
    fn parse_remote_url_strips_basic_auth_userinfo() {
        let cases = [
            (
                "https://user:pass@github.com/sympoies/nils-cli.git",
                "sympoies/nils-cli",
                "github.com",
                Provider::GitHub,
            ),
            (
                "https://x-access-token:TOKEN@gitlab.com/group/proj.git",
                "group/proj",
                "gitlab.com",
                Provider::GitLab,
            ),
        ];
        for (remote, slug, host, provider) in cases {
            let (s, h, p) = parse_remote_url(remote).unwrap_or_else(|| panic!("parse {remote}"));
            assert_eq!(s, slug);
            assert_eq!(h, host);
            assert_eq!(p, provider);
        }
    }

    #[test]
    fn parse_repo_with_host_strips_basic_auth_userinfo() {
        let repo =
            parse_repo_with_host("https://user:pass@github.com/sympoies/nils-cli").expect("parse");
        assert_eq!(repo.provider, Provider::GitHub);
        assert_eq!(repo.slug, "sympoies/nils-cli");
        assert_eq!(repo.host.as_deref(), Some("github.com"));
    }

    #[test]
    fn parse_remote_url_strips_userinfo_from_ssh_scheme() {
        let (slug, host, provider) =
            parse_remote_url("ssh://deploy@gitlab.example.com/group/proj.git").expect("parse");
        assert_eq!(slug, "group/proj");
        assert_eq!(host, "gitlab.example.com");
        assert_eq!(provider, Provider::GitLab);
    }

    #[test]
    fn parse_repo_with_host_strips_userinfo_from_ssh_scheme() {
        let repo =
            parse_repo_with_host("ssh://deploy@gitlab.example.com/group/proj").expect("parse");
        assert_eq!(repo.provider, Provider::GitLab);
        assert_eq!(repo.slug, "group/proj");
        assert_eq!(repo.host.as_deref(), Some("gitlab.example.com"));
    }

    #[test]
    fn resolve_repo_recognises_local_scheme() {
        let repo = resolve_repo(Some("local:demo")).expect("local");
        assert_eq!(repo.provider, Provider::Local);
        assert_eq!(repo.slug, "demo");
        assert_eq!(repo.host, None);

        let nested = resolve_repo(Some("local:acme/widgets")).expect("local nested");
        assert_eq!(nested.provider, Provider::Local);
        assert_eq!(nested.slug, "acme/widgets");
    }

    #[test]
    fn resolve_repo_rejects_empty_local_slug() {
        assert!(resolve_repo(Some("local:")).is_err());
        assert!(resolve_repo(Some("local:/")).is_err());
    }

    #[test]
    fn local_provider_renders_local_scheme_urls() {
        assert_eq!(Provider::Local.as_str(), "local");
        let repo = Repo {
            provider: Provider::Local,
            slug: "demo".into(),
            host: None,
        };
        assert_eq!(repo.issue_url(12), "local://demo/issues/12");
        assert_eq!(repo.pr_url(7), "local://demo/pull/7");
    }

    #[test]
    fn is_group_project_path_accepts_nested_groups_only() {
        assert!(is_group_project_path("group/project"));
        assert!(is_group_project_path("group/sub/project"));
        assert!(!is_group_project_path("loose"));
        assert!(!is_group_project_path(""));
        assert!(!is_group_project_path("with:colon"));
    }
}
