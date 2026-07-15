use serde_json::Value;

use crate::cli::ToolProfile;
use crate::error::CliError;

const HARD_DENIED_COMMON: &[&str] = &[
    "agent",
    "analyze",
    "audio",
    "browser",
    "clipboard",
    "config",
    "credentials",
    "image",
    "mcp_agent",
    "shell",
];

const HARD_DENIED_CLI: &[&str] = &[
    "bridge",
    "clean",
    "daemon",
    "mcp",
    "open",
    "permissions",
    "run",
];

const ADMITTED_CLI: &[&str] = &[
    "app",
    "capture",
    "click",
    "dialog",
    "dock",
    "drag",
    "hotkey",
    "inspect_ui",
    "list",
    "menu",
    "menubar",
    "move",
    "paste",
    "perform_action",
    "press",
    "scroll",
    "see",
    "set_value",
    "sleep",
    "space",
    "swipe",
    "tools",
    "type",
    "window",
];

const HARD_DENIED_OPTIONS: &[&str] = &[
    "analyze",
    "api_key",
    "credential",
    "credentials",
    "model",
    "provider",
];

const OBSERVE: &[&str] = &["see", "inspect_ui", "list", "permissions", "sleep"];

const INTERACT: &[&str] = &[
    "see",
    "inspect_ui",
    "list",
    "permissions",
    "sleep",
    "click",
    "type",
    "hotkey",
    "scroll",
    "swipe",
    "drag",
    "move",
    "set_value",
    "perform_action",
    "window",
    "app",
    "menu",
];

const EXTENDED: &[&str] = &[
    "see",
    "inspect_ui",
    "list",
    "permissions",
    "sleep",
    "click",
    "type",
    "hotkey",
    "scroll",
    "swipe",
    "drag",
    "move",
    "set_value",
    "perform_action",
    "window",
    "app",
    "menu",
    "dialog",
    "dock",
    "space",
    "capture",
    "paste",
];

pub fn validate_exec_argv(argv: &[String]) -> Result<(), CliError> {
    let command = argv
        .first()
        .filter(|value| !value.starts_with('-'))
        .map(|value| normalize_tool(value))
        .ok_or_else(|| {
            CliError::usage(
                "Peekaboo argv must start with a command; leading global options are not admitted",
            )
        })?;
    if is_cli_hard_denied(&command) {
        return Err(policy_error(format!(
            "Peekaboo command `{command}` is disabled by the adapter safety ceiling"
        )));
    }
    if !ADMITTED_CLI.contains(&command.as_str()) {
        return Err(policy_error(format!(
            "Peekaboo command `{command}` is not in the pinned adapter allowlist"
        )));
    }
    if argv.iter().skip(1).any(|value| denied_option(value)) {
        return Err(policy_error(format!(
            "Peekaboo command `{command}` requests an AI/provider capability disabled by the adapter safety ceiling"
        )));
    }
    Ok(())
}

pub fn exec_command(argv: &[String]) -> Option<String> {
    argv.first().map(|value| normalize_tool(value))
}

pub fn mutating_command(command: &str) -> bool {
    matches!(
        normalize_tool(command).as_str(),
        "click"
            | "type"
            | "press"
            | "hotkey"
            | "scroll"
            | "swipe"
            | "drag"
            | "move"
            | "set_value"
            | "perform_action"
            | "window"
            | "app"
            | "menu"
            | "menubar"
            | "dialog"
            | "dock"
            | "space"
            | "capture"
            | "paste"
    )
}

pub fn snapshot_lineage(argv: &[String]) -> Option<String> {
    argv.iter().enumerate().find_map(|(index, value)| {
        if value == "--snapshot" {
            return argv
                .get(index + 1)
                .filter(|candidate| !candidate.trim().is_empty() && !candidate.starts_with('-'))
                .cloned();
        }
        value
            .strip_prefix("--snapshot=")
            .filter(|candidate| !candidate.trim().is_empty())
            .map(str::to_owned)
    })
}

pub fn validate_scenario(value: &Value) -> Result<(), CliError> {
    scan_scenario(value, None)
}

fn scan_scenario(value: &Value, key: Option<&str>) -> Result<(), CliError> {
    match value {
        Value::Object(map) => {
            for (child_key, child) in map {
                if HARD_DENIED_OPTIONS.contains(&normalize_tool(child_key).as_str()) {
                    return Err(policy_error(format!(
                        "scenario requests disabled Peekaboo option `{}`",
                        normalize_tool(child_key)
                    )));
                }
                scan_scenario(child, Some(child_key))?;
            }
        }
        Value::Array(values) => {
            for child in values {
                scan_scenario(child, key)?;
            }
        }
        Value::String(text)
            if key
                .is_some_and(|key| matches!(normalize_tool(key).as_str(), "command" | "tool")) =>
        {
            let command = normalize_tool(text);
            if is_cli_hard_denied(&command) || !ADMITTED_CLI.contains(&command.as_str()) {
                return Err(policy_error(format!(
                    "scenario requests disabled or unreviewed Peekaboo capability `{command}`"
                )));
            }
        }
        Value::String(text)
            if key.is_some_and(|key| matches!(normalize_tool(key).as_str(), "action" | "name"))
                && is_cli_hard_denied(&normalize_tool(text)) =>
        {
            return Err(policy_error(format!(
                "scenario requests disabled Peekaboo capability `{}`",
                normalize_tool(text)
            )));
        }
        _ => {}
    }
    Ok(())
}

pub fn allowed_tools(profile: ToolProfile) -> &'static [&'static str] {
    match profile {
        ToolProfile::Observe => OBSERVE,
        ToolProfile::Interact => INTERACT,
        ToolProfile::Extended => EXTENDED,
    }
}

