use super::git::Git;
use super::preflight::{RepositoryState, cached_relation_counts, verify_identity_after};

#[derive(Debug)]
pub(super) struct Postconditions {
    pub(super) parent: String,
    pub(super) tree: String,
    pub(super) relation_after: &'static str,
}

pub(super) fn verify(state: &RepositoryState, new_head: &str) -> Result<Postconditions, String> {
    let git = Git::at(state.root.clone());
    verify_identity_after(state)?;
    if git.stdout(["rev-parse", "--verify", "HEAD^{commit}"])? != new_head {
        return Err("HEAD changed during postcondition verification".to_string());
    }
    let parent = git.stdout(["rev-parse", "--verify", "HEAD^1^{commit}"])?;
    if parent != state.head {
        return Err("created commit parent does not match --expect-head".to_string());
    }
    let parent_count = git
        .stdout(["rev-list", "--parents", "-n", "1", "HEAD"])?
        .split_whitespace()
        .count()
        .saturating_sub(1);
    if parent_count != 1 {
        return Err("created commit must have exactly one parent".to_string());
    }
    let status = git.stdout_allow_empty(["status", "--porcelain", "--untracked-files=all"])?;
    if !status.is_empty() {
        return Err("worktree or index is not clean after commit".to_string());
    }
    git.run(["verify-commit", "HEAD"])
        .map_err(|_| "created commit signature verification failed".to_string())?;
    if git.stdout(["log", "-1", "--format=%G?", "HEAD"])? != "G" {
        return Err("created commit signature is not locally verified-good".to_string());
    }
    let relation_after = match state.remote.upstream_sha.as_deref() {
        Some(upstream_sha) => {
            let (behind, ahead) = cached_relation_counts(&git, upstream_sha, new_head)?;
            if behind != 0 || ahead != 1 {
                return Err("cached upstream relation after commit is not ahead-by-one".to_string());
            }
            "ahead-by-one"
        }
        None => "untracked",
    };
    let tree = git.stdout(["rev-parse", "--verify", "HEAD^{tree}"])?;
    Ok(Postconditions {
        parent,
        tree,
        relation_after,
    })
}
