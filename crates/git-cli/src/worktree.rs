use crate::commit_shared::{git_output, git_status_success, git_stdout_trimmed_optional};
use anyhow::Context;
use nils_common::cli_contract::{Envelope, EnvelopeError, OutputFormat, exit, schema_version_for};
use serde::Serialize;
use serde_json::json;
use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Component, Path, PathBuf};

const BINARY: &str = "git-cli";

pub fn dispatch(cmd: &str, args: &[String]) -> Option<i32> {
    match cmd {
        "add" => Some(run_add(args)),
        "list" => Some(run_list(args)),
        "remove" => Some(run_remove(args)),
        "prune" => Some(run_prune(args)),
        _ => None,
    }
}

#[derive(Debug, Clone)]
pub(crate) struct LinkedWorktree {
    pub path: PathBuf,
    pub head: Option<String>,
    pub branch: Option<String>,
    pub bare: bool,
    pub detached: bool,
    pub prunable: Option<String>,
}

#[derive(Debug)]
struct WorktreeLayout {
    agent_home: PathBuf,
    worktree_root: PathBuf,
    repo_root: PathBuf,
    repo_key: String,
}

#[derive(Debug)]
struct AddArgs {
    slug: String,
    from: Option<String>,
    format: OutputFormat,
}

#[derive(Debug)]
struct ListArgs {
    format: OutputFormat,
}

#[derive(Debug)]
struct RemoveArgs {
    target: String,
    format: OutputFormat,
}

#[derive(Debug)]
struct PruneArgs {
    format: OutputFormat,
}

#[derive(Debug, Clone)]
struct CliError {
    code: &'static str,
    message: Box<str>,
    hint: Option<Box<str>>,
    exit_code: i32,
    details: Option<Box<serde_json::Value>>,
}

#[derive(Debug, Serialize)]
struct AddOutput {
    agent_home: String,
    worktree_root: String,
    repo_root: String,
    repo_key: String,
    slug: String,
    branch: String,
    base_ref: String,
    path: String,
}

#[derive(Debug, Serialize)]
struct ListOutput {
    agent_home: String,
    worktree_root: String,
    repo_root: String,
    repo_key: String,
    entries: Vec<WorktreeEntryOutput>,
}

#[derive(Debug, Serialize)]
struct WorktreeEntryOutput {
    path: String,
    head: Option<String>,
    branch: Option<String>,
    bare: bool,
    detached: bool,
    prunable: Option<String>,
    managed: bool,
}

#[derive(Debug, Serialize)]
struct RemoveOutput {
    removed_path: String,
    pruned: bool,
}

#[derive(Debug, Serialize)]
struct PruneOutput {
    pruned: bool,
}

impl CliError {
    fn usage(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into().into_boxed_str(),
            hint: None,
            exit_code: exit::USAGE,
            details: None,
        }
    }

    fn runtime(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into().into_boxed_str(),
            hint: None,
            exit_code: exit::RUNTIME,
            details: None,
        }
    }

    fn data(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into().into_boxed_str(),
            hint: None,
            exit_code: exit::DATA,
            details: None,
        }
    }

    fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into().into_boxed_str());
        self
    }

    fn with_details(mut self, details: serde_json::Value) -> Self {
        self.details = Some(Box::new(details));
        self
    }
}

fn run_add(args: &[String]) -> i32 {
    let requested_format = detect_format(args);
    let parsed = match parse_add_args(args) {
        Ok(parsed) => parsed,
        Err(err) => return emit_error("worktree.add", requested_format, err),
    };

    match add_worktree(&parsed) {
        Ok(output) => emit_success("worktree.add", parsed.format, &output, || {
            format!(
                "Created worktree {}\nBranch: {}\nBase: {}",
                output.path, output.branch, output.base_ref
            )
        }),
        Err(err) => emit_error("worktree.add", parsed.format, err),
    }
}

