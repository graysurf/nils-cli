mod cli;
mod completion;

use std::env;
use std::ffi::OsString;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Stdio};

use clap::Parser;
use clap::error::ErrorKind;
use nils_common::cli_contract::{Envelope, EnvelopeError, OutputFormat, emit_parse_error, exit};
use nils_common::fs::display_path;
use nils_common::process::find_in_path;
use nils_common::redact::redact_text;
use serde::Serialize;
use serde_json::{Value, json};

use cli::{Cli, Command, InstallTools, SetupArgs};

const BINARY: &str = "zsh-kit";
const SETUP_SCHEMA_VERSION: &str = "cli.zsh-kit.setup.v1";
const ERROR_SCHEMA_VERSION: &str = "cli.zsh-kit.error.v1";
const ZSHENV_MARKER: &str = "# Managed by zsh-kit.";

pub fn run() -> i32 {
    run_with_args(env::args_os())
}

pub fn run_with_args<I, T>(args: I) -> i32
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let args_vec: Vec<OsString> = args.into_iter().map(Into::into).collect();
    let cli = match Cli::try_parse_from(args_vec.clone()) {
        Ok(cli) => cli,
        Err(err) => {
            let kind = err.kind();
            if matches!(
                kind,
                ErrorKind::DisplayHelp
                    | ErrorKind::DisplayVersion
                    | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
            ) {
                err.exit();
            }
            let format = detect_format_from_argv(args_vec.iter());
            let code = if matches!(kind, ErrorKind::InvalidSubcommand) {
                "unknown-subcommand"
            } else {
                "parse-error"
            };
            let message = render_clap_message(&err);
            return emit_parse_error(BINARY, format, code, &message);
        }
    };

    match cli.command {
        Command::Setup(args) => run_setup(args),
        Command::Completion(args) => completion::run(args.shell),
    }
}

fn run_setup(args: SetupArgs) -> i32 {
    let format = args.format;
    match setup(&args) {
        Ok(result) => render_setup_success(format, &result),
        Err(err) => render_setup_error(format, err),
    }
}

fn setup(args: &SetupArgs) -> Result<SetupResult, CliError> {
    let repo = prepare_repo(&args.repo)?;
    let dest = resolve_dest(args.dest.as_deref())?;
    let mut plan = SetupPlan::new(args, repo, dest);

    if plan.dest.exists() && !plan.dest.is_dir() {
        return Err(CliError::data(
            "destination-conflict",
            format!(
                "destination exists and is not a directory: {}",
                display_path(&plan.dest)
            ),
            Some(json!({ "dest": display_path(&plan.dest) })),
        ));
    }

    validate_destination(args, &plan)?;
    validate_local_source_hook(args, &plan)?;
    plan.add_bootstrap_action();

    if args.dry_run {
        plan.mutation_status = "planned".to_string();
        return Ok(plan.into_result(None, Vec::new()));
    }

    apply_setup(args, plan)
}

fn apply_setup(args: &SetupArgs, mut plan: SetupPlan) -> Result<SetupResult, CliError> {
    let mut changed_paths = Vec::new();
    if !plan.dest.exists() {
        clone_repo(args, &plan)?;
        changed_paths.push(display_path(&plan.dest));
    } else {
        update_repo(args, &plan)?;
    }

    let hook_path = find_hook(&plan.dest).ok_or_else(|| missing_hook_error(&plan.dest))?;

    if args.write_zshenv {
        let paths = write_zshenv(&plan.dest, args.force)?;
        changed_paths.extend(paths.into_iter().map(|path| display_path(&path)));
    }

    dispatch_hook(args, &hook_path)?;
    plan.mutation_status = "applied".to_string();
    changed_paths.sort();
    changed_paths.dedup();
    Ok(plan.into_result(Some(hook_path), changed_paths))
}

