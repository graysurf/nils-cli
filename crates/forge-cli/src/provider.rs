//! Provider detection and host-classification.
//!
//! Spec: `crates/forge-cli/docs/specs/forge-cli-spec-v1.md` §"Provider
//! detection". The ladder is: explicit `--provider` flag, then
//! `git remote get-url <--remote>` host parse, then cached `gh auth status`
//! / `glab auth status` host match. Unknown host produces `USAGE 64` with
//! `error.kind = "provider_unsupported"`.

use std::ffi::OsString;
use std::process::Command;

use serde::Serialize;

use nils_common::git::canonical_git_host;

use crate::cli::BINARY;
use crate::error::ForgeError;

const REMOTE_URL_OMITTED_DETAIL: &str = "selected remote URL omitted from diagnostics";

/// Two-provider enum used everywhere downstream of detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    GitHub,
    GitLab,
    /// In-process, file-backed backend selected by `--provider local`. Rides
    /// the GitHub op paths (gh-style argv + JSON) but is served by
    /// [`crate::local::LocalRunner`] against a local store instead of `gh`.
    Local,
}

impl Provider {
    /// Stable lower-case rendering used in envelopes (`data.provider`).
    pub fn as_str(self) -> &'static str {
        match self {
            Provider::GitHub => "github",
            Provider::GitLab => "gitlab",
            Provider::Local => "local",
        }
    }
}

/// Hint from the CLI layer: caller may force a provider via `--provider`, or
/// leave it on auto-detect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderHint {
    Auto,
    Forced(Provider),
    /// Provider and host were both explicitly bound by the caller.
    ForcedHost(Provider, String),
    /// Host was explicitly supplied and determines the provider.
    Host(String),
}

/// Resolved provider context handed to every op.
#[derive(Debug, Clone)]
pub struct ProviderContext {
    pub provider: Provider,
    pub host: String,
    pub source: DetectionSource,
    /// Repo slug, from explicit `--repo owner/name` or else derived from the
    /// detected remote (see [`detect`]). Ops push `--repo <slug>` into the
    /// backend argv when set so dry-run plans and live calls hit the cloned repo
    /// instead of gh/glab's cwd default (which retargets a fork clone to its
    /// upstream parent).
    pub repo: Option<String>,
}

impl ProviderContext {
    /// Render the repository in the provider-specific locator shape accepted by
    /// backend commands. Default-host repositories stay as their raw slug;
    /// GitHub Enterprise uses `HOST/OWNER/REPO`, while self-hosted GitLab uses a
    /// full URL so `glab` cannot reinterpret the host as another namespace.
    pub fn repo_locator(&self) -> Option<String> {
        let repo = self.repo.as_deref()?;
        let host = canonical_provider_host(self.provider, &self.host);
        if host.eq_ignore_ascii_case(default_host_for(self.provider)) {
            return Some(repo.to_string());
        }
        Some(match self.provider {
            Provider::GitHub => format!("{host}/{repo}"),
            Provider::GitLab => format!("https://{host}/{repo}"),
            Provider::Local => repo.to_string(),
        })
    }

    /// Push the centralized provider-aware repository locator into `argv`.
    pub fn push_repo_override(&self, argv: &mut Vec<OsString>) {
        if let Some(locator) = self.repo_locator() {
            argv.push(OsString::from("--repo"));
            argv.push(OsString::from(locator));
        }
    }

    /// Push `gh api --hostname <host>` for GitHub Enterprise hosts. `gh api`
    /// defaults to github.com unless this flag is present.
    ///
    /// github.com transport aliases — notably the SSH-over-443 host
    /// `ssh.github.com` — share the github.com API host, so they must not get a
    /// `--hostname` override (that would point `gh api` at the SSH host, not the
    /// API). Real GitHub Enterprise hosts are distinct API hosts and keep it.
    pub fn push_github_api_hostname(&self, argv: &mut Vec<OsString>) {
        let host = canonical_provider_host(self.provider, &self.host);
        if matches!(self.provider, Provider::GitHub) && host != "github.com" {
            argv.push(OsString::from("--hostname"));
            argv.push(OsString::from(host));
        }
    }
}

/// Where the provider decision came from. Useful for diagnostics and tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DetectionSource {
    /// `--provider` flag.
    Flag,
    /// Host parsed from `git remote get-url <--remote>`.
    Remote,
    /// Default-host fallback (`github.com` / `gitlab.com`).
    DefaultHost,
}

/// Whether repository input participates in provider resolution for a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RepositoryScope {
    Scoped,
    Independent,
}