fn run_list(args: &[String]) -> i32 {
    let requested_format = detect_format(args);
    let parsed = match parse_list_args(args) {
        Ok(parsed) => parsed,
        Err(err) => return emit_error("worktree.list", requested_format, err),
    };

    match list_worktrees() {
        Ok(output) => emit_success("worktree.list", parsed.format, &output, || {
            render_list_text(&output)
        }),
        Err(err) => emit_error("worktree.list", parsed.format, err),
    }
}

fn run_remove(args: &[String]) -> i32 {
    let requested_format = detect_format(args);
    let parsed = match parse_remove_args(args) {
        Ok(parsed) => parsed,
        Err(err) => return emit_error("worktree.remove", requested_format, err),
    };

    match remove_worktree(&parsed) {
        Ok(output) => emit_success("worktree.remove", parsed.format, &output, || {
            format!(
                "Removed worktree {}\nPruned stale worktree metadata",
                output.removed_path
            )
        }),
        Err(err) => emit_error("worktree.remove", parsed.format, err),
    }
}

fn run_prune(args: &[String]) -> i32 {
    let requested_format = detect_format(args);
    let parsed = match parse_prune_args(args) {
        Ok(parsed) => parsed,
        Err(err) => return emit_error("worktree.prune", requested_format, err),
    };

    match prune_worktrees() {
        Ok(output) => emit_success("worktree.prune", parsed.format, &output, || {
            "Pruned stale worktree metadata".to_string()
        }),
        Err(err) => emit_error("worktree.prune", parsed.format, err),
    }
}

fn add_worktree(args: &AddArgs) -> Result<AddOutput, CliError> {
    let layout = resolve_layout()?;
    let branch_slug = normalize_slug(&args.slug)?;
    let branch = format!("feat/{branch_slug}");
    let path = layout
        .worktree_root
        .join(&layout.repo_key)
        .join(&branch_slug);
    let base_ref = args.from.clone().unwrap_or_else(resolve_default_base_ref);

    let branch_ref = format!("refs/heads/{branch}");
    if git_status_success(&["show-ref", "--verify", "--quiet", &branch_ref]) {
        return Err(
            CliError::data("branch-exists", format!("branch {branch} already exists"))
                .with_hint("use a different slug or remove the existing branch first"),
        );
    }

    if path.exists() && !is_empty_dir(&path).unwrap_or(false) {
        return Err(CliError::data(
            "worktree-path-exists",
            format!(
                "managed worktree path already exists: {}",
                display_path(&path)
            ),
        )
        .with_details(json!({ "path": display_path(&path) })));
    }

    let parent = path.parent().ok_or_else(|| {
        CliError::runtime(
            "invalid-worktree-path",
            format!("unable to resolve parent for {}", display_path(&path)),
        )
    })?;
    fs::create_dir_all(parent).map_err(|err| {
        CliError::runtime(
            "create-worktree-parent-failed",
            format!("failed to create {}: {err}", display_path(parent)),
        )
    })?;

    let path_arg = display_path(&path);
    git_output(&[
        "worktree",
        "add",
        "-b",
        branch.as_str(),
        path_arg.as_str(),
        base_ref.as_str(),
    ])
    .map_err(|err| {
        CliError::runtime(
            "git-worktree-add-failed",
            summarize_git_error(&err.to_string()),
        )
    })?;

    Ok(AddOutput {
        agent_home: display_path(&layout.agent_home),
        worktree_root: display_path(&layout.worktree_root),
        repo_root: display_path(&layout.repo_root),
        repo_key: layout.repo_key,
        slug: branch_slug,
        branch,
        base_ref,
        path: path_arg,
    })
}

