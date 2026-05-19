use nils_common::cli_contract::{Envelope, EnvelopeError};
use serde::Serialize;

use crate::errors::AppError;

/// Emit a success envelope with the given payload.
pub fn emit_data<T>(schema_version: &str, data: T) -> Result<(), AppError>
where
    T: Serialize,
{
    let envelope = Envelope::success(schema_version, data);
    print_envelope(&envelope)
}

/// Emit a success envelope with the given payload and warnings.
pub fn emit_data_with_warnings<T>(
    schema_version: &str,
    data: T,
    warnings: Vec<String>,
) -> Result<(), AppError>
where
    T: Serialize,
{
    let envelope = Envelope::success(schema_version, data).with_warnings(warnings);
    print_envelope(&envelope)
}

/// Emit a failure envelope built from an `AppError`.
pub fn emit_json_error(schema_version: &str, err: &AppError) -> Result<(), AppError> {
    let mut envelope_error = EnvelopeError::new(err.code(), err.message());
    if let Some(details) = err.details() {
        envelope_error = envelope_error.with_details(details.clone());
    }
    let envelope: Envelope<()> = Envelope::failure(schema_version, envelope_error);
    print_envelope(&envelope)
}

fn print_envelope<T>(envelope: &Envelope<T>) -> Result<(), AppError>
where
    T: Serialize,
{
    let encoded = serde_json::to_string(envelope).map_err(|err| {
        AppError::runtime(format!("failed to serialize JSON output: {err}"))
            .with_code("internal-error")
    })?;
    println!("{encoded}");
    Ok(())
}
