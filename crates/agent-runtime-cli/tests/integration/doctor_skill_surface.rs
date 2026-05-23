//! Integration coverage for `agent-runtime doctor --class skill-surface`.

use nils_test_support::bin;
use nils_test_support::cmd::{self, CmdOutput};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

fn agent_runtime_bin() -> PathBuf {
    bin::resolve("agent-runtime")
}

fn run(args: &[&str]) -> CmdOutput {
    cmd::run(&agent_runtime_bin(), args, &[], None)
}

fn write(path: &Path, body: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, body).unwrap();
}

fn codex_fixture() -> TempDir {
    let tmp = TempDir::new().unwrap();
    write(
        &tmp.path()
            .join("build/codex/plugins/reporting/skills/daily-brief/SKILL.md"),
        "# daily brief\n",
    );
    write(
        &tmp.path()
            .join("build/codex/plugins/reporting/skills/bad/SKILL.md"),
        "# bad\n",
    );
    write(
        &tmp.path().join("targets/codex/link-map.yaml"),
        r#"schema_version: 1
entries:
  - id: reporting.daily-brief
    kind: symlinked-file
    source: build/codex/plugins/reporting/skills/daily-brief
    destination: skills/reporting/daily-brief
  - id: reporting.bad-file-leaf
    kind: symlinked-file
    source: build/codex/plugins/reporting/skills/bad/SKILL.md
    destination: skills/reporting/bad/SKILL.md
"#,
    );
    tmp
}

#[test]
fn skill_surface_json_reports_directory_shape_and_file_leaf_warning() {
    let tmp = codex_fixture();
    let source_root = tmp.path().to_string_lossy().into_owned();

    let output = run(&[
        "doctor",
        "--source-root",
        &source_root,
        "--product",
        "codex",
        "--class",
        "skill-surface",
        "--format",
        "json",
    ]);

    assert_eq!(output.code, 1, "warning path should exit 1");
    let json: Value = serde_json::from_str(&output.stdout_text()).unwrap();
    assert_eq!(json["schema_version"], "agent-runtime-cli.doctor.v1");
    assert_eq!(
        json["acceptance_boundary"],
        "shape validation only; live Codex Desktop discovery still requires `codex debug prompt-input` in a fresh session"
    );
    assert_eq!(
        json["skill_surface"]["items"][0]["expected_codex_discoverable"],
        true
    );
    assert_eq!(json["skill_surface"]["items"][0]["link_mode"], "directory");
    assert_eq!(
        json["skill_surface"]["items"][1]["warnings"][0]["code"],
        "codex.active-skill.file-symlink"
    );
}

#[test]
fn skill_surface_human_output_ends_with_acceptance_boundary() {
    let tmp = codex_fixture();
    let source_root = tmp.path().to_string_lossy().into_owned();

    let output = run(&[
        "doctor",
        "--source-root",
        &source_root,
        "--product",
        "codex",
        "--class",
        "skill-surface",
    ]);

    assert_eq!(output.code, 1, "warning path should exit 1");
    let stderr = output.stderr_text();
    assert!(
        stderr.trim_end().ends_with(
            "agent-runtime doctor: acceptance-boundary: shape validation only; live Codex Desktop discovery still requires `codex debug prompt-input` in a fresh session"
        ),
        "stderr should end with acceptance boundary: {stderr}"
    );
}

#[test]
fn skill_surface_empty_link_map_exits_zero_without_manifests() {
    let tmp = TempDir::new().unwrap();
    write(
        &tmp.path().join("targets/codex/link-map.yaml"),
        "schema_version: 1\nentries: []\n",
    );
    let source_root = tmp.path().to_string_lossy().into_owned();

    let output = run(&[
        "doctor",
        "--source-root",
        &source_root,
        "--product",
        "codex",
        "--class",
        "skill-surface",
        "--format",
        "json",
    ]);

    assert_eq!(output.code, 0, "empty link-map should exit 0");
    let json: Value = serde_json::from_str(&output.stdout_text()).unwrap();
    assert_eq!(json["skill_surface"]["items"], Value::Array(Vec::new()));
}
