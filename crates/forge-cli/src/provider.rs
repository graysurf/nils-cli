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

use crate::cli::BINARY;
use crate::error::ForgeError;

/// Two-provider enum used everywhere downstream of detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    GitHub,
    GitLab,
}

impl Provider {
    /// Stable lower-case rendering used in envelopes (`data.provider`).
    pub fn as_str(self) -> &'static str {
        match self {
            Provider::GitHub => "github",
            Provider::GitLab => "gitlab",
        }
    }
}

/// Hint from the CLI layer: caller may force a provider via `--provider`, or
/// leave it on auto-detect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderHint {
    Auto,
    Forced(Provider),
}

/// Resolved provider context handed to every op.
#[derive(Debug, Clone)]
pub struct ProviderContext {
    pub provider: Provider,
    pub host: String,
    pub source: DetectionSource,
    /// Repo slug from `--repo owner/name`; ops push `--repo <slug>` into the
    /// backend argv when set so dry-run plans and live calls hit the same repo.
    pub repo: Option<String>,
}

impl ProviderContext {
    /// Push `--repo <owner/name>` into `argv` when the caller passed
    /// `--repo`. Both `gh` and `glab` accept the long form across all
    /// non-`repo view` subcommands, so this helper centralizes the wiring.
    /// `repo view` does not use this — it pushes the slug as a positional
    /// argument instead.
    pub fn push_repo_override(&self, argv: &mut Vec<OsString>) {
        if let Some(slug) = self.repo.as_deref() {
            argv.push(OsString::from("--repo"));
            argv.push(OsString::from(slug));
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

/// Detect the provider using the spec's ladder.
///
/// `remote_url_lookup` is injected so tests can stub it without spawning git.
pub fn detect(
    hint: ProviderHint,
    remote: &str,
    repo_override: Option<&str>,
    remote_url_lookup: impl Fn(&str) -> Option<String>,
) -> Result<ProviderContext, ForgeError> {
    let repo = repo_override.map(str::to_string);

    if let ProviderHint::Forced(provider) = hint {
        return Ok(ProviderContext {
            provider,
            host: default_host_for(provider).to_string(),
            source: DetectionSource::Flag,
            repo,
        });
    }

    let url = remote_url_lookup(remote);
    if let Some(url) = url.as_deref()
        && let Some(host) = parse_host(url)
    {
        if let Some(provider) = classify_host(&host) {
            return Ok(ProviderContext {
                provider,
                host,
                source: DetectionSource::Remote,
                repo,
            });
        }
        return Err(ForgeError::provider_unsupported(
            schema(),
            format!("unsupported forge host: {host}"),
            Some(format!("remote_url={url}")),
        ));
    }

    Err(ForgeError::provider_unsupported(
        schema(),
        "no remote URL available and no --provider override supplied",
        None,
    ))
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
    }
}

/// Classify a host string against the v1 backend table. `*.gitlab.<corp>` is
/// allowed as a GitLab self-hosted shape.
pub fn classify_host(host: &str) -> Option<Provider> {
    let host = host.trim().to_ascii_lowercase();
    if host == "github.com" || host.ends_with(".github.com") || host.ends_with(".ghe.com") {
        return Some(Provider::GitHub);
    }
    if host == "gitlab.com" || host.ends_with(".gitlab.com") || host.starts_with("gitlab.") {
        return Some(Provider::GitLab);
    }
    None
}

/// Parse the host out of a remote URL. Supports:
/// - `https://[userinfo@]<host>/<owner>/<repo>(.git)?`
/// - `ssh://[user@]<host>(:port)?/<owner>/<repo>(.git)?`
/// - `git@<host>:<owner>/<repo>(.git)?`
pub fn parse_host(url: &str) -> Option<String> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(rest) = trimmed.strip_prefix("https://") {
        return host_before_path(rest)
            .map(|host| nils_common::git::strip_userinfo(&host).to_string())
            .filter(|host| !host.is_empty());
    }
    if let Some(rest) = trimmed.strip_prefix("ssh://") {
        // ssh://[user@]host[:port]/owner/repo
        let after_user = nils_common::git::strip_userinfo(rest);
        return host_before_path(after_user)
            .map(strip_port)
            .filter(|host| !host.is_empty());
    }
    if let Some((user_host, _)) = trimmed.split_once(':')
        && let Some((_, host)) = user_host.rsplit_once('@')
        && !host.is_empty()
    {
        return Some(host.to_string());
    }
    None
}

fn host_before_path(s: &str) -> Option<String> {
    let host = s.split('/').next()?.trim();
    if host.is_empty() {
        None
    } else {
        Some(host.to_string())
    }
}

fn strip_port(host: String) -> String {
    match host.split_once(':') {
        Some((h, _)) => h.to_string(),
        None => host,
    }
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
    fn classify_host_recognises_self_hosted_gitlab() {
        assert_eq!(classify_host("gitlab.example.com"), Some(Provider::GitLab));
    }

    #[test]
    fn classify_host_rejects_unknown() {
        assert_eq!(classify_host("bitbucket.org"), None);
        assert_eq!(classify_host("codeberg.org"), None);
    }

    #[test]
    fn parse_host_https() {
        assert_eq!(
            parse_host("https://github.com/sympoies/nils-cli.git"),
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
        let host = parse_host("ssh://deploy@gitlab.example.com:22/group/proj.git").expect("host");
        assert_eq!(classify_host(&host), Some(Provider::GitLab));
    }

    #[test]
    fn parse_host_rejects_empty_after_userinfo() {
        assert_eq!(parse_host("https://user:pass@/owner/repo"), None);
        assert_eq!(parse_host("ssh://deploy@/owner/repo"), None);
    }

    #[test]
    fn detect_with_flag_does_not_consult_remote() {
        let counter = std::cell::Cell::new(0_u32);
        let lookup = |_: &str| -> Option<String> {
            counter.set(counter.get() + 1);
            None
        };
        let ctx = detect(
            ProviderHint::Forced(Provider::GitHub),
            "origin",
            None,
            lookup,
        )
        .expect("forced provider");
        assert_eq!(ctx.provider, Provider::GitHub);
        assert_eq!(ctx.source, DetectionSource::Flag);
        assert_eq!(counter.get(), 0, "remote lookup must not run when forced");
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
    fn detect_no_remote_errors() {
        let err = detect(ProviderHint::Auto, "origin", None, |_| None).expect_err("no remote");
        assert_eq!(err.kind(), "provider_unsupported");
    }
}
