use crate::{open, util};
use std::collections::HashSet;
use std::env;
use std::fs;
use std::io::{self, IsTerminal, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const DEFAULT_MAX_FILES: usize = 5;
const GIT_ROOT_MAX_DEPTH: usize = 5;
const BATCH_SIZE: usize = 50;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SourceMode {
    List,
    Git,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WorkspaceMode {
    Pwd,
    Git,
}

#[derive(Debug)]
struct Options {
    source_mode: SourceMode,
    workspace_mode: WorkspaceMode,
    dry_run: bool,
    verbose: bool,
    max_files: usize,
    files: Vec<String>,
}

enum CodeResolution {
    Disabled,
    Available(String),
}

pub fn run(args: &[String]) -> i32 {
    let options = match parse_options(args) {
        Ok(options) => options,
        Err(code) => return code,
    };

    if options.max_files == 0 {
        return 0;
    }

    let files = match options.source_mode {
        SourceMode::List => collect_list_files(&options.files, options.verbose),
        SourceMode::Git => collect_git_files(options.verbose),
    };

    open_vscode_files(
        files,
        options.workspace_mode,
        options.max_files,
        options.dry_run,
        options.verbose,
    )
}

pub(crate) fn open_vscode_files(
    mut files: Vec<PathBuf>,
    workspace_mode: WorkspaceMode,
    max_files: usize,
    dry_run: bool,
    verbose: bool,
) -> i32 {
    if max_files == 0 {
        return 0;
    }

    let code_path = match resolve_code_path(dry_run, verbose) {
        CodeResolution::Disabled => return 0,
        CodeResolution::Available(path) => path,
    };

    files.retain(|file| file.is_file());
    if files.is_empty() {
        return 0;
    }
    files.truncate(max_files);

    let pwd_workspace = current_dir_abs();
    let groups = group_by_workspace(&files, workspace_mode, &pwd_workspace);
    for WorkspaceGroup(workspace, workspace_files) in groups {
        for (idx, batch) in workspace_files.chunks(BATCH_SIZE).enumerate() {
            let window_mode = if idx == 0 {
                WindowMode::New
            } else {
                WindowMode::Reuse
            };
            if dry_run {
                println!(
                    "{}",
                    format_code_invocation(&code_path, window_mode, &workspace, batch)
                );
            } else if let Some(code) =
                run_code_invocation(&code_path, window_mode, &workspace, batch)
            {
                return code;
            }
        }
    }

    0
}

fn parse_options(args: &[String]) -> Result<Options, i32> {
    let mut seen_list = false;
    let mut seen_git = false;
    let mut dry_run = false;
    let mut verbose = false;
    let mut workspace_raw: Option<String> = None;
    let mut max_raw: Option<String> = None;
    let mut files: Vec<String> = Vec::new();

    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        match arg.as_str() {
            "-h" | "--help" => {
                print_usage();
                return Err(0);
            }
            "--list" => {
                seen_list = true;
                i += 1;
            }
            "--git" => {
                seen_git = true;
                i += 1;
            }
            "--dry-run" => {
                dry_run = true;
                i += 1;
            }
            "-v" | "--verbose" => {
                verbose = true;
                i += 1;
            }
            "--workspace-mode" => {
                let Some(value) = args.get(i + 1) else {
                    return die_usage("--workspace-mode requires a value");
                };
                workspace_raw = Some(value.clone());
                i += 2;
            }
            "--max-files" => {
                let Some(value) = args.get(i + 1) else {
                    return die_usage("--max-files requires a value");
                };
                max_raw = Some(value.clone());
                i += 2;
            }
            "--" => {
                files.extend_from_slice(&args[i + 1..]);
                break;
            }
            _ if arg.starts_with("--workspace-mode=") => {
                workspace_raw = Some(arg["--workspace-mode=".len()..].to_string());
                i += 1;
            }
            _ if arg.starts_with("--max-files=") => {
                max_raw = Some(arg["--max-files=".len()..].to_string());
                i += 1;
            }
            _ if arg.starts_with("--") => {
                return die_usage(&format!("Unknown flag: {arg}"));
            }
            _ => {
                files.push(arg.clone());
                i += 1;
            }
        }
    }

    if seen_list && seen_git {
        return die_usage("Flags are mutually exclusive: --list and --git");
    }

    let source_raw = env::var("OPEN_CHANGED_FILES_SOURCE").unwrap_or_else(|_| "list".to_string());
    let mut source_mode = match source_raw.as_str() {
        "list" => SourceMode::List,
        "git" => SourceMode::Git,
        _ => {
            return die_usage(&format!(
                "Invalid OPEN_CHANGED_FILES_SOURCE: {source_raw} (expected: list|git)"
            ));
        }
    };
    if seen_list {
        source_mode = SourceMode::List;
    }
    if seen_git {
        source_mode = SourceMode::Git;
    }

    let workspace_raw = workspace_raw.unwrap_or_else(|| {
        env::var("OPEN_CHANGED_FILES_WORKSPACE_MODE").unwrap_or_else(|_| "pwd".to_string())
    });
    let workspace_mode = match workspace_raw.as_str() {
        "pwd" => WorkspaceMode::Pwd,
        "git" => WorkspaceMode::Git,
        _ => {
            return die_usage(&format!(
                "Invalid workspace mode: {workspace_raw} (expected: pwd|git)"
            ));
        }
    };

    let max_raw = max_raw.unwrap_or_else(|| {
        env::var("OPEN_CHANGED_FILES_MAX_FILES").unwrap_or_else(|_| DEFAULT_MAX_FILES.to_string())
    });
    let max_files = match max_raw.parse::<usize>() {
        Ok(value) => value,
        Err(_) => return die_usage(&format!("Invalid --max-files: {max_raw}")),
    };

    Ok(Options {
        source_mode,
        workspace_mode,
        dry_run,
        verbose,
        max_files,
        files,
    })
}

fn print_usage() {
    println!("Open changed files in VSCode.");
    println!();
    println!("Usage:");
    println!(
        "  fzf-cli open-changed-files [--list|--git] [--workspace-mode pwd|git] [--dry-run] [--verbose] [--max-files N] [--] [files...]"
    );
}

fn die_usage(message: &str) -> Result<Options, i32> {
    eprintln!("❌ {message}");
    eprintln!();
    print_usage_stderr();
    Err(2)
}

fn print_usage_stderr() {
    eprintln!("Open changed files in VSCode.");
    eprintln!();
    eprintln!("Usage:");
    eprintln!(
        "  fzf-cli open-changed-files [--list|--git] [--workspace-mode pwd|git] [--dry-run] [--verbose] [--max-files N] [--] [files...]"
    );
}

fn log_verbose(verbose: bool, message: impl AsRef<str>) {
    if verbose {
        eprintln!("{}", message.as_ref());
    }
}

fn collect_list_files(inputs: &[String], verbose: bool) -> Vec<PathBuf> {
    let mut raw_inputs = inputs.to_vec();
    if raw_inputs.is_empty() && !io::stdin().is_terminal() {
        let mut input = String::new();
        if io::stdin().read_to_string(&mut input).is_ok() {
            raw_inputs.extend(
                input
                    .lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty())
                    .map(ToOwned::to_owned),
            );
        }
    }

    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for raw in raw_inputs {
        if raw.is_empty() {
            continue;
        }
        let candidate = abs_path(Path::new(&raw));
        if !candidate.is_file() {
            log_verbose(
                verbose,
                format!("skip: not a file: {}", candidate.display()),
            );
            continue;
        }
        let abs = canonical_or_abs(&candidate);
        if seen.insert(abs.clone()) {
            out.push(abs);
        }
    }
    out
}

