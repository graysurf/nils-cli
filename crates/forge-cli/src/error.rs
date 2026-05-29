//! Typed error returned by every op. Each variant carries the discriminator
//! that goes into `data.error.kind` plus the BSD sysexits constant it maps to.
//!
//! Spec: `crates/forge-cli/docs/specs/forge-cli-spec-v1.md` §"Exit code map" + §"Lock-down
//! policy". The numeric exit values come from `nils_common::cli_contract::exit`
//! and are never inlined as integer literals.

use nils_common::cli_contract::{Envelope, EnvelopeError, OutputFormat, exit};
use serde::Serialize;
use thiserror::Error;

use crate::cli::BINARY;

/// Top-level forge-cli error type. Every leaf knows its `error.kind` and
/// exit-code class.
#[derive(Debug, Error)]
pub enum ForgeError {
    /// Subcommand has not been implemented in this sprint.
    #[error("{message}")]
    NotImplemented {
        schema_version: String,
        message: String,
    },
    /// Backend binary (`gh` or `glab`) is missing or unauthenticated.
    #[error("{message}")]
    BackendUnavailable {
        schema_version: String,
        kind: &'static str,
        message: String,
        detail: Option<String>,
    },
    /// Backend exited with a non-zero status while attempting a remote
    /// operation.
    #[error("{message}")]
    BackendError {
        schema_version: String,
        message: String,
        detail: Option<String>,
    },
    /// Provider could not be resolved (unknown host or no git remote).
    #[error("{message}")]
    ProviderUnsupported {
        schema_version: String,
        message: String,
        detail: Option<String>,
    },
    /// Internal invariant violation — backend JSON did not match the expected
    /// shape, or another assumption that should hold blew up.
    #[error("{message}")]
    SoftwareError {
        schema_version: String,
        message: String,
        detail: Option<String>,
    },
    /// Lock-down policy violation (branch / title / body / worktree / push
    /// state). Maps to `DATA 65` with the rule-specific `error.kind`
    /// discriminator declared in spec §"Lock-down policy".
    #[error("{message}")]
    Validation {
        schema_version: String,
        kind: &'static str,
        message: String,
        detail: Option<String>,
    },
    /// Op-specific `RUNTIME 1` failure with a rule-specific `error.kind`.
    /// Currently used by `pr wait-checks` for `checks_failed`; future ops
    /// (e.g. `pr merge` conflict) can reuse the variant.
    #[error("{message}")]
    RuntimeFailure {
        schema_version: String,
        kind: &'static str,
        message: String,
        detail: Option<String>,
    },
}

impl ForgeError {
    /// Build a `not_implemented` error.
    pub fn not_implemented(schema_version: impl Into<String>, message: impl Into<String>) -> Self {
        Self::NotImplemented {
            schema_version: schema_version.into(),
            message: message.into(),
        }
    }

    /// Build a `backend_missing` error (`UNAVAILABLE 69`).
    pub fn backend_missing(
        schema_version: impl Into<String>,
        message: impl Into<String>,
        detail: Option<String>,
    ) -> Self {
        Self::BackendUnavailable {
            schema_version: schema_version.into(),
            kind: "backend_missing",
            message: message.into(),
            detail,
        }
    }

    /// Build a `backend_unauthenticated` error (`UNAVAILABLE 69`).
    pub fn backend_unauthenticated(
        schema_version: impl Into<String>,
        message: impl Into<String>,
        detail: Option<String>,
    ) -> Self {
        Self::BackendUnavailable {
            schema_version: schema_version.into(),
            kind: "backend_unauthenticated",
            message: message.into(),
            detail,
        }
    }

    /// Build an `UNAVAILABLE 69` error with a custom rule-specific kind.
    /// Used for op-specific unavailability (e.g. `checks_timeout`,
    /// `glab_version_unsupported`) that share exit code 69 but need their
    /// own `error.kind` discriminator per spec.
    pub fn unavailable(
        schema_version: impl Into<String>,
        kind: &'static str,
        message: impl Into<String>,
        detail: Option<String>,
    ) -> Self {
        Self::BackendUnavailable {
            schema_version: schema_version.into(),
            kind,
            message: message.into(),
            detail,
        }
    }

    /// Build a `RUNTIME 1` failure with a rule-specific kind (e.g.
    /// `checks_failed`).
    pub fn runtime_failure(
        schema_version: impl Into<String>,
        kind: &'static str,
        message: impl Into<String>,
        detail: Option<String>,
    ) -> Self {
        Self::RuntimeFailure {
            schema_version: schema_version.into(),
            kind,
            message: message.into(),
            detail,
        }
    }

    /// Build a `backend_error` error (`RUNTIME 1`).
    pub fn backend_error(
        schema_version: impl Into<String>,
        message: impl Into<String>,
        detail: Option<String>,
    ) -> Self {
        Self::BackendError {
            schema_version: schema_version.into(),
            message: message.into(),
            detail,
        }
    }

