use std::collections::BTreeMap;
use std::process::{Command, Stdio};
use std::time::Duration;

use nils_common::cli_contract::exit;
use nils_common::process as shared_process;
use serde_json::json;

use crate::agent::commit::{
    COMMIT_REQUIRED_CAPABILITIES, SEMANTIC_COMMIT_HELP_OPTIONS, STAGED_CONTEXT_HELP_OPTIONS,
    doctor_capabilities, required_capabilities,
};
use crate::agent::oneshot::{
    capability_report, claude_binary, clear_wrapper_environment, help_has_option,
};
use crate::process::{ProcessOutputError, output_with_limits_retry_io};
use crate::wrapper_config::{resolve_effort, resolve_model};

const DOCTOR_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_DOCTOR_OUTPUT_BYTES: usize = 256 * 1024;
const DEPENDENCY_PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_DEPENDENCY_HELP_BYTES: usize = 128 * 1024;

pub fn run(json_output: bool) -> i32 {
    let binary = claude_binary();
    let claude_available = shared_process::find_in_path(&binary).is_some();
    let git_available = shared_process::find_in_path("git").is_some();
    let semantic_commit_available = shared_process::find_in_path("semantic-commit").is_some();
    let semantic_commit_compatible =
        semantic_commit_available && semantic_commit_contract_available();
    let capabilities = doctor_capabilities();
    let flags = if claude_available {
        capability_report(&binary, &capabilities).unwrap_or_else(|_| {
            capabilities
                .iter()
                .map(|flag| ((*flag).to_string(), false))
                .collect()
        })
    } else {
        capabilities
            .iter()
            .map(|flag| ((*flag).to_string(), false))
            .collect::<BTreeMap<_, _>>()
    };
    let commit_profile = COMMIT_REQUIRED_CAPABILITIES
        .iter()
        .all(|flag| flags.get(*flag).copied().unwrap_or(false));
    let configured_capabilities = match (resolve_model(None), resolve_effort(None)) {
        (Ok(model), Ok(effort)) => Some(required_capabilities(model.is_some(), effort.is_some())),
        _ => None,
    };
    let configured_commit_profile = configured_capabilities.is_some_and(|required| {
        required
            .iter()
            .all(|flag| flags.get(*flag).copied().unwrap_or(false))
    });
    let upstream_doctor_status = if claude_available {
        upstream_doctor_status(&binary)
    } else {
        UpstreamDoctorStatus::LaunchFailed
    };
    let upstream_doctor = upstream_doctor_status == UpstreamDoctorStatus::Ready;
    let ready = claude_available
        && git_available
        && semantic_commit_compatible
        && configured_commit_profile
        && upstream_doctor;

    if json_output {
        let payload = json!({
            "schema_version": "claude-cli.agent.doctor.v1",
            "command": "agent doctor",
            "ok": true,
            "result": {
                "ready": ready,
                "commit_profile": commit_profile,
                "configured_commit_profile": configured_commit_profile,
                "upstream_doctor": upstream_doctor,
                "upstream_doctor_status": upstream_doctor_status.as_str(),
                "dependencies": {
                    "claude": claude_available,
                    "git": git_available,
                    "semantic_commit": semantic_commit_available,
                    "semantic_commit_compatible": semantic_commit_compatible
                },
                "flags": flags
            }
        });
        println!(
            "{}",
            serde_json::to_string(&payload).expect("agent doctor JSON serialization")
        );
    } else if ready {
        println!("claude-cli agent runtime: ready");
    } else {
        println!("claude-cli agent runtime: unavailable");
    }

    if ready { exit::SUCCESS } else { exit::RUNTIME }
}

fn semantic_commit_contract_available() -> bool {
    help_contract_available(&["staged-context", "--help"], &STAGED_CONTEXT_HELP_OPTIONS)
        && help_contract_available(&["commit", "--help"], &SEMANTIC_COMMIT_HELP_OPTIONS)
}

fn help_contract_available(args: &[&str], required: &[&str]) -> bool {
    let mut command = Command::new("semantic-commit");
    command.args(args).stdin(Stdio::null());
    let Ok(output) = output_with_limits_retry_io(
        &mut command,
        DEPENDENCY_PROBE_TIMEOUT,
        MAX_DEPENDENCY_HELP_BYTES,
        3,
    ) else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    let help = String::from_utf8_lossy(&output.stdout);
    required.iter().all(|option| help_has_option(&help, option))
}

fn upstream_doctor_status(binary: &str) -> UpstreamDoctorStatus {
    let mut command = Command::new(binary);
    command.arg("doctor").stdin(Stdio::null());
    clear_wrapper_environment(&mut command);
    match output_with_limits_retry_io(&mut command, DOCTOR_TIMEOUT, MAX_DOCTOR_OUTPUT_BYTES, 3) {
        Ok(output) if output.status.success() => UpstreamDoctorStatus::Ready,
        Ok(_) => UpstreamDoctorStatus::Failed,
        Err(ProcessOutputError::Timeout) => UpstreamDoctorStatus::Timeout,
        Err(ProcessOutputError::OutputLimit) => UpstreamDoctorStatus::OutputTooLarge,
        Err(ProcessOutputError::Io(_)) => UpstreamDoctorStatus::LaunchFailed,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum UpstreamDoctorStatus {
    Ready,
    Failed,
    Timeout,
    OutputTooLarge,
    LaunchFailed,
}

impl UpstreamDoctorStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Failed => "failed",
            Self::Timeout => "timeout",
            Self::OutputTooLarge => "output-too-large",
            Self::LaunchFailed => "launch-failed",
        }
    }
}
