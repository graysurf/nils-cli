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

#[cfg(test)]
mod tests {
    use super::*;

    use pretty_assertions::assert_eq;

    #[test]
    fn every_key_accepts_both_its_short_name_and_its_env_var_name() {
        for key in ["model", "CLAUDE_CLI_MODEL"] {
            assert_eq!(set(key, "claude-opus-5"), exit::SUCCESS, "{key}");
        }
        for key in ["effort", "CLAUDE_CLI_EFFORT"] {
            assert_eq!(set(key, "HIGH"), exit::SUCCESS, "{key}");
        }
        for key in [
            "agent-runtime",
            "agent_runtime",
            "runtime",
            "CLAUDE_CLI_AGENT_RUNTIME",
        ] {
            assert_eq!(set(key, "safe"), exit::SUCCESS, "{key}");
        }
        for key in [
            "no-session-persistence",
            "no_session_persistence",
            "CLAUDE_CLI_NO_SESSION_PERSISTENCE",
        ] {
            assert_eq!(set(key, "true"), exit::SUCCESS, "{key}");
        }
    }

    #[test]
    fn an_unknown_key_is_a_usage_error() {
        assert_eq!(set("nope", "value"), exit::USAGE);
        assert_eq!(set("", "value"), exit::USAGE);
    }

    #[test]
    fn each_key_rejects_a_value_its_validator_does_not_accept() {
        assert_eq!(set("model", "  "), exit::USAGE, "empty model");
        assert_eq!(set("model", "bad\u{7}model"), exit::USAGE, "control char");
        assert_eq!(set("effort", "turbo"), exit::USAGE);
        assert_eq!(set("runtime", "sandboxed"), exit::USAGE);
        assert_eq!(set("no-session-persistence", "maybe"), exit::USAGE);
    }

    #[test]
    fn accepted_values_are_normalized_before_export() {
        // These are the exact shell-eval'able lines the wrapper sources, so
        // casing and aliasing must collapse to the canonical spelling.
        assert_eq!(
            super::wrapper_config::validate_effort("XHigh").unwrap(),
            "xhigh"
        );
        assert_eq!(
            super::wrapper_config::parse_runtime(" Inherited ")
                .unwrap()
                .as_str(),
            "inherited"
        );
        assert!(super::wrapper_config::parse_bool("ON").unwrap());
        assert!(!super::wrapper_config::parse_bool("Off").unwrap());
    }

    #[test]
    fn a_model_containing_a_single_quote_stays_shell_safe() {
        // `emit_quoted` is what protects `eval "$(claude-cli config set ...)"`
        // from a value that would otherwise break out of its quoting.
        assert_eq!(
            emit_quoted("CLAUDE_CLI_MODEL", "it's-a-model"),
            exit::SUCCESS
        );
        assert_eq!(set("model", "it's-a-model"), exit::SUCCESS);
    }

    #[test]
    fn invalid_config_always_reports_usage() {
        assert_eq!(invalid_config("boom"), exit::USAGE);
        assert_eq!(invalid_config(String::from("boom")), exit::USAGE);
    }
}