    /// Build a `provider_unsupported` error (`USAGE 64`).
    pub fn provider_unsupported(
        schema_version: impl Into<String>,
        message: impl Into<String>,
        detail: Option<String>,
    ) -> Self {
        Self::ProviderUnsupported {
            schema_version: schema_version.into(),
            message: message.into(),
            detail,
        }
    }

    /// Build a `SOFTWARE 70` error for invariant violations.
    pub fn software(
        schema_version: impl Into<String>,
        message: impl Into<String>,
        detail: Option<String>,
    ) -> Self {
        Self::SoftwareError {
            schema_version: schema_version.into(),
            message: message.into(),
            detail,
        }
    }

    /// Build a `DATA 65` validation error with the given rule-specific kind.
    /// The `kind` literal MUST match one of the entries in spec §"Lock-down
    /// policy" so callers can branch on `error.kind`.
    pub fn validation(
        schema_version: impl Into<String>,
        kind: &'static str,
        message: impl Into<String>,
        detail: Option<String>,
    ) -> Self {
        Self::Validation {
            schema_version: schema_version.into(),
            kind,
            message: message.into(),
            detail,
        }
    }

    /// Map the error to its exit-code constant.
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::NotImplemented { .. } => exit::SOFTWARE,
            Self::BackendUnavailable { .. } => exit::UNAVAILABLE,
            Self::BackendError { .. } => exit::RUNTIME,
            Self::ProviderUnsupported { .. } => exit::USAGE,
            Self::SoftwareError { .. } => exit::SOFTWARE,
            Self::Validation { .. } => exit::DATA,
            Self::RuntimeFailure { .. } => exit::RUNTIME,
        }
    }

    /// Return the `error.kind` discriminator.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::NotImplemented { .. } => "not_implemented",
            Self::BackendUnavailable { kind, .. } => kind,
            Self::BackendError { .. } => "backend_error",
            Self::ProviderUnsupported { .. } => "provider_unsupported",
            Self::SoftwareError { .. } => "software_error",
            Self::Validation { kind, .. } => kind,
            Self::RuntimeFailure { kind, .. } => kind,
        }
    }

    fn schema_version(&self) -> &str {
        match self {
            Self::NotImplemented { schema_version, .. }
            | Self::BackendUnavailable { schema_version, .. }
            | Self::BackendError { schema_version, .. }
            | Self::ProviderUnsupported { schema_version, .. }
            | Self::SoftwareError { schema_version, .. }
            | Self::Validation { schema_version, .. }
            | Self::RuntimeFailure { schema_version, .. } => schema_version,
        }
    }

    /// Return the human-readable error message.
    pub fn message(&self) -> &str {
        match self {
            Self::NotImplemented { message, .. }
            | Self::BackendUnavailable { message, .. }
            | Self::BackendError { message, .. }
            | Self::ProviderUnsupported { message, .. }
            | Self::SoftwareError { message, .. }
            | Self::Validation { message, .. }
            | Self::RuntimeFailure { message, .. } => message,
        }
    }

    /// Return the optional structured detail string.
    pub fn detail(&self) -> Option<&str> {
        match self {
            Self::NotImplemented { .. } => None,
            Self::BackendUnavailable { detail, .. }
            | Self::BackendError { detail, .. }
            | Self::ProviderUnsupported { detail, .. }
            | Self::SoftwareError { detail, .. }
            | Self::Validation { detail, .. }
            | Self::RuntimeFailure { detail, .. } => detail.as_deref(),
        }
    }

    /// Build the JSON envelope wire string the error would produce. Pulled out
    /// of [`emit`] so unit tests can lock the parity-gated shape without
    /// capturing stdout.
    fn render_json(&self) -> String {
        let envelope_error = EnvelopeError::new(self.kind(), self.message());
        let envelope_error = match self.detail() {
            Some(detail) => envelope_error.with_details(serde_json::json!({ "detail": detail })),
            None => envelope_error,
        };
        let envelope: Envelope<EnvelopeStub> =
            Envelope::failure(self.schema_version().to_string(), envelope_error);
        serde_json::to_string(&envelope).unwrap_or_else(|_| String::from("{\"ok\":false}"))
    }

    /// Build the text envelope wire string the error would produce.
    fn render_text(&self) -> String {
        let kind = self.kind();
        let detail_suffix = self
            .detail()
            .map(|d| format!("\n  detail: {d}"))
            .unwrap_or_default();
        format!(
            "error: {kind}: {message}{detail_suffix}",
            message = self.message()
        )
    }

    /// Render the error through the workspace envelope and return the
    /// exit-code constant.
    pub fn emit(&self, format: OutputFormat) -> i32 {
        let code = self.exit_code();
        match format {
            OutputFormat::Json => {
                println!("{}", self.render_json());
            }
            OutputFormat::Text => {
                eprintln!("{}", self.render_text());
            }
        }
        let _ = BINARY; // silence unused-import lint when this module is consumed in isolation
        code
    }
}

