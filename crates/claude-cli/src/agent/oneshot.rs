use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::process::{Command, Stdio};
use std::time::Duration;

use nils_common::cli_contract::exit;
use nils_common::env as shared_env;

use crate::process::{ProcessOutputError, output_with_limits_retry_io};
pub use crate::wrapper_config::RuntimeMode;
use crate::wrapper_config::{
    EFFORT_ENV, MODEL_ENV, NO_PERSISTENCE_ENV, RUNTIME_ENV, resolve_effort, resolve_model,
    resolve_no_persistence, resolve_runtime,
};

const CLAUDE_BIN_ENV: &str = "CLAUDE_CLI_BIN";
const MAX_HELP_BYTES: usize = 128 * 1024;
const MAX_INPUT_BYTES: usize = 1024 * 1024;
const HELP_TIMEOUT: Duration = Duration::from_secs(5);
const ADVICE_TEMPLATE: &str = "\
nils-claude-cli.agent-advice.v1
Give concise, actionable engineering advice. State assumptions, identify material risks, and recommend a concrete next step.";
const KNOWLEDGE_TEMPLATE: &str = "\
nils-claude-cli.agent-knowledge.v1
Explain the requested concept accurately and clearly. Prefer a compact example when it materially improves understanding.";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentTask {
    Prompt,
    Advice,
    Knowledge,
}

pub struct OneShotOptions {
    pub task: AgentTask,
    pub runtime: Option<RuntimeMode>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub ephemeral: bool,
    pub input: Vec<String>,
}

pub fn run(options: &OneShotOptions) -> i32 {
    let input = match collect_input(&options.input) {
        Ok(value) => value,
        Err(message) => {
            eprintln!("claude-cli agent: {message}");
            return exit::DATA;
        }
    };
    let runtime = match resolve_runtime(options.runtime) {
        Ok(value) => value,
        Err(message) => {
            eprintln!("claude-cli agent: {message}");
            return exit::USAGE;
        }
    };
    let effort = match resolve_effort(options.effort.as_deref()) {
        Ok(value) => value,
        Err(message) => {
            eprintln!("claude-cli agent: {message}");
            return exit::USAGE;
        }
    };
    let model = match resolve_model(options.model.as_deref()) {
        Ok(value) => value,
        Err(message) => {
            eprintln!("claude-cli agent: {message}");
            return exit::USAGE;
        }
    };
    let no_persistence = if runtime == RuntimeMode::Safe || options.ephemeral {
        true
    } else {
        match resolve_no_persistence() {
            Ok(value) => value,
            Err(message) => {
                eprintln!("claude-cli agent: {message}");
                return exit::USAGE;
            }
        }
    };

    let argv = build_argv(
        options.task,
        runtime,
        model.as_deref(),
        effort.as_deref(),
        no_persistence,
    );
    let binary = claude_binary();
    let required = required_capabilities(
        options.task,
        runtime,
        model.is_some(),
        effort.is_some(),
        no_persistence,
    );
    if let Err(missing) = probe_capabilities(&binary, &required) {
        eprintln!(
            "claude-cli agent: missing required Claude capabilities: {}",
            missing.join(", ")
        );
        return exit::UNAVAILABLE;
    }

    let mut command = Command::new(&binary);
    command
        .args(&argv)
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    clear_wrapper_environment(&mut command);
    match command.spawn() {
        Ok(mut child) => {
            let write_result = child
                .stdin
                .take()
                .ok_or_else(|| io::Error::other("Claude stdin pipe unavailable"))
                .and_then(|mut stdin| stdin.write_all(input.as_bytes()));
            if let Err(error) = write_result {
                eprintln!("claude-cli agent: failed to send input to Claude Code: {error}");
                let _ = child.kill();
                let _ = child.wait();
                return exit::RUNTIME;
            }
            match child.wait() {
                Ok(status) => status.code().unwrap_or(exit::RUNTIME),
                Err(error) => {
                    eprintln!("claude-cli agent: failed to wait for Claude Code: {error}");
                    exit::RUNTIME
                }
            }
        }
        Err(error) => {
            eprintln!("claude-cli agent: failed to launch Claude Code: {error}");
            exit::UNAVAILABLE
        }
    }
}

pub(crate) fn claude_binary() -> String {
    shared_env::env_non_empty(CLAUDE_BIN_ENV).unwrap_or_else(|| "claude".to_string())
}

fn collect_input(args: &[String]) -> Result<String, &'static str> {
    let argument_bytes = args
        .iter()
        .enumerate()
        .try_fold(0usize, |total, (index, value)| {
            total
                .checked_add(usize::from(index > 0))
                .and_then(|total| total.checked_add(value.len()))
        })
        .ok_or("input exceeds the 1 MiB safety limit")?;
    if argument_bytes > MAX_INPUT_BYTES {
        return Err("input exceeds the 1 MiB safety limit");
    }
    let joined = args.join(" ");
    if !joined.trim().is_empty() {
        return Ok(joined);
    }

    let mut raw = String::new();
    io::stdin()
        .take((MAX_INPUT_BYTES + 1) as u64)
        .read_to_string(&mut raw)
        .map_err(|_| "failed to read input from stdin")?;
    if raw.len() > MAX_INPUT_BYTES {
        return Err("input exceeds the 1 MiB safety limit");
    }
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("input must not be empty");
    }
    Ok(trimmed.to_string())
}