fn list_worktrees() -> Result<ListOutput, CliError> {
    let layout = resolve_layout()?;
    let entries = list_linked_worktrees()
        .map_err(|err| CliError::runtime("git-worktree-list-failed", err.to_string()))?
        .into_iter()
        .map(|entry| {
            let managed = is_managed_worktree(&entry.path, &layout);
            WorktreeEntryOutput {
                path: display_path(&entry.path),
                head: entry.head,
                branch: entry.branch,
                bare: entry.bare,
                detached: entry.detached,
                prunable: entry.prunable,
                managed,
            }
        })
        .collect();

    Ok(ListOutput {
        agent_home: display_path(&layout.agent_home),
        worktree_root: display_path(&layout.worktree_root),
        repo_root: display_path(&layout.repo_root),
        repo_key: layout.repo_key,
        entries,
    })
}

fn remove_worktree(args: &RemoveArgs) -> Result<RemoveOutput, CliError> {
    let layout = resolve_layout()?;
    let target = resolve_remove_target(&args.target, &layout)?;
    let target_key = path_key(&target);
    let repo_root_key = path_key(&layout.repo_root);

    if target_key == repo_root_key {
        return Err(CliError::data(
            "refuse-primary-worktree",
            "refusing to remove the primary repository worktree",
        )
        .with_details(json!({ "path": display_path(&target) })));
    }

    let cwd = env::current_dir().map_err(|err| {
        CliError::runtime(
            "current-dir-failed",
            format!("failed to read current dir: {err}"),
        )
    })?;
    let cwd_key = path_key(&cwd);
    if cwd_key == target_key || cwd_key.starts_with(&(target_key.clone() + "/")) {
        return Err(CliError::data(
            "refuse-current-worktree",
            format!(
                "refusing to remove the current worktree {}",
                display_path(&target)
            ),
        ));
    }

    let entries = list_linked_worktrees()
        .map_err(|err| CliError::runtime("git-worktree-list-failed", err.to_string()))?;
    let known = entries
        .iter()
        .any(|entry| path_key(&entry.path) == target_key);
    if !known {
        return Err(CliError::data(
            "worktree-not-found",
            format!("no linked worktree found for {}", args.target),
        )
        .with_details(json!({ "target": args.target })));
    }

    let target_arg = display_path(&target);
    git_output(&["worktree", "remove", "--force", target_arg.as_str()]).map_err(|err| {
        CliError::runtime(
            "git-worktree-remove-failed",
            summarize_git_error(&err.to_string()),
        )
    })?;
    run_git_worktree_prune()?;

    Ok(RemoveOutput {
        removed_path: target_arg,
        pruned: true,
    })
}

fn prune_worktrees() -> Result<PruneOutput, CliError> {
    ensure_inside_git_repo()?;
    run_git_worktree_prune()?;
    Ok(PruneOutput { pruned: true })
}

pub(crate) fn linked_worktrees_by_branch() -> anyhow::Result<HashMap<String, String>> {
    let entries = list_linked_worktrees()?;
    Ok(entries
        .into_iter()
        .filter_map(|entry| {
            let branch = entry.branch?;
            Some((branch, display_path(&entry.path)))
        })
        .collect())
}

fn list_linked_worktrees() -> anyhow::Result<Vec<LinkedWorktree>> {
    let output = git_output(&["worktree", "list", "--porcelain"])?;
    let stdout = String::from_utf8(output.stdout).context("git worktree output was not UTF-8")?;
    Ok(parse_worktree_porcelain(&stdout))
}

fn parse_worktree_porcelain(stdout: &str) -> Vec<LinkedWorktree> {
    let mut entries = Vec::new();
    let mut current: Option<LinkedWorktree> = None;

    for line in stdout.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            if let Some(previous) = current.take() {
                entries.push(previous);
            }
            current = Some(LinkedWorktree {
                path: PathBuf::from(path.trim()),
                head: None,
                branch: None,
                bare: false,
                detached: false,
                prunable: None,
            });
            continue;
        }

        if line.trim().is_empty() {
            if let Some(previous) = current.take() {
                entries.push(previous);
            }
            continue;
        }

        let Some(entry) = current.as_mut() else {
            continue;
        };

        if let Some(head) = line.strip_prefix("HEAD ") {
            entry.head = Some(head.trim().to_string());
        } else if let Some(branch) = line.strip_prefix("branch ") {
            entry.branch = Some(branch.trim().trim_start_matches("refs/heads/").to_string());
        } else if line.trim() == "bare" {
            entry.bare = true;
        } else if line.trim() == "detached" {
            entry.detached = true;
        } else if let Some(reason) = line.strip_prefix("prunable ") {
            entry.prunable = Some(reason.trim().to_string());
        }
    }

    if let Some(previous) = current {
        entries.push(previous);
    }

    entries
}

