//! Thin helpers for emitting the canonical workspace envelope from op
//! handlers. The serde types live in `nils_common::cli_contract`; this module
//! adds the text-mode renderer (one-liner per op) and the success-emission
//! glue.

use std::io::{self, Write};

use nils_common::cli_contract::{Envelope, OutputFormat, exit};
use serde::Serialize;

/// Emit a success envelope. JSON branch writes to stdout, text branch lets the
/// caller decide via the `render_text` closure.
pub fn emit_success<T, F>(
    schema_version: impl Into<String>,
    data: T,
    format: OutputFormat,
    render_text: F,
) -> i32
where
    T: Serialize,
    F: FnOnce(&T),
{
    emit_success_to(
        &mut io::stdout().lock(),
        schema_version,
        data,
        format,
        render_text,
    )
}

/// Test-friendly variant of [`emit_success`] that writes JSON to an injected
/// sink. Text rendering goes through the caller's closure regardless.
pub fn emit_success_to<W, T, F>(
    json_sink: &mut W,
    schema_version: impl Into<String>,
    data: T,
    format: OutputFormat,
    render_text: F,
) -> i32
where
    W: Write,
    T: Serialize,
    F: FnOnce(&T),
{
    let envelope = Envelope::success(schema_version, data);
    match format {
        OutputFormat::Json => {
            match serde_json::to_string(&envelope) {
                Ok(serialized) => {
                    let _ = writeln!(json_sink, "{serialized}");
                    exit::SUCCESS
                }
                Err(_) => exit::SOFTWARE,
            }
        }
        OutputFormat::Text => {
            if let Some(payload) = envelope.data.as_ref() {
                render_text(payload);
            }
            exit::SUCCESS
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde::Serialize;

    #[derive(Serialize)]
    struct Payload {
        binary: &'static str,
    }

    #[test]
    fn emit_success_writes_json_envelope() {
        let mut sink: Vec<u8> = Vec::new();
        let code = emit_success_to(
            &mut sink,
            "cli.forge-cli.auth.status.v1",
            Payload { binary: "x" },
            OutputFormat::Json,
            |_| panic!("text branch should not run for json"),
        );
        assert_eq!(code, exit::SUCCESS);
        let s = String::from_utf8(sink).unwrap();
        assert!(
            s.starts_with("{\"schema_version\":\"cli.forge-cli.auth.status.v1\""),
            "got {s}"
        );
        assert!(s.ends_with("\n"));
    }

    #[test]
    fn emit_success_text_uses_render_closure() {
        let mut sink: Vec<u8> = Vec::new();
        let rendered = std::cell::Cell::new(false);
        let code = emit_success_to(
            &mut sink,
            "cli.forge-cli.auth.status.v1",
            Payload { binary: "x" },
            OutputFormat::Text,
            |_p| rendered.set(true),
        );
        assert_eq!(code, exit::SUCCESS);
        assert!(rendered.get());
        assert!(sink.is_empty(), "text branch must not write to json sink");
    }
}
