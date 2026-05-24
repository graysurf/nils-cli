use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, ExitStatus};

use clap::{Args, Parser, Subcommand, ValueEnum, ValueHint};
use serde::Serialize;
use serde_json::{Value, json};

use crate::common::{
    CliError, EXIT_RUNTIME, EXIT_UNAVAILABLE, OutputFormat, absolute_path, display_path,
    render_error, render_success,
};
use crate::completion::{self, CompletionShell};

const DOCTOR_COMMAND: &str = "agent-run doctor";
const ENV_COMMAND: &str = "agent-run env";
const DOCTOR_SCHEMA_VERSION: &str = "cli.agent-run.doctor.v1";
const ENV_SCHEMA_VERSION: &str = "cli.agent-run.env.v1";

pub fn run() -> i32 {
    run_with_args(env::args_os())
}

pub fn run_with_args<I, T>(args: I) -> i32
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let argv: Vec<OsString> = args.into_iter().map(Into::into).collect();
    let cli = match Cli::try_parse_from(argv.clone()) {
        Ok(cli) => cli,
        Err(err) => return crate::common::handle_parse_error("agent-run", argv, err),
    };

    match cli.command {
        Command::Exec(args) => run_exec(args),
        Command::Doctor(args) => run_doctor(args),
        Command::Env(args) => run_env(args),
        Command::Completion(args) => completion::run::<Cli>(args.shell, "agent-run"),
    }
}

fn run_exec(args: ExecArgs) -> i32 {
    if args.command.is_empty() {
        let err = AgentRunError::usage("missing-command", "command after `--` is required", None);
        eprintln!("error[{}]: {}", err.code, err.message);
        return err.exit_code;
    }

    match execution_plan(&args.cwd, args.direnv) {
        Ok(plan) => match plan.decision.status {
            DecisionStatus::Absent | DecisionStatus::Bypassed => {
                run_child_direct(&plan.cwd, &args.command)
            }
            DecisionStatus::Active => run_child_direnv(&plan.cwd, &args.command),
            DecisionStatus::RequiredMissing
            | DecisionStatus::MissingDirenv
            | DecisionStatus::Blocked => {
                eprintln!(
                    "agent-run exec: error[{}]: {}",
                    plan.decision.error_code(),
                    plan.decision.message()
                );
                if let Some(env_file) = plan.env_file.as_ref() {
                    eprintln!("agent-run exec: env-file={}", env_file.path.display());
                }
                EXIT_UNAVAILABLE
            }
        },
        Err(err) => {
            eprintln!("agent-run exec: error[{}]: {}", err.code, err.message);
            err.exit_code
        }
    }
}

fn run_doctor(args: DoctorArgs) -> i32 {
    let format = args.format;
    match execution_plan(&args.cwd, args.direnv) {
        Ok(plan) => {
            let result = DoctorResult::from_plan(plan);
            render_success(
                DOCTOR_SCHEMA_VERSION,
                DOCTOR_COMMAND,
                format,
                || result.text_summary(),
                &result,
            )
        }
        Err(err) => render_error(
            DOCTOR_SCHEMA_VERSION,
            DOCTOR_COMMAND,
            format,
            err.into_cli_error(),
        ),
    }
}

fn run_env(args: EnvArgs) -> i32 {
    let format = args.format;
    match execution_plan(&args.cwd, args.direnv) {
        Ok(plan) => {
            let result = EnvResult::from_plan(plan);
            render_success(
                ENV_SCHEMA_VERSION,
                ENV_COMMAND,
                format,
                || result.text_summary(),
                &result,
            )
        }
        Err(err) => render_error(
            ENV_SCHEMA_VERSION,
            ENV_COMMAND,
            format,
            err.into_cli_error(),
        ),
    }
}

fn run_child_direct(cwd: &Path, command: &[OsString]) -> i32 {
    let Some((program, args)) = command.split_first() else {
        return EXIT_UNAVAILABLE;
    };
    match ProcessCommand::new(program)
        .args(args)
        .current_dir(cwd)
        .status()
    {
        Ok(status) => status_code(status),
        Err(err) => {
            eprintln!(
                "agent-run exec: error[child-spawn-failed]: failed to run {}: {err}",
                display_os(program)
            );
            EXIT_UNAVAILABLE
        }
    }
}