/// Detect provider context for a repository-scoped command.
///
/// `remote_url_lookup` is injected so tests can stub it without spawning git.
pub fn detect(
    hint: ProviderHint,
    remote: &str,
    repo_override: Option<&str>,
    remote_url_lookup: impl Fn(&str) -> Option<String>,
) -> Result<ProviderContext, ForgeError> {
    detect_with_scope(
        hint,
        remote,
        repo_override,
        remote_url_lookup,
        RepositoryScope::Scoped,
    )
}

/// Detect provider context for a command that does not operate on a repository.
/// Repository input is retained for diagnostics but its shape is irrelevant.
pub fn detect_unscoped(
    hint: ProviderHint,
    remote: &str,
    repo_override: Option<&str>,
    remote_url_lookup: impl Fn(&str) -> Option<String>,
) -> Result<ProviderContext, ForgeError> {
    detect_with_scope(
        hint,
        remote,
        repo_override,
        remote_url_lookup,
        RepositoryScope::Independent,
    )
}

fn detect_with_scope(
    hint: ProviderHint,
    remote: &str,
    repo_override: Option<&str>,
    remote_url_lookup: impl Fn(&str) -> Option<String>,
    scope: RepositoryScope,
) -> Result<ProviderContext, ForgeError> {
    let url = remote_url_lookup(remote);
    let remote_host = url.as_deref().and_then(parse_host);
    let remote_repo = url.as_deref().and_then(parse_slug);

    match hint {
        ProviderHint::ForcedHost(provider, host) => {
            if provider == Provider::Local {
                if host != "local" {
                    return Err(unsupported_explicit_host());
                }
                return resolved_context(
                    provider,
                    host,
                    DetectionSource::Flag,
                    repo_override.map(str::to_string).or(remote_repo),
                    scope,
                );
            }
            let host = parse_authority(&host).ok_or_else(unsupported_explicit_host)?;
            if classify_host(&host).is_some_and(|classified| classified != provider) {
                return Err(ForgeError::provider_unsupported(
                    schema(),
                    "explicit forge host does not match the selected provider",
                    None,
                ));
            }
            let repo = repo_override.map(str::to_string).or_else(|| {
                remote_host
                    .as_deref()
                    .filter(|remote_host| authorities_equal(provider, remote_host, &host))
                    .and(remote_repo)
            });
            if matches!(scope, RepositoryScope::Scoped) && repo.is_none() {
                return Err(explicit_host_repo_required());
            }
            resolved_context(provider, host, DetectionSource::Flag, repo, scope)
        }
        ProviderHint::Host(host) => {
            let host = parse_authority(&host).ok_or_else(unsupported_explicit_host)?;
            let provider = classify_host(&host).ok_or_else(unsupported_explicit_host)?;
            let repo = repo_override.map(str::to_string).or_else(|| {
                remote_host
                    .as_deref()
                    .filter(|remote_host| authorities_equal(provider, remote_host, &host))
                    .and(remote_repo)
            });
            if matches!(scope, RepositoryScope::Scoped) && repo.is_none() {
                return Err(explicit_host_repo_required());
            }
            resolved_context(provider, host, DetectionSource::Flag, repo, scope)
        }
        ProviderHint::Forced(Provider::Local) => resolved_context(
            Provider::Local,
            "local".to_string(),
            DetectionSource::Flag,
            repo_override.map(str::to_string).or(remote_repo),
            scope,
        ),
        ProviderHint::Forced(provider) => {
            let remote_authority_is_optional =
                matches!(scope, RepositoryScope::Independent) || repo_override.is_some();
            let remote_positively_conflicts = remote_host
                .as_deref()
                .and_then(classify_host)
                .is_some_and(|classified| classified != provider);
            let (host, repo) = if url.is_none()
                || (remote_authority_is_optional && remote_positively_conflicts)
            {
                (
                    default_host_for(provider).to_string(),
                    repo_override.map(str::to_string),
                )
            } else {
                let host = remote_host.ok_or_else(|| {
                    ForgeError::provider_unsupported(
                        schema(),
                        "the selected Git remote does not expose a valid forge authority",
                        None,
                    )
                })?;
                if classify_host(&host) != Some(provider) {
                    return Err(ForgeError::provider_unsupported(
                        schema(),
                        format!(
                            "selected provider '{}' does not match Git remote authority '{host}'; pass --host with an explicit custom authority",
                            provider.as_str()
                        ),
                        None,
                    ));
                }
                (host, repo_override.map(str::to_string).or(remote_repo))
            };
            resolved_context(provider, host, DetectionSource::Flag, repo, scope)
        }
        ProviderHint::Auto => {
            if url.is_none() {
                return Err(ForgeError::provider_unsupported(
                    schema(),
                    "no remote URL available and no --provider override supplied",
                    None,
                ));
            }
            let Some(host) = remote_host else {
                return Err(ForgeError::provider_unsupported(
                    schema(),
                    "the selected Git remote does not expose a valid forge authority",
                    Some(REMOTE_URL_OMITTED_DETAIL.to_string()),
                ));
            };
            let provider = classify_host(&host).ok_or_else(|| {
                ForgeError::provider_unsupported(
                    schema(),
                    format!("unsupported forge host: {host}"),
                    Some(REMOTE_URL_OMITTED_DETAIL.to_string()),
                )
            })?;
            resolved_context(
                provider,
                host,
                DetectionSource::Remote,
                repo_override.map(str::to_string).or(remote_repo),
                scope,
            )
        }
    }
}

