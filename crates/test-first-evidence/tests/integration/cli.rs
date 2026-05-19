use std::fs;
use std::path::Path;

use nils_test_support::cmd::{CmdOutput, run_resolved_in_dir};
use pretty_assertions::assert_eq;
use serde_json::Value;

fn run(dir: &Path, args: &[&str]) -> CmdOutput {
    run_resolved_in_dir("test-first-evidence", dir, args, &[], None)
}

fn json_stdout(output: &CmdOutput) -> Value {
    serde_json::from_str(&output.stdout_text()).expect("stdout should be json")
}

fn out_arg(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

fn combined_output(output: &CmdOutput) -> String {
    format!("{}{}", output.stdout_text(), output.stderr_text())
}

#[test]
fn help_includes_version_flag_and_examples() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let output = run(tmp.path(), &["--help"]);

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let stdout = output.stdout_text();
    assert!(
        stdout.contains("-V, --version"),
        "missing version flag: {stdout}"
    );
    assert!(stdout.contains("EXAMPLES:"), "missing examples: {stdout}");
}

#[test]
fn records_failing_evidence_final_validation_and_verifies_json() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let out_dir = tmp.path().join("evidence");
    let out_arg = out_arg(&out_dir);

    let init = run(
        tmp.path(),
        &[
            "init",
            "--out",
            &out_arg,
            "--classification",
            "bug-fix",
            "--production-path",
            "src/lib.rs",
            "--note",
            "ticket VY-1",
            "--format",
            "json",
        ],
    );
    assert_eq!(init.code, 0, "stderr={}", init.stderr_text());
    let init_json = json_stdout(&init);
    assert_eq!(
        init_json["schema_version"],
        "cli.test-first-evidence.init.v1"
    );
    assert_eq!(init_json["command"], "test-first-evidence init");
    assert_eq!(init_json["ok"], true);
    assert_eq!(init_json["result"]["complete"], false);

    let failing = run(
        tmp.path(),
        &[
            "record-failing",
            "--out",
            &out_arg,
            "--command",
            "cargo test bug_repro",
            "--exit-code",
            "101",
            "--summary",
            "assertion failed before fix",
            "--test-name",
            "bug_repro",
            "--format",
            "json",
        ],
    );
    assert_eq!(failing.code, 0, "stderr={}", failing.stderr_text());
    let failing_json = json_stdout(&failing);
    assert_eq!(
        failing_json["schema_version"],
        "cli.test-first-evidence.record-failing.v1"
    );
    assert_eq!(
        failing_json["result"]["record"]["failing_test"]["exit_code"],
        101
    );

    let final_validation = run(
        tmp.path(),
        &[
            "record-final",
            "--out",
            &out_arg,
            "--command",
            "cargo test bug_repro",
            "--status",
            "pass",
            "--summary",
            "targeted regression passed",
            "--format",
            "json",
        ],
    );
    assert_eq!(
        final_validation.code,
        0,
        "stderr={}",
        final_validation.stderr_text()
    );
    assert_eq!(json_stdout(&final_validation)["result"]["complete"], true);

    let verify = run(
        tmp.path(),
        &["verify", "--out", &out_arg, "--format", "json"],
    );
    assert_eq!(verify.code, 0, "stderr={}", verify.stderr_text());
    let verify_json = json_stdout(&verify);
    assert_eq!(
        verify_json["schema_version"],
        "cli.test-first-evidence.verify.v1"
    );
    assert_eq!(verify_json["result"]["complete"], true);

    let record = fs::read_to_string(out_dir.join("test-first-evidence.json")).expect("record");
    assert!(record.contains("\"schema_version\": \"test-first-evidence.record.v1\""));
    assert!(record.contains("\"status\": \"pass\""));
}