fn run_child_direnv(cwd: &Path, command: &[OsString]) -> i32 {
    match ProcessCommand::new("direnv")
        .arg("exec")
        .arg(cwd)
        .args(command)
        .current_dir(cwd)
        .status()
    {
        Ok(status) => status_code(status),
        Err(err) => {
            eprintln!("agent-run exec: error[direnv-unavailable]: failed to run direnv: {err}");
            EXIT_UNAVAILABLE
        }
    }
}

fn status_code(status: ExitStatus) -> i32 {
    status.code().unwrap_or(EXIT_RUNTIME)
}

fn execution_plan(cwd: &Path, mode: DirenvMode) -> Result<ExecutionPlan, AgentRunError> {
    let cwd = resolve_cwd(cwd)?;
    let env_file = discover_env_file(&cwd);
    let mut direnv = if matches!(mode, DirenvMode::Off) {
        DirenvStatus::default()
    } else {
        direnv_status(&cwd)
    };
    let decision = decide(mode, env_file.as_ref(), &mut direnv);
    Ok(ExecutionPlan {
        cwd,
        mode,
        env_file,
        direnv,
        decision,
    })
}

fn resolve_cwd(cwd: &Path) -> Result<PathBuf, AgentRunError> {
    let absolute = absolute_path(cwd).map_err(AgentRunError::from_cli_error)?;
    let canonical = fs::canonicalize(&absolute).map_err(|err| {
        AgentRunError::usage(
            "invalid-cwd",
            format!("failed to resolve --cwd {}: {err}", absolute.display()),
            Some(json!({ "cwd": display_path(&absolute) })),
        )
    })?;
    if !canonical.is_dir() {
        return Err(AgentRunError::usage(
            "invalid-cwd",
            format!("--cwd must be a directory: {}", canonical.display()),
            Some(json!({ "cwd": display_path(&canonical) })),
        ));
    }
    Ok(canonical)
}

fn discover_env_file(cwd: &Path) -> Option<EnvFile> {
    for dir in cwd.ancestors() {
        let envrc = dir.join(".envrc");
        if envrc.is_file() {
            return Some(EnvFile {
                path: envrc,
                kind: EnvFileKind::Envrc,
            });
        }
        let dotenv = dir.join(".env");
        if dotenv.is_file() {
            return Some(EnvFile {
                path: dotenv,
                kind: EnvFileKind::Dotenv,
            });
        }
    }
    None
}

