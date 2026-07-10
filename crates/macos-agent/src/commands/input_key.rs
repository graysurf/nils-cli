use std::time::Instant;

use crate::backend::applescript;
use crate::backend::process::ProcessRunner;
use crate::cli::{InputKeyArgs, OutputFormat};
use crate::commands::input_hotkey::validate_key;
use crate::commands::{emit_json_success, reject_tsv_for_list_only};
use crate::error::CliError;
use crate::model::InputKeyResult;
use crate::retry::run_with_retry;
use crate::run::{
    ActionPolicy, action_policy_result, build_action_meta_with_attempts, next_action_id,
};

pub fn run(
    format: OutputFormat,
    args: &InputKeyArgs,
    policy: ActionPolicy,
    runner: &dyn ProcessRunner,
) -> Result<(), CliError> {
    let key = args.key.trim();
    validate_key(key)?;
    if args.count == 0 {
        return Err(CliError::usage("--count must be at least 1"));
    }

    let action_id = next_action_id("input.key");
    let started = Instant::now();
    let mut attempts_used = 0u8;
    if !policy.dry_run {
        let retry = policy.retry_policy();
        let (_, attempts) = run_with_retry(retry, || {
            applescript::send_key(runner, key, args.count, policy.timeout_ms)
        })?;
        attempts_used = attempts;
    }

    let result = InputKeyResult {
        key: key.to_string(),
        count: args.count,
        policy: action_policy_result(policy),
        meta: build_action_meta_with_attempts(action_id, started, policy, attempts_used),
    };
    match format {
        OutputFormat::Json => emit_json_success("input.key", result)?,
        OutputFormat::Text => println!(
            "input.key\taction_id={}\tkey={}\tcount={}\telapsed_ms={}",
            result.meta.action_id, result.key, result.count, result.meta.elapsed_ms
        ),
        OutputFormat::Tsv => return reject_tsv_for_list_only(),
    }
    Ok(())
}
