use std::process::{Command, Stdio};
use std::time::Duration;

use nils_common::cli_contract::exit;
use nils_common::diag_output;
use serde::Serialize;
use serde_json::Value;

use crate::agent::oneshot::claude_binary;
use crate::process::{ProcessOutputError, output_with_limits_retry_io};

const AUTH_SCHEMA_VERSION: &str = "claude-cli.auth.v1";
const MAX_STATUS_BYTES: usize = 64 * 1024;
const STATUS_TIMEOUT: Duration = Duration::from_secs(3);

pub struct LoginOptions {
    pub console: bool,
    pub claudeai: bool,
    pub email: Option<String>,
    pub sso: bool,
}

#[derive(Debug, Serialize)]
struct AuthStatusResult {
    logged_in: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    auth_method: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    api_provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    subscription_type: Option<String>,
}

pub fn login(options: &LoginOptions) -> i32 {
    let mut args = vec!["auth".to_string(), "login".to_string()];
    if options.console {
        args.push("--console".to_string());
    } else if options.claudeai {
        args.push("--claudeai".to_string());
    }
    if let Some(email) = options.email.as_deref() {
        args.push("--email".to_string());
        args.push(email.to_string());
    }
    if options.sso {
        args.push("--sso".to_string());
    }
    run_inherited(&args, "login")
}

pub fn logout() -> i32 {
    run_inherited(&["auth".to_string(), "logout".to_string()], "logout")
}

pub fn status(output_json: bool) -> i32 {
    let binary = claude_binary();
    let mut command = Command::new(&binary);
    command
        .args(["auth", "status", "--json"])
        .stdin(Stdio::null());
    let output =
        match output_with_limits_retry_io(&mut command, STATUS_TIMEOUT, MAX_STATUS_BYTES, 3) {
            Ok(output) => output,
            Err(ProcessOutputError::Io(error)) => {
                return emit_status_error(
                    output_json,
                    "launch-failed",
                    format!("failed to run Claude Code auth status: {error}"),
                    exit::UNAVAILABLE,
                );
            }
            Err(ProcessOutputError::Timeout) => {
                return emit_status_error(
                    output_json,
                    "upstream-timeout",
                    "Claude auth status exceeded the 3 second deadline",
                    exit::RUNTIME,
                );
            }
            Err(ProcessOutputError::OutputLimit) => {
                return emit_status_error(
                    output_json,
                    "output-too-large",
                    "Claude auth status output exceeded the safety limit",
                    exit::DATA,
                );
            }
        };
    let upstream_code = match output.status.code() {
        Some(code @ (0 | 1)) => code,
        Some(code) => {
            return emit_status_error(
                output_json,
                "unexpected-upstream-status",
                format!("Claude auth status returned unexpected exit code {code}"),
                exit::RUNTIME,
            );
        }
        None => {
            return emit_status_error(
                output_json,
                "unexpected-upstream-status",
                "Claude auth status terminated without an exit code",
                exit::RUNTIME,
            );
        }
    };
    let value: Value = match serde_json::from_slice(&output.stdout) {
        Ok(value) => value,
        Err(_) => {
            return emit_status_error(
                output_json,
                "invalid-upstream-output",
                "Claude auth status returned invalid JSON",
                exit::DATA,
            );
        }
    };
    if !value.is_object() {
        return emit_status_error(
            output_json,
            "invalid-upstream-shape",
            "Claude auth status returned a non-object JSON value",
            exit::DATA,
        );
    }
    let Some(logged_in) = value.get("loggedIn").and_then(Value::as_bool) else {
        return emit_status_error(
            output_json,
            "invalid-upstream-shape",
            "Claude auth status omitted the required boolean loggedIn field",
            exit::DATA,
        );
    };
    let expected_code = if logged_in { 0 } else { 1 };
    if upstream_code != expected_code {
        return emit_status_error(
            output_json,
            "inconsistent-upstream-status",
            "Claude auth status JSON disagreed with its exit code",
            exit::DATA,
        );
    }
    let result = AuthStatusResult {
        logged_in,
        auth_method: public_string(&value, "authMethod"),
        api_provider: public_string(&value, "apiProvider"),
        subscription_type: public_string(&value, "subscriptionType"),
    };

    if output_json {
        if diag_output::emit_success_result(AUTH_SCHEMA_VERSION, "auth status", &result).is_err() {
            return exit::RUNTIME;
        }
    } else {
        println!(
            "claude: auth logged_in={} method={} provider={} subscription={}",
            result.logged_in,
            result.auth_method.as_deref().unwrap_or("unknown"),
            result.api_provider.as_deref().unwrap_or("unknown"),
            result.subscription_type.as_deref().unwrap_or("unknown")
        );
    }
    expected_code
}

fn public_string(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty() && value.len() <= 128)
        .filter(|value| !value.chars().any(char::is_control))
        .map(str::to_string)
}

fn run_inherited(args: &[String], operation: &str) -> i32 {
    let binary = claude_binary();
    match Command::new(&binary)
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
    {
        Ok(status) => status.code().unwrap_or(exit::RUNTIME),
        Err(error) => {
            eprintln!("claude-cli auth {operation}: failed to launch Claude Code: {error}");
            exit::UNAVAILABLE
        }
    }
}

fn emit_status_error(
    output_json: bool,
    code: &str,
    message: impl Into<String>,
    exit_code: i32,
) -> i32 {
    let message = message.into();
    if output_json {
        let _ = diag_output::emit_error(AUTH_SCHEMA_VERSION, "auth status", code, message, None);
    } else {
        eprintln!("claude-cli auth status: {message}");
    }
    exit_code
}
