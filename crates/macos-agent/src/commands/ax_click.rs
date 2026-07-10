use std::time::Instant;

use crate::backend::cliclick;
use crate::backend::process::ProcessRunner;
use crate::backend::{AutoAxBackend, AxBackendAdapter};
use crate::cli::{AxClickArgs, AxPostconditionArgs, MouseButton, OutputFormat};
use crate::commands::ax_common::{
    AxActionGateOptions, AxPostconditionCheck, AxPostconditionOptions, build_selector_from_args,
    build_target_from_args, evaluate_selector_against_backend, parse_postcondition_expected_value,
    run_action_gates, run_postconditions, selector_selection_error,
};
use crate::commands::{emit_json_success, reject_tsv_for_list_only};
use crate::error::CliError;
use crate::model::{
    AxClickCommandResult, AxClickFallbackStage, AxClickRequest, AxClickResult, AxSelector,
};
use crate::retry::run_with_retry;
use crate::run::{
    ActionPolicy, action_policy_result, build_action_meta_with_attempts, next_action_id,
};

pub fn run(
    format: OutputFormat,
    args: &AxClickArgs,
    policy: ActionPolicy,
    runner: &dyn ProcessRunner,
) -> Result<(), CliError> {
    let mut request = build_request(args)?;
    let action_id = next_action_id("ax.click");
    let started = Instant::now();
    let mut attempts_used = 0u8;
    let mut detail = AxClickResult {
        node_id: request.selector.node_id.clone(),
        matched_count: 0,
        action: "dry-run".to_string(),
        used_coordinate_fallback: false,
        fallback_x: None,
        fallback_y: None,
        fallback_order: request.fallback_order.clone(),
        attempted_stages: Vec::new(),
        selector_explain: None,
        gates: None,
        postconditions: None,
    };
    let gate_options = build_gate_options(args);
    let postcondition_options =
        build_postcondition_options(&args.postcondition, args.wait_timeout_ms, args.wait_poll_ms)?;

    if !policy.dry_run {
        let backend = AutoAxBackend::default();
        let gate_result = run_action_gates(
            "ax.click",
            runner,
            &backend,
            &request.target,
            &request.selector,
            gate_options,
            policy.timeout_ms,
        )?;
        let selector_evaluation = evaluate_selector_against_backend(
            runner,
            &backend,
            &request.target,
            &request.selector,
            policy.timeout_ms,
        )?;
        if let Some(error) =
            selector_selection_error("ax.click", selector_evaluation.selection_status)
        {
            return Err(error);
        }
        let selected_node_id = selector_evaluation
            .selected_node_id
            .clone()
            .ok_or_else(|| {
                CliError::ax_contract_failure("ax.click", "selector evaluation returned no node")
            })?;
        request.selector = AxSelector {
            node_id: Some(selected_node_id.clone()),
            ..AxSelector::default()
        };

        let retry = policy.retry_policy();
        let (mut backend_result, attempts) =
            run_with_retry(retry, || backend.click(runner, &request, policy.timeout_ms)).map_err(
                |error| {
                    error.with_hint(format!(
                        "Attempted fallback stages: {}",
                        format_fallback_stages(&request.fallback_order)
                    ))
                },
            )?;
        attempts_used = attempts;
        let mut attempted_stages = vec![AxClickFallbackStage::AxPress];
        if backend_result.used_coordinate_fallback {
            attempted_stages.push(AxClickFallbackStage::Coordinate);
            let x = backend_result.fallback_x.ok_or_else(|| {
                CliError::ax_contract_failure(
                    "ax.click",
                    "backend requested coordinate fallback but x coordinate is missing",
                )
            })?;
            let y = backend_result.fallback_y.ok_or_else(|| {
                CliError::ax_contract_failure(
                    "ax.click",
                    "backend requested coordinate fallback but y coordinate is missing",
                )
            })?;
            cliclick::click(runner, x, y, MouseButton::Left, 1, &[], policy.timeout_ms)?;
            backend_result.action = "coordinate-fallback".to_string();
        }

        backend_result.fallback_order = request.fallback_order.clone();
        backend_result.attempted_stages = attempted_stages;
        backend_result.selector_explain = if format == OutputFormat::Json {
            selector_evaluation.explain
        } else {
            None
        };
        if backend_result.node_id.is_none() {
            backend_result.node_id = Some(selected_node_id.clone());
        }
        if backend_result.matched_count == 0 {
            backend_result.matched_count = selector_evaluation.matched_count;
        }
        backend_result.gates = gate_result;
        backend_result.postconditions = run_postconditions(
            "ax.click",
            runner,
            &backend,
            &request.target,
            &selected_node_id,
            &postcondition_options,
            policy.timeout_ms,
        )?;
        detail = backend_result;
    }

    let result = AxClickCommandResult {
        detail,
        policy: action_policy_result(policy),
        meta: build_action_meta_with_attempts(action_id, started, policy, attempts_used),
    };

    match format {
        OutputFormat::Json => {
            emit_json_success("ax.click", result)?;
        }
        OutputFormat::Text => {
            println!(
                "ax.click\taction_id={}\tnode_id={}\taction={}\tmatched_count={}\telapsed_ms={}",
                result.meta.action_id,
                result.detail.node_id.unwrap_or_default(),
                result.detail.action,
                result.detail.matched_count,
                result.meta.elapsed_ms,
            );
        }
        OutputFormat::Tsv => {
            return reject_tsv_for_list_only();
        }
    }

    Ok(())
}

