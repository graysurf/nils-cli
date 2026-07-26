use std::io::{Seek, SeekFrom, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use nils_common::cli_contract::exit;
use nils_common::{git as shared_git, process as shared_process};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::agent::oneshot::{claude_binary, clear_wrapper_environment, probe_capabilities};
use crate::process::{
    ProcessOutputError, ProcessStatusError, output_with_limits, output_with_limits_retry_io,
    status_with_deadline,
};
use crate::wrapper_config::{resolve_effort, resolve_model};

const MAX_CONTEXT_BYTES: usize = 2 * 1024 * 1024;
const MAX_EXTRA_BYTES: usize = 64 * 1024;
const MAX_MODEL_OUTPUT_BYTES: usize = 1024 * 1024;
const GIT_TIMEOUT: Duration = Duration::from_secs(10);
const CONTEXT_TIMEOUT: Duration = Duration::from_secs(30);
const MODEL_TIMEOUT: Duration = Duration::from_secs(300);
const AUTO_STAGE_TIMEOUT: Duration = Duration::from_secs(60);
const COMMIT_TIMEOUT: Duration = Duration::from_secs(120);
const PUSH_TIMEOUT: Duration = Duration::from_secs(120);
pub(crate) const COMMIT_REQUIRED_CAPABILITIES: [&str; 11] = [
    "--print",
    "--output-format",
    "--json-schema",
    "--safe-mode",
    "--strict-mcp-config",
    "--no-session-persistence",
    "--permission-mode",
    "--disable-slash-commands",
    "--no-chrome",
    "--tools",
    "--append-system-prompt",
];
pub(crate) const COMMIT_OPTIONAL_CAPABILITIES: [&str; 2] = ["--model", "--effort"];
pub(crate) const STAGED_CONTEXT_HELP_OPTIONS: [&str; 2] = ["--format", "--repo"];
pub(crate) const SEMANTIC_COMMIT_HELP_OPTIONS: [&str; 8] = [
    "--type",
    "--scope",
    "--subject",
    "--body-bullet",
    "--expect-head",
    "--repo",
    "--summary",
    "--automation",
];
const COMMIT_SYSTEM_PROMPT: &str = "\
nils-claude-cli.agent-commit.v1
Generate only the semantic commit message fields required by the JSON schema.
Use only the staged-context bundle supplied by the user.
Choose exactly one Conventional Commit type allowed by the schema.
Do not request, describe, or execute commands.
Do not infer unstaged or untracked repository content.";

pub struct CommitOptions {
    pub push: bool,
    pub auto_stage: bool,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub extra: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
struct GeneratedCommitMessage {
    #[serde(rename = "type")]
    commit_type: String,
    scope: Option<String>,
    subject: String,
    #[serde(default)]
    body_bullets: Vec<String>,
}

pub fn run(options: &CommitOptions) -> i32 {
    let binary = claude_binary();
    if shared_process::find_in_path(&binary).is_none() {
        eprintln!("claude-cli agent commit: missing dependency: claude");
        return exit::UNAVAILABLE;
    }
    if shared_process::find_in_path("git").is_none() {
        eprintln!("claude-cli agent commit: missing dependency: git");
        return exit::UNAVAILABLE;
    }
    if shared_process::find_in_path("semantic-commit").is_none() {
        eprintln!("claude-cli agent commit: missing dependency: semantic-commit");
        return exit::UNAVAILABLE;
    }

    let model = match resolve_model(options.model.as_deref()) {
        Ok(value) => value,
        Err(message) => {
            eprintln!("claude-cli agent commit: {message}");
            return exit::USAGE;
        }
    };
    let effort = match resolve_effort(options.effort.as_deref()) {
        Ok(value) => value,
        Err(message) => {
            eprintln!("claude-cli agent commit: {message}");
            return exit::USAGE;
        }
    };
    let extra = match collect_extra(&options.extra) {
        Ok(value) => value,
        Err(message) => {
            eprintln!("claude-cli agent commit: {message}");
            return exit::DATA;
        }
    };
    let required_capabilities = required_capabilities(model.is_some(), effort.is_some());
    if let Err(missing) = probe_capabilities(&binary, &required_capabilities) {
        eprintln!(
            "claude-cli agent commit: missing required Claude capabilities: {}",
            missing.join(", ")
        );
        return exit::UNAVAILABLE;
    }

    let git_root = match shared_git::repo_root()
        .ok()
        .flatten()
        .and_then(|path| std::fs::canonicalize(path).ok())
    {
        Some(path) => path,
        None => {
            eprintln!("claude-cli agent commit: not a git repository");
            return exit::RUNTIME;
        }
    };
    if options.auto_stage {
        match git_status(&git_root, &["add", "-A"], AUTO_STAGE_TIMEOUT) {
            Ok(status) if status.success() => {}
            Ok(_) => {
                eprintln!("claude-cli agent commit: auto-stage failed; no commit created");
                return exit::RUNTIME;
            }
            Err(ProcessStatusError::Timeout) => {
                eprintln!("claude-cli agent commit: auto-stage timed out; no commit created");
                return exit::RUNTIME;
            }
            Err(ProcessStatusError::Launch) => {
                eprintln!(
                    "claude-cli agent commit: auto-stage failed to launch; no commit created"
                );
                return exit::UNAVAILABLE;
            }
            Err(ProcessStatusError::Wait | ProcessStatusError::Failed) => {
                eprintln!("claude-cli agent commit: auto-stage process failed; no commit created");
                return exit::RUNTIME;
            }
        }
    }
    match has_staged_changes(&git_root) {
        Ok(true) => {}
        Ok(false) => {
            eprintln!("claude-cli agent commit: no staged changes (stage files then retry)");
            return exit::RUNTIME;
        }
        Err(ProcessStatusError::Timeout) => {
            eprintln!("claude-cli agent commit: staged-change check timed out");
            return exit::RUNTIME;
        }
        Err(ProcessStatusError::Launch) => {
            eprintln!("claude-cli agent commit: staged-change check failed to launch");
            return exit::UNAVAILABLE;
        }
        Err(ProcessStatusError::Wait | ProcessStatusError::Failed) => {
            eprintln!("claude-cli agent commit: staged-change check failed");
            return exit::RUNTIME;
        }
    }

    let old_head = match git_stdout(&git_root, &["rev-parse", "--verify", "HEAD^{commit}"]) {
        Ok(value) => value,
        Err(message) => {
            eprintln!("claude-cli agent commit: {message}");
            return exit::RUNTIME;
        }
    };
    let staged_tree = match git_stdout(&git_root, &["write-tree"]) {
        Ok(value) => value,
        Err(message) => {
            eprintln!("claude-cli agent commit: {message}");
            return exit::RUNTIME;
        }
    };
    let push_target = if options.push {
        match resolve_push_target(&git_root) {
            Ok(target) => Some(target),
            Err(message) => {
                eprintln!("claude-cli agent commit: {message}; no commit created");
                return exit::RUNTIME;
            }
        }
    } else {
        None
    };
    let context = match staged_context(&git_root) {
        Ok(value) => value,
        Err(message) => {
            eprintln!("claude-cli agent commit: {message}; index remains staged");
            return exit::RUNTIME;
        }
    };
    let secret_scan = nils_scrub::scrub_text(&context);
    if !secret_scan.matches.is_empty() {
        eprintln!(
            "claude-cli agent commit: staged context contains secret-like content (patterns: {}); Claude was not launched and the index remains staged",
            secret_scan.triggered_patterns().join(", ")
        );
        return exit::DATA;
    }
    let prompt = format!(
        "User wording guidance: {}\n\n{}",
        extra.as_deref().unwrap_or("(none)"),
        context
    );
    let message = match generate_message(&binary, &prompt, model.as_deref(), effort.as_deref()) {
        Ok(message) => message,
        Err(GenerateError::Unavailable(message)) => {
            eprintln!("claude-cli agent commit: {message}; index remains staged");
            return exit::UNAVAILABLE;
        }
        Err(GenerateError::Invalid(message)) => {
            eprintln!("claude-cli agent commit: {message}; index remains staged");
            return exit::DATA;
        }
        Err(GenerateError::Runtime(message)) => {
            eprintln!("claude-cli agent commit: {message}; index remains staged");
            return exit::RUNTIME;
        }
    };

    let current_head = git_stdout(&git_root, &["rev-parse", "--verify", "HEAD^{commit}"]);
    let current_tree = git_stdout(&git_root, &["write-tree"]);
    if current_head.as_deref() != Ok(old_head.as_str())
        || current_tree.as_deref() != Ok(staged_tree.as_str())
    {
        eprintln!(
            "claude-cli agent commit: repository changed during message generation; no commit created"
        );
        return exit::RUNTIME;
    }

    let status = match run_semantic_commit(&git_root, &old_head, &message) {
        Ok(status) => status,
        Err(ProcessStatusError::Timeout) => {
            eprintln!(
                "claude-cli agent commit: {}",
                mutation_failure_diagnostic(
                    &git_root,
                    &old_head,
                    &staged_tree,
                    "semantic-commit timed out"
                )
            );
            return exit::RUNTIME;
        }
        Err(ProcessStatusError::Launch) => {
            eprintln!(
                "claude-cli agent commit: failed to launch semantic-commit; index remains staged"
            );
            return exit::UNAVAILABLE;
        }
        Err(ProcessStatusError::Wait | ProcessStatusError::Failed) => {
            eprintln!(
                "claude-cli agent commit: {}",
                mutation_failure_diagnostic(
                    &git_root,
                    &old_head,
                    &staged_tree,
                    "semantic-commit process failed"
                )
            );
            return exit::RUNTIME;
        }
    };
    if !status.success() {
        eprintln!(
            "claude-cli agent commit: {}",
            mutation_failure_diagnostic(
                &git_root,
                &old_head,
                &staged_tree,
                "semantic-commit failed"
            )
        );
        return status.code().unwrap_or(exit::RUNTIME);
    }
    let new_head = match git_stdout(&git_root, &["rev-parse", "--verify", "HEAD^{commit}"]) {
        Ok(value) => value,
        Err(_) => {
            eprintln!(
                "claude-cli agent commit: commit completed but HEAD could not be verified; push skipped"
            );
            return exit::RUNTIME;
        }
    };
    match commit_matches_snapshot(&git_root, &new_head, &old_head, &staged_tree) {
        Ok(true) => {}
        Ok(false) => {
            eprintln!(
                "claude-cli agent commit: commit {new_head} was created but failed parent/tree integrity verification; local commit was preserved and push was skipped"
            );
            return exit::RUNTIME;
        }
        Err(()) => {
            eprintln!(
                "claude-cli agent commit: commit {new_head} was created but parent/tree integrity could not be read; local commit was preserved and push was skipped"
            );
            return exit::RUNTIME;
        }
    }
    eprintln!("claude-cli agent commit: committed {new_head}");

    if let Some(push_target) = push_target {
        match push_source_is_unchanged(&git_root, &push_target, &new_head) {
            Ok(true) => {}
            Ok(false) => {
                eprintln!(
                    "claude-cli agent commit: commit {new_head} succeeded, but HEAD, branch, or push endpoint changed before push; local commit was preserved"
                );
                return exit::RUNTIME;
            }
            Err(()) => {
                eprintln!(
                    "claude-cli agent commit: commit {new_head} succeeded, but HEAD or branch could not be read before push; local commit was preserved"
                );
                return exit::RUNTIME;
            }
        }
        let refspec = format!("{new_head}:{}", push_target.merge_ref);
        let mut command = Command::new("git");
        command
            .args(["-C"])
            .arg(&git_root)
            .args(["push", "--"])
            .arg(&push_target.push_url)
            .arg(refspec)
            .stdin(Stdio::null());
        pin_push_endpoint(&mut command, &push_target.push_url);
        let status = status_with_deadline(&mut command, PUSH_TIMEOUT);
        match status {
            Ok(status) if status.success() => {}
            Ok(status) => {
                eprintln!(
                    "claude-cli agent commit: commit {new_head} succeeded, but push failed; local commit was preserved"
                );
                return status.code().unwrap_or(exit::RUNTIME);
            }
            Err(ProcessStatusError::Timeout) => {
                eprintln!(
                    "claude-cli agent commit: commit {new_head} succeeded, but push timed out; local commit was preserved"
                );
                return exit::RUNTIME;
            }
            Err(ProcessStatusError::Launch) => {
                eprintln!(
                    "claude-cli agent commit: commit {new_head} succeeded, but push could not launch; local commit was preserved"
                );
                return exit::UNAVAILABLE;
            }
            Err(ProcessStatusError::Wait | ProcessStatusError::Failed) => {
                eprintln!(
                    "claude-cli agent commit: commit {new_head} succeeded, but push process failed; local commit was preserved"
                );
                return exit::RUNTIME;
            }
        }
    }
    exit::SUCCESS
}

fn collect_extra(extra: &[String]) -> Result<Option<String>, &'static str> {
    let bytes = extra
        .iter()
        .enumerate()
        .try_fold(0usize, |total, (index, value)| {
            total
                .checked_add(usize::from(index > 0))
                .and_then(|total| total.checked_add(value.len()))
        })
        .ok_or("additional guidance exceeds the 64 KiB safety limit")?;
    if bytes > MAX_EXTRA_BYTES {
        return Err("additional guidance exceeds the 64 KiB safety limit");
    }
    let joined = extra.join(" ");
    let trimmed = joined.trim();
    Ok((!trimmed.is_empty()).then(|| trimmed.to_string()))
}

pub(crate) fn required_capabilities(has_model: bool, has_effort: bool) -> Vec<&'static str> {
    let mut required = COMMIT_REQUIRED_CAPABILITIES.to_vec();
    if has_model {
        required.push("--model");
    }
    if has_effort {
        required.push("--effort");
    }
    required
}