fn validate_destination(args: &SetupArgs, plan: &SetupPlan) -> Result<(), CliError> {
    if !plan.dest.exists() {
        return Ok(());
    }
    if !is_git_repo(&plan.dest) {
        return Err(CliError::data(
            "destination-not-git",
            format!(
                "destination exists but is not a Git repository: {}",
                display_path(&plan.dest)
            ),
            Some(json!({ "dest": display_path(&plan.dest) })),
        ));
    }

    if let Some(origin) = git_stdout(&["config", "--get", "remote.origin.url"], Some(&plan.dest))?
        && origin.trim() != plan.repo
        && !args.force
    {
        return Err(CliError::data(
            "destination-repo-mismatch",
            "destination remote.origin.url does not match --repo; pass --force to proceed",
            Some(json!({
                "dest": display_path(&plan.dest),
                "expected_repo": plan.repo_safe,
                "actual_origin": redact_text(origin.trim()).value,
            })),
        ));
    }

    let status = git_stdout_required(&["status", "--porcelain"], Some(&plan.dest))?;
    if !status.trim().is_empty() && !args.force {
        return Err(CliError::data(
            "destination-dirty",
            "destination Git repository has uncommitted changes; pass --force to proceed",
            Some(json!({
                "dest": display_path(&plan.dest),
                "status": redact_text(status.trim()).value,
            })),
        ));
    }

    if find_hook(&plan.dest).is_none() {
        return Err(missing_hook_error(&plan.dest));
    }

    Ok(())
}

fn validate_local_source_hook(args: &SetupArgs, plan: &SetupPlan) -> Result<(), CliError> {
    if plan.dest.exists() {
        return Ok(());
    }
    let Some(source_path) = local_repo_path(&args.repo) else {
        return Ok(());
    };
    if find_hook(&source_path).is_none() {
        return Err(missing_hook_error(&source_path));
    }
    Ok(())
}

fn clone_repo(args: &SetupArgs, plan: &SetupPlan) -> Result<(), CliError> {
    if let Some(parent) = plan.dest.parent() {
        fs::create_dir_all(parent).map_err(|err| io_error("io-error", parent, err))?;
    }

    let mut command_args = vec!["clone".to_string()];
    if let Some(branch) = &args.branch {
        command_args.push("--branch".to_string());
        command_args.push(branch.clone());
    }
    command_args.push(plan.repo_raw.clone());
    command_args.push(display_path(&plan.dest));
    run_git(&command_args, None)?;

    if let Some(rev) = &args.ref_name {
        run_git(&["checkout".to_string(), rev.clone()], Some(&plan.dest))?;
    }
    Ok(())
}

fn update_repo(args: &SetupArgs, plan: &SetupPlan) -> Result<(), CliError> {
    run_git(
        &[
            "fetch".to_string(),
            "--all".to_string(),
            "--prune".to_string(),
        ],
        Some(&plan.dest),
    )?;
    if let Some(branch) = &args.branch {
        run_git(&["checkout".to_string(), branch.clone()], Some(&plan.dest))?;
        run_git(
            &[
                "pull".to_string(),
                "--ff-only".to_string(),
                "origin".to_string(),
                branch.clone(),
            ],
            Some(&plan.dest),
        )?;
    } else if let Some(rev) = &args.ref_name {
        run_git(&["checkout".to_string(), rev.clone()], Some(&plan.dest))?;
    } else {
        run_git(
            &["pull".to_string(), "--ff-only".to_string()],
            Some(&plan.dest),
        )?;
    }
    Ok(())
}

fn dispatch_hook(args: &SetupArgs, hook_path: &Path) -> Result<(), CliError> {
    let zsh = find_in_path("zsh").ok_or_else(|| {
        CliError::unavailable(
            "zsh-not-found",
            "`zsh` is required to dispatch the repository setup hook",
            None,
        )
    })?;
    let feature_csv = args.features.join(",");
    let output = ProcessCommand::new(zsh)
        .arg(hook_path)
        .arg("--features")
        .arg(&feature_csv)
        .arg("--install-tools")
        .arg(args.install_tools.as_str())
        .current_dir(
            hook_path
                .parent()
                .and_then(Path::parent)
                .unwrap_or_else(|| Path::new(".")),
        )
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|err| {
            CliError::runtime(
                "hook-command-failed",
                format!("failed to spawn setup hook: {err}"),
                Some(json!({ "hook_path": display_path(hook_path) })),
            )
        })?;
    if output.status.success() {
        return Ok(());
    }

    Err(CliError::runtime(
        "hook-command-failed",
        format!("setup hook exited with status {}", output.status),
        Some(json!({
            "hook_path": display_path(hook_path),
            "stdout": redact_text(&String::from_utf8_lossy(&output.stdout)).value,
            "stderr": redact_text(&String::from_utf8_lossy(&output.stderr)).value,
        })),
    ))
}

