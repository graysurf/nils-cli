//! Local lifecycle mutation lock for provider issue comments.
//!
//! The lock is intentionally local and fail-fast: it prevents two agent
//! processes on the same machine from mutating the same lifecycle stream at
//! once, while leaving provider-level cross-machine coordination out of scope.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;

use crate::commands::record::RecordProfile;
use crate::provider::Repo;
use crate::{CommandError, state};

const LOCK_DIR: &str = "lifecycle-post";
const LOCK_BUSY_CODE: &str = "plan-issue-lifecycle-lock-busy";

#[derive(Debug)]
pub struct LifecycleMutationLock {
    path: PathBuf,
}

impl Drop for LifecycleMutationLock {
    fn drop(&mut self) {
        match fs::remove_file(&self.path) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(_) => {}
        }
    }
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

    let key = key_for(provider, host, repo_slug, issue, profile);
    let path = dir.join(format!("{key}.lock"));
    let payload = payload_for(provider, host, repo_slug, issue, profile);
    match OpenOptions::new().write(true).create_new(true).open(&path) {
        Ok(mut file) => {
            if let Err(err) = file.write_all(payload.as_bytes()) {
                let _ = fs::remove_file(&path);
                return Err(CommandError::runtime(
                    "plan-issue-lifecycle-lock-write-failed",
                    format!("failed to write lifecycle lock {}: {err}", path.display()),
                ));
            }
            if let Err(err) = file.flush() {
                let _ = fs::remove_file(&path);
                return Err(CommandError::runtime(
                    "plan-issue-lifecycle-lock-write-failed",
                    format!("failed to flush lifecycle lock {}: {err}", path.display()),
                ));
            }
            Ok(LifecycleMutationLock { path })
        }
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => Err(CommandError::runtime(
            LOCK_BUSY_CODE,
            format!(
                "another plan-issue lifecycle mutation is already in progress for provider={} repo={} issue={} profile={}; retry after it finishes or remove stale lock {} if the owning process exited",
                provider,
                repo_slug,
                issue,
                profile.as_str(),
                path.display(),
            ),
        )),
        Err(err) => Err(CommandError::runtime(
            "plan-issue-lifecycle-lock-acquire-failed",
            format!("failed to acquire lifecycle lock {}: {err}", path.display()),
        )),
    }
}

fn key_for(
    provider: &str,
    host: Option<&str>,
    repo_slug: &str,
    issue: u64,
    profile: RecordProfile,
) -> String {
    let host = host.unwrap_or("default");
    sanitize_key(&format!(
        "{}__{}__{}__issue-{}__{}",
        provider,
        host,
        repo_slug,
        issue,
        profile.as_str()
    ))
}

fn payload_for(
    provider: &str,
    host: Option<&str>,
    repo_slug: &str,
    issue: u64,
    profile: RecordProfile,
) -> String {
    format!(
        "provider={}\nhost={}\nrepo={}\nissue={}\nprofile={}\n",
        provider,
        host.unwrap_or("default"),
        repo_slug,
        issue,
        profile.as_str(),
    )
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
        let busy = acquire(&repo, 42, RecordProfile::Tracking).expect_err("second lock busy");
        assert_eq!(busy.code, LOCK_BUSY_CODE);

        acquire(&repo, 43, RecordProfile::Tracking).expect("different issue");
        acquire(&repo, 42, RecordProfile::Dispatch).expect("different profile");

        drop(first);
        acquire(&repo, 42, RecordProfile::Tracking).expect("lock released");
    }

    #[test]
    fn lifecycle_lock_key_is_path_safe() {
        let repo = Repo {
            provider: Provider::GitLab,
            slug: "group/sub/project".to_string(),
            host: Some("gitlab.example.com".to_string()),
        };
        let key = key_for(
            repo.provider.as_str(),
            repo.host.as_deref(),
            &repo.slug,
            7,
            RecordProfile::Dispatch,
        );
        assert!(!key.contains('/'));
        assert!(!key.contains('\\'));
        assert!(key.contains("gitlab"));
        assert!(key.contains("issue-7"));
    }
}
