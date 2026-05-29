mod cli;
mod completion;

use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use clap::error::ErrorKind;
use clap::{CommandFactory, Parser};
use serde_json::json;

use cli::{Cli, Command, IdArgs, ScopeArgs};

use nils_common::cli_contract::exit;
use nils_common::fs::display_path;

const EXIT_OK: i32 = exit::SUCCESS;
const EXIT_RUNTIME: i32 = exit::RUNTIME;
const EXIT_USAGE: i32 = exit::USAGE;

pub fn run() -> i32 {
    run_with_args(env::args_os())
}

pub fn run_with_args<I, T>(args: I) -> i32
where
    I: IntoIterator<Item = T>,
    T: Into<OsString>,
{
    let args: Vec<OsString> = args.into_iter().map(Into::into).collect();
    if args.len() == 1 {
        return match print_help() {
            Ok(code) => code,
            Err(err) => {
                eprintln!("agent-memory: {}", err.message);
                err.exit_code
            }
        };
    }

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

    match dispatch(cli) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("agent-memory: {}", err.message);
            err.exit_code
        }
    }
}

fn dispatch(cli: Cli) -> Result<i32, CliError> {
    let layout = Layout::from_env()?;
    match cli.command {
        Command::Path(args) => {
            println!(
                "{}",
                display_path(&layout.resolve_scope(args.scope.as_deref()))
            );
            Ok(EXIT_OK)
        }
        Command::List(args) => list_scope(&layout, &args),
        Command::Index(args) => index_scope(&layout, &args),
        Command::Agents => list_named_dirs(&layout.agents_dir()),
        Command::Personas => list_named_dirs(&layout.personas_dir()),
        Command::InitAgent(args) => init_agent(&layout, &args),
        Command::InitPersona(args) => init_persona(&layout, &args),
        Command::Resolve(args) => resolve_agent(&layout, &args),
        Command::Env => {
            print_env(&layout);
            Ok(EXIT_OK)
        }
        Command::Doctor => doctor(&layout),
        Command::Completion(args) => Ok(completion::run(args.shell)),
        Command::Help => print_help(),
    }
}

fn print_help() -> Result<i32, CliError> {
    let mut command = Cli::command();
    command
        .print_long_help()
        .map_err(|err| CliError::runtime(format!("failed to print help: {err}")))?;
    println!();
    Ok(EXIT_OK)
}

#[derive(Debug)]
struct CliError {
    message: String,
    exit_code: i32,
}

impl CliError {
    fn runtime(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            exit_code: EXIT_RUNTIME,
        }
    }

    fn usage(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            exit_code: EXIT_USAGE,
        }
    }
}

#[derive(Debug)]
struct Layout {
    root: PathBuf,
}

impl Layout {
    fn from_env() -> Result<Self, CliError> {
        if let Some(value) = non_empty_env("AGENT_MEMORY_HOME") {
            return Ok(Self {
                root: PathBuf::from(value),
            });
        }

        if let Some(value) = non_empty_env("XDG_CONFIG_HOME") {
            return Ok(Self {
                root: PathBuf::from(value).join("agent-memory"),
            });
        }

        let home = non_empty_env("HOME").ok_or_else(|| {
            CliError::runtime("HOME is not set and AGENT_MEMORY_HOME was omitted")
        })?;

        Ok(Self {
            root: PathBuf::from(home).join(".config").join("agent-memory"),
        })
    }

    fn global_dir(&self) -> PathBuf {
        self.root.join("global")
    }

    fn agents_dir(&self) -> PathBuf {
        self.root.join("agents")
    }

    fn personas_dir(&self) -> PathBuf {
        self.root.join("personas")
    }

    fn resolve_scope(&self, scope: Option<&str>) -> PathBuf {
        match scope.unwrap_or("root") {
            "" | "root" => self.root.clone(),
            "global" => self.global_dir(),
            value if value.starts_with("agents/") || value.starts_with("personas/") => {
                self.root.join(value)
            }
            value => self.agents_dir().join(value),
        }
    }
}

fn non_empty_env(name: &str) -> Option<OsString> {
    env::var_os(name).filter(|value| !value.is_empty())
}

fn list_scope(layout: &Layout, args: &ScopeArgs) -> Result<i32, CliError> {
    let path = layout.resolve_scope(args.scope.as_deref().or(Some("global")));
    require_dir(&path)?;

    for file in markdown_files(&path)? {
        if let Some(name) = file.file_name() {
            println!("{}", name.to_string_lossy());
        }
    }
    Ok(EXIT_OK)
}