fn unsupported_explicit_host() -> ForgeError {
    ForgeError::provider_unsupported(
        schema(),
        "explicit forge host must be a hostname or hostname:port authority without userinfo, path, query, fragment, whitespace, or control characters",
        None,
    )
}

fn explicit_host_repo_required() -> ForgeError {
    ForgeError::validation(
        schema(),
        "repo_required",
        "repository-scoped operations with --host require --repo or a same-authority Git remote",
        None,
    )
}

fn schema() -> String {
    nils_common::cli_contract::schema_version_for(BINARY, "error", 1)
}

/// Default host used when the caller forces a provider but no remote URL is
/// available. The auth-status fallback layer can override this later.
fn default_host_for(provider: Provider) -> &'static str {
    match provider {
        Provider::GitHub => "github.com",
        Provider::GitLab => "gitlab.com",
        Provider::Local => "local",
    }
}

fn resolved_context(
    provider: Provider,
    host: String,
    source: DetectionSource,
    repo: Option<String>,
    scope: RepositoryScope,
) -> Result<ProviderContext, ForgeError> {
    if matches!(scope, RepositoryScope::Scoped)
        && let Some(repository) = repo.as_deref()
    {
        validate_repo_shape(provider, repository)?;
    }
    Ok(ProviderContext {
        provider,
        host: canonical_provider_host(provider, &host),
        source,
        repo,
    })
}

/// Canonical API authority for a resolved provider. The common Git helper owns
/// transport aliases shared across the workspace; forge-cli adds GitLab's
/// alternate SSH transport because the provider is known here. Non-default
/// HTTPS ports remain part of the authority.
pub(crate) fn canonical_provider_host(provider: Provider, host: &str) -> String {
    let Some(authority) = parse_authority(host) else {
        return canonical_git_host(host);
    };
    let (hostname, port) = split_authority(&authority);
    let mut hostname = canonical_git_host(hostname);
    if matches!(provider, Provider::GitLab) && hostname == "altssh.gitlab.com" {
        hostname = "gitlab.com".to_string();
    }
    match port {
        Some(port) => format!("{hostname}:{port}"),
        None => hostname,
    }
}

pub(crate) fn authorities_equal(provider: Provider, left: &str, right: &str) -> bool {
    canonical_provider_host(provider, left)
        .eq_ignore_ascii_case(&canonical_provider_host(provider, right))
}

fn validate_repo_shape(provider: Provider, repository: &str) -> Result<(), ForgeError> {
    let shaped_repository = if matches!(provider, Provider::Local) {
        repository.strip_prefix("local:").unwrap_or(repository)
    } else {
        repository
    };
    let segments = shaped_repository.split('/').collect::<Vec<_>>();
    let segments_are_valid = repository == repository.trim()
        && !repository.contains("://")
        && !repository
            .chars()
            .any(|ch| ch.is_whitespace() || ch.is_control())
        && segments
            .iter()
            .all(|segment| !segment.is_empty() && !matches!(*segment, "." | ".."));
    let provider_shape_is_valid = match provider {
        Provider::GitHub => segments.len() == 2,
        Provider::GitLab => segments.len() >= 2,
        Provider::Local => !segments.is_empty(),
    };
    if segments_are_valid && provider_shape_is_valid {
        return Ok(());
    }

    let expected = match provider {
        Provider::GitHub => "owner/name",
        Provider::GitLab => "group[/subgroup...]/project",
        Provider::Local => "local:<slug> or <slug>",
    };
    Err(ForgeError::validation(
        schema(),
        "repo_invalid",
        format!(
            "repository is not a valid {} repository path; expected {expected}",
            provider.as_str()
        ),
        None,
    ))
}

