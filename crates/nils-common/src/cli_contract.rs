//! Workspace-wide CLI output contract primitives.
//!
//! Every binary in the `nils-cli` workspace renders machine-readable output
//! through the [`Envelope`] type and signals failure through the BSD sysexits
//! constants in the [`exit`] module. The durable spec lives at
//! `docs/specs/cli-output-contract-v1.md`; `crates/cli-template` is the
//! reference implementation.
//!
//! The crate-level boundary rule (see `crates/nils-common/README.md`) still
//! applies — these primitives expose structured data and constants; user-facing
//! warning/error text and exit-code mapping live in caller adapters.

use std::io::{self, Write};

use serde::{Deserialize, Serialize};

/// Canonical output-format flag value for every workspace CLI.
///
/// Binaries surface this enum via `clap`'s `value_enum`, typically as
/// `--format text|json`. Pre-contract `--json` boolean flags may remain as
/// hidden aliases for one minor cycle (see the contract spec).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
#[clap(rename_all = "lower")]
pub enum OutputFormat {
    /// Human-readable text output (default).
    #[default]
    Text,
    /// Single-record JSON envelope (snake_case).
    Json,
}

impl OutputFormat {
    /// Returns `true` when the caller asked for machine-readable JSON.
    pub fn is_json(self) -> bool {
        matches!(self, Self::Json)
    }

    /// Returns `true` when the caller is rendering text.
    pub fn is_text(self) -> bool {
        matches!(self, Self::Text)
    }
}

/// Envelope shared by every JSON-emitting subcommand.
///
/// The shape is intentionally narrow: `schema_version` pins the wire contract,
/// `ok` is a boolean success flag, `data` carries the per-subcommand payload,
/// `warnings` collects non-fatal diagnostics (so JSON consumers see what text
/// mode would print to stderr), and `error` carries a structured failure.
/// Deserialization accepts additive fields so same-version producers can add
/// metadata without breaking consumers; callers still validate the required
/// schema version, success state, and command-specific payload fields.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Envelope<T: Serialize> {
    pub schema_version: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<T>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub warnings: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<EnvelopeError>,
}

impl<T: Serialize> Envelope<T> {
    /// Build a successful envelope.
    pub fn success(schema_version: impl Into<String>, data: T) -> Self {
        Self {
            schema_version: schema_version.into(),
            ok: true,
            data: Some(data),
            warnings: Vec::new(),
            error: None,
        }
    }

    /// Build a failure envelope with no payload.
    pub fn failure(schema_version: impl Into<String>, error: EnvelopeError) -> Self {
        Self {
            schema_version: schema_version.into(),
            ok: false,
            data: None,
            warnings: Vec::new(),
            error: Some(error),
        }
    }

    /// Append a single warning to the envelope.
    pub fn with_warning(mut self, warning: impl Into<String>) -> Self {
        self.warnings.push(warning.into());
        self
    }

    /// Append multiple warnings to the envelope.
    pub fn with_warnings<I, S>(mut self, warnings: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.warnings.extend(warnings.into_iter().map(|w| w.into()));
        self
    }
}

/// Structured error rendered inside the JSON envelope's `error` field.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct EnvelopeError {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

impl EnvelopeError {
    /// Build an error with a code and message.
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            hint: None,
            details: None,
        }
    }

    /// Attach an optional human-readable hint to the error.
    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    /// Attach optional machine-readable structured detail to the error (e.g. the offending payload path).
    pub fn with_details(mut self, details: serde_json::Value) -> Self {
        self.details = Some(details);
        self
    }
}

/// Build the canonical `cli.<binary>.<command>.v<N>` schema-version string.
pub fn schema_version_for(binary: &str, command: &str, version: u32) -> String {
    format!("cli.{binary}.{command}.v{version}")
}

/// BSD sysexits-aligned exit-code constants used by every workspace binary.
///
/// The full table is captured in `docs/specs/cli-output-contract-v1.md`.
pub mod exit {
    /// Successful termination.
    pub const SUCCESS: i32 = 0;
    /// Generic runtime error (the historic catch-all for "something went wrong at runtime").
    pub const RUNTIME: i32 = 1;
    /// `EX_USAGE` — command-line syntax error.
    pub const USAGE: i32 = 64;
    /// `EX_DATAERR` — input data is malformed or otherwise invalid.
    pub const DATA: i32 = 65;
    /// `EX_UNAVAILABLE` — a required service or resource is unavailable.
    pub const UNAVAILABLE: i32 = 69;
    /// `EX_SOFTWARE` — internal software error (an invariant was violated).
    pub const SOFTWARE: i32 = 70;
}

