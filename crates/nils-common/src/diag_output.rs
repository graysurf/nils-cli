//! Shared JSON envelope helpers for CLI diagnostic / structured output.
//!
//! Caller crates re-export the surface (typically via `pub use nils_common::diag_output::*;`
//! in a crate-local `diag_output` module) so existing `crate::diag_output::*` paths keep
//! working without touching call sites.

use anyhow::Result;
use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, Serialize)]
pub struct ErrorEnvelope {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct JsonEnvelopeResult<T: Serialize> {
    pub schema_version: String,
    pub command: String,
    pub ok: bool,
    pub result: T,
}

#[derive(Debug, Clone, Serialize)]
pub struct JsonEnvelopeResults<T: Serialize> {
    pub schema_version: String,
    pub command: String,
    pub ok: bool,
    pub results: Vec<T>,
}

#[derive(Debug, Clone, Serialize)]
pub struct JsonEnvelopeError {
    pub schema_version: String,
    pub command: String,
    pub ok: bool,
    pub error: ErrorEnvelope,
}

pub fn emit_json<T: Serialize>(payload: &T) -> Result<()> {
    println!("{}", serde_json::to_string(payload)?);
    Ok(())
}

pub fn emit_success_result<T: Serialize>(
    schema_version: &str,
    command: &str,
    result: T,
) -> Result<()> {
    emit_json(&JsonEnvelopeResult {
        schema_version: schema_version.to_string(),
        command: command.to_string(),
        ok: true,
        result,
    })
}

pub fn emit_success_results<T: Serialize>(
    schema_version: &str,
    command: &str,
    results: Vec<T>,
) -> Result<()> {
    emit_json(&JsonEnvelopeResults {
        schema_version: schema_version.to_string(),
        command: command.to_string(),
        ok: true,
        results,
    })
}

pub fn emit_error(
    schema_version: &str,
    command: &str,
    code: &str,
    message: impl Into<String>,
    details: Option<Value>,
) -> Result<()> {
    emit_json(&JsonEnvelopeError {
        schema_version: schema_version.to_string(),
        command: command.to_string(),
        ok: false,
        error: ErrorEnvelope {
            code: code.to_string(),
            message: message.into(),
            details,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, to_value};

    const TEST_SCHEMA: &str = "nils-common.diag-output.test.v1";

    #[test]
    fn error_envelope_serialization_omits_details_when_none() {
        let envelope = JsonEnvelopeError {
            schema_version: TEST_SCHEMA.to_string(),
            command: "diag test".to_string(),
            ok: false,
            error: ErrorEnvelope {
                code: "bad-input".to_string(),
                message: "invalid".to_string(),
                details: None,
            },
        };
        let value = to_value(envelope).expect("serialize");
        assert_eq!(value["ok"], false);
        assert!(value["error"].get("details").is_none());
    }

    #[test]
    fn error_envelope_serialization_emits_details_when_present() {
        let envelope = JsonEnvelopeError {
            schema_version: TEST_SCHEMA.to_string(),
            command: "diag test".to_string(),
            ok: false,
            error: ErrorEnvelope {
                code: "bad-input".to_string(),
                message: "invalid".to_string(),
                details: Some(json!({"hint": "retry"})),
            },
        };
        let value = to_value(envelope).expect("serialize");
        assert_eq!(value["error"]["details"], json!({"hint": "retry"}));
    }

    #[test]
    fn success_result_serialization_round_trip() {
        let payload = JsonEnvelopeResult {
            schema_version: TEST_SCHEMA.to_string(),
            command: "diag list".to_string(),
            ok: true,
            result: json!({"status": "ok"}),
        };
        let value = to_value(&payload).expect("serialize");
        assert_eq!(value["schema_version"], TEST_SCHEMA);
        assert_eq!(value["command"], "diag list");
        assert_eq!(value["ok"], true);
        assert_eq!(value["result"]["status"], "ok");
    }

    #[test]
    fn success_results_serialization_preserves_vec_order() {
        let payload = JsonEnvelopeResults {
            schema_version: TEST_SCHEMA.to_string(),
            command: "diag list".to_string(),
            ok: true,
            results: vec![json!({"item": 1}), json!({"item": 2})],
        };
        let value = to_value(&payload).expect("serialize");
        assert_eq!(value["results"][0]["item"], 1);
        assert_eq!(value["results"][1]["item"], 2);
    }

    #[test]
    fn emit_helpers_return_ok() {
        assert!(emit_success_result(TEST_SCHEMA, "diag test", json!({"status": "ok"})).is_ok());
        assert!(
            emit_success_results(
                TEST_SCHEMA,
                "diag test",
                vec![json!({"item": 1}), json!({"item": 2})],
            )
            .is_ok()
        );
        assert!(
            emit_error(
                TEST_SCHEMA,
                "diag test",
                "failure",
                "boom",
                Some(json!({"hint": "retry"})),
            )
            .is_ok()
        );
        assert!(emit_error(TEST_SCHEMA, "diag test", "failure", "boom", None).is_ok());
    }
}