/// Classify a canonical hostname or hostname:port authority against the v1
/// backend table. Classification uses the hostname only; an explicit provider
/// is required for otherwise-valid custom authorities.
pub fn classify_host(host: &str) -> Option<Provider> {
    let authority = parse_authority(host)?;
    let (hostname, _) = split_authority(&authority);
    let hostname = canonical_git_host(hostname);
    if hostname == "github.com"
        || hostname.ends_with(".github.com")
        || hostname.ends_with(".ghe.com")
    {
        return Some(Provider::GitHub);
    }
    if hostname == "gitlab.com" || hostname.ends_with(".gitlab.com") {
        return Some(Provider::GitLab);
    }
    None
}

/// Parse the canonical API authority out of a remote URL. HTTP(S) remotes
/// retain non-default HTTPS ports; SSH transport ports are intentionally
/// discarded because they are not API ports.
pub fn parse_host(url: &str) -> Option<String> {
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
    nils_common::git::parse_git_remote_url(url).and_then(|remote| parse_authority(&remote.host))
}

/// Parse and normalize a user-facing hostname or hostname:port authority.
/// Schemes, userinfo, paths, query strings, fragments, whitespace, control
/// characters, malformed labels, and invalid ports are rejected.
pub(crate) fn parse_authority(value: &str) -> Option<String> {
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

fn split_authority(authority: &str) -> (&str, Option<u16>) {
    match authority.rsplit_once(':') {
        Some((hostname, port)) => (hostname, port.parse::<u16>().ok()),
        None => (authority, None),
    }
}

/// Parse the `owner/name` (or GitLab `group/.../project`) slug out of a remote
/// URL, using the same parser as [`parse_host`] so the supported URL shapes
/// match. Returns `None` when the URL has no `owner/name`-shaped path (e.g.
/// `file://` remotes or a bare segment), in which case ops fall back to
/// gh/glab's own cwd-based repo resolution rather than pinning a bad `--repo`.
pub fn parse_slug(url: &str) -> Option<String> {
    nils_common::git::parse_git_remote_url(url)
        .map(|remote| remote.path)
        .filter(|path| path.contains('/'))
}

/// Default `git remote get-url` lookup used in production. Returns `None`
/// when git is unavailable or the remote does not exist.
pub fn git_remote_url(remote: &str) -> Option<String> {
    let output = Command::new("git")
        .args(["remote", "get-url", remote])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let url = String::from_utf8(output.stdout).ok()?;
    let url = url.trim().to_string();
    if url.is_empty() { None } else { Some(url) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn classify_host_recognises_github_com() {
        assert_eq!(classify_host("github.com"), Some(Provider::GitHub));
    }

    #[test]
    fn classify_host_recognises_ghe_host() {
        assert_eq!(classify_host("internal.ghe.com"), Some(Provider::GitHub));
    }

    #[test]
    fn classify_host_recognises_gitlab_com() {
        assert_eq!(classify_host("gitlab.com"), Some(Provider::GitLab));
    }

    #[test]
    fn classify_host_requires_trusted_gitlab_suffix() {
        assert_eq!(classify_host("gitlab.example.com"), None);
        assert_eq!(classify_host("forge.gitlab.com"), Some(Provider::GitLab));
    }

    #[test]
    fn classify_host_rejects_unknown() {
        assert_eq!(classify_host("bitbucket.org"), None);
        assert_eq!(classify_host("codeberg.org"), None);
    }

    fn github_ctx(host: &str) -> ProviderContext {
        ProviderContext {
            provider: Provider::GitHub,
            host: host.into(),
            source: DetectionSource::Remote,
            repo: None,
        }
    }

    #[test]
    fn push_github_api_hostname_omitted_for_github_com() {
        let mut argv = Vec::new();
        github_ctx("github.com").push_github_api_hostname(&mut argv);
        assert!(
            argv.is_empty(),
            "github.com is the default API host: {argv:?}"
        );
    }

    #[test]
    fn push_github_api_hostname_omitted_for_ssh_github_com_alias() {
        // `ssh.github.com` (SSH over 443) shares the github.com API host, so it
        // must not become a `--hostname` override pointing at the SSH host.
        let mut argv = Vec::new();
        github_ctx("ssh.github.com").push_github_api_hostname(&mut argv);
        assert!(
            argv.is_empty(),
            "ssh.github.com must resolve to the github.com API host: {argv:?}"
        );
    }

    #[test]
    fn canonicalizes_github_and_gitlab_transport_aliases() {
        assert_eq!(
            canonical_provider_host(Provider::GitHub, "ssh.github.com"),
            "github.com"
        );
        assert_eq!(
            canonical_provider_host(Provider::GitLab, "altssh.gitlab.com"),
            "gitlab.com"
        );
        assert_eq!(
            canonical_provider_host(Provider::GitLab, "altssh.gitlab.com:8443"),
            "gitlab.com:8443"
        );
        assert_eq!(
            canonical_provider_host(Provider::GitLab, "gitlab.example.com:8443"),
            "gitlab.example.com:8443"
        );
    }

    #[test]
    fn push_github_api_hostname_set_for_enterprise_host() {
        // A real GHE host is a distinct API host and keeps the override.
        let mut argv = Vec::new();
        github_ctx("internal.ghe.com").push_github_api_hostname(&mut argv);
        assert_eq!(
            argv,
            vec![
                OsString::from("--hostname"),
                OsString::from("internal.ghe.com")
            ]
        );
    }

    #[test]
    fn push_repo_override_qualifies_non_default_host() {
        let context = ProviderContext {
            provider: Provider::GitLab,
            host: "gitlab.example.com".into(),
            source: DetectionSource::Flag,
            repo: Some("group/project".into()),
        };
        let mut argv = Vec::new();
        context.push_repo_override(&mut argv);
        assert_eq!(
            argv,
            vec![
                OsString::from("--repo"),
                OsString::from("https://gitlab.example.com/group/project")
            ]
        );
    }

    #[test]
    fn push_repo_override_keeps_default_host_slug_unqualified() {
        let context = ProviderContext {
            provider: Provider::GitHub,
            host: "github.com".into(),
            source: DetectionSource::Flag,
            repo: Some("owner/repo".into()),
        };
        let mut argv = Vec::new();
        context.push_repo_override(&mut argv);
        assert_eq!(
            argv,
            vec![OsString::from("--repo"), OsString::from("owner/repo")]
        );
    }

    #[test]
    fn push_repo_override_canonicalizes_github_transport_alias() {
        let context = ProviderContext {
            provider: Provider::GitHub,
            host: "ssh.github.com".into(),
            source: DetectionSource::Remote,
            repo: Some("owner/repo".into()),
        };
        let mut argv = Vec::new();
        context.push_repo_override(&mut argv);
        assert_eq!(
            argv,
            vec![OsString::from("--repo"), OsString::from("owner/repo")]
        );
    }

    #[test]
    fn push_repo_override_uses_url_for_non_default_gitlab_host() {
        let context = ProviderContext {
            provider: Provider::GitLab,
            host: "gitlab.example.com".into(),
            source: DetectionSource::Flag,
            repo: Some("group/subgroup/project".into()),
        };
        let mut argv = Vec::new();
        context.push_repo_override(&mut argv);
        assert_eq!(
            argv,
            vec![
                OsString::from("--repo"),
                OsString::from("https://gitlab.example.com/group/subgroup/project")
            ]
        );
    }

    #[test]
    fn detect_explicit_host_is_independent_of_cwd_remote() {
        let context = detect(
            ProviderHint::ForcedHost(Provider::GitLab, "gitlab.example.com".into()),
            "origin",
            Some("group/project"),
            |_| Some("git@github.com:unrelated/checkout.git".into()),
        )
        .expect("explicit host binding");
        assert_eq!(context.provider, Provider::GitLab);
        assert_eq!(context.host, "gitlab.example.com");
        assert_eq!(context.repo.as_deref(), Some("group/project"));
        assert_eq!(context.source, DetectionSource::Flag);
    }

    #[test]
    fn detect_explicit_host_rejects_provider_mismatch() {
        let error = detect(
            ProviderHint::ForcedHost(Provider::GitHub, "gitlab.com".into()),
            "origin",
            Some("group/project"),
            |_| None,
        )
        .expect_err("mismatched explicit host");
        assert_eq!(error.kind(), "provider_unsupported");
    }

    #[test]
    fn detect_forced_host_accepts_valid_unclassified_authority() {
        let context = detect(
            ProviderHint::ForcedHost(Provider::GitLab, "forge.corp.example:8443".into()),
            "origin",
            Some("group/project"),
            |_| None,
        )
        .expect("explicit provider makes a valid custom authority unambiguous");
        assert_eq!(context.provider, Provider::GitLab);
        assert_eq!(context.host, "forge.corp.example:8443");
    }

    #[test]
    fn classify_host_does_not_trust_branding_like_prefix() {
        assert_eq!(classify_host("gitlab.attacker.example"), None);
    }

    #[test]
    fn detect_rejects_malformed_explicit_authorities() {
        for host in [
            "https://github.com",
            "github.com/path",
            "gitlab.com@attacker.example",
            "github.com?query",
            "github.com#fragment",
            " github.com",
            "github.com\nattacker.example",
            "github.com:not-a-port",
        ] {
            let error = detect(
                ProviderHint::ForcedHost(Provider::GitHub, host.into()),
                "origin",
                Some("owner/repo"),
                |_| None,
            )
            .expect_err("malformed authority must fail before backend execution");
            assert_eq!(error.kind(), "provider_unsupported", "host={host:?}");
        }
    }

    #[test]
    fn detect_explicit_host_with_port_preserves_authority() {
        let context = detect(
            ProviderHint::ForcedHost(Provider::GitHub, "internal.ghe.com:8443".into()),
            "origin",
            Some("owner/repo"),
            |_| None,
        )
        .expect("explicit authority with port");
        assert_eq!(context.host, "internal.ghe.com:8443");
        assert_eq!(
            context.repo_locator().as_deref(),
            Some("internal.ghe.com:8443/owner/repo")
        );
    }

    #[test]
    fn detect_explicit_host_requires_same_authority_remote_for_repo_derivation() {
        let error = detect(
            ProviderHint::ForcedHost(Provider::GitHub, "internal.ghe.com".into()),
            "origin",
            None,
            |_| Some("git@github.com:owner/repo.git".into()),
        )
        .expect_err("a different authority must not supply the repository path");
        assert_eq!(error.kind(), "repo_required");

        let context = detect(
            ProviderHint::ForcedHost(Provider::GitHub, "internal.ghe.com".into()),
            "origin",
            None,
            |_| Some("git@internal.ghe.com:owner/repo.git".into()),
        )
        .expect("same-authority remote may supply the repository path");
        assert_eq!(context.repo.as_deref(), Some("owner/repo"));
    }

    #[test]
    fn detect_unscoped_ignores_irrelevant_repo_shape() {
        let context = detect_unscoped(
            ProviderHint::Forced(Provider::GitHub),
            "origin",
            Some("owner/nested/repo"),
            |_| None,
        )
        .expect("repository-independent commands do not validate repository shape");
        assert_eq!(context.host, "github.com");
    }

    #[test]
    fn detect_forced_provider_unscoped_ignores_other_provider_remote() {
        let context = detect_unscoped(
            ProviderHint::Forced(Provider::GitHub),
            "origin",
            None,
            |_| Some("git@gitlab.com:unrelated/checkout.git".into()),
        )
        .expect("repository-independent provider selection must ignore an unrelated remote");
        assert_eq!(context.provider, Provider::GitHub);
        assert_eq!(context.host, "github.com");
        assert_eq!(context.source, DetectionSource::Flag);
    }

    #[test]
    fn detect_host_only_infers_provider_without_remote() {
        let context = detect(
            ProviderHint::Host("internal.ghe.com".into()),
            "origin",
            Some("owner/repo"),
            |_| None,
        )
        .expect("host-only binding");
        assert_eq!(context.provider, Provider::GitHub);
        assert_eq!(context.host, "internal.ghe.com");
        assert_eq!(context.repo.as_deref(), Some("owner/repo"));
        assert_eq!(context.source, DetectionSource::Flag);
    }

    #[test]
    fn parse_host_https() {
        assert_eq!(
            parse_host("https://github.com/sympoies/nils-cli.git"),
            Some("github.com".to_string())
        );
        assert_eq!(
            parse_host("https://internal.ghe.com:8443/sympoies/nils-cli.git"),
            Some("internal.ghe.com:8443".to_string())
        );
        assert_eq!(
            parse_host("https://github.com:443/sympoies/nils-cli.git"),
            Some("github.com".to_string())
        );
    }

    #[test]
    fn parse_host_ssh_form() {
        assert_eq!(
            parse_host("git@github.com:sympoies/nils-cli.git"),
            Some("github.com".to_string())
        );
    }

    #[test]
    fn parse_host_ssh_scheme() {
        assert_eq!(
            parse_host("ssh://git@gitlab.com:22/sympoies/nils-cli.git"),
            Some("gitlab.com".to_string())
        );
    }

    #[test]
    fn parse_host_rejects_empty() {
        assert_eq!(parse_host(""), None);
        assert_eq!(parse_host("   "), None);
    }

    #[test]
    fn parse_host_strips_basic_auth_userinfo_from_https() {
        assert_eq!(
            parse_host("https://user:pass@github.com/sympoies/nils-cli.git"),
            Some("github.com".to_string())
        );
        assert_eq!(
            parse_host("https://x-access-token:TOKEN@gitlab.com/group/proj.git"),
            Some("gitlab.com".to_string())
        );
    }

    #[test]
    fn parse_host_strips_userinfo_from_ssh_scheme_with_port() {
        assert_eq!(
            parse_host("ssh://deploy@gitlab.example.com:2222/group/proj.git"),
            Some("gitlab.example.com".to_string())
        );
    }

    #[test]
    fn parse_host_then_classify_recovers_provider_for_userinfo_remotes() {
        let host = parse_host("https://user:pass@github.com/owner/repo.git").expect("host");
        assert_eq!(classify_host(&host), Some(Provider::GitHub));
        let host = parse_host("ssh://deploy@internal.gitlab.com:22/group/proj.git").expect("host");
        assert_eq!(classify_host(&host), Some(Provider::GitLab));
    }

    #[test]
    fn parse_host_rejects_empty_after_userinfo() {
        assert_eq!(parse_host("https://user:pass@/owner/repo"), None);
        assert_eq!(parse_host("ssh://deploy@/owner/repo"), None);
    }

    #[test]
    fn detect_forced_provider_resolves_host_from_remote() {
        let ctx = detect(
            ProviderHint::Forced(Provider::GitLab),
            "origin",
            None,
            |_| Some("git@internal.gitlab.com:group/proj.git".to_string()),
        )
        .expect("forced provider");
        assert_eq!(ctx.provider, Provider::GitLab);
        assert_eq!(ctx.host, "internal.gitlab.com");
        assert_eq!(ctx.source, DetectionSource::Flag);
    }

    #[test]
    fn detect_forced_provider_falls_back_to_default_host_without_remote() {
        let ctx = detect(
            ProviderHint::Forced(Provider::GitLab),
            "origin",
            None,
            |_| None,
        )
        .expect("forced provider");
        assert_eq!(ctx.provider, Provider::GitLab);
        assert_eq!(ctx.host, "gitlab.com");
        assert_eq!(ctx.source, DetectionSource::Flag);
    }

    #[test]
    fn detect_forced_provider_with_explicit_repo_ignores_other_provider_remote() {
        let context = detect(
            ProviderHint::Forced(Provider::GitHub),
            "origin",
            Some("owner/repo"),
            |_| Some("git@gitlab.com:unrelated/checkout.git".into()),
        )
        .expect("an explicit slug binds the repository to the forced provider default host");
        assert_eq!(context.provider, Provider::GitHub);
        assert_eq!(context.host, "github.com");
        assert_eq!(context.repo.as_deref(), Some("owner/repo"));
        assert_eq!(context.source, DetectionSource::Flag);
    }

    #[test]
    fn detect_forced_provider_rejects_remote_host_of_other_provider() {
        let error = detect(
            ProviderHint::Forced(Provider::GitLab),
            "origin",
            None,
            |_| Some("git@github.com:owner/repo.git".to_string()),
        )
        .expect_err("forced provider must reject a conflicting remote authority");
        assert_eq!(error.kind(), "provider_unsupported");

        let error = detect(
            ProviderHint::Forced(Provider::GitHub),
            "origin",
            None,
            |_| Some("ssh://git@gitlab.example.com:22/group/proj.git".to_string()),
        )
        .expect_err("forced provider must reject an unclassified custom remote");
        assert_eq!(error.kind(), "provider_unsupported");
    }

    #[test]
    fn detect_forced_provider_rejects_unclassifiable_remote() {
        let error = detect(
            ProviderHint::Forced(Provider::GitLab),
            "origin",
            None,
            |_| Some("https://bitbucket.org/owner/repo.git".to_string()),
        )
        .expect_err("provider-only selection must fail closed on unsupported remotes");
        assert_eq!(error.kind(), "provider_unsupported");
    }

    #[test]
    fn detect_from_remote_url() {
        let ctx = detect(ProviderHint::Auto, "origin", None, |_| {
            Some("git@gitlab.com:owner/repo.git".to_string())
        })
        .expect("auto from remote");
        assert_eq!(ctx.provider, Provider::GitLab);
        assert_eq!(ctx.host, "gitlab.com");
        assert_eq!(ctx.source, DetectionSource::Remote);
    }

    #[test]
    fn detect_unknown_host_errors() {
        let err = detect(ProviderHint::Auto, "origin", None, |_| {
            Some("https://bitbucket.org/owner/repo.git".to_string())
        })
        .expect_err("unknown host");
        assert_eq!(err.kind(), "provider_unsupported");
    }

    #[test]
    fn detect_remote_errors_do_not_echo_credential_bearing_urls() {
        for url in [
            "https://alice:secret@/owner/repo.git",
            "https://alice:secret@bitbucket.org/owner/repo.git",
        ] {
            let error = detect(ProviderHint::Auto, "origin", None, |_| {
                Some(url.to_string())
            })
            .expect_err("credential-bearing unsupported remotes must fail closed");
            assert_eq!(error.kind(), "provider_unsupported");
            let ForgeError::ProviderUnsupported { schema_version, .. } = &error else {
                panic!("unexpected provider resolution error: {error:?}");
            };
            assert_eq!(schema_version, "cli.forge-cli.error.v1");
            assert_eq!(
                error.detail(),
                Some("selected remote URL omitted from diagnostics"),
                "url={url:?}"
            );
        }
    }

    #[test]
    fn detect_no_remote_errors() {
        let err = detect(ProviderHint::Auto, "origin", None, |_| None).expect_err("no remote");
        assert_eq!(err.kind(), "provider_unsupported");
    }

    #[test]
    fn detect_derives_repo_slug_from_remote_when_no_override() {
        // Without --repo, the repo slug must be derived from the same remote
        // used for host detection, so ops pin `--repo <slug>` to the cloned
        // repo instead of letting gh/glab silently default to a fork's upstream.
        let ctx = detect(ProviderHint::Auto, "origin", None, |_| {
            Some("git@github.com:sympoies/nils-cli.git".to_string())
        })
        .expect("auto from remote");
        assert_eq!(ctx.repo.as_deref(), Some("sympoies/nils-cli"));
    }

    #[test]
    fn detect_explicit_repo_override_wins_over_derived_slug() {
        let ctx = detect(ProviderHint::Auto, "origin", Some("acme/override"), |_| {
            Some("git@github.com:sympoies/nils-cli.git".to_string())
        })
        .expect("auto from remote");
        assert_eq!(ctx.repo.as_deref(), Some("acme/override"));
    }

    #[test]
    fn detect_rejects_nested_github_repository_shape() {
        let error = detect(
            ProviderHint::Forced(Provider::GitHub),
            "origin",
            Some("owner/subgroup/repo"),
            |_| None,
        )
        .expect_err("GitHub repositories must have exactly owner/name");
        assert_eq!(error.kind(), "repo_invalid");
    }

    #[test]
    fn detect_rejects_single_segment_gitlab_repository_shape() {
        let error = detect(
            ProviderHint::Forced(Provider::GitLab),
            "origin",
            Some("project"),
            |_| None,
        )
        .expect_err("GitLab repositories must include a namespace and project");
        assert_eq!(error.kind(), "repo_invalid");
    }

    #[test]
    fn detect_accepts_nested_gitlab_repository_shape() {
        let ctx = detect(
            ProviderHint::Forced(Provider::GitLab),
            "origin",
            Some("group/subgroup/project"),
            |_| None,
        )
        .expect("nested GitLab project path");
        assert_eq!(ctx.repo.as_deref(), Some("group/subgroup/project"));
    }

    #[test]
    fn detect_forced_provider_also_derives_repo_slug() {
        // Nested GitLab paths (group/subgroup/project) must survive intact for
        // `glab --repo`.
        let ctx = detect(
            ProviderHint::Forced(Provider::GitLab),
            "origin",
            None,
            |_| Some("git@internal.gitlab.com:group/sub/proj.git".to_string()),
        )
        .expect("forced provider");
        assert_eq!(ctx.repo.as_deref(), Some("group/sub/proj"));
    }

    #[test]
    fn detect_no_remote_leaves_repo_none() {
        // Graceful degradation: with no resolvable remote and no override, the
        // slug is None, so ops fall back to today's behavior (no --repo pinned).
        let ctx = detect(
            ProviderHint::Forced(Provider::GitHub),
            "origin",
            None,
            |_| None,
        )
        .expect("forced provider");
        assert_eq!(ctx.repo, None);
    }
}
