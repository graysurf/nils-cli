use std::env;
use std::ffi::OsString;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
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

use crate::cli::{self, AgentKind, CoordinationMode};
use crate::coordination::context::{
    Scope, ScopeKind, WORK_CONTEXT_INPUT_VERSION, WorkContextInput,
};
use crate::orchestration::{
    self, ASSIGNMENT_INPUT_SCHEMA, ASSIGNMENT_SCHEMA, AssignmentRecord, CHECKPOINT_INPUT_SCHEMA,
    IdempotencyReceipt, PACKET_SCHEMA, RunCheckpoint, RunRecord, SessionRef, TimedRelationship,
};
use crate::{
    CliContext, CliError, SessionRecord, SessionRegistryFence, StartFailureDisposition,
    delete_session, load_session_record, resolve_tmux_bin, session_dir, session_status,
    start_session,
};

const BINARY: &str = "main-agent";
const IDEMPOTENCY_KEY_HELP: &str = "Retry an ambiguous outcome with the same idempotency key and the same logical request; use a new key for a changed request.";
const ASSIGNMENT_REVISION_HELP: &str =
    "Expected current assignment revision; stale values fail closed and report current_revision.";
const RUN_REVISION_HELP: &str =
    "Expected current run revision; stale values fail closed and report current_revision.";
const WORKER_START_RUN_REVISION_HELP: &str = "Optional expected run revision. Omit to launch without a run-revision fence: assignment creation is decoupled from the run revision, so parallel and batch starts no longer collide. When supplied, a stale value fails closed and reports current_revision.";
const QUICK_IDEMPOTENCY_KEY_HELP: &str = "Optional for the fast-path: omit to derive a stable idempotency key from a digest of the assignment packet, or supply one to control replay explicitly. 8-128 printable non-space ASCII bytes.";
const MAIN_AGENT_AFTER_HELP: &str = "SAFE LIFECYCLE:\n  init -> rehydrate/status -> worker start --await-ready -> worker bootstrap\n  submit/release claim -> accept -> retire -> close\n\nREVISION AND RETRY RULES:\n  Read the current run or assignment revision before each mutation. Retry an\n  ambiguous outcome with the identical request and idempotency key. After a\n  confirmed revision conflict, re-read state and use a new key for the revised\n  request.\n\nEXAMPLES:\n  main-agent init --packet-file objective.json --if-absent --idempotency-key init-001 --format json\n  main-agent rehydrate --format markdown\n  main-agent worker start --assignment-file assignment.json --if-run-revision 1 --await-ready 5m --idempotency-key start-001 --format json\n  main-agent worker accept ASSIGNMENT_ID --if-revision 4 --idempotency-key accept-001 --format json\n\nOPERATOR RUNBOOK:\n  crates/agent-session/docs/runbooks/main-agent-orchestration.md\n\nEXIT CODES:\n  0   success\n  1   runtime error\n  64  command-line usage error\n  65  invalid or stale data\n  69  temporarily unavailable";

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
    /// Accept a submitted worker result after Main Agent review.
    Accept(AssignmentMutationArgs),
    /// Mark an accepted assignment terminal before worker deletion.
    Release(AssignmentMutationArgs),
    /// Delete a released worker through guarded agent-session cleanup.
    Delete(AssignmentMutationArgs),
    /// Retire an accepted assignment in one call: release -> delete -> confirm
    /// the worker is absent from a fresh list.
    Retire(AssignmentMutationArgs),
}