pub fn allowed_tools_csv(profile: ToolProfile) -> String {
    allowed_tools(profile).join(",")
}

pub fn denied_tools_csv() -> String {
    HARD_DENIED_COMMON.join(",")
}

pub fn disabled_capabilities() -> Vec<&'static str> {
    let mut disabled = HARD_DENIED_COMMON.to_vec();
    disabled.extend(["permission_mutation", "http_mcp", "sse_mcp"]);
    disabled.sort_unstable();
    disabled.dedup();
    disabled
}

pub fn tool_allowed(profile: ToolProfile, tool: &str) -> bool {
    let normalized = normalize_tool(tool);
    !is_common_hard_denied(&normalized) && allowed_tools(profile).contains(&normalized.as_str())
}

pub fn mcp_call_allowed(profile: ToolProfile, tool: &str, request: &Value) -> bool {
    let normalized = normalize_tool(tool);
    tool_allowed(profile, &normalized)
        && (normalized != "permissions" || !contains_permission_mutation(request))
}

fn contains_permission_mutation(value: &Value) -> bool {
    match value {
        Value::Object(map) => map.values().any(contains_permission_mutation),
        Value::Array(values) => values.iter().any(contains_permission_mutation),
        Value::String(value) => matches!(
            normalize_tool(value).as_str(),
            "grant" | "request" | "reset" | "authorize" | "open_settings"
        ),
        _ => false,
    }
}

pub fn normalize_tool(value: &str) -> String {
    value.trim().to_ascii_lowercase().replace('-', "_")
}

fn is_common_hard_denied(tool: &str) -> bool {
    HARD_DENIED_COMMON.contains(&tool)
}

fn is_cli_hard_denied(tool: &str) -> bool {
    is_common_hard_denied(tool) || HARD_DENIED_CLI.contains(&tool)
}

fn denied_option(value: &str) -> bool {
    let option = value
        .trim_start_matches('-')
        .split('=')
        .next()
        .map(normalize_tool)
        .unwrap_or_default();
    HARD_DENIED_OPTIONS.contains(&option.as_str())
}

fn policy_error(message: impl Into<String>) -> CliError {
    CliError::policy(message).with_operation("policy")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        ToolProfile, allowed_tools, mcp_call_allowed, tool_allowed, validate_exec_argv,
        validate_scenario,
    };

    #[test]
    fn hard_denials_apply_to_cli_and_scenarios() {
        assert!(validate_exec_argv(&["see".into(), "--json".into()]).is_ok());
        assert!(
            validate_exec_argv(&["see".into(), "--analyze".into(), "describe".into()]).is_err()
        );
        assert!(validate_exec_argv(&["see".into(), "--analyze=describe".into()]).is_err());
        assert!(validate_exec_argv(&["future-unsafe-command".into()]).is_err());
        assert!(validate_exec_argv(&["browser".into(), "status".into()]).is_err());
        assert!(validate_exec_argv(&["permissions".into(), "grant".into()]).is_err());
        assert!(validate_exec_argv(&["bridge".into(), "status".into()]).is_err());
        assert!(validate_exec_argv(&["run".into(), "unreviewed.json".into()]).is_err());
        assert!(
            validate_exec_argv(&[
                "--bridge-socket".into(),
                "/private/path".into(),
                "see".into()
            ])
            .is_err()
        );
        assert!(validate_scenario(&json!({"steps":[{"command":"shell"}]})).is_err());
        assert!(
            validate_scenario(&json!({"steps":[{"command":"future-unsafe-command"}]})).is_err()
        );
        assert!(
            validate_scenario(&json!({
                "steps":[{"command":"see","analyze":"describe the screen"}]
            }))
            .is_err()
        );
    }

    #[test]
    fn profiles_are_monotonic_but_never_admit_hard_denials() {
        assert!(
            allowed_tools(ToolProfile::Observe).len() < allowed_tools(ToolProfile::Interact).len()
        );
        assert!(
            allowed_tools(ToolProfile::Interact).len() < allowed_tools(ToolProfile::Extended).len()
        );
        for profile in [
            ToolProfile::Observe,
            ToolProfile::Interact,
            ToolProfile::Extended,
        ] {
            assert!(!tool_allowed(profile, "shell"));
            assert!(!tool_allowed(profile, "browser"));
            assert!(!tool_allowed(profile, "image"));
            assert!(!tool_allowed(profile, "clipboard"));
        }
    }

    #[test]
    fn profiles_are_exactly_the_documented_upstream_tool_sets() {
        assert_eq!(
            allowed_tools(ToolProfile::Observe),
            ["see", "inspect_ui", "list", "permissions", "sleep"]
        );
        assert_eq!(
            &allowed_tools(ToolProfile::Interact)[5..],
            [
                "click",
                "type",
                "hotkey",
                "scroll",
                "swipe",
                "drag",
                "move",
                "set_value",
                "perform_action",
                "window",
                "app",
                "menu",
            ]
        );
        assert_eq!(
            &allowed_tools(ToolProfile::Extended)[17..],
            ["dialog", "dock", "space", "capture", "paste"]
        );
    }

    #[test]
    fn mcp_permission_status_is_observable_but_mutation_is_never_admitted() {
        assert!(mcp_call_allowed(
            ToolProfile::Observe,
            "permissions",
            &json!({"params":{"name":"permissions","arguments":{"action":"status"}}})
        ));
        assert!(!mcp_call_allowed(
            ToolProfile::Extended,
            "permissions",
            &json!({"params":{"name":"permissions","arguments":{"action":"grant"}}})
        ));
    }
}
