use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum, ValueHint};
use serde::{Deserialize, Serialize};

#[derive(Debug, Parser)]
#[command(
    name = "test-first-evidence",
    version,
    long_version = nils_build_info::long_version(env!("CARGO_PKG_VERSION")),
    about = "Record test-first evidence and waivers for agent workflows.",
    long_about = "Create and verify durable test-first evidence records that capture contract impact, meaningful failures, waivers, residual gaps, and scoped final validation.",
    disable_help_subcommand = true,
    after_help = "EXAMPLES:\n  test-first-evidence init --out /tmp/evidence --classification behavior-change --production-path src/lib.rs --changed-behavior 'new contract'\n  test-first-evidence record-impact --out /tmp/evidence --target tests/lib.rs::contract --disposition add-missing --protected-behavior 'new contract' --reason 'no owner test exists'\n  test-first-evidence record-failing --out /tmp/evidence --command 'cargo test contract' --exit-code 101 --summary 'bug reproduced' --expected-failure 'new contract missing' --observed-failure 'assertion mismatch'\n  test-first-evidence record-final --out /tmp/evidence --command 'cargo test contract' --status pass --scope focused\n  test-first-evidence record-gap --out /tmp/evidence --none\n  test-first-evidence verify --out /tmp/evidence --format json\n  test-first-evidence completion zsh\n\nENVIRONMENT:\n  none\n\nEXIT CODES:\n  0   success\n  1   runtime error\n  64  command-line usage error\n  65  invalid input data"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
#[command(rename_all = "kebab-case")]
pub enum Command {
    /// Create a deterministic evidence record.
    Init(InitArgs),
    /// Record a failing test or reproducible failure before the fix.
    RecordFailing(RecordFailingArgs),
    /// Record one materially affected test target or declare none exist.
    RecordImpact(RecordImpactArgs),
    /// Record an explicit waiver when failing evidence is not practical.
    RecordWaiver(RecordWaiverArgs),
    /// Record final validation after the implementation.
    RecordFinal(RecordFinalArgs),
    /// Record one residual gap or explicitly declare that none remain.
    RecordGap(RecordGapArgs),
    /// Query classified, pre-edit, or delivery readiness without mutating the record.
    Check(CheckArgs),
    /// Verify the evidence record is complete enough for delivery.
    Verify(CommonArgs),
    /// Print the current evidence record.
    Show(CommonArgs),
    /// Print shell completion script.
    Completion(CompletionArgs),
}

#[derive(Debug, Args)]
pub struct CommonArgs {
    /// Evidence artifact directory containing `test-first-evidence.json`.
    #[arg(long = "out", value_name = "DIR", value_hint = ValueHint::DirPath)]
    pub out_dir: PathBuf,

    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,
}

#[derive(Debug, Args)]
pub struct InitArgs {
    #[command(flatten)]
    pub common: CommonArgs,

    /// Change classification. Feature/bug delivery requires a testable classification.
    #[arg(long, value_enum)]
    pub classification: ChangeClassification,

    /// Production path affected by the change. Repeat for multiple paths.
    #[arg(long = "production-path", value_name = "PATH", value_hint = ValueHint::AnyPath)]
    pub production_paths: Vec<PathBuf>,

    /// Optional note to keep with the record. Repeat for multiple notes.
    #[arg(long = "note", value_name = "TEXT")]
    pub notes: Vec<String>,

    /// Existing behavior or invariant that must remain unchanged. Repeatable.
    #[arg(long = "retained-behavior", value_name = "TEXT")]
    pub retained_behaviors: Vec<String>,

    /// Existing behavior intentionally changed by this work. Repeatable.
    #[arg(long = "changed-behavior", value_name = "TEXT")]
    pub changed_behaviors: Vec<String>,

    /// Existing behavior intentionally removed by this work. Repeatable.
    #[arg(long = "removed-behavior", value_name = "TEXT")]
    pub removed_behaviors: Vec<String>,

    /// New behavior added by this work. Repeatable.
    #[arg(long = "added-behavior", value_name = "TEXT")]
    pub added_behaviors: Vec<String>,

    /// Cross-change invariant that must continue to hold. Repeatable.
    #[arg(long, value_name = "TEXT")]
    pub invariant: Vec<String>,

    /// Overwrite an existing record.
    #[arg(long)]
    pub force: bool,
}

