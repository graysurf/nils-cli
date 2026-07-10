use std::time::Instant;

use crate::backend::applescript;
use crate::backend::cliclick;
use crate::backend::process::ProcessRunner;
use crate::cli::{InputClickArgs, OutputFormat};
use crate::commands::{emit_json_success, reject_tsv_for_list_only};
use crate::error::CliError;
use crate::model::InputClickResult;
use crate::retry::run_with_retry;
use crate::run::{
    ActionPolicy, action_policy_result, build_action_meta_with_attempts, next_action_id,
};
use crate::wait;

pub fn run(
    format: OutputFormat,
    args: &InputClickArgs,
    policy: ActionPolicy,
    runner: &dyn ProcessRunner,
) -> Result<(), CliError> {
    if args.count == 0 {
        return Err(CliError::usage("--count must be at least 1"));
    }
    let modifiers = if args.mods.trim().is_empty() {
        Vec::new()
    } else {
        applescript::parse_modifiers(&args.mods)?
    };

    let action_id = next_action_id("input.click");
    let started = Instant::now();
    let mut attempts_used = 0u8;

    if !policy.dry_run {
        wait::sleep_ms(args.pre_wait_ms);
        let retry = policy.retry_policy();
        let (_, attempts) = run_with_retry(retry, || {
            cliclick::click(
                runner,
                args.x,
                args.y,
                args.button,
                args.count,
                &modifiers,
                policy.timeout_ms,
            )
        })?;
        attempts_used = attempts;
        wait::sleep_ms(args.post_wait_ms);
    }

    let result = InputClickResult {
        x: args.x,
        y: args.y,
        button: cliclick::button_name(args.button),
        count: args.count,
        mods: modifiers
            .iter()
            .map(|modifier| modifier.canonical().to_string())
            .collect(),
        policy: action_policy_result(policy),
        meta: build_action_meta_with_attempts(action_id, started, policy, attempts_used),
    };

    match format {
        OutputFormat::Json => {
            emit_json_success("input.click", result)?;
        }
        OutputFormat::Text => {
            println!(
                "input.click\taction_id={}\tx={}\ty={}\tbutton={}\tcount={}\tmods={}\telapsed_ms={}",
                result.meta.action_id,
                result.x,
                result.y,
                result.button,
                result.count,
                result.mods.join(","),
                result.meta.elapsed_ms,
            );
        }
        OutputFormat::Tsv => {
            return reject_tsv_for_list_only();
        }
    }

    Ok(())
}
