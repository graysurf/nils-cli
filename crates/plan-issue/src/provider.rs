//! Provider routing layer for plan-issue (Sprint 2 Task 2.1).
//!
//! plan-issue's lifecycle subcommands were originally hard-wired to `gh`
//! for every provider call. This module owns the abstraction that picks a
//! provider-aware adapter at runtime:
//!
//! - [`Provider`] discriminates GitHub vs GitLab.
//! - [`Repo`] carries the slug + host + provider needed to dispatch.
//! - [`resolve_repo`] derives a [`Repo`] from an explicit `--repo` flag and/or
//!   the cwd's git remote.
//! - [`select_adapter`] returns a `Box<dyn ProviderAdapter>` for the right
//!   backend. Every provider — GitHub, GitLab, and Local — routes through
//!   [`crate::forge_cli_adapter::ForgeCliAdapter`], a `forge-cli` subprocess
//!   wrapper. The trait itself lives in [`crate::adapter`].
//!
//! GitHub was the last provider on the retired in-crate `gh` client; the
//! plan-issue → forge-cli consolidation
//! (`docs/plans/2026-06-19-plan-issue-forge-cli-consolidation`) flipped it onto
//! `ForgeCliAdapter` so `forge-cli` is the single provider gateway.

use std::fmt;

use nils_common::git as common_git;

pub use crate::adapter::ProviderAdapter;

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
        self.host
            .as_deref()
            .unwrap_or_else(|| default_provider_host(self.provider))
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
            if !is_repo_path_for(Provider::Local, name) {
                return Err("invalid --repo value".to_string());
            }
            return Ok(Repo {
                provider: Provider::Local,
                slug: name.to_string(),
                host: None,
            });
        }
        if let Some(repo) = parse_repo_with_host(raw) {
            return Ok(repo);
        }
        if let Some(slug) = normalize_bare_slug(raw) {
            // Bare slug — fall back to the cwd remote to learn the host.
            let provider_from_remote = remote_provider().unwrap_or((Provider::GitHub, None));
            if !is_repo_path_for(provider_from_remote.0, &slug) {
                return Err("invalid --repo value".to_string());
            }
            return Ok(Repo {
                provider: provider_from_remote.0,
                slug,
                host: provider_from_remote.1,
            });
        }
        return Err("invalid --repo value".to_string());
    }

    let (slug, host, provider) = remote_repo()?;
    Ok(Repo {
        provider,
        slug,
        host: Some(host),
    })
}

/// Render a repository-valued CLI argument without transport credentials.
/// Qualified inputs retain their canonical provider authority; bare slugs stay
/// bare so command envelopes do not acquire ambient checkout details.
pub fn credential_free_repo_argument(raw: &str) -> String {
    let is_local = raw.starts_with("local:");
    let is_qualified = is_local || parse_repo_with_host(raw).is_some();
    match resolve_repo(Some(raw)) {
        Ok(repo) if is_local => format!("local:{}", repo.slug),
        Ok(repo) if is_qualified => {
            format!("https://{}/{}", repo.default_host(), repo.slug)
        }
        Ok(repo) => repo.slug,
        Err(_) => "[invalid repository identity]".to_string(),
    }
}

/// Pick the right adapter implementation for [`Repo`].
///
/// Every provider routes through [`crate::forge_cli_adapter::ForgeCliAdapter`],
/// a `forge-cli` subprocess wrapper. GitHub was the last provider on the
/// retired in-crate `gh` client; the consolidation
/// (`docs/plans/2026-06-19-plan-issue-forge-cli-consolidation`) flipped it onto
/// `ForgeCliAdapter` so `forge-cli` is the single provider gateway and identity
/// chokepoint. The adapter spawns `forge-cli` with `--provider github` and the
/// ambient token is inherited verbatim, preserving the prior identity model.
pub fn select_adapter(repo: &Repo, force: bool) -> Box<dyn ProviderAdapter> {
    match repo.provider {
        Provider::GitHub => match repo.host.as_deref() {
            Some(host) => Box::new(
                crate::forge_cli_adapter::ForgeCliAdapter::new_github_on_host(force, Some(host)),
            ),
            None => Box::new(crate::forge_cli_adapter::ForgeCliAdapter::new_github(force)),
        },
        Provider::GitLab => match repo.host.as_deref() {
            Some(host) => Box::new(crate::forge_cli_adapter::ForgeCliAdapter::new_on_host(
                force,
                Some(host),
            )),
            None => Box::new(crate::forge_cli_adapter::ForgeCliAdapter::new(force)),
        },
        Provider::Local => Box::new(crate::forge_cli_adapter::ForgeCliAdapter::new_local(force)),
    }
}