fn build_request(args: &AxClickArgs) -> Result<AxClickRequest, CliError> {
    let target = build_target_from_args(&args.target)?;
    let selector = build_selector_from_args(&args.selector)?;
    let fallback_order =
        normalized_fallback_order(&args.fallback_order, args.allow_coordinate_fallback);
    let allow_coordinate_fallback = args.allow_coordinate_fallback
        || fallback_order.contains(&AxClickFallbackStage::Coordinate);
    Ok(AxClickRequest {
        target,
        selector,
        allow_coordinate_fallback,
        reselect_before_click: args.reselect_before_click,
        fallback_order,
    })
}

fn build_gate_options(args: &AxClickArgs) -> AxActionGateOptions {
    AxActionGateOptions {
        app_active: args.gate.gate_app_active,
        window_present: args.gate.gate_window_present,
        ax_present: args.gate.gate_ax_present,
        ax_unique: args.gate.gate_ax_unique,
        timeout_ms: args.wait_timeout_ms.unwrap_or(args.gate.gate_timeout_ms),
        poll_ms: args.wait_poll_ms.unwrap_or(args.gate.gate_poll_ms),
    }
}

fn build_postcondition_options(
    args: &AxPostconditionArgs,
    wait_timeout_ms: Option<u64>,
    wait_poll_ms: Option<u64>,
) -> Result<AxPostconditionOptions, CliError> {
    let mut checks = Vec::new();
    if let Some(expected) = args.postcondition_focused {
        checks.push(AxPostconditionCheck::Focused(expected));
    }
    if let (Some(name), Some(raw_expected)) = (
        args.postcondition_attribute.as_ref(),
        args.postcondition_attribute_value.as_ref(),
    ) {
        if name.trim().is_empty() {
            return Err(CliError::usage("--postcondition-attribute cannot be empty"));
        }
        checks.push(AxPostconditionCheck::AttributeValue {
            name: name.clone(),
            expected: parse_postcondition_expected_value(raw_expected),
        });
    }
    Ok(AxPostconditionOptions {
        checks,
        timeout_ms: wait_timeout_ms.unwrap_or(args.postcondition_timeout_ms),
        poll_ms: wait_poll_ms.unwrap_or(args.postcondition_poll_ms),
    })
}

fn normalized_fallback_order(
    user_order: &[AxClickFallbackStage],
    allow_coordinate_fallback: bool,
) -> Vec<AxClickFallbackStage> {
    if user_order.is_empty() {
        let mut stages = vec![AxClickFallbackStage::AxPress];
        if allow_coordinate_fallback {
            stages.push(AxClickFallbackStage::Coordinate);
        }
        return stages;
    }

    let mut deduped = Vec::new();
    for stage in user_order {
        if !deduped.contains(stage) {
            deduped.push(*stage);
        }
    }

    if !deduped.contains(&AxClickFallbackStage::AxPress) {
        deduped.insert(0, AxClickFallbackStage::AxPress);
    }
    if allow_coordinate_fallback && !deduped.contains(&AxClickFallbackStage::Coordinate) {
        deduped.push(AxClickFallbackStage::Coordinate);
    }

    deduped
}

fn format_fallback_stages(stages: &[AxClickFallbackStage]) -> String {
    stages
        .iter()
        .map(|stage| match stage {
            AxClickFallbackStage::AxPress => "ax-press",
            AxClickFallbackStage::AxConfirm => "ax-confirm",
            AxClickFallbackStage::FrameCenter => "frame-center",
            AxClickFallbackStage::Coordinate => "coordinate",
        })
        .collect::<Vec<_>>()
        .join(",")
}