fn index_scope(layout: &Layout, args: &ScopeArgs) -> Result<i32, CliError> {
    let path = layout.resolve_scope(args.scope.as_deref().or(Some("global")));
    require_dir(&path)?;

    let index = path.join("MEMORY.md");
    if !index.is_file() {
        return Err(CliError::runtime(format!(
            "no MEMORY.md in {}",
            display_path(&path)
        )));
    }

    let contents = fs::read_to_string(&index).map_err(|err| {
        CliError::runtime(format!("failed to read {}: {err}", display_path(&index)))
    })?;
    print!("{contents}");
    Ok(EXIT_OK)
}

fn list_named_dirs(path: &Path) -> Result<i32, CliError> {
    if !path.is_dir() {
        return Ok(EXIT_OK);
    }

    for dir in child_dirs(path)? {
        if let Some(name) = dir.file_name() {
            println!("{}", name.to_string_lossy());
        }
    }
    Ok(EXIT_OK)
}

fn init_agent(layout: &Layout, args: &IdArgs) -> Result<i32, CliError> {
    validate_id(&args.id)?;
    let path = layout.agents_dir().join(&args.id);
    if path.exists() {
        return Err(CliError::runtime(format!(
            "already exists: {}",
            display_path(&path)
        )));
    }

    fs::create_dir_all(&path).map_err(|err| {
        CliError::runtime(format!("failed to create {}: {err}", display_path(&path)))
    })?;
    fs::write(
        path.join("MEMORY.md"),
        format!("# Memory index ({})\n\n", args.id),
    )
    .map_err(|err| CliError::runtime(format!("failed to write MEMORY.md: {err}")))?;

    println!("created: {}", display_path(&path));
    Ok(EXIT_OK)
}

fn init_persona(layout: &Layout, args: &IdArgs) -> Result<i32, CliError> {
    validate_id(&args.id)?;
    let path = layout.personas_dir().join(&args.id);
    if path.exists() {
        return Err(CliError::runtime(format!(
            "already exists: {}",
            display_path(&path)
        )));
    }

    let memory_dir = path.join("memory");
    let claude_dir = path.join(".claude");
    fs::create_dir_all(&memory_dir).map_err(|err| {
        CliError::runtime(format!(
            "failed to create {}: {err}",
            display_path(&memory_dir)
        ))
    })?;
    fs::create_dir_all(&claude_dir).map_err(|err| {
        CliError::runtime(format!(
            "failed to create {}: {err}",
            display_path(&claude_dir)
        ))
    })?;

    fs::write(path.join("CLAUDE.md"), persona_claude_template(&args.id)).map_err(|err| {
        CliError::runtime(format!(
            "failed to write {}: {err}",
            display_path(&path.join("CLAUDE.md"))
        ))
    })?;
    fs::write(
        memory_dir.join("MEMORY.md"),
        format!("# Memory index ({} persona)\n\n", args.id),
    )
    .map_err(|err| CliError::runtime(format!("failed to write persona MEMORY.md: {err}")))?;

    let settings = json!({
        "autoMemoryDirectory": to_tilde(&memory_dir),
    });
    fs::write(
        claude_dir.join("settings.local.json"),
        format!(
            "{}\n",
            serde_json::to_string_pretty(&settings).expect("settings json should serialize")
        ),
    )
    .map_err(|err| CliError::runtime(format!("failed to write persona settings: {err}")))?;

    println!("created: {}", display_path(&path));
    println!("  next: $EDITOR {}/CLAUDE.md", display_path(&path));
    println!(
        "  launch: claude-{}  (after sourcing shell/agent-memory.zsh)",
        args.id
    );
    Ok(EXIT_OK)
}

fn resolve_agent(layout: &Layout, args: &IdArgs) -> Result<i32, CliError> {
    validate_id(&args.id)?;
    println!("global\t{}", display_path(&layout.global_dir()));
    println!(
        "agent\t{}",
        display_path(&layout.agents_dir().join(&args.id))
    );
    Ok(EXIT_OK)
}

fn print_env(layout: &Layout) {
    println!(
        "export AGENT_MEMORY_HOME={}",
        shell_escape(&display_path(&layout.root))
    );
    println!(
        "export AGENT_MEMORY_GLOBAL={}",
        shell_escape(&display_path(&layout.global_dir()))
    );
    println!(
        "export AGENT_MEMORY_AGENTS={}",
        shell_escape(&display_path(&layout.agents_dir()))
    );
    println!(
        "export AGENT_MEMORY_PERSONAS={}",
        shell_escape(&display_path(&layout.personas_dir()))
    );
}

