//! Integration test for the `md-render` binary. Exercises both the
//! text and JSON envelope paths and asserts byte-equal rendered
//! output against the same template + view rendered through the
//! library directly.

#![cfg(feature = "bin-cli")]

use nils_markdown::Engine;
use nils_test_support::cmd::{CmdOptions, CmdOutput, run_resolved};
use serde_json::json;
use tempfile::TempDir;

fn write_fixture(
    tmp: &TempDir,
    template_name: &str,
    template_body: &str,
    data_body: &str,
) -> (std::path::PathBuf, std::path::PathBuf) {
    let template_path = tmp.path().join(template_name);
    std::fs::write(&template_path, template_body).expect("write template");
    let data_path = tmp.path().join("data.json");
    std::fs::write(&data_path, data_body).expect("write data");
    (template_path, data_path)
}

fn run_md_render(args: &[&str]) -> CmdOutput {
    run_resolved("md-render", args, &CmdOptions::new())
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

    let output = run_md_render(&[
        "--template",
        template_path.to_str().unwrap(),
        "--data",
        data_path.to_str().unwrap(),
    ]);
    assert_eq!(
        output.code,
        0,
        "md-render exited non-zero: stderr={}",
        output.stderr_text()
    );

    let stdout = output.stdout_text();
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

    let output = run_md_render(&[
        "--format",
        "json",
        "--template",
        template_path.to_str().unwrap(),
        "--data",
        data_path.to_str().unwrap(),
    ]);
    assert_eq!(output.code, 0, "md-render exited non-zero");

    let envelope = output.stdout_json();
    assert_eq!(envelope["schema_version"], "cli.md-render.render.v1");
    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["data"]["template"], "greeting");
    assert_eq!(envelope["data"]["body"], "hi world\n");
}

#[test]
fn md_render_render_subcommand_accepts_explicit_format_equals_json() {
    let tmp = TempDir::new().expect("tempdir");
    let (template_path, data_path) = write_fixture(
        &tmp,
        "explicit.tera",
        "hello {{ name }}",
        r#"{"name":"Ada"}"#,
    );

    let output = run_md_render(&[
        "--format=json",
        "render",
        "--template",
        template_path.to_str().unwrap(),
        "--data",
        data_path.to_str().unwrap(),
    ]);

    assert_eq!(output.code, 0, "md-render exited non-zero");
    let envelope = output.stdout_json();
    assert_eq!(envelope["schema_version"], "cli.md-render.render.v1");
    assert_eq!(envelope["data"]["template"], "explicit");
    assert_eq!(envelope["data"]["body"], "hello Ada");
}

#[test]
fn md_render_completion_exports_bash_and_zsh_scripts() {
    let bash = run_md_render(&["completion", "bash"]);
    assert_eq!(bash.code, 0, "bash completion failed");
    let bash_stdout = bash.stdout_text();
    assert!(bash_stdout.contains("complete -o nospace -F _md-render"));
    assert!(bash_stdout.contains("md-render"));
    assert!(bash_stdout.contains("render"));

    let zsh = run_md_render(&["completion", "zsh"]);
    assert_eq!(zsh.code, 0, "zsh completion failed");
    let zsh_stdout = zsh.stdout_text();
    assert!(zsh_stdout.contains("#compdef md-render"));
    assert!(zsh_stdout.contains("completion"));
}

#[test]
fn md_render_unknown_subcommand_honors_json_format_detection() {
    let output = run_md_render(&["--format=json", "nope"]);

    assert_ne!(output.code, 0, "unknown subcommand should fail");
    let envelope = output.stdout_json();
    assert_eq!(envelope["ok"], false);
    assert_eq!(envelope["error"]["code"], "unknown-subcommand");
}

