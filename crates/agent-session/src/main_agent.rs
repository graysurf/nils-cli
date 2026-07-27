use std::env;
use std::ffi::{CString, OsString};
use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::io::{AsRawFd, FromRawFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use clap::error::ErrorKind;
use clap::{Args, CommandFactory, Parser, Subcommand, ValueEnum, ValueHint};
use clap_complete::Shell;
use jiff::Zoned;
use nils_common::cli_contract::{
    Envelope, EnvelopeError, OutputFormat, emit_parse_error, exit, schema_version_for,
};
use nils_common::fs::{SECRET_FILE_MODE, write_atomic};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::cli::{self, AgentKind, CoordinationMode};
use crate::coordination::context::{
    Scope, ScopeKind, WORK_CONTEXT_INPUT_VERSION, WorkContextInput, checkout_root,
};
use crate::orchestration::{
    self, ACCOUNT_HANDOFF_RESERVATION_SCHEMA, ASSIGNMENT_INPUT_SCHEMA, ASSIGNMENT_SCHEMA,
    AccountHandoffReservationRecord, AssignmentRecord, CHECKPOINT_INPUT_SCHEMA,
    GroupCleanupProgressReceipt, IdempotencyReceipt, LEGACY_ACCOUNT_HANDOFF_RESERVATION_V2_SCHEMA,
    PACKET_SCHEMA, RunCheckpoint, RunRecord, SUBMIT_RECOVERY_SCHEMA, SessionRef,
    SubmitRecoveryRecord, TimedRelationship, WORKER_QUARANTINE_SCHEMA, WorkerQuarantineRecord,
};
use crate::{
    CliContext, CliError, PromptDelivery, SessionRecord, SessionRegistryFence,
    StartFailureDisposition, acquire_session_record_lock, delete_session, load_session_record,
    resolve_tmux_bin, run_output_with_timeout_and_cap, runtime_is_proven_never_launched,
    send_submit_recovery_input_serialized, session_dir, session_status,
};

const BINARY: &str = "main-agent";
const IDEMPOTENCY_KEY_HELP: &str = "Retry an ambiguous outcome with the same idempotency key and the same logical request; use a new key for a changed request.";
const ASSIGNMENT_REVISION_HELP: &str =
    "Expected current assignment revision; stale values fail closed and report current_revision.";
const RUN_REVISION_HELP: &str =
    "Expected current run revision; stale values fail closed and report current_revision.";
const MAX_IDEMPOTENCY_RECEIPTS: usize = 32_768;
#[cfg(test)]
static IDEMPOTENCY_RECEIPT_CAPACITY_FOR_TEST: AtomicUsize =
    AtomicUsize::new(MAX_IDEMPOTENCY_RECEIPTS);
const WORKER_START_RUN_REVISION_HELP: &str = "Optional expected run revision. Omit to launch without a run-revision fence: assignment creation is decoupled from the run revision, so parallel and batch starts no longer collide. When supplied, a stale value fails closed and reports current_revision.";
const QUICK_IDEMPOTENCY_KEY_HELP: &str = "Optional for the fast-path: omit to derive a stable idempotency key from a digest of the assignment packet, or supply one to control replay explicitly. 8-128 printable non-space ASCII bytes.";
const MAIN_AGENT_AFTER_HELP: &str = "SAFE LIFECYCLE:\n  init -> rehydrate/status -> worker start --await-ready -> worker bootstrap\n  worker supervise -> accept -> retire -> close\n\nMACRO-FIRST RECOVERY:\n  Use worker supervise for repeatable diagnosis. Use self recover only for this\n  exact Main Agent controller's stale broker. Guidance continuity and managed\n  account handoff use their typed worker actions. Use worker reassign only when\n  supervision proves safe reassignment. If a macro stops, continue from its\n  last_proven_safe_state with worker diagnose, submit-recovery, cancel,\n  account-handoff-cancel, or retire. Account handoff cancellation requires the\n  current assignment revision and --authorize-account-change.\n  never resend a prompt or inject an unbounded/manual Enter.\n\nREVISION AND RETRY RULES:\n  Read the current run or assignment revision before each mutation. Retry an\n  ambiguous outcome with the identical request and idempotency key. After a\n  confirmed revision conflict, re-read state and use a new key for the revised\n  request.\n\nEXAMPLES:\n  main-agent init --packet-file objective.json --if-absent --idempotency-key init-001 --format json\n  main-agent self recover --idempotency-key controller-recover-001 --format json\n  main-agent worker start --assignment-file assignment.json --await-ready 5m --idempotency-key start-001 --format json\n  main-agent worker supervise ASSIGNMENT_ID --format json\n  main-agent worker reassign ASSIGNMENT_ID --assignment-file replacement.json --if-revision 3 --reason \"pre-claim bootstrap failure\" --idempotency-key reassign-001 --format json\n\nOPERATOR RUNBOOK:\n  crates/agent-session/docs/runbooks/main-agent-orchestration.md\n\nEXIT CODES:\n  0   success\n  1   runtime error\n  64  command-line usage error\n  65  invalid or stale data\n  69  temporarily unavailable";

#[derive(Debug, Parser)]
#[command(
    name = "main-agent",
    version,
    long_version = nils_build_info::long_version(env!("CARGO_PKG_VERSION")),
    about = "Manage durable Main Agent orchestration runs and workers.",
    disable_help_subcommand = true,
    after_help = MAIN_AGENT_AFTER_HELP
)]
struct MainAgentCli {
    /// agent-session state directory.
    #[arg(long = "state-dir", global = true, value_name = "PATH", value_hint = ValueHint::DirPath)]
    state_dir: Option<PathBuf>,
    /// Advisory machine label for public projections.
    #[arg(long, global = true, value_name = "HOST")]
    host: Option<String>,
    #[command(subcommand)]
    command: MainAgentCommand,
}

#[derive(Debug, Subcommand)]
enum MainAgentCommand {
    /// Create or continuity-rebind this Main Agent's durable run.
    Init(InitArgs),
    /// Re-bind this Main Agent's durable run to the current session incarnation
    /// after a resume, reusing the stored objective packet (no packet file).
    Rebind(RunMutationArgs),
    /// Inspect the authenticated Main Agent or worker identity.
    #[command(name = "self")]
    SelfGroup(SelfGroupArgs),
    /// Recover the authenticated durable objective or assignment capsule.
    Rehydrate(RehydrateArgs),
    /// Show a bounded status capsule.
    Status(ReadArgs),
    /// Record a revision-fenced run or worker checkpoint.
    Checkpoint(CheckpointArgs),
    /// Authenticated worker bootstrap: acquire the assignment-derived claim,
    /// checkpoint `working`, and return the private assignment packet.
    Bootstrap(BootstrapArgs),
    /// Launch and manage interactive worker assignments.
    Worker(WorkerArgs),
    /// Add a non-authoritative collaborator relationship.
    Collaborate(RelationshipArgs),
    /// Add a bounded non-authoritative borrowing relationship.
    Borrow(BorrowArgs),
    /// Transfer primary assignment routing after quiescence checks.
    Handoff(HandoffArgs),
    /// Adopt an orphaned assignment into this Main Agent's run.
    Adopt(AssignmentMutationArgs),
    /// Close this run after all assignments are terminal.
    Close(RunMutationArgs),
    /// Fast-path: create an ephemeral run, single assignment, and launch in one
    /// call. The run auto-closes once the worker is torn down.
    Quick(QuickArgs),
    /// Print an example objective packet (schema, required fields, and the
    /// nested work-context) that `init --packet-file` accepts.
    PacketSchema(PacketSchemaArgs),
    /// Print shell completion script.
    Completion(CompletionArgs),
}

#[derive(Clone, Debug, Args)]
struct InitArgs {
    /// Private objective packet JSON file.
    #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
    packet_file: PathBuf,
    /// Required absence fence for initial creation; existing continuity is returned or revision-fenced rebind is attempted.
    #[arg(long)]
    if_absent: bool,
    /// Expected current run revision when continuity-rebinding a stopped prior incarnation.
    #[arg(long)]
    if_revision: Option<u64>,
    #[arg(long, help = IDEMPOTENCY_KEY_HELP)]
    idempotency_key: String,
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    format: OutputFormat,
}

#[derive(Debug, Args)]
struct SelfGroupArgs {
    #[command(subcommand)]
    command: SelfCommand,
}

#[derive(Debug, Subcommand)]
enum SelfCommand {
    /// Show this authenticated session's private run or assignment identity.
    Show(ReadArgs),
    /// Recover this exact Main Agent controller's stale coordination heartbeat.
    Recover(ControllerRecoverArgs),
}

#[derive(Clone, Debug, Args)]
struct ReadArgs {
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    format: OutputFormat,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
#[value(rename_all = "lower")]
enum RehydrateFormat {
    Json,
    Markdown,
}

#[derive(Clone, Debug, Args)]
struct RehydrateArgs {
    /// Recovery capsule output format.
    #[arg(long, value_enum, default_value_t = RehydrateFormat::Markdown)]
    format: RehydrateFormat,
}

#[derive(Clone, Debug, Args)]
struct ControllerRecoverArgs {
    #[arg(long, help = IDEMPOTENCY_KEY_HELP)]
    idempotency_key: String,
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    format: OutputFormat,
}

#[derive(Clone, Debug, Args)]
struct CheckpointArgs {
    /// Private checkpoint input JSON file.
    #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
    file: PathBuf,
    /// Expected current revision of the authenticated run or assignment; stale values fail closed and report current_revision.
    #[arg(long)]
    if_revision: u64,
    #[arg(long, help = IDEMPOTENCY_KEY_HELP)]
    idempotency_key: String,
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    format: OutputFormat,
}

#[derive(Clone, Debug, Args)]
struct BootstrapArgs {
    #[arg(long, help = IDEMPOTENCY_KEY_HELP)]
    idempotency_key: String,
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    format: OutputFormat,
}

#[derive(Clone, Debug, Args)]
struct WorkerArgs {
    #[command(subcommand)]
    command: WorkerCommand,
}

#[derive(Clone, Debug, Subcommand)]
enum WorkerCommand {
    /// Create an assignment and launch its interactive managed worker.
    Start(WorkerStartArgs),
    /// List assignments owned by this Main Agent's active run.
    List(ReadArgs),
    /// Show one assignment, including its private packet.
    Show(WorkerShowArgs),
    /// Bounded long-poll until an assignment reaches a target state.
    Wait(WorkerWaitArgs),
    /// Send a private mailbox message to an assignment's worker.
    Message(WorkerMessageArgs),
    /// Carry this controller's unread guidance from the immediately stale
    /// worker incarnation into the exact current incarnation.
    #[command(name = "guidance-reconcile")]
    GuidanceReconcile(AssignmentMutationArgs),
    /// Quarantine this controller's unread guidance for unretained stale
    /// worker incarnations without forwarding or marking it consumed.
    #[command(name = "guidance-quarantine")]
    GuidanceQuarantine(AssignmentMutationArgs),
    /// Apply an explicitly authorized account handoff to the exact worker.
    #[command(name = "account-handoff")]
    AccountHandoff(WorkerAccountHandoffArgs),
    /// Safely cancel a failed, superseded, or queued account-handoff
    /// reservation without changing the bound account.
    #[command(name = "account-handoff-cancel")]
    AccountHandoffCancel(WorkerAccountHandoffCancelArgs),
    /// Return a submitted assignment to its exact worker for bounded revisions.
    #[command(name = "request-changes")]
    RequestChanges(WorkerRequestChangesArgs),
    /// Accept a submitted worker result after Main Agent review.
    Accept(AssignmentMutationArgs),
    /// Mark an accepted assignment terminal before worker deletion.
    Release(AssignmentMutationArgs),
    /// Delete a released worker through guarded agent-session cleanup.
    Delete(AssignmentMutationArgs),
    /// Retire an accepted assignment in one call: release -> delete -> confirm
    /// the worker is absent from a fresh list.
    Retire(AssignmentMutationArgs),
    /// Inspect assignment, provider activity, claim, operation, and worktree
    /// progress without mutating the worker.
    Diagnose(WorkerDiagnoseArgs),
    /// Repeatable bounded supervision macro with a typed classification and
    /// deterministic next action.
    Supervise(WorkerDiagnoseArgs),
    /// Send at most one guarded recovery Enter from a proven startup state.
    #[command(name = "submit-recovery")]
    SubmitRecovery(WorkerSubmitRecoveryArgs),
    /// Terminalize an unknown recovery without sending input after the exact
    /// worker runtime is stopped and coordination state is quiescent.
    #[command(name = "reconcile-recovery")]
    ReconcileRecovery(WorkerReconcileRecoveryArgs),
    /// Terminalize only a proven failed pre-claim assignment.
    Cancel(WorkerCancelArgs),
    /// Cancel and retire a safely reassignable worker, then start one distinct
    /// clean replacement assignment without reusing its prompt or worktree.
    Reassign(WorkerReassignArgs),
}

#[derive(Clone, Debug, Args)]
struct WorkerStartArgs {
    /// Private assignment packet JSON file (single-lane launch).
    #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath, conflicts_with = "batch")]
    assignment_file: Option<PathBuf>,
    /// Directory of assignment packet JSON files to launch as one transport-only
    /// batch. Each lane is fenced independently (T2 decouple), so one lane
    /// failing isolates to its own typed result rather than aborting the batch.
    #[arg(long, value_name = "DIR", value_hint = ValueHint::DirPath, conflicts_with = "await_ready")]
    batch: Option<PathBuf>,
    #[arg(long, help = WORKER_START_RUN_REVISION_HELP)]
    if_run_revision: Option<u64>,
    /// For a single assignment launch, wait up to this bounded duration
    /// (0 = launch-only) for the worker's authenticated checkpoint to advance
    /// the assignment past `starting`. A fresh Codex or Claude launch that
    /// remains `starting` receives one runtime-owned recovery Enter before the
    /// same deadline. This folds the readiness + newer-turn + identity proof
    /// into worker start's typed result. 0-5m (integer with optional s/m/h
    /// suffix). Batch launch is transport-only.
    #[arg(long, default_value = "5m")]
    await_ready: String,
    #[arg(long, help = IDEMPOTENCY_KEY_HELP)]
    idempotency_key: String,
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    format: OutputFormat,
}

#[derive(Clone, Debug, Args)]
struct WorkerShowArgs {
    assignment_id: String,
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    format: OutputFormat,
}

#[derive(Clone, Debug, Args)]
struct WorkerDiagnoseArgs {
    assignment_id: String,
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    format: OutputFormat,
}

#[derive(Clone, Debug, Args)]
struct WorkerSubmitRecoveryArgs {
    assignment_id: String,
    #[arg(long, help = ASSIGNMENT_REVISION_HELP)]
    if_revision: u64,
    /// Bounded wait for an authenticated checkpoint after the single guarded
    /// Enter. 1-30s (integer with optional s/m/h suffix).
    #[arg(long, default_value = "5s")]
    timeout: String,
    #[arg(long, help = IDEMPOTENCY_KEY_HELP)]
    idempotency_key: String,
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    format: OutputFormat,
}

#[derive(Clone, Debug, Args)]
struct WorkerReconcileRecoveryArgs {
    assignment_id: String,
    #[arg(long, help = ASSIGNMENT_REVISION_HELP)]
    if_revision: u64,
    #[arg(long, help = IDEMPOTENCY_KEY_HELP)]
    idempotency_key: String,
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    format: OutputFormat,
}

#[derive(Clone, Debug, Args)]
struct WorkerCancelArgs {
    assignment_id: String,
    #[arg(long, help = ASSIGNMENT_REVISION_HELP)]
    if_revision: u64,
    /// Bounded durable reason for cancelling this failed pre-claim assignment.
    #[arg(long)]
    reason: String,
    #[arg(long, help = IDEMPOTENCY_KEY_HELP)]
    idempotency_key: String,
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    format: OutputFormat,
}

#[derive(Clone, Debug, Args)]
struct WorkerRequestChangesArgs {
    assignment_id: String,
    #[arg(long, help = ASSIGNMENT_REVISION_HELP)]
    if_revision: u64,
    /// Bounded durable reason recorded for the exact worker's next revision.
    #[arg(long)]
    reason: String,
    #[arg(long, help = IDEMPOTENCY_KEY_HELP)]
    idempotency_key: String,
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    format: OutputFormat,
}

#[derive(Clone, Debug, Args)]
struct WorkerReassignArgs {
    assignment_id: String,
    /// Private packet for a distinct replacement assignment and clean worktree.
    #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
    assignment_file: PathBuf,
    #[arg(long, help = ASSIGNMENT_REVISION_HELP)]
    if_revision: u64,
    /// Bounded durable reason recorded on the cancelled assignment.
    #[arg(long)]
    reason: String,
    /// Bounded readiness wait for the replacement worker. 0-5m.
    #[arg(long, default_value = "5m")]
    await_ready: String,
    #[arg(long, help = IDEMPOTENCY_KEY_HELP)]
    idempotency_key: String,
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    format: OutputFormat,
}

/// Assignment-state milestone a `worker wait` long-poll watches for.
#[derive(Clone, Copy, Debug, ValueEnum)]
enum WaitUntil {
    /// The worker submitted a result for Main Agent review.
    Submitted,
    /// The worker reported itself blocked.
    Blocked,
    /// The assignment reached a terminal state (accepted, released, or cancelled).
    Terminal,
}

impl WaitUntil {
    /// Whether an assignment `state` string satisfies this wait target. The
    /// terminal set mirrors the orchestration spec's terminal states so this
    /// stays aligned with `Registry::validate`'s state allowlist.
    fn matches(self, state: &str) -> bool {
        match self {
            WaitUntil::Submitted => state == "submitted",
            WaitUntil::Blocked => state == "blocked",
            WaitUntil::Terminal => matches!(state, "accepted" | "released" | "cancelled"),
        }
    }

    /// Stable label echoed back in the wait result payload.
    fn as_label(self) -> &'static str {
        match self {
            WaitUntil::Submitted => "submitted",
            WaitUntil::Blocked => "blocked",
            WaitUntil::Terminal => "terminal",
        }
    }
}

#[derive(Clone, Debug, Args)]
struct WorkerWaitArgs {
    /// Assignment to watch. Omit and pass --any to watch every assignment.
    assignment_id: Option<String>,
    /// Watch every assignment in the active run instead of a single id.
    #[arg(long)]
    any: bool,
    /// Assignment-state milestone to wait for.
    #[arg(long, value_enum)]
    until: WaitUntil,
    /// Bounded wait duration (1-60s; integer with optional s/m/h/d suffix).
    #[arg(long, default_value = "30s")]
    timeout: String,
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    format: OutputFormat,
}

#[derive(Clone, Debug, Args)]
struct WorkerMessageArgs {
    assignment_id: String,
    /// Private message body file.
    #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
    body_file: PathBuf,
    #[arg(long, help = IDEMPOTENCY_KEY_HELP)]
    idempotency_key: String,
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    format: OutputFormat,
}

#[derive(Clone, Debug, Args)]
struct WorkerAccountHandoffArgs {
    assignment_id: String,
    /// Explicit allowlisted account nickname to apply.
    #[arg(long)]
    account: String,
    #[arg(long, help = ASSIGNMENT_REVISION_HELP)]
    if_revision: u64,
    /// Required acknowledgement that this operation may change the selected
    /// account for the exact worker.
    #[arg(long)]
    authorize_account_change: bool,
    /// Bound for managed account application and verification (1-60s).
    #[arg(long, default_value = "30s")]
    timeout: String,
    #[arg(long, help = IDEMPOTENCY_KEY_HELP)]
    idempotency_key: String,
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    format: OutputFormat,
}

#[derive(Clone, Debug, Args)]
struct WorkerAccountHandoffCancelArgs {
    assignment_id: String,
    /// Exact opaque reservation identity returned by the reservation record.
    #[arg(long)]
    reservation_id: String,
    /// Exact account nickname named by the reservation.
    #[arg(long)]
    account: String,
    /// Exact managed account intent identity. Omit only for a historical v1
    /// reservation that never owned a provider-side intent identity.
    #[arg(long)]
    intent_id: Option<String>,
    #[arg(long, help = ASSIGNMENT_REVISION_HELP)]
    if_revision: u64,
    /// Required acknowledgement that this operation cancels the pending
    /// account intent for the exact worker without changing its bound account.
    #[arg(long)]
    authorize_account_change: bool,
    #[arg(long, help = IDEMPOTENCY_KEY_HELP)]
    idempotency_key: String,
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    format: OutputFormat,
}

#[derive(Clone, Debug, Args)]
struct AssignmentMutationArgs {
    assignment_id: String,
    #[arg(long, help = ASSIGNMENT_REVISION_HELP)]
    if_revision: u64,
    #[arg(long, help = IDEMPOTENCY_KEY_HELP)]
    idempotency_key: String,
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    format: OutputFormat,
}

#[derive(Clone, Debug, Args)]
struct RelationshipArgs {
    assignment_id: String,
    /// Exact live session ref, formatted as SESSION_ID@SESSION_INCARNATION.
    #[arg(long)]
    session: String,
    #[arg(long, help = ASSIGNMENT_REVISION_HELP)]
    if_revision: u64,
    #[arg(long, help = IDEMPOTENCY_KEY_HELP)]
    idempotency_key: String,
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    format: OutputFormat,
}

#[derive(Clone, Debug, Args)]
struct BorrowArgs {
    assignment_id: String,
    /// Exact live session ref, formatted as SESSION_ID@SESSION_INCARNATION.
    #[arg(long)]
    session: String,
    #[arg(long, value_name = "DURATION")]
    duration: String,
    #[arg(long, help = ASSIGNMENT_REVISION_HELP)]
    if_revision: u64,
    #[arg(long, help = IDEMPOTENCY_KEY_HELP)]
    idempotency_key: String,
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    format: OutputFormat,
}

#[derive(Clone, Debug, Args)]
struct HandoffArgs {
    assignment_id: String,
    /// Exact live Main Agent ref, formatted as SESSION_ID@SESSION_INCARNATION.
    #[arg(long = "to")]
    to_session: String,
    #[arg(long, help = ASSIGNMENT_REVISION_HELP)]
    if_revision: u64,
    #[arg(long, help = IDEMPOTENCY_KEY_HELP)]
    idempotency_key: String,
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    format: OutputFormat,
}

#[derive(Clone, Debug, Args)]
struct RunMutationArgs {
    #[arg(long, help = RUN_REVISION_HELP)]
    if_revision: u64,
    #[arg(long, help = IDEMPOTENCY_KEY_HELP)]
    idempotency_key: String,
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    format: OutputFormat,
}

pub(crate) const GROUP_CLEANUP_SCHEMA: &str = "agent-session.main-agent-group-cleanup.v1";
pub(crate) const GROUP_CLEANUP_REQUEST_SCHEMA: &str =
    "agent-session.main-agent-group-cleanup-request.v1";
pub(crate) const GROUP_CLEANUP_RESULT_SCHEMA: &str =
    "agent-session.main-agent-group-cleanup-result.v1";
const GROUP_CLEANUP_MAX_ASSIGNMENTS: usize = 64;
const MANAGED_ACCOUNT_HANDOFF_CAPABILITY: &str =
    crate::codex_app_server::MANAGED_ACCOUNT_HANDOFF_CAPABILITY;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum GroupCleanupMode {
    Safe,
    Force,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GroupCleanupRequest {
    pub schema_version: String,
    pub expected_main_incarnation: String,
    pub expected_run_revision: u64,
    pub expected_plan_digest: String,
    pub mode: GroupCleanupMode,
    pub idempotency_key: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct GroupCleanupWorkerPlan {
    assignment_id: String,
    state: String,
    worker: Option<SessionRef>,
    force_required: bool,
    primary_managed: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct GroupCleanupPlan {
    schema_version: String,
    main: SessionRef,
    run_id: String,
    run_revision: u64,
    requires_force: bool,
    workers: Vec<GroupCleanupWorkerPlan>,
    plan_digest: String,
}

pub(crate) struct GroupCleanupExecution {
    pub value: Value,
    pub deleted_registry_fences: Vec<SessionRegistryFence>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct GroupCleanupResumeState {
    schema_version: String,
    plan: GroupCleanupPlan,
    #[serde(default)]
    authority_sealed: bool,
    worker_results: Vec<Value>,
    deleted_registry_fences: Vec<SessionRegistryFence>,
    #[serde(default)]
    pending_registry_fences: Vec<SessionRegistryFence>,
    run_closed: bool,
}

struct GroupCleanupReplay {
    value: Value,
    resume: Option<GroupCleanupResumeState>,
}

struct GroupCleanupProgressIdentity<'a> {
    requested_session_id: &'a str,
    principal_session_id: &'a str,
    incarnation: &'a str,
}

#[derive(Clone, Debug, Args)]
struct QuickArgs {
    /// Private assignment packet JSON file.
    #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
    assignment_file: PathBuf,
    /// Work tier for the synthesized ephemeral run (L0/L1 delegate-all).
    #[arg(long, default_value = "L0")]
    tier: String,
    /// Bounded wait for the worker's authenticated checkpoint, with the same
    /// runtime-owned single-Enter recovery `worker start --await-ready` performs.
    /// Both commands default to waiting because a launch-only result can leave
    /// a dropped submit key for the caller to notice and hand-repair. Pass 0
    /// for launch-only behavior. 0-5m (integer with optional s/m/h suffix).
    #[arg(long, default_value = "5m")]
    await_ready: String,
    #[arg(long, help = QUICK_IDEMPOTENCY_KEY_HELP)]
    idempotency_key: Option<String>,
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    format: OutputFormat,
}

#[derive(Clone, Debug, Args)]
struct PacketSchemaArgs {
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    format: OutputFormat,
}

#[derive(Debug, Args)]
struct CompletionArgs {
    #[arg(value_enum)]
    shell: crate::completion::CompletionShell,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ObjectivePacket {
    schema_version: String,
    #[serde(default)]
    run_id: Option<String>,
    tier: String,
    objective_summary: String,
    #[serde(default)]
    objective: Value,
    #[serde(default)]
    done_criteria: Vec<String>,
    #[serde(default)]
    constraints: Vec<String>,
    #[serde(default)]
    durable_refs: Vec<String>,
    work_context: WorkContextInput,
    #[serde(default)]
    next_action: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct AssignmentInput {
    schema_version: String,
    #[serde(default)]
    assignment_id: Option<String>,
    task_summary: String,
    #[serde(default)]
    task: Value,
    launch: WorkerLaunchInput,
    #[serde(default)]
    repository: Option<String>,
    #[serde(default)]
    worktree: Option<String>,
    #[serde(default)]
    base_ref: Option<String>,
    #[serde(default)]
    scopes: Vec<String>,
    #[serde(default)]
    durable_refs: Vec<String>,
    /// Assignment ids in the same run that must be accepted before this
    /// assignment's worker may launch. Empty is serialized away so packets that
    /// omit it keep an identical request digest and stored-packet digest.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    depends_on: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct WorkerLaunchInput {
    agent: String,
    cwd: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    coordination_mode: CoordinationMode,
    #[serde(default)]
    agent_args: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CheckpointInput {
    schema_version: String,
    summary: String,
    next_action: String,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    result_summary: Option<String>,
    #[serde(default)]
    blocker_summary: Option<String>,
}

enum Principal {
    Main {
        run: Box<RunRecord>,
        rebind_required: bool,
    },
    Worker {
        assignment: Box<AssignmentRecord>,
        rebind_required: bool,
    },
}

pub(crate) fn run() -> i32 {
    run_with_args(env::args_os())
}

pub(crate) fn run_with_args<I, T>(args: I) -> i32
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let raw_args = args.into_iter().map(Into::into).collect::<Vec<OsString>>();
    let cli = match MainAgentCli::try_parse_from(raw_args.clone()) {
        Ok(cli) => cli,
        Err(error) => {
            let kind = error.kind();
            if matches!(
                kind,
                ErrorKind::DisplayHelp
                    | ErrorKind::DisplayVersion
                    | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
            ) {
                let _ = error.print();
                return error.exit_code();
            }
            let format = detect_output_format(&raw_args);
            let code = if kind == ErrorKind::InvalidSubcommand {
                "unknown-subcommand"
            } else {
                "parse-error"
            };
            return emit_parse_error(BINARY, format, code, &crate::render_clap_message(&error));
        }
    };
    dispatch(cli)
}

fn dispatch(cli: MainAgentCli) -> i32 {
    if let MainAgentCommand::Completion(args) = cli.command {
        return run_completion(args.shell);
    }
    let format = command_output_format(&cli.command);
    let command = command_name(&cli.command);
    let context = match CliContext::resolve(cli.state_dir, cli.host) {
        Ok(context) => context,
        Err(error) => return render_error(command, format, error),
    };
    let result = if command_owns_internal_deadline(&cli.command) {
        run_command(&context, &cli.command)
    } else {
        retry_transient_store(|| run_command(&context, &cli.command))
    };
    match result {
        Ok(value) => {
            if command == "rehydrate" && matches!(format, OutputFormat::Text) {
                render_markdown(&value)
            } else {
                render_success(command, format, &value)
            }
        }
        Err(error) => render_error(command, format, error),
    }
}

/// Execute one resolved Main Agent command. Split out from [`dispatch`] so the
/// facade can re-run commands without internal wall-clock bounds under
/// [`retry_transient_store`] without duplicating the command match. Each
/// handler still owns its own claim, revision, and idempotency fences, so a
/// re-run converges through those rather than duplicating an effect.
fn run_command(context: &CliContext, command: &MainAgentCommand) -> Result<Value, CliError> {
    match command {
        MainAgentCommand::Init(args) => run_init(context, args.clone()),
        MainAgentCommand::Rebind(args) => run_rebind(context, args.clone()),
        MainAgentCommand::SelfGroup(args) => match &args.command {
            SelfCommand::Show(args) => run_self_show(context, args.clone()),
            SelfCommand::Recover(args) => run_controller_recover(context, args.clone()),
        },
        MainAgentCommand::Rehydrate(args) => run_rehydrate(context, args.clone()),
        MainAgentCommand::Status(args) => run_status(context, args.clone()),
        MainAgentCommand::Checkpoint(args) => run_checkpoint(context, args.clone()),
        MainAgentCommand::Bootstrap(args) => run_bootstrap(context, args.clone()),
        MainAgentCommand::Worker(args) => run_worker(context, args.clone()),
        MainAgentCommand::Collaborate(args) => run_collaborate(context, args.clone()),
        MainAgentCommand::Borrow(args) => run_borrow(context, args.clone()),
        MainAgentCommand::Handoff(args) => run_handoff(context, args.clone()),
        MainAgentCommand::Adopt(args) => run_adopt(context, args.clone()),
        MainAgentCommand::Close(args) => run_close(context, args.clone()),
        MainAgentCommand::Quick(args) => run_quick(context, args.clone()),
        MainAgentCommand::PacketSchema(_) => Ok(objective_packet_schema_example()),
        MainAgentCommand::Completion(_) => unreachable!(),
    }
}

// ---- T6: bounded auto-retry for transient orchestration-store conditions ----

fn command_owns_internal_deadline(command: &MainAgentCommand) -> bool {
    matches!(
        command,
        MainAgentCommand::SelfGroup(SelfGroupArgs {
            command: SelfCommand::Recover(_),
        }) | MainAgentCommand::Worker(WorkerArgs {
            command: WorkerCommand::Start(_)
                | WorkerCommand::Wait(_)
                | WorkerCommand::SubmitRecovery(_)
                | WorkerCommand::Reassign(_)
                | WorkerCommand::AccountHandoff(_),
        }) | MainAgentCommand::Quick(_)
    )
}

/// Transient orchestration-store conditions that are safe to auto-retry: the
/// exclusive registry lock was never acquired (`orchestration-store-busy`) or an
/// atomic store read/write never landed (`orchestration-store-unavailable`).
/// Both leave durable state unchanged, so re-running the same command with the
/// same idempotency key converges through idempotency replay and pending
/// start/delete receipts rather than duplicating a side effect. Every other
/// outcome (revision conflicts, usage errors, data errors) is surfaced
/// unchanged so strict callers still see it immediately.
const STORE_RETRY_CODES: [&str; 2] = [
    "orchestration-store-busy",
    "orchestration-store-unavailable",
];
const STORE_RETRY_MAX_ATTEMPTS: u32 = 3;
const STORE_RETRY_BASE_DELAY: Duration = Duration::from_millis(50);

fn is_store_retryable(error: &CliError) -> bool {
    STORE_RETRY_CODES.contains(&error.code())
}

/// Run `attempt`, auto-retrying only the transient store conditions in
/// [`STORE_RETRY_CODES`] with a bounded, linearly-backing-off delay. This retry
/// policy lives in the facade layer; the low-level agent-session primitives stay
/// non-retrying so strict automation still observes transient conditions
/// directly.
fn retry_transient_store<F>(attempt: F) -> Result<Value, CliError>
where
    F: FnMut() -> Result<Value, CliError>,
{
    retry_transient_store_inner(
        STORE_RETRY_MAX_ATTEMPTS,
        STORE_RETRY_BASE_DELAY,
        attempt,
        thread::sleep,
    )
}

fn retry_transient_store_inner<F, S>(
    max_attempts: u32,
    base_delay: Duration,
    mut attempt: F,
    mut sleep: S,
) -> Result<Value, CliError>
where
    F: FnMut() -> Result<Value, CliError>,
    S: FnMut(Duration),
{
    let mut tries: u32 = 0;
    loop {
        tries += 1;
        match attempt() {
            Ok(value) => return Ok(value),
            Err(error) => {
                if tries >= max_attempts || !is_store_retryable(&error) {
                    return Err(error);
                }
                // Linear backoff proportional to the attempt count.
                sleep(base_delay.saturating_mul(tries));
            }
        }
    }
}

fn run_init(context: &CliContext, args: InitArgs) -> Result<Value, CliError> {
    if !args.if_absent {
        return Err(CliError::usage(
            "expected-absence-required",
            "init requires --if-absent",
            None,
        ));
    }
    validate_idempotency_key(&args.idempotency_key)?;
    let packet: ObjectivePacket = crate::coordination::read_bounded_json(
        &args.packet_file,
        256 * 1024,
        "invalid-objective-packet",
    )?;
    validate_objective_packet(&packet)?;
    let (record, incarnation) = authenticated_self(context)?;
    ensure_or_acquire_claim(
        context,
        &record,
        &packet.work_context,
        &args.idempotency_key,
        None,
        false,
    )?;

    let packet_value =
        serde_json::to_value(&packet).map_err(|_| invalid_input("objective packet is invalid"))?;
    let packet_digest = orchestration::packet_digest(&packet_value)?;
    let request_digest = crate::coordination::request_digest(
        "main-agent-init",
        &json!({ "packet": packet, "if_revision": args.if_revision }),
    );
    let mut locked = orchestration::lock_registry(context)?;
    if let Some(value) = idempotency_replay(
        &locked.registry,
        &record,
        &incarnation,
        &args.idempotency_key,
        "init",
        &request_digest,
    )? {
        return Ok(value);
    }

    if let Some(existing) = locked.registry.runs.values_mut().find(|run| {
        run_is_live(run)
            && run.controller.session_id == record.id
            && run.controller.session_created_at == record.created_at
    }) {
        if existing.objective_packet_digest != packet_digest {
            return Err(CliError::data(
                "run-objective-conflict",
                "existing run is bound to a different private objective packet",
                Some(json!({ "run_id": existing.run_id, "current_revision": existing.revision })),
            ));
        }
        let rebound = existing.controller.session_incarnation != incarnation;
        if rebound {
            let expected = args.if_revision.ok_or_else(|| {
                CliError::data(
                    "orchestration-revision-required",
                    "continuity rebind requires --if-revision",
                    Some(
                        json!({ "run_id": existing.run_id, "current_revision": existing.revision }),
                    ),
                )
            })?;
            ensure_revision(expected, existing.revision, "run")?;
            if orchestration::session_ref_is_live(context, &existing.controller) {
                return Err(CliError::data(
                    "controller-incarnation-still-live",
                    "prior controller incarnation is still live; continuity rebind refused",
                    Some(
                        json!({ "run_id": existing.run_id, "current_revision": existing.revision }),
                    ),
                ));
            }
            existing.controller = session_ref(context, &record, &incarnation);
            existing.revision = existing.revision.saturating_add(1);
            existing.updated_at = timestamp();
        }
        let outcome = run_outcome(existing, rebound);
        store_receipt(
            &mut locked.registry,
            &record,
            &incarnation,
            &args.idempotency_key,
            "init",
            &request_digest,
            outcome.clone(),
        )?;
        locked.save()?;
        return Ok(outcome);
    }

    let packet_digest = orchestration::store_packet(context, &packet_value)?;
    let run_id = packet
        .run_id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    orchestration::validate_slug("run id", &run_id, 128)?;
    if locked.registry.runs.contains_key(&run_id) {
        return Err(CliError::data(
            "run-exists",
            "orchestration run already exists",
            Some(json!({ "run_id": run_id })),
        ));
    }
    let now = timestamp();
    let run = RunRecord {
        schema_version: orchestration::RUN_SCHEMA.to_string(),
        run_id: run_id.clone(),
        revision: 1,
        state: "active".to_string(),
        tier: packet.tier.clone(),
        objective_summary: packet.objective_summary.clone(),
        objective_packet_digest: packet_digest,
        controller: session_ref(context, &record, &incarnation),
        durable_refs: packet.durable_refs.clone(),
        ephemeral: false,
        checkpoint: packet
            .next_action
            .as_ref()
            .map(|next_action| RunCheckpoint {
                revision: 1,
                summary: "Run initialized".to_string(),
                next_action: next_action.clone(),
                updated_at: now.clone(),
            }),
        created_at: now.clone(),
        updated_at: now,
    };
    let outcome = run_outcome(&run, false);
    locked.registry.runs.insert(run_id, run);
    store_receipt(
        &mut locked.registry,
        &record,
        &incarnation,
        &args.idempotency_key,
        "init",
        &request_digest,
        outcome.clone(),
    )?;
    locked.save()?;
    Ok(outcome)
}

/// Re-bind an existing run to the caller's current session incarnation after a
/// resume, using the run's stored objective packet to re-acquire the
/// work-context claim. Mirrors the continuity-rebind preconditions in
/// [`run_init`] but requires no packet file: the server already holds the
/// packet. A same-incarnation caller is a no-op that still confirms the claim.
fn run_rebind(context: &CliContext, args: RunMutationArgs) -> Result<Value, CliError> {
    validate_idempotency_key(&args.idempotency_key)?;
    let (record, incarnation) = authenticated_self(context)?;

    // Recover the run's stored objective packet (read-only) so the work-context
    // claim can be re-acquired without the caller re-supplying the packet file.
    let packet_digest = {
        let registry = orchestration::load_registry_readonly(context)?;
        let existing = registry
            .runs
            .values()
            .find(|run| {
                run.controller.session_id == record.id
                    && run.controller.session_created_at == record.created_at
            })
            .ok_or_else(|| {
                not_found(
                    "orchestration-self-not-found",
                    "authenticated session has no orchestration relationship",
                )
            })?;
        existing.objective_packet_digest.clone()
    };
    let packet_value = orchestration::read_packet(context, &packet_digest)?;
    let packet: ObjectivePacket = serde_json::from_value(packet_value)
        .map_err(|_| invalid_input("stored objective packet is invalid"))?;
    ensure_or_acquire_claim(
        context,
        &record,
        &packet.work_context,
        &args.idempotency_key,
        None,
        false,
    )?;

    let request_digest = crate::coordination::request_digest(
        "main-agent-rebind",
        &json!({ "if_revision": args.if_revision }),
    );
    let mut locked = orchestration::lock_registry(context)?;
    if let Some(value) = idempotency_replay(
        &locked.registry,
        &record,
        &incarnation,
        &args.idempotency_key,
        "rebind",
        &request_digest,
    )? {
        return Ok(value);
    }
    let existing = locked
        .registry
        .runs
        .values_mut()
        .find(|run| {
            run.controller.session_id == record.id
                && run.controller.session_created_at == record.created_at
        })
        .ok_or_else(|| {
            not_found(
                "orchestration-self-not-found",
                "authenticated session has no orchestration relationship",
            )
        })?;
    let rebound = existing.controller.session_incarnation != incarnation;
    if rebound {
        ensure_revision(args.if_revision, existing.revision, "run")?;
        if orchestration::session_ref_is_live(context, &existing.controller) {
            return Err(CliError::data(
                "controller-incarnation-still-live",
                "prior controller incarnation is still live; continuity rebind refused",
                Some(json!({ "run_id": existing.run_id, "current_revision": existing.revision })),
            ));
        }
        existing.controller = session_ref(context, &record, &incarnation);
        existing.revision = existing.revision.saturating_add(1);
        existing.updated_at = timestamp();
    }
    let outcome = run_outcome(existing, rebound);
    store_receipt(
        &mut locked.registry,
        &record,
        &incarnation,
        &args.idempotency_key,
        "rebind",
        &request_digest,
        outcome.clone(),
    )?;
    locked.save()?;
    Ok(outcome)
}

fn run_self_show(context: &CliContext, _args: ReadArgs) -> Result<Value, CliError> {
    let (record, incarnation) = authenticated_self(context)?;
    let registry = orchestration::load_registry_readonly(context)?;
    match resolve_principal(&registry, &record, &incarnation)? {
        Principal::Main {
            run,
            rebind_required,
        } => Ok(json!({
            "schema_version": "main-agent.self.v1",
            "role": "main",
            "run": private_run_view(context, &run)?,
            "rebind_required": rebind_required
        })),
        Principal::Worker {
            assignment,
            rebind_required,
        } => Ok(json!({
            "schema_version": "main-agent.self.v1",
            "role": "worker",
            "assignment": private_assignment_view(context, &assignment)?,
            "rebind_required": rebind_required
        })),
    }
}

fn run_rehydrate(context: &CliContext, _args: RehydrateArgs) -> Result<Value, CliError> {
    let (record, incarnation) = authenticated_self(context)?;
    let registry = orchestration::load_registry_readonly(context)?;
    let observed_at = timestamp();
    match resolve_principal(&registry, &record, &incarnation)? {
        Principal::Main {
            run,
            rebind_required,
        } => {
            let assignments = registry
                .assignments
                .values()
                .filter(|assignment| assignment.run_id == run.run_id)
                .map(public_assignment_view)
                .collect::<Vec<_>>();
            Ok(json!({
                "schema_version": "main-agent.rehydrate.v1",
                "durable": {
                    "role": "main",
                    "run": private_run_view(context, &run)?,
                    "assignments": assignments
                },
                "observed": {
                    "observed_at": observed_at,
                    "rebind_required": rebind_required,
                    "controller_current": orchestration::controller_is_current(context, &run)
                }
            }))
        }
        Principal::Worker {
            assignment,
            rebind_required,
        } => Ok(json!({
            "schema_version": "main-agent.rehydrate.v1",
            "durable": {
                "role": "worker",
                "assignment": private_assignment_view(context, &assignment)?
            },
            "observed": {
                "observed_at": observed_at,
                "rebind_required": rebind_required,
                "manager_current": orchestration::session_ref_is_live(context, &assignment.primary_manager)
            }
        })),
    }
}

fn run_controller_recover(
    context: &CliContext,
    args: ControllerRecoverArgs,
) -> Result<Value, CliError> {
    validate_idempotency_key(&args.idempotency_key)?;
    crate::coordination::ensure_recovery_registry_schema(context)?;
    let session_id = env::var("AGENT_SESSION_ID")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            CliError::usage(
                "controller-recovery-session-unavailable",
                "controller recovery requires AGENT_SESSION_ID for the exact current session",
                None,
            )
        })?;
    let capability_file = env::var_os("AGENT_SESSION_CAPABILITY_FILE")
        .map(PathBuf::from)
        .ok_or_else(|| {
            CliError::usage(
                "controller-recovery-capability-unavailable",
                "controller recovery requires AGENT_SESSION_CAPABILITY_FILE",
                None,
            )
        })?;
    let record = load_session_record(context, &session_id)?;
    let incarnation = record
        .runtime
        .as_ref()
        .map(|runtime| runtime.launch_id.clone())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            CliError::data(
                "controller-recovery-incarnation-conflict",
                "the exact controller incarnation is unavailable",
                None,
            )
        })?;
    let registry = orchestration::load_registry_readonly(context)?;
    let controller_run = match resolve_principal(&registry, &record, &incarnation)? {
        Principal::Main {
            run,
            rebind_required: false,
        } => *run,
        Principal::Main {
            rebind_required: true,
            ..
        } => {
            return Err(CliError::data(
                "controller-recovery-incarnation-conflict",
                "the durable run is bound to a different controller incarnation",
                None,
            ));
        }
        Principal::Worker { .. } => {
            return Err(CliError::data(
                "controller-recovery-role",
                "controller recovery is available only to the exact Main Agent controller",
                None,
            ));
        }
    };
    crate::coordination::validate_recovery_capability(context, &record, &capability_file)?;
    let request_digest = crate::coordination::request_digest(
        "main-agent-controller-recover",
        &json!({
            "session_id": record.id,
            "session_incarnation": incarnation,
            "run_id": controller_run.run_id
        }),
    );
    if let Some(outcome) = idempotency_replay(
        &registry,
        &record,
        &incarnation,
        &args.idempotency_key,
        "controller-recover",
        &request_digest,
    )? {
        return Ok(outcome);
    }
    let mut runtime = crate::coordination_runtime_evidence(&record)?;
    #[cfg(debug_assertions)]
    match env::var("NILS_AGENT_SESSION_TEST_CONTROLLER_RUNTIME_STATUS").as_deref() {
        Ok("stopped") => runtime.status = crate::CoordinationRuntimeStatus::Stopped,
        Ok("unknown") => runtime.status = crate::CoordinationRuntimeStatus::Unknown,
        _ => {}
    }
    if runtime.status != crate::CoordinationRuntimeStatus::Running {
        return Err(CliError::runtime(
            "controller-recovery-runtime-uncertain",
            "controller recovery requires the exact unchanged live runtime",
            Some(json!({
                "runtime_status": match runtime.status {
                    crate::CoordinationRuntimeStatus::Running => "running",
                    crate::CoordinationRuntimeStatus::Stopped => "stopped",
                    crate::CoordinationRuntimeStatus::Unknown => "unknown"
                },
                "required_action": "preserve the controller and use broker status/adopt only after exact runtime identity is proven"
            })),
        ));
    }
    let quiescence =
        crate::coordination::lock_session_quiescence(context, &session_id, &incarnation)?;
    if !quiescence.broker_present || !quiescence.broker_identity_matched {
        return Err(CliError::data(
            "controller-recovery-incarnation-conflict",
            "the coordination broker does not match the exact controller incarnation",
            None,
        ));
    }
    if quiescence.active_operation || quiescence.uncertain_operation {
        return Err(CliError::data(
            "controller-recovery-operation-fenced",
            "controller recovery refuses an active or uncertain mutation operation",
            None,
        ));
    }
    if !quiescence.active_claim {
        return Err(CliError::data(
            "controller-recovery-claim-unavailable",
            "controller recovery requires the existing active Main Agent claim",
            None,
        ));
    }
    if quiescence.broker_runtime_identity_digest.as_deref()
        != Some(runtime.identity_digest.as_str())
        || quiescence.broker_generation != record.runtime.as_ref().map(|runtime| runtime.generation)
    {
        return Err(CliError::runtime(
            "controller-recovery-runtime-uncertain",
            "the live controller runtime does not match the durable broker identity",
            None,
        ));
    }
    let claim_revision = quiescence.claim_revision;
    let was_healthy = quiescence.broker_authoritative;
    drop(quiescence);

    let proof = json!({
        "schema_version": "agent-session.coordination-recovery-proof.v1",
        "session_incarnation": incarnation,
        "generation": record.runtime.as_ref().map(|runtime| runtime.generation).unwrap_or_default()
    });
    let directory = session_dir(context, &record.id).join("coordination");
    fs::create_dir_all(&directory).map_err(|_| {
        CliError::runtime(
            "controller-recovery-proof-unavailable",
            "the bounded controller recovery proof could not be prepared",
            None,
        )
    })?;
    let proof_path = directory.join(format!(
        "main-agent-controller-recovery-{}.json",
        uuid::Uuid::new_v4()
    ));
    let proof_bytes = serde_json::to_vec(&proof).map_err(|_| {
        CliError::runtime(
            "controller-recovery-proof-unavailable",
            "the bounded controller recovery proof could not be prepared",
            None,
        )
    })?;
    write_atomic(&proof_path, &proof_bytes, SECRET_FILE_MODE).map_err(|_| {
        CliError::runtime(
            "controller-recovery-proof-unavailable",
            "the bounded controller recovery proof could not be prepared",
            None,
        )
    })?;
    let primitive = crate::coordination::recover_broker(
        context,
        cli::BrokerRecoveryArgs {
            session: record.id.clone(),
            capability_file: Some(capability_file),
            proof_file: proof_path.clone(),
            idempotency_key: args.idempotency_key.clone(),
            operation: None,
            if_revision: None,
            attest_inactive: false,
            format: OutputFormat::Json,
        },
    );
    let _ = fs::remove_file(proof_path);
    let recovery = match primitive {
        Ok(value) => {
            if value["recovery"] != "adopted" {
                return Err(CliError::runtime(
                    "controller-recovery-verification-failed",
                    "controller broker recovery returned an unexpected result",
                    None,
                ));
            }
            "adopted"
        }
        Err(error) if error.code() == "coordination-broker-not-lost" && was_healthy => {
            "healthy_noop"
        }
        Err(error) => return Err(error),
    };

    let (verified_record, verified_incarnation) = authenticated_self(context)?;
    if verified_record.id != record.id || verified_incarnation != incarnation {
        return Err(CliError::data(
            "controller-recovery-verification-failed",
            "authenticated controller identity changed after broker recovery",
            None,
        ));
    }
    let verified_registry = orchestration::load_registry_readonly(context)?;
    let verified_run =
        match resolve_principal(&verified_registry, &verified_record, &verified_incarnation)? {
            Principal::Main {
                run,
                rebind_required: false,
            } if run.run_id == controller_run.run_id => *run,
            _ => {
                return Err(CliError::data(
                    "controller-recovery-verification-failed",
                    "authenticated Main Agent run state did not verify after broker recovery",
                    None,
                ));
            }
        };
    let verified_quiescence = crate::coordination::lock_session_quiescence(
        context,
        &verified_record.id,
        &verified_incarnation,
    )?;
    if !verified_quiescence.broker_authoritative
        || !verified_quiescence.active_claim
        || verified_quiescence.claim_revision != claim_revision
    {
        return Err(CliError::data(
            "controller-recovery-verification-failed",
            "controller broker or claim continuity did not verify after recovery",
            None,
        ));
    }
    let outcome = json!({
        "schema_version": "main-agent.controller-recovery.v1",
        "recovery": recovery,
        "session_id": verified_record.id,
        "session_incarnation": verified_incarnation,
        "broker": {
            "authoritative": true,
            "generation": verified_quiescence.broker_generation
        },
        "claim": {
            "active": true,
            "revision": verified_quiescence.claim_revision,
            "retained": true
        },
        "run": {
            "run_id": verified_run.run_id,
            "revision": verified_run.revision,
            "rebind_required": false
        },
        "forbidden_side_effects": {
            "account_changed": false,
            "provider_resumed_or_replaced": false,
            "prompt_resent": false,
            "enter_sent": false,
            "operation_fence_cleared": false
        }
    });
    drop(verified_quiescence);

    let mut locked = orchestration::lock_registry(context)?;
    if let Some(existing) = idempotency_replay(
        &locked.registry,
        &record,
        &incarnation,
        &args.idempotency_key,
        "controller-recover",
        &request_digest,
    )? {
        return Ok(existing);
    }
    match resolve_principal(&locked.registry, &record, &incarnation)? {
        Principal::Main {
            run,
            rebind_required: false,
        } if run.run_id == controller_run.run_id => {}
        _ => {
            return Err(CliError::data(
                "controller-recovery-verification-failed",
                "authenticated Main Agent run state changed before recovery could be recorded",
                None,
            ));
        }
    }
    store_receipt(
        &mut locked.registry,
        &record,
        &incarnation,
        &args.idempotency_key,
        "controller-recover",
        &request_digest,
        outcome.clone(),
    )?;
    locked.save()?;
    Ok(outcome)
}

fn run_status(context: &CliContext, _args: ReadArgs) -> Result<Value, CliError> {
    let (record, incarnation) = authenticated_self(context)?;
    let registry = orchestration::load_registry_readonly(context)?;
    match resolve_principal(&registry, &record, &incarnation)? {
        Principal::Main {
            run,
            rebind_required,
        } => {
            let assignments = registry
                .assignments
                .values()
                .filter(|assignment| assignment.run_id == run.run_id)
                .map(public_assignment_view)
                .collect::<Vec<_>>();
            Ok(json!({
                "schema_version": "main-agent.status.v1",
                "role": "main",
                "run": public_run_view(&run),
                "assignments": assignments,
                "rebind_required": rebind_required
            }))
        }
        Principal::Worker {
            assignment,
            rebind_required,
        } => Ok(json!({
            "schema_version": "main-agent.status.v1",
            "role": "worker",
            "assignment": public_assignment_view(&assignment),
            "rebind_required": rebind_required
        })),
    }
}

fn run_checkpoint(context: &CliContext, args: CheckpointArgs) -> Result<Value, CliError> {
    validate_idempotency_key(&args.idempotency_key)?;
    let input: CheckpointInput =
        crate::coordination::read_bounded_json(&args.file, 64 * 1024, "invalid-checkpoint")?;
    validate_checkpoint(&input)?;
    let (record, incarnation) = authenticated_self(context)?;
    orchestration::ensure_session_not_quarantined(context, &record)?;
    ensure_active_claim(context, &record)?;
    let mut locked = orchestration::lock_registry(context)?;
    let principal = resolve_principal(&locked.registry, &record, &incarnation)?;
    let request_digest = crate::coordination::request_digest("main-agent-checkpoint", &input);
    if let Some(value) = idempotency_replay(
        &locked.registry,
        &record,
        &incarnation,
        &args.idempotency_key,
        "checkpoint",
        &request_digest,
    )? {
        return Ok(value);
    }
    let outcome = match principal {
        Principal::Main {
            run,
            rebind_required: false,
        } => {
            let current = locked
                .registry
                .runs
                .get_mut(&run.run_id)
                .expect("run exists");
            ensure_revision(args.if_revision, current.revision, "run")?;
            current.revision = current.revision.saturating_add(1);
            current.checkpoint = Some(RunCheckpoint {
                revision: current.revision,
                summary: input.summary,
                next_action: input.next_action,
                updated_at: timestamp(),
            });
            current.updated_at = timestamp();
            json!({ "schema_version": "main-agent.checkpoint-result.v1", "role": "main", "run": public_run_view(current) })
        }
        Principal::Worker {
            assignment,
            rebind_required,
        } => {
            let current = locked
                .registry
                .assignments
                .get_mut(&assignment.assignment_id)
                .expect("assignment exists");
            if current.worker_quarantine.is_some() {
                return Err(CliError::data(
                    "worker-quarantined",
                    "worker checkpoints are disabled after stopped-runtime recovery reconciliation",
                    Some(json!({
                        "assignment_id": current.assignment_id,
                        "current_revision": current.revision
                    })),
                ));
            }
            ensure_account_handoff_not_in_flight(current)?;
            ensure_revision(args.if_revision, current.revision, "assignment")?;
            if matches!(
                current.state.as_str(),
                "accepted" | "released" | "cancelled"
            ) {
                return Err(CliError::data(
                    "assignment-terminal",
                    "worker checkpoints are immutable after a manager-terminal transition",
                    Some(json!({
                        "assignment_id": current.assignment_id,
                        "state": current.state,
                        "revision": current.revision
                    })),
                ));
            }
            if rebind_required {
                let previous_worker = current
                    .worker
                    .as_ref()
                    .expect("resolved worker assignment has a worker")
                    .clone();
                if orchestration::session_ref_is_live(context, &previous_worker) {
                    return Err(CliError::data(
                        "worker-incarnation-still-live",
                        "prior worker incarnation is still live; continuity rebind refused",
                        Some(json!({
                            "assignment_id": current.assignment_id,
                            "current_revision": current.revision
                        })),
                    ));
                }
                current.previous_worker = Some(previous_worker);
                current.worker = Some(session_ref(context, &record, &incarnation));
            }
            if let Some(state) = input.state {
                if !matches!(state.as_str(), "working" | "blocked" | "submitted") {
                    return Err(invalid_input("worker checkpoint state is invalid"));
                }
                if !matches!(
                    current.state.as_str(),
                    "submitted" | "accepted" | "released" | "cancelled"
                ) {
                    current.state = state;
                }
            }
            current.revision = current.revision.saturating_add(1);
            current.checkpoint = Some(RunCheckpoint {
                revision: current.revision,
                summary: input.summary,
                next_action: input.next_action,
                updated_at: timestamp(),
            });
            current.result_summary = input.result_summary;
            current.blocker_summary = input.blocker_summary;
            current.updated_at = timestamp();
            json!({ "schema_version": "main-agent.checkpoint-result.v1", "role": "worker", "assignment": public_assignment_view(current) })
        }
        _ => return Err(rebind_required()),
    };
    store_receipt(
        &mut locked.registry,
        &record,
        &incarnation,
        &args.idempotency_key,
        "checkpoint",
        &request_digest,
        outcome.clone(),
    )?;
    locked.save()?;
    Ok(outcome)
}

fn run_bootstrap(context: &CliContext, args: BootstrapArgs) -> Result<Value, CliError> {
    validate_idempotency_key(&args.idempotency_key)?;
    let (record, incarnation) = authenticated_self(context)?;
    orchestration::ensure_session_not_quarantined(context, &record)?;
    let (assignment, tier, rebind_from) = {
        let registry = orchestration::load_registry_readonly(context)?;
        let principal = resolve_principal(&registry, &record, &incarnation)?;
        let (assignment, rebind_from) = match principal {
            Principal::Worker {
                assignment,
                rebind_required: false,
            } => (*assignment, None),
            Principal::Worker {
                assignment,
                rebind_required: true,
            } => {
                let previous = assignment
                    .worker
                    .as_ref()
                    .ok_or_else(rebind_required)?
                    .clone();
                if orchestration::session_ref_is_live(context, &previous) {
                    return Err(CliError::data(
                        "worker-incarnation-still-live",
                        "prior worker incarnation is still live; resume bootstrap refused",
                        Some(json!({
                            "assignment_id": assignment.assignment_id,
                            "current_revision": assignment.revision
                        })),
                    ));
                }
                (*assignment, Some(previous))
            }
            Principal::Main { .. } => {
                return Err(CliError::data(
                    "worker-bootstrap-role",
                    "bootstrap requires an authenticated worker assignment",
                    None,
                ));
            }
        };
        let tier = registry
            .runs
            .get(&assignment.run_id)
            .map(|run| run.tier.clone())
            .ok_or_else(|| not_found("run-not-found", "orchestration run was not found"))?;
        (assignment, tier, rebind_from)
    };
    if !matches!(assignment.state.as_str(), "starting" | "working") {
        return Err(CliError::data(
            "worker-bootstrap-state",
            "worker bootstrap requires a starting or working assignment",
            Some(json!({
                "assignment_id": assignment.assignment_id,
                "current_revision": assignment.revision,
                "state": assignment.state
            })),
        ));
    }
    let packet_value = orchestration::read_packet(context, &assignment.private_packet_digest)?;
    let packet: AssignmentInput = serde_json::from_value(packet_value)
        .map_err(|_| invalid_input("stored assignment packet is invalid"))?;
    validate_assignment_input(&packet)?;
    if let Err(error) = validate_bootstrap_checkout_binding(&record, &assignment, &packet) {
        if rebind_from.is_none() {
            record_preclaim_bootstrap_blocker(
                context,
                &record,
                &incarnation,
                &assignment,
                error.code(),
            )?;
        }
        return Err(error);
    }
    let repository = packet.repository.clone().ok_or_else(|| {
        invalid_input("worker bootstrap requires the assignment packet to declare a repository")
    })?;
    let work_context = WorkContextInput {
        schema_version: WORK_CONTEXT_INPUT_VERSION.to_string(),
        intent: "implementation".to_string(),
        tier,
        repositories: vec![repository.clone()],
        // `AssignmentInput::worktree` is durable routing metadata and may be
        // the literal managed-worktree path. The claim schema accepts only
        // HMAC fingerprints. `claims::claim` derives that fingerprint from the
        // authenticated worker session's canonical cwd, so never serialize the
        // routing value into this field.
        worktrees: Vec::new(),
        provider_refs: Vec::new(),
        plan_refs: Vec::new(),
        scopes: packet
            .scopes
            .iter()
            .map(|value| Scope {
                kind: ScopeKind::PathPrefix,
                repository: repository.clone(),
                value: value.clone(),
            })
            .collect(),
        summary: packet.task_summary.clone(),
    };
    if let Err(error) = ensure_or_acquire_claim(
        context,
        &record,
        &work_context,
        &args.idempotency_key,
        rebind_from.as_ref(),
        true,
    ) {
        if rebind_from.is_none() {
            record_preclaim_bootstrap_blocker(
                context,
                &record,
                &incarnation,
                &assignment,
                error.code(),
            )?;
        }
        return Err(error);
    }
    if rebind_from.is_some() {
        pause_bootstrap_guidance_for_test()?;
    }

    let checkpoint = CheckpointInput {
        schema_version: CHECKPOINT_INPUT_SCHEMA.to_string(),
        summary: "Assignment authenticated; worker bootstrap complete".to_string(),
        next_action: "Execute the private assignment packet".to_string(),
        state: Some("working".to_string()),
        result_summary: None,
        blocker_summary: None,
    };
    let directory = session_dir(context, &record.id).join("coordination");
    fs::create_dir_all(&directory)
        .map_err(|_| invalid_input("bootstrap checkpoint directory is unavailable"))?;
    let checkpoint_path = directory.join(format!(
        "main-agent-bootstrap-{}.json",
        uuid::Uuid::new_v4()
    ));
    let bytes = serde_json::to_vec(&checkpoint)
        .map_err(|_| invalid_input("bootstrap checkpoint is invalid"))?;
    write_atomic(&checkpoint_path, &bytes, SECRET_FILE_MODE)
        .map_err(|_| invalid_input("bootstrap checkpoint could not be prepared"))?;
    let result = run_checkpoint(
        context,
        CheckpointArgs {
            file: checkpoint_path.clone(),
            if_revision: assignment.revision,
            idempotency_key: args.idempotency_key,
            format: OutputFormat::Json,
        },
    );
    let _ = fs::remove_file(checkpoint_path);
    result?;

    let registry = orchestration::load_registry_readonly(context)?;
    let current = registry
        .assignments
        .get(&assignment.assignment_id)
        .ok_or_else(|| not_found("assignment-not-found", "assignment was not found"))?
        .clone();
    drop(registry);
    if let Some(previous) = rebind_from.as_ref() {
        carry_forward_bootstrap_guidance_with_authorization(
            context,
            &record,
            &incarnation,
            &current,
            previous,
        )?;
    }
    Ok(json!({
        "schema_version": "main-agent.bootstrap-result.v1",
        "claim": "active",
        "assignment": private_assignment_view(context, &current)?
    }))
}

fn carry_forward_bootstrap_guidance_with_authorization(
    context: &CliContext,
    record: &SessionRecord,
    incarnation: &str,
    assignment: &AssignmentRecord,
    previous_worker: &SessionRef,
) -> Result<(), CliError> {
    let current_worker = assignment
        .worker
        .as_ref()
        .filter(|worker| orchestration::session_ref_matches(worker, record, incarnation))
        .cloned()
        .ok_or_else(|| {
            CliError::data(
                "guidance-continuity-conflict",
                "resumed worker identity changed before guidance reconciliation",
                None,
            )
        })?;
    let expected_assignment_id = assignment.assignment_id.clone();
    let expected_run_id = assignment.run_id.clone();
    let expected_revision = assignment.revision;
    let expected_manager = assignment.primary_manager.clone();
    let worker_authority =
        crate::lock_exact_session_authority(context, &current_worker.session_id)?
            .ok_or_else(|| not_found("worker-session-not-found", "worker session was not found"))?;
    let worker_incarnation = worker_authority
        .record
        .runtime
        .as_ref()
        .map(|runtime| runtime.launch_id.as_str())
        .unwrap_or_default();
    if !orchestration::session_ref_matches(
        &current_worker,
        &worker_authority.record,
        worker_incarnation,
    ) {
        return Err(CliError::data(
            "guidance-continuity-conflict",
            "resumed worker authority changed before guidance reconciliation",
            None,
        ));
    }
    crate::coordination::carry_forward_unread_controller_guidance_with_authorization(
        context,
        &current_worker.session_id,
        &previous_worker.session_incarnation,
        &current_worker.session_incarnation,
        &expected_manager.session_id,
        &expected_manager.session_incarnation,
        || {
            let locked = orchestration::lock_registry(context)?;
            let current = locked
                .registry
                .assignments
                .get(&expected_assignment_id)
                .filter(|current| current.run_id == expected_run_id)
                .ok_or_else(|| not_found("assignment-not-found", "assignment was not found"))?;
            let run = locked
                .registry
                .runs
                .get(&expected_run_id)
                .filter(|run| {
                    run.state == "active"
                        && run.controller.session_id == expected_manager.session_id
                        && run.controller.session_incarnation
                            == expected_manager.session_incarnation
                })
                .ok_or_else(|| {
                    CliError::data(
                        "guidance-continuity-conflict",
                        "assignment controller changed before guidance reconciliation",
                        None,
                    )
                })?;
            if run.run_id != current.run_id
                || current.revision != expected_revision
                || current.primary_manager != expected_manager
                || current.worker.as_ref() != Some(&current_worker)
                || current.previous_worker.as_ref() != Some(previous_worker)
            {
                return Err(CliError::data(
                    "guidance-continuity-conflict",
                    "assignment guidance routing changed before reconciliation",
                    None,
                ));
            }
            Ok(locked)
        },
    )?;
    Ok(())
}

fn pause_bootstrap_guidance_for_test() -> Result<(), CliError> {
    #[cfg(debug_assertions)]
    if let Some(directory) =
        env::var_os("NILS_AGENT_SESSION_TEST_BOOTSTRAP_GUIDANCE_BARRIER_DIR").map(PathBuf::from)
    {
        fs::write(directory.join("ready"), b"bootstrap-claim-rebound").map_err(|_| {
            CliError::runtime(
                "test-barrier-unavailable",
                "bootstrap guidance test barrier could not be signalled",
                None,
            )
        })?;
        let release = directory.join("release");
        let deadline = Instant::now() + Duration::from_secs(10);
        while !release.is_file() {
            if Instant::now() >= deadline {
                return Err(CliError::runtime(
                    "test-barrier-timeout",
                    "bootstrap guidance test barrier timed out",
                    None,
                ));
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
    Ok(())
}

fn record_preclaim_bootstrap_blocker(
    context: &CliContext,
    record: &SessionRecord,
    incarnation: &str,
    assignment: &AssignmentRecord,
    failure_code: &str,
) -> Result<(), CliError> {
    let (active_claim, active_operation) =
        crate::coordination::session_has_active_claim_or_operation(
            context,
            &record.id,
            incarnation,
        )?;
    if active_claim || active_operation {
        return Ok(());
    }
    let mut locked = orchestration::lock_registry(context)?;
    let current = locked
        .registry
        .assignments
        .get_mut(&assignment.assignment_id)
        .ok_or_else(|| not_found("assignment-not-found", "assignment was not found"))?;
    if current.revision != assignment.revision
        || current.state != "starting"
        || !current
            .worker
            .as_ref()
            .is_some_and(|worker| orchestration::session_ref_matches(worker, record, incarnation))
    {
        return Err(CliError::data(
            "assignment-bootstrap-blocker-conflict",
            "assignment changed before the pre-claim blocker could be recorded",
            Some(json!({
                "assignment_id": current.assignment_id,
                "current_revision": current.revision,
                "state": current.state
            })),
        ));
    }
    ensure_account_handoff_not_in_flight(current)?;
    current.state = "blocked".to_string();
    current.revision = current.revision.saturating_add(1);
    current.updated_at = timestamp();
    let blocker = format!("[pre-claim:{failure_code}] worker bootstrap failed");
    current.blocker_summary = Some(blocker.clone());
    current.checkpoint = Some(RunCheckpoint {
        revision: current.revision,
        summary: "Worker bootstrap failed before claim acquisition".to_string(),
        next_action:
            "Main Agent must diagnose, then cancel or safely reassign this exact assignment"
                .to_string(),
        updated_at: current.updated_at.clone(),
    });
    locked.save()
}

fn run_worker(context: &CliContext, args: WorkerArgs) -> Result<Value, CliError> {
    match args.command {
        WorkerCommand::Start(args) => run_worker_start(context, args),
        WorkerCommand::List(_) => run_worker_list(context),
        WorkerCommand::Show(args) => run_worker_show(context, args),
        WorkerCommand::Wait(args) => run_worker_wait(context, args),
        WorkerCommand::Message(args) => run_worker_message(context, args),
        WorkerCommand::GuidanceReconcile(args) => run_worker_guidance_reconcile(context, args),
        WorkerCommand::GuidanceQuarantine(args) => run_worker_guidance_quarantine(context, args),
        WorkerCommand::AccountHandoff(args) => run_worker_account_handoff(context, args),
        WorkerCommand::AccountHandoffCancel(args) => {
            run_worker_account_handoff_cancel(context, args)
        }
        WorkerCommand::RequestChanges(args) => run_worker_request_changes(context, args),
        WorkerCommand::Accept(args) => {
            run_assignment_state(context, args, "submitted", "accepted", "worker-accept")
        }
        WorkerCommand::Release(args) => {
            run_assignment_state(context, args, "accepted", "released", "worker-release")
        }
        WorkerCommand::Delete(args) => run_worker_delete(context, args),
        WorkerCommand::Retire(args) => run_worker_retire(context, args),
        WorkerCommand::Diagnose(args) => run_worker_diagnose(context, args),
        WorkerCommand::Supervise(args) => run_worker_supervise(context, args),
        WorkerCommand::SubmitRecovery(args) => run_worker_submit_recovery(context, args),
        WorkerCommand::ReconcileRecovery(args) => run_worker_reconcile_recovery(context, args),
        WorkerCommand::Cancel(args) => run_worker_cancel(context, args),
        WorkerCommand::Reassign(args) => run_worker_reassign(context, args),
    }
}

/// Dispatch a single (`--assignment-file`) or batch (`--batch DIR`) launch.
/// clap's `conflicts_with` rejects supplying both; this rejects supplying
/// neither.
fn run_worker_start(context: &CliContext, args: WorkerStartArgs) -> Result<Value, CliError> {
    match (args.assignment_file.is_some(), args.batch.clone()) {
        (true, None) => run_worker_start_single(context, args),
        (false, Some(dir)) => run_worker_start_batch(context, &args, &dir),
        (true, Some(_)) => Err(invalid_input(
            "worker start takes either --assignment-file or --batch, not both",
        )),
        (false, None) => Err(CliError::usage(
            "worker-start-source",
            "worker start requires --assignment-file or --batch",
            None,
        )),
    }
}

fn run_worker_start_single(context: &CliContext, args: WorkerStartArgs) -> Result<Value, CliError> {
    let assignment_file = args
        .assignment_file
        .as_ref()
        .ok_or_else(|| invalid_input("worker start requires --assignment-file"))?;
    let input: AssignmentInput = crate::coordination::read_bounded_json(
        assignment_file,
        256 * 1024,
        "invalid-assignment-packet",
    )?;
    run_worker_start_single_input(context, args, input, None)
}

fn run_worker_start_single_input(
    context: &CliContext,
    args: WorkerStartArgs,
    input: AssignmentInput,
    batch_lane: Option<&BatchLaneFence<'_>>,
) -> Result<Value, CliError> {
    validate_idempotency_key(&args.idempotency_key)?;
    let await_ready = parse_await_ready(&args.await_ready)?;
    validate_assignment_input(&input)?;
    let (record, incarnation) = authenticated_self(context)?;
    ensure_active_claim(context, &record)?;
    let (packet_value, request_digest, legacy_request_digest) =
        worker_start_request_digests(&input, &args.await_ready)?;
    let mut locked = orchestration::lock_registry(context)?;
    if let Some(batch_lane) = batch_lane {
        renew_worker_start_batch_lane_locked(&mut locked.registry, batch_lane)?;
        locked.save()?;
    }
    let replay = worker_start_idempotency_replay(
        &locked.registry,
        &record,
        &incarnation,
        &args.idempotency_key,
        &request_digest,
        &legacy_request_digest,
    )?;
    let pending_start = match replay {
        Some(value) if worker_start_readiness_is_pending(&value) => {
            drop(locked);
            return finish_worker_start_readiness(
                context,
                &record,
                &incarnation,
                &args.idempotency_key,
                &request_digest,
                value,
                None,
            );
        }
        Some(value) if worker_start_is_pending(&value) => {
            let assignment_id = value["assignment_id"]
                .as_str()
                .ok_or_else(|| invalid_input("pending worker start receipt is invalid"))?
                .to_string();
            let worker_session_id = value["worker_session_id"]
                .as_str()
                .map(str::to_string)
                .or_else(|| input.launch.session_id.clone())
                .unwrap_or_else(|| {
                    retry_stable_worker_session_id(&assignment_id, &legacy_request_digest)
                });
            Some((assignment_id, worker_session_id))
        }
        Some(value) => return Ok(value),
        None => None,
    };
    let assignment_id = input
        .assignment_id
        .clone()
        .or_else(|| pending_start.as_ref().map(|(id, _)| id.clone()))
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    orchestration::validate_slug("assignment id", &assignment_id, 128)?;
    let worker_session_id = pending_start
        .as_ref()
        .map(|(_, id)| id.clone())
        .or_else(|| input.launch.session_id.clone())
        .unwrap_or_else(|| retry_stable_worker_session_id(&assignment_id, &legacy_request_digest));
    crate::validate_id(&worker_session_id)?;
    let run = require_current_main(&locked.registry, &record, &incarnation)?.clone();
    // T2: the run-revision fence is now advisory. Assignment creation is fenced
    // by the active claim, current-main check, and assignment-absence below, so
    // parallel/batch starts no longer have to serialize on a shared run
    // revision. When a caller does supply --if-run-revision, still honor it.
    if let Some(expected) = args.if_run_revision {
        ensure_revision(expected, run.revision, "run")?;
    }
    if pending_start.is_some() {
        let expected_packet_digest = orchestration::packet_digest(&packet_value)?;
        let current = locked
            .registry
            .assignments
            .get(&assignment_id)
            .ok_or_else(|| not_found("assignment-not-found", "assignment was not found"))?;
        ensure_primary_manager(current, &record, &incarnation)?;
        if current.run_id != run.run_id
            || current.state != "starting"
            || current.revision != 1
            || current.worker.is_some()
            || current.private_packet_digest != expected_packet_digest
        {
            return Err(CliError::data(
                "assignment-start-conflict",
                "persisted assignment cannot resume worker start",
                Some(json!({ "assignment_id": assignment_id, "revision": current.revision })),
            ));
        }
    } else {
        if locked.registry.assignments.contains_key(&assignment_id) {
            return Err(CliError::data(
                "assignment-exists",
                "orchestration assignment already exists",
                Some(json!({ "assignment_id": assignment_id })),
            ));
        }
        // T5: advisory dependency ordering. Refuse to launch a dependent until
        // every declared dependency in this run has been accepted. Evaluated
        // against live state under the lock; a missing, cross-run, or
        // pre-terminal dependency blocks the launch with a typed result the
        // caller can wait on (`worker wait --until terminal`) and retry.
        let blocked_on = unsatisfied_dependencies(&locked.registry, &run.run_id, &input.depends_on);
        if !blocked_on.is_empty() {
            return Err(CliError::data(
                "dependency-not-satisfied",
                "assignment dependencies have not been accepted",
                Some(json!({ "assignment_id": assignment_id, "blocked_on": blocked_on })),
            ));
        }
        let packet_digest = orchestration::store_packet(context, &packet_value)?;
        let now = timestamp();
        let assignment = AssignmentRecord {
            schema_version: ASSIGNMENT_SCHEMA.to_string(),
            assignment_id: assignment_id.clone(),
            run_id: run.run_id.clone(),
            revision: 1,
            state: "starting".to_string(),
            task_summary: input.task_summary.clone(),
            private_packet_digest: packet_digest,
            primary_manager: run.controller.clone(),
            worker: None,
            previous_worker: None,
            collaborators: Vec::new(),
            borrowed_by: Vec::new(),
            repository: input.repository.clone(),
            worktree: input.worktree.clone(),
            base_ref: input.base_ref.clone(),
            scopes: input.scopes.clone(),
            durable_refs: input.durable_refs.clone(),
            depends_on: input.depends_on.clone(),
            checkpoint: None,
            result_summary: None,
            blocker_summary: None,
            submit_recovery: None,
            worker_quarantine: None,
            account_handoff: None,
            created_at: now.clone(),
            updated_at: now,
        };
        locked
            .registry
            .assignments
            .insert(assignment_id.clone(), assignment);
        let pending = json!({
            "schema_version": "main-agent.worker-start-result.v1",
            "assignment_id": assignment_id,
            "worker_session_id": worker_session_id,
            "state": "starting",
            "acceptance": "pending"
        });
        store_receipt(
            &mut locked.registry,
            &record,
            &incarnation,
            &args.idempotency_key,
            "worker-start",
            &request_digest,
            pending,
        )?;
        locked.save()?;
    }
    drop(locked);

    let agent = AgentKind::from_name(&input.launch.agent)
        .ok_or_else(|| invalid_input("assignment launch agent is invalid"))?;
    let main_agent_bin = env::current_exe().map_err(|_| {
        CliError::runtime(
            "main-agent-executable-unavailable",
            "the current main-agent executable could not be resolved for worker bootstrap",
            None,
        )
    })?;
    let prompt = worker_start_prompt(&assignment_id, &main_agent_bin);
    if let Some(batch_lane) = batch_lane {
        renew_worker_start_batch_lane(context, batch_lane)?;
    }
    let existing = match load_session_record(context, &worker_session_id) {
        Ok(worker) if runtime_is_proven_never_launched(&worker) => {
            ensure_worker_launch_matches(context, &worker, &input, &prompt)?;
            delete_session(context, &worker_session_id, resolve_tmux_bin(None))?;
            None
        }
        Ok(worker) => Some(worker),
        Err(error) if error.code() == "session-not-found" => None,
        Err(error) => return Err(error),
    };
    let fresh_launch = existing.is_none();
    let (worker_record, worker_status) = if let Some(worker) = existing {
        ensure_worker_launch_matches(context, &worker, &input, &prompt)?;
        let status = session_status(&resolve_tmux_bin(None), &worker);
        (worker, status)
    } else {
        let mut create_guard = || {
            pause_batch_lane_for_test("before_session_create")?;
            batch_lane.map_or(Ok(()), |batch_lane| {
                renew_worker_start_batch_lane(context, batch_lane)
            })
        };
        let started = crate::start_session_with_create_guard(
            context,
            cli::StartArgs {
                // A synchronous `main-agent` invocation does not retain the
                // daemon-owned Codex control handle. Keep this launch on the
                // bounded raw path until worker creation crosses a typed
                // `agent-session serve` launch boundary.
                app_server_managed: false,
                initial_codex_account: None,
                initial_title_state: None,
                initial_agent_profile: None,
                initial_provider_config_dir: None,
                initial_profile_auto_resume_supported: None,
                initial_codex_usage_account: None,
                agent,
                cwd: Some(PathBuf::from(&input.launch.cwd)),
                title: input.launch.title.clone(),
                id: Some(worker_session_id),
                prompt: Some(prompt),
                prompt_file: None,
                prompt_stdin: false,
                tmux_bin: None,
                agent_bin: None,
                agent_args: input.launch.agent_args.clone(),
                coordination_mode: input.launch.coordination_mode,
                paste_delay_ms: cli::DEFAULT_PASTE_DELAY_MS,
                format: OutputFormat::Json,
            },
            StartFailureDisposition::ReturnError,
            PromptDelivery::ManagedWorkerExactlyOnce,
            Some(&mut create_guard),
        )?;
        let worker = load_session_record(context, &started.result.id)?;
        (worker, started.result.status)
    };
    let worker_incarnation = worker_record
        .runtime
        .as_ref()
        .map(|runtime| runtime.launch_id.clone())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| invalid_input("worker session incarnation is unavailable"))?;

    let mut locked = orchestration::lock_registry(context)?;
    if let Some(batch_lane) = batch_lane {
        renew_worker_start_batch_lane_after_child_side_effect_locked(
            &mut locked.registry,
            batch_lane,
        )?;
    }
    let current = locked
        .registry
        .assignments
        .get_mut(&assignment_id)
        .ok_or_else(|| not_found("assignment-not-found", "assignment was not found"))?;
    if current.state != "starting" || current.revision != 1 || current.worker.is_some() {
        return Err(CliError::data(
            "assignment-start-conflict",
            "assignment changed while the worker was starting",
            Some(json!({ "assignment_id": assignment_id, "revision": current.revision })),
        ));
    }
    current.worker = Some(session_ref(context, &worker_record, &worker_incarnation));
    current.revision = 2;
    current.updated_at = timestamp();
    let outcome = json!({
        "schema_version": "main-agent.worker-start-result.v1",
        "assignment": public_assignment_view(current),
        "worker": {
            "session_id": worker_record.id,
            "session_incarnation": worker_incarnation,
            "status": worker_status
        },
        "acceptance": {
            "state": "pending-worker-checkpoint",
            "transport_only": true
        },
        "polling": worker_start_polling_evidence(await_ready)
    });
    let receipt_outcome = await_ready.map_or_else(
        || outcome.clone(),
        |timeout| {
            worker_start_readiness_progress(
                outcome.clone(),
                timeout,
                worker_submit_key_recovery_eligible(fresh_launch, agent),
            )
        },
    );
    store_receipt(
        &mut locked.registry,
        &record,
        &incarnation,
        &args.idempotency_key,
        "worker-start",
        &request_digest,
        receipt_outcome.clone(),
    )?;
    locked.save()?;
    // T1: fold the readiness proof. Drop the write lock first so the wait never
    // blocks the worker's own checkpoint. The worker's authenticated,
    // revision-fenced, incarnation-matched checkpoint advancing the assignment
    // past `starting` is the readiness + newer-turn + identity proof; a bounded
    // poll classifies it into a typed result. `--await-ready 0` stays launch-only.
    drop(locked);
    if await_ready.is_some() {
        let finalizer_id = receipt_outcome["finalizer_id"]
            .as_str()
            .ok_or_else(|| invalid_input("worker start readiness finalizer is invalid"))?
            .to_string();
        return finish_worker_start_readiness(
            context,
            &record,
            &incarnation,
            &args.idempotency_key,
            &request_digest,
            receipt_outcome,
            Some(finalizer_id),
        );
    }
    Ok(outcome)
}

fn worker_start_request_digests(
    input: &AssignmentInput,
    await_ready: &str,
) -> Result<(Value, String, String), CliError> {
    let packet_value =
        serde_json::to_value(input).map_err(|_| invalid_input("assignment packet is invalid"))?;
    let legacy_request_digest =
        crate::coordination::request_digest("main-agent-worker-start", input);
    let request_digest = crate::coordination::request_digest(
        "main-agent-worker-start-v2",
        &json!({
            "assignment": &packet_value,
            "await_ready": await_ready
        }),
    );
    Ok((packet_value, request_digest, legacy_request_digest))
}

fn worker_start_polling_evidence(timeout: Option<Duration>) -> Value {
    let Some(timeout) = timeout else {
        return json!({
            "mode": "launch_only",
            "readiness_registry_read_bound": 0,
            "readiness_registry_write_bound": 0
        });
    };
    let timeout_millis = timeout.as_millis() as u64;
    let poll_millis = WORKER_WAIT_POLL_INTERVAL.as_millis() as u64;
    let renewal_millis = WORKER_START_FINALIZER_RENEW_INTERVAL.as_millis() as u64;
    let readiness_registry_read_bound = timeout_millis.div_ceil(poll_millis).saturating_add(1);
    // One initial progress receipt, bounded lease renewals, one recovery
    // reservation/transition pair, and one final result receipt.
    let readiness_registry_write_bound = timeout_millis.div_ceil(renewal_millis).saturating_add(5);
    json!({
        "mode": "bounded_wait",
        "timeout_seconds": timeout.as_secs(),
        "readiness_registry_read_bound": readiness_registry_read_bound,
        "readiness_registry_write_bound": readiness_registry_write_bound
    })
}

fn worker_start_readiness_progress(
    outcome: Value,
    timeout: Duration,
    submit_key_recovery_eligible: bool,
) -> Value {
    let now = crate::coordination::now_epoch();
    let initial_lease_secs = worker_start_finalizer_lease_secs(&Value::Null);
    let deadline_at_epoch =
        now.saturating_add(i64::try_from(timeout.as_secs()).unwrap_or(i64::MAX));
    json!({
        "schema_version": "main-agent.worker-start-readiness-progress.v1",
        "state": "awaiting_readiness",
        "deadline_at_epoch": deadline_at_epoch,
        "finalizer_id": uuid::Uuid::new_v4().to_string(),
        "finalizer_lease_until_epoch": now.saturating_add(initial_lease_secs),
        "submit_key_recovery_eligible": submit_key_recovery_eligible,
        "recovery_continuation": Value::Null,
        "outcome": outcome
    })
}

fn worker_start_readiness_is_pending(value: &Value) -> bool {
    value["schema_version"] == "main-agent.worker-start-readiness-progress.v1"
        && value["state"] == "awaiting_readiness"
}

fn finish_worker_start_readiness(
    context: &CliContext,
    main: &SessionRecord,
    main_incarnation: &str,
    idempotency_key: &str,
    request_digest: &str,
    mut progress: Value,
    mut finalizer_id: Option<String>,
) -> Result<Value, CliError> {
    loop {
        if finalizer_id.is_none() {
            let (current, claimed_finalizer) = join_or_claim_worker_start_readiness(
                context,
                main,
                main_incarnation,
                idempotency_key,
                request_digest,
                progress,
            )?;
            let Some(claimed_finalizer) = claimed_finalizer else {
                return Ok(current);
            };
            progress = current;
            finalizer_id = Some(claimed_finalizer);
        }
        if !worker_start_readiness_is_pending(&progress) {
            return Ok(progress);
        }
        let deadline_at_epoch = progress["deadline_at_epoch"]
            .as_i64()
            .ok_or_else(|| invalid_input("worker start readiness deadline is invalid"))?;
        let timeout = Duration::from_secs(
            deadline_at_epoch
                .saturating_sub(crate::coordination::now_epoch())
                .try_into()
                .unwrap_or(0),
        );
        let submit_key_recovery_eligible = progress["submit_key_recovery_eligible"]
            .as_bool()
            .ok_or_else(|| invalid_input("worker start recovery eligibility is invalid"))?;
        let mut outcome = progress["outcome"].clone();
        let assignment_id = outcome["assignment"]["assignment_id"]
            .as_str()
            .ok_or_else(|| invalid_input("worker start assignment is unavailable"))?
            .to_string();
        let worker_session_id = outcome["worker"]["session_id"]
            .as_str()
            .ok_or_else(|| invalid_input("worker start session is unavailable"))?;
        let worker_incarnation = outcome["worker"]["session_incarnation"]
            .as_str()
            .ok_or_else(|| invalid_input("worker start incarnation is unavailable"))?
            .to_string();
        let worker_record = load_session_record(context, worker_session_id)?;
        let recovery_continuation = progress
            .get("recovery_continuation")
            .filter(|value| !value.is_null())
            .cloned();
        outcome["readiness"] = await_worker_readiness(
            context,
            main,
            main_incarnation,
            WorkerReadinessRequest {
                assignment_id: &assignment_id,
                timeout,
                worker: (&worker_record, &worker_incarnation),
                starting_revision: outcome["assignment"]["revision"]
                    .as_u64()
                    .ok_or_else(|| invalid_input("worker start revision is unavailable"))?,
                submit_key_recovery_eligible,
                readiness_receipt: WorkerStartReadinessReceipt {
                    idempotency_key,
                    request_digest,
                    finalizer_id: finalizer_id.as_deref().ok_or_else(|| {
                        invalid_input("worker start readiness finalizer is unavailable")
                    })?,
                },
                recovery_continuation,
            },
        )?;
        let mut locked = orchestration::lock_registry(context)?;
        let current = idempotency_replay(
            &locked.registry,
            main,
            main_incarnation,
            idempotency_key,
            "worker-start",
            request_digest,
        )?
        .ok_or_else(|| invalid_input("worker start readiness receipt is unavailable"))?;
        if !worker_start_readiness_is_pending(&current) {
            return Ok(current);
        }
        if current["finalizer_id"].as_str() != finalizer_id.as_deref() {
            drop(locked);
            progress = current;
            finalizer_id = None;
            continue;
        }
        store_receipt(
            &mut locked.registry,
            main,
            main_incarnation,
            idempotency_key,
            "worker-start",
            request_digest,
            outcome.clone(),
        )?;
        locked.save()?;
        return Ok(outcome);
    }
}

fn join_or_claim_worker_start_readiness(
    context: &CliContext,
    main: &SessionRecord,
    main_incarnation: &str,
    idempotency_key: &str,
    request_digest: &str,
    mut progress: Value,
) -> Result<(Value, Option<String>), CliError> {
    loop {
        if !worker_start_readiness_is_pending(&progress) {
            return Ok((progress, None));
        }
        let lease_until = progress["finalizer_lease_until_epoch"]
            .as_i64()
            .ok_or_else(|| invalid_input("worker start readiness lease is invalid"))?;
        if crate::coordination::now_epoch() < lease_until {
            thread::sleep(WORKER_WAIT_POLL_INTERVAL);
            let registry = orchestration::load_registry_readonly(context)?;
            progress = idempotency_replay(
                &registry,
                main,
                main_incarnation,
                idempotency_key,
                "worker-start",
                request_digest,
            )?
            .ok_or_else(|| invalid_input("worker start readiness receipt is unavailable"))?;
            continue;
        }
        let mut locked = orchestration::lock_registry(context)?;
        let mut current = idempotency_replay(
            &locked.registry,
            main,
            main_incarnation,
            idempotency_key,
            "worker-start",
            request_digest,
        )?
        .ok_or_else(|| invalid_input("worker start readiness receipt is unavailable"))?;
        if !worker_start_readiness_is_pending(&current) {
            return Ok((current, None));
        }
        let now = crate::coordination::now_epoch();
        if current["finalizer_lease_until_epoch"]
            .as_i64()
            .is_some_and(|lease| now < lease)
        {
            drop(locked);
            progress = current;
            continue;
        }
        let finalizer_id = uuid::Uuid::new_v4().to_string();
        current["finalizer_id"] = json!(finalizer_id.clone());
        current["finalizer_lease_until_epoch"] =
            json!(now.saturating_add(worker_start_finalizer_lease_secs(&current)));
        store_receipt(
            &mut locked.registry,
            main,
            main_incarnation,
            idempotency_key,
            "worker-start",
            request_digest,
            current.clone(),
        )?;
        locked.save()?;
        drop(locked);
        pause_readiness_finalizer_takeover_for_test(&finalizer_id)?;
        return Ok((current, Some(finalizer_id)));
    }
}

struct BatchPacket {
    path: PathBuf,
    name: String,
    bytes: Vec<u8>,
    digest: String,
}

struct BatchLaneFence<'a> {
    main: &'a SessionRecord,
    main_incarnation: &'a str,
    idempotency_key: &'a str,
    request_digest: &'a str,
    index: usize,
    owner_id: &'a str,
}

/// Launch every `*.json` assignment packet in `dir` as one batch. The parent
/// idempotency receipt is committed before any lane starts and binds the
/// ordered names and raw packet digests. Exact replay resumes missing lanes;
/// any manifest drift conflicts before a new worker launch.
fn run_worker_start_batch(
    context: &CliContext,
    args: &WorkerStartArgs,
    dir: &std::path::Path,
) -> Result<Value, CliError> {
    validate_idempotency_key(&args.idempotency_key)?;
    let mut packets = Vec::new();
    for entry in fs::read_dir(dir).map_err(|_| invalid_input("batch directory is unavailable"))? {
        let path = entry
            .map_err(|_| invalid_input("batch directory entry is unavailable"))?
            .path();
        if path.is_file() && path.extension().and_then(|ext| ext.to_str()) == Some("json") {
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| invalid_input("batch packet name must be UTF-8"))?
                .to_string();
            let bytes = crate::coordination::read_bounded_bytes(
                &path,
                256 * 1024,
                "invalid-assignment-packet",
            )?;
            packets.push(BatchPacket {
                path,
                name,
                digest: crate::coordination::digest_bytes(&bytes),
                bytes,
            });
        }
    }
    packets.sort_by(|left, right| left.name.cmp(&right.name));
    if packets.is_empty() {
        return Err(invalid_input(
            "batch directory has no .json assignment packets",
        ));
    }
    if packets.len() > 64 {
        return Err(invalid_input("batch exceeds the 64-lane limit"));
    }
    let manifest = packets
        .iter()
        .map(|packet| {
            json!({
                "name": packet.name,
                "packet_digest": packet.digest
            })
        })
        .collect::<Vec<_>>();
    let request_digest =
        crate::coordination::request_digest("main-agent-worker-start-batch-v1", &manifest);
    let (main, main_incarnation) = authenticated_self(context)?;
    ensure_active_claim(context, &main)?;
    let mut progress = {
        let mut locked = orchestration::lock_registry(context)?;
        match idempotency_replay(
            &locked.registry,
            &main,
            &main_incarnation,
            &args.idempotency_key,
            "worker-start-batch",
            &request_digest,
        )? {
            Some(progress) => progress,
            None => {
                let progress = json!({
                    "schema_version": "main-agent.worker-start-batch-progress.v1",
                    "state": "in_progress",
                    "manifest": manifest,
                    "lanes": vec![Value::Null; packets.len()]
                });
                store_receipt(
                    &mut locked.registry,
                    &main,
                    &main_incarnation,
                    &args.idempotency_key,
                    "worker-start-batch",
                    &request_digest,
                    progress.clone(),
                )?;
                locked.save()?;
                progress
            }
        }
    };
    if progress["schema_version"] != "main-agent.worker-start-batch-progress.v1"
        || !progress["lanes"].is_array()
        || progress["lanes"].as_array().map(Vec::len) != Some(packets.len())
    {
        return Err(CliError::data(
            "idempotency-conflict",
            "batch start receipt is not resumable",
            None,
        ));
    }
    if progress["state"] == "completed" {
        return Ok(progress["result"].clone());
    }

    let claim_wait_deadline = Instant::now() + WORKER_START_BATCH_CLAIM_WAIT_MAX;
    for (index, packet) in packets.iter().enumerate() {
        loop {
            let (current, lane_owner) = claim_worker_start_batch_lane(
                context,
                &main,
                &main_incarnation,
                &args.idempotency_key,
                &request_digest,
                index,
                claim_wait_deadline,
            )?;
            progress = current;
            if progress["state"] == "completed" {
                return Ok(progress["result"].clone());
            }
            let Some(lane_owner) = lane_owner else {
                break;
            };
            let lane_key = batch_lane_idempotency_key(&args.idempotency_key, index);
            let lane_args = WorkerStartArgs {
                assignment_file: Some(packet.path.clone()),
                batch: None,
                if_run_revision: None,
                idempotency_key: lane_key.clone(),
                await_ready: "0".to_string(),
                format: OutputFormat::Json,
            };
            let assignment_file = packet.path.to_string_lossy().into_owned();
            let lane_fence = BatchLaneFence {
                main: &main,
                main_incarnation: &main_incarnation,
                idempotency_key: &args.idempotency_key,
                request_digest: &request_digest,
                index,
                owner_id: &lane_owner,
            };
            let parsed = serde_json::from_slice::<AssignmentInput>(&packet.bytes).map_err(|_| {
                CliError::data(
                    "invalid-assignment-packet",
                    "batch assignment packet is invalid",
                    None,
                )
            });
            let lane = match parsed {
                Err(error) => batch_lane_failure(&assignment_file, error),
                Ok(input) => {
                    let (_, child_request_digest, child_legacy_request_digest) =
                        worker_start_request_digests(&input, &lane_args.await_ready)?;
                    match run_worker_start_single_input(
                        context,
                        lane_args,
                        input,
                        Some(&lane_fence),
                    ) {
                        Ok(result) => batch_lane_success(&assignment_file, result),
                        Err(error) => {
                            let registry = orchestration::load_registry_readonly(context)?;
                            match worker_start_idempotency_replay(
                                &registry,
                                &main,
                                &main_incarnation,
                                &lane_key,
                                &child_request_digest,
                                &child_legacy_request_digest,
                            )? {
                                Some(value)
                                    if !worker_start_is_pending(&value)
                                        && !worker_start_readiness_is_pending(&value) =>
                                {
                                    batch_lane_success(&assignment_file, value)
                                }
                                Some(_) => {
                                    release_worker_start_batch_lane(context, &lane_fence)?;
                                    return Err(error);
                                }
                                None if batch_lane_error_is_resumable(&error) => {
                                    release_worker_start_batch_lane(context, &lane_fence)?;
                                    return Err(error);
                                }
                                None => batch_lane_failure(&assignment_file, error),
                            }
                        }
                    }
                }
            };
            progress = complete_worker_start_batch_lane(
                context,
                &main,
                &main_incarnation,
                &args.idempotency_key,
                &request_digest,
                index,
                &lane_owner,
                lane,
            )?;
            if !worker_start_batch_lane_is_claim(&progress["lanes"][index]) {
                break;
            }
        }
    }
    let outcome = json!({
        "schema_version": "main-agent.worker-start-batch.v1",
        "lanes": progress["lanes"]
    });
    let mut locked = orchestration::lock_registry(context)?;
    let mut current = idempotency_replay(
        &locked.registry,
        &main,
        &main_incarnation,
        &args.idempotency_key,
        "worker-start-batch",
        &request_digest,
    )?
    .ok_or_else(|| invalid_input("batch start receipt is unavailable"))?;
    if current["state"] != "completed" {
        current["state"] = json!("completed");
        current["result"] = outcome.clone();
        store_receipt(
            &mut locked.registry,
            &main,
            &main_incarnation,
            &args.idempotency_key,
            "worker-start-batch",
            &request_digest,
            current,
        )?;
        locked.save()?;
    }
    Ok(outcome)
}

fn claim_worker_start_batch_lane(
    context: &CliContext,
    main: &SessionRecord,
    main_incarnation: &str,
    idempotency_key: &str,
    request_digest: &str,
    index: usize,
    wait_deadline: Instant,
) -> Result<(Value, Option<String>), CliError> {
    loop {
        let mut locked = orchestration::lock_registry(context)?;
        let mut current = idempotency_replay(
            &locked.registry,
            main,
            main_incarnation,
            idempotency_key,
            "worker-start-batch",
            request_digest,
        )?
        .ok_or_else(|| invalid_input("batch start receipt is unavailable"))?;
        if current["state"] == "completed"
            || !worker_start_batch_lane_is_incomplete(&current["lanes"][index])
        {
            return Ok((current, None));
        }
        let now = crate::coordination::now_epoch();
        let lease_active = worker_start_batch_lane_is_claim(&current["lanes"][index])
            && current["lanes"][index]["lease_until_epoch"]
                .as_i64()
                .is_some_and(|lease| now < lease);
        if lease_active {
            if Instant::now() >= wait_deadline {
                return Err(CliError::runtime(
                    "worker-start-batch-lane-wait-timeout",
                    "worker start batch lane claim remained owned past the bounded wait",
                    Some(json!({ "lane_index": index })),
                ));
            }
            drop(locked);
            thread::sleep(WORKER_WAIT_POLL_INTERVAL);
            continue;
        }
        let owner_id = uuid::Uuid::new_v4().to_string();
        current["lanes"][index] = json!({
            "schema_version": "main-agent.worker-start-batch-lane-claim.v1",
            "state": "in_progress",
            "owner_id": owner_id,
            "lease_until_epoch": worker_start_batch_lane_lease_until(now)
        });
        store_receipt(
            &mut locked.registry,
            main,
            main_incarnation,
            idempotency_key,
            "worker-start-batch",
            request_digest,
            current.clone(),
        )?;
        locked.save()?;
        return Ok((current, Some(owner_id)));
    }
}

fn renew_worker_start_batch_lane(
    context: &CliContext,
    lane: &BatchLaneFence<'_>,
) -> Result<(), CliError> {
    let mut locked = orchestration::lock_registry(context)?;
    renew_worker_start_batch_lane_locked(&mut locked.registry, lane)?;
    locked.save()
}

fn renew_worker_start_batch_lane_locked(
    registry: &mut orchestration::Registry,
    lane: &BatchLaneFence<'_>,
) -> Result<(), CliError> {
    renew_worker_start_batch_lane_at_fence_locked(
        registry,
        lane,
        BatchLaneFencePoint::BeforeChildSideEffect,
    )
}

fn renew_worker_start_batch_lane_after_child_side_effect_locked(
    registry: &mut orchestration::Registry,
    lane: &BatchLaneFence<'_>,
) -> Result<(), CliError> {
    renew_worker_start_batch_lane_at_fence_locked(
        registry,
        lane,
        BatchLaneFencePoint::AfterChildSideEffect,
    )
}

#[derive(Clone, Copy)]
enum BatchLaneFencePoint {
    BeforeChildSideEffect,
    AfterChildSideEffect,
}

fn renew_worker_start_batch_lane_at_fence_locked(
    registry: &mut orchestration::Registry,
    lane: &BatchLaneFence<'_>,
    fence_point: BatchLaneFencePoint,
) -> Result<(), CliError> {
    let mut current = idempotency_replay(
        registry,
        lane.main,
        lane.main_incarnation,
        lane.idempotency_key,
        "worker-start-batch",
        lane.request_digest,
    )?
    .ok_or_else(|| invalid_input("batch start receipt is unavailable"))?;
    let now = crate::coordination::now_epoch();
    if !worker_start_batch_lane_fence_is_valid(
        &current["lanes"][lane.index],
        lane.owner_id,
        now,
        fence_point,
    ) {
        let message = match fence_point {
            BatchLaneFencePoint::BeforeChildSideEffect => {
                "worker start batch lane ownership changed before a child side effect"
            }
            BatchLaneFencePoint::AfterChildSideEffect => {
                "worker start batch lane ownership changed during a child side effect"
            }
        };
        return Err(CliError::data(
            "worker-start-batch-lane-owner-changed",
            message,
            Some(json!({ "lane_index": lane.index })),
        ));
    }
    current["lanes"][lane.index]["lease_until_epoch"] =
        json!(worker_start_batch_lane_lease_until(now));
    store_receipt(
        registry,
        lane.main,
        lane.main_incarnation,
        lane.idempotency_key,
        "worker-start-batch",
        lane.request_digest,
        current,
    )
}

fn worker_start_batch_lane_fence_is_valid(
    value: &Value,
    owner_id: &str,
    now: i64,
    fence_point: BatchLaneFencePoint,
) -> bool {
    let owner_matches =
        worker_start_batch_lane_is_claim(value) && value["owner_id"].as_str() == Some(owner_id);
    owner_matches
        && match fence_point {
            BatchLaneFencePoint::BeforeChildSideEffect => value["lease_until_epoch"]
                .as_i64()
                .is_some_and(|lease| now < lease),
            BatchLaneFencePoint::AfterChildSideEffect => true,
        }
}

fn release_worker_start_batch_lane(
    context: &CliContext,
    lane: &BatchLaneFence<'_>,
) -> Result<(), CliError> {
    let mut locked = orchestration::lock_registry(context)?;
    let mut current = idempotency_replay(
        &locked.registry,
        lane.main,
        lane.main_incarnation,
        lane.idempotency_key,
        "worker-start-batch",
        lane.request_digest,
    )?
    .ok_or_else(|| invalid_input("batch start receipt is unavailable"))?;
    if worker_start_batch_lane_is_claim(&current["lanes"][lane.index])
        && current["lanes"][lane.index]["owner_id"].as_str() == Some(lane.owner_id)
    {
        current["lanes"][lane.index] = Value::Null;
        store_receipt(
            &mut locked.registry,
            lane.main,
            lane.main_incarnation,
            lane.idempotency_key,
            "worker-start-batch",
            lane.request_digest,
            current,
        )?;
        locked.save()?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn complete_worker_start_batch_lane(
    context: &CliContext,
    main: &SessionRecord,
    main_incarnation: &str,
    idempotency_key: &str,
    request_digest: &str,
    index: usize,
    owner_id: &str,
    lane: Value,
) -> Result<Value, CliError> {
    let mut locked = orchestration::lock_registry(context)?;
    let mut current = idempotency_replay(
        &locked.registry,
        main,
        main_incarnation,
        idempotency_key,
        "worker-start-batch",
        request_digest,
    )?
    .ok_or_else(|| invalid_input("batch start receipt is unavailable"))?;
    if worker_start_batch_lane_is_claim(&current["lanes"][index])
        && current["lanes"][index]["owner_id"].as_str() == Some(owner_id)
    {
        current["lanes"][index] = lane;
        store_receipt(
            &mut locked.registry,
            main,
            main_incarnation,
            idempotency_key,
            "worker-start-batch",
            request_digest,
            current.clone(),
        )?;
        locked.save()?;
    }
    Ok(current)
}

fn worker_start_batch_lane_is_claim(value: &Value) -> bool {
    value["schema_version"] == "main-agent.worker-start-batch-lane-claim.v1"
        && value["state"] == "in_progress"
}

fn worker_start_batch_lane_is_incomplete(value: &Value) -> bool {
    value.is_null() || worker_start_batch_lane_is_claim(value)
}

fn worker_start_batch_lane_lease_secs() -> i64 {
    #[cfg(debug_assertions)]
    if let Ok(value) = env::var("NILS_AGENT_SESSION_TEST_BATCH_LANE_LEASE_SECS")
        && let Ok(value) = value.parse::<i64>()
        && (1..=WORKER_START_BATCH_LANE_LEASE_SECS).contains(&value)
    {
        return value;
    }
    WORKER_START_BATCH_LANE_LEASE_SECS
}

fn worker_start_batch_lane_lease_until(now: i64) -> i64 {
    // Epoch-second timestamps truncate the current subsecond. One rounding
    // second guarantees the configured duration instead of a lease that can
    // expire immediately after a renewal near the next second boundary.
    now.saturating_add(worker_start_batch_lane_lease_secs())
        .saturating_add(1)
}

fn pause_batch_lane_for_test(stage: &str) -> Result<(), CliError> {
    #[cfg(debug_assertions)]
    if env::var("NILS_AGENT_SESSION_TEST_BATCH_LANE_BARRIER_STAGE").as_deref() == Ok(stage)
        && let Some(directory) =
            env::var_os("NILS_AGENT_SESSION_TEST_BATCH_LANE_BARRIER_DIR").map(PathBuf::from)
    {
        fs::write(directory.join("ready"), stage).map_err(|_| {
            CliError::runtime(
                "worker-start-batch-test-barrier",
                "worker start batch test barrier is unavailable",
                None,
            )
        })?;
        let deadline = Instant::now() + Duration::from_secs(30);
        while !directory.join("release").is_file() {
            if Instant::now() >= deadline {
                return Err(CliError::runtime(
                    "worker-start-batch-test-barrier",
                    "worker start batch test barrier timed out",
                    None,
                ));
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
    let _ = stage;
    Ok(())
}

fn batch_lane_success(assignment_file: &str, result: Value) -> Value {
    json!({
        "assignment_file": assignment_file,
        "ok": true,
        "result": result,
    })
}

fn batch_lane_failure(assignment_file: &str, error: CliError) -> Value {
    let data = error.into_inner();
    json!({
        "assignment_file": assignment_file,
        "ok": false,
        "error": {
            "code": data.code,
            "message": data.message,
            "details": data.details,
        },
    })
}

fn batch_lane_error_is_resumable(error: &CliError) -> bool {
    is_store_retryable(error)
        || matches!(
            error.code(),
            "command-timeout"
                | "command-wait-failed"
                | "command-failed"
                | "worker-start-finalizer-changed"
                | "worker-start-batch-lane-owner-changed"
        )
}

fn worker_start_is_pending(value: &Value) -> bool {
    value["schema_version"] == "main-agent.worker-start-result.v1"
        && value["state"] == "starting"
        && value["acceptance"] == "pending"
}

fn worker_start_prompt(assignment_id: &str, main_agent_bin: &Path) -> String {
    let bootstrap_key = worker_bootstrap_idempotency_key(assignment_id);
    let main_agent_bin = main_agent_bin.to_string_lossy();
    let main_agent_bin = shell_words::quote(&main_agent_bin);
    format!(
        "You are a managed worker for assignment {assignment_id}. First run `{main_agent_bin} bootstrap --idempotency-key {bootstrap_key} --format json`. Use the returned private assignment packet as your task; do not mutate before bootstrap succeeds. After checkpointing the final result, release your work-context claim before reporting completion."
    )
}

fn worker_bootstrap_idempotency_key(assignment_id: &str) -> String {
    let digest = crate::coordination::request_digest(
        "main-agent-worker-bootstrap-idempotency",
        &assignment_id,
    );
    format!("bootstrap-{}", &digest[..32])
}

fn retry_stable_worker_session_id(assignment_id: &str, request_digest: &str) -> String {
    let candidate = format!("worker-{assignment_id}");
    if crate::validate_id(&candidate).is_ok() {
        candidate
    } else {
        format!("worker-{}", &request_digest[..32])
    }
}

fn ensure_worker_launch_matches(
    _context: &CliContext,
    worker: &SessionRecord,
    input: &AssignmentInput,
    expected_prompt: &str,
) -> Result<(), CliError> {
    let expected_cwd = fs::canonicalize(&input.launch.cwd)
        .map_err(|_| invalid_input("assignment launch cwd is unavailable"))?;
    let worker_cwd_matches = fs::canonicalize(&worker.cwd)
        .ok()
        .is_some_and(|cwd| cwd == expected_cwd);
    let prompt_matches = worker
        .prompt_file
        .as_deref()
        .and_then(|path| fs::read_to_string(path).ok())
        .is_some_and(|prompt| prompt == expected_prompt);
    if worker.agent != input.launch.agent
        || !worker_cwd_matches
        || worker.coordination_mode != input.launch.coordination_mode
        || !prompt_matches
    {
        return Err(CliError::data(
            "assignment-start-conflict",
            "stable worker session identity belongs to a different launch",
            Some(json!({ "session_id": worker.id })),
        ));
    }
    Ok(())
}

/// A dependency assignment counts as satisfied once its work has been accepted.
/// `released` (accepted then cleaned up) still counts as done; `cancelled`, any
/// pre-terminal state, and a missing (never-created or deleted-before-accept)
/// dependency do not.
fn dependency_state_satisfies(state: &str) -> bool {
    matches!(state, "accepted" | "released")
}

/// Dependency ids that do not yet satisfy a dependent launching in `run_id`,
/// each annotated with its observed `state` (or JSON null when the dependency
/// is missing or belongs to another run). An empty result clears the launch.
fn unsatisfied_dependencies(
    registry: &orchestration::Registry,
    run_id: &str,
    depends_on: &[String],
) -> Vec<Value> {
    depends_on
        .iter()
        .filter_map(|dependency| match registry.assignments.get(dependency) {
            Some(assignment)
                if assignment.run_id == run_id && dependency_state_satisfies(&assignment.state) =>
            {
                None
            }
            Some(assignment) if assignment.run_id == run_id => Some(json!({
                "assignment_id": dependency,
                "state": assignment.state,
            })),
            _ => Some(json!({ "assignment_id": dependency, "state": Value::Null })),
        })
        .collect()
}

fn run_worker_list(context: &CliContext) -> Result<Value, CliError> {
    let (record, incarnation) = authenticated_self(context)?;
    let registry = orchestration::load_registry_readonly(context)?;
    let run = require_current_main(&registry, &record, &incarnation)?;
    let workers = registry
        .assignments
        .values()
        .filter(|assignment| assignment.run_id == run.run_id)
        .map(public_assignment_view)
        .collect::<Vec<_>>();
    Ok(json!({ "schema_version": "main-agent.worker-list.v1", "workers": workers }))
}

fn run_worker_show(context: &CliContext, args: WorkerShowArgs) -> Result<Value, CliError> {
    let (record, incarnation) = authenticated_self(context)?;
    let registry = orchestration::load_registry_readonly(context)?;
    let run = require_current_main(&registry, &record, &incarnation)?;
    let assignment = registry
        .assignments
        .get(&args.assignment_id)
        .filter(|assignment| assignment.run_id == run.run_id)
        .ok_or_else(|| not_found("assignment-not-found", "assignment was not found"))?;
    Ok(json!({
        "schema_version": "main-agent.worker-show.v1",
        "assignment": private_assignment_view(context, assignment)?
    }))
}

/// Upper bound on a single `worker wait` long-poll, matching the mailbox
/// `message wait` bound so both surfaces cap a blocked call the same way.
const WORKER_WAIT_MAX_SECS: u64 = 60;
/// Worker bootstrap may include a high-reasoning provider turn before its
/// authenticated checkpoint, so it has a separate five-minute bound.
const WORKER_AWAIT_READY_MAX_SECS: u64 = 5 * 60;
/// Delay between registry reads while polling; mirrors `mailbox::wait`.
const WORKER_WAIT_POLL_INTERVAL: Duration = Duration::from_millis(250);
const WORKER_ACTIVITY_POLL_INTERVAL: Duration = Duration::from_millis(500);
const WORKER_START_FINALIZER_LEASE_SECS: i64 = 8;
const WORKER_START_FINALIZER_RENEW_INTERVAL: Duration = Duration::from_secs(5);
const WORKER_START_BATCH_LANE_LEASE_SECS: i64 = 12;
const WORKER_START_BATCH_CLAIM_WAIT_MAX: Duration = Duration::from_secs(30);
const WORKER_PROVIDER_STALE_SECS: i64 = 15 * 60;
const WORKER_CLAIM_RENEWAL_RISK_SECS: i64 = 5 * 60;
/// Leave enough heartbeat horizon for diagnosis to return and the worker's
/// next edit hook to evaluate the same sidecar before its 30-second hard TTL.
const WORKER_EDIT_AUTHORITY_HEARTBEAT_FRESH_SECS: i64 = 10;
const WORKTREE_STATUS_TIMEOUT: Duration = Duration::from_secs(2);
const WORKTREE_STATUS_MAX_OUTPUT_BYTES: usize = 256 * 1024;
const WORKTREE_FINGERPRINT_MAX_BYTES: usize = 512 * 1024;
const WORKTREE_FINGERPRINT_MAX_FILES: usize = 1024;
const WORKTREE_FINGERPRINT_MAX_REAPERS: usize = 8;
const WORKTREE_FINGERPRINT_REAP_GRACE: Duration = Duration::from_millis(10);
const WORKTREE_PROGRESS_SNAPSHOT_SCHEMA: &str = "main-agent.worktree-progress-snapshot.v1";
const WORKTREE_PROGRESS_SNAPSHOT_MAX_BYTES: u64 = 16 * 1024;
const RAW_RATE_LIMIT_DIAGNOSTIC_TIMEOUT: Duration = Duration::from_secs(3);
const RAW_RATE_LIMIT_DIAGNOSTIC_MAX_BYTES: usize = 64 * 1024;
const RAW_RATE_LIMIT_ACCOUNT_MAX_BYTES: usize = 128;
/// Maximum delay before one runtime-owned submit-key recovery. Shorter
/// `--await-ready` bounds recover halfway through so the same total deadline
/// still leaves time for the authenticated checkpoint.
const WORKER_SUBMIT_KEY_RECOVERY_DELAY: Duration = Duration::from_secs(5);

/// Completion-awareness for the orchestrating Main Agent: a bounded, read-only
/// long-poll that returns once the watched assignment(s) reach the `--until`
/// milestone, or reports `timeout` when the bound elapses. The operator console
/// gets sub-second SSE push; this is the equivalent for the CLI consumer.
///
/// It is level-triggered — an assignment already in the target state returns
/// immediately, so "launch N workers, then wait until any submitted" works even
/// if one finished before the wait began. It takes no registry lock and mutates
/// nothing (`load_registry_readonly` per poll), so it neither contends with
/// concurrent worker transitions nor resets its deadline through the facade's
/// transient-store auto-retry. Like `worker list`/`show`, it requires only the
/// authenticated live main controller — no work-context claim, revision fence,
/// or idempotency key. A `--until submitted` result reports a state transition
/// only; it is never itself acceptance evidence (spec §worker acceptance).
fn run_worker_wait(context: &CliContext, args: WorkerWaitArgs) -> Result<Value, CliError> {
    let target_id = match (&args.assignment_id, args.any) {
        (Some(_), true) => {
            return Err(CliError::usage(
                "worker-wait-target",
                "pass either an assignment id or --any, not both",
                None,
            ));
        }
        (None, false) => {
            return Err(CliError::usage(
                "worker-wait-target",
                "pass an assignment id or --any",
                None,
            ));
        }
        (Some(id), false) => Some(id.clone()),
        (None, true) => None,
    };
    let timeout = parse_wait_timeout(&args.timeout)?;
    let (record, incarnation) = authenticated_self(context)?;
    let started = Instant::now();
    loop {
        let registry = orchestration::load_registry_readonly(context)?;
        let run = require_current_main(&registry, &record, &incarnation)?;
        let matched = match target_id.as_deref() {
            Some(id) => {
                let assignment = registry
                    .assignments
                    .get(id)
                    .filter(|assignment| assignment.run_id == run.run_id)
                    .ok_or_else(|| not_found("assignment-not-found", "assignment was not found"))?;
                args.until.matches(&assignment.state).then_some(assignment)
            }
            None => registry
                .assignments
                .values()
                .filter(|assignment| assignment.run_id == run.run_id)
                .find(|assignment| args.until.matches(&assignment.state)),
        };
        if let Some(assignment) = matched {
            return Ok(json!({
                "schema_version": "main-agent.worker-wait.v1",
                "outcome": "transitioned",
                "until": args.until.as_label(),
                "assignment": public_assignment_view(assignment)
            }));
        }
        if started.elapsed() >= timeout {
            return Ok(json!({
                "schema_version": "main-agent.worker-wait.v1",
                "outcome": "timeout",
                "until": args.until.as_label(),
                "timeout": args.timeout
            }));
        }
        thread::sleep(WORKER_WAIT_POLL_INTERVAL);
    }
}

/// Parse and bound a `worker wait` timeout (1-60s; integer with optional
/// s/m/h/d suffix). Mirrors `mailbox::parse_wait` so the two long-poll surfaces
/// accept identical duration syntax and bounds.
fn parse_duration_seconds(value: &str) -> Result<u64, CliError> {
    let (number, multiplier) = if let Some(number) = value.strip_suffix('s') {
        (number, 1)
    } else if let Some(number) = value.strip_suffix('m') {
        (number, 60)
    } else if let Some(number) = value.strip_suffix('h') {
        (number, 60 * 60)
    } else if let Some(number) = value.strip_suffix('d') {
        (number, 24 * 60 * 60)
    } else {
        (value, 1)
    };
    let seconds = number
        .parse::<u64>()
        .ok()
        .and_then(|seconds| seconds.checked_mul(multiplier))
        .ok_or_else(|| {
            CliError::usage(
                "invalid-duration",
                "duration must be an integer with optional s, m, h, or d suffix",
                None,
            )
        })?;
    Ok(seconds)
}

fn parse_wait_timeout(value: &str) -> Result<Duration, CliError> {
    let seconds = parse_duration_seconds(value)?;
    if seconds == 0 || seconds > WORKER_WAIT_MAX_SECS {
        return Err(CliError::usage(
            "worker-wait-timeout",
            "worker wait must be between 1 and 60 seconds",
            None,
        ));
    }
    Ok(Duration::from_secs(seconds))
}

/// Parse the `worker start --await-ready` bound. `0` (with an optional s/m/h
/// suffix) means launch-only — no readiness wait; any other value is bounded
/// to five minutes.
fn parse_await_ready(value: &str) -> Result<Option<Duration>, CliError> {
    let trimmed = value.trim();
    if matches!(trimmed, "0" | "0s" | "0m" | "0h") {
        return Ok(None);
    }
    let seconds = parse_duration_seconds(trimmed)?;
    if seconds == 0 || seconds > WORKER_AWAIT_READY_MAX_SECS {
        return Err(CliError::usage(
            "worker-await-ready-timeout",
            "worker start --await-ready must be between 1 second and 5 minutes",
            None,
        ));
    }
    Ok(Some(Duration::from_secs(seconds)))
}

/// Classify an assignment state as worker readiness. `starting` means the worker
/// has not yet run its authenticated self-check + checkpoint; any advanced state
/// proves readiness because that checkpoint is revision-fenced and
/// incarnation-matched by the worker-checkpoint path.
fn readiness_from_state(state: &str) -> &'static str {
    if state == "starting" {
        "readiness_failed"
    } else {
        "ready"
    }
}

fn assignment_has_preclaim_blocker(assignment: &AssignmentRecord) -> bool {
    assignment
        .blocker_summary
        .as_deref()
        .is_some_and(|summary| summary.starts_with("[pre-claim:"))
}

fn worker_readiness_checkpoint(
    assignment: &AssignmentRecord,
    main: &SessionRecord,
    main_incarnation: &str,
    worker: &SessionRef,
    starting_revision: u64,
) -> bool {
    assignment.primary_manager.session_id == main.id
        && assignment.primary_manager.session_incarnation == main_incarnation
        && assignment.worker.as_ref() == Some(worker)
        && !assignment_has_preclaim_blocker(assignment)
        && matches!(
            assignment.state.as_str(),
            "working" | "blocked" | "submitted"
        )
        && assignment.checkpoint.as_ref().is_some_and(|checkpoint| {
            checkpoint.revision > starting_revision && checkpoint.revision == assignment.revision
        })
}

fn worker_submit_key_recovery_eligible(fresh_launch: bool, agent: AgentKind) -> bool {
    fresh_launch && matches!(agent, AgentKind::Codex | AgentKind::Claude)
}

struct WorkerReadinessRequest<'a> {
    assignment_id: &'a str,
    timeout: Duration,
    worker: (&'a SessionRecord, &'a str),
    starting_revision: u64,
    submit_key_recovery_eligible: bool,
    readiness_receipt: WorkerStartReadinessReceipt<'a>,
    recovery_continuation: Option<Value>,
}

struct WorkerStartReadinessReceipt<'a> {
    idempotency_key: &'a str,
    request_digest: &'a str,
    finalizer_id: &'a str,
}

fn worker_start_recovery_continuation(
    reservation: &SubmitRecoveryReservation,
    stage: &str,
) -> Value {
    json!({
        "schema_version": "main-agent.worker-start-recovery-continuation.v1",
        "stage": stage,
        "reservation": submit_recovery_progress(reservation)["reservation"]
    })
}

fn worker_start_recovery_from_continuation(
    value: &Value,
) -> Result<(SubmitRecoveryReservation, String), CliError> {
    if value["schema_version"] != "main-agent.worker-start-recovery-continuation.v1" {
        return Err(CliError::data(
            "idempotency-conflict",
            "worker start recovery continuation is invalid",
            None,
        ));
    }
    let stage = value["stage"]
        .as_str()
        .filter(|stage| {
            matches!(
                *stage,
                "reserved" | "sending" | "sent" | "failed" | "outcome_unknown"
            )
        })
        .ok_or_else(|| invalid_input("worker start recovery stage is invalid"))?
        .to_string();
    let progress = json!({
        "schema_version": "main-agent.worker-submit-recovery-progress.v1",
        "state": "in_progress",
        "reservation": value["reservation"]
    });
    Ok((submit_recovery_reservation_from_progress(&progress)?, stage))
}

fn persist_worker_start_readiness_progress(
    context: &CliContext,
    main: &SessionRecord,
    main_incarnation: &str,
    receipt: &WorkerStartReadinessReceipt<'_>,
    continuation: Option<Value>,
) -> Result<(), CliError> {
    let mut locked = orchestration::lock_registry(context)?;
    let mut current = idempotency_replay(
        &locked.registry,
        main,
        main_incarnation,
        receipt.idempotency_key,
        "worker-start",
        receipt.request_digest,
    )?
    .ok_or_else(|| invalid_input("worker start readiness receipt is unavailable"))?;
    if !worker_start_readiness_is_pending(&current)
        || current["finalizer_id"].as_str() != Some(receipt.finalizer_id)
    {
        return Err(CliError::data(
            "worker-start-finalizer-changed",
            "worker start readiness finalizer lease changed",
            None,
        ));
    }
    if let Some(continuation) = continuation {
        current["recovery_continuation"] = continuation;
    }
    let lease_secs = worker_start_finalizer_lease_secs(&current);
    current["finalizer_lease_until_epoch"] =
        json!(crate::coordination::now_epoch().saturating_add(lease_secs));
    store_receipt(
        &mut locked.registry,
        main,
        main_incarnation,
        receipt.idempotency_key,
        "worker-start",
        receipt.request_digest,
        current,
    )?;
    locked.save()
}

fn worker_start_finalizer_lease_secs(progress: &Value) -> i64 {
    #[cfg(debug_assertions)]
    if let Ok(value) = env::var("NILS_AGENT_SESSION_TEST_READINESS_FINALIZER_LEASE_SECS")
        && let Ok(value) = value.parse::<i64>()
        && (1..=WORKER_START_FINALIZER_LEASE_SECS).contains(&value)
    {
        return value;
    }
    if progress["recovery_continuation"]["stage"].as_str() == Some("sending") {
        i64::try_from(crate::PANE_INPUT_COMMAND_TIMEOUT.as_secs())
            .unwrap_or(i64::MAX)
            .saturating_add(2)
    } else {
        WORKER_START_FINALIZER_LEASE_SECS
    }
}

fn pause_readiness_recovery_for_test(stage: &str) -> Result<(), CliError> {
    #[cfg(debug_assertions)]
    if env::var("NILS_AGENT_SESSION_TEST_READINESS_RECOVERY_BARRIER_STAGE").as_deref() == Ok(stage)
        && let Some(directory) =
            env::var_os("NILS_AGENT_SESSION_TEST_READINESS_RECOVERY_BARRIER_DIR").map(PathBuf::from)
    {
        fs::write(directory.join("ready"), stage).map_err(|_| {
            CliError::runtime(
                "readiness-recovery-test-barrier",
                "readiness recovery test barrier is unavailable",
                None,
            )
        })?;
        let deadline = Instant::now() + Duration::from_secs(30);
        while !directory.join("release").is_file() {
            if Instant::now() >= deadline {
                return Err(CliError::runtime(
                    "readiness-recovery-test-barrier",
                    "readiness recovery test barrier timed out",
                    None,
                ));
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
    let _ = stage;
    Ok(())
}

fn pause_readiness_finalizer_takeover_for_test(finalizer_id: &str) -> Result<(), CliError> {
    #[cfg(debug_assertions)]
    if let Some(directory) =
        env::var_os("NILS_AGENT_SESSION_TEST_READINESS_TAKEOVER_BARRIER_DIR").map(PathBuf::from)
    {
        fs::write(directory.join("ready"), finalizer_id).map_err(|_| {
            CliError::runtime(
                "readiness-takeover-test-barrier",
                "readiness finalizer takeover test barrier is unavailable",
                None,
            )
        })?;
        let deadline = Instant::now() + Duration::from_secs(30);
        while !directory.join("release").is_file() {
            if Instant::now() >= deadline {
                return Err(CliError::runtime(
                    "readiness-takeover-test-barrier",
                    "readiness finalizer takeover test barrier timed out",
                    None,
                ));
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
    let _ = finalizer_id;
    Ok(())
}

/// Bounded, read-only wait for a freshly launched worker to advance its
/// assignment past `starting`, returning the typed readiness projection. Mirrors
/// the `worker wait` poll (no lock, level-triggered) so it never blocks the
/// worker's own checkpoint.
fn await_worker_readiness(
    context: &CliContext,
    record: &SessionRecord,
    incarnation: &str,
    request: WorkerReadinessRequest<'_>,
) -> Result<Value, CliError> {
    let WorkerReadinessRequest {
        assignment_id,
        timeout,
        worker,
        starting_revision,
        submit_key_recovery_eligible,
        readiness_receipt,
        recovery_continuation,
    } = request;
    let started = Instant::now();
    let recovery_after = std::cmp::min(WORKER_SUBMIT_KEY_RECOVERY_DELAY, timeout / 2);
    let recovery_eligible = submit_key_recovery_eligible;
    let (mut recovery, mut recovery_stage) = match recovery_continuation.as_ref() {
        Some(value) => {
            let (reservation, stage) = worker_start_recovery_from_continuation(value)?;
            (Some(reservation), Some(stage))
        }
        None => (None, None),
    };
    let mut resumed_ambiguous_send = recovery_stage.as_deref() == Some("sending");
    let mut recovery_transport_state = match recovery_stage.as_deref() {
        Some("sent") => "submit-key-recovery-succeeded",
        Some("sending" | "outcome_unknown") => "submit-command-outcome-unknown",
        Some("failed") => "submit-key-recovery-failed",
        _ => "submit-command-succeeded",
    };
    let mut last_finalizer_renewal = Instant::now();
    let mut last_activity_check = started
        .checked_sub(WORKER_ACTIVITY_POLL_INTERVAL)
        .unwrap_or(started);
    loop {
        if last_finalizer_renewal.elapsed() >= WORKER_START_FINALIZER_RENEW_INTERVAL {
            persist_worker_start_readiness_progress(
                context,
                record,
                incarnation,
                &readiness_receipt,
                None,
            )?;
            last_finalizer_renewal = Instant::now();
        }
        let registry = orchestration::load_registry_readonly(context)?;
        let run = require_current_main(&registry, record, incarnation)?;
        let assignment = registry
            .assignments
            .get(assignment_id)
            .filter(|assignment| assignment.run_id == run.run_id)
            .ok_or_else(|| not_found("assignment-not-found", "assignment was not found"))?
            .clone();
        let state = assignment.state.clone();
        if matches!(
            recovery_stage.as_deref(),
            Some("failed" | "outcome_unknown")
        ) {
            let result = assignment
                .submit_recovery
                .as_ref()
                .filter(|current| {
                    recovery
                        .as_ref()
                        .is_some_and(|reservation| current.attempt_id == reservation.attempt_id)
                })
                .map(|current| current.result.as_str())
                .unwrap_or("submit-recovery-send-outcome-unknown");
            return Ok(json!({
                "state": "readiness_failed",
                "assignment_state": state,
                "worker_launched": true,
                "delivery": {
                    "state": "unverified",
                    "transport_state": recovery_transport_state,
                    "proof": result
                },
                "submit_key_recovery": {
                    "eligible": true,
                    "attempted": true,
                    "attempt_count": 1,
                    "result": result
                },
                "automatic_retry_safe": false,
                "safe_state": "worker remains bound and the persisted recovery continuation forbids another Enter"
            }));
        }
        if resumed_ambiguous_send {
            resumed_ambiguous_send = false;
            let recovery_record = assignment.submit_recovery.as_ref().filter(|current| {
                recovery.as_ref().is_some_and(|reservation| {
                    current.attempt_id == reservation.attempt_id
                        && current.reserved_revision == reservation.reserved_revision
                        && current.session_incarnation == reservation.worker.session_incarnation
                })
            });
            match recovery_record.map(|current| current.state.as_str()) {
                Some("sent" | "checkpoint_confirmed") => {
                    recovery_stage = Some("sent".to_string());
                    recovery_transport_state = "submit-key-recovery-succeeded";
                    persist_worker_start_readiness_progress(
                        context,
                        record,
                        incarnation,
                        &readiness_receipt,
                        recovery.as_ref().map(|reservation| {
                            worker_start_recovery_continuation(reservation, "sent")
                        }),
                    )?;
                }
                Some("failed" | "reconciled") => {
                    let result = recovery_record
                        .map(|current| current.result.as_str())
                        .unwrap_or("submit-recovery-terminal");
                    return Ok(json!({
                        "state": "readiness_failed",
                        "assignment_state": state,
                        "worker_launched": true,
                        "delivery": {
                            "state": "unverified",
                            "transport_state": "submit-key-recovery-failed",
                            "proof": result
                        },
                        "submit_key_recovery": {
                            "eligible": true,
                            "attempted": true,
                            "attempt_count": 1,
                            "result": result
                        },
                        "automatic_retry_safe": false,
                        "safe_state": "the persisted recovery attempt is terminal and no further input is authorized"
                    }));
                }
                _ => {
                    return Ok(json!({
                        "state": "readiness_failed",
                        "assignment_state": state,
                        "worker_launched": true,
                        "delivery": {
                            "state": "unverified",
                            "transport_state": "submit-command-outcome-unknown",
                            "proof": "submit-recovery-send-outcome-unknown"
                        },
                        "submit_key_recovery": {
                            "eligible": true,
                            "attempted": true,
                            "attempt_count": 1,
                            "result": "submit-recovery-send-outcome-unknown"
                        },
                        "automatic_retry_safe": false,
                        "safe_state": "worker remains bound and the predecessor finalizer may have sent Enter; preserve the recovery fence and never resend"
                    }));
                }
            }
        }
        if state != "starting" {
            let checkpoint_confirmed = if let Some(reservation) = recovery.as_ref() {
                matches!(
                    submit_recovery_checkpoint(&assignment, record, incarnation, reservation),
                    SubmitRecoveryCheckpoint::Confirmed
                )
            } else {
                assignment.worker.as_ref().is_some_and(|bound| {
                    worker_readiness_checkpoint(
                        &assignment,
                        record,
                        incarnation,
                        bound,
                        starting_revision,
                    )
                })
            };
            if !checkpoint_confirmed {
                let result = if let Some(reservation) = recovery.as_ref() {
                    match submit_recovery_checkpoint(&assignment, record, incarnation, reservation)
                    {
                        SubmitRecoveryCheckpoint::Rejected(code) => code,
                        _ => "worker-checkpoint-proof-unavailable",
                    }
                } else {
                    "worker-checkpoint-proof-unavailable"
                };
                if result == "worker-bootstrap-preclaim-failed"
                    && let Some(reservation) = recovery.as_ref()
                {
                    let _ = update_submit_recovery(
                        context,
                        record,
                        incarnation,
                        reservation,
                        "failed",
                        result,
                    )?;
                }
                return Ok(json!({
                    "state": "readiness_failed",
                    "classification": "checkpoint_proof_failed",
                    "assignment_state": state,
                    "worker_launched": true,
                    "delivery": {
                        "state": "unverified",
                        "transport_state": recovery_transport_state,
                        "proof": result
                    },
                    "submit_key_recovery": {
                        "eligible": recovery_eligible,
                        "attempted": recovery.is_some(),
                        "attempt_count": usize::from(recovery.is_some()),
                        "result": result
                    },
                    "automatic_retry_safe": false,
                    "safe_state": "assignment changed without a newer incarnation-matched worker checkpoint; preserve the worker and diagnose the conflicting transition"
                }));
            }
            if let Some(reservation) = recovery.as_ref() {
                let _ = update_submit_recovery(
                    context,
                    record,
                    incarnation,
                    reservation,
                    "checkpoint_confirmed",
                    "authenticated worker checkpoint confirmed",
                )?;
            }
            return Ok(json!({
                "state": readiness_from_state(&state),
                "assignment_state": state,
                "worker_launched": true,
                "delivery": {
                    "state": "confirmed",
                    "transport_state": recovery_transport_state,
                    "proof": "authenticated-worker-checkpoint"
                },
                "submit_key_recovery": {
                    "eligible": recovery_eligible,
                    "attempted": recovery.is_some(),
                    "attempt_count": usize::from(recovery.is_some()),
                    "result": if recovery.is_some() {
                        "checkpoint-confirmed"
                    } else {
                        "not-needed"
                    }
                }
            }));
        }
        let should_check_activity = last_activity_check.elapsed() >= WORKER_ACTIVITY_POLL_INTERVAL;
        if should_check_activity {
            last_activity_check = Instant::now();
        }
        if should_check_activity
            && authoritative_worker_turn_terminated(context, worker.0, worker.1)
        {
            if let Some(reservation) = recovery.as_ref() {
                let _ = update_submit_recovery(
                    context,
                    record,
                    incarnation,
                    reservation,
                    "failed",
                    "provider-turn-terminated-without-checkpoint",
                )?;
            }
            return Ok(json!({
                "state": "readiness_failed",
                "classification": "submitted_or_waiting_without_checkpoint",
                "assignment_state": state,
                "worker_launched": true,
                "delivery": {
                    "state": "unverified",
                    "transport_state": recovery_transport_state,
                    "proof": "authoritative-provider-turn-terminated"
                },
                "submit_key_recovery": {
                    "eligible": recovery_eligible,
                    "attempted": recovery.is_some(),
                    "attempt_count": usize::from(recovery.is_some()),
                    "result": if recovery.is_some() {
                        "provider-turn-terminated"
                    } else {
                        "not-attempted-provider-terminated"
                    }
                },
                "automatic_retry_safe": false,
                "safe_state": "worker remains bound without an authenticated checkpoint; the authoritative provider turn has terminated. Do not resend the prompt or inject Enter. Diagnose, then cancel or safely reassign only if claim and operation evidence permit it."
            }));
        }
        let elapsed = started.elapsed();
        if recovery.is_none()
            && elapsed >= recovery_after
            && elapsed < timeout
            && submit_key_recovery_eligible
        {
            pause_readiness_recovery_for_test("before_reserve")?;
            let expected_worker = assignment.worker.as_ref().ok_or_else(|| {
                not_found("worker-not-started", "worker session is not available")
            })?;
            let reservation = match reserve_submit_recovery(
                context,
                record,
                incarnation,
                assignment_id,
                Some(assignment.revision),
                Some(expected_worker),
                None,
                Some(&readiness_receipt),
            ) {
                Ok(reservation) => reservation,
                Err(error) => {
                    let result = error.code().to_string();
                    return Ok(json!({
                        "state": "readiness_failed",
                        "assignment_state": state,
                        "worker_launched": true,
                        "delivery": {
                            "state": "unverified",
                            "transport_state": "submit-key-recovery-failed",
                            "proof": result
                        },
                        "submit_key_recovery": {
                            "eligible": true,
                            "attempted": false,
                            "attempt_count": 0,
                            "result": result
                        },
                        "automatic_retry_safe": false,
                        "safe_state": "worker remains bound; durable submit recovery reservation was refused, so no input was sent"
                    }));
                }
            };
            recovery = Some(reservation.clone());
            recovery_stage = Some("reserved".to_string());
            pause_readiness_recovery_for_test("reserved")?;
            continue;
        }
        if recovery_stage.as_deref() == Some("reserved") && elapsed < timeout {
            let reservation = recovery
                .as_ref()
                .ok_or_else(|| invalid_input("worker start recovery reservation is missing"))?
                .clone();
            persist_worker_start_readiness_progress(
                context,
                record,
                incarnation,
                &readiness_receipt,
                Some(worker_start_recovery_continuation(&reservation, "sending")),
            )?;
            pause_readiness_recovery_for_test("sending")?;
            match send_reserved_submit_recovery(
                context,
                &reservation,
                Some((record, incarnation, &readiness_receipt)),
            ) {
                Ok(()) => {
                    recovery_transport_state = "submit-key-recovery-succeeded";
                    let _ = update_submit_recovery(
                        context,
                        record,
                        incarnation,
                        &reservation,
                        "sent",
                        "single guarded Enter sent",
                    )?;
                    recovery_stage = Some("sent".to_string());
                    persist_worker_start_readiness_progress(
                        context,
                        record,
                        incarnation,
                        &readiness_receipt,
                        Some(worker_start_recovery_continuation(&reservation, "sent")),
                    )?;
                    pause_readiness_recovery_for_test("sent")?;
                }
                Err(code) => {
                    if code == "worker-start-finalizer-changed" {
                        return Err(CliError::data(
                            "worker-start-finalizer-changed",
                            "worker start readiness finalizer lease changed before recovery Enter",
                            None,
                        ));
                    }
                    let outcome_unknown = submit_recovery_send_outcome_is_unknown(&code);
                    let result = if outcome_unknown {
                        persist_worker_start_readiness_progress(
                            context,
                            record,
                            incarnation,
                            &readiness_receipt,
                            Some(worker_start_recovery_continuation(
                                &reservation,
                                "outcome_unknown",
                            )),
                        )?;
                        "submit-recovery-send-outcome-unknown".to_string()
                    } else {
                        let result = update_submit_recovery(
                            context,
                            record,
                            incarnation,
                            &reservation,
                            "failed",
                            &code,
                        )?
                        .1;
                        persist_worker_start_readiness_progress(
                            context,
                            record,
                            incarnation,
                            &readiness_receipt,
                            Some(worker_start_recovery_continuation(&reservation, "failed")),
                        )?;
                        result
                    };
                    return Ok(json!({
                        "state": "readiness_failed",
                        "assignment_state": state,
                        "worker_launched": true,
                        "delivery": {
                            "state": "unverified",
                            "transport_state": "submit-key-recovery-failed",
                            "proof": result
                        },
                        "submit_key_recovery": {
                            "eligible": true,
                            "attempted": true,
                            "attempt_count": 1,
                            "result": result
                        },
                        "automatic_retry_safe": false,
                        "safe_state": if outcome_unknown {
                            "worker remains bound in `starting`; the tmux send outcome is unknown, so the recovery record and all manager-mutation fences remain active. Never resend Enter; wait for the original sender or a newer worker checkpoint."
                        } else {
                            "worker remains bound in `starting`; runtime-owned single-Enter recovery failed before delivery and no further input is allowed. Keep the worker available for typed session diagnostics."
                        }
                    }));
                }
            }
            continue;
        }
        if elapsed >= timeout {
            if let Some(reservation) = recovery.as_ref() {
                let _ = update_submit_recovery(
                    context,
                    record,
                    incarnation,
                    reservation,
                    "failed",
                    "checkpoint-timeout",
                )?;
            }
            return Ok(json!({
                "state": "readiness_failed",
                "assignment_state": state,
                "worker_launched": true,
                "delivery": {
                    "state": "unverified",
                    "transport_state": recovery_transport_state,
                    "proof": "worker-checkpoint-timeout"
                },
                "submit_key_recovery": {
                    "eligible": recovery_eligible,
                    "attempted": recovery.is_some(),
                    "attempt_count": usize::from(recovery.is_some()),
                    "result": if recovery.is_some() {
                        "checkpoint-timeout"
                    } else {
                        "not-eligible"
                    }
                },
                "automatic_retry_safe": false,
                "safe_state": if recovery.is_some() {
                    "worker remains launched and bound in `starting`; runtime-owned single-Enter recovery is exhausted, so do not resend the prompt or inject another Enter. Keep the worker available for typed session diagnostics."
                } else {
                    "worker remains launched and bound in `starting`; submit-key recovery was not eligible, so do not resend the prompt or inject Enter. Keep the worker available for typed session diagnostics."
                }
            }));
        }
        thread::sleep(WORKER_WAIT_POLL_INTERVAL);
    }
}

fn authoritative_worker_turn_terminated(
    context: &CliContext,
    expected: &SessionRecord,
    expected_incarnation: &str,
) -> bool {
    if expected
        .runtime
        .as_ref()
        .map(|runtime| runtime.launch_id.as_str())
        != Some(expected_incarnation)
    {
        return false;
    }
    let Ok(activity) = crate::activity::activity_status_for_record(context, expected) else {
        return false;
    };
    activity.turn_state.phase == crate::activity::TurnPhase::Waiting
        && activity.turn_state.last_turn.is_some()
        && activity.turn_state.source.confidence == crate::activity::Confidence::Authoritative
}

#[derive(Clone)]
struct SubmitRecoveryReservation {
    assignment_id: String,
    attempt_id: String,
    reserved_revision: u64,
    run_id: String,
    controller: SessionRef,
    worker: SessionRef,
}

enum SubmitRecoveryCheckpoint {
    Pending,
    Confirmed,
    Rejected(&'static str),
}

/// Reserve the one incarnation-bound recovery attempt before any input side
/// effect. Automatic readiness recovery and the explicit primitive both call
/// this function, so a second path cannot obtain an independent allowance.
#[allow(clippy::too_many_arguments)]
fn reserve_submit_recovery(
    context: &CliContext,
    main: &SessionRecord,
    main_incarnation: &str,
    assignment_id: &str,
    expected_revision: Option<u64>,
    expected_worker: Option<&SessionRef>,
    receipt: Option<(&str, &str)>,
    readiness_receipt: Option<&WorkerStartReadinessReceipt<'_>>,
) -> Result<SubmitRecoveryReservation, CliError> {
    if receipt.is_some() && readiness_receipt.is_some() {
        return Err(invalid_input(
            "submit recovery cannot use explicit and readiness receipts together",
        ));
    }
    let mut locked = orchestration::lock_registry(context)?;
    let mut readiness_progress = if let Some(readiness) = readiness_receipt {
        let progress = idempotency_replay(
            &locked.registry,
            main,
            main_incarnation,
            readiness.idempotency_key,
            "worker-start",
            readiness.request_digest,
        )?
        .ok_or_else(|| invalid_input("worker start readiness receipt is unavailable"))?;
        let lease_is_live = progress["finalizer_lease_until_epoch"]
            .as_i64()
            .is_some_and(|lease| crate::coordination::now_epoch() < lease);
        if !worker_start_readiness_is_pending(&progress)
            || progress["finalizer_id"].as_str() != Some(readiness.finalizer_id)
            || !lease_is_live
        {
            return Err(CliError::data(
                "worker-start-finalizer-changed",
                "worker start readiness finalizer lease changed",
                None,
            ));
        }
        Some(progress)
    } else {
        None
    };
    let run = require_current_main(&locked.registry, main, main_incarnation)?.clone();
    let run_id = run.run_id.clone();
    let current = locked
        .registry
        .assignments
        .get_mut(assignment_id)
        .filter(|assignment| assignment.run_id == run_id)
        .ok_or_else(|| not_found("assignment-not-found", "assignment was not found"))?;
    ensure_primary_manager(current, main, main_incarnation)?;
    ensure_account_handoff_not_in_flight(current)?;
    if current.state != "starting" || current.submit_recovery.is_some() {
        return Err(CliError::data(
            "submit-recovery-ineligible",
            "submit recovery is allowed exactly once while the assignment remains starting",
            Some(json!({
                "state": current.state,
                "revision": current.revision,
                "attempted": current.submit_recovery.is_some()
            })),
        ));
    }
    if let Some(expected_revision) = expected_revision {
        ensure_revision(expected_revision, current.revision, "assignment")?;
    }
    let worker = current
        .worker
        .clone()
        .ok_or_else(|| not_found("worker-not-started", "worker session is not available"))?;
    if expected_worker.is_some_and(|expected| expected != &worker) {
        return Err(CliError::data(
            "session-incarnation-conflict",
            "worker identity changed before submit recovery reservation",
            None,
        ));
    }
    let attempt_id = uuid::Uuid::new_v4().to_string();
    let now = timestamp();
    current.revision = current.revision.saturating_add(1);
    let reserved_revision = current.revision;
    current.submit_recovery = Some(SubmitRecoveryRecord {
        schema_version: SUBMIT_RECOVERY_SCHEMA.to_string(),
        attempt_id: attempt_id.clone(),
        origin: if receipt.is_some() {
            "explicit".to_string()
        } else {
            "automatic".to_string()
        },
        run_id: Some(run_id.clone()),
        controller: Some(run.controller.clone()),
        session_incarnation: worker.session_incarnation.clone(),
        reserved_revision,
        state: "attempting".to_string(),
        attempt_count: 1,
        result: "single guarded Enter reserved".to_string(),
        attempted_at: now.clone(),
        updated_at: now.clone(),
    });
    current.updated_at = now;
    let reservation = SubmitRecoveryReservation {
        assignment_id: assignment_id.to_string(),
        attempt_id,
        reserved_revision,
        run_id,
        controller: run.controller,
        worker,
    };
    if let Some((idempotency_key, request_digest)) = receipt {
        store_receipt(
            &mut locked.registry,
            main,
            main_incarnation,
            idempotency_key,
            "worker-submit-recovery",
            request_digest,
            submit_recovery_progress(&reservation),
        )?;
    }
    if let (Some(readiness), Some(mut progress)) = (readiness_receipt, readiness_progress.take()) {
        progress["recovery_continuation"] =
            worker_start_recovery_continuation(&reservation, "reserved");
        progress["finalizer_lease_until_epoch"] = json!(
            crate::coordination::now_epoch()
                .saturating_add(worker_start_finalizer_lease_secs(&progress))
        );
        store_receipt(
            &mut locked.registry,
            main,
            main_incarnation,
            readiness.idempotency_key,
            "worker-start",
            readiness.request_digest,
            progress,
        )?;
    }
    locked.save()?;
    Ok(reservation)
}

fn submit_recovery_progress(reservation: &SubmitRecoveryReservation) -> Value {
    json!({
        "schema_version": "main-agent.worker-submit-recovery-progress.v1",
        "state": "in_progress",
        "reservation": {
            "assignment_id": reservation.assignment_id,
            "attempt_id": reservation.attempt_id,
            "reserved_revision": reservation.reserved_revision,
            "run_id": reservation.run_id,
            "controller": reservation.controller,
            "worker": reservation.worker
        }
    })
}

fn submit_recovery_reservation_from_progress(
    value: &Value,
) -> Result<SubmitRecoveryReservation, CliError> {
    if value["schema_version"] != "main-agent.worker-submit-recovery-progress.v1"
        || value["state"] != "in_progress"
    {
        return Err(CliError::data(
            "idempotency-conflict",
            "submit recovery receipt is not resumable",
            None,
        ));
    }
    let reservation = &value["reservation"];
    Ok(SubmitRecoveryReservation {
        assignment_id: reservation["assignment_id"]
            .as_str()
            .ok_or_else(|| invalid_input("submit recovery progress assignment is invalid"))?
            .to_string(),
        attempt_id: reservation["attempt_id"]
            .as_str()
            .ok_or_else(|| invalid_input("submit recovery progress attempt is invalid"))?
            .to_string(),
        reserved_revision: reservation["reserved_revision"]
            .as_u64()
            .ok_or_else(|| invalid_input("submit recovery progress revision is invalid"))?,
        run_id: reservation["run_id"]
            .as_str()
            .ok_or_else(|| invalid_input("submit recovery progress run is invalid"))?
            .to_string(),
        controller: serde_json::from_value(reservation["controller"].clone())
            .map_err(|_| invalid_input("submit recovery progress controller is invalid"))?,
        worker: serde_json::from_value(reservation["worker"].clone())
            .map_err(|_| invalid_input("submit recovery progress worker is invalid"))?,
    })
}

fn submit_recovery_reservation_from_assignment(
    assignment: &AssignmentRecord,
) -> Result<SubmitRecoveryReservation, CliError> {
    let recovery = assignment.submit_recovery.as_ref().ok_or_else(|| {
        invalid_input("submit recovery record is unavailable for final receipt reconciliation")
    })?;
    let worker = assignment.worker.clone().ok_or_else(|| {
        invalid_input("submit recovery worker is unavailable for final receipt reconciliation")
    })?;
    let run_id = recovery
        .run_id
        .clone()
        .filter(|run_id| run_id == &assignment.run_id)
        .ok_or_else(|| invalid_input("submit recovery run binding is invalid"))?;
    let controller = recovery
        .controller
        .clone()
        .filter(|controller| controller == &assignment.primary_manager)
        .ok_or_else(|| invalid_input("submit recovery controller binding is invalid"))?;
    if recovery.session_incarnation != worker.session_incarnation {
        return Err(invalid_input(
            "submit recovery worker incarnation binding is invalid",
        ));
    }
    Ok(SubmitRecoveryReservation {
        assignment_id: assignment.assignment_id.clone(),
        attempt_id: recovery.attempt_id.clone(),
        reserved_revision: recovery.reserved_revision,
        run_id,
        controller,
        worker,
    })
}

fn adopt_automatic_submit_recovery(
    context: &CliContext,
    main: &SessionRecord,
    main_incarnation: &str,
    assignment_id: &str,
    expected_revision: u64,
    idempotency_key: &str,
    request_digest: &str,
) -> Result<Option<SubmitRecoveryReservation>, CliError> {
    let mut locked = orchestration::lock_registry(context)?;
    let run = require_current_main(&locked.registry, main, main_incarnation)?.clone();
    let run_id = run.run_id.clone();
    let current = locked
        .registry
        .assignments
        .get(assignment_id)
        .filter(|assignment| assignment.run_id == run_id)
        .ok_or_else(|| not_found("assignment-not-found", "assignment was not found"))?;
    ensure_primary_manager(current, main, main_incarnation)?;
    let Some(recovery) = current
        .submit_recovery
        .as_ref()
        .filter(|recovery| recovery.origin == "automatic")
    else {
        return Ok(None);
    };
    let reservation_advanced_expected_revision =
        expected_revision.checked_add(1).is_some_and(|revision| {
            revision == current.revision && recovery.reserved_revision == current.revision
        });
    if expected_revision != current.revision && !reservation_advanced_expected_revision {
        ensure_revision(expected_revision, current.revision, "assignment")?;
    }
    let worker = current
        .worker
        .as_ref()
        .filter(|worker| worker.session_incarnation == recovery.session_incarnation)
        .ok_or_else(|| {
            CliError::data(
                "worker-incarnation-changed",
                "automatic submit recovery belongs to a different worker incarnation",
                None,
            )
        })?
        .clone();
    let reservation = SubmitRecoveryReservation {
        assignment_id: current.assignment_id.clone(),
        attempt_id: recovery.attempt_id.clone(),
        reserved_revision: recovery.reserved_revision,
        run_id: recovery
            .run_id
            .as_deref()
            .filter(|bound| *bound == current.run_id && *bound == run_id)
            .ok_or_else(|| {
                CliError::data(
                    "submit-recovery-controller-unbound",
                    "automatic submit recovery is not bound to the current run",
                    None,
                )
            })?
            .to_string(),
        controller: recovery
            .controller
            .as_ref()
            .filter(|bound| *bound == &current.primary_manager && *bound == &run.controller)
            .ok_or_else(|| {
                CliError::data(
                    "submit-recovery-controller-unbound",
                    "automatic submit recovery is not bound to the current controller",
                    None,
                )
            })?
            .clone(),
        worker,
    };
    store_receipt(
        &mut locked.registry,
        main,
        main_incarnation,
        idempotency_key,
        "worker-submit-recovery",
        request_digest,
        submit_recovery_progress(&reservation),
    )?;
    locked.save()?;
    Ok(Some(reservation))
}

fn submit_recovery_checkpoint(
    assignment: &AssignmentRecord,
    main: &SessionRecord,
    main_incarnation: &str,
    reservation: &SubmitRecoveryReservation,
) -> SubmitRecoveryCheckpoint {
    if reservation.controller.session_id != main.id
        || reservation.controller.session_incarnation != main_incarnation
        || assignment.run_id != reservation.run_id
        || assignment.primary_manager != reservation.controller
    {
        return SubmitRecoveryCheckpoint::Rejected("assignment-manager-handoff");
    }
    let Some(recovery) = assignment.submit_recovery.as_ref() else {
        return SubmitRecoveryCheckpoint::Rejected("submit-recovery-record-missing");
    };
    if recovery.attempt_id != reservation.attempt_id
        || recovery.session_incarnation != reservation.worker.session_incarnation
        || recovery.reserved_revision != reservation.reserved_revision
    {
        return SubmitRecoveryCheckpoint::Rejected("submit-recovery-attempt-conflict");
    }
    if assignment.worker.as_ref() != Some(&reservation.worker) {
        return SubmitRecoveryCheckpoint::Rejected("worker-incarnation-changed");
    }
    if assignment_has_preclaim_blocker(assignment) {
        return SubmitRecoveryCheckpoint::Rejected("worker-bootstrap-preclaim-failed");
    }
    if assignment.state == "starting" {
        return SubmitRecoveryCheckpoint::Pending;
    }
    if !matches!(
        assignment.state.as_str(),
        "working" | "blocked" | "submitted" | "accepted" | "released"
    ) {
        return SubmitRecoveryCheckpoint::Rejected(match assignment.state.as_str() {
            "cancelled" => "assignment-cancelled",
            _ => "assignment-state-without-worker-checkpoint",
        });
    }
    let Some(checkpoint) = assignment.checkpoint.as_ref() else {
        return SubmitRecoveryCheckpoint::Rejected("worker-checkpoint-missing");
    };
    if checkpoint.revision <= reservation.reserved_revision
        || checkpoint.revision > assignment.revision
    {
        return SubmitRecoveryCheckpoint::Rejected("worker-checkpoint-revision-conflict");
    }
    SubmitRecoveryCheckpoint::Confirmed
}

fn update_submit_recovery(
    context: &CliContext,
    main: &SessionRecord,
    main_incarnation: &str,
    reservation: &SubmitRecoveryReservation,
    requested_state: &str,
    requested_result: &str,
) -> Result<(bool, String), CliError> {
    let mut locked = orchestration::lock_registry(context)?;
    let current = locked
        .registry
        .assignments
        .get_mut(&reservation.assignment_id)
        .ok_or_else(|| not_found("assignment-not-found", "assignment was not found"))?;
    let checkpoint = submit_recovery_checkpoint(current, main, main_incarnation, reservation);
    let (confirmed, state, result) = match checkpoint {
        SubmitRecoveryCheckpoint::Confirmed => (
            true,
            "checkpoint_confirmed",
            "authenticated worker checkpoint confirmed",
        ),
        SubmitRecoveryCheckpoint::Rejected(code) => (false, "failed", code),
        SubmitRecoveryCheckpoint::Pending => (false, requested_state, requested_result),
    };
    if current.run_id != reservation.run_id
        || current.primary_manager != reservation.controller
        || reservation.controller.session_id != main.id
        || reservation.controller.session_incarnation != main_incarnation
    {
        return Ok((false, result.to_string()));
    }
    let Some(recovery) = current.submit_recovery.as_mut() else {
        return Ok((false, "submit-recovery-record-missing".to_string()));
    };
    if recovery.attempt_id != reservation.attempt_id
        || recovery.session_incarnation != reservation.worker.session_incarnation
        || recovery.reserved_revision != reservation.reserved_revision
    {
        return Ok((false, "submit-recovery-attempt-conflict".to_string()));
    }
    if recovery.state == "reconciled" {
        return Ok((false, recovery.result.clone()));
    }
    if matches!(recovery.state.as_str(), "failed" | "checkpoint_confirmed")
        && state != "checkpoint_confirmed"
    {
        return Ok((
            recovery.state == "checkpoint_confirmed",
            recovery.result.clone(),
        ));
    }
    if state == "sent" && recovery.state != "attempting" {
        return Ok((false, recovery.result.clone()));
    }
    recovery.state = state.to_string();
    recovery.result = result.to_string();
    recovery.updated_at = timestamp();
    current.updated_at = recovery.updated_at.clone();
    locked.save()?;
    Ok((confirmed, result.to_string()))
}

fn send_reserved_submit_recovery(
    context: &CliContext,
    reservation: &SubmitRecoveryReservation,
    readiness: Option<(&SessionRecord, &str, &WorkerStartReadinessReceipt<'_>)>,
) -> Result<(), String> {
    let worker_record = load_session_record(context, &reservation.worker.session_id)
        .map_err(|error| error.code().to_string())?;
    let worker_incarnation = worker_record
        .runtime
        .as_ref()
        .map(|runtime| runtime.launch_id.as_str())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "worker-incarnation-unavailable".to_string())?;
    if !orchestration::session_ref_matches(&reservation.worker, &worker_record, worker_incarnation)
    {
        return Err("worker-incarnation-changed".to_string());
    }
    send_submit_recovery_input_serialized(
        context,
        &worker_record,
        &reservation.worker.session_incarnation,
        &reservation.controller.session_id,
        &reservation.controller.session_incarnation,
        &resolve_tmux_bin(None),
        || {
            let locked = orchestration::lock_registry(context)?;
            if let Some((main, main_incarnation, readiness)) = readiness {
                let progress = idempotency_replay(
                    &locked.registry,
                    main,
                    main_incarnation,
                    readiness.idempotency_key,
                    "worker-start",
                    readiness.request_digest,
                )?
                .ok_or_else(|| {
                    CliError::data(
                        "worker-start-finalizer-changed",
                        "worker start readiness receipt is unavailable before recovery Enter",
                        None,
                    )
                })?;
                let lease_is_live = progress["finalizer_lease_until_epoch"]
                    .as_i64()
                    .is_some_and(|lease| crate::coordination::now_epoch() < lease);
                let continuation_matches = progress["recovery_continuation"]["stage"] == "sending"
                    && progress["recovery_continuation"]["reservation"]["attempt_id"]
                        == reservation.attempt_id
                    && progress["recovery_continuation"]["reservation"]["reserved_revision"]
                        == reservation.reserved_revision;
                if !worker_start_readiness_is_pending(&progress)
                    || progress["finalizer_id"].as_str() != Some(readiness.finalizer_id)
                    || !lease_is_live
                    || !continuation_matches
                {
                    return Err(CliError::data(
                        "worker-start-finalizer-changed",
                        "worker start readiness finalizer lease changed before recovery Enter",
                        None,
                    ));
                }
            }
            {
                let run = locked
                    .registry
                    .runs
                    .get(&reservation.run_id)
                    .filter(|run| run.state == "active" && run.controller == reservation.controller)
                    .ok_or_else(|| {
                        CliError::data(
                            "main-agent-authority-changed",
                            "reserving Main Agent no longer controls the recovery run",
                            None,
                        )
                    })?;
                let assignment = locked
                    .registry
                    .assignments
                    .get(&reservation.assignment_id)
                    .filter(|assignment| {
                        assignment.run_id == run.run_id
                            && assignment.primary_manager == reservation.controller
                            && assignment.worker.as_ref() == Some(&reservation.worker)
                            && assignment.state == "starting"
                            && assignment.revision == reservation.reserved_revision
                            && assignment.checkpoint.as_ref().is_none_or(|checkpoint| {
                                checkpoint.revision <= reservation.reserved_revision
                            })
                    })
                    .ok_or_else(|| {
                        CliError::data(
                            "main-agent-authority-changed",
                            "recovery assignment changed after its Enter reservation",
                            None,
                        )
                    })?;
                let recovery = assignment
                    .submit_recovery
                    .as_ref()
                    .filter(|recovery| {
                        recovery.attempt_id == reservation.attempt_id
                            && recovery.reserved_revision == reservation.reserved_revision
                            && recovery.run_id.as_deref() == Some(reservation.run_id.as_str())
                            && recovery.controller.as_ref() == Some(&reservation.controller)
                            && recovery.session_incarnation
                                == reservation.worker.session_incarnation
                            && recovery.state == "attempting"
                    })
                    .ok_or_else(|| {
                        CliError::data(
                            "submit-recovery-attempt-conflict",
                            "submit recovery reservation changed before Enter",
                            None,
                        )
                    })?;
                let _ = recovery;
            }
            Ok(locked)
        },
    )
    .map_err(|error| error.code().to_string())
}

fn submit_recovery_send_outcome_is_unknown(code: &str) -> bool {
    matches!(
        code,
        "command-timeout" | "command-wait-failed" | "command-failed"
    )
}

fn await_submit_recovery_result(
    context: &CliContext,
    main: &SessionRecord,
    main_incarnation: &str,
    reservation: &SubmitRecoveryReservation,
    timeout: Duration,
) -> Result<(bool, String), CliError> {
    let started = Instant::now();
    let mut last_activity_check = started
        .checked_sub(WORKER_ACTIVITY_POLL_INTERVAL)
        .unwrap_or(started);
    loop {
        let registry = orchestration::load_registry_readonly(context)?;
        let current = registry
            .assignments
            .get(&reservation.assignment_id)
            .ok_or_else(|| not_found("assignment-not-found", "assignment was not found"))?;
        if let Some(recovery) = current.submit_recovery.as_ref()
            && recovery.attempt_id == reservation.attempt_id
            && recovery.session_incarnation == reservation.worker.session_incarnation
            && recovery.reserved_revision == reservation.reserved_revision
            && recovery.state == "reconciled"
        {
            return Ok((false, recovery.result.clone()));
        }
        match submit_recovery_checkpoint(current, main, main_incarnation, reservation) {
            SubmitRecoveryCheckpoint::Confirmed => {
                return update_submit_recovery(
                    context,
                    main,
                    main_incarnation,
                    reservation,
                    "checkpoint_confirmed",
                    "authenticated worker checkpoint confirmed",
                );
            }
            SubmitRecoveryCheckpoint::Rejected(code) => {
                let (_, result) = update_submit_recovery(
                    context,
                    main,
                    main_incarnation,
                    reservation,
                    "failed",
                    code,
                )?;
                return Ok((false, result));
            }
            SubmitRecoveryCheckpoint::Pending => {}
        }
        let recovery = current
            .submit_recovery
            .as_ref()
            .ok_or_else(|| invalid_input("submit recovery record is unavailable"))?;
        if recovery.attempt_id != reservation.attempt_id {
            return Ok((false, "submit-recovery-attempt-conflict".to_string()));
        }
        if recovery.state == "failed" {
            return Ok((false, recovery.result.clone()));
        }
        let should_check_activity = recovery.state == "sent"
            && last_activity_check.elapsed() >= WORKER_ACTIVITY_POLL_INTERVAL;
        if should_check_activity {
            last_activity_check = Instant::now();
        }
        if should_check_activity {
            let worker_record = load_session_record(context, &reservation.worker.session_id);
            if worker_record.as_ref().is_ok_and(|record| {
                authoritative_worker_turn_terminated(
                    context,
                    record,
                    &reservation.worker.session_incarnation,
                )
            }) {
                let (_, result) = update_submit_recovery(
                    context,
                    main,
                    main_incarnation,
                    reservation,
                    "failed",
                    "provider-turn-terminated-without-checkpoint",
                )?;
                return Ok((false, result));
            }
        }
        if started.elapsed() >= timeout {
            if recovery.state == "attempting" {
                // An observer cannot infer that the process which owns this
                // reservation is dead. Return a typed unknown outcome without
                // revoking its send authority or clearing manager-mutation
                // fences. The original sender must still pass the serialized
                // boundary, and no observer is allowed to send.
                return Ok((false, "submit-recovery-send-outcome-unknown".to_string()));
            }
            let (_, result) = update_submit_recovery(
                context,
                main,
                main_incarnation,
                reservation,
                "failed",
                "checkpoint-timeout",
            )?;
            return Ok((false, result));
        }
        thread::sleep(WORKER_WAIT_POLL_INTERVAL);
    }
}

/// T1 teardown macro: retire an accepted (or already terminal) assignment in
/// one call by composing release -> delete and reporting the worker's absence
/// from the delete result. Replaces the hand-run release -> delete -> confirm
/// sequence. Idempotent: it reads current state, skips steps already done, and
/// derives per-step idempotency keys from the retire key so a retry converges
/// through each step's own receipt.
fn run_worker_retire(
    context: &CliContext,
    args: AssignmentMutationArgs,
) -> Result<Value, CliError> {
    validate_idempotency_key(&args.idempotency_key)?;
    let (record, incarnation) = authenticated_self(context)?;
    ensure_active_claim(context, &record)?;
    let request_digest = crate::coordination::request_digest(
        "worker-retire",
        &json!({
            "assignment_id": args.assignment_id,
            "if_revision": args.if_revision
        }),
    );
    let registry = orchestration::load_registry_readonly(context)?;
    let replay = idempotency_replay(
        &registry,
        &record,
        &incarnation,
        &args.idempotency_key,
        "worker-retire",
        &request_digest,
    )?;
    let mut progress = match replay {
        Some(value) if value["schema_version"] == "main-agent.worker-retire-result.v1" => {
            return Ok(value);
        }
        Some(value)
            if value["schema_version"] == "main-agent.worker-retire-progress.v1"
                && value["state"] == "in_progress" =>
        {
            value
        }
        Some(_) => {
            return Err(CliError::data(
                "idempotency-conflict",
                "worker retire receipt is not resumable",
                None,
            ));
        }
        None => {
            let run = require_current_main(&registry, &record, &incarnation)?;
            let assignment = registry
                .assignments
                .get(&args.assignment_id)
                .filter(|assignment| assignment.run_id == run.run_id)
                .ok_or_else(|| not_found("assignment-not-found", "assignment was not found"))?;
            ensure_primary_manager(assignment, &record, &incarnation)?;
            ensure_account_handoff_not_in_flight(assignment)?;
            if !matches!(
                assignment.state.as_str(),
                "accepted" | "released" | "cancelled"
            ) {
                return Err(CliError::data(
                    "assignment-not-retireable",
                    "worker retire requires an accepted, released, or cancelled assignment",
                    Some(json!({
                        "assignment_id": args.assignment_id,
                        "state": assignment.state
                    })),
                ));
            }
            let historical_release = if assignment.state == "released" {
                let release_key =
                    compatible_child_idempotency_key(&args.idempotency_key, "release");
                let release_digest = crate::coordination::request_digest(
                    "worker-release",
                    &json!({
                        "assignment_id": args.assignment_id,
                        "if_revision": args.if_revision
                    }),
                );
                idempotency_replay(
                    &registry,
                    &record,
                    &incarnation,
                    &release_key,
                    "worker-release",
                    &release_digest,
                )?
            } else {
                None
            };
            let progress = if let Some(release) = historical_release {
                if release["assignment"]["revision"].as_u64() != Some(assignment.revision) {
                    return Err(CliError::data(
                        "idempotency-conflict",
                        "historical worker release receipt does not match the released assignment",
                        None,
                    ));
                }
                json!({
                    "schema_version": "main-agent.worker-retire-progress.v1",
                    "state": "in_progress",
                    "assignment_id": args.assignment_id,
                    "initial_revision": args.if_revision,
                    "initial_state": "accepted",
                    "release": release,
                    "delete": Value::Null
                })
            } else {
                ensure_revision(args.if_revision, assignment.revision, "assignment")?;
                json!({
                    "schema_version": "main-agent.worker-retire-progress.v1",
                    "state": "in_progress",
                    "assignment_id": args.assignment_id,
                    "initial_revision": assignment.revision,
                    "initial_state": assignment.state,
                    "release": Value::Null,
                    "delete": Value::Null
                })
            };
            persist_worker_retire_receipt(
                context,
                &record,
                &incarnation,
                &args.idempotency_key,
                &request_digest,
                progress.clone(),
            )?;
            progress
        }
    };
    let state = progress["initial_state"]
        .as_str()
        .ok_or_else(|| invalid_input("worker retire progress state is invalid"))?;
    let mut revision = progress["initial_revision"]
        .as_u64()
        .ok_or_else(|| invalid_input("worker retire progress revision is invalid"))?;
    let released = state == "accepted";
    if released {
        let release = if progress["release"].is_null() {
            let value = run_assignment_state(
                context,
                AssignmentMutationArgs {
                    assignment_id: args.assignment_id.clone(),
                    if_revision: revision,
                    idempotency_key: compatible_child_idempotency_key(
                        &args.idempotency_key,
                        "release",
                    ),
                    format: OutputFormat::Json,
                },
                "accepted",
                "released",
                "worker-release",
            )?;
            progress["release"] = value.clone();
            persist_worker_retire_receipt(
                context,
                &record,
                &incarnation,
                &args.idempotency_key,
                &request_digest,
                progress.clone(),
            )?;
            value
        } else {
            progress["release"].clone()
        };
        revision = release["assignment"]["revision"]
            .as_u64()
            .ok_or_else(|| invalid_input("worker release result revision is invalid"))?;
    }
    let delete = if progress["delete"].is_null() {
        let value = run_worker_delete(
            context,
            AssignmentMutationArgs {
                assignment_id: args.assignment_id.clone(),
                if_revision: revision,
                idempotency_key: compatible_child_idempotency_key(&args.idempotency_key, "delete"),
                format: OutputFormat::Json,
            },
        )?;
        progress["delete"] = value.clone();
        persist_worker_retire_receipt(
            context,
            &record,
            &incarnation,
            &args.idempotency_key,
            &request_digest,
            progress.clone(),
        )?;
        value
    } else {
        progress["delete"].clone()
    };
    let deleted = delete
        .get("deleted")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let cleanup_pending = delete
        .get("cleanup_pending")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let outcome = json!({
        "schema_version": "main-agent.worker-retire-result.v1",
        "assignment_id": args.assignment_id,
        "released": released,
        "deleted": deleted,
        "cleanup_pending": cleanup_pending,
        "run_closed": delete.get("run_closed").cloned().unwrap_or(Value::Bool(false)),
        "retired": deleted
    });
    persist_worker_retire_receipt(
        context,
        &record,
        &incarnation,
        &args.idempotency_key,
        &request_digest,
        outcome.clone(),
    )?;
    Ok(outcome)
}

fn persist_worker_retire_receipt(
    context: &CliContext,
    record: &SessionRecord,
    incarnation: &str,
    idempotency_key: &str,
    request_digest: &str,
    outcome: Value,
) -> Result<(), CliError> {
    let mut locked = orchestration::lock_registry(context)?;
    store_receipt(
        &mut locked.registry,
        record,
        incarnation,
        idempotency_key,
        "worker-retire",
        request_digest,
        outcome,
    )?;
    locked.save()
}

fn run_worker_diagnose(context: &CliContext, args: WorkerDiagnoseArgs) -> Result<Value, CliError> {
    diagnose_worker(context, &args.assignment_id)
}

fn run_worker_supervise(context: &CliContext, args: WorkerDiagnoseArgs) -> Result<Value, CliError> {
    let diagnosis = diagnose_worker(context, &args.assignment_id)?;
    Ok(json!({
        "schema_version": "main-agent.worker-supervise-result.v1",
        "assignment_id": args.assignment_id,
        "classification": diagnosis["classification"],
        "next_action": diagnosis["next_action"],
        "recovery_action": diagnosis["recovery_action"],
        "automatic_retry_safe": diagnosis["automatic_retry_safe"],
        "last_proven_safe_state": diagnosis
    }))
}

enum DiagnosticEvidence<T> {
    Present(T),
    Absent(&'static str),
    Unavailable(String),
    IdentityMismatch(&'static str),
}

impl<T> DiagnosticEvidence<T> {
    fn value(&self) -> Option<&T> {
        match self {
            Self::Present(value) => Some(value),
            _ => None,
        }
    }

    fn is_unavailable_or_mismatched(&self) -> bool {
        matches!(self, Self::Unavailable(_) | Self::IdentityMismatch(_))
    }

    fn projection(&self) -> Value {
        match self {
            Self::Present(_) => json!({ "state": "present", "error_code": Value::Null }),
            Self::Absent(code) => json!({ "state": "absent", "error_code": code }),
            Self::Unavailable(code) => {
                json!({ "state": "unavailable", "error_code": code })
            }
            Self::IdentityMismatch(code) => {
                json!({ "state": "identity_mismatch", "error_code": code })
            }
        }
    }
}

struct CoordinationDiagnosis {
    guidance: crate::coordination::GuidanceSummary,
    broker_authoritative: bool,
    broker_lost_since_epoch: Option<i64>,
    claim_active: bool,
    claim_id: Option<String>,
    claim_revision: Option<u64>,
    claim_expires_at: Option<String>,
    claim_expires_at_epoch: Option<i64>,
    active_operations: u64,
    uncertain_operations: u64,
}

fn has_terminal_reconciled_recovery(assignment: &AssignmentRecord, run: &RunRecord) -> bool {
    assignment.state == "starting"
        && assignment.run_id == run.run_id
        && assignment.primary_manager == run.controller
        && assignment
            .worker
            .as_ref()
            .zip(assignment.submit_recovery.as_ref())
            .is_some_and(|(worker, recovery)| {
                recovery.state == "reconciled"
                    && recovery.run_id.as_deref() == Some(run.run_id.as_str())
                    && recovery.controller.as_ref() == Some(&run.controller)
                    && recovery.session_incarnation == worker.session_incarnation
            })
}

fn diagnose_worker(context: &CliContext, assignment_id: &str) -> Result<Value, CliError> {
    let (main, main_incarnation) = authenticated_self(context)?;
    let registry = orchestration::load_registry_readonly(context)?;
    let run = require_current_main(&registry, &main, &main_incarnation)?;
    let assignment = registry
        .assignments
        .get(assignment_id)
        .filter(|assignment| assignment.run_id == run.run_id)
        .ok_or_else(|| not_found("assignment-not-found", "assignment was not found"))?
        .clone();
    ensure_primary_manager(&assignment, &main, &main_incarnation)?;
    let terminal_recovery_recorded = has_terminal_reconciled_recovery(&assignment, run);

    let packet_evidence =
        match orchestration::read_packet(context, &assignment.private_packet_digest) {
            Ok(packet) => match serde_json::from_value::<AssignmentInput>(packet) {
                Ok(packet) => DiagnosticEvidence::Present(packet),
                Err(_) => {
                    DiagnosticEvidence::Unavailable("stored-assignment-packet-invalid".to_string())
                }
            },
            Err(error) => DiagnosticEvidence::Unavailable(error.code().to_string()),
        };
    let session_evidence = match assignment.worker.as_ref() {
        None => DiagnosticEvidence::Absent("worker-not-bound"),
        Some(worker) => match load_session_record(context, &worker.session_id) {
            Ok(record) => {
                let actual_incarnation = record
                    .runtime
                    .as_ref()
                    .map(|runtime| runtime.launch_id.as_str())
                    .unwrap_or_default();
                if orchestration::session_ref_matches(worker, &record, actual_incarnation) {
                    DiagnosticEvidence::Present(record)
                } else {
                    DiagnosticEvidence::IdentityMismatch("worker-session-incarnation-conflict")
                }
            }
            Err(error) if error.code() == "session-not-found" => {
                DiagnosticEvidence::Absent("worker-session-not-found")
            }
            Err(error) => DiagnosticEvidence::Unavailable(error.code().to_string()),
        },
    };
    let activity_evidence = match session_evidence.value() {
        Some(record) => match crate::activity::activity_status(context, &record.id) {
            Ok(result) if result.turn_state.phase != crate::activity::TurnPhase::Unknown => {
                DiagnosticEvidence::Present(result.turn_state)
            }
            Ok(_) => DiagnosticEvidence::Unavailable("worker-activity-unknown".to_string()),
            Err(error) => DiagnosticEvidence::Unavailable(error.code().to_string()),
        },
        None => DiagnosticEvidence::Absent("worker-session-evidence-absent"),
    };
    let coordination_evidence = match assignment.worker.as_ref() {
        Some(worker) if session_evidence.value().is_some() => {
            match crate::coordination::lock_session_quiescence(
                context,
                &worker.session_id,
                &worker.session_incarnation,
            ) {
                Ok(guard)
                    if terminal_recovery_recorded
                        && (!guard.broker_present || guard.broker_identity_matched) =>
                {
                    DiagnosticEvidence::Present(CoordinationDiagnosis {
                        guidance: guard.guidance_summary(
                            &worker.session_id,
                            &worker.session_incarnation,
                            &assignment.primary_manager.session_id,
                            &assignment.primary_manager.session_incarnation,
                        ),
                        broker_authoritative: guard.broker_authoritative,
                        broker_lost_since_epoch: guard.broker_lost_since_epoch,
                        claim_active: guard.active_claim,
                        claim_id: guard.claim_id,
                        claim_revision: guard.claim_revision,
                        claim_expires_at: guard.claim_expires_at,
                        claim_expires_at_epoch: guard.claim_expires_at_epoch,
                        active_operations: u64::from(guard.active_operation),
                        uncertain_operations: u64::from(guard.uncertain_operation),
                    })
                }
                Ok(guard) if !guard.broker_present => {
                    DiagnosticEvidence::Unavailable("coordination-broker-unavailable".to_string())
                }
                Ok(guard) if !guard.broker_identity_matched => {
                    DiagnosticEvidence::IdentityMismatch("coordination-broker-incarnation-conflict")
                }
                Ok(guard) => DiagnosticEvidence::Present(CoordinationDiagnosis {
                    guidance: guard.guidance_summary(
                        &worker.session_id,
                        &worker.session_incarnation,
                        &assignment.primary_manager.session_id,
                        &assignment.primary_manager.session_incarnation,
                    ),
                    broker_authoritative: guard.broker_authoritative,
                    broker_lost_since_epoch: guard.broker_lost_since_epoch,
                    claim_active: guard.active_claim,
                    claim_id: guard.claim_id,
                    claim_revision: guard.claim_revision,
                    claim_expires_at: guard.claim_expires_at,
                    claim_expires_at_epoch: guard.claim_expires_at_epoch,
                    active_operations: u64::from(guard.active_operation),
                    uncertain_operations: u64::from(guard.uncertain_operation),
                }),
                Err(error) => DiagnosticEvidence::Unavailable(error.code().to_string()),
            }
        }
        _ => DiagnosticEvidence::Absent("worker-coordination-evidence-absent"),
    };
    let worker_status = session_evidence
        .value()
        .map(|record| session_status(&resolve_tmux_bin(None), record))
        .unwrap_or_else(|| "missing".to_string());
    let claim_active = coordination_evidence
        .value()
        .is_some_and(|value| value.claim_active);
    let claim_revision = coordination_evidence
        .value()
        .and_then(|value| value.claim_revision);
    let claim_id = coordination_evidence
        .value()
        .and_then(|value| value.claim_id.as_deref());
    let claim_expires_at = coordination_evidence
        .value()
        .and_then(|value| value.claim_expires_at.as_deref());
    let claim_expires_in_seconds = coordination_evidence
        .value()
        .and_then(|value| value.claim_expires_at_epoch)
        .map(|expires| expires.saturating_sub(crate::coordination::now_epoch()));
    let broker_authoritative = coordination_evidence
        .value()
        .is_some_and(|value| value.broker_authoritative);
    let broker_lost_since_epoch = coordination_evidence
        .value()
        .and_then(|value| value.broker_lost_since_epoch);
    let active_operations = coordination_evidence
        .value()
        .map(|value| value.active_operations)
        .unwrap_or(0);
    let uncertain_operations = coordination_evidence
        .value()
        .map(|value| value.uncertain_operations)
        .unwrap_or(0);
    let activity = activity_evidence.value();
    let provider_terminated = activity.as_ref().is_some_and(|state| {
        state.phase == crate::activity::TurnPhase::Waiting
            && state.last_turn.is_some()
            && state.source.confidence == crate::activity::Confidence::Authoritative
    });
    let attention_kind = activity
        .as_ref()
        .and_then(|state| state.current_turn.as_ref())
        .and_then(|turn| turn.attention.as_ref())
        .map(|attention| bounded_attention_kind(&attention.kind));
    let startup_dialog =
        activity.is_some_and(|state| state.phase == crate::activity::TurnPhase::NeedsInput);
    let account_view = session_evidence
        .value()
        .map(crate::codex_account::view_for_record)
        .and_then(|view| serde_json::to_value(view).ok())
        .unwrap_or(Value::Null);
    let auto_resume_view = session_evidence
        .value()
        .map(|record| crate::auto_resume::view_for_record(context, record))
        .and_then(|view| serde_json::to_value(view).ok())
        .unwrap_or(Value::Null);
    let provider_resume_preserved = session_evidence
        .value()
        .is_some_and(|record| record.provider_resume.is_some());
    let structured_quota_or_credit_evidence = matches!(attention_kind, Some("quota_or_credits"))
        || activity
            .and_then(|state| state.last_turn.as_ref())
            .is_some_and(|turn| bounded_quota_outcome(&turn.outcome));
    // Guidance is projected from the same locked coordination snapshot when
    // that snapshot is usable. Any lock/schema failure is already represented
    // by coordination evidence, so it must not create a second, higher
    // precedence failure classification.
    let guidance_unavailable = false;
    let guidance = coordination_evidence
        .value()
        .map(|value| value.guidance.clone())
        .unwrap_or_default();
    let worktree = session_evidence
        .value()
        .map(|record| PathBuf::from(&record.cwd))
        .or_else(|| {
            packet_evidence
                .value()
                .map(|packet| PathBuf::from(&packet.launch.cwd))
        });
    let worktree_progress = worktree.as_deref().map_or_else(
        || json!({ "available": false, "clean": false, "change_count": Value::Null }),
        |path| {
            inspect_worktree_progress(context, &assignment, path).unwrap_or_else(|error| {
                json!({
                    "available": false,
                    "clean": false,
                    "change_count": Value::Null,
                    "reason_code": error.code()
                })
            })
        },
    );
    let worktree_unavailable = worktree_progress["available"] != true;
    let worktree_clean = worktree_progress["clean"].as_bool().unwrap_or(false);
    let provider_active =
        activity.is_some_and(|state| state.phase == crate::activity::TurnPhase::Working);
    let provider_progress_stale = activity
        .and_then(|state| state.current_turn.as_ref())
        .and_then(|turn| {
            turn.last_progress_at
                .as_deref()
                .unwrap_or(&turn.started_at)
                .parse::<jiff::Timestamp>()
                .ok()
        })
        .is_some_and(|last_progress| {
            jiff::Timestamp::now()
                .as_second()
                .saturating_sub(last_progress.as_second())
                >= WORKER_PROVIDER_STALE_SECS
        });
    let material_progress_stale = worktree_clean
        || worktree_progress["snapshot_age_seconds"]
            .as_i64()
            .is_some_and(|age| age >= WORKER_PROVIDER_STALE_SECS);
    let provider_activity_stale =
        provider_active && provider_progress_stale && material_progress_stale;
    let capability_advertised = session_evidence
        .value()
        .is_some_and(crate::codex_app_server::managed_account_handoff_supported);
    let controls_supported = capability_advertised
        && account_view["supported"] == true
        && auto_resume_view["supported"] == true;
    let raw_rate_limit_diagnostic = if raw_rate_limit_diagnostic_required(
        provider_activity_stale,
        structured_quota_or_credit_evidence,
        controls_supported,
        session_evidence.value().map(|record| record.agent.as_str()),
    ) {
        selected_raw_account_provenance(&account_view).map_or_else(
            || RawRateLimitDiagnostic::Unavailable("selected-raw-account-unavailable"),
            diagnose_selected_raw_account_rate_limits,
        )
    } else {
        RawRateLimitDiagnostic::NotRequested
    };
    let quota_or_credit_evidence =
        structured_quota_or_credit_evidence || raw_rate_limit_diagnostic.is_exhausted();
    let account_handoff = account_handoff_facts(
        &account_view,
        &auto_resume_view,
        quota_or_credit_evidence,
        startup_dialog && attention_kind == Some("authentication"),
        capability_advertised,
    );
    let account_handoff_state_eligible = matches!(
        assignment.state.as_str(),
        "starting" | "working" | "blocked"
    );
    let worker_unreachable =
        assignment.worker.is_some() && matches!(&session_evidence, DiagnosticEvidence::Absent(_));
    let evidence_unavailable = packet_evidence.is_unavailable_or_mismatched()
        || session_evidence.is_unavailable_or_mismatched()
        || activity_evidence.is_unavailable_or_mismatched()
        || coordination_evidence.is_unavailable_or_mismatched()
        || worktree_unavailable
        || guidance_unavailable
        || raw_rate_limit_diagnostic.is_unavailable();
    let preclaim_blocker = assignment_has_preclaim_blocker(&assignment);
    let terminal_recovery_reconciled =
        terminal_recovery_recorded && worker_status == "stopped" && assignment.worker.is_some();
    let failed_preclaim = worker_failed_preclaim(PreClaimEvidence {
        assignment_state: &assignment.state,
        claim_active,
        operations_quiescent: active_operations == 0 && uncertain_operations == 0,
        worker_bound: assignment.worker.is_some(),
        worker_status: &worker_status,
        preclaim_blocker,
        provider_terminated,
        terminal_recovery_reconciled,
    });
    let starting_provider_terminated = assignment.state == "starting" && provider_terminated;
    let terminal_quiescent = matches!(assignment.state.as_str(), "cancelled" | "released")
        && !claim_active
        && active_operations == 0
        && uncertain_operations == 0;
    // Sample the exact edit-hook heartbeat at the end of diagnosis. A boolean
    // captured before worktree/provider probes can expire before the worker's
    // next PreToolUse gate and falsely report healthy progress.
    let edit_authority_heartbeat_age_seconds = assignment.worker.as_ref().and_then(|worker| {
        nils_common::coordination_projection::heartbeat_age_seconds(
            &context.state_dir,
            &worker.session_id,
            &worker.session_incarnation,
            crate::coordination::now_epoch(),
        )
    });
    let edit_authority_fresh = broker_authoritative
        && edit_authority_heartbeat_age_seconds
            .is_some_and(|age| age <= WORKER_EDIT_AUTHORITY_HEARTBEAT_FRESH_SECS);
    let assignment_nonterminal = matches!(
        assignment.state.as_str(),
        "starting" | "working" | "blocked"
    );
    let edit_authority_stale = assignment_nonterminal && claim_active && !edit_authority_fresh;
    let coordination_broker_stale = edit_authority_stale && broker_lost_since_epoch.is_some();
    let claim_renewal_required = worker_claim_renewal_required(
        &assignment.state,
        claim_active,
        broker_authoritative,
        claim_expires_in_seconds,
    );
    let cancel_then_reassign_safe = failed_preclaim
        && worktree_clean
        && !startup_dialog
        && !evidence_unavailable
        && !worker_unreachable;
    let new_assignment_safe = (failed_preclaim || terminal_quiescent)
        && worktree_clean
        && !startup_dialog
        && !evidence_unavailable
        && !worker_unreachable;
    let orphan_guidance_quarantine_required =
        guidance.stale_incarnation_unread_count > 0 && assignment.previous_worker.is_none();

    let facts = WorkerDiagnosisFacts {
        evidence_unavailable,
        worker_unreachable,
        active_or_uncertain_operation: active_operations > 0 || uncertain_operations > 0,
        coordination_broker_stale,
        edit_authority_stale,
        claim_renewal_required: claim_renewal_required && !failed_preclaim,
        orphan_guidance_quarantine_required,
        guidance_continuity_required: guidance.stale_incarnation_unread_count > 0
            && assignment.previous_worker.is_some(),
        startup_dialog,
        account_handoff_capability_gap: account_handoff_state_eligible
            && account_handoff.capability_gap,
        account_handoff_required: account_handoff_state_eligible && account_handoff.required,
        provider_activity_stale,
        unread_guidance: guidance.unread_count > 0,
        preclaim_blocker,
        runtime_gone_preclaim: failed_preclaim
            && !preclaim_blocker
            && !terminal_recovery_reconciled
            && !starting_provider_terminated,
        terminal_recovery_reconciled,
        starting_provider_terminated,
        terminal_quiescent,
        submitted: assignment.state == "submitted",
        reassignment_safe: new_assignment_safe,
    };
    let (classification, next_action, automatic_retry_safe) = classify_worker_diagnosis(facts);
    let recovery_action =
        worker_recovery_action(classification, &assignment, claim_id, claim_revision);

    let activity_view = activity.map(|state| {
        json!({
            "phase": state.phase,
            "revision": state.revision,
            "confidence": state.source.confidence,
            "authoritative_turn_terminated": provider_terminated,
            "attention_kind": attention_kind
        })
    });
    Ok(json!({
        "schema_version": "main-agent.worker-diagnose-result.v1",
        "assignment_id": assignment.assignment_id,
        "assignment_revision": assignment.revision,
        "assignment_state": assignment.state,
        "classification": classification,
        "next_action": next_action,
        "recovery_action": recovery_action,
        "automatic_retry_safe": automatic_retry_safe,
        "failed_preclaim": failed_preclaim,
        "cancel_then_reassign_safe": cancel_then_reassign_safe,
        "new_assignment_safe": new_assignment_safe,
        "reassignment_safe": new_assignment_safe,
        "worker": {
            "bound": assignment.worker.is_some(),
            "identity_matched": session_evidence.value().is_some(),
            "status": worker_status
        },
        "activity": activity_view,
        "account": account_view,
        "auto_resume": auto_resume_view,
        "quota_or_credit_evidence": quota_or_credit_evidence,
        "raw_rate_limit_diagnostic": raw_rate_limit_diagnostic.projection(),
        "provider_resume_preserved": provider_resume_preserved,
        "guidance": {
            "state": if orphan_guidance_quarantine_required {
                "orphan_stale_incarnation_unread"
            } else if guidance.stale_incarnation_unread_count > 0 {
                "stale_incarnation_unread"
            } else if guidance.unread_count > 0 {
                "queued_unread"
            } else if guidance.consumed_count > 0 {
                "consumed"
            } else {
                "none"
            },
            "unread_count": guidance.unread_count,
            "consumed_count": guidance.consumed_count,
            "stale_incarnation_unread_count": guidance.stale_incarnation_unread_count
        },
        "progress": {
            "provider_active": provider_active,
            "provider_activity_stale": provider_activity_stale,
            "material_worktree_changes": worktree_progress["change_count"],
            "assignment_revision": assignment.revision
        },
        "coordination": {
            "broker_authoritative": broker_authoritative,
            "claim_active": claim_active,
            "claim_id": claim_id,
            "claim_revision": claim_revision,
            "claim_expires_at": claim_expires_at,
            "claim_expires_in_seconds": claim_expires_in_seconds,
            "edit_authority_fresh": edit_authority_fresh,
            "edit_authority_heartbeat_age_seconds": edit_authority_heartbeat_age_seconds,
            "edit_authority_state": if coordination_broker_stale {
                "broker_lost"
            } else if edit_authority_stale {
                "stale"
            } else {
                "fresh"
            },
            "broker_lost_since_epoch": broker_lost_since_epoch,
            "claim_renewal_source": if broker_authoritative && claim_active {
                "automatic_broker_heartbeat"
            } else {
                "target_owned_explicit_fallback"
            },
            "active_operations": active_operations,
            "uncertain_operations": uncertain_operations
        },
        "worktree_progress": worktree_progress,
        "submit_recovery": assignment.submit_recovery,
        "evidence": {
            "packet": packet_evidence.projection(),
            "session": session_evidence.projection(),
            "activity": activity_evidence.projection(),
            "coordination": coordination_evidence.projection(),
            "guidance": if guidance_unavailable {
                json!({ "state": "unavailable", "error_code": "guidance-evidence-unavailable" })
            } else {
                json!({ "state": "present", "error_code": Value::Null })
            },
            "worktree": if worktree_unavailable {
                json!({ "state": "unavailable", "error_code": "worktree-evidence-unavailable" })
            } else {
                json!({ "state": "present", "error_code": Value::Null })
            }
        }
    }))
}

fn worker_recovery_action(
    classification: &str,
    assignment: &AssignmentRecord,
    claim_id: Option<&str>,
    claim_revision: Option<u64>,
) -> Value {
    let main_owner = json!({
        "role": "main",
        "session_id": assignment.primary_manager.session_id,
        "session_incarnation": assignment.primary_manager.session_incarnation
    });
    let worker_owner = assignment.worker.as_ref().map(|worker| {
        json!({
            "role": "worker",
            "session_id": worker.session_id,
            "session_incarnation": worker.session_incarnation
        })
    });
    let supervise = json!([
        "main-agent",
        "worker",
        "supervise",
        assignment.assignment_id,
        "--format",
        "json"
    ]);
    let mut action = json!({
        "schema_version": "main-agent.worker-recovery-action.v1",
        "classification": classification,
        "kind": "bounded_supervision_recheck",
        "owner": main_owner,
        "assignment_id": assignment.assignment_id,
        "assignment_revision": assignment.revision,
        "capability_delivery": "owner-local-capability-required",
        "executable": true,
        "argv": supervise
    });
    match classification {
        "claim_renewal_required" => {
            action["kind"] = json!("worker_claim_renew");
            action["owner"] = worker_owner.unwrap_or_else(|| {
                json!({
                    "role": "worker",
                    "session_id": Value::Null,
                    "session_incarnation": Value::Null
                })
            });
            action["claim_id"] = claim_id.map_or(Value::Null, |value| json!(value));
            action["claim_revision"] = claim_revision.map_or(Value::Null, |value| json!(value));
            action["capability_delivery"] =
                json!("worker-owned-capability-file-from-local-environment");
            if let (Some(worker), Some(claim_id), Some(claim_revision)) =
                (assignment.worker.as_ref(), claim_id, claim_revision)
            {
                action["argv_template"] = json!([
                    "agent-session",
                    "work-context",
                    "renew",
                    "--session",
                    worker.session_id,
                    "--claim",
                    claim_id,
                    "--if-revision",
                    claim_revision.to_string(),
                    "--idempotency-key",
                    "<idempotency-key>",
                    "--format",
                    "json"
                ]);
                action["executable"] = json!(false);
                action["required_inputs"] = json!(["idempotency_key"]);
            } else {
                action["kind"] = json!("worker_claim_identity_reconcile");
                action["required_inputs"] = json!(["claim_id", "claim_revision"]);
            }
        }
        "orphan_guidance_quarantine_required" => {
            action["kind"] = json!("guidance_quarantine");
            action["argv_template"] = json!([
                "main-agent",
                "worker",
                "guidance-quarantine",
                assignment.assignment_id,
                "--if-revision",
                assignment.revision.to_string(),
                "--idempotency-key",
                "<idempotency-key>",
                "--format",
                "json"
            ]);
            action["executable"] = json!(false);
            action["required_inputs"] = json!(["idempotency_key"]);
        }
        "guidance_continuity_required" => {
            action["kind"] = json!("guidance_reconcile");
            action["argv_template"] = json!([
                "main-agent",
                "worker",
                "guidance-reconcile",
                assignment.assignment_id,
                "--if-revision",
                assignment.revision.to_string(),
                "--idempotency-key",
                "<idempotency-key>",
                "--format",
                "json"
            ]);
            action["executable"] = json!(false);
            action["required_inputs"] = json!(["idempotency_key"]);
        }
        "account_handoff_required" => {
            action["kind"] = json!("managed_account_handoff");
            action["argv_template"] = json!([
                "main-agent",
                "worker",
                "account-handoff",
                assignment.assignment_id,
                "--account",
                "<allowlisted-account>",
                "--if-revision",
                assignment.revision.to_string(),
                "--authorize-account-change",
                "--idempotency-key",
                "<idempotency-key>",
                "--format",
                "json"
            ]);
            action["executable"] = json!(false);
            action["required_inputs"] = json!(["allowlisted_account", "idempotency_key"]);
        }
        "pre_claim_failure" | "safe_reassignment" => {
            action["kind"] = json!("distinct_assignment_replacement");
            action["argv_template"] = json!([
                "main-agent",
                "worker",
                "reassign",
                assignment.assignment_id,
                "--assignment-file",
                "<private-replacement-packet>",
                "--if-revision",
                assignment.revision.to_string(),
                "--reason",
                "<bounded-reassignment-reason>",
                "--idempotency-key",
                "<idempotency-key>",
                "--format",
                "json"
            ]);
            action["executable"] = json!(false);
            action["required_inputs"] = json!([
                "private_replacement_packet",
                "reassignment_reason",
                "idempotency_key"
            ]);
        }
        "coordination_broker_stale" => {
            action["kind"] = json!("exact_worker_broker_recovery");
            action["owner"] = worker_owner.unwrap_or(Value::Null);
            action["capability_delivery"] =
                json!("worker-owned-capability-file-from-local-environment");
        }
        "account_handoff_capability_gap" => {
            action["kind"] = json!("preserve_until_explicit_lifecycle_boundary");
        }
        "uncertain_mutation" => {
            action["kind"] = json!("operation_reconciliation");
        }
        "evidence_unavailable" | "worker_unreachable" => {
            action["kind"] = json!("identity_evidence_reconciliation");
        }
        "edit_authority_stale" => {
            action["kind"] = json!("bounded_edit_authority_recheck");
        }
        "startup_dialog_failure" => {
            action["kind"] = json!("owner_decision_required");
        }
        "stale_provider_activity" => {
            action["kind"] = json!("bounded_progress_recheck");
        }
        "submitted_or_waiting_without_checkpoint" => {
            action["kind"] = json!("checkpoint_reconciliation");
        }
        "healthy_progress" => {
            action["kind"] = json!("bounded_supervision_recheck");
        }
        _ => {
            action["kind"] = json!("unknown_classification_recheck");
        }
    }
    action
}

#[derive(Clone, Copy)]
struct WorkerDiagnosisFacts {
    evidence_unavailable: bool,
    worker_unreachable: bool,
    active_or_uncertain_operation: bool,
    coordination_broker_stale: bool,
    edit_authority_stale: bool,
    claim_renewal_required: bool,
    orphan_guidance_quarantine_required: bool,
    guidance_continuity_required: bool,
    startup_dialog: bool,
    account_handoff_capability_gap: bool,
    account_handoff_required: bool,
    provider_activity_stale: bool,
    unread_guidance: bool,
    preclaim_blocker: bool,
    /// A pre-claim failure that no other fact represents: the worker runtime is
    /// durably gone and there is no turn evidence to derive termination from.
    /// Cases already carried by `preclaim_blocker`,
    /// `terminal_recovery_reconciled`, or `starting_provider_terminated` are
    /// excluded so this never displaces their classifications.
    runtime_gone_preclaim: bool,
    terminal_recovery_reconciled: bool,
    starting_provider_terminated: bool,
    terminal_quiescent: bool,
    submitted: bool,
    reassignment_safe: bool,
}

fn classify_worker_diagnosis(facts: WorkerDiagnosisFacts) -> (&'static str, &'static str, bool) {
    if facts.evidence_unavailable {
        (
            "evidence_unavailable",
            "preserve the exact worker and restore the unavailable or mismatched session, activity, packet, coordination, or worktree evidence",
            false,
        )
    } else if facts.worker_unreachable {
        (
            "worker_unreachable",
            "preserve the assignment and recover or explicitly reconcile the exact missing worker identity; automatic retry and reassignment are unsafe",
            false,
        )
    } else if facts.active_or_uncertain_operation {
        (
            "uncertain_mutation",
            "preserve the exact worker and reconcile the operation before any retry, cancellation, retirement, or reassignment",
            false,
        )
    } else if facts.coordination_broker_stale {
        (
            "coordination_broker_stale",
            "preserve the exact worker and route recovery to its authenticated, exact-incarnation broker recovery primitive; the Main Agent must not copy the worker capability, restart the provider, resend the prompt, or send raw terminal input",
            false,
        )
    } else if facts.edit_authority_stale {
        (
            "edit_authority_stale",
            "preserve the exact worker and perform a bounded supervision recheck so the broker heartbeat sidecar can refresh; if durable broker-lost evidence appears, route to exact-session authenticated broker recovery rather than renewing the work-context claim",
            false,
        )
    } else if facts.claim_renewal_required {
        (
            "claim_renewal_required",
            "request the exact worker run `agent-session work-context renew` for its current claim and revision using its own capability file before further mutation; the Main Agent must not copy or use the worker capability, restart the provider, or resend the prompt",
            false,
        )
    } else if facts.orphan_guidance_quarantine_required {
        (
            "orphan_guidance_quarantine_required",
            "the current primary controller must run `main-agent worker guidance-quarantine` with the current assignment revision to quarantine only its exact orphan stale-incarnation records; do not forward unrelated/current guidance or send raw terminal input",
            false,
        )
    } else if facts.guidance_continuity_required {
        (
            "guidance_continuity_required",
            "the current primary controller must run `main-agent worker guidance-reconcile` with the current assignment revision; do not mark stale guidance consumed or send raw terminal input",
            false,
        )
    } else if facts.account_handoff_capability_gap {
        (
            "account_handoff_capability_gap",
            "this raw worker has a terminal capability gap for the current assignment: `agent-session.codex-managed-account-handoff.v1` cannot be added by worker reassign or retry. Preserve it until the assignment reaches an explicit accept, release, cancel, or retire boundary; only a later assignment may launch a daemon-managed worker. Never use /logout, resend the prompt, or send raw terminal input",
            false,
        )
    } else if facts.account_handoff_required {
        (
            "account_handoff_required",
            "run `main-agent worker account-handoff` for the exact assignment and allowlisted account with the current revision and `--authorize-account-change`; the typed action queues, verifies, and re-arms structured control without /logout or raw terminal input",
            false,
        )
    } else if facts.startup_dialog {
        (
            "startup_dialog_failure",
            "route the trust, update, authentication, permission, or MCP decision to its owner; do not accept it automatically",
            false,
        )
    } else if facts.provider_activity_stale {
        (
            "stale_provider_activity",
            if facts.unread_guidance {
                "keep the typed guidance queued for the exact worker and wait for the provider turn boundary; do not send raw terminal input or infer material progress from provider activity alone"
            } else {
                "request a typed follow-up for the exact worker at its next turn boundary and continue bounded supervision; do not send raw terminal input"
            },
            false,
        )
    } else if facts.preclaim_blocker
        || facts.terminal_recovery_reconciled
        || facts.runtime_gone_preclaim
    {
        (
            "pre_claim_failure",
            if facts.reassignment_safe {
                "run worker reassign, or worker cancel followed by retire, using the current revision"
            } else {
                "preserve the worker until claim, operation, and clean-worktree evidence proves cancellation safe"
            },
            facts.reassignment_safe,
        )
    } else if facts.starting_provider_terminated {
        (
            "submitted_or_waiting_without_checkpoint",
            if facts.reassignment_safe {
                "run worker reassign with a distinct clean worktree and assignment"
            } else {
                "preserve the worker and resolve the missing safety evidence; never resend the prompt or inject Enter"
            },
            false,
        )
    } else if facts.terminal_quiescent {
        (
            "safe_reassignment",
            "start only a distinct assignment and clean worktree; never reuse the retired prompt",
            true,
        )
    } else {
        (
            "healthy_progress",
            if facts.submitted {
                "inspect the complete diff and validation evidence before acceptance"
            } else if facts.unread_guidance {
                "leave typed guidance queued for the exact worker to consume at its provider turn boundary; continue bounded supervision and do not send raw terminal input"
            } else {
                "continue bounded supervision; no terminal or provider input is required"
            },
            true,
        )
    }
}

fn bounded_attention_kind(value: &str) -> &'static str {
    let normalized = value.to_ascii_lowercase();
    if normalized.contains("trust") {
        "trust"
    } else if normalized.contains("update") {
        "update"
    } else if normalized.contains("auth") || normalized.contains("login") {
        "authentication"
    } else if normalized.contains("quota") || normalized.contains("credit") {
        "quota_or_credits"
    } else if normalized.contains("permission") || normalized.contains("approval") {
        "permission"
    } else if normalized.contains("mcp") {
        "mcp"
    } else {
        "other"
    }
}

fn bounded_quota_outcome(value: &str) -> bool {
    let bounded = value
        .chars()
        .take(128)
        .collect::<String>()
        .to_ascii_lowercase();
    ["quota", "credit", "usage_exhausted", "rate_limit"]
        .iter()
        .any(|needle| bounded.contains(needle))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AccountHandoffFacts {
    capability_gap: bool,
    required: bool,
}

fn account_handoff_facts(
    account: &Value,
    auto_resume: &Value,
    quota_or_credit_evidence: bool,
    authentication_required: bool,
    capability_advertised: bool,
) -> AccountHandoffFacts {
    let transition_pending = matches!(
        account["next"]["state"].as_str(),
        Some("queued" | "applying" | "failed")
    );
    let actionable = transition_pending || quota_or_credit_evidence || authentication_required;
    let controls_supported =
        capability_advertised && account["supported"] == true && auto_resume["supported"] == true;
    AccountHandoffFacts {
        capability_gap: actionable && !controls_supported,
        required: actionable && controls_supported,
    }
}

/// Evidence that decides whether an assignment failed before its worker
/// acquired the assignment-derived claim.
#[derive(Clone, Copy, Debug)]
struct PreClaimEvidence<'a> {
    assignment_state: &'a str,
    claim_active: bool,
    operations_quiescent: bool,
    worker_bound: bool,
    worker_status: &'a str,
    preclaim_blocker: bool,
    provider_terminated: bool,
    terminal_recovery_reconciled: bool,
}

/// Decide whether an assignment failed before its worker acquired the
/// assignment-derived claim. Only from this state are `worker cancel` and
/// `worker reassign` safe: no claim is held, no operation is in flight, and the
/// assignment never advanced past `starting`/`blocked` into a `working`
/// checkpoint.
fn worker_failed_preclaim(evidence: PreClaimEvidence<'_>) -> bool {
    if evidence.claim_active || !evidence.operations_quiescent {
        return false;
    }
    if !matches!(evidence.assignment_state, "starting" | "blocked") {
        return false;
    }
    let starting = evidence.assignment_state == "starting";
    evidence.preclaim_blocker
        || evidence.terminal_recovery_reconciled
        // A `starting` assignment never recorded the `working` checkpoint that
        // bootstrap writes, so any of these means the worker is gone before it
        // could hold a claim. `worker_status == "stopped"` is the case
        // `provider_terminated` cannot see: a runtime that dies during startup
        // never begins the turn that activity evidence is derived from.
        || (starting
            && (evidence.provider_terminated
                || !evidence.worker_bound
                || evidence.worker_status == "stopped"))
}

fn worker_claim_renewal_required(
    assignment_state: &str,
    claim_active: bool,
    broker_authoritative: bool,
    claim_expires_in_seconds: Option<i64>,
) -> bool {
    matches!(assignment_state, "starting" | "working" | "blocked")
        && (!claim_active
            || (!broker_authoritative
                && claim_expires_in_seconds
                    .is_some_and(|remaining| remaining <= WORKER_CLAIM_RENEWAL_RISK_SECS)))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RawRateLimitDiagnostic {
    NotRequested,
    Exhausted,
    Available,
    Unavailable(&'static str),
}

impl RawRateLimitDiagnostic {
    fn is_exhausted(self) -> bool {
        self == Self::Exhausted
    }

    fn is_unavailable(self) -> bool {
        matches!(self, Self::Unavailable(_))
    }

    fn projection(self) -> Value {
        match self {
            Self::NotRequested => json!({
                "state": "not_requested",
                "reason_code": "staleness-prerequisites-not-met"
            }),
            Self::Exhausted => json!({
                "state": "exhausted",
                "reason_code": "selected-account-rate-limit-exhausted"
            }),
            Self::Available => json!({
                "state": "available",
                "reason_code": "selected-account-capacity-available"
            }),
            Self::Unavailable(reason_code) => json!({
                "state": "unavailable",
                "reason_code": reason_code
            }),
        }
    }
}

#[derive(Debug, Deserialize)]
struct RawRateLimitEnvelope {
    schema_version: String,
    command: String,
    mode: String,
    ok: bool,
    result: RawRateLimitResult,
}

#[derive(Debug, Deserialize)]
struct RawRateLimitResult {
    provider: String,
    name: String,
    target_file: String,
    status: String,
    ok: bool,
    source: String,
    windows: Vec<RawRateLimitWindow>,
}

#[derive(Debug, Deserialize)]
struct RawRateLimitWindow {
    used_percent: i64,
    remaining_percent: i64,
}

fn diagnose_selected_raw_account_rate_limits(account: &str) -> RawRateLimitDiagnostic {
    let Some(binary) = resolve_codex_cli_executable() else {
        return RawRateLimitDiagnostic::Unavailable("diagnostic-binary-unavailable");
    };
    diagnose_selected_raw_account_rate_limits_with(
        &binary,
        account,
        RAW_RATE_LIMIT_DIAGNOSTIC_TIMEOUT,
    )
}

fn raw_rate_limit_diagnostic_required(
    provider_activity_stale: bool,
    structured_quota_or_credit_evidence: bool,
    controls_supported: bool,
    agent: Option<&str>,
) -> bool {
    provider_activity_stale
        && !structured_quota_or_credit_evidence
        && !controls_supported
        && agent == Some("codex")
}

fn selected_raw_account_provenance(account_view: &Value) -> Option<&str> {
    account_view["selected_account"]
        .as_str()
        .filter(|account| !account.is_empty())
}

fn resolve_codex_cli_executable() -> Option<PathBuf> {
    let current = env::current_exe().ok()?;
    let current_dir = current.parent()?;
    let profile_dir = if current_dir.file_name() == Some(std::ffi::OsStr::new("deps")) {
        current_dir.parent()?
    } else {
        current_dir
    };
    let candidate = profile_dir.join(format!("codex-cli{}", env::consts::EXE_SUFFIX));
    let metadata = fs::metadata(&candidate).ok()?;
    (metadata.is_file() && metadata.permissions().mode() & 0o111 != 0).then_some(candidate)
}

fn diagnose_selected_raw_account_rate_limits_with(
    binary: &Path,
    account: &str,
    timeout: Duration,
) -> RawRateLimitDiagnostic {
    match diagnose_selected_raw_account_rate_limits_with_io(binary, account, timeout) {
        Ok(diagnostic) => diagnostic,
        Err(error) if error.kind() == io::ErrorKind::TimedOut => {
            RawRateLimitDiagnostic::Unavailable("diagnostic-timeout")
        }
        Err(_) => RawRateLimitDiagnostic::Unavailable("diagnostic-execution-failed"),
    }
}

fn diagnose_selected_raw_account_rate_limits_with_io(
    binary: &Path,
    account: &str,
    timeout: Duration,
) -> io::Result<RawRateLimitDiagnostic> {
    if account.is_empty()
        || account.len() > RAW_RATE_LIMIT_ACCOUNT_MAX_BYTES
        || !account
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        || account.contains("..")
        || account.ends_with(".json")
    {
        return Ok(RawRateLimitDiagnostic::Unavailable(
            "selected-raw-account-invalid",
        ));
    }
    let target_file = format!("{account}.json");
    let mut command = Command::new(binary);
    command.args([
        "diag",
        "rate-limits",
        "--format",
        "json",
        "--no-refresh-auth",
        &target_file,
    ]);
    let output =
        run_output_with_timeout_and_cap(command, timeout, RAW_RATE_LIMIT_DIAGNOSTIC_MAX_BYTES + 1)?;
    if output.stdout.len() > RAW_RATE_LIMIT_DIAGNOSTIC_MAX_BYTES {
        return Ok(RawRateLimitDiagnostic::Unavailable(
            "diagnostic-output-oversized",
        ));
    }
    let envelope: RawRateLimitEnvelope = match serde_json::from_slice(&output.stdout) {
        Ok(envelope) => envelope,
        Err(_) => {
            return Ok(RawRateLimitDiagnostic::Unavailable(
                "diagnostic-response-invalid",
            ));
        }
    };
    if envelope.schema_version != "codex-cli.diag.rate-limits.v1"
        || envelope.command != "diag rate-limits"
        || envelope.mode != "single"
        || !output.status.success()
        || !envelope.ok
        || !envelope.result.ok
        || envelope.result.provider != "codex"
        || envelope.result.status != "ok"
        || envelope.result.source != "network"
    {
        return Ok(RawRateLimitDiagnostic::Unavailable(
            "diagnostic-evidence-inconclusive",
        ));
    }
    if envelope.result.name != account || envelope.result.target_file != target_file {
        return Ok(RawRateLimitDiagnostic::Unavailable(
            "diagnostic-account-mismatch",
        ));
    }
    if envelope.result.windows.iter().any(|window| {
        !(0..=100).contains(&window.used_percent)
            || !(0..=100).contains(&window.remaining_percent)
            || window.used_percent.saturating_add(window.remaining_percent) != 100
    }) {
        return Ok(RawRateLimitDiagnostic::Unavailable(
            "diagnostic-response-invalid",
        ));
    }
    if envelope
        .result
        .windows
        .iter()
        .any(|window| window.used_percent == 100 && window.remaining_percent == 0)
    {
        Ok(RawRateLimitDiagnostic::Exhausted)
    } else {
        Ok(RawRateLimitDiagnostic::Available)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct WorktreeProgressSnapshot {
    schema_version: String,
    assignment_id: String,
    worker_incarnation: String,
    material_fingerprint: String,
    observed_at_epoch: i64,
    changed_at_epoch: i64,
}

fn inspect_worktree_progress(
    context: &CliContext,
    assignment: &AssignmentRecord,
    path: &Path,
) -> Result<Value, CliError> {
    if !path.is_dir() {
        return Ok(json!({ "available": false, "clean": false, "change_count": Value::Null }));
    }
    let mut command = Command::new("git");
    command
        .current_dir(path)
        .args(["status", "--porcelain=v1", "-z", "--untracked-files=all"]);
    let output = run_output_with_timeout_and_cap(
        command,
        WORKTREE_STATUS_TIMEOUT,
        WORKTREE_STATUS_MAX_OUTPUT_BYTES,
    );
    match output {
        Ok(output) if output.status.success() => {
            let change_count = output
                .stdout
                .split(|byte| *byte == 0)
                .filter(|entry| !entry.is_empty())
                .count();
            let Some(material_fingerprint) = worktree_material_fingerprint(path, &output.stdout)
            else {
                return Ok(json!({
                    "available": false,
                    "clean": false,
                    "change_count": Value::Null,
                    "reason_code": "material-fingerprint-unavailable"
                }));
            };
            let now = crate::coordination::now_epoch();
            let session_absent = assignment
                .worker
                .as_ref()
                .is_some_and(|worker| !session_dir(context, &worker.session_id).is_dir());
            let (changed_at_epoch, observed_at_epoch) = if session_absent {
                (now, now)
            } else {
                match persist_worktree_progress_snapshot(
                    context,
                    assignment,
                    &material_fingerprint,
                    now,
                ) {
                    Ok(snapshot) => snapshot,
                    Err(error) if error.code() == "session-not-found" => (now, now),
                    Err(error) => return Err(error),
                }
            };
            Ok(json!({
                "available": true,
                "clean": change_count == 0,
                "change_count": change_count,
                "status_digest": &material_fingerprint,
                "material_fingerprint": material_fingerprint,
                "changed_at_epoch": changed_at_epoch,
                "observed_at_epoch": observed_at_epoch,
                "snapshot_age_seconds": now.saturating_sub(changed_at_epoch)
            }))
        }
        _ => Ok(json!({ "available": false, "clean": false, "change_count": Value::Null })),
    }
}

fn worktree_is_clean(path: &Path) -> bool {
    if !path.is_dir() {
        return false;
    }
    let mut command = Command::new("git");
    command
        .current_dir(path)
        .args(["status", "--porcelain=v1", "--untracked-files=normal"]);
    run_output_with_timeout_and_cap(
        command,
        WORKTREE_STATUS_TIMEOUT,
        WORKTREE_STATUS_MAX_OUTPUT_BYTES,
    )
    .is_ok_and(|output| output.status.success() && output.stdout.is_empty())
}

fn worktree_material_fingerprint(path: &Path, status: &[u8]) -> Option<String> {
    worktree_material_fingerprint_with_git(path, status, Path::new("git"), WORKTREE_STATUS_TIMEOUT)
}

fn worktree_material_fingerprint_with_git(
    path: &Path,
    status: &[u8],
    git_binary: &Path,
    timeout: Duration,
) -> Option<String> {
    if status.is_empty() {
        return Some(clean_worktree_material_fingerprint());
    }
    let started = Instant::now();
    let mut material = Vec::new();
    append_hashed_fingerprint_component(&mut material, b"status", status)?;
    for (label, args) in [
        (
            b"tracked-worktree".as_slice(),
            [
                "diff",
                "--no-ext-diff",
                "--no-textconv",
                "--binary",
                "--full-index",
                "--",
            ]
            .as_slice(),
        ),
        (
            b"tracked-index".as_slice(),
            [
                "diff",
                "--cached",
                "--no-ext-diff",
                "--no-textconv",
                "--binary",
                "--full-index",
                "--",
            ]
            .as_slice(),
        ),
    ] {
        let mut command = Command::new(git_binary);
        command.current_dir(path).args(args);
        let remaining = timeout.checked_sub(started.elapsed())?;
        let (length, digest) = stream_command_digest(command, remaining)?;
        append_digest_fingerprint_component(&mut material, label, length, &digest)?;
    }

    let mut list = Command::new(git_binary);
    list.current_dir(path)
        .args(["ls-files", "--others", "--exclude-standard", "-z"]);
    let remaining = timeout.checked_sub(started.elapsed())?;
    let untracked =
        run_fingerprint_command_output(list, remaining, WORKTREE_STATUS_MAX_OUTPUT_BYTES as u64)?;
    if started.elapsed() >= timeout {
        return None;
    }
    let paths = untracked
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
        .map(<[u8]>::to_vec)
        .collect::<Vec<_>>();
    if paths.len() > WORKTREE_FINGERPRINT_MAX_FILES {
        return None;
    }
    let status_untracked = status
        .split(|byte| *byte == 0)
        .filter_map(|entry| entry.strip_prefix(b"?? "))
        .map(<[u8]>::to_vec)
        .collect::<Vec<_>>();
    if status_untracked != paths {
        return None;
    }
    let repository = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
        .ok()?;
    let mut untracked_logical_size = material.len();
    for relative in &paths {
        if started.elapsed() >= timeout {
            return None;
        }
        untracked_logical_size = fingerprint_component_projected_len(
            untracked_logical_size,
            b"untracked-path",
            relative.len(),
        )?;
        append_fingerprint_component(&mut material, b"untracked-path", relative)?;
    }
    let remaining = timeout.checked_sub(started.elapsed())?;
    let digests = stream_untracked_file_digests(repository, paths, remaining)?;
    if started.elapsed() >= timeout {
        return None;
    }
    for digest in digests {
        untracked_logical_size = fingerprint_component_projected_len(
            untracked_logical_size,
            b"untracked-content",
            usize::try_from(digest.length).ok()?,
        )?;
        append_digest_fingerprint_component(
            &mut material,
            b"untracked-content",
            digest.length,
            &digest.digest,
        )?;
    }
    Some(format!(
        "sha256:{}",
        crate::coordination::digest_bytes(&material)
    ))
}

fn clean_worktree_material_fingerprint() -> String {
    static FINGERPRINT: OnceLock<String> = OnceLock::new();
    FINGERPRINT
        .get_or_init(|| {
            let mut material = Vec::new();
            append_hashed_fingerprint_component(&mut material, b"status", &[])
                .expect("clean status fingerprint is bounded");
            let empty_digest = Sha256::digest([]);
            for label in [b"tracked-worktree".as_slice(), b"tracked-index".as_slice()] {
                append_digest_fingerprint_component(&mut material, label, 0, &empty_digest)
                    .expect("clean tracked fingerprint is bounded");
            }
            format!("sha256:{}", crate::coordination::digest_bytes(&material))
        })
        .clone()
}

fn open_untracked_regular_file(repository: &fs::File, relative: &[u8]) -> Option<fs::File> {
    if relative.is_empty() || relative.starts_with(b"/") {
        return None;
    }
    let components = relative.split(|byte| *byte == b'/').collect::<Vec<_>>();
    if components
        .iter()
        .any(|component| component.is_empty() || matches!(*component, b"." | b".."))
    {
        return None;
    }
    let mut directory = repository.try_clone().ok()?;
    for component in &components[..components.len().saturating_sub(1)] {
        let component = CString::new(*component).ok()?;
        let descriptor = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                component.as_ptr(),
                libc::O_RDONLY
                    | libc::O_DIRECTORY
                    | libc::O_NOFOLLOW
                    | libc::O_CLOEXEC
                    | libc::O_NONBLOCK,
            )
        };
        if descriptor < 0 {
            return None;
        }
        directory = unsafe { fs::File::from_raw_fd(descriptor) };
    }
    let name = CString::new(*components.last()?).ok()?;
    let descriptor = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
        )
    };
    if descriptor < 0 {
        return None;
    }
    let file = unsafe { fs::File::from_raw_fd(descriptor) };
    file.metadata().ok()?.is_file().then_some(file)
}

fn same_file_snapshot(before: &fs::Metadata, after: &fs::Metadata) -> bool {
    before.dev() == after.dev()
        && before.ino() == after.ino()
        && before.len() == after.len()
        && before.mtime() == after.mtime()
        && before.mtime_nsec() == after.mtime_nsec()
        && before.ctime() == after.ctime()
        && before.ctime_nsec() == after.ctime_nsec()
}

fn stream_command_digest(mut command: Command, timeout: Duration) -> Option<(u64, [u8; 32])> {
    command.stdin(Stdio::null());
    stream_prepared_command_digest(command, timeout, None)
}

fn stream_file_digest(
    file: &mut fs::File,
    max_bytes: u64,
    timeout: Duration,
) -> Option<(u64, [u8; 32])> {
    #[cfg(test)]
    {
        let stall = FINGERPRINT_FILE_READ_STALL_MILLIS_FOR_TEST.swap(0, Ordering::AcqRel);
        if stall > 0 {
            thread::sleep(Duration::from_millis(stall));
        }
    }
    let started = Instant::now();
    let mut length = 0_u64;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        if started.elapsed() >= timeout {
            return None;
        }
        let read = match file.read(&mut buffer) {
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return None,
        };
        if started.elapsed() >= timeout {
            return None;
        }
        if read == 0 {
            return Some((length, digest.finalize().into()));
        }
        length = length.checked_add(read as u64)?;
        if length > max_bytes {
            return None;
        }
        digest.update(&buffer[..read]);
    }
}

#[derive(Debug)]
struct UntrackedFileDigest {
    length: u64,
    digest: [u8; 32],
}

const WORKTREE_FINGERPRINT_MAX_FILE_READERS: usize = 4;
static ACTIVE_FINGERPRINT_FILE_READERS: AtomicUsize = AtomicUsize::new(0);

struct FingerprintFileReaderPermit;

impl FingerprintFileReaderPermit {
    fn acquire() -> Option<Self> {
        let mut active = ACTIVE_FINGERPRINT_FILE_READERS.load(Ordering::Acquire);
        loop {
            if active >= WORKTREE_FINGERPRINT_MAX_FILE_READERS {
                return None;
            }
            match ACTIVE_FINGERPRINT_FILE_READERS.compare_exchange_weak(
                active,
                active + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Some(Self),
                Err(observed) => active = observed,
            }
        }
    }
}

impl Drop for FingerprintFileReaderPermit {
    fn drop(&mut self) {
        ACTIVE_FINGERPRINT_FILE_READERS.fetch_sub(1, Ordering::AcqRel);
    }
}

fn stream_untracked_file_digests(
    repository: fs::File,
    paths: Vec<Vec<u8>>,
    timeout: Duration,
) -> Option<Vec<UntrackedFileDigest>> {
    let permit = FingerprintFileReaderPermit::acquire()?;
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    thread::Builder::new()
        .name("worktree-fingerprint-file-reader".to_string())
        .spawn(move || {
            let _permit = permit;
            let result = stream_untracked_file_digests_inner(&repository, &paths, timeout);
            let _ = sender.send(result);
        })
        .ok()?;
    receiver.recv_timeout(timeout).ok()?
}

fn stream_untracked_file_digests_inner(
    repository: &fs::File,
    paths: &[Vec<u8>],
    timeout: Duration,
) -> Option<Vec<UntrackedFileDigest>> {
    let started = Instant::now();
    let mut total_bytes = 0_u64;
    let mut digests = Vec::with_capacity(paths.len());
    for relative in paths {
        let mut file = open_untracked_regular_file(repository, relative)?;
        let before = file.metadata().ok()?;
        total_bytes = total_bytes.checked_add(before.len())?;
        if total_bytes > WORKTREE_FINGERPRINT_MAX_BYTES as u64 {
            return None;
        }
        let remaining = timeout.checked_sub(started.elapsed())?;
        let (length, digest) = stream_file_digest(&mut file, before.len(), remaining)?;
        let after = file.metadata().ok()?;
        if !same_file_snapshot(&before, &after) || length != after.len() {
            return None;
        }
        digests.push(UntrackedFileDigest { length, digest });
    }
    Some(digests)
}

fn stream_prepared_command_digest(
    mut command: Command,
    timeout: Duration,
    max_output_bytes: Option<u64>,
) -> Option<(u64, [u8; 32])> {
    let reaper = fingerprint_reaper_queue()?;
    let permit = FingerprintProcessPermit::acquire()?;
    command
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0);
    #[cfg(test)]
    record_fingerprint_subprocess_launch_for_test();
    let mut child = command.spawn().ok()?;
    let mut stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            terminate_fingerprint_process_group(child, permit, reaper);
            return None;
        }
    };
    let flags = unsafe { libc::fcntl(stdout.as_raw_fd(), libc::F_GETFL) };
    if flags < 0
        || unsafe { libc::fcntl(stdout.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
    {
        terminate_fingerprint_process_group(child, permit, reaper);
        return None;
    }
    let started = Instant::now();
    let mut length = 0_u64;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 16 * 1024];
    let mut eof = false;
    loop {
        while !eof && started.elapsed() < timeout {
            match stdout.read(&mut buffer) {
                Ok(0) => eof = true,
                Ok(read) => {
                    let Some(next_length) = length.checked_add(read as u64) else {
                        terminate_fingerprint_process_group(child, permit, reaper);
                        return None;
                    };
                    if max_output_bytes.is_some_and(|maximum| next_length > maximum) {
                        terminate_fingerprint_process_group(child, permit, reaper);
                        return None;
                    }
                    length = next_length;
                    digest.update(&buffer[..read]);
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(_) => {
                    terminate_fingerprint_process_group(child, permit, reaper);
                    return None;
                }
            }
        }
        if eof {
            match child.try_wait() {
                Ok(Some(status)) => {
                    return status.success().then(|| (length, digest.finalize().into()));
                }
                Ok(None) => {}
                Err(_) => {
                    terminate_fingerprint_process_group(child, permit, reaper);
                    return None;
                }
            }
        }
        if started.elapsed() >= timeout {
            // The caller's evidence deadline is independent of process
            // cleanup. Normal kills are reaped during a tiny bounded grace;
            // an uninterruptible reader is transferred to the admission-
            // controlled reaper without delaying supervision.
            terminate_fingerprint_process_group(child, permit, reaper);
            return None;
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn run_fingerprint_command_output(
    mut command: Command,
    timeout: Duration,
    max_output_bytes: u64,
) -> Option<Vec<u8>> {
    let reaper = fingerprint_reaper_queue()?;
    let permit = FingerprintProcessPermit::acquire()?;
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0);
    #[cfg(test)]
    record_fingerprint_subprocess_launch_for_test();
    let mut child = command.spawn().ok()?;
    let mut stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            terminate_fingerprint_process_group(child, permit, reaper);
            return None;
        }
    };
    let flags = unsafe { libc::fcntl(stdout.as_raw_fd(), libc::F_GETFL) };
    if flags < 0
        || unsafe { libc::fcntl(stdout.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
    {
        terminate_fingerprint_process_group(child, permit, reaper);
        return None;
    }
    let started = Instant::now();
    let mut output = Vec::new();
    let mut buffer = [0_u8; 16 * 1024];
    let mut eof = false;
    loop {
        while !eof && started.elapsed() < timeout {
            match stdout.read(&mut buffer) {
                Ok(0) => eof = true,
                Ok(read) => {
                    let Some(next_length) = (output.len() as u64).checked_add(read as u64) else {
                        terminate_fingerprint_process_group(child, permit, reaper);
                        return None;
                    };
                    if next_length > max_output_bytes {
                        terminate_fingerprint_process_group(child, permit, reaper);
                        return None;
                    }
                    output.extend_from_slice(&buffer[..read]);
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(_) => {
                    terminate_fingerprint_process_group(child, permit, reaper);
                    return None;
                }
            }
        }
        if eof {
            match child.try_wait() {
                Ok(Some(status)) => return status.success().then_some(output),
                Ok(None) => {}
                Err(_) => {
                    terminate_fingerprint_process_group(child, permit, reaper);
                    return None;
                }
            }
        }
        if started.elapsed() >= timeout {
            terminate_fingerprint_process_group(child, permit, reaper);
            return None;
        }
        thread::sleep(Duration::from_millis(10));
    }
}

static ACTIVE_FINGERPRINT_PROCESSES: AtomicUsize = AtomicUsize::new(0);
#[cfg(test)]
static FINGERPRINT_FILE_READ_STALL_MILLIS_FOR_TEST: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
#[cfg(test)]
fn stall_next_fingerprint_file_read_for_test(duration: Duration) {
    FINGERPRINT_FILE_READ_STALL_MILLIS_FOR_TEST.store(
        duration.as_millis().try_into().unwrap_or(u64::MAX),
        Ordering::Release,
    );
}
#[cfg(test)]
thread_local! {
    static FINGERPRINT_SUBPROCESS_LAUNCHES: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn record_fingerprint_subprocess_launch_for_test() {
    FINGERPRINT_SUBPROCESS_LAUNCHES.with(|launches| {
        launches.set(launches.get().saturating_add(1));
    });
}

#[cfg(test)]
fn reset_fingerprint_subprocess_launches_for_test() {
    FINGERPRINT_SUBPROCESS_LAUNCHES.with(|launches| launches.set(0));
}

#[cfg(test)]
fn fingerprint_subprocess_launches_for_test() -> usize {
    FINGERPRINT_SUBPROCESS_LAUNCHES.with(std::cell::Cell::get)
}

struct FingerprintProcessPermit {
    active: &'static AtomicUsize,
}

impl FingerprintProcessPermit {
    fn acquire() -> Option<Self> {
        Self::acquire_from(
            &ACTIVE_FINGERPRINT_PROCESSES,
            WORKTREE_FINGERPRINT_MAX_REAPERS,
        )
    }

    fn acquire_from(active_processes: &'static AtomicUsize, limit: usize) -> Option<Self> {
        let mut active = active_processes.load(Ordering::Acquire);
        loop {
            if active >= limit {
                return None;
            }
            match active_processes.compare_exchange_weak(
                active,
                active + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    return Some(Self {
                        active: active_processes,
                    });
                }
                Err(observed) => active = observed,
            }
        }
    }
}

impl Drop for FingerprintProcessPermit {
    fn drop(&mut self) {
        self.active.fetch_sub(1, Ordering::AcqRel);
    }
}

struct FingerprintReapTask {
    child: std::process::Child,
    _permit: FingerprintProcessPermit,
    not_before: Option<Instant>,
}

fn fingerprint_reaper_queue() -> Option<&'static Arc<Mutex<Vec<FingerprintReapTask>>>> {
    static REAPER: OnceLock<Option<Arc<Mutex<Vec<FingerprintReapTask>>>>> = OnceLock::new();
    REAPER
        .get_or_init(|| {
            let queue = Arc::new(Mutex::new(Vec::<FingerprintReapTask>::new()));
            let worker_queue = Arc::clone(&queue);
            thread::Builder::new()
                .name("worktree-fingerprint-reaper".to_string())
                .spawn(move || {
                    loop {
                        reap_fingerprint_tasks_once(&worker_queue);
                        thread::sleep(Duration::from_millis(10));
                    }
                })
                .ok()
                .map(|_| queue)
        })
        .as_ref()
}

fn reap_fingerprint_tasks_once(reaper: &Arc<Mutex<Vec<FingerprintReapTask>>>) {
    let mut tasks = reaper
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut index = 0;
    while index < tasks.len() {
        if tasks[index]
            .not_before
            .is_some_and(|not_before| Instant::now() < not_before)
        {
            index += 1;
            continue;
        }
        match tasks[index].child.try_wait() {
            Ok(Some(_)) | Err(_) => {
                tasks.swap_remove(index);
            }
            Ok(None) => index += 1,
        }
    }
}

fn enqueue_fingerprint_reap_task(
    reaper: &Arc<Mutex<Vec<FingerprintReapTask>>>,
    child: std::process::Child,
    permit: FingerprintProcessPermit,
) {
    enqueue_fingerprint_reap_task_after(reaper, child, permit, None);
}

fn enqueue_fingerprint_reap_task_after(
    reaper: &Arc<Mutex<Vec<FingerprintReapTask>>>,
    child: std::process::Child,
    permit: FingerprintProcessPermit,
    not_before: Option<Instant>,
) {
    let mut tasks = reaper
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    debug_assert!(
        tasks.len() < WORKTREE_FINGERPRINT_MAX_REAPERS,
        "one permit is reserved for every queued fingerprint child"
    );
    tasks.push(FingerprintReapTask {
        child,
        _permit: permit,
        not_before,
    });
}

#[cfg(test)]
thread_local! {
    static FINGERPRINT_REAP_DELAY_FOR_TEST: std::cell::Cell<Option<Duration>> =
        const { std::cell::Cell::new(None) };
}

#[cfg(test)]
fn stall_next_fingerprint_reap_for_test(duration: Duration) {
    FINGERPRINT_REAP_DELAY_FOR_TEST.with(|delay| delay.set(Some(duration)));
}

fn terminate_fingerprint_process_group(
    mut child: std::process::Child,
    permit: FingerprintProcessPermit,
    reaper: &Arc<Mutex<Vec<FingerprintReapTask>>>,
) {
    let pid = child.id() as libc::pid_t;
    // SAFETY: fingerprint commands are launched as leaders of dedicated
    // process groups, so the negative PID targets only this probe.
    unsafe {
        libc::kill(-pid, libc::SIGKILL);
    }
    let _ = child.kill();
    #[cfg(test)]
    if let Some(delay) = FINGERPRINT_REAP_DELAY_FOR_TEST.with(|slot| slot.replace(None)) {
        enqueue_fingerprint_reap_task_after(reaper, child, permit, Some(Instant::now() + delay));
        return;
    }
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) | Err(_) => return,
            Ok(None) if started.elapsed() < WORKTREE_FINGERPRINT_REAP_GRACE => {
                thread::sleep(Duration::from_millis(1));
            }
            Ok(None) => {
                enqueue_fingerprint_reap_task(reaper, child, permit);
                return;
            }
        }
    }
}

fn append_hashed_fingerprint_component(
    target: &mut Vec<u8>,
    label: &[u8],
    value: &[u8],
) -> Option<()> {
    append_digest_fingerprint_component(target, label, value.len() as u64, &Sha256::digest(value))
}

fn append_digest_fingerprint_component(
    target: &mut Vec<u8>,
    label: &[u8],
    value_len: u64,
    value_digest: &[u8],
) -> Option<()> {
    let mut descriptor = Vec::with_capacity(8 + value_digest.len());
    descriptor.extend_from_slice(&value_len.to_be_bytes());
    descriptor.extend_from_slice(value_digest);
    append_fingerprint_component(target, label, &descriptor)
}

fn append_fingerprint_component(target: &mut Vec<u8>, label: &[u8], value: &[u8]) -> Option<()> {
    fingerprint_component_projected_len(target.len(), label, value.len())?;
    target.extend_from_slice(&(label.len() as u64).to_be_bytes());
    target.extend_from_slice(label);
    target.extend_from_slice(&(value.len() as u64).to_be_bytes());
    target.extend_from_slice(value);
    Some(())
}

fn fingerprint_component_projected_len(
    current: usize,
    label: &[u8],
    value_len: usize,
) -> Option<usize> {
    let projected = current
        .checked_add(16)?
        .checked_add(label.len())?
        .checked_add(value_len)?;
    if projected > WORKTREE_FINGERPRINT_MAX_BYTES {
        return None;
    }
    Some(projected)
}

fn persist_worktree_progress_snapshot(
    context: &CliContext,
    assignment: &AssignmentRecord,
    material_fingerprint: &str,
    now: i64,
) -> Result<(i64, i64), CliError> {
    let Some(worker) = assignment.worker.as_ref() else {
        return Ok((now, now));
    };
    let _record_lock = acquire_session_record_lock(context, &worker.session_id)?;
    let current = load_session_record(context, &worker.session_id)?;
    let current_incarnation = current
        .runtime
        .as_ref()
        .map(|runtime| runtime.launch_id.as_str())
        .unwrap_or_default();
    if !orchestration::session_ref_matches(worker, &current, current_incarnation) {
        return Err(CliError::data(
            "worker-session-incarnation-conflict",
            "worker identity changed while persisting progress evidence",
            None,
        ));
    }
    let directory = session_dir(context, &worker.session_id).join("coordination");
    fs::create_dir_all(&directory).map_err(|_| {
        CliError::runtime(
            "worktree-progress-store-unavailable",
            "worktree progress evidence directory is unavailable",
            None,
        )
    })?;
    let name = crate::coordination::digest_bytes(assignment.assignment_id.as_bytes());
    let snapshot_path = directory.join(format!("main-agent-progress-{name}.json"));
    let previous = match fs::symlink_metadata(&snapshot_path) {
        Ok(_) => Some(crate::coordination::read_bounded_json::<
            WorktreeProgressSnapshot,
        >(
            &snapshot_path,
            WORKTREE_PROGRESS_SNAPSHOT_MAX_BYTES,
            "worktree-progress-store-invalid",
        )?),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(_) => {
            return Err(CliError::runtime(
                "worktree-progress-store-unavailable",
                "worktree progress evidence is unavailable",
                None,
            ));
        }
    };
    if let Some(previous) = previous.as_ref()
        && (previous.schema_version != WORKTREE_PROGRESS_SNAPSHOT_SCHEMA
            || previous.assignment_id != assignment.assignment_id
            || !previous.material_fingerprint.starts_with("sha256:")
            || previous.observed_at_epoch < previous.changed_at_epoch)
    {
        return Err(CliError::data(
            "worktree-progress-store-invalid",
            "worktree progress evidence is invalid",
            None,
        ));
    }
    if let Some(previous) = previous.as_ref().filter(|previous| {
        previous.worker_incarnation == worker.session_incarnation
            && previous.material_fingerprint == material_fingerprint
    }) {
        return Ok((previous.changed_at_epoch, now));
    }
    let changed_at_epoch = now;
    let snapshot = WorktreeProgressSnapshot {
        schema_version: WORKTREE_PROGRESS_SNAPSHOT_SCHEMA.to_string(),
        assignment_id: assignment.assignment_id.clone(),
        worker_incarnation: worker.session_incarnation.clone(),
        material_fingerprint: material_fingerprint.to_string(),
        observed_at_epoch: now,
        changed_at_epoch,
    };
    let bytes = serde_json::to_vec_pretty(&snapshot).map_err(|_| {
        CliError::runtime(
            "worktree-progress-store-invalid",
            "worktree progress evidence could not be serialized",
            None,
        )
    })?;
    write_atomic(&snapshot_path, &bytes, SECRET_FILE_MODE).map_err(|_| {
        CliError::runtime(
            "worktree-progress-store-unavailable",
            "worktree progress evidence could not be persisted",
            None,
        )
    })?;
    Ok((changed_at_epoch, now))
}

fn run_worker_cancel(context: &CliContext, args: WorkerCancelArgs) -> Result<Value, CliError> {
    validate_idempotency_key(&args.idempotency_key)?;
    orchestration::validate_summary("cancellation reason", &args.reason)?;
    let (main, main_incarnation) = authenticated_self(context)?;
    ensure_active_claim(context, &main)?;
    let request_digest = crate::coordination::request_digest(
        "worker-cancel",
        &json!({
            "assignment_id": args.assignment_id,
            "if_revision": args.if_revision,
            "reason": args.reason
        }),
    );
    let registry = orchestration::load_registry_readonly(context)?;
    if let Some(value) = idempotency_replay(
        &registry,
        &main,
        &main_incarnation,
        &args.idempotency_key,
        "worker-cancel",
        &request_digest,
    )? {
        return Ok(value);
    }
    let diagnosis = diagnose_worker(context, &args.assignment_id)?;
    if diagnosis["failed_preclaim"] != true {
        return Err(CliError::data(
            "assignment-not-preclaim-failed",
            "worker cancel requires a proven failed pre-claim assignment",
            Some(json!({
                "assignment_id": args.assignment_id,
                "classification": diagnosis["classification"],
                "last_proven_safe_state": diagnosis
            })),
        ));
    }
    let registry = orchestration::load_registry_readonly(context)?;
    let run = require_current_main(&registry, &main, &main_incarnation)?;
    let assignment = registry
        .assignments
        .get(&args.assignment_id)
        .filter(|assignment| assignment.run_id == run.run_id)
        .ok_or_else(|| not_found("assignment-not-found", "assignment was not found"))?
        .clone();
    ensure_primary_manager(&assignment, &main, &main_incarnation)?;
    ensure_revision(args.if_revision, assignment.revision, "assignment")?;
    pause_cancel_after_admission_for_test(&assignment)?;
    let terminal_recovery_reconciled = has_terminal_reconciled_recovery(&assignment, run)
        && diagnosis["worker"]["status"] == "stopped";
    // A worker whose runtime died before bootstrap never opened a broker, so
    // demanding authoritative broker evidence would leave it permanently
    // uncancellable. It earns the same waiver as a reconciled recovery by
    // proving the same stopped runtime below; the quiescence checks after that
    // still fence any claim or active/uncertain operation.
    let preclaim_runtime_gone = !terminal_recovery_reconciled
        && assignment.state == "starting"
        && assignment.worker.is_some()
        && diagnosis["worker"]["status"] == "stopped";
    let broker_evidence_waived = terminal_recovery_reconciled || preclaim_runtime_gone;
    // Reconciliation committed this same stopped-runtime proof under the
    // record -> coordination -> orchestration lock order. Reacquire the
    // lifecycle boundary before relying on a stopped or absent broker so the
    // exact incarnation cannot be replaced while cancellation commits.
    let _worker_lifecycle = if broker_evidence_waived {
        let worker = assignment.worker.as_ref().ok_or_else(|| {
            CliError::data(
                "worker-incarnation-changed",
                "worker identity is unavailable at the cancellation boundary",
                None,
            )
        })?;
        let lifecycle = acquire_session_record_lock(context, &worker.session_id)?;
        let worker_record = load_session_record(context, &worker.session_id)?;
        let worker_incarnation = worker_record
            .runtime
            .as_ref()
            .map(|runtime| runtime.launch_id.as_str())
            .unwrap_or_default();
        if !orchestration::session_ref_matches(worker, &worker_record, worker_incarnation) {
            return Err(CliError::data(
                "worker-incarnation-changed",
                "reconciled worker identity changed before cancellation",
                None,
            ));
        }
        let runtime_evidence = crate::coordination_runtime_evidence(&worker_record)?;
        if runtime_evidence.status != crate::CoordinationRuntimeStatus::Stopped
            || session_status(&resolve_tmux_bin(None), &worker_record) != "stopped"
        {
            return Err(CliError::data(
                "worker-runtime-still-live",
                "cancellation on waived broker evidence requires the exact worker runtime to remain stopped",
                Some(json!({ "assignment_id": args.assignment_id })),
            ));
        }
        Some(lifecycle)
    } else {
        None
    };
    let worker_bound = assignment.worker.is_some();
    let quiescence = if let Some(worker) = &assignment.worker {
        crate::coordination::lock_session_quiescence(
            context,
            &worker.session_id,
            &worker.session_incarnation,
        )?
    } else {
        crate::coordination::lock_session_quiescence(context, &main.id, &main_incarnation)?
    };
    if worker_bound && !quiescence.broker_present && !broker_evidence_waived {
        return Err(CliError::runtime(
            "coordination-broker-unavailable",
            "worker cancel requires authoritative coordination broker evidence",
            Some(json!({ "assignment_id": args.assignment_id })),
        ));
    }
    if worker_bound && quiescence.broker_present && !quiescence.broker_identity_matched {
        return Err(CliError::data(
            "coordination-broker-incarnation-conflict",
            "worker cancel requires an incarnation-matched coordination broker",
            Some(json!({ "assignment_id": args.assignment_id })),
        ));
    }
    if worker_bound && !quiescence.broker_authoritative && !broker_evidence_waived {
        return Err(CliError::runtime(
            "coordination-broker-unavailable",
            "worker cancel requires a ready, fresh, capability-backed coordination broker",
            Some(json!({ "assignment_id": args.assignment_id })),
        ));
    }
    if worker_bound
        && (quiescence.active_claim
            || quiescence.active_operation
            || quiescence.uncertain_operation)
    {
        return Err(CliError::data(
            "worker-not-quiescent",
            "worker cancel refuses an active claim or active/uncertain operation",
            Some(json!({
                "assignment_id": args.assignment_id,
                "last_proven_safe_state": diagnosis
            })),
        ));
    }
    if !quiescence.has_active_claim(&main.id, &main_incarnation) {
        return Err(CliError::data(
            "claim-not-active",
            "Main Agent claim is no longer active at the cancellation boundary",
            Some(json!({ "assignment_id": args.assignment_id })),
        ));
    }
    let mut locked = orchestration::lock_registry(context)?;
    if let Some(value) = idempotency_replay(
        &locked.registry,
        &main,
        &main_incarnation,
        &args.idempotency_key,
        "worker-cancel",
        &request_digest,
    )? {
        return Ok(value);
    }
    let current_run = require_current_main(&locked.registry, &main, &main_incarnation)?.clone();
    let run_id = current_run.run_id.clone();
    let current = locked
        .registry
        .assignments
        .get_mut(&args.assignment_id)
        .filter(|assignment| assignment.run_id == run_id)
        .ok_or_else(|| not_found("assignment-not-found", "assignment was not found"))?;
    ensure_primary_manager(current, &main, &main_incarnation)?;
    ensure_revision(args.if_revision, current.revision, "assignment")?;
    if terminal_recovery_reconciled && !has_terminal_reconciled_recovery(current, &current_run) {
        return Err(CliError::data(
            "submit-recovery-attempt-conflict",
            "terminal recovery reconciliation changed before cancellation",
            None,
        ));
    }
    ensure_submit_recovery_not_in_flight(current)?;
    ensure_account_handoff_not_in_flight(current)?;
    if !matches!(current.state.as_str(), "starting" | "blocked") {
        return Err(CliError::data(
            "assignment-state-conflict",
            "worker cancel requires a starting or blocked assignment",
            Some(json!({ "state": current.state, "revision": current.revision })),
        ));
    }
    current.state = "cancelled".to_string();
    current.revision = current.revision.saturating_add(1);
    current.updated_at = timestamp();
    current.blocker_summary = Some(format!(
        "Cancelled before claim acquisition: {}",
        args.reason
    ));
    let outcome = json!({
        "schema_version": "main-agent.worker-cancel-result.v1",
        "assignment": public_assignment_view(current),
        "claim_absent": true,
        "operation_quiescent": true,
        "next_action": "retire this exact cancelled assignment before starting a replacement"
    });
    store_receipt(
        &mut locked.registry,
        &main,
        &main_incarnation,
        &args.idempotency_key,
        "worker-cancel",
        &request_digest,
        outcome.clone(),
    )?;
    locked.save()?;
    drop(quiescence);
    Ok(outcome)
}

fn ensure_submit_recovery_not_in_flight(assignment: &AssignmentRecord) -> Result<(), CliError> {
    if assignment.submit_recovery.as_ref().is_some_and(|recovery| {
        matches!(recovery.state.as_str(), "attempting" | "sent")
            && assignment
                .checkpoint
                .as_ref()
                .is_none_or(|checkpoint| checkpoint.revision <= recovery.reserved_revision)
    }) {
        return Err(CliError::data(
            "submit-recovery-in-flight",
            "assignment mutation is fenced until the reserved recovery attempt is resolved",
            Some(json!({
                "assignment_id": assignment.assignment_id,
                "revision": assignment.revision
            })),
        ));
    }
    Ok(())
}

fn run_worker_submit_recovery(
    context: &CliContext,
    args: WorkerSubmitRecoveryArgs,
) -> Result<Value, CliError> {
    validate_idempotency_key(&args.idempotency_key)?;
    let timeout = Duration::from_secs(parse_bounded_duration(&args.timeout, 30)?);
    let (main, main_incarnation) = authenticated_self(context)?;
    ensure_active_claim(context, &main)?;
    let request_digest = crate::coordination::request_digest(
        "worker-submit-recovery",
        &json!({
            "assignment_id": args.assignment_id,
            "if_revision": args.if_revision,
            "timeout": args.timeout
        }),
    );
    let registry = orchestration::load_registry_readonly(context)?;
    let replay = idempotency_replay(
        &registry,
        &main,
        &main_incarnation,
        &args.idempotency_key,
        "worker-submit-recovery",
        &request_digest,
    )?;
    let (reservation, send_reserved_input) = match replay {
        Some(value) if value["schema_version"] == "main-agent.worker-submit-recovery-result.v1" => {
            if value["checkpoint_confirmed"] != true {
                let current = registry
                    .assignments
                    .get(&args.assignment_id)
                    .ok_or_else(|| not_found("assignment-not-found", "assignment was not found"))?;
                let reservation = submit_recovery_reservation_from_assignment(current)?;
                if matches!(
                    submit_recovery_checkpoint(current, &main, &main_incarnation, &reservation),
                    SubmitRecoveryCheckpoint::Confirmed
                ) {
                    let (checkpoint_confirmed, result) = update_submit_recovery(
                        context,
                        &main,
                        &main_incarnation,
                        &reservation,
                        "checkpoint_confirmed",
                        "authenticated worker checkpoint confirmed",
                    )?;
                    let registry = orchestration::load_registry_readonly(context)?;
                    let assignment_view = registry
                        .assignments
                        .get(&args.assignment_id)
                        .map(public_assignment_view)
                        .ok_or_else(|| {
                            not_found("assignment-not-found", "assignment was not found")
                        })?;
                    let upgraded = json!({
                        "schema_version": "main-agent.worker-submit-recovery-result.v1",
                        "assignment": assignment_view,
                        "attempt_count": 1,
                        "checkpoint_confirmed": checkpoint_confirmed,
                        "automatic_retry_safe": false,
                        "result": result,
                        "last_proven_safe_state": "authenticated worker checkpoint confirmed"
                    });
                    let mut locked = orchestration::lock_registry(context)?;
                    store_receipt(
                        &mut locked.registry,
                        &main,
                        &main_incarnation,
                        &args.idempotency_key,
                        "worker-submit-recovery",
                        &request_digest,
                        upgraded.clone(),
                    )?;
                    locked.save()?;
                    return Ok(upgraded);
                }
            }
            return Ok(value);
        }
        Some(value) => (submit_recovery_reservation_from_progress(&value)?, false),
        None => match reserve_submit_recovery(
            context,
            &main,
            &main_incarnation,
            &args.assignment_id,
            Some(args.if_revision),
            None,
            Some((&args.idempotency_key, &request_digest)),
            None,
        ) {
            Ok(reservation) => (reservation, true),
            Err(error) if error.code() == "submit-recovery-ineligible" => {
                let registry = orchestration::load_registry_readonly(context)?;
                if let Some(replay) = idempotency_replay(
                    &registry,
                    &main,
                    &main_incarnation,
                    &args.idempotency_key,
                    "worker-submit-recovery",
                    &request_digest,
                )? {
                    if replay["schema_version"] == "main-agent.worker-submit-recovery-result.v1" {
                        return Ok(replay);
                    }
                    (submit_recovery_reservation_from_progress(&replay)?, false)
                } else if let Some(reservation) = adopt_automatic_submit_recovery(
                    context,
                    &main,
                    &main_incarnation,
                    &args.assignment_id,
                    args.if_revision,
                    &args.idempotency_key,
                    &request_digest,
                )? {
                    (reservation, false)
                } else {
                    return Err(error);
                }
            }
            Err(error) => return Err(error),
        },
    };
    pause_submit_recovery_for_test(if send_reserved_input {
        "owner_reserved"
    } else {
        "joined_in_progress"
    })?;
    let (checkpoint_confirmed, result) = if send_reserved_input {
        match send_reserved_submit_recovery(context, &reservation, None) {
            Err(code) if submit_recovery_send_outcome_is_unknown(&code) => {
                (false, "submit-recovery-send-outcome-unknown".to_string())
            }
            Err(code) => {
                let (confirmed, result) = update_submit_recovery(
                    context,
                    &main,
                    &main_incarnation,
                    &reservation,
                    "failed",
                    &code,
                )?;
                (confirmed, result)
            }
            Ok(()) => {
                let (confirmed, result) = update_submit_recovery(
                    context,
                    &main,
                    &main_incarnation,
                    &reservation,
                    "sent",
                    "single guarded Enter sent",
                )?;
                if confirmed || result != "single guarded Enter sent" {
                    (confirmed, result)
                } else {
                    await_submit_recovery_result(
                        context,
                        &main,
                        &main_incarnation,
                        &reservation,
                        timeout,
                    )?
                }
            }
        }
    } else {
        await_submit_recovery_result(context, &main, &main_incarnation, &reservation, timeout)?
    };
    let assignment_view = {
        let registry = orchestration::load_registry_readonly(context)?;
        registry
            .assignments
            .get(&args.assignment_id)
            .map(public_assignment_view)
            .ok_or_else(|| not_found("assignment-not-found", "assignment was not found"))?
    };
    let outcome_resumable =
        result == "submit-recovery-send-outcome-unknown" && !checkpoint_confirmed;
    let outcome = json!({
        "schema_version": "main-agent.worker-submit-recovery-result.v1",
        "assignment": assignment_view,
        "attempt_count": 1,
        "checkpoint_confirmed": checkpoint_confirmed,
        "automatic_retry_safe": false,
        "result": result,
        "last_proven_safe_state": if checkpoint_confirmed {
            "authenticated worker checkpoint confirmed"
        } else {
            "one guarded Enter is durably recorded and may never be repeated; diagnose without resending the prompt or injecting Enter"
        }
    });
    if !outcome_resumable {
        let mut locked = orchestration::lock_registry(context)?;
        store_receipt(
            &mut locked.registry,
            &main,
            &main_incarnation,
            &args.idempotency_key,
            "worker-submit-recovery",
            &request_digest,
            outcome.clone(),
        )?;
        locked.save()?;
    }
    Ok(outcome)
}

fn pause_submit_recovery_for_test(stage: &str) -> Result<(), CliError> {
    #[cfg(debug_assertions)]
    if env::var("NILS_AGENT_SESSION_TEST_SUBMIT_RECOVERY_BARRIER_STAGE").as_deref() == Ok(stage)
        && let Some(directory) =
            env::var_os("NILS_AGENT_SESSION_TEST_SUBMIT_RECOVERY_BARRIER_DIR").map(PathBuf::from)
    {
        fs::write(directory.join("ready"), stage.as_bytes()).map_err(|_| {
            CliError::runtime(
                "test-barrier-unavailable",
                "submit recovery test barrier could not be signalled",
                None,
            )
        })?;
        let release = directory.join("release");
        let deadline = Instant::now() + Duration::from_secs(10);
        while !release.is_file() {
            if Instant::now() >= deadline {
                return Err(CliError::runtime(
                    "test-barrier-timeout",
                    "submit recovery test barrier timed out",
                    None,
                ));
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
    Ok(())
}

fn run_worker_reconcile_recovery(
    context: &CliContext,
    args: WorkerReconcileRecoveryArgs,
) -> Result<Value, CliError> {
    validate_idempotency_key(&args.idempotency_key)?;
    let (main, main_incarnation) = authenticated_self(context)?;
    ensure_active_claim(context, &main)?;
    let request_digest = crate::coordination::request_digest(
        "worker-reconcile-recovery",
        &json!({
            "assignment_id": args.assignment_id,
            "if_revision": args.if_revision
        }),
    );
    let registry = orchestration::load_registry_readonly(context)?;
    if let Some(value) = idempotency_replay(
        &registry,
        &main,
        &main_incarnation,
        &args.idempotency_key,
        "worker-reconcile-recovery",
        &request_digest,
    )? {
        return Ok(value);
    }
    let run = require_current_main(&registry, &main, &main_incarnation)?;
    let assignment = registry
        .assignments
        .get(&args.assignment_id)
        .filter(|assignment| assignment.run_id == run.run_id)
        .ok_or_else(|| not_found("assignment-not-found", "assignment was not found"))?
        .clone();
    ensure_primary_manager(&assignment, &main, &main_incarnation)?;
    ensure_revision(args.if_revision, assignment.revision, "assignment")?;
    let recovery = assignment.submit_recovery.as_ref().ok_or_else(|| {
        CliError::data(
            "submit-recovery-not-unknown",
            "assignment has no recovery attempt to reconcile",
            None,
        )
    })?;
    if recovery.state != "attempting" {
        return Err(CliError::data(
            "submit-recovery-not-unknown",
            "only an unknown attempting recovery can be reconciled without input",
            Some(json!({ "state": recovery.state, "result": recovery.result })),
        ));
    }
    if recovery.run_id.as_deref() != Some(run.run_id.as_str())
        || recovery.controller.as_ref() != Some(&run.controller)
    {
        return Err(CliError::data(
            "submit-recovery-controller-unbound",
            "recovery attempt is not bound to the current run and controller",
            None,
        ));
    }
    let worker = assignment
        .worker
        .clone()
        .filter(|worker| worker.session_incarnation == recovery.session_incarnation)
        .ok_or_else(|| {
            CliError::data(
                "worker-incarnation-changed",
                "reserved recovery worker identity is unavailable",
                None,
            )
        })?;
    if assignment
        .checkpoint
        .as_ref()
        .is_some_and(|checkpoint| checkpoint.revision > recovery.reserved_revision)
    {
        return Err(CliError::data(
            "submit-recovery-checkpoint-available",
            "a newer worker checkpoint must be reconciled through submit-recovery replay",
            None,
        ));
    }

    // The record lock is the sender lifecycle boundary. Acquiring it proves
    // that the original timed-out invocation no longer owns the tmux command,
    // and retaining it prevents the exact runtime from resuming or being
    // replaced while stopped/quiescent evidence is committed.
    let _worker_lifecycle = acquire_session_record_lock(context, &worker.session_id)?;
    let worker_record = load_session_record(context, &worker.session_id)?;
    let worker_incarnation = worker_record
        .runtime
        .as_ref()
        .map(|runtime| runtime.launch_id.as_str())
        .unwrap_or_default();
    if !orchestration::session_ref_matches(&worker, &worker_record, worker_incarnation) {
        return Err(CliError::data(
            "worker-incarnation-changed",
            "reserved recovery worker identity changed before reconciliation",
            None,
        ));
    }
    let runtime_evidence = crate::coordination_runtime_evidence(&worker_record)?;
    match runtime_evidence.status {
        crate::CoordinationRuntimeStatus::Stopped => {}
        crate::CoordinationRuntimeStatus::Running => {
            return Err(CliError::data(
                "submit-recovery-runtime-still-live",
                "recovery cannot be terminalized while the exact worker process runtime can still act",
                Some(json!({ "assignment_id": args.assignment_id })),
            ));
        }
        crate::CoordinationRuntimeStatus::Unknown => {
            return Err(CliError::runtime(
                "coordination-runtime-unverified",
                "recovery cannot be terminalized without stopped exact-runtime evidence",
                Some(json!({ "assignment_id": args.assignment_id })),
            ));
        }
    }
    if session_status(&resolve_tmux_bin(None), &worker_record) != "stopped" {
        return Err(CliError::data(
            "submit-recovery-runtime-still-live",
            "recovery cannot be terminalized while the exact worker runtime can still act",
            Some(json!({ "assignment_id": args.assignment_id })),
        ));
    }
    let quiescence = crate::coordination::lock_session_quiescence(
        context,
        &worker.session_id,
        &worker.session_incarnation,
    )?;
    if quiescence.active_claim || quiescence.active_operation || quiescence.uncertain_operation {
        return Err(CliError::data(
            "worker-not-quiescent",
            "recovery reconciliation requires no worker claim or active/uncertain operation",
            None,
        ));
    }
    if !quiescence.has_active_claim(&main.id, &main_incarnation) {
        return Err(CliError::data(
            "claim-not-active",
            "reserving Main Agent claim is no longer active at recovery reconciliation",
            None,
        ));
    }

    let mut locked = orchestration::lock_registry(context)?;
    if let Some(value) = idempotency_replay(
        &locked.registry,
        &main,
        &main_incarnation,
        &args.idempotency_key,
        "worker-reconcile-recovery",
        &request_digest,
    )? {
        return Ok(value);
    }
    let current_run = require_current_main(&locked.registry, &main, &main_incarnation)?.clone();
    let current = locked
        .registry
        .assignments
        .get_mut(&args.assignment_id)
        .filter(|assignment| assignment.run_id == current_run.run_id)
        .ok_or_else(|| not_found("assignment-not-found", "assignment was not found"))?;
    ensure_primary_manager(current, &main, &main_incarnation)?;
    ensure_revision(args.if_revision, current.revision, "assignment")?;
    if current.worker.as_ref() != Some(&worker) {
        return Err(CliError::data(
            "worker-incarnation-changed",
            "reserved recovery worker identity changed before reconciliation commit",
            None,
        ));
    }
    let recovery = current.submit_recovery.as_mut().ok_or_else(|| {
        CliError::data(
            "submit-recovery-not-unknown",
            "assignment recovery disappeared before reconciliation commit",
            None,
        )
    })?;
    if recovery.state != "attempting"
        || recovery.run_id.as_deref() != Some(current_run.run_id.as_str())
        || recovery.controller.as_ref() != Some(&current_run.controller)
        || recovery.session_incarnation != worker.session_incarnation
    {
        return Err(CliError::data(
            "submit-recovery-attempt-conflict",
            "recovery attempt changed before reconciliation commit",
            None,
        ));
    }
    let reconciled_at = timestamp();
    let proposed_quarantine = WorkerQuarantineRecord {
        schema_version: WORKER_QUARANTINE_SCHEMA.to_string(),
        worker: worker.clone(),
        reason: "stopped runtime reconciled without a worker checkpoint".to_string(),
        runtime_identity_digest: format!("sha256:{}", runtime_evidence.identity_digest),
        created_at: reconciled_at.clone(),
    };
    let reconciled_revision = current.revision.saturating_add(1);
    let quarantine = orchestration::persist_session_authority_quarantine(
        context,
        &current.assignment_id,
        reconciled_revision,
        &proposed_quarantine,
    )?;
    current.worker_quarantine = Some(quarantine);
    recovery.state = "reconciled".to_string();
    recovery.result = "worker-runtime-stopped-without-checkpoint".to_string();
    recovery.updated_at = reconciled_at;
    current.revision = reconciled_revision;
    current.updated_at = recovery.updated_at.clone();
    let assignment_view = public_assignment_view(current);
    let outcome = json!({
        "schema_version": "main-agent.worker-reconcile-recovery-result.v1",
        "assignment": assignment_view,
        "reconciled": true,
        "checkpoint_confirmed": false,
        "input_sent": false,
        "automatic_retry_safe": false,
        "proof": {
            "worker_runtime": "stopped",
            "runtime_identity_digest": runtime_evidence.identity_digest,
            "send_boundary": "exclusive-record-lock",
            "coordination": "quiescent"
        },
        "last_proven_safe_state": "unknown recovery terminalized without input; guarded cancellation, retirement, or reassignment may proceed"
    });
    store_receipt(
        &mut locked.registry,
        &main,
        &main_incarnation,
        &args.idempotency_key,
        "worker-reconcile-recovery",
        &request_digest,
        outcome.clone(),
    )?;
    locked.save()?;
    drop(quiescence);
    Ok(outcome)
}

fn pause_cancel_after_admission_for_test(assignment: &AssignmentRecord) -> Result<(), CliError> {
    #[cfg(debug_assertions)]
    if let Some(directory) =
        env::var_os("NILS_AGENT_SESSION_TEST_WORKER_CANCEL_BARRIER_DIR").map(PathBuf::from)
    {
        let expected_assignment = env::var("NILS_AGENT_SESSION_TEST_WORKER_CANCEL_ASSIGNMENT")
            .map_err(|_| {
                CliError::runtime(
                    "test-barrier-unavailable",
                    "worker cancel test barrier assignment is unavailable",
                    None,
                )
            })?;
        let expected_worker =
            env::var("NILS_AGENT_SESSION_TEST_WORKER_CANCEL_WORKER").map_err(|_| {
                CliError::runtime(
                    "test-barrier-unavailable",
                    "worker cancel test barrier worker is unavailable",
                    None,
                )
            })?;
        let actual_worker = assignment
            .worker
            .as_ref()
            .map(|worker| format!("{}@{}", worker.session_id, worker.session_incarnation))
            .unwrap_or_default();
        if expected_assignment != assignment.assignment_id || expected_worker != actual_worker {
            return Ok(());
        }
        let ready = directory.join("ready");
        let release = directory.join("release");
        fs::write(&ready, b"cancel-admission-complete").map_err(|_| {
            CliError::runtime(
                "test-barrier-unavailable",
                "worker cancel test barrier could not be signalled",
                None,
            )
        })?;
        let deadline = Instant::now() + Duration::from_secs(10);
        while !release.is_file() {
            if Instant::now() >= deadline {
                return Err(CliError::runtime(
                    "test-barrier-timeout",
                    "worker cancel test barrier timed out",
                    None,
                ));
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
    Ok(())
}

fn run_worker_reassign(context: &CliContext, args: WorkerReassignArgs) -> Result<Value, CliError> {
    validate_idempotency_key(&args.idempotency_key)?;
    orchestration::validate_summary("reassignment reason", &args.reason)?;
    parse_await_ready(&args.await_ready)?;
    let replacement: AssignmentInput = crate::coordination::read_bounded_json(
        &args.assignment_file,
        256 * 1024,
        "invalid-assignment-packet",
    )?;
    validate_assignment_input(&replacement)?;
    let replacement_id = replacement.assignment_id.clone().ok_or_else(|| {
        invalid_input("worker reassign requires the replacement packet to declare assignment_id")
    })?;
    if replacement_id == args.assignment_id {
        return Err(invalid_input(
            "worker reassign requires a distinct replacement assignment_id",
        ));
    }
    let replacement_start_digest =
        crate::coordination::request_digest("main-agent-worker-start", &replacement);
    let replacement_session = replacement.launch.session_id.clone().unwrap_or_else(|| {
        retry_stable_worker_session_id(&replacement_id, &replacement_start_digest)
    });
    let (main, main_incarnation) = authenticated_self(context)?;
    ensure_active_claim(context, &main)?;
    let request_digest = crate::coordination::request_digest(
        "worker-reassign",
        &json!({
            "assignment_id": args.assignment_id,
            "replacement": replacement,
            "if_revision": args.if_revision,
            "reason": args.reason,
            "await_ready": args.await_ready
        }),
    );
    let registry = orchestration::load_registry_readonly(context)?;
    let mut progress = match idempotency_replay(
        &registry,
        &main,
        &main_incarnation,
        &args.idempotency_key,
        "worker-reassign",
        &request_digest,
    )? {
        Some(value) if value["state"] == "reassigned" => return Ok(value),
        Some(value)
            if value["schema_version"] == "main-agent.worker-reassign-progress.v1"
                && value["state"] == "in_progress" =>
        {
            value
        }
        Some(_) => {
            return Err(CliError::data(
                "idempotency-conflict",
                "worker reassign receipt is not resumable",
                None,
            ));
        }
        None => {
            let diagnosis = diagnose_worker(context, &args.assignment_id)?;
            if diagnosis["new_assignment_safe"] != true {
                return Ok(json!({
                    "schema_version": "main-agent.worker-reassign-result.v1",
                    "state": "failed",
                    "failed_stage": "diagnosis",
                    "automatic_retry_safe": false,
                    "last_proven_safe_state": diagnosis
                }));
            }
            let old = registry
                .assignments
                .get(&args.assignment_id)
                .ok_or_else(|| not_found("assignment-not-found", "assignment was not found"))?;
            let old_packet: AssignmentInput = serde_json::from_value(orchestration::read_packet(
                context,
                &old.private_packet_digest,
            )?)
            .map_err(|_| invalid_input("stored assignment packet is invalid"))?;
            if old
                .worker
                .as_ref()
                .is_some_and(|worker| worker.session_id == replacement_session)
            {
                return Err(invalid_input(
                    "worker reassign requires a distinct replacement session_id",
                ));
            }
            let old_cwd = fs::canonicalize(&old_packet.launch.cwd)
                .map_err(|_| invalid_input("failed assignment worktree is unavailable"))?;
            let replacement_cwd = fs::canonicalize(&replacement.launch.cwd)
                .map_err(|_| invalid_input("replacement assignment worktree is unavailable"))?;
            if old_cwd == replacement_cwd {
                return Err(invalid_input(
                    "worker reassign requires a distinct replacement worktree",
                ));
            }
            if diagnosis["worktree_progress"]["clean"] != true
                || !worktree_is_clean(&replacement_cwd)
            {
                return Err(CliError::data(
                    "reassignment-worktree-not-clean",
                    "worker reassign requires both retained and replacement worktrees to be clean",
                    Some(json!({ "last_proven_safe_state": diagnosis })),
                ));
            }
            let (cancel_then_reassign_safe, cancel_step) =
                reassign_cancel_step(&diagnosis, &args.assignment_id);
            let progress = json!({
                "schema_version": "main-agent.worker-reassign-progress.v1",
                "state": "in_progress",
                "old_assignment_id": args.assignment_id,
                "replacement_assignment_id": replacement_id,
                "replacement_session_id": replacement_session,
                "reason": args.reason,
                "next_stage": if cancel_then_reassign_safe { "cancel" } else { "retire" },
                "diagnosis": diagnosis,
                "cancel": cancel_step,
                "retire": Value::Null,
                "start": Value::Null,
                "last_proven_safe_state": diagnosis
            });
            persist_reassign_receipt(
                context,
                &main,
                &main_incarnation,
                &args.idempotency_key,
                &request_digest,
                progress.clone(),
            )?;
            progress
        }
    };
    let cancelled = if progress["cancel"].is_null() {
        match run_worker_cancel(
            context,
            WorkerCancelArgs {
                assignment_id: args.assignment_id.clone(),
                if_revision: args.if_revision,
                reason: args.reason.clone(),
                idempotency_key: child_idempotency_key(&args.idempotency_key, "cancel"),
                format: OutputFormat::Json,
            },
        ) {
            Ok(value) => {
                progress["cancel"] = value.clone();
                progress["next_stage"] = json!("retire");
                progress["last_proven_safe_state"] = value.clone();
                persist_reassign_receipt(
                    context,
                    &main,
                    &main_incarnation,
                    &args.idempotency_key,
                    &request_digest,
                    progress.clone(),
                )?;
                value
            }
            Err(error) => {
                persist_reassign_failure(
                    context,
                    &main,
                    &main_incarnation,
                    &args.idempotency_key,
                    &request_digest,
                    &mut progress,
                    "cancel",
                    &error,
                )?;
                return Ok(macro_failure(
                    "main-agent.worker-reassign-result.v1",
                    "cancel",
                    progress["last_proven_safe_state"].clone(),
                    error,
                ));
            }
        }
    } else {
        progress["cancel"].clone()
    };
    let cancelled_revision = cancelled["assignment"]["revision"]
        .as_u64()
        .ok_or_else(|| invalid_input("worker cancel result revision is unavailable"))?;
    let retired = if progress["retire"].is_null() {
        match run_worker_retire(
            context,
            AssignmentMutationArgs {
                assignment_id: args.assignment_id.clone(),
                if_revision: cancelled_revision,
                idempotency_key: child_idempotency_key(&args.idempotency_key, "retire"),
                format: OutputFormat::Json,
            },
        ) {
            Ok(value) => {
                progress["retire"] = value.clone();
                progress["next_stage"] = json!("start");
                progress["last_proven_safe_state"] = value.clone();
                persist_reassign_receipt(
                    context,
                    &main,
                    &main_incarnation,
                    &args.idempotency_key,
                    &request_digest,
                    progress.clone(),
                )?;
                value
            }
            Err(error) => {
                persist_reassign_failure(
                    context,
                    &main,
                    &main_incarnation,
                    &args.idempotency_key,
                    &request_digest,
                    &mut progress,
                    "retire",
                    &error,
                )?;
                return Ok(macro_failure(
                    "main-agent.worker-reassign-result.v1",
                    "retire",
                    progress["last_proven_safe_state"].clone(),
                    error,
                ));
            }
        }
    } else {
        progress["retire"].clone()
    };
    let started = if progress["start"].is_null() {
        match run_worker_start_single(
            context,
            WorkerStartArgs {
                assignment_file: Some(args.assignment_file.clone()),
                batch: None,
                if_run_revision: None,
                await_ready: args.await_ready.clone(),
                idempotency_key: child_idempotency_key(&args.idempotency_key, "start"),
                format: OutputFormat::Json,
            },
        ) {
            Ok(value) => {
                progress["start"] = value.clone();
                progress["next_stage"] = json!("complete");
                progress["last_proven_safe_state"] = value.clone();
                persist_reassign_receipt(
                    context,
                    &main,
                    &main_incarnation,
                    &args.idempotency_key,
                    &request_digest,
                    progress.clone(),
                )?;
                value
            }
            Err(error) => {
                persist_reassign_failure(
                    context,
                    &main,
                    &main_incarnation,
                    &args.idempotency_key,
                    &request_digest,
                    &mut progress,
                    "start",
                    &error,
                )?;
                return Ok(macro_failure(
                    "main-agent.worker-reassign-result.v1",
                    "start",
                    progress["last_proven_safe_state"].clone(),
                    error,
                ));
            }
        }
    } else {
        progress["start"].clone()
    };
    let outcome = json!({
        "schema_version": "main-agent.worker-reassign-result.v1",
        "state": "reassigned",
        "old_assignment_id": args.assignment_id,
        "replacement_assignment_id": replacement_id,
        "reason": args.reason,
        "cancel": cancelled,
        "retire": retired,
        "start": started,
        "last_proven_safe_state": "replacement assignment is durably created; branch on its typed readiness result"
    });
    persist_reassign_receipt(
        context,
        &main,
        &main_incarnation,
        &args.idempotency_key,
        &request_digest,
        outcome.clone(),
    )?;
    Ok(outcome)
}

fn reassign_cancel_step(diagnosis: &Value, assignment_id: &str) -> (bool, Value) {
    let cancel_then_reassign_safe = diagnosis["cancel_then_reassign_safe"]
        .as_bool()
        .unwrap_or(true);
    let cancel_step = if cancel_then_reassign_safe {
        Value::Null
    } else {
        json!({
            "skipped": true,
            "reason": "assignment is already terminal and coordination-quiescent",
            "assignment": {
                "assignment_id": assignment_id,
                "revision": diagnosis["assignment_revision"],
                "state": diagnosis["assignment_state"]
            }
        })
    };
    (cancel_then_reassign_safe, cancel_step)
}

fn persist_reassign_receipt(
    context: &CliContext,
    main: &SessionRecord,
    main_incarnation: &str,
    idempotency_key: &str,
    request_digest: &str,
    outcome: Value,
) -> Result<(), CliError> {
    let mut locked = orchestration::lock_registry(context)?;
    store_receipt(
        &mut locked.registry,
        main,
        main_incarnation,
        idempotency_key,
        "worker-reassign",
        request_digest,
        outcome,
    )?;
    locked.save()
}

#[allow(clippy::too_many_arguments)]
fn persist_reassign_failure(
    context: &CliContext,
    main: &SessionRecord,
    main_incarnation: &str,
    idempotency_key: &str,
    request_digest: &str,
    progress: &mut Value,
    stage: &str,
    error: &CliError,
) -> Result<(), CliError> {
    progress["failed_stage"] = json!(stage);
    progress["next_stage"] = json!(stage);
    progress["error"] = json!({
        "code": error.code()
    });
    persist_reassign_receipt(
        context,
        main,
        main_incarnation,
        idempotency_key,
        request_digest,
        progress.clone(),
    )
}

fn macro_failure(
    schema_version: &'static str,
    stage: &'static str,
    safe_state: Value,
    error: CliError,
) -> Value {
    let error = error.into_inner();
    json!({
        "schema_version": schema_version,
        "state": "failed",
        "failed_stage": stage,
        "automatic_retry_safe": false,
        "last_proven_safe_state": safe_state,
        "error": {
            "code": error.code,
            "message": error.message,
            "details": error.details
        }
    })
}

fn run_worker_message(context: &CliContext, args: WorkerMessageArgs) -> Result<Value, CliError> {
    validate_idempotency_key(&args.idempotency_key)?;
    let (record, incarnation) = authenticated_self(context)?;
    ensure_active_claim(context, &record)?;
    let registry = orchestration::load_registry_readonly(context)?;
    let run = require_current_main(&registry, &record, &incarnation)?;
    let assignment = registry
        .assignments
        .get(&args.assignment_id)
        .filter(|assignment| assignment.run_id == run.run_id)
        .ok_or_else(|| not_found("assignment-not-found", "assignment was not found"))?;
    ensure_primary_manager(assignment, &record, &incarnation)?;
    let worker = assignment
        .worker
        .as_ref()
        .ok_or_else(|| not_found("worker-not-started", "worker session is not available"))?;
    let expected_worker = worker.clone();
    let expected_run_id = run.run_id.clone();
    pause_message_after_routing_read_for_test(&args.assignment_id, &expected_worker)?;
    crate::coordination::mailbox::send_with_commit_authorization(
        context,
        cli::MessageSendArgs {
            from_session: record.id.clone(),
            to_session: worker.session_id.clone(),
            body_file: args.body_file,
            capability_file: None,
            idempotency_key: args.idempotency_key,
            reply_to: None,
            expires_in: None,
            format: OutputFormat::Json,
        },
        || {
            let locked = orchestration::lock_registry(context)?;
            {
                let run = require_current_main(&locked.registry, &record, &incarnation)?;
                if run.run_id != expected_run_id {
                    return Err(not_found(
                        "assignment-not-found",
                        "assignment was not found",
                    ));
                }
                let assignment = locked
                    .registry
                    .assignments
                    .get(&args.assignment_id)
                    .filter(|assignment| assignment.run_id == expected_run_id)
                    .ok_or_else(|| not_found("assignment-not-found", "assignment was not found"))?;
                ensure_primary_manager(assignment, &record, &incarnation)?;
                if assignment.worker.as_ref() != Some(&expected_worker) {
                    return Err(CliError::data(
                        "worker-session-conflict",
                        "assignment worker changed before message delivery",
                        None,
                    ));
                }
            }
            Ok(locked)
        },
    )
}

fn run_worker_guidance_reconcile(
    context: &CliContext,
    args: AssignmentMutationArgs,
) -> Result<Value, CliError> {
    validate_idempotency_key(&args.idempotency_key)?;
    let request_digest = crate::coordination::request_digest(
        "worker-guidance-reconcile",
        &json!({
            "assignment_id": args.assignment_id,
            "if_revision": args.if_revision
        }),
    );
    let (record, incarnation) = authenticated_self(context)?;
    ensure_active_claim(context, &record)?;
    let registry = orchestration::load_registry_readonly(context)?;
    let run = require_current_main(&registry, &record, &incarnation)?;
    if let Some(value) = idempotency_replay(
        &registry,
        &record,
        &incarnation,
        &args.idempotency_key,
        "worker-guidance-reconcile",
        &request_digest,
    )? {
        return Ok(value);
    }
    let assignment = registry
        .assignments
        .get(&args.assignment_id)
        .filter(|assignment| assignment.run_id == run.run_id)
        .ok_or_else(|| not_found("assignment-not-found", "assignment was not found"))?;
    ensure_primary_manager(assignment, &record, &incarnation)?;
    ensure_revision(args.if_revision, assignment.revision, "assignment")?;
    let current_worker = assignment
        .worker
        .as_ref()
        .cloned()
        .ok_or_else(|| not_found("worker-not-started", "worker session is not available"))?;
    let previous_worker = assignment
        .previous_worker
        .as_ref()
        .cloned()
        .ok_or_else(|| {
            CliError::data(
                "guidance-continuity-unavailable",
                "the assignment does not retain an immediately stale worker incarnation",
                None,
            )
        })?;
    let expected_run_id = run.run_id.clone();
    let expected_manager = assignment.primary_manager.clone();
    let expected_revision = assignment.revision;
    drop(registry);

    let worker_authority =
        crate::lock_exact_session_authority(context, &current_worker.session_id)?
            .ok_or_else(|| not_found("worker-session-not-found", "worker session was not found"))?;
    let worker_incarnation = worker_authority
        .record
        .runtime
        .as_ref()
        .map(|runtime| runtime.launch_id.as_str())
        .unwrap_or_default();
    if !orchestration::session_ref_matches(
        &current_worker,
        &worker_authority.record,
        worker_incarnation,
    ) {
        return Err(CliError::data(
            "worker-session-conflict",
            "assignment worker changed before guidance reconciliation",
            None,
        ));
    }
    let _carried =
        crate::coordination::carry_forward_unread_controller_guidance_with_authorization(
            context,
            &current_worker.session_id,
            &previous_worker.session_incarnation,
            &current_worker.session_incarnation,
            &record.id,
            &incarnation,
            || {
                let locked = orchestration::lock_registry(context)?;
                let run = require_current_main(&locked.registry, &record, &incarnation)?;
                if run.run_id != expected_run_id {
                    return Err(not_found(
                        "assignment-not-found",
                        "assignment was not found",
                    ));
                }
                let current = locked
                    .registry
                    .assignments
                    .get(&args.assignment_id)
                    .filter(|assignment| assignment.run_id == expected_run_id)
                    .ok_or_else(|| not_found("assignment-not-found", "assignment was not found"))?;
                ensure_primary_manager(current, &record, &incarnation)?;
                ensure_revision(expected_revision, current.revision, "assignment")?;
                if current.primary_manager != expected_manager
                    || current.worker.as_ref() != Some(&current_worker)
                    || current.previous_worker.as_ref() != Some(&previous_worker)
                {
                    return Err(CliError::data(
                        "guidance-continuity-conflict",
                        "assignment guidance routing changed before reconciliation",
                        None,
                    ));
                }
                Ok(locked)
            },
        )?;
    let outcome = json!({
        "schema_version": "main-agent.worker-guidance-reconcile.v1",
        "assignment_id": args.assignment_id,
        "assignment_revision": expected_revision,
        "controller": {
            "session_id": record.id,
            "session_incarnation": incarnation
        },
        "worker": {
            "session_id": current_worker.session_id,
            "previous_incarnation": previous_worker.session_incarnation,
            "current_incarnation": current_worker.session_incarnation
        },
        "state": "reconciled",
        "message_identity_retained": true,
        "message_body_exposed": false,
        "message_marked_consumed": false
    });
    let mut locked = orchestration::lock_registry(context)?;
    if let Some(value) = idempotency_replay(
        &locked.registry,
        &record,
        &incarnation,
        &args.idempotency_key,
        "worker-guidance-reconcile",
        &request_digest,
    )? {
        return Ok(value);
    }
    store_receipt(
        &mut locked.registry,
        &record,
        &incarnation,
        &args.idempotency_key,
        "worker-guidance-reconcile",
        &request_digest,
        outcome.clone(),
    )?;
    locked.save()?;
    Ok(outcome)
}

fn run_worker_guidance_quarantine(
    context: &CliContext,
    args: AssignmentMutationArgs,
) -> Result<Value, CliError> {
    validate_idempotency_key(&args.idempotency_key)?;
    let request_digest = crate::coordination::request_digest(
        "worker-guidance-quarantine",
        &json!({
            "assignment_id": args.assignment_id,
            "if_revision": args.if_revision
        }),
    );
    let (record, incarnation) = authenticated_self(context)?;
    ensure_active_claim(context, &record)?;
    let registry = orchestration::load_registry_readonly(context)?;
    let run = require_current_main(&registry, &record, &incarnation)?;
    if let Some(value) = idempotency_replay(
        &registry,
        &record,
        &incarnation,
        &args.idempotency_key,
        "worker-guidance-quarantine",
        &request_digest,
    )? {
        return Ok(value);
    }
    let assignment = registry
        .assignments
        .get(&args.assignment_id)
        .filter(|assignment| assignment.run_id == run.run_id)
        .ok_or_else(|| not_found("assignment-not-found", "assignment was not found"))?;
    ensure_primary_manager(assignment, &record, &incarnation)?;
    ensure_revision(args.if_revision, assignment.revision, "assignment")?;
    if assignment.previous_worker.is_some() {
        return Err(CliError::data(
            "guidance-quarantine-unavailable",
            "guidance quarantine is only for stale guidance without a retained previous worker; use guidance-reconcile when continuity identity exists",
            None,
        ));
    }
    let current_worker = assignment
        .worker
        .as_ref()
        .cloned()
        .ok_or_else(|| not_found("worker-not-started", "worker session is not available"))?;
    let expected_run_id = run.run_id.clone();
    let expected_manager = assignment.primary_manager.clone();
    let expected_revision = assignment.revision;
    drop(registry);

    let worker_authority =
        crate::lock_exact_session_authority(context, &current_worker.session_id)?
            .ok_or_else(|| not_found("worker-session-not-found", "worker session was not found"))?;
    let worker_incarnation = worker_authority
        .record
        .runtime
        .as_ref()
        .map(|runtime| runtime.launch_id.as_str())
        .unwrap_or_default();
    if !orchestration::session_ref_matches(
        &current_worker,
        &worker_authority.record,
        worker_incarnation,
    ) {
        return Err(CliError::data(
            "worker-session-conflict",
            "assignment worker changed before guidance quarantine",
            None,
        ));
    }
    let quarantined =
        crate::coordination::quarantine_orphaned_controller_guidance_with_authorization(
            context,
            &current_worker.session_id,
            &current_worker.session_incarnation,
            &record.id,
            &incarnation,
            || {
                let locked = orchestration::lock_registry(context)?;
                let run = require_current_main(&locked.registry, &record, &incarnation)?;
                if run.run_id != expected_run_id {
                    return Err(not_found(
                        "assignment-not-found",
                        "assignment was not found",
                    ));
                }
                let current = locked
                    .registry
                    .assignments
                    .get(&args.assignment_id)
                    .filter(|assignment| assignment.run_id == expected_run_id)
                    .ok_or_else(|| not_found("assignment-not-found", "assignment was not found"))?;
                ensure_primary_manager(current, &record, &incarnation)?;
                ensure_revision(expected_revision, current.revision, "assignment")?;
                if current.primary_manager != expected_manager
                    || current.worker.as_ref() != Some(&current_worker)
                    || current.previous_worker.is_some()
                {
                    return Err(CliError::data(
                        "guidance-quarantine-conflict",
                        "assignment guidance routing changed before quarantine",
                        None,
                    ));
                }
                Ok(locked)
            },
        )?;
    let outcome = json!({
        "schema_version": "main-agent.worker-guidance-quarantine.v1",
        "assignment_id": args.assignment_id,
        "assignment_revision": expected_revision,
        "controller": {
            "session_id": record.id,
            "session_incarnation": incarnation
        },
        "worker": {
            "session_id": current_worker.session_id,
            "current_incarnation": current_worker.session_incarnation
        },
        "state": "quarantined",
        "quarantined_count": quarantined,
        "message_body_exposed": false,
        "current_incarnation_preserved": true,
        "unrelated_controller_preserved": true
    });
    let mut locked = orchestration::lock_registry(context)?;
    if let Some(value) = idempotency_replay(
        &locked.registry,
        &record,
        &incarnation,
        &args.idempotency_key,
        "worker-guidance-quarantine",
        &request_digest,
    )? {
        return Ok(value);
    }
    store_receipt(
        &mut locked.registry,
        &record,
        &incarnation,
        &args.idempotency_key,
        "worker-guidance-quarantine",
        &request_digest,
        outcome.clone(),
    )?;
    locked.save()?;
    Ok(outcome)
}

fn run_worker_account_handoff(
    context: &CliContext,
    args: WorkerAccountHandoffArgs,
) -> Result<Value, CliError> {
    validate_idempotency_key(&args.idempotency_key)?;
    if !args.authorize_account_change {
        return Err(CliError::usage(
            "account-handoff-authorization-required",
            "account handoff requires --authorize-account-change for the explicitly named account",
            None,
        ));
    }
    crate::codex_account::validate_account(&args.account)?;
    let timeout = parse_wait_timeout(&args.timeout)?;
    let request_digest = crate::coordination::request_digest(
        "worker-account-handoff",
        &json!({
            "assignment_id": args.assignment_id,
            "account": args.account,
            "if_revision": args.if_revision,
            "authorize_account_change": args.authorize_account_change,
            "timeout": args.timeout
        }),
    );
    let (record, incarnation) = authenticated_self(context)?;
    ensure_active_claim(context, &record)?;
    let registry = orchestration::load_registry_readonly(context)?;
    let run = require_current_main(&registry, &record, &incarnation)?;
    let replay = idempotency_replay(
        &registry,
        &record,
        &incarnation,
        &args.idempotency_key,
        "worker-account-handoff",
        &request_digest,
    )?;
    let resume_stage = replay
        .as_ref()
        .and_then(|value| value["stage"].as_str())
        .map(str::to_string);
    if let Some(value) = replay {
        if value["schema_version"] == "main-agent.worker-account-handoff.v1" {
            return Ok(value);
        }
        if value["schema_version"] != "main-agent.worker-account-handoff-progress.v1" {
            return Err(CliError::data(
                "idempotency-conflict",
                "account handoff receipt is not resumable",
                None,
            ));
        }
    }
    let assignment = registry
        .assignments
        .get(&args.assignment_id)
        .filter(|assignment| assignment.run_id == run.run_id)
        .ok_or_else(|| not_found("assignment-not-found", "assignment was not found"))?;
    ensure_primary_manager(assignment, &record, &incarnation)?;
    let matching_reservation = assignment
        .account_handoff
        .as_ref()
        .is_some_and(|reservation| {
            account_handoff_reservation_matches(
                reservation,
                &request_digest,
                &run.run_id,
                &assignment.primary_manager,
                assignment.worker.as_ref().unwrap_or(&reservation.worker),
                args.if_revision,
                &args.account,
            ) && reservation_assignment_revision(reservation) == Some(assignment.revision)
        });
    if !matching_reservation {
        ensure_revision(args.if_revision, assignment.revision, "assignment")?;
    }
    ensure_account_handoff_eligible_state(assignment)?;
    ensure_submit_recovery_not_in_flight(assignment)?;
    let worker = assignment
        .worker
        .as_ref()
        .cloned()
        .ok_or_else(|| not_found("worker-not-started", "worker session is not available"))?;
    let expected_run_id = run.run_id.clone();
    let expected_revision = args.if_revision;
    drop(registry);
    pause_account_handoff_for_test("after_initial_ownership_read")?;

    let worker_record = load_session_record(context, &worker.session_id)?;
    let worker_incarnation = worker_record
        .runtime
        .as_ref()
        .map(|runtime| runtime.launch_id.as_str())
        .unwrap_or_default();
    if !orchestration::session_ref_matches(&worker, &worker_record, worker_incarnation) {
        return Err(CliError::data(
            "worker-session-conflict",
            "assignment worker changed before account handoff",
            None,
        ));
    }
    if worker_record.agent != "codex" {
        return Err(CliError::data(
            "account-handoff-provider-unsupported",
            "account handoff is available only for Codex workers",
            None,
        ));
    }
    let quiescence = crate::coordination::lock_session_quiescence(
        context,
        &worker.session_id,
        &worker.session_incarnation,
    )?;
    if !quiescence.broker_present
        || !quiescence.broker_identity_matched
        || !quiescence.broker_authoritative
    {
        return Err(CliError::runtime(
            "account-handoff-worker-authority-unavailable",
            "account handoff requires the exact worker broker to be authoritative",
            None,
        ));
    }
    if quiescence.active_operation || quiescence.uncertain_operation {
        return Err(CliError::data(
            "account-handoff-operation-fenced",
            "account handoff refuses an active or uncertain worker mutation",
            None,
        ));
    }
    if !quiescence.active_claim {
        return Err(CliError::data(
            "account-handoff-claim-unavailable",
            "account handoff requires the exact worker claim to remain active",
            None,
        ));
    }
    // Group cleanup orders locks as session-record -> coordination ->
    // orchestration. Account handoff never carries coordination authority into
    // session mutation or the bounded apply wait, so the reciprocal path cannot
    // form coordination -> session-record.
    drop(quiescence);
    let activity = crate::activity::activity_status_for_record(context, &worker_record)?.turn_state;
    let blocked_turn = activity
        .last_turn
        .as_ref()
        .filter(|turn| bounded_quota_outcome(&turn.outcome))
        .and_then(|turn| turn.provider_turn_id.clone())
        .map(|turn_id| (turn_id, activity.revision));
    let initial_account = crate::codex_account::view_for_record(&worker_record);
    let initial_next_identity = crate::codex_account::next_account_identity(&worker_record)?;
    let reserved_account_intent_id = initial_next_identity
        .as_ref()
        .filter(|identity| identity.account == args.account)
        .and_then(|identity| identity.intent_id.clone())
        .unwrap_or_else(|| uuid::Uuid::new_v4().simple().to_string());
    let initial_auto_resume = crate::auto_resume::view_for_record(context, &worker_record);
    if !crate::codex_app_server::managed_account_handoff_supported(&worker_record)
        || !initial_account.supported
        || !initial_auto_resume.supported
    {
        return Err(CliError::data(
            "account-handoff-capability-unavailable",
            "the exact worker does not advertise the managed account-handoff capability",
            Some(json!({
                "assignment_id": args.assignment_id,
                "worker_session_id": worker.session_id,
                "worker_session_incarnation": worker.session_incarnation,
                "exact_provider_resume_available": worker_record.provider_resume.is_some(),
                "required_capability": MANAGED_ACCOUNT_HANDOFF_CAPABILITY,
                "capability_gap_terminal_for_assignment": true,
                "lifecycle_boundary": "accept|release|cancel|retire",
                "next_action": "preserve this raw worker until the assignment reaches an explicit accept, release, cancel, or retire boundary; worker reassign and retry cannot add this capability to the current assignment",
                "account_changed": false,
                "runtime_restarted": false,
                "public_raw_fallback_advertised": false
            })),
        ));
    }
    let (reservation, reservation_created) = {
        let mut locked = orchestration::lock_registry(context)?;
        if let Some(value) = idempotency_replay(
            &locked.registry,
            &record,
            &incarnation,
            &args.idempotency_key,
            "worker-account-handoff",
            &request_digest,
        )? {
            if value["schema_version"] == "main-agent.worker-account-handoff.v1" {
                return Ok(value);
            }
            if value["schema_version"] != "main-agent.worker-account-handoff-progress.v1" {
                return Err(CliError::data(
                    "idempotency-conflict",
                    "account handoff receipt is not resumable",
                    None,
                ));
            }
        }
        let current_run = require_current_main(&locked.registry, &record, &incarnation)?;
        if current_run.run_id != expected_run_id {
            return Err(CliError::data(
                "account-handoff-assignment-conflict",
                "assignment ownership changed before the account intent was reserved",
                None,
            ));
        }
        let current = locked
            .registry
            .assignments
            .get_mut(&args.assignment_id)
            .filter(|assignment| assignment.run_id == expected_run_id)
            .ok_or_else(|| not_found("assignment-not-found", "assignment was not found"))?;
        ensure_primary_manager(current, &record, &incarnation)?;
        let matching_reservation = current.account_handoff.as_ref().is_some_and(|reservation| {
            account_handoff_reservation_matches(
                reservation,
                &request_digest,
                &expected_run_id,
                &current.primary_manager,
                &worker,
                expected_revision,
                &args.account,
            ) && reservation_assignment_revision(reservation) == Some(current.revision)
        });
        if !matching_reservation {
            ensure_revision(expected_revision, current.revision, "assignment")?;
        }
        ensure_account_handoff_eligible_state(current)?;
        ensure_submit_recovery_not_in_flight(current)?;
        if current.worker.as_ref() != Some(&worker) {
            return Err(CliError::data(
                "account-handoff-assignment-conflict",
                "assignment worker changed before the account intent was reserved",
                None,
            ));
        }
        let reservation_created = match current.account_handoff.as_ref() {
            Some(reservation)
                if account_handoff_reservation_matches(
                    reservation,
                    &request_digest,
                    &expected_run_id,
                    &current.primary_manager,
                    &worker,
                    expected_revision,
                    &args.account,
                ) =>
            {
                false
            }
            Some(_) => return Err(account_handoff_in_flight(current)),
            None => {
                let now = timestamp();
                current.account_handoff = Some(AccountHandoffReservationRecord {
                    schema_version: ACCOUNT_HANDOFF_RESERVATION_SCHEMA.to_string(),
                    request_digest: request_digest.clone(),
                    reservation_id: Some(uuid::Uuid::new_v4().simple().to_string()),
                    account_intent_id: Some(reserved_account_intent_id),
                    run_id: expected_run_id.clone(),
                    controller: current.primary_manager.clone(),
                    worker: worker.clone(),
                    reserved_revision: expected_revision,
                    account: args.account.clone(),
                    created_at: now.clone(),
                    updated_at: now.clone(),
                });
                current.revision = current.revision.saturating_add(1);
                current.updated_at = now;
                true
            }
        };
        let reservation = current
            .account_handoff
            .clone()
            .expect("account handoff reservation was just validated");
        if reservation_created {
            store_receipt(
                &mut locked.registry,
                &record,
                &incarnation,
                &args.idempotency_key,
                "worker-account-handoff",
                &request_digest,
                account_handoff_progress(&reservation, "reserved"),
            )?;
        }
        locked.save()?;
        (reservation, reservation_created)
    };
    pause_account_handoff_for_test("after_reservation")?;
    let already_bound = initial_account.state == "bound"
        && initial_account.selected_account.as_deref() == Some(args.account.as_str())
        && initial_account.applied_runtime_id.as_deref()
            == Some(worker.session_incarnation.as_str())
        && initial_account.next.is_none();
    if !reservation_created && !already_bound {
        let reservation_intent_present = initial_next_identity.as_ref().is_some_and(|identity| {
            identity.account == reservation.account
                && identity.intent_id.as_deref() == reservation.account_intent_id.as_deref()
        });
        let reservation_not_yet_queued =
            initial_next_identity.is_none() && resume_stage.as_deref() == Some("reserved");
        if !reservation_intent_present && !reservation_not_yet_queued {
            return Err(CliError::data(
                "account-handoff-superseded",
                "a newer account intent superseded this handoff",
                None,
            ));
        }
    }

    ensure_account_handoff_authority(
        context,
        &record,
        &incarnation,
        &expected_run_id,
        &args.assignment_id,
        &worker,
        &reservation,
    )?;
    if !already_bound {
        match initial_account.next.as_ref() {
            Some(next)
                if next.account.as_deref() == Some(args.account.as_str())
                    && matches!(next.state, "queued" | "applying")
                    && initial_next_identity.as_ref().is_some_and(|identity| {
                        identity.intent_id.as_deref() == reservation.account_intent_id.as_deref()
                    }) => {}
            Some(next)
                if next.account.as_deref() == Some(args.account.as_str())
                    && next.state == "failed" =>
            {
                return Err(CliError::data(
                    "account-handoff-apply-failed",
                    "the exact managed account apply failed and remains durably fenced",
                    Some(json!({ "failure_reason": next.failure_reason })),
                ));
            }
            _ => {
                crate::codex_account::queue_next_account_if_unchanged(
                    context,
                    &worker.session_id,
                    &worker.session_incarnation,
                    &args.account,
                    initial_next_identity.as_ref(),
                    reservation.account_intent_id.as_deref().ok_or_else(|| {
                        CliError::data(
                            "account-handoff-v1-reservation",
                            "v1 account handoff reservation must be cancelled before retry",
                            None,
                        )
                    })?,
                )
                .map_err(|error| {
                    if error.code() == "codex-account-next-superseded" {
                        CliError::data(
                            "account-handoff-superseded",
                            "a newer account intent superseded this handoff",
                            None,
                        )
                    } else {
                        error
                    }
                })?;
            }
        }
    }
    persist_account_handoff_progress(
        context,
        &record,
        &incarnation,
        &args.idempotency_key,
        &request_digest,
        &reservation,
        "intent_queued",
    )?;
    drive_account_handoff_apply_for_test(context, &worker, &args.account)?;

    let started = Instant::now();
    loop {
        let current = load_session_record(context, &worker.session_id)?;
        let current_incarnation = current
            .runtime
            .as_ref()
            .map(|runtime| runtime.launch_id.as_str())
            .unwrap_or_default();
        if !orchestration::session_ref_matches(&worker, &current, current_incarnation) {
            return Err(CliError::data(
                "account-handoff-worker-incarnation-conflict",
                "worker incarnation changed while applying the account handoff",
                None,
            ));
        }
        let view = crate::codex_account::view_for_record(&current);
        let current_next_identity = crate::codex_account::next_account_identity(&current)?;
        if view.state == "bound"
            && view.selected_account.as_deref() == Some(args.account.as_str())
            && view.applied_runtime_id.as_deref() == Some(worker.session_incarnation.as_str())
            && view.next.is_none()
        {
            break;
        }
        if let Some(next) = view.next.as_ref() {
            if next.account.as_deref() != Some(args.account.as_str())
                || current_next_identity.as_ref().is_none_or(|identity| {
                    identity.intent_id.as_deref() != reservation.account_intent_id.as_deref()
                })
            {
                return Err(CliError::data(
                    "account-handoff-superseded",
                    "a newer account intent superseded this handoff",
                    None,
                ));
            }
            if next.state == "failed" {
                return Err(CliError::data(
                    "account-handoff-apply-failed",
                    "the exact managed account apply failed and remains durably fenced",
                    Some(json!({ "failure_reason": next.failure_reason })),
                ));
            }
        }
        if started.elapsed() >= timeout {
            return Err(CliError::runtime(
                "account-handoff-binding-timeout",
                "the managed account intent remains durably queued or applying",
                Some(json!({
                    "assignment_id": args.assignment_id,
                    "worker_session_id": worker.session_id,
                    "worker_session_incarnation": worker.session_incarnation
                })),
            ));
        }
        thread::sleep(Duration::from_millis(100));
    }

    ensure_account_handoff_authority(
        context,
        &record,
        &incarnation,
        &expected_run_id,
        &args.assignment_id,
        &worker,
        &reservation,
    )?;
    pause_account_handoff_for_test("before_auto_resume_rearm")?;
    let auto_resume_rearmed = if let Some((blocked_turn_id, blocked_revision)) = blocked_turn {
        let now = jiff::Timestamp::now().to_string();
        crate::auto_resume::rearm_usage_exhaustion_for_runtime(
            context,
            &worker.session_id,
            &worker.session_incarnation,
            blocked_turn_id,
            blocked_revision,
            &now,
        )
        .map_err(|error| {
            if error.code() == "auto-resume-runtime-changed" {
                CliError::data(
                    "account-handoff-worker-incarnation-conflict",
                    "worker incarnation changed before auto-resume rearm",
                    None,
                )
            } else {
                error
            }
        })?;
        true
    } else {
        false
    };
    let final_quiescence = crate::coordination::lock_session_quiescence(
        context,
        &worker.session_id,
        &worker.session_incarnation,
    )?;
    if !final_quiescence.broker_present
        || !final_quiescence.broker_identity_matched
        || !final_quiescence.broker_authoritative
        || final_quiescence.active_operation
        || final_quiescence.uncertain_operation
        || !final_quiescence.active_claim
    {
        return Err(CliError::runtime(
            "account-handoff-worker-authority-changed",
            "worker coordination authority changed while applying the account handoff",
            None,
        ));
    }
    let mut locked = orchestration::lock_registry(context)?;
    if let Some(value) = idempotency_replay(
        &locked.registry,
        &record,
        &incarnation,
        &args.idempotency_key,
        "worker-account-handoff",
        &request_digest,
    )? {
        if value["schema_version"] == "main-agent.worker-account-handoff.v1" {
            return Ok(value);
        }
        if value["schema_version"] != "main-agent.worker-account-handoff-progress.v1" {
            return Err(CliError::data(
                "idempotency-conflict",
                "account handoff receipt is not resumable",
                None,
            ));
        }
    }
    let current_run_id = require_current_main(&locked.registry, &record, &incarnation)?
        .run_id
        .clone();
    if current_run_id != expected_run_id {
        return Err(CliError::data(
            "account-handoff-assignment-conflict",
            "assignment ownership changed while applying the account handoff",
            None,
        ));
    }
    let current = locked
        .registry
        .assignments
        .get_mut(&args.assignment_id)
        .filter(|assignment| assignment.run_id == expected_run_id)
        .ok_or_else(|| not_found("assignment-not-found", "assignment was not found"))?;
    ensure_primary_manager(current, &record, &incarnation)?;
    if reservation_assignment_revision(&reservation) != Some(current.revision)
        || current.worker.as_ref() != Some(&worker)
        || current.account_handoff.as_ref().is_none_or(|reservation| {
            !account_handoff_reservation_matches(
                reservation,
                &request_digest,
                &expected_run_id,
                &current.primary_manager,
                &worker,
                expected_revision,
                &args.account,
            )
        })
    {
        return Err(CliError::data(
            "account-handoff-assignment-conflict",
            "assignment identity or reservation changed while applying the account handoff",
            None,
        ));
    }
    current.account_handoff = None;
    current.revision = current.revision.saturating_add(1);
    current.updated_at = timestamp();
    let outcome = json!({
        "schema_version": "main-agent.worker-account-handoff.v1",
        "assignment_id": args.assignment_id,
        "assignment_revision": current.revision,
        "worker_session_id": worker.session_id,
        "worker_session_incarnation": worker.session_incarnation,
        "account": args.account,
        "state": "bound",
        "managed": true,
        "auto_resume_rearmed": auto_resume_rearmed,
        "provider_resume_preserved": worker_record.provider_resume.is_some(),
        "forbidden_side_effects": {
            "logout_used": false,
            "prompt_resent": false,
            "blind_enter_sent": false,
            "duplicate_worker_created": false,
            "provider_conversation_replaced": false
        }
    });
    store_receipt(
        &mut locked.registry,
        &record,
        &incarnation,
        &args.idempotency_key,
        "worker-account-handoff",
        &request_digest,
        outcome.clone(),
    )?;
    locked.save()?;
    drop(final_quiescence);
    Ok(outcome)
}

fn run_worker_account_handoff_cancel(
    context: &CliContext,
    args: WorkerAccountHandoffCancelArgs,
) -> Result<Value, CliError> {
    validate_idempotency_key(&args.idempotency_key)?;
    crate::codex_account::validate_account(&args.account)?;
    orchestration::validate_slug("account handoff reservation id", &args.reservation_id, 128)?;
    if let Some(intent_id) = args.intent_id.as_deref() {
        orchestration::validate_slug("account intent id", intent_id, 128)?;
    }
    if !args.authorize_account_change {
        return Err(CliError::usage(
            "account-handoff-cancel-authorization-required",
            "account handoff cancellation requires --authorize-account-change for the exact reserved intent",
            None,
        ));
    }
    let request_digest = crate::coordination::request_digest(
        "worker-account-handoff-cancel",
        &json!({
            "assignment_id": args.assignment_id,
            "reservation_id": args.reservation_id,
            "account": args.account,
            "intent_id": args.intent_id,
            "if_revision": args.if_revision,
            "authorize_account_change": args.authorize_account_change
        }),
    );
    let (record, incarnation) = authenticated_self(context)?;
    ensure_active_claim(context, &record)?;
    let registry = orchestration::load_registry_readonly(context)?;
    let run = require_current_main(&registry, &record, &incarnation)?;
    if let Some(value) = idempotency_replay(
        &registry,
        &record,
        &incarnation,
        &args.idempotency_key,
        "worker-account-handoff-cancel",
        &request_digest,
    )? {
        return Ok(value);
    }
    let assignment = registry
        .assignments
        .get(&args.assignment_id)
        .filter(|assignment| assignment.run_id == run.run_id)
        .ok_or_else(|| not_found("assignment-not-found", "assignment was not found"))?;
    ensure_primary_manager(assignment, &record, &incarnation)?;
    ensure_revision(args.if_revision, assignment.revision, "assignment")?;
    let reservation = assignment
        .account_handoff
        .as_ref()
        .cloned()
        .ok_or_else(|| {
            CliError::data(
                "account-handoff-reservation-unavailable",
                "the assignment has no account handoff reservation to cancel",
                None,
            )
        })?;
    if account_handoff_reservation_identity(&reservation) != args.reservation_id
        || reservation.account != args.account
        || reservation.account_intent_id != args.intent_id
    {
        return Err(CliError::data(
            "account-handoff-cancel-reservation-conflict",
            "account handoff cancellation must name the exact reservation, account, and managed intent identity",
            None,
        ));
    }
    let worker = assignment
        .worker
        .as_ref()
        .filter(|worker| **worker == reservation.worker)
        .cloned()
        .ok_or_else(|| {
            CliError::data(
                "account-handoff-assignment-conflict",
                "the reserved account handoff worker no longer matches the assignment",
                None,
            )
        })?;
    let expected_run_id = run.run_id.clone();
    drop(registry);

    let worker_record = load_session_record(context, &worker.session_id)?;
    let worker_incarnation = worker_record
        .runtime
        .as_ref()
        .map(|runtime| runtime.launch_id.as_str())
        .unwrap_or_default();
    if !orchestration::session_ref_matches(&worker, &worker_record, worker_incarnation) {
        return Err(CliError::data(
            "worker-session-conflict",
            "the reserved account handoff worker changed before cancellation",
            None,
        ));
    }
    let quiescence = crate::coordination::lock_session_quiescence(
        context,
        &worker.session_id,
        &worker.session_incarnation,
    )?;
    if !quiescence.broker_present
        || !quiescence.broker_identity_matched
        || !quiescence.broker_authoritative
    {
        return Err(CliError::runtime(
            "account-handoff-worker-authority-unavailable",
            "account handoff cancellation requires the exact worker broker to be authoritative",
            None,
        ));
    }
    if quiescence.active_operation || quiescence.uncertain_operation {
        return Err(CliError::data(
            "account-handoff-operation-fenced",
            "account handoff cancellation refuses an active or uncertain worker mutation",
            None,
        ));
    }
    if !quiescence.active_claim {
        return Err(CliError::data(
            "account-handoff-claim-unavailable",
            "account handoff cancellation requires the exact worker claim to remain active",
            None,
        ));
    }
    // Do not hold the coordination registry while acquiring or mutating the
    // session record. This matches group cleanup's global lock order.
    drop(quiescence);

    let before = crate::codex_account::view_for_record(&worker_record);
    let expected_next_identity = crate::codex_account::next_account_identity(&worker_record)?;
    if !before.supported {
        return Err(CliError::data(
            "account-handoff-capability-unavailable",
            "managed account control is unavailable for the reserved worker",
            None,
        ));
    }
    if before.state == "bound"
        && before.selected_account.as_deref() == Some(reservation.account.as_str())
        && before.next.is_none()
    {
        return Err(CliError::data(
            "account-handoff-already-applied",
            "the reserved account is already bound; retry the original account-handoff request so auto-resume and its receipt can converge",
            None,
        ));
    }
    let reservation_owns_next = expected_next_identity.as_ref().is_some_and(|identity| {
        identity.account == reservation.account
            && identity.intent_id.as_deref() == reservation.account_intent_id.as_deref()
            && reservation.account_intent_id.is_some()
    });
    if reservation_owns_next
        && before
            .next
            .as_ref()
            .is_some_and(|next| next.state == "applying")
    {
        return Err(CliError::data(
            "account-handoff-apply-active",
            "the reserved account intent is still applying; cancellation is unsafe until it becomes queued or failed",
            None,
        ));
    }
    let expected_next_account = if reservation_owns_next {
        match before.next.as_ref() {
            Some(next) => Some(next.account.as_deref().ok_or_else(|| {
                CliError::data(
                    "account-handoff-cancel-intent-invalid",
                    "the pending managed account intent has no exact account identity",
                    None,
                )
            })?),
            None => None,
        }
    } else {
        None
    };
    let newer_account_intent_preserved = expected_next_identity.is_some() && !reservation_owns_next;
    let preserved_account_intent = if newer_account_intent_preserved {
        before
            .next
            .as_ref()
            .map(|next| {
                next.account.as_deref().ok_or_else(|| {
                    CliError::data(
                        "account-handoff-cancel-intent-invalid",
                        "the pending managed account intent has no exact account identity",
                        None,
                    )
                })
            })
            .transpose()?
    } else {
        None
    };
    {
        let locked = orchestration::lock_registry(context)?;
        let current_run = require_current_main(&locked.registry, &record, &incarnation)?;
        let current = locked
            .registry
            .assignments
            .get(&args.assignment_id)
            .filter(|assignment| assignment.run_id == expected_run_id)
            .ok_or_else(|| not_found("assignment-not-found", "assignment was not found"))?;
        if current_run.run_id != expected_run_id
            || current.revision != args.if_revision
            || current.primary_manager != reservation.controller
            || current.worker.as_ref() != Some(&worker)
            || current.account_handoff.as_ref() != Some(&reservation)
        {
            return Err(CliError::data(
                "account-handoff-assignment-conflict",
                "assignment identity or reservation changed before account cancellation",
                None,
            ));
        }
    }
    pause_account_handoff_for_test("before_cancel")?;
    let after = if reservation_owns_next {
        crate::codex_account::cancel_next_account_if_matches(
            context,
            &worker.session_id,
            &worker.session_incarnation,
            expected_next_identity.as_ref(),
        )?
    } else {
        crate::codex_account::view_for_record(&load_session_record(context, &worker.session_id)?)
    };
    if after.selected_account != before.selected_account
        || (reservation_owns_next && after.next.is_some())
        || (newer_account_intent_preserved && after.next != before.next)
    {
        return Err(CliError::runtime(
            "account-handoff-cancel-verification-failed",
            "the managed account cancellation did not preserve the bound account and clear the pending intent",
            None,
        ));
    }
    let mut outcome = json!({
        "schema_version": "main-agent.worker-account-handoff-cancel.v1",
        "assignment_id": args.assignment_id,
        "assignment_revision": args.if_revision,
        "worker_session_id": worker.session_id,
        "worker_session_incarnation": worker.session_incarnation,
        "reserved_account": reservation.account,
        "cancelled_pending_account": expected_next_account,
        "newer_account_intent_preserved": newer_account_intent_preserved,
        "legacy_reservation_recovered": reservation.account_intent_id.is_none(),
        "preserved_pending_account": preserved_account_intent,
        "selected_account": after.selected_account,
        "state": "cancelled",
        "account_changed": false,
        "auto_resume_rearmed": false,
        "forbidden_side_effects": {
            "logout_used": false,
            "prompt_resent": false,
            "blind_enter_sent": false,
            "runtime_restarted": false,
            "worker_replaced": false
        }
    });
    let final_quiescence = crate::coordination::lock_session_quiescence(
        context,
        &worker.session_id,
        &worker.session_incarnation,
    )?;
    if !final_quiescence.broker_present
        || !final_quiescence.broker_identity_matched
        || !final_quiescence.broker_authoritative
        || final_quiescence.active_operation
        || final_quiescence.uncertain_operation
        || !final_quiescence.active_claim
    {
        return Err(CliError::runtime(
            "account-handoff-worker-authority-changed",
            "worker coordination authority changed while cancelling the account handoff",
            None,
        ));
    }
    let mut locked = orchestration::lock_registry(context)?;
    if let Some(value) = idempotency_replay(
        &locked.registry,
        &record,
        &incarnation,
        &args.idempotency_key,
        "worker-account-handoff-cancel",
        &request_digest,
    )? {
        return Ok(value);
    }
    let current_run_id = require_current_main(&locked.registry, &record, &incarnation)?
        .run_id
        .clone();
    let current = locked
        .registry
        .assignments
        .get_mut(&args.assignment_id)
        .filter(|assignment| assignment.run_id == expected_run_id)
        .ok_or_else(|| not_found("assignment-not-found", "assignment was not found"))?;
    if current_run_id != expected_run_id
        || current.revision != args.if_revision
        || current.primary_manager != reservation.controller
        || current.worker.as_ref() != Some(&worker)
        || current.account_handoff.as_ref() != Some(&reservation)
    {
        return Err(CliError::data(
            "account-handoff-assignment-conflict",
            "assignment identity or reservation changed while cancelling the account handoff",
            None,
        ));
    }
    current.account_handoff = None;
    current.revision = current.revision.saturating_add(1);
    current.updated_at = timestamp();
    outcome["assignment_revision"] = json!(current.revision);
    store_receipt(
        &mut locked.registry,
        &record,
        &incarnation,
        &args.idempotency_key,
        "worker-account-handoff-cancel",
        &request_digest,
        outcome.clone(),
    )?;
    locked.save()?;
    drop(final_quiescence);
    Ok(outcome)
}

fn account_handoff_reservation_matches(
    reservation: &AccountHandoffReservationRecord,
    request_digest: &str,
    run_id: &str,
    controller: &SessionRef,
    worker: &SessionRef,
    revision: u64,
    account: &str,
) -> bool {
    matches!(
        reservation.schema_version.as_str(),
        ACCOUNT_HANDOFF_RESERVATION_SCHEMA | LEGACY_ACCOUNT_HANDOFF_RESERVATION_V2_SCHEMA
    ) && reservation.account_intent_id.is_some()
        && reservation.request_digest == request_digest
        && reservation.run_id == run_id
        && &reservation.controller == controller
        && &reservation.worker == worker
        && reservation.reserved_revision == revision
        && reservation.account == account
}

fn reservation_assignment_revision(reservation: &AccountHandoffReservationRecord) -> Option<u64> {
    if reservation.schema_version == ACCOUNT_HANDOFF_RESERVATION_SCHEMA {
        reservation.reserved_revision.checked_add(1)
    } else {
        Some(reservation.reserved_revision)
    }
}

fn account_handoff_reservation_identity(reservation: &AccountHandoffReservationRecord) -> &str {
    reservation
        .reservation_id
        .as_deref()
        .unwrap_or(&reservation.request_digest)
}

fn account_handoff_progress(reservation: &AccountHandoffReservationRecord, stage: &str) -> Value {
    json!({
        "schema_version": "main-agent.worker-account-handoff-progress.v1",
        "state": "in_progress",
        "stage": stage,
        "reservation_id": account_handoff_reservation_identity(reservation),
        "assignment_revision": reservation_assignment_revision(reservation)
    })
}

#[allow(clippy::too_many_arguments)]
fn persist_account_handoff_progress(
    context: &CliContext,
    controller: &SessionRecord,
    controller_incarnation: &str,
    idempotency_key: &str,
    request_digest: &str,
    reservation: &AccountHandoffReservationRecord,
    stage: &str,
) -> Result<(), CliError> {
    let mut locked = orchestration::lock_registry(context)?;
    if let Some(existing) = idempotency_replay(
        &locked.registry,
        controller,
        controller_incarnation,
        idempotency_key,
        "worker-account-handoff",
        request_digest,
    )? && existing["schema_version"] == "main-agent.worker-account-handoff.v1"
    {
        return Ok(());
    }
    let current = locked
        .registry
        .assignments
        .values()
        .find(|assignment| assignment.account_handoff.as_ref() == Some(reservation))
        .ok_or_else(|| {
            CliError::data(
                "account-handoff-assignment-conflict",
                "account handoff reservation changed before progress could be recorded",
                None,
            )
        })?;
    let _ = current;
    store_receipt(
        &mut locked.registry,
        controller,
        controller_incarnation,
        idempotency_key,
        "worker-account-handoff",
        request_digest,
        account_handoff_progress(reservation, stage),
    )?;
    locked.save()
}

fn ensure_account_handoff_authority(
    context: &CliContext,
    controller: &SessionRecord,
    controller_incarnation: &str,
    run_id: &str,
    assignment_id: &str,
    worker: &SessionRef,
    reservation: &AccountHandoffReservationRecord,
) -> Result<(), CliError> {
    let quiescence = crate::coordination::lock_session_quiescence(
        context,
        &worker.session_id,
        &worker.session_incarnation,
    )?;
    if !quiescence.broker_present
        || !quiescence.broker_identity_matched
        || !quiescence.broker_authoritative
        || !quiescence.active_claim
        || quiescence.active_operation
        || quiescence.uncertain_operation
    {
        return Err(CliError::runtime(
            "account-handoff-worker-authority-changed",
            "the exact worker coordination authority changed at an account-handoff side-effect boundary",
            None,
        ));
    }
    drop(quiescence);
    let locked = orchestration::lock_registry(context)?;
    let current_run = require_current_main(&locked.registry, controller, controller_incarnation)?;
    let current = locked
        .registry
        .assignments
        .get(assignment_id)
        .filter(|assignment| assignment.run_id == run_id)
        .ok_or_else(|| not_found("assignment-not-found", "assignment was not found"))?;
    if current_run.run_id != run_id
        || current.primary_manager != reservation.controller
        || current.worker.as_ref() != Some(worker)
        || current.account_handoff.as_ref() != Some(reservation)
        || reservation_assignment_revision(reservation) != Some(current.revision)
    {
        return Err(CliError::data(
            "account-handoff-assignment-conflict",
            "assignment identity or reservation changed at an account-handoff side-effect boundary",
            None,
        ));
    }
    Ok(())
}

fn account_handoff_in_flight(assignment: &AssignmentRecord) -> CliError {
    CliError::data(
        "account-handoff-in-flight",
        "assignment mutation is fenced until the reserved account handoff is resolved",
        Some(json!({
            "assignment_id": assignment.assignment_id,
            "revision": assignment.revision
        })),
    )
}

fn ensure_account_handoff_not_in_flight(assignment: &AssignmentRecord) -> Result<(), CliError> {
    if assignment.account_handoff.is_some() {
        return Err(account_handoff_in_flight(assignment));
    }
    Ok(())
}

fn ensure_account_handoff_eligible_state(assignment: &AssignmentRecord) -> Result<(), CliError> {
    if !matches!(
        assignment.state.as_str(),
        "starting" | "working" | "blocked"
    ) {
        return Err(CliError::data(
            "account-handoff-assignment-state",
            "account handoff requires a starting, working, or blocked assignment",
            Some(json!({
                "assignment_id": assignment.assignment_id,
                "state": assignment.state,
                "revision": assignment.revision
            })),
        ));
    }
    Ok(())
}

fn pause_account_handoff_for_test(stage: &str) -> Result<(), CliError> {
    #[cfg(debug_assertions)]
    if env::var("NILS_AGENT_SESSION_TEST_ACCOUNT_HANDOFF_BARRIER_STAGE").as_deref() == Ok(stage)
        && let Some(directory) =
            env::var_os("NILS_AGENT_SESSION_TEST_ACCOUNT_HANDOFF_BARRIER_DIR").map(PathBuf::from)
    {
        fs::write(directory.join("ready"), stage.as_bytes()).map_err(|_| {
            CliError::runtime(
                "test-barrier-unavailable",
                "account handoff test barrier could not be signalled",
                None,
            )
        })?;
        let release = directory.join("release");
        let deadline = Instant::now() + Duration::from_secs(10);
        while !release.is_file() {
            if Instant::now() >= deadline {
                return Err(CliError::runtime(
                    "test-barrier-timeout",
                    "account handoff test barrier timed out",
                    None,
                ));
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
    Ok(())
}

fn drive_account_handoff_apply_for_test(
    context: &CliContext,
    worker: &SessionRef,
    account: &str,
) -> Result<(), CliError> {
    #[cfg(debug_assertions)]
    match env::var("NILS_AGENT_SESSION_TEST_ACCOUNT_HANDOFF_APPLY_RESULT").as_deref() {
        Ok("success") | Ok("failed") => {
            if let Some(next) = crate::codex_account::begin_next_apply(
                context,
                &worker.session_id,
                &worker.session_incarnation,
            )? {
                let intent_id = next.intent_id.as_deref().ok_or_else(|| {
                    CliError::data(
                        "codex-account-intent-id-invalid",
                        "managed account handoff apply intent identity is unavailable",
                        None,
                    )
                })?;
                let result = if env::var("NILS_AGENT_SESSION_TEST_ACCOUNT_HANDOFF_APPLY_RESULT")
                    .as_deref()
                    == Ok("success")
                {
                    Ok(())
                } else {
                    Err("test-managed-account-apply-failed")
                };
                crate::codex_account::finish_next_apply(
                    context,
                    &worker.session_id,
                    &worker.session_incarnation,
                    &next.account,
                    next.revision,
                    intent_id,
                    result,
                )?;
            }
        }
        Ok("superseded") => {
            crate::codex_account::queue_next_account(
                context,
                &worker.session_id,
                &worker.session_incarnation,
                "test-superseding-account",
            )?;
        }
        Ok("timeout") | Err(_) => {}
        Ok(_) => {
            return Err(CliError::runtime(
                "test-account-handoff-driver-invalid",
                "account handoff test apply result is invalid",
                None,
            ));
        }
    }
    let _ = (context, worker, account);
    Ok(())
}

fn pause_message_after_routing_read_for_test(
    assignment_id: &str,
    worker: &SessionRef,
) -> Result<(), CliError> {
    #[cfg(debug_assertions)]
    if let Some(directory) =
        env::var_os("NILS_AGENT_SESSION_TEST_MESSAGE_ROUTING_BARRIER_DIR").map(PathBuf::from)
    {
        let expected_assignment = env::var("NILS_AGENT_SESSION_TEST_MESSAGE_ROUTING_ASSIGNMENT")
            .map_err(|_| {
                CliError::runtime(
                    "test-barrier-unavailable",
                    "message routing test barrier assignment is unavailable",
                    None,
                )
            })?;
        let expected_worker =
            env::var("NILS_AGENT_SESSION_TEST_MESSAGE_ROUTING_WORKER").map_err(|_| {
                CliError::runtime(
                    "test-barrier-unavailable",
                    "message routing test barrier worker is unavailable",
                    None,
                )
            })?;
        if expected_assignment != assignment_id
            || expected_worker != format!("{}@{}", worker.session_id, worker.session_incarnation)
        {
            return Ok(());
        }
        let ready = directory.join("ready");
        let release = directory.join("release");
        fs::write(&ready, b"routing-read-complete").map_err(|_| {
            CliError::runtime(
                "test-barrier-unavailable",
                "message routing test barrier could not be signalled",
                None,
            )
        })?;
        let deadline = Instant::now() + Duration::from_secs(10);
        while !release.is_file() {
            if Instant::now() >= deadline {
                return Err(CliError::runtime(
                    "test-barrier-timeout",
                    "message routing test barrier timed out",
                    None,
                ));
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
    Ok(())
}

fn run_assignment_state(
    context: &CliContext,
    args: AssignmentMutationArgs,
    expected: &str,
    next: &str,
    operation: &str,
) -> Result<Value, CliError> {
    validate_idempotency_key(&args.idempotency_key)?;
    let (record, incarnation) = authenticated_self(context)?;
    ensure_active_claim(context, &record)?;
    let mut locked = orchestration::lock_registry(context)?;
    let run = require_current_main(&locked.registry, &record, &incarnation)?.clone();
    let request_digest = crate::coordination::request_digest(
        operation,
        &json!({ "assignment_id": args.assignment_id, "if_revision": args.if_revision }),
    );
    if let Some(value) = idempotency_replay(
        &locked.registry,
        &record,
        &incarnation,
        &args.idempotency_key,
        operation,
        &request_digest,
    )? {
        return Ok(value);
    }
    let current = locked
        .registry
        .assignments
        .get_mut(&args.assignment_id)
        .filter(|assignment| assignment.run_id == run.run_id)
        .ok_or_else(|| not_found("assignment-not-found", "assignment was not found"))?;
    ensure_primary_manager(current, &record, &incarnation)?;
    ensure_revision(args.if_revision, current.revision, "assignment")?;
    ensure_account_handoff_not_in_flight(current)?;
    if current.state != expected {
        return Err(CliError::data(
            "assignment-state-conflict",
            format!("assignment must be {expected} before {next}"),
            Some(
                json!({ "assignment_id": current.assignment_id, "state": current.state, "revision": current.revision }),
            ),
        ));
    }
    current.state = next.to_string();
    current.revision = current.revision.saturating_add(1);
    current.updated_at = timestamp();
    let outcome = json!({
        "schema_version": "main-agent.assignment-mutation-result.v1",
        "assignment": public_assignment_view(current)
    });
    store_receipt(
        &mut locked.registry,
        &record,
        &incarnation,
        &args.idempotency_key,
        operation,
        &request_digest,
        outcome.clone(),
    )?;
    locked.save()?;
    Ok(outcome)
}

fn run_worker_request_changes(
    context: &CliContext,
    args: WorkerRequestChangesArgs,
) -> Result<Value, CliError> {
    validate_idempotency_key(&args.idempotency_key)?;
    orchestration::validate_summary("request-changes reason", &args.reason)
        .map_err(|_| invalid_input("request-changes reason is invalid"))?;
    let (record, incarnation) = authenticated_self(context)?;
    ensure_active_claim(context, &record)?;
    let mut locked = orchestration::lock_registry(context)?;
    let run = require_current_main(&locked.registry, &record, &incarnation)?.clone();
    let request_digest = crate::coordination::request_digest(
        "worker-request-changes",
        &json!({
            "assignment_id": args.assignment_id,
            "if_revision": args.if_revision,
            "reason": args.reason
        }),
    );
    if let Some(value) = idempotency_replay(
        &locked.registry,
        &record,
        &incarnation,
        &args.idempotency_key,
        "worker-request-changes",
        &request_digest,
    )? {
        return Ok(value);
    }
    let current = locked
        .registry
        .assignments
        .get_mut(&args.assignment_id)
        .filter(|assignment| assignment.run_id == run.run_id)
        .ok_or_else(|| not_found("assignment-not-found", "assignment was not found"))?;
    ensure_primary_manager(current, &record, &incarnation)?;
    ensure_revision(args.if_revision, current.revision, "assignment")?;
    ensure_account_handoff_not_in_flight(current)?;
    if current.state != "submitted" {
        return Err(CliError::data(
            "assignment-state-conflict",
            "assignment must be submitted before Main Agent can request changes",
            Some(json!({
                "assignment_id": current.assignment_id,
                "state": current.state,
                "revision": current.revision
            })),
        ));
    }
    let next_revision = current.revision.checked_add(1).ok_or_else(|| {
        CliError::data(
            "orchestration-revision-capacity",
            "assignment revision reached its maximum value",
            Some(json!({
                "assignment_id": current.assignment_id,
                "current_revision": current.revision
            })),
        )
    })?;
    current.state = "working".to_string();
    current.revision = next_revision;
    current.result_summary = None;
    current.blocker_summary = None;
    current.updated_at = timestamp();
    current.checkpoint = Some(RunCheckpoint {
        revision: current.revision,
        summary: "Main Agent requested revisions".to_string(),
        next_action: args.reason,
        updated_at: current.updated_at.clone(),
    });
    let outcome = json!({
        "schema_version": "main-agent.worker-request-changes-result.v1",
        "assignment": public_assignment_view(current)
    });
    store_receipt(
        &mut locked.registry,
        &record,
        &incarnation,
        &args.idempotency_key,
        "worker-request-changes",
        &request_digest,
        outcome.clone(),
    )?;
    locked.save()?;
    Ok(outcome)
}

fn run_worker_delete(
    context: &CliContext,
    args: AssignmentMutationArgs,
) -> Result<Value, CliError> {
    validate_idempotency_key(&args.idempotency_key)?;
    let (record, incarnation) = authenticated_self(context)?;
    ensure_active_claim(context, &record)?;
    let request_digest = crate::coordination::request_digest(
        "worker-delete",
        &json!({ "assignment_id": args.assignment_id, "if_revision": args.if_revision }),
    );
    let locked = orchestration::lock_registry(context)?;
    let pending_worker = match idempotency_replay(
        &locked.registry,
        &record,
        &incarnation,
        &args.idempotency_key,
        "worker-delete",
        &request_digest,
    )? {
        Some(value) if worker_delete_is_pending(&value) => Some(
            serde_json::from_value::<SessionRef>(value["worker"].clone())
                .map_err(|_| invalid_input("pending worker delete receipt is invalid"))?,
        ),
        Some(value) => return Ok(value),
        None => None,
    };
    let run = require_current_main(&locked.registry, &record, &incarnation)?;
    let assignment = locked
        .registry
        .assignments
        .get(&args.assignment_id)
        .filter(|assignment| assignment.run_id == run.run_id)
        .ok_or_else(|| not_found("assignment-not-found", "assignment was not found"))?
        .clone();
    ensure_primary_manager(&assignment, &record, &incarnation)?;
    ensure_revision(args.if_revision, assignment.revision, "assignment")?;
    if !matches!(assignment.state.as_str(), "released" | "cancelled") {
        return Err(CliError::data(
            "assignment-not-terminal",
            "worker delete requires a released or cancelled assignment",
            Some(json!({ "assignment_id": assignment.assignment_id, "state": assignment.state })),
        ));
    }
    if assignment.worker.is_none() {
        if pending_worker.is_some() {
            return Err(CliError::data(
                "assignment-delete-conflict",
                "pending worker delete receipt names a worker but the assignment has none",
                None,
            ));
        }
        drop(locked);
        return finalize_worker_delete(
            context,
            &args,
            &record,
            &incarnation,
            &request_digest,
            true,
            false,
        );
    }
    let worker = assignment
        .worker
        .clone()
        .ok_or_else(|| not_found("worker-not-started", "worker session is not available"))?;
    if pending_worker
        .as_ref()
        .is_some_and(|pending| pending != &worker)
    {
        return Err(CliError::data(
            "assignment-delete-conflict",
            "pending worker delete receipt does not match the assignment worker",
            None,
        ));
    }
    drop(locked);

    let session_path = session_dir(context, &worker.session_id);
    match fs::symlink_metadata(&session_path) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            if pending_worker.is_none() {
                reserve_worker_delete(
                    context,
                    &args,
                    &record,
                    &incarnation,
                    &request_digest,
                    &worker,
                )?;
            }
            let cleanup_pending = worker_delete_tombstone_exists(context, &worker)?;
            return finalize_worker_delete(
                context,
                &args,
                &record,
                &incarnation,
                &request_digest,
                true,
                cleanup_pending,
            );
        }
        Err(_) => {
            return Err(CliError::runtime(
                "orchestration-store-unavailable",
                "worker session state is unavailable",
                None,
            ));
        }
    }
    let worker_record = load_session_record(context, &worker.session_id)?;
    if !orchestration::session_ref_matches(
        &worker,
        &worker_record,
        worker_record
            .runtime
            .as_ref()
            .map(|runtime| runtime.launch_id.as_str())
            .unwrap_or_default(),
    ) {
        return Err(CliError::data(
            "session-incarnation-conflict",
            "worker session identity changed before deletion",
            None,
        ));
    }
    let (active_claim, active_operation) =
        crate::coordination::session_has_active_claim_or_operation(
            context,
            &worker.session_id,
            &worker.session_incarnation,
        )?;
    if active_claim || active_operation {
        return Err(CliError::data(
            "worker-not-quiescent",
            "worker claim or mutation operation remains active",
            Some(
                json!({ "assignment_id": assignment.assignment_id, "active_claim": active_claim, "active_operation": active_operation }),
            ),
        ));
    }
    if pending_worker.is_none() {
        reserve_worker_delete(
            context,
            &args,
            &record,
            &incarnation,
            &request_digest,
            &worker,
        )?;
    }
    let deleted = delete_session(context, &worker.session_id, resolve_tmux_bin(None))?;
    finalize_worker_delete(
        context,
        &args,
        &record,
        &incarnation,
        &request_digest,
        deleted.deleted,
        deleted.cleanup_pending,
    )
}

fn worker_delete_is_pending(value: &Value) -> bool {
    value["schema_version"] == "main-agent.worker-delete-pending.v1"
}

fn reserve_worker_delete(
    context: &CliContext,
    args: &AssignmentMutationArgs,
    record: &SessionRecord,
    incarnation: &str,
    request_digest: &str,
    worker: &SessionRef,
) -> Result<(), CliError> {
    let mut locked = orchestration::lock_registry(context)?;
    if let Some(value) = idempotency_replay(
        &locked.registry,
        record,
        incarnation,
        &args.idempotency_key,
        "worker-delete",
        request_digest,
    )? {
        if worker_delete_is_pending(&value) {
            return Ok(());
        }
        return Err(CliError::data(
            "assignment-delete-conflict",
            "worker delete already completed with a different continuation state",
            None,
        ));
    }
    let run = require_current_main(&locked.registry, record, incarnation)?;
    let current = locked
        .registry
        .assignments
        .get(&args.assignment_id)
        .filter(|assignment| assignment.run_id == run.run_id)
        .ok_or_else(|| not_found("assignment-not-found", "assignment was not found"))?;
    ensure_primary_manager(current, record, incarnation)?;
    ensure_revision(args.if_revision, current.revision, "assignment")?;
    if current.worker.as_ref() != Some(worker) {
        return Err(CliError::data(
            "assignment-delete-conflict",
            "assignment worker changed before deletion",
            None,
        ));
    }
    let pending = json!({
        "schema_version": "main-agent.worker-delete-pending.v1",
        "assignment_id": args.assignment_id,
        "worker": worker
    });
    store_receipt(
        &mut locked.registry,
        record,
        incarnation,
        &args.idempotency_key,
        "worker-delete",
        request_digest,
        pending,
    )?;
    locked.save()
}

#[allow(clippy::too_many_arguments)]
fn finalize_worker_delete(
    context: &CliContext,
    args: &AssignmentMutationArgs,
    record: &SessionRecord,
    incarnation: &str,
    request_digest: &str,
    deleted: bool,
    cleanup_pending: bool,
) -> Result<Value, CliError> {
    let mut locked = orchestration::lock_registry(context)?;
    if let Some(value) = idempotency_replay(
        &locked.registry,
        record,
        incarnation,
        &args.idempotency_key,
        "worker-delete",
        request_digest,
    )? && !worker_delete_is_pending(&value)
    {
        return Ok(value);
    }
    let run_id = require_current_main(&locked.registry, record, incarnation)?
        .run_id
        .clone();
    let current = locked
        .registry
        .assignments
        .get_mut(&args.assignment_id)
        .filter(|assignment| assignment.run_id == run_id)
        .ok_or_else(|| not_found("assignment-not-found", "assignment was not found"))?;
    ensure_primary_manager(current, record, incarnation)?;
    ensure_revision(args.if_revision, current.revision, "assignment")?;
    current.revision = current.revision.saturating_add(1);
    current.updated_at = timestamp();
    let assignment_view = public_assignment_view(current);
    // T2 fast-path: an ephemeral run (created by `quick`) auto-closes once its
    // last worker is torn down, so the caller never runs an explicit `close`.
    let run_closed = maybe_autoclose_ephemeral_run(&mut locked.registry, &run_id);
    let outcome = json!({
        "schema_version": "main-agent.worker-delete-result.v1",
        "assignment": assignment_view,
        "deleted": deleted,
        "cleanup_pending": cleanup_pending,
        "run_closed": run_closed
    });
    store_receipt(
        &mut locked.registry,
        record,
        incarnation,
        &args.idempotency_key,
        "worker-delete",
        request_digest,
        outcome.clone(),
    )?;
    locked.save()?;
    Ok(outcome)
}

fn worker_delete_tombstone_exists(
    context: &CliContext,
    worker: &SessionRef,
) -> Result<bool, CliError> {
    let root = context.state_dir.join("session-delete-tombstones");
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(_) => {
            return Err(CliError::runtime(
                "orchestration-store-unavailable",
                "worker delete tombstone state is unavailable",
                None,
            ));
        }
    };
    let prefix = format!("{}-", worker.session_id);
    let mut mismatched_incarnation = false;
    for entry in entries.filter_map(Result::ok) {
        if !entry.file_name().to_string_lossy().starts_with(&prefix) {
            continue;
        }
        let record: SessionRecord =
            serde_json::from_slice(&fs::read(entry.path().join("session.json")).map_err(|_| {
                CliError::runtime(
                    "orchestration-store-unavailable",
                    "worker delete tombstone state is unavailable",
                    None,
                )
            })?)
            .map_err(|_| invalid_input("worker delete tombstone is invalid"))?;
        let incarnation = record
            .runtime
            .as_ref()
            .map(|runtime| runtime.launch_id.as_str())
            .unwrap_or_default();
        if orchestration::session_ref_matches(worker, &record, incarnation) {
            return Ok(true);
        }
        mismatched_incarnation = true;
    }
    if mismatched_incarnation {
        return Err(CliError::data(
            "session-incarnation-conflict",
            "worker delete tombstones belong to a different session incarnation",
            None,
        ));
    }
    Ok(false)
}

fn run_collaborate(context: &CliContext, args: RelationshipArgs) -> Result<Value, CliError> {
    let target = resolve_live_session_ref(context, &args.session)?;
    run_relationship_mutation(
        context,
        args.assignment_id,
        args.if_revision,
        args.idempotency_key,
        "collaborate",
        |assignment| {
            if assignment.collaborators.len() >= 16 {
                return Err(invalid_input("collaborator limit exceeded"));
            }
            if !assignment.collaborators.contains(&target) {
                assignment.collaborators.push(target.clone());
                assignment
                    .collaborators
                    .sort_by(|a, b| a.session_id.cmp(&b.session_id));
            }
            Ok(())
        },
    )
}

fn run_borrow(context: &CliContext, args: BorrowArgs) -> Result<Value, CliError> {
    let target = resolve_live_session_ref(context, &args.session)?;
    let seconds = parse_bounded_duration(&args.duration, 8 * 60 * 60)?;
    let now = crate::coordination::now_epoch();
    let expires_at_epoch = now.saturating_add(seconds as i64);
    run_relationship_mutation(
        context,
        args.assignment_id,
        args.if_revision,
        args.idempotency_key,
        "borrow",
        |assignment| {
            assignment.borrowed_by.retain(|relationship| {
                relationship.expires_at_epoch > now && relationship.session != target
            });
            if assignment.borrowed_by.len() >= 16 {
                return Err(invalid_input("borrower limit exceeded"));
            }
            assignment.borrowed_by.push(TimedRelationship {
                session: target.clone(),
                expires_at: crate::coordination::timestamp(expires_at_epoch),
                expires_at_epoch,
            });
            assignment
                .borrowed_by
                .sort_by(|a, b| a.session.session_id.cmp(&b.session.session_id));
            Ok(())
        },
    )
}

fn run_handoff(context: &CliContext, args: HandoffArgs) -> Result<Value, CliError> {
    validate_idempotency_key(&args.idempotency_key)?;
    let target = resolve_live_session_ref(context, &args.to_session)?;
    let (record, incarnation) = authenticated_self(context)?;
    ensure_active_claim(context, &record)?;
    let quiescence =
        crate::coordination::lock_session_quiescence(context, &record.id, &incarnation)?;
    if !quiescence.active_claim {
        return Err(CliError::data(
            "claim-not-active",
            "no matching active work claim exists",
            None,
        ));
    }
    if quiescence.active_operation || quiescence.uncertain_operation {
        return Err(CliError::data(
            "handoff-not-quiescent",
            "primary manager has an active or uncertain mutation operation",
            None,
        ));
    }
    let mut locked = orchestration::lock_registry(context)?;
    let source_run_id = require_current_main(&locked.registry, &record, &incarnation)?
        .run_id
        .clone();
    let target_run_id = locked
        .registry
        .runs
        .values()
        .find(|run| run.controller == target && run.state == "active")
        .map(|run| run.run_id.clone())
        .ok_or_else(|| invalid_input("handoff target is not an active Main Agent"))?;
    let request_digest = crate::coordination::request_digest(
        "handoff",
        &json!({ "assignment_id": args.assignment_id, "if_revision": args.if_revision }),
    );
    if let Some(value) = idempotency_replay(
        &locked.registry,
        &record,
        &incarnation,
        &args.idempotency_key,
        "handoff",
        &request_digest,
    )? {
        return Ok(value);
    }
    {
        let current = locked
            .registry
            .assignments
            .get(&args.assignment_id)
            .filter(|assignment| assignment.run_id == source_run_id)
            .ok_or_else(|| not_found("assignment-not-found", "assignment was not found"))?;
        ensure_primary_manager(current, &record, &incarnation)?;
        ensure_revision(args.if_revision, current.revision, "assignment")?;
        ensure_submit_recovery_not_in_flight(current)?;
        ensure_account_handoff_not_in_flight(current)?;
        let mut dependency_edges = current.depends_on.clone();
        dependency_edges.extend(
            locked
                .registry
                .assignments
                .values()
                .filter(|candidate| {
                    candidate.run_id == source_run_id
                        && candidate.depends_on.contains(&args.assignment_id)
                })
                .map(|candidate| candidate.assignment_id.clone()),
        );
        dependency_edges.sort();
        dependency_edges.dedup();
        if !dependency_edges.is_empty() {
            return Err(CliError::data(
                "handoff-dependency-conflict",
                "assignment handoff would create a cross-run dependency edge",
                Some(json!({ "assignments": dependency_edges })),
            ));
        }
    }
    let current = locked
        .registry
        .assignments
        .get_mut(&args.assignment_id)
        .expect("validated handoff assignment remains present");
    current.run_id = target_run_id;
    current.primary_manager = target;
    current.revision = current.revision.saturating_add(1);
    current.updated_at = timestamp();
    let outcome = json!({
        "schema_version": "main-agent.relationship-mutation-result.v1",
        "assignment": public_assignment_view(current)
    });
    store_receipt(
        &mut locked.registry,
        &record,
        &incarnation,
        &args.idempotency_key,
        "handoff",
        &request_digest,
        outcome.clone(),
    )?;
    locked.save()?;
    drop(quiescence);
    Ok(outcome)
}

fn run_adopt(context: &CliContext, args: AssignmentMutationArgs) -> Result<Value, CliError> {
    validate_idempotency_key(&args.idempotency_key)?;
    let (record, incarnation) = authenticated_self(context)?;
    ensure_active_claim(context, &record)?;
    let mut locked = orchestration::lock_registry(context)?;
    let run = require_current_main(&locked.registry, &record, &incarnation)?.clone();
    let current = locked
        .registry
        .assignments
        .get_mut(&args.assignment_id)
        .ok_or_else(|| not_found("assignment-not-found", "assignment was not found"))?;
    ensure_revision(args.if_revision, current.revision, "assignment")?;
    ensure_submit_recovery_not_in_flight(current)?;
    ensure_account_handoff_not_in_flight(current)?;
    if orchestration::session_ref_is_live(context, &current.primary_manager) {
        return Err(CliError::data(
            "assignment-not-orphaned",
            "assignment primary manager is still live",
            None,
        ));
    }
    current.run_id = run.run_id.clone();
    current.primary_manager = run.controller.clone();
    current.revision = current.revision.saturating_add(1);
    current.updated_at = timestamp();
    let outcome = json!({
        "schema_version": "main-agent.assignment-mutation-result.v1",
        "assignment": public_assignment_view(current)
    });
    let request_digest = crate::coordination::request_digest(
        "adopt",
        &json!({ "assignment_id": args.assignment_id, "if_revision": args.if_revision }),
    );
    store_receipt(
        &mut locked.registry,
        &record,
        &incarnation,
        &args.idempotency_key,
        "adopt",
        &request_digest,
        outcome.clone(),
    )?;
    locked.save()?;
    Ok(outcome)
}

fn run_close(context: &CliContext, args: RunMutationArgs) -> Result<Value, CliError> {
    validate_idempotency_key(&args.idempotency_key)?;
    let (record, incarnation) = authenticated_self(context)?;
    ensure_active_claim(context, &record)?;
    let mut locked = orchestration::lock_registry(context)?;
    let run = require_current_main(&locked.registry, &record, &incarnation)?.clone();
    ensure_revision(args.if_revision, run.revision, "run")?;
    let nonterminal = locked
        .registry
        .assignments
        .values()
        .filter(|assignment| assignment.run_id == run.run_id)
        .filter(|assignment| !matches!(assignment.state.as_str(), "released" | "cancelled"))
        .map(|assignment| assignment.assignment_id.clone())
        .collect::<Vec<_>>();
    if !nonterminal.is_empty() {
        return Err(CliError::data(
            "run-not-closeable",
            "run has nonterminal assignments",
            Some(json!({ "assignment_ids": nonterminal })),
        ));
    }
    let current = locked
        .registry
        .runs
        .get_mut(&run.run_id)
        .expect("run exists");
    current.state = "closed".to_string();
    current.revision = current.revision.saturating_add(1);
    current.updated_at = timestamp();
    let outcome =
        json!({ "schema_version": "main-agent.close-result.v1", "run": public_run_view(current) });
    let request_digest = crate::coordination::request_digest("close", &args.if_revision);
    store_receipt(
        &mut locked.registry,
        &record,
        &incarnation,
        &args.idempotency_key,
        "close",
        &request_digest,
        outcome.clone(),
    )?;
    locked.save()?;
    Ok(outcome)
}

pub(crate) fn preview_group_cleanup(
    context: &CliContext,
    main_session_id: &str,
) -> Result<Value, CliError> {
    crate::validate_id(main_session_id)?;
    let record = load_session_record(context, main_session_id)?;
    let incarnation = record
        .runtime
        .as_ref()
        .map(|runtime| runtime.launch_id.as_str())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            CliError::data(
                "main-session-incarnation-unavailable",
                "Main Agent session incarnation is unavailable",
                None,
            )
        })?;
    let main = session_ref(context, &record, incarnation);
    let registry = orchestration::load_registry_readonly(context)?;
    let run = registry
        .runs
        .values()
        .find(|run| run.state == "active" && run.controller == main)
        .ok_or_else(|| {
            not_found(
                "main-agent-run-not-found",
                "session is not the current controller of an active Main Agent run",
            )
        })?;
    serde_json::to_value(build_group_cleanup_plan(&registry, run, &main)?)
        .map_err(|_| invalid_input("group cleanup preview could not be serialized"))
}

pub(crate) fn execute_group_cleanup(
    context: &CliContext,
    main_session_id: &str,
    request: GroupCleanupRequest,
    tmux_bin: PathBuf,
) -> Result<GroupCleanupExecution, CliError> {
    let requested_main_session_id = main_session_id.to_string();
    validate_group_cleanup_request(&requested_main_session_id, &request)?;
    let request_digest = group_cleanup_request_digest(&request);
    let resolved_record = load_session_record(context, main_session_id);
    let canonical_main_session_id = match resolved_record.as_ref() {
        Ok(record) => record.id.clone(),
        Err(error) if error.code() == "session-not-found" => {
            let progress_principal = orchestration::recover_group_cleanup_progress_principal(
                context,
                &requested_main_session_id,
                &request.expected_main_incarnation,
                &request.idempotency_key,
                &request_digest,
            )?;
            if let Some(principal) = progress_principal {
                principal
            } else {
                let registry = orchestration::load_registry_readonly(context)?;
                recover_completed_group_cleanup_principal(
                    &registry,
                    &requested_main_session_id,
                    &request.expected_main_incarnation,
                    &request.idempotency_key,
                    &request_digest,
                )?
                .unwrap_or_else(|| requested_main_session_id.clone())
            }
        }
        Err(_) => requested_main_session_id.clone(),
    };
    let main_session_id = canonical_main_session_id.as_str();
    let legacy_alias = (requested_main_session_id != canonical_main_session_id)
        .then_some(requested_main_session_id.as_str());
    let execution_owner_digest = group_cleanup_execution_owner_digest(main_session_id);
    let _execution_lock = lock_group_cleanup_execution(context, &execution_owner_digest)?;

    let replay = {
        let locked = orchestration::lock_registry(context)?;
        group_cleanup_replay_with_legacy_alias(
            context,
            &locked.registry,
            main_session_id,
            legacy_alias,
            &request.expected_main_incarnation,
            &request.idempotency_key,
            &request_digest,
        )?
    };
    let resume_state = if let Some(replay) = replay {
        if replay.value["completed"] == true {
            let deleted_registry_fences = replay
                .resume
                .map(|resume| resume.deleted_registry_fences)
                .unwrap_or_default();
            orchestration::remove_group_cleanup_progress(
                context,
                &group_cleanup_progress_key(
                    main_session_id,
                    &request.expected_main_incarnation,
                    &request.idempotency_key,
                ),
            )?;
            remove_legacy_group_cleanup_progress(
                context,
                legacy_alias,
                &request.expected_main_incarnation,
                &request.idempotency_key,
            )?;
            return Ok(GroupCleanupExecution {
                value: replay.value,
                deleted_registry_fences,
            });
        }
        Some(replay.resume.ok_or_else(|| {
            CliError::data(
                "group-cleanup-progress-invalid",
                "retryable group cleanup receipt has no resumable progress",
                None,
            )
        })?)
    } else {
        None
    };
    let closed_run_revision = request
        .expected_run_revision
        .checked_add(1)
        .ok_or_else(|| {
            CliError::data(
                "orchestration-revision-capacity",
                "Main Agent run revision reached its maximum value",
                Some(json!({ "run_revision": request.expected_run_revision })),
            )
        })?;

    let record = match resolved_record {
        Ok(record) => Some(record),
        Err(error)
            if error.code() == "session-not-found"
                && resume_state.as_ref().is_some_and(|resume| {
                    resume
                        .pending_registry_fences
                        .iter()
                        .any(|fence| fence.session_id == main_session_id)
                }) =>
        {
            None
        }
        Err(error) => return Err(error),
    };
    let incarnation = record
        .as_ref()
        .and_then(|record| record.runtime.as_ref())
        .map(|runtime| runtime.launch_id.as_str())
        .filter(|value| !value.is_empty())
        .unwrap_or(&request.expected_main_incarnation)
        .to_string();
    if incarnation != request.expected_main_incarnation {
        return Err(CliError::data(
            "main-session-incarnation-conflict",
            "Main Agent session incarnation changed after preview",
            Some(json!({
                "expected_main_incarnation": request.expected_main_incarnation,
                "actual_main_incarnation": incarnation,
            })),
        ));
    }
    let progress_identity = GroupCleanupProgressIdentity {
        requested_session_id: &requested_main_session_id,
        principal_session_id: main_session_id,
        incarnation: &incarnation,
    };
    let main = record.as_ref().map_or_else(
        || {
            resume_state
                .as_ref()
                .expect("missing Main Agent record requires resumable progress")
                .plan
                .main
                .clone()
        },
        |record| session_ref(context, record, &incarnation),
    );

    let plan = if let Some(resume) = resume_state.as_ref() {
        if resume.schema_version != "agent-session.main-agent-group-cleanup-progress.v1"
            || resume.plan.main != main
            || resume.plan.run_id.is_empty()
            || resume.plan.run_revision != request.expected_run_revision
            || resume.plan.plan_digest != request.expected_plan_digest
        {
            return Err(CliError::data(
                "group-cleanup-progress-invalid",
                "durable group cleanup progress does not match the immutable request",
                None,
            ));
        }
        resume.plan.clone()
    } else {
        let locked = orchestration::lock_registry(context)?;
        let run = locked
            .registry
            .runs
            .values()
            .find(|run| run.state == "active" && run.controller == main)
            .cloned()
            .ok_or_else(|| {
                not_found(
                    "main-agent-run-not-found",
                    "session is not the current controller of an active Main Agent run",
                )
            })?;
        ensure_revision(request.expected_run_revision, run.revision, "run")?;
        let plan = build_group_cleanup_plan(&locked.registry, &run, &main)?;
        if plan.plan_digest != request.expected_plan_digest {
            return Err(CliError::data(
                "group-cleanup-plan-conflict",
                "Main Agent group cleanup plan changed after preview",
                Some(json!({
                    "expected_plan_digest": request.expected_plan_digest,
                    "current_plan_digest": plan.plan_digest,
                    "current_run_revision": run.revision,
                })),
            ));
        }
        plan
    };
    let mut worker_refs = plan
        .workers
        .iter()
        .filter_map(|worker| worker.worker.as_ref())
        .cloned()
        .collect::<Vec<_>>();
    worker_refs.sort_by(|left, right| left.session_id.cmp(&right.session_id));
    let mut locked_worker_authorities = Vec::with_capacity(worker_refs.len());
    for worker in &worker_refs {
        let locked = crate::lock_exact_session_authority(context, &worker.session_id)?;
        if let Some(locked) = locked.as_ref() {
            let worker_incarnation = locked
                .record
                .runtime
                .as_ref()
                .map(|runtime| runtime.launch_id.as_str())
                .unwrap_or_default();
            if !orchestration::session_ref_matches(worker, &locked.record, worker_incarnation) {
                return Err(CliError::data(
                    "session-incarnation-conflict",
                    "worker session identity changed before group cleanup authority was fenced",
                    Some(json!({ "session_id": worker.session_id })),
                ));
            }
        }
        locked_worker_authorities.push((worker.clone(), locked));
    }
    let plan = if resume_state
        .as_ref()
        .is_none_or(|resume| !resume.authority_sealed)
    {
        let worker_sessions = worker_refs
            .iter()
            .map(|worker| {
                (
                    worker.session_id.clone(),
                    worker.session_incarnation.clone(),
                )
            })
            .collect::<Vec<_>>();
        let cleanup_quiescence = crate::coordination::lock_group_cleanup_quiescence(
            context,
            &worker_sessions,
            request.mode == GroupCleanupMode::Force,
        )?;
        let mut locked = orchestration::lock_registry(context)?;
        if resume_state.is_none()
            && group_cleanup_replay_with_legacy_alias(
                context,
                &locked.registry,
                main_session_id,
                legacy_alias,
                &incarnation,
                &request.idempotency_key,
                &request_digest,
            )?
            .is_some()
        {
            return Err(CliError::runtime(
                "group-cleanup-in-progress",
                "an identical group cleanup invocation is already making progress",
                None,
            ));
        }
        let run = locked
            .registry
            .runs
            .values()
            .find(|run| run.state == "active" && run.controller == main)
            .cloned()
            .ok_or_else(|| {
                not_found(
                    "main-agent-run-not-found",
                    "session is not the current controller of an active Main Agent run",
                )
            })?;
        ensure_revision(request.expected_run_revision, run.revision, "run")?;
        let current_plan = build_group_cleanup_plan(&locked.registry, &run, &main)?;
        if current_plan.plan_digest != request.expected_plan_digest {
            return Err(CliError::data(
                "group-cleanup-plan-conflict",
                "Main Agent group cleanup plan changed before authority was fenced",
                Some(json!({
                    "expected_plan_digest": request.expected_plan_digest,
                    "current_plan_digest": current_plan.plan_digest,
                    "current_run_revision": run.revision,
                })),
            ));
        }
        group_cleanup_assignment_transitions(&locked.registry, &run, &main, request.mode)?;
        let prior_results = resume_state
            .as_ref()
            .map(|resume| resume.worker_results.clone())
            .unwrap_or_default();
        let prior_fences = resume_state
            .as_ref()
            .map(|resume| resume.deleted_registry_fences.clone())
            .unwrap_or_default();
        let prior_pending_fences = resume_state
            .as_ref()
            .map(|resume| resume.pending_registry_fences.clone())
            .unwrap_or_default();
        if resume_state.is_none() {
            let initial_resume = GroupCleanupResumeState {
                schema_version: "agent-session.main-agent-group-cleanup-progress.v1".to_string(),
                plan: current_plan.clone(),
                authority_sealed: false,
                worker_results: prior_results.clone(),
                deleted_registry_fences: prior_fences.clone(),
                pending_registry_fences: prior_pending_fences.clone(),
                run_closed: false,
            };
            let initial_value = group_cleanup_progress_value(
                &current_plan,
                &prior_results,
                false,
                "authority_fence",
            );
            store_receipt_for_principal(
                &mut locked.registry,
                main_session_id,
                &incarnation,
                &request.idempotency_key,
                "group-cleanup",
                &request_digest,
                group_cleanup_stored_outcome(&initial_value, &initial_resume)?,
            )?;
            // The initial progress receipt is the first durable transition.
            // Every later external effect is therefore adoptable by exact retry.
            locked.save()?;
            interrupt_group_cleanup_for_test(context, "authority_fence")?;
        }
        // The exact session record locks above serialize this durable fence
        // against resume and broker reprovision. Persist it before sealing the
        // coordination broker and before making assignment terminalization
        // durable; an interrupted retry adopts the same fence.
        for (worker, present) in &locked_worker_authorities {
            if present.is_some() {
                orchestration::persist_session_group_cleanup_fence(
                    context,
                    worker,
                    &main,
                    &current_plan.run_id,
                    &current_plan.plan_digest,
                )?;
            }
        }
        cleanup_quiescence.seal(context)?;
        // Coordination authority is sealed before assignment state changes
        // become durable. The sealed progress update and assignment transition
        // share the orchestration save.
        prepare_group_cleanup_assignments(&mut locked.registry, &run, &main, request.mode)?;
        let sealed_resume = group_cleanup_resume_state(
            &current_plan,
            &prior_results,
            &prior_fences,
            &prior_pending_fences,
            false,
        );
        let sealed_value =
            group_cleanup_progress_value(&current_plan, &prior_results, false, "authority_sealed");
        store_receipt_for_principal(
            &mut locked.registry,
            main_session_id,
            &incarnation,
            &request.idempotency_key,
            "group-cleanup",
            &request_digest,
            group_cleanup_stored_outcome(&sealed_value, &sealed_resume)?,
        )?;
        locked.save()?;
        interrupt_group_cleanup_for_test(context, "authority_sealed")?;
        current_plan
    } else {
        for (worker, present) in &locked_worker_authorities {
            if present.is_some() {
                orchestration::persist_session_group_cleanup_fence(
                    context,
                    worker,
                    &main,
                    &plan.run_id,
                    &plan.plan_digest,
                )?;
            }
        }
        plan
    };
    drop(locked_worker_authorities);

    let mut deleted_registry_fences = resume_state
        .as_ref()
        .map(|resume| resume.deleted_registry_fences.clone())
        .unwrap_or_default();
    let mut pending_registry_fences = resume_state
        .as_ref()
        .map(|resume| resume.pending_registry_fences.clone())
        .unwrap_or_default();
    let mut worker_results = resume_state
        .as_ref()
        .map(|resume| {
            resume
                .worker_results
                .iter()
                .filter(|result| {
                    !matches!(
                        result["outcome"].as_str(),
                        Some("failed" | "delete_pending")
                    )
                })
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    for worker_plan in &plan.workers {
        if worker_results.iter().any(|result| {
            result["assignment_id"] == worker_plan.assignment_id
                && matches!(
                    result["outcome"].as_str(),
                    Some("deleted" | "absent" | "not_started")
                )
        }) {
            continue;
        }
        let Some(worker) = worker_plan.worker.as_ref() else {
            worker_results.push(json!({
                "assignment_id": worker_plan.assignment_id,
                "session_id": null,
                "outcome": "not_started",
                "cleanup_pending": false,
            }));
            let progress =
                group_cleanup_progress_value(&plan, &worker_results, false, "worker_checkpoint");
            store_group_cleanup_receipt(
                context,
                &progress_identity,
                &request,
                &request_digest,
                progress,
                group_cleanup_resume_state(
                    &plan,
                    &worker_results,
                    &deleted_registry_fences,
                    &pending_registry_fences,
                    false,
                ),
            )?;
            interrupt_group_cleanup_for_test(context, "worker_checkpoint")?;
            continue;
        };
        let worker_path = session_dir(context, &worker.session_id);
        match fs::symlink_metadata(&worker_path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if let Some(index) = pending_registry_fences
                    .iter()
                    .position(|fence| fence.session_id == worker.session_id)
                {
                    let fence = pending_registry_fences.remove(index);
                    if !deleted_registry_fences.contains(&fence) {
                        deleted_registry_fences.push(fence);
                    }
                }
                worker_results.push(json!({
                    "assignment_id": worker_plan.assignment_id,
                    "session_id": worker.session_id,
                    "outcome": "absent",
                    "cleanup_pending": false,
                }));
                let progress = group_cleanup_progress_value(
                    &plan,
                    &worker_results,
                    false,
                    "worker_checkpoint",
                );
                store_group_cleanup_receipt(
                    context,
                    &progress_identity,
                    &request,
                    &request_digest,
                    progress,
                    group_cleanup_resume_state(
                        &plan,
                        &worker_results,
                        &deleted_registry_fences,
                        &pending_registry_fences,
                        false,
                    ),
                )?;
                interrupt_group_cleanup_for_test(context, "worker_checkpoint")?;
                continue;
            }
            Err(_) => {
                let error = CliError::runtime(
                    "worker-session-unavailable",
                    "worker session state is unavailable",
                    None,
                );
                let value = group_cleanup_failure(
                    &plan,
                    &worker_results,
                    Some(worker_plan),
                    "worker_cleanup",
                    &error,
                    false,
                );
                store_group_cleanup_receipt(
                    context,
                    &progress_identity,
                    &request,
                    &request_digest,
                    value.clone(),
                    group_cleanup_resume_state(
                        &plan,
                        &worker_results,
                        &deleted_registry_fences,
                        &pending_registry_fences,
                        false,
                    ),
                )?;
                return Ok(GroupCleanupExecution {
                    value,
                    deleted_registry_fences,
                });
            }
            Ok(_) => {}
        }
        let worker_record = match load_session_record(context, &worker.session_id) {
            Ok(record) => record,
            Err(error) => {
                let value = group_cleanup_failure(
                    &plan,
                    &worker_results,
                    Some(worker_plan),
                    "worker_cleanup",
                    &error,
                    false,
                );
                store_group_cleanup_receipt(
                    context,
                    &progress_identity,
                    &request,
                    &request_digest,
                    value.clone(),
                    group_cleanup_resume_state(
                        &plan,
                        &worker_results,
                        &deleted_registry_fences,
                        &pending_registry_fences,
                        false,
                    ),
                )?;
                return Ok(GroupCleanupExecution {
                    value,
                    deleted_registry_fences,
                });
            }
        };
        let worker_incarnation = worker_record
            .runtime
            .as_ref()
            .map(|runtime| runtime.launch_id.as_str())
            .unwrap_or_default();
        if !orchestration::session_ref_matches(worker, &worker_record, worker_incarnation) {
            let error = CliError::data(
                "session-incarnation-conflict",
                "worker session identity changed before group cleanup",
                Some(json!({ "assignment_id": worker_plan.assignment_id })),
            );
            let value = group_cleanup_failure(
                &plan,
                &worker_results,
                Some(worker_plan),
                "worker_cleanup",
                &error,
                false,
            );
            store_group_cleanup_receipt(
                context,
                &progress_identity,
                &request,
                &request_digest,
                value.clone(),
                group_cleanup_resume_state(
                    &plan,
                    &worker_results,
                    &deleted_registry_fences,
                    &pending_registry_fences,
                    false,
                ),
            )?;
            return Ok(GroupCleanupExecution {
                value,
                deleted_registry_fences,
            });
        }
        let pending_fence = SessionRegistryFence::from_record(&worker_record);
        if !pending_registry_fences.contains(&pending_fence) {
            pending_registry_fences.push(pending_fence.clone());
        }
        worker_results.push(json!({
            "assignment_id": worker_plan.assignment_id,
            "session_id": worker.session_id,
            "outcome": "delete_pending",
            "cleanup_pending": false,
        }));
        let pending_value =
            group_cleanup_progress_value(&plan, &worker_results, false, "worker_delete_pending");
        store_group_cleanup_receipt(
            context,
            &progress_identity,
            &request,
            &request_digest,
            pending_value,
            group_cleanup_resume_state(
                &plan,
                &worker_results,
                &deleted_registry_fences,
                &pending_registry_fences,
                false,
            ),
        )?;
        interrupt_group_cleanup_for_test(
            context,
            &format!("worker_delete_pending:{}", worker_plan.assignment_id),
        )?;
        match delete_session(context, &worker.session_id, tmux_bin.clone()) {
            Ok(deleted) => {
                interrupt_group_cleanup_for_test(
                    context,
                    &format!(
                        "worker_deleted_uncheckpointed:{}",
                        worker_plan.assignment_id
                    ),
                )?;
                worker_results.retain(|result| {
                    result["assignment_id"] != worker_plan.assignment_id
                        || result["outcome"] != "delete_pending"
                });
                pending_registry_fences.retain(|fence| fence != &pending_fence);
                worker_results.push(json!({
                    "assignment_id": worker_plan.assignment_id,
                    "session_id": worker.session_id,
                    "outcome": "deleted",
                    "cleanup_pending": deleted.cleanup_pending,
                }));
                deleted_registry_fences.push(deleted.registry_fence);
                let progress =
                    group_cleanup_progress_value(&plan, &worker_results, false, "worker_deleted");
                store_group_cleanup_receipt(
                    context,
                    &progress_identity,
                    &request,
                    &request_digest,
                    progress,
                    group_cleanup_resume_state(
                        &plan,
                        &worker_results,
                        &deleted_registry_fences,
                        &pending_registry_fences,
                        false,
                    ),
                )?;
                interrupt_group_cleanup_for_test(
                    context,
                    &format!("worker_deleted:{}", worker_plan.assignment_id),
                )?;
            }
            Err(error) => {
                worker_results.retain(|result| {
                    result["assignment_id"] != worker_plan.assignment_id
                        || result["outcome"] != "delete_pending"
                });
                let value = group_cleanup_failure(
                    &plan,
                    &worker_results,
                    Some(worker_plan),
                    "worker_cleanup",
                    &error,
                    false,
                );
                store_group_cleanup_receipt(
                    context,
                    &progress_identity,
                    &request,
                    &request_digest,
                    value.clone(),
                    group_cleanup_resume_state(
                        &plan,
                        &worker_results,
                        &deleted_registry_fences,
                        &pending_registry_fences,
                        false,
                    ),
                )?;
                return Ok(GroupCleanupExecution {
                    value,
                    deleted_registry_fences,
                });
            }
        }
    }

    {
        let mut locked = orchestration::lock_registry(context)?;
        let run = locked
            .registry
            .runs
            .get(&plan.run_id)
            .cloned()
            .ok_or_else(|| {
                CliError::data(
                    "group-cleanup-run-conflict",
                    "Main Agent run changed while workers were being cleaned up",
                    None,
                )
            })?;
        if run.controller != main
            || !matches!(
                (run.state.as_str(), run.revision),
                ("active", revision) if revision == request.expected_run_revision
            ) && !matches!(
                (run.state.as_str(), run.revision),
                ("closed", revision) if revision == closed_run_revision
            )
        {
            let error = CliError::data(
                "group-cleanup-run-conflict",
                "Main Agent run changed while workers were being cleaned up",
                Some(json!({
                    "current_run_revision": run.revision,
                    "current_run_state": run.state
                })),
            );
            let value =
                group_cleanup_failure(&plan, &worker_results, None, "run_close", &error, false);
            store_receipt_for_principal(
                &mut locked.registry,
                main_session_id,
                &incarnation,
                &request.idempotency_key,
                "group-cleanup",
                &request_digest,
                group_cleanup_stored_outcome(
                    &value,
                    &group_cleanup_resume_state(
                        &plan,
                        &worker_results,
                        &deleted_registry_fences,
                        &pending_registry_fences,
                        false,
                    ),
                )?,
            )?;
            locked.save()?;
            return Ok(GroupCleanupExecution {
                value,
                deleted_registry_fences,
            });
        }
        if run.state == "active" {
            let run = locked
                .registry
                .runs
                .get_mut(&plan.run_id)
                .expect("run checked above");
            run.state = "closed".to_string();
            run.revision = closed_run_revision;
            run.updated_at = timestamp();
        }
        let run_closed_resume = group_cleanup_resume_state(
            &plan,
            &worker_results,
            &deleted_registry_fences,
            &pending_registry_fences,
            true,
        );
        let run_closed_value =
            group_cleanup_progress_value(&plan, &worker_results, true, "run_closed");
        store_receipt_for_principal(
            &mut locked.registry,
            main_session_id,
            &incarnation,
            &request.idempotency_key,
            "group-cleanup",
            &request_digest,
            group_cleanup_stored_outcome(&run_closed_value, &run_closed_resume)?,
        )?;
        locked.save()?;
    }
    interrupt_group_cleanup_for_test(context, "run_closed")?;

    let current_main = match load_session_record(context, main_session_id) {
        Ok(record) => record,
        Err(error) if error.code() == "session-not-found" => {
            let Some(index) = pending_registry_fences
                .iter()
                .position(|fence| fence.session_id == main_session_id)
            else {
                return Err(error);
            };
            let fence = pending_registry_fences.remove(index);
            if !deleted_registry_fences.contains(&fence) {
                deleted_registry_fences.push(fence);
            }
            let value = json!({
                "schema_version": GROUP_CLEANUP_RESULT_SCHEMA,
                "run_id": plan.run_id,
                "completed": true,
                "run_closed": true,
                "main_deleted": true,
                "workers": worker_results,
            });
            let completed_resume = group_cleanup_resume_state(
                &plan,
                &worker_results,
                &deleted_registry_fences,
                &pending_registry_fences,
                true,
            );
            store_completed_group_cleanup_receipt(
                context,
                main_session_id,
                legacy_alias,
                &incarnation,
                &request,
                &request_digest,
                group_cleanup_stored_outcome(&value, &completed_resume)?,
            )?;
            interrupt_group_cleanup_for_test(context, "main_deleted")?;
            return Ok(GroupCleanupExecution {
                value,
                deleted_registry_fences,
            });
        }
        Err(error) => return Err(error),
    };
    let current_main_incarnation = current_main
        .runtime
        .as_ref()
        .map(|runtime| runtime.launch_id.as_str())
        .unwrap_or_default();
    if !orchestration::session_ref_matches(&main, &current_main, current_main_incarnation) {
        let error = CliError::data(
            "main-session-incarnation-conflict",
            "Main Agent session identity changed before final deletion",
            None,
        );
        let value =
            group_cleanup_failure(&plan, &worker_results, None, "main_delete", &error, true);
        store_group_cleanup_receipt(
            context,
            &progress_identity,
            &request,
            &request_digest,
            value.clone(),
            group_cleanup_resume_state(
                &plan,
                &worker_results,
                &deleted_registry_fences,
                &pending_registry_fences,
                true,
            ),
        )?;
        return Ok(GroupCleanupExecution {
            value,
            deleted_registry_fences,
        });
    }
    let pending_main_fence = SessionRegistryFence::from_record(&current_main);
    if !pending_registry_fences.contains(&pending_main_fence) {
        pending_registry_fences.push(pending_main_fence.clone());
    }
    let pending_value =
        group_cleanup_progress_value(&plan, &worker_results, true, "main_delete_pending");
    store_group_cleanup_receipt(
        context,
        &progress_identity,
        &request,
        &request_digest,
        pending_value,
        group_cleanup_resume_state(
            &plan,
            &worker_results,
            &deleted_registry_fences,
            &pending_registry_fences,
            true,
        ),
    )?;
    interrupt_group_cleanup_for_test(context, "main_delete_pending")?;
    let main_deleted = match delete_session(context, main_session_id, tmux_bin) {
        Ok(deleted) => {
            interrupt_group_cleanup_for_test(context, "main_deleted_uncheckpointed")?;
            deleted
        }
        Err(error) => {
            let value =
                group_cleanup_failure(&plan, &worker_results, None, "main_delete", &error, true);
            store_group_cleanup_receipt(
                context,
                &progress_identity,
                &request,
                &request_digest,
                value.clone(),
                group_cleanup_resume_state(
                    &plan,
                    &worker_results,
                    &deleted_registry_fences,
                    &pending_registry_fences,
                    true,
                ),
            )?;
            return Ok(GroupCleanupExecution {
                value,
                deleted_registry_fences,
            });
        }
    };
    pending_registry_fences.retain(|fence| fence != &pending_main_fence);
    if !deleted_registry_fences.contains(&main_deleted.registry_fence) {
        deleted_registry_fences.push(main_deleted.registry_fence);
    }
    let value = json!({
        "schema_version": GROUP_CLEANUP_RESULT_SCHEMA,
        "run_id": plan.run_id,
        "completed": true,
        "run_closed": true,
        "main_deleted": true,
        "workers": worker_results,
    });
    let completed_resume = group_cleanup_resume_state(
        &plan,
        &worker_results,
        &deleted_registry_fences,
        &pending_registry_fences,
        true,
    );
    store_completed_group_cleanup_receipt(
        context,
        main_session_id,
        legacy_alias,
        &incarnation,
        &request,
        &request_digest,
        group_cleanup_stored_outcome(&value, &completed_resume)?,
    )?;
    interrupt_group_cleanup_for_test(context, "main_deleted")?;
    Ok(GroupCleanupExecution {
        value,
        deleted_registry_fences,
    })
}

fn validate_group_cleanup_request(
    main_session_id: &str,
    request: &GroupCleanupRequest,
) -> Result<(), CliError> {
    crate::validate_id(main_session_id)?;
    if request.schema_version != GROUP_CLEANUP_REQUEST_SCHEMA {
        return Err(invalid_input("group cleanup request schema is unsupported"));
    }
    orchestration::validate_slug(
        "main session incarnation",
        &request.expected_main_incarnation,
        128,
    )?;
    orchestration::validate_digest(&request.expected_plan_digest)?;
    validate_idempotency_key(&request.idempotency_key)
}

fn group_cleanup_request_digest(request: &GroupCleanupRequest) -> String {
    crate::coordination::request_digest(
        "main-agent-group-cleanup",
        &json!({
            "expected_main_incarnation": request.expected_main_incarnation,
            "expected_run_revision": request.expected_run_revision,
            "expected_plan_digest": request.expected_plan_digest,
            "mode": request.mode,
        }),
    )
}

fn group_cleanup_execution_owner_digest(main_session_id: &str) -> String {
    crate::coordination::request_digest(
        "main-agent-group-cleanup-owner",
        &json!({ "main_session_id": main_session_id }),
    )
}

#[derive(Debug)]
struct GroupCleanupExecutionLock {
    _file: fs::File,
}

fn lock_group_cleanup_execution(
    context: &CliContext,
    request_digest: &str,
) -> Result<GroupCleanupExecutionLock, CliError> {
    fs::create_dir_all(&context.state_dir).map_err(|_| {
        CliError::runtime(
            "orchestration-store-unavailable",
            "orchestration store is unavailable",
            None,
        )
    })?;
    let directory_path = orchestration::ensure_orchestration_root(context)?;
    let directory = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(directory_path)
        .map_err(|_| {
            CliError::runtime(
                "orchestration-store-unavailable",
                "orchestration store is unavailable",
                None,
            )
        })?;
    let directory_metadata = directory.metadata().map_err(|_| {
        CliError::runtime(
            "orchestration-store-unavailable",
            "orchestration store is unavailable",
            None,
        )
    })?;
    if !directory_metadata.is_dir()
        || directory_metadata.uid() != unsafe { libc::geteuid() }
        || directory_metadata.mode() & 0o077 != 0
    {
        return Err(CliError::data(
            "orchestration-store-invalid",
            "orchestration store root is unsafe",
            None,
        ));
    }
    let name = CString::new(format!("group-cleanup-{request_digest}.lock"))
        .map_err(|_| invalid_input("group cleanup request digest is invalid"))?;
    // SAFETY: the directory descriptor is a validated, non-symlinked private
    // orchestration root, and the returned descriptor is owned by `lock`.
    let descriptor = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDWR | libc::O_CREAT | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
            nils_common::fs::SECRET_FILE_MODE,
        )
    };
    if descriptor < 0 {
        return Err(CliError::runtime(
            "orchestration-store-unavailable",
            "orchestration store is unavailable",
            None,
        ));
    }
    // SAFETY: `openat` returned a newly owned descriptor.
    let lock = unsafe { fs::File::from_raw_fd(descriptor) };
    let lock_metadata = lock.metadata().map_err(|_| {
        CliError::runtime(
            "orchestration-store-unavailable",
            "orchestration store is unavailable",
            None,
        )
    })?;
    if !lock_metadata.is_file()
        || lock_metadata.uid() != unsafe { libc::geteuid() }
        || lock_metadata.mode() & 0o077 != 0
    {
        return Err(CliError::data(
            "orchestration-store-invalid",
            "orchestration cleanup lock is unsafe",
            None,
        ));
    }
    // SAFETY: the descriptor remains open for the duration of the execution.
    let result = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if result != 0 {
        return Err(CliError::runtime(
            "group-cleanup-in-progress",
            "an identical group cleanup invocation is already making progress",
            None,
        ));
    }
    Ok(GroupCleanupExecutionLock { _file: lock })
}

struct GroupCleanupReplaySelector<'a> {
    progress_principal_session_id: &'a str,
    requested_session_id: &'a str,
    include_registry_outcome: bool,
}

fn group_cleanup_replay(
    context: &CliContext,
    registry: &orchestration::Registry,
    selector: GroupCleanupReplaySelector<'_>,
    incarnation: &str,
    idempotency_key: &str,
    request_digest: &str,
) -> Result<Option<GroupCleanupReplay>, CliError> {
    let registry_outcome = if selector.include_registry_outcome
        && let Some(receipt) = registry.receipts.get(&receipt_key(
            selector.progress_principal_session_id,
            incarnation,
            idempotency_key,
        )) {
        if receipt.operation != "group-cleanup" || receipt.request_digest != request_digest {
            return Err(CliError::data(
                "idempotency-conflict",
                "idempotency key was already used for a different request",
                None,
            ));
        }
        let replay = decode_group_cleanup_replay(receipt.outcome.clone())?;
        if replay.value["completed"] == true {
            return Ok(Some(replay));
        }
        Some(replay)
    } else {
        None
    };
    let key = group_cleanup_progress_key(
        selector.progress_principal_session_id,
        incarnation,
        idempotency_key,
    );
    let Some(bytes) = orchestration::read_group_cleanup_progress(context, &key)? else {
        return Ok(registry_outcome);
    };
    let receipt = orchestration::decode_group_cleanup_progress_receipt(&bytes).map_err(|_| {
        CliError::data(
            "group-cleanup-progress-invalid",
            "durable group cleanup progress is invalid",
            None,
        )
    })?;
    let (Some(canonical_session_id), Some(canonical_incarnation)) = (
        receipt.outcome["_resume"]["plan"]["main"]["session_id"].as_str(),
        receipt.outcome["_resume"]["plan"]["main"]["session_incarnation"].as_str(),
    ) else {
        return Err(CliError::data(
            "group-cleanup-progress-invalid",
            "durable group cleanup progress identity is invalid",
            None,
        ));
    };
    if receipt.principal_session_id != selector.progress_principal_session_id
        || receipt.idempotency_key != idempotency_key
        || !(orchestration::GroupCleanupSelectorBinding {
            schema_version: &receipt.schema_version,
            requested_session_id: receipt.requested_session_id.as_deref(),
            stored_principal_session_id: &receipt.principal_session_id,
            canonical_session_id,
            stored_incarnation: &receipt.principal_incarnation,
            canonical_incarnation,
            expected_session_id: selector.requested_session_id,
            expected_incarnation: incarnation,
        })
        .is_exact()
    {
        return Err(CliError::data(
            "group-cleanup-progress-invalid",
            "durable group cleanup progress identity is invalid",
            None,
        ));
    }
    if receipt.request_digest != request_digest {
        return Err(CliError::data(
            "idempotency-conflict",
            "idempotency key was already used for a different request",
            None,
        ));
    }
    decode_group_cleanup_replay(receipt.outcome).map(Some)
}

fn group_cleanup_replay_with_legacy_alias(
    context: &CliContext,
    registry: &orchestration::Registry,
    canonical_main_session_id: &str,
    legacy_alias: Option<&str>,
    incarnation: &str,
    idempotency_key: &str,
    request_digest: &str,
) -> Result<Option<GroupCleanupReplay>, CliError> {
    let Some(legacy_alias) = legacy_alias else {
        return group_cleanup_replay(
            context,
            registry,
            GroupCleanupReplaySelector {
                progress_principal_session_id: canonical_main_session_id,
                requested_session_id: canonical_main_session_id,
                include_registry_outcome: true,
            },
            incarnation,
            idempotency_key,
            request_digest,
        );
    };
    let validate_alias_replay = |replay: &GroupCleanupReplay| {
        replay.resume.as_ref().is_none_or(|resume| {
            resume.plan.main.session_id != canonical_main_session_id
                || resume.plan.main.session_incarnation != incarnation
        })
    };
    let legacy_replay = group_cleanup_replay(
        context,
        registry,
        GroupCleanupReplaySelector {
            progress_principal_session_id: legacy_alias,
            requested_session_id: legacy_alias,
            include_registry_outcome: true,
        },
        incarnation,
        idempotency_key,
        request_digest,
    )?;
    if let Some(replay) = legacy_replay.as_ref()
        && validate_alias_replay(replay)
    {
        return Err(CliError::data(
            "main-session-incarnation-conflict",
            "session alias resolved to a different Main Agent cleanup principal",
            None,
        ));
    }
    if legacy_replay.is_some() {
        return Ok(legacy_replay);
    }
    let canonical_progress = group_cleanup_replay(
        context,
        registry,
        GroupCleanupReplaySelector {
            progress_principal_session_id: canonical_main_session_id,
            requested_session_id: legacy_alias,
            include_registry_outcome: false,
        },
        incarnation,
        idempotency_key,
        request_digest,
    )?;
    if let Some(replay) = canonical_progress.as_ref()
        && validate_alias_replay(replay)
    {
        return Err(CliError::data(
            "main-session-incarnation-conflict",
            "session alias resolved to a different Main Agent cleanup principal",
            None,
        ));
    }
    Ok(canonical_progress)
}

fn recover_completed_group_cleanup_principal(
    registry: &orchestration::Registry,
    requested_session_id: &str,
    incarnation: &str,
    idempotency_key: &str,
    request_digest: &str,
) -> Result<Option<String>, CliError> {
    let mut recovered = None;
    for (key, receipt) in &registry.receipts {
        if receipt.operation != "group-cleanup"
            || receipt.request_digest != request_digest
            || receipt.outcome["completed"] != true
            || key != &receipt_key(&receipt.principal_session_id, incarnation, idempotency_key)
        {
            continue;
        }
        let Some(plan_main) = receipt.outcome["_resume"]["plan"]["main"].as_object() else {
            continue;
        };
        let Some(canonical_session_id) = plan_main["session_id"].as_str() else {
            continue;
        };
        let Some(canonical_incarnation) = plan_main["session_incarnation"].as_str() else {
            continue;
        };
        if crate::validate_id(canonical_session_id).is_err()
            || !(orchestration::GroupCleanupSelectorBinding {
                schema_version: orchestration::GROUP_CLEANUP_PROGRESS_RECEIPT_V1_SCHEMA,
                requested_session_id: None,
                stored_principal_session_id: &receipt.principal_session_id,
                canonical_session_id,
                stored_incarnation: &receipt.principal_incarnation,
                canonical_incarnation,
                expected_session_id: requested_session_id,
                expected_incarnation: incarnation,
            })
            .is_exact()
        {
            continue;
        }
        if recovered
            .as_deref()
            .is_some_and(|existing| existing != canonical_session_id)
        {
            return Err(CliError::data(
                "group-cleanup-progress-conflict",
                "multiple completed cleanup principals matched the requested session alias",
                None,
            ));
        }
        recovered = Some(canonical_session_id.to_string());
    }
    Ok(recovered)
}

fn remove_legacy_group_cleanup_progress(
    context: &CliContext,
    legacy_alias: Option<&str>,
    incarnation: &str,
    idempotency_key: &str,
) -> Result<(), CliError> {
    let Some(legacy_alias) = legacy_alias else {
        return Ok(());
    };
    orchestration::remove_group_cleanup_progress(
        context,
        &group_cleanup_progress_key(legacy_alias, incarnation, idempotency_key),
    )
}

fn decode_group_cleanup_replay(mut value: Value) -> Result<GroupCleanupReplay, CliError> {
    let resume = value
        .as_object_mut()
        .and_then(|object| object.remove("_resume"))
        .map(serde_json::from_value)
        .transpose()
        .map_err(|_| {
            CliError::data(
                "group-cleanup-progress-invalid",
                "durable group cleanup progress is invalid",
                None,
            )
        })?;
    Ok(GroupCleanupReplay { value, resume })
}

fn group_cleanup_progress_key(
    main_session_id: &str,
    incarnation: &str,
    idempotency_key: &str,
) -> String {
    crate::coordination::request_digest(
        "main-agent-group-cleanup-progress-key",
        &json!({
            "main_session_id": main_session_id,
            "incarnation": incarnation,
            "idempotency_key": idempotency_key,
        }),
    )
}

fn group_cleanup_stored_outcome(
    value: &Value,
    resume: &GroupCleanupResumeState,
) -> Result<Value, CliError> {
    let mut stored = value.clone();
    let object = stored.as_object_mut().ok_or_else(|| {
        CliError::runtime(
            "group-cleanup-progress-invalid",
            "group cleanup result is not an object",
            None,
        )
    })?;
    object.insert(
        "_resume".to_string(),
        serde_json::to_value(resume).map_err(|_| {
            CliError::runtime(
                "group-cleanup-progress-invalid",
                "group cleanup progress could not be serialized",
                None,
            )
        })?,
    );
    Ok(stored)
}

fn group_cleanup_resume_state(
    plan: &GroupCleanupPlan,
    worker_results: &[Value],
    deleted_registry_fences: &[SessionRegistryFence],
    pending_registry_fences: &[SessionRegistryFence],
    run_closed: bool,
) -> GroupCleanupResumeState {
    GroupCleanupResumeState {
        schema_version: "agent-session.main-agent-group-cleanup-progress.v1".to_string(),
        plan: plan.clone(),
        authority_sealed: true,
        worker_results: worker_results.to_vec(),
        deleted_registry_fences: deleted_registry_fences.to_vec(),
        pending_registry_fences: pending_registry_fences.to_vec(),
        run_closed,
    }
}

fn group_cleanup_progress_value(
    plan: &GroupCleanupPlan,
    worker_results: &[Value],
    run_closed: bool,
    stage: &str,
) -> Value {
    json!({
        "schema_version": GROUP_CLEANUP_RESULT_SCHEMA,
        "run_id": plan.run_id,
        "completed": false,
        "run_closed": run_closed,
        "main_deleted": false,
        "workers": worker_results,
        "progress": { "stage": stage },
    })
}

fn interrupt_group_cleanup_for_test(context: &CliContext, stage: &str) -> Result<(), CliError> {
    #[cfg(debug_assertions)]
    if fs::read_to_string(context.state_dir.join("group-cleanup-interrupt-test"))
        .ok()
        .as_deref()
        == Some(stage)
    {
        return Err(CliError::runtime(
            "group-cleanup-test-interrupted",
            "group cleanup interrupted after its durable stage checkpoint",
            Some(json!({ "stage": stage })),
        ));
    }
    let _ = context;
    Ok(())
}

fn store_group_cleanup_receipt(
    context: &CliContext,
    identity: &GroupCleanupProgressIdentity<'_>,
    request: &GroupCleanupRequest,
    request_digest: &str,
    value: Value,
    resume: GroupCleanupResumeState,
) -> Result<(), CliError> {
    let outcome = group_cleanup_stored_outcome(&value, &resume)?;
    let progress_key = group_cleanup_progress_key(
        identity.principal_session_id,
        identity.incarnation,
        &request.idempotency_key,
    );
    if value["completed"] == true {
        return store_completed_group_cleanup_receipt(
            context,
            identity.principal_session_id,
            (identity.requested_session_id != identity.principal_session_id)
                .then_some(identity.requested_session_id),
            identity.incarnation,
            request,
            request_digest,
            outcome,
        );
    }
    let progress = GroupCleanupProgressReceipt {
        schema_version: orchestration::GROUP_CLEANUP_PROGRESS_RECEIPT_SCHEMA.to_string(),
        requested_session_id: Some(identity.requested_session_id.to_string()),
        principal_session_id: identity.principal_session_id.to_string(),
        principal_incarnation: identity.incarnation.to_string(),
        idempotency_key: request.idempotency_key.clone(),
        request_digest: request_digest.to_string(),
        outcome,
    };
    let bytes = serde_json::to_vec(&progress).map_err(|_| {
        CliError::runtime(
            "group-cleanup-progress-invalid",
            "group cleanup progress could not be serialized",
            None,
        )
    })?;
    orchestration::store_group_cleanup_progress(context, &progress_key, &bytes)
}

#[allow(clippy::too_many_arguments)]
fn store_completed_group_cleanup_receipt(
    context: &CliContext,
    principal_session_id: &str,
    legacy_alias: Option<&str>,
    incarnation: &str,
    request: &GroupCleanupRequest,
    request_digest: &str,
    outcome: Value,
) -> Result<(), CliError> {
    let mut locked = orchestration::lock_registry(context)?;
    let canonical_key = receipt_key(principal_session_id, incarnation, &request.idempotency_key);
    let legacy_key =
        legacy_alias.map(|alias| receipt_key(alias, incarnation, &request.idempotency_key));
    let destinations = legacy_key
        .iter()
        .chain(std::iter::once(&canonical_key))
        .cloned()
        .collect::<Vec<_>>();
    for (key, expected_principal) in legacy_key
        .as_ref()
        .zip(legacy_alias)
        .into_iter()
        .chain(std::iter::once((&canonical_key, principal_session_id)))
    {
        if let Some(existing) = locked.registry.receipts.get(key)
            && (existing.principal_session_id != expected_principal
                || existing.principal_incarnation != incarnation
                || existing.operation != "group-cleanup"
                || existing.request_digest != request_digest)
        {
            return Err(CliError::data(
                "idempotency-conflict",
                "cleanup receipt conflicts with the completed canonical request",
                None,
            ));
        }
    }
    let new_destinations = destinations
        .iter()
        .filter(|key| !locked.registry.receipts.contains_key(*key))
        .count();
    while locked
        .registry
        .receipts
        .len()
        .saturating_add(new_destinations)
        > idempotency_receipt_capacity()
    {
        let victim = locked
            .registry
            .receipts
            .iter()
            .filter(|(key, _)| !destinations.contains(key))
            .min_by_key(|(_, receipt)| receipt.created_at_epoch)
            .map(|(key, _)| key.clone())
            .ok_or_else(|| {
                CliError::unavailable(
                    "orchestration-store-capacity",
                    "orchestration receipt capacity is exhausted",
                    None,
                )
            })?;
        locked.registry.receipts.remove(&victim);
    }
    let created_at_epoch = crate::coordination::now_epoch();
    if let (Some(legacy_alias), Some(legacy_key)) = (legacy_alias, legacy_key) {
        locked.registry.receipts.insert(
            legacy_key,
            IdempotencyReceipt {
                principal_session_id: legacy_alias.to_string(),
                principal_incarnation: incarnation.to_string(),
                operation: "group-cleanup".to_string(),
                request_digest: request_digest.to_string(),
                outcome: outcome.clone(),
                created_at_epoch,
            },
        );
    }
    locked.registry.receipts.insert(
        canonical_key,
        IdempotencyReceipt {
            principal_session_id: principal_session_id.to_string(),
            principal_incarnation: incarnation.to_string(),
            operation: "group-cleanup".to_string(),
            request_digest: request_digest.to_string(),
            outcome,
            created_at_epoch,
        },
    );
    locked.save()?;
    drop(locked);
    orchestration::remove_group_cleanup_progress(
        context,
        &group_cleanup_progress_key(principal_session_id, incarnation, &request.idempotency_key),
    )?;
    remove_legacy_group_cleanup_progress(
        context,
        legacy_alias,
        incarnation,
        &request.idempotency_key,
    )
}

fn group_cleanup_failure(
    plan: &GroupCleanupPlan,
    prior_results: &[Value],
    failed_worker: Option<&GroupCleanupWorkerPlan>,
    stage: &str,
    error: &CliError,
    run_closed: bool,
) -> Value {
    let mut workers = prior_results.to_vec();
    if let Some(worker) = failed_worker {
        workers.push(json!({
            "assignment_id": worker.assignment_id,
            "session_id": worker.worker.as_ref().map(|item| item.session_id.as_str()),
            "outcome": "failed",
            "cleanup_pending": false,
            "error_code": error.code(),
        }));
    }
    json!({
        "schema_version": GROUP_CLEANUP_RESULT_SCHEMA,
        "run_id": plan.run_id,
        "completed": false,
        "run_closed": run_closed,
        "main_deleted": false,
        "workers": workers,
        "failure": {
            "stage": stage,
            "code": error.code(),
            "message": error.message(),
        },
    })
}

fn build_group_cleanup_plan(
    registry: &orchestration::Registry,
    run: &RunRecord,
    main: &SessionRef,
) -> Result<GroupCleanupPlan, CliError> {
    if run.state != "active" || run.controller != *main {
        return Err(CliError::data(
            "main-agent-run-conflict",
            "Main Agent run does not match the requested controller",
            None,
        ));
    }
    let mut workers = registry
        .assignments
        .values()
        .filter(|assignment| assignment.run_id == run.run_id && assignment.primary_manager == *main)
        .map(|assignment| GroupCleanupWorkerPlan {
            assignment_id: assignment.assignment_id.clone(),
            state: assignment.state.clone(),
            worker: assignment.worker.clone(),
            force_required: !matches!(
                assignment.state.as_str(),
                "accepted" | "released" | "cancelled"
            ),
            primary_managed: true,
        })
        .collect::<Vec<_>>();
    if workers.len() > GROUP_CLEANUP_MAX_ASSIGNMENTS {
        return Err(CliError::data(
            "group-cleanup-batch-too-large",
            "group cleanup is bounded to 64 primary assignments so resumable per-worker checkpoints and lock hold times remain bounded",
            Some(json!({
                "assignment_count": workers.len(),
                "maximum_assignment_count": GROUP_CLEANUP_MAX_ASSIGNMENTS
            })),
        ));
    }
    workers.sort_by(|left, right| left.assignment_id.cmp(&right.assignment_id));
    let requires_force = workers.iter().any(|worker| worker.force_required);
    let digest = format!(
        "sha256:{}",
        crate::coordination::request_digest(
            "main-agent-group-cleanup-plan",
            &json!({
                "schema_version": GROUP_CLEANUP_SCHEMA,
                "main": main,
                "run_id": run.run_id,
                "run_revision": run.revision,
                "requires_force": requires_force,
                "workers": workers,
            }),
        )
    );
    Ok(GroupCleanupPlan {
        schema_version: GROUP_CLEANUP_SCHEMA.to_string(),
        main: main.clone(),
        run_id: run.run_id.clone(),
        run_revision: run.revision,
        requires_force,
        workers,
        plan_digest: digest,
    })
}

fn prepare_group_cleanup_assignments(
    registry: &mut orchestration::Registry,
    run: &RunRecord,
    main: &SessionRef,
    mode: GroupCleanupMode,
) -> Result<(), CliError> {
    let transitions = group_cleanup_assignment_transitions(registry, run, main, mode)?;
    for (assignment_id, next_state, next_revision) in transitions {
        let assignment = registry
            .assignments
            .get_mut(&assignment_id)
            .expect("group cleanup assignment transition was preflighted");
        assignment.state = next_state;
        assignment.revision = next_revision;
        assignment.updated_at = timestamp();
    }
    Ok(())
}

fn group_cleanup_assignment_transitions(
    registry: &orchestration::Registry,
    run: &RunRecord,
    main: &SessionRef,
    mode: GroupCleanupMode,
) -> Result<Vec<(String, String, u64)>, CliError> {
    let force_required = registry
        .assignments
        .values()
        .filter(|assignment| assignment.run_id == run.run_id && assignment.primary_manager == *main)
        .filter(|assignment| {
            !matches!(
                assignment.state.as_str(),
                "accepted" | "released" | "cancelled"
            )
        })
        .map(|assignment| assignment.assignment_id.clone())
        .collect::<Vec<_>>();
    if mode == GroupCleanupMode::Safe && !force_required.is_empty() {
        return Err(CliError::data(
            "group-cleanup-force-required",
            "group cleanup includes nonterminal assignments and requires explicit force",
            Some(json!({ "assignment_ids": force_required })),
        ));
    }
    for assignment in registry
        .assignments
        .values()
        .filter(|assignment| assignment.run_id == run.run_id && assignment.primary_manager == *main)
    {
        ensure_submit_recovery_not_in_flight(assignment)?;
        ensure_account_handoff_not_in_flight(assignment)?;
    }
    registry
        .assignments
        .values()
        .filter(|assignment| assignment.run_id == run.run_id && assignment.primary_manager == *main)
        .filter_map(|assignment| {
            let next = match assignment.state.as_str() {
                "accepted" => Some("released"),
                "released" | "cancelled" => None,
                _ if mode == GroupCleanupMode::Force => Some("cancelled"),
                _ => None,
            }?;
            Some(
                assignment
                    .revision
                    .checked_add(1)
                    .map(|revision| (assignment.assignment_id.clone(), next.to_string(), revision))
                    .ok_or_else(|| {
                        CliError::data(
                            "orchestration-revision-capacity",
                            "assignment revision reached its maximum value",
                            Some(json!({
                                "assignment_id": assignment.assignment_id,
                                "current_revision": assignment.revision
                            })),
                        )
                    }),
            )
        })
        .collect()
}

/// Fast-path for L0/L1 delegate-all: acquire the claim, create an ephemeral
/// run synthesized from the assignment packet, then launch the assignment's
/// worker — all in one call. The run is marked ephemeral so it auto-closes when
/// the worker is torn down (see `finalize_worker_delete`), sparing the caller an
/// explicit `close`. A session that already controls a run must use the
/// granular `init` + `worker start` path instead.
/// A closed run is history, not control. Treating it as control permanently
/// locked a session out of delegating again: `quick` refused with
/// `quick-run-exists`, and its documented fallback was unreachable because the
/// objective packet `quick` synthesizes is never handed to the caller, so
/// `init --if-absent` could only report `run-objective-conflict`.
fn run_is_live(run: &RunRecord) -> bool {
    run.state == "active"
}

fn session_controls_live_run(
    registry: &orchestration::Registry,
    session_id: &str,
    session_created_at: &str,
) -> bool {
    registry.runs.values().any(|run| {
        run_is_live(run)
            && run.controller.session_id == session_id
            && run.controller.session_created_at == session_created_at
    })
}

fn run_quick(context: &CliContext, args: QuickArgs) -> Result<Value, CliError> {
    if !matches!(args.tier.as_str(), "L0" | "L1" | "L2" | "L3") {
        return Err(invalid_input("quick tier is invalid"));
    }
    // Reject a malformed duration before the ephemeral run exists, so a typo
    // cannot leave a created run behind for the caller to clean up.
    let await_ready_seconds = parse_await_ready(&args.await_ready)?
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let input: AssignmentInput = crate::coordination::read_bounded_json(
        &args.assignment_file,
        256 * 1024,
        "invalid-assignment-packet",
    )?;
    validate_assignment_input(&input)?;
    // The fast-path caller supplies only one packet: default the idempotency key
    // from a digest of that packet when --idempotency-key is omitted. An explicit
    // key still wins and is validated exactly as before.
    let idempotency_key = match args.idempotency_key.as_deref() {
        Some(key) => {
            validate_idempotency_key(key)?;
            key.to_string()
        }
        None => default_quick_idempotency_key(&input),
    };
    let repository = input.repository.clone().ok_or_else(|| {
        invalid_input("quick requires the assignment packet to declare a repository")
    })?;
    let (record, incarnation) = authenticated_self(context)?;

    // Synthesize the ephemeral run's objective + work-context claim from the
    // assignment so the fast-path caller supplies only one packet.
    let work_context = WorkContextInput {
        schema_version: WORK_CONTEXT_INPUT_VERSION.to_string(),
        intent: "implementation".to_string(),
        tier: args.tier.clone(),
        repositories: vec![repository.clone()],
        worktrees: input.worktree.clone().into_iter().collect(),
        provider_refs: Vec::new(),
        plan_refs: Vec::new(),
        scopes: input
            .scopes
            .iter()
            .map(|value| Scope {
                kind: ScopeKind::PathPrefix,
                repository: repository.clone(),
                value: value.clone(),
            })
            .collect(),
        summary: input.task_summary.clone(),
    };
    ensure_or_acquire_claim(
        context,
        &record,
        &work_context,
        &idempotency_key,
        None,
        false,
    )?;

    let objective = json!({
        "schema_version": PACKET_SCHEMA,
        "tier": args.tier,
        "objective_summary": input.task_summary,
        "objective": {},
        "done_criteria": [],
        "constraints": [],
        "durable_refs": input.durable_refs,
        "work_context": work_context,
        "next_action": null,
    });
    let legacy_request_digest = crate::coordination::request_digest(
        "main-agent-quick",
        &json!({ "objective": objective, "assignment": input }),
    );
    let request_digest = crate::coordination::request_digest(
        "main-agent-quick-v2",
        &json!({
            "objective": objective,
            "assignment": input,
            "await_ready_seconds": await_ready_seconds
        }),
    );

    let run_id = {
        let mut locked = orchestration::lock_registry(context)?;
        match idempotency_replay_compatible(
            &locked.registry,
            &record,
            &incarnation,
            &idempotency_key,
            "quick",
            &[&request_digest, &legacy_request_digest],
        )? {
            Some(value) if value["schema_version"] == "main-agent.quick-result.v1" => {
                return Ok(value);
            }
            Some(value) => value["run"]["run_id"]
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| invalid_input("pending quick receipt is invalid"))?,
            None => {
                if session_controls_live_run(&locked.registry, &record.id, &record.created_at) {
                    return Err(CliError::data(
                        "quick-run-exists",
                        "this session already controls a run; use init and worker start",
                        None,
                    ));
                }
                let run_id = uuid::Uuid::new_v4().to_string();
                let packet_digest = orchestration::store_packet(context, &objective)?;
                let now = timestamp();
                let run = RunRecord {
                    schema_version: orchestration::RUN_SCHEMA.to_string(),
                    run_id: run_id.clone(),
                    revision: 1,
                    state: "active".to_string(),
                    tier: args.tier.clone(),
                    objective_summary: input.task_summary.clone(),
                    objective_packet_digest: packet_digest,
                    controller: session_ref(context, &record, &incarnation),
                    durable_refs: input.durable_refs.clone(),
                    ephemeral: true,
                    checkpoint: None,
                    created_at: now.clone(),
                    updated_at: now,
                };
                let pending = json!({
                    "schema_version": "main-agent.quick-pending.v1",
                    "run": public_run_view(&run),
                });
                locked.registry.runs.insert(run_id.clone(), run);
                store_receipt(
                    &mut locked.registry,
                    &record,
                    &incarnation,
                    &idempotency_key,
                    "quick",
                    &request_digest,
                    pending,
                )?;
                locked.save()?;
                run_id
            }
        }
    };

    // Launch the single assignment on the freshly created ephemeral run.
    let worker = run_worker_start_single(
        context,
        WorkerStartArgs {
            assignment_file: Some(args.assignment_file.clone()),
            batch: None,
            if_run_revision: None,
            idempotency_key: compatible_child_idempotency_key(&idempotency_key, "worker"),
            await_ready: canonical_await_ready_arg(await_ready_seconds),
            format: OutputFormat::Json,
        },
    )?;

    let run_view = {
        let registry = orchestration::load_registry_readonly(context)?;
        registry
            .runs
            .get(&run_id)
            .map(public_run_view)
            .ok_or_else(|| not_found("run-not-found", "ephemeral run was not found"))?
    };
    let outcome = json!({
        "schema_version": "main-agent.quick-result.v1",
        "run": run_view,
        "worker": worker,
    });
    let mut locked = orchestration::lock_registry(context)?;
    store_receipt(
        &mut locked.registry,
        &record,
        &incarnation,
        &idempotency_key,
        "quick",
        &request_digest,
        outcome.clone(),
    )?;
    locked.save()?;
    Ok(outcome)
}

fn canonical_await_ready_arg(seconds: u64) -> String {
    if seconds == 0 {
        "0".to_string()
    } else {
        format!("{seconds}s")
    }
}

/// Close an ephemeral run once none of its assignments remain non-terminal.
/// Returns whether the run transitioned to closed. Only ephemeral runs (created
/// by `main-agent quick`) auto-close; this lets the fast-path skip `close`.
fn maybe_autoclose_ephemeral_run(registry: &mut orchestration::Registry, run_id: &str) -> bool {
    let eligible = registry
        .runs
        .get(run_id)
        .is_some_and(|run| run.ephemeral && run.state == "active");
    if !eligible {
        return false;
    }
    let has_nonterminal = registry.assignments.values().any(|assignment| {
        assignment.run_id == run_id
            && !matches!(assignment.state.as_str(), "released" | "cancelled")
    });
    if has_nonterminal {
        return false;
    }
    if let Some(run) = registry.runs.get_mut(run_id) {
        run.state = "closed".to_string();
        run.revision = run.revision.saturating_add(1);
        run.updated_at = timestamp();
        return true;
    }
    false
}

fn run_relationship_mutation<F>(
    context: &CliContext,
    assignment_id: String,
    if_revision: u64,
    idempotency_key: String,
    operation: &'static str,
    mutate: F,
) -> Result<Value, CliError>
where
    F: FnOnce(&mut AssignmentRecord) -> Result<(), CliError>,
{
    validate_idempotency_key(&idempotency_key)?;
    let (record, incarnation) = authenticated_self(context)?;
    ensure_active_claim(context, &record)?;
    let mut locked = orchestration::lock_registry(context)?;
    let run_id = require_current_main(&locked.registry, &record, &incarnation)?
        .run_id
        .clone();
    let request_digest = crate::coordination::request_digest(
        operation,
        &json!({ "assignment_id": assignment_id, "if_revision": if_revision }),
    );
    if let Some(value) = idempotency_replay(
        &locked.registry,
        &record,
        &incarnation,
        &idempotency_key,
        operation,
        &request_digest,
    )? {
        return Ok(value);
    }
    let current = locked
        .registry
        .assignments
        .get_mut(&assignment_id)
        .filter(|assignment| assignment.run_id == run_id)
        .ok_or_else(|| not_found("assignment-not-found", "assignment was not found"))?;
    ensure_primary_manager(current, &record, &incarnation)?;
    ensure_revision(if_revision, current.revision, "assignment")?;
    ensure_submit_recovery_not_in_flight(current)?;
    ensure_account_handoff_not_in_flight(current)?;
    mutate(current)?;
    current.revision = current.revision.saturating_add(1);
    current.updated_at = timestamp();
    let outcome = json!({
        "schema_version": "main-agent.relationship-mutation-result.v1",
        "assignment": public_assignment_view(current)
    });
    store_receipt(
        &mut locked.registry,
        &record,
        &incarnation,
        &idempotency_key,
        operation,
        &request_digest,
        outcome.clone(),
    )?;
    locked.save()?;
    Ok(outcome)
}

fn authenticated_self(context: &CliContext) -> Result<(SessionRecord, String), CliError> {
    crate::coordination::authenticate_any_from_file(context, None)
}

fn ensure_active_claim(context: &CliContext, record: &SessionRecord) -> Result<(), CliError> {
    crate::coordination::claims::show(
        context,
        cli::WorkContextShowArgs {
            session: record.id.clone(),
            capability_file: None,
            format: OutputFormat::Json,
        },
    )?;
    Ok(())
}

fn ensure_or_acquire_claim(
    context: &CliContext,
    record: &SessionRecord,
    candidate: &WorkContextInput,
    idempotency_key: &str,
    rebind_from: Option<&SessionRef>,
    checkout_shell_grant: bool,
) -> Result<(), CliError> {
    if checkout_shell_grant {
        match crate::coordination::claims::main_agent_worker_claim_match(
            context, record, candidate,
        )? {
            Some(true) => return Ok(()),
            Some(false) => {
                return Err(CliError::data(
                    "worker-bootstrap-claim-mismatch",
                    "worker bootstrap requires the exact assignment-derived claim and checkout-shell grant",
                    None,
                ));
            }
            None => {}
        }
    } else {
        match ensure_active_claim(context, record) {
            Ok(()) => return Ok(()),
            Err(error) if error.code() == "claim-not-active" => {}
            Err(error) => return Err(error),
        }
    }
    let directory = session_dir(context, &record.id).join("coordination");
    fs::create_dir_all(&directory)
        .map_err(|_| invalid_input("claim input directory is unavailable"))?;
    let candidate_path = directory.join(format!("main-agent-init-{}.json", uuid::Uuid::new_v4()));
    let bytes =
        serde_json::to_vec(candidate).map_err(|_| invalid_input("work context is invalid"))?;
    write_atomic(&candidate_path, &bytes, SECRET_FILE_MODE)
        .map_err(|_| invalid_input("claim input could not be prepared"))?;
    let claim_args = cli::WorkContextClaimArgs {
        session: record.id.clone(),
        file: candidate_path.clone(),
        capability_file: None,
        idempotency_key: idempotency_key.to_string(),
        if_revision: None,
        format: OutputFormat::Json,
    };
    let result = if checkout_shell_grant {
        crate::coordination::claims::claim_main_agent_worker(
            context,
            claim_args,
            rebind_from.map(|previous| previous.session_incarnation.as_str()),
        )
    } else {
        crate::coordination::claims::claim(context, claim_args)
    };
    let _ = fs::remove_file(candidate_path);
    result.map(|_| ())
}

fn resolve_principal(
    registry: &orchestration::Registry,
    record: &SessionRecord,
    incarnation: &str,
) -> Result<Principal, CliError> {
    if let Some(run) = registry.runs.values().find(|run| {
        run.controller.session_id == record.id
            && run.controller.session_created_at == record.created_at
    }) {
        return Ok(Principal::Main {
            run: Box::new(run.clone()),
            rebind_required: run.controller.session_incarnation != incarnation,
        });
    }
    if let Some(assignment) = registry.assignments.values().find(|assignment| {
        assignment.worker.as_ref().is_some_and(|worker| {
            worker.session_id == record.id && worker.session_created_at == record.created_at
        })
    }) {
        return Ok(Principal::Worker {
            assignment: Box::new(assignment.clone()),
            rebind_required: assignment
                .worker
                .as_ref()
                .is_some_and(|worker| worker.session_incarnation != incarnation),
        });
    }
    Err(not_found(
        "orchestration-self-not-found",
        "authenticated session has no orchestration relationship",
    ))
}

fn require_current_main<'a>(
    registry: &'a orchestration::Registry,
    record: &SessionRecord,
    incarnation: &str,
) -> Result<&'a RunRecord, CliError> {
    registry
        .runs
        .values()
        .find(|run| orchestration::session_ref_matches(&run.controller, record, incarnation))
        .filter(|run| run.state == "active")
        .ok_or_else(rebind_required)
}

fn ensure_primary_manager(
    assignment: &AssignmentRecord,
    record: &SessionRecord,
    incarnation: &str,
) -> Result<(), CliError> {
    if orchestration::session_ref_matches(&assignment.primary_manager, record, incarnation) {
        Ok(())
    } else {
        Err(CliError::data(
            "primary-manager-conflict",
            "authenticated Main Agent is not the assignment primary manager",
            Some(
                json!({ "assignment_id": assignment.assignment_id, "revision": assignment.revision }),
            ),
        ))
    }
}

fn session_ref(context: &CliContext, record: &SessionRecord, incarnation: &str) -> SessionRef {
    SessionRef {
        machine: context.host.clone(),
        session_id: record.id.clone(),
        session_incarnation: incarnation.to_string(),
        session_created_at: record.created_at.clone(),
    }
}

fn resolve_live_session_ref(context: &CliContext, value: &str) -> Result<SessionRef, CliError> {
    let (id, expected_incarnation) = value
        .split_once('@')
        .ok_or_else(|| invalid_input("session ref must be SESSION_ID@SESSION_INCARNATION"))?;
    crate::validate_id(id)?;
    orchestration::validate_slug("session incarnation", expected_incarnation, 128)?;
    let record = load_session_record(context, id)?;
    let actual = record
        .runtime
        .as_ref()
        .map(|runtime| runtime.launch_id.as_str())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| invalid_input("session incarnation is unavailable"))?;
    if actual != expected_incarnation {
        return Err(CliError::data(
            "session-incarnation-conflict",
            "session incarnation fence did not match",
            Some(json!({ "session_id": id })),
        ));
    }
    Ok(session_ref(context, &record, actual))
}

fn public_run_view(run: &RunRecord) -> Value {
    json!({
        "schema_version": run.schema_version,
        "run_id": run.run_id,
        "revision": run.revision,
        "state": run.state,
        "tier": run.tier,
        "objective_summary": run.objective_summary,
        "controller": run.controller,
        "durable_refs": run.durable_refs,
        "ephemeral": run.ephemeral,
        "checkpoint": run.checkpoint,
        "created_at": run.created_at,
        "updated_at": run.updated_at
    })
}

fn private_run_view(context: &CliContext, run: &RunRecord) -> Result<Value, CliError> {
    let packet = orchestration::read_packet(context, &run.objective_packet_digest)?;
    Ok(json!({
        "record": public_run_view(run),
        "objective_packet": packet
    }))
}

fn public_assignment_view(assignment: &AssignmentRecord) -> Value {
    let now = crate::coordination::now_epoch();
    let borrowed_by = assignment
        .borrowed_by
        .iter()
        .filter(|relationship| relationship.expires_at_epoch > now)
        .map(|relationship| &relationship.session)
        .collect::<Vec<_>>();
    json!({
        "schema_version": assignment.schema_version,
        "assignment_id": assignment.assignment_id,
        "run_id": assignment.run_id,
        "revision": assignment.revision,
        "state": assignment.state,
        "task_summary": assignment.task_summary,
        "primary_manager": assignment.primary_manager,
        "worker": assignment.worker,
        "previous_worker": assignment.previous_worker,
        "collaborators": assignment.collaborators,
        "borrowed_by": borrowed_by,
        "repository": assignment.repository,
        "worktree": assignment.worktree,
        "base_ref": assignment.base_ref,
        "scopes": assignment.scopes,
        "durable_refs": assignment.durable_refs,
        "depends_on": assignment.depends_on,
        "checkpoint": assignment.checkpoint,
        "result_summary": assignment.result_summary,
        "blocker_summary": assignment.blocker_summary,
        "submit_recovery": assignment.submit_recovery,
        "worker_quarantine": assignment.worker_quarantine,
        "account_handoff": assignment.account_handoff,
        "created_at": assignment.created_at,
        "updated_at": assignment.updated_at
    })
}

fn private_assignment_view(
    context: &CliContext,
    assignment: &AssignmentRecord,
) -> Result<Value, CliError> {
    let packet = orchestration::read_packet(context, &assignment.private_packet_digest)?;
    Ok(json!({
        "record": public_assignment_view(assignment),
        "assignment_packet": packet
    }))
}

fn run_outcome(run: &RunRecord, rebound: bool) -> Value {
    json!({
        "schema_version": "main-agent.init-result.v1",
        "run": public_run_view(run),
        "rebound": rebound
    })
}

fn receipt_key(session_id: &str, incarnation: &str, idempotency_key: &str) -> String {
    format!("{session_id}:{incarnation}:{idempotency_key}")
}

fn idempotency_replay(
    registry: &orchestration::Registry,
    record: &SessionRecord,
    incarnation: &str,
    idempotency_key: &str,
    operation: &str,
    request_digest: &str,
) -> Result<Option<Value>, CliError> {
    let key = receipt_key(&record.id, incarnation, idempotency_key);
    let Some(receipt) = registry.receipts.get(&key) else {
        return Ok(None);
    };
    if receipt.operation != operation || receipt.request_digest != request_digest {
        return Err(CliError::data(
            "idempotency-conflict",
            "idempotency key was already used for a different request",
            None,
        ));
    }
    Ok(Some(receipt.outcome.clone()))
}

fn worker_start_idempotency_replay(
    registry: &orchestration::Registry,
    record: &SessionRecord,
    incarnation: &str,
    idempotency_key: &str,
    request_digest: &str,
    legacy_request_digest: &str,
) -> Result<Option<Value>, CliError> {
    idempotency_replay_compatible(
        registry,
        record,
        incarnation,
        idempotency_key,
        "worker-start",
        &[request_digest, legacy_request_digest],
    )
}

fn idempotency_replay_compatible(
    registry: &orchestration::Registry,
    record: &SessionRecord,
    incarnation: &str,
    idempotency_key: &str,
    operation: &str,
    request_digests: &[&str],
) -> Result<Option<Value>, CliError> {
    let key = receipt_key(&record.id, incarnation, idempotency_key);
    let Some(receipt) = registry.receipts.get(&key) else {
        return Ok(None);
    };
    if receipt.operation != operation || !request_digests.contains(&receipt.request_digest.as_str())
    {
        return Err(CliError::data(
            "idempotency-conflict",
            "idempotency key was already used for a different request",
            None,
        ));
    }
    Ok(Some(receipt.outcome.clone()))
}

#[allow(clippy::too_many_arguments)]
fn store_receipt(
    registry: &mut orchestration::Registry,
    record: &SessionRecord,
    incarnation: &str,
    idempotency_key: &str,
    operation: &str,
    request_digest: &str,
    outcome: Value,
) -> Result<(), CliError> {
    store_receipt_for_principal(
        registry,
        &record.id,
        incarnation,
        idempotency_key,
        operation,
        request_digest,
        outcome,
    )
}

#[allow(clippy::too_many_arguments)]
fn store_receipt_for_principal(
    registry: &mut orchestration::Registry,
    principal_session_id: &str,
    incarnation: &str,
    idempotency_key: &str,
    operation: &str,
    request_digest: &str,
    outcome: Value,
) -> Result<(), CliError> {
    let key = receipt_key(principal_session_id, incarnation, idempotency_key);
    if !registry.receipts.contains_key(&key)
        && registry.receipts.len() >= idempotency_receipt_capacity()
    {
        let oldest = registry
            .receipts
            .iter()
            .min_by_key(|(_, receipt)| receipt.created_at_epoch)
            .map(|(key, _)| key.clone());
        if let Some(oldest) = oldest {
            registry.receipts.remove(&oldest);
        }
    }
    registry.receipts.insert(
        key,
        IdempotencyReceipt {
            principal_session_id: principal_session_id.to_string(),
            principal_incarnation: incarnation.to_string(),
            operation: operation.to_string(),
            request_digest: request_digest.to_string(),
            outcome,
            created_at_epoch: crate::coordination::now_epoch(),
        },
    );
    Ok(())
}

fn idempotency_receipt_capacity() -> usize {
    #[cfg(test)]
    {
        IDEMPOTENCY_RECEIPT_CAPACITY_FOR_TEST.load(Ordering::Acquire)
    }
    #[cfg(not(test))]
    {
        MAX_IDEMPOTENCY_RECEIPTS
    }
}

/// A ready-to-edit example objective packet, printed by `main-agent
/// packet-schema` so operators can discover the required fields (and the two
/// nested schema_version constants) without reverse-engineering a validation
/// error. Illustrative placeholders, not a live packet.
fn objective_packet_schema_example() -> Value {
    json!({
        "schema_version": PACKET_SCHEMA,
        "tier": "L0",
        "objective_summary": "<one-line objective summary>",
        "objective": {},
        "done_criteria": ["<done criterion>"],
        "constraints": ["<constraint>"],
        "durable_refs": [],
        "next_action": null,
        "work_context": {
            "schema_version": WORK_CONTEXT_INPUT_VERSION,
            "intent": "implementation",
            "tier": "L0",
            "repositories": ["owner/name"],
            "summary": "<work-context summary>"
        }
    })
}

/// Derive a stable, slug-safe idempotency key from the assignment packet so the
/// `quick` fast-path caller can omit `--idempotency-key`. Identical packets map
/// to the same key, preserving idempotent replay; the `quick-` prefix plus 32
/// hex digits satisfies the 8-128 printable-ASCII key rule.
fn default_quick_idempotency_key(input: &AssignmentInput) -> String {
    let digest = crate::coordination::request_digest("main-agent-quick-idempotency", input);
    format!("quick-{}", &digest[..32])
}

fn validate_objective_packet(packet: &ObjectivePacket) -> Result<(), CliError> {
    if packet.schema_version != PACKET_SCHEMA {
        return Err(invalid_input(&format!(
            "objective packet schema is unsupported; expected schema_version \"{PACKET_SCHEMA}\""
        ))
        .with_hint("run `main-agent packet-schema` for an example objective packet"));
    }
    orchestration::validate_summary("objective summary", &packet.objective_summary)?;
    if !matches!(packet.tier.as_str(), "L0" | "L1" | "L2" | "L3") {
        return Err(invalid_input("objective packet tier is invalid"));
    }
    if packet.done_criteria.len() > 64
        || packet.constraints.len() > 64
        || packet.durable_refs.len() > 64
    {
        return Err(invalid_input("objective packet exceeds collection limits"));
    }
    packet.work_context.clone().validate_and_canonicalize()?;
    if let Some(next_action) = &packet.next_action {
        orchestration::validate_summary("next action", next_action)?;
    }
    Ok(())
}

fn validate_assignment_input(input: &AssignmentInput) -> Result<(), CliError> {
    if input.schema_version != ASSIGNMENT_INPUT_SCHEMA {
        return Err(invalid_input(&format!(
            "assignment packet schema is unsupported; expected schema_version \"{ASSIGNMENT_INPUT_SCHEMA}\""
        )));
    }
    orchestration::validate_summary("task summary", &input.task_summary)?;
    if input.scopes.len() > 32
        || input.durable_refs.len() > 64
        || input.launch.agent_args.len() > 64
        || input.depends_on.len() > 64
    {
        return Err(invalid_input("assignment packet exceeds collection limits"));
    }
    for dependency in &input.depends_on {
        orchestration::validate_slug("assignment dependency id", dependency, 128)?;
    }
    if AgentKind::from_name(&input.launch.agent).is_none() {
        return Err(invalid_input("assignment launch agent is invalid"));
    }
    if input.launch.cwd.trim().is_empty() || input.launch.cwd.len() > 4096 {
        return Err(invalid_input("assignment launch cwd is invalid"));
    }
    if let Some(id) = &input.launch.session_id {
        crate::validate_id(id)?;
    }
    Ok(())
}

fn validate_bootstrap_checkout_binding(
    record: &SessionRecord,
    assignment: &AssignmentRecord,
    input: &AssignmentInput,
) -> Result<(), CliError> {
    let declared = input.worktree.as_deref().ok_or_else(|| {
        invalid_input("worker bootstrap requires the assignment packet to declare a worktree")
    })?;
    let durable = assignment.worktree.as_deref().ok_or_else(|| {
        invalid_input("worker bootstrap requires the durable assignment to declare a worktree")
    })?;
    let declared_root = exact_assignment_checkout_root(declared, "assignment worktree")?;
    let durable_root = exact_assignment_checkout_root(durable, "durable assignment worktree")?;
    let launch_root = exact_assignment_checkout_root(&input.launch.cwd, "assignment launch cwd")?;
    let session_root = exact_assignment_checkout_root(&record.cwd, "worker session cwd")?;
    if declared_root != durable_root
        || declared_root != launch_root
        || declared_root != session_root
    {
        return Err(CliError::data(
            "worker-bootstrap-checkout-mismatch",
            "worker bootstrap requires the assignment worktree, launch cwd, durable worktree, and authenticated session cwd to resolve to the same checkout",
            None,
        ));
    }
    Ok(())
}

fn exact_assignment_checkout_root(raw: &str, label: &str) -> Result<PathBuf, CliError> {
    let path = Path::new(raw);
    if !path.is_absolute() {
        return Err(invalid_input(&format!("{label} must be an absolute path")));
    }
    let canonical =
        fs::canonicalize(path).map_err(|_| invalid_input(&format!("{label} is unavailable")))?;
    let root = checkout_root(&canonical)
        .map_err(|_| invalid_input(&format!("{label} is not a valid checkout")))?;
    if canonical != root {
        return Err(invalid_input(&format!(
            "{label} must name the checkout root"
        )));
    }
    Ok(root)
}

fn validate_checkpoint(input: &CheckpointInput) -> Result<(), CliError> {
    if input.schema_version != CHECKPOINT_INPUT_SCHEMA {
        return Err(invalid_input(&format!(
            "checkpoint schema is unsupported; expected schema_version \"{CHECKPOINT_INPUT_SCHEMA}\""
        )));
    }
    orchestration::validate_summary("checkpoint summary", &input.summary)?;
    orchestration::validate_summary("next action", &input.next_action)?;
    if let Some(value) = &input.result_summary {
        orchestration::validate_summary("result summary", value)?;
    }
    if let Some(value) = &input.blocker_summary {
        orchestration::validate_summary("blocker summary", value)?;
    }
    Ok(())
}

fn validate_idempotency_key(value: &str) -> Result<(), CliError> {
    orchestration::validate_slug("idempotency key", value, 128)
}

fn child_idempotency_key(parent: &str, stage: &str) -> String {
    let digest = crate::coordination::request_digest(
        "main-agent-child-idempotency",
        &json!({ "parent": parent, "stage": stage }),
    );
    format!("child-{stage}-{}", &digest[..32])
}

fn compatible_child_idempotency_key(parent: &str, stage: &str) -> String {
    let historical_key = format!("{parent}-{stage}");
    if validate_idempotency_key(&historical_key).is_ok() {
        historical_key
    } else {
        child_idempotency_key(parent, stage)
    }
}

fn batch_lane_idempotency_key(parent: &str, index: usize) -> String {
    let historical_key = format!("{parent}-{index}");
    if validate_idempotency_key(&historical_key).is_ok() {
        historical_key
    } else {
        child_idempotency_key(parent, &format!("lane-{index}"))
    }
}

fn ensure_revision(expected: u64, actual: u64, resource: &str) -> Result<(), CliError> {
    if expected == actual {
        Ok(())
    } else {
        Err(CliError::data(
            "orchestration-revision-conflict",
            "orchestration revision fence did not match",
            Some(json!({ "resource": resource, "current_revision": actual })),
        ))
    }
}

fn parse_bounded_duration(value: &str, max: u64) -> Result<u64, CliError> {
    let (digits, multiplier) = if let Some(value) = value.strip_suffix('s') {
        (value, 1)
    } else if let Some(value) = value.strip_suffix('m') {
        (value, 60)
    } else if let Some(value) = value.strip_suffix('h') {
        (value, 60 * 60)
    } else {
        return Err(invalid_input("duration must use s, m, or h"));
    };
    let number = digits
        .parse::<u64>()
        .map_err(|_| invalid_input("duration is invalid"))?;
    let seconds = number
        .checked_mul(multiplier)
        .ok_or_else(|| invalid_input("duration is invalid"))?;
    if seconds == 0 || seconds > max {
        return Err(invalid_input("duration is outside the allowed bound"));
    }
    Ok(seconds)
}

fn timestamp() -> String {
    Zoned::now().timestamp().to_string()
}

fn invalid_input(message: &str) -> CliError {
    CliError::data("invalid-orchestration-input", message, None)
}

fn not_found(code: &'static str, message: &'static str) -> CliError {
    CliError::data(code, message, None)
}

fn rebind_required() -> CliError {
    CliError::data(
        "controller-rebind-required",
        "controller incarnation changed; rerun init with the original packet and --if-absent",
        None,
    )
}

fn command_name(command: &MainAgentCommand) -> &'static str {
    match command {
        MainAgentCommand::Init(_) => "init",
        MainAgentCommand::Rebind(_) => "rebind",
        MainAgentCommand::SelfGroup(args) => match &args.command {
            SelfCommand::Show(_) => "self-show",
            SelfCommand::Recover(_) => "self-recover",
        },
        MainAgentCommand::Rehydrate(_) => "rehydrate",
        MainAgentCommand::Status(_) => "status",
        MainAgentCommand::Checkpoint(_) => "checkpoint",
        MainAgentCommand::Bootstrap(_) => "bootstrap",
        MainAgentCommand::Worker(args) => match args.command {
            WorkerCommand::Start(_) => "worker-start",
            WorkerCommand::List(_) => "worker-list",
            WorkerCommand::Show(_) => "worker-show",
            WorkerCommand::Wait(_) => "worker-wait",
            WorkerCommand::Message(_) => "worker-message",
            WorkerCommand::GuidanceReconcile(_) => "worker-guidance-reconcile",
            WorkerCommand::GuidanceQuarantine(_) => "worker-guidance-quarantine",
            WorkerCommand::AccountHandoff(_) => "worker-account-handoff",
            WorkerCommand::AccountHandoffCancel(_) => "worker-account-handoff-cancel",
            WorkerCommand::RequestChanges(_) => "worker-request-changes",
            WorkerCommand::Accept(_) => "worker-accept",
            WorkerCommand::Release(_) => "worker-release",
            WorkerCommand::Delete(_) => "worker-delete",
            WorkerCommand::Retire(_) => "worker-retire",
            WorkerCommand::Diagnose(_) => "worker-diagnose",
            WorkerCommand::Supervise(_) => "worker-supervise",
            WorkerCommand::SubmitRecovery(_) => "worker-submit-recovery",
            WorkerCommand::ReconcileRecovery(_) => "worker-reconcile-recovery",
            WorkerCommand::Cancel(_) => "worker-cancel",
            WorkerCommand::Reassign(_) => "worker-reassign",
        },
        MainAgentCommand::Collaborate(_) => "collaborate",
        MainAgentCommand::Borrow(_) => "borrow",
        MainAgentCommand::Handoff(_) => "handoff",
        MainAgentCommand::Adopt(_) => "adopt",
        MainAgentCommand::Close(_) => "close",
        MainAgentCommand::Quick(_) => "quick",
        MainAgentCommand::PacketSchema(_) => "packet-schema",
        MainAgentCommand::Completion(_) => "completion",
    }
}

fn command_output_format(command: &MainAgentCommand) -> OutputFormat {
    match command {
        MainAgentCommand::Init(args) => args.format,
        MainAgentCommand::Rebind(args) => args.format,
        MainAgentCommand::SelfGroup(args) => match &args.command {
            SelfCommand::Show(args) => args.format,
            SelfCommand::Recover(args) => args.format,
        },
        MainAgentCommand::Rehydrate(args) => match args.format {
            RehydrateFormat::Json => OutputFormat::Json,
            RehydrateFormat::Markdown => OutputFormat::Text,
        },
        MainAgentCommand::Status(args) => args.format,
        MainAgentCommand::Checkpoint(args) => args.format,
        MainAgentCommand::Bootstrap(args) => args.format,
        MainAgentCommand::Worker(args) => match &args.command {
            WorkerCommand::Start(args) => args.format,
            WorkerCommand::List(args) => args.format,
            WorkerCommand::Show(args) => args.format,
            WorkerCommand::Diagnose(args) | WorkerCommand::Supervise(args) => args.format,
            WorkerCommand::Wait(args) => args.format,
            WorkerCommand::Message(args) => args.format,
            WorkerCommand::GuidanceReconcile(args) => args.format,
            WorkerCommand::GuidanceQuarantine(args) => args.format,
            WorkerCommand::AccountHandoff(args) => args.format,
            WorkerCommand::AccountHandoffCancel(args) => args.format,
            WorkerCommand::RequestChanges(args) => args.format,
            WorkerCommand::Accept(args)
            | WorkerCommand::Release(args)
            | WorkerCommand::Delete(args)
            | WorkerCommand::Retire(args) => args.format,
            WorkerCommand::SubmitRecovery(args) => args.format,
            WorkerCommand::ReconcileRecovery(args) => args.format,
            WorkerCommand::Cancel(args) => args.format,
            WorkerCommand::Reassign(args) => args.format,
        },
        MainAgentCommand::Collaborate(args) => args.format,
        MainAgentCommand::Borrow(args) => args.format,
        MainAgentCommand::Handoff(args) => args.format,
        MainAgentCommand::Adopt(args) => args.format,
        MainAgentCommand::Close(args) => args.format,
        MainAgentCommand::Quick(args) => args.format,
        MainAgentCommand::PacketSchema(args) => args.format,
        MainAgentCommand::Completion(_) => OutputFormat::Text,
    }
}

fn detect_output_format(args: &[OsString]) -> OutputFormat {
    if args
        .windows(2)
        .any(|pair| pair[0] == "--format" && pair[1].to_str().is_some_and(|value| value == "json"))
        || args.iter().any(|argument| {
            argument
                .to_str()
                .is_some_and(|value| value == "--format=json")
        })
    {
        OutputFormat::Json
    } else {
        OutputFormat::Text
    }
}

fn render_success(command: &'static str, format: OutputFormat, value: &Value) -> i32 {
    match format {
        OutputFormat::Json => print_json(&Envelope::success(
            schema_version_for(BINARY, command, 1),
            value,
        )),
        OutputFormat::Text => {
            println!(
                "{}",
                serde_json::to_string_pretty(value).unwrap_or_else(|_| "{}".to_string())
            );
            exit::SUCCESS
        }
    }
}

fn render_markdown(value: &Value) -> i32 {
    println!("# Main Agent recovery capsule\n");
    println!("```json");
    println!(
        "{}",
        serde_json::to_string_pretty(value).unwrap_or_else(|_| "{}".to_string())
    );
    println!("```");
    exit::SUCCESS
}

fn render_error(command: &'static str, format: OutputFormat, error: CliError) -> i32 {
    let error = error.into_inner();
    match format {
        OutputFormat::Json => {
            let mut envelope_error = EnvelopeError::new(error.code, error.message);
            if let Some(details) = error.details {
                envelope_error = envelope_error.with_details(details);
            }
            if let Some(hint) = error.hint {
                envelope_error = envelope_error.with_hint(hint);
            }
            let envelope: Envelope<()> =
                Envelope::failure(schema_version_for(BINARY, command, 1), envelope_error);
            print_json(&envelope);
        }
        OutputFormat::Text => {
            let _ = writeln!(io::stderr(), "error: {}", error.message);
            if let Some(hint) = error.hint.as_deref() {
                let _ = writeln!(io::stderr(), "hint: {hint}");
            }
        }
    }
    error.exit_code
}

fn print_json<T: Serialize>(value: &T) -> i32 {
    match serde_json::to_string(value) {
        Ok(serialized) => {
            println!("{serialized}");
            exit::SUCCESS
        }
        Err(error) => {
            eprintln!("error: failed to serialize json: {error}");
            exit::SOFTWARE
        }
    }
}

fn run_completion(shell: crate::completion::CompletionShell) -> i32 {
    let mut command = MainAgentCli::command();
    let bin_name = command.get_name().to_string();
    match shell {
        crate::completion::CompletionShell::Bash => {
            crate::completion::print_completion(Shell::Bash, &mut command, &bin_name)
        }
        crate::completion::CompletionShell::Zsh => {
            crate::completion::print_completion(Shell::Zsh, &mut command, &bin_name)
        }
    }
    exit::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;
    use nils_test_support::GlobalStateLock;
    use std::os::unix::fs::symlink;

    fn busy() -> CliError {
        CliError::unavailable("orchestration-store-busy", "busy", None)
    }

    #[test]
    fn exact_assignment_checkout_accepts_canonical_alias_and_rejects_non_root() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let checkout = tmp.path().join("checkout");
        let nested = checkout.join("src");
        fs::create_dir_all(&nested).expect("checkout");
        let initialized = Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(&checkout)
            .status()
            .expect("git init");
        assert!(initialized.success());
        let alias = tmp.path().join("checkout-alias");
        symlink(&checkout, &alias).expect("checkout alias");

        let checkout_root = exact_assignment_checkout_root(checkout.to_str().unwrap(), "checkout")
            .expect("checkout root");
        let alias_root = exact_assignment_checkout_root(alias.to_str().unwrap(), "checkout alias")
            .expect("canonical alias");
        assert_eq!(alias_root, checkout_root);
        assert!(exact_assignment_checkout_root("checkout", "relative checkout").is_err());
        assert!(
            exact_assignment_checkout_root(nested.to_str().unwrap(), "nested checkout").is_err()
        );
    }

    #[test]
    fn child_idempotency_keys_remain_bounded_and_stage_distinct() {
        let parent = "p".repeat(128);
        let release = child_idempotency_key(&parent, "release");
        let delete = child_idempotency_key(&parent, "delete");
        assert!(release.len() <= 128);
        assert!(delete.len() <= 128);
        assert_ne!(release, delete);
        validate_idempotency_key(&release).expect("derived release key");
        validate_idempotency_key(&delete).expect("derived delete key");
        assert_eq!(release, child_idempotency_key(&parent, "release"));
    }

    #[test]
    fn child_and_batch_lane_keys_preserve_cross_version_authority() {
        assert_eq!(
            compatible_child_idempotency_key("parent-key", "worker"),
            "parent-key-worker"
        );
        assert!(compatible_child_idempotency_key(&"p".repeat(128), "worker").len() <= 128);
        assert_eq!(batch_lane_idempotency_key("batch-key", 3), "batch-key-3");
        let bounded = batch_lane_idempotency_key(&"p".repeat(128), 3);
        assert!(bounded.len() <= 128);
        validate_idempotency_key(&bounded).expect("bounded batch lane key");
    }

    #[test]
    fn pinned_v2_lane_receipts_join_one_child_authority_across_retry_states() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../tests/fixtures/orchestration/released-v2-lane-authority.json"
        ))
        .expect("released-v2 lane authority fixture");
        assert_eq!(
            fixture["source_commit"],
            "89fae89403782b7caec965c614dd1516d903a1e0"
        );
        assert_eq!(
            compatible_child_idempotency_key("quick-v2", "worker"),
            fixture["quick"]["child_key"]
                .as_str()
                .expect("quick child key")
        );
        assert_eq!(
            batch_lane_idempotency_key("batch-v2", 0),
            fixture["batch"]["child_key"]
                .as_str()
                .expect("batch child key")
        );
        assert_ne!(
            fixture["batch"]["manifest"], fixture["batch"]["changed_manifest"],
            "fixture must include changed packet membership"
        );
        assert_ne!(
            crate::coordination::request_digest(
                "main-agent-worker-start-batch-v1",
                &fixture["batch"]["manifest"],
            ),
            crate::coordination::request_digest(
                "main-agent-worker-start-batch-v1",
                &fixture["batch"]["changed_manifest"],
            ),
            "changed pinned manifests must conflict before any lane retry"
        );

        let record = cleanup_test_session("main-v2", "main-v2-incarnation");
        let incarnation = "main-v2-incarnation";
        let child_key = fixture["batch"]["child_key"]
            .as_str()
            .expect("batch child key");
        let current_digest = "current-packet-digest";
        let legacy_digest = "released-v2-packet-digest";

        let retry_lane = |registry: &mut orchestration::Registry,
                          lane_key: &str,
                          terminal: &Value,
                          launches: &mut usize|
         -> Value {
            if let Some(replay) = worker_start_idempotency_replay(
                registry,
                &record,
                incarnation,
                lane_key,
                current_digest,
                legacy_digest,
            )
            .expect("inspect one child authority")
            {
                return replay;
            }
            *launches += 1;
            store_receipt(
                registry,
                &record,
                incarnation,
                lane_key,
                "worker-start",
                current_digest,
                terminal.clone(),
            )
            .expect("commit one child authority");
            terminal.clone()
        };

        for case in ["completed", "ambiguous"] {
            let mut registry = orchestration::Registry::default();
            let historical = fixture["cases"][case].clone();
            store_receipt(
                &mut registry,
                &record,
                incarnation,
                child_key,
                "worker-start",
                legacy_digest,
                historical.clone(),
            )
            .expect("seed released-v2 child receipt");
            let mut retry_launches = 0;
            let replay = retry_lane(
                &mut registry,
                child_key,
                &fixture["cases"]["completed"],
                &mut retry_launches,
            );
            assert_eq!(replay, historical);
            assert_eq!(
                retry_launches, 0,
                "{case} retry must join the released child rather than launch"
            );
            assert_eq!(
                registry.receipts.len(),
                1,
                "{case} retry must retain one child authority"
            );
            let total_worker_launches = 1 + retry_launches;
            assert_eq!(
                total_worker_launches, 1,
                "{case} retry must never launch a second worker"
            );
            assert_eq!(
                retry_lane(
                    &mut registry,
                    child_key,
                    &fixture["cases"]["completed"],
                    &mut retry_launches,
                ),
                historical,
                "{case} replay must remain stable"
            );
            assert_eq!(retry_launches, 0);
        }

        for case in ["incomplete", "transient"] {
            let mut registry = orchestration::Registry::default();
            if case == "incomplete" {
                assert_eq!(fixture["cases"][case], Value::Null);
            } else {
                let transient = busy();
                assert_eq!(
                    transient.code(),
                    fixture["cases"][case]["error_code"]
                        .as_str()
                        .expect("transient error code")
                );
                assert!(batch_lane_error_is_resumable(&transient));
                assert_eq!(fixture["cases"][case]["resumable"], true);
            }
            let mut worker_launches = 0;
            let terminal = retry_lane(
                &mut registry,
                child_key,
                &fixture["cases"]["completed"],
                &mut worker_launches,
            );
            assert_eq!(terminal, fixture["cases"]["completed"]);
            assert_eq!(worker_launches, 1);
            assert_eq!(registry.receipts.len(), 1);
            assert_eq!(
                retry_lane(
                    &mut registry,
                    child_key,
                    &fixture["cases"]["completed"],
                    &mut worker_launches,
                ),
                terminal,
                "{case} terminal replay must be stable"
            );
            assert_eq!(
                worker_launches, 1,
                "{case} terminal replay launches no replacement"
            );
        }

        let quick_key = fixture["quick"]["child_key"]
            .as_str()
            .expect("quick child key");
        let mut quick_registry = orchestration::Registry::default();
        store_receipt(
            &mut quick_registry,
            &record,
            incarnation,
            quick_key,
            "worker-start",
            legacy_digest,
            fixture["quick"]["completed_child"].clone(),
        )
        .expect("seed released-v2 quick child");
        let mut quick_retry_launches = 0;
        assert_eq!(
            retry_lane(
                &mut quick_registry,
                quick_key,
                &fixture["quick"]["completed_child"],
                &mut quick_retry_launches,
            ),
            fixture["quick"]["completed_child"]
        );
        assert_eq!(quick_retry_launches, 0);
        assert_eq!(
            quick_registry.receipts.len(),
            1,
            "quick retry must retain one child authority"
        );

        let mut changed_registry = orchestration::Registry::default();
        let mut changed_launches = 0;
        retry_lane(
            &mut changed_registry,
            child_key,
            &fixture["cases"]["completed"],
            &mut changed_launches,
        );
        let changed_current = "changed-current-packet-digest";
        let changed_legacy = "changed-released-v2-packet-digest";
        let conflict = worker_start_idempotency_replay(
            &changed_registry,
            &record,
            incarnation,
            child_key,
            changed_current,
            changed_legacy,
        )
        .expect_err("changed packet membership must conflict with prior child authority");
        assert_eq!(conflict.code(), "idempotency-conflict");
        assert_eq!(changed_launches, 1);
    }

    #[test]
    fn batch_lane_transient_and_ambiguous_errors_remain_resumable() {
        assert!(batch_lane_error_is_resumable(&busy()));
        assert!(batch_lane_error_is_resumable(&CliError::runtime(
            "command-timeout",
            "ambiguous child outcome",
            None,
        )));
        assert!(!batch_lane_error_is_resumable(&CliError::data(
            "dependency-not-satisfied",
            "deterministic lane failure",
            None,
        )));
    }

    #[test]
    fn batch_lane_lease_includes_epoch_rounding_margin() {
        assert_eq!(
            worker_start_batch_lane_lease_until(100),
            100 + WORKER_START_BATCH_LANE_LEASE_SECS + 1,
            "a fresh second-granularity lease must retain the configured duration"
        );
    }

    #[test]
    fn batch_lane_post_effect_fence_accepts_only_the_unchanged_owner() {
        let claim = json!({
            "schema_version": "main-agent.worker-start-batch-lane-claim.v1",
            "state": "in_progress",
            "owner_id": "owner-one",
            "lease_until_epoch": 100
        });
        assert!(!worker_start_batch_lane_fence_is_valid(
            &claim,
            "owner-one",
            100,
            BatchLaneFencePoint::BeforeChildSideEffect,
        ));
        assert!(worker_start_batch_lane_fence_is_valid(
            &claim,
            "owner-one",
            100,
            BatchLaneFencePoint::AfterChildSideEffect,
        ));
        assert!(!worker_start_batch_lane_fence_is_valid(
            &claim,
            "owner-two",
            99,
            BatchLaneFencePoint::AfterChildSideEffect,
        ));
        assert!(worker_start_batch_lane_fence_is_valid(
            &claim,
            "owner-one",
            99,
            BatchLaneFencePoint::BeforeChildSideEffect,
        ));
    }

    #[test]
    fn readiness_finalizer_lease_covers_an_in_flight_recovery_send() {
        for stage in ["reserved", "sent", "failed", "outcome_unknown"] {
            assert_eq!(
                worker_start_finalizer_lease_secs(
                    &json!({ "recovery_continuation": { "stage": stage } })
                ),
                WORKER_START_FINALIZER_LEASE_SECS,
                "{stage} remains promptly recoverable after owner loss"
            );
        }
        assert!(
            worker_start_finalizer_lease_secs(
                &json!({ "recovery_continuation": { "stage": "sending" } })
            ) > i64::try_from(crate::PANE_INPUT_COMMAND_TIMEOUT.as_secs()).expect("pane timeout"),
            "the finalizer lease must outlive a possibly ambiguous send"
        );
    }

    #[test]
    fn retries_transient_then_succeeds() {
        let mut calls = 0u32;
        let mut slept: Vec<Duration> = Vec::new();
        let result = retry_transient_store_inner(
            3,
            Duration::from_millis(10),
            || {
                calls += 1;
                if calls < 3 {
                    Err(busy())
                } else {
                    Ok(json!({ "ok": true }))
                }
            },
            |delay| slept.push(delay),
        );
        assert!(result.is_ok(), "expected success after transient retries");
        assert_eq!(calls, 3, "should attempt three times");
        assert_eq!(
            slept,
            vec![Duration::from_millis(10), Duration::from_millis(20)],
            "linear backoff between the two retries",
        );
    }

    #[test]
    fn does_not_retry_non_retryable() {
        let mut calls = 0u32;
        let mut slept = 0u32;
        let result = retry_transient_store_inner(
            3,
            Duration::from_millis(10),
            || {
                calls += 1;
                Err(CliError::data("run-objective-conflict", "conflict", None))
            },
            |_| slept += 1,
        );
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code(), "run-objective-conflict");
        assert_eq!(calls, 1, "non-retryable errors must not retry");
        assert_eq!(slept, 0);
    }

    #[test]
    fn exhausts_and_returns_last_error() {
        let mut calls = 0u32;
        let result = retry_transient_store_inner(
            3,
            Duration::from_millis(1),
            || {
                calls += 1;
                Err(busy())
            },
            |_| {},
        );
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code(), "orchestration-store-busy");
        assert_eq!(calls, 3, "should stop after max attempts");
    }

    #[test]
    fn is_store_retryable_matches_only_transient_codes() {
        assert!(is_store_retryable(&busy()));
        assert!(is_store_retryable(&CliError::unavailable(
            "orchestration-store-unavailable",
            "io",
            None,
        )));
        assert!(!is_store_retryable(&CliError::data(
            "assignment-exists",
            "exists",
            None,
        )));
    }

    #[test]
    fn facade_retries_never_restart_command_owned_wall_clock_deadlines() {
        for args in [
            vec![
                "main-agent",
                "self",
                "recover",
                "--idempotency-key",
                "self-recover-deadline-0001",
            ],
            vec![
                "main-agent",
                "worker",
                "wait",
                "assignment",
                "--until",
                "submitted",
                "--timeout",
                "1s",
            ],
            vec![
                "main-agent",
                "worker",
                "start",
                "--assignment-file",
                "packet.json",
                "--await-ready",
                "1s",
                "--idempotency-key",
                "start-deadline-0001",
            ],
            vec![
                "main-agent",
                "worker",
                "submit-recovery",
                "assignment",
                "--if-revision",
                "1",
                "--timeout",
                "1s",
                "--idempotency-key",
                "recovery-deadline-0001",
            ],
            vec![
                "main-agent",
                "worker",
                "reassign",
                "assignment",
                "--assignment-file",
                "packet.json",
                "--if-revision",
                "1",
                "--reason",
                "test",
                "--await-ready",
                "1s",
                "--idempotency-key",
                "reassign-deadline-0001",
            ],
            vec![
                "main-agent",
                "worker",
                "account-handoff",
                "assignment",
                "--account",
                "managed",
                "--if-revision",
                "1",
                "--authorize-account-change",
                "--timeout",
                "1s",
                "--idempotency-key",
                "handoff-deadline-0001",
            ],
            vec![
                "main-agent",
                "quick",
                "--assignment-file",
                "packet.json",
                "--await-ready",
                "1s",
            ],
        ] {
            let cli = MainAgentCli::try_parse_from(args.clone()).expect("deadline command parses");
            assert!(
                command_owns_internal_deadline(&cli.command),
                "whole-command retry must not reset {args:?}"
            );
        }
        let list = MainAgentCli::try_parse_from(["main-agent", "worker", "list"])
            .expect("list command parses");
        assert!(
            !command_owns_internal_deadline(&list.command),
            "bounded transient retry remains available to immediate reads"
        );
    }

    #[test]
    fn wait_until_submitted_and_blocked_match_only_their_state() {
        assert!(WaitUntil::Submitted.matches("submitted"));
        assert!(!WaitUntil::Submitted.matches("working"));
        assert!(!WaitUntil::Submitted.matches("accepted"));
        assert!(WaitUntil::Blocked.matches("blocked"));
        assert!(!WaitUntil::Blocked.matches("submitted"));
    }

    #[test]
    fn wait_until_terminal_matches_exactly_the_terminal_states() {
        for terminal in ["accepted", "released", "cancelled"] {
            assert!(
                WaitUntil::Terminal.matches(terminal),
                "{terminal} is terminal"
            );
        }
        for live in ["assigned", "starting", "working", "blocked", "submitted"] {
            assert!(
                !WaitUntil::Terminal.matches(live),
                "{live} must not count as terminal"
            );
        }
    }

    #[test]
    fn wait_until_label_round_trips() {
        assert_eq!(WaitUntil::Submitted.as_label(), "submitted");
        assert_eq!(WaitUntil::Blocked.as_label(), "blocked");
        assert_eq!(WaitUntil::Terminal.as_label(), "terminal");
    }

    #[test]
    fn parse_wait_timeout_accepts_bounds_and_suffixes() {
        assert_eq!(parse_wait_timeout("1").unwrap(), Duration::from_secs(1));
        assert_eq!(parse_wait_timeout("30s").unwrap(), Duration::from_secs(30));
        assert_eq!(parse_wait_timeout("1m").unwrap(), Duration::from_secs(60));
        assert_eq!(parse_wait_timeout("60").unwrap(), Duration::from_secs(60));
    }

    #[test]
    fn parse_wait_timeout_rejects_zero_over_max_and_garbage() {
        assert_eq!(
            parse_wait_timeout("0").unwrap_err().code(),
            "worker-wait-timeout"
        );
        assert_eq!(
            parse_wait_timeout("61").unwrap_err().code(),
            "worker-wait-timeout"
        );
        // 2m == 120s, over the 60s bound.
        assert_eq!(
            parse_wait_timeout("2m").unwrap_err().code(),
            "worker-wait-timeout"
        );
        assert_eq!(
            parse_wait_timeout("abc").unwrap_err().code(),
            "invalid-duration"
        );
    }

    #[test]
    fn dependency_state_satisfies_only_after_acceptance() {
        for done in ["accepted", "released"] {
            assert!(
                dependency_state_satisfies(done),
                "{done} counts a dependency as satisfied"
            );
        }
        for pending in [
            "assigned",
            "starting",
            "working",
            "blocked",
            "submitted",
            "cancelled",
        ] {
            assert!(
                !dependency_state_satisfies(pending),
                "{pending} must not satisfy a dependency"
            );
        }
    }

    fn dep_assignment(id: &str, run_id: &str, state: &str) -> AssignmentRecord {
        AssignmentRecord {
            schema_version: ASSIGNMENT_SCHEMA.to_string(),
            assignment_id: id.to_string(),
            run_id: run_id.to_string(),
            revision: 1,
            state: state.to_string(),
            task_summary: "dependency".to_string(),
            private_packet_digest: format!("sha256:{}", "0".repeat(64)),
            primary_manager: SessionRef {
                machine: None,
                session_id: "main".to_string(),
                session_incarnation: "inc".to_string(),
                session_created_at: "2030-01-01T00:00:00Z".to_string(),
            },
            worker: None,
            previous_worker: None,
            collaborators: Vec::new(),
            borrowed_by: Vec::new(),
            repository: None,
            worktree: None,
            base_ref: None,
            scopes: Vec::new(),
            durable_refs: Vec::new(),
            depends_on: Vec::new(),
            checkpoint: None,
            result_summary: None,
            blocker_summary: None,
            submit_recovery: None,
            worker_quarantine: None,
            account_handoff: None,
            created_at: "2030-01-01T00:00:00Z".to_string(),
            updated_at: "2030-01-01T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn submit_recovery_confirms_a_newer_worker_checkpoint_after_manager_mutations() {
        let main = cleanup_test_session("main", "inc");
        let worker = SessionRef {
            machine: None,
            session_id: "worker".to_string(),
            session_incarnation: "worker-inc".to_string(),
            session_created_at: "2030-01-01T00:00:00Z".to_string(),
        };
        let controller = SessionRef {
            machine: None,
            session_id: "main".to_string(),
            session_incarnation: "inc".to_string(),
            session_created_at: "2030-01-01T00:00:00Z".to_string(),
        };
        let reservation = SubmitRecoveryReservation {
            assignment_id: "assignment".to_string(),
            attempt_id: "attempt-one".to_string(),
            reserved_revision: 2,
            run_id: "run-one".to_string(),
            controller: controller.clone(),
            worker: worker.clone(),
        };

        for state in ["working", "accepted", "released"] {
            let mut assignment = dep_assignment("assignment", "run-one", state);
            assignment.revision = 4;
            assignment.worker = Some(worker.clone());
            assignment.checkpoint = Some(RunCheckpoint {
                revision: 3,
                summary: "worker submitted an authenticated checkpoint".to_string(),
                next_action: "manager mutation".to_string(),
                updated_at: "2030-01-01T00:00:03Z".to_string(),
            });
            assignment.submit_recovery = Some(SubmitRecoveryRecord {
                schema_version: SUBMIT_RECOVERY_SCHEMA.to_string(),
                attempt_id: "attempt-one".to_string(),
                origin: "explicit".to_string(),
                run_id: Some("run-one".to_string()),
                controller: Some(controller.clone()),
                session_incarnation: worker.session_incarnation.clone(),
                reserved_revision: 2,
                state: "sent".to_string(),
                attempt_count: 1,
                result: "single guarded Enter sent".to_string(),
                attempted_at: "2030-01-01T00:00:02Z".to_string(),
                updated_at: "2030-01-01T00:00:02Z".to_string(),
            });

            assert!(
                matches!(
                    submit_recovery_checkpoint(&assignment, &main, "inc", &reservation),
                    SubmitRecoveryCheckpoint::Confirmed
                ),
                "{state} must preserve the newer authenticated worker checkpoint"
            );
        }

        let mut preclaim_failure = dep_assignment("assignment", "run-one", "blocked");
        preclaim_failure.revision = 3;
        preclaim_failure.worker = Some(worker.clone());
        preclaim_failure.checkpoint = Some(RunCheckpoint {
            revision: 3,
            summary: "worker bootstrap failed before claim acquisition".to_string(),
            next_action: "diagnose the failed pre-claim assignment".to_string(),
            updated_at: "2030-01-01T00:00:03Z".to_string(),
        });
        preclaim_failure.blocker_summary = Some(
            "[pre-claim:worker-bootstrap-checkout-mismatch] worker bootstrap failed".to_string(),
        );
        preclaim_failure.submit_recovery = Some(SubmitRecoveryRecord {
            schema_version: SUBMIT_RECOVERY_SCHEMA.to_string(),
            attempt_id: "attempt-one".to_string(),
            origin: "explicit".to_string(),
            run_id: Some("run-one".to_string()),
            controller: Some(controller),
            session_incarnation: worker.session_incarnation.clone(),
            reserved_revision: 2,
            state: "sent".to_string(),
            attempt_count: 1,
            result: "single guarded Enter sent".to_string(),
            attempted_at: "2030-01-01T00:00:02Z".to_string(),
            updated_at: "2030-01-01T00:00:02Z".to_string(),
        });
        assert!(matches!(
            submit_recovery_checkpoint(&preclaim_failure, &main, "inc", &reservation),
            SubmitRecoveryCheckpoint::Rejected("worker-bootstrap-preclaim-failed")
        ));
        assert!(
            !worker_readiness_checkpoint(
                &preclaim_failure,
                &main,
                "inc",
                &worker,
                reservation.reserved_revision,
            ),
            "a typed pre-claim blocker is not an authenticated ready checkpoint"
        );
    }

    #[test]
    fn unsatisfied_dependencies_clears_on_accepted_and_flags_the_rest() {
        let mut registry = orchestration::Registry::default();
        registry.assignments.insert(
            "done".to_string(),
            dep_assignment("done", "run-one", "accepted"),
        );
        registry.assignments.insert(
            "cleaned".to_string(),
            dep_assignment("cleaned", "run-one", "released"),
        );
        registry.assignments.insert(
            "busy".to_string(),
            dep_assignment("busy", "run-one", "working"),
        );
        registry.assignments.insert(
            "elsewhere".to_string(),
            dep_assignment("elsewhere", "other-run", "accepted"),
        );

        // No dependencies is always cleared.
        assert!(unsatisfied_dependencies(&registry, "run-one", &[]).is_empty());
        // Accepted and released dependencies clear the launch.
        assert!(
            unsatisfied_dependencies(
                &registry,
                "run-one",
                &["done".to_string(), "cleaned".to_string()]
            )
            .is_empty()
        );
        // A pre-terminal dependency blocks and reports its observed state.
        let busy = unsatisfied_dependencies(&registry, "run-one", &["busy".to_string()]);
        assert_eq!(busy.len(), 1);
        assert_eq!(busy[0]["assignment_id"], "busy");
        assert_eq!(busy[0]["state"], "working");
        // A missing dependency blocks with a null state.
        let missing = unsatisfied_dependencies(&registry, "run-one", &["ghost".to_string()]);
        assert_eq!(missing[0]["assignment_id"], "ghost");
        assert!(missing[0]["state"].is_null());
        // A same-id dependency in a different run does not count as satisfied.
        let cross = unsatisfied_dependencies(&registry, "run-one", &["elsewhere".to_string()]);
        assert_eq!(cross.len(), 1);
        assert!(cross[0]["state"].is_null());
    }

    fn run_record(id: &str, ephemeral: bool) -> RunRecord {
        RunRecord {
            schema_version: orchestration::RUN_SCHEMA.to_string(),
            run_id: id.to_string(),
            revision: 1,
            state: "active".to_string(),
            tier: "L0".to_string(),
            objective_summary: "summary".to_string(),
            objective_packet_digest: format!("sha256:{}", "a".repeat(64)),
            controller: SessionRef {
                machine: None,
                session_id: "main".to_string(),
                session_incarnation: "inc".to_string(),
                session_created_at: "2030-01-01T00:00:00Z".to_string(),
            },
            durable_refs: Vec::new(),
            ephemeral,
            checkpoint: None,
            created_at: "2030-01-01T00:00:00Z".to_string(),
            updated_at: "2030-01-01T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn group_cleanup_plan_is_exactly_scoped_and_requires_force_for_live_work() {
        let mut registry = orchestration::Registry::default();
        let run = run_record("run-one", false);
        let controller = run.controller.clone();
        registry.runs.insert(run.run_id.clone(), run.clone());

        let mut accepted = dep_assignment("accepted", "run-one", "accepted");
        accepted.worker = Some(SessionRef {
            machine: None,
            session_id: "worker-accepted".to_string(),
            session_incarnation: "worker-inc-accepted".to_string(),
            session_created_at: "2030-01-01T00:01:00Z".to_string(),
        });
        registry
            .assignments
            .insert(accepted.assignment_id.clone(), accepted);

        let mut submitted = dep_assignment("submitted", "run-one", "submitted");
        submitted.worker = Some(SessionRef {
            machine: None,
            session_id: "worker-submitted".to_string(),
            session_incarnation: "worker-inc-submitted".to_string(),
            session_created_at: "2030-01-01T00:02:00Z".to_string(),
        });
        registry
            .assignments
            .insert(submitted.assignment_id.clone(), submitted);

        let mut collaborator_owned = dep_assignment("borrowed", "run-one", "working");
        collaborator_owned.primary_manager = SessionRef {
            machine: None,
            session_id: "other-main".to_string(),
            session_incarnation: "other-inc".to_string(),
            session_created_at: "2030-01-01T00:00:00Z".to_string(),
        };
        registry
            .assignments
            .insert(collaborator_owned.assignment_id.clone(), collaborator_owned);

        let plan = build_group_cleanup_plan(&registry, &run, &controller).unwrap();

        assert!(plan.requires_force);
        assert_eq!(plan.run_id, "run-one");
        assert_eq!(plan.run_revision, 1);
        assert_eq!(
            plan.workers
                .iter()
                .map(|worker| worker.assignment_id.as_str())
                .collect::<Vec<_>>(),
            vec!["accepted", "submitted"],
        );
        assert!(plan.workers.iter().all(|worker| worker.primary_managed),);
        assert!(plan.plan_digest.starts_with("sha256:"));
    }

    #[test]
    fn group_cleanup_force_terminalizes_primary_assignments_before_deletion() {
        let mut registry = orchestration::Registry::default();
        let run = run_record("run-one", false);
        let controller = run.controller.clone();
        registry.runs.insert(run.run_id.clone(), run.clone());
        for state in ["working", "submitted", "accepted", "released", "cancelled"] {
            let id = format!("assignment-{state}");
            registry
                .assignments
                .insert(id.clone(), dep_assignment(&id, "run-one", state));
        }

        let safe_error = prepare_group_cleanup_assignments(
            &mut registry,
            &run,
            &controller,
            GroupCleanupMode::Safe,
        )
        .unwrap_err();
        assert_eq!(safe_error.code(), "group-cleanup-force-required");
        assert_eq!(registry.assignments["assignment-working"].state, "working");

        registry
            .assignments
            .get_mut("assignment-working")
            .expect("working assignment")
            .submit_recovery = Some(SubmitRecoveryRecord {
            schema_version: SUBMIT_RECOVERY_SCHEMA.to_string(),
            attempt_id: "cleanup-recovery".to_string(),
            origin: "automatic".to_string(),
            run_id: Some("run-one".to_string()),
            controller: Some(controller.clone()),
            session_incarnation: "worker-inc".to_string(),
            reserved_revision: 1,
            state: "attempting".to_string(),
            attempt_count: 1,
            result: "recovery reserved".to_string(),
            attempted_at: "2030-01-01T00:00:01Z".to_string(),
            updated_at: "2030-01-01T00:00:01Z".to_string(),
        });
        let recovery_error = prepare_group_cleanup_assignments(
            &mut registry,
            &run,
            &controller,
            GroupCleanupMode::Force,
        )
        .unwrap_err();
        assert_eq!(recovery_error.code(), "submit-recovery-in-flight");
        assert_eq!(registry.assignments["assignment-working"].state, "working");
        assert_eq!(
            registry.assignments["assignment-submitted"].state,
            "submitted"
        );
        assert_eq!(
            registry.assignments["assignment-accepted"].state,
            "accepted"
        );
        registry
            .assignments
            .get_mut("assignment-working")
            .expect("working assignment")
            .submit_recovery
            .as_mut()
            .expect("submit recovery")
            .state = "failed".to_string();

        prepare_group_cleanup_assignments(
            &mut registry,
            &run,
            &controller,
            GroupCleanupMode::Force,
        )
        .unwrap();
        assert_eq!(
            registry.assignments["assignment-working"].state,
            "cancelled"
        );
        assert_eq!(
            registry.assignments["assignment-submitted"].state,
            "cancelled"
        );
        assert_eq!(
            registry.assignments["assignment-accepted"].state,
            "released"
        );
        assert_eq!(
            registry.assignments["assignment-released"].state,
            "released"
        );
        assert_eq!(
            registry.assignments["assignment-cancelled"].state,
            "cancelled"
        );
    }

    #[test]
    fn group_cleanup_assignment_revision_overflow_fails_before_mutation() {
        let mut registry = orchestration::Registry::default();
        let run = run_record("run-one", false);
        let controller = run.controller.clone();
        registry.runs.insert(run.run_id.clone(), run.clone());
        let mut assignment = dep_assignment("assignment-max", "run-one", "accepted");
        assignment.revision = u64::MAX;
        registry
            .assignments
            .insert(assignment.assignment_id.clone(), assignment);
        let before = serde_json::to_vec(&registry).unwrap();

        let error = prepare_group_cleanup_assignments(
            &mut registry,
            &run,
            &controller,
            GroupCleanupMode::Safe,
        )
        .expect_err("an exhausted assignment revision must fail closed");
        assert_eq!(error.code(), "orchestration-revision-capacity");
        assert_eq!(
            serde_json::to_vec(&registry).unwrap(),
            before,
            "revision overflow must not partially terminalize assignments"
        );
    }

    #[test]
    fn group_cleanup_plan_enforces_the_bounded_checkpoint_batch() {
        let run = run_record("run-one", false);
        let controller = run.controller.clone();
        let registry_with = |count: usize| {
            let mut registry = orchestration::Registry::default();
            registry.runs.insert(run.run_id.clone(), run.clone());
            for index in 0..count {
                let id = format!("assignment-{index:03}");
                registry
                    .assignments
                    .insert(id.clone(), dep_assignment(&id, "run-one", "accepted"));
            }
            registry
        };

        let bounded = registry_with(GROUP_CLEANUP_MAX_ASSIGNMENTS);
        assert_eq!(
            build_group_cleanup_plan(&bounded, &run, &controller)
                .expect("maximum bounded cleanup plan")
                .workers
                .len(),
            GROUP_CLEANUP_MAX_ASSIGNMENTS
        );
        let oversized = registry_with(GROUP_CLEANUP_MAX_ASSIGNMENTS + 1);
        let error = build_group_cleanup_plan(&oversized, &run, &controller)
            .expect_err("oversized cleanup plan must fail before acquiring worker locks");
        assert_eq!(error.code(), "group-cleanup-batch-too-large");
    }

    fn cleanup_test_session(id: &str, incarnation: &str) -> SessionRecord {
        SessionRecord {
            schema_version: crate::SESSION_DOCUMENT_VERSION.to_string(),
            id: id.to_string(),
            agent: "codex".to_string(),
            mode: "interactive".to_string(),
            coordination_mode: CoordinationMode::Advisory,
            title: None,
            title_state: None,
            title_revision: 0,
            cwd: "/tmp".to_string(),
            tmux_session: format!("agent-{id}"),
            prompt_file: None,
            log_file: None,
            created_at: "2030-01-01T00:00:00Z".to_string(),
            updated_at: "2030-01-01T00:00:00Z".to_string(),
            provider_resume: None,
            runtime: Some(crate::RuntimeInfo {
                kind: "tmux".to_string(),
                tmux_session: format!("agent-{id}"),
                generation: 1,
                started_at: "2030-01-01T00:00:00Z".to_string(),
                launch_id: incarnation.to_string(),
                extra: std::collections::BTreeMap::new(),
            }),
            agent_args: Vec::new(),
            agent_bin: None,
            extra: std::collections::BTreeMap::new(),
            resume_sidecar_extra: std::collections::BTreeMap::new(),
        }
    }

    fn group_cleanup_progress_race_fixture(
        context: &CliContext,
        idempotency_key: &str,
    ) -> (PathBuf, PathBuf, Vec<u8>) {
        fs::create_dir(&context.state_dir).unwrap();
        let progress_bytes = |principal_session_id: &str, principal_incarnation: &str| {
            serde_json::to_vec(&GroupCleanupProgressReceipt {
                schema_version: orchestration::GROUP_CLEANUP_PROGRESS_RECEIPT_V1_SCHEMA.to_string(),
                requested_session_id: None,
                principal_session_id: principal_session_id.to_string(),
                principal_incarnation: principal_incarnation.to_string(),
                idempotency_key: idempotency_key.to_string(),
                request_digest: "6".repeat(64),
                outcome: json!({
                    "schema_version": GROUP_CLEANUP_RESULT_SCHEMA,
                    "completed": false,
                    "workers": []
                }),
            })
            .unwrap()
        };
        let source_key = format!("{:064x}", 0);
        orchestration::store_group_cleanup_progress(
            context,
            &source_key,
            &progress_bytes("abandoned-main", "abandoned-incarnation"),
        )
        .unwrap();
        let progress_dir = context
            .state_dir
            .join("orchestration/group-cleanup-progress");
        let source_path = progress_dir.join(&source_key);
        fs::File::options()
            .write(true)
            .open(&source_path)
            .unwrap()
            .set_times(
                fs::FileTimes::new().set_modified(std::time::UNIX_EPOCH + Duration::from_secs(1)),
            )
            .unwrap();
        for index in 1..128 {
            let session_id = format!("live-race-main-{index}");
            let incarnation = format!("live-race-incarnation-{index}");
            crate::write_session_record(context, &cleanup_test_session(&session_id, &incarnation))
                .unwrap();
            let path = progress_dir.join(format!("{index:064x}"));
            fs::write(&path, progress_bytes(&session_id, &incarnation)).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        (
            progress_dir,
            source_path,
            progress_bytes("incoming-main", "incoming-incarnation"),
        )
    }

    fn group_cleanup_progress_byte_pressure_fixture(
        context: &CliContext,
        idempotency_key: &str,
    ) -> (PathBuf, String, Vec<u8>) {
        fs::create_dir(&context.state_dir).unwrap();
        let progress_bytes = |principal_session_id: &str, padding: usize| {
            serde_json::to_vec(&GroupCleanupProgressReceipt {
                schema_version: orchestration::GROUP_CLEANUP_PROGRESS_RECEIPT_V1_SCHEMA.to_string(),
                requested_session_id: None,
                principal_session_id: principal_session_id.to_string(),
                principal_incarnation: "byte-pressure-incarnation".to_string(),
                idempotency_key: idempotency_key.to_string(),
                request_digest: "d".repeat(64),
                outcome: json!({
                    "schema_version": GROUP_CLEANUP_RESULT_SCHEMA,
                    "completed": false,
                    "padding": "x".repeat(padding)
                }),
            })
            .unwrap()
        };
        let current_key = "f".repeat(64);
        orchestration::store_group_cleanup_progress(
            context,
            &current_key,
            &progress_bytes("current-main", 0),
        )
        .unwrap();
        let progress_dir = context
            .state_dir
            .join("orchestration/group-cleanup-progress");
        let mut source_path = None;
        for index in 0..4 {
            let path = progress_dir.join(format!("{index:064x}"));
            fs::write(
                &path,
                progress_bytes(&format!("abandoned-byte-main-{index}"), 3_200_000),
            )
            .unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
            if index == 0 {
                fs::File::options()
                    .write(true)
                    .open(&path)
                    .unwrap()
                    .set_times(
                        fs::FileTimes::new()
                            .set_modified(std::time::UNIX_EPOCH + Duration::from_secs(1)),
                    )
                    .unwrap();
                source_path = Some(path);
            }
        }
        (
            source_path.unwrap(),
            current_key,
            progress_bytes("current-main", 4_000_000),
        )
    }

    #[test]
    fn group_cleanup_worker_failure_preserves_the_main_session() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().to_path_buf(),
            host: None,
        };
        let main = cleanup_test_session("main", "inc");
        fs::create_dir_all(session_dir(&context, &main.id)).unwrap();
        crate::write_session_record(&context, &main).unwrap();
        let mut worker_a = cleanup_test_session("worker-a", "worker-a-inc");
        worker_a.created_at = "2030-01-01T00:00:30Z".to_string();
        crate::mark_tmux_runtime_never_launched(&mut worker_a);
        fs::create_dir_all(session_dir(&context, &worker_a.id)).unwrap();
        crate::write_session_record(&context, &worker_a).unwrap();
        fs::create_dir_all(session_dir(&context, "worker-broken")).unwrap();

        let mut run = run_record("run-one", false);
        run.controller = session_ref(&context, &main, "inc");
        let mut assignment_a = dep_assignment("assignment-a", "run-one", "submitted");
        assignment_a.primary_manager = run.controller.clone();
        assignment_a.worker = Some(session_ref(&context, &worker_a, "worker-a-inc"));
        let mut assignment = dep_assignment("assignment-broken", "run-one", "submitted");
        assignment.primary_manager = run.controller.clone();
        assignment.worker = Some(SessionRef {
            machine: None,
            session_id: "worker-broken".to_string(),
            session_incarnation: "worker-inc".to_string(),
            session_created_at: "2030-01-01T00:01:00Z".to_string(),
        });
        {
            let mut locked = orchestration::lock_registry(&context).unwrap();
            locked.registry.runs.insert(run.run_id.clone(), run);
            locked
                .registry
                .assignments
                .insert(assignment_a.assignment_id.clone(), assignment_a);
            locked
                .registry
                .assignments
                .insert(assignment.assignment_id.clone(), assignment);
            locked.save().unwrap();
        }

        let preview = preview_group_cleanup(&context, "main").unwrap();
        let request = GroupCleanupRequest {
            schema_version: GROUP_CLEANUP_REQUEST_SCHEMA.to_string(),
            expected_main_incarnation: "inc".to_string(),
            expected_run_revision: preview["run_revision"].as_u64().unwrap(),
            expected_plan_digest: preview["plan_digest"].as_str().unwrap().to_string(),
            mode: GroupCleanupMode::Force,
            idempotency_key: "cleanup-001".to_string(),
        };
        let repaired_tmux = tmp.path().join("tmux-missing-session");
        fs::write(
            &repaired_tmux,
            "#!/bin/sh\nprintf \"%s\\n\" \"can't find session: test\" >&2\nexit 1\n",
        )
        .unwrap();
        fs::set_permissions(&repaired_tmux, fs::Permissions::from_mode(0o700)).unwrap();
        let execution =
            execute_group_cleanup(&context, "main", request.clone(), repaired_tmux.clone())
                .unwrap();

        assert_eq!(execution.value["completed"], false);
        assert_eq!(execution.value["main_deleted"], false);
        assert_eq!(execution.value["failure"]["stage"], "worker_cleanup");
        assert_eq!(
            execution.value["workers"][0]["assignment_id"],
            "assignment-a"
        );
        assert_eq!(execution.value["workers"][0]["outcome"], "deleted");
        assert_eq!(
            execution.value["workers"][1]["assignment_id"],
            "assignment-broken"
        );
        let first_fences = execution.deleted_registry_fences.clone();
        assert_eq!(first_fences.len(), 1);
        assert!(!session_dir(&context, "worker-a").exists());
        assert!(
            session_dir(&context, "main").join("session.json").exists(),
            "worker cleanup failure must preserve the Main Agent record"
        );

        let mut repaired_worker = cleanup_test_session("worker-broken", "worker-inc");
        repaired_worker.created_at = "2030-01-01T00:01:00Z".to_string();
        crate::mark_tmux_runtime_never_launched(&mut repaired_worker);
        crate::write_session_record(&context, &repaired_worker).unwrap();
        let mut repaired_main = load_session_record(&context, "main").unwrap();
        crate::mark_tmux_runtime_never_launched(&mut repaired_main);
        crate::write_session_record(&context, &repaired_main).unwrap();
        let resumed =
            execute_group_cleanup(&context, "main", request.clone(), repaired_tmux.clone())
                .unwrap();
        assert_eq!(
            resumed.value["completed"], true,
            "identical retry must resume from durable progress: {}",
            resumed.value
        );
        assert_eq!(resumed.value["run_closed"], true);
        assert_eq!(resumed.value["main_deleted"], true);
        assert_eq!(resumed.value["workers"][0]["outcome"], "deleted");
        assert_eq!(resumed.value["workers"][0]["session_id"], "worker-a");
        assert_eq!(resumed.value["workers"][1]["outcome"], "deleted");
        assert_eq!(
            &resumed.deleted_registry_fences[..first_fences.len()],
            first_fences.as_slice(),
            "retry must carry forward the exact first-worker registry fence"
        );
        assert_eq!(
            resumed.value["workers"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|worker| worker["session_id"] == "worker-a")
                .count(),
            1,
            "retry must skip the already deleted first worker"
        );
        assert!(!session_dir(&context, "worker-broken").exists());
        assert!(!session_dir(&context, "main").exists());
        assert_eq!(
            orchestration::load_registry_readonly(&context)
                .unwrap()
                .runs["run-one"]
                .state,
            "closed"
        );

        let replayed = execute_group_cleanup(&context, "main", request, repaired_tmux).unwrap();
        assert_eq!(replayed.value, resumed.value);
        assert_eq!(
            replayed.deleted_registry_fences, resumed.deleted_registry_fences,
            "successful replay must retain every daemon registry fence"
        );
    }

    #[test]
    fn group_cleanup_deletes_workers_closes_the_run_and_deletes_main_last() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().to_path_buf(),
            host: None,
        };
        let tmux = tmp.path().join("tmux-missing-session");
        fs::write(
            &tmux,
            "#!/bin/sh\nprintf \"%s\\n\" \"can't find session: test\" >&2\nexit 1\n",
        )
        .unwrap();
        fs::set_permissions(&tmux, fs::Permissions::from_mode(0o700)).unwrap();
        let mut main = cleanup_test_session("main", "inc");
        crate::mark_tmux_runtime_never_launched(&mut main);
        fs::create_dir_all(session_dir(&context, &main.id)).unwrap();
        crate::write_session_record(&context, &main).unwrap();
        let mut worker = cleanup_test_session("worker", "worker-inc");
        crate::mark_tmux_runtime_never_launched(&mut worker);
        fs::create_dir_all(session_dir(&context, &worker.id)).unwrap();
        crate::write_session_record(&context, &worker).unwrap();

        let mut run = run_record("run-one", false);
        run.controller = session_ref(&context, &main, "inc");
        let mut assignment = dep_assignment("assignment", "run-one", "accepted");
        assignment.primary_manager = run.controller.clone();
        assignment.worker = Some(session_ref(&context, &worker, "worker-inc"));
        {
            let mut locked = orchestration::lock_registry(&context).unwrap();
            locked.registry.runs.insert(run.run_id.clone(), run);
            locked
                .registry
                .assignments
                .insert(assignment.assignment_id.clone(), assignment);
            locked.save().unwrap();
        }

        let preview = preview_group_cleanup(&context, "main").unwrap();
        let execution = execute_group_cleanup(
            &context,
            "main",
            GroupCleanupRequest {
                schema_version: GROUP_CLEANUP_REQUEST_SCHEMA.to_string(),
                expected_main_incarnation: "inc".to_string(),
                expected_run_revision: preview["run_revision"].as_u64().unwrap(),
                expected_plan_digest: preview["plan_digest"].as_str().unwrap().to_string(),
                mode: GroupCleanupMode::Safe,
                idempotency_key: "cleanup-success-001".to_string(),
            },
            tmux,
        )
        .unwrap();

        assert_eq!(
            execution.value["completed"], true,
            "unexpected cleanup result: {}",
            execution.value
        );
        assert_eq!(execution.value["run_closed"], true);
        assert_eq!(execution.value["main_deleted"], true);
        assert_eq!(execution.value["workers"][0]["outcome"], "deleted");
        assert!(!session_dir(&context, "worker").exists());
        assert!(!session_dir(&context, "main").exists());
        let registry = orchestration::load_registry_readonly(&context).unwrap();
        assert_eq!(registry.runs["run-one"].state, "closed");
    }

    #[test]
    fn group_cleanup_exact_retry_adopts_every_durable_interruption_stage() {
        for (stage, worker_started) in [
            ("authority_fence", true),
            ("authority_sealed", true),
            ("worker_checkpoint", false),
            ("worker_delete_pending:assignment", true),
            ("worker_deleted_uncheckpointed:assignment", true),
            ("worker_deleted:assignment", true),
            ("run_closed", true),
            ("main_delete_pending", true),
            ("main_deleted_uncheckpointed", true),
            ("main_deleted", true),
        ] {
            let tmp = tempfile::TempDir::new().unwrap();
            let context = CliContext {
                state_dir: tmp.path().join("state"),
                host: None,
            };
            let tmux = tmp.path().join("tmux-missing-session");
            fs::write(
                &tmux,
                "#!/bin/sh\nprintf \"%s\\n\" \"can't find session: test\" >&2\nexit 1\n",
            )
            .unwrap();
            fs::set_permissions(&tmux, fs::Permissions::from_mode(0o700)).unwrap();
            let mut main = cleanup_test_session("main", "inc");
            crate::mark_tmux_runtime_never_launched(&mut main);
            fs::create_dir_all(session_dir(&context, &main.id)).unwrap();
            crate::write_session_record(&context, &main).unwrap();
            let mut run = run_record("run-one", false);
            run.controller = session_ref(&context, &main, "inc");
            let mut assignment = dep_assignment("assignment", "run-one", "accepted");
            assignment.primary_manager = run.controller.clone();
            if worker_started {
                let mut worker = cleanup_test_session("worker", "worker-inc");
                crate::mark_tmux_runtime_never_launched(&mut worker);
                fs::create_dir_all(session_dir(&context, &worker.id)).unwrap();
                crate::write_session_record(&context, &worker).unwrap();
                assignment.worker = Some(session_ref(&context, &worker, "worker-inc"));
            } else {
                assignment.worker = None;
            }
            {
                let mut locked = orchestration::lock_registry(&context).unwrap();
                locked.registry.runs.insert(run.run_id.clone(), run);
                locked
                    .registry
                    .assignments
                    .insert(assignment.assignment_id.clone(), assignment);
                locked.save().unwrap();
            }
            let preview = preview_group_cleanup(&context, "main").unwrap();
            let request = GroupCleanupRequest {
                schema_version: GROUP_CLEANUP_REQUEST_SCHEMA.to_string(),
                expected_main_incarnation: "inc".to_string(),
                expected_run_revision: preview["run_revision"].as_u64().unwrap(),
                expected_plan_digest: preview["plan_digest"].as_str().unwrap().to_string(),
                mode: GroupCleanupMode::Safe,
                idempotency_key: format!("cleanup-interrupt-{stage}"),
            };
            fs::write(
                context.state_dir.join("group-cleanup-interrupt-test"),
                stage,
            )
            .unwrap();
            let interrupted =
                match execute_group_cleanup(&context, "main", request.clone(), tmux.clone()) {
                    Ok(_) => panic!("expected stage interruption"),
                    Err(error) => error,
                };
            assert_eq!(interrupted.code(), "group-cleanup-test-interrupted");
            fs::remove_file(context.state_dir.join("group-cleanup-interrupt-test")).unwrap();
            if stage == "main_deleted_uncheckpointed" {
                let progress_key =
                    group_cleanup_progress_key("main", "inc", &request.idempotency_key);
                let progress_path = context
                    .state_dir
                    .join("orchestration/group-cleanup-progress")
                    .join(&progress_key);
                fs::File::options()
                    .write(true)
                    .open(&progress_path)
                    .unwrap()
                    .set_times(
                        fs::FileTimes::new()
                            .set_modified(std::time::UNIX_EPOCH + Duration::from_secs(1)),
                    )
                    .unwrap();
                for index in 0..128 {
                    let competing = GroupCleanupProgressReceipt {
                        schema_version: "agent-session.main-agent-group-cleanup-receipt.v1"
                            .to_string(),
                        requested_session_id: None,
                        principal_session_id: format!("abandoned-main-{index}"),
                        principal_incarnation: "abandoned-incarnation".to_string(),
                        idempotency_key: "retention-pressure".to_string(),
                        request_digest: "e".repeat(64),
                        outcome: json!({
                            "schema_version": GROUP_CLEANUP_RESULT_SCHEMA,
                            "completed": false,
                            "workers": []
                        }),
                    };
                    orchestration::store_group_cleanup_progress(
                        &context,
                        &format!("{index:064x}"),
                        &serde_json::to_vec(&competing).unwrap(),
                    )
                    .unwrap();
                }
                assert!(
                    orchestration::read_group_cleanup_progress(&context, &progress_key)
                        .unwrap()
                        .is_some(),
                    "post-delete resume progress must survive retention pressure"
                );
            }
            let resumed =
                execute_group_cleanup(&context, "main", request.clone(), tmux.clone()).unwrap();
            assert_eq!(
                resumed.value["completed"], true,
                "stage={stage} result={}",
                resumed.value
            );
            let replayed = execute_group_cleanup(&context, "main", request, tmux).unwrap();
            assert_eq!(replayed.value, resumed.value, "stage={stage}");
            assert_eq!(
                replayed.deleted_registry_fences, resumed.deleted_registry_fences,
                "stage={stage}"
            );
        }
    }

    #[test]
    fn group_cleanup_safe_and_force_requests_have_one_execution_owner() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let safe_request = GroupCleanupRequest {
            schema_version: GROUP_CLEANUP_REQUEST_SCHEMA.to_string(),
            expected_main_incarnation: "incarnation".to_string(),
            expected_run_revision: 1,
            expected_plan_digest: format!("sha256:{}", "a".repeat(64)),
            mode: GroupCleanupMode::Safe,
            idempotency_key: "safe-cleanup".to_string(),
        };
        let mut force_request = safe_request.clone();
        force_request.mode = GroupCleanupMode::Force;
        force_request.idempotency_key = "force-cleanup".to_string();
        assert_ne!(
            group_cleanup_request_digest(&safe_request),
            group_cleanup_request_digest(&force_request),
            "safe and force remain distinct idempotent requests"
        );
        let owner_digest = group_cleanup_execution_owner_digest("main");
        let first = lock_group_cleanup_execution(&context, &owner_digest).unwrap();
        let competing = lock_group_cleanup_execution(&context, &owner_digest).unwrap_err();
        assert_eq!(competing.code(), "group-cleanup-in-progress");
        drop(first);
        lock_group_cleanup_execution(&context, &owner_digest).unwrap();
    }

    #[test]
    fn group_cleanup_execution_lock_is_not_inherited_by_provider_children() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let owner_digest = group_cleanup_execution_owner_digest("main");
        let lock = lock_group_cleanup_execution(&context, &owner_digest).unwrap();
        let descriptor_flags = unsafe { libc::fcntl(lock._file.as_raw_fd(), libc::F_GETFD) };
        assert_ne!(descriptor_flags, -1);
        assert_ne!(
            descriptor_flags & libc::FD_CLOEXEC,
            0,
            "provider and tmux children must not inherit the cleanup execution flock"
        );
    }

    #[test]
    fn group_cleanup_execution_lock_rejects_a_symlinked_orchestration_root() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::TempDir::new().unwrap();
        let outside = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        fs::create_dir_all(&context.state_dir).unwrap();
        symlink(outside.path(), context.state_dir.join("orchestration")).unwrap();

        let owner_digest = group_cleanup_execution_owner_digest("main");
        let error = lock_group_cleanup_execution(&context, &owner_digest).unwrap_err();
        assert_eq!(error.code(), "orchestration-store-invalid");
        assert!(
            !outside
                .path()
                .join(format!("group-cleanup-{owner_digest}.lock"))
                .exists(),
            "cleanup locking must not create files through an unsafe parent symlink"
        );
    }

    #[test]
    fn group_cleanup_session_aliases_share_lock_but_not_exact_receipt_selector() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let main = cleanup_test_session("main-controller-unique", "main-incarnation");
        fs::create_dir_all(session_dir(&context, &main.id)).unwrap();
        crate::write_session_record(&context, &main).unwrap();
        let request = GroupCleanupRequest {
            schema_version: GROUP_CLEANUP_REQUEST_SCHEMA.to_string(),
            expected_main_incarnation: "main-incarnation".to_string(),
            expected_run_revision: 1,
            expected_plan_digest: format!("sha256:{}", "a".repeat(64)),
            mode: GroupCleanupMode::Safe,
            idempotency_key: "alias-cleanup".to_string(),
        };
        let owner_digest = group_cleanup_execution_owner_digest(&main.id);
        let lock = lock_group_cleanup_execution(&context, &owner_digest).unwrap();
        for alias in ["main-c", "main-controller"] {
            let error = execute_group_cleanup(
                &context,
                alias,
                request.clone(),
                PathBuf::from("/bin/false"),
            )
            .err()
            .expect("an exact cleanup lock must reject every session alias");
            assert_eq!(
                error.code(),
                "group-cleanup-in-progress",
                "{alias} must collide with the exact session cleanup lock"
            );
        }
        drop(lock);

        let request_digest = group_cleanup_request_digest(&request);
        {
            let mut locked = orchestration::lock_registry(&context).unwrap();
            store_receipt_for_principal(
                &mut locked.registry,
                &main.id,
                "main-incarnation",
                &request.idempotency_key,
                "group-cleanup",
                &request_digest,
                json!({
                    "schema_version": GROUP_CLEANUP_RESULT_SCHEMA,
                    "completed": true,
                    "run_closed": true,
                    "main_deleted": true,
                    "workers": [],
                }),
            )
            .unwrap();
            locked.save().unwrap();
        }
        for alias in ["main-c", "main-controller"] {
            let error = match execute_group_cleanup(
                &context,
                alias,
                request.clone(),
                PathBuf::from("/bin/false"),
            ) {
                Ok(_) => panic!("a canonical-only receipt must not authorize an alias replay"),
                Err(error) => error,
            };
            assert_eq!(
                error.code(),
                "main-agent-run-not-found",
                "{alias} must not adopt the canonical receipt selector"
            );
        }
        let replay = execute_group_cleanup(
            &context,
            "main-controller-unique",
            request,
            PathBuf::from("/bin/false"),
        )
        .unwrap();
        assert_eq!(replay.value["completed"], true);
    }

    #[test]
    fn group_cleanup_alias_retry_adopts_legacy_incomplete_receipt_namespace() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let mut main = cleanup_test_session("main-controller-unique", "main-incarnation");
        crate::mark_tmux_runtime_never_launched(&mut main);
        fs::create_dir_all(session_dir(&context, &main.id)).unwrap();
        crate::write_session_record(&context, &main).unwrap();
        let mut run = run_record("run-one", false);
        run.controller = session_ref(&context, &main, "main-incarnation");
        {
            let mut locked = orchestration::lock_registry(&context).unwrap();
            locked.registry.runs.insert(run.run_id.clone(), run);
            locked.save().unwrap();
        }
        let preview = preview_group_cleanup(&context, "main-c").unwrap();
        let plan: GroupCleanupPlan = serde_json::from_value(preview.clone()).unwrap();
        let request = GroupCleanupRequest {
            schema_version: GROUP_CLEANUP_REQUEST_SCHEMA.to_string(),
            expected_main_incarnation: "main-incarnation".to_string(),
            expected_run_revision: preview["run_revision"].as_u64().unwrap(),
            expected_plan_digest: preview["plan_digest"].as_str().unwrap().to_string(),
            mode: GroupCleanupMode::Safe,
            idempotency_key: "prior-alias-progress".to_string(),
        };
        let request_digest = group_cleanup_request_digest(&request);
        let legacy_results = vec![json!({
            "assignment_id": "prior-progress-canary",
            "outcome": "prior-alias-adopted"
        })];
        store_group_cleanup_receipt(
            &context,
            &GroupCleanupProgressIdentity {
                requested_session_id: "main-c",
                principal_session_id: "main-c",
                incarnation: "main-incarnation",
            },
            &request,
            &request_digest,
            group_cleanup_progress_value(&plan, &legacy_results, false, "authority_sealed"),
            group_cleanup_resume_state(&plan, &legacy_results, &[], &[], false),
        )
        .unwrap();
        let legacy_progress_key =
            group_cleanup_progress_key("main-c", "main-incarnation", &request.idempotency_key);
        let legacy_progress_path = context
            .state_dir
            .join("orchestration/group-cleanup-progress")
            .join(&legacy_progress_key);
        let mut prior_progress: GroupCleanupProgressReceipt = serde_json::from_slice(
            &orchestration::read_group_cleanup_progress(&context, &legacy_progress_key)
                .unwrap()
                .expect("alias progress"),
        )
        .unwrap();
        prior_progress.schema_version =
            orchestration::GROUP_CLEANUP_PROGRESS_RECEIPT_V1_SCHEMA.to_string();
        prior_progress.requested_session_id = None;
        orchestration::store_group_cleanup_progress(
            &context,
            &legacy_progress_key,
            &serde_json::to_vec(&prior_progress).unwrap(),
        )
        .unwrap();
        fs::File::options()
            .write(true)
            .open(&legacy_progress_path)
            .unwrap()
            .set_times(
                fs::FileTimes::new().set_modified(std::time::UNIX_EPOCH + Duration::from_secs(1)),
            )
            .unwrap();
        for index in 0..128 {
            let competing = GroupCleanupProgressReceipt {
                schema_version: "agent-session.main-agent-group-cleanup-receipt.v1".to_string(),
                requested_session_id: None,
                principal_session_id: format!("abandoned-alias-main-{index}"),
                principal_incarnation: "abandoned-incarnation".to_string(),
                idempotency_key: "prior-alias-retention-pressure".to_string(),
                request_digest: "f".repeat(64),
                outcome: json!({
                    "schema_version": GROUP_CLEANUP_RESULT_SCHEMA,
                    "completed": false,
                    "workers": []
                }),
            };
            orchestration::store_group_cleanup_progress(
                &context,
                &format!("{index:064x}"),
                &serde_json::to_vec(&competing).unwrap(),
            )
            .unwrap();
        }
        assert!(
            orchestration::read_group_cleanup_progress(&context, &legacy_progress_key)
                .unwrap()
                .is_some(),
            "retention pressure must preserve live prior-version alias progress"
        );

        let tmux = tmp.path().join("tmux-missing-session");
        fs::write(
            &tmux,
            "#!/bin/sh\nprintf \"%s\\n\" \"can't find session: test\" >&2\nexit 1\n",
        )
        .unwrap();
        fs::set_permissions(&tmux, fs::Permissions::from_mode(0o700)).unwrap();
        let execution =
            execute_group_cleanup(&context, "main-c", request.clone(), tmux.clone()).unwrap();
        assert_eq!(
            execution.value["completed"], true,
            "unexpected prior-version replay result: {}",
            execution.value
        );
        assert_eq!(
            execution.value["workers"][0]["assignment_id"], "prior-progress-canary",
            "the canonical retry must adopt the prior alias-keyed progress"
        );
        assert!(
            orchestration::read_group_cleanup_progress(&context, &legacy_progress_key)
                .unwrap()
                .is_none(),
            "canonical completion must remove the adopted prior-version alias sidecar"
        );
        let replayed = execute_group_cleanup(&context, "main-c", request, tmux).unwrap();
        assert_eq!(replayed.value, execution.value);
        assert_eq!(
            replayed.deleted_registry_fences,
            execution.deleted_registry_fences
        );
    }

    #[test]
    fn group_cleanup_alias_retry_recovers_after_main_deletion_and_rejects_alias_reuse() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let mut main = cleanup_test_session("main-controller-unique", "main-incarnation");
        crate::mark_tmux_runtime_never_launched(&mut main);
        fs::create_dir_all(session_dir(&context, &main.id)).unwrap();
        crate::write_session_record(&context, &main).unwrap();
        let mut run = run_record("run-one", false);
        run.controller = session_ref(&context, &main, "main-incarnation");
        {
            let mut locked = orchestration::lock_registry(&context).unwrap();
            locked.registry.runs.insert(run.run_id.clone(), run);
            locked.save().unwrap();
        }
        let preview = preview_group_cleanup(&context, "main-c").unwrap();
        let request = GroupCleanupRequest {
            schema_version: GROUP_CLEANUP_REQUEST_SCHEMA.to_string(),
            expected_main_incarnation: "main-incarnation".to_string(),
            expected_run_revision: preview["run_revision"].as_u64().unwrap(),
            expected_plan_digest: preview["plan_digest"].as_str().unwrap().to_string(),
            mode: GroupCleanupMode::Safe,
            idempotency_key: "alias-post-delete-retry".to_string(),
        };
        let tmux = tmp.path().join("tmux-missing-session");
        fs::write(
            &tmux,
            "#!/bin/sh\nprintf \"%s\\n\" \"can't find session: test\" >&2\nexit 1\n",
        )
        .unwrap();
        fs::set_permissions(&tmux, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(
            context.state_dir.join("group-cleanup-interrupt-test"),
            "main_deleted_uncheckpointed",
        )
        .unwrap();
        let interrupted = execute_group_cleanup(&context, "main-c", request.clone(), tmux.clone())
            .err()
            .expect("cleanup must stop after deleting the canonical Main Agent");
        assert_eq!(interrupted.code(), "group-cleanup-test-interrupted");
        assert!(!session_dir(&context, "main-controller-unique").exists());
        fs::remove_file(context.state_dir.join("group-cleanup-interrupt-test")).unwrap();
        let canonical_progress_key = group_cleanup_progress_key(
            "main-controller-unique",
            "main-incarnation",
            &request.idempotency_key,
        );
        let pending_progress: GroupCleanupProgressReceipt = serde_json::from_slice(
            &orchestration::read_group_cleanup_progress(&context, &canonical_progress_key)
                .unwrap()
                .expect("canonical pending progress"),
        )
        .unwrap();
        assert_eq!(
            pending_progress.schema_version,
            orchestration::GROUP_CLEANUP_PROGRESS_RECEIPT_SCHEMA
        );
        assert_eq!(
            pending_progress.requested_session_id.as_deref(),
            Some("main-c"),
            "new progress must retain the exact original selector"
        );
        assert_eq!(
            pending_progress.principal_session_id,
            "main-controller-unique"
        );

        let resumed =
            execute_group_cleanup(&context, "main-c", request.clone(), tmux.clone()).unwrap();
        assert_eq!(resumed.value["completed"], true);
        assert!(
            resumed
                .deleted_registry_fences
                .iter()
                .any(|fence| fence.session_id == "main-controller-unique"),
            "alias retry must recover the deleted canonical session fence"
        );
        orchestration::store_group_cleanup_progress(
            &context,
            &canonical_progress_key,
            br#"{"ambiguous_terminal_cleanup":true}"#,
        )
        .unwrap();
        let replayed =
            execute_group_cleanup(&context, "main-c", request.clone(), tmux.clone()).unwrap();
        assert_eq!(replayed.value, resumed.value);
        assert_eq!(
            replayed.deleted_registry_fences,
            resumed.deleted_registry_fences
        );
        assert!(
            orchestration::read_group_cleanup_progress(&context, &canonical_progress_key)
                .unwrap()
                .is_none(),
            "completed replay must reconcile a canonical sidecar left by ambiguous cleanup"
        );

        let mut replacement = cleanup_test_session("main-c-replacement", "replacement-incarnation");
        crate::mark_tmux_runtime_never_launched(&mut replacement);
        fs::create_dir_all(session_dir(&context, &replacement.id)).unwrap();
        crate::write_session_record(&context, &replacement).unwrap();
        let before_registry =
            fs::read(context.state_dir.join("orchestration/registry.json")).unwrap();
        let conflict = execute_group_cleanup(&context, "main-c", request, tmux)
            .err()
            .expect("a reused alias must not adopt the prior canonical cleanup");
        assert_eq!(conflict.code(), "main-session-incarnation-conflict");
        assert_eq!(
            fs::read(context.state_dir.join("orchestration/registry.json")).unwrap(),
            before_registry,
            "alias reuse must not mutate either cleanup namespace"
        );
    }

    #[test]
    fn group_cleanup_legacy_alias_retry_recovers_after_main_deletion_and_reconciles_receipt() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let mut main = cleanup_test_session("main-controller-unique", "main-incarnation");
        crate::mark_tmux_runtime_never_launched(&mut main);
        fs::create_dir_all(session_dir(&context, &main.id)).unwrap();
        crate::write_session_record(&context, &main).unwrap();
        let mut run = run_record("run-one", false);
        run.controller = session_ref(&context, &main, "main-incarnation");
        {
            let mut locked = orchestration::lock_registry(&context).unwrap();
            locked.registry.runs.insert(run.run_id.clone(), run);
            locked.save().unwrap();
        }
        let preview = preview_group_cleanup(&context, "main-c").unwrap();
        let request = GroupCleanupRequest {
            schema_version: GROUP_CLEANUP_REQUEST_SCHEMA.to_string(),
            expected_main_incarnation: "main-incarnation".to_string(),
            expected_run_revision: preview["run_revision"].as_u64().unwrap(),
            expected_plan_digest: preview["plan_digest"].as_str().unwrap().to_string(),
            mode: GroupCleanupMode::Safe,
            idempotency_key: "prior-alias-post-delete".to_string(),
        };
        let tmux = tmp.path().join("tmux-missing-session");
        fs::write(
            &tmux,
            "#!/bin/sh\nprintf \"%s\\n\" \"can't find session: test\" >&2\nexit 1\n",
        )
        .unwrap();
        fs::set_permissions(&tmux, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(
            context.state_dir.join("group-cleanup-interrupt-test"),
            "main_deleted_uncheckpointed",
        )
        .unwrap();
        let interrupted = execute_group_cleanup(&context, "main-c", request.clone(), tmux.clone())
            .err()
            .expect("cleanup must stop after deleting the canonical Main Agent");
        assert_eq!(interrupted.code(), "group-cleanup-test-interrupted");
        fs::remove_file(context.state_dir.join("group-cleanup-interrupt-test")).unwrap();

        let canonical_progress_key = group_cleanup_progress_key(
            "main-controller-unique",
            "main-incarnation",
            &request.idempotency_key,
        );
        let legacy_progress_key =
            group_cleanup_progress_key("main-c", "main-incarnation", &request.idempotency_key);
        let mut progress: GroupCleanupProgressReceipt = serde_json::from_slice(
            &orchestration::read_group_cleanup_progress(&context, &canonical_progress_key)
                .unwrap()
                .expect("canonical pending progress"),
        )
        .unwrap();
        progress.schema_version =
            orchestration::GROUP_CLEANUP_PROGRESS_RECEIPT_V1_SCHEMA.to_string();
        progress.requested_session_id = None;
        progress.principal_session_id = "main-c".to_string();
        orchestration::store_group_cleanup_progress(
            &context,
            &legacy_progress_key,
            &serde_json::to_vec(&progress).unwrap(),
        )
        .unwrap();
        orchestration::remove_group_cleanup_progress(&context, &canonical_progress_key).unwrap();
        {
            let mut locked = orchestration::lock_registry(&context).unwrap();
            let canonical_receipt_key = receipt_key(
                "main-controller-unique",
                "main-incarnation",
                &request.idempotency_key,
            );
            let legacy_receipt_key =
                receipt_key("main-c", "main-incarnation", &request.idempotency_key);
            let mut receipt = locked
                .registry
                .receipts
                .remove(&canonical_receipt_key)
                .expect("canonical run-closed receipt");
            receipt.principal_session_id = "main-c".to_string();
            locked.registry.receipts.insert(legacy_receipt_key, receipt);
            locked.save().unwrap();
        }

        let resumed =
            execute_group_cleanup(&context, "main-c", request.clone(), tmux.clone()).unwrap();
        assert_eq!(resumed.value["completed"], true);
        assert!(
            orchestration::read_group_cleanup_progress(&context, &legacy_progress_key)
                .unwrap()
                .is_none(),
            "prior-version sidecar must be removed after canonical completion"
        );
        let registry = orchestration::load_registry_readonly(&context).unwrap();
        let legacy_receipt = &registry.receipts
            [&receipt_key("main-c", "main-incarnation", &request.idempotency_key)];
        assert_eq!(
            legacy_receipt.outcome["completed"], true,
            "prior-version registry readers must observe the reconciled terminal outcome"
        );
        let replayed = execute_group_cleanup(&context, "main-c", request, tmux).unwrap();
        assert_eq!(replayed.value, resumed.value);
        assert_eq!(
            replayed.deleted_registry_fences,
            resumed.deleted_registry_fences
        );
    }

    #[test]
    fn group_cleanup_progress_does_not_rewrite_near_limit_registry_per_worker() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        fs::create_dir(&context.state_dir).unwrap();
        {
            let mut locked = orchestration::lock_registry(&context).unwrap();
            locked.registry.receipts.insert(
                "near-limit-filler".to_string(),
                orchestration::IdempotencyReceipt {
                    principal_session_id: "filler".to_string(),
                    principal_incarnation: "filler-incarnation".to_string(),
                    operation: "filler".to_string(),
                    request_digest: "f".repeat(64),
                    outcome: json!({ "padding": "x".repeat(3 * 1024 * 1024) }),
                    created_at_epoch: 0,
                },
            );
            locked.save().unwrap();
        }
        orchestration::reset_registry_save_bytes_for_test();
        let main = SessionRef {
            machine: None,
            session_id: "main-controller".to_string(),
            session_incarnation: "main-incarnation".to_string(),
            session_created_at: "2030-01-01T00:00:00Z".to_string(),
        };
        let plan = GroupCleanupPlan {
            schema_version: GROUP_CLEANUP_SCHEMA.to_string(),
            main,
            run_id: "run-one".to_string(),
            run_revision: 1,
            requires_force: false,
            workers: Vec::new(),
            plan_digest: format!("sha256:{}", "a".repeat(64)),
        };
        let request = GroupCleanupRequest {
            schema_version: GROUP_CLEANUP_REQUEST_SCHEMA.to_string(),
            expected_main_incarnation: "main-incarnation".to_string(),
            expected_run_revision: 1,
            expected_plan_digest: plan.plan_digest.clone(),
            mode: GroupCleanupMode::Safe,
            idempotency_key: "bounded-progress".to_string(),
        };
        let request_digest = group_cleanup_request_digest(&request);
        let mut worker_results = Vec::new();
        for index in 0..64 {
            worker_results.push(json!({
                "assignment_id": format!("worker-{index:02}"),
                "outcome": "deleted",
            }));
            let value =
                group_cleanup_progress_value(&plan, &worker_results, false, "worker_deleted");
            store_group_cleanup_receipt(
                &context,
                &GroupCleanupProgressIdentity {
                    requested_session_id: "main-controller",
                    principal_session_id: "main-controller",
                    incarnation: "main-incarnation",
                },
                &request,
                &request_digest,
                value,
                group_cleanup_resume_state(&plan, &worker_results, &[], &[], false),
            )
            .unwrap();
        }
        assert!(
            orchestration::registry_save_bytes_for_test() <= 8 * 1024 * 1024,
            "worker progress must not serialize a near-limit registry once per checkpoint"
        );
    }

    #[test]
    fn group_cleanup_progress_retention_bounds_abandoned_records_without_evicting_active() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        fs::create_dir(&context.state_dir).unwrap();
        let active_main = cleanup_test_session("active-main", "active-incarnation");
        crate::write_session_record(&context, &active_main).unwrap();

        let progress_bytes = |principal_session_id: &str, principal_incarnation: &str| {
            serde_json::to_vec(&GroupCleanupProgressReceipt {
                schema_version: "agent-session.main-agent-group-cleanup-receipt.v1".to_string(),
                requested_session_id: None,
                principal_session_id: principal_session_id.to_string(),
                principal_incarnation: principal_incarnation.to_string(),
                idempotency_key: "bounded-retention".to_string(),
                request_digest: "a".repeat(64),
                outcome: json!({
                    "schema_version": GROUP_CLEANUP_RESULT_SCHEMA,
                    "completed": false,
                    "workers": []
                }),
            })
            .unwrap()
        };
        let active_key = "a".repeat(64);
        orchestration::store_group_cleanup_progress(
            &context,
            &active_key,
            &progress_bytes("active-main", "active-incarnation"),
        )
        .unwrap();
        for index in 0..160 {
            orchestration::store_group_cleanup_progress(
                &context,
                &format!("{index:064x}"),
                &progress_bytes(&format!("abandoned-main-{index}"), "stale-incarnation"),
            )
            .unwrap();
        }

        let progress_dir = context
            .state_dir
            .join("orchestration/group-cleanup-progress");
        let retained = fs::read_dir(&progress_dir).unwrap().count();
        assert!(
            retained <= 128,
            "abandoned progress must stay under the aggregate file-count bound"
        );
        assert!(
            orchestration::read_group_cleanup_progress(&context, &active_key)
                .unwrap()
                .is_some(),
            "an exact live principal's resumable progress must be retained"
        );
    }

    #[test]
    fn group_cleanup_progress_retention_reads_bodies_only_under_pressure_and_never_evicts_live() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        fs::create_dir(&context.state_dir).unwrap();
        let progress_bytes = |principal_session_id: &str, principal_incarnation: &str| {
            serde_json::to_vec(&GroupCleanupProgressReceipt {
                schema_version: "agent-session.main-agent-group-cleanup-receipt.v1".to_string(),
                requested_session_id: None,
                principal_session_id: principal_session_id.to_string(),
                principal_incarnation: principal_incarnation.to_string(),
                idempotency_key: "retention-read-bound".to_string(),
                request_digest: "b".repeat(64),
                outcome: json!({
                    "schema_version": GROUP_CLEANUP_RESULT_SCHEMA,
                    "completed": false,
                    "workers": []
                }),
            })
            .unwrap()
        };

        for index in 0..10 {
            orchestration::store_group_cleanup_progress(
                &context,
                &format!("{index:064x}"),
                &progress_bytes(&format!("missing-{index}"), "missing-incarnation"),
            )
            .unwrap();
        }
        orchestration::reset_group_cleanup_progress_body_reads_for_test();
        orchestration::store_group_cleanup_progress(
            &context,
            &format!("{:064x}", 10),
            &progress_bytes("missing-10", "missing-incarnation"),
        )
        .unwrap();
        assert_eq!(
            orchestration::group_cleanup_progress_body_reads_for_test(),
            0,
            "an in-capacity checkpoint must scan metadata without reading progress bodies"
        );

        let progress_dir = context
            .state_dir
            .join("orchestration/group-cleanup-progress");
        for index in 10..128 {
            let session_id = format!("live-main-{index}");
            let incarnation = format!("live-incarnation-{index}");
            let record = cleanup_test_session(&session_id, &incarnation);
            crate::write_session_record(&context, &record).unwrap();
            let path = progress_dir.join(format!("{index:064x}"));
            fs::write(&path, progress_bytes(&session_id, &incarnation)).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
            fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_times(
                    fs::FileTimes::new()
                        .set_modified(std::time::UNIX_EPOCH + Duration::from_secs(1)),
                )
                .unwrap();
        }
        for index in 0..10 {
            let session_id = format!("live-main-{index}");
            let incarnation = format!("live-incarnation-{index}");
            let record = cleanup_test_session(&session_id, &incarnation);
            crate::write_session_record(&context, &record).unwrap();
            let path = progress_dir.join(format!("{index:064x}"));
            fs::write(&path, progress_bytes(&session_id, &incarnation)).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
            fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_times(
                    fs::FileTimes::new()
                        .set_modified(std::time::UNIX_EPOCH + Duration::from_secs(1)),
                )
                .unwrap();
        }

        let error = orchestration::store_group_cleanup_progress(
            &context,
            &"f".repeat(64),
            &progress_bytes("new-main", "new-incarnation"),
        )
        .expect_err("all-live capacity must fail closed");
        assert_eq!(error.code(), "group-cleanup-progress-capacity");
        assert_eq!(fs::read_dir(&progress_dir).unwrap().count(), 128);
        assert!(
            orchestration::read_group_cleanup_progress(&context, &format!("{:064x}", 0))
                .unwrap()
                .is_some(),
            "old progress for an exact live incarnation must never be evicted"
        );
    }

    #[test]
    fn group_cleanup_progress_removal_serializes_with_capacity_admission() {
        let _fixture_ownership = GlobalStateLock::new();
        let tmp = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        fs::create_dir(&context.state_dir).unwrap();
        let progress_bytes = |principal_session_id: &str, principal_incarnation: &str| {
            serde_json::to_vec(&GroupCleanupProgressReceipt {
                schema_version: "agent-session.main-agent-group-cleanup-receipt.v1".to_string(),
                requested_session_id: None,
                principal_session_id: principal_session_id.to_string(),
                principal_incarnation: principal_incarnation.to_string(),
                idempotency_key: "retention-removal-race".to_string(),
                request_digest: "c".repeat(64),
                outcome: json!({
                    "schema_version": GROUP_CLEANUP_RESULT_SCHEMA,
                    "completed": false,
                    "workers": []
                }),
            })
            .unwrap()
        };
        let abandoned_key = format!("{:064x}", 0);
        orchestration::store_group_cleanup_progress(
            &context,
            &abandoned_key,
            &progress_bytes("abandoned-main", "abandoned-incarnation"),
        )
        .unwrap();
        let progress_dir = context
            .state_dir
            .join("orchestration/group-cleanup-progress");
        fs::File::options()
            .write(true)
            .open(progress_dir.join(&abandoned_key))
            .unwrap()
            .set_times(
                fs::FileTimes::new().set_modified(std::time::UNIX_EPOCH + Duration::from_secs(1)),
            )
            .unwrap();
        for index in 1..128 {
            let session_id = format!("live-main-{index}");
            let incarnation = format!("live-incarnation-{index}");
            crate::write_session_record(&context, &cleanup_test_session(&session_id, &incarnation))
                .unwrap();
            let path = progress_dir.join(format!("{index:064x}"));
            fs::write(&path, progress_bytes(&session_id, &incarnation)).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        }

        let (scanned, resume) =
            orchestration::install_group_cleanup_progress_scan_hook_for_test(&progress_dir);
        let writer_context = context.clone();
        let incoming = progress_bytes("new-main", "new-incarnation");
        let writer = thread::spawn(move || {
            orchestration::store_group_cleanup_progress(&writer_context, &"f".repeat(64), &incoming)
        });
        scanned.wait();

        let remover_context = context.clone();
        let remover_key = abandoned_key.clone();
        let (removed_tx, removed_rx) = std::sync::mpsc::channel();
        let remover = thread::spawn(move || {
            let result =
                orchestration::remove_group_cleanup_progress(&remover_context, &remover_key);
            removed_tx.send(()).unwrap();
            result
        });
        let removal_completed_while_store_locked =
            removed_rx.recv_timeout(Duration::from_millis(50)).is_ok();
        resume.wait();

        let writer_result = writer.join().unwrap();
        let remover_result = remover.join().unwrap();
        assert!(
            !removal_completed_while_store_locked,
            "removal must wait for the aggregate capacity snapshot and admission"
        );
        writer_result.expect("capacity admission must use a stable projection");
        remover_result.expect("idempotent removal must succeed after admission");
        assert!(
            fs::read_dir(&progress_dir).unwrap().count() <= 128,
            "the serialized race must preserve the aggregate file-count bound"
        );
    }

    #[test]
    fn completed_group_cleanup_receipt_does_not_hold_registry_while_waiting_for_progress_lock() {
        let _fixture_ownership = GlobalStateLock::new();
        let tmp = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        fs::create_dir(&context.state_dir).unwrap();
        let main = SessionRef {
            machine: None,
            session_id: "main-controller".to_string(),
            session_incarnation: "main-incarnation".to_string(),
            session_created_at: "2030-01-01T00:00:00Z".to_string(),
        };
        let plan = GroupCleanupPlan {
            schema_version: GROUP_CLEANUP_SCHEMA.to_string(),
            main,
            run_id: "run-one".to_string(),
            run_revision: 1,
            requires_force: false,
            workers: Vec::new(),
            plan_digest: format!("sha256:{}", "a".repeat(64)),
        };
        let request = GroupCleanupRequest {
            schema_version: GROUP_CLEANUP_REQUEST_SCHEMA.to_string(),
            expected_main_incarnation: "main-incarnation".to_string(),
            expected_run_revision: 1,
            expected_plan_digest: plan.plan_digest.clone(),
            mode: GroupCleanupMode::Safe,
            idempotency_key: "completed-lock-order".to_string(),
        };
        let request_digest = group_cleanup_request_digest(&request);
        store_group_cleanup_receipt(
            &context,
            &GroupCleanupProgressIdentity {
                requested_session_id: "main-controller",
                principal_session_id: "main-controller",
                incarnation: "main-incarnation",
            },
            &request,
            &request_digest,
            group_cleanup_progress_value(&plan, &[], false, "worker_checkpoint"),
            group_cleanup_resume_state(&plan, &[], &[], &[], false),
        )
        .unwrap();

        let progress_dir = context
            .state_dir
            .join("orchestration/group-cleanup-progress");
        let (scanned, resume) =
            orchestration::install_group_cleanup_progress_scan_hook_for_test(&progress_dir);
        let holder_context = context.clone();
        let holder = thread::spawn(move || {
            orchestration::store_group_cleanup_progress(
                &holder_context,
                &"f".repeat(64),
                br#"{"holder":true}"#,
            )
        });
        scanned.wait();

        let finalizer_context = context.clone();
        let finalizer_request = request.clone();
        let finalizer_digest = request_digest.clone();
        let finalizer_plan = plan.clone();
        let finalizer = thread::spawn(move || {
            store_group_cleanup_receipt(
                &finalizer_context,
                &GroupCleanupProgressIdentity {
                    requested_session_id: "main-controller",
                    principal_session_id: "main-controller",
                    incarnation: "main-incarnation",
                },
                &finalizer_request,
                &finalizer_digest,
                json!({
                    "schema_version": GROUP_CLEANUP_RESULT_SCHEMA,
                    "completed": true,
                    "run_closed": true,
                    "main_deleted": true,
                    "workers": []
                }),
                group_cleanup_resume_state(&finalizer_plan, &[], &[], &[], true),
            )
        });
        let receipt_key = receipt_key(
            "main-controller",
            "main-incarnation",
            &request.idempotency_key,
        );
        let receipt_deadline = Instant::now() + Duration::from_secs(1);
        loop {
            let saved = orchestration::load_registry_readonly(&context)
                .ok()
                .is_some_and(|registry| registry.receipts.contains_key(&receipt_key));
            if saved {
                break;
            }
            assert!(
                Instant::now() < receipt_deadline,
                "completed receipt was not saved before the test deadline"
            );
            thread::sleep(Duration::from_millis(5));
        }

        let probe_context = context.clone();
        let (acquired_tx, acquired_rx) = std::sync::mpsc::channel();
        let probe = thread::spawn(move || {
            let locked = orchestration::lock_registry(&probe_context);
            acquired_tx.send(locked.is_ok()).unwrap();
            locked.map(drop)
        });
        let registry_acquired_while_progress_locked = acquired_rx
            .recv_timeout(Duration::from_millis(100))
            .is_ok_and(|value| value);
        resume.wait();

        holder.join().unwrap().unwrap();
        finalizer.join().unwrap().unwrap();
        probe.join().unwrap().unwrap();
        assert!(
            registry_acquired_while_progress_locked,
            "completed finalization must release the global registry lock before progress removal waits"
        );
    }

    #[test]
    fn completed_group_cleanup_preserves_alias_and_canonical_receipts_at_capacity() {
        let _fixture_ownership = GlobalStateLock::new();
        IDEMPOTENCY_RECEIPT_CAPACITY_FOR_TEST.store(4, Ordering::Release);
        let tmp = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        fs::create_dir(&context.state_dir).unwrap();
        let principal = "main-controller";
        let alias = "main-c";
        let incarnation = "main-incarnation";
        let request = GroupCleanupRequest {
            schema_version: GROUP_CLEANUP_REQUEST_SCHEMA.to_string(),
            expected_main_incarnation: incarnation.to_string(),
            expected_run_revision: 1,
            expected_plan_digest: format!("sha256:{}", "a".repeat(64)),
            mode: GroupCleanupMode::Safe,
            idempotency_key: "dual-receipt-capacity".to_string(),
        };
        let request_digest = group_cleanup_request_digest(&request);
        {
            let mut locked = orchestration::lock_registry(&context).unwrap();
            store_receipt_for_principal(
                &mut locked.registry,
                principal,
                incarnation,
                &request.idempotency_key,
                "group-cleanup",
                &request_digest,
                json!({ "completed": false }),
            )
            .unwrap();
            for index in 0..3 {
                let filler_principal = format!("filler-{index}");
                store_receipt_for_principal(
                    &mut locked.registry,
                    &filler_principal,
                    "filler-incarnation",
                    &format!("filler-capacity-{index}"),
                    "filler",
                    &"b".repeat(64),
                    json!({ "completed": true }),
                )
                .unwrap();
            }
            for receipt in locked.registry.receipts.values_mut() {
                receipt.created_at_epoch = i64::MAX;
            }
            let oldest_filler = receipt_key("filler-0", "filler-incarnation", "filler-capacity-0");
            locked
                .registry
                .receipts
                .get_mut(&oldest_filler)
                .unwrap()
                .created_at_epoch = i64::MIN;
            locked.save().unwrap();
        }
        let plan = GroupCleanupPlan {
            schema_version: GROUP_CLEANUP_SCHEMA.to_string(),
            main: SessionRef {
                machine: None,
                session_id: principal.to_string(),
                session_incarnation: incarnation.to_string(),
                session_created_at: "2030-01-01T00:00:00Z".to_string(),
            },
            run_id: "run-one".to_string(),
            run_revision: 1,
            requires_force: false,
            workers: Vec::new(),
            plan_digest: request.expected_plan_digest.clone(),
        };
        let completed = json!({
            "schema_version": GROUP_CLEANUP_RESULT_SCHEMA,
            "completed": true,
            "run_closed": true,
            "main_deleted": true,
            "workers": []
        });
        let outcome = group_cleanup_stored_outcome(
            &completed,
            &group_cleanup_resume_state(&plan, &[], &[], &[], true),
        )
        .unwrap();
        let stored = store_completed_group_cleanup_receipt(
            &context,
            principal,
            Some(alias),
            incarnation,
            &request,
            &request_digest,
            outcome,
        );
        IDEMPOTENCY_RECEIPT_CAPACITY_FOR_TEST.store(MAX_IDEMPOTENCY_RECEIPTS, Ordering::Release);
        stored.unwrap();

        let registry = orchestration::load_registry_readonly(&context).unwrap();
        for receipt_principal in [principal, alias] {
            let key = receipt_key(receipt_principal, incarnation, &request.idempotency_key);
            assert_eq!(
                registry.receipts[&key].outcome["completed"], true,
                "{receipt_principal} terminal receipt must survive capacity admission"
            );
        }
        assert_eq!(registry.receipts.len(), 4);
    }

    #[test]
    fn group_cleanup_progress_retention_protects_unverifiable_entries() {
        let _fixture_ownership = GlobalStateLock::new();
        let tmp = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        fs::create_dir(&context.state_dir).unwrap();
        let progress_bytes = |principal_session_id: &str, principal_incarnation: &str| {
            serde_json::to_vec(&GroupCleanupProgressReceipt {
                schema_version: "agent-session.main-agent-group-cleanup-receipt.v1".to_string(),
                requested_session_id: None,
                principal_session_id: principal_session_id.to_string(),
                principal_incarnation: principal_incarnation.to_string(),
                idempotency_key: "retention-unverifiable".to_string(),
                request_digest: "d".repeat(64),
                outcome: json!({
                    "schema_version": GROUP_CLEANUP_RESULT_SCHEMA,
                    "completed": false,
                    "workers": []
                }),
            })
            .unwrap()
        };
        let oldest_key = format!("{:064x}", 0);
        crate::write_session_record(
            &context,
            &cleanup_test_session("live-main-0", "live-incarnation-0"),
        )
        .unwrap();
        orchestration::store_group_cleanup_progress(
            &context,
            &oldest_key,
            &progress_bytes("live-main-0", "live-incarnation-0"),
        )
        .unwrap();
        let progress_dir = context
            .state_dir
            .join("orchestration/group-cleanup-progress");
        let oldest_path = progress_dir.join(&oldest_key);
        fs::File::options()
            .write(true)
            .open(&oldest_path)
            .unwrap()
            .set_times(
                fs::FileTimes::new().set_modified(std::time::UNIX_EPOCH + Duration::from_secs(1)),
            )
            .unwrap();
        for index in 1..128 {
            let session_id = format!("live-main-{index}");
            let incarnation = format!("live-incarnation-{index}");
            crate::write_session_record(&context, &cleanup_test_session(&session_id, &incarnation))
                .unwrap();
            let path = progress_dir.join(format!("{index:064x}"));
            fs::write(&path, progress_bytes(&session_id, &incarnation)).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        }

        let (scanned, resume) =
            orchestration::install_group_cleanup_progress_scan_hook_for_test(&progress_dir);
        let writer_context = context.clone();
        let incoming = progress_bytes("new-main", "new-incarnation");
        let writer = thread::spawn(move || {
            orchestration::store_group_cleanup_progress(&writer_context, &"f".repeat(64), &incoming)
        });
        scanned.wait();
        fs::write(&oldest_path, b"{").unwrap();
        fs::set_permissions(&oldest_path, fs::Permissions::from_mode(0o600)).unwrap();
        resume.wait();

        let error = writer
            .join()
            .unwrap()
            .expect_err("unverifiable progress must fail capacity admission closed");
        assert_eq!(error.code(), "group-cleanup-progress-capacity");
        assert!(
            oldest_path.exists(),
            "unverifiable exact-live progress must remain protected"
        );
        assert_eq!(fs::read_dir(&progress_dir).unwrap().count(), 128);
    }

    #[test]
    fn group_cleanup_progress_retention_does_not_unlink_a_replacement_after_identity_check() {
        let _fixture_ownership = GlobalStateLock::new();
        let tmp = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        fs::create_dir(&context.state_dir).unwrap();
        let progress_bytes = |principal_session_id: &str, principal_incarnation: &str| {
            serde_json::to_vec(&GroupCleanupProgressReceipt {
                schema_version: "agent-session.main-agent-group-cleanup-receipt.v1".to_string(),
                requested_session_id: None,
                principal_session_id: principal_session_id.to_string(),
                principal_incarnation: principal_incarnation.to_string(),
                idempotency_key: "retention-replacement-race".to_string(),
                request_digest: "e".repeat(64),
                outcome: json!({
                    "schema_version": GROUP_CLEANUP_RESULT_SCHEMA,
                    "completed": false,
                    "workers": []
                }),
            })
            .unwrap()
        };
        let abandoned_key = format!("{:064x}", 0);
        orchestration::store_group_cleanup_progress(
            &context,
            &abandoned_key,
            &progress_bytes("abandoned-main", "abandoned-incarnation"),
        )
        .unwrap();
        let progress_dir = context
            .state_dir
            .join("orchestration/group-cleanup-progress");
        let abandoned_path = progress_dir.join(&abandoned_key);
        fs::File::options()
            .write(true)
            .open(&abandoned_path)
            .unwrap()
            .set_times(
                fs::FileTimes::new().set_modified(std::time::UNIX_EPOCH + Duration::from_secs(1)),
            )
            .unwrap();
        for index in 1..128 {
            let session_id = format!("live-main-{index}");
            let incarnation = format!("live-incarnation-{index}");
            crate::write_session_record(&context, &cleanup_test_session(&session_id, &incarnation))
                .unwrap();
            let path = progress_dir.join(format!("{index:064x}"));
            fs::write(&path, progress_bytes(&session_id, &incarnation)).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        }

        let (classified, resume) =
            orchestration::install_group_cleanup_progress_eviction_hook_for_test(&abandoned_path);
        let writer_context = context.clone();
        let incoming = progress_bytes("new-main", "new-incarnation");
        let writer = thread::spawn(move || {
            orchestration::store_group_cleanup_progress(&writer_context, &"f".repeat(64), &incoming)
        });
        classified.wait();

        let replacement_session_id = "replacement-live-main";
        let replacement_incarnation = "replacement-live-incarnation";
        crate::write_session_record(
            &context,
            &cleanup_test_session(replacement_session_id, replacement_incarnation),
        )
        .unwrap();
        fs::remove_file(&abandoned_path).unwrap();
        let replacement_bytes = progress_bytes(replacement_session_id, replacement_incarnation);
        fs::write(&abandoned_path, &replacement_bytes).unwrap();
        fs::set_permissions(&abandoned_path, fs::Permissions::from_mode(0o600)).unwrap();
        let replacement_metadata = fs::symlink_metadata(&abandoned_path).unwrap();
        resume.wait();

        let error = writer
            .join()
            .unwrap()
            .expect_err("a replacement must not be admitted by deleting the new path identity");
        assert_eq!(error.code(), "group-cleanup-progress-capacity");
        assert_eq!(
            fs::read(&abandoned_path).unwrap(),
            replacement_bytes,
            "eviction must remain bound to the stale descriptor snapshot"
        );
        let replacement_after = fs::symlink_metadata(&abandoned_path).unwrap();
        assert_eq!(
            (replacement_after.dev(), replacement_after.ino()),
            (replacement_metadata.dev(), replacement_metadata.ino()),
            "the exact raced replacement inode must remain at its original key"
        );
        assert_eq!(fs::read_dir(&progress_dir).unwrap().count(), 128);
        assert!(
            orchestration::group_cleanup_progress_recycle_slot_is_retired_for_test(&context),
            "a restored replacement must leave the bounded recycle slot retired"
        );
    }

    #[test]
    fn group_cleanup_progress_reconcile_preserves_replacement_after_exchange() {
        let _fixture_ownership = GlobalStateLock::new();
        let tmp = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let (progress_dir, source_path, incoming) =
            group_cleanup_progress_race_fixture(&context, "post-exchange-replacement");
        let (swapped, resume, _) =
            orchestration::install_group_cleanup_progress_recycle_hook_for_test(&context);
        let writer_context = context.clone();
        let writer = thread::spawn(move || {
            orchestration::store_group_cleanup_progress(&writer_context, &"f".repeat(64), &incoming)
        });
        swapped.wait();

        let replacement_session_id = "post-exchange-live-main";
        let replacement_incarnation = "post-exchange-live-incarnation";
        crate::write_session_record(
            &context,
            &cleanup_test_session(replacement_session_id, replacement_incarnation),
        )
        .unwrap();
        let mut replacement =
            serde_json::from_slice::<Value>(&fs::read(&source_path).unwrap()).unwrap();
        replacement["principal_session_id"] = json!(replacement_session_id);
        replacement["principal_incarnation"] = json!(replacement_incarnation);
        let replacement_bytes = serde_json::to_vec(&replacement).unwrap();
        fs::remove_file(&source_path).unwrap();
        fs::write(&source_path, &replacement_bytes).unwrap();
        fs::set_permissions(&source_path, fs::Permissions::from_mode(0o600)).unwrap();
        let replacement_metadata = fs::symlink_metadata(&source_path).unwrap();
        resume.wait();

        let error = writer
            .join()
            .unwrap()
            .expect_err("post-exchange replacement must abort admission without displacement");
        assert_eq!(error.code(), "group-cleanup-progress-capacity");
        assert_eq!(fs::read(&source_path).unwrap(), replacement_bytes);
        let replacement_after = fs::symlink_metadata(&source_path).unwrap();
        assert_eq!(
            (replacement_after.dev(), replacement_after.ino()),
            (replacement_metadata.dev(), replacement_metadata.ino())
        );
        assert!(!progress_dir.join("f".repeat(64)).exists());
        assert!(orchestration::group_cleanup_progress_recycle_slot_is_retired_for_test(&context));
    }

    #[test]
    fn group_cleanup_progress_post_verify_race_restores_source_replacement() {
        let _fixture_ownership = GlobalStateLock::new();
        let tmp = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let (progress_dir, source_path, incoming) =
            group_cleanup_progress_race_fixture(&context, "post-verify-replacement");
        let current_path = progress_dir.join("f".repeat(64));
        let (ready, resume) =
            orchestration::install_group_cleanup_progress_post_verify_hook_for_test(&source_path);
        let writer_context = context.clone();
        let writer = thread::spawn(move || {
            orchestration::store_group_cleanup_progress(&writer_context, &"f".repeat(64), &incoming)
        });
        ready.wait();

        let replacement_session_id = "post-verify-live-main";
        let replacement_incarnation = "post-verify-live-incarnation";
        crate::write_session_record(
            &context,
            &cleanup_test_session(replacement_session_id, replacement_incarnation),
        )
        .unwrap();
        let mut replacement =
            serde_json::from_slice::<Value>(&fs::read(&source_path).unwrap()).unwrap();
        replacement["principal_session_id"] = json!(replacement_session_id);
        replacement["principal_incarnation"] = json!(replacement_incarnation);
        let replacement_bytes = serde_json::to_vec(&replacement).unwrap();
        fs::remove_file(&source_path).unwrap();
        fs::write(&source_path, &replacement_bytes).unwrap();
        fs::set_permissions(&source_path, fs::Permissions::from_mode(0o600)).unwrap();
        let replacement_metadata = fs::symlink_metadata(&source_path).unwrap();
        resume.wait();

        let error = writer
            .join()
            .unwrap()
            .expect_err("a source replacement must not be moved to the incoming key");
        assert_eq!(error.code(), "group-cleanup-progress-capacity");
        assert_eq!(fs::read(&source_path).unwrap(), replacement_bytes);
        let replacement_after = fs::symlink_metadata(&source_path).unwrap();
        assert_eq!(
            (replacement_after.dev(), replacement_after.ino()),
            (replacement_metadata.dev(), replacement_metadata.ino()),
            "compensation must restore the exact post-verify replacement inode to its source key"
        );
        assert!(
            !current_path.exists(),
            "the replacement must not be installed at the incoming key"
        );
        assert!(orchestration::group_cleanup_progress_recycle_slot_is_retired_for_test(&context));
    }

    #[test]
    fn group_cleanup_progress_final_rename_preserves_replacement_before_verification() {
        let _fixture_ownership = GlobalStateLock::new();
        let tmp = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let (progress_dir, source_path, incoming) =
            group_cleanup_progress_race_fixture(&context, "post-final-rename-replacement");
        let current_path = progress_dir.join("f".repeat(64));
        let (renamed, resume) =
            orchestration::install_group_cleanup_progress_final_rename_hook_for_test(&current_path);
        let writer_context = context.clone();
        let writer = thread::spawn(move || {
            orchestration::store_group_cleanup_progress(&writer_context, &"f".repeat(64), &incoming)
        });
        renamed.wait();

        let replacement_session_id = "post-rename-live-main";
        let replacement_incarnation = "post-rename-live-incarnation";
        crate::write_session_record(
            &context,
            &cleanup_test_session(replacement_session_id, replacement_incarnation),
        )
        .unwrap();
        let mut replacement =
            serde_json::from_slice::<Value>(&fs::read(&current_path).unwrap()).unwrap();
        replacement["principal_session_id"] = json!(replacement_session_id);
        replacement["principal_incarnation"] = json!(replacement_incarnation);
        let replacement_bytes = serde_json::to_vec(&replacement).unwrap();
        fs::remove_file(&current_path).unwrap();
        fs::write(&current_path, &replacement_bytes).unwrap();
        fs::set_permissions(&current_path, fs::Permissions::from_mode(0o600)).unwrap();
        let replacement_metadata = fs::symlink_metadata(&current_path).unwrap();
        resume.wait();

        let error = writer
            .join()
            .unwrap()
            .expect_err("post-rename replacement must abort admission without displacement");
        assert_eq!(error.code(), "group-cleanup-progress-capacity");
        assert!(
            !current_path.exists(),
            "an unproven destination identity must be compensated back to the source key"
        );
        assert_eq!(fs::read(&source_path).unwrap(), replacement_bytes);
        let replacement_after = fs::symlink_metadata(&source_path).unwrap();
        assert_eq!(
            (replacement_after.dev(), replacement_after.ino()),
            (replacement_metadata.dev(), replacement_metadata.ino())
        );
        assert!(
            orchestration::group_cleanup_progress_recycle_slot_is_retired_for_test(&context),
            "successful exact compensation must retire the transaction"
        );
    }

    #[test]
    fn group_cleanup_progress_recovery_uses_durable_final_install_proof() {
        let _fixture_ownership = GlobalStateLock::new();
        for destination_state in ["replaced", "removed"] {
            let tmp = tempfile::TempDir::new().unwrap();
            let context = CliContext {
                state_dir: tmp.path().join("state"),
                host: None,
            };
            let (progress_dir, _, incoming) = group_cleanup_progress_race_fixture(
                &context,
                &format!("post-install-{destination_state}"),
            );
            let current_path = progress_dir.join("f".repeat(64));
            let (installed, resume) =
                orchestration::install_group_cleanup_progress_post_install_hook_for_test(
                    &current_path,
                );
            orchestration::fail_group_cleanup_progress_after_final_install_for_test(&current_path);
            let writer_context = context.clone();
            let writer = thread::spawn(move || {
                orchestration::store_group_cleanup_progress(
                    &writer_context,
                    &"f".repeat(64),
                    &incoming,
                )
            });
            installed.wait();

            let replacement = if destination_state == "replaced" {
                let replacement_session_id = "post-install-live-main";
                let replacement_incarnation = "post-install-live-incarnation";
                crate::write_session_record(
                    &context,
                    &cleanup_test_session(replacement_session_id, replacement_incarnation),
                )
                .unwrap();
                let mut replacement =
                    serde_json::from_slice::<Value>(&fs::read(&current_path).unwrap()).unwrap();
                replacement["principal_session_id"] = json!(replacement_session_id);
                replacement["principal_incarnation"] = json!(replacement_incarnation);
                let bytes = serde_json::to_vec(&replacement).unwrap();
                fs::remove_file(&current_path).unwrap();
                fs::write(&current_path, &bytes).unwrap();
                fs::set_permissions(&current_path, fs::Permissions::from_mode(0o600)).unwrap();
                let metadata = fs::symlink_metadata(&current_path).unwrap();
                Some((bytes, metadata.dev(), metadata.ino()))
            } else {
                fs::remove_file(&current_path).unwrap();
                None
            };
            resume.wait();

            let error = writer
                .join()
                .unwrap()
                .expect_err("the post-install crash must leave durable recovery work");
            assert_eq!(error.code(), "orchestration-store-unavailable");
            assert_eq!(
                orchestration::recover_group_cleanup_progress_principal(
                    &context,
                    "unrelated-selector",
                    "unrelated-incarnation",
                    "unrelated-idempotency",
                    &"a".repeat(64),
                )
                .unwrap(),
                None,
                "durable installed-phase evidence must let recovery converge"
            );
            if let Some((bytes, device, inode)) = replacement {
                assert_eq!(fs::read(&current_path).unwrap(), bytes);
                let metadata = fs::symlink_metadata(&current_path).unwrap();
                assert_eq!(
                    (metadata.dev(), metadata.ino()),
                    (device, inode),
                    "post-install recovery must retain the exact destination replacement"
                );
            } else {
                assert!(
                    !current_path.exists(),
                    "post-install recovery must respect destination removal"
                );
            }
            assert!(
                orchestration::group_cleanup_progress_recycle_slot_is_retired_for_test(&context)
            );
        }
    }

    #[test]
    fn group_cleanup_progress_recovery_compensates_unproven_final_rename() {
        let _fixture_ownership = GlobalStateLock::new();
        for destination_state in ["replaced", "removed"] {
            let tmp = tempfile::TempDir::new().unwrap();
            let context = CliContext {
                state_dir: tmp.path().join("state"),
                host: None,
            };
            let (progress_dir, source_path, incoming) = group_cleanup_progress_race_fixture(
                &context,
                &format!("unproven-final-{destination_state}"),
            );
            let current_path = progress_dir.join("f".repeat(64));
            let (renamed, resume) =
                orchestration::install_group_cleanup_progress_final_rename_hook_for_test(
                    &current_path,
                );
            orchestration::fail_group_cleanup_progress_after_final_rename_for_test(&current_path);
            let writer_context = context.clone();
            let writer = thread::spawn(move || {
                orchestration::store_group_cleanup_progress(
                    &writer_context,
                    &"f".repeat(64),
                    &incoming,
                )
            });
            renamed.wait();

            let replacement = if destination_state == "replaced" {
                let replacement_session_id = "unproven-final-live-main";
                let replacement_incarnation = "unproven-final-live-incarnation";
                crate::write_session_record(
                    &context,
                    &cleanup_test_session(replacement_session_id, replacement_incarnation),
                )
                .unwrap();
                let mut replacement =
                    serde_json::from_slice::<Value>(&fs::read(&current_path).unwrap()).unwrap();
                replacement["principal_session_id"] = json!(replacement_session_id);
                replacement["principal_incarnation"] = json!(replacement_incarnation);
                let bytes = serde_json::to_vec(&replacement).unwrap();
                fs::remove_file(&current_path).unwrap();
                fs::write(&current_path, &bytes).unwrap();
                fs::set_permissions(&current_path, fs::Permissions::from_mode(0o600)).unwrap();
                let metadata = fs::symlink_metadata(&current_path).unwrap();
                Some((bytes, metadata.dev(), metadata.ino()))
            } else {
                fs::remove_file(&current_path).unwrap();
                None
            };
            resume.wait();

            let error = writer
                .join()
                .unwrap()
                .expect_err("the pre-proof crash must leave a prepared-phase transaction");
            assert_eq!(error.code(), "orchestration-store-unavailable");
            assert_eq!(
                orchestration::recover_group_cleanup_progress_principal(
                    &context,
                    "unrelated-selector",
                    "unrelated-incarnation",
                    "unrelated-idempotency",
                    &"b".repeat(64),
                )
                .unwrap(),
                None
            );
            assert!(
                !current_path.exists(),
                "prepared-phase recovery must not admit an unproven destination"
            );
            if let Some((bytes, device, inode)) = replacement {
                assert_eq!(fs::read(&source_path).unwrap(), bytes);
                let metadata = fs::symlink_metadata(&source_path).unwrap();
                assert_eq!(
                    (metadata.dev(), metadata.ino()),
                    (device, inode),
                    "prepared-phase recovery must restore the exact displaced inode"
                );
            } else {
                assert!(
                    !source_path.exists(),
                    "destination removal must converge without manufacturing a source"
                );
            }
            assert!(
                orchestration::group_cleanup_progress_recycle_slot_is_retired_for_test(&context)
            );
        }
    }

    #[test]
    fn group_cleanup_progress_recycle_recovers_a_post_swap_replacement_after_crash() {
        let _fixture_ownership = GlobalStateLock::new();
        let tmp = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        fs::create_dir(&context.state_dir).unwrap();
        let progress_bytes = |principal_session_id: &str, principal_incarnation: &str| {
            serde_json::to_vec(&GroupCleanupProgressReceipt {
                schema_version: orchestration::GROUP_CLEANUP_PROGRESS_RECEIPT_V1_SCHEMA.to_string(),
                requested_session_id: None,
                principal_session_id: principal_session_id.to_string(),
                principal_incarnation: principal_incarnation.to_string(),
                idempotency_key: "retention-post-swap-race".to_string(),
                request_digest: "1".repeat(64),
                outcome: json!({
                    "schema_version": GROUP_CLEANUP_RESULT_SCHEMA,
                    "completed": false,
                    "workers": []
                }),
            })
            .unwrap()
        };
        let abandoned_key = format!("{:064x}", 0);
        orchestration::store_group_cleanup_progress(
            &context,
            &abandoned_key,
            &progress_bytes("abandoned-main", "abandoned-incarnation"),
        )
        .unwrap();
        let progress_dir = context
            .state_dir
            .join("orchestration/group-cleanup-progress");
        let abandoned_path = progress_dir.join(&abandoned_key);
        fs::File::options()
            .write(true)
            .open(&abandoned_path)
            .unwrap()
            .set_times(
                fs::FileTimes::new().set_modified(std::time::UNIX_EPOCH + Duration::from_secs(1)),
            )
            .unwrap();
        for index in 1..128 {
            let session_id = format!("live-main-{index}");
            let incarnation = format!("live-incarnation-{index}");
            crate::write_session_record(&context, &cleanup_test_session(&session_id, &incarnation))
                .unwrap();
            let path = progress_dir.join(format!("{index:064x}"));
            fs::write(&path, progress_bytes(&session_id, &incarnation)).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        }

        let (swapped, resume, recycle_path) =
            orchestration::install_group_cleanup_progress_recycle_hook_for_test(&context);
        orchestration::fail_group_cleanup_progress_after_recycle_for_test(&context);
        let writer_context = context.clone();
        let incoming = progress_bytes("new-main", "new-incarnation");
        let retry_incoming = incoming.clone();
        let writer = thread::spawn(move || {
            orchestration::store_group_cleanup_progress(&writer_context, &"f".repeat(64), &incoming)
        });
        swapped.wait();
        resume.wait();

        let crash_error = writer
            .join()
            .unwrap()
            .expect_err("the injected post-exchange crash must preserve its active journal");
        assert_eq!(crash_error.code(), "orchestration-store-unavailable");
        let (recovery_ready, recovery_resume) =
            orchestration::install_group_cleanup_progress_recovery_exchange_hook_for_test(
                &abandoned_path,
            );
        let retry_context = context.clone();
        let retry = thread::spawn(move || {
            orchestration::store_group_cleanup_progress(
                &retry_context,
                &"f".repeat(64),
                &retry_incoming,
            )
        });
        recovery_ready.wait();
        let replacement_session_id = "replacement-live-main";
        let replacement_incarnation = "replacement-live-incarnation";
        crate::write_session_record(
            &context,
            &cleanup_test_session(replacement_session_id, replacement_incarnation),
        )
        .unwrap();
        fs::remove_file(&abandoned_path).unwrap();
        let replacement_bytes = progress_bytes(replacement_session_id, replacement_incarnation);
        fs::write(&abandoned_path, &replacement_bytes).unwrap();
        fs::set_permissions(&abandoned_path, fs::Permissions::from_mode(0o600)).unwrap();
        let replacement_metadata = fs::symlink_metadata(&abandoned_path).unwrap();
        recovery_resume.wait();

        let error = retry
            .join()
            .unwrap()
            .expect_err("recovery must restore the live replacement and fail capacity closed");
        assert_eq!(error.code(), "group-cleanup-progress-capacity");
        assert_eq!(
            fs::read(&abandoned_path).unwrap(),
            replacement_bytes,
            "rollback must preserve the replacement rather than unlinking it"
        );
        let replacement_after = fs::symlink_metadata(&abandoned_path).unwrap();
        assert_eq!(
            (replacement_after.dev(), replacement_after.ino()),
            (replacement_metadata.dev(), replacement_metadata.ino()),
            "recovery must restore the exact replacement inode after a raced exchange"
        );
        assert_eq!(fs::read_dir(&progress_dir).unwrap().count(), 128);
        assert!(
            orchestration::group_cleanup_progress_recycle_slot_is_retired_for_test(&context),
            "post-swap rollback must leave the recycle slot reusable"
        );
        assert!(
            recycle_path.exists(),
            "the bounded recycle slot must remain available after recovery"
        );
    }

    #[test]
    fn group_cleanup_alias_recovery_reconciles_post_exchange_residue_before_scanning() {
        let _fixture_ownership = GlobalStateLock::new();
        let tmp = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let (progress_dir, source_path, _) =
            group_cleanup_progress_race_fixture(&context, "alias-post-exchange-residue");
        let source_before = fs::read(&source_path).unwrap();
        let source_metadata_before = fs::symlink_metadata(&source_path).unwrap();
        let requested_alias = "main-c";
        let canonical = "main-controller-unique";
        let incarnation = "alias-post-exchange-incarnation";
        let idempotency_key = "alias-post-exchange-residue";
        let request_digest = "4".repeat(64);
        let incoming = serde_json::to_vec(&GroupCleanupProgressReceipt {
            schema_version: orchestration::GROUP_CLEANUP_PROGRESS_RECEIPT_SCHEMA.to_string(),
            requested_session_id: Some(requested_alias.to_string()),
            principal_session_id: canonical.to_string(),
            principal_incarnation: incarnation.to_string(),
            idempotency_key: idempotency_key.to_string(),
            request_digest: request_digest.clone(),
            outcome: json!({
                "schema_version": GROUP_CLEANUP_RESULT_SCHEMA,
                "completed": false,
                "_resume": {
                    "plan": {
                        "main": {
                            "session_id": canonical,
                            "session_incarnation": incarnation
                        }
                    },
                    "pending_registry_fences": [{
                        "session_id": canonical,
                        "runtime_launch_id": incarnation
                    }]
                }
            }),
        })
        .unwrap();

        orchestration::fail_group_cleanup_progress_after_recycle_for_test(&context);
        let crash_error =
            orchestration::store_group_cleanup_progress(&context, &"f".repeat(64), &incoming)
                .expect_err("the fixture must leave an active post-exchange transaction");
        assert_eq!(crash_error.code(), "orchestration-store-unavailable");
        assert_ne!(
            fs::read(&source_path).unwrap(),
            source_before,
            "the crash point must expose the not-yet-admitted prepared receipt at the stale key"
        );

        assert_eq!(
            orchestration::recover_group_cleanup_progress_principal(
                &context,
                requested_alias,
                incarnation,
                idempotency_key,
                &request_digest,
            )
            .unwrap(),
            None,
            "alias recovery must reconcile the active journal before considering receipt bodies"
        );
        assert_eq!(fs::read(&source_path).unwrap(), source_before);
        let source_metadata_after = fs::symlink_metadata(&source_path).unwrap();
        assert_eq!(
            (source_metadata_after.dev(), source_metadata_after.ino()),
            (source_metadata_before.dev(), source_metadata_before.ino()),
            "recovery must restore the exact stale inode to its origin key"
        );
        assert!(!progress_dir.join("f".repeat(64)).exists());
        assert!(orchestration::group_cleanup_progress_recycle_slot_is_retired_for_test(&context));
    }

    #[test]
    fn group_cleanup_progress_recycle_recovers_durable_residue() {
        let _fixture_ownership = GlobalStateLock::new();
        let tmp = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        fs::create_dir(&context.state_dir).unwrap();
        let progress_bytes = |principal_session_id: &str, principal_incarnation: &str| {
            serde_json::to_vec(&GroupCleanupProgressReceipt {
                schema_version: orchestration::GROUP_CLEANUP_PROGRESS_RECEIPT_V1_SCHEMA.to_string(),
                requested_session_id: None,
                principal_session_id: principal_session_id.to_string(),
                principal_incarnation: principal_incarnation.to_string(),
                idempotency_key: "retention-residue-recovery".to_string(),
                request_digest: "2".repeat(64),
                outcome: json!({
                    "schema_version": GROUP_CLEANUP_RESULT_SCHEMA,
                    "completed": false,
                    "workers": []
                }),
            })
            .unwrap()
        };
        let abandoned_key = format!("{:064x}", 0);
        orchestration::store_group_cleanup_progress(
            &context,
            &abandoned_key,
            &progress_bytes("abandoned-main", "abandoned-incarnation"),
        )
        .unwrap();
        let progress_dir = context
            .state_dir
            .join("orchestration/group-cleanup-progress");
        let abandoned_path = progress_dir.join(&abandoned_key);
        fs::File::options()
            .write(true)
            .open(&abandoned_path)
            .unwrap()
            .set_times(
                fs::FileTimes::new().set_modified(std::time::UNIX_EPOCH + Duration::from_secs(1)),
            )
            .unwrap();
        for index in 1..128 {
            let session_id = format!("live-main-{index}");
            let incarnation = format!("live-incarnation-{index}");
            crate::write_session_record(&context, &cleanup_test_session(&session_id, &incarnation))
                .unwrap();
            let path = progress_dir.join(format!("{index:064x}"));
            fs::write(&path, progress_bytes(&session_id, &incarnation)).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        orchestration::seed_group_cleanup_progress_recycle_residue_for_test(&context, b"{")
            .unwrap();

        let incoming_key = "f".repeat(64);
        let incoming = progress_bytes("new-main", "new-incarnation");
        let abandoned_before = fs::read(&abandoned_path).unwrap();
        let abandoned_metadata_before = fs::symlink_metadata(&abandoned_path).unwrap();
        orchestration::fail_group_cleanup_progress_journal_sync_for_test(&context);
        let durability_error =
            orchestration::store_group_cleanup_progress(&context, &incoming_key, &incoming)
                .expect_err("exchange must not start without a durable active journal");
        assert_eq!(durability_error.code(), "orchestration-store-unavailable");
        assert_eq!(fs::read(&abandoned_path).unwrap(), abandoned_before);
        let abandoned_metadata_after = fs::symlink_metadata(&abandoned_path).unwrap();
        assert_eq!(
            (
                abandoned_metadata_after.dev(),
                abandoned_metadata_after.ino()
            ),
            (
                abandoned_metadata_before.dev(),
                abandoned_metadata_before.ino()
            ),
            "the exchange must not run before the active journal directory is durable"
        );
        assert!(!progress_dir.join(&incoming_key).exists());

        orchestration::fail_group_cleanup_progress_directory_sync_for_test(&context);
        let progress_sync_error =
            orchestration::store_group_cleanup_progress(&context, &incoming_key, &incoming)
                .expect_err("the journal must remain active until progress namespace sync");
        assert_eq!(
            progress_sync_error.code(),
            "orchestration-store-unavailable"
        );
        orchestration::store_group_cleanup_progress(&context, &incoming_key, &incoming).unwrap();

        assert_eq!(
            fs::read(progress_dir.join(&incoming_key)).unwrap(),
            incoming
        );
        assert!(!abandoned_path.exists());
        assert_eq!(fs::read_dir(&progress_dir).unwrap().count(), 128);
        assert!(
            orchestration::group_cleanup_progress_recycle_slot_is_retired_for_test(&context),
            "a crash residue must converge to the bounded retired slot"
        );
    }

    #[test]
    fn group_cleanup_progress_journal_dirfd_rejects_parent_replacement() {
        let _fixture_ownership = GlobalStateLock::new();
        let tmp = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        fs::create_dir(&context.state_dir).unwrap();
        let (renamed, resume, recycle_parent) =
            orchestration::install_group_cleanup_progress_journal_hook_for_test(&context);
        let writer_context = context.clone();
        let writer = thread::spawn(move || {
            orchestration::store_idle_group_cleanup_progress_recycle_journal_for_test(
                &writer_context,
            )
        });
        renamed.wait();
        let moved_parent = recycle_parent.with_file_name("group-cleanup-progress-recycle-moved");
        fs::rename(&recycle_parent, &moved_parent).unwrap();
        fs::create_dir(&recycle_parent).unwrap();
        fs::set_permissions(&recycle_parent, fs::Permissions::from_mode(0o700)).unwrap();
        resume.wait();

        let error = writer
            .join()
            .unwrap()
            .expect_err("a replacement directory must not satisfy the durability barrier");
        assert_eq!(error.code(), "orchestration-store-unavailable");
        assert!(
            moved_parent.join("journal.json").exists(),
            "the journal remains in the exact directory pinned by the write"
        );
        assert!(
            !recycle_parent.join("journal.json").exists(),
            "the replacement directory must never be mistaken for the synced journal parent"
        );
    }

    #[test]
    fn group_cleanup_progress_exchange_keeps_the_pinned_recycle_parent() {
        let _fixture_ownership = GlobalStateLock::new();
        let tmp = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        fs::create_dir(&context.state_dir).unwrap();
        let progress_bytes = |principal_session_id: &str| {
            serde_json::to_vec(&GroupCleanupProgressReceipt {
                schema_version: orchestration::GROUP_CLEANUP_PROGRESS_RECEIPT_V1_SCHEMA.to_string(),
                requested_session_id: None,
                principal_session_id: principal_session_id.to_string(),
                principal_incarnation: "pinned-recycle-incarnation".to_string(),
                idempotency_key: "pinned-recycle-parent".to_string(),
                request_digest: "5".repeat(64),
                outcome: json!({
                    "schema_version": GROUP_CLEANUP_RESULT_SCHEMA,
                    "completed": false
                }),
            })
            .unwrap()
        };
        let source_key = format!("{:064x}", 0);
        orchestration::store_group_cleanup_progress(
            &context,
            &source_key,
            &progress_bytes("abandoned-main-0"),
        )
        .unwrap();
        let progress_dir = context
            .state_dir
            .join("orchestration/group-cleanup-progress");
        let source_path = progress_dir.join(&source_key);
        fs::File::options()
            .write(true)
            .open(&source_path)
            .unwrap()
            .set_times(
                fs::FileTimes::new().set_modified(std::time::UNIX_EPOCH + Duration::from_secs(1)),
            )
            .unwrap();
        for index in 1..128 {
            let path = progress_dir.join(format!("{index:064x}"));
            fs::write(&path, progress_bytes(&format!("abandoned-main-{index}"))).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        let source_before = fs::read(&source_path).unwrap();
        let source_metadata_before = fs::symlink_metadata(&source_path).unwrap();
        let (ready, resume, recycle_parent) =
            orchestration::install_group_cleanup_progress_pre_exchange_hook_for_test(&context);
        let writer_context = context.clone();
        let incoming_key = "f".repeat(64);
        let incoming = progress_bytes("new-main");
        let writer = thread::spawn(move || {
            orchestration::store_group_cleanup_progress(&writer_context, &incoming_key, &incoming)
        });
        ready.wait();
        let moved_parent =
            recycle_parent.with_file_name("group-cleanup-progress-recycle-post-active");
        fs::rename(&recycle_parent, &moved_parent).unwrap();
        fs::create_dir(&recycle_parent).unwrap();
        fs::set_permissions(&recycle_parent, fs::Permissions::from_mode(0o700)).unwrap();
        let replacement_slot = recycle_parent.join("slot");
        fs::write(&replacement_slot, b"replacement-slot").unwrap();
        fs::set_permissions(&replacement_slot, fs::Permissions::from_mode(0o600)).unwrap();
        resume.wait();

        let error = writer
            .join()
            .unwrap()
            .expect_err("post-journal parent replacement must abort before exchange");
        assert_eq!(error.code(), "orchestration-store-unavailable");
        assert_eq!(fs::read(&source_path).unwrap(), source_before);
        let source_metadata_after = fs::symlink_metadata(&source_path).unwrap();
        assert_eq!(
            (source_metadata_after.dev(), source_metadata_after.ino()),
            (source_metadata_before.dev(), source_metadata_before.ino())
        );
        assert_eq!(fs::read(&replacement_slot).unwrap(), b"replacement-slot");
        assert!(!progress_dir.join("f".repeat(64)).exists());
        assert!(moved_parent.join("journal.json").exists());
    }

    #[test]
    fn group_cleanup_progress_recycle_compacts_bytes_without_path_deletion() {
        let _fixture_ownership = GlobalStateLock::new();
        let tmp = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        fs::create_dir(&context.state_dir).unwrap();
        let progress_bytes = |principal_session_id: &str, padding: usize| {
            serde_json::to_vec(&GroupCleanupProgressReceipt {
                schema_version: orchestration::GROUP_CLEANUP_PROGRESS_RECEIPT_V1_SCHEMA.to_string(),
                requested_session_id: None,
                principal_session_id: principal_session_id.to_string(),
                principal_incarnation: "retention-byte-incarnation".to_string(),
                idempotency_key: "retention-byte-compaction".to_string(),
                request_digest: "3".repeat(64),
                outcome: json!({
                    "schema_version": GROUP_CLEANUP_RESULT_SCHEMA,
                    "completed": false,
                    "padding": "x".repeat(padding)
                }),
            })
            .unwrap()
        };
        let current_key = "f".repeat(64);
        orchestration::store_group_cleanup_progress(
            &context,
            &current_key,
            &progress_bytes("current-main", 0),
        )
        .unwrap();
        let progress_dir = context
            .state_dir
            .join("orchestration/group-cleanup-progress");
        let stale_padding = 1024 * 1024 - 2048;
        let mut large_paths = Vec::new();
        let stale_bytes = progress_bytes("abandoned-main", stale_padding);
        for index in 0..15 {
            let path = progress_dir.join(format!("{index:064x}"));
            fs::write(&path, &stale_bytes).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
            large_paths.push(path);
        }

        let incoming = progress_bytes("current-main", 4 * 1024 * 1024 - 2048);
        let retired_len = serde_json::to_vec(&GroupCleanupProgressReceipt {
            schema_version: orchestration::GROUP_CLEANUP_PROGRESS_RECEIPT_V1_SCHEMA.to_string(),
            requested_session_id: None,
            principal_session_id: "retired-progress".to_string(),
            principal_incarnation: "retired-progress".to_string(),
            idempotency_key: "retired-progress".to_string(),
            request_digest: "0".repeat(64),
            outcome: json!({}),
        })
        .unwrap()
        .len() as u64;
        let projected_bytes =
            large_paths.len() as u64 * stale_bytes.len() as u64 + incoming.len() as u64;
        let reclaim_per_compaction = stale_bytes.len() as u64 - retired_len;
        let expected_compactions = projected_bytes
            .saturating_sub(16 * 1024 * 1024)
            .div_ceil(reclaim_per_compaction) as usize;
        assert!(
            expected_compactions >= 2,
            "fixture must require multiple stale compactions"
        );
        orchestration::fail_group_cleanup_progress_directory_sync_for_test(&context);
        let sync_error =
            orchestration::store_group_cleanup_progress(&context, &current_key, &incoming)
                .expect_err("byte compaction must sync the progress namespace before idle");
        assert_eq!(sync_error.code(), "orchestration-store-unavailable");
        orchestration::reset_group_cleanup_progress_compactions_for_test();
        orchestration::store_group_cleanup_progress(&context, &current_key, &incoming).unwrap();

        assert_eq!(fs::read(progress_dir.join(current_key)).unwrap(), incoming);
        assert_eq!(fs::read_dir(&progress_dir).unwrap().count(), 16);
        let compacted = large_paths
            .iter()
            .filter(|path| {
                fs::read(path).is_ok_and(|bytes| {
                    serde_json::from_slice::<Value>(&bytes).is_ok_and(|receipt| {
                        receipt["principal_session_id"].as_str() == Some("retired-progress")
                    })
                })
            })
            .count();
        assert_eq!(
            compacted, expected_compactions,
            "successful admission must use the exact planned stale compaction count"
        );
        assert_eq!(
            orchestration::group_cleanup_progress_compactions_for_test() as usize,
            expected_compactions,
            "the retry must execute only the minimum sufficient compaction prefix"
        );
        assert!(orchestration::group_cleanup_progress_recycle_slot_is_retired_for_test(&context));
    }

    #[test]
    fn group_cleanup_progress_byte_compaction_recovers_when_source_disappears_before_exchange() {
        let _fixture_ownership = GlobalStateLock::new();
        let tmp = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let (source_path, current_key, incoming) = group_cleanup_progress_byte_pressure_fixture(
            &context,
            "byte-source-missing-before-exchange",
        );
        let (ready, resume, _) =
            orchestration::install_group_cleanup_progress_pre_exchange_hook_for_test(&context);
        let writer_context = context.clone();
        let writer_key = current_key.clone();
        let writer_incoming = incoming.clone();
        let writer = thread::spawn(move || {
            orchestration::store_group_cleanup_progress(
                &writer_context,
                &writer_key,
                &writer_incoming,
            )
        });
        ready.wait();
        fs::remove_file(&source_path).unwrap();
        resume.wait();

        let error = writer
            .join()
            .unwrap()
            .expect_err("a vanished pre-exchange source must abort the planned compaction");
        assert_eq!(error.code(), "group-cleanup-progress-capacity");
        orchestration::store_group_cleanup_progress(&context, &current_key, &incoming).unwrap();
        assert!(
            !source_path.exists(),
            "recovery must not recreate a source deliberately removed before exchange"
        );
        assert!(orchestration::group_cleanup_progress_recycle_slot_is_retired_for_test(&context));
    }

    #[test]
    fn group_cleanup_progress_byte_compaction_recovers_when_source_disappears_after_exchange() {
        let _fixture_ownership = GlobalStateLock::new();
        let tmp = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let (source_path, current_key, incoming) = group_cleanup_progress_byte_pressure_fixture(
            &context,
            "byte-source-missing-after-exchange",
        );
        let (swapped, resume, _) =
            orchestration::install_group_cleanup_progress_recycle_hook_for_test(&context);
        orchestration::fail_group_cleanup_progress_after_recycle_for_test(&context);
        let writer_context = context.clone();
        let writer_key = current_key.clone();
        let writer_incoming = incoming.clone();
        let writer = thread::spawn(move || {
            orchestration::store_group_cleanup_progress(
                &writer_context,
                &writer_key,
                &writer_incoming,
            )
        });
        swapped.wait();
        fs::remove_file(&source_path).unwrap();
        resume.wait();

        let error = writer
            .join()
            .unwrap()
            .expect_err("the injected crash must preserve the active post-exchange journal");
        assert_eq!(error.code(), "orchestration-store-unavailable");
        orchestration::store_group_cleanup_progress(&context, &current_key, &incoming).unwrap();
        assert!(
            !source_path.exists(),
            "recovery must not recreate a source removed after the exact exchange"
        );
        assert!(orchestration::group_cleanup_progress_recycle_slot_is_retired_for_test(&context));
    }

    #[test]
    fn group_cleanup_progress_capacity_preflight_does_not_compact_when_reclaim_is_insufficient() {
        let _fixture_ownership = GlobalStateLock::new();
        let tmp = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        fs::create_dir(&context.state_dir).unwrap();
        let progress_bytes = |principal_session_id: &str,
                              principal_incarnation: &str,
                              padding: usize| {
            serde_json::to_vec(&GroupCleanupProgressReceipt {
                schema_version: orchestration::GROUP_CLEANUP_PROGRESS_RECEIPT_V1_SCHEMA.to_string(),
                requested_session_id: None,
                principal_session_id: principal_session_id.to_string(),
                principal_incarnation: principal_incarnation.to_string(),
                idempotency_key: "capacity-preflight".to_string(),
                request_digest: "8".repeat(64),
                outcome: json!({
                    "schema_version": GROUP_CLEANUP_RESULT_SCHEMA,
                    "completed": false,
                    "padding": "x".repeat(padding),
                }),
            })
            .unwrap()
        };
        let stale_key = format!("{:064x}", 0);
        orchestration::store_group_cleanup_progress(
            &context,
            &stale_key,
            &progress_bytes("stale-main", "stale-incarnation", 0),
        )
        .unwrap();
        let progress_dir = context
            .state_dir
            .join("orchestration/group-cleanup-progress");
        let stale_path = progress_dir.join(&stale_key);
        fs::File::options()
            .write(true)
            .open(&stale_path)
            .unwrap()
            .set_times(
                fs::FileTimes::new().set_modified(std::time::UNIX_EPOCH + Duration::from_secs(1)),
            )
            .unwrap();
        for index in 1..=4 {
            let session_id = format!("live-capacity-main-{index}");
            let incarnation = format!("live-capacity-incarnation-{index}");
            crate::write_session_record(&context, &cleanup_test_session(&session_id, &incarnation))
                .unwrap();
            orchestration::store_group_cleanup_progress(
                &context,
                &format!("{index:064x}"),
                &progress_bytes(&session_id, &incarnation, 3_200_000),
            )
            .unwrap();
        }

        orchestration::reset_group_cleanup_progress_compactions_for_test();
        let error = orchestration::store_group_cleanup_progress(
            &context,
            &"f".repeat(64),
            &progress_bytes("incoming-main", "incoming-incarnation", 4_000_000),
        )
        .expect_err("insufficient total reclaim must fail before durable compaction");
        assert_eq!(error.code(), "group-cleanup-progress-capacity");
        assert_eq!(
            orchestration::group_cleanup_progress_compactions_for_test(),
            0,
            "capacity preflight must not start sync-heavy compaction when admission is impossible"
        );
        assert_eq!(
            fs::read(&stale_path).unwrap(),
            progress_bytes("stale-main", "stale-incarnation", 0),
            "failed preflight must leave the stale candidate unchanged"
        );
    }

    #[test]
    fn group_cleanup_progress_recycle_does_not_install_before_combined_capacity() {
        let _fixture_ownership = GlobalStateLock::new();
        let tmp = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        fs::create_dir(&context.state_dir).unwrap();
        let progress_bytes = |principal_session_id: &str,
                              principal_incarnation: &str,
                              padding: usize| {
            serde_json::to_vec(&GroupCleanupProgressReceipt {
                schema_version: orchestration::GROUP_CLEANUP_PROGRESS_RECEIPT_V1_SCHEMA.to_string(),
                requested_session_id: None,
                principal_session_id: principal_session_id.to_string(),
                principal_incarnation: principal_incarnation.to_string(),
                idempotency_key: "retention-combined-capacity".to_string(),
                request_digest: "4".repeat(64),
                outcome: json!({
                    "schema_version": GROUP_CLEANUP_RESULT_SCHEMA,
                    "completed": false,
                    "padding": "x".repeat(padding)
                }),
            })
            .unwrap()
        };
        let stale_key = format!("{:064x}", 0);
        orchestration::store_group_cleanup_progress(
            &context,
            &stale_key,
            &progress_bytes("abandoned-main", "abandoned-incarnation", 0),
        )
        .unwrap();
        let progress_dir = context
            .state_dir
            .join("orchestration/group-cleanup-progress");
        let stale_path = progress_dir.join(&stale_key);
        fs::File::options()
            .write(true)
            .open(&stale_path)
            .unwrap()
            .set_times(
                fs::FileTimes::new().set_modified(std::time::UNIX_EPOCH + Duration::from_secs(1)),
            )
            .unwrap();
        let large_padding = 4 * 1024 * 1024 - 2048;
        for index in 1..128 {
            let session_id = format!("live-main-{index}");
            let incarnation = format!("live-incarnation-{index}");
            crate::write_session_record(&context, &cleanup_test_session(&session_id, &incarnation))
                .unwrap();
            let padding = usize::from(index <= 3) * large_padding;
            let path = progress_dir.join(format!("{index:064x}"));
            fs::write(&path, progress_bytes(&session_id, &incarnation, padding)).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        }

        let incoming_key = "f".repeat(64);
        let incoming = progress_bytes("new-main", "new-incarnation", large_padding);
        let error = orchestration::store_group_cleanup_progress(&context, &incoming_key, &incoming)
            .expect_err("combined pressure without enough stale bytes must fail closed");

        assert_eq!(error.code(), "group-cleanup-progress-capacity");
        assert!(
            !progress_dir.join(incoming_key).exists(),
            "the incoming checkpoint is the final mutation after both limits are secured"
        );
        let aggregate = fs::read_dir(&progress_dir)
            .unwrap()
            .map(|entry| entry.unwrap().metadata().unwrap().len())
            .sum::<u64>();
        assert!(aggregate <= 16 * 1024 * 1024);
        assert_eq!(fs::read_dir(&progress_dir).unwrap().count(), 128);
    }

    #[test]
    fn group_cleanup_progress_retention_treats_uncertain_session_lookup_as_live_capacity() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        fs::create_dir(&context.state_dir).unwrap();
        let progress_bytes = |principal_session_id: &str, principal_incarnation: &str| {
            serde_json::to_vec(&GroupCleanupProgressReceipt {
                schema_version: orchestration::GROUP_CLEANUP_PROGRESS_RECEIPT_V1_SCHEMA.to_string(),
                requested_session_id: None,
                principal_session_id: principal_session_id.to_string(),
                principal_incarnation: principal_incarnation.to_string(),
                idempotency_key: "retention-uncertain-session".to_string(),
                request_digest: "f".repeat(64),
                outcome: json!({
                    "schema_version": GROUP_CLEANUP_RESULT_SCHEMA,
                    "completed": false,
                    "workers": []
                }),
            })
            .unwrap()
        };
        for session_id in ["uncertain-main-a", "uncertain-main-b"] {
            crate::write_session_record(
                &context,
                &cleanup_test_session(session_id, "uncertain-incarnation"),
            )
            .unwrap();
        }
        let uncertain_key = format!("{:064x}", 0);
        orchestration::store_group_cleanup_progress(
            &context,
            &uncertain_key,
            &progress_bytes("uncertain-main", "uncertain-incarnation"),
        )
        .unwrap();
        let progress_dir = context
            .state_dir
            .join("orchestration/group-cleanup-progress");
        let uncertain_path = progress_dir.join(&uncertain_key);
        fs::File::options()
            .write(true)
            .open(&uncertain_path)
            .unwrap()
            .set_times(
                fs::FileTimes::new().set_modified(std::time::UNIX_EPOCH + Duration::from_secs(1)),
            )
            .unwrap();
        for index in 1..128 {
            let session_id = format!("live-main-{index}");
            let incarnation = format!("live-incarnation-{index}");
            crate::write_session_record(&context, &cleanup_test_session(&session_id, &incarnation))
                .unwrap();
            let path = progress_dir.join(format!("{index:064x}"));
            fs::write(&path, progress_bytes(&session_id, &incarnation)).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        }

        let error = orchestration::store_group_cleanup_progress(
            &context,
            &"f".repeat(64),
            &progress_bytes("new-main", "new-incarnation"),
        )
        .expect_err("uncertain session lookup must fail capacity admission closed");
        assert_eq!(error.code(), "group-cleanup-progress-capacity");
        assert!(
            uncertain_path.exists(),
            "an uncertain principal lookup must never make progress evictable"
        );
        assert_eq!(fs::read_dir(&progress_dir).unwrap().count(), 128);
    }

    #[test]
    fn group_cleanup_progress_retention_preserves_legacy_alias_fence_after_alias_reuse() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        fs::create_dir(&context.state_dir).unwrap();
        let progress_bytes = |principal_session_id: &str, principal_incarnation: &str| {
            serde_json::to_vec(&GroupCleanupProgressReceipt {
                schema_version: orchestration::GROUP_CLEANUP_PROGRESS_RECEIPT_V1_SCHEMA.to_string(),
                requested_session_id: None,
                principal_session_id: principal_session_id.to_string(),
                principal_incarnation: principal_incarnation.to_string(),
                idempotency_key: "v1-alias-fence-retention".to_string(),
                request_digest: "7".repeat(64),
                outcome: json!({
                    "schema_version": GROUP_CLEANUP_RESULT_SCHEMA,
                    "completed": false,
                    "workers": []
                }),
            })
            .unwrap()
        };
        let legacy_bytes = serde_json::to_vec(&GroupCleanupProgressReceipt {
            schema_version: orchestration::GROUP_CLEANUP_PROGRESS_RECEIPT_V1_SCHEMA.to_string(),
            requested_session_id: None,
            principal_session_id: "main-c".to_string(),
            principal_incarnation: "original-incarnation".to_string(),
            idempotency_key: "v1-alias-fence-retention".to_string(),
            request_digest: "7".repeat(64),
            outcome: json!({
                "schema_version": GROUP_CLEANUP_RESULT_SCHEMA,
                "completed": false,
                "_resume": {
                    "plan": {
                        "main": {
                            "session_id": "main-controller",
                            "session_incarnation": "original-incarnation"
                        }
                    },
                    "pending_registry_fences": [{
                        "session_id": "main-controller",
                        "runtime_launch_id": "original-incarnation"
                    }]
                }
            }),
        })
        .unwrap();
        crate::write_session_record(
            &context,
            &cleanup_test_session("main-cat", "replacement-incarnation"),
        )
        .unwrap();
        let legacy_key = format!("{:064x}", 0);
        orchestration::store_group_cleanup_progress(&context, &legacy_key, &legacy_bytes).unwrap();
        let progress_dir = context
            .state_dir
            .join("orchestration/group-cleanup-progress");
        let legacy_path = progress_dir.join(&legacy_key);
        fs::File::options()
            .write(true)
            .open(&legacy_path)
            .unwrap()
            .set_times(
                fs::FileTimes::new().set_modified(std::time::UNIX_EPOCH + Duration::from_secs(1)),
            )
            .unwrap();
        for index in 1..128 {
            let session_id = format!("live-v1-main-{index}");
            let incarnation = format!("live-v1-incarnation-{index}");
            crate::write_session_record(&context, &cleanup_test_session(&session_id, &incarnation))
                .unwrap();
            let path = progress_dir.join(format!("{index:064x}"));
            fs::write(&path, progress_bytes(&session_id, &incarnation)).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        }

        let error = orchestration::store_group_cleanup_progress(
            &context,
            &"f".repeat(64),
            &progress_bytes("incoming-main", "incoming-incarnation"),
        )
        .expect_err("a matching canonical pending fence must protect released-v1 alias progress");
        assert_eq!(error.code(), "group-cleanup-progress-capacity");
        assert_eq!(fs::read(&legacy_path).unwrap(), legacy_bytes);
    }

    #[test]
    fn group_cleanup_progress_retention_protects_stable_parse_and_open_failures() {
        let _fixture_ownership = GlobalStateLock::new();
        for failure in ["malformed", "unreadable"] {
            let tmp = tempfile::TempDir::new().unwrap();
            let context = CliContext {
                state_dir: tmp.path().join("state"),
                host: None,
            };
            fs::create_dir(&context.state_dir).unwrap();
            let progress_bytes = |principal_session_id: &str, principal_incarnation: &str| {
                serde_json::to_vec(&GroupCleanupProgressReceipt {
                    schema_version: orchestration::GROUP_CLEANUP_PROGRESS_RECEIPT_V1_SCHEMA
                        .to_string(),
                    requested_session_id: None,
                    principal_session_id: principal_session_id.to_string(),
                    principal_incarnation: principal_incarnation.to_string(),
                    idempotency_key: "retention-reader-failure".to_string(),
                    request_digest: "a".repeat(64),
                    outcome: json!({
                        "schema_version": GROUP_CLEANUP_RESULT_SCHEMA,
                        "completed": false,
                        "workers": []
                    }),
                })
                .unwrap()
            };
            let protected_key = format!("{:064x}", 0);
            let protected_bytes = match failure {
                "malformed" => b"{".to_vec(),
                "unreadable" => progress_bytes("unreadable-main", "unreadable-incarnation"),
                _ => unreachable!(),
            };
            orchestration::store_group_cleanup_progress(&context, &protected_key, &protected_bytes)
                .unwrap();
            let progress_dir = context
                .state_dir
                .join("orchestration/group-cleanup-progress");
            let protected_path = progress_dir.join(&protected_key);
            fs::File::options()
                .write(true)
                .open(&protected_path)
                .unwrap()
                .set_times(
                    fs::FileTimes::new()
                        .set_modified(std::time::UNIX_EPOCH + Duration::from_secs(1)),
                )
                .unwrap();
            for index in 1..128 {
                let session_id = format!("live-main-{index}");
                let incarnation = format!("live-incarnation-{index}");
                crate::write_session_record(
                    &context,
                    &cleanup_test_session(&session_id, &incarnation),
                )
                .unwrap();
                let path = progress_dir.join(format!("{index:064x}"));
                fs::write(&path, progress_bytes(&session_id, &incarnation)).unwrap();
                fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
            }
            if failure == "unreadable" {
                orchestration::fail_group_cleanup_progress_read_for_test(&protected_path);
            }

            let error = orchestration::store_group_cleanup_progress(
                &context,
                &"f".repeat(64),
                &progress_bytes("new-main", "new-incarnation"),
            )
            .expect_err("unverifiable progress must fail capacity admission closed");
            assert_eq!(
                error.code(),
                "group-cleanup-progress-capacity",
                "failure={failure}"
            );
            assert!(protected_path.exists(), "failure={failure}");
            assert_eq!(
                fs::read_dir(&progress_dir).unwrap().count(),
                128,
                "failure={failure}"
            );
        }
    }

    #[test]
    fn group_cleanup_progress_recovery_requires_an_exact_durable_requested_selector() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        fs::create_dir(&context.state_dir).unwrap();
        let canonical = "main-controller-unique";
        let alias = "main-c";
        let collision = "main-cat";
        let incarnation = "main-incarnation";
        let idempotency_key = "exact-progress-selector";
        let request_digest = "f".repeat(64);
        let outcome = json!({
            "schema_version": GROUP_CLEANUP_RESULT_SCHEMA,
            "completed": false,
            "_resume": {
                "plan": {
                    "main": {
                        "session_id": canonical,
                        "session_incarnation": incarnation
                    }
                },
                "pending_registry_fences": [{
                    "session_id": canonical,
                    "runtime_launch_id": incarnation
                }]
            }
        });
        let progress_key = "a".repeat(64);
        orchestration::store_group_cleanup_progress(
            &context,
            &progress_key,
            &serde_json::to_vec(&json!({
                "schema_version": "agent-session.main-agent-group-cleanup-receipt.v1",
                "principal_session_id": canonical,
                "principal_incarnation": incarnation,
                "idempotency_key": idempotency_key,
                "request_digest": request_digest,
                "outcome": outcome,
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            orchestration::recover_group_cleanup_progress_principal(
                &context,
                alias,
                incarnation,
                idempotency_key,
                &request_digest,
            )
            .unwrap(),
            None,
            "a canonical principal that merely shares the alias prefix is not durable alias evidence"
        );

        orchestration::remove_group_cleanup_progress(&context, &progress_key).unwrap();
        orchestration::store_group_cleanup_progress(
            &context,
            &progress_key,
            &serde_json::to_vec(&json!({
                "schema_version": "agent-session.main-agent-group-cleanup-receipt.v2",
                "requested_session_id": alias,
                "principal_session_id": canonical,
                "principal_incarnation": incarnation,
                "idempotency_key": idempotency_key,
                "request_digest": request_digest,
                "outcome": outcome,
            }))
            .unwrap(),
        )
        .unwrap();
        orchestration::store_group_cleanup_progress(
            &context,
            &"b".repeat(64),
            &serde_json::to_vec(&json!({
                "schema_version": "agent-session.main-agent-group-cleanup-receipt.v2",
                "requested_session_id": "other-selector",
                "principal_session_id": "other-principal",
                "principal_incarnation": "other-incarnation",
                "idempotency_key": "other-idempotency-key",
                "request_digest": "0".repeat(64),
                "outcome": {},
            }))
            .unwrap(),
        )
        .unwrap();
        orchestration::reset_group_cleanup_progress_receipt_decodes_for_test();
        assert_eq!(
            orchestration::recover_group_cleanup_progress_principal(
                &context,
                alias,
                incarnation,
                idempotency_key,
                &request_digest,
            )
            .unwrap()
            .as_deref(),
            Some(canonical),
            "the exact requested selector must recover its canonical principal"
        );
        assert_eq!(
            orchestration::group_cleanup_progress_receipt_decodes_for_test(),
            2,
            "each candidate body must be decoded exactly once while recovery holds the progress lock"
        );
        assert_eq!(
            orchestration::recover_group_cleanup_progress_principal(
                &context,
                collision,
                incarnation,
                idempotency_key,
                &request_digest,
            )
            .unwrap(),
            None,
            "a longer colliding selector must not adopt another alias mapping"
        );
    }

    #[test]
    fn group_cleanup_progress_recovery_rejects_a_wrong_selector_without_pending_fences() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        fs::create_dir(&context.state_dir).unwrap();
        let canonical = "main-controller-unique";
        let original_alias = "main-c";
        let other_alias = "main-cat";
        let incarnation = "main-incarnation";
        let idempotency_key = "exact-progress-selector-without-fences";
        let request_digest = "e".repeat(64);
        let progress = GroupCleanupProgressReceipt {
            schema_version: orchestration::GROUP_CLEANUP_PROGRESS_RECEIPT_SCHEMA.to_string(),
            requested_session_id: Some(original_alias.to_string()),
            principal_session_id: canonical.to_string(),
            principal_incarnation: incarnation.to_string(),
            idempotency_key: idempotency_key.to_string(),
            request_digest: request_digest.clone(),
            outcome: json!({
                "schema_version": GROUP_CLEANUP_RESULT_SCHEMA,
                "completed": true,
                "_resume": {
                    "plan": {
                        "main": {
                            "session_id": canonical,
                            "session_incarnation": incarnation
                        }
                    },
                    "pending_registry_fences": []
                }
            }),
        };
        orchestration::store_group_cleanup_progress(
            &context,
            &"a".repeat(64),
            &serde_json::to_vec(&progress).unwrap(),
        )
        .unwrap();

        assert_eq!(
            orchestration::recover_group_cleanup_progress_principal(
                &context,
                other_alias,
                incarnation,
                idempotency_key,
                &request_digest,
            )
            .unwrap(),
            None,
            "absence of a pending fence must not substitute for an exact durable selector mapping"
        );
    }

    #[test]
    fn group_cleanup_live_replay_requires_the_original_exact_selector() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        fs::create_dir(&context.state_dir).unwrap();
        let canonical = "main-controller-unique";
        let original_alias = "main-c";
        let other_alias = "main-cont";
        let incarnation = "main-incarnation";
        let idempotency_key = "live-exact-selector";
        let request_digest = "9".repeat(64);
        let plan = GroupCleanupPlan {
            schema_version: GROUP_CLEANUP_SCHEMA.to_string(),
            main: SessionRef {
                machine: None,
                session_id: canonical.to_string(),
                session_incarnation: incarnation.to_string(),
                session_created_at: "2030-01-01T00:00:00Z".to_string(),
            },
            run_id: "run-live-selector".to_string(),
            run_revision: 7,
            requires_force: false,
            workers: Vec::new(),
            plan_digest: format!("sha256:{}", "a".repeat(64)),
        };
        let outcome = group_cleanup_stored_outcome(
            &group_cleanup_progress_value(&plan, &[], false, "authority_sealed"),
            &group_cleanup_resume_state(&plan, &[], &[], &[], false),
        )
        .unwrap();
        let progress_key = group_cleanup_progress_key(canonical, incarnation, idempotency_key);
        let progress = serde_json::to_vec(&GroupCleanupProgressReceipt {
            schema_version: orchestration::GROUP_CLEANUP_PROGRESS_RECEIPT_SCHEMA.to_string(),
            requested_session_id: Some(original_alias.to_string()),
            principal_session_id: canonical.to_string(),
            principal_incarnation: incarnation.to_string(),
            idempotency_key: idempotency_key.to_string(),
            request_digest: request_digest.to_string(),
            outcome: outcome.clone(),
        })
        .unwrap();
        orchestration::store_group_cleanup_progress(&context, &progress_key, &progress).unwrap();

        let error = match group_cleanup_replay_with_legacy_alias(
            &context,
            &orchestration::Registry::default(),
            canonical,
            Some(other_alias),
            incarnation,
            idempotency_key,
            &request_digest,
        ) {
            Ok(_) => panic!("a different live alias must not adopt exact-selector progress"),
            Err(error) => error,
        };
        assert_eq!(error.code(), "group-cleanup-progress-invalid");
        assert_eq!(
            orchestration::read_group_cleanup_progress(&context, &progress_key)
                .unwrap()
                .unwrap(),
            progress,
            "a rejected alias must not rewrite the original durable selector"
        );

        orchestration::remove_group_cleanup_progress(&context, &progress_key).unwrap();
        let mut registry = orchestration::Registry::default();
        store_receipt_for_principal(
            &mut registry,
            canonical,
            incarnation,
            idempotency_key,
            "group-cleanup",
            &request_digest,
            json!({
                "schema_version": GROUP_CLEANUP_RESULT_SCHEMA,
                "completed": true,
            }),
        )
        .unwrap();
        assert!(
            group_cleanup_replay_with_legacy_alias(
                &context,
                &registry,
                canonical,
                Some(other_alias),
                incarnation,
                idempotency_key,
                &request_digest,
            )
            .unwrap()
            .is_none(),
            "a canonical terminal receipt must not authorize a different exact alias"
        );
    }

    #[test]
    fn completed_group_cleanup_recovery_requires_an_exact_alias_receipt() {
        let canonical = "main-controller-unique";
        let alias = "main-c";
        let incarnation = "main-incarnation";
        let idempotency_key = "exact-completed-selector";
        let request_digest = "a".repeat(64);
        let outcome = json!({
            "completed": true,
            "_resume": {
                "plan": {
                    "main": {
                        "session_id": canonical,
                        "session_incarnation": incarnation
                    }
                }
            }
        });
        let mut registry = orchestration::Registry::default();
        store_receipt_for_principal(
            &mut registry,
            canonical,
            incarnation,
            idempotency_key,
            "group-cleanup",
            &request_digest,
            outcome.clone(),
        )
        .unwrap();
        assert_eq!(
            recover_completed_group_cleanup_principal(
                &registry,
                alias,
                incarnation,
                idempotency_key,
                &request_digest,
            )
            .unwrap(),
            None,
            "a canonical receipt must not be discovered through prefix inference"
        );

        registry.receipts.clear();
        store_receipt_for_principal(
            &mut registry,
            alias,
            incarnation,
            idempotency_key,
            "group-cleanup",
            &request_digest,
            outcome,
        )
        .unwrap();
        assert_eq!(
            recover_completed_group_cleanup_principal(
                &registry,
                alias,
                incarnation,
                idempotency_key,
                &request_digest,
            )
            .unwrap()
            .as_deref(),
            Some(canonical),
            "the exact alias-keyed terminal receipt must recover the canonical plan principal"
        );
        assert_eq!(
            recover_completed_group_cleanup_principal(
                &registry,
                "main-cat",
                incarnation,
                idempotency_key,
                &request_digest,
            )
            .unwrap(),
            None,
            "a colliding longer selector must not adopt the exact alias receipt"
        );
    }

    #[test]
    fn group_cleanup_progress_scans_stop_at_the_first_capacity_overflow() {
        let _fixture_ownership = GlobalStateLock::new();
        let tmp = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        fs::create_dir(&context.state_dir).unwrap();
        orchestration::store_group_cleanup_progress(&context, &"a".repeat(64), b"{}").unwrap();
        let progress_dir = context
            .state_dir
            .join("orchestration/group-cleanup-progress");
        for index in 0..300 {
            let path = progress_dir.join(format!("{index:064x}"));
            fs::write(&path, []).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        }

        let visits =
            orchestration::install_group_cleanup_progress_visit_counter_for_test(&progress_dir);
        let store_error =
            orchestration::store_group_cleanup_progress(&context, &"f".repeat(64), b"{}")
                .expect_err("an externally flooded progress directory must fail capacity closed");
        orchestration::clear_group_cleanup_progress_visit_counter_for_test();
        assert_eq!(store_error.code(), "group-cleanup-progress-capacity");
        assert_eq!(
            visits.load(Ordering::Acquire),
            129,
            "retention admission must stop at the first file-count overflow"
        );

        let visits =
            orchestration::install_group_cleanup_progress_visit_counter_for_test(&progress_dir);
        let recovery_error = orchestration::recover_group_cleanup_progress_principal(
            &context,
            "main",
            "main-incarnation",
            "capacity-recovery",
            &"c".repeat(64),
        )
        .expect_err("alias recovery must reject a flooded progress directory");
        orchestration::clear_group_cleanup_progress_visit_counter_for_test();
        assert_eq!(recovery_error.code(), "group-cleanup-progress-capacity");
        assert_eq!(
            visits.load(Ordering::Acquire),
            129,
            "alias recovery must stop at the first file-count overflow"
        );
    }

    #[test]
    fn group_cleanup_run_revision_overflow_fails_before_registry_or_session_mutation() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let mut main = cleanup_test_session("main", "main-incarnation");
        crate::mark_tmux_runtime_never_launched(&mut main);
        fs::create_dir_all(session_dir(&context, &main.id)).unwrap();
        crate::write_session_record(&context, &main).unwrap();
        let mut run = run_record("run-max", false);
        run.controller = session_ref(&context, &main, "main-incarnation");
        run.revision = u64::MAX;
        {
            let mut locked = orchestration::lock_registry(&context).unwrap();
            locked.registry.runs.insert(run.run_id.clone(), run);
            locked.save().unwrap();
        }
        let preview = preview_group_cleanup(&context, "main").unwrap();
        let before_registry =
            fs::read(context.state_dir.join("orchestration/registry.json")).unwrap();
        let request = GroupCleanupRequest {
            schema_version: GROUP_CLEANUP_REQUEST_SCHEMA.to_string(),
            expected_main_incarnation: "main-incarnation".to_string(),
            expected_run_revision: u64::MAX,
            expected_plan_digest: preview["plan_digest"].as_str().unwrap().to_string(),
            mode: GroupCleanupMode::Safe,
            idempotency_key: "run-revision-overflow".to_string(),
        };

        let error = execute_group_cleanup(&context, "main", request, PathBuf::from("/bin/false"))
            .err()
            .expect("an exhausted run revision must fail before cleanup effects");
        assert_eq!(error.code(), "orchestration-revision-capacity");
        assert_eq!(
            fs::read(context.state_dir.join("orchestration/registry.json")).unwrap(),
            before_registry,
            "run revision overflow must leave the registry byte-for-byte unchanged"
        );
        assert!(
            session_dir(&context, "main").exists(),
            "run revision overflow must not delete the Main Agent"
        );
    }

    #[test]
    fn a_closed_run_no_longer_counts_as_session_control() {
        let mut registry = orchestration::Registry::default();
        let (session, created_at) = ("main", "2030-01-01T00:00:00Z");

        registry.runs.insert("q".to_string(), run_record("q", true));
        assert!(
            session_controls_live_run(&registry, session, created_at),
            "an active run must still block a second delegation"
        );

        // `quick` auto-closes its ephemeral run when the worker is torn down.
        // That must release the session, or it can never delegate again: the
        // documented `init` fallback needs the objective packet `quick`
        // synthesized and never handed back.
        registry.runs.get_mut("q").expect("run").state = "closed".to_string();
        assert!(!session_controls_live_run(&registry, session, created_at));

        // A live run owned by a different session is not this session's control.
        let mut other = run_record("other", false);
        other.controller.session_id = "someone-else".to_string();
        registry.runs.insert("other".to_string(), other);
        assert!(!session_controls_live_run(&registry, session, created_at));
    }

    #[test]
    fn maybe_autoclose_closes_only_ephemeral_runs_with_terminal_work() {
        // Ephemeral run, all assignments terminal → closes.
        let mut registry = orchestration::Registry::default();
        registry.runs.insert("q".to_string(), run_record("q", true));
        registry
            .assignments
            .insert("a".to_string(), dep_assignment("a", "q", "released"));
        assert!(maybe_autoclose_ephemeral_run(&mut registry, "q"));
        assert_eq!(registry.runs["q"].state, "closed");

        // Ephemeral run with a non-terminal assignment → stays open.
        let mut registry = orchestration::Registry::default();
        registry.runs.insert("q".to_string(), run_record("q", true));
        registry
            .assignments
            .insert("a".to_string(), dep_assignment("a", "q", "released"));
        registry
            .assignments
            .insert("b".to_string(), dep_assignment("b", "q", "working"));
        assert!(!maybe_autoclose_ephemeral_run(&mut registry, "q"));
        assert_eq!(registry.runs["q"].state, "active");

        // A non-ephemeral run never auto-closes.
        let mut registry = orchestration::Registry::default();
        registry
            .runs
            .insert("r".to_string(), run_record("r", false));
        registry
            .assignments
            .insert("a".to_string(), dep_assignment("a", "r", "released"));
        assert!(!maybe_autoclose_ephemeral_run(&mut registry, "r"));
        assert_eq!(registry.runs["r"].state, "active");
    }

    #[test]
    fn readiness_from_state_flags_only_starting_as_failed() {
        assert_eq!(readiness_from_state("starting"), "readiness_failed");
        for advanced in [
            "working",
            "blocked",
            "submitted",
            "accepted",
            "released",
            "cancelled",
        ] {
            assert_eq!(
                readiness_from_state(advanced),
                "ready",
                "{advanced} proves readiness"
            );
        }
    }

    #[test]
    fn readiness_from_state_maps_starting_to_failed_and_advanced_to_ready() {
        // T1 readiness fold classification: only `starting` is not-yet-ready; any
        // advanced state the worker's checkpoint reaches classifies as ready.
        assert_eq!(readiness_from_state("starting"), "readiness_failed");
        assert_eq!(readiness_from_state("working"), "ready");
        assert_eq!(readiness_from_state("submitted"), "ready");
        assert_eq!(readiness_from_state("accepted"), "ready");
        assert_eq!(readiness_from_state("released"), "ready");
    }

    #[test]
    fn submit_key_recovery_is_only_for_fresh_codex_and_claude_workers() {
        assert!(worker_submit_key_recovery_eligible(true, AgentKind::Codex));
        assert!(worker_submit_key_recovery_eligible(true, AgentKind::Claude));
        assert!(!worker_submit_key_recovery_eligible(
            true,
            AgentKind::Hermes
        ));
        assert!(!worker_submit_key_recovery_eligible(
            false,
            AgentKind::Codex
        ));
        assert!(!worker_submit_key_recovery_eligible(
            false,
            AgentKind::Claude
        ));
    }

    fn write_fake_rate_limit_diagnostic(
        stdout: &str,
        prelude: &str,
    ) -> (tempfile::TempDir, PathBuf) {
        use std::os::unix::fs::PermissionsExt;

        assert!(
            !stdout.contains('\''),
            "fixture must remain shell-literal safe"
        );
        let temporary = tempfile::TempDir::new().expect("temporary diagnostic directory");
        let binary = temporary.path().join("codex-cli");
        let script = format!(
            "#!/bin/sh\n\
             if [ \"$1\" != \"diag\" ] || [ \"$2\" != \"rate-limits\" ] || \
             [ \"$3\" != \"--format\" ] || [ \"$4\" != \"json\" ] || \
             [ \"$5\" != \"--no-refresh-auth\" ] || [ \"$6\" != \"alpha.json\" ]; then\n\
               exit 97\n\
             fi\n\
             {prelude}\n\
             printf '%s' '{stdout}'\n"
        );
        let staged_binary = temporary.path().join("codex-cli.staged");
        let mut file = fs::File::create(&staged_binary).expect("create diagnostic fixture");
        std::io::Write::write_all(&mut file, script.as_bytes()).expect("write diagnostic fixture");
        file.sync_all().expect("sync diagnostic fixture");
        drop(file);
        fs::set_permissions(&staged_binary, fs::Permissions::from_mode(0o700))
            .expect("make diagnostic fixture executable");
        fs::rename(&staged_binary, &binary).expect("publish diagnostic fixture");
        (temporary, binary)
    }

    fn rate_limit_diagnostic_fixture(account: &str, used: i64, remaining: i64) -> String {
        serde_json::to_string(&json!({
            "schema_version": "codex-cli.diag.rate-limits.v1",
            "command": "diag rate-limits",
            "mode": "single",
            "ok": true,
            "result": {
                "provider": "codex",
                "name": account,
                "target_file": format!("{account}.json"),
                "status": "ok",
                "ok": true,
                "source": "network",
                "windows": [{
                    "label": "Weekly",
                    "used_percent": used,
                    "remaining_percent": remaining
                }],
                "raw_usage": {
                    "email": "must-not-be-projected@example.invalid",
                    "transcript": "must-not-be-projected",
                    "path": "/must/not/be/projected"
                }
            }
        }))
        .expect("serialize diagnostic fixture")
    }

    #[test]
    fn raw_rate_limit_diagnostic_requires_true_staleness_and_exact_provenance() {
        assert!(raw_rate_limit_diagnostic_required(
            true,
            false,
            false,
            Some("codex")
        ));
        for (stale, structured, supported, agent) in [
            (false, false, false, Some("codex")),
            (true, true, false, Some("codex")),
            (true, false, true, Some("codex")),
            (true, false, false, Some("claude")),
            (true, false, false, None),
        ] {
            assert!(!raw_rate_limit_diagnostic_required(
                stale, structured, supported, agent
            ));
        }

        let live_raw_shape = json!({
            "schema_version": "agent-session.codex-account.v1",
            "supported": false,
            "state": "unsupported",
            "revision": 0
        });
        assert_eq!(selected_raw_account_provenance(&live_raw_shape), None);
        let diagnostic = selected_raw_account_provenance(&live_raw_shape).map_or(
            RawRateLimitDiagnostic::Unavailable("selected-raw-account-unavailable"),
            diagnose_selected_raw_account_rate_limits,
        );
        assert_eq!(
            diagnostic,
            RawRateLimitDiagnostic::Unavailable("selected-raw-account-unavailable")
        );
    }

    #[test]
    fn raw_rate_limit_diagnostic_classifies_exhausted_and_available_without_raw_output() {
        let _fixture_ownership = GlobalStateLock::new();
        let exhausted_json = rate_limit_diagnostic_fixture("alpha", 100, 0);
        let (_temporary, binary) = write_fake_rate_limit_diagnostic(&exhausted_json, ":");
        let exhausted = diagnose_selected_raw_account_rate_limits_with_io(
            &binary,
            "alpha",
            Duration::from_secs(1),
        )
        .unwrap_or_else(|error| {
            panic!(
                "exhausted diagnostic fixture {} failed: {error:?}",
                binary.display()
            )
        });
        assert_eq!(exhausted, RawRateLimitDiagnostic::Exhausted);
        let projection = serde_json::to_string(&exhausted.projection()).expect("projection");
        for private in [
            "must-not-be-projected@example.invalid",
            "must-not-be-projected",
            "/must/not/be/projected",
            exhausted_json.as_str(),
        ] {
            assert!(!projection.contains(private));
        }

        let available_json = rate_limit_diagnostic_fixture("alpha", 41, 59);
        let (_temporary, binary) = write_fake_rate_limit_diagnostic(&available_json, ":");
        assert_eq!(
            diagnose_selected_raw_account_rate_limits_with_io(
                &binary,
                "alpha",
                Duration::from_secs(1)
            )
            .unwrap_or_else(|error| {
                panic!(
                    "available diagnostic fixture {} failed: {error:?}",
                    binary.display()
                )
            }),
            RawRateLimitDiagnostic::Available
        );
    }

    #[test]
    fn raw_rate_limit_diagnostic_fails_closed_for_malformed_timeout_and_wrong_account() {
        let _fixture_ownership = GlobalStateLock::new();
        let (_temporary, binary) = write_fake_rate_limit_diagnostic("{malformed", ":");
        assert_eq!(
            diagnose_selected_raw_account_rate_limits_with(
                &binary,
                "alpha",
                Duration::from_secs(1)
            ),
            RawRateLimitDiagnostic::Unavailable("diagnostic-response-invalid")
        );

        let available_json = rate_limit_diagnostic_fixture("alpha", 10, 90);
        let (_temporary, binary) = write_fake_rate_limit_diagnostic(&available_json, "sleep 1");
        assert_eq!(
            diagnose_selected_raw_account_rate_limits_with(
                &binary,
                "alpha",
                Duration::from_millis(25)
            ),
            RawRateLimitDiagnostic::Unavailable("diagnostic-timeout")
        );

        let wrong_account_json = rate_limit_diagnostic_fixture("beta", 100, 0);
        let (_temporary, binary) = write_fake_rate_limit_diagnostic(&wrong_account_json, ":");
        assert_eq!(
            diagnose_selected_raw_account_rate_limits_with(
                &binary,
                "alpha",
                Duration::from_secs(1)
            ),
            RawRateLimitDiagnostic::Unavailable("diagnostic-account-mismatch")
        );
    }

    #[test]
    fn account_handoff_does_not_advertise_an_unimplemented_raw_restart_flag() {
        let error = MainAgentCli::try_parse_from([
            "main-agent",
            "worker",
            "account-handoff",
            "assignment",
            "--account",
            "alpha",
            "--if-revision",
            "1",
            "--authorize-account-change",
            "--allow-raw-restart",
            "--idempotency-key",
            "handoff-001",
        ])
        .expect_err("the removed raw restart flag must not parse");
        assert_eq!(error.kind(), ErrorKind::UnknownArgument);
    }

    #[test]
    fn account_handoff_requires_both_actionable_evidence_and_typed_controls() {
        let unsupported = json!({
            "supported": false,
            "next": { "state": "unavailable" }
        });
        let supported = json!({
            "supported": true,
            "next": { "state": "idle" }
        });
        let queued = json!({
            "supported": true,
            "next": { "state": "queued" }
        });
        let raw_auto_resume = json!({ "supported": false });
        let managed_auto_resume = json!({ "supported": true });

        assert_eq!(
            account_handoff_facts(&unsupported, &raw_auto_resume, true, false, false),
            AccountHandoffFacts {
                capability_gap: true,
                required: false
            },
            "real raw quota evidence must expose the capability gap, not a prose-only action"
        );
        assert_eq!(
            account_handoff_facts(&supported, &managed_auto_resume, true, false, true),
            AccountHandoffFacts {
                capability_gap: false,
                required: true
            },
            "managed quota evidence has an executable typed handoff"
        );
        assert_eq!(
            account_handoff_facts(&supported, &managed_auto_resume, true, false, false),
            AccountHandoffFacts {
                capability_gap: true,
                required: false
            },
            "account and auto-resume booleans must not infer the versioned capability"
        );
        assert_eq!(
            account_handoff_facts(&queued, &managed_auto_resume, false, false, true),
            AccountHandoffFacts {
                capability_gap: false,
                required: true
            },
            "a durable queued transition remains actionable without re-inferring quota"
        );
        assert_eq!(
            account_handoff_facts(&unsupported, &raw_auto_resume, false, false, false),
            AccountHandoffFacts {
                capability_gap: false,
                required: false
            },
            "an unsupported raw worker without exact evidence must not infer ambient account state"
        );
    }

    #[test]
    fn account_handoff_state_fails_closed_after_active_worker_states() {
        for allowed in ["starting", "working", "blocked"] {
            let assignment = dep_assignment("assignment", "run", allowed);
            assert!(
                ensure_account_handoff_eligible_state(&assignment).is_ok(),
                "{allowed} remains eligible for typed account recovery"
            );
        }
        for refused in ["assigned", "submitted", "accepted", "released", "cancelled"] {
            let assignment = dep_assignment("assignment", "run", refused);
            assert_eq!(
                ensure_account_handoff_eligible_state(&assignment)
                    .unwrap_err()
                    .code(),
                "account-handoff-assignment-state",
                "{refused} must not admit an account side effect"
            );
        }
    }

    fn git_stdout(path: &Path, args: &[&str]) -> Vec<u8> {
        let output = Command::new("git")
            .current_dir(path)
            .args(args)
            .output()
            .expect("run git fixture command");
        assert!(
            output.status.success(),
            "git fixture command failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    }

    fn fingerprint_status(path: &Path) -> Vec<u8> {
        git_stdout(
            path,
            &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
        )
    }

    #[test]
    fn worktree_fingerprint_detects_same_status_edits_deletions_and_untracked_material() {
        let temporary = tempfile::TempDir::new().expect("temporary repository");
        let repository = temporary.path();
        git_stdout(repository, &["init", "--quiet"]);
        fs::write(repository.join("tracked.txt"), "base\n").expect("write tracked fixture");
        git_stdout(repository, &["add", "tracked.txt"]);

        fs::write(repository.join("tracked.txt"), "aaaa\n").expect("first tracked edit");
        let first_status = fingerprint_status(repository);
        let first =
            worktree_material_fingerprint(repository, &first_status).expect("first fingerprint");
        fs::write(repository.join("tracked.txt"), "bbbb\n").expect("same-size tracked edit");
        let second_status = fingerprint_status(repository);
        assert_eq!(
            first_status, second_status,
            "porcelain status alone cannot observe continued edits to one modified path"
        );
        let second =
            worktree_material_fingerprint(repository, &second_status).expect("second fingerprint");
        assert_ne!(
            first, second,
            "tracked material must advance the fingerprint"
        );

        fs::remove_file(repository.join("tracked.txt")).expect("delete tracked fixture");
        let deleted_status = fingerprint_status(repository);
        let deleted = worktree_material_fingerprint(repository, &deleted_status)
            .expect("deletion fingerprint");
        assert_ne!(
            second, deleted,
            "deletion-only progress must not inherit the prior dirty snapshot"
        );

        fs::write(repository.join("untracked.txt"), "one\n").expect("first untracked content");
        let untracked_status = fingerprint_status(repository);
        let untracked_first = worktree_material_fingerprint(repository, &untracked_status)
            .expect("first untracked fingerprint");
        fs::write(repository.join("untracked.txt"), "two\n")
            .expect("same-size untracked content edit");
        assert_eq!(
            untracked_status,
            fingerprint_status(repository),
            "untracked porcelain identity stays constant across content edits"
        );
        let untracked_second =
            worktree_material_fingerprint(repository, &untracked_status).expect("second untracked");
        assert_ne!(
            untracked_first, untracked_second,
            "untracked content must participate in the bounded fingerprint"
        );
    }

    #[test]
    fn worktree_fingerprint_streams_oversize_tracked_patch_for_healthy_supervision() {
        let temporary = tempfile::TempDir::new().expect("temporary repository");
        let repository = temporary.path();
        git_stdout(repository, &["init", "--quiet"]);
        let tracked = repository.join("tracked.bin");
        let length = WORKTREE_FINGERPRINT_MAX_BYTES + 64 * 1024;
        fs::write(&tracked, vec![0_u8; length]).expect("write tracked base");
        git_stdout(repository, &["add", "tracked.bin"]);

        let mut state = 0x4d59_5df4_d0f3_3173_u64;
        let mut changed = Vec::with_capacity(length);
        for _ in 0..length {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            changed.push(state as u8);
        }
        fs::write(&tracked, &changed).expect("write oversized tracked edit");
        let patch = git_stdout(
            repository,
            &[
                "diff",
                "--no-ext-diff",
                "--no-textconv",
                "--binary",
                "--full-index",
                "--",
            ],
        );
        assert!(
            patch.len() > WORKTREE_FINGERPRINT_MAX_BYTES,
            "the regression must exercise a tracked patch larger than the former buffer cap"
        );

        let status = fingerprint_status(repository);
        let first = worktree_material_fingerprint(repository, &status)
            .expect("oversized tracked fingerprint");
        assert_eq!(
            worktree_material_fingerprint(repository, &status),
            Some(first.clone()),
            "unchanged oversized material must fingerprint deterministically"
        );
        changed[length / 2] ^= 0xff;
        fs::write(&tracked, &changed).expect("change oversized tracked edit");
        let changed_status = fingerprint_status(repository);
        let second = worktree_material_fingerprint(repository, &changed_status)
            .expect("changed oversized tracked fingerprint");
        assert_ne!(
            first, second,
            "continued edits must advance an oversized tracked fingerprint"
        );

        let context = CliContext {
            state_dir: temporary.path().join("state"),
            host: None,
        };
        let assignment = dep_assignment("oversized-progress", "run-one", "working");
        let progress = inspect_worktree_progress(&context, &assignment, repository)
            .expect("inspect oversized worktree progress");
        assert_eq!(progress["available"], true);
        let repeated = inspect_worktree_progress(&context, &assignment, repository)
            .expect("repeat public supervision worktree observation");
        assert_eq!(
            repeated["material_fingerprint"], progress["material_fingerprint"],
            "repeated supervision must retain a stable fingerprint for unchanged material"
        );
        changed[length / 3] ^= 0xff;
        fs::write(&tracked, &changed).expect("same-size rewrite before supervision");
        let rewritten = inspect_worktree_progress(&context, &assignment, repository)
            .expect("inspect same-size rewrite through public supervision evidence");
        assert_eq!(rewritten["available"], true);
        assert_ne!(
            rewritten["material_fingerprint"], progress["material_fingerprint"],
            "same-size rewrites must never reuse stale progress"
        );
        let classification = classify_worker_diagnosis(WorkerDiagnosisFacts {
            evidence_unavailable: rewritten["available"] != true,
            ..base_diagnosis_facts()
        })
        .0;
        assert_eq!(
            classification, "healthy_progress",
            "oversized tracked progress must not degrade supervision evidence"
        );
    }

    #[test]
    fn clean_worktree_progress_skips_material_probes_and_unchanged_snapshot_rewrite() {
        let temporary = tempfile::TempDir::new().expect("temporary repository");
        let repository = temporary.path().join("repo");
        fs::create_dir(&repository).expect("repository directory");
        git_stdout(&repository, &["init", "--quiet"]);
        let context = CliContext {
            state_dir: temporary.path().join("state"),
            host: None,
        };
        let worker_record = cleanup_test_session("clean-progress-worker", "clean-progress-inc");
        fs::create_dir_all(session_dir(&context, &worker_record.id)).expect("session directory");
        crate::write_session_record(&context, &worker_record).expect("worker session");
        let mut assignment = dep_assignment("clean-progress-assignment", "run-one", "working");
        assignment.worker = Some(session_ref(&context, &worker_record, "clean-progress-inc"));

        reset_fingerprint_subprocess_launches_for_test();
        let first = inspect_worktree_progress(&context, &assignment, &repository)
            .expect("initial clean progress");
        assert_eq!(first["clean"], true);
        assert_eq!(
            fingerprint_subprocess_launches_for_test(),
            0,
            "empty porcelain status must not launch diff or untracked-list probes"
        );
        let name = crate::coordination::digest_bytes(assignment.assignment_id.as_bytes());
        let snapshot_path = session_dir(&context, &worker_record.id)
            .join("coordination")
            .join(format!("main-agent-progress-{name}.json"));
        let first_snapshot = fs::read(&snapshot_path).expect("first durable snapshot");

        reset_fingerprint_subprocess_launches_for_test();
        let repeated = inspect_worktree_progress(&context, &assignment, &repository)
            .expect("repeated clean progress");
        assert_eq!(
            repeated["material_fingerprint"],
            first["material_fingerprint"]
        );
        assert_eq!(
            fingerprint_subprocess_launches_for_test(),
            0,
            "repeated clean supervision must still use only porcelain status"
        );
        assert_eq!(
            fs::read(&snapshot_path).expect("unchanged durable snapshot"),
            first_snapshot,
            "an unchanged fingerprint must not rewrite and fsync its durable snapshot"
        );
    }

    #[test]
    fn worktree_fingerprint_streaming_boundaries_and_untracked_aggregate_are_explicit() {
        let temporary = tempfile::TempDir::new().expect("temporary repository");
        let repository = temporary.path();
        git_stdout(repository, &["init", "--quiet"]);
        let script_dir = tempfile::TempDir::new().expect("boundary git directory");
        let fake_git = script_dir.path().join("git");
        let output_size_file = script_dir.path().join("output-size");
        fs::write(
            &output_size_file,
            format!(
                "{} {}",
                WORKTREE_FINGERPRINT_MAX_BYTES - 1,
                WORKTREE_FINGERPRINT_MAX_BYTES - 1
            ),
        )
        .expect("initial output size");
        fs::write(
            &fake_git,
            format!(
                "#!/bin/sh\n\
                 if [ \"$1\" = \"diff\" ]; then\n\
                   case \" $* \" in\n\
                     *' --cached '*) selector=2 ;;\n\
                     *) selector=1 ;;\n\
                   esac\n\
                   set -- $(cat '{}')\n\
                   case \"$selector\" in\n\
                     2) size=\"$2\" ;;\n\
                     *) size=\"$1\" ;;\n\
                   esac\n\
                   head -c \"$size\" /dev/zero\n\
                 fi\n",
                output_size_file.display()
            ),
        )
        .expect("boundary git");
        fs::set_permissions(&fake_git, fs::Permissions::from_mode(0o700))
            .expect("boundary git mode");

        let half = WORKTREE_FINGERPRINT_MAX_BYTES / 2;
        let cases = [
            (WORKTREE_FINGERPRINT_MAX_BYTES - 1, 0),
            (WORKTREE_FINGERPRINT_MAX_BYTES, 0),
            (WORKTREE_FINGERPRINT_MAX_BYTES + 1, 0),
            (0, WORKTREE_FINGERPRINT_MAX_BYTES - 1),
            (0, WORKTREE_FINGERPRINT_MAX_BYTES),
            (0, WORKTREE_FINGERPRINT_MAX_BYTES + 1),
            (half, half - 1),
            (half, half),
            (half, half + 1),
        ];
        let mut fingerprints = Vec::new();
        for (unstaged_size, staged_size) in cases {
            fs::write(&output_size_file, format!("{unstaged_size} {staged_size}"))
                .expect("update staged and unstaged output size");
            fingerprints.push(
                worktree_material_fingerprint_with_git(
                    repository,
                    b" M tracked.bin\0",
                    &fake_git,
                    Duration::from_secs(2),
                )
                .expect("tracked boundary output remains streamable"),
            );
        }
        for pair in fingerprints.windows(2) {
            assert_ne!(
                pair[0], pair[1],
                "minus/at/plus and staged-plus-unstaged aggregate lengths must fingerprint distinctly"
            );
        }

        let mut status = Vec::new();
        for index in 0..8 {
            let name = format!("aggregate-{index}.bin");
            status.extend_from_slice(b"?? ");
            status.extend_from_slice(name.as_bytes());
            status.push(0);
            fs::write(
                repository.join(name),
                vec![index as u8; WORKTREE_FINGERPRINT_MAX_BYTES / 8],
            )
            .expect("aggregate untracked file");
        }
        assert_eq!(
            worktree_material_fingerprint(repository, &status),
            None,
            "aggregate untracked content above the bounded contract must be explicitly unavailable"
        );
    }

    #[test]
    fn worktree_fingerprint_rejects_large_untracked_sets_before_path_walk() {
        let temporary = tempfile::TempDir::new().expect("temporary repository");
        let repository = temporary.path();
        git_stdout(repository, &["init", "--quiet"]);
        let script_dir = tempfile::TempDir::new().expect("large-list git directory");
        let fake_git = script_dir.path().join("git");
        let mut status = Vec::new();
        let mut list_script = String::from("#!/bin/sh\nif [ \"$1\" = \"ls-files\" ]; then\n");
        for index in 0..(WORKTREE_FINGERPRINT_MAX_FILES * 4) {
            let name = format!("short-{index:04}");
            status.extend_from_slice(b"?? ");
            status.extend_from_slice(name.as_bytes());
            status.push(0);
            list_script.push_str(&format!("printf '%s\\\\0' '{name}'\n"));
        }
        list_script.push_str("fi\n");
        fs::write(&fake_git, list_script).expect("large-list git");
        fs::set_permissions(&fake_git, fs::Permissions::from_mode(0o700))
            .expect("large-list git mode");

        let started = Instant::now();
        assert_eq!(
            worktree_material_fingerprint_with_git(
                repository,
                &status,
                &fake_git,
                Duration::from_secs(2),
            ),
            None,
            "the file-count cap must reject before nonexistent paths are opened"
        );
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "well-over-cap untracked sets must reject in bounded linear time"
        );
    }

    #[test]
    fn worktree_fingerprint_ls_files_timeout_does_not_wait_for_reaping() {
        let _fixture_ownership = GlobalStateLock::new();
        let temporary = tempfile::TempDir::new().expect("temporary repository");
        let repository = temporary.path();
        git_stdout(repository, &["init", "--quiet"]);
        let script_dir = tempfile::TempDir::new().expect("stalled git directory");
        let fake_git = script_dir.path().join("git");
        fs::write(
            &fake_git,
            "#!/bin/sh\nif [ \"$1\" = \"ls-files\" ]; then sleep 10; fi\n",
        )
        .expect("stalled git");
        fs::set_permissions(&fake_git, fs::Permissions::from_mode(0o700))
            .expect("stalled git mode");
        let idle_deadline = Instant::now() + Duration::from_secs(1);
        while ACTIVE_FINGERPRINT_PROCESSES.load(Ordering::Acquire) != 0
            && Instant::now() < idle_deadline
        {
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(ACTIVE_FINGERPRINT_PROCESSES.load(Ordering::Acquire), 0);
        stall_next_fingerprint_reap_for_test(Duration::from_millis(250));
        let started = Instant::now();
        assert_eq!(
            worktree_material_fingerprint_with_git(
                repository,
                b"?? pending\0",
                &fake_git,
                Duration::from_millis(25),
            ),
            None
        );
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "ls-files evidence must return at its deadline even when reaping remains deferred"
        );
        assert_eq!(
            ACTIVE_FINGERPRINT_PROCESSES.load(Ordering::Acquire),
            1,
            "the delayed child must retain its admission permit until reaped"
        );
        assert!(
            !fingerprint_reaper_queue()
                .expect("fingerprint reaper")
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .is_empty(),
            "the delayed ls-files child must be owned by the bounded reaper"
        );
        let reaped_deadline = Instant::now() + Duration::from_secs(1);
        while ACTIVE_FINGERPRINT_PROCESSES.load(Ordering::Acquire) != 0
            && Instant::now() < reaped_deadline
        {
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            ACTIVE_FINGERPRINT_PROCESSES.load(Ordering::Acquire),
            0,
            "the deferred reaper must eventually release admission"
        );
    }

    #[test]
    fn worktree_fingerprint_hashes_1024_small_files_without_per_file_processes() {
        let temporary = tempfile::TempDir::new().expect("temporary repository");
        let repository = temporary.path();
        git_stdout(repository, &["init", "--quiet"]);
        for index in 0..WORKTREE_FINGERPRINT_MAX_FILES {
            fs::write(repository.join(format!("small-{index:04}.txt")), b"x")
                .expect("small untracked file");
        }
        let status = fingerprint_status(repository);
        reset_fingerprint_subprocess_launches_for_test();
        let started = Instant::now();
        let fingerprint = worktree_material_fingerprint(repository, &status);
        let launches = fingerprint_subprocess_launches_for_test();
        assert!(
            fingerprint.is_some(),
            "the maximum supported small-file set must finish inside the evidence deadline"
        );
        assert!(
            started.elapsed() < WORKTREE_STATUS_TIMEOUT,
            "the small-file aggregate must honor the evidence deadline"
        );
        assert!(
            launches <= 3,
            "fingerprinting may launch only the two diff commands and one ls-files command, got {launches}"
        );
    }

    #[test]
    fn worktree_fingerprint_stalled_regular_file_read_respects_caller_deadline() {
        let _fixture_ownership = GlobalStateLock::new();
        let temporary = tempfile::TempDir::new().expect("temporary repository");
        let repository = temporary.path();
        git_stdout(repository, &["init", "--quiet"]);
        fs::write(repository.join("blocked.txt"), b"blocked").expect("untracked file");
        let script_dir = tempfile::TempDir::new().expect("file-reader git directory");
        let fake_git = script_dir.path().join("git");
        fs::write(
            &fake_git,
            "#!/bin/sh\nif [ \"$1\" = \"ls-files\" ]; then printf 'blocked.txt\\0'; fi\n",
        )
        .expect("file-reader git");
        fs::set_permissions(&fake_git, fs::Permissions::from_mode(0o700))
            .expect("file-reader git mode");

        stall_next_fingerprint_file_read_for_test(Duration::from_millis(250));
        let started = Instant::now();
        assert_eq!(
            worktree_material_fingerprint_with_git(
                repository,
                b"?? blocked.txt\0",
                &fake_git,
                Duration::from_millis(50),
            ),
            None
        );
        assert!(
            started.elapsed() < Duration::from_millis(150),
            "a stalled regular-file read must not hold supervision past its deadline"
        );
        assert_eq!(
            ACTIVE_FINGERPRINT_FILE_READERS.load(Ordering::Acquire),
            1,
            "a timed-out reader retains bounded admission until its descriptor work ends"
        );
        let release_deadline = Instant::now() + Duration::from_secs(1);
        while ACTIVE_FINGERPRINT_FILE_READERS.load(Ordering::Acquire) != 0
            && Instant::now() < release_deadline
        {
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            ACTIVE_FINGERPRINT_FILE_READERS.load(Ordering::Acquire),
            0,
            "the delayed reader must eventually release bounded admission"
        );
    }

    #[test]
    fn worktree_fingerprint_fails_closed_for_oversize_special_path_escape_and_timeout_inputs() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::symlink;

        let temporary = tempfile::TempDir::new().expect("temporary repository");
        let repository = temporary.path();
        git_stdout(repository, &["init", "--quiet"]);
        fs::write(
            repository.join("oversize.bin"),
            vec![b'x'; WORKTREE_FINGERPRINT_MAX_BYTES + 1],
        )
        .expect("oversize fixture");
        let status = fingerprint_status(repository);
        assert_eq!(
            worktree_material_fingerprint(repository, &status),
            None,
            "bounded diagnostics must fail closed rather than hash unbounded material"
        );

        fs::remove_file(repository.join("oversize.bin")).expect("remove oversize fixture");
        let fifo = repository.join("special.fifo");
        let fifo_c = CString::new(fifo.as_os_str().as_bytes()).expect("fifo path");
        assert_eq!(
            unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) },
            0,
            "create special-file fixture"
        );
        let special_script_dir = tempfile::TempDir::new().expect("special git directory");
        let special_git = special_script_dir.path().join("git");
        fs::write(
            &special_git,
            "#!/bin/sh\nif [ \"$1\" = \"ls-files\" ]; then printf 'special.fifo\\0'; fi\n",
        )
        .expect("special git");
        fs::set_permissions(&special_git, fs::Permissions::from_mode(0o700))
            .expect("special git mode");
        let special_started = Instant::now();
        assert_eq!(
            worktree_material_fingerprint_with_git(
                repository,
                b"?? special.fifo\0",
                &special_git,
                Duration::from_secs(1)
            ),
            None,
            "non-regular untracked material must be unavailable"
        );
        assert!(
            special_started.elapsed() < Duration::from_millis(500),
            "opening a FIFO must remain nonblocking"
        );

        let outside = tempfile::TempDir::new().expect("outside directory");
        fs::write(outside.path().join("secret"), "outside\n").expect("outside fixture");
        symlink(outside.path(), repository.join("escape")).expect("parent symlink fixture");
        fs::write(
            &special_git,
            "#!/bin/sh\nif [ \"$1\" = \"ls-files\" ]; then printf 'escape/secret\\0'; fi\n",
        )
        .expect("parent symlink git");
        assert_eq!(
            worktree_material_fingerprint_with_git(
                repository,
                b"?? escape/secret\0",
                &special_git,
                Duration::from_secs(1)
            ),
            None,
            "untracked paths must not traverse a symlinked parent outside the checkout"
        );

        let script_dir = tempfile::TempDir::new().expect("fake git directory");
        let fake_git = script_dir.path().join("git");
        fs::write(&fake_git, "#!/bin/sh\nsleep 1\n").expect("fake git");
        fs::set_permissions(&fake_git, fs::Permissions::from_mode(0o700)).expect("fake git mode");
        assert_eq!(
            worktree_material_fingerprint_with_git(
                repository,
                b"?? bounded\0",
                &fake_git,
                Duration::from_millis(25)
            ),
            None,
            "a bounded subprocess timeout must make the evidence unavailable"
        );

        let reader_input = repository.join("reader-input.bin");
        fs::write(&reader_input, b"bounded input").expect("reader input");
        let mut reader_file = fs::File::open(&reader_input).expect("reader input descriptor");
        let reader_started = Instant::now();
        assert_eq!(
            stream_file_digest(&mut reader_file, 1, Duration::from_millis(100),),
            None,
            "regular-file hashing must enforce its exact byte cap in-process"
        );
        assert!(
            reader_started.elapsed() < Duration::from_secs(1),
            "regular-file hashing must remain bounded without a helper process"
        );

        #[cfg(target_os = "linux")]
        {
            let escaped_script_dir =
                tempfile::TempDir::new().expect("escaped descendant git directory");
            let escaped_git = escaped_script_dir.path().join("git");
            let escaped_pid = escaped_script_dir.path().join("escaped.pid");
            let escaped_ready = escaped_script_dir.path().join("escaped.ready");
            fs::write(
                &escaped_git,
                format!(
                    "#!/bin/sh\nsetsid sh -c 'echo $$ > \"{}\"; touch \"{}\"; sleep 10' &\nexit 0\n",
                    escaped_pid.display(),
                    escaped_ready.display()
                ),
            )
            .expect("escaped descendant git");
            fs::set_permissions(&escaped_git, fs::Permissions::from_mode(0o700))
                .expect("escaped descendant git mode");
            let mut escaped_command = Command::new(&escaped_git);
            escaped_command.current_dir(repository).arg("diff");
            let started = Instant::now();
            assert_eq!(
                stream_command_digest(escaped_command, Duration::from_millis(25)),
                None,
                "an escaped stdout holder must not make fingerprint timeout unbounded"
            );
            assert!(
                started.elapsed() < Duration::from_millis(500),
                "an escaped stdout holder must not outlive the bounded fingerprint call"
            );
            let ready_deadline = Instant::now() + Duration::from_millis(500);
            while !escaped_ready.is_file() && Instant::now() < ready_deadline {
                thread::sleep(Duration::from_millis(10));
            }
            assert!(
                escaped_ready.is_file(),
                "the fixture must prove that a detached stdout holder started"
            );
            let pid = fs::read_to_string(&escaped_pid)
                .expect("escaped descendant pid")
                .trim()
                .parse::<libc::pid_t>()
                .expect("escaped descendant numeric pid");
            assert_eq!(
                unsafe { libc::kill(pid, libc::SIGKILL) },
                0,
                "clean up the proven escaped stdout holder"
            );

            let grouped_script_dir =
                tempfile::TempDir::new().expect("same-group descendant git directory");
            let grouped_git = grouped_script_dir.path().join("git");
            let grouped_pid = grouped_script_dir.path().join("grouped.pid");
            let grouped_ready = grouped_script_dir.path().join("grouped.ready");
            fs::write(
                &grouped_git,
                format!(
                    "#!/bin/sh\nsh -c 'echo $$ > \"{}\"; touch \"{}\"; sleep 10' &\nwhile [ ! -f \"{}\" ]; do :; done\nexit 0\n",
                    grouped_pid.display(),
                    grouped_ready.display(),
                    grouped_ready.display()
                ),
            )
            .expect("same-group descendant git");
            fs::set_permissions(&grouped_git, fs::Permissions::from_mode(0o700))
                .expect("same-group descendant git mode");
            let mut grouped_command = Command::new(&grouped_git);
            grouped_command.current_dir(repository).arg("diff");
            let started = Instant::now();
            assert_eq!(
                stream_command_digest(grouped_command, Duration::from_millis(25)),
                None,
                "a same-group stdout holder must time out"
            );
            assert!(
                started.elapsed() < Duration::from_millis(500),
                "same-group cleanup must remain bounded"
            );
            assert!(
                grouped_ready.is_file(),
                "the fixture must prove that a same-group stdout holder started"
            );
            let pid = fs::read_to_string(&grouped_pid)
                .expect("same-group descendant pid")
                .trim()
                .parse::<libc::pid_t>()
                .expect("same-group descendant numeric pid");
            let gone_deadline = Instant::now() + Duration::from_millis(500);
            while unsafe { libc::kill(pid, 0) } == 0 && Instant::now() < gone_deadline {
                thread::sleep(Duration::from_millis(10));
            }
            assert_eq!(
                unsafe { libc::kill(pid, 0) },
                -1,
                "same-group timeout must terminate the stdout-holding descendant"
            );
        }

        let streaming_script_dir =
            tempfile::TempDir::new().expect("continuous output git directory");
        let streaming_git = streaming_script_dir.path().join("git");
        fs::write(&streaming_git, "#!/bin/sh\nexec yes 0123456789abcdef\n")
            .expect("continuous output git");
        fs::set_permissions(&streaming_git, fs::Permissions::from_mode(0o700))
            .expect("continuous output git mode");
        let mut streaming_command = Command::new(&streaming_git);
        streaming_command.current_dir(repository).arg("diff");
        let started = Instant::now();
        assert_eq!(
            stream_command_digest(streaming_command, Duration::from_millis(25)),
            None,
            "continuous output must not starve the fingerprint deadline"
        );
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "continuous output must remain inside the bounded fingerprint call"
        );
    }

    #[test]
    fn fingerprint_reaper_caps_admission_and_releases_permits_after_later_reap() {
        static TEST_ACTIVE: AtomicUsize = AtomicUsize::new(0);

        assert_eq!(TEST_ACTIVE.load(Ordering::Acquire), 0);
        let reaper = Arc::new(Mutex::new(Vec::new()));
        for _ in 0..WORKTREE_FINGERPRINT_MAX_REAPERS {
            let permit = FingerprintProcessPermit::acquire_from(
                &TEST_ACTIVE,
                WORKTREE_FINGERPRINT_MAX_REAPERS,
            )
            .expect("admit bounded synthetic reap task");
            let child = Command::new("/bin/sleep")
                .arg("10")
                .spawn()
                .expect("synthetic unreaped child");
            enqueue_fingerprint_reap_task(&reaper, child, permit);
        }
        assert_eq!(
            TEST_ACTIVE.load(Ordering::Acquire),
            WORKTREE_FINGERPRINT_MAX_REAPERS
        );
        assert!(
            FingerprintProcessPermit::acquire_from(&TEST_ACTIVE, WORKTREE_FINGERPRINT_MAX_REAPERS,)
                .is_none(),
            "the ninth stalled fingerprint child must fail admission without spawning"
        );

        {
            let mut tasks = reaper
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            tasks[0]
                .child
                .kill()
                .expect("make one queued child reapable");
        }
        let release_deadline = Instant::now() + Duration::from_secs(1);
        while TEST_ACTIVE.load(Ordering::Acquire) == WORKTREE_FINGERPRINT_MAX_REAPERS
            && Instant::now() < release_deadline
        {
            reap_fingerprint_tasks_once(&reaper);
            thread::sleep(Duration::from_millis(5));
        }
        let resumed =
            FingerprintProcessPermit::acquire_from(&TEST_ACTIVE, WORKTREE_FINGERPRINT_MAX_REAPERS)
                .expect("later reap must restore bounded admission");
        drop(resumed);

        {
            let mut tasks = reaper
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            for task in tasks.iter_mut() {
                let _ = task.child.kill();
            }
        }
        let cleanup_deadline = Instant::now() + Duration::from_secs(1);
        while TEST_ACTIVE.load(Ordering::Acquire) != 0 && Instant::now() < cleanup_deadline {
            reap_fingerprint_tasks_once(&reaper);
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            TEST_ACTIVE.load(Ordering::Acquire),
            0,
            "all synthetic reaper tasks must eventually release their permits"
        );
        assert!(
            reaper
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .is_empty(),
            "all completed synthetic children must be removed from the reaper queue"
        );
    }

    #[test]
    fn worktree_progress_snapshot_preserves_age_across_observations_and_process_contexts() {
        let temporary = tempfile::TempDir::new().expect("temporary state");
        let context = CliContext {
            state_dir: temporary.path().to_path_buf(),
            host: None,
        };
        let mut worker_record = cleanup_test_session("progress-worker", "progress-incarnation");
        fs::create_dir_all(session_dir(&context, &worker_record.id)).expect("session directory");
        crate::write_session_record(&context, &worker_record).expect("worker session");
        let mut assignment = dep_assignment("assignment-progress", "run-one", "working");
        assignment.worker = Some(session_ref(
            &context,
            &worker_record,
            "progress-incarnation",
        ));

        assert_eq!(
            persist_worktree_progress_snapshot(&context, &assignment, "sha256:first", 100)
                .expect("initial snapshot"),
            (100, 100)
        );
        let name = crate::coordination::digest_bytes(assignment.assignment_id.as_bytes());
        let snapshot_path = session_dir(&context, &worker_record.id)
            .join("coordination")
            .join(format!("main-agent-progress-{name}.json"));
        let initial_bytes = fs::read(&snapshot_path).expect("initial snapshot bytes");
        assert_eq!(
            persist_worktree_progress_snapshot(&context, &assignment, "sha256:first", 900)
                .expect("unchanged snapshot"),
            (100, 900),
            "unchanged dirty material retains the original progress time"
        );
        assert_eq!(
            fs::read(&snapshot_path).expect("unchanged snapshot bytes"),
            initial_bytes,
            "unchanged progress must not rewrite the durable observation"
        );

        let reconstructed_context = CliContext {
            state_dir: temporary.path().to_path_buf(),
            host: None,
        };
        assert_eq!(
            persist_worktree_progress_snapshot(
                &reconstructed_context,
                &assignment,
                "sha256:second",
                901
            )
            .expect("changed snapshot"),
            (901, 901),
            "a separate observation must advance time only for a new material fingerprint"
        );
        assert_eq!(
            persist_worktree_progress_snapshot(
                &reconstructed_context,
                &assignment,
                "sha256:second",
                1_800
            )
            .expect("persisted observation"),
            (901, 1_800),
            "the durable snapshot carries progress age across process contexts"
        );

        worker_record
            .runtime
            .as_mut()
            .expect("worker runtime")
            .launch_id = "progress-incarnation-two".to_string();
        crate::write_session_record(&context, &worker_record).expect("resumed worker session");
        assignment.worker = Some(session_ref(
            &context,
            &worker_record,
            "progress-incarnation-two",
        ));
        assert_eq!(
            persist_worktree_progress_snapshot(
                &reconstructed_context,
                &assignment,
                "sha256:second",
                1_801
            )
            .expect("resumed incarnation snapshot"),
            (1_801, 1_801),
            "a valid prior-incarnation snapshot resets progress time instead of becoming corruption"
        );
        assert_eq!(
            persist_worktree_progress_snapshot(
                &reconstructed_context,
                &assignment,
                "sha256:second",
                2_000
            )
            .expect("resumed incarnation observation"),
            (1_801, 2_000)
        );
    }

    #[test]
    fn controller_recovery_is_ownership_qualified_under_self() {
        let parsed = MainAgentCli::try_parse_from([
            "main-agent",
            "self",
            "recover",
            "--idempotency-key",
            "recover-main-001",
        ])
        .expect("ownership-qualified recovery parses");
        assert!(matches!(
            parsed.command,
            MainAgentCommand::SelfGroup(SelfGroupArgs {
                command: SelfCommand::Recover(_)
            })
        ));

        let error = MainAgentCli::try_parse_from([
            "main-agent",
            "recover",
            "--idempotency-key",
            "recover-main-001",
        ])
        .expect_err("ambiguous top-level recovery must not parse");
        assert_eq!(error.kind(), ErrorKind::InvalidSubcommand);
    }

    #[test]
    fn account_handoff_cancel_parser_requires_exact_selectors_with_released_v1_intent_exception() {
        let base = [
            "main-agent",
            "worker",
            "account-handoff-cancel",
            "assignment-one",
        ];
        for missing in ["reservation", "account"] {
            let mut args = base.to_vec();
            if missing != "reservation" {
                args.extend(["--reservation-id", "reservation-one"]);
            }
            if missing != "account" {
                args.extend(["--account", "alpha"]);
            }
            args.extend([
                "--if-revision",
                "7",
                "--authorize-account-change",
                "--idempotency-key",
                "cancel-one",
            ]);
            let error = MainAgentCli::try_parse_from(args)
                .expect_err("required cancellation selector must fail closed");
            assert_eq!(error.kind(), ErrorKind::MissingRequiredArgument);
        }

        MainAgentCli::try_parse_from([
            "main-agent",
            "worker",
            "account-handoff-cancel",
            "assignment-one",
            "--reservation-id",
            "released-v1-reservation-one",
            "--account",
            "alpha",
            "--if-revision",
            "7",
            "--authorize-account-change",
            "--idempotency-key",
            "released-v1-cancel-one",
        ])
        .expect("frozen released-v1 reservation may omit only --intent-id");
    }

    #[test]
    fn claim_renewal_remains_distinct_from_edit_authority_freshness() {
        assert!(!worker_claim_renewal_required(
            "working",
            true,
            true,
            Some(30 * 60)
        ));
        assert!(worker_claim_renewal_required("working", false, true, None));
        assert!(!worker_claim_renewal_required(
            "accepted",
            true,
            true,
            Some(30 * 60)
        ));
        assert!(
            !worker_claim_renewal_required("submitted", false, false, Some(0)),
            "submitted work is terminal from the worker's perspective and must never renew its claim"
        );
    }

    /// Neutral supervision facts: every signal absent, so a single flipped
    /// field isolates exactly one classification.
    fn base_diagnosis_facts() -> WorkerDiagnosisFacts {
        WorkerDiagnosisFacts {
            evidence_unavailable: false,
            worker_unreachable: false,
            active_or_uncertain_operation: false,
            coordination_broker_stale: false,
            edit_authority_stale: false,
            claim_renewal_required: false,
            orphan_guidance_quarantine_required: false,
            guidance_continuity_required: false,
            startup_dialog: false,
            account_handoff_capability_gap: false,
            account_handoff_required: false,
            provider_activity_stale: false,
            unread_guidance: false,
            preclaim_blocker: false,
            runtime_gone_preclaim: false,
            terminal_recovery_reconciled: false,
            starting_provider_terminated: false,
            terminal_quiescent: false,
            submitted: false,
            reassignment_safe: false,
        }
    }

    /// A worker whose provider runtime exits during startup never reaches
    /// `main-agent bootstrap`, so it records no turn and holds no claim. It must
    /// still be recognised as a pre-claim failure: otherwise supervision reports
    /// `claim_renewal_required` and asks the dead worker to renew a claim it
    /// never held, which leaves `worker cancel` and `worker reassign` refusing
    /// the assignment and whole-run force cleanup as the only recovery.
    #[test]
    fn stopped_worker_on_a_starting_assignment_is_a_preclaim_failure() {
        let startup_exited = PreClaimEvidence {
            assignment_state: "starting",
            claim_active: false,
            operations_quiescent: true,
            worker_bound: true,
            worker_status: "stopped",
            // The provider died before its first turn, so activity-derived
            // termination is not observable.
            provider_terminated: false,
            preclaim_blocker: false,
            terminal_recovery_reconciled: false,
        };
        assert!(
            worker_failed_preclaim(startup_exited),
            "a bound worker whose runtime is gone while the assignment is still `starting` never acquired a claim"
        );

        // The verdict must reach the classifier, not just the safety fields.
        // Without this the supervisor reports `healthy_progress` and tells the
        // Main Agent that a dead worker needs no intervention.
        let classified = classify_worker_diagnosis(WorkerDiagnosisFacts {
            runtime_gone_preclaim: true,
            reassignment_safe: true,
            ..base_diagnosis_facts()
        });
        assert_eq!(
            classified.0, "pre_claim_failure",
            "a pre-claim failure must never be reported as healthy progress"
        );
        assert!(
            classified.1.contains("reassign"),
            "the next action must route to reassign/cancel, got: {}",
            classified.1
        );
        assert!(classified.2, "cancel and reassign are safe from this state");

        // An authoritative terminal turn keeps its own classification: it has
        // turn evidence, so it is not the blind runtime-gone case.
        assert_eq!(
            classify_worker_diagnosis(WorkerDiagnosisFacts {
                starting_provider_terminated: true,
                reassignment_safe: true,
                ..base_diagnosis_facts()
            })
            .0,
            "submitted_or_waiting_without_checkpoint",
            "provider-turn evidence must not be reclassified as a blind runtime-gone failure"
        );

        assert!(
            !worker_failed_preclaim(PreClaimEvidence {
                worker_status: "running",
                ..startup_exited
            }),
            "a live worker that has simply not checkpointed yet is not a pre-claim failure"
        );
        assert!(
            !worker_failed_preclaim(PreClaimEvidence {
                assignment_state: "working",
                ..startup_exited
            }),
            "a `working` assignment already proved bootstrap and its claim"
        );
        assert!(
            !worker_failed_preclaim(PreClaimEvidence {
                claim_active: true,
                ..startup_exited
            }),
            "an active claim must never be cancelled as a pre-claim failure"
        );
        assert!(
            !worker_failed_preclaim(PreClaimEvidence {
                operations_quiescent: false,
                ..startup_exited
            }),
            "an in-flight or uncertain operation must dominate pre-claim cancellation"
        );
    }

    #[test]
    fn worker_supervision_classifications_have_deterministic_precedence() {
        let base = base_diagnosis_facts();
        assert_eq!(classify_worker_diagnosis(base).0, "healthy_progress");
        assert_eq!(
            classify_worker_diagnosis(WorkerDiagnosisFacts {
                evidence_unavailable: true,
                ..base
            })
            .0,
            "evidence_unavailable"
        );
        assert_eq!(
            classify_worker_diagnosis(WorkerDiagnosisFacts {
                worker_unreachable: true,
                ..base
            })
            .0,
            "worker_unreachable"
        );
        let broker_stale = classify_worker_diagnosis(WorkerDiagnosisFacts {
            coordination_broker_stale: true,
            edit_authority_stale: true,
            claim_renewal_required: true,
            ..base
        });
        assert_eq!(broker_stale.0, "coordination_broker_stale");
        assert!(!broker_stale.1.contains("work-context renew"));
        let edit_stale = classify_worker_diagnosis(WorkerDiagnosisFacts {
            edit_authority_stale: true,
            claim_renewal_required: true,
            ..base
        });
        assert_eq!(edit_stale.0, "edit_authority_stale");
        assert!(!edit_stale.1.contains("work-context renew"));
        assert_eq!(
            classify_worker_diagnosis(WorkerDiagnosisFacts {
                claim_renewal_required: true,
                guidance_continuity_required: true,
                account_handoff_required: true,
                ..base
            })
            .0,
            "claim_renewal_required"
        );
        assert_eq!(
            classify_worker_diagnosis(WorkerDiagnosisFacts {
                orphan_guidance_quarantine_required: true,
                guidance_continuity_required: true,
                account_handoff_required: true,
                ..base
            })
            .0,
            "orphan_guidance_quarantine_required"
        );
        assert_eq!(
            classify_worker_diagnosis(WorkerDiagnosisFacts {
                guidance_continuity_required: true,
                account_handoff_required: true,
                ..base
            })
            .0,
            "guidance_continuity_required"
        );
        assert_eq!(
            classify_worker_diagnosis(WorkerDiagnosisFacts {
                account_handoff_capability_gap: true,
                account_handoff_required: true,
                ..base
            })
            .0,
            "account_handoff_capability_gap"
        );
        assert_eq!(
            classify_worker_diagnosis(WorkerDiagnosisFacts {
                account_handoff_required: true,
                startup_dialog: true,
                ..base
            })
            .0,
            "account_handoff_required"
        );
        assert_eq!(
            classify_worker_diagnosis(WorkerDiagnosisFacts {
                startup_dialog: true,
                ..base
            })
            .0,
            "startup_dialog_failure"
        );
        assert_eq!(
            classify_worker_diagnosis(WorkerDiagnosisFacts {
                provider_activity_stale: true,
                unread_guidance: true,
                ..base
            })
            .0,
            "stale_provider_activity"
        );
        assert_eq!(
            classify_worker_diagnosis(WorkerDiagnosisFacts {
                preclaim_blocker: true,
                reassignment_safe: true,
                ..base
            })
            .0,
            "pre_claim_failure"
        );
        assert_eq!(
            classify_worker_diagnosis(WorkerDiagnosisFacts {
                terminal_recovery_reconciled: true,
                reassignment_safe: true,
                ..base
            })
            .0,
            "pre_claim_failure"
        );
        assert_eq!(
            classify_worker_diagnosis(WorkerDiagnosisFacts {
                starting_provider_terminated: true,
                ..base
            })
            .0,
            "submitted_or_waiting_without_checkpoint"
        );
        assert_eq!(
            classify_worker_diagnosis(WorkerDiagnosisFacts {
                terminal_quiescent: true,
                ..base
            })
            .0,
            "safe_reassignment"
        );
        assert_eq!(
            classify_worker_diagnosis(WorkerDiagnosisFacts {
                active_or_uncertain_operation: true,
                startup_dialog: true,
                preclaim_blocker: true,
                ..base
            })
            .0,
            "uncertain_mutation",
            "uncertain mutation must dominate every less-safe classification"
        );
    }

    #[test]
    fn every_supervision_classification_projects_a_typed_bounded_action() {
        let mut assignment = dep_assignment("typed-action", "run-one", "working");
        assignment.worker = Some(SessionRef {
            machine: None,
            session_id: "worker-one".to_string(),
            session_incarnation: "worker-incarnation-one".to_string(),
            session_created_at: "2030-01-01T00:00:00Z".to_string(),
        });
        for classification in [
            "evidence_unavailable",
            "worker_unreachable",
            "uncertain_mutation",
            "coordination_broker_stale",
            "edit_authority_stale",
            "claim_renewal_required",
            "orphan_guidance_quarantine_required",
            "guidance_continuity_required",
            "account_handoff_capability_gap",
            "account_handoff_required",
            "startup_dialog_failure",
            "stale_provider_activity",
            "pre_claim_failure",
            "submitted_or_waiting_without_checkpoint",
            "safe_reassignment",
            "healthy_progress",
        ] {
            let action = worker_recovery_action(
                classification,
                &assignment,
                Some("worker-claim-one"),
                Some(7),
            );
            assert_eq!(
                action["schema_version"], "main-agent.worker-recovery-action.v1",
                "classification={classification}"
            );
            assert_eq!(action["classification"], classification);
            assert!(action["owner"]["role"].is_string());
            assert!(
                action["argv"].is_array() || action["argv_template"].is_array(),
                "classification={classification} action={action}"
            );
            let rendered = action.to_string();
            for forbidden in [
                "capability_file",
                "capability_path",
                "provider-account-nickname",
                "private_packet_digest",
                "worktree",
            ] {
                assert!(
                    !rendered.contains(forbidden),
                    "classification={classification} leaked {forbidden}: {rendered}"
                );
            }
        }
        let renewal = worker_recovery_action(
            "claim_renewal_required",
            &assignment,
            Some("worker-claim-one"),
            Some(7),
        );
        assert_eq!(renewal["owner"]["session_id"], "worker-one");
        assert_eq!(renewal["claim_id"], "worker-claim-one");
        assert_eq!(renewal["claim_revision"], 7);
        assert_eq!(
            renewal["argv_template"],
            json!([
                "agent-session",
                "work-context",
                "renew",
                "--session",
                "worker-one",
                "--claim",
                "worker-claim-one",
                "--if-revision",
                "7",
                "--idempotency-key",
                "<idempotency-key>",
                "--format",
                "json"
            ])
        );
    }

    #[test]
    fn failed_macro_exposes_its_last_proven_safe_state() {
        let value = macro_failure(
            "main-agent.worker-reassign-result.v1",
            "retire",
            json!({
                "assignment_id": "failed-worker",
                "state": "cancelled",
                "claim_absent": true,
                "operation_quiescent": true
            }),
            CliError::runtime("delete-failed", "delete failed", None),
        );
        assert_eq!(value["state"], "failed");
        assert_eq!(value["failed_stage"], "retire");
        assert_eq!(value["automatic_retry_safe"], false);
        assert_eq!(value["last_proven_safe_state"]["state"], "cancelled");
        assert_eq!(value["error"]["code"], "delete-failed");
    }

    #[test]
    fn terminal_quiescent_reassignment_skips_the_ineligible_cancel_stage() {
        let diagnosis = json!({
            "assignment_revision": 9,
            "assignment_state": "released",
            "cancel_then_reassign_safe": false,
            "new_assignment_safe": true
        });
        let (cancel_safe, cancel_step) = reassign_cancel_step(&diagnosis, "terminal-assignment");
        assert!(!cancel_safe);
        assert_eq!(cancel_step["skipped"], true);
        assert_eq!(cancel_step["assignment"]["revision"], 9);
        assert_eq!(cancel_step["assignment"]["state"], "released");
    }

    #[test]
    fn default_quick_idempotency_key_is_stable_and_slug_valid() {
        let input = AssignmentInput {
            schema_version: ASSIGNMENT_INPUT_SCHEMA.to_string(),
            assignment_id: None,
            task_summary: "demo task".to_string(),
            task: json!({}),
            launch: WorkerLaunchInput {
                agent: "codex".to_string(),
                cwd: "/repo".to_string(),
                title: None,
                session_id: None,
                coordination_mode: CoordinationMode::default(),
                agent_args: Vec::new(),
            },
            repository: Some("owner/name".to_string()),
            worktree: None,
            base_ref: None,
            scopes: Vec::new(),
            durable_refs: Vec::new(),
            depends_on: Vec::new(),
        };
        let key = default_quick_idempotency_key(&input);
        assert!(key.starts_with("quick-"), "key = {key}");
        assert_eq!(key.len(), 38, "quick- prefix plus 32 hex digits");
        assert!(
            validate_idempotency_key(&key).is_ok(),
            "derived key must satisfy the idempotency-key rule: {key}"
        );
        // Deterministic: identical packets map to the same key so replay stays
        // idempotent without a caller-supplied key.
        assert_eq!(key, default_quick_idempotency_key(&input));
        // A materially different packet maps to a different key.
        let mut other = input.clone();
        other.task_summary = "different task".to_string();
        assert_ne!(key, default_quick_idempotency_key(&other));
    }

    #[test]
    fn parse_await_ready_treats_zero_as_launch_only() {
        assert!(parse_await_ready("0").unwrap().is_none());
        assert!(parse_await_ready("0s").unwrap().is_none());
        assert_eq!(
            parse_await_ready("5s").unwrap(),
            Some(Duration::from_secs(5))
        );
        assert_eq!(
            parse_await_ready("5m").unwrap(),
            Some(Duration::from_secs(300))
        );
        assert_eq!(
            parse_await_ready("301").unwrap_err().code(),
            "worker-await-ready-timeout"
        );
        assert_eq!(
            parse_await_ready("abc").unwrap_err().code(),
            "invalid-duration"
        );
    }

    #[test]
    fn quick_defaults_to_awaiting_worker_readiness() {
        // The fast path used to hardcode launch-only, so its result could never
        // carry a readiness proof and the runtime-owned single-Enter recovery
        // never ran for it. A dropped submit key then became something the
        // caller had to notice and hand-repair.
        let cli = MainAgentCli::try_parse_from([
            "main-agent",
            "quick",
            "--assignment-file",
            "packet.json",
        ])
        .expect("quick parses without an explicit await");
        let MainAgentCommand::Quick(args) = cli.command else {
            panic!("expected the quick subcommand");
        };
        assert_eq!(
            parse_await_ready(&args.await_ready).expect("default await duration"),
            Some(Duration::from_secs(300))
        );
    }

    #[test]
    fn worker_start_defaults_to_bounded_readiness() {
        let cli = MainAgentCli::try_parse_from([
            "main-agent",
            "worker",
            "start",
            "--assignment-file",
            "packet.json",
            "--idempotency-key",
            "worker-start-default-readiness-0001",
        ])
        .expect("worker start parses without an explicit await");
        let MainAgentCommand::Worker(WorkerArgs {
            command: WorkerCommand::Start(args),
        }) = cli.command
        else {
            panic!("expected the worker start subcommand");
        };
        assert_eq!(
            parse_await_ready(&args.await_ready).expect("default await duration"),
            Some(Duration::from_secs(300)),
            "omitting --await-ready must preserve the released readiness guarantee"
        );
        assert_eq!(
            worker_start_polling_evidence(None),
            json!({
                "mode": "launch_only",
                "readiness_registry_read_bound": 0,
                "readiness_registry_write_bound": 0
            })
        );
        assert_eq!(
            worker_start_polling_evidence(Some(Duration::from_secs(300))),
            json!({
                "mode": "bounded_wait",
                "timeout_seconds": 300,
                "readiness_registry_read_bound": 1201,
                "readiness_registry_write_bound": 65
            }),
            "an explicit five-minute wait has finite documented registry I/O bounds"
        );
    }

    #[test]
    fn quick_still_allows_an_explicit_launch_only_wait() {
        let cli = MainAgentCli::try_parse_from([
            "main-agent",
            "quick",
            "--assignment-file",
            "packet.json",
            "--await-ready",
            "0",
        ])
        .expect("quick parses an explicit zero");
        let MainAgentCommand::Quick(args) = cli.command else {
            panic!("expected the quick subcommand");
        };
        assert!(
            parse_await_ready(&args.await_ready)
                .expect("explicit zero")
                .is_none()
        );
    }

    #[test]
    fn worker_start_prompt_requires_deterministic_bootstrap() {
        let prompt = worker_start_prompt(
            "assignment-one",
            std::path::Path::new("/release path/main-agent"),
        );
        assert!(prompt.contains("'/release path/main-agent' bootstrap"));
        assert!(prompt.contains(" bootstrap "));
        assert!(prompt.contains("--idempotency-key bootstrap-"));
        assert!(prompt.contains("--format json"));
        assert!(prompt.contains("release your work-context claim"));
        assert!(!prompt.contains("then checkpoint"));
    }
}