/// Emit a parse-error / unknown-subcommand failure through the shared contract.
///
/// When `format` is [`OutputFormat::Json`] the helper writes a single-line JSON
/// envelope (schema `cli.<binary>.error.v1`) to stdout. In text mode it writes
/// the historical `error: <msg>` line to stderr. Both branches return
/// [`exit::USAGE`] so callers can do `std::process::exit(emit_parse_error(...))`.
pub fn emit_parse_error(binary: &str, format: OutputFormat, code: &str, message: &str) -> i32 {
    emit_parse_error_to(
        &mut io::stdout().lock(),
        &mut io::stderr().lock(),
        binary,
        format,
        code,
        message,
    )
}

/// Test-friendly variant of [`emit_parse_error`] that writes to caller-provided sinks.
pub fn emit_parse_error_to<W1: Write, W2: Write>(
    stdout: &mut W1,
    stderr: &mut W2,
    binary: &str,
    format: OutputFormat,
    code: &str,
    message: &str,
) -> i32 {
    match format {
        OutputFormat::Json => {
            let envelope: Envelope<()> = Envelope::failure(
                schema_version_for(binary, "error", 1),
                EnvelopeError::new(code, message),
            );
            // Single-line JSON so log scrapers see one record per error.
            let serialized =
                serde_json::to_string(&envelope).unwrap_or_else(|_| String::from("{\"ok\":false}"));
            let _ = writeln!(stdout, "{serialized}");
        }
        OutputFormat::Text => {
            let _ = writeln!(stderr, "error: {message}");
        }
    }
    exit::USAGE
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::ValueEnum;
    use pretty_assertions::assert_eq;

    #[test]
    fn output_format_round_trips_through_value_enum() {
        let text = OutputFormat::from_str("text", false).expect("text variant");
        let json = OutputFormat::from_str("json", false).expect("json variant");
        assert_eq!(text, OutputFormat::Text);
        assert_eq!(json, OutputFormat::Json);
        assert!(json.is_json());
        assert!(text.is_text());
        assert_eq!(OutputFormat::default(), OutputFormat::Text);
    }

    #[test]
    fn envelope_success_serializes_snake_case() {
        #[derive(Serialize)]
        struct Payload {
            item_count: u32,
        }
        let envelope = Envelope::success(
            schema_version_for("cli-template", "status", 1),
            Payload { item_count: 3 },
        );
        let json = serde_json::to_string(&envelope).expect("serialize envelope");
        assert_eq!(
            json,
            "{\"schema_version\":\"cli.cli-template.status.v1\",\"ok\":true,\"data\":{\"item_count\":3}}"
        );
    }

    #[test]
    fn envelope_success_includes_warnings_when_present() {
        let envelope: Envelope<()> = Envelope {
            schema_version: schema_version_for("memo", "apply", 1),
            ok: true,
            data: None,
            warnings: Vec::new(),
            error: None,
        }
        .with_warning("entry-42 skipped: missing body");
        let json = serde_json::to_string(&envelope).expect("serialize envelope");
        assert_eq!(
            json,
            "{\"schema_version\":\"cli.memo.apply.v1\",\"ok\":true,\"warnings\":[\"entry-42 skipped: missing body\"]}"
        );
    }

    #[test]
    fn envelope_failure_serializes_error_only() {
        let envelope: Envelope<()> = Envelope::failure(
            schema_version_for("cli-template", "error", 1),
            EnvelopeError::new("parse-error", "missing required argument <name>")
                .with_hint("see --help"),
        );
        let json = serde_json::to_string(&envelope).expect("serialize envelope");
        assert_eq!(
            json,
            "{\"schema_version\":\"cli.cli-template.error.v1\",\"ok\":false,\"error\":{\"code\":\"parse-error\",\"message\":\"missing required argument <name>\",\"hint\":\"see --help\"}}"
        );
    }

    #[test]
    fn envelope_deserialization_accepts_additive_metadata() {
        let envelope: Envelope<serde_json::Value> = serde_json::from_str(
            r#"{
                "schema_version":"cli.agent-hook.setup.v1",
                "ok":true,
                "data":{"product":"codex","future_result_metadata":true},
                "warnings":[],
                "error":null,
                "future_envelope_metadata":{"source":"newer-producer"}
            }"#,
        )
        .expect("same-version additive metadata remains compatible");

        assert!(envelope.ok);
        assert_eq!(envelope.data.expect("data")["product"], "codex");
    }

    #[test]
    fn exit_constants_match_bsd_sysexits() {
        assert_eq!(exit::SUCCESS, 0);
        assert_eq!(exit::RUNTIME, 1);
        assert_eq!(exit::USAGE, 64);
        assert_eq!(exit::DATA, 65);
        assert_eq!(exit::UNAVAILABLE, 69);
        assert_eq!(exit::SOFTWARE, 70);
    }

    #[test]
    fn schema_version_for_builds_canonical_string() {
        assert_eq!(schema_version_for("memo", "list", 1), "cli.memo.list.v1");
        assert_eq!(
            schema_version_for("cli-template", "status", 2),
            "cli.cli-template.status.v2"
        );
    }
}