fn collect_git_files(verbose: bool) -> Vec<PathBuf> {
    if !util::cmd_exists("git") {
        log_verbose(verbose, "no-op: git not found");
        return Vec::new();
    }

    let Ok(inside) = util::run_output("git", &["rev-parse", "--is-inside-work-tree"]) else {
        log_verbose(verbose, "no-op: not inside a git work tree");
        return Vec::new();
    };
    if !inside.status.success() {
        log_verbose(verbose, "no-op: not inside a git work tree");
        return Vec::new();
    }

    let repo_root_raw = match util::run_capture("git", &["rev-parse", "--show-toplevel"]) {
        Ok(value) => value,
        Err(_) => return Vec::new(),
    };
    let repo_root = canonical_or_abs(Path::new(repo_root_raw.trim()));

    let mut candidates: Vec<String> = Vec::new();
    for args in [
        &["diff", "--name-only", "--cached"][..],
        &["diff", "--name-only"][..],
        &["ls-files", "--others", "--exclude-standard"][..],
    ] {
        let output = util::run_capture("git", args).unwrap_or_default();
        candidates.extend(
            output
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(ToOwned::to_owned),
        );
    }

    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for rel in candidates {
        let abs = canonical_or_abs(&repo_root.join(rel));
        if abs.is_file() && seen.insert(abs.clone()) {
            out.push(abs);
        }
    }
    out
}

