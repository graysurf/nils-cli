pub mod json;
pub mod text;

use serde_json::Value;

use crate::cli::OutputFormat;

pub fn emit_success(
    format: OutputFormat,
    schema_version: &str,
    command: &str,
    payload: &Value,
) -> Result<(), String> {
    match format {
        OutputFormat::Text => text::print_success(schema_version, command, payload),
        OutputFormat::Json => json::print_success(schema_version, command, payload),
    }
}

pub fn emit_error(
    format: OutputFormat,
    schema_version: &str,
    command: &str,
    code: &str,
    message: &str,
) -> Result<(), String> {
    match format {
        OutputFormat::Text => text::print_error(schema_version, command, code, message),
        OutputFormat::Json => json::print_error(schema_version, command, code, message),
    }
}
