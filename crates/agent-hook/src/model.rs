use std::collections::BTreeMap;
use std::path::PathBuf;

use clap::ValueEnum;
use serde::{Deserialize, Serialize};

pub const CONFIG_VERSION: &str = "agent-hook.config.v1";
pub const POLICY_VERSION: &str = "agent-hook.policy.v1";
pub const REQUEST_VERSION: &str = "agent-hook.normalized-request.v1";
pub const DECISION_VERSION: &str = "agent-hook.normalized-decision.v1";

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord, ValueEnum)]
#[serde(rename_all = "snake_case")]
#[clap(rename_all = "lower")]
pub enum Product {
    Codex,
    Claude,
    Hermes,
}

impl Product {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
            Self::Hermes => "hermes",
        }
    }

    pub fn enforceable(self) -> bool {
        !matches!(self, Self::Hermes)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum RecoveryScope {
    OneShot,
    RepairWindow,
}

impl RecoveryScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OneShot => "one-shot",
            Self::RepairWindow => "repair-window",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub schema_version: String,
    pub policy: PolicySelection,
    #[serde(default)]
    pub providers: BTreeMap<String, ProviderConfig>,
    #[serde(default)]
    pub overrides: BTreeMap<String, RuleOverride>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PolicySelection {
    pub path: PathBuf,
    pub digest: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderConfig {
    #[serde(default)]
    pub mode: ProviderMode,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderMode {
    #[default]
    Enforce,
    Shadow,
    Disabled,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuleOverride {
    pub mode: RuleMode,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyBundle {
    pub schema_version: String,
    pub bundle_id: String,
    pub version: String,
    pub rules: Vec<PolicyRule>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyRule {
    pub id: String,
    pub products: Vec<Product>,
    pub events: Vec<String>,
    #[serde(default)]
    pub matcher: Option<String>,
    pub priority: i32,
    #[serde(default)]
    pub mode: RuleMode,
    pub failure_posture: FailurePosture,
    #[serde(default)]
    pub timeout_posture: TimeoutPosture,
    pub override_class: OverrideClass,
    pub capability: Capability,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuleMode {
    #[default]
    Enforce,
    Shadow,
    Disabled,
}

impl RuleMode {
    pub fn authority(self) -> u8 {
        match self {
            Self::Disabled => 0,
            Self::Shadow => 1,
            Self::Enforce => 2,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FailurePosture {
    Open,
    Warn,
    Closed,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TimeoutPosture {
    #[default]
    Closed,
    Warn,
    EffectGated,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OperationEffectClass {
    ReadOnly,
    LocalReversible,
    LocalDestructive,
    ExternalMutation,
    SensitiveConfiguration,
    Unknown,
}

impl OperationEffectClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read_only",
            Self::LocalReversible => "local_reversible",
            Self::LocalDestructive => "local_destructive",
            Self::ExternalMutation => "external_mutation",
            Self::SensitiveConfiguration => "sensitive_configuration",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum OverrideClass {
    Locked,
    DowngradeOnly,
    Free,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "id", deny_unknown_fields)]
pub enum Capability {
    #[serde(rename = "decision.allow.v1")]
    Allow { reason_code: String },
    #[serde(rename = "decision.warn.v1")]
    Warn {
        reason_code: String,
        message: String,
    },
    #[serde(rename = "decision.block.v1")]
    Block {
        reason_code: String,
        message: String,
    },
    #[serde(rename = "decision.context.v1")]
    Context { reason_code: String, text: String },
    #[serde(rename = "decision.transform.v1")]
    Transform {
        reason_code: String,
        replacement: serde_json::Value,
    },
    #[serde(rename = "agent-session.activity.v1")]
    SessionActivity { reason_code: String },
    #[serde(rename = "agent-session.owner-liveness.v1")]
    OwnerLiveness {
        reason_code: String,
        #[serde(default = "default_legacy_ttl")]
        legacy_ttl_seconds: u64,
    },
    #[serde(rename = "agent-session.semantic-conflict.v1")]
    SemanticConflict { reason_code: String },
    #[serde(rename = "agent-session.coordination.v1")]
    SessionCoordination { reason_code: String },
    #[serde(rename = "execution.read-only.v1")]
    ExecutionReadOnly {
        reason_code: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fallback_handler_id: Option<String>,
    },
    #[serde(rename = "runtime-kit.handler.v1")]
    RuntimeKitHandler { handler_id: String },
}

fn default_legacy_ttl() -> u64 {
    300
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NormalizedRequest {
    pub schema_version: String,
    pub request_id: String,
    pub product: Product,
    pub event: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matcher: Option<String>,
    pub target_digest: String,
    pub command_digest: String,
    pub snapshot_digest: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree_fingerprint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub semantic_conflict: Option<SemanticConflict>,
    /// Public boolean fact projected from the provider's own Stop re-entry
    /// marker, for example Claude's `stop_hook_active`. `None` means the
    /// provider did not report re-entry state for this delivery.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_reentry: Option<bool>,
    #[serde(skip)]
    pub target_paths: Vec<PathBuf>,
    #[serde(skip)]
    pub execution_path: Option<PathBuf>,
    #[serde(skip)]
    pub binding_roots: Vec<PathBuf>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SemanticConflict {
    Definite,
    Potential,
    Unknown,
    Clear,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum DecisionAction {
    Allow,
    Warn,
    Context,
    Transform,
    Block,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DecisionReason {
    pub rule_id: String,
    pub code: String,
    pub disposition: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ShadowObservation {
    pub rule_id: String,
    pub action: DecisionAction,
    pub code: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NormalizedDecision {
    pub schema_version: String,
    pub request_id: String,
    pub product: Product,
    pub event: String,
    pub action: DecisionAction,
    pub reasons: Vec<DecisionReason>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replacement: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shadow: Vec<ShadowObservation>,
    pub config_digest: String,
    pub policy_digest: String,
    #[serde(default)]
    pub recovery_applied: bool,
    #[serde(skip)]
    pub provider_output: Option<serde_json::Value>,
}

#[derive(Clone, Debug)]
pub struct LoadedPolicy {
    pub config: Config,
    pub bundle: PolicyBundle,
    pub config_digest: String,
    pub policy_digest: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SetupAction {
    DryRun,
    RemoveDryRun,
    Apply,
    Repair,
    Remove,
}

impl SetupAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DryRun => "dry-run",
            Self::RemoveDryRun => "remove-dry-run",
            Self::Apply => "apply",
            Self::Repair => "repair",
            Self::Remove => "remove",
        }
    }

    pub(crate) fn is_preview(self) -> bool {
        matches!(self, Self::DryRun | Self::RemoveDryRun)
    }

    pub(crate) fn is_remove(self) -> bool {
        matches!(self, Self::Remove | Self::RemoveDryRun)
    }
}