fn doctor(layout: &Layout) -> Result<i32, CliError> {
    let mut failed = false;

    println!("AGENT_MEMORY_HOME={}", display_path(&layout.root));
    if layout.root.is_dir() {
        println!("  [ok]      root present");
    } else {
        eprintln!("  [missing] root");
        failed = true;
    }

    let global = layout.global_dir();
    match fs::symlink_metadata(&global) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            let target = fs::read_link(&global)
                .map(|path| display_path(&path))
                .unwrap_or_else(|_| "<unreadable>".to_string());
            if global.is_dir() {
                println!("  [ok]      global -> {target}");
            } else {
                eprintln!("  [broken]  global -> {target}");
                failed = true;
            }
        }
        Ok(metadata) if metadata.is_dir() => {
            println!("  [ok]      global (real dir)");
        }
        _ => {
            eprintln!("  [missing] global");
            failed = true;
        }
    }

    print_dir_count("agents/", &layout.agents_dir())?;
    print_dir_count("personas/", &layout.personas_dir())?;

    if failed {
        Ok(EXIT_RUNTIME)
    } else {
        Ok(EXIT_OK)
    }
}

fn print_dir_count(label: &str, path: &Path) -> Result<(), CliError> {
    if path.is_dir() {
        println!(
            "  [ok]      {label:<10}({} entries)",
            child_dirs(path)?.len()
        );
    } else if label == "agents/" {
        println!("  [empty]   agents/   (run 'agent-memory init-agent <id>')");
    } else if label == "personas/" {
        println!("  [empty]   personas/ (run 'agent-memory init-persona <id>')");
    } else {
        println!("  [empty]   {label:<10}");
    }
    Ok(())
}

fn require_dir(path: &Path) -> Result<(), CliError> {
    if path.is_dir() {
        Ok(())
    } else {
        Err(CliError::runtime(format!(
            "not found: {}",
            display_path(path)
        )))
    }
}

fn validate_id(id: &str) -> Result<(), CliError> {
    if id.is_empty() || id.contains('/') || id == "." || id == ".." {
        return Err(CliError::usage(format!("invalid id: '{id}'")));
    }
    Ok(())
}

fn markdown_files(path: &Path) -> Result<Vec<PathBuf>, CliError> {
    let mut files = Vec::new();
    for entry in fs::read_dir(path)
        .map_err(|err| CliError::runtime(format!("failed to read {}: {err}", display_path(path))))?
    {
        let entry =
            entry.map_err(|err| CliError::runtime(format!("failed to read entry: {err}")))?;
        let path = entry.path();
        if path.is_file() && path.extension().is_some_and(|ext| ext == "md") {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

fn child_dirs(path: &Path) -> Result<Vec<PathBuf>, CliError> {
    let mut dirs = Vec::new();
    for entry in fs::read_dir(path)
        .map_err(|err| CliError::runtime(format!("failed to read {}: {err}", display_path(path))))?
    {
        let entry =
            entry.map_err(|err| CliError::runtime(format!("failed to read entry: {err}")))?;
        let path = entry.path();
        if path.is_dir() {
            dirs.push(path);
        }
    }
    dirs.sort();
    Ok(dirs)
}

fn persona_claude_template(id: &str) -> String {
    format!(
        r#"# Persona: {id}

Claude Code session scoped to "{id}" persona. Loads on top of the base
`~/.claude/CLAUDE.md` policies (this file is additive, not a replacement).

## Scope

- In scope: <fill in what this persona handles>
- Out of scope: anything outside the persona's domain - recommend exiting
  to base `claude` or another persona.

## Memory

- Auto-memory store: `./memory/` (this persona's isolated scope, wired via
  `.claude/settings.local.json`).
- Cross-persona facts (shell, git identity, host) belong in global memory,
  not here.
"#
    )
}

fn to_tilde(path: &Path) -> String {
    let Some(home) = env::var_os("HOME") else {
        return display_path(path);
    };
    let home = PathBuf::from(home);
    if path == home {
        return "~".to_string();
    }
    if let Ok(rest) = path.strip_prefix(&home) {
        if rest.as_os_str().is_empty() {
            "~".to_string()
        } else {
            format!("~/{}", rest.to_string_lossy())
        }
    } else {
        display_path(path)
    }
}

fn shell_escape(value: &str) -> String {
    if value.is_empty() {
        return "''".to_string();
    }

    if value.bytes().all(is_shell_safe_byte) {
        return value.to_string();
    }

    format!("'{}'", value.replace('\'', "'\\''"))
}

fn is_shell_safe_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'_' | b'@' | b'%' | b'+' | b'=' | b':' | b',' | b'.' | b'/' | b'-'
        )
}