#[test]
fn md_render_missing_data_argument_returns_json_contract_error() {
    let tmp = TempDir::new().expect("tempdir");
    let template_path = tmp.path().join("missing-data.tera");
    std::fs::write(&template_path, "hi").expect("write template");

    let output = run_md_render(&[
        "--format",
        "json",
        "--template",
        template_path.to_str().unwrap(),
    ]);

    assert_ne!(output.code, 0, "missing data should fail");
    let envelope = output.stdout_json();
    assert_eq!(envelope["error"]["code"], "missing-argument");
    assert!(
        envelope["error"]["message"]
            .as_str()
            .unwrap()
            .contains("--data is required")
    );
}

#[test]
fn md_render_invalid_template_stem_is_reported_before_reading_files() {
    let tmp = TempDir::new().expect("tempdir");
    let template_path = tmp.path().join(".tera");
    std::fs::write(&template_path, "not read").expect("write template");
    let data_path = tmp.path().join("data.json");
    std::fs::write(&data_path, "{}").expect("write data");

    let output = run_md_render(&[
        "--format",
        "json",
        "--template",
        template_path.to_str().unwrap(),
        "--data",
        data_path.to_str().unwrap(),
    ]);

    assert_ne!(output.code, 0, "invalid template stem should fail");
    let envelope = output.stdout_json();
    assert_eq!(envelope["error"]["code"], "invalid-template-path");
}

#[test]
fn md_render_file_and_json_errors_keep_stable_error_codes() {
    let tmp = TempDir::new().expect("tempdir");
    let template_path = tmp.path().join("broken.tera");
    std::fs::write(&template_path, "{{ name }}").expect("write template");
    let data_path = tmp.path().join("bad.json");
    std::fs::write(&data_path, "{").expect("write data");

    let missing_template = run_md_render(&[
        "--format",
        "json",
        "--template",
        tmp.path().join("missing.tera").to_str().unwrap(),
        "--data",
        data_path.to_str().unwrap(),
    ]);
    assert_eq!(
        missing_template.stdout_json()["error"]["code"],
        "template-read-failed"
    );

    let missing_data = run_md_render(&[
        "--format",
        "json",
        "--template",
        template_path.to_str().unwrap(),
        "--data",
        tmp.path().join("missing.json").to_str().unwrap(),
    ]);
    assert_eq!(
        missing_data.stdout_json()["error"]["code"],
        "data-read-failed"
    );

    let bad_json = run_md_render(&[
        "--format",
        "json",
        "--template",
        template_path.to_str().unwrap(),
        "--data",
        data_path.to_str().unwrap(),
    ]);
    assert_eq!(bad_json.stdout_json()["error"]["code"], "data-parse-failed");
}

#[test]
fn md_render_template_register_and_render_errors_are_distinct() {
    let tmp = TempDir::new().expect("tempdir");
    let data_path = tmp.path().join("data.json");
    std::fs::write(&data_path, "{}").expect("write data");

    let register_error_template = tmp.path().join("uses-now.tera");
    std::fs::write(&register_error_template, "{{ now() }}").expect("write template");
    let register_error = run_md_render(&[
        "--format",
        "json",
        "--template",
        register_error_template.to_str().unwrap(),
        "--data",
        data_path.to_str().unwrap(),
    ]);
    assert_eq!(
        register_error.stdout_json()["error"]["code"],
        "template-register-failed"
    );

    let render_error_template = tmp.path().join("missing-field.tera");
    std::fs::write(&render_error_template, "{{ missing.field }}").expect("write template");
    let render_error = run_md_render(&[
        "--format",
        "json",
        "--template",
        render_error_template.to_str().unwrap(),
        "--data",
        data_path.to_str().unwrap(),
    ]);
    assert_eq!(
        render_error.stdout_json()["error"]["code"],
        "template-render-failed"
    );
}

#[test]
fn md_render_missing_template_argument_returns_usage_error() {
    let tmp = TempDir::new().expect("tempdir");
    let data_path = tmp.path().join("data.json");
    std::fs::write(&data_path, "{}").expect("write data");

    let output = run_md_render(&["--format", "json", "--data", data_path.to_str().unwrap()]);
    let envelope = output.stdout_json();
    assert_eq!(envelope["ok"], false);
    assert_eq!(envelope["error"]["code"], "missing-argument");
}
