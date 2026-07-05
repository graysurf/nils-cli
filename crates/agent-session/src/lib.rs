mod cli;
pub mod completion;
mod serve;

use std::cmp::Reverse;
use std::collections::{BTreeMap, VecDeque};
use std::env;
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::{self, BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use clap::Parser;
use clap::error::ErrorKind;
use jiff::{Timestamp, Zoned};
use nils_common::cli_contract::{
    Envelope, EnvelopeError, OutputFormat, emit_parse_error, exit, schema_version_for,
};
use nils_common::fs::{
    SECRET_FILE_MODE, display_path, expand_home, home_dir, normalize_path, write_atomic,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use cli::{AgentKind, Cli, Command, SpecialKey};

const SESSION_DOCUMENT_VERSION: &str = "agent-session.session.v1";
const SESSION_RESUME_DOCUMENT_VERSION: &str = "agent-session.resume.v1";
const SESSION_RESUME_FILE: &str = "resume.json";
const BINARY: &str = "agent-session";
const START_COMMAND: &str = "start";
const RUN_COMMAND: &str = "run";
const LIST_COMMAND: &str = "list";
const COMMAND_COMMAND: &str = "command";
const LOGS_COMMAND: &str = "logs";
const SEND_COMMAND: &str = "send";
const GLANCE_COMMAND: &str = "glance";
const RESUME_COMMAND: &str = "resume";
const DELETE_COMMAND: &str = "delete";
const WORKDIR_USAGE_FILE: &str = "workdir-usage.json";
const CODEX_RESUME_CAPTURE_TIMEOUT_MS: u64 = 1500;
const CODEX_RESUME_CAPTURE_POLL_MS: u64 = 100;
const CODEX_RESUME_SCAN_MAX_DEPTH: usize = 6;
const CODEX_RESUME_SCAN_MAX_ENTRIES: usize = 5000;
const CODEX_RESUME_SCAN_SLICE_MS: u64 = 250;

pub fn run() -> i32 {
    run_with_args(env::args_os())
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
            return render_error("error", format, err);
        }
    };

    match cli.command {
        Command::Start(args) => run_start(&context, args),
        Command::Run(args) => run_one_shot(&context, args),
        Command::List(args) => run_list(&context, args),
        Command::Show(args) => run_command(&context, args),
        Command::Attach(args) => run_attach(&context, args),
        Command::Logs(args) => run_logs(&context, args),
        Command::Send(args) => run_send(&context, args),
        Command::Glance(args) => run_glance(&context, args),
        Command::Resume(args) => run_resume(&context, args),
        Command::Serve(args) => serve::run_serve(&context, args),
        Command::Delete(args) => run_delete(&context, args),
        Command::Completion(_) => unreachable!("completion is handled before context resolution"),
    }
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
        Command::Delete(args) => args.format,
        Command::Attach(_) | Command::Serve(_) | Command::Completion(_) => OutputFormat::Text,
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
    match start_session(context, args) {
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
    title: Option<String>,
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
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct ProviderResume {
    provider: String,
    session_id: String,
    captured_at: String,
    capture_method: String,
    resume_args: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct RuntimeInfo {
    kind: String,
    tmux_session: String,
    generation: u64,
    started_at: String,
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
}

#[derive(Debug, Serialize)]
struct SessionView {
    id: String,
    agent: String,
    mode: String,
    title: Option<String>,
    cwd: String,
    tmux_session: String,
    status: String,
    resumable: bool,
    repo_name: Option<String>,
    attach_command: String,
    ssh_attach_command: Option<String>,
    prompt_file: Option<String>,
    log_file: Option<String>,
    created_at: String,
    updated_at: String,
}

#[derive(Debug)]
struct StartView {
    format: OutputFormat,
    result: SessionView,
}

#[derive(Debug, Serialize)]
struct DeleteResult {
    id: String,
    tmux_session: String,
    killed: bool,
    deleted: bool,
    session_dir: String,
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
    tmux_session: String,
    status: String,
    resumable: bool,
    repo_name: Option<String>,
    tail: String,
    created_at: String,
    updated_at: String,
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
    exit_code: i32,
}

impl CliError {
    fn usage(code: impl Into<String>, message: impl Into<String>, details: Option<Value>) -> Self {
        Self(Box::new(CliErrorData {
            code: code.into(),
            message: message.into(),
            details,
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
            exit_code: exit::RUNTIME,
        }))
    }

    fn data(code: impl Into<String>, message: impl Into<String>, details: Option<Value>) -> Self {
        Self(Box::new(CliErrorData {
            code: code.into(),
            message: message.into(),
            details,
            exit_code: exit::DATA,
        }))
    }

    fn into_inner(self) -> CliErrorData {
        *self.0
    }
}

fn start_session(context: &CliContext, args: cli::StartArgs) -> Result<StartView, CliError> {
    let cwd = resolve_cwd(args.cwd.as_deref())?;
    let prompt = read_prompt(&args.prompt, args.prompt_file.as_deref(), args.prompt_stdin)?;
    let provider_plan = initial_provider_resume_plan(args.agent, &cwd);
    let launch_started_at = SystemTime::now();
    let tmux_bin = resolve_tmux_bin(args.tmux_bin.as_deref());
    let agent_bin = resolve_agent_bin(args.agent, args.agent_bin.as_deref());
    let mut created = create_record(RecordRequest {
        context,
        agent: args.agent,
        mode: "interactive",
        title: args.title.as_deref(),
        explicit_id: args.id.as_deref(),
        cwd: &cwd,
        prompt: prompt.as_deref(),
        log_file_name: None,
        provider_resume: provider_plan.provider_resume.clone(),
        agent_args: args.agent_args.clone(),
        agent_bin: Some(display_path(&agent_bin)),
    })?;

    if let Err(err) = start_interactive_tmux(
        &tmux_bin,
        &agent_bin,
        args.agent,
        &created.record,
        args.title.as_deref(),
        &provider_plan.launch_args,
        &args.agent_args,
    ) {
        cleanup_created_record(&created);
        return Err(err);
    }
    if created.prompt_file.is_some() {
        if args.paste_delay_ms > 0 {
            thread::sleep(Duration::from_millis(args.paste_delay_ms));
        }
        if let Err(err) = paste_prompt(&tmux_bin, &created.record) {
            let _ = kill_tmux_session(&tmux_bin, &created.record.tmux_session);
            cleanup_created_record(&created);
            return Err(err);
        }
    }
    if created.record.provider_resume.is_none()
        && let Some(provider_resume) =
            capture_provider_resume_after_launch(args.agent, &created.record, launch_started_at)
    {
        created.record.provider_resume = Some(provider_resume);
        created.record = persist_or_reload_session_record(context, &created.record);
    }

    let result = session_view(context, &created.record, Some("running".to_string()));
    record_workdir_usage(context, &cwd);
    Ok(StartView {
        format: args.format,
        result,
    })
}

fn start_run_session(context: &CliContext, args: cli::RunArgs) -> Result<StartView, CliError> {
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
    let created = create_record(RecordRequest {
        context,
        agent: args.agent,
        mode: "run",
        title: args.title.as_deref(),
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
    if let Err(err) = start_run_tmux(
        &tmux_bin,
        &agent_bin,
        args.agent,
        &created.record,
        &args.agent_args,
    ) {
        cleanup_created_record(&created);
        return Err(err);
    }

    let result = session_view(context, &created.record, Some("running".to_string()));
    record_workdir_usage(context, &cwd);
    Ok(StartView {
        format: args.format,
        result,
    })
}

struct CreatedRecord {
    record: SessionRecord,
    prompt_file: Option<PathBuf>,
    session_dir: PathBuf,
}

struct RecordRequest<'a> {
    context: &'a CliContext,
    agent: AgentKind,
    mode: &'a str,
    title: Option<&'a str>,
    explicit_id: Option<&'a str>,
    cwd: &'a Path,
    prompt: Option<&'a str>,
    log_file_name: Option<&'a str>,
    provider_resume: Option<ProviderResume>,
    agent_args: Vec<String>,
    agent_bin: Option<String>,
}

fn create_record(request: RecordRequest<'_>) -> Result<CreatedRecord, CliError> {
    let now = Zoned::now();
    let timestamp = now.strftime("%Y%m%d-%H%M%S").to_string();
    let iso = now.timestamp().to_string();
    let slug = slugify(request.title.unwrap_or(request.agent.as_str()));
    let id = resolve_session_id(
        request.context,
        request.explicit_id,
        request.agent,
        &timestamp,
        &slug,
    )?;
    let tmux_session = format!("hs-{}-{id}", request.agent.as_str());
    let session_dir = session_dir(request.context, &id);
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
    let record = SessionRecord {
        schema_version: SESSION_DOCUMENT_VERSION.to_string(),
        id,
        agent: request.agent.as_str().to_string(),
        mode: request.mode.to_string(),
        title: request.title.map(str::to_string),
        cwd: display_path(request.cwd),
        tmux_session: tmux_session.clone(),
        prompt_file: prompt_file.as_ref().map(|path| display_path(path)),
        log_file: log_file.as_ref().map(|path| display_path(path)),
        created_at: iso.clone(),
        updated_at: iso,
        provider_resume: request.provider_resume,
        runtime: Some(RuntimeInfo {
            kind: "tmux".to_string(),
            tmux_session: tmux_session.clone(),
            generation: 1,
            started_at: now.timestamp().to_string(),
        }),
        agent_args: request.agent_args,
        agent_bin: request.agent_bin,
        extra: BTreeMap::new(),
    };

    write_session_record(request.context, &record)?;
    Ok(CreatedRecord {
        record,
        prompt_file,
        session_dir,
    })
}

fn cleanup_created_record(created: &CreatedRecord) {
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
                }),
                launch_args: vec!["--session-id".to_string(), session_id],
            }
        }
        AgentKind::Codex => InitialProviderPlan::default(),
        AgentKind::Hermes => InitialProviderPlan::default(),
    }
}

