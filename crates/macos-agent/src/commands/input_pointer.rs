use std::time::Instant;

use crate::backend::process::ProcessRunner;
use crate::backend::{applescript, cliclick, hammerspoon_input};
use crate::cli::{InputDragArgs, InputMoveArgs, InputScrollArgs, OutputFormat, ScrollUnit};
use crate::commands::{emit_json_success, reject_tsv_for_list_only};
use crate::error::CliError;
use crate::model::{InputDragResult, InputMoveResult, InputScrollResult};
use crate::retry::run_with_retry;
use crate::run::{
    ActionPolicy, action_policy_result, build_action_meta_with_attempts, next_action_id,
};

const CLICLICK_EVENT_DELAY_MS: u64 = 20;
const DRAG_PROCESS_HEADROOM_MS: u64 = 250;

pub fn run_move(
    format: OutputFormat,
    args: &InputMoveArgs,
    policy: ActionPolicy,
    runner: &dyn ProcessRunner,
) -> Result<(), CliError> {
    let action_id = next_action_id("input.move");
    let started = Instant::now();
    let mut attempts_used = 0u8;
    if !policy.dry_run {
        let (_, attempts) = run_with_retry(policy.retry_policy(), || {
            cliclick::move_pointer(runner, args.x, args.y, policy.timeout_ms)
        })?;
        attempts_used = attempts;
    }
    let result = InputMoveResult {
        x: args.x,
        y: args.y,
        policy: action_policy_result(policy),
        meta: build_action_meta_with_attempts(action_id, started, policy, attempts_used),
    };
    match format {
        OutputFormat::Json => emit_json_success("input.move", result)?,
        OutputFormat::Text => println!(
            "input.move\taction_id={}\tx={}\ty={}\telapsed_ms={}",
            result.meta.action_id, result.x, result.y, result.meta.elapsed_ms
        ),
        OutputFormat::Tsv => return reject_tsv_for_list_only(),
    }
    Ok(())
}

pub fn run_drag(
    format: OutputFormat,
    args: &InputDragArgs,
    policy: ActionPolicy,
    runner: &dyn ProcessRunner,
) -> Result<(), CliError> {
    if args.steps == 0 || args.steps > 100 {
        return Err(CliError::usage("--steps must be between 1 and 100"));
    }
    let modifiers = if args.mods.trim().is_empty() {
        Vec::new()
    } else {
        applescript::parse_modifiers(&args.mods)?
    };
    let required_timeout_ms = args
        .duration_ms
        .saturating_add(u64::from(args.steps + 2) * CLICLICK_EVENT_DELAY_MS)
        .saturating_add(DRAG_PROCESS_HEADROOM_MS);
    if required_timeout_ms > policy.timeout_ms {
        return Err(CliError::usage(format!(
            "--duration-ms {} with --steps {} requires --timeout-ms of at least {required_timeout_ms}",
            args.duration_ms, args.steps
        )));
    }
    let action_id = next_action_id("input.drag");
    let started = Instant::now();
    let mut attempts_used = 0u8;
    if !policy.dry_run {
        let (_, attempts) = run_with_retry(policy.retry_policy(), || {
            cliclick::drag(
                runner,
                args.from_x,
                args.from_y,
                args.to_x,
                args.to_y,
                args.duration_ms,
                args.steps,
                &modifiers,
                policy.timeout_ms,
            )
        })?;
        attempts_used = attempts;
    }
    let result = InputDragResult {
        from_x: args.from_x,
        from_y: args.from_y,
        to_x: args.to_x,
        to_y: args.to_y,
        duration_ms: args.duration_ms,
        steps: args.steps,
        mods: modifiers
            .iter()
            .map(|modifier| modifier.canonical().to_string())
            .collect(),
        policy: action_policy_result(policy),
        meta: build_action_meta_with_attempts(action_id, started, policy, attempts_used),
    };
    match format {
        OutputFormat::Json => emit_json_success("input.drag", result)?,
        OutputFormat::Text => println!(
            "input.drag\taction_id={}\tfrom={},{}\tto={},{}\tduration_ms={}\tsteps={}\tmods={}\telapsed_ms={}",
            result.meta.action_id,
            result.from_x,
            result.from_y,
            result.to_x,
            result.to_y,
            result.duration_ms,
            result.steps,
            result.mods.join(","),
            result.meta.elapsed_ms
        ),
        OutputFormat::Tsv => return reject_tsv_for_list_only(),
    }
    Ok(())
}

pub fn run_scroll(
    format: OutputFormat,
    args: &InputScrollArgs,
    policy: ActionPolicy,
    runner: &dyn ProcessRunner,
) -> Result<(), CliError> {
    if args.delta_x == 0 && args.delta_y == 0 {
        return Err(CliError::usage(
            "at least one of --delta-x or --delta-y must be nonzero",
        ));
    }
    let modifiers = if args.mods.trim().is_empty() {
        Vec::new()
    } else {
        applescript::parse_modifiers(&args.mods)?
    };
    let action_id = next_action_id("input.scroll");
    let started = Instant::now();
    let mut attempts_used = 0u8;
    if !policy.dry_run {
        let (_, attempts) = run_with_retry(policy.retry_policy(), || {
            hammerspoon_input::scroll(
                runner,
                args.delta_x,
                args.delta_y,
                args.unit,
                &modifiers,
                policy.timeout_ms,
            )
        })?;
        attempts_used = attempts;
    }
    let unit = match args.unit {
        ScrollUnit::Pixel => "pixel",
        ScrollUnit::Line => "line",
    };
    let result = InputScrollResult {
        delta_x: args.delta_x,
        delta_y: args.delta_y,
        unit,
        mods: modifiers
            .iter()
            .map(|modifier| modifier.canonical().to_string())
            .collect(),
        policy: action_policy_result(policy),
        meta: build_action_meta_with_attempts(action_id, started, policy, attempts_used),
    };
    match format {
        OutputFormat::Json => emit_json_success("input.scroll", result)?,
        OutputFormat::Text => println!(
            "input.scroll\taction_id={}\tdelta_x={}\tdelta_y={}\tunit={}\tmods={}\telapsed_ms={}",
            result.meta.action_id,
            result.delta_x,
            result.delta_y,
            result.unit,
            result.mods.join(","),
            result.meta.elapsed_ms
        ),
        OutputFormat::Tsv => return reject_tsv_for_list_only(),
    }
    Ok(())
}