#[derive(Debug, Args)]
pub struct RecordFailingArgs {
    #[command(flatten)]
    pub common: CommonArgs,

    /// Command or manual step that produced the before-fix failure.
    #[arg(long, value_name = "TEXT")]
    pub command: String,

    /// Exit code from the failing command.
    #[arg(long, value_name = "CODE")]
    pub exit_code: i32,

    /// Concise failure summary.
    #[arg(long, value_name = "TEXT")]
    pub summary: String,

    /// Failure that should occur because the intended behavior is not implemented yet.
    #[arg(long = "expected-failure", value_name = "TEXT")]
    pub expected_failure: String,

    /// Failure actually observed in the before-fix run.
    #[arg(long = "observed-failure", value_name = "TEXT")]
    pub observed_failure: String,

    /// Optional test name or scenario identifier.
    #[arg(long = "test-name", value_name = "TEXT")]
    pub test_name: Option<String>,

    /// Optional evidence artifact path. Repeat for multiple artifacts.
    #[arg(long, value_name = "PATH", value_hint = ValueHint::AnyPath)]
    pub artifact: Vec<PathBuf>,
}

#[derive(Debug, Args)]
pub struct RecordImpactArgs {
    #[command(flatten)]
    pub common: CommonArgs,

    /// Declare that no existing test target is materially affected.
    #[arg(long)]
    pub none: bool,

    /// Test name, path, suite, fixture family, or snapshot group.
    #[arg(long, value_name = "TEXT")]
    pub target: Option<String>,

    /// Planned disposition for the affected target.
    #[arg(long, value_enum)]
    pub disposition: Option<TestDisposition>,

    /// Behavior or risk protected by the target.
    #[arg(long = "protected-behavior", value_name = "TEXT")]
    pub protected_behavior: Option<String>,

    /// Why this disposition is correct. Required for both target and none forms.
    #[arg(long, value_name = "TEXT")]
    pub reason: Option<String>,

    /// Replacement or primary owner test when another target preserves the contract.
    #[arg(long = "owner-test", value_name = "TEXT")]
    pub owner_test: Option<String>,

    /// Confirm that a removed test's protected invariant is intentionally retired.
    #[arg(long = "invariant-retired")]
    pub invariant_retired: bool,

    /// Final validation scopes required by this impact. Repeatable.
    #[arg(long = "validation-scope", value_enum)]
    pub validation_scopes: Vec<ValidationScope>,
}

#[derive(Debug, Args)]
pub struct RecordWaiverArgs {
    #[command(flatten)]
    pub common: CommonArgs,

    /// Reason failing-test evidence was not practical.
    #[arg(long, value_name = "TEXT")]
    pub reason: String,

    /// Whether the change is permanently non-testable or carries deferred test debt.
    #[arg(long = "waiver-kind", value_enum)]
    pub waiver_kind: WaiverKind,

    /// Why meaningful failing evidence cannot be captured now.
    #[arg(long = "why-no-red", value_name = "TEXT")]
    pub why_no_red: String,

    /// Substitute validation captured before editing. Repeat for multiple items.
    #[arg(long = "substitute-validation", value_name = "TEXT")]
    pub substitute_validation: Vec<String>,

    /// Durable follow-up issue or action for deferred test debt.
    #[arg(long, value_name = "TEXT")]
    pub follow_up: Option<String>,

    /// Expiry or removal condition for deferred test debt.
    #[arg(long, value_name = "TEXT")]
    pub expires: Option<String>,
}

#[derive(Debug, Args)]
pub struct RecordFinalArgs {
    #[command(flatten)]
    pub common: CommonArgs,

    /// Final validation command or manual validation step.
    #[arg(long, value_name = "TEXT")]
    pub command: String,

    /// Final validation status.
    #[arg(long, value_enum)]
    pub status: ValidationStatus,

    /// Risk scope proven by this validation.
    #[arg(long, value_enum)]
    pub scope: ValidationScope,

    /// Optional final validation summary.
    #[arg(long, value_name = "TEXT")]
    pub summary: Option<String>,

    /// Optional validation artifact path. Repeat for multiple artifacts.
    #[arg(long, value_name = "PATH", value_hint = ValueHint::AnyPath)]
    pub artifact: Vec<PathBuf>,
}

#[derive(Debug, Args)]
pub struct RecordGapArgs {
    #[command(flatten)]
    pub common: CommonArgs,

    /// Explicitly declare that no residual validation or coverage gaps remain.
    #[arg(long)]
    pub none: bool,