fn start_interactive_tmux(
    tmux_bin: &Path,
    agent_bin: &Path,
    agent: AgentKind,
    record: &SessionRecord,
    title: Option<&str>,
    provider_launch_args: &[String],
    agent_args: &[String],
) -> Result<(), CliError> {
    let mut command = ProcessCommand::new(tmux_bin);
    command
        .arg("new-session")
        .arg("-d")
        .arg("-s")
        .arg(&record.tmux_session)
        .arg("-c")
        .arg(&record.cwd)
        .arg("--")
        .arg(agent_bin);

    match agent {
        AgentKind::Codex => {
            command.arg("--cd").arg(&record.cwd).arg("--no-alt-screen");
        }
        AgentKind::Claude => {
            command.args(provider_launch_args);
            if let Some(title) = title.filter(|value| !value.trim().is_empty()) {
                command.arg("--name").arg(title);
            }
        }
        AgentKind::Hermes => {
            command.arg("chat");
        }
    }
    command.args(agent_args);
    run_status(command, "tmux new-session")
}

fn start_run_tmux(
    tmux_bin: &Path,
    agent_bin: &Path,
    agent: AgentKind,
    record: &SessionRecord,
    agent_args: &[String],
) -> Result<(), CliError> {
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

    let mut command = ProcessCommand::new(tmux_bin);
    command
        .arg("new-session")
        .arg("-d")
        .arg("-s")
        .arg(&record.tmux_session)
        .arg("-c")
        .arg(&record.cwd)
        .arg("--")
        .arg("sh")
        .arg("-lc")
        .arg(script);
    run_status(command, "tmux new-session")
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
    modified_at: SystemTime,
}