fn direnv_status(cwd: &Path) -> DirenvStatus {
    match ProcessCommand::new("direnv")
        .args(["status", "--json"])
        .current_dir(cwd)
        .output()
    {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let status_json = serde_json::from_str::<Value>(&stdout).ok();
            let self_path = status_json
                .as_ref()
                .and_then(|value| value.pointer("/config/SelfPath"))
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            DirenvStatus {
                available: true,
                self_path,
                status_exit_code: output.status.code(),
                status_json,
                probe_stderr: None,
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => DirenvStatus {
            available: false,
            self_path: None,
            status_exit_code: None,
            status_json: None,
            probe_stderr: None,
        },
        Err(_) => DirenvStatus {
            available: false,
            self_path: None,
            status_exit_code: None,
            status_json: None,
            probe_stderr: None,
        },
    }
}

fn decide(mode: DirenvMode, env_file: Option<&EnvFile>, direnv: &mut DirenvStatus) -> Decision {
    match mode {
        DirenvMode::Off => {
            if env_file.is_some() {
                Decision::new(DecisionKind::Direct, DecisionStatus::Bypassed)
            } else {
                Decision::new(DecisionKind::Direct, DecisionStatus::Absent)
            }
        }
        DirenvMode::Require if env_file.is_none() => {
            Decision::new(DecisionKind::Fail, DecisionStatus::RequiredMissing)
        }
        DirenvMode::Auto | DirenvMode::Require => {
            let Some(env_file) = env_file else {
                return Decision::new(DecisionKind::Direct, DecisionStatus::Absent);
            };
            if !direnv.available {
                return Decision::new(DecisionKind::Fail, DecisionStatus::MissingDirenv);
            }
            match direnv_env_file_status(direnv) {
                DirenvEnvFileStatus::Allowed => {
                    Decision::new(DecisionKind::Direnv, DecisionStatus::Active)
                }
                DirenvEnvFileStatus::Blocked => {
                    Decision::new(DecisionKind::Fail, DecisionStatus::Blocked)
                }
                DirenvEnvFileStatus::Unknown => match env_file.kind {
                    EnvFileKind::Dotenv => {
                        Decision::new(DecisionKind::Direnv, DecisionStatus::Active)
                    }
                    EnvFileKind::Envrc => {
                        Decision::new(DecisionKind::Fail, DecisionStatus::Blocked)
                    }
                },
            }
        }
    }
}

fn direnv_env_file_status(direnv: &DirenvStatus) -> DirenvEnvFileStatus {
    match direnv.status_json.as_ref() {
        Some(status_json) => match status_json.pointer("/state/foundRC/allowed") {
            Some(Value::Number(value)) if value.as_i64() == Some(0) => DirenvEnvFileStatus::Allowed,
            Some(_) => DirenvEnvFileStatus::Blocked,
            None => DirenvEnvFileStatus::Unknown,
        },
        None => DirenvEnvFileStatus::Unknown,
    }
}

fn display_os(value: &OsStr) -> String {
    value.to_string_lossy().to_string()
}

#[derive(Debug, Parser)]
#[command(
    name = "agent-run",
    version,
    about = "Run agent commands through a normalized project environment.",
    long_about = "Run project build, test, and validation commands through a normalized project environment, including direnv when a project env file applies.",
    disable_help_subcommand = true,
    after_help = "EXAMPLES:\n  agent-run exec --cwd . -- cargo test\n  agent-run exec --cwd . --direnv require -- npm test\n  agent-run doctor --cwd . --format json\n  agent-run env --cwd . --format json\n  agent-run completion zsh\n\nENVIRONMENT:\n  PATH  Used to locate direnv and the child command.\n\nEXIT CODES:\n  0   success\n  1   runtime error\n  64  command-line usage error\n  69  required project environment unavailable\n  N   child command exit code when the child starts and exits normally"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
#[command(rename_all = "kebab-case")]
enum Command {
    /// Execute a command in the selected project environment.
    Exec(ExecArgs),
    /// Report project environment readiness.
    Doctor(DoctorArgs),
    /// Emit machine-readable project environment status.
    Env(EnvArgs),
    /// Print shell completion script.
    Completion(CompletionArgs),
}

#[derive(Debug, Args)]
struct ExecArgs {
    /// Working directory for env discovery and child execution.
    #[arg(long, value_name = "DIR", default_value = ".", value_hint = ValueHint::DirPath)]
    cwd: PathBuf,

    /// Direnv policy for project env files.
    #[arg(long, value_enum, default_value_t = DirenvMode::Auto)]
    direnv: DirenvMode,

    /// Command argv to execute. Pass after `--`.
    #[arg(
        required = true,
        trailing_var_arg = true,
        allow_hyphen_values = true,
        value_name = "COMMAND"
    )]
    command: Vec<OsString>,
}

#[derive(Debug, Args)]
struct DoctorArgs {
    /// Working directory for env discovery.
    #[arg(long, value_name = "DIR", default_value = ".", value_hint = ValueHint::DirPath)]
    cwd: PathBuf,

    /// Direnv policy to evaluate.
    #[arg(long, value_enum, default_value_t = DirenvMode::Auto)]
    direnv: DirenvMode,

    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    format: OutputFormat,
}

#[derive(Debug, Args)]
struct EnvArgs {
    /// Working directory for env discovery.
    #[arg(long, value_name = "DIR", default_value = ".", value_hint = ValueHint::DirPath)]
    cwd: PathBuf,

    /// Direnv policy to evaluate.
    #[arg(long, value_enum, default_value_t = DirenvMode::Auto)]
    direnv: DirenvMode,

    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    format: OutputFormat,
}

#[derive(Debug, Args)]
struct CompletionArgs {
    /// Shell to generate completion script for.
    #[arg(value_enum)]
    shell: CompletionShell,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Serialize)]
#[serde(rename_all = "kebab-case")]
#[value(rename_all = "kebab-case")]
enum DirenvMode {
    Auto,
    Require,
    Off,
}

#[derive(Debug)]
struct ExecutionPlan {
    cwd: PathBuf,
    mode: DirenvMode,
    env_file: Option<EnvFile>,
    direnv: DirenvStatus,
    decision: Decision,
}

