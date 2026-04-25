use crate::common;
use std::fs;

use pretty_assertions::assert_eq;

#[test]
fn convert_dry_run_json_report_writes_artifacts_without_writing_output() {
    let dir = tempfile::TempDir::new().unwrap();
    fs::write(
        dir.path().join("icon.svg"),
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32">
<rect x="2" y="2" width="28" height="28" rx="6" fill="#0f62fe"/>
<path d="M9 16h14" stroke="#ffffff" stroke-width="3" stroke-linecap="round"/>
</svg>"##,
    )
    .unwrap();

    let stub = common::make_stub_dir();
    let path_s = stub.path().to_string_lossy().to_string();
    let envs = [("PATH", path_s.as_str())];

    let out = common::run_image_processing(
        dir.path(),
        &[
            "convert",
            "--in",
            "icon.svg",
            "--to",
            "webp",
            "--out",
            "out/icon.webp",
            "--dry-run",
            "--report",
            "--json",
        ],
        &envs,
    );
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);

    assert!(
        !dir.path().join("out/icon.webp").exists(),
        "dry-run should not write output"
    );

    let v: serde_json::Value = serde_json::from_str(&out.stdout).unwrap();
    assert_eq!(v["operation"], "convert");
    assert_eq!(v["backend"], "rust:resvg");
    assert_eq!(v["source"]["mode"], "svg");
    assert_eq!(v["source"]["input_path"], "icon.svg");
    assert_eq!(v["source"]["input_format"], "svg");
    assert_eq!(v["dry_run"], true);
    assert_eq!(v["options"]["report"], true);
    assert_eq!(v["items"].as_array().unwrap().len(), 1);
    assert_eq!(v["items"][0]["status"], "ok");
    assert!(v["items"][0]["output_info"].is_null());
    let command = v["commands"][0].as_str().unwrap_or("");
    assert!(
        command.contains("convert --in icon.svg"),
        "commands: {command}"
    );
    assert!(command.contains("--dry-run"), "commands: {command}");

    let run_id = v["run_id"].as_str().unwrap();
    let summary_json = dir
        .path()
        .join("out/image-processing/runs")
        .join(run_id)
        .join("summary.json");
    let report_md = dir
        .path()
        .join("out/image-processing/runs")
        .join(run_id)
        .join("report.md");
    assert!(summary_json.exists(), "missing {}", summary_json.display());
    assert!(report_md.exists(), "missing {}", report_md.display());
    assert_eq!(
        v["report_path"],
        format!("out/image-processing/runs/{run_id}/report.md")
    );

    let report = fs::read_to_string(report_md).unwrap();
    assert!(
        report.contains("- Operation: `convert`"),
        "report: {report}"
    );
    assert!(report.contains("- Source mode: `svg`"), "report: {report}");
    assert!(
        report.contains("- Source input: `icon.svg`"),
        "report: {report}"
    );
    assert!(
        report.contains("- Source format: `svg`"),
        "report: {report}"
    );
    assert!(report.contains("- Dry run: `true`"), "report: {report}");
    assert!(report.contains("convert --in icon.svg"), "report: {report}");
}

#[test]
fn svg_validate_dry_run_json_report_writes_artifacts_without_writing_output() {
    let dir = tempfile::TempDir::new().unwrap();
    fs::write(
        dir.path().join("valid.svg"),
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24">
<rect x="2" y="2" width="20" height="20" fill="#0f62fe"/>
</svg>"##,
    )
    .unwrap();

    let stub = common::make_stub_dir();
    let path_s = stub.path().to_string_lossy().to_string();
    let envs = [("PATH", path_s.as_str())];

    let out = common::run_image_processing(
        dir.path(),
        &[
            "svg-validate",
            "--in",
            "valid.svg",
            "--out",
            "out/valid.cleaned.svg",
            "--dry-run",
            "--report",
            "--json",
        ],
        &envs,
    );
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);

    assert!(
        !dir.path().join("out/valid.cleaned.svg").exists(),
        "dry-run should not write output"
    );

    let v: serde_json::Value = serde_json::from_str(&out.stdout).unwrap();
    assert_eq!(v["operation"], "svg-validate");
    assert_eq!(v["backend"], "rust:svg-validate");
    assert_eq!(v["source"]["mode"], "svg");
    assert_eq!(v["source"]["input_path"], "valid.svg");
    assert_eq!(v["source"]["input_format"], "svg");
    assert_eq!(v["dry_run"], true);
    assert_eq!(v["options"]["report"], true);
    assert_eq!(v["items"][0]["status"], "ok");
    assert!(v["items"][0]["output_info"].is_null());
    let command = v["commands"][0].as_str().unwrap_or("");
    assert!(command.contains("svg-validate --in valid.svg"));
    assert!(command.contains("--dry-run"));

    let run_id = v["run_id"].as_str().unwrap();
    let summary_json = dir
        .path()
        .join("out/image-processing/runs")
        .join(run_id)
        .join("summary.json");
    let report_md = dir
        .path()
        .join("out/image-processing/runs")
        .join(run_id)
        .join("report.md");
    assert!(summary_json.exists(), "missing {}", summary_json.display());
    assert!(report_md.exists(), "missing {}", report_md.display());

    let report = fs::read_to_string(report_md).unwrap();
    assert!(report.contains("- Operation: `svg-validate`"));
    assert!(report.contains("- Source mode: `svg`"));
    assert!(report.contains("- Source input: `valid.svg`"));
    assert!(report.contains("- Source format: `svg`"));
    assert!(report.contains("svg-validate --in valid.svg"));
}