    /// Residual gap accepted for this delivery.
    #[arg(long, value_name = "TEXT")]
    pub gap: Option<String>,

    /// Why the residual gap is acceptable now.
    #[arg(long, value_name = "TEXT")]
    pub reason: Option<String>,

    /// Durable follow-up issue or action for the gap.
    #[arg(long = "follow-up", value_name = "TEXT")]
    pub follow_up: Option<String>,
}

#[derive(Debug, Args)]
pub struct CompletionArgs {
    /// Shell to generate completion script for.
    #[arg(value_enum)]
    pub shell: crate::completion::CompletionShell,
}

#[derive(Debug, Args)]
pub struct CheckArgs {
    #[command(flatten)]
    pub common: CommonArgs,

    /// Readiness phase to evaluate.
    #[arg(long, value_enum)]
    pub phase: CheckPhase,

    /// Repository root used by the pre-edit path-class contract.
    #[arg(long = "project-path", value_name = "DIR", value_hint = ValueHint::DirPath)]
    pub project_path: Option<PathBuf>,

    /// Repository-relative path to classify. Repeat for batches.
    #[arg(long, value_name = "PATH", value_hint = ValueHint::AnyPath)]
    pub path: Vec<PathBuf>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum OutputFormat {
    Text,
    Json,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Deserialize, Serialize, ValueEnum)]
#[value(rename_all = "kebab-case")]
#[serde(rename_all = "kebab-case")]
pub enum ValidationStatus {
    Pass,
    Fail,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Deserialize, Serialize, ValueEnum)]
#[value(rename_all = "kebab-case")]
#[serde(rename_all = "kebab-case")]
pub enum TestDisposition {
    Keep,
    UpdateSpec,
    RemoveObsolete,
    AddMissing,
    RefactorOnly,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Deserialize, Serialize, ValueEnum)]
#[value(rename_all = "kebab-case")]
#[serde(rename_all = "kebab-case")]
pub enum ValidationScope {
    Focused,
    AffectedSuite,
    ContractConsumer,
    Full,
    Manual,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize, ValueEnum)]
#[value(rename_all = "kebab-case")]
#[serde(rename_all = "kebab-case")]
pub enum WaiverKind {
    NonTestable,
    DeferredDebt,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize, ValueEnum)]
#[value(rename_all = "kebab-case")]
#[serde(rename_all = "kebab-case")]
pub enum ChangeClassification {
    BehaviorChange,
    BugFix,
    Feature,
    DocsOnly,
    ConfigOnly,
    GeneratedOnly,
    RefactorOnly,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum CheckPhase {
    Classified,
    PreEdit,
    Delivery,
}

impl CheckPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Classified => "classified",
            Self::PreEdit => "pre-edit",
            Self::Delivery => "delivery",
        }
    }
}

impl ValidationStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
        }
    }
}

impl ChangeClassification {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BehaviorChange => "behavior-change",
            Self::BugFix => "bug-fix",
            Self::Feature => "feature",
            Self::DocsOnly => "docs-only",
            Self::ConfigOnly => "config-only",
            Self::GeneratedOnly => "generated-only",
            Self::RefactorOnly => "refactor-only",
        }
    }

    pub fn is_testable(self) -> bool {
        matches!(self, Self::BehaviorChange | Self::BugFix | Self::Feature)
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "behavior-change" => Some(Self::BehaviorChange),
            "bug-fix" => Some(Self::BugFix),
            "feature" => Some(Self::Feature),
            "docs-only" => Some(Self::DocsOnly),
            "config-only" => Some(Self::ConfigOnly),
            "generated-only" => Some(Self::GeneratedOnly),
            "refactor-only" => Some(Self::RefactorOnly),
            _ => None,
        }
    }
}

impl TestDisposition {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Keep => "keep",
            Self::UpdateSpec => "update-spec",
            Self::RemoveObsolete => "remove-obsolete",
            Self::AddMissing => "add-missing",
            Self::RefactorOnly => "refactor-only",
        }
    }
}

impl ValidationScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Focused => "focused",
            Self::AffectedSuite => "affected-suite",
            Self::ContractConsumer => "contract-consumer",
            Self::Full => "full",
            Self::Manual => "manual",
        }
    }
}

impl WaiverKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NonTestable => "non-testable",
            Self::DeferredDebt => "deferred-debt",
        }
    }
}
