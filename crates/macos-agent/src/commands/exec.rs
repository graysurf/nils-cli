use std::path::Path;
use std::time::Duration;

use nils_common::fs::{SECRET_FILE_MODE, write_atomic};

use crate::cli::ExecArgs;
use crate::commands::{hardened_env, peekaboo_binary, prepare_runtime, runtime_argv};
use crate::error::{CliError, ErrorClass};
use crate::journal::{
    ArtifactInput, Journal, StepInput, StepStatus, sanitize_json, sanitize_output,
    sanitize_result_json,
};
use crate::model::{ExecutionResult, UpstreamResult};
use crate::policy;
use crate::process;
use crate::test_mode;

pub struct ExecOutcome {
    pub result: ExecutionResult,
    pub exit_code: u8,
}

pub fn run_local(
    args: &ExecArgs,
    parent_id: Option<String>,
    transport: &'static str,
) -> Result<ExecOutcome, CliError> {
    if let Err(error) = validate(args) {
        let mut journal = Journal::open(
            &args.out_dir,
            args.runtime,
            transport,
            args.evidence_mode,
            None,
        )?;
        let _ = journal.record_step(StepInput {
            parent_id,
            intent: args.intent.clone(),
            expected: args.expected.clone(),
            argv: args.argv.clone(),
            status: StepStatus::PolicyBlocked,
            failure_class: Some("policy".into()),
            duration_ms: 0,
            retries: 0,
            precondition_refs: Vec::new(),
            postcondition_refs: Vec::new(),
            snapshot_lineage: None,
            artifact_refs: Vec::new(),
        });
        let _ = journal.close();
        return Err(error);
    }
    let binary = peekaboo_binary()?;
    let mut journal = Journal::open_for_backend(
        &args.out_dir,
        args.runtime,
        transport,
        args.evidence_mode,
        None,
        &binary,
    )?;
    if let Err(error) = prepare_runtime(args.runtime, &binary) {
        let _ = journal.record_step(StepInput {
            parent_id,
            intent: args.intent.clone(),
            expected: args.expected.clone(),
            argv: args.argv.clone(),
            status: StepStatus::Failed,
            failure_class: Some("backend_drift".into()),
            duration_ms: 0,
            retries: 0,
            precondition_refs: Vec::new(),
            postcondition_refs: Vec::new(),
            snapshot_lineage: None,
            artifact_refs: Vec::new(),
        });
        let _ = journal.close();
        return Err(error);
    }
    let (envs, removed_envs) = hardened_env(None);
    let upstream_argv = runtime_argv(args.runtime, &args.argv, binary.runtime_identity());
    let output = process::run(
        binary.path(),
        &upstream_argv,
        &envs,
        &removed_envs,
        None,
        Duration::from_secs(args.timeout_seconds.clamp(1, 3600)),
    )
    .map_err(|_| {
        CliError::upstream("failed to start the locked Peekaboo executable").with_operation("exec")
    })?;
    let command = policy::exec_command(&args.argv).unwrap_or_else(|| "unknown".into());
    let mutating = policy::mutating_command(&command);
    let mut status = if output.timed_out && mutating {
        StepStatus::Unknown
    } else if output.timed_out || output.exit_code != 0 {
        StepStatus::Failed
    } else {
        StepStatus::Passed
    };
    let mut failure_class = if output.timed_out && mutating {
        Some("unknown_mutation".into())
    } else if output.timed_out {
        Some("upstream_timeout".into())
    } else if output.signal.is_some() {
        Some("upstream_signal".into())
    } else if output.exit_code != 0 {
        let diagnostic = String::from_utf8_lossy(&output.stderr).to_ascii_lowercase();
        if diagnostic.contains("permission")
            || diagnostic.contains("accessibility")
            || diagnostic.contains("screen recording")
        {
            Some("permission".into())
        } else {
            Some("upstream".into())
        }
    } else {
        None
    };

    let stdout_text = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr_text = String::from_utf8_lossy(&output.stderr).to_string();
    let parsed = serde_json::from_str::<serde_json::Value>(stdout_text.trim()).ok();
    let malformed_json = args
        .argv
        .iter()
        .any(|argument| matches!(argument.as_str(), "--json" | "--json-output"))
        && !output.timed_out
        && output.exit_code == 0
        && parsed.is_none();
    if malformed_json {
        status = StepStatus::Failed;
        failure_class = Some("upstream_malformed_json".into());
    }
    let debug_artifact = if args.evidence_mode == crate::cli::EvidenceMode::Debug {
        Some(write_debug_artifact(
            &args.out_dir,
            parsed.as_ref(),
            &stdout_text,
            &stderr_text,
            args.evidence_mode,
        )?)
    } else {
        None
    };
    let postconditions = if mutating {
        vec!["caller-observation-required".into()]
    } else {
        Vec::new()
    };
    let snapshot_lineage = mutating
        .then(|| policy::snapshot_lineage(&args.argv))
        .flatten();
    let preconditions = snapshot_lineage.iter().cloned().collect();
    let step = journal.record_step(StepInput {
        parent_id,
        intent: args.intent.clone(),
        expected: args.expected.clone(),
        argv: args.argv.clone(),
        status,
        failure_class,
        duration_ms: output.elapsed_ms,
        retries: 0,
        precondition_refs: preconditions,
        postcondition_refs: postconditions,
        snapshot_lineage,
        artifact_refs: debug_artifact.iter().cloned().collect(),
    })?;
    if let Some(relative) = debug_artifact {
        journal.register_artifact(ArtifactInput {
            step: &step.id,
            path: &args.out_dir.join(&relative),
            kind: "upstream_result",
            mime: "application/json",
            sensitivity: "private",
            redaction: "sanitized",
            retention: "debug",
        })?;
    }
    journal.close()?;

    let json = parsed
        .as_ref()
        .map(|value| sanitize_result_json(value, args.evidence_mode));
    let text = (json.is_none() && !stdout_text.trim().is_empty())
        .then(|| sanitize_output(stdout_text.trim(), args.evidence_mode));
    let diagnostic = (!stderr_text.trim().is_empty())
        .then(|| sanitize_output(stderr_text.trim(), args.evidence_mode));
    let exit_code = if output.timed_out || output.exit_code != 0 || malformed_json {
        ErrorClass::Upstream.exit_code()
    } else {
        0
    };
    Ok(ExecOutcome {
        result: ExecutionResult {
            transport,
            runtime: args.runtime,
            evidence_mode: args.evidence_mode,
            journal_step: step.id,
            upstream: UpstreamResult {
                exit_code: output.exit_code,
                signal: output.signal,
                timed_out: output.timed_out,
                stdout_truncated: output.stdout_truncated,
                stderr_truncated: output.stderr_truncated,
                json,
                text,
                diagnostic,
            },
        },
        exit_code,
    })
}