#[derive(Debug, PartialEq)]
struct CodexSessionMeta {
    session_id: String,
    created_at: SystemTime,
}

fn capture_codex_resume(
    record: &SessionRecord,
    launch_started_at: SystemTime,
) -> Option<ProviderResume> {
    let root = codex_sessions_root()?;
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
    let started = Instant::now();

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
        candidates.sort_by_key(|candidate| Reverse(candidate.modified_at));
        match candidates.as_slice() {
            [candidate] => {
                return Some(ProviderResume {
                    provider: "codex".to_string(),
                    session_id: candidate.session_id.clone(),
                    captured_at: Zoned::now().timestamp().to_string(),
                    capture_method: "codex-session-meta".to_string(),
                    resume_args: vec![
                        "resume".to_string(),
                        candidate.session_id.clone(),
                        "--cd".to_string(),
                        record.cwd.clone(),
                        "--no-alt-screen".to_string(),
                    ],
                });
            }
            [] => {}
            _ => return None,
        }
        if timeout.is_zero() || started.elapsed() >= timeout {
            return None;
        }
        let remaining = timeout.saturating_sub(started.elapsed());
        thread::sleep(poll.min(remaining));
    }
}

fn codex_sessions_root() -> Option<PathBuf> {
    if let Some(codex_home) = non_empty_env("CODEX_HOME") {
        return Some(PathBuf::from(codex_home).join("sessions"));
    }
    home_dir().map(|home| home.join(".codex/sessions"))
}

