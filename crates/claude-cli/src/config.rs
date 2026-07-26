use nils_common::cli_contract::exit;
use nils_common::shell::{SingleQuoteEscapeStyle, quote_posix_single_with_style};

use crate::wrapper_config;

pub fn show() -> i32 {
    let model = match wrapper_config::resolve_model(None) {
        Ok(value) => value.unwrap_or_default(),
        Err(message) => return invalid_config(message),
    };
    let effort = match wrapper_config::resolve_effort(None) {
        Ok(value) => value.unwrap_or_default(),
        Err(message) => return invalid_config(message),
    };
    let runtime = match wrapper_config::resolve_runtime(None) {
        Ok(value) => value,
        Err(message) => return invalid_config(message),
    };
    let no_persistence = match wrapper_config::resolve_no_persistence() {
        Ok(value) => value,
        Err(message) => return invalid_config(message),
    };

    println!("CLAUDE_CLI_MODEL={model}");
    println!("CLAUDE_CLI_EFFORT={effort}");
    println!("CLAUDE_CLI_AGENT_RUNTIME={}", runtime.as_str());
    println!("CLAUDE_CLI_NO_SESSION_PERSISTENCE={no_persistence}");
    exit::SUCCESS
}

pub fn set(key: &str, value: &str) -> i32 {
    match key {
        "model" | "CLAUDE_CLI_MODEL" => {
            let value = match wrapper_config::validate_model(value) {
                Ok(value) => value,
                Err(message) => return invalid_config(message),
            };
            emit_quoted("CLAUDE_CLI_MODEL", &value)
        }
        "effort" | "CLAUDE_CLI_EFFORT" => {
            let lowered = match wrapper_config::validate_effort(value) {
                Ok(value) => value,
                Err(message) => return invalid_config(message),
            };
            println!("export CLAUDE_CLI_EFFORT={lowered}");
            exit::SUCCESS
        }
        "agent-runtime" | "agent_runtime" | "runtime" | "CLAUDE_CLI_AGENT_RUNTIME" => {
            let runtime = match wrapper_config::parse_runtime(value) {
                Ok(value) => value,
                Err(message) => return invalid_config(message),
            };
            println!("export CLAUDE_CLI_AGENT_RUNTIME={}", runtime.as_str());
            exit::SUCCESS
        }
        "no-session-persistence"
        | "no_session_persistence"
        | "CLAUDE_CLI_NO_SESSION_PERSISTENCE" => {
            let value = match wrapper_config::parse_bool(value) {
                Ok(value) => value,
                Err(message) => return invalid_config(message),
            };
            println!("export CLAUDE_CLI_NO_SESSION_PERSISTENCE={value}");
            exit::SUCCESS
        }
        _ => {
            eprintln!("claude-cli config: unknown key: {key}");
            eprintln!("claude-cli config: keys: model|effort|agent-runtime|no-session-persistence");
            exit::USAGE
        }
    }
}

fn invalid_config(message: impl AsRef<str>) -> i32 {
    eprintln!("claude-cli config: {}", message.as_ref());
    exit::USAGE
}

fn emit_quoted(key: &str, value: &str) -> i32 {
    println!(
        "export {key}={}",
        quote_posix_single_with_style(value, SingleQuoteEscapeStyle::DoubleQuoteBoundary)
    );
    exit::SUCCESS
}