pub(crate) fn doctor_capabilities() -> Vec<&'static str> {
    COMMIT_REQUIRED_CAPABILITIES
        .iter()
        .chain(COMMIT_OPTIONAL_CAPABILITIES.iter())
        .copied()
        .collect()
}

fn staged_context(git_root: &Path) -> Result<String, &'static str> {
    let mut command = Command::new("semantic-commit");
    command
        .args(["staged-context", "--format", "bundle", "--repo"])
        .arg(git_root)
        .stdin(Stdio::null());
    let output =
        match output_with_limits_retry_io(&mut command, CONTEXT_TIMEOUT, MAX_CONTEXT_BYTES, 3) {
            Ok(output) => output,
            Err(ProcessOutputError::Timeout) => return Err("staged-context timed out"),
            Err(ProcessOutputError::OutputLimit) => return Err("staged-context exceeds 2 MiB"),
            Err(ProcessOutputError::Io(_)) => return Err("staged-context failed to launch"),
        };
    if !output.status.success() {
        return Err("staged-context failed");
    }
    String::from_utf8(output.stdout).map_err(|_| "staged-context was not UTF-8")
}

fn generate_message(
    binary: &str,
    prompt: &str,
    model: Option<&str>,
    effort: Option<&str>,
) -> Result<GeneratedCommitMessage, GenerateError> {
    let schema = commit_schema().to_string();
    let mut prompt_file = tempfile::tempfile()
        .map_err(|_| GenerateError::Runtime("failed to create bounded prompt input".to_string()))?;
    prompt_file
        .write_all(prompt.as_bytes())
        .and_then(|()| prompt_file.flush())
        .map_err(|_| {
            GenerateError::Runtime("failed to prepare bounded prompt input".to_string())
        })?;
    prompt_file
        .seek(SeekFrom::Start(0))
        .map_err(|_| GenerateError::Runtime("failed to rewind bounded prompt input".to_string()))?;
    let input = prompt_file
        .try_clone()
        .map_err(|_| GenerateError::Runtime("failed to reopen bounded prompt input".to_string()))?;
    let child_workdir = tempfile::tempdir().map_err(|_| {
        GenerateError::Runtime("failed to create isolated working directory".to_string())
    })?;

    let mut command = Command::new(binary);
    command
        .args(["--print", "--output-format", "json", "--json-schema"])
        .arg(schema)
        .args([
            "--safe-mode",
            "--strict-mcp-config",
            "--no-session-persistence",
            "--permission-mode",
            "dontAsk",
            "--disable-slash-commands",
            "--no-chrome",
            "--tools",
            "",
            "--append-system-prompt",
            COMMIT_SYSTEM_PROMPT,
        ])
        .current_dir(child_workdir.path())
        .stdin(Stdio::from(input));
    if let Some(model) = model {
        command.args(["--model", model]);
    }
    if let Some(effort) = effort {
        command.args(["--effort", effort]);
    }
    clear_wrapper_environment(&mut command);
    clear_git_environment(&mut command);

    let output = match output_with_limits(&mut command, MODEL_TIMEOUT, MAX_MODEL_OUTPUT_BYTES) {
        Ok(output) => output,
        Err(ProcessOutputError::Timeout) => {
            return Err(GenerateError::Runtime(
                "model message generation timed out".to_string(),
            ));
        }
        Err(ProcessOutputError::OutputLimit) => {
            return Err(GenerateError::Invalid(
                "model output exceeds 1 MiB".to_string(),
            ));
        }
        Err(ProcessOutputError::Io(_)) => {
            return Err(GenerateError::Unavailable(
                "failed to launch Claude Code".to_string(),
            ));
        }
    };
    if !output.status.success() {
        return Err(GenerateError::Runtime(format!(
            "model message generation failed (exit code: {})",
            output.status.code().unwrap_or(exit::RUNTIME)
        )));
    }
    parse_generated_message(&output.stdout)
}