fn parse_repo_with_host(raw: &str) -> Option<Repo> {
    let parsed = common_git::parse_git_remote_url(raw)?;
    let host = parse_host(raw)?;
    let provider = classify_explicit_host(&host)?;
    let host = canonical_provider_host(provider, &host);
    if !is_repo_path_for(provider, &parsed.path) {
        return None;
    }
    Some(Repo {
        provider,
        slug: parsed.path,
        host: Some(host),
    })
}

fn normalize_bare_slug(raw: &str) -> Option<String> {
    if is_owner_repo(raw) || is_group_project_path(raw) {
        return Some(raw.to_string());
    }
    None
}

fn repo_segments_are_valid(value: &str) -> bool {
    value == value.trim()
        && !value.contains("://")
        && !value
            .chars()
            .any(|ch| ch.is_whitespace() || ch.is_control())
        && value
            .split('/')
            .all(|segment| !segment.is_empty() && !matches!(segment, "." | ".."))
}

fn is_owner_repo(value: &str) -> bool {
    repo_segments_are_valid(value)
        && value.split('/').count() == 2
        && !value.contains(':')
        && !value.ends_with(".git")
}

fn is_group_project_path(value: &str) -> bool {
    repo_segments_are_valid(value)
        && value.split('/').count() >= 2
        && !value.contains(':')
        && !value.ends_with(".git")
}

fn is_repo_path_for(provider: Provider, value: &str) -> bool {
    match provider {
        Provider::GitHub => is_owner_repo(value),
        Provider::GitLab => is_group_project_path(value),
        Provider::Local => repo_segments_are_valid(value),
    }
}

/// Classify authorities that may be inherited automatically from the checkout.
/// Only provider-owned suffixes are trusted; branding-like prefixes on an
/// unrelated domain must never select a credential-bearing backend.
fn classify_host(host: &str) -> Option<Provider> {
    let authority = parse_authority(host)?;
    let hostname = authority
        .rsplit_once(':')
        .map(|(hostname, _)| hostname)
        .unwrap_or(&authority);
    if hostname == "github.com"
        || hostname.ends_with(".github.com")
        || hostname.ends_with(".ghe.com")
    {
        Some(Provider::GitHub)
    } else if hostname == "gitlab.com" || hostname.ends_with(".gitlab.com") {
        Some(Provider::GitLab)
    } else {
        None
    }
}

/// Qualified `--repo` values deliberately name their authority, so documented
/// self-hosted GitLab names remain available without trusting the same host when
/// it appears only in ambient Git configuration.
fn classify_explicit_host(host: &str) -> Option<Provider> {
    if let Some(provider) = classify_host(host) {
        return Some(provider);
    }
    let authority = parse_authority(host)?;
    let hostname = authority
        .rsplit_once(':')
        .map(|(hostname, _)| hostname)
        .unwrap_or(&authority);
    hostname.starts_with("gitlab.").then_some(Provider::GitLab)
}

/// Parse the credential-free API authority from a Git remote URL. HTTP(S)
/// remotes retain canonical non-default ports; SSH transport ports are
/// intentionally discarded because they are not API ports.
fn parse_host(url: &str) -> Option<String> {
    if url != url.trim() || url.chars().any(char::is_control) {
        return None;
    }
    if let Some((scheme, remainder)) = url.split_once("://")
        && matches!(scheme.to_ascii_lowercase().as_str(), "http" | "https")
    {
        let raw_authority = remainder.split(['/', '?', '#']).next().unwrap_or_default();
        let authority = raw_authority
            .rsplit_once('@')
            .map(|(_, authority)| authority)
            .unwrap_or(raw_authority);
        return parse_authority(authority);
    }
    common_git::parse_git_remote_url(url).and_then(|remote| parse_authority(&remote.host))
}

