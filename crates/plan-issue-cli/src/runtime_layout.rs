//! Canonical runtime layout for plan-issue artifacts.
//!
//! Path math derived from
//! `agent-kit/skills/automation/plan-issue-delivery/references/RUNTIME_LAYOUT.md`.
//! See also `docs/specs/plan-issue-cli-contract-v2.md` "Canonical Runtime
//! Artifacts (v2)".

use std::error::Error;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use nils_common::env as common_env;

const RUNTIME_DIR: &str = "out";
const PLAN_ISSUE_DELIVERY_DIR: &str = "plan-issue-delivery";
const ISSUE_PREFIX: &str = "issue-";
const SPRINT_PREFIX: &str = "sprint-";
const PROMPTS_DIR: &str = "prompts";
const PLAN_DIR: &str = "plan";
const SPECS_DIR: &str = "specs";
const MANIFESTS_DIR: &str = "manifests";
const WORKTREES_DIR: &str = "worktrees";
const MAIN_AGENT_INIT_FILE: &str = "plan-issue-delivery-main-agent-init.snapshot.md";
const SUBAGENT_INIT_FILE: &str = "plan-issue-delivery-subagent-init.snapshot.md";
const PLAN_SNAPSHOT_FILE: &str = "plan.snapshot.md";
const PLAN_BRANCH_REF_FILE: &str = "plan-branch.ref";
const PROMPT_MANIFEST_FILE: &str = "prompt-manifest.tsv";
const SPRINT_TASK_SPEC_FILE: &str = "sprint-task-spec.tsv";

/// Errors emitted by canonical runtime-layout helpers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeLayoutError {
    /// `AGENT_HOME` env var is missing or empty.
    AgentHomeNotSet,
    /// Repo slug is empty or contains a path separator after substitution.
    InvalidRepoSlug { slug: String },
    /// Task id is empty or contains a path separator.
    InvalidTaskId { task_id: String },
}

impl fmt::Display for RuntimeLayoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AgentHomeNotSet => write!(
                f,
                "AGENT_HOME env var is not set; canonical runtime layout requires AGENT_HOME"
            ),
            Self::InvalidRepoSlug { slug } => {
                write!(f, "invalid repo slug `{slug}` for runtime layout")
            }
            Self::InvalidTaskId { task_id } => {
                write!(f, "invalid task id `{task_id}` for runtime layout")
            }
        }
    }
}

impl Error for RuntimeLayoutError {}

/// Resolve `RUNTIME_ROOT="$AGENT_HOME/out/plan-issue-delivery"`.
pub fn runtime_root() -> Result<PathBuf, RuntimeLayoutError> {
    let agent_home =
        common_env::env_non_empty("AGENT_HOME").ok_or(RuntimeLayoutError::AgentHomeNotSet)?;
    Ok(PathBuf::from(agent_home)
        .join(RUNTIME_DIR)
        .join(PLAN_ISSUE_DELIVERY_DIR))
}

/// Convert `owner/repo` to `owner__repo`.
pub fn repo_slug(owner_repo: &str) -> String {
    owner_repo.trim().replace('/', "__")
}

/// Create a directory and all parents (idempotent).
pub fn ensure_dir(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)
}

/// Issue-scoped runtime root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueRoot {
    root: PathBuf,
}

impl IssueRoot {
    /// Compute `$RUNTIME_ROOT/<repo-slug>/issue-<issue_number>`.
    pub fn new(repo_slug: &str, issue_number: u64) -> Result<Self, RuntimeLayoutError> {
        let trimmed = repo_slug.trim();
        if trimmed.is_empty()
            || trimmed.contains('/')
            || trimmed.contains('\\')
            || trimmed.contains('\0')
        {
            return Err(RuntimeLayoutError::InvalidRepoSlug {
                slug: repo_slug.to_string(),
            });
        }
        let runtime = runtime_root()?;
        let root = runtime
            .join(trimmed)
            .join(format!("{ISSUE_PREFIX}{issue_number}"));
        Ok(Self { root })
    }

    /// `$ISSUE_ROOT`.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `$ISSUE_ROOT/prompts/plan-issue-delivery-main-agent-init.snapshot.md`.
    pub fn main_agent_init_snapshot(&self) -> PathBuf {
        self.root.join(PROMPTS_DIR).join(MAIN_AGENT_INIT_FILE)
    }

    /// `$ISSUE_ROOT/plan/plan.snapshot.md`.
    pub fn plan_snapshot(&self) -> PathBuf {
        self.root.join(PLAN_DIR).join(PLAN_SNAPSHOT_FILE)
    }