fn clear_git_environment(command: &mut Command) {
    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    ] {
        command.env_remove(key);
    }
}

fn commit_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["type", "scope", "subject", "body_bullets"],
        "properties": {
            "type": {
                "type": "string",
                "enum": [
                    "build", "chore", "ci", "docs", "feat", "fix", "perf",
                    "refactor", "revert", "style", "test"
                ]
            },
            "scope": {
                "anyOf": [
                    {"type": "null"},
                    {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": 64,
                        "pattern": "^[^\\s()]+$"
                    }
                ]
            },
            "subject": {
                "type": "string",
                "minLength": 1,
                "maxLength": 100,
                "pattern": "^[^\\r\\n]+$"
            },
            "body_bullets": {
                "type": "array",
                "maxItems": 20,
                "items": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": 500,
                    "pattern": "^[^\\r\\n]+$"
                }
            }
        }
    })
}

fn parse_generated_message(bytes: &[u8]) -> Result<GeneratedCommitMessage, GenerateError> {
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|_| GenerateError::Invalid("invalid structured model output".to_string()))?;
    let result = match &value {
        Value::Array(items) => items
            .iter()
            .rev()
            .find(|item| item.get("type").and_then(Value::as_str) == Some("result")),
        Value::Object(_) if value.get("type").and_then(Value::as_str) == Some("result") => {
            Some(&value)
        }
        _ => None,
    }
    .ok_or_else(|| GenerateError::Invalid("missing structured result envelope".to_string()))?;
    if result.get("subtype").and_then(Value::as_str) != Some("success")
        || result.get("is_error").and_then(Value::as_bool) != Some(false)
    {
        return Err(GenerateError::Invalid(
            "unsuccessful structured result envelope".to_string(),
        ));
    }
    let structured = result
        .get("structured_output")
        .ok_or_else(|| GenerateError::Invalid("missing structured output payload".to_string()))?;
    let message: GeneratedCommitMessage = serde_json::from_value(structured.clone())
        .map_err(|_| GenerateError::Invalid("invalid structured commit message".to_string()))?;
    validate_message(&message).map_err(GenerateError::Invalid)?;
    Ok(message)
}