/// Normalize a user-facing hostname or hostname:port authority without ever
/// retaining URL credentials. This intentionally mirrors forge-cli's authority
/// contract so plan-issue forwards the exact authority forge-cli accepts.
fn parse_authority(value: &str) -> Option<String> {
    if value.is_empty()
        || value != value.trim()
        || value
            .chars()
            .any(|ch| ch.is_whitespace() || ch.is_control())
        || value.contains(['/', '\\', '@', '?', '#'])
        || value.matches(':').count() > 1
    {
        return None;
    }
    let (hostname, port) = match value.rsplit_once(':') {
        Some((hostname, port)) => {
            let port = port.parse::<u16>().ok().filter(|port| *port != 0)?;
            (hostname, Some(port))
        }
        None => (value, None),
    };
    if hostname.is_empty() || hostname.len() > 253 {
        return None;
    }
    let hostname = hostname.to_ascii_lowercase();
    if hostname != "local"
        && !hostname.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                && label
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_alphanumeric)
                && label
                    .as_bytes()
                    .last()
                    .is_some_and(u8::is_ascii_alphanumeric)
        })
    {
        return None;
    }
    match port {
        Some(443) => Some(hostname),
        Some(port) => Some(format!("{hostname}:{port}")),
        None => Some(hostname),
    }
}

/// Canonical web/API authority for a resolved provider. Transport aliases are
/// normalized without changing self-hosted authorities or meaningful ports.
pub(crate) fn canonical_provider_host(provider: Provider, host: &str) -> String {
    let Some(authority) = parse_authority(host) else {
        return common_git::canonical_git_host(host);
    };
    let (hostname, port) = authority
        .rsplit_once(':')
        .map(|(hostname, port)| (hostname, Some(port)))
        .unwrap_or((authority.as_str(), None));
    let mut hostname = common_git::canonical_git_host(hostname);
    if matches!(provider, Provider::GitLab) && hostname == "altssh.gitlab.com" {
        hostname = "gitlab.com".to_string();
    }
    match port {
        Some(port) => format!("{hostname}:{port}"),
        None => hostname,
    }
}

pub(crate) fn default_provider_host(provider: Provider) -> &'static str {
    match provider {
        Provider::GitHub => "github.com",
        Provider::GitLab => "gitlab.com",
        // Local URLs use the `local://` scheme, not a network authority.
        Provider::Local => "local",
    }
}

pub(crate) fn authorities_equal(provider: Provider, left: &str, right: &str) -> bool {
    canonical_provider_host(provider, left)
        .eq_ignore_ascii_case(&canonical_provider_host(provider, right))
}

pub(crate) fn optional_authorities_equal(
    provider: Provider,
    left: Option<&str>,
    right: Option<&str>,
) -> bool {
    authorities_equal(
        provider,
        left.unwrap_or_else(|| default_provider_host(provider)),
        right.unwrap_or_else(|| default_provider_host(provider)),
    )
}

