mod cli;
pub mod completion;

use std::env;
use std::ffi::OsString;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;
use std::thread;
use std::time::Duration;

use clap::Parser;
use clap::error::ErrorKind;
use jiff::Zoned;
use nils_common::cli_contract::{
    Envelope, EnvelopeError, OutputFormat, emit_parse_error, exit, schema_version_for,
};
use nils_common::fs::{
    SECRET_FILE_MODE, display_path, expand_home, home_dir, normalize_path, write_atomic,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use cli::{AgentKind, Cli, Command};

const SESSION_DOCUMENT_VERSION: &str = "agent-session.session.v1";
const BINARY: &str = "agent-session";
const START_COMMAND: &str = "start";
const RUN_COMMAND: &str = "run";
const LIST_COMMAND: &str = "list";
const COMMAND_COMMAND: &str = "command";
const LOGS_COMMAND: &str = "logs";
const DELETE_COMMAND: &str = "delete";

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
        Command::Delete(args) => args.format,
        Command::Attach(_) | Command::Completion(_) => OutputFormat::Text,
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
    let created = create_record(RecordRequest {
        context,
        agent: args.agent,
        mode: "interactive",
        title: args.title.as_deref(),
        explicit_id: args.id.as_deref(),
        cwd: &cwd,
        prompt: prompt.as_deref(),
        log_file_name: None,
    })?;

    let tmux_bin = resolve_tmux_bin(args.tmux_bin.as_deref());
    let agent_bin = resolve_agent_bin(args.agent, args.agent_bin.as_deref());
    if let Err(err) = start_interactive_tmux(
        &tmux_bin,
        &agent_bin,
        args.agent,
        &created.record,
        args.title.as_deref(),
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
            cleanup_created_record(&created);
            return Err(err);
        }
    }

    let result = session_view(context, &created.record, Some("running".to_string()));
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
        tmux_session,
        prompt_file: prompt_file.as_ref().map(|path| display_path(path)),
        log_file: log_file.as_ref().map(|path| display_path(path)),
        created_at: iso.clone(),
        updated_at: iso,
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

