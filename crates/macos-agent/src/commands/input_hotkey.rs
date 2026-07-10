use std::time::Instant;

use crate::backend::applescript;
use crate::backend::process::ProcessRunner;
use crate::cli::{InputHotkeyArgs, OutputFormat};
use crate::commands::{emit_json_success, reject_tsv_for_list_only};
use crate::error::CliError;
use crate::model::InputHotkeyResult;
use crate::retry::run_with_retry;
use crate::run::{
    ActionPolicy, action_policy_result, build_action_meta_with_attempts, next_action_id,
};

const NAMED_KEYS: &[&str] = &[
    "tab", "return", "enter", "escape", "space", "left", "right", "up", "down", "delete",
];

pub fn run(
    format: OutputFormat,
    args: &InputHotkeyArgs,
    policy: ActionPolicy,
    runner: &dyn ProcessRunner,
) -> Result<(), CliError> {
    let key = args.key.trim();
    validate_key(key)?;
    let modifiers = applescript::parse_modifiers(&args.mods)?;

    let action_id = next_action_id("input.hotkey");
    let started = Instant::now();
    let mut attempts_used = 0u8;

    if !policy.dry_run {
        let retry = policy.retry_policy();
        let (_, attempts) = run_with_retry(retry, || {
            applescript::send_hotkey(runner, &modifiers, key, policy.timeout_ms)
        })?;
        attempts_used = attempts;
    }

    let mods = modifiers
        .iter()
        .map(|modifier| modifier.canonical().to_string())
        .collect::<Vec<_>>();

    let result = InputHotkeyResult {
        mods,
        key: key.to_string(),
        policy: action_policy_result(policy),
        meta: build_action_meta_with_attempts(action_id, started, policy, attempts_used),
    };

    match format {
        OutputFormat::Json => {
            emit_json_success("input.hotkey", result)?;
        }
        OutputFormat::Text => {
            println!(
                "input.hotkey\taction_id={}\tmods={}\tkey={}\telapsed_ms={}",
                result.meta.action_id,
                result.mods.join(","),
                result.key,
                result.meta.elapsed_ms,
            );
        }
        OutputFormat::Tsv => {
            return reject_tsv_for_list_only();
        }
    }

    Ok(())
}

pub(crate) fn validate_key(key: &str) -> Result<(), CliError> {
    let token = key.trim();
    if token.is_empty() {
        return Err(CliError::usage("--key cannot be empty"));
    }

    if token.chars().count() == 1 || NAMED_KEYS.contains(&token.to_ascii_lowercase().as_str()) {
        return Ok(());
    }

    Err(CliError::usage(format!(
        "unsupported --key `{key}`; use a single character or one of: {}",
        NAMED_KEYS.join(",")
    )))
}

#[cfg(test)]
mod tests {
    use super::validate_key;

    #[test]
    fn validate_key_accepts_single_char_and_named_key() {
        validate_key("4").expect("single char should pass");
        validate_key("tab").expect("named key should pass");
    }

    #[test]
    fn validate_key_rejects_long_unknown_key() {
        let err = validate_key("unknown-key").expect_err("should fail for unknown key");
        assert_eq!(err.exit_code(), 2);
    }
}