fn parse_add_args(args: &[String]) -> Result<AddArgs, CliError> {
    let mut args = args.to_vec();
    let format = take_format(&mut args)?;
    if take_help(&args) {
        print_add_help();
        return Err(CliError::usage("help", "help requested"));
    }

    let mut slug: Option<String> = None;
    let mut from: Option<String> = None;
    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--from" => {
                let Some(value) = args.get(i + 1) else {
                    return Err(CliError::usage("missing-from", "--from requires a value"));
                };
                from = Some(value.to_string());
                i += 2;
            }
            value if value.starts_with("--from=") => {
                from = Some(value.trim_start_matches("--from=").to_string());
                i += 1;
            }
            value if value.starts_with('-') => {
                return Err(CliError::usage(
                    "unknown-argument",
                    format!("unknown argument: {value}"),
                ));
            }
            value => {
                if slug.is_some() {
                    return Err(CliError::usage(
                        "too-many-arguments",
                        "worktree add accepts exactly one slug",
                    ));
                }
                slug = Some(value.to_string());
                i += 1;
            }
        }
    }

    let slug = slug.ok_or_else(|| CliError::usage("missing-slug", "missing worktree slug"))?;
    Ok(AddArgs { slug, from, format })
}

fn parse_list_args(args: &[String]) -> Result<ListArgs, CliError> {
    let mut args = args.to_vec();
    let format = take_format(&mut args)?;
    if take_help(&args) {
        print_list_help();
        return Err(CliError::usage("help", "help requested"));
    }
    reject_extra_args("worktree list", &args)?;
    Ok(ListArgs { format })
}

fn parse_remove_args(args: &[String]) -> Result<RemoveArgs, CliError> {
    let mut args = args.to_vec();
    let format = take_format(&mut args)?;
    if take_help(&args) {
        print_remove_help();
        return Err(CliError::usage("help", "help requested"));
    }

    reject_unknown_flags(&args)?;
    let positionals: Vec<_> = args.iter().filter(|arg| !arg.starts_with('-')).collect();
    if positionals.len() != 1 {
        return Err(CliError::usage(
            "invalid-target-count",
            "worktree remove accepts exactly one slug or path",
        ));
    }
    Ok(RemoveArgs {
        target: positionals[0].to_string(),
        format,
    })
}

fn parse_prune_args(args: &[String]) -> Result<PruneArgs, CliError> {
    let mut args = args.to_vec();
    let format = take_format(&mut args)?;
    if take_help(&args) {
        print_prune_help();
        return Err(CliError::usage("help", "help requested"));
    }
    reject_extra_args("worktree prune", &args)?;
    Ok(PruneArgs { format })
}

fn take_format(args: &mut Vec<String>) -> Result<OutputFormat, CliError> {
    let mut format = OutputFormat::Text;
    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--format" => {
                let Some(value) = args.get(i + 1) else {
                    return Err(CliError::usage(
                        "missing-format",
                        "--format requires text or json",
                    ));
                };
                format = parse_format_value(value)?;
                args.drain(i..=i + 1);
            }
            value if value.starts_with("--format=") => {
                let value = value.trim_start_matches("--format=");
                format = parse_format_value(value)?;
                args.remove(i);
            }
            _ => i += 1,
        }
    }
    Ok(format)
}