#[test]
fn waiver_path_can_complete_evidence_record() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let out_dir = tmp.path().join("waiver-evidence");
    let out_arg = out_arg(&out_dir);

    let init = run(
        tmp.path(),
        &["init", "--out", &out_arg, "--classification", "docs-only"],
    );
    assert_eq!(init.code, 0, "stderr={}", init.stderr_text());

    let waiver = run(
        tmp.path(),
        &[
            "record-waiver",
            "--out",
            &out_arg,
            "--reason",
            "docs-only change; no production behavior edited",
            "--substitute-validation",
            "markdownlint passed",
        ],
    );
    assert_eq!(waiver.code, 0, "stderr={}", waiver.stderr_text());

    let final_validation = run(
        tmp.path(),
        &[
            "record-final",
            "--out",
            &out_arg,
            "--command",
            "bash scripts/ci/markdownlint-audit.sh --strict",
            "--status",
            "pass",
        ],
    );
    assert_eq!(
        final_validation.code,
        0,
        "stderr={}",
        final_validation.stderr_text()
    );

    let verify = run(tmp.path(), &["verify", "--out", &out_arg]);
    assert_eq!(verify.code, 0, "stderr={}", verify.stderr_text());
    assert!(
        verify
            .stdout_text()
            .contains("test-first evidence complete"),
        "missing completion text: {}",
        verify.stdout_text()
    );
}

#[test]
fn verify_incomplete_record_returns_json_error() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let out_dir = tmp.path().join("incomplete");
    let out_arg = out_arg(&out_dir);

    let init = run(
        tmp.path(),
        &[
            "init",
            "--out",
            &out_arg,
            "--classification",
            "behavior-change",
        ],
    );
    assert_eq!(init.code, 0, "stderr={}", init.stderr_text());

    let verify = run(
        tmp.path(),
        &["verify", "--out", &out_arg, "--format", "json"],
    );
    assert_eq!(verify.code, 1, "stdout={}", verify.stdout_text());
    let value = json_stdout(&verify);
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"]["code"], "incomplete-evidence");
    assert_eq!(
        value["error"]["details"]["missing"][0],
        "failing_test_or_waiver"
    );
}

#[test]
fn secret_like_inputs_are_redacted_in_outputs_and_record() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let out_dir = tmp.path().join("secret-safe");
    let out_arg = out_arg(&out_dir);

    let init = run(
        tmp.path(),
        &[
            "init",
            "--out",
            &out_arg,
            "--classification",
            "bug-fix",
            "--note",
            "OPENAI_API_KEY=sk-proj-supersecret",
        ],
    );
    assert_eq!(init.code, 0, "stderr={}", init.stderr_text());

    let failing = run(
        tmp.path(),
        &[
            "record-failing",
            "--out",
            &out_arg,
            "--command",
            "OPENAI_API_KEY=sk-proj-supersecret cargo test",
            "--exit-code",
            "101",
            "--summary",
            "token: abcdefghijklmnop failure",
            "--format",
            "json",
        ],
    );
    assert_eq!(failing.code, 0, "stderr={}", failing.stderr_text());

    let record = fs::read_to_string(out_dir.join("test-first-evidence.json")).expect("record");
    let combined = format!("{record}\n{}", combined_output(&failing));
    assert!(combined.contains("[REDACTED]"));
    assert!(!combined.contains("sk-proj-supersecret"));
    assert!(!combined.contains("abcdefghijklmnop"));
}

#[test]
fn missing_record_fails_with_json_error() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let out_dir = tmp.path().join("missing");
    let out_arg = out_arg(&out_dir);

    let output = run(tmp.path(), &["show", "--out", &out_arg, "--format", "json"]);

    assert_eq!(output.code, 1, "stderr={}", output.stderr_text());
    let value = json_stdout(&output);
    assert_eq!(value["schema_version"], "cli.test-first-evidence.show.v1");
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"]["code"], "missing-record");
}

#[test]
fn completion_export_succeeds_outside_git_repo() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let output = run(tmp.path(), &["completion", "zsh"]);

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    assert!(
        output
            .stdout_text()
            .contains("#compdef test-first-evidence"),
        "missing completion header: {}",
        output.stdout_text()
    );
}