#[derive(Clone, Debug, Args)]
struct WorkerStartArgs {
    /// Private assignment packet JSON file (single-lane launch).
    #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath, conflicts_with = "batch")]
    assignment_file: Option<PathBuf>,
    /// Directory of assignment packet JSON files to launch as one batch. Each
    /// lane is fenced independently (T2 decouple), so one lane failing isolates
    /// to its own typed result rather than aborting the batch.
    #[arg(long, value_name = "DIR", value_hint = ValueHint::DirPath)]
    batch: Option<PathBuf>,
    #[arg(long, help = WORKER_START_RUN_REVISION_HELP)]
    if_run_revision: Option<u64>,
    /// After launch, wait up to this bounded duration (0 = no wait) for the
    /// worker's authenticated checkpoint to advance the assignment past
    /// `starting`. This folds the readiness + newer-turn + identity proof into
    /// worker start's typed result. 0-5m (integer with optional s/m/h suffix).
    #[arg(long, default_value = "0")]
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

#[derive(Clone, Debug, Serialize)]
pub(crate) struct GroupCleanupWorkerPlan {
    assignment_id: String,
    state: String,
    worker: Option<SessionRef>,
    force_required: bool,
    primary_managed: bool,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct GroupCleanupPlan {
    schema_version: &'static str,
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

#[derive(Clone, Debug, Args)]
struct QuickArgs {
    /// Private assignment packet JSON file.
    #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
    assignment_file: PathBuf,
    /// Work tier for the synthesized ephemeral run (L0/L1 delegate-all).
    #[arg(long, default_value = "L0")]
    tier: String,
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
    let result = retry_transient_store(|| run_command(&context, &cli.command));
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
/// facade can re-run it under [`retry_transient_store`] without duplicating the
/// command match. Each handler still owns its own claim, revision, and
/// idempotency fences, so a re-run converges through those rather than
/// duplicating an effect.
fn run_command(context: &CliContext, command: &MainAgentCommand) -> Result<Value, CliError> {
    match command {
        MainAgentCommand::Init(args) => run_init(context, args.clone()),
        MainAgentCommand::Rebind(args) => run_rebind(context, args.clone()),
        MainAgentCommand::SelfGroup(args) => match &args.command {
            SelfCommand::Show(args) => run_self_show(context, args.clone()),
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
        run.controller.session_id == record.id
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
            ensure_revision(args.if_revision, current.revision, "assignment")?;
            if rebind_required {
                let previous_worker = current
                    .worker
                    .as_ref()
                    .expect("resolved worker assignment has a worker");
                if orchestration::session_ref_is_live(context, previous_worker) {
                    return Err(CliError::data(
                        "worker-incarnation-still-live",
                        "prior worker incarnation is still live; continuity rebind refused",
                        Some(json!({
                            "assignment_id": current.assignment_id,
                            "current_revision": current.revision
                        })),
                    ));
                }
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
    let (assignment, tier) = {
        let registry = orchestration::load_registry_readonly(context)?;
        let principal = resolve_principal(&registry, &record, &incarnation)?;
        let assignment = match principal {
            Principal::Worker {
                assignment,
                rebind_required: false,
            } => *assignment,
            Principal::Worker {
                rebind_required: true,
                ..
            } => return Err(rebind_required()),
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
        (assignment, tier)
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
    let repository = packet.repository.clone().ok_or_else(|| {
        invalid_input("worker bootstrap requires the assignment packet to declare a repository")
    })?;
    let work_context = WorkContextInput {
        schema_version: WORK_CONTEXT_INPUT_VERSION.to_string(),
        intent: "implementation".to_string(),
        tier,
        repositories: vec![repository.clone()],
        worktrees: packet.worktree.clone().into_iter().collect(),
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
    ensure_or_acquire_claim(context, &record, &work_context, &args.idempotency_key)?;

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
        .ok_or_else(|| not_found("assignment-not-found", "assignment was not found"))?;
    Ok(json!({
        "schema_version": "main-agent.bootstrap-result.v1",
        "claim": "active",
        "assignment": private_assignment_view(context, current)?
    }))
}

fn run_worker(context: &CliContext, args: WorkerArgs) -> Result<Value, CliError> {
    match args.command {
        WorkerCommand::Start(args) => run_worker_start(context, args),
        WorkerCommand::List(_) => run_worker_list(context),
        WorkerCommand::Show(args) => run_worker_show(context, args),
        WorkerCommand::Wait(args) => run_worker_wait(context, args),
        WorkerCommand::Message(args) => run_worker_message(context, args),
        WorkerCommand::Accept(args) => {
            run_assignment_state(context, args, "submitted", "accepted", "worker-accept")
        }
        WorkerCommand::Release(args) => {
            run_assignment_state(context, args, "accepted", "released", "worker-release")
        }
        WorkerCommand::Delete(args) => run_worker_delete(context, args),
        WorkerCommand::Retire(args) => run_worker_retire(context, args),
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
    validate_idempotency_key(&args.idempotency_key)?;
    let await_ready = parse_await_ready(&args.await_ready)?;
    let assignment_file = args
        .assignment_file
        .as_ref()
        .ok_or_else(|| invalid_input("worker start requires --assignment-file"))?;
    let input: AssignmentInput = crate::coordination::read_bounded_json(
        assignment_file,
        256 * 1024,
        "invalid-assignment-packet",
    )?;
    validate_assignment_input(&input)?;
    let (record, incarnation) = authenticated_self(context)?;
    ensure_active_claim(context, &record)?;
    let packet_value =
        serde_json::to_value(&input).map_err(|_| invalid_input("assignment packet is invalid"))?;
    let request_digest = crate::coordination::request_digest("main-agent-worker-start", &input);
    let mut locked = orchestration::lock_registry(context)?;
    let replay = idempotency_replay(
        &locked.registry,
        &record,
        &incarnation,
        &args.idempotency_key,
        "worker-start",
        &request_digest,
    )?;
    let pending_start = match replay {
        Some(value) if worker_start_is_pending(&value) => {
            let assignment_id = value["assignment_id"]
                .as_str()
                .ok_or_else(|| invalid_input("pending worker start receipt is invalid"))?
                .to_string();
            let worker_session_id = value["worker_session_id"]
                .as_str()
                .map(str::to_string)
                .or_else(|| input.launch.session_id.clone())
                .unwrap_or_else(|| retry_stable_worker_session_id(&assignment_id, &request_digest));
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
        .unwrap_or_else(|| retry_stable_worker_session_id(&assignment_id, &request_digest));
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
    let existing = match load_session_record(context, &worker_session_id) {
        Ok(worker) => Some(worker),
        Err(error) if error.code() == "session-not-found" => None,
        Err(error) => return Err(error),
    };
    let (worker_record, worker_status) = if let Some(worker) = existing {
        ensure_worker_launch_matches(context, &worker, &input, &prompt)?;
        let status = session_status(&resolve_tmux_bin(None), &worker);
        (worker, status)
    } else {
        let started = start_session(
            context,
            cli::StartArgs {
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
        }
    });
    store_receipt(
        &mut locked.registry,
        &record,
        &incarnation,
        &args.idempotency_key,
        "worker-start",
        &request_digest,
        outcome.clone(),
    )?;
    locked.save()?;
    // T1: fold the readiness proof. Drop the write lock first so the wait never
    // blocks the worker's own checkpoint. The worker's authenticated,
    // revision-fenced, incarnation-matched checkpoint advancing the assignment
    // past `starting` is the readiness + newer-turn + identity proof; a bounded
    // poll classifies it into a typed result. `--await-ready 0` stays launch-only.
    drop(locked);
    let mut outcome = outcome;
    if let Some(timeout) = await_ready {
        outcome["readiness"] =
            await_worker_readiness(context, &record, &incarnation, &assignment_id, timeout)?;
    }
    Ok(outcome)
}

/// Launch every `*.json` assignment packet in `dir` as one batch. Lanes are
/// independent (T2 run-revision decouple), so a failing lane isolates to its
/// own typed result instead of aborting the batch. Per-lane idempotency keys
/// are derived from the batch key and the sorted lane index, so a re-run over
/// the same directory converges each lane through its own receipt. The command
/// itself succeeds; the caller branches on each lane's `ok`.
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
            packets.push(path);
        }
    }
    packets.sort();
    if packets.is_empty() {
        return Err(invalid_input(
            "batch directory has no .json assignment packets",
        ));
    }
    if packets.len() > 64 {
        return Err(invalid_input("batch exceeds the 64-lane limit"));
    }
    let mut lanes = Vec::with_capacity(packets.len());
    for (index, path) in packets.iter().enumerate() {
        let lane_args = WorkerStartArgs {
            assignment_file: Some(path.clone()),
            batch: None,
            if_run_revision: None,
            idempotency_key: format!("{}-{index}", args.idempotency_key),
            await_ready: "0".to_string(),
            format: OutputFormat::Json,
        };
        let assignment_file = path.to_string_lossy().into_owned();
        let lane = match run_worker_start_single(context, lane_args) {
            Ok(result) => json!({
                "assignment_file": assignment_file,
                "ok": true,
                "result": result,
            }),
            Err(error) => {
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
        };
        lanes.push(lane);
    }
    Ok(json!({
        "schema_version": "main-agent.worker-start-batch.v1",
        "lanes": lanes,
    }))
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
const WORKER_WAIT_POLL_INTERVAL: Duration = Duration::from_millis(50);

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

/// Bounded, read-only wait for a freshly launched worker to advance its
/// assignment past `starting`, returning the typed readiness projection. Mirrors
/// the `worker wait` poll (no lock, level-triggered) so it never blocks the
/// worker's own checkpoint.
fn await_worker_readiness(
    context: &CliContext,
    record: &SessionRecord,
    incarnation: &str,
    assignment_id: &str,
    timeout: Duration,
) -> Result<Value, CliError> {
    let started = Instant::now();
    loop {
        let registry = orchestration::load_registry_readonly(context)?;
        let run = require_current_main(&registry, record, incarnation)?;
        let state = registry
            .assignments
            .get(assignment_id)
            .filter(|assignment| assignment.run_id == run.run_id)
            .map(|assignment| assignment.state.clone())
            .ok_or_else(|| not_found("assignment-not-found", "assignment was not found"))?;
        if state != "starting" {
            return Ok(json!({
                "state": readiness_from_state(&state),
                "assignment_state": state,
                "worker_launched": true,
                "delivery": {
                    "state": "confirmed",
                    "transport_state": "submit-command-succeeded",
                    "proof": "authenticated-worker-checkpoint"
                }
            }));
        }
        if started.elapsed() >= timeout {
            return Ok(json!({
                "state": "readiness_failed",
                "assignment_state": state,
                "worker_launched": true,
                "delivery": {
                    "state": "unverified",
                    "transport_state": "submit-command-succeeded",
                    "proof": "worker-checkpoint-timeout"
                },
                "automatic_retry_safe": false,
                "safe_state": "worker remains launched and bound in `starting`; prompt delivery is unverified, so do not resend the prompt or inject another Enter. Keep the worker available for transport diagnostics."
            }));
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

    let (mut revision, state) = {
        let registry = orchestration::load_registry_readonly(context)?;
        let run = require_current_main(&registry, &record, &incarnation)?;
        let assignment = registry
            .assignments
            .get(&args.assignment_id)
            .filter(|assignment| assignment.run_id == run.run_id)
            .ok_or_else(|| not_found("assignment-not-found", "assignment was not found"))?;
        ensure_primary_manager(assignment, &record, &incarnation)?;
        (assignment.revision, assignment.state.clone())
    };

    if !matches!(state.as_str(), "accepted" | "released" | "cancelled") {
        return Err(CliError::data(
            "assignment-not-retireable",
            "worker retire requires an accepted, released, or cancelled assignment",
            Some(json!({ "assignment_id": args.assignment_id, "state": state })),
        ));
    }

    // Release first only when the assignment is still accepted; a re-run that
    // already released (or a natively terminal assignment) skips straight to
    // delete.
    let released = if state == "accepted" {
        let release = run_assignment_state(
            context,
            AssignmentMutationArgs {
                assignment_id: args.assignment_id.clone(),
                if_revision: revision,
                idempotency_key: format!("{}-release", args.idempotency_key),
                format: OutputFormat::Json,
            },
            "accepted",
            "released",
            "worker-release",
        )?;
        revision = release["assignment"]["revision"]
            .as_u64()
            .unwrap_or(revision.saturating_add(1));
        true
    } else {
        false
    };

    let delete = run_worker_delete(
        context,
        AssignmentMutationArgs {
            assignment_id: args.assignment_id.clone(),
            if_revision: revision,
            idempotency_key: format!("{}-delete", args.idempotency_key),
            format: OutputFormat::Json,
        },
    )?;
    let deleted = delete
        .get("deleted")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let cleanup_pending = delete
        .get("cleanup_pending")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    Ok(json!({
        "schema_version": "main-agent.worker-retire-result.v1",
        "assignment_id": args.assignment_id,
        "released": released,
        "deleted": deleted,
        "cleanup_pending": cleanup_pending,
        "run_closed": delete.get("run_closed").cloned().unwrap_or(Value::Bool(false)),
        "retired": deleted
    }))
}

fn run_worker_message(context: &CliContext, args: WorkerMessageArgs) -> Result<Value, CliError> {
    validate_idempotency_key(&args.idempotency_key)?;
    let (record, incarnation) = authenticated_self(context)?;
    ensure_active_claim(context, &record)?;
    let registry = orchestration::load_registry_readonly(context)?;
    let run = require_current_main(&registry, &record, &incarnation)?;
    let worker = registry
        .assignments
        .get(&args.assignment_id)
        .filter(|assignment| assignment.run_id == run.run_id)
        .and_then(|assignment| assignment.worker.as_ref())
        .ok_or_else(|| not_found("worker-not-started", "worker session is not available"))?;
    crate::coordination::mailbox::send(
        context,
        cli::MessageSendArgs {
            from_session: record.id,
            to_session: worker.session_id.clone(),
            body_file: args.body_file,
            capability_file: None,
            idempotency_key: args.idempotency_key,
            reply_to: None,
            expires_in: None,
            format: OutputFormat::Json,
        },
    )
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
                return Err(not_found(
                    "worker-not-started",
                    "worker session is not available",
                ));
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
        let mut locked = orchestration::lock_registry(context)?;
        if let Some(value) = idempotency_replay(
            &locked.registry,
            &record,
            &incarnation,
            &args.idempotency_key,
            "worker-delete",
            &request_digest,
        )? && !worker_delete_is_pending(&value)
        {
            return Ok(value);
        }
        let run = require_current_main(&locked.registry, &record, &incarnation)?;
        let current = locked
            .registry
            .assignments
            .get(&args.assignment_id)
            .filter(|assignment| assignment.run_id == run.run_id)
            .ok_or_else(|| not_found("assignment-not-found", "assignment was not found"))?;
        ensure_primary_manager(current, &record, &incarnation)?;
        ensure_revision(args.if_revision, current.revision, "assignment")?;
        if current.worker.as_ref() != Some(&worker) {
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
            &record,
            &incarnation,
            &args.idempotency_key,
            "worker-delete",
            &request_digest,
            pending,
        )?;
        locked.save()?;
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
    let target = resolve_live_session_ref(context, &args.to_session)?;
    let registry = orchestration::load_registry_readonly(context)?;
    if !registry
        .runs
        .values()
        .any(|run| run.controller == target && run.state == "active")
    {
        return Err(invalid_input("handoff target is not an active Main Agent"));
    }
    let (record, incarnation) = authenticated_self(context)?;
    let (_, active_operation) = crate::coordination::session_has_active_claim_or_operation(
        context,
        &record.id,
        &incarnation,
    )?;
    if active_operation {
        return Err(CliError::data(
            "handoff-not-quiescent",
            "primary manager has an active or uncertain mutation operation",
            None,
        ));
    }
    run_relationship_mutation(
        context,
        args.assignment_id,
        args.if_revision,
        args.idempotency_key,
        "handoff",
        |assignment| {
            assignment.primary_manager = target.clone();
            Ok(())
        },
    )
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
    validate_group_cleanup_request(main_session_id, &request)?;
    let request_digest = group_cleanup_request_digest(&request);

    {
        let locked = orchestration::lock_registry(context)?;
        if let Some(value) = group_cleanup_replay(
            &locked.registry,
            main_session_id,
            &request.expected_main_incarnation,
            &request.idempotency_key,
            &request_digest,
        )? {
            return Ok(GroupCleanupExecution {
                value,
                deleted_registry_fences: Vec::new(),
            });
        }
    }

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
        })?
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
    let main = session_ref(context, &record, &incarnation);

    let plan = {
        let mut locked = orchestration::lock_registry(context)?;
        if let Some(value) = group_cleanup_replay(
            &locked.registry,
            main_session_id,
            &incarnation,
            &request.idempotency_key,
            &request_digest,
        )? {
            return Ok(GroupCleanupExecution {
                value,
                deleted_registry_fences: Vec::new(),
            });
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
        prepare_group_cleanup_assignments(&mut locked.registry, &run, &main, request.mode)?;
        locked.save()?;
        plan
    };

    let mut deleted_registry_fences = Vec::new();
    let mut worker_results = Vec::new();
    for worker_plan in &plan.workers {
        let Some(worker) = worker_plan.worker.as_ref() else {
            worker_results.push(json!({
                "assignment_id": worker_plan.assignment_id,
                "session_id": null,
                "outcome": "not_started",
                "cleanup_pending": false,
            }));
            continue;
        };
        let worker_path = session_dir(context, &worker.session_id);
        match fs::symlink_metadata(&worker_path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                worker_results.push(json!({
                    "assignment_id": worker_plan.assignment_id,
                    "session_id": worker.session_id,
                    "outcome": "absent",
                    "cleanup_pending": false,
                }));
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
                    &record,
                    &incarnation,
                    &request,
                    &request_digest,
                    value.clone(),
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
                    &record,
                    &incarnation,
                    &request,
                    &request_digest,
                    value.clone(),
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
                &record,
                &incarnation,
                &request,
                &request_digest,
                value.clone(),
            )?;
            return Ok(GroupCleanupExecution {
                value,
                deleted_registry_fences,
            });
        }
        match delete_session(context, &worker.session_id, tmux_bin.clone()) {
            Ok(deleted) => {
                worker_results.push(json!({
                    "assignment_id": worker_plan.assignment_id,
                    "session_id": worker.session_id,
                    "outcome": "deleted",
                    "cleanup_pending": deleted.cleanup_pending,
                }));
                deleted_registry_fences.push(deleted.registry_fence);
            }
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
                    &record,
                    &incarnation,
                    &request,
                    &request_digest,
                    value.clone(),
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
            .get_mut(&plan.run_id)
            .filter(|run| run.state == "active" && run.controller == main)
            .ok_or_else(|| {
                CliError::data(
                    "group-cleanup-run-conflict",
                    "Main Agent run changed while workers were being cleaned up",
                    None,
                )
            })?;
        if run.revision != request.expected_run_revision {
            let error = CliError::data(
                "group-cleanup-run-conflict",
                "Main Agent run revision changed while workers were being cleaned up",
                Some(json!({ "current_run_revision": run.revision })),
            );
            let value =
                group_cleanup_failure(&plan, &worker_results, None, "run_close", &error, false);
            store_receipt(
                &mut locked.registry,
                &record,
                &incarnation,
                &request.idempotency_key,
                "group-cleanup",
                &request_digest,
                value.clone(),
            )?;
            locked.save()?;
            return Ok(GroupCleanupExecution {
                value,
                deleted_registry_fences,
            });
        }
        run.state = "closed".to_string();
        run.revision = run.revision.saturating_add(1);
        run.updated_at = timestamp();
        locked.save()?;
    }

    let current_main = load_session_record(context, main_session_id)?;
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
            &record,
            &incarnation,
            &request,
            &request_digest,
            value.clone(),
        )?;
        return Ok(GroupCleanupExecution {
            value,
            deleted_registry_fences,
        });
    }
    let main_deleted = match delete_session(context, main_session_id, tmux_bin) {
        Ok(deleted) => deleted,
        Err(error) => {
            let value =
                group_cleanup_failure(&plan, &worker_results, None, "main_delete", &error, true);
            store_group_cleanup_receipt(
                context,
                &record,
                &incarnation,
                &request,
                &request_digest,
                value.clone(),
            )?;
            return Ok(GroupCleanupExecution {
                value,
                deleted_registry_fences,
            });
        }
    };
    deleted_registry_fences.push(main_deleted.registry_fence);
    let value = json!({
        "schema_version": GROUP_CLEANUP_RESULT_SCHEMA,
        "run_id": plan.run_id,
        "completed": true,
        "run_closed": true,
        "main_deleted": true,
        "workers": worker_results,
    });
    store_group_cleanup_receipt(
        context,
        &record,
        &incarnation,
        &request,
        &request_digest,
        value.clone(),
    )?;
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

fn group_cleanup_replay(
    registry: &orchestration::Registry,
    main_session_id: &str,
    incarnation: &str,
    idempotency_key: &str,
    request_digest: &str,
) -> Result<Option<Value>, CliError> {
    let Some(receipt) =
        registry
            .receipts
            .get(&receipt_key(main_session_id, incarnation, idempotency_key))
    else {
        return Ok(None);
    };
    if receipt.operation != "group-cleanup" || receipt.request_digest != request_digest {
        return Err(CliError::data(
            "idempotency-conflict",
            "idempotency key was already used for a different request",
            None,
        ));
    }
    Ok(Some(receipt.outcome.clone()))
}

fn store_group_cleanup_receipt(
    context: &CliContext,
    record: &SessionRecord,
    incarnation: &str,
    request: &GroupCleanupRequest,
    request_digest: &str,
    value: Value,
) -> Result<(), CliError> {
    let mut locked = orchestration::lock_registry(context)?;
    store_receipt(
        &mut locked.registry,
        record,
        incarnation,
        &request.idempotency_key,
        "group-cleanup",
        request_digest,
        value,
    )?;
    locked.save()
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
        schema_version: GROUP_CLEANUP_SCHEMA,
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
        .values_mut()
        .filter(|assignment| assignment.run_id == run.run_id && assignment.primary_manager == *main)
    {
        let next = match assignment.state.as_str() {
            "accepted" => Some("released"),
            "released" | "cancelled" => None,
            _ if mode == GroupCleanupMode::Force => Some("cancelled"),
            _ => None,
        };
        if let Some(next) = next {
            assignment.state = next.to_string();
            assignment.revision = assignment.revision.saturating_add(1);
            assignment.updated_at = timestamp();
        }
    }
    Ok(())
}

/// Fast-path for L0/L1 delegate-all: acquire the claim, create an ephemeral
/// run synthesized from the assignment packet, then launch the assignment's
/// worker — all in one call. The run is marked ephemeral so it auto-closes when
/// the worker is torn down (see `finalize_worker_delete`), sparing the caller an
/// explicit `close`. A session that already controls a run must use the
/// granular `init` + `worker start` path instead.
fn run_quick(context: &CliContext, args: QuickArgs) -> Result<Value, CliError> {
    if !matches!(args.tier.as_str(), "L0" | "L1" | "L2" | "L3") {
        return Err(invalid_input("quick tier is invalid"));
    }
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
    ensure_or_acquire_claim(context, &record, &work_context, &idempotency_key)?;

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
    let request_digest = crate::coordination::request_digest(
        "main-agent-quick",
        &json!({ "objective": objective, "assignment": input }),
    );

    let run_id = {
        let mut locked = orchestration::lock_registry(context)?;
        match idempotency_replay(
            &locked.registry,
            &record,
            &incarnation,
            &idempotency_key,
            "quick",
            &request_digest,
        )? {
            Some(value) if value["schema_version"] == "main-agent.quick-result.v1" => {
                return Ok(value);
            }
            Some(value) => value["run"]["run_id"]
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| invalid_input("pending quick receipt is invalid"))?,
            None => {
                if locked.registry.runs.values().any(|run| {
                    run.controller.session_id == record.id
                        && run.controller.session_created_at == record.created_at
                }) {
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
            idempotency_key: format!("{}-worker", idempotency_key),
            await_ready: "0".to_string(),
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
) -> Result<(), CliError> {
    match ensure_active_claim(context, record) {
        Ok(()) => return Ok(()),
        Err(error) if error.code() == "claim-not-active" => {}
        Err(error) => return Err(error),
    }
    let directory = session_dir(context, &record.id).join("coordination");
    fs::create_dir_all(&directory)
        .map_err(|_| invalid_input("claim input directory is unavailable"))?;
    let candidate_path = directory.join(format!("main-agent-init-{}.json", uuid::Uuid::new_v4()));
    let bytes =
        serde_json::to_vec(candidate).map_err(|_| invalid_input("work context is invalid"))?;
    write_atomic(&candidate_path, &bytes, SECRET_FILE_MODE)
        .map_err(|_| invalid_input("claim input could not be prepared"))?;
    let result = crate::coordination::claims::claim(
        context,
        cli::WorkContextClaimArgs {
            session: record.id.clone(),
            file: candidate_path.clone(),
            capability_file: None,
            idempotency_key: idempotency_key.to_string(),
            if_revision: None,
            format: OutputFormat::Json,
        },
    );
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
    if registry.receipts.len() >= 32_768 {
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
        receipt_key(&record.id, incarnation, idempotency_key),
        IdempotencyReceipt {
            principal_session_id: record.id.clone(),
            principal_incarnation: incarnation.to_string(),
            operation: operation.to_string(),
            request_digest: request_digest.to_string(),
            outcome,
            created_at_epoch: crate::coordination::now_epoch(),
        },
    );
    Ok(())
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
        MainAgentCommand::SelfGroup(_) => "self-show",
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
            WorkerCommand::Accept(_) => "worker-accept",
            WorkerCommand::Release(_) => "worker-release",
            WorkerCommand::Delete(_) => "worker-delete",
            WorkerCommand::Retire(_) => "worker-retire",
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
            WorkerCommand::Wait(args) => args.format,
            WorkerCommand::Message(args) => args.format,
            WorkerCommand::Accept(args)
            | WorkerCommand::Release(args)
            | WorkerCommand::Delete(args)
            | WorkerCommand::Retire(args) => args.format,
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
        .any(|pair| pair[0] == "--format" && pair[1] == "json")
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

    fn busy() -> CliError {
        CliError::unavailable("orchestration-store-busy", "busy", None)
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
            created_at: "2030-01-01T00:00:00Z".to_string(),
            updated_at: "2030-01-01T00:00:00Z".to_string(),
        }
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
        fs::create_dir_all(session_dir(&context, "worker-broken")).unwrap();

        let mut run = run_record("run-one", false);
        run.controller = session_ref(&context, &main, "inc");
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
                mode: GroupCleanupMode::Force,
                idempotency_key: "cleanup-001".to_string(),
            },
            PathBuf::from("/bin/false"),
        )
        .unwrap();

        assert_eq!(execution.value["completed"], false);
        assert_eq!(execution.value["main_deleted"], false);
        assert_eq!(execution.value["failure"]["stage"], "worker_cleanup");
        assert!(
            session_dir(&context, "main").join("session.json").exists(),
            "worker cleanup failure must preserve the Main Agent record"
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