fn parse_format_value(value: &str) -> Result<OutputFormat, CliError> {
    match value {
        "text" => Ok(OutputFormat::Text),
        "json" => Ok(OutputFormat::Json),
        other => Err(CliError::usage(
            "invalid-format",
            format!("unsupported output format: {other}"),
        )),
    }
}

fn detect_format(args: &[String]) -> OutputFormat {
    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--format" if args.get(i + 1).is_some_and(|value| value == "json") => {
                return OutputFormat::Json;
            }
            "--format" => i += 2,
            "--format=json" => return OutputFormat::Json,
            _ => i += 1,
        }
    }
    OutputFormat::Text
}

fn take_help(args: &[String]) -> bool {
    args.iter()
        .any(|arg| matches!(arg.as_str(), "-h" | "--help"))
}

fn reject_unknown_flags(args: &[String]) -> Result<(), CliError> {
    if let Some(flag) = args.iter().find(|arg| arg.starts_with('-')) {
        return Err(CliError::usage(
            "unknown-argument",
            format!("unknown argument: {flag}"),
        ));
    }
    Ok(())
}

fn reject_extra_args(command: &str, args: &[String]) -> Result<(), CliError> {
    reject_unknown_flags(args)?;
    if !args.is_empty() {
        return Err(CliError::usage(
            "too-many-arguments",
            format!("{command} does not accept positional arguments"),
        ));
    }
    Ok(())
}

fn print_add_help() {
    println!("Usage: git-cli worktree add <slug> [--from <ref>] [--format text|json]");
}

fn print_list_help() {
    println!("Usage: git-cli worktree list [--format text|json]");
}

fn print_remove_help() {
    println!("Usage: git-cli worktree remove <slug-or-path> [--format text|json]");
}

fn print_prune_help() {
    println!("Usage: git-cli worktree prune [--format text|json]");
}

fn resolve_layout() -> Result<WorktreeLayout, CliError> {
    let repo_root = require_repo_root()?;
    let agent_home = resolve_agent_home()?;
    let worktree_root = agent_home.join("worktrees");
    let repo_key = repo_key_for_path(&repo_root);
    Ok(WorktreeLayout {
        agent_home,
        worktree_root,
        repo_root,
        repo_key,
    })
}

fn ensure_inside_git_repo() -> Result<(), CliError> {
    if git_status_success(&["rev-parse", "--is-inside-work-tree"]) {
        Ok(())
    } else {
        Err(CliError::runtime(
            "not-git-repository",
            "not in a git repository",
        ))
    }
}

fn require_repo_root() -> Result<PathBuf, CliError> {
    ensure_inside_git_repo()?;
    let root = git_stdout_trimmed_optional(&["rev-parse", "--show-toplevel"]).ok_or_else(|| {
        CliError::runtime(
            "repo-root-unavailable",
            "unable to resolve git repository root",
        )
    })?;
    absolute_existing_path(Path::new(&root))
}

fn resolve_agent_home() -> Result<PathBuf, CliError> {
    if let Some(value) = env_non_empty("AGENT_HOME") {
        return absolute_or_existing_path(Path::new(&value));
    }
    if let Some(value) = env_non_empty("XDG_STATE_HOME") {
        return absolute_or_existing_path(&Path::new(&value).join("agent-runtime-kit"));
    }
    if let Some(value) = env_non_empty("HOME") {
        return absolute_or_existing_path(
            &Path::new(&value).join(".local/state/agent-runtime-kit"),
        );
    }
    Ok(env::temp_dir().join("agent-runtime-kit"))
}

fn env_non_empty(key: &str) -> Option<String> {
    env::var_os(key)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string_lossy().to_string())
}

fn absolute_path(path: &Path) -> Result<PathBuf, CliError> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    let cwd = env::current_dir().map_err(|err| {
        CliError::runtime(
            "current-dir-failed",
            format!("failed to read current dir: {err}"),
        )
    })?;
    Ok(cwd.join(path))
}

fn absolute_or_existing_path(path: &Path) -> Result<PathBuf, CliError> {
    if path.exists() {
        return absolute_existing_path(path);
    }
    absolute_path(path)
}