fn resolve_code_path(dry_run: bool, verbose: bool) -> CodeResolution {
    let override_raw = env::var("OPEN_CHANGED_FILES_CODE_PATH").unwrap_or_default();
    if !override_raw.is_empty() && override_raw != "auto" {
        if override_raw == "none" {
            log_verbose(verbose, "no-op: code disabled");
            return CodeResolution::Disabled;
        }

        if dry_run {
            return CodeResolution::Available(override_raw);
        }

        if let Some(path) = util::find_in_path(&override_raw) {
            return CodeResolution::Available(path.to_string_lossy().to_string());
        }

        log_verbose(
            verbose,
            format!("no-op: code override not found: {override_raw}"),
        );
        return CodeResolution::Disabled;
    }

    if let Some(path) = util::find_in_path("code") {
        return CodeResolution::Available(path.to_string_lossy().to_string());
    }
    for candidate in code_candidates() {
        if is_executable_file(&candidate) {
            return CodeResolution::Available(candidate.to_string_lossy().to_string());
        }
    }

    if dry_run {
        CodeResolution::Available("code".to_string())
    } else {
        log_verbose(verbose, "no-op: 'code' not found");
        CodeResolution::Disabled
    }
}

fn code_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    let home = env::var("HOME").ok();
    if cfg!(target_os = "macos") {
        candidates.extend([
            PathBuf::from("/usr/local/bin/code"),
            PathBuf::from("/opt/homebrew/bin/code"),
            PathBuf::from("/Applications/Visual Studio Code.app/Contents/Resources/app/bin/code"),
            PathBuf::from(
                "/Applications/Visual Studio Code - Insiders.app/Contents/Resources/app/bin/code",
            ),
        ]);
        if let Some(home) = home {
            candidates.push(
                PathBuf::from(&home)
                    .join("Applications/Visual Studio Code.app/Contents/Resources/app/bin/code"),
            );
            candidates.push(PathBuf::from(home).join(
                "Applications/Visual Studio Code - Insiders.app/Contents/Resources/app/bin/code",
            ));
        }
    } else {
        candidates.extend([
            PathBuf::from("/usr/bin/code"),
            PathBuf::from("/usr/local/bin/code"),
            PathBuf::from("/snap/bin/code"),
            PathBuf::from("/var/lib/flatpak/exports/bin/com.visualstudio.code"),
        ]);
        if let Some(home) = home {
            candidates.push(PathBuf::from(&home).join(".local/bin/code"));
            candidates.push(PathBuf::from(&home).join("bin/code"));
            candidates.push(PathBuf::from(home).join(".linuxbrew/bin/code"));
        }
    }
    candidates
}

