mod cli;
mod completion;

use std::collections::BTreeSet;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::process::Command as ProcessCommand;

use clap::Parser;
use clap::error::ErrorKind;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use cli::{ChangeMode, Cli, Command, CommonArgs, CreateArgs, OutputFormat, ValidateArgs};

use nils_common::cli_contract::exit;
use nils_common::fs::{display_path, normalize_path as normalize_absolute_path};

const EXIT_OK: i32 = exit::SUCCESS;
const EXIT_RUNTIME_OR_SCOPE: i32 = exit::RUNTIME;
const EXIT_USAGE: i32 = exit::USAGE;

const LOCK_DOCUMENT_VERSION: &str = "agent-scope-lock.v1";
const LOCK_FILE_NAME: &str = "agent-scope-lock.json";

const CREATE_SCHEMA_VERSION: &str = "cli.agent-scope-lock.create.v1";
const READ_SCHEMA_VERSION: &str = "cli.agent-scope-lock.read.v1";
const VALIDATE_SCHEMA_VERSION: &str = "cli.agent-scope-lock.validate.v1";
const CLEAR_SCHEMA_VERSION: &str = "cli.agent-scope-lock.clear.v1";

const CREATE_COMMAND: &str = "agent-scope-lock create";
const READ_COMMAND: &str = "agent-scope-lock read";
const VALIDATE_COMMAND: &str = "agent-scope-lock validate";
const CLEAR_COMMAND: &str = "agent-scope-lock clear";

pub fn run() -> i32 {
    run_with_args(env::args_os())
}

pub fn run_with_args<I, T>(args: I) -> i32
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(err) => {
            let code = match err.kind() {
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => err.exit_code(),
                _ => EXIT_USAGE,
            };
            let _ = err.print();
            return code;
        }
    };

    dispatch(cli)
}

fn dispatch(cli: Cli) -> i32 {
    match cli.command {
        Command::Create(args) => run_create(args),
        Command::Read(args) => run_read(args),
        Command::Validate(args) => run_validate(args),
        Command::Clear(args) => run_clear(args),
        Command::Completion(args) => completion::run(args.shell),
    }
}

fn run_create(args: CreateArgs) -> i32 {
    match create_lock(&args) {
        Ok(result) => render_create_success(args.common.format, &result),
        Err(err) => render_error(
            CREATE_SCHEMA_VERSION,
            CREATE_COMMAND,
            args.common.format,
            err,
        ),
    }
}

fn run_read(args: CommonArgs) -> i32 {
    match read_lock_result(&args) {
        Ok(result) => render_read_success(args.format, &result),
        Err(err) => render_error(READ_SCHEMA_VERSION, READ_COMMAND, args.format, err),
    }
}

fn run_validate(args: ValidateArgs) -> i32 {
    match validate_lock(&args) {
        Ok(report) if report.violations.is_empty() => {
            render_validate_success(args.common.format, &report)
        }
        Ok(report) => render_validate_violations(args.common.format, &report),
        Err(err) => render_error(
            VALIDATE_SCHEMA_VERSION,
            VALIDATE_COMMAND,
            args.common.format,
            err,
        ),
    }
}

fn run_clear(args: CommonArgs) -> i32 {
    match clear_lock(&args) {
        Ok(result) => render_clear_success(args.format, &result),
        Err(err) => render_error(CLEAR_SCHEMA_VERSION, CLEAR_COMMAND, args.format, err),
    }
}

fn create_lock(args: &CreateArgs) -> Result<LockResult, CliError> {
    if args.paths.is_empty() {
        return Err(CliError::usage(
            "missing-path",
            "create requires at least one --path",
            Some(json!({ "flag": "--path" })),
        ));
    }

    let lock_file = resolve_lock_file(args.common.lock_file.as_deref())?;
    if lock_file.exists() && !args.force {
        return Err(CliError::runtime(
            "lock-exists",
            format!(
                "{} already exists; pass --force to overwrite",
                lock_file.display()
            ),
            Some(json!({ "lock_file": display_path(&lock_file), "force_flag": "--force" })),
        ));
    }

    let repo_root = git_repo_root()?;
    let allowed_paths = normalize_allowed_paths(&repo_root, &args.paths)?;
    let lock = LockDocument {
        schema_version: LOCK_DOCUMENT_VERSION.to_string(),
        allowed_paths,
        owner: args.owner.clone().filter(|value| !value.is_empty()),
        note: args.note.clone().filter(|value| !value.is_empty()),
    };

    if let Some(parent) = lock_file.parent() {
        fs::create_dir_all(parent).map_err(|err| {
            CliError::runtime(
                "lock-parent-create-failed",
                format!("failed to create {}: {err}", parent.display()),
                Some(json!({ "path": display_path(parent) })),
            )
        })?;
    }

    let mut contents = serde_json::to_string_pretty(&lock).map_err(|err| {
        CliError::runtime(
            "lock-render-failed",
            format!("failed to render lock json: {err}"),
            None,
        )
    })?;
    contents.push('\n');
    fs::write(&lock_file, contents).map_err(|err| {
        CliError::runtime(
            "lock-write-failed",
            format!("failed to write {}: {err}", lock_file.display()),
            Some(json!({ "lock_file": display_path(&lock_file) })),
        )
    })?;

    Ok(LockResult {
        lock_file: display_path(&lock_file),
        lock,
    })
}

