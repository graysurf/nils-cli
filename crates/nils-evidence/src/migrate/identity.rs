//! Derive the repo identity (host, org/group path, repo) for a skill-usage
//! rollup.
//!
//! ADAPTED, not a literal mirror of plan-archive's `identity.rs`. Plan-archive
//! derives identity from a git repo root via `origin`. Skill-usage records
//! live under the `agent-out` tree, **not** inside a git checkout, so the
//! authoritative key is the agent-out project directory name
//! `<owner__repo>` (the slug `AGENT_HOME` already encodes). We split that on
//! `__` and validate the host against `config/hosts.yaml`. Only when the dir
//! name is ambiguous (no `__`) do we fall back to resolving the record's
//! `cwd` to a git repo root and reading its `origin` remote.

use std::path::Path;

use serde::Serialize;

use nils_common::git::parse_git_remote_url;

use crate::validate::hosts::HostsConfig;

/// Repo identity captured for a rollup.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RepoIdentity {
    pub host: String,
    pub org: String,
    pub repo: String,
}

#[derive(Debug, thiserror::Error)]
pub enum IdentityError {
    #[error("could not derive repo identity from agent-out dir `{0}` or record cwd `{1}`")]
    Unresolvable(String, String),
    #[error(
        "--host `{0}` is not present in config/hosts.yaml; add it or drop the override to fall back to cwd->origin"
    )]
    HostNotConfigured(String),
    #[error(
        "--host `{0}` was supplied but the agent-out dir `{1}` is not an `<owner__repo>` slug, so org/repo cannot be derived"
    )]
    HostOverrideNeedsSlug(String, String),
    #[error("git command failed: {0}")]
    Io(String),
}

/// Resolve identity for a record.
///
/// `project_dir_name` is the agent-out project folder name (e.g.
/// `graysurf__agent-runtime-kit`). `hosts` is the archive's host config used
/// to disambiguate the host for an `<owner__repo>` slug (which carries no
/// host). `cwd` is the record's recorded working directory, used only for the
/// fallback path. `host_override` is the operator-supplied `--host` value: when
/// present and the slug resolves to `(org, repo)`, it pins the host directly
/// (after validating it against `hosts`), bypassing the multi-host cwd
/// ambiguity.
pub fn derive_repo_identity(
    project_dir_name: &str,
    hosts: &HostsConfig,
    cwd: &str,
    host_override: Option<&str>,
) -> Result<RepoIdentity, IdentityError> {
    let slug = split_owner_repo(project_dir_name);

    // (B) `--host` override: the operator vouches for the host of a slug-only
    // record whose `cwd` cannot be resolved. `--host` is GLOBAL, so it must not
    // clobber the authoritative `cwd -> origin` identity of a record that DOES
    // resolve (even to a different host) — otherwise rescuing one slug-only
    // record silently mis-attributes other records in the same batch. So the
    // override is a FALLBACK: try the record's own cwd first, and apply the
    // override only when cwd resolution fails. It still only applies to an
    // `<owner__repo>` slug, and the host must be present in config/hosts.yaml so
    // a typo is rejected rather than silently archived.
    if let Some(host) = host_override {
        if let Some(identity) = identity_from_cwd(cwd) {
            return Ok(identity);
        }
        let Some((org, repo)) = slug else {
            return Err(IdentityError::HostOverrideNeedsSlug(
                host.to_string(),
                project_dir_name.to_string(),
            ));
        };
        if !hosts.hosts.contains_key(host) {
            return Err(IdentityError::HostNotConfigured(host.to_string()));
        }
        return Ok(RepoIdentity {
            host: host.to_string(),
            org,
            repo,
        });
    }

    // F8: the agent-out `<owner__repo>` slug carries NO host. When more than
    // one host is configured we must not guess one (that mis-attributes an
    // employer repo to the first personal host) — the only trustworthy host
    // source is the record's own `cwd -> origin` remote. So prefer the cwd
    // derivation, falling back to the slug only when cwd resolution fails.
    if hosts.hosts.len() > 1 {
        if let Some(identity) = identity_from_cwd(cwd) {
            return Ok(identity);
        }
        // cwd could not be resolved; the slug alone cannot pin a host under a
        // multi-host config, so surface it as unresolvable rather than
        // silently guessing.
        return Err(IdentityError::Unresolvable(
            project_dir_name.to_string(),
            cwd.to_string(),
        ));
    }

    // Single-host (or empty) config. The record's own `cwd -> origin` is the
    // authoritative host signal, so prefer it even here: a record whose checkout
    // points at a DIFFERENT provider must be classified by its real host (and
    // blocked upstream if that host is absent from config/hosts.yaml), not
    // silently archived under the sole configured host. The slug -> sole-host
    // mapping is the fallback used only when cwd cannot be resolved.
    if let Some(identity) = identity_from_cwd(cwd) {
        return Ok(identity);
    }
    if let Some((org, repo)) = slug {
        let host = sole_host(hosts);
        return Ok(RepoIdentity { host, org, repo });
    }
    Err(IdentityError::Unresolvable(
        project_dir_name.to_string(),
        cwd.to_string(),
    ))
}

/// Split an agent-out `<owner__repo>` slug into `(org, repo)`. The slug uses a
/// double underscore between owner and repo; the repo half may itself contain
/// single underscores or hyphens.
pub fn split_owner_repo(slug: &str) -> Option<(String, String)> {
    let (owner, repo) = slug.split_once("__")?;
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some((owner.to_string(), repo.to_string()))
}