fn write_zshenv(dest: &Path, force: bool) -> Result<Vec<PathBuf>, CliError> {
    let home = home_dir()?;
    let zshenv = home.join(".zshenv");
    let mut changed = Vec::new();
    if zshenv.exists() {
        let existing =
            fs::read_to_string(&zshenv).map_err(|err| io_error("io-error", &zshenv, err))?;
        if !existing.contains(ZSHENV_MARKER) && !force {
            return Err(CliError::data(
                "zshenv-conflict",
                "existing .zshenv is not managed by zsh-kit; pass --force to replace it",
                Some(json!({ "path": display_path(&zshenv) })),
            ));
        }
        if !existing.contains(ZSHENV_MARKER) && force {
            let backup = home.join(".zshenv.zsh-kit.bak");
            fs::write(&backup, existing).map_err(|err| io_error("io-error", &backup, err))?;
            changed.push(backup);
        }
    }

    let content = format!(
        "{ZSHENV_MARKER}\nexport ZDOTDIR={}\n",
        shell_quote(&display_path(dest))
    );
    fs::write(&zshenv, content).map_err(|err| io_error("io-error", &zshenv, err))?;
    changed.push(zshenv);
    Ok(changed)
}

#[derive(Debug)]
struct SetupPlan {
    repo: String,
    repo_raw: String,
    repo_safe: String,
    dest: PathBuf,
    branch: Option<String>,
    ref_name: Option<String>,
    features: Vec<String>,
    install_tools: InstallTools,
    write_zshenv: bool,
    force: bool,
    mode: String,
    actions: Vec<SetupAction>,
    mutation_status: String,
}

impl SetupPlan {
    fn new(args: &SetupArgs, repo: RepoInput, dest: PathBuf) -> Self {
        let mut actions = Vec::new();
        if dest.exists() {
            actions.push(SetupAction::new(
                "update",
                "fetch and fast-forward destination repository",
                true,
                Some(display_path(&dest)),
                Some("git fetch --all --prune; git pull --ff-only".to_string()),
            ));
        } else {
            actions.push(SetupAction::new(
                "clone",
                "clone repository into destination",
                true,
                Some(display_path(&dest)),
                Some("git clone <repo> <dest>".to_string()),
            ));
        }
        actions.push(SetupAction::new(
            "hook",
            "validate repository setup hook",
            false,
            Some(display_path(&dest)),
            None,
        ));
        actions.push(SetupAction::new(
            "dispatch-hook",
            "dispatch repository setup hook in apply mode",
            true,
            Some(display_path(&dest)),
            Some("zsh <hook> --features <csv> --install-tools <policy>".to_string()),
        ));

        Self {
            repo: repo.safe.clone(),
            repo_raw: repo.raw,
            repo_safe: repo.safe,
            dest,
            branch: args.branch.clone(),
            ref_name: args.ref_name.clone(),
            features: args.features.clone(),
            install_tools: args.install_tools,
            write_zshenv: args.write_zshenv,
            force: args.force,
            mode: if args.apply { "apply" } else { "dry-run" }.to_string(),
            actions,
            mutation_status: "not-started".to_string(),
        }
    }

    fn add_bootstrap_action(&mut self) {
        if self.write_zshenv {
            self.actions.push(SetupAction::new(
                "write-zshenv",
                "write managed .zshenv bootstrap",
                true,
                home_dir()
                    .ok()
                    .map(|home| display_path(&home.join(".zshenv"))),
                None,
            ));
        }
    }