fn read_lock_result(args: &CommonArgs) -> Result<LockResult, CliError> {
    let lock_file = resolve_lock_file(args.lock_file.as_deref())?;
    let lock = read_lock_document(&lock_file)?;
    Ok(LockResult {
        lock_file: display_path(&lock_file),
        lock,
    })
}

fn validate_lock(args: &ValidateArgs) -> Result<ValidateReport, CliError> {
    let lock_file = resolve_lock_file(args.common.lock_file.as_deref())?;
    let lock = read_lock_document(&lock_file)?;
    let changed_paths = changed_paths(args.changes)?;
    let violations: Vec<ScopeViolation> = changed_paths
        .iter()
        .filter(|path| !is_allowed_path(path, &lock.allowed_paths))
        .map(|path| ScopeViolation {
            path: path.clone(),
            reason: "changed path is outside allowed prefixes".to_string(),
        })
        .collect();

    Ok(ValidateReport {
        lock_file: display_path(&lock_file),
        mode: change_mode_name(args.changes).to_string(),
        allowed_paths: lock.allowed_paths,
        changed_paths,
        violations,
    })
}

fn clear_lock(args: &CommonArgs) -> Result<ClearResult, CliError> {
    let lock_file = resolve_lock_file(args.lock_file.as_deref())?;
    let removed = match fs::remove_file(&lock_file) {
        Ok(()) => true,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => false,
        Err(err) => {
            return Err(CliError::runtime(
                "lock-clear-failed",
                format!("failed to remove {}: {err}", lock_file.display()),
                Some(json!({ "lock_file": display_path(&lock_file) })),
            ));
        }
    };

    Ok(ClearResult {
        lock_file: display_path(&lock_file),
        removed,
    })
}

fn read_lock_document(lock_file: &Path) -> Result<LockDocument, CliError> {
    let contents = fs::read_to_string(lock_file).map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound {
            CliError::runtime(
                "missing-lock",
                format!("scope lock not found: {}", lock_file.display()),
                Some(json!({ "lock_file": display_path(lock_file) })),
            )
        } else {
            CliError::runtime(
                "lock-read-failed",
                format!("failed to read {}: {err}", lock_file.display()),
                Some(json!({ "lock_file": display_path(lock_file) })),
            )
        }
    })?;

    let lock: LockDocument = serde_json::from_str(&contents).map_err(|err| {
        CliError::runtime(
            "invalid-lock-json",
            format!("failed to parse {}: {err}", lock_file.display()),
            Some(json!({ "lock_file": display_path(lock_file) })),
        )
    })?;

    if lock.schema_version != LOCK_DOCUMENT_VERSION {
        return Err(CliError::runtime(
            "unsupported-lock-version",
            format!(
                "unsupported lock schema_version {}; expected {}",
                lock.schema_version, LOCK_DOCUMENT_VERSION
            ),
            Some(json!({
                "lock_file": display_path(lock_file),
                "schema_version": lock.schema_version,
                "expected": LOCK_DOCUMENT_VERSION
            })),
        ));
    }
    if lock.allowed_paths.is_empty() {
        return Err(CliError::runtime(
            "invalid-lock",
            "scope lock has no allowed_paths",
            Some(json!({ "lock_file": display_path(lock_file) })),
        ));
    }

    Ok(lock)
}