fn validate_message(message: &GeneratedCommitMessage) -> Result<(), String> {
    const TYPES: [&str; 11] = [
        "build", "chore", "ci", "docs", "feat", "fix", "perf", "refactor", "revert", "style",
        "test",
    ];
    if !TYPES.contains(&message.commit_type.as_str()) {
        return Err("invalid commit type".to_string());
    }
    if let Some(scope) = &message.scope
        && (scope.trim().is_empty()
            || scope.len() > 64
            || scope.chars().any(|character| {
                character.is_whitespace()
                    || matches!(character, '(' | ')')
                    || is_disallowed_message_character(character)
            }))
    {
        return Err("invalid commit scope".to_string());
    }
    let subject = message.subject.trim();
    if subject.is_empty()
        || subject != message.subject
        || subject.len() > 100
        || subject.contains(['\r', '\n'])
        || subject.chars().any(is_disallowed_message_character)
    {
        return Err("invalid commit subject".to_string());
    }
    let header_len = message.commit_type.len()
        + message.scope.as_ref().map_or(0, |scope| scope.len() + 2)
        + 2
        + subject.len();
    if header_len > 100 {
        return Err("commit header exceeds 100 characters".to_string());
    }
    if message.body_bullets.len() > 20
        || message.body_bullets.iter().any(|bullet| {
            bullet.trim().is_empty()
                || bullet != bullet.trim()
                || bullet.len() > 500
                || bullet.contains(['\r', '\n'])
                || bullet.chars().any(is_disallowed_message_character)
        })
    {
        return Err("invalid body bullet".to_string());
    }
    Ok(())
}

