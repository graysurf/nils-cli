//! Local lifecycle mutation lock for provider issue comments.
//!
//! The lock is intentionally local and fail-fast: it prevents two agent
//! processes on the same machine from mutating the same lifecycle stream at
//! once, while leaving provider-level cross-machine coordination out of scope.

use std::fs;

use plan_tooling::mutation_lock::{OwnedFileLock, OwnedFileLockError};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::commands::record::RecordProfile;
use crate::provider::{Provider, Repo};
use crate::{CommandError, state};

const LOCK_DIR: &str = "lifecycle-post";
const RECORD_OPEN_LOCK_DIR: &str = "record-open";
const LOCK_BUSY_CODE: &str = "plan-issue-lifecycle-lock-busy";

#[derive(Debug)]
pub struct RecordOpenReservation {
    _inner: OwnedFileLock,
}

#[derive(Debug)]
pub struct LifecycleMutationLock {
    _inner: OwnedFileLock,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordOpenIdentityKey {
    pub provider: String,
    pub authority: String,
    pub repo_slug: String,
    pub profile: String,
    pub source_path: String,
    pub source_commit: String,
}

pub fn record_open_identity_key(
    repo: &Repo,
    profile: RecordProfile,
    source_path: &str,
    source_commit: &str,
) -> RecordOpenIdentityKey {
    RecordOpenIdentityKey {
        provider: repo.provider.as_str().to_string(),
        authority: canonical_lock_host(repo.provider.as_str(), repo.host.as_deref()),
        repo_slug: canonical_lock_slug(repo.provider.as_str(), &repo.slug),
        profile: profile.as_str().to_string(),
        source_path: source_path.to_string(),
        source_commit: source_commit.to_string(),
    }
}

pub fn record_open_key(identity: &RecordOpenIdentityKey) -> String {
    let mut digest = Sha256::new();
    for component in [
        identity.provider.as_str(),
        identity.authority.as_str(),
        identity.repo_slug.as_str(),
        identity.profile.as_str(),
        identity.source_path.as_str(),
        identity.source_commit.as_str(),
    ] {
        digest.update((component.len() as u64).to_be_bytes());
        digest.update(component.as_bytes());
    }
    let digest = digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!(
        "{}__{}__{}",
        sanitize_key(&identity.provider),
        sanitize_key(&identity.profile),
        digest
    )
}

/// Acquire the local lock for one provider issue lifecycle stream.
pub fn acquire(
    repo: &Repo,
    issue: u64,
    profile: RecordProfile,
) -> Result<LifecycleMutationLock, CommandError> {
    acquire_for_identity(
        repo.provider.as_str(),
        repo.host.as_deref(),
        &repo.slug,
        issue,
        profile,
    )
}

/// Acquire the local lock from a provider/repo identity.
///
/// This is public so integration tests and future provider-neutral callers do
/// not need access to the internal provider-routing module.
pub fn acquire_for_identity(
    provider: &str,
    host: Option<&str>,
    repo_slug: &str,
    issue: u64,
    profile: RecordProfile,
) -> Result<LifecycleMutationLock, CommandError> {
    let dir = state::state_dir().join("locks").join(LOCK_DIR);
    fs::create_dir_all(&dir).map_err(|err| {
        CommandError::runtime(
            "plan-issue-lifecycle-lock-dir-failed",
            format!(
                "failed to create lifecycle lock directory {}: {err}",
                dir.display()
            ),
        )
    })?;

    let key = key_for(provider, host, repo_slug, issue);
    let path = dir.join(format!("{key}.lock"));
    match OwnedFileLock::acquire(&path) {
        Ok(inner) => Ok(LifecycleMutationLock { _inner: inner }),
        Err(OwnedFileLockError::Busy) => Err(CommandError::runtime(
            LOCK_BUSY_CODE,
            format!(
                "another plan-issue lifecycle mutation is already in progress for provider={} repo={} issue={} profile={}; retry after it finishes (the kernel releases lock {} when its process exits)",
                provider,
                repo_slug,
                issue,
                profile.as_str(),
                path.display(),
            ),
        )),
        Err(OwnedFileLockError::Failed(err)) => Err(CommandError::runtime(
            "plan-issue-lifecycle-lock-acquire-failed",
            format!("failed to acquire lifecycle lock {}: {err}", path.display()),
        )),
    }
}

pub fn acquire_record_open(
    repo: &Repo,
    profile: RecordProfile,
    source_path: &str,
    source_commit: &str,
) -> Result<RecordOpenReservation, CommandError> {
    let dir = state::state_dir().join("locks").join(RECORD_OPEN_LOCK_DIR);
    fs::create_dir_all(&dir).map_err(|err| {
        CommandError::runtime(
            "plan-issue-record-open-lock-dir-failed",
            format!(
                "failed to create record-open lock directory {}: {err}",
                dir.display()
            ),
        )
    })?;

    let identity = record_open_identity_key(repo, profile, source_path, source_commit);
    let key = record_open_key(&identity);
    let path = dir.join(format!("{key}.lock"));
    match OwnedFileLock::acquire(&path) {
        Ok(inner) => Ok(RecordOpenReservation { _inner: inner }),
        Err(OwnedFileLockError::Busy) => Err(CommandError::runtime(
            "plan-issue-record-open-lock-busy",
            format!(
                "another record-open operation is already resolving this bundle for provider={} repo={} profile={}; retry after it finishes",
                repo.provider.as_str(),
                repo.slug,
                profile.as_str(),
            ),
        )),
        Err(OwnedFileLockError::Failed(err)) => Err(CommandError::runtime(
            "plan-issue-record-open-lock-acquire-failed",
            format!(
                "failed to acquire record-open lock {}: {err}",
                path.display()
            ),
        )),
    }
}

fn canonical_lock_host(provider: &str, host: Option<&str>) -> String {
    match (provider, host) {
        ("github", Some(host)) => crate::provider::canonical_provider_host(Provider::GitHub, host),
        ("gitlab", Some(host)) => crate::provider::canonical_provider_host(Provider::GitLab, host),
        ("github", None) => "github.com".to_string(),
        ("gitlab", None) => "gitlab.com".to_string(),
        ("local", None) => "local".to_string(),
        (_, Some(host)) => host.to_string(),
        (_, None) => "default".to_string(),
    }
}

fn canonical_lock_slug(provider: &str, repo_slug: &str) -> String {
    if provider.eq_ignore_ascii_case("github") {
        repo_slug.to_ascii_lowercase()
    } else {
        repo_slug.to_string()
    }
}

fn key_for(provider: &str, host: Option<&str>, repo_slug: &str, issue: u64) -> String {
    let host = canonical_lock_host(provider, host);
    let repo_slug = canonical_lock_slug(provider, repo_slug);
    let mut digest = Sha256::new();
    for component in [provider, host.as_str(), repo_slug.as_str()] {
        digest.update((component.len() as u64).to_be_bytes());
        digest.update(component.as_bytes());
    }
    digest.update(issue.to_be_bytes());
    let digest = digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("{}__issue-{}__{}", sanitize_key(provider), issue, digest)
}

fn sanitize_key(raw: &str) -> String {
    raw.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{Provider, Repo};
    use nils_test_support::{EnvGuard, GlobalStateLock};
    use tempfile::TempDir;

    fn repo() -> Repo {
        Repo {
            provider: Provider::GitHub,
            slug: "owner/repo".to_string(),
            host: Some("github.com".to_string()),
        }
    }

    fn isolate_state(lock: &GlobalStateLock) -> (TempDir, EnvGuard) {
        crate::state::set_state_dir_override(None);
        let tmp = TempDir::new().expect("tmp");
        let path = tmp.path().to_string_lossy().to_string();
        let guard = EnvGuard::set(lock, "PLAN_ISSUE_HOME", &path);
        (tmp, guard)
    }

    #[test]
    fn lifecycle_lock_is_issue_scoped_and_released_on_drop() {
        let lock = GlobalStateLock::new();
        let (_state, _env) = isolate_state(&lock);
        let repo = repo();

        let first = acquire(&repo, 42, RecordProfile::Tracking).expect("first lock");
        let path = state::state_dir()
            .join("locks")
            .join(LOCK_DIR)
            .join(format!(
                "{}.lock",
                key_for(repo.provider.as_str(), repo.host.as_deref(), &repo.slug, 42,)
            ));
        let busy = acquire(&repo, 42, RecordProfile::Tracking).expect_err("second lock busy");
        assert_eq!(busy.code, LOCK_BUSY_CODE);

        acquire(&repo, 43, RecordProfile::Tracking).expect("different issue");
        let cross_profile = acquire(&repo, 42, RecordProfile::Dispatch)
            .expect_err("different profiles must share one issue mutation lock");
        assert_eq!(cross_profile.code, LOCK_BUSY_CODE);

        drop(first);
        assert!(path.exists(), "advisory lifecycle lock path must persist");
        acquire(&repo, 42, RecordProfile::Tracking).expect("lock released");
    }

    #[test]
    fn lifecycle_lock_transport_aliases_contend_on_one_identity() {
        let lock = GlobalStateLock::new();
        let (_state, _env) = isolate_state(&lock);
        let cases = [
            ("github", "github.com", "ssh.github.com"),
            ("gitlab", "gitlab.com", "altssh.gitlab.com"),
        ];

        for (provider, canonical, transport_alias) in cases {
            let held = acquire_for_identity(
                provider,
                Some(canonical),
                "owner/repo",
                42,
                RecordProfile::Tracking,
            )
            .expect("canonical lock");
            let busy = acquire_for_identity(
                provider,
                Some(transport_alias),
                "owner/repo",
                42,
                RecordProfile::Tracking,
            )
            .expect_err("transport alias must contend with canonical authority");
            assert_eq!(busy.code, LOCK_BUSY_CODE, "{provider}");
            drop(held);
        }
    }

    #[test]
    fn github_slug_case_aliases_share_lifecycle_and_record_open_locks() {
        let lock = GlobalStateLock::new();
        let (_state, _env) = isolate_state(&lock);
        let mixed_case = Repo {
            provider: Provider::GitHub,
            slug: "Owner/Repo".to_string(),
            host: Some("github.com".to_string()),
        };
        let lower_case = Repo {
            slug: "owner/repo".to_string(),
            ..mixed_case.clone()
        };

        let held =
            acquire(&mixed_case, 42, RecordProfile::Tracking).expect("mixed-case lifecycle lock");
        let busy = acquire(&lower_case, 42, RecordProfile::Dispatch)
            .expect_err("GitHub slug case aliases must contend");
        assert_eq!(busy.code, LOCK_BUSY_CODE);
        drop(held);

        let held = acquire_record_open(
            &mixed_case,
            RecordProfile::Tracking,
            "docs/plans/example/source.md",
            "abc123",
        )
        .expect("mixed-case record-open lock");
        let busy = acquire_record_open(
            &lower_case,
            RecordProfile::Tracking,
            "docs/plans/example/source.md",
            "abc123",
        )
        .expect_err("GitHub slug case aliases must share record-open reservation");
        assert_eq!(busy.code, "plan-issue-record-open-lock-busy");
        drop(held);
    }

    #[test]
    fn gitlab_slug_case_variants_remain_distinct_lock_identities() {
        let lock = GlobalStateLock::new();
        let (_state, _env) = isolate_state(&lock);

        let held = acquire_for_identity(
            "gitlab",
            Some("gitlab.com"),
            "Owner/Repo",
            42,
            RecordProfile::Tracking,
        )
        .expect("mixed-case GitLab lock");
        acquire_for_identity(
            "gitlab",
            Some("gitlab.com"),
            "owner/repo",
            42,
            RecordProfile::Tracking,
        )
        .expect("GitLab slug case variants are distinct");
        drop(held);
    }

    #[test]
    fn lifecycle_lock_default_and_explicit_hosts_contend() {
        let lock = GlobalStateLock::new();
        let (_state, _env) = isolate_state(&lock);

        for (provider, default_host) in [("github", "github.com"), ("gitlab", "gitlab.com")] {
            let held =
                acquire_for_identity(provider, None, "owner/repo", 42, RecordProfile::Tracking)
                    .expect("implicit default host lock");
            let busy = acquire_for_identity(
                provider,
                Some(default_host),
                "owner/repo",
                42,
                RecordProfile::Dispatch,
            )
            .expect_err("explicit default host must contend across profiles");
            assert_eq!(busy.code, LOCK_BUSY_CODE, "{provider}");
            drop(held);
        }
    }

    #[test]
    fn lifecycle_lock_distinguishes_sanitized_slug_collisions() {
        let lock = GlobalStateLock::new();
        let (_state, _env) = isolate_state(&lock);
        let held = acquire_for_identity(
            "github",
            Some("github.com"),
            "a/b_c",
            42,
            RecordProfile::Tracking,
        )
        .expect("first repository lock");

        let distinct = acquire_for_identity(
            "github",
            Some("github.com"),
            "a_b/c",
            42,
            RecordProfile::Tracking,
        )
        .expect("distinct repository lock");

        drop(distinct);
        drop(held);
    }

    #[test]
    fn record_open_reservation_serializes_one_bundle_identity() {
        let lock = GlobalStateLock::new();
        let (_state, _env) = isolate_state(&lock);
        let repo = repo();

        let held = acquire_record_open(
            &repo,
            RecordProfile::Tracking,
            "docs/plans/example/source.md",
            "abc123",
        )
        .expect("first reservation");
        let busy = acquire_record_open(
            &repo,
            RecordProfile::Tracking,
            "docs/plans/example/source.md",
            "abc123",
        )
        .expect_err("same bundle reservation must contend");
        assert_eq!(busy.code, "plan-issue-record-open-lock-busy");

        acquire_record_open(
            &repo,
            RecordProfile::Tracking,
            "docs/plans/example/source.md",
            "def456",
        )
        .expect("different source commit");
        drop(held);
        acquire_record_open(
            &repo,
            RecordProfile::Tracking,
            "docs/plans/example/source.md",
            "abc123",
        )
        .expect("reservation released");
    }

    #[test]
    fn lifecycle_lock_key_is_path_safe() {
        let repo = Repo {
            provider: Provider::GitLab,
            slug: "group/sub/project".to_string(),
            host: Some("gitlab.example.com".to_string()),
        };
        let key = key_for(repo.provider.as_str(), repo.host.as_deref(), &repo.slug, 7);
        assert!(!key.contains('/'));
        assert!(!key.contains('\\'));
        assert!(key.contains("gitlab"));
        assert!(key.contains("issue-7"));
    }
}