fn normalize_allowed_paths(repo_root: &Path, paths: &[PathBuf]) -> Result<Vec<String>, CliError> {
    let repo_root = normalize_absolute_path(repo_root);
    let mut normalized = BTreeSet::new();

    for path in paths {
        let absolute = if path.is_absolute() {
            normalize_absolute_path(path)
        } else {
            normalize_absolute_path(&repo_root.join(path))
        };

        let relative = absolute.strip_prefix(&repo_root).map_err(|_| {
            CliError::usage(
                "path-outside-repo",
                format!(
                    "{} is outside repository {}",
                    path.display(),
                    repo_root.display()
                ),
                Some(json!({ "path": display_path(path), "repo_root": display_path(&repo_root) })),
            )
        })?;

        let normalized_path = repo_relative_path(relative)?;
        normalized.insert(normalized_path);
    }

    Ok(normalized.into_iter().collect())
}

fn repo_relative_path(path: &Path) -> Result<String, CliError> {
    if path.as_os_str().is_empty() {
        return Ok(".".to_string());
    }

    let parts: Vec<String> = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part.to_string_lossy().to_string()),
            Component::CurDir => None,
            _ => Some(component.as_os_str().to_string_lossy().to_string()),
        })
        .collect();
    let joined = parts.join("/");
    let trimmed = joined.trim_matches('/').to_string();

    if trimmed.is_empty() {
        Ok(".".to_string())
    } else if trimmed == ".git" || trimmed.starts_with(".git/") {
        Err(CliError::usage(
            "git-dir-not-allowed",
            "allowed paths must not target the git metadata directory",
            Some(json!({ "path": trimmed })),
        ))
    } else {
        Ok(trimmed)
    }
}

fn is_allowed_path(path: &str, allowed_paths: &[String]) -> bool {
    allowed_paths.iter().any(|allowed| {
        allowed == "." || path == allowed || path.starts_with(&format!("{allowed}/"))
    })
}

fn changed_paths(mode: ChangeMode) -> Result<Vec<String>, CliError> {
    let mut paths = BTreeSet::new();

    match mode {
        ChangeMode::All => {
            collect_git_paths(&mut paths, &["diff", "--name-only"])?;
            collect_git_paths(&mut paths, &["diff", "--name-only", "--cached"])?;
            collect_git_paths(&mut paths, &["ls-files", "--others", "--exclude-standard"])?;
        }
        ChangeMode::Staged => {
            collect_git_paths(&mut paths, &["diff", "--name-only", "--cached"])?;
        }
        ChangeMode::Unstaged => {
            collect_git_paths(&mut paths, &["diff", "--name-only"])?;
            collect_git_paths(&mut paths, &["ls-files", "--others", "--exclude-standard"])?;
        }
    }

    Ok(paths.into_iter().collect())
}

fn collect_git_paths(paths: &mut BTreeSet<String>, args: &[&str]) -> Result<(), CliError> {
    let output = git_stdout(args)?;
    for line in output.lines() {
        let path = line.trim();
        if !path.is_empty() {
            paths.insert(path.to_string());
        }
    }
    Ok(())
}

fn resolve_lock_file(explicit: Option<&Path>) -> Result<PathBuf, CliError> {
    match explicit {
        Some(path) => Ok(absolute_path(path)?),
        None => {
            let git_path = git_stdout(&["rev-parse", "--git-path", LOCK_FILE_NAME])?;
            let git_path = PathBuf::from(git_path.trim());
            if git_path.is_absolute() {
                Ok(normalize_absolute_path(&git_path))
            } else {
                Ok(absolute_path(&git_path)?)
            }
        }
    }
}

fn git_repo_root() -> Result<PathBuf, CliError> {
    let root = git_stdout(&["rev-parse", "--show-toplevel"])?;
    Ok(normalize_absolute_path(Path::new(root.trim())))
}