fn absolute_existing_path(path: &Path) -> Result<PathBuf, CliError> {
    fs::canonicalize(path).map_err(|err| {
        CliError::runtime(
            "canonicalize-path-failed",
            format!("failed to canonicalize {}: {err}", display_path(path)),
        )
    })
}

fn repo_key_for_path(repo_root: &Path) -> String {
    let basename = repo_root
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("repo");
    let slug = sanitize_segment(basename, "repo");
    let hash = stable_short_hash(&display_path(repo_root));
    format!("{slug}-{hash}")
}

fn normalize_slug(value: &str) -> Result<String, CliError> {
    let slug = sanitize_segment(value, "");
    if slug.is_empty() {
        return Err(CliError::usage(
            "invalid-slug",
            "slug must contain at least one ASCII letter or digit",
        ));
    }
    Ok(slug)
}

fn sanitize_segment(value: &str, fallback: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if matches!(ch, '-' | '_' | '.') {
            out.push(ch);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }

    let trimmed = out.trim_matches(['-', '_', '.']);
    let mut sanitized: String = trimmed.chars().take(80).collect();
    sanitized = sanitized.trim_matches(['-', '_', '.']).to_string();

    if sanitized.is_empty() {
        fallback.to_string()
    } else {
        sanitized
    }
}

fn stable_short_hash(value: &str) -> String {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{:08x}", hash as u32)
}

fn resolve_default_base_ref() -> String {
    if let Some(origin_head) = git_stdout_trimmed_optional(&[
        "symbolic-ref",
        "--quiet",
        "--short",
        "refs/remotes/origin/HEAD",
    ]) {
        return origin_head;
    }
    if git_status_success(&["show-ref", "--verify", "--quiet", "refs/heads/main"]) {
        return "main".to_string();
    }
    if git_status_success(&["show-ref", "--verify", "--quiet", "refs/heads/master"]) {
        return "master".to_string();
    }
    "HEAD".to_string()
}

fn resolve_remove_target(target: &str, layout: &WorktreeLayout) -> Result<PathBuf, CliError> {
    let candidate = Path::new(target);
    if candidate.is_absolute() || path_has_multiple_components(candidate) {
        return absolute_path(candidate);
    }

    let slug = normalize_slug(target)?;
    Ok(layout.worktree_root.join(&layout.repo_key).join(slug))
}

fn path_has_multiple_components(path: &Path) -> bool {
    path.components()
        .filter(|component| !matches!(component, Component::CurDir))
        .count()
        > 1
}

fn path_key(path: &Path) -> String {
    let normalized = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    display_path(&normalized)
}

fn is_managed_worktree(path: &Path, layout: &WorktreeLayout) -> bool {
    let managed_root = canonical_or_raw(&layout.worktree_root.join(&layout.repo_key));
    canonical_or_raw(path).starts_with(managed_root)
}

