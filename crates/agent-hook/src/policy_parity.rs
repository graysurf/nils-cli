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
}
