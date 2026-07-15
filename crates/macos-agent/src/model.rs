use serde::Serialize;
use serde_json::Value;

use crate::cli::{EvidenceMode, RuntimeMode};
use crate::error::CliError;

pub const ADAPTER_SCHEMA: &str = "macos-agent.adapter.v2";
pub const ERROR_SCHEMA: &str = "macos-agent.error.v2";

#[derive(Debug, Clone, Serialize)]
pub struct SuccessEnvelope<T>
where
    T: Serialize,
{
    pub schema_version: &'static str,
    pub ok: bool,
    pub command: &'static str,
    pub result: T,
}

impl<T> SuccessEnvelope<T>
where
    T: Serialize,
{
    pub fn new(command: &'static str, result: T) -> Self {
        Self {
            schema_version: ADAPTER_SCHEMA,
            ok: true,
            command,
            result,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ErrorEnvelope {
    pub schema_version: &'static str,
    pub ok: bool,
    pub error: ErrorResult,
}

impl ErrorEnvelope {
    pub fn from_error(error: &CliError) -> Self {
        Self {
            schema_version: ERROR_SCHEMA,
            ok: false,
            error: ErrorResult {
                class: error.class().as_str(),
                exit_code: error.exit_code(),
                operation: error.operation().map(str::to_string),
                message: error.message().to_string(),
                hints: error.hints().to_vec(),
            },
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ErrorResult {
    pub class: &'static str,
    pub exit_code: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation: Option<String>,
    pub message: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hints: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UpstreamResult {
    pub exit_code: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signal: Option<i32>,
    pub timed_out: bool,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub json: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostic: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExecutionResult {
    pub transport: &'static str,
    pub runtime: RuntimeMode,
    pub evidence_mode: EvidenceMode,
    pub journal_step: String,
    pub upstream: UpstreamResult,
}

pub fn emit_json<T: Serialize>(command: &'static str, result: T) -> Result<(), CliError> {
    let body = serde_json::to_string(&SuccessEnvelope::new(command, result)).map_err(|error| {
        CliError::upstream(format!("failed to encode JSON response: {error}"))
            .with_operation("response.encode")
    })?;
    println!("{body}");
    Ok(())
}
