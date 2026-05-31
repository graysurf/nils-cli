use nils_common::cli_contract::{
    Envelope, EnvelopeError, OutputFormat, emit_parse_error_to, exit, schema_version_for,
};
use pretty_assertions::assert_eq;

#[test]
fn emit_parse_error_json_writes_envelope_to_stdout() {
    let mut stdout: Vec<u8> = Vec::new();
    let mut stderr: Vec<u8> = Vec::new();
    let code = emit_parse_error_to(
        &mut stdout,
        &mut stderr,
        "cli-template",
        OutputFormat::Json,
        "parse-error",
        "missing required argument <name>",
    );
    assert_eq!(code, exit::USAGE);
    let stdout = String::from_utf8(stdout).expect("stdout utf-8");
    let stderr = String::from_utf8(stderr).expect("stderr utf-8");
    assert!(
        stderr.is_empty(),
        "stderr should stay clean in JSON mode: {stderr:?}"
    );
    assert_eq!(
        stdout,
        "{\"schema_version\":\"cli.cli-template.error.v1\",\"ok\":false,\"error\":{\"code\":\"parse-error\",\"message\":\"missing required argument <name>\"}}\n"
    );
}

#[test]
fn emit_parse_error_text_writes_historical_error_prefix() {
    let mut stdout: Vec<u8> = Vec::new();
    let mut stderr: Vec<u8> = Vec::new();
    let code = emit_parse_error_to(
        &mut stdout,
        &mut stderr,
        "cli-template",
        OutputFormat::Text,
        "unknown-subcommand",
        "unrecognized subcommand 'bogus'",
    );
    assert_eq!(code, exit::USAGE);
    let stdout = String::from_utf8(stdout).expect("stdout utf-8");
    let stderr = String::from_utf8(stderr).expect("stderr utf-8");
    assert!(
        stdout.is_empty(),
        "stdout should stay clean in text mode: {stdout:?}"
    );
    assert_eq!(stderr, "error: unrecognized subcommand 'bogus'\n");
}

#[test]
fn emit_parse_error_returns_usage_for_unknown_subcommand() {
    let mut stdout: Vec<u8> = Vec::new();
    let mut stderr: Vec<u8> = Vec::new();
    let code = emit_parse_error_to(
        &mut stdout,
        &mut stderr,
        "memo",
        OutputFormat::Json,
        "unknown-subcommand",
        "no such subcommand: bogus",
    );
    assert_eq!(code, exit::USAGE);
}

#[test]
fn envelope_failure_round_trips_through_serde_json_value() {
    let envelope: Envelope<()> = Envelope::failure(
        schema_version_for("cli-template", "error", 1),
        EnvelopeError::new("parse-error", "missing required argument <name>"),
    );
    let value: serde_json::Value =
        serde_json::to_value(&envelope).expect("serialize via serde_json::Value");
    assert_eq!(value["schema_version"], "cli.cli-template.error.v1");
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"]["code"], "parse-error");
    assert_eq!(
        value["error"]["message"],
        "missing required argument <name>"
    );
    assert!(value.get("data").is_none());
    assert!(value.get("warnings").is_none());
}
