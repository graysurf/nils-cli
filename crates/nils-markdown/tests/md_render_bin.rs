//! Integration test for the `md-render` binary. Exercises both the
//! text and JSON envelope paths and asserts byte-equal rendered
//! output against the same template + view rendered through the
//! library directly.

#![cfg(feature = "bin-cli")]

use std::process::Command;

use nils_markdown::Engine;
use serde_json::json;
use tempfile::TempDir;

fn binary_path() -> std::path::PathBuf {
    let mut path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.pop(); // crates/
    path.pop(); // workspace root
    path.push("target");
    path.push("debug");
    path.push("md-render");
    path
}

#[test]
fn md_render_text_envelope_writes_rendered_template_to_stdout() {
    let tmp = TempDir::new().expect("tempdir");
    let template_path = tmp.path().join("greeting.md.tera");
    std::fs::write(
        &template_path,
        "# Hello {{ name }}\n\nWelcome to {{ project }}!\n",
    )
    .expect("write template");
    let data_path = tmp.path().join("greeting.json");
    std::fs::write(
        &data_path,
        serde_json::to_string(&json!({"name": "world", "project": "nils-markdown"})).unwrap(),
    )
    .expect("write data");

    let output = Command::new(binary_path())
        .args([
            "--template",
            template_path.to_str().unwrap(),
            "--data",
            data_path.to_str().unwrap(),
        ])
        .output()
        .expect("md-render binary runs");
    assert!(
        output.status.success(),
        "md-render exited non-zero: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("stdout is UTF-8");
    let mut engine = Engine::builder().build();
    engine
        .register_template(
            "greeting",
            "# Hello {{ name }}\n\nWelcome to {{ project }}!\n",
        )
        .expect("register library template");
    let expected = engine
        .render_value(
            "greeting",
            &json!({"name": "world", "project": "nils-markdown"}),
        )
        .expect("library render");
    assert_eq!(stdout, expected);
}

#[test]
fn md_render_json_envelope_wraps_body_in_render_v1_envelope() {
    let tmp = TempDir::new().expect("tempdir");
    let template_path = tmp.path().join("greeting.md.tera");
    std::fs::write(&template_path, "hi {{ name }}\n").expect("write template");
    let data_path = tmp.path().join("greeting.json");
    std::fs::write(
        &data_path,
        serde_json::to_string(&json!({"name": "world"})).unwrap(),
    )
    .expect("write data");

    let output = Command::new(binary_path())
        .args([
            "--format",
            "json",
            "--template",
            template_path.to_str().unwrap(),
            "--data",
            data_path.to_str().unwrap(),
        ])
        .output()
        .expect("md-render binary runs");
    assert!(output.status.success(), "md-render exited non-zero");

    let stdout = String::from_utf8(output.stdout).expect("stdout is UTF-8");
    let envelope: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("stdout is a JSON envelope");
    assert_eq!(envelope["schema_version"], "cli.md-render.render.v1");
    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["data"]["template"], "greeting");
    assert_eq!(envelope["data"]["body"], "hi world\n");
}

#[test]
fn md_render_missing_template_argument_returns_usage_error() {
    let tmp = TempDir::new().expect("tempdir");
    let data_path = tmp.path().join("data.json");
    std::fs::write(&data_path, "{}").expect("write data");

    let output = Command::new(binary_path())
        .args(["--format", "json", "--data", data_path.to_str().unwrap()])
        .output()
        .expect("md-render binary runs");
    let stdout = String::from_utf8(output.stdout).expect("stdout is UTF-8");
    let envelope: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("stdout is a JSON envelope");
    assert_eq!(envelope["ok"], false);
    assert_eq!(envelope["error"]["code"], "missing-argument");
}