fn git_stdout(args: &[&str]) -> Result<String, CliError> {
    let executable = trusted_git_executable()?;
    let mut command = ProcessCommand::new(executable);
    command
        .env_clear()
        .env("HOME", "/nonexistent")
        .env("LC_ALL", "C")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .args([
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.untrackedCache=false",
            "-c",
            "core.pager=cat",
        ]);
    if args.first() == Some(&"diff") {
        command.arg("diff").args(["--no-ext-diff", "--no-textconv"]);
        command.args(&args[1..]);
    } else {
        command.args(args);
    }
    let output = command.output().map_err(|err| {
        CliError::runtime(
            "git-spawn-failed",
            format!("failed to run git: {err}"),
            None,
        )
    })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(CliError::runtime(
            "git-command-failed",
            if stderr.is_empty() {
                format!("git {} failed", args.join(" "))
            } else {
                format!("git {} failed: {stderr}", args.join(" "))
            },
            Some(json!({ "git_args": args })),
        ));
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn trusted_git_executable() -> Result<PathBuf, CliError> {
    for directory in ["/usr/bin", "/bin", "/usr/local/bin", "/opt/homebrew/bin"] {
        let candidate = Path::new(directory).join("git");
        let Ok(canonical) = fs::canonicalize(&candidate) else {
            continue;
        };
        let Ok(metadata) = fs::metadata(&canonical) else {
            continue;
        };
        if metadata.is_file()
            && (metadata.uid() == 0 || metadata.uid() == unsafe { libc::geteuid() })
            && metadata.permissions().mode() & 0o022 == 0
            && metadata.permissions().mode() & 0o111 != 0
        {
            return Ok(canonical);
        }
    }
    Err(CliError::runtime(
        "git-spawn-failed",
        "a trusted system Git executable is unavailable",
        None,
    ))
}

fn absolute_path(path: &Path) -> Result<PathBuf, CliError> {
    if path.is_absolute() {
        return Ok(normalize_absolute_path(path));
    }

    let current_dir = env::current_dir().map_err(|err| {
        CliError::runtime(
            "cwd-unavailable",
            format!("failed to read current directory: {err}"),
            None,
        )
    })?;
    Ok(normalize_absolute_path(&current_dir.join(path)))
}

fn change_mode_name(mode: ChangeMode) -> &'static str {
    match mode {
        ChangeMode::All => "all",
        ChangeMode::Staged => "staged",
        ChangeMode::Unstaged => "unstaged",
    }
}

fn render_create_success(format: OutputFormat, result: &LockResult) -> i32 {
    match format {
        OutputFormat::Json => print_json_success(CREATE_SCHEMA_VERSION, CREATE_COMMAND, result)
            .unwrap_or_else(render_json_failure),
        OutputFormat::Text => {
            println!("created scope lock: {}", result.lock_file);
            print_lock_text(&result.lock);
            EXIT_OK
        }
    }
}

fn render_read_success(format: OutputFormat, result: &LockResult) -> i32 {
    match format {
        OutputFormat::Json => print_json_success(READ_SCHEMA_VERSION, READ_COMMAND, result)
            .unwrap_or_else(render_json_failure),
        OutputFormat::Text => {
            println!("lock file: {}", result.lock_file);
            print_lock_text(&result.lock);
            EXIT_OK
        }
    }
}

fn render_validate_success(format: OutputFormat, report: &ValidateReport) -> i32 {
    match format {
        OutputFormat::Json => print_json_success(VALIDATE_SCHEMA_VERSION, VALIDATE_COMMAND, report)
            .unwrap_or_else(render_json_failure),
        OutputFormat::Text => {
            println!(
                "scope ok: {} changed path(s) allowed by {}",
                report.changed_paths.len(),
                report.lock_file
            );
            EXIT_OK
        }
    }
}

fn render_validate_violations(format: OutputFormat, report: &ValidateReport) -> i32 {
    match format {
        OutputFormat::Json => print_json_error(
            VALIDATE_SCHEMA_VERSION,
            VALIDATE_COMMAND,
            "scope-violations",
            "changed paths are outside allowed prefixes",
            Some(json!({
                "lock_file": report.lock_file,
                "mode": report.mode,
                "allowed_paths": report.allowed_paths,
                "changed_paths": report.changed_paths,
                "violations": report.violations,
            })),
            EXIT_RUNTIME_OR_SCOPE,
        )
        .unwrap_or_else(render_json_failure),
        OutputFormat::Text => {
            eprintln!("agent-scope-lock: scope violations:");
            for violation in &report.violations {
                eprintln!("  - {}", violation.path);
            }
            eprintln!("allowed paths:");
            for path in &report.allowed_paths {
                eprintln!("  - {path}");
            }
            EXIT_RUNTIME_OR_SCOPE
        }
    }
}

fn render_clear_success(format: OutputFormat, result: &ClearResult) -> i32 {
    match format {
        OutputFormat::Json => print_json_success(CLEAR_SCHEMA_VERSION, CLEAR_COMMAND, result)
            .unwrap_or_else(render_json_failure),
        OutputFormat::Text => {
            if result.removed {
                println!("cleared scope lock: {}", result.lock_file);
            } else {
                println!("scope lock already clear: {}", result.lock_file);
            }
            EXIT_OK
        }
    }
}

fn print_lock_text(lock: &LockDocument) {
    if let Some(owner) = lock.owner.as_deref() {
        println!("owner: {owner}");
    }
    if let Some(note) = lock.note.as_deref() {
        println!("note: {note}");
    }
    println!("allowed paths:");
    for path in &lock.allowed_paths {
        println!("  - {path}");
    }
}