pub(crate) fn current_remote_matches(expected: &Repo) -> bool {
    if matches!(expected.provider, Provider::Local) {
        return false;
    }
    let Ok(output) = common_git::run_output(&["remote", "get-url", "origin"]) else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    let remote = String::from_utf8_lossy(&output.stdout);
    let remote = remote.trim();
    let Some(parsed) = common_git::parse_git_remote_url(remote) else {
        return false;
    };
    let Some(host) = parse_host(remote) else {
        return false;
    };
    is_repo_path_for(expected.provider, &parsed.path)
        && authorities_equal(expected.provider, &host, expected.default_host())
        && match expected.provider {
            Provider::GitHub => parsed.path.eq_ignore_ascii_case(&expected.slug),
            Provider::GitLab | Provider::Local => parsed.path == expected.slug,
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
        "unable to derive owner/repo from origin remote; pass --repo <owner/repo>".to_string()
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
    let host = parse_host(remote)?;
    let provider = classify_host(&host)?;
    let host = canonical_provider_host(provider, &host);
    if !is_repo_path_for(provider, &parsed.path) {
        return None;
    }
    Some((parsed.path, host, provider))
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
    fn classify_host_is_case_insensitive_and_rejects_whitespace() {
        assert_eq!(classify_host("GitHub.com"), Some(Provider::GitHub));
        assert_eq!(classify_host("  GitLab.com  "), None);
    }

    #[test]
    fn classify_host_recognises_github_enterprise_hosts() {
        assert_eq!(classify_host("internal.ghe.com"), Some(Provider::GitHub));
        assert_eq!(classify_host("corp.ghe.com"), Some(Provider::GitHub));
        assert_eq!(
            classify_host("internal.ghe.com:8443"),
            Some(Provider::GitHub)
        );
    }

    #[test]
    fn automatic_host_classification_requires_a_trusted_gitlab_suffix() {
        assert_eq!(classify_host("gitlab.example.com:8443"), None);
        assert_eq!(
            classify_host("team.gitlab.com:8443"),
            Some(Provider::GitLab)
        );
        assert_eq!(classify_host("gitlab.attacker.example"), None);
        assert_eq!(classify_host("evil.gitlab.example.com"), None);
    }

    #[test]
    fn explicit_qualified_repo_can_select_self_hosted_gitlab() {
        let repo = parse_repo_with_host("https://gitlab.example.com:8443/group/sub/project.git")
            .expect("explicit self-hosted GitLab repository");

        assert_eq!(repo.provider, Provider::GitLab);
        assert_eq!(repo.slug, "group/sub/project");
        assert_eq!(repo.host.as_deref(), Some("gitlab.example.com:8443"));
    }

    #[test]
    fn automatic_remote_detection_rejects_gitlab_branding_prefix() {
        assert!(parse_remote_url("https://gitlab.attacker.example/group/project.git").is_none());
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
    fn qualified_https_preserves_canonical_non_default_authority_in_urls() {
        let repo =
            parse_repo_with_host("https://operator:secret@INTERNAL.GHE.COM:8443/acme/widgets.git")
                .expect("qualified GitHub Enterprise repository");

        assert_eq!(repo.provider, Provider::GitHub);
        assert_eq!(repo.slug, "acme/widgets");
        assert_eq!(repo.host.as_deref(), Some("internal.ghe.com:8443"));
        assert_eq!(
            repo.issue_url(42),
            "https://internal.ghe.com:8443/acme/widgets/issues/42"
        );
        assert_eq!(
            repo.pr_url(7),
            "https://internal.ghe.com:8443/acme/widgets/pull/7"
        );
    }

    #[test]
    fn explicit_https_443_canonicalizes_to_same_repo_identity() {
        let explicit = parse_repo_with_host("https://INTERNAL.GHE.COM:443/acme/widgets.git")
            .expect("explicit default port");
        let implicit = parse_repo_with_host("https://internal.ghe.com/acme/widgets.git")
            .expect("implicit default port");
        let non_default = parse_repo_with_host("https://internal.ghe.com:8443/acme/widgets.git")
            .expect("non-default port");

        assert_eq!(explicit, implicit);
        assert_ne!(explicit, non_default);
    }

    #[test]
    fn qualified_http_authority_rejects_invalid_ports_without_leaking_credentials() {
        assert!(
            parse_repo_with_host("https://user:secret@internal.ghe.com:0/acme/widgets").is_none()
        );
        assert!(
            parse_repo_with_host("https://user:secret@internal.ghe.com:not-a-port/acme/widgets")
                .is_none()
        );
        assert!(parse_repo_with_host(" https://internal.ghe.com:8443/acme/widgets").is_none());
    }

    #[test]
    fn resolve_repo_rejects_malformed_qualified_url_without_leaking_credentials() {
        let error = resolve_repo(Some(
            "https://alice:secret@internal.ghe.com:not-a-port/acme/widgets",
        ))
        .expect_err("malformed authority");

        assert_eq!(error, "invalid --repo value");
        assert!(!error.contains("alice"));
        assert!(!error.contains("secret"));
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
    fn transport_aliases_resolve_to_canonical_provider_authorities() {
        let cases = [
            (
                "ssh://git@ssh.github.com:443/acme/widgets.git",
                Provider::GitHub,
                "github.com",
                "https://github.com/acme/widgets/issues/42",
                "https://github.com/acme/widgets/pull/7",
            ),
            (
                "ssh://git@altssh.gitlab.com:443/group/sub/project.git",
                Provider::GitLab,
                "gitlab.com",
                "https://gitlab.com/group/sub/project/-/issues/42",
                "https://gitlab.com/group/sub/project/-/merge_requests/7",
            ),
        ];

        for (raw, provider, host, issue_url, pr_url) in cases {
            let repo = resolve_repo(Some(raw)).unwrap_or_else(|error| panic!("{raw}: {error}"));
            assert_eq!(repo.provider, provider, "{raw}");
            assert_eq!(repo.host.as_deref(), Some(host), "{raw}");
            assert_eq!(repo.issue_url(42), issue_url, "{raw}");
            assert_eq!(repo.pr_url(7), pr_url, "{raw}");
            assert_eq!(
                credential_free_repo_argument(raw),
                format!("https://{host}/{}", repo.slug),
                "{raw}"
            );
        }
    }

    #[test]
    fn transport_alias_canonicalization_preserves_ports_and_self_hosted_authorities() {
        let github = parse_repo_with_host("https://ssh.github.com:8443/acme/widgets.git")
            .expect("GitHub transport alias with API port");
        assert_eq!(github.host.as_deref(), Some("github.com:8443"));

        let gitlab = parse_repo_with_host("https://altssh.gitlab.com:8443/group/sub/project.git")
            .expect("GitLab transport alias with API port");
        assert_eq!(gitlab.host.as_deref(), Some("gitlab.com:8443"));

        let self_hosted =
            parse_repo_with_host("https://gitlab.example.com:8443/group/sub/project.git")
                .expect("self-hosted GitLab authority");
        assert_eq!(self_hosted.host.as_deref(), Some("gitlab.example.com:8443"));
    }

    #[test]
    fn remote_http_authority_keeps_api_port_but_ssh_discards_transport_port() {
        let https = parse_remote_url("https://internal.ghe.com:8443/acme/widgets.git")
            .expect("HTTPS remote");
        assert_eq!(
            https,
            (
                "acme/widgets".to_string(),
                "internal.ghe.com:8443".to_string(),
                Provider::GitHub,
            )
        );

        let ssh = parse_remote_url("ssh://git@internal.ghe.com:2222/acme/widgets.git")
            .expect("SSH remote");
        assert_eq!(
            ssh,
            (
                "acme/widgets".to_string(),
                "internal.ghe.com".to_string(),
                Provider::GitHub,
            )
        );
    }

    #[test]
    fn github_repo_paths_reject_nested_namespaces() {
        assert!(parse_repo_with_host("https://github.com/org/team/repo").is_none());
        assert!(parse_repo_with_host("git@github.com:org/team/repo.git").is_none());
        assert!(parse_remote_url("https://github.com/org/team/repo.git").is_none());
        assert!(parse_remote_url("git@github.com:org/team/repo.git").is_none());
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
            parse_remote_url("ssh://deploy@gitlab.com/group/proj.git").expect("parse");
        assert_eq!(slug, "group/proj");
        assert_eq!(host, "gitlab.com");
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
    fn repository_paths_reject_whitespace_empty_and_dot_segments() {
        for slug in [
            "owner /repo",
            "owner/ repo",
            "./repo",
            "owner/../repo",
            "owner//repo",
            "owner/repo/",
        ] {
            assert!(normalize_bare_slug(slug).is_none(), "bare slug: {slug:?}");
            assert!(
                resolve_repo(Some(&format!("local:{slug}"))).is_err(),
                "local slug: {slug:?}"
            );
        }
        assert!(
            resolve_repo(Some("https://github.com/owner /repo")).is_err(),
            "qualified repository path must use the same segment contract"
        );
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