/// The host for a slug-only identity under a single-host (or empty) config:
/// the sole configured host, defaulting to `github.com` when the config is
/// empty. Only ever called when `hosts.len() <= 1`, so there is no guessing
/// across multiple hosts (see `derive_repo_identity`).
fn sole_host(hosts: &HostsConfig) -> String {
    hosts
        .hosts
        .keys()
        .next()
        .cloned()
        .unwrap_or_else(|| "github.com".to_string())
}

/// Resolve a repo identity from a local checkout path by reading its `origin`
/// remote. Returns `None` when the path is not a git repo or has no usable
/// `origin`. Exposed for the migrate `working_repo_roots` rescue path.
pub fn identity_from_cwd(cwd: &str) -> Option<RepoIdentity> {
    if cwd.is_empty() {
        return None;
    }
    let root = nils_common::git::repo_root_in(Path::new(cwd)).ok()??;
    let out = nils_common::git::run_output_in(&root, &["remote", "get-url", "origin"]).ok()?;
    if !out.status.success() {
        return None;
    }
    let url = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let parsed = parse_git_remote_url(&url)?;
    let mut segments: Vec<&str> = parsed.path.split('/').filter(|s| !s.is_empty()).collect();
    if segments.len() < 2 {
        return None;
    }
    let repo = segments.pop()?.to_string();
    let org = segments.join("/");
    if org.is_empty() {
        return None;
    }
    Some(RepoIdentity {
        host: parsed.host,
        org,
        repo,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::validate::hosts::validate_hosts_yaml;

    fn hosts(yaml: &str) -> HostsConfig {
        validate_hosts_yaml(yaml).expect("hosts").data.config
    }

    #[test]
    fn splits_owner_repo() {
        assert_eq!(
            split_owner_repo("graysurf__agent-runtime-kit"),
            Some(("graysurf".to_string(), "agent-runtime-kit".to_string()))
        );
    }

    #[test]
    fn split_owner_repo_handles_underscore_in_repo() {
        assert_eq!(
            split_owner_repo("acme__my_repo"),
            Some(("acme".to_string(), "my_repo".to_string()))
        );
    }

    #[test]
    fn split_owner_repo_rejects_no_separator() {
        assert!(split_owner_repo("localslug").is_none());
    }

    #[test]
    fn derive_from_agent_out_dir_single_host() {
        let h = hosts("version: 1\nhosts:\n  github.com:\n    class: personal\n");
        let id =
            derive_repo_identity("graysurf__agent-runtime-kit", &h, "/anywhere", None).unwrap();
        assert_eq!(id.host, "github.com");
        assert_eq!(id.org, "graysurf");
        assert_eq!(id.repo, "agent-runtime-kit");
    }

    #[test]
    fn multi_host_slug_only_does_not_guess_first_personal_host() {
        // F8: with more than one host configured and a slug-only identity
        // (the agent-out `<owner__repo>` dir carries no host) and no resolvable
        // cwd, we must NOT silently pick the first personal host — that would
        // mis-attribute an employer repo. The identity is unresolvable instead.
        let h = hosts(
            "version: 1\nhosts:\n  gitlab.example.com:\n    class: employer\n    employer: X\n  github.com:\n    class: personal\n",
        );
        let err = derive_repo_identity("graysurf__kit", &h, "", None).unwrap_err();
        assert!(
            matches!(err, IdentityError::Unresolvable(_, _)),
            "multi-host slug-only identity must not guess a host"
        );
    }

    #[test]
    fn unresolvable_when_no_separator_and_no_cwd() {
        let h = hosts("version: 1\nhosts:\n  github.com:\n    class: personal\n");
        let err = derive_repo_identity("localonly", &h, "", None).unwrap_err();
        assert!(matches!(err, IdentityError::Unresolvable(_, _)));
    }

    #[test]
    fn host_override_resolves_slug_only_under_multi_host() {
        // (B): with multiple hosts configured and an unresolvable cwd, an
        // operator-supplied `--host` present in the config pins the host
        // directly from the slug — no cwd derivation needed.
        let h = hosts(
            "version: 1\nhosts:\n  gitlab.example.com:\n    class: employer\n    employer: X\n  github.com:\n    class: personal\n",
        );
        let id = derive_repo_identity("graysurf__kit", &h, "", Some("github.com")).unwrap();
        assert_eq!(id.host, "github.com");
        assert_eq!(id.org, "graysurf");
        assert_eq!(id.repo, "kit");
    }

    #[test]
    fn host_override_rejects_host_absent_from_config() {
        let h = hosts("version: 1\nhosts:\n  github.com:\n    class: personal\n");
        let err = derive_repo_identity("graysurf__kit", &h, "", Some("nope.example")).unwrap_err();
        match err {
            IdentityError::HostNotConfigured(host) => assert_eq!(host, "nope.example"),
            other => panic!("expected HostNotConfigured, got {other:?}"),
        }
    }

    #[test]
    fn host_override_requires_a_slug() {
        // The override pins only the host; org/repo still come from the slug.
        // A dir name with no `__` cannot supply org/repo, so it is rejected.
        let h = hosts("version: 1\nhosts:\n  github.com:\n    class: personal\n");
        let err = derive_repo_identity("localonly", &h, "", Some("github.com")).unwrap_err();
        assert!(matches!(err, IdentityError::HostOverrideNeedsSlug(_, _)));
    }
}