fn run_semantic_commit(
    git_root: &Path,
    old_head: &str,
    message: &GeneratedCommitMessage,
) -> Result<std::process::ExitStatus, ProcessStatusError> {
    let mut command = Command::new("semantic-commit");
    command
        .arg("commit")
        .args(["--type", message.commit_type.as_str()]);
    if let Some(scope) = &message.scope {
        command.args(["--scope", scope]);
    }
    command.args(["--subject", message.subject.as_str()]);
    for bullet in &message.body_bullets {
        command.args(["--body-bullet", bullet]);
    }
    command
        .args(["--expect-head", old_head, "--repo"])
        .arg(git_root)
        .args(["--summary", "none", "--automation"])
        .stdin(Stdio::null());
    status_with_deadline(&mut command, COMMIT_TIMEOUT)
}

fn git_stdout(git_root: &Path, args: &[&str]) -> Result<String, String> {
    let mut last_error = None;
    for attempt in 0..3 {
        match git_stdout_once(git_root, args) {
            Ok(value) => return Ok(value),
            Err(message) => last_error = Some(message),
        }
        if attempt < 2 {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    Err(last_error.unwrap_or_else(|| format!("git {} failed", args.join(" "))))
}

fn git_stdout_once(git_root: &Path, args: &[&str]) -> Result<String, String> {
    let mut command = Command::new("git");
    command
        .args(["-C"])
        .arg(git_root)
        .args(args)
        .stdin(Stdio::null());
    let output = output_with_limits(&mut command, GIT_TIMEOUT, 64 * 1024)
        .map_err(|_| format!("git {} failed", args.join(" ")))?;
    if !output.status.success() {
        return Err(format!("git {} failed", args.join(" ")));
    }
    String::from_utf8(output.stdout)
        .map(|value| value.trim().to_string())
        .map_err(|_| format!("git {} returned non-UTF-8 output", args.join(" ")))
}

fn git_status(
    git_root: &Path,
    args: &[&str],
    timeout: Duration,
) -> Result<std::process::ExitStatus, ProcessStatusError> {
    let mut command = Command::new("git");
    command
        .args(["-C"])
        .arg(git_root)
        .args(args)
        .stdin(Stdio::null());
    status_with_deadline(&mut command, timeout)
}

fn has_staged_changes(git_root: &Path) -> Result<bool, ProcessStatusError> {
    let status = git_status(
        git_root,
        &["diff", "--cached", "--quiet", "--exit-code"],
        GIT_TIMEOUT,
    )?;
    match status.code() {
        Some(0) => Ok(false),
        Some(1) => Ok(true),
        _ => Err(ProcessStatusError::Failed),
    }
}

fn mutation_failure_diagnostic(
    git_root: &Path,
    old_head: &str,
    staged_tree: &str,
    action: &str,
) -> String {
    let current_head = git_stdout(git_root, &["rev-parse", "--verify", "HEAD^{commit}"]);
    let current_tree = git_stdout(git_root, &["write-tree"]);
    mutation_failure_diagnostic_from_observation(
        action,
        old_head,
        staged_tree,
        current_head.as_deref().ok(),
        current_tree.as_deref().ok(),
    )
}

fn mutation_failure_diagnostic_from_observation(
    action: &str,
    old_head: &str,
    staged_tree: &str,
    current_head: Option<&str>,
    current_tree: Option<&str>,
) -> String {
    match (current_head, current_tree) {
        (Some(head), Some(tree)) if head == old_head && tree == staged_tree => {
            format!("{action}; no commit was observed and the index remains staged")
        }
        (Some(head), Some(_)) if head != old_head => format!(
            "{action} after repository state changed; local HEAD {head} was preserved, push was skipped, and the repository must be inspected"
        ),
        (Some(_), Some(_)) => format!(
            "{action} after the index changed; push was skipped and the repository must be inspected"
        ),
        _ => format!(
            "{action}; repository state could not be verified, push was skipped, and the repository must be inspected"
        ),
    }
}

fn is_disallowed_message_character(character: char) -> bool {
    character.is_control()
        || matches!(
            character,
            '\u{061c}'
                | '\u{200e}'
                | '\u{200f}'
                | '\u{202a}'..='\u{202e}'
                | '\u{2066}'..='\u{2069}'
        )
}

struct PushTarget {
    branch: String,
    remote: String,
    push_url: String,
    merge_ref: String,
}

fn resolve_push_target(git_root: &Path) -> Result<PushTarget, String> {
    let branch = git_stdout(git_root, &["symbolic-ref", "--quiet", "--short", "HEAD"])
        .map_err(|_| "push requires an attached branch".to_string())?;
    if branch.is_empty() || branch.chars().any(char::is_control) {
        return Err("push target is not a safe branch ref".to_string());
    }
    let remote_key = format!("branch.{branch}.remote");
    let merge_key = format!("branch.{branch}.merge");
    let remote = git_stdout(git_root, &["config", "--get", &remote_key])
        .map_err(|_| "push requires a configured upstream remote".to_string())?;
    if remote.is_empty() || remote.starts_with('-') || remote.chars().any(char::is_control) {
        return Err("push target is not a safe remote".to_string());
    }
    let merge_ref = git_stdout(git_root, &["config", "--get", &merge_key])
        .map_err(|_| "push requires a configured upstream branch".to_string())?;
    let push_url = git_stdout(git_root, &["remote", "get-url", "--push", &remote])
        .map_err(|_| "push requires a resolvable upstream endpoint".to_string())?;
    if !merge_ref.starts_with("refs/heads/")
        || [merge_ref.as_str(), push_url.as_str()]
            .iter()
            .any(|value| value.is_empty() || value.chars().any(char::is_control))
    {
        return Err("push target is not a safe branch ref".to_string());
    }
    Ok(PushTarget {
        branch,
        remote,
        push_url,
        merge_ref,
    })
}

fn commit_matches_snapshot(
    git_root: &Path,
    new_head: &str,
    old_head: &str,
    staged_tree: &str,
) -> Result<bool, ()> {
    let parent = git_stdout(git_root, &["show", "-s", "--format=%P", new_head]).map_err(|_| ())?;
    let treeish = format!("{new_head}^{{tree}}");
    let tree = git_stdout(git_root, &["rev-parse", "--verify", &treeish]).map_err(|_| ())?;
    Ok(parent == old_head && tree == staged_tree)
}

fn push_source_is_unchanged(
    git_root: &Path,
    target: &PushTarget,
    new_head: &str,
) -> Result<bool, ()> {
    let branch =
        git_stdout(git_root, &["symbolic-ref", "--quiet", "--short", "HEAD"]).map_err(|_| ())?;
    let head = git_stdout(git_root, &["rev-parse", "--verify", "HEAD^{commit}"]).map_err(|_| ())?;
    let push_url =
        git_stdout(git_root, &["remote", "get-url", "--push", &target.remote]).map_err(|_| ())?;
    Ok(branch == target.branch && head == new_head && push_url == target.push_url)
}

fn pin_push_endpoint(command: &mut Command, push_url: &str) {
    command
        .env_remove("GIT_CONFIG_PARAMETERS")
        .env("GIT_CONFIG_COUNT", "2")
        .env("GIT_CONFIG_KEY_0", format!("url.{push_url}.insteadOf"))
        .env("GIT_CONFIG_VALUE_0", push_url)
        .env("GIT_CONFIG_KEY_1", format!("url.{push_url}.pushInsteadOf"))
        .env("GIT_CONFIG_VALUE_1", push_url);
}

#[derive(Debug)]
enum GenerateError {
    Unavailable(String),
    Invalid(String),
    Runtime(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn validates_structured_commit_message_constraints() {
        let valid = GeneratedCommitMessage {
            commit_type: "feat".to_string(),
            scope: Some("agent".to_string()),
            subject: "add safe commit workflow".to_string(),
            body_bullets: vec!["Keep commit creation deterministic".to_string()],
        };
        assert_eq!(validate_message(&valid), Ok(()));

        let invalid = GeneratedCommitMessage {
            commit_type: "feature".to_string(),
            ..valid
        };
        assert_eq!(
            validate_message(&invalid),
            Err("invalid commit type".to_string())
        );

        let terminal_escape = GeneratedCommitMessage {
            commit_type: "fix".to_string(),
            scope: None,
            subject: "spoof \u{1b}]52;clipboard\u{7}".to_string(),
            body_bullets: Vec::new(),
        };
        assert_eq!(
            validate_message(&terminal_escape),
            Err("invalid commit subject".to_string())
        );

        let bidi_override = GeneratedCommitMessage {
            commit_type: "fix".to_string(),
            scope: None,
            subject: "spoof \u{202e}txt".to_string(),
            body_bullets: Vec::new(),
        };
        assert_eq!(
            validate_message(&bidi_override),
            Err("invalid commit subject".to_string())
        );
    }

    #[test]
    fn accepts_object_and_event_array_result_envelopes() {
        let object = br#"{"type":"result","subtype":"success","is_error":false,"structured_output":{"type":"fix","scope":null,"subject":"fix a thing","body_bullets":[]}}"#;
        let array = format!(
            "[{{\"type\":\"system\"}},{}]",
            String::from_utf8_lossy(object)
        );

        assert_eq!(
            parse_generated_message(object).expect("object").subject,
            "fix a thing"
        );
        assert_eq!(
            parse_generated_message(array.as_bytes())
                .expect("array")
                .subject,
            "fix a thing"
        );
    }

    #[test]
    fn mutation_failure_diagnostics_never_claim_a_staged_index_after_head_changes() {
        let diagnostic = mutation_failure_diagnostic_from_observation(
            "semantic-commit timed out",
            "old-head",
            "staged-tree",
            Some("new-head"),
            Some("staged-tree"),
        );

        assert!(diagnostic.contains("local HEAD new-head was preserved"));
        assert!(diagnostic.contains("push was skipped"));
        assert!(!diagnostic.contains("index remains staged"));
    }
}