#[derive(Debug)]
struct CodexResumeScanBudget {
    visited: usize,
    max_entries: usize,
    deadline: Instant,
    truncated: bool,
}

impl CodexResumeScanBudget {
    fn from_env() -> Self {
        let max_entries = env_usize(
            "AGENT_SESSION_CODEX_CAPTURE_MAX_ENTRIES",
            CODEX_RESUME_SCAN_MAX_ENTRIES,
        )
        .max(1);
        let slice = Duration::from_millis(env_u64(
            "AGENT_SESSION_CODEX_SCAN_SLICE_MS",
            CODEX_RESUME_SCAN_SLICE_MS,
        ));
        Self {
            visited: 0,
            max_entries,
            deadline: Instant::now() + slice,
            truncated: false,
        }
    }

    fn visit_entry(&mut self) -> bool {
        if self.visited >= self.max_entries || Instant::now() >= self.deadline {
            self.truncated = true;
            return false;
        }
        self.visited += 1;
        true
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
        if let Some(meta) = read_codex_session_meta(&path, cwd) {
            if meta.created_at < earliest {
                continue;
            }
            candidates.push(CodexResumeCandidate {
                session_id: meta.session_id,
                modified_at,
            });
        }
    }
}

fn read_codex_session_meta(path: &Path, cwd: &str) -> Option<CodexSessionMeta> {
    let file = fs::File::open(path).ok()?;
    let mut lines = io::BufReader::new(file).lines();
    let first_line = lines.next()?.ok()?;
    let value: Value = serde_json::from_str(&first_line).ok()?;
    if value.get("type").and_then(Value::as_str)? != "session_meta" {
        return None;
    }
    let payload = value.get("payload")?;
    if payload.get("cwd").and_then(Value::as_str)? != cwd {
        return None;
    }
    if payload.get("source").and_then(Value::as_str)? != "cli" {
        return None;
    }
    let session_id = payload
        .get("id")
        .or_else(|| payload.get("session_id"))
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .map(str::to_string)?;
    let timestamp = payload
        .get("timestamp")
        .or_else(|| value.get("timestamp"))
        .and_then(Value::as_str)?;
    let created_at: SystemTime = timestamp.parse::<Timestamp>().ok()?.into();
    Some(CodexSessionMeta {
        session_id,
        created_at,
    })
}

fn paste_prompt(tmux_bin: &Path, record: &SessionRecord) -> Result<(), CliError> {
    let prompt_file = record.prompt_file.as_ref().ok_or_else(|| {
        CliError::runtime(
            "missing-prompt-file",
            "session prompt file is missing",
            None,
        )
    })?;
    let buffer_name = format!("{}-prompt", record.id);
    let target = format!("{}:0.0", record.tmux_session);

    load_and_paste_buffer(tmux_bin, &buffer_name, &target, Path::new(prompt_file))?;

    // The initial prompt is submitted; `send` deliberately leaves this to
    // an explicit `--key enter`.
    let mut enter = ProcessCommand::new(tmux_bin);
    enter.arg("send-keys").arg("-t").arg(&target).arg("Enter");
    run_status(enter, "tmux send-keys")
}

/// Load `file` into a named tmux buffer and paste it into `target`, deleting the
/// buffer after paste (`-d`) or on failure. Shared by `paste_prompt` (initial
/// prompt) and `send` (steering text) so the buffer lifecycle lives in one place.
fn load_and_paste_buffer(
    tmux_bin: &Path,
    buffer_name: &str,
    target: &str,
    file: &Path,
) -> Result<(), CliError> {
    let mut load = ProcessCommand::new(tmux_bin);
    load.arg("load-buffer").arg("-b").arg(buffer_name).arg(file);
    run_status(load, "tmux load-buffer")?;

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
        return Err(err);
    }
    Ok(())
}