fn start_interactive_tmux(
    tmux_bin: &Path,
    agent_bin: &Path,
    agent: AgentKind,
    record: &SessionRecord,
    title: Option<&str>,
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
            if let Some(title) = title.filter(|value| !value.trim().is_empty()) {
                command.arg("--name").arg(title);
            }
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

    let mut load = ProcessCommand::new(tmux_bin);
    load.arg("load-buffer")
        .arg("-b")
        .arg(&buffer_name)
        .arg(prompt_file);
    run_status(load, "tmux load-buffer")?;

    let mut paste = ProcessCommand::new(tmux_bin);
    paste
        .arg("paste-buffer")
        .arg("-b")
        .arg(&buffer_name)
        .arg("-d")
        .arg("-t")
        .arg(&target);
    if let Err(err) = run_status(paste, "tmux paste-buffer") {
        delete_tmux_buffer(tmux_bin, &buffer_name);
        return Err(err);
    }

    let mut enter = ProcessCommand::new(tmux_bin);
    enter.arg("send-keys").arg("-t").arg(&target).arg("Enter");
    if let Err(err) = run_status(enter, "tmux send-keys") {
        delete_tmux_buffer(tmux_bin, &buffer_name);
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
            let record_path = entry.path().join("session.json");
            if record_path.is_file() {
                let record = read_session_record(&record_path)?;
                let status = live_status(&tmux_bin, &record.tmux_session);
                records.push(session_view(context, &record, Some(status)));
            }
        }
    }
    records.sort_by(|a, b| {
        b.created_at
            .cmp(&a.created_at)
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
    let status = live_status(&tmux_bin, &record.tmux_session);
    Ok(session_view(context, &record, Some(status)))
}

fn load_session_record(context: &CliContext, id: &str) -> Result<SessionRecord, CliError> {
    let record_path = resolve_session_record_path(context, id)?;
    read_session_record(&record_path)
}

fn resolve_session_record_path(context: &CliContext, id: &str) -> Result<PathBuf, CliError> {
    validate_id(id)?;
    let exact = session_dir(context, id).join("session.json");
    if exact.is_file() {
        return ensure_record_in_sessions_root(context, &exact);
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
            if name.starts_with(id) && entry.path().join("session.json").is_file() {
                matches.push(ensure_record_in_sessions_root(
                    context,
                    &entry.path().join("session.json"),
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

fn ensure_record_in_sessions_root(context: &CliContext, path: &Path) -> Result<PathBuf, CliError> {
    let sessions_root = context.state_dir.join("sessions");
    let canonical_root = fs::canonicalize(&sessions_root).map_err(|err| {
        CliError::runtime(
            "session-root-unavailable",
            format!("failed to canonicalize {}: {err}", sessions_root.display()),
            Some(json!({ "path": display_path(&sessions_root) })),
        )
    })?;
    let canonical_path = fs::canonicalize(path).map_err(|err| {
        CliError::runtime(
            "session-read-failed",
            format!("failed to canonicalize {}: {err}", path.display()),
            Some(json!({ "path": display_path(path) })),
        )
    })?;
    if !canonical_path.starts_with(&canonical_root) {
        return Err(CliError::usage(
            "session-path-escaped",
            "session record path escapes the managed state directory",
            Some(json!({ "id_path": display_path(path) })),
        ));
    }
    Ok(canonical_path)
}

fn read_session_record(path: &Path) -> Result<SessionRecord, CliError> {
    let contents = fs::read_to_string(path).map_err(|err| {
        CliError::runtime(
            "session-read-failed",
            format!("failed to read {}: {err}", path.display()),
            Some(json!({ "path": display_path(path) })),
        )
    })?;
    let record: SessionRecord = serde_json::from_str(&contents).map_err(|err| {
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
    Ok(record)
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
    write_private_file(&path, &bytes)
}

fn session_view(
    context: &CliContext,
    record: &SessionRecord,
    forced_status: Option<String>,
) -> SessionView {
    let status =
        forced_status.unwrap_or_else(|| live_status(&resolve_tmux_bin(None), &record.tmux_session));
    SessionView {
        id: record.id.clone(),
        agent: record.agent.clone(),
        mode: record.mode.clone(),
        title: record.title.clone(),
        cwd: record.cwd.clone(),
        tmux_session: record.tmux_session.clone(),
        status,
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
    if live_status(tmux_bin, &record.tmux_session) == "running" {
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
        if output.status.success() {
            return Ok(LogsResult {
                id: record.id.clone(),
                source: "tmux".to_string(),
                text: String::from_utf8_lossy(&output.stdout).to_string(),
            });
        }
    }

    if let Some(log_file) = &record.log_file {
        let text = fs::read_to_string(log_file).map_err(|err| {
            CliError::runtime(
                "log-read-failed",
                format!("failed to read {log_file}: {err}"),
                Some(json!({ "log_file": log_file })),
            )
        })?;
        return Ok(LogsResult {
            id: record.id.clone(),
            source: "file".to_string(),
            text: tail_lines(&text, tail),
        });
    }

    Err(CliError::runtime(
        "logs-unavailable",
        "no tmux pane output or log file is available",
        Some(json!({ "id": record.id })),
    ))
}

fn delete_session(
    context: &CliContext,
    id: &str,
    tmux_bin: PathBuf,
) -> Result<DeleteResult, CliError> {
    let record_path = resolve_session_record_path(context, id)?;
    let record = read_session_record(&record_path)?;
    let session_dir = record_path
        .parent()
        .ok_or_else(|| {
            CliError::runtime(
                "invalid-session-path",
                "session record has no parent directory",
                None,
            )
        })?
        .to_path_buf();
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

fn live_status(tmux_bin: &Path, tmux_session: &str) -> String {
    match ProcessCommand::new(tmux_bin)
        .arg("has-session")
        .arg("-t")
        .arg(tmux_session)
        .status()
    {
        Ok(status) if status.success() => "running".to_string(),
        Ok(_) => "stopped".to_string(),
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
