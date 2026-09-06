use serde::{Deserialize, Serialize};

pub const DSH_CAPABILITY_GROUP_SCHEMA_VERSION: &str = "agent-hook.dsh-policy-capability-groups.v1";

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
pub enum DshCapabilityGroup {
    AgentActivity,
    OwnerUnclaimed,
    SemanticConflict,
    OperationLifecycle,
    AgentScopeLockGuard,
    BlockDirectGitCommit,
    BlockDirectGitWorktree,
    BlockDirectPrCreate,
    BlockDirectPython,
    BlockProjectMemoryWrite,
    BlockUnsafeDefaultDelivery,
    CheckoutLeaseGuard,
    FinishLineRecord,
    ForgeLabelReminder,
    McpSecretScan,
    MemoryWritePrincipleReminder,
    PortablePathsScan,
    PreEditIntentGate,
    SemanticCommitBodyGate,
    SessionStartHealthcheck,
    SkillUsageReminder,
    StopPrePrReminder,
    UserPromptAgentMemory,
}

impl DshCapabilityGroup {
    pub const ALL: [Self; 23] = [
        Self::AgentActivity,
        Self::OwnerUnclaimed,
        Self::SemanticConflict,
        Self::OperationLifecycle,
        Self::AgentScopeLockGuard,
        Self::BlockDirectGitCommit,
        Self::BlockDirectGitWorktree,
        Self::BlockDirectPrCreate,
        Self::BlockDirectPython,
        Self::BlockProjectMemoryWrite,
        Self::BlockUnsafeDefaultDelivery,
        Self::CheckoutLeaseGuard,
        Self::FinishLineRecord,
        Self::ForgeLabelReminder,
        Self::McpSecretScan,
        Self::MemoryWritePrincipleReminder,
        Self::PortablePathsScan,
        Self::PreEditIntentGate,
        Self::SemanticCommitBodyGate,
        Self::SessionStartHealthcheck,
        Self::SkillUsageReminder,
        Self::StopPrePrReminder,
        Self::UserPromptAgentMemory,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AgentActivity => "agent-activity",
            Self::OwnerUnclaimed => "owner-unclaimed",
            Self::SemanticConflict => "semantic-conflict",
            Self::OperationLifecycle => "operation-lifecycle",
            Self::AgentScopeLockGuard => "agent-scope-lock-guard",
            Self::BlockDirectGitCommit => "block-direct-git-commit",
            Self::BlockDirectGitWorktree => "block-direct-git-worktree",
            Self::BlockDirectPrCreate => "block-direct-pr-create",
            Self::BlockDirectPython => "block-direct-python",
            Self::BlockProjectMemoryWrite => "block-project-memory-write",
            Self::BlockUnsafeDefaultDelivery => "block-unsafe-default-delivery",
            Self::CheckoutLeaseGuard => "checkout-lease-guard",
            Self::FinishLineRecord => "finish-line-record",
            Self::ForgeLabelReminder => "forge-label-reminder",
            Self::McpSecretScan => "mcp-secret-scan",
            Self::MemoryWritePrincipleReminder => "memory-write-principle-reminder",
            Self::PortablePathsScan => "portable-paths-scan",
            Self::PreEditIntentGate => "pre-edit-intent-gate",
            Self::SemanticCommitBodyGate => "semantic-commit-body-gate",
            Self::SessionStartHealthcheck => "session-start-healthcheck",
            Self::SkillUsageReminder => "skill-usage-reminder",
            Self::StopPrePrReminder => "stop-pre-pr-reminder",
            Self::UserPromptAgentMemory => "user-prompt-agent-memory",
        }
    }

    pub const fn task_3_2(self) -> bool {
        matches!(
            self,
            Self::OwnerUnclaimed
                | Self::SemanticConflict
                | Self::AgentScopeLockGuard
                | Self::BlockDirectGitCommit
                | Self::BlockDirectGitWorktree
                | Self::BlockDirectPrCreate
                | Self::BlockDirectPython
                | Self::BlockUnsafeDefaultDelivery
                | Self::CheckoutLeaseGuard
                | Self::PreEditIntentGate
                | Self::SemanticCommitBodyGate
        )
    }

    pub const fn task_3_3(self) -> bool {
        matches!(
            self,
            Self::BlockProjectMemoryWrite
                | Self::ForgeLabelReminder
                | Self::McpSecretScan
                | Self::MemoryWritePrincipleReminder
                | Self::PortablePathsScan
                | Self::SessionStartHealthcheck
                | Self::SkillUsageReminder
                | Self::StopPrePrReminder
                | Self::UserPromptAgentMemory
        )
    }

    pub const fn task_3_4(self) -> bool {
        matches!(self, Self::AgentActivity | Self::OperationLifecycle)
    }

    /// Enforcement tier accepted on the runtime-kit tracker (dsh-runtime-kit#197,
    /// child #199). Every group has exactly one tier; `tests/policy_parity.rs`
    /// freezes the table.
    pub const fn tier(self) -> DshTier {
        match self {
            Self::OwnerUnclaimed
            | Self::SemanticConflict
            | Self::OperationLifecycle
            | Self::AgentScopeLockGuard
            | Self::CheckoutLeaseGuard
            | Self::McpSecretScan
            | Self::FinishLineRecord => DshTier::Integrity,
            Self::BlockDirectGitCommit
            | Self::BlockDirectGitWorktree
            | Self::BlockDirectPrCreate
            | Self::BlockUnsafeDefaultDelivery
            | Self::SemanticCommitBodyGate
            | Self::BlockProjectMemoryWrite
            | Self::PortablePathsScan
            | Self::PreEditIntentGate => DshTier::GovernedSeam,
            Self::AgentActivity
            | Self::BlockDirectPython
            | Self::ForgeLabelReminder
            | Self::MemoryWritePrincipleReminder
            | Self::SessionStartHealthcheck
            | Self::SkillUsageReminder
            | Self::StopPrePrReminder
            | Self::UserPromptAgentMemory => DshTier::Reminder,
        }
    }

    /// Executable names whose presence in a raw shell command keeps the
    /// fail-closed shell classification for a Tier B group. A group without
    /// subjects never blocks on an unclassifiable command; it yields context.
    pub const fn shell_subjects(self) -> &'static [&'static str] {
        match self {
            Self::BlockDirectGitCommit
            | Self::BlockDirectGitWorktree
            | Self::BlockUnsafeDefaultDelivery => &["git"],
            Self::BlockDirectPrCreate => &["gh", "glab"],
            Self::SemanticCommitBodyGate => &["semantic-commit"],
            _ => &[],
        }
    }

    /// Remediation carried by every Tier B denial and by its `advise`
    /// projection. It names the governed replacement rather than the failure.
    pub const fn remediation(self) -> Option<&'static str> {
        match self {
            Self::BlockDirectGitCommit => Some(
                "Direct `git commit` is a governed seam. Commit through `semantic-commit commit` (for example `semantic-commit commit -F <message-file>`) from a managed worktree; it validates the message and records the delivery.",
            ),
            Self::BlockDirectGitWorktree => Some(
                "Direct `git worktree` is a governed seam. Create, list, and remove agent worktrees through `git-cli worktree add|list|remove` so ownership and cleanup stay recorded.",
            ),
            Self::BlockDirectPrCreate => Some(
                "Direct pull-request creation is a governed seam. Open and deliver records through `forge-cli pr deliver` (or `forge-cli pr create`), which renders the body, applies labels, and runs the review gates.",
            ),
            Self::BlockUnsafeDefaultDelivery => Some(
                "Mutating the default branch directly is a governed seam. Deliver through a feature branch and `forge-cli pr deliver`; when direct-main delivery was explicitly authorized, use the `semantic-commit default-branch` receipt flow or `forge-cli repo push-default`.",
            ),
            Self::SemanticCommitBodyGate => Some(
                "The commit message needs a body. Re-run `semantic-commit commit` with at least one `--body-bullet` or a message file whose body explains the user-visible change.",
            ),
            Self::BlockProjectMemoryWrite => Some(
                "Project truth belongs in repository-owned documents, not agent memory. Record it in AGENTS.md, DEVELOPMENT.md, or docs/ and keep memory for personal setup and preferences.",
            ),
            Self::PortablePathsScan => Some(
                "A machine-local path is about to land in a portable surface. Replace it with `$HOME`, a repository-relative path, or a placeholder before writing.",
            ),
            Self::PreEditIntentGate => Some(
                "The edit needs a current agent-docs intent. Run `agent-docs session prepare --intent <intent>` for the relevant intent, then retry the edit.",
            ),
            _ => None,
        }
    }
}

/// Enforcement tier of a DSH capability group.
///
/// - `Integrity` (Tier A): blocks and can never be downgraded; the contract
///   rejects any declaration that is not enforce, fail closed, and locked.
/// - `GovernedSeam` (Tier B): blocks by default; a config override may
///   downgrade it to `advise`, which projects the block to context.
/// - `Reminder` (Tier C): emits context only and never blocks.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
pub enum DshTier {
    Integrity,
    GovernedSeam,
    Reminder,
}

impl DshTier {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Integrity => "integrity",
            Self::GovernedSeam => "governed-seam",
            Self::Reminder => "reminder",
        }
    }

    /// Enforcement the tier applies before any override.
    pub const fn enforcement_default(self) -> &'static str {
        match self {
            Self::Integrity | Self::GovernedSeam => "block",
            Self::Reminder => "context",
        }
    }
}