#[derive(Debug)]
struct EnvFile {
    path: PathBuf,
    kind: EnvFileKind,
}

#[derive(Debug, Clone, Copy)]
enum EnvFileKind {
    Envrc,
    Dotenv,
}

impl EnvFileKind {
    fn as_str(self) -> &'static str {
        match self {
            EnvFileKind::Envrc => ".envrc",
            EnvFileKind::Dotenv => ".env",
        }
    }
}

#[derive(Debug, Default)]
struct DirenvStatus {
    available: bool,
    self_path: Option<String>,
    status_exit_code: Option<i32>,
    status_json: Option<Value>,
    probe_stderr: Option<String>,
}

enum DirenvEnvFileStatus {
    Allowed,
    Blocked,
    Unknown,
}

#[derive(Debug, Clone, Copy)]
struct Decision {
    kind: DecisionKind,
    status: DecisionStatus,
}

impl Decision {
    fn new(kind: DecisionKind, status: DecisionStatus) -> Self {
        Self { kind, status }
    }

    fn message(self) -> &'static str {
        match self.status {
            DecisionStatus::Absent => "no .envrc or .env applies; running directly",
            DecisionStatus::Bypassed => "direnv bypassed by --direnv off",
            DecisionStatus::Active => "project env is active through direnv",
            DecisionStatus::RequiredMissing => {
                "no .envrc or .env applies, but --direnv require was set"
            }
            DecisionStatus::MissingDirenv => "project env file applies, but direnv is unavailable",
            DecisionStatus::Blocked => "project env file is blocked or not allowed by direnv",
        }
    }

    fn error_code(self) -> &'static str {
        match self.status {
            DecisionStatus::RequiredMissing => "project-env-missing",
            DecisionStatus::MissingDirenv => "direnv-unavailable",
            DecisionStatus::Blocked => "direnv-blocked",
            DecisionStatus::Absent | DecisionStatus::Bypassed | DecisionStatus::Active => "ok",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "kebab-case")]
enum DecisionKind {
    Direct,
    Direnv,
    Fail,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "kebab-case")]
enum DecisionStatus {
    Absent,
    Bypassed,
    Active,
    RequiredMissing,
    MissingDirenv,
    Blocked,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
struct DoctorResult {
    schema: &'static str,
    cwd: String,
    mode: DirenvMode,
    direnv: DirenvReport,
    env_file: Option<EnvFileReport>,
    decision: DecisionReport,
    checks: Vec<DoctorCheck>,
}

impl DoctorResult {
    fn from_plan(plan: ExecutionPlan) -> Self {
        let checks = vec![
            DoctorCheck {
                name: "env-file".to_string(),
                status: if plan.env_file.is_some() {
                    "present".to_string()
                } else {
                    "absent".to_string()
                },
            },
            DoctorCheck {
                name: "direnv".to_string(),
                status: if plan.direnv.available {
                    "available".to_string()
                } else {
                    "missing".to_string()
                },
            },
            DoctorCheck {
                name: "decision".to_string(),
                status: status_str(plan.decision.status).to_string(),
            },
        ];
        Self {
            schema: "agent-run.doctor.v1",
            cwd: display_path(&plan.cwd),
            mode: plan.mode,
            direnv: DirenvReport::from_status(&plan.direnv),
            env_file: plan.env_file.as_ref().map(EnvFileReport::from_env_file),
            decision: DecisionReport::from_decision(&plan.decision),
            checks,
        }
    }

