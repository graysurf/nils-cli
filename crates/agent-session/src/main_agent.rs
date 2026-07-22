use std::env;
use std::ffi::OsString;
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;

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
use crate::coordination::context::WorkContextInput;
use crate::orchestration::{
    self, ASSIGNMENT_INPUT_SCHEMA, ASSIGNMENT_SCHEMA, AssignmentRecord, CHECKPOINT_INPUT_SCHEMA,
    IdempotencyReceipt, PACKET_SCHEMA, RunCheckpoint, RunRecord, SessionRef, TimedRelationship,
};
use crate::{
    CliContext, CliError, SessionRecord, StartFailureDisposition, delete_session,
    load_session_record, resolve_tmux_bin, session_dir, session_status, start_session,
};

const BINARY: &str = "main-agent";
const IDEMPOTENCY_KEY_HELP: &str = "Retry an ambiguous outcome with the same idempotency key and the same logical request; use a new key for a changed request.";
const ASSIGNMENT_REVISION_HELP: &str =
    "Expected current assignment revision; stale values fail closed and report current_revision.";
const RUN_REVISION_HELP: &str =
    "Expected current run revision; stale values fail closed and report current_revision.";
const MAIN_AGENT_AFTER_HELP: &str = "SAFE LIFECYCLE:\n  init -> rehydrate/status -> worker start -> worker self/checkpoint\n  accept -> release -> delete -> close\n\nREVISION AND RETRY RULES:\n  Read the current run or assignment revision before each mutation. Retry an\n  ambiguous outcome with the identical request and idempotency key. After a\n  confirmed revision conflict, re-read state and use a new key for the revised\n  request.\n\nEXAMPLES:\n  main-agent init --packet-file objective.json --if-absent --idempotency-key init-001 --format json\n  main-agent rehydrate --format markdown\n  main-agent worker start --assignment-file assignment.json --if-run-revision 1 --idempotency-key start-001 --format json\n  main-agent worker accept ASSIGNMENT_ID --if-revision 4 --idempotency-key accept-001 --format json\n\nOPERATOR RUNBOOK:\n  crates/agent-session/docs/runbooks/main-agent-orchestration.md\n\nEXIT CODES:\n  0   success\n  1   runtime error\n  64  command-line usage error\n  65  invalid or stale data\n  69  temporarily unavailable";

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
    /// Inspect the authenticated Main Agent or worker identity.
    #[command(name = "self")]
    SelfGroup(SelfGroupArgs),
    /// Recover the authenticated durable objective or assignment capsule.
    Rehydrate(RehydrateArgs),
    /// Show a bounded status capsule.
    Status(ReadArgs),
    /// Record a revision-fenced run or worker checkpoint.
    Checkpoint(CheckpointArgs),
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
    /// Print shell completion script.
    Completion(CompletionArgs),
}

#[derive(Debug, Args)]
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

#[derive(Debug, Args)]
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

#[derive(Debug, Args)]
struct RehydrateArgs {
    /// Recovery capsule output format.
    #[arg(long, value_enum, default_value_t = RehydrateFormat::Markdown)]
    format: RehydrateFormat,
}

#[derive(Debug, Args)]
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

#[derive(Debug, Args)]
struct WorkerArgs {
    #[command(subcommand)]
    command: WorkerCommand,
}

#[derive(Debug, Subcommand)]
enum WorkerCommand {
    /// Create an assignment and launch its interactive managed worker.
    Start(WorkerStartArgs),
    /// List assignments owned by this Main Agent's active run.
    List(ReadArgs),
    /// Show one assignment, including its private packet.
    Show(WorkerShowArgs),
    /// Send a private mailbox message to an assignment's worker.
    Message(WorkerMessageArgs),
    /// Accept a submitted worker result after Main Agent review.
    Accept(AssignmentMutationArgs),
    /// Mark an accepted assignment terminal before worker deletion.
    Release(AssignmentMutationArgs),
    /// Delete a released worker through guarded agent-session cleanup.
    Delete(AssignmentMutationArgs),
}