fn render_error(
    schema_version: &'static str,
    command: &'static str,
    format: OutputFormat,
    err: CliError,
) -> i32 {
    if format == OutputFormat::Json {
        return print_json_error(
            schema_version,
            command,
            err.code,
            &err.message,
            err.details,
            err.exit_code,
        )
        .unwrap_or_else(render_json_failure);
    }

    eprintln!("agent-scope-lock: error: {}", err.message);
    err.exit_code
}

fn print_json_success<T: Serialize>(
    schema_version: &'static str,
    command: &'static str,
    result: &T,
) -> Result<i32, serde_json::Error> {
    let envelope = SuccessEnvelope {
        schema_version,
        command,
        ok: true,
        result,
    };
    println!("{}", serde_json::to_string_pretty(&envelope)?);
    Ok(EXIT_OK)
}

fn print_json_error(
    schema_version: &'static str,
    command: &'static str,
    code: &'static str,
    message: &str,
    details: Option<Value>,
    exit_code: i32,
) -> Result<i32, serde_json::Error> {
    let envelope = ErrorEnvelope {
        schema_version,
        command,
        ok: false,
        error: ErrorBody {
            code,
            message,
            details,
        },
    };
    println!("{}", serde_json::to_string_pretty(&envelope)?);
    Ok(exit_code)
}

fn render_json_failure(err: serde_json::Error) -> i32 {
    eprintln!("agent-scope-lock: error: failed to render json: {err}");
    EXIT_RUNTIME_OR_SCOPE
}

#[derive(Debug)]
struct CliError {
    code: &'static str,
    message: String,
    details: Option<Value>,
    exit_code: i32,
}

impl CliError {
    fn usage(code: &'static str, message: impl Into<String>, details: Option<Value>) -> Self {
        Self {
            code,
            message: message.into(),
            details,
            exit_code: EXIT_USAGE,
        }
    }

    fn runtime(code: &'static str, message: impl Into<String>, details: Option<Value>) -> Self {
        Self {
            code,
            message: message.into(),
            details,
            exit_code: EXIT_RUNTIME_OR_SCOPE,
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct LockDocument {
    pub schema_version: String,
    pub allowed_paths: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct LockResult {
    pub lock_file: String,
    pub lock: LockDocument,
}

#[derive(Debug, Serialize)]
pub struct ClearResult {
    pub lock_file: String,
    pub removed: bool,
}

#[derive(Debug, Serialize)]
pub struct ValidateReport {
    pub lock_file: String,
    pub mode: String,
    pub allowed_paths: Vec<String>,
    pub changed_paths: Vec<String>,
    pub violations: Vec<ScopeViolation>,
}

#[derive(Debug, Serialize)]
pub struct ScopeViolation {
    pub path: String,
    pub reason: String,
}

#[derive(Serialize)]
struct SuccessEnvelope<'a, T: Serialize> {
    schema_version: &'static str,
    command: &'static str,
    ok: bool,
    result: &'a T,
}

#[derive(Serialize)]
struct ErrorEnvelope<'a> {
    schema_version: &'static str,
    command: &'static str,
    ok: bool,
    error: ErrorBody<'a>,
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    code: &'static str,
    message: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    details: Option<Value>,
}

#[cfg(test)]
mod tests {
    use super::{is_allowed_path, normalize_absolute_path, repo_relative_path};
    use std::path::Path;

    #[test]
    fn allowed_prefix_matching_is_component_aware() {
        let allowed = vec!["src".to_string(), "Cargo.toml".to_string()];

        assert!(is_allowed_path("src/lib.rs", &allowed));
        assert!(is_allowed_path("src", &allowed));
        assert!(is_allowed_path("Cargo.toml", &allowed));
        assert!(!is_allowed_path("src-next/lib.rs", &allowed));
        assert!(!is_allowed_path("Cargo.toml.bak", &allowed));
    }

    #[test]
    fn dot_allows_entire_repo() {
        let allowed = vec![".".to_string()];
        assert!(is_allowed_path("anything/here.txt", &allowed));
    }

    #[test]
    fn path_normalization_handles_parent_segments() {
        let path = normalize_absolute_path(Path::new("/tmp/repo/src/../README.md"));
        assert_eq!(path, Path::new("/tmp/repo/README.md"));
    }

    #[test]
    fn empty_repo_relative_path_becomes_dot() {
        assert_eq!(repo_relative_path(Path::new("")).expect("path"), ".");
    }
}