fn delete_tmux_buffer(tmux_bin: &Path, buffer_name: &str) {
    let _ = ProcessCommand::new(tmux_bin)
        .arg("delete-buffer")
        .arg("-b")
        .arg(buffer_name)
        .status();
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
    let mut record = load_session_record(context, &args.id)?;
    let tmux_bin = resolve_tmux_bin(args.tmux_bin.as_deref());
    if live_status(&tmux_bin, &record.tmux_session) != "running" {
        return Err(CliError::runtime(
            "session-not-running",
            format!("session is not running: {}", record.id),
            Some(json!({ "id": record.id })),
        ));
    }
    send_input(context, &record, text.as_deref(), &args.keys, &tmux_bin)?;
    touch_updated_at(context, &mut record)?;
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
fn send_input(
    context: &CliContext,
    record: &SessionRecord,
    text: Option<&str>,
    keys: &[SpecialKey],
    tmux_bin: &Path,
) -> Result<(), CliError> {
    let target = format!("{}:0.0", record.tmux_session);
    if let Some(text) = text {
        let buffer_name = format!("{}-send", record.id);
        let temp = session_dir(context, &record.id).join("send-input");
        write_private_file(&temp, text.as_bytes())?;
        let result = load_and_paste_buffer(tmux_bin, &buffer_name, &target, &temp);
        let _ = fs::remove_file(&temp);
        result?;
    }
    for key in keys {
        let mut command = ProcessCommand::new(tmux_bin);
        command
            .arg("send-keys")
            .arg("-t")
            .arg(&target)
            .arg(key.tmux_key());
        run_status(command, "tmux send-keys")?;
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
    let record = load_session_record(context, &args.id)?;
    let tmux_bin = resolve_tmux_bin(args.tmux_bin.as_deref());
    let status = session_status(&tmux_bin, &record);
    let tail = if status == "running" {
        capture_pane_tail(&record, args.tail, &tmux_bin)?
    } else {
        String::new()
    };
    Ok(GlanceResult {
        id: record.id.clone(),
        agent: record.agent.clone(),
        title: record.title.clone(),
        tmux_session: record.tmux_session.clone(),
        status,
        resumable: is_resumable(&record),
        repo_name: repo_name_from_cwd(&record.cwd),
        tail,
        created_at: record.created_at.clone(),
        updated_at: record.updated_at.clone(),
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
    fn codex_session_meta_reader_matches_only_cli_source_and_cwd() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("rollout.jsonl");
        fs::write(
            &path,
            r#"{"timestamp":"2099-01-01T00:00:00Z","type":"session_meta","payload":{"id":"codex-id","session_id":"codex-id","cwd":"/repo","source":"cli","timestamp":"2099-01-01T00:00:00Z"}}"#,
        )
        .unwrap();
        assert_eq!(
            read_codex_session_meta(&path, "/repo").map(|meta| meta.session_id),
            Some("codex-id".to_string())
        );
        assert_eq!(read_codex_session_meta(&path, "/other"), None);

        fs::write(
            &path,
            r#"{"timestamp":"2099-01-01T00:00:00Z","type":"session_meta","payload":{"id":"subagent-id","cwd":"/repo","source":{"subagent":{}},"timestamp":"2099-01-01T00:00:00Z"}}"#,
        )
        .unwrap();
        assert_eq!(read_codex_session_meta(&path, "/repo"), None);
    }
}

/// Bump `updated_at` to now so `list` can order by real control-plane activity.
/// Applied on `send` (a steering action); intentionally not on `glance`, which
/// is a high-frequency dashboard poll that would otherwise make `updated_at`
/// track polling rather than activity.
fn touch_updated_at(context: &CliContext, record: &mut SessionRecord) -> Result<(), CliError> {
    record.updated_at = Zoned::now().timestamp().to_string();
    write_session_record(context, record)
}

fn update_session_title(
    context: &CliContext,
    id: &str,
    title: Option<String>,
    tmux_bin: &Path,
) -> Result<SessionView, CliError> {
    let mut record = load_session_record(context, id)?;
    record.title = normalize_title(title)?;
    touch_updated_at(context, &mut record)?;
    let status = session_status(tmux_bin, &record);
    Ok(session_view(context, &record, Some(status)))
}

fn resume_session(context: &CliContext, args: cli::ResumeArgs) -> Result<SessionView, CliError> {
    let tmux_bin = resolve_tmux_bin(args.tmux_bin.as_deref());
    resume_session_by_id(context, &args.id, &tmux_bin)
}

fn resume_session_by_id(
    context: &CliContext,
    id: &str,
    tmux_bin: &Path,
) -> Result<SessionView, CliError> {
    let mut record = load_session_record(context, id)?;
    match session_status(tmux_bin, &record).as_str() {
        "running" => return Ok(session_view(context, &record, Some("running".to_string()))),
        "unknown" => {
            return Err(CliError::runtime(
                "session-status-unknown",
                format!("session status could not be checked: {}", record.id),
                Some(json!({ "id": record.id })),
            ));
        }
        _ => {}
    }
    let (provider_resume, agent) = validate_resume_metadata(&record)?;
    let resume_args = provider_resume.resume_args.clone();
    let agent_bin = record
        .agent_bin
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(|| resolve_agent_bin(agent, None));
    start_resume_tmux(tmux_bin, &agent_bin, &record, &resume_args)?;

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
    });
    record.updated_at = now.timestamp().to_string();
    record = persist_or_reload_session_record(context, &record);
    Ok(session_view(context, &record, Some("running".to_string())))
}

fn start_resume_tmux(
    tmux_bin: &Path,
    agent_bin: &Path,
    record: &SessionRecord,
    resume_args: &[String],
) -> Result<(), CliError> {
    let mut command = ProcessCommand::new(tmux_bin);
    command
        .arg("new-session")
        .arg("-d")
        .arg("-s")
        .arg(&record.tmux_session)
        .arg("-c")
        .arg(&record.cwd)
        .arg("--")
        .arg(agent_bin)
        .args(resume_args)
        .args(&record.agent_args);
    run_status(command, "tmux new-session")
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
    let sessions_root = context.state_dir.join("sessions");
    if !sessions_root.exists() {
        return Ok(Vec::new());
    }
    let tmux_bin = tmux_bin
        .map(Path::to_path_buf)
        .unwrap_or_else(|| resolve_tmux_bin(None));
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
                let status = session_status(&tmux_bin, &record);
                records.push(session_view(context, &record, Some(status)));
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
    let record = load_session_record(context, id)?;
    let tmux_bin = tmux_bin
        .map(Path::to_path_buf)
        .unwrap_or_else(|| resolve_tmux_bin(None));
    let status = session_status(&tmux_bin, &record);
    Ok(session_view(context, &record, Some(status)))
}

fn load_session_record(context: &CliContext, id: &str) -> Result<SessionRecord, CliError> {
    let resolved = resolve_session_record_path(context, id)?;
    let record = read_session_record(&resolved.record_path)?;
    validate_record_id(&record, &resolved.expected_id, &resolved.record_path)?;
    Ok(record)
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

fn write_session_record(context: &CliContext, record: &SessionRecord) -> Result<(), CliError> {
    let bytes = serde_json::to_vec_pretty(record).map_err(|err| {
        CliError::runtime(
            "session-render-failed",
            format!("failed to render session json: {err}"),
            None,
        )
    })?;
    let path = session_dir(context, &record.id).join("session.json");
    write_private_file(&path, &bytes)?;
    write_resume_sidecar(context, record)
}

fn persist_or_reload_session_record(context: &CliContext, record: &SessionRecord) -> SessionRecord {
    match write_session_record(context, record) {
        Ok(()) => record.clone(),
        Err(_) => load_session_record(context, &record.id).unwrap_or_else(|_| record.clone()),
    }
}

fn merge_resume_sidecar(path: &Path, record: &mut SessionRecord) -> Result<(), CliError> {
    let sidecar_path = path.with_file_name(SESSION_RESUME_FILE);
    if !sidecar_path.is_file() {
        return Ok(());
    }
    let contents = fs::read_to_string(&sidecar_path).map_err(|err| {
        CliError::runtime(
            "session-read-failed",
            format!("failed to read {}: {err}", sidecar_path.display()),
            Some(json!({ "path": display_path(&sidecar_path) })),
        )
    })?;
    let sidecar: DurableResumeRecord = serde_json::from_str(&contents).map_err(|err| {
        CliError::data(
            "session-json-invalid",
            format!("failed to parse {}: {err}", sidecar_path.display()),
            Some(json!({ "path": display_path(&sidecar_path) })),
        )
    })?;
    if sidecar.schema_version != SESSION_RESUME_DOCUMENT_VERSION {
        return Err(CliError::data(
            "unsupported-session-resume-version",
            format!(
                "unsupported resume schema_version {}; expected {}",
                sidecar.schema_version, SESSION_RESUME_DOCUMENT_VERSION
            ),
            Some(json!({
                "path": display_path(&sidecar_path),
                "schema_version": sidecar.schema_version,
            })),
        ));
    }
    if record.provider_resume.is_none() {
        record.provider_resume = sidecar.provider_resume;
    }
    if record.runtime.is_none() {
        record.runtime = sidecar.runtime;
    }
    if record.agent_args.is_empty() {
        record.agent_args = sidecar.agent_args;
    }
    if record.agent_bin.is_none() {
        record.agent_bin = sidecar.agent_bin;
    }
    Ok(())
}

fn write_resume_sidecar(context: &CliContext, record: &SessionRecord) -> Result<(), CliError> {
    let path = session_dir(context, &record.id).join(SESSION_RESUME_FILE);
    let Some(sidecar) = durable_resume_record(record) else {
        if path.exists() {
            let _ = fs::remove_file(path);
        }
        return Ok(());
    };
    let bytes = serde_json::to_vec_pretty(&sidecar).map_err(|err| {
        CliError::runtime(
            "session-render-failed",
            format!("failed to render resume json: {err}"),
            None,
        )
    })?;
    write_private_file(&path, &bytes)
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
        })
}

