use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use nils_common::fs::{SECRET_FILE_MODE, write_atomic};
use sha2::{Digest, Sha256};

use crate::cli::ScenarioArgs;
use crate::commands::{hardened_env, peekaboo_binary, prepare_runtime, runtime_argv};
use crate::error::{CliError, ErrorClass};
use crate::journal::{Journal, StepInput, StepStatus, sanitize_output, sanitize_result_json};
use crate::model::{ExecutionResult, UpstreamResult};
use crate::policy;
use crate::process;

pub struct ScenarioOutcome {
    pub result: ExecutionResult,
    pub exit_code: u8,
}

pub fn run_local(
    args: &ScenarioArgs,
    transport: &'static str,
) -> Result<ScenarioOutcome, CliError> {
    let raw = validate_source(args)?;
    let scenario: serde_json::Value = serde_json::from_slice(&raw).map_err(|error| {
        CliError::usage(format!("scenario is not valid JSON: {error}")).with_operation("scenario")
    })?;
    policy::validate_scenario(&scenario)?;
    let binary = peekaboo_binary()?;
    let mut journal = Journal::open_for_backend(
        &args.out_dir,
        args.runtime,
        transport,
        args.evidence_mode,
        None,
        &binary,
    )?;
    prepare_runtime(args.runtime, binary.path())?;
    let source_digest = hex(&Sha256::digest(&raw));
    let staged_source = StagedScenario::create(&args.out_dir, &raw)?;
    let upstream = vec![
        "run".into(),
        staged_source.path().to_string_lossy().into_owned(),
        "--json".into(),
    ];
    let (envs, removed_envs) = hardened_env(None);
    let output = process::run(
        binary.path(),
        &runtime_argv(args.runtime, &upstream),
        &envs,
        &removed_envs,
        None,
        Duration::from_secs(args.timeout_seconds.clamp(1, 3600)),
    )
    .map_err(|_| {
        CliError::upstream("failed to start the locked Peekaboo scenario runner")
            .with_operation("scenario")
    })?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let parsed = serde_json::from_str::<serde_json::Value>(stdout.trim()).ok();
    let malformed_json = !output.timed_out && output.exit_code == 0 && parsed.is_none();
    let status = if output.timed_out {
        StepStatus::Unknown
    } else if output.exit_code == 0 && !malformed_json {
        StepStatus::Passed
    } else {
        StepStatus::Failed
    };
    let failure_class = if output.timed_out {
        Some("unknown_mutation".into())
    } else if output.signal.is_some() {
        Some("upstream_signal".into())
    } else if malformed_json {
        Some("upstream_malformed_json".into())
    } else if output.exit_code != 0 {
        Some("upstream_scenario".into())
    } else {
        None
    };
    let step = journal.record_step(StepInput {
        parent_id: None,
        intent: Some("run reviewed Peekaboo scenario".into()),
        expected: Some("scenario assertions determine per-step success".into()),
        argv: vec!["run".into(), format!("sha256:{source_digest}")],
        status,
        failure_class,
        duration_ms: output.elapsed_ms,
        retries: 0,
        precondition_refs: vec![format!("scenario-sha256-{source_digest}")],
        postcondition_refs: vec!["scenario-assertions".into()],
        snapshot_lineage: None,
        artifact_refs: Vec::new(),
    })?;
    journal.close()?;
    let json = parsed
        .as_ref()
        .map(|value| sanitize_result_json(value, args.evidence_mode));
    let text = (json.is_none() && !stdout.trim().is_empty())
        .then(|| sanitize_output(stdout.trim(), args.evidence_mode));
    let diagnostic =
        (!stderr.trim().is_empty()).then(|| sanitize_output(stderr.trim(), args.evidence_mode));
    Ok(ScenarioOutcome {
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
        exit_code: if output.timed_out || output.exit_code != 0 || malformed_json {
            ErrorClass::Upstream.exit_code()
        } else {
            0
        },
    })
}

struct StagedScenario(PathBuf);

impl StagedScenario {
    fn create(root: &std::path::Path, raw: &[u8]) -> Result<Self, CliError> {
        let path = root.join(format!(
            ".scenario-input-{}-{}.json",
            std::process::id(),
            crate::test_mode::timestamp()
                .chars()
                .filter(|character| character.is_ascii_alphanumeric())
                .take(24)
                .collect::<String>()
        ));
        write_atomic(&path, raw, SECRET_FILE_MODE)
            .map_err(|_| CliError::journal("failed to stage the reviewed scenario source"))?;
        Ok(Self(path))
    }

    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for StagedScenario {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

pub(crate) fn validate_source(args: &ScenarioArgs) -> Result<Vec<u8>, CliError> {
    let metadata = fs::symlink_metadata(&args.file)
        .map_err(|_| CliError::usage("scenario file is unavailable"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(
            CliError::policy("scenario source must be a regular non-symlink file")
                .with_operation("scenario"),
        );
    }
    if metadata.len() > 1024 * 1024 {
        return Err(CliError::usage(
            "scenario file exceeds the 1 MiB input bound",
        ));
    }
    if args.file.extension().and_then(|value| value.to_str()) != Some("json") {
        return Err(CliError::usage(
            "scenario file must use the `.json` extension",
        ));
    }
    fs::read(&args.file).map_err(|_| CliError::usage("failed to read scenario file"))
}