fn build_argv(
    task: AgentTask,
    runtime: RuntimeMode,
    model: Option<&str>,
    effort: Option<&str>,
    no_persistence: bool,
) -> Vec<String> {
    let mut argv = vec![
        "--print".to_string(),
        "--output-format".to_string(),
        "text".to_string(),
    ];
    if runtime == RuntimeMode::Safe {
        argv.push("--safe-mode".to_string());
        argv.push("--strict-mcp-config".to_string());
    }
    if no_persistence {
        argv.push("--no-session-persistence".to_string());
    }
    argv.extend([
        "--permission-mode".to_string(),
        "dontAsk".to_string(),
        "--disable-slash-commands".to_string(),
        "--no-chrome".to_string(),
        "--tools".to_string(),
        task.allowed_tools().to_string(),
    ]);
    if let Some(template) = task.template() {
        argv.push("--append-system-prompt".to_string());
        argv.push(template.to_string());
    }
    if let Some(model) = model {
        argv.push("--model".to_string());
        argv.push(model.to_string());
    }
    if let Some(effort) = effort {
        argv.push("--effort".to_string());
        argv.push(effort.to_string());
    }
    argv
}

impl AgentTask {
    fn allowed_tools(self) -> &'static str {
        match self {
            Self::Prompt | Self::Advice => "Read,Glob,Grep",
            Self::Knowledge => "",
        }
    }

    fn template(self) -> Option<&'static str> {
        match self {
            Self::Prompt => None,
            Self::Advice => Some(ADVICE_TEMPLATE),
            Self::Knowledge => Some(KNOWLEDGE_TEMPLATE),
        }
    }
}

fn required_capabilities(
    task: AgentTask,
    runtime: RuntimeMode,
    has_model: bool,
    has_effort: bool,
    no_persistence: bool,
) -> Vec<&'static str> {
    let mut required = vec![
        "--print",
        "--output-format",
        "--permission-mode",
        "--disable-slash-commands",
        "--no-chrome",
        "--tools",
    ];
    if runtime == RuntimeMode::Safe {
        required.extend(["--safe-mode", "--strict-mcp-config"]);
    }
    if no_persistence {
        required.push("--no-session-persistence");
    }
    if task.template().is_some() {
        required.push("--append-system-prompt");
    }
    if has_model {
        required.push("--model");
    }
    if has_effort {
        required.push("--effort");
    }
    required
}

pub(crate) fn probe_capabilities(binary: &str, required: &[&str]) -> Result<(), Vec<String>> {
    let report = capability_report(binary, required).map_err(|error| vec![error])?;
    let missing = report
        .into_iter()
        .filter_map(|(flag, available)| (!available).then_some(flag))
        .collect::<Vec<_>>();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(missing)
    }
}

pub(crate) fn capability_report(
    binary: &str,
    required: &[&str],
) -> Result<BTreeMap<String, bool>, String> {
    let mut command = Command::new(binary);
    command.arg("--help").stdin(Stdio::null());
    let output = match output_with_limits_retry_io(&mut command, HELP_TIMEOUT, MAX_HELP_BYTES, 3) {
        Ok(output) => output,
        Err(ProcessOutputError::Timeout) => {
            return Err("bounded `claude --help` timeout".to_string());
        }
        Err(ProcessOutputError::OutputLimit) => {
            return Err("bounded `claude --help` output".to_string());
        }
        Err(ProcessOutputError::Io(_)) => {
            return Err("claude executable".to_string());
        }
    };
    if !output.status.success() {
        return Err("bounded `claude --help` output".to_string());
    }
    let help = String::from_utf8_lossy(&output.stdout);
    Ok(required
        .iter()
        .map(|flag| ((*flag).to_string(), help_has_option(&help, flag)))
        .collect())
}

pub(crate) fn help_has_option(help: &str, required: &str) -> bool {
    help.lines().any(|line| {
        let trimmed = line.trim_start();
        if !trimmed.starts_with('-') {
            return false;
        }
        let option_spec = trimmed
            .split_once("  ")
            .map_or(trimmed, |(options, _)| options);
        option_spec.split_whitespace().any(|token| {
            token.trim_matches(|character: char| {
                matches!(character, ',' | '[' | ']' | '<' | '>' | '=')
            }) == required
        })
    })
}

pub(crate) fn clear_wrapper_environment(command: &mut Command) {
    for key in [
        CLAUDE_BIN_ENV,
        RUNTIME_ENV,
        MODEL_ENV,
        EFFORT_ENV,
        NO_PERSISTENCE_ENV,
    ] {
        command.env_remove(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn safe_prompt_argv_has_read_only_tools_and_no_persistence() {
        assert_eq!(
            build_argv(AgentTask::Prompt, RuntimeMode::Safe, None, None, true),
            vec![
                "--print",
                "--output-format",
                "text",
                "--safe-mode",
                "--strict-mcp-config",
                "--no-session-persistence",
                "--permission-mode",
                "dontAsk",
                "--disable-slash-commands",
                "--no-chrome",
                "--tools",
                "Read,Glob,Grep",
            ]
        );
    }

    #[test]
    fn capability_probe_matches_option_tokens_not_prose_or_superstrings() {
        let help = "\
  --printable             mentions --print in prose
  --output-format <kind>  Output format
";
        assert!(!help_has_option(help, "--print"));
        assert!(help_has_option(help, "--output-format"));
    }
}