    fn text_summary(&self) -> String {
        format!(
            "agent-run doctor: cwd={} mode={} direnv={} status={}",
            self.cwd,
            mode_str(self.mode),
            if self.direnv.available {
                "available"
            } else {
                "missing"
            },
            self.decision.status
        )
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
struct EnvResult {
    schema: &'static str,
    cwd: String,
    mode: DirenvMode,
    direnv: DirenvReport,
    env_file: Option<EnvFileReport>,
    decision: DecisionReport,
}

impl EnvResult {
    fn from_plan(plan: ExecutionPlan) -> Self {
        Self {
            schema: "agent-run.env.v1",
            cwd: display_path(&plan.cwd),
            mode: plan.mode,
            direnv: DirenvReport::from_status(&plan.direnv),
            env_file: plan.env_file.as_ref().map(EnvFileReport::from_env_file),
            decision: DecisionReport::from_decision(&plan.decision),
        }
    }

    fn text_summary(&self) -> String {
        format!(
            "agent-run env: cwd={} mode={} status={} decision={}",
            self.cwd,
            mode_str(self.mode),
            self.decision.status,
            self.decision.kind
        )
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
struct DirenvReport {
    available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    status_exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    status_found_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    probe_stderr: Option<String>,
}

impl DirenvReport {
    fn from_status(status: &DirenvStatus) -> Self {
        Self {
            available: status.available,
            path: status.self_path.clone(),
            status_exit_code: status.status_exit_code,
            status_found_path: status
                .status_json
                .as_ref()
                .and_then(|value| value.pointer("/state/foundRC/path"))
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            probe_stderr: status
                .probe_stderr
                .clone()
                .filter(|value| !value.is_empty()),
        }
    }
}

#[derive(Debug, Serialize)]
struct EnvFileReport {
    kind: &'static str,
    path: String,
}

impl EnvFileReport {
    fn from_env_file(env_file: &EnvFile) -> Self {
        Self {
            kind: env_file.kind.as_str(),
            path: display_path(&env_file.path),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
struct DecisionReport {
    kind: &'static str,
    status: &'static str,
    message: &'static str,
}

impl DecisionReport {
    fn from_decision(decision: &Decision) -> Self {
        Self {
            kind: kind_str(decision.kind),
            status: status_str(decision.status),
            message: decision.message(),
        }
    }
}

#[derive(Debug, Serialize)]
struct DoctorCheck {
    name: String,
    status: String,
}

fn mode_str(mode: DirenvMode) -> &'static str {
    match mode {
        DirenvMode::Auto => "auto",
        DirenvMode::Require => "require",
        DirenvMode::Off => "off",
    }
}

fn kind_str(kind: DecisionKind) -> &'static str {
    match kind {
        DecisionKind::Direct => "direct",
        DecisionKind::Direnv => "direnv",
        DecisionKind::Fail => "fail",
    }
}

fn status_str(status: DecisionStatus) -> &'static str {
    match status {
        DecisionStatus::Absent => "absent",
        DecisionStatus::Bypassed => "bypassed",
        DecisionStatus::Active => "active",
        DecisionStatus::RequiredMissing => "required-missing",
        DecisionStatus::MissingDirenv => "missing-direnv",
        DecisionStatus::Blocked => "blocked",
    }
}

#[derive(Debug)]
struct AgentRunError {
    code: &'static str,
    message: String,
    details: Option<Value>,
    exit_code: i32,
}

impl AgentRunError {
    fn usage(code: &'static str, message: impl Into<String>, details: Option<Value>) -> Self {
        Self {
            code,
            message: message.into(),
            details,
            exit_code: crate::common::EXIT_USAGE,
        }
    }

    fn from_cli_error(_err: CliError) -> Self {
        Self {
            code: "path-resolution-failed",
            message: "failed to resolve path".to_string(),
            details: None,
            exit_code: EXIT_RUNTIME,
        }
    }

    fn into_cli_error(self) -> CliError {
        match self.exit_code {
            crate::common::EXIT_USAGE => CliError::usage(self.code, self.message, self.details),
            EXIT_UNAVAILABLE => CliError::unavailable(self.code, self.message, self.details),
            _ => CliError::runtime(self.code, self.message, self.details),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn discover_env_file_prefers_nearest_envrc() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let root = tmp.path();
        let child = root.join("a/b");
        fs::create_dir_all(&child).expect("child");
        fs::write(root.join(".env"), "ROOT=1\n").expect("root env");
        fs::write(root.join("a/.envrc"), "export A=1\n").expect("envrc");

        let env_file = discover_env_file(&child).expect("env file");

        assert_eq!(env_file.kind.as_str(), ".envrc");
        assert_eq!(env_file.path, root.join("a/.envrc"));
    }

    #[test]
    fn require_without_env_file_is_failure_decision() {
        let mut direnv = DirenvStatus::default();

        let decision = decide(DirenvMode::Require, None, &mut direnv);

        assert_eq!(status_str(decision.status), "required-missing");
        assert_eq!(kind_str(decision.kind), "fail");
    }
}
