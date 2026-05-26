//! Schema validators backing the `plan-archive validate-*` subcommands.

pub mod hosts;
pub mod local;
pub mod metadata;

use serde::Serialize;

/// Free-form warning attached to a successful validation envelope.
///
/// Validators emit warnings (rather than errors) when the input parses and
/// matches required fields but has surprising shape — for example, the
/// pre-classification `metadata.yaml` that omits the captured host
/// classification field.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ValidationWarning {
    /// Stable identifier so JSON consumers can branch on the warning.
    pub code: String,
    /// Human-readable explanation suitable for `stderr` rendering.
    pub message: String,
}

impl ValidationWarning {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}