fn validate(args: &ExecArgs) -> Result<(), CliError> {
    policy::validate_exec_argv(&args.argv)?;
    let command = policy::exec_command(&args.argv).unwrap_or_default();
    if policy::mutating_command(&command)
        && args
            .expected
            .as_deref()
            .is_none_or(|value| value.trim().is_empty())
    {
        return Err(CliError::policy(
            "mutating Peekaboo commands require --expected with an observable postcondition",
        )
        .with_operation("exec"));
    }
    Ok(())
}

fn write_debug_artifact(
    root: &Path,
    parsed: Option<&serde_json::Value>,
    stdout: &str,
    stderr: &str,
    mode: crate::cli::EvidenceMode,
) -> Result<String, CliError> {
    let filename = format!(
        "artifacts/upstream-{}-{}.json",
        test_mode::timestamp()
            .chars()
            .filter(|character| character.is_ascii_alphanumeric())
            .take(28)
            .collect::<String>(),
        std::process::id()
    );
    let payload = serde_json::json!({
        "schema_version": "macos-agent.debug-upstream.v1",
        "stdout": parsed
            .map(|value| sanitize_json(value, mode))
            .unwrap_or_else(|| serde_json::Value::String(sanitize_output(stdout, mode))),
        "stderr": sanitize_output(stderr, mode),
    });
    let body = serde_json::to_vec_pretty(&payload)
        .map_err(|_| CliError::journal("failed to encode debug upstream artifact"))?;
    write_atomic(&root.join(&filename), &body, SECRET_FILE_MODE)
        .map_err(|_| CliError::journal("failed to write debug upstream artifact"))?;
    Ok(filename)
}