fn canonical_or_raw(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn is_empty_dir(path: &Path) -> std::io::Result<bool> {
    Ok(path.is_dir() && fs::read_dir(path)?.next().is_none())
}

fn run_git_worktree_prune() -> Result<(), CliError> {
    git_output(&["worktree", "prune"]).map_err(|err| {
        CliError::runtime(
            "git-worktree-prune-failed",
            summarize_git_error(&err.to_string()),
        )
    })?;
    Ok(())
}

fn emit_success<T: Serialize, F: FnOnce() -> String>(
    command: &str,
    format: OutputFormat,
    payload: &T,
    text: F,
) -> i32 {
    match format {
        OutputFormat::Json => {
            let envelope = Envelope::success(schema_version_for(BINARY, command, 1), payload);
            match serde_json::to_string(&envelope) {
                Ok(serialized) => {
                    println!("{serialized}");
                    exit::SUCCESS
                }
                Err(err) => {
                    eprintln!("failed to serialize JSON output: {err}");
                    exit::RUNTIME
                }
            }
        }
        OutputFormat::Text => {
            println!("{}", text());
            exit::SUCCESS
        }
    }
}

fn emit_error(command: &str, format: OutputFormat, err: CliError) -> i32 {
    if err.code == "help" {
        return exit::SUCCESS;
    }

    match format {
        OutputFormat::Json => {
            let mut envelope_error = EnvelopeError::new(err.code, err.message.as_ref());
            if let Some(hint) = &err.hint {
                envelope_error = envelope_error.with_hint(hint.as_ref());
            }
            if let Some(details) = err.details {
                envelope_error = envelope_error.with_details(*details);
            }
            let envelope: Envelope<()> =
                Envelope::failure(schema_version_for(BINARY, command, 1), envelope_error);
            match serde_json::to_string(&envelope) {
                Ok(serialized) => println!("{serialized}"),
                Err(serialize_err) => eprintln!("failed to serialize JSON error: {serialize_err}"),
            }
        }
        OutputFormat::Text => eprintln!("{}", err.message),
    }
    err.exit_code
}

fn render_list_text(output: &ListOutput) -> String {
    if output.entries.is_empty() {
        return "No linked git worktrees found.".to_string();
    }

    let mut lines = vec!["Worktrees:".to_string()];
    for entry in &output.entries {
        let branch = entry.branch.as_deref().unwrap_or("(detached)");
        let marker = if entry.managed { "managed" } else { "external" };
        lines.push(format!("  - {branch}: {} [{marker}]", entry.path));
    }
    lines.join("\n")
}

fn summarize_git_error(message: &str) -> String {
    let trimmed = message.trim();
    let summary = trimmed
        .rsplit_once(" failed: ")
        .map(|(_, suffix)| suffix.trim())
        .unwrap_or(trimmed);
    summary.replace('\n', " ")
}

fn display_path(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

#[cfg(test)]
mod tests {
    use super::{
        normalize_slug, parse_worktree_porcelain, repo_key_for_path, resolve_remove_target,
    };
    use pretty_assertions::{assert_eq, assert_ne};
    use std::path::Path;

    #[test]
    fn repo_key_includes_basename_and_stable_hash() {
        let first = repo_key_for_path(Path::new("/tmp/a/repo"));
        let second = repo_key_for_path(Path::new("/tmp/b/repo"));
        assert!(first.starts_with("repo-"));
        assert!(second.starts_with("repo-"));
        assert_ne!(first, second);
        assert_eq!(first, repo_key_for_path(Path::new("/tmp/a/repo")));
    }

    #[test]
    fn normalize_slug_keeps_branch_safe_path_segment() {
        assert_eq!(normalize_slug("Topic One").expect("slug"), "topic-one");
        assert_eq!(normalize_slug("fix/foo_bar").expect("slug"), "fix-foo_bar");
        assert!(normalize_slug("!!!").is_err());
    }

    #[test]
    fn parse_worktree_porcelain_captures_branch_and_detached_entries() {
        let entries = parse_worktree_porcelain(
            "worktree /repo\nHEAD 111\nbranch refs/heads/main\n\nworktree /repo/wt\nHEAD 222\ndetached\n",
        );
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].path, Path::new("/repo"));
        assert_eq!(entries[0].branch.as_deref(), Some("main"));
        assert_eq!(entries[1].path, Path::new("/repo/wt"));
        assert!(entries[1].detached);
    }

    #[test]
    fn resolve_remove_target_uses_managed_path_for_slug() {
        let layout = super::WorktreeLayout {
            agent_home: Path::new("/agent").to_path_buf(),
            worktree_root: Path::new("/agent/worktrees").to_path_buf(),
            repo_root: Path::new("/repo").to_path_buf(),
            repo_key: "repo-12345678".to_string(),
        };
        let target = resolve_remove_target("Topic One", &layout).expect("target");
        assert_eq!(
            target,
            Path::new("/agent/worktrees/repo-12345678/topic-one")
        );
    }
}