/// Placeholder envelope payload for error-only emissions.
#[derive(Serialize)]
struct EnvelopeStub {}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn exit_code_mapping_matches_spec() {
        let cases = [
            (
                ForgeError::not_implemented("cli.forge-cli.error.v1", "x"),
                exit::SOFTWARE,
            ),
            (
                ForgeError::backend_missing("cli.forge-cli.error.v1", "x", None),
                exit::UNAVAILABLE,
            ),
            (
                ForgeError::backend_unauthenticated("cli.forge-cli.error.v1", "x", None),
                exit::UNAVAILABLE,
            ),
            (
                ForgeError::backend_error("cli.forge-cli.error.v1", "x", None),
                exit::RUNTIME,
            ),
            (
                ForgeError::provider_unsupported("cli.forge-cli.error.v1", "x", None),
                exit::USAGE,
            ),
            (
                ForgeError::software("cli.forge-cli.error.v1", "x", None),
                exit::SOFTWARE,
            ),
            (
                ForgeError::validation("cli.forge-cli.error.v1", "branch_name_invalid", "x", None),
                exit::DATA,
            ),
            (
                ForgeError::unavailable("cli.forge-cli.error.v1", "checks_timeout", "x", None),
                exit::UNAVAILABLE,
            ),
            (
                ForgeError::runtime_failure("cli.forge-cli.error.v1", "checks_failed", "x", None),
                exit::RUNTIME,
            ),
        ];
        for (err, expected) in cases {
            assert_eq!(err.exit_code(), expected, "{}", err.kind());
        }
    }

    #[test]
    fn json_envelope_carries_schema_ok_false_and_error_block() {
        let err = ForgeError::provider_unsupported(
            "cli.forge-cli.error.v1",
            "unsupported forge host: bitbucket.org",
            Some("remote_url=https://bitbucket.org/o/r.git".into()),
        );
        assert_eq!(
            err.render_json(),
            "{\"schema_version\":\"cli.forge-cli.error.v1\",\"ok\":false,\"error\":{\"code\":\"provider_unsupported\",\"message\":\"unsupported forge host: bitbucket.org\",\"details\":{\"detail\":\"remote_url=https://bitbucket.org/o/r.git\"}}}"
        );
    }

    #[test]
    fn json_envelope_omits_details_when_no_detail() {
        let err = ForgeError::backend_missing("cli.forge-cli.error.v1", "gh not installed", None);
        assert_eq!(
            err.render_json(),
            "{\"schema_version\":\"cli.forge-cli.error.v1\",\"ok\":false,\"error\":{\"code\":\"backend_missing\",\"message\":\"gh not installed\"}}"
        );
    }

    #[test]
    fn text_envelope_renders_kind_message_and_optional_detail() {
        let err = ForgeError::provider_unsupported(
            "cli.forge-cli.error.v1",
            "unsupported forge host: bitbucket.org",
            Some("remote_url=https://bitbucket.org/o/r.git".into()),
        );
        assert_eq!(
            err.render_text(),
            "error: provider_unsupported: unsupported forge host: bitbucket.org\n  detail: remote_url=https://bitbucket.org/o/r.git"
        );

        let err = ForgeError::backend_missing("cli.forge-cli.error.v1", "gh not installed", None);
        assert_eq!(
            err.render_text(),
            "error: backend_missing: gh not installed"
        );
    }

    #[test]
    fn kind_discriminators_match_spec() {
        assert_eq!(
            ForgeError::backend_missing("v1", "x", None).kind(),
            "backend_missing"
        );
        assert_eq!(
            ForgeError::backend_unauthenticated("v1", "x", None).kind(),
            "backend_unauthenticated"
        );
        assert_eq!(
            ForgeError::backend_error("v1", "x", None).kind(),
            "backend_error"
        );
        assert_eq!(
            ForgeError::provider_unsupported("v1", "x", None).kind(),
            "provider_unsupported"
        );
        assert_eq!(
            ForgeError::software("v1", "x", None).kind(),
            "software_error"
        );
        assert_eq!(
            ForgeError::not_implemented("v1", "x").kind(),
            "not_implemented"
        );
        assert_eq!(
            ForgeError::unavailable("v1", "checks_timeout", "x", None).kind(),
            "checks_timeout"
        );
        assert_eq!(
            ForgeError::unavailable("v1", "glab_version_unsupported", "x", None).kind(),
            "glab_version_unsupported"
        );
        assert_eq!(
            ForgeError::runtime_failure("v1", "checks_failed", "x", None).kind(),
            "checks_failed"
        );
    }
}