#[derive(Debug, Args)]
struct WorkerStartArgs {
    /// Private assignment packet JSON file.
    #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
    assignment_file: PathBuf,
    #[arg(long, help = RUN_REVISION_HELP)]
    if_run_revision: u64,
    #[arg(long, help = IDEMPOTENCY_KEY_HELP)]
    idempotency_key: String,
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    format: OutputFormat,
}

#[derive(Debug, Args)]
struct WorkerShowArgs {
    assignment_id: String,
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    format: OutputFormat,
}

#[derive(Debug, Args)]
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

#[derive(Debug, Args)]
struct AssignmentMutationArgs {
    assignment_id: String,
    #[arg(long, help = ASSIGNMENT_REVISION_HELP)]
    if_revision: u64,
    #[arg(long, help = IDEMPOTENCY_KEY_HELP)]
    idempotency_key: String,
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    format: OutputFormat,
}

#[derive(Debug, Args)]
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

#[derive(Debug, Args)]
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

#[derive(Debug, Args)]
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

#[derive(Debug, Args)]
struct RunMutationArgs {
    #[arg(long, help = RUN_REVISION_HELP)]
    if_revision: u64,
    #[arg(long, help = IDEMPOTENCY_KEY_HELP)]
    idempotency_key: String,
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
    let result = match cli.command {
        MainAgentCommand::Init(args) => run_init(&context, args),
        MainAgentCommand::SelfGroup(args) => match args.command {
            SelfCommand::Show(args) => run_self_show(&context, args),
        },
        MainAgentCommand::Rehydrate(args) => run_rehydrate(&context, args),
        MainAgentCommand::Status(args) => run_status(&context, args),
        MainAgentCommand::Checkpoint(args) => run_checkpoint(&context, args),
        MainAgentCommand::Worker(args) => run_worker(&context, args),
        MainAgentCommand::Collaborate(args) => run_collaborate(&context, args),
        MainAgentCommand::Borrow(args) => run_borrow(&context, args),
        MainAgentCommand::Handoff(args) => run_handoff(&context, args),
        MainAgentCommand::Adopt(args) => run_adopt(&context, args),
        MainAgentCommand::Close(args) => run_close(&context, args),
        MainAgentCommand::Completion(_) => unreachable!(),
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

fn run_worker(context: &CliContext, args: WorkerArgs) -> Result<Value, CliError> {
    match args.command {
        WorkerCommand::Start(args) => run_worker_start(context, args),
        WorkerCommand::List(_) => run_worker_list(context),
        WorkerCommand::Show(args) => run_worker_show(context, args),
        WorkerCommand::Message(args) => run_worker_message(context, args),
        WorkerCommand::Accept(args) => {
            run_assignment_state(context, args, "submitted", "accepted", "worker-accept")
        }
        WorkerCommand::Release(args) => {
            run_assignment_state(context, args, "accepted", "released", "worker-release")
        }
        WorkerCommand::Delete(args) => run_worker_delete(context, args),
    }
}

fn run_worker_start(context: &CliContext, args: WorkerStartArgs) -> Result<Value, CliError> {
    validate_idempotency_key(&args.idempotency_key)?;
    let input: AssignmentInput = crate::coordination::read_bounded_json(
        &args.assignment_file,
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
    ensure_revision(args.if_run_revision, run.revision, "run")?;
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
    let prompt = worker_start_prompt(&assignment_id);
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
    Ok(outcome)
}

fn worker_start_is_pending(value: &Value) -> bool {
    value["schema_version"] == "main-agent.worker-start-result.v1"
        && value["state"] == "starting"
        && value["acceptance"] == "pending"
}

fn worker_start_prompt(assignment_id: &str) -> String {
    format!(
        "You are a managed worker for assignment {assignment_id}. Run `main-agent self show --format json`, then checkpoint state `working` before mutations."
    )
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
    let prompt_matches = worker
        .prompt_file
        .as_deref()
        .and_then(|path| fs::read_to_string(path).ok())
        .is_some_and(|prompt| prompt == expected_prompt);
    if worker.agent != input.launch.agent
        || std::path::Path::new(&worker.cwd) != expected_cwd
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
    let outcome = json!({
        "schema_version": "main-agent.worker-delete-result.v1",
        "assignment": public_assignment_view(current),
        "deleted": deleted,
        "cleanup_pending": cleanup_pending
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

fn validate_objective_packet(packet: &ObjectivePacket) -> Result<(), CliError> {
    if packet.schema_version != PACKET_SCHEMA {
        return Err(invalid_input("objective packet schema is unsupported"));
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
        return Err(invalid_input("assignment packet schema is unsupported"));
    }
    orchestration::validate_summary("task summary", &input.task_summary)?;
    if input.scopes.len() > 32
        || input.durable_refs.len() > 64
        || input.launch.agent_args.len() > 64
    {
        return Err(invalid_input("assignment packet exceeds collection limits"));
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
        return Err(invalid_input("checkpoint schema is unsupported"));
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
        MainAgentCommand::SelfGroup(_) => "self-show",
        MainAgentCommand::Rehydrate(_) => "rehydrate",
        MainAgentCommand::Status(_) => "status",
        MainAgentCommand::Checkpoint(_) => "checkpoint",
        MainAgentCommand::Worker(args) => match args.command {
            WorkerCommand::Start(_) => "worker-start",
            WorkerCommand::List(_) => "worker-list",
            WorkerCommand::Show(_) => "worker-show",
            WorkerCommand::Message(_) => "worker-message",
            WorkerCommand::Accept(_) => "worker-accept",
            WorkerCommand::Release(_) => "worker-release",
            WorkerCommand::Delete(_) => "worker-delete",
        },
        MainAgentCommand::Collaborate(_) => "collaborate",
        MainAgentCommand::Borrow(_) => "borrow",
        MainAgentCommand::Handoff(_) => "handoff",
        MainAgentCommand::Adopt(_) => "adopt",
        MainAgentCommand::Close(_) => "close",
        MainAgentCommand::Completion(_) => "completion",
    }
}

fn command_output_format(command: &MainAgentCommand) -> OutputFormat {
    match command {
        MainAgentCommand::Init(args) => args.format,
        MainAgentCommand::SelfGroup(args) => match &args.command {
            SelfCommand::Show(args) => args.format,
        },
        MainAgentCommand::Rehydrate(args) => match args.format {
            RehydrateFormat::Json => OutputFormat::Json,
            RehydrateFormat::Markdown => OutputFormat::Text,
        },
        MainAgentCommand::Status(args) => args.format,
        MainAgentCommand::Checkpoint(args) => args.format,
        MainAgentCommand::Worker(args) => match &args.command {
            WorkerCommand::Start(args) => args.format,
            WorkerCommand::List(args) => args.format,
            WorkerCommand::Show(args) => args.format,
            WorkerCommand::Message(args) => args.format,
            WorkerCommand::Accept(args)
            | WorkerCommand::Release(args)
            | WorkerCommand::Delete(args) => args.format,
        },
        MainAgentCommand::Collaborate(args) => args.format,
        MainAgentCommand::Borrow(args) => args.format,
        MainAgentCommand::Handoff(args) => args.format,
        MainAgentCommand::Adopt(args) => args.format,
        MainAgentCommand::Close(args) => args.format,
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
            let envelope: Envelope<()> =
                Envelope::failure(schema_version_for(BINARY, command, 1), envelope_error);
            print_json(&envelope);
        }
        OutputFormat::Text => {
            let _ = writeln!(io::stderr(), "error: {}", error.message);
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