fn session_view(
    context: &CliContext,
    record: &SessionRecord,
    forced_status: Option<String>,
) -> SessionView {
    let status = forced_status.unwrap_or_else(|| session_status(&resolve_tmux_bin(None), record));
    SessionView {
        id: record.id.clone(),
        agent: record.agent.clone(),
        mode: record.mode.clone(),
        title: record.title.clone(),
        cwd: record.cwd.clone(),
        tmux_session: record.tmux_session.clone(),
        status,
        resumable: is_resumable(record),
        repo_name: repo_name_from_cwd(&record.cwd),
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
    }
}

fn session_logs(
    record: &SessionRecord,
    tail: usize,
    tmux_bin: &Path,
) -> Result<LogsResult, CliError> {
    if let Some(result) = read_session_log_file(record, tail)? {
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
        "no tmux pane output or log file is available",
        Some(json!({ "id": record.id })),
    ))
}

fn read_session_log_file(
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
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(err) => {
                return Err(CliError::runtime(
                    "log-read-failed",
                    format!("failed to read {log_file}: {err}"),
                    Some(json!({ "log_file": log_file })),
                ));
            }
        }
    }
    Ok(None)
}

fn delete_session(
    context: &CliContext,
    id: &str,
    tmux_bin: PathBuf,
) -> Result<DeleteResult, CliError> {
    let resolved = resolve_session_record_path(context, id)?;
    let record = read_session_record(&resolved.record_path)?;
    validate_record_id(&record, &resolved.expected_id, &resolved.record_path)?;
    let session_dir = resolved.session_dir;
    let killed = kill_tmux_session(&tmux_bin, &record.tmux_session);
    fs::remove_dir_all(&session_dir).map_err(|err| {
        CliError::runtime(
            "session-delete-failed",
            format!("failed to delete {}: {err}", session_dir.display()),
            Some(json!({ "path": display_path(&session_dir) })),
        )
    })?;
    Ok(DeleteResult {
        id: record.id,
        tmux_session: record.tmux_session,
        killed,
        deleted: true,
        session_dir: display_path(&session_dir),
    })
}