    fn into_result(self, hook_path: Option<PathBuf>, changed_paths: Vec<String>) -> SetupResult {
        let hook_candidates = hook_candidates(&self.dest)
            .into_iter()
            .map(|path| display_path(&path))
            .collect();
        SetupResult {
            repo: self.repo,
            dest: display_path(&self.dest),
            mode: self.mode,
            branch: self.branch,
            ref_name: self.ref_name,
            features: self.features,
            install_tools: self.install_tools,
            write_zshenv: self.write_zshenv,
            force: self.force,
            hook_path: hook_path.map(|path| display_path(&path)),
            hook_candidates,
            actions: self.actions,
            changed_paths,
            mutation_status: self.mutation_status,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
struct SetupResult {
    repo: String,
    dest: String,
    mode: String,
    branch: Option<String>,
    #[serde(rename = "ref")]
    ref_name: Option<String>,
    features: Vec<String>,
    install_tools: InstallTools,
    write_zshenv: bool,
    force: bool,
    hook_path: Option<String>,
    hook_candidates: Vec<String>,
    actions: Vec<SetupAction>,
    changed_paths: Vec<String>,
    mutation_status: String,
}

#[derive(Clone, Debug, Serialize)]
struct SetupAction {
    kind: String,
    description: String,
    mutation: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    command: Option<String>,
}

impl SetupAction {
    fn new(
        kind: impl Into<String>,
        description: impl Into<String>,
        mutation: bool,
        path: Option<String>,
        command: Option<String>,
    ) -> Self {
        Self {
            kind: kind.into(),
            description: description.into(),
            mutation,
            path,
            command,
        }
    }
}

#[derive(Debug)]
struct RepoInput {
    raw: String,
    safe: String,
}

fn prepare_repo(raw: &str) -> Result<RepoInput, CliError> {
    let redacted = redact_text(raw);
    if contains_url_userinfo(raw) {
        return Err(CliError::data(
            "credential-bearing-repo-url",
            "repository URLs must not include embedded credentials",
            Some(json!({ "repo": redacted.value })),
        ));
    }
    Ok(RepoInput {
        raw: raw.to_string(),
        safe: redacted.value,
    })
}

fn contains_url_userinfo(value: &str) -> bool {
    let Some((scheme, rest)) = value.split_once("://") else {
        return false;
    };
    if !matches!(scheme, "http" | "https") {
        return false;
    }
    let authority = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default()
        .trim();
    authority.contains('@')
}

fn local_repo_path(raw: &str) -> Option<PathBuf> {
    if let Some(rest) = raw.strip_prefix("file://") {
        return Some(PathBuf::from(rest));
    }
    let path = PathBuf::from(raw);
    path.exists().then_some(path)
}

fn resolve_dest(dest: Option<&Path>) -> Result<PathBuf, CliError> {
    match dest {
        Some(path) => Ok(expand_home(path)),
        None => Ok(home_dir()?.join(".config/zsh")),
    }
}

fn home_dir() -> Result<PathBuf, CliError> {
    env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .ok_or_else(|| CliError::data("home-not-set", "HOME is required for zsh-kit setup", None))
}

fn expand_home(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    if text == "~"
        && let Ok(home) = home_dir()
    {
        return home;
    }
    if let Some(rest) = text.strip_prefix("~/")
        && let Ok(home) = home_dir()
    {
        return home.join(rest);
    }
    path.to_path_buf()
}

fn hook_candidates(root: &Path) -> Vec<PathBuf> {
    vec![
        root.join("bootstrap/zsh-kit-setup.zsh"),
        root.join(".zsh-kit/setup.zsh"),
    ]
}

fn find_hook(root: &Path) -> Option<PathBuf> {
    hook_candidates(root)
        .into_iter()
        .find(|path| path.is_file())
}

fn missing_hook_error(root: &Path) -> CliError {
    CliError::data(
        "missing-setup-hook",
        "repository does not contain a supported zsh-kit setup hook",
        Some(json!({
            "root": display_path(root),
            "candidates": hook_candidates(root)
                .into_iter()
                .map(|path| display_path(&path))
                .collect::<Vec<_>>(),
        })),
    )
}

fn is_git_repo(path: &Path) -> bool {
    git_stdout(&["rev-parse", "--is-inside-work-tree"], Some(path))
        .ok()
        .flatten()
        .is_some_and(|value| value.trim() == "true")
}

fn git_stdout(args: &[&str], cwd: Option<&Path>) -> Result<Option<String>, CliError> {
    let output = git_output(args, cwd)?;
    if output.status.success() {
        return Ok(Some(String::from_utf8_lossy(&output.stdout).to_string()));
    }
    Ok(None)
}

fn git_stdout_required(args: &[&str], cwd: Option<&Path>) -> Result<String, CliError> {
    let output = git_output(args, cwd)?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).to_string());
    }
    Err(git_failed(args, &output))
}

fn run_git(args: &[String], cwd: Option<&Path>) -> Result<(), CliError> {
    let args_ref: Vec<&str> = args.iter().map(String::as_str).collect();
    let output = git_output(&args_ref, cwd)?;
    if output.status.success() {
        return Ok(());
    }
    Err(git_failed(&args_ref, &output))
}

fn git_output(args: &[&str], cwd: Option<&Path>) -> Result<std::process::Output, CliError> {
    let mut command = ProcessCommand::new("git");
    command.args(args);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|err| {
            CliError::runtime(
                "git-command-failed",
                format!("failed to spawn git: {err}"),
                None,
            )
        })
}