fn group_by_workspace(
    files: &[PathBuf],
    workspace_mode: WorkspaceMode,
    pwd_workspace: &Path,
) -> Vec<WorkspaceGroup> {
    let mut groups: Vec<WorkspaceGroup> = Vec::new();
    for file in files {
        let workspace = match workspace_mode {
            WorkspaceMode::Pwd => pwd_workspace.to_path_buf(),
            WorkspaceMode::Git => file
                .parent()
                .and_then(|parent| open::find_git_root_upwards(parent, GIT_ROOT_MAX_DEPTH))
                .map(|path| canonical_or_abs(&path))
                .unwrap_or_else(|| pwd_workspace.to_path_buf()),
        };

        if let Some(group) = groups.iter_mut().find(|group| group.0 == workspace) {
            group.1.push(file.clone());
        } else {
            groups.push(WorkspaceGroup(workspace, vec![file.clone()]));
        }
    }
    groups
}

struct WorkspaceGroup(PathBuf, Vec<PathBuf>);

#[derive(Clone, Copy)]
enum WindowMode {
    New,
    Reuse,
}

fn format_code_invocation(
    code_path: &str,
    window_mode: WindowMode,
    workspace: &Path,
    files: &[PathBuf],
) -> String {
    let mut args = code_args(code_path, window_mode, workspace, files);
    args.iter_mut().for_each(|arg| *arg = shell_quote(arg));
    args.join(" ")
}

fn run_code_invocation(
    code_path: &str,
    window_mode: WindowMode,
    workspace: &Path,
    files: &[PathBuf],
) -> Option<i32> {
    let mut command = Command::new(code_path);
    for arg in code_args("", window_mode, workspace, files)
        .into_iter()
        .skip(1)
    {
        command.arg(arg);
    }
    let status = command
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status();
    Some(status.ok().and_then(|status| status.code()).unwrap_or(1)).filter(|code| *code != 0)
}

fn code_args(
    code_path: &str,
    window_mode: WindowMode,
    workspace: &Path,
    files: &[PathBuf],
) -> Vec<String> {
    let mut args = Vec::new();
    args.push(code_path.to_string());
    match window_mode {
        WindowMode::New => args.push("--new-window".to_string()),
        WindowMode::Reuse => args.push("--reuse-window".to_string()),
    }
    args.push("--".to_string());
    args.push(workspace.to_string_lossy().to_string());
    args.extend(files.iter().map(|file| file.to_string_lossy().to_string()));
    args
}

fn shell_quote(input: &str) -> String {
    if input.is_empty() {
        return "''".to_string();
    }
    if input
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b'/' | b':' | b'='))
    {
        return input.to_string();
    }
    format!("'{}'", input.replace('\'', "'\\''"))
}

fn current_dir_abs() -> PathBuf {
    env::current_dir()
        .map(|path| canonical_or_abs(&path))
        .unwrap_or_else(|_| PathBuf::from("."))
}

fn abs_path(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        current_dir_abs().join(path)
    }
}

fn canonical_or_abs(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| abs_path(path))
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn parse_options_accepts_core_flags() {
        let args = vec![
            "--git".to_string(),
            "--dry-run".to_string(),
            "--workspace-mode=git".to_string(),
            "--max-files".to_string(),
            "9".to_string(),
        ];
        let options = parse_options(&args).expect("parse");
        assert_eq!(options.source_mode, SourceMode::Git);
        assert_eq!(options.workspace_mode, WorkspaceMode::Git);
        assert!(options.dry_run);
        assert_eq!(options.max_files, 9);
    }

    #[test]
    fn shell_quote_preserves_safe_paths_and_quotes_spaces() {
        assert_eq!(shell_quote("/tmp/a.txt"), "/tmp/a.txt");
        assert_eq!(shell_quote("/tmp/a file.txt"), "'/tmp/a file.txt'");
    }
}
