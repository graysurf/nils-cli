use std::fs;
use std::path::Path;
use std::process::Command;

use nils_test_support::cmd::{CmdOutput, run_resolved_in_dir};
use pretty_assertions::assert_eq;
use serde_json::Value;

fn run(bin: &str, dir: &Path, args: &[&str]) -> CmdOutput {
    run_resolved_in_dir(bin, dir, args, &[], None)
}

fn json_stdout(output: &CmdOutput) -> Value {
    serde_json::from_str(&output.stdout_text()).expect("stdout should be json")
}

fn out_arg(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

#[test]
fn all_binaries_export_zsh_completion() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    for bin in [
        "browser-session",
        "canary-check",
        "docs-impact",
        "model-cross-check",
        "review-evidence",
    ] {
        let output = run(bin, tmp.path(), &["completion", "zsh"]);
        assert_eq!(output.code, 0, "{bin} stderr={}", output.stderr_text());
        assert!(
            output.stdout_text().contains(&format!("#compdef {bin}")),
            "missing zsh header for {bin}: {}",
            output.stdout_text()
        );
    }
}

#[test]
fn docs_impact_scans_docs_and_non_docs_changes() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    git(tmp.path(), &["init"]);
    fs::create_dir_all(tmp.path().join("src")).expect("src dir");
    fs::create_dir_all(tmp.path().join("docs")).expect("docs dir");
    fs::write(
        tmp.path().join("src/lib.rs"),
        "pub fn value() -> u8 { 1 }\n",
    )
    .expect("src write");
    fs::write(tmp.path().join("docs/runbook.md"), "# Runbook\n").expect("docs write");

    let output = run(
        "docs-impact",
        tmp.path(),
        &["scan", "--include-untracked", "--format", "json"],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let value = json_stdout(&output);
    assert_eq!(value["schema_version"], "cli.docs-impact.scan.v1");
    assert_eq!(value["result"]["docs_changed"], true);
    assert_eq!(value["result"]["non_docs_changed"], true);
    assert!(
        value["result"]["docs_files"]
            .as_array()
            .expect("docs array")
            .iter()
            .any(|path| path == "docs/runbook.md")
    );
}

#[test]
fn canary_check_records_passing_command() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let out_dir = tmp.path().join("canary");
    let out = out_arg(&out_dir);

    let run_output = run(
        "canary-check",
        tmp.path(),
        &[
            "run",
            "--out",
            &out,
            "--name",
            "smoke",
            "--command",
            "printf ok",
            "--format",
            "json",
        ],
    );
    assert_eq!(run_output.code, 0, "stderr={}", run_output.stderr_text());
    let run_json = json_stdout(&run_output);
    assert_eq!(run_json["schema_version"], "cli.canary-check.run.v1");
    assert_eq!(run_json["result"]["record"]["last_run"]["status"], "pass");

    let verify = run(
        "canary-check",
        tmp.path(),
        &["verify", "--out", &out, "--format", "json"],
    );
    assert_eq!(verify.code, 0, "stderr={}", verify.stderr_text());
    assert_eq!(
        json_stdout(&verify)["result"]["last_run"]["stdout_preview"],
        "ok"
    );
}

#[test]
fn review_evidence_requires_no_open_medium_or_high_findings() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let out_dir = tmp.path().join("review");
    let out = out_arg(&out_dir);

    assert_eq!(
        run(
            "review-evidence",
            tmp.path(),
            &["init", "--out", &out, "--subject", "PR #1"]
        )
        .code,
        0
    );
    assert_eq!(
        run(
            "review-evidence",
            tmp.path(),
            &[
                "record-finding",
                "--out",
                &out,
                "--severity",
                "medium",
                "--path",
                "src/lib.rs",
                "--summary",
                "needs guard",
                "--status",
                "fixed",
            ],
        )
        .code,
        0
    );
    assert_eq!(
        run(
            "review-evidence",
            tmp.path(),
            &[
                "record-validation",
                "--out",
                &out,
                "--command",
                "cargo test",
                "--status",
                "pass",
            ],
        )
        .code,
        0
    );

    let verify = run(
        "review-evidence",
        tmp.path(),
        &["verify", "--out", &out, "--format", "json"],
    );
    assert_eq!(verify.code, 0, "stderr={}", verify.stderr_text());
    assert_eq!(json_stdout(&verify)["result"]["complete"], true);
}

#[test]
fn browser_session_records_steps_and_verifies() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let out_dir = tmp.path().join("browser");
    let out = out_arg(&out_dir);

    assert_eq!(
        run(
            "browser-session",
            tmp.path(),
            &[
                "init",
                "--out",
                &out,
                "--target",
                "http://localhost:3000",
                "--goal",
                "verify checkout",
            ],
        )
        .code,
        0
    );
    assert_eq!(
        run(
            "browser-session",
            tmp.path(),
            &[
                "record-step",
                "--out",
                &out,
                "--action",
                "opened checkout page",
                "--status",
                "pass",
            ],
        )
        .code,
        0
    );

    let verify = run(
        "browser-session",
        tmp.path(),
        &["verify", "--out", &out, "--format", "json"],
    );
    assert_eq!(verify.code, 0, "stderr={}", verify.stderr_text());
    assert_eq!(json_stdout(&verify)["result"]["complete"], true);
}

#[test]
fn model_cross_check_requires_primary_and_checker_observations() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let out_dir = tmp.path().join("model");
    let out = out_arg(&out_dir);

    assert_eq!(
        run(
            "model-cross-check",
            tmp.path(),
            &[
                "init",
                "--out",
                &out,
                "--prompt",
                "review patch",
                "--primary-model",
                "gpt-5.5",
                "--checker-model",
                "gemini-2.5-pro",
            ],
        )
        .code,
        0
    );
    for (role, model) in [("primary", "gpt-5.5"), ("checker", "gemini-2.5-pro")] {
        let output = run(
            "model-cross-check",
            tmp.path(),
            &[
                "record-observation",
                "--out",
                &out,
                "--role",
                role,
                "--model",
                model,
                "--verdict",
                "pass",
                "--summary",
                "no blocker",
            ],
        );
        assert_eq!(output.code, 0, "{role} stderr={}", output.stderr_text());
    }

    let verify = run(
        "model-cross-check",
        tmp.path(),
        &["verify", "--out", &out, "--format", "json"],
    );
    assert_eq!(verify.code, 0, "stderr={}", verify.stderr_text());
    assert_eq!(json_stdout(&verify)["result"]["complete"], true);
}

#[test]
fn secret_like_inputs_are_redacted() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let out_dir = tmp.path().join("canary-secret");
    let out = out_arg(&out_dir);

    let output = run(
        "canary-check",
        tmp.path(),
        &[
            "run",
            "--out",
            &out,
            "--name",
            "secret",
            "--command",
            "printf OPENAI_API_KEY=sk-proj-supersecret",
            "--format",
            "json",
        ],
    );
    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let combined = format!(
        "{}\n{}",
        output.stdout_text(),
        fs::read_to_string(out_dir.join("canary-check.json")).expect("record")
    );
    assert!(combined.contains("[REDACTED]"));
    assert!(!combined.contains("sk-proj-supersecret"));
}

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} failed");
}