fn git_failed(args: &[&str], output: &std::process::Output) -> CliError {
    CliError::runtime(
        "git-command-failed",
        format!(
            "git {} failed with status {}",
            args.join(" "),
            output.status
        ),
        Some(json!({
            "args": args,
            "stdout": redact_text(&String::from_utf8_lossy(&output.stdout)).value,
            "stderr": redact_text(&String::from_utf8_lossy(&output.stderr)).value,
        })),
    )
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn io_error(code: &'static str, path: &Path, err: io::Error) -> CliError {
    CliError::runtime(
        code,
        format!("{}: {err}", display_path(path)),
        Some(json!({ "path": display_path(path) })),
    )
}

#[derive(Debug)]
struct CliError {
    code: &'static str,
    message: String,
    exit_code: i32,
    details: Option<Value>,
}

impl CliError {
    fn data(code: &'static str, message: impl Into<String>, details: Option<Value>) -> Self {
        Self {
            code,
            message: message.into(),
            exit_code: exit::DATA,
            details,
        }
    }

    fn runtime(code: &'static str, message: impl Into<String>, details: Option<Value>) -> Self {
        Self {
            code,
            message: message.into(),
            exit_code: exit::RUNTIME,
            details,
        }
    }

    fn unavailable(code: &'static str, message: impl Into<String>, details: Option<Value>) -> Self {
        Self {
            code,
            message: message.into(),
            exit_code: exit::UNAVAILABLE,
            details,
        }
    }
}

fn render_setup_success(format: OutputFormat, result: &SetupResult) -> i32 {
    match format {
        OutputFormat::Json => {
            let envelope = Envelope::success(SETUP_SCHEMA_VERSION, result);
            print_json(&envelope)
        }
        OutputFormat::Text => {
            println!("zsh-kit setup {}", result.mutation_status);
            println!("repo: {}", result.repo);
            println!("dest: {}", result.dest);
            println!("mode: {}", result.mode);
            match &result.hook_path {
                Some(path) => println!("hook: {path}"),
                None => println!("hook: pending"),
            }
            println!("actions:");
            for action in &result.actions {
                println!("- {}", action.description);
            }
            if !result.changed_paths.is_empty() {
                println!("changed paths:");
                for path in &result.changed_paths {
                    println!("- {path}");
                }
            }
            exit::SUCCESS
        }
    }
}

fn render_setup_error(format: OutputFormat, err: CliError) -> i32 {
    match format {
        OutputFormat::Json => {
            let mut envelope_error = EnvelopeError::new(err.code, err.message);
            if let Some(details) = err.details {
                envelope_error = envelope_error.with_details(details);
            }
            let envelope: Envelope<()> = Envelope::failure(ERROR_SCHEMA_VERSION, envelope_error);
            let _ = print_json(&envelope);
        }
        OutputFormat::Text => {
            let redacted = redact_text(&err.message).value;
            let _ = writeln!(io::stderr(), "error: {redacted}");
        }
    }
    err.exit_code
}

fn print_json<T: Serialize>(envelope: &Envelope<T>) -> i32 {
    match serde_json::to_string(envelope) {
        Ok(serialized) => {
            println!("{serialized}");
            exit::SUCCESS
        }
        Err(_) => exit::SOFTWARE,
    }
}

fn detect_format_from_argv<'a, I>(args: I) -> OutputFormat
where
    I: IntoIterator<Item = &'a OsString>,
{
    let mut iter = args.into_iter().skip(1).peekable();
    while let Some(arg) = iter.next() {
        let arg = arg.to_string_lossy();
        if arg == "--format" {
            if let Some(next) = iter.peek()
                && next.to_string_lossy().eq_ignore_ascii_case("json")
            {
                return OutputFormat::Json;
            }
        } else if let Some(rest) = arg.strip_prefix("--format=")
            && rest.eq_ignore_ascii_case("json")
        {
            return OutputFormat::Json;
        }
    }
    OutputFormat::Text
}

fn render_clap_message(err: &clap::Error) -> String {
    err.to_string()
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(|line| {
            line.trim()
                .strip_prefix("error:")
                .map(str::trim)
                .unwrap_or_else(|| line.trim())
                .to_string()
        })
        .unwrap_or_else(|| "command-line parse failed".to_string())
}