    /// `$ISSUE_ROOT/plan/plan-branch.ref`.
    pub fn plan_branch_ref(&self) -> PathBuf {
        self.root.join(PLAN_DIR).join(PLAN_BRANCH_REF_FILE)
    }

    /// Plan-scope task-spec TSV at `$ISSUE_ROOT/plan/tasks.tsv`.
    pub fn plan_task_spec(&self) -> PathBuf {
        self.root.join(PLAN_DIR).join("tasks.tsv")
    }

    /// Plan-scope rendered issue body at `$ISSUE_ROOT/plan/issue-body.md`.
    pub fn plan_issue_body(&self) -> PathBuf {
        self.root.join(PLAN_DIR).join("issue-body.md")
    }

    /// `$ISSUE_ROOT/worktrees`.
    pub fn worktree_root(&self) -> PathBuf {
        self.root.join(WORKTREES_DIR)
    }

    /// Canonical assigned-worktree path for one task.
    ///
    /// Per `RUNTIME_LAYOUT.md` "Worktree Layout (Assigned Paths)":
    ///
    /// - `pr-isolated` → `$WORKTREE_ROOT/pr-isolated/<TASK_ID>`
    /// - `pr-shared`   → `$WORKTREE_ROOT/pr-shared/<PR_GROUP>`
    /// - `per-sprint`  → `$WORKTREE_ROOT/per-sprint/sprint-<N>`
    ///
    /// Unknown `execution_mode` falls back to the `pr-isolated` shape so
    /// the dispatch record always names an absolute path under
    /// `WORKTREE_ROOT`.
    pub fn assigned_worktree(
        &self,
        execution_mode: &str,
        task_id: &str,
        pr_group: &str,
        sprint: i32,
    ) -> Result<PathBuf, RuntimeLayoutError> {
        let trim_segment = |seg: &str| -> Result<String, RuntimeLayoutError> {
            let t = seg.trim();
            if t.is_empty() || t.contains('/') || t.contains('\\') || t.contains('\0') {
                return Err(RuntimeLayoutError::InvalidTaskId {
                    task_id: seg.to_string(),
                });
            }
            Ok(t.to_string())
        };
        let root = self.worktree_root();
        match execution_mode {
            "pr-shared" => Ok(root.join("pr-shared").join(trim_segment(pr_group)?)),
            "per-sprint" => Ok(root.join("per-sprint").join(format!("sprint-{sprint}"))),
            _ => Ok(root.join("pr-isolated").join(trim_segment(task_id)?)),
        }
    }
}

/// Sprint-scoped runtime root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SprintRoot {
    root: PathBuf,
}

impl SprintRoot {
    /// Compute `$ISSUE_ROOT/sprint-<n>`.
    pub fn new(issue: &IssueRoot, sprint: i32) -> Self {
        let root = issue.root().join(format!("{SPRINT_PREFIX}{sprint}"));
        Self { root }
    }

    /// `$SPRINT_ROOT`.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `$SPRINT_ROOT/prompts`.
    pub fn prompts_dir(&self) -> PathBuf {
        self.root.join(PROMPTS_DIR)
    }

    /// `$SPRINT_ROOT/manifests`.
    pub fn manifests_dir(&self) -> PathBuf {
        self.root.join(MANIFESTS_DIR)
    }

    /// `$SPRINT_ROOT/specs`.
    pub fn specs_dir(&self) -> PathBuf {
        self.root.join(SPECS_DIR)
    }

    /// `$SPRINT_ROOT/prompts/plan-issue-delivery-subagent-init.snapshot.md`.
    pub fn subagent_init_snapshot(&self) -> PathBuf {
        self.prompts_dir().join(SUBAGENT_INIT_FILE)
    }

    /// `$SPRINT_ROOT/prompts/<TASK_ID>.md`.
    pub fn task_prompt(&self, task_id: &str) -> Result<PathBuf, RuntimeLayoutError> {
        let trimmed = task_id.trim();
        if trimmed.is_empty()
            || trimmed.contains('/')
            || trimmed.contains('\\')
            || trimmed.contains('\0')
        {
            return Err(RuntimeLayoutError::InvalidTaskId {
                task_id: task_id.to_string(),
            });
        }
        Ok(self.prompts_dir().join(format!("{trimmed}.md")))
    }

