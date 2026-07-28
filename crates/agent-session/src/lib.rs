mod activity;
mod auto_resume;
mod cli;
mod codex_account;
mod codex_app_server;
pub mod completion;
mod coordination;
mod main_agent;
mod maintenance;
mod orchestration;
mod provider_prompt;
mod serve;

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::env;
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
#[cfg(target_os = "linux")]
use std::os::fd::{FromRawFd, OwnedFd};
#[cfg(target_os = "linux")]
use std::os::unix::fs::MetadataExt;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use clap::Parser;
use clap::error::ErrorKind;
use jiff::{Timestamp, Zoned};
use nils_common::cli_contract::{
    Envelope, EnvelopeError, OutputFormat, emit_parse_error, exit, schema_version_for,
};
use nils_common::fs::{
    SECRET_FILE_MODE, display_path, expand_home, home_dir, normalize_path, write_atomic,
};
use nils_common::git::parse_git_remote_url;
// The provider session-resume resolver, session-history scanning primitives, and
// bounded-scan budgets live in `nils-provider-resume` so `codex-cli`,
// `claude-cli`, and this crate share one implementation. Items reused by the
// `provider_prompt` module via `crate::` are re-exported at crate scope.
use nils_provider_resume::{
    CODEX_RESUME_SCAN_MAX_DEPTH, ClaudeResumeScanBudget, CodexResumeScanBudget, ResumeIdError,
    ResumeProvider, ResumeResolveError, collect_claude_provider_resume_matches,
    collect_codex_provider_resume_matches, normalize_resume_id, resolve_resume_source,
    resolve_resume_source_in_config_dir,
};
pub(crate) use nils_provider_resume::{
    claude_projects_root, codex_sessions_root, read_claude_session_cwd,
    read_codex_resumable_session_meta, read_codex_session_meta,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use cli::{AgentKind, Cli, Command, SpecialKey};

const SESSION_DOCUMENT_VERSION: &str = "agent-session.session.v1";
const SESSION_RESUME_DOCUMENT_VERSION: &str = "agent-session.resume.v1";
const STARTUP_PROJECTION_VERSION: &str = "agent-session.startup.v1";
const STARTUP_EXTRA_KEY: &str = "startup";
const AGENT_PROFILE_RUNTIME_KEY: &str = "agent_profile";
const AGENT_PROFILE_PROVIDER_CONFIG_DIR_RUNTIME_KEY: &str = "agent_profile_provider_config_dir";
const AGENT_PROFILE_AUTO_RESUME_SUPPORTED_RUNTIME_KEY: &str = "agent_profile_auto_resume_supported";
const AGENT_PROFILE_CODEX_USAGE_ACCOUNT_RUNTIME_KEY: &str = "agent_profile_codex_usage_account";
const STARTUP_STAGE_FILE: &str = ".startup-stage";
const STARTUP_FAILURE_FILE: &str = ".startup-failure";
const STARTUP_DIAGNOSTIC_FILE: &str = ".startup-diagnostic.log";
const RUNTIME_EXIT_STATUS_FILE: &str = ".runtime-exit-status";
const STARTUP_ARTIFACT_FILES: [&str; 4] = [
    STARTUP_STAGE_FILE,
    STARTUP_FAILURE_FILE,
    STARTUP_DIAGNOSTIC_FILE,
    RUNTIME_EXIT_STATUS_FILE,
];
const SESSION_RESUME_FILE: &str = "resume.json";
const SESSION_LOCKS_DIR: &str = "session-locks";
const SESSION_DELETE_TOMBSTONES_DIR: &str = "session-delete-tombstones";
const BINARY: &str = "agent-session";
const START_COMMAND: &str = "start";
const RUN_COMMAND: &str = "run";
const LIST_COMMAND: &str = "list";
const COMMAND_COMMAND: &str = "command";
const LOGS_COMMAND: &str = "logs";
const SEND_COMMAND: &str = "send";
const GLANCE_COMMAND: &str = "glance";
const RESUME_COMMAND: &str = "resume";
const ACTIVITY_EVENT_COMMAND: &str = "activity-event";
const ACTIVITY_STATUS_COMMAND: &str = "activity-status";
const ACTIVITY_DOCTOR_COMMAND: &str = "activity-doctor";
const ACTIVITY_SETUP_COMMAND: &str = "activity-setup";
const DELETE_COMMAND: &str = "delete";
const WORKDIR_USAGE_FILE: &str = "workdir-usage.json";
const CODEX_RESUME_CAPTURE_TIMEOUT_MS: u64 = 1500;
const CODEX_RESUME_CAPTURE_POLL_MS: u64 = 100;
const CODEX_RESUME_AMBIGUITY_WINDOW_MS: u64 = 500;
const CODEX_RESUME_BACKFILL_MAX_AGE_SECS: u64 = 10 * 60;
const PANE_INPUT_COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
const SUBMIT_RECOVERY_INPUT_COMMAND_TIMEOUT: Duration = Duration::from_secs(1);
const POST_PASTE_KEY_SETTLE_DELAY: Duration = Duration::from_millis(500);
const PANE_PASTE_READY_POLL_INTERVAL: Duration = Duration::from_millis(100);
const PANE_PASTE_READY_DEADLINE: Duration = Duration::from_secs(15);
const DELETE_TERMINATION_VERIFY_TIMEOUT: Duration = Duration::from_secs(1);
const DELETE_TERMINATION_VERIFY_POLL_INTERVAL: Duration = Duration::from_millis(25);
const DELETE_TERMINATION_PROBE_TIMEOUT: Duration = Duration::from_millis(100);
const DELETE_TERMINATION_IDENTITY_RETRY_LIMIT: usize = 3;
const DELETE_TMUX_PROBE_MAX_OUTPUT_BYTES: usize = 4 * 1024;
const AGENT_HOOK_SETUP_MAX_OUTPUT_BYTES: usize = 1024 * 1024;
const DELETE_TMUX_IDENTITY_KEY: &str = "delete_tmux_identity";
const DELETE_TMUX_PRIOR_IDENTITIES_KEY: &str = "delete_tmux_prior_identities";
const DELETE_TMUX_TERMINATION_STATE_KEY: &str = "delete_tmux_termination_state";
const TMUX_RUNTIME_NEVER_LAUNCHED_KEY: &str = "tmux_runtime_never_launched";
const TMUX_RUNTIME_IDENTITY_CHANGED_OUTPUT: &str = "agent-session-runtime-identity-changed";
const COORDINATION_LAUNCH_GATE: &str = "launch-ready";
const COORDINATION_BROKER_GATE: &str = "broker-provisioned";
const HELD_LAUNCH_SCRIPT: &str = "gate=$1; broker_gate=$2; heartbeat=$3; capability=$4; incarnation=$5; generation=$6; broker_bin=$7; shift 7; done_file=\"${heartbeat}.done.$$\"; umask 077; while [ ! -f \"$broker_gate\" ]; do sleep 0.01; done; \"$broker_bin\" --state-dir \"$AGENT_SESSION_STATE_DIR\" broker heartbeat --session \"$AGENT_SESSION_ID\" --incarnation \"$incarnation\" --generation \"$generation\" --capability-file \"$capability\" --format json >/dev/null 2>&1 & broker_pid=$!; while [ ! -f \"$gate\" ]; do sleep 0.01; done; \"$@\"; status=$?; printf '%s\\n' \"$status\" > \"$done_file\"; kill \"$broker_pid\" >/dev/null 2>&1 || true; wait \"$broker_pid\" >/dev/null 2>&1 || true; \"$broker_bin\" --state-dir \"$AGENT_SESSION_STATE_DIR\" broker stop --session \"$AGENT_SESSION_ID\" --capability-file \"$capability\" --format json >/dev/null 2>&1 || true; rm -f \"$done_file\" \"$capability\" \"$broker_gate\" \"$gate\"; exit \"$status\"";

pub fn run() -> i32 {
    run_with_args(env::args_os())
}

pub fn run_main_agent() -> i32 {
    main_agent::run()
}

pub fn run_main_agent_with_args<I, T>(args: I) -> i32
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    main_agent::run_with_args(args)
}

pub fn run_with_args<I, T>(args: I) -> i32
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let raw_args: Vec<OsString> = args.into_iter().map(Into::into).collect();
    let cli = match Cli::try_parse_from(raw_args.clone()) {
        Ok(cli) => cli,
        Err(err) => {
            let kind = err.kind();
            if matches!(
                kind,
                ErrorKind::DisplayHelp
                    | ErrorKind::DisplayVersion
                    | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
            ) {
                let _ = err.print();
                return err.exit_code();
            }

            let format = detect_format_from_args(&raw_args);
            let code = match kind {
                ErrorKind::InvalidSubcommand => "unknown-subcommand",
                _ => "parse-error",
            };
            let message = render_clap_message(&err);
            if let Some(command) = coordination_leaf_from_raw_args(&raw_args) {
                return render_error(command, format, CliError::usage(code, message, None));
            }
            return emit_parse_error(BINARY, format, code, &message);
        }
    };

    dispatch(cli)
}

fn dispatch(cli: Cli) -> i32 {
    if let Command::Completion(args) = cli.command {
        return completion::run(args.shell);
    }

    let format = command_format(&cli.command);
    let context = match CliContext::resolve(cli.state_dir, cli.host) {
        Ok(context) => context,
        Err(err) => {
            return render_error(
                coordination_command_name(&cli.command).unwrap_or("error"),
                format,
                err,
            );
        }
    };

    match cli.command {
        Command::Start(args) => run_start(&context, *args),
        Command::Run(args) => run_one_shot(&context, args),
        Command::List(args) => run_list(&context, args),
        Command::Show(args) => run_command(&context, args),
        Command::Attach(args) => run_attach(&context, args),
        Command::Logs(args) => run_logs(&context, args),
        Command::Send(args) => run_send(&context, args),
        Command::Glance(args) => run_glance(&context, args),
        Command::Resume(args) => run_resume(&context, args),
        Command::Activity(args) => run_activity(&context, args),
        Command::WorkContext(args) => coordination::run_work_context(&context, args),
        Command::Broker(args) => coordination::run_broker(&context, args),
        Command::Message(args) => coordination::run_message(&context, args),
        Command::Serve(args) => serve::run_serve(&context, args),
        Command::CodexAppServerProxy(args) => codex_app_server::run_proxy(&context, args),
        Command::Delete(args) => run_delete(&context, args),
        Command::Completion(_) => unreachable!("completion is handled before context resolution"),
    }
}

fn coordination_command_name(command: &Command) -> Option<&'static str> {
    match command {
        Command::WorkContext(args) => Some(match &args.command {
            cli::WorkContextCommand::Status(_) => "work-context-status",
            cli::WorkContextCommand::Set(_) => "work-context-set",
            cli::WorkContextCommand::Clear(_) => "work-context-clear",
            cli::WorkContextCommand::Advise(_) => "work-context-advise",
            cli::WorkContextCommand::Acknowledge(_) => "work-context-acknowledge",
            cli::WorkContextCommand::Claim(_) => "work-context-claim",
            cli::WorkContextCommand::Show(_) => "work-context-show",
            cli::WorkContextCommand::Check(_) => "work-context-check",
            cli::WorkContextCommand::Renew(_) => "work-context-renew",
            cli::WorkContextCommand::Release(_) => "work-context-release",
            cli::WorkContextCommand::Admit(_) => "work-context-admit",
            cli::WorkContextCommand::Complete(_) => "work-context-complete",
            cli::WorkContextCommand::Reconcile(_) => "work-context-reconcile",
        }),
        Command::Broker(args) => Some(match &args.command {
            cli::BrokerCommand::Status(_) => "broker-status",
            cli::BrokerCommand::Adopt(_) => "broker-adopt",
            cli::BrokerCommand::Reconcile(_) => "broker-reconcile",
            cli::BrokerCommand::Stop(_) => "broker-stop",
            cli::BrokerCommand::Heartbeat(_) => "broker-heartbeat",
        }),
        Command::Message(args) => Some(match &args.command {
            cli::MessageCommand::Send(_) => "message-send",
            cli::MessageCommand::Inbox(_) => "message-inbox",
            cli::MessageCommand::Show(_) => "message-show",
            cli::MessageCommand::Ack(_) => "message-ack",
            cli::MessageCommand::Reply(_) => "message-reply",
            cli::MessageCommand::Wait(_) => "message-wait",
        }),
        _ => None,
    }
}

fn coordination_leaf_from_raw_args(args: &[OsString]) -> Option<&'static str> {
    args.windows(2).find_map(|pair| {
        let group = pair[0].to_str()?;
        let leaf = pair[1].to_str()?;
        match (group, leaf) {
            ("work-context", "status") => Some("work-context-status"),
            ("work-context", "set") => Some("work-context-set"),
            ("work-context", "clear") => Some("work-context-clear"),
            ("work-context", "advise") => Some("work-context-advise"),
            ("work-context", "acknowledge") => Some("work-context-acknowledge"),
            ("work-context", "claim") => Some("work-context-claim"),
            ("work-context", "show") => Some("work-context-show"),
            ("work-context", "check") => Some("work-context-check"),
            ("work-context", "renew") => Some("work-context-renew"),
            ("work-context", "release") => Some("work-context-release"),
            ("work-context", "admit") => Some("work-context-admit"),
            ("work-context", "complete") => Some("work-context-complete"),
            ("work-context", "reconcile") => Some("work-context-reconcile"),
            ("broker", "status") => Some("broker-status"),
            ("broker", "adopt") => Some("broker-adopt"),
            ("broker", "reconcile") => Some("broker-reconcile"),
            ("broker", "stop") => Some("broker-stop"),
            ("message", "send") => Some("message-send"),
            ("message", "inbox") => Some("message-inbox"),
            ("message", "show") => Some("message-show"),
            ("message", "ack") => Some("message-ack"),
            ("message", "reply") => Some("message-reply"),
            ("message", "wait") => Some("message-wait"),
            _ => None,
        }
    })
}

fn command_format(command: &Command) -> OutputFormat {
    match command {
        Command::Start(args) => args.format,
        Command::Run(args) => args.format,
        Command::List(args) => args.format,
        Command::Show(args) => args.format,
        Command::Logs(args) => args.format,
        Command::Send(args) => args.format,
        Command::Glance(args) => args.format,
        Command::Resume(args) => args.format,
        Command::Activity(args) => match &args.command {
            cli::ActivityCommand::Event(args) => args.format,
            cli::ActivityCommand::Status(args) => args.format,
            cli::ActivityCommand::Hook(_) | cli::ActivityCommand::Notify(_) => OutputFormat::Text,
            cli::ActivityCommand::Doctor(args) => args.format,
            cli::ActivityCommand::Setup(args) => args.format,
        },
        Command::WorkContext(args) => match &args.command {
            cli::WorkContextCommand::Status(args) => args.format,
            cli::WorkContextCommand::Set(args) => args.format,
            cli::WorkContextCommand::Clear(args) => args.format,
            cli::WorkContextCommand::Advise(args) => args.format,
            cli::WorkContextCommand::Acknowledge(args) => args.format,
            cli::WorkContextCommand::Claim(args) => args.format,
            cli::WorkContextCommand::Show(args) => args.format,
            cli::WorkContextCommand::Check(args) => args.format,
            cli::WorkContextCommand::Renew(args) => args.format,
            cli::WorkContextCommand::Release(args) => args.format,
            cli::WorkContextCommand::Admit(args) => args.format,
            cli::WorkContextCommand::Complete(args) => args.format,
            cli::WorkContextCommand::Reconcile(args) => args.format,
        },
        Command::Broker(args) => match &args.command {
            cli::BrokerCommand::Status(args) => args.format,
            cli::BrokerCommand::Adopt(args) | cli::BrokerCommand::Reconcile(args) => args.format,
            cli::BrokerCommand::Stop(args) => args.format,
            cli::BrokerCommand::Heartbeat(args) => args.format,
        },
        Command::Message(args) => match &args.command {
            cli::MessageCommand::Send(args) => args.format,
            cli::MessageCommand::Inbox(args) => args.format,
            cli::MessageCommand::Show(args) => args.format,
            cli::MessageCommand::Ack(args) => args.format,
            cli::MessageCommand::Reply(args) => args.format,
            cli::MessageCommand::Wait(args) => args.format,
        },
        Command::Delete(args) => args.format,
        Command::Attach(_)
        | Command::Serve(_)
        | Command::CodexAppServerProxy(_)
        | Command::Completion(_) => OutputFormat::Text,
    }
}

fn detect_format_from_args(args: &[OsString]) -> OutputFormat {
    let mut index = 1;
    while index < args.len() {
        let Some(arg) = args[index].to_str() else {
            index += 1;
            continue;
        };
        if arg == "--format" {
            if args
                .get(index + 1)
                .and_then(|value| value.to_str())
                .is_some_and(|value| value.eq_ignore_ascii_case("json"))
            {
                return OutputFormat::Json;
            }
            index += 2;
            continue;
        }
        if let Some(value) = arg.strip_prefix("--format=")
            && value.eq_ignore_ascii_case("json")
        {
            return OutputFormat::Json;
        }
        index += 1;
    }
    OutputFormat::Text
}

fn render_clap_message(err: &clap::Error) -> String {
    // clap prints the missing argument names on indented continuation lines that
    // the first-non-empty-line collapse below would drop, leaving an unnamed
    // "the following required arguments were not provided:" message. Pull the
    // names from the structured error context so the envelope names them.
    if err.kind() == clap::error::ErrorKind::MissingRequiredArgument
        && let Some(clap::error::ContextValue::Strings(missing)) =
            err.get(clap::error::ContextKind::InvalidArg)
        && !missing.is_empty()
    {
        return format!(
            "the following required arguments were not provided: {}",
            missing.join(", ")
        );
    }
    let rendered = err.to_string();
    rendered
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(|line| {
            let line = line.trim();
            line.strip_prefix("error:")
                .map(str::trim)
                .unwrap_or(line)
                .to_string()
        })
        .unwrap_or_else(|| "command-line parse failed".to_string())
}

fn run_start(context: &CliContext, args: cli::StartArgs) -> i32 {
    let format = args.format;
    match start_session(
        context,
        args,
        StartFailureDisposition::ReturnError,
        PromptDelivery::ResilientBeforeSubmit,
    ) {
        Ok(view) => render_single_success(
            START_COMMAND,
            view.format,
            &view.result,
            render_started_text,
        ),
        Err(err) => render_error(START_COMMAND, format, err),
    }
}

fn run_one_shot(context: &CliContext, args: cli::RunArgs) -> i32 {
    let format = args.format;
    match start_run_session(context, args) {
        Ok(view) => {
            render_single_success(RUN_COMMAND, view.format, &view.result, render_started_text)
        }
        Err(err) => render_error(RUN_COMMAND, format, err),
    }
}

fn run_list(context: &CliContext, args: cli::ListArgs) -> i32 {
    match list_sessions(context, None) {
        Ok(results) => render_list_success(args.format, &results),
        Err(err) => render_error(LIST_COMMAND, args.format, err),
    }
}

fn run_command(context: &CliContext, args: cli::SessionRefArgs) -> i32 {
    match load_session_view(context, &args.id, None) {
        Ok(view) => render_single_success(COMMAND_COMMAND, args.format, &view, render_command_text),
        Err(err) => render_error(COMMAND_COMMAND, args.format, err),
    }
}

fn run_attach(context: &CliContext, args: cli::AttachArgs) -> i32 {
    match load_session_record(context, &args.id) {
        Ok(record) => {
            let tmux_bin = resolve_tmux_bin(args.tmux_bin.as_deref());
            let status = ProcessCommand::new(&tmux_bin)
                .arg("attach-session")
                .arg("-t")
                .arg(&record.tmux_session)
                .status();
            match status {
                Ok(status) if status.success() => exit::SUCCESS,
                Ok(status) => {
                    eprintln!("error: tmux attach failed with status {status}");
                    exit::RUNTIME
                }
                Err(err) => {
                    eprintln!("error: failed to run {}: {err}", tmux_bin.display());
                    exit::RUNTIME
                }
            }
        }
        Err(err) => render_error("attach", OutputFormat::Text, err),
    }
}

fn run_logs(context: &CliContext, args: cli::LogsArgs) -> i32 {
    match load_session_record(context, &args.id).and_then(|record| {
        session_logs(
            context,
            &record,
            args.tail,
            &resolve_tmux_bin(args.tmux_bin.as_deref()),
        )
    }) {
        Ok(result) => render_single_success(LOGS_COMMAND, args.format, &result, render_logs_text),
        Err(err) => render_error(LOGS_COMMAND, args.format, err),
    }
}

fn run_send(context: &CliContext, args: cli::SendArgs) -> i32 {
    let format = args.format;
    match send_to_session(context, args) {
        Ok(result) => render_single_success(SEND_COMMAND, format, &result, render_send_text),
        Err(err) => render_error(SEND_COMMAND, format, err),
    }
}

fn run_glance(context: &CliContext, args: cli::GlanceArgs) -> i32 {
    let format = args.format;
    match glance_session(context, args) {
        Ok(result) => render_single_success(GLANCE_COMMAND, format, &result, render_glance_text),
        Err(err) => render_error(GLANCE_COMMAND, format, err),
    }
}

fn run_resume(context: &CliContext, args: cli::ResumeArgs) -> i32 {
    let format = args.format;
    match resume_session(context, args) {
        Ok(result) => render_single_success(RESUME_COMMAND, format, &result, render_resumed_text),
        Err(err) => render_error(RESUME_COMMAND, format, err),
    }
}

fn run_activity(context: &CliContext, args: cli::ActivityArgs) -> i32 {
    match args.command {
        cli::ActivityCommand::Event(args) => {
            let format = args.format;
            let retry_agent = std::env::var(activity::ACTIVITY_RETRY_PROVIDER_ENV)
                .ok()
                .and_then(|provider| AgentKind::from_name(&provider));
            let result = activity::read_event_from_stdin().and_then(|event| {
                if retry_agent.is_some() {
                    activity::ingest_event_retry(context, &args.id, event)
                } else {
                    activity::ingest_event(context, &args.id, event)
                }
            });
            if let Some(agent) = retry_agent {
                match &result {
                    Ok(_) => activity::clear_hook_diagnostic(context, agent),
                    Err(error) => activity::record_hook_diagnostic(context, agent, error.code()),
                }
            }
            match result {
                Ok(result) => render_single_success(
                    ACTIVITY_EVENT_COMMAND,
                    format,
                    &result,
                    render_activity_text,
                ),
                Err(err) => render_error(ACTIVITY_EVENT_COMMAND, format, err),
            }
        }
        cli::ActivityCommand::Status(args) => match activity::activity_status(context, &args.id) {
            Ok(result) => render_single_success(
                ACTIVITY_STATUS_COMMAND,
                args.format,
                &result,
                render_activity_text,
            ),
            Err(err) => render_error(ACTIVITY_STATUS_COMMAND, args.format, err),
        },
        cli::ActivityCommand::Hook(args) => {
            // Provider telemetry is deliberately fail-open: malformed or stale
            // hook input must never block a prompt, permission, or turn.
            activity::ingest_provider_hook_fail_open(context, args.agent, args.event.as_deref());
            exit::SUCCESS
        }
        cli::ActivityCommand::Notify(args) => {
            // Provider notifications are auxiliary telemetry. Invalid, stale,
            // or mismatched input must never change Codex's own exit behavior.
            activity::ingest_provider_notification_fail_open(context, args.agent, &args.payload);
            activity::forward_provider_notification_fail_open(
                args.agent,
                args.forward_notify_argv_json.as_deref(),
                &args.payload,
            );
            exit::SUCCESS
        }
        cli::ActivityCommand::Doctor(args) => match activity::doctor(context, args.agent) {
            Ok(result) => render_single_success(
                ACTIVITY_DOCTOR_COMMAND,
                args.format,
                &result,
                render_doctor_text,
            ),
            Err(err) => render_error(ACTIVITY_DOCTOR_COMMAND, args.format, err),
        },
        cli::ActivityCommand::Setup(args) => match forward_activity_setup_to_agent_hook(&args) {
            Ok(result) => render_single_success(
                ACTIVITY_SETUP_COMMAND,
                args.format,
                &result,
                render_forwarded_setup_text,
            ),
            Err(err) => render_error(ACTIVITY_SETUP_COMMAND, args.format, err),
        },
    }
}

fn forward_activity_setup_to_agent_hook(args: &cli::ActivitySetupArgs) -> Result<Value, CliError> {
    let explicit_binary = std::env::var_os("AGENT_HOOK_BIN");
    let binary = explicit_binary
        .clone()
        .unwrap_or_else(|| OsString::from("agent-hook"));
    let action = if args.dry_run {
        "--dry-run"
    } else if args.apply {
        "--apply"
    } else if args.remove {
        "--remove"
    } else {
        "--repair"
    };
    let mut command = ProcessCommand::new(binary);
    command.args([
        "setup",
        "--product",
        args.agent.as_str(),
        action,
        "--format",
        "json",
    ]);
    if let Some(digest) = args.expected_preview_digest.as_deref() {
        command.args(["--expected-plan-digest", digest]);
    }
    let output = match run_output_with_timeout_and_cap(
        command,
        Duration::from_secs(10),
        AGENT_HOOK_SETUP_MAX_OUTPUT_BYTES,
    ) {
        Ok(output) => output,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(CliError::unavailable(
                "agent-hook-setup-unavailable",
                "agent-hook is required for provider setup; install the matching nils-agent-hook binary, then rerun the same dry-run before any apply, repair, or remove",
                Some(json!({
                    "compatibility_owner": "agent-hook",
                    "action": "install-agent-hook-and-repeat-reviewed-preview"
                })),
            ));
        }
        Err(error) => {
            return Err(CliError::unavailable(
                "agent-hook-setup-unavailable",
                format!("agent-hook setup compatibility forward failed: {error}"),
                Some(json!({
                    "compatibility_owner": "agent-hook",
                    "action": "inspect-agent-hook-installation-and-repeat-reviewed-preview"
                })),
            ));
        }
    };
    let envelope: Envelope<Value> = match serde_json::from_slice(&output.stdout) {
        Ok(value) => value,
        Err(_) => {
            return Err(CliError::data(
                "agent-hook-setup-output-invalid",
                "agent-hook setup returned invalid JSON",
                None,
            ));
        }
    };
    if envelope.schema_version != "cli.agent-hook.setup.v1" {
        return Err(CliError::data(
            "agent-hook-setup-output-invalid",
            "agent-hook setup returned an unsupported envelope schema",
            None,
        ));
    }
    if !output.status.success() || !envelope.ok {
        let (code, message, details) = envelope.error.map_or_else(
            || {
                (
                    "agent-hook-setup-failed".to_string(),
                    "agent-hook setup rejected the compatibility request".to_string(),
                    None,
                )
            },
            |error| (error.code, error.message, error.details),
        );
        let child_exit = output
            .status
            .code()
            .filter(|code| {
                matches!(
                    *code,
                    exit::RUNTIME | exit::USAGE | exit::DATA | exit::UNAVAILABLE | exit::SOFTWARE
                )
            })
            .unwrap_or(exit::DATA);
        return Err(CliError::with_exit_code(code, message, details, child_exit));
    }
    let typed: Envelope<agent_hook::setup::SetupResult> = serde_json::from_slice(&output.stdout)
        .map_err(|_| {
            CliError::data(
                "agent-hook-setup-output-invalid",
                "agent-hook setup returned an incomplete success contract",
                None,
            )
        })?;
    let result = typed.data.ok_or_else(|| {
        CliError::data(
            "agent-hook-setup-output-invalid",
            "agent-hook setup omitted its data object",
            None,
        )
    })?;
    if result.schema_version != "agent-hook.setup-result.v1"
        || result.product != args.agent.as_str()
        || result.action != action.trim_start_matches("--")
        || result.compatibility_owner != "agent-hook"
        || !valid_sha256(&result.plan_digest)
        || !valid_sha256(&result.config_digest)
        || !valid_sha256(&result.policy_digest)
    {
        return Err(CliError::data(
            "agent-hook-setup-output-invalid",
            "agent-hook setup response does not match the requested operation",
            None,
        ));
    }
    let mut result = serde_json::to_value(result)
        .ok()
        .and_then(|value| value.as_object().cloned())
        .ok_or_else(|| {
            CliError::data(
                "agent-hook-setup-output-invalid",
                "agent-hook setup result could not be projected",
                None,
            )
        })?;
    result.insert(
        "compatibility_owner".to_string(),
        Value::String("agent-hook".to_string()),
    );
    Ok(Value::Object(result))
}

fn render_forwarded_setup_text(result: &Value) -> String {
    let product = result
        .get("product")
        .and_then(Value::as_str)
        .unwrap_or("provider");
    let action = result
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("setup");
    let status = result
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("reported");
    format!("{product} activity {action}: {status} (owner: agent-hook)\n")
}

fn valid_sha256(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    })
}

fn run_delete(context: &CliContext, args: cli::DeleteArgs) -> i32 {
    match delete_session(
        context,
        &args.id,
        resolve_tmux_bin(args.tmux_bin.as_deref()),
    ) {
        Ok(result) => {
            render_single_success(DELETE_COMMAND, args.format, &result, render_delete_text)
        }
        Err(err) => render_error(DELETE_COMMAND, args.format, err),
    }
}

#[derive(Debug, Clone)]
struct CliContext {
    state_dir: PathBuf,
    host: Option<String>,
}

impl CliContext {
    fn resolve(state_dir: Option<PathBuf>, host: Option<String>) -> Result<Self, CliError> {
        let state_dir = resolve_state_dir(state_dir)?;
        let host = resolve_host(
            host.or_else(|| non_empty_env("AGENT_SESSION_HOST"))
                .or_else(short_hostname),
        )?;
        Ok(Self { state_dir, host })
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct SessionRecord {
    schema_version: String,
    id: String,
    agent: String,
    mode: String,
    #[serde(default)]
    coordination_mode: cli::CoordinationMode,
    title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    title_state: Option<SessionTitleState>,
    #[serde(default)]
    title_revision: u64,
    cwd: String,
    tmux_session: String,
    prompt_file: Option<String>,
    log_file: Option<String>,
    created_at: String,
    updated_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    provider_resume: Option<ProviderResume>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    runtime: Option<RuntimeInfo>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    agent_args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    agent_bin: Option<String>,
    #[serde(default, flatten, skip_serializing_if = "BTreeMap::is_empty")]
    extra: BTreeMap<String, Value>,
    #[serde(skip)]
    resume_sidecar_extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum SessionTitleTopicSource {
    None,
    Auto,
    User,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
struct SessionTitleState {
    topic: Option<String>,
    topic_source: SessionTitleTopicSource,
    #[serde(default)]
    references: Vec<String>,
    activity: Option<String>,
    #[serde(default, flatten, skip_serializing_if = "BTreeMap::is_empty")]
    extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionTitleStateInput {
    topic: Option<String>,
    topic_source: SessionTitleTopicSource,
    #[serde(default)]
    references: Vec<String>,
    activity: Option<String>,
}

impl From<SessionTitleStateInput> for SessionTitleState {
    fn from(input: SessionTitleStateInput) -> Self {
        Self {
            topic: input.topic,
            topic_source: input.topic_source,
            references: input.references,
            activity: input.activity,
            extra: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct SessionTitleStateView {
    topic: Option<String>,
    topic_source: SessionTitleTopicSource,
    references: Vec<String>,
    activity: Option<String>,
}

impl From<&SessionTitleState> for SessionTitleStateView {
    fn from(state: &SessionTitleState) -> Self {
        Self {
            topic: state.topic.clone(),
            topic_source: state.topic_source.clone(),
            references: state.references.clone(),
            activity: state.activity.clone(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct ProviderResume {
    provider: String,
    session_id: String,
    captured_at: String,
    capture_method: String,
    resume_args: Vec<String>,
    #[serde(default, flatten, skip_serializing_if = "BTreeMap::is_empty")]
    extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize)]
struct ProviderResumeView {
    provider: String,
    session_id: String,
    captured_at: String,
    capture_method: String,
    resume_args: Vec<String>,
}

impl From<&ProviderResume> for ProviderResumeView {
    fn from(provider_resume: &ProviderResume) -> Self {
        Self {
            provider: provider_resume.provider.clone(),
            session_id: provider_resume.session_id.clone(),
            captured_at: provider_resume.captured_at.clone(),
            capture_method: provider_resume.capture_method.clone(),
            resume_args: provider_resume.resume_args.clone(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct RuntimeInfo {
    kind: String,
    tmux_session: String,
    generation: u64,
    started_at: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    launch_id: String,
    #[serde(default, flatten, skip_serializing_if = "BTreeMap::is_empty")]
    extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq, Serialize)]
struct StartupProjection {
    schema_version: String,
    state: String,
    stage: String,
    started_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    failure_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    occurred_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    retry_safe: Option<bool>,
    #[serde(default, flatten, skip_serializing_if = "BTreeMap::is_empty")]
    extra: BTreeMap<String, Value>,
}

fn startup_stage_is_allowed(stage: &str) -> bool {
    matches!(
        stage,
        "record"
            | "tmux"
            | "runtime"
            | "app_server"
            | "proxy"
            | "provider_client"
            | "initial_connection"
    )
}

fn startup_failure_details(
    code: &str,
    observed_stage: &str,
) -> Option<(&'static str, &'static str, bool)> {
    match code {
        "runtime-helper-unavailable" => Some((
            "proxy",
            "Session runtime helper is unavailable after an upgrade.",
            true,
        )),
        "agent-binary-unavailable" => Some((
            "runtime",
            "The selected agent CLI could not be started.",
            false,
        )),
        "working-directory-unavailable" => Some((
            "record",
            "The selected working directory is unavailable.",
            false,
        )),
        "terminal-runtime-create-failed" => {
            Some(("tmux", "The terminal runtime could not be created.", true))
        }
        "app-server-start-failed" => Some((
            "app_server",
            "The agent control server did not become ready.",
            true,
        )),
        "proxy-start-failed" => Some((
            "proxy",
            "Agent Console could not connect the terminal to the agent runtime.",
            true,
        )),
        "provider-client-exited" => Some((
            "provider_client",
            "The agent exited before the session was ready.",
            true,
        )),
        "provider-configuration-rejected" => Some((
            "provider_client",
            "The agent needs configuration or sign-in on this machine.",
            false,
        )),
        "startup-timeout" if startup_stage_is_allowed(observed_stage) => Some((
            match observed_stage {
                "record" => "record",
                "tmux" => "tmux",
                "runtime" => "runtime",
                "app_server" => "app_server",
                "proxy" => "proxy",
                "provider_client" => "provider_client",
                "initial_connection" => "initial_connection",
                _ => unreachable!("stage checked above"),
            },
            "Session startup timed out; the outcome may be uncertain.",
            false,
        )),
        "startup-exited" if startup_stage_is_allowed(observed_stage) => Some((
            match observed_stage {
                "record" => "record",
                "tmux" => "tmux",
                "runtime" => "runtime",
                "app_server" => "app_server",
                "proxy" => "proxy",
                "provider_client" => "provider_client",
                "initial_connection" => "initial_connection",
                _ => unreachable!("stage checked above"),
            },
            "The session stopped during startup.",
            true,
        )),
        _ => None,
    }
}

fn startup_projection_is_valid(startup: &StartupProjection) -> bool {
    if startup.schema_version != STARTUP_PROJECTION_VERSION
        || !startup_stage_is_allowed(&startup.stage)
        || startup.started_at.parse::<Timestamp>().is_err()
    {
        return false;
    }
    match startup.state.as_str() {
        "starting" | "ready" => {
            startup.failure_code.is_none()
                && startup.message.is_none()
                && startup.occurred_at.is_none()
                && startup.retry_safe.is_none()
        }
        "failed" => {
            let (Some(code), Some(message), Some(occurred_at), Some(retry_safe)) = (
                startup.failure_code.as_deref(),
                startup.message.as_deref(),
                startup.occurred_at.as_deref(),
                startup.retry_safe,
            ) else {
                return false;
            };
            let Some((stage, expected_message, expected_retry_safe)) =
                startup_failure_details(code, &startup.stage)
            else {
                return false;
            };
            startup.stage == stage
                && message == expected_message
                && retry_safe == expected_retry_safe
                && occurred_at.parse::<Timestamp>().is_ok()
        }
        _ => false,
    }
}

fn startup_projection(record: &SessionRecord) -> Option<StartupProjection> {
    let startup =
        serde_json::from_value::<StartupProjection>(record.extra.get(STARTUP_EXTRA_KEY)?.clone())
            .ok()?;
    startup_projection_is_valid(&startup).then_some(startup)
}

fn store_startup_projection(record: &mut SessionRecord, startup: &StartupProjection) {
    let mut durable = startup.clone();
    if let Some(current) = startup_projection(record) {
        for (key, value) in current.extra {
            durable.extra.entry(key).or_insert(value);
        }
    }
    record.extra.insert(
        STARTUP_EXTRA_KEY.to_string(),
        serde_json::to_value(durable).expect("startup projection is serializable"),
    );
}

fn startup_projection_for_view(record: &SessionRecord) -> Option<StartupProjection> {
    let mut startup = startup_projection(record)?;
    startup.extra.clear();
    Some(startup)
}

pub(crate) fn runtime_is_proven_never_launched(record: &SessionRecord) -> bool {
    let Some(current_launch_id) = record
        .runtime
        .as_ref()
        .map(|runtime| runtime.launch_id.as_str())
        .filter(|launch_id| !launch_id.is_empty())
    else {
        return false;
    };
    record
        .extra
        .get(TMUX_RUNTIME_NEVER_LAUNCHED_KEY)
        .and_then(Value::as_str)
        .is_some_and(|marker| marker == current_launch_id)
}

fn mark_tmux_runtime_never_launched(record: &mut SessionRecord) {
    if let Some(launch_id) = record
        .runtime
        .as_ref()
        .map(|runtime| runtime.launch_id.as_str())
        .filter(|launch_id| !launch_id.is_empty())
    {
        record.extra.insert(
            TMUX_RUNTIME_NEVER_LAUNCHED_KEY.to_string(),
            Value::String(launch_id.to_string()),
        );
    }
}

fn starting_projection(started_at: &str, stage: &str) -> StartupProjection {
    debug_assert!(startup_stage_is_allowed(stage));
    StartupProjection {
        schema_version: STARTUP_PROJECTION_VERSION.to_string(),
        state: "starting".to_string(),
        stage: stage.to_string(),
        started_at: started_at.to_string(),
        failure_code: None,
        message: None,
        occurred_at: None,
        retry_safe: None,
        extra: BTreeMap::new(),
    }
}

fn ready_projection(started_at: &str) -> StartupProjection {
    StartupProjection {
        state: "ready".to_string(),
        stage: "initial_connection".to_string(),
        ..starting_projection(started_at, "initial_connection")
    }
}

fn failed_projection(
    started_at: &str,
    code: &str,
    observed_stage: &str,
    occurred_at: Option<&str>,
) -> StartupProjection {
    let (effective_code, (stage, message, retry_safe)) =
        startup_failure_details(code, observed_stage)
            .map(|details| (code, details))
            .unwrap_or_else(|| {
                (
                    "startup-exited",
                    startup_failure_details("startup-exited", observed_stage)
                        .expect("allowed startup stage has a generic failure"),
                )
            });
    StartupProjection {
        schema_version: STARTUP_PROJECTION_VERSION.to_string(),
        state: "failed".to_string(),
        stage: stage.to_string(),
        started_at: started_at.to_string(),
        failure_code: Some(effective_code.to_string()),
        message: Some(message.to_string()),
        occurred_at: Some(
            occurred_at
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| Zoned::now().timestamp().to_string()),
        ),
        retry_safe: Some(retry_safe),
        extra: BTreeMap::new(),
    }
}

fn read_bounded_startup_marker(path: &Path) -> Option<String> {
    let metadata = fs::metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > 64 {
        return None;
    }
    let value = fs::read_to_string(path).ok()?;
    let value = value.trim();
    (!value.is_empty() && value.len() <= 64 && !value.chars().any(char::is_whitespace))
        .then(|| value.to_string())
}

fn startup_stage_marker(context: &CliContext, record: &SessionRecord) -> Option<String> {
    let stage =
        read_bounded_startup_marker(&session_dir(context, &record.id).join(STARTUP_STAGE_FILE))?;
    startup_stage_is_allowed(&stage).then_some(stage)
}

fn startup_failure_marker(
    context: &CliContext,
    record: &SessionRecord,
) -> Option<(String, String)> {
    let path = session_dir(context, &record.id).join(STARTUP_FAILURE_FILE);
    let code = read_bounded_startup_marker(&path)?;
    startup_failure_details(&code, "runtime")?;
    let seconds = fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_secs()
        .try_into()
        .ok()?;
    let occurred_at = Timestamp::from_second(seconds).ok()?.to_string();
    Some((code, occurred_at))
}

fn stopped_startup_failure_code(stage: &str) -> &'static str {
    match stage {
        "tmux" => "terminal-runtime-create-failed",
        "app_server" => "app-server-start-failed",
        "proxy" => "proxy-start-failed",
        "provider_client" | "initial_connection" => "provider-client-exited",
        _ => "startup-exited",
    }
}

fn desired_startup_projection(
    context: &CliContext,
    record: &SessionRecord,
    status: &str,
) -> Option<StartupProjection> {
    let current = startup_projection(record)?;
    if current.state != "starting" {
        return None;
    }
    let observed_stage =
        startup_stage_marker(context, record).unwrap_or_else(|| current.stage.clone());
    if let Some((code, occurred_at)) = startup_failure_marker(context, record) {
        return Some(failed_projection(
            &current.started_at,
            &code,
            &observed_stage,
            Some(&occurred_at),
        ));
    }
    if status == "stopped" {
        if observed_stage == "initial_connection" {
            return Some(ready_projection(&current.started_at));
        }
        let code = stopped_startup_failure_code(&observed_stage);
        return Some(failed_projection(
            &current.started_at,
            code,
            &observed_stage,
            None,
        ));
    }
    if status == "running" {
        if codex_app_server::runtime_is_supported(record) {
            if observed_stage == "initial_connection" {
                return Some(ready_projection(&current.started_at));
            }
            if observed_stage != current.stage {
                return Some(starting_projection(&current.started_at, &observed_stage));
            }
            return None;
        }
        return Some(ready_projection(&current.started_at));
    }
    None
}

fn reconcile_owned_startup_projection(
    context: &CliContext,
    record: &mut SessionRecord,
    status: &str,
) {
    let Some(desired) = desired_startup_projection(context, record, status) else {
        return;
    };
    if startup_projection(record).as_ref() == Some(&desired) {
        return;
    }
    let previous = startup_projection(record);
    let previous_updated_at = record.updated_at.clone();
    store_startup_projection(record, &desired);
    record.updated_at = Zoned::now().timestamp().to_string();
    if write_session_record(context, record).is_err() {
        if let Some(previous) = previous {
            store_startup_projection(record, &previous);
        }
        record.updated_at = previous_updated_at;
    }
}

fn advance_owned_startup_stage(
    context: &CliContext,
    record: &mut SessionRecord,
    stage: &str,
) -> Result<(), CliError> {
    let Some(current) = startup_projection(record) else {
        return Ok(());
    };
    if current.state != "starting" || current.stage == stage {
        return Ok(());
    }
    let previous_updated_at = record.updated_at.clone();
    store_startup_projection(record, &starting_projection(&current.started_at, stage));
    record.updated_at = Zoned::now().timestamp().to_string();
    if let Err(err) = write_session_record(context, record) {
        store_startup_projection(record, &current);
        record.updated_at = previous_updated_at;
        return Err(err);
    }
    Ok(())
}

fn reconcile_startup_projection(
    context: &CliContext,
    observed: SessionRecord,
    observed_status: String,
    bulk_snapshot_started_at: Option<&Timestamp>,
) -> (SessionRecord, String) {
    if startup_projection(&observed).is_none_or(|startup| startup.state != "starting") {
        return (observed, observed_status);
    }
    let lock = match try_acquire_session_record_lock(context, &observed.id) {
        Ok(Some(lock)) => lock,
        Ok(None) | Err(_) => return (observed, observed_status),
    };
    let mut current = match load_session_record(context, &observed.id) {
        Ok(current) if ensure_same_session_identity(&observed, &current).is_ok() => current,
        _ => return (observed, observed_status),
    };
    if current.updated_at != observed.updated_at
        || startup_projection(&current) != startup_projection(&observed)
    {
        return (current, observed_status);
    }
    if let Some(snapshot_started_at) = bulk_snapshot_started_at
        && current
            .updated_at
            .parse::<Timestamp>()
            .ok()
            .is_none_or(|updated_at| updated_at >= *snapshot_started_at)
    {
        return (current, observed_status);
    }
    let Some(desired) = desired_startup_projection(context, &current, &observed_status) else {
        return (current, observed_status);
    };
    if startup_projection(&current).as_ref() == Some(&desired) {
        return (current, observed_status);
    }
    store_startup_projection(&mut current, &desired);
    current.updated_at = Zoned::now().timestamp().to_string();
    if write_session_record(context, &current).is_err() {
        return (observed, observed_status);
    }
    drop(lock);
    (current, observed_status)
}

fn same_runtime_identity(left: Option<&RuntimeInfo>, right: Option<&RuntimeInfo>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => {
            left.generation == right.generation
                && left.launch_id == right.launch_id
                && left.tmux_session == right.tmux_session
        }
        (None, None) => true,
        _ => false,
    }
}

fn ensure_same_session_identity(
    observed: &SessionRecord,
    current: &SessionRecord,
) -> Result<(), CliError> {
    if observed.id == current.id
        && observed.agent == current.agent
        && observed.created_at == current.created_at
        && observed.tmux_session == current.tmux_session
        && same_runtime_identity(observed.runtime.as_ref(), current.runtime.as_ref())
    {
        return Ok(());
    }
    Err(CliError::runtime(
        "session-runtime-changed",
        "session identity changed while waiting for a lifecycle operation",
        Some(json!({ "id": observed.id })),
    ))
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct DurableResumeRecord {
    schema_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    provider_resume: Option<ProviderResume>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    runtime: Option<RuntimeInfo>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    agent_args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    agent_bin: Option<String>,
    #[serde(default, flatten, skip_serializing_if = "BTreeMap::is_empty")]
    extra: BTreeMap<String, Value>,
}

#[derive(Debug, Serialize)]
struct SessionView {
    id: String,
    agent: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    capabilities: Vec<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    agent_profile: Option<String>,
    #[serde(skip)]
    profile_resume_context: Result<Option<DurableProfileResumeContext>, CliError>,
    mode: String,
    coordination_mode: cli::CoordinationMode,
    title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    title_state: Option<SessionTitleStateView>,
    title_state_supported: bool,
    title_revision: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_incarnation: Option<String>,
    cwd: String,
    tmux_session: String,
    status: String,
    resumable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    resume_blocked_reason: Option<String>,
    repo_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    provider_resume: Option<ProviderResumeView>,
    attach_command: String,
    ssh_attach_command: Option<String>,
    prompt_file: Option<String>,
    log_file: Option<String>,
    created_at: String,
    updated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_terminal_activity_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    runtime_started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    turn_state: Option<activity::TurnState>,
    /// Most recent user prompt, populated on demand by the list handler from the
    /// provider transcript (never persisted). Absent unless the daemon advertises
    /// the `last_prompt` capability and a preview was resolved.
    #[serde(skip_serializing_if = "Option::is_none")]
    last_prompt: Option<provider_prompt::LastPrompt>,
    /// Freshness/availability of the exact transcript-backed prompt projection.
    /// Populated only for eligible running Codex/Claude sessions.
    #[serde(skip_serializing_if = "Option::is_none")]
    last_prompt_state: Option<serve::LastPromptState>,
    /// Opaque process-local fence for exact transcript continuity.
    #[serde(skip_serializing_if = "Option::is_none")]
    last_prompt_continuity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    startup: Option<StartupProjection>,
    auto_resume: auto_resume::AutoResumeView,
    codex_account: codex_account::CodexAccountView,
    #[serde(flatten)]
    coordination: coordination::CoordinationSummary,
    #[serde(skip_serializing_if = "Option::is_none")]
    orchestration: Option<orchestration::SessionOrchestrationProjection>,
}

#[derive(Debug)]
struct StartView {
    format: OutputFormat,
    result: SessionView,
}

pub(crate) struct ProviderResumeImportArgs {
    pub(crate) agent: AgentKind,
    pub(crate) provider_resume_id: String,
    pub(crate) title: Option<String>,
    pub(crate) title_state: Option<SessionTitleState>,
    pub(crate) id: Option<String>,
    pub(crate) coordination_mode: cli::CoordinationMode,
    pub(crate) tmux_bin: Option<PathBuf>,
    pub(crate) agent_bin: Option<PathBuf>,
    pub(crate) agent_profile: Option<String>,
    pub(crate) provider_config_dir: Option<PathBuf>,
    pub(crate) profile_auto_resume_supported: Option<bool>,
    pub(crate) codex_usage_account: Option<String>,
    pub(crate) agent_args: Vec<String>,
    pub(crate) format: OutputFormat,
}

#[derive(Debug, Serialize)]
struct DeleteResult {
    id: String,
    tmux_session: String,
    killed: bool,
    deleted: bool,
    session_dir: String,
    #[serde(skip)]
    cleanup_pending: bool,
    #[serde(skip)]
    registry_fence: SessionRegistryFence,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
struct SessionRegistryFence {
    pub(crate) session_id: String,
    tmux_session: String,
    runtime_launch_id: Option<String>,
    runtime_generation: Option<u64>,
}

impl SessionRegistryFence {
    pub(crate) fn from_record(record: &SessionRecord) -> Self {
        Self {
            session_id: record.id.clone(),
            tmux_session: record.tmux_session.clone(),
            runtime_launch_id: record
                .runtime
                .as_ref()
                .map(|runtime| runtime.launch_id.clone())
                .filter(|launch_id| !launch_id.is_empty()),
            runtime_generation: record.runtime.as_ref().map(|runtime| runtime.generation),
        }
    }
}

#[derive(Debug, Serialize)]
struct LogsResult {
    id: String,
    source: String,
    text: String,
}

#[derive(Debug, Serialize)]
struct SendResult {
    id: String,
    tmux_session: String,
    sent_text: bool,
    keys: Vec<String>,
}

#[derive(Debug, Serialize)]
struct GlanceResult {
    id: String,
    agent: String,
    title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    title_state: Option<SessionTitleStateView>,
    title_state_supported: bool,
    title_revision: u64,
    tmux_session: String,
    status: String,
    resumable: bool,
    repo_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    provider_resume: Option<ProviderResumeView>,
    tail: String,
    created_at: String,
    updated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_terminal_activity_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    runtime_started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    turn_state: Option<activity::TurnState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    startup: Option<StartupProjection>,
    auto_resume: auto_resume::AutoResumeView,
    #[serde(flatten)]
    coordination: coordination::CoordinationSummary,
}

#[derive(Debug, Serialize)]
struct AttachmentResult {
    id: String,
    filename: String,
    path: String,
    bytes: usize,
    content_type: Option<String>,
}

#[derive(Debug, Serialize)]
struct WorkdirResult {
    path: String,
    name: String,
    root: String,
    is_git_repo: bool,
    last_used: Option<String>,
}

#[derive(Debug, Default, Clone, Copy)]
struct WorkdirSearchOptions {
    git_only: bool,
    exclude_worktrees: bool,
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct WorkdirUsage {
    entries: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
struct CliError(Box<CliErrorData>);

#[derive(Debug, Clone)]
struct CliErrorData {
    code: String,
    message: String,
    details: Option<Value>,
    /// Optional actionable remedy surfaced as the envelope `hint` (JSON) or a
    /// `hint:` line (text). None keeps the wire shape identical to before.
    hint: Option<String>,
    exit_code: i32,
}

impl CliError {
    fn code(&self) -> &str {
        &self.0.code
    }

    fn message(&self) -> &str {
        &self.0.message
    }

    fn usage(code: impl Into<String>, message: impl Into<String>, details: Option<Value>) -> Self {
        Self(Box::new(CliErrorData {
            code: code.into(),
            message: message.into(),
            details,
            hint: None,
            exit_code: exit::USAGE,
        }))
    }

    fn runtime(
        code: impl Into<String>,
        message: impl Into<String>,
        details: Option<Value>,
    ) -> Self {
        Self(Box::new(CliErrorData {
            code: code.into(),
            message: message.into(),
            details,
            hint: None,
            exit_code: exit::RUNTIME,
        }))
    }

    fn data(code: impl Into<String>, message: impl Into<String>, details: Option<Value>) -> Self {
        Self(Box::new(CliErrorData {
            code: code.into(),
            message: message.into(),
            details,
            hint: None,
            exit_code: exit::DATA,
        }))
    }

    fn unavailable(
        code: impl Into<String>,
        message: impl Into<String>,
        details: Option<Value>,
    ) -> Self {
        Self::with_exit_code(code, message, details, exit::UNAVAILABLE)
    }

    fn with_exit_code(
        code: impl Into<String>,
        message: impl Into<String>,
        details: Option<Value>,
        exit_code: i32,
    ) -> Self {
        Self(Box::new(CliErrorData {
            code: code.into(),
            message: message.into(),
            details,
            hint: None,
            exit_code,
        }))
    }

    /// Attach an actionable remedy hint to this error. Mirrors git-cli's
    /// `CliError::with_hint`; surfaced by `render_error` in both binaries.
    fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.0.hint = Some(hint.into());
        self
    }

    fn into_inner(self) -> CliErrorData {
        *self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StartFailureDisposition {
    ReturnError,
    ReturnSession,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PromptDelivery {
    /// Ordinary interactive starts may retry a paste while it is still proven
    /// unsubmitted. This covers provider TUIs that draw before accepting input.
    ResilientBeforeSubmit,
    /// Managed worker startup owns later recovery through a durable, guarded
    /// Enter. Its private assignment prompt must never be pasted more than once.
    ManagedWorkerExactlyOnce,
}

fn start_session(
    context: &CliContext,
    args: cli::StartArgs,
    failure_disposition: StartFailureDisposition,
    prompt_delivery: PromptDelivery,
) -> Result<StartView, CliError> {
    start_session_with_create_guard(context, args, failure_disposition, prompt_delivery, None)
}

fn start_session_with_create_guard(
    context: &CliContext,
    args: cli::StartArgs,
    failure_disposition: StartFailureDisposition,
    prompt_delivery: PromptDelivery,
    create_guard: Option<&mut dyn FnMut() -> Result<(), CliError>>,
) -> Result<StartView, CliError> {
    validate_agent_args(args.agent, &args.agent_args)?;
    let cwd = resolve_cwd(args.cwd.as_deref())?;
    let prompt = read_prompt(&args.prompt, args.prompt_file.as_deref(), args.prompt_stdin)?;
    let provider_plan = initial_provider_resume_plan(args.agent, &cwd);
    let launch_started_at = SystemTime::now();
    let tmux_bin = resolve_tmux_bin(args.tmux_bin.as_deref());
    let agent_bin = resolve_agent_bin(args.agent, args.agent_bin.as_deref());
    let mut created = create_record_with_guard(
        RecordRequest {
            context,
            agent: args.agent,
            mode: "interactive",
            coordination_mode: args.coordination_mode,
            title: args.title.as_deref(),
            title_state: args.initial_title_state,
            explicit_id: args.id.as_deref(),
            cwd: &cwd,
            prompt: prompt.as_deref(),
            log_file_name: None,
            provider_resume: provider_plan.provider_resume.clone(),
            agent_args: args.agent_args.clone(),
            agent_bin: Some(display_path(&agent_bin)),
        },
        create_guard,
    )?;
    persist_initial_profile_context(
        context,
        &mut created,
        args.initial_agent_profile.as_deref(),
        args.initial_provider_config_dir.as_deref(),
        args.initial_profile_auto_resume_supported,
        args.initial_codex_usage_account.as_deref(),
    )?;
    if let Err(err) = codex_account::set_initial_binding(
        &mut created.record,
        args.initial_codex_account.as_deref(),
    ) {
        cleanup_created_record(context, &created);
        return Err(err);
    }
    if args.initial_codex_account.is_some() {
        write_session_record(context, &created.record)?;
    }

    if let Err(err) = codex_app_server::configure_runtime(
        context,
        &agent_bin,
        &mut created.record,
        args.app_server_managed,
    ) {
        cleanup_created_record(context, &created);
        return Err(err);
    }

    let create_bootstrap = match codex_app_server::begin_create_bootstrap(&created.record) {
        Ok(guard) => guard,
        Err(err) => {
            cleanup_created_record(context, &created);
            return Err(err);
        }
    };

    if let Err(err) = advance_owned_startup_stage(context, &mut created.record, "tmux") {
        cleanup_created_record(context, &created);
        return Err(err);
    }

    let launch_identity = match start_interactive_tmux(
        &tmux_bin,
        &agent_bin,
        args.agent,
        &context.state_dir,
        &created.record,
        &provider_plan.launch_args,
        &args.agent_args,
    ) {
        Ok(identity) => identity,
        Err(err) => {
            if tmux_launch_may_have_created_runtime(&err)
                && let Err(termination_err) = recover_failed_tmux_launch(
                    context,
                    &mut created.record,
                    &tmux_bin,
                    None,
                    SessionTerminationOperation::FailedLaunch,
                )
            {
                return Err(termination_err);
            }
            mark_tmux_runtime_never_launched(&mut created.record);
            let started_at = startup_projection(&created.record)
                .map(|startup| startup.started_at)
                .unwrap_or_else(|| created.record.created_at.clone());
            let code = if err.code() == "codex-app-server-proxy-binary-unavailable" {
                "runtime-helper-unavailable"
            } else {
                "terminal-runtime-create-failed"
            };
            let failed = failed_projection(&started_at, code, "tmux", None);
            store_startup_projection(&mut created.record, &failed);
            created.record.updated_at = Zoned::now().timestamp().to_string();
            if let Err(write_err) = write_session_record(context, &created.record) {
                cleanup_created_record(context, &created);
                return Err(write_err);
            }
            let result =
                (failure_disposition == StartFailureDisposition::ReturnSession).then(|| {
                    session_view(
                        context,
                        &created.record,
                        Some("stopped".to_string()),
                        Some(&tmux_bin),
                    )
                });
            record_workdir_usage(context, &cwd);
            if let Some(create_bootstrap) = create_bootstrap {
                create_bootstrap.finish(|| created.release_lifecycle_lock());
            } else {
                created.release_lifecycle_lock();
            }
            return match result {
                Some(result) => Ok(StartView {
                    format: args.format,
                    result,
                }),
                None => Err(err),
            };
        }
    };
    if let Err(err) = persist_launched_tmux_identity(context, &mut created.record, &launch_identity)
    {
        recover_failed_tmux_launch(
            context,
            &mut created.record,
            &tmux_bin,
            Some(&launch_identity),
            SessionTerminationOperation::FailedLaunch,
        )?;
        cleanup_created_record(context, &created);
        return Err(err);
    }
    if let Err(err) = establish_coordination_broker(context, &created.record) {
        recover_failed_tmux_launch(
            context,
            &mut created.record,
            &tmux_bin,
            Some(&launch_identity),
            SessionTerminationOperation::FailedLaunch,
        )?;
        cleanup_created_record(context, &created);
        return Err(err);
    }
    if let Err(err) = release_held_runtime(context, &created.record) {
        recover_failed_tmux_launch(
            context,
            &mut created.record,
            &tmux_bin,
            Some(&launch_identity),
            SessionTerminationOperation::FailedLaunch,
        )?;
        cleanup_created_record(context, &created);
        return Err(err);
    }
    let _ = advance_owned_startup_stage(context, &mut created.record, "runtime");
    let mut prompt_delivery_error = None;
    if created.prompt_file.is_some() {
        if args.paste_delay_ms > 0 {
            thread::sleep(Duration::from_millis(args.paste_delay_ms));
        }
        if let Err(err) = paste_prompt(&tmux_bin, &created.record, prompt_delivery) {
            if prompt_delivery == PromptDelivery::ManagedWorkerExactlyOnce
                && err.code() == "managed-worker-prompt-delivery-outcome-unknown"
            {
                prompt_delivery_error = Some(err);
            } else {
                recover_failed_tmux_launch(
                    context,
                    &mut created.record,
                    &tmux_bin,
                    Some(&launch_identity),
                    SessionTerminationOperation::FailedLaunch,
                )?;
                cleanup_created_record(context, &created);
                return Err(err);
            }
        }
    }
    if created.record.provider_resume.is_none()
        && let Some(provider_resume) =
            capture_provider_resume_after_launch(args.agent, &created.record, launch_started_at)
    {
        let persisted_before_capture = created.record.clone();
        created.record.provider_resume = Some(provider_resume);
        if write_session_record(context, &created.record).is_err() {
            created.record = load_session_record(context, &created.record.id)
                .unwrap_or(persisted_before_capture);
        }
    }

    let status = session_status(&tmux_bin, &created.record);
    reconcile_owned_startup_projection(context, &mut created.record, &status);
    let result = session_view(context, &created.record, Some(status), Some(&tmux_bin));
    record_workdir_usage(context, &cwd);
    // Keep the app-server bootstrap marker valid for the entire create-lock
    // lifetime. Explicit ordering avoids Rust's reverse local-drop order from
    // removing the marker while the lifecycle lock is still held.
    if let Some(create_bootstrap) = create_bootstrap {
        create_bootstrap.finish(|| created.release_lifecycle_lock());
    } else {
        created.release_lifecycle_lock();
    }
    match prompt_delivery_error {
        Some(error) => Err(error),
        None => Ok(StartView {
            format: args.format,
            result,
        }),
    }
}

fn start_run_session(context: &CliContext, args: cli::RunArgs) -> Result<StartView, CliError> {
    validate_agent_args(args.agent, &args.agent_args)?;
    let cwd = resolve_cwd(args.cwd.as_deref())?;
    let prompt = read_prompt(&args.prompt, args.prompt_file.as_deref(), args.prompt_stdin)?;
    let prompt = prompt
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            CliError::usage(
                "missing-prompt",
                "run requires --prompt, --prompt-file, or --prompt-stdin",
                None,
            )
        })?;
    let log_file = Some("output.log");
    let mut created = create_record(RecordRequest {
        context,
        agent: args.agent,
        mode: "run",
        coordination_mode: args.coordination_mode,
        title: args.title.as_deref(),
        title_state: None,
        explicit_id: args.id.as_deref(),
        cwd: &cwd,
        prompt: Some(&prompt),
        log_file_name: log_file,
        provider_resume: None,
        agent_args: args.agent_args.clone(),
        agent_bin: None,
    })?;

    let tmux_bin = resolve_tmux_bin(args.tmux_bin.as_deref());
    let agent_bin = resolve_agent_bin(args.agent, args.agent_bin.as_deref());
    let launch_identity = match start_run_tmux(
        &tmux_bin,
        &agent_bin,
        args.agent,
        &context.state_dir,
        &created.record,
        &args.agent_args,
    ) {
        Ok(identity) => identity,
        Err(err) => {
            if tmux_launch_may_have_created_runtime(&err)
                && let Err(termination_err) = recover_failed_tmux_launch(
                    context,
                    &mut created.record,
                    &tmux_bin,
                    None,
                    SessionTerminationOperation::FailedLaunch,
                )
            {
                return Err(termination_err);
            }
            cleanup_created_record(context, &created);
            return Err(err);
        }
    };
    if let Err(err) = persist_launched_tmux_identity(context, &mut created.record, &launch_identity)
    {
        recover_failed_tmux_launch(
            context,
            &mut created.record,
            &tmux_bin,
            Some(&launch_identity),
            SessionTerminationOperation::FailedLaunch,
        )?;
        cleanup_created_record(context, &created);
        return Err(err);
    }
    if let Err(err) = establish_coordination_broker(context, &created.record) {
        recover_failed_tmux_launch(
            context,
            &mut created.record,
            &tmux_bin,
            Some(&launch_identity),
            SessionTerminationOperation::FailedLaunch,
        )?;
        cleanup_created_record(context, &created);
        return Err(err);
    }
    if let Err(err) = release_held_runtime(context, &created.record) {
        recover_failed_tmux_launch(
            context,
            &mut created.record,
            &tmux_bin,
            Some(&launch_identity),
            SessionTerminationOperation::FailedLaunch,
        )?;
        cleanup_created_record(context, &created);
        return Err(err);
    }

    let result = session_view(
        context,
        &created.record,
        Some("running".to_string()),
        Some(&tmux_bin),
    );
    record_workdir_usage(context, &cwd);
    Ok(StartView {
        format: args.format,
        result,
    })
}

pub(crate) fn start_provider_resume_session(
    context: &CliContext,
    args: ProviderResumeImportArgs,
) -> Result<StartView, CliError> {
    validate_provider_resume_import_agent_args(args.agent, &args.agent_args)?;
    validate_agent_args(args.agent, &args.agent_args)?;
    let provider_resume_id = normalize_provider_resume_id(&args.provider_resume_id)?;
    let source = resolve_provider_resume_source(
        args.agent,
        &provider_resume_id,
        args.provider_config_dir.as_deref(),
        args.agent_profile.as_deref(),
    )?;
    let cwd = resolve_cwd(Some(&source.cwd))?;
    let cwd_string = display_path(&cwd);
    let resume_args = canonical_provider_resume_args(args.agent, &cwd_string, &provider_resume_id)
        .ok_or_else(|| {
            CliError::usage(
                "unsupported-provider-resume-agent",
                format!(
                    "{} sessions cannot be imported by provider resume id",
                    args.agent.as_str()
                ),
                Some(json!({ "agent": args.agent.as_str() })),
            )
        })?;
    let provider_resume = ProviderResume {
        provider: args.agent.as_str().to_string(),
        session_id: provider_resume_id,
        captured_at: Zoned::now().timestamp().to_string(),
        capture_method: source.capture_method,
        resume_args,
        extra: BTreeMap::new(),
    };
    let tmux_bin = resolve_tmux_bin(args.tmux_bin.as_deref());
    let agent_bin = resolve_agent_bin(args.agent, args.agent_bin.as_deref());
    let mut created = create_record(RecordRequest {
        context,
        agent: args.agent,
        mode: "interactive",
        coordination_mode: args.coordination_mode,
        title: args.title.as_deref(),
        title_state: args.title_state,
        explicit_id: args.id.as_deref(),
        cwd: &cwd,
        prompt: None,
        log_file_name: None,
        provider_resume: Some(provider_resume.clone()),
        agent_args: args.agent_args,
        agent_bin: Some(display_path(&agent_bin)),
    })?;

    persist_initial_profile_context(
        context,
        &mut created,
        args.agent_profile.as_deref(),
        args.provider_config_dir.as_deref(),
        args.profile_auto_resume_supported,
        args.codex_usage_account.as_deref(),
    )?;

    advance_owned_startup_stage(context, &mut created.record, "tmux")?;

    let launch_identity = match start_resume_tmux(
        &tmux_bin,
        &agent_bin,
        &context.state_dir,
        &created.record,
        &provider_resume.resume_args,
    ) {
        Ok(identity) => identity,
        Err(err) => {
            if tmux_launch_may_have_created_runtime(&err)
                && let Err(termination_err) = recover_failed_tmux_launch(
                    context,
                    &mut created.record,
                    &tmux_bin,
                    None,
                    SessionTerminationOperation::FailedLaunch,
                )
            {
                return Err(termination_err);
            }
            cleanup_created_record(context, &created);
            return Err(err);
        }
    };
    if let Err(err) = persist_launched_tmux_identity(context, &mut created.record, &launch_identity)
    {
        recover_failed_tmux_launch(
            context,
            &mut created.record,
            &tmux_bin,
            Some(&launch_identity),
            SessionTerminationOperation::FailedLaunch,
        )?;
        cleanup_created_record(context, &created);
        return Err(err);
    }
    if let Err(err) = establish_coordination_broker(context, &created.record) {
        recover_failed_tmux_launch(
            context,
            &mut created.record,
            &tmux_bin,
            Some(&launch_identity),
            SessionTerminationOperation::FailedLaunch,
        )?;
        cleanup_created_record(context, &created);
        return Err(err);
    }
    if let Err(err) = release_held_runtime(context, &created.record) {
        recover_failed_tmux_launch(
            context,
            &mut created.record,
            &tmux_bin,
            Some(&launch_identity),
            SessionTerminationOperation::FailedLaunch,
        )?;
        cleanup_created_record(context, &created);
        return Err(err);
    }
    advance_owned_startup_stage(context, &mut created.record, "runtime")?;

    let status = session_status(&tmux_bin, &created.record);
    reconcile_owned_startup_projection(context, &mut created.record, &status);
    let result = session_view(context, &created.record, Some(status), Some(&tmux_bin));
    record_workdir_usage(context, &cwd);
    Ok(StartView {
        format: args.format,
        result,
    })
}

fn persist_initial_profile_context(
    context: &CliContext,
    created: &mut CreatedRecord,
    agent_profile: Option<&str>,
    provider_config_dir: Option<&Path>,
    profile_auto_resume_supported: Option<bool>,
    codex_usage_account: Option<&str>,
) -> Result<(), CliError> {
    if agent_profile.is_none()
        && provider_config_dir.is_none()
        && profile_auto_resume_supported.is_none()
        && codex_usage_account.is_none()
    {
        return Ok(());
    }
    let Some(runtime) = created.record.runtime.as_mut() else {
        cleanup_created_record(context, created);
        return Err(CliError::runtime(
            "agent-profile-runtime-missing",
            "launch profile metadata requires a session runtime",
            Some(json!({ "id": created.record.id })),
        ));
    };
    if let Some(agent_profile) = agent_profile {
        runtime
            .extra
            .insert(AGENT_PROFILE_RUNTIME_KEY.to_string(), json!(agent_profile));
    }
    if let Some(config_dir) = provider_config_dir {
        runtime.extra.insert(
            AGENT_PROFILE_PROVIDER_CONFIG_DIR_RUNTIME_KEY.to_string(),
            json!(display_path(config_dir)),
        );
    }
    if let Some(supported) = profile_auto_resume_supported {
        runtime.extra.insert(
            AGENT_PROFILE_AUTO_RESUME_SUPPORTED_RUNTIME_KEY.to_string(),
            json!(supported),
        );
    }
    if let Some(account) = codex_usage_account {
        runtime.extra.insert(
            AGENT_PROFILE_CODEX_USAGE_ACCOUNT_RUNTIME_KEY.to_string(),
            json!(account),
        );
    }
    if let Err(err) = write_session_record(context, &created.record) {
        cleanup_created_record(context, created);
        return Err(err);
    }
    Ok(())
}

struct CreatedRecord {
    record: SessionRecord,
    prompt_file: Option<PathBuf>,
    session_dir: PathBuf,
    _lifecycle_lock: Option<SessionRecordLock>,
}

impl CreatedRecord {
    fn release_lifecycle_lock(&mut self) {
        self._lifecycle_lock = None;
    }
}

struct RecordRequest<'a> {
    context: &'a CliContext,
    agent: AgentKind,
    mode: &'a str,
    coordination_mode: cli::CoordinationMode,
    title: Option<&'a str>,
    title_state: Option<SessionTitleState>,
    explicit_id: Option<&'a str>,
    cwd: &'a Path,
    prompt: Option<&'a str>,
    log_file_name: Option<&'a str>,
    provider_resume: Option<ProviderResume>,
    agent_args: Vec<String>,
    agent_bin: Option<String>,
}

fn create_record(request: RecordRequest<'_>) -> Result<CreatedRecord, CliError> {
    create_record_with_guard(request, None)
}

fn create_record_with_guard(
    request: RecordRequest<'_>,
    mut create_guard: Option<&mut dyn FnMut() -> Result<(), CliError>>,
) -> Result<CreatedRecord, CliError> {
    let now = Zoned::now();
    let timestamp = now.strftime("%Y%m%d-%H%M%S").to_string();
    let iso = now.timestamp().to_string();
    let title_supplied = request.title.is_some();
    let title = request.title.map(str::to_string);
    let (title, title_state) = match request.title_state {
        Some(title_state) => {
            canonicalize_structured_title_pair(title, title_supplied, title_state)?
        }
        None => (title, None),
    };
    let title_slug = title.as_deref().map(slugify);
    let id = resolve_session_id(
        request.context,
        request.explicit_id,
        request.agent,
        &timestamp,
        title_slug.as_deref(),
    )?;
    let lifecycle_lock = acquire_session_record_lock(request.context, &id)?;
    let tmux_session = format!("hs-{}-{id}", request.agent.as_str());
    let session_dir = session_dir(request.context, &id);
    if session_dir.exists() {
        return Err(CliError::runtime(
            "session-exists",
            format!("session already exists: {id}"),
            Some(json!({ "id": id })),
        ));
    }
    if let Some(guard) = create_guard.as_mut() {
        guard()?;
    }
    private_dir(&session_dir)?;

    let prompt_file = match request.prompt {
        Some(prompt) => {
            let path = session_dir.join("prompt.md");
            write_private_file(&path, prompt.as_bytes())?;
            Some(path)
        }
        None => None,
    };
    let log_file = request.log_file_name.map(|name| session_dir.join(name));
    let mut record = SessionRecord {
        schema_version: SESSION_DOCUMENT_VERSION.to_string(),
        id,
        agent: request.agent.as_str().to_string(),
        mode: request.mode.to_string(),
        coordination_mode: request.coordination_mode,
        title,
        title_state,
        title_revision: 0,
        cwd: display_path(request.cwd),
        tmux_session: tmux_session.clone(),
        prompt_file: prompt_file.as_ref().map(|path| display_path(path)),
        log_file: log_file.as_ref().map(|path| display_path(path)),
        created_at: iso.clone(),
        updated_at: iso.clone(),
        provider_resume: request.provider_resume,
        runtime: Some(RuntimeInfo {
            kind: "tmux".to_string(),
            tmux_session: tmux_session.clone(),
            generation: 1,
            started_at: now.timestamp().to_string(),
            launch_id: uuid::Uuid::new_v4().to_string(),
            extra: if request.agent == AgentKind::Codex {
                BTreeMap::from([(
                    codex_app_server::ATTENTION_AUTHORITY_KEY.to_string(),
                    json!("hook"),
                )])
            } else {
                BTreeMap::new()
            },
        }),
        agent_args: request.agent_args,
        agent_bin: request.agent_bin,
        extra: BTreeMap::new(),
        resume_sidecar_extra: BTreeMap::new(),
    };
    if request.mode == "interactive" {
        store_startup_projection(&mut record, &starting_projection(&iso, "record"));
    }

    write_session_record(request.context, &record)?;
    if let Err(err) = activity::activate_runtime(request.context, &record) {
        let _ = fs::remove_dir_all(&session_dir);
        return Err(err);
    }
    if let Err(err) = coordination::prepare(request.context, &record) {
        let _ = fs::remove_dir_all(&session_dir);
        return Err(err);
    }
    Ok(CreatedRecord {
        record,
        prompt_file,
        session_dir,
        _lifecycle_lock: Some(lifecycle_lock),
    })
}

fn cleanup_created_record(context: &CliContext, created: &CreatedRecord) {
    let _ = coordination::revoke(context, &created.record);
    let _ = codex_app_server::cleanup_runtime_files(context, &created.record);
    let _ = fs::remove_dir_all(&created.session_dir);
}

#[derive(Debug, Default)]
struct InitialProviderPlan {
    provider_resume: Option<ProviderResume>,
    launch_args: Vec<String>,
}

fn initial_provider_resume_plan(agent: AgentKind, _cwd: &Path) -> InitialProviderPlan {
    match agent {
        AgentKind::Claude => {
            let session_id = uuid::Uuid::new_v4().to_string();
            InitialProviderPlan {
                provider_resume: Some(ProviderResume {
                    provider: agent.as_str().to_string(),
                    session_id: session_id.clone(),
                    captured_at: Zoned::now().timestamp().to_string(),
                    capture_method: "claude-explicit-session-id".to_string(),
                    resume_args: vec!["--resume".to_string(), session_id.clone()],
                    extra: BTreeMap::new(),
                }),
                launch_args: vec!["--session-id".to_string(), session_id],
            }
        }
        AgentKind::Codex => InitialProviderPlan::default(),
        AgentKind::Hermes => InitialProviderPlan::default(),
    }
}

fn validate_agent_args(agent: AgentKind, args: &[String]) -> Result<(), CliError> {
    if agent != AgentKind::Claude {
        return Ok(());
    }
    for arg in args {
        if let Some(flag) = reserved_claude_resume_arg(arg) {
            return Err(CliError::usage(
                "reserved-agent-arg",
                format!(
                    "{flag} is managed by agent-session for durable Claude resume; do not pass it via --agent-arg"
                ),
                Some(json!({ "agent": agent.as_str(), "flag": flag })),
            ));
        }
    }
    Ok(())
}

fn validate_provider_resume_import_agent_args(
    agent: AgentKind,
    args: &[String],
) -> Result<(), CliError> {
    if args.is_empty() {
        return Ok(());
    }
    Err(CliError::usage(
        "provider-resume-agent-args-conflict",
        "provider_resume_id mode owns the resume command; omit agent_args",
        Some(json!({ "agent": agent.as_str() })),
    ))
}

fn reserved_claude_resume_arg(arg: &str) -> Option<&'static str> {
    [
        ("--session-id", false),
        ("--resume", false),
        ("-r", true),
        ("--continue", false),
        ("-c", false),
        ("--fork-session", false),
        ("--from-pr", false),
    ]
    .into_iter()
    .find_map(|(flag, allow_attached_short_value)| {
        reserved_agent_arg_matches(arg, flag, allow_attached_short_value).then_some(flag)
    })
}

fn reserved_codex_resume_arg(arg: &str) -> Option<&'static str> {
    [
        ("--cd", false),
        ("-C", true),
        ("--last", false),
        ("--all", false),
        ("--include-non-interactive", false),
    ]
    .into_iter()
    .find_map(|(flag, allow_attached_short_value)| {
        reserved_agent_arg_matches(arg, flag, allow_attached_short_value).then_some(flag)
    })
}

fn reserved_agent_arg_matches(
    arg: &str,
    flag: &'static str,
    allow_attached_short_value: bool,
) -> bool {
    if arg == flag {
        return true;
    }
    arg.strip_prefix(flag).is_some_and(|rest| {
        rest.starts_with('=')
            || (allow_attached_short_value
                && flag.starts_with('-')
                && !flag.starts_with("--")
                && !rest.is_empty())
    })
}

struct ProviderResumeSource {
    cwd: PathBuf,
    capture_method: String,
}

fn normalize_provider_resume_id(session_id: &str) -> Result<String, CliError> {
    normalize_resume_id(session_id).map_err(|err| {
        let message = match err {
            ResumeIdError::Empty => "provider resume id must not be empty",
            ResumeIdError::ControlChar => "provider resume id must not contain control characters",
        };
        CliError::usage("invalid-provider-resume-id", message, None)
    })
}

fn resolve_provider_resume_source(
    agent: AgentKind,
    session_id: &str,
    provider_config_dir: Option<&Path>,
    agent_profile: Option<&str>,
) -> Result<ProviderResumeSource, CliError> {
    let provider = match agent {
        AgentKind::Codex => ResumeProvider::Codex,
        AgentKind::Claude => ResumeProvider::Claude,
        AgentKind::Hermes => {
            return Err(CliError::usage(
                "unsupported-provider-resume-agent",
                "hermes sessions cannot be imported by provider resume id",
                Some(json!({ "agent": agent.as_str() })),
            ));
        }
    };
    // The shared resolver owns the bounded history scan and returns a structured
    // outcome; the user-facing error text and exit-code mapping stay here.
    let resolved = match provider_config_dir {
        Some(config_dir) => resolve_resume_source_in_config_dir(provider, config_dir, session_id),
        None => resolve_resume_source(provider, session_id),
    };
    match resolved {
        Ok(resolved) => Ok(ProviderResumeSource {
            cwd: resolved.cwd,
            capture_method: resolved.capture_method.to_string(),
        }),
        Err(ResumeResolveError::NotFound) => {
            Err(provider_resume_not_found(agent, session_id, agent_profile))
        }
        Err(ResumeResolveError::Ambiguous { cwd_count }) => Err(provider_resume_ambiguous(
            agent,
            session_id,
            cwd_count,
            agent_profile,
        )),
        Err(ResumeResolveError::Truncated) => Err(provider_resume_scan_truncated(
            agent,
            session_id,
            agent_profile,
        )),
    }
}

fn provider_resume_details(
    agent: AgentKind,
    session_id: &str,
    agent_profile: Option<&str>,
) -> Value {
    let mut details = json!({
        "agent": agent.as_str(),
        "provider_resume_id": session_id,
    });
    if let Some(agent_profile) = agent_profile {
        details["agent_profile"] = json!(agent_profile);
    }
    details
}

fn provider_resume_not_found(
    agent: AgentKind,
    session_id: &str,
    agent_profile: Option<&str>,
) -> CliError {
    CliError::data(
        "provider-resume-not-found",
        format!(
            "no {} provider history contains resume id: {session_id}",
            agent.as_str()
        ),
        Some(provider_resume_details(agent, session_id, agent_profile)),
    )
}

fn provider_resume_ambiguous(
    agent: AgentKind,
    session_id: &str,
    cwd_count: usize,
    agent_profile: Option<&str>,
) -> CliError {
    let mut details = provider_resume_details(agent, session_id, agent_profile);
    details["cwd_count"] = json!(cwd_count);
    CliError::data(
        "provider-resume-ambiguous",
        format!(
            "{} provider history has multiple cwd matches for resume id: {session_id}",
            agent.as_str()
        ),
        Some(details),
    )
}

fn provider_resume_scan_truncated(
    agent: AgentKind,
    session_id: &str,
    agent_profile: Option<&str>,
) -> CliError {
    CliError::runtime(
        "provider-resume-scan-truncated",
        format!(
            "{} provider history scan was truncated before resume id could be resolved: {session_id}",
            agent.as_str()
        ),
        Some(provider_resume_details(agent, session_id, agent_profile)),
    )
}

pub(crate) fn resolve_provider_transcript_path_from_roots(
    agent: AgentKind,
    session_id: &str,
    codex_root: Option<&Path>,
    claude_root: Option<&Path>,
) -> Option<PathBuf> {
    let mut matches = BTreeSet::new();
    let truncated = match agent {
        AgentKind::Codex => {
            let mut budget = CodexResumeScanBudget::from_env();
            collect_codex_provider_resume_matches(
                codex_root?,
                0,
                session_id,
                &mut matches,
                &mut budget,
            );
            budget.truncated
        }
        AgentKind::Claude => {
            let mut budget = ClaudeResumeScanBudget::from_env();
            collect_claude_provider_resume_matches(
                claude_root?,
                session_id,
                &mut matches,
                &mut budget,
            );
            budget.truncated
        }
        AgentKind::Hermes => return None,
    };
    if truncated || matches.len() != 1 {
        return None;
    }
    matches.into_iter().next().map(|candidate| candidate.path)
}

/// Resolve the `systemd-run` binary used to launch the tmux server inside a
/// transient systemd `--user` scope, or `None` to launch tmux directly.
///
/// `agent-session serve` starts each session as a child `tmux new-session -d`,
/// so the tmux server it spawns lands in the caller's cgroup. Under the
/// agent-console serve systemd service that means the server shares the unit
/// cgroup, and a service stop/restart can kill every live session
/// (`sympoies/agent-console#122`). Wrapping the server start in
/// `systemd-run --user --scope` moves it into its own transient scope cgroup, a
/// sibling of the service, so the sessions survive even an explicit
/// cgroup-wide kill of the serve unit.
///
/// This is opt-in via `AGENT_SESSION_TMUX_SCOPE` (the serve launcher sets it) and
/// only engages when a systemd `--user` manager is actually reachable, so an
/// opt-in on an unsupported host (no user manager, missing `systemd-run`,
/// non-Linux) degrades to a direct launch instead of failing session creation.
fn tmux_scope_runner() -> Option<PathBuf> {
    if !env_truthy("AGENT_SESSION_TMUX_SCOPE") {
        return None;
    }
    if !cfg!(target_os = "linux") {
        return None;
    }
    // A running systemd --user manager exposes this socket; without it
    // `systemd-run --user` cannot register the scope.
    let runtime_dir = env::var_os("XDG_RUNTIME_DIR")?;
    if !Path::new(&runtime_dir)
        .join("systemd")
        .join("private")
        .exists()
    {
        return None;
    }
    binary_on_path("systemd-run")
}

/// Build the base command for a `tmux new-session` that may start the tmux
/// server. With `scope_runner` set the server is launched inside a transient
/// systemd user scope (see [`tmux_scope_runner`]); otherwise tmux runs directly.
/// Callers append the `new-session ...` arguments to the returned command; both
/// forms accept the same trailing arguments because `systemd-run`'s `--`
/// hands everything after the tmux binary straight to tmux.
fn new_session_command(tmux_bin: &Path, scope_runner: Option<&Path>) -> ProcessCommand {
    match scope_runner {
        Some(runner) => {
            let mut command = ProcessCommand::new(runner);
            command
                .arg("--user")
                .arg("--scope")
                .arg("--quiet")
                .arg("--collect")
                .arg("--")
                .arg(tmux_bin);
            command
        }
        None => ProcessCommand::new(tmux_bin),
    }
}

pub(crate) fn resolve_agent_session_executable() -> io::Result<PathBuf> {
    let current_executable = env::current_exe()?;
    // Unit tests call launch helpers from the hashed `deps` harness; bind those
    // calls to Cargo's same-profile binary without changing production lookup.
    #[cfg(test)]
    if current_executable
        .parent()
        .and_then(Path::file_name)
        .is_some_and(|name| name == std::ffi::OsStr::new("deps"))
    {
        let profile_dir = current_executable
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    "test executable has no Cargo profile directory",
                )
            })?;
        let test_binary = profile_dir.join(format!("agent-session{}", env::consts::EXE_SUFFIX));
        return resolve_agent_session_executable_from(&test_binary);
    }
    resolve_agent_session_executable_from(&current_executable)
}

fn resolve_agent_session_executable_from(current_executable: &Path) -> io::Result<PathBuf> {
    let binary_name = format!("agent-session{}", env::consts::EXE_SUFFIX);
    let executable = if current_executable
        .file_name()
        .is_some_and(|name| name == std::ffi::OsStr::new(&binary_name))
    {
        current_executable.to_path_buf()
    } else {
        current_executable
            .parent()
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    "current executable has no release directory",
                )
            })?
            .join(binary_name)
    };
    let metadata = fs::symlink_metadata(&executable)?;
    if !metadata.file_type().is_file() || metadata.permissions().mode() & 0o111 == 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "agent-session executable is unavailable",
        ));
    }
    Ok(executable)
}

fn current_runtime_helper() -> Result<PathBuf, CliError> {
    #[cfg(test)]
    let executable = env::var_os("AGENT_SESSION_TEST_RUNTIME_HELPER")
        .map(PathBuf::from)
        .map(Ok)
        .unwrap_or_else(env::current_exe);
    #[cfg(not(test))]
    let executable = env::current_exe();
    let executable = executable.map_err(|_| {
        CliError::runtime(
            "codex-app-server-proxy-binary-unavailable",
            "the agent-session runtime helper is unavailable",
            None,
        )
    })?;
    let executable_ready = fs::metadata(&executable)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0);
    if !executable_ready {
        return Err(CliError::runtime(
            "codex-app-server-proxy-binary-unavailable",
            "the agent-session runtime helper is unavailable",
            None,
        ));
    }
    Ok(executable)
}

fn start_interactive_tmux(
    tmux_bin: &Path,
    agent_bin: &Path,
    agent: AgentKind,
    state_dir: &Path,
    record: &SessionRecord,
    provider_launch_args: &[String],
    agent_args: &[String],
) -> Result<TmuxRuntimeIdentity, CliError> {
    let mut command = new_session_command(tmux_bin, tmux_scope_runner().as_deref());
    command
        .arg("new-session")
        .arg("-d")
        .arg("-P")
        .arg("-F")
        .arg("#{session_id}\t#{pane_id}\t#{pane_pid}")
        .arg("-s")
        .arg(&record.tmux_session)
        .arg("-c")
        .arg(&record.cwd);
    add_runtime_tmux_environment(&mut command, state_dir, record)?;
    begin_held_runtime(&mut command, state_dir, record)?;

    if agent == AgentKind::Codex && codex_app_server::runtime_is_supported(record) {
        let socket = codex_app_server::socket_path(record).ok_or_else(|| {
            CliError::data(
                "codex-app-server-socket-missing",
                "Codex app-server runtime is missing its private socket",
                Some(json!({ "id": record.id })),
            )
        })?;
        let proxy = codex_app_server::proxy_path(record).ok_or_else(|| {
            CliError::data(
                "codex-app-server-proxy-missing",
                "Codex app-server runtime is missing its private TUI proxy",
                Some(json!({ "id": record.id })),
            )
        })?;
        let handoff = codex_app_server::thread_handoff_path(record).ok_or_else(|| {
            CliError::data(
                "codex-app-server-handoff-missing",
                "Codex app-server runtime is missing its thread handoff path",
                Some(json!({ "id": record.id })),
            )
        })?;
        let attached = codex_app_server::thread_attached_path(record).ok_or_else(|| {
            CliError::data(
                "codex-app-server-attached-marker-missing",
                "Codex app-server runtime is missing its attached marker path",
                Some(json!({ "id": record.id })),
            )
        })?;
        let proxy_bin = current_runtime_helper()?;
        command
            .arg("sh")
            .arg("-c")
            .arg(codex_app_server::launch_script())
            .arg("agent-session-codex-app-server")
            .arg(socket)
            .arg(proxy)
            .arg(handoff)
            .arg(attached)
            .arg(proxy_bin)
            .arg(state_dir)
            .arg(&record.id)
            .arg(agent_bin)
            .arg(&record.cwd);
        if provider_launch_args.is_empty() {
            command.arg("--cd").arg(&record.cwd).arg("--no-alt-screen");
        } else {
            command.args(provider_launch_args);
        }
        command.args(agent_args);
        return run_tmux_new_session(command, record);
    }

    command.arg(agent_bin);

    match agent {
        AgentKind::Codex => {
            command.arg("--cd").arg(&record.cwd).arg("--no-alt-screen");
        }
        AgentKind::Claude => {
            command.args(provider_launch_args);
            if let Some(title) = record
                .title
                .as_deref()
                .filter(|value| !value.trim().is_empty())
            {
                command.arg("--name").arg(title);
            }
        }
        AgentKind::Hermes => {
            command.arg("chat");
        }
    }
    command.args(agent_args);
    run_tmux_new_session(command, record)
}

fn start_run_tmux(
    tmux_bin: &Path,
    agent_bin: &Path,
    agent: AgentKind,
    state_dir: &Path,
    record: &SessionRecord,
    agent_args: &[String],
) -> Result<TmuxRuntimeIdentity, CliError> {
    let prompt_file = record.prompt_file.as_ref().ok_or_else(|| {
        CliError::runtime(
            "missing-prompt-file",
            "session prompt file is missing",
            None,
        )
    })?;
    let log_file = record.log_file.as_ref().ok_or_else(|| {
        CliError::runtime("missing-log-file", "session log file is missing", None)
    })?;
    let mut parts = Vec::new();
    parts.push(shell_words::quote(&display_path(agent_bin)).into_owned());
    match agent {
        AgentKind::Codex => {
            parts.push("exec".to_string());
            parts.push("--cd".to_string());
            parts.push(shell_words::quote(&record.cwd).into_owned());
        }
        AgentKind::Claude => {
            parts.push("-p".to_string());
        }
        AgentKind::Hermes => {
            return Err(CliError::usage(
                "unsupported-run-agent",
                "hermes does not support one-shot run mode; use start --agent hermes",
                None,
            ));
        }
    }
    parts.extend(
        agent_args
            .iter()
            .map(|arg| shell_words::quote(arg).into_owned()),
    );
    parts.push(format!("\"$(cat {})\"", shell_words::quote(prompt_file)));
    let script = format!(
        "set -u\n{} > {} 2>&1\n",
        parts.join(" "),
        shell_words::quote(log_file)
    );

    let mut command = new_session_command(tmux_bin, tmux_scope_runner().as_deref());
    command
        .arg("new-session")
        .arg("-d")
        .arg("-P")
        .arg("-F")
        .arg("#{session_id}\t#{pane_id}\t#{pane_pid}")
        .arg("-s")
        .arg(&record.tmux_session)
        .arg("-c")
        .arg(&record.cwd);
    add_runtime_tmux_environment(&mut command, state_dir, record)?;
    begin_held_runtime(&mut command, state_dir, record)?;
    command.arg("sh").arg("-lc").arg(script);
    run_tmux_new_session(command, record)
}

fn run_tmux_new_session(
    command: ProcessCommand,
    record: &SessionRecord,
) -> Result<TmuxRuntimeIdentity, CliError> {
    let output = run_output_with_timeout(command, PANE_INPUT_COMMAND_TIMEOUT).map_err(|err| {
        let code = if err.kind() == io::ErrorKind::TimedOut {
            "command-timeout"
        } else {
            "command-wait-failed"
        };
        CliError::runtime(code, format!("failed to run tmux new-session: {err}"), None)
    })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(CliError::runtime(
            "command-failed",
            if stderr.is_empty() {
                format!("tmux new-session failed with status {}", output.status)
            } else {
                format!("tmux new-session failed: {stderr}")
            },
            None,
        ));
    }
    let output = String::from_utf8(output.stdout).map_err(|_| {
        CliError::runtime(
            "tmux-runtime-identity-invalid",
            "tmux new-session returned a non-UTF-8 runtime identity",
            Some(json!({ "id": record.id })),
        )
    })?;
    let mut fields = output.trim().split('\t');
    let session_id = fields.next().unwrap_or_default();
    let pane_id = fields.next().unwrap_or_default();
    let pane_pid = fields
        .next()
        .filter(|_| fields.next().is_none())
        .and_then(|value| value.parse::<libc::pid_t>().ok())
        .filter(|pid| *pid > 1);
    if !valid_tmux_session_id(session_id) || !valid_tmux_pane_id(pane_id) || pane_pid.is_none() {
        return Err(CliError::runtime(
            "tmux-runtime-identity-invalid",
            "tmux new-session did not return a valid session, pane, and process identity",
            Some(json!({ "id": record.id })),
        ));
    }
    let pane_pid = pane_pid.expect("checked pane pid");
    let observed_process_group = unsafe { libc::getpgid(pane_pid) };
    let current_process_group = unsafe { libc::getpgrp() };
    let process_group_id = if observed_process_group > 1 {
        if observed_process_group == current_process_group {
            return Err(CliError::runtime(
                "tmux-runtime-identity-invalid",
                "tmux new-session returned a pane in the caller process group",
                Some(json!({ "id": record.id })),
            ));
        }
        Some(observed_process_group)
    } else {
        match process_group_status(pane_pid) {
            ProcessGroupStatus::Running => Some(pane_pid),
            ProcessGroupStatus::Stopped => None,
            ProcessGroupStatus::Unknown => {
                return Err(CliError::runtime(
                    "tmux-runtime-identity-invalid",
                    "tmux new-session returned an unverifiable pane process group",
                    Some(json!({ "id": record.id })),
                ));
            }
        }
    };
    let launch_id = record
        .runtime
        .as_ref()
        .map(|runtime| runtime.launch_id.clone())
        .filter(|launch_id| !launch_id.is_empty());
    let process_session_id = match process_group_id {
        Some(_) => process_session_id(pane_pid).map_err(|_| {
            CliError::runtime(
                "tmux-runtime-identity-invalid",
                "tmux new-session returned an unverifiable pane process session",
                Some(json!({ "id": record.id })),
            )
        })?,
        None => None,
    };
    let control_group = linux_process_control_group(pane_pid).map_err(|_| {
        CliError::runtime(
            "tmux-runtime-identity-invalid",
            "tmux new-session returned an unverifiable pane control group",
            Some(json!({ "id": record.id })),
        )
    })?;
    Ok(TmuxRuntimeIdentity {
        launch_id,
        session_id: session_id.to_string(),
        pane_id: pane_id.to_string(),
        pane_pid,
        process_group_id,
        process_session_id,
        process_session_members: Vec::new(),
        control_group_members: Vec::new(),
        control_group,
    })
}

fn tmux_launch_may_have_created_runtime(err: &CliError) -> bool {
    matches!(
        err.code(),
        "command-timeout" | "command-wait-failed" | "tmux-runtime-identity-invalid"
    )
}

fn capture_provider_resume_after_launch(
    agent: AgentKind,
    record: &SessionRecord,
    launch_started_at: SystemTime,
) -> Option<ProviderResume> {
    match agent {
        AgentKind::Codex => capture_codex_resume(record, launch_started_at),
        AgentKind::Claude | AgentKind::Hermes => None,
    }
}

#[derive(Debug)]
struct CodexResumeCandidate {
    session_id: String,
    created_at: SystemTime,
}

fn capture_codex_resume(
    record: &SessionRecord,
    launch_started_at: SystemTime,
) -> Option<ProviderResume> {
    let root = codex_resume_history_root(record)?;
    let timeout = Duration::from_millis(env_u64(
        "AGENT_SESSION_CODEX_CAPTURE_TIMEOUT_MS",
        CODEX_RESUME_CAPTURE_TIMEOUT_MS,
    ));
    let poll = Duration::from_millis(
        env_u64(
            "AGENT_SESSION_CODEX_CAPTURE_POLL_MS",
            CODEX_RESUME_CAPTURE_POLL_MS,
        )
        .max(1),
    );
    let ambiguity_window = Duration::from_millis(env_u64(
        "AGENT_SESSION_CODEX_AMBIGUITY_WINDOW_MS",
        CODEX_RESUME_AMBIGUITY_WINDOW_MS,
    ));
    let started = Instant::now();
    let mut observed_singleton: Option<(String, Instant)> = None;

    loop {
        let mut candidates = Vec::new();
        let mut budget = CodexResumeScanBudget::from_env();
        collect_codex_resume_candidates(
            &root,
            0,
            launch_started_at,
            &record.cwd,
            &mut candidates,
            &mut budget,
        );
        if budget.truncated {
            return None;
        }

        let candidate_ids: BTreeSet<String> = candidates
            .into_iter()
            .map(|candidate| candidate.session_id)
            .collect();
        match candidate_ids.len() {
            1 => {
                let candidate_id = candidate_ids.iter().next().expect("singleton candidate");
                match &observed_singleton {
                    Some((observed, first_seen_at)) if observed == candidate_id => {
                        if codex_candidate_satisfied_ambiguity_window(
                            *first_seen_at,
                            ambiguity_window,
                        ) {
                            return Some(codex_provider_resume(record, candidate_id));
                        }
                    }
                    Some(_) => return None,
                    None => observed_singleton = Some((candidate_id.clone(), Instant::now())),
                }
            }
            0 => {
                if observed_singleton.is_some() {
                    return None;
                }
            }
            _ => return None,
        }
        if timeout.is_zero() || started.elapsed() >= timeout {
            return observed_singleton
                .as_ref()
                .filter(|(_, first_seen_at)| {
                    codex_candidate_satisfied_ambiguity_window(*first_seen_at, ambiguity_window)
                })
                .map(|(session_id, _)| codex_provider_resume(record, session_id));
        }
        let remaining = timeout.saturating_sub(started.elapsed());
        thread::sleep(poll.min(remaining));
    }
}

fn capture_codex_resume_from_history(record: &SessionRecord) -> Option<ProviderResume> {
    let root = codex_resume_history_root(record)?;
    let earliest = record
        .created_at
        .parse::<Timestamp>()
        .ok()
        .map(SystemTime::from)?;
    let latest = earliest.checked_add(Duration::from_secs(CODEX_RESUME_BACKFILL_MAX_AGE_SECS))?;
    let mut candidates = Vec::new();
    let mut budget = CodexResumeScanBudget::from_env();
    collect_codex_resume_candidates(
        &root,
        0,
        earliest,
        &record.cwd,
        &mut candidates,
        &mut budget,
    );
    if budget.truncated {
        return None;
    }

    let candidate_ids: BTreeSet<String> = candidates
        .into_iter()
        .filter(|candidate| candidate.created_at <= latest)
        .map(|candidate| candidate.session_id)
        .collect();
    if candidate_ids.len() == 1 {
        let candidate_id = candidate_ids.iter().next().expect("singleton candidate");
        return Some(codex_provider_resume(record, candidate_id));
    }
    None
}

fn codex_resume_history_root(record: &SessionRecord) -> Option<PathBuf> {
    if session_agent_profile(record).is_some() {
        return session_provider_config_dir(record).map(|root| root.join("sessions"));
    }
    codex_sessions_root()
}

fn codex_candidate_satisfied_ambiguity_window(
    first_seen_at: Instant,
    ambiguity_window: Duration,
) -> bool {
    ambiguity_window.is_zero() || first_seen_at.elapsed() >= ambiguity_window
}

fn codex_provider_resume(record: &SessionRecord, session_id: &str) -> ProviderResume {
    ProviderResume {
        provider: "codex".to_string(),
        session_id: session_id.to_string(),
        captured_at: Zoned::now().timestamp().to_string(),
        capture_method: "codex-session-meta".to_string(),
        resume_args: canonical_provider_resume_args(AgentKind::Codex, &record.cwd, session_id)
            .expect("codex resume args"),
        extra: BTreeMap::new(),
    }
}

fn collect_codex_resume_candidates(
    dir: &Path,
    depth: usize,
    earliest: SystemTime,
    cwd: &str,
    candidates: &mut Vec<CodexResumeCandidate>,
    budget: &mut CodexResumeScanBudget,
) {
    if depth > CODEX_RESUME_SCAN_MAX_DEPTH {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if !budget.visit_entry() {
            return;
        }
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            collect_codex_resume_candidates(&path, depth + 1, earliest, cwd, candidates, budget);
            if budget.truncated {
                return;
            }
            continue;
        }
        if path.extension().and_then(|value| value.to_str()) != Some("jsonl") {
            continue;
        }
        let modified_at = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        if modified_at < earliest {
            continue;
        }
        if let Some(meta) = read_codex_session_meta(&path) {
            if meta.cwd != cwd {
                continue;
            }
            if meta.created_at < earliest {
                continue;
            }
            candidates.push(CodexResumeCandidate {
                session_id: meta.session_id,
                created_at: meta.created_at,
            });
        }
    }
}

fn paste_prompt(
    tmux_bin: &Path,
    record: &SessionRecord,
    delivery: PromptDelivery,
) -> Result<(), CliError> {
    let prompt_file = record.prompt_file.as_ref().ok_or_else(|| {
        CliError::runtime(
            "missing-prompt-file",
            "session prompt file is missing",
            None,
        )
    })?;
    let buffer_name = format!("{}-prompt", record.id);
    let target = format!("{}:0.0", record.tmux_session);

    // The caller's fixed pre-paste delay is only a guess at provider startup.
    // Confirm the pane has actually been drawn before pasting: bytes delivered
    // while the launch shell still owns the tty are echoed into scrollback and
    // lost, and the single-Enter recovery cannot restore a prompt that never
    // reached the provider's input at all.
    await_pane_drawn_before_paste(tmux_bin, &target);

    // A drawn pane is still not proof that the provider reads stdin yet: Claude
    // Code parks its cursor on the input line before it accepts input. Ordinary
    // starts may therefore use a pane digest to prove that the first paste was
    // ignored and retry it while no submit key has been sent. Managed workers
    // deliberately skip this probe because their private assignment prompt is
    // exactly-once transport and later recovery may only send one guarded Enter.
    let before = (delivery == PromptDelivery::ResilientBeforeSubmit)
        .then(|| capture_pane_digest(tmux_bin, &target))
        .flatten();
    load_and_paste_buffer(tmux_bin, &buffer_name, &target, Path::new(prompt_file))
        .map_err(|failure| prompt_delivery_failure(delivery, failure))?;

    // The initial prompt is submitted; `send` deliberately leaves this to
    // an explicit `--key enter`. Tmux confirms that it wrote the paste bytes,
    // but provider TUIs can still be processing the bracketed paste when an
    // immediately adjacent Enter arrives. Keep the key a separate command and
    // give both Codex and Claude one bounded settle interval first.
    thread::sleep(POST_PASTE_KEY_SETTLE_DELAY);
    if delivery == PromptDelivery::ResilientBeforeSubmit
        && pane_ignored_paste(before, capture_pane_digest(tmux_bin, &target))
    {
        load_and_paste_buffer(tmux_bin, &buffer_name, &target, Path::new(prompt_file))
            .map_err(|failure| prompt_delivery_failure(delivery, failure))?;
        thread::sleep(POST_PASTE_KEY_SETTLE_DELAY);
    }
    let mut enter = ProcessCommand::new(tmux_bin);
    enter.arg("send-keys").arg("-t").arg(&target).arg("Enter");
    run_status(enter, "tmux send-keys").map_err(|error| {
        if delivery == PromptDelivery::ManagedWorkerExactlyOnce {
            managed_worker_prompt_delivery_outcome_unknown(error, "submit")
        } else {
            error
        }
    })
}

/// Parse `#{cursor_y}|#{cursor_x}` from a tmux `display-message` reply. `None`
/// means tmux could not answer in the expected shape — an older tmux, a stubbed
/// binary, or a pane that is already gone — and the caller must then proceed
/// instead of waiting on a signal that will never arrive.
fn parse_pane_cursor(display_output: &str) -> Option<(u32, u32)> {
    let trimmed = display_output.trim();
    let (row, column) = trimmed.split_once('|')?;
    Some((row.trim().parse().ok()?, column.trim().parse().ok()?))
}

/// A pane whose cursor has left the origin has been drawn on, which is the
/// observable moment the provider TUI owns the terminal. The managed launch
/// wrapper waits on its gate files without writing anything, so an untouched
/// pane still reads `(0, 0)`.
fn pane_drawn(cursor: (u32, u32)) -> bool {
    cursor != (0, 0)
}

/// Bounded wait for the pane to be drawn before the initial prompt is pasted.
/// Never fails a launch: an unanswerable query returns immediately and the
/// deadline gives up rather than blocking, leaving the existing post-paste
/// settle and the runtime-owned single-Enter recovery as the later guards.
fn await_pane_drawn_before_paste(tmux_bin: &Path, target: &str) {
    let started = Instant::now();
    while started.elapsed() < PANE_PASTE_READY_DEADLINE {
        let Ok(output) = ProcessCommand::new(tmux_bin)
            .arg("display-message")
            .arg("-p")
            .arg("-t")
            .arg(target)
            .arg("#{cursor_y}|#{cursor_x}")
            .output()
        else {
            return;
        };
        if !output.status.success() {
            return;
        }
        match parse_pane_cursor(&String::from_utf8_lossy(&output.stdout)) {
            None => return,
            Some(cursor) if pane_drawn(cursor) => return,
            Some(_) => thread::sleep(PANE_PASTE_READY_POLL_INTERVAL),
        }
    }
}

/// Digest of the pane's visible content, used only to tell whether an ordinary
/// start reacted to a paste. `None` when tmux cannot answer, which must not be
/// read as "unchanged".
fn capture_pane_digest(tmux_bin: &Path, target: &str) -> Option<u64> {
    let output = ProcessCommand::new(tmux_bin)
        .arg("capture-pane")
        .arg("-p")
        .arg("-t")
        .arg(target)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let mut hasher = DefaultHasher::new();
    output.stdout.hash(&mut hasher);
    Some(hasher.finish())
}

/// Byte-identical observable panes prove that the provider ignored an ordinary
/// pre-submit paste. Missing observations never authorize another delivery.
fn pane_ignored_paste(before: Option<u64>, after: Option<u64>) -> bool {
    match (before, after) {
        (Some(before), Some(after)) => before == after,
        _ => false,
    }
}

/// Load `file` into a named tmux buffer and paste it into `target`, deleting the
/// buffer after paste (`-d`) or on failure. The staged error distinguishes a
/// proven pre-delivery load failure from a paste result that may have landed.
enum PromptPasteFailure {
    BeforeDelivery(CliError),
    OutcomeUnknown(CliError),
}

fn prompt_delivery_failure(delivery: PromptDelivery, failure: PromptPasteFailure) -> CliError {
    match failure {
        PromptPasteFailure::BeforeDelivery(error) => error,
        PromptPasteFailure::OutcomeUnknown(error)
            if delivery == PromptDelivery::ManagedWorkerExactlyOnce =>
        {
            managed_worker_prompt_delivery_outcome_unknown(error, "paste")
        }
        PromptPasteFailure::OutcomeUnknown(error) => error,
    }
}

fn managed_worker_prompt_delivery_outcome_unknown(error: CliError, phase: &str) -> CliError {
    CliError::runtime(
        "managed-worker-prompt-delivery-outcome-unknown",
        "managed worker prompt delivery may have reached the provider; preserve the exact session and retry only the durable worker-start request",
        Some(json!({
            "phase": phase,
            "transport_error": error.code()
        })),
    )
}

fn load_and_paste_buffer(
    tmux_bin: &Path,
    buffer_name: &str,
    target: &str,
    file: &Path,
) -> Result<(), PromptPasteFailure> {
    let mut load = ProcessCommand::new(tmux_bin);
    load.arg("load-buffer").arg("-b").arg(buffer_name).arg(file);
    run_status(load, "tmux load-buffer").map_err(PromptPasteFailure::BeforeDelivery)?;

    let mut paste = ProcessCommand::new(tmux_bin);
    paste
        .arg("paste-buffer")
        .arg("-b")
        .arg(buffer_name)
        .arg("-d")
        .arg("-t")
        .arg(target);
    if let Err(err) = run_status(paste, "tmux paste-buffer") {
        delete_tmux_buffer(tmux_bin, buffer_name);
        return Err(PromptPasteFailure::OutcomeUnknown(err));
    }
    Ok(())
}

fn delete_tmux_buffer(tmux_bin: &Path, buffer_name: &str) {
    let mut command = ProcessCommand::new(tmux_bin);
    command.arg("delete-buffer").arg("-b").arg(buffer_name);
    let _ = run_status_with_timeout(command, "tmux delete-buffer", PANE_INPUT_COMMAND_TIMEOUT);
}

fn send_to_session(context: &CliContext, args: cli::SendArgs) -> Result<SendResult, CliError> {
    let text = read_send_text(&args.text, args.text_stdin)?;
    if text.is_none() && args.keys.is_empty() {
        return Err(CliError::usage(
            "empty-send",
            "send requires --text, --text-stdin, or at least one --key",
            None,
        ));
    }
    let observed = load_session_record(context, &args.id)?;
    let tmux_bin = resolve_tmux_bin(args.tmux_bin.as_deref());
    let record_lock = acquire_session_record_lock(context, &observed.id)?;
    let mut manual_input = ManualInputSection::new(record_lock);
    let mut record = load_session_record(context, &observed.id)?;
    ensure_same_session_identity(&observed, &record)?;
    if live_status(&tmux_bin, &record.tmux_session) != "running" {
        return Err(CliError::runtime(
            "session-not-running",
            format!("session is not running: {}", record.id),
            Some(json!({ "id": record.id })),
        ));
    }
    let submits = codex_app_server::input_contains_submission(text.as_deref(), &args.keys);
    if submits {
        codex_app_server::ensure_manual_input_capability(context, &record)?;
        codex_account::authorize_input_locked(context, &mut record)?;
    }
    auto_resume::cancel_for_manual_input_locked(
        context,
        &record.id,
        &Timestamp::now().to_string(),
    )?;
    send_input_unlocked(
        context,
        &record,
        text.as_deref(),
        &args.keys,
        &tmux_bin,
        Some(&mut manual_input),
    )?;
    record.updated_at = Zoned::now().timestamp().to_string();
    write_session_record(context, &record)?;
    Ok(SendResult {
        id: record.id.clone(),
        tmux_session: record.tmux_session.clone(),
        sent_text: text.is_some(),
        keys: args
            .keys
            .iter()
            .map(|key| key.as_str().to_string())
            .collect(),
    })
}

/// Push literal text (via a private buffer file, never argv/stdout) and then
/// each special key into the live pane. `send-keys` interprets the tmux key
/// names, so approvals like Enter/Esc/Ctrl-C/arrows work from mobile.
fn send_input_serialized(
    context: &CliContext,
    expected: &SessionRecord,
    text: Option<&str>,
    keys: &[SpecialKey],
    tmux_bin: &Path,
) -> Result<(), CliError> {
    send_input_serialized_with_title_guard(context, expected, text, keys, tmux_bin, false)
}

/// Deliver the one Main Agent submit-recovery Enter while every mutable
/// authority source is fenced. The record, activity, and coordination locks
/// remain held through the tmux write, so an incarnation replacement, startup
/// dialog/turn transition, claim, or operation cannot cross the final check.
pub(crate) fn send_submit_recovery_input_serialized<G, F>(
    context: &CliContext,
    expected: &SessionRecord,
    expected_incarnation: &str,
    controller_session_id: &str,
    controller_incarnation: &str,
    tmux_bin: &Path,
    authorize: F,
) -> Result<(), CliError>
where
    F: FnOnce() -> Result<G, CliError>,
{
    let record_lock = acquire_session_record_lock(context, &expected.id)?;
    let mut manual_input = ManualInputSection::new(record_lock);
    let mut current = load_session_record(context, &expected.id)?;
    ensure_same_session_identity(expected, &current)?;
    let current_incarnation = current
        .runtime
        .as_ref()
        .map(|runtime| runtime.launch_id.as_str())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            CliError::data(
                "worker-incarnation-unavailable",
                "worker session incarnation is unavailable at submit recovery",
                None,
            )
        })?;
    if current_incarnation != expected_incarnation {
        return Err(CliError::data(
            "worker-incarnation-changed",
            "worker session incarnation changed at submit recovery",
            None,
        ));
    }
    let _activity_lock = activity::acquire_coordination_activity_lock(context, &current.id)?;
    if live_status(tmux_bin, &current.tmux_session) != "running" {
        return Err(CliError::runtime(
            "session-not-running",
            format!("session is not running: {}", current.id),
            Some(json!({ "id": current.id })),
        ));
    }
    codex_app_server::ensure_manual_input_capability(context, &current)?;
    codex_account::ensure_input_allowed(&current)?;
    // The global coordination lock is acquired only for the final authority
    // check and a one-second Enter write. Potentially slower capability,
    // account, and liveness checks above remain protected by the per-session
    // record/activity fences without blocking unrelated coordination traffic.
    let quiescence =
        coordination::lock_session_quiescence(context, &current.id, expected_incarnation)?;
    if !quiescence.broker_present {
        return Err(CliError::runtime(
            "coordination-broker-unavailable",
            "worker coordination broker evidence is unavailable at submit recovery",
            None,
        ));
    }
    if !quiescence.broker_identity_matched {
        return Err(CliError::data(
            "coordination-broker-incarnation-conflict",
            "worker coordination broker belongs to a different incarnation",
            None,
        ));
    }
    if !quiescence.broker_authoritative {
        return Err(CliError::runtime(
            "coordination-broker-unavailable",
            "worker coordination broker is not ready, fresh, and capability-backed at submit recovery",
            None,
        ));
    }
    if quiescence.active_claim || quiescence.active_operation || quiescence.uncertain_operation {
        return Err(CliError::data(
            "worker-not-quiescent",
            "submit recovery refuses a worker claim or active/uncertain operation",
            None,
        ));
    }
    let activity = activity::activity_status_for_record(context, &current)?;
    let turn = activity.turn_state;
    if turn.phase != activity::TurnPhase::Starting
        || turn.source.confidence != activity::Confidence::Authoritative
        || turn.current_turn.is_some()
        || turn.last_turn.is_some()
    {
        return Err(CliError::data(
            "worker-activity-not-authoritative-starting",
            "submit recovery requires authoritative startup evidence with no current or last turn",
            Some(json!({
                "phase": turn.phase,
                "confidence": turn.source.confidence,
                "current_turn": turn.current_turn.is_some(),
                "last_turn": turn.last_turn.is_some()
            })),
        ));
    }
    if !quiescence.has_active_claim(controller_session_id, controller_incarnation) {
        return Err(CliError::data(
            "claim-not-active",
            "reserving Main Agent claim is no longer active at submit recovery",
            None,
        ));
    }
    // Coordination remains locked while this guard revalidates and retains the
    // controller/run/assignment binding. This preserves the established
    // coordination -> orchestration lock order through the Enter side effect.
    let _authorization_guard = authorize()?;
    // These durable input side effects are intentionally last. Every liveness,
    // broker, quiescence, activity, claim, and orchestration rejection above is
    // proven pre-send and must leave both the Codex account fence and
    // auto-resume state untouched.
    auto_resume::cancel_for_manual_input_locked(
        context,
        &current.id,
        &Timestamp::now().to_string(),
    )?;
    codex_account::authorize_input_locked(context, &mut current)?;
    manual_input.arm(context, &current)?;
    send_tmux_key_with_timeout(
        tmux_bin,
        &format!("{}:0.0", current.tmux_session),
        SpecialKey::Enter,
        SUBMIT_RECOVERY_INPUT_COMMAND_TIMEOUT,
    )
}

fn send_title_rename_serialized(
    context: &CliContext,
    expected: &SessionRecord,
    text: Option<&str>,
    keys: &[SpecialKey],
    tmux_bin: &Path,
) -> Result<(), CliError> {
    send_input_serialized_with_title_guard(context, expected, text, keys, tmux_bin, true)
}

fn send_input_serialized_with_title_guard(
    context: &CliContext,
    expected: &SessionRecord,
    text: Option<&str>,
    keys: &[SpecialKey],
    tmux_bin: &Path,
    require_current_title: bool,
) -> Result<(), CliError> {
    let record_lock = acquire_session_record_lock(context, &expected.id)?;
    let mut manual_input = ManualInputSection::new(record_lock);
    let mut current = load_session_record(context, &expected.id)?;
    ensure_same_session_identity(expected, &current)?;
    // Title persistence completes before this best-effort Claude prompt-bar
    // projection. A newer title may win while this caller is waiting to regain
    // the session lock, so suppress the stale side effect instead of sending an
    // older `/rename` after the newer revision.
    if require_current_title
        && (current.title_revision != expected.title_revision || current.title != expected.title)
    {
        return Ok(());
    }
    if live_status(tmux_bin, &current.tmux_session) != "running" {
        return Err(CliError::runtime(
            "session-not-running",
            format!("session is not running: {}", current.id),
            Some(json!({ "id": current.id })),
        ));
    }
    if codex_app_server::input_contains_submission(text, keys) {
        codex_app_server::ensure_manual_input_capability(context, &current)?;
        codex_account::authorize_input_locked(context, &mut current)?;
    }
    auto_resume::cancel_for_manual_input_locked(
        context,
        &current.id,
        &Timestamp::now().to_string(),
    )?;
    send_input_unlocked(
        context,
        &current,
        text,
        keys,
        tmux_bin,
        Some(&mut manual_input),
    )
}

/// Submit the product-owned continuation while `auto_resume::tick` holds the
/// session record lock. Reloading here prevents a stale scheduler record from
/// targeting a restarted or recreated runtime.
pub(crate) fn send_auto_resume_input(
    context: &CliContext,
    expected: &SessionRecord,
    text: &str,
    tmux_bin: &Path,
) -> Result<(), CliError> {
    let mut current = load_session_record(context, &expected.id)?;
    ensure_same_session_identity(expected, &current)?;
    if live_status(tmux_bin, &current.tmux_session) != "running" {
        return Err(CliError::runtime(
            "session-not-running",
            format!("session is not running: {}", current.id),
            Some(json!({ "id": current.id })),
        ));
    }
    codex_account::authorize_input_locked(context, &mut current)?;
    send_input_unlocked(
        context,
        &current,
        Some(text),
        &[SpecialKey::Enter],
        tmux_bin,
        None,
    )
}

struct ManualInputSection {
    record_lock: Option<SessionRecordLock>,
    marker: Option<codex_app_server::ManualInputMarker>,
}

impl ManualInputSection {
    fn new(record_lock: SessionRecordLock) -> Self {
        Self {
            record_lock: Some(record_lock),
            marker: None,
        }
    }

    fn arm(&mut self, context: &CliContext, record: &SessionRecord) -> Result<(), CliError> {
        if self.marker.is_none() {
            self.marker = codex_app_server::begin_manual_input_section(context, record)?;
        }
        Ok(())
    }
}

impl Drop for ManualInputSection {
    fn drop(&mut self) {
        let record_lock = self.record_lock.take();
        if let Some(marker) = self.marker.take() {
            // The proxy holds the marker lock while forwarding a Busy-bypassed
            // turn. Marker teardown therefore cannot race a new lifecycle lock
            // holder or remove authority before the forwarded request lands.
            marker.finish(|| drop(record_lock));
        } else {
            drop(record_lock);
        }
    }
}

fn send_input_unlocked(
    context: &CliContext,
    record: &SessionRecord,
    text: Option<&str>,
    keys: &[SpecialKey],
    tmux_bin: &Path,
    mut manual_input: Option<&mut ManualInputSection>,
) -> Result<(), CliError> {
    let target = format!("{}:0.0", record.tmux_session);
    let mut pasted_literal_text = false;
    if let Some(text) = text {
        if matches!(text, "\r" | "\n" | "\r\n") {
            if let Some(section) = manual_input.as_deref_mut() {
                section.arm(context, record)?;
            }
            send_tmux_key(tmux_bin, &target, SpecialKey::Enter)?;
        } else {
            let nonce = uuid::Uuid::new_v4();
            let buffer_name = format!("{}-send-{nonce}", record.id);
            let temp = session_dir(context, &record.id).join(format!("send-input-{nonce}"));
            write_private_file(&temp, text.as_bytes())?;
            let result = load_and_paste_buffer_with_timeout(
                tmux_bin,
                &buffer_name,
                &target,
                &temp,
                PANE_INPUT_COMMAND_TIMEOUT,
            );
            let _ = fs::remove_file(&temp);
            result?;
            pasted_literal_text = true;
        }
    }
    if let Some(delay) = post_paste_settle_delay(pasted_literal_text, !keys.is_empty()) {
        thread::sleep(delay);
    }
    for key in keys {
        if *key == SpecialKey::Enter
            && let Some(section) = manual_input.as_deref_mut()
        {
            section.arm(context, record)?;
        }
        send_tmux_key(tmux_bin, &target, *key)?;
    }
    Ok(())
}

fn post_paste_settle_delay(text_pasted: bool, has_keys: bool) -> Option<Duration> {
    (text_pasted && has_keys).then_some(POST_PASTE_KEY_SETTLE_DELAY)
}

fn send_tmux_key(tmux_bin: &Path, target: &str, key: SpecialKey) -> Result<(), CliError> {
    send_tmux_key_with_timeout(tmux_bin, target, key, PANE_INPUT_COMMAND_TIMEOUT)
}

fn send_tmux_key_with_timeout(
    tmux_bin: &Path,
    target: &str,
    key: SpecialKey,
    timeout: Duration,
) -> Result<(), CliError> {
    let mut command = ProcessCommand::new(tmux_bin);
    command
        .arg("send-keys")
        .arg("-t")
        .arg(target)
        .arg(key.tmux_key());
    run_status_with_timeout(command, "tmux send-keys", timeout)
}

fn load_and_paste_buffer_with_timeout(
    tmux_bin: &Path,
    buffer_name: &str,
    target: &str,
    file: &Path,
    timeout: Duration,
) -> Result<(), CliError> {
    let mut load = ProcessCommand::new(tmux_bin);
    load.arg("load-buffer").arg("-b").arg(buffer_name).arg(file);
    run_status_with_timeout(load, "tmux load-buffer", timeout)?;

    let mut paste = ProcessCommand::new(tmux_bin);
    paste
        .arg("paste-buffer")
        .arg("-b")
        .arg(buffer_name)
        .arg("-d")
        .arg("-t")
        .arg(target);
    if let Err(err) = run_status_with_timeout(paste, "tmux paste-buffer", timeout) {
        delete_tmux_buffer(tmux_bin, buffer_name);
        return Err(err);
    }
    Ok(())
}

fn read_send_text(text: &Option<String>, text_stdin: bool) -> Result<Option<String>, CliError> {
    // Empty text (an empty `--text ""` or an empty stdin pipe) collapses to
    // `None` so the caller's empty-send guard treats it as "no text" rather than
    // pasting an empty buffer and reporting `sent_text: true` for a no-op. A
    // whitespace-only value is preserved: a space can be a meaningful keystroke.
    match (text, text_stdin) {
        (Some(_), true) => Err(CliError::usage(
            "multiple-text-sources",
            "use only one of --text or --text-stdin",
            None,
        )),
        (Some(value), false) => Ok(Some(value.clone()).filter(|value| !value.is_empty())),
        (None, true) => {
            let mut input = String::new();
            io::stdin().read_to_string(&mut input).map_err(|err| {
                CliError::runtime(
                    "stdin-read-failed",
                    format!("failed to read stdin: {err}"),
                    None,
                )
            })?;
            Ok(Some(input).filter(|value| !value.is_empty()))
        }
        (None, false) => Ok(None),
    }
}

fn glance_session(context: &CliContext, args: cli::GlanceArgs) -> Result<GlanceResult, CliError> {
    let mut record = load_session_record(context, &args.id)?;
    let tmux_bin = resolve_tmux_bin(args.tmux_bin.as_deref());
    let status = session_status(&tmux_bin, &record);
    let (reconciled, status) = reconcile_startup_projection(context, record, status, None);
    record = reconciled;
    if status != "running" {
        record = backfill_provider_resume(context, record);
    }
    let tail = if status == "running" {
        capture_pane_tail(&record, args.tail, &tmux_bin)?
    } else {
        String::new()
    };
    let last_terminal_activity_at = last_terminal_activity_at(&tmux_bin, &record, &status);
    Ok(GlanceResult {
        id: record.id.clone(),
        agent: record.agent.clone(),
        title: record.title.clone(),
        title_state: effective_session_title_state(&record),
        title_state_supported: true,
        title_revision: record.title_revision,
        tmux_session: record.tmux_session.clone(),
        status,
        resumable: is_resumable(&record),
        repo_name: repo_name_from_cwd(&record.cwd),
        provider_resume: record
            .provider_resume
            .as_ref()
            .map(ProviderResumeView::from),
        tail,
        created_at: record.created_at.clone(),
        updated_at: record.updated_at.clone(),
        last_terminal_activity_at,
        runtime_started_at: record
            .runtime
            .as_ref()
            .map(|runtime| runtime.started_at.clone()),
        turn_state: activity::state_for_view(context, &record),
        startup: startup_projection_for_view(&record),
        auto_resume: auto_resume::view_for_record(context, &record),
        coordination: coordination::public_summary(context, &record.id),
    })
}

/// Run `tmux capture-pane -p -S -<tail>` for a session. Returns `Ok(Some(text))`
/// on success, `Ok(None)` when tmux ran but capture failed (a non-running or
/// gone pane), and `Err` only when the tmux binary could not be spawned. Shared
/// by `glance` and `logs` so the capture invocation lives in one place.
fn run_capture_pane(
    record: &SessionRecord,
    tail: usize,
    tmux_bin: &Path,
) -> Result<Option<String>, CliError> {
    let start = format!("-{}", tail.max(1));
    let output = ProcessCommand::new(tmux_bin)
        .arg("capture-pane")
        .arg("-p")
        .arg("-t")
        .arg(&record.tmux_session)
        .arg("-S")
        .arg(start)
        .output()
        .map_err(|err| {
            CliError::runtime(
                "tmux-capture-failed",
                format!("failed to run {}: {err}", tmux_bin.display()),
                Some(json!({ "tmux_session": record.tmux_session })),
            )
        })?;
    if !output.status.success() {
        return Ok(None);
    }
    Ok(Some(String::from_utf8_lossy(&output.stdout).to_string()))
}

/// Read the tmux paste buffer for a session's server (`tmux show-buffer`). tmux
/// buffers are server-global, so this returns the most recently set buffer on the
/// socket — which, for an agent pane whose TUI copies mouse selections into the
/// buffer (e.g. Claude Code's "copied N chars to tmux buffer"), is the user's last
/// on-screen selection. The `id` only validates the session (and picks the
/// daemon's socket); the buffer itself is not session-scoped. A fresh server with
/// no buffer yet exits non-zero ("no buffers") — treated as an empty selection,
/// not an error, so "nothing selected yet" is a normal empty result.
fn session_clipboard_buffer(
    context: &CliContext,
    id: &str,
    tmux_bin: &Path,
) -> Result<String, CliError> {
    // Validate the session exists first (clean not-found) before touching tmux.
    let _record = load_session_record(context, id)?;
    let output = ProcessCommand::new(tmux_bin)
        .arg("show-buffer")
        .output()
        .map_err(|err| {
            CliError::runtime(
                "tmux-show-buffer-failed",
                format!("failed to run {}: {err}", tmux_bin.display()),
                None,
            )
        })?;
    if !output.status.success() {
        return Ok(String::new());
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn capture_pane_tail(
    record: &SessionRecord,
    tail: usize,
    tmux_bin: &Path,
) -> Result<String, CliError> {
    match run_capture_pane(record, tail, tmux_bin)? {
        Some(text) => Ok(tail_lines(&strip_trailing_blank_lines(&text), tail)),
        None => Err(CliError::runtime(
            "tmux-capture-failed",
            "tmux capture-pane failed",
            Some(json!({ "tmux_session": record.tmux_session })),
        )),
    }
}

/// `capture-pane` pads its output to the full pane height with blank lines, so a
/// short, top-anchored pane ends with many empties. Drop the trailing blank
/// lines before taking the tail, or `glance` would show the empty bottom of the
/// pane instead of the actual recent content.
fn strip_trailing_blank_lines(text: &str) -> String {
    let mut lines: Vec<&str> = text.lines().collect();
    while lines.last().is_some_and(|line| line.trim().is_empty()) {
        lines.pop();
    }
    lines.join("\n")
}

#[cfg(test)]
mod codex_resume_tests {
    use super::*;

    #[test]
    fn codex_resume_scan_truncates_large_stale_tree_by_entry_budget() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path().join("sessions");
        fs::create_dir_all(&root).unwrap();
        for index in 0..32 {
            fs::write(
                root.join(format!("stale-{index}.jsonl")),
                r#"{"timestamp":"2000-01-01T00:00:00Z","type":"session_meta","payload":{"id":"old","cwd":"/repo","source":"cli","timestamp":"2000-01-01T00:00:00Z"}}"#,
            )
            .unwrap();
        }

        let mut candidates = Vec::new();
        let mut budget = CodexResumeScanBudget {
            visited: 0,
            max_entries: 5,
            deadline: Instant::now() + Duration::from_secs(60),
            truncated: false,
        };
        collect_codex_resume_candidates(
            &root,
            0,
            SystemTime::now(),
            "/repo",
            &mut candidates,
            &mut budget,
        );

        assert_eq!(budget.visited, 5);
        assert!(budget.truncated);
        assert!(candidates.is_empty());
    }
}

#[cfg(test)]
fn update_session_title(
    context: &CliContext,
    id: &str,
    title: Option<String>,
    tmux_bin: &Path,
) -> Result<SessionView, CliError> {
    update_session_title_if_revision(
        context,
        id,
        title,
        true,
        None,
        TitleUpdatePreconditions::default(),
        tmux_bin,
    )
}

#[derive(Default)]
struct TitleUpdatePreconditions {
    title_revision: Option<u64>,
    session_created_at: Option<String>,
    session_incarnation: Option<String>,
    session_title: Option<Option<String>>,
}

fn update_session_title_if_revision(
    context: &CliContext,
    id: &str,
    title: Option<String>,
    title_supplied: bool,
    title_state: Option<SessionTitleState>,
    expected: TitleUpdatePreconditions,
    tmux_bin: &Path,
) -> Result<SessionView, CliError> {
    let (normalized_title, normalized_title_state) = match title_state {
        Some(title_state) => {
            canonicalize_structured_title_pair(title, title_supplied, title_state)?
        }
        None => (normalize_title(title)?, None),
    };
    let (record, previous_title) = mutate_session_record_for_title(
        context,
        id,
        expected.session_created_at.as_deref(),
        expected.session_incarnation.as_deref(),
        |record| {
            if let Some(expected) = expected.session_title.as_ref()
                && &record.title != expected
            {
                return Err(CliError::data(
                    "title-state-conflict",
                    "session title changed since it was read",
                    Some(json!({
                        "expected_session_title": expected,
                        "actual_session_title": record.title,
                        "actual_title_revision": record.title_revision,
                    })),
                ));
            }
            let actual_title_revision = record.title_revision;
            if let Some(expected) = expected.title_revision
                && actual_title_revision != expected
            {
                return Err(CliError::data(
                    "title-revision-conflict",
                    "session title changed since it was read",
                    Some(json!({
                        "expected_title_revision": expected,
                        "actual_title_revision": actual_title_revision,
                        "actual_title": record.title,
                    })),
                ));
            }
            let previous_title = record.title.clone();
            record.title_revision = actual_title_revision;
            record.title = normalized_title;
            record.title_state = normalized_title_state;
            record.title_revision = actual_title_revision.checked_add(1).ok_or_else(|| {
                CliError::data(
                    "title-revision-overflow",
                    "session title revision cannot advance",
                    Some(json!({ "actual_title_revision": record.title_revision })),
                )
            })?;
            record.updated_at = Zoned::now().timestamp().to_string();
            Ok((record.clone(), previous_title))
        },
    )?;
    let status = session_status(tmux_bin, &record);
    // The persisted record above is the source of truth. Claude also carries its
    // own prompt-bar display name, which we set once via `--name` at launch
    // (`start_interactive_tmux`) and which never changes afterwards, so a renamed
    // session would show a stale name in the terminal while the console shows the
    // new one. Claude exposes `/rename <name>` as a runtime rename, so push the
    // new title into the live pane to keep the two in sync. Best-effort: a tmux
    // hiccup must not fail the title update, and Codex/Hermes have no such display
    // name so this is Claude-only and only when the title actually changed.
    if status == "running"
        && AgentKind::from_name(&record.agent) == Some(AgentKind::Claude)
        && record.title != previous_title
        && let Some(new_title) = record.title.as_deref()
    {
        let _ = rename_live_claude_session(context, &record, new_title, tmux_bin);
    }
    Ok(session_view(context, &record, Some(status), Some(tmux_bin)))
}

/// Push Claude's `/rename <name>` slash command into the live pane so the
/// prompt-bar display name follows a title change. Reuses the same buffered
/// paste + Enter injection as steering `send`, and collapses any embedded
/// newlines so the rename stays a single submitted line.
fn rename_live_claude_session(
    context: &CliContext,
    record: &SessionRecord,
    title: &str,
    tmux_bin: &Path,
) -> Result<(), CliError> {
    let single_line = title.replace(['\n', '\r'], " ");
    let command = format!("/rename {single_line}");
    send_title_rename_serialized(
        context,
        record,
        Some(&command),
        &[SpecialKey::Enter],
        tmux_bin,
    )
}

fn resume_session(context: &CliContext, args: cli::ResumeArgs) -> Result<SessionView, CliError> {
    let tmux_bin = resolve_tmux_bin(args.tmux_bin.as_deref());
    resume_session_by_id(context, &args.id, &tmux_bin)
}

struct StartupArtifactBackup {
    entries: Vec<(PathBuf, PathBuf)>,
    finalized: bool,
}

impl StartupArtifactBackup {
    fn ensure_not_interrupted(
        context: &CliContext,
        record: &SessionRecord,
    ) -> Result<(), CliError> {
        let dir = session_dir(context, &record.id);
        for name in STARTUP_ARTIFACT_FILES {
            let staged = dir.join(format!("{name}.resume-backup"));
            match fs::symlink_metadata(&staged) {
                Ok(_) => {
                    return Err(CliError::runtime(
                        "startup-artifact-backup-interrupted",
                        "session has an interrupted startup artifact backup; resume is blocked to preserve prior diagnostics",
                        Some(json!({ "id": record.id })),
                    ));
                }
                Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                Err(err) => {
                    return Err(session_io_error(
                        "startup-artifact-backup-failed",
                        &staged,
                        err,
                    ));
                }
            }
        }
        Ok(())
    }

    fn stage(context: &CliContext, record: &SessionRecord) -> Result<Self, CliError> {
        Self::ensure_not_interrupted(context, record)?;
        let dir = session_dir(context, &record.id);
        let mut backup = Self {
            entries: Vec::new(),
            finalized: false,
        };
        for name in STARTUP_ARTIFACT_FILES {
            let current = dir.join(name);
            let staged = dir.join(format!("{name}.resume-backup"));
            match fs::rename(&current, &staged) {
                Ok(()) => backup.entries.push((current, staged)),
                Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                Err(err) => {
                    let _ = backup.restore();
                    return Err(session_io_error(
                        "startup-artifact-backup-failed",
                        &current,
                        err,
                    ));
                }
            }
        }
        Ok(backup)
    }

    fn restore(&mut self) -> Result<(), CliError> {
        let mut first_error = None;
        for (current, staged) in self.entries.iter().rev() {
            match fs::remove_file(current) {
                Ok(()) => {}
                Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                Err(err) => {
                    first_error.get_or_insert_with(|| {
                        session_io_error("startup-artifact-restore-failed", current, err)
                    });
                    continue;
                }
            }
            if let Err(err) = fs::rename(staged, current) {
                first_error.get_or_insert_with(|| {
                    session_io_error("startup-artifact-restore-failed", staged, err)
                });
            }
        }
        if first_error.is_none() {
            self.finalized = true;
        }
        match first_error {
            Some(err) => Err(err),
            None => Ok(()),
        }
    }

    fn discard(mut self) {
        for (_, staged) in &self.entries {
            let _ = fs::remove_file(staged);
        }
        self.finalized = true;
    }
}

impl Drop for StartupArtifactBackup {
    fn drop(&mut self) {
        if !self.finalized {
            let _ = self.restore();
        }
    }
}

fn resume_session_by_id(
    context: &CliContext,
    id: &str,
    tmux_bin: &Path,
) -> Result<SessionView, CliError> {
    // Preserve not-found semantics before creating the private lock file, then
    // serialize the entire resume transition (including launch and rollback)
    // against title, hook, timestamp, and backfill writers.
    let observed = load_session_record(context, id)?;
    let canonical_id = observed.id.clone();
    let _record_lock = acquire_session_record_lock(context, &canonical_id)?;
    let record = load_session_record(context, &canonical_id)?;
    ensure_same_session_identity(&observed, &record)?;
    resume_session_locked(context, record, tmux_bin).map(|outcome| outcome.session)
}

struct ResumeSessionOutcome {
    session: SessionView,
    session_incarnation: Option<String>,
    session_generation: Option<u64>,
}

fn resume_session_locked(
    context: &CliContext,
    mut record: SessionRecord,
    tmux_bin: &Path,
) -> Result<ResumeSessionOutcome, CliError> {
    orchestration::ensure_session_not_quarantined(context, &record)?;
    match session_status(tmux_bin, &record).as_str() {
        "running" => {
            return Ok(ResumeSessionOutcome {
                session: session_view(
                    context,
                    &record,
                    Some("running".to_string()),
                    Some(tmux_bin),
                ),
                session_incarnation: record
                    .runtime
                    .as_ref()
                    .map(|runtime| runtime.launch_id.clone())
                    .filter(|launch_id| !launch_id.is_empty()),
                session_generation: record.runtime.as_ref().map(|runtime| runtime.generation),
            });
        }
        "unknown" => {
            return Err(CliError::runtime(
                "session-status-unknown",
                format!("session status could not be checked: {}", record.id),
                Some(json!({ "id": record.id })),
            ));
        }
        _ => {}
    }
    let durable_profile_context = validate_durable_profile_resume_context(&record)?;
    StartupArtifactBackup::ensure_not_interrupted(context, &record)?;
    if record.provider_resume.is_none()
        && AgentKind::from_name(&record.agent) == Some(AgentKind::Codex)
        && let Some(provider_resume) = capture_codex_resume_from_history(&record)
    {
        record.provider_resume = Some(provider_resume);
        write_session_record(context, &record)?;
    }
    let (provider_resume, agent) = validate_resume_metadata(&record)?;
    let resume_args = provider_resume.resume_args.clone();
    let prior_identities = persisted_prior_tmux_runtime_identities(&record).map_err(|reason| {
        session_termination_error(&record, reason, SessionTerminationOperation::Resume)
    })?;
    let termination_state = persisted_tmux_termination_state(&record).map_err(|reason| {
        session_termination_error(&record, reason, SessionTerminationOperation::Resume)
    })?;
    match persisted_tmux_runtime_identity(&record).map_err(|reason| {
        session_termination_error(&record, reason, SessionTerminationOperation::Resume)
    })? {
        Some(identity) => {
            if prior_identities
                .iter()
                .any(|prior| !prior.same_runtime_target(&identity))
            {
                return Err(session_termination_error(
                    &record,
                    SessionTerminationFailure::RuntimeIdentityMismatch,
                    SessionTerminationOperation::Resume,
                ));
            }
            let verification_started = Instant::now();
            verify_stopped_process_runtimes(&prior_identities, DELETE_TERMINATION_VERIFY_TIMEOUT)
                .map_err(|reason| {
                session_termination_error(&record, reason, SessionTerminationOperation::Resume)
            })?;
            let remaining =
                DELETE_TERMINATION_VERIFY_TIMEOUT.saturating_sub(verification_started.elapsed());
            if remaining.is_zero() {
                return Err(session_termination_error(
                    &record,
                    SessionTerminationFailure::VerificationFailed,
                    SessionTerminationOperation::Resume,
                ));
            }
            verify_stopped_tmux_runtime(tmux_bin, &identity, remaining).map_err(|reason| {
                session_termination_error(&record, reason, SessionTerminationOperation::Resume)
            })?;
        }
        None if runtime_is_proven_never_launched(&record) && prior_identities.is_empty() => {}
        None => {
            return Err(session_termination_error(
                &record,
                SessionTerminationFailure::RuntimeIdentityUnavailable,
                SessionTerminationOperation::Resume,
            ));
        }
    }
    if matches!(
        termination_state,
        Some(TmuxTerminationState::Pending { .. })
    ) {
        return Err(session_termination_error(
            &record,
            SessionTerminationFailure::RuntimeIdentityUnavailable,
            SessionTerminationOperation::Resume,
        ));
    }
    record.extra.remove(DELETE_TMUX_PRIOR_IDENTITIES_KEY);
    record.extra.remove(DELETE_TMUX_TERMINATION_STATE_KEY);
    let previous_record = record.clone();
    let app_server_managed = agent == AgentKind::Codex
        && (codex_app_server::runtime_is_supported(&previous_record)
            || codex_account::binding_is_present(&previous_record));
    let previous_activity = activity::capture_snapshot(context, &record.id)?;
    let mut startup_artifacts = StartupArtifactBackup::stage(context, &record)?;
    let agent_bin = durable_profile_context
        .map(|profile| profile.agent_bin)
        .or_else(|| record.agent_bin.as_deref().map(PathBuf::from))
        .unwrap_or_else(|| resolve_agent_bin(agent, None));
    let now = Zoned::now();
    let next_generation = record
        .runtime
        .as_ref()
        .map(|runtime| runtime.generation.saturating_add(1))
        .unwrap_or(1);
    record.runtime = Some(RuntimeInfo {
        kind: "tmux".to_string(),
        tmux_session: record.tmux_session.clone(),
        generation: next_generation,
        started_at: now.timestamp().to_string(),
        launch_id: uuid::Uuid::new_v4().to_string(),
        extra: record
            .runtime
            .as_ref()
            .map(|runtime| runtime.extra.clone())
            .unwrap_or_default(),
    });
    record.extra.remove(TMUX_RUNTIME_NEVER_LAUNCHED_KEY);
    store_startup_projection(
        &mut record,
        &starting_projection(&now.timestamp().to_string(), "record"),
    );
    if app_server_managed {
        codex_account::mark_runtime_pending(&mut record)?;
        codex_account::recover_next_after_restart(&mut record)?;
        codex_app_server::configure_runtime(context, &agent_bin, &mut record, true)?;
    }
    record.updated_at = now.timestamp().to_string();
    write_session_record(context, &record)?;
    activity::activate_runtime(context, &record)?;
    if let Err(error) = coordination::prepare(context, &record) {
        let _ = write_session_record(context, &previous_record);
        let _ = activity::restore_snapshot(context, &record.id, &previous_activity);
        let _ = startup_artifacts.restore();
        return Err(error);
    }
    let launch = if app_server_managed {
        start_interactive_tmux(
            tmux_bin,
            &agent_bin,
            agent,
            &context.state_dir,
            &record,
            &resume_args,
            &record.agent_args,
        )
    } else {
        start_resume_tmux(
            tmux_bin,
            &agent_bin,
            &context.state_dir,
            &record,
            &resume_args,
        )
    };
    let launch_error = match launch {
        Ok(identity) => {
            if let Err(err) = persist_launched_tmux_identity(context, &mut record, &identity) {
                match recover_failed_tmux_launch(
                    context,
                    &mut record,
                    tmux_bin,
                    Some(&identity),
                    SessionTerminationOperation::Resume,
                ) {
                    Ok(()) => Some(err),
                    Err(termination_err) => {
                        startup_artifacts.discard();
                        return Err(termination_err);
                    }
                }
            } else if let Err(err) = establish_coordination_broker(context, &record) {
                match recover_failed_tmux_launch(
                    context,
                    &mut record,
                    tmux_bin,
                    Some(&identity),
                    SessionTerminationOperation::Resume,
                ) {
                    Ok(()) => Some(err),
                    Err(termination_err) => {
                        startup_artifacts.discard();
                        return Err(termination_err);
                    }
                }
            } else if let Err(err) = release_held_runtime(context, &record) {
                match recover_failed_tmux_launch(
                    context,
                    &mut record,
                    tmux_bin,
                    Some(&identity),
                    SessionTerminationOperation::Resume,
                ) {
                    Ok(()) => Some(err),
                    Err(termination_err) => {
                        startup_artifacts.discard();
                        return Err(termination_err);
                    }
                }
            } else {
                None
            }
        }
        Err(err) if tmux_launch_may_have_created_runtime(&err) => {
            match recover_failed_tmux_launch(
                context,
                &mut record,
                tmux_bin,
                None,
                SessionTerminationOperation::Resume,
            ) {
                Ok(()) => Some(err),
                Err(termination_err) => {
                    startup_artifacts.discard();
                    return Err(termination_err);
                }
            }
        }
        Err(err) => Some(err),
    };
    if let Some(launch_err) = launch_error {
        let coordination_revoke = coordination::revoke(context, &record);
        let record_restore = write_session_record(context, &previous_record);
        let activity_restore = activity::restore_snapshot(context, &record.id, &previous_activity);
        let artifact_restore = startup_artifacts.restore();
        if coordination_revoke.is_err()
            || record_restore.is_err()
            || activity_restore.is_err()
            || artifact_restore.is_err()
        {
            return Err(CliError::runtime(
                "resume-launch-rollback-failed",
                "provider resume launch failed and the prior durable runtime could not be fully restored",
                Some(json!({
                    "id": record.id,
                    "launch_error": launch_err.code(),
                    "new_coordination_revoked": coordination_revoke.is_ok(),
                    "record_restored": record_restore.is_ok(),
                    "activity_restored": activity_restore.is_ok(),
                    "startup_artifacts_restored": artifact_restore.is_ok()
                })),
            ));
        }
        return Err(launch_err);
    }
    if let (Ok(previous_incarnation), Ok(current_incarnation)) = (
        coordination::incarnation(&previous_record),
        coordination::incarnation(&record),
    ) && previous_incarnation != current_incarnation
    {
        let _ = fs::remove_file(coordination::checkpoint_path_for_state(
            &context.state_dir,
            &previous_record.id,
            &previous_incarnation,
        ));
    }
    reconcile_owned_startup_projection(context, &mut record, "running");
    startup_artifacts.discard();
    Ok(ResumeSessionOutcome {
        session: session_view(
            context,
            &record,
            Some("running".to_string()),
            Some(tmux_bin),
        ),
        session_incarnation: record
            .runtime
            .as_ref()
            .map(|runtime| runtime.launch_id.clone())
            .filter(|launch_id| !launch_id.is_empty()),
        session_generation: record.runtime.as_ref().map(|runtime| runtime.generation),
    })
}

fn start_resume_tmux(
    tmux_bin: &Path,
    agent_bin: &Path,
    state_dir: &Path,
    record: &SessionRecord,
    resume_args: &[String],
) -> Result<TmuxRuntimeIdentity, CliError> {
    let mut command = new_session_command(tmux_bin, tmux_scope_runner().as_deref());
    command
        .arg("new-session")
        .arg("-d")
        .arg("-P")
        .arg("-F")
        .arg("#{session_id}\t#{pane_id}\t#{pane_pid}")
        .arg("-s")
        .arg(&record.tmux_session)
        .arg("-c")
        .arg(&record.cwd);
    add_runtime_tmux_environment(&mut command, state_dir, record)?;
    begin_held_runtime(&mut command, state_dir, record)?;
    command
        .arg(agent_bin)
        .args(resume_args)
        .args(&record.agent_args);
    run_tmux_new_session(command, record)
}

fn launch_gate_path(state_dir: &Path, record: &SessionRecord) -> PathBuf {
    state_dir
        .join("sessions")
        .join(&record.id)
        .join("coordination")
        .join(COORDINATION_LAUNCH_GATE)
}

fn broker_gate_path(state_dir: &Path, record: &SessionRecord) -> PathBuf {
    state_dir
        .join("sessions")
        .join(&record.id)
        .join("coordination")
        .join(COORDINATION_BROKER_GATE)
}

fn begin_held_runtime(
    command: &mut ProcessCommand,
    state_dir: &Path,
    record: &SessionRecord,
) -> Result<(), CliError> {
    let gate = launch_gate_path(state_dir, record);
    let broker_gate = broker_gate_path(state_dir, record);
    let broker_bin = resolve_agent_session_executable().map_err(|_| {
        CliError::runtime(
            "coordination-unavailable",
            "failed to resolve the coordination broker executable",
            None,
        )
    })?;
    for path in [&gate, &broker_gate] {
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(_) => {
                return Err(CliError::runtime(
                    "coordination-unavailable",
                    "failed to reset a held launch gate",
                    None,
                ));
            }
        }
    }
    command
        .arg("--")
        .arg("sh")
        .arg("-c")
        .arg(HELD_LAUNCH_SCRIPT)
        .arg("agent-session-held-launch")
        .arg(gate)
        .arg(broker_gate)
        .arg(coordination::heartbeat_path(state_dir, &record.id))
        .arg(coordination::capability_path_for_state(
            state_dir,
            &record.id,
            record
                .runtime
                .as_ref()
                .map(|runtime| runtime.launch_id.as_str())
                .unwrap_or_default(),
        ))
        .arg(
            record
                .runtime
                .as_ref()
                .map(|runtime| runtime.launch_id.as_str())
                .unwrap_or_default(),
        )
        .arg(
            record
                .runtime
                .as_ref()
                .map(|runtime| runtime.generation)
                .unwrap_or_default()
                .to_string(),
        )
        .arg(broker_bin);
    Ok(())
}

fn release_held_runtime(context: &CliContext, record: &SessionRecord) -> Result<(), CliError> {
    let incarnation = coordination::incarnation(record)?;
    let capability = coordination::capability_path(context, &record.id, &incarnation);
    let metadata = fs::metadata(&capability).map_err(|_| {
        CliError::runtime(
            "coordination-broker-start-timeout",
            "coordination capability was not ready before agent launch",
            None,
        )
    })?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o077 != 0 {
        return Err(CliError::runtime(
            "coordination-broker-start-timeout",
            "coordination capability was not private before agent launch",
            None,
        ));
    }
    coordination::ensure_ready(context, record)?;
    write_private_file(&launch_gate_path(&context.state_dir, record), b"ready\n").map_err(|_| {
        CliError::runtime(
            "coordination-broker-start-timeout",
            "held agent runtime could not be released",
            None,
        )
    })
}

fn establish_coordination_broker(
    context: &CliContext,
    record: &SessionRecord,
) -> Result<(), CliError> {
    coordination::provision(context, record)?;
    write_private_file(&broker_gate_path(&context.state_dir, record), b"ready\n").map_err(
        |_| {
            CliError::runtime(
                "coordination-unavailable",
                "failed to release the coordination broker launch gate",
                None,
            )
        },
    )?;
    coordination::activate_ready(context, record)
}

fn add_runtime_tmux_environment(
    command: &mut ProcessCommand,
    state_dir: &Path,
    record: &SessionRecord,
) -> Result<(), CliError> {
    let runtime_id = record
        .runtime
        .as_ref()
        .map(|runtime| runtime.launch_id.as_str())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            CliError::data(
                "runtime-id-missing",
                "session runtime is missing its launch id",
                Some(json!({ "id": record.id })),
            )
        })?;
    for value in [
        format!("AGENT_SESSION_ID={}", record.id),
        format!("AGENT_SESSION_STATE_DIR={}", display_path(state_dir)),
        format!("AGENT_SESSION_RUNTIME_ID={runtime_id}"),
        format!(
            "AGENT_SESSION_COORDINATION_MODE={}",
            record.coordination_mode.as_str()
        ),
        format!(
            "{}={}",
            coordination::CAPABILITY_ENV,
            display_path(&coordination::capability_path_for_state(
                state_dir, &record.id, runtime_id
            ))
        ),
        format!(
            "{}={}",
            coordination::CHECKPOINT_ENV,
            display_path(&coordination::checkpoint_path_for_state(
                state_dir, &record.id, runtime_id
            ))
        ),
        format!(
            "{}={}",
            codex_app_server::ATTENTION_AUTHORITY_ENV,
            codex_app_server::attention_authority(record)
        ),
    ] {
        command.arg("-e").arg(value);
    }
    if let (Some(agent), Some(config_dir)) = (
        AgentKind::from_name(&record.agent),
        session_provider_config_dir(record),
    ) {
        let env_key = match agent {
            AgentKind::Codex => Some("CODEX_HOME"),
            AgentKind::Claude => Some("CLAUDE_CONFIG_DIR"),
            AgentKind::Hermes => None,
        };
        if let Some(env_key) = env_key {
            let mut assignment = OsString::from(format!("{env_key}="));
            assignment.push(config_dir);
            command.arg("-e").arg(assignment);
        }
    }
    if let Some(path) = env::var_os("PATH") {
        // A long-lived tmux server keeps the environment from when that server
        // started. Pin each new session to the current daemon PATH so provider
        // hooks cannot resolve an older agent-session helper after a staged
        // daemon upgrade.
        let mut assignment = OsString::from("PATH=");
        assignment.push(path);
        command.arg("-e").arg(assignment);
    }
    Ok(())
}

fn normalize_title(title: Option<String>) -> Result<Option<String>, CliError> {
    let Some(title) = title else {
        return Ok(None);
    };
    let title = title.trim().to_string();
    if title.is_empty() {
        return Ok(None);
    }
    if title.chars().count() > 120 {
        return Err(CliError::usage(
            "title-too-long",
            "session title must be 120 characters or fewer",
            Some(json!({ "max_chars": 120 })),
        ));
    }
    Ok(Some(title))
}

pub(crate) fn canonicalize_structured_title_pair(
    title: Option<String>,
    title_supplied: bool,
    title_state: SessionTitleState,
) -> Result<(Option<String>, Option<SessionTitleState>), CliError> {
    let normalized_title = normalize_structured_compatibility_title(title)?;
    let state = normalize_title_state(title_state)?;
    let rendered = render_session_title_state(&state)?;
    let supplied_title_matches = !title_supplied
        || normalized_title == rendered
        || normalized_title == render_v122_legacy_session_title_state(&state)?;
    if !supplied_title_matches {
        return Err(CliError::usage(
            "title-state-mismatch",
            "session title must match the canonical title_state rendering",
            Some(json!({ "field": "title_state" })),
        ));
    }
    Ok((rendered, Some(state)))
}

fn normalize_structured_compatibility_title(
    title: Option<String>,
) -> Result<Option<String>, CliError> {
    let Some(title) = title else {
        return Ok(None);
    };
    let title = title.trim_matches(is_javascript_whitespace).to_string();
    if title.is_empty() {
        return Ok(None);
    }
    if title.chars().count() > SESSION_TITLE_MAX_CHARS {
        return Err(CliError::usage(
            "title-too-long",
            "session title must be 120 characters or fewer",
            Some(json!({ "max_chars": SESSION_TITLE_MAX_CHARS })),
        ));
    }
    Ok(Some(title))
}

const SESSION_TITLE_MAX_CHARS: usize = 120;
const SESSION_TITLE_SEPARATOR: &str = " - ";
const SESSION_TITLE_MIN_ACTIVITY_CHARS: usize = 32;
const SESSION_TITLE_MAX_REFERENCES: usize = 2;

fn normalize_title_state(mut state: SessionTitleState) -> Result<SessionTitleState, CliError> {
    state.topic = normalize_title_state_component(state.topic, "topic")?;
    state.activity = normalize_title_state_component(state.activity, "activity")?;
    match (&state.topic_source, &state.topic) {
        (SessionTitleTopicSource::None, None)
        | (SessionTitleTopicSource::Auto, Some(_))
        | (SessionTitleTopicSource::User, Some(_)) => {}
        _ => {
            return Err(CliError::usage(
                "invalid-title-state",
                "topic_source must be none without a topic, or auto/user with a topic",
                Some(json!({ "field": "title_state.topic_source" })),
            ));
        }
    }
    if state.references.len() > SESSION_TITLE_MAX_REFERENCES {
        return Err(CliError::usage(
            "invalid-title-state",
            format!(
                "title_state supports at most {SESSION_TITLE_MAX_REFERENCES} work-item references"
            ),
            Some(json!({ "field": "title_state.references" })),
        ));
    }
    let mut normalized_references = Vec::with_capacity(state.references.len());
    for reference in state.references {
        let reference = reference.trim_matches(is_javascript_whitespace).to_string();
        let number = reference.strip_prefix('#').unwrap_or_default();
        if number.is_empty()
            || number.starts_with('0')
            || number.len() > 10
            || !number.chars().all(|character| character.is_ascii_digit())
        {
            return Err(CliError::usage(
                "invalid-title-state",
                "title_state references must use #<positive-number>",
                Some(json!({ "field": "title_state.references" })),
            ));
        }
        if !normalized_references.contains(&reference) {
            normalized_references.push(reference);
        }
    }
    state.references = normalized_references;
    Ok(state)
}

fn normalize_title_state_component(
    value: Option<String>,
    field: &str,
) -> Result<Option<String>, CliError> {
    let Some(value) = value else {
        return Ok(None);
    };
    normalize_title_state_component_chars(value.chars(), field)
}

fn normalize_title_state_component_chars(
    characters: impl IntoIterator<Item = char>,
    field: &str,
) -> Result<Option<String>, CliError> {
    let mut normalized = String::with_capacity(SESSION_TITLE_MAX_CHARS);
    let mut character_count = 0usize;
    let mut pending_space = false;
    for character in characters {
        if is_javascript_whitespace(character) {
            pending_space = !normalized.is_empty();
            continue;
        }
        if pending_space {
            if character_count == SESSION_TITLE_MAX_CHARS {
                return Err(title_state_component_too_long(field));
            }
            normalized.push(' ');
            character_count += 1;
            pending_space = false;
        }
        if character_count == SESSION_TITLE_MAX_CHARS {
            return Err(title_state_component_too_long(field));
        }
        normalized.push(character);
        character_count += 1;
    }
    Ok((!normalized.is_empty()).then_some(normalized))
}

fn title_state_component_too_long(field: &str) -> CliError {
    CliError::usage(
        "invalid-title-state",
        format!("title_state {field} must be {SESSION_TITLE_MAX_CHARS} characters or fewer"),
        Some(
            json!({ "field": format!("title_state.{field}"), "max_chars": SESSION_TITLE_MAX_CHARS }),
        ),
    )
}

fn is_javascript_whitespace(character: char) -> bool {
    matches!(
        character,
        '\u{0009}'..='\u{000d}'
            | '\u{0020}'
            | '\u{00a0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200a}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202f}'
            | '\u{205f}'
            | '\u{3000}'
            | '\u{feff}'
    )
}

fn truncate_title_component(value: &str, max_chars: usize) -> String {
    let chars = value.chars().collect::<Vec<_>>();
    if chars.len() <= max_chars {
        return value.to_string();
    }
    if max_chars <= 3 {
        return chars.into_iter().take(max_chars).collect();
    }
    format!(
        "{}...",
        chars
            .into_iter()
            .take(max_chars.saturating_sub(3))
            .collect::<String>()
    )
}

fn title_contains_reference(title: &str, reference: &str) -> bool {
    title.match_indices(reference).any(|(index, _)| {
        title[index + reference.len()..]
            .chars()
            .next()
            .is_none_or(|character| !character.is_ascii_alphanumeric() && character != '_')
    })
}

fn v122_legacy_title_contains_reference(title: &str, reference: &str) -> bool {
    title.match_indices(reference).any(|(index, _)| {
        title[index + reference.len()..]
            .chars()
            .next()
            .is_none_or(|character| !character.is_ascii_digit())
    })
}

fn render_session_title_state(state: &SessionTitleState) -> Result<Option<String>, CliError> {
    render_session_title_state_with_reference_matcher(state, title_contains_reference)
}

fn render_v122_legacy_session_title_state(
    state: &SessionTitleState,
) -> Result<Option<String>, CliError> {
    // This compatibility output is a released v1.22.0 persistence contract.
    // Its divergent reference-boundary cases are frozen by release-derived
    // fixtures. A future renderer change must migrate stored pairs or retain
    // those exact outputs before this transition path can be changed or removed.
    render_session_title_state_with_reference_matcher(state, v122_legacy_title_contains_reference)
}

fn render_session_title_state_with_reference_matcher(
    state: &SessionTitleState,
    contains_reference: fn(&str, &str) -> bool,
) -> Result<Option<String>, CliError> {
    let state = normalize_title_state(state.clone())?;
    let max_anchor_chars = if state.activity.is_some() {
        SESSION_TITLE_MAX_CHARS
            .saturating_sub(SESSION_TITLE_SEPARATOR.chars().count())
            .saturating_sub(SESSION_TITLE_MIN_ACTIVITY_CHARS)
    } else {
        SESSION_TITLE_MAX_CHARS
    };
    let topic = state.topic.as_deref().filter(|value| !value.is_empty());
    let mut visible_topic = topic.map(|value| truncate_title_component(value, max_anchor_chars));
    let mut references = String::new();
    if let Some(topic) = topic {
        for _ in 0..=state.references.len() {
            references = state
                .references
                .iter()
                .filter(|reference| {
                    visible_topic
                        .as_deref()
                        .is_none_or(|value| !contains_reference(value, reference))
                })
                .cloned()
                .collect::<Vec<_>>()
                .join(" ");
            let topic_chars = if references.is_empty() {
                max_anchor_chars
            } else {
                max_anchor_chars.saturating_sub(references.chars().count() + 1)
            };
            let next_visible_topic = truncate_title_component(topic, topic_chars);
            if visible_topic.as_deref() == Some(next_visible_topic.as_str()) {
                break;
            }
            visible_topic = Some(next_visible_topic);
        }
    } else {
        references = state.references.join(" ");
    }
    let anchor = match (
        visible_topic.as_deref().filter(|value| !value.is_empty()),
        references.is_empty(),
    ) {
        (None, true) => None,
        (None, false) => Some(truncate_title_component(&references, max_anchor_chars)),
        (Some(topic), true) => Some(topic.to_string()),
        (Some(topic), false) => Some(if topic.is_empty() {
            truncate_title_component(&references, max_anchor_chars)
        } else {
            format!("{topic} {references}")
        }),
    };
    match (anchor, state.activity.as_deref()) {
        (None, None) => Ok(None),
        (Some(anchor), None) => Ok(Some(truncate_title_component(
            &anchor,
            SESSION_TITLE_MAX_CHARS,
        ))),
        (None, Some(activity)) => Ok(Some(truncate_title_component(
            activity,
            SESSION_TITLE_MAX_CHARS,
        ))),
        (Some(anchor), Some(activity)) => {
            let activity_chars = SESSION_TITLE_MAX_CHARS
                .saturating_sub(anchor.chars().count())
                .saturating_sub(SESSION_TITLE_SEPARATOR.chars().count());
            Ok(Some(format!(
                "{anchor}{SESSION_TITLE_SEPARATOR}{}",
                truncate_title_component(activity, activity_chars)
            )))
        }
    }
}

fn effective_session_title_state(record: &SessionRecord) -> Option<SessionTitleStateView> {
    let state = record.title_state.as_ref()?;
    let rendered = render_session_title_state(state).ok()?;
    let legacy_rendered = render_v122_legacy_session_title_state(state).ok()?;
    if rendered == record.title || legacy_rendered == record.title {
        Some(SessionTitleStateView::from(state))
    } else {
        None
    }
}

fn write_session_attachment(
    context: &CliContext,
    id: &str,
    filename: Option<&str>,
    content_type: Option<String>,
    bytes: &[u8],
) -> Result<AttachmentResult, CliError> {
    let record = load_session_record(context, id)?;
    let filename = sanitize_attachment_filename(filename.unwrap_or("attachment.bin"));
    let dir = session_dir(context, &record.id).join("attachments");
    ensure_private_dir(&dir)?;
    let path = write_unique_attachment_file(&dir, &filename, bytes)?;
    Ok(AttachmentResult {
        id: record.id,
        filename,
        path: display_path(&path),
        bytes: bytes.len(),
        content_type,
    })
}

fn sanitize_attachment_filename(raw: &str) -> String {
    let leaf = Path::new(raw)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("attachment.bin");
    let mut safe = leaf
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    safe = safe
        .trim_matches(|ch| matches!(ch, '.' | '-' | '_'))
        .to_string();
    if safe.is_empty() {
        safe = "attachment.bin".to_string();
    }
    if safe.len() > 120 {
        safe.truncate(120);
    }
    safe
}

fn attachment_candidate_path(dir: &Path, filename: &str, attempt: usize) -> PathBuf {
    let stamp = Zoned::now().strftime("%Y%m%d-%H%M%S").to_string();
    if attempt == 0 {
        dir.join(format!("{stamp}-{filename}"))
    } else {
        dir.join(format!("{stamp}-{attempt}-{filename}"))
    }
}

fn write_unique_attachment_file(
    dir: &Path,
    filename: &str,
    bytes: &[u8],
) -> Result<PathBuf, CliError> {
    for attempt in 0..1000 {
        let path = attachment_candidate_path(dir, filename, attempt);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(SECRET_FILE_MODE);
        }
        let mut file = match options.open(&path) {
            Ok(file) => file,
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => {
                return Err(CliError::runtime(
                    "file-write-failed",
                    format!("failed to write {}: {err}", path.display()),
                    Some(json!({ "path": display_path(&path) })),
                ));
            }
        };
        if let Err(err) = file.write_all(bytes).and_then(|_| file.sync_all()) {
            let _ = fs::remove_file(&path);
            return Err(CliError::runtime(
                "file-write-failed",
                format!("failed to write {}: {err}", path.display()),
                Some(json!({ "path": display_path(&path) })),
            ));
        }
        return Ok(path);
    }
    Err(CliError::runtime(
        "attachment-name-exhausted",
        "failed to allocate a unique attachment filename",
        Some(json!({ "filename": filename })),
    ))
}

const WORKDIR_SEARCH_MAX_DEPTH: usize = 4;
const WORKDIR_SEARCH_MAX_VISITED: usize = 5000;
const WORKDIR_SEARCH_TIMEOUT: Duration = Duration::from_millis(250);

fn search_workdirs(
    context: &CliContext,
    query: Option<&str>,
    limit: Option<usize>,
    options: WorkdirSearchOptions,
) -> Result<Vec<WorkdirResult>, CliError> {
    let Some(home) = home_dir() else {
        return Ok(Vec::new());
    };
    let roots = [home.join("Project"), home.join(".config")];
    let usage = load_workdir_usage(context);
    search_workdirs_in_roots(
        &roots,
        query.unwrap_or_default(),
        limit.unwrap_or(30).clamp(1, 100),
        options,
        &usage,
    )
}

fn search_workdirs_in_roots(
    roots: &[PathBuf],
    query: &str,
    limit: usize,
    options: WorkdirSearchOptions,
    usage: &BTreeMap<String, String>,
) -> Result<Vec<WorkdirResult>, CliError> {
    #[derive(Debug)]
    struct Candidate {
        result: WorkdirResult,
        depth: usize,
    }

    let query = query.trim().to_ascii_lowercase();
    let started = Instant::now();
    let mut visited = 0usize;
    let mut matches = Vec::new();

    for root in roots {
        if started.elapsed() >= WORKDIR_SEARCH_TIMEOUT || visited >= WORKDIR_SEARCH_MAX_VISITED {
            break;
        }
        let Ok(root_meta) = fs::symlink_metadata(root) else {
            continue;
        };
        if root_meta.file_type().is_symlink() || !root_meta.is_dir() {
            continue;
        }
        let Ok(canonical_root) = fs::canonicalize(root) else {
            continue;
        };
        let root_display = display_path(root);
        let mut queue = VecDeque::from([(root.clone(), 0usize)]);
        while let Some((path, depth)) = queue.pop_front() {
            if started.elapsed() >= WORKDIR_SEARCH_TIMEOUT || visited >= WORKDIR_SEARCH_MAX_VISITED
            {
                break;
            }
            let Ok(canonical_path) = fs::canonicalize(&path) else {
                continue;
            };
            if !canonical_path.starts_with(&canonical_root) {
                continue;
            }
            visited += 1;
            let name = path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or_default()
                .to_string();
            let is_git_repo = is_git_repo(&path);
            let is_linked_worktree = is_linked_worktree(&path);
            let include = depth > 0
                && workdir_matches(&path, &name, &query)
                && (!options.git_only || is_git_repo)
                && (!options.exclude_worktrees || !is_linked_worktree);
            if include {
                let path_display = display_path(&path);
                matches.push(Candidate {
                    depth,
                    result: WorkdirResult {
                        last_used: usage.get(&path_display).cloned(),
                        path: path_display,
                        name,
                        root: root_display.clone(),
                        is_git_repo,
                    },
                });
            }
            if depth >= WORKDIR_SEARCH_MAX_DEPTH {
                continue;
            }
            if options.git_only && is_git_repo {
                continue;
            }
            let Ok(entries) = fs::read_dir(&path) else {
                continue;
            };
            for entry in entries.flatten() {
                let Ok(file_type) = entry.file_type() else {
                    continue;
                };
                if options.git_only
                    && entry
                        .file_name()
                        .to_str()
                        .is_some_and(|name| name == ".git")
                {
                    continue;
                }
                if file_type.is_dir() {
                    queue.push_back((entry.path(), depth + 1));
                }
            }
        }
    }

    matches.sort_by(|a, b| {
        let base = if options.git_only || options.exclude_worktrees {
            b.result
                .last_used
                .cmp(&a.result.last_used)
                .then_with(|| a.result.name.cmp(&b.result.name))
        } else {
            b.result.is_git_repo.cmp(&a.result.is_git_repo)
        };
        base.then_with(|| a.depth.cmp(&b.depth))
            .then_with(|| a.result.path.cmp(&b.result.path))
    });
    matches.truncate(limit);
    Ok(matches
        .into_iter()
        .map(|candidate| candidate.result)
        .collect())
}

fn workdir_matches(path: &Path, name: &str, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    name.to_ascii_lowercase().contains(query)
        || path.to_string_lossy().to_ascii_lowercase().contains(query)
}

fn is_git_repo(path: &Path) -> bool {
    path.join(".git").exists()
}

fn is_linked_worktree(path: &Path) -> bool {
    fs::symlink_metadata(path.join(".git"))
        .map(|meta| meta.is_file())
        .unwrap_or(false)
}

fn workdir_usage_path(context: &CliContext) -> PathBuf {
    context.state_dir.join(WORKDIR_USAGE_FILE)
}

fn load_workdir_usage(context: &CliContext) -> BTreeMap<String, String> {
    let path = workdir_usage_path(context);
    let Ok(contents) = fs::read_to_string(path) else {
        return BTreeMap::new();
    };
    serde_json::from_str::<WorkdirUsage>(&contents)
        .map(|usage| usage.entries)
        .unwrap_or_default()
}

fn record_workdir_usage(context: &CliContext, cwd: &Path) {
    let path = workdir_usage_path(context);
    let mut usage = WorkdirUsage {
        entries: load_workdir_usage(context),
    };
    usage
        .entries
        .insert(display_path(cwd), Zoned::now().timestamp().to_string());
    if let Ok(bytes) = serde_json::to_vec_pretty(&usage) {
        let _ = write_atomic(&path, &bytes, SECRET_FILE_MODE);
    }
}

fn list_sessions(
    context: &CliContext,
    tmux_bin: Option<&Path>,
) -> Result<Vec<SessionView>, CliError> {
    list_sessions_with_shadow_sampling(context, tmux_bin, false)
}

fn list_sessions_for_serve(
    context: &CliContext,
    tmux_bin: Option<&Path>,
) -> Result<Vec<SessionView>, CliError> {
    list_sessions_with_shadow_sampling(context, tmux_bin, true)
}

fn list_sessions_with_shadow_sampling(
    context: &CliContext,
    tmux_bin: Option<&Path>,
    schedule_shadow_sampling: bool,
) -> Result<Vec<SessionView>, CliError> {
    let sessions_root = context.state_dir.join("sessions");
    if !sessions_root.exists() {
        return Ok(Vec::new());
    }
    let tmux_bin = tmux_bin
        .map(Path::to_path_buf)
        .unwrap_or_else(|| resolve_tmux_bin(None));
    let tmux_snapshots = tmux_session_snapshots(&tmux_bin);
    let mut records = Vec::new();
    for entry in fs::read_dir(&sessions_root).map_err(|err| {
        CliError::runtime(
            "session-list-failed",
            format!("failed to read {}: {err}", sessions_root.display()),
            Some(json!({ "path": display_path(&sessions_root) })),
        )
    })? {
        let entry = entry.map_err(|err| {
            CliError::runtime(
                "session-list-failed",
                format!("failed to read session entry: {err}"),
                None,
            )
        })?;
        if entry.path().is_dir() {
            let entry_name = entry.file_name().to_string_lossy().to_string();
            let record_path = entry.path().join("session.json");
            if record_path.is_file() {
                let resolved = ensure_record_in_session_dir(
                    context,
                    &record_path,
                    &entry.path(),
                    &entry_name,
                )?;
                let record = read_session_record(&resolved.record_path)?;
                validate_record_id(&record, &resolved.expected_id, &resolved.record_path)?;
                let (status, observed_last_terminal_activity_at) =
                    session_list_runtime_snapshot(&tmux_bin, tmux_snapshots.as_ref(), &record);
                let observed_status = status.clone();
                let (record, status) = reconcile_startup_projection(
                    context,
                    record,
                    status,
                    tmux_snapshots
                        .as_ref()
                        .map(|snapshots| &snapshots.started_at),
                );
                let last_terminal_activity_at = if status == observed_status {
                    observed_last_terminal_activity_at
                } else {
                    last_terminal_activity_at(&tmux_bin, &record, &status)
                };
                let record = if status == "running" {
                    record
                } else {
                    backfill_provider_resume(context, record)
                };
                records.push(session_view_from_parts(
                    context,
                    &record,
                    status,
                    last_terminal_activity_at,
                    &tmux_bin,
                    schedule_shadow_sampling,
                ));
            }
        }
    }
    records.sort_by(|a, b| {
        b.updated_at
            .cmp(&a.updated_at)
            .then_with(|| b.created_at.cmp(&a.created_at))
            .then_with(|| a.id.cmp(&b.id))
    });
    Ok(records)
}

fn load_session_view(
    context: &CliContext,
    id: &str,
    tmux_bin: Option<&Path>,
) -> Result<SessionView, CliError> {
    let mut record = load_session_record(context, id)?;
    let tmux_bin = tmux_bin
        .map(Path::to_path_buf)
        .unwrap_or_else(|| resolve_tmux_bin(None));
    let status = session_status(&tmux_bin, &record);
    let (reconciled, status) = reconcile_startup_projection(context, record, status, None);
    record = reconciled;
    if status != "running" {
        record = backfill_provider_resume(context, record);
    }
    Ok(session_view(
        context,
        &record,
        Some(status),
        Some(&tmux_bin),
    ))
}

fn load_session_record(context: &CliContext, id: &str) -> Result<SessionRecord, CliError> {
    let resolved = resolve_session_record_path(context, id)?;
    let record = read_session_record(&resolved.record_path)?;
    validate_record_id(&record, &resolved.expected_id, &resolved.record_path)?;
    Ok(record)
}

fn backfill_provider_resume(context: &CliContext, record: SessionRecord) -> SessionRecord {
    if record.provider_resume.is_some()
        || AgentKind::from_name(&record.agent) != Some(AgentKind::Codex)
        || record
            .runtime
            .as_ref()
            .is_some_and(|runtime| runtime.generation == 1)
    {
        return record;
    }
    let Some(provider_resume) = capture_codex_resume_from_history(&record) else {
        return record;
    };
    mutate_session_record(context, &record.id, |current| {
        if current.provider_resume.is_none()
            && current.agent == record.agent
            && current.cwd == record.cwd
            && same_runtime_identity(current.runtime.as_ref(), record.runtime.as_ref())
        {
            current.provider_resume = Some(provider_resume);
        }
        Ok(current.clone())
    })
    .unwrap_or_else(|_| load_session_record(context, &record.id).unwrap_or(record))
}

#[derive(Debug)]
struct ResolvedRecordPath {
    record_path: PathBuf,
    session_dir: PathBuf,
    expected_id: String,
}

fn resolve_session_record_path(
    context: &CliContext,
    id: &str,
) -> Result<ResolvedRecordPath, CliError> {
    validate_id(id)?;
    let exact_dir = session_dir(context, id);
    let exact = exact_dir.join("session.json");
    if exact.is_file() {
        return ensure_record_in_session_dir(context, &exact, &exact_dir, id);
    }
    let sessions_root = context.state_dir.join("sessions");
    let mut matches = Vec::new();
    if sessions_root.exists() {
        for entry in fs::read_dir(&sessions_root).map_err(|err| {
            CliError::runtime(
                "session-list-failed",
                format!("failed to read {}: {err}", sessions_root.display()),
                Some(json!({ "path": display_path(&sessions_root) })),
            )
        })? {
            let entry = entry.map_err(|err| {
                CliError::runtime(
                    "session-list-failed",
                    format!("failed to read session entry: {err}"),
                    None,
                )
            })?;
            let name = entry.file_name().to_string_lossy().to_string();
            let record_path = entry.path().join("session.json");
            if name.starts_with(id) && record_path.is_file() {
                matches.push(ensure_record_in_session_dir(
                    context,
                    &record_path,
                    &entry.path(),
                    &name,
                )?);
            }
        }
    }
    match matches.len() {
        1 => Ok(matches.remove(0)),
        0 => Err(CliError::runtime(
            "session-not-found",
            format!("session not found: {id}"),
            Some(json!({ "id": id })),
        )),
        _ => Err(CliError::usage(
            "ambiguous-session-id",
            format!("session id prefix is ambiguous: {id}"),
            Some(json!({ "id": id, "matches": matches.len() })),
        )),
    }
}

fn ensure_record_in_session_dir(
    context: &CliContext,
    path: &Path,
    expected_session_dir: &Path,
    expected_id: &str,
) -> Result<ResolvedRecordPath, CliError> {
    let sessions_root = context.state_dir.join("sessions");
    let canonical_root = fs::canonicalize(&sessions_root).map_err(|err| {
        CliError::runtime(
            "session-root-unavailable",
            format!("failed to canonicalize {}: {err}", sessions_root.display()),
            Some(json!({ "path": display_path(&sessions_root) })),
        )
    })?;
    let canonical_session_dir = fs::canonicalize(expected_session_dir).map_err(|err| {
        CliError::runtime(
            "session-read-failed",
            format!(
                "failed to canonicalize {}: {err}",
                expected_session_dir.display()
            ),
            Some(json!({ "path": display_path(expected_session_dir) })),
        )
    })?;
    if !canonical_session_dir.starts_with(&canonical_root) {
        return Err(CliError::usage(
            "session-path-escaped",
            "session directory escapes the managed state directory",
            Some(json!({ "session_dir": display_path(expected_session_dir) })),
        ));
    }
    let canonical_path = fs::canonicalize(path).map_err(|err| {
        CliError::runtime(
            "session-read-failed",
            format!("failed to canonicalize {}: {err}", path.display()),
            Some(json!({ "path": display_path(path) })),
        )
    })?;
    let expected_record_path = canonical_session_dir.join("session.json");
    if canonical_path != expected_record_path {
        return Err(CliError::usage(
            "session-path-escaped",
            "session record path escapes the requested session directory",
            Some(json!({
                "path": display_path(path),
                "expected_session_dir": display_path(expected_session_dir),
            })),
        ));
    }
    Ok(ResolvedRecordPath {
        record_path: canonical_path,
        session_dir: canonical_session_dir,
        expected_id: expected_id.to_string(),
    })
}

fn read_session_record(path: &Path) -> Result<SessionRecord, CliError> {
    let contents = fs::read_to_string(path).map_err(|err| {
        CliError::runtime(
            "session-read-failed",
            format!("failed to read {}: {err}", path.display()),
            Some(json!({ "path": display_path(path) })),
        )
    })?;
    let mut record: SessionRecord = serde_json::from_str(&contents).map_err(|err| {
        CliError::data(
            "session-json-invalid",
            format!("failed to parse {}: {err}", path.display()),
            Some(json!({ "path": display_path(path) })),
        )
    })?;
    if record.schema_version != SESSION_DOCUMENT_VERSION {
        return Err(CliError::data(
            "unsupported-session-version",
            format!(
                "unsupported session schema_version {}; expected {}",
                record.schema_version, SESSION_DOCUMENT_VERSION
            ),
            Some(json!({ "path": display_path(path), "schema_version": record.schema_version })),
        ));
    }
    merge_resume_sidecar(path, &mut record)?;
    Ok(record)
}

fn validate_record_id(
    record: &SessionRecord,
    expected_id: &str,
    path: &Path,
) -> Result<(), CliError> {
    if record.id != expected_id {
        return Err(CliError::data(
            "session-record-mismatch",
            format!(
                "session record id {} does not match directory {}",
                record.id, expected_id
            ),
            Some(json!({
                "path": display_path(path),
                "record_id": record.id,
                "expected_id": expected_id,
            })),
        ));
    }
    Ok(())
}

#[derive(Debug)]
pub(crate) struct SessionRecordLock(fs::File);

impl Drop for SessionRecordLock {
    fn drop(&mut self) {
        // SAFETY: `flock` only observes the valid descriptor owned by this lock.
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

pub(crate) struct LockedSessionAuthority {
    _lock: SessionRecordLock,
    pub(crate) record: SessionRecord,
}

pub(crate) fn lock_exact_session_authority(
    context: &CliContext,
    id: &str,
) -> Result<Option<LockedSessionAuthority>, CliError> {
    validate_id(id)?;
    let lock = acquire_session_record_lock(context, id)?;
    let directory = session_dir(context, id);
    let record_path = directory.join("session.json");
    match fs::symlink_metadata(&record_path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(session_io_error("session-read-failed", &record_path, error));
        }
        Ok(_) => {}
    }
    let resolved = ensure_record_in_session_dir(context, &record_path, &directory, id)?;
    let record = read_session_record(&resolved.record_path)?;
    validate_record_id(&record, &resolved.expected_id, &resolved.record_path)?;
    Ok(Some(LockedSessionAuthority {
        _lock: lock,
        record,
    }))
}

fn acquire_session_record_lock(
    context: &CliContext,
    id: &str,
) -> Result<SessionRecordLock, CliError> {
    acquire_session_record_lock_with_mode(context, id, SessionRecordLockMode::Blocking)?.ok_or_else(
        || {
            CliError::runtime(
                "session-record-lock-busy",
                "session record is busy",
                Some(json!({ "id": id })),
            )
        },
    )
}

pub(crate) fn try_acquire_session_record_lock(
    context: &CliContext,
    id: &str,
) -> Result<Option<SessionRecordLock>, CliError> {
    acquire_session_record_lock_with_mode(context, id, SessionRecordLockMode::NonBlocking)
}

pub(crate) fn acquire_session_record_lock_timed(
    context: &CliContext,
    id: &str,
    timeout: Duration,
) -> Result<SessionRecordLock, CliError> {
    acquire_session_record_lock_with_mode(context, id, SessionRecordLockMode::Timed(timeout))?
        .ok_or_else(|| {
            CliError::runtime(
                "session-record-lock-timeout",
                "timed out waiting for the session record lock",
                Some(json!({ "id": id })),
            )
        })
}

#[derive(Clone, Copy)]
enum SessionRecordLockMode {
    Blocking,
    NonBlocking,
    Timed(Duration),
}

fn acquire_session_record_lock_with_mode(
    context: &CliContext,
    id: &str,
    mode: SessionRecordLockMode,
) -> Result<Option<SessionRecordLock>, CliError> {
    validate_id(id)?;
    let lock_dir = context.state_dir.join(SESSION_LOCKS_DIR);
    ensure_private_dir(&lock_dir)?;
    let path = lock_dir.join(format!("{id}.lock"));
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .mode(SECRET_FILE_MODE)
        .open(&path)
        .map_err(|err| session_io_error("session-record-lock-open-failed", &path, err))?;
    fs::set_permissions(&path, fs::Permissions::from_mode(SECRET_FILE_MODE))
        .map_err(|err| session_io_error("session-record-lock-permission-failed", &path, err))?;
    if matches!(mode, SessionRecordLockMode::Blocking) {
        // SAFETY: `flock` only observes the valid descriptor owned by `file`.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(session_io_error(
                "session-record-lock-failed",
                &path,
                io::Error::last_os_error(),
            ));
        }
        return Ok(Some(SessionRecordLock(file)));
    }
    let deadline = match mode {
        SessionRecordLockMode::Timed(timeout) => Some(Instant::now() + timeout),
        SessionRecordLockMode::NonBlocking => None,
        SessionRecordLockMode::Blocking => unreachable!("blocking mode returned above"),
    };
    loop {
        // SAFETY: `flock` only observes the valid descriptor owned by `file`.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(Some(SessionRecordLock(file)));
        }
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::WouldBlock {
            return Err(session_io_error("session-record-lock-failed", &path, err));
        }
        let Some(deadline) = deadline else {
            return Ok(None);
        };
        if Instant::now() >= deadline {
            return Ok(None);
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn session_io_error(code: &str, path: &Path, err: io::Error) -> CliError {
    CliError::runtime(
        code,
        format!("session storage failed at {}: {err}", path.display()),
        Some(json!({ "path": display_path(path) })),
    )
}

pub(crate) fn mutate_session_record<T, F>(
    context: &CliContext,
    id: &str,
    mutate: F,
) -> Result<T, CliError>
where
    F: FnOnce(&mut SessionRecord) -> Result<T, CliError>,
{
    // Preserve the public not-found contract before touching the private lock
    // file; the authoritative record is reloaded after the lock is acquired.
    let observed = load_session_record(context, id)?;
    let canonical_id = observed.id.clone();
    let _lock = acquire_session_record_lock(context, &canonical_id)?;
    let mut record = load_session_record(context, &canonical_id)?;
    ensure_same_session_identity(&observed, &record)?;
    let result = mutate(&mut record)?;
    write_session_record(context, &record)?;
    Ok(result)
}

fn mutate_session_record_for_title<T, F>(
    context: &CliContext,
    id: &str,
    expected_session_created_at: Option<&str>,
    expected_session_incarnation: Option<&str>,
    mutate: F,
) -> Result<T, CliError>
where
    F: FnOnce(&mut SessionRecord) -> Result<T, CliError>,
{
    let observed = load_session_record(context, id)?;
    let canonical_id = observed.id.clone();
    let _lock = acquire_session_record_lock(context, &canonical_id)?;
    let mut record = load_session_record(context, &canonical_id)?;
    if let Some(expected) = expected_session_created_at
        && record.created_at != expected
    {
        return Err(CliError::data(
            "session-incarnation-conflict",
            "session was replaced since it was read",
            Some(json!({
                "expected_session_created_at": expected,
                "actual_session_created_at": record.created_at,
            })),
        ));
    }
    if let Some(expected) = expected_session_incarnation {
        let actual = record
            .runtime
            .as_ref()
            .map(|runtime| runtime.launch_id.as_str())
            .filter(|launch_id| !launch_id.is_empty());
        if actual != Some(expected) {
            return Err(CliError::data(
                "session-incarnation-conflict",
                "session runtime was replaced since it was read",
                Some(json!({
                    "expected_session_incarnation": expected,
                    "actual_session_incarnation": actual,
                })),
            ));
        }
    }
    ensure_same_session_identity(&observed, &record)?;
    let result = mutate(&mut record)?;
    write_session_record(context, &record)?;
    Ok(result)
}

pub(crate) fn write_session_record(
    context: &CliContext,
    record: &SessionRecord,
) -> Result<(), CliError> {
    let bytes = serde_json::to_vec_pretty(record).map_err(|err| {
        CliError::runtime(
            "session-render-failed",
            format!("failed to render session json: {err}"),
            None,
        )
    })?;
    let path = session_dir(context, &record.id).join("session.json");
    write_resume_sidecar(context, record)?;
    write_private_file(&path, &bytes)
}

fn merge_resume_sidecar(path: &Path, record: &mut SessionRecord) -> Result<(), CliError> {
    let sidecar_path = path.with_file_name(SESSION_RESUME_FILE);
    if !sidecar_path.is_file() {
        return Ok(());
    }
    let Ok(contents) = fs::read_to_string(&sidecar_path) else {
        return Ok(());
    };
    let Ok(sidecar) = serde_json::from_str::<DurableResumeRecord>(&contents) else {
        return Ok(());
    };
    if sidecar.schema_version != SESSION_RESUME_DOCUMENT_VERSION {
        return Ok(());
    }
    if let Some(provider_resume) = sidecar.provider_resume.clone() {
        if let Some(existing) = record.provider_resume.as_mut() {
            merge_extra_fields(&mut existing.extra, provider_resume.extra);
        } else {
            record.provider_resume = Some(provider_resume);
        }
    }
    if let Some(runtime) = sidecar.runtime.clone() {
        if let Some(existing) = record.runtime.as_mut() {
            merge_extra_fields(&mut existing.extra, runtime.extra);
        } else {
            record.runtime = Some(runtime);
        }
    }
    if record.agent_args.is_empty() {
        record.agent_args = sidecar.agent_args.clone();
    }
    if record.agent_bin.is_none() {
        record.agent_bin = sidecar.agent_bin.clone();
    }
    record.resume_sidecar_extra = sidecar.extra;
    Ok(())
}

fn merge_extra_fields(target: &mut BTreeMap<String, Value>, source: BTreeMap<String, Value>) {
    for (key, value) in source {
        target.entry(key).or_insert(value);
    }
}

fn write_resume_sidecar(context: &CliContext, record: &SessionRecord) -> Result<(), CliError> {
    let path = session_dir(context, &record.id).join(SESSION_RESUME_FILE);
    let Some(sidecar) = durable_resume_record(record) else {
        return remove_current_resume_sidecar_if_present(&path);
    };
    if should_preserve_existing_unsupported_resume_sidecar(&path) {
        return Ok(());
    }
    let bytes = serde_json::to_vec_pretty(&sidecar).map_err(|err| {
        CliError::runtime(
            "session-render-failed",
            format!("failed to render resume json: {err}"),
            None,
        )
    })?;
    write_private_file(&path, &bytes)
}

fn should_preserve_existing_unsupported_resume_sidecar(path: &Path) -> bool {
    match fs::read_to_string(path) {
        Ok(contents) => serde_json::from_str::<DurableResumeRecord>(&contents)
            .map(|sidecar| sidecar.schema_version != SESSION_RESUME_DOCUMENT_VERSION)
            .unwrap_or(true),
        Err(_) => false,
    }
}

fn remove_current_resume_sidecar_if_present(path: &Path) -> Result<(), CliError> {
    let Ok(contents) = fs::read_to_string(path) else {
        return Ok(());
    };
    let Ok(sidecar) = serde_json::from_str::<DurableResumeRecord>(&contents) else {
        return Ok(());
    };
    if sidecar.schema_version != SESSION_RESUME_DOCUMENT_VERSION {
        return Ok(());
    }
    fs::remove_file(path).map_err(|err| {
        CliError::runtime(
            "file-delete-failed",
            format!("failed to delete {}: {err}", path.display()),
            Some(json!({ "path": display_path(path) })),
        )
    })
}

fn durable_resume_record(record: &SessionRecord) -> Option<DurableResumeRecord> {
    record
        .provider_resume
        .as_ref()
        .map(|provider_resume| DurableResumeRecord {
            schema_version: SESSION_RESUME_DOCUMENT_VERSION.to_string(),
            provider_resume: Some(provider_resume.clone()),
            runtime: record.runtime.clone(),
            agent_args: record.agent_args.clone(),
            agent_bin: record.agent_bin.clone(),
            extra: record.resume_sidecar_extra.clone(),
        })
}

fn session_view(
    context: &CliContext,
    record: &SessionRecord,
    forced_status: Option<String>,
    tmux_bin: Option<&Path>,
) -> SessionView {
    let fallback_tmux;
    let tmux_bin = match tmux_bin {
        Some(tmux_bin) => tmux_bin,
        None => {
            fallback_tmux = resolve_tmux_bin(None);
            &fallback_tmux
        }
    };
    let status = forced_status.unwrap_or_else(|| session_status(tmux_bin, record));
    let last_terminal_activity_at = last_terminal_activity_at(tmux_bin, record, &status);
    session_view_from_parts(
        context,
        record,
        status,
        last_terminal_activity_at,
        tmux_bin,
        false,
    )
}

fn session_view_from_parts(
    context: &CliContext,
    record: &SessionRecord,
    status: String,
    last_terminal_activity_at: Option<String>,
    tmux_bin: &Path,
    schedule_shadow_sampling: bool,
) -> SessionView {
    let resume_blocked_reason =
        match orchestration::session_authority_is_quarantined(context, record) {
            Ok(true) => Some("worker-quarantined".to_string()),
            Ok(false) => None,
            Err(_) => Some("worker-quarantine-unavailable".to_string()),
        };
    let resumable = is_resumable(record) && resume_blocked_reason.is_none();
    let profile_resume_context = if status == "stopped" && resumable {
        durable_profile_resume_context(record)
    } else {
        Ok(None)
    };
    let turn_state = activity::state_for_view(context, record).map(|state| {
        activity::shadow::annotate_for_view(
            context,
            record,
            &status,
            tmux_bin,
            state,
            schedule_shadow_sampling,
        )
    });
    SessionView {
        id: record.id.clone(),
        agent: record.agent.clone(),
        capabilities: if status == "running"
            && codex_app_server::managed_account_handoff_supported(record)
        {
            vec![codex_app_server::MANAGED_ACCOUNT_HANDOFF_CAPABILITY]
        } else {
            Vec::new()
        },
        agent_profile: session_agent_profile(record).map(str::to_string),
        profile_resume_context,
        mode: record.mode.clone(),
        coordination_mode: record.coordination_mode,
        title: record.title.clone(),
        title_state: effective_session_title_state(record),
        title_state_supported: true,
        title_revision: record.title_revision,
        session_incarnation: record
            .runtime
            .as_ref()
            .map(|runtime| runtime.launch_id.clone())
            .filter(|launch_id| !launch_id.is_empty()),
        cwd: record.cwd.clone(),
        tmux_session: record.tmux_session.clone(),
        status,
        resumable,
        resume_blocked_reason,
        repo_name: repo_name_from_cwd(&record.cwd),
        provider_resume: record
            .provider_resume
            .as_ref()
            .map(ProviderResumeView::from),
        attach_command: local_attach_command(&record.tmux_session),
        ssh_attach_command: context
            .host
            .as_deref()
            .filter(|host| !host.trim().is_empty())
            .map(|host| ssh_attach_command(host, &record.tmux_session)),
        prompt_file: record.prompt_file.clone(),
        log_file: record.log_file.clone(),
        created_at: record.created_at.clone(),
        updated_at: record.updated_at.clone(),
        last_terminal_activity_at,
        runtime_started_at: record
            .runtime
            .as_ref()
            .map(|runtime| runtime.started_at.clone()),
        turn_state,
        // Populated on demand by the list handler; never computed in the shared
        // collector so the expensive transcript path stays out of the hot build.
        last_prompt: None,
        last_prompt_state: None,
        last_prompt_continuity: None,
        startup: startup_projection_for_view(record),
        auto_resume: auto_resume::view_for_record(context, record),
        codex_account: codex_account::view_for_record(record),
        coordination: coordination::public_summary(context, &record.id),
        orchestration: orchestration::session_projection(context, record)
            .ok()
            .flatten(),
    }
}

fn runtime_extra_string<'a>(record: &'a SessionRecord, key: &str) -> Option<&'a str> {
    record
        .runtime
        .as_ref()
        .and_then(|runtime| runtime.extra.get(key))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
}

pub(crate) fn session_agent_profile(record: &SessionRecord) -> Option<&str> {
    runtime_extra_string(record, AGENT_PROFILE_RUNTIME_KEY)
}

pub(crate) fn session_provider_config_dir(record: &SessionRecord) -> Option<PathBuf> {
    runtime_extra_string(record, AGENT_PROFILE_PROVIDER_CONFIG_DIR_RUNTIME_KEY).map(PathBuf::from)
}

/// Authoritative Codex usage account nickname persisted for a `claude` launch
/// profile that runs on a Codex/GPT backend. Present only when the launch
/// profile declared `codex_usage_account`; the auto-resume loop keys off this
/// account's rate limits instead of native Claude usage.
pub(crate) fn session_codex_usage_account(record: &SessionRecord) -> Option<String> {
    runtime_extra_string(record, AGENT_PROFILE_CODEX_USAGE_ACCOUNT_RUNTIME_KEY).map(str::to_string)
}

pub(crate) fn session_profile_auto_resume_setting(record: &SessionRecord) -> Option<bool> {
    record.runtime.as_ref().and_then(|runtime| {
        runtime
            .extra
            .get(AGENT_PROFILE_AUTO_RESUME_SUPPORTED_RUNTIME_KEY)
            .and_then(Value::as_bool)
    })
}

pub(crate) fn session_profile_auto_resume_supported(record: &SessionRecord) -> bool {
    let configured = session_profile_auto_resume_setting(record);
    if session_agent_profile(record).is_some() {
        configured == Some(true)
    } else {
        configured.unwrap_or(true)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DurableProfileResumeContext {
    profile_id: String,
    agent: AgentKind,
    agent_bin: PathBuf,
    provider_config_dir: Option<PathBuf>,
    auto_resume_supported: bool,
}

fn validate_durable_profile_resume_context(
    record: &SessionRecord,
) -> Result<Option<DurableProfileResumeContext>, CliError> {
    let Some(context) = durable_profile_resume_context(record)? else {
        return Ok(None);
    };
    let launcher_ready = fs::metadata(&context.agent_bin)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0);
    let provider_root_ready = context
        .provider_config_dir
        .as_ref()
        .is_none_or(|root| fs::metadata(root).is_ok_and(|metadata| metadata.is_dir()));
    if !launcher_ready || !provider_root_ready {
        return Err(profile_unavailable(&context.profile_id));
    }
    Ok(Some(context))
}

fn durable_profile_resume_context(
    record: &SessionRecord,
) -> Result<Option<DurableProfileResumeContext>, CliError> {
    let Some(profile_id) = session_agent_profile(record) else {
        return Ok(None);
    };
    let Some(agent) = AgentKind::from_name(&record.agent) else {
        return Err(profile_metadata_unavailable(profile_id));
    };
    if agent == AgentKind::Hermes {
        return Err(profile_metadata_unavailable(profile_id));
    }
    let Some(agent_bin) = record
        .agent_bin
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
    else {
        return Err(profile_metadata_unavailable(profile_id));
    };
    let provider_config_dir = session_provider_config_dir(record);
    let Some(auto_resume_supported) = session_profile_auto_resume_setting(record) else {
        return Err(profile_metadata_unavailable(profile_id));
    };
    if !agent_bin.is_absolute()
        || provider_config_dir
            .as_ref()
            .is_some_and(|path| !path.is_absolute())
    {
        return Err(profile_metadata_unavailable(profile_id));
    }
    Ok(Some(DurableProfileResumeContext {
        profile_id: profile_id.to_string(),
        agent,
        agent_bin,
        provider_config_dir,
        auto_resume_supported,
    }))
}

fn profile_metadata_unavailable(profile_id: &str) -> CliError {
    CliError::runtime(
        "agent-profile-metadata-unavailable",
        "session launch profile metadata does not contain an enforceable provider context",
        Some(json!({ "agent_profile": profile_id })),
    )
}

fn profile_unavailable(profile_id: &str) -> CliError {
    CliError::runtime(
        "agent-profile-unavailable",
        "agent launch profile is unavailable",
        Some(json!({ "agent_profile": profile_id })),
    )
}

fn last_terminal_activity_at(
    tmux_bin: &Path,
    record: &SessionRecord,
    status: &str,
) -> Option<String> {
    if status != "running" {
        return None;
    }
    tmux_window_activity_at(tmux_bin, &record.tmux_session)
}

#[derive(Debug, Clone)]
struct TmuxSessionSnapshot {
    last_terminal_activity_at: Option<String>,
}

struct TmuxSessionSnapshots {
    started_at: Timestamp,
    sessions: BTreeMap<String, TmuxSessionSnapshot>,
}

impl TmuxSessionSnapshots {
    fn contains_key(&self, tmux_session: &str) -> bool {
        self.sessions.contains_key(tmux_session)
    }
}

fn session_list_runtime_snapshot(
    tmux_bin: &Path,
    tmux_snapshots: Option<&TmuxSessionSnapshots>,
    record: &SessionRecord,
) -> (String, Option<String>) {
    match tmux_snapshots {
        Some(snapshots) => match snapshots.sessions.get(&record.tmux_session) {
            Some(snapshot) => (
                "running".to_string(),
                snapshot.last_terminal_activity_at.clone(),
            ),
            None => ("stopped".to_string(), None),
        },
        None => (session_status(tmux_bin, record), None),
    }
}

fn tmux_session_snapshots(tmux_bin: &Path) -> Option<TmuxSessionSnapshots> {
    let started_at = Timestamp::now();
    let output = ProcessCommand::new(tmux_bin)
        .arg("list-windows")
        .arg("-a")
        .arg("-F")
        .arg("#{session_name}\t#{window_activity}")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let raw = String::from_utf8_lossy(&output.stdout);
    let mut activity_by_session: BTreeMap<String, Option<i64>> = BTreeMap::new();
    for line in raw.lines() {
        let Some((session, activity)) = line.split_once('\t') else {
            continue;
        };
        if session.is_empty() {
            continue;
        }
        let Some(epoch_seconds) = parse_tmux_window_activity_seconds(activity) else {
            activity_by_session
                .entry(session.to_string())
                .or_insert(None);
            continue;
        };
        activity_by_session
            .entry(session.to_string())
            .and_modify(|current| {
                *current = Some(current.map_or(epoch_seconds, |value| value.max(epoch_seconds)));
            })
            .or_insert(Some(epoch_seconds));
    }
    Some(TmuxSessionSnapshots {
        started_at,
        sessions: activity_by_session
            .into_iter()
            .map(|(session, maybe_epoch_seconds)| {
                (
                    session,
                    TmuxSessionSnapshot {
                        last_terminal_activity_at: maybe_epoch_seconds
                            .and_then(format_tmux_window_activity),
                    },
                )
            })
            .collect(),
    })
}

fn tmux_window_activity_at(tmux_bin: &Path, tmux_session: &str) -> Option<String> {
    let output = ProcessCommand::new(tmux_bin)
        .arg("display-message")
        .arg("-p")
        .arg("-t")
        .arg(tmux_session)
        .arg("#{window_activity}")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let raw = String::from_utf8_lossy(&output.stdout);
    let epoch_seconds = parse_tmux_window_activity_seconds(raw.trim())?;
    format_tmux_window_activity(epoch_seconds)
}

fn parse_tmux_window_activity_seconds(raw: &str) -> Option<i64> {
    let epoch_seconds = raw.trim().parse::<i64>().ok()?;
    (epoch_seconds > 0).then_some(epoch_seconds)
}

fn format_tmux_window_activity(epoch_seconds: i64) -> Option<String> {
    Timestamp::from_second(epoch_seconds)
        .ok()
        .map(|timestamp| timestamp.to_string())
}

fn session_logs(
    context: &CliContext,
    record: &SessionRecord,
    tail: usize,
    tmux_bin: &Path,
) -> Result<LogsResult, CliError> {
    if let Some(result) = read_session_log_file(context, record, tail)? {
        return Ok(result);
    }

    if live_status(tmux_bin, &record.tmux_session) == "running"
        && let Some(text) = run_capture_pane(record, tail, tmux_bin)?
    {
        return Ok(LogsResult {
            id: record.id.clone(),
            source: "tmux".to_string(),
            text,
        });
    }

    Err(CliError::runtime(
        "logs-unavailable",
        "no tmux pane output, log file, or failure diagnostic is available",
        Some(json!({ "id": record.id })),
    ))
}

fn read_session_log_file(
    context: &CliContext,
    record: &SessionRecord,
    tail: usize,
) -> Result<Option<LogsResult>, CliError> {
    if let Some(log_file) = &record.log_file {
        match fs::read_to_string(log_file) {
            Ok(text) => {
                return Ok(Some(LogsResult {
                    id: record.id.clone(),
                    source: "file".to_string(),
                    text: tail_lines(&text, tail),
                }));
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(CliError::runtime(
                    "log-read-failed",
                    format!("failed to read {log_file}: {err}"),
                    Some(json!({ "log_file": log_file })),
                ));
            }
        }
    }
    let diagnostic = session_dir(context, &record.id).join(STARTUP_DIAGNOSTIC_FILE);
    match fs::read(&diagnostic) {
        Ok(bytes) => Ok(Some(LogsResult {
            id: record.id.clone(),
            source: "diagnostic".to_string(),
            text: tail_lines(&String::from_utf8_lossy(&bytes), tail),
        })),
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            let exit_status = session_dir(context, &record.id).join(RUNTIME_EXIT_STATUS_FILE);
            let Some(status) = read_bounded_startup_marker(&exit_status)
                .and_then(|value| value.parse::<u8>().ok())
                .filter(|status| *status != 0)
            else {
                return Ok(None);
            };
            Ok(Some(LogsResult {
                id: record.id.clone(),
                source: "diagnostic".to_string(),
                text: tail_lines(
                    &format!("provider client exited with status {status}\n"),
                    tail,
                ),
            }))
        }
        Err(err) => Err(CliError::runtime(
            "log-read-failed",
            format!("failed to read {}: {err}", diagnostic.display()),
            Some(json!({ "log_file": display_path(&diagnostic) })),
        )),
    }
}

fn delete_session(
    context: &CliContext,
    id: &str,
    tmux_bin: PathBuf,
) -> Result<DeleteResult, CliError> {
    delete_session_with_timeouts(
        context,
        id,
        tmux_bin,
        PANE_INPUT_COMMAND_TIMEOUT,
        DELETE_TERMINATION_VERIFY_TIMEOUT,
    )
}

fn delete_session_with_timeouts(
    context: &CliContext,
    id: &str,
    tmux_bin: PathBuf,
    kill_timeout: Duration,
    verify_timeout: Duration,
) -> Result<DeleteResult, CliError> {
    let observed = load_session_record(context, id)?;
    let canonical_id = observed.id.clone();
    let _record_lock = acquire_session_record_lock(context, &canonical_id)?;
    let resolved = resolve_session_record_path(context, &canonical_id)?;
    let record = read_session_record(&resolved.record_path)?;
    ensure_same_session_identity(&observed, &record)?;
    validate_record_id(&record, &resolved.expected_id, &resolved.record_path)?;
    delete_session_locked_with_timeouts(
        context,
        record,
        resolved.session_dir,
        &tmux_bin,
        kill_timeout,
        verify_timeout,
    )
}

fn delete_session_locked_with_timeouts(
    context: &CliContext,
    mut record: SessionRecord,
    session_dir: PathBuf,
    tmux_bin: &Path,
    kill_timeout: Duration,
    verify_timeout: Duration,
) -> Result<DeleteResult, CliError> {
    let registry_fence = SessionRegistryFence::from_record(&record);
    terminate_tmux_session_with_timeouts(
        context,
        &mut record,
        tmux_bin,
        None,
        kill_timeout,
        verify_timeout,
        true,
    )
    .map_err(|reason| {
        session_termination_error(&record, reason, SessionTerminationOperation::Delete)
    })?;
    coordination::revoke(context, &record)?;
    codex_app_server::cleanup_runtime_files(context, &record)?;
    let cleanup_pending = commit_session_directory_delete(context, &record.id, &session_dir)?;
    Ok(DeleteResult {
        id: record.id,
        tmux_session: record.tmux_session,
        killed: true,
        deleted: true,
        session_dir: display_path(&session_dir),
        cleanup_pending,
        registry_fence,
    })
}

fn commit_session_directory_delete(
    context: &CliContext,
    id: &str,
    session_dir: &Path,
) -> Result<bool, CliError> {
    commit_session_directory_delete_with(context, id, session_dir, |path| fs::remove_dir_all(path))
}

fn commit_session_directory_delete_with<F>(
    context: &CliContext,
    id: &str,
    session_dir: &Path,
    cleanup: F,
) -> Result<bool, CliError>
where
    F: FnOnce(&Path) -> io::Result<()>,
{
    let tombstones = context.state_dir.join(SESSION_DELETE_TOMBSTONES_DIR);
    ensure_private_delete_tombstone_root(&tombstones)?;
    let tombstone = tombstones.join(format!("{id}-{}", uuid::Uuid::new_v4()));
    fs::rename(session_dir, &tombstone).map_err(|err| {
        CliError::runtime(
            "session-delete-failed",
            format!("failed to delete {}: {err}", session_dir.display()),
            Some(json!({ "path": display_path(session_dir) })),
        )
    })?;
    // The same-filesystem rename is the logical delete commit. Cleanup happens
    // outside the live sessions namespace and is deliberately best-effort: a
    // failure can leave quarantined bytes, but can no longer produce a partial
    // live session while the API falsely claims metadata was retained.
    Ok(cleanup(&tombstone).is_err())
}

fn ensure_private_delete_tombstone_root(root: &Path) -> Result<(), CliError> {
    match fs::create_dir(root) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => {
            return Err(CliError::runtime(
                "session-delete-failed",
                format!("failed to prepare session delete quarantine: {error}"),
                None,
            ));
        }
    }
    let metadata = fs::symlink_metadata(root).map_err(|error| {
        CliError::runtime(
            "session-delete-failed",
            format!("failed to verify session delete quarantine: {error}"),
            None,
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(CliError::runtime(
            "session-delete-failed",
            "session delete quarantine is not a private directory",
            None,
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(root, fs::Permissions::from_mode(0o700)).map_err(|error| {
            CliError::runtime(
                "session-delete-failed",
                format!("failed to secure session delete quarantine: {error}"),
                None,
            )
        })?;
    }
    Ok(())
}

fn cleanup_session_delete_tombstones(context: &CliContext, limit: usize) -> io::Result<usize> {
    let root = context.state_dir.join(SESSION_DELETE_TOMBSTONES_DIR);
    let root_type = match fs::symlink_metadata(&root) {
        Ok(metadata) => metadata.file_type(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error),
    };
    if root_type.is_symlink() || !root_type.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "session delete tombstone root is not a private directory",
        ));
    }
    let entries = fs::read_dir(&root)?;
    let mut removed = 0;
    for entry in entries.take(limit) {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let path = entry.path();
        let result = if file_type.is_dir() && !file_type.is_symlink() {
            fs::remove_dir_all(&path)
        } else {
            fs::remove_file(&path)
        };
        if result.is_ok() {
            removed += 1;
        }
    }
    Ok(removed)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SessionTerminationFailure {
    KillFailed,
    KillTimeout,
    KillError,
    StillRunning,
    ProcessStillRunning,
    RuntimeIdentityChanged,
    RuntimeIdentityMismatch,
    RuntimeIdentityUnavailable,
    VerificationFailed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SessionTerminationOperation {
    Delete,
    Resume,
    FailedLaunch,
    RuntimeStop,
}

impl SessionTerminationOperation {
    fn retry_action(self) -> &'static str {
        match self {
            Self::Resume => "retry-resume",
            Self::RuntimeStop => "retry-runtime-stop",
            Self::Delete | Self::FailedLaunch => "retry-delete",
        }
    }
}

pub(crate) fn stop_session_runtime_locked(
    context: &CliContext,
    record: &mut SessionRecord,
    tmux_bin: &Path,
) -> Result<(), CliError> {
    terminate_tmux_session_with_timeouts(
        context,
        record,
        tmux_bin,
        None,
        PANE_INPUT_COMMAND_TIMEOUT,
        DELETE_TERMINATION_VERIFY_TIMEOUT,
        true,
    )
    .map_err(|reason| {
        session_termination_error(record, reason, SessionTerminationOperation::RuntimeStop)
    })
}

impl SessionTerminationFailure {
    fn reason(self) -> &'static str {
        match self {
            Self::KillFailed => "kill-failed",
            Self::KillTimeout => "kill-timeout",
            Self::KillError => "kill-error",
            Self::StillRunning => "session-still-running",
            Self::ProcessStillRunning => "process-still-running",
            Self::RuntimeIdentityChanged => "runtime-identity-changed",
            Self::RuntimeIdentityMismatch => "runtime-identity-mismatch",
            Self::RuntimeIdentityUnavailable => "runtime-identity-unavailable",
            Self::VerificationFailed => "verification-failed",
        }
    }

    fn message(self) -> &'static str {
        match self {
            Self::KillFailed => "tmux kill-session failed",
            Self::KillTimeout => "tmux kill-session timed out",
            Self::KillError => "tmux kill-session could not be executed",
            Self::StillRunning => "tmux session remained live after kill-session",
            Self::ProcessStillRunning => {
                "the recorded tmux runtime process boundary remained live after kill-session"
            }
            Self::RuntimeIdentityChanged => {
                "the managed tmux pane identity changed before kill-session"
            }
            Self::RuntimeIdentityMismatch => {
                "the live tmux session does not belong to the recorded agent session"
            }
            Self::RuntimeIdentityUnavailable => {
                "the recorded tmux runtime identity could not be established"
            }
            Self::VerificationFailed => "tmux session termination could not be verified",
        }
    }

    fn retryable(self) -> bool {
        !matches!(
            self,
            Self::RuntimeIdentityMismatch | Self::RuntimeIdentityUnavailable
        )
    }

    fn action(self, operation: SessionTerminationOperation) -> &'static str {
        match self {
            Self::RuntimeIdentityMismatch => "resolve-runtime-identity-mismatch",
            Self::RuntimeIdentityUnavailable => "manual-runtime-verification-required",
            _ => operation.retry_action(),
        }
    }
}

fn session_termination_error(
    record: &SessionRecord,
    reason: SessionTerminationFailure,
    operation: SessionTerminationOperation,
) -> CliError {
    CliError::runtime(
        "session-termination-failed",
        format!("{}; session metadata was retained", reason.message()),
        Some(json!({
            "id": record.id,
            "tmux_session": record.tmux_session,
            "reason": reason.reason(),
            "retryable": reason.retryable(),
            "action": reason.action(operation),
        })),
    )
}

fn recover_failed_tmux_launch(
    context: &CliContext,
    record: &mut SessionRecord,
    tmux_bin: &Path,
    known_identity: Option<&TmuxRuntimeIdentity>,
    operation: SessionTerminationOperation,
) -> Result<(), CliError> {
    let identity = match known_identity {
        Some(identity) => identity.clone(),
        None => match capture_tmux_runtime_identity(
            context,
            record,
            tmux_bin,
            DELETE_TERMINATION_PROBE_TIMEOUT,
        )
        .map_err(|reason| session_termination_error(record, reason, operation))?
        {
            TmuxRuntimeProbe::Running(identity) => *identity,
            TmuxRuntimeProbe::Stopped => {
                return Err(session_termination_error(
                    record,
                    SessionTerminationFailure::RuntimeIdentityUnavailable,
                    operation,
                ));
            }
        },
    };

    persist_tmux_runtime_identity(record, &identity)
        .map_err(|reason| session_termination_error(record, reason, operation))?;
    write_session_record(context, record)?;
    terminate_tmux_session_with_timeouts(
        context,
        record,
        tmux_bin,
        Some(identity),
        PANE_INPUT_COMMAND_TIMEOUT,
        DELETE_TERMINATION_VERIFY_TIMEOUT,
        false,
    )
    .map_err(|reason| session_termination_error(record, reason, operation))
}

#[derive(Clone, Copy)]
enum VerifiedRuntimeTerminationMode<'a> {
    LiveTmux {
        tmux_bin: &'a Path,
        kill_timeout: Duration,
    },
    AlreadyStopped {
        tmux_bin: &'a Path,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VerifiedRuntimeTerminationOutcome {
    Complete,
    TmuxIdentityChanged,
}

fn terminate_verified_process_runtime_transaction(
    context: &CliContext,
    record: &mut SessionRecord,
    identity: &mut TmuxRuntimeIdentity,
    mode: VerifiedRuntimeTerminationMode<'_>,
    verify_timeout: Duration,
) -> Result<VerifiedRuntimeTerminationOutcome, SessionTerminationFailure> {
    let mut pinned_runtime = prepare_process_runtime(identity)?;
    refresh_process_runtime_freeze_ownership(&mut pinned_runtime)?;
    let mut thaw_on_recovery = process_runtime_thaw_on_recovery(&pinned_runtime);
    set_tmux_termination_state(
        record,
        TmuxTerminationState::FreezePending { thaw_on_recovery },
    )?;
    write_session_record(context, record)
        .map_err(|_| SessionTerminationFailure::RuntimeIdentityUnavailable)?;
    if let Err(reason) = freeze_and_pin_process_runtime(identity, &mut pinned_runtime) {
        let _ = thaw_owned_process_runtime(&mut pinned_runtime);
        release_process_runtime(Some(pinned_runtime));
        record.extra.remove(DELETE_TMUX_TERMINATION_STATE_KEY);
        let _ = write_session_record(context, record);
        return Err(reason);
    }
    thaw_on_recovery = process_runtime_thaw_on_recovery(&pinned_runtime);
    identity.control_group_members = process_runtime_identities(&pinned_runtime);
    let prior_identities = persisted_prior_tmux_runtime_identities(record)?;
    persist_tmux_runtime_identities(record, identity, &prior_identities)?;
    set_tmux_termination_state(record, TmuxTerminationState::Pending { thaw_on_recovery })?;
    write_session_record(context, record)
        .map_err(|_| SessionTerminationFailure::RuntimeIdentityUnavailable)?;
    verify_process_runtime_frozen(&pinned_runtime)?;

    let kill_outcome = match mode {
        VerifiedRuntimeTerminationMode::LiveTmux {
            tmux_bin,
            kill_timeout,
        } => tmux_kill_identity_with_timeout(tmux_bin, identity, kill_timeout)?,
        VerifiedRuntimeTerminationMode::AlreadyStopped { tmux_bin } => {
            verify_tmux_target_stopped(tmux_bin, identity, verify_timeout)?;
            TmuxIdentityKillOutcome::KillConfirmed
        }
    };
    if kill_outcome == TmuxIdentityKillOutcome::IdentityChanged {
        thaw_owned_process_runtime(&mut pinned_runtime)?;
        release_process_runtime(Some(pinned_runtime));
        record.extra.remove(DELETE_TMUX_TERMINATION_STATE_KEY);
        write_session_record(context, record)
            .map_err(|_| SessionTerminationFailure::RuntimeIdentityUnavailable)?;
        return Ok(VerifiedRuntimeTerminationOutcome::TmuxIdentityChanged);
    }

    set_tmux_termination_state(
        record,
        TmuxTerminationState::KillConfirmed { thaw_on_recovery },
    )?;
    write_session_record(context, record)
        .map_err(|_| SessionTerminationFailure::RuntimeIdentityUnavailable)?;
    let verification_started = Instant::now();
    let terminated =
        terminate_captured_process_runtime(identity, &mut pinned_runtime, verify_timeout);
    release_process_runtime(Some(pinned_runtime));
    terminated?;
    let remaining = verify_timeout.saturating_sub(verification_started.elapsed());
    if remaining.is_zero() {
        return Err(SessionTerminationFailure::VerificationFailed);
    }
    let tmux_bin = match mode {
        VerifiedRuntimeTerminationMode::LiveTmux { tmux_bin, .. }
        | VerifiedRuntimeTerminationMode::AlreadyStopped { tmux_bin } => tmux_bin,
    };
    verify_stopped_tmux_runtime(tmux_bin, identity, remaining)?;
    record.extra.remove(DELETE_TMUX_TERMINATION_STATE_KEY);
    write_session_record(context, record)
        .map_err(|_| SessionTerminationFailure::RuntimeIdentityUnavailable)?;
    Ok(VerifiedRuntimeTerminationOutcome::Complete)
}

fn terminate_tmux_session_with_timeouts(
    context: &CliContext,
    record: &mut SessionRecord,
    tmux_bin: &Path,
    initial_identity: Option<TmuxRuntimeIdentity>,
    kill_timeout: Duration,
    verify_timeout: Duration,
    terminate_process_group: bool,
) -> Result<(), SessionTerminationFailure> {
    recover_interrupted_tmux_termination_locked(context, record)?;
    let mut identity_changes = 0;
    let mut initial_identity = initial_identity;
    loop {
        let runtime_probe = match initial_identity.take() {
            Some(identity) => TmuxRuntimeProbe::Running(Box::new(identity)),
            None => capture_tmux_runtime_identity(
                context,
                record,
                tmux_bin,
                verify_timeout.min(DELETE_TERMINATION_PROBE_TIMEOUT),
            )?,
        };
        let mut identity = match runtime_probe {
            TmuxRuntimeProbe::Running(identity) => {
                let mut identity = *identity;
                let mut prior_identities = persisted_prior_tmux_runtime_identities(record)?;
                if prior_identities
                    .iter()
                    .any(|prior| !prior.same_runtime_target(&identity))
                {
                    return Err(SessionTerminationFailure::RuntimeIdentityMismatch);
                }
                if let Some(persisted) = persisted_tmux_runtime_identity(record)? {
                    if !persisted.same_runtime_target(&identity) {
                        return Err(SessionTerminationFailure::RuntimeIdentityMismatch);
                    }
                    if persisted.same_process_identity(&identity) {
                        identity.merge_process_evidence_from(&persisted);
                    } else if !prior_identities
                        .iter()
                        .any(|prior| prior.same_process_identity(&persisted))
                    {
                        prior_identities.push(persisted);
                    }
                }
                persist_tmux_runtime_identities(record, &identity, &prior_identities)?;
                write_session_record(context, record)
                    .map_err(|_| SessionTerminationFailure::RuntimeIdentityUnavailable)?;
                verify_stopped_process_runtimes(&prior_identities, verify_timeout)?;
                if !prior_identities.is_empty() {
                    persist_tmux_runtime_identities(record, &identity, &[])?;
                    write_session_record(context, record)
                        .map_err(|_| SessionTerminationFailure::RuntimeIdentityUnavailable)?;
                }
                identity
            }
            TmuxRuntimeProbe::Stopped => {
                if runtime_is_proven_never_launched(record) {
                    return Ok(());
                }
                let identity = persisted_tmux_runtime_identity(record)?
                    .ok_or(SessionTerminationFailure::RuntimeIdentityUnavailable)?;
                let prior_identities = persisted_prior_tmux_runtime_identities(record)?;
                if prior_identities
                    .iter()
                    .any(|prior| !prior.same_runtime_target(&identity))
                {
                    return Err(SessionTerminationFailure::RuntimeIdentityMismatch);
                }
                let verification_started = Instant::now();
                verify_stopped_process_runtimes(&prior_identities, verify_timeout)?;
                let remaining = verify_timeout.saturating_sub(verification_started.elapsed());
                if remaining.is_zero() {
                    return Err(SessionTerminationFailure::VerificationFailed);
                }
                verify_stopped_tmux_runtime(tmux_bin, &identity, remaining)?;
                match persisted_tmux_termination_state(record)? {
                    Some(TmuxTerminationState::FreezePending { .. }) => {
                        return Err(SessionTerminationFailure::RuntimeIdentityUnavailable);
                    }
                    Some(TmuxTerminationState::Pending { .. })
                    | Some(TmuxTerminationState::KillConfirmed { .. }) => {
                        record.extra.remove(DELETE_TMUX_TERMINATION_STATE_KEY);
                        write_session_record(context, record)
                            .map_err(|_| SessionTerminationFailure::RuntimeIdentityUnavailable)?;
                    }
                    None => {}
                }
                return Ok(());
            }
        };

        if identity_changes >= DELETE_TERMINATION_IDENTITY_RETRY_LIMIT {
            return Err(SessionTerminationFailure::RuntimeIdentityChanged);
        }
        if terminate_process_group {
            match terminate_verified_process_runtime_transaction(
                context,
                record,
                &mut identity,
                VerifiedRuntimeTerminationMode::LiveTmux {
                    tmux_bin,
                    kill_timeout,
                },
                verify_timeout,
            )? {
                VerifiedRuntimeTerminationOutcome::Complete => return Ok(()),
                VerifiedRuntimeTerminationOutcome::TmuxIdentityChanged => {
                    identity_changes += 1;
                    continue;
                }
            }
        }

        let thaw_on_recovery = false;
        set_tmux_termination_state(record, TmuxTerminationState::Pending { thaw_on_recovery })?;
        write_session_record(context, record)
            .map_err(|_| SessionTerminationFailure::RuntimeIdentityUnavailable)?;
        match tmux_kill_identity_with_timeout(tmux_bin, &identity, kill_timeout)? {
            TmuxIdentityKillOutcome::IdentityChanged => {
                record.extra.remove(DELETE_TMUX_TERMINATION_STATE_KEY);
                write_session_record(context, record)
                    .map_err(|_| SessionTerminationFailure::RuntimeIdentityUnavailable)?;
                identity_changes += 1;
            }
            TmuxIdentityKillOutcome::KillConfirmed => {
                set_tmux_termination_state(
                    record,
                    TmuxTerminationState::KillConfirmed { thaw_on_recovery },
                )?;
                write_session_record(context, record)
                    .map_err(|_| SessionTerminationFailure::RuntimeIdentityUnavailable)?;
                let verification_started = Instant::now();
                let remaining = verify_timeout.saturating_sub(verification_started.elapsed());
                if remaining.is_zero() {
                    return Err(SessionTerminationFailure::VerificationFailed);
                }
                verify_stopped_tmux_runtime(tmux_bin, &identity, remaining)?;
                record.extra.remove(DELETE_TMUX_TERMINATION_STATE_KEY);
                write_session_record(context, record)
                    .map_err(|_| SessionTerminationFailure::RuntimeIdentityUnavailable)?;
                return Ok(());
            }
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
struct TmuxRuntimeIdentity {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    launch_id: Option<String>,
    session_id: String,
    pane_id: String,
    pane_pid: libc::pid_t,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    process_group_id: Option<libc::pid_t>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    process_session_id: Option<libc::pid_t>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    process_session_members: Vec<TmuxProcessIdentity>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    control_group_members: Vec<TmuxProcessIdentity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    control_group: Option<TmuxControlGroupIdentity>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
struct TmuxProcessIdentity {
    pid: libc::pid_t,
    start_time: u64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
struct TmuxControlGroupIdentity {
    path: String,
    device: u64,
    inode: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    boot_id: Option<String>,
}

impl TmuxRuntimeIdentity {
    fn same_runtime_target(&self, other: &Self) -> bool {
        self.launch_id == other.launch_id
            && self.session_id == other.session_id
            && self.pane_id == other.pane_id
    }

    fn same_process_identity(&self, other: &Self) -> bool {
        self.pane_pid == other.pane_pid
            && self.process_group_id == other.process_group_id
            && (self.process_session_id == other.process_session_id
                || self.process_session_id.is_none()
                || other.process_session_id.is_none())
            && match (&self.control_group, &other.control_group) {
                (Some(left), Some(right)) => left.same_runtime_identity(right),
                _ => true,
            }
    }

    fn merge_process_evidence_from(&mut self, other: &Self) {
        merge_process_identities(
            &mut self.control_group_members,
            &other.control_group_members,
        );
    }
}

fn merge_process_identities(
    identities: &mut Vec<TmuxProcessIdentity>,
    additional: &[TmuxProcessIdentity],
) {
    identities.extend_from_slice(additional);
    identities.sort_unstable_by_key(|identity| (identity.pid, identity.start_time));
    identities.dedup();
}

impl TmuxControlGroupIdentity {
    fn same_runtime_identity(&self, other: &Self) -> bool {
        self.path == other.path
            && self.device == other.device
            && self.inode == other.inode
            && (self.boot_id == other.boot_id || self.boot_id.is_none() || other.boot_id.is_none())
    }
}

enum TmuxRuntimeProbe {
    Running(Box<TmuxRuntimeIdentity>),
    Stopped,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TmuxIdentityKillOutcome {
    KillConfirmed,
    IdentityChanged,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TmuxTerminationState {
    FreezePending { thaw_on_recovery: bool },
    Pending { thaw_on_recovery: bool },
    KillConfirmed { thaw_on_recovery: bool },
}

impl TmuxTerminationState {
    fn thaw_on_recovery(self) -> bool {
        match self {
            Self::FreezePending { thaw_on_recovery }
            | Self::Pending { thaw_on_recovery }
            | Self::KillConfirmed { thaw_on_recovery } => thaw_on_recovery,
        }
    }

    fn with_thaw_on_recovery(self, thaw_on_recovery: bool) -> Self {
        match self {
            Self::FreezePending { .. } => Self::FreezePending { thaw_on_recovery },
            Self::Pending { .. } => Self::Pending { thaw_on_recovery },
            Self::KillConfirmed { .. } => Self::KillConfirmed { thaw_on_recovery },
        }
    }
}

fn capture_tmux_runtime_identity(
    context: &CliContext,
    record: &SessionRecord,
    tmux_bin: &Path,
    timeout: Duration,
) -> Result<TmuxRuntimeProbe, SessionTerminationFailure> {
    let launch_id = record
        .runtime
        .as_ref()
        .map(|runtime| runtime.launch_id.as_str())
        .filter(|launch_id| !launch_id.is_empty())
        .map(ToOwned::to_owned);
    let mut command = ProcessCommand::new(tmux_bin);
    command
        .env("LC_ALL", "C")
        .arg("display-message")
        .arg("-p")
        .arg("-t")
        .arg(managed_tmux_pane_target(&record.tmux_session))
        .arg("#{session_id} #{pane_id} #{pane_pid}");
    let output = run_output_with_timeout(command, timeout)
        .map_err(|_| SessionTerminationFailure::VerificationFailed)?;
    if !output.status.success() {
        return if tmux_output_reports_absent(&output) {
            Ok(TmuxRuntimeProbe::Stopped)
        } else {
            Err(SessionTerminationFailure::VerificationFailed)
        };
    }
    let output = String::from_utf8(output.stdout)
        .map_err(|_| SessionTerminationFailure::RuntimeIdentityUnavailable)?;
    if output.trim().is_empty() {
        let exact_session_target = format!("={}", record.tmux_session);
        return match verified_tmux_status_with_timeout(tmux_bin, &exact_session_target, timeout)
            .as_str()
        {
            "stopped" => Ok(TmuxRuntimeProbe::Stopped),
            "running" => Err(SessionTerminationFailure::RuntimeIdentityUnavailable),
            _ => Err(SessionTerminationFailure::VerificationFailed),
        };
    }
    let mut fields = output.split_whitespace();
    let session_id = fields
        .next()
        .ok_or(SessionTerminationFailure::RuntimeIdentityUnavailable)?;
    let pane_id = fields
        .next()
        .ok_or(SessionTerminationFailure::RuntimeIdentityUnavailable)?;
    let pane_pid = fields
        .next()
        .filter(|_| fields.next().is_none())
        .ok_or(SessionTerminationFailure::RuntimeIdentityUnavailable)?;
    if !valid_tmux_session_id(session_id) || !valid_tmux_pane_id(pane_id) {
        return Err(SessionTerminationFailure::RuntimeIdentityUnavailable);
    }
    let pane_pid = pane_pid
        .trim()
        .parse::<libc::pid_t>()
        .ok()
        .filter(|pid| *pid > 1)
        .ok_or(SessionTerminationFailure::RuntimeIdentityUnavailable)?;

    let expected_state_dir = display_path(&context.state_dir);
    let expected = [
        ("AGENT_SESSION_ID", record.id.as_str()),
        ("AGENT_SESSION_STATE_DIR", expected_state_dir.as_str()),
    ];
    for (name, expected_value) in expected {
        let value = tmux_environment_value(tmux_bin, session_id, name, timeout)?;
        if value != expected_value {
            return Err(SessionTerminationFailure::RuntimeIdentityMismatch);
        }
    }
    if let Some(expected_launch_id) = launch_id.as_deref()
        && tmux_environment_value(tmux_bin, session_id, "AGENT_SESSION_RUNTIME_ID", timeout)?
            != expected_launch_id
    {
        return Err(SessionTerminationFailure::RuntimeIdentityMismatch);
    }
    let process_group_id = process_group_id(pane_pid)?;
    if process_group_id <= 1 || process_group_id == unsafe { libc::getpgrp() } {
        return Err(SessionTerminationFailure::RuntimeIdentityUnavailable);
    }
    let process_session_id = process_session_id(pane_pid)?;
    let process_session_members = process_session_members(process_session_id, pane_pid)?;
    let control_group = linux_process_control_group(pane_pid)?;

    Ok(TmuxRuntimeProbe::Running(Box::new(TmuxRuntimeIdentity {
        launch_id,
        session_id: session_id.to_string(),
        pane_id: pane_id.to_string(),
        pane_pid,
        process_group_id: Some(process_group_id),
        process_session_id,
        process_session_members,
        control_group_members: Vec::new(),
        control_group,
    })))
}

fn tmux_environment_value(
    tmux_bin: &Path,
    session_id: &str,
    name: &str,
    timeout: Duration,
) -> Result<String, SessionTerminationFailure> {
    let mut command = ProcessCommand::new(tmux_bin);
    command
        .env("LC_ALL", "C")
        .arg("show-environment")
        .arg("-t")
        .arg(session_id)
        .arg(name);
    let output = run_output_with_timeout(command, timeout)
        .map_err(|_| SessionTerminationFailure::VerificationFailed)?;
    if !output.status.success() {
        return Err(SessionTerminationFailure::RuntimeIdentityUnavailable);
    }
    let output = String::from_utf8(output.stdout)
        .map_err(|_| SessionTerminationFailure::RuntimeIdentityUnavailable)?;
    let prefix = format!("{name}=");
    output
        .trim_end_matches(['\r', '\n'])
        .strip_prefix(&prefix)
        .map(ToOwned::to_owned)
        .ok_or(SessionTerminationFailure::RuntimeIdentityUnavailable)
}

fn process_group_id(pane_pid: libc::pid_t) -> Result<libc::pid_t, SessionTerminationFailure> {
    let process_group_id = unsafe { libc::getpgid(pane_pid) };
    if process_group_id < 0 {
        Err(SessionTerminationFailure::RuntimeIdentityUnavailable)
    } else {
        Ok(process_group_id)
    }
}

fn process_session_id(
    pane_pid: libc::pid_t,
) -> Result<Option<libc::pid_t>, SessionTerminationFailure> {
    #[cfg(target_os = "linux")]
    {
        let process_session_id = unsafe { libc::getsid(pane_pid) };
        let caller_session_id = unsafe { libc::getsid(0) };
        if process_session_id <= 1 || caller_session_id <= 1 {
            return Err(SessionTerminationFailure::RuntimeIdentityUnavailable);
        }
        if process_session_id == caller_session_id {
            return Ok(None);
        }
        Ok(Some(process_session_id))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pane_pid;
        Ok(None)
    }
}

fn process_session_members(
    process_session_id: Option<libc::pid_t>,
    pane_pid: libc::pid_t,
) -> Result<Vec<TmuxProcessIdentity>, SessionTerminationFailure> {
    #[cfg(target_os = "linux")]
    {
        let Some(process_session_id) = process_session_id else {
            return Ok(Vec::new());
        };
        let members = linux_process_session_members(process_session_id)?;
        if !members.iter().any(|identity| identity.pid == pane_pid) {
            return Err(SessionTerminationFailure::RuntimeIdentityUnavailable);
        }
        Ok(members
            .into_iter()
            .map(|identity| TmuxProcessIdentity {
                pid: identity.pid,
                start_time: identity.start_time,
            })
            .collect())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (process_session_id, pane_pid);
        Ok(Vec::new())
    }
}

fn linux_process_control_group(
    pane_pid: libc::pid_t,
) -> Result<Option<TmuxControlGroupIdentity>, SessionTerminationFailure> {
    #[cfg(target_os = "linux")]
    {
        let Some(path) = linux_process_control_group_path(pane_pid)? else {
            return Ok(None);
        };
        if linux_process_control_group_path(unsafe { libc::getpid() })?.as_deref()
            == Some(path.as_path())
        {
            return Ok(None);
        }
        if !valid_tmux_spawn_control_group_path(&path) {
            return Ok(None);
        }
        let full_path = linux_control_group_full_path(&path)?;
        let metadata =
            fs::metadata(&full_path).map_err(|_| SessionTerminationFailure::VerificationFailed)?;
        if !metadata.is_dir()
            || !full_path.join("cgroup.kill").is_file()
            || !full_path.join("cgroup.freeze").is_file()
            || !full_path.join("cgroup.events").is_file()
        {
            return Ok(None);
        }
        Ok(Some(TmuxControlGroupIdentity {
            path: display_path(&path),
            device: metadata.dev(),
            inode: metadata.ino(),
            boot_id: Some(linux_boot_id()?),
        }))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pane_pid;
        Ok(None)
    }
}

#[cfg(target_os = "linux")]
fn linux_boot_id() -> Result<String, SessionTerminationFailure> {
    let boot_id = fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .map_err(|_| SessionTerminationFailure::VerificationFailed)?;
    let boot_id = boot_id.trim();
    let parsed = uuid::Uuid::parse_str(boot_id)
        .map_err(|_| SessionTerminationFailure::VerificationFailed)?;
    if parsed.to_string() != boot_id {
        return Err(SessionTerminationFailure::VerificationFailed);
    }
    Ok(boot_id.to_string())
}

fn valid_linux_boot_id(boot_id: &str) -> bool {
    uuid::Uuid::parse_str(boot_id).is_ok_and(|parsed| parsed.to_string() == boot_id)
}

#[cfg(target_os = "linux")]
fn linux_process_control_group_path(
    pid: libc::pid_t,
) -> Result<Option<PathBuf>, SessionTerminationFailure> {
    let text = match fs::read_to_string(format!("/proc/{pid}/cgroup")) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(SessionTerminationFailure::VerificationFailed),
    };
    let mut unified = text.lines().filter_map(|line| {
        let (hierarchy, rest) = line.split_once(':')?;
        let (controllers, path) = rest.split_once(':')?;
        (hierarchy == "0" && controllers.is_empty()).then_some(path)
    });
    let Some(path) = unified.next() else {
        return Ok(None);
    };
    if unified.next().is_some() {
        return Err(SessionTerminationFailure::VerificationFailed);
    }
    let path = PathBuf::from(path);
    if !path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir | std::path::Component::CurDir
            )
        })
    {
        return Err(SessionTerminationFailure::VerificationFailed);
    }
    Ok(Some(path))
}

fn valid_tmux_spawn_control_group_path(path: &Path) -> bool {
    if !path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir | std::path::Component::CurDir
            )
        })
    {
        return false;
    }
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let Some(uuid) = name
        .strip_prefix("tmux-spawn-")
        .and_then(|name| name.strip_suffix(".scope"))
    else {
        return false;
    };
    uuid.len() == 36
        && uuid.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

#[cfg(target_os = "linux")]
fn linux_control_group_full_path(path: &Path) -> Result<PathBuf, SessionTerminationFailure> {
    linux_control_group_full_path_at_root(path, Path::new("/sys/fs/cgroup"))
}

#[cfg(target_os = "linux")]
fn linux_control_group_full_path_at_root(
    path: &Path,
    root: &Path,
) -> Result<PathBuf, SessionTerminationFailure> {
    let relative = path
        .strip_prefix("/")
        .map_err(|_| SessionTerminationFailure::VerificationFailed)?;
    Ok(root.join(relative))
}

fn persist_tmux_runtime_identity(
    record: &mut SessionRecord,
    identity: &TmuxRuntimeIdentity,
) -> Result<(), SessionTerminationFailure> {
    record.extra.insert(
        DELETE_TMUX_IDENTITY_KEY.to_string(),
        serde_json::to_value(identity)
            .map_err(|_| SessionTerminationFailure::RuntimeIdentityUnavailable)?,
    );
    record.extra.remove(DELETE_TMUX_PRIOR_IDENTITIES_KEY);
    record.extra.remove(DELETE_TMUX_TERMINATION_STATE_KEY);
    Ok(())
}

fn persist_tmux_runtime_identities(
    record: &mut SessionRecord,
    identity: &TmuxRuntimeIdentity,
    prior_identities: &[TmuxRuntimeIdentity],
) -> Result<(), SessionTerminationFailure> {
    persist_tmux_runtime_identity(record, identity)?;
    if prior_identities.is_empty() {
        record.extra.remove(DELETE_TMUX_PRIOR_IDENTITIES_KEY);
    } else {
        record.extra.insert(
            DELETE_TMUX_PRIOR_IDENTITIES_KEY.to_string(),
            serde_json::to_value(prior_identities)
                .map_err(|_| SessionTerminationFailure::RuntimeIdentityUnavailable)?,
        );
    }
    Ok(())
}

fn persist_launched_tmux_identity(
    context: &CliContext,
    record: &mut SessionRecord,
    identity: &TmuxRuntimeIdentity,
) -> Result<(), CliError> {
    persist_tmux_runtime_identity(record, identity).map_err(|_| {
        CliError::runtime(
            "tmux-runtime-identity-persist-failed",
            "failed to retain the launched tmux runtime identity",
            Some(json!({ "id": record.id })),
        )
    })?;
    write_session_record(context, record)
}

fn persisted_tmux_runtime_identity(
    record: &SessionRecord,
) -> Result<Option<TmuxRuntimeIdentity>, SessionTerminationFailure> {
    let Some(value) = record.extra.get(DELETE_TMUX_IDENTITY_KEY) else {
        return Ok(None);
    };
    let identity: TmuxRuntimeIdentity = serde_json::from_value(value.clone())
        .map_err(|_| SessionTerminationFailure::RuntimeIdentityUnavailable)?;
    let current_launch_id = record
        .runtime
        .as_ref()
        .map(|runtime| runtime.launch_id.as_str())
        .filter(|launch_id| !launch_id.is_empty());
    if identity.launch_id.as_deref() != current_launch_id {
        return Ok(None);
    }
    if !valid_tmux_session_id(&identity.session_id)
        || !valid_tmux_pane_id(&identity.pane_id)
        || identity.pane_pid <= 1
        || identity
            .process_group_id
            .is_some_and(|process_group_id| process_group_id <= 1)
        || identity
            .process_session_id
            .is_some_and(|process_session_id| process_session_id <= 1)
        || identity
            .process_session_members
            .iter()
            .any(|member| member.pid <= 1 || member.start_time == 0)
        || identity
            .control_group_members
            .iter()
            .any(|member| member.pid <= 1 || member.start_time == 0)
        || identity
            .control_group
            .as_ref()
            .is_some_and(|control_group| {
                control_group.device == 0
                    || control_group.inode == 0
                    || control_group
                        .boot_id
                        .as_deref()
                        .is_some_and(|boot_id| !valid_linux_boot_id(boot_id))
                    || !valid_tmux_spawn_control_group_path(Path::new(&control_group.path))
            })
    {
        return Err(SessionTerminationFailure::RuntimeIdentityUnavailable);
    }
    Ok(Some(identity))
}

fn persisted_prior_tmux_runtime_identities(
    record: &SessionRecord,
) -> Result<Vec<TmuxRuntimeIdentity>, SessionTerminationFailure> {
    let Some(value) = record.extra.get(DELETE_TMUX_PRIOR_IDENTITIES_KEY) else {
        return Ok(Vec::new());
    };
    let identities: Vec<TmuxRuntimeIdentity> = serde_json::from_value(value.clone())
        .map_err(|_| SessionTerminationFailure::RuntimeIdentityUnavailable)?;
    let current_launch_id = record
        .runtime
        .as_ref()
        .map(|runtime| runtime.launch_id.as_str())
        .filter(|launch_id| !launch_id.is_empty());
    if identities.iter().any(|identity| {
        identity.launch_id.as_deref() != current_launch_id
            || !valid_tmux_session_id(&identity.session_id)
            || !valid_tmux_pane_id(&identity.pane_id)
            || identity.pane_pid <= 1
            || identity
                .process_group_id
                .is_none_or(|process_group_id| process_group_id <= 1)
            || identity
                .process_session_id
                .is_some_and(|process_session_id| process_session_id <= 1)
            || identity
                .process_session_members
                .iter()
                .any(|member| member.pid <= 1 || member.start_time == 0)
            || identity
                .control_group_members
                .iter()
                .any(|member| member.pid <= 1 || member.start_time == 0)
            || identity
                .control_group
                .as_ref()
                .is_some_and(|control_group| {
                    control_group.device == 0
                        || control_group.inode == 0
                        || control_group
                            .boot_id
                            .as_deref()
                            .is_some_and(|boot_id| !valid_linux_boot_id(boot_id))
                        || !valid_tmux_spawn_control_group_path(Path::new(&control_group.path))
                })
    }) {
        return Err(SessionTerminationFailure::RuntimeIdentityUnavailable);
    }
    Ok(identities)
}

fn set_tmux_termination_state(
    record: &mut SessionRecord,
    state: TmuxTerminationState,
) -> Result<(), SessionTerminationFailure> {
    let launch_id = record
        .runtime
        .as_ref()
        .map(|runtime| runtime.launch_id.as_str())
        .filter(|launch_id| !launch_id.is_empty());
    record.extra.insert(
        DELETE_TMUX_TERMINATION_STATE_KEY.to_string(),
        json!({
            "launch_id": launch_id,
            "state": match state {
                TmuxTerminationState::FreezePending { .. } => "freeze-pending",
                TmuxTerminationState::Pending { .. } => "pending",
                TmuxTerminationState::KillConfirmed { .. } => "kill-confirmed",
            },
            "thaw_on_recovery": state.thaw_on_recovery(),
        }),
    );
    Ok(())
}

fn persisted_tmux_termination_state(
    record: &SessionRecord,
) -> Result<Option<TmuxTerminationState>, SessionTerminationFailure> {
    let Some(value) = record.extra.get(DELETE_TMUX_TERMINATION_STATE_KEY) else {
        return Ok(None);
    };
    let launch_id = match value.get("launch_id") {
        Some(Value::Null) => None,
        Some(Value::String(launch_id)) if !launch_id.is_empty() => Some(launch_id.as_str()),
        _ => return Err(SessionTerminationFailure::RuntimeIdentityUnavailable),
    };
    let current_launch_id = record
        .runtime
        .as_ref()
        .map(|runtime| runtime.launch_id.as_str())
        .filter(|launch_id| !launch_id.is_empty());
    if launch_id != current_launch_id {
        return Err(SessionTerminationFailure::RuntimeIdentityUnavailable);
    }
    let thaw_on_recovery = value
        .get("thaw_on_recovery")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    match value.get("state").and_then(Value::as_str) {
        Some("freeze-pending") => Ok(Some(TmuxTerminationState::FreezePending {
            thaw_on_recovery,
        })),
        Some("pending") => Ok(Some(TmuxTerminationState::Pending { thaw_on_recovery })),
        Some("kill-confirmed") => Ok(Some(TmuxTerminationState::KillConfirmed {
            thaw_on_recovery,
        })),
        _ => Err(SessionTerminationFailure::RuntimeIdentityUnavailable),
    }
}

fn recover_interrupted_tmux_termination_locked(
    context: &CliContext,
    record: &mut SessionRecord,
) -> Result<(), SessionTerminationFailure> {
    recover_interrupted_tmux_termination_locked_at_cgroup_root(
        context,
        record,
        Path::new("/sys/fs/cgroup"),
    )
}

fn recover_interrupted_tmux_termination_locked_at_cgroup_root(
    context: &CliContext,
    record: &mut SessionRecord,
    cgroup_root: &Path,
) -> Result<(), SessionTerminationFailure> {
    let Some(state) = persisted_tmux_termination_state(record)? else {
        return Ok(());
    };
    #[cfg(target_os = "linux")]
    let different_boot = {
        let identity = persisted_tmux_runtime_identity(record)?
            .ok_or(SessionTerminationFailure::RuntimeIdentityUnavailable)?;
        if let Some(control_group) = identity.control_group.as_ref() {
            matches!(
                recover_persisted_linux_control_group_at_root(
                    control_group,
                    &identity.control_group_members,
                    state,
                    cgroup_root,
                )?,
                LinuxControlGroupRecovery::DifferentBoot
            )
        } else if state.thaw_on_recovery() {
            return Err(SessionTerminationFailure::RuntimeIdentityUnavailable);
        } else {
            false
        }
    };
    #[cfg(not(target_os = "linux"))]
    let different_boot = {
        let _ = cgroup_root;
        false
    };
    match state {
        TmuxTerminationState::FreezePending { .. } => {
            record.extra.remove(DELETE_TMUX_TERMINATION_STATE_KEY);
        }
        TmuxTerminationState::Pending { .. } if different_boot => {
            set_tmux_termination_state(
                record,
                TmuxTerminationState::KillConfirmed {
                    thaw_on_recovery: false,
                },
            )?;
        }
        TmuxTerminationState::Pending { .. } | TmuxTerminationState::KillConfirmed { .. } => {
            set_tmux_termination_state(record, state.with_thaw_on_recovery(false))?;
        }
    }
    write_session_record(context, record)
        .map_err(|_| SessionTerminationFailure::RuntimeIdentityUnavailable)
}

#[cfg(target_os = "linux")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LinuxControlGroupRecovery {
    CurrentBoot,
    DifferentBoot,
}

#[cfg(target_os = "linux")]
fn recover_persisted_linux_control_group_at_root(
    control_group: &TmuxControlGroupIdentity,
    control_group_members: &[TmuxProcessIdentity],
    state: TmuxTerminationState,
    root: &Path,
) -> Result<LinuxControlGroupRecovery, SessionTerminationFailure> {
    let full_path = linux_control_group_full_path_at_root(Path::new(&control_group.path), root)?;
    let current_boot_id = linux_boot_id()?;
    match control_group.boot_id.as_deref() {
        Some(boot_id) if boot_id == current_boot_id => {}
        Some(_) => return Ok(LinuxControlGroupRecovery::DifferentBoot),
        None if !full_path.exists() => return Ok(LinuxControlGroupRecovery::CurrentBoot),
        None => return Err(SessionTerminationFailure::RuntimeIdentityUnavailable),
    }
    let mut pinned_control_group = match fs::metadata(&full_path) {
        Ok(_) => Some(open_pinned_linux_control_group(control_group, &full_path)?),
        Err(err) if err.kind() == io::ErrorKind::NotFound => None,
        Err(_) => return Err(SessionTerminationFailure::VerificationFailed),
    };
    let pinned_processes = if matches!(state, TmuxTerminationState::KillConfirmed { .. }) {
        control_group_members
            .iter()
            .map(pin_matching_linux_process)
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    if matches!(state, TmuxTerminationState::KillConfirmed { .. }) {
        if let Some(pinned) = pinned_control_group.as_ref() {
            write_control_group_file(&pinned.kill_fd, b"1")?;
        }
        for process in &pinned_processes {
            pidfd_send_signal(&process.pidfd, libc::SIGKILL, true)?;
        }
    }
    if state.thaw_on_recovery()
        && let Some(pinned) = pinned_control_group.as_mut()
    {
        pinned.thaw_on_drop = true;
        if !thaw_pinned_control_group(pinned) {
            return Err(SessionTerminationFailure::VerificationFailed);
        }
        pinned.thaw_on_drop = false;
    }
    Ok(LinuxControlGroupRecovery::CurrentBoot)
}

pub(crate) fn recover_interrupted_tmux_terminations(context: &CliContext) -> Result<(), CliError> {
    recover_interrupted_tmux_terminations_at_cgroup_root(context, Path::new("/sys/fs/cgroup"))
}

fn recover_interrupted_tmux_terminations_at_cgroup_root(
    context: &CliContext,
    cgroup_root: &Path,
) -> Result<(), CliError> {
    let sessions_root = context.state_dir.join("sessions");
    if !sessions_root.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(&sessions_root)
        .map_err(|err| session_io_error("session-recovery-list-failed", &sessions_root, err))?
    {
        let entry = entry
            .map_err(|err| session_io_error("session-recovery-list-failed", &sessions_root, err))?;
        let record_path = entry.path().join("session.json");
        if !entry.path().is_dir() || !record_path.is_file() {
            continue;
        }
        let serialized = fs::read(&record_path).map_err(|err| {
            session_io_error("session-recovery-record-read-failed", &record_path, err)
        })?;
        let has_termination_marker = serde_json::from_slice::<Value>(&serialized)
            .ok()
            .and_then(|value| {
                value
                    .as_object()
                    .map(|object| object.contains_key(DELETE_TMUX_TERMINATION_STATE_KEY))
            })
            .unwrap_or(false);
        if !has_termination_marker {
            continue;
        }
        let id = entry.file_name().to_string_lossy().into_owned();
        let record = load_session_record(context, &id)?;
        let Some(_lock) = try_acquire_session_record_lock(context, &record.id)? else {
            continue;
        };
        let mut record = load_session_record(context, &record.id)?;
        recover_interrupted_tmux_termination_locked_at_cgroup_root(
            context,
            &mut record,
            cgroup_root,
        )
        .map_err(|reason| {
            session_termination_error(&record, reason, SessionTerminationOperation::Delete)
        })?;
    }
    Ok(())
}

fn valid_tmux_session_id(value: &str) -> bool {
    value.strip_prefix('$').is_some_and(|digits| {
        !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
    })
}

fn valid_tmux_pane_id(value: &str) -> bool {
    value.strip_prefix('%').is_some_and(|digits| {
        !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProcessGroupStatus {
    Running,
    Stopped,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CoordinationRuntimeStatus {
    Running,
    Stopped,
    Unknown,
}

pub(crate) struct CoordinationRuntimeEvidence {
    pub(crate) identity_digest: String,
    pub(crate) identity: Value,
    pub(crate) status: CoordinationRuntimeStatus,
}

pub(crate) fn coordination_runtime_evidence(
    record: &SessionRecord,
) -> Result<CoordinationRuntimeEvidence, CliError> {
    let identity = persisted_tmux_runtime_identity(record)
        .map_err(|_| {
            CliError::runtime(
                "coordination-runtime-unverified",
                "persisted runtime identity is unavailable or invalid",
                None,
            )
        })?
        .ok_or_else(|| {
            CliError::runtime(
                "coordination-runtime-unverified",
                "persisted runtime identity is unavailable",
                None,
            )
        })?;
    let bytes = serde_json::to_vec(&identity).map_err(|_| {
        CliError::runtime(
            "coordination-runtime-unverified",
            "persisted runtime identity could not be canonicalized",
            None,
        )
    })?;
    let status = match coordination_process_runtime_status(&identity) {
        ProcessGroupStatus::Running => CoordinationRuntimeStatus::Running,
        ProcessGroupStatus::Stopped => CoordinationRuntimeStatus::Stopped,
        ProcessGroupStatus::Unknown => CoordinationRuntimeStatus::Unknown,
    };
    Ok(CoordinationRuntimeEvidence {
        identity_digest: coordination::digest_bytes(&bytes),
        identity: serde_json::to_value(identity).map_err(|_| {
            CliError::runtime(
                "coordination-runtime-unverified",
                "persisted runtime identity could not be serialized",
                None,
            )
        })?,
        status,
    })
}

pub(crate) fn coordination_runtime_status_for_identity(value: &Value) -> CoordinationRuntimeStatus {
    let Ok(identity) = serde_json::from_value::<TmuxRuntimeIdentity>(value.clone()) else {
        return CoordinationRuntimeStatus::Unknown;
    };
    match coordination_process_runtime_status(&identity) {
        ProcessGroupStatus::Running => CoordinationRuntimeStatus::Running,
        ProcessGroupStatus::Stopped => CoordinationRuntimeStatus::Stopped,
        ProcessGroupStatus::Unknown => CoordinationRuntimeStatus::Unknown,
    }
}

fn process_group_status(process_group_id: libc::pid_t) -> ProcessGroupStatus {
    if unsafe { libc::kill(-process_group_id, 0) } == 0 {
        return ProcessGroupStatus::Running;
    }
    match io::Error::last_os_error().raw_os_error() {
        Some(libc::ESRCH) => ProcessGroupStatus::Stopped,
        Some(libc::EPERM) => ProcessGroupStatus::Running,
        _ => ProcessGroupStatus::Unknown,
    }
}

#[cfg(target_os = "linux")]
fn coordination_process_runtime_status(identity: &TmuxRuntimeIdentity) -> ProcessGroupStatus {
    let mut evidence = Vec::with_capacity(3);
    if let Some(control_group) = identity.control_group.as_ref() {
        let status = linux_control_group_runtime_status(identity, control_group);
        if status == ProcessGroupStatus::Running {
            return ProcessGroupStatus::Running;
        }
        evidence.push(status);
    }
    if let Some(process_session_id) = identity.process_session_id {
        let status = linux_process_runtime_status(identity, process_session_id);
        if status == ProcessGroupStatus::Running {
            return ProcessGroupStatus::Running;
        }
        evidence.push(status);
    }
    if let Some(process_group_id) = identity.process_group_id {
        let status = process_group_status(process_group_id);
        if status == ProcessGroupStatus::Running {
            return ProcessGroupStatus::Running;
        }
        evidence.push(status);
    }
    combine_runtime_status_evidence(&evidence)
}

#[cfg(not(target_os = "linux"))]
fn coordination_process_runtime_status(identity: &TmuxRuntimeIdentity) -> ProcessGroupStatus {
    identity
        .process_group_id
        .map(process_group_status)
        .map(conservative_coordination_process_group_status)
        .unwrap_or(ProcessGroupStatus::Unknown)
}

#[cfg(any(not(target_os = "linux"), test))]
fn conservative_coordination_process_group_status(
    status: ProcessGroupStatus,
) -> ProcessGroupStatus {
    match status {
        ProcessGroupStatus::Running => ProcessGroupStatus::Running,
        ProcessGroupStatus::Stopped | ProcessGroupStatus::Unknown => ProcessGroupStatus::Unknown,
    }
}

#[cfg(target_os = "linux")]
fn combine_runtime_status_evidence(evidence: &[ProcessGroupStatus]) -> ProcessGroupStatus {
    if evidence.contains(&ProcessGroupStatus::Running) {
        ProcessGroupStatus::Running
    } else if evidence.is_empty() || evidence.contains(&ProcessGroupStatus::Unknown) {
        ProcessGroupStatus::Unknown
    } else {
        ProcessGroupStatus::Stopped
    }
}

fn process_runtime_status(identity: &TmuxRuntimeIdentity) -> ProcessGroupStatus {
    #[cfg(target_os = "linux")]
    if let Some(control_group) = identity.control_group.as_ref() {
        return linux_control_group_runtime_status(identity, control_group);
    }
    #[cfg(target_os = "linux")]
    if let Some(process_session_id) = identity.process_session_id
        && !identity.process_session_members.is_empty()
    {
        return linux_process_runtime_status(identity, process_session_id);
    }
    identity
        .process_group_id
        .map(process_group_status)
        .unwrap_or(ProcessGroupStatus::Stopped)
}

fn verify_stopped_process_runtime(
    identity: &TmuxRuntimeIdentity,
    verify_timeout: Duration,
) -> Result<(), SessionTerminationFailure> {
    let started_at = Instant::now();
    loop {
        match process_runtime_status(identity) {
            ProcessGroupStatus::Stopped => return Ok(()),
            ProcessGroupStatus::Running if started_at.elapsed() >= verify_timeout => {
                return Err(SessionTerminationFailure::ProcessStillRunning);
            }
            ProcessGroupStatus::Unknown => {
                return Err(SessionTerminationFailure::VerificationFailed);
            }
            ProcessGroupStatus::Running => {}
        }
        thread::sleep(
            DELETE_TERMINATION_VERIFY_POLL_INTERVAL
                .min(verify_timeout.saturating_sub(started_at.elapsed())),
        );
    }
}

fn terminate_captured_process_runtime(
    identity: &TmuxRuntimeIdentity,
    pinned_runtime: &mut PinnedProcessRuntime,
    verify_timeout: Duration,
) -> Result<(), SessionTerminationFailure> {
    #[cfg(target_os = "linux")]
    {
        let process_session_id = identity
            .process_session_id
            .ok_or(SessionTerminationFailure::RuntimeIdentityUnavailable)?;
        if process_session_id <= 1 || process_session_id == unsafe { libc::getsid(0) } {
            return Err(SessionTerminationFailure::RuntimeIdentityUnavailable);
        }
        let thaw_after_kill = pinned_runtime
            .control_group
            .as_mut()
            .is_some_and(|control_group| {
                let thaw_after_kill = control_group.thaw_on_drop;
                control_group.thaw_on_drop = false;
                thaw_after_kill
            });
        kill_pinned_control_group(pinned_runtime)?;
        signal_pinned_process_runtime(pinned_runtime, libc::SIGKILL)?;
        if thaw_after_kill {
            let control_group = pinned_runtime
                .control_group
                .as_ref()
                .ok_or(SessionTerminationFailure::RuntimeIdentityUnavailable)?;
            if !thaw_pinned_control_group(control_group) {
                return Err(SessionTerminationFailure::VerificationFailed);
            }
            signal_pinned_process_runtime(pinned_runtime, libc::SIGKILL)?;
        }
        verify_stopped_pinned_process_runtime(pinned_runtime, verify_timeout)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pinned_runtime;
        verify_stopped_process_runtime(identity, verify_timeout)
    }
}

#[derive(Debug)]
struct PinnedProcessRuntime {
    #[cfg(target_os = "linux")]
    processes: Vec<PinnedLinuxProcess>,
    #[cfg(target_os = "linux")]
    control_group: Option<PinnedLinuxControlGroup>,
}

fn release_process_runtime(pinned_runtime: Option<PinnedProcessRuntime>) {
    #[cfg(target_os = "linux")]
    drop(pinned_runtime);
    #[cfg(not(target_os = "linux"))]
    let _ = pinned_runtime;
}

#[cfg(target_os = "linux")]
#[derive(Debug)]
struct PinnedLinuxProcess {
    identity: TmuxProcessIdentity,
    pidfd: OwnedFd,
}

#[cfg(target_os = "linux")]
#[derive(Debug)]
struct PinnedLinuxControlGroup {
    kill_fd: OwnedFd,
    freeze_fd: OwnedFd,
    freeze_state_fd: OwnedFd,
    thaw_fd: OwnedFd,
    events_fd: OwnedFd,
    procs_fd: OwnedFd,
    directory_fd: OwnedFd,
    initially_frozen: bool,
    thaw_on_drop: bool,
}

#[cfg(target_os = "linux")]
impl Drop for PinnedLinuxControlGroup {
    fn drop(&mut self) {
        if self.thaw_on_drop {
            thaw_pinned_control_group(self);
        }
    }
}

fn prepare_process_runtime(
    identity: &TmuxRuntimeIdentity,
) -> Result<PinnedProcessRuntime, SessionTerminationFailure> {
    #[cfg(target_os = "linux")]
    {
        identity
            .process_session_id
            .filter(|session_id| *session_id > 1 && *session_id != unsafe { libc::getsid(0) })
            .ok_or(SessionTerminationFailure::RuntimeIdentityUnavailable)?;
        let control_group_identity = identity
            .control_group
            .as_ref()
            .ok_or(SessionTerminationFailure::RuntimeIdentityUnavailable)?;
        let control_group = identity
            .control_group
            .as_ref()
            .map(|control_group| {
                open_verified_linux_control_group(
                    control_group,
                    identity.pane_pid,
                    &identity.process_session_members,
                )
            })
            .transpose()?
            .ok_or(SessionTerminationFailure::RuntimeIdentityUnavailable)?;
        let mut processes = pin_linux_control_group_processes(
            &control_group,
            control_group_identity,
            identity.pane_pid,
            &identity.process_session_members,
            false,
            DELETE_TERMINATION_VERIFY_TIMEOUT,
        )?;
        for captured in &identity.control_group_members {
            if processes.iter().any(|process| {
                process.identity.pid == captured.pid
                    && process.identity.start_time == captured.start_time
            }) {
                continue;
            }
            if let Some(process) = pin_matching_linux_process(captured)? {
                processes.push(process);
            }
        }
        Ok(PinnedProcessRuntime {
            processes,
            control_group: Some(control_group),
        })
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = identity;
        Ok(PinnedProcessRuntime {})
    }
}

fn freeze_and_pin_process_runtime(
    identity: &TmuxRuntimeIdentity,
    pinned_runtime: &mut PinnedProcessRuntime,
) -> Result<(), SessionTerminationFailure> {
    #[cfg(target_os = "linux")]
    {
        let process_session_id = identity
            .process_session_id
            .filter(|session_id| *session_id > 1 && *session_id != unsafe { libc::getsid(0) })
            .ok_or(SessionTerminationFailure::RuntimeIdentityUnavailable)?;
        let control_group_identity = identity
            .control_group
            .as_ref()
            .ok_or(SessionTerminationFailure::RuntimeIdentityUnavailable)?;
        let control_group = pinned_runtime
            .control_group
            .as_mut()
            .ok_or(SessionTerminationFailure::RuntimeIdentityUnavailable)?;
        freeze_and_revalidate_linux_control_group(
            control_group,
            control_group_identity,
            identity.pane_pid,
            &identity.process_session_members,
        )?;
        if identity.process_session_members.is_empty() {
            let pane = read_linux_process_identity(identity.pane_pid)?
                .filter(|pane| !pane.zombie && pane.session_id == process_session_id)
                .ok_or(SessionTerminationFailure::VerificationFailed)?;
            if pane.pid != identity.pane_pid {
                return Err(SessionTerminationFailure::VerificationFailed);
            }
        } else {
            verify_captured_linux_process_session(
                process_session_id,
                &identity.process_session_members,
            )?;
        }
        let frozen_processes = pin_linux_control_group_processes(
            control_group,
            control_group_identity,
            identity.pane_pid,
            &identity.process_session_members,
            true,
            DELETE_TERMINATION_VERIFY_TIMEOUT,
        )?;
        for process in frozen_processes {
            if !pinned_runtime.processes.iter().any(|pinned| {
                pinned.identity.pid == process.identity.pid
                    && pinned.identity.start_time == process.identity.start_time
            }) {
                pinned_runtime.processes.push(process);
            }
        }
        Ok(())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (identity, pinned_runtime);
        Ok(())
    }
}

fn process_runtime_thaw_on_recovery(pinned_runtime: &PinnedProcessRuntime) -> bool {
    #[cfg(target_os = "linux")]
    {
        pinned_runtime
            .control_group
            .as_ref()
            .is_some_and(|control_group| !control_group.initially_frozen)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pinned_runtime;
        false
    }
}

fn refresh_process_runtime_freeze_ownership(
    pinned_runtime: &mut PinnedProcessRuntime,
) -> Result<(), SessionTerminationFailure> {
    #[cfg(target_os = "linux")]
    {
        let control_group = pinned_runtime
            .control_group
            .as_mut()
            .ok_or(SessionTerminationFailure::RuntimeIdentityUnavailable)?;
        refresh_pinned_control_group_freeze_ownership(control_group)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pinned_runtime;
        Ok(())
    }
}

fn verify_process_runtime_frozen(
    pinned_runtime: &PinnedProcessRuntime,
) -> Result<(), SessionTerminationFailure> {
    #[cfg(target_os = "linux")]
    {
        let control_group = pinned_runtime
            .control_group
            .as_ref()
            .ok_or(SessionTerminationFailure::RuntimeIdentityUnavailable)?;
        let leaf_frozen = read_control_group_value(&control_group.freeze_state_fd)?
            .trim()
            .eq("1");
        let events = read_control_group_events(&control_group.events_fd)?;
        if leaf_frozen && control_group_event(&events, "frozen") == Some("1") {
            Ok(())
        } else {
            Err(SessionTerminationFailure::VerificationFailed)
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pinned_runtime;
        Ok(())
    }
}

fn process_runtime_identities(pinned_runtime: &PinnedProcessRuntime) -> Vec<TmuxProcessIdentity> {
    #[cfg(target_os = "linux")]
    {
        let mut identities = pinned_runtime
            .processes
            .iter()
            .map(|process| process.identity.clone())
            .collect::<Vec<_>>();
        identities.sort_unstable_by_key(|identity| (identity.pid, identity.start_time));
        identities.dedup();
        identities
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pinned_runtime;
        Vec::new()
    }
}

fn thaw_owned_process_runtime(
    pinned_runtime: &mut PinnedProcessRuntime,
) -> Result<(), SessionTerminationFailure> {
    #[cfg(target_os = "linux")]
    if let Some(control_group) = pinned_runtime.control_group.as_mut()
        && control_group.thaw_on_drop
    {
        if !thaw_pinned_control_group(control_group) {
            return Err(SessionTerminationFailure::VerificationFailed);
        }
        control_group.thaw_on_drop = false;
    }
    #[cfg(not(target_os = "linux"))]
    let _ = pinned_runtime;
    Ok(())
}

#[cfg(target_os = "linux")]
#[derive(Clone, Copy)]
struct LinuxProcessIdentity {
    pid: libc::pid_t,
    session_id: libc::pid_t,
    start_time: u64,
    zombie: bool,
}

#[cfg(target_os = "linux")]
fn read_linux_process_identity(
    pid: libc::pid_t,
) -> Result<Option<LinuxProcessIdentity>, SessionTerminationFailure> {
    let stat = match fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => stat,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(SessionTerminationFailure::VerificationFailed),
    };
    let fields: Vec<&str> = stat
        .rsplit_once(") ")
        .map(|(_, fields)| fields.split_whitespace().collect())
        .ok_or(SessionTerminationFailure::VerificationFailed)?;
    if fields.len() <= 19 {
        return Err(SessionTerminationFailure::VerificationFailed);
    }
    let session_id = fields[3]
        .parse()
        .map_err(|_| SessionTerminationFailure::VerificationFailed)?;
    let start_time = fields[19]
        .parse()
        .map_err(|_| SessionTerminationFailure::VerificationFailed)?;
    Ok(Some(LinuxProcessIdentity {
        pid,
        session_id,
        start_time,
        zombie: fields[0] == "Z",
    }))
}

#[cfg(target_os = "linux")]
fn linux_process_session_members(
    process_session_id: libc::pid_t,
) -> Result<Vec<LinuxProcessIdentity>, SessionTerminationFailure> {
    let mut members = Vec::new();
    let entries =
        fs::read_dir("/proc").map_err(|_| SessionTerminationFailure::VerificationFailed)?;
    for entry in entries {
        let entry = entry.map_err(|_| SessionTerminationFailure::VerificationFailed)?;
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<libc::pid_t>().ok())
            .filter(|pid| *pid > 1)
        else {
            continue;
        };
        if let Some(identity) = read_linux_process_identity(pid)?
            && identity.session_id == process_session_id
            && !identity.zombie
        {
            members.push(identity);
        }
    }
    members.sort_unstable_by_key(|identity| identity.pid);
    Ok(members)
}

#[cfg(target_os = "linux")]
fn verify_captured_linux_process_session(
    process_session_id: libc::pid_t,
    captured_members: &[TmuxProcessIdentity],
) -> Result<(), SessionTerminationFailure> {
    let current_members = linux_process_session_members(process_session_id)?;
    if current_members.len() != captured_members.len()
        || current_members.iter().any(|current| {
            !captured_members.iter().any(|captured| {
                captured.pid == current.pid && captured.start_time == current.start_time
            })
        })
    {
        return Err(SessionTerminationFailure::VerificationFailed);
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn linux_process_runtime_status(
    identity: &TmuxRuntimeIdentity,
    process_session_id: libc::pid_t,
) -> ProcessGroupStatus {
    for captured in &identity.process_session_members {
        match read_linux_process_identity(captured.pid) {
            Ok(Some(current)) if current.start_time == captured.start_time && !current.zombie => {
                return ProcessGroupStatus::Running;
            }
            Ok(Some(_)) | Ok(None) => {}
            Err(_) => return ProcessGroupStatus::Unknown,
        }
    }
    match linux_process_session_members(process_session_id) {
        Ok(members) if members.is_empty() => ProcessGroupStatus::Stopped,
        Ok(_) => ProcessGroupStatus::Running,
        Err(_) => ProcessGroupStatus::Unknown,
    }
}

#[cfg(target_os = "linux")]
fn linux_control_group_runtime_status(
    identity: &TmuxRuntimeIdentity,
    control_group: &TmuxControlGroupIdentity,
) -> ProcessGroupStatus {
    linux_control_group_runtime_status_at_root(identity, control_group, Path::new("/sys/fs/cgroup"))
}

#[cfg(target_os = "linux")]
fn linux_control_group_runtime_status_at_root(
    identity: &TmuxRuntimeIdentity,
    control_group: &TmuxControlGroupIdentity,
    root: &Path,
) -> ProcessGroupStatus {
    if let Some(boot_id) = control_group.boot_id.as_deref() {
        match linux_boot_id() {
            Ok(current_boot_id) if current_boot_id == boot_id => {}
            Ok(_) => return ProcessGroupStatus::Stopped,
            Err(_) => return ProcessGroupStatus::Unknown,
        }
    }
    for captured in identity
        .process_session_members
        .iter()
        .chain(&identity.control_group_members)
    {
        match read_linux_process_identity(captured.pid) {
            Ok(Some(current)) if current.start_time == captured.start_time && !current.zombie => {
                return ProcessGroupStatus::Running;
            }
            Ok(Some(_)) | Ok(None) => {}
            Err(_) => return ProcessGroupStatus::Unknown,
        }
    }
    let full_path =
        match linux_control_group_full_path_at_root(Path::new(&control_group.path), root) {
            Ok(path) => path,
            Err(_) => return ProcessGroupStatus::Unknown,
        };
    let metadata = match fs::metadata(&full_path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return ProcessGroupStatus::Stopped,
        Err(_) => return ProcessGroupStatus::Unknown,
    };
    if !metadata.is_dir() {
        return ProcessGroupStatus::Unknown;
    }
    if metadata.dev() != control_group.device || metadata.ino() != control_group.inode {
        return ProcessGroupStatus::Unknown;
    }
    let events = match fs::read_to_string(full_path.join("cgroup.events")) {
        Ok(events) => events,
        Err(_) => return ProcessGroupStatus::Unknown,
    };
    match events.lines().find_map(|line| {
        let (name, value) = line.split_once(' ')?;
        (name == "populated").then_some(value)
    }) {
        Some("0") => ProcessGroupStatus::Stopped,
        Some("1") => ProcessGroupStatus::Running,
        _ => ProcessGroupStatus::Unknown,
    }
}

#[cfg(target_os = "linux")]
fn open_verified_linux_control_group(
    control_group: &TmuxControlGroupIdentity,
    pane_pid: libc::pid_t,
    captured_members: &[TmuxProcessIdentity],
) -> Result<PinnedLinuxControlGroup, SessionTerminationFailure> {
    let path = Path::new(&control_group.path);
    if !valid_tmux_spawn_control_group_path(path) {
        return Err(SessionTerminationFailure::RuntimeIdentityUnavailable);
    }
    if linux_process_control_group_path(unsafe { libc::getpid() })?.as_deref() == Some(path) {
        return Err(SessionTerminationFailure::RuntimeIdentityUnavailable);
    }
    if linux_process_control_group(pane_pid)?.as_ref() != Some(control_group) {
        return Err(SessionTerminationFailure::VerificationFailed);
    }
    for member in captured_members {
        if linux_process_control_group(member.pid)?.as_ref() != Some(control_group) {
            return Err(SessionTerminationFailure::VerificationFailed);
        }
    }

    let full_path = linux_control_group_full_path(path)?;
    open_pinned_linux_control_group(control_group, &full_path)
}

#[cfg(target_os = "linux")]
fn freeze_and_revalidate_linux_control_group(
    pinned: &mut PinnedLinuxControlGroup,
    control_group: &TmuxControlGroupIdentity,
    pane_pid: libc::pid_t,
    captured_members: &[TmuxProcessIdentity],
) -> Result<(), SessionTerminationFailure> {
    freeze_pinned_control_group(pinned, DELETE_TERMINATION_VERIFY_TIMEOUT)?;
    if linux_process_control_group(pane_pid)?.as_ref() != Some(control_group) {
        return Err(SessionTerminationFailure::VerificationFailed);
    }
    for member in captured_members {
        if linux_process_control_group(member.pid)?.as_ref() != Some(control_group) {
            return Err(SessionTerminationFailure::VerificationFailed);
        }
    }
    ensure_linux_control_group_is_leaf(&pinned.directory_fd)?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn open_pinned_linux_control_group(
    control_group: &TmuxControlGroupIdentity,
    full_path: &Path,
) -> Result<PinnedLinuxControlGroup, SessionTerminationFailure> {
    let directory =
        fs::File::open(full_path).map_err(|_| SessionTerminationFailure::VerificationFailed)?;
    let metadata = directory
        .metadata()
        .map_err(|_| SessionTerminationFailure::VerificationFailed)?;
    if !metadata.is_dir()
        || metadata.dev() != control_group.device
        || metadata.ino() != control_group.inode
    {
        return Err(SessionTerminationFailure::VerificationFailed);
    }
    let raw_kill_fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            c"cgroup.kill".as_ptr(),
            libc::O_WRONLY | libc::O_CLOEXEC,
        )
    };
    if raw_kill_fd < 0 {
        return Err(SessionTerminationFailure::RuntimeIdentityUnavailable);
    }
    let kill_fd = unsafe { OwnedFd::from_raw_fd(raw_kill_fd) };
    let raw_freeze_fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            c"cgroup.freeze".as_ptr(),
            libc::O_WRONLY | libc::O_CLOEXEC,
        )
    };
    if raw_freeze_fd < 0 {
        return Err(SessionTerminationFailure::RuntimeIdentityUnavailable);
    }
    let freeze_fd = unsafe { OwnedFd::from_raw_fd(raw_freeze_fd) };
    let raw_freeze_state_fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            c"cgroup.freeze".as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC,
        )
    };
    if raw_freeze_state_fd < 0 {
        return Err(SessionTerminationFailure::RuntimeIdentityUnavailable);
    }
    let freeze_state_fd = unsafe { OwnedFd::from_raw_fd(raw_freeze_state_fd) };
    let raw_thaw_fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            c"cgroup.freeze".as_ptr(),
            libc::O_WRONLY | libc::O_CLOEXEC,
        )
    };
    if raw_thaw_fd < 0 {
        return Err(SessionTerminationFailure::RuntimeIdentityUnavailable);
    }
    let thaw_fd = unsafe { OwnedFd::from_raw_fd(raw_thaw_fd) };
    let raw_events_fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            c"cgroup.events".as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC,
        )
    };
    if raw_events_fd < 0 {
        return Err(SessionTerminationFailure::RuntimeIdentityUnavailable);
    }
    let events_fd = unsafe { OwnedFd::from_raw_fd(raw_events_fd) };
    let raw_procs_fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            c"cgroup.procs".as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC,
        )
    };
    if raw_procs_fd < 0 {
        return Err(SessionTerminationFailure::RuntimeIdentityUnavailable);
    }
    let procs_fd = unsafe { OwnedFd::from_raw_fd(raw_procs_fd) };
    let initially_frozen = match read_control_group_value(&freeze_state_fd)?.trim() {
        "0" => false,
        "1" => true,
        _ => return Err(SessionTerminationFailure::VerificationFailed),
    };
    let pinned = PinnedLinuxControlGroup {
        kill_fd,
        freeze_fd,
        freeze_state_fd,
        thaw_fd,
        events_fd,
        procs_fd,
        directory_fd: directory.into(),
        initially_frozen,
        thaw_on_drop: false,
    };
    Ok(pinned)
}

#[cfg(target_os = "linux")]
fn ensure_linux_control_group_is_leaf(
    directory_fd: &OwnedFd,
) -> Result<(), SessionTerminationFailure> {
    let pinned_path = PathBuf::from(format!("/proc/self/fd/{}", directory_fd.as_raw_fd()));
    for entry in
        fs::read_dir(pinned_path).map_err(|_| SessionTerminationFailure::VerificationFailed)?
    {
        let entry = entry.map_err(|_| SessionTerminationFailure::VerificationFailed)?;
        if entry
            .file_type()
            .map_err(|_| SessionTerminationFailure::VerificationFailed)?
            .is_dir()
        {
            return Err(SessionTerminationFailure::RuntimeIdentityUnavailable);
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn read_linux_control_group_pids(
    procs_fd: &OwnedFd,
) -> Result<Vec<libc::pid_t>, SessionTerminationFailure> {
    if unsafe { libc::lseek(procs_fd.as_raw_fd(), 0, libc::SEEK_SET) } < 0 {
        return Err(SessionTerminationFailure::VerificationFailed);
    }
    let mut contents = Vec::new();
    loop {
        let mut buffer = [0_u8; 4096];
        let length = unsafe {
            libc::read(
                procs_fd.as_raw_fd(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
            )
        };
        if length < 0 {
            return Err(SessionTerminationFailure::VerificationFailed);
        }
        if length == 0 {
            break;
        }
        contents.extend_from_slice(&buffer[..length as usize]);
        if contents.len() > 1024 * 1024 {
            return Err(SessionTerminationFailure::VerificationFailed);
        }
    }
    let contents = std::str::from_utf8(&contents)
        .map_err(|_| SessionTerminationFailure::VerificationFailed)?;
    let mut pids = contents
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| {
            line.parse::<libc::pid_t>()
                .map_err(|_| SessionTerminationFailure::VerificationFailed)
        })
        .collect::<Result<Vec<_>, _>>()?;
    if pids
        .iter()
        .any(|pid| *pid <= 1 || *pid == unsafe { libc::getpid() })
    {
        return Err(SessionTerminationFailure::RuntimeIdentityUnavailable);
    }
    pids.sort_unstable();
    pids.dedup();
    Ok(pids)
}

#[cfg(target_os = "linux")]
fn pin_linux_control_group_processes(
    pinned: &PinnedLinuxControlGroup,
    control_group: &TmuxControlGroupIdentity,
    pane_pid: libc::pid_t,
    captured_members: &[TmuxProcessIdentity],
    stabilize_membership: bool,
    timeout: Duration,
) -> Result<Vec<PinnedLinuxProcess>, SessionTerminationFailure> {
    let deadline = Instant::now() + timeout;
    // The user manager can move descendants while the scope is being collected, so
    // cgroup.procs alone is not a stable membership snapshot. Scan /proc before and
    // after pidfd pinning, with one shared deadline to bound the frozen interval.
    let mut pids = linux_control_group_snapshot_pids(pinned, control_group, deadline)?;
    if stabilize_membership {
        pids = stabilize_linux_control_group_pids(
            pids,
            DELETE_TERMINATION_PROBE_TIMEOUT,
            DELETE_TERMINATION_VERIFY_POLL_INTERVAL,
            deadline,
            |snapshot_deadline| {
                linux_control_group_snapshot_pids(pinned, control_group, snapshot_deadline)
            },
        )?;
    }
    pids.sort_unstable();
    pids.dedup();
    if pids.is_empty()
        || !pids.contains(&pane_pid)
        || captured_members
            .iter()
            .any(|member| !pids.contains(&member.pid))
    {
        return Err(SessionTerminationFailure::VerificationFailed);
    }
    let processes = pids
        .into_iter()
        .map(|pid| pin_linux_control_group_process(pid, control_group))
        .collect::<Result<Vec<_>, _>>()?;
    let revalidated = linux_control_group_member_pids(control_group, deadline)?;
    if revalidated
        .iter()
        .any(|pid| !processes.iter().any(|process| process.identity.pid == *pid))
    {
        return Err(SessionTerminationFailure::VerificationFailed);
    }
    Ok(processes)
}

#[cfg(target_os = "linux")]
fn stabilize_linux_control_group_pids<F>(
    mut pids: Vec<libc::pid_t>,
    observation_duration: Duration,
    poll_interval: Duration,
    deadline: Instant,
    mut snapshot: F,
) -> Result<Vec<libc::pid_t>, SessionTerminationFailure>
where
    F: FnMut(Instant) -> Result<Vec<libc::pid_t>, SessionTerminationFailure>,
{
    let observation_started = Instant::now();
    loop {
        if Instant::now() >= deadline {
            return Err(SessionTerminationFailure::VerificationFailed);
        }
        thread::sleep(poll_interval.min(deadline.saturating_duration_since(Instant::now())));
        pids.extend(snapshot(deadline)?);
        pids.sort_unstable();
        pids.dedup();
        if observation_started.elapsed() >= observation_duration {
            return Ok(pids);
        }
    }
}

#[cfg(target_os = "linux")]
fn linux_control_group_snapshot_pids(
    pinned: &PinnedLinuxControlGroup,
    control_group: &TmuxControlGroupIdentity,
    deadline: Instant,
) -> Result<Vec<libc::pid_t>, SessionTerminationFailure> {
    let mut pids = read_linux_control_group_pids(&pinned.procs_fd)?;
    pids.extend(linux_control_group_member_pids(control_group, deadline)?);
    pids.sort_unstable();
    pids.dedup();
    Ok(pids)
}

#[cfg(target_os = "linux")]
fn linux_control_group_member_pids(
    control_group: &TmuxControlGroupIdentity,
    deadline: Instant,
) -> Result<Vec<libc::pid_t>, SessionTerminationFailure> {
    let mut pids = Vec::new();
    for entry in fs::read_dir("/proc").map_err(|_| SessionTerminationFailure::VerificationFailed)? {
        if Instant::now() >= deadline {
            return Err(SessionTerminationFailure::VerificationFailed);
        }
        let entry = entry.map_err(|_| SessionTerminationFailure::VerificationFailed)?;
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<libc::pid_t>().ok())
            .filter(|pid| *pid > 1 && *pid != unsafe { libc::getpid() })
        else {
            continue;
        };
        let Some(identity) = read_linux_process_identity(pid)? else {
            continue;
        };
        if identity.zombie
            || linux_process_control_group_path(pid)?.as_deref()
                != Some(Path::new(&control_group.path))
        {
            continue;
        }
        if linux_process_control_group(pid)?.as_ref() != Some(control_group) {
            return Err(SessionTerminationFailure::VerificationFailed);
        }
        pids.push(pid);
    }
    pids.sort_unstable();
    pids.dedup();
    Ok(pids)
}

#[cfg(target_os = "linux")]
fn pin_linux_control_group_process(
    pid: libc::pid_t,
    control_group: &TmuxControlGroupIdentity,
) -> Result<PinnedLinuxProcess, SessionTerminationFailure> {
    let current = read_linux_process_identity(pid)?
        .filter(|current| !current.zombie)
        .ok_or(SessionTerminationFailure::VerificationFailed)?;
    if linux_process_control_group(pid)?.as_ref() != Some(control_group) {
        return Err(SessionTerminationFailure::VerificationFailed);
    }
    let raw_fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
    if raw_fd < 0 {
        return Err(SessionTerminationFailure::VerificationFailed);
    }
    let pidfd = unsafe { OwnedFd::from_raw_fd(raw_fd as libc::c_int) };
    let revalidated = read_linux_process_identity(pid)?
        .filter(|identity| identity.start_time == current.start_time && !identity.zombie)
        .ok_or(SessionTerminationFailure::VerificationFailed)?;
    if revalidated.pid != pid || linux_process_control_group(pid)?.as_ref() != Some(control_group) {
        return Err(SessionTerminationFailure::VerificationFailed);
    }
    pidfd_send_signal(&pidfd, 0, false)?;
    Ok(PinnedLinuxProcess {
        identity: TmuxProcessIdentity {
            pid,
            start_time: current.start_time,
        },
        pidfd,
    })
}

#[cfg(target_os = "linux")]
fn pin_matching_linux_process(
    captured: &TmuxProcessIdentity,
) -> Result<Option<PinnedLinuxProcess>, SessionTerminationFailure> {
    if captured.pid <= 1 || captured.pid == unsafe { libc::getpid() } || captured.start_time == 0 {
        return Err(SessionTerminationFailure::RuntimeIdentityUnavailable);
    }
    let Some(current) = read_linux_process_identity(captured.pid)? else {
        return Ok(None);
    };
    if current.start_time != captured.start_time || current.zombie {
        return Ok(None);
    }
    let raw_fd = unsafe { libc::syscall(libc::SYS_pidfd_open, captured.pid, 0) };
    if raw_fd < 0 {
        return match io::Error::last_os_error().raw_os_error() {
            Some(libc::ESRCH) => Ok(None),
            _ => Err(SessionTerminationFailure::VerificationFailed),
        };
    }
    let pidfd = unsafe { OwnedFd::from_raw_fd(raw_fd as libc::c_int) };
    let Some(revalidated) = read_linux_process_identity(captured.pid)? else {
        return Ok(None);
    };
    if revalidated.start_time != captured.start_time || revalidated.zombie {
        return Ok(None);
    }
    match pidfd_send_signal(&pidfd, 0, true) {
        Ok(()) => Ok(Some(PinnedLinuxProcess {
            identity: captured.clone(),
            pidfd,
        })),
        Err(reason) => Err(reason),
    }
}

#[cfg(all(target_os = "linux", test))]
fn pin_linux_process(
    captured: &TmuxProcessIdentity,
    process_session_id: libc::pid_t,
) -> Result<PinnedLinuxProcess, SessionTerminationFailure> {
    if captured.pid <= 1 || captured.pid == unsafe { libc::getpid() } {
        return Err(SessionTerminationFailure::RuntimeIdentityUnavailable);
    }
    let raw_fd = unsafe { libc::syscall(libc::SYS_pidfd_open, captured.pid, 0) };
    if raw_fd < 0 {
        return Err(SessionTerminationFailure::VerificationFailed);
    }
    let pidfd = unsafe { OwnedFd::from_raw_fd(raw_fd as libc::c_int) };
    let current = read_linux_process_identity(captured.pid)?
        .filter(|current| {
            current.session_id == process_session_id
                && current.start_time == captured.start_time
                && !current.zombie
        })
        .ok_or(SessionTerminationFailure::VerificationFailed)?;
    if current.pid != captured.pid {
        return Err(SessionTerminationFailure::VerificationFailed);
    }
    pidfd_send_signal(&pidfd, 0, false)?;
    Ok(PinnedLinuxProcess {
        identity: captured.clone(),
        pidfd,
    })
}

#[cfg(target_os = "linux")]
fn signal_pinned_process_runtime(
    pinned_runtime: &PinnedProcessRuntime,
    signal: libc::c_int,
) -> Result<(), SessionTerminationFailure> {
    if pinned_runtime.processes.is_empty() {
        return Err(SessionTerminationFailure::RuntimeIdentityUnavailable);
    }
    for process in &pinned_runtime.processes {
        if process.identity.pid <= 1 {
            return Err(SessionTerminationFailure::RuntimeIdentityUnavailable);
        }
        pidfd_send_signal(&process.pidfd, signal, true)?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn verify_stopped_pinned_process_runtime(
    pinned_runtime: &PinnedProcessRuntime,
    timeout: Duration,
) -> Result<(), SessionTerminationFailure> {
    let control_group = pinned_runtime
        .control_group
        .as_ref()
        .ok_or(SessionTerminationFailure::RuntimeIdentityUnavailable)?;
    let started_at = Instant::now();
    loop {
        let mut any_running = false;
        for process in &pinned_runtime.processes {
            let mut poll_fd = libc::pollfd {
                fd: process.pidfd.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let poll_result = unsafe { libc::poll(&mut poll_fd, 1, 0) };
            if poll_result < 0 {
                return Err(SessionTerminationFailure::VerificationFailed);
            }
            if poll_result == 1 && poll_fd.revents & libc::POLLIN != 0 {
                continue;
            }
            let result = unsafe {
                libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    process.pidfd.as_raw_fd(),
                    0,
                    std::ptr::null::<libc::siginfo_t>(),
                    0,
                )
            };
            if result == 0 {
                any_running = true;
            } else if io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH) {
                return Err(SessionTerminationFailure::VerificationFailed);
            }
        }
        let populated = match read_control_group_events(&control_group.events_fd) {
            Ok(events) => control_group_event(&events, "populated")
                .filter(|value| matches!(*value, "0" | "1"))
                .map(ToOwned::to_owned),
            Err(_)
                if pinned_control_group_was_removed(&control_group.directory_fd)
                    .unwrap_or(false) =>
            {
                Some("0".to_string())
            }
            Err(_) => None,
        };
        if !any_running && populated.as_deref() == Some("0") {
            return Ok(());
        }
        if started_at.elapsed() >= timeout {
            return Err(if any_running || populated.as_deref() == Some("1") {
                SessionTerminationFailure::ProcessStillRunning
            } else {
                SessionTerminationFailure::VerificationFailed
            });
        }
        thread::sleep(
            DELETE_TERMINATION_VERIFY_POLL_INTERVAL
                .min(timeout.saturating_sub(started_at.elapsed())),
        );
    }
}

#[cfg(target_os = "linux")]
fn pinned_control_group_was_removed(
    directory_fd: &OwnedFd,
) -> Result<bool, SessionTerminationFailure> {
    let pinned_path = PathBuf::from(format!("/proc/self/fd/{}", directory_fd.as_raw_fd()));
    let target =
        fs::read_link(&pinned_path).map_err(|_| SessionTerminationFailure::VerificationFailed)?;
    if target.to_string_lossy().ends_with(" (deleted)") {
        return Ok(true);
    }
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    if unsafe { libc::fstat(directory_fd.as_raw_fd(), stat.as_mut_ptr()) } != 0 {
        return Err(SessionTerminationFailure::VerificationFailed);
    }
    let stat = unsafe { stat.assume_init() };
    Ok(stat.st_nlink == 0)
}

#[cfg(target_os = "linux")]
fn thaw_pinned_control_group(control_group: &PinnedLinuxControlGroup) -> bool {
    let started_at = Instant::now();
    loop {
        let _ = write_control_group_file(&control_group.thaw_fd, b"0");
        let thawed = read_control_group_events(&control_group.events_fd)
            .ok()
            .and_then(|events| control_group_event(&events, "frozen").map(ToOwned::to_owned))
            .as_deref()
            == Some("0");
        if thawed {
            return true;
        }
        if pinned_control_group_was_removed(&control_group.directory_fd).unwrap_or(false)
            || started_at.elapsed() >= DELETE_TERMINATION_VERIFY_TIMEOUT
        {
            return pinned_control_group_was_removed(&control_group.directory_fd).unwrap_or(false);
        }
        thread::sleep(DELETE_TERMINATION_VERIFY_POLL_INTERVAL);
    }
}

#[cfg(target_os = "linux")]
fn kill_pinned_control_group(
    pinned_runtime: &PinnedProcessRuntime,
) -> Result<(), SessionTerminationFailure> {
    let Some(control_group) = pinned_runtime.control_group.as_ref() else {
        return Err(SessionTerminationFailure::RuntimeIdentityUnavailable);
    };
    write_control_group_file(&control_group.kill_fd, b"1")
}

#[cfg(target_os = "linux")]
fn freeze_pinned_control_group(
    control_group: &mut PinnedLinuxControlGroup,
    timeout: Duration,
) -> Result<(), SessionTerminationFailure> {
    if !control_group.initially_frozen {
        control_group.thaw_on_drop = true;
    }
    write_control_group_file(&control_group.freeze_fd, b"1")?;
    let started_at = Instant::now();
    loop {
        let events = read_control_group_events(&control_group.events_fd)?;
        if control_group_event(&events, "frozen") == Some("1") {
            return Ok(());
        }
        if started_at.elapsed() >= timeout {
            return Err(SessionTerminationFailure::VerificationFailed);
        }
        thread::sleep(
            DELETE_TERMINATION_VERIFY_POLL_INTERVAL
                .min(timeout.saturating_sub(started_at.elapsed())),
        );
    }
}

#[cfg(target_os = "linux")]
fn refresh_pinned_control_group_freeze_ownership(
    control_group: &mut PinnedLinuxControlGroup,
) -> Result<(), SessionTerminationFailure> {
    control_group.initially_frozen =
        match read_control_group_value(&control_group.freeze_state_fd)?.trim() {
            "0" => false,
            "1" => true,
            _ => return Err(SessionTerminationFailure::VerificationFailed),
        };
    if control_group.initially_frozen {
        control_group.thaw_on_drop = false;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn write_control_group_file(fd: &OwnedFd, value: &[u8]) -> Result<(), SessionTerminationFailure> {
    if unsafe { libc::lseek(fd.as_raw_fd(), 0, libc::SEEK_SET) } < 0 {
        return Err(SessionTerminationFailure::VerificationFailed);
    }
    let written = unsafe { libc::write(fd.as_raw_fd(), value.as_ptr().cast(), value.len()) };
    if written == value.len() as isize {
        Ok(())
    } else {
        Err(SessionTerminationFailure::VerificationFailed)
    }
}

#[cfg(target_os = "linux")]
fn read_control_group_events(fd: &OwnedFd) -> Result<String, SessionTerminationFailure> {
    read_control_group_value(fd)
}

#[cfg(target_os = "linux")]
fn read_control_group_value(fd: &OwnedFd) -> Result<String, SessionTerminationFailure> {
    if unsafe { libc::lseek(fd.as_raw_fd(), 0, libc::SEEK_SET) } < 0 {
        return Err(SessionTerminationFailure::VerificationFailed);
    }
    let mut buffer = [0_u8; 256];
    let length = unsafe { libc::read(fd.as_raw_fd(), buffer.as_mut_ptr().cast(), buffer.len()) };
    if length < 0 {
        return Err(SessionTerminationFailure::VerificationFailed);
    }
    std::str::from_utf8(&buffer[..length as usize])
        .map(ToOwned::to_owned)
        .map_err(|_| SessionTerminationFailure::VerificationFailed)
}

#[cfg(target_os = "linux")]
fn control_group_event<'a>(events: &'a str, name: &str) -> Option<&'a str> {
    events.lines().find_map(|line| {
        let (event_name, value) = line.split_once(' ')?;
        (event_name == name).then_some(value)
    })
}

#[cfg(target_os = "linux")]
fn pidfd_send_signal(
    pidfd: &OwnedFd,
    signal: libc::c_int,
    allow_exited: bool,
) -> Result<(), SessionTerminationFailure> {
    let result = unsafe {
        libc::syscall(
            libc::SYS_pidfd_send_signal,
            pidfd.as_raw_fd(),
            signal,
            std::ptr::null::<libc::siginfo_t>(),
            0,
        )
    };
    if result == 0 {
        return Ok(());
    }
    match io::Error::last_os_error().raw_os_error() {
        Some(libc::ESRCH) if allow_exited => Ok(()),
        _ => Err(SessionTerminationFailure::VerificationFailed),
    }
}

fn verify_stopped_process_runtimes(
    identities: &[TmuxRuntimeIdentity],
    verify_timeout: Duration,
) -> Result<(), SessionTerminationFailure> {
    let started_at = Instant::now();
    for identity in identities {
        let remaining = verify_timeout.saturating_sub(started_at.elapsed());
        if remaining.is_zero() {
            return Err(SessionTerminationFailure::VerificationFailed);
        }
        verify_stopped_process_runtime(identity, remaining)?;
    }
    Ok(())
}

fn verify_stopped_tmux_runtime(
    tmux_bin: &Path,
    identity: &TmuxRuntimeIdentity,
    verify_timeout: Duration,
) -> Result<(), SessionTerminationFailure> {
    let started_at = Instant::now();
    let mut observed_tmux_running = false;
    let mut observed_tmux_stopped = false;
    let mut observed_process_running = false;
    loop {
        let remaining = verify_timeout.saturating_sub(started_at.elapsed());
        if remaining.is_zero() {
            return Err(if observed_tmux_running {
                SessionTerminationFailure::StillRunning
            } else if observed_tmux_stopped && observed_process_running {
                SessionTerminationFailure::ProcessStillRunning
            } else {
                SessionTerminationFailure::VerificationFailed
            });
        }
        let tmux_status = verified_tmux_status_with_timeout(
            tmux_bin,
            &identity.session_id,
            remaining.min(DELETE_TERMINATION_PROBE_TIMEOUT),
        );
        let process_status = process_runtime_status(identity);
        if tmux_status == "stopped" && process_status == ProcessGroupStatus::Stopped {
            return Ok(());
        }
        if tmux_status == "running" {
            observed_tmux_running = true;
        } else if tmux_status == "stopped" {
            observed_tmux_stopped = true;
        }
        if process_status == ProcessGroupStatus::Running {
            observed_process_running = true;
        }
        if started_at.elapsed() >= verify_timeout {
            return Err(if observed_tmux_running {
                SessionTerminationFailure::StillRunning
            } else if observed_tmux_stopped && observed_process_running {
                SessionTerminationFailure::ProcessStillRunning
            } else {
                SessionTerminationFailure::VerificationFailed
            });
        }
        thread::sleep(
            DELETE_TERMINATION_VERIFY_POLL_INTERVAL
                .min(verify_timeout.saturating_sub(started_at.elapsed())),
        );
    }
}

fn verify_tmux_target_stopped(
    tmux_bin: &Path,
    identity: &TmuxRuntimeIdentity,
    verify_timeout: Duration,
) -> Result<(), SessionTerminationFailure> {
    let started_at = Instant::now();
    let mut observed_running = false;
    loop {
        let remaining = verify_timeout.saturating_sub(started_at.elapsed());
        if remaining.is_zero() {
            return Err(if observed_running {
                SessionTerminationFailure::StillRunning
            } else {
                SessionTerminationFailure::VerificationFailed
            });
        }
        match verified_tmux_status_with_timeout(
            tmux_bin,
            &identity.session_id,
            remaining.min(DELETE_TERMINATION_PROBE_TIMEOUT),
        )
        .as_str()
        {
            "stopped" => return Ok(()),
            "running" => observed_running = true,
            _ => {}
        }
        thread::sleep(
            DELETE_TERMINATION_VERIFY_POLL_INTERVAL
                .min(verify_timeout.saturating_sub(started_at.elapsed())),
        );
    }
}

fn verified_tmux_status_with_timeout(tmux_bin: &Path, target: &str, timeout: Duration) -> String {
    let mut command = ProcessCommand::new(tmux_bin);
    command
        .env("LC_ALL", "C")
        .arg("has-session")
        .arg("-t")
        .arg(target);
    match run_output_with_timeout(command, timeout) {
        Ok(output) if output.status.success() => "running".to_string(),
        Ok(output) if tmux_output_reports_absent(&output) => "stopped".to_string(),
        Ok(_) | Err(_) => "unknown".to_string(),
    }
}

fn tmux_output_reports_absent(output: &std::process::Output) -> bool {
    if output.status.code() != Some(1) {
        return false;
    }
    let stderr = String::from_utf8_lossy(&output.stderr).to_ascii_lowercase();
    stderr.contains("can't find session:") || stderr.contains("no server running on")
}

fn tmux_kill_identity_with_timeout(
    tmux_bin: &Path,
    identity: &TmuxRuntimeIdentity,
    timeout: Duration,
) -> Result<TmuxIdentityKillOutcome, SessionTerminationFailure> {
    let condition = format!(
        "#{{&&:#{{==:#{{session_id}},{}}},#{{&&:#{{==:#{{pane_id}},{}}},#{{==:#{{pane_pid}},{}}}}}}}",
        identity.session_id, identity.pane_id, identity.pane_pid
    );
    let mut command = ProcessCommand::new(tmux_bin);
    command
        .arg("if-shell")
        .arg("-F")
        .arg("-t")
        .arg(&identity.pane_id)
        .arg(condition)
        .arg(format!("kill-session -t {}", identity.session_id))
        .arg(format!(
            "display-message -p {TMUX_RUNTIME_IDENTITY_CHANGED_OUTPUT}"
        ));
    let output = run_output_with_timeout(command, timeout).map_err(|err| {
        if err.kind() == io::ErrorKind::TimedOut {
            SessionTerminationFailure::KillTimeout
        } else {
            SessionTerminationFailure::KillError
        }
    })?;
    if !output.status.success() {
        return Err(SessionTerminationFailure::KillFailed);
    }
    let stdout = String::from_utf8(output.stdout)
        .map_err(|_| SessionTerminationFailure::VerificationFailed)?;
    match stdout.trim() {
        "" => Ok(TmuxIdentityKillOutcome::KillConfirmed),
        TMUX_RUNTIME_IDENTITY_CHANGED_OUTPUT => Ok(TmuxIdentityKillOutcome::IdentityChanged),
        _ => Err(SessionTerminationFailure::VerificationFailed),
    }
}

#[cfg(test)]
fn kill_tmux_session_with_timeout(tmux_bin: &Path, tmux_session: &str, timeout: Duration) -> bool {
    tmux_kill_status_with_timeout(tmux_bin, tmux_session, timeout)
        .is_ok_and(|status| status.success())
}

#[cfg(test)]
fn tmux_kill_status_with_timeout(
    tmux_bin: &Path,
    tmux_session: &str,
    timeout: Duration,
) -> io::Result<std::process::ExitStatus> {
    let mut command = ProcessCommand::new(tmux_bin);
    command
        .arg("kill-session")
        .arg("-t")
        .arg(exact_tmux_target(tmux_session));
    run_output_with_timeout(command, timeout).map(|output| output.status)
}

fn exact_tmux_target(tmux_session: &str) -> String {
    format!("={tmux_session}")
}

fn managed_tmux_pane_target(tmux_session: &str) -> String {
    format!("={tmux_session}:0.0")
}

fn session_status(tmux_bin: &Path, record: &SessionRecord) -> String {
    live_status(tmux_bin, &record.tmux_session)
}

fn is_resumable(record: &SessionRecord) -> bool {
    validate_resume_metadata(record).is_ok()
}

fn validate_resume_metadata(
    record: &SessionRecord,
) -> Result<(&ProviderResume, AgentKind), CliError> {
    if record.mode != "interactive" {
        return Err(CliError::data(
            "session-not-resumable",
            format!("session mode is not resumable: {}", record.id),
            Some(json!({ "id": record.id.clone(), "mode": record.mode.clone() })),
        ));
    }
    let provider_resume = record.provider_resume.as_ref().ok_or_else(|| {
        CliError::data(
            "session-not-resumable",
            format!(
                "session has no exact provider resume identity: {}",
                record.id
            ),
            Some(json!({ "id": record.id.clone() })),
        )
    })?;
    if provider_resume.resume_args.is_empty() {
        return Err(CliError::data(
            "session-not-resumable",
            format!("session has no provider resume command: {}", record.id),
            Some(json!({ "id": record.id.clone() })),
        ));
    }
    let agent = AgentKind::from_name(&record.agent).ok_or_else(|| {
        CliError::data(
            "invalid-agent",
            format!("unknown agent in session record: {}", record.agent),
            Some(json!({ "id": record.id.clone(), "agent": record.agent.clone() })),
        )
    })?;
    if provider_resume.provider != agent.as_str() {
        return Err(CliError::data(
            "session-provider-mismatch",
            "session provider resume metadata does not match the agent",
            Some(json!({
                "id": record.id.clone(),
                "agent": record.agent.clone(),
                "provider": provider_resume.provider.clone(),
            })),
        ));
    }
    validate_stored_agent_args(record, agent)?;
    let expected_args =
        canonical_provider_resume_args(agent, &record.cwd, &provider_resume.session_id)
            .ok_or_else(|| {
                CliError::data(
                    "session-not-resumable",
                    format!("session provider is not resumable: {}", record.id),
                    Some(json!({
                        "id": record.id.clone(),
                        "agent": record.agent.clone(),
                        "provider": provider_resume.provider.clone(),
                    })),
                )
            })?;
    if provider_resume.session_id.trim().is_empty() || provider_resume.resume_args != expected_args
    {
        return Err(CliError::data(
            "session-not-resumable",
            "session provider resume command does not match the stored identity",
            Some(json!({
                "id": record.id.clone(),
                "agent": record.agent.clone(),
                "provider": provider_resume.provider.clone(),
            })),
        ));
    }
    Ok((provider_resume, agent))
}

pub(crate) fn canonical_provider_resume_args(
    agent: AgentKind,
    cwd: &str,
    session_id: &str,
) -> Option<Vec<String>> {
    match agent {
        AgentKind::Codex => Some(vec![
            "resume".to_string(),
            session_id.to_string(),
            "--cd".to_string(),
            cwd.to_string(),
            "--no-alt-screen".to_string(),
        ]),
        AgentKind::Claude => Some(vec!["--resume".to_string(), session_id.to_string()]),
        AgentKind::Hermes => None,
    }
}

fn validate_stored_agent_args(record: &SessionRecord, agent: AgentKind) -> Result<(), CliError> {
    let flag = match agent {
        AgentKind::Codex => record
            .agent_args
            .iter()
            .find_map(|arg| reserved_codex_resume_arg(arg)),
        AgentKind::Claude => record
            .agent_args
            .iter()
            .find_map(|arg| reserved_claude_resume_arg(arg)),
        AgentKind::Hermes => None,
    };
    if let Some(flag) = flag {
        return Err(CliError::data(
            "session-not-resumable",
            "session provider arguments conflict with durable resume identity",
            Some(json!({
                "id": record.id.clone(),
                "agent": record.agent.clone(),
                "flag": flag,
            })),
        ));
    }
    Ok(())
}

fn repo_name_from_cwd(cwd: &str) -> Option<String> {
    let trimmed = cwd.trim_end_matches(['/', '\\']);
    if trimmed.is_empty() {
        return None;
    }
    Path::new(trimmed)
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
}

fn repo_remote_url_from_cwd(cwd: &str) -> Option<String> {
    let trimmed = cwd.trim();
    if trimmed.is_empty() {
        return None;
    }
    let root_output = ProcessCommand::new("git")
        .arg("-C")
        .arg(trimmed)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if !root_output.status.success() {
        return None;
    }
    let root = String::from_utf8(root_output.stdout).ok()?;
    let root = root.trim();
    if root.is_empty() {
        return None;
    }

    let remote_output = ProcessCommand::new("git")
        .arg("-C")
        .arg(root)
        .args(["remote", "get-url", "origin"])
        .output()
        .ok()?;
    if !remote_output.status.success() {
        return None;
    }
    let remote = String::from_utf8(remote_output.stdout).ok()?;
    git_remote_web_url(&remote)
}

fn git_remote_web_url(remote: &str) -> Option<String> {
    let parsed = parse_git_remote_url(remote)?;
    if parsed.host.trim().is_empty() || parsed.path.trim().is_empty() {
        return None;
    }
    Some(format!("https://{}/{}", parsed.host, parsed.path))
}

fn live_status(tmux_bin: &Path, tmux_session: &str) -> String {
    live_status_with_timeout(tmux_bin, tmux_session, PANE_INPUT_COMMAND_TIMEOUT)
}

fn live_status_with_timeout(tmux_bin: &Path, tmux_session: &str, timeout: Duration) -> String {
    let mut command = ProcessCommand::new(tmux_bin);
    command
        .arg("has-session")
        .arg("-t")
        .arg(exact_tmux_target(tmux_session));
    match run_exit_status_with_timeout(command, timeout) {
        Ok(status) if status.success() => "running".to_string(),
        Ok(status) if status.code() == Some(1) => "stopped".to_string(),
        Ok(_) => "unknown".to_string(),
        Err(_) => "unknown".to_string(),
    }
}

fn run_status(mut command: ProcessCommand, label: &str) -> Result<(), CliError> {
    let output = command.output().map_err(|err| {
        CliError::runtime(
            "command-spawn-failed",
            format!("failed to run {label}: {err}"),
            None,
        )
    })?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    Err(CliError::runtime(
        "command-failed",
        if stderr.is_empty() {
            format!("{label} failed with status {}", output.status)
        } else {
            format!("{label} failed: {stderr}")
        },
        None,
    ))
}

fn run_status_with_timeout(
    command: ProcessCommand,
    label: &str,
    timeout: Duration,
) -> Result<(), CliError> {
    let status = run_exit_status_with_timeout(command, timeout).map_err(|err| {
        let code = if err.kind() == io::ErrorKind::TimedOut {
            "command-timeout"
        } else {
            "command-wait-failed"
        };
        CliError::runtime(code, format!("failed to run {label}: {err}"), None)
    })?;
    if status.success() {
        Ok(())
    } else {
        Err(CliError::runtime(
            "command-failed",
            format!("{label} failed with status {status}"),
            None,
        ))
    }
}

fn run_output_with_timeout(
    command: ProcessCommand,
    timeout: Duration,
) -> io::Result<std::process::Output> {
    run_output_with_timeout_and_cap(command, timeout, DELETE_TMUX_PROBE_MAX_OUTPUT_BYTES)
}

pub(crate) fn run_output_with_timeout_and_cap(
    mut command: ProcessCommand,
    timeout: Duration,
    max_output_bytes: usize,
) -> io::Result<std::process::Output> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let mut child = command.spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("command stdout was not captured"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("command stderr was not captured"))?;
    let (output_tx, output_rx) = std::sync::mpsc::channel();
    let stdout_tx = output_tx.clone();
    thread::spawn(move || {
        let _ = stdout_tx.send((true, read_capped_output(stdout, max_output_bytes)));
    });
    thread::spawn(move || {
        let _ = output_tx.send((false, read_capped_output(stderr, max_output_bytes)));
    });
    let started_at = Instant::now();
    let mut status = None;
    let mut stdout = None;
    let mut stderr = None;
    loop {
        if status.is_none() {
            match child.try_wait() {
                Ok(Some(exit)) => status = Some(exit),
                Ok(None) => {}
                Err(err) => {
                    terminate_command_process_group(&mut child);
                    return Err(err);
                }
            }
        }
        while let Ok((is_stdout, bytes)) = output_rx.try_recv() {
            if is_stdout {
                stdout = Some(bytes);
            } else {
                stderr = Some(bytes);
            }
        }
        if status.is_some() && stdout.is_some() && stderr.is_some() {
            let Some(status) = status.take() else {
                unreachable!("command status was checked")
            };
            let Some(stdout) = stdout.take() else {
                unreachable!("command stdout was checked")
            };
            let Some(stderr) = stderr.take() else {
                unreachable!("command stderr was checked")
            };
            return Ok(std::process::Output {
                status,
                stdout: stdout?,
                stderr: stderr?,
            });
        }
        if started_at.elapsed() >= timeout {
            terminate_command_process_group(&mut child);
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("process exceeded {} ms", timeout.as_millis()),
            ));
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn read_capped_output(mut pipe: impl Read, max_output_bytes: usize) -> io::Result<Vec<u8>> {
    let mut retained = Vec::new();
    let mut buffer = [0_u8; 1024];
    loop {
        let read = pipe.read(&mut buffer)?;
        if read == 0 {
            return Ok(retained);
        }
        let remaining = max_output_bytes.saturating_sub(retained.len());
        retained.extend_from_slice(&buffer[..read.min(remaining)]);
    }
}

fn terminate_command_process_group(child: &mut std::process::Child) {
    let pid = child.id() as libc::pid_t;
    // SAFETY: probe commands are launched as leaders of dedicated process groups.
    unsafe {
        libc::kill(-pid, libc::SIGKILL);
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn run_exit_status_with_timeout(
    mut command: ProcessCommand,
    timeout: Duration,
) -> io::Result<std::process::ExitStatus> {
    command.stdout(Stdio::null()).stderr(Stdio::null());
    let mut child = command.spawn()?;
    let started_at = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) if started_at.elapsed() < timeout => {
                thread::sleep(Duration::from_millis(10));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("process exceeded {} ms", timeout.as_millis()),
                ));
            }
            Err(err) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(err);
            }
        }
    }
}

fn read_prompt(
    prompt: &Option<String>,
    prompt_file: Option<&Path>,
    prompt_stdin: bool,
) -> Result<Option<String>, CliError> {
    let source_count = usize::from(prompt.is_some())
        + usize::from(prompt_file.is_some())
        + usize::from(prompt_stdin);
    if source_count > 1 {
        return Err(CliError::usage(
            "multiple-prompt-sources",
            "use only one of --prompt, --prompt-file, or --prompt-stdin",
            None,
        ));
    }
    if let Some(prompt) = prompt {
        return Ok(Some(prompt.clone()));
    }
    if prompt_stdin || prompt_file == Some(Path::new("-")) {
        let mut input = String::new();
        io::stdin().read_to_string(&mut input).map_err(|err| {
            CliError::runtime(
                "stdin-read-failed",
                format!("failed to read stdin: {err}"),
                None,
            )
        })?;
        return Ok(Some(input));
    }
    if let Some(path) = prompt_file {
        let path = absolute_path(path)?;
        let input = fs::read_to_string(&path).map_err(|err| {
            CliError::runtime(
                "prompt-file-read-failed",
                format!("failed to read {}: {err}", path.display()),
                Some(json!({ "path": display_path(&path) })),
            )
        })?;
        return Ok(Some(input));
    }
    Ok(None)
}

fn resolve_state_dir(explicit: Option<PathBuf>) -> Result<PathBuf, CliError> {
    if let Some(path) = explicit {
        return absolute_path(&path);
    }
    if let Some(path) = non_empty_env("AGENT_SESSION_STATE_DIR") {
        return absolute_path(Path::new(&path));
    }
    if let Some(path) = non_empty_env("XDG_STATE_HOME") {
        return Ok(normalize_path(&PathBuf::from(path).join("agent-session")));
    }
    let home = home_dir().ok_or_else(|| {
        CliError::runtime(
            "home-unavailable",
            "HOME is unset; pass --state-dir",
            Some(json!({ "flag": "--state-dir" })),
        )
    })?;
    Ok(normalize_path(&home.join(".local/state/agent-session")))
}

fn resolve_cwd(explicit: Option<&Path>) -> Result<PathBuf, CliError> {
    let cwd = match explicit {
        Some(path) => absolute_path(path)?,
        None => env::current_dir().map_err(|err| {
            CliError::runtime(
                "cwd-unavailable",
                format!("failed to read current directory: {err}"),
                None,
            )
        })?,
    };
    let metadata = fs::metadata(&cwd).map_err(|err| {
        CliError::usage(
            "cwd-unavailable",
            format!("working directory does not exist: {}: {err}", cwd.display()),
            Some(json!({ "cwd": display_path(&cwd) })),
        )
    })?;
    if !metadata.is_dir() {
        return Err(CliError::usage(
            "cwd-not-directory",
            format!("working directory is not a directory: {}", cwd.display()),
            Some(json!({ "cwd": display_path(&cwd) })),
        ));
    }
    Ok(cwd)
}

fn absolute_path(path: &Path) -> Result<PathBuf, CliError> {
    let expanded = expand_home(path);
    if expanded.is_absolute() {
        return Ok(normalize_path(&expanded));
    }
    let cwd = env::current_dir().map_err(|err| {
        CliError::runtime(
            "cwd-unavailable",
            format!("failed to read current directory: {err}"),
            None,
        )
    })?;
    Ok(normalize_path(&cwd.join(expanded)))
}

fn resolve_tmux_bin(explicit: Option<&Path>) -> PathBuf {
    explicit
        .map(Path::to_path_buf)
        .or_else(|| non_empty_env("AGENT_SESSION_TMUX_BIN").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("tmux"))
}

fn resolve_host(host: Option<String>) -> Result<Option<String>, CliError> {
    let Some(host) = host else {
        return Ok(None);
    };
    let host = host.trim();
    if host.is_empty() {
        return Ok(None);
    }
    validate_host(host)?;
    Ok(Some(host.to_string()))
}

fn validate_host(host: &str) -> Result<(), CliError> {
    if host.starts_with('-') {
        return Err(CliError::usage(
            "invalid-host",
            "host must not start with '-' because ssh would parse it as an option",
            Some(json!({ "host": host })),
        ));
    }
    if host.chars().any(char::is_control) || host.chars().any(char::is_whitespace) {
        return Err(CliError::usage(
            "invalid-host",
            "host must not contain whitespace or control characters",
            Some(json!({ "host": host })),
        ));
    }
    Ok(())
}

fn resolve_agent_bin(agent: AgentKind, explicit: Option<&Path>) -> PathBuf {
    if let Some(path) = explicit {
        return path.to_path_buf();
    }
    let env_key = match agent {
        AgentKind::Codex => "AGENT_SESSION_CODEX_BIN",
        AgentKind::Claude => "AGENT_SESSION_CLAUDE_BIN",
        AgentKind::Hermes => "AGENT_SESSION_HERMES_BIN",
    };
    non_empty_env(env_key)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(agent.as_str()))
}

fn resolve_session_id(
    context: &CliContext,
    explicit_id: Option<&str>,
    agent: AgentKind,
    timestamp: &str,
    title_slug: Option<&str>,
) -> Result<String, CliError> {
    if let Some(id) = explicit_id {
        validate_id(id)?;
        if session_dir(context, id).exists() {
            return Err(CliError::runtime(
                "session-exists",
                format!("session already exists: {id}"),
                Some(json!({ "id": id })),
            ));
        }
        return Ok(id.to_string());
    }
    let base = default_session_id_base(timestamp, agent, title_slug);
    for index in 0..100 {
        let id = if index == 0 {
            base.clone()
        } else {
            format!("{base}-{index}")
        };
        if !session_dir(context, &id).exists() {
            return Ok(id);
        }
    }
    Err(CliError::runtime(
        "session-id-exhausted",
        "failed to allocate a unique session id",
        Some(json!({ "base": base })),
    ))
}

fn default_session_id_base(timestamp: &str, agent: AgentKind, title_slug: Option<&str>) -> String {
    match title_slug {
        Some(slug) => format!("{timestamp}-{}-{slug}", agent.as_str()),
        None => format!("{timestamp}-{}", agent.as_str()),
    }
}

fn validate_id(id: &str) -> Result<(), CliError> {
    if id.is_empty()
        || !id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
    {
        return Err(CliError::usage(
            "invalid-session-id",
            "session id may contain only ASCII letters, digits, '-' and '_'",
            Some(json!({ "id": id })),
        ));
    }
    Ok(())
}

fn session_dir(context: &CliContext, id: &str) -> PathBuf {
    context.state_dir.join("sessions").join(id)
}

fn private_dir(path: &Path) -> Result<(), CliError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| {
            CliError::runtime(
                "directory-create-failed",
                format!("failed to create {}: {err}", parent.display()),
                Some(json!({ "path": display_path(parent) })),
            )
        })?;
    }
    // Create the session dir as an ATOMIC ownership claim (fail if it already
    // exists) rather than create_dir_all. This closes a create/create race:
    // without it, two concurrent creates of the same id both pass the earlier
    // exists() check, both proceed, and the one whose tmux new-session loses the
    // duplicate-name race runs cleanup_created_record -> remove_dir_all on the
    // shared dir, deleting the winner's session.json and orphaning a live agent.
    match fs::create_dir(path) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
            return Err(CliError::runtime(
                "session-exists",
                format!("session already exists: {}", path.display()),
                Some(json!({ "path": display_path(path) })),
            ));
        }
        Err(err) => {
            return Err(CliError::runtime(
                "directory-create-failed",
                format!("failed to create {}: {err}", path.display()),
                Some(json!({ "path": display_path(path) })),
            ));
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let permissions = fs::Permissions::from_mode(0o700);
        fs::set_permissions(path, permissions).map_err(|err| {
            CliError::runtime(
                "directory-permissions-failed",
                format!("failed to set permissions on {}: {err}", path.display()),
                Some(json!({ "path": display_path(path) })),
            )
        })?;
    }
    Ok(())
}

fn ensure_private_dir(path: &Path) -> Result<(), CliError> {
    fs::create_dir_all(path).map_err(|err| {
        CliError::runtime(
            "directory-create-failed",
            format!("failed to create {}: {err}", path.display()),
            Some(json!({ "path": display_path(path) })),
        )
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let permissions = fs::Permissions::from_mode(0o700);
        fs::set_permissions(path, permissions).map_err(|err| {
            CliError::runtime(
                "directory-permissions-failed",
                format!("failed to set permissions on {}: {err}", path.display()),
                Some(json!({ "path": display_path(path) })),
            )
        })?;
    }
    Ok(())
}

fn write_private_file(path: &Path, bytes: &[u8]) -> Result<(), CliError> {
    write_atomic(path, bytes, SECRET_FILE_MODE).map_err(|err| {
        CliError::runtime(
            "file-write-failed",
            format!("failed to write {}: {err}", path.display()),
            Some(json!({ "path": display_path(path) })),
        )
    })
}

fn render_single_success<T: Serialize>(
    command: &'static str,
    format: OutputFormat,
    result: &T,
    render_text: fn(&T) -> String,
) -> i32 {
    match format {
        OutputFormat::Json => {
            let envelope = Envelope::success(schema_version_for(BINARY, command, 1), result);
            print_json(&envelope)
        }
        OutputFormat::Text => {
            print!("{}", render_text(result));
            exit::SUCCESS
        }
    }
}

fn render_list_success(format: OutputFormat, results: &[SessionView]) -> i32 {
    match format {
        OutputFormat::Json => {
            let envelope = Envelope::success(schema_version_for(BINARY, LIST_COMMAND, 1), results);
            print_json(&envelope)
        }
        OutputFormat::Text => {
            if results.is_empty() {
                println!("no agent sessions");
            } else {
                for result in results {
                    println!(
                        "{}  {}  {}  {}",
                        result.id, result.agent, result.status, result.cwd
                    );
                }
            }
            exit::SUCCESS
        }
    }
}

fn render_error(command: &'static str, format: OutputFormat, err: CliError) -> i32 {
    let err = err.into_inner();
    match format {
        OutputFormat::Json => {
            let mut envelope_error = EnvelopeError::new(err.code, err.message);
            if let Some(details) = err.details {
                envelope_error = envelope_error.with_details(details);
            }
            if let Some(hint) = err.hint {
                envelope_error = envelope_error.with_hint(hint);
            }
            let envelope: Envelope<()> =
                Envelope::failure(schema_version_for(BINARY, command, 1), envelope_error);
            print_json(&envelope);
        }
        OutputFormat::Text => {
            let _ = writeln!(io::stderr(), "error: {}", err.message);
            if let Some(hint) = err.hint.as_deref() {
                let _ = writeln!(io::stderr(), "hint: {hint}");
            }
        }
    }
    err.exit_code
}

fn print_json<T: Serialize>(value: &T) -> i32 {
    match serde_json::to_string(value) {
        Ok(serialized) => {
            println!("{serialized}");
            exit::SUCCESS
        }
        Err(err) => {
            eprintln!("error: failed to serialize json: {err}");
            exit::SOFTWARE
        }
    }
}

fn render_started_text(result: &SessionView) -> String {
    let mut text = format!(
        "started {} session {}\ntmux: {}\nattach: {}\n",
        result.agent, result.id, result.tmux_session, result.attach_command
    );
    if let Some(command) = &result.ssh_attach_command {
        text.push_str(&format!("ssh: {command}\n"));
    }
    text.push_str(&format!("delete: agent-session delete {}\n", result.id));
    text
}

fn render_command_text(result: &SessionView) -> String {
    match &result.ssh_attach_command {
        Some(command) => format!("{command}\nlocal: {}\n", result.attach_command),
        None => format!("{}\n", result.attach_command),
    }
}

fn render_logs_text(result: &LogsResult) -> String {
    result.text.clone()
}

fn render_send_text(result: &SendResult) -> String {
    let mut parts = Vec::new();
    if result.sent_text {
        parts.push("text".to_string());
    }
    if !result.keys.is_empty() {
        parts.push(format!("keys [{}]", result.keys.join(" ")));
    }
    let detail = if parts.is_empty() {
        "nothing".to_string()
    } else {
        parts.join(" + ")
    };
    format!("sent {detail} to {}\n", result.id)
}

fn render_glance_text(result: &GlanceResult) -> String {
    let mut text = format!("{} {} [{}]\n", result.id, result.agent, result.status);
    text.push_str(&result.tail);
    if !result.tail.is_empty() && !result.tail.ends_with('\n') {
        text.push('\n');
    }
    text
}

fn render_resumed_text(result: &SessionView) -> String {
    format!(
        "resumed {} session {}\ntmux: {}\nattach: {}\n",
        result.agent, result.id, result.tmux_session, result.attach_command
    )
}

fn render_activity_text(result: &activity::ActivityResult) -> String {
    format!(
        "{}: {:?} (revision {})\n",
        result.id, result.turn_state.phase, result.turn_state.revision
    )
}

fn render_doctor_text(result: &activity::DoctorResult) -> String {
    let mut text = String::new();
    text.push_str(&format!("binary: {}\n", result.binary_version));
    for provider in &result.providers {
        text.push_str(&format!(
            "{}: {} (configured: {}, can launch worker: {})\n",
            provider.provider,
            provider.classification,
            if provider.configured { "yes" } else { "no" },
            if provider.can_launch_worker {
                "yes"
            } else {
                "no"
            }
        ));
        text.push_str(&format!("  completion: {}\n", provider.completion));
        if let Some(mode) = provider.notification_mode.as_deref() {
            text.push_str(&format!("  notification: {mode}\n"));
        }
        if let Some(representation) = provider.hook_representation.as_deref() {
            text.push_str(&format!(
                "  hooks: {representation} (migration required: {}; conflict: {})\n",
                if provider.hook_migration_required == Some(true) {
                    "yes"
                } else {
                    "no"
                },
                if provider.representation_conflict == Some(true) {
                    "yes"
                } else {
                    "no"
                }
            ));
        }
        text.push_str(&format!(
            "  attention: {}\n",
            provider.attention_correlation
        ));
        text.push_str(&format!("  next: {}\n", provider.guidance));
    }
    text
}

fn render_delete_text(result: &DeleteResult) -> String {
    format!(
        "deleted {} (tmux stopped: {})\n",
        result.id,
        if result.killed { "yes" } else { "no" }
    )
}

fn local_attach_command(tmux_session: &str) -> String {
    format!("tmux attach -t {}", shell_words::quote(tmux_session))
}

fn ssh_attach_command(host: &str, tmux_session: &str) -> String {
    let remote = local_attach_command(tmux_session);
    format!(
        "ssh -t {} {}",
        shell_words::quote(host),
        shell_words::quote(&remote)
    )
}

fn slugify(value: &str) -> String {
    let mut slug = String::new();
    let mut last_dash = false;
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            slug.push('-');
            last_dash = true;
        }
        if slug.len() >= 32 {
            break;
        }
    }
    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() {
        "session".to_string()
    } else {
        slug
    }
}

fn non_empty_env(key: &str) -> Option<String> {
    env::var(key).ok().filter(|value| !value.trim().is_empty())
}

fn is_truthy_flag(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn env_truthy(key: &str) -> bool {
    non_empty_env(key).is_some_and(|value| is_truthy_flag(&value))
}

/// First `name` found on `PATH` as a regular file, or `None`.
fn binary_on_path(name: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

fn env_u64(key: &str, default: u64) -> u64 {
    non_empty_env(key)
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(default)
}

fn short_hostname() -> Option<String> {
    let output = ProcessCommand::new("hostname").arg("-s").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if value.is_empty() { None } else { Some(value) }
}

fn tail_lines(text: &str, tail: usize) -> String {
    if tail == 0 {
        return String::new();
    }
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(tail);
    let mut output = lines[start..].join("\n");
    if !output.is_empty() {
        output.push('\n');
    }
    output
}

#[cfg(test)]
mod tests {
    use super::{
        AgentKind, CliContext, DeleteResult, RecordRequest, SessionRegistryFence,
        TmuxProcessIdentity, TmuxRuntimeIdentity, acquire_session_record_lock,
        acquire_session_record_lock_timed, create_record, delete_session_with_timeouts,
        kill_tmux_session_with_timeout, live_status_with_timeout, load_session_record, pane_drawn,
        parse_pane_cursor, persist_tmux_runtime_identity, render_delete_text,
        resolve_agent_session_executable_from, resolve_session_id, session_dir, session_view,
        strip_trailing_blank_lines, tmux_launch_may_have_created_runtime,
        try_acquire_session_record_lock, write_session_record,
    };

    #[test]
    fn pane_readiness_treats_an_unanswerable_tmux_reply_as_unobservable() {
        // Anything other than the exact `row|column` shape must not be turned
        // into a readiness decision, or a launch would block on a signal that
        // is never coming.
        for reply in ["", "\n", "unknown", "3", "3|", "|4", "a|b", "3|4|5\nextra"] {
            assert_eq!(parse_pane_cursor(reply), None, "reply={reply:?}");
        }
        assert_eq!(parse_pane_cursor("9|25\n"), Some((9, 25)));
        assert_eq!(parse_pane_cursor(" 0 | 0 "), Some((0, 0)));
    }

    #[test]
    fn pane_is_drawn_only_after_the_cursor_leaves_the_origin() {
        // The managed launch wrapper waits on its gate files silently, so an
        // untouched pane stays at the origin until the provider TUI draws.
        assert!(!pane_drawn((0, 0)));
        assert!(pane_drawn((0, 1)));
        assert!(pane_drawn((9, 25)));
    }

    #[test]
    fn agent_session_executable_resolves_facade_to_exact_release_sibling() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let main_agent = tmp
            .path()
            .join(format!("main-agent{}", std::env::consts::EXE_SUFFIX));
        let agent_session = tmp
            .path()
            .join(format!("agent-session{}", std::env::consts::EXE_SUFFIX));
        fs::write(&main_agent, "main-agent").expect("main-agent fixture");
        fs::write(&agent_session, "agent-session").expect("agent-session fixture");
        fs::set_permissions(&main_agent, fs::Permissions::from_mode(0o700))
            .expect("main-agent mode");
        fs::set_permissions(&agent_session, fs::Permissions::from_mode(0o700))
            .expect("agent-session mode");

        assert_eq!(
            resolve_agent_session_executable_from(&main_agent).expect("sibling executable"),
            agent_session
        );
    }

    #[test]
    fn agent_session_executable_preserves_direct_agent_session_recursion() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let agent_session = tmp
            .path()
            .join(format!("agent-session{}", std::env::consts::EXE_SUFFIX));
        fs::write(&agent_session, "agent-session").expect("agent-session fixture");
        fs::set_permissions(&agent_session, fs::Permissions::from_mode(0o700))
            .expect("agent-session mode");

        assert_eq!(
            resolve_agent_session_executable_from(&agent_session).expect("current executable"),
            agent_session
        );
    }

    #[test]
    fn agent_session_executable_fails_closed_for_missing_or_non_executable_sibling() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let main_agent = tmp
            .path()
            .join(format!("main-agent{}", std::env::consts::EXE_SUFFIX));
        let agent_session = tmp
            .path()
            .join(format!("agent-session{}", std::env::consts::EXE_SUFFIX));
        fs::write(&main_agent, "main-agent").expect("main-agent fixture");
        fs::set_permissions(&main_agent, fs::Permissions::from_mode(0o700))
            .expect("main-agent mode");

        let missing = resolve_agent_session_executable_from(&main_agent)
            .expect_err("missing sibling must fail");
        assert_eq!(missing.kind(), io::ErrorKind::NotFound);

        fs::write(&agent_session, "agent-session").expect("agent-session fixture");
        fs::set_permissions(&agent_session, fs::Permissions::from_mode(0o600))
            .expect("agent-session mode");
        let non_executable = resolve_agent_session_executable_from(&main_agent)
            .expect_err("non-executable sibling must fail");
        assert_eq!(non_executable.kind(), io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn explicit_coordination_mode_is_in_the_first_durable_record() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let created = create_record(RecordRequest {
            context: &context,
            agent: AgentKind::Codex,
            mode: "interactive",
            coordination_mode: crate::cli::CoordinationMode::Enforce,
            title: None,
            title_state: None,
            explicit_id: Some("explicit-enforce"),
            cwd: Path::new("/repo"),
            prompt: None,
            log_file_name: None,
            provider_resume: None,
            agent_args: Vec::new(),
            agent_bin: None,
        })
        .expect("create record");
        let durable = load_session_record(&context, &created.record.id).expect("durable record");
        assert_eq!(
            durable.coordination_mode,
            crate::cli::CoordinationMode::Enforce
        );
    }

    #[test]
    fn coordination_review_held_launch_starts_persistent_broker_before_the_gate() {
        let heartbeat = super::HELD_LAUNCH_SCRIPT
            .find("broker heartbeat")
            .expect("broker sidecar setup");
        let gate = super::HELD_LAUNCH_SCRIPT
            .find("while [ ! -f \"$gate\" ]")
            .expect("gate wait");
        assert!(heartbeat < gate);
        assert!(super::HELD_LAUNCH_SCRIPT.contains("\"$capability\""));
        assert!(!super::HELD_LAUNCH_SCRIPT.contains("heartbeat_once"));
    }

    #[test]
    fn held_launch_waits_for_broker_provision_before_starting_heartbeat() {
        let provision = super::HELD_LAUNCH_SCRIPT
            .find("while [ ! -f \"$broker_gate\" ]; do sleep 0.01; done")
            .expect("broker provision wait");
        let heartbeat = super::HELD_LAUNCH_SCRIPT
            .find("broker heartbeat")
            .expect("broker sidecar setup");
        let gate = super::HELD_LAUNCH_SCRIPT
            .find("while [ ! -f \"$gate\" ]")
            .expect("gate wait");

        assert!(provision < heartbeat);
        assert!(heartbeat < gate);
    }

    #[test]
    fn held_launch_keeps_provider_in_foreground_and_tracks_broker_lifecycle() {
        assert!(!super::HELD_LAUNCH_SCRIPT.contains("kill -0"));
        assert!(!super::HELD_LAUNCH_SCRIPT.contains("child=$!"));
        assert!(super::HELD_LAUNCH_SCRIPT.contains("broker_pid=$!"));
        assert!(super::HELD_LAUNCH_SCRIPT.contains("; \"$@\"; status=$?;"));
    }

    #[test]
    fn held_launch_executes_broker_and_provider_lifecycle_under_terminal() {
        use std::fs::File;
        use std::os::fd::FromRawFd;
        use std::process::Stdio;

        let tmp = tempfile::TempDir::new().unwrap();
        let gate = tmp.path().join("launch-ready");
        let broker_gate = tmp.path().join("broker-provisioned");
        let heartbeat = tmp.path().join("heartbeat");
        let capability = tmp.path().join("capability");
        let events = tmp.path().join("events");
        let broker = tmp.path().join("broker");
        let provider = tmp.path().join("provider");
        fs::write(&capability, "capability\n").unwrap();
        fs::write(
            &broker,
            r#"#!/bin/sh
case " $* " in
  *" broker heartbeat "*)
    printf 'heartbeat\n' >> "$HELD_LAUNCH_EVENTS"
    while :; do sleep 0.05; done
    ;;
  *" broker stop "*)
    printf 'stop\n' >> "$HELD_LAUNCH_EVENTS"
    ;;
  *) exit 64 ;;
esac
"#,
        )
        .unwrap();
        fs::write(
            &provider,
            r#"#!/bin/sh
if [ -t 0 ]; then
  printf 'provider-tty\n' >> "$HELD_LAUNCH_EVENTS"
  exit 23
fi
printf 'provider-no-tty\n' >> "$HELD_LAUNCH_EVENTS"
exit 97
"#,
        )
        .unwrap();
        for executable in [&broker, &provider] {
            fs::set_permissions(executable, fs::Permissions::from_mode(0o700)).unwrap();
        }

        let mut master_fd = -1;
        let mut slave_fd = -1;
        // SAFETY: openpty initializes both descriptors; each successful descriptor is
        // immediately transferred into exactly one File and closed by its owner.
        let openpty_status = unsafe {
            libc::openpty(
                &mut master_fd,
                &mut slave_fd,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(openpty_status, 0, "openpty: {}", io::Error::last_os_error());
        // SAFETY: openpty returned two fresh, owned descriptors above.
        let _pty_master = unsafe { File::from_raw_fd(master_fd) };
        // SAFETY: openpty returned two fresh, owned descriptors above.
        let pty_slave = unsafe { File::from_raw_fd(slave_fd) };

        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg(super::HELD_LAUNCH_SCRIPT)
            .arg("agent-session-held-launch")
            .arg(&gate)
            .arg(&broker_gate)
            .arg(&heartbeat)
            .arg(&capability)
            .arg("incarnation")
            .arg("7")
            .arg(&broker)
            .arg(&provider)
            .env("AGENT_SESSION_STATE_DIR", tmp.path())
            .env("AGENT_SESSION_ID", "held-launch-test")
            .env("HELD_LAUNCH_EVENTS", &events)
            .stdin(Stdio::from(pty_slave))
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut child = command.spawn().unwrap();

        thread::sleep(Duration::from_millis(100));
        let before_provision = fs::read_to_string(&events).unwrap_or_default();
        if !before_provision.is_empty() {
            let _ = child.kill();
            let _ = child.wait();
            panic!("heartbeat ran before broker provisioning: {before_provision:?}");
        }

        fs::write(&broker_gate, "ready\n").unwrap();
        let heartbeat_deadline = Instant::now() + Duration::from_secs(2);
        while !fs::read_to_string(&events)
            .unwrap_or_default()
            .contains("heartbeat\n")
        {
            if Instant::now() >= heartbeat_deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("heartbeat did not start after broker provisioning");
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(fs::read_to_string(&events).unwrap(), "heartbeat\n");

        fs::write(&gate, "ready\n").unwrap();
        let exit_deadline = Instant::now() + Duration::from_secs(2);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= exit_deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("held launch did not exit after the provider completed");
            }
            thread::sleep(Duration::from_millis(10));
        };

        assert_eq!(status.code(), Some(23));
        assert_eq!(
            fs::read_to_string(&events).unwrap(),
            "heartbeat\nprovider-tty\nstop\n"
        );
        assert!(!capability.exists());
        assert!(!broker_gate.exists());
        assert!(!gate.exists());
        assert!(!fs::read_dir(tmp.path()).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("heartbeat.done.")
        }));
    }
    use pretty_assertions::assert_eq;
    #[cfg(target_os = "linux")]
    use std::env;
    use std::fs;
    use std::io;
    #[cfg(target_os = "linux")]
    use std::io::BufRead as _;
    #[cfg(target_os = "linux")]
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::process::CommandExt;
    use std::path::Path;
    #[cfg(target_os = "linux")]
    use std::path::PathBuf;
    #[cfg(target_os = "linux")]
    use std::process::Stdio;
    use std::process::{Child, Command};
    use std::sync::mpsc;
    use std::thread;
    use std::time::{Duration, Instant};

    fn test_context(state_dir: &Path) -> CliContext {
        CliContext {
            state_dir: state_dir.to_path_buf(),
            host: None,
        }
    }

    #[test]
    fn structured_title_renderer_keeps_references_visible_after_topic_truncation() {
        let title = super::render_session_title_state(&super::SessionTitleState {
            topic: Some(format!("{} #317", "x".repeat(100))),
            topic_source: super::SessionTitleTopicSource::Auto,
            references: vec!["#317".to_string()],
            activity: Some("Implement contract".to_string()),
            extra: std::collections::BTreeMap::new(),
        })
        .unwrap()
        .unwrap();

        assert!(title.contains("#317 - Implement contract"));
    }

    #[test]
    fn structured_title_renderer_requires_reference_token_boundary() {
        let render = |topic: &str| {
            super::render_session_title_state(&super::SessionTitleState {
                topic: Some(topic.to_string()),
                topic_source: super::SessionTitleTopicSource::Auto,
                references: vec!["#317".to_string()],
                activity: Some("Implement fix".to_string()),
                extra: std::collections::BTreeMap::new(),
            })
            .unwrap()
            .unwrap()
        };

        assert_eq!(
            render("Parser #317alpha"),
            "Parser #317alpha #317 - Implement fix"
        );
        assert_eq!(
            render("Parser #317_foo"),
            "Parser #317_foo #317 - Implement fix"
        );
        assert_eq!(render("Parser #317"), "Parser #317 - Implement fix");
    }

    #[test]
    fn structured_title_boundary_accepts_v122_legacy_pairs_during_transition() {
        let state = super::SessionTitleState {
            topic: Some("Parser #317alpha".to_string()),
            topic_source: super::SessionTitleTopicSource::Auto,
            references: vec!["#317".to_string()],
            activity: Some("Implement fix".to_string()),
            extra: std::collections::BTreeMap::new(),
        };
        let legacy_title = "Parser #317alpha - Implement fix";
        let canonical_title = "Parser #317alpha #317 - Implement fix";

        let (title, _) = super::canonicalize_structured_title_pair(
            Some(legacy_title.to_string()),
            true,
            state.clone(),
        )
        .expect("v1.22.0 clients remain compatible during the transition");
        assert_eq!(title.as_deref(), Some(canonical_title));

        let tmp = tempfile::TempDir::new().unwrap();
        let context = test_context(tmp.path());
        let created = create_record(RecordRequest {
            context: &context,
            agent: AgentKind::Codex,
            mode: "interactive",
            coordination_mode: crate::cli::CoordinationMode::Advisory,
            title: Some(canonical_title),
            title_state: Some(state),
            explicit_id: Some("v122-compatible-title-boundary"),
            cwd: Path::new("/repo"),
            prompt: None,
            log_file_name: None,
            provider_resume: None,
            agent_args: Vec::new(),
            agent_bin: None,
        })
        .unwrap();
        drop(created);

        let record_path =
            super::session_dir(&context, "v122-compatible-title-boundary").join("session.json");
        let mut raw: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&record_path).unwrap()).unwrap();
        raw["title"] = serde_json::json!(legacy_title);
        fs::write(&record_path, serde_json::to_vec_pretty(&raw).unwrap()).unwrap();

        let record =
            super::load_session_record(&context, "v122-compatible-title-boundary").unwrap();
        assert!(super::effective_session_title_state(&record).is_some());
    }

    #[test]
    fn structured_title_v122_compatibility_outputs_match_release_fixtures() {
        let render = |topic: &str, references: &[&str]| {
            super::render_v122_legacy_session_title_state(&super::SessionTitleState {
                topic: Some(topic.to_string()),
                topic_source: super::SessionTitleTopicSource::Auto,
                references: references
                    .iter()
                    .map(|reference| (*reference).to_string())
                    .collect(),
                activity: Some("Implement fix".to_string()),
                extra: std::collections::BTreeMap::new(),
            })
            .unwrap()
            .unwrap()
        };

        assert_eq!(
            render("Parser #317alpha", &["#317"]),
            "Parser #317alpha - Implement fix"
        );
        assert_eq!(
            render("Parser #317_foo", &["#317"]),
            "Parser #317_foo - Implement fix"
        );
        assert_eq!(
            render("Parser #317alpha", &["#317", "#42"]),
            "Parser #317alpha #42 - Implement fix"
        );
        assert_eq!(
            render("Parser #317", &["#317"]),
            "Parser #317 - Implement fix"
        );
    }

    #[test]
    fn structured_title_normalization_matches_javascript_whitespace_contract() {
        let title = super::render_session_title_state(&super::SessionTitleState {
            topic: Some("Alpha\u{0085}Beta\u{00a0}Gamma".to_string()),
            topic_source: super::SessionTitleTopicSource::User,
            references: Vec::new(),
            activity: None,
            extra: std::collections::BTreeMap::new(),
        })
        .unwrap()
        .unwrap();

        assert_eq!(title, "Alpha\u{0085}Beta Gamma");
    }

    #[test]
    fn structured_title_normalization_stops_at_the_character_limit() {
        let consumed = std::cell::Cell::new(0usize);
        let input = std::iter::repeat_n('x', 1_000_000).inspect(|_| {
            consumed.set(consumed.get() + 1);
        });

        let err = super::normalize_title_state_component_chars(input, "topic").unwrap_err();

        assert_eq!(err.0.code, "invalid-title-state");
        assert_eq!(consumed.get(), super::SESSION_TITLE_MAX_CHARS + 1);
    }

    #[test]
    fn structured_title_references_use_javascript_edge_whitespace() {
        let make_state = |reference: &str| super::SessionTitleState {
            topic: None,
            topic_source: super::SessionTitleTopicSource::None,
            references: vec![reference.to_string()],
            activity: None,
            extra: std::collections::BTreeMap::new(),
        };

        let err = super::normalize_title_state(make_state("\u{0085}#317\u{0085}"))
            .expect_err("JavaScript preserves U+0085, so the strict reference grammar rejects it");
        assert_eq!(err.0.code, "invalid-title-state");

        let normalized = super::normalize_title_state(make_state("\u{00a0}#317\u{00a0}"))
            .expect("JavaScript trims non-breaking space");
        assert_eq!(normalized.references, vec!["#317"]);
    }

    #[test]
    fn structured_title_pair_preserves_edge_u0085_like_javascript() {
        let edge_title = "\u{0085}Alpha\u{0085}";
        let result = super::canonicalize_structured_title_pair(
            Some(edge_title.to_string()),
            true,
            super::SessionTitleState {
                topic: Some(edge_title.to_string()),
                topic_source: super::SessionTitleTopicSource::User,
                references: Vec::new(),
                activity: None,
                extra: std::collections::BTreeMap::new(),
            },
        );

        let (title, _) = result.expect("JavaScript-compatible edge whitespace should match");
        assert_eq!(title.as_deref(), Some(edge_title));
    }

    #[test]
    fn create_record_rejects_a_mismatched_structured_title_pair() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = test_context(tmp.path());
        let result = create_record(RecordRequest {
            context: &context,
            agent: AgentKind::Codex,
            mode: "interactive",
            coordination_mode: crate::cli::CoordinationMode::Advisory,
            title: Some("Unrelated title"),
            title_state: Some(super::SessionTitleState {
                topic: Some("Canonical topic".to_string()),
                topic_source: super::SessionTitleTopicSource::User,
                references: Vec::new(),
                activity: None,
                extra: std::collections::BTreeMap::new(),
            }),
            explicit_id: Some("mismatched-title-state"),
            cwd: Path::new("/repo"),
            prompt: None,
            log_file_name: None,
            provider_resume: None,
            agent_args: Vec::new(),
            agent_bin: None,
        });

        let err = match result {
            Ok(_) => panic!("mismatched structured title must be rejected"),
            Err(err) => err,
        };
        assert_eq!(err.0.code, "title-state-mismatch");
        assert!(!super::session_dir(&context, "mismatched-title-state").exists());
    }

    #[test]
    fn create_record_preserves_title_only_input_contract() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = test_context(tmp.path());
        let padded = create_record(RecordRequest {
            context: &context,
            agent: AgentKind::Codex,
            mode: "interactive",
            coordination_mode: crate::cli::CoordinationMode::Advisory,
            title: Some("  padded title-only input  "),
            title_state: None,
            explicit_id: Some("padded-title-only"),
            cwd: Path::new("/repo"),
            prompt: None,
            log_file_name: None,
            provider_resume: None,
            agent_args: Vec::new(),
            agent_bin: None,
        })
        .unwrap();
        assert_eq!(
            padded.record.title.as_deref(),
            Some("  padded title-only input  ")
        );
        drop(padded);

        let long_title = "x".repeat(121);
        let long = create_record(RecordRequest {
            context: &context,
            agent: AgentKind::Codex,
            mode: "interactive",
            coordination_mode: crate::cli::CoordinationMode::Advisory,
            title: Some(&long_title),
            title_state: None,
            explicit_id: Some("long-title-only"),
            cwd: Path::new("/repo"),
            prompt: None,
            log_file_name: None,
            provider_resume: None,
            agent_args: Vec::new(),
            agent_bin: None,
        })
        .unwrap();
        assert_eq!(long.record.title.as_deref(), Some(long_title.as_str()));
    }

    #[test]
    fn durable_title_state_preserves_future_fields_across_rewrites() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = test_context(tmp.path());
        let created = create_record(RecordRequest {
            context: &context,
            agent: AgentKind::Codex,
            mode: "interactive",
            coordination_mode: crate::cli::CoordinationMode::Advisory,
            title: Some("Topic"),
            title_state: Some(super::SessionTitleState {
                topic: Some("Topic".to_string()),
                topic_source: super::SessionTitleTopicSource::User,
                references: Vec::new(),
                activity: None,
                extra: std::collections::BTreeMap::new(),
            }),
            explicit_id: Some("future-title-state"),
            cwd: Path::new("/repo"),
            prompt: None,
            log_file_name: None,
            provider_resume: None,
            agent_args: Vec::new(),
            agent_bin: None,
        })
        .unwrap();
        drop(created);

        let record_path = super::session_dir(&context, "future-title-state").join("session.json");
        let mut raw: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&record_path).unwrap()).unwrap();
        raw["title_state"]["future_field"] = serde_json::json!({ "enabled": true });
        fs::write(&record_path, serde_json::to_vec_pretty(&raw).unwrap()).unwrap();

        let mut record = super::load_session_record(&context, "future-title-state").unwrap();
        let projected = serde_json::to_value(
            super::effective_session_title_state(&record).expect("known title state projects"),
        )
        .unwrap();
        assert!(projected.get("future_field").is_none());
        record.updated_at = "2099-01-01T00:00:00Z".to_string();
        super::write_session_record(&context, &record).unwrap();

        let rewritten: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&record_path).unwrap()).unwrap();
        assert_eq!(
            rewritten["title_state"]["future_field"],
            serde_json::json!({ "enabled": true })
        );
    }

    fn create_test_record_id(
        context: &CliContext,
        agent: AgentKind,
        title: Option<&str>,
        explicit_id: Option<&str>,
    ) -> String {
        create_record(RecordRequest {
            context,
            agent,
            mode: "interactive",
            coordination_mode: crate::cli::CoordinationMode::Advisory,
            title,
            title_state: None,
            explicit_id,
            cwd: Path::new("/repo"),
            prompt: None,
            log_file_name: None,
            provider_resume: None,
            agent_args: Vec::new(),
            agent_bin: None,
        })
        .unwrap()
        .record
        .id
    }

    #[test]
    fn maintenance_resume_actions_reject_a_session_owned_worker_quarantine() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = test_context(tmp.path());
        let id = create_test_record_id(
            &context,
            AgentKind::Codex,
            None,
            Some("maintenance-quarantined-worker"),
        );
        let record = load_session_record(&context, &id).unwrap();
        let runtime_before = serde_json::to_value(&record.runtime).unwrap();
        let quarantine = crate::orchestration::WorkerQuarantineRecord {
            schema_version: crate::orchestration::WORKER_QUARANTINE_SCHEMA.to_string(),
            worker: crate::orchestration::SessionRef {
                machine: None,
                session_id: record.id.clone(),
                session_incarnation: "stopped-worker-incarnation".to_string(),
                session_created_at: record.created_at.clone(),
            },
            reason: "stopped runtime reconciled without a worker checkpoint".to_string(),
            runtime_identity_digest: format!("sha256:{}", "a".repeat(64)),
            created_at: "2030-01-01T00:00:00Z".to_string(),
        };
        crate::orchestration::persist_session_authority_quarantine(
            &context,
            "assignment-maintenance-quarantine",
            4,
            &quarantine,
        )
        .unwrap();
        let mut retry = quarantine;
        retry.created_at = "2031-01-01T00:00:00Z".to_string();
        let adopted = crate::orchestration::persist_session_authority_quarantine(
            &context,
            "assignment-maintenance-quarantine",
            4,
            &retry,
        )
        .unwrap();
        assert_eq!(
            adopted.created_at, "2030-01-01T00:00:00Z",
            "retry must adopt the durable quarantine timestamp after a pre-commit crash"
        );
        let false_bin = super::binary_on_path("false").expect("false executable");
        let view = session_view(
            &context,
            &record,
            Some("stopped".to_string()),
            Some(&false_bin),
        );
        assert!(!view.resumable);
        assert_eq!(
            view.resume_blocked_reason.as_deref(),
            Some("worker-quarantined")
        );
        let preview = crate::maintenance::preview(
            &context,
            &id,
            &false_bin,
            crate::maintenance::MaintenanceOperation::Resume,
        )
        .unwrap();
        for (action, confirmed) in [
            (crate::maintenance::MaintenanceActionId::RetryResume, false),
            (
                crate::maintenance::MaintenanceActionId::TerminateRuntimeThenResume,
                true,
            ),
        ] {
            let error = crate::maintenance::execute_with_resume_guard(
                &context,
                &id,
                &false_bin,
                crate::maintenance::MaintenanceActionRequest {
                    operation: crate::maintenance::MaintenanceOperation::Resume,
                    action,
                    expected_session_incarnation: preview.session_incarnation.clone(),
                    expected_session_generation: preview.session_generation,
                    expected_preview_digest: preview.preview_digest.clone(),
                    confirmed,
                },
                |_| panic!("quarantine must reject before the caller resume guard"),
            )
            .expect_err("maintenance resume must remain quarantined");
            assert_eq!(error.code(), "worker-quarantined");
        }
        assert_eq!(
            serde_json::to_value(load_session_record(&context, &id).unwrap().runtime).unwrap(),
            runtime_before,
            "maintenance must not launch a new runtime generation"
        );
    }

    #[test]
    fn group_cleanup_session_fence_serializes_resume_and_blocks_broker_reprovision() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = test_context(tmp.path());
        let id = create_test_record_id(
            &context,
            AgentKind::Codex,
            None,
            Some("group-cleanup-fenced-worker"),
        );
        let mut record = load_session_record(&context, &id).unwrap();
        record.runtime = Some(super::RuntimeInfo {
            kind: "tmux".to_string(),
            tmux_session: record.tmux_session.clone(),
            generation: 1,
            started_at: "2030-01-01T00:00:00Z".to_string(),
            launch_id: "worker-incarnation".to_string(),
            extra: std::collections::BTreeMap::new(),
        });
        write_session_record(&context, &record).unwrap();
        let worker = crate::orchestration::SessionRef {
            machine: None,
            session_id: record.id.clone(),
            session_incarnation: "worker-incarnation".to_string(),
            session_created_at: record.created_at.clone(),
        };
        let main = crate::orchestration::SessionRef {
            machine: None,
            session_id: "main-cleanup-owner".to_string(),
            session_incarnation: "main-incarnation".to_string(),
            session_created_at: "2030-01-01T00:00:00Z".to_string(),
        };
        let locked = super::lock_exact_session_authority(&context, &id)
            .unwrap()
            .expect("worker record exists");

        let resume_context = context.clone();
        let resume_id = id.clone();
        let false_bin = super::binary_on_path("false").expect("false executable");
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let resume_thread = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            let result = super::resume_session_by_id(&resume_context, &resume_id, &false_bin);
            done_tx.send(result).unwrap();
        });
        started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("resume contender starts");
        assert!(
            done_rx.recv_timeout(Duration::from_millis(100)).is_err(),
            "resume must remain blocked while group cleanup owns the exact session record lock"
        );

        let first = crate::orchestration::persist_session_group_cleanup_fence(
            &context,
            &worker,
            &main,
            "run-cleanup",
            &format!("sha256:{}", "a".repeat(64)),
        )
        .unwrap();
        let retry = crate::orchestration::persist_session_group_cleanup_fence(
            &context,
            &worker,
            &main,
            "run-cleanup",
            &format!("sha256:{}", "a".repeat(64)),
        )
        .unwrap();
        assert_eq!(
            retry, first,
            "an interrupted cleanup retry must adopt the durable fence"
        );
        drop(locked);

        let resume_error = done_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("resume unblocks after cleanup releases the record lock")
            .expect_err("durable cleanup fence rejects resume");
        assert_eq!(resume_error.code(), "worker-group-cleanup-fenced");
        resume_thread.join().unwrap();
        let broker_error = crate::coordination::broker::provision(&context, &record)
            .expect_err("durable cleanup fence rejects direct broker reprovision");
        assert_eq!(broker_error.code(), "worker-group-cleanup-fenced");
        assert!(
            !crate::coordination::capability_path(&context, &record.id, "worker-incarnation")
                .exists(),
            "fenced broker reprovision must not create credentials"
        );
    }

    struct TestProcessGroup {
        child: Option<Child>,
        process_group_id: libc::pid_t,
    }

    impl TestProcessGroup {
        fn spawn() -> Self {
            let mut command = Command::new("sleep");
            command.arg("30");
            // SAFETY: this test-only child must own a dedicated process session.
            unsafe {
                command.pre_exec(|| {
                    if libc::setsid() < 0 {
                        Err(io::Error::last_os_error())
                    } else {
                        Ok(())
                    }
                });
            }
            let child = command.spawn().expect("spawn test process session");
            let process_group_id = child.id() as libc::pid_t;
            Self {
                child: Some(child),
                process_group_id,
            }
        }

        fn pid(&self) -> u32 {
            self.child.as_ref().expect("live test child").id()
        }

        fn stop(&mut self) {
            let Some(mut child) = self.child.take() else {
                return;
            };
            // SAFETY: the child is the leader of a dedicated test-only process group.
            unsafe {
                libc::kill(-self.process_group_id, libc::SIGKILL);
            }
            let _ = child.wait();
        }
    }

    impl Drop for TestProcessGroup {
        fn drop(&mut self) {
            self.stop();
        }
    }

    struct ReapedTestProcessGroup {
        process_group_id: libc::pid_t,
        descendant_process_group_id: Option<libc::pid_t>,
        reaper: Option<thread::JoinHandle<()>>,
    }

    impl ReapedTestProcessGroup {
        fn spawn() -> Self {
            let mut command = Command::new("sleep");
            command.arg("30");
            // SAFETY: this test-only child must own a dedicated process session.
            unsafe {
                command.pre_exec(|| {
                    if libc::setsid() < 0 {
                        Err(io::Error::last_os_error())
                    } else {
                        Ok(())
                    }
                });
            }
            let child = command.spawn().expect("spawn reaped test process session");
            Self::from_child(child)
        }

        #[cfg(target_os = "linux")]
        fn spawn_tree(descendant_pid: &Path) -> Self {
            Self::spawn_session_helper(descendant_pid, None, None)
        }

        #[cfg(target_os = "linux")]
        fn spawn_escaping_tree(
            descendant_pid: &Path,
            escape_trigger: &Path,
            escaped_marker: &Path,
        ) -> Self {
            Self::spawn_session_helper(descendant_pid, Some(escape_trigger), Some(escaped_marker))
        }

        #[cfg(target_os = "linux")]
        fn spawn_session_helper(
            descendant_pid: &Path,
            escape_trigger: Option<&Path>,
            escaped_marker: Option<&Path>,
        ) -> Self {
            let mut command = Command::new(env::current_exe().expect("test executable"));
            command
                .arg("--exact")
                .arg("tests::process_session_descendant_helper")
                .arg("--nocapture")
                .env("AGENT_SESSION_TEST_DESCENDANT_PID", descendant_pid)
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            if let Some(escape_trigger) = escape_trigger {
                command.env("AGENT_SESSION_TEST_ESCAPE_TRIGGER", escape_trigger);
            }
            if let Some(escaped_marker) = escaped_marker {
                command.env("AGENT_SESSION_TEST_ESCAPED_MARKER", escaped_marker);
            }
            // SAFETY: this test-only child must own a dedicated process session.
            unsafe {
                command.pre_exec(|| {
                    if libc::setsid() < 0 {
                        Err(io::Error::last_os_error())
                    } else {
                        Ok(())
                    }
                });
            }
            let child = command.spawn().expect("spawn reaped test process session");
            let mut process_group = Self::from_child(child);
            let started_at = Instant::now();
            while !descendant_pid.is_file() && started_at.elapsed() < Duration::from_secs(1) {
                thread::sleep(Duration::from_millis(10));
            }
            assert!(descendant_pid.is_file(), "descendant pid must be recorded");
            process_group.descendant_process_group_id = Some(
                fs::read_to_string(descendant_pid)
                    .unwrap()
                    .trim()
                    .parse()
                    .unwrap(),
            );
            process_group
        }

        fn from_child(mut child: Child) -> Self {
            let process_group_id = child.id() as libc::pid_t;
            let reaper = thread::spawn(move || {
                let _ = child.wait();
            });
            Self {
                process_group_id,
                descendant_process_group_id: None,
                reaper: Some(reaper),
            }
        }

        fn pid(&self) -> u32 {
            self.process_group_id as u32
        }

        #[cfg(target_os = "linux")]
        fn stop_session_leader_only(&self) {
            // SAFETY: the helper is the leader of this test-owned process group.
            // Its separately grouped descendant remains in the same POSIX session.
            unsafe {
                libc::kill(-self.process_group_id, libc::SIGKILL);
            }
        }

        fn stop(&mut self) {
            // SAFETY: the helper is the leader of a dedicated test-only process group.
            unsafe {
                libc::kill(-self.process_group_id, libc::SIGKILL);
                if let Some(descendant_process_group_id) = self.descendant_process_group_id {
                    libc::kill(-descendant_process_group_id, libc::SIGKILL);
                }
            }
            if let Some(reaper) = self.reaper.take() {
                let _ = reaper.join();
            }
        }
    }

    impl Drop for ReapedTestProcessGroup {
        fn drop(&mut self) {
            self.stop();
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn coordination_runtime_keeps_a_late_same_session_descendant_running() {
        let tmp = tempfile::TempDir::new().unwrap();
        let descendant_pid = tmp.path().join("descendant.pid");
        let mut process = ReapedTestProcessGroup::spawn_tree(&descendant_pid);
        let session_id = process.process_group_id;
        let descendant_group = process
            .descendant_process_group_id
            .expect("descendant process group");
        process.stop_session_leader_only();

        let stopped_deadline = Instant::now() + Duration::from_secs(2);
        while super::process_group_status(process.process_group_id)
            != super::ProcessGroupStatus::Stopped
        {
            assert!(
                Instant::now() < stopped_deadline,
                "session leader process group did not stop"
            );
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            unsafe { libc::getsid(descendant_group) },
            session_id,
            "late descendant must remain in the captured process session"
        );

        let identity = TmuxRuntimeIdentity {
            launch_id: Some("late-session-descendant".to_string()),
            session_id: "$late".to_string(),
            pane_id: "%late".to_string(),
            pane_pid: process.process_group_id,
            process_group_id: Some(process.process_group_id),
            process_session_id: Some(session_id),
            process_session_members: Vec::new(),
            control_group: None,
            control_group_members: Vec::new(),
        };
        assert_eq!(
            super::coordination_process_runtime_status(&identity),
            super::ProcessGroupStatus::Running,
            "a live same-session descendant must dominate the stopped leader group"
        );
        process.stop();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn coordination_runtime_evidence_combination_is_fail_closed() {
        use super::ProcessGroupStatus::{Running, Stopped, Unknown};

        assert_eq!(
            super::combine_runtime_status_evidence(&[Stopped, Running]),
            Running,
            "positive live evidence dominates a stale absence proof"
        );
        assert_eq!(
            super::combine_runtime_status_evidence(&[Stopped, Unknown]),
            Unknown,
            "unavailable evidence prevents stopped reconciliation"
        );
        assert_eq!(
            super::combine_runtime_status_evidence(&[Stopped, Stopped]),
            Stopped
        );
        assert_eq!(
            super::combine_runtime_status_evidence(&[]),
            Unknown,
            "no evidence is never proof of absence"
        );
    }

    #[test]
    fn an_unenumerated_process_group_absence_is_not_stopped_runtime_proof() {
        use super::ProcessGroupStatus::{Running, Stopped, Unknown};

        assert_eq!(
            super::conservative_coordination_process_group_status(Running),
            Running
        );
        assert_eq!(
            super::conservative_coordination_process_group_status(Stopped),
            Unknown
        );
        assert_eq!(
            super::conservative_coordination_process_group_status(Unknown),
            Unknown
        );
    }

    #[cfg(target_os = "linux")]
    struct TestTmuxServer {
        bin: PathBuf,
    }

    #[cfg(target_os = "linux")]
    impl Drop for TestTmuxServer {
        fn drop(&mut self) {
            let _ = Command::new(&self.bin)
                .arg("kill-server")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }

    #[cfg(target_os = "linux")]
    struct TestPidGuard(libc::pid_t);

    #[cfg(target_os = "linux")]
    impl Drop for TestPidGuard {
        fn drop(&mut self) {
            if self.0 > 1 {
                // SAFETY: the PID belongs to this test's dedicated detached child.
                unsafe {
                    libc::kill(self.0, libc::SIGKILL);
                }
            }
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn process_session_descendant_helper() {
        let Some(descendant_pid_path) = env::var_os("AGENT_SESSION_TEST_DESCENDANT_PID") else {
            return;
        };
        let escape = env::var_os("AGENT_SESSION_TEST_ESCAPE_TRIGGER");
        let mut command = if escape.is_some() {
            let mut command = Command::new(env::current_exe().expect("test executable"));
            command
                .arg("--exact")
                .arg("tests::process_session_escape_helper")
                .arg("--nocapture");
            command
        } else {
            let mut command = Command::new("sleep");
            command.arg("30").process_group(0);
            command
        };
        // SAFETY: the helper deliberately models an independently grouped descendant.
        unsafe {
            command.pre_exec(|| {
                libc::signal(libc::SIGTERM, libc::SIG_IGN);
                libc::signal(libc::SIGHUP, libc::SIG_IGN);
                Ok(())
            });
        }
        let mut descendant = command.spawn().expect("spawn process-session descendant");
        fs::write(descendant_pid_path, descendant.id().to_string()).expect("write descendant pid");
        let _ = descendant.wait();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn process_session_escape_helper() {
        let (Some(trigger), Some(marker)) = (
            env::var_os("AGENT_SESSION_TEST_ESCAPE_TRIGGER"),
            env::var_os("AGENT_SESSION_TEST_ESCAPED_MARKER"),
        ) else {
            return;
        };
        // SAFETY: this test helper models a captured descendant escaping its original session.
        unsafe {
            libc::signal(libc::SIGTERM, libc::SIG_IGN);
            libc::signal(libc::SIGHUP, libc::SIG_IGN);
        }
        let trigger = PathBuf::from(trigger);
        let started_at = Instant::now();
        while !trigger.is_file() && started_at.elapsed() < Duration::from_secs(5) {
            thread::sleep(Duration::from_millis(5));
        }
        assert!(trigger.is_file(), "escape trigger must arrive");
        assert!(
            unsafe { libc::setsid() } > 1,
            "descendant must escape its session"
        );
        fs::write(marker, "escaped").unwrap();
        thread::sleep(Duration::from_secs(30));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn control_group_detached_descendant_helper() {
        let (Some(descendant_trigger_path), Some(descendant_pid_path)) = (
            env::var_os("AGENT_SESSION_TEST_CGROUP_DESCENDANT_TRIGGER"),
            env::var_os("AGENT_SESSION_TEST_CGROUP_DESCENDANT_PID"),
        ) else {
            return;
        };
        let descendant_trigger_path = PathBuf::from(descendant_trigger_path);
        let started_at = Instant::now();
        while !descendant_trigger_path.is_file() && started_at.elapsed() < Duration::from_secs(5) {
            thread::sleep(Duration::from_millis(5));
        }
        assert!(
            descendant_trigger_path.is_file(),
            "descendant trigger must arrive after pane cgroup containment"
        );
        let mut command = Command::new("sleep");
        command.arg("30");
        // SAFETY: this helper models a descendant that leaves the pane's process session
        // while remaining inside the pane's dedicated cgroup.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 {
                    return Err(io::Error::last_os_error());
                }
                libc::signal(libc::SIGTERM, libc::SIG_IGN);
                libc::signal(libc::SIGHUP, libc::SIG_IGN);
                Ok(())
            });
        }
        let mut descendant = command.spawn().expect("spawn detached descendant");
        fs::write(descendant_pid_path, descendant.id().to_string())
            .expect("record detached descendant pid");
        let _ = descendant.wait();
    }

    #[cfg(target_os = "linux")]
    fn linux_scoped_process_test_capability(
        runtime_dir: Option<&std::ffi::OsStr>,
        systemd_run: Option<PathBuf>,
        probe_manager: impl FnOnce(&Path) -> bool,
    ) -> Result<PathBuf, &'static str> {
        let runtime_dir = runtime_dir
            .map(PathBuf::from)
            .ok_or("XDG_RUNTIME_DIR is unavailable")?;
        let systemd_run = systemd_run.ok_or("systemd-run is unavailable")?;
        if !runtime_dir.join("systemd/private").exists() || !probe_manager(&systemd_run) {
            return Err("the systemd user manager is unreachable");
        }
        Ok(systemd_run)
    }

    #[cfg(target_os = "linux")]
    fn probe_systemd_user_scope(systemd_run: &Path) -> bool {
        let Some(success) = super::binary_on_path("true") else {
            return false;
        };
        let mut command = Command::new(systemd_run);
        command
            .args(["--user", "--scope", "--quiet", "--collect", "--unit"])
            .arg(format!(
                "nils-agent-session-cgroup-probe-{}",
                uuid::Uuid::new_v4()
            ))
            .arg("--")
            .arg(success);
        super::run_output_with_timeout_and_cap(command, Duration::from_secs(3), 4 * 1024)
            .is_ok_and(|output| output.status.success())
    }

    #[cfg(target_os = "linux")]
    fn linux_cgroup_test_capability_or_skip(
        capability: Result<PathBuf, &'static str>,
        required: bool,
        label: &str,
    ) -> Option<PathBuf> {
        match capability {
            Ok(executable) => Some(executable),
            Err(reason) if required => {
                panic!("{label} is required but unavailable: {reason}");
            }
            Err(reason) => {
                eprintln!("SKIP: {label} unavailable: {reason}");
                None
            }
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_scoped_process_capability_is_explicitly_skippable() {
        let tmp = tempfile::TempDir::new().unwrap();
        let systemd_dir = tmp.path().join("systemd");
        fs::create_dir(&systemd_dir).unwrap();
        fs::write(systemd_dir.join("private"), "").unwrap();
        let executable = tmp.path().join("systemd-run");
        fs::write(&executable, "").unwrap();

        assert_eq!(
            linux_scoped_process_test_capability(None, Some(executable.clone()), |_| true),
            Err("XDG_RUNTIME_DIR is unavailable")
        );
        assert_eq!(
            linux_scoped_process_test_capability(Some(tmp.path().as_os_str()), None, |_| true),
            Err("systemd-run is unavailable")
        );
        assert_eq!(
            linux_scoped_process_test_capability(
                Some(tmp.path().as_os_str()),
                Some(executable.clone()),
                |_| false,
            ),
            Err("the systemd user manager is unreachable")
        );
        assert_eq!(
            linux_scoped_process_test_capability(
                Some(tmp.path().as_os_str()),
                Some(executable.clone()),
                |_| true,
            ),
            Ok(executable)
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[should_panic(expected = "required cgroup capability is required but unavailable")]
    fn linux_scoped_process_capability_fails_closed_when_ci_requires_it() {
        let _ = linux_cgroup_test_capability_or_skip(
            Err("the systemd user manager is unreachable"),
            true,
            "required cgroup capability",
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_systemd_scope_probe_executes_when_the_user_manager_is_available() {
        let required = env::var("AGENT_SESSION_TEST_REQUIRE_CGROUP").as_deref() == Ok("1");
        let capability = linux_scoped_process_test_capability(
            env::var_os("XDG_RUNTIME_DIR").as_deref(),
            super::binary_on_path("systemd-run"),
            probe_systemd_user_scope,
        );
        let _ =
            linux_cgroup_test_capability_or_skip(capability, required, "Linux systemd scope probe");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_systemd_scope_probe_kills_and_reaps_a_hung_helper() {
        let tmp = tempfile::TempDir::new().unwrap();
        let helper = tmp.path().join("systemd-run");
        let pid_file = tmp.path().join("probe.pid");
        fs::write(
            &helper,
            format!(
                "#!/bin/sh\nprintf '%s' \"$$\" > {}\nexec sleep 30\n",
                shell_words::quote(&super::display_path(&pid_file)),
            ),
        )
        .unwrap();
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).unwrap();

        let started = Instant::now();
        assert!(!probe_systemd_user_scope(&helper));
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the systemd capability probe must have a bounded deadline"
        );
        let pid = fs::read_to_string(&pid_file)
            .unwrap()
            .parse::<libc::pid_t>()
            .unwrap();
        assert_eq!(
            unsafe { libc::kill(pid, 0) },
            -1,
            "the timed-out probe process must be killed and reaped"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn delete_kills_a_detached_descendant_inside_a_pinned_tmux_control_group() {
        assert!(
            unsafe { libc::getsid(0) } > 1,
            "cgroup test must run in an isolated POSIX session"
        );
        let required = env::var("AGENT_SESSION_TEST_REQUIRE_CGROUP").as_deref() == Ok("1");
        let Some(tmux) = linux_cgroup_test_capability_or_skip(
            super::binary_on_path("tmux").ok_or("tmux is unavailable"),
            required,
            "Linux cgroup integration",
        ) else {
            return;
        };
        let Some(systemd_run) = linux_cgroup_test_capability_or_skip(
            linux_scoped_process_test_capability(
                env::var_os("XDG_RUNTIME_DIR").as_deref(),
                super::binary_on_path("systemd-run"),
                probe_systemd_user_scope,
            ),
            required,
            "Linux cgroup integration capability",
        ) else {
            return;
        };
        let tmp = tempfile::TempDir::new().unwrap();
        let context = test_context(tmp.path());
        let id = create_test_record_id(
            &context,
            AgentKind::Codex,
            None,
            Some("delete-cgroup-detached-descendant"),
        );
        let record = load_session_record(&context, &id).unwrap();
        let socket = format!("nils-delete-cgroup-{}", unsafe { libc::getpid() });
        let scope = format!("tmux-spawn-6302a262-c059-4ec9-9b93-{:012x}", unsafe {
            libc::getpid()
        });
        let wrapper = tmp.path().join("tmux-cgroup-wrapper");
        let wrapper_calls = tmp.path().join("wrapper-calls");
        let force_stopped = tmp.path().join("force-tmux-stopped");
        fs::write(
            &wrapper,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {}\nif [ -f {} ] && {{ [ \"$1\" = has-session ] || [ \"$1\" = display-message ]; }}; then printf '%s\\n' \"can't find session: stopped\" >&2; exit 1; fi\nif [ \"$1\" = new-session ]; then exec {} --user --scope --quiet --collect --unit {} -- {} -f /dev/null -L {} \"$@\"; fi\nexec {} -f /dev/null -L {} \"$@\"\n",
                shell_words::quote(&super::display_path(&wrapper_calls)),
                shell_words::quote(&super::display_path(&force_stopped)),
                shell_words::quote(&super::display_path(&systemd_run)),
                shell_words::quote(&scope),
                shell_words::quote(&super::display_path(&tmux)),
                shell_words::quote(&socket),
                shell_words::quote(&super::display_path(&tmux)),
                shell_words::quote(&socket),
            ),
        )
        .unwrap();
        fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o700)).unwrap();
        let _tmux_server = TestTmuxServer {
            bin: wrapper.clone(),
        };
        let descendant_trigger_path = tmp.path().join("detached-descendant.trigger");
        let descendant_pid_path = tmp.path().join("detached-descendant.pid");
        let runtime_id = &record.runtime.as_ref().unwrap().launch_id;
        let scope_anchor_channel =
            format!("nils-delete-cgroup-anchor-{}", unsafe { libc::getpid() });
        let mut scope_anchor = Command::new(&wrapper);
        scope_anchor
            .arg("new-session")
            .arg("-d")
            .arg("-P")
            .arg("-F")
            .arg("#{session_id}\t#{pane_id}\t#{pane_pid}")
            .arg("-s")
            .arg(&record.tmux_session)
            .arg("-e")
            .arg(format!("AGENT_SESSION_ID={}", record.id))
            .arg("-e")
            .arg(format!(
                "AGENT_SESSION_STATE_DIR={}",
                super::display_path(&context.state_dir)
            ))
            .arg("-e")
            .arg(format!("AGENT_SESSION_RUNTIME_ID={runtime_id}"))
            .arg("--")
            .arg(env::current_exe().unwrap())
            .arg("--exact")
            .arg("tests::control_group_detached_descendant_helper")
            .arg("--nocapture")
            .arg(";")
            .arg("wait-for")
            .arg(&scope_anchor_channel)
            .env(
                "AGENT_SESSION_TEST_CGROUP_DESCENDANT_TRIGGER",
                &descendant_trigger_path,
            )
            .env(
                "AGENT_SESSION_TEST_CGROUP_DESCENDANT_PID",
                &descendant_pid_path,
            )
            .process_group(0)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut scope_anchor = scope_anchor.spawn().unwrap();
        let mut output = String::new();
        let output_length = io::BufReader::new(scope_anchor.stdout.take().unwrap())
            .read_line(&mut output)
            .unwrap();
        assert!(
            output_length > 0,
            "tmux scope anchor exited before publishing pane identity: status={:?}",
            scope_anchor.try_wait()
        );
        let mut output_fields = output.trim().split('\t');
        let tmux_session_id = output_fields.next().unwrap().to_string();
        let tmux_pane_id = output_fields.next().unwrap().to_string();
        let pane_pid: libc::pid_t = output_fields.next().unwrap().parse().unwrap();
        let containment_started_at = Instant::now();
        let mut observed_control_group = None;
        let mut observed_since = Instant::now();
        let control_group = loop {
            let current = super::linux_process_control_group(pane_pid).unwrap();
            if current != observed_control_group {
                observed_control_group = current;
                observed_since = Instant::now();
            } else if observed_since.elapsed() >= Duration::from_millis(500) {
                let Some(control_group) = observed_control_group else {
                    scope_anchor.kill().unwrap();
                    scope_anchor.wait().unwrap();
                    panic!("required cgroup test must exercise a dedicated tmux cgroup path");
                };
                break control_group;
            }
            assert!(
                containment_started_at.elapsed() < Duration::from_secs(3),
                "tmux pane never reached stable cgroup containment: {observed_control_group:?}"
            );
            thread::sleep(Duration::from_millis(10));
        };
        let capture_started_at = Instant::now();
        let mut retained_identity = loop {
            match super::capture_tmux_runtime_identity(
                &context,
                &record,
                &wrapper,
                Duration::from_secs(1),
            ) {
                Ok(super::TmuxRuntimeProbe::Running(identity)) => break *identity,
                Ok(super::TmuxRuntimeProbe::Stopped) => {
                    panic!("tmux runtime must still be running")
                }
                Err(error)
                    if matches!(
                        error,
                        super::SessionTerminationFailure::RuntimeIdentityUnavailable
                            | super::SessionTerminationFailure::VerificationFailed
                    ) && capture_started_at.elapsed() < Duration::from_secs(2) =>
                {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) => {
                    let pane_session_id = unsafe { libc::getsid(pane_pid) };
                    let raw_tmux_identity = Command::new(&wrapper)
                        .env("LC_ALL", "C")
                        .arg("display-message")
                        .arg("-p")
                        .arg("-t")
                        .arg(super::managed_tmux_pane_target(&record.tmux_session))
                        .arg("#{session_id} #{pane_id} #{pane_pid}")
                        .output()
                        .map(|output| {
                            (
                                output.status,
                                String::from_utf8_lossy(&output.stdout).into_owned(),
                                String::from_utf8_lossy(&output.stderr).into_owned(),
                            )
                        });
                    let pane_session_members =
                        super::linux_process_session_members(pane_session_id).map(|members| {
                            members
                                .iter()
                                .map(|member| {
                                    (
                                        member.pid,
                                        member.session_id,
                                        member.start_time,
                                        member.zombie,
                                    )
                                })
                                .collect::<Vec<_>>()
                        });
                    panic!(
                        "tmux runtime identity did not stabilize within the fixture timeout: {error:?}; initial_tmux_session={tmux_session_id:?}; initial_tmux_pane={tmux_pane_id:?}; raw_tmux_identity={raw_tmux_identity:?}; caller_pid={}; caller_pgid={}; caller_sid={}; pane_pid={pane_pid}; pane_pgid={:?}; pane_sid={pane_session_id}; pane_members={pane_session_members:?}; pane_cgroup={:?}; pane_stat={:?}; calls={}",
                        unsafe { libc::getpid() },
                        unsafe { libc::getpgrp() },
                        unsafe { libc::getsid(0) },
                        super::process_group_id(pane_pid),
                        super::linux_process_control_group(pane_pid),
                        fs::read_to_string(format!("/proc/{pane_pid}/stat")),
                        fs::read_to_string(&wrapper_calls).unwrap_or_default(),
                    );
                }
            }
        };
        assert_eq!(retained_identity.session_id, tmux_session_id);
        assert_eq!(retained_identity.pane_id, tmux_pane_id);
        assert_eq!(
            retained_identity.control_group.as_ref(),
            Some(&control_group),
            "captured runtime identity must retain the stabilized pane cgroup"
        );
        fs::write(&descendant_trigger_path, "spawn").unwrap();
        let started_at = Instant::now();
        let descendant_pid: libc::pid_t = loop {
            if let Some(pid) = fs::read_to_string(&descendant_pid_path)
                .ok()
                .and_then(|pid| pid.trim().parse().ok())
            {
                break pid;
            }
            assert!(
                started_at.elapsed() < Duration::from_secs(2),
                "detached descendant PID was not published"
            );
            thread::sleep(Duration::from_millis(10));
        };
        let descendant_start_time = super::read_linux_process_identity(descendant_pid)
            .unwrap()
            .expect("detached descendant process identity")
            .start_time;
        let _descendant = TestPidGuard(descendant_pid);
        assert_ne!(
            unsafe { libc::getsid(descendant_pid) },
            unsafe { libc::getsid(pane_pid) },
            "the descendant must leave the pane's process session"
        );
        let membership_started = Instant::now();
        loop {
            if super::linux_process_control_group(descendant_pid)
                .unwrap()
                .as_ref()
                == Some(&control_group)
            {
                break;
            }
            assert!(
                membership_started.elapsed() < Duration::from_secs(2),
                "detached descendant never entered the pane's dedicated cgroup: pane={control_group:?}, descendant={:?}",
                super::linux_process_control_group(descendant_pid),
            );
            thread::sleep(Duration::from_millis(10));
        }
        let events_path = super::linux_control_group_full_path(Path::new(&control_group.path))
            .unwrap()
            .join("cgroup.events");
        let procs_path = events_path.with_file_name("cgroup.procs");

        let mut escaped_member = TestProcessGroup::spawn();
        let escaped_pid = escaped_member.pid() as libc::pid_t;
        let escaped_identity = super::read_linux_process_identity(escaped_pid)
            .unwrap()
            .expect("live escaped member");
        assert_ne!(
            super::linux_process_control_group(escaped_pid).unwrap(),
            Some(control_group.clone()),
            "the retained member must model a process that already left the pane cgroup"
        );
        let mut retained = load_session_record(&context, &id).unwrap();
        retained_identity.control_group_members = vec![
            TmuxProcessIdentity {
                pid: descendant_pid,
                start_time: descendant_start_time,
            },
            TmuxProcessIdentity {
                pid: escaped_pid,
                start_time: escaped_identity.start_time,
            },
        ];
        persist_tmux_runtime_identity(&mut retained, &retained_identity).unwrap();
        write_session_record(&context, &retained).unwrap();
        fs::write(&force_stopped, b"stopped").unwrap();

        let preview = crate::maintenance::preview(
            &context,
            &id,
            &wrapper,
            crate::maintenance::MaintenanceOperation::Delete,
        )
        .unwrap();
        let preview: serde_json::Value = serde_json::to_value(preview).unwrap();
        assert_eq!(preview["state"], "repairable");
        assert_eq!(preview["boundary"]["kind"], "managed_scope");
        assert!(
            preview["actions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|action| { action["id"] == "terminate_runtime_then_delete" })
        );
        let result = crate::maintenance::execute_with_resume_guard(
            &context,
            &id,
            &wrapper,
            crate::maintenance::MaintenanceActionRequest {
                operation: crate::maintenance::MaintenanceOperation::Delete,
                action: crate::maintenance::MaintenanceActionId::TerminateRuntimeThenDelete,
                expected_session_incarnation: preview["session_incarnation"]
                    .as_str()
                    .map(str::to_string),
                expected_session_generation: preview["session_generation"].as_u64(),
                expected_preview_digest: preview["preview_digest"].as_str().unwrap().to_string(),
                confirmed: true,
            },
            |_| Ok(()),
        )
        .unwrap_or_else(|error| {
            let retained = load_session_record(&context, &id).unwrap();
            let cgroup_processes = fs::read_to_string(&procs_path).map(|pids| {
                pids.lines()
                    .map(|pid| (pid.to_string(), fs::read_to_string(format!("/proc/{pid}/stat"))))
                    .collect::<Vec<_>>()
            });
            panic!(
                "delete failed: {error:?}; state={:?}; cgroup_events={:?}; cgroup_processes={cgroup_processes:?}; escaped={:?}; calls={} ",
                retained.extra.get(super::DELETE_TMUX_TERMINATION_STATE_KEY),
                fs::read_to_string(&events_path),
                fs::read_to_string(format!("/proc/{escaped_pid}/stat")),
                fs::read_to_string(&wrapper_calls).unwrap_or_default(),
            )
        });
        let result: serde_json::Value = serde_json::to_value(result).unwrap();

        assert_eq!(result["outcome"], "deleted");
        assert_eq!(result["cleanup_pending"], false);
        assert!(!session_dir(&context, &id).exists());
        let started_at = Instant::now();
        let stopped = loop {
            match super::read_linux_process_identity(descendant_pid) {
                Ok(None) => break true,
                Ok(Some(identity)) if identity.start_time != descendant_start_time => break true,
                Ok(Some(identity)) if identity.zombie => break true,
                Ok(Some(_)) => {}
                Err(_) => break false,
            }
            if started_at.elapsed() >= Duration::from_secs(2) {
                break false;
            }
            thread::sleep(Duration::from_millis(10));
        };
        assert!(
            stopped,
            "the cgroup-pinned detached child must not survive: stat={:?}, cgroup={:?}, events={:?}, calls={}",
            fs::read_to_string(format!("/proc/{descendant_pid}/stat")),
            super::linux_process_control_group_path(descendant_pid),
            fs::read_to_string(&events_path),
            fs::read_to_string(&wrapper_calls).unwrap_or_default(),
        );
        let escaped_started_at = Instant::now();
        let escaped_stopped = loop {
            match super::read_linux_process_identity(escaped_pid) {
                Ok(None) => break true,
                Ok(Some(identity)) if identity.start_time != escaped_identity.start_time => {
                    break true;
                }
                Ok(Some(identity)) if identity.zombie => break true,
                Ok(Some(_)) => {}
                Err(_) => break false,
            }
            if escaped_started_at.elapsed() >= Duration::from_secs(2) {
                break false;
            }
            thread::sleep(Duration::from_millis(10));
        };
        assert!(
            escaped_stopped,
            "a same-boot Pending retry must kill a retained member outside the cgroup"
        );
        escaped_member.stop();
        let _ = Command::new(&wrapper)
            .arg("wait-for")
            .arg("-S")
            .arg(&scope_anchor_channel)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let anchor_started_at = Instant::now();
        loop {
            if scope_anchor.try_wait().unwrap().is_some() {
                break;
            }
            if anchor_started_at.elapsed() >= Duration::from_secs(2) {
                scope_anchor.kill().unwrap();
                scope_anchor.wait().unwrap();
                panic!("the cgroup scope anchor did not exit after deletion cleanup");
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn tmux_control_group_path_requires_a_dedicated_spawn_scope() {
        assert!(super::valid_tmux_spawn_control_group_path(Path::new(
            "/user.slice/user-1000.slice/user@1000.service/app.slice/tmux-spawn-6302a262-c059-4ec9-9b93-1318f3b50a3f.scope"
        )));
        assert!(!super::valid_tmux_spawn_control_group_path(Path::new(
            "/user.slice/user-1000.slice/user@1000.service/app.slice/tmux.service"
        )));
        assert!(!super::valid_tmux_spawn_control_group_path(Path::new(
            "/user.slice/user-1000.slice/user@1000.service/app.slice/tmux-spawn-not-a-uuid.scope"
        )));
        assert!(!super::valid_tmux_spawn_control_group_path(Path::new(
            "/user.slice/../tmp/tmux-spawn-6302a262-c059-4ec9-9b93-1318f3b50a3f.scope"
        )));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn pinned_control_group_uses_verified_files_for_freeze_kill_and_status() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = Path::new(
            "/user.slice/user-1000.slice/user@1000.service/app.slice/tmux-spawn-6302a262-c059-4ec9-9b93-1318f3b50a3f.scope",
        );
        let full_path = super::linux_control_group_full_path_at_root(path, tmp.path()).unwrap();
        fs::create_dir_all(&full_path).unwrap();
        fs::write(full_path.join("cgroup.kill"), "0\n").unwrap();
        fs::write(full_path.join("cgroup.freeze"), "0\n").unwrap();
        fs::write(full_path.join("cgroup.events"), "populated 1\nfrozen 0\n").unwrap();
        fs::write(full_path.join("cgroup.procs"), "").unwrap();
        let metadata = fs::metadata(&full_path).unwrap();
        let control_group = super::TmuxControlGroupIdentity {
            path: path.to_string_lossy().into_owned(),
            device: metadata.dev(),
            inode: metadata.ino(),
            boot_id: Some(super::linux_boot_id().unwrap()),
        };
        let identity = TmuxRuntimeIdentity {
            launch_id: Some("launch-cgroup-fixture".to_string()),
            session_id: "$91".to_string(),
            pane_id: "%91".to_string(),
            pane_pid: 91,
            process_group_id: Some(91),
            process_session_id: Some(91),
            process_session_members: Vec::new(),
            control_group_members: Vec::new(),
            control_group: Some(control_group.clone()),
        };

        assert_eq!(
            super::linux_control_group_runtime_status_at_root(
                &identity,
                &control_group,
                tmp.path(),
            ),
            super::ProcessGroupStatus::Running
        );
        let mut pinned =
            super::open_pinned_linux_control_group(&control_group, &full_path).unwrap();
        let events_path = full_path.join("cgroup.events");
        let freeze_path = full_path.join("cgroup.freeze");
        let events_updater = thread::spawn(move || {
            let started_at = Instant::now();
            while started_at.elapsed() < Duration::from_secs(1) {
                if fs::read_to_string(&freeze_path).is_ok_and(|value| value.starts_with('1')) {
                    fs::write(&events_path, "populated 1\nfrozen 1\n").unwrap();
                    return;
                }
                thread::sleep(Duration::from_millis(1));
            }
            panic!("fixture never observed freeze request");
        });
        super::freeze_pinned_control_group(&mut pinned, Duration::from_secs(1)).unwrap();
        events_updater.join().unwrap();
        assert!(
            fs::read_to_string(full_path.join("cgroup.freeze"))
                .unwrap()
                .starts_with('1')
        );
        super::write_control_group_file(&pinned.kill_fd, b"1").unwrap();
        assert!(
            fs::read_to_string(full_path.join("cgroup.kill"))
                .unwrap()
                .starts_with('1')
        );

        fs::write(full_path.join("cgroup.events"), "populated 0\nfrozen 1\n").unwrap();
        assert_eq!(
            super::linux_control_group_runtime_status_at_root(
                &identity,
                &control_group,
                tmp.path(),
            ),
            super::ProcessGroupStatus::Stopped
        );
        let events_path = full_path.join("cgroup.events");
        let freeze_path = full_path.join("cgroup.freeze");
        let thaw_updater = thread::spawn(move || {
            let started_at = Instant::now();
            while started_at.elapsed() < Duration::from_secs(1) {
                if fs::read_to_string(&freeze_path).is_ok_and(|value| value.starts_with('0')) {
                    fs::write(events_path, "populated 0\nfrozen 0\n").unwrap();
                    return;
                }
                thread::sleep(Duration::from_millis(1));
            }
            panic!("fixture never observed thaw request");
        });
        drop(pinned);
        thaw_updater.join().unwrap();
        assert!(
            fs::read_to_string(full_path.join("cgroup.freeze"))
                .unwrap()
                .starts_with('0')
        );

        let replaced_path = full_path.with_extension("replaced");
        fs::rename(&full_path, &replaced_path).unwrap();
        fs::create_dir_all(&full_path).unwrap();
        fs::write(full_path.join("cgroup.events"), "populated 1\nfrozen 0\n").unwrap();
        assert_eq!(
            super::linux_control_group_runtime_status_at_root(
                &identity,
                &control_group,
                tmp.path(),
            ),
            super::ProcessGroupStatus::Unknown,
            "a replacement path with a different inode must fail closed"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn pinned_control_group_preserves_a_preexisting_frozen_state() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = Path::new(
            "/user.slice/user-1000.slice/user@1000.service/app.slice/tmux-spawn-6302a262-c059-4ec9-9b93-1318f3b50a3f.scope",
        );
        let full_path = super::linux_control_group_full_path_at_root(path, tmp.path()).unwrap();
        fs::create_dir_all(&full_path).unwrap();
        fs::write(full_path.join("cgroup.kill"), "0\n").unwrap();
        fs::write(full_path.join("cgroup.freeze"), "1\n").unwrap();
        fs::write(full_path.join("cgroup.events"), "populated 1\nfrozen 1\n").unwrap();
        fs::write(full_path.join("cgroup.procs"), "").unwrap();
        let metadata = fs::metadata(&full_path).unwrap();
        let identity = super::TmuxControlGroupIdentity {
            path: path.to_string_lossy().into_owned(),
            device: metadata.dev(),
            inode: metadata.ino(),
            boot_id: Some(super::linux_boot_id().unwrap()),
        };

        let mut pinned = super::open_pinned_linux_control_group(&identity, &full_path).unwrap();
        super::freeze_pinned_control_group(&mut pinned, Duration::from_millis(10)).unwrap();
        drop(pinned);

        assert!(
            fs::read_to_string(full_path.join("cgroup.freeze"))
                .unwrap()
                .starts_with('1'),
            "delete must not thaw a scope that was already frozen"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn pinned_control_group_resamples_freeze_ownership_before_freezing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = Path::new(
            "/user.slice/user-1000.slice/user@1000.service/app.slice/tmux-spawn-6302a262-c059-4ec9-9b93-1318f3b50a3f.scope",
        );
        let full_path = super::linux_control_group_full_path_at_root(path, tmp.path()).unwrap();
        fs::create_dir_all(&full_path).unwrap();
        fs::write(full_path.join("cgroup.kill"), "0\n").unwrap();
        fs::write(full_path.join("cgroup.freeze"), "0\n").unwrap();
        fs::write(full_path.join("cgroup.events"), "populated 1\nfrozen 0\n").unwrap();
        fs::write(full_path.join("cgroup.procs"), "").unwrap();
        let metadata = fs::metadata(&full_path).unwrap();
        let identity = super::TmuxControlGroupIdentity {
            path: path.to_string_lossy().into_owned(),
            device: metadata.dev(),
            inode: metadata.ino(),
            boot_id: Some(super::linux_boot_id().unwrap()),
        };

        let mut pinned = super::open_pinned_linux_control_group(&identity, &full_path).unwrap();
        fs::write(full_path.join("cgroup.freeze"), "1\n").unwrap();
        fs::write(full_path.join("cgroup.events"), "populated 1\nfrozen 1\n").unwrap();

        super::refresh_pinned_control_group_freeze_ownership(&mut pinned).unwrap();
        super::freeze_pinned_control_group(&mut pinned, Duration::from_millis(10)).unwrap();
        drop(pinned);

        assert!(
            fs::read_to_string(full_path.join("cgroup.freeze"))
                .unwrap()
                .starts_with('1'),
            "delete must not thaw a leaf frozen after the cgroup was pinned"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn pinned_control_group_owns_a_leaf_freeze_when_only_an_ancestor_was_frozen() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = Path::new(
            "/user.slice/user-1000.slice/user@1000.service/app.slice/tmux-spawn-6302a262-c059-4ec9-9b93-1318f3b50a3f.scope",
        );
        let full_path = super::linux_control_group_full_path_at_root(path, tmp.path()).unwrap();
        fs::create_dir_all(&full_path).unwrap();
        fs::write(full_path.join("cgroup.kill"), "0\n").unwrap();
        fs::write(full_path.join("cgroup.freeze"), "0\n").unwrap();
        fs::write(full_path.join("cgroup.events"), "populated 1\nfrozen 1\n").unwrap();
        fs::write(full_path.join("cgroup.procs"), "").unwrap();
        let metadata = fs::metadata(&full_path).unwrap();
        let identity = super::TmuxControlGroupIdentity {
            path: path.to_string_lossy().into_owned(),
            device: metadata.dev(),
            inode: metadata.ino(),
            boot_id: Some(super::linux_boot_id().unwrap()),
        };

        let mut pinned = super::open_pinned_linux_control_group(&identity, &full_path).unwrap();
        super::freeze_pinned_control_group(&mut pinned, Duration::from_millis(10)).unwrap();
        assert!(
            fs::read_to_string(full_path.join("cgroup.freeze"))
                .unwrap()
                .starts_with('1'),
            "effective ancestor freeze must not substitute for a leaf freeze request"
        );
        let events_path = full_path.join("cgroup.events");
        let freeze_path = full_path.join("cgroup.freeze");
        let thaw_updater = thread::spawn(move || {
            let started_at = Instant::now();
            while started_at.elapsed() < Duration::from_secs(1) {
                if fs::read_to_string(&freeze_path).is_ok_and(|value| value.starts_with('0')) {
                    fs::write(events_path, "populated 1\nfrozen 0\n").unwrap();
                    return;
                }
                thread::sleep(Duration::from_millis(1));
            }
            panic!("fixture never observed owned leaf thaw request");
        });
        drop(pinned);
        thaw_updater.join().unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn interrupted_cgroup_freeze_helper() {
        let (Some(state_dir), Some(id), Some(full_path)) = (
            env::var_os("AGENT_SESSION_TEST_RECOVERY_STATE_DIR"),
            env::var_os("AGENT_SESSION_TEST_RECOVERY_ID"),
            env::var_os("AGENT_SESSION_TEST_RECOVERY_CGROUP"),
        ) else {
            return;
        };
        let context = test_context(Path::new(&state_dir));
        let mut record = load_session_record(&context, id.to_str().unwrap()).unwrap();
        let identity = super::persisted_tmux_runtime_identity(&record)
            .unwrap()
            .unwrap();
        let control_group = identity.control_group.as_ref().unwrap();
        let full_path = PathBuf::from(full_path);
        let mut pinned = super::open_pinned_linux_control_group(control_group, &full_path).unwrap();
        super::set_tmux_termination_state(
            &mut record,
            super::TmuxTerminationState::FreezePending {
                thaw_on_recovery: true,
            },
        )
        .unwrap();
        write_session_record(&context, &record).unwrap();
        let events_path = full_path.join("cgroup.events");
        let freeze_path = full_path.join("cgroup.freeze");
        thread::spawn(move || {
            let started_at = Instant::now();
            while started_at.elapsed() < Duration::from_secs(3) {
                if fs::read_to_string(&freeze_path).is_ok_and(|value| value.starts_with('1')) {
                    fs::write(events_path, "populated 1\nfrozen 1\n").unwrap();
                    return;
                }
                thread::sleep(Duration::from_millis(1));
            }
            panic!("crash helper never observed freeze request");
        });
        super::freeze_pinned_control_group(&mut pinned, Duration::from_secs(1)).unwrap();
        std::process::exit(0);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn startup_recovers_a_freeze_left_by_an_interrupted_delete() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = test_context(tmp.path());
        let id = create_test_record_id(
            &context,
            AgentKind::Codex,
            None,
            Some("recover-interrupted-freeze"),
        );
        let path = Path::new(
            "/user.slice/user-1000.slice/user@1000.service/app.slice/tmux-spawn-6302a262-c059-4ec9-9b93-1318f3b50a3f.scope",
        );
        let cgroup_root = tmp.path().join("cgroup-root");
        let full_path = super::linux_control_group_full_path_at_root(path, &cgroup_root).unwrap();
        fs::create_dir_all(&full_path).unwrap();
        fs::write(full_path.join("cgroup.kill"), "0\n").unwrap();
        fs::write(full_path.join("cgroup.freeze"), "0\n").unwrap();
        fs::write(full_path.join("cgroup.events"), "populated 1\nfrozen 0\n").unwrap();
        fs::write(full_path.join("cgroup.procs"), "").unwrap();
        let metadata = fs::metadata(&full_path).unwrap();
        let mut record = load_session_record(&context, &id).unwrap();
        let identity = TmuxRuntimeIdentity {
            launch_id: Some(record.runtime.as_ref().unwrap().launch_id.clone()),
            session_id: "$91".to_string(),
            pane_id: "%91".to_string(),
            pane_pid: 91,
            process_group_id: Some(91),
            process_session_id: Some(91),
            process_session_members: Vec::new(),
            control_group_members: Vec::new(),
            control_group: Some(super::TmuxControlGroupIdentity {
                path: path.to_string_lossy().into_owned(),
                device: metadata.dev(),
                inode: metadata.ino(),
                boot_id: Some(super::linux_boot_id().unwrap()),
            }),
        };
        persist_tmux_runtime_identity(&mut record, &identity).unwrap();
        write_session_record(&context, &record).unwrap();

        let output = Command::new(env::current_exe().unwrap())
            .arg("--exact")
            .arg("tests::interrupted_cgroup_freeze_helper")
            .arg("--nocapture")
            .env("AGENT_SESSION_TEST_RECOVERY_STATE_DIR", tmp.path())
            .env("AGENT_SESSION_TEST_RECOVERY_ID", &id)
            .env("AGENT_SESSION_TEST_RECOVERY_CGROUP", &full_path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "freeze helper failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            fs::read_to_string(full_path.join("cgroup.freeze"))
                .unwrap()
                .starts_with('1'),
            "the subprocess must exit without running the cgroup guard destructor"
        );

        let events_path = full_path.join("cgroup.events");
        let freeze_path = full_path.join("cgroup.freeze");
        let thaw_updater = thread::spawn(move || {
            let started_at = Instant::now();
            while started_at.elapsed() < Duration::from_secs(1) {
                if fs::read_to_string(&freeze_path).is_ok_and(|value| value.starts_with('0')) {
                    fs::write(events_path, "populated 1\nfrozen 0\n").unwrap();
                    return;
                }
                thread::sleep(Duration::from_millis(1));
            }
            panic!("fixture never observed recovery thaw request");
        });

        super::recover_interrupted_tmux_terminations_at_cgroup_root(&context, &cgroup_root)
            .unwrap();
        thaw_updater.join().unwrap();

        assert!(
            fs::read_to_string(full_path.join("cgroup.freeze"))
                .unwrap()
                .starts_with('0')
        );
        let recovered = load_session_record(&context, &id).unwrap();
        assert!(
            !recovered
                .extra
                .contains_key(super::DELETE_TMUX_TERMINATION_STATE_KEY)
        );
    }

    #[test]
    fn startup_recovery_ignores_an_unrelated_malformed_session_record() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = test_context(tmp.path());
        let malformed_dir = context.state_dir.join("sessions/unrelated-malformed");
        fs::create_dir_all(&malformed_dir).unwrap();
        fs::write(malformed_dir.join("session.json"), "{not-json").unwrap();

        super::recover_interrupted_tmux_terminations_at_cgroup_root(
            &context,
            &tmp.path().join("missing-cgroup-root"),
        )
        .unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn startup_retains_freeze_ownership_when_thaw_cannot_be_verified() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = test_context(tmp.path());
        let id = create_test_record_id(
            &context,
            AgentKind::Codex,
            None,
            Some("retain-interrupted-freeze"),
        );
        let path = Path::new(
            "/user.slice/user-1000.slice/user@1000.service/app.slice/tmux-spawn-6302a262-c059-4ec9-9b93-1318f3b50a3f.scope",
        );
        let cgroup_root = tmp.path().join("cgroup-root");
        let full_path = super::linux_control_group_full_path_at_root(path, &cgroup_root).unwrap();
        fs::create_dir_all(&full_path).unwrap();
        fs::write(full_path.join("cgroup.kill"), "0\n").unwrap();
        fs::write(full_path.join("cgroup.freeze"), "1\n").unwrap();
        fs::write(full_path.join("cgroup.events"), "populated 1\nfrozen 1\n").unwrap();
        fs::write(full_path.join("cgroup.procs"), "").unwrap();
        let metadata = fs::metadata(&full_path).unwrap();
        let mut record = load_session_record(&context, &id).unwrap();
        let launch_id = record.runtime.as_ref().unwrap().launch_id.clone();
        persist_tmux_runtime_identity(
            &mut record,
            &TmuxRuntimeIdentity {
                launch_id: Some(launch_id),
                session_id: "$91".to_string(),
                pane_id: "%91".to_string(),
                pane_pid: 91,
                process_group_id: Some(91),
                process_session_id: Some(91),
                process_session_members: Vec::new(),
                control_group_members: Vec::new(),
                control_group: Some(super::TmuxControlGroupIdentity {
                    path: path.to_string_lossy().into_owned(),
                    device: metadata.dev(),
                    inode: metadata.ino(),
                    boot_id: Some(super::linux_boot_id().unwrap()),
                }),
            },
        )
        .unwrap();
        super::set_tmux_termination_state(
            &mut record,
            super::TmuxTerminationState::FreezePending {
                thaw_on_recovery: true,
            },
        )
        .unwrap();
        write_session_record(&context, &record).unwrap();

        assert!(
            super::recover_interrupted_tmux_terminations_at_cgroup_root(&context, &cgroup_root,)
                .is_err()
        );
        let retained = load_session_record(&context, &id).unwrap();
        assert!(
            retained
                .extra
                .contains_key(super::DELETE_TMUX_TERMINATION_STATE_KEY),
            "unverified thaw must retain durable recovery ownership"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn startup_recovery_covers_pending_confirmed_and_prefrozen_states() {
        let cases = [
            (
                super::TmuxTerminationState::Pending {
                    thaw_on_recovery: true,
                },
                Some(super::TmuxTerminationState::Pending {
                    thaw_on_recovery: false,
                }),
                true,
                false,
            ),
            (
                super::TmuxTerminationState::KillConfirmed {
                    thaw_on_recovery: true,
                },
                Some(super::TmuxTerminationState::KillConfirmed {
                    thaw_on_recovery: false,
                }),
                true,
                true,
            ),
            (
                super::TmuxTerminationState::FreezePending {
                    thaw_on_recovery: false,
                },
                None,
                false,
                false,
            ),
        ];

        for (index, (state, expected_state, expect_thaw, expect_kill)) in
            cases.into_iter().enumerate()
        {
            let tmp = tempfile::TempDir::new().unwrap();
            let context = test_context(tmp.path());
            let id = create_test_record_id(
                &context,
                AgentKind::Codex,
                None,
                Some(&format!("recover-state-{index}")),
            );
            let path = Path::new(
                "/user.slice/user-1000.slice/user@1000.service/app.slice/tmux-spawn-6302a262-c059-4ec9-9b93-1318f3b50a3f.scope",
            );
            let cgroup_root = tmp.path().join("cgroup-root");
            let full_path =
                super::linux_control_group_full_path_at_root(path, &cgroup_root).unwrap();
            fs::create_dir_all(&full_path).unwrap();
            fs::write(full_path.join("cgroup.kill"), "0\n").unwrap();
            fs::write(full_path.join("cgroup.freeze"), "1\n").unwrap();
            fs::write(full_path.join("cgroup.events"), "populated 1\nfrozen 1\n").unwrap();
            fs::write(full_path.join("cgroup.procs"), "").unwrap();
            let metadata = fs::metadata(&full_path).unwrap();
            let mut record = load_session_record(&context, &id).unwrap();
            let launch_id = record.runtime.as_ref().unwrap().launch_id.clone();
            persist_tmux_runtime_identity(
                &mut record,
                &TmuxRuntimeIdentity {
                    launch_id: Some(launch_id),
                    session_id: "$91".to_string(),
                    pane_id: "%91".to_string(),
                    pane_pid: 91,
                    process_group_id: Some(91),
                    process_session_id: Some(91),
                    process_session_members: Vec::new(),
                    control_group_members: Vec::new(),
                    control_group: Some(super::TmuxControlGroupIdentity {
                        path: path.to_string_lossy().into_owned(),
                        device: metadata.dev(),
                        inode: metadata.ino(),
                        boot_id: Some(super::linux_boot_id().unwrap()),
                    }),
                },
            )
            .unwrap();
            super::set_tmux_termination_state(&mut record, state).unwrap();
            write_session_record(&context, &record).unwrap();

            let thaw_updater = expect_thaw.then(|| {
                let events_path = full_path.join("cgroup.events");
                let freeze_path = full_path.join("cgroup.freeze");
                thread::spawn(move || {
                    let started_at = Instant::now();
                    while started_at.elapsed() < Duration::from_secs(2) {
                        if fs::read_to_string(&freeze_path)
                            .is_ok_and(|value| value.starts_with('0'))
                        {
                            fs::write(events_path, "populated 1\nfrozen 0\n").unwrap();
                            return;
                        }
                        thread::sleep(Duration::from_millis(1));
                    }
                    panic!("fixture never observed state-recovery thaw request");
                })
            });

            super::recover_interrupted_tmux_terminations_at_cgroup_root(&context, &cgroup_root)
                .unwrap();
            if let Some(thaw_updater) = thaw_updater {
                thaw_updater.join().unwrap();
            }
            let recovered = load_session_record(&context, &id).unwrap();
            assert_eq!(
                super::persisted_tmux_termination_state(&recovered).unwrap(),
                expected_state
            );
            assert_eq!(
                fs::read_to_string(full_path.join("cgroup.kill"))
                    .unwrap()
                    .starts_with('1'),
                expect_kill
            );
            assert_eq!(
                fs::read_to_string(full_path.join("cgroup.freeze"))
                    .unwrap()
                    .starts_with('0'),
                expect_thaw
            );
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn startup_kill_confirmed_recovery_signals_a_persisted_member_outside_the_cgroup() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = test_context(tmp.path());
        let id = create_test_record_id(
            &context,
            AgentKind::Codex,
            None,
            Some("recover-migrated-cgroup-member"),
        );
        let path = Path::new(
            "/user.slice/user-1000.slice/user@1000.service/app.slice/tmux-spawn-6302a262-c059-4ec9-9b93-1318f3b50a3f.scope",
        );
        let cgroup_root = tmp.path().join("cgroup-root");
        let full_path = super::linux_control_group_full_path_at_root(path, &cgroup_root).unwrap();
        fs::create_dir_all(&full_path).unwrap();
        fs::write(full_path.join("cgroup.kill"), "0\n").unwrap();
        fs::write(full_path.join("cgroup.freeze"), "0\n").unwrap();
        fs::write(full_path.join("cgroup.events"), "populated 0\nfrozen 0\n").unwrap();
        fs::write(full_path.join("cgroup.procs"), "").unwrap();
        let metadata = fs::metadata(&full_path).unwrap();
        let mut pane = TestProcessGroup::spawn();
        let pane_identity = super::read_linux_process_identity(pane.pid() as libc::pid_t)
            .unwrap()
            .expect("live test process");
        let member = TmuxProcessIdentity {
            pid: pane_identity.pid,
            start_time: pane_identity.start_time,
        };
        let mut record = load_session_record(&context, &id).unwrap();
        let identity = TmuxRuntimeIdentity {
            launch_id: Some(record.runtime.as_ref().unwrap().launch_id.clone()),
            session_id: "$91".to_string(),
            pane_id: "%91".to_string(),
            pane_pid: member.pid,
            process_group_id: Some(pane.process_group_id),
            process_session_id: Some(pane_identity.session_id),
            process_session_members: Vec::new(),
            control_group_members: vec![member.clone()],
            control_group: Some(super::TmuxControlGroupIdentity {
                path: path.to_string_lossy().into_owned(),
                device: metadata.dev(),
                inode: metadata.ino(),
                boot_id: Some(super::linux_boot_id().unwrap()),
            }),
        };
        persist_tmux_runtime_identity(&mut record, &identity).unwrap();
        super::set_tmux_termination_state(
            &mut record,
            super::TmuxTerminationState::KillConfirmed {
                thaw_on_recovery: false,
            },
        )
        .unwrap();
        write_session_record(&context, &record).unwrap();

        super::recover_interrupted_tmux_terminations_at_cgroup_root(&context, &cgroup_root)
            .unwrap();

        assert!(
            fs::read_to_string(full_path.join("cgroup.kill"))
                .unwrap()
                .starts_with('1')
        );
        let started_at = Instant::now();
        loop {
            match super::read_linux_process_identity(member.pid).unwrap() {
                None => break,
                Some(current) if current.zombie => break,
                Some(_) if started_at.elapsed() < Duration::from_secs(1) => {
                    thread::sleep(Duration::from_millis(5));
                }
                Some(_) => panic!("persisted migrated process survived recovery"),
            }
        }
        assert_eq!(
            super::linux_control_group_runtime_status_at_root(
                &identity,
                identity.control_group.as_ref().unwrap(),
                &cgroup_root,
            ),
            super::ProcessGroupStatus::Stopped
        );
        let recovered = load_session_record(&context, &id).unwrap();
        assert_eq!(
            super::persisted_tmux_termination_state(&recovered).unwrap(),
            Some(super::TmuxTerminationState::KillConfirmed {
                thaw_on_recovery: false,
            })
        );
        pane.stop();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn startup_recovery_never_signals_a_persisted_member_from_another_boot() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = test_context(tmp.path());
        let id = create_test_record_id(
            &context,
            AgentKind::Codex,
            None,
            Some("ignore-cross-boot-cgroup-member"),
        );
        let mut pane = TestProcessGroup::spawn();
        let pane_identity = super::read_linux_process_identity(pane.pid() as libc::pid_t)
            .unwrap()
            .expect("live test process");
        let member = TmuxProcessIdentity {
            pid: pane_identity.pid,
            start_time: pane_identity.start_time,
        };
        let other_boot_id = "00000000-0000-0000-0000-000000000000";
        assert_ne!(super::linux_boot_id().unwrap(), other_boot_id);
        let mut record = load_session_record(&context, &id).unwrap();
        let identity = TmuxRuntimeIdentity {
            launch_id: Some(record.runtime.as_ref().unwrap().launch_id.clone()),
            session_id: "$91".to_string(),
            pane_id: "%91".to_string(),
            pane_pid: member.pid,
            process_group_id: Some(pane.process_group_id),
            process_session_id: Some(pane_identity.session_id),
            process_session_members: Vec::new(),
            control_group_members: vec![member],
            control_group: Some(super::TmuxControlGroupIdentity {
                path: "/user.slice/user-1000.slice/user@1000.service/app.slice/tmux-spawn-6302a262-c059-4ec9-9b93-1318f3b50a3f.scope".to_string(),
                device: 1,
                inode: 1,
                boot_id: Some(other_boot_id.to_string()),
            }),
        };
        persist_tmux_runtime_identity(&mut record, &identity).unwrap();
        super::set_tmux_termination_state(
            &mut record,
            super::TmuxTerminationState::KillConfirmed {
                thaw_on_recovery: false,
            },
        )
        .unwrap();
        write_session_record(&context, &record).unwrap();

        super::recover_interrupted_tmux_terminations_at_cgroup_root(
            &context,
            &tmp.path().join("missing-cgroup-root"),
        )
        .unwrap();

        assert_eq!(unsafe { libc::kill(pane.pid() as libc::pid_t, 0) }, 0);
        let recovered = load_session_record(&context, &id).unwrap();
        assert_eq!(
            super::persisted_tmux_termination_state(&recovered).unwrap(),
            Some(super::TmuxTerminationState::KillConfirmed {
                thaw_on_recovery: false,
            })
        );
        pane.stop();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn cross_boot_pending_recovery_converges_without_signaling_a_reused_identity() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = test_context(tmp.path());
        let id = create_test_record_id(
            &context,
            AgentKind::Codex,
            None,
            Some("converge-cross-boot-pending"),
        );
        let mut pane = TestProcessGroup::spawn();
        let pane_identity = super::read_linux_process_identity(pane.pid() as libc::pid_t)
            .unwrap()
            .expect("live test process");
        let other_boot_id = "00000000-0000-0000-0000-000000000000";
        assert_ne!(super::linux_boot_id().unwrap(), other_boot_id);
        let mut record = load_session_record(&context, &id).unwrap();
        let launch_id = record.runtime.as_ref().unwrap().launch_id.clone();
        persist_tmux_runtime_identity(
            &mut record,
            &TmuxRuntimeIdentity {
                launch_id: Some(launch_id),
                session_id: "$91".to_string(),
                pane_id: "%91".to_string(),
                pane_pid: pane_identity.pid,
                process_group_id: Some(pane.process_group_id),
                process_session_id: Some(pane_identity.session_id),
                process_session_members: Vec::new(),
                control_group_members: vec![TmuxProcessIdentity {
                    pid: pane_identity.pid,
                    start_time: pane_identity.start_time,
                }],
                control_group: Some(super::TmuxControlGroupIdentity {
                    path: "/user.slice/user-1000.slice/user@1000.service/app.slice/tmux-spawn-6302a262-c059-4ec9-9b93-1318f3b50a3f.scope".to_string(),
                    device: 1,
                    inode: 1,
                    boot_id: Some(other_boot_id.to_string()),
                }),
            },
        )
        .unwrap();
        super::set_tmux_termination_state(
            &mut record,
            super::TmuxTerminationState::Pending {
                thaw_on_recovery: true,
            },
        )
        .unwrap();
        write_session_record(&context, &record).unwrap();

        super::recover_interrupted_tmux_terminations_at_cgroup_root(
            &context,
            &tmp.path().join("missing-cgroup-root"),
        )
        .unwrap();
        assert_eq!(unsafe { libc::kill(pane.pid() as libc::pid_t, 0) }, 0);
        let recovered = load_session_record(&context, &id).unwrap();
        assert_eq!(
            super::persisted_tmux_termination_state(&recovered).unwrap(),
            Some(super::TmuxTerminationState::KillConfirmed {
                thaw_on_recovery: false,
            })
        );

        let tmux = tmp.path().join("tmux-cross-boot-stopped");
        fs::write(
            &tmux,
            "#!/bin/sh\nprintf '%s\n' \"can't find session: $91\" >&2\nexit 1\n",
        )
        .unwrap();
        fs::set_permissions(&tmux, fs::Permissions::from_mode(0o700)).unwrap();
        let result = delete_session_with_timeouts(
            &context,
            &id,
            tmux,
            Duration::from_millis(50),
            Duration::from_millis(250),
        )
        .unwrap();

        assert!(result.deleted);
        assert!(!session_dir(&context, &id).exists());
        assert_eq!(unsafe { libc::kill(pane.pid() as libc::pid_t, 0) }, 0);
        pane.stop();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_process_pin_rejects_a_reused_numeric_pid_identity() {
        let pane = TestProcessGroup::spawn();
        let mut expected = super::read_linux_process_identity(pane.pid() as libc::pid_t)
            .unwrap()
            .expect("live process identity");
        expected.start_time = expected.start_time.saturating_add(1);
        let captured = TmuxProcessIdentity {
            pid: expected.pid,
            start_time: expected.start_time,
        };

        assert_eq!(
            super::pin_linux_process(&captured, expected.session_id).unwrap_err(),
            super::SessionTerminationFailure::VerificationFailed
        );
        assert!(
            super::pin_matching_linux_process(&captured)
                .unwrap()
                .is_none()
        );
        assert_eq!(unsafe { libc::kill(pane.pid() as libc::pid_t, 0) }, 0);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn frozen_cgroup_stabilization_accumulates_a_late_member() {
        let mut snapshots = 0;
        let pids = super::stabilize_linux_control_group_pids(
            vec![91],
            Duration::from_millis(5),
            Duration::from_millis(1),
            Instant::now() + Duration::from_millis(100),
            |_| {
                snapshots += 1;
                Ok(if snapshots == 1 {
                    vec![91, 92]
                } else {
                    vec![91, 92, 92]
                })
            },
        )
        .unwrap();

        assert!(snapshots >= 1);
        assert_eq!(pids, vec![91, 92]);
    }

    #[test]
    fn same_runtime_retry_merges_durable_control_group_members() {
        let mut current = TmuxRuntimeIdentity {
            launch_id: Some("launch-member-merge".to_string()),
            session_id: "$91".to_string(),
            pane_id: "%91".to_string(),
            pane_pid: 91,
            process_group_id: Some(91),
            process_session_id: Some(91),
            process_session_members: vec![TmuxProcessIdentity {
                pid: 91,
                start_time: 910,
            }],
            control_group_members: vec![TmuxProcessIdentity {
                pid: 92,
                start_time: 920,
            }],
            control_group: None,
        };
        let mut persisted = current.clone();
        persisted.process_session_members.push(TmuxProcessIdentity {
            pid: 94,
            start_time: 940,
        });
        persisted.control_group_members = vec![
            TmuxProcessIdentity {
                pid: 93,
                start_time: 930,
            },
            TmuxProcessIdentity {
                pid: 92,
                start_time: 920,
            },
        ];

        current.merge_process_evidence_from(&persisted);

        assert_eq!(
            current.control_group_members,
            vec![
                TmuxProcessIdentity {
                    pid: 92,
                    start_time: 920,
                },
                TmuxProcessIdentity {
                    pid: 93,
                    start_time: 930,
                },
            ]
        );
        assert_eq!(
            current.process_session_members,
            vec![TmuxProcessIdentity {
                pid: 91,
                start_time: 910,
            }],
            "fresh process-session membership must not retain exited historical members"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_process_session_pin_rejects_an_uncaptured_member() {
        let tmp = tempfile::TempDir::new().unwrap();
        let descendant_pid_path = tmp.path().join("uncaptured-descendant.pid");
        let pane = ReapedTestProcessGroup::spawn_tree(&descendant_pid_path);
        let descendant_pid: libc::pid_t = fs::read_to_string(descendant_pid_path)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let leader = super::read_linux_process_identity(pane.pid() as libc::pid_t)
            .unwrap()
            .expect("live session leader");
        let identity = TmuxRuntimeIdentity {
            launch_id: Some("launch-uncaptured-member".to_string()),
            session_id: "$91".to_string(),
            pane_id: "%91".to_string(),
            pane_pid: pane.pid() as libc::pid_t,
            process_group_id: Some(pane.process_group_id),
            process_session_id: Some(leader.session_id),
            process_session_members: vec![TmuxProcessIdentity {
                pid: leader.pid,
                start_time: leader.start_time,
            }],
            control_group_members: Vec::new(),
            control_group: None,
        };

        assert_eq!(
            super::verify_captured_linux_process_session(
                leader.session_id,
                &identity.process_session_members,
            )
            .unwrap_err(),
            super::SessionTerminationFailure::VerificationFailed
        );
        assert_eq!(unsafe { libc::kill(leader.pid, 0) }, 0);
        assert_eq!(unsafe { libc::kill(descendant_pid, 0) }, 0);
    }

    fn deletion_identity_script(
        context: &CliContext,
        record: &super::SessionRecord,
        pane_pid: u32,
    ) -> String {
        let runtime_id = &record.runtime.as_ref().expect("runtime").launch_id;
        format!(
            r#"if [ "$1" = display-message ]; then printf '$91\t%%91\t{pane_pid}\n'; exit 0; fi
if [ "$1" = show-environment ]; then
  case "$4" in
    AGENT_SESSION_ID) printf 'AGENT_SESSION_ID=%s\n' {id} ;;
    AGENT_SESSION_STATE_DIR) printf 'AGENT_SESSION_STATE_DIR=%s\n' {state_dir} ;;
    AGENT_SESSION_RUNTIME_ID) printf 'AGENT_SESSION_RUNTIME_ID=%s\n' {runtime_id} ;;
  esac
  exit 0
fi
"#,
            id = shell_words::quote(&record.id),
            state_dir = shell_words::quote(&super::display_path(&context.state_dir)),
            runtime_id = shell_words::quote(runtime_id),
        )
    }

    fn terminate_without_process_runtime_for_test(
        context: &CliContext,
        id: &str,
        tmux: &Path,
        kill_timeout: Duration,
        verify_timeout: Duration,
    ) -> Result<(), super::SessionTerminationFailure> {
        let mut record = load_session_record(context, id).unwrap();
        super::terminate_tmux_session_with_timeouts(
            context,
            &mut record,
            tmux,
            None,
            kill_timeout,
            verify_timeout,
            false,
        )
    }

    #[test]
    fn create_record_untitled_default_ids_do_not_repeat_agent_slug() {
        let tmp = tempfile::TempDir::new().unwrap();

        for (agent, slug) in [
            (AgentKind::Codex, "codex"),
            (AgentKind::Claude, "claude"),
            (AgentKind::Hermes, "hermes"),
        ] {
            let context = test_context(&tmp.path().join(slug));
            let id = create_test_record_id(&context, agent, None, None);

            assert!(
                !id.contains(&format!("{slug}-{slug}")),
                "untitled {slug} id should not repeat the agent slug: {id}"
            );
            assert_eq!(
                id.matches(slug).count(),
                1,
                "untitled {slug} id should include the agent slug once: {id}"
            );
        }
    }

    #[test]
    fn resolve_session_id_appends_collision_suffix_to_untitled_agent_base() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = test_context(tmp.path());
        let timestamp = "20260709-121932";
        let existing_id = format!("{timestamp}-codex");
        fs::create_dir_all(super::session_dir(&context, &existing_id)).unwrap();

        let id = resolve_session_id(&context, None, AgentKind::Codex, timestamp, None).unwrap();

        assert_eq!(id, format!("{existing_id}-1"));
    }

    #[test]
    fn create_record_title_derived_default_ids_keep_title_slug() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = test_context(tmp.path());

        let id = create_test_record_id(&context, AgentKind::Codex, Some("New Codex session"), None);

        assert!(
            id.ends_with("-codex-new-codex-session"),
            "title-derived id should preserve the title slug: {id}"
        );
    }

    #[test]
    fn create_record_explicit_ids_are_unchanged() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = test_context(tmp.path());

        let id = create_test_record_id(&context, AgentKind::Codex, None, Some("custom-id"));

        assert_eq!(id, "custom-id");
    }

    #[test]
    fn profiled_codex_capture_uses_only_its_persisted_history_root() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = test_context(tmp.path());
        let profile_root = tmp.path().join("profile-codex-home");
        let mut record = create_record(RecordRequest {
            context: &context,
            agent: AgentKind::Codex,
            mode: "interactive",
            coordination_mode: crate::cli::CoordinationMode::Advisory,
            title: None,
            title_state: None,
            explicit_id: Some("profiled-codex-root"),
            cwd: Path::new("/repo"),
            prompt: None,
            log_file_name: None,
            provider_resume: None,
            agent_args: Vec::new(),
            agent_bin: Some("/opt/profile-codex".to_string()),
        })
        .unwrap()
        .record;
        let runtime = record.runtime.as_mut().unwrap();
        runtime.extra.insert(
            super::AGENT_PROFILE_RUNTIME_KEY.to_string(),
            serde_json::json!("codex-profile"),
        );
        runtime.extra.insert(
            super::AGENT_PROFILE_PROVIDER_CONFIG_DIR_RUNTIME_KEY.to_string(),
            serde_json::json!(profile_root),
        );

        assert_eq!(
            super::codex_resume_history_root(&record),
            Some(profile_root.join("sessions"))
        );

        record
            .runtime
            .as_mut()
            .unwrap()
            .extra
            .remove(super::AGENT_PROFILE_PROVIDER_CONFIG_DIR_RUNTIME_KEY);
        assert_eq!(super::codex_resume_history_root(&record), None);
    }

    #[test]
    fn malformed_or_unknown_startup_metadata_is_not_projected() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = test_context(tmp.path());
        let mut record = create_record(RecordRequest {
            context: &context,
            agent: AgentKind::Codex,
            mode: "interactive",
            coordination_mode: crate::cli::CoordinationMode::Advisory,
            title: None,
            title_state: None,
            explicit_id: Some("malformed-startup"),
            cwd: Path::new("/repo"),
            prompt: None,
            log_file_name: None,
            provider_resume: None,
            agent_args: Vec::new(),
            agent_bin: None,
        })
        .unwrap()
        .record;
        record.extra.insert(
            super::STARTUP_EXTRA_KEY.to_string(),
            serde_json::json!({
                "schema_version": "agent-session.startup.v1",
                "state": "failed",
                "stage": "proxy",
                "started_at": "2000-01-01T00:00:00Z",
                "failure_code": "raw-provider-error",
                "message": "token=secret /home/user/private",
                "occurred_at": "2000-01-01T00:00:01Z",
                "retry_safe": true
            }),
        );

        assert!(super::startup_projection(&record).is_none());
    }

    fn make_managed_runtime(record: &mut super::SessionRecord, tmp: &Path) {
        let runtime = record.runtime.as_mut().unwrap();
        runtime.kind = super::codex_app_server::RUNTIME_KIND.to_string();
        runtime.extra.insert(
            super::codex_app_server::PROTOCOL_KEY.to_string(),
            serde_json::json!(super::codex_app_server::PROTOCOL_VERSION),
        );
        for (key, name) in [
            (super::codex_app_server::SOCKET_KEY, "runtime.sock"),
            (super::codex_app_server::PROXY_KEY, "runtime.proxy"),
            (
                super::codex_app_server::THREAD_HANDOFF_KEY,
                "runtime.thread",
            ),
            (
                super::codex_app_server::THREAD_ATTACHED_KEY,
                "runtime.attached",
            ),
        ] {
            runtime
                .extra
                .insert(key.to_string(), serde_json::json!(tmp.join(name)));
        }
    }

    #[test]
    fn managed_provider_stage_stays_starting_until_connection_or_failure() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = test_context(tmp.path());
        let mut created = create_record(RecordRequest {
            context: &context,
            agent: AgentKind::Codex,
            mode: "interactive",
            coordination_mode: crate::cli::CoordinationMode::Advisory,
            title: None,
            title_state: None,
            explicit_id: Some("provider-stage"),
            cwd: Path::new("/repo"),
            prompt: None,
            log_file_name: None,
            provider_resume: None,
            agent_args: Vec::new(),
            agent_bin: None,
        })
        .unwrap();
        make_managed_runtime(&mut created.record, tmp.path());
        super::write_session_record(&context, &created.record).unwrap();
        let session_dir = super::session_dir(&context, &created.record.id);
        fs::write(
            session_dir.join(super::STARTUP_STAGE_FILE),
            "provider_client\n",
        )
        .unwrap();

        let projection = super::desired_startup_projection(&context, &created.record, "running")
            .expect("provider stage should advance the starting projection");
        assert_eq!(projection.state, "starting");
        assert_eq!(projection.stage, "provider_client");

        fs::write(
            session_dir.join(super::STARTUP_FAILURE_FILE),
            "provider-client-exited\n",
        )
        .unwrap();
        let projection = super::desired_startup_projection(&context, &created.record, "stopped")
            .expect("provider exit should become a durable startup failure");
        assert_eq!(projection.state, "failed");
        assert_eq!(
            projection.failure_code.as_deref(),
            Some("provider-client-exited")
        );
    }

    #[test]
    fn startup_reconciliation_skips_owned_or_changed_lifecycles_without_a_locked_probe() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = test_context(tmp.path());
        let mut created = create_record(RecordRequest {
            context: &context,
            agent: AgentKind::Codex,
            mode: "interactive",
            coordination_mode: crate::cli::CoordinationMode::Advisory,
            title: None,
            title_state: None,
            explicit_id: Some("reconcile-race"),
            cwd: Path::new("/repo"),
            prompt: None,
            log_file_name: None,
            provider_resume: None,
            agent_args: Vec::new(),
            agent_bin: None,
        })
        .unwrap();
        make_managed_runtime(&mut created.record, tmp.path());
        super::write_session_record(&context, &created.record).unwrap();
        fs::write(
            super::session_dir(&context, &created.record.id).join(super::STARTUP_STAGE_FILE),
            "provider_client\n",
        )
        .unwrap();

        let observed = created.record.clone();
        let (record, status) =
            super::reconcile_startup_projection(&context, observed, "stopped".to_string(), None);
        assert_eq!(status, "stopped");
        assert_eq!(
            super::startup_projection(&record).unwrap().state,
            "starting"
        );

        created.record.updated_at = "2099-01-01T00:00:00Z".to_string();
        super::write_session_record(&context, &created.record).unwrap();
        drop(created);
        let (record, status) =
            super::reconcile_startup_projection(&context, record, "stopped".to_string(), None);
        assert_eq!(status, "stopped");
        assert_eq!(
            super::startup_projection(&record).unwrap().state,
            "starting"
        );

        let stale_bulk_started_at = "2000-01-01T00:00:00Z".parse::<jiff::Timestamp>().unwrap();
        let (record, status) = super::reconcile_startup_projection(
            &context,
            record,
            "stopped".to_string(),
            Some(&stale_bulk_started_at),
        );
        assert_eq!(status, "stopped");
        assert_eq!(
            super::startup_projection(&record).unwrap().state,
            "starting"
        );

        let (record, status) =
            super::reconcile_startup_projection(&context, record, "running".to_string(), None);
        assert_eq!(status, "running");
        assert_eq!(
            super::startup_projection(&record).unwrap().state,
            "starting"
        );
        assert_eq!(
            super::startup_projection(&record).unwrap().stage,
            "provider_client"
        );

        fs::write(
            super::session_dir(&context, &record.id).join(super::STARTUP_STAGE_FILE),
            "initial_connection\n",
        )
        .unwrap();
        let (record, status) =
            super::reconcile_startup_projection(&context, record, "running".to_string(), None);
        assert_eq!(status, "running");
        assert_eq!(super::startup_projection(&record).unwrap().state, "ready");
        assert_eq!(
            super::startup_projection(
                &super::load_session_record(&context, "reconcile-race").unwrap()
            )
            .unwrap()
            .state,
            "ready"
        );
    }

    #[test]
    fn startup_reconciliation_preserves_nested_future_fields_on_disk_but_not_in_views() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = test_context(tmp.path());
        let mut created = create_record(RecordRequest {
            context: &context,
            agent: AgentKind::Codex,
            mode: "interactive",
            coordination_mode: crate::cli::CoordinationMode::Advisory,
            title: None,
            title_state: None,
            explicit_id: Some("startup-future-fields"),
            cwd: Path::new("/repo"),
            prompt: None,
            log_file_name: None,
            provider_resume: None,
            agent_args: Vec::new(),
            agent_bin: None,
        })
        .unwrap();
        created.record.extra.insert(
            super::STARTUP_EXTRA_KEY.to_string(),
            serde_json::json!({
                "schema_version": "agent-session.startup.v1",
                "state": "starting",
                "stage": "record",
                "started_at": created.record.created_at,
                "future_private_state": { "keep": true }
            }),
        );
        super::write_session_record(&context, &created.record).unwrap();

        super::reconcile_owned_startup_projection(&context, &mut created.record, "running");
        let persisted: serde_json::Value = serde_json::from_slice(
            &fs::read(super::session_dir(&context, &created.record.id).join("session.json"))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            persisted["startup"]["future_private_state"],
            serde_json::json!({ "keep": true })
        );
        let view = serde_json::to_value(super::session_view(
            &context,
            &created.record,
            Some("running".to_string()),
            Some(Path::new("/bin/true")),
        ))
        .unwrap();
        assert!(view["startup"].get("future_private_state").is_none());
    }

    #[test]
    fn list_reconciles_multiple_starting_records_with_one_bulk_tmux_probe() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = test_context(tmp.path());
        let mut sessions = Vec::new();
        for id in ["bulk-starting-a", "bulk-starting-b"] {
            let created = create_record(RecordRequest {
                context: &context,
                agent: AgentKind::Codex,
                mode: "interactive",
                coordination_mode: crate::cli::CoordinationMode::Advisory,
                title: None,
                title_state: None,
                explicit_id: Some(id),
                cwd: Path::new("/repo"),
                prompt: None,
                log_file_name: None,
                provider_resume: None,
                agent_args: Vec::new(),
                agent_bin: None,
            })
            .unwrap();
            sessions.push(created.record.tmux_session.clone());
        }
        let calls = tmp.path().join("tmux.calls");
        let tmux = tmp.path().join("tmux");
        fs::write(
            &tmux,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$1\" >> {}\nif [ \"$1\" = list-windows ]; then\n  printf '%s\\t100\\n' {} {}\n  exit 0\nfi\nif [ \"$1\" = has-session ]; then exit 0; fi\nexit 1\n",
                shell_words::quote(&calls.to_string_lossy()),
                shell_words::quote(&sessions[0]),
                shell_words::quote(&sessions[1]),
            ),
        )
        .unwrap();
        fs::set_permissions(&tmux, fs::Permissions::from_mode(0o700)).unwrap();

        let views = super::list_sessions(&context, Some(&tmux)).unwrap();
        assert_eq!(views.len(), 2);
        assert_eq!(fs::read_to_string(calls).unwrap(), "list-windows\n");
    }

    #[test]
    fn lifecycle_lock_survives_session_delete_and_same_id_recreation() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = test_context(tmp.path());
        let id = "stable-lock";
        let dir = session_dir(&context, id);
        fs::create_dir_all(&dir).unwrap();
        let first = acquire_session_record_lock(&context, id).unwrap();

        fs::remove_dir_all(&dir).unwrap();
        fs::create_dir_all(&dir).unwrap();
        let (tx, rx) = mpsc::channel();
        let waiter_context = context.clone();
        let waiter = thread::spawn(move || {
            let _lock = acquire_session_record_lock(&waiter_context, id).unwrap();
            tx.send(()).unwrap();
        });
        assert!(matches!(
            rx.recv_timeout(Duration::from_millis(50)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        drop(first);
        rx.recv_timeout(Duration::from_secs(1)).unwrap();
        waiter.join().unwrap();
    }

    #[test]
    fn session_record_try_and_timed_locks_never_wait_indefinitely() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = test_context(tmp.path());
        let held = acquire_session_record_lock(&context, "bounded-lock").unwrap();

        assert!(
            try_acquire_session_record_lock(&context, "bounded-lock")
                .unwrap()
                .is_none()
        );
        let started = Instant::now();
        let error =
            acquire_session_record_lock_timed(&context, "bounded-lock", Duration::from_millis(50))
                .expect_err("timed lock should fail while held");
        assert_eq!(error.code(), "session-record-lock-timeout");
        assert!(started.elapsed() < Duration::from_secs(1));
        drop(held);
    }

    #[test]
    fn live_status_timeout_bounds_a_hung_tmux_probe() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tmux = tmp.path().join("tmux");
        fs::write(&tmux, "#!/bin/sh\nsleep 5\n").unwrap();
        fs::set_permissions(&tmux, fs::Permissions::from_mode(0o700)).unwrap();
        let started = std::time::Instant::now();

        assert_eq!(
            live_status_with_timeout(&tmux, "hung", Duration::from_millis(50)),
            "unknown"
        );
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn kill_session_timeout_bounds_a_hung_tmux_delete() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tmux = tmp.path().join("tmux");
        fs::write(&tmux, "#!/bin/sh\nsleep 5\n").unwrap();
        fs::set_permissions(&tmux, fs::Permissions::from_mode(0o700)).unwrap();
        let started = std::time::Instant::now();

        assert!(!kill_tmux_session_with_timeout(
            &tmux,
            "hung",
            Duration::from_millis(50)
        ));
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn delete_timeout_returns_structured_failure_and_retains_state() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = test_context(tmp.path());
        let id = create_test_record_id(&context, AgentKind::Codex, None, Some("delete-timeout"));
        let record = load_session_record(&context, &id).unwrap();
        let pane = TestProcessGroup::spawn();
        let tmux = tmp.path().join("tmux-timeout");
        fs::write(
            &tmux,
            format!(
                "#!/bin/sh\n{}if [ \"$1\" = if-shell ]; then exec sleep 5; fi\nexit 0\n",
                deletion_identity_script(&context, &record, pane.pid())
            ),
        )
        .unwrap();
        fs::set_permissions(&tmux, fs::Permissions::from_mode(0o700)).unwrap();

        let error = terminate_without_process_runtime_for_test(
            &context,
            &id,
            &tmux,
            Duration::from_millis(250),
            Duration::from_millis(75),
        )
        .unwrap_err();

        assert_eq!(error, super::SessionTerminationFailure::KillTimeout);
        assert!(session_dir(&context, &id).exists());
        assert_eq!(
            load_session_record(&context, &id).unwrap().tmux_session,
            record.tmux_session
        );
    }

    #[test]
    fn delete_timeout_terminates_tmux_wrapper_descendants() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = test_context(tmp.path());
        let id = create_test_record_id(
            &context,
            AgentKind::Codex,
            None,
            Some("delete-timeout-descendant"),
        );
        let record = load_session_record(&context, &id).unwrap();
        let pane = TestProcessGroup::spawn();
        let descendant_pid = tmp.path().join("descendant.pid");
        let tmux = tmp.path().join("tmux-timeout-descendant");
        fs::write(
            &tmux,
            format!(
                "#!/bin/sh\n{}if [ \"$1\" = if-shell ]; then sleep 30 & printf '%s\\n' \"$!\" > {}; wait; fi\nexit 0\n",
                deletion_identity_script(&context, &record, pane.pid()),
                shell_words::quote(&descendant_pid.to_string_lossy()),
            ),
        )
        .unwrap();
        fs::set_permissions(&tmux, fs::Permissions::from_mode(0o700)).unwrap();

        let error = terminate_without_process_runtime_for_test(
            &context,
            &id,
            &tmux,
            Duration::from_millis(50),
            Duration::from_millis(75),
        )
        .unwrap_err();

        assert_eq!(error, super::SessionTerminationFailure::KillTimeout);
        let descendant_pid: libc::pid_t = fs::read_to_string(&descendant_pid)
            .expect("descendant pid")
            .trim()
            .parse()
            .expect("numeric descendant pid");
        let started_at = Instant::now();
        let stopped = loop {
            if unsafe { libc::kill(descendant_pid, 0) } < 0 {
                break io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
            }
            if started_at.elapsed() >= Duration::from_secs(2) {
                break false;
            }
            thread::sleep(Duration::from_millis(10));
        };
        assert!(
            stopped,
            "the timed-out tmux wrapper descendant must not survive"
        );
        assert!(session_dir(&context, &id).exists());
    }

    #[test]
    fn delete_rejects_a_still_live_tmux_postcondition_and_retains_state() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = test_context(tmp.path());
        let id =
            create_test_record_id(&context, AgentKind::Claude, None, Some("delete-still-live"));
        let record = load_session_record(&context, &id).unwrap();
        let pane = ReapedTestProcessGroup::spawn();
        let tmux = tmp.path().join("tmux-still-live");
        fs::write(
            &tmux,
            format!(
                "#!/bin/sh\n{}if [ \"$1\" = if-shell ]; then /bin/kill -KILL -- -{} 2>/dev/null || true; sleep 0.05; fi\nexit 0\n",
                deletion_identity_script(&context, &record, pane.pid()),
                pane.pid(),
            ),
        )
        .unwrap();
        fs::set_permissions(&tmux, fs::Permissions::from_mode(0o700)).unwrap();

        let error = terminate_without_process_runtime_for_test(
            &context,
            &id,
            &tmux,
            Duration::from_millis(250),
            Duration::from_secs(1),
        )
        .unwrap_err();

        assert_eq!(error, super::SessionTerminationFailure::StillRunning);
        assert!(session_dir(&context, &id).exists());
    }

    #[test]
    fn delete_rejects_an_indeterminate_tmux_postcondition_and_retains_state() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = test_context(tmp.path());
        let id = create_test_record_id(&context, AgentKind::Codex, None, Some("delete-unverified"));
        let record = load_session_record(&context, &id).unwrap();
        let pane = ReapedTestProcessGroup::spawn();
        let tmux = tmp.path().join("tmux-unverified");
        fs::write(
            &tmux,
            format!(
                "#!/bin/sh\n{}if [ \"$1\" = if-shell ]; then /bin/kill -KILL -- -{} 2>/dev/null || true; sleep 0.05; exit 0; fi\nexit 42\n",
                deletion_identity_script(&context, &record, pane.pid()),
                pane.pid(),
            ),
        )
        .unwrap();
        fs::set_permissions(&tmux, fs::Permissions::from_mode(0o700)).unwrap();

        let error = terminate_without_process_runtime_for_test(
            &context,
            &id,
            &tmux,
            Duration::from_millis(250),
            Duration::from_millis(75),
        )
        .unwrap_err();

        assert_eq!(error, super::SessionTerminationFailure::VerificationFailed);
        assert!(session_dir(&context, &id).exists());
    }

    #[test]
    fn delete_retry_cleans_up_after_the_exact_tmux_session_is_stopped() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = test_context(tmp.path());
        let id = create_test_record_id(&context, AgentKind::Codex, None, Some("delete-retry"));
        let record = load_session_record(&context, &id).unwrap();
        let mut pane = ReapedTestProcessGroup::spawn();
        let tmux = tmp.path().join("tmux-retry");
        let killed = tmp.path().join("killed");
        let report_stopped = tmp.path().join("report-stopped");
        let calls = tmp.path().join("calls");
        fs::write(
            &tmux,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {calls}\nif [ \"$1\" = display-message ] && [ -f {report_stopped} ]; then printf \"%s\\n\" \"can't find session: $91\" >&2; exit 1; fi\n{identity}if [ \"$1\" = has-session ]; then\n  if [ -f {report_stopped} ]; then printf \"%s\\n\" \"can't find session: $91\" >&2; exit 1; fi\n  if [ -f {killed} ]; then exit 42; fi\n  exit 0\nfi\nif [ \"$1\" = if-shell ]; then\n  if [ -f {killed} ]; then exit 42; fi\n  : > {killed}\n  exit 0\nfi\nexit 42\n",
                calls = calls.display(),
                report_stopped = report_stopped.display(),
                killed = killed.display(),
                identity = deletion_identity_script(&context, &record, pane.pid()),
            ),
        )
        .unwrap();
        fs::set_permissions(&tmux, fs::Permissions::from_mode(0o700)).unwrap();

        let first_error = terminate_without_process_runtime_for_test(
            &context,
            &id,
            &tmux,
            Duration::from_millis(50),
            Duration::from_millis(75),
        )
        .unwrap_err();
        assert_eq!(
            first_error,
            super::SessionTerminationFailure::VerificationFailed
        );
        assert!(session_dir(&context, &id).exists());

        pane.stop();
        fs::write(&report_stopped, "stopped").unwrap();
        terminate_without_process_runtime_for_test(
            &context,
            &id,
            &tmux,
            Duration::from_millis(50),
            Duration::from_millis(75),
        )
        .unwrap();

        assert!(session_dir(&context, &id).exists());
        let calls = fs::read_to_string(calls).unwrap();
        assert_eq!(calls.matches("if-shell").count(), 1, "{calls}");
        assert!(
            calls.contains("if-shell -F -t %91") && calls.contains("kill-session -t $91"),
            "conditional kill must bind the captured immutable pane and session ids: {calls}"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn delete_fails_closed_for_live_descendants_without_a_control_group() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = test_context(tmp.path());
        let id = create_test_record_id(
            &context,
            AgentKind::Codex,
            None,
            Some("delete-runtime-descendants"),
        );
        let record = load_session_record(&context, &id).unwrap();
        let descendant_pid_path = tmp.path().join("runtime-descendant.pid");
        let pane = ReapedTestProcessGroup::spawn_tree(&descendant_pid_path);
        let descendant_pid: libc::pid_t = fs::read_to_string(&descendant_pid_path)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(unsafe { libc::getpgid(descendant_pid) }, descendant_pid);
        assert_eq!(
            unsafe { libc::getsid(descendant_pid) },
            pane.pid() as libc::pid_t
        );
        let tmux = tmp.path().join("tmux-runtime-descendants");
        let killed = tmp.path().join("tmux-killed");
        fs::write(
            &tmux,
            format!(
                "#!/bin/sh\n{identity}if [ \"$1\" = has-session ]; then\n  if [ -f {killed} ]; then printf \"%s\\n\" \"can't find session: $91\" >&2; exit 1; fi\n  exit 0\nfi\nif [ \"$1\" = if-shell ]; then : > {killed}; exit 0; fi\nexit 42\n",
                identity = deletion_identity_script(&context, &record, pane.pid()),
                killed = killed.display(),
            ),
        )
        .unwrap();
        fs::set_permissions(&tmux, fs::Permissions::from_mode(0o700)).unwrap();

        let error = delete_session_with_timeouts(
            &context,
            &id,
            tmux,
            Duration::from_millis(50),
            Duration::from_secs(1),
        )
        .unwrap_err()
        .into_inner();

        assert_eq!(
            error.details.unwrap()["reason"],
            "runtime-identity-unavailable"
        );
        assert!(session_dir(&context, &id).exists());
        assert!(
            !killed.is_file(),
            "tmux must remain untouched without a cgroup"
        );
        assert_eq!(unsafe { libc::kill(descendant_pid, 0) }, 0);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn delete_fails_closed_when_a_pinned_member_escapes_without_a_control_group() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = test_context(tmp.path());
        let id = create_test_record_id(
            &context,
            AgentKind::Codex,
            None,
            Some("delete-runtime-session-escape"),
        );
        let record = load_session_record(&context, &id).unwrap();
        let descendant_pid_path = tmp.path().join("escaping-descendant.pid");
        let escape_trigger = tmp.path().join("escape.trigger");
        let escaped_marker = tmp.path().join("escaped.marker");
        let pane = ReapedTestProcessGroup::spawn_escaping_tree(
            &descendant_pid_path,
            &escape_trigger,
            &escaped_marker,
        );
        let descendant_pid: libc::pid_t = fs::read_to_string(&descendant_pid_path)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(
            unsafe { libc::getsid(descendant_pid) },
            pane.pid() as libc::pid_t
        );
        let tmux = tmp.path().join("tmux-runtime-session-escape");
        let killed = tmp.path().join("tmux-killed");
        fs::write(
            &tmux,
            format!(
                "#!/bin/sh\n{identity}if [ \"$1\" = has-session ]; then\n  if [ -f {killed} ]; then printf \"%s\\n\" \"can't find session: $91\" >&2; exit 1; fi\n  exit 0\nfi\nif [ \"$1\" = if-shell ]; then\n  : > {escape_trigger}\n  while [ ! -f {escaped_marker} ]; do sleep 0.01; done\n  : > {killed}\n  exit 0\nfi\nexit 42\n",
                identity = deletion_identity_script(&context, &record, pane.pid()),
                killed = killed.display(),
                escape_trigger = escape_trigger.display(),
                escaped_marker = escaped_marker.display(),
            ),
        )
        .unwrap();
        fs::set_permissions(&tmux, fs::Permissions::from_mode(0o700)).unwrap();

        let error = delete_session_with_timeouts(
            &context,
            &id,
            tmux,
            Duration::from_secs(1),
            Duration::from_secs(1),
        )
        .unwrap_err()
        .into_inner();

        assert!(
            !escaped_marker.is_file(),
            "delete must fail before opening a runtime escape window"
        );
        assert_eq!(
            error.details.unwrap()["reason"],
            "runtime-identity-unavailable"
        );
        assert!(session_dir(&context, &id).exists());
        assert!(
            !killed.is_file(),
            "tmux must remain untouched without a cgroup"
        );
        assert_eq!(unsafe { libc::kill(descendant_pid, 0) }, 0);
    }

    #[test]
    fn delete_retry_finishes_from_kill_confirmed_after_process_stop() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = test_context(tmp.path());
        let id = create_test_record_id(
            &context,
            AgentKind::Codex,
            None,
            Some("delete-kill-confirmed-retry"),
        );
        let mut record = load_session_record(&context, &id).unwrap();
        let mut pane = TestProcessGroup::spawn();
        let identity = TmuxRuntimeIdentity {
            launch_id: Some(record.runtime.as_ref().unwrap().launch_id.clone()),
            session_id: "$91".to_string(),
            pane_id: "%91".to_string(),
            pane_pid: pane.pid() as libc::pid_t,
            process_group_id: Some(pane.process_group_id),
            process_session_id: super::process_session_id(pane.pid() as libc::pid_t).unwrap(),
            process_session_members: super::process_session_members(
                super::process_session_id(pane.pid() as libc::pid_t).unwrap(),
                pane.pid() as libc::pid_t,
            )
            .unwrap(),
            control_group_members: Vec::new(),
            control_group: None,
        };
        persist_tmux_runtime_identity(&mut record, &identity).unwrap();
        super::set_tmux_termination_state(
            &mut record,
            super::TmuxTerminationState::KillConfirmed {
                thaw_on_recovery: false,
            },
        )
        .unwrap();
        write_session_record(&context, &record).unwrap();
        pane.stop();
        let tmux = tmp.path().join("tmux-kill-confirmed-retry");
        fs::write(
            &tmux,
            "#!/bin/sh\nif [ \"$1\" = display-message ] || [ \"$1\" = has-session ]; then printf \"%s\\n\" \"can't find session: $91\" >&2; exit 1; fi\nexit 42\n",
        )
        .unwrap();
        fs::set_permissions(&tmux, fs::Permissions::from_mode(0o700)).unwrap();

        let result = delete_session_with_timeouts(
            &context,
            &id,
            tmux,
            Duration::from_millis(50),
            Duration::from_millis(150),
        )
        .unwrap();

        assert!(result.deleted);
        assert!(!session_dir(&context, &id).exists());
    }

    #[test]
    fn delete_never_kills_a_longer_tmux_session_matching_a_stale_prefix() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = test_context(tmp.path());
        let id = create_test_record_id(
            &context,
            AgentKind::Claude,
            None,
            Some("delete-stale-prefix"),
        );
        let mut record = load_session_record(&context, &id).unwrap();
        let mut pane = TestProcessGroup::spawn();
        let identity = TmuxRuntimeIdentity {
            launch_id: Some(record.runtime.as_ref().unwrap().launch_id.clone()),
            session_id: "$91".to_string(),
            pane_id: "%91".to_string(),
            pane_pid: pane.pid() as libc::pid_t,
            process_group_id: Some(pane.process_group_id),
            process_session_id: super::process_session_id(pane.pid() as libc::pid_t).unwrap(),
            process_session_members: super::process_session_members(
                super::process_session_id(pane.pid() as libc::pid_t).unwrap(),
                pane.pid() as libc::pid_t,
            )
            .unwrap(),
            control_group_members: Vec::new(),
            control_group: None,
        };
        persist_tmux_runtime_identity(&mut record, &identity).unwrap();
        write_session_record(&context, &record).unwrap();
        pane.stop();
        let tmux = tmp.path().join("tmux-stale-prefix");
        let killed = tmp.path().join("wrong-session-killed");
        fs::write(
            &tmux,
            format!(
                "#!/bin/sh\nif [ \"$1\" = display-message ] && [ \"$4\" = \"={}:0.0\" ]; then printf \"%s\\n\" \"can't find session: {}\" >&2; exit 1; fi\nif [ \"$1\" = has-session ] && [ \"$3\" = '$91' ]; then printf \"%s\\n\" \"can't find session: $91\" >&2; exit 1; fi\nif [ \"$1\" = kill-session ]; then : > {}; exit 0; fi\nexit 42\n",
                record.tmux_session,
                record.tmux_session,
                killed.display(),
            ),
        )
        .unwrap();
        fs::set_permissions(&tmux, fs::Permissions::from_mode(0o700)).unwrap();

        let result = delete_session_with_timeouts(
            &context,
            &id,
            tmux,
            Duration::from_millis(50),
            Duration::from_millis(75),
        )
        .unwrap();

        assert!(result.deleted);
        assert!(
            !killed.exists(),
            "a longer prefix match must never be killed"
        );
        assert!(!session_dir(&context, &id).exists());
    }

    #[test]
    fn delete_text_describes_verified_stop_instead_of_a_kill_command() {
        let result = DeleteResult {
            id: "stopped-session".to_string(),
            tmux_session: "hs-codex-stopped-session".to_string(),
            killed: true,
            deleted: true,
            session_dir: "/state/sessions/stopped-session".to_string(),
            cleanup_pending: false,
            registry_fence: SessionRegistryFence {
                session_id: "stopped-session".to_string(),
                tmux_session: "hs-codex-stopped-session".to_string(),
                runtime_launch_id: None,
                runtime_generation: None,
            },
        };

        assert_eq!(
            render_delete_text(&result),
            "deleted stopped-session (tmux stopped: yes)\n"
        );
    }

    #[test]
    fn tmux_post_spawn_wait_failure_requires_ambiguous_launch_recovery() {
        let error = super::CliError::runtime(
            "command-wait-failed",
            "tmux new-session failed after spawn",
            None,
        );

        assert!(tmux_launch_may_have_created_runtime(&error));
    }

    #[test]
    fn strip_trailing_blank_lines_preserves_content_and_internal_blanks() {
        // Trailing blank/whitespace-only lines are dropped...
        assert_eq!(
            strip_trailing_blank_lines("top-line\nsecond-line\n\n\n\n"),
            "top-line\nsecond-line"
        );
        assert_eq!(strip_trailing_blank_lines("a\nb\n   \n\t\n"), "a\nb");
        // ...but internal blank lines are preserved (only the tail is trimmed).
        assert_eq!(strip_trailing_blank_lines("a\n\nb\n\n\n"), "a\n\nb");
        // An all-blank pane collapses to empty.
        assert_eq!(strip_trailing_blank_lines("\n\n\n"), "");
        assert_eq!(strip_trailing_blank_lines(""), "");
        // Content with no trailing blanks is unchanged.
        assert_eq!(strip_trailing_blank_lines("only"), "only");
    }

    #[test]
    fn session_delete_commits_before_best_effort_partial_tombstone_cleanup() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = test_context(tmp.path());
        let session_dir = tmp.path().join("sessions").join("delete-commit");
        std::fs::create_dir_all(&session_dir).unwrap();
        std::fs::write(session_dir.join("session.json"), b"retained record").unwrap();
        std::fs::write(session_dir.join("artifact.log"), b"cleanup first").unwrap();

        let cleanup_pending = super::commit_session_directory_delete_with(
            &context,
            "delete-commit",
            &session_dir,
            |tombstone| {
                std::fs::remove_file(tombstone.join("artifact.log"))?;
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "injected cleanup failure",
                ))
            },
        )
        .expect("the atomic rename commits deletion before cleanup");
        assert!(cleanup_pending);

        assert!(
            !session_dir.exists(),
            "the live session namespace is deleted"
        );
        let tombstone_root = tmp.path().join(super::SESSION_DELETE_TOMBSTONES_DIR);
        let tombstones = std::fs::read_dir(&tombstone_root)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        assert_eq!(tombstones.len(), 1);
        assert!(tombstones[0].join("session.json").exists());
        assert!(!tombstones[0].join("artifact.log").exists());

        // A replacement may legitimately reuse the same public id after the
        // logical delete commits. Tombstone cleanup is rooted outside the live
        // namespace and must never follow the id back into that replacement.
        std::fs::create_dir_all(&session_dir).unwrap();
        std::fs::write(session_dir.join("session.json"), b"replacement record").unwrap();

        assert_eq!(
            super::cleanup_session_delete_tombstones(&context, 64).unwrap(),
            1
        );
        assert_eq!(
            std::fs::read(session_dir.join("session.json")).unwrap(),
            b"replacement record"
        );
        assert_eq!(std::fs::read_dir(tombstone_root).unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn session_delete_rejects_a_symlinked_tombstone_root_before_moving_live_metadata() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::TempDir::new().unwrap();
        let context = test_context(tmp.path());
        let session_dir = tmp.path().join("sessions").join("delete-symlink");
        std::fs::create_dir_all(&session_dir).unwrap();
        std::fs::write(session_dir.join("session.json"), b"live record").unwrap();
        let foreign_root = tmp.path().join("foreign-quarantine");
        std::fs::create_dir(&foreign_root).unwrap();
        symlink(
            &foreign_root,
            tmp.path().join(super::SESSION_DELETE_TOMBSTONES_DIR),
        )
        .unwrap();

        let error = super::commit_session_directory_delete_with(
            &context,
            "delete-symlink",
            &session_dir,
            |_| panic!("cleanup must not run when quarantine ownership is invalid"),
        )
        .expect_err("a symlinked quarantine root must fail closed");

        assert_eq!(error.code(), "session-delete-failed");
        assert_eq!(
            std::fs::read(session_dir.join("session.json")).unwrap(),
            b"live record"
        );
        assert_eq!(std::fs::read_dir(foreign_root).unwrap().count(), 0);
    }

    #[test]
    fn new_session_command_runs_tmux_directly_without_scope() {
        use super::new_session_command;
        use std::ffi::OsStr;
        use std::path::Path;

        let command = new_session_command(Path::new("/opt/tmux"), None);
        assert_eq!(command.get_program(), OsStr::new("/opt/tmux"));
        // The caller appends `new-session ...`, so the base command has no args.
        assert_eq!(command.get_args().count(), 0);
    }

    #[test]
    fn new_session_command_wraps_tmux_in_systemd_scope() {
        use super::new_session_command;
        use std::ffi::OsStr;
        use std::path::Path;

        let command = new_session_command(
            Path::new("/usr/bin/tmux"),
            Some(Path::new("/usr/bin/systemd-run")),
        );
        assert_eq!(command.get_program(), OsStr::new("/usr/bin/systemd-run"));
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(
            args,
            vec![
                OsStr::new("--user"),
                OsStr::new("--scope"),
                OsStr::new("--quiet"),
                OsStr::new("--collect"),
                OsStr::new("--"),
                OsStr::new("/usr/bin/tmux"),
            ]
        );
    }

    #[test]
    fn is_truthy_flag_accepts_common_true_values_case_insensitively() {
        use super::is_truthy_flag;

        for value in ["1", "true", "TRUE", " Yes ", "on", "On"] {
            assert!(is_truthy_flag(value), "expected truthy: {value:?}");
        }
        for value in ["0", "false", "no", "off", "", "  ", "2", "enabled"] {
            assert!(!is_truthy_flag(value), "expected falsey: {value:?}");
        }
    }

    #[test]
    fn post_paste_settle_applies_only_between_literal_paste_and_keys() {
        assert_eq!(
            super::post_paste_settle_delay(true, true),
            Some(std::time::Duration::from_millis(500))
        );
        assert_eq!(super::post_paste_settle_delay(true, false), None);
        assert_eq!(super::post_paste_settle_delay(false, true), None);
        assert_eq!(super::post_paste_settle_delay(false, false), None);
    }
}