fn kill_tmux_session(tmux_bin: &Path, tmux_session: &str) -> bool {
    ProcessCommand::new(tmux_bin)
        .arg("kill-session")
        .arg("-t")
        .arg(tmux_session)
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn session_status(tmux_bin: &Path, record: &SessionRecord) -> String {
    match live_status(tmux_bin, &record.tmux_session).as_str() {
        "stopped" if is_resumable(record) => "resumable".to_string(),
        other => other.to_string(),
    }
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
    Ok((provider_resume, agent))
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

fn live_status(tmux_bin: &Path, tmux_session: &str) -> String {
    match ProcessCommand::new(tmux_bin)
        .arg("has-session")
        .arg("-t")
        .arg(tmux_session)
        .status()
    {
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
    slug: &str,
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
    let base = format!("{timestamp}-{}-{slug}", agent.as_str());
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
            let envelope: Envelope<()> =
                Envelope::failure(schema_version_for(BINARY, command, 1), envelope_error);
            print_json(&envelope);
        }
        OutputFormat::Text => {
            let _ = writeln!(io::stderr(), "error: {}", err.message);
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

fn render_delete_text(result: &DeleteResult) -> String {
    format!(
        "deleted {} (tmux killed: {})\n",
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

fn env_u64(key: &str, default: u64) -> u64 {
    non_empty_env(key)
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(default)
}

fn env_usize(key: &str, default: usize) -> usize {
    non_empty_env(key)
        .and_then(|value| value.parse::<usize>().ok())
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
    use super::strip_trailing_blank_lines;

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
}