    /// `$SPRINT_ROOT/manifests/prompt-manifest.tsv`.
    pub fn prompt_manifest(&self) -> PathBuf {
        self.manifests_dir().join(PROMPT_MANIFEST_FILE)
    }

    /// `$SPRINT_ROOT/specs/sprint-task-spec.tsv`.
    pub fn task_spec(&self) -> PathBuf {
        self.specs_dir().join(SPRINT_TASK_SPEC_FILE)
    }

    /// `$SPRINT_ROOT/manifests/dispatch-<TASK_ID>.json`.
    pub fn dispatch_record(&self, task_id: &str) -> Result<PathBuf, RuntimeLayoutError> {
        let trimmed = task_id.trim();
        if trimmed.is_empty()
            || trimmed.contains('/')
            || trimmed.contains('\\')
            || trimmed.contains('\0')
        {
            return Err(RuntimeLayoutError::InvalidTaskId {
                task_id: task_id.to_string(),
            });
        }
        Ok(self
            .manifests_dir()
            .join(format!("dispatch-{trimmed}.json")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nils_test_support::{EnvGuard, GlobalStateLock};

    fn issue_root_for(repo: &str, issue: u64) -> IssueRoot {
        IssueRoot::new(repo, issue).expect("issue root")
    }

    #[test]
    fn test_runtime_root_requires_agent_home() {
        let lock = GlobalStateLock::new();
        let _guard = EnvGuard::remove(&lock, "AGENT_HOME");

        let err = runtime_root().expect_err("missing AGENT_HOME must error");
        assert_eq!(err, RuntimeLayoutError::AgentHomeNotSet);
        assert!(
            err.to_string().contains("AGENT_HOME"),
            "error message must reference AGENT_HOME: {err}"
        );

        let _empty = EnvGuard::set(&lock, "AGENT_HOME", "");
        let err = runtime_root().expect_err("empty AGENT_HOME must error");
        assert_eq!(err, RuntimeLayoutError::AgentHomeNotSet);
    }

    #[test]
    fn test_runtime_root_uses_agent_home_value() {
        let lock = GlobalStateLock::new();
        let _guard = EnvGuard::set(&lock, "AGENT_HOME", "/tmp/agent-home-fixture");

        let root = runtime_root().expect("runtime_root");
        assert_eq!(
            root,
            PathBuf::from("/tmp/agent-home-fixture/out/plan-issue-delivery")
        );
    }

    #[test]
    fn test_repo_slug_uses_double_underscore() {
        assert_eq!(
            repo_slug("graysurf/plan-issue-smoke"),
            "graysurf__plan-issue-smoke"
        );
        assert_eq!(repo_slug("  graysurf/repo  "), "graysurf__repo");
        assert_eq!(repo_slug("plain-no-slash"), "plain-no-slash");
    }

    #[test]
    fn test_issue_root_path_layout() {
        let lock = GlobalStateLock::new();
        let _guard = EnvGuard::set(&lock, "AGENT_HOME", "/tmp/agent-home-fixture");

        let issue = issue_root_for("graysurf__plan-issue-smoke", 17);
        assert_eq!(
            issue.root(),
            Path::new(
                "/tmp/agent-home-fixture/out/plan-issue-delivery/graysurf__plan-issue-smoke/issue-17"
            )
        );
        assert_eq!(
            issue.main_agent_init_snapshot(),
            issue
                .root()
                .join("prompts/plan-issue-delivery-main-agent-init.snapshot.md")
        );
        assert_eq!(
            issue.plan_snapshot(),
            issue.root().join("plan/plan.snapshot.md")
        );
        assert_eq!(
            issue.plan_branch_ref(),
            issue.root().join("plan/plan-branch.ref")
        );
        assert_eq!(issue.plan_task_spec(), issue.root().join("plan/tasks.tsv"));
        assert_eq!(
            issue.plan_issue_body(),
            issue.root().join("plan/issue-body.md")
        );
        assert_eq!(issue.worktree_root(), issue.root().join("worktrees"));
    }

    #[test]
    fn test_assigned_worktree_canonical_paths() {
        let lock = GlobalStateLock::new();
        let _guard = EnvGuard::set(&lock, "AGENT_HOME", "/tmp/agent-home-fixture");

        let issue = issue_root_for("graysurf__plan-issue-smoke", 17);

        // pr-isolated: pinned by TASK_ID
        assert_eq!(
            issue
                .assigned_worktree("pr-isolated", "S1T1", "s1-auto-g1", 1)
                .expect("pr-isolated"),
            issue.worktree_root().join("pr-isolated").join("S1T1")
        );

        // pr-shared: pinned by PR_GROUP
        assert_eq!(
            issue
                .assigned_worktree("pr-shared", "S1T1", "s1-auto-g1", 1)
                .expect("pr-shared"),
            issue.worktree_root().join("pr-shared").join("s1-auto-g1")
        );

        // per-sprint: pinned by sprint number
        assert_eq!(
            issue
                .assigned_worktree("per-sprint", "S1T1", "s1", 1)
                .expect("per-sprint"),
            issue.worktree_root().join("per-sprint").join("sprint-1")
        );
        assert_eq!(
            issue
                .assigned_worktree("per-sprint", "S2T1", "s2", 2)
                .expect("per-sprint sprint-2"),
            issue.worktree_root().join("per-sprint").join("sprint-2")
        );

        // Unknown mode falls back to pr-isolated shape.
        assert_eq!(
            issue
                .assigned_worktree("unknown-mode", "S1T1", "s1", 1)
                .expect("fallback"),
            issue.worktree_root().join("pr-isolated").join("S1T1")
        );

        // Empty task id rejected for pr-isolated.
        assert!(issue
            .assigned_worktree("pr-isolated", "", "g1", 1)
            .is_err());
        // Empty pr_group rejected for pr-shared.
        assert!(issue.assigned_worktree("pr-shared", "S1T1", "", 1).is_err());
    }

    #[test]
    fn test_issue_root_rejects_invalid_repo_slug() {
        let lock = GlobalStateLock::new();
        let _guard = EnvGuard::set(&lock, "AGENT_HOME", "/tmp/agent-home-fixture");

        let err = IssueRoot::new("", 1).expect_err("empty slug must reject");
        assert!(matches!(err, RuntimeLayoutError::InvalidRepoSlug { .. }));

        let err = IssueRoot::new("owner/repo", 1).expect_err("unconverted slash must reject");
        assert!(matches!(err, RuntimeLayoutError::InvalidRepoSlug { .. }));
    }

    #[test]
    fn test_sprint_root_path_layout() {
        let lock = GlobalStateLock::new();
        let _guard = EnvGuard::set(&lock, "AGENT_HOME", "/tmp/agent-home-fixture");

        let issue = issue_root_for("graysurf__plan-issue-smoke", 17);
        let sprint = SprintRoot::new(&issue, 1);
        assert_eq!(sprint.root(), issue.root().join("sprint-1"));
        assert_eq!(sprint.prompts_dir(), sprint.root().join("prompts"));
        assert_eq!(sprint.manifests_dir(), sprint.root().join("manifests"));
        assert_eq!(sprint.specs_dir(), sprint.root().join("specs"));
        assert_eq!(
            sprint.subagent_init_snapshot(),
            sprint
                .root()
                .join("prompts/plan-issue-delivery-subagent-init.snapshot.md")
        );
        assert_eq!(
            sprint.task_prompt("S1T1").expect("task prompt"),
            sprint.root().join("prompts/S1T1.md")
        );
        assert_eq!(
            sprint.prompt_manifest(),
            sprint.root().join("manifests/prompt-manifest.tsv")
        );
        assert_eq!(
            sprint.task_spec(),
            sprint.root().join("specs/sprint-task-spec.tsv")
        );
        assert_eq!(
            sprint.dispatch_record("S1T1").expect("dispatch record"),
            sprint.root().join("manifests/dispatch-S1T1.json")
        );
    }

    #[test]
    fn test_sprint_root_rejects_invalid_task_id() {
        let lock = GlobalStateLock::new();
        let _guard = EnvGuard::set(&lock, "AGENT_HOME", "/tmp/agent-home-fixture");

        let issue = issue_root_for("graysurf__plan-issue-smoke", 17);
        let sprint = SprintRoot::new(&issue, 1);

        let err = sprint.task_prompt("").expect_err("empty id rejected");
        assert!(matches!(err, RuntimeLayoutError::InvalidTaskId { .. }));

        let err = sprint
            .dispatch_record("S1/T1")
            .expect_err("slash in id rejected");
        assert!(matches!(err, RuntimeLayoutError::InvalidTaskId { .. }));
    }

    #[test]
    fn test_ensure_dir_is_idempotent() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let target = tmp.path().join("a").join("b").join("c");

        ensure_dir(&target).expect("first ensure_dir");
        ensure_dir(&target).expect("second ensure_dir");
        assert!(target.is_dir());
    }
}
